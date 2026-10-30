//! Integration tests for Batch 23 remediation (Abyssal exit suspect flag, war interference, command burst filter, CONCORD & jump-in).

use crate::plan_c7_crimewatch_closure::{dict_get, item_rec, setup_pilot_in_space};
use eve_db::{InventoryFlag, WarRecord};
use eve_proto::PyRep;
use eve_server::combat::crimewatch::concord_delay_for_security;
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::{
    evaluate_and_record_pvp_aggression, evaluate_and_record_remote_assistance,
    with_mut_combat_runtime, SafetyLevel,
};
use eve_server::services::pve_abyssal_lifecycle::evacuate_abyssal_pockets_for_downtime;
use eve_server::services::tactical_modules::activate_command_burst;
use glam::DVec3;
use std::collections::HashMap;

const AMARR_SYS: u32 = 30_002_187; // 1.0
const TAMA_SYS: u32 = 30_002_813; // 0.3

#[tokio::test]
async fn test_b23_abyssal_suspect_flag_on_exit_not_entry() {
    // parity: feat:crimewatch.abyssal-suspect-flag-tier
    let ctx = ServerContext::new();
    let char_id: i64 = 95_230_101;
    let ship_id: i64 = 105_230_101;
    let sys_07 = ctx
        .sde
        .solar_systems
        .iter()
        .find_map(|(&sid, s)| (0.65..0.75).contains(&s.security_status).then_some(sid))
        .expect("0.7 high-sec system in SDE");
    let mut sess = setup_pilot_in_space(&ctx, 952_401, char_id, 98_230_101, ship_id, 621, sys_07, DVec3::ZERO).await;

    let t6_filament_tid = ctx
        .sde
        .types
        .iter()
        .find_map(|(&tid, t)| {
            (t.group_id == 1979 && t.default_attributes.get(&2761).copied().unwrap_or(0.0) >= 6.0)
                .then_some(tid)
        })
        .expect("T6 Abyssal filament in SDE");

    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(
            700_101,
            item_rec(700_101, t6_filament_tid, char_id, ship_id, InventoryFlag::Cargo, 1, false),
        );
    });

    let kw = HashMap::new();
    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "abyssal_content_manager",
            None,
            "ActivateAbyssalKey",
            &[PyRep::Integer(700_101)],
            &kw,
            None,
        )
        .await
        .expect("ActivateAbyssalKey");
    assert_eq!(dict_get(&res, "suspectFlagOnExit").and_then(|v| v.as_bool()), Some(true));

    let now = ctx.now_unix();
    assert!(
        !ctx.crimewatch.write().unwrap().get_or_create(char_id).is_suspect(now),
        "T4+ Abyssal filament must NOT set suspect flag on entry"
    );

    let evacuated = evacuate_abyssal_pockets_for_downtime(&ctx);
    assert_eq!(evacuated, 1);
    assert!(
        ctx.crimewatch.write().unwrap().get_or_create(char_id).is_suspect(now),
        "T4+ Abyssal filament MUST set suspect flag on exit from 0.5-0.7 high-sec"
    );
}

#[tokio::test]
async fn test_b23_war_interference_and_command_burst_filtering() {
    // parity: feat:crimewatch.third-party-assistance-legality, feat:crimewatch.safety-setting-action-block
    let ctx = ServerContext::new();
    let corp_a: i64 = 98_230_201;
    let corp_b: i64 = 98_230_202;
    let corp_c: i64 = 98_230_203;

    let pilot_a: i64 = 95_230_201;
    let ship_a: i64 = 105_230_201;
    let corpmate_a: i64 = 95_230_202;
    let ship_mate: i64 = 105_230_202;
    let neutral_logi: i64 = 95_230_203;
    let ship_logi: i64 = 105_230_203;

    let mut sess_a = setup_pilot_in_space(&ctx, 952_411, pilot_a, corp_a, ship_a, 621, AMARR_SYS, DVec3::ZERO).await;
    let mut sess_mate = setup_pilot_in_space(&ctx, 952_412, corpmate_a, corp_a, ship_mate, 621, AMARR_SYS, DVec3::new(500.0, 0.0, 0.0)).await;
    let mut sess_logi = setup_pilot_in_space(&ctx, 952_413, neutral_logi, corp_c, ship_logi, 621, AMARR_SYS, DVec3::new(1000.0, 0.0, 0.0)).await;

    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|s| {
        s.wars.insert(
            990_001,
            WarRecord {
                war_id: 990_001,
                aggressor_corp_id: corp_a,
                defender_corp_id: corp_b,
                mutual: true,
                started_unix: now.saturating_sub(100_000),
                active: true,
            },
        );
    });

    // Corpmate assisting war participant is legal
    evaluate_and_record_remote_assistance(&ctx, corpmate_a, ship_mate, AMARR_SYS, ship_a)
        .expect("corpmate assistance is legal");
    assert!(!ctx.crimewatch.write().unwrap().get_or_create(corpmate_a).is_criminal(now));

    // Neutral third party assisting war participant in high-sec triggers Criminal + CONCORD + notification
    with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(neutral_logi, SafetyLevel::Red);
    });
    let _ = ctx.drain_notifications_for_char(neutral_logi);
    evaluate_and_record_remote_assistance(&ctx, neutral_logi, ship_logi, AMARR_SYS, ship_a)
        .expect("Red safety allows war interference");
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        let st = cw.get_or_create(neutral_logi);
        assert!(st.is_criminal(now));
        assert!(st.pending_concord_arrival.is_some());
    }
    let notes = ctx.drain_notifications_for_char(neutral_logi);
    assert!(notes.iter().any(|n| n.event == "OnCriminalTimerUpdate"));

    // Low-sec war interference triggers Suspect (not Criminal)
    let logi_low: i64 = 95_230_204;
    let ship_logi_low: i64 = 105_230_204;
    let _ = setup_pilot_in_space(&ctx, 952_414, logi_low, corp_c, ship_logi_low, 621, TAMA_SYS, DVec3::ZERO).await;
    with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(logi_low, SafetyLevel::Red);
    });
    evaluate_and_record_remote_assistance(&ctx, logi_low, ship_logi_low, TAMA_SYS, ship_a)
        .expect("low-sec war interference");
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        let st = cw.get_or_create(logi_low);
        assert!(st.is_suspect(now));
        assert!(!st.is_criminal(now));
    }

    // Command burst in high-sec: corpmate receives burst, neutral booster excludes war-engaged pilot
    let kw = HashMap::new();
    let _ = ctx.dispatch_rpc(&mut sess_a, "fleetMgr", None, "CreateFleet", &[], &kw, None).await.expect("CreateFleet");
    let fleet_id = sess_a.fleet_id.expect("fleet_id");
    let _ = ctx.dispatch_rpc(&mut sess_a, "fleetObjectHandler", None, "Invite", &[PyRep::Integer(corpmate_a), PyRep::Integer(0), PyRep::Integer(0), PyRep::Integer(0)], &kw, None).await.expect("Invite mate");
    let _ = ctx.dispatch_rpc(&mut sess_mate, "fleetMgr", None, "AcceptInvite", &[PyRep::Integer(fleet_id)], &kw, None).await.expect("AcceptInvite mate");
    let _ = ctx.dispatch_rpc(&mut sess_a, "fleetObjectHandler", None, "Invite", &[PyRep::Integer(neutral_logi), PyRep::Integer(0), PyRep::Integer(0), PyRep::Integer(0)], &kw, None).await.expect("Invite logi");
    let _ = ctx.dispatch_rpc(&mut sess_logi, "fleetMgr", None, "AcceptInvite", &[PyRep::Integer(fleet_id)], &kw, None).await.expect("AcceptInvite logi");

    let (burst_mod_tid, burst_charge_tid) = ctx
        .sde
        .types
        .iter()
        .find_map(|(&tid, t)| {
            if t.group_id != 1770 || t.name.contains(" II") || t.name.contains("Presidential") {
                return None;
            }
            let cg = t.default_attributes.get(&604).copied()? as u32;
            let c_tid = ctx.sde.types.iter().find_map(|(&ctid, ct)| (ct.group_id == cg).then_some(ctid))?;
            Some((tid, c_tid))
        })
        .expect("Tech I Command Burst module and charge in SDE");
    let burst_mod_id: i64 = 700_201;
    let burst_charge_id: i64 = 700_202;
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(burst_mod_id, item_rec(burst_mod_id, burst_mod_tid, pilot_a, ship_a, InventoryFlag::HiSlot0, 1, true));
        s.items.insert(burst_charge_id, item_rec(burst_charge_id, burst_charge_tid, pilot_a, ship_a, InventoryFlag::HiSlot0, 10, false));
        s.module_online_states.insert(burst_mod_id, true);
    });
    ctx.invalidate_ship_fitting(ship_a);
    let sys_h = ctx.systems.get_or_spawn(AMARR_SYS);
    let fit_a = ctx.get_or_rebuild_ship_fitting(pilot_a, ship_a).expect("fit_a");
    let mod_type = ctx.sde.types.get(&burst_mod_tid).expect("burst_mod_tid");
    let mod_state = eve_dogma::DogmaItemState::from_sde(burst_mod_id, burst_mod_tid, &ctx.sde);
    let burst_res = activate_command_burst(&ctx, &sys_h, pilot_a, ship_a, AMARR_SYS, burst_mod_id, mod_type, &mod_state, Some(burst_charge_tid), &fit_a)
        .await
        .expect("corpmate burst");
    let affected: Vec<i64> = match dict_get(&burst_res, "affectedMemberIDs") {
        Some(PyRep::List(l)) => l.iter().filter_map(|v| v.as_i64()).collect(),
        _ => Vec::new(),
    };
    assert!(affected.contains(&corpmate_a), "corpmate in same war must receive command burst");
    assert!(!affected.contains(&neutral_logi), "neutral pilot who is criminal must be excluded");

    // Clear neutral_logi's criminal flag so only war-interference exclusion is tested
    ctx.crimewatch.write().unwrap().states.remove(&neutral_logi);
    let burst_mod_id2: i64 = 700_203;
    let burst_charge_id2: i64 = 700_204;
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(burst_mod_id2, item_rec(burst_mod_id2, burst_mod_tid, neutral_logi, ship_logi, InventoryFlag::HiSlot0, 1, true));
        s.items.insert(burst_charge_id2, item_rec(burst_charge_id2, burst_charge_tid, neutral_logi, ship_logi, InventoryFlag::HiSlot0, 10, false));
        s.module_online_states.insert(burst_mod_id2, true);
    });
    ctx.invalidate_ship_fitting(ship_logi);
    let fit_logi = ctx.get_or_rebuild_ship_fitting(neutral_logi, ship_logi).expect("fit_logi");
    let mod_state2 = eve_dogma::DogmaItemState::from_sde(burst_mod_id2, burst_mod_tid, &ctx.sde);
    let burst_res2 = activate_command_burst(&ctx, &sys_h, neutral_logi, ship_logi, AMARR_SYS, burst_mod_id2, mod_type, &mod_state2, Some(burst_charge_tid), &fit_logi)
        .await
        .expect("neutral burst");
    let affected2: Vec<i64> = match dict_get(&burst_res2, "affectedMemberIDs") {
        Some(PyRep::List(l)) => l.iter().filter_map(|v| v.as_i64()).collect(),
        _ => Vec::new(),
    };
    assert!(!affected2.contains(&pilot_a), "war-engaged pilot must be excluded from neutral booster's command burst in high-sec");
}

#[tokio::test]
async fn test_b23_le_interference_concord_and_jump_into_highsec_concord() {
    // parity: feat:crimewatch.highsec-criminal-restrictions, feat:crimewatch.lowsec-suspect-vs-criminal, feat:crimewatch.third-party-assistance-legality
    let ctx = ServerContext::new();
    let p1: i64 = 95_230_301;
    let s1: i64 = 105_230_301;
    let p2: i64 = 95_230_302;
    let s2: i64 = 105_230_302;
    let logi: i64 = 95_230_303;
    let s_logi: i64 = 105_230_303;

    let mut sess1 = setup_pilot_in_space(&ctx, 952_421, p1, 98_230_301, s1, 621, AMARR_SYS, DVec3::ZERO).await;
    let mut sess2 = setup_pilot_in_space(&ctx, 952_422, p2, 98_230_302, s2, 621, AMARR_SYS, DVec3::new(500.0, 0.0, 0.0)).await;
    let _ = setup_pilot_in_space(&ctx, 952_423, logi, 98_230_303, s_logi, 621, AMARR_SYS, DVec3::new(1000.0, 0.0, 0.0)).await;

    let kw = HashMap::new();
    let _ = ctx.dispatch_rpc(&mut sess1, "crimewatch", None, "StartDuelChallenge", &[PyRep::Integer(p2)], &kw, None).await.expect("StartDuelChallenge");
    let _ = ctx.dispatch_rpc(&mut sess2, "crimewatch", None, "RespondToDuelChallenge", &[PyRep::Integer(p1), PyRep::Bool(true)], &kw, None).await.expect("RespondToDuelChallenge");
    evaluate_and_record_pvp_aggression(&ctx, p1, s1, AMARR_SYS, s2, false).expect("duel legal");

    with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(logi, SafetyLevel::Red);
    });
    let _ = ctx.drain_notifications_for_char(logi);
    evaluate_and_record_remote_assistance(&ctx, logi, s_logi, AMARR_SYS, s1).expect("LE interference");
    let now = ctx.now_unix();
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        let st = cw.get_or_create(logi);
        assert!(st.is_criminal(now));
        assert!(st.pending_concord_arrival.is_some(), "LE interference in high-sec must schedule CONCORD");
    }
    let notes = ctx.drain_notifications_for_char(logi);
    assert!(notes.iter().any(|n| n.event == "OnCriminalTimerUpdate"), "LE interference must emit timer notifications");

    // Stargate jump from low-sec into high-sec with active criminal timer schedules CONCORD
    let (gate_id, from_sys, to_sys, to_sec) = ctx
        .sde
        .jumps
        .iter()
        .find_map(|(&gid, j)| {
            let f_sec = ctx.sde.solar_systems.get(&j.from_solar_system_id)?.security_status;
            let t_sec = ctx.sde.solar_systems.get(&j.to_solar_system_id)?.security_status;
            (f_sec > 0.0 && f_sec < 0.45 && t_sec >= 0.45).then_some((gid, j.from_solar_system_id, j.to_solar_system_id, t_sec))
        })
        .expect("low-sec to high-sec stargate in SDE");

    let jumper: i64 = 95_230_304;
    let jumper_ship: i64 = 105_230_304;
    let gate_pos = ctx.sde.jumps[&gate_id].position;
    let mut j_sess = setup_pilot_in_space(&ctx, 952_424, jumper, 98_230_304, jumper_ship, 621, from_sys, gate_pos).await;
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        let st = cw.get_or_create(jumper);
        st.criminal_flag_until_unix = now + 900;
        st.pending_concord_arrival = None;
    }
    eve_server::services::space_destiny::clear_session_change_timer(&ctx, jumper);
    let beyonce = ctx.bind_object(&mut j_sess, "beyonce", from_sys as i64).expect("bind beyonce");
    ctx.dispatch_rpc(&mut j_sess, "beyonce", Some(beyonce), "CmdStargateJump", &[PyRep::Integer(gate_id as i64)], &kw, None)
        .await
        .expect("CmdStargateJump into high-sec");

    let mut cw = ctx.crimewatch.write().unwrap();
    let st = cw.get_or_create(jumper);
    let (arr_sys, arr_unix, arr_ship, _) = st.pending_concord_arrival.expect("CONCORD scheduled on jump into high-sec");
    assert_eq!(arr_sys, to_sys);
    assert_eq!(arr_ship, jumper_ship);
    let expected_delay = concord_delay_for_security(to_sec).ceil() as u64;
    assert!(arr_unix >= now + expected_delay && arr_unix <= now + expected_delay + 2);
}
