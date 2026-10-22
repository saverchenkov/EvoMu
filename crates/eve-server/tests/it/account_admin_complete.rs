//! Integration tests for `feat:account.*` and `feat:admin.*` rows. Only rows whose every acceptance
//! clause is asserted here are tagged.


use crate::common;

use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

#[tokio::test]
async fn test_account_bans_suspensions_and_gm_ban_live_session_kick() {
    // parity: feat:account.account-bans, feat:admin.gm-ban, rpc:authentication.Login, rpc:charUnboundMgr.SelectCharacterID, rpc:slash.SlashCmd
    eve_testkit::attest(&["feat:account.account-bans", "feat:admin.gm-ban"]);
    let ctx = ServerContext::new();
    // Deterministic virtual time: the suspension expiry assertion must not race the host wall clock.
    ctx.set_clock(std::sync::Arc::new(eve_db::VirtualServerClock::with_fixed_base(1_800_000_000)));
    let kw = HashMap::new();
    let base_now = ctx.now_unix();

    // 1. Live player session logs in and selects character
    let mut player_sess = SessionState::new(30_001);
    let (player_tx, mut player_rx) = tokio::sync::mpsc::unbounded_channel::<(String, String)>();
    ctx.register_session_closer(30_001, player_tx);
    common::login_pilot(&ctx, &mut player_sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut player_sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    // 2. GM logs in on separate account and issues timed /suspend against live player session
    ctx.db
        .provision_gm_account("gm_admin", "gm-pass", 90_000_003, "GM Enforcer");
    let mut gm_sess = SessionState::new(30_002);
    common::login_pilot(&ctx, &mut gm_sess, "gm_admin", "gm-pass").await;
    ctx.dispatch_rpc(
        &mut gm_sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_003)],
        &kw,
        None,
    )
    .await
    .expect("select gm char");

    let suspend_rep = ctx
        .dispatch_rpc(
            &mut gm_sess,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String("/suspend demo 600 Account sharing investigation".into())],
            &kw,
            None,
        )
        .await
        .expect("/suspend demo");
    let PyRep::KeyVal(susp_kv) = suspend_rep else {
        panic!("expected KeyVal from /suspend");
    };
    assert_eq!(susp_kv.get("kickedSessionID").and_then(|v| v.as_i64()), Some(30_001));
    let susp_until = susp_kv
        .get("suspendedUntilUnix")
        .and_then(|v| v.as_i64())
        .expect("suspendedUntilUnix");
    assert!((susp_until - (base_now + 600) as i64).abs() <= 2);
    let (kick_code, kick_msg) = player_rx.try_recv().expect("live session must receive disconnect signal on /suspend");
    assert_eq!(kick_code, "AccountSuspended");
    assert!(kick_msg.contains("Account sharing investigation"));

    // 3. Refused at login while suspended with reason and expiry
    let mut retry_sess = SessionState::new(30_003);
    let susp_err = ctx
        .dispatch_rpc(
            &mut retry_sess,
            "authentication",
            None,
            "Login",
            &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())],
            &kw,
            None,
        )
        .await
        .expect_err("suspended account must be refused at login");
    assert_eq!(susp_err.code, "AccountSuspended");
    assert!(susp_err.message.contains("Account sharing investigation"));
    assert!(susp_err.message.contains(&susp_until.to_string()));

    // 4. Advance clock past suspension expiry -> login succeeds again
    ctx.advance_secs(605);
    let mut post_susp_sess = SessionState::new(30_004);
    let (post_susp_tx, mut post_susp_rx) =
        tokio::sync::mpsc::unbounded_channel::<(String, String)>();
    ctx.register_session_closer(30_004, post_susp_tx);
    common::login_pilot(&ctx, &mut post_susp_sess, "demo", "demo-secret").await;

    // 5. GM issues timed /ban against live session -> kicks live session and refuses login with ban reason & expiry
    let now_after = ctx.now_unix();
    let ban_rep = ctx
        .dispatch_rpc(
            &mut gm_sess,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String("/ban demo 1200 EULA RMT Violation".into())],
            &kw,
            None,
        )
        .await
        .expect("/ban demo");
    let PyRep::KeyVal(ban_kv) = ban_rep else {
        panic!("expected KeyVal from /ban");
    };
    assert_eq!(ban_kv.get("kickedSessionID").and_then(|v| v.as_i64()), Some(30_004));
    let ban_until = ban_kv
        .get("bannedUntilUnix")
        .and_then(|v| v.as_i64())
        .expect("bannedUntilUnix");
    assert!((ban_until - (now_after + 1200) as i64).abs() <= 2);
    let (ban_kick_code, ban_kick_msg) = post_susp_rx.try_recv().expect("live session kicked on /ban");
    assert_eq!(ban_kick_code, "UserBanned");
    assert!(ban_kick_msg.contains("EULA RMT Violation"));

    let mut banned_login_sess = SessionState::new(30_005);
    let ban_err = ctx
        .dispatch_rpc(
            &mut banned_login_sess,
            "authentication",
            None,
            "Login",
            &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())],
            &kw,
            None,
        )
        .await
        .expect_err("banned account must be refused at login");
    assert_eq!(ban_err.code, "UserBanned");
    assert!(ban_err.message.contains("EULA RMT Violation"));
    assert!(ban_err.message.contains(&(now_after + 1200).to_string()));
}

#[tokio::test]
async fn test_account_alpha_omega_switch_and_subscription_time() {
    // parity: feat:account.alpha-omega-switch, feat:account.clone-state-subscription, rpc:charUnboundMgr.SelectCharacterID, rpc:slash.SlashCmd, rpc:subscriptionMgr.GetCloneGrade, rpc:subscriptionMgr.GetSubscriptionTime
    eve_testkit::attest(&["feat:account.alpha-omega-switch", "feat:account.clone-state-subscription"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let char_id = 90_000_001;
    let alt_id = 90_000_002;
    let mut sess = SessionState::new(30_010);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let uid = sess.user_id.expect("demo user id");
    // Second character on the same account (test setup) so "all characters" is observable.
    ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&alt_id).unwrap().user_id = uid);
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None)
        .await
        .expect("SelectCharacterID");

    // Pick, from the SDE alpha clone caps for this race, a skill whose alpha cap is below V.
    let race_id = ctx.db.get_character(char_id).unwrap().race_id;
    let (skill_id, alpha_cap) = ctx
        .sde
        .clone_grades
        .get(&race_id)
        .and_then(|g| g.alpha_skill_caps.iter().filter(|(_, &cap)| cap > 0 && cap < 5).map(|(&t, &c)| (t, c)).min())
        .expect("SDE alpha skill cap below level 5");
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            skill_id,
            eve_db::TrainedSkillRecord { type_id: skill_id, level: 5, skill_points: 256_000.0 * 5.0 },
        );
    });

    // GM sets the account Omega expiry one hour ahead.
    ctx.db.provision_gm_account("gm_billing", "gm-pass", 90_000_003, "GM Billing");
    let mut gm = SessionState::new(30_011);
    common::login_pilot(&ctx, &mut gm, "gm_billing", "gm-pass").await;
    ctx.dispatch_rpc(&mut gm, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_003)], &kw, None)
        .await
        .expect("select gm");
    let now = ctx.now_unix();
    let omega_until = now + 3_600;
    ctx.dispatch_rpc(&mut gm, "slash", None, "SlashCmd", &[PyRep::String(format!("/setomega {uid} {omega_until}"))], &kw, None)
        .await
        .expect("/setomega");

    let sub_rep = ctx
        .dispatch_rpc(&mut sess, "subscriptionMgr", None, "GetSubscriptionTime", &[], &kw, None)
        .await
        .expect("GetSubscriptionTime");
    let PyRep::KeyVal(sub_kv) = sub_rep else { panic!("expected KeyVal") };
    assert_eq!(sub_kv.get("subscriptionEndTime").and_then(|v| v.as_i64()), Some(omega_until as i64));
    assert_eq!(sub_kv.get("cloneGrade").and_then(|v| v.as_i64()), Some(2));
    assert_eq!(sub_kv.get("isOmega").and_then(|v| v.as_bool()), Some(true));
    // The client cannot extend its own subscription by passing an argument.
    let spoof = ctx
        .dispatch_rpc(&mut sess, "subscriptionMgr", None, "GetSubscriptionTime", &[PyRep::Integer((now + 999_999) as i64)], &kw, None)
        .await
        .expect("GetSubscriptionTime with spoofed arg");
    let PyRep::KeyVal(spoof_kv) = spoof else { panic!("expected KeyVal") };
    assert_eq!(spoof_kv.get("subscriptionEndTime").and_then(|v| v.as_i64()), Some(omega_until as i64));
    let omega_rows = skills_rows(&ctx, &mut sess).await;
    assert_eq!(skill_levels(&omega_rows, skill_id), (5, 5), "Omega: trained and active level V");

    // Advance past expiry: subscription RPCs report Alpha and every character on the account downgrades.
    ctx.advance_virtual_clock(3_601);
    let grade_rep = ctx
        .dispatch_rpc(&mut sess, "subscriptionMgr", None, "GetCloneGrade", &[], &kw, None)
        .await
        .expect("GetCloneGrade after lapse");
    let PyRep::KeyVal(grade_kv) = grade_rep else { panic!("expected KeyVal") };
    assert_eq!(grade_kv.get("cloneGrade").and_then(|v| v.as_i64()), Some(1));
    let sub_after = ctx
        .dispatch_rpc(&mut sess, "subscriptionMgr", None, "GetSubscriptionTime", &[], &kw, None)
        .await
        .expect("GetSubscriptionTime after lapse");
    let PyRep::KeyVal(sub_after_kv) = sub_after else { panic!("expected KeyVal") };
    assert_eq!(sub_after_kv.get("isOmega").and_then(|v| v.as_bool()), Some(false));
    assert_eq!(sub_after_kv.get("subscriptionEndTime").and_then(|v| v.as_i64()), Some(omega_until as i64));
    assert_eq!(ctx.db.get_character(char_id).unwrap().clone_grade, 1);
    assert_eq!(ctx.db.get_character(alt_id).unwrap().clone_grade, 1, "all account characters downgrade");

    // Skills are kept, but the alpha-restricted level is inactive.
    let alpha_rows = skills_rows(&ctx, &mut sess).await;
    assert_eq!(skill_levels(&alpha_rows, skill_id), (5, alpha_cap as i64));

    // Renewal reactivates immediately in the same session (no relog).
    let renewed_until = ctx.now_unix() + 30 * 86_400;
    ctx.dispatch_rpc(&mut gm, "slash", None, "SlashCmd", &[PyRep::String(format!("/setomega {uid} {renewed_until}"))], &kw, None)
        .await
        .expect("renew Omega");
    assert_eq!(ctx.db.get_character(alt_id).unwrap().clone_grade, 2);
    let renewed_rows = skills_rows(&ctx, &mut sess).await;
    assert_eq!(skill_levels(&renewed_rows, skill_id), (5, 5), "active level V restored without relog");
}

async fn skills_rows(ctx: &ServerContext, sess: &mut SessionState) -> Vec<Vec<PyRep>> {
    let rep = ctx
        .dispatch_rpc(sess, "skillMgr2", None, "GetSkills", &[], &HashMap::new(), None)
        .await
        .expect("GetSkills");
    let PyRep::Rowset { rows, .. } = rep else { panic!("expected rowset") };
    rows
}

fn skill_levels(rows: &[Vec<PyRep>], skill_id: u32) -> (i64, i64) {
    let row = rows
        .iter()
        .find(|cols| cols.first().and_then(|c| c.as_i64()) == Some(skill_id as i64))
        .expect("skill row present");
    (row[1].as_i64().unwrap(), row[3].as_i64().unwrap())
}

#[tokio::test]
async fn test_account_kiring_disabled_and_accrued_playtime() {
    // parity: feat:account.kiring-china, feat:account.underage-accrued-time, rpc:authentication.AccruedTime, rpc:authentication.AmUnderage, rpc:authentication.Logout, rpc:charUnboundMgr.SelectCharacterID, rpc:kiringMgr.GetEveGuardServerAddress, rpc:kiringMgr.GetKiringConfiguration, rpc:slash.SlashCmd, rpc:userSvc.GetSessionPlaytimeMinutes
    eve_testkit::attest(&["feat:account.kiring-china", "feat:account.underage-accrued-time"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let mut sess = SessionState::new(30_020);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let uid = sess.user_id.unwrap();

    // Kiring (China) auth / anti-addiction paths all report disabled on Tranquility.
    let cfg = ctx
        .dispatch_rpc(&mut sess, "kiringMgr", None, "GetKiringConfiguration", &[], &kw, None)
        .await
        .expect("GetKiringConfiguration");
    let PyRep::KeyVal(k_kv) = cfg else { panic!("expected KeyVal") };
    assert_eq!(k_kv.get("enabled").and_then(|v| v.as_bool()), Some(false));
    assert_eq!(k_kv.get("antiAddictionEnabled").and_then(|v| v.as_bool()), Some(false));
    for m in ["ShouldKickCheater", "IsMonochromeStyleEnabled"] {
        let r = ctx.dispatch_rpc(&mut sess, "kiringMgr", None, m, &[], &kw, None).await.expect(m);
        assert_eq!(r, PyRep::Bool(false), "{m}");
    }
    for m in ["GetAntiAddictionCode", "GetSAuthAid"] {
        let r = ctx.dispatch_rpc(&mut sess, "kiringMgr", None, m, &[], &kw, None).await.expect(m);
        assert_eq!(r.as_i64(), Some(0), "{m}");
    }
    let guard = ctx
        .dispatch_rpc(&mut sess, "kiringMgr", None, "GetEveGuardServerAddress", &[], &kw, None)
        .await
        .expect("GetEveGuardServerAddress");
    assert_eq!(guard, PyRep::None);

    // Accrued time and session playtime follow the server clock and survive logout/login.
    // (The virtual clock also advances with wall time, so allow a couple of seconds of drift.)
    let acc = |rep: PyRep| rep.as_i64().expect("integer accrued time");
    let t0 = acc(ctx.dispatch_rpc(&mut sess, "authentication", None, "AccruedTime", &[], &kw, None).await.expect("AccruedTime"));
    assert!((0..=2).contains(&t0), "fresh session accrued {t0}");
    ctx.advance_virtual_clock(360);
    let t1 = acc(ctx.dispatch_rpc(&mut sess, "authentication", None, "AccruedTime", &[], &kw, None).await.unwrap());
    assert!((360..=t0 + 362).contains(&t1), "accrued after +360s: {t1}");
    let mins = ctx
        .dispatch_rpc(&mut sess, "userSvc", None, "GetSessionPlaytimeMinutes", &[], &kw, None)
        .await
        .expect("GetSessionPlaytimeMinutes");
    assert_eq!(mins.as_i64(), Some(t1 / 60));
    assert_eq!(t1 / 60, 6);
    ctx.dispatch_rpc(&mut sess, "authentication", None, "Logout", &[], &kw, None).await.expect("Logout");
    ctx.advance_virtual_clock(10_000); // offline time does not accrue
    let mut sess2 = SessionState::new(30_021);
    common::login_pilot(&ctx, &mut sess2, "demo", "demo-secret").await;
    ctx.advance_virtual_clock(120);
    let t2 = acc(ctx.dispatch_rpc(&mut sess2, "authentication", None, "AccruedTime", &[], &kw, None).await.unwrap());
    assert!((t1 + 120..=t1 + 124).contains(&t2), "accrued after relog: {t2} (before logout {t1})");
    let mins2 = ctx.dispatch_rpc(&mut sess2, "userSvc", None, "GetSessionPlaytimeMinutes", &[], &kw, None).await.unwrap();
    assert_eq!(mins2.as_i64(), Some(t2 / 60));
    assert_eq!(t2 / 60, 8);

    // AmUnderage reflects the account flag (set by a GM tool).
    let under0 = ctx.dispatch_rpc(&mut sess2, "authentication", None, "AmUnderage", &[], &kw, None).await.unwrap();
    assert_eq!(under0, PyRep::Bool(false));
    ctx.db.provision_gm_account("gm_age", "gm-pass", 90_000_003, "GM Age");
    let mut gm = SessionState::new(30_022);
    common::login_pilot(&ctx, &mut gm, "gm_age", "gm-pass").await;
    ctx.dispatch_rpc(&mut gm, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_003)], &kw, None)
        .await
        .unwrap();
    ctx.dispatch_rpc(&mut gm, "slash", None, "SlashCmd", &[PyRep::String(format!("/setunderage {uid} 1"))], &kw, None)
        .await
        .expect("/setunderage");
    let under1 = ctx.dispatch_rpc(&mut sess2, "authentication", None, "AmUnderage", &[], &kw, None).await.unwrap();
    assert_eq!(under1, PyRep::Bool(true));
}

#[tokio::test]
async fn test_account_redeem_tokens_claim_to_character_station() {
    // parity: feat:account.redeem-tokens, rpc:charUnboundMgr.SelectCharacterID, rpc:userSvc.ClaimRedeemTokens, rpc:userSvc.CreateRedeemTokenQA, rpc:userSvc.GetRedeemTokens
    eve_testkit::attest(&["feat:account.redeem-tokens"]);
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    ctx.db.provision_gm_account("gm_gifts", "gm-pass", 90_000_003, "GM Gifts");
    let mut gm = SessionState::new(30_030);
    common::login_pilot(&ctx, &mut gm, "gm_gifts", "gm-pass").await;
    ctx.dispatch_rpc(&mut gm, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_003)], &kw, None)
        .await
        .unwrap();
    let gm_char = ctx.db.get_character(90_000_003).unwrap();
    let dest_station = gm_char.station_id.or(gm_char.structure_id).unwrap_or(gm_char.home_station_id);

    // Players cannot mint tokens.
    let mut player = SessionState::new(30_031);
    common::login_pilot(&ctx, &mut player, "demo", "demo-secret").await;
    let denied = ctx
        .dispatch_rpc(&mut player, "userSvc", None, "CreateRedeemTokenQA", &[PyRep::Integer(34), PyRep::Integer(10)], &kw, None)
        .await
        .expect_err("non-GM CreateRedeemTokenQA");
    assert_eq!(denied.code, "InsufficientRole");

    let tok = ctx
        .dispatch_rpc(&mut gm, "userSvc", None, "CreateRedeemTokenQA", &[PyRep::Integer(34), PyRep::Integer(500)], &kw, None)
        .await
        .expect("CreateRedeemTokenQA");
    let PyRep::KeyVal(tok_kv) = tok else { panic!("expected KeyVal") };
    let tok_id = tok_kv.get("tokenID").and_then(|v| v.as_i64()).unwrap();

    // Pending redeemables are listed with type and quantity.
    let list = ctx.dispatch_rpc(&mut gm, "userSvc", None, "GetRedeemTokens", &[], &kw, None).await.unwrap();
    let PyRep::List(toks) = list else { panic!("expected List") };
    assert_eq!(toks.len(), 1);
    let PyRep::KeyVal(t0) = &toks[0] else { panic!("expected KeyVal") };
    assert_eq!(t0.get("tokenID").and_then(|v| v.as_i64()), Some(tok_id));
    assert_eq!(t0.get("typeID").and_then(|v| v.as_i64()), Some(34));
    assert_eq!(t0.get("quantity").and_then(|v| v.as_i64()), Some(500));
    // Other accounts do not see this account's tokens.
    let other_list = ctx.dispatch_rpc(&mut player, "userSvc", None, "GetRedeemTokens", &[], &kw, None).await.unwrap();
    assert_eq!(other_list, PyRep::List(vec![]));

    // Claiming onto a character of another account is refused.
    let cross = ctx
        .dispatch_rpc(&mut gm, "userSvc", None, "ClaimRedeemTokens", &[PyRep::List(vec![PyRep::Integer(tok_id)]), PyRep::Integer(90_000_001)], &kw, None)
        .await
        .expect_err("cross-account claim");
    assert_eq!(cross.code, "CharacterOwnershipMismatch");

    // Claim to the selected character: item delivered to its station hangar.
    let claim = ctx
        .dispatch_rpc(&mut gm, "userSvc", None, "ClaimRedeemTokens", &[PyRep::List(vec![PyRep::Integer(tok_id)]), PyRep::Integer(90_000_003)], &kw, None)
        .await
        .expect("ClaimRedeemTokens");
    let PyRep::KeyVal(ckv) = claim else { panic!("expected KeyVal") };
    assert_eq!(ckv.get("stationID").and_then(|v| v.as_i64()), Some(dest_station));
    let Some(PyRep::List(created)) = ckv.get("createdItemIDs") else { panic!("createdItemIDs") };
    assert_eq!(created.len(), 1);
    let item = ctx.db.with_snapshot(|s| s.items.get(&created[0].as_i64().unwrap()).cloned()).expect("item");
    assert_eq!((item.owner_id, item.type_id, item.quantity, item.location_id), (90_000_003, 34, 500, dest_station));
    assert_eq!(item.flag, eve_db::InventoryFlag::Hangar);

    // Token is consumed.
    let after = ctx.dispatch_rpc(&mut gm, "userSvc", None, "GetRedeemTokens", &[], &kw, None).await.unwrap();
    assert_eq!(after, PyRep::List(vec![]));
    let again = ctx
        .dispatch_rpc(&mut gm, "userSvc", None, "ClaimRedeemTokens", &[PyRep::List(vec![PyRep::Integer(tok_id)]), PyRep::Integer(90_000_003)], &kw, None)
        .await
        .expect_err("double claim");
    assert_eq!(again.code, "RedeemTokenNotFound");
}

#[tokio::test]
async fn test_account_logout_saves_location_and_removes_ship_from_space() {
    // parity: feat:account.logout, rpc:authentication.Logout, rpc:charUnboundMgr.SelectCharacterID
    eve_testkit::attest(&["feat:account.logout"]);
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let char_id = 90_000_001;
    let mut sess = SessionState::new(30_040);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let uid = sess.user_id.unwrap();
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None)
        .await
        .unwrap();

    // Undock: the character is now in space in its station's system.
    let in_space_sys = sess.solar_system_id.expect("docked system");
    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None).await.expect("Undock");
    assert_eq!(sess.station_id, None);
    let ship_id = ctx.db.get_character(char_id).unwrap().active_ship_id;
    let handle = ctx.systems.get_or_spawn(in_space_sys);
    assert!(handle.get_ball_snapshot(ship_id).await.unwrap().is_some(), "ship ball in space after undock");

    let rep = ctx
        .dispatch_rpc(&mut sess, "authentication", None, "Logout", &[], &kw, None)
        .await
        .expect("Logout");
    let PyRep::KeyVal(kv) = rep else { panic!("expected KeyVal") };
    assert_eq!(kv.get("loggedOut").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(kv.get("removedFromSpace").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(kv.get("savedSolarSystemID").and_then(|v| v.as_i64()), Some(in_space_sys as i64));
    // Location persisted, ship removed from space, session ended.
    let ch = ctx.db.get_character(char_id).unwrap();
    assert_eq!(ch.solar_system_id, in_space_sys);
    assert_eq!(ch.station_id, None);
    assert!(handle.get_ball_snapshot(ship_id).await.unwrap().is_none());
    assert_eq!(sess.user_id, None);
    assert_eq!(sess.character_id, None);
    assert!(ctx.db.get_online_session(uid).is_none(), "online lock released");
    // An ended session can no longer call character-bound services.
    let after = ctx.dispatch_rpc(&mut sess, "skillMgr2", None, "GetSkills", &[], &kw, None).await;
    assert!(after.is_err());
    // Next login resumes at the saved in-space location.
    let mut again = SessionState::new(30_041);
    common::login_pilot(&ctx, &mut again, "demo", "demo-secret").await;
    ctx.dispatch_rpc(&mut again, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None)
        .await
        .unwrap();
    assert_eq!(again.solar_system_id, Some(in_space_sys));
    assert_eq!(again.station_id, None);
}

#[tokio::test]
async fn test_petitions_player_and_gm_flow() {
    // parity: feat:account.petitions, feat:admin.gm-petitions, rpc:charUnboundMgr.SelectCharacterID, rpc:petitioner.AddPetitionRating, rpc:petitioner.CancelPetition, rpc:petitioner.ClaimPetition, rpc:petitioner.CreatePetition, rpc:petitioner.GetLog, rpc:petitioner.GetMyPetitionsEx, rpc:petitioner.GetPetitionQueue
    eve_testkit::attest(&["feat:account.petitions", "feat:admin.gm-petitions"]);
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let player_cid = 90_000_001;
    let mut player = SessionState::new(30_050);
    common::login_pilot(&ctx, &mut player, "demo", "demo-secret").await;
    ctx.dispatch_rpc(&mut player, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(player_cid)], &kw, None)
        .await
        .unwrap();

    let create = |subject: &'static str| {
        vec![PyRep::String(subject.into()), PyRep::String("details".into()), PyRep::Integer(1)]
    };
    let p1 = ctx.dispatch_rpc(&mut player, "petitioner", None, "CreatePetition", &create("Ship stuck in gate"), &kw, None).await.unwrap();
    let PyRep::KeyVal(p1kv) = p1 else { panic!("expected KeyVal") };
    let pet_id = p1kv.get("petitionID").and_then(|v| v.as_i64()).unwrap();
    let p2 = ctx.dispatch_rpc(&mut player, "petitioner", None, "CreatePetition", &create("Wrong petition"), &kw, None).await.unwrap();
    let PyRep::KeyVal(p2kv) = p2 else { panic!("expected KeyVal") };
    let pet2_id = p2kv.get("petitionID").and_then(|v| v.as_i64()).unwrap();
    let empty_subject = ctx
        .dispatch_rpc(&mut player, "petitioner", None, "CreatePetition", &[PyRep::String("  ".into())], &kw, None)
        .await
        .expect_err("empty subject");
    assert_eq!(empty_subject.code, "InvalidPetitionSubject");

    // Player lists own petitions and cancels one.
    let mine = ctx.dispatch_rpc(&mut player, "petitioner", None, "GetMyPetitionsEx", &[], &kw, None).await.unwrap();
    let PyRep::List(mine) = mine else { panic!("expected List") };
    assert_eq!(mine.len(), 2);
    ctx.dispatch_rpc(&mut player, "petitioner", None, "CancelPetition", &[PyRep::Integer(pet2_id)], &kw, None)
        .await
        .expect("CancelPetition");
    let mine = ctx.dispatch_rpc(&mut player, "petitioner", None, "GetMyPetitionsEx", &[], &kw, None).await.unwrap();
    let PyRep::List(mine) = mine else { panic!("expected List") };
    let status_of = |rows: &[PyRep], id: i64| {
        rows.iter()
            .find_map(|r| match r {
                PyRep::KeyVal(kv) if kv.get("petitionID").and_then(|v| v.as_i64()) == Some(id) => {
                    kv.get("status").and_then(|v| v.as_str()).map(str::to_string)
                }
                _ => None,
            })
            .expect("petition row")
    };
    assert_eq!(status_of(&mine, pet2_id), "cancelled");
    assert_eq!(status_of(&mine, pet_id), "open");

    // Players cannot use GM petition actions; cannot rate an open petition.
    let not_gm = ctx
        .dispatch_rpc(&mut player, "petitioner", None, "ClaimPetition", &[PyRep::Integer(pet_id)], &kw, None)
        .await
        .expect_err("player ClaimPetition");
    assert_eq!(not_gm.code, "InsufficientRole");
    let early_rate = ctx
        .dispatch_rpc(&mut player, "petitioner", None, "AddPetitionRating", &[PyRep::Integer(pet_id), PyRep::Integer(9)], &kw, None)
        .await
        .expect_err("rating before close");
    assert_eq!(early_rate.code, "PetitionNotClosed");

    // GM claims, escalates, comments and closes.
    ctx.db.provision_gm_account("gm_ops", "gm-pass", 90_000_003, "GM Support");
    let mut gm = SessionState::new(30_051);
    common::login_pilot(&ctx, &mut gm, "gm_ops", "gm-pass").await;
    ctx.dispatch_rpc(&mut gm, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_003)], &kw, None)
        .await
        .unwrap();
    let queue = ctx.dispatch_rpc(&mut gm, "petitioner", None, "GetPetitionQueue", &[], &kw, None).await.unwrap();
    let PyRep::List(queue) = queue else { panic!("expected List") };
    assert_eq!(queue.len(), 1, "cancelled petitions leave the GM queue");
    let gm_step = |m: &'static str, extra: Option<&'static str>| {
        let mut a = vec![PyRep::Integer(pet_id)];
        if let Some(e) = extra {
            a.push(PyRep::String(e.into()));
        }
        (m, a)
    };
    for (m, a) in [
        gm_step("ClaimPetition", None),
        gm_step("EscalatePetition", Some("Needs senior GM")),
        gm_step("PetitioneeChat", Some("Moved your ship 10km off gate.")),
        gm_step("ClosePetition", Some("Resolved")),
    ] {
        ctx.dispatch_rpc(&mut gm, "petitioner", None, m, &a, &kw, None).await.expect(m);
    }

    // Player sees the log of every step and rates the closed petition.
    let log = ctx.dispatch_rpc(&mut player, "petitioner", None, "GetLog", &[PyRep::Integer(pet_id)], &kw, None).await.unwrap();
    let PyRep::List(log) = log else { panic!("expected List") };
    let events: Vec<String> = log
        .iter()
        .filter_map(|e| match e {
            PyRep::KeyVal(kv) => kv.get("event").and_then(|v| v.as_str()).map(str::to_string),
            _ => None,
        })
        .collect();
    assert_eq!(
        events,
        vec!["Created", "Claimed", "Escalated:Needs senior GM", "GMComment:Moved your ship 10km off gate.", "Closed:Resolved"]
    );
    let rate = ctx
        .dispatch_rpc(&mut player, "petitioner", None, "AddPetitionRating", &[PyRep::Integer(pet_id), PyRep::Integer(10), PyRep::String("Fast help!".into())], &kw, None)
        .await
        .expect("AddPetitionRating");
    let PyRep::KeyVal(rkv) = rate else { panic!("expected KeyVal") };
    assert_eq!(rkv.get("rating").and_then(|v| v.as_i64()), Some(10));
    let mine = ctx.dispatch_rpc(&mut player, "petitioner", None, "GetMyPetitionsEx", &[], &kw, None).await.unwrap();
    let PyRep::List(mine) = mine else { panic!("expected List") };
    assert_eq!(status_of(&mine, pet_id), "closed");
}
