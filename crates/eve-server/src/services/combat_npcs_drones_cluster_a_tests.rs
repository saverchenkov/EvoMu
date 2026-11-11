#![forbid(unsafe_code)]

use super::*;
use eve_db::{
    InventoryFlag, InventoryItemRecord, TrainedSkillRecord, VirtualServerClock, WorldRepository,
};
use eve_destiny::{Ball, CombatState, DamageVector, MovementMode, SlimItemMeta};
use eve_dogma::{FittedModule, ModuleState};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{InvType, SdeRegistry};
use glam::DVec3;
use std::collections::HashMap;
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
    volume: f64,
    name: &str,
    attrs: &[(u16, f64)],
) -> InvType {
    let mut t = sde.types.get(&587).expect("seed type").clone();
    t.type_id = type_id;
    t.group_id = group_id;
    t.category_id = category_id;
    t.volume = volume;
    t.name = name.into();
    t.default_attributes.clear();
    for &(k, v) in attrs {
        t.default_attributes.insert(k, v);
    }
    t
}

fn test_ctx_with_custom<F: FnOnce(&mut SdeRegistry)>(mutate_sde: F) -> ServerContext {
    let mut sde = (*shared_sde()).clone();
    mutate_sde(&mut sde);
    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(Arc::new(sde), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    ctx
}

#[test]
fn drone_tracking_falloff_cycle_hit_multiplier_and_control_range_math() {
    let ctx = test_ctx_with_custom(|sde| {
        sde.types.insert(
            2454,
            custom_inv_type(
                sde,
                2454,
                100,
                18,
                5.0,
                "Hobgoblin I",
                &[(51, 4_000.0), (54, 2_000.0), (158, 3_000.0), (160, 1.8)],
            ),
        );
    });

    let d = DroneSpaceState {
        ball_id: 1_000_955_001,
        drone_item_id: 1_000_955_002,
        type_id: 2454,
        group_id: 100,
        owner_char_id: 95_500_001,
        original_owner_char_id: 95_500_001,
        parent_ship_id: 1_000_955_000,
        solar_system_id: 30_000_142,
        target_id: None,
        activity_state: 1,
        assist_char_id: None,
        guard_char_id: None,
        is_abandoned: false,
        is_sentry: true,
        bandwidth_used: 5.0,
        max_velocity: 3_000.0,
        optimal_range: 2_000.0,
        damage_vector: DamageVector {
            em: 0.0,
            thermal: 16.0,
            kinetic: 0.0,
            explosive: 0.0,
        },
        shield_repair_amount: 0.0,
        armor_repair_amount: 0.0,
        hull_repair_amount: 0.0,
        web_speed_factor: 0.0,
        neut_amount: 0.0,
        ecm_strength: 0.0,
        tp_sig_bonus_pct: 0.0,
    };

    assert!((d.effective_tracking_speed(&ctx) - 1.8).abs() < 1e-6);
    assert!((d.effective_falloff_range(&ctx) - 3_000.0).abs() < 1e-6);
    assert_eq!(d.cycle_duration_secs(&ctx), 4);

    let drone_ball = Ball {
        id: d.ball_id,
        type_id: 2454,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 3_000.0,
        inertia: 1.0,
        mass: 2_500.0,
        radius: 5.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let close_stationary = Ball {
        id: 2,
        type_id: 587,
        position: DVec3::new(1_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    assert!((d.compute_combat_hit_multiplier(&ctx, &drone_ball, &close_stationary, 50.0) - 1.0).abs() < 1e-6);

    let far_fast = Ball {
        id: 3,
        type_id: 587,
        position: DVec3::new(8_000.0, 0.0, 0.0),
        velocity: DVec3::new(0.0, 2_000.0, 0.0),
        max_velocity: 2_000.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 30.0,
        speed_fraction: 1.0,
        mode: MovementMode::Stop,
    };
    assert!(d.compute_combat_hit_multiplier(&ctx, &drone_ball, &far_fast, 30.0) < 0.25);

    // Control range: base 20km -> +5km*4 (Drone Avionics IV) + 3km*3 (Adv Drone Avionics III) = 49km
    let char_id = 95_500_001;
    let ship_id = 1_000_955_000;
    assert!((compute_drone_control_range(&ctx, char_id, ship_id) - 20_000.0).abs() < 1e-6);
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        ch.user_id = char_id as u64;
        snap.characters.insert(char_id, ch);
        let sk = snap.skills.entry(char_id).or_default();
        sk.insert(
            3437,
            TrainedSkillRecord {
                type_id: 3437,
                level: 4,
                skill_points: 100_000.0,
            },
        );
        sk.insert(
            23618,
            TrainedSkillRecord {
                type_id: 23618,
                level: 3,
                skill_points: 100_000.0,
            },
        );
    });
    assert!((compute_drone_control_range(&ctx, char_id, ship_id) - 49_000.0).abs() < 1e-6);
}

#[tokio::test]
async fn tractor_beam_and_drone_launch_engage_scoop_and_destruction_lifecycle() {
    let ctx = test_ctx_with_custom(|sde| {
        sde.types.insert(
            995_001,
            custom_inv_type(
                sde,
                995_001,
                25,
                6,
                10_000.0,
                "Test Drone Hull",
                &[(283, 50.0), (1271, 25.0)],
            ),
        );
        sde.types.insert(
            2454,
            custom_inv_type(
                sde,
                2454,
                100,
                18,
                5.0,
                "Hobgoblin I",
                &[(37, 3_000.0), (51, 4_000.0), (54, 2_000.0), (116, 10.0), (1272, 5.0)],
            ),
        );
        sde.types.insert(
            24348,
            custom_inv_type(
                sde,
                24348,
                650,
                7,
                5.0,
                "Small Tractor Beam I",
                &[(54, 20_000.0), (73, 5_000.0), (624, 1_000.0)],
            ),
        );
    });

    let char_id = 95_500_010;
    let sys_id = 30_002_505u32;
    let ship_id = 1_000_955_010;
    let drone_id = 1_000_955_011;
    let wreck_id = 1_000_955_012;
    let foreign_wreck_id = 1_000_955_013;
    let tractor_id = 1_000_955_014;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        ch.user_id = char_id as u64;
        ch.corporation_id = 98_500_001;
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = None;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);
        snap.accounts.insert(
            format!("user_{char_id}"),
            eve_db::AccountRecord {
                user_id: char_id as u64,
                username: format!("user_{char_id}"),
                password_token: String::new(),
                role: 0,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
        snap.skills.entry(char_id).or_default().insert(
            3436,
            TrainedSkillRecord {
                type_id: 3436,
                level: 5,
                skill_points: 256_000.0,
            },
        );
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 995_001,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            drone_id,
            InventoryItemRecord {
                item_id: drone_id,
                type_id: 2454,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::DroneBay,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            wreck_id,
            InventoryItemRecord {
                item_id: wreck_id,
                type_id: 26561,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            foreign_wreck_id,
            InventoryItemRecord {
                item_id: foreign_wreck_id,
                type_id: 26561,
                owner_id: 99_999_999,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    let _ = ctx
        .db
        .acquire_online_lock(char_id as u64, char_id as u64, Some(char_id), true, ctx.now_unix_secs());

    let sys = ctx.systems.get_or_spawn(sys_id);
    let ship_ball = Ball {
        id: ship_id,
        type_id: 995_001,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    sys.spawn_ball_with_meta(
        ship_ball.clone(),
        CombatState::default(),
        SlimItemMeta {
            group_id: 25,
            category_id: 6,
            owner_id: char_id,
            char_id: Some(char_id),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    for (wid, pos) in [(wreck_id, DVec3::new(10_000.0, 0.0, 0.0)), (foreign_wreck_id, DVec3::new(10_000.0, 0.0, 0.0))] {
        sys.spawn_ball_with_meta(
            Ball {
                id: wid,
                type_id: 26561,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 1_000.0,
                radius: 30.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
            SlimItemMeta {
                group_id: 186,
                category_id: 2,
                owner_id: char_id,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    }

    // 1. Tractor Beam guards & pull execution
    let fm = FittedModule {
        item_id: tractor_id,
        type_id: 24348,
        flag_id: InventoryFlag::HiSlot0 as u16,
        state: ModuleState::Online,
        charge_type_id: None,
        charge_quantity: 0,
        damage_hp: 0.0,
        max_hp: 40.0,
        dynamic_attribute_overrides: HashMap::new(),
    };
    let tractor_type = ctx.sde.types.get(&24348).unwrap().clone();
    let kw = HashMap::new();
    assert_eq!(
        activate_salvager_or_tractor_module(&ctx, char_id, ship_id, sys_id, tractor_id, &fm, &tractor_type, &ship_ball, None, &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "TargetRequired"
    );
    assert_eq!(
        activate_salvager_or_tractor_module(
            &ctx,
            char_id,
            ship_id,
            sys_id,
            tractor_id,
            &fm,
            &tractor_type,
            &ship_ball,
            Some(foreign_wreck_id),
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "TractorBeamOwnershipRestricted"
    );
    let pulled = activate_salvager_or_tractor_module(
        &ctx,
        char_id,
        ship_id,
        sys_id,
        tractor_id,
        &fm,
        &tractor_type,
        &ship_ball,
        Some(wreck_id),
        &kw,
    )
    .await
    .unwrap()
    .expect("tractor pull");
    assert!((pulled.dict_get("newDistanceMeters").and_then(|v| v.as_f64()).unwrap() - 5_000.0).abs() < 1e-3);

    // 2. LaunchDrones in space, engage, scoop back to drone bay, then launch again and destroy in space
    let mut sess = SessionState {
        user_id: Some(char_id as u64),
        character_id: Some(char_id),
        corporation_id: Some(98_500_001),
        solar_system_id: Some(sys_id),
        ship_id: Some(ship_id),
        ..Default::default()
    };
    let launch_rep = handle_npcs_drones(
        &ctx,
        &mut sess,
        "ship",
        "LaunchDrones",
        &[PyRep::List(vec![PyRep::Integer(drone_id)])],
        &kw,
    )
    .await
    .unwrap()
    .expect("LaunchDrones");
    let launched_ball_id = with_combat_runtime(&ctx, |rt| {
        rt.active_drones_in_space
            .values()
            .find(|d| d.drone_item_id == drone_id)
            .map(|d| d.ball_id)
            .unwrap()
    });
    assert!(launch_rep != PyRep::None);

    let scoop_rep = handle_npcs_drones(
        &ctx,
        &mut sess,
        "ship",
        "ScoopToDroneBay",
        &[PyRep::Integer(launched_ball_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("ScoopToDroneBay");
    assert_eq!(scoop_rep.dict_get("scoopedDroneItemID").and_then(|v| v.as_i64()), Some(drone_id));

    // Launch again and destroy via apply_damage_to_drone_in_space
    let _ = handle_npcs_drones(
        &ctx,
        &mut sess,
        "ship",
        "LaunchDrones",
        &[PyRep::List(vec![PyRep::Integer(drone_id)])],
        &kw,
    )
    .await
    .unwrap()
    .expect("re-launch drone");
    let ball_id_2 = with_combat_runtime(&ctx, |rt| {
        rt.active_drones_in_space
            .values()
            .find(|d| d.drone_item_id == drone_id)
            .map(|d| d.ball_id)
            .unwrap()
    });
    let (_, destroyed) = apply_damage_to_drone_in_space(
        &ctx,
        sys_id,
        ball_id_2,
        DamageVector {
            em: 5_000.0,
            thermal: 5_000.0,
            kinetic: 5_000.0,
            explosive: 5_000.0,
        },
    )
    .await;
    assert!(destroyed);
    assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&drone_id)));
}

async fn call_drone_rpc(
    ctx: &ServerContext,
    session: &mut SessionState,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let kw = std::collections::HashMap::new();
    for svc in ["entity", "ship", "beyonce", "dogmaIM", "droneMgr"] {
        if let Some(res) = handle_npcs_drones(ctx, session, svc, method, args, &kw).await {
            return res;
        }
    }
    panic!("handle_npcs_drones did not claim method {method}");
}

#[tokio::test]
async fn combat_hit_multiplier_salvager_tractor_and_pilot_capacity_edge_cases() {
    use eve_db::{InventoryFlag, InventoryItemRecord};
    use eve_destiny::{Ball, DamageVector, MovementMode};
    use eve_dogma::{FittedModule, ModuleState};
    use glam::DVec3;
    use std::collections::HashMap;

    let ctx = test_ctx_with_custom(|sde| {
        custom_inv_type(
            sde,
            32787,
            1159,
            18,
            5.0,
            "Salvage Drone I",
            &[(161, 5.0), (263, 250.0), (265, 250.0), (9, 250.0)],
        );
        custom_inv_type(
            sde,
            41030,
            101,
            18,
            500.0,
            "Excavator Mining Drone",
            &[(161, 500.0), (263, 1000.0), (265, 1000.0), (9, 1000.0)],
        );
        custom_inv_type(
            sde,
            25861,
            1122,
            7,
            5.0,
            "Salvager I",
            &[(54, 5_000.0)],
        );
    })
    .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED);

    let char_id = 94_100_001i64;
    let sys_id: u32 = 30_000_142;
    let ship_id = 10_941_000_001i64;
    let wreck_id = 10_941_000_002i64;
    let missing_drone_id = 10_941_000_003i64;
    let drone_item_id = 10_941_000_004i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut c = snap.characters.values().next().cloned().expect("seed char");
        c.character_id = char_id;
        c.name = "DroneCapPilot".to_string();
        c.solar_system_id = sys_id;
        c.active_ship_id = ship_id;
        snap.characters.insert(char_id, c);
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 593,
                owner_id: char_id,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            drone_item_id,
            InventoryItemRecord {
                item_id: drone_item_id,
                type_id: 2454,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::DroneBay,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    let d = DroneSpaceState {
        ball_id: 10_941_000_099,
        drone_item_id,
        type_id: 2454,
        group_id: 100,
        owner_char_id: char_id,
        original_owner_char_id: char_id,
        parent_ship_id: ship_id,
        solar_system_id: sys_id,
        target_id: Some(wreck_id),
        activity_state: 1,
        assist_char_id: None,
        guard_char_id: None,
        is_abandoned: false,
        is_sentry: false,
        bandwidth_used: 5.0,
        max_velocity: 2_600.0,
        optimal_range: 1_000.0,
        damage_vector: DamageVector::new(0.0, 40.0, 0.0, 0.0),
        shield_repair_amount: 0.0,
        armor_repair_amount: 0.0,
        hull_repair_amount: 0.0,
        web_speed_factor: 0.0,
        neut_amount: 0.0,
        ecm_strength: 0.0,
        tp_sig_bonus_pct: 0.0,
    };
    let b_drone = Ball {
        id: d.ball_id,
        type_id: 2454,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 2600.0,
        inertia: 1.0,
        mass: 5000.0,
        radius: 25.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let b_close = Ball {
        id: wreck_id,
        type_id: 26557,
        position: DVec3::new(0.5, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 0.0,
        inertia: 1.0,
        mass: 5000.0,
        radius: 25.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let mut b_far = b_close.clone();
    b_far.position = DVec3::new(100_000.0, 0.0, 0.0);
    assert!(d.compute_combat_hit_multiplier(&ctx, &b_drone, &b_close, 40.0) >= 0.0);
    let mut d_sentry = d.clone();
    d_sentry.is_sentry = true;
    assert_eq!(d_sentry.compute_combat_hit_multiplier(&ctx, &b_drone, &b_far, 40.0), 0.0);

    // pilot_drone_capacity without fitting
    let (max_d, _bw, _cap) = pilot_drone_capacity(&ctx, char_id, ship_id, None);
    assert!(max_d <= 5);

    let attacker_ball = Ball {
        id: ship_id,
        type_id: 593,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let dummy_mod = FittedModule {
        item_id: 50,
        type_id: 25861,
        flag_id: 27,
        state: ModuleState::Active,
        damage_hp: 0.0,
        max_hp: 40.0,
        charge_type_id: None,
        charge_quantity: 0,
        dynamic_attribute_overrides: HashMap::new(),
    };
    let salvager_type = ctx.sde.types[&25861].clone();
    let kw = HashMap::new();
    let _ = activate_salvager_or_tractor_module(
        &ctx,
        char_id,
        ship_id,
        sys_id,
        50,
        &dummy_mod,
        &salvager_type,
        &attacker_ball,
        Some(wreck_id),
        &kw,
    )
    .await;

    let (_dmg, destroyed) = apply_damage_to_drone_in_space(
        &ctx,
        sys_id,
        missing_drone_id,
        DamageVector::new(100.0, 0.0, 0.0, 0.0),
    )
    .await;
    assert!(!destroyed);
}

#[tokio::test]
async fn launch_engage_return_assist_abandon_scoop_and_salvage_drone_branches() {
    use eve_db::{InventoryFlag, InventoryItemRecord};
    use eve_destiny::{Ball, CombatState, MovementMode};
    use glam::DVec3;
    use std::collections::HashMap;

    let ctx = test_ctx_with_custom(|sde| {
        custom_inv_type(
            sde,
            32787,
            1159,
            18,
            5.0,
            "Salvage Drone I",
            &[(161, 5.0), (263, 250.0), (265, 250.0), (9, 250.0), (54, 1000.0), (64, 5.0), (1272, 5.0)],
        );
        custom_inv_type(
            sde,
            41030,
            101,
            18,
            500.0,
            "Excavator",
            &[(161, 500.0), (263, 1000.0), (265, 1000.0), (9, 1000.0), (1272, 20.0)],
        );
        custom_inv_type(
            sde,
            2454,
            100,
            18,
            5.0,
            "Hobgoblin I",
            &[(161, 5.0), (263, 250.0), (265, 250.0), (9, 250.0), (51, 3000.0), (54, 5000.0), (64, 5.0), (114, 10.0), (116, 10.0), (1272, 5.0)],
        );
        custom_inv_type(
            sde,
            593,
            25,
            6,
            10_000.0,
            "Tristan",
            &[(1271, 50.0), (283, 100.0)],
        );
    })
    .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED);

    let char_id = 94_100_010i64;
    let ally_id = 94_100_011i64;
    let corp_id = 98_400_010i64;
    let sys_id: u32 = 30_000_142;
    let ship_id = 10_941_000_010i64;
    let ally_ship_id = 10_941_000_011i64;
    let exc_id = 10_941_000_012i64;
    let hob_id = 10_941_000_013i64;
    let salvage_id = 10_941_000_014i64;
    let zero_hp_drone_id = 10_941_000_015i64;
    let target_id = 10_941_000_016i64;
    let wreck_id = 10_941_000_017i64;

    ctx.db.with_mut_snapshot(|snap| {
        let base = snap.characters.values().next().cloned().expect("seed char");
        let mut c = base.clone();
        c.character_id = char_id;
        c.name = "DroneCommander".to_string();
        c.corporation_id = corp_id;
        c.solar_system_id = sys_id;
        c.active_ship_id = ship_id;
        snap.characters.insert(char_id, c);

        let base_sk = snap
            .skills
            .values()
            .next()
            .and_then(|m| m.values().next().cloned())
            .expect("seed skill");
        for (sid, lvl) in [(3436, 5), (3440, 1)] {
            let mut s = base_sk.clone();
            s.type_id = sid;
            s.level = lvl;
            snap.skills.entry(char_id).or_default().insert(sid, s);
        }

        let mut ally = base;
        ally.character_id = ally_id;
        ally.name = "AllyPilot".to_string();
        ally.corporation_id = corp_id;
        ally.solar_system_id = sys_id;
        ally.active_ship_id = ally_ship_id;
        snap.characters.insert(ally_id, ally);

        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 593,
                owner_id: char_id,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            exc_id,
            InventoryItemRecord {
                item_id: exc_id,
                type_id: 41030,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::DroneBay,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        let mut hob_dyn = HashMap::new();
        hob_dyn.insert(9, 100.0);
        hob_dyn.insert(263, 200.0);
        hob_dyn.insert(265, 200.0);
        snap.items.insert(
            hob_id,
            InventoryItemRecord {
                item_id: hob_id,
                type_id: 2454,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::DroneBay,
                quantity: 1,
                singleton: true,
                dynamic_attributes: hob_dyn,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            salvage_id,
            InventoryItemRecord {
                item_id: salvage_id,
                type_id: 32787,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::DroneBay,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            zero_hp_drone_id,
            InventoryItemRecord {
                item_id: zero_hp_drone_id,
                type_id: 2454,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::DroneBay,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let cs = CombatState {
        shield_hp: 500.0,
        max_shield_hp: 500.0,
        armor_hp: 500.0,
        max_armor_hp: 500.0,
        structure_hp: 500.0,
        max_structure_hp: 500.0,
        capacitor: 500.0,
        max_capacitor: 500.0,
        ..Default::default()
    };
    let mk_ball = |id: i64, type_id: u32, x: f64| Ball {
        id,
        type_id,
        position: DVec3::new(x, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    sys_handle.try_spawn_ball_with_combat(mk_ball(ship_id, 593, 0.0), cs.clone());
    sys_handle.try_spawn_ball_with_combat(mk_ball(ally_ship_id, 593, 2000.0), cs.clone());
    sys_handle.try_spawn_ball_with_combat(mk_ball(target_id, 593, 3000.0), cs.clone());
    sys_handle.try_spawn_ball_with_combat(mk_ball(wreck_id, 26557, 1500.0), cs);

    let mut session = SessionState::new(73_001);
    session.user_id = Some(1);
    session.character_id = Some(char_id);
    session.corporation_id = Some(corp_id);
    session.solar_system_id = Some(sys_id);
    session.ship_id = Some(ship_id);

    assert!(call_drone_rpc(&ctx, &mut session, "LaunchDrones", &[]).await.is_err());
    assert!(
        call_drone_rpc(&ctx, &mut session, "LaunchDrones", &[PyRep::List(vec![PyRep::Integer(exc_id)])])
            .await
            .is_err()
    );
    let _ = call_drone_rpc(&ctx, &mut session, "LaunchDrones", &[PyRep::Integer(hob_id)]).await;
    let _ = call_drone_rpc(
        &ctx,
        &mut session,
        "LaunchDrones",
        &[PyRep::List(vec![PyRep::Integer(salvage_id), PyRep::Integer(zero_hp_drone_id)])],
    )
    .await;

    assert!(call_drone_rpc(&ctx, &mut session, "CmdEngage", &[]).await.is_err());
    let _ = call_drone_rpc(
        &ctx,
        &mut session,
        "CmdEngage",
        &[PyRep::List(vec![PyRep::Integer(hob_id)]), PyRep::Integer(target_id)],
    )
    .await;
    let _ = call_drone_rpc(
        &ctx,
        &mut session,
        "CmdEngage",
        &[PyRep::List(vec![PyRep::Integer(hob_id)]), PyRep::Integer(target_id)],
    )
    .await;

    let _ = call_drone_rpc(
        &ctx,
        &mut session,
        "CmdAssist",
        &[PyRep::List(vec![PyRep::Integer(hob_id)]), PyRep::Integer(ally_ship_id)],
    )
    .await;
    let _ = call_drone_rpc(
        &ctx,
        &mut session,
        "CmdGuard",
        &[PyRep::List(vec![PyRep::Integer(hob_id)]), PyRep::Integer(ally_ship_id)],
    )
    .await;
    let _ = call_drone_rpc(
        &ctx,
        &mut session,
        "ChangeDroneSettings",
        &[PyRep::Dict(vec![(PyRep::String("aggressive".to_string()), PyRep::Bool(true))])],
    )
    .await;
    assert!(call_drone_rpc(&ctx, &mut session, "CmdSalvage", &[]).await.is_err());
    let _ = call_drone_rpc(
        &ctx,
        &mut session,
        "CmdSalvage",
        &[PyRep::List(vec![PyRep::Integer(salvage_id)]), PyRep::Integer(wreck_id)],
    )
    .await;

    let _ = call_drone_rpc(
        &ctx,
        &mut session,
        "CmdReturnHome",
        &[PyRep::List(vec![PyRep::Integer(hob_id)])],
    )
    .await;
    let _ = call_drone_rpc(
        &ctx,
        &mut session,
        "CmdReturnBay",
        &[PyRep::List(vec![PyRep::Integer(hob_id)])],
    )
    .await;
    let _ = call_drone_rpc(
        &ctx,
        &mut session,
        "CmdAbandonDrone",
        &[PyRep::List(vec![PyRep::Integer(salvage_id)])],
    )
    .await;
    let _ = call_drone_rpc(
        &ctx,
        &mut session,
        "CmdReconnectToDrones",
        &[PyRep::List(vec![PyRep::Integer(salvage_id)])],
    )
    .await;
    assert!(call_drone_rpc(&ctx, &mut session, "ScoopDrone", &[]).await.is_err());
    let _ = call_drone_rpc(&ctx, &mut session, "ScoopDrone", &[PyRep::Integer(target_id)]).await;
    let _ = call_drone_rpc(&ctx, &mut session, "ScoopDrone", &[PyRep::Integer(salvage_id)]).await;
}

#[tokio::test]
async fn launch_engage_return_scoop_error_and_sentry_kill_paths() {
    let mut ctx = test_ctx_with_custom(|sde| {
        sde.types.insert(
            2454,
            custom_inv_type(
                sde,
                2454,
                100,
                18,
                5.0,
                "Hobgoblin I",
                &[(51, 4_000.0), (54, 2_000.0), (158, 3_000.0), (160, 1.8), (1271, 5.0), (64, 50.0)],
            ),
        );
        sde.types.insert(
            23563,
            custom_inv_type(
                sde,
                23563,
                100,
                18,
                25.0,
                "Garde I",
                &[(51, 4_000.0), (54, 40_000.0), (158, 3_000.0), (160, 0.5), (1271, 25.0), (64, 120.0)],
            ),
        );
        sde.types.insert(
            587001,
            custom_inv_type(sde, 587001, 25, 6, 5_000.0, "Drone Carrier Test", &[(283, 100.0), (1271, 100.0)]),
        );
        sde.types.insert(
            587002,
            custom_inv_type(sde, 587002, 25, 6, 5_000.0, "Zero Bay Test", &[(283, 0.0), (1271, 50.0)]),
        );
    })
    .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED);
    let _ = &mut ctx;

    let (base_char, sys_id) = ctx.db.with_snapshot(|snap| {
        (snap.characters.values().next().cloned().expect("demo char"), 30_000_142u32)
    });
    let char_id = 92_700_001i64;
    let other_char_id = 92_700_002i64;
    let ship_id = 1_927_000_010i64;
    let zero_bay_ship_id = 1_927_000_011i64;
    let hob_in_bay = 1_927_000_020i64;
    let hob_in_cargo = 1_927_000_021i64;
    let non_drone_in_bay = 1_927_000_022i64;
    let space_non_drone = 1_927_000_023i64;
    let space_other_drone = 1_927_000_024i64;
    let sentry_ball = 1_927_000_030i64;
    let far_drone_ball = 1_927_000_031i64;
    let fragile_npc_id = 1_927_000_040i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut c1 = base_char.clone();
        c1.character_id = char_id;
        c1.solar_system_id = sys_id;
        c1.station_id = None;
        c1.structure_id = None;
        c1.active_ship_id = ship_id;
        snap.characters.insert(char_id, c1);

        let mut c2 = base_char;
        c2.character_id = other_char_id;
        c2.solar_system_id = sys_id;
        c2.station_id = None;
        snap.characters.insert(other_char_id, c2);

        let mk = |item_id, type_id, owner_id, location_id, flag| InventoryItemRecord {
            item_id,
            type_id,
            owner_id,
            location_id,
            flag,
            quantity: 1,
            singleton: true,
            ..Default::default()
        };
        snap.items.insert(ship_id, mk(ship_id, 587001, char_id, sys_id as i64, InventoryFlag::None));
        snap.items.insert(zero_bay_ship_id, mk(zero_bay_ship_id, 587002, char_id, sys_id as i64, InventoryFlag::None));
        snap.items.insert(hob_in_bay, mk(hob_in_bay, 2454, char_id, ship_id, InventoryFlag::DroneBay));
        snap.items.insert(hob_in_cargo, mk(hob_in_cargo, 2454, char_id, ship_id, InventoryFlag::Cargo));
        snap.items.insert(non_drone_in_bay, mk(non_drone_in_bay, 587001, char_id, ship_id, InventoryFlag::DroneBay));
        snap.items.insert(space_non_drone, mk(space_non_drone, 587001, char_id, sys_id as i64, InventoryFlag::None));
        snap.items.insert(space_other_drone, mk(space_other_drone, 2454, other_char_id, sys_id as i64, InventoryFlag::None));
        snap.skills.insert(
            char_id,
            HashMap::from([
                (3436u32, TrainedSkillRecord { type_id: 3436, skill_points: 256_000.0, level: 5 }),
                (3437u32, TrainedSkillRecord { type_id: 3437, skill_points: 256_000.0, level: 5 }),
            ]),
        );
    });

    let sys = ctx.systems.get_or_spawn(sys_id);
    let mk_ball = |id, type_id, x| Ball {
        id,
        type_id,
        position: DVec3::new(x, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 2.0,
        mass: 50_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let _ = sys.spawn_ball(mk_ball(ship_id, 587001, 0.0)).await;
    let _ = sys.spawn_ball(mk_ball(zero_bay_ship_id, 587002, 0.0)).await;
    let _ = sys.spawn_ball(mk_ball(space_non_drone, 587001, 200.0)).await;
    let _ = sys.spawn_ball(mk_ball(space_other_drone, 2454, 200.0)).await;
    let _ = sys.spawn_ball(mk_ball(sentry_ball, 23563, 500.0)).await;
    let _ = sys.spawn_ball(mk_ball(far_drone_ball, 2454, 5_000_000.0)).await;
    let _ = sys.try_spawn_ball_with_combat(
        mk_ball(fragile_npc_id, 2454, 1_000.0),
        CombatState {
            shield_hp: 0.0,
            max_shield_hp: 10.0,
            armor_hp: 0.0,
            max_armor_hp: 10.0,
            structure_hp: 1.0,
            max_structure_hp: 10.0,
            ..Default::default()
        },
    );

    with_mut_combat_runtime(&ctx, |rt| {
        let mk_drone_state = |id, owner, parent, tid, bw, dmg, is_sentry| DroneSpaceState {
            ball_id: id,
            drone_item_id: id,
            type_id: tid,
            group_id: 100,
            owner_char_id: owner,
            original_owner_char_id: owner,
            parent_ship_id: parent,
            solar_system_id: sys_id,
            target_id: None,
            activity_state: 0,
            assist_char_id: None,
            guard_char_id: None,
            is_abandoned: false,
            is_sentry,
            bandwidth_used: bw,
            max_velocity: 1_000.0,
            optimal_range: 20_000.0,
            damage_vector: dmg,
            shield_repair_amount: 0.0,
            armor_repair_amount: 0.0,
            hull_repair_amount: 0.0,
            web_speed_factor: 0.0,
            neut_amount: 0.0,
            ecm_strength: 0.0,
            tp_sig_bonus_pct: 0.0,
        };
        rt.active_drones_in_space.insert(
            sentry_ball,
            mk_drone_state(
                sentry_ball,
                char_id,
                ship_id,
                23563,
                25.0,
                DamageVector { em: 0.0, thermal: 200.0, kinetic: 0.0, explosive: 0.0 },
                true,
            ),
        );
        rt.active_drones_in_space.insert(
            far_drone_ball,
            mk_drone_state(
                far_drone_ball,
                char_id,
                ship_id,
                2454,
                5.0,
                DamageVector { em: 0.0, thermal: 10.0, kinetic: 0.0, explosive: 0.0 },
                false,
            ),
        );
        rt.active_drones_in_space.insert(
            space_other_drone,
            mk_drone_state(
                space_other_drone,
                other_char_id,
                ship_id + 99,
                2454,
                5.0,
                DamageVector::default(),
                false,
            ),
        );
    });

    let mut sess = SessionState::new(92_700);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);

    // 1. LaunchDrones error paths (docked, missing sys, cloaked, DB missing, not in bay, not a drone, over drone cap)
    sess.station_id = Some(60_003_760);
    assert_eq!(call_drone_rpc(&ctx, &mut sess, "LaunchDrones", &[PyRep::Integer(hob_in_bay)]).await.unwrap_err().code, "CantLaunchDronesWhileDocked");
    sess.station_id = None;
    sess.solar_system_id = None;
    assert_eq!(call_drone_rpc(&ctx, &mut sess, "LaunchDrones", &[PyRep::Integer(hob_in_bay)]).await.unwrap_err().code, "NoSolarSystem");
    sess.solar_system_id = Some(sys_id);

    let _ = sys.try_update_combat_state(ship_id, |cs| cs.is_cloaked = true);
    assert!(call_drone_rpc(&ctx, &mut sess, "LaunchDrones", &[PyRep::Integer(hob_in_bay)]).await.is_err());
    let _ = sys.try_update_combat_state(ship_id, |cs| cs.is_cloaked = false);

    assert_eq!(call_drone_rpc(&ctx, &mut sess, "LaunchDrones", &[PyRep::Tuple(vec![PyRep::Integer(99_999_888)])]).await.unwrap_err().code, "DroneNotFound");
    assert_eq!(call_drone_rpc(&ctx, &mut sess, "LaunchDrones", &[PyRep::Integer(hob_in_cargo)]).await.unwrap_err().code, "DroneNotInBay");
    assert_eq!(call_drone_rpc(&ctx, &mut sess, "LaunchDrones", &[PyRep::Integer(non_drone_in_bay)]).await.unwrap_err().code, "NotADrone");
    let over_cap: Vec<PyRep> = (0..8).map(|k| PyRep::Integer(hob_in_bay + 100 + k)).collect();
    assert!(call_drone_rpc(&ctx, &mut sess, "LaunchDrones", &[PyRep::List(over_cap)]).await.is_err());

    // 2. CmdReturnAndOrbit (Integer/List/out-of-range/sentry MovementMode::Stop)
    sess.ship_id = None;
    let _ = call_drone_rpc(&ctx, &mut sess, "CmdReturnAndOrbit", &[PyRep::Integer(sentry_ball)]).await; // DB active_ship_id fallback
    sess.ship_id = Some(ship_id);
    assert!(call_drone_rpc(&ctx, &mut sess, "CmdReturnAndOrbit", &[PyRep::Tuple(vec![PyRep::Integer(sentry_ball), PyRep::Integer(far_drone_ball)])]).await.is_ok());

    // 3. CmdEngage guards (docked, tethered, cloaked) + kill path
    sess.station_id = Some(60_003_760);
    assert_eq!(call_drone_rpc(&ctx, &mut sess, "CmdEngage", &[PyRep::Integer(fragile_npc_id)]).await.unwrap_err().code, "ShipDocked");
    sess.station_id = None;

    let _ = sys.try_update_combat_state(ship_id, |cs| cs.is_tethered = true);
    assert_eq!(call_drone_rpc(&ctx, &mut sess, "CmdEngage", &[PyRep::Tuple(vec![PyRep::Integer(sentry_ball)]), PyRep::Integer(fragile_npc_id)]).await.unwrap_err().code, "CantActivateWhileTethered");
    let _ = sys.try_update_combat_state(ship_id, |cs| cs.is_tethered = false);

    let _ = sys.try_update_combat_state(ship_id, |cs| cs.is_cloaked = true);
    assert!(call_drone_rpc(&ctx, &mut sess, "CmdEngage", &[PyRep::Integer(fragile_npc_id), PyRep::List(vec![PyRep::Integer(sentry_ball)])]).await.is_err());
    let _ = sys.try_update_combat_state(ship_id, |cs| cs.is_cloaked = false);

    // TargetNotLocked error path, then lock and engage fragile NPC to destroy it
    assert_eq!(
        call_drone_rpc(&ctx, &mut sess, "CmdEngage", &[PyRep::Integer(fragile_npc_id), PyRep::List(vec![PyRep::Integer(sentry_ball)])])
            .await
            .unwrap_err()
            .code,
        "TargetNotLocked"
    );
    let _ = sys
        .add_target_lock(
            ship_id,
            eve_destiny::TargetLock {
                target_id: fragile_npc_id,
                lock_time_ms: 100.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: Some(50_000.0),
            },
        )
        .await;
    let engage_res = call_drone_rpc(&ctx, &mut sess, "CmdEngage", &[PyRep::Integer(fragile_npc_id), PyRep::List(vec![PyRep::Integer(sentry_ball)])]).await;
    assert!(engage_res.is_ok(), "CmdEngage failed: {engage_res:?}");

    // 4. ScoopDrone error paths
    assert_eq!(call_drone_rpc(&ctx, &mut sess, "ScoopDrone", &[PyRep::Integer(99_888_777)]).await.unwrap_err().code, "DroneNotInSpace");
    assert_eq!(call_drone_rpc(&ctx, &mut sess, "ScoopDrone", &[PyRep::Integer(far_drone_ball)]).await.unwrap_err().code, "OutOfScoopRange");
    assert_eq!(call_drone_rpc(&ctx, &mut sess, "ScoopDrone", &[PyRep::Integer(space_other_drone)]).await.unwrap_err().code, "DroneOwnedByOther");
    assert_eq!(call_drone_rpc(&ctx, &mut sess, "ScoopDrone", &[PyRep::Integer(space_non_drone)]).await.unwrap_err().code, "NotADrone");
    assert_eq!(call_drone_rpc(&ctx, &mut sess, "ScoopDrone", &[PyRep::Integer(ship_id)]).await.unwrap_err().code, "NotADrone");
    assert_eq!(call_drone_rpc(&ctx, &mut sess, "ScoopDrone", &[PyRep::Integer(sentry_ball)]).await.unwrap_err().code, "DroneNotFound");
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            sentry_ball,
            InventoryItemRecord {
                item_id: sentry_ball,
                type_id: 23563,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    sess.ship_id = Some(zero_bay_ship_id);
    assert_eq!(call_drone_rpc(&ctx, &mut sess, "ScoopDrone", &[PyRep::Integer(sentry_ball)]).await.unwrap_err().code, "DroneNotInSpace");
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().active_ship_id = zero_bay_ship_id;
    });
    assert_eq!(call_drone_rpc(&ctx, &mut sess, "ScoopDrone", &[PyRep::Integer(sentry_ball)]).await.unwrap_err().code, "InsufficientDroneBayCapacity");
}


