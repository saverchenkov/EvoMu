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

#[tokio::test]
async fn group_2b_fighter_no_split_concord_branches_lancer_annulus_probe_merge_and_sites() {
    use eve_db::{DrifterHiveRecord, HomefrontSiteRecord, TriglavianSiteRecord};

    // 1. prepare_tube_fighter_stack_for_launch: qty <= cap returns unmodified stack without splitting.
    let ctx = fresh_ctx();
    let char_id = 98_400_001i64;
    let ship_id = 98_400_100i64;
    let small_stack_id = 98_400_101i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 23757,
                owner_id: char_id,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            small_stack_id,
            InventoryItemRecord {
                item_id: small_stack_id,
                type_id: 23055,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::FighterBay,
                quantity: 6,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });
    let unsplit = prepare_tube_fighter_stack_for_launch(&ctx, small_stack_id, ship_id, 9)
        .expect("unsplit stack");
    assert_eq!(unsplit.quantity, 6);
    assert_eq!(
        ctx.db.with_snapshot(|s| {
            s.items
                .values()
                .filter(|i| i.location_id == ship_id && i.flag == InventoryFlag::FighterBay)
                .count()
        }),
        1
    );

    // 2. expire_overdue_probes_for_char: overdue merges onto existing Cargo probe stack; non-overdue kept.
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            98_400_102,
            InventoryItemRecord {
                item_id: 98_400_102,
                type_id: 30013,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 2,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });
    {
        let mut map = ctx.active_scanner_probes.write().unwrap();
        map.insert(
            98_400_201,
            ActiveScannerProbe {
                probe_id: 98_400_201,
                probe_type_id: 30013,
                owner_char_id: char_id,
                ship_id,
                launcher_module_id: 98_400_210,
                solar_system_id: 30_000_142,
                position: [0.0; 3],
                destination: [0.0; 3],
                range_step: 4,
                scan_range_m: 1_000_000.0,
                base_sensor_strength: 40.0,
                max_flight_time_s: 500.0,
                launched_at_unix: 100,
                state: 1,
                warp_end_ft: None,
            },
        );
        map.insert(
            98_400_202,
            ActiveScannerProbe {
                probe_id: 98_400_202,
                probe_type_id: 30013,
                owner_char_id: char_id,
                ship_id,
                launcher_module_id: 98_400_210,
                solar_system_id: 30_000_142,
                position: [0.0; 3],
                destination: [0.0; 3],
                range_step: 4,
                scan_range_m: 1_000_000.0,
                base_sensor_strength: 40.0,
                max_flight_time_s: 600.0,
                launched_at_unix: 800,
                state: 1,
                warp_end_ft: None,
            },
        );
    }
    let (ret, lost) = expire_overdue_probes_for_char(&ctx, char_id, 1_000).await;
    assert_eq!(ret, vec![98_400_201]);
    assert!(lost.is_empty());
    assert_eq!(
        ctx.db.with_snapshot(|s| s.items[&98_400_102].quantity),
        3,
        "expired probe must merge into existing Cargo stack"
    );
    assert_eq!(
        ctx.active_scanner_probes.read().unwrap()[&98_400_202].state,
        1
    );

    // 3. enforce_legacy_pvp_safety_and_concord: Yellow allows lowsec suspect ship, blocks lowsec pod;
    // Red triggers CONCORD death pipeline (ship->capsule, pod->podless, missing->Err killmail fallback).
    let sys_id = 30_000_142u32;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let att_char = 98_500_001i64;
    let vic_char = 98_500_002i64;
    let att_ship = 98_500_101i64;
    let att_pod = 98_500_102i64;
    let vic_ship = 98_500_103i64;
    let vic_pod = 98_500_104i64;
    let now_unix = 1_760_000_000u64;
    ctx.db.with_mut_snapshot(|snap| {
        let base_ch = snap.characters[&90_000_001].clone();
        snap.characters.insert(
            att_char,
            CharacterRecord {
                character_id: att_char,
                user_id: 91,
                name: "Att2".into(),
                corporation_id: 1_000_009,
                solar_system_id: sys_id,
                station_id: None,
                structure_id: None,
                home_station_id: 60_003_760,
                active_ship_id: att_ship,
                balance: IskCents::from_isk(100_000),
                ..base_ch.clone()
            },
        );
        snap.characters.insert(
            vic_char,
            CharacterRecord {
                character_id: vic_char,
                user_id: 92,
                name: "Vic2".into(),
                corporation_id: 1_000_010,
                solar_system_id: sys_id,
                station_id: None,
                structure_id: None,
                home_station_id: 60_003_760,
                active_ship_id: vic_ship,
                balance: IskCents::from_isk(100_000),
                ..base_ch
            },
        );
        for (id, tid, owner) in [
            (att_ship, 587u32, att_char),
            (att_pod, 670u32, att_char),
            (vic_ship, 587u32, vic_char),
        ] {
            snap.items.insert(
                id,
                InventoryItemRecord {
                    item_id: id,
                    type_id: tid,
                    owner_id: owner,
                    location_id: sys_id as i64,
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
    });

    let mk_ball = |id: i64, type_id: u32, pos: DVec3, mass: f64, max_velocity: f64, radius: f64| Ball {
        id,
        type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity,
        inertia: 1.0,
        mass,
        radius,
        speed_fraction: 0.0,
        mode: eve_destiny::MovementMode::Stop,
    };
    let att_ball = mk_ball(att_ship, 587, DVec3::ZERO, 1_000_000.0, 300.0, 50.0);
    let att_pod_ball = mk_ball(att_pod, 670, DVec3::ZERO, 100_000.0, 150.0, 20.0);
    let vic_ball = mk_ball(vic_ship, 587, DVec3::new(5_000.0, 0.0, 0.0), 1_000_000.0, 300.0, 50.0);
    let pod_ball = mk_ball(vic_pod, 670, DVec3::new(6_000.0, 0.0, 0.0), 100_000.0, 150.0, 20.0);
    let _ = sys_handle.spawn_ball(att_ball.clone()).await;
    let _ = sys_handle.spawn_ball(att_pod_ball.clone()).await;
    let _ = sys_handle.spawn_ball(vic_ball.clone()).await;
    let _ = sys_handle.spawn_ball(pod_ball.clone()).await;

    let mut sess = SessionState::new(98_501);
    sess.user_id = Some(91);
    sess.character_id = Some(att_char);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(att_ship);

    with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(att_char, SafetyLevel::Yellow);
    });
    // Lowsec ship -> suspect -> allowed by Yellow safety; Lowsec pod -> criminal -> blocked by Yellow safety.
    // Use an actual low-sec system from SDE so `evaluate_and_record_pvp_aggression` resolves 0.0 < sys_sec < 0.45.
    let (low_sys_id, low_sec) = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.security_status >= 0.05 && s.security_status < 0.45)
        .map(|s| (s.solar_system_id, s.security_status))
        .expect("canonical SDE contains at least one low-sec solar system");
    let low_sys_handle = ctx.systems.get_or_spawn(low_sys_id);
    let _ = low_sys_handle.spawn_ball(att_ball.clone()).await;
    let _ = low_sys_handle.spawn_ball(vic_ball.clone()).await;
    let _ = low_sys_handle.spawn_ball(pod_ball.clone()).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&att_char).unwrap().solar_system_id = low_sys_id;
        snap.characters.get_mut(&vic_char).unwrap().solar_system_id = low_sys_id;
    });
    sess.solar_system_id = Some(low_sys_id);

    let (legal, concord) = enforce_legacy_pvp_safety_and_concord(
        &ctx,
        &mut sess,
        &low_sys_handle,
        att_char,
        att_ship,
        &att_ball,
        low_sys_id,
        low_sec,
        vic_ship,
        &vic_ball,
        vic_char,
        now_unix,
    )
    .await
    .expect("Yellow allows lowsec suspect ship aggression");
    assert!(!legal);
    assert!(concord.is_none());

    // Lowsec pod -> criminal -> blocked by Yellow safety
    let pod_err = enforce_legacy_pvp_safety_and_concord(
        &ctx,
        &mut sess,
        &low_sys_handle,
        att_char,
        att_ship,
        &att_ball,
        low_sys_id,
        low_sec,
        vic_pod,
        &pod_ball,
        vic_char,
        now_unix,
    )
    .await
    .expect_err("Yellow blocks lowsec pod criminal action");
    assert_eq!(pod_err.code, "CantDoInCurrentSafetyLevel");

    // Restore high-sec Jita system for Red CONCORD death pipeline checks
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&att_char).unwrap().solar_system_id = sys_id;
        snap.characters.get_mut(&vic_char).unwrap().solar_system_id = sys_id;
    });
    sess.solar_system_id = Some(sys_id);

    // Red safety in highsec -> triggers CONCORD execute_death_pipeline Ok(Some(capsule_id))
    with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(att_char, SafetyLevel::Red);
    });
    let (_, conc1) = enforce_legacy_pvp_safety_and_concord(
        &ctx,
        &mut sess,
        &sys_handle,
        att_char,
        att_ship,
        &att_ball,
        sys_id,
        0.95,
        vic_ship,
        &vic_ball,
        vic_char,
        now_unix,
    )
    .await
    .expect("CONCORD pipeline runs");
    let conc1 = conc1.expect("CONCORD response payload");
    assert!(conc1.dict_get("wreckID").is_some());
    assert!(matches!(conc1.dict_get("capsuleID"), Some(PyRep::Integer(_))));

    // Attacker already in pod -> execute_death_pipeline Ok with ejected_capsule_id == None
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&att_char).unwrap().active_ship_id = att_pod;
    });
    sess.ship_id = Some(att_pod);
    let (_, conc_pod) = enforce_legacy_pvp_safety_and_concord(
        &ctx,
        &mut sess,
        &sys_handle,
        att_char,
        att_pod,
        &att_pod_ball,
        sys_id,
        0.95,
        vic_ship,
        &vic_ball,
        vic_char,
        now_unix,
    )
    .await
    .expect("CONCORD podless branch runs");
    assert_eq!(
        conc_pod.expect("podless payload").dict_get("capsuleID"),
        Some(&PyRep::None)
    );

    // Missing attacker ship item -> execute_death_pipeline Err fallback records synthetic killmail
    let (_, conc_err) = enforce_legacy_pvp_safety_and_concord(
        &ctx,
        &mut sess,
        &sys_handle,
        att_char,
        98_599_999,
        &att_ball,
        sys_id,
        0.95,
        vic_ship,
        &vic_ball,
        vic_char,
        now_unix,
    )
    .await
    .expect("CONCORD Err fallback records killmail");
    let conc_err = conc_err.expect("Err fallback payload");
    assert!(conc_err.dict_get("concordKillmailID").is_some());
    assert!(conc_err.dict_get("wreckID").is_none());

    // 4. collect_and_precheck_lancer_cone_targets & execute_lancer_beam_attack:
    // spatial culls, invuln/tether/gate_cloak/undock skips, outer neut annulus, lethal destroy, & modified sec.
    let t_behind = mk_ball(98_600_001, 587, DVec3::new(50.0, 0.0, 0.0), 1_000_000.0, 0.0, 50.0);
    let t_oob_range = mk_ball(98_600_002, 587, DVec3::new(25_000.0, 0.0, 0.0), 1_000_000.0, 0.0, 50.0);
    let t_oob_perp = mk_ball(98_600_003, 587, DVec3::new(5_000.0, 6_000.0, 0.0), 1_000_000.0, 0.0, 50.0);
    let t_tether = mk_ball(98_600_004, 587, DVec3::new(5_000.0, 0.0, 0.0), 1_000_000.0, 0.0, 50.0);
    let t_undock = mk_ball(98_600_005, 587, DVec3::new(5_000.0, 0.0, 0.0), 1_000_000.0, 0.0, 50.0);
    let t_gate_cloak = mk_ball(98_600_006, 587, DVec3::new(5_000.0, 0.0, 0.0), 1_000_000.0, 0.0, 50.0);
    let t_annulus = mk_ball(98_600_007, 587, DVec3::new(5_000.0, 3_000.0, 0.0), 1_000_000.0, 0.0, 50.0);
    let t_lethal = mk_ball(98_600_008, 587, DVec3::new(5_000.0, 100.0, 0.0), 1_000_000.0, 0.0, 50.0);
    ctx.db.with_mut_snapshot(|snap| {
        for (id, owner) in [(98_600_007i64, vic_char), (98_600_008i64, vic_char)] {
            snap.items.insert(
                id,
                InventoryItemRecord {
                    item_id: id,
                    type_id: 587,
                    owner_id: owner,
                    location_id: sys_id as i64,
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
        if let Some(ch) = snap.characters.get_mut(&vic_char) {
            ch.active_ship_id = 98_600_008;
        }
    });
    for (b, cs_opt) in [
        (&t_behind, None),
        (&t_oob_range, None),
        (&t_oob_perp, None),
        (
            &t_tether,
            Some(CombatState {
                is_tethered: true,
                ..Default::default()
            }),
        ),
        (
            &t_undock,
            Some(CombatState {
                undock_invulnerable_remaining_ticks: 5,
                ..Default::default()
            }),
        ),
        (
            &t_gate_cloak,
            Some(CombatState {
                gate_cloak_remaining_ticks: 5,
                ..Default::default()
            }),
        ),
        (
            &t_annulus,
            Some(CombatState {
                max_structure_hp: 500.0,
                structure_hp: 500.0,
                structure_resonance: [1.0; 4],
                max_capacitor: 500.0,
                capacitor: 300.0,
                ..Default::default()
            }),
        ),
        (
            &t_lethal,
            Some(CombatState {
                max_structure_hp: 50.0,
                structure_hp: 50.0,
                structure_resonance: [1.0; 4],
                ..Default::default()
            }),
        ),
    ] {
        let _ = sys_handle.spawn_ball(b.clone()).await;
        if let Some(cs) = cs_opt {
            let _ = sys_handle.set_combat_state(b.id, cs).await;
        }
    }

    let cone_balls = vec![
        att_ball.clone(),
        t_behind,
        t_oob_range,
        t_oob_perp,
        t_tether,
        t_undock,
        t_gate_cloak,
        t_annulus.clone(),
        t_lethal.clone(),
    ];

    // Green safety aborts collect_and_precheck_lancer_cone_targets
    with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(att_char, SafetyLevel::Green);
    });
    assert_eq!(
        collect_and_precheck_lancer_cone_targets(
            &ctx,
            &sys_handle,
            sys_id,
            att_char,
            att_ship,
            &cone_balls,
            DVec3::ZERO,
            DVec3::X,
            20_000.0,
            5_000.0,
            now_unix,
        )
        .await
        .expect_err("Green blocks lancer precheck")
        .code,
        "CantDoInCurrentSafetyLevel"
    );

    // Override modified_system_securities -> 0.25 (lowsec), Red safety, execute beam
    with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(att_char, SafetyLevel::Red);
        rt.modified_system_securities.insert(sys_id, 0.25);
    });
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        cw.states.remove(&att_char);
    }
    let hit_ids = execute_lancer_beam_attack(
        &ctx,
        &sys_handle,
        sys_id,
        att_char,
        att_ship,
        587,
        2873,
        &cone_balls,
        DVec3::ZERO,
        DVec3::X,
        20_000.0,
        2_000.0,
        5_000.0,
        DamageVector {
            em: 500.0,
            thermal: 0.0,
            kinetic: 0.0,
            explosive: 0.0,
        },
        120.0,
        45,
        now_unix,
    )
    .await
    .expect("execute lancer beam");
    assert_eq!(hit_ids, vec![t_annulus.id, t_lethal.id]);
    let ann_after = sys_handle.get_combat_state(t_annulus.id).await.unwrap().unwrap();
    assert_eq!(ann_after.capacitor, 180.0);
    assert_eq!(ann_after.structure_hp, 500.0, "annulus target takes neut only, no damage");
    assert_eq!(ann_after.lance_disrupted_remaining_s, 45.0);
    {
        let cw = ctx.crimewatch.read().unwrap();
        let st = &cw.states[&att_char];
        assert!(st.is_suspect(now_unix));
        assert!(!st.is_criminal(now_unix));
    }

    // 5. verify_special_site_caller_on_grid missing ship/cloaked/item_positions fallback + seeded DRIFTER/HF/TRIG resolution
    let mut sp_sess = SessionState::new(98_601);
    sp_sess.character_id = Some(att_char);
    sp_sess.solar_system_id = Some(sys_id);
    assert_eq!(
        verify_special_site_caller_on_grid(&ctx, &sp_sess, &sys_handle, att_char, DVec3::ZERO, "Site")
            .await
            .expect_err("no ship")
            .code,
        "NoActiveShip"
    );

    let grid_ship = 98_600_999i64;
    let _ = sys_handle
        .spawn_ball(mk_ball(grid_ship, 587, DVec3::ZERO, 1_000_000.0, 300.0, 50.0))
        .await;
    sp_sess.ship_id = Some(grid_ship);
    let _ = sys_handle
        .set_combat_state(
            grid_ship,
            CombatState {
                is_cloaked: true,
                ..Default::default()
            },
        )
        .await;
    assert_eq!(
        verify_special_site_caller_on_grid(&ctx, &sp_sess, &sys_handle, att_char, DVec3::ZERO, "Site")
            .await
            .expect_err("cloaked")
            .code,
        "ShipCloaked"
    );

    let unspawned_ship = 98_699_001i64;
    sp_sess.ship_id = Some(unspawned_ship);
    ctx.db.with_mut_snapshot(|snap| {
        snap.item_positions.insert(unspawned_ship, [200_000.0, 0.0, 0.0]);
    });
    assert_eq!(
        verify_special_site_caller_on_grid(&ctx, &sp_sess, &sys_handle, att_char, DVec3::ZERO, "Site")
            .await
            .expect_err("item_positions too far")
            .code,
        "TooFarFromSite"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.item_positions.insert(unspawned_ship, [1_000.0, 0.0, 0.0]);
        snap.pve_special_sites.homefront_sites.insert(
            sys_id,
            vec![HomefrontSiteRecord {
                instance_id: 42_001,
                dungeon_id: 1,
                dungeon_name: "HF".into(),
                solar_system_id: sys_id,
                optimal_fleet_size: 5,
                base_pilot_isk: 1_000_000,
                completed: false,
                reward_paid_isk: 0,
                boss_npc_ball_id: 1,
                boss_type_id: 0,
            }],
        );
        snap.pve_special_sites.triglavian_sites.insert(
            sys_id,
            vec![TriglavianSiteRecord {
                instance_id: 42_002,
                dungeon_id: 2,
                dungeon_name: "TRIG".into(),
                solar_system_id: sys_id,
                faction_owner_id: 500_026,
                base_isk: 1_000_000,
                survey_db_drop: 0,
                completed: false,
                boss_npc_ball_id: 2,
                boss_type_id: 0,
                escort_ball_ids: Vec::new(),
                escort_type_ids: Vec::new(),
                beacon_ball_id: 3,
            }],
        );
        snap.pve_special_sites.drifter_hives.insert(
            31_000_001,
            DrifterHiveRecord {
                site_id: "DRIFTER-31000001-0".into(),
                dungeon_id: 3,
                dungeon_name: "Hive".into(),
                solar_system_id: 31_000_001,
                element_type_id: 1,
                element_name: "Vidette".into(),
                current_room: 0,
                total_rooms: 3,
                warden_npc_ball_id: 4,
                cleared: false,
                rooms: Vec::new(),
            },
        );
    });
    verify_special_site_caller_on_grid(&ctx, &sp_sess, &sys_handle, att_char, DVec3::ZERO, "Site")
        .await
        .expect("item_positions within 100km passes");
    assert_eq!(
        resolve_special_site_string_instance_id(&ctx, &format!("HF-{sys_id}-0")),
        Some(42_001)
    );
    assert_eq!(
        resolve_special_site_string_instance_id(&ctx, &format!("TRIG-{sys_id}-0")),
        Some(42_002)
    );
    assert_eq!(
        resolve_special_site_string_instance_id(&ctx, "DRIFTER-31000001-0"),
        Some(31_000_001)
    );
    assert!(is_known_special_site_instance_id(&ctx, 42_001));
    assert!(is_known_special_site_instance_id(&ctx, 42_002));
    assert!(is_known_special_site_instance_id(&ctx, 31_000_001));
}

#[tokio::test]
async fn mutant_kill_enforce_legacy_pvp_return_combinations() {
    let ctx = fresh_ctx();
    let att_char = 98_700_001_i64;
    let vic_char = 98_700_002_i64;
    let att_ship = 98_700_101_i64;
    let vic_ship = 98_700_102_i64;
    let high_sys = 30_000_142_u32;
    let (low_sys, low_sec) = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.security_status >= 0.05 && s.security_status < 0.45)
        .map(|s| (s.solar_system_id, s.security_status))
        .expect("SDE must contain a lowsec solar system");
    let now_unix = 1_750_000_500_u64;

    ctx.db.with_mut_snapshot(|snap| {
        let base_char = snap.characters.get(&90_000_001).unwrap().clone();
        snap.characters.insert(
            att_char,
            CharacterRecord {
                character_id: att_char,
                user_id: 11,
                name: "LegalAttacker".into(),
                corporation_id: 1_000_009,
                solar_system_id: high_sys,
                station_id: None,
                structure_id: None,
                home_station_id: 60_003_760,
                active_ship_id: att_ship,
                security_status: 0.0,
                balance: IskCents::from_isk(100_000),
                ..base_char.clone()
            },
        );
        snap.characters.insert(
            vic_char,
            CharacterRecord {
                character_id: vic_char,
                user_id: 12,
                name: "OutlawVictim".into(),
                corporation_id: 1_000_010,
                solar_system_id: high_sys,
                station_id: None,
                structure_id: None,
                home_station_id: 60_003_760,
                active_ship_id: vic_ship,
                security_status: -5.0,
                balance: IskCents::from_isk(100_000),
                ..base_char
            },
        );
        for (iid, owner, loc) in [(att_ship, att_char, high_sys as i64), (vic_ship, vic_char, high_sys as i64)] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: 587,
                    owner_id: owner,
                    location_id: loc,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
    });

    let high_handle = ctx.systems.get_or_spawn(high_sys);
    let low_handle = ctx.systems.get_or_spawn(low_sys);
    let mk_ship_ball = |id: i64, x: f64| Ball {
        id,
        type_id: 587,
        position: DVec3::new(x, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let att_ball = mk_ship_ball(att_ship, 0.0);
    let vic_ball = mk_ship_ball(vic_ship, 5_000.0);
    let _ = high_handle.spawn_ball(att_ball.clone()).await;
    let _ = high_handle.spawn_ball(vic_ball.clone()).await;

    let mut sess = SessionState::new(98_701);
    sess.user_id = Some(11);
    sess.character_id = Some(att_char);
    sess.solar_system_id = Some(high_sys);
    sess.ship_id = Some(att_ship);

    // 1. Lawful outlaw target (-5.0) in highsec under Green safety -> Ok((true, None))
    with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(att_char, SafetyLevel::Green);
    });
    let (legal_1, concord_1) = enforce_legacy_pvp_safety_and_concord(
        &ctx,
        &mut sess,
        &high_handle,
        att_char,
        att_ship,
        &att_ball,
        high_sys,
        0.95,
        vic_ship,
        &vic_ball,
        vic_char,
        now_unix,
    )
    .await
    .unwrap();
    assert!(legal_1);
    assert!(concord_1.is_none());

    // 2. Neutral target in lowsec under Yellow safety -> Ok((false, None))
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&vic_char).unwrap().security_status = 0.0;
    });
    with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(att_char, SafetyLevel::Yellow);
    });
    let (legal_2, concord_2) = enforce_legacy_pvp_safety_and_concord(
        &ctx,
        &mut sess,
        &low_handle,
        att_char,
        att_ship,
        &att_ball,
        low_sys,
        low_sec,
        vic_ship,
        &vic_ball,
        vic_char,
        now_unix,
    )
    .await
    .unwrap();
    assert!(!legal_2);
    assert!(concord_2.is_none());

    // 3. Neutral target in highsec under Red safety -> Ok((false, Some(keyval)))
    with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(att_char, SafetyLevel::Red);
    });
    let (legal_3, concord_3) = enforce_legacy_pvp_safety_and_concord(
        &ctx,
        &mut sess,
        &high_handle,
        att_char,
        att_ship,
        &att_ball,
        high_sys,
        0.95,
        vic_ship,
        &vic_ball,
        vic_char,
        now_unix,
    )
    .await
    .unwrap();
    assert!(!legal_3);
    let kv = concord_3.expect("expected CONCORD KeyVal");
    assert_eq!(kv.dict_get("criminalCharacterID"), Some(&PyRep::Integer(att_char)));
    assert_eq!(kv.dict_get("victimCharacterID"), Some(&PyRep::Integer(vic_char)));
    assert_eq!(kv.dict_get("criminalTimerSeconds"), Some(&PyRep::Integer(900)));
    assert!(kv.dict_get("concordKillmailID").and_then(|v| v.as_i64()).unwrap_or(0) > 0);
}
