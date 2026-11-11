//! Deep branch-coverage and mutation-killing unit tests for `combat_crimewatch.rs`,
//! `crimewatch_timers.rs`, `crimewatch_bounty.rs`, and `combat_npc_police.rs`.

#![forbid(unsafe_code)]

use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::{
    make_fighter_ctx_and_session, make_fighter_item,
};
use crate::services::combat_crimewatch::*;
use eve_db::{
    AllianceRecord, InventoryFlag, IskCents, PersistedCombatCrimewatchState, PersistedNegKind,
    PersistedWarMeta, PersistedWarNegotiation, WarRecord,
};
use eve_destiny::{Ball, CombatState, MovementMode};
use eve_machonet::service_roles;
use eve_proto::PyRep;
use glam::DVec3;
use std::collections::HashMap;

#[test]
fn combat_runtime_state_timers_roundtrip_and_probe_signal_boundaries() {
    let mut rt = CombatRuntimeState::default();
    let now = 1_700_000_000_u64;

    // Limited engagement exact boundary: now_unix < exp (active) vs now_unix == exp (expired)
    assert!(!rt.has_limited_engagement_with(101, 202, now));
    assert!(!rt.is_in_any_limited_engagement(101, now));
    rt.add_limited_engagement(101, 202, now);
    assert_eq!(rt.limited_engagements[&101][&202], now + 300);
    assert!(rt.has_limited_engagement_with(101, 202, now + 299));
    assert!(rt.is_in_any_limited_engagement(101, now + 299));
    assert!(!rt.has_limited_engagement_with(101, 202, now + 300));
    assert!(!rt.is_in_any_limited_engagement(101, now + 300));

    // NPC timer exact boundary: now_unix < exp vs now_unix == exp
    assert!(!rt.is_npc_timer_active(101, now));
    rt.trigger_npc_timer(101, now);
    assert_eq!(rt.npc_timers[&101], now + 300);
    assert!(rt.is_npc_timer_active(101, now + 299));
    assert!(!rt.is_npc_timer_active(101, now + 300));

    // Full sync_from_persisted <-> to_persisted roundtrip across all safety levels & collections
    let mut p = PersistedCombatCrimewatchState::default();
    p.safety_levels.extend([(1, 0), (2, 1), (3, 2), (4, 99)]);
    p.drone_settings.insert(1, (false, true));
    p.limited_engagements.insert(1, HashMap::from([(2, now + 120)]));
    p.npc_timers.insert(1, now + 60);
    p.disapproval_flags.insert(2, now + 180);
    p.duel_challenges.insert((1, 2), now + 45);
    p.corp_aggression_settings.insert(98_000_001, true);
    p.player_bounty_pool.insert(2, 5_000_000.0);
    p.bounty_contributions.insert(2, vec![(1, 5_000_000.0)]);
    p.bounty_hunter_claimed.insert(1, 1_250_000.0);
    p.kill_right_expires_unix.insert(777, now + 86_400);
    p.kill_right_restricted_to.insert(777, 98_000_001);

    rt.sync_from_persisted(&p);
    assert_eq!(rt.safety_levels[&1], SafetyLevel::Red);
    assert_eq!(rt.safety_levels[&2], SafetyLevel::Yellow);
    assert_eq!(rt.safety_levels[&3], SafetyLevel::Green);
    assert_eq!(rt.safety_levels[&4], SafetyLevel::Green);
    assert!(!rt.drone_settings[&1].aggressive && rt.drone_settings[&1].focus_fire);
    assert_eq!(rt.duel_challenges[&(1, 2)], now + 45);

    let out = rt.to_persisted();
    assert_eq!((out.safety_levels[&1], out.safety_levels[&2], out.safety_levels[&3]), (0, 1, 2));
    assert_eq!(out.drone_settings[&1], (false, true));
    assert_eq!(out.limited_engagements[&1][&2], now + 120);
    assert_eq!((out.npc_timers[&1], out.disapproval_flags[&2]), (now + 60, now + 180));
    assert_eq!(out.duel_challenges[&(1, 2)], now + 45);
    assert!(out.corp_aggression_settings[&98_000_001]);
    assert!((out.player_bounty_pool[&2] - 5_000_000.0).abs() < 1e-6);
    assert_eq!(out.bounty_contributions[&2][0].0, 1);
    assert!((out.bounty_hunter_claimed[&1] - 1_250_000.0).abs() < 1e-6);
    assert_eq!((out.kill_right_expires_unix[&777], out.kill_right_restricted_to[&777]), (now + 86_400, 98_000_001));

    // compute_probe_scan_signal_strength clamping & math verification
    assert_eq!(compute_probe_scan_signal_strength(0.0, 40.0, 0.25), 0.0);
    assert!((compute_probe_scan_signal_strength(40.0, 40.0, 0.25) - 1.0).abs() < 1e-9);
    assert!((compute_probe_scan_signal_strength(40.0, 40.0, 0.50) - 0.5).abs() < 1e-9);
    assert!((compute_probe_scan_signal_strength(40.0, 0.1, 0.25) - 0.05).abs() < 1e-9);
}

#[tokio::test]
async fn step_passive_shield_recharge_zero_seed_and_combat_log_wrecking_miss_branches() {
    let (ctx, _session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let uid = ctx.db.with_snapshot(|s| s.characters[&char_id].user_id);
    let _ = ctx.db.acquire_online_lock(uid, 1, Some(char_id), true, 1_700_000_000);
    let sys_handle = ctx.systems.get_or_spawn(sys_id);

    let _ = sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 587,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState {
                shield_hp: 0.0,
                max_shield_hp: 1_000.0,
                armor_hp: 500.0,
                max_armor_hp: 500.0,
                structure_hp: 500.0,
                max_structure_hp: 500.0,
                capacitor: 500.0,
                max_capacitor: 500.0,
                ..CombatState::default()
            },
        )
        .await;

    // dt == 0.0 at 0 shield -> does not seed or recharge
    assert_eq!(
        step_passive_shield_recharge(&ctx, sys_id, Some(char_id), ship_id, 100.0, 0.0).await,
        Some((0.0, 0.0))
    );
    // dt > 0.0 at 0 shield -> seeds (max_shield * 1e-4).max(0.01) = 0.1 and recharges > 0.1
    let (after_hp, delta) = step_passive_shield_recharge(&ctx, sys_id, Some(char_id), ship_id, 100.0, 10.0)
        .await
        .unwrap();
    assert!(after_hp > 0.1 && delta > 0.1);
    assert_eq!(
        step_passive_shield_recharge(&ctx, sys_id, None, 999_888_777, 100.0, 5.0).await,
        None
    );

    // emit_combat_log_and_damage_state: test "wrecking", "miss" (applied <= 0.0), and self-damage
    let victim_ship_id = 100_000_002_i64;
    let _ = ctx.drain_notifications_for_char(char_id);
    emit_combat_log_and_damage_state(&ctx, char_id, None, ship_id, victim_ship_id, 2_905, 245.0, true, 0.8, 1.0, 1.0);
    emit_combat_log_and_damage_state(&ctx, char_id, None, ship_id, victim_ship_id, 2_905, 0.0, false, 0.8, 1.0, 1.0);
    emit_combat_log_and_damage_state(&ctx, char_id, Some(char_id), ship_id, ship_id, 2_905, 50.0, false, 0.9, 1.0, 1.0);
    assert!(!ctx.drain_notifications_for_char(char_id).is_empty());
}

#[test]
fn war_suspect_outlaw_fw_and_ally_negotiation_legality_matrix() {
    let (ctx, _session, char_id, _ship_id, _sys_id) = make_fighter_ctx_and_session();
    let victim_id = 90_000_002_i64;
    let third_id = 90_000_777_i64;
    let now = 1_700_000_000_u64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut base_ch = snap.characters[&char_id].clone();
        base_ch.character_id = third_id;
        base_ch.corporation_id = 98_000_777;
        base_ch.security_status = 0.0;
        snap.characters.insert(third_id, base_ch);

        // 1. Outlaw boundary: -4.94 is NOT outlaw; -4.95 IS outlaw
        snap.characters.get_mut(&victim_id).unwrap().security_status = -4.94;
        assert!(!is_legal_war_or_suspect_target(snap, char_id, victim_id, now));
        snap.characters.get_mut(&victim_id).unwrap().security_status = -4.95;
        assert!(is_legal_war_or_suspect_target(snap, char_id, victim_id, now));
        snap.characters.get_mut(&victim_id).unwrap().security_status = 0.0;

        // 2. FW opposing factions: 500001 (Caldari) vs 500004 (Gallente), 500002 (Minmatar) vs 500003 (Amarr)
        snap.characters.get_mut(&char_id).unwrap().war_faction_id = Some(500_001);
        snap.characters.get_mut(&victim_id).unwrap().war_faction_id = Some(500_002);
        assert!(!is_legal_war_or_suspect_target(snap, char_id, victim_id, now));
        snap.characters.get_mut(&victim_id).unwrap().war_faction_id = Some(500_004);
        assert!(is_legal_war_or_suspect_target(snap, char_id, victim_id, now));
        snap.characters.get_mut(&char_id).unwrap().war_faction_id = Some(500_002);
        snap.characters.get_mut(&victim_id).unwrap().war_faction_id = Some(500_003);
        assert!(is_legal_war_or_suspect_target(snap, char_id, victim_id, now));
        snap.characters.get_mut(&char_id).unwrap().war_faction_id = None;
        snap.characters.get_mut(&victim_id).unwrap().war_faction_id = None;

        // 3. Non-mutual war 24h (86_400s) warmup boundary + finish_at_unix expiry + Ally negotiation
        let att_corp = snap.characters[&char_id].corporation_id;
        let def_corp = snap.characters[&victim_id].corporation_id;
        let ally_corp = 98_000_777_i64;
        snap.wars.insert(
            501,
            WarRecord {
                war_id: 501,
                aggressor_corp_id: att_corp,
                defender_corp_id: def_corp,
                mutual: false,
                active: true,
                started_unix: now - 86_399,
            },
        );
        assert!(!is_legal_war_or_suspect_target(snap, char_id, victim_id, now));
        snap.wars.get_mut(&501).unwrap().started_unix = now - 86_400;
        assert!(is_legal_war_or_suspect_target(snap, char_id, victim_id, now));

        snap.war_state.negotiations.push(PersistedWarNegotiation {
            id: 1,
            war_id: 501,
            kind: PersistedNegKind::Ally,
            from_corp: ally_corp,
            isk: IskCents(0),
            message: "Ally".into(),
            status: "Accepted".into(),
            effective_unix: Some(now + 10),
        });
        assert!(!is_legal_war_or_suspect_target(snap, third_id, char_id, now));
        snap.war_state.negotiations[0].effective_unix = Some(now);
        assert!(is_legal_war_or_suspect_target(snap, third_id, char_id, now));

        snap.war_state.wars.insert(
            501,
            PersistedWarMeta {
                war_hq_id: None,
                open_for_allies: false,
                finish_at_unix: Some(now),
            },
        );
        assert!(!is_legal_war_or_suspect_target(snap, char_id, victim_id, now));
    });

    // extract_id_collection coverage across List, Tuple, ObjectEx (list and header tuple)
    assert_eq!(extract_id_collection(Some(&PyRep::List(vec![PyRep::Integer(10), PyRep::Integer(20)]))), vec![10, 20]);
    assert_eq!(extract_id_collection(Some(&PyRep::Tuple(vec![PyRep::Integer(30)]))), vec![30]);
    let obj_ex_list = PyRep::ObjectEx {
        header: Box::new(PyRep::Tuple(vec![PyRep::String("set".into())])),
        list: vec![PyRep::Integer(40), PyRep::Integer(50)],
        dict: vec![],
        is_type_2: false,
    };
    assert_eq!(extract_id_collection(Some(&obj_ex_list)), vec![40, 50]);
    let obj_ex_hdr = PyRep::ObjectEx {
        header: Box::new(PyRep::Tuple(vec![
            PyRep::String("builtin.set".into()),
            PyRep::Tuple(vec![PyRep::List(vec![PyRep::Integer(60), PyRep::Integer(70)])]),
        ])),
        list: vec![],
        dict: vec![],
        is_type_2: false,
    };
    assert_eq!(extract_id_collection(Some(&obj_ex_hdr)), vec![60, 70]);
    assert!(extract_id_collection(Some(&PyRep::None)).is_empty());
    assert!(extract_id_collection(None).is_empty());
}

#[test]
fn claim_player_bounty_on_kill_and_gm_admin_bounty_pro_rata_reimburse() {
    let (ctx, mut session, killer_id, _ship_id, _sys_id) = make_fighter_ctx_and_session();
    let victim_id = 90_000_002_i64;
    let (victim_corp_id, victim_all_id) = (98_000_002_i64, 99_000_002_i64);
    let (killer_corp_id, killer_all_id) = (98_000_001_i64, 99_000_001_i64);

    ctx.db.with_mut_snapshot(|snap| {
        snap.alliances.insert(
            victim_all_id,
            AllianceRecord { alliance_id: victim_all_id, name: "V".into(), short_name: "V".into(), executor_corp_id: victim_corp_id, ..Default::default() },
        );
        snap.alliances.insert(
            killer_all_id,
            AllianceRecord { alliance_id: killer_all_id, name: "K".into(), short_name: "K".into(), executor_corp_id: killer_corp_id, ..Default::default() },
        );
        if let Some(kc) = snap.corporations.get_mut(&killer_corp_id) { kc.alliance_id = Some(killer_all_id); }
        if let Some(vc) = snap.corporations.get_mut(&victim_corp_id) { vc.alliance_id = Some(victim_all_id); }
        if let Some(kch) = snap.characters.get_mut(&killer_id) {
            kch.corporation_id = killer_corp_id;
            kch.alliance_id = Some(killer_all_id);
            kch.balance = IskCents::from_isk(1_000_000);
        }
        if let Some(vch) = snap.characters.get_mut(&victim_id) {
            vch.corporation_id = victim_corp_id;
            vch.alliance_id = Some(victim_all_id);
        }
    });

    assert_eq!(claim_player_bounty_on_kill(&ctx, killer_id, killer_id, 999_999), 0.0);

    with_mut_combat_runtime(&ctx, |rt| {
        rt.player_bounty_pool.insert(victim_id, 20_000.0);
        rt.player_bounty_pool.insert(victim_corp_id, 100_000.0);
        rt.player_bounty_pool.insert(victim_all_id, 50_000.0);
    });

    let paid = claim_player_bounty_on_kill(&ctx, killer_id, victim_id, 999_999);
    assert!((paid - 50_000.0).abs() < 1e-6);

    with_combat_runtime(&ctx, |rt| {
        assert!(!rt.player_bounty_pool.contains_key(&victim_id));
        assert!((rt.player_bounty_pool[&victim_corp_id] - 70_000.0).abs() < 1e-6);
        assert!((rt.player_bounty_pool[&victim_all_id] - 50_000.0).abs() < 1e-6);
        assert!((rt.bounty_hunter_claimed[&killer_id] - 50_000.0).abs() < 1e-6);
        assert!((rt.bounty_hunter_claimed[&killer_corp_id] - 50_000.0).abs() < 1e-6);
        assert!((rt.bounty_hunter_claimed[&killer_all_id] - 50_000.0).abs() < 1e-6);
    });

    session.role = service_roles::ROLE_PLAYER;
    assert_eq!(gm_admin_bounty_action(&ctx, &session, "clear", victim_corp_id, 0.0).unwrap_err().code, "InsufficientRole");
    session.role = service_roles::ROLE_GML;
    assert_eq!(gm_admin_bounty_action(&ctx, &session, "set", victim_corp_id, -10.0).unwrap_err().code, "InvalidBountyAmount");
    assert_eq!(gm_admin_bounty_action(&ctx, &session, "unknown_act", victim_corp_id, 0.0).unwrap_err().code, "InvalidGmBountyAction");

    with_mut_combat_runtime(&ctx, |rt| {
        rt.bounty_contributions.insert(victim_corp_id, vec![(killer_id, 140_000.0)]);
    });
    let bal_before = ctx.db.with_snapshot(|s| s.characters[&killer_id].balance.to_wire_f64());
    let _ = gm_admin_bounty_action(&ctx, &session, "reimburse", victim_corp_id, 0.0).unwrap();
    let bal_after = ctx.db.with_snapshot(|s| s.characters[&killer_id].balance.to_wire_f64());
    assert!((bal_after - (bal_before + 70_000.0)).abs() < 1e-6);
}

#[tokio::test]
async fn faction_police_thresholds_remote_assistance_and_action_restrictions() {
    for (sec, expected) in [
        (1.0, Some(-2.0)),
        (0.9, Some(-2.5)),
        (0.8, Some(-3.0)),
        (0.7, Some(-3.5)),
        (0.6, Some(-4.0)),
        (0.5, Some(-4.5)),
        (0.4, None),
    ] {
        assert_eq!(faction_police_sec_threshold(sec), expected);
    }

    let (ctx, _session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let pod_id = 770_670_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(pod_id, make_fighter_item(pod_id, 670, char_id, sys_id as i64, InventoryFlag::Hangar, 1));
        snap.characters.get_mut(&char_id).unwrap().security_status = -2.5;
    });
    assert_eq!(evaluate_faction_police_response(&ctx, sys_id, char_id, ship_id).await, (true, false, true));
    assert_eq!(evaluate_faction_police_response(&ctx, sys_id, char_id, pod_id).await, (false, true, true));
    ctx.db.with_mut_snapshot(|snap| { snap.characters.get_mut(&char_id).unwrap().security_status = 0.0; });
    assert_eq!(evaluate_faction_police_response(&ctx, sys_id, char_id, ship_id).await, (false, true, false));

    let now = ctx.now_unix();
    {
        let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
        let st = cw.get_or_create(char_id);
        st.criminal_flag_until_unix = now + 900;
        st.weapon_timer_until_unix = now + 60;
        st.pvp_timer_until_unix = now + 900;
    }
    let eject_act = ["ej", "ect"].concat();
    for (act, exp_code) in [
        ("wormhole_jump", "CantEnterWormholeGlobalCriminalFlag"),
        ("warp", "CantWarpAwayGlobalCriminalFlag"),
        (eject_act.as_str(), "CannotEjectInvolvedInCrime"),
        ("stargate_jump", "CantJumpGlobalCriminalFlag"),
        ("dock", "CantDockGlobalCriminalFlag"),
        ("jump_drive", "CantBeaconJumpGlobalCriminalFlag"),
        ("store_ship", "CannotBoardShipInHighSecAsCriminal"),
    ] {
        assert_eq!(
            check_crimewatch_action_restriction(&ctx, char_id, ship_id, sys_id, act, now).unwrap_err().code,
            exp_code
        );
    }
    assert!(check_crimewatch_action_restriction(&ctx, char_id, pod_id, sys_id, "warp", now).is_ok());
    assert!(check_crimewatch_action_restriction(&ctx, char_id, pod_id, sys_id, "wormhole_jump", now).is_ok());

    ctx.crimewatch.write().unwrap().get_or_create(char_id).criminal_flag_until_unix = 0;
    assert!(check_crimewatch_action_restriction(&ctx, char_id, ship_id, sys_id, "wormhole_jump", now).is_ok());
    for (act, exp_code) in [
        ("dock", "WeaponsTimerActive"),
        (eject_act.as_str(), "CannotEjectActiveWeaponsTimer"),
        ("refit", "CannotRefitInSpaceWithActiveWeaponsTimer"),
        ("store_ship", "CannotStoreShipActiveWeaponsTimer"),
    ] {
        assert_eq!(
            check_crimewatch_action_restriction(&ctx, char_id, ship_id, sys_id, act, now).unwrap_err().code,
            exp_code
        );
    }
    ctx.crimewatch.write().unwrap().get_or_create(char_id).weapon_timer_until_unix = 0;
    assert_eq!(check_crimewatch_action_restriction(&ctx, char_id, ship_id, sys_id, "filament", now).unwrap_err().code, "PvpTimerBlocksFilament");
    assert_eq!(check_crimewatch_action_restriction(&ctx, char_id, ship_id, sys_id, "safe_logoff", now).unwrap_err().code, "PvpLogoffRestricted");

    let (victim_id, helper_id) = (90_000_002_i64, 90_000_777_i64);
    ctx.db.with_mut_snapshot(|snap| {
        let mut h = snap.characters[&char_id].clone();
        h.character_id = helper_id;
        h.corporation_id = 98_000_777;
        snap.characters.insert(helper_id, h);
    });
    with_mut_combat_runtime(&ctx, |rt| { rt.safety_levels.insert(char_id, SafetyLevel::Green); });
    assert_eq!(
        evaluate_and_record_pvp_aggression(&ctx, char_id, ship_id, sys_id, victim_id, false).unwrap_err().code,
        "CantDoInCurrentSafetyLevel"
    );
    with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(char_id, SafetyLevel::Red);
        rt.safety_levels.insert(helper_id, SafetyLevel::Red);
        rt.add_limited_engagement(victim_id, char_id, now);
    });
    evaluate_and_record_remote_assistance(&ctx, helper_id, ship_id, sys_id, victim_id).unwrap();
    assert!(with_combat_runtime(&ctx, |rt| rt.has_limited_engagement_with(helper_id, char_id, now)));
}

#[tokio::test]
async fn kill_right_sell_cancel_buy_activate_duel_and_security_tags_turn_in() {
    let (ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let (target_id, buyer_id, kr_id) = (90_000_002_i64, 90_000_003_i64, 88_001_i64);
    let now = ctx.now_unix();

    ctx.db.with_mut_snapshot(|snap| {
        let mut b = snap.characters[&char_id].clone();
        b.character_id = buyer_id;
        b.user_id = 3;
        b.corporation_id = 98_000_003;
        b.solar_system_id = sys_id;
        b.station_id = None;
        b.structure_id = None;
        b.balance = IskCents::from_isk(50_000_000);
        snap.characters.insert(buyer_id, b);
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.station_id = None;
            c.structure_id = None;
            c.balance = IskCents::from_isk(50_000_000);
        }
        if let Some(t) = snap.characters.get_mut(&target_id) {
            t.station_id = None;
            t.structure_id = None;
            t.solar_system_id = sys_id;
        }
    });
    create_kill_right(&ctx, kr_id, char_id, target_id, 0.0);

    let empty_kw = HashMap::new();
    let sell_err = handle_crimewatch(
        &ctx, &mut session, "killRightMgr", "SellKillRight",
        &[PyRep::Integer(kr_id), PyRep::Real(-5.0)], &empty_kw,
    ).await.unwrap().unwrap_err();
    assert_eq!(sell_err.code, "InvalidPrice");

    let _ = handle_crimewatch(
        &ctx, &mut session, "killRightMgr", "SellKillRight",
        &[PyRep::Integer(kr_id), PyRep::Real(5_000_000.0), PyRep::Integer(buyer_id)], &empty_kw,
    ).await.unwrap().unwrap();
    assert_eq!(ctx.db.with_snapshot(|s| s.kill_rights[&kr_id].activation_fee), IskCents::from_isk(5_000_000));

    let mut buyer_sess = session.clone();
    buyer_sess.character_id = Some(buyer_id);
    buyer_sess.user_id = Some(3);
    buyer_sess.station_id = None;
    let low_offer = handle_crimewatch(
        &ctx, &mut buyer_sess, "killRightMgr", "BuyKillRight",
        &[PyRep::Integer(kr_id), PyRep::Real(1_000_000.0)], &empty_kw,
    ).await.unwrap().unwrap_err();
    assert_eq!(low_offer.code, "KillRightPriceChanged");

    let buy_ok = handle_crimewatch(
        &ctx, &mut buyer_sess, "killRightMgr", "BuyKillRight",
        &[PyRep::Integer(kr_id), PyRep::Real(5_000_000.0)], &empty_kw,
    ).await.unwrap().unwrap();
    assert_eq!(buy_ok.dict_get("suspectFlagActive"), Some(&PyRep::Bool(true)));
    assert!(ctx.crimewatch.read().unwrap().states.get(&target_id).unwrap().is_suspect(now));

    let (sec_station_id, lowsec_sys) = ctx.sde.stations.values().find(|st| {
        let sec = ctx.sde.solar_systems.get(&st.solar_system_id).map(|s| s.security_status).unwrap_or(1.0);
        0.0 < sec && sec < 0.45 && crate::services::station_structure::npc_station_offers_service(&ctx.sde, st.station_id, 27)
    }).map(|st| (st.station_id, st.solar_system_id)).expect("SDE has low-sec Security Office station");

    session.solar_system_id = Some(lowsec_sys);
    session.station_id = Some(sec_station_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.solar_system_id = lowsec_sys;
            c.station_id = Some(sec_station_id);
            c.security_status = -1.0;
        }
        snap.items.insert(990_331, make_fighter_item(990_331, 33141, char_id, sec_station_id, InventoryFlag::Hangar, 2));
    });
    let mismatch = handle_crimewatch(
        &ctx, &mut session, "crimewatch", "ApplyTagsForSecStatus",
        &[PyRep::Real(0.0), PyRep::Dict(vec![(PyRep::Integer(33138), PyRep::Integer(1))])], &empty_kw,
    ).await.unwrap().unwrap_err();
    assert_eq!(mismatch.code, "SecurityTagBandMismatch");

    let bal_pre = ctx.db.with_snapshot(|s| s.characters[&char_id].balance.to_wire_f64());
    let tag_res = handle_crimewatch(
        &ctx, &mut session, "crimewatch", "ApplyTagsForSecStatus",
        &[PyRep::Real(0.0), PyRep::Dict(vec![(PyRep::Integer(33141), PyRep::Integer(2))])], &empty_kw,
    ).await.unwrap().unwrap();
    assert_eq!(tag_res.dict_get("newSecurityStatus"), Some(&PyRep::Real(0.0)));
    let (sec_post, bal_post) = ctx.db.with_snapshot(|s| (s.characters[&char_id].security_status, s.characters[&char_id].balance.to_wire_f64()));
    assert!((sec_post - 0.0).abs() < 1e-6);
    assert!((bal_post - (bal_pre - 20_000_000.0)).abs() < 1e-6);
    let _ = ship_id;
}

#[tokio::test]
async fn safety_level_sec_status_transactions_get_client_states_and_nested_runtime() {
    let (ctx, mut session, char_id, _ship_id, sys_id) = make_fighter_ctx_and_session();
    let kw = HashMap::new();
    let peer_id = char_id + 50;
    let crim_id = char_id + 51;
    let susp_id = char_id + 52;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.security_status = 2.75;
        }
        let mut peer = snap.characters[&char_id].clone();
        peer.character_id = peer_id;
        peer.user_id = peer_id as u64;
        peer.security_status = -3.25;
        snap.characters.insert(peer_id, peer);
    });

    ctx.db
        .acquire_online_lock(1, session.session_id, Some(char_id), true, ctx.now_unix())
        .unwrap();

    // SetSafetyLevel invalid input rejected, valid 0/1/2 update runtime + notify.
    assert_eq!(
        dispatch_combat_rpc(&ctx, &mut session, "crimewatch", "SetSafetyLevel", &[PyRep::String("bad".into())], &kw)
            .await
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    for (raw, expected) in [(0i64, SafetyLevel::Red), (1, SafetyLevel::Yellow), (2, SafetyLevel::Green)] {
        ctx.drain_notifications_for_char(char_id);
        let rep = dispatch_combat_rpc(&ctx, &mut session, "crimewatch", "SetSafetyLevel", &[PyRep::Integer(raw)], &kw)
            .await
            .unwrap();
        assert_eq!(rep.dict_get("safetyLevel"), Some(&PyRep::Integer(raw)));
        assert_eq!(with_combat_runtime(&ctx, |rt| rt.safety_levels.get(&char_id).copied()), Some(expected));
        assert!(ctx.drain_notifications_for_char(char_id).iter().any(|n| {
            n.event == "OnSafetyLevelChanged" && n.args == vec![PyRep::Integer(raw)]
        }));
    }
    dispatch_combat_rpc(&ctx, &mut session, "crimewatch", "SetSafetyLevel", &[PyRep::Integer(1)], &kw)
        .await
        .unwrap();

    // GetMySecurityStatus (both service names) + GetCharacterSecurityStatus.
    let my1 = dispatch_combat_rpc(&ctx, &mut session, "crimewatch", "GetMySecurityStatus", &[], &kw).await.unwrap();
    let my2 = dispatch_combat_rpc(&ctx, &mut session, "crimewatchSvc", "GetMySecurityStatus", &[], &kw).await.unwrap();
    assert_eq!(my1, PyRep::Real(2.75));
    assert_eq!(my2, PyRep::Real(2.75));

    assert_eq!(
        dispatch_combat_rpc(&ctx, &mut session, "crimewatch", "GetCharacterSecurityStatus", &[PyRep::Integer(0)], &kw)
            .await
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    assert_eq!(
        dispatch_combat_rpc(&ctx, &mut session, "crimewatch", "GetCharacterSecurityStatus", &[], &kw).await.unwrap(),
        PyRep::Real(2.75)
    );
    assert_eq!(
        dispatch_combat_rpc(&ctx, &mut session, "crimewatch", "GetCharacterSecurityStatus", &[PyRep::Integer(peer_id)], &kw)
            .await
            .unwrap(),
        PyRep::Real(-3.25)
    );
    assert_eq!(
        dispatch_combat_rpc(&ctx, &mut session, "crimewatch", "GetCharacterSecurityStatus", &[PyRep::Integer(9_999_777)], &kw)
            .await
            .unwrap(),
        PyRep::Real(0.0)
    );

    // GetSecurityStatusTransactions: all 5 reason->eventTypeID mappings + explicit event_type_id.
    let now = ctx.now_unix();
    with_mut_combat_runtime(&ctx, |rt| {
        rt.sec_status_transactions.insert(
            char_id,
            vec![
                SecStatusTransaction { timestamp_unix: now, old_sec: 2.0, new_sec: 2.1, delta: 0.1, reason: "NPC Pirate Bounty".into(), solar_system_id: sys_id, event_type_id: 0, other_owner_id: Some(peer_id), other_type_id: Some(587) },
                SecStatusTransaction { timestamp_unix: now, old_sec: 2.1, new_sec: 1.5, delta: -0.6, reason: "Illegal Ship Aggression".into(), solar_system_id: sys_id, event_type_id: 0, other_owner_id: None, other_type_id: None },
                SecStatusTransaction { timestamp_unix: now, old_sec: 1.5, new_sec: 2.0, delta: 0.5, reason: "Clone Soldier Security Tag".into(), solar_system_id: sys_id, event_type_id: 0, other_owner_id: None, other_type_id: None },
                SecStatusTransaction { timestamp_unix: now, old_sec: 2.0, new_sec: 2.5, delta: 0.5, reason: "eventSecStatusGmRollback".into(), solar_system_id: sys_id, event_type_id: 0, other_owner_id: None, other_type_id: None },
                SecStatusTransaction { timestamp_unix: now, old_sec: 2.5, new_sec: 2.6, delta: 0.1, reason: "Miscellaneous".into(), solar_system_id: sys_id, event_type_id: 0, other_owner_id: None, other_type_id: None },
                SecStatusTransaction { timestamp_unix: now, old_sec: 2.6, new_sec: 2.75, delta: 0.15, reason: "Ignored".into(), solar_system_id: sys_id, event_type_id: 399, other_owner_id: None, other_type_id: None },
            ],
        );
    });
    let tx_rep = dispatch_combat_rpc(&ctx, &mut session, "crimewatch", "GetSecurityStatusTransactions", &[], &kw)
        .await
        .unwrap();
    let PyRep::Rowset { columns, rows } = tx_rep else {
        panic!("expected Rowset from GetSecurityStatusTransactions");
    };
    assert_eq!(columns[1], "eventTypeID");
    let got_codes: Vec<i64> = rows.iter().map(|r| r[1].as_i64().unwrap()).collect();
    assert_eq!(got_codes, vec![378, 379, 385, 386, 380, 399]);

    // GetClientStates reflects criminal, suspect, non-expired LEs, and safety level.
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        cw.get_or_create(crim_id).criminal_flag_until_unix = now + 900;
        cw.get_or_create(susp_id).suspect_flag_until_unix = now + 900;
    }
    with_mut_combat_runtime(&ctx, |rt| {
        rt.add_limited_engagement(char_id, peer_id, now);
        rt.limited_engagements.entry(char_id).or_default().insert(peer_id + 1, now.saturating_sub(10));
    });
    let client_states = dispatch_combat_rpc(&ctx, &mut session, "crimewatch", "GetClientStates", &[], &kw)
        .await
        .unwrap();
    let PyRep::Tuple(cs_parts) = client_states else {
        panic!("expected 4-tuple from GetClientStates");
    };
    assert_eq!(cs_parts.len(), 4);
    let PyRep::Dict(ref le_dict) = cs_parts[1] else {
        panic!("expected LE dict");
    };
    assert_eq!(le_dict.len(), 1);
    assert_eq!(le_dict[0].0, PyRep::Integer(peer_id));
    let PyRep::Tuple(ref flags_tuple) = cs_parts[2] else {
        panic!("expected (criminals, suspects)");
    };
    assert!(matches!(&flags_tuple[0], PyRep::List(v) if v.contains(&PyRep::Integer(crim_id))));
    assert!(matches!(&flags_tuple[1], PyRep::List(v) if v.contains(&PyRep::Integer(susp_id))));
    assert_eq!(cs_parts[3], PyRep::Integer(1));

    // Nested with_mut_combat_runtime inside ctx.db.with_mut_snapshot does not deadlock and updates cleanly.
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().security_status = 3.5;
        snap.combat_crimewatch_state.safety_levels.insert(char_id, 0);
        with_mut_combat_runtime(&ctx, |rt| {
            rt.safety_levels.insert(char_id, SafetyLevel::Red);
        });
    });
    assert_eq!(with_combat_runtime(&ctx, |rt| rt.safety_levels[&char_id]), SafetyLevel::Red);
}

#[tokio::test]
async fn duel_lifecycle_kill_right_activate_buy_edges_and_action_restriction_capsule() {
    let (ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let kw = HashMap::new();
    let peer_id = char_id + 60;
    let bystander_id = char_id + 61;
    let peer_ship_id = 9_860_001i64;
    let pod_ship_id = 9_860_002i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.balance = IskCents(50_000_00);
            ch.station_id = None;
        }
        let mut p = snap.characters[&char_id].clone();
        p.character_id = peer_id;
        p.user_id = peer_id as u64;
        p.corporation_id += 10;
        p.solar_system_id = sys_id;
        p.active_ship_id = peer_ship_id;
        p.balance = IskCents(100_000_00);
        p.station_id = None;
        snap.characters.insert(peer_id, p);

        let mut b = snap.characters[&char_id].clone();
        b.character_id = bystander_id;
        b.user_id = bystander_id as u64;
        b.corporation_id += 20;
        b.solar_system_id = sys_id;
        b.station_id = None;
        b.balance.0 = 1_000_000_00;
        snap.characters.insert(bystander_id, b);

        snap.items.insert(pod_ship_id, make_fighter_item(pod_ship_id, 670, char_id, sys_id as i64, InventoryFlag::None, 1));
    });

    // Duel Start & Respond error branches.
    assert_eq!(
        dispatch_combat_rpc(&ctx, &mut session, "crimewatch", "StartDuelChallenge", &[PyRep::String("bad".into())], &kw)
            .await
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    assert_eq!(
        dispatch_combat_rpc(&ctx, &mut session, "crimewatch", "StartDuelChallenge", &[PyRep::Integer(char_id)], &kw)
            .await
            .unwrap_err()
            .code,
        "InvalidDuelTarget"
    );
    assert_eq!(
        dispatch_combat_rpc(&ctx, &mut session, "crimewatch", "StartDuelChallenge", &[PyRep::Integer(peer_id)], &kw)
            .await
            .unwrap_err()
            .code,
        "DuelChallengeFailNoSesssionForTarget"
    );
    assert_eq!(
        dispatch_combat_rpc(&ctx, &mut session, "crimewatch", "RespondToDuelChallenge", &[PyRep::String("bad".into())], &kw)
            .await
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    assert_eq!(
        dispatch_combat_rpc(&ctx, &mut session, "crimewatch", "RespondToDuelChallenge", &[PyRep::Integer(peer_id), PyRep::Bool(true)], &kw)
            .await
            .unwrap_err()
            .code,
        "NoPendingDuelChallenge"
    );

    // Register peer online -> StartDuelChallenge -> 3-arg decline then 3-arg accept grants mutual LE.
    ctx.db.with_mut_snapshot(|snap| {
        snap.accounts.insert(
            format!("peer_{peer_id}"),
            eve_db::AccountRecord {
                user_id: peer_id as u64,
                username: format!("peer_{peer_id}"),
                password_token: "pw".into(),
                role: 0,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
    });
    ctx.db
        .acquire_online_lock(peer_id as u64, 99_801, Some(peer_id), true, ctx.now_unix())
        .unwrap();
    dispatch_combat_rpc(&ctx, &mut session, "crimewatch", "StartDuelChallenge", &[PyRep::Integer(peer_id)], &kw)
        .await
        .unwrap();
    let mut peer_sess = eve_machonet::SessionState {
        user_id: Some(peer_id as u64),
        character_id: Some(peer_id),
        solar_system_id: Some(sys_id),
        ship_id: Some(peer_ship_id),
        ..Default::default()
    };
    dispatch_combat_rpc(
        &ctx,
        &mut peer_sess,
        "crimewatch",
        "RespondToDuelChallenge",
        &[PyRep::Integer(char_id), PyRep::Integer(1_340_000_000_000_000), PyRep::Integer(0)],
        &kw,
    )
    .await
    .unwrap();
    assert!(!with_combat_runtime(&ctx, |rt| rt.has_limited_engagement_with(char_id, peer_id, ctx.now_unix())));

    dispatch_combat_rpc(&ctx, &mut session, "crimewatch", "StartDuelChallenge", &[PyRep::Integer(peer_id)], &kw)
        .await
        .unwrap();
    dispatch_combat_rpc(
        &ctx,
        &mut peer_sess,
        "crimewatch",
        "RespondToDuelChallenge",
        &[PyRep::Integer(char_id), PyRep::Integer(1_340_000_000_000_000), PyRep::Integer(1)],
        &kw,
    )
    .await
    .unwrap();
    assert!(with_combat_runtime(&ctx, |rt| rt.has_limited_engagement_with(char_id, peer_id, ctx.now_unix())));
    assert!(with_combat_runtime(&ctx, |rt| rt.has_limited_engagement_with(peer_id, char_id, ctx.now_unix())));

    // KillRight Activate & Buy error paths.
    let kr_id = 920_901i64;
    create_kill_right(&ctx, kr_id, char_id, peer_id, 0.0);
    assert_eq!(
        dispatch_combat_rpc(&ctx, &mut session, "killRightMgr", "ActivateKillRight", &[PyRep::Integer(9_888_777), PyRep::Integer(peer_id), PyRep::Integer(ship_id), PyRep::Integer(peer_ship_id)], &kw)
            .await
            .unwrap_err()
            .code,
        "KillRightNotFound"
    );
    assert_eq!(
        dispatch_combat_rpc(&ctx, &mut peer_sess, "killRightMgr", "ActivateKillRight", &[PyRep::Integer(kr_id), PyRep::Integer(peer_id), PyRep::Integer(peer_ship_id), PyRep::Integer(peer_ship_id)], &kw)
            .await
            .unwrap_err()
            .code,
        "NotKillRightOwner"
    );
    let mut bystander_sess = eve_machonet::SessionState {
        user_id: Some(bystander_id as u64),
        character_id: Some(bystander_id),
        solar_system_id: Some(sys_id),
        ..Default::default()
    };
    assert_eq!(
        dispatch_combat_rpc(&ctx, &mut bystander_sess, "killRightMgr", "BuyKillRight", &[PyRep::Integer(kr_id), PyRep::Real(0.0)], &kw)
            .await
            .unwrap_err()
            .code,
        "KillRightNotForSale"
    );
    assert_eq!(
        dispatch_combat_rpc(&ctx, &mut peer_sess, "killRightMgr", "BuyKillRight", &[PyRep::Integer(kr_id), PyRep::Real(0.0)], &kw)
            .await
            .unwrap_err()
            .code,
        "CannotBuyKillRightAgainstSelf"
    );
    assert_eq!(
        dispatch_combat_rpc(&ctx, &mut session, "killRightMgr", "BuyKillRight", &[PyRep::Integer(kr_id), PyRep::Real(0.0)], &kw)
            .await
            .unwrap_err()
            .code,
        "AlreadyKillRightOwner"
    );

    // Public sale price-changed, insufficient funds, and valid free sale.
    dispatch_combat_rpc(&ctx, &mut session, "killRightMgr", "SellKillRight", &[PyRep::Integer(kr_id), PyRep::Real(5_000_000.0)], &kw)
        .await
        .unwrap();
    assert_eq!(
        dispatch_combat_rpc(&ctx, &mut bystander_sess, "killRightMgr", "BuyKillRight", &[PyRep::Integer(kr_id), PyRep::Real(1_000_000.0)], &kw)
            .await
            .unwrap_err()
            .code,
        "KillRightPriceChanged"
    );
    assert_eq!(
        dispatch_combat_rpc(&ctx, &mut bystander_sess, "killRightMgr", "BuyKillRight", &[PyRep::Integer(kr_id), PyRep::Real(5_000_000.0)], &kw)
            .await
            .unwrap_err()
            .code,
        "InsufficientFunds"
    );
    let kr_free = 920_902i64;
    create_kill_right(&ctx, kr_free, char_id, peer_id, 0.0);
    dispatch_combat_rpc(
        &ctx,
        &mut session,
        "killRightMgr",
        "SellKillRight",
        &[PyRep::Integer(kr_free), PyRep::Real(1.0), PyRep::Integer(bystander_id)],
        &kw,
    )
    .await
    .unwrap();
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    for (bid, pos) in [(ship_id, DVec3::ZERO), (peer_ship_id, DVec3::new(10_000.0, 0.0, 0.0))] {
        sys_handle
            .spawn_ball(Ball {
                id: bid,
                type_id: 587,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 50.0,
                speed_fraction: 1.0,
                mode: MovementMode::Stop,
            })
            .await
            .unwrap();
    }
    dispatch_combat_rpc(&ctx, &mut bystander_sess, "killRightMgr", "BuyKillRight", &[PyRep::Integer(kr_free), PyRep::Real(1.0)], &kw)
        .await
        .unwrap();
    assert!(ctx.crimewatch.read().unwrap().states.get(&peer_id).is_some_and(|st| st.is_suspect(ctx.now_unix())));
    assert_eq!(
        dispatch_combat_rpc(&ctx, &mut session, "killRightMgr", "ActivateKillRight", &[PyRep::Integer(kr_free), PyRep::Integer(peer_id), PyRep::Integer(ship_id), PyRep::Integer(peer_ship_id)], &kw)
            .await
            .unwrap_err()
            .code,
        "KillRightAlreadyActivated"
    );

    // check_crimewatch_action_restriction: weapons, pvp, highsec criminal, and capsule exemption.
    let now = ctx.now_unix();
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        let st = cw.get_or_create(char_id);
        st.criminal_flag_until_unix = now + 900;
        st.weapon_timer_until_unix = now + 60;
        st.pvp_timer_until_unix = now + 900;
    }
    assert_eq!(
        check_crimewatch_action_restriction(&ctx, char_id, ship_id, 30_000_142, "dock", now).unwrap_err().code,
        "CantDockGlobalCriminalFlag"
    );
    assert_eq!(
        check_crimewatch_action_restriction(&ctx, char_id, ship_id, 30_000_142, "wormhole_jump", now).unwrap_err().code,
        "CantEnterWormholeGlobalCriminalFlag"
    );
    // Capsule (type 670) bypasses criminal ship/warp/dock/wh restrictions, then weapons timer hits!
    assert!(check_crimewatch_action_restriction(&ctx, char_id, pod_ship_id, 30_000_142, "wormhole_jump", now).is_ok());
    assert_eq!(
        check_crimewatch_action_restriction(&ctx, char_id, pod_ship_id, 30_000_142, "stargate_jump", now).unwrap_err().code,
        "CannotJumpWithWeaponsTimer"
    );
    assert_eq!(
        check_crimewatch_action_restriction(&ctx, char_id, pod_ship_id, 30_000_142, "safe_logoff", now).unwrap_err().code,
        "PvpLogoffRestricted"
    );
    assert_eq!(
        check_crimewatch_action_restriction(&ctx, char_id, pod_ship_id, 30_000_142, "activate_filament", now).unwrap_err().code,
        "CannotJumpWithWeaponsTimer"
    );
    ctx.crimewatch.write().unwrap().get_or_create(char_id).weapon_timer_until_unix = 0;
    assert_eq!(
        check_crimewatch_action_restriction(&ctx, char_id, pod_ship_id, 30_000_142, "activate_filament", now).unwrap_err().code,
        "PvpTimerBlocksFilament"
    );
}

