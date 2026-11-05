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
