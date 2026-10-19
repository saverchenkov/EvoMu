mod common;
use common::*;
use eve_db::{ServerClock, VirtualServerClock};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;
use std::sync::Arc;

/// Daily downtime belt respawn interval (24 hours = 86,400 seconds per EVE wiki `Asteroids_and_ore.md:69`).
const DAILY_DOWNTIME_BELT_RESPAWN_SECS: u64 = 86_400;

#[derive(Debug, Clone)]
struct SdeBeltRespawnHarness {
    belts: HashMap<i64, (u32, u64, Option<u64>)>, // belt_id -> (solar_system_id, remaining_units, depleted_at_unix)
}

impl SdeBeltRespawnHarness {
    fn from_sde_system(ctx: &ServerContext, solar_system_id: u32, units_per_belt: u64) -> Self {
        let mut belts = HashMap::new();
        for cel in ctx.sde.celestials_in_system(solar_system_id) {
            if cel.group_id == 9 {
                belts.insert(cel.celestial_id, (solar_system_id, units_per_belt, None));
            }
        }
        Self { belts }
    }

    fn harvest(&mut self, belt_id: i64, amount: u64, now_unix: u64) -> Result<u64, &'static str> {
        let Some((_, rem, depleted_at)) = self.belts.get_mut(&belt_id) else {
            return Err("UnknownAsteroidBelt");
        };
        if *rem == 0 {
            return Err("AsteroidBeltDepleted");
        }
        let harvested = amount.min(*rem);
        *rem -= harvested;
        if *rem == 0 {
            *depleted_at = Some(now_unix);
        }
        Ok(*rem)
    }

    fn maintenance_tick(&mut self, now_unix: u64, full_units: u64) {
        for (_, rem, depleted_at) in self.belts.values_mut() {
            if let Some(dep_ts) = *depleted_at {
                if now_unix.saturating_sub(dep_ts) >= DAILY_DOWNTIME_BELT_RESPAWN_SECS {
                    *rem = full_units;
                    *depleted_at = None;
                }
            }
        }
    }
}

#[tokio::test]
async fn test_t7_injectable_server_clock_time_acceleration_and_pause_on_tick() {
    // Verify wall-anchored VirtualServerClock pause/unpause monotonicity and tick_wall_second
    let wall_clock = VirtualServerClock::new();
    let t0 = wall_clock.now_unix_secs();
    wall_clock.set_paused(true);
    assert!(wall_clock.is_paused());
    let t_paused = wall_clock.now_unix_secs();
    assert_eq!(wall_clock.tick_wall_second(), 0);
    assert_eq!(wall_clock.now_unix_secs(), t_paused);
    wall_clock.set_paused(false);
    assert!(!wall_clock.is_paused());
    let t_resumed = wall_clock.now_unix_secs();
    assert!(t_resumed >= t0 && t_resumed <= t_paused + 1);
    wall_clock.set_acceleration_multiplier(60);
    assert_eq!(wall_clock.tick_wall_second(), 60);
    assert!(wall_clock.now_unix_secs() >= t_resumed + 59);

    let ctx = ServerContext::new();
    let fixed_clock = Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000));
    ctx.set_clock(fixed_clock.clone());

    assert_eq!(ctx.now_unix_secs(), 1_800_000_000);
    assert_eq!(ctx.db.now_unix_secs(), 1_800_000_000);

    // Verify pause-on-tick and time-acceleration multiplier on the injected ServerClock
    ctx.set_time_acceleration_multiplier(60);
    assert_eq!(ctx.time_acceleration_multiplier(), 60);
    ctx.set_pause_on_tick(true);
    assert!(ctx.pause_on_tick());
    assert!(!ctx.is_clock_paused());

    // One 10s base tick at 60x acceleration advances 600 virtual seconds and pauses automatically
    let step1 = ctx.step_clock_tick(10);
    assert_eq!(step1, 600);
    assert_eq!(ctx.now_unix_secs(), 1_800_000_600);
    assert!(ctx.is_clock_paused());

    // While paused, further ticks advance 0 seconds
    let step_while_paused = ctx.step_clock_tick(10);
    assert_eq!(step_while_paused, 0);
    assert_eq!(ctx.now_unix_secs(), 1_800_000_600);

    // Unpause and disable pause-on-tick for continuous accelerated simulation
    ctx.set_pause_on_tick(false);
    ctx.set_clock_paused(false);

    let mut session = SessionState::default();
    let empty = HashMap::new();
    login_pilot(&ctx, &mut session, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut session,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &empty,
        None,
    )
    .await
    .expect("SelectCharacterID");

    // -------------------------------------------------------------------------
    // 1. Skill training under accelerated virtual clock time
    // -------------------------------------------------------------------------
    let sp_before = ctx
        .db
        .with_snapshot(|snap| snap.characters[&90_000_001].skill_points);
    let queue_res = ctx
        .dispatch_rpc(
            &mut session,
            "skillMgr2",
            None,
            "SaveSkillQueue",
            &[PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(3392), // Mechanics I (rank 1, no prerequisites)
                PyRep::Integer(1),
            ])])],
            &empty,
            None,
        )
        .await
        .expect("SaveSkillQueue Mechanics I should succeed");
    let PyRep::KeyVal(qkv) = queue_res else {
        panic!("Expected KeyVal from SaveSkillQueue");
    };
    let skill_duration = qkv
        .get("durationSeconds")
        .and_then(|v| v.as_f64())
        .expect("durationSeconds");
    let skill_end_sp = qkv.get("endSP").and_then(|v| v.as_f64()).expect("endSP");
    assert!(skill_duration > 100.0);

    // Advance part of the training time via 60x accelerated clock ticks (no wall-clock sleep)
    ctx.set_time_acceleration_multiplier(60);
    let partial_advanced = ctx.step_clock_tick(2); // 120 virtual seconds
    assert_eq!(partial_advanced, 120);
    let queue_mid = ctx.db.with_snapshot(|snap| {
        snap.skill_queues
            .get(&90_000_001)
            .cloned()
            .unwrap_or_default()
    });
    assert_eq!(
        queue_mid.len(),
        1,
        "Skill should still be training mid-way through duration"
    );
    assert!(
        queue_mid[0].start_sp > 0.0 && queue_mid[0].start_sp < skill_end_sp,
        "Partial virtual clock advance must progress skill SP proportionally"
    );

    // Advance past remaining skill duration via accelerated tick
    let rem_ticks = ((skill_duration as u64) / 60) + 5;
    let _ = ctx.step_clock_tick(rem_ticks);
    let (queue_after, trained_skill, sp_after) = ctx.db.with_snapshot(|snap| {
        let q = snap
            .skill_queues
            .get(&90_000_001)
            .cloned()
            .unwrap_or_default();
        let sk = snap
            .skills
            .get(&90_000_001)
            .and_then(|m| m.get(&3392))
            .cloned();
        let sp = snap.characters[&90_000_001].skill_points;
        (q, sk, sp)
    });
    assert!(
        queue_after.is_empty(),
        "Skill queue must empty once virtual clock passes skill completion"
    );
    let trained_skill = trained_skill.expect("Mechanics must be recorded in trained skills");
    assert_eq!(trained_skill.level, 1);
    assert!((trained_skill.skill_points - skill_end_sp).abs() < 1e-6);
    assert!((sp_after - (sp_before + skill_end_sp)).abs() < 1e-6);

    // -------------------------------------------------------------------------
    // 2. Industry job installation & completion under accelerated virtual time
    // -------------------------------------------------------------------------
    let job_res = ctx
        .dispatch_rpc(
            &mut session,
            "industryManager",
            None,
            "InstallJob",
            &[PyRep::Integer(691), PyRep::Integer(1), PyRep::Integer(1)],
            &empty,
            None,
        )
        .await
        .expect("InstallJob for Rifter BPO should succeed");
    let PyRep::KeyVal(jkv) = job_res else {
        panic!("Expected KeyVal from InstallJob");
    };
    let job_id = jkv.get("jobID").and_then(|v| v.as_i64()).expect("jobID");
    let job_dur = jkv
        .get("durationSeconds")
        .and_then(|v| v.as_i64())
        .expect("durationSeconds") as u64;
    assert!(job_dur > 0);

    // CompleteJob before clock advance must reject with JobNotYetComplete
    let early_err = ctx
        .dispatch_rpc(
            &mut session,
            "industryManager",
            None,
            "CompleteJob",
            &[PyRep::Integer(job_id)],
            &empty,
            None,
        )
        .await
        .expect_err("CompleteJob before endTime must fail");
    assert_eq!(early_err.code, "JobNotYetComplete");

    // Advance virtual clock past job_dur using 600x time acceleration
    ctx.set_time_acceleration_multiplier(600);
    let ticks_needed = (job_dur / 600) + 2;
    let _ = ctx.step_clock_tick(ticks_needed);

    let complete_res = ctx
        .dispatch_rpc(
            &mut session,
            "industryManager",
            None,
            "CompleteJob",
            &[PyRep::Integer(job_id)],
            &empty,
            None,
        )
        .await
        .expect("CompleteJob after virtual clock advance must succeed");
    let PyRep::KeyVal(ckv) = complete_res else {
        panic!("Expected KeyVal from CompleteJob");
    };
    assert_eq!(ckv.get("productTypeID"), Some(&PyRep::Integer(587)));
    let job_status = ctx
        .db
        .with_snapshot(|snap| snap.industry_jobs[&job_id].status.clone());
    assert_eq!(job_status, "Delivered");

    // -------------------------------------------------------------------------
    // 3. Planetary Interaction SDE raw P0 extraction cooldown under virtual time
    // -------------------------------------------------------------------------
    let jita_planet_id = ctx
        .sde
        .celestials_in_system(30_000_142)
        .into_iter()
        .find(|c| c.group_id == 7)
        .map(|c| c.celestial_id)
        .expect("Jita must have an SDE planet");

    // Client-supplied extra arguments / schematic overrides must be rejected
    let create_override_err = ctx
        .dispatch_rpc(
            &mut session,
            "planetMgr",
            None,
            "CreateColony",
            &[PyRep::Integer(jita_planet_id), PyRep::Integer(65)],
            &empty,
            None,
        )
        .await
        .expect_err("CreateColony with client override must fail");
    assert_eq!(create_override_err.code, "ClientSchematicOverrideForbidden");

    ctx.dispatch_rpc(
        &mut session,
        "planetMgr",
        None,
        "CreateColony",
        &[PyRep::Integer(jita_planet_id)],
        &empty,
        None,
    )
    .await
    .expect("CreateColony on Jita planet should succeed");

    let flush_override_err = ctx
        .dispatch_rpc(
            &mut session,
            "planetMgr",
            None,
            "Flush",
            &[PyRep::Integer(jita_planet_id), PyRep::Integer(65)],
            &empty,
            None,
        )
        .await
        .expect_err("Flush with client schematic override must fail");
    assert_eq!(flush_override_err.code, "ClientSchematicOverrideForbidden");

    let flush1 = ctx
        .dispatch_rpc(
            &mut session,
            "planetMgr",
            None,
            "Flush",
            &[PyRep::Integer(jita_planet_id)],
            &empty,
            None,
        )
        .await
        .expect("Initial planetMgr.Flush should succeed");
    let PyRep::KeyVal(fkv1) = flush1 else {
        panic!("Expected KeyVal from Flush");
    };
    let cycle_time = fkv1
        .get("cycleTimeSeconds")
        .and_then(|v| v.as_i64())
        .expect("cycleTimeSeconds from SDE pinCycleTime (1643)") as u64;
    let produced_type_id = fkv1
        .get("producedTypeID")
        .and_then(|v| v.as_i64())
        .expect("producedTypeID") as u32;
    let first_qty = fkv1
        .get("producedQuantity")
        .and_then(|v| v.as_i64())
        .expect("producedQuantity");
    assert_eq!(cycle_time, 300); // SDE pinCycleTime (attribute 1643) = 300s
    assert_eq!(produced_type_id, 2_268); // Aqueous Liquids (harvesterType 709 on Barren extractor 2409)
    assert_eq!(first_qty, 1_000); // SDE pinExtractionQuantity (attribute 1642) = 1000 units
    assert_eq!(
        fkv1.get("totalStoredQuantity"),
        Some(&PyRep::Integer(first_qty))
    );

    // Immediate second Flush before SDE pinCycleTime (300s) must reject
    let flush_early = ctx
        .dispatch_rpc(
            &mut session,
            "planetMgr",
            None,
            "Flush",
            &[PyRep::Integer(jita_planet_id)],
            &empty,
            None,
        )
        .await
        .expect_err("Flush before SDE pinCycleTime must fail");
    assert_eq!(flush_early.code, "ExtractorCycleNotReady");

    // Advance 600s via 1 accelerated clock tick (600x * 1s = 600s >= 300s pinCycleTime)
    let _ = ctx.step_clock_tick(1);
    let flush2 = ctx
        .dispatch_rpc(
            &mut session,
            "planetMgr",
            None,
            "Flush",
            &[PyRep::Integer(jita_planet_id)],
            &empty,
            None,
        )
        .await
        .expect("Second planetMgr.Flush after SDE pinCycleTime virtual clock advance must succeed");
    let PyRep::KeyVal(fkv2) = flush2 else {
        panic!("Expected KeyVal from second Flush");
    };
    assert_eq!(
        fkv2.get("totalStoredQuantity"),
        Some(&PyRep::Integer(first_qty * 2))
    );

    // Advance another tick and select a second valid SDE resource on the Barren planet (Noble Metals 2270, harvesterType 709 on 2435)
    let _ = ctx.step_clock_tick(1);
    let flush_noble = ctx
        .dispatch_rpc(
            &mut session,
            "planetMgr",
            None,
            "Flush",
            &[PyRep::Integer(jita_planet_id), PyRep::Integer(2_270)],
            &empty,
            None,
        )
        .await
        .expect("Flush selecting Noble Metals (2270) on Barren planet must succeed");
    let PyRep::KeyVal(fkv_noble) = flush_noble else {
        panic!("Expected KeyVal from Noble Metals Flush");
    };
    assert_eq!(fkv_noble.get("producedTypeID"), Some(&PyRep::Integer(2_270)));
    assert_eq!(fkv_noble.get("producedQuantity"), Some(&PyRep::Integer(1_000)));

    // Selecting a P0 resource not present on a Barren planet (Noble Gas 2310) must be rejected
    let _ = ctx.step_clock_tick(1);
    let wrong_biome_err = ctx
        .dispatch_rpc(
            &mut session,
            "planetMgr",
            None,
            "Flush",
            &[PyRep::Integer(jita_planet_id), PyRep::Integer(2_310)],
            &empty,
            None,
        )
        .await
        .expect_err("Selecting a P0 resource not on Barren planet must fail");
    assert_eq!(wrong_biome_err.code, "ClientSchematicOverrideForbidden");

    // -------------------------------------------------------------------------
    // 4. SDE Asteroid Belt daily downtime respawn (24h = 86,400s) under virtual time
    // -------------------------------------------------------------------------
    // Verify miningScanMgr.perform_scan returns honest UnimplementedRPC until Phase 5M
    let scan_unimpl = ctx
        .dispatch_rpc(
            &mut session,
            "miningScanMgr",
            None,
            "perform_scan",
            &[],
            &empty,
            None,
        )
        .await
        .expect_err("miningScanMgr.perform_scan must return UnimplementedRPC until Phase 5M");
    assert_eq!(scan_unimpl.code, "UnimplementedRPC");

    // Per EVE wiki Asteroids_and_ore.md:67, Jita (30000142) has 0 asteroid belts in SDE,
    // while neighboring Perimeter (30000144) has 3 real SDE asteroid belts (group_id == 9).
    let jita_belts = SdeBeltRespawnHarness::from_sde_system(&ctx, 30_000_142, 25_000);
    assert!(
        jita_belts.belts.is_empty(),
        "Jita (30000142) has 0 SDE asteroid belts"
    );

    let mut perimeter_belts = SdeBeltRespawnHarness::from_sde_system(&ctx, 30_000_144, 25_000);
    assert_eq!(
        perimeter_belts.belts.len(),
        3,
        "Perimeter (30000144) must have 3 SDE asteroid belts (group_id == 9)"
    );
    let belt_id = *perimeter_belts.belts.keys().min().expect("Perimeter belt");
    assert_eq!(belt_id, 40_009_232);

    // Deplete Perimeter's first SDE belt at current virtual clock timestamp
    let rem_after_mine = perimeter_belts
        .harvest(belt_id, 25_000, ctx.now_unix_secs())
        .expect("harvest full belt");
    assert_eq!(rem_after_mine, 0);
    assert_eq!(
        perimeter_belts.harvest(belt_id, 1_000, ctx.now_unix_secs()),
        Err("AsteroidBeltDepleted")
    );

    // Advance virtual clock by 12 hours (43,200s < 86,400s daily downtime respawn): belt stays depleted
    let _ = ctx.step_clock_tick(43_200 / 600);
    perimeter_belts.maintenance_tick(ctx.now_unix_secs(), 25_000);
    assert_eq!(perimeter_belts.belts[&belt_id].1, 0);

    // Advance remaining 12 hours (total 86,400s = 24h daily downtime window): belt respawns
    let _ = ctx.step_clock_tick(43_200 / 600);
    perimeter_belts.maintenance_tick(ctx.now_unix_secs(), 25_000);
    assert_eq!(perimeter_belts.belts[&belt_id].1, 25_000);
    assert!(perimeter_belts.belts[&belt_id].2.is_none());
}
