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

#[tokio::test]
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
        s.characters.get_mut(&char_id).unwrap().jump_cooldown_until_unix = 0;
        s.characters.get_mut(&char_id).unwrap().jump_fatigue_until_unix = 0;
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

#[tokio::test]
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
