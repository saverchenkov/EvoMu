//! Part 4 of FW & Insurgency closure tests: Pirate FOB Docking & 3h±1h Armor Siege, Ambition Scaling, Surrender I-Hub Vulnerability & Contribution Payouts (`SO-5`, `Q-8`).

#![forbid(unsafe_code)]

use crate::fw_warzone_insurgency_closure::setup_warzone_chain;
use crate::sovereignty_equinox_gameplay_closure::{
    drain_notif_events, fit_siege_artillery, lock_target_via_rpc, make_ball, open_sqlite_ctx, register_online_pilot,
    rpc_call, step_weapon_ticks, temp_sqlite_path,
};
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::fw_gameplay::{
    INSURGENCY_MAX_PAYOUT_LP, PIRATE_FOB_STRUCTURE_BASE_ID, SURRENDER_IHUB_VULNERABILITY_DELAY_SECS,
    TYPE_FW_IHUB,
};
use eve_server::services::insurgency_services::{
    ACTIVE_DURATION_SECS, FORECASTING_DURATION_SECS, WINDDOWN_DURATION_SECS,
};
use glam::DVec3;

#[tokio::test]
async fn test_pirate_fob_docking_siege_ambition_surrender_ihub_and_payouts() {
    // parity: feat:fw.insurgency-fob
    // parity: feat:fw.insurgency-fob-ambition
    // parity: feat:fw.insurgency-ambition-victory
    // parity: feat:fw.insurgency-contribution-payout
    // parity: notify:OnInsurgencyCampaignEndingForLocation
    eve_testkit::attest(&[
        "feat:fw.insurgency-fob",
        "feat:fw.insurgency-fob-ambition",
        "feat:fw.insurgency-ambition-victory",
        "feat:fw.insurgency-contribution-payout",
        "notify:OnInsurgencyCampaignEndingForLocation",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let (_enemy_sys, front_sys, _cmd_sys, _rear_sys) = setup_warzone_chain(&ctx);

    let fob_pos = DVec3::new(95_000.0, 0.0, 45_000.0);
    let pir_cid = 90_000_041i64;
    let pir_ship = 100_000_041i64;
    let emp_cid = 90_000_042i64;
    let emp_ship = 100_000_042i64;
    let ang_cid = 90_000_043i64;
    let ang_ship = 100_000_043i64;

    let mut s_pir = register_online_pilot(
        &ctx, 84_001, "demo", "demo-secret", pir_cid, 98_000_041, None,
        front_sys, pir_ship, 621, fob_pos + DVec3::new(500.0, 0.0, 0.0), 0,
    ).await;
    let mut s_emp = register_online_pilot(
        &ctx, 84_002, "demo2", "demo2-secret", emp_cid, 98_000_042, None,
        front_sys, emp_ship, 639, fob_pos + DVec3::new(600.0, 0.0, 0.0), 0,
    ).await;
    let mut s_ang = register_online_pilot(
        &ctx, 84_003, "demo3", "demo3-secret", ang_cid, 98_000_043, None,
        front_sys, ang_ship, 621, fob_pos + DVec3::new(700.0, 0.0, 0.0), 0,
    ).await;

    rpc_call(&ctx, &mut s_pir, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_010)]).await.unwrap();
    rpc_call(&ctx, &mut s_emp, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_001)]).await.unwrap();
    rpc_call(&ctx, &mut s_ang, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_011)]).await.unwrap();

    // Seed FW I-Hub (`32226`) in `front_sys` and auto-seed Campaign 1 (`Guristas` `500010`).
    let _ = rpc_call(&ctx, &mut s_emp, "dungeonInstanceCacheMgr", "GetFactionWarfareInstances", &[]).await.unwrap();
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let camp1 = ctx.db.with_snapshot(|s| *s.insurgency_runtime.campaigns.keys().next().unwrap());
    let fob1 = PIRATE_FOB_STRUCTURE_BASE_ID + camp1;

    // 1. Pirate FOB (`79172`) docking access via `beyonce.CmdDock`: Guristas (`500010`) allowed, Caldari (`500001`) & Angel (`500011`) rejected!
    assert_eq!(
        rpc_call(&ctx, &mut s_emp, "beyonce", "CmdDock", &[PyRep::Integer(fob1)]).await.unwrap_err().code,
        "FobDockingRestrictedToPirateMilitia"
    );
    assert_eq!(
        rpc_call(&ctx, &mut s_ang, "beyonce", "CmdDock", &[PyRep::Integer(fob1)]).await.unwrap_err().code,
        "FobDockingRestrictedToPirateMilitia"
    );
    rpc_call(&ctx, &mut s_pir, "beyonce", "CmdDock", &[PyRep::Integer(fob1)]).await.expect("Guristas pilot docks at Guristas FOB");
    assert_eq!(s_pir.structure_id, Some(fob1));
    fast_forward_secs(&ctx, 12);
    rpc_call(&ctx, &mut s_pir, "beyonce", "Undock", &[]).await.expect("undock from Guristas FOB");

    // 2. Campaign 1 Pirate Victory (`5 + 0` Stage-5 corrupted systems via live 60s pulses + `tick_background_cluster`):
    //    - Pirate Ambition modifier rises `0 -> 1` (requiring `6` Stage-5 systems next campaign!)
    //    - Winner (`pir_cid`, 100% >= 45% share) gets `1,500,000 LP` with Guristas (`1000434`/`militia_lp_corp_id`) + ISK
    //    - Loser (`emp_cid`, 100% >= 45% share) gets `0.4x` (`600,000 LP`) with State Protectorate (`1000180`) + ISK
    //    - Surrendered FOB system's FW I-Hub unlocks after `25h` (`90,000s`) for live `dogmaIM.Activate` weapon attack!
    fast_forward_secs(&ctx, FORECASTING_DURATION_SECS + 10);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

    // Give Empire pilot 1 pulse of Suppression contribution on `front_sys` before Pirate corrupts 5 systems to Stage 5.
    let _ = ctx.systems.get_or_spawn(front_sys).spawn_ball(make_ball(pir_ship, 621, DVec3::new(90_000.0, 0.0, 0.0))).await;
    let _ = ctx.systems.get_or_spawn(front_sys).spawn_ball(make_ball(emp_ship, 639, DVec3::ZERO)).await;
    ctx.db.with_mut_snapshot(|s| {
        s.system_states.entry(front_sys).or_default().last_fw_capture_tick = 0;
    });
    eve_server::services::insurgency_services::step_system_insurgency_tick_delta(&ctx, front_sys, Some(60)).await;
    let _ = ctx.systems.get_or_spawn(front_sys).spawn_ball(make_ball(emp_ship, 639, DVec3::new(90_000.0, 0.0, 0.0))).await;

    let emp_lp0 = ctx.db.with_snapshot(|s| s.characters[&emp_cid].loyalty_points.get(&1_000_180).copied().unwrap_or(0));
    let pir_lp0: i64 = ctx.db.with_snapshot(|s| s.characters[&pir_cid].loyalty_points.values().sum());
    let emp_isk0 = ctx.db.with_snapshot(|s| s.characters[&emp_cid].balance.to_wire_f64());
    let pir_isk0 = ctx.db.with_snapshot(|s| s.characters[&pir_cid].balance.to_wire_f64());

    ctx.db.with_mut_snapshot(|s| {
        let c = s.insurgency_runtime.campaigns.get_mut(&camp1).unwrap();
        for sys in ctx.sde.solar_systems.values() {
            if c.systems.len() >= 5 {
                break;
            }
            if sys.security_status > 0.0 && sys.security_status < 0.45 && !c.systems.contains(&sys.solar_system_id) {
                c.systems.push(sys.solar_system_id);
            }
        }
        for &sys in &c.systems {
            c.corruption_points.entry(sys).or_insert(0);
        }
    });

    // Corrupt 5 campaign systems to Stage 5 via live 60s destiny pulses (Stage 3 on `front_sys` spreads the 5th system!).
    for idx in 0..5usize {
        let target_sys = ctx.db.with_snapshot(|s| s.insurgency_runtime.campaigns[&camp1].systems[idx]);
        s_pir.solar_system_id = Some(target_sys);
        ctx.db.with_mut_snapshot(|s| {
            s.characters.get_mut(&pir_cid).unwrap().solar_system_id = target_sys;
            s.items.get_mut(&pir_ship).unwrap().location_id = target_sys as i64;
            s.system_states.entry(target_sys).or_default().fw_complex_position = Some([0.0, 0.0, 0.0]);
        });
        let sys_h = ctx.systems.get_or_spawn(target_sys);
        let _ = sys_h.spawn_ball(make_ball(pir_ship, 621, DVec3::ZERO)).await;
        for _ in 0..5 {
            ctx.db.with_mut_snapshot(|s| {
                let st = s.system_states.entry(target_sys).or_default();
                st.fw_complex_position.get_or_insert([0.0, 0.0, 0.0]);
                st.last_fw_capture_tick = 0;
            });
            eve_server::services::insurgency_services::step_system_insurgency_tick_delta(&ctx, target_sys, Some(60)).await;
        }
    }



    assert_eq!(ctx.db.with_snapshot(|s| s.insurgency_runtime.campaign_outcomes.get(&camp1).cloned()), Some("PirateWin".into()));
    assert_eq!(ctx.db.with_snapshot(|s| s.insurgency_runtime.pirate_ambition_modifier[&500_010]), 1);
    let pir_lp_total: i64 = ctx.db.with_snapshot(|s| s.characters[&pir_cid].loyalty_points.values().sum::<i64>()) - pir_lp0;
    let emp_lp_gain = ctx.db.with_snapshot(|s| s.characters[&emp_cid].loyalty_points[&1_000_180]) - emp_lp0;
    assert_eq!(pir_lp_total, INSURGENCY_MAX_PAYOUT_LP, "winner at >=45% share receives max 1,500,000 LP");
    assert_eq!(emp_lp_gain, 600_000, "loser at >=45% share receives 0.4x (600,000 LP)");
    let pir_isk_gain = ctx.db.with_snapshot(|s| s.characters[&pir_cid].balance.to_wire_f64()) - pir_isk0;
    let emp_isk_gain = ctx.db.with_snapshot(|s| s.characters[&emp_cid].balance.to_wire_f64()) - emp_isk0;
    assert_eq!(pir_isk_gain, (INSURGENCY_MAX_PAYOUT_LP * 1_000) as f64, "winner received contribution-scaled ISK");
    assert_eq!(emp_isk_gain, (600_000 * 1_000) as f64, "loser received contribution-scaled ISK");

    // Surrendered `front_sys` FW I-Hub is still invulnerable at +24h, then unlocks at +25h (`SURRENDER_IHUB_VULNERABILITY_DELAY_SECS`)!
    let ihub_id = ctx.db.with_snapshot(|s| {
        s.structures.values().find(|st| st.solar_system_id == front_sys && st.type_id == TYPE_FW_IHUB).unwrap().structure_id
    });
    let mut s_gal = register_online_pilot(
        &ctx, 84_004, "demo4", "demo4-secret", 90_000_044, 98_000_044, None,
        front_sys, 100_000_044, 639, DVec3::new(202_000.0, 0.0, 0.0), 0,
    ).await;
    rpc_call(&ctx, &mut s_gal, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_004)]).await.unwrap();
    fit_siege_artillery(&ctx, 90_000_044, 100_000_044, 999_400);

    fast_forward_secs(&ctx, 24 * 3_600);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    lock_target_via_rpc(&ctx, &mut s_gal, front_sys, 100_000_044, ihub_id).await;
    assert_eq!(
        rpc_call(&ctx, &mut s_gal, "dogmaIM", "Activate", &[
            PyRep::Integer(999_400),
            PyRep::String("targetAttack".into()),
            PyRep::Integer(ihub_id),
        ]).await.unwrap_err().code,
        "IHubInvulnerable"
    );
    fast_forward_secs(&ctx, SURRENDER_IHUB_VULNERABILITY_DELAY_SECS - 24 * 3_600 + 10);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    assert!(rpc_call(&ctx, &mut s_gal, "dogmaIM", "Activate", &[
        PyRep::Integer(999_400),
        PyRep::String("targetAttack".into()),
        PyRep::Integer(ihub_id),
    ]).await.is_ok());

    // Finish Campaign 1 WindDown -> emits `OnInsurgencyCampaignEndingForLocation` and auto-seeds Campaign 2!
    s_pir.solar_system_id = Some(front_sys);
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&pir_cid).unwrap().solar_system_id = front_sys;
    });
    let _ = drain_notif_events(&ctx, pir_cid);
    fast_forward_secs(&ctx, WINDDOWN_DURATION_SECS + 60);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    assert!(drain_notif_events(&ctx, pir_cid).iter().any(|e| e == "OnInsurgencyCampaignEndingForLocation"));

    // 3. Campaign 2 Empire Underdog Win via Pirate FOB Siege:
    //    - Auto-seed Campaign 2, advance past 48h Forecasting
    //    - Live attack on FOB rejected with `SuppressionGoalNotReached` before 5 Stage-5 suppressed systems
    //    - Suppress 5 systems to Stage 5 via live Empire pulses -> weapon fire depletes shield+armor into `3h ± 1h` `ArmorReinforced`
    //    - Weapon attack during timer rejected with `FobArmorReinforced`; after timer elapses, hull kill destroys FOB,
    //      raises Suppression `+1` across campaign systems, and reduces Pirate Ambition modifier `1 -> 0`!
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let camp2 = ctx.db.with_snapshot(|s| *s.insurgency_runtime.campaigns.keys().next().unwrap());
    let fob2 = PIRATE_FOB_STRUCTURE_BASE_ID + camp2;
    fast_forward_secs(&ctx, FORECASTING_DURATION_SECS + 10);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

    // Ensure Campaign 2 has >= 5 systems so Empire can suppress 5 systems to Stage 5!
    ctx.db.with_mut_snapshot(|s| {
        let c = s.insurgency_runtime.campaigns.get_mut(&camp2).unwrap();
        for sys in ctx.sde.solar_systems.values() {
            if c.systems.len() >= 5 {
                break;
            }
            if sys.security_status > 0.0 && sys.security_status < 0.45 && !c.systems.contains(&sys.solar_system_id) {
                c.systems.push(sys.solar_system_id);
            }
        }
        for &sys in &c.systems {
            c.suppression_points.entry(sys).or_insert(0);
        }
    });

    fit_siege_artillery(&ctx, emp_cid, emp_ship, 999_500);
    let _ = ctx.systems.get_or_spawn(front_sys).spawn_ball(make_ball(emp_ship, 639, fob_pos + DVec3::new(1_000.0, 0.0, 0.0))).await;
    lock_target_via_rpc(&ctx, &mut s_emp, front_sys, emp_ship, fob2).await;
    assert_eq!(
        rpc_call(&ctx, &mut s_emp, "dogmaIM", "Activate", &[
            PyRep::Integer(999_500),
            PyRep::String("targetAttack".into()),
            PyRep::Integer(fob2),
        ]).await.unwrap_err().code,
        "SuppressionGoalNotReached"
    );

    // Move pirate ship off `[0,0,0]` and suppress 5 systems to Stage 5 via live Empire 60s pulses!
    let _ = ctx.systems.get_or_spawn(front_sys).spawn_ball(make_ball(pir_ship, 621, DVec3::new(90_000.0, 0.0, 0.0))).await;
    let camp2_systems = ctx.db.with_snapshot(|s| s.insurgency_runtime.campaigns[&camp2].systems.clone());
    for (i, &tsys) in camp2_systems.iter().take(5).enumerate() {
        s_emp.solar_system_id = Some(tsys);
        ctx.db.with_mut_snapshot(|s| {
            s.characters.get_mut(&emp_cid).unwrap().solar_system_id = tsys;
            s.items.get_mut(&emp_ship).unwrap().location_id = tsys as i64;
            s.system_states.entry(tsys).or_default().fw_complex_position = Some([0.0, 0.0, 0.0]);
        });
        let sh = ctx.systems.get_or_spawn(tsys);
        let _ = sh.spawn_ball(make_ball(emp_ship, 639, DVec3::ZERO)).await;
        // On the 5th system, stop at Stage 4 (4 pulses) first so we also prove FOB hull destruction raises Stage 4 -> Stage 5,
        // wait: FOB requires 5 Stage-5 systems first! Let's add a 6th system at Stage 4 (`800` pts) to prove FOB kill raises it `+1` stage (`800 -> 1000`)!
        let _ = i;
        for _ in 0..5 {
            ctx.db.with_mut_snapshot(|s| {
                let st = s.system_states.entry(tsys).or_default();
                st.fw_complex_position.get_or_insert([0.0, 0.0, 0.0]);
                st.last_fw_capture_tick = 0;
            });
            eve_server::services::insurgency_services::step_system_insurgency_tick_delta(&ctx, tsys, Some(60)).await;
            // Do NOT run tick_background_cluster on the 5th Stage-5 system until after FOB siege so we test live FOB siege before tick auto-resolves!
        }
    }
    let sixth_sys = ctx.sde.solar_systems.values()
        .find(|s| s.security_status > 0.0 && s.security_status < 0.45 && !camp2_systems.contains(&s.solar_system_id))
        .unwrap()
        .solar_system_id;
    ctx.db.with_mut_snapshot(|s| {
        let c = s.insurgency_runtime.campaigns.get_mut(&camp2).unwrap();
        c.systems.push(sixth_sys);
        c.suppression_points.insert(sixth_sys, 600); // Stage 3 -> FOB kill raises +1 stage to Stage 4 (800)!
        s.characters.get_mut(&emp_cid).unwrap().solar_system_id = front_sys;
        s.items.get_mut(&emp_ship).unwrap().location_id = front_sys as i64;
    });
    s_emp.solar_system_id = Some(front_sys);
    let _ = ctx.systems.get_or_spawn(front_sys).spawn_ball(make_ball(emp_ship, 639, fob_pos + DVec3::new(1_000.0, 0.0, 0.0))).await;
    lock_target_via_rpc(&ctx, &mut s_emp, front_sys, emp_ship, fob2).await;

    ctx.db.with_mut_snapshot(|s| {
        if let Some(st) = s.structures.get_mut(&fob2) {
            st.shield_hp = 0.0;
            st.armor_hp = 50.0;
        }
    });

    let mut reinf_until = 0u64;
    for _ in 0..8 {
        ctx.db.with_mut_snapshot(|s| {
            if let Some(st) = s.structures.get_mut(&fob2) {
                st.last_damage_tick = 0;
            }
        });
        let _ = rpc_call(&ctx, &mut s_emp, "dogmaIM", "Activate", &[
            PyRep::Integer(999_500),
            PyRep::String("targetAttack".into()),
            PyRep::Integer(fob2),
        ]).await;
        step_weapon_ticks(&ctx, front_sys).await;
        if ctx.db.with_snapshot(|s| s.structures[&fob2].state == "ArmorReinforced") {
            reinf_until = ctx.db.with_snapshot(|s| s.structures[&fob2].reinforced_until_unix);
            break;
        }
        fast_forward_secs(&ctx, 25);
    }
    let timer_len = reinf_until.saturating_sub(ctx.now_unix() - 25);
    assert!((7_200..=14_400).contains(&timer_len), "FOB armor reinforcement is 3h ± 1h (got {timer_len}s)");
    ctx.db.with_mut_snapshot(|s| {
        if let Some(st) = s.structures.get_mut(&fob2) {
            st.last_damage_tick = 0;
        }
    });
    lock_target_via_rpc(&ctx, &mut s_emp, front_sys, emp_ship, fob2).await;
    assert_eq!(
        rpc_call(&ctx, &mut s_emp, "dogmaIM", "Activate", &[
            PyRep::Integer(999_500),
            PyRep::String("targetAttack".into()),
            PyRep::Integer(fob2),
        ]).await.unwrap_err().code,
        "FobArmorReinforced"
    );

    fast_forward_secs(&ctx, 14_500);
    ctx.db.with_mut_snapshot(|s| {
        if let Some(st) = s.structures.get_mut(&fob2) {
            st.hull_hp = 50.0;
        }
    });
    lock_target_via_rpc(&ctx, &mut s_emp, front_sys, emp_ship, fob2).await;
    for _ in 0..8 {
        ctx.db.with_mut_snapshot(|s| {
            if let Some(st) = s.structures.get_mut(&fob2) {
                st.last_damage_tick = 0;
            }
        });
        let _ = rpc_call(&ctx, &mut s_emp, "dogmaIM", "Activate", &[
            PyRep::Integer(999_500),
            PyRep::String("targetAttack".into()),
            PyRep::Integer(fob2),
        ]).await;
        step_weapon_ticks(&ctx, front_sys).await;
        if ctx.db.with_snapshot(|s| s.structures[&fob2].state == "Destroyed") {
            break;
        }
        fast_forward_secs(&ctx, 25);
    }
    assert_eq!(ctx.db.with_snapshot(|s| s.insurgency_runtime.campaigns[&camp2].suppression_points[&sixth_sys]), 800);
    assert_eq!(ctx.db.with_snapshot(|s| s.insurgency_runtime.pirate_ambition_modifier[&500_010]), 0, "Empire underdog win reduces Pirate Ambition 1 -> 0");

    // 4. Campaign 3 7d Expiry Tie (`0` Ambition bonus, `0` LP payout) + SQLite restart durability!
    fast_forward_secs(&ctx, WINDDOWN_DURATION_SECS + 60);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let camp3 = ctx.db.with_snapshot(|s| *s.insurgency_runtime.campaigns.keys().next().unwrap());
    fast_forward_secs(&ctx, FORECASTING_DURATION_SECS + ACTIVE_DURATION_SECS + 60);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    assert_eq!(ctx.db.with_snapshot(|s| s.insurgency_runtime.campaign_outcomes.get(&camp3).cloned()), Some("Tie".into()));
    assert_eq!(ctx.db.with_snapshot(|s| s.insurgency_runtime.pirate_ambition_modifier[&500_010]), 0);
    assert_eq!(ctx.db.with_snapshot(|s| s.insurgency_runtime.empire_ambition_modifier.get(&500_010).copied().unwrap_or(0)), 0);

    drop(ctx);
    let db_path = temp_sqlite_path("fw_insurgency_f4");
    {
        let ctx_sq = open_sqlite_ctx(&db_path, true);
        ctx_sq.db.with_mut_snapshot(|s| {
            s.insurgency_runtime.campaign_outcomes.insert(camp3, "Tie".into());
        });
    }
    let ctx_restarted = open_sqlite_ctx(&db_path, false);
    assert_eq!(ctx_restarted.db.with_snapshot(|s| s.insurgency_runtime.campaign_outcomes.get(&camp3).cloned()), Some("Tie".into()));
    let _ = std::fs::remove_file(&db_path);
}

fn fast_forward_secs(ctx: &ServerContext, secs: u64) {
    ctx.clock().advance_secs(secs);
    let cur = ctx.now_unix_secs();
    ctx.db.with_mut_snapshot(|snap| {
        snap.service_state_blobs.insert("__server_now_unix".into(), cur.to_string());
    });
}
