//! Comprehensive deterministic unit tests covering edge cases across:
//! 1. Combat formulas, EWAR, turret/missile/Vorton/entropic/ECM/capacitor mechanics, death pipeline, insurance, killmails & loot
//! 2. Dogma fitting, CPU/PG/calibration/hardpoints, module linking, heat/burnout, ancillary/overload/ewar/capital, repackage & container bay guards
//! 3. Space docking, tether, ACL, cloak delay, wormhole stages/buckets/jumps, cyno beacons & filaments

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use eve_db::{
    CosmicSignatureRecord, InventoryFlag, InventoryItemRecord, TrainedSkillRecord,
    UpwellStructureRecord, WorldRepository,
};
use eve_destiny::{Ball, CombatState, DamageVector, MovementMode};
use eve_dogma::{ModuleState, ShipFittingEngine};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{InvType, SdeRegistry};
use glam::DVec3;

use crate::combat::death_pipeline::{
    drain_nested_ship_fittings, execute_live_self_destruct_sync, is_item_descendant_of_in_snap,
    resolve_destroyed_ship_contents_in_snap, SelfDestructManager,
};
use crate::formulas::{
    abyssal_attribute_roll, booster_side_effect_check, breacher_pod_tick_damage,
    command_burst_aggregate, compute_ecm_jam_chance, compute_fighter_squadron_damage,
    compute_layer_ehp, crystal_expected_cycles, dogma_stacking_penalty, entropic_spool_multiplier,
    missile_damage_factor, nanite_repair_paste_needed, passive_capacitor_or_shield_recharge,
    peak_passive_recharge_per_sec, rack_heat_step, slot_heat_damage_chance, target_lock_time_sec,
    turret_angular_velocity, turret_chance_to_hit, turret_chance_to_hit_with_res,
    turret_shot_damage, vorton_arc_targets_with_cap, vorton_chain_targets,
};
use crate::router::{BoundObjectEntry, ServerContext};
use crate::services::combat_crimewatch::combat_missiles_aoe::step_missiles_and_breachers;
use crate::services::combat_crimewatch::combat_npc_police::is_outlaw_ship_docking_refused;
use crate::services::combat_crimewatch::combat_wrecks_killmails::{
    compute_killmail_esi_hash, handle_wrecks_killmails, process_ship_insurance_payout,
};
use crate::services::combat_crimewatch::{
    with_combat_runtime, with_mut_combat_runtime, AssistLogEntry, InFlightMissileState,
    InsuranceContractState, SafetyLevel,
};
use crate::services::fitting_cycles_overheat::{
    check_nanite_repair_overheat_lockout, compute_overload_multiplier,
    handle_active_overload_toggle, has_category5_overload, prepare_ancillary_module,
};
use crate::services::fitting_saved_apply::{dna_to_fitting, fitting_to_dna, handle_fit_ships};
use crate::services::inventory_fitting::fitting_containers_ships::{
    validate_container_and_bay_move, write_container_ship_state,
};
use crate::services::inventory_fitting::fitting_dogma_im::handle_dogma_im;
use crate::services::inventory_fitting::fitting_dogma_ops::handle_dogma_ops;
use crate::services::inventory_fitting::fitting_ewar_capital::{
    prepare_ewar_and_capital_module, validate_passive_targeter_activation,
};
use crate::services::inventory_fitting::fitting_saved_multifit::handle_ships_and_services;
use crate::services::space_destiny::space_docking_cloaking::{
    compute_wormhole_stages, derive_wormhole_buckets, derive_wormhole_buckets_for_type,
};
use crate::services::space_destiny::space_nav_travel::sync_tether;
use crate::services::space_destiny::{space_cyno_filaments, space_warp_jump};
use crate::services::space_navigation_advanced::{
    check_immobilizing_module_active, compute_decloak_targeting_delay_for_module,
    compute_decloak_targeting_delay_s, validate_npc_station_docking,
};
use crate::services::structure_acl_defense_rules::{
    verify_structure_docking_acl, StructureDefenseSettings,
};
use crate::services::tactical_module_mechanics::{
    apply_overheat_rack_and_damage_on_cycle, evaluate_ship_thermodynamics, step_rack_heat_per_tick,
    validate_module_fitting_constraints,
};
use crate::system_actor::ActiveModuleCycle;

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new().with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED);
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

fn custom_ctx_with<F>(mutator: F) -> ServerContext
where
    F: FnOnce(&mut SdeRegistry),
{
    let base = test_ctx();
    let mut sde = (*base.sde).clone();
    mutator(&mut sde);
    ServerContext::with_sde_and_repository(
        Arc::new(sde),
        Arc::new(WorldRepository::with_demo_seed()),
    )
    .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
}

fn clone_inv_type(
    sde: &SdeRegistry,
    type_id: u32,
    group_id: u32,
    category_id: u32,
    volume: f64,
    capacity: f64,
    attrs: &[(u16, f64)],
    effects: &[u16],
) -> InvType {
    let mut base = sde
        .types
        .get(&587)
        .or_else(|| sde.types.get(&670))
        .unwrap_or_else(|| {
            let min_k = sde.types.keys().min().copied().expect("non-empty sde");
            &sde.types[&min_k]
        })
        .clone();
    base.type_id = type_id;
    base.group_id = group_id;
    base.category_id = category_id;
    base.name = format!("TestType_{type_id}");
    base.volume = volume;
    base.capacity = capacity;
    let mut default_attributes = HashMap::new();
    for &(k, v) in attrs {
        default_attributes.insert(k, v);
    }
    base.default_attributes = default_attributes;
    base.effect_ids = effects.to_vec();
    base
}

fn make_item(
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
    singleton: bool,
) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id,
        type_id,
        owner_id,
        location_id,
        flag,
        quantity,
        singleton,
        version: 1,
        dynamic_attributes: HashMap::new(),
        source_mutaplasmid_id: None,
        insured_payout_isk: 0.0,
        is_blueprint_copy: false,
        blueprint_runs_remaining: None,
    }
}

fn make_session(
    char_id: i64,
    corp_id: i64,
    sys_id: Option<u32>,
    station_id: Option<i64>,
    structure_id: Option<i64>,
    ship_id: Option<i64>,
) -> SessionState {
    let mut s = SessionState::new(901);
    s.user_id = Some(42);
    s.character_id = Some(char_id);
    s.corporation_id = Some(corp_id);
    s.solar_system_id = sys_id;
    s.station_id = station_id;
    s.structure_id = structure_id;
    s.ship_id = ship_id;
    s
}

fn bound_entry(char_id: i64, svc: &str, object_id: i64) -> BoundObjectEntry {
    BoundObjectEntry {
        bound_id: 77,
        session_id: 901,
        user_id: 42,
        character_id: char_id,
        service: svc.into(),
        object_id,
        oid_str: object_id.to_string(),
        ref_id: 1,
    }
}

fn spawn_ship_ball(
    ctx: &ServerContext,
    sys_id: u32,
    ship_id: i64,
    owner_id: i64,
    type_id: u32,
    pos: DVec3,
    mode: MovementMode,
) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            make_item(ship_id, type_id, owner_id, sys_id as i64, InventoryFlag::None, 1, true),
        );
        snap.item_positions.insert(ship_id, [pos.x, pos.y, pos.z]);
        if let Some(ch) = snap.characters.get_mut(&owner_id) {
            ch.active_ship_id = ship_id;
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
        }
    });
    let cs = ctx.build_ship_combat_state(owner_id, ship_id, type_id);
    ctx.systems.get_or_spawn(sys_id).try_spawn_ball_with_combat(
        Ball {
            id: ship_id,
            type_id,
            position: pos,
            velocity: DVec3::ZERO,
            max_velocity: 350.0,
            inertia: 2.0,
            mass: 1_200_000.0,
            radius: 40.0,
            speed_fraction: 1.0,
            mode,
        },
        cs,
    );
}

fn make_structure(
    structure_id: i64,
    type_id: u32,
    owner_corp_id: i64,
    sys_id: u32,
    state: &str,
    quantum_core_installed: bool,
    fuel_blocks: u32,
) -> UpwellStructureRecord {
    UpwellStructureRecord {
        structure_id,
        type_id,
        owner_corp_id,
        solar_system_id: sys_id,
        name: format!("Citadel {structure_id}"),
        position: [1_000_000.0, 0.0, 0.0],
        state: state.to_string(),
        quantum_core_installed,
        fuel_blocks,
        shield_hp: 1_000_000.0,
        armor_hp: 1_000_000.0,
        hull_hp: 1_000_000.0,
        reinforced_until_unix: 0,
        last_damage_tick: 0,
        last_damage_unix: 0,
        self_destruct_at_unix: None,
    }
}

// ============================================================================
// DOMAIN 1: COMBAT FORMULAS, EWAR, CYCLES, DEATH PIPELINE, INSURANCE, LOOT
// ============================================================================

#[test]
fn combat_formulas_stacking_heat_ewar_and_vorton_edges() {
    assert!((dogma_stacking_penalty(0) - 1.0).abs() < 1e-12);
    assert!(dogma_stacking_penalty(1) < dogma_stacking_penalty(0));
    assert!(dogma_stacking_penalty(9) < 1e-4);
    assert!(dogma_stacking_penalty(20) < 1e-20);

    // Passive recharge monotonically increases toward max_capacity and clamps at max
    let empty_step = passive_capacitor_or_shield_recharge(0.0, 100.0, 10.0, 1.0);
    assert!(empty_step > 0.0 && empty_step < 25.0);
    assert_eq!(passive_capacitor_or_shield_recharge(100.0, 100.0, 10.0, 1.0), 100.0);
    assert_eq!(passive_capacitor_or_shield_recharge(100.0, 0.0, 10.0, 1.0), 0.0);
    let peak_step = passive_capacitor_or_shield_recharge(250.0, 1000.0, 100_000.0, 1.0);
    let inst_rate = (peak_step - 250.0) / 0.001;
    let expected_peak = peak_passive_recharge_per_sec(1000.0, 100.0);
    assert!((inst_rate - expected_peak).abs() < 1e-2);
    let high_rate = (passive_capacitor_or_shield_recharge(900.0, 1000.0, 100_000.0, 1.0) - 900.0) / 0.001;
    assert!(high_rate < expected_peak);

    // Heat step, damage chance, and nanite repair paste
    assert!(rack_heat_step(0.0, 1.0, 0.5, 1.0) > 0.0);
    assert!(rack_heat_step(1.0, 5.0, 1.0, 1.0) <= 1.0);
    assert_eq!(slot_heat_damage_chance(0.0, 5, 0.0), 0.0);
    assert!(slot_heat_damage_chance(0.8, 0, 0.5) > slot_heat_damage_chance(0.8, 5, 0.5));
    assert!(slot_heat_damage_chance(0.8, 1, 0.9) < slot_heat_damage_chance(0.8, 1, 0.2));
    assert_eq!(nanite_repair_paste_needed(0.0, 5.0), 0);
    assert!(nanite_repair_paste_needed(20.0, 2.0) > 0);

    // Booster side effect & abyssal roll
    assert!(booster_side_effect_check(0.2, -10.0, 5, 5, 0.01).is_some());
    assert!(booster_side_effect_check(0.2, -10.0, 5, 5, 0.99).is_none());
    let base_map = HashMap::from([(1u16, 10.0)]);
    let ranges_map = HashMap::from([(1u16, [0.5, 1.5])]);
    let rolled_mid = abyssal_attribute_roll(&base_map, &ranges_map, &HashMap::from([(1u16, 0.5)]));
    assert!((rolled_mid[&1] - 10.0).abs() < 1e-9);
    let rolled_low = abyssal_attribute_roll(&base_map, &ranges_map, &HashMap::from([(1u16, 0.0)]));
    assert!((rolled_low[&1] - 5.0).abs() < 1e-9);
    let rolled_high = abyssal_attribute_roll(&base_map, &ranges_map, &HashMap::from([(1u16, 10.0)]));
    assert!((rolled_high[&1] - 15.0).abs() < 1e-9);

    // Command burst, EHP, and crystal expected cycles
    assert_eq!(command_burst_aggregate("max", &[]), 0.0);
    assert!((command_burst_aggregate("max", &[-5.0, 10.0, 25.0]) - 25.0).abs() < 1e-9);
    assert_eq!(compute_layer_ehp(0.0, [0.5; 4], [0.25; 4]), 0.0);
    let ehp_inf_guarded = compute_layer_ehp(100.0, [0.0; 4], [1.0, 0.0, 0.0, 0.0]);
    assert!(ehp_inf_guarded.is_finite() && ehp_inf_guarded > 100.0);
    assert!(crystal_expected_cycles(1.0, 0.0, 1.0) > 100_000.0);
    assert!((crystal_expected_cycles(1.0, 0.5, 0.5) - 4.0).abs() < 1e-9);

    // Lock time, turret tracking, shot damage, missile damage factor
    let slow_lock = target_lock_time_sec(0.0, 40.0);
    assert!(slow_lock > target_lock_time_sec(500.0, 400.0));
    let ang = turret_angular_velocity(
        DVec3::ZERO,
        DVec3::ZERO,
        DVec3::new(10_000.0, 0.0, 0.0),
        DVec3::new(0.0, 500.0, 0.0),
    );
    assert!((ang - 0.05).abs() < 1e-9);
    assert_eq!(
        turret_chance_to_hit_with_res(0.0, 0.0, 40.0, 40.0, 5_000.0, 1_000.0, 5_000.0),
        turret_chance_to_hit(0.0, 0.0, 40.0, 5_000.0, 1_000.0, 5_000.0)
    );
    assert_eq!(turret_chance_to_hit_with_res(1.0, 0.0, 40.0, 40.0, 5_000.0, 1_000.0, 5_000.0), 0.0);
    let base_dmg = DamageVector { em: 100.0, thermal: 0.0, kinetic: 0.0, explosive: 0.0 };
    let (wreck_dmg, is_wreck) = turret_shot_damage(base_dmg, 0.2, 0.005);
    assert!(is_wreck && (wreck_dmg.em - 300.0).abs() < 1e-9);
    let (miss_dmg, miss_wreck) = turret_shot_damage(base_dmg, 0.5, 0.6);
    assert!(!miss_wreck && miss_dmg.total() == 0.0);
    let (hit_dmg, _) = turret_shot_damage(base_dmg, 0.8, 0.5);
    assert!(hit_dmg.total() > 0.0);

    assert_eq!(missile_damage_factor(1000.0, 0.0, 100.0, 100.0, 0.5), 1.0);
    assert!(missile_damage_factor(15.0, 2000.0, 150.0, 100.0, 0.6) < 0.2);

    // Breacher pod, entropic spool, ECM jam, fighter damage, Vorton chain cap
    assert_eq!(breacher_pod_tick_damage(1000.0, 0.05, 100.0), 5.0);
    assert_eq!(breacher_pod_tick_damage(10.0, 0.05, 10_000.0), 10.0);
    assert_eq!(entropic_spool_multiplier(0, 10, 0.15), 1.0);
    assert!((entropic_spool_multiplier(50, 10, 0.15) - 2.5).abs() < 1e-9);
    assert_eq!(compute_ecm_jam_chance(10.0, 0.0, 1000.0, 2000.0, 1000.0), 1.0);
    assert!(compute_ecm_jam_chance(5.0, 20.0, 1000.0, 2000.0, 1000.0) < 0.5);
    assert!((compute_fighter_squadron_damage(100.0, 6, 1.2) - 720.0).abs() < 1e-9);

    let candidates = vec![
        (10, DVec3::ZERO),
        (20, DVec3::new(1_000.0, 0.0, 0.0)),
        (30, DVec3::new(500.0, 0.0, 0.0)),
        (40, DVec3::new(20_000.0, 0.0, 0.0)),
    ];
    assert_eq!(vorton_arc_targets_with_cap(10, DVec3::ZERO, &candidates, 10_000.0, 4), vec![10, 30, 20]);
    assert_eq!(vorton_chain_targets(10, DVec3::ZERO, &[(1, DVec3::new(100.0, 0.0, 0.0))], 500.0), vec![10, 1]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn system_actor_ecm_burst_sleeper_cap_immunity_and_blood_raider_nos_edges() {
    let ctx = custom_ctx_with(|sde| {
        let ship_grav = clone_inv_type(sde, 880_001, 25, 6, 10_000.0, 100.0, &[(211, 20.0)], &[]);
        let pod_t = clone_inv_type(sde, 880_002, 29, 6, 10_000.0, 100.0, &[(208, 12.0)], &[]);
        let wreck_t = clone_inv_type(sde, 880_003, 186, 2, 10_000.0, 100.0, &[], &[]);
        let sleeper_t = clone_inv_type(sde, 880_004, 959, 11, 10_000.0, 0.0, &[(211, 20.0)], &[]);
        sde.types.insert(ship_grav.type_id, ship_grav);
        sde.types.insert(pod_t.type_id, pod_t);
        sde.types.insert(wreck_t.type_id, wreck_t);
        sde.types.insert(sleeper_t.type_id, sleeper_t);
    });

    let sys_id = 30_000_142;
    let sh = ctx.systems.get_or_spawn(sys_id);
    let spawn = |id: i64, tid: u32, pos: DVec3, cloaked: bool, cap: f64| {
        let mut cs = CombatState::default();
        cs.is_cloaked = cloaked;
        cs.capacitor = cap;
        cs.max_capacitor = 1000.0;
        sh.try_spawn_ball_with_combat(
            Ball {
                id,
                type_id: tid,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 100.0,
                inertia: 1.0,
                mass: 1_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            cs,
        );
    };
    spawn(1, 880_001, DVec3::ZERO, false, 800.0); // source attacker
    spawn(2, 880_001, DVec3::new(1_000.0, 0.0, 0.0), true, 500.0); // cloaked
    spawn(3, 880_001, DVec3::new(1_000.0, 0.0, 0.0), false, 500.0); // has self_ewar_immune cycle
    spawn(4, 880_003, DVec3::new(1_000.0, 0.0, 0.0), false, 500.0); // non-ship wreck
    spawn(5, 880_002, DVec3::new(1_000.0, 0.0, 0.0), false, 200.0); // pod -> jammed & nos target
    spawn(6, 880_001, DVec3::new(9_000.0, 0.0, 0.0), false, 500.0); // out of burst range
    spawn(7, 880_004, DVec3::new(500.0, 0.0, 0.0), false, 500.0); // Sleeper group 959 -> cap immune

    let _ = sh.add_module_cycle(ActiveModuleCycle {
        ship_id: 3,
        module_id: 90_001,
        module_type_id: 880_001,
        effect_name: "ewarImmunityTest".into(),
        repeat: 1,
        duration_s: 10.0,
        self_ewar_immune: true,
        ..Default::default()
    }).await;

    let jammed = sh
        .apply_ecm_burst_ships(1, 5_000.0, [20.0; 4], 10.0, 0.01, None)
        .await
        .unwrap();
    assert!(jammed.contains(&5));
    assert!(!jammed.contains(&1));
    assert!(!jammed.contains(&2));
    assert!(!jammed.contains(&3));
    assert!(!jammed.contains(&4));
    assert!(!jammed.contains(&6));
    assert_eq!(sh.get_combat_state(5).await.unwrap().unwrap().ecm_jammed_remaining_s, 0.0);

    // Neut vs Sleeper (cap immune) & Standard Nos vs ship 5 (attacker cap 800 > target 200)
    let _ = sh.add_module_cycle(ActiveModuleCycle {
        ship_id: 1,
        module_id: 90_002,
        module_type_id: 880_001,
        effect_name: "energyDestabilizationNew".into(),
        target_id: Some(7),
        repeat: 0,
        duration_s: 1.0,
        elapsed_s: 0.9,
        cap_drain_target: 150.0,
        ..Default::default()
    }).await;
    let _ = sh.add_module_cycle(ActiveModuleCycle {
        ship_id: 1,
        module_id: 90_003,
        module_type_id: 880_001,
        effect_name: "energyTransfer".into(),
        target_id: Some(5),
        repeat: 0,
        duration_s: 1.0,
        elapsed_s: 0.9,
        cap_transfer_target: 50.0,
        nosferatu_blood_raider_override: false,
        ..Default::default()
    }).await;
    let _ = sh.step_manual(0.2).await;
    assert!(sh.get_combat_state(7).await.unwrap().unwrap().capacitor >= 500.0);
    assert!(sh.get_combat_state(5).await.unwrap().unwrap().capacitor >= 200.0);

    // Blood Raider Nosferatu override drains target regardless of attacker cap > target cap
    let cap_before = sh.get_combat_state(5).await.unwrap().unwrap().capacitor;
    let _ = sh.add_module_cycle(ActiveModuleCycle {
        ship_id: 1,
        module_id: 90_004,
        module_type_id: 880_001,
        effect_name: "energyTransfer".into(),
        target_id: Some(5),
        repeat: 0,
        duration_s: 1.0,
        elapsed_s: 0.9,
        cap_transfer_target: 80.0,
        nosferatu_blood_raider_override: true,
        ..Default::default()
    }).await;
    let _ = sh.step_manual(0.2).await;
    assert!(sh.get_combat_state(5).await.unwrap().unwrap().capacitor < cap_before);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn death_pipeline_self_destruct_insurance_hash_and_loot_edges() {
    let ctx = test_ctx();
    let sys_id = 30_000_142;
    let victim = 90_000_001;
    let attacker = 90_000_002;
    let now = ctx.now_unix();

    // 1. Descendant helper cycle/depth guard, plastic wrap unwrapping, rig 0% drop, nested drain
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(100, make_item(100, 587, victim, sys_id as i64, InventoryFlag::None, 1, true));
        snap.items.insert(101, make_item(101, 23, victim, 100, InventoryFlag::Cargo, 1, true));
        snap.items.insert(102, make_item(102, 34, victim, 101, InventoryFlag::Cargo, 50, false));
        snap.items.insert(201, make_item(201, 23, victim, 202, InventoryFlag::Cargo, 1, true));
        snap.items.insert(202, make_item(202, 23, victim, 201, InventoryFlag::Cargo, 1, true));
        assert!(is_item_descendant_of_in_snap(snap, 102, 100));
        assert!(!is_item_descendant_of_in_snap(snap, 201, 100));
        assert!(!is_item_descendant_of_in_snap(snap, 100, 100));

        snap.items.insert(300, make_item(300, 587, victim, sys_id as i64, InventoryFlag::None, 1, true));
        snap.items.insert(301, make_item(301, 3468, victim, 300, InventoryFlag::Cargo, 1, true));
        snap.items.insert(302, make_item(302, 34, victim, 301, InventoryFlag::Cargo, 10, false));
        snap.items.insert(303, make_item(303, 31790, victim, 300, InventoryFlag::RigSlot0, 1, true));
        let resolved = resolve_destroyed_ship_contents_in_snap(
            snap, &ctx.sde, 300, 399, None, None, false, 0.5, |_| true, |_| 0.1,
        );
        assert!(resolved.dropped_ids.contains(&302));
        assert!(!resolved.dropped_ids.contains(&301));
        assert!(resolved.destroyed_ids.contains(&303));
    });
    let drained = drain_nested_ship_fittings(&ctx, 300, false);
    assert!(drained.is_empty());

    // 2. SelfDestructManager lifecycle + uncontested vs assisted live self-destruct
    let mut sdm = SelfDestructManager::new();
    let det = sdm.start_self_destruct(500, victim, sys_id, now);
    assert_eq!(det, now + 120);
    assert!(sdm.is_self_destructing(500));
    assert_eq!(sdm.remaining_seconds(500, now + 30), Some(90));
    assert!(sdm.abort_self_destruct(500));
    assert!(!sdm.abort_self_destruct(500));

    let sd_ship = 500_001_001;
    spawn_ship_ball(&ctx, sys_id, sd_ship, victim, 587, DVec3::new(10_000.0, 0.0, 0.0), MovementMode::Stop);
    let uncontested = execute_live_self_destruct_sync(&ctx, sys_id, victim, sd_ship, DVec3::new(10_000.0, 0.0, 0.0), now).unwrap().unwrap();
    if uncontested.killmail_id > 0 {
        let final_blow = ctx.db.with_snapshot(|s| s.killmails.get(&uncontested.killmail_id).map(|k| k.attacker_character_id));
        assert_eq!(final_blow, Some(victim));
    }

    let sd_ship_assisted = 500_001_002;
    spawn_ship_ball(&ctx, sys_id, sd_ship_assisted, victim, 587, DVec3::new(20_000.0, 0.0, 0.0), MovementMode::Stop);
    with_mut_combat_runtime(&ctx, |rt| {
        rt.aggression_assist_log.insert(
            sd_ship_assisted,
            vec![AssistLogEntry {
                attacker_char_id: attacker,
                corporation_id: 98_000_002,
                alliance_id: None,
                ship_type_id: 587,
                weapon_type_id: 2873,
                damage_done: 100.0,
                is_ewar: false,
                last_action_unix: now,
            }],
        );
    });
    let contested = execute_live_self_destruct_sync(&ctx, sys_id, victim, sd_ship_assisted, DVec3::new(20_000.0, 0.0, 0.0), now).unwrap().unwrap();
    assert!(contested.killmail_id > 0);
    let contested_fb = ctx.db.with_snapshot(|s| s.killmails.get(&contested.killmail_id).map(|k| k.attacker_character_id));
    assert_eq!(contested_fb, Some(attacker));

    // 3. Insurance payout edge cases: CONCORD criminal -> 0, Capsule 670 -> 0
    let ins_ship = 500_002_001;
    with_mut_combat_runtime(&ctx, |rt| {
        rt.insurance_contracts.insert(
            ins_ship,
            InsuranceContractState {
                ship_id: ins_ship,
                ship_type_id: 587,
                owner_id: victim,
                level_name: "platinum".into(),
                level_fraction: 1.0,
                cost_isk: 10_000.0,
                payout_isk: 500_000.0,
                start_unix: now.saturating_sub(100),
                end_unix: now + 86_400,
            },
        );
    });
    assert_eq!(process_ship_insurance_payout(&ctx, victim, ins_ship, 587, true), 0.0);
    assert_eq!(process_ship_insurance_payout(&ctx, victim, 500_002_002, 670, false), 0.0);

    // 4. ESI Killmail SHA1 hash deterministic formatting
    let h1 = compute_killmail_esi_hash(victim, 0, 587, 1_700_000_000);
    let h2 = compute_killmail_esi_hash(victim, 0, 587, 1_700_000_000);
    assert_eq!(h1, h2);
    assert_eq!(h1.len(), 40);

    // 5. LootAll edges: docked, out-of-range, green-safety-suspect
    let mut sess = make_session(victim, 98_000_001, Some(sys_id), Some(60_003_760), None, Some(sd_ship_assisted));
    let res_docked = handle_wrecks_killmails(&ctx, &mut sess, "invbroker", "LootAll", &[PyRep::Integer(1)], &HashMap::new()).await.unwrap();
    assert!(res_docked.is_err());

    sess.station_id = None;
    let active_ship = 500_003_001;
    let wreck_id = 500_003_002;
    let wreck_near = 500_003_004;
    spawn_ship_ball(&ctx, sys_id, active_ship, victim, 587, DVec3::ZERO, MovementMode::Stop);
    sess.ship_id = Some(active_ship);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(wreck_id, make_item(wreck_id, 26557, attacker, sys_id as i64, InventoryFlag::None, 1, true));
        snap.item_positions.insert(wreck_id, [10_000.0, 0.0, 0.0]);
    });
    let res_not_in_space = handle_wrecks_killmails(&ctx, &mut sess, "invbroker", "LootAll", &[PyRep::Integer(wreck_id)], &HashMap::new()).await.unwrap();
    assert_eq!(res_not_in_space.unwrap_err().code, "ContainerNotInSpace");

    spawn_ship_ball(&ctx, sys_id, wreck_id, attacker, 26557, DVec3::new(10_000.0, 0.0, 0.0), MovementMode::Stop);
    let res_far = handle_wrecks_killmails(&ctx, &mut sess, "invbroker", "LootAll", &[PyRep::Integer(wreck_id)], &HashMap::new()).await.unwrap();
    assert_eq!(res_far.unwrap_err().code, "OutOfRange");

    spawn_ship_ball(&ctx, sys_id, wreck_near, attacker, 26557, DVec3::new(500.0, 0.0, 0.0), MovementMode::Stop);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(500_003_003, make_item(500_003_003, 34, attacker, wreck_near, InventoryFlag::None, 10, false));
    });
    with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(victim, SafetyLevel::Green);
    });
    let res_safety = handle_wrecks_killmails(&ctx, &mut sess, "invbroker", "LootAll", &[PyRep::Integer(wreck_near)], &HashMap::new()).await.unwrap();
    assert_eq!(res_safety.unwrap_err().code, "CantDoInCurrentSafetyLevel");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missiles_and_breacher_pod_step_edges() {
    let ctx = test_ctx();
    let sys_id = 30_000_144;
    let attacker = 90_000_001;
    let defender = 90_000_002;
    let atk_ship = 600_001_001;
    let def_ship = 600_001_002;
    spawn_ship_ball(&ctx, sys_id, atk_ship, attacker, 587, DVec3::ZERO, MovementMode::Stop);
    spawn_ship_ball(&ctx, sys_id, def_ship, defender, 587, DVec3::new(1_000.0, 0.0, 0.0), MovementMode::Stop);

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let spawn_m = |mid: i64, pos: DVec3, vel: f64, rem_s: f64, breacher: bool, spawn_ball: bool| {
        if spawn_ball {
            let mut cs = CombatState::default();
            cs.structure_hp = 50.0;
            sys_handle.try_spawn_ball_with_combat(
                Ball {
                    id: mid,
                    type_id: 210,
                    position: pos,
                    velocity: DVec3::new(vel, 0.0, 0.0),
                    max_velocity: vel,
                    inertia: 1.0,
                    mass: 100.0,
                    radius: 1.0,
                    speed_fraction: 1.0,
                    mode: MovementMode::Missile { target_id: def_ship },
                },
                cs,
            );
        }
        with_mut_combat_runtime(&ctx, |rt| {
            rt.in_flight_missiles.insert(
                mid,
                InFlightMissileState {
                    missile_ball_id: mid,
                    solar_system_id: sys_id,
                    attacker_char_id: attacker,
                    attacker_ship_id: atk_ship,
                    launcher_module_id: 700_001,
                    launcher_type_id: 2404,
                    missile_type_id: 210,
                    target_ball_id: def_ship,
                    max_velocity: vel,
                    flight_time_s: 5.0,
                    remaining_flight_s: rem_s,
                    hp: 50.0,
                    max_hp: 50.0,
                    base_damage: DamageVector { em: 0.0, thermal: 100.0, kinetic: 0.0, explosive: 0.0 },
                    explosion_radius: 40.0,
                    explosion_velocity: 100.0,
                    drf: 0.5,
                    is_fof: false,
                    is_breacher_pod: breacher,
                    breacher_dot_duration_s: 20.0,
                    breacher_flat_cap: 150.0,
                    breacher_pct_cap: 0.02,
                    destroyed: false,
                    expired: false,
                    impacted: false,
                    applied_damage: 0.0,
                },
            );
        });
    };

    spawn_m(600_100_001, DVec3::new(500.0, 0.0, 0.0), 1000.0, 2.0, false, false);
    spawn_m(600_100_002, DVec3::new(500.0, 0.0, 0.0), 1000.0, 0.2, false, true);
    spawn_m(600_100_003, DVec3::new(995.0, 0.0, 0.0), 2000.0, 3.0, true, true);

    let completed = step_missiles_and_breachers(&ctx, sys_id, 0.5).await;
    assert_eq!(completed.len(), 3);
    assert!(completed.iter().any(|m| m.missile_ball_id == 600_100_001 && m.destroyed));
    assert!(completed.iter().any(|m| m.missile_ball_id == 600_100_002 && m.expired));
    assert!(completed.iter().any(|m| m.missile_ball_id == 600_100_003 && m.impacted));
    let breacher_registered = with_combat_runtime(&ctx, |rt| rt.active_breacher_dots.contains_key(&def_ship));
    assert!(breacher_registered);
}

// ============================================================================
// DOMAIN 2: FITTING, DOGMA, HEAT, EWAR/CAPITAL MODULES, REPACKAGE & BAYS
// ============================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fitting_apply_dna_cpu_pg_calibration_hardpoints_and_holds_edges() {
    assert!(dna_to_fitting("").is_err());
    assert!(dna_to_fitting("587:bad").is_err());
    let (parsed_ship, parsed_items) = dna_to_fitting("587:2873;1:1952;2:210;100::").unwrap();
    assert_eq!(parsed_ship, 587);
    let dna = fitting_to_dna(587, &parsed_items);
    assert!(dna.starts_with("587:"));

    let ctx = custom_ctx_with(|sde| {
        let ship_t = clone_inv_type(
            sde, 890_001, 25, 6, 10_000.0, 50.0,
            &[
                (eve_sde::attr_ids::CPU_OUTPUT, 100.0),
                (eve_sde::attr_ids::POWER_OUTPUT, 100.0),
                (eve_sde::attr_ids::HI_SLOTS, 2.0),
                (eve_sde::attr_ids::MED_SLOTS, 2.0),
                (eve_sde::attr_ids::LOW_SLOTS, 2.0),
                (eve_sde::attr_ids::RIG_SLOTS, 2.0),
                (eve_sde::attr_ids::UPGRADE_CAPACITY, 400.0),
                (eve_sde::attr_ids::RIG_SIZE, 1.0),
                (eve_sde::attr_ids::TURRET_SLOTS_LEFT, 1.0),
                (eve_sde::attr_ids::LAUNCHER_SLOTS_LEFT, 1.0),
                (eve_sde::attr_ids::DRONE_CAPACITY, 10.0),
            ],
            &[],
        );
        let lo_mod = clone_inv_type(sde, 890_010, 53, 7, 5.0, 0.0, &[(eve_sde::attr_ids::CPU, 10.0)], &[11]);
        let turret_hi = clone_inv_type(sde, 890_012, 55, 7, 5.0, 10.0, &[(eve_sde::attr_ids::CPU, 10.0), (137, 83.0)], &[12, 42]);
        let launcher_hi = clone_inv_type(sde, 890_013, 507, 7, 5.0, 10.0, &[(eve_sde::attr_ids::CPU, 10.0)], &[12, 40]);
        let rig_big = clone_inv_type(sde, 890_014, 773, 7, 5.0, 0.0, &[(eve_sde::attr_ids::RIG_SIZE, 2.0), (eve_sde::attr_ids::UPGRADE_COST, 100.0)], &[2663]);
        let rig_high_cal = clone_inv_type(sde, 890_015, 773, 7, 5.0, 0.0, &[(eve_sde::attr_ids::RIG_SIZE, 1.0), (eve_sde::attr_ids::UPGRADE_COST, 450.0)], &[2663]);
        let fighter_t = clone_inv_type(sde, 890_020, 1652, 87, 5.0, 0.0, &[], &[]);
        let mineral_t = clone_inv_type(sde, 890_021, 18, 4, 200.0, 0.0, &[], &[]);
        for t in [ship_t, lo_mod, turret_hi, launcher_hi, rig_big, rig_high_cal, fighter_t, mineral_t] {
            sde.types.insert(t.type_id, t);
        }
    });

    let char_id = 90_000_001;
    let station_id = 60_003_760_i64;
    let ship_id = 700_001_001;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(ship_id, make_item(ship_id, 890_001, char_id, station_id, InventoryFlag::Hangar, 1, true));
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(station_id);
            ch.active_ship_id = ship_id;
        }
    });
    let mut sess = make_session(char_id, 98_000_001, Some(30_000_142), Some(station_id), None, Some(ship_id));

    let build_req = |items: Vec<(u32, u16, i32)>| {
        let py_items: Vec<PyRep> = items
            .into_iter()
            .map(|(tid, flag, qty)| PyRep::Tuple(vec![PyRep::Integer(tid as i64), PyRep::Integer(flag as i64), PyRep::Integer(qty as i64)]))
            .collect();
        let mut kw = HashMap::new();
        kw.insert("shipTypeID".into(), PyRep::Integer(890_001));
        kw.insert("fitData".into(), PyRep::List(py_items));
        let args = vec![PyRep::List(vec![PyRep::Integer(ship_id)])];
        (args, kw)
    };

    let (a, k) = build_req(vec![(890_014, 92, 1)]);
    assert!(handle_fit_ships(&ctx, &mut sess, &a, &k).await.unwrap().unwrap_err().message.contains("RigSizeMismatch"));

    let (a, k) = build_req(vec![(890_015, 92, 1)]);
    assert!(handle_fit_ships(&ctx, &mut sess, &a, &k).await.unwrap().unwrap_err().message.contains("UpgradeCapacityExceeded"));

    let (a, k) = build_req(vec![(890_012, 27, 1), (890_012, 28, 1)]);
    assert!(handle_fit_ships(&ctx, &mut sess, &a, &k).await.unwrap().unwrap_err().message.contains("InsufficientTurretHardpoints"));

    let (a, k) = build_req(vec![(890_013, 27, 1), (890_013, 28, 1)]);
    assert!(handle_fit_ships(&ctx, &mut sess, &a, &k).await.unwrap().unwrap_err().message.contains("InsufficientLauncherHardpoints"));

    let (a, k) = build_req(vec![(890_010, 27, 1)]);
    assert!(handle_fit_ships(&ctx, &mut sess, &a, &k).await.unwrap().unwrap_err().message.contains("SlotEffectMismatch"));

    let (a, k) = build_req(vec![(890_021, 5, 1)]);
    assert_eq!(handle_fit_ships(&ctx, &mut sess, &a, &k).await.unwrap().unwrap_err().code, "CapacityExceeded");

    let (a, k) = build_req(vec![(890_021, 87, 1)]);
    assert_eq!(handle_fit_ships(&ctx, &mut sess, &a, &k).await.unwrap().unwrap_err().code, "ItemNotADrone");

    let (a, k) = build_req(vec![(890_020, 158, 1)]);
    assert_eq!(handle_fit_ships(&ctx, &mut sess, &a, &k).await.unwrap().unwrap_err().code, "ShipHasNoFighterBay");

    let (a, k) = build_req(vec![(890_010, 11, 1), (890_010, 11, 1)]);
    let err_dup = handle_fit_ships(&ctx, &mut sess, &a, &k).await.unwrap().unwrap_err();
    assert_eq!(err_dup.code, "UserError");
    assert!(err_dup.message.contains("SlotAlreadyOccupied"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn heat_overload_ancillary_ewar_capital_and_constraints_edges() {
    let ctx = custom_ctx_with(|sde| {
        let cat5_eff_base = sde.effects.keys().min().copied().expect("seed effect");
        let mut eff = sde.effects[&cat5_eff_base].clone();
        eff.effect_id = 65_001;
        eff.name = "overloadTestEffect".into();
        eff.effect_category = 5;
        sde.effects.insert(65_001, eff);

        let frig = clone_inv_type(
            sde, 891_001, 25, 6, 1000.0, 100.0,
            &[
                (14, 1.0), (13, 1.0), (12, 1.0), (1137, 0.0),
                (1224, 1.0), (1179, 0.05), (1196, 0.05), (1198, 0.05),
                (1222, 0.5), (1223, 0.5), (1225, 0.5),
            ],
            &[],
        );
        let bs = clone_inv_type(sde, 891_002, 27, 6, 50_000.0, 500.0, &[], &[]);
        let grappler = clone_inv_type(sde, 891_010, 1672, 7, 5.0, 0.0, &[(1544, 1.0)], &[]);
        let asb = clone_inv_type(sde, 891_011, 1156, 7, 5.0, 0.0, &[(6, 200.0), (68, 300.0), (56, 1.0)], &[13]);
        let aar = clone_inv_type(sde, 891_012, 1199, 7, 5.0, 0.0, &[(6, 100.0), (84, 100.0), (56, 4.0)], &[11]);
        let nullifier = clone_inv_type(sde, 891_013, 4117, 7, 5.0, 0.0, &[], &[11]);
        let adc = clone_inv_type(sde, 891_014, 60, 7, 5.0, 0.0, &[], &[11, 7012]);
        let cehe = clone_inv_type(sde, 891_015, 60, 7, 5.0, 0.0, &[], &[11, 6484]);
        let passive_targ = clone_inv_type(sde, 891_016, 82, 7, 5.0, 0.0, &[], &[13]);
        let cat5_mod = clone_inv_type(sde, 891_020, 53, 7, 5.0, 0.0, &[], &[11, 65_001]);
        let burnout_mod = clone_inv_type(sde, 891_030, 53, 7, 5.0, 0.0, &[(3, 0.0), (9, 20.0), (1180, 10.0), (1211, 20.0)], &[13]);

        for t in [frig, bs, grappler, asb, aar, nullifier, adc, cehe, passive_targ, cat5_mod, burnout_mod] {
            sde.types.insert(t.type_id, t);
        }
    });

    let char_id = 90_000_001;
    let sys_id = 30_000_142;
    let ship_frig = 710_001_001;
    let ship_bs = 710_001_002;
    let mod_asb = 710_002_001;
    let mod_aar = 710_002_002;
    let mod_paste = 710_002_003;
    let mod_grap = 710_002_004;
    let mod_adc = 710_003_002;
    let mod_cehe = 710_003_003;
    let mod_null = 710_003_004;
    let mod_burn = 710_002_099;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(ship_frig, make_item(ship_frig, 891_001, char_id, sys_id as i64, InventoryFlag::None, 1, true));
        snap.items.insert(ship_bs, make_item(ship_bs, 891_002, char_id, sys_id as i64, InventoryFlag::None, 1, true));
        snap.items.insert(mod_grap, make_item(mod_grap, 891_010, char_id, ship_bs, InventoryFlag::MedSlot0, 1, true));
        snap.items.insert(mod_asb, make_item(mod_asb, 891_011, char_id, ship_frig, InventoryFlag::MedSlot0, 1, true));
        snap.items.insert(mod_aar, make_item(mod_aar, 891_012, char_id, ship_frig, InventoryFlag::LoSlot0, 1, true));
        snap.items.insert(mod_paste, make_item(mod_paste, 28668, char_id, ship_frig, InventoryFlag::LoSlot0, 4, false));
        let mut bm = make_item(mod_burn, 891_030, char_id, ship_frig, InventoryFlag::MedSlot1, 1, true);
        bm.dynamic_attributes.insert(3, 15.0);
        snap.items.insert(mod_burn, bm);
        snap.skills.entry(char_id).or_default().insert(
            28164,
            TrainedSkillRecord { type_id: 28164, level: 5, skill_points: 256_000.0 },
        );
    });

    // 1. Heavy Stasis Grappler rejects Frigate, enforces maxGroupFitted
    let grap_t = &ctx.sde.types[&891_010];
    let frig_t = &ctx.sde.types[&891_001];
    let bs_t = &ctx.sde.types[&891_002];
    assert_eq!(
        ctx.db.with_snapshot(|s| validate_module_fitting_constraints(&ctx, s, ship_frig, 710_999_001, grap_t, frig_t)).unwrap_err().code,
        "CanNotFitModuleToShip"
    );
    assert_eq!(
        ctx.db.with_snapshot(|s| validate_module_fitting_constraints(&ctx, s, ship_bs, 710_999_002, grap_t, bs_t)).unwrap_err().code,
        "MaxGroupFitted"
    );

    // 2. Category-5 overload multiplier avoids double multiplication
    let plain_t = &ctx.sde.types[&891_011];
    let cat5_t = &ctx.sde.types[&891_020];
    assert!(!has_category5_overload(plain_t, &ctx.sde));
    assert!(has_category5_overload(cat5_t, &ctx.sde));
    assert_eq!(compute_overload_multiplier(true, cat5_t, &ctx.sde), 1.0);
    assert!((compute_overload_multiplier(true, plain_t, &ctx.sde) - 1.2).abs() < 1e-9);

    // 3. ASB unfueled cap fallback vs AAR 3x armor repair bonus with paste
    let mut fit = ShipFittingEngine::from_sde(ship_frig, 891_001, &ctx.sde);
    let _ = fit.fit_module_with_state(mod_asb, 891_011, InventoryFlag::MedSlot0 as u16, true, &ctx.sde);
    let _ = fit.fit_module_with_state(mod_aar, 891_012, InventoryFlag::LoSlot0 as u16, true, &ctx.sde);

    let asb_prep = prepare_ancillary_module(
        &ctx, ship_frig, mod_asb, InventoryFlag::MedSlot0 as u16, &ctx.sde.types[&891_011], &fit, false, 0.0, 0.0, 0.0, 1.0,
    );
    assert!(asb_prep.is_asb && asb_prep.asb_fallback_cap_need > 0.0);
    let aar_prep = prepare_ancillary_module(
        &ctx, ship_frig, mod_aar, InventoryFlag::LoSlot0 as u16, &ctx.sde.types[&891_012], &fit, false, 100.0, 100.0, 0.0, 1.0,
    );
    assert!(aar_prep.is_aar && (aar_prep.armor_repair_amount - 300.0).abs() < 1e-6);

    // 4. Overload toggle & nanite repair lockout
    let sys = ctx.systems.get_or_spawn(sys_id);
    assert!(check_nanite_repair_overheat_lockout(&sys, ship_frig, mod_asb, "Activate", ModuleState::Online).is_ok());
    let _ = handle_active_overload_toggle(&ctx, &sys, char_id, ship_frig, mod_asb, fit.clone(), false).await;

    // 5. Thermodynamics rack heat step & burnout on cycle
    let sde_opt = Some(Arc::clone(&ctx.sde));
    let db_opt = Some(Arc::clone(&ctx.db));
    let thermo = evaluate_ship_thermodynamics(ship_frig, &sde_opt, &db_opt);
    assert!(thermo.diss_rates[1] > 0.0 && thermo.heat_gen_mult > 0.0);

    let mut rack_heat: HashMap<i64, [f64; 3]> = HashMap::new();
    ctx.db.with_mut_snapshot(|snap| {
        snap.transient_item_attributes.entry(ship_frig).or_default().insert(1176, 0.5);
    });
    rack_heat.insert(ship_frig, [0.0, 0.5, 0.0]);
    step_rack_heat_per_tick(&mut rack_heat, &[], &sde_opt, &db_opt, 1.0);
    assert!(rack_heat[&ship_frig][1] < 0.5);

    ctx.db.with_mut_snapshot(|snap| {
        snap.transient_item_attributes.entry(ship_frig).or_default().insert(1176, 1.0);
    });
    rack_heat.insert(ship_frig, [0.0, 1.0, 0.0]);
    let burn_cycle = ActiveModuleCycle {
        ship_id: ship_frig,
        module_id: mod_burn,
        module_type_id: 891_030,
        owner_char_id: Some(char_id),
        rack_idx: 1,
        is_overloaded: true,
        heat_damage_per_cycle: 10.0,
        ..Default::default()
    };
    let mut burned_out = false;
    let mut burned_mods = Vec::new();
    let rng_opt = Some(Arc::clone(&ctx.mechanic_rng));
    apply_overheat_rack_and_damage_on_cycle(&burn_cycle, &mut rack_heat, &sde_opt, &db_opt, &rng_opt, &mut burned_out, &mut burned_mods);
    assert!(burned_out);
    assert!(burned_mods.is_empty());
    assert!(ctx.db.with_snapshot(|s| !s.module_online_states[&mod_burn]));

    // 6. EWAR & Capital module guards (Nullifier in warp, ADC on non-AF/HAC, CEHE on non-capital, Passive Targeter)
    spawn_ship_ball(
        &ctx,
        sys_id,
        ship_frig,
        char_id,
        891_001,
        DVec3::ZERO,
        MovementMode::new_warp(DVec3::ZERO, DVec3::new(1e9, 0.0, 0.0), 3.0),
    );
    let mut fit_ewar = ShipFittingEngine::from_sde(ship_frig, 891_001, &ctx.sde);
    fit_ewar.fit_module_with_state(mod_null, 891_013, 11, true, &ctx.sde).unwrap();
    let null_m = fit_ewar.modules[&mod_null].clone();
    let null_state = fit_ewar.build_effective_module_state(mod_null, 891_013, false, &ctx.sde);
    let null_res = prepare_ewar_and_capital_module(
        &ctx, &sys, sys_id, char_id, ship_frig, 25, mod_null, &null_m, &ctx.sde.types[&891_013],
        &null_state, &fit_ewar, None, true, false, false, &HashMap::new(),
    ).await.unwrap();
    assert!(null_res.is_err());

    let mut fit_adc = ShipFittingEngine::from_sde(ship_frig, 891_001, &ctx.sde);
    fit_adc.fit_module_with_state(mod_adc, 891_014, 11, true, &ctx.sde).unwrap();
    let adc_m = fit_adc.modules[&mod_adc].clone();
    let adc_state = fit_adc.build_effective_module_state(mod_adc, 891_014, false, &ctx.sde);
    let adc_res = prepare_ewar_and_capital_module(
        &ctx, &sys, sys_id, char_id, ship_frig, 25, mod_adc, &adc_m, &ctx.sde.types[&891_014],
        &adc_state, &fit_adc, None, false, false, false, &HashMap::new(),
    ).await.unwrap();
    assert!(adc_res.is_err());

    let mut fit_cehe = ShipFittingEngine::from_sde(ship_frig, 891_001, &ctx.sde);
    fit_cehe.fit_module_with_state(mod_cehe, 891_015, 11, true, &ctx.sde).unwrap();
    let cehe_m = fit_cehe.modules[&mod_cehe].clone();
    let cehe_state = fit_cehe.build_effective_module_state(mod_cehe, 891_015, false, &ctx.sde);
    let cehe_res = prepare_ewar_and_capital_module(
        &ctx, &sys, sys_id, char_id, ship_frig, 25, mod_cehe, &cehe_m, &ctx.sde.types[&891_015],
        &cehe_state, &fit_cehe, None, false, false, false, &HashMap::new(),
    ).await.unwrap();
    assert!(cehe_res.is_err());

    let self_targ_err = validate_passive_targeter_activation(&ctx, &sys, char_id, ship_frig, ship_frig, &fit_ewar).await.unwrap_err();
    assert!(!self_targ_err.code.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dogma_im_targets_linking_stance_destroy_and_ops_edges() {
    let ctx = test_ctx();
    let sys_id = 30_000_142;
    let char_id = 90_000_001;
    let other_char = 90_000_002;
    let ship_a = 720_001_001;
    let ship_b = 720_001_002;
    let probe_id = 720_001_003;
    spawn_ship_ball(&ctx, sys_id, ship_a, char_id, 587, DVec3::ZERO, MovementMode::Stop);
    spawn_ship_ball(&ctx, sys_id, ship_b, other_char, 587, DVec3::new(1000.0, 0.0, 0.0), MovementMode::Stop);
    spawn_ship_ball(&ctx, sys_id, probe_id, other_char, 22778, DVec3::new(1000.0, 0.0, 0.0), MovementMode::Stop);

    let mut sess = make_session(char_id, 98_000_001, Some(sys_id), None, None, Some(ship_a));
    let b = bound_entry(char_id, "dogmaIM", sys_id as i64);

    let sh = ctx.systems.get_or_spawn(sys_id);
    // Clear spawn invuln after verifying TargetInvulnerable first
    let invuln_res = handle_dogma_im(&ctx, &mut sess, Some(&b), "dogmaIM", "AddTarget", &[PyRep::Integer(ship_b)], &HashMap::new()).await.unwrap();
    if let Err(ref e) = invuln_res {
        assert_eq!(e.code, "TargetInvulnerable");
    }
    sh.try_update_combat_state(ship_a, |cs| {
        cs.undock_invulnerable_remaining_ticks = 0;
        cs.warp_exit_invulnerable_remaining_ticks = 0;
        cs.gate_cloak_remaining_ticks = 0;
    });
    sh.try_update_combat_state(ship_b, |cs| {
        cs.undock_invulnerable_remaining_ticks = 0;
        cs.warp_exit_invulnerable_remaining_ticks = 0;
        cs.gate_cloak_remaining_ticks = 0;
        cs.is_tethered = true;
    });
    let res = handle_dogma_im(&ctx, &mut sess, Some(&b), "dogmaIM", "AddTarget", &[PyRep::Integer(ship_b)], &HashMap::new()).await.unwrap();
    let tethered_err = res.unwrap_err();
    assert_eq!(tethered_err.code, "TargetInvulnerable");
    assert!(tethered_err.message.contains("tethered"));

    sh.try_update_combat_state(ship_b, |cs| { cs.is_tethered = false; cs.is_cloaked = true; });
    let res = handle_dogma_im(&ctx, &mut sess, Some(&b), "dogmaIM", "AddTarget", &[PyRep::Integer(ship_b)], &HashMap::new()).await.unwrap();
    assert_eq!(res.unwrap_err().code, "TargetingFailed");

    sh.try_update_combat_state(ship_b, |cs| cs.is_cloaked = false);
    sh.try_update_combat_state(ship_a, |cs| cs.ecm_jammed_remaining_s = 5.0);
    let res = handle_dogma_im(&ctx, &mut sess, Some(&b), "dogmaIM", "AddTarget", &[PyRep::Integer(ship_b)], &HashMap::new()).await.unwrap();
    assert_eq!(res.unwrap_err().code, "ShipJammed");
    sh.try_update_combat_state(ship_a, |cs| cs.ecm_jammed_remaining_s = 0.0);

    let far_ship = 720_001_004;
    spawn_ship_ball(&ctx, sys_id, far_ship, other_char, 587, DVec3::new(500_000.0, 0.0, 0.0), MovementMode::Stop);
    let res = handle_dogma_im(&ctx, &mut sess, Some(&b), "dogmaIM", "AddTarget", &[PyRep::Integer(far_ship)], &HashMap::new()).await.unwrap();
    let range_err = res.unwrap_err();
    assert_eq!(range_err.code, "TargetingFailed");
    assert!(range_err.message.contains("maxTargetRange"));

    // Weapon linking & unlinking lifecycle
    let w1 = 720_002_001;
    let w2 = 720_002_002;
    let w3 = 720_002_003;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(w1, make_item(w1, 2873, char_id, ship_a, InventoryFlag::HiSlot0, 1, true));
        snap.items.insert(w2, make_item(w2, 2873, char_id, ship_a, InventoryFlag::HiSlot1, 1, true));
        snap.items.insert(w3, make_item(w3, 2873, char_id, ship_a, InventoryFlag::HiSlot2, 1, true));
    });
    assert!(handle_dogma_im(&ctx, &mut sess, Some(&b), "dogmaIM", "LinkWeapons", &[PyRep::Integer(ship_a), PyRep::Integer(w1), PyRep::Integer(w2)], &HashMap::new()).await.unwrap().is_ok());
    assert!(handle_dogma_im(&ctx, &mut sess, Some(&b), "dogmaIM", "MergeModuleGroups", &[PyRep::Integer(ship_a), PyRep::Integer(w1), PyRep::Integer(w3)], &HashMap::new()).await.unwrap().is_ok());
    assert!(handle_dogma_im(&ctx, &mut sess, Some(&b), "dogmaIM", "PeelAndLink", &[PyRep::Integer(ship_a), PyRep::Integer(w3), PyRep::Integer(w2)], &HashMap::new()).await.unwrap().is_ok());
    assert!(handle_dogma_im(&ctx, &mut sess, Some(&b), "dogmaIM", "UnlinkAllModules", &[PyRep::Integer(ship_a)], &HashMap::new()).await.unwrap().is_ok());

    let df_space = handle_dogma_im(&ctx, &mut sess, Some(&b), "dogmaIM", "DestroyFitting", &[PyRep::Integer(w1)], &HashMap::new()).await.unwrap();
    assert!(df_space.is_err());

    let stance_err = handle_dogma_im(&ctx, &mut sess, Some(&b), "shipStanceMgr", "SetShipStance", &[PyRep::Integer(ship_a), PyRep::Integer(99)], &HashMap::new()).await.unwrap();
    assert!(stance_err.is_err());

    assert!(handle_dogma_ops(&ctx, &mut sess, Some(&b), "dogmaIM", "QueryAttributeValue", &[PyRep::Integer(ship_a), PyRep::Integer(48)], &HashMap::new()).await.unwrap().is_ok());
    assert!(handle_dogma_ops(&ctx, &mut sess, Some(&b), "dogmaIM", "FullyDescribeAttribute", &[PyRep::Integer(ship_a), PyRep::Integer(48)], &HashMap::new()).await.unwrap().is_ok());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repackage_assemble_insurance_and_container_bay_guards() {
    let ctx = test_ctx();
    let char_id = 90_000_001;
    let corp_id = 98_000_001;
    let station_id = 60_003_760_i64;
    let active_ship = 730_001_001;
    let spare_ship = 730_001_002;
    let heated_mod = 730_001_003;
    let loaded_item = 730_001_004;
    let bpo_item = 730_001_005;
    let cargo_in_spare = 730_001_006;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(active_ship, make_item(active_ship, 587, char_id, station_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(spare_ship, make_item(spare_ship, 587, char_id, station_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(cargo_in_spare, make_item(cargo_in_spare, 34, char_id, spare_ship, InventoryFlag::Cargo, 5, false));
        let mut hm = make_item(heated_mod, 2873, char_id, station_id, InventoryFlag::Hangar, 1, true);
        hm.dynamic_attributes.insert(3, 4.0);
        snap.items.insert(heated_mod, hm);
        snap.items.insert(loaded_item, make_item(loaded_item, 2873, char_id, spare_ship, InventoryFlag::HiSlot0, 1, true));
        let mut bpo = make_item(bpo_item, 588, char_id, station_id, InventoryFlag::Hangar, 1, true);
        bpo.is_blueprint_copy = true;
        snap.items.insert(bpo_item, bpo);
    });
    let mut sess = make_session(char_id, corp_id, Some(30_000_142), Some(station_id), None, Some(active_ship));
    let b = bound_entry(char_id, "invbroker", station_id);

    for id in [active_ship, heated_mod, loaded_item, spare_ship, bpo_item] {
        let rep = handle_ships_and_services(
            &ctx, &mut sess, Some(&b), "repackagingSvc", "RepackageItems",
            &[PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(id), PyRep::Integer(station_id)])])],
            &HashMap::new(),
        ).await.unwrap();
        assert!(rep.is_err());
    }

    let pod_id = 730_002_001;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(pod_id, make_item(pod_id, 670, char_id, station_id, InventoryFlag::Hangar, 1, true));
    });
    let ins_err = handle_ships_and_services(
        &ctx, &mut sess, Some(&b), "insuranceSvc", "InsureShip",
        &[PyRep::Integer(pod_id), PyRep::Integer(10_000)],
        &HashMap::new(),
    ).await.unwrap().unwrap_err();
    assert!(!ins_err.code.is_empty());

    // Container and bay validation guards
    let moving_ship_id = 730_003_002;
    let cont_id = 730_003_010;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(moving_ship_id, make_item(moving_ship_id, 587, char_id, station_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(cont_id, make_item(cont_id, 17366, char_id, station_id, InventoryFlag::Hangar, 1, true));
    });
    write_container_ship_state(&ctx, |st| {
        st.alsc_locked_items.insert(moving_ship_id);
        st.container_passwords.insert(cont_id, "secret123".into());
    });

    let res_self = validate_container_and_bay_move(
        &ctx, char_id, moving_ship_id, moving_ship_id, InventoryFlag::Cargo as u16, &HashMap::new(), None,
    );
    assert_eq!(res_self.unwrap_err().message, "CannotStoreShipInItself");

    let res_alsc = validate_container_and_bay_move(
        &ctx, char_id, moving_ship_id, station_id, InventoryFlag::Hangar as u16, &HashMap::new(), None,
    );
    assert_eq!(res_alsc.unwrap_err().code, "ALSCItemLocked");

    write_container_ship_state(&ctx, |st| {
        st.alsc_locked_items.clear();
    });
    let res_pw = validate_container_and_bay_move(
        &ctx, char_id, moving_ship_id, cont_id, InventoryFlag::None as u16, &HashMap::new(), None,
    );
    assert_eq!(res_pw.unwrap_err().code, "ALSCPasswordNeededToOpen");
}

// ============================================================================
// DOMAIN 3: DOCKING, TETHER, ACL, CLOAK DELAY, WORMHOLES, CYNO & FILAMENTS
// ============================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cmd_dock_criminal_outlaw_structure_acl_size_supercap_and_perimeter_edges() {
    let ctx = test_ctx();
    let sys_id = 30_000_142;
    let low_sys = ctx
        .sde
        .solar_systems
        .iter()
        .filter(|(_, s)| s.security_status > 0.0 && s.security_status < 0.45)
        .map(|(&id, _)| id)
        .min()
        .expect("lowsec system in SDE");
    let char_id = 90_000_001;
    let corp_id = 98_000_001;
    let other_corp = 98_000_999;
    let ship_id = 800_001_001;
    let pod_id = 800_001_002;
    spawn_ship_ball(&ctx, sys_id, ship_id, char_id, 587, DVec3::ZERO, MovementMode::Stop);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(pod_id, make_item(pod_id, 670, char_id, sys_id as i64, InventoryFlag::None, 1, true));
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.security_status = -6.0;
        }
    });

    let mut sess = make_session(char_id, corp_id, Some(sys_id), None, None, Some(ship_id));
    let b = bound_entry(char_id, "beyonce", sys_id as i64);

    // Outlaw docking check: highsec non-pod refused, pod immune, lowsec allowed
    assert!(is_outlaw_ship_docking_refused(&ctx, sys_id, char_id, ship_id));
    assert!(!is_outlaw_ship_docking_refused(&ctx, sys_id, char_id, pod_id));
    assert!(!is_outlaw_ship_docking_refused(&ctx, low_sys, char_id, ship_id));
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.security_status = 5.0;
        }
    });

    assert!(validate_npc_station_docking(25, true).is_ok());
    assert!(validate_npc_station_docking(30, true).is_err());
    assert!(validate_npc_station_docking(659, true).is_err());

    sess.station_id = Some(60_003_760);
    let err = space_warp_jump::handle(&ctx, &mut sess, Some(&b), "beyonce", "CmdDock", &[PyRep::Integer(60_003_760)], &HashMap::new()).await.unwrap().unwrap_err();
    assert_eq!(err.code, "AlreadyDocked");
    sess.station_id = None;

    // Weapons timer blocks dock
    let now = ctx.now_unix();
    {
        let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
        cw.get_or_create(char_id).weapon_timer_until_unix = now + 60;
    }
    let err = space_warp_jump::handle(&ctx, &mut sess, Some(&b), "beyonce", "CmdDock", &[PyRep::Integer(60_003_760)], &HashMap::new()).await.unwrap().unwrap_err();
    assert!(!err.code.is_empty());

    // Highsec Criminal non-capsule blocked
    {
        let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
        let st = cw.get_or_create(char_id);
        st.weapon_timer_until_unix = 0;
        st.criminal_flag_until_unix = now + 600;
    }
    let err = space_warp_jump::handle(&ctx, &mut sess, Some(&b), "beyonce", "CmdDock", &[PyRep::Integer(60_003_760)], &HashMap::new()).await.unwrap().unwrap_err();
    assert!(!err.code.is_empty());
    {
        let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
        cw.get_or_create(char_id).criminal_flag_until_unix = 0;
    }

    // Structure ACL deny, Anchoring state deny, and Scrammed at structure
    let struct_id = 800_100_001;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(struct_id, make_structure(struct_id, 35832, other_corp, sys_id, "Anchoring", true, 100));
        snap.structure_docking_acl_denied.entry(struct_id).or_default().insert(char_id);
    });
    let acl_check = ctx.db.with_snapshot(|snap| verify_structure_docking_acl(&ctx, snap, struct_id, char_id));
    assert_eq!(acl_check.unwrap_err().code, "StructureDockingAccessDenied");

    let err_anchoring = space_warp_jump::handle(&ctx, &mut sess, Some(&b), "beyonce", "CmdDock", &[PyRep::Integer(struct_id)], &HashMap::new()).await.unwrap().unwrap_err();
    assert!(!err_anchoring.code.is_empty());

    ctx.db.with_mut_snapshot(|snap| {
        snap.structure_docking_acl_denied.clear();
        let st = snap.structures.get_mut(&struct_id).unwrap();
        st.owner_corp_id = corp_id;
        st.state = "ShieldVulnerable".into();
        st.position = [100.0, 0.0, 0.0];
    });
    ctx.systems.get_or_spawn(sys_id).try_update_combat_state(ship_id, |cs| cs.warp_scramble_strength = 2);
    let err_scram = space_warp_jump::handle(&ctx, &mut sess, Some(&b), "beyonce", "CmdDock", &[PyRep::Integer(struct_id)], &HashMap::new()).await.unwrap().unwrap_err();
    assert!(!err_scram.code.is_empty());

    let mut def = StructureDefenseSettings::default();
    def.record_attacker(char_id, now);
    def.prune_expired_attackers(now + 1_000);
    assert!(!def.known_attackers.contains(&char_id));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tether_sync_decloak_delay_and_immobilizing_module_edges() {
    let ctx = custom_ctx_with(|sde| {
        let base_eff_id = sde.effects.keys().min().copied().expect("seed effect");
        let mut warp_safe_eff = sde.effects[&base_eff_id].clone();
        warp_safe_eff.effect_id = 65_101;
        warp_safe_eff.name = "cloakingWarpSafe".into();
        warp_safe_eff.is_warp_safe = true;
        sde.effects.insert(65_101, warp_safe_eff);
        let mut covops_role_eff = sde.effects[&base_eff_id].clone();
        covops_role_eff.effect_id = 65_102;
        covops_role_eff.name = "covertOpsCloakCpuPercentBonus1".into();
        sde.effects.insert(65_102, covops_role_eff);

        let cloak_t2 = clone_inv_type(sde, 892_001, 330, 7, 5.0, 0.0, &[(1034, 5.0)], &[]);
        let covops_cloak = clone_inv_type(sde, 892_002, 330, 7, 5.0, 0.0, &[(1034, 0.0)], &[65_101]);
        let bomber = clone_inv_type(sde, 892_003, 834, 6, 1000.0, 100.0, &[(1035, 0.0)], &[65_102]);
        let frig = clone_inv_type(sde, 892_004, 25, 6, 1000.0, 100.0, &[], &[]);
        let mut bastion_attrs = Vec::new();
        for attr_name in ["siegeModeWarpStatus", "disallowDocking", "disallowTethering"] {
            if let Some(aid) = crate::services::space_navigation_advanced::find_attr_id_by_name(sde, attr_name) {
                bastion_attrs.push((aid, 1.0));
            }
        }
        let bastion_mod = clone_inv_type(sde, 892_010, 515, 7, 5.0, 0.0, &bastion_attrs, &[]);
        for t in [cloak_t2, covops_cloak, bomber, frig, bastion_mod] {
            sde.types.insert(t.type_id, t);
        }
    });

    let char_id = 90_000_001;
    let corp_id = 98_000_001;
    let sys_id = 30_000_142;
    let ship_id = 810_001_001;
    let struct_id = 810_100_001;

    // 1. Decloak targeting delay calculations
    assert_eq!(compute_decloak_targeting_delay_s(&ctx.sde, 892_004, 0, 5.0), 5.0);
    assert!(compute_decloak_targeting_delay_s(&ctx.sde, 892_004, 5, 5.0) < 5.0);
    assert_eq!(compute_decloak_targeting_delay_s(&ctx.sde, 892_003, 0, 5.0), 0.0);
    let mod_zero = compute_decloak_targeting_delay_for_module(&ctx, char_id, ship_id, 892_003, None, 892_002, 0);
    assert_eq!(mod_zero, 0.0);

    // 2. Immobilizing module active errors (Bastion)
    let mod_id = 810_002_001;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(mod_id, make_item(mod_id, 892_010, char_id, ship_id, InventoryFlag::HiSlot0, 1, true));
        snap.module_active_states.insert(mod_id);
    });
    assert!(check_immobilizing_module_active(&ctx, ship_id, "warp").is_err());
    assert!(check_immobilizing_module_active(&ctx, ship_id, "dock").is_err());

    // 3. Tether grant vs break on weapons timer
    spawn_ship_ball(&ctx, sys_id, ship_id, char_id, 892_004, DVec3::new(100.0, 0.0, 0.0), MovementMode::Stop);
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.remove(&mod_id);
        let mut st = make_structure(struct_id, 35832, corp_id, sys_id, "ShieldVulnerable", true, 500);
        st.position = [0.0, 0.0, 0.0];
        snap.structures.insert(struct_id, st);
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
        }
    });
    sync_tether(&ctx, char_id, sys_id, ship_id).await;
    let sh = ctx.systems.get_or_spawn(sys_id);
    assert!(sh.get_combat_state(ship_id).await.unwrap().unwrap().is_tethered);

    let now = ctx.now_unix();
    {
        let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
        cw.get_or_create(char_id).weapon_timer_until_unix = now + 30;
    }
    sync_tether(&ctx, char_id, sys_id, ship_id).await;
    assert!(!sh.get_combat_state(ship_id).await.unwrap().unwrap().is_tethered);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wormhole_stages_buckets_jump_filaments_and_structure_beacons_edges() {
    let ctx = test_ctx();
    let sys_id = 30_000_142;
    let char_id = 90_000_001;
    let corp_id = 98_000_001;
    let ship_id = 820_001_001;

    let now = 1_700_000_000_u64;
    let mut wh_rec = CosmicSignatureRecord {
        signature_id: "ABC-123".into(),
        solar_system_id: sys_id,
        sig_group: "Wormhole".into(),
        type_name: "K162".into(),
        position: [100.0, 0.0, 0.0],
        signal_strength: 1.0,
        wormhole_type_id: None,
        destination_system_id: Some(30_000_144),
        max_mass_kg: 1_000_000_000.0,
        remaining_mass_kg: 1_000_000_000.0,
        max_jump_mass_kg: 300_000_000.0,
        lifetime_seconds: 86_400 + 7_200,
        created_unix: now,
        collapsed: false,
    };

    // 1. Wormhole bucket & stage boundaries
    let b_healthy = derive_wormhole_buckets(&wh_rec, now, &ctx.sde);
    assert_eq!(b_healthy.mass_stability_bucket, ">50%");
    assert_eq!(b_healthy.lifetime_bucket, ">1 day");
    let b_cap = derive_wormhole_buckets_for_type(
        None, sys_id, 1_500_000_000.0, 50_000_000.0, 1_000_000_000.0, now, 86_400, false, now + 80_000, &ctx.sde,
    );
    assert_eq!(b_cap.mass_stability_bucket, "<10%");

    assert_eq!(compute_wormhole_stages(&wh_rec, now), ("Stable", 1.0, "Healthy", 0, false));
    wh_rec.remaining_mass_kg = 400_000_000.0;
    assert_eq!(compute_wormhole_stages(&wh_rec, now + 10_000), ("Destabilized", 0.5, "BeginningToDecay", 1, false));
    wh_rec.remaining_mass_kg = 50_000_000.0;
    assert_eq!(compute_wormhole_stages(&wh_rec, now + 80_000), ("Critical", 0.1, "ReachingEndOfLifetime", 2, false));
    wh_rec.remaining_mass_kg = 0.0;
    assert_eq!(compute_wormhole_stages(&wh_rec, now + 100_000), ("Collapsed_Exhausted", 0.0, "Collapsed_Expired", 3, true));

    // 2. WormholeJump range error
    spawn_ship_ball(&ctx, sys_id, ship_id, char_id, 587, DVec3::new(50_000.0, 0.0, 0.0), MovementMode::Stop);
    let mut sess = make_session(char_id, corp_id, Some(sys_id), None, None, Some(ship_id));
    let b = bound_entry(char_id, "beyonce", sys_id as i64);
    let wh_sig_item = 880_000_000_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.cosmic_signatures.insert(
            sys_id,
            vec![CosmicSignatureRecord {
                signature_id: "WH-001".into(),
                solar_system_id: sys_id,
                sig_group: "Wormhole".into(),
                type_name: "C1".into(),
                position: [0.0, 0.0, 0.0],
                signal_strength: 1.0,
                wormhole_type_id: None,
                destination_system_id: Some(30_000_144),
                max_mass_kg: 1_000_000_000.0,
                remaining_mass_kg: 100.0,
                max_jump_mass_kg: 300_000_000.0,
                lifetime_seconds: 86_400,
                created_unix: ctx.now_unix(),
                collapsed: false,
            }],
        );
    });
    let err_range = space_cyno_filaments::handle(
        &ctx, &mut sess, Some(&b), "wormholeMgr", "WormholeJump",
        &[PyRep::Integer(wh_sig_item)], &HashMap::new(),
    ).await.unwrap().unwrap_err();
    assert!(!err_range.code.is_empty());

    // 3. ActivateRandomJumpFilament: in-warp & scrambled rejections
    let ctx_fil = custom_ctx_with(|sde| {
        let fil_t = clone_inv_type(sde, 899_001, 1984, 30, 0.1, 0.0, &[(3025, 1.0), (2832, 5.0)], &[]);
        sde.types.insert(fil_t.type_id, fil_t);
    });
    let fil_ship = 820_001_099;
    let fil_id = 820_002_001;
    spawn_ship_ball(
        &ctx_fil,
        sys_id,
        fil_ship,
        char_id,
        587,
        DVec3::new(1e9, 0.0, 0.0),
        MovementMode::new_warp(DVec3::ZERO, DVec3::new(1e10, 0.0, 0.0), 3.0),
    );
    ctx_fil.db.with_mut_snapshot(|snap| {
        snap.items.insert(fil_id, make_item(fil_id, 899_001, char_id, fil_ship, InventoryFlag::Cargo, 1, false));
    });
    let mut sess_fil = make_session(char_id, corp_id, Some(sys_id), None, None, Some(fil_ship));
    let err_warp = space_cyno_filaments::handle(
        &ctx_fil, &mut sess_fil, Some(&b), "randomJumpMgr", "ActivateRandomJumpFilament",
        &[PyRep::Integer(fil_id)], &HashMap::new(),
    ).await.unwrap().unwrap_err();
    assert!(!err_warp.code.is_empty());

    ctx_fil.systems.get_or_spawn(sys_id).try_update_combat_state(fil_ship, |cs| cs.warp_scramble_strength = 1);
    let err_scram = space_cyno_filaments::handle(
        &ctx_fil, &mut sess_fil, Some(&b), "randomJumpMgr", "ActivateRandomJumpFilament",
        &[PyRep::Integer(fil_id)], &HashMap::new(),
    ).await.unwrap().unwrap_err();
    assert!(!err_scram.code.is_empty());

    // 4. CmdJumpToStructureBeacon missing / invalid target structure
    let err_beacon = space_cyno_filaments::handle(
        &ctx, &mut sess, Some(&b), "structureCynoBeaconMgr", "CmdJumpToStructureBeacon",
        &[PyRep::Integer(999_999_111)], &HashMap::new(),
    ).await.unwrap().unwrap_err();
    assert!(!err_beacon.code.is_empty());
}
