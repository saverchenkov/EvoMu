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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn crimewatch_stage5_pod_override_skip_guards_lock_drop_tick_and_bounty_dedup() {
    // 1. Stage 5 corruption overrides non-pod aggression to Suspect, NEVER pod aggression in highsec;
    //    is_legal=true yields LegalEngagement; director.get_aggression honors expires_unix boundary.
    let mut dir = CrimeWatchDirector::new();
    let sys_id = 30_000_142;
    let now = 20_000;
    dir.system_corruption_stages.insert(sys_id, 5);

    let legal = dir.handle_combat_aggression(701, 702, 7_001, sys_id, 0.9, true, now);
    assert_eq!(legal, AggressionEvaluation::LegalEngagement);
    assert_eq!(dir.states[&701].criminal_flag_until_unix, 0);
    assert_eq!(dir.states[&701].suspect_flag_until_unix, 0);
    assert!(dir.get_aggression(701, 702, now + 899).is_some());
    assert!(dir.get_aggression(701, 702, now + 900).is_none());

    let pod_in_stage5 =
        dir.handle_combat_aggression_with_pod(703, 702, 7_002, sys_id, 0.9, false, true, now);
    assert_eq!(
        pod_in_stage5,
        AggressionEvaluation::CriminalFlagTriggered {
            concord_delay_sec: 6.0,
            arrival_unix: now + 6,
        }
    );
    assert!(dir.states[&703].pending_concord_arrival.is_some());

    // 2. step_concord expiry + invalid target skip guards (expired flag, docked, wrong system, mismatched ship, capsule, off-grid)
    let ctx = test_ctx();
    let t_now = ctx.now_unix();
    let crim_docked = 90_000_001;
    let crim_capsule = 90_000_002;
    let crim_live = 90_000_003;
    let victim = 90_000_004;
    let pod_id = 1_000_991_001;
    let live_ship_id = 1_000_991_002;

    spawn_ship(&ctx, sys_id, live_ship_id, crim_live, 587, DVec3::new(10_000.0, 0.0, 0.0));
    spawn_ship(&ctx, sys_id, pod_id, crim_capsule, 670, DVec3::new(15_000.0, 0.0, 0.0));
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&crim_docked) {
            ch.station_id = Some(60_003_760);
            ch.active_ship_id = 1_000_991_099;
        }
        if !snap.characters.contains_key(&crim_live) {
            let mut c = snap.characters.values().next().cloned().unwrap();
            c.character_id = crim_live;
            c.station_id = None;
            c.structure_id = None;
            c.solar_system_id = sys_id;
            c.active_ship_id = live_ship_id;
            snap.characters.insert(crim_live, c);
        }
    });

    let mut cw = CrimeWatchDirector::new();
    // Expired criminal flag (now >= criminal_flag_until_unix) cleared cleanly without spawning
    let st_expired = cw.get_or_create(990_001);
    st_expired.criminal_flag_until_unix = t_now;
    st_expired.pending_concord_arrival = Some((sys_id, t_now, 1_000_991_098, victim));
    // Docked, capsule, wrong ship, and missing ball criminals are skipped cleanly
    cw.get_or_create(crim_docked).pending_concord_arrival = Some((sys_id, t_now, 1_000_991_099, victim));
    cw.get_or_create(crim_capsule).pending_concord_arrival = Some((sys_id, t_now, pod_id, victim));
    cw.get_or_create(crim_live).pending_concord_arrival = Some((sys_id, t_now, live_ship_id + 999, victim));
    let skipped = cw.step_concord(&ctx, t_now, 11).await;
    assert!(skipped.is_empty());
    assert_eq!(cw.states[&990_001].criminal_flag_until_unix, 0);
    assert!(cw.states[&990_001].pending_concord_arrival.is_none());

    // 3. CrimeWatchLock::write persistence + tick_concord_responses arrival dispatch & transient ball despawn boundary
    {
        let mut guard = ctx.crimewatch.write().unwrap();
        let eval = guard.handle_combat_aggression(crim_live, victim, live_ship_id, sys_id, 0.9, false, t_now);
        assert!(matches!(eval, AggressionEvaluation::CriminalFlagTriggered { .. }));
    }
    {
        let read_guard = ctx.crimewatch.read().unwrap();
        assert!(read_guard.states.contains_key(&crim_live));
        assert!(read_guard.get_aggression(crim_live, victim, t_now).is_some());
    }

    let arrival_unix = t_now + 6;
    tick_concord_responses(&ctx, arrival_unix).await;
    let reports = ctx.crimewatch.concord_reports();
    assert_eq!(reports.len(), 1);
    let concord_ball_id = reports[0].concord_ball_id;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    assert!(sys_handle.get_ball_snapshot(concord_ball_id).await.unwrap().is_some());

    // Before CONCORD_ON_GRID_DURATION_SECS elapses -> commander stays on grid; at exact boundary -> despawned
    tick_concord_responses(&ctx, arrival_unix + CONCORD_ON_GRID_DURATION_SECS - 1).await;
    assert!(sys_handle.get_ball_snapshot(concord_ball_id).await.unwrap().is_some());
    tick_concord_responses(&ctx, arrival_unix + CONCORD_ON_GRID_DURATION_SECS).await;
    assert!(sys_handle.get_ball_snapshot(concord_ball_id).await.unwrap().is_none());

    // 4. credit_specialized_npc_bounty_and_sec_status deduplicates identical npc_ball_id
    crate::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.pending_bounty_ticks.clear();
    });
    credit_specialized_npc_bounty_and_sec_status(&ctx, sys_id, 955_001, crim_live, 125_000.0);
    credit_specialized_npc_bounty_and_sec_status(&ctx, sys_id, 955_001, crim_live, 125_000.0);
    let credited_total = crate::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| {
        rt.pending_bounty_ticks.get(&(sys_id, crim_live)).copied().unwrap_or(0.0)
    });
    assert!((credited_total - 125_000.0).abs() < 1e-6);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tick_concord_responses_and_step_concord_edge_guards() {
    let ctx = test_ctx();
    let sys_id = 30_000_142;
    let crim = 90_000_001;
    let victim = 90_000_002;
    let ship_id = 100_210_001;
    spawn_ship(&ctx, sys_id, ship_id, crim, 587, DVec3::new(900.0, 0.0, 0.0));

    let now = ctx.now_unix();
    let sys_handle = ctx.systems.get_or_spawn(sys_id);

    // Spawn a dummy ball to verify pending_concord_despawns persistence across CrimeWatchWriteGuard Drop and tick_concord_responses
    let dummy_concord_ball = 100_210_999;
    sys_handle.try_spawn_ball_with_combat(
        Ball {
            id: dummy_concord_ball,
            type_id: 11957,
            position: DVec3::new(100.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 1_200.0,
            inertia: 0.1,
            mass: 10_000_000.0,
            radius: 120.0,
            speed_fraction: 1.0,
            mode: MovementMode::Stop,
        },
        ctx.build_type_combat_state(11957),
    );

    let arrival = {
        let mut cw = ctx.crimewatch.write().expect("crimewatch write lock");
        cw.pending_concord_despawns
            .push((sys_id, dummy_concord_ball, now + 30));
        match cw.handle_combat_aggression(crim, victim, ship_id, sys_id, 1.0, false, now) {
            AggressionEvaluation::CriminalFlagTriggered { arrival_unix, .. } => arrival_unix,
            other => panic!("expected CriminalFlagTriggered, got {other:?}"),
        }
    };

    // Before arrival: dummy ball remains in ballpark
    tick_concord_responses(&ctx, arrival - 1).await;
    assert!(sys_handle.get_ball_snapshot(dummy_concord_ball).await.unwrap().is_some());
    assert!(ctx.crimewatch.concord_reports().is_empty());

    // At arrival: CONCORD executes criminal ship and schedules CONCORD Commander despawn
    tick_concord_responses(&ctx, arrival).await;
    let reports = ctx.crimewatch.concord_reports();
    assert_eq!(reports.len(), 1);
    let concord_ball_id = reports[0].concord_ball_id;
    assert!(reports[0].death_result.is_ok());
    assert!(sys_handle.get_ball_snapshot(concord_ball_id).await.unwrap().is_some());

    // Before CONCORD_ON_GRID_DURATION_SECS elapses: CONCORD ball remains on grid, dummy ball at now+30 despawns
    tick_concord_responses(&ctx, now + 30).await;
    assert!(sys_handle.get_ball_snapshot(dummy_concord_ball).await.unwrap().is_none());
    assert!(sys_handle.get_ball_snapshot(concord_ball_id).await.unwrap().is_some());

    // After CONCORD_ON_GRID_DURATION_SECS elapses: CONCORD Commander ball is removed from grid
    tick_concord_responses(&ctx, arrival + CONCORD_ON_GRID_DURATION_SECS).await;
    assert!(sys_handle.get_ball_snapshot(concord_ball_id).await.unwrap().is_none());

    // Direct step_concord edge guards:
    // 1. Remaining pending despawn (now < remove_at), expired criminal flag, missing char, docked char,
    //    wrong ship_id, missing ship item, and missing ball in ballpark.
    let mut dir = CrimeWatchDirector::new();
    dir.pending_concord_despawns.push((sys_id, 888_777, now + 500));

    // Expired criminal flag clears pending arrival
    dir.states.insert(
        90_999_001,
        CharacterCrimeWatchState {
            character_id: 90_999_001,
            criminal_flag_until_unix: now + 5,
            pending_concord_arrival: Some((sys_id, now + 10, 100_000_001, victim)),
            ..Default::default()
        },
    );
    // Missing character in DB
    dir.states.insert(
        90_999_002,
        CharacterCrimeWatchState {
            character_id: 90_999_002,
            criminal_flag_until_unix: now + 900,
            pending_concord_arrival: Some((sys_id, now, 100_000_001, victim)),
            ..Default::default()
        },
    );
    // Docked character (station_id = Some)
    let docked_char = 90_999_003;
    let wrong_ship_char = 90_999_004;
    let missing_item_char = 90_999_005;
    let missing_ball_char = 90_999_006;
    ctx.db.with_mut_snapshot(|snap| {
        let base_ch = snap.characters[&crim].clone();

        let mut c3 = base_ch.clone();
        c3.character_id = docked_char;
        c3.station_id = Some(60_003_760);
        c3.solar_system_id = sys_id;
        c3.active_ship_id = 100_220_003;
        snap.characters.insert(docked_char, c3);

        let mut c4 = base_ch.clone();
        c4.character_id = wrong_ship_char;
        c4.station_id = None;
        c4.structure_id = None;
        c4.solar_system_id = sys_id;
        c4.active_ship_id = 100_220_004;
        snap.characters.insert(wrong_ship_char, c4);

        let mut c5 = base_ch.clone();
        c5.character_id = missing_item_char;
        c5.station_id = None;
        c5.structure_id = None;
        c5.solar_system_id = sys_id;
        c5.active_ship_id = 100_220_005; // not in snap.items
        snap.characters.insert(missing_item_char, c5);

        let mut c6 = base_ch;
        c6.character_id = missing_ball_char;
        c6.station_id = None;
        c6.structure_id = None;
        c6.solar_system_id = sys_id;
        c6.active_ship_id = 100_220_006;
        snap.characters.insert(missing_ball_char, c6);
        snap.items.insert(
            100_220_006,
            InventoryItemRecord {
                item_id: 100_220_006,
                type_id: 587,
                owner_id: missing_ball_char,
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
    for (cid, sid) in [
        (docked_char, 100_220_003),
        (wrong_ship_char, 100_999_999),
        (missing_item_char, 100_220_005),
        (missing_ball_char, 100_220_006),
    ] {
        dir.states.insert(
            cid,
            CharacterCrimeWatchState {
                character_id: cid,
                criminal_flag_until_unix: now + 900,
                pending_concord_arrival: Some((sys_id, now, sid, victim)),
                ..Default::default()
            },
        );
    }

    let edge_reports = dir.step_concord(&ctx, now + 10, 99).await;
    assert!(edge_reports.is_empty());
    assert_eq!(dir.pending_concord_despawns, vec![(sys_id, 888_777, now + 500)]);
    assert_eq!(dir.states[&90_999_001].criminal_flag_until_unix, 0);
    assert!(dir.states[&90_999_001].pending_concord_arrival.is_none());

    // 2. victim_char_id == 0 -> kill_right_id == 0, plus pre-logged sec_status_transaction skips duplicate sec penalty
    let ship_id_2 = 100_210_002;
    spawn_ship(&ctx, sys_id, ship_id_2, crim, 587, DVec3::new(1200.0, 0.0, 0.0));
    crate::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.sec_status_transactions.entry(crim).or_default().push(
            crate::services::combat_crimewatch::SecStatusTransaction {
                timestamp_unix: ctx.now_unix(),
                old_sec: -0.5,
                new_sec: -1.0,
                delta: -0.5,
                reason: "recent".into(),
                solar_system_id: sys_id,
                event_type_id: 0,
                other_owner_id: None,
                other_type_id: None,
            },
        );
    });
    let mut dir2 = CrimeWatchDirector::new();
    dir2.states.insert(
        crim,
        CharacterCrimeWatchState {
            character_id: crim,
            criminal_flag_until_unix: ctx.now_unix() + 900,
            pending_concord_arrival: Some((sys_id, ctx.now_unix(), ship_id_2, 0)),
            ..Default::default()
        },
    );
    let rep2 = dir2.step_concord(&ctx, ctx.now_unix(), 11).await;
    assert_eq!(rep2.len(), 1);
    assert_eq!(rep2[0].kill_right_id, 0);

    // 3. Kill right expiry present only in snap.combat_crimewatch_state (not in runtime map) is reused
    let ship_id_3 = 100_210_003;
    spawn_ship(&ctx, sys_id, ship_id_3, crim, 587, DVec3::new(1500.0, 0.0, 0.0));
    let db_kr_id = 88_991;
    let now3 = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.kill_rights.clear();
        snap.kill_rights.insert(
            db_kr_id,
            eve_db::KillRightRecord {
                kill_right_id: db_kr_id,
                owner_id: victim,
                target_id: crim,
                activation_fee: eve_db::IskCents(0),
                is_active_suspect: false,
            },
        );
        snap.combat_crimewatch_state
            .kill_right_expires_unix
            .clear();
        snap.combat_crimewatch_state
            .kill_right_expires_unix
            .insert(db_kr_id, now3 + 86_400);
    });
    ctx.combat_ephemeral.write().unwrap().kill_right_expires_unix.clear();
    ctx.combat_ephemeral.write().unwrap().synced_db_epoch = 0;
    let mut dir3 = CrimeWatchDirector::new();
    dir3.states.insert(
        crim,
        CharacterCrimeWatchState {
            character_id: crim,
            criminal_flag_until_unix: now3 + 900,
            pending_concord_arrival: Some((sys_id, now3, ship_id_3, victim)),
            ..Default::default()
        },
    );
    let rep3 = dir3.step_concord(&ctx, now3, 12).await;
    assert_eq!(rep3.len(), 1);
    assert_eq!(rep3[0].kill_right_id, db_kr_id);
}

#[test]
fn crimewatch_lock_guards_try_with_mut_and_director_edge_branches() {
    // 1. Exact sec-status loss boundaries at current_sec == 10.0 and standing_factor == 0.0
    assert_eq!(compute_sec_status_loss(10.0, 10.0, false, 1.0), 0.0);
    assert_eq!(compute_sec_status_loss(0.0, -100.0, true, 1.0), 0.0);
    assert!(compute_sec_status_loss(0.0, -99.0, true, 1.0) > 0.0);

    // 2. AggressionManager overwrite and exact expiry boundary
    let mut mgr = AggressionManager::new();
    let r1 = mgr.record_aggression(10, 20, 30_000_142, false, false, 1_000);
    assert_eq!(r1.expires_unix, 1_900);
    assert!(mgr.is_aggressor_against(10, 20, 1_899));
    assert!(!mgr.is_aggressor_against(10, 20, 1_900));
    let r2 = mgr.record_aggression(10, 20, 30_000_142, true, true, 1_500);
    assert_eq!(r2.expires_unix, 2_400);
    assert!(r2.is_legal);
    assert!(r2.is_pod);

    // 3. Legal engagement & Stage-5 high-sec pod attack
    let mut dir = CrimeWatchDirector::new();
    let now = 50_000;
    let legal = dir.handle_combat_aggression(10, 20, 1001, 30_000_142, 1.0, true, now);
    assert_eq!(legal, AggressionEvaluation::LegalEngagement);
    assert_eq!(dir.states[&10].weapon_timer_until_unix, now + 60);
    assert_eq!(dir.states[&10].pvp_timer_until_unix, now + 900);
    assert_eq!(dir.states[&10].criminal_flag_until_unix, 0);
    assert_eq!(dir.states[&10].suspect_flag_until_unix, 0);
    assert!(dir.get_aggression(10, 20, now).is_some());

    dir.system_corruption_stages.insert(30_000_142, 5);
    let pod_at_stg5 =
        dir.handle_combat_aggression_with_pod(11, 20, 1002, 30_000_142, 0.9, false, true, now);
    assert_eq!(
        pod_at_stg5,
        AggressionEvaluation::CriminalFlagTriggered {
            concord_delay_sec: 6.0,
            arrival_unix: now + 6,
        }
    );
    assert!(dir.states[&11].pending_concord_arrival.is_some());

    // 4. CrimeWatchLock::read(), write() (no-op drop + mutated drop), try_with_mut_crimewatch (Ok + Err)
    let ctx = test_ctx();
    let lock = CrimeWatchLock::new(Arc::clone(&ctx.db));
    {
        let rg = lock.read().unwrap();
        assert!(rg.states.is_empty());
    }
    {
        let wg_noop = lock.write().unwrap();
        assert!(wg_noop.states.is_empty());
    }
    assert!(!ctx.db.with_snapshot(|s| s.dirty_hints.crimewatch_dirty));
    {
        let mut wg = lock.write().unwrap();
        wg.trigger_suspect_flag(90_000_001, now);
        wg.pending_concord_despawns.push((30_000_142, 999, now + 10));
    }
    assert!(ctx.db.with_snapshot(|s| s.dirty_hints.crimewatch_dirty && !s.dirty_hints.system_states_dirty));
    assert!(lock.with_crimewatch(|d| d.is_suspect(90_000_001, now + 10)));
    assert_eq!(lock.with_crimewatch(|d| d.pending_concord_despawns.len()), 1);

    // Write guard drop when only aggression_mgr.records changes vs when system_corruption_stages changes
    {
        let mut wg_agg = lock.write().unwrap();
        wg_agg.aggression_mgr.record_aggression(90_000_001, 90_000_002, 30_000_142, false, false, now);
    }
    assert!(ctx.db.with_snapshot(|s| s.dirty_hints.crimewatch_dirty && !s.dirty_hints.system_states_dirty));
    {
        let mut wg_stg = lock.write().unwrap();
        wg_stg.system_corruption_stages.insert(30_000_142, 2);
    }
    assert!(ctx.db.with_snapshot(|s| s.dirty_hints.crimewatch_dirty && s.dirty_hints.system_states_dirty));

    // with_mut_crimewatch with stages_changed == false vs true
    lock.with_mut_crimewatch(|d| {
        d.trigger_weapon_timer(90_000_001, now);
    });
    assert!(ctx.db.with_snapshot(|s| s.dirty_hints.crimewatch_dirty && !s.dirty_hints.system_states_dirty));
    lock.with_mut_crimewatch(|d| {
        d.system_corruption_stages.insert(30_000_142, 4);
    });
    assert!(ctx.db.with_snapshot(|s| s.dirty_hints.crimewatch_dirty && s.dirty_hints.system_states_dirty));

    // try_with_mut_crimewatch with stages_changed == false vs true
    let ok_no_stg: Result<(), String> = lock.try_with_mut_crimewatch(|d| {
        d.trigger_weapon_timer(90_000_002, now - 10);
        Ok(())
    });
    assert!(ok_no_stg.is_ok());
    assert!(ctx.db.with_snapshot(|s| s.dirty_hints.crimewatch_dirty && !s.dirty_hints.system_states_dirty));

    let ok_res: Result<i32, String> = lock.try_with_mut_crimewatch(|d| {
        d.trigger_weapon_timer(90_000_002, now);
        d.system_corruption_stages.insert(30_000_142, 3);
        Ok(42)
    });
    assert_eq!(ok_res.unwrap(), 42);
    assert!(ctx.db.with_snapshot(|s| s.dirty_hints.crimewatch_dirty && s.dirty_hints.system_states_dirty));
    assert_eq!(
        lock.with_crimewatch(|d| d.states[&90_000_002].weapon_timer_until_unix),
        now + 60
    );
    // CrimeWatchLock::state_of(char_id) found vs missing char_id without full-map clone
    assert!(lock.state_of(999_888_777).is_none());
    let st_found = lock.state_of(90_000_002).expect("state_of found");
    assert_eq!(st_found.character_id, 90_000_002);
    assert_eq!(st_found.weapon_timer_until_unix, now + 60);
    assert_eq!(
        ctx.db.with_snapshot(|s| s.system_states[&30_000_142].insurgency_corruption_stage),
        3
    );

    let err_res: Result<(), String> = lock.try_with_mut_crimewatch(|_| Err("boom".to_string()));
    assert_eq!(err_res.unwrap_err(), "boom");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn step_concord_validation_despawns_kill_rights_and_tick_concord_responses() {
    let ctx = test_ctx();
    let sys_id = 30_000_142;
    let now = ctx.now_unix();
    let crim = 90_000_001;
    let victim = 90_000_002;

    // 1. Validation skip cases in step_concord:
    // (a) unknown char, (b) docked char, (c) wrong active_ship_id, (d) capsule, (e) missing ballpark ball, (f) expired criminal flag
    let ship_docked = 100_120_001;
    let ship_capsule = 100_120_002;
    let ship_no_ball = 100_120_003;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_capsule,
            InventoryItemRecord {
                item_id: ship_capsule,
                type_id: 670,
                owner_id: crim,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            ship_no_ball,
            InventoryItemRecord {
                item_id: ship_no_ball,
                type_id: 587,
                owner_id: crim,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    let mut cw = CrimeWatchDirector::new();
    // (a) unknown character
    cw.states.insert(
        888_888_001,
        CharacterCrimeWatchState {
            character_id: 888_888_001,
            criminal_flag_until_unix: now + 900,
            pending_concord_arrival: Some((sys_id, now, 111, victim)),
            ..Default::default()
        },
    );
    // (b) expired criminal flag
    cw.states.insert(
        888_888_002,
        CharacterCrimeWatchState {
            character_id: 888_888_002,
            criminal_flag_until_unix: now,
            pending_concord_arrival: Some((sys_id, now, 112, victim)),
            ..Default::default()
        },
    );
    assert!(cw.step_concord(&ctx, now, 1).await.is_empty());
    assert_eq!(cw.states[&888_888_002].criminal_flag_until_unix, 0);
    assert!(cw.states[&888_888_002].pending_concord_arrival.is_none());

    // (c) docked character
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&crim) {
            ch.station_id = Some(60_003_760);
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_docked;
        }
    });
    cw.states.insert(
        crim,
        CharacterCrimeWatchState {
            character_id: crim,
            criminal_flag_until_unix: now + 900,
            pending_concord_arrival: Some((sys_id, now, ship_docked, victim)),
            ..Default::default()
        },
    );
    assert!(cw.step_concord(&ctx, now, 1).await.is_empty());

    // (d) capsule in space -> skipped
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&crim) {
            ch.station_id = None;
            ch.active_ship_id = ship_capsule;
        }
    });
    cw.states.get_mut(&crim).unwrap().pending_concord_arrival =
        Some((sys_id, now, ship_capsule, victim));
    assert!(cw.step_concord(&ctx, now, 1).await.is_empty());

    // (e) non-capsule ship in DB but NOT in ballpark -> skipped
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&crim) {
            ch.active_ship_id = ship_no_ball;
        }
    });
    cw.states.get_mut(&crim).unwrap().pending_concord_arrival =
        Some((sys_id, now, ship_no_ball, victim));
    assert!(cw.step_concord(&ctx, now, 1).await.is_empty());

    // 2. Full CONCORD execution with target locks, new KillRight creation, sec loss, and pending_concord_despawns
    let live_ship = 100_120_010;
    spawn_ship(&ctx, sys_id, live_ship, crim, 587, DVec3::new(1_000.0, 0.0, 0.0));
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle
        .add_target_lock(
            live_ship,
            eve_destiny::TargetLock {
                target_id: 99_999,
                lock_time_ms: 1000.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: None,
            },
        )
        .await
        .unwrap();

    // Non-due despawn stays in queue
    let dummy_concord_ball = 100_120_099;
    sys_handle
        .spawn_ball(Ball {
            id: dummy_concord_ball,
            type_id: 11957,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 1_200.0,
            inertia: 0.1,
            mass: 10_000_000.0,
            radius: 120.0,
            speed_fraction: 1.0,
            mode: MovementMode::Stop,
        })
        .await
        .unwrap();
    cw.pending_concord_despawns
        .push((sys_id, dummy_concord_ball, now + 50));
    cw.states.get_mut(&crim).unwrap().pending_concord_arrival =
        Some((sys_id, now, live_ship, victim));

    let execs = cw.step_concord(&ctx, now, 17).await;
    assert_eq!(execs.len(), 1);
    let rep = &execs[0];
    assert_eq!(rep.locks_cleared, 1);
    assert!((rep.webbed_speed_fraction - 0.0).abs() < 1e-9);
    assert_eq!(rep.warp_scramble_strength, 100);
    assert!((rep.capacitor_after_neut - 0.0).abs() < 1e-9);
    assert!(rep.ecm_jammed);
    assert!(rep.death_result.is_ok());
    assert!(rep.kill_right_id > 0);
    assert_eq!(cw.pending_concord_despawns.len(), 2);

    // Due despawn removes dummy_concord_ball
    let _ = cw.step_concord(&ctx, now + 50, 17).await;
    assert!(sys_handle.get_ball_snapshot(dummy_concord_ball).await.unwrap().is_none());
    assert_eq!(cw.pending_concord_despawns.len(), 1);

    // 3. tick_concord_responses via ctx.crimewatch (with victim_char_id == 0 -> kill_right_id == 0, and already_logged_sec == true)
    let live_ship2 = 100_120_020;
    spawn_ship(&ctx, sys_id, live_ship2, crim, 587, DVec3::new(2_000.0, 0.0, 0.0));
    let arrival2 = {
        let mut lock_w = ctx.crimewatch.write().unwrap();
        let eval = lock_w.handle_criminal_remote_assistance(crim, victim, live_ship2, sys_id, 1.0, now);
        match eval {
            AggressionEvaluation::CriminalFlagTriggered { arrival_unix, .. } => arrival_unix,
            other => panic!("unexpected {other:?}"),
        }
    };
    tick_concord_responses(&ctx, arrival2).await;
    let reports = ctx.crimewatch.concord_reports();
    let last_rep = reports.last().expect("tick_concord_responses pushed report");
    assert_eq!(last_rep.victim_char_id, 0);
    assert_eq!(last_rep.kill_right_id, 0);
    assert!(last_rep.death_result.is_ok());
    let concord_ball_to_despawn = last_rep.concord_ball_id;
    assert!(sys_handle
        .get_ball_snapshot(concord_ball_to_despawn)
        .await
        .unwrap()
        .is_some());

    // Advance past CONCORD_ON_GRID_DURATION_SECS and tick again -> CONCORD ball despawns
    tick_concord_responses(&ctx, arrival2 + CONCORD_ON_GRID_DURATION_SECS).await;
    assert!(sys_handle
        .get_ball_snapshot(concord_ball_to_despawn)
        .await
        .unwrap()
        .is_none());

    // 4. Duplicate npc_ball_id & unknown killer_char_id in credit_specialized_npc_bounty_and_sec_status
    credit_specialized_npc_bounty_and_sec_status(&ctx, sys_id, 910_001, 90_000_002, 100_000.0);
    let pending_before = crate::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| {
        rt.pending_bounty_ticks[&(sys_id, 90_000_002)]
    });
    credit_specialized_npc_bounty_and_sec_status(&ctx, sys_id, 910_001, 90_000_002, 100_000.0);
    let pending_after = crate::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| {
        rt.pending_bounty_ticks[&(sys_id, 90_000_002)]
    });
    assert!((pending_after - pending_before).abs() < 1e-6);

    credit_specialized_npc_bounty_and_sec_status(&ctx, sys_id, 910_002, 777_666_555, 50_000.0);
    let unknown_pending = crate::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| {
        rt.pending_bounty_ticks.get(&(sys_id, 777_666_555)).copied()
    });
    assert_eq!(unknown_pending, Some(50_000.0));
}

#[test]
fn crimewatch_lock_ranks_and_unmutated_drop_fast_path() {
    let ctx = test_ctx();
    let now = ctx.now_unix();
    let lock = CrimeWatchLock::new(Arc::clone(&ctx.db));

    // Read-only use of a write guard leaves `mutated == false` and skips DB snapshot reads/compares on Drop.
    {
        let wg_readonly = lock.write().unwrap();
        assert!(wg_readonly.states.is_empty());
        // Acquire a RANK_LEAF lock (30 < 60 is valid), then drop `wg_readonly` first.
        // Without `!self.mutated`, Drop would acquire RANK_WORLD_INNER (40) under RANK_LEAF (60) and panic.
        let _leaf = ctx.pending_notifications.read().unwrap();
        drop(wg_readonly);
    }
    assert!(!ctx.db.with_snapshot(|s| s.dirty_hints.crimewatch_dirty));
    assert!(eve_db::lock_rank::held_lock_ranks().is_empty());

    // Mutating through DerefMut sets `mutated = true` and still persists cleanly on Drop.
    {
        let mut wg_mut = lock.write().unwrap();
        wg_mut.trigger_suspect_flag(90_000_001, now);
    }
    assert!(ctx.db.with_snapshot(|s| s.dirty_hints.crimewatch_dirty));

    // Holding RANK_CRIMEWATCH (30) permits reading RANK_LEAF (60) clock, whereas the inverse panics.
    let ts_inside = lock.with_crimewatch(|_| ctx.now_unix());
    assert!(ts_inside >= now);
    if cfg!(any(debug_assertions, feature = "lock-rank")) {
        let _leaf = ctx.pending_notifications.read().unwrap();
        let inv = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = lock.read();
        }));
        assert!(inv.is_err(), "acquiring CrimeWatchLock under RANK_LEAF must panic");
    }
    assert!(eve_db::lock_rank::held_lock_ranks().is_empty());
}
