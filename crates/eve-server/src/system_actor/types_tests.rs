//! Unit and mutation-killing tests for `system_actor::types`.

use super::{
    ActiveModuleCycle, ActiveNaniteRepair, AsteroidState, ClusterNodeRegistry,
    MiningExtractionResult, PendingAnchoringDisruptor, PendingCycleAction, SystemActorState,
    SystemCommand, TickBroadcast, DEFAULT_NODE_LOAD_TIDI_THRESHOLD,
};
use glam::DVec3;
use tokio::sync::oneshot;

#[test]
fn cluster_node_registry_default_values_are_exact() {
    let reg = ClusterNodeRegistry::default();
    assert_eq!(reg.worker_nodes, vec![1, 2, 3, 4]);
    assert!(reg.system_node_overrides.is_empty());
    assert!(reg.reinforced_nodes.is_empty());
    assert!(reg.manual_tidi_overrides.is_empty());
    assert!(reg.sim_clock_anchoring.is_empty());
    assert_eq!(DEFAULT_NODE_LOAD_TIDI_THRESHOLD, 80);
    assert_eq!(reg.node_load_tidi_threshold, 80);
    assert_eq!(reg.next_dedicated_node_id, 100);
    assert!(!reg.auto_tidi_enabled);
    assert!(reg.scheduled_downtime_enabled);

    let cloned = reg.clone();
    assert_eq!(cloned.worker_nodes, vec![1, 2, 3, 4]);
    assert!(format!("{cloned:?}").contains("ClusterNodeRegistry"));
}

#[test]
fn asteroid_state_and_mining_extraction_result_serde_roundtrip() {
    let ast = AsteroidState {
        asteroid_id: 70_001,
        belt_id: 40_009_050,
        type_id: 1228, // Scordite
        initial_units: 20_000,
        remaining_units: 15_000,
        unit_volume: 0.15,
        depleted: false,
        respawn_at_tick: Some(500),
    };
    let json_str = serde_json::to_string(&ast).expect("serialize AsteroidState");
    let decoded: AsteroidState =
        serde_json::from_str(&json_str).expect("deserialize AsteroidState");
    assert_eq!(decoded.asteroid_id, ast.asteroid_id);
    assert_eq!(decoded.belt_id, ast.belt_id);
    assert_eq!(decoded.type_id, ast.type_id);
    assert_eq!(decoded.initial_units, ast.initial_units);
    assert_eq!(decoded.remaining_units, ast.remaining_units);
    assert!((decoded.unit_volume - ast.unit_volume).abs() < 1e-9);
    assert_eq!(decoded.depleted, ast.depleted);
    assert_eq!(decoded.respawn_at_tick, ast.respawn_at_tick);

    let ext = MiningExtractionResult {
        mined_units: 450,
        wasted_units: 50,
        remaining_units: 14_500,
        ore_type_id: 1228,
        depleted: false,
    };
    let ext_json = serde_json::to_string(&ext).expect("serialize MiningExtractionResult");
    let ext_decoded: MiningExtractionResult =
        serde_json::from_str(&ext_json).expect("deserialize MiningExtractionResult");
    assert_eq!(ext_decoded.mined_units, ext.mined_units);
    assert_eq!(ext_decoded.wasted_units, ext.wasted_units);
    assert_eq!(ext_decoded.remaining_units, ext.remaining_units);
    assert_eq!(ext_decoded.ore_type_id, ext.ore_type_id);
    assert_eq!(ext_decoded.depleted, ext.depleted);
}

#[test]
fn active_module_cycle_and_system_actor_state_defaults_and_variants() {
    let cycle = ActiveModuleCycle::default();
    assert_eq!(cycle.ship_id, 0);
    assert_eq!(cycle.module_id, 0);
    assert_eq!(cycle.module_type_id, 0);
    assert_eq!(cycle.owner_char_id, None);
    assert_eq!(cycle.target_id, None);
    assert_eq!(cycle.duration_s, 0.0);
    assert_eq!(cycle.elapsed_s, 0.0);
    assert_eq!(cycle.cap_need_per_cycle, 0.0);
    assert_eq!(cycle.repeat, 0);
    assert!(!cycle.is_overloaded);
    assert!(cycle.can_stay_active_in_warp_check_helper());

    let state = SystemActorState::default();
    assert_eq!(state.sim_elapsed_secs, 0.0);
    assert!(state.asteroids.is_empty());
    assert!(state.active_module_cycles.is_empty());
    assert!(state.pending_cycle_actions.is_empty());
    assert!(state.active_nanite_repairs.is_empty());
    assert!(state.rah_incoming_damage.is_empty());
    assert!(state.pending_anchoring_disruptors.is_empty());
    assert!(state.ship_rack_heat.is_empty());

    let act1 = PendingCycleAction::SyncOverloadState {
        char_id: Some(30),
        ship_id: 10,
        module_id: 20,
        is_overloaded: true,
    };
    let act2 = PendingCycleAction::InvalidateShipFitting {
        char_id: Some(30),
        ship_id: 10,
    };
    assert!(format!("{act1:?}").contains("SyncOverloadState"));
    assert!(format!("{act2:?}").contains("InvalidateShipFitting"));

    let nanite = ActiveNaniteRepair {
        char_id: 30,
        ship_id: 10,
        module_id: 20,
        initial_damage_hp: 40.0,
        paste_total_needed: 8,
        paste_consumed_so_far: 3,
        duration_s: 60.0,
        elapsed_s: 15.0,
    };
    assert_eq!(nanite.clone().paste_total_needed, 8);
    assert_eq!(nanite.paste_consumed_so_far, 3);

    let anchor = PendingAnchoringDisruptor {
        id: 100,
        type_id: 12_198,
        position: DVec3::new(100.0, 200.0, 300.0),
        radius: 15_000.0,
        remaining_s: 45.0,
    };
    assert_eq!(anchor.clone().type_id, 12_198);

    let tb = TickBroadcast {
        solar_system_id: 30_000_142,
        tick: 99,
        events: vec![],
        observer_events: std::collections::BTreeMap::new(),
        observer_updates_pyrep: std::collections::BTreeMap::new(),
    };
    assert_eq!(tb.clone().tick, 99);

    let (tx, _rx) = oneshot::channel();
    let cmd = SystemCommand::StopNonWarpModuleCycles {
        ship_id: 10,
        reply: tx,
    };
    match cmd {
        SystemCommand::StopNonWarpModuleCycles { ship_id, .. } => assert_eq!(ship_id, 10),
        _ => panic!("expected StopNonWarpModuleCycles"),
    }
}

trait CycleCheckExt {
    fn can_stay_active_in_warp_check_helper(&self) -> bool;
}

impl CycleCheckExt for ActiveModuleCycle {
    fn can_stay_active_in_warp_check_helper(&self) -> bool {
        !self.is_mwd_or_mjd && self.current_spool_cycles == 0 && self.mutadaptive_spool_step == 0.0
    }
}
