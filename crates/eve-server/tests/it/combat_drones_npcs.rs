//! Integration tests for Domain 6 (`§3.6`):
//! - `C-7`: Salvagers, Wreck Size/Tech Salvage Difficulty Tiers, Tractor Beams, Complete Drone Lifecycle,
//!   Bandwidth, Control Range, Immobile Sentries, Salvage/Logistic/EWAR Drones, Gecko/Augmented/Mutated Drones, & Drone HP Loss.
//! - `C-4`: Extended NPC AI, Archetypes (Triglavians, Drifters, Sleepers, EDENCOM, Rogue Drones, Incursion Sansha,
//!   Empire Navy, Industrial Haulers, Officers, Clone Soldiers), NPC EWAR & Remote Repair, Target Switching,
//!   Standings Aggression, Multi-Wave Encounters, & 20-Minute Batched Bounty Ticks.

use eve_db::{
    CharacterRecord, InventoryFlag, InventoryItemRecord, IskCents, NeuralAttributes,
    SystemDynamicState, TrainedSkillRecord,
};
use eve_destiny::{Ball, DamageVector, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::{
    apply_damage_to_drone_in_space, compute_drone_control_range, create_encounter_wave_site,
    destroy_specialized_npc_and_queue_bounty, dispatch_combat_rpc, flush_npc_bounty_tick,
    spawn_specialized_npc_entity, step_drone_auto_aggression_and_assist, step_extended_npc_ai,
    step_sentry_gun_grid_aggression, with_combat_runtime, with_mut_combat_runtime,
};
use glam::DVec3;
use std::collections::HashMap;

fn dict_get<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match rep {
        PyRep::KeyVal(map) => map.get(key),
        PyRep::Dict(entries) => entries
            .iter()
            .find_map(|(k, v)| (k.as_str() == Some(key)).then_some(v)),
        _ => None,
    }
}

async fn setup_pilot_session(
    ctx: &ServerContext,
    user_id: u64,
    char_id: i64,
    ship_id: i64,
    ship_type_id: u32,
    solar_system_id: u32,
    pos: DVec3,
) -> SessionState {
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.insert(
            char_id,
            CharacterRecord {
                character_id: char_id,
                user_id,
                name: format!("Pilot {char_id}"),
                corporation_id: 1_000_125,
                alliance_id: None,
                war_faction_id: None,
                station_id: None,
                structure_id: None,
                solar_system_id,
                constellation_id: 20_000_020,
                region_id: 10_000_002,
                active_ship_id: ship_id,
                balance: IskCents::from_isk(100_000_000),
                plex_balance: 0,
                loyalty_points: HashMap::new(),
                standings: HashMap::new(),
                security_status: 0.0,
                clone_grade: 2,
                home_station_id: 60_003_760,
                race_id: 1,
                bloodline_id: 1,
                ancestry_id: 1,
                school_id: 1,
                neural: NeuralAttributes::default(),
                skill_points: 5_000_000.0,
                activity_status: None,
            free_skill_points: 0.0,
                last_clone_jump_unix: 0,
                jump_fatigue_minutes: 0.0,
                jump_cooldown_until_unix: 0,
                jump_fatigue_until_unix: 0,
                description: String::new(),
                title: String::new(),
                gender: 1,
                create_date_unix: 1_700_000_000,
                fleet_id: None, autopilot_waypoints: Vec::new(),
                version: 1,
                system_epoch: 0,
            },
        );
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: ship_type_id,
                owner_id: char_id,
                location_id: solar_system_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });

    let sys_handle = ctx.systems.get_or_spawn(solar_system_id);
    let mut cs = ctx.build_type_combat_state(ship_type_id);
    cs.capacitor = 5_000.0;
    cs.max_capacitor = 5_000.0;
    let _ = sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: ship_type_id,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 2.5,
                mass: 10_000_000.0,
                radius: 120.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            cs,
        )
        .await;

    let mut sess = SessionState::default();
    sess.user_id = Some(user_id);
    sess.character_id = Some(char_id);
    sess.ship_id = Some(ship_id);
    sess.solar_system_id = Some(solar_system_id);
    sess.station_id = None;
    sess
}

fn set_skill_level(ctx: &ServerContext, char_id: i64, skill_type_id: u32, level: u8) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            skill_type_id,
            TrainedSkillRecord {
                type_id: skill_type_id,
                level,
                skill_points: 256_000.0,
            },
        );
    });
}

fn add_drone_to_bay(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    drone_item_id: i64,
    drone_type_id: u32,
    dyn_attrs: HashMap<u16, f64>,
) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            drone_item_id,
            InventoryItemRecord {
                item_id: drone_item_id,
                type_id: drone_type_id,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::DroneBay,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: dyn_attrs,
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });
}

#[tokio::test]
async fn test_c7_salvager_wreck_difficulty_tiers_and_tractor_beam_physics() {
    // parity: feat:modules.salvager
    // parity: feat:combat.wreck-size-salvage-difficulty
    // parity: feat:modules.tractor-beam
    eve_testkit::attest(&[
        "feat:modules.salvager",
        "feat:combat.wreck-size-salvage-difficulty",
        "feat:modules.tractor-beam",
    ]);

    let ctx = ServerContext::new();
    let char_id = 91_000_001;
    let other_char_id = 91_000_002;
    let ship_id = 101_000_001;
    let sys_id = 30_000_142; // Jita

    let mut sess = setup_pilot_session(&ctx, 8001, char_id, ship_id, 626, sys_id, DVec3::ZERO).await;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);

    // Fit Salvager I (`25861`, `group_id == 1122`, `attr 901 == 0.0`), Salvager II (`30836`, `attr 901 == 15.0`),
    // and Small Tractor Beam I (`24348`, `group_id == 650`, `attr 54 == 20_000.0`).
    let salvager1_id = 301_001;
    let salvager2_id = 301_002;
    let tractor_id = 301_003;
    ctx.db.with_mut_snapshot(|snap| {
        for (iid, tid, flag) in [
            (salvager1_id, 25861, InventoryFlag::HiSlot0),
            (salvager2_id, 30836, InventoryFlag::HiSlot1),
            (tractor_id, 24348, InventoryFlag::HiSlot2),
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: char_id,
                    location_id: ship_id,
                    flag,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    dynamic_attributes: HashMap::new(),
                    source_mutaplasmid_id: None,
                    insured_payout_isk: 0.0,
                    is_blueprint_copy: false,
                    blueprint_runs_remaining: None,
                },
            );
            snap.module_online_states.insert(iid, true);
        }
    });
    ctx.invalidate_ship_fitting(ship_id);

    // Spawn 3 real SDE wrecks within 3 km:
    // 1. Amarr Cruiser Wreck (`26473`, `attr 901 == +20.0%` difficulty bonus)
    // 2. Amarr Elite Cruiser Wreck (`26478`, `attr 901 == -10.0%` Tech II difficulty bonus)
    // 3. Sleeper Large Advanced Wreck (`30496`, `attr 901 == -20.0%` Sleeper difficulty bonus)
    let t1_wreck_id = 401_001;
    let t2_wreck_id = 401_002;
    let sleeper_wreck_id = 401_003;
    for (wid, wtype) in [
        (t1_wreck_id, 26473u32),
        (t2_wreck_id, 26478u32),
        (sleeper_wreck_id, 30496u32),
    ] {
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                wid,
                InventoryItemRecord {
                    item_id: wid,
                    type_id: wtype,
                    owner_id: char_id,
                    location_id: sys_id as i64,
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    dynamic_attributes: HashMap::new(),
                    source_mutaplasmid_id: None,
                    insured_payout_isk: 0.0,
                    is_blueprint_copy: false,
                    blueprint_runs_remaining: None,
                },
            );
        });
        let _ = sys_handle
            .spawn_ball_with_combat(
                Ball {
                    id: wid,
                    type_id: wtype,
                    position: DVec3::new(2_000.0, 0.0, 0.0),
                    velocity: DVec3::ZERO,
                    max_velocity: 0.0,
                    inertia: 1.0,
                    mass: 10_000.0,
                    radius: 25.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                ctx.build_type_combat_state(wtype),
            )
            .await;
    }

    // With Salvaging skill 0 and Salvager I (`0%`), Sleeper Large Advanced Wreck (`-20%`) has `accessChancePct == -20.0 <= 0.0` -> cannot be salvaged!
    let sl_fail = dispatch_combat_rpc(
        &ctx,
        &mut sess,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(salvager1_id), PyRep::Integer(sleeper_wreck_id)],
        &HashMap::new(),
    )
    .await
    .expect("Salvager I on Sleeper Wreck");
    assert_eq!(dict_get(&sl_fail, "salvaged").and_then(|v| v.as_bool()), Some(false));
    assert_eq!(
        dict_get(&sl_fail, "reason").and_then(|v| v.as_str()),
        Some("InsufficientAccessChance")
    );

    // Amarr Cruiser Wreck (`+20%`) with Salvager I has `accessChancePct == +20.0%` -> succeeds and yields T1 salvage (`25588`, `25595`)
    let t1_ok = dispatch_combat_rpc(
        &ctx,
        &mut sess,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(salvager1_id), PyRep::Integer(t1_wreck_id)],
        &HashMap::new(),
    )
    .await
    .expect("Salvager I on T1 Cruiser Wreck");
    assert_eq!(dict_get(&t1_ok, "salvaged").and_then(|v| v.as_bool()), Some(true));
    let has_t1_salvage = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .any(|i| i.location_id == ship_id && i.type_id == 25588)
    });
    assert!(has_t1_salvage, "T1 wreck must yield Alloyed Tritanium Bar (25588) in ship cargo");

    // Train Salvaging (`3438`) to level 5 (`+25%`) and use Salvager II (`+15%`):
    // - On Tech II wreck (`-10%`): `accessChancePct == 30.0%` -> yields T2 salvage (`25605`)
    // - On Sleeper Large Advanced Wreck (`-20%`): `accessChancePct == 20.0%` -> yields Sleeper salvage (`30259`)
    set_skill_level(&ctx, char_id, 3438, 5);
    let t2_ok = dispatch_combat_rpc(
        &ctx,
        &mut sess,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(salvager2_id), PyRep::Integer(t2_wreck_id)],
        &HashMap::new(),
    )
    .await
    .expect("Salvager II on T2 Wreck");
    assert_eq!(dict_get(&t2_ok, "salvaged").and_then(|v| v.as_bool()), Some(true));

    let sl_ok = dispatch_combat_rpc(
        &ctx,
        &mut sess,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(salvager2_id), PyRep::Integer(sleeper_wreck_id)],
        &HashMap::new(),
    )
    .await
    .expect("Salvager II on Sleeper Wreck");
    assert_eq!(dict_get(&sl_ok, "salvaged").and_then(|v| v.as_bool()), Some(true));
    let (has_t2_salvage, has_sleeper_salvage) = ctx.db.with_snapshot(|snap| {
        let t2 = snap
            .items
            .values()
            .any(|i| i.location_id == ship_id && i.type_id == 25605);
        let sl = snap
            .items
            .values()
            .any(|i| i.location_id == ship_id && i.type_id == 30259);
        (t2, sl)
    });
    assert!(has_t2_salvage, "Tech II wreck must yield Artificial Neural Network (25605)");
    assert!(has_sleeper_salvage, "Sleeper wreck must yield Melted Nanoribbons (30259)");

    // Tractor Beam (`24348`):
    // 1. Foreign wreck owned by `other_char_id` at 12 km -> rejected (`TractorBeamOwnershipRestricted`)
    // 2. Own wreck at 12 km -> pulled at 1,000 m/s for 5s -> new distance 7,000 m, then pulled 10s -> arrives within 2,500 m (`arrivedWithinLootRange == true`)
    let tract_wreck_id = 401_004;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            tract_wreck_id,
            InventoryItemRecord {
                item_id: tract_wreck_id,
                type_id: 26473,
                owner_id: other_char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });
    let _ = sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: tract_wreck_id,
                type_id: 26473,
                position: DVec3::new(12_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 10_000.0,
                radius: 25.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(26473),
        )
        .await;

    let foreign_err = dispatch_combat_rpc(
        &ctx,
        &mut sess,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(tractor_id), PyRep::Integer(tract_wreck_id)],
        &HashMap::new(),
    )
    .await
    .expect_err("Cannot tractor another pilot's wreck");
    assert_eq!(foreign_err.code, "TractorBeamOwnershipRestricted");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(w) = snap.items.get_mut(&tract_wreck_id) {
            w.owner_id = char_id;
        }
    });
    let pull1 = dispatch_combat_rpc(
        &ctx,
        &mut sess,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(tractor_id), PyRep::Integer(tract_wreck_id)],
        &HashMap::new(),
    )
    .await
    .expect("Tractor pull 1");
    let dist1 = dict_get(&pull1, "newDistanceMeters")
        .and_then(|v| v.as_f64())
        .unwrap_or(12_000.0);
    assert!((dist1 - 7_000.0).abs() < 1e-2, "5s tractor pull at 1,000 m/s from 12km must reach 7km");

    let _pull2 = dispatch_combat_rpc(
        &ctx,
        &mut sess,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(tractor_id), PyRep::Integer(tract_wreck_id)],
        &HashMap::new(),
    )
    .await
    .expect("Tractor pull 2 (7km -> 2km)");
    let pull3 = dispatch_combat_rpc(
        &ctx,
        &mut sess,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(tractor_id), PyRep::Integer(tract_wreck_id)],
        &HashMap::new(),
    )
    .await
    .expect("Tractor pull 3 (2km -> 1.2km stop distance)");
    assert_eq!(
        dict_get(&pull3, "arrivedWithinLootRange").and_then(|v| v.as_bool()),
        Some(true)
    );
}

#[tokio::test]
async fn test_c7_drone_lifecycle_bandwidth_control_range_sentries_salvage_logi_ewar_and_gecko() {
    // parity: feat:drones.engage-target
    // parity: feat:drones.assist-guard
    // parity: feat:drones.aggressive-passive
    // parity: feat:drones.abandon-scoop
    // parity: feat:drones.reconnect-drones
    // parity: feat:drones.drone-bandwidth
    // parity: feat:drones.drone-control-range
    // parity: feat:drones.salvage-drones
    // parity: feat:drones.logistic-drones
    // parity: feat:drones.ewar-drones
    // parity: feat:drones.sentry-drones
    // parity: feat:drones.drone-hp-loss
    // parity: feat:drones.sentry-drone-scoop-distance
    // parity: feat:drones.gecko-augmented-mutated-drones
    // parity: feat:drones.gecko-subverted-bandwidth
    eve_testkit::attest(&[
        "feat:drones.engage-target",
        "feat:drones.assist-guard",
        "feat:drones.aggressive-passive",
        "feat:drones.abandon-scoop",
        "feat:drones.reconnect-drones",
        "feat:drones.drone-bandwidth",
        "feat:drones.drone-control-range",
        "feat:drones.salvage-drones",
        "feat:drones.logistic-drones",
        "feat:drones.ewar-drones",
        "feat:drones.sentry-drones",
        "feat:drones.drone-hp-loss",
        "feat:drones.sentry-drone-scoop-distance",
        "feat:drones.gecko-augmented-mutated-drones",
        "feat:drones.gecko-subverted-bandwidth",
    ]);

    let ctx = ServerContext::new();
    let char_id = 91_000_010;
    let fleetmate_char_id = 91_000_011;
    let ship_id = 101_000_010;
    let fleetmate_ship_id = 101_000_011;
    let sys_id = 30_002_537; // Amamake

    // Vexor (`626`) has 75 Mbit/s drone bandwidth (`attr 1271 == 75.0`)
    let mut sess = setup_pilot_session(&ctx, 8010, char_id, ship_id, 626, sys_id, DVec3::ZERO).await;
    let _fleet_sess = setup_pilot_session(
        &ctx,
        8011,
        fleetmate_char_id,
        fleetmate_ship_id,
        626,
        sys_id,
        DVec3::new(1_000.0, 0.0, 0.0),
    )
    .await;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);

    // Populate DroneBay with:
    // - 2x Gecko (`33681`, 50 Mbit/s omni damage)
    // - 1x 'Subverted' JVN-UC49 (`48744`, 15 Mbit/s medium omni drone)
    // - 1x 'Augmented' Ogre (`28288`, 25 Mbit/s dual Kinetic+Thermal drone)
    // - 1x Abyssal Mutated Hobgoblin (`2454` with `dynamic_attributes`: `1272 -> 5.0`, `64 -> 3.5`, `9 -> 450.0`)
    // - 1x Warden I (`23559`, 25 Mbit/s immobile sentry drone)
    // - 1x Light Armor Maintenance Bot I (`23711`, 5 Mbit/s logistic drone)
    // - 1x Berserker SW-900 (`23536`, 25 Mbit/s stasis webifier EWAR drone)
    // - 1x Salvage Drone I (`32787`, 5 Mbit/s salvage drone)
    let gecko1_id = 310_001;
    let gecko2_id = 310_002;
    let subverted_id = 310_003;
    let aug_ogre_id = 310_004;
    let mutated_hob_id = 310_005;
    let warden_id = 310_006;
    let logi_drone_id = 310_007;
    let ewar_drone_id = 310_008;
    let salvage_drone_id = 310_009;

    add_drone_to_bay(&ctx, char_id, ship_id, gecko1_id, 33681, HashMap::new());
    add_drone_to_bay(&ctx, char_id, ship_id, gecko2_id, 33681, HashMap::new());
    add_drone_to_bay(&ctx, char_id, ship_id, subverted_id, 48744, HashMap::new());
    add_drone_to_bay(&ctx, char_id, ship_id, logi_drone_id, 23711, HashMap::new());

    // 1. Bandwidth enforcement: launching 2 Geckos (100 Mbit/s > 75 Mbit/s on Vexor) must fail with `InsufficientDroneBandwidth`
    let bw_err = dispatch_combat_rpc(
        &ctx,
        &mut sess,
        "ship",
        "LaunchDrones",
        &[PyRep::List(vec![
            PyRep::Integer(gecko1_id),
            PyRep::Integer(gecko2_id),
        ])],
        &HashMap::new(),
    )
    .await
    .expect_err("2 Geckos exceed 75 Mbit/s Vexor bandwidth");
    assert_eq!(bw_err.code, "InsufficientDroneBandwidth");

    // 2. Launch 1 Gecko (50 Mbit/s) + 1 'Subverted' JVN-UC49 (20 Mbit/s) + 1 Light Armor Bot (5 Mbit/s) = 75 Mbit/s
    let launch_res = dispatch_combat_rpc(
        &ctx,
        &mut sess,
        "ship",
        "LaunchDrones",
        &[PyRep::List(vec![
            PyRep::Integer(gecko1_id),
            PyRep::Integer(subverted_id),
            PyRep::Integer(logi_drone_id),
        ])],
        &HashMap::new(),
    )
    .await
    .expect("Launch 75 Mbit/s drone flight");
    let launched_balls = match launch_res {
        PyRep::List(v) => v.iter().filter_map(|x| x.as_i64()).collect::<Vec<_>>(),
        _ => Vec::new(),
    };
    assert_eq!(launched_balls.len(), 3);
    assert!(
        sess.notifications
            .iter()
            .any(|(ev, _)| ev == "OnDroneStateChange"),
        "ship.LaunchDrones must emit OnDroneStateChange"
    );

    // 3. Drone Control Range (`feat:drones.drone-control-range`) & Engage (`feat:drones.engage-target`):
    // Spawn a hostile target at 35,000 m. Base control range is 20,000 m -> `OutOfDroneControlRange`.
    let target_ball_id = 510_001;
    let mut tcs = ctx.build_type_combat_state(24690);
    tcs.max_shield_hp = 20_000.0;
    tcs.shield_hp = 20_000.0;
    tcs.max_armor_hp = 10_000.0;
    tcs.armor_hp = 5_000.0; // damaged armor so logistic drone can also repair
    let _ = sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: target_ball_id,
                type_id: 24690,
                position: DVec3::new(35_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 150.0,
                inertia: 1.0,
                mass: 50_000_000.0,
                radius: 400.0,
                speed_fraction: 1.0,
                mode: MovementMode::Stop,
            },
            tcs,
        )
        .await;

    assert!((compute_drone_control_range(&ctx, char_id, ship_id) - 20_000.0).abs() < 1e-3);
    let range_err = dispatch_combat_rpc(
        &ctx,
        &mut sess,
        "entity",
        "CmdEngage",
        &[PyRep::Integer(target_ball_id)],
        &HashMap::new(),
    )
    .await
    .expect_err("Target at 35km exceeds 20km base drone control range");
    assert_eq!(range_err.code, "OutOfDroneControlRange");

    // Train Drone Avionics (`3437`) to level 4 (+20km) and Advanced Drone Avionics (`23618`) to level 2 (+6km) -> 46,000 m
    set_skill_level(&ctx, char_id, 3437, 4);
    set_skill_level(&ctx, char_id, 23618, 2);
    assert!((compute_drone_control_range(&ctx, char_id, ship_id) - 46_000.0).abs() < 1e-3);

    let engage_res = dispatch_combat_rpc(
        &ctx,
        &mut sess,
        "entity",
        "CmdEngage",
        &[PyRep::Integer(target_ball_id)],
        &HashMap::new(),
    )
    .await
    .expect("CmdEngage within 46km control range");
    let dmg_dealt = dict_get(&engage_res, "totalDamageDealt")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let rep_done = dict_get(&engage_res, "totalRepairedHP")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    assert!(dmg_dealt > 150.0, "Gecko + Subverted + Mutated Hobgoblin must deal damage (got {dmg_dealt})");
    assert!(rep_done > 0.0, "Light Armor Maintenance Bot I must repair damaged armor (got {rep_done})");

    // 4. Return & Orbit (`entity.CmdReturnAndOrbit`) and Recall to Bay (`entity.CmdReturnBay`)
    let orbit_cnt = dispatch_combat_rpc(
        &ctx,
        &mut sess,
        "entity",
        "CmdReturnAndOrbit",
        &[],
        &HashMap::new(),
    )
    .await
    .expect("CmdReturnAndOrbit");
    assert_eq!(orbit_cnt.as_i64(), Some(3));

    for &bid in &launched_balls {
        let _ = sys_handle
            .spawn_ball(eve_destiny::Ball {
                id: bid,
                type_id: 2464,
                position: glam::DVec3::new(500.0, 0.0, 0.0),
                velocity: glam::DVec3::ZERO,
                max_velocity: 1000.0,
                inertia: 1.0,
                mass: 1000.0,
                radius: 5.0,
                speed_fraction: 1.0,
                mode: eve_destiny::MovementMode::Stop,
            })
            .await;
    }

    let recalled = dispatch_combat_rpc(
        &ctx,
        &mut sess,
        "entity",
        "CmdReturnBay",
        &[],
        &HashMap::new(),
    )
    .await
    .expect("CmdReturnBay");
    assert_eq!(recalled, PyRep::Dict(vec![]));
    with_combat_runtime(&ctx, |rt| {
        for &bid in &launched_balls {
            assert!(!rt.active_drones_in_space.contains_key(&bid));
        }
    });

    // 5. Launch Warden I (`23559` immobile sentry), 'Augmented' Ogre (`28288`), Berserker SW-900 (`23536`), Salvage Drone I (`32787`), and Mutated Hobgoblin (`2454`)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.type_id = 645; // Dominix (125 Mbit/s bandwidth)
        }
    });
    add_drone_to_bay(&ctx, char_id, ship_id, aug_ogre_id, 28288, HashMap::new());
    let mut mut_attrs = HashMap::new();
    mut_attrs.insert(1272, 5.0);
    mut_attrs.insert(64, 3.5);
    mut_attrs.insert(9, 450.0);
    add_drone_to_bay(&ctx, char_id, ship_id, mutated_hob_id, 2454, mut_attrs);
    add_drone_to_bay(&ctx, char_id, ship_id, warden_id, 23559, HashMap::new());
    add_drone_to_bay(&ctx, char_id, ship_id, ewar_drone_id, 23536, HashMap::new());
    add_drone_to_bay(&ctx, char_id, ship_id, salvage_drone_id, 32787, HashMap::new());
    let launch2 = dispatch_combat_rpc(
        &ctx,
        &mut sess,
        "ship",
        "LaunchDrones",
        &[PyRep::List(vec![
            PyRep::Integer(warden_id),
            PyRep::Integer(aug_ogre_id),
            PyRep::Integer(ewar_drone_id),
            PyRep::Integer(salvage_drone_id),
            PyRep::Integer(mutated_hob_id),
        ])],
        &HashMap::new(),
    )
    .await
    .expect("Launch Warden + Augmented Ogre + Berserker SW-900 + Salvage Drone + Mutated Hobgoblin");
    let balls2 = match launch2 {
        PyRep::List(v) => v.iter().filter_map(|x| x.as_i64()).collect::<Vec<_>>(),
        _ => Vec::new(),
    };
    let warden_ball_id = balls2[0];

    // Engage with EWAR drone + Augmented Ogre + Warden Sentry
    let engage2 = dispatch_combat_rpc(
        &ctx,
        &mut sess,
        "entity",
        "CmdEngage",
        &[PyRep::Integer(target_ball_id)],
        &HashMap::new(),
    )
    .await
    .expect("CmdEngage with EWAR + Sentry + Augmented");
    assert_eq!(
        dict_get(&engage2, "ewarApplied").and_then(|v| v.as_bool()),
        Some(true),
        "Berserker SW-900 must apply stasis webifier EWAR"
    );

    // Command Salvage Drone I (`entity.CmdSalvage`) on a wreck
    let drone_wreck_id = 410_001;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            drone_wreck_id,
            InventoryItemRecord {
                item_id: drone_wreck_id,
                type_id: 26473,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });
    let _ = sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: drone_wreck_id,
                type_id: 26473,
                position: DVec3::new(1_500.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 10_000.0,
                radius: 25.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(26473),
        )
        .await;
    let drone_salv_res = dispatch_combat_rpc(
        &ctx,
        &mut sess,
        "entity",
        "CmdSalvage",
        &[PyRep::Integer(drone_wreck_id)],
        &HashMap::new(),
    )
    .await
    .expect("entity.CmdSalvage");
    assert_eq!(
        dict_get(&drone_salv_res, "salvaged").and_then(|v| v.as_bool()),
        Some(true)
    );

    // 6. Immobile Sentry Drone (`Warden I`) 2,500m scoop distance restriction (`feat:drones.sentry-drone-scoop-distance`):
    // Move controlling ship to 5,000 m away from `warden_ball_id` -> `entity.CmdReturnBay` must fail with `SentryDroneOutOfScoopRange`!
    let _ = sys_handle
        .set_ball_position(ship_id, DVec3::new(5_000.0, 0.0, 0.0))
        .await;
    let sentry_err = dispatch_combat_rpc(
        &ctx,
        &mut sess,
        "entity",
        "CmdReturnBay",
        &[],
        &HashMap::new(),
    )
    .await
    .expect_err("Cannot return immobile sentry drone from > 2,500m");
    assert_eq!(sentry_err.code, "SentryDroneOutOfScoopRange");
    let _ = sys_handle.set_ball_position(ship_id, DVec3::ZERO).await;

    // 7. Aggressive/Passive settings (`entity.ChangeDroneSettings`), Assist/Guard (`entity.CmdAssist`, `entity.CmdGuard`),
    //    Abandon (`entity.CmdAbandonDrone`), Reconnect (`entity.CmdReconnectToDrones`), Scoop (`ship.ScoopToDroneBay`), and Drone HP Loss (`feat:drones.drone-hp-loss`):
    let _ = dispatch_combat_rpc(
        &ctx,
        &mut sess,
        "entity",
        "ChangeDroneSettings",
        &[PyRep::Bool(true), PyRep::Bool(true)],
        &HashMap::new(),
    )
    .await
    .expect("ChangeDroneSettings");
    let _ = dispatch_combat_rpc(
        &ctx,
        &mut sess,
        "entity",
        "CmdAssist",
        &[PyRep::Integer(fleetmate_char_id)],
        &HashMap::new(),
    )
    .await
    .expect("CmdAssist");
    let auto_engaged = step_drone_auto_aggression_and_assist(
        &ctx,
        sys_id,
        char_id,
        None,
        Some((fleetmate_char_id, target_ball_id)),
        None,
    )
    .await;
    assert!(!auto_engaged.is_empty(), "Assisting drones must engage assisted fleetmate's target");

    let _ = dispatch_combat_rpc(
        &ctx,
        &mut sess,
        "entity",
        "CmdAbandonDrone",
        &[],
        &HashMap::new(),
    )
    .await
    .expect("CmdAbandonDrone");
    assert!(
        sess.notifications
            .iter()
            .any(|(ev, _)| ev == "OnDroneControlLost"),
        "CmdAbandonDrone must emit OnDroneControlLost"
    );

    let reconnected = dispatch_combat_rpc(
        &ctx,
        &mut sess,
        "entity",
        "CmdReconnectToDrones",
        &[],
        &HashMap::new(),
    )
    .await
    .expect("CmdReconnectToDrones");
    match &reconnected {
        PyRep::Dict(d) => assert!(d.is_empty(), "CmdReconnectToDrones returned errors: {d:?}"),
        other => panic!("Expected dict response from CmdReconnectToDrones, got {other:?}"),
    }
    let (reconnected_count, reconnected_states) = with_combat_runtime(&ctx, |rt| {
        let matching: Vec<_> = rt
            .active_drones_in_space
            .values()
            .filter(|d| d.owner_char_id == char_id && !d.is_abandoned)
            .collect();
        (
            matching.len(),
            matching.iter().map(|d| d.activity_state).collect::<Vec<_>>(),
        )
    });
    assert_eq!(reconnected_count, 5, "Expected all 5 abandoned drones to be reconnected");
    for st in reconnected_states {
        assert_eq!(st, 0, "Reconnected drone activity_state must be 0 (idle)");
    }

    // Scoop Warden sentry drone directly via `ship.ScoopToDroneBay`
    let scoop_res = dispatch_combat_rpc(
        &ctx,
        &mut sess,
        "ship",
        "ScoopToDroneBay",
        &[PyRep::Integer(warden_ball_id)],
        &HashMap::new(),
    )
    .await
    .expect("ScoopToDroneBay");
    assert_eq!(
        dict_get(&scoop_res, "scoopedDroneItemID").and_then(|v| v.as_i64()),
        Some(warden_id)
    );

    // Scoop another drone via `ship.ScoopDrone`
    // parity: rpc:ship.ScoopDrone
    let scoop_drone_res = dispatch_combat_rpc(
        &ctx,
        &mut sess,
        "ship",
        "ScoopDrone",
        &[PyRep::Integer(balls2[2])],
        &HashMap::new(),
    )
    .await
    .expect("ScoopDrone");
    assert!(dict_get(&scoop_drone_res, "scoopedDroneItemID").is_some());

    // Destroy one active drone in space via `apply_damage_to_drone_in_space` (`feat:drones.drone-hp-loss`)
    let victim_drone_ball = balls2[1];
    let (_dmg, destroyed) = apply_damage_to_drone_in_space(
        &ctx,
        sys_id,
        victim_drone_ball,
        DamageVector::new(10_000.0, 10_000.0, 10_000.0, 10_000.0),
    )
    .await;
    assert!(destroyed, "Lethal damage to drone in space must destroy the drone ball");
    let item_still_exists = ctx
        .db
        .with_snapshot(|snap| snap.items.contains_key(&aug_ogre_id));
    assert!(
        !item_still_exists,
        "Destroyed drone must be permanently removed from inventory"
    );
}

#[tokio::test]
async fn test_c4_npc_archetypes_triglavian_drifter_sleeper_edencom_clone_soldiers_and_ewar_rep() {
    // parity: feat:npc.drifters
    // parity: feat:npc.triglavians
    // parity: feat:npc.sleepers
    // parity: feat:npc.edencom-npc
    // parity: feat:npc.rogue-drones
    // parity: feat:npc.incursion-npc
    // parity: feat:npc.navy-npc
    // parity: feat:npc.npc-hauler
    // parity: feat:npc.npc-standings-aggression
    // parity: feat:npc.officer-npc
    // parity: feat:npc.npc-ewar-application
    // parity: feat:npc.npc-remote-repair
    // parity: feat:npc.npc-target-switching
    // parity: feat:npc.clone-soldier-spawns
    eve_testkit::attest(&[
        "feat:npc.drifters",
        "feat:npc.triglavians",
        "feat:npc.sleepers",
        "feat:npc.edencom-npc",
        "feat:npc.rogue-drones",
        "feat:npc.incursion-npc",
        "feat:npc.navy-npc",
        "feat:npc.npc-hauler",
        "feat:npc.npc-standings-aggression",
        "feat:npc.officer-npc",
        "feat:npc.npc-ewar-application",
        "feat:npc.npc-remote-repair",
        "feat:npc.npc-target-switching",
        "feat:npc.clone-soldier-spawns",
    ]);

    let ctx = ServerContext::new();
    let char_id = 91_000_020;
    let friend_char_id = 91_000_021;
    let ship_id = 101_000_020;
    let friend_ship_id = 101_000_021;
    let sys_id = 30_002_537;

    let _sess = setup_pilot_session(&ctx, 8020, char_id, ship_id, 24690, sys_id, DVec3::ZERO).await;
    let _friend_sess = setup_pilot_session(
        &ctx,
        8021,
        friend_char_id,
        friend_ship_id,
        24690,
        sys_id,
        DVec3::new(3_000.0, 0.0, 0.0),
    )
    .await;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);

    // Give `ship_id` high shield/armor/hull HP so it survives multiple NPC ticks including Drifter Doomsday
    if let Ok(Some(mut cs)) = sys_handle.get_combat_state(ship_id).await {
        cs.max_shield_hp = 100_000.0;
        cs.shield_hp = 100_000.0;
        cs.max_armor_hp = 100_000.0;
        cs.armor_hp = 100_000.0;
        cs.max_structure_hp = 100_000.0;
        cs.structure_hp = 100_000.0;
        let _ = sys_handle.set_combat_state(ship_id, cs).await;
    }

    // Give `friend_char_id` +8.0 standing toward Caldari State (`500001`) so Navy/Faction NPCs with `500001` do not auto-aggress `friend_ship_id` (`feat:npc.npc-standings-aggression`)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(fc) = snap.characters.get_mut(&friend_char_id) {
            fc.standings.insert(500_001, 8.0);
        }
    });
    let navy_npc = spawn_specialized_npc_entity(
        &ctx,
        sys_id,
        10046, // Caldari Navy Captain 2nd Rank
        "Navy",
        10,
        DVec3::new(4_000.0, 0.0, 0.0),
    )
    .await;
    with_mut_combat_runtime(&ctx, |rt| {
        if let Some(n) = rt.npc_entities.get_mut(&navy_npc) {
            n.faction_id = 500_001;
        }
    });
    step_extended_npc_ai(&ctx, sys_id).await;
    let navy_threat_has_friend = with_combat_runtime(&ctx, |rt| {
        rt.npc_entities
            .get(&navy_npc)
            .map(|n| n.aggression_threat.contains_key(&friend_ship_id))
            .unwrap_or(true)
    });
    assert!(
        !navy_threat_has_friend,
        "NPC must not auto-aggress a pilot with >= +5.0 faction standing"
    );
    let _ = sys_handle.remove_ball(navy_npc).await;
    with_mut_combat_runtime(&ctx, |rt| {
        rt.npc_entities.remove(&navy_npc);
    });

    // 1. Triglavian (`Raznaborg Damavik` `52645`) entropic spooling & target switching reset (`feat:npc.triglavians`, `feat:npc.npc-target-switching`)
    let trig_npc = spawn_specialized_npc_entity(
        &ctx,
        sys_id,
        52645,
        "Triglavian",
        20,
        DVec3::new(5_000.0, 0.0, 0.0),
    )
    .await;
    with_mut_combat_runtime(&ctx, |rt| {
        if let Some(n) = rt.npc_entities.get_mut(&trig_npc) {
            n.aggression_threat.insert(ship_id, 200.0);
        }
    });
    step_extended_npc_ai(&ctx, sys_id).await;
    step_extended_npc_ai(&ctx, sys_id).await;
    let cycles_before = with_combat_runtime(&ctx, |rt| {
        rt.npc_entities
            .get(&trig_npc)
            .map(|n| n.entropic_cycles)
            .unwrap_or(0)
    });
    assert!(cycles_before >= 2, "Triglavian NPC must spool entropic cycles across ticks");

    // Inject higher threat on `friend_ship_id` so Triglavian switches target and resets `entropic_cycles`
    with_mut_combat_runtime(&ctx, |rt| {
        if let Some(n) = rt.npc_entities.get_mut(&trig_npc) {
            n.aggression_threat.insert(friend_ship_id, 500.0);
        }
    });
    step_extended_npc_ai(&ctx, sys_id).await;
    let (switched_target, cycles_after_switch) = with_combat_runtime(&ctx, |rt| {
        let n = &rt.npc_entities[&trig_npc];
        (n.target_id, n.entropic_cycles)
    });
    assert_eq!(switched_target, Some(friend_ship_id));
    assert_eq!(
        cycles_after_switch, 1,
        "Switching target must reset Triglavian entropic spool before applying first cycle on new target"
    );

    // 2. Drifter (`Drifter Battleship` `34495`) overshield & Doomsday superweapon (`feat:npc.drifters`)
    let drifter_npc = spawn_specialized_npc_entity(
        &ctx,
        sys_id,
        34495,
        "Drifter",
        30,
        DVec3::new(6_000.0, 0.0, 0.0),
    )
    .await;
    with_mut_combat_runtime(&ctx, |rt| {
        if let Some(n) = rt.npc_entities.get_mut(&drifter_npc) {
            n.drifter_overshield_hp = 0.0; // overshield depleted -> triggers Doomsday on next AI step
        }
    });
    step_extended_npc_ai(&ctx, sys_id).await;
    let dd_fired = with_combat_runtime(&ctx, |rt| {
        rt.npc_entities
            .get(&drifter_npc)
            .map(|n| n.drifter_doomsday_fired)
            .unwrap_or(false)
    });
    assert!(dd_fired, "Drifter Battleship must fire its Doomsday once overshield is depleted");

    // 3. Sleeper (`Sleepless Patroller` `30188`) remote repair on group peer, 0 bounty, and blue loot (`30744`) (`feat:npc.sleepers`, `feat:npc.npc-remote-repair`)
    let sleeper1 = spawn_specialized_npc_entity(
        &ctx,
        sys_id,
        30188,
        "Sleeper",
        40,
        DVec3::new(7_000.0, 0.0, 0.0),
    )
    .await;
    let sleeper2 = spawn_specialized_npc_entity(
        &ctx,
        sys_id,
        30188,
        "Sleeper",
        40,
        DVec3::new(7_500.0, 0.0, 0.0),
    )
    .await;
    if let Ok(Some(mut s2_cs)) = sys_handle.get_combat_state(sleeper2).await {
        s2_cs.armor_hp = (s2_cs.max_armor_hp * 0.2).max(100.0);
        let before_armor = s2_cs.armor_hp;
        let _ = sys_handle.set_combat_state(sleeper2, s2_cs).await;
        step_extended_npc_ai(&ctx, sys_id).await;
        let after_armor = sys_handle
            .get_combat_state(sleeper2)
            .await
            .unwrap()
            .unwrap()
            .armor_hp;
        assert!(
            after_armor > before_armor,
            "Sleeper NPC must remote-repair damaged fellow NPC in the same spawn group ({after_armor} > {before_armor})"
        );
    }
    let (_sl_wreck, sl_bounty, sl_loot) =
        destroy_specialized_npc_and_queue_bounty(&ctx, sys_id, sleeper1, char_id).await;
    assert_eq!(sl_bounty, 0.0, "Sleepers pay 0 direct bounty");
    assert!(sl_loot.contains(&30744), "Sleepers must drop Sleeper Data Library blue loot (30744)");

    // 4. EDENCOM (`54335`), Rogue Drone (`3744`), Incursion Sansha (`2845`), Hauler (`42656`), Officer (`13654`), and Clone Soldiers (`33132`, `33142`, `33163`, `33175`)
    let edencom_npc = spawn_specialized_npc_entity(
        &ctx,
        sys_id,
        54335,
        "EDENCOM",
        50,
        DVec3::new(4_500.0, 0.0, 0.0),
    )
    .await;
    step_extended_npc_ai(&ctx, sys_id).await;
    let _ = destroy_specialized_npc_and_queue_bounty(&ctx, sys_id, edencom_npc, char_id).await;

    let hauler_npc = spawn_specialized_npc_entity(
        &ctx,
        sys_id,
        42656,
        "Hauler",
        51,
        DVec3::new(4_500.0, 0.0, 0.0),
    )
    .await;
    let (_hw, _hb, hauler_loot) =
        destroy_specialized_npc_and_queue_bounty(&ctx, sys_id, hauler_npc, char_id).await;
    assert!(hauler_loot.contains(&34) && hauler_loot.contains(&35), "NPC Hauler must drop mineral cache (Tritanium 34 + Pyerite 35)");

    let officer_npc = spawn_specialized_npc_entity(
        &ctx,
        sys_id,
        13654, // Cormack Vaaja
        "Officer",
        52,
        DVec3::new(4_500.0, 0.0, 0.0),
    )
    .await;
    let (_ow, officer_bounty, officer_loot) =
        destroy_specialized_npc_and_queue_bounty(&ctx, sys_id, officer_npc, char_id).await;
    assert!(officer_bounty >= 25_000_000.0, "Officer NPC must have high bounty");
    assert!(officer_loot.contains(&14254), "Officer NPC must drop officer faction module");

    // Clone Soldier Negotiator (`33175`) warp scrambles at 24 km and drops Negotiator Tag (`35664`)
    let cs_neg = spawn_specialized_npc_entity(
        &ctx,
        sys_id,
        33175,
        "CloneSoldier",
        60,
        DVec3::new(8_000.0, 0.0, 0.0),
    )
    .await;
    step_extended_npc_ai(&ctx, sys_id).await;
    let scram_str = sys_handle
        .get_combat_state(ship_id)
        .await
        .unwrap()
        .unwrap()
        .warp_scramble_strength;
    assert!(scram_str >= 2, "Clone Soldier Negotiator must apply warp scramble EWAR");
    let (_csw, _csb, cs_loot) =
        destroy_specialized_npc_and_queue_bounty(&ctx, sys_id, cs_neg, char_id).await;
    assert!(cs_loot.contains(&35664), "Clone Soldier Negotiator must drop Negotiator Tag (35664)");
}

#[tokio::test]
async fn test_c4_bounty_tick_dbs_ess_split_encounter_waves_and_sentry_guns() {
    // parity: feat:npc.belt-rats
    // parity: feat:npc.npc-ai-targeting
    // parity: feat:npc.npc-damage-types
    // parity: feat:npc.npc-loot
    // parity: feat:combat.bounty-payout-npc
    // parity: feat:npc.npc-spawn-encounter
    // parity: feat:npc.sentry-guns
    // parity: feat:npc.sentry-gun-grid-aggression
    eve_testkit::attest(&[
        "feat:npc.belt-rats",
        "feat:npc.npc-ai-targeting",
        "feat:npc.npc-damage-types",
        "feat:npc.npc-loot",
        "feat:combat.bounty-payout-npc",
        "feat:npc.npc-spawn-encounter",
        "feat:npc.sentry-guns",
        "feat:npc.sentry-gun-grid-aggression",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let char_id = 91_000_030;
    let ship_id = 101_000_030;
    let nullsec_sys_id = 30_004_759; // 1DQ1-A (K-space null-sec <= 0.0)

    let _sess = setup_pilot_session(&ctx, 8030, char_id, ship_id, 24690, nullsec_sys_id, DVec3::ZERO).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.insert(
            nullsec_sys_id,
            SystemDynamicState {
                solar_system_id: nullsec_sys_id,
                dbs_multiplier: 1.50,
                ess_main_bank: IskCents(0),
                ess_reserve_bank: IskCents(0),
                ess_linked_character_id: None,
                ess_link_started_tick: None,
                sov_alliance_id: None,
                military_index: 1,
                industrial_index: 1,
                strategic_index: 1,
                fw_contested_pct: 0.0,
                insurgency_corruption_stage: 0,
                insurgency_suppression_stage: 0,
                wormhole_remaining_mass_kg: 0.0,
                wormhole_destination_system_id: None,
                signature_position: None,
                fw_complex_position: None,
                fw_capture_seconds_elapsed: 0,
                fw_capture_duration_seconds: 0,
                last_fw_capture_tick: 0,
                last_sov_claim_tick: 0,
            },
        );
    });

    // 1. Multi-wave encounter site (`feat:npc.npc-spawn-encounter`) + 20-minute batched bounty tick (`feat:combat.bounty-payout-npc`)
    let wave1_balls = create_encounter_wave_site(
        &ctx,
        500,
        nullsec_sys_id,
        33132, // Angel Clone Soldier Trainer
        2,
        DVec3::new(5_000.0, 0.0, 0.0),
    )
    .await;
    assert_eq!(wave1_balls.len(), 2);

    // Kill wave 1 trigger ball -> spawns wave 2 automatically and queues bounty into `pending_bounty_ticks`
    let (_w1, b1, _l1) =
        destroy_specialized_npc_and_queue_bounty(&ctx, nullsec_sys_id, wave1_balls[0], char_id).await;
    assert!(b1 > 0.0);
    let current_wave = with_combat_runtime(&ctx, |rt| rt.encounter_waves[&500].current_wave);
    assert_eq!(current_wave, 2, "Killing trigger NPC must spawn Wave 2");

    // Flush the 20-minute batched bounty tick (`flush_npc_bounty_tick`) with 10% corp tax
    let (pilot_isk, main_ess, reserve_ess) =
        flush_npc_bounty_tick(&ctx, nullsec_sys_id, char_id, 0.10);
    assert!(
        pilot_isk > 0.0 && main_ess > 0.0 && reserve_ess > 0.0,
        "Null-sec 20m bounty tick must credit pilot wallet and ESS Main/Reserve banks (pilot={pilot_isk}, main={main_ess}, reserve={reserve_ess})"
    );
    let has_bounty_journal = ctx.db.with_snapshot(|snap| {
        snap.wallet_journal
            .iter()
            .any(|j| j.character_id == char_id && j.entry_type == "bounty_prizes")
    });
    assert!(has_bounty_journal, "Batched bounty tick must write a 'bounty_prizes' wallet journal entry");

    // 2. Gate/Station Sentry Gun grid aggression (`feat:npc.sentry-guns`, `feat:npc.sentry-gun-grid-aggression`):
    let lowsec_sys_id = 30_002_537;
    let ls_handle = ctx.systems.get_or_spawn(lowsec_sys_id);
    let _ = ls_handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 24690,
                position: DVec3::new(10_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 150.0,
                inertia: 1.0,
                mass: 50_000_000.0,
                radius: 300.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(24690),
        )
        .await;
    let (engaged, dmg) = step_sentry_gun_grid_aggression(
        &ctx,
        lowsec_sys_id,
        char_id,
        ship_id,
        DVec3::ZERO,
        true,
    )
    .await;
    assert!(engaged && dmg > 0.0, "Sentry guns must engage offender within 500km optimal range");

    // Warp off-grid (> 500 km) -> sentry gun drops aggression
    let _ = ls_handle
        .set_ball_position(ship_id, DVec3::new(600_000.0, 0.0, 0.0))
        .await;
    let (engaged_offgrid, _) = step_sentry_gun_grid_aggression(
        &ctx,
        lowsec_sys_id,
        char_id,
        ship_id,
        DVec3::ZERO,
        false,
    )
    .await;
    assert!(!engaged_offgrid, "Sentry guns must drop aggression when offender warps > 500km off-grid");
}
