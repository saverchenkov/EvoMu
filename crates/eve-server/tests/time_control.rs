mod common;
use common::*;
use eve_db::{ServerClock, VirtualServerClock};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::system_actor::{
    next_downtime_unix_secs, DEFAULT_BELT_RESPAWN_INTERVAL_SECS, DEFAULT_DOWNTIME_UTC_SECS_OF_DAY,
};
use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

#[tokio::test]
async fn test_t7_injectable_server_clock_time_acceleration_and_pause_on_tick() {
    // parity: plan:T-7
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
            "UserUpdateNetwork",
            &[PyRep::Integer(jita_planet_id), PyRep::Integer(65)],
            &empty,
            None,
        )
        .await
        .expect_err("UserUpdateNetwork with client override must fail");
    assert_eq!(create_override_err.code, "ClientSchematicOverrideForbidden");

    let ship_id = session.ship_id.unwrap_or(100_000_001);
    let prev_st = session.station_id;
    session.station_id = None;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.station_id = None;
        }
        snap.next_entity_id += 1;
        let id = snap.next_entity_id;
        snap.items.insert(
            id,
            eve_db::InventoryItemRecord {
                item_id: id,
                type_id: 2254,
                owner_id: 90_000_001,
                location_id: ship_id,
                flag: eve_db::InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });
    ctx.dispatch_rpc(
        &mut session,
        "planetMgr",
        None,
        "UserUpdateNetwork",
        &[
            PyRep::Integer(jita_planet_id),
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(1),
                PyRep::Integer(1),
                PyRep::Integer(2254),
                PyRep::Real(0.1),
                PyRep::Real(0.2),
            ])]),
        ],
        &empty,
        None,
    )
    .await
    .expect("UserUpdateNetwork on Jita planet should succeed");
    session.station_id = prev_st;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.station_id = prev_st;
        }
    });

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
    let poco_tax = fkv1
        .get("pocoExportTaxISK")
        .and_then(|v| v.as_f64())
        .expect("pocoExportTaxISK");
    assert_eq!(cycle_time, 300); // SDE pinCycleTime (attribute 1643) = 300s
    assert_eq!(produced_type_id, 2_268); // Aqueous Liquids (harvesterType 709 on Barren extractor 2409)
    assert_eq!(first_qty, 1_000); // SDE pinExtractionQuantity (attribute 1642) = 1000 units
    assert!((poco_tax - 750.0).abs() < 1e-6); // SDE exportTaxMultiplier (attribute 1641) = 5.0 ISK * 1000 units * 0.15 = 750.0 ISK
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
    let jita_sys = ctx.systems.get_or_spawn(30_000_142);
    let jita_asteroids = jita_sys
        .populate_asteroid_belt_entities()
        .await
        .expect("populate Jita belts");
    assert!(
        jita_asteroids.is_empty(),
        "Jita (30000142) has 0 SDE asteroid belts"
    );

    let perimeter_sys = ctx.systems.get_or_spawn(30_000_144);
    let perimeter_asteroids = perimeter_sys
        .populate_asteroid_belt_entities()
        .await
        .expect("populate Perimeter belts");
    let perimeter_belt_ids: BTreeSet<i64> =
        perimeter_asteroids.iter().map(|a| a.belt_id).collect();
    assert_eq!(
        perimeter_belt_ids.len(),
        3,
        "Perimeter (30000144) must have 3 SDE asteroid belts (group_id == 9)"
    );
    let first_belt_id = *perimeter_belt_ids.iter().next().expect("Perimeter belt");
    assert_eq!(first_belt_id, 40_009_232);

    let belt_asteroids: Vec<_> = perimeter_asteroids
        .iter()
        .filter(|a| a.belt_id == first_belt_id)
        .cloned()
        .collect();
    assert!(
        belt_asteroids.len() >= 2,
        "Perimeter belt must contain multiple asteroids"
    );
    let target_ast = &belt_asteroids[0];
    let ast_id = target_ast.asteroid_id;
    let full_units = target_ast.initial_units;
    assert!(full_units > 0);

    // 4a. Partly mined belt: depleting only the first asteroid while others remain in the belt
    // must NOT schedule belt respawn (`Asteroids_and_ore.md:69`: respawn after downtime only if no asteroids left).
    let mined = perimeter_sys
        .mine_asteroid(ast_id, full_units, 0.0)
        .await
        .expect("mine first asteroid");
    assert!(mined.depleted);
    assert_eq!(mined.remaining_units, 0);
    assert!(
        perimeter_sys
            .mine_asteroid(ast_id, 1, 0.0)
            .await
            .is_err(),
        "mining a depleted asteroid must fail"
    );
    assert!(
        perimeter_sys
            .get_ball_snapshot(ast_id)
            .await
            .expect("get ball snapshot")
            .is_none(),
        "depleted asteroid must be removed from Ballpark"
    );
    let state_partly_mined = perimeter_sys
        .get_asteroid_state(ast_id)
        .await
        .expect("get asteroid state partly mined")
        .expect("asteroid state exists");
    assert!(state_partly_mined.depleted);
    assert_eq!(
        state_partly_mined.respawn_at_tick, None,
        "partly mined belt must not schedule respawn while other asteroids remain"
    );

    // Even after advancing a full 24h downtime interval, a partly mined belt must NOT respawn
    let _ = ctx.step_clock_tick(DEFAULT_BELT_RESPAWN_INTERVAL_SECS / 600);
    let _ = perimeter_sys
        .step_manual(1.0)
        .await
        .expect("step system after 24h partly mined");
    let state_still_partial = perimeter_sys
        .get_asteroid_state(ast_id)
        .await
        .expect("get asteroid state after 24h partial")
        .expect("asteroid state after 24h partial");
    assert!(state_still_partial.depleted);
    assert_eq!(state_still_partial.remaining_units, 0);

    // 4b. Align virtual clock to 10:50 UTC (600s BEFORE daily 11:00 UTC downtime) and empty the belt:
    // it must schedule respawn at that upcoming 11:00 UTC downtime (600s later), not 24h later.
    let upcoming_dt = next_downtime_unix_secs(
        ctx.now_unix_secs(),
        DEFAULT_BELT_RESPAWN_INTERVAL_SECS,
        DEFAULT_DOWNTIME_UTC_SECS_OF_DAY,
    );
    let target_just_before_dt = if upcoming_dt.saturating_sub(ctx.now_unix_secs()) >= 600 {
        upcoming_dt - 600
    } else {
        upcoming_dt + DEFAULT_BELT_RESPAWN_INTERVAL_SECS - 600
    };
    ctx.advance_secs(target_just_before_dt - ctx.now_unix_secs());
    let expected_first_downtime = target_just_before_dt + 600;
    assert_eq!(
        expected_first_downtime % DEFAULT_BELT_RESPAWN_INTERVAL_SECS,
        DEFAULT_DOWNTIME_UTC_SECS_OF_DAY
    );

    for rem_ast in &belt_asteroids[1..] {
        let m = perimeter_sys
            .mine_asteroid(rem_ast.asteroid_id, rem_ast.initial_units, 0.0)
            .await
            .expect("deplete remaining asteroid in belt before downtime");
        assert!(m.depleted);
    }

    let state_before_dt = perimeter_sys
        .get_asteroid_state(ast_id)
        .await
        .expect("get asteroid state after belt empty before downtime")
        .expect("asteroid state exists");
    assert!(state_before_dt.depleted);
    assert_eq!(
        state_before_dt.respawn_at_tick,
        Some(expected_first_downtime),
        "belt emptied 600s before 11:00 UTC downtime must schedule respawn at that 11:00 UTC downtime"
    );

    // Advance 300s to 10:55 UTC (still 300s before 11:00 UTC downtime): belt stays depleted
    ctx.advance_secs(300);
    let _ = perimeter_sys
        .step_manual(1.0)
        .await
        .expect("step system at 10:55 UTC");
    assert!(
        perimeter_sys
            .get_asteroid_state(ast_id)
            .await
            .expect("get asteroid state at 10:55 UTC")
            .expect("asteroid state")
            .depleted
    );

    // Advance remaining 300s to reach 11:00 UTC downtime: entire belt respawns together
    ctx.advance_secs(300);
    assert_eq!(ctx.now_unix_secs(), expected_first_downtime);
    let _ = perimeter_sys
        .step_manual(1.0)
        .await
        .expect("step system at 11:00 UTC downtime");
    for belt_ast in &belt_asteroids {
        let state_at_dt = perimeter_sys
            .get_asteroid_state(belt_ast.asteroid_id)
            .await
            .expect("get asteroid state at downtime")
            .expect("asteroid state at downtime");
        assert!(!state_at_dt.depleted);
        assert_eq!(state_at_dt.remaining_units, belt_ast.initial_units);
        assert_eq!(state_at_dt.respawn_at_tick, None);
        assert!(
            perimeter_sys
                .get_ball_snapshot(belt_ast.asteroid_id)
                .await
                .expect("get ball snapshot at downtime")
                .is_some(),
            "respawned asteroid ball must be restored to Ballpark at downtime"
        );
    }

    // 4c. Advance 600s past downtime to 11:10 UTC and empty the belt again:
    // it must wait for the FOLLOWING day's 11:00 UTC downtime (85,800s later).
    let _ = ctx.step_clock_tick(1); // +600s -> 11:10 UTC
    let expected_following_downtime =
        expected_first_downtime + DEFAULT_BELT_RESPAWN_INTERVAL_SECS;
    for belt_ast in &belt_asteroids {
        let m = perimeter_sys
            .mine_asteroid(belt_ast.asteroid_id, belt_ast.initial_units, 0.0)
            .await
            .expect("deplete asteroid after downtime");
        assert!(m.depleted);
    }
    let state_after_dt_empty = perimeter_sys
        .get_asteroid_state(ast_id)
        .await
        .expect("get asteroid state emptied after downtime")
        .expect("asteroid state");
    assert!(state_after_dt_empty.depleted);
    assert_eq!(
        state_after_dt_empty.respawn_at_tick,
        Some(expected_following_downtime),
        "belt emptied at 11:10 UTC must wait for the following day's 11:00 UTC downtime"
    );

    // Advance virtual clock by 12 hours (43,200s -> 23:10 UTC): belt stays depleted
    let _ = ctx.step_clock_tick(43_200 / 600);
    let _ = perimeter_sys
        .step_manual(1.0)
        .await
        .expect("step system after 12h");
    let state_12h = perimeter_sys
        .get_asteroid_state(ast_id)
        .await
        .expect("get asteroid state 12h")
        .expect("asteroid state 12h");
    assert!(state_12h.depleted);
    assert_eq!(state_12h.remaining_units, 0);
    assert!(
        perimeter_sys
            .get_ball_snapshot(ast_id)
            .await
            .expect("get ball snapshot 12h")
            .is_none()
    );

    // Advance remaining 42,600s (71 ticks of 600s) to reach the following day's 11:00 UTC downtime: belt respawns
    let _ = ctx.step_clock_tick(42_600 / 600);
    assert_eq!(ctx.now_unix_secs(), expected_following_downtime);
    let _ = perimeter_sys
        .step_manual(1.0)
        .await
        .expect("step system at following downtime");
    for belt_ast in &belt_asteroids {
        let state_next_dt = perimeter_sys
            .get_asteroid_state(belt_ast.asteroid_id)
            .await
            .expect("get asteroid state at following downtime")
            .expect("asteroid state at following downtime");
        assert!(!state_next_dt.depleted);
        assert_eq!(state_next_dt.remaining_units, belt_ast.initial_units);
        assert_eq!(state_next_dt.respawn_at_tick, None);
        assert!(
            perimeter_sys
                .get_ball_snapshot(belt_ast.asteroid_id)
                .await
                .expect("get ball snapshot at following downtime")
                .is_some(),
            "respawned asteroid ball must be restored to Ballpark"
        );
    }
}
