//! Deep branch and mutation-killing unit tests for `auth_cache`.

#![forbid(unsafe_code)]

use crate::services::account_services::account_services_tests::{auth_session, test_ctx};
use crate::services::auth_cache::*;
use eve_db::AccountRecord;
use eve_machonet::{service_roles, SessionState};
use eve_proto::PyRep;
use std::collections::HashMap;

#[test]
fn required_slash_role_and_gm_audit_cap_cover_all_branches() {
    let qa_gm = service_roles::ROLE_QA | service_roles::ROLE_GM_MASK;
    for cmd in [
        "/online",
        "/location",
        "/getattr 587",
        "/survey",
        "/moveme 30000142",
        "/translocate 30000142",
        "/whoami",
        "   /whoami  ",
    ] {
        assert_eq!(required_slash_role(cmd), qa_gm, "cmd {cmd}");
    }
    assert_eq!(required_slash_role("/WHOAMI"), service_roles::ROLE_GM_MASK);

    let spawn_gm = service_roles::ROLE_SPAWN | service_roles::ROLE_GM_MASK;
    for cmd in ["/spawn 587", "/unspawn 1", "/despawn 1", "/spawnnpc 587"] {
        assert_eq!(required_slash_role(cmd), spawn_gm, "cmd {cmd}");
    }

    let high_gm = service_roles::ROLE_GMH
        | service_roles::ROLE_GMS
        | service_roles::ROLE_ADMIN
        | service_roles::ROLE_ROLEADMIN;
    for cmd in ["/createccpevent 1 60 1 t", "/role u GMS"] {
        assert_eq!(required_slash_role(cmd), high_gm, "cmd {cmd}");
    }

    assert_eq!(required_slash_role("/giveisk 500"), service_roles::ROLE_GM_MASK);
    assert_eq!(required_slash_role(""), service_roles::ROLE_GM_MASK);

    // Verify GM audit log ring buffer caps at 256 entries
    let ctx = test_ctx();
    let cid = 90_000_001_i64;
    for i in 0..270 {
        record_gm_audit(&ctx, cid, &format!("cmd_{i}"), true);
    }
    let logs: Vec<serde_json::Value> = ctx.db.with_snapshot(|s| {
        serde_json::from_str(s.service_state_blobs.get("gm_audit_log").unwrap()).unwrap()
    });
    assert_eq!(logs.len(), 256);
    assert_eq!(logs.first().unwrap()["cmd"].as_str(), Some("cmd_14"));
    assert_eq!(logs.last().unwrap()["cmd"].as_str(), Some("cmd_269"));
}

#[tokio::test]
async fn machonet_cache_login_multilogin_and_clone_grade_edge_branches() {
    let ctx = test_ctx();
    let uid = 1_u64;
    let cid = 90_000_001_i64;
    let mut sess = auth_session(uid, cid, 98_000_001, 30_000_142);
    sess.role = service_roles::ROLE_GMH;
    let kw = HashMap::new();

    // 1. GetCachedObject: (bool, oid), (int, oid), empty args -> InvalidArgument, unauthenticated -> Unauthenticated
    let mut unauth_sess = SessionState::new(10);
    assert_eq!(
        handle(
            &ctx,
            &mut unauth_sess,
            "machoNet",
            "GetCachedObject",
            &[PyRep::String("oid1".into())],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "Unauthorized"
    );
    assert_eq!(
        handle(&ctx, &mut sess, "machoNet", "GetCachedObject", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "machoNet",
            "GetCachedObject",
            &[PyRep::Bool(false), PyRep::String("missing_oid".into())],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "CachedObjectNotFound"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "machoNet",
            "GetCachedObject",
            &[PyRep::Integer(1), PyRep::String("missing_oid".into())],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "CachedObjectNotFound"
    );

    // 2. ForwardCharacterNotification: missing charID, Tuple payload, single value payload, default 2-arg
    assert_eq!(
        handle(&ctx, &mut sess, "machoNet", "ForwardCharacterNotification", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    for args in [
        vec![PyRep::Integer(cid), PyRep::String("Ev1".into()), PyRep::Tuple(vec![PyRep::Integer(7)])],
        vec![PyRep::Integer(cid), PyRep::String("Ev2".into()), PyRep::Integer(9)],
        vec![PyRep::Integer(cid)],
    ] {
        assert_eq!(
            handle(&ctx, &mut sess, "machoNet", "ForwardCharacterNotification", &args, &kw)
                .await
                .unwrap()
                .unwrap(),
            PyRep::Bool(true)
        );
    }

    // 3. SetGlobalConfigValue via kwargs & empty key error
    assert_eq!(
        handle(&ctx, &mut sess, "machoNet", "SetGlobalConfigValue", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    let mut cfg_kw = HashMap::new();
    cfg_kw.insert("key".to_string(), PyRep::String("kw_cfg".into()));
    cfg_kw.insert("val".to_string(), PyRep::Integer(42));
    assert_eq!(
        handle(&ctx, &mut sess, "machoNet", "SetGlobalConfigValue", &[], &cfg_kw)
            .await
            .unwrap()
            .unwrap(),
        PyRep::Bool(true)
    );

    // 4. cache.Setting: empty args -> InvalidArguments, 2-arg read, 1000-char truncation on 3-arg write
    assert_eq!(
        handle(&ctx, &mut sess, "cache", "Setting", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidArguments"
    );
    let long_val = "x".repeat(1_200);
    let written = handle(
        &ctx,
        &mut sess,
        "cache",
        "Setting",
        &[
            PyRep::String("sec".into()),
            PyRep::String("long_k".into()),
            PyRep::String(long_val),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(written.as_str().unwrap().len(), 1_000);
    let read_back = handle(
        &ctx,
        &mut sess,
        "cache",
        "Setting",
        &[PyRep::String("sec".into()), PyRep::String("long_k".into())],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(read_back.as_str().unwrap().len(), 1_000);

    // 5. authentication.Login: wrong arg count -> InvalidLoginArguments
    assert_eq!(
        handle(&ctx, &mut sess, "authentication", "Login", &[PyRep::String("u".into())], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidLoginArguments"
    );

    // 6. multiLoginBlocker.Login: Bytes, kwargs, empty hash error, Alpha hardware conflict
    assert_eq!(
        handle(&ctx, &mut sess, "multiLoginBlocker", "Login", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidComputerHash"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "multiLoginBlocker",
            "Login",
            &[PyRep::Bytes(b"hw_hash_1".to_vec())],
            &kw,
        )
        .await
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    // Second Alpha user with same hardware hash and different session_id is blocked (`Bool(false)`)
    let mut sess2 = auth_session(2, 90_000_002, 98_000_001, 30_000_142);
    sess2.session_id = 2;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.clone_grade = 1; // Alpha
        }
        let mut ch2 = snap.characters[&cid].clone();
        ch2.character_id = 90_000_002;
        ch2.user_id = 2;
        ch2.clone_grade = 1;
        snap.characters.insert(90_000_002, ch2);
    });
    let mut hw_kw = HashMap::new();
    hw_kw.insert("computerHash".to_string(), PyRep::String("hw_hash_1".into()));
    assert_eq!(
        handle(&ctx, &mut sess2, "multiLoginBlocker", "Login", &[], &hw_kw)
            .await
            .unwrap()
            .unwrap(),
        PyRep::Bool(false)
    );

    // 7. Clone grade Omega (2 -> 1) and fallback when session.character_id is None
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&cid).unwrap().clone_grade = 2;
    });
    let mut no_char_sess = sess.clone();
    no_char_sess.character_id = None;
    assert_eq!(
        handle(&ctx, &mut no_char_sess, "subscriptionMgr", "GetCloneGrade", &[], &kw)
            .await
            .unwrap()
            .unwrap(),
        PyRep::Integer(1)
    );
}

#[tokio::test]
async fn slash_commands_error_branches_and_role_aliases() {
    let ctx = test_ctx();
    let uid = 1_u64;
    let cid = 90_000_001_i64;
    let mut sess = auth_session(uid, cid, 98_000_001, 30_000_142);
    sess.role = service_roles::ROLE_GMH
        | service_roles::ROLE_GMS
        | service_roles::ROLE_ADMIN
        | service_roles::ROLE_ROLEADMIN
        | service_roles::ROLE_SPAWN;
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|snap| {
        snap.accounts.insert(
            "mod_user".into(),
            AccountRecord {
                user_id: 888,
                username: "mod_user".into(),
                password_token: String::new(),
                role: eve_db::service_roles::ROLE_PLAYER_DEFAULT,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
        snap.accounts.insert(
            "admin_user".into(),
            AccountRecord {
                user_id: 889,
                username: "admin_user".into(),
                password_token: String::new(),
                role: eve_db::service_roles::ROLE_ADMIN,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
    });

    // Test role aliases: hex, decimal, GMS, GMH, WORLDMOD, CONTENT, ADMIN, PLAYER
    for role_tok in ["0x10", "32", "GMS", "GMH", "WORLDMOD", "CONTENT", "ADMIN", "PLAYER"] {
        let res = handle(
            &ctx,
            &mut sess,
            "slash",
            "SlashCmd",
            &[PyRep::String(format!("/role mod_user {role_tok}"))],
            &kw,
        )
        .await
        .unwrap();
        assert!(res.is_ok(), "role alias {role_tok} failed");
    }

    // Test permanent ban variants ("0", "perm", custom text reason)
    for ban_cmd in [
        "/ban mod_user 0 rule violation",
        "/ban mod_user perm permanent ban",
        "/ban mod_user macroing in jita",
    ] {
        let res = handle(&ctx, &mut sess, "slash", "SlashCmd", &[PyRep::String(ban_cmd.into())], &kw)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            res.dict_get("bannedUntilUnix"),
            Some(&PyRep::Integer(32_503_680_000))
        );
    }

    // Test error codes across slash commands
    for (bad_cmd, expected_code) in [
        ("", "UnknownSlashCommand"),
        ("/giveisk abc", "InvalidSlashArgument"),
        ("/ban   ", "UnknownSlashCommand"),
        ("/ban no_such_user 60", "BanFailed"),
        ("/unban   ", "UnknownSlashCommand"),
        ("/unban no_such_user", "UnbanFailed"),
        ("/suspend mod_user", "InvalidSlashArgument"),
        ("/suspend mod_user 0", "InvalidSlashArgument"),
        ("/unsuspend   ", "UnknownSlashCommand"),
        ("/role mod_user", "InvalidSlashArgument"),
        ("/role mod_user NOT_A_ROLE", "InvalidSlashArgument"),
        ("/kick   ", "UnknownSlashCommand"),
        ("/kick admin_user", "KickFailed"),
        ("/giveitem 999999999 1", "TypeNotFound"),
        ("/setskill 3300 6", "InvalidSlashArgument"),
        ("/setskill 34 3", "InvalidSkillType"),
        ("/spawnnpc 999999999", "TypeNotFound"),
        ("/teleport 999999999", "SolarSystemNotFound"),
        ("/createccpevent 1 60", "InvalidSlashArgument"),
    ] {
        let err = handle(&ctx, &mut sess, "slash", "SlashCmd", &[PyRep::String(bad_cmd.into())], &kw)
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(err.code, expected_code, "for command '{bad_cmd}'");
    }
}
