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
        .keys()
        .copied()
        .filter(|&tid| {
            eve_server::services::pve_special_sites_abyssal::classify_abyssal_filament(tid)
                .is_some_and(|(tier, _)| tier == 6)
        })
        .min()
        .unwrap_or(56_139);

    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(
            700_101,
            item_rec(700_101, t6_filament_tid, char_id, ship_id, InventoryFlag::Cargo, 1, false),
        );
    });

    let kw = HashMap::new();
    let deploy_res = ctx
        .dispatch_rpc(
            &mut sess,
            "abyssalMgr",
            None,
            "AbyssalEntranceDeployment",
            &[PyRep::Integer(700_101)],
            &kw,
            None,
        )
        .await
        .expect("AbyssalEntranceDeployment");
    let trace_ball_id = dict_get(&deploy_res, "traceBallID")
        .and_then(|v| v.as_i64())
        .expect("traceBallID");
    let _enter_res = ctx
        .dispatch_rpc(
            &mut sess,
            "abyssalMgr",
            None,
            "AbyssalEntranceGateActivation",
            &[PyRep::Integer(trace_ball_id)],
            &kw,
            None,
        )
        .await
        .expect("AbyssalEntranceGateActivation");

    let now = ctx.now_unix();
    assert!(
        !ctx.crimewatch.write().unwrap().get_or_create(char_id).is_suspect(now),
        "T6 Abyssal filament MUST NOT set suspect flag on entry in 0.7 high-sec"
    );

    ctx.advance_time_seconds(120);
    let exit_now = ctx.now_unix();
    let evacuated = evacuate_abyssal_pockets_for_downtime(&ctx);
    assert_eq!(evacuated, 1);
    assert!(
        ctx.crimewatch.write().unwrap().get_or_create(char_id).is_suspect(exit_now),
        "T6 Abyssal filament MUST set suspect flag on exit from 0.7 high-sec"
    );
}

#[tokio::test]
async fn test_b23_war_interference_and_command_burst_filtering() {
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

    // Negative: Neutral third party assisting an IDLE war member (no PvP timer) -> no flag
    with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(neutral_logi, SafetyLevel::Green);
    });
    evaluate_and_record_remote_assistance(&ctx, neutral_logi, ship_logi, AMARR_SYS, ship_a)
        .expect("assisting idle war member with no PvP timer is legal even on Green safety");
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        let st = cw.get_or_create(neutral_logi);
        assert!(!st.is_criminal(now));
        assert!(!st.is_suspect(now));
    }

    // Activate PvP timer on pilot_a (war engagement)
    ctx.crimewatch.write().unwrap().get_or_create(pilot_a).pvp_timer_until_unix = now + 900;

    // Neutral third party assisting war participant with active PvP timer in high-sec triggers Criminal + CONCORD + notification
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
            if t.group_id != 1770 || t.name.contains(" II") || t.name.contains("Presidential") || t.name.contains("Expedition") || t.name.contains("Mining Foreman") {
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
        if let Some(sh) = s.items.get_mut(&ship_a) {
            sh.type_id = 37480;
        }
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
    assert!(
        ctx.crimewatch.read().unwrap().states.get(&pilot_a).is_some_and(|s| s.weapon_timer_until_unix > ctx.now_unix()),
        "Command burst must apply mandatory 60s Weapons Timer"
    );

    // Clear neutral_logi's criminal flag so only war-interference exclusion is tested
    ctx.crimewatch.write().unwrap().states.remove(&neutral_logi);
    let burst_mod_id2: i64 = 700_203;
    let burst_charge_id2: i64 = 700_204;
    ctx.db.with_mut_snapshot(|s| {
        if let Some(sh) = s.items.get_mut(&ship_logi) {
            sh.type_id = 37480;
        }
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

#[tokio::test]
async fn test_b23_skinr_plex_pricing_and_sec_office_tag_remediation() {
    use crate::quasar_progression_complete::{build_sess, quasar_rpc};
    use eve_db::{IskCents, TrainedSkillRecord};
    use eve_server::quasar_gateway::STATUS_OK;

    let ctx = ServerContext::new();
    let cid: u32 = 95_230_401;
    let corp_id: u32 = 98_230_401;
    ctx.db.with_mut_snapshot(|s| {
        let mut ch = s.characters[&90_000_001].clone();
        ch.character_id = cid as i64;
        ch.corporation_id = corp_id as i64;
        ch.plex_balance = 100;
        ch.balance = IskCents::from_isk(50_000_000);
        s.characters.insert(cid as i64, ch);
    });
    let mut sess = build_sess(952_430, cid, corp_id);
    sess.user_id = Some(952_430);
    sess.role = eve_machonet::service_roles::ROLE_PLAYER_DEFAULT;

    // 1. R-B23-01: SKINR InitiateRequest rejects empty payload and debits whole PLEX (17 PLEX for Tier 1, not 1,700)
    let empty_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(empty_rsp.status_code, 400, "Empty InitiateRequest must be rejected");

    use eve_proto::quasar::{encode_bytes_field, encode_string_field, encode_varint_field};
    let mut layout = Vec::new();
    let mut r_tid = Vec::new();
    encode_varint_field(1, 587, &mut r_tid);
    encode_bytes_field(2, &r_tid, &mut layout);
    encode_varint_field(3, 1, &mut layout);
    let mut init_req = Vec::new();
    encode_string_field(1, "B23 Test SKIN", &mut init_req);
    encode_string_field(2, "B23 Line", &mut init_req);
    encode_bytes_field(3, &layout, &mut init_req);
    encode_varint_field(5, 1, &mut init_req);

    let init_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateRequest",
        init_req.clone(),
    )
    .await;
    assert_eq!(init_rsp.status_code, STATUS_OK);
    let plex_after = ctx.db.with_snapshot(|s| s.characters[&(cid as i64)].plex_balance);
    assert_eq!(plex_after, 83, "Tier 1 SKINR sequencing must debit 17 whole PLEX (100 - 17 = 83)");

    // With skill 81044 level 5 (10% price discount): 1700 * 90 / 100 = 1530 cents -> 16 whole PLEX
    ctx.db.with_mut_snapshot(|s| {
        s.skills.entry(cid as i64).or_default().insert(
            81044,
            TrainedSkillRecord { type_id: 81044, level: 5, skill_points: 256_000.0 },
        );
    });
    let init_rsp2 = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateRequest",
        init_req,
    )
    .await;
    assert_eq!(init_rsp2.status_code, STATUS_OK);
    let plex_after2 = ctx.db.with_snapshot(|s| s.characters[&(cid as i64)].plex_balance);
    assert_eq!(plex_after2, 67, "10% discounted Tier 1 SKINR sequencing must debit 16 whole PLEX (83 - 16 = 67)");

    // 2. R-B23-04: ApplyTagsForSecStatus rejects High-Sec Jita & qty <= 0, accepts SDE 33141 at Security Office, charges ISK, records journal, crosses 0.0, and preserves excess tags
    let kw = HashMap::new();
    sess.station_id = Some(60_003_760);
    let jita_err = ctx
        .dispatch_rpc(&mut sess, "crimewatch", None, "ApplyTagsForSecStatus", &[PyRep::Real(0.4), PyRep::Dict(vec![(PyRep::Integer(33141), PyRep::Integer(1))])], &kw, None)
        .await
        .expect_err("High-Sec Jita must be rejected for Security Office tag turn-in");
    assert_eq!(jita_err.code, "SecurityOfficeRequired");

    let sec_office_station = ctx
        .sde
        .stations
        .iter()
        .find_map(|(&sid, st)| {
            let sec = ctx.sde.solar_systems.get(&st.solar_system_id)?.security_status;
            ((0.0 < sec && sec < 0.45)
                && eve_server::services::station_structure::npc_station_offers_service(&ctx.sde, sid, 27))
            .then_some(sid)
        })
        .expect("Low-sec station with CONCORD/DED Security Office service (27) in SDE");
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&(cid as i64)) {
            ch.station_id = Some(sec_office_station);
        }
    });
    sess.station_id = Some(sec_office_station);

    let zero_err = ctx
        .dispatch_rpc(&mut sess, "crimewatch", None, "ApplyTagsForSecStatus", &[PyRep::Real(0.4), PyRep::Dict(vec![(PyRep::Integer(33141), PyRep::Integer(0))])], &kw, None)
        .await
        .expect_err("Zero quantity must be rejected");
    assert_eq!(zero_err.code, "InvalidQuantity");

    crate::common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let uid = sess.user_id.unwrap();
    ctx.db.with_mut_snapshot(|s| {
        let ch = s.characters.get_mut(&(cid as i64)).unwrap();
        ch.user_id = uid;
        ch.security_status = -0.1;
        ch.balance = eve_db::IskCents::from_isk(25_000_000);
        s.items.insert(700_998, item_rec(700_998, 33141, cid as i64, sec_office_station, InventoryFlag::Hangar, 1, false));
        s.items.insert(700_999, item_rec(700_999, 33141, cid as i64, sec_office_station, InventoryFlag::Hangar, 2, false));
    });
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(cid as i64)], &kw, None).await.unwrap();
    sess.character_id = Some(cid as i64);
    sess.station_id = Some(sec_office_station);
    let low_err = ctx
        .dispatch_rpc(&mut sess, "crimewatch", None, "ApplyTagsForSecStatus", &[PyRep::Real(-0.5), PyRep::Dict(vec![(PyRep::Integer(33141), PyRep::Integer(1))])], &kw, None)
        .await
        .expect_err("targetSecStatus <= cur_sec must be rejected");
    assert_eq!(low_err.code, "InvalidTargetSecurityStatus");
    let _ = ctx.drain_notifications_for_char(cid as i64);
    ctx.dispatch_rpc(&mut sess, "crimewatch", None, "ApplyTagsForSecStatus", &[PyRep::Real(0.4), PyRep::Dict(vec![(PyRep::Integer(33141), PyRep::Integer(3))])], &kw, None)
        .await
        .expect("ApplyTagsForSecStatus with SDE Clone Soldier Negotiator Tag (33141)");

    let notifs = ctx.drain_notifications_for_char(cid as i64);
    let item_change = notifs.iter().find(|n| n.event == "OnItemChange").expect("3-arg OnItemChange");
    assert_eq!(item_change.args.len(), 3, "OnItemChange must have 3 positional args");
    assert_eq!(item_change.args[2].as_i64(), Some(sec_office_station));

    ctx.db.with_snapshot(|s| {
        let ch = &s.characters[&(cid as i64)];
        assert!((ch.security_status - 0.0).abs() < 1e-6, "Clone Soldier tags cap security status at 0.0, got {}", ch.security_status);
        assert_eq!(ch.balance, eve_db::IskCents::from_isk(15_000_000), "Must debit 10M ISK for 1 tag");
        assert_eq!(s.items.get(&700_999).map(|i| i.quantity), Some(2), "Must preserve 2 excess tags");
        assert!(
            s.wallet_journal.iter().any(|e| e.character_id == (cid as i64) && e.entry_type == "SecurityTagProcessing"),
            "Must record SecurityTagProcessing wallet journal entry"
        );
    });
}

