use super::*;
use eve_destiny::{Ball, CombatState, MovementMode, SlimItemMeta};
use eve_sde::{InvType, SdeRegistry};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

fn make_ship_ball(id: BallId, type_id: u32, pos: DVec3) -> Ball {
    Ball {
        id,
        type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 350.0,
        inertia: 2.5,
        mass: 1_200_000.0,
        radius: 40.0,
        speed_fraction: 1.0,
        mode: MovementMode::Stop,
    }
}

fn make_sde_with_sensors_and_wreck() -> Arc<SdeRegistry> {
    let mut sde = SdeRegistry::with_canonical_seed();
    let mut attrs = HashMap::new();
    // Gravimetric sensor strength = 12.0 (attr 211)
    attrs.insert(211u16, 12.0);
    sde.types.insert(
        587,
        InvType {
            type_id: 587,
            group_id: 25,
            category_id: 6,
            name: "Rifter".into(),
            mass: 1_067_000.0,
            volume: 27_000.0,
            capacity: 140.0,
            portion_size: 1,
            radius: 39.0,
            base_price: 250_000.0,
            published: true,
            market_group_id: None,
            race_id: Some(2),
            faction_id: None,
            variation_parent_type_id: None,
            description_id: None,
            icon_id: None,
            graphic_id: None,
            sound_id: None,
            meta_group_id: None,
            meta_level: None,
            tech_level: Some(1),
            default_effect_id: None,
            description: String::new(),
            effect_ids: Vec::new(),
            default_attributes: attrs,
        },
    );
    sde.types.insert(
        26469,
        InvType {
            type_id: 26469,
            group_id: 186,
            category_id: 2,
            name: "Minmatar Frigate Wreck".into(),
            mass: 50_000.0,
            volume: 500.0,
            capacity: 500.0,
            portion_size: 1,
            radius: 75.0,
            base_price: 0.0,
            published: false,
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
            default_effect_id: None,
            description: String::new(),
            effect_ids: Vec::new(),
            default_attributes: HashMap::new(),
        },
    );
    Arc::new(sde)
}

#[test]
fn schedule_mobile_warp_disruptor_creates_and_replaces_ball_with_clamped_delay() {
    let mut bp = Ballpark::new(30_000_142);
    let mut pending = Vec::new();
    let pos1 = DVec3::new(10_000.0, 2_000.0, -5_000.0);

    // Delay below 1.0 is clamped to 1.0s
    let ok1 = handle_schedule_mobile_warp_disruptor(
        &mut bp,
        &mut pending,
        9_001,
        12_198,
        pos1,
        20_000.0,
        0.25,
    );
    assert!(ok1);
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].id, 9_001);
    assert_eq!(pending[0].type_id, 12_198);
    assert_eq!(pending[0].position, pos1);
    assert!((pending[0].radius - 20_000.0).abs() < 1e-6);
    assert!((pending[0].remaining_s - 1.0).abs() < 1e-6);
    let ball = bp.balls.get(&9_001).expect("disruptor ball must exist");
    assert!((ball.radius - 20_000.0).abs() < 1e-6);
    assert_eq!(ball.position, pos1);

    // Scheduling same ball ID replaces previous ball and records longer delay
    let pos2 = DVec3::new(-3_000.0, 0.0, 4_000.0);
    let ok2 = handle_schedule_mobile_warp_disruptor(
        &mut bp,
        &mut pending,
        9_001,
        12_199,
        pos2,
        25_000.0,
        45.0,
    );
    assert!(ok2);
    assert_eq!(pending.len(), 2);
    assert!((pending[1].remaining_s - 45.0).abs() < 1e-6);
    let replaced = bp.balls.get(&9_001).expect("replaced ball must exist");
    assert_eq!(replaced.type_id, 12_199);
    assert_eq!(replaced.position, pos2);
    assert!((replaced.radius - 25_000.0).abs() < 1e-6);
}

#[test]
fn apply_ecm_burst_filters_immune_cloaked_out_of_range_and_non_ships() {
    let sde = Some(make_sde_with_sensors_and_wreck());
    let mut bp = Ballpark::new(30_000_142);

    // 100: attacker at origin
    bp.add_ball(make_ship_ball(100, 587, DVec3::ZERO));
    bp.combat_states.insert(100, CombatState::default());

    // 101: valid target within 25km (gravimetric 12.0 vs jam 24.0, guaranteed hit roll 0.1)
    bp.add_ball(make_ship_ball(101, 587, DVec3::new(10_000.0, 0.0, 0.0)));
    bp.combat_states.insert(101, CombatState::default());
    assert!(bp.add_target_lock(
        101,
        eve_destiny::TargetLock {
            target_id: 100,
            lock_time_ms: 1_000.0,
            remaining_lock_ms: 0.0,
            locked: true,
            max_range_m: None,
        },
    ));

    // 102: ewar-immune ship (Bastion active) within range -> must NOT be jammed
    bp.add_ball(make_ship_ball(102, 587, DVec3::new(5_000.0, 0.0, 0.0)));
    bp.combat_states.insert(102, CombatState::default());

    // 103: cloaked ship (slim meta + combat state) within range -> must NOT be jammed
    bp.add_ball(make_ship_ball(103, 587, DVec3::new(4_000.0, 0.0, 0.0)));
    bp.combat_states.insert(
        103,
        CombatState {
            is_cloaked: true,
            ..CombatState::default()
        },
    );
    bp.ball_slim_meta.insert(
        103,
        SlimItemMeta {
            is_cloaked: true,
            ..SlimItemMeta::default()
        },
    );

    // 104: ship outside burst radius (40km > 25km) -> must NOT be jammed
    bp.add_ball(make_ship_ball(104, 587, DVec3::new(40_000.0, 0.0, 0.0)));
    bp.combat_states.insert(104, CombatState::default());

    // 105: non-ship wreck inside range -> must NOT be jammed
    bp.add_ball(make_ship_ball(105, 26469, DVec3::new(3_000.0, 0.0, 0.0)));
    bp.combat_states.insert(105, CombatState::default());

    // 106: ship with high sensor strength via override (sensor=100, jam=12, roll=0.95 -> fails jam)
    bp.add_ball(make_ship_ball(106, 587, DVec3::new(6_000.0, 0.0, 0.0)));
    bp.combat_states.insert(106, CombatState::default());

    let cycles = vec![ActiveModuleCycle {
        ship_id: 102,
        module_id: 5001,
        self_ewar_immune: true,
        duration_s: 60.0,
        ..ActiveModuleCycle::default()
    }];

    let mut overrides = HashMap::new();
    overrides.insert(106i64, (3usize, 100.0_f64, 0.95_f64));

    let jammed = handle_apply_ecm_burst(
        &mut bp,
        &cycles,
        &sde,
        &None,
        100,
        25_000.0,
        [24.0, 24.0, 24.0, 24.0],
        0.0,
        0.10,
        Some(overrides),
    );

    assert_eq!(jammed, vec![101]);
    assert!(
        bp.get_target_locks(101).is_empty(),
        "ECM burst must break locks on jammed target 101"
    );
    assert!(
        bp.combat_states.get(&102).is_some_and(|c| c.ecm_jammed_remaining_s <= 0.0),
        "ewar immune ship must remain unjammed"
    );
    assert!(
        bp.combat_states.get(&106).is_some_and(|c| c.ecm_jammed_remaining_s <= 0.0),
        "ship with high sensor override must resist ECM burst"
    );
}

#[test]
fn apply_ecm_burst_uses_seeded_rng_when_roll_out_of_unit_interval() {
    let sde = Some(make_sde_with_sensors_and_wreck());
    let rng = Some(Arc::new(crate::rng::MechanicRng::seeded(42)));
    let mut bp = Ballpark::new(30_000_142);

    bp.add_ball(make_ship_ball(200, 587, DVec3::ZERO));
    bp.combat_states.insert(200, CombatState::default());

    bp.add_ball(make_ship_ball(201, 587, DVec3::new(8_000.0, 0.0, 0.0)));
    bp.combat_states.insert(
        201,
        CombatState {
            ecm_jammed_remaining_s: 2.0,
            ..CombatState::default()
        },
    );
    bp.add_ball(make_ship_ball(202, 587, DVec3::new(9_000.0, 0.0, 0.0)));
    bp.combat_states.insert(
        202,
        CombatState {
            ecm_jammed_remaining_s: 12.0,
            ..CombatState::default()
        },
    );

    // First candidate (201) evaluates with explicit roll=0.5 (jammed), second (202) draws from mechanic_rng_opt
    let jammed = handle_apply_ecm_burst(
        &mut bp,
        &[],
        &sde,
        &rng,
        200,
        25_000.0,
        [5_000.0, 5_000.0, 5_000.0, 5_000.0],
        12.0,
        0.5,
        None,
    );

    assert!(jammed.contains(&201) && jammed.contains(&202));
    let rem = bp
        .combat_states
        .get(&202)
        .map(|c| c.ecm_jammed_remaining_s)
        .unwrap_or(0.0);
    assert!(rem >= 12.0, "expected jammed duration >= 12s, got {rem}");
}

#[tokio::test]
async fn try_remove_module_cycle_removes_matching_cycle_and_keeps_others() {
    let reg = crate::system_actor::SystemRegistry::new();
    let handle = reg.get_or_spawn(30_000_142);

    {
        let mut st = handle.actor_state.write().unwrap();
        st.active_module_cycles.push(ActiveModuleCycle {
            ship_id: 300,
            module_id: 701,
            elapsed_s: 4.5,
            duration_s: 10.0,
            ..ActiveModuleCycle::default()
        });
        st.active_module_cycles.push(ActiveModuleCycle {
            ship_id: 300,
            module_id: 702,
            elapsed_s: 1.0,
            duration_s: 10.0,
            ..ActiveModuleCycle::default()
        });
    }

    handle.try_remove_module_cycle(300, 701);
    handle.try_remove_module_cycle(300, 701);
    let st = handle.actor_state.read().unwrap();
    assert_eq!(st.active_module_cycles.len(), 1);
    assert_eq!(st.active_module_cycles[0].module_id, 702);
}
