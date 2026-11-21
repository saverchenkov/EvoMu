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
        broadcast_seq: 0,
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

#[test]
fn pending_cycle_action_all_variants_clone_and_debug() {
    let salvage = PendingCycleAction::SalvageEnd {
        char_id: 90_000_001,
        ship_id: 1001,
        module_id: 2001,
        module_type_id: 25861,
        wreck_id: 3001,
        repeat: 2,
        effect_name: "salvaging".into(),
        next_cycle_cap_deducted: true,
        deducted_cap_gj: 12.5,
    };
    let s_clone = salvage.clone();
    match s_clone {
        PendingCycleAction::SalvageEnd {
            char_id,
            ship_id,
            module_id,
            module_type_id,
            wreck_id,
            repeat,
            effect_name,
            next_cycle_cap_deducted,
            deducted_cap_gj,
        } => {
            assert_eq!(char_id, 90_000_001);
            assert_eq!(ship_id, 1001);
            assert_eq!(module_id, 2001);
            assert_eq!(module_type_id, 25861);
            assert_eq!(wreck_id, 3001);
            assert_eq!(repeat, 2);
            assert_eq!(effect_name, "salvaging");
            assert!(next_cycle_cap_deducted);
            assert!((deducted_cap_gj - 12.5).abs() < 1e-9);
        }
        _ => panic!("expected SalvageEnd"),
    }
    assert!(format!("{salvage:?}").contains("SalvageEnd"));

    let lance = PendingCycleAction::LanceBeamFired {
        attacker_char_id: 90_000_002,
        attacker_ship_id: 1002,
        attacker_ship_type_id: 3764,
        module_id: 2002,
        module_type_id: 41492,
        hits: vec![(3002, 5000.0), (3003, 7500.0)],
    };
    let l_clone = lance.clone();
    match l_clone {
        PendingCycleAction::LanceBeamFired {
            attacker_char_id,
            attacker_ship_id,
            attacker_ship_type_id,
            module_id,
            module_type_id,
            hits,
        } => {
            assert_eq!(attacker_char_id, 90_000_002);
            assert_eq!(attacker_ship_id, 1002);
            assert_eq!(attacker_ship_type_id, 3764);
            assert_eq!(module_id, 2002);
            assert_eq!(module_type_id, 41492);
            assert_eq!(hits.len(), 2);
            assert_eq!(hits[0].0, 3002);
            assert!((hits[1].1 - 7500.0).abs() < 1e-9);
        }
        _ => panic!("expected LanceBeamFired"),
    }
    assert!(format!("{lance:?}").contains("LanceBeamFired"));

    let repeat = PendingCycleAction::RepeatCycleStart {
        char_id: 90_000_003,
        ship_id: 1003,
        module_id: 2003,
        module_type_id: 526,
        target_id: Some(3004),
        duration_s: 5.0,
        is_overloaded: true,
        effect_name: "webify".into(),
    };
    let r_clone = repeat.clone();
    match r_clone {
        PendingCycleAction::RepeatCycleStart {
            char_id,
            ship_id,
            module_id,
            module_type_id,
            target_id,
            duration_s,
            is_overloaded,
            effect_name,
        } => {
            assert_eq!(char_id, 90_000_003);
            assert_eq!(ship_id, 1003);
            assert_eq!(module_id, 2003);
            assert_eq!(module_type_id, 526);
            assert_eq!(target_id, Some(3004));
            assert!((duration_s - 5.0).abs() < 1e-9);
            assert!(is_overloaded);
            assert_eq!(effect_name, "webify");
        }
        _ => panic!("expected RepeatCycleStart"),
    }
    assert!(format!("{repeat:?}").contains("RepeatCycleStart"));
}

#[test]
fn active_module_cycle_and_state_extended_default_fields() {
    let c = ActiveModuleCycle::default();
    assert!(c.linked_turrets.is_empty());
    assert_eq!(c.ammo_type_id, None);
    assert_eq!(c.damage_vector.total(), 0.0);
    assert_eq!(c.cap_drain_target, 0.0);
    assert_eq!(c.cap_transfer_target, 0.0);
    assert!(!c.nosferatu_blood_raider_override);
    assert_eq!(c.shield_boost_amount, 0.0);
    assert_eq!(c.armor_repair_amount, 0.0);
    assert_eq!(c.hull_repair_amount, 0.0);
    assert_eq!(c.remote_shield_amount, 0.0);
    assert_eq!(c.remote_armor_amount, 0.0);
    assert_eq!(c.remote_hull_amount, 0.0);
    assert_eq!(c.remote_cap_amount, 0.0);
    assert_eq!(c.remote_optimal, 0.0);
    assert_eq!(c.remote_falloff, 0.0);
    assert_eq!(c.mutadaptive_spool_step, 0.0);
    assert_eq!(c.mutadaptive_spool_max, 0.0);
    assert_eq!(c.current_spool_cycles, 0);
    assert_eq!(c.cap_booster_inject_amount, 0.0);
    assert!(!c.is_asb);
    assert_eq!(c.asb_fallback_cap_need, 0.0);
    assert!(!c.is_aar);
    assert_eq!(c.aar_paste_rate, 0);
    assert_eq!(c.aar_base_armor_repair, 0.0);
    assert!(!c.is_rah);
    assert_eq!(c.rah_shift_amount, 0.0);
    assert!(!c.is_adc);
    assert_eq!(c.ehe_hull_resists, None);
    assert_eq!(c.target_velocity_multiplier, None);
    assert!(!c.immobilize_self);
    assert!(!c.self_disallow_assistance);
    assert!(!c.self_ewar_immune);
    assert!(!c.self_disable_mwd_mjd);
    assert_eq!(c.self_mass_multiplier, None);
    assert!(!c.is_auto_targeter);
    assert_eq!(c.auto_target_max_range, 0.0);
    assert_eq!(c.auto_target_max_locks, 0);
    assert_eq!(c.auto_target_scan_res, 0.0);
    assert_eq!(c.crystal_volatility_chance, 0.0);
    assert_eq!(c.crystal_volatility_damage, 0.0);
    assert_eq!(c.pending_overloaded, None);
    assert_eq!(c.rack_idx, 0);
    assert_eq!(c.heat_damage_per_cycle, 0.0);
    assert_eq!(c.reactivation_delay_s, 0.0);
    assert_eq!(c.warp_scramble_points, 0);
    assert!(!c.disables_target_mwd_mjd);
    assert_eq!(c.warp_core_stab_bonus, 0);
    assert_eq!(c.hic_bubble_radius_m, None);
    assert_eq!(c.hic_velocity_multiplier, None);
    assert_eq!(c.hic_sig_multiplier, None);
    assert_eq!(c.hic_mass_addition, None);
    assert_eq!(c.mjd_jump_distance_m, None);
    assert_eq!(c.mjfg_field_params, None);
    assert!(!c.grants_interdiction_nullification);
    assert_eq!(c.effective_max_capacitor, None);
    assert_eq!(c.max_effective_range_m, None);
    assert_eq!(c.lance_beam_dir, None);
    assert_eq!(c.grappler_optimal_m, 0.0);
    assert_eq!(c.grappler_falloff_m, 0.0);
    assert_eq!(c.grappler_speed_factor_pct, 0.0);
    assert_eq!(c.started_at_unix, 0);

    let st = SystemActorState::default();
    assert!(st.base_scramble_strengths.is_empty());
    assert!(st.base_core_strengths.is_empty());
    assert!(st.asteroid_templates.is_empty());
    assert!(st.sde_opt.is_none());
    assert!(st.db_opt.is_none());
    assert!(st.mechanic_rng_opt.is_none());
    assert!(st.pending_notifications_opt.is_none());
    assert!(st.notify_tx_opt.is_none());
}

#[test]
fn cluster_node_registry_overrides_and_clock_anchoring() {
    let mut reg = ClusterNodeRegistry::default();
    reg.system_node_overrides.insert(30_000_142, 100);
    reg.reinforced_nodes.insert(100, 30_000_142);
    reg.manual_tidi_overrides.insert(30_000_142, 0.5);
    reg.sim_clock_anchoring
        .insert(30_000_142, (133_000_000_000_000_000, 133_000_000_000_000_000, 0.5));
    reg.auto_tidi_enabled = true;
    reg.scheduled_downtime_enabled = false;
    reg.node_load_tidi_threshold = 120;
    reg.next_dedicated_node_id = 101;

    let cloned = reg.clone();
    assert_eq!(cloned.system_node_overrides.get(&30_000_142), Some(&100));
    assert_eq!(cloned.reinforced_nodes.get(&100), Some(&30_000_142));
    assert_eq!(cloned.manual_tidi_overrides.get(&30_000_142), Some(&0.5));
    assert_eq!(
        cloned.sim_clock_anchoring.get(&30_000_142),
        Some(&(133_000_000_000_000_000, 133_000_000_000_000_000, 0.5))
    );
    assert!(cloned.auto_tidi_enabled);
    assert!(!cloned.scheduled_downtime_enabled);
    assert_eq!(cloned.node_load_tidi_threshold, 120);
    assert_eq!(cloned.next_dedicated_node_id, 101);
}

#[test]
fn system_command_all_variants_construct_and_match() {
    use eve_destiny::{Ball, CombatState, DamageVector, DestinyEvent, MovementMode, SlimItemMeta, TargetLock};
    use std::collections::HashMap;

    fn tag_of(cmd: SystemCommand) -> &'static str {
        match cmd {
            SystemCommand::SpawnBall { .. } => "SpawnBall",
            SystemCommand::SpawnBallWithCombat { .. } => "SpawnBallWithCombat",
            SystemCommand::SpawnBallWithMeta { .. } => "SpawnBallWithMeta",
            SystemCommand::SpawnBallWithMetaTagged { .. } => "SpawnBallWithMetaTagged",
            SystemCommand::ChangeSlimItem { .. } => "ChangeSlimItem",
            SystemCommand::SetBallWormholeStages { .. } => "SetBallWormholeStages",
            SystemCommand::SetBallMaxVelocity { .. } => "SetBallMaxVelocity",
            SystemCommand::SetBallMaxVelocityCap { .. } => "SetBallMaxVelocityCap",
            SystemCommand::GetBallSlimItem { .. } => "GetBallSlimItem",
            SystemCommand::SpawnAsteroid { .. } => "SpawnAsteroid",
            SystemCommand::MineAsteroid { .. } => "MineAsteroid",
            SystemCommand::GetAsteroidState { .. } => "GetAsteroidState",
            SystemCommand::BreakGateCloak { .. } => "BreakGateCloak",
            SystemCommand::PopulateAsteroids { .. } => "PopulateAsteroids",
            SystemCommand::RemoveBall { .. } => "RemoveBall",
            SystemCommand::RemoveBallIfToken { .. } => "RemoveBallIfToken",
            SystemCommand::AcceptDocking { .. } => "AcceptDocking",
            SystemCommand::EmitPackagedActions { .. } => "EmitPackagedActions",
            SystemCommand::SetMovementMode { .. } => "SetMovementMode",
            SystemCommand::SetWarpMovementMode { .. } => "SetWarpMovementMode",
            SystemCommand::SetWarpScrambleStrength { .. } => "SetWarpScrambleStrength",
            SystemCommand::SetWarpCoreStrength { .. } => "SetWarpCoreStrength",
            SystemCommand::AddWarpBubble { .. } => "AddWarpBubble",
            SystemCommand::RemoveWarpBubble { .. } => "RemoveWarpBubble",
            SystemCommand::SetSpeedFraction { .. } => "SetSpeedFraction",
            SystemCommand::SetBallPosition { .. } => "SetBallPosition",
            SystemCommand::EmitDestinyEvent { .. } => "EmitDestinyEvent",
            SystemCommand::EmitObserverDestinyEvent { .. } => "EmitObserverDestinyEvent",
            SystemCommand::GotoPoint { .. } => "GotoPoint",
            SystemCommand::PushSetState { .. } => "PushSetState",
            SystemCommand::GetBallSnapshot { .. } => "GetBallSnapshot",
            SystemCommand::GetBallGroupId { .. } => "GetBallGroupId",
            SystemCommand::ListBalls { .. } => "ListBalls",
            SystemCommand::ListBallsInBubble { .. } => "ListBallsInBubble",
            SystemCommand::GetSetState { .. } => "GetSetState",
            SystemCommand::GetSetStateWithSeq { .. } => "GetSetStateWithSeq",
            SystemCommand::GetBubbleIdForBall { .. } => "GetBubbleIdForBall",
            SystemCommand::DrainObserverUpdates { .. } => "DrainObserverUpdates",
            SystemCommand::DrainObserverUpdatesWithSeq { .. } => "DrainObserverUpdatesWithSeq",
            SystemCommand::ApplyDamage { .. } => "ApplyDamage",
            SystemCommand::ApplyTrueDamage { .. } => "ApplyTrueDamage",
            SystemCommand::StepManual { .. } => "StepManual",
            SystemCommand::StepManualTickOnly { .. } => "StepManualTickOnly",
            SystemCommand::FastForwardClock { .. } => "FastForwardClock",
            SystemCommand::GetTargetLocks { .. } => "GetTargetLocks",
            SystemCommand::AddTargetLock { .. } => "AddTargetLock",
            SystemCommand::RemoveTargetLock { .. } => "RemoveTargetLock",
            SystemCommand::RemoveTargetLockQuiet { .. } => "RemoveTargetLockQuiet",
            SystemCommand::ClearTargetLocks { .. } => "ClearTargetLocks",
            SystemCommand::ClearTargetLocksQuiet { .. } => "ClearTargetLocksQuiet",
            SystemCommand::SetCapacitor { .. } => "SetCapacitor",
            SystemCommand::HealCombatState { .. } => "HealCombatState",
            SystemCommand::SetCombatState { .. } => "SetCombatState",
            SystemCommand::GetCombatState { .. } => "GetCombatState",
            SystemCommand::AddModuleCycle { .. } => "AddModuleCycle",
            SystemCommand::RemoveModuleCycle { .. } => "RemoveModuleCycle",
            SystemCommand::RequestCycleDeactivation { .. } => "RequestCycleDeactivation",
            SystemCommand::FireWeaponCycleVolley { .. } => "FireWeaponCycleVolley",
            SystemCommand::HasModuleCycle { .. } => "HasModuleCycle",
            SystemCommand::ReconcileModuleCycleBeforeActivate { .. } => "ReconcileModuleCycleBeforeActivate",
            SystemCommand::SetInterdictionNullified { .. } => "SetInterdictionNullified",
            SystemCommand::MicroJump { .. } => "MicroJump",
            SystemCommand::MicroJumpField { .. } => "MicroJumpField",
            SystemCommand::IsBallScrambled { .. } => "IsBallScrambled",
            SystemCommand::AddInterdictionSphere { .. } => "AddInterdictionSphere",
            SystemCommand::AddMobileWarpDisruptor { .. } => "AddMobileWarpDisruptor",
            SystemCommand::ApplyEcmJam { .. } => "ApplyEcmJam",
            SystemCommand::ApplyEcmJamFrom { .. } => "ApplyEcmJamFrom",
            SystemCommand::ApplyEcmJamFromModule { .. } => "ApplyEcmJamFromModule",
            SystemCommand::ApplyEcmBurst { .. } => "ApplyEcmBurst",
            SystemCommand::ScheduleMobileWarpDisruptor { .. } => "ScheduleMobileWarpDisruptor",
            SystemCommand::UpdateModuleOverload { .. } => "UpdateModuleOverload",
            SystemCommand::StartNaniteRepair { .. } => "StartNaniteRepair",
            SystemCommand::StopNaniteRepair { .. } => "StopNaniteRepair",
            SystemCommand::RecordIncomingArmorDamage { .. } => "RecordIncomingArmorDamage",
            SystemCommand::StopNonWarpModuleCycles { .. } => "StopNonWarpModuleCycles",
            SystemCommand::FlushPeerObserverUpdates { .. } => "FlushPeerObserverUpdates",
            SystemCommand::TakeBall { .. } => "TakeBall",
            SystemCommand::RestoreBall { .. } => "RestoreBall",
            SystemCommand::RestoreBallIfAbsent { .. } => "RestoreBallIfAbsent",
            SystemCommand::ModifyCombatState { .. } => "ModifyCombatState",
            SystemCommand::FlushConditionCheckpoint { .. } => "FlushConditionCheckpoint",
            SystemCommand::ApplyFitProjection { .. } => "ApplyFitProjection",
            SystemCommand::TrackModuleReloadSim { .. } => "TrackModuleReloadSim",
            SystemCommand::RepairCombatState { .. } => "RepairCombatState",
            SystemCommand::CurrentTick { .. } => "CurrentTick",
            SystemCommand::BreakProtection { .. } => "BreakProtection",
            SystemCommand::ForceDecloak { .. } => "ForceDecloak",
            SystemCommand::GetProtectionSnapshot { .. } => "GetProtectionSnapshot",
            SystemCommand::SyncLocationChangeInvuln { .. } => "SyncLocationChangeInvuln",
            SystemCommand::ActivateModuleCycleAtomic { .. } => "ActivateModuleCycleAtomic",
            SystemCommand::SetMovementModeBreakingProtection { .. } => "SetMovementModeBreakingProtection",
            SystemCommand::SetWarpMovementModeBreakingProtection { .. } => "SetWarpMovementModeBreakingProtection",
            SystemCommand::GotoPointBreakingProtection { .. } => "GotoPointBreakingProtection",
            SystemCommand::ShutdownActor { .. } => "ShutdownActor",
        }
    }

    let ball = Ball {
        id: 1,
        type_id: 587,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 2.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 1.0,
        mode: MovementMode::Stop,
    };
    let ast = AsteroidState {
        asteroid_id: 2,
        belt_id: 3,
        type_id: 1228,
        initial_units: 100,
        remaining_units: 100,
        unit_volume: 0.1,
        depleted: false,
        respawn_at_tick: None,
    };

    let cmds = vec![
        SystemCommand::SpawnBall { ball: ball.clone(), reply: oneshot::channel().0 },
        SystemCommand::SpawnBallWithCombat { ball: ball.clone(), combat_state: CombatState::default(), reply: oneshot::channel().0 },
        SystemCommand::SpawnBallWithMeta { ball: ball.clone(), combat_state: CombatState::default(), slim_meta: SlimItemMeta::default(), reply: oneshot::channel().0 },
        SystemCommand::ChangeSlimItem { ball_id: 1, slim_meta: SlimItemMeta::default(), reply: oneshot::channel().0 },
        SystemCommand::SetBallWormholeStages { ball_id: 1, wormhole_age: 1, wormhole_size: 1.0, reply: oneshot::channel().0 },
        SystemCommand::SetBallMaxVelocity { ball_id: 1, max_velocity: 400.0, reply: oneshot::channel().0 },
        SystemCommand::SetBallMaxVelocityCap { ball_id: 1, cap: Some(250.0), reply: oneshot::channel().0 },
        SystemCommand::GetBallSlimItem { ball_id: 1, reply: oneshot::channel().0 },
        SystemCommand::SpawnAsteroid { asteroid: ast, ball: ball.clone(), reply: oneshot::channel().0 },
        SystemCommand::MineAsteroid { asteroid_id: 2, requested_units: 10, waste_probability: 0.1, persist_db: false, reply: oneshot::channel().0 },
        SystemCommand::GetAsteroidState { asteroid_id: 2, reply: oneshot::channel().0 },
        SystemCommand::BreakGateCloak { ball_id: 1, reply: oneshot::channel().0 },
        SystemCommand::PopulateAsteroids { reply: oneshot::channel().0 },
        SystemCommand::RemoveBall { ball_id: 1, reply: oneshot::channel().0 },
        SystemCommand::AcceptDocking { ship_id: 1, station_id: 60_003_760, station_pos: DVec3::ZERO, reply: oneshot::channel().0 },
        SystemCommand::EmitPackagedActions { ball_id: 1, events: vec![], reply: oneshot::channel().0 },
        SystemCommand::SetMovementMode { ball_id: 1, mode: MovementMode::Stop, reply: oneshot::channel().0 },
        SystemCommand::SetWarpMovementMode { ball_id: 1, mode: MovementMode::Stop, provenance: super::WarpProvenance::PlayerOrFleetWarp, reply: oneshot::channel().0 },
        SystemCommand::SetWarpScrambleStrength { ball_id: 1, strength: 2, reply: oneshot::channel().0 },
        SystemCommand::SetWarpCoreStrength { ball_id: 1, strength: 3, reply: oneshot::channel().0 },
        SystemCommand::AddWarpBubble { id: 10, position: DVec3::ZERO, radius: 20_000.0, reply: oneshot::channel().0 },
        SystemCommand::RemoveWarpBubble { id: 10, reply: oneshot::channel().0 },
        SystemCommand::SetSpeedFraction { ball_id: 1, fraction: 0.5, reply: oneshot::channel().0 },
        SystemCommand::SetBallPosition { ball_id: 1, position: DVec3::X, reply: oneshot::channel().0 },
        SystemCommand::EmitDestinyEvent { ball_id: 1, event: DestinyEvent::OnSlimItemChange { ball_id: 1 }, reply: oneshot::channel().0 },
        SystemCommand::EmitObserverDestinyEvent { observer_ball_id: 1, event: DestinyEvent::OnSlimItemChange { ball_id: 1 }, reply: oneshot::channel().0 },
        SystemCommand::GotoPoint { ball_id: 1, point: DVec3::Y, reply: oneshot::channel().0 },
        SystemCommand::PushSetState { observer_ball_id: 1, reply: oneshot::channel().0 },
        SystemCommand::GetBallSnapshot { ball_id: 1, reply: oneshot::channel().0 },
        SystemCommand::GetBallGroupId { ball_id: 1, reply: oneshot::channel().0 },
        SystemCommand::ListBalls { reply: oneshot::channel().0 },
        SystemCommand::ListBallsInBubble { observer_ball_id: 1, dynamic_only: true, reply: oneshot::channel().0 },
        SystemCommand::GetSetState { observer_ball_id: 1, reply: oneshot::channel().0 },
        SystemCommand::GetBubbleIdForBall { ball_id: 1, reply: oneshot::channel().0 },
        SystemCommand::DrainObserverUpdates { observer_ball_id: 1, reply: oneshot::channel().0 },
        SystemCommand::ApplyDamage { ball_id: 1, damage: DamageVector::default(), reply: oneshot::channel().0 },
        SystemCommand::ApplyTrueDamage { ball_id: 1, amount: 50.0, reply: oneshot::channel().0 },
        SystemCommand::StepManual { dt: 1.0, reply: oneshot::channel().0 },
        SystemCommand::FastForwardClock { ship_id: 1, secs: 60, reply: oneshot::channel().0 },
        SystemCommand::GetTargetLocks { ball_id: 1, reply: oneshot::channel().0 },
        SystemCommand::AddTargetLock { source_id: 1, lock: TargetLock { target_id: 2, lock_time_ms: 100.0, remaining_lock_ms: 0.0, locked: true, max_range_m: None }, reply: oneshot::channel().0 },
        SystemCommand::RemoveTargetLock { source_id: 1, target_id: 2, reply: oneshot::channel().0 },
        SystemCommand::RemoveTargetLockQuiet { source_id: 1, target_id: 2, reply: oneshot::channel().0 },
        SystemCommand::ClearTargetLocks { source_id: 1, reply: oneshot::channel().0 },
        SystemCommand::ClearTargetLocksQuiet { source_id: 1, reply: oneshot::channel().0 },
        SystemCommand::SetCapacitor { ball_id: 1, capacitor: 50.0, reply: oneshot::channel().0 },
        SystemCommand::HealCombatState { ball_id: 1, reply: oneshot::channel().0 },
        SystemCommand::SetCombatState { ball_id: 1, combat_state: CombatState::default(), reply: oneshot::channel().0 },
        SystemCommand::GetCombatState { ball_id: 1, reply: oneshot::channel().0 },
        SystemCommand::AddModuleCycle { cycle: ActiveModuleCycle::default(), reply: oneshot::channel().0 },
        SystemCommand::RemoveModuleCycle { ship_id: 1, module_id: 2, reply: oneshot::channel().0 },
        SystemCommand::RequestCycleDeactivation { ship_id: 1, module_id: 2, reply: oneshot::channel().0 },
        SystemCommand::FireWeaponCycleVolley { ship_id: 1, module_id: 2, reply: oneshot::channel().0 },
        SystemCommand::HasModuleCycle { ship_id: 1, module_id: 2, reply: oneshot::channel().0 },
        SystemCommand::ReconcileModuleCycleBeforeActivate { ship_id: 1, module_id: 2, now_unix: 0, reply: oneshot::channel().0 },
        SystemCommand::SetInterdictionNullified { ball_id: 1, nullified: true, reply: oneshot::channel().0 },
        SystemCommand::MicroJump { ball_id: 1, distance_meters: 100_000.0, reply: oneshot::channel().0 },
        SystemCommand::MicroJumpField { ball_id: 1, distance_meters: 100_000.0, field_radius_meters: 6_000.0, max_jumped_ships: 25, reply: oneshot::channel().0 },
        SystemCommand::IsBallScrambled { ball_id: 1, reply: oneshot::channel().0 },
        SystemCommand::AddInterdictionSphere { id: 11, position: DVec3::ZERO, radius: 20_000.0, reply: oneshot::channel().0 },
        SystemCommand::AddMobileWarpDisruptor { id: 12, position: DVec3::ZERO, radius: 15_000.0, reply: oneshot::channel().0 },
        SystemCommand::ApplyEcmJam { target_id: 1, duration_s: 20.0, reply: oneshot::channel().0 },
        SystemCommand::ApplyEcmBurst { source_id: 1, burst_radius_m: 10_000.0, jam_strengths: [5.0; 4], duration_s: 20.0, roll: 0.1, ship_eval_overrides: Some(HashMap::new()), reply: oneshot::channel().0 },
        SystemCommand::ScheduleMobileWarpDisruptor { id: 13, type_id: 12_198, position: DVec3::ZERO, radius: 15_000.0, anchoring_delay_s: 10.0, reply: oneshot::channel().0 },
        SystemCommand::UpdateModuleOverload { ship_id: 1, module_id: 2, is_overloaded: true, reply: oneshot::channel().0 },
        SystemCommand::StartNaniteRepair { repair: ActiveNaniteRepair { char_id: 1, ship_id: 1, module_id: 2, initial_damage_hp: 10.0, paste_total_needed: 1, paste_consumed_so_far: 1, duration_s: 5.0, elapsed_s: 0.0 }, reply: oneshot::channel().0 },
        SystemCommand::StopNaniteRepair { ship_id: 1, module_id: 2, reply: oneshot::channel().0 },
        SystemCommand::RecordIncomingArmorDamage { ship_id: 1, damage: [1.0, 2.0, 3.0, 4.0], reply: oneshot::channel().0 },
        SystemCommand::StopNonWarpModuleCycles { ship_id: 1, reply: oneshot::channel().0 },
        SystemCommand::FlushPeerObserverUpdates { exclude_ball_id: None, reply: oneshot::channel().0 },
        SystemCommand::FlushConditionCheckpoint { reply: oneshot::channel().0 },
        SystemCommand::RepairCombatState {
            ball_id: 1,
            hull_heal: 10.0,
            armor_heal: 10.0,
            shield_heal: 10.0,
            reply: oneshot::channel().0,
        },
        SystemCommand::CurrentTick { reply: oneshot::channel().0 },
        SystemCommand::RestoreBallIfAbsent {
            taken: super::TakenBall {
                tick: 0,
                ball: ball.clone(),
                combat_state: None,
                slim_meta: None,
                cycles_cleared: 0,
                removed_own_cycles: Vec::new(),
                pre_cleanup_combat_flags: None,
            },
            reply: oneshot::channel().0,
        },
        SystemCommand::SpawnBallWithMetaTagged {
            ball: ball.clone(),
            combat_state: CombatState::default(),
            slim_meta: SlimItemMeta::default(),
            spawn_token: 1,
            reply: oneshot::channel().0,
        },
        SystemCommand::RemoveBallIfToken {
            ball_id: 1,
            spawn_token: 1,
            reply: oneshot::channel().0,
        },
        SystemCommand::GetSetStateWithSeq {
            observer_ball_id: 1,
            reply: oneshot::channel().0,
        },
        SystemCommand::ShutdownActor {
            reply: oneshot::channel().0,
        },
    ];
    let tags: Vec<&'static str> = cmds.into_iter().map(tag_of).collect();
    assert_eq!(tags.len(), 78);
    assert_eq!(tags[0], "SpawnBall");
    assert_eq!(tags[69], "FlushPeerObserverUpdates");
    assert_eq!(tags[70], "FlushConditionCheckpoint");
    assert_eq!(tags[71], "RepairCombatState");
    assert_eq!(tags[72], "CurrentTick");
    assert_eq!(tags[73], "RestoreBallIfAbsent");
    assert_eq!(tags[74], "SpawnBallWithMetaTagged");
    assert_eq!(tags[75], "RemoveBallIfToken");
    assert_eq!(tags[76], "GetSetStateWithSeq");
}

#[test]
fn pending_destroyed_outbox_checkout_entries_isolates_unrelated_pending_items_1168() {
    let outbox = super::PendingDestroyedOutbox::default();
    outbox.record_one(1001, Some(10));
    outbox.record_one(1002, Some(20));
    outbox.record_one(1003, Some(30));

    // Checking out only ball 1002 must leave 1001 and 1003 undisturbed in `pending`.
    let (batch, lease_opt) = outbox.checkout_entries(&[(1002, Some(20)), (1002, Some(20))]);
    assert_eq!(batch, vec![(1002, Some(20))]);
    assert_eq!(outbox.pending_and_in_flight_counts(), (2, 1));
    assert!(outbox.is_non_empty());

    // Dropping the lease without ack rolls 1002 back to pending cleanly.
    drop(lease_opt);
    assert_eq!(outbox.pending_and_in_flight_counts(), (3, 0));

    // Re-checking out 1002 and acking commits only 1002 while keeping 1001 and 1003 pending.
    let (batch2, lease2) = outbox.checkout_entries(&[(1002, Some(20))]);
    assert_eq!(batch2, vec![(1002, Some(20))]);
    lease2.unwrap().ack();
    assert_eq!(outbox.pending_and_in_flight_counts(), (2, 0));
    assert!(outbox.is_non_empty());

    let (rest, rest_lease) = outbox.checkout_for_step();
    assert_eq!(rest, vec![(1001, Some(10)), (1003, Some(30))]);
    rest_lease.unwrap().ack();
    assert_eq!(outbox.pending_and_in_flight_counts(), (0, 0));
    assert!(!outbox.is_non_empty());
    assert_eq!(outbox.totals(), (3, 3));
}

#[test]
fn system_view_from_ballpark_captures_kinematics_flags_damage_modules_and_grids() {
    use super::{BallDamageFractions, SystemView};
    use eve_destiny::{Ball, Ballpark, CombatState, MovementMode, SlimItemMeta, TargetLock};

    let def = SystemView::default();
    assert!((def.tidi_factor - 1.0).abs() < 1e-12);
    assert!(def.balls.is_empty());

    let mut bp = Ballpark::new(30_000_142);
    bp.current_tick = 42;
    bp.sim_unix_secs = 1_700_000_123;

    let planet_id = 40_009_001_i64;
    let far_static_id = 40_009_002_i64;
    bp.add_static_ball_with_meta(
        Ball {
            id: planet_id,
            type_id: 11,
            position: DVec3::new(0.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1e18,
            radius: 500_000.0,
            speed_fraction: 0.0,
            mode: MovementMode::Rigid,
        },
        SlimItemMeta {
            group_id: 7,
            category_id: 2,
            owner_id: 1_000_125,
            ..Default::default()
        },
    );
    bp.add_static_ball_with_meta(
        Ball {
            id: far_static_id,
            type_id: 11,
            position: DVec3::new(500_000_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1e18,
            radius: 100_000.0,
            speed_fraction: 0.0,
            mode: MovementMode::Rigid,
        },
        SlimItemMeta {
            group_id: 7,
            category_id: 2,
            owner_id: 1_000_125,
            ..Default::default()
        },
    );

    let ship_a = 1001_i64;
    let ship_b = 1002_i64;
    let ship_c = 1003_i64;
    let ship_d = 1004_i64;
    let shuttle_e = 1005_i64;
    let ship_f = 1006_i64;

    bp.set_ball_slim_meta(
        ship_a,
        SlimItemMeta {
            group_id: 900,
            category_id: 6,
            owner_id: 90_001,
            char_id: Some(90_001),
            corp_id: 98_001,
            alliance_id: 99_001,
            is_cloaked: false,
            ..Default::default()
        },
    );
    bp.add_ball(Ball {
        id: ship_a,
        type_id: 28_659,
        position: DVec3::new(10_000.0, 0.0, 0.0),
        velocity: DVec3::new(150.0, 0.0, 0.0),
        max_velocity: 200.0,
        inertia: 3.1,
        mass: 12_500_000.0,
        radius: 135.0,
        speed_fraction: 0.75,
        mode: MovementMode::GotoDirection { direction: DVec3::X },
    });
    bp.combat_states.insert(
        ship_a,
        CombatState {
            shield_hp: 250.0,
            max_shield_hp: 500.0,
            armor_hp: 300.0,
            max_armor_hp: 400.0,
            structure_hp: 150.0,
            max_structure_hp: 300.0,
            warp_scramble_strength: 2,
            warp_core_strength: 1,
            ..CombatState::default()
        },
    );

    bp.add_ball(Ball {
        id: ship_b,
        type_id: 587,
        position: DVec3::new(12_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 350.0,
        inertia: 2.0,
        mass: 1_100_000.0,
        radius: 40.0,
        speed_fraction: 1.0,
        mode: MovementMode::Stop,
    });
    bp.combat_states.insert(
        ship_b,
        CombatState {
            is_cloaked: true,
            is_tethered: true,
            gate_cloak_remaining_ticks: 6,
            ..CombatState::default()
        },
    );

    bp.set_ball_slim_meta(
        ship_c,
        SlimItemMeta {
            group_id: 25,
            category_id: 6,
            owner_id: 90_003,
            is_cloaked: true,
            ..Default::default()
        },
    );
    bp.add_ball(Ball {
        id: ship_c,
        type_id: 587,
        position: DVec3::new(13_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 350.0,
        inertia: 2.0,
        mass: 1_100_000.0,
        radius: 40.0,
        speed_fraction: 1.0,
        mode: MovementMode::Stop,
    });

    bp.add_warp_bubble(90_999, DVec3::new(50_000_000.0, 0.0, 0.0), 20_000.0);
    bp.add_ball(Ball {
        id: ship_d,
        type_id: 587,
        position: DVec3::new(50_005_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 350.0,
        inertia: 2.0,
        mass: 1_100_000.0,
        radius: 40.0,
        speed_fraction: 1.0,
        mode: MovementMode::Stop,
    });
    bp.combat_states.insert(
        ship_d,
        CombatState {
            max_shield_hp: 0.0,
            shield_hp: 0.0,
            max_armor_hp: 0.0,
            armor_hp: 0.0,
            max_structure_hp: 0.0,
            structure_hp: 0.0,
            ..CombatState::default()
        },
    );

    bp.ball_group_ids.insert(shuttle_e, 31);
    bp.add_ball(Ball {
        id: shuttle_e,
        type_id: 672,
        position: DVec3::new(50_006_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 500.0,
        inertia: 1.0,
        mass: 800_000.0,
        radius: 25.0,
        speed_fraction: 1.0,
        mode: MovementMode::Stop,
    });

    bp.add_ball(Ball {
        id: ship_f,
        type_id: 28_606,
        position: DVec3::new(15_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 100.0,
        inertia: 5.0,
        mass: 100_000_000.0,
        radius: 300.0,
        speed_fraction: 1.0,
        mode: MovementMode::Stop,
    });

    bp.target_locks.insert(
        ship_a,
        vec![TargetLock {
            target_id: ship_b,
            lock_time_ms: 1000.0,
            remaining_lock_ms: 0.0,
            locked: true,
            max_range_m: Some(50_000.0),
        }],
    );

    let mut st = SystemActorState::default();
    st.sim_elapsed_secs = 27.5;
    st.active_module_cycles.push(ActiveModuleCycle {
        ship_id: ship_a,
        module_id: 1,
        module_type_id: 33400,
        effect_name: "bastion".into(),
        ..ActiveModuleCycle::default()
    });
    st.active_module_cycles.push(ActiveModuleCycle {
        ship_id: ship_b,
        module_id: 2,
        module_type_id: 27951,
        effect_name: "triage".into(),
        ..ActiveModuleCycle::default()
    });
    st.active_module_cycles.push(ActiveModuleCycle {
        ship_id: ship_c,
        module_id: 3,
        module_type_id: 20280,
        effect_name: "siege".into(),
        ..ActiveModuleCycle::default()
    });
    st.active_module_cycles.push(ActiveModuleCycle {
        ship_id: ship_f,
        module_id: 4,
        module_type_id: 28583,
        effect_name: "industrialCore".into(),
        ..ActiveModuleCycle::default()
    });
    st.active_module_cycles.push(ActiveModuleCycle {
        ship_id: ship_d,
        module_id: 5,
        module_type_id: 4383,
        effect_name: "microJumpDrive".into(),
        mjd_jump_distance_m: Some(100_000.0),
        ..ActiveModuleCycle::default()
    });

    let view = SystemView::from_ballpark(&bp, Some(&st), 0.5, None);
    assert_eq!(view.solar_system_id, 30_000_142);
    assert_eq!(view.current_tick, 42);
    assert_eq!(view.sim_unix_secs, 1_700_000_123);
    assert!((view.tidi_factor - 0.5).abs() < 1e-12);
    assert!((view.sim_elapsed_secs - 27.5).abs() < 1e-12);

    assert!(view.contains_ball(ship_a));
    assert!(view.is_static_ball(planet_id));
    assert!(!view.is_static_ball(ship_a));
    assert_eq!(view.ball_position(ship_a), Some(DVec3::new(10_000.0, 0.0, 0.0)));
    assert_eq!(view.ball_velocity(ship_a), Some(DVec3::new(150.0, 0.0, 0.0)));
    assert!(matches!(
        view.movement_mode(ship_a),
        Some(MovementMode::GotoDirection { .. })
    ));
    assert!((view.ball(ship_a).unwrap().mass - 12_500_000.0).abs() < 1e-6);
    assert!((view.ball(ship_a).unwrap().radius - 135.0).abs() < 1e-6);
    assert_eq!(view.group_id(ship_a), Some(900));
    assert_eq!(view.owner_id(ship_a), Some(90_001));
    assert_eq!(view.corp_id(ship_a), Some(98_001));
    assert_eq!(view.alliance_id(ship_a), Some(99_001));

    let fracs = view.damage_fractions(ship_a).unwrap();
    assert_eq!(
        fracs,
        BallDamageFractions {
            shield: 0.5,
            armor: 0.75,
            structure: 0.5,
        }
    );
    let zero_fracs = view.damage_fractions(ship_d).unwrap();
    assert_eq!(
        zero_fracs,
        BallDamageFractions {
            shield: 0.0,
            armor: 0.0,
            structure: 0.0,
        }
    );

    assert!(view.is_ball_scrambled(ship_a));
    assert!(view.is_ball_scrambled(ship_d));
    assert!(view.is_ball_warp_disrupted(ship_d));
    assert!(!view.is_ball_warp_disrupted(ship_a));
    assert!(!view.is_ball_scrambled(shuttle_e));
    assert!(!view.is_ball_warp_disrupted(shuttle_e));

    assert!(view.is_ball_cloaked(ship_b));
    assert!(view.is_ball_cloaked(ship_c));
    assert!(view.is_gate_cloaked(ship_b));
    assert!(view.is_ball_tethered(ship_b));
    assert!(view.is_ball_invulnerable(ship_b));
    assert!(!view.is_ball_tethered(ship_a));
    assert!(!view.is_ball_invulnerable(ship_a));

    assert!(view.has_active_module(ship_a));
    assert!(view.has_active_bastion(ship_a));
    assert!(view.has_active_triage(ship_b));
    assert!(view.has_active_siege(ship_c));
    assert!(view.has_active_industrial_core(ship_f));
    assert!(view.has_active_microjump(ship_d));

    assert!(view.is_ball_targeted(ship_b));
    assert!(!view.is_ball_targeted(ship_a));
    assert_eq!(view.targeters_of(ship_b).collect::<Vec<_>>(), vec![ship_a]);

    assert!(view.bubble_id_for_ball(planet_id).is_some());
    assert_eq!(view.bubble_id_for_ball(planet_id), bp.bubble_id_for_ball(planet_id));
    assert_eq!(view.bubble_id_for_ball(far_static_id), None);
    assert_eq!(
        view.bubble_id_for_ball(far_static_id),
        bp.bubble_id_for_ball(far_static_id)
    );
    assert_eq!(view.bubble_id_for_ball(ship_a), bp.bubble_id_for_ball(ship_a));
    let obs_gid = view.bubble_id_for_ball(ship_a).unwrap();
    assert!(view.grid_bubble(obs_gid).is_some());
    let expected_vis: Vec<i64> = bp
        .visible_balls_for_observer(ship_a)
        .into_iter()
        .map(|b| b.id)
        .collect();
    let got_vis: Vec<i64> = view
        .visible_balls_for_observer(ship_a)
        .into_iter()
        .map(|b| b.id)
        .collect();
    assert_eq!(got_vis, expected_vis);
    let expected_dyn: Vec<i64> = bp
        .dynamic_balls_in_bubble(ship_a)
        .into_iter()
        .map(|b| b.id)
        .collect();
    let got_dyn: Vec<i64> = view
        .dynamic_balls_in_bubble(ship_a)
        .into_iter()
        .map(|b| b.id)
        .collect();
    assert_eq!(got_dyn, expected_dyn);

    // Passing actor_state = None preserves prior active-module flags and sim_elapsed_secs
    let preserved = SystemView::from_ballpark(&bp, None, 0.5, Some(&view));
    assert!(preserved.has_active_bastion(ship_a));
    assert!(preserved.has_active_triage(ship_b));
    assert!(preserved.has_active_siege(ship_c));
    assert!(preserved.has_active_industrial_core(ship_f));
    assert!(preserved.has_active_microjump(ship_d));
    assert!((preserved.sim_elapsed_secs - 27.5).abs() < 1e-12);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn get_set_state_with_seq_returns_snapshot_and_watermark_without_draining_1461() {
    use eve_destiny::{Ball, CombatState, DestinyEvent, MovementMode, SlimItemMeta};

    let ctx = crate::router::ServerContext::new();
    let sys = ctx.systems.get_or_spawn(30_000_142);
    let _rx = sys.subscribe();

    let obs_id = 704_901_i64;
    let peer_id = 704_902_i64;
    let mk_ball = |id: i64, x: f64| Ball {
        id,
        type_id: 606,
        position: DVec3::new(x, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 2.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let meta = SlimItemMeta {
        group_id: 25,
        category_id: 6,
        owner_id: 90_000_001,
        ..Default::default()
    };
    sys.spawn_ball_with_meta(mk_ball(obs_id, 50_000.0), CombatState::default(), meta.clone())
        .await
        .expect("spawn observer");
    sys.spawn_ball_with_meta(mk_ball(peer_id, 55_000.0), CombatState::default(), meta)
        .await
        .expect("spawn peer");

    assert!(
        sys.get_set_state_with_seq(999_999_999)
            .await
            .expect_err("absent observer ball must error")
            .contains("not in system")
    );

    let bcast1 = sys.step_manual(1.0).await.expect("step 1");
    assert!(bcast1.broadcast_seq >= 1);

    assert!(
        sys.emit_observer_destiny_event(obs_id, DestinyEvent::OnSlimItemChange { ball_id: peer_id })
            .await
    );

    let (tick1, payload1, updates1, seq1) = sys
        .get_set_state_with_seq(obs_id)
        .await
        .expect("get_set_state_with_seq 1");
    let (plain_tick, plain_payload, plain_updates) =
        sys.get_set_state(obs_id).await.expect("get_set_state");

    assert_eq!(tick1, bcast1.tick);
    assert_eq!(seq1, bcast1.broadcast_seq);
    assert_eq!(tick1, plain_tick);
    assert_eq!(payload1, plain_payload);
    assert_eq!(updates1, plain_updates);

    // Must not drain pending observer events queued for `obs_id`.
    let (drain_tick, drained_events, _drain_pyrep, drain_seq) = sys
        .drain_observer_updates_with_seq(obs_id)
        .await
        .expect("drain after get_set_state_with_seq");
    assert_eq!(drain_tick, tick1);
    assert_eq!(drain_seq, seq1);
    assert!(drained_events.iter().any(|ev| matches!(
        ev,
        DestinyEvent::OnSlimItemChange { ball_id } if *ball_id == peer_id
    )));

    let bcast2 = sys.step_manual(1.0).await.expect("step 2");
    assert!(bcast2.broadcast_seq > seq1);
    let (tick2, _payload2, _updates2, seq2) = sys
        .get_set_state_with_seq(obs_id)
        .await
        .expect("get_set_state_with_seq 2");
    assert_eq!(tick2, bcast2.tick);
    assert_eq!(seq2, bcast2.broadcast_seq);
}
