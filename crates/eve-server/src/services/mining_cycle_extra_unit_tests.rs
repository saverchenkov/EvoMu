//! Deep branch-coverage and mutation-killing unit tests for `mining_cycle` (`MN-04`, `MN-06`, `MN-07`).

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::inventory_fitting::{scanners_superweapons_state, ActiveDbuffEntry};
use crate::services::mining_cycle::*;
use eve_db::{
    AsteroidRecord, InventoryFlag, InventoryItemRecord, StandingTransactionRecord,
    TrainedSkillRecord, VirtualServerClock, WorldRepository,
};
use eve_destiny::{Ball, CombatState, MovementMode, TargetLock, WarpPhase};
use eve_sde::{
    DbuffCollectionInfo, DbuffSkillModifier, DogmaEffectType, InvType, SdeRegistry, TypeListInfo,
};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

fn make_inv_type(
    type_id: u32,
    group_id: u32,
    category_id: u32,
    name: &str,
    volume: f64,
    capacity: f64,
    default_effect_id: Option<u16>,
    default_attributes: HashMap<u16, f64>,
) -> InvType {
    InvType {
        type_id,
        group_id,
        category_id,
        name: name.into(),
        mass: 1.0,
        volume,
        capacity,
        radius: 10.0,
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
        effect_ids: default_effect_id.into_iter().chain([12, 16]).collect(),
        default_attributes,
    }
}

fn build_mining_ctx() -> ServerContext {
    let mut sde = SdeRegistry::with_canonical_seed();
    for (eid, ename) in [(101, "miningLaser"), (102, "miningClouds"), (103, "shieldBoosting")] {
        sde.effects.insert(
            eid,
            DogmaEffectType {
                effect_id: eid,
                name: ename.into(),
                effect_category: 1,
                ..Default::default()
            },
        );
    }
    for (sid, sname) in [(3386, "Mining"), (16281, "Ice Harvesting"), (11395, "Deep Core Mining")] {
        sde.types.insert(sid, make_inv_type(sid, 255, 16, sname, 0.01, 0.0, None, HashMap::new()));
    }
    // Miner I (483) with crit & waste attributes
    sde.types.insert(
        483,
        make_inv_type(
            483,
            54,
            7,
            "Miner I",
            5.0,
            0.0,
            Some(101),
            HashMap::from([
                (6, 20.0), (54, 10_000.0), (73, 60_000.0), (77, 100.0),
                (182, 3386.0), (183, 11395.0), (3153, 2.0), (3154, 100.0), (5967, 1.0), (5969, 2.0),
            ]),
        ),
    );
    // Ice Harvester I (16278) with REQUIRED_SKILL_3 = Ice Harvesting (16281)
    sde.types.insert(
        16278,
        make_inv_type(
            16278, 464, 7, "Ice Harvester I", 5.0, 0.0, Some(101),
            HashMap::from([(6, 15.0), (54, 12_000.0), (73, 30_000.0), (77, 1000.0), (182, 0.0), (184, 16281.0)]),
        ),
    );
    // Gas Cloud Harvester I (25266) & Non-mining module (9901)
    sde.types.insert(
        25266,
        make_inv_type(
            25266, 737, 7, "Gas Cloud Harvester I", 5.0, 0.0, Some(102),
            HashMap::from([(6, 10.0), (54, 1_500.0), (73, 30_000.0), (77, 20.0)]),
        ),
    );
    sde.types.insert(9901, make_inv_type(9901, 40, 7, "Shield Booster", 5.0, 0.0, Some(103), HashMap::new()));
    // Veldspar (1230), Blue Ice (16264), Golden Cytoserocin (25268), Phased Ore group 4915 (9902)
    sde.types.insert(
        1230,
        make_inv_type(
            1230, 462, 25, "Veldspar", 0.1, 0.0, None,
            HashMap::from([(182, 3386.0), (eve_sde::attr_ids::REPROCESSING_SKILL_TYPE, 12195.0)]),
        ),
    );
    sde.types.insert(
        16264,
        make_inv_type(16264, 465, 25, "Blue Ice", 1000.0, 0.0, None, HashMap::from([(182, 16281.0)])),
    );
    sde.types.insert(25268, make_inv_type(25268, 711, 4, "Golden Cytoserocin", 10.0, 0.0, None, HashMap::new()));
    sde.types.insert(
        9902,
        make_inv_type(9902, 4915, 4, "Phased Asteroid", 1.0, 0.0, None, HashMap::from([(182, 11395.0)])),
    );
    // Mining crystal (18068 = Veldspar Mining Crystal I)
    sde.type_lists.insert(
        7001,
        TypeListInfo {
            type_list_id: 7001,
            name: "VeldsparCrystals".into(),
            included_category_ids: Vec::new(),
            included_group_ids: Vec::new(),
            included_type_ids: vec![1230],
            excluded_category_ids: Vec::new(),
            excluded_group_ids: Vec::new(),
            excluded_type_ids: Vec::new(),
        },
    );
    sde.types.insert(
        18068,
        make_inv_type(
            18068, 482, 8, "Veldspar Mining Crystal I", 1.0, 0.0, None,
            HashMap::from([(9, 1.0), (783, 1.0), (784, 0.6), (786, 1.0), (3148, 7001.0)]),
        ),
    );
    // Ships: Venture (32880) & RookieShip (32883, cargo only 400)
    sde.types.insert(
        32880,
        make_inv_type(
            32880, 1283, 6, "Venture", 2900.0, 50.0, None,
            HashMap::from([(38, 50.0), (1556, 5_000.0), (14, 3.0), (482, 250.0)]),
        ),
    );
    sde.types.insert(
        32883,
        make_inv_type(
            32883, 25, 6, "RookieShip", 2000.0, 10.0, None,
            HashMap::from([(38, 10.0), (14, 3.0), (482, 100.0)]),
        ),
    );
    // Dbuff collections for command bursts
    sde.dbuff_collections.insert(
        801,
        DbuffCollectionInfo {
            dbuff_id: 801,
            name: "MiningBurstDuration".into(),
            operation: "PostPercent".into(),
            aggregate_mode: "Minimum".into(),
            display_name_id: None,
            show_output_value_in_ui: None,
            item_modifiers: Vec::new(),
            location_modifiers: Vec::new(),
            location_group_modifiers: Vec::new(),
            location_required_skill_modifiers: vec![DbuffSkillModifier { attribute_id: 73, skill_id: 3386 }],
            target_attribute_ids: Vec::new(),
        },
    );
    sde.dbuff_collections.insert(
        802,
        DbuffCollectionInfo {
            dbuff_id: 802,
            name: "MiningBurstRange".into(),
            operation: "PostPercent".into(),
            aggregate_mode: "Maximum".into(),
            display_name_id: None,
            show_output_value_in_ui: None,
            item_modifiers: Vec::new(),
            location_modifiers: vec![54],
            location_group_modifiers: Vec::new(),
            location_required_skill_modifiers: Vec::new(),
            target_attribute_ids: Vec::new(),
        },
    );
    sde.dbuff_collections.insert(
        803,
        DbuffCollectionInfo {
            dbuff_id: 803,
            name: "MiningBurstModAdd".into(),
            operation: "ModAdd".into(),
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
    // Implant (9905) and Scrapmetal item (9906)
    sde.types.insert(
        9905,
        make_inv_type(9905, 300, 20, "Beancounter", 1.0, 0.0, None, HashMap::from([(379, 4.0)])),
    );
    sde.types.insert(
        9906,
        make_inv_type(
            9906, 40, 7, "Scrap Module", 5.0, 0.0, None,
            HashMap::from([(eve_sde::attr_ids::REPROCESSING_SKILL_TYPE, 12196.0)]),
        ),
    );

    let db = Arc::new(WorldRepository::with_demo_seed());
    db.with_mut_snapshot(|snap| {
        let sk = snap.skills.entry(90_000_001).or_default();
        sk.insert(3386, TrainedSkillRecord { type_id: 3386, level: 5, skill_points: 256_000.0 });
        sk.insert(11395, TrainedSkillRecord { type_id: 11395, level: 5, skill_points: 256_000.0 });
    });
    let ctx = ServerContext::with_sde_and_repository(Arc::new(sde), db)
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    ctx
}

#[test]
fn can_harvest_mining_kind_and_dbuff_multiplier_edge_cases() {
    let ctx = build_mining_ctx();
    let miner = ctx.sde.types.get(&483).unwrap();
    let ice_harv = ctx.sde.types.get(&16278).unwrap();
    let gas_harv = ctx.sde.types.get(&25266).unwrap();
    let non_miner = ctx.sde.types.get(&9901).unwrap();
    let veld = ctx.sde.types.get(&1230).unwrap();
    let ice = ctx.sde.types.get(&16264).unwrap();
    let gas = ctx.sde.types.get(&25268).unwrap();
    let phased = ctx.sde.types.get(&9902).unwrap();

    assert_eq!(mining_kind_of(&ctx.sde, non_miner), None);
    assert!(can_harvest(miner, MiningKind::Asteroid, phased));
    assert!(can_harvest(ice_harv, MiningKind::Asteroid, ice));
    assert!(!can_harvest(ice_harv, MiningKind::Asteroid, veld));
    let mut zero_vol_gas = gas.clone();
    zero_vol_gas.volume = 0.0;
    assert!(!can_harvest(gas_harv, MiningKind::Gas, &zero_vol_gas));
    let mut skilled_gas = gas.clone();
    skilled_gas.default_attributes.insert(182, 3386.0);
    assert!(!can_harvest(gas_harv, MiningKind::Gas, &skilled_gas));

    let ship_id = 810_001_i64;
    let now = ctx.now_unix_secs();
    assert!((dbuff_multiplier(&ctx, ship_id, miner, 73) - 1.0).abs() < 1e-9);
    scanners_superweapons_state().write().unwrap().ship_dbuffs.insert(
        (ctx.instance_id, ship_id),
        vec![
            ActiveDbuffEntry { dbuff_id: 801, value: -20.0, expires_at_unix: now + 300, source_module_id: 1 },
            ActiveDbuffEntry { dbuff_id: 802, value: 50.0, expires_at_unix: now + 300, source_module_id: 2 },
            ActiveDbuffEntry { dbuff_id: 803, value: 99.0, expires_at_unix: now + 300, source_module_id: 3 },
        ],
    );
    assert!((dbuff_multiplier(&ctx, ship_id, miner, 73) - 0.80).abs() < 1e-9);
    assert!((dbuff_multiplier(&ctx, ship_id, miner, 54) - 1.50).abs() < 1e-9);
    assert!((dbuff_multiplier(&ctx, ship_id, gas_harv, 73) - 1.0).abs() < 1e-9);
}

#[tokio::test]
async fn activate_and_check_requirements_error_branches_and_capacitor_auto_init() {
    let ctx = build_mining_ctx();
    let char_id = 90_000_001_i64;
    let ship_id = 820_001_i64;
    let mod_id = 820_002_i64;
    let non_mine_mod_id = 820_003_i64;
    let bad_type_mod_id = 820_004_i64;
    let ast_id = 820_010_i64;
    let ice_ast_id = 820_011_i64;
    let sys_id = 30_000_142_u32;

    assert_eq!(activate(&ctx, char_id, ship_id, sys_id, mod_id, ast_id).await.unwrap_err().code, "ModuleNotFound");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = Some(60_003_760);
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id, type_id: 32880, owner_id: char_id, location_id: i64::from(sys_id),
                flag: InventoryFlag::Hangar, quantity: 1, singleton: true, ..Default::default()
            },
        );
        for (id, tid, flag) in [
            (mod_id, 483_u32, InventoryFlag::HiSlot0),
            (non_mine_mod_id, 9901_u32, InventoryFlag::MedSlot0),
            (bad_type_mod_id, 999_999_u32, InventoryFlag::HiSlot1),
        ] {
            snap.items.insert(
                id,
                InventoryItemRecord {
                    item_id: id, type_id: tid, owner_id: char_id, location_id: ship_id,
                    flag, quantity: 1, singleton: true, ..Default::default()
                },
            );
            snap.module_online_states.insert(id, true);
        }
        snap.asteroids.insert(
            ast_id,
            AsteroidRecord {
                asteroid_id: ast_id, solar_system_id: sys_id, belt_id: 0, type_id: 1230,
                quantity: 0, initial_quantity: 500, position: (25_000.0, 0.0, 0.0), radius: 100.0,
            },
        );
        snap.asteroids.insert(
            ice_ast_id,
            AsteroidRecord {
                asteroid_id: ice_ast_id, solar_system_id: sys_id, belt_id: 0, type_id: 16264,
                quantity: 50, initial_quantity: 50, position: (1_000.0, 0.0, 0.0), radius: 100.0,
            },
        );
    });

    assert_eq!(activate(&ctx, char_id, ship_id, sys_id, bad_type_mod_id, ast_id).await.unwrap_err().code, "ModuleNotFound");
    assert_eq!(activate(&ctx, char_id, ship_id, sys_id, non_mine_mod_id, ast_id).await.unwrap_err().code, "NotAMiningModule");
    assert_eq!(activate(&ctx, char_id, ship_id, sys_id, mod_id, ice_ast_id).await.unwrap_err().code, "TargetNotMinable");
    assert_eq!(activate(&ctx, char_id, ship_id, sys_id, mod_id, ast_id).await.unwrap_err().code, "NotInSpace");

    ctx.db.with_mut_snapshot(|snap| snap.characters.get_mut(&char_id).unwrap().station_id = None);
    assert_eq!(activate(&ctx, char_id, ship_id, sys_id, mod_id, ast_id).await.unwrap_err().code, "NotInSpace");

    let sys = ctx.systems.get_or_spawn(sys_id);
    let mut ball = Ball {
        id: ship_id, type_id: 32880, position: DVec3::ZERO, velocity: DVec3::ZERO, max_velocity: 200.0,
        inertia: 1.0, mass: 1e6, radius: 50.0, speed_fraction: 0.0,
        mode: MovementMode::Warp {
            origin: DVec3::ZERO,
            destination: DVec3::new(1e8, 0.0, 0.0),
            warp_speed_au_s: 3.0,
            total_distance: 1e8,
            phase: WarpPhase::Cruising,
            elapsed_warp_time: 1.0,
        },
    };
    let _ = sys.spawn_ball(ball.clone()).await;
    assert_eq!(activate(&ctx, char_id, ship_id, sys_id, mod_id, ast_id).await.unwrap_err().code, "CannotActivateWhileWarping");

    ball.mode = MovementMode::Stop;
    let _ = sys.spawn_ball(ball).await;
    let _ = sys.set_combat_state(ship_id, CombatState { is_cloaked: true, capacitor: 250.0, max_capacitor: 250.0, ..Default::default() }).await;
    assert_eq!(activate(&ctx, char_id, ship_id, sys_id, mod_id, ast_id).await.unwrap_err().code, "CannotActivateWhileCloaked");

    let _ = sys.set_combat_state(ship_id, CombatState { is_cloaked: false, capacitor: 250.0, max_capacitor: 250.0, ..Default::default() }).await;
    let mut fit = ctx.get_or_rebuild_ship_fitting(char_id, ship_id).unwrap();
    fit.modules.get_mut(&mod_id).unwrap().state = eve_dogma::ModuleState::Offline;
    ctx.store_ship_fitting(ship_id, fit);
    assert_eq!(activate(&ctx, char_id, ship_id, sys_id, mod_id, ast_id).await.unwrap_err().code, "ModuleOffline");

    let mut fit = ctx.get_or_rebuild_ship_fitting(char_id, ship_id).unwrap();
    fit.modules.get_mut(&mod_id).unwrap().state = eve_dogma::ModuleState::Online;
    ctx.store_ship_fitting(ship_id, fit);
    assert_eq!(activate(&ctx, char_id, ship_id, sys_id, mod_id, 999_999).await.unwrap_err().code, "TargetNotLocked");
    assert_eq!(activate(&ctx, char_id, ship_id, sys_id, mod_id, ast_id).await.unwrap_err().code, "AsteroidDepleted");

    ctx.db.with_mut_snapshot(|snap| snap.asteroids.get_mut(&ast_id).unwrap().quantity = 10_000);
    assert_eq!(activate(&ctx, char_id, ship_id, sys_id, mod_id, ast_id).await.unwrap_err().code, "TargetNotLocked");

    let _ = sys.add_target_lock(ship_id, TargetLock { target_id: ast_id, lock_time_ms: 1000.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0) }).await;
    assert_eq!(activate(&ctx, char_id, ship_id, sys_id, mod_id, ast_id).await.unwrap_err().code, "TargetOutOfRange");

    ctx.db.with_mut_snapshot(|snap| snap.asteroids.get_mut(&ast_id).unwrap().position = (10_150.0, 0.0, 0.0));
    let _ = sys.set_capacitor(ship_id, 5.0).await;
    assert_eq!(activate(&ctx, char_id, ship_id, sys_id, mod_id, ast_id).await.unwrap_err().code, "NotEnoughCapacitor");

    let _ = sys.set_capacitor(ship_id, 30.0).await;
    activate(&ctx, char_id, ship_id, sys_id, mod_id, ast_id).await.expect("exact maxRange boundary ok");
    assert_eq!(activate(&ctx, char_id, ship_id, sys_id, mod_id, ast_id).await.unwrap_err().code, "EffectAlreadyActive");
    assert!(deactivate(&ctx, char_id + 1, Some(ship_id), mod_id).await.unwrap().is_err());
    assert!(deactivate(&ctx, char_id, Some(ship_id), 999_888).await.is_none());

    ctx.mining_cycles.write().unwrap().get_mut(&mod_id).unwrap().cycle_start_ms = 0;
    let outcomes = process_mining_cycles(&ctx).await;
    assert_eq!(outcomes.len(), 1);
    assert!(outcomes[0].crit && outcomes[0].bonus_crit_units > 0 && outcomes[0].residue_units > 0);
    assert_eq!(outcomes[0].stop, Some(MiningStopReason::CapacitorEmpty));
    assert!(active_cycles_for_ship(&ctx, ship_id).is_empty());
}

#[tokio::test]
async fn crystal_matching_shattering_hold_selection_and_reprocessing_standings() {
    let ctx = build_mining_ctx();
    let char_id = 90_000_001_i64;
    let ship_id = 830_001_i64;
    let mod_id = 830_002_i64;
    let crystal_item_id = 830_003_i64;
    let ast_id = 830_010_i64;
    let sys_id = 30_000_142_u32;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id, type_id: 32883, owner_id: char_id, location_id: i64::from(sys_id),
                flag: InventoryFlag::Hangar, quantity: 1, singleton: true, ..Default::default()
            },
        );
        snap.items.insert(
            mod_id,
            InventoryItemRecord {
                item_id: mod_id, type_id: 483, owner_id: char_id, location_id: ship_id,
                flag: InventoryFlag::HiSlot0, quantity: 1, singleton: true, ..Default::default()
            },
        );
        snap.items.insert(
            crystal_item_id,
            InventoryItemRecord {
                item_id: crystal_item_id, type_id: 18068, owner_id: char_id, location_id: ship_id,
                flag: InventoryFlag::HiSlot0, quantity: 2, singleton: false,
                dynamic_attributes: HashMap::from([(3, 0.5)]), ..Default::default()
            },
        );
        snap.module_online_states.insert(mod_id, true);
        snap.asteroids.insert(
            ast_id,
            AsteroidRecord {
                asteroid_id: ast_id, solar_system_id: sys_id, belt_id: 0, type_id: 1230,
                quantity: 10_000, initial_quantity: 10_000, position: (1_000.0, 0.0, 0.0), radius: 50.0,
            },
        );
    });

    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = sys.spawn_ball(Ball {
        id: ship_id, type_id: 32883, position: DVec3::ZERO, velocity: DVec3::ZERO,
        max_velocity: 200.0, inertia: 1.0, mass: 1e6, radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await;
    let _ = sys.add_target_lock(ship_id, TargetLock {
        target_id: ast_id, lock_time_ms: 1000.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(20_000.0),
    }).await;

    let mut fit = ctx.get_or_rebuild_ship_fitting(char_id, ship_id).unwrap();
    if let Some(m) = fit.modules.get_mut(&mod_id) {
        m.state = eve_dogma::ModuleState::Online;
        m.charge_type_id = Some(18068);
    }
    ctx.store_ship_fitting(ship_id, fit);

    activate(&ctx, char_id, ship_id, sys_id, mod_id, ast_id).await.expect("auto-init combat state ok");
    ctx.mining_cycles.write().unwrap().get_mut(&mod_id).unwrap().cycle_start_ms = 0;
    let outcomes = process_mining_cycles(&ctx).await;
    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0].destination_flag, InventoryFlag::Cargo);
    assert_eq!(outcomes[0].crystal, Some(CrystalUse { type_id: 18068, matched: true }));
    assert!(outcomes[0].crystal_shattered);
    assert_eq!(outcomes[0].stop, Some(MiningStopReason::HoldFull));
    ctx.db.with_snapshot(|snap| {
        let cr = snap.items.get(&crystal_item_id).unwrap();
        assert_eq!(cr.quantity, 1);
        assert!(!cr.dynamic_attributes.contains_key(&3));
    });

    let station_id = 60_003_760_i64;
    let scrap = ctx.sde.types.get(&9906).unwrap().clone();
    let veld = ctx.sde.types.get(&1230).unwrap().clone();
    ctx.db.with_mut_snapshot(|snap| {
        let sk = snap.skills.entry(char_id).or_default();
        sk.insert(12196, TrainedSkillRecord { type_id: 12196, level: 5, skill_points: 256_000.0 });
        sk.insert(3359, TrainedSkillRecord { type_id: 3359, level: 5, skill_points: 256_000.0 });
        snap.active_implants.insert(char_id, vec![9905]);
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.insert(1_000_035, 5.0);
        }
        snap.standing_transactions.entry(char_id).or_default().push(StandingTransactionRecord {
            event_id: 1, from_id: 1_000_035, to_id: char_id, modification: 0.5, event_type_id: 85,
        });
    });
    let (s_base, s_yield, s_tax) = ctx.db.with_snapshot(|snap| {
        compute_reprocessing_rates_for_item(&ctx.sde, snap, char_id, station_id, &scrap)
    });
    assert!((s_base - 0.50).abs() < 1e-6);
    assert!((s_yield - 0.55).abs() < 1e-6);
    assert!((s_tax - 0.005).abs() < 1e-6);
    let (_, ore_yield_with_implant, _) = ctx.db.with_snapshot(|snap| {
        compute_reprocessing_rates_for_item(&ctx.sde, snap, char_id, station_id, &veld)
    });
    assert!(ore_yield_with_implant > 0.50);
}
