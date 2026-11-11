//! Extra unit and mutation-killing tests for `services::inventory_fitting::fitting_cyno_portal`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::inventory_fitting::fitting_cyno_portal::*;
use crate::services::mobile_deployables::{write_deployable_state_snap, MobileDeployableRecord};
use eve_db::{
    FleetRecord, InventoryFlag, InventoryItemRecord, TrainedSkillRecord, UpwellStructureRecord,
    VirtualServerClock, WorldRepository,
};
use eve_destiny::{Ball, CombatState, MovementMode, SlimItemMeta};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{InvType, SdeRegistry, SolarSystem};
use glam::DVec3;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn custom_inv_type(
    sde: &SdeRegistry,
    type_id: u32,
    group_id: u32,
    category_id: u32,
    radius: f64,
    name: &str,
    attrs: &[(u16, f64)],
) -> InvType {
    let mut t = sde.types.get(&587).expect("seed type").clone();
    t.type_id = type_id;
    t.group_id = group_id;
    t.category_id = category_id;
    t.radius = radius;
    t.name = name.into();
    t.default_attributes.clear();
    for &(k, v) in attrs {
        t.default_attributes.insert(k, v);
    }
    t
}

fn custom_system(
    sde: &SdeRegistry,
    solar_system_id: u32,
    region_id: u32,
    security_status: f64,
    galactic_pos_ly: [f64; 3],
) -> SolarSystem {
    let mut s = sde.solar_systems.get(&30_002_505).expect("seed system").clone();
    s.solar_system_id = solar_system_id;
    s.region_id = region_id;
    s.security_status = security_status;
    s.galactic_pos_ly = galactic_pos_ly;
    s
}

fn test_ctx_with_custom<F: FnOnce(&mut SdeRegistry)>(mutate_sde: F) -> ServerContext {
    let mut sde = (*shared_sde()).clone();
    mutate_sde(&mut sde);
    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(Arc::new(sde), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_820_000_000)));
    ctx
}

fn ensure_char(
    ctx: &ServerContext,
    char_id: i64,
    corp_id: i64,
    sys_id: u32,
    ship_id: i64,
    fleet_id: Option<i64>,
) {
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("demo char");
        ch.character_id = char_id;
        ch.user_id = char_id as u64;
        ch.name = format!("Pilot {char_id}");
        ch.corporation_id = corp_id;
        ch.station_id = None;
        ch.structure_id = None;
        ch.solar_system_id = sys_id;
        ch.active_ship_id = ship_id;
        ch.security_status = 5.0;
        ch.fleet_id = fleet_id;
        ch.clone_grade = 1;
        snap.characters.insert(char_id, ch);
    });
    let _ = ctx.db.acquire_online_lock(
        char_id as u64,
        char_id as u64,
        Some(char_id),
        true,
        ctx.now_unix_secs(),
    );
}

fn insert_item(
    ctx: &ServerContext,
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            item_id,
            InventoryItemRecord {
                item_id,
                type_id,
                owner_id,
                location_id,
                flag,
                quantity,
                singleton: quantity == 1,
                ..Default::default()
            },
        );
    });
}

async fn spawn_ship(
    ctx: &ServerContext,
    sys_id: u32,
    ship_id: i64,
    type_id: u32,
    owner_id: i64,
    pos: DVec3,
    mode: MovementMode,
    cap: f64,
) {
    let sys = ctx.systems.get_or_spawn(sys_id);
    let ball = Ball {
        id: ship_id,
        type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 2_000.0,
        inertia: 1.0,
        mass: 5_000_000.0,
        radius: 500.0,
        speed_fraction: 0.0,
        mode,
    };
    let mut cs = CombatState::default();
    cs.capacitor = cap;
    cs.max_capacitor = cap;
    let meta = SlimItemMeta {
        group_id: 27,
        category_id: 6,
        owner_id,
        char_id: Some(owner_id),
        corp_id: 98_000_001,
        alliance_id: 0,
        security_status: 5.0,
        is_cloaked: false,
        ..Default::default()
    };
    sys.spawn_ball_with_meta(ball, cs, meta).await.unwrap();
}

#[test]
fn covert_beacon_types_and_jammer_edge_cases() {
    for tid in [28646, 28650, 59630, 59631] {
        assert!(is_covert_beacon_type(tid));
    }
    for tid in [21096, 57425, 57319, 58906] {
        assert!(!is_covert_beacon_type(tid));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn caller_in_space_guards_deployable_beacon_resolution_and_structure_ejection() {
    let origin_sys = 30_002_505_u32;
    let dest_sys = 30_002_506_u32;
    let wh_sys = 31_000_005_u32;
    let ctx = test_ctx_with_custom(|sde| {
        sde.solar_systems.insert(
            origin_sys,
            custom_system(sde, origin_sys, 10_000_001, -0.5, [0.0, 0.0, 0.0]),
        );
        sde.solar_systems.insert(
            dest_sys,
            custom_system(sde, dest_sys, 10_000_001, -0.5, [1.0, 0.0, 0.0]),
        );
        sde.type_lists.insert(
            99_318,
            eve_sde::TypeListInfo {
                type_list_id: 99_318,
                name: "Standard Cyno Beacons".into(),
                included_type_ids: vec![21096, 57425],
                ..Default::default()
            },
        );
        sde.types.insert(
            999_403,
            custom_inv_type(sde, 999_403, 1299, 7, 10.0, "Jump Drive Economizer", &[(1296, -10.0)]),
        );
        sde.types.insert(
            999_404,
            custom_inv_type(
                sde,
                999_404,
                547,
                6,
                800.0,
                "Test Jump Carrier",
                &[(861, 1.0), (866, 17888.0), (867, 6.0), (868, 1_000.0), (898, 0.10), (3317, 99_318.0)],
            ),
        );
    });

    let char_id = 95_100_010;
    let corp_id = 98_100_010;
    let fleet_id = 700_110;
    let ship_id = 1_000_600_101;
    ensure_char(&ctx, char_id, corp_id, origin_sys, ship_id, Some(fleet_id));
    insert_item(&ctx, ship_id, 999_404, char_id, origin_sys as i64, InventoryFlag::None, 1);

    let mut sess = SessionState {
        user_id: Some(char_id as u64),
        character_id: Some(char_id),
        corporation_id: Some(corp_id),
        solar_system_id: Some(origin_sys),
        ship_id: Some(ship_id),
        ..Default::default()
    };
    let jump_args = [
        PyRep::Integer(0),
        PyRep::Integer(dest_sys as i64),
        PyRep::Integer(1_000_600_200),
    ];

    // 1. NoActiveShip, NoSolarSystem, Wormhole, ShipDocked, ShipNotInSpace
    sess.ship_id = None;
    assert_eq!(
        handle_cyno_portal(&ctx, &mut sess, "beyonce", "CmdJumpToFleetDeployableBeacon", &jump_args)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NoActiveShip"
    );
    sess.ship_id = Some(ship_id);
    sess.solar_system_id = None;
    assert_eq!(
        handle_cyno_portal(&ctx, &mut sess, "beyonce", "CmdJumpToFleetDeployableBeacon", &jump_args)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NoSolarSystem"
    );
    sess.solar_system_id = Some(wh_sys);
    assert_eq!(
        handle_cyno_portal(&ctx, &mut sess, "beyonce", "CmdJumpToFleetDeployableBeacon", &jump_args)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "JumpDriveForbiddenInWormholeSpace"
    );
    sess.solar_system_id = Some(origin_sys);
    sess.station_id = Some(60_003_760);
    assert_eq!(
        handle_cyno_portal(&ctx, &mut sess, "beyonce", "CmdJumpToFleetDeployableBeacon", &jump_args)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "ShipDocked"
    );
    sess.station_id = None;
    assert_eq!(
        handle_cyno_portal(&ctx, &mut sess, "beyonce", "CmdJumpToFleetDeployableBeacon", &jump_args)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "ShipNotInSpace"
    );

    // 2. Spawn in warp -> ShipInWarp
    spawn_ship(
        &ctx,
        origin_sys,
        ship_id,
        999_404,
        char_id,
        DVec3::ZERO,
        MovementMode::new_warp(DVec3::ZERO, DVec3::new(100_000.0, 0.0, 0.0), 3.0),
        1_000.0,
    )
    .await;
    assert_eq!(
        handle_cyno_portal(&ctx, &mut sess, "beyonce", "CmdJumpToFleetDeployableBeacon", &jump_args)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "ShipInWarp"
    );

    // Stop ship, test WarpScrambled and JumpReactivationCooldown
    let sys = ctx.systems.get_or_spawn(origin_sys);
    sys.try_stop_ball(ship_id);
    sys.try_update_combat_state(ship_id, |cs| {
        cs.warp_scramble_strength = 2;
        cs.warp_core_strength = 0;
    });
    assert_eq!(
        handle_cyno_portal(&ctx, &mut sess, "beyonce", "CmdJumpToFleetDeployableBeacon", &jump_args)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "WarpScrambled"
    );
    sys.try_update_combat_state(ship_id, |cs| cs.warp_scramble_strength = 0);

    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&char_id).unwrap().jump_cooldown_until_unix = now + 60;
    });
    assert_eq!(
        handle_cyno_portal(&ctx, &mut sess, "beyonce", "CmdJumpToFleetDeployableBeacon", &jump_args)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "JumpReactivationCooldown"
    );
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&char_id).unwrap().jump_cooldown_until_unix = 0;
    });

    // 3. Deployable beacon resolution (`is_deployable = true`): mobile cyno beacon (57319 -> 57425)
    // plus JFC IV + 1 online Economizer + 1 offline Economizer -> 1000 * 0.60 * 0.90 = 540 fuel
    let dep_beacon_id = 1_000_600_200_i64;
    insert_item(&ctx, 1_000_600_102, 999_403, char_id, ship_id, InventoryFlag::LoSlot0, 1);
    insert_item(&ctx, 1_000_600_103, 999_403, char_id, ship_id, InventoryFlag::LoSlot1, 1);
    insert_item(&ctx, 1_000_600_104, 17888, char_id, ship_id, InventoryFlag::SpecializedFuelBay, 1_000);
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(1_000_600_103, false);
        let sk = snap.skills.entry(char_id).or_default();
        sk.insert(21610, TrainedSkillRecord { type_id: 21610, level: 4, skill_points: 100_000.0 });
        sk.insert(29029, TrainedSkillRecord { type_id: 29029, level: 5, skill_points: 256_000.0 });
        snap.fleets.insert(
            fleet_id,
            FleetRecord {
                fleet_id,
                boss_id: char_id,
                members: vec![char_id],
                ..Default::default()
            },
        );
        snap.items.insert(
            dep_beacon_id,
            InventoryItemRecord {
                item_id: dep_beacon_id,
                type_id: 57319,
                owner_id: char_id,
                location_id: dest_sys as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        write_deployable_state_snap(snap, |st| {
            st.deployables.insert(
                dep_beacon_id,
                MobileDeployableRecord {
                    item_id: dep_beacon_id,
                    type_id: 57319,
                    group_id: 4093,
                    owner_id: char_id,
                    solar_system_id: dest_sys,
                    anchor_start_unix: now - 120,
                    anchor_duration_secs: 60,
                    anchor_completed: true,
                    is_unanchored: false,
                    expires_at_unix: now + 3_600,
                    last_interaction_unix: now,
                    reinforced_until_unix: None,
                    last_ping_unix: None,
                    position: [0.0, 0.0, 0.0],
                    deployed_fleet_id: Some(fleet_id),
                },
            );
        });
    });
    let jump_res = handle_cyno_portal(&ctx, &mut sess, "beyonce", "CmdJumpToFleetDeployableBeacon", &jump_args)
        .await
        .unwrap()
        .expect("jump to deployable mobile cyno beacon");
    assert_eq!(jump_res.dict_get("fuelConsumed").and_then(|v| v.as_i64()), Some(540));

    // Clear session change timer and cooldown before testing relocate_ship structure ejection
    ctx.session_change_timers.lock().unwrap().remove(&char_id);
    ctx.db.with_mut_snapshot(|s| {
        let ch = s.characters.get_mut(&char_id).unwrap();
        ch.solar_system_id = origin_sys;
        ch.jump_cooldown_until_unix = 0;
        ch.jump_fatigue_until_unix = 0;
        s.items.get_mut(&ship_id).unwrap().location_id = origin_sys as i64;
    });

    // 4. relocate_ship ejects ship with outward velocity when arriving overlapping an Upwell structure
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            1_046_000_001,
            UpwellStructureRecord {
                structure_id: 1_046_000_001,
                type_id: 35834,
                owner_corp_id: corp_id,
                solar_system_id: dest_sys,
                name: "Obstacle Keepstar".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );
    });
    let (cd_min, fat_min) = relocate_ship(
        &ctx,
        char_id,
        ship_id,
        origin_sys,
        dest_sys,
        DVec3::new(100.0, 0.0, 0.0), // inside Keepstar radius
        2.0,
        0.0,
    )
    .await
    .expect("relocate_ship with structure overlap");
    assert!(cd_min > 0.0 && fat_min > cd_min);
    let dest_handle = ctx.systems.get_or_spawn(dest_sys);
    let (_, dest_ball) = dest_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!(dest_ball.position.x > 1_000.0);
    assert!((dest_ball.velocity.length() - 250.0).abs() < 1e-6);

    // Immediate second relocate_ship while session change timer is active -> SessionChangeTimerActive
    assert_eq!(
        relocate_ship(&ctx, char_id, ship_id, dest_sys, origin_sys, DVec3::ZERO, 2.0, 0.0)
            .await
            .unwrap_err()
            .code,
        "SessionChangeTimerActive"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn light_cyno_strategic_cruiser_skill_and_jammer_guards() {
    let lowsec_sys = 30_002_505_u32;
    let pochven_sys = 30_002_507_u32;
    let ctx = test_ctx_with_custom(|sde| {
        sde.solar_systems.insert(
            lowsec_sys,
            custom_system(sde, lowsec_sys, 10_000_001, 0.2, [0.0, 0.0, 0.0]),
        );
        sde.solar_systems.insert(
            pochven_sys,
            custom_system(sde, pochven_sys, 10_000_070, -1.0, [1.0, 0.0, 0.0]),
        );
        // Strategic Cruiser (group 963)
        sde.types.insert(
            29984,
            custom_inv_type(sde, 29984, 963, 6, 150.0, "Tengu", &[(3320, 0.0)]),
        );
        // Covert Cyno (28646) requiring skill 21603 level 5
        sde.types.insert(
            28646,
            custom_inv_type(
                sde,
                28646,
                658,
                7,
                20.0,
                "Covert Cynosural Field Generator I",
                &[
                    (1252, 1.0),
                    (713, 16273.0),
                    (714, 50.0),
                    (73, 60_000.0),
                    (182, 21603.0),
                    (277, 5.0),
                ],
            ),
        );
    });

    let char_id = 95_100_030;
    let fleet_id = 700_130;
    let ship_id = 1_000_600_301;
    let mod_id = 1_000_600_302;
    ensure_char(&ctx, char_id, 98_000_001, lowsec_sys, ship_id, Some(fleet_id));
    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.insert(
            fleet_id,
            FleetRecord {
                fleet_id,
                boss_id: char_id,
                members: vec![char_id],
                ..Default::default()
            },
        );
    });
    insert_item(&ctx, ship_id, 29984, char_id, lowsec_sys as i64, InventoryFlag::None, 1);
    insert_item(&ctx, mod_id, 28646, char_id, ship_id, InventoryFlag::HiSlot0, 1);
    spawn_ship(
        &ctx,
        lowsec_sys,
        ship_id,
        29984,
        char_id,
        DVec3::ZERO,
        MovementMode::Stop,
        5_000.0,
    )
    .await;

    let sys = ctx.systems.get_or_spawn(lowsec_sys);
    let non_cyno = ctx.sde.types.get(&587).unwrap().clone();
    assert!(
        handle_cyno_generator_activation(&ctx, &sys, char_id, ship_id, lowsec_sys, mod_id, &non_cyno)
            .await
            .is_none()
    );

    let covert_type = ctx.sde.types.get(&28646).unwrap().clone();
    // Pochven guard -> CynoJammedInPochven
    assert_eq!(
        handle_cyno_generator_activation(
            &ctx,
            &sys,
            char_id,
            ship_id,
            pochven_sys,
            mod_id,
            &covert_type,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "CynoJammedInPochven"
    );

    // Strategic Cruiser without Covert Reconfiguration subsystem (attr 3320 <= 0) -> CannotLightCynoInSystem
    assert_eq!(
        handle_cyno_generator_activation(
            &ctx,
            &sys,
            char_id,
            ship_id,
            lowsec_sys,
            mod_id,
            &covert_type,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "CannotLightCynoInSystem"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cyno_destroy_counts_activation_guards_mobile_beacons_and_jump_restrictions() {
    let lowsec_sys = 30_002_505_u32;
    let highsec_sys = 30_000_142_u32;
    let wh_sys = 31_000_001_u32;
    let abyss_sys = 32_000_001_u32;
    let zarzakh_sys = 30_100_000_u32;
    let pochven_sys = 30_002_702_u32;
    let far_lowsec_sys = 30_002_999_u32;

    let ctx = test_ctx_with_custom(|sde| {
        sde.solar_systems.insert(lowsec_sys, custom_system(sde, lowsec_sys, 10_000_030, 0.2, [0.0, 0.0, 0.0]));
        sde.solar_systems.insert(highsec_sys, custom_system(sde, highsec_sys, 10_000_002, 0.9, [1.0, 0.0, 0.0]));
        sde.solar_systems.insert(pochven_sys, custom_system(sde, pochven_sys, 10_000_070, -0.5, [1.0, 0.0, 0.0]));
        sde.solar_systems.insert(far_lowsec_sys, custom_system(sde, far_lowsec_sys, 10_000_030, 0.2, [50.0, 0.0, 0.0]));
        // Industrial cyno module (type 52_694, attr 2826=1.0, attr 713=16274, attr 714=800, attr 73=600_000, attr 182=21603, attr 277=1)
        sde.types.insert(
            52_694,
            custom_inv_type(
                sde,
                52_694,
                658,
                7,
                20.0,
                "Industrial Cynosural Field Generator I",
                &[(73, 600_000.0), (713, 16_274.0), (714, 800.0), (2826, 1.0), (182, 21_603.0), (277, 1.0)],
            ),
        );
        // Jump-capable carrier (group 547)
        sde.type_lists.insert(
            99_317,
            eve_sde::TypeListInfo {
                type_list_id: 99_317,
                name: "Standard Cyno Beacons".into(),
                included_type_ids: vec![21_094, 21_096, 57_425],
                ..Default::default()
            },
        );
        sde.types.insert(
            23_911,
            custom_inv_type(
                sde,
                23_911,
                547,
                6,
                1500.0,
                "Thanatos",
                &[(861, 1.0), (866, 16_274.0), (867, 6.5), (868, 1000.0), (898, 1.0), (3317, 99_317.0)],
            ),
        );
    });

    let cyno_char = 94_001_001_i64;
    let cyno_ship = 94_101_001_i64;
    let jumper_char = 94_001_002_i64;
    let jumper_ship = 94_101_002_i64;
    let corp_id = 98_401_001_i64;
    let fleet_id = 77_001_i64;

    ensure_char(&ctx, cyno_char, corp_id, lowsec_sys, cyno_ship, Some(fleet_id));
    ensure_char(&ctx, jumper_char, corp_id, lowsec_sys, jumper_ship, Some(fleet_id));
    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.insert(
            fleet_id,
            FleetRecord {
                fleet_id,
                boss_id: cyno_char,
                members: vec![cyno_char, jumper_char],
                ..Default::default()
            },
        );
        snap.items.insert(
            cyno_ship,
            InventoryItemRecord {
                item_id: cyno_ship,
                type_id: 587,
                owner_id: cyno_char,
                location_id: lowsec_sys as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            jumper_ship,
            InventoryItemRecord {
                item_id: jumper_ship,
                type_id: 23_911,
                owner_id: jumper_char,
                location_id: lowsec_sys as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    let sys_low = ctx.systems.get_or_spawn(lowsec_sys);
    // Moving >= 500 m/s -> ShipMovingTooFast
    sys_low
        .spawn_ball_with_meta(
            Ball {
                id: cyno_ship,
                type_id: 587,
                position: DVec3::ZERO,
                velocity: DVec3::new(600.0, 0.0, 0.0),
                max_velocity: 700.0,
                inertia: 1.0,
                mass: 1e6,
                radius: 40.0,
                speed_fraction: 1.0,
                mode: MovementMode::GotoDirection { direction: DVec3::X },
            },
            CombatState {
                capacitor: 50_000.0,
                max_capacitor: 50_000.0,
                ..Default::default()
            },
            SlimItemMeta::default(),
        )
        .await
        .unwrap();
    let indy_mod_type = ctx.sde.types.get(&52_694).unwrap().clone();
    assert_eq!(
        handle_cyno_generator_activation(&ctx, &sys_low, cyno_char, cyno_ship, lowsec_sys, 94_200_001, &indy_mod_type)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "ShipMovingTooFast"
    );

    // Stop ship, test SkillMissing (missing Cynosural Field Theory 21603)
    let _ = sys_low.remove_ball(cyno_ship).await;
    sys_low
        .spawn_ball_with_meta(
            Ball {
                id: cyno_ship,
                type_id: 587,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1e6,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState {
                capacitor: 50_000.0,
                max_capacitor: 50_000.0,
                ..Default::default()
            },
            SlimItemMeta::default(),
        )
        .await
        .unwrap();
    assert_eq!(
        handle_cyno_generator_activation(&ctx, &sys_low, cyno_char, cyno_ship, lowsec_sys, 94_200_001, &indy_mod_type)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "SkillMissing"
    );

    // Grant CFT skill, test missing fuel -> MissingFuel/InsufficientFuel error, then grant fuel & activate
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(cyno_char).or_default().insert(
            21_603,
            TrainedSkillRecord { type_id: 21_603, level: 5, skill_points: 256_000.0 },
        );
    });
    assert!(
        handle_cyno_generator_activation(&ctx, &sys_low, cyno_char, cyno_ship, lowsec_sys, 94_200_001, &indy_mod_type)
            .await
            .unwrap()
            .is_err()
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            94_200_002,
            InventoryItemRecord {
                item_id: 94_200_002,
                type_id: 16_274,
                owner_id: cyno_char,
                location_id: cyno_ship,
                flag: InventoryFlag::Cargo,
                quantity: 5000,
                singleton: false,
                ..Default::default()
            },
        );
    });
    let act_ok = handle_cyno_generator_activation(&ctx, &sys_low, cyno_char, cyno_ship, lowsec_sys, 94_200_001, &indy_mod_type)
        .await
        .unwrap()
        .expect("industrial cyno activation");
    assert_eq!(act_ok.dict_get("consumptionQuantity").and_then(|v| v.as_i64()), Some(400)); // min_floor 400 for industrial cyno

    // Activating second cyno on same ship -> CannotLightCynoInSystem
    assert_eq!(
        handle_cyno_generator_activation(&ctx, &sys_low, cyno_char, cyno_ship, lowsec_sys, 94_200_003, &indy_mod_type)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "CannotLightCynoInSystem"
    );

    // active_cyno_beacon_counts & ship_cyno_activity_expiry_unix & on_cyno_ship_destroyed
    assert_eq!(active_cyno_beacon_counts(&ctx).get(&i64::from(lowsec_sys)).copied(), Some(1));
    assert!(ship_cyno_activity_expiry_unix(&ctx, cyno_ship) > ctx.now_unix());
    on_cyno_ship_destroyed(&ctx, cyno_ship);
    assert!(active_cyno_beacon_counts(&ctx).get(&i64::from(lowsec_sys)).is_none());

    // Mobile Cyno Beacon (57319): unanchored/expired/not-spooled/inhibited, then valid spooled
    let dep_beacon_id = 94_300_001_i64;
    let now_u = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            dep_beacon_id,
            InventoryItemRecord {
                item_id: dep_beacon_id,
                type_id: 57_319,
                owner_id: cyno_char,
                location_id: lowsec_sys as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.item_positions.insert(dep_beacon_id, [10_000.0, 0.0, 0.0]);
        write_deployable_state_snap(snap, |st| {
            st.deployables.insert(
                dep_beacon_id,
                MobileDeployableRecord {
                    item_id: dep_beacon_id,
                    type_id: 57_319,
                    group_id: 4093,
                    owner_id: cyno_char,
                    solar_system_id: lowsec_sys,
                    anchor_start_unix: now_u - 10,
                    anchor_duration_secs: 60,
                    anchor_completed: false,
                    is_unanchored: false,
                    expires_at_unix: now_u + 3600,
                    last_interaction_unix: now_u,
                    reinforced_until_unix: None,
                    last_ping_unix: None,
                    position: [10_000.0, 0.0, 0.0],
                    deployed_fleet_id: Some(fleet_id),
                },
            );
        });
    });
    // Not spooled yet -> NoActiveBeacon
    assert_eq!(
        resolve_fleet_beacon_or_destroyed(&ctx, jumper_char, cyno_char, dep_beacon_id, lowsec_sys, true)
            .await
            .unwrap_err()
            .code,
        "NoActiveBeacon"
    );
    ctx.db.with_mut_snapshot(|snap| {
        write_deployable_state_snap(snap, |st| {
            st.deployables.get_mut(&dep_beacon_id).unwrap().anchor_completed = true;
        });
    });
    let (resolved_dep, _) = resolve_fleet_beacon_or_destroyed(&ctx, jumper_char, cyno_char, dep_beacon_id, lowsec_sys, true)
        .await
        .expect("spooled mobile cyno beacon");
    assert_eq!(resolved_dep.beacon_id, dep_beacon_id);

    // jump_to_fleet_beacon restrictions: WH, Abyssal, Zarzakh, Pochven, HighSec, OutOfRange
    sys_low
        .spawn_ball_with_meta(
            Ball {
                id: jumper_ship,
                type_id: 23_911,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 100.0,
                inertia: 1.0,
                mass: 1e9,
                radius: 1500.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState {
                capacitor: 50_000.0,
                max_capacitor: 50_000.0,
                ..Default::default()
            },
            SlimItemMeta::default(),
        )
        .await
        .unwrap();
    let mut jumper_sess = SessionState {
        user_id: Some(jumper_char as u64),
        character_id: Some(jumper_char),
        corporation_id: Some(corp_id),
        solar_system_id: Some(lowsec_sys),
        ship_id: Some(jumper_ship),
        fleet_id: Some(fleet_id),
        ..Default::default()
    };
    for (bad_sys, expected_code) in [
        (wh_sys, "JumpDriveForbiddenInWormholeSpace"),
        (abyss_sys, "JumpDriveForbiddenInAbyssalSpace"),
        (zarzakh_sys, "JumpDriveForbiddenInZarzakh"),
        (pochven_sys, "CannotJumpToPochven"),
        (far_lowsec_sys, "JumpDistanceOutOfRange"),
    ] {
        assert_eq!(
            jump_to_fleet_beacon(&ctx, &mut jumper_sess, &[PyRep::Integer(cyno_char), PyRep::Integer(bad_sys as i64), PyRep::Integer(0)], false)
                .await
                .err()
                .unwrap()
                .code,
            expected_code
        );
    }
    // Beacon in highsec -> CannotJumpToHighSec
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&dep_beacon_id).unwrap().location_id = highsec_sys as i64;
        write_deployable_state_snap(snap, |st| {
            st.deployables.get_mut(&dep_beacon_id).unwrap().solar_system_id = highsec_sys;
        });
    });
    assert_eq!(
        jump_to_fleet_beacon(&ctx, &mut jumper_sess, &[PyRep::Integer(cyno_char), PyRep::Integer(highsec_sys as i64), PyRep::Integer(dep_beacon_id)], true)
            .await
            .err()
            .unwrap()
            .code,
        "CannotJumpToHighSec"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cyno_expiry_cleanup_inhibitor_deployable_types_and_jump_fuel_timer_paths() {
    let lowsec_sys = 30_002_505_u32;
    let jump_from_sys = 30_002_506_u32;
    let far_sys = 30_002_999_u32;
    let wh_sys = 31_000_005_u32;
    let abyss_sys = 32_000_001_u32;
    let zarzakh_sys = 30_100_000_u32;
    let pochven_sys = 30_002_700_u32;
    let far_lowsec_sys = far_sys;
    let ctx = test_ctx_with_custom(|sde| {
        sde.solar_systems.insert(lowsec_sys, custom_system(sde, lowsec_sys, 10_000_030, 0.2, [0.0, 0.0, 0.0]));
        sde.solar_systems.insert(jump_from_sys, custom_system(sde, jump_from_sys, 10_000_030, 0.2, [1.0, 0.0, 0.0]));
        sde.solar_systems.insert(far_sys, custom_system(sde, far_sys, 10_000_030, 0.2, [50.0, 0.0, 0.0]));
        sde.solar_systems.insert(pochven_sys, custom_system(sde, pochven_sys, 10_000_070, -0.5, [1.0, 0.0, 0.0]));
        sde.types.insert(
            21_096,
            custom_inv_type(
                sde,
                21_096,
                658,
                7,
                20.0,
                "Cynosural Field Generator I",
                &[(73, 600_000.0), (713, 16_273.0), (714, 500.0), (2455, 5000.0), (182, 21_603.0), (277, 1.0)],
            ),
        );
        sde.types.insert(
            21_094,
            custom_inv_type(sde, 21_094, 2020, 2, 100.0, "Cynosural Field", &[]),
        );
        sde.type_lists.insert(
            99_317,
            eve_sde::TypeListInfo {
                type_list_id: 99_317,
                name: "Standard Cyno Beacons".into(),
                included_type_ids: vec![21_094, 21_096, 57_425],
                ..Default::default()
            },
        );
        sde.types.insert(
            23_911,
            custom_inv_type(
                sde,
                23_911,
                547,
                6,
                1500.0,
                "Thanatos",
                &[(861, 1.0), (866, 16_274.0), (867, 6.5), (868, 1000.0), (898, 1.0), (3317, 99_317.0)],
            ),
        );
    });
    let cyno_char = 94_050_001_i64;
    let cyno_ship = 94_150_001_i64;
    let jumper_char = 94_050_002_i64;
    let jumper_ship = 94_150_002_i64;
    let corp_id = 98_450_001_i64;
    let fleet_id = 77_050_i64;
    ensure_char(&ctx, cyno_char, corp_id, lowsec_sys, cyno_ship, Some(fleet_id));
    ensure_char(&ctx, jumper_char, corp_id, jump_from_sys, jumper_ship, Some(fleet_id));
    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.insert(fleet_id, FleetRecord { fleet_id, boss_id: cyno_char, members: vec![cyno_char, jumper_char], ..Default::default() });
        snap.skills.entry(cyno_char).or_default().insert(21_603, TrainedSkillRecord { type_id: 21_603, level: 5, skill_points: 256_000.0 });
        snap.items.insert(cyno_ship, InventoryItemRecord { item_id: cyno_ship, type_id: 587, owner_id: cyno_char, location_id: lowsec_sys as i64, flag: InventoryFlag::None, quantity: 1, singleton: true, ..Default::default() });
        snap.items.insert(jumper_ship, InventoryItemRecord { item_id: jumper_ship, type_id: 23_911, owner_id: jumper_char, location_id: jump_from_sys as i64, flag: InventoryFlag::None, quantity: 1, singleton: true, ..Default::default() });
        snap.items.insert(94_250_001, InventoryItemRecord { item_id: 94_250_001, type_id: 16_273, owner_id: cyno_char, location_id: cyno_ship, flag: InventoryFlag::Cargo, quantity: 5000, singleton: false, ..Default::default() });
    });
    let sys_low = ctx.systems.get_or_spawn(lowsec_sys);
    sys_low.spawn_ball_with_meta(
        Ball { id: cyno_ship, type_id: 587, position: DVec3::ZERO, velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 1.0, mass: 1e6, radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop },
        CombatState { capacitor: 50_000.0, max_capacitor: 50_000.0, ..Default::default() },
        SlimItemMeta::default(),
    ).await.unwrap();
    let sys_from = ctx.systems.get_or_spawn(jump_from_sys);
    sys_from.spawn_ball_with_meta(
        Ball { id: jumper_ship, type_id: 23_911, position: DVec3::ZERO, velocity: DVec3::ZERO, max_velocity: 100.0, inertia: 1.0, mass: 1e9, radius: 1500.0, speed_fraction: 0.0, mode: MovementMode::Stop },
        CombatState { capacitor: 50_000.0, max_capacitor: 50_000.0, ..Default::default() },
        SlimItemMeta::default(),
    ).await.unwrap();

    // 1. Activate normal cyno generator -> then mismatch beacon_id/sys_id -> then expire clock & test cleanup_expired_beacons
    let std_mod = ctx.sde.types.get(&21_096).unwrap().clone();
    let act = handle_cyno_generator_activation(&ctx, &sys_low, cyno_char, cyno_ship, lowsec_sys, 94_250_010, &std_mod)
        .await.unwrap().unwrap();
    let bid = act.dict_get("beaconID").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(live_beacon(&ctx, cyno_char, bid + 999, lowsec_sys).err().unwrap().code, "NoActiveBeacon");
    ctx.db.clock().advance_secs(1200);
    assert!(live_beacon(&ctx, cyno_char, 0, lowsec_sys).err().unwrap().message.contains("collapsed"));

    // 2. Cyno inhibitor near cyno_ship blocks normal cyno activation -> CynoInhibited
    let inhib_id = 94_350_001_i64;
    let now_u = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(inhib_id, InventoryItemRecord { item_id: inhib_id, type_id: 33_476, owner_id: cyno_char, location_id: lowsec_sys as i64, flag: InventoryFlag::None, quantity: 1, singleton: true, ..Default::default() });
        snap.item_positions.insert(inhib_id, [1000.0, 0.0, 0.0]);
        write_deployable_state_snap(snap, |st| {
            st.deployables.insert(inhib_id, MobileDeployableRecord {
                item_id: inhib_id, type_id: 33_476, group_id: 1250, owner_id: cyno_char, solar_system_id: lowsec_sys,
                anchor_start_unix: now_u.saturating_sub(300), anchor_duration_secs: 60,
                anchor_completed: true, is_unanchored: false, expires_at_unix: now_u + 3600,
                last_interaction_unix: now_u, reinforced_until_unix: None, last_ping_unix: None,
                position: [1000.0, 0.0, 0.0], deployed_fleet_id: Some(fleet_id),
            });
        });
    });
    assert_eq!(
        handle_cyno_generator_activation(&ctx, &sys_low, cyno_char, cyno_ship, lowsec_sys, 94_250_011, &std_mod)
            .await.unwrap().unwrap_err().code,
        "CynoInhibited"
    );

    // Clear inhibitor, relight cyno, test jump_to_fleet_beacon SessionChangeTimerActive, NotEnoughFuel, and success!
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&inhib_id);
        snap.item_positions.remove(&inhib_id);
        write_deployable_state_snap(snap, |st| { st.deployables.remove(&inhib_id); });
    });
    let _ = handle_cyno_generator_activation(&ctx, &sys_low, cyno_char, cyno_ship, lowsec_sys, 94_250_012, &std_mod)
        .await.unwrap().unwrap();
    let mut jumper_sess = SessionState {
        user_id: Some(jumper_char as u64), character_id: Some(jumper_char), corporation_id: Some(corp_id),
        solar_system_id: Some(jump_from_sys), ship_id: Some(jumper_ship), fleet_id: Some(fleet_id),
        ..Default::default()
    };
    let timer_now = ctx.now_unix();
    crate::services::space_destiny::record_session_change_timer(&ctx, jumper_char, timer_now, jump_from_sys, 0, "jump");
    assert_eq!(
        jump_to_fleet_beacon(&ctx, &mut jumper_sess, &[PyRep::Integer(cyno_char), PyRep::Integer(lowsec_sys as i64), PyRep::Integer(0)], false)
            .await.err().unwrap().code,
        "SessionChangeTimerActive"
    );
    ctx.db.clock().advance_secs(30);
    assert_eq!(
        jump_to_fleet_beacon(&ctx, &mut jumper_sess, &[PyRep::Integer(cyno_char), PyRep::Integer(lowsec_sys as i64), PyRep::Integer(0)], false)
            .await.err().unwrap().code,
        "NotEnoughFuel"
    );
    // Grant fuel and complete jump_to_fleet_beacon!
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(94_250_099, InventoryItemRecord { item_id: 94_250_099, type_id: 16_274, owner_id: jumper_char, location_id: jumper_ship, flag: InventoryFlag::Cargo, quantity: 10_000, singleton: false, ..Default::default() });
    });
    let out = jump_to_fleet_beacon(&ctx, &mut jumper_sess, &[PyRep::Integer(cyno_char), PyRep::Integer(lowsec_sys as i64), PyRep::Integer(0)], false)
        .await.expect("jump_to_fleet_beacon succeeds");
    assert!(out.fuel_qty > 0);
    assert_eq!(jumper_sess.solar_system_id, Some(lowsec_sys));

    // 3. Deployable beacon with beacon_id=0 fallback + 56099 unspooled space_object_meta
    ctx.db.clock().advance_secs(1200);
    let dep56099 = 94_350_099_i64;
    let d_now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(dep56099, InventoryItemRecord { item_id: dep56099, type_id: 56_099, owner_id: cyno_char, location_id: lowsec_sys as i64, flag: InventoryFlag::None, quantity: 1, singleton: true, ..Default::default() });
        snap.sp6_lifecycle.space_object_meta.insert(dep56099, eve_db::PersistedSpaceObjectMeta { anchor_start_unix: Some(d_now), anchor_completed: false, ..Default::default() });
    });
    assert_eq!(
        resolve_fleet_beacon_or_destroyed(&ctx, jumper_char, cyno_char, 0, lowsec_sys, true)
            .await.err().unwrap().code,
        "NoActiveBeacon"
    );
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(m) = snap.sp6_lifecycle.space_object_meta.get_mut(&dep56099) {
            m.anchor_completed = true;
        }
    });
    assert!(resolve_fleet_beacon_or_destroyed(&ctx, jumper_char, cyno_char, 0, lowsec_sys, true).await.is_ok());

    // 4. Unanchored deployable (is_unanchored: true), WH/Abyssal/Zarzakh/Pochven/OutOfRange beacon_sys, and ShipCannotJumpToBeacon
    let unanch_id = 94_350_101_i64;
    let dep_bad_id = 94_350_102_i64;
    let now2 = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(unanch_id, InventoryItemRecord { item_id: unanch_id, type_id: 57_319, owner_id: cyno_char, location_id: lowsec_sys as i64, flag: InventoryFlag::None, quantity: 1, singleton: true, ..Default::default() });
        write_deployable_state_snap(snap, |st| {
            st.deployables.insert(unanch_id, MobileDeployableRecord {
                item_id: unanch_id, type_id: 57_319, group_id: 4093, owner_id: cyno_char, solar_system_id: lowsec_sys,
                anchor_start_unix: now2 - 300, anchor_duration_secs: 60, anchor_completed: true, is_unanchored: true,
                expires_at_unix: now2 + 3600, last_interaction_unix: now2, reinforced_until_unix: None, last_ping_unix: None,
                position: [10_000.0, 0.0, 0.0], deployed_fleet_id: Some(fleet_id),
            });
        });
    });
    assert_eq!(
        resolve_fleet_beacon_or_destroyed(&ctx, jumper_char, cyno_char, unanch_id, lowsec_sys, true)
            .await.err().unwrap().code,
        "NoActiveBeacon"
    );

    // Clear dep56099 & add dep_bad_id in each restricted system with args target_sys = 0 so beacon.solar_system_id triggers 1316..1358!
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&dep56099);
        snap.items.remove(&unanch_id);
        snap.items.insert(dep_bad_id, InventoryItemRecord { item_id: dep_bad_id, type_id: 57_319, owner_id: cyno_char, location_id: wh_sys as i64, flag: InventoryFlag::None, quantity: 1, singleton: true, ..Default::default() });
        write_deployable_state_snap(snap, |st| {
            st.deployables.remove(&unanch_id);
            st.deployables.insert(dep_bad_id, MobileDeployableRecord {
                item_id: dep_bad_id, type_id: 57_319, group_id: 4093, owner_id: cyno_char, solar_system_id: wh_sys,
                anchor_start_unix: now2 - 300, anchor_duration_secs: 60, anchor_completed: true, is_unanchored: false,
                expires_at_unix: now2 + 3600, last_interaction_unix: now2, reinforced_until_unix: None, last_ping_unix: None,
                position: [10_000.0, 0.0, 0.0], deployed_fleet_id: Some(fleet_id),
            });
        });
    });
    jumper_sess.solar_system_id = Some(jump_from_sys);
    let sys_low_spawn = ctx.systems.get_or_spawn(jump_from_sys);
    let _ = sys_low_spawn.remove_ball(jumper_ship).await;
    sys_low_spawn.spawn_ball_with_meta(
        Ball { id: jumper_ship, type_id: 23_911, position: DVec3::ZERO, velocity: DVec3::ZERO, max_velocity: 100.0, inertia: 1.0, mass: 1e9, radius: 1500.0, speed_fraction: 0.0, mode: MovementMode::Stop },
        CombatState { capacitor: 50_000.0, max_capacitor: 50_000.0, ..Default::default() },
        SlimItemMeta::default(),
    ).await.unwrap();
    for (bad_sys, expected_code) in [
        (wh_sys, "JumpDriveForbiddenInWormholeSpace"),
        (abyss_sys, "JumpDriveForbiddenInAbyssalSpace"),
        (zarzakh_sys, "JumpDriveForbiddenInZarzakh"),
        (pochven_sys, "CannotJumpToPochven"),
        (far_lowsec_sys, "JumpDistanceOutOfRange"),
    ] {
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.get_mut(&dep_bad_id).unwrap().location_id = bad_sys as i64;
            write_deployable_state_snap(snap, |st| {
                st.deployables.get_mut(&dep_bad_id).unwrap().solar_system_id = bad_sys;
            });
        });
        assert_eq!(
            jump_to_fleet_beacon(&ctx, &mut jumper_sess, &[PyRep::Integer(cyno_char), PyRep::Integer(0), PyRep::Integer(dep_bad_id)], true)
                .await.err().unwrap().code,
            expected_code
        );
    }
    // ShipCannotJumpToBeacon when ship cannot jump (type 587 Rifter) targeting valid lowsec deployable
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&dep_bad_id).unwrap().location_id = lowsec_sys as i64;
        snap.items.get_mut(&jumper_ship).unwrap().type_id = 587;
        write_deployable_state_snap(snap, |st| {
            st.deployables.get_mut(&dep_bad_id).unwrap().solar_system_id = lowsec_sys;
        });
    });
    assert_eq!(
        jump_to_fleet_beacon(&ctx, &mut jumper_sess, &[PyRep::Integer(cyno_char), PyRep::Integer(0), PyRep::Integer(dep_bad_id)], true)
            .await.err().unwrap().code,
        "ShipCannotJumpToBeacon"
    );

    // 5. ship_cyno_activity_expiry_unix, is_ship_cyno_active (beacon + bridge), and on_cyno_ship_destroyed cleanup
    let _ = handle_cyno_generator_activation(&ctx, &sys_low, cyno_char, cyno_ship, lowsec_sys, 94_250_013, &std_mod)
        .await.unwrap().unwrap();
    let br_exp = ctx.now_unix() + 500;
    {
        let mut st = cyno_state().write().unwrap();
        st.bridges.insert((ctx.instance_id as usize, cyno_ship), ActiveJumpBridge {
            bridger_char_id: cyno_char,
            bridger_ship_id: cyno_ship,
            portal_module_id: 94_350_556,
            portal_module_type_id: 21_096,
            origin_system_id: lowsec_sys,
            beacon: FleetModuleBeacon {
                beacon_id: 94_350_557,
                char_id: cyno_char,
                ship_id: cyno_ship,
                beacon_type_id: 21_094,
                solar_system_id: lowsec_sys,
                position: DVec3::ZERO,
                expires_at_unix: br_exp,
            },
            distance_ly: 1.0,
            expires_at_unix: br_exp,
            portal_ball_id: 94_350_555,
        });
    }
    assert!(is_ship_cyno_active(&ctx, cyno_ship));
    assert!(ship_cyno_activity_expiry_unix(&ctx, cyno_ship) >= br_exp);
    on_cyno_ship_destroyed(&ctx, cyno_ship);
}

#[test]
fn purge_expired_beacons_and_bridges_respects_ctx_and_exact_expiry_boundary() {
    let mut st = super::super::CynoPortalState::default();
    let ctx_a = 101usize;
    let ctx_b = 202usize;
    let mk_beacon = |id: i64, exp: u64| super::super::FleetModuleBeacon {
        beacon_id: id,
        char_id: 94_000_001,
        ship_id: 94_000_100 + id,
        beacon_type_id: 21_094,
        solar_system_id: 30_002_505,
        position: DVec3::ZERO,
        expires_at_unix: exp,
    };
    let mk_bridge = |id: i64, exp: u64| super::super::ActiveJumpBridge {
        bridger_char_id: 94_000_001,
        bridger_ship_id: 94_000_200 + id,
        portal_module_id: 94_000_300 + id,
        portal_module_type_id: 21_096,
        origin_system_id: 30_002_505,
        beacon: mk_beacon(id, exp),
        distance_ly: 2.5,
        expires_at_unix: exp,
        portal_ball_id: 94_000_400 + id,
    };

    for (idx, &(ck, exp)) in [
        (ctx_a, 999_u64),
        (ctx_a, 1_000_u64),
        (ctx_a, 1_001_u64),
        (ctx_b, 500_u64),
    ]
    .iter()
    .enumerate()
    {
        let key = (ck, 10 + idx as i64);
        st.beacons.insert(key, mk_beacon(idx as i64 + 1, exp));
        st.destroyed_beacons.insert(key, mk_beacon(idx as i64 + 10, exp));
        st.bridges.insert(key, mk_bridge(idx as i64 + 20, exp));
    }

    super::super::purge_expired(&mut st, ctx_a, 1_000);
    assert_eq!(st.beacons.len(), 2);
    assert!(st.beacons.contains_key(&(ctx_a, 12)));
    assert!(st.beacons.contains_key(&(ctx_b, 13)));
    assert_eq!(st.destroyed_beacons.len(), 2);
    assert!(st.destroyed_beacons.contains_key(&(ctx_a, 12)));
    assert!(st.destroyed_beacons.contains_key(&(ctx_b, 13)));
    assert_eq!(st.bridges.len(), 4);

    super::super::purge_expired_bridges(&mut st, ctx_a, 1_000);
    assert_eq!(st.bridges.len(), 2);
    assert!(st.bridges.contains_key(&(ctx_a, 12)));
    assert!(st.bridges.contains_key(&(ctx_b, 13)));
}

#[tokio::test]
async fn cyno_light_relocate_ship_relocate_caller_and_caller_in_space_pin_mutants() {
    let custom_cyno_tid = 94_490_001_u32;
    let custom_ind_cyno_tid = 94_490_002_u32;
    let custom_cov_cyno_tid = 94_490_003_u32;
    let custom_beacon_tid = 21_094_u32;
    let custom_cov_beacon_tid = 28_646_u32;
    let t3c_tid = 94_490_010_u32;
    let blops_ship_tid = 22_428_u32; // Redeemer or black ops / ship eligible for standard cyno
    let poch_sys = 30_002_770_u32;
    let hisec_sys = 30_000_142_u32;
    let low_a = 30_002_505_u32;
    let low_b = 30_002_506_u32;
    let fuel_tid = 16_274_u32; // Liquid Ozone

    let ctx = test_ctx_with_custom(|sde| {
        sde.solar_systems.insert(
            poch_sys,
            custom_system(sde, poch_sys, 10_000_070, -0.5, [0.0, 0.0, 0.0]),
        );
        sde.solar_systems.insert(
            low_a,
            custom_system(sde, low_a, 10_000_030, 0.2, [0.0, 0.0, 0.0]),
        );
        sde.solar_systems.insert(
            low_b,
            custom_system(sde, low_b, 10_000_030, 0.2, [2.5, 0.0, 0.0]),
        );

        let mut cyno_t = custom_inv_type(
            sde,
            custom_cyno_tid,
            658,
            7,
            1.0,
            "Custom Cyno",
            &[
                (attr_ids::DURATION, 300_000.0),
                (super::super::ATTR_CONSUMPTION_TYPE, f64::from(fuel_tid)),
                (super::super::ATTR_CONSUMPTION_QUANTITY, 200.0),
                (super::super::ATTR_CYNO_SPAWN_RADIUS, 2_500.0),
                (182, 21_603.0),
                (277, 2.0),
            ],
        );
        cyno_t.effect_ids = vec![16];
        sde.types.insert(custom_cyno_tid, cyno_t);

        let mut ind_t = custom_inv_type(
            sde,
            custom_ind_cyno_tid,
            658,
            7,
            1.0,
            "Custom Ind Cyno",
            &[
                (attr_ids::DURATION, 120_000.0),
                (super::super::ATTR_CONSUMPTION_TYPE, f64::from(fuel_tid)),
                (super::super::ATTR_CONSUMPTION_QUANTITY, 100.0),
                (super::super::ATTR_IS_INDUSTRIAL_CYNO, 1.0),
                (super::super::ATTR_CYNO_SPAWN_RADIUS, 2_500.0),
            ],
        );
        ind_t.effect_ids = vec![16];
        sde.types.insert(custom_ind_cyno_tid, ind_t);

        let mut cov_t = custom_inv_type(
            sde,
            custom_cov_cyno_tid,
            658,
            7,
            1.0,
            "Custom Covert Cyno",
            &[
                (attr_ids::DURATION, 60_000.0),
                (super::super::ATTR_CONSUMPTION_TYPE, f64::from(fuel_tid)),
                (super::super::ATTR_CONSUMPTION_QUANTITY, 50.0),
                (super::super::ATTR_IS_COVERT, 1.0),
                (super::super::ATTR_CYNO_SPAWN_RADIUS, 2_500.0),
            ],
        );
        cov_t.effect_ids = vec![16];
        sde.types.insert(custom_cov_cyno_tid, cov_t);

        let mut t3c = custom_inv_type(
            sde,
            t3c_tid,
            963,
            6,
            150.0,
            "T3 Cruiser",
            &[],
        );
        t3c.effect_ids = Vec::new();
        sde.types.insert(t3c_tid, t3c);

        if !sde.types.contains_key(&custom_beacon_tid) {
            sde.types.insert(
                custom_beacon_tid,
                custom_inv_type(sde, custom_beacon_tid, 684, 2, 100.0, "Cyno Beacon", &[]),
            );
        }
        if !sde.types.contains_key(&custom_cov_beacon_tid) {
            sde.types.insert(
                custom_cov_beacon_tid,
                custom_inv_type(sde, custom_cov_beacon_tid, 684, 2, 100.0, "Covert Beacon", &[]),
            );
        }
        let mut bl = custom_inv_type(
            sde,
            blops_ship_tid,
            898,
            6,
            400.0,
            "Custom Black Ops",
            &[(1296, -50.0), (attr_ids::CAPACITOR_CAPACITY, 50_000.0)],
        );
        bl.effect_ids.push(3526);
        sde.types.insert(blops_ship_tid, bl);
    });

    let char_id = 94_491_001_i64;
    let ship_id = 94_491_100_i64;
    let mod_id = 94_491_201_i64;
    let cov_mod_id = 94_491_202_i64;
    let ind_mod_id = 94_491_203_i64;
    let corp_id = 98_491_001_i64;
    let fleet_id = 94_491_900_i64;
    let now = ctx.now_unix();

    ensure_char(&ctx, char_id, corp_id, low_a, ship_id, Some(fleet_id));
    insert_item(&ctx, ship_id, t3c_tid, char_id, i64::from(low_a), InventoryFlag::None, 1);
    insert_item(&ctx, mod_id, custom_cyno_tid, char_id, ship_id, InventoryFlag::HiSlot0, 1);
    insert_item(&ctx, cov_mod_id, custom_cov_cyno_tid, char_id, ship_id, InventoryFlag::HiSlot1, 1);
    insert_item(&ctx, ind_mod_id, custom_ind_cyno_tid, char_id, ship_id, InventoryFlag::HiSlot2, 1);

    let std_mod = ctx.sde.types[&custom_cyno_tid].clone();
    let cov_mod = ctx.sde.types[&custom_cov_cyno_tid].clone();
    let ind_mod = ctx.sde.types[&custom_ind_cyno_tid].clone();

    let sys_low_a = ctx.systems.get_or_spawn(low_a);
    spawn_ship(&ctx, low_a, ship_id, t3c_tid, char_id, DVec3::ZERO, MovementMode::Stop, 50_000.0).await;

    // 1. light_cyno error gates: Zarzakh, Pochven, Highsec, NoFleet, Speed >= 500, T3C Covert Reconfig missing, SkillMissing
    assert_eq!(
        super::super::light_cyno(&ctx, &sys_low_a, char_id, ship_id, 30_100_000, mod_id, &std_mod).await.unwrap_err().code,
        "CynoDisallowedInZarzakh"
    );
    assert_eq!(
        super::super::light_cyno(&ctx, &sys_low_a, char_id, ship_id, poch_sys, cov_mod_id, &cov_mod).await.unwrap_err().code,
        "CynoJammedInPochven"
    );
    assert_eq!(
        super::super::light_cyno(&ctx, &sys_low_a, char_id, ship_id, hisec_sys, cov_mod_id, &cov_mod).await.unwrap_err().code,
        "CannotLightCynoInSystem"
    );
    // Covert T3C without attr 3320 -> CannotLightCynoInSystem
    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.insert(fleet_id, FleetRecord { fleet_id, boss_id: char_id, members: vec![char_id], ..Default::default() });
    });
    let cov_err = super::super::light_cyno(&ctx, &sys_low_a, char_id, ship_id, low_a, cov_mod_id, &cov_mod).await.unwrap_err();
    assert_eq!(cov_err.code, "CannotLightCynoInSystem");

    // Switch ship to blops_ship_tid; check NotInFleet, MovingTooFast, SkillMissing, FuelShortage rollback + retry
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&ship_id).unwrap().type_id = blops_ship_tid;
        snap.fleets.remove(&fleet_id);
        snap.characters.get_mut(&char_id).unwrap().fleet_id = None;
    });
    assert_eq!(
        super::super::light_cyno(&ctx, &sys_low_a, char_id, ship_id, low_a, mod_id, &std_mod).await.unwrap_err().code,
        "MustBeInFleet"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.insert(fleet_id, FleetRecord { fleet_id, boss_id: char_id, members: vec![char_id], ..Default::default() });
        snap.characters.get_mut(&char_id).unwrap().fleet_id = Some(fleet_id);
    });
    // Set ship speed == 500.0 -> ShipMovingTooFast
    let _ = sys_low_a.remove_ball(ship_id).await;
    sys_low_a.spawn_ball_with_meta(
        Ball {
            id: ship_id, type_id: blops_ship_tid, position: DVec3::ZERO,
            velocity: DVec3::new(500.0, 0.0, 0.0), max_velocity: 600.0, inertia: 1.0,
            mass: 5e6, radius: 500.0, speed_fraction: 0.8, mode: MovementMode::Stop,
        },
        CombatState { capacitor: 50_000.0, max_capacitor: 50_000.0, ..Default::default() },
        SlimItemMeta::default(),
    ).await.unwrap();
    assert_eq!(
        super::super::light_cyno(&ctx, &sys_low_a, char_id, ship_id, low_a, mod_id, &std_mod).await.unwrap_err().code,
        "ShipMovingTooFast"
    );
    // Stop ship, test SkillMissing (req skill 21603 level 2 missing -> error)
    let _ = sys_low_a.remove_ball(ship_id).await;
    spawn_ship(&ctx, low_a, ship_id, blops_ship_tid, char_id, DVec3::ZERO, MovementMode::Stop, 50_000.0).await;
    assert_eq!(
        super::super::light_cyno(&ctx, &sys_low_a, char_id, ship_id, low_a, mod_id, &std_mod).await.unwrap_err().code,
        "SkillMissing"
    );
    // Grant skill 21603 level 5 -> fails on fuel & cleanly clears claim (`-ship_id`) so retry succeeds!
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            21_603,
            TrainedSkillRecord { type_id: 21_603, level: 5, skill_points: 256_000.0 },
        );
    });
    assert!(super::super::light_cyno(&ctx, &sys_low_a, char_id, ship_id, low_a, mod_id, &std_mod).await.is_err());
    assert!(!is_ship_cyno_active(&ctx, ship_id));

    // Load 1_000 Liquid Ozone (expected consumption: 200 * 0.5 (role) * 0.5 (CFT lvl 5) = 50)
    insert_item(&ctx, 94_491_301, fuel_tid, char_id, ship_id, InventoryFlag::Cargo, 1_000);
    let cyno_kv = super::super::light_cyno(&ctx, &sys_low_a, char_id, ship_id, low_a, mod_id, &std_mod)
        .await
        .expect("light_cyno succeeds");
    assert_eq!(cyno_kv.dict_get("moduleID").and_then(PyRep::as_i64), Some(mod_id));
    assert_eq!(cyno_kv.dict_get("shipID").and_then(PyRep::as_i64), Some(ship_id));
    let real_beacon_id = cyno_kv.dict_get("beaconID").and_then(PyRep::as_i64).unwrap();
    assert!(real_beacon_id > 0 && real_beacon_id != -ship_id);
    assert_eq!(cyno_kv.dict_get("solarSystemID").and_then(PyRep::as_i64), Some(i64::from(low_a)));
    assert_eq!(cyno_kv.dict_get("isCovert").and_then(PyRep::as_bool), Some(false));
    assert_eq!(cyno_kv.dict_get("consumptionQuantity").and_then(PyRep::as_i64), Some(50));
    assert_eq!(cyno_kv.dict_get("durationSeconds").and_then(PyRep::as_f64), Some(300.0));
    assert_eq!(cyno_kv.dict_get("expiresAtUnix").and_then(PyRep::as_i64), Some((now + 300) as i64));

    // 2. caller_in_space boundaries, expired bridge cleanup, active bridge error, cooldown boundary, & relocate_ship/relocate_caller
    let mut sess = SessionState::new(94_491);
    sess.user_id = Some(char_id as u64);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.solar_system_id = Some(low_a);
    sess.ship_id = Some(ship_id);

    // Clear active cyno for jump tests
    on_cyno_ship_destroyed(&ctx, ship_id);
    let sys_low_b = ctx.systems.get_or_spawn(low_b);
    // Add structure obstacle right at arrival spot in low_b so arrival push branch fires cleanly!
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            94_491_800,
            UpwellStructureRecord {
                structure_id: 94_491_800,
                type_id: 35832,
                owner_corp_id: corp_id,
                solar_system_id: low_b,
                position: [10_000.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );
        snap.characters.get_mut(&char_id).unwrap().jump_fatigue_until_unix = now + 600;
        snap.characters.get_mut(&char_id).unwrap().jump_cooldown_until_unix = now + 120;
    });

    // Cooldown > now triggers JumpReactivationCooldown
    assert_eq!(super::super::caller_in_space(&ctx, &sess, true).await.unwrap_err().code, "JumpReactivationCooldown");
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().jump_cooldown_until_unix = now; // == now passes
    });
    // Expired bridge cleaned up on caller_in_space
    let exp_bridge_exp = now;
    {
        let mut st = super::super::cyno_state().write().unwrap();
        st.bridges.insert(
            (super::super::ctx_key(&ctx), ship_id),
            super::super::ActiveJumpBridge {
                bridger_char_id: char_id,
                bridger_ship_id: ship_id,
                portal_module_id: cov_mod_id,
                portal_module_type_id: custom_cov_cyno_tid,
                origin_system_id: low_a,
                beacon: super::super::FleetModuleBeacon {
                    beacon_id: 94_491_950,
                    char_id,
                    ship_id: ship_id + 1,
                    beacon_type_id: custom_beacon_tid,
                    solar_system_id: low_b,
                    position: DVec3::ZERO,
                    expires_at_unix: exp_bridge_exp,
                },
                distance_ly: 2.5,
                expires_at_unix: exp_bridge_exp,
                portal_ball_id: 94_491_951,
            },
        );
    }
    let (c_id, s_id, c_sys, c_ball) = super::super::caller_in_space(&ctx, &sess, true).await.unwrap();
    assert_eq!((c_id, s_id, c_sys, c_ball.id), (char_id, ship_id, low_a, ship_id));

    // Active bridge -> JumpPortalOpen
    {
        let mut st = super::super::cyno_state().write().unwrap();
        st.bridges.insert(
            (super::super::ctx_key(&ctx), ship_id),
            super::super::ActiveJumpBridge {
                bridger_char_id: char_id,
                bridger_ship_id: ship_id,
                portal_module_id: cov_mod_id,
                portal_module_type_id: custom_cov_cyno_tid,
                origin_system_id: low_a,
                beacon: super::super::FleetModuleBeacon {
                    beacon_id: 94_491_952,
                    char_id,
                    ship_id: ship_id + 1,
                    beacon_type_id: custom_beacon_tid,
                    solar_system_id: low_b,
                    position: DVec3::ZERO,
                    expires_at_unix: now + 300,
                },
                distance_ly: 2.5,
                expires_at_unix: now + 300,
                portal_ball_id: 94_491_953,
            },
        );
    }
    assert_eq!(super::super::caller_in_space(&ctx, &sess, true).await.unwrap_err().code, "CynosuralFieldActive");
    on_cyno_ship_destroyed(&ctx, ship_id);

    // Industrial cyno min floor 400.0
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&94_491_301).unwrap().quantity = 500;
    });
    let ind_kv = super::super::light_cyno(&ctx, &sys_low_a, char_id, ship_id, low_a, ind_mod_id, &ind_mod).await.unwrap();
    assert_eq!(ind_kv.dict_get("consumptionQuantity").and_then(PyRep::as_i64), Some(400));

    // relocate_ship auth failures + full relocate_caller 3-tuple verification & SessionChangeTimerActive
    assert_eq!(
        super::super::relocate_ship(&ctx, char_id, ship_id + 55, low_a, low_b, DVec3::new(10_000.0, 0.0, 0.0), 2.5, 0.5).await.unwrap_err().code,
        "SystemTransitionFailed"
    );
    let (cd_min, fat_min, bound_id) = super::super::relocate_caller(
        &ctx,
        &mut sess,
        char_id,
        ship_id,
        low_a,
        low_b,
        DVec3::new(10_000.0, 0.0, 0.0),
        2.5,
        0.5,
    )
    .await
    .expect("relocate_caller succeeds");
    assert!(cd_min > 1.0 && fat_min > cd_min);
    assert!(bound_id > 0);
    assert_eq!(sess.solar_system_id, Some(low_b));
    let dest_ball = sys_low_b.get_ball_snapshot(ship_id).await.unwrap().unwrap().1;
    assert!(dest_ball.position.distance(DVec3::new(10_000.0, 0.0, 0.0)) > 1_000.0);
    assert_eq!(dest_ball.speed_fraction, 1.0);
    assert_eq!(
        super::super::relocate_ship(&ctx, char_id, ship_id, low_b, low_a, DVec3::ZERO, 2.5, 0.5).await.unwrap_err().code,
        "SessionChangeTimerActive"
    );
}

