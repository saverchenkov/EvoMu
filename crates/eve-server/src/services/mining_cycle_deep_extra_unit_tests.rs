use crate::router::ServerContext;
use crate::services::mining_cycle::{
    activate, active_cycles_for_ship, can_harvest, deactivate, mining_kind_of,
    process_mining_cycles, CrystalUse, MiningKind, MiningStopReason,
};
use crate::services::serverops_cluster::set_system_tidi_factor;
use crate::system_actor::AsteroidState;
use eve_db::{
    AsteroidRecord, CharacterRecord, InventoryFlag, InventoryItemRecord, IskCents,
    NeuralAttributes, ServerClock, TrainedSkillRecord, VirtualServerClock, WorldRepository,
};
use eve_destiny::{Ball, MovementMode, TargetLock};
use eve_sde::{
    CategoryId, DogmaEffectType, EffectId, GroupId, InvType, SdeRegistry, TypeId, TypeListInfo,
};
use glam::DVec3;
use std::sync::Arc;

fn mk_type(
    type_id: TypeId,
    group_id: GroupId,
    category_id: CategoryId,
    name: &str,
    volume: f64,
    capacity: f64,
    default_effect_id: Option<EffectId>,
    attrs: &[(u16, f64)],
) -> InvType {
    InvType {
        type_id,
        group_id,
        category_id,
        name: name.to_string(),
        mass: 1_000.0,
        volume,
        capacity,
        radius: 100.0,
        base_price: 100.0,
        published: true,
        portion_size: 100,
        market_group_id: None,
        race_id: None,
        faction_id: None,
        variation_parent_type_id: None,
        description_id: None,
        icon_id: None,
        graphic_id: None,
        sound_id: None,
        meta_group_id: None,
        meta_level: None,
        tech_level: None,
        default_effect_id,
        description: String::new(),
        effect_ids: {
            let mut ids: Vec<EffectId> = default_effect_id.into_iter().collect();
            if default_effect_id.is_some() {
                ids.push(12);
            }
            ids
        },
        default_attributes: attrs.iter().copied().collect(),
    }
}

fn mk_char(character_id: i64, solar_system_id: u32, active_ship_id: i64) -> CharacterRecord {
    CharacterRecord {
        activity_status: None,
        character_id,
        user_id: character_id as u64,
        name: format!("Miner-{character_id}"),
        corporation_id: 98_000_001,
        alliance_id: None,
        war_faction_id: None,
        station_id: None,
        structure_id: None,
        solar_system_id,
        constellation_id: 20_000_020,
        region_id: 10_000_002,
        active_ship_id,
        balance: IskCents::default(),
        plex_balance: 0,
        loyalty_points: Default::default(),
        standings: Default::default(),
        security_status: 5.0,
        clone_grade: 2,
        home_station_id: 60_003_760,
        race_id: 1,
        bloodline_id: 1,
        ancestry_id: 1,
        school_id: 1,
        neural: NeuralAttributes {
            intelligence: 20,
            memory: 20,
            perception: 20,
            willpower: 20,
            charisma: 19,
            bonus_remaps: 0,
            last_remap_unix: None,
            remap_cooldown_until_unix: None,
        },
        skill_points: 0.0,
        free_skill_points: 0.0,
        last_clone_jump_unix: 0,
        jump_fatigue_minutes: 0.0,
        jump_cooldown_until_unix: 0,
        jump_fatigue_until_unix: 0,
        description: String::new(),
        title: String::new(),
        gender: 1,
        create_date_unix: 0,
        fleet_id: None,
        autopilot_waypoints: Vec::new(),
        version: 1,
        system_epoch: 1,
    }
}

fn build_mining_cycle_sde() -> Arc<SdeRegistry> {
    let mut sde = SdeRegistry::with_canonical_seed();
    sde.effects.insert(
        67,
        DogmaEffectType {
            effect_id: 67,
            name: "miningLaser".into(),
            effect_category: 1,
            is_offensive: true,
            is_assistance: false,
            is_warp_safe: false,
            disallow_auto_repeat: false,
            electronic_chance: false,
            propulsion_chance: false,
            range_chance: false,
            guid: None,
            published: true,
            modifiers: Vec::new(),
        },
    );
    sde.types.insert(3386, mk_type(3386, 255, 16, "Mining", 0.01, 0.0, None, &[]));
    sde.types
        .insert(12180, mk_type(12180, 255, 16, "Deep Core Mining", 0.01, 0.0, None, &[]));
    sde.types
        .insert(1230, mk_type(1230, 462, 25, "Veldspar", 0.1, 0.0, None, &[(182, 3386.0)]));
    sde.types
        .insert(20, mk_type(20, 454, 25, "Kernite", 1.2, 0.0, None, &[(182, 3386.0)]));
    sde.types.insert(
        11396,
        mk_type(11396, 468, 25, "Mercoxit", 40.0, 0.0, None, &[(182, 12180.0)]),
    );
    sde.type_lists.insert(
        9001,
        TypeListInfo {
            type_list_id: 9001,
            name: "Veldspar Specialization".into(),
            included_type_ids: vec![1230],
            ..Default::default()
        },
    );
    sde.types.insert(
        17478,
        mk_type(
            17478,
            463,
            6,
            "Retriever",
            100_000.0,
            400.0,
            None,
            &[
                (14, 2.0),
                (13, 2.0),
                (12, 3.0),
                (1137, 3.0),
                (48, 500.0),
                (11, 100.0),
                (38, 400.0),
                (1556, 5_000.0),
                (482, 1_000.0),
                (55, 1_000_000.0),
            ],
        ),
    );
    sde.types.insert(
        17482,
        mk_type(
            17482,
            54,
            7,
            "Strip Miner I",
            50.0,
            1.0,
            Some(67),
            &[
                (50, 30.0),
                (30, 1.0),
                (77, 100.0),
                (73, 180_000.0),
                (54, 15_000.0),
                (6, 40.0),
                (182, 3386.0),
            ],
        ),
    );
    sde.types.insert(
        24305,
        mk_type(
            24305,
            54,
            7,
            "Modulated Deep Core Strip Miner II",
            50.0,
            1.0,
            Some(67),
            &[
                (50, 30.0),
                (30, 1.0),
                (77, 80.0),
                (73, 180_000.0),
                (54, 15_000.0),
                (6, 40.0),
                (182, 3386.0),
                (183, 12180.0),
            ],
        ),
    );
    sde.types.insert(
        18068,
        mk_type(
            18068,
            482,
            8,
            "Veldspar Mining Crystal Type A I",
            1.0,
            0.0,
            None,
            &[(3148, 9001.0), (786, 1.0), (783, 1.0), (784, 25.0), (9, 50.0)],
        ),
    );
    Arc::new(sde)
}

async fn setup_mining_ship(
    ctx: &ServerContext,
    sys_id: u32,
    char_id: i64,
    ship_id: i64,
    mod_id: i64,
    mod_tid: TypeId,
    ast_id: i64,
    ast_tid: TypeId,
    ast_qty: i64,
) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.insert(char_id, mk_char(char_id, sys_id, ship_id));
        snap.skills.entry(char_id).or_default().insert(
            3386,
            TrainedSkillRecord {
                type_id: 3386,
                level: 5,
                skill_points: 256_000.0,
            },
        );
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 17478,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::Unlocked,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            mod_id,
            InventoryItemRecord {
                item_id: mod_id,
                type_id: mod_tid,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(mod_id, true);
        snap.asteroids.insert(
            ast_id,
            AsteroidRecord {
                asteroid_id: ast_id,
                solar_system_id: sys_id,
                belt_id: 1,
                type_id: ast_tid,
                quantity: ast_qty,
                initial_quantity: ast_qty,
                position: (2_000.0, 0.0, 0.0),
                radius: 100.0,
            },
        );
    });

    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = sys
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 17478,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 150.0,
                inertia: 1.0,
                mass: 10_000_000.0,
                radius: 100.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_ship_combat_state(char_id, ship_id, 17478),
        )
        .await;
    let _ = sys
        .spawn_asteroid(
            AsteroidState {
                asteroid_id: ast_id,
                belt_id: 1,
                type_id: ast_tid,
                initial_units: ast_qty as u64,
                remaining_units: ast_qty as u64,
                unit_volume: 0.1,
                depleted: false,
                respawn_at_tick: None,
            },
            Ball {
                id: ast_id,
                type_id: ast_tid,
                position: DVec3::new(2_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 50_000.0,
                radius: 100.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
        )
        .await;
    let _ = sys
        .add_target_lock(
            ship_id,
            TargetLock {
                target_id: ast_id,
                lock_time_ms: 1_000.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: None,
            },
        )
        .await;
}

#[tokio::test]
async fn test_mining_cycle_mercoxit_skill_gate_and_crystal_specialization() {
    let sde = build_mining_cycle_sde();
    assert_eq!(mining_kind_of(&sde, &sde.types[&17482]), Some(MiningKind::Asteroid));
    assert!(!can_harvest(&sde.types[&17482], MiningKind::Asteroid, &sde.types[&11396]));
    assert!(can_harvest(&sde.types[&24305], MiningKind::Asteroid, &sde.types[&11396]));

    let db = Arc::new(WorldRepository::new_empty());
    let clock = Arc::new(VirtualServerClock::with_fixed_base(1_000));
    let ctx = ServerContext::with_sde_and_repository(sde, db.clone());
    ctx.set_clock(clock.clone());
    setup_mining_ship(&ctx, 30_000_142, 90_001, 200_001, 300_001, 24305, 400_001, 11396, 50).await;

    let err_no_skill = activate(&ctx, 90_001, 200_001, 30_000_142, 300_001, 400_001)
        .await
        .expect_err("requires Deep Core Mining");
    assert_eq!(err_no_skill.code, "ModuleOffline");

    db.with_mut_snapshot(|snap| {
        snap.skills.get_mut(&90_001).unwrap().insert(
            12180,
            TrainedSkillRecord {
                type_id: 12180,
                level: 1,
                skill_points: 2_000.0,
            },
        );
        snap.module_online_states.insert(300_001, true);
    });
    ctx.invalidate_ship_fitting(200_001);
    assert!(activate(&ctx, 90_001, 200_001, 30_000_142, 300_001, 400_001).await.is_ok());
    let _ = deactivate(&ctx, 90_001, Some(200_001), 300_001).await;

    // Crystal specialization match on Veldspar (1230) vs mismatch on Kernite (20)
    db.with_mut_snapshot(|snap| {
        snap.items.insert(
            300_009,
            InventoryItemRecord {
                item_id: 300_009,
                type_id: 18068,
                owner_id: 90_001,
                location_id: 200_001,
                flag: InventoryFlag::HiSlot0,
                quantity: 2,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.module_active_charges.insert(300_001, 18068);
        for (aid, tid) in [(400_002i64, 1230u32), (400_003i64, 20u32)] {
            snap.asteroids.insert(
                aid,
                AsteroidRecord {
                    asteroid_id: aid,
                    solar_system_id: 30_000_142,
                    belt_id: 1,
                    type_id: tid,
                    quantity: 50_000,
                    initial_quantity: 50_000,
                    position: (2_000.0, 0.0, 0.0),
                    radius: 100.0,
                },
            );
        }
    });
    ctx.invalidate_ship_fitting(200_001);
    let sys = ctx.systems.get_or_spawn(30_000_142);
    for (aid, tid) in [(400_002i64, 1230u32), (400_003i64, 20u32)] {
        let _ = sys
            .spawn_asteroid(
                AsteroidState {
                    asteroid_id: aid,
                    belt_id: 1,
                    type_id: tid,
                    initial_units: 50_000,
                    remaining_units: 50_000,
                    unit_volume: 1.0,
                    depleted: false,
                    respawn_at_tick: None,
                },
                Ball {
                    id: aid,
                    type_id: tid,
                    position: DVec3::new(2_000.0, 0.0, 0.0),
                    velocity: DVec3::ZERO,
                    max_velocity: 0.0,
                    inertia: 1.0,
                    mass: 50_000.0,
                    radius: 100.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
            )
            .await;
        let _ = sys
            .add_target_lock(
                200_001,
                TargetLock {
                    target_id: aid,
                    lock_time_ms: 1_000.0,
                    remaining_lock_ms: 0.0,
                    locked: true,
                    max_range_m: None,
                },
            )
            .await;
    }

    assert!(activate(&ctx, 90_001, 200_001, 30_000_142, 300_001, 400_002).await.is_ok());
    clock.advance_secs(180);
    let out_matched = process_mining_cycles(&ctx).await;
    assert_eq!(out_matched[0].crystal, Some(CrystalUse { type_id: 18068, matched: true }));
    let _ = deactivate(&ctx, 90_001, Some(200_001), 300_001).await;

    assert!(activate(&ctx, 90_001, 200_001, 30_000_142, 300_001, 400_003).await.is_ok());
    clock.advance_secs(180);
    let out_mismatched = process_mining_cycles(&ctx).await;
    assert_eq!(out_mismatched[0].crystal, Some(CrystalUse { type_id: 18068, matched: false }));
    let _ = deactivate(&ctx, 90_001, Some(200_001), 300_001).await;
}

#[tokio::test]
async fn test_mining_cycle_crystal_shatter_short_cycle_tidi_and_cap_stop() {
    let sde = build_mining_cycle_sde();
    let db = Arc::new(WorldRepository::new_empty());
    let clock = Arc::new(VirtualServerClock::with_fixed_base(2_000));
    let ctx = ServerContext::with_sde_and_repository(sde, db.clone());
    ctx.set_clock(clock.clone());
    setup_mining_ship(&ctx, 30_000_142, 90_002, 200_002, 300_002, 17482, 400_010, 1230, 100_000).await;

    db.with_mut_snapshot(|snap| {
        snap.items.insert(
            300_099,
            InventoryItemRecord {
                item_id: 300_099,
                type_id: 18068,
                owner_id: 90_002,
                location_id: 200_002,
                flag: InventoryFlag::HiSlot0,
                quantity: 2,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.module_active_charges.insert(300_002, 18068);
    });
    ctx.invalidate_ship_fitting(200_002);

    assert!(activate(&ctx, 90_002, 200_002, 30_000_142, 300_002, 400_010).await.is_ok());
    clock.advance_secs(180);
    assert!(!process_mining_cycles(&ctx).await[0].crystal_shattered);

    clock.advance_secs(180);
    assert!(process_mining_cycles(&ctx).await[0].crystal_shattered);
    db.with_snapshot(|snap| assert_eq!(snap.items[&300_099].quantity, 1));

    clock.advance_secs(360);
    let o34 = process_mining_cycles(&ctx).await;
    assert_eq!(o34.len(), 2);
    assert!(o34[1].crystal_shattered);
    db.with_snapshot(|snap| assert!(!snap.items.contains_key(&300_099)));
    let _ = deactivate(&ctx, 90_002, Some(200_002), 300_002).await;

    // 50% TiDi short-cycle: 180s elapsed of 360s effective duration => cycle_fraction == 0.5
    db.with_mut_snapshot(|snap| {
        snap.module_active_charges.remove(&300_002);
    });
    ctx.invalidate_ship_fitting(200_002);
    set_system_tidi_factor(&ctx, 30_000_142, 0.5);
    assert!(activate(&ctx, 90_002, 200_002, 30_000_142, 300_002, 400_010).await.is_ok());
    assert_eq!(active_cycles_for_ship(&ctx, 200_002).len(), 1);
    clock.advance_secs(180);
    let short_out = deactivate(&ctx, 90_002, Some(200_002), 300_002)
        .await
        .expect("was active")
        .expect("owner ok")
        .expect("short cycle outcome");
    assert!((short_out.cycle_fraction - 0.5).abs() < 1e-6);
    assert_eq!(short_out.stop, Some(MiningStopReason::Deactivated));

    // Capacitor depletion stops repeating cycle at end of full cycle
    set_system_tidi_factor(&ctx, 30_000_142, 1.0);
    let _ = ctx.systems.get_or_spawn(30_000_142).set_capacitor(200_002, 50.0).await;
    assert!(activate(&ctx, 90_002, 200_002, 30_000_142, 300_002, 400_010).await.is_ok());
    clock.advance_secs(180);
    let cap_out = process_mining_cycles(&ctx).await;
    assert_eq!(cap_out.len(), 1);
    assert_eq!(cap_out[0].stop, Some(MiningStopReason::CapacitorEmpty));
}
