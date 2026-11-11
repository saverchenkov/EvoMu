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

    db.with_mut_snapshot(|snap| {
        if !snap.accounts.values().any(|a| a.user_id == 90_002) {
            snap.accounts.insert(
                "miner_90002".into(),
                eve_db::AccountRecord {
                    user_id: 90_002,
                    username: "miner_90002".into(),
                    password_token: String::new(),
                    role: eve_db::service_roles::ROLE_PLAYER_DEFAULT,
                    banned_until_unix: None,
                    ban_reason: None,
                },
            );
        }
    });
    db.acquire_online_lock(90_002, 1, Some(90_002), false, ctx.now_unix())
        .unwrap();
    assert!(activate(&ctx, 90_002, 200_002, 30_000_142, 300_002, 400_010).await.is_ok());
    let _ = ctx.drain_notifications_for_char(90_002);
    clock.advance_secs(180);
    let cyc1 = process_mining_cycles(&ctx).await;
    assert!(!cyc1[0].crystal_shattered);
    let notes1 = ctx.drain_notifications_for_char(90_002);
    let item_chg1 = notes1
        .iter()
        .find(|n| n.event == "OnItemChange")
        .expect("first mining cycle must emit OnItemChange for new ore stack");
    assert!(matches!(&item_chg1.args[1], eve_proto::PyRep::Dict(d) if d.contains(&(eve_proto::PyRep::Integer(3), eve_proto::PyRep::Integer(0))) && !d.iter().any(|(k, _)| k == &eve_proto::PyRep::Integer(9))));

    clock.advance_secs(180);
    let cyc2 = process_mining_cycles(&ctx).await;
    assert!(cyc2[0].crystal_shattered);
    let notes2 = ctx.drain_notifications_for_char(90_002);
    let item_chgs2: Vec<_> = notes2.iter().filter(|n| n.event == "OnItemChange").collect();
    assert_eq!(item_chgs2.len(), 2, "second mining cycle must emit OnItemChange for shattered crystal stack and ore stack");
    assert!(item_chgs2.iter().any(|n| matches!(&n.args[1], eve_proto::PyRep::Dict(d) if d.contains(&(eve_proto::PyRep::Integer(9), eve_proto::PyRep::Integer(2))))));
    assert!(item_chgs2.iter().any(|n| matches!(&n.args[1], eve_proto::PyRep::Dict(d) if d.contains(&(eve_proto::PyRep::Integer(9), eve_proto::PyRep::Integer(cyc1[0].units_delivered))) && !d.iter().any(|(k, _)| k == &eve_proto::PyRep::Integer(3)))));
    db.with_snapshot(|snap| assert_eq!(snap.items[&300_099].quantity, 1));

    clock.advance_secs(360);
    let o34 = process_mining_cycles(&ctx).await;
    assert_eq!(o34.len(), 2);
    assert!(o34[1].crystal_shattered);
    let notes34 = ctx.drain_notifications_for_char(90_002);
    assert!(notes34.iter().any(|n| {
        n.event == "OnItemChange"
            && matches!(
                &n.args[1],
                eve_proto::PyRep::Dict(d)
                    if d.contains(&(eve_proto::PyRep::Integer(3), eve_proto::PyRep::Integer(200_002)))
                        && d.contains(&(eve_proto::PyRep::Integer(4), eve_proto::PyRep::Integer(eve_db::InventoryFlag::HiSlot0 as i64)))
                        && d.contains(&(eve_proto::PyRep::Integer(9), eve_proto::PyRep::Integer(1)))
            )
    }));
    db.with_snapshot(|snap| assert!(!snap.items.contains_key(&300_099)));
    let _ = deactivate(&ctx, 90_002, Some(200_002), 300_002).await;

    // 50% TiDi short-cycle: 180s elapsed of 360s effective duration => cycle_fraction == 0.5
    db.with_mut_snapshot(|snap| {
        snap.module_active_charges.remove(&300_002);
    });
    ctx.invalidate_ship_fitting(200_002);
    set_system_tidi_factor(&ctx, 30_000_142, 0.5);
    db.with_mut_snapshot(|snap| {
        if !snap.accounts.values().any(|a| a.user_id == 90_002) {
            snap.accounts.insert(
                "miner_90002".into(),
                eve_db::AccountRecord {
                    user_id: 90_002,
                    username: "miner_90002".into(),
                    password_token: String::new(),
                    role: eve_db::service_roles::ROLE_PLAYER_DEFAULT,
                    banned_until_unix: None,
                    ban_reason: None,
                },
            );
        }
    });
    db.acquire_online_lock(90_002, 1, Some(90_002), false, ctx.now_unix())
        .unwrap();
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
    db.with_mut_snapshot(|snap| {
        if !snap.accounts.values().any(|a| a.user_id == 90_002) {
            snap.accounts.insert(
                "miner_90002".into(),
                eve_db::AccountRecord {
                    user_id: 90_002,
                    username: "miner_90002".into(),
                    password_token: String::new(),
                    role: eve_db::service_roles::ROLE_PLAYER_DEFAULT,
                    banned_until_unix: None,
                    ban_reason: None,
                },
            );
        }
    });
    db.acquire_online_lock(90_002, 1, Some(90_002), false, ctx.now_unix())
        .unwrap();
    assert!(activate(&ctx, 90_002, 200_002, 30_000_142, 300_002, 400_010).await.is_ok());
    clock.advance_secs(180);
    let cap_out = process_mining_cycles(&ctx).await;
    assert_eq!(cap_out.len(), 1);
    assert_eq!(cap_out[0].stop, Some(MiningStopReason::CapacitorEmpty));
}

fn sde_with_fx_crit_waste_gas_ice() -> Arc<SdeRegistry> {
    let base = build_mining_cycle_sde();
    let mut sde = (*base).clone();

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
            guid: Some("effects.Laser".into()),
            published: true,
            modifiers: Vec::new(),
        },
    );
    sde.effects.insert(
        2726,
        DogmaEffectType {
            effect_id: 2726,
            name: "miningClouds".into(),
            effect_category: 1,
            is_offensive: false,
            is_assistance: false,
            is_warp_safe: false,
            disallow_auto_repeat: false,
            electronic_chance: false,
            propulsion_chance: false,
            range_chance: false,
            guid: Some("effects.CloudMining".into()),
            published: true,
            modifiers: Vec::new(),
        },
    );
    sde.effects.insert(
        17,
        DogmaEffectType {
            effect_id: 17,
            name: "mining".into(),
            effect_category: 1,
            is_offensive: false,
            is_assistance: false,
            is_warp_safe: false,
            disallow_auto_repeat: false,
            electronic_chance: false,
            propulsion_chance: false,
            range_chance: false,
            guid: Some("None".into()),
            published: true,
            modifiers: Vec::new(),
        },
    );

    sde.types.insert(16281, mk_type(16281, 255, 16, "Ice Harvesting", 0.01, 0.0, None, &[]));
    sde.types.insert(25544, mk_type(25544, 255, 16, "Gas Cloud Harvesting", 0.01, 0.0, None, &[]));

    // Crit + waste deterministic Strip Miner variant
    sde.types.insert(
        99_101,
        mk_type(
            99_101,
            54,
            7,
            "CritWaste Strip Miner",
            50.0,
            1.0,
            Some(67),
            &[
                (50, 30.0),
                (30, 1.0),
                (77, 100.0),      // miningAmount = 100 m3 -> 1000 Veldspar units
                (73, 60_000.0),   // duration = 60s
                (54, 15_000.0),
                (6, 40.0),
                (182, 3386.0),
                (5967, 1.0),      // crit_chance = 100%
                (5969, 0.5),      // +50% crit bonus yield -> 500 units
                (3154, 100.0),    // waste_probability = 100%
                (3153, 2.0),      // wasted_volume_multiplier = 2x -> 2000 units
            ],
        ),
    );
    // Ice Harvester module (miningLaser effect, Ice Harvesting skill 16281)
    sde.types.insert(
        99_102,
        mk_type(
            99_102,
            54,
            7,
            "Ice Harvester Test",
            50.0,
            0.0,
            Some(67),
            &[
                (50, 30.0),
                (30, 1.0),
                (77, 1_000.0),
                (73, 60_000.0),
                (54, 15_000.0),
                (6, 20.0),
                (182, 16281.0),
            ],
        ),
    );
    // Gas Harvester module (miningClouds effect 2726)
    sde.types.insert(
        99_103,
        mk_type(
            99_103,
            737,
            7,
            "Gas Cloud Harvester Test",
            5.0,
            0.0,
            Some(2726),
            &[
                (50, 10.0),
                (30, 1.0),
                (77, 50.0),
                (73, 30_000.0),
                (54, 10_000.0),
                (6, 15.0),
                (182, 25544.0),
            ],
        ),
    );
    // Non-mining module (no mining default effect)
    sde.types.insert(
        99_104,
        mk_type(99_104, 52, 7, "Not A Miner", 5.0, 0.0, None, &[]),
    );
    // Ice target (category 25, required skill 16281) & Gas target (non-25, no req skill)
    sde.types.insert(
        99_201,
        mk_type(99_201, 465, 25, "Blue Ice Test", 100.0, 0.0, None, &[(182, 16281.0)]),
    );
    sde.types.insert(
        99_202,
        mk_type(99_202, 711, 2, "Fullerite Gas Test", 1.0, 0.0, None, &[]),
    );
    // Ship with specialized Ice & Gas & Mining holds
    sde.types.insert(
        99_301,
        mk_type(
            99_301,
            463,
            6,
            "OmniHold Barge",
            100_000.0,
            200.0,
            None,
            &[
                (14, 4.0),
                (13, 2.0),
                (12, 2.0),
                (1137, 2.0),
                (48, 500.0),
                (11, 100.0),
                (38, 200.0),
                (1556, 2_000.0),
                (1557, 500.0),
                (3136, 1_500.0),
                (482, 1_000.0),
                (55, 1_000_000.0),
            ],
        ),
    );
    // Ship with NO specialized holds (falls back to Cargo capacity = 120 m3)
    sde.types.insert(
        99_302,
        mk_type(
            99_302,
            25,
            6,
            "CargoFallback Frig",
            20_000.0,
            120.0,
            None,
            &[
                (14, 2.0),
                (13, 2.0),
                (12, 2.0),
                (1137, 2.0),
                (48, 200.0),
                (11, 50.0),
                (38, 120.0),
                (482, 500.0),
                (55, 1_000_000.0),
            ],
        ),
    );
    // Dbuff collection targeting attribute 73 (duration) on skill 3386
    sde.dbuff_collections.insert(
        88_001,
        eve_sde::DbuffCollectionInfo {
            dbuff_id: 88_001,
            name: "Mining Laser Optimization Test".into(),
            operation: "PostPercent".into(),
            aggregate_mode: "Maximum".into(),
            display_name_id: None,
            show_output_value_in_ui: None,
            item_modifiers: Vec::new(),
            location_modifiers: Vec::new(),
            location_group_modifiers: Vec::new(),
            location_required_skill_modifiers: vec![eve_sde::DbuffSkillModifier {
                attribute_id: 73,
                skill_id: 3386,
            }],
            target_attribute_ids: vec![73],
        },
    );
    sde.dbuff_collections.insert(
        88_002,
        eve_sde::DbuffCollectionInfo {
            dbuff_id: 88_002,
            name: "Ignored NonPostPercent Test".into(),
            operation: "ModAdd".into(),
            aggregate_mode: "Sum".into(),
            display_name_id: None,
            show_output_value_in_ui: None,
            item_modifiers: Vec::new(),
            location_modifiers: vec![73],
            location_group_modifiers: Vec::new(),
            location_required_skill_modifiers: Vec::new(),
            target_attribute_ids: vec![73],
        },
    );

    Arc::new(sde)
}

fn mark_online(ctx: &ServerContext, char_id: i64, session_id: u64, now_unix: u64) {
    let uid = char_id as u64;
    let uname = format!("u{uid}");
    ctx.db.with_mut_snapshot(|snap| {
        if !snap.accounts.values().any(|a| a.user_id == uid) {
            snap.accounts.insert(
                uname.clone(),
                eve_db::AccountRecord {
                    user_id: uid,
                    username: uname,
                    password_token: String::new(),
                    role: 0,
                    banned_until_unix: None,
                    ban_reason: None,
                },
            );
        }
    });
    ctx.db
        .acquire_online_lock(uid, session_id, Some(char_id), true, now_unix)
        .unwrap();
}

#[tokio::test]
async fn test_wave2_resolve_and_emit_mining_godma_and_special_fx_scoping() {
    let sde = sde_with_fx_crit_waste_gas_ice();
    let clock = Arc::new(VirtualServerClock::with_fixed_base(1_776_100_000));
    let db = Arc::new(WorldRepository::new_empty());
    let ctx = ServerContext::with_sde_and_repository(sde.clone(), db);
    ctx.set_clock(Arc::clone(&clock) as Arc<dyn ServerClock>);

    // resolve_mining_effect_id_and_fx coverage (with SDE entries & fallbacks)
    assert_eq!(
        crate::services::mining_cycle::resolve_mining_effect_id_and_fx(
            &ctx,
            Some(17482),
            "miningLaser",
            Some(MiningKind::Asteroid),
        ),
        (67, "effects.Laser".to_string(), true)
    );
    assert_eq!(
        crate::services::mining_cycle::resolve_mining_effect_id_and_fx(
            &ctx,
            Some(99_103),
            "defaultEffect",
            Some(MiningKind::Gas),
        ),
        (2726, "effects.CloudMining".to_string(), false)
    );
    // Effect 17 has guid="None", so fallback maps "mining" -> "effects.Mining"
    assert_eq!(
        crate::services::mining_cycle::resolve_mining_effect_id_and_fx(
            &ctx,
            None,
            "mining",
            None,
        ),
        (17, "effects.Mining".to_string(), false)
    );
    // Completely empty name + missing module defaults to 67 / "effects.Laser"
    assert_eq!(
        crate::services::mining_cycle::resolve_mining_effect_id_and_fx(&ctx, None, "", None),
        (67, "effects.Laser".to_string(), true)
    );

    // Setup pilot + observers for emit_mining_godma_and_special_fx scoping:
    // 91_001: pilot (online, in space sys 30_000_142) -> gets OnGodmaShipEffect + OnSpecialFX
    // 91_002: online observer in space sys 30_000_142 -> gets ONLY OnSpecialFX
    // 91_003: online observer docked at station in sys 30_000_142 -> NO notification
    // 91_004: online observer docked at structure in sys 30_000_142 -> NO notification
    // 91_005: online observer in space in OTHER sys 30_000_144 -> NO notification
    // 91_006: OFFLINE character in space sys 30_000_142 -> NO notification
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.insert(91_001, mk_char(91_001, 30_000_142, 210_001));
        snap.characters.insert(91_002, mk_char(91_002, 30_000_142, 210_002));
        let mut docked_st = mk_char(91_003, 30_000_142, 210_003);
        docked_st.station_id = Some(60_003_760);
        snap.characters.insert(91_003, docked_st);
        let mut docked_str = mk_char(91_004, 30_000_142, 210_004);
        docked_str.structure_id = Some(1_000_000_000_111);
        snap.characters.insert(91_004, docked_str);
        snap.characters.insert(91_005, mk_char(91_005, 30_000_144, 210_005));
        snap.characters.insert(91_006, mk_char(91_006, 30_000_142, 210_006));
    });
    for (sess_idx, cid) in [1_u64, 2, 3, 4, 5].into_iter().zip([91_001_i64, 91_002, 91_003, 91_004, 91_005]) {
        mark_online(&ctx, cid, 9_000 + sess_idx, 1_776_100_000);
    }

    // Active FX call (active = true)
    crate::services::mining_cycle::emit_mining_godma_and_special_fx(
        &ctx,
        91_001,
        210_001,
        30_000_142,
        310_001,
        Some(17482),
        Some(410_001),
        Some(18068),
        67,
        "effects.Laser",
        true,
        true,
        180_000,
    );
    // Stop FX call (active = false, no target/charge)
    crate::services::mining_cycle::emit_mining_godma_and_special_fx(
        &ctx,
        91_001,
        210_001,
        30_000_142,
        310_001,
        None,
        None,
        None,
        67,
        "effects.Laser",
        true,
        false,
        180_000,
    );

    let pilot_notes = ctx.drain_notifications_for_char(91_001);
    assert_eq!(pilot_notes.len(), 4);
    assert_eq!(pilot_notes[0].event, "OnGodmaShipEffect");
    assert_eq!(pilot_notes[0].args.len(), 11);
    assert_eq!(pilot_notes[0].args[0], eve_proto::PyRep::Integer(310_001));
    assert_eq!(pilot_notes[0].args[1], eve_proto::PyRep::Integer(67));
    assert_eq!(pilot_notes[0].args[3], eve_proto::PyRep::Integer(1));
    assert_eq!(pilot_notes[0].args[8], eve_proto::PyRep::Integer(1000));
    assert_eq!(pilot_notes[0].args[10], eve_proto::PyRep::None);

    assert_eq!(pilot_notes[1].event, "OnSpecialFX");
    assert_eq!(pilot_notes[1].args.len(), 14);
    assert_eq!(pilot_notes[1].args[0], eve_proto::PyRep::Integer(210_001));
    assert_eq!(pilot_notes[1].args[2], eve_proto::PyRep::Integer(17482));
    assert_eq!(pilot_notes[1].args[3], eve_proto::PyRep::Integer(410_001));
    assert_eq!(pilot_notes[1].args[4], eve_proto::PyRep::Integer(18068));
    assert_eq!(pilot_notes[1].args[5], eve_proto::PyRep::String("effects.Laser".into()));
    assert_eq!(pilot_notes[1].args[6], eve_proto::PyRep::Bool(true));
    assert_eq!(pilot_notes[1].args[7], eve_proto::PyRep::Integer(1));

    // Stopped call has flag=0, repeat=0, actual_stop_time=Integer(..), target=None, charge=None
    assert_eq!(pilot_notes[2].event, "OnGodmaShipEffect");
    assert_eq!(pilot_notes[2].args[3], eve_proto::PyRep::Integer(0));
    assert_eq!(pilot_notes[2].args[8], eve_proto::PyRep::Integer(0));
    assert!(matches!(pilot_notes[2].args[10], eve_proto::PyRep::Integer(_)));
    assert_eq!(pilot_notes[3].event, "OnSpecialFX");
    assert_eq!(pilot_notes[3].args[2], eve_proto::PyRep::None);
    assert_eq!(pilot_notes[3].args[3], eve_proto::PyRep::None);
    assert_eq!(pilot_notes[3].args[4], eve_proto::PyRep::None);
    assert_eq!(pilot_notes[3].args[7], eve_proto::PyRep::Integer(0));
    assert_eq!(pilot_notes[3].args[10], eve_proto::PyRep::Integer(0));

    let obs_notes = ctx.drain_notifications_for_char(91_002);
    assert_eq!(obs_notes.len(), 2);
    assert!(obs_notes.iter().all(|n| n.event == "OnSpecialFX"));
    assert!(ctx.drain_notifications_for_char(91_003).is_empty());
    assert!(ctx.drain_notifications_for_char(91_004).is_empty());
    assert!(ctx.drain_notifications_for_char(91_005).is_empty());
    assert!(ctx.drain_notifications_for_char(91_006).is_empty());
}

#[tokio::test]
async fn test_wave2_activate_guards_rollback_not_enough_energy_and_deactivate_auth() {
    let sde = sde_with_fx_crit_waste_gas_ice();
    let clock = Arc::new(VirtualServerClock::with_fixed_base(1_776_200_000));
    let db = Arc::new(WorldRepository::new_empty());
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock(Arc::clone(&clock) as Arc<dyn ServerClock>);

    setup_mining_ship(&ctx, 30_000_142, 92_001, 220_001, 320_001, 17482, 420_001, 1230, 5_000).await;
    mark_online(&ctx, 92_001, 9_201, 1_776_200_000);

    // Non-existent / wrong-ship module -> ModuleNotFound
    assert_eq!(
        activate(&ctx, 92_001, 220_001, 30_000_142, 999_999, 420_001).await.unwrap_err().code,
        "ModuleNotFound"
    );

    // Non-mining module -> NotAMiningModule
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            320_002,
            InventoryItemRecord {
                item_id: 320_002,
                type_id: 99_104,
                owner_id: 92_001,
                location_id: 220_001,
                flag: InventoryFlag::HiSlot1,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    assert_eq!(
        activate(&ctx, 92_001, 220_001, 30_000_142, 320_002, 420_001).await.unwrap_err().code,
        "NotAMiningModule"
    );

    // Incompatible asteroid target (Gas cloud for laser) -> TargetNotMinable
    ctx.db.with_mut_snapshot(|snap| {
        snap.asteroids.insert(
            420_002,
            AsteroidRecord {
                asteroid_id: 420_002,
                solar_system_id: 30_000_142,
                belt_id: 1,
                type_id: 99_202,
                quantity: 500,
                initial_quantity: 500,
                position: (1_000.0, 0.0, 0.0),
                radius: 50.0,
            },
        );
    });
    assert_eq!(
        activate(&ctx, 92_001, 220_001, 30_000_142, 320_001, 420_002).await.unwrap_err().code,
        "TargetNotMinable"
    );

    // Move ship ball out of range (50km > 15km maxRange) to fail check_requirements and verify rollback
    let _ = ctx
        .systems
        .get_or_spawn(30_000_142)
        .set_ball_position(220_001, DVec3::new(50_000.0, 0.0, 0.0))
        .await;
    assert_eq!(
        activate(&ctx, 92_001, 220_001, 30_000_142, 320_001, 420_001).await.unwrap_err().code,
        "TargetOutOfRange"
    );
    assert!(active_cycles_for_ship(&ctx, 220_001).is_empty(), "check_requirements error must roll back cycle");

    // Back in range, drain cap below 40 GJ -> CapacitorEmpty rollback + NotEnoughEnergy notification
    let _ = ctx
        .systems
        .get_or_spawn(30_000_142)
        .set_ball_position(220_001, DVec3::ZERO)
        .await;
    let _ = ctx.systems.get_or_spawn(30_000_142).set_capacitor(220_001, 5.0).await;
    assert_eq!(
        activate(&ctx, 92_001, 220_001, 30_000_142, 320_001, 420_001).await.unwrap_err().code,
        "NotEnoughCapacitor"
    );
    assert!(active_cycles_for_ship(&ctx, 220_001).is_empty(), "capacitor failure must roll back cycle");
    let cap_notes = ctx.drain_notifications_for_char(92_001);
    assert!(
        cap_notes.iter().any(|n| n.event == "OnEveMessage" || format!("{n:?}").contains("NotEnoughEnergy")),
        "expected NotEnoughEnergy notification, got {cap_notes:?}"
    );

    // Valid activation succeeds, persists runtime, rejects duplicate EffectAlreadyActive, and rejects foreign deactivate
    let _ = ctx.systems.get_or_spawn(30_000_142).set_capacitor(220_001, 100.0).await;
    activate(&ctx, 92_001, 220_001, 30_000_142, 320_001, 420_001).await.unwrap();
    assert_eq!(
        activate(&ctx, 92_001, 220_001, 30_000_142, 320_001, 420_001).await.unwrap_err().code,
        "EffectAlreadyActive"
    );
    assert_eq!(
        deactivate(&ctx, 99_999, Some(220_001), 320_001).await.unwrap().unwrap_err().code,
        "ModuleNotFound"
    );
    assert_eq!(
        deactivate(&ctx, 92_001, Some(999_999), 320_001).await.unwrap().unwrap_err().code,
        "ModuleNotFound"
    );
    assert!(deactivate(&ctx, 92_001, Some(220_001), 987_654).await.is_none());
    assert!(active_cycles_for_ship(&ctx, 220_001).contains_key(&320_001));
}

#[tokio::test]
async fn test_wave2_deterministic_crit_residue_hold_full_and_stacked_crystal_shatter() {
    let sde = sde_with_fx_crit_waste_gas_ice();
    let clock = Arc::new(VirtualServerClock::with_fixed_base(1_776_300_000));
    let db = Arc::new(WorldRepository::new_empty());
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock(Arc::clone(&clock) as Arc<dyn ServerClock>);

    setup_mining_ship(&ctx, 30_000_142, 93_001, 230_001, 330_001, 99_101, 430_001, 1230, 5_000).await;
    mark_online(&ctx, 93_001, 9_301, 1_776_300_000);

    // Load a stack of 2 volatile crystals (HP 50, volatility damage 25, chance 1.0);
    // pre-damaged at 25.0 so +25.0 hits exact boundary total == hp (50.0), shattering 1 (2 -> 1) and clearing DAMAGE.
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            330_099,
            InventoryItemRecord {
                item_id: 330_099,
                type_id: 18068,
                owner_id: 93_001,
                location_id: 230_001,
                flag: InventoryFlag::HiSlot0,
                quantity: 2,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.get_mut(&330_099).unwrap().dynamic_attributes.insert(3, 25.0);
    });

    activate(&ctx, 93_001, 230_001, 30_000_142, 330_001, 430_001).await.unwrap();
    let _ = ctx.drain_notifications_for_char(93_001);
    clock.advance_secs(60);
    let outs = process_mining_cycles(&ctx).await;
    assert_eq!(outs.len(), 1);
    let out = &outs[0];
    assert_eq!(out.units_removed, 1_000);
    assert!(out.crit);
    assert_eq!(out.bonus_crit_units, 500);
    assert_eq!(out.residue_units, 2_000);
    assert_eq!(out.units_delivered, 1_500);
    assert_eq!(out.asteroid_remaining, 2_000);
    assert!(out.crystal_shattered);
    assert_eq!(out.crystal, Some(CrystalUse { type_id: 18068, matched: true }));
    assert_eq!(out.destination_flag, InventoryFlag::SpecializedOreHold);
    assert_eq!(out.stop, None);

    // Remaining crystal stack decremented 2 -> 1 and damage attribute cleared
    ctx.db.with_snapshot(|snap| {
        let crystal = &snap.items[&330_099];
        assert_eq!(crystal.quantity, 1);
        assert_eq!(crystal.dynamic_attributes.get(&3), None);
        assert_eq!(snap.mining_ledger.len(), 1);
        assert_eq!(snap.mining_ledger[0].quantity, 1_500);
        assert_eq!(snap.mining_and_ess_state.mining_ledger_waste.get(&0), Some(&2_000));
    });

    // Verify exact OnOreMined dict fields
    let notes = ctx.drain_notifications_for_char(93_001);
    let ore_note = notes.iter().find(|n| n.event == "OnOreMined").expect("OnOreMined missing");
    if let eve_proto::PyRep::Dict(entries) = &ore_note.args[0] {
        let map: std::collections::HashMap<String, eve_proto::PyRep> = entries
            .iter()
            .filter_map(|(k, v)| match k {
                eve_proto::PyRep::String(s) => Some((s.clone(), v.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(map["oreType"], eve_proto::PyRep::Integer(1230));
        assert_eq!(map["quantity_added"], eve_proto::PyRep::Integer(1_500));
        assert_eq!(map["quantity_removed"], eve_proto::PyRep::Integer(1_000));
        assert_eq!(map["amountWasted"], eve_proto::PyRep::Integer(2_000));
        assert_eq!(map["amountCritBonus"], eve_proto::PyRep::Integer(500));
        assert_eq!(map["hasRewards"], eve_proto::PyRep::Bool(false));
        assert_eq!(map["itemID"], eve_proto::PyRep::Integer(430_001));
        assert_eq!(map["moduleItemID"], eve_proto::PyRep::Integer(330_001));
    } else {
        panic!("expected PyRep::Dict on OnOreMined");
    }

    // HoldFull branch: CargoFallback Frig (120 m3 cargo = 1200 Veldspar units) fills cleanly and stops
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.insert(93_002, mk_char(93_002, 30_000_142, 230_002));
        snap.skills.entry(93_002).or_default().insert(
            3386,
            TrainedSkillRecord { type_id: 3386, level: 5, skill_points: 256_000.0 },
        );
        snap.items.insert(
            230_002,
            InventoryItemRecord {
                item_id: 230_002,
                type_id: 99_302,
                owner_id: 93_002,
                location_id: 30_000_142,
                flag: InventoryFlag::Unlocked,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            330_002,
            InventoryItemRecord {
                item_id: 330_002,
                type_id: 99_101,
                owner_id: 93_002,
                location_id: 230_002,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(330_002, true);
        snap.asteroids.insert(
            430_002,
            AsteroidRecord {
                asteroid_id: 430_002,
                solar_system_id: 30_000_142,
                belt_id: 1,
                type_id: 1230,
                quantity: 5_000,
                initial_quantity: 5_000,
                position: (2_000.0, 0.0, 0.0),
                radius: 50.0,
            },
        );
        // Pre-fill 110 m3 of cargo so only 10 m3 (= 100 units) fit
        snap.items.insert(
            330_050,
            InventoryItemRecord {
                item_id: 330_050,
                type_id: 1230,
                owner_id: 93_002,
                location_id: 230_002,
                flag: InventoryFlag::Cargo,
                quantity: 1_100,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });
    let sys = ctx.systems.get_or_spawn(30_000_142);
    let _ = sys
        .spawn_ball_with_combat(
            Ball {
                id: 230_002,
                type_id: 99_302,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_200_000.0,
                radius: 35.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_ship_combat_state(93_002, 230_002, 99_302),
        )
        .await;
    let _ = sys
        .spawn_asteroid(
            AsteroidState {
                asteroid_id: 430_002,
                belt_id: 1,
                type_id: 1230,
                initial_units: 5_000,
                remaining_units: 5_000,
                unit_volume: 0.1,
                depleted: false,
                respawn_at_tick: None,
            },
            Ball {
                id: 430_002,
                type_id: 1230,
                position: DVec3::new(2_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 10_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
        )
        .await;
    let _ = sys
        .add_target_lock(
            230_002,
            TargetLock {
                target_id: 430_002,
                lock_time_ms: 500.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: None,
            },
        )
        .await;

    // Deactivate first ship module cleanly before advancing second ship
    assert!(deactivate(&ctx, 93_001, Some(230_001), 330_001).await.unwrap().is_ok());
    activate(&ctx, 93_002, 230_002, 30_000_142, 330_002, 430_002).await.unwrap();
    clock.advance_secs(60);
    let outs2 = process_mining_cycles(&ctx).await;
    assert_eq!(outs2.len(), 1);
    assert_eq!(outs2[0].destination_flag, InventoryFlag::Cargo);
    assert_eq!(outs2[0].units_removed, 100);
    assert_eq!(outs2[0].units_delivered, 100); // crit clamped by free_units
    assert_eq!(outs2[0].stop, Some(MiningStopReason::HoldFull));
    assert!(active_cycles_for_ship(&ctx, 230_002).is_empty());
}

#[tokio::test]
async fn test_wave2_ice_gas_holds_can_harvest_clear_emptied_sites_dbuff_and_reprocessing() {
    let sde = sde_with_fx_crit_waste_gas_ice();
    let clock = Arc::new(VirtualServerClock::with_fixed_base(1_776_400_000));
    let db = Arc::new(WorldRepository::new_empty());
    let ctx = ServerContext::with_sde_and_repository(sde.clone(), db);
    ctx.set_clock(Arc::clone(&clock) as Arc<dyn ServerClock>);

    // mining_kind_of & can_harvest matrix
    let ice_mod = &sde.types[&99_102];
    let gas_mod = &sde.types[&99_103];
    let no_mod = &sde.types[&99_104];
    let ice_target = &sde.types[&99_201];
    let gas_target = &sde.types[&99_202];
    let veld = &sde.types[&1230];
    let zero_vol_gas = mk_type(99_203, 711, 2, "Zero Gas", 0.0, 0.0, None, &[]);
    let moon_ore = mk_type(99_204, 4915, 2, "Moon Ore Group 4915", 10.0, 0.0, None, &[(182, 3386.0)]);

    assert_eq!(mining_kind_of(&sde, ice_mod), Some(MiningKind::Asteroid));
    assert_eq!(mining_kind_of(&sde, gas_mod), Some(MiningKind::Gas));
    assert_eq!(mining_kind_of(&sde, no_mod), None);

    assert!(can_harvest(ice_mod, MiningKind::Asteroid, ice_target));
    assert!(!can_harvest(ice_mod, MiningKind::Asteroid, veld));
    assert!(can_harvest(&sde.types[&17482], MiningKind::Asteroid, &moon_ore));
    assert!(can_harvest(gas_mod, MiningKind::Gas, gas_target));
    assert!(!can_harvest(gas_mod, MiningKind::Gas, &zero_vol_gas));
    assert!(!can_harvest(gas_mod, MiningKind::Gas, veld));
    assert!(!can_harvest(gas_mod, MiningKind::Gas, ice_target));

    // SpecializedIceHold and SpecializedGasHold routing via OmniHold Barge (99_301)
    let sys_id = 30_000_142;
    let char_id = 94_001;
    let ship_id = 240_001;
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.insert(char_id, mk_char(char_id, sys_id, ship_id));
        for sk in [16281_u32, 25544] {
            snap.skills.entry(char_id).or_default().insert(
                sk,
                TrainedSkillRecord { type_id: sk, level: 5, skill_points: 256_000.0 },
            );
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 99_301,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::Unlocked,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        for (mid, tid, flag) in [
            (340_001, 99_102, InventoryFlag::HiSlot0),
            (340_002, 99_103, InventoryFlag::HiSlot1),
        ] {
            snap.items.insert(
                mid,
                InventoryItemRecord {
                    item_id: mid,
                    type_id: tid,
                    owner_id: char_id,
                    location_id: ship_id,
                    flag,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.module_online_states.insert(mid, true);
        }
        snap.asteroids.insert(
            440_001,
            AsteroidRecord {
                asteroid_id: 440_001,
                solar_system_id: sys_id,
                belt_id: 1,
                type_id: 99_201,
                quantity: 50,
                initial_quantity: 50,
                position: (1_000.0, 0.0, 0.0),
                radius: 50.0,
            },
        );
        snap.asteroids.insert(
            440_002,
            AsteroidRecord {
                asteroid_id: 440_002,
                solar_system_id: sys_id,
                belt_id: 1,
                type_id: 99_202,
                quantity: 200,
                initial_quantity: 200,
                position: (1_500.0, 0.0, 0.0),
                radius: 50.0,
            },
        );
    });
    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = sys
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 99_301,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 150.0,
                inertia: 1.0,
                mass: 10_000_000.0,
                radius: 100.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_ship_combat_state(char_id, ship_id, 99_301),
        )
        .await;
    for (aid, atid, qty, vol, pos) in [
        (440_001, 99_201, 50_u64, 100.0, DVec3::new(1_000.0, 0.0, 0.0)),
        (440_002, 99_202, 200_u64, 1.0, DVec3::new(1_500.0, 0.0, 0.0)),
    ] {
        let _ = sys
            .spawn_asteroid(
                AsteroidState {
                    asteroid_id: aid,
                    belt_id: 1,
                    type_id: atid,
                    initial_units: qty,
                    remaining_units: qty,
                    unit_volume: vol,
                    depleted: false,
                    respawn_at_tick: None,
                },
                Ball {
                    id: aid,
                    type_id: atid,
                    position: pos,
                    velocity: DVec3::ZERO,
                    max_velocity: 0.0,
                    inertia: 1.0,
                    mass: 50_000.0,
                    radius: 50.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
            )
            .await;
        let _ = sys
            .add_target_lock(
                ship_id,
                TargetLock {
                    target_id: aid,
                    lock_time_ms: 500.0,
                    remaining_lock_ms: 0.0,
                    locked: true,
                    max_range_m: None,
                },
            )
            .await;
    }
    activate(&ctx, char_id, ship_id, sys_id, 340_001, 440_001).await.unwrap();
    activate(&ctx, char_id, ship_id, sys_id, 340_002, 440_002).await.unwrap();
    clock.advance_secs(60);
    let mixed_outs = process_mining_cycles(&ctx).await;
    let ice_out = mixed_outs.iter().find(|o| o.module_id == 340_001).unwrap();
    let gas_out = mixed_outs.iter().find(|o| o.module_id == 340_002).unwrap();
    assert_eq!(ice_out.destination_flag, InventoryFlag::SpecializedIceHold);
    assert_eq!(gas_out.destination_flag, InventoryFlag::SpecializedGasHold);
    assert!(deactivate(&ctx, char_id, Some(ship_id), 340_001).await.unwrap().is_ok());
    assert!(deactivate(&ctx, char_id, Some(ship_id), 340_002).await.unwrap().is_ok());

    // clear_emptied_sites 50 km boundary + signature collapse
    ctx.db.with_mut_snapshot(|snap| {
        snap.asteroids.clear();
        snap.asteroids.insert(
            440_100,
            AsteroidRecord {
                asteroid_id: 440_100,
                solar_system_id: sys_id,
                belt_id: 1,
                type_id: 1230,
                quantity: 100,
                initial_quantity: 100,
                position: (60_000.0, 0.0, 0.0), // >50km from (0,0,0), <=50km from (20_000,0,0)
                radius: 50.0,
            },
        );
        snap.cosmic_anomalies.insert(
            sys_id,
            vec![
                eve_db::CosmicAnomalyRecord {
                    anomaly_id: "ANOM-EMPTY".into(),
                    solar_system_id: sys_id,
                    name: "Empty Ore Site".into(),
                    anomaly_type: "Ore".into(),
                    position: [0.0, 0.0, 0.0],
                    signal_strength: 1.0,
                    ore_types: vec![1230],
                    remaining_volume: 100.0,
                    created_unix: 1,
                    lifetime_seconds: 3600,
                    cleared: false,
                },
                eve_db::CosmicAnomalyRecord {
                    anomaly_id: "ANOM-LIVE".into(),
                    solar_system_id: sys_id,
                    name: "Live Ore Site".into(),
                    anomaly_type: "Ore".into(),
                    position: [20_000.0, 0.0, 0.0],
                    signal_strength: 1.0,
                    ore_types: vec![1230],
                    remaining_volume: 100.0,
                    created_unix: 1,
                    lifetime_seconds: 3600,
                    cleared: false,
                },
            ],
        );
        snap.cosmic_signatures.insert(
            sys_id,
            vec![
                eve_db::CosmicSignatureRecord {
                    signature_id: "ANOM-EMPTY".into(),
                    solar_system_id: sys_id,
                    sig_group: "Ore".into(),
                    type_name: "Ore Site".into(),
                    position: [0.0, 0.0, 0.0],
                    signal_strength: 1.0,
                    wormhole_type_id: None,
                    destination_system_id: None,
                    created_unix: 1,
                    lifetime_seconds: 3600,
                    max_mass_kg: 0.0,
                    remaining_mass_kg: 0.0,
                    max_jump_mass_kg: 0.0,
                    collapsed: false,
                },
                eve_db::CosmicSignatureRecord {
                    signature_id: "ANOM-LIVE".into(),
                    solar_system_id: sys_id,
                    sig_group: "Ore".into(),
                    type_name: "Ore Site".into(),
                    position: [20_000.0, 0.0, 0.0],
                    signal_strength: 0.85,
                    wormhole_type_id: None,
                    destination_system_id: None,
                    created_unix: 1,
                    lifetime_seconds: 3600,
                    max_mass_kg: 0.0,
                    remaining_mass_kg: 0.0,
                    max_jump_mass_kg: 0.0,
                    collapsed: false,
                },
            ],
        );
    });
    crate::services::mining_cycle::clear_emptied_sites(&ctx, sys_id, DVec3::new(1_000.0, 0.0, 0.0));
    ctx.db.with_snapshot(|snap| {
        let anoms = &snap.cosmic_anomalies[&sys_id];
        assert!(anoms[0].cleared);
        assert_eq!(anoms[0].remaining_volume, 0.0);
        assert!(!anoms[1].cleared);
        let sigs = &snap.cosmic_signatures[&sys_id];
        assert!(sigs[0].collapsed);
        assert_eq!(sigs[0].signal_strength, 0.0);
        assert!(!sigs[1].collapsed);
        assert_eq!(sigs[1].signal_strength, 0.85);
    });

    // dbuff_multiplier: PostPercent active applies 0.85x, expired and ModAdd are ignored, unmatched attr = 1.0
    {
        let mut st = crate::services::inventory_fitting::fitting_scanners_superweapons::scanners_superweapons_state()
            .write()
            .unwrap();
        st.ship_dbuffs.insert(
            (ctx.instance_id, ship_id),
            vec![
                crate::services::inventory_fitting::fitting_scanners_superweapons::ActiveDbuffEntry {
                    dbuff_id: 88_001,
                    value: -15.0,
                    expires_at_unix: clock.now_unix_secs() + 100,
                    source_module_id: 999,
                },
                crate::services::inventory_fitting::fitting_scanners_superweapons::ActiveDbuffEntry {
                    dbuff_id: 88_001,
                    value: -50.0,
                    expires_at_unix: clock.now_unix_secs() - 1,
                    source_module_id: 998,
                },
                crate::services::inventory_fitting::fitting_scanners_superweapons::ActiveDbuffEntry {
                    dbuff_id: 88_002,
                    value: -20.0,
                    expires_at_unix: clock.now_unix_secs() + 100,
                    source_module_id: 997,
                },
            ],
        );
    }
    let strip_miner = &sde.types[&17482];
    assert!(
        (crate::services::mining_cycle::dbuff_multiplier(&ctx, ship_id, strip_miner, 73) - 0.85).abs() < 1e-9
    );
    assert!(
        (crate::services::mining_cycle::dbuff_multiplier(&ctx, ship_id, strip_miner, 77) - 1.0).abs() < 1e-9
    );

    // compute_reprocessing_rates_for_item_at: NPC station standing tax + ore vs scrap rates
    let station = sde
        .solar_systems
        .values()
        .flat_map(|s| s.stations.iter())
        .next()
        .expect("canonical sde station");
    let npc_corp = station.owner_corporation_id as i64;
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = mk_char(94_002, sys_id, 240_002);
        ch.corporation_id = 98_444_001;
        ch.standings.insert(npc_corp, 4.0);
        snap.characters.insert(94_002, ch);
        let mut corp_peer = mk_char(94_003, sys_id, 240_003);
        corp_peer.corporation_id = 98_444_001;
        corp_peer.standings.insert(npc_corp, 6.0);
        snap.characters.insert(94_003, corp_peer);
        for sk in [3385_u32, 3389] {
            snap.skills.entry(94_002).or_default().insert(
                sk,
                TrainedSkillRecord { type_id: sk, level: 5, skill_points: 256_000.0 },
            );
        }
        snap.skills.entry(94_002).or_default().insert(
            12196,
            TrainedSkillRecord { type_id: 12196, level: 5, skill_points: 256_000.0 },
        );
    });
    let scrap_item = mk_type(99_401, 52, 7, "Scrap Module", 5.0, 0.0, None, &[(790, 12196.0)]);
    let (base_ore, yield_ore, tax_ore) = ctx.db.with_snapshot(|snap| {
        crate::services::mining_cycle::compute_reprocessing_rates_for_item_at(
            &sde,
            snap,
            94_002,
            station.station_id,
            veld,
            None,
        )
    });
    let expected_base = station.reprocessing_efficiency.clamp(0.30, 0.50);
    assert!((base_ore - expected_base).abs() < 1e-9);
    assert!(yield_ore > base_ore);
    // Corp average standing = (4.0 + 6.0)/2 = 5.0 -> tax = 0.05 - 0.0075 * 5.0 = 0.0125
    assert!((tax_ore - 0.0125).abs() < 1e-9, "got tax {tax_ore}");

    let (_, scrap_yield, _) = ctx.db.with_snapshot(|snap| {
        crate::services::mining_cycle::compute_reprocessing_rates_for_item_at(
            &sde,
            snap,
            94_002,
            station.station_id,
            &scrap_item,
            None,
        )
    });
    assert!((scrap_yield - (expected_base * 1.10).clamp(0.0, 0.55)).abs() < 1e-9);
}

#[tokio::test]
async fn test_wave3_mining_mutants_dbuff_excluding_crystal_boundaries_residue_and_tick_edges() {
    let mut sde = (*sde_with_fx_crit_waste_gas_ice()).clone();
    // 1. Setup custom dbuffs for dbuff_multiplier_excluding & module_attr multiplication check
    sde.dbuff_collections.insert(
        88_101,
        eve_sde::DbuffCollectionInfo {
            dbuff_id: 88_101,
            name: "excludedDuration".into(),
            operation: "PostPercent".into(),
            aggregate_mode: "Minimum".into(),
            display_name_id: None,
            show_output_value_in_ui: None,
            item_modifiers: Vec::new(),
            location_modifiers: Vec::new(),
            location_group_modifiers: Vec::new(),
            location_required_skill_modifiers: vec![eve_sde::DbuffSkillModifier {
                attribute_id: 73,
                skill_id: 3386,
            }],
            target_attribute_ids: Vec::new(),
        },
    );
    sde.dbuff_collections.insert(
        88_102,
        eve_sde::DbuffCollectionInfo {
            dbuff_id: 88_102,
            name: "appliedSkillDuration".into(),
            operation: "PostPercent".into(),
            aggregate_mode: "Minimum".into(),
            display_name_id: None,
            show_output_value_in_ui: None,
            item_modifiers: Vec::new(),
            location_modifiers: Vec::new(),
            location_group_modifiers: Vec::new(),
            location_required_skill_modifiers: vec![eve_sde::DbuffSkillModifier {
                attribute_id: 73,
                skill_id: 3386,
            }],
            target_attribute_ids: Vec::new(),
        },
    );
    sde.dbuff_collections.insert(
        88_103,
        eve_sde::DbuffCollectionInfo {
            dbuff_id: 88_103,
            name: "appliedLocationModifier".into(),
            operation: "PostPercent".into(),
            aggregate_mode: "Maximum".into(),
            display_name_id: None,
            show_output_value_in_ui: None,
            item_modifiers: Vec::new(),
            location_modifiers: vec![73, 555],
            location_group_modifiers: Vec::new(),
            location_required_skill_modifiers: Vec::new(),
            target_attribute_ids: Vec::new(),
        },
    );
    sde.dbuff_collections.insert(
        88_104,
        eve_sde::DbuffCollectionInfo {
            dbuff_id: 88_104,
            name: "ignoredNonPostPercent".into(),
            operation: "PostMul".into(),
            aggregate_mode: "Maximum".into(),
            display_name_id: None,
            show_output_value_in_ui: None,
            item_modifiers: Vec::new(),
            location_modifiers: vec![73],
            location_group_modifiers: Vec::new(),
            location_required_skill_modifiers: Vec::new(),
            target_attribute_ids: Vec::new(),
        },
    );

    // Custom effects for resolve_mining_effect_id_and_fx (empty guid & name-lookup path)
    sde.effects.insert(
        64_801,
        DogmaEffectType {
            effect_id: 64_801,
            name: "customEmptyGuidMining".into(),
            effect_category: 1,
            is_offensive: false,
            is_assistance: false,
            is_warp_safe: false,
            disallow_auto_repeat: false,
            electronic_chance: false,
            propulsion_chance: false,
            range_chance: false,
            guid: Some(String::new()),
            published: true,
            modifiers: Vec::new(),
        },
    );
    sde.effects.insert(
        64_802,
        DogmaEffectType {
            effect_id: 64_802,
            name: "customNamedFx".into(),
            effect_category: 1,
            is_offensive: false,
            is_assistance: false,
            is_warp_safe: false,
            disallow_auto_repeat: false,
            electronic_chance: false,
            propulsion_chance: false,
            range_chance: false,
            guid: Some("effects.CustomResolvedFX".into()),
            published: true,
            modifiers: Vec::new(),
        },
    );

    // Custom crystals:
    //  99_501: vol_chance=1.0, crystalsGetDamaged=0.0 -> MUST NOT damage or shatter!
    //  99_502: vol_chance=0.0, crystalsGetDamaged=1.0 -> MUST NOT damage or shatter!
    //  99_503: vol_chance=1.0, crystalsGetDamaged=1.0, vol_dmg=40.0, hp=100.0 -> cycle 1 damages to 40 & version += 1; pre-set 60 hits exact 100.0 == hp shatter!
    sde.types.insert(
        99_501,
        mk_type(
            99_501,
            482,
            8,
            "NoDmg Crystal",
            1.0,
            0.0,
            None,
            &[(783, 1.0), (784, 40.0), (786, 0.0), (9, 100.0), (77, 1.2)],
        ),
    );
    sde.types.insert(
        99_502,
        mk_type(
            99_502,
            482,
            8,
            "ZeroVol Crystal",
            1.0,
            0.0,
            None,
            &[(783, 0.0), (784, 40.0), (786, 1.0), (9, 100.0), (77, 1.2)],
        ),
    );
    sde.types.insert(
        99_503,
        mk_type(
            99_503,
            482,
            8,
            "ExactBoundary Crystal",
            1.0,
            0.0,
            None,
            &[(783, 1.0), (784, 40.0), (786, 1.0), (9, 100.0), (77, 1.2)],
        ),
    );

    let sde = Arc::new(sde);
    let clock = Arc::new(VirtualServerClock::with_fixed_base(1_776_500_000));
    let db = Arc::new(WorldRepository::new_empty());
    let ctx = ServerContext::with_sde_and_repository(sde.clone(), db.clone());
    ctx.set_clock(Arc::clone(&clock) as Arc<dyn ServerClock>);

    // Verify resolve_mining_effect_id_and_fx empty guid, named SDE lookup, and miningClouds without kind
    assert_eq!(
        crate::services::mining_cycle::resolve_mining_effect_id_and_fx(
            &ctx,
            None,
            "customNamedFx",
            None,
        ),
        (64_802, "effects.CustomResolvedFX".to_string(), false)
    );
    assert_eq!(
        crate::services::mining_cycle::resolve_mining_effect_id_and_fx(
            &ctx,
            None,
            "customEmptyGuidMining",
            Some(MiningKind::Gas),
        ),
        (64_801, "effects.CloudMining".to_string(), false)
    );
    assert_eq!(
        crate::services::mining_cycle::resolve_mining_effect_id_and_fx(
            &ctx,
            None,
            "miningClouds",
            None,
        ),
        (2726, "effects.CloudMining".to_string(), false)
    );

    let sys_id = 30_000_142_u32;
    let char_id = 95_001_i64;
    let ship_id = 250_001_i64;
    let mod_id = 350_001_i64;
    let ast_id = 450_001_i64;
    let cry_nodmg = 350_011_i64;
    let cry_zerovol = 350_012_i64;
    let cry_exact = 350_013_i64;

    setup_mining_ship(&ctx, sys_id, char_id, ship_id, mod_id, 17482, ast_id, 1230, 100_000).await;
    mark_online(&ctx, char_id, 9_501, clock.now_unix_secs());

    // Dbuff tests on Strip Miner I (17482): 88_101 excluded (-50%), 88_102 active (-20%), 88_103 active (-10%), 88_104 ignored ("PostMul")
    {
        let mut st = crate::services::inventory_fitting::fitting_scanners_superweapons::scanners_superweapons_state()
            .write()
            .unwrap();
        st.ship_dbuffs.insert(
            (ctx.instance_id, ship_id),
            vec![
                crate::services::inventory_fitting::fitting_scanners_superweapons::ActiveDbuffEntry {
                    dbuff_id: 88_101,
                    value: -50.0,
                    expires_at_unix: clock.now_unix_secs() + 500,
                    source_module_id: 901,
                },
                crate::services::inventory_fitting::fitting_scanners_superweapons::ActiveDbuffEntry {
                    dbuff_id: 88_102,
                    value: -20.0,
                    expires_at_unix: clock.now_unix_secs() + 500,
                    source_module_id: 902,
                },
                crate::services::inventory_fitting::fitting_scanners_superweapons::ActiveDbuffEntry {
                    dbuff_id: 88_103,
                    value: -10.0,
                    expires_at_unix: clock.now_unix_secs() + 500,
                    source_module_id: 903,
                },
                crate::services::inventory_fitting::fitting_scanners_superweapons::ActiveDbuffEntry {
                    dbuff_id: 88_104,
                    value: -30.0,
                    expires_at_unix: clock.now_unix_secs() + 500,
                    source_module_id: 904,
                },
            ],
        );
    }
    let strip = &sde.types[&17482];
    let mult_excl = crate::services::mining_cycle::dbuff_multiplier_excluding(&ctx, ship_id, strip, 73, &[88_101]);
    assert!((mult_excl - (0.80 * 0.90)).abs() < 1e-9, "got mult_excl={mult_excl}");
    assert!((crate::services::mining_cycle::dbuff_multiplier(&ctx, ship_id, strip, 555) - 0.90).abs() < 1e-9);

    // Clear 88_101 & 88_103, leave ONLY 88_102 (-20% duration) and verify activate computes exact cycleTimeMs = 180_000 * 0.80 = 144_000!
    {
        let mut st = crate::services::inventory_fitting::fitting_scanners_superweapons::scanners_superweapons_state()
            .write()
            .unwrap();
        st.ship_dbuffs.get_mut(&(ctx.instance_id, ship_id)).unwrap().retain(|d| d.dbuff_id == 88_102);
    }
    ctx.invalidate_ship_fitting(ship_id);
    let act_res = activate(&ctx, char_id, ship_id, sys_id, mod_id, ast_id).await.unwrap();
    assert_eq!(act_res.dict_get("cycleTimeMs").and_then(eve_proto::PyRep::as_i64), Some(144_000));

    // process_mining_cycles timing boundary: 143s elapsed -> 0 outcomes; +1s (= 144s exact) -> 1 outcome & cycle_start_ms advances!
    clock.advance_secs(143);
    assert!(process_mining_cycles(&ctx).await.is_empty());
    clock.advance_secs(1);
    let o_exact = process_mining_cycles(&ctx).await;
    assert_eq!(o_exact.len(), 1);
    // Still does NOT fire 143s into the second cycle
    clock.advance_secs(143);
    assert!(process_mining_cycles(&ctx).await.is_empty());
    let _ = deactivate(&ctx, char_id, Some(ship_id), mod_id).await;

    // Clear dbuffs so base duration is 180s again
    {
        let mut st = crate::services::inventory_fitting::fitting_scanners_superweapons::scanners_superweapons_state()
            .write()
            .unwrap();
        st.ship_dbuffs.remove(&(ctx.instance_id, ship_id));
    }

    // Crystal tests:
    // (A) crystalsGetDamaged = 0.0, vol_chance = 1.0 -> 0 damage
    db.with_mut_snapshot(|snap| {
        snap.items.insert(
            cry_nodmg,
            InventoryItemRecord {
                item_id: cry_nodmg, type_id: 99_501, owner_id: char_id, location_id: ship_id,
                flag: InventoryFlag::HiSlot0, quantity: 1, singleton: true, version: 7, ..Default::default()
            },
        );
        snap.module_active_charges.insert(mod_id, 99_501);
    });
    ctx.invalidate_ship_fitting(ship_id);
    activate(&ctx, char_id, ship_id, sys_id, mod_id, ast_id).await.unwrap();
    clock.advance_secs(180);
    let out_nodmg = process_mining_cycles(&ctx).await;
    assert_eq!(out_nodmg[0].crystal_damage, 0.0);
    assert!(!out_nodmg[0].crystal_shattered);
    let _ = deactivate(&ctx, char_id, Some(ship_id), mod_id).await;

    // (B) crystalsGetDamaged = 1.0, vol_chance = 0.0 -> 0 damage
    db.with_mut_snapshot(|snap| {
        snap.items.remove(&cry_nodmg);
        snap.items.insert(
            cry_zerovol,
            InventoryItemRecord {
                item_id: cry_zerovol, type_id: 99_502, owner_id: char_id, location_id: ship_id,
                flag: InventoryFlag::HiSlot0, quantity: 1, singleton: true, version: 7, ..Default::default()
            },
        );
        snap.module_active_charges.insert(mod_id, 99_502);
    });
    ctx.invalidate_ship_fitting(ship_id);
    activate(&ctx, char_id, ship_id, sys_id, mod_id, ast_id).await.unwrap();
    clock.advance_secs(180);
    let out_zvol = process_mining_cycles(&ctx).await;
    assert_eq!(out_zvol[0].crystal_damage, 0.0);
    assert!(!out_zvol[0].crystal_shattered);
    let _ = deactivate(&ctx, char_id, Some(ship_id), mod_id).await;

    // (C) Exact hp == total boundary & version += 1 check (version 7 -> 8!)
    db.with_mut_snapshot(|snap| {
        snap.items.remove(&cry_zerovol);
        snap.items.insert(
            cry_exact,
            InventoryItemRecord {
                item_id: cry_exact, type_id: 99_503, owner_id: char_id, location_id: ship_id,
                flag: InventoryFlag::HiSlot0, quantity: 1, singleton: true, version: 7, ..Default::default()
            },
        );
        snap.module_active_charges.insert(mod_id, 99_503);
    });
    ctx.invalidate_ship_fitting(ship_id);
    activate(&ctx, char_id, ship_id, sys_id, mod_id, ast_id).await.unwrap();
    clock.advance_secs(180);
    let out_hit1 = process_mining_cycles(&ctx).await;
    assert_eq!(out_hit1[0].crystal_damage, 40.0);
    assert!(!out_hit1[0].crystal_shattered);
    db.with_snapshot(|snap| {
        let cry = &snap.items[&cry_exact];
        assert_eq!(cry.version, 8, "item.version += 1 must advance 7 -> 8");
        assert_eq!(cry.dynamic_attributes.get(&3), Some(&40.0));
    });
    // Set pre-damage = 60.0 -> 60 + 40 == 100.0 == hp -> shatters on next cycle!
    db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&cry_exact).unwrap().dynamic_attributes.insert(3, 60.0);
    });
    clock.advance_secs(180);
    let out_shat = process_mining_cycles(&ctx).await;
    assert_eq!(out_shat[0].crystal_damage, 40.0);
    assert!(out_shat[0].crystal_shattered);
    db.with_snapshot(|snap| assert!(!snap.items.contains_key(&cry_exact)));
    let _ = deactivate(&ctx, char_id, Some(ship_id), mod_id).await;

    // (D) Hold completely full (delivered == 0) + waste_probability = 1.0 (residue_units > 0) still writes mining ledger!
    let full_hold_char = 95_002_i64;
    let full_hold_ship = 250_002_i64;
    let waste_mod = 350_002_i64;
    setup_mining_ship(&ctx, sys_id, full_hold_char, full_hold_ship, waste_mod, 99_101, ast_id, 1230, 50_000).await;
    db.with_mut_snapshot(|snap| {
        // Retriever has 5,000 m3 general mining hold -> fill 50,000 x 0.1 m3 Veldspar so free_units == 0!
        snap.items.insert(
            350_999,
            InventoryItemRecord {
                item_id: 350_999, type_id: 1230, owner_id: full_hold_char, location_id: full_hold_ship,
                flag: InventoryFlag::SpecializedOreHold, quantity: 50_000, singleton: false, ..Default::default()
            },
        );
    });
    activate(&ctx, full_hold_char, full_hold_ship, sys_id, waste_mod, ast_id).await.unwrap();
    clock.advance_secs(60);
    let waste_only = process_mining_cycles(&ctx).await;
    assert_eq!(waste_only.len(), 1);
    assert_eq!(waste_only[0].units_delivered, 0);
    assert!(waste_only[0].residue_units > 0);
    let ledger = ctx.db.get_mining_ledger_with_waste(full_hold_char);
    assert!(!ledger.is_empty() && ledger[0].1 == waste_only[0].residue_units);
}


