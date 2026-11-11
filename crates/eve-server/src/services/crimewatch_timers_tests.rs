use super::*;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::{
    make_fighter_ctx_and_session, make_fighter_item, make_fighter_type,
};
use eve_db::{InventoryFlag, IskCents, KillRightRecord};
use eve_destiny::{Ball, CombatState, DVec3, MovementMode};
use std::collections::HashMap;

#[tokio::test]
async fn test_faction_police_sec_threshold_and_response() {
    assert_eq!(faction_police_sec_threshold(1.0), Some(-2.0));
    assert_eq!(faction_police_sec_threshold(0.95), Some(-2.0));
    assert_eq!(faction_police_sec_threshold(0.9), Some(-2.5));
    assert_eq!(faction_police_sec_threshold(0.8), Some(-3.0));
    assert_eq!(faction_police_sec_threshold(0.7), Some(-3.5));
    assert_eq!(faction_police_sec_threshold(0.6), Some(-4.0));
    assert_eq!(faction_police_sec_threshold(0.5), Some(-4.5));
    assert_eq!(faction_police_sec_threshold(0.45), Some(-4.5));
    assert_eq!(faction_police_sec_threshold(0.4), None);

    let (ctx, _sess, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    assert_eq!(
        evaluate_faction_police_response(&ctx, sys_id, char_id, ship_id).await,
        (false, true, false)
    );

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.security_status = -3.0;
        }
    });
    assert_eq!(
        evaluate_faction_police_response(&ctx, sys_id, char_id, ship_id).await,
        (true, false, true)
    );
}

#[test]
fn test_evaluate_remote_assistance_legality_and_action_restrictions() {
    let (ctx, _sess, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let target_id = 90_000_002;
    let now = ctx.now_unix();

    let (status, criminal, suspect) =
        evaluate_remote_assistance_legality(&ctx, char_id, target_id, sys_id, now);
    assert_eq!(status, "Legal");
    assert!(!criminal);
    assert!(!suspect);

    {
        let mut cw = ctx.crimewatch.write().unwrap();
        cw.get_or_create(target_id).suspect_flag_until_unix = now + 900;
    }
    let (status, criminal, suspect) =
        evaluate_remote_assistance_legality(&ctx, char_id, target_id, sys_id, now);
    assert_eq!(status, "Suspect");
    assert!(!criminal);
    assert!(suspect);

    {
        let mut cw = ctx.crimewatch.write().unwrap();
        cw.get_or_create(target_id).criminal_flag_until_unix = now + 900;
    }
    let (status, criminal, _suspect) =
        evaluate_remote_assistance_legality(&ctx, char_id, target_id, sys_id, now);
    assert_eq!(status, "Criminal");
    assert!(criminal);

    // Action restrictions: weapon timer blocks dock / stargate_jump, allows wormhole_jump
    assert!(
        check_crimewatch_action_restriction(&ctx, char_id, ship_id, sys_id, "dock", now).is_err()
    );
    assert!(
        check_crimewatch_action_restriction(&ctx, char_id, ship_id, sys_id, "stargate_jump", now)
            .is_err()
    );

    // Pod exemptions for dock/jump in highsec vs non-pod criminal block
    let pod_id = 200_000_999_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            pod_id,
            make_fighter_item(pod_id, 670, char_id, sys_id as i64, InventoryFlag::None, 1),
        );
    });
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        let st = cw.get_or_create(char_id);
        st.weapon_timer_until_unix = 0;
        st.criminal_flag_until_unix = now + 900;
    }
    assert!(
        check_crimewatch_action_restriction(&ctx, char_id, ship_id, sys_id, "dock", now).is_err()
    );
    assert!(
        check_crimewatch_action_restriction(&ctx, char_id, pod_id, sys_id, "dock", now).is_ok()
    );
}

#[tokio::test]
async fn test_apply_illegal_pvp_sec_status_loss_and_sentry_guns() {
    let (ctx, _sess, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let now = ctx.now_unix();
    apply_illegal_pvp_sec_status_loss(&ctx, char_id, 90_000_002, sys_id, 0.9, false, now);
    let new_sec = ctx
        .db
        .with_snapshot(|s| s.characters.get(&char_id).unwrap().security_status);
    assert!(new_sec < 0.5);

    // Sentry gun grid aggression
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        cw.get_or_create(char_id).criminal_flag_until_unix = now + 900;
    }
    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = sys
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 600,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 100.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::new(),
        )
        .await;
    let (engaged, dmg) =
        step_sentry_gun_grid_aggression(&ctx, sys_id, char_id, ship_id, DVec3::ZERO, true).await;
    assert!(engaged);
    assert!(dmg > 0.0);
}

#[tokio::test]
async fn test_handle_crimewatch_rpcs_and_duel_lifecycle() {
    let (ctx, mut sess, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let kw = HashMap::new();
    let other_id = 90_000_002;
    let other_ship_id = 100_000_099;
    let other_user_id = 42;
    ctx.db.with_mut_snapshot(|snap| {
        let mut other = snap.characters.get(&char_id).cloned().unwrap();
        other.character_id = other_id;
        other.user_id = other_user_id;
        other.name = "DuelTarget".into();
        other.corporation_id = 1_000_009;
        other.alliance_id = None;
        other.station_id = None;
        other.structure_id = None;
        other.solar_system_id = sys_id;
        other.active_ship_id = other_ship_id;
        other.balance = IskCents::from_isk(50_000_000);
        other.security_status = 1.5;
        snap.characters.insert(other_id, other);
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.balance = IskCents::from_isk(50_000_000);
        }
    });

    // GetMySecurityStatus & GetCharacterSecurityStatus
    let my_sec = handle_crimewatch(&ctx, &mut sess, "crimewatch", "GetMySecurityStatus", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(my_sec.as_f64(), Some(0.5));
    let other_sec = handle_crimewatch(
        &ctx,
        &mut sess,
        "crimewatch",
        "GetCharacterSecurityStatus",
        &[PyRep::Integer(other_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(other_sec.as_f64(), Some(1.5));

    // SetSafetyLevel & GetClientStates
    let set_red = handle_crimewatch(
        &ctx,
        &mut sess,
        "crimewatch",
        "SetSafetyLevel",
        &[PyRep::Integer(0)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(set_red.dict_get("safetyLevel").and_then(|v| v.as_i64()), Some(0));

    let states = handle_crimewatch(&ctx, &mut sess, "crimewatch", "GetClientStates", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    if let PyRep::Tuple(items) = states {
        assert_eq!(items.len(), 4);
        assert_eq!(items[3].as_i64(), Some(0));
    } else {
        panic!("expected 4-tuple from GetClientStates");
    }

    // GetSecurityStatusTransactions
    apply_illegal_pvp_sec_status_loss(&ctx, char_id, other_id, sys_id, 0.9, false, ctx.now_unix());
    let txs = handle_crimewatch(
        &ctx,
        &mut sess,
        "crimewatch",
        "GetSecurityStatusTransactions",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(!matches!(txs, PyRep::None));

    // BuyKillRight (requires both ships on-grid in space)
    let sys = ctx.systems.get_or_spawn(sys_id);
    for (id, pos) in [(ship_id, DVec3::ZERO), (other_ship_id, DVec3::new(1_000.0, 0.0, 0.0))] {
        sys.spawn_ball_with_combat(
            Ball {
                id,
                type_id: 600,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 100.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::new(),
        )
        .await
        .unwrap();
    }
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.kill_rights.insert(
            777,
            KillRightRecord {
                kill_right_id: 777,
                owner_id: other_id,
                target_id: other_id,
                activation_fee: IskCents::from_isk(1_000_000),
                is_active_suspect: false,
            },
        );
    });
    with_mut_combat_runtime(&ctx, |rt| {
        rt.kill_right_expires_unix.insert(777, now + 86_400);
    });
    // Fix owner != target for valid BuyKillRight
    ctx.db.with_mut_snapshot(|snap| {
        let mut third = snap.characters.get(&char_id).cloned().unwrap();
        third.character_id = 90_000_099;
        snap.characters.insert(90_000_099, third);
        snap.kill_rights.get_mut(&777).unwrap().owner_id = 90_000_099;
    });
    let buy = handle_crimewatch(
        &ctx,
        &mut sess,
        "killRightMgr",
        "BuyKillRight",
        &[PyRep::Integer(777), PyRep::Real(1_000_000.0)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(buy.dict_get("killRightID").and_then(|v| v.as_i64()), Some(777));

    // Duel challenge -> offline error when not registered online
    let offline_err = handle_crimewatch(
        &ctx,
        &mut sess,
        "crimewatch",
        "StartDuelChallenge",
        &[PyRep::Integer(other_id)],
        &kw,
    )
    .await
    .unwrap();
    assert!(offline_err.is_err());

    // Seed challenge directly and test RespondToDuelChallenge
    with_mut_combat_runtime(&ctx, |rt| {
        rt.duel_challenges.insert((other_id, char_id), now + 60);
    });
    let respond = handle_crimewatch(
        &ctx,
        &mut sess,
        "crimewatch",
        "RespondToDuelChallenge",
        &[PyRep::Integer(other_id), PyRep::Bool(true)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        respond.dict_get("limitedEngagementSeconds").and_then(|v| v.as_i64()),
        Some(300)
    );
    assert!(with_combat_runtime(&ctx, |rt| {
        rt.has_limited_engagement_with(char_id, other_id, now)
    }));
}

#[tokio::test]
async fn test_respond_to_duel_challenge_three_arg_and_fail_closed() {
    let (ctx, mut sess, char_id, _ship_id, _sys_id) = make_fighter_ctx_and_session();
    let other_id = 90_000_002;
    let kw = HashMap::new();
    let now = ctx.now_unix();
    let expiry_ft = ctx.now_blueos_filetime_ticks() + 600_000_000;

    // 1. V24.01 3-arg decline: (fromCharID, expiryTime, false) must decline with zero LE created
    with_mut_combat_runtime(&ctx, |rt| {
        rt.duel_challenges.insert((other_id, char_id), now + 60);
    });
    let decline_3arg = handle_crimewatch(
        &ctx,
        &mut sess,
        "crimewatch",
        "RespondToDuelChallenge",
        &[
            PyRep::Integer(other_id),
            PyRep::Integer(expiry_ft),
            PyRep::Bool(false),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(decline_3arg.dict_get("accepted").and_then(|v| v.as_bool()), Some(false));
    assert_eq!(
        decline_3arg
            .dict_get("limitedEngagementSeconds")
            .and_then(|v| v.as_i64()),
        Some(0)
    );
    assert!(!with_combat_runtime(&ctx, |rt| {
        rt.has_limited_engagement_with(char_id, other_id, now)
            || rt.has_limited_engagement_with(other_id, char_id, now)
    }));

    // 2. Fail-closed on 2-arg non-bool `(fromCharID, expiryTime)` and 1-arg `(fromCharID,)` and non-bool 3rd arg
    for bad_args in [
        vec![PyRep::Integer(other_id), PyRep::Integer(expiry_ft)],
        vec![PyRep::Integer(other_id)],
        vec![
            PyRep::Integer(other_id),
            PyRep::Integer(expiry_ft),
            PyRep::None,
        ],
    ] {
        with_mut_combat_runtime(&ctx, |rt| {
            rt.duel_challenges.insert((other_id, char_id), now + 60);
        });
        let resp = handle_crimewatch(
            &ctx,
            &mut sess,
            "crimewatch",
            "RespondToDuelChallenge",
            &bad_args,
            &kw,
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(resp.dict_get("accepted").and_then(|v| v.as_bool()), Some(false));
        assert!(!with_combat_runtime(&ctx, |rt| {
            rt.has_limited_engagement_with(char_id, other_id, now)
        }));
    }

    // 3. V24.01 3-arg accept: (fromCharID, expiryTime, true) creates mutual LE
    with_mut_combat_runtime(&ctx, |rt| {
        rt.duel_challenges.insert((other_id, char_id), now + 60);
    });
    let accept_3arg = handle_crimewatch(
        &ctx,
        &mut sess,
        "crimewatch",
        "RespondToDuelChallenge",
        &[
            PyRep::Integer(other_id),
            PyRep::Integer(expiry_ft),
            PyRep::Bool(true),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(accept_3arg.dict_get("accepted").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(
        accept_3arg
            .dict_get("limitedEngagementSeconds")
            .and_then(|v| v.as_i64()),
        Some(300)
    );
    assert!(with_combat_runtime(&ctx, |rt| {
        rt.has_limited_engagement_with(char_id, other_id, now)
            && rt.has_limited_engagement_with(other_id, char_id, now)
    }));
}

#[test]
fn test_evaluate_and_record_pvp_aggression_and_remote_assistance() {
    let (ctx, _sess, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let victim_id = 90_000_003;
    let victim_ship_id = 100_000_003;
    ctx.db.with_mut_snapshot(|snap| {
        let mut vic = snap.characters.get(&char_id).cloned().unwrap();
        vic.character_id = victim_id;
        vic.user_id = 43;
        vic.name = "Victim".into();
        vic.corporation_id = 1_000_009;
        vic.alliance_id = None;
        vic.station_id = None;
        vic.structure_id = None;
        vic.solar_system_id = sys_id;
        vic.active_ship_id = victim_ship_id;
        vic.balance = IskCents(0);
        vic.security_status = 0.0;
        snap.characters.insert(victim_id, vic);
        snap.items.insert(
            victim_ship_id,
            make_fighter_item(
                victim_ship_id,
                600,
                victim_id,
                sys_id as i64,
                InventoryFlag::None,
                1,
            ),
        );
    });

    // Green safety blocks illegal highsec aggression
    with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(char_id, SafetyLevel::Green);
    });
    assert!(
        evaluate_and_record_pvp_aggression(&ctx, char_id, ship_id, sys_id, victim_ship_id, false)
            .is_err()
    );

    // Red safety allows aggression and flags criminal in highsec
    with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(char_id, SafetyLevel::Red);
    });
    evaluate_and_record_pvp_aggression(&ctx, char_id, ship_id, sys_id, victim_ship_id, false)
        .expect("Red safety should permit PvP aggression");
    let now = ctx.now_unix();
    assert!(ctx
        .crimewatch
        .write()
        .unwrap()
        .get_or_create(char_id)
        .is_criminal(now));

    // Remote assistance to criminal victim is blocked on Green safety, allowed on Red safety
    with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(victim_id, SafetyLevel::Green);
    });
    assert!(
        evaluate_and_record_remote_assistance(&ctx, victim_id, victim_ship_id, sys_id, ship_id)
            .is_err()
    );
    with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(victim_id, SafetyLevel::Red);
    });
    evaluate_and_record_remote_assistance(&ctx, victim_id, victim_ship_id, sys_id, ship_id)
        .expect("Red safety remote assistance to criminal should succeed and inherit criminal flag");
    assert!(ctx
        .crimewatch
        .write()
        .unwrap()
        .get_or_create(victim_id)
        .is_criminal(now));
    let _ = make_fighter_type(600, 25, 6, 10_000.0, vec![]);
}

#[test]
fn on_advance_secs_internal_sweeps_expired_crimewatch_and_runtime_maps() {
    let (ctx, _sess, char_id, _ship_id, _sys_id) = make_fighter_ctx_and_session();
    let now = 1_750_000_000_u64;
    ctx.set_clock(std::sync::Arc::new(eve_db::VirtualServerClock::with_fixed_base(now)));
    let target_id = 90_000_002_i64;
    let active_char_id = 90_000_003_i64;

    {
        let mut cw = ctx.crimewatch.write().unwrap();
        let st_exp = cw.get_or_create(char_id);
        st_exp.suspect_flag_until_unix = now + 10;
        st_exp.criminal_flag_until_unix = now + 10;
        st_exp.weapon_timer_until_unix = now + 10;
        st_exp.pvp_timer_until_unix = now + 10;

        let st_live = cw.get_or_create(active_char_id);
        st_live.suspect_flag_until_unix = now + 300;
    }
    with_mut_combat_runtime(&ctx, |rt| {
        rt.disapproval_flags.insert(char_id, now + 10);
        rt.disapproval_flags.insert(active_char_id, now + 300);
        rt.npc_timers.insert(char_id, now + 10);
        rt.duel_challenges.insert((char_id, target_id), now + 10);
        rt.kill_right_expires_unix.insert(5001, now + 10);
        rt.limited_engagements
            .entry(char_id)
            .or_default()
            .insert(target_id, now + 10);
    });

    ctx.clock().advance_secs(10);
    crate::sp6_lifecycle::on_advance_secs_internal(&ctx, now + 10, 10, false, false);

    {
        let cw = ctx.crimewatch.read().unwrap();
        assert!(
            !cw.states.contains_key(&char_id),
            "fully expired pilot CrimeWatch state must be swept"
        );
        assert!(
            cw.states.contains_key(&active_char_id),
            "unexpired pilot CrimeWatch state must remain"
        );
    }
    with_combat_runtime(&ctx, |rt| {
        assert!(!rt.disapproval_flags.contains_key(&char_id));
        assert!(rt.disapproval_flags.contains_key(&active_char_id));
        assert!(!rt.npc_timers.contains_key(&char_id));
        assert!(!rt.duel_challenges.contains_key(&(char_id, target_id)));
        assert!(!rt.kill_right_expires_unix.contains_key(&5001));
        assert!(!rt.limited_engagements.contains_key(&char_id));
    });

    // Boundary check: timer expiring at now + 11 (> now + 10) is NOT swept at now + 10
    with_mut_combat_runtime(&ctx, |rt| {
        rt.npc_timers.insert(active_char_id, now + 11);
    });
    crate::sp6_lifecycle::on_advance_secs_internal(&ctx, now + 10, 0, false, false);
    with_combat_runtime(&ctx, |rt| {
        assert_eq!(rt.npc_timers.get(&active_char_id).copied(), Some(now + 11));
    });
}

#[cfg(test)]
#[path = "crimewatch_timers_extra_unit_tests.rs"]
mod crimewatch_timers_extra_unit_tests;
