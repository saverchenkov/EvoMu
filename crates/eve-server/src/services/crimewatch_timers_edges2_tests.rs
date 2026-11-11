//! Additional edge-case and branch-coverage unit tests for `crimewatch_timers.rs`.

#![forbid(unsafe_code)]

use super::check_kill_right_restriction;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::{
    make_fighter_ctx_and_session, make_fighter_item,
};
use crate::services::combat_crimewatch::*;
use eve_db::{InventoryFlag, IskCents, KillRightRecord};
use eve_proto::PyRep;
use std::collections::HashMap;

#[test]
fn faction_police_thresholds_kill_right_restrictions_and_timer_state_matrix() {
    let (ctx, mut sess, char_id, ship_id, hs_sys) = make_fighter_ctx_and_session();
    let now = ctx.now_unix();

    // Exact sec-status threshold steps
    assert_eq!(faction_police_sec_threshold(1.0), Some(-2.0));
    assert_eq!(faction_police_sec_threshold(0.9), Some(-2.5));
    assert_eq!(faction_police_sec_threshold(0.8), Some(-3.0));
    assert_eq!(faction_police_sec_threshold(0.7), Some(-3.5));
    assert_eq!(faction_police_sec_threshold(0.6), Some(-4.0));
    assert_eq!(faction_police_sec_threshold(0.5), Some(-4.5));
    assert_eq!(faction_police_sec_threshold(0.44), None);

    // compute_crimewatch_timer_states: active -> cooldown -> idle transitions
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        let st = cw.get_or_create(char_id);
        st.weapon_timer_until_unix = now + 10;
        st.pvp_timer_until_unix = now + 10;
        st.suspect_flag_until_unix = now + 10;
    }
    let st_active = super::compute_crimewatch_timer_states(&ctx, char_id, now).timers;
    assert_eq!(st_active[0].0, 101);
    assert_eq!(st_active[1].0, 201);
    assert_eq!(st_active[3].0, 302);

    // Seed combat_runtime state via emit_crimewatch_timer_notifications, then advance past trigger -> cooldown (102, 202, 304)
    emit_crimewatch_timer_notifications(&ctx, char_id, now);
    let st_cd = super::compute_crimewatch_timer_states(&ctx, char_id, now + 5).timers;
    assert_eq!(st_cd[0].0, 102);
    assert_eq!(st_cd[1].0, 202);
    assert_eq!(st_cd[3].0, 304);

    //Clear timers (or advance past expiry) so compute enters idle (100, 200, 300)
    ctx.crimewatch.write().unwrap().states.clear();
    let st_idle = super::compute_crimewatch_timer_states(&ctx, char_id, now + 20).timers;
    assert_eq!(st_idle[0].0, 100);
    assert_eq!(st_idle[1].0, 200);
    assert_eq!(st_idle[3].0, 300);

    // Direct emit_crimewatch_timer_notifications_with_state coverage (idle no-op -> active -> cooldown -> clear)
    let idle_st = crate::combat::CharacterCrimeWatchState::default();
    emit_crimewatch_timer_notifications(&ctx, char_id, now + 20);
    assert!(with_combat_runtime(&ctx, |rt| !rt.crimewatch_timer_states.contains_key(&char_id)));
    emit_crimewatch_timer_notifications_with_state(&ctx, char_id, &idle_st, now + 20);
    assert!(with_combat_runtime(&ctx, |rt| !rt.crimewatch_timer_states.contains_key(&char_id)));

    let active_st = crate::combat::CharacterCrimeWatchState {
        character_id: char_id,
        weapon_timer_until_unix: now + 80,
        pvp_timer_until_unix: now + 900,
        criminal_flag_until_unix: now + 900,
        suspect_flag_until_unix: 0,
        pending_concord_arrival: None,
    };
    emit_crimewatch_timer_notifications_with_state(&ctx, char_id, &active_st, now + 25);
    let sig_active = with_combat_runtime(&ctx, |rt| rt.crimewatch_timer_states[&char_id]);
    assert_eq!(sig_active.0 >> 40, 101);
    assert_eq!(sig_active.1 >> 40, 201);
    assert_eq!(sig_active.3 >> 40, 301);

    emit_crimewatch_timer_notifications_with_state(&ctx, char_id, &active_st, now + 30);
    let sig_cd = with_combat_runtime(&ctx, |rt| rt.crimewatch_timer_states[&char_id]);
    assert_eq!(sig_cd.0 >> 40, 102);
    assert_eq!(sig_cd.1 >> 40, 202);
    assert_eq!(sig_cd.3 >> 40, 303);

    emit_crimewatch_timer_notifications_with_state(&ctx, char_id, &idle_st, now + 1_000);
    assert!(with_combat_runtime(&ctx, |rt| !rt.crimewatch_timer_states.contains_key(&char_id)));

    // check_kill_right_restriction: char match, corp match, alliance match, mismatch
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.corporation_id = 98_600_001;
            c.alliance_id = Some(99_600_001);
        }
        snap.corporations.insert(
            98_600_001,
            { let mut __corp = eve_db::CorporationRecord::default(); __corp.corporation_id = 98_600_001; __corp.name = "Restriction Test Corp".into(); __corp.ticker = "RSTC".into(); __corp.ceo_id = char_id; __corp.alliance_id = Some(99_600_001); __corp.ensure_wallet_divisions(); __corp },
        );
        snap.kill_rights.insert(
            801,
            KillRightRecord {
                kill_right_id: 801,
                owner_id: 90_000_099,
                target_id: 90_000_088,
                activation_fee: IskCents(0),
                is_active_suspect: false,
            },
        );
        snap.combat_crimewatch_state
            .kill_right_restricted_to
            .insert(801, char_id);
    });
    assert!(ctx
        .db
        .with_snapshot(|s| check_kill_right_restriction(s, 801, char_id).is_ok()));

    ctx.db.with_mut_snapshot(|snap| {
        snap.combat_crimewatch_state
            .kill_right_restricted_to
            .insert(801, 98_600_001);
    });
    assert!(ctx
        .db
        .with_snapshot(|s| check_kill_right_restriction(s, 801, char_id).is_ok()));

    ctx.db.with_mut_snapshot(|snap| {
        snap.combat_crimewatch_state
            .kill_right_restricted_to
            .insert(801, 99_600_001);
    });
    assert!(ctx
        .db
        .with_snapshot(|s| check_kill_right_restriction(s, 801, char_id).is_ok()));

    ctx.db.with_mut_snapshot(|snap| {
        snap.combat_crimewatch_state
            .kill_right_restricted_to
            .insert(801, 99_999_999);
    });
    assert_eq!(
        ctx.db
            .with_snapshot(|s| check_kill_right_restriction(s, 801, char_id)
                .unwrap_err()
                .code),
        "KillRightRestricted"
    );

    // Capsule exempt from highsec criminal warp/wormhole block
    let pod_id = 888_670_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            pod_id,
            make_fighter_item(pod_id, 670, char_id, hs_sys as i64, InventoryFlag::None, 1),
        );
    });
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        let st = cw.get_or_create(char_id);
        st.criminal_flag_until_unix = now + 900;
        st.pending_concord_arrival = None;
    }
    assert!(
        check_crimewatch_action_restriction(&ctx, char_id, pod_id, hs_sys, "warp", now).is_ok()
    );
    assert!(check_crimewatch_action_restriction(
        &ctx,
        char_id,
        pod_id,
        hs_sys,
        "wormhole_jump",
        now
    )
    .is_ok());
    // Ship under criminal flag IS blocked from eject / dock / stargate_jump
    for act in [
        "eject",
        "dock",
        "stargate_jump",
        "store_ship",
        "switch_ship",
        "jump_drive",
    ] {
        assert!(
            check_crimewatch_action_restriction(&ctx, char_id, ship_id, hs_sys, act, now).is_err()
        );
    }
    let _ = &mut sess;
}

#[tokio::test]
async fn kill_right_buy_activate_errors_set_safety_duel_accept_and_pvp_remote_assist_edges() {
    let (ctx, mut sess, char_id, ship_id, hs_sys) = make_fighter_ctx_and_session();
    let owner_id = 90_000_002_i64;
    let target_id = 90_000_003_i64;
    let kw = HashMap::new();
    let now = ctx.now_unix();

    ctx.db.with_mut_snapshot(|snap| {
        let base = snap.characters[&char_id].clone();
        snap.characters.get_mut(&char_id).unwrap().balance = IskCents::from_isk(20_000_000);
        snap.characters.get_mut(&char_id).unwrap().station_id = None;
        for (cid, corp) in [(owner_id, 98_600_002_i64), (target_id, 98_600_003_i64)] {
            let mut ch = base.clone();
            ch.character_id = cid;
            ch.user_id = cid as u64;
            ch.corporation_id = corp;
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = hs_sys;
            ch.balance = IskCents::from_isk(1_000_000);
            snap.characters.insert(cid, ch);
        }
        // KR 901: owned by char_id, target docked -> KillRightCannotActivateDocked (after target set docked)
        // KR 902: owned by owner_id, for sale at 5M against target_id
        // KR 903: owned by owner_id, not for sale (0)
        // KR 904: already active suspect
        snap.kill_rights.insert(
            901,
            KillRightRecord {
                kill_right_id: 901,
                owner_id: char_id,
                target_id,
                activation_fee: IskCents(0),
                is_active_suspect: false,
            },
        );
        snap.kill_rights.insert(
            902,
            KillRightRecord {
                kill_right_id: 902,
                owner_id,
                target_id,
                activation_fee: IskCents::from_isk(5_000_000),
                is_active_suspect: false,
            },
        );
        snap.kill_rights.insert(
            903,
            KillRightRecord {
                kill_right_id: 903,
                owner_id,
                target_id,
                activation_fee: IskCents(0),
                is_active_suspect: false,
            },
        );
        snap.kill_rights.insert(
            904,
            KillRightRecord {
                kill_right_id: 904,
                owner_id: char_id,
                target_id,
                activation_fee: IskCents(0),
                is_active_suspect: true,
            },
        );
        for kr in [901, 902, 903, 904] {
            snap.combat_crimewatch_state
                .kill_right_expires_unix
                .insert(kr, now + 86_400);
        }
    });

    // Unhandled RPC returns None
    assert!(
        handle_crimewatch(&ctx, &mut sess, "charMgr", "GetCharacterInfo", &[], &kw)
            .await
            .is_none()
    );

    // GetClientStates + SetSafetyLevel (0/1/2 + invalid)
    assert!(
        handle_crimewatch(&ctx, &mut sess, "crimewatch", "GetClientStates", &[], &kw)
            .await
            .unwrap()
            .is_ok()
    );
    assert_eq!(
        handle_crimewatch(
            &ctx,
            &mut sess,
            "crimewatch",
            "SetSafetyLevel",
            &[PyRep::Integer(0)],
            &kw
        )
        .await
        .unwrap()
        .unwrap()
        .dict_get("safetyLevel")
        .and_then(|v| v.as_i64()),
        Some(0)
    );
    assert_eq!(
        handle_crimewatch(
            &ctx,
            &mut sess,
            "crimewatch",
            "SetSafetyLevel",
            &[PyRep::Integer(2)],
            &kw
        )
        .await
        .unwrap()
        .unwrap()
        .dict_get("safetyLevel")
        .and_then(|v| v.as_i64()),
        Some(2)
    );

    // ActivateKillRight errors: AlreadyActivated, NotOwner, CannotActivateDocked, TargetNotInSystem
    assert_eq!(
        handle_crimewatch(
            &ctx,
            &mut sess,
            "killRightMgr",
            "ActivateKillRight",
            &[PyRep::Integer(904)],
            &kw
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "KillRightAlreadyActivated"
    );
    assert_eq!(
        handle_crimewatch(
            &ctx,
            &mut sess,
            "killRightMgr",
            "ActivateKillRight",
            &[PyRep::Integer(903)],
            &kw
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "NotKillRightOwner"
    );
    sess.station_id = Some(60_003_760);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&target_id).unwrap().station_id = Some(60_003_760);
    });
    assert_eq!(
        handle_crimewatch(
            &ctx,
            &mut sess,
            "killRightMgr",
            "ActivateKillRight",
            &[PyRep::Integer(901)],
            &kw
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "KillRightCannotActivateDocked"
    );
    sess.station_id = None;
    ctx.db.with_mut_snapshot(|snap| {
        let t = snap.characters.get_mut(&target_id).unwrap();
        t.station_id = None;
        t.solar_system_id = 30_002_537;
    });
    assert_eq!(
        handle_crimewatch(
            &ctx,
            &mut sess,
            "killRightMgr",
            "ActivateKillRight",
            &[PyRep::Integer(901)],
            &kw
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "KillRightTargetNotInSystem"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&target_id).unwrap().solar_system_id = hs_sys;
    });

    // BuyKillRight: NotForSale (903), AlreadyOwner (901), PriceChanged, InsufficientFunds, Success (902)
    assert_eq!(
        handle_crimewatch(
            &ctx,
            &mut sess,
            "killRightMgr",
            "BuyKillRight",
            &[PyRep::Integer(903), PyRep::Real(1.0)],
            &kw
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "KillRightNotForSale"
    );
    assert_eq!(
        handle_crimewatch(
            &ctx,
            &mut sess,
            "killRightMgr",
            "BuyKillRight",
            &[PyRep::Integer(901), PyRep::Real(0.0)],
            &kw
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "AlreadyKillRightOwner"
    );
    assert_eq!(
        handle_crimewatch(
            &ctx,
            &mut sess,
            "killRightMgr",
            "BuyKillRight",
            &[PyRep::Integer(902), PyRep::Real(1_000_000.0)],
            &kw
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "KillRightPriceChanged"
    );
    let bought = handle_crimewatch(
        &ctx,
        &mut sess,
        "killRightMgr",
        "BuyKillRight",
        &[PyRep::Integer(902), PyRep::Real(5_000_000.0)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        bought.dict_get("suspectFlagActive"),
        Some(&PyRep::Bool(true))
    );
    assert_eq!(
        ctx.db.with_snapshot(|s| s.characters[&owner_id].balance),
        IskCents::from_isk(6_000_000)
    );

    // StartDuelChallenge validation: self, missing arg, RespondToDuelChallenge accept (3-arg FILETIME)
    assert_eq!(
        handle_crimewatch(
            &ctx,
            &mut sess,
            "crimewatch",
            "StartDuelChallenge",
            &[],
            &kw
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidArgument"
    );
    assert_eq!(
        handle_crimewatch(
            &ctx,
            &mut sess,
            "crimewatch",
            "StartDuelChallenge",
            &[PyRep::Integer(char_id)],
            &kw
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidDuelTarget"
    );
    with_mut_combat_runtime(&ctx, |rt| {
        rt.duel_challenges.insert((owner_id, char_id), now + 60);
    });
    let acc = handle_crimewatch(
        &ctx,
        &mut sess,
        "crimewatch",
        "RespondToDuelChallenge",
        &[
            PyRep::Integer(owner_id),
            PyRep::Integer(0),
            PyRep::Bool(true),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(acc.dict_get("accepted"), Some(&PyRep::Bool(true)));

    // precheck_pvp_aggression_safety wormhole exemption (31M/32M) + remote assistance in wormhole
    {
        let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
        cw.get_or_create(target_id).suspect_flag_until_unix = 0;
    }
    assert!(precheck_pvp_aggression_safety(&ctx, char_id, 31_000_001, target_id, true).is_ok());
    assert_eq!(
        evaluate_remote_assistance_legality(&ctx, char_id, target_id, 31_000_001, now),
        ("Legal", false, false)
    );

    // Set Green safety -> evaluate_and_record_pvp_aggression in highsec blocked
    with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(char_id, SafetyLevel::Green);
        rt.limited_engagements.clear();
    });
    assert_eq!(
        evaluate_and_record_pvp_aggression(&ctx, char_id, ship_id, hs_sys, target_id, false)
            .unwrap_err()
            .code,
        "CantDoInCurrentSafetyLevel"
    );
}
