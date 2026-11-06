//! Unit tests covering `r6_t1_p0_helpers` (`prepare_tube_fighter_stack_for_launch`,
//! `enforce_legacy_pvp_safety_and_concord`, `execute_lancer_beam_attack`,
//! `expire_overdue_probes_for_char`, `expire_all_overdue_probes`,
//! `verify_special_site_caller_on_grid`, `resolve_special_site_string_instance_id`,
//! `is_known_special_site_instance_id`).

use super::*;
use crate::router::{ActiveFighterSquadron, ActiveScannerProbe, ServerContext};
use crate::services::combat_crimewatch::{with_mut_combat_runtime, SafetyLevel};
use eve_db::{
    CharacterRecord, InventoryFlag, InventoryItemRecord, IskCents, KillRightRecord,
    WorldRepository,
};
use eve_destiny::{Ball, CombatState, DamageVector, MovementMode};
use eve_machonet::SessionState;
use eve_sde::SdeRegistry;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn fresh_ctx() -> ServerContext {
    ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    )
}

#[tokio::test]
async fn prepare_tube_fighter_stack_and_expire_overdue_probes_cover_splits_and_capacity() {
    let ctx = fresh_ctx();
    let char_id = 98_100_001i64;
    let ship_id = 98_100_100i64;
    let tube_stack_id = 98_100_101i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 23757, // Archon
                owner_id: char_id,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::from([(38u16, 200.0)]),
                ..Default::default()
            },
        );
        snap.items.insert(
            tube_stack_id,
            InventoryItemRecord {
                item_id: tube_stack_id,
                type_id: 23055,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::FighterBay,
                quantity: 12,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    // Missing item returns None
    assert!(prepare_tube_fighter_stack_for_launch(&ctx, 999_999, ship_id, 9).is_none());

    // Oversized stack splits excess 3 into a separate FighterBay stack
    let capped = prepare_tube_fighter_stack_for_launch(&ctx, tube_stack_id, ship_id, 9)
        .expect("fighter stack");
    assert_eq!(capped.quantity, 9);
    let excess_total: i32 = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .filter(|i| {
                i.location_id == ship_id
                    && i.flag == InventoryFlag::FighterBay
                    && i.item_id != tube_stack_id
            })
            .map(|i| i.quantity)
            .sum()
    });
    assert_eq!(excess_total, 3);

    // Scanner probe expiry: no overdue probes -> empty
    assert_eq!(expire_overdue_probes_for_char(&ctx, char_id, 1_000).await, (Vec::new(), Vec::new()));

    // Insert two overdue probes: one fits in cargo, second exceeds cargo and is lost (state = -2)
    let probe1 = 98_100_201i64;
    let probe2 = 98_100_202i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            98_100_203,
            InventoryItemRecord {
                item_id: 98_100_203,
                type_id: 34, // Tritanium taking 199.95 m3 of 200 m3 cargo
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 19_995,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });
    {
        let mut map = ctx.active_scanner_probes.write().unwrap();
        map.insert(
            probe1,
            ActiveScannerProbe {
                probe_id: probe1,
                probe_type_id: 30013,
                owner_char_id: char_id,
                ship_id,
                launcher_module_id: 98_100_204,
                solar_system_id: 30_000_142,
                position: [0.0; 3],
                destination: [0.0; 3],
                range_step: 4,
                scan_range_m: 1_000_000.0,
                base_sensor_strength: 40.0,
                max_flight_time_s: 600.0,
                launched_at_unix: 100,
                state: 1,
                warp_end_ft: None,
            },
        );
        map.insert(
            probe2,
            ActiveScannerProbe {
                probe_id: probe2,
                probe_type_id: 30013,
                owner_char_id: char_id,
                ship_id,
                launcher_module_id: 98_100_204,
                solar_system_id: 30_000_142,
                position: [100.0, 0.0, 0.0],
                destination: [100.0, 0.0, 0.0],
                range_step: 4,
                scan_range_m: 1_000_000.0,
                base_sensor_strength: 40.0,
                max_flight_time_s: 600.0,
                launched_at_unix: 100,
                state: 1,
                warp_end_ft: None,
            },
        );
    }

    expire_all_overdue_probes(&ctx, 1_000).await;
    let map = ctx.active_scanner_probes.read().unwrap();
    assert!(map.values().any(|p| p.state < 0));
}

#[tokio::test]
async fn pvp_safety_concord_and_lancer_beam_enforce_crimewatch_and_damage() {
    let ctx = fresh_ctx();
    let sys_id = 30_000_142u32; // Jita (0.946 highsec)
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let attacker_char = 98_200_001i64;
    let victim_char = 98_200_002i64;
    let attacker_ship = 98_200_101i64;
    let victim_ship = 98_200_102i64;
    let pod_ship = 98_200_103i64;
    let now_unix = 1_750_000_000u64;

    ctx.db.with_mut_snapshot(|snap| {
        let base_char = snap
            .characters
            .get(&90_000_001)
            .expect("seed char")
            .clone();
        snap.characters.insert(
            attacker_char,
            CharacterRecord {
                character_id: attacker_char,
                user_id: 1,
                name: "Attacker".into(),
                corporation_id: 1_000_009,
                solar_system_id: sys_id,
                station_id: None,
                structure_id: None,
                home_station_id: 60_003_760,
                active_ship_id: attacker_ship,
                balance: IskCents::from_isk(100_000),
                ..base_char.clone()
            },
        );
        snap.characters.insert(
            victim_char,
            CharacterRecord {
                character_id: victim_char,
                user_id: 2,
                name: "Victim".into(),
                corporation_id: 1_000_010,
                solar_system_id: sys_id,
                station_id: None,
                structure_id: None,
                home_station_id: 60_003_760,
                active_ship_id: victim_ship,
                balance: IskCents::from_isk(100_000),
                ..base_char
            },
        );
        snap.items.insert(
            attacker_ship,
            InventoryItemRecord {
                item_id: attacker_ship,
                type_id: 587,
                owner_id: attacker_char,
                location_id: sys_id as i64,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            victim_ship,
            InventoryItemRecord {
                item_id: victim_ship,
                type_id: 587,
                owner_id: victim_char,
                location_id: sys_id as i64,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    let att_ball = Ball {
        id: attacker_ship,
        type_id: 587,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let vic_ball = Ball {
        id: victim_ship,
        type_id: 587,
        position: DVec3::new(5_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let pod_ball = Ball {
        id: pod_ship,
        type_id: 670,
        position: DVec3::new(6_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 150.0,
        inertia: 1.0,
        mass: 100_000.0,
        radius: 20.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let _ = sys_handle.spawn_ball(att_ball.clone()).await;
    let _ = sys_handle.spawn_ball(vic_ball.clone()).await;
    let _ = sys_handle.spawn_ball(pod_ball.clone()).await;
    let mut v_cs = CombatState::default();
    v_cs.max_structure_hp = 500.0;
    v_cs.structure_hp = 500.0;
    v_cs.structure_resonance = [1.0; 4];
    v_cs.max_capacitor = 500.0;
    v_cs.capacitor = 400.0;
    let _ = sys_handle.set_combat_state(victim_ship, v_cs).await;

    // Green safety blocks illegal highsec aggression
    with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(attacker_char, SafetyLevel::Green);
    });
    let mut sess = SessionState::new(98_201);
    sess.user_id = Some(1);
    sess.character_id = Some(attacker_char);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(attacker_ship);

    let safety_err = enforce_legacy_pvp_safety_and_concord(
        &ctx,
        &mut sess,
        &sys_handle,
        attacker_char,
        attacker_ship,
        &att_ball,
        sys_id,
        0.95,
        victim_ship,
        &vic_ball,
        victim_char,
        now_unix,
    )
    .await
    .expect_err("green safety blocks illegal attack");
    assert_eq!(safety_err.code, "CantDoInCurrentSafetyLevel");

    // KillRight + suspect flag makes target lawful under Red safety and adds limited engagement
    with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(attacker_char, SafetyLevel::Red);
    });
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        cw.get_or_create(victim_char).suspect_flag_until_unix = now_unix + 900;
    }
    ctx.db.with_mut_snapshot(|snap| {
        snap.kill_rights.insert(
            777,
            KillRightRecord {
                kill_right_id: 777,
                owner_id: attacker_char,
                target_id: victim_char,
                activation_fee: IskCents::from_isk(0),
                is_active_suspect: false,
            },
        );
    });
    let (_, kr_concord) = enforce_legacy_pvp_safety_and_concord(
        &ctx,
        &mut sess,
        &sys_handle,
        attacker_char,
        attacker_ship,
        &att_ball,
        sys_id,
        0.95,
        victim_ship,
        &vic_ball,
        victim_char,
        now_unix,
    )
    .await
    .expect("lawful via kill right");
    assert!(kr_concord.is_none());

    // Lancer beam hits victim ship + pod and applies neut/scramble debuffs
    let hits = execute_lancer_beam_attack(
        &ctx,
        &sys_handle,
        sys_id,
        attacker_char,
        attacker_ship,
        587,
        2873,
        &[att_ball, vic_ball, pod_ball],
        DVec3::ZERO,
        DVec3::X,
        20_000.0,
        2_000.0,
        5_000.0,
        DamageVector { em: 100.0, thermal: 0.0, kinetic: 0.0, explosive: 0.0 },
        150.0,
        30,
        now_unix,
    )
    .await
    .expect("lancer beam attack");
    assert!(hits.contains(&victim_ship));
    let after_cs = sys_handle.get_combat_state(victim_ship).await.unwrap().unwrap();
    assert_eq!(after_cs.capacitor, 250.0);
    assert_eq!(after_cs.lance_disrupted_remaining_s, 30.0);
    let _ = ActiveFighterSquadron {
        ball_id: 1,
        item_id: 1,
        type_id: 23055,
        owner_char_id: attacker_char,
        parent_ship_id: attacker_ship,
        tube_index: 0,
        squadron_size: 9,
        target_id: None,
        is_abandoned: false,
        mwd_active: false,
    };
}

#[tokio::test]
async fn special_site_grid_and_string_resolution_checks_work() {
    let ctx = fresh_ctx();
    let sys_id = 30_000_142u32;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let char_id = 98_300_001i64;
    let ship_id = 98_300_100i64;

    let mut sess = SessionState::new(98_301);
    sess.character_id = Some(char_id);
    sess.station_id = Some(60_003_760);

    // Docked rejected
    assert_eq!(
        verify_special_site_caller_on_grid(&ctx, &sess, &sys_handle, char_id, DVec3::ZERO, "Site")
            .await
            .expect_err("docked")
            .code,
        "ShipDocked"
    );

    // Undocked without solar system rejected
    sess.station_id = None;
    assert_eq!(
        verify_special_site_caller_on_grid(&ctx, &sess, &sys_handle, char_id, DVec3::ZERO, "Site")
            .await
            .expect_err("no system")
            .code,
        "NotInSpace"
    );

    // Cloaked / too far / on-grid checks
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);
    let _ = sys_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 587,
            position: DVec3::new(250_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    assert_eq!(
        verify_special_site_caller_on_grid(&ctx, &sess, &sys_handle, char_id, DVec3::ZERO, "Site")
            .await
            .expect_err("too far")
            .code,
        "TooFarFromSite"
    );

    let _ = sys_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 587,
            position: DVec3::new(10_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    verify_special_site_caller_on_grid(&ctx, &sess, &sys_handle, char_id, DVec3::ZERO, "Site")
        .await
        .expect("on grid succeeds");

    // String instance ID resolution
    assert!(resolve_special_site_string_instance_id(&ctx, "INVALID").is_none());
    assert_eq!(
        resolve_special_site_string_instance_id(&ctx, "HF-30000142-5001"),
        Some(5001)
    );
    assert_eq!(
        resolve_special_site_string_instance_id(&ctx, "TRIG-30000142-6001"),
        Some(6001)
    );
    assert!(!is_known_special_site_instance_id(&ctx, 999_888_777));
}
