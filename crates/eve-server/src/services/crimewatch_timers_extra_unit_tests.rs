//! Extra unit and mutation-killing tests for `crimewatch_timers`.

#![forbid(unsafe_code)]

use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::{
    make_fighter_ctx_and_session, make_fighter_item,
};
use crate::services::combat_crimewatch::*;
use eve_db::{InventoryFlag, IskCents, KillRightRecord, WarRecord};
use eve_destiny::{Ball, CombatState, DVec3, MovementMode};
use eve_proto::PyRep;
use std::collections::HashMap;

#[tokio::test]
async fn faction_police_capsule_threshold_and_lowsec_branches() {
    let (ctx, _sess, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();

    // Above threshold in highsec -> (false, true, false)
    assert_eq!(
        evaluate_faction_police_response(&ctx, sys_id, char_id, ship_id).await,
        (false, true, false)
    );

    // Lowsec system (< 0.45) never triggers faction police even for -5.0 outlaw
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().security_status = -5.0;
    });
    assert_eq!(
        evaluate_faction_police_response(&ctx, 30_002_537, char_id, ship_id).await,
        (false, true, false)
    );

    // Highsec ship below threshold -> (true, false, true)
    assert_eq!(
        evaluate_faction_police_response(&ctx, sys_id, char_id, ship_id).await,
        (true, false, true)
    );

    // Highsec capsule (type 670) below threshold -> spawn_pursuit = false, can_dock = true, cloak_disabled = true
    let pod_id = 200_111_001i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            pod_id,
            make_fighter_item(pod_id, 670, char_id, sys_id as i64, InventoryFlag::None, 1),
        );
    });
    assert_eq!(
        evaluate_faction_police_response(&ctx, sys_id, char_id, pod_id).await,
        (false, true, true)
    );
}

#[test]
fn remote_assistance_legality_war_interference_and_propagation_matrix() {
    let (ctx, _sess, char_id, ship_id, hs_sys) = make_fighter_ctx_and_session();
    let ls_sys = 30_002_537u32;
    let ns_sys = 30_004_759u32;
    let target_id = 90_000_002i64;
    let enemy_id = 90_000_003i64;
    let now = ctx.now_unix();

    ctx.db.with_mut_snapshot(|snap| {
        let base = snap.characters[&char_id].clone();
        for (cid, corp) in [(target_id, 98_200_002i64), (enemy_id, 98_200_003i64)] {
            let mut c = base.clone();
            c.character_id = cid;
            c.corporation_id = corp;
            snap.characters.insert(cid, c);
        }
    });

    // Self-assistance is always Legal
    assert_eq!(
        evaluate_remote_assistance_legality(&ctx, char_id, char_id, hs_sys, now),
        ("Legal", false, false)
    );

    // Limited engagement interference (helper not in LE with opponent): Criminal in highsec, Suspect in lowsec, Legal in nullsec
    with_mut_combat_runtime(&ctx, |rt| {
        rt.add_limited_engagement(target_id, enemy_id, now);
    });
    assert_eq!(
        evaluate_remote_assistance_legality(&ctx, char_id, target_id, hs_sys, now),
        ("Criminal", true, false)
    );
    assert_eq!(
        evaluate_remote_assistance_legality(&ctx, char_id, target_id, ls_sys, now),
        ("Suspect", false, true)
    );
    assert_eq!(
        evaluate_remote_assistance_legality(&ctx, char_id, target_id, ns_sys, now),
        ("Legal", false, false)
    );

    // Propagating remote assistance in nullsec inherits LE without Suspect/Criminal
    ctx.crimewatch.write().unwrap().states.clear();
    evaluate_and_record_remote_assistance(&ctx, char_id, ship_id, ns_sys, target_id).unwrap();
    assert!(with_combat_runtime(&ctx, |rt| {
        rt.has_limited_engagement_with(char_id, enemy_id, now)
    }));
    with_mut_combat_runtime(&ctx, |rt| {
        rt.limited_engagements.clear();
    });
    ctx.crimewatch.write().unwrap().states.clear();

    // War interference: neutral helper assisting war participant with active PvP timer
    crate::sp6_lifecycle::record_pvp_aggression_and_renew_logoff_timer(&ctx, target_id, enemy_id, now);
    ctx.db.with_mut_snapshot(|snap| {
        snap.wars.insert(
            501,
            WarRecord {
                war_id: 501,
                aggressor_corp_id: 98_200_002,
                defender_corp_id: 98_200_003,
                started_unix: now - 1_000, // in 24h warmup unless mutual
                active: true,
                mutual: false,
            },
        );
    });
    // Non-mutual war during 24h warmup is not yet active for war interference
    assert!(!is_war_interference_assistance(&ctx, char_id, target_id, now));
    // Mutual war is immediately active -> triggers war interference (Criminal in HS, Suspect in LS)
    ctx.db.with_mut_snapshot(|snap| {
        snap.wars.get_mut(&501).unwrap().mutual = true;
    });
    assert!(is_war_interference_assistance(&ctx, char_id, target_id, now));
    assert_eq!(
        evaluate_remote_assistance_legality(&ctx, char_id, target_id, ls_sys, now),
        ("Suspect", false, true)
    );

    // Outlaw (-5.0) beneficiary in highsec vs lowsec
    ctx.db.with_mut_snapshot(|snap| {
        snap.wars.clear();
        snap.characters.get_mut(&target_id).unwrap().security_status = -5.0;
    });
    ctx.crimewatch.write().unwrap().states.clear();
    assert_eq!(
        evaluate_remote_assistance_legality(&ctx, char_id, target_id, hs_sys, now),
        ("Suspect", false, true)
    );
    assert_eq!(
        evaluate_remote_assistance_legality(&ctx, char_id, target_id, ls_sys, now),
        ("Legal", false, false)
    );
}

#[tokio::test]
async fn crimewatch_timer_notifications_action_restrictions_and_sentry_grid() {
    let (ctx, _sess, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let now = ctx.now_unix();

    // Emit timer notifications: active -> cooldown -> idle transitions across all 5 timer slots (including NPC & disapproval)
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        let st = cw.get_or_create(char_id);
        st.weapon_timer_until_unix = now + 60;
        st.pvp_timer_until_unix = now + 900;
        st.suspect_flag_until_unix = now + 900;
    }
    with_mut_combat_runtime(&ctx, |rt| {
        rt.npc_timers.insert(char_id, now + 300);
        rt.disapproval_flags.insert(char_id, now + 900);
    });
    emit_crimewatch_timer_notifications(&ctx, char_id, now);
    emit_crimewatch_timer_notifications(&ctx, char_id, now + 10);
    // Transition Suspect -> Criminal -> Idle
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        let st = cw.get_or_create(char_id);
        st.criminal_flag_until_unix = now + 900;
    }
    emit_crimewatch_timer_notifications(&ctx, char_id, now + 20);
    ctx.crimewatch.write().unwrap().states.clear();
    with_mut_combat_runtime(&ctx, |rt| {
        rt.npc_timers.clear();
        rt.disapproval_flags.clear();
    });
    emit_crimewatch_timer_notifications(&ctx, char_id, now + 1_000);

    update_system_security_status(&ctx, sys_id, 0.5);
    on_weapon_aggression_post_hook(&ctx, char_id, None, sys_id, now);

    // Exhaustive action restriction matrix: highsec criminal + pending CONCORD
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        let st = cw.get_or_create(char_id);
        st.pending_concord_arrival = Some((sys_id, now + 15, ship_id, char_id));
        st.criminal_flag_until_unix = now + 900;
    }
    for act in [
        "wormhole_jump",
        "warp",
        "eject",
        "stargate_jump",
        "dock",
        "jump_drive",
        "store_ship",
        "switch_ship",
    ] {
        assert!(
            check_crimewatch_action_restriction(&ctx, char_id, ship_id, sys_id, act, now).is_err()
        );
    }

    // Weapon timer active (no criminal/CONCORD): blocks store_ship, switch_ship, acceleration_gate, refit, tether, filament; allows wormhole_jump
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        let st = cw.get_or_create(char_id);
        st.pending_concord_arrival = None;
        st.criminal_flag_until_unix = 0;
        st.weapon_timer_until_unix = now + 60;
        st.pvp_timer_until_unix = now + 900;
    }
    for blocked in [
        "store_ship",
        "switch_ship",
        "acceleration_gate",
        "refit",
        "refit_in_space",
        "tether",
        "filament",
        "activate_filament",
        "safe_logoff",
        "jump_drive",
        "jump_bridge",
        "jump_portal",
        "eject",
        "dock",
    ] {
        assert!(
            check_crimewatch_action_restriction(&ctx, char_id, ship_id, sys_id, blocked, now)
                .is_err()
        );
    }
    assert!(
        check_crimewatch_action_restriction(&ctx, char_id, ship_id, sys_id, "wormhole_jump", now)
            .is_ok()
    );

    // PvP timer only (weapon timer expired): blocks filament & safe_logoff, allows tether
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        let st = cw.get_or_create(char_id);
        st.weapon_timer_until_unix = 0;
        st.pvp_timer_until_unix = now + 900;
    }
    assert!(
        check_crimewatch_action_restriction(&ctx, char_id, ship_id, sys_id, "filament", now)
            .is_err()
    );
    assert!(
        check_crimewatch_action_restriction(&ctx, char_id, ship_id, sys_id, "safe_logoff", now)
            .is_err()
    );
    assert!(
        check_crimewatch_action_restriction(&ctx, char_id, ship_id, sys_id, "tether", now).is_ok()
    );

    // Sentry gun grid aggression: nullsec returns (false, 0.0); on-grid highsec engages, off-grid (>500km) drops engagement
    assert_eq!(
        step_sentry_gun_grid_aggression(&ctx, 30_004_759, char_id, ship_id, DVec3::ZERO, true)
            .await,
        (false, 0.0)
    );
    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = sys
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 600,
                position: DVec3::new(10_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::new(),
        )
        .await;
    let (engaged_on, _) =
        step_sentry_gun_grid_aggression(&ctx, sys_id, char_id, ship_id, DVec3::ZERO, true).await;
    assert!(engaged_on);
    let (engaged_off, _) = step_sentry_gun_grid_aggression(
        &ctx,
        sys_id,
        char_id,
        ship_id,
        DVec3::new(1_000_000.0, 0.0, 0.0),
        false,
    )
    .await;
    assert!(!engaged_off);
}

#[tokio::test]
async fn handle_crimewatch_kill_rights_aggression_mgr_duels_and_pvp_branches() {
    let (ctx, mut sess, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let victim_id = 90_000_002i64;
    let third_id = 90_000_003i64;
    let kw = HashMap::new();
    let now = ctx.now_unix();

    ctx.db.with_mut_snapshot(|snap| {
        let base = snap.characters[&char_id].clone();
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = 98_000_001;
            ch.station_id = None;
            ch.structure_id = None;
        }
        for (cid, corp, stn) in [
            (victim_id, 98_000_001i64, None),
            (third_id, 98_300_003i64, Some(60_003_760i64)),
        ] {
            let mut c = base.clone();
            c.character_id = cid;
            c.corporation_id = corp;
            c.station_id = stn;
            c.structure_id = None;
            c.balance = IskCents::from_isk(10_000_000);
            snap.characters.insert(cid, c);
        }
        // Seed KillRights: 701 (free corp-restricted to 98_000_001), 702 (for sale 5M against char_id)
        snap.kill_rights.insert(
            701,
            KillRightRecord {
                kill_right_id: 701,
                owner_id: third_id,
                target_id: victim_id,
                activation_fee: IskCents(0),
                is_active_suspect: false,
            },
        );
        snap.kill_rights.insert(
            702,
            KillRightRecord {
                kill_right_id: 702,
                owner_id: third_id,
                target_id: char_id,
                activation_fee: IskCents::from_isk(5_000_000),
                is_active_suspect: false,
            },
        );
        snap.combat_crimewatch_state
            .kill_right_restricted_to
            .insert(701, 98_000_001);
        snap.combat_crimewatch_state
            .kill_right_expires_unix
            .insert(701, now + 86_400);
        snap.combat_crimewatch_state
            .kill_right_expires_unix
            .insert(702, now + 86_400);
    });

    // Seed SecStatusTransactions with event_type_id == 0 to exercise reason-string fallback mapping
    with_mut_combat_runtime(&ctx, |rt| {
        for reason in [
            "CONCORD kill penalty",
            "Illegal Capsule Destruction",
            "Illegal PvP Aggression",
            "Pirate NPC Bounty",
            "Other Security Change",
        ] {
            rt.sec_status_transactions
                .entry(char_id)
                .or_default()
                .push(SecStatusTransaction {
                    timestamp_unix: now,
                    old_sec: 0.0,
                    new_sec: -0.1,
                    delta: -0.1,
                    reason: reason.into(),
                    solar_system_id: sys_id,
                    event_type_id: 0,
                    other_owner_id: None,
                    other_type_id: None,
                });
        }
    });

    let txs = handle_crimewatch(&ctx, &mut sess, "crimewatch", "GetSecurityStatusTransactions", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(txs, PyRep::Rowset { .. }));

    // ActivateKillRight on free corp-restricted KR 701 succeeds for corp member `char_id`
    let act_kr = handle_crimewatch(&ctx, &mut sess, "killRightMgr", "ActivateKillRight", &[PyRep::Integer(701)], &kw)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        act_kr.dict_get("suspectFlagActive").and_then(|v| v.as_bool()),
        Some(true)
    );

    // BuyKillRight validation errors: self-target (702 is against char_id)
    assert_eq!(
        handle_crimewatch(
            &ctx,
            &mut sess,
            "killRightMgr",
            "BuyKillRight",
            &[PyRep::Integer(702), PyRep::Real(5_000_000.0)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "CannotBuyKillRightAgainstSelf"
    );

    // GetMySecurityStatus & GetCharacterSecurityStatus RPCs
    let my_sec = handle_crimewatch(&ctx, &mut sess, "crimewatch", "GetMySecurityStatus", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(my_sec, PyRep::Real(_)));
    let char_sec = handle_crimewatch(&ctx, &mut sess, "crimewatch", "GetCharacterSecurityStatus", &[PyRep::Integer(char_id)], &kw)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(char_sec, PyRep::Real(_)));

    // Duel challenge offline error, decline & expired challenge
    assert_eq!(
        handle_crimewatch(&ctx, &mut sess, "crimewatch", "StartDuelChallenge", &[PyRep::Integer(victim_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "DuelChallengeFailNoSesssionForTarget"
    );
    with_mut_combat_runtime(&ctx, |rt| {
        rt.duel_challenges.insert((char_id, victim_id), now + 60);
    });
    let mut vic_sess = sess.clone();
    vic_sess.character_id = Some(victim_id);
    let dec = handle_crimewatch(
        &ctx,
        &mut vic_sess,
        "crimewatch",
        "RespondToDuelChallenge",
        &[PyRep::Integer(char_id), PyRep::Bool(false)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(dec.dict_get("accepted").and_then(|v| v.as_bool()), Some(false));
    with_mut_combat_runtime(&ctx, |rt| {
        rt.duel_challenges.insert((char_id, victim_id), now.saturating_sub(10));
    });
    assert_eq!(
        handle_crimewatch(
            &ctx,
            &mut vic_sess,
            "crimewatch",
            "RespondToDuelChallenge",
            &[PyRep::Integer(char_id), PyRep::Bool(true)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "NoPendingDuelChallenge"
    );

    // PvP aggression in Abyssal (32_000_001 -> Ok), Zarzakh (30_100_000 -> disapproval), and Yellow safety in lowsec against pod (rejected)
    with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(char_id, SafetyLevel::Red);
    });
    ctx.crimewatch.write().unwrap().states.clear();
    evaluate_and_record_pvp_aggression(&ctx, char_id, ship_id, 32_000_001, victim_id, false).unwrap();
    evaluate_and_record_pvp_aggression(&ctx, char_id, ship_id, 30_100_000, victim_id, false).unwrap();
    assert!(with_combat_runtime(&ctx, |rt| {
        rt.disapproval_flags.contains_key(&char_id)
    }));

    // Clear LEs & flags, set Yellow safety -> attacking pod in lowsec is Criminal, so blocked by Yellow safety
    with_mut_combat_runtime(&ctx, |rt| {
        rt.limited_engagements.clear();
        rt.safety_levels.insert(char_id, SafetyLevel::Yellow);
    });
    ctx.crimewatch.write().unwrap().states.clear();
    assert_eq!(
        evaluate_and_record_pvp_aggression(&ctx, char_id, ship_id, 30_002_537, victim_id, true)
            .unwrap_err()
            .code,
        "CantDoInCurrentSafetyLevel"
    );
}
