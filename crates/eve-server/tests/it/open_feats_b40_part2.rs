//! Integration tests for Pirate Insurgency Suppression (Stages 1–5) system effects,
//! effect removal upon stage drop, and negative RPC auth/validation (`feat:fw.corruption-suppression-effects`, Part 2).

#![forbid(unsafe_code)]

use crate::open_feats_b40::{
    fast_forward_secs, fit_tactical_sabre, in_memory_ctx, local_campaign_stages,
    lookup_modified_sec, pulse_complex_60s, setup_warzone_chain_with_hs_station,
};
use crate::sovereignty_equinox_gameplay_closure::{
    fit_siege_artillery, make_ball, place_ship_ball, register_online_pilot, rpc_call,
    set_pilot_location,
};
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::TargetLock;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::{
    spawn_specialized_npc_entity, with_combat_runtime, with_mut_combat_runtime,
};
use eve_server::services::insurgency_services::{
    ACTIVE_DURATION_SECS, FORECASTING_DURATION_SECS, WINDDOWN_DURATION_SECS,
};
use glam::DVec3;
use std::collections::HashMap;

async fn measure_live_npc_kill_bounty_and_sec_gain(
    ctx: &ServerContext,
    sess: &mut SessionState,
    cid: i64,
    ship_id: i64,
    gun_id: i64,
    sys_id: u32,
    rat_type_id: u32,
) -> (f64, f64) {
    set_pilot_location(ctx, sess, sys_id, None);
    let pos = DVec3::new(350_000.0, -250_000.0, 0.0);
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&cid).unwrap().security_status = 0.0;
        s.sp6_lifecycle.ship_combat_hp.remove(&ship_id);
        if let Some(it) = s.items.get_mut(&ship_id) {
            it.location_id = i64::from(sys_id);
        }
    });
    with_mut_combat_runtime(ctx, |rt| {
        rt.pending_bounty_ticks.remove(&(sys_id, cid));
    });
    place_ship_ball(ctx, sys_id, cid, ship_id, 22_456, pos).await;
    let npc_ball_id =
        spawn_specialized_npc_entity(ctx, sys_id, rat_type_id, "Pirate", 1, pos + DVec3::new(250.0, 0.0, 0.0)).await;
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    sys_h
        .add_target_lock(
            ship_id,
            TargetLock {
                target_id: npc_ball_id,
                lock_time_ms: 0.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: Some(50_000.0),
            },
        )
        .await
        .unwrap();
    rpc_call(ctx, sess, "dogmaIM", "Activate", &[PyRep::Integer(gun_id), PyRep::Integer(npc_ball_id)])
        .await
        .expect("live NPC rat kill via dogmaIM.Activate");
    for _ in 0..30 {
        sys_h.step_manual(3.0).await.unwrap();
        if sys_h.get_ball_snapshot(npc_ball_id).await.unwrap().is_none() {
            break;
        }
    }
    let _ = rpc_call(ctx, sess, "dogmaIM", "Deactivate", &[PyRep::Integer(gun_id)]).await;
    assert!(sys_h.get_ball_snapshot(npc_ball_id).await.unwrap().is_none(), "NPC rat {npc_ball_id} must be destroyed by live turret cycles");
    let bounty = with_combat_runtime(ctx, |rt| rt.pending_bounty_ticks.get(&(sys_id, cid)).copied().unwrap_or(0.0));
    let sec_gain = ctx.db.with_snapshot(|s| s.characters[&cid].security_status);
    assert!(bounty > 0.0 && sec_gain > 0.0);
    (bounty, sec_gain)
}

async fn measure_live_militia_pvp_kill_lp(
    ctx: &ServerContext,
    killer_sess: &mut SessionState,
    killer_cid: i64,
    killer_ship: i64,
    gun_id: i64,
    militia_corp: i64,
    sys_id: u32,
    victim_cid: i64,
    victim_ship: i64,
) -> i64 {
    set_pilot_location(ctx, killer_sess, sys_id, None);
    let pos = DVec3::new(-350_000.0, 250_000.0, 0.0);
    let lp_before = ctx
        .db
        .with_snapshot(|s| s.characters[&killer_cid].loyalty_points.get(&militia_corp).copied().unwrap_or(0));
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ks) = s.items.get_mut(&killer_ship) {
            ks.location_id = i64::from(sys_id);
        }
        let vch = s.characters.get_mut(&victim_cid).unwrap();
        vch.solar_system_id = sys_id;
        vch.active_ship_id = victim_ship;
        s.items.insert(
            victim_ship,
            InventoryItemRecord {
                item_id: victim_ship,
                type_id: 621,
                owner_id: victim_cid,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    place_ship_ball(ctx, sys_id, killer_cid, killer_ship, 22_456, pos).await;
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let mut vcs = ctx.build_type_combat_state(621);
    vcs.shield_hp = 0.0;
    vcs.armor_hp = 0.0;
    vcs.structure_hp = 1.0;
    let _ = sys_h.remove_ball(victim_ship).await;
    sys_h
        .spawn_ball_with_combat(make_ball(victim_ship, 621, pos + DVec3::new(250.0, 0.0, 0.0)), vcs)
        .await
        .unwrap();
    sys_h
        .add_target_lock(
            killer_ship,
            TargetLock {
                target_id: victim_ship,
                lock_time_ms: 0.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: Some(50_000.0),
            },
        )
        .await
        .unwrap();
    rpc_call(ctx, killer_sess, "crimewatch", "SetSafetyLevel", &[PyRep::Integer(0)])
        .await
        .unwrap();
    rpc_call(ctx, killer_sess, "dogmaIM", "Activate", &[PyRep::Integer(gun_id), PyRep::Integer(victim_ship)])
        .await
        .expect("live militia PvP ship kill");
    let _ = rpc_call(ctx, killer_sess, "dogmaIM", "Deactivate", &[PyRep::Integer(gun_id)]).await;
    let lp_after = ctx
        .db
        .with_snapshot(|s| s.characters[&killer_cid].loyalty_points.get(&militia_corp).copied().unwrap_or(0));
    lp_after - lp_before
}

#[tokio::test]
async fn test_suppression_all_stages_1_to_5_and_drop_removal() {
    // parity: feat:fw.corruption-suppression-effects
    eve_testkit::attest(&["feat:fw.corruption-suppression-effects"]);

    let ctx = in_memory_ctx();
    let (_enemy_sys, front_sys, cmd_sys, _rear_sys, _hs_sys, _hs_st) =
        setup_warzone_chain_with_hs_station(&ctx);

    let (emp_cid, emp_ship, pir_cid, pir_ship, emp_vic_cid, pir_vic_cid) =
        (90_450_001, 100_450_001, 90_450_002, 100_450_002, 90_450_004, 90_450_005);

    let mut s_emp = register_online_pilot(&ctx, 84_501, "b40s_emp", "pw", emp_cid, 98_450_001, None, cmd_sys, emp_ship, 22_456, DVec3::new(900_000.0, 900_000.0, 0.0), 0).await;
    let mut s_pir = register_online_pilot(&ctx, 84_502, "b40s_pir", "pw", pir_cid, 98_450_002, None, front_sys, pir_ship, 22_456, DVec3::new(900_000.0, 900_000.0, 0.0), 0).await;
    let _s_evic = register_online_pilot(&ctx, 84_504, "b40s_evic", "pw", emp_vic_cid, 98_450_004, None, cmd_sys, 100_450_004, 621, DVec3::new(950_000.0, 950_000.0, 0.0), 0).await;
    let mut s_pvic = register_online_pilot(&ctx, 84_505, "b40s_pvic", "pw", pir_vic_cid, 98_450_005, None, cmd_sys, 100_450_005, 621, DVec3::new(950_000.0, 950_000.0, 0.0), 0).await;

    rpc_call(&ctx, &mut s_emp, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_001)]).await.unwrap();
    rpc_call(&ctx, &mut s_pir, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_010)]).await.unwrap();
    rpc_call(&ctx, &mut s_pvic, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_010)]).await.unwrap();
    ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&emp_vic_cid).unwrap().war_faction_id = Some(500_001));

    let (emp_gun, _eb, _er, emp_web, emp_scram, _emd) = fit_tactical_sabre(&ctx, emp_cid, emp_ship, 60_451_000);
    let (pir_gun, _pb, _pr, pir_web, _ps, _pmd) = fit_tactical_sabre(&ctx, pir_cid, pir_ship, 60_452_000);

    let caldari_militia_corp = ctx.sde.factions[&500_001].militia_corporation_id.unwrap() as i64;
    let guristas_militia_corp = ctx.sde.factions[&500_010].militia_corporation_id.unwrap() as i64;
    let mut rat_cands: Vec<(u64, u32)> = ctx
        .sde
        .types
        .values()
        .filter_map(|t| {
            let bty = t.default_attributes.get(&481).copied().unwrap_or(0.0);
            if t.category_id != 11 || bty < 5_000.0 {
                return None;
            }
            let hp = t.default_attributes.get(&263).copied().unwrap_or(0.0)
                + t.default_attributes.get(&265).copied().unwrap_or(0.0)
                + t.default_attributes.get(&9).copied().unwrap_or(0.0);
            (hp > 0.0 && hp <= 1_200.0).then_some((hp as u64, t.type_id))
        })
        .collect();
    rat_cands.sort_unstable();
    let rat_type_id = rat_cands[0].1;

    let _ = rpc_call(&ctx, &mut s_pir, "dungeonInstanceCacheMgr", "GetFactionWarfareInstances", &[]).await.unwrap();
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    fast_forward_secs(&ctx, FORECASTING_DURATION_SECS + 10);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

    let (base_bounty, base_sec_gain) =
        measure_live_npc_kill_bounty_and_sec_gain(&ctx, &mut s_emp, emp_cid, emp_ship, emp_gun, cmd_sys, rat_type_id).await;
    assert!(base_bounty > 0.0 && base_sec_gain > 0.0);
    let base_emp_lp =
        measure_live_militia_pvp_kill_lp(&ctx, &mut s_emp, emp_cid, emp_ship, emp_gun, caldari_militia_corp, cmd_sys, pir_vic_cid, 100_470_000).await;
    let base_pir_lp =
        measure_live_militia_pvp_kill_lp(&ctx, &mut s_pir, pir_cid, pir_ship, pir_gun, guristas_militia_corp, cmd_sys, emp_vic_cid, 100_470_100).await;
    assert!(base_emp_lp > 0 && base_pir_lp > 0);

    // 1. Step `cmd_sys` through Suppression Stages 1 -> 2 -> 3 -> 4 -> 5 via 5 live 60s Empire complex pulses.
    let mut expected_cmd_corr_pts = 0i64;
    let mut prev_enf_hp = 0.0f64;
    for s_stage in 1u8..=5 {
        pulse_complex_60s(&ctx, cmd_sys, &mut s_emp, emp_cid, emp_ship, &[(pir_ship, cmd_sys), (100_450_004, cmd_sys), (100_450_005, cmd_sys)]).await;
        let (_, cur_c_pts, cur_s_st, cur_s_pts) = local_campaign_stages(&ctx, &mut s_emp, cmd_sys).await.unwrap();
        assert_eq!((cur_s_st, cur_s_pts, cur_c_pts), (s_stage, i64::from(s_stage) * 200, expected_cmd_corr_pts));
        assert_eq!(lookup_modified_sec(&rpc_call(&ctx, &mut s_emp, "securityMgr", "get_modified_systems", &[]).await.unwrap(), cmd_sys), None);

        let (st_bounty, st_sec_gain) =
            measure_live_npc_kill_bounty_and_sec_gain(&ctx, &mut s_emp, emp_cid, emp_ship, emp_gun, cmd_sys, rat_type_id).await;
        let b_mult = 1.0 + 0.05 * f64::from(s_stage);
        assert!((st_bounty - base_bounty * b_mult).abs() < 1e-4);
        assert!((st_sec_gain - base_sec_gain * b_mult).abs() < 1e-9);

        let st_emp_lp =
            measure_live_militia_pvp_kill_lp(&ctx, &mut s_emp, emp_cid, emp_ship, emp_gun, caldari_militia_corp, cmd_sys, pir_vic_cid, 100_470_000 + i64::from(s_stage)).await;
        let lp_mult = if s_stage >= 2 { 1.0 + 0.05 * f64::from(s_stage - 1) } else { 1.0 };
        assert_eq!(st_emp_lp, ((base_emp_lp as f64) * lp_mult).round() as i64);
        if s_stage == 5 {
            let st_pir_lp =
                measure_live_militia_pvp_kill_lp(&ctx, &mut s_pir, pir_cid, pir_ship, pir_gun, guristas_militia_corp, cmd_sys, emp_vic_cid, 100_470_199).await;
            assert_eq!(st_pir_lp, base_pir_lp);
        }

        let enf_tid = with_combat_runtime(&ctx, |rt| {
            rt.npc_entities.values().find_map(|n| (n.solar_system_id == cmd_sys && ctx.sde.types.get(&n.type_id).is_some_and(|t| t.group_id == 4638)).then_some(n.type_id))
        });
        assert_eq!(enf_tid.is_some(), s_stage >= 3);
        if let Some(tid) = enf_tid {
            let hp = ctx.sde.types[&tid].default_attributes.get(&263).copied().unwrap_or(0.0)
                + ctx.sde.types[&tid].default_attributes.get(&265).copied().unwrap_or(0.0)
                + ctx.sde.types[&tid].default_attributes.get(&9).copied().unwrap_or(0.0);
            assert!(hp >= prev_enf_hp);
            prev_enf_hp = hp;
        }
        if s_stage == 3 {
            pulse_complex_60s(&ctx, cmd_sys, &mut s_pir, pir_cid, pir_ship, &[(emp_ship, cmd_sys), (100_450_004, cmd_sys), (100_450_005, cmd_sys)]).await;
            expected_cmd_corr_pts = 140;
            let (c_st3, c_pts3, _, _) = local_campaign_stages(&ctx, &mut s_emp, cmd_sys).await.unwrap();
            assert_eq!((c_st3, c_pts3), (1, 140), "Suppression Stage 3 must apply -30% corruption gain (200 -> 140)");
        }

        let cmd_h = ctx.systems.get_or_spawn(cmd_sys);
        set_pilot_location(&ctx, &mut s_emp, cmd_sys, None);
        set_pilot_location(&ctx, &mut s_pir, cmd_sys, None);
        place_ship_ball(&ctx, cmd_sys, emp_cid, emp_ship, 22_456, DVec3::new(-500_000.0, 0.0, 0.0)).await;
        place_ship_ball(&ctx, cmd_sys, pir_cid, pir_ship, 22_456, DVec3::new(-489_500.0, 0.0, 0.0)).await;
        cmd_h.add_target_lock(emp_ship, TargetLock { target_id: pir_ship, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0) }).await.unwrap();
        cmd_h.add_target_lock(pir_ship, TargetLock { target_id: emp_ship, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0) }).await.unwrap();
        let web_res = rpc_call(&ctx, &mut s_emp, "dogmaIM", "Activate", &[PyRep::Integer(emp_web), PyRep::Integer(pir_ship)]).await;
        if s_stage < 4 {
            assert_eq!(web_res.unwrap_err().code, "TargetOutOfRange");
        } else {
            web_res.expect("Empire Webifier succeeds at 10,500 m at Suppression Stage 4+");
            let _ = rpc_call(&ctx, &mut s_emp, "dogmaIM", "Deactivate", &[PyRep::Integer(emp_web)]).await;
            assert_eq!(rpc_call(&ctx, &mut s_pir, "dogmaIM", "Activate", &[PyRep::Integer(pir_web), PyRep::Integer(emp_ship)]).await.unwrap_err().code, "TargetOutOfRange");
        }
        place_ship_ball(&ctx, cmd_sys, pir_cid, pir_ship, 22_456, DVec3::new(-492_000.0, 0.0, 0.0)).await;
        cmd_h.add_target_lock(emp_ship, TargetLock { target_id: pir_ship, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0) }).await.unwrap();
        let scram_res = rpc_call(&ctx, &mut s_emp, "dogmaIM", "Activate", &[PyRep::Integer(emp_scram), PyRep::Integer(pir_ship)]).await;
        if s_stage < 4 {
            assert_eq!(scram_res.unwrap_err().code, "TargetOutOfRange");
        } else {
            scram_res.expect("Empire Warp Scrambler succeeds at 8,000 m at Suppression Stage 4+");
            assert!(cmd_h.ballpark.read().unwrap().is_ball_scrambled(pir_ship));
            let _ = rpc_call(&ctx, &mut s_emp, "dogmaIM", "Deactivate", &[PyRep::Integer(emp_scram)]).await;
        }

        let ci_sentry = with_combat_runtime(&ctx, |rt| {
            rt.npc_entities.values().find(|n| n.solar_system_id == cmd_sys && n.type_id == 79_507).map(|n| (n.ball_id, n.spawn_position))
        });
        assert_eq!(ci_sentry.is_some(), s_stage == 5);
        if let Some((_ci_bid, ci_pos)) = ci_sentry {
            ctx.db.with_mut_snapshot(|s| { s.sp6_lifecycle.ship_combat_hp.remove(&pir_ship); });
            place_ship_ball(&ctx, cmd_sys, pir_cid, pir_ship, 22_456, ci_pos + DVec3::new(8_000.0, 0.0, 0.0)).await;
            let (sig_before, sh_before) = {
                let bp = cmd_h.ballpark.read().unwrap();
                let cs = &bp.combat_states[&pir_ship];
                (cs.signature_radius, cs.shield_hp)
            };
            eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
            {
                let bp = cmd_h.ballpark.read().unwrap();
                let cs_after = &bp.combat_states[&pir_ship];
                assert!(cs_after.shield_hp < sh_before && cs_after.signature_radius > sig_before * 1.4 && cs_after.ecm_jammed_remaining_s > 0.0);
            }

            let PyRep::List(vis_pre) = rpc_call(&ctx, &mut s_emp, "insurgencySolarsystem", "GetAllVisibleCampaigns", &[]).await.unwrap() else { panic!() };
            let sys_count_pre = match vis_pre[0].dict_get("systems").unwrap() { PyRep::List(l) => l.len(), _ => 0 };
            for _ in 0..6 {
                pulse_complex_60s(&ctx, cmd_sys, &mut s_pir, pir_cid, pir_ship, &[(emp_ship, cmd_sys), (100_450_004, cmd_sys), (100_450_005, cmd_sys)]).await;
            }
            let (cap_c_st, cap_c_pts, _, _) = local_campaign_stages(&ctx, &mut s_emp, cmd_sys).await.unwrap();
            assert_eq!((cap_c_st, cap_c_pts), (4, 800));
            let PyRep::List(vis_post) = rpc_call(&ctx, &mut s_emp, "insurgencySolarsystem", "GetAllVisibleCampaigns", &[]).await.unwrap() else { panic!() };
            let sys_count_post = match vis_post[0].dict_get("systems").unwrap() { PyRep::List(l) => l.len(), _ => 0 };
            assert_eq!(sys_count_post, sys_count_pre);
        }
    }

    // 2. Non-retroactive Stage 5 rule: `front_sys` at Corruption 5 -> Suppression 5 preserves LAWLESS Corruption 5!
    for _ in 0..5 {
        pulse_complex_60s(&ctx, front_sys, &mut s_pir, pir_cid, pir_ship, &[(emp_ship, cmd_sys)]).await;
    }
    for _ in 0..5 {
        pulse_complex_60s(&ctx, front_sys, &mut s_emp, emp_cid, emp_ship, &[(pir_ship, front_sys)]).await;
    }
    assert_eq!(local_campaign_stages(&ctx, &mut s_emp, front_sys).await, Some((5, 1_000, 5, 1_000)));
    assert_eq!(lookup_modified_sec(&rpc_call(&ctx, &mut s_emp, "securityMgr", "get_modified_systems", &[]).await.unwrap(), front_sys), Some(0.0));

    // 3. Live Suppression Stage Drop (`5 -> 0`) via natural campaign expiry on `tick_background_cluster`:
    fast_forward_secs(&ctx, ACTIVE_DURATION_SECS + WINDDOWN_DURATION_SECS + 120);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

    assert_eq!(local_campaign_stages(&ctx, &mut s_emp, cmd_sys).await, None);
    let (post_bounty, post_sec) = measure_live_npc_kill_bounty_and_sec_gain(&ctx, &mut s_emp, emp_cid, emp_ship, emp_gun, cmd_sys, rat_type_id).await;
    assert!((post_bounty - base_bounty).abs() < 1e-4 && (post_sec - base_sec_gain).abs() < 1e-9);
    let post_lp = measure_live_militia_pvp_kill_lp(&ctx, &mut s_emp, emp_cid, emp_ship, emp_gun, caldari_militia_corp, cmd_sys, pir_vic_cid, 100_495_002).await;
    assert_eq!(post_lp, base_emp_lp);
    assert!(!with_combat_runtime(&ctx, |rt| rt.npc_entities.values().any(|n| ctx.sde.types.get(&n.type_id).is_some_and(|t| t.group_id == 4638))));

    let cmd_h = ctx.systems.get_or_spawn(cmd_sys);
    set_pilot_location(&ctx, &mut s_emp, cmd_sys, None);
    place_ship_ball(&ctx, cmd_sys, emp_cid, emp_ship, 22_456, DVec3::new(-500_000.0, 0.0, 0.0)).await;
    place_ship_ball(&ctx, cmd_sys, pir_cid, pir_ship, 22_456, DVec3::new(-489_500.0, 0.0, 0.0)).await;
    cmd_h.add_target_lock(emp_ship, TargetLock { target_id: pir_ship, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0) }).await.unwrap();
    assert_eq!(rpc_call(&ctx, &mut s_emp, "dogmaIM", "Activate", &[PyRep::Integer(emp_web), PyRep::Integer(pir_ship)]).await.unwrap_err().code, "TargetOutOfRange");
    place_ship_ball(&ctx, cmd_sys, pir_cid, pir_ship, 22_456, DVec3::new(-492_000.0, 0.0, 0.0)).await;
    cmd_h.add_target_lock(emp_ship, TargetLock { target_id: pir_ship, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0) }).await.unwrap();
    assert_eq!(rpc_call(&ctx, &mut s_emp, "dogmaIM", "Activate", &[PyRep::Integer(emp_scram), PyRep::Integer(pir_ship)]).await.unwrap_err().code, "TargetOutOfRange");

    // Persistent `79507` Counter-Insurgency Sentry Gun survives campaign end until destroyed via live combat:
    let (ci_bid, ci_pos) = with_combat_runtime(&ctx, |rt| {
        rt.npc_entities.values().find(|n| n.solar_system_id == cmd_sys && n.type_id == 79_507).map(|n| (n.ball_id, n.spawn_position))
    }).expect("79507 sentry persists after campaign end");
    set_pilot_location(&ctx, &mut s_pir, cmd_sys, None);
    ctx.db.with_mut_snapshot(|s| {
        s.items.retain(|_, i| i.location_id != pir_ship);
        s.sp6_lifecycle.ship_combat_hp.remove(&pir_ship);
    });
    fit_siege_artillery(&ctx, pir_cid, pir_ship, 60_499_000);
    ctx.invalidate_ship_fitting(pir_ship);
    place_ship_ball(&ctx, cmd_sys, pir_cid, pir_ship, 19_722, ci_pos + DVec3::new(5_000.0, 0.0, 0.0)).await;
    cmd_h.add_target_lock(pir_ship, TargetLock { target_id: ci_bid, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0) }).await.unwrap();
    for siege_gun in [60_499_000i64, 60_499_002, 60_499_004] {
        rpc_call(&ctx, &mut s_pir, "dogmaIM", "Activate", &[PyRep::Integer(siege_gun), PyRep::Integer(ci_bid)]).await
            .expect("Activate XL Siege Artillery on persistent 79507 sentry");
    }
    for _ in 0..80 {
        cmd_h.step_manual(12.0).await.unwrap();
        if cmd_h.get_ball_snapshot(ci_bid).await.unwrap().is_none() {
            break;
        }
    }
    assert!(cmd_h.get_ball_snapshot(ci_bid).await.unwrap().is_none());
}

#[tokio::test]
async fn test_security_mgr_and_insurgency_rpc_negative_auth_and_validation() {
    // parity: feat:fw.corruption-suppression-effects
    eve_testkit::attest(&["feat:fw.corruption-suppression-effects"]);
    let ctx = in_memory_ctx();
    let kw = HashMap::new();

    // 1. Unauthenticated (`Unauthorized`) and pre-character-selection (`CharacterNotSelected`) sessions rejected across all touched RPC handlers:
    let mut unauth = SessionState::new(84_901);
    ctx.db.provision_player_account("b40_nochar", "pw");
    let mut no_char = SessionState::new(84_903);
    crate::common::login_pilot(&ctx, &mut no_char, "b40_nochar", "pw").await;
    for (svc, method, args) in [
        ("securityMgr", "get_modified_systems", vec![]),
        ("insurgencySolarsystem", "GetAllVisibleCampaigns", vec![]),
        ("insurgencySolarsystem", "GetLocalCampaignClientSnapshot", vec![]),
        ("dungeonInstanceCacheMgr", "GetPirateInsurgencyInstances", vec![]),
        ("dungeonInstanceCacheMgr", "GetPirateInsurgencyIceHeistInstances", vec![]),
    ] {
        let u_err = ctx.dispatch_rpc(&mut unauth, svc, None, method, &args, &kw, None).await.unwrap_err();
        assert_eq!(u_err.code, "Unauthorized", "{svc}.{method} must reject unauthenticated session");
        let c_err = ctx.dispatch_rpc(&mut no_char, svc, None, method, &args, &kw, None).await.unwrap_err();
        assert_eq!(c_err.code, "CharacterNotSelected", "{svc}.{method} must require selected character");
    }

    // 2. Authenticated character negative validation (`NoSolarSystem`, `InvalidSolarSystem`, cross-service unknown method):
    let mut orphan = no_char.clone();
    orphan.character_id = Some(90_499_999);
    orphan.solar_system_id = None;
    let no_sys_err = ctx.dispatch_rpc(&mut orphan, "insurgencySolarsystem", None, "GetLocalCampaignClientSnapshot", &[], &kw, None).await.unwrap_err();
    assert_eq!(no_sys_err.code, "NoSolarSystem");

    let mut sess = register_online_pilot(&ctx, 84_902, "b40_neg", "pw", 90_499_001, 98_499_001, None, 30_000_142, 100_499_001, 587, DVec3::ZERO, 0).await;
    let wrong_svc = ctx.dispatch_rpc(&mut sess, "securityMgr", None, "GetAllVisibleCampaigns", &[], &kw, None).await.unwrap_err();
    assert_eq!(wrong_svc.code, "UnimplementedRPC");
    for bad_arg in [
        PyRep::Integer(-1),
        PyRep::Integer(0),
        PyRep::Integer(99_999_999),
        PyRep::String("not_a_system_id".into()),
    ] {
        for method in ["GetPirateInsurgencyInstances", "GetPirateInsurgencyIceHeistInstances"] {
            let err = ctx
                .dispatch_rpc(&mut sess, "dungeonInstanceCacheMgr", None, method, std::slice::from_ref(&bad_arg), &kw, None)
                .await
                .unwrap_err();
            assert_eq!(err.code, "InvalidSolarSystem", "{method} must reject {bad_arg:?}"); // rpc-lint: allow UserError code, not RPC method
        }
    }
}

#[tokio::test]
async fn test_specialized_npc_destroy_grants_sec_status_with_suppression_multiplier() {
    // parity: feat:fw.corruption-suppression-effects
    eve_testkit::attest(&["feat:fw.corruption-suppression-effects"]);
    let ctx = in_memory_ctx();
    let lowsec_sys = 30_002_537u32; // Tama
    let char_id = 90_499_111i64;
    let _sess = register_online_pilot(
        &ctx,
        84_911,
        "b40_spec_sec",
        "pw",
        char_id,
        98_499_111,
        None,
        lowsec_sys,
        100_499_111,
        587,
        DVec3::ZERO,
        0,
    )
    .await;
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&char_id) {
            ch.security_status = 0.0;
        }
    });

    // 1. Baseline specialized NPC kill through destroy_specialized_npc_and_queue_bounty
    let npc1 = spawn_specialized_npc_entity(&ctx, lowsec_sys, 16_934, "Belt", 1, DVec3::new(5_000.0, 0.0, 0.0)).await;
    let (bounty1, _, _) = eve_server::services::combat_crimewatch::combat_npc_ai::destroy_specialized_npc_and_queue_bounty(
        &ctx,
        lowsec_sys,
        npc1,
        char_id,
    )
    .await;
    assert!(bounty1 > 0);
    let delta_base = ctx.db.with_snapshot(|s| s.characters.get(&char_id).map(|c| c.security_status).unwrap_or(0.0));
    assert!(delta_base > 0.0, "Specialized NPC kill must grant positive security status (got {delta_base})");
    let tx_count = with_combat_runtime(&ctx, |rt| {
        rt.sec_status_transactions
            .get(&char_id)
            .map(|v| v.iter().filter(|t| t.reason == "NPC Pirate Bounty").count())
            .unwrap_or(0)
    });
    assert_eq!(tx_count, 1, "Must record 'NPC Pirate Bounty' SecStatusTransaction");

    // 2. Suppression Stage 5 (1.25x sec-status multiplier)
    let now = ctx.now_unix_secs();
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&char_id) {
            ch.security_status = 0.0;
        }
        s.insurgency_runtime.campaigns.insert(
            77_911,
            eve_db::PersistedInsurgencyCampaign {
                campaign_id: 77_911,
                pirate_faction_id: 500_011,
                target_faction_id: 500_001,
                fob_solar_system_id: lowsec_sys,
                systems: vec![lowsec_sys],
                created_at_unix: now.saturating_sub(20),
                forecasting_end_unix: now.saturating_sub(10),
                active_end_unix: now + 3_000,
                winddown_end_unix: now + 3_600,
                corruption_points: HashMap::new(),
                suppression_points: HashMap::from([(lowsec_sys, 1_000)]),
            },
        );
    });
    let npc2 = spawn_specialized_npc_entity(&ctx, lowsec_sys, 16_934, "Belt", 1, DVec3::new(6_000.0, 0.0, 0.0)).await;
    let _ = eve_server::services::combat_crimewatch::combat_npc_ai::destroy_specialized_npc_and_queue_bounty(
        &ctx,
        lowsec_sys,
        npc2,
        char_id,
    )
    .await;
    let delta_supp = ctx.db.with_snapshot(|s| s.characters.get(&char_id).map(|c| c.security_status).unwrap_or(0.0));
    assert!(
        (delta_supp - delta_base * 1.25).abs() < 1e-9,
        "Suppression Stage 5 must apply 1.25x sec-status multiplier (base={delta_base}, supp={delta_supp})"
    );
}
