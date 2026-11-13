use super::*;
use crate::router::ServerContext;
use eve_db::{
    AsteroidRecord, CosmicAnomalyRecord, CosmicSignatureRecord, InventoryFlag, InventoryItemRecord,
    TrainedSkillRecord, WorldRepository,
};
use eve_destiny::{Ball, CombatState, MovementMode, TargetLock};
use eve_sde::{DogmaEffectType, InvType, SdeRegistry};
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

fn make_ctx() -> ServerContext {
    let mut sde = SdeRegistry::with_canonical_seed();
    sde.effects.insert(
        101,
        DogmaEffectType {
            effect_id: 101,
            name: "miningLaser".into(),
            effect_category: 1,
            is_offensive: false,
            is_assistance: false,
            ..Default::default()
        },
    );
    sde.effects.insert(
        102,
        DogmaEffectType {
            effect_id: 102,
            name: "miningClouds".into(),
            effect_category: 1,
            is_offensive: false,
            is_assistance: false,
            ..Default::default()
        },
    );
    // Skills: 3386 = Mining, 16281 = Ice Harvesting
    sde.types.insert(
        3386,
        make_inv_type(3386, 255, 16, "Mining", 0.01, 0.0, None, HashMap::new()),
    );
    sde.types.insert(
        16281,
        make_inv_type(
            16281,
            255,
            16,
            "Ice Harvesting",
            0.01,
            0.0,
            None,
            HashMap::new(),
        ),
    );
    // Miner I (type 483)
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
                (attr::CAPACITOR_NEED, 10.0),
                (attr::MAX_RANGE, 10_000.0),
                (attr::DURATION, 60_000.0),
                (attr::MINING_AMOUNT, 60.0),
                (attr::REQUIRED_SKILL_1, 3386.0),
            ]),
        ),
    );
    // Gas Cloud Harvester I (type 25266)
    sde.types.insert(
        25266,
        make_inv_type(
            25266,
            737,
            7,
            "Gas Cloud Harvester I",
            5.0,
            0.0,
            Some(102),
            HashMap::from([
                (attr::CAPACITOR_NEED, 10.0),
                (attr::MAX_RANGE, 1_500.0),
                (attr::DURATION, 30_000.0),
                (attr::MINING_AMOUNT, 20.0),
            ]),
        ),
    );
    // Veldspar (type 1230, volume 0.1)
    sde.types.insert(
        1230,
        make_inv_type(
            1230,
            462,
            25,
            "Veldspar",
            0.1,
            0.0,
            None,
            HashMap::from([
                (attr::REQUIRED_SKILL_1, 3386.0),
                (eve_sde::attr_ids::REPROCESSING_SKILL_TYPE, 12195.0),
            ]),
        ),
    );
    // Blue Ice (type 16264, volume 1000.0)
    sde.types.insert(
        16264,
        make_inv_type(
            16264,
            465,
            25,
            "Blue Ice",
            1000.0,
            0.0,
            None,
            HashMap::from([(attr::REQUIRED_SKILL_1, 16281.0)]),
        ),
    );
    // Golden Cytoserocin (type 25268, gas, category 4, group 711)
    sde.types.insert(
        25268,
        make_inv_type(
            25268,
            711,
            4,
            "Golden Cytoserocin",
            10.0,
            0.0,
            None,
            HashMap::new(),
        ),
    );
    // Venture (type 32880) with GENERAL_MINING_HOLD = 5000.0
    sde.types.insert(
        32880,
        make_inv_type(
            32880,
            1283,
            6,
            "Venture",
            2900.0,
            50.0,
            None,
            HashMap::from([
                (attr::CAPACITY, 50.0),
                (attr::GENERAL_MINING_HOLD, 5_000.0),
                (12, 3.0),    // lowSlots
                (13, 3.0),    // medSlots
                (14, 3.0),    // hiSlots
                (482, 250.0), // capacitorCapacity
            ]),
        ),
    );

    let db = Arc::new(WorldRepository::with_demo_seed());
    db.acquire_online_lock(1, 501, Some(90_000_001), true, 1_775_400_000)
        .unwrap();
    ServerContext::with_sde_and_repository(Arc::new(sde), db)
}

#[test]
fn test_mining_stop_reasons_and_kind_and_can_harvest() {
    assert_eq!(MiningStopReason::HoldFull.code(), "MiningHoldFull");
    assert_eq!(
        MiningStopReason::AsteroidDepleted.code(),
        "AsteroidDepleted"
    );
    assert_eq!(MiningStopReason::TargetLost.code(), "TargetNotLocked");
    assert_eq!(MiningStopReason::OutOfRange.code(), "TargetOutOfRange");
    assert_eq!(
        MiningStopReason::Warping.code(),
        "CannotActivateWhileWarping"
    );
    assert_eq!(MiningStopReason::NotInSpace.code(), "NotInSpace");
    assert_eq!(
        MiningStopReason::Cloaked.code(),
        "CannotActivateWhileCloaked"
    );
    assert_eq!(
        MiningStopReason::CapacitorEmpty.code(),
        "NotEnoughCapacitor"
    );
    assert_eq!(MiningStopReason::ModuleOffline.code(), "ModuleOffline");
    assert_eq!(MiningStopReason::Deactivated.code(), "Deactivated");

    let ctx = make_ctx();
    let miner = ctx.sde.types.get(&483).unwrap();
    let gas_harv = ctx.sde.types.get(&25266).unwrap();
    let veld = ctx.sde.types.get(&1230).unwrap();
    let ice = ctx.sde.types.get(&16264).unwrap();
    let gas = ctx.sde.types.get(&25268).unwrap();

    assert_eq!(mining_kind_of(&ctx.sde, miner), Some(MiningKind::Asteroid));
    assert_eq!(mining_kind_of(&ctx.sde, gas_harv), Some(MiningKind::Gas));
    assert_eq!(mining_kind_of(&ctx.sde, veld), None);

    assert!(can_harvest(miner, MiningKind::Asteroid, veld));
    assert!(!can_harvest(miner, MiningKind::Asteroid, ice)); // Miner I lacks Ice Harvesting
    assert!(!can_harvest(miner, MiningKind::Asteroid, gas));
    assert!(can_harvest(gas_harv, MiningKind::Gas, gas));
    assert!(!can_harvest(gas_harv, MiningKind::Gas, veld));
}

#[test]
fn test_destination_hold_units_for_and_clear_emptied_sites() {
    let ctx = make_ctx();
    assert_eq!(units_for(60.0, 0.1), 600);
    assert_eq!(units_for(0.05, 0.1), 0);

    let fit = eve_dogma::ShipFittingEngine::from_sde(100, 32880, &ctx.sde);
    let veld = ctx.sde.types.get(&1230).unwrap();
    let (flag, cap) = destination_hold(&ctx, &fit, veld);
    assert_eq!(flag, InventoryFlag::SpecializedOreHold);
    assert!((cap - 5_000.0).abs() < 1e-6);

    // clear_emptied_sites marks anomaly cleared and signature collapsed when no rocks > 0 remain
    let sys_id = 30_000_142;
    ctx.db.with_mut_snapshot(|snap| {
        snap.cosmic_anomalies
            .entry(sys_id)
            .or_default()
            .push(CosmicAnomalyRecord {
                anomaly_id: "ORE-1".into(),
                solar_system_id: sys_id,
                name: "Small Deposit".into(),
                anomaly_type: "OreAnomaly".into(),
                position: [1_000.0, 0.0, 0.0],
                signal_strength: 1.0,
                ore_types: vec![1230],
                remaining_volume: 100.0,
                created_unix: 100,
                lifetime_seconds: 3600,
                cleared: false,
            });
        snap.cosmic_signatures
            .entry(sys_id)
            .or_default()
            .push(CosmicSignatureRecord {
                signature_id: "ORE-1".into(),
                solar_system_id: sys_id,
                sig_group: "Ore".into(),
                type_name: "Small Deposit".into(),
                position: [1_000.0, 0.0, 0.0],
                signal_strength: 1.0,
                wormhole_type_id: None,
                destination_system_id: None,
                created_unix: 100,
                lifetime_seconds: 3600,
                max_mass_kg: 0.0,
                remaining_mass_kg: 0.0,
                max_jump_mass_kg: 0.0,
                collapsed: false,
            });
        snap.asteroids.insert(
            901,
            AsteroidRecord {
                asteroid_id: 901,
                solar_system_id: sys_id,
                belt_id: 0,
                type_id: 1230,
                quantity: 0,
                initial_quantity: 500,
                position: (1_500.0, 0.0, 0.0),
                radius: 50.0,
            },
        );
    });
    clear_emptied_sites(&ctx, sys_id, DVec3::new(1_500.0, 0.0, 0.0));
    ctx.db.with_snapshot(|snap| {
        let a = snap.cosmic_anomalies[&sys_id]
            .iter()
            .find(|x| x.anomaly_id == "ORE-1")
            .unwrap();
        assert!(a.cleared);
        assert_eq!(a.remaining_volume, 0.0);
        let s = snap.cosmic_signatures[&sys_id]
            .iter()
            .find(|x| x.signature_id == "ORE-1")
            .unwrap();
        assert!(s.collapsed);
    });
}

#[tokio::test]
async fn test_activate_process_and_deactivate_mining_cycle() {
    let ctx = make_ctx();
    let char_id = 90_000_001;
    let ship_id = 800_001;
    let mod_id = 800_002;
    let ast_id = 800_003;
    let sys_id = 30_000_142;

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
                item_id: ship_id,
                type_id: 32880,
                owner_id: char_id,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            mod_id,
            InventoryItemRecord {
                item_id: mod_id,
                type_id: 483,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(mod_id, true);
        snap.skills.entry(char_id).or_default().insert(
            3386,
            eve_db::TrainedSkillRecord {
                type_id: 3386,
                level: 5,
                skill_points: 256_000.0,
            },
        );
        snap.asteroids.insert(
            ast_id,
            AsteroidRecord {
                asteroid_id: ast_id,
                solar_system_id: sys_id,
                belt_id: 0,
                type_id: 1230,
                quantity: 1_000,
                initial_quantity: 1_000,
                position: (2_000.0, 0.0, 0.0),
                radius: 100.0,
            },
        );
    });

    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = sys
        .spawn_ball(Ball {
            id: ast_id,
            type_id: 1230,
            position: DVec3::new(1000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1e9,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    let _ = sys
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 32880,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 200.0,
            inertia: 1.0,
            mass: 1e6,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    let _ = sys
        .set_combat_state(
            ship_id,
            CombatState {
                shield_hp: 500.0,
                max_shield_hp: 500.0,
                armor_hp: 500.0,
                max_armor_hp: 500.0,
                structure_hp: 500.0,
                max_structure_hp: 500.0,
                capacitor: 250.0,
                max_capacitor: 250.0,
                signature_radius: 40.0,
                ..Default::default()
            },
        )
        .await;
    let _ = sys
        .add_target_lock(
            ship_id,
            TargetLock {
                target_id: ast_id,
                lock_time_ms: 1000.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: Some(20_000.0),
            },
        )
        .await;

    let mut fit = ctx.get_or_rebuild_ship_fitting(char_id, ship_id).unwrap();
    if let Some(m) = fit.modules.get_mut(&mod_id) {
        m.state = eve_dogma::ModuleState::Online;
    }
    ctx.store_ship_fitting(ship_id, fit);

    // Negative check: activating on an unlocked / nonexistent target must reject cleanly
    // without leaking OnGodmaShipEffect or OnSpecialFX notifications.
    let _ = ctx.drain_notifications_for_char(char_id);
    let bad_err = activate(&ctx, char_id, ship_id, sys_id, mod_id, 999_999_999)
        .await
        .expect_err("unlocked/missing target must fail");
    assert_eq!(bad_err.code, "TargetNotLocked");
    let leaked = ctx.drain_notifications_for_char(char_id);
    assert!(
        leaked
            .iter()
            .all(|n| n.event != "OnGodmaShipEffect" && n.event != "OnSpecialFX"),
        "Rejected mining activation must not leak OnGodmaShipEffect or OnSpecialFX: {leaked:?}"
    );

    let res = activate(&ctx, char_id, ship_id, sys_id, mod_id, ast_id)
        .await
        .expect("activate should succeed");
    assert_eq!(res.dict_get("moduleID").and_then(|v| v.as_i64()), Some(mod_id));
    assert_eq!(active_cycles_for_ship(&ctx, ship_id).len(), 1);

    let start_notifs = ctx.drain_notifications_for_char(char_id);
    let gse_start = start_notifs
        .iter()
        .find(|n| n.event == "OnGodmaShipEffect")
        .expect("activate must emit OnGodmaShipEffect");
    assert_eq!(gse_start.args.len(), 11);
    assert_eq!(gse_start.args[0], PyRep::Integer(mod_id));
    assert_eq!(gse_start.args[1], PyRep::Integer(101));
    assert!(gse_start.args[2].as_i64().unwrap() > 116_444_736_000_000_000);
    assert_eq!(&gse_start.args[3..5], &[PyRep::Integer(1), PyRep::Integer(1)]);
    assert!(matches!(&gse_start.args[5], PyRep::Tuple(env) if env.get(6) == Some(&PyRep::Integer(101))));
    assert!(gse_start.args[6].as_i64().unwrap() > 116_444_736_000_000_000);
    assert_eq!(gse_start.args[7], PyRep::Integer(60_000));
    assert_eq!(gse_start.args[8], PyRep::Integer(1000));

    let sfx_start = start_notifs
        .iter()
        .find(|n| n.event == "OnSpecialFX")
        .expect("activate must emit OnSpecialFX");
    assert_eq!(sfx_start.args.len(), 14);
    assert_eq!(sfx_start.args[0], PyRep::Integer(ship_id));
    assert_eq!(sfx_start.args[1], PyRep::Integer(mod_id));
    assert_eq!(sfx_start.args[2], PyRep::Integer(483));
    assert_eq!(sfx_start.args[3], PyRep::Integer(ast_id));
    assert_eq!(sfx_start.args[5], PyRep::String("effects.Laser".into()));
    assert_eq!(&sfx_start.args[7..9], &[PyRep::Integer(1), PyRep::Integer(1)]);
    assert_eq!(sfx_start.args[9], PyRep::Real(60_000.0));
    assert_eq!(sfx_start.args[10], PyRep::Integer(1000));
    assert!(sfx_start.args[11].as_i64().unwrap() > 116_444_736_000_000_000);

    // Backdate cycle_start_ms by 60_000 ms so process_mining_cycles resolves one full cycle
    ctx.mining_cycles
        .write()
        .unwrap()
        .get_mut(&mod_id)
        .unwrap()
        .cycle_start_ms = now_ms(&ctx).saturating_sub(60_000);

    let outcomes = process_mining_cycles(&ctx).await;
    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0].units_removed, 600);
    assert_eq!(outcomes[0].units_delivered, 600);
    assert_eq!(outcomes[0].asteroid_remaining, 400);

    let cycle_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(
        cycle_notifs.iter().any(|n| n.event == "OnItemChange"),
        "process_mining_cycles must emit OnItemChange when delivering mined ore"
    );
    // Deactivate remaining running cycle
    let deact = deactivate(&ctx, char_id, Some(ship_id), mod_id)
        .await
        .expect("cycle was active")
        .expect("caller owns cycle");
    let _ = deact;
    assert!(active_cycles_for_ship(&ctx, ship_id).is_empty());

    let stop_notifs = ctx.drain_notifications_for_char(char_id);
    let gse_stop = stop_notifs
        .iter()
        .find(|n| n.event == "OnGodmaShipEffect")
        .expect("deactivate must emit OnGodmaShipEffect");
    assert_eq!(gse_stop.args[1], PyRep::Integer(101));
    assert_eq!(&gse_stop.args[3..5], &[PyRep::Integer(0), PyRep::Integer(0)]);
    assert!(gse_stop.args[10].as_i64().unwrap() > 116_444_736_000_000_000);

    let sfx_stop = stop_notifs
        .iter()
        .find(|n| n.event == "OnSpecialFX")
        .expect("deactivate must emit OnSpecialFX");
    assert_eq!(sfx_stop.args[5], PyRep::String("effects.Laser".into()));
    assert_eq!(&sfx_stop.args[7..9], &[PyRep::Integer(0), PyRep::Integer(0)]);
    assert_eq!(sfx_stop.args[10], PyRep::Integer(0));
    assert!(sfx_stop.args[11].as_i64().unwrap() > 116_444_736_000_000_000);
}

#[test]
fn test_compute_reprocessing_rates_for_item_npc_and_structure() {
    let ctx = make_ctx();
    let char_id = 90_000_001;
    let station_id = 60_003_760;
    let veld = ctx.sde.types.get(&1230).unwrap().clone();

    ctx.db.with_mut_snapshot(|snap| {
        let skills = snap.skills.entry(char_id).or_default();
        skills.insert(
            3385,
            TrainedSkillRecord {
                type_id: 3385,
                level: 5,
                skill_points: 256_000.0,
            },
        );
        skills.insert(
            3389,
            TrainedSkillRecord {
                type_id: 3389,
                level: 4,
                skill_points: 100_000.0,
            },
        );
        skills.insert(
            12195,
            TrainedSkillRecord {
                type_id: 12195,
                level: 3,
                skill_points: 50_000.0,
            },
        );
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.insert(1_000_035, 4.0);
        }
    });

    let (base, yield_rate, tax_rate) = ctx.db.with_snapshot(|snap| {
        compute_reprocessing_rates_for_item(&ctx.sde, snap, char_id, station_id, &veld)
    });
    assert!((base - 0.50).abs() < 1e-6);
    assert!(yield_rate > 0.50 && yield_rate < 0.90);
    // standing 4.0 -> 0.05 - 0.0075 * 4.0 = 0.02
    assert!((tax_rate - 0.02).abs() < 1e-6);

    // Upwell structure ID (>= 1_000_000_000_000)
    let (s_base, s_yield, s_tax) = ctx.db.with_snapshot(|snap| {
        compute_reprocessing_rates_for_item_at(
            &ctx.sde,
            snap,
            char_id,
            1_000_000_000_001,
            &veld,
            Some(1_775_400_000),
        )
    });
    assert!((s_base - 0.50).abs() < 1e-6);
    assert!(s_yield > 0.50);
    assert_eq!(s_tax, 0.0);
}

#[tokio::test]
async fn dedicated_physics_and_cluster_ticks_yield_1x_mining_not_2x() {
    let ctx = make_ctx();
    let now = 1_750_000_000_u64;
    ctx.set_clock(Arc::new(eve_db::VirtualServerClock::with_fixed_base(now)));
    ctx.dedicated_physics_ticker_active
        .store(true, std::sync::atomic::Ordering::Release);

    let char_id = 90_000_001_i64;
    let ship_id = 100_000_001_i64;
    let mod_id = 200_000_999_i64;
    let ast_id = 300_000_999_i64;
    let sys_id = 30_000_142_u32;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
            ch.station_id = None;
            ch.structure_id = None;
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 32880,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            mod_id,
            InventoryItemRecord {
                item_id: mod_id,
                type_id: 483,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.asteroids.insert(
            ast_id,
            AsteroidRecord {
                asteroid_id: ast_id,
                solar_system_id: sys_id,
                belt_id: 0,
                type_id: 1230,
                quantity: 10_000,
                initial_quantity: 10_000,
                position: (1000.0, 0.0, 0.0),
                radius: 50.0,
            },
        );
    });

    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = sys
        .spawn_ball(Ball {
            id: ast_id,
            type_id: 1230,
            position: DVec3::new(1000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1e9,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    let _ = sys
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 32880,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 200.0,
            inertia: 1.0,
            mass: 1e6,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    let _ = sys
        .set_combat_state(
            ship_id,
            CombatState {
                capacitor: 500.0,
                max_capacitor: 500.0,
                ..Default::default()
            },
        )
        .await;
    let _ = sys
        .add_target_lock(
            ship_id,
            TargetLock {
                target_id: ast_id,
                lock_time_ms: 1000.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: Some(20_000.0),
            },
        )
        .await;

    let mut fit = ctx.get_or_rebuild_ship_fitting(char_id, ship_id).unwrap();
    if let Some(m) = fit.modules.get_mut(&mod_id) {
        m.state = eve_dogma::ModuleState::Online;
    }
    ctx.store_ship_fitting(ship_id, fit);
    activate(&ctx, char_id, ship_id, sys_id, mod_id, ast_id)
        .await
        .expect("activate");

    // Step 1 second x 60 using both dedicated physics tick and background cluster tick:
    // total yield after 60s must be strictly 1x (600 units), never 2x (1200).
    for _ in 0..60 {
        ctx.clock().advance_secs(1);
        crate::sp6_lifecycle::step_active_systems_physics_tick(&ctx, 1).await;
        crate::sp6_lifecycle::tick_background_cluster(&ctx).await;
    }

    ctx.db.with_snapshot(|snap| {
        let mined: i32 = snap
            .items
            .values()
            .filter(|i| i.owner_id == char_id && i.location_id == ship_id && i.type_id == 1230)
            .map(|i| i.quantity)
            .sum();
        assert_eq!(mined, 600, "60s mining cycle must deliver strictly 1x yield (600), got {mined}");
        assert_eq!(snap.asteroids[&ast_id].quantity, 9_400);
    });
}

#[tokio::test]
async fn gh1024_mining_and_godma_effect_timestamps_stay_anchored_across_advancetime_8_days() {
    let ctx = make_ctx();
    let char_id = 90_000_001_i64;
    let ship_id = 99_102_401_i64;
    let sys_id = 30_000_142_u32;
    let mod_id = 99_102_402_i64;
    let ast_id = 99_102_403_i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 32880,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            mod_id,
            InventoryItemRecord {
                item_id: mod_id,
                type_id: 483, // Miner I
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(mod_id, true);
        snap.skills.entry(char_id).or_default().insert(
            3386,
            eve_db::TrainedSkillRecord {
                type_id: 3386,
                level: 5,
                skill_points: 256_000.0,
            },
        );
        snap.asteroids.insert(
            ast_id,
            AsteroidRecord {
                asteroid_id: ast_id,
                solar_system_id: sys_id,
                belt_id: 0,
                type_id: 1230,
                quantity: 5_000,
                initial_quantity: 5_000,
                position: (1_000.0, 0.0, 0.0),
                radius: 100.0,
            },
        );
    });

    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = sys
        .spawn_ball(Ball {
            id: ast_id,
            type_id: 1230,
            position: DVec3::new(1_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1e9,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    let _ = sys
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 32880,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 335.0,
            inertia: 1.0,
            mass: 1e6,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    let _ = sys
        .set_combat_state(
            ship_id,
            CombatState {
                shield_hp: 500.0,
                max_shield_hp: 500.0,
                armor_hp: 500.0,
                max_armor_hp: 500.0,
                structure_hp: 500.0,
                max_structure_hp: 500.0,
                capacitor: 250.0,
                max_capacitor: 250.0,
                signature_radius: 40.0,
                ..Default::default()
            },
        )
        .await;
    let _ = sys
        .add_target_lock(
            ship_id,
            TargetLock {
                target_id: ast_id,
                lock_time_ms: 1000.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: Some(20_000.0),
            },
        )
        .await;
    let mut fit = ctx.get_or_rebuild_ship_fitting(char_id, ship_id).unwrap();
    if let Some(m) = fit.modules.get_mut(&mod_id) {
        m.state = eve_dogma::ModuleState::Online;
    }
    ctx.store_ship_fitting(ship_id, fit);

    // Simulate `/advancetime 691200` (+8 days) via `advance_secs_without_sim_clock_rebase`.
    ctx.advance_secs_without_sim_clock_rebase(691_200);
    let client_sim_ft = crate::router::host_wall_blueos_filetime_ticks();
    assert!(
        ctx.now_blueos_filetime_ticks() - client_sim_ft > 7 * 86_400 * 10_000_000,
        "virtual server clock must be >7 days ahead of client blue.os.GetSimTime()"
    );

    let max_skew_ticks: i64 = 5 * 10_000_000; // 5s tolerance around host wallclock
    let day_ticks: i64 = 86_400 * 10_000_000;

    // 1. Activate and manually deactivate Miner I after +8d advance.
    let _ = ctx.drain_notifications_for_char(char_id);
    activate(&ctx, char_id, ship_id, sys_id, mod_id, ast_id)
        .await
        .expect("mining activate after +8d");
    let _ = deactivate(&ctx, char_id, Some(ship_id), mod_id)
        .await
        .expect("cycle was active")
        .expect("mining deactivate ok");
    let notifs = ctx.drain_notifications_for_char(char_id);
    let gse_count = notifs.iter().filter(|n| n.event == "OnGodmaShipEffect").count();
    assert_eq!(gse_count, 2, "expected start + stop OnGodmaShipEffect");
    for n in notifs.iter().filter(|n| n.event == "OnGodmaShipEffect") {
        for &idx in &[2usize, 6usize] {
            let ts = n.args[idx].as_i64().expect("FILETIME int");
            let delta = (ts - client_sim_ft).abs();
            assert!(
                delta < max_skew_ticks && ts - client_sim_ft < day_ticks,
                "OnGodmaShipEffect arg[{idx}] drifted from client sim clock: delta_ms={}",
                delta / 10_000
            );
        }
        if let Some(stop_ts) = n.args.get(10).and_then(PyRep::as_i64) {
            let delta = (stop_ts - client_sim_ft).abs();
            assert!(
                delta < max_skew_ticks && stop_ts - client_sim_ft < day_ticks,
                "OnGodmaShipEffect actualStopTime drifted from client sim clock: delta_ms={}",
                delta / 10_000
            );
        }
    }
    for n in notifs.iter().filter(|n| n.event == "OnSpecialFX") {
        let ts = n.args[11].as_i64().expect("OnSpecialFX startTime FILETIME");
        assert!(
            (ts - client_sim_ft).abs() < max_skew_ticks,
            "OnSpecialFX startTime drifted from client sim clock"
        );
    }

    // 2. Also verify warp/auto-deactivation hook + router godma/attr helpers + ItemGetInfo/GetAllInfo.
    ctx.wire_module_deactivation_hook();
    let hook = ctx
        .systems
        .module_deactivation_hook
        .read()
        .unwrap()
        .clone()
        .expect("deactivation hook");
    hook(
        char_id,
        &crate::system_actor::ActiveModuleCycle {
            module_id: mod_id,
            module_type_id: 483,
            ship_id,
            effect_name: "miningLaser".into(),
            duration_s: 60.0,
            ..Default::default()
        },
    );
    ctx.notify_ship_fitting_load_changes(char_id, ship_id, 10.0, 20.0, 5.0, 15.0);
    ctx.notify_ship_capacitor_change(char_id, ship_id, 200.0, 250.0, Some(220.0));
    let hook_notifs = ctx.drain_notifications_for_char(char_id);
    for n in hook_notifs.iter().filter(|n| n.event == "OnGodmaShipEffect") {
        let ts = n.args[2].as_i64().unwrap();
        let st = n.args[6].as_i64().unwrap();
        assert!((ts - client_sim_ft).abs() < max_skew_ticks);
        assert!((st - client_sim_ft).abs() < max_skew_ticks);
    }
    for n in hook_notifs.iter().filter(|n| n.event == "OnModuleAttributeChanges") {
        let PyRep::List(entries) = &n.args[0] else { panic!() };
        for e in entries {
            let PyRep::Tuple(t) = e else { panic!() };
            let ts = t[4].as_i64().expect("attr change timestamp");
            assert!((ts - client_sim_ft).abs() < max_skew_ticks);
        }
    }

    let mut sess = eve_machonet::SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        ship_id: Some(ship_id),
        solar_system_id: Some(sys_id),
        ..Default::default()
    };
    let item_info = crate::services::inventory_fitting::handle(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "ItemGetInfo",
        &[PyRep::Integer(mod_id)],
        &std::collections::HashMap::new(),
    )
    .await
    .unwrap()
    .expect("ItemGetInfo ok");
    let info_wt = item_info
        .dict_get("wallclockTime")
        .and_then(PyRep::as_i64)
        .expect("ItemGetInfo wallclockTime");
    assert!((info_wt - client_sim_ft).abs() < max_skew_ticks);
}

#[cfg(test)]
#[path = "mining_cycle_extra_unit_tests.rs"]
mod extra_unit_tests;
