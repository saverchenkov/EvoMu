//! Part 7 of FW & Insurgency closure tests:
//! - Highsec Corruption Stage 5 (`LAWLESS`) `dogmaIM.Activate` safety checks for Vorton Projectors
//!   (`group_id == 4060`) and cross-faction FW turret shots (`SafetyLevel::Yellow` allowed on ships,
//!   blocked on `SafetyLevel::Green` and on Capsules `type_id == 670`).
//! - Vorton Projector secondary arc and Smartbomb (`group_id == 72`) hits on Capsules (`670`) in
//!   Highsec Stage 5 escalating to Criminal flag + CONCORD response.
//! - SQLite reload durability of `system_corruption_stages` and `is_pilot_aligned_with_insurgency_pirate`
//!   returning `false` when no active/winddown campaign covers a solar system.

#![forbid(unsafe_code)]

use crate::combat_weapons_wrecks_part2::fit_module_and_charge;
use crate::fw_warzone_insurgency_closure::setup_warzone_chain;
use crate::sovereignty_equinox_gameplay_closure::{make_ball, register_online_pilot, rpc_call};
use eve_db::{
    FwSystemOccupationRecord, InventoryFlag, InventoryItemRecord, TrainedSkillRecord,
    VirtualServerClock, WorldRepository,
};
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::{with_mut_combat_runtime, SafetyLevel};
use eve_server::services::insurgency_services::{
    add_system_corruption, init_insurgency_campaign,
};
use eve_server::services::security_mgr::is_pilot_aligned_with_insurgency_pirate;
use glam::DVec3;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

fn temp_sqlite_path(tag: &str) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(1);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let p = std::env::temp_dir().join(format!("evomu_hs_stage5_{tag}_{}_{n}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&p);
    p
}

async fn lock_ball(ctx: &ServerContext, sys_id: u32, ship_id: i64, target_id: i64) {
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_h
        .add_target_lock(
            ship_id,
            eve_destiny::TargetLock {
                target_id,
                lock_time_ms: 0.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: Some(150_000.0),
            },
        )
        .await;
}

#[tokio::test]
async fn test_highsec_stage5_vorton_weapons_yellow_safety_and_arc_capsule_concord() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let (_enemy_sys, front_sys, _cmd_sys, _rear_sys) = setup_warzone_chain(&ctx);
    let hs_sys = 30_045_324u32; // Onnamon (0.5555 Highsec)

    let atk_cid = 90_000_191i64;
    let atk_ship = 100_000_191i64;
    let vic_cid = 90_000_192i64;
    let vic_ship = 100_000_192i64;
    let pod_cid = 90_000_193i64;
    let pod_ball = 100_000_193i64;

    // 54731 = Skybreaker (EDENCOM frigate)
    let mut s_atk = register_online_pilot(
        &ctx, 89_101, "demo", "demo-secret", atk_cid, 98_000_191, None, front_sys, atk_ship, 54_731, DVec3::ZERO, 0,
    )
    .await;
    rpc_call(&ctx, &mut s_atk, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_001)])
        .await
        .expect("enlist Caldari");
    let camp_id = 501i64;
    init_insurgency_campaign(&ctx, camp_id, 500_010, 500_001, front_sys, vec![front_sys, hs_sys]);

    let now0 = ctx.now_unix();
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&atk_cid).unwrap().solar_system_id = hs_sys;
        s.items.get_mut(&atk_ship).unwrap().location_id = i64::from(hs_sys);
        // Mark hs_sys FW-occupied and victim enlisted in non-opposing Amarr (`500_003`) so cross-faction
        // turret activation (`kf != vf`) ALSO exercises `fitting_module_cycles -> handle_weapons`.
        s.fw_system_occupations.insert(
            hs_sys,
            FwSystemOccupationRecord {
                solar_system_id: hs_sys,
                occupier_faction_id: 500_001,
                owner_faction_id: 500_001,
                victory_points: 0,
                victory_points_threshold: 3_000,
            },
        );
        for (cid, sid, tid, name, wf) in [
            (vic_cid, vic_ship, 621u32, "Amarr Neutral Ally", Some(500_003i64)),
            (pod_cid, pod_ball, 670u32, "Bystander Capsule", None),
        ] {
            let mut ch = s.characters[&atk_cid].clone();
            ch.character_id = cid;
            ch.name = name.into();
            ch.solar_system_id = hs_sys;
            ch.active_ship_id = sid;
            ch.war_faction_id = wf;
            s.characters.insert(cid, ch);
            s.items.insert(
                sid,
                InventoryItemRecord {
                    item_id: sid,
                    type_id: tid,
                    owner_id: cid,
                    location_id: i64::from(hs_sys),
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        }
        for &skill_id in &[3300u32, 3302, 55031, 55033, 55034] {
            s.skills.entry(atk_cid).or_default().insert(
                skill_id,
                TrainedSkillRecord { type_id: skill_id, level: 5, skill_points: 256_000.0 },
            );
        }
        s.insurgency_runtime.campaigns.get_mut(&camp_id).unwrap().forecasting_end_unix = now0.saturating_sub(1);
    });
    s_atk.solar_system_id = Some(hs_sys);
    eve_server::services::space_destiny::clear_session_change_timer(&ctx, atk_cid);

    let hs_h = ctx.systems.get_or_spawn(hs_sys);
    let _ = hs_h
        .spawn_ball_with_combat(make_ball(atk_ship, 54_731, DVec3::ZERO), ctx.build_ship_combat_state(atk_cid, atk_ship, 54_731))
        .await;
    let _ = hs_h
        .spawn_ball_with_combat(make_ball(vic_ship, 621, DVec3::new(3_000.0, 0.0, 0.0)), ctx.build_type_combat_state(621))
        .await;
    let _ = hs_h
        .spawn_ball_with_combat(make_ball(pod_ball, 670, DVec3::new(80_000.0, 0.0, 0.0)), ctx.build_type_combat_state(670))
        .await;

    let vorton_id = 701_001i64;
    let turret_id = 701_003i64;
    fit_module_and_charge(&ctx, atk_cid, atk_ship, vorton_id, 54_739, InventoryFlag::HiSlot0, Some((701_002, 54_769, 200)));
    fit_module_and_charge(&ctx, atk_cid, atk_ship, turret_id, 2_873, InventoryFlag::HiSlot1, Some((701_004, 182, 200)));
    ctx.db.with_mut_snapshot(|s| {
        s.module_online_states.insert(vorton_id, true);
        s.module_online_states.insert(turret_id, true);
    });
    ctx.invalidate_ship_fitting(atk_ship);
    lock_ball(&ctx, hs_sys, atk_ship, vic_ship).await;
    lock_ball(&ctx, hs_sys, atk_ship, pod_ball).await;

    assert_eq!(add_system_corruption(&ctx, camp_id, hs_sys, 1_000), (1_000, 5));

    // 1. Safety Green blocks Vorton & cross-faction turret against non-pod ship in Highsec Stage 5:
    with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(atk_cid, SafetyLevel::Green);
    });
    let err_green = rpc_call(&ctx, &mut s_atk, "dogmaIM", "Activate", &[PyRep::Integer(vorton_id), PyRep::Integer(vic_ship)])
        .await
        .unwrap_err();
    assert_eq!(err_green.code, "CantDoInCurrentSafetyLevel");

    // 2. Safety Yellow blocks direct Capsule (`670`) attack, but ALLOWS Vorton & cross-faction turret on non-pod ship (Suspect, no CONCORD):
    with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(atk_cid, SafetyLevel::Yellow);
    });
    let err_pod_yellow = rpc_call(&ctx, &mut s_atk, "dogmaIM", "Activate", &[PyRep::Integer(vorton_id), PyRep::Integer(pod_ball)])
        .await
        .unwrap_err();
    assert_eq!(err_pod_yellow.code, "CantDoInCurrentSafetyLevel");

    rpc_call(&ctx, &mut s_atk, "dogmaIM", "Activate", &[PyRep::Integer(vorton_id), PyRep::Integer(vic_ship)])
        .await
        .expect("Vorton Activate on non-pod ship must succeed under Safety Yellow in Highsec Stage 5");
    rpc_call(&ctx, &mut s_atk, "dogmaIM", "Activate", &[PyRep::Integer(turret_id), PyRep::Integer(vic_ship)])
        .await
        .expect("Cross-faction FW turret Activate on non-pod ship must succeed under Safety Yellow in Highsec Stage 5");

    let now_shot1 = ctx.now_unix();
    let (susp1, crim1, concord1) = ctx.crimewatch.with_crimewatch(|cw| {
        let st = &cw.states[&atk_cid];
        (st.is_suspect(now_shot1), st.is_criminal(now_shot1), st.pending_concord_arrival.is_some())
    });
    assert_eq!(
        (susp1, crim1, concord1),
        (true, false, false),
        "Highsec Stage 5 non-pod Vorton/turret shots under Safety Yellow must grant Suspect without Criminal or CONCORD"
    );

    // 3. Move Capsule (`670`) within 10 km Vorton secondary arc range of `vic_ship`:
    //    - Under Safety Yellow, Vorton skips the Capsule secondary arc without Criminal/CONCORD.
    //    - Under Safety Red, Vorton secondary arc hits Capsule (`670`) and escalates to Criminal + CONCORD!
    {
        let mut bp = hs_h.ballpark.write().unwrap_or_else(|e| e.into_inner());
        bp.set_ball_position(pod_ball, DVec3::new(3_500.0, 0.0, 0.0));
    }
    ctx.db.with_mut_snapshot(|s| {
        s.module_active_states.remove(&vorton_id);
    });
    let v_yellow_arc = rpc_call(&ctx, &mut s_atk, "dogmaIM", "Activate", &[PyRep::Integer(vorton_id), PyRep::Integer(vic_ship)])
        .await
        .expect("Vorton shot with nearby pod under Safety Yellow");
    assert_eq!(v_yellow_arc.dict_get("secondaryTargetsHit").and_then(|v| v.as_i64()), Some(0));

    with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(atk_cid, SafetyLevel::Red);
    });
    ctx.db.with_mut_snapshot(|s| {
        s.module_active_states.remove(&vorton_id);
    });
    let v_red_arc = rpc_call(&ctx, &mut s_atk, "dogmaIM", "Activate", &[PyRep::Integer(vorton_id), PyRep::Integer(vic_ship)])
        .await
        .expect("Vorton shot chaining onto Capsule under Safety Red");
    assert_eq!(v_red_arc.dict_get("secondaryTargetsHit").and_then(|v| v.as_i64()), Some(1));

    let now_arc = ctx.now_unix();
    let (arc_crim, arc_concord) = ctx.crimewatch.with_crimewatch(|cw| {
        let st = &cw.states[&atk_cid];
        (st.is_criminal(now_arc), st.pending_concord_arrival.is_some())
    });
    assert_eq!(
        (arc_crim, arc_concord),
        (true, true),
        "Vorton secondary arc hitting Capsule (670) in Highsec Stage 5 must escalate to Criminal + CONCORD"
    );
    ctx.advance_secs(25);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let balls_after = hs_h.list_balls().await.unwrap();
    assert!(
        balls_after.iter().any(|b| b.type_id == 11_957),
        "CONCORD Police Commander (11957) must spawn after Vorton secondary arc hits a Capsule in Highsec Stage 5"
    );
}

#[tokio::test]
async fn test_highsec_stage5_smartbomb_capsule_concord_and_corruption_reload_persistence() {
    let db_path = temp_sqlite_path("smartbomb_reload");
    let hs_sys = 30_045_324u32; // Onnamon (0.5555 Highsec)
    let jita_sys = 30_000_142u32;
    let t0 = 1_770_000_000u64;

    let atk_cid = 90_000_001i64;
    let atk_ship = 100_000_001i64;
    let ship_vic_cid = 90_000_202i64;
    let ship_vic_ball = 100_000_202i64;
    let pod_vic_cid = 90_000_203i64;
    let pod_vic_ball = 100_000_203i64;
    let sb_mod_id = 702_001i64;
    let sb_mod_id2 = 702_002i64;

    // Phase 1: Persist Highsec Stage 5 campaign & system state to SQLite, and verify `!any_covering => false`
    {
        let repo = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open_sqlite 1"));
        let seed_snap = WorldRepository::with_demo_seed().with_snapshot(|s| s.clone());
        repo.with_mut_snapshot(|snap| *snap = seed_snap);
        let clock = Arc::new(VirtualServerClock::with_fixed_base(t0));
        repo.set_clock(clock.clone());
        let ctx1 = ServerContext::with_repository(repo);
        ctx1.set_clock(clock);

        let (_enemy_sys, front_sys, _cmd_sys, _rear_sys) = setup_warzone_chain(&ctx1);
        let mut s1 = register_online_pilot(
            &ctx1, 89_201, "demo", "demo-secret", atk_cid, 98_000_001, None, front_sys, atk_ship, 639, DVec3::ZERO, 0,
        )
        .await;
        rpc_call(&ctx1, &mut s1, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_010)])
            .await
            .unwrap();
        let _ = rpc_call(&ctx1, &mut s1, "dungeonInstanceCacheMgr", "GetFactionWarfareInstances", &[])
            .await
            .unwrap();
        eve_server::sp6_lifecycle::tick_background_cluster(&ctx1).await;

        let camp_id = ctx1.db.with_mut_snapshot(|s| {
            // Uncovered system with stale `insurgency_corruption_stage = 5` must NOT grant pirate alignment!
            let j_st = s.system_states.entry(jita_sys).or_default();
            j_st.solar_system_id = jita_sys;
            j_st.insurgency_corruption_stage = 5;

            s.characters.get_mut(&atk_cid).unwrap().solar_system_id = hs_sys;
            s.items.get_mut(&atk_ship).unwrap().location_id = i64::from(hs_sys);
            for (mid, flag) in [(sb_mod_id, InventoryFlag::HiSlot0), (sb_mod_id2, InventoryFlag::HiSlot1)] {
                s.items.insert(
                    mid,
                    InventoryItemRecord {
                        item_id: mid,
                        type_id: 3_993, // Large EMP Smartbomb I (group 72, 6,000 m radius)
                        owner_id: atk_cid,
                        location_id: atk_ship,
                        flag,
                        quantity: 1,
                        singleton: true,
                        ..Default::default()
                    },
                );
                s.module_online_states.insert(mid, true);
            }
            for (cid, sid, tid, name) in [
                (ship_vic_cid, ship_vic_ball, 621u32, "Smartbomb Ship Victim"),
                (pod_vic_cid, pod_vic_ball, 670u32, "Smartbomb Pod Victim"),
            ] {
                let mut ch = s.characters[&atk_cid].clone();
                ch.character_id = cid;
                ch.name = name.into();
                ch.solar_system_id = hs_sys;
                ch.active_ship_id = sid;
                ch.war_faction_id = None;
                s.characters.insert(cid, ch);
                s.items.insert(
                    sid,
                    InventoryItemRecord {
                        item_id: sid,
                        type_id: tid,
                        owner_id: cid,
                        location_id: i64::from(hs_sys),
                        flag: InventoryFlag::None,
                        quantity: 1,
                        singleton: true,
                        ..Default::default()
                    },
                );
            }
            let (&cid, c) = s.insurgency_runtime.campaigns.iter_mut().next().unwrap();
            c.forecasting_end_unix = t0.saturating_sub(10);
            if !c.systems.contains(&hs_sys) {
                c.systems.push(hs_sys);
            }
            cid
        });

        assert!(
            !is_pilot_aligned_with_insurgency_pirate(&ctx1, jita_sys, atk_cid),
            "Pirate alignment must return false when no active/winddown campaign covers the solar system"
        );
        assert_eq!(add_system_corruption(&ctx1, camp_id, hs_sys, 1_000), (1_000, 5));
        assert!(is_pilot_aligned_with_insurgency_pirate(&ctx1, hs_sys, atk_cid));
    }

    // Phase 2: Reload fresh ServerContext from SQLite WITHOUT calling `add_system_corruption`!
    {
        let repo2 = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open_sqlite 2"));
        let clock2 = Arc::new(VirtualServerClock::with_fixed_base(t0 + 30));
        repo2.set_clock(clock2.clone());
        let ctx2 = ServerContext::with_repository(repo2);
        ctx2.set_clock(clock2);

        assert_eq!(
            ctx2.crimewatch.read().unwrap().system_corruption_stages.get(&hs_sys),
            Some(&5),
            "Highsec Stage 5 system_corruption_stages must hydrate immediately on SQLite reload"
        );

        let mut s2 = eve_machonet::SessionState::new(89_202);
        crate::common::login_pilot(&ctx2, &mut s2, "demo", "demo-secret").await;
        rpc_call(&ctx2, &mut s2, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(atk_cid)])
            .await
            .unwrap();
        eve_server::services::space_destiny::clear_session_change_timer(&ctx2, atk_cid);

        let hs_h2 = ctx2.systems.get_or_spawn(hs_sys);
        let mut atk_cs = ctx2.build_ship_combat_state(atk_cid, atk_ship, 639);
        atk_cs.capacitor = 5_000.0;
        atk_cs.max_capacitor = 5_000.0;
        let _ = hs_h2.spawn_ball_with_combat(make_ball(atk_ship, 639, DVec3::ZERO), atk_cs).await;
        let _ = hs_h2
            .spawn_ball_with_combat(make_ball(ship_vic_ball, 621, DVec3::new(2_500.0, 0.0, 0.0)), ctx2.build_type_combat_state(621))
            .await;
        let _ = hs_h2
            .spawn_ball_with_combat(make_ball(pod_vic_ball, 670, DVec3::new(25_000.0, 0.0, 0.0)), ctx2.build_type_combat_state(670))
            .await;

        with_mut_combat_runtime(&ctx2, |rt| {
            rt.safety_levels.insert(atk_cid, SafetyLevel::Red);
        });

        // (a) Smartbomb hitting ONLY non-pod ship in reloaded Highsec Stage 5 -> Suspect, concordTriggered == false, no CONCORD!
        let sb_ship_res = rpc_call(&ctx2, &mut s2, "dogmaIM", "Activate", &[PyRep::Integer(sb_mod_id)])
            .await
            .expect("Smartbomb Activate hitting non-pod ship in Highsec Stage 5");
        assert_eq!(sb_ship_res.dict_get("concordTriggered"), Some(&PyRep::Bool(false)));
        let now1 = ctx2.now_unix();
        let (sb_susp, sb_crim, sb_concord) = ctx2.crimewatch.with_crimewatch(|cw| {
            let st = &cw.states[&atk_cid];
            (st.is_suspect(now1), st.is_criminal(now1), st.pending_concord_arrival.is_some())
        });
        assert_eq!(
            (sb_susp, sb_crim, sb_concord),
            (true, false, false),
            "Reloaded Highsec Stage 5 Smartbomb hitting only non-pod ship must grant Suspect without CONCORD"
        );

        // (b) Move Capsule (`670`) into 6 km blast radius and fire second Smartbomb -> Criminal + CONCORD!
        {
            let mut bp = hs_h2.ballpark.write().unwrap_or_else(|e| e.into_inner());
            bp.set_ball_position(pod_vic_ball, DVec3::new(2_000.0, 0.0, 0.0));
        }
        let sb_pod_res = rpc_call(&ctx2, &mut s2, "dogmaIM", "Activate", &[PyRep::Integer(sb_mod_id2)])
            .await
            .expect("Smartbomb Activate hitting Capsule in Highsec Stage 5");
        assert_eq!(sb_pod_res.dict_get("concordTriggered"), Some(&PyRep::Bool(true)));
        let now2 = ctx2.now_unix();
        let (pod_crim, pod_concord) = ctx2.crimewatch.with_crimewatch(|cw| {
            let st = &cw.states[&atk_cid];
            (st.is_criminal(now2), st.pending_concord_arrival.is_some())
        });
        assert_eq!(
            (pod_crim, pod_concord),
            (true, true),
            "Smartbomb hitting Capsule (670) in Highsec Stage 5 must escalate to Criminal + CONCORD"
        );
        ctx2.advance_secs(25);
        eve_server::sp6_lifecycle::tick_background_cluster(&ctx2).await;
        let balls_after = hs_h2.list_balls().await.unwrap();
        assert!(
            balls_after.iter().any(|b| b.type_id == 11_957),
            "CONCORD Police Commander (11957) must spawn after Smartbomb hits a Capsule in Highsec Stage 5"
        );
    }

    let _ = std::fs::remove_file(&db_path);
}

#[tokio::test]
async fn test_ci_sentry_grid_targeting_destruction_and_lowsec_sentry_separation() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let (enemy_sys, front_sys, _cmd_sys, _rear_sys) = setup_warzone_chain(&ctx);

    let emp_cid = 90_000_301i64;
    let emp_ship = 100_000_301i64;
    let neut_cid = 90_000_302i64;
    let neut_ship = 100_000_302i64;
    let crim_cid = 90_000_303i64;
    let crim_ship = 100_000_303i64;
    let pir_cid = 90_000_304i64;
    let pir_ship = 100_000_304i64;
    let offgrid_pir_cid = 90_000_305i64;
    let offgrid_pir_ship = 100_000_305i64;

    let mut s_emp = register_online_pilot(
        &ctx, 89_301, "demo", "demo-secret", emp_cid, 98_000_301, None, enemy_sys, emp_ship, 621, DVec3::new(15_000.0, 0.0, 0.0), 0,
    )
    .await;
    rpc_call(&ctx, &mut s_emp, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_001)])
        .await
        .unwrap();

    init_insurgency_campaign(&ctx, 701, 500_010, 500_001, front_sys, vec![front_sys, enemy_sys]);
    let now0 = ctx.now_unix();
    ctx.db.with_mut_snapshot(|s| {
        s.insurgency_runtime.campaigns.get_mut(&701).unwrap().forecasting_end_unix = now0.saturating_sub(1);
        for (cid, sid, wf, name) in [
            (neut_cid, neut_ship, None, "Neutral Suspect"),
            (crim_cid, crim_ship, None, "Lowsec Criminal"),
            (pir_cid, pir_ship, Some(500_010i64), "On-Grid Pirate"),
            (offgrid_pir_cid, offgrid_pir_ship, Some(500_010i64), "Off-Grid Pirate"),
        ] {
            let mut ch = s.characters[&emp_cid].clone();
            ch.character_id = cid;
            ch.name = name.into();
            ch.active_ship_id = sid;
            ch.war_faction_id = wf;
            s.characters.insert(cid, ch);
            s.items.insert(
                sid,
                InventoryItemRecord {
                    item_id: sid, type_id: 621, owner_id: cid, location_id: i64::from(enemy_sys),
                    flag: InventoryFlag::None, quantity: 1, singleton: true, ..Default::default()
                },
            );
        }
    });
    // Reach Corruption 5 FIRST, then Suppression 5 in `enemy_sys`:
    assert_eq!(add_system_corruption(&ctx, 701, enemy_sys, 1_000), (1_000, 5));
    assert_eq!(eve_server::services::insurgency_services::add_system_suppression(&ctx, 701, enemy_sys, 1_000), (1_000, 5));
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

    let sys_h = ctx.systems.get_or_spawn(enemy_sys);
    for (sid, pos) in [
        (emp_ship, DVec3::new(15_000.0, 0.0, 0.0)),
        (neut_ship, DVec3::new(20_000.0, 0.0, 0.0)),
        (crim_ship, DVec3::new(30_000.0, 0.0, 0.0)),
        (pir_ship, DVec3::new(120_000.0, 0.0, 0.0)),      // > 75 km (TP only, no ECM)
        (offgrid_pir_ship, DVec3::new(900_000.0, 0.0, 0.0)), // > 500 km off-grid
    ] {
        let mut cs = ctx.build_type_combat_state(621);
        cs.shield_hp = 50_000.0;
        cs.max_shield_hp = 50_000.0;
        let _ = sys_h.spawn_ball_with_combat(make_ball(sid, 621, pos), cs).await;
    }
    // Move roaming pirates/enforcers far away so we isolate `79_507` Counter-Insurgency Sentry Gun behavior:
    let all_balls = sys_h.list_balls().await.unwrap();
    let ci_ball_id = all_balls.iter().find(|b| b.type_id == 79_507).expect("79507 deployed").id;
    {
        let mut bp = sys_h.ballpark.write().unwrap_or_else(|e| e.into_inner());
        for b in all_balls.iter().filter(|b| b.id != ci_ball_id && b.id < 100_000_300) {
            bp.set_ball_position(b.id, DVec3::new(-900_000.0, 0.0, 0.0));
        }
    }
    assert!(eve_server::services::security_mgr::has_active_counter_insurgency_sentries(&ctx, enemy_sys));
    assert!(eve_server::services::combat_npc_police::is_sentry_disabled_by_corruption(&ctx, enemy_sys));
    assert_eq!(
        eve_server::services::combat_crimewatch::step_sentry_gun_grid_aggression(&ctx, enemy_sys, crim_cid, crim_ship, DVec3::ZERO, true).await,
        (false, 0.0),
        "Regular gate/station sentries must stay disabled at Corruption 5 and never morph into 79507"
    );

    // Flag Empire militia pilot AND neutral pilot as Suspects, and `crim_cid` as Criminal:
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        cw.trigger_suspect_flag(emp_cid, now0);
        cw.trigger_suspect_flag(neut_cid, now0);
        cw.get_or_create(crim_cid).criminal_flag_until_unix = now0 + 900;
    }
    let base_sig = ctx.build_type_combat_state(621).signature_radius;

    // Step 1: `79_507` shoots closest valid target (`crim_ship` at ~30 km -> volley + TP + ECM),
    //         while completely ignoring closer `emp_ship` (15 km Suspect) and `neut_ship` (20 km Suspect)!
    eve_server::services::combat_crimewatch::combat_npc_ai::step_extended_npc_ai_sync(&ctx, enemy_sys);
    let emp_cs = sys_h.get_combat_state(emp_ship).await.unwrap().unwrap();
    let neut_cs = sys_h.get_combat_state(neut_ship).await.unwrap().unwrap();
    let crim_cs = sys_h.get_combat_state(crim_ship).await.unwrap().unwrap();
    assert_eq!((emp_cs.shield_hp, emp_cs.signature_radius, emp_cs.ecm_jammed_remaining_s), (50_000.0, base_sig, 0.0), "Empire Suspect must NOT be shot/painted/jammed by 79507");
    assert_eq!((neut_cs.shield_hp, neut_cs.signature_radius, neut_cs.ecm_jammed_remaining_s), (50_000.0, base_sig, 0.0), "Neutral Suspect must NOT be shot/painted/jammed by 79507");
    assert!(crim_cs.shield_hp < 50_000.0 && (crim_cs.signature_radius - base_sig * 1.50).abs() < 1e-3 && (crim_cs.ecm_jammed_remaining_s - 5.0).abs() < 1e-6);

    // Step 2: Remove `crim_ship` so `79_507` retargets `pir_ship` at 120 km (within 250 km TP, outside 75 km ECM)
    //         while `offgrid_pir_ship` at 900 km takes zero fire!
    let _ = sys_h.remove_ball(crim_ship).await;
    eve_server::services::combat_crimewatch::combat_npc_ai::step_extended_npc_ai_sync(&ctx, enemy_sys);
    let pir_cs = sys_h.get_combat_state(pir_ship).await.unwrap().unwrap();
    let off_cs = sys_h.get_combat_state(offgrid_pir_ship).await.unwrap().unwrap();
    assert!(pir_cs.shield_hp < 50_000.0 && (pir_cs.signature_radius - base_sig * 1.50).abs() < 1e-3 && pir_cs.ecm_jammed_remaining_s == 0.0);
    assert_eq!(off_cs.shield_hp, 50_000.0, "Off-grid pirate (900 km > 500 km) must not be hit by 79507");

    // Step 3: Destroy `79_507` ball -> `has_active_counter_insurgency_sentries` becomes false, does not auto-respawn on tick, and stops all fire!
    let _ = sys_h.remove_ball(ci_ball_id).await;
    eve_server::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.npc_entities.remove(&ci_ball_id);
    });
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    assert!(!eve_server::services::security_mgr::has_active_counter_insurgency_sentries(&ctx, enemy_sys));
    let hp_before = sys_h.get_combat_state(pir_ship).await.unwrap().unwrap().shield_hp;
    eve_server::services::combat_crimewatch::combat_npc_ai::step_extended_npc_ai_sync(&ctx, enemy_sys);
    assert_eq!(sys_h.get_combat_state(pir_ship).await.unwrap().unwrap().shield_hp, hp_before);
}

#[tokio::test]
async fn test_unscoped_corruption5_rejected_lowsec_sec_loss_and_vorton_arc_notifications() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let (_enemy_sys, front_sys, _cmd_sys, _rear_sys) = setup_warzone_chain(&ctx);
    let stale_ls_sys = 30_002_538u32; // Lowsec system NOT in any insurgency campaign

    let atk_cid = 90_000_401i64;
    let atk_ship = 100_000_401i64;
    let vic_cid = 90_000_402i64;
    let vic_ship = 100_000_402i64;
    let pod_cid = 90_000_403i64;
    let pod_ball = 100_000_403i64;
    let npc_ball = 900_777_001i64;

    // 22464 = Flycatcher (Interdictor, group 541) in `stale_ls_sys` with stale `insurgency_corruption_stage = 5`
    let mut s_atk = register_online_pilot(
        &ctx, 89_401, "demo", "demo-secret", atk_cid, 98_000_401, None, stale_ls_sys, atk_ship, 22_464, DVec3::ZERO, 0,
    )
    .await;
    let bomb_mod = 704_001i64;
    let bubble_mod = 704_003i64;
    let mwd_item = 704_005i64;
    fit_module_and_charge(&ctx, atk_cid, atk_ship, bomb_mod, 27_914, InventoryFlag::HiSlot0, Some((704_002, 27_916, 5)));
    fit_module_and_charge(&ctx, atk_cid, atk_ship, bubble_mod, 22_782, InventoryFlag::HiSlot1, Some((704_004, 22_778, 5)));
    ctx.db.with_mut_snapshot(|s| {
        let st = s.system_states.entry(stale_ls_sys).or_default();
        st.solar_system_id = stale_ls_sys;
        st.insurgency_corruption_stage = 5; // Stale stage 5 WITHOUT an active campaign!
        s.module_online_states.insert(bomb_mod, true);
        s.module_online_states.insert(bubble_mod, true);
        s.items.insert(
            mwd_item,
            InventoryItemRecord {
                item_id: mwd_item, type_id: 12_198, owner_id: atk_cid, location_id: atk_ship,
                flag: InventoryFlag::Cargo, quantity: 1, singleton: true, ..Default::default()
            },
        );
        for &sk in &[3300u32, 3302, 3433, 3435, 11584, 12098, 28073, 55031, 55033, 55034] {
            s.skills.entry(atk_cid).or_default().insert(sk, TrainedSkillRecord { type_id: sk, level: 5, skill_points: 256_000.0 });
        }
    });
    ctx.invalidate_ship_fitting(atk_ship);

    // CW-02(a): Stale `insurgency_corruption_stage = 5` without a covering campaign MUST reject Green-safety Bombs, Bubbles, and Mobile Warp Disruptors!
    with_mut_combat_runtime(&ctx, |rt| { rt.safety_levels.insert(atk_cid, SafetyLevel::Green); });
    assert_eq!(rpc_call(&ctx, &mut s_atk, "dogmaIM", "Activate", &[PyRep::Integer(bomb_mod)]).await.unwrap_err().code, "CantDoInCurrentSafetyLevel");
    with_mut_combat_runtime(&ctx, |rt| { rt.safety_levels.insert(atk_cid, SafetyLevel::Red); });
    assert_eq!(rpc_call(&ctx, &mut s_atk, "dogmaIM", "Activate", &[PyRep::Integer(bubble_mod)]).await.unwrap_err().code, "CannotLaunchInterdictionProbeInEmpire");
    assert_eq!(rpc_call(&ctx, &mut s_atk, "posMgr", "AnchorStructure", &[PyRep::Integer(mwd_item)]).await.unwrap_err().code, "CantAnchorInEmpire");

    // CW-02(b) & CW-03: Move to campaign-covered Lowsec `front_sys` at Corruption Stage 5 in a Skybreaker (`54731`) with Vorton Projector + Turret.
    init_insurgency_campaign(&ctx, 702, 500_010, 500_001, front_sys, vec![front_sys]);
    let now0 = ctx.now_unix();
    let vorton_mod = 704_010i64;
    let turret_mod = 704_012i64;
    ctx.db.with_mut_snapshot(|s| {
        s.insurgency_runtime.campaigns.get_mut(&702).unwrap().forecasting_end_unix = now0.saturating_sub(1);
        s.characters.get_mut(&atk_cid).unwrap().solar_system_id = front_sys;
        s.characters.get_mut(&atk_cid).unwrap().security_status = 2.0;
        s.items.get_mut(&atk_ship).unwrap().type_id = 54_731;
        s.items.get_mut(&atk_ship).unwrap().location_id = i64::from(front_sys);
        for (cid, sid, tid, name) in [(vic_cid, vic_ship, 621u32, "Lowsec Ship Victim"), (pod_cid, pod_ball, 670u32, "Lowsec Pod Victim")] {
            let mut ch = s.characters[&atk_cid].clone();
            ch.character_id = cid;
            ch.name = name.into();
            ch.active_ship_id = sid;
            ch.war_faction_id = None;
            s.characters.insert(cid, ch);
            s.items.insert(sid, InventoryItemRecord { item_id: sid, type_id: tid, owner_id: cid, location_id: i64::from(front_sys), flag: InventoryFlag::None, quantity: 1, singleton: true, ..Default::default() });
        }
    });
    fit_module_and_charge(&ctx, atk_cid, atk_ship, vorton_mod, 54_739, InventoryFlag::HiSlot2, Some((704_011, 54_769, 200)));
    fit_module_and_charge(&ctx, atk_cid, atk_ship, turret_mod, 2_873, InventoryFlag::HiSlot3, Some((704_013, 182, 200)));
    ctx.db.with_mut_snapshot(|s| { s.module_online_states.insert(vorton_mod, true); s.module_online_states.insert(turret_mod, true); });
    ctx.invalidate_ship_fitting(atk_ship);
    s_atk.solar_system_id = Some(front_sys);
    eve_server::services::space_destiny::clear_session_change_timer(&ctx, atk_cid);
    assert_eq!(add_system_corruption(&ctx, 702, front_sys, 1_000), (1_000, 5));

    let front_h = ctx.systems.get_or_spawn(front_sys);
    let _ = front_h.spawn_ball_with_combat(make_ball(atk_ship, 54_731, DVec3::ZERO), ctx.build_ship_combat_state(atk_cid, atk_ship, 54_731)).await;
    let _ = front_h.spawn_ball_with_combat(make_ball(vic_ship, 621, DVec3::new(2_500.0, 0.0, 0.0)), ctx.build_type_combat_state(621)).await;
    let _ = front_h.spawn_ball_with_combat(make_ball(npc_ball, 10_029, DVec3::new(3_000.0, 0.0, 0.0)), ctx.build_type_combat_state(10_029)).await;
    let _ = front_h.spawn_ball_with_combat(make_ball(pod_ball, 670, DVec3::new(3_400.0, 0.0, 0.0)), ctx.build_type_combat_state(670)).await;
    lock_ball(&ctx, front_sys, atk_ship, vic_ship).await;
    lock_ball(&ctx, front_sys, atk_ship, npc_ball).await;

    // CW-02(b): Illegal turret aggression against `vic_ship` in Lowsec Corruption 5 STILL applies security-status loss!
    let _ = ctx.drain_notifications_for_char(atk_cid);
    rpc_call(&ctx, &mut s_atk, "dogmaIM", "Activate", &[PyRep::Integer(turret_mod), PyRep::Integer(vic_ship)]).await.expect("Lowsec C5 turret shot");
    let sec_after_turret = ctx.db.with_snapshot(|s| s.characters[&atk_cid].security_status);
    assert!(sec_after_turret < 2.0, "Lowsec Corruption 5 illegal ship attack must still reduce security_status (got {sec_after_turret})");

    // CW-03: Fire Vorton Projector at legal NPC primary target (`npc_ball`) whose secondary arc chains onto innocent Capsule (`pod_ball`):
    //        must reduce `security_status` further AND emit `OnCriminalTimerUpdate` (301) + `OnSystemCriminalFlagUpdates`!
    let _ = ctx.drain_notifications_for_char(atk_cid);
    let v_res = rpc_call(&ctx, &mut s_atk, "dogmaIM", "Activate", &[PyRep::Integer(vorton_mod), PyRep::Integer(npc_ball)]).await.expect("Vorton on NPC arcing to pod");
    assert!(v_res.dict_get("secondaryTargetsHit").and_then(|v| v.as_i64()).unwrap_or(0) >= 1);
    let sec_after_vorton = ctx.db.with_snapshot(|s| s.characters[&atk_cid].security_status);
    assert!(sec_after_vorton < sec_after_turret, "Vorton secondary arc hitting innocent Capsule must apply compute_sec_status_loss ({sec_after_vorton} < {sec_after_turret})");
    let notifs = ctx.drain_notifications_for_char(atk_cid);
    assert!(notifs.iter().any(|n| n.event == "OnCriminalTimerUpdate" && n.args.first() == Some(&PyRep::Integer(301))), "Vorton secondary arc must emit OnCriminalTimerUpdate(301), got {notifs:?}");
    assert!(notifs.iter().any(|n| n.event == "OnSystemCriminalFlagUpdates"), "Vorton secondary arc must broadcast OnSystemCriminalFlagUpdates");
}
