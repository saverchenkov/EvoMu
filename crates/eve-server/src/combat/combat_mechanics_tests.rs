//! Unit tests covering CrimeWatch, death pipeline, drones/salvage/tractor, and NPC AI.

use super::crimewatch::{
    compute_sec_status_loss, concord_delay_for_security, credit_specialized_npc_bounty_and_sec_status,
    AggressionEvaluation, AggressionManager, ConcordExecutionReport, CrimeWatchDirector,
    CrimeWatchLock,
};
use super::death_pipeline::{
    execute_death_pipeline_sync, execute_self_destruct, SelfDestructManager,
};
use super::drones_salvage_tractor::{
    step_tractor_beam, validate_drone_bandwidth, validate_wreck_salvage_target,
    execute_wreck_salvage_cycle, execute_wreck_salvage_auto_repeat, DroneController,
};
use super::npc_ai::{
    clone_soldier_tier_for_security, clone_soldier_type_for_security,
    clone_soldier_type_for_security_and_faction, compute_sec_status_gain, faction_damage_vector,
    regional_pirate_faction_id, NpcAiState, NpcSpawnManager,
};
use crate::router::ServerContext;
use eve_db::{ContractRecord, InventoryFlag, InventoryItemRecord, IskCents};
use eve_destiny::{Ball, MovementMode};
use eve_sde::{NpcSpawnGroupInfo, SdeRegistry};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(eve_db::WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

fn spawn_test_ship(ctx: &ServerContext, sys_id: u32, ship_id: i64, owner_id: i64, type_id: u32, pos: DVec3) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id,
                owner_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 250_000.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        snap.item_positions.insert(ship_id, [pos.x, pos.y, pos.z]);
        if let Some(ch) = snap.characters.get_mut(&owner_id) {
            ch.active_ship_id = ship_id;
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
        }
    });
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let mut cs = ctx.build_ship_combat_state(owner_id, ship_id, type_id);
    cs.capacitor = 100.0;
    sys_handle.try_spawn_ball_with_combat(
        Ball {
            id: ship_id,
            type_id,
            position: pos,
            velocity: DVec3::ZERO,
            max_velocity: 350.0,
            inertia: 2.0,
            mass: 1_100_000.0,
            radius: 40.0,
            speed_fraction: 1.0,
            mode: MovementMode::Stop,
        },
        cs,
    );
}

#[test]
fn crimewatch_sec_loss_concord_delays_and_aggression_manager() {
    assert!((compute_sec_status_loss(2.0, 1.0, false, 0.0) - 0.0).abs() < 1e-9);
    assert!((compute_sec_status_loss(-10.0, 5.0, false, 0.9) - 0.0).abs() < 1e-9);
    let vs_outlaw = compute_sec_status_loss(2.0, -5.0, false, 0.9);
    assert!(vs_outlaw > 0.1 && vs_outlaw < 0.25);
    let hi_ship = compute_sec_status_loss(2.0, 2.0, false, 0.9);
    let hi_pod = compute_sec_status_loss(2.0, 2.0, true, 0.9);
    assert!(hi_pod > hi_ship && hi_ship > 0.15);
    assert!(compute_sec_status_loss(-9.9, 5.0, true, 1.0) >= -10.0);

    assert_eq!(concord_delay_for_security(1.0), 6.0);
    assert_eq!(concord_delay_for_security(0.8), 8.0);
    assert_eq!(concord_delay_for_security(0.7), 11.0);
    assert_eq!(concord_delay_for_security(0.6), 14.0);
    assert_eq!(concord_delay_for_security(0.5), 19.0);
    assert!(concord_delay_for_security(0.3) >= 19.0);

    let mut mgr = AggressionManager::new();
    mgr.record_aggression(90_000_001, 90_000_002, 30_000_142, false, false, 1_000);
    assert!(mgr.get_aggression(90_000_001, 90_000_002, 1_500).is_some());
    assert!(mgr.is_aggressor_against(90_000_001, 90_000_002, 1_899));
    assert!(!mgr.is_aggressor_against(90_000_001, 90_000_002, 1_900));
    assert_eq!(
        mgr.list_active_aggressors_for_victim(90_000_002, 1_500),
        vec![90_000_001]
    );
    assert!(mgr.list_active_aggressors_for_victim(90_000_002, 2_000).is_empty());
}

#[test]
fn crimewatch_director_aggression_remote_rep_lock_and_npc_bounty() {
    let ctx = test_ctx();
    let now = 5_000;
    let mut cw = CrimeWatchDirector::new();

    cw.trigger_weapon_timer(90_000_001, now);
    cw.trigger_suspect_flag(90_000_001, now);
    assert!(cw.is_suspect(90_000_001, now + 10));
    assert!(!cw.is_suspect(90_000_001, now + 901));

    // Legal engagement
    assert_eq!(
        cw.handle_combat_aggression(90_000_001, 90_000_002, 100_000_001, 30_000_142, 0.9, true, now),
        AggressionEvaluation::LegalEngagement
    );

    // Low-sec ship vs pod
    assert_eq!(
        cw.handle_combat_aggression_with_pod(90_000_001, 90_000_002, 100_000_001, 30_002_048, 0.3, false, false, now),
        AggressionEvaluation::SuspectFlagTriggered
    );
    assert!(matches!(
        cw.handle_combat_aggression_with_pod(90_000_001, 90_000_002, 100_000_001, 30_002_048, 0.3, false, true, now),
        AggressionEvaluation::CriminalFlagTriggered { .. }
    ));

    // High-sec Stage 5 corruption: ship is suspect, pod is criminal
    cw.system_corruption_stages.insert(30_000_142, 5);
    assert_eq!(
        cw.handle_combat_aggression_with_pod(90_000_001, 90_000_002, 100_000_001, 30_000_142, 0.9, false, false, now),
        AggressionEvaluation::SuspectFlagTriggered
    );
    assert!(matches!(
        cw.handle_combat_aggression_with_pod(90_000_001, 90_000_002, 100_000_001, 30_000_142, 0.9, false, true, now),
        AggressionEvaluation::CriminalFlagTriggered { concord_delay_sec, arrival_unix }
            if concord_delay_sec == 6.0 && arrival_unix == now + 6
    ));
    assert!(cw.get_aggression(90_000_001, 90_000_002, now + 10).is_some());

    // Criminal remote rep & null-sec engagement
    assert!(matches!(
        cw.handle_criminal_remote_assistance(90_000_001, 90_000_002, 100_000_001, 30_000_142, 0.9, now),
        AggressionEvaluation::CriminalFlagTriggered { .. }
    ));
    assert_eq!(
        cw.handle_combat_aggression(90_000_001, 90_000_002, 100_000_001, 30_000_142, 0.0, false, now),
        AggressionEvaluation::LegalEngagement
    );

    // CrimeWatchLock write guard persistence + concord report ring buffer cap
    let lock = CrimeWatchLock::new(Arc::clone(&ctx.db));
    {
        let mut guard = lock.write().unwrap();
        guard.trigger_suspect_flag(90_000_002, now);
        guard.system_corruption_stages.insert(30_000_142, 3);
    }
    let read_guard = lock.read().unwrap();
    assert!(read_guard.is_suspect(90_000_002, now + 5));
    drop(read_guard);

    let dummy_reports: Vec<ConcordExecutionReport> = (0..35)
        .map(|i| ConcordExecutionReport {
            criminal_char_id: 90_000_001,
            victim_char_id: 90_000_002,
            concord_ball_id: 700_000 + i,
            kill_right_id: 10 + i,
            webbed_speed_fraction: 0.0,
            warp_scramble_strength: 100,
            capacitor_after_neut: 0.0,
            ecm_jammed: true,
            locks_cleared: 1,
            death_result: Err("test".into()),
        })
        .collect();
    lock.push_concord_reports(&dummy_reports);
    assert_eq!(lock.concord_reports().len(), 32);

    // Specialized NPC bounty + sec status gain & deduplication
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&90_000_001) {
            c.security_status = 0.0;
        }
    });
    credit_specialized_npc_bounty_and_sec_status(&ctx, 30_000_142, 888_001, 90_000_001, 250_000.0);
    credit_specialized_npc_bounty_and_sec_status(&ctx, 30_000_142, 888_001, 90_000_001, 250_000.0);
    let sec_after = ctx.db.with_snapshot(|s| s.characters[&90_000_001].security_status);
    assert!(sec_after > 0.0);
}

#[tokio::test]
async fn crimewatch_concord_step_executes_criminal_and_grants_kill_right() {
    let ctx = test_ctx();
    let sys_id = 30_000_142;
    let crim_ship = 100_050_001;
    spawn_test_ship(&ctx, sys_id, crim_ship, 90_000_001, 587, DVec3::new(1_000.0, 0.0, 0.0));
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle
        .add_target_lock(
            crim_ship,
            eve_destiny::TargetLock {
                target_id: 100_000_002,
                lock_time_ms: 1_000.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: None,
            },
        )
        .await;

    let now = 10_000;
    let mut cw = CrimeWatchDirector::new();
    let eval = cw.handle_combat_aggression(90_000_001, 90_000_002, crim_ship, sys_id, 0.9, false, now);
    let arrival = match eval {
        AggressionEvaluation::CriminalFlagTriggered { arrival_unix, .. } => arrival_unix,
        other => panic!("expected CriminalFlagTriggered, got {other:?}"),
    };

    let reports = cw.step_concord(&ctx, arrival, 42).await;
    assert_eq!(reports.len(), 1);
    let rep = &reports[0];
    assert_eq!(rep.criminal_char_id, 90_000_001);
    assert_eq!(rep.webbed_speed_fraction, 0.0);
    assert_eq!(rep.warp_scramble_strength, 100);
    assert_eq!(rep.capacitor_after_neut, 0.0);
    assert!(rep.ecm_jammed);
    assert!(rep.locks_cleared >= 1);
    let death = rep.death_result.as_ref().expect("CONCORD kills criminal ship");
    assert!(death.wreck_item_id > 0);
    assert!(death.killmail_id > 0);
}

#[tokio::test]
async fn death_pipeline_ship_destruction_courier_fail_pod_and_self_destruct() {
    let ctx = test_ctx();
    let sys_id = 30_000_142;
    let ship_id = 100_060_001;
    spawn_test_ship(&ctx, sys_id, ship_id, 90_000_001, 587, DVec3::new(5_000.0, 0.0, 0.0));

    // Seed cargo, implant, courier contract in ship, and escape frigate in FrigateEscapeBay
    ctx.db.with_mut_snapshot(|snap| {
        for (idx, tid) in [(1, 204), (2, 212), (3, 19540), (4, 582)] {
            let item_id = 100_060_100 + idx;
            let (flag, qty, loc) = match idx {
                3 => (InventoryFlag::Implant, 1, 90_000_001),
                4 => (InventoryFlag::FrigateEscapeBay, 1, ship_id),
                _ => (InventoryFlag::Cargo, 5, ship_id),
            };
            snap.items.insert(
                item_id,
                InventoryItemRecord {
                    item_id,
                    type_id: tid,
                    owner_id: 90_000_001,
                    location_id: loc,
                    flag,
                    quantity: qty,
                    singleton: flag != InventoryFlag::Cargo,
                    version: 1,
                    dynamic_attributes: HashMap::new(),
                    source_mutaplasmid_id: None,
                    insured_payout_isk: 0.0,
                    is_blueprint_copy: false,
                    blueprint_runs_remaining: None,
                },
            );
        }
        let courier_item_id = 100_060_105;
        snap.items.insert(
            courier_item_id,
            InventoryItemRecord {
                item_id: courier_item_id,
                type_id: 204,
                owner_id: 90_000_001,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        snap.contracts.insert(
            77_001,
            ContractRecord {
                contract_id: 77_001,
                issuer_id: 90_000_002,
                issuer_corp_id: 98_000_001,
                assignee_id: 90_000_001,
                acceptor_id: Some(90_000_001),
                contract_type: "Courier".into(),
                status: "InProgress".into(),
                title: "Courier test".into(),
                availability: "Personal".into(),
                item_ids: vec![courier_item_id],
                start_station_id: 60_003_760,
                end_station_id: 60_003_760,
                price: IskCents::from_isk(1_000_000),
                reward: IskCents::from_isk(100_000),
                collateral: IskCents::from_isk(500_000),
                expiration_unix: 10_000,
                date_issued_unix: 100,
                ..Default::default()
            },
        );
    });

    assert!(execute_death_pipeline_sync(&ctx, sys_id, 90_000_001, 999_999_999, 90_000_002, DVec3::ZERO, 1).is_err());

    let res = execute_death_pipeline_sync(&ctx, sys_id, 90_000_001, ship_id, 90_000_002, DVec3::new(5_000.0, 0.0, 0.0), 77)
        .expect("ship destruction succeeds");
    assert!(res.wreck_item_id > 0);
    let pod_id = res.ejected_capsule_id.expect("capsule ejected");
    assert!(res.insurance_payout_isk > 0.0);
    assert!(res.killmail_id > 0);
    assert_eq!(
        ctx.db.with_snapshot(|s| s.contracts[&77_001].status.clone()),
        "Failed"
    );

    // Self-destruct timer + pod destruction
    let mut sd = SelfDestructManager::new();
    let _det = sd.start_self_destruct(pod_id, 90_000_001, sys_id, 2_000);
    assert!(sd.is_self_destructing(pod_id));
    assert_eq!(sd.remaining_seconds(pod_id, 2_030), Some(90));
    assert!(sd.abort_self_destruct(pod_id));
    assert!(!sd.is_self_destructing(pod_id));

    let det2 = sd.start_self_destruct(pod_id, 90_000_001, sys_id, 2_100);
    let stepped = sd.step_timers(&ctx, det2, 99).await;
    assert_eq!(stepped.len(), 1);
    let pod_death = stepped[0].1.as_ref().expect("pod self-destruct succeeds");
    assert!(pod_death.killmail_id > res.killmail_id);

    // Direct self-destruct on another ship
    let ship2 = 100_060_002;
    spawn_test_ship(&ctx, sys_id, ship2, 90_000_002, 587, DVec3::new(9_000.0, 0.0, 0.0));
    let sd_res = execute_self_destruct(&ctx, sys_id, 90_000_002, ship2, DVec3::new(9_000.0, 0.0, 0.0), 55)
        .await
        .expect("execute_self_destruct succeeds");
    assert!(sd_res.killmail_id > 0);
}

#[tokio::test]
async fn salvage_tractor_beam_and_drone_controller_lifecycle() {
    let ctx = test_ctx();
    let sys_id = 30_000_142;
    let ship_id = 100_070_001;
    let ship_pos = DVec3::new(10_000.0, 0.0, 0.0);
    spawn_test_ship(&ctx, sys_id, ship_id, 90_000_001, 626, ship_pos);

    // Tractor beam math
    let (stay, arrived0) = step_tractor_beam(ship_pos + DVec3::new(1_000.0, 0.0, 0.0), ship_pos, 500.0, 1.0);
    assert!(arrived0);
    assert_eq!(stay, ship_pos + DVec3::new(1_000.0, 0.0, 0.0));
    let (mid, arrived1) = step_tractor_beam(ship_pos + DVec3::new(3_000.0, 0.0, 0.0), ship_pos, 500.0, 2.0);
    assert!(!arrived1);
    assert!((ship_pos.distance(mid) - 2_000.0).abs() < 1e-6);
    let (end_pos, arrived2) = step_tractor_beam(mid, ship_pos, 500.0, 10.0);
    assert!(arrived2);
    assert!((ship_pos.distance(end_pos) - 1_200.0).abs() < 1e-6);

    // Spawn a target ship and destroy it to produce a real wreck in ballpark
    let victim_ship = 100_070_002;
    let wreck_pos = ship_pos + DVec3::new(1_500.0, 0.0, 0.0);
    spawn_test_ship(&ctx, sys_id, victim_ship, 90_000_002, 587, wreck_pos);
    let death = execute_death_pipeline_sync(&ctx, sys_id, 90_000_002, victim_ship, 90_000_001, wreck_pos, 12).unwrap();
    let wid = death.wreck_item_id;

    validate_wreck_salvage_target(&ctx, sys_id, 90_000_001, ship_id, wid, None)
        .await
        .unwrap();
    let ok_salv = execute_wreck_salvage_cycle(&ctx, sys_id, 90_000_001, ship_id, wid, None)
        .await
        .unwrap();
    assert!(!matches!(ok_salv, eve_proto::PyRep::None));
    let _ = execute_wreck_salvage_auto_repeat(&ctx, sys_id, 90_000_001, ship_id, wid, None).await;

    // Drone bandwidth validation + DroneController lifecycle
    assert!(validate_drone_bandwidth(&ctx, ship_id, &[2454; 6], &[]).is_err());
    assert!(validate_drone_bandwidth(&ctx, 999_888_777, &[2454], &[]).is_err());
    assert!(validate_drone_bandwidth(&ctx, ship_id, &[2454, 2454], &[2454]).is_ok());

    let drone_id = 100_070_500;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            drone_id,
            InventoryItemRecord {
                item_id: drone_id,
                type_id: 2454, // Hobgoblin I
                owner_id: 90_000_001,
                location_id: ship_id,
                flag: InventoryFlag::DroneBay,
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

    let target_ball = 100_070_600;
    spawn_test_ship(&ctx, sys_id, target_ball, 90_000_002, 587, ship_pos + DVec3::new(2_000.0, 0.0, 0.0));

    let mut ctrl = DroneController::new();
    let launched = ctrl.launch_drones(&ctx, sys_id, 90_000_001, ship_id, &[drone_id]).await.unwrap();
    assert_eq!(launched.len(), 1);
    let d_ball = launched[0];
    ctrl.engage_target(&ctx, sys_id, target_ball).await.unwrap();
    assert_eq!(ctrl.active_drones[&d_ball].target_id, Some(target_ball));
    ctrl.return_and_orbit(&ctx, sys_id, ship_id).await.unwrap();
    assert_eq!(ctrl.active_drones[&d_ball].target_id, None);
    let recalled = ctrl.recall_drones(&ctx, sys_id, ship_id).await.unwrap();
    assert_eq!(recalled, vec![drone_id]);
}

#[tokio::test]
async fn npc_ai_spawn_belt_rats_step_machine_and_destruction() {
    let ctx = test_ctx();
    let dmg_g = faction_damage_vector(500010, 100.0);
    assert!((dmg_g.kinetic - 70.0).abs() < 1e-6);
    let dmg_b = faction_damage_vector(500012, 100.0);
    assert!((dmg_b.em - 60.0).abs() < 1e-6);
    let dmg_s = faction_damage_vector(500020, 100.0);
    assert!((dmg_s.thermal - 60.0).abs() < 1e-6);
    let dmg_a = faction_damage_vector(500011, 100.0);
    assert!((dmg_a.explosive - 60.0).abs() < 1e-6);
    let dmg_def = faction_damage_vector(999999, 100.0);
    assert!((dmg_def.em - 25.0).abs() < 1e-6);

    assert_eq!(regional_pirate_faction_id(&ctx, 30_000_142), 500_010);
    assert_eq!(clone_soldier_tier_for_security(0.6), None);
    assert_eq!(clone_soldier_tier_for_security(0.4), Some("Trainer"));
    assert_eq!(clone_soldier_tier_for_security(0.3), Some("Recruiter"));
    assert_eq!(clone_soldier_tier_for_security(0.2), Some("Transporter"));
    assert_eq!(clone_soldier_tier_for_security(0.1), Some("Negotiator"));
    assert!(clone_soldier_type_for_security(0.3).is_some());
    assert!(clone_soldier_type_for_security_and_faction(&ctx.sde, 0.2, 500_011).is_some());
    assert_eq!(compute_sec_status_gain(5.0, 100_000.0), 0.0);
    assert_eq!(compute_sec_status_gain(0.0, 0.0), 0.0);
    assert!(compute_sec_status_gain(0.0, 250_000.0) > 0.0);

    // Add a custom NPC spawn group and test spawn_group + step_ai + handle_npc_destroyed
    let sys_id = 30_000_142;
    let group_id = 880_001;
    let mut ctx_mut = test_ctx();
    Arc::make_mut(&mut ctx_mut.sde).npc_spawn_groups.insert(
        group_id,
        NpcSpawnGroupInfo {
            spawn_group_id: group_id,
            faction_id: 500_010,
            ship_class: 1,
            group_id: 550,
            group_name: "Test Pirate".into(),
            npc_type_ids: vec![597],
        },
    );

    let player_ship = 100_080_001;
    let base_pos = DVec3::new(20_000.0, 0.0, 0.0);
    spawn_test_ship(&ctx_mut, sys_id, player_ship, 90_000_001, 587, base_pos);

    let mut mgr = NpcSpawnManager::new();
    assert!(mgr.spawn_group(&ctx_mut, sys_id, 999_888, base_pos).await.is_err());

    // Spawn 5km away -> within orbit/optimal -> Attack!
    let npcs = mgr
        .spawn_group(&ctx_mut, sys_id, group_id, base_pos + DVec3::new(3_000.0, 0.0, 0.0))
        .await
        .unwrap();
    assert_eq!(npcs.len(), 1);
    let npc_bid = npcs[0];
    ctx_mut.advance_secs(5);
    mgr.step_ai(&ctx_mut, sys_id, 10, 1.0).await;
    assert_eq!(mgr.entities[&npc_bid].state, NpcAiState::Attack);

    // Move NPC far away (> 60km from any player) -> transitions to Idle
    let sys_handle = ctx_mut.systems.get_or_spawn(sys_id);
    let _ = sys_handle.set_ball_position(npc_bid, base_pos + DVec3::new(90_000.0, 0.0, 0.0)).await;
    mgr.step_ai_sync(&ctx_mut, sys_id, 11, 1.0);
    assert_eq!(mgr.entities[&npc_bid].state, NpcAiState::Idle);

    // Belt rat spawns in high-sec (real belt system & no-belt system) & wormhole
    let empty_belt_rats = mgr.spawn_belt_rats_for_system(&ctx_mut, sys_id).await.unwrap();
    assert!(empty_belt_rats.is_empty());
    let belt_sys_id = ctx_mut
        .sde
        .solar_systems
        .keys()
        .copied()
        .find(|&sid| {
            ctx_mut
                .sde
                .celestials_in_system(sid)
                .iter()
                .any(|c| c.group_id == 9)
        })
        .unwrap_or(30_000_144);
    let hi_rats = mgr
        .spawn_belt_rats_for_system(&ctx_mut, belt_sys_id)
        .await
        .unwrap();
    assert!(!hi_rats.is_empty());
    let wh_rats = mgr.spawn_belt_rats_for_system(&ctx_mut, 31_000_005).await.unwrap();
    assert_eq!(wh_rats.len(), 2);

    // Destroy tracked NPC and untracked error
    assert!(mgr.handle_npc_destroyed(&ctx_mut, sys_id, 999_777, 90_000_001, 20_000).is_err());
    let bounty = mgr.handle_npc_destroyed(&ctx_mut, sys_id, npc_bid, 90_000_001, 20_000).unwrap();
    assert!(bounty > 0.0);
    assert!(mgr.respawn_timers.contains_key(&(sys_id, group_id)));
}

#[tokio::test]
async fn crimewatch_step_concord_guards_despawns_tick_and_kill_right_reuse() {
    use super::crimewatch::{
        tick_concord_responses, AggressionEvaluation, CharacterCrimeWatchState, CrimeWatchDirector,
    };
    use crate::services::combat_crimewatch::{with_mut_combat_runtime, SecStatusTransaction};

    let ctx = test_ctx();
    let sys_id = 30_000_142u32;
    let now = ctx.now_unix();
    let sys_handle = ctx.systems.get_or_spawn(sys_id);

    // 1. try_with_mut_crimewatch persists pending_concord_despawns diff (`L365-369`)
    let due_ball = 100_085_001i64;
    let keep_ball = 100_085_002i64;
    for bid in [due_ball, keep_ball] {
        let _ = sys_handle
            .spawn_ball(Ball {
                id: bid,
                type_id: 3764,
                position: DVec3::new(500.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 100.0,
                inertia: 1.0,
                mass: 1_000.0,
                radius: 30.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await;
    }
    let _: Result<(), String> = ctx.crimewatch.try_with_mut_crimewatch(|dir| {
        dir.pending_concord_despawns.push((sys_id, due_ball, now));
        dir.pending_concord_despawns
            .push((sys_id, keep_ball, now + 500));
        Ok(())
    });
    assert_eq!(ctx.crimewatch.read().unwrap().pending_concord_despawns.len(), 2);

    // 2. Criminal flag expiration & invalid criminal ship guards in `CrimeWatchDirector::step_concord` (`L549-614`)
    let mut cw = CrimeWatchDirector::new();
    cw.pending_concord_despawns.push((sys_id, due_ball, now));
    cw.pending_concord_despawns.push((sys_id, keep_ball, now + 500));

    // Expired criminal flag clears pending_concord_arrival (`L562-565`)
    cw.states.insert(
        90_085_001,
        CharacterCrimeWatchState {
            criminal_flag_until_unix: now,
            pending_concord_arrival: Some((sys_id, now, 100_085_101, 90_000_002)),
            ..Default::default()
        },
    );
    // Missing character (`L579`)
    cw.states.insert(
        90_085_002,
        CharacterCrimeWatchState {
            pending_concord_arrival: Some((sys_id, now, 100_085_102, 90_000_002)),
            ..Default::default()
        },
    );
    // Docked character (`L581-585`), wrong system (`L583`), zero/mismatched ship (`L588`), missing ship item (`L591`), capsule (`L593-600`), unspawned ball (`L607-613`)
    let char_docked = 90_085_003i64;
    let char_wrong_sys = 90_085_004i64;
    let char_mismatch_ship = 90_085_005i64;
    let char_missing_item = 90_085_006i64;
    let char_capsule = 90_085_007i64;
    let char_no_ball = 90_085_008i64;
    let capsule_ship = 100_085_107i64;
    let unspawned_ship = 100_085_108i64;
    let ship_wrong_sys = 100_085_104i64;

    ctx.db.with_mut_snapshot(|snap| {
        let tmpl = snap.characters[&90_000_001].clone();
        let mk_char = |cid: i64, st: Option<i64>, sid: u32, ship: i64| {
            let mut c = tmpl.clone();
            c.character_id = cid;
            c.user_id = cid as u64;
            c.name = format!("CW{cid}");
            c.station_id = st;
            c.solar_system_id = sid;
            c.active_ship_id = ship;
            c.security_status = 2.0;
            c
        };
        snap.characters.insert(char_docked, mk_char(char_docked, Some(60_003_760), sys_id, 100_085_103));
        snap.characters.insert(char_wrong_sys, mk_char(char_wrong_sys, None, 30_000_144, ship_wrong_sys));
        snap.characters.insert(char_mismatch_ship, mk_char(char_mismatch_ship, None, sys_id, 100_085_199));
        snap.characters.insert(char_missing_item, mk_char(char_missing_item, None, sys_id, 100_085_106));
        snap.characters.insert(char_capsule, mk_char(char_capsule, None, sys_id, capsule_ship));
        snap.characters.insert(char_no_ball, mk_char(char_no_ball, None, sys_id, unspawned_ship));

        let mk_item = |iid: i64, tid: u32, owner: i64| eve_db::InventoryItemRecord {
            item_id: iid,
            type_id: tid,
            owner_id: owner,
            location_id: sys_id as i64,
            flag: InventoryFlag::None,
            quantity: 1,
            singleton: true,
            version: 1,
            ..Default::default()
        };
        snap.items.insert(ship_wrong_sys, mk_item(ship_wrong_sys, 587, char_wrong_sys));
        snap.items.insert(capsule_ship, mk_item(capsule_ship, 670, char_capsule));
        snap.items.insert(unspawned_ship, mk_item(unspawned_ship, 587, char_no_ball));
    });
    for (cid, sid_item) in [
        (char_docked, 100_085_103i64),
        (char_wrong_sys, ship_wrong_sys),
        (char_mismatch_ship, 100_085_105i64),
        (char_missing_item, 100_085_106i64),
        (char_capsule, capsule_ship),
        (char_no_ball, unspawned_ship),
    ] {
        cw.states.insert(
            cid,
            CharacterCrimeWatchState {
                pending_concord_arrival: Some((sys_id, now, sid_item, 90_000_002)),
                ..Default::default()
            },
        );
    }

    let guard_reports = cw.step_concord(&ctx, now, 77).await;
    assert!(guard_reports.is_empty());
    assert_eq!(cw.pending_concord_despawns.len(), 1);
    assert!(sys_handle.get_ball_snapshot(due_ball).await.unwrap().is_none());
    assert!(sys_handle.get_ball_snapshot(keep_ball).await.unwrap().is_some());

    // 3. tick_concord_responses (`L819-858`) with valid live criminal, recent sec-loss tx (`L741-748`),
    // persisted DB kill_right (`L770-775`), AND self-aggression (`L796` -> kill_right_id == 0)
    let live_crim = 90_000_001i64;
    let live_ship = 100_085_201i64;
    let self_ship = 100_085_202i64;
    let self_char = 90_085_020i64;
    spawn_test_ship(&ctx, sys_id, live_ship, live_crim, 587, DVec3::new(2_000.0, 0.0, 0.0));
    spawn_test_ship(&ctx, sys_id, self_ship, self_char, 587, DVec3::new(4_000.0, 0.0, 0.0));
    with_mut_combat_runtime(&ctx, |rt| {
        rt.kill_right_expires_unix.insert(980_001, now + 3_600);
        rt.sec_status_transactions.insert(
            live_crim,
            vec![SecStatusTransaction {
                timestamp_unix: now,
                old_sec: 2.0,
                new_sec: 1.8,
                delta: -0.2,
                reason: "Illegal Aggression".into(),
                solar_system_id: sys_id,
                event_type_id: 1,
                other_owner_id: Some(90_000_002),
                other_type_id: None,
            }],
        );
    });
    ctx.db.with_mut_snapshot(|snap| {
        let mut sc = snap.characters[&live_crim].clone();
        if let Some(ch) = snap.characters.get_mut(&live_crim) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = sys_id;
            ch.active_ship_id = live_ship;
        }
        sc.character_id = self_char;
        sc.user_id = self_char as u64;
        sc.name = "SelfCrim".into();
        sc.solar_system_id = sys_id;
        sc.active_ship_id = self_ship;
        sc.security_status = 2.0;
        sc.station_id = None;
        sc.structure_id = None;
        snap.characters.insert(self_char, sc);
        snap.kill_rights.insert(
            980_001,
            eve_db::KillRightRecord {
                kill_right_id: 980_001,
                owner_id: 90_000_002,
                target_id: live_crim,
                activation_fee: eve_db::IskCents::ZERO,
                is_active_suspect: false,
            },
        );
        snap.combat_crimewatch_state
            .kill_right_expires_unix
            .insert(980_001, now + 3_600);
    });
    {
        let mut guard = ctx.crimewatch.write().unwrap();
        let eval = guard.handle_combat_aggression(live_crim, 90_000_002, live_ship, sys_id, 0.9, false, now);
        assert!(matches!(eval, AggressionEvaluation::CriminalFlagTriggered { .. }));
        guard.states.insert(
            self_char,
            CharacterCrimeWatchState {
                criminal_flag_until_unix: now + 900,
                pending_concord_arrival: Some((sys_id, now + 1, self_ship, self_char)),
                ..Default::default()
            },
        );
    }

    tick_concord_responses(&ctx, now + 100).await;
    let reports = ctx.crimewatch.concord_reports();
    assert!(
        reports.iter().any(|r| r.criminal_char_id == live_crim && r.kill_right_id == 980_001),
        "expected report for {live_crim} with kr 980_001, got: {reports:?}"
    );
    assert!(reports.iter().any(|r| r.criminal_char_id == self_char && r.kill_right_id == 0));
    assert!(ctx.crimewatch.read().unwrap().pending_concord_despawns.iter().any(|&(s, b, _)| s == sys_id && b == keep_ball));
}

