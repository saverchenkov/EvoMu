#![forbid(unsafe_code)]

use super::*;
use eve_db::{InventoryFlag, InventoryItemRecord, VirtualServerClock, WorldRepository};
use eve_destiny::{Ball, Ballpark, CombatState, MovementMode};
use eve_sde::{InvType, SdeRegistry};
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
    name: &str,
    attrs: &[(u16, f64)],
) -> InvType {
    let mut t = sde.types.get(&587).expect("seed type").clone();
    t.type_id = type_id;
    t.group_id = group_id;
    t.category_id = category_id;
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
fn siege_triage_strontium_and_nullifier_scaling_formulas() {
    assert_eq!(compute_siege_strontium_cost(0), 250);
    assert_eq!(compute_siege_strontium_cost(3), 175);
    assert_eq!(compute_siege_strontium_cost(5), 125);
    assert_eq!(compute_siege_strontium_cost(9), 125);

    assert_eq!(compute_triage_strontium_cost(0), 200);
    assert_eq!(compute_triage_strontium_cost(3), 125);
    assert_eq!(compute_triage_strontium_cost(5), 75);
    assert_eq!(compute_triage_strontium_cost(9), 75);

    let (dur0, cd0) = compute_nullifier_duration_and_cooldown(10.0, 150.0, 0);
    assert!((dur0 - 10.0).abs() < 1e-9);
    assert!((cd0 - 150.0).abs() < 1e-9);

    let (dur5, cd5) = compute_nullifier_duration_and_cooldown(10.0, 150.0, 5);
    assert!((dur5 - 15.0).abs() < 1e-9);
    assert!((cd5 - 75.0).abs() < 1e-9);
}

#[test]
fn combatant_and_player_ship_ball_classification() {
    let ctx = test_ctx_with_custom(|sde| {
        sde.types.insert(992_001, custom_inv_type(sde, 992_001, 25, 6, "Rifter", &[]));
        sde.types.insert(992_002, custom_inv_type(sde, 992_002, 186, 2, "Wreck", &[]));
        sde.types.insert(992_003, custom_inv_type(sde, 992_003, 549, 18, "Combat Drone", &[]));
    });
    let sde_opt = Some(&ctx.sde);
    let db_opt = Some(&ctx.db);

    // Celestial IDs (40M..80M) are never combatants or player ships
    assert!(!is_combatant_ship_or_entity_ball(sde_opt, db_opt, 40_000_001, 992_001));
    assert!(!is_player_ship_ball(sde_opt, db_opt, 40_000_001, 992_001));

    // Player ship (category 6)
    assert!(is_combatant_ship_or_entity_ball(sde_opt, db_opt, 1_000_952_001, 992_001));
    assert!(is_player_ship_ball(sde_opt, db_opt, 1_000_952_001, 992_001));

    // Wreck (group 186) is excluded
    assert!(!is_combatant_ship_or_entity_ball(sde_opt, db_opt, 1_000_952_002, 992_002));
    assert!(!is_player_ship_ball(sde_opt, db_opt, 1_000_952_002, 992_002));

    // Drone (category 18) is combatant entity, not player ship
    assert!(is_combatant_ship_or_entity_ball(sde_opt, db_opt, 1_000_952_003, 992_003));
    assert!(!is_player_ship_ball(sde_opt, db_opt, 1_000_952_003, 992_003));

    // Fallback to DB item lookup when ball_type_id == 0
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            1_000_952_004,
            InventoryItemRecord {
                item_id: 1_000_952_004,
                type_id: 992_001,
                owner_id: 95_200_001,
                location_id: 30_000_142,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    assert!(is_player_ship_ball(sde_opt, db_opt, 1_000_952_004, 0));
}

#[test]
fn wubble_probe_registration_detonation_debuff_application_and_expiry() {
    let ctx = test_ctx_with_custom(|sde| {
        sde.types.insert(
            56748,
            custom_inv_type(
                sde,
                56748,
                548,
                8,
                "Stasis Webification Probe",
                &[(20, -30.0), (2262, 2_000.0), (2279, 15_000.0), (2280, 10_000.0)],
            ),
        );
    });

    let sys_id = 30_002_505u32;
    let probe_id = 1_000_952_101;
    let victim_ship_id = 1_000_952_102;
    let far_ship_id = 1_000_952_103;

    let mut ballpark = Ballpark::new(sys_id);
    ballpark.add_ball(Ball {
        id: probe_id,
        type_id: 56748,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 0.0,
        inertia: 1.0,
        mass: 100.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    });
    ballpark.combat_states.insert(probe_id, CombatState::default());

    ballpark.add_ball(Ball {
        id: victim_ship_id,
        type_id: 587,
        position: DVec3::new(5_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    });
    ballpark.add_ball(Ball {
        id: far_ship_id,
        type_id: 587,
        position: DVec3::new(50_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    });

    register_launched_wubble_probe(&ctx, probe_id, 56748, sys_id, DVec3::ZERO, 10_000.0);
    let sde_opt = Some(Arc::clone(&ctx.sde));
    let db_opt = Some(Arc::clone(&ctx.db));

    // Step 1.0s (< 2.0s arming): probe still arming, no debuffs applied yet
    step_wubble_probes_and_debuffs(&mut ballpark, &sde_opt, &db_opt, &ctx.tactical_state, 1.0);
    assert!(active_wubble_multipliers_for_ballpark(&ballpark).is_empty());
    assert!(ballpark.balls.contains_key(&probe_id));

    // Step another 1.5s: probe detonates, applies 0.70 velocity multiplier to victim within 15km, despawns probe ball
    step_wubble_probes_and_debuffs(&mut ballpark, &sde_opt, &db_opt, &ctx.tactical_state, 1.5);
    let mults = active_wubble_multipliers_for_ballpark(&ballpark);
    assert!((mults.get(&victim_ship_id).copied().unwrap_or(0.0) - 0.70).abs() < 1e-6);
    assert!(!mults.contains_key(&far_ship_id));
    assert!(!ballpark.balls.contains_key(&probe_id));

    // Step 11.0s (> 10.0s debuff duration): debuff expires cleanly
    step_wubble_probes_and_debuffs(&mut ballpark, &sde_opt, &db_opt, &ctx.tactical_state, 11.0);
    assert!(active_wubble_multipliers_for_ballpark(&ballpark).is_empty());
}
