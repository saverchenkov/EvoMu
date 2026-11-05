//! Unit and mutation-killing tests for `combat::crimewatch`.

use super::*;
use crate::router::ServerContext;
use eve_db::{
    InventoryFlag, InventoryItemRecord, PersistedInsurgencyCampaign,
};
use eve_destiny::{Ball, MovementMode};
use eve_sde::SdeRegistry;
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

fn spawn_ship(
    ctx: &ServerContext,
    sys_id: u32,
    ship_id: i64,
    owner_id: i64,
    type_id: u32,
    pos: DVec3,
) {
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
                insured_payout_isk: 100_000.0,
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
    cs.capacitor = 200.0;
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
fn sec_status_loss_and_concord_delay_exact_boundaries() {
    // Boundary at current_sec <= -10.0
    assert_eq!(compute_sec_status_loss(-10.0, 5.0, false, 1.0), 0.0);
    assert_eq!(compute_sec_status_loss(-10.5, 5.0, true, 1.0), 0.0);
    assert!(compute_sec_status_loss(-9.99, 5.0, false, 1.0) > 0.0);

    // Exact arithmetic verification for ship (0.025) vs pod (0.25)
    // current_sec = 2.0, target_sec = 7.0, sys_sec = 0.8:
    // standing_factor = 1.0 + (7.0 - 2.0) / 100.0 = 1.05
    // ship mod_pct = 0.025 * 0.8 * 1.05 = 0.021 -> (10.0 - 2.0) * 0.021 = 0.168
    let ship_loss = compute_sec_status_loss(2.0, 7.0, false, 0.8);
    assert!((ship_loss - 0.168).abs() < 1e-12);
    let pod_loss = compute_sec_status_loss(2.0, 7.0, true, 0.8);
    assert!((pod_loss - 1.68).abs() < 1e-12);

    // Negative sys_sec clamped to 0.0; extreme negative target_sec clamped to 0.0; current_sec > 10.0 clamped to 0.0
    assert_eq!(compute_sec_status_loss(0.0, 0.0, false, -0.3), 0.0);
    assert_eq!(compute_sec_status_loss(5.0, -150.0, true, 1.0), 0.0);
    assert_eq!(compute_sec_status_loss(10.5, 5.0, true, 1.0), 0.0);

    // concord_delay_for_security across all high-sec bands and low-sec fallback
    assert_eq!(concord_delay_for_security(1.0), 6.0);
    assert_eq!(concord_delay_for_security(0.9), 6.0);
    assert_eq!(concord_delay_for_security(0.85), 6.0);
    assert_eq!(concord_delay_for_security(0.8), 8.0);
    assert_eq!(concord_delay_for_security(0.7), 11.0);
    assert_eq!(concord_delay_for_security(0.6), 14.0);
    assert_eq!(concord_delay_for_security(0.5), 19.0);
    assert_eq!(
        concord_delay_for_security(0.4),
        crate::formulas::concord_response_delay_sec(0.4)
    );
}

#[test]
fn aggression_manager_and_director_exact_timer_and_security_boundaries() {
    let mut mgr = AggressionManager::new();
    let rec = mgr.record_aggression(101, 202, 30_000_142, true, true, 1_000);
    assert_eq!(rec.aggressor_char_id, 101);
    assert_eq!(rec.victim_char_id, 202);
    assert_eq!(rec.system_id, 30_000_142);
    assert_eq!(rec.started_unix, 1_000);
    assert_eq!(rec.expires_unix, 1_900);
    assert!(rec.is_legal);
    assert!(rec.is_pod);

    assert!(mgr.get_aggression(101, 202, 1_899).is_some());
    assert!(mgr.get_aggression(101, 202, 1_900).is_none());
    assert!(!mgr.is_aggressor_against(202, 101, 1_500));

    mgr.record_aggression(103, 202, 30_000_142, false, false, 1_100);
    mgr.record_aggression(104, 999, 30_000_142, false, false, 1_100);
    assert_eq!(mgr.list_active_aggressors_for_victim(202, 1_900), vec![103]);
    assert!(mgr.list_active_aggressors_for_victim(202, 2_000).is_empty());

    let mut dir = CrimeWatchDirector::new();
    let now = 10_000;
    dir.trigger_weapon_timer(501, now);
    assert_eq!(dir.states[&501].weapon_timer_until_unix, now + 60);
    dir.trigger_suspect_flag(501, now);
    assert_eq!(dir.states[&501].suspect_flag_until_unix, now + 900);
    assert!(dir.is_suspect(501, now + 899));
    assert!(!dir.is_suspect(501, now + 900));
    assert!(!dir.is_suspect(999_999, now));

    // Remote assistance at 0.45 (high-sec boundary) vs 0.44 (low-sec boundary)
    let rem_hi = dir.handle_criminal_remote_assistance(502, 503, 9_001, 30_000_142, 0.45, now);
    assert_eq!(
        rem_hi,
        AggressionEvaluation::CriminalFlagTriggered {
            concord_delay_sec: 19.0,
            arrival_unix: now + 19,
        }
    );
    assert_eq!(dir.states[&502].weapon_timer_until_unix, now + 60);
    assert_eq!(dir.states[&502].pvp_timer_until_unix, now + 900);
    assert_eq!(dir.states[&502].criminal_flag_until_unix, now + 900);
    assert_eq!(
        dir.states[&502].pending_concord_arrival,
        Some((30_000_142, now + 19, 9_001, 0))
    );

    let rem_lo = dir.handle_criminal_remote_assistance(504, 503, 9_002, 30_002_000, 0.44, now);
    assert_eq!(
        rem_lo,
        AggressionEvaluation::CriminalFlagTriggered {
            concord_delay_sec: 0.0,
            arrival_unix: now,
        }
    );
    assert!(dir.states[&504].pending_concord_arrival.is_none());

    // Corruption stage 4 vs 5 at sys_sec == 0.45
    let sys_id = 30_000_142;
    dir.system_corruption_stages.insert(sys_id, 4);
    let stg4_eval =
        dir.handle_combat_aggression_with_pod(601, 602, 8_001, sys_id, 0.45, false, false, now);
    assert_eq!(
        stg4_eval,
        AggressionEvaluation::CriminalFlagTriggered {
            concord_delay_sec: 19.0,
            arrival_unix: now + 19,
        }
    );

    dir.system_corruption_stages.insert(sys_id, 5);
    let stg5_ship =
        dir.handle_combat_aggression_with_pod(603, 602, 8_002, sys_id, 0.45, false, false, now);
    assert_eq!(stg5_ship, AggressionEvaluation::SuspectFlagTriggered);
    assert_eq!(dir.states[&603].suspect_flag_until_unix, now + 900);
    assert!(dir.states[&603].pending_concord_arrival.is_none());

    // Low-sec boundary: 0.44 vs 0.01 vs 0.0 (null-sec)
    let lo_ship =
        dir.handle_combat_aggression_with_pod(604, 602, 8_003, 30_002_001, 0.01, false, false, now);
    assert_eq!(lo_ship, AggressionEvaluation::SuspectFlagTriggered);
    let lo_pod =
        dir.handle_combat_aggression_with_pod(604, 602, 8_003, 30_002_001, 0.01, false, true, now);
    assert_eq!(
        lo_pod,
        AggressionEvaluation::CriminalFlagTriggered {
            concord_delay_sec: 0.0,
            arrival_unix: now,
        }
    );
    assert_eq!(dir.states[&604].suspect_flag_until_unix, 0);
    assert_eq!(dir.states[&604].criminal_flag_until_unix, now + 900);

    let null_pod =
        dir.handle_combat_aggression_with_pod(605, 602, 8_004, 30_003_001, 0.0, false, true, now);
    assert_eq!(null_pod, AggressionEvaluation::LegalEngagement);
    let wh_ship =
        dir.handle_combat_aggression_with_pod(606, 602, 8_005, 31_000_001, -1.0, false, false, now);
    assert_eq!(wh_ship, AggressionEvaluation::LegalEngagement);
}

#[test]
fn crimewatch_lock_corruption_campaigns_and_report_ring_buffer() {
    let ctx = test_ctx();
    let now = ctx.now_unix();
    let sys_active = 30_000_142;
    let sys_future = 30_000_144;
    let sys_expired = 30_002_187;

    ctx.db.with_mut_snapshot(|snap| {
        // Active at exact boundary now == forecasting_end_unix
        snap.insurgency_runtime.campaigns.insert(
            1,
            PersistedInsurgencyCampaign {
                campaign_id: 1,
                pirate_faction_id: 500_010,
                target_faction_id: 500_001,
                fob_solar_system_id: sys_active,
                systems: vec![sys_active, 30_000_145],
                created_at_unix: now.saturating_sub(100),
                forecasting_end_unix: now,
                active_end_unix: now + 100,
                winddown_end_unix: now + 200,
                corruption_points: HashMap::from([(sys_active, 100), (30_000_145, 100_000)]),
                suppression_points: HashMap::new(),
            },
        );
        // System state has higher stage (4) than campaign 1 (stage 1) on sys_active
        snap.system_states
            .entry(sys_active)
            .or_default()
            .insurgency_corruption_stage = 4;
        // Future campaign (now < forecasting_end_unix) -> ignored
        snap.insurgency_runtime.campaigns.insert(
            2,
            PersistedInsurgencyCampaign {
                campaign_id: 2,
                pirate_faction_id: 500_010,
                target_faction_id: 500_001,
                fob_solar_system_id: sys_future,
                systems: vec![sys_future],
                created_at_unix: now,
                forecasting_end_unix: now + 1,
                active_end_unix: now + 100,
                winddown_end_unix: now + 200,
                corruption_points: HashMap::from([(sys_future, 100_000)]),
                suppression_points: HashMap::new(),
            },
        );
        // Expired campaign (now == winddown_end_unix) -> ignored
        snap.insurgency_runtime.campaigns.insert(
            3,
            PersistedInsurgencyCampaign {
                campaign_id: 3,
                pirate_faction_id: 500_010,
                target_faction_id: 500_001,
                fob_solar_system_id: sys_expired,
                systems: vec![sys_expired],
                created_at_unix: now.saturating_sub(500),
                forecasting_end_unix: now.saturating_sub(400),
                active_end_unix: now.saturating_sub(100),
                winddown_end_unix: now,
                corruption_points: HashMap::from([(sys_expired, 100_000)]),
                suppression_points: HashMap::new(),
            },
        );
    });

    let lock = CrimeWatchLock::new(Arc::clone(&ctx.db));
    let (stage_active, stage_camp) = lock.with_crimewatch(|dir| {
        assert!(!dir.system_corruption_stages.contains_key(&sys_future));
        assert!(!dir.system_corruption_stages.contains_key(&sys_expired));
        (
            dir.system_corruption_stages.get(&sys_active).copied(),
            dir.system_corruption_stages.get(&30_000_145).copied(),
        )
    });
    assert_eq!(stage_active, Some(4));
    assert_eq!(stage_camp, Some(5));
    lock.with_mut_crimewatch(|dir| {
        dir.system_corruption_stages.insert(sys_active, 5);
    });
    assert_eq!(
        ctx.db.with_snapshot(|s| s.system_states[&sys_active].insurgency_corruption_stage),
        5
    );

    // Empty push is no-op; 32 reports kept; 33rd evicts 1st
    lock.push_concord_reports(&[]);
    assert!(lock.concord_reports().is_empty());
    let rep_32: Vec<ConcordExecutionReport> = (1..=32)
        .map(|i| ConcordExecutionReport {
            criminal_char_id: i,
            victim_char_id: 0,
            concord_ball_id: i,
            kill_right_id: 0,
            webbed_speed_fraction: 0.0,
            warp_scramble_strength: 100,
            capacitor_after_neut: 0.0,
            ecm_jammed: true,
            locks_cleared: 0,
            death_result: Err("test".into()),
        })
        .collect();
    lock.push_concord_reports(&rep_32);
    assert_eq!(lock.concord_reports().len(), 32);
    assert_eq!(lock.concord_reports()[0].criminal_char_id, 1);

    lock.push_concord_reports(&[ConcordExecutionReport {
        criminal_char_id: 33,
        victim_char_id: 0,
        concord_ball_id: 33,
        kill_right_id: 0,
        webbed_speed_fraction: 0.0,
        warp_scramble_strength: 100,
        capacitor_after_neut: 0.0,
        ecm_jammed: true,
        locks_cleared: 0,
        death_result: Err("test".into()),
    }]);
    let after = lock.concord_reports();
    assert_eq!(after.len(), 32);
    assert_eq!(after[0].criminal_char_id, 2);
    assert_eq!(after[31].criminal_char_id, 33);
}

#[tokio::test]
async fn step_concord_arrival_boundary_and_bounty_sec_gain_intervals() {
    let ctx = test_ctx();
    let sys_id = 30_000_142;
    let crim = 90_000_001;
    let victim = 90_000_002;
    let ship_id = 100_110_001;
    spawn_ship(&ctx, sys_id, ship_id, crim, 587, DVec3::new(500.0, 0.0, 0.0));

    let now = ctx.now_unix();
    let mut cw = CrimeWatchDirector::new();
    let eval = cw.handle_combat_aggression(crim, victim, ship_id, sys_id, 1.0, false, now);
    let arrival = match eval {
        AggressionEvaluation::CriminalFlagTriggered { arrival_unix, .. } => arrival_unix,
        other => panic!("expected CriminalFlagTriggered, got {other:?}"),
    };

    // At arrival - 1, CONCORD does not fire yet
    let early = cw.step_concord(&ctx, arrival - 1, 7).await;
    assert!(early.is_empty());
    assert!(cw.states[&crim].pending_concord_arrival.is_some());

    // Pre-create an active KillRight so step_concord reuses existing_kr
    crate::services::combat_crimewatch::create_kill_right(&ctx, 77_889, victim, crim, 0.0);

    let fired = cw.step_concord(&ctx, arrival, 7).await;
    assert_eq!(fired.len(), 1);
    assert_eq!(fired[0].kill_right_id, 77_889);

    // Non-positive bounty or killer_char_id returns early
    let char_id = 90_000_002;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.security_status = 1.0;
        }
    });
    credit_specialized_npc_bounty_and_sec_status(&ctx, sys_id, 900_001, char_id, 0.0);
    credit_specialized_npc_bounty_and_sec_status(&ctx, sys_id, 900_002, char_id, -50.0);
    credit_specialized_npc_bounty_and_sec_status(&ctx, sys_id, 900_003, 0, 100_000.0);
    assert_eq!(
        ctx.db.with_snapshot(|s| s.characters[&char_id].security_status),
        1.0
    );

    // 1st kill in 900s interval gains sec status; 2nd kill with smaller bounty does not; 3rd kill with larger bounty gains incremental difference
    credit_specialized_npc_bounty_and_sec_status(&ctx, sys_id, 900_010, char_id, 100_000.0);
    let sec1 = ctx.db.with_snapshot(|s| s.characters[&char_id].security_status);
    assert!(sec1 > 1.0);

    credit_specialized_npc_bounty_and_sec_status(&ctx, sys_id, 900_011, char_id, 80_000.0);
    let sec2 = ctx.db.with_snapshot(|s| s.characters[&char_id].security_status);
    assert!((sec2 - sec1).abs() < 1e-12);

    credit_specialized_npc_bounty_and_sec_status(&ctx, sys_id, 900_012, char_id, 300_000.0);
    let sec3 = ctx.db.with_snapshot(|s| s.characters[&char_id].security_status);
    assert!(sec3 > sec2);

    // Advance 900s -> interval resets and smaller bounty now grants gain again
    ctx.advance_secs(900);
    credit_specialized_npc_bounty_and_sec_status(&ctx, sys_id, 900_013, char_id, 50_000.0);
    let sec4 = ctx.db.with_snapshot(|s| s.characters[&char_id].security_status);
    assert!(sec4 > sec3);

    // At security_status == 5.0, no further gain is applied
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.security_status = 5.0;
        }
    });
    ctx.advance_secs(900);
    credit_specialized_npc_bounty_and_sec_status(&ctx, sys_id, 900_014, char_id, 500_000.0);
    assert_eq!(
        ctx.db.with_snapshot(|s| s.characters[&char_id].security_status),
        5.0
    );
}
