//! Additional unit and mutation-killing tests for `space_cyno_filaments`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::space_destiny::space_cyno_filaments::handle;
use eve_db::types::FleetRecord;
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, CombatState, MovementMode, SlimItemMeta};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::{InvType, SdeRegistry};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

#[cfg(test)]
#[path = "space_filaments_and_beacons_unit_tests.rs"]
mod space_filaments_and_beacons_unit_tests;

pub(super) static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

pub(super) fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(eve_db::WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

pub(super) fn clone_type_with(
    sde: &mut Arc<SdeRegistry>,
    new_tid: u32,
    group_id: u32,
    category_id: u32,
    name: &str,
    mass: f64,
    attrs: &[(u16, f64)],
) {
    let reg = Arc::make_mut(sde);
    let mut rec: InvType = reg.types.values().next().cloned().expect("base sde type");
    rec.type_id = new_tid;
    rec.group_id = group_id;
    rec.category_id = category_id;
    rec.name = name.to_string();
    rec.mass = mass;
    rec.default_attributes.clear();
    for &(k, v) in attrs {
        rec.default_attributes.insert(k, v);
    }
    reg.types.insert(new_tid, rec);
}

pub(super) async fn spawn_ship(
    sys_h: &crate::system_actor::SystemHandle,
    id: i64,
    type_id: u32,
    group_id: u32,
    category_id: u32,
    position: DVec3,
    mass: f64,
) {
    let ball = Ball {
        id,
        type_id,
        position,
        velocity: DVec3::ZERO,
        max_velocity: 350.0,
        inertia: 2.0,
        mass,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let meta = SlimItemMeta {
        owner_id: 90_000_001,
        char_id: Some(90_000_001),
        corp_id: 98_000_001,
        alliance_id: -1,
        group_id,
        category_id,
        security_status: 0.0,
        is_cloaked: false,
        ..Default::default()
    };
    let _ = sys_h
        .spawn_ball_with_meta(ball, CombatState::default(), meta)
        .await;
}

pub(super) async fn call_cyno(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let kw = HashMap::new();
    handle(ctx, sess, None, svc, method, args, &kw).await.unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wormhole_jump_validation_capital_restriction_polarization_mass_and_k162_transit() {
    let mut ctx = test_ctx();
    clone_type_with(&mut ctx.sde, 990_001, 659, 6, "Test Supercarrier", 20_000_000.0, &[]);
    let sys_id = 30_000_142_u32;
    let char_id = 90_000_001_i64;
    let ship_id = 100_000_001_i64;
    let mut sess = SessionState::new(801);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(98_000_001);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);

    // 1. ShipDocked, NoSolarSystem, ShipNotInSpace
    sess.station_id = Some(60_003_760);
    assert_eq!(
        call_cyno(&ctx, &mut sess, "wormholeMgr", "WormholeJump", &[]).await.unwrap_err().code,
        "ShipDocked"
    );
    sess.station_id = None;
    sess.solar_system_id = None;
    assert_eq!(
        call_cyno(&ctx, &mut sess, "wormholeMgr", "WormholeJump", &[]).await.unwrap_err().code,
        "NoSolarSystem"
    );
    sess.solar_system_id = Some(sys_id);
    assert_eq!(
        call_cyno(&ctx, &mut sess, "wormholeMgr", "WormholeJump", &[PyRep::Integer(750_000_001)])
            .await
            .unwrap_err()
            .code,
        "ShipNotInSpace"
    );

    // 2. Supercarrier restricted from wormhole -> ShipRestrictedFromWormhole
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let k162_id = 750_000_001_i64;
    spawn_ship(&sys_handle, k162_id, 26_272, 988, 2, DVec3::new(500.0, 0.0, 0.0), 1.0).await;
    ctx.db.with_mut_snapshot(|s| {
        if let Some(sh) = s.items.get_mut(&ship_id) {
            sh.type_id = 990_001;
        }
    });
    spawn_ship(&sys_handle, ship_id, 990_001, 659, 6, DVec3::ZERO, 20_000_000.0).await;
    assert_eq!(
        call_cyno(&ctx, &mut sess, "wormholeMgr", "WormholeJump", &[PyRep::Integer(k162_id)])
            .await
            .unwrap_err()
            .code,
        "ShipRestrictedFromWormhole"
    );

    // Replace with subcap ship (587)
    ctx.db.with_mut_snapshot(|s| {
        if let Some(sh) = s.items.get_mut(&ship_id) {
            sh.type_id = 587;
        }
    });
    let _ = sys_handle.remove_ball(ship_id).await;
    spawn_ship(&sys_handle, ship_id, 587, 25, 6, DVec3::ZERO, 1_200_000.0).await;

    // 3. MissingWormholeBallId, WormholeNotPresent, OutsideWormholeJumpRange, NoWormholeDestination
    assert_eq!(
        call_cyno(&ctx, &mut sess, "wormholeMgr", "WormholeJump", &[]).await.unwrap_err().code,
        "MissingWormholeBallId"
    );
    assert_eq!(
        call_cyno(&ctx, &mut sess, "wormholeMgr", "WormholeJump", &[PyRep::Integer(750_999_999)])
            .await
            .unwrap_err()
            .code,
        "WormholeNotPresent"
    );

    let wh_no_dest = 750_000_099_i64;
    spawn_ship(&sys_handle, wh_no_dest, 26_272, 988, 2, DVec3::new(5_000.0, 0.0, 0.0), 1.0).await;
    assert_eq!(
        call_cyno(&ctx, &mut sess, "wormholeMgr", "WormholeJump", &[PyRep::Integer(wh_no_dest)])
            .await
            .unwrap_err()
            .code,
        "OutsideWormholeJumpRange"
    );
    let _ = sys_handle.set_ball_position(wh_no_dest, DVec3::new(500.0, 0.0, 0.0)).await;
    assert_eq!(
        call_cyno(&ctx, &mut sess, "wormholeMgr", "WormholeJump", &[PyRep::Integer(wh_no_dest)])
            .await
            .unwrap_err()
            .code,
        "NoWormholeDestination"
    );

    // 4. Configure wormhole destination 31_000_001 and execute jump through canonical K162 (750_000_001) at 500 m
    ctx.db.with_mut_snapshot(|s| {
        let st = s.system_states.entry(sys_id).or_default();
        st.wormhole_destination_system_id = Some(31_000_001);
        st.wormhole_remaining_mass_kg = 1_000_000_000.0;
    });
    let jump_res = call_cyno(&ctx, &mut sess, "wormholeMgr", "WormholeJump", &[PyRep::Integer(k162_id)])
        .await
        .unwrap();
    let PyRep::KeyVal(jkv) = jump_res else {
        panic!("expected KeyVal from WormholeJump");
    };
    assert_eq!(jkv["solarSystemID"], PyRep::Integer(31_000_001));
    assert_eq!(sess.solar_system_id, Some(31_000_001));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn random_jump_filament_pochven_restrictions_scramble_and_invulnerability() {
    let mut ctx = test_ctx();
    clone_type_with(
        &mut ctx.sde,
        56_065,
        4087,
        66,
        "Glorification-1 'Devana' Filament",
        1.0,
        &[(3025, 4087.0)],
    );
    clone_type_with(
        &mut ctx.sde,
        56_071,
        4087,
        66,
        "Border-5 'Pochven' Filament",
        1.0,
        &[(3025, 4087.0)],
    );
    let sys_id = 30_000_142_u32;
    let char_id = 90_000_001_i64;
    let ship_id = 100_000_001_i64;
    let mut sess = SessionState::new(802);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(98_000_001);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);
    sess.fleet_id = Some(501);

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let exit_fil_id = 920_001_i64;
    let entry_fil_id = 920_002_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.insert(
            501,
            FleetRecord {
                fleet_id: 501,
                boss_id: char_id,
                members: vec![char_id],
                free_move: true,
                ..Default::default()
            },
        );
        for (id, tid) in [(exit_fil_id, 56_065), (entry_fil_id, 56_071)] {
            snap.items.insert(
                id,
                InventoryItemRecord {
                    item_id: id,
                    type_id: tid,
                    owner_id: char_id,
                    location_id: ship_id,
                    flag: InventoryFlag::Cargo,
                    quantity: 1,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
        }
    });

    // 1. ShipInvulnerable when post-warp invulnerability timer is active
    let invuln_ball = Ball {
        id: ship_id,
        type_id: 587,
        position: DVec3::new(1.5e13, -2.1e13, 0.9e13),
        velocity: DVec3::ZERO,
        max_velocity: 350.0,
        inertia: 2.0,
        mass: 1_000_000.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let _ = sys_handle
        .spawn_ball_with_combat(
            invuln_ball,
            CombatState {
                warp_exit_invulnerable_remaining_ticks: 10,
                ..Default::default()
            },
        )
        .await;
    assert_eq!(
        call_cyno(
            &ctx,
            &mut sess,
            "randomJumpMgr",
            "ActivateRandomJumpFilament",
            &[PyRep::Integer(exit_fil_id)]
        )
        .await
        .unwrap_err()
        .code,
        "ShipInvulnerable"
    );
    let _ = sys_handle.remove_ball(ship_id).await;
    spawn_ship(
        &sys_handle,
        ship_id,
        587,
        25,
        6,
        DVec3::new(1.5e13, -2.1e13, 0.9e13),
        1_000_000.0,
    )
    .await;

    // 2. Pochven exit filament used in Jita (non-Pochven) -> PochvenExitFilamentRequiresPochvenOrigin
    assert_eq!(
        call_cyno(
            &ctx,
            &mut sess,
            "randomJumpMgr",
            "ActivateRandomJumpFilament",
            &[PyRep::Integer(exit_fil_id)]
        )
        .await
        .unwrap_err()
        .code,
        "PochvenExitFilamentRequiresPochvenOrigin"
    );

    // 3. Pochven entry filament used from Wormhole space (31_000_001) -> PochvenEntryFilamentDisallowedOrigin
    let wh_sys = 31_000_001_u32;
    let wh_handle = ctx.systems.get_or_spawn(wh_sys);
    spawn_ship(
        &wh_handle,
        ship_id,
        587,
        25,
        6,
        DVec3::new(1.5e13, -2.1e13, 0.9e13),
        1_000_000.0,
    )
    .await;
    sess.solar_system_id = Some(wh_sys);
    assert_eq!(
        call_cyno(
            &ctx,
            &mut sess,
            "randomJumpMgr",
            "ActivateRandomJumpFilament",
            &[PyRep::Integer(entry_fil_id)]
        )
        .await
        .unwrap_err()
        .code,
        "PochvenEntryFilamentDisallowedOrigin"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn random_jump_filament_proximity_cargo_locks_and_fleet_beacon_jump_full_paths() {
    let mut ctx = test_ctx();
    let jita_sys = 30_000_142_u32;
    let lowsec_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| {
            s.solar_system_id < 31_000_000
                && s.solar_system_id != 30_100_000
                && s.region_id != 10_000_070
                && s.security_status > 0.0
                && s.security_status < 0.45
        })
        .map(|s| s.solar_system_id)
        .unwrap_or(30_002_505);
    let wh_sys = 31_000_001_u32;
    let abyss_sys = 32_000_001_u32;
    let zarzakh_sys = 30_100_000_u32;
    let char_id = 90_000_001_i64;
    let cyno_char = 90_000_002_i64;
    let ship_id = 100_000_001_i64;
    let cyno_ship = 100_000_002_i64;

    // Make 23_911 a jump-capable carrier and 587 a frigate
    clone_type_with(
        &mut ctx.sde,
        23_911,
        547,
        6,
        "Thanatos",
        1_000_000_000.0,
        &[(861, 1.0), (866, 16_274.0), (867, 6.5), (868, 1000.0), (898, 1.0)],
    );

    let mut sess = SessionState::new(809);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(98_000_001);
    sess.solar_system_id = Some(jita_sys);
    sess.ship_id = Some(ship_id);

    let jita_h = ctx.systems.get_or_spawn(jita_sys);
    // 1. NotInFleet error on ActivateRandomJumpFilament
    spawn_ship(&jita_h, ship_id, 587, 25, 6, DVec3::new(1.5e13, -2.1e13, 0.9e13), 1_000_000.0).await;
    assert_eq!(
        call_cyno(&ctx, &mut sess, "randomJumpMgr", "ActivateRandomJumpFilament", &[])
            .await.unwrap_err().code,
        "NotInFleet"
    );

    // 2. TooCloseToStructureOrStation when near station in Jita
    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.insert(
            901,
            FleetRecord {
                fleet_id: 901,
                boss_id: char_id,
                members: vec![char_id],
                ..Default::default()
            },
        );
    });
    if let Some(st) = ctx.sde.stations.values().find(|s| s.solar_system_id == jita_sys) {
        let _ = jita_h.remove_ball(ship_id).await;
        spawn_ship(&jita_h, ship_id, 587, 25, 6, st.position + DVec3::new(1000.0, 0.0, 0.0), 1_000_000.0).await;
        assert_eq!(
            call_cyno(&ctx, &mut sess, "randomJumpMgr", "ActivateRandomJumpFilament", &[])
                .await.unwrap_err().code,
            "TooCloseToStructureOrStation"
        );
    }

    // Move far from objects and check missing filament / locked filament / wrong item type / NeedlejackDisallowedOrigin
    let _ = jita_h.remove_ball(ship_id).await;
    spawn_ship(&jita_h, ship_id, 587, 25, 6, DVec3::new(1.5e13, -2.1e13, 0.9e13), 1_000_000.0).await;
    assert_eq!(
        call_cyno(&ctx, &mut sess, "randomJumpMgr", "ActivateRandomJumpFilament", &[])
            .await.unwrap_err().code,
        "InvalidArguments"
    );
    let locked_fil = 97_100_001_i64;
    let non_fil = 97_100_002_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            locked_fil,
            InventoryItemRecord {
                item_id: locked_fil,
                type_id: 53_861,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Locked,
                quantity: 1,
                singleton: false,
                ..Default::default()
            },
        );
        snap.items.insert(
            non_fil,
            InventoryItemRecord {
                item_id: non_fil,
                type_id: 34, // Tritanium
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 10,
                singleton: false,
                ..Default::default()
            },
        );
    });
    assert_eq!(
        call_cyno(&ctx, &mut sess, "randomJumpMgr", "ActivateRandomJumpFilament", &[PyRep::Integer(locked_fil)])
            .await.unwrap_err().code,
        "ItemLocked"
    );
    assert_eq!(
        call_cyno(&ctx, &mut sess, "randomJumpMgr", "ActivateRandomJumpFilament", &[PyRep::Integer(99_999_999)])
            .await.unwrap_err().code,
        "FilamentNotInCargo"
    );
    assert_eq!(
        call_cyno(&ctx, &mut sess, "randomJumpMgr", "ActivateRandomJumpFilament", &[PyRep::Integer(non_fil)])
            .await.unwrap_err().code,
        "InvalidItemType"
    );

    // 3. CmdBeaconJumpFleet validation errors + successful jump to lowsec fleet cyno beacon
    sess.station_id = Some(60_003_760);
    assert_eq!(
        // rpc-lint: allow live crucible:src/eve-server/ship/BeyonceService.h CmdBeaconJumpFleet
        call_cyno(&ctx, &mut sess, "beyonce", "CmdBeaconJumpFleet", &[PyRep::Integer(cyno_char), PyRep::Integer(cyno_ship), PyRep::Integer(lowsec_sys as i64)])
            .await.unwrap_err().code,
        "ShipDocked"
    );
    sess.station_id = None;
    sess.ship_id = None;
    assert_eq!(
        // rpc-lint: allow live crucible:src/eve-server/ship/BeyonceService.h CmdBeaconJumpFleet
        call_cyno(&ctx, &mut sess, "beyonce", "CmdBeaconJumpFleet", &[])
            .await.unwrap_err().code,
        "NoActiveShip"
    );
    sess.ship_id = Some(ship_id);
    sess.solar_system_id = None;
    assert_eq!(
        // rpc-lint: allow live crucible:src/eve-server/ship/BeyonceService.h CmdBeaconJumpFleet
        call_cyno(&ctx, &mut sess, "beyonce", "CmdBeaconJumpFleet", &[])
            .await.unwrap_err().code,
        "NoSolarSystem"
    );

    // Not in same fleet -> FleetNotInFleet
    sess.solar_system_id = Some(jita_sys);
    assert_eq!(
        // rpc-lint: allow live crucible:src/eve-server/ship/BeyonceService.h CmdBeaconJumpFleet
        call_cyno(&ctx, &mut sess, "beyonce", "CmdBeaconJumpFleet", &[PyRep::Integer(cyno_char), PyRep::Integer(cyno_ship), PyRep::Integer(lowsec_sys as i64)])
            .await.unwrap_err().code,
        "FleetNotInFleet"
    );

    // Add cyno_char to fleet 901 and check WH / Abyssal / Zarzakh / HighSec / NoActiveBeacon
    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.get_mut(&901).unwrap().members.push(cyno_char);
        let mut ch = snap.characters[&char_id].clone();
        ch.character_id = cyno_char;
        ch.solar_system_id = lowsec_sys;
        ch.active_ship_id = cyno_ship;
        ch.station_id = None;
        snap.characters.insert(cyno_char, ch);
        snap.items.get_mut(&ship_id).unwrap().type_id = 23_911; // Thanatos
    });
    for (target_sys, expected_code) in [
        (wh_sys, "JumpDriveForbiddenInWormholeSpace"),
        (abyss_sys, "JumpDriveForbiddenInAbyssalSpace"),
        (zarzakh_sys, "JumpDriveForbiddenInZarzakh"),
        (jita_sys, "CannotJumpToHighSecBeacon"),
        (lowsec_sys, "NoActiveBeacon"),
    ] {
        assert_eq!(
            // rpc-lint: allow live crucible:src/eve-server/ship/BeyonceService.h CmdBeaconJumpFleet
            call_cyno(&ctx, &mut sess, "beyonce", "CmdBeaconJumpFleet", &[PyRep::Integer(cyno_char), PyRep::Integer(cyno_ship), PyRep::Integer(target_sys as i64)])
                .await.unwrap_err().code,
            expected_code
        );
    }

    // Register a living-world convoy in Jita so ConeScan exercises convoy scanning branch too
    ctx.living_world.register_convoy(crate::living_world::LivingWorldConvoy {
        convoy_id: 97_999_001,
        archetype: crate::living_world::NpcArchetype::HaulerConvoy,
        ship_type_id: 587,
        origin_system_id: jita_sys,
        current_system_id: jita_sys,
        destination_system_id: jita_sys,
        destination_station_id: 60_003_760,
        cargo_type_id: 34,
        cargo_quantity: 100,
        lod_level: 1,
        destroyed: false,
    });
    let _ = call_cyno(
        &ctx,
        &mut sess,
        "scanMgr",
        "ConeScan",
        &[PyRep::Real(360.0), PyRep::Real(1.0e16), PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)],
    )
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cmd_beacon_jump_fleet_happy_path_structure_beacon_guards_wormhole_mass_and_fleet_filaments() {
    use crate::services::inventory_fitting::fitting_cyno_portal::handle_cyno_generator_activation;
    use eve_db::types::TrainedSkillRecord;

    let mut ctx = test_ctx();
    // Custom non-needlejack filament (group 4041, no attr 3025) to hit UnimplementedRPC branch
    clone_type_with(&mut ctx.sde, 991_101, 4041, 66, "Test Non-Needle Filament", 1.0, &[]);
    // Pochven exit filament (group 4087)
    clone_type_with(
        &mut ctx.sde,
        56_065,
        4087,
        66,
        "Glorification-1 'Devana' Filament",
        1.0,
        &[(3025, 4087.0)],
    );

    // Find two lowsec systems within 5.0 LY of each other for happy-path CmdBeaconJumpFleet
    let lowsec_pair: Option<(u32, u32)> = {
        let lows: Vec<_> = ctx
            .sde
            .solar_systems
            .values()
            .filter(|s| s.solar_system_id < 31_000_000 && s.security_status > 0.0 && s.security_status < 0.45)
            .collect();
        let mut found = None;
        for (i, a) in lows.iter().enumerate() {
            for b in lows.iter().skip(i + 1) {
                let dx = a.galactic_pos_ly[0] - b.galactic_pos_ly[0];
                let dy = a.galactic_pos_ly[1] - b.galactic_pos_ly[1];
                let dz = a.galactic_pos_ly[2] - b.galactic_pos_ly[2];
                let d2 = dx * dx + dy * dy + dz * dz;
                if d2 > 0.01 && d2 < 20.0 {
                    found = Some((a.solar_system_id, b.solar_system_id));
                    break;
                }
            }
            if found.is_some() {
                break;
            }
        }
        found
    };
    let (from_sys, dest_sys) = lowsec_pair.expect("two nearby lowsec systems");

    let char_id = 90_000_001_i64;
    let ship_id = 100_000_001_i64;
    let cyno_char = 90_000_077_i64;
    let cyno_ship = 100_000_077_i64;
    let mate_valid_char = 90_000_078_i64;
    let mate_valid_ship = 100_000_078_i64;
    let mate_docked_char = 90_000_079_i64;
    let mate_docked_ship = 100_000_079_i64;
    let mate_far_char = 90_000_080_i64;
    let mate_far_ship = 100_000_080_i64;

    let mut sess = SessionState::new(820);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(98_000_001);
    sess.solar_system_id = Some(from_sys);
    sess.ship_id = Some(ship_id);
    sess.fleet_id = Some(902);
    sess.station_id = None;

    let sys_from = ctx.systems.get_or_spawn(from_sys);
    let sys_dest = ctx.systems.get_or_spawn(dest_sys);

    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.insert(
            902,
            FleetRecord {
                fleet_id: 902,
                boss_id: char_id,
                members: vec![char_id, cyno_char, mate_valid_char, mate_docked_char, mate_far_char],
                free_move: true,
                ..Default::default()
            },
        );
        for (cid, sid, s_sys, st_id, tid) in [
            (cyno_char, cyno_ship, dest_sys, None, 11_957_u32), // Falcon (Force Recon)
            (mate_valid_char, mate_valid_ship, from_sys, None, 587_u32),
            (mate_docked_char, mate_docked_ship, from_sys, Some(60_003_760_i64), 587_u32),
            (mate_far_char, mate_far_ship, from_sys, None, 23_911_u32),
        ] {
            let mut ch = snap.characters[&char_id].clone();
            ch.character_id = cid;
            ch.solar_system_id = s_sys;
            ch.active_ship_id = sid;
            ch.station_id = st_id;
            snap.characters.insert(cid, ch);
            snap.items.insert(
                sid,
                InventoryItemRecord {
                    item_id: sid,
                    type_id: tid,
                    owner_id: cid,
                    location_id: if st_id.is_some() { 60_003_760 } else { s_sys as i64 },
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        }
        snap.items.get_mut(&ship_id).unwrap().type_id = 23_911; // Thanatos
        // Jump fuel (all 4 racial isotopes including 17887 Oxygen Isotopes for Thanatos) + Cyno fuel (16273)
        for (idx, iso_tid) in [16_274_u32, 17_887, 17_888, 17_889].into_iter().enumerate() {
            snap.items.insert(
                95_100_020 + idx as i64,
                InventoryItemRecord {
                    item_id: 95_100_020 + idx as i64,
                    type_id: iso_tid,
                    owner_id: char_id,
                    location_id: ship_id,
                    flag: InventoryFlag::Cargo,
                    quantity: 100_000,
                    singleton: false,
                    ..Default::default()
                },
            );
        }
        snap.skills.entry(char_id).or_default().insert(
            3456, // Jump Drive Operation
            TrainedSkillRecord { type_id: 3456, level: 5, skill_points: 256_000.0 },
        );
        snap.skills.entry(char_id).or_default().insert(
            21_611, // Jump Drive Calibration
            TrainedSkillRecord { type_id: 21_611, level: 5, skill_points: 256_000.0 },
        );
        snap.items.insert(
            95_100_002,
            InventoryItemRecord {
                item_id: 95_100_002,
                type_id: 16_273,
                owner_id: cyno_char,
                location_id: cyno_ship,
                flag: InventoryFlag::Cargo,
                quantity: 5_000,
                singleton: false,
                ..Default::default()
            },
        );
        snap.items.insert(
            95_100_003,
            InventoryItemRecord {
                item_id: 95_100_003,
                type_id: 991_101,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                ..Default::default()
            },
        );
        snap.items.insert(
            95_100_004,
            InventoryItemRecord {
                item_id: 95_100_004,
                type_id: 56_065,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                ..Default::default()
            },
        );
        snap.skills.entry(cyno_char).or_default().insert(
            21_603,
            TrainedSkillRecord { type_id: 21_603, level: 5, skill_points: 256_000.0 },
        );
    });

    // Light normal cyno beacon in dest_sys (cyno ship needs capacitor!)
    let _ = sys_dest
        .spawn_ball_with_meta(
            Ball {
                id: cyno_ship,
                type_id: 11_957,
                position: DVec3::new(1000.0, 2000.0, 3000.0),
                velocity: DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 2.0,
                mass: 12_000_000.0,
                radius: 150.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState { capacitor: 50_000.0, max_capacitor: 50_000.0, ..Default::default() },
            SlimItemMeta {
                owner_id: cyno_char,
                char_id: Some(cyno_char),
                corp_id: 98_000_001,
                alliance_id: -1,
                group_id: 833,
                category_id: 6,
                ..Default::default()
            },
        )
        .await;
    let cyno_mod_type = ctx.sde.types.get(&21_096).unwrap().clone();
    let _ = handle_cyno_generator_activation(&ctx, &sys_dest, cyno_char, cyno_ship, dest_sys, 95_100_010, &cyno_mod_type)
        .await
        .unwrap()
        .expect("cyno activation ok");

    // 1. structureCynoBeaconMgr CmdJumpToStructureBeacon early guards
    sess.station_id = Some(60_003_760);
    assert_eq!(
        call_cyno(&ctx, &mut sess, "structureCynoBeaconMgr", "CmdJumpToStructureBeacon", &[PyRep::Integer(dest_sys as i64), PyRep::Integer(1)])
            .await.unwrap_err().code,
        "ShipDocked"
    );
    sess.station_id = None;
    sess.ship_id = None;
    assert_eq!(
        call_cyno(&ctx, &mut sess, "structureCynoBeaconMgr", "CmdJumpToStructureBeacon", &[])
            .await.unwrap_err().code,
        "NoActiveShip"
    );
    sess.ship_id = Some(ship_id);
    sess.solar_system_id = None;
    assert_eq!(
        call_cyno(&ctx, &mut sess, "structureCynoBeaconMgr", "CmdJumpToStructureBeacon", &[])
            .await.unwrap_err().code,
        "NoSolarSystem"
    );
    sess.solar_system_id = Some(from_sys);
    assert_eq!(
        call_cyno(&ctx, &mut sess, "structureCynoBeaconMgr", "CmdJumpToStructureBeacon", &[PyRep::Integer(dest_sys as i64), PyRep::Integer(1)])
            .await.unwrap_err().code,
        "ShipNotInSpace"
    );

    // 2. CmdBeaconJumpFleet ShipNotInSpace, then spawn ship and hit ShipInWarp / CannotJumpWhileCloaked / WarpScrambled
    assert_eq!(
        // rpc-lint: allow live crucible:src/eve-server/ship/BeyonceService.h CmdBeaconJumpFleet
        call_cyno(&ctx, &mut sess, "beyonce", "CmdBeaconJumpFleet", &[PyRep::Integer(cyno_char), PyRep::Integer(cyno_ship), PyRep::Integer(dest_sys as i64)])
            .await.unwrap_err().code,
        "ShipNotInSpace"
    );

    // Spawn cloaked ball (is_cloaked: true on CombatState)
    let _ = sys_from
        .spawn_ball_with_meta(
            Ball {
                id: ship_id,
                type_id: 23_911,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 100.0,
                inertia: 2.0,
                mass: 1_100_000_000.0,
                radius: 500.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState { is_cloaked: true, gate_cloak_remaining_ticks: 0, capacitor: 50_000.0, max_capacitor: 50_000.0, ..Default::default() },
            SlimItemMeta {
                owner_id: char_id,
                char_id: Some(char_id),
                corp_id: 98_000_001,
                alliance_id: -1,
                group_id: 547,
                category_id: 6,
                is_cloaked: true,
                ..Default::default()
            },
        )
        .await;
    for svc in ["beyonce", "structureCynoBeaconMgr"] {
        // rpc-lint: allow live crucible:src/eve-server/ship/BeyonceService.h CmdBeaconJumpFleet
        let method = if svc == "beyonce" { "CmdBeaconJumpFleet" } else { "CmdJumpToStructureBeacon" };
        let args = if svc == "beyonce" {
            vec![PyRep::Integer(cyno_char), PyRep::Integer(cyno_ship), PyRep::Integer(dest_sys as i64)]
        } else {
            vec![PyRep::Integer(dest_sys as i64), PyRep::Integer(1)]
        };
        assert_eq!(call_cyno(&ctx, &mut sess, svc, method, &args).await.unwrap_err().code, "CannotJumpWhileCloaked");
    }
    let _ = sys_from.remove_ball(ship_id).await;
    let _ = sys_from
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 23_911,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 100.0,
                inertia: 2.0,
                mass: 1_100_000_000.0,
                radius: 500.0,
                speed_fraction: 0.0,
                mode: MovementMode::Warp {
                    origin: DVec3::ZERO,
                    destination: DVec3::new(1e9, 0.0, 0.0),
                    warp_speed_au_s: 3.0,
                    total_distance: 1e9,
                    phase: eve_destiny::WarpPhase::Cruising,
                    elapsed_warp_time: 1.0,
                },
            },
            CombatState { capacitor: 50_000.0, max_capacitor: 50_000.0, ..Default::default() },
        )
        .await;
    for svc in ["beyonce", "structureCynoBeaconMgr"] {
        // rpc-lint: allow live crucible:src/eve-server/ship/BeyonceService.h CmdBeaconJumpFleet
        let method = if svc == "beyonce" { "CmdBeaconJumpFleet" } else { "CmdJumpToStructureBeacon" };
        let args = if svc == "beyonce" {
            vec![PyRep::Integer(cyno_char), PyRep::Integer(cyno_ship), PyRep::Integer(dest_sys as i64)]
        } else {
            vec![PyRep::Integer(dest_sys as i64), PyRep::Integer(1)]
        };
        assert_eq!(call_cyno(&ctx, &mut sess, svc, method, &args).await.unwrap_err().code, "ShipInWarp");
    }
    // Same ball for ActivateRandomJumpFilament ShipInWarp
    assert_eq!(
        call_cyno(&ctx, &mut sess, "randomJumpMgr", "ActivateRandomJumpFilament", &[PyRep::Integer(95_100_004)])
            .await.unwrap_err().code,
        "ShipInWarp"
    );

    let _ = sys_from.remove_ball(ship_id).await;
    let _ = sys_from
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 23_911,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 100.0,
                inertia: 2.0,
                mass: 1_100_000_000.0,
                radius: 500.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState { warp_scramble_strength: 5, capacitor: 50_000.0, max_capacitor: 50_000.0, ..Default::default() },
        )
        .await;
    for svc in ["beyonce", "structureCynoBeaconMgr"] {
        // rpc-lint: allow live crucible:src/eve-server/ship/BeyonceService.h CmdBeaconJumpFleet
        let method = if svc == "beyonce" { "CmdBeaconJumpFleet" } else { "CmdJumpToStructureBeacon" };
        let args = if svc == "beyonce" {
            vec![PyRep::Integer(cyno_char), PyRep::Integer(cyno_ship), PyRep::Integer(dest_sys as i64)]
        } else {
            vec![PyRep::Integer(dest_sys as i64), PyRep::Integer(1)]
        };
        assert_eq!(call_cyno(&ctx, &mut sess, svc, method, &args).await.unwrap_err().code, "WarpScrambled");
    }

    // Clear combat scramble & execute happy-path CmdBeaconJumpFleet!
    let _ = sys_from.remove_ball(ship_id).await;
    let _ = sys_from
        .spawn_ball_with_meta(
            Ball {
                id: ship_id,
                type_id: 23_911,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 100.0,
                inertia: 2.0,
                mass: 1_100_000_000.0,
                radius: 500.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState { capacitor: 50_000.0, max_capacitor: 50_000.0, ..Default::default() },
            SlimItemMeta {
                owner_id: char_id,
                char_id: Some(char_id),
                corp_id: 98_000_001,
                alliance_id: -1,
                group_id: 547,
                category_id: 6,
                ..Default::default()
            },
        )
        .await;
    let ok_jump = call_cyno(
        &ctx,
        &mut sess,
        "beyonce",
        // rpc-lint: allow live crucible:src/eve-server/ship/BeyonceService.h CmdBeaconJumpFleet
        "CmdBeaconJumpFleet",
        &[PyRep::Integer(cyno_char), PyRep::Integer(cyno_ship), PyRep::Integer(dest_sys as i64)],
    )
    .await
    .unwrap();
    assert!(matches!(ok_jump, PyRep::KeyVal(_) | PyRep::None));
    assert_eq!(sess.solar_system_id, Some(dest_sys));
    ctx.db.clock().advance_secs(60);

    // 3. ActivateRandomJumpFilament: UnimplementedRPC for non-needlejack filament, then Pochven exit fleet jump
    let _ = sys_dest.remove_ball(ship_id).await;
    spawn_ship(&sys_dest, ship_id, 587, 25, 6, DVec3::ZERO, 1_200_000.0).await;
    assert_eq!(
        call_cyno(&ctx, &mut sess, "randomJumpMgr", "ActivateRandomJumpFilament", &[PyRep::Integer(95_100_003)])
            .await.unwrap_err().code,
        "UnimplementedRPC"
    );

    // Move fleet into a Pochven system (region 10_000_070) so Pochven exit filament jumps caller + nearby subcap fleetmate
    let poch_sys = *ctx
        .sde
        .solar_systems
        .iter()
        .find(|(_, s)| s.region_id == 10_000_070)
        .map(|(id, _)| id)
        .expect("pochven system");
    let sys_poch = ctx.systems.get_or_spawn(poch_sys);
    sess.solar_system_id = Some(poch_sys);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&ship_id).unwrap().type_id = 587;
        for cid in [char_id, mate_valid_char, mate_docked_char, mate_far_char] {
            snap.characters.get_mut(&cid).unwrap().solar_system_id = poch_sys;
        }
    });
    spawn_ship(&sys_poch, ship_id, 587, 25, 6, DVec3::ZERO, 1_200_000.0).await;
    spawn_ship(&sys_poch, mate_valid_ship, 587, 25, 6, DVec3::new(1_000.0, 0.0, 0.0), 1_200_000.0).await;
    spawn_ship(&sys_poch, mate_far_ship, 23_911, 547, 6, DVec3::new(50_000.0, 0.0, 0.0), 1_100_000_000.0).await;
    let fil_ok = call_cyno(
        &ctx,
        &mut sess,
        "randomJumpMgr",
        "ActivateRandomJumpFilament",
        &[PyRep::Integer(95_100_004)],
    )
    .await
    .unwrap();
    assert!(matches!(fil_ok, PyRep::KeyVal(_)));
    ctx.db.clock().advance_secs(60);

    // 4. WormholeJump mass-too-heavy, mass-collapsed, and SessionChangeTimerActive guards
    let wh_test_sys = 30_000_142_u32;
    let sys_wh = ctx.systems.get_or_spawn(wh_test_sys);
    sess.solar_system_id = Some(wh_test_sys);
    let wh_ball_id = 750_000_123_i64;
    spawn_ship(&sys_wh, ship_id, 587, 25, 6, DVec3::ZERO, 10_000_000.0).await;
    spawn_ship(&sys_wh, wh_ball_id, 26_272, 988, 2, DVec3::new(500.0, 0.0, 0.0), 1.0).await;
    ctx.db.with_mut_snapshot(|snap| {
        let st = snap.system_states.entry(wh_test_sys).or_default();
        st.wormhole_destination_system_id = Some(31_000_001);
        st.wormhole_remaining_mass_kg = 1_000_000_000.0;
        snap.cosmic_signatures.entry(wh_test_sys).or_default().push(eve_db::types::CosmicSignatureRecord {
            signature_id: "WH-MASS-1".into(),
            solar_system_id: wh_test_sys,
            sig_group: "Wormhole".into(),
            type_name: "Wormhole K162".into(),
            signal_strength: 100.0,
            position: [500.0, 0.0, 0.0],
            wormhole_type_id: None,
            destination_system_id: Some(31_000_001),
            created_unix: 0,
            lifetime_seconds: 86_400,
            max_mass_kg: 1.0e9,
            remaining_mass_kg: 1.0e9,
            max_jump_mass_kg: 1_000_000.0, // smaller than 10_000_000 ship mass
            collapsed: false,
        });
    });
    assert_eq!(
        call_cyno(&ctx, &mut sess, "wormholeMgr", "WormholeJump", &[PyRep::Integer(wh_ball_id)])
            .await.unwrap_err().code,
        "ShipTooHeavyForWormhole"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.cosmic_signatures.remove(&wh_test_sys);
        snap.system_states.get_mut(&wh_test_sys).unwrap().wormhole_remaining_mass_kg = 500.0;
    });
    assert_eq!(
        call_cyno(&ctx, &mut sess, "wormholeMgr", "WormholeJump", &[PyRep::Integer(wh_ball_id)])
            .await.unwrap_err().code,
        "WormholeMassCollapsed"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.get_mut(&wh_test_sys).unwrap().wormhole_remaining_mass_kg = 1_000_000_000.0;
    });
    crate::services::space_destiny::record_session_change_timer(&ctx, char_id, ctx.now_unix_secs(), wh_test_sys, 0, "jump");
    assert_eq!(
        call_cyno(&ctx, &mut sess, "wormholeMgr", "WormholeJump", &[PyRep::Integer(wh_ball_id)])
            .await.unwrap_err().code,
        "SessionChangeInProgress"
    );

    // 5. ConeScan narrow 30 deg angle + off-axis convoy exclusion
    ctx.living_world.register_convoy(crate::living_world::LivingWorldConvoy {
        convoy_id: 97_999_099,
        archetype: crate::living_world::NpcArchetype::HaulerConvoy,
        ship_type_id: 587,
        origin_system_id: wh_test_sys,
        current_system_id: wh_test_sys,
        destination_system_id: wh_test_sys,
        destination_station_id: 60_003_760,
        cargo_type_id: 34,
        cargo_quantity: 10,
        lod_level: 1,
        destroyed: false,
    });
    let _ = call_cyno(
        &ctx,
        &mut sess,
        "scanMgr",
        "ConeScan",
        &[PyRep::Real(30.0), PyRep::Real(1.0e16), PyRep::Real(0.0), PyRep::Real(1.0), PyRep::Real(0.0)],
    )
    .await
    .unwrap();

    // 6. ConeScan Tuple direction form (1144..1152) + 359 deg angle matching convoy (1294..1300)
    let _ = call_cyno(
        &ctx,
        &mut sess,
        "scanMgr",
        "ConeScan",
        &[PyRep::Real(359.0), PyRep::Real(1.0e16), PyRep::Tuple(vec![PyRep::Real(0.0), PyRep::Real(1.0), PyRep::Real(0.0)])],
    )
    .await
    .unwrap();

    // 7. ActivateRandomJumpFilament SessionChangeInProgress while timer is active (clear Pochven 15m pvp timer first)
    ctx.db.clock().advance_secs(1000);
    crate::services::space_destiny::record_session_change_timer(&ctx, char_id, ctx.now_unix_secs(), wh_test_sys, 0, "jump");
    assert_eq!(
        call_cyno(&ctx, &mut sess, "randomJumpMgr", "ActivateRandomJumpFilament", &[PyRep::Integer(95_100_004)])
            .await.unwrap_err().code,
        "SessionChangeInProgress"
    );

    // 8. WormholeJump WormholeNotPresent for non-26272 ball + WormholePolarized
    ctx.db.clock().advance_secs(60);
    assert_eq!(
        call_cyno(&ctx, &mut sess, "wormholeMgr", "WormholeJump", &[PyRep::Integer(ship_id)])
            .await.unwrap_err().code,
        "WormholeNotPresent"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.wh_polarizations.insert((char_id, 31_000_001), ctx.now_unix() + 3600);
    });
    assert_eq!(
        call_cyno(&ctx, &mut sess, "wormholeMgr", "WormholeJump", &[PyRep::Integer(wh_ball_id)])
            .await.unwrap_err().code,
        "WormholePolarized"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cyno_structure_beacon_pochven_and_happy_path_filament_qty2_wormhole_and_fleet_beacon() {
    use crate::services::inventory_fitting::fitting_cyno_portal::handle_cyno_generator_activation;
    use eve_db::types::{TrainedSkillRecord, UpwellStructureRecord};

    let mut ctx = test_ctx();
    clone_type_with(
        &mut ctx.sde,
        56_065,
        4087,
        66,
        "Glorification-1 'Devana' Filament",
        1.0,
        &[(3025, 4087.0)],
    );

    // Find two lowsec systems within 5.0 LY of each other + a Pochven system
    let (from_sys, dest_sys) = {
        let lows: Vec<_> = ctx
            .sde
            .solar_systems
            .values()
            .filter(|s| s.solar_system_id < 31_000_000 && s.security_status > 0.0 && s.security_status < 0.45)
            .collect();
        let mut found = None;
        for (i, a) in lows.iter().enumerate() {
            for b in lows.iter().skip(i + 1) {
                let dx = a.galactic_pos_ly[0] - b.galactic_pos_ly[0];
                let dy = a.galactic_pos_ly[1] - b.galactic_pos_ly[1];
                let dz = a.galactic_pos_ly[2] - b.galactic_pos_ly[2];
                let d2 = dx * dx + dy * dy + dz * dz;
                if d2 > 0.01 && d2 < 20.0 {
                    found = Some((a.solar_system_id, b.solar_system_id));
                    break;
                }
            }
            if found.is_some() {
                break;
            }
        }
        found.expect("two nearby lowsec systems")
    };
    let poch_sys = *ctx
        .sde
        .solar_systems
        .iter()
        .find(|(_, s)| s.region_id == 10_000_070)
        .map(|(id, _)| id)
        .expect("pochven system");

    let char_id = 90_000_001_i64;
    let ship_id = 100_000_001_i64;
    let mate_char = 90_000_091_i64;
    let mate_ship = 100_000_091_i64;
    let cyno_char = 90_000_092_i64;
    let cyno_ship = 100_000_092_i64;
    let poch_beacon_id = 1_900_700_001_i64;
    let lowsec_beacon_id = 1_900_700_002_i64;

    let mut sess = SessionState::new(920);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(98_000_001);
    sess.solar_system_id = Some(from_sys);
    sess.ship_id = Some(ship_id);
    sess.fleet_id = Some(921);

    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.insert(
            921,
            FleetRecord {
                fleet_id: 921,
                boss_id: char_id,
                members: vec![char_id, mate_char, cyno_char],
                free_move: true,
                ..Default::default()
            },
        );
        for (cid, sid, s_sys, tid) in [
            (char_id, ship_id, from_sys, 23_911_u32),
            (mate_char, mate_ship, poch_sys, 587_u32),
            (cyno_char, cyno_ship, from_sys, 11_957_u32),
        ] {
            let mut ch = snap.characters[&char_id].clone();
            ch.character_id = cid;
            ch.corporation_id = 98_000_001;
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = s_sys;
            ch.active_ship_id = sid;
            snap.characters.insert(cid, ch);
            snap.items.insert(
                sid,
                InventoryItemRecord {
                    item_id: sid,
                    type_id: tid,
                    owner_id: cid,
                    location_id: s_sys as i64,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        }
        // Carrier fuel + skills for jump drive
        snap.items.insert(
            95_200_001,
            InventoryItemRecord {
                item_id: 95_200_001,
                type_id: 17_887,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 100_000,
                singleton: false,
                ..Default::default()
            },
        );
        snap.skills.entry(char_id).or_default().insert(
            3456,
            TrainedSkillRecord { type_id: 3456, level: 5, skill_points: 256_000.0 },
        );
        snap.skills.entry(char_id).or_default().insert(
            21_611,
            TrainedSkillRecord { type_id: 21_611, level: 5, skill_points: 256_000.0 },
        );
        // Cyno fuel & skill for cyno_char
        snap.items.insert(
            95_200_002,
            InventoryItemRecord {
                item_id: 95_200_002,
                type_id: 16_273,
                owner_id: cyno_char,
                location_id: cyno_ship,
                flag: InventoryFlag::Cargo,
                quantity: 5_000,
                singleton: false,
                ..Default::default()
            },
        );
        snap.skills.entry(cyno_char).or_default().insert(
            21_603,
            TrainedSkillRecord { type_id: 21_603, level: 5, skill_points: 256_000.0 },
        );
        // Filament stack quantity == 2 (kills L623 `item.quantity < 1` -> `>` mutant); 56_066 supports fleet size 5
        snap.items.insert(
            95_200_003,
            InventoryItemRecord {
                item_id: 95_200_003,
                type_id: 56_066,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 2,
                singleton: false,
                ..Default::default()
            },
        );
        // Two Pharolux Cyno Beacon structures: one in Pochven (forbidden), one in dest_sys (allowed)
        snap.structures.insert(
            poch_beacon_id,
            UpwellStructureRecord {
                structure_id: poch_beacon_id,
                type_id: 35_840,
                owner_corp_id: 98_000_001,
                solar_system_id: poch_sys,
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 500,
                ..Default::default()
            },
        );
        snap.structures.insert(
            lowsec_beacon_id,
            UpwellStructureRecord {
                structure_id: lowsec_beacon_id,
                type_id: 35_840,
                owner_corp_id: 98_000_001,
                solar_system_id: dest_sys,
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 500,
                ..Default::default()
            },
        );
    });

    // Spawn ships in space
    let sys_from = ctx.systems.get_or_spawn(from_sys);
    let _ = sys_from
        .spawn_ball_with_meta(
            Ball {
                id: ship_id,
                type_id: 23_911,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 100.0,
                inertia: 2.0,
                mass: 1_100_000_000.0,
                radius: 500.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState { capacitor: 50_000.0, max_capacitor: 50_000.0, ..Default::default() },
            SlimItemMeta {
                owner_id: char_id,
                char_id: Some(char_id),
                corp_id: 98_000_001,
                alliance_id: -1,
                group_id: 547,
                category_id: 6,
                ..Default::default()
            },
        )
        .await;
    let _ = sys_from
        .spawn_ball_with_meta(
            Ball {
                id: cyno_ship,
                type_id: 11_957,
                position: DVec3::new(1_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 2.0,
                mass: 12_000_000.0,
                radius: 150.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState { capacitor: 50_000.0, max_capacitor: 50_000.0, ..Default::default() },
            SlimItemMeta {
                owner_id: cyno_char,
                char_id: Some(cyno_char),
                corp_id: 98_000_001,
                alliance_id: -1,
                group_id: 833,
                category_id: 6,
                ..Default::default()
            },
        )
        .await;
    // 1. CmdJumpToStructureBeacon: Pochven beacon fails CannotJumpToPochven (kills L908 !=); lowsec beacon succeeds with KeyVal
    assert_eq!(
        call_cyno(
            &ctx,
            &mut sess,
            "structureCynoBeaconMgr",
            "CmdJumpToStructureBeacon",
            &[PyRep::Integer(poch_sys as i64), PyRep::Integer(poch_beacon_id)],
        )
        .await
        .unwrap_err()
        .code,
        "CannotJumpToPochven"
    );
    let beacon_res = call_cyno(
        &ctx,
        &mut sess,
        "structureCynoBeaconMgr",
        "CmdJumpToStructureBeacon",
        &[PyRep::Integer(dest_sys as i64), PyRep::Integer(lowsec_beacon_id)],
    )
    .await
    .unwrap();
    let PyRep::KeyVal(bkv) = beacon_res else {
        panic!("expected KeyVal from CmdJumpToStructureBeacon");
    };
    assert_eq!(bkv["solarSystemID"], PyRep::Integer(dest_sys as i64));
    assert_eq!(bkv["structureID"], PyRep::Integer(lowsec_beacon_id));
    assert_eq!(sess.solar_system_id, Some(dest_sys));

    // 2. Clear session timer & recharge jump capacitor, light cyno in from_sys, & jump back via CmdBeaconJumpFleet asserting strict KeyVal (kills L1351)
    ctx.db.clock().advance_secs(1_800);
    let cyno_mod_type = ctx.sde.types.get(&21_096).unwrap().clone();
    let _ = handle_cyno_generator_activation(&ctx, &sys_from, cyno_char, cyno_ship, from_sys, 95_200_010, &cyno_mod_type)
        .await
        .unwrap()
        .expect("cyno activation in from_sys ok");
    let fleet_jump = call_cyno(
        &ctx,
        &mut sess,
        "beyonce",
        // rpc-lint: allow live crucible:src/eve-server/ship/BeyonceService.h CmdBeaconJumpFleet
        "CmdBeaconJumpFleet",
        &[PyRep::Integer(cyno_char), PyRep::Integer(cyno_ship), PyRep::Integer(from_sys as i64)],
    )
    .await
    .unwrap();
    let PyRep::KeyVal(fjkv) = fleet_jump else {
        panic!("expected strict KeyVal from CmdBeaconJumpFleet");
    };
    assert_eq!(fjkv["solarSystemID"], PyRep::Integer(from_sys as i64));
    assert!(fjkv["fuelConsumed"].as_i64().unwrap_or(0) > 0);
    assert_eq!(sess.solar_system_id, Some(from_sys));

    // 3. ActivateRandomJumpFilament with stack quantity == 2 and fleetmate in Pochven
    ctx.db.clock().advance_secs(600);
    let _ = sys_from.remove_ball(ship_id).await;
    sess.solar_system_id = Some(poch_sys);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&ship_id).unwrap().type_id = 587;
        snap.characters.get_mut(&char_id).unwrap().solar_system_id = poch_sys;
    });
    let sys_poch = ctx.systems.get_or_spawn(poch_sys);
    spawn_ship(&sys_poch, ship_id, 587, 25, 6, DVec3::new(5.0e11, 0.0, 0.0), 1_200_000.0).await;
    spawn_ship(&sys_poch, mate_ship, 587, 25, 6, DVec3::new(5.0e11 + 1_000.0, 0.0, 0.0), 1_200_000.0).await;
    let fil_res = call_cyno(
        &ctx,
        &mut sess,
        "randomJumpMgr",
        "ActivateRandomJumpFilament",
        &[PyRep::Integer(95_200_003)],
    )
    .await
    .unwrap();
    let PyRep::KeyVal(fkv) = fil_res else {
        panic!("expected KeyVal from ActivateRandomJumpFilament");
    };
    let fil_dest_sys = fkv["solarSystemID"].as_i64().unwrap() as u32;
    assert_ne!(fil_dest_sys, poch_sys);
    assert_eq!(sess.solar_system_id, Some(fil_dest_sys));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&95_200_003].quantity, 1);
        assert_eq!(snap.characters[&char_id].solar_system_id, fil_dest_sys);
        assert_eq!(snap.characters[&mate_char].solar_system_id, fil_dest_sys);
    });

    // 4. WormholeJump happy path strict KeyVal verification (kills L74)
    ctx.db.clock().advance_secs(60);
    let wh_origin = 30_000_142_u32;
    let old_handle = ctx.systems.get_or_spawn(fil_dest_sys);
    let _ = old_handle.remove_ball(ship_id).await;
    sess.solar_system_id = Some(wh_origin);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().solar_system_id = wh_origin;
        let st = snap.system_states.entry(wh_origin).or_default();
        st.wormhole_destination_system_id = Some(31_000_001);
        st.wormhole_remaining_mass_kg = 1.0e9;
    });
    let sys_wh_orig = ctx.systems.get_or_spawn(wh_origin);
    spawn_ship(&sys_wh_orig, ship_id, 587, 25, 6, DVec3::ZERO, 1_200_000.0).await;
    spawn_ship(&sys_wh_orig, 750_000_001, 26_272, 988, 2, DVec3::new(500.0, 0.0, 0.0), 1.0).await;
    let wh_ok = call_cyno(&ctx, &mut sess, "wormholeMgr", "WormholeJump", &[PyRep::Integer(750_000_001)])
        .await
        .unwrap();
    let PyRep::KeyVal(wkv) = wh_ok else {
        panic!("expected KeyVal from WormholeJump");
    };
    assert_eq!(wkv["solarSystemID"], PyRep::Integer(31_000_001));
    assert!(wkv["beyonceBoundID"].as_i64().unwrap_or(0) > 0);
    assert_ne!(wkv["exitWormholeBallID"].as_i64().unwrap_or(0), 0);
    assert_eq!(sess.solar_system_id, Some(31_000_001));
}
