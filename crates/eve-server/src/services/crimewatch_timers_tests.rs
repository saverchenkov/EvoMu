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
        snap.combat_crimewatch_state
            .as_inner_mut()
            .kill_right_expires_unix
            .insert(777, now + 86_400);
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
    make_duel_peer_online_and_undocked(&ctx, &mut sess, char_id, other_id, now);
    ctx.db.with_mut_snapshot(|snap| {
        snap.combat_crimewatch_state
            .as_inner_mut()
            .duel_challenges
            .insert((other_id, char_id), now + 60);
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
    assert!(has_limited_engagement_with(&ctx, char_id, other_id, now));

    // Seed an expired challenge and verify StartDuelChallenge failing with
    // DuelChallengeActiveEngagement rolls back the expired-challenge retain (plan:S-8).
    let stale_pair = (90_000_055_i64, 90_000_056_i64);
    ctx.db.with_mut_snapshot(|snap| {
        snap.combat_crimewatch_state
            .as_inner_mut()
            .duel_challenges
            .insert(stale_pair, now.saturating_sub(1));
    });
    let active_le_err = handle_crimewatch(
        &ctx,
        &mut sess,
        "crimewatch",
        "StartDuelChallenge",
        &[PyRep::Integer(other_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(active_le_err.code, "DuelChallengeActiveEngagement");
    assert!(ctx.db.with_snapshot(|snap| {
        snap.combat_crimewatch_state
            .duel_challenges
            .contains_key(&stale_pair)
    }));
}

fn make_duel_peer_online_and_undocked(
    ctx: &ServerContext,
    sess: &mut SessionState,
    char_id: i64,
    peer_id: i64,
    now_unix: u64,
) {
    sess.station_id = None;
    sess.structure_id = None;
    let duel_sys = sess.solar_system_id.unwrap_or(30_000_142);
    let peer_uid = ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = duel_sys;
        }
        let peer_uid = {
            let peer = snap.characters.get_mut(&peer_id).unwrap();
            peer.station_id = None;
            peer.structure_id = None;
            peer.solar_system_id = duel_sys;
            peer.user_id
        };
        if !snap.accounts.values().any(|a| a.user_id == peer_uid) {
            let uname = format!("u{peer_uid}");
            snap.accounts.insert(
                uname.clone(),
                eve_db::AccountRecord {
                    user_id: peer_uid,
                    username: uname,
                    password_token: String::new(),
                    role: 0,
                    banned_until_unix: None,
                    ban_reason: None,
                },
            );
        }
        peer_uid
    });
    if !ctx.is_character_online(peer_id, peer_uid) {
        ctx.db
            .acquire_online_lock(peer_uid, 99_703, Some(peer_id), true, now_unix)
            .unwrap();
    }
}

#[tokio::test]
async fn test_respond_to_duel_challenge_three_arg_and_fail_closed() {
    let (ctx, mut sess, char_id, _ship_id, _sys_id) = make_fighter_ctx_and_session();
    let other_id = 90_000_002;
    let kw = HashMap::new();
    let now = ctx.now_unix();
    let expiry_ft = ctx.now_blueos_filetime_ticks() + 600_000_000;

    // 1. V24.01 3-arg decline: (fromCharID, expiryTime, false) must decline with zero LE created
    ctx.db.with_mut_snapshot(|snap| {
        snap.combat_crimewatch_state
            .as_inner_mut()
            .duel_challenges
            .insert((other_id, char_id), now + 60);
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
    assert!(!(has_limited_engagement_with(&ctx, char_id, other_id, now)
        || has_limited_engagement_with(&ctx, other_id, char_id, now)));

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
        ctx.db.with_mut_snapshot(|snap| {
            snap.combat_crimewatch_state
                .as_inner_mut()
                .duel_challenges
                .insert((other_id, char_id), now + 60);
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
        assert!(!has_limited_engagement_with(&ctx, char_id, other_id, now));
    }

    // 3. V24.01 3-arg accept: (fromCharID, expiryTime, true) creates mutual LE
    make_duel_peer_online_and_undocked(&ctx, &mut sess, char_id, other_id, now);
    ctx.db.with_mut_snapshot(|snap| {
        snap.combat_crimewatch_state
            .as_inner_mut()
            .duel_challenges
            .insert((other_id, char_id), now + 60);
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
    assert!(
        has_limited_engagement_with(&ctx, char_id, other_id, now)
            && has_limited_engagement_with(&ctx, other_id, char_id, now)
    );
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
    crate::services::combat_crimewatch::set_char_safety_level(&ctx, char_id, SafetyLevel::Green);
    assert!(
        evaluate_and_record_pvp_aggression(&ctx, char_id, ship_id, sys_id, victim_ship_id, false)
            .is_err()
    );

    // Red safety allows aggression and flags criminal in highsec
    crate::services::combat_crimewatch::set_char_safety_level(&ctx, char_id, SafetyLevel::Red);
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
    crate::services::combat_crimewatch::set_char_safety_level(&ctx, victim_id, SafetyLevel::Green);
    assert!(
        evaluate_and_record_remote_assistance(&ctx, victim_id, victim_ship_id, sys_id, ship_id)
            .is_err()
    );
    crate::services::combat_crimewatch::set_char_safety_level(&ctx, victim_id, SafetyLevel::Red);
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
    });
    ctx.db.with_mut_snapshot(|snap| {
        let cw = snap.combat_crimewatch_state.as_inner_mut();
        cw.duel_challenges.insert((char_id, target_id), now + 10);
        cw.limited_engagements
            .entry(char_id)
            .or_default()
            .insert(target_id, now + 10);
        cw.kill_right_expires_unix.insert(5001, now + 10);
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
    });
    ctx.db.with_snapshot(|snap| {
        let cw = &snap.combat_crimewatch_state;
        assert!(!cw.duel_challenges.contains_key(&(char_id, target_id)));
        assert!(!cw.limited_engagements.contains_key(&char_id));
        assert!(!cw.kill_right_expires_unix.contains_key(&5001));
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

#[test]
fn war_interference_assistance_honors_ally_expiry_and_alliance_allies() {
    use eve_db::{
        AllianceRecord, CorporationRecord, PersistedNegKind, PersistedWarNegotiation, WarRecord,
    };

    let (ctx, _sess, def_char, _ship_id, _sys_id) = make_fighter_ctx_and_session();
    let corp_ally_char = 90_000_302_i64;
    let alliance_ally_char = 90_000_303_i64;
    let neutral_char = 90_000_304_i64;
    let agg_corp = 98_300_001_i64;
    let def_corp = 98_300_002_i64;
    let corp_ally = 98_300_003_i64;
    let ext_alliance = 99_300_004_i64;
    let ext_member_corp = 98_300_004_i64;
    let neutral_corp = 98_300_005_i64;
    let war_id = 700_300_001_i64;
    let now = 1_750_000_000_u64;
    let eff = now + 100;
    let exp = now + 500;

    ctx.db.with_mut_snapshot(|snap| {
        let base = snap.characters.get(&def_char).cloned().expect("base char");
        if let Some(ch) = snap.characters.get_mut(&def_char) {
            ch.corporation_id = def_corp;
            ch.alliance_id = None;
        }
        for (cid, corp_id) in [
            (corp_ally_char, corp_ally),
            (alliance_ally_char, ext_member_corp),
            (neutral_char, neutral_corp),
        ] {
            let mut ch = base.clone();
            ch.character_id = cid;
            ch.corporation_id = corp_id;
            ch.alliance_id = None;
            snap.characters.insert(cid, ch);
        }
        for (cid, alliance_opt) in [
            (agg_corp, None),
            (def_corp, None),
            (corp_ally, None),
            (ext_member_corp, Some(ext_alliance)),
            (neutral_corp, None),
        ] {
            let mut corp = CorporationRecord::default();
            corp.corporation_id = cid;
            corp.alliance_id = alliance_opt;
            snap.corporations.insert(cid, corp);
        }
        snap.alliances.insert(
            ext_alliance,
            AllianceRecord {
                alliance_id: ext_alliance,
                name: "Ally Alliance".into(),
                short_name: "ALYA".into(),
                executor_corp_id: ext_member_corp,
                member_corp_ids: [ext_member_corp].into_iter().collect(),
            },
        );
        snap.wars.insert(
            war_id,
            WarRecord {
                war_id,
                aggressor_corp_id: agg_corp,
                defender_corp_id: def_corp,
                mutual: true,
                active: true,
                started_unix: now,
            },
        );
        snap.war_state.negotiations.push(PersistedWarNegotiation {
            id: 21,
            war_id,
            kind: PersistedNegKind::Ally,
            from_corp: corp_ally,
            isk: IskCents::ZERO,
            message: String::new(),
            status: "Accepted".into(),
            effective_unix: Some(eff),
        });
        snap.war_state.negotiations.push(PersistedWarNegotiation {
            id: 22,
            war_id,
            kind: PersistedNegKind::Ally,
            from_corp: ext_alliance,
            isk: IskCents::ZERO,
            message: String::new(),
            status: "Accepted".into(),
            effective_unix: Some(eff),
        });
        snap.war_state.ally_expires_unix.insert((war_id, corp_ally), exp);
        snap.war_state
            .ally_expires_unix
            .insert((war_id, ext_alliance), exp);
    });

    {
        let mut cw = ctx.crimewatch.write().unwrap();
        for cid in [def_char, corp_ally_char, alliance_ally_char] {
            cw.get_or_create(cid).pvp_timer_until_unix = exp + 1_000;
        }
    }

    // Before effective_unix: allies are not yet active on the defender's side.
    assert!(is_war_interference_assistance(&ctx, corp_ally_char, def_char, eff - 1));
    assert!(is_war_interference_assistance(&ctx, alliance_ally_char, def_char, eff - 1));
    assert!(!is_war_interference_assistance(&ctx, neutral_char, corp_ally_char, eff - 1));

    // Active window [eff, exp): allies may assist defender legally; neutrals assisting allies interfere.
    for ts in [eff, exp - 1] {
        assert!(!is_war_interference_assistance(&ctx, corp_ally_char, def_char, ts));
        assert!(!is_war_interference_assistance(&ctx, alliance_ally_char, def_char, ts));
        assert!(is_war_interference_assistance(&ctx, neutral_char, corp_ally_char, ts));
        assert!(is_war_interference_assistance(&ctx, neutral_char, alliance_ally_char, ts));
    }

    // At and after ally_expires_unix: expired allies trigger interference when assisting defender,
    // and neutral pilots assisting expired allies no longer interfere.
    for ts in [exp, exp + 60] {
        assert!(is_war_interference_assistance(&ctx, corp_ally_char, def_char, ts));
        assert!(is_war_interference_assistance(&ctx, alliance_ally_char, def_char, ts));
        assert!(!is_war_interference_assistance(&ctx, neutral_char, corp_ally_char, ts));
        assert!(!is_war_interference_assistance(&ctx, neutral_char, alliance_ally_char, ts));
    }
}

#[tokio::test]
async fn crimewatch_timers_scale_sim_expiries_under_tidi_and_reemit_on_tidi_change() {
    use crate::services::serverops_cluster::{
        clear_manual_system_tidi_override, set_system_tidi_factor,
    };
    use eve_db::clock::{SimFileTime, VirtualServerClock, FILETIME_TICKS_PER_SEC};

    let base = 1_800_000_000_u64;
    let (ctx, _old_sess, char_id, _ship_id, sys_id) = make_fighter_ctx_and_session();
    ctx.set_clock(std::sync::Arc::new(VirtualServerClock::with_fixed_base(base)));
    let idle_char_id = 90_000_009_i64;
    ctx.db.with_mut_snapshot(|snap| {
        let mut acc = snap
            .accounts
            .values()
            .find(|a| a.user_id == 1)
            .expect("account 1")
            .clone();
        acc.user_id = 9;
        acc.username = "idle_pilot_9".into();
        snap.accounts.insert(acc.username.clone(), acc);

        let mut ch = snap.characters[&char_id].clone();
        ch.character_id = idle_char_id;
        ch.user_id = 9;
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = None;
        snap.characters.insert(idle_char_id, ch);
    });
    ctx.db
        .acquire_online_lock(1, 501, Some(char_id), true, base)
        .expect("fighter online lock");
    ctx.db
        .acquire_online_lock(9, 509, Some(idle_char_id), true, base)
        .expect("idle pilot online lock");

    // Start in 0.25x TiDi and drain initial DoSimClockRebase notifications.
    let applied_025 = set_system_tidi_factor(&ctx, sys_id, 0.25);
    assert!((applied_025 - 0.25).abs() < 1e-9);
    let _ = ctx.drain_notifications_for_char(char_id);
    let _ = ctx.drain_notifications_for_char(idle_char_id);

    let weapon_exp = base + 60;
    let pvp_exp = base + 900;
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        let st = cw.get_or_create(char_id);
        st.weapon_timer_until_unix = weapon_exp;
        st.pvp_timer_until_unix = pvp_exp;
    }

    let sim_0: SimFileTime = ctx.peek_system_sim_filetime(sys_id).expect("0.25 TiDi clock");
    emit_crimewatch_timer_notifications(&ctx, char_id, ctx.now_unix());
    let notifs_0 = ctx.drain_notifications_for_char(char_id);
    let w_notif_0 = notifs_0
        .iter()
        .find(|n| n.event == "OnWeaponsTimerUpdate")
        .expect("initial OnWeaponsTimerUpdate");
    assert_eq!(w_notif_0.args[0].as_i64(), Some(101));
    let w_exp_sim_0 = w_notif_0.args[1].as_i64().expect("weapons sim expiry");
    assert_eq!(
        w_exp_sim_0,
        sim_0.as_ticks() + 15 * FILETIME_TICKS_PER_SEC,
        "60s wall weapon timer in 0.25x TiDi must expire at +15s SimFileTime"
    );
    let p_notif_0 = notifs_0
        .iter()
        .find(|n| n.event == "OnPvpTimerUpdate")
        .expect("initial OnPvpTimerUpdate");
    assert_eq!(p_notif_0.args[0].as_i64(), Some(201));
    let p_exp_sim_0 = p_notif_0.args[1].as_i64().expect("pvp sim expiry");
    assert_eq!(p_exp_sim_0, sim_0.as_ticks() + 225 * FILETIME_TICKS_PER_SEC);

    let mut sess = SessionState::default();
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(1_000_009);
    sess.solar_system_id = Some(sys_id);
    let kw = HashMap::new();
    let cs_0 = handle_crimewatch(&ctx, &mut sess, "crimewatch", "GetClientStates", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    let PyRep::Tuple(cs0_parts) = cs_0 else {
        panic!("GetClientStates tuple");
    };
    let PyRep::Tuple(cs0_timers) = &cs0_parts[0] else {
        panic!("GetClientStates timers tuple");
    };
    let PyRep::Tuple(cs0_w) = &cs0_timers[0] else {
        panic!("weapons timer entry");
    };
    assert_eq!(cs0_w[0].as_i64(), Some(101));
    assert_eq!(cs0_w[1].as_i64(), Some(w_exp_sim_0));

    // Advance 20 real wall seconds in 0.25x TiDi -> SimTime advances +5s, 40s wall left (10s sim).
    // Active -> cooldown transition (101 -> 102, 201 -> 202) keeps invariant SimFileTime expiry.
    ctx.advance_secs(20);
    let sim_20 = ctx.peek_system_sim_filetime(sys_id).expect("sim at +20s");
    assert_eq!(sim_20, sim_0.saturating_add_secs(5));
    emit_crimewatch_timer_notifications(&ctx, char_id, ctx.now_unix());
    let notifs_20 = ctx.drain_notifications_for_char(char_id);
    let w_notif_20 = notifs_20
        .iter()
        .find(|n| n.event == "OnWeaponsTimerUpdate")
        .expect("101 -> 102 OnWeaponsTimerUpdate");
    assert_eq!(w_notif_20.args[0].as_i64(), Some(102));
    assert_eq!(
        w_notif_20.args[1].as_i64(),
        Some(w_exp_sim_0),
        "remaining SimFileTime expiry must remain invariant while TiDi holds at 0.25x"
    );
    let p_notif_20 = notifs_20
        .iter()
        .find(|n| n.event == "OnPvpTimerUpdate")
        .expect("201 -> 202 OnPvpTimerUpdate");
    assert_eq!(p_notif_20.args[0].as_i64(), Some(202));
    assert_eq!(p_notif_20.args[1].as_i64(), Some(p_exp_sim_0));

    // Transition TiDi 0.25 -> 0.50: DoSimClockRebase fires and active timers re-emit scaled expiries;
    // idle pilots only receive DoSimClockRebase with zero CrimeWatch timer churn.
    let applied_050 = set_system_tidi_factor(&ctx, sys_id, 0.50);
    assert!((applied_050 - 0.50).abs() < 1e-9);
    let idle_notifs = ctx.drain_notifications_for_char(idle_char_id);
    assert_eq!(
        idle_notifs.iter().filter(|n| n.event == "DoSimClockRebase").count(),
        1
    );
    assert!(
        idle_notifs
            .iter()
            .all(|n| !n.event.ends_with("TimerUpdate")),
        "idle pilot must not receive CrimeWatch timer churn on TiDi change"
    );
    let tidi50_notifs = ctx.drain_notifications_for_char(char_id);
    let rebase_pos = tidi50_notifs
        .iter()
        .position(|n| n.event == "DoSimClockRebase")
        .expect("DoSimClockRebase emitted first");
    let w_pos = tidi50_notifs
        .iter()
        .position(|n| n.event == "OnWeaponsTimerUpdate")
        .expect("active weapons timer re-emitted on TiDi transition");
    assert!(rebase_pos < w_pos);
    // 40s wall left at 0.50x TiDi -> sim_20 + 20s sim ticks.
    assert_eq!(tidi50_notifs[w_pos].args[0].as_i64(), Some(102));
    assert_eq!(
        tidi50_notifs[w_pos].args[1].as_i64(),
        Some(sim_20.as_ticks() + 20 * FILETIME_TICKS_PER_SEC)
    );
    let p_50 = tidi50_notifs
        .iter()
        .find(|n| n.event == "OnPvpTimerUpdate")
        .expect("active pvp timer re-emitted on TiDi transition");
    // 880s wall left at 0.50x TiDi -> sim_20 + 440s sim ticks.
    assert_eq!(p_50.args[0].as_i64(), Some(202));
    assert_eq!(
        p_50.args[1].as_i64(),
        Some(sim_20.as_ticks() + 440 * FILETIME_TICKS_PER_SEC)
    );

    // Advance 20 real wall seconds in 0.50x TiDi -> sim advances +10s (sim_20 + 10s), 20s wall left.
    // Restore TiDi 0.50 -> 1.0: re-emits 102 with sim_40 + 20s and matches GetClientStates.
    ctx.advance_secs(20);
    let sim_40 = ctx.peek_system_sim_filetime(sys_id).expect("sim at +40s");
    assert_eq!(sim_40, sim_20.saturating_add_secs(10));
    let restored = clear_manual_system_tidi_override(&ctx, sys_id);
    assert!((restored - 1.0).abs() < 1e-9);
    let tidi100_notifs = ctx.drain_notifications_for_char(char_id);
    let w_100 = tidi100_notifs
        .iter()
        .find(|n| n.event == "OnWeaponsTimerUpdate")
        .expect("active weapons timer re-emitted when TiDi clears to 1.0");
    let expected_w_100 = sim_40.as_ticks() + 20 * FILETIME_TICKS_PER_SEC;
    assert_eq!(w_100.args[0].as_i64(), Some(102));
    assert_eq!(w_100.args[1].as_i64(), Some(expected_w_100));

    let cs_1 = handle_crimewatch(&ctx, &mut sess, "crimewatch", "GetClientStates", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    let PyRep::Tuple(cs1_parts) = cs_1 else {
        panic!("GetClientStates tuple");
    };
    let PyRep::Tuple(cs1_timers) = &cs1_parts[0] else {
        panic!("GetClientStates timers tuple");
    };
    let PyRep::Tuple(cs1_w) = &cs1_timers[0] else {
        panic!("weapons timer entry");
    };
    assert_eq!(cs1_w[0].as_i64(), Some(102));
    assert_eq!(cs1_w[1].as_i64(), Some(expected_w_100));
}

#[cfg(test)]
#[path = "crimewatch_timers_extra_unit_tests.rs"]
mod crimewatch_timers_extra_unit_tests;
