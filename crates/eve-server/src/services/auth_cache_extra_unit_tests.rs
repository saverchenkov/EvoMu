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
    assert_eq!(
        required_slash_role("/"),
        service_roles::ROLE_QA
            | service_roles::ROLE_SPAWN
            | service_roles::ROLE_ROLEADMIN
            | service_roles::ROLE_GM_MASK
    );

    let qa_cmds = remote_slash_commands_for_role(service_roles::ROLE_QA);
    assert!(qa_cmds.contains(&"whoami") && qa_cmds.contains(&"online"));
    assert!(!qa_cmds.contains(&"giveisk"));
    let spawn_cmds = remote_slash_commands_for_role(service_roles::ROLE_SPAWN);
    assert!(spawn_cmds.contains(&"spawn") && spawn_cmds.contains(&"unspawn"));
    assert!(!spawn_cmds.contains(&"giveisk"));
    let gm_cmds = remote_slash_commands_for_role(service_roles::ROLE_GM_MASK);
    for expected in [
        "giveisk",
        "heal",
        "load",
        "tr",
        "setomega",
        "ban",
        "spawn",
        "whoami",
        "advancetime",
        "fastforward",
        "ff",
        "completeskills",
        "trainskills",
        "finishskills",
        "upwelltimer",
        "structuretimer",
        "finishstructure",
        "completeindustry",
        "finishjobs",
        "industrytimer",
        "cleartimers",
    ] {
        assert!(gm_cmds.contains(&expected), "missing {expected}");
    }
    for timer_cmd in [
        "/advancetime 60",
        "/fastforward 60",
        "/ff 60",
        "/completeskills all",
        "/trainskills",
        "/finishskills 10",
        "/upwelltimer",
        "/structuretimer 1001",
        "/finishstructure",
        "/completeindustry",
        "/finishjobs 10",
        "/industrytimer",
        "/cleartimers",
    ] {
        assert_eq!(
            required_slash_role(timer_cmd),
            service_roles::ROLE_GM_MASK,
            "timer cmd {timer_cmd}"
        );
    }
    assert!(!qa_cmds.contains(&"advancetime"));
    assert!(!qa_cmds.contains(&"cleartimers"));
    assert!(!spawn_cmds.contains(&"completeskills"));
    let reason = format_remote_command_list_reason(service_roles::ROLE_GM_MASK);
    assert!(reason.starts_with("Commands: ['"), "unexpected reason: {reason}");
    assert!(reason.ends_with("']"));

    // Verify GM audit log ring buffer caps at 256 entries
    let ctx = test_ctx();
    let cid = 90_000_001_i64;
    for i in 0..270 {
        record_gm_audit(&ctx, cid, &format!("cmd_{i}"), true);
    }
    let logs: Vec<serde_json::Value> =
        ctx.db.with_snapshot(|s| s.cluster_and_runtime.gm_audit_log.clone());
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
        vec![
            PyRep::Integer(cid),
            PyRep::String("OnNewNotificationReceived".into()),
            PyRep::Tuple(vec![PyRep::Integer(7)]),
        ],
        vec![
            PyRep::Integer(cid),
            PyRep::String("OnNewNotificationReceived".into()),
            PyRep::Integer(9),
        ],
        vec![
            PyRep::Integer(cid),
            PyRep::String("OnNotificationReceived".into()),
            PyRep::Tuple(vec![
                PyRep::Integer(1),
                PyRep::Integer(2),
                PyRep::None,
                PyRep::Integer(3),
                PyRep::String("ok".into()),
            ]),
        ],
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
    for blocked_event in ["Ev1", "OnTarget", "OnSpecialFX"] {
        assert_eq!(
            handle(
                &ctx,
                &mut sess,
                "machoNet",
                "ForwardCharacterNotification",
                &[
                    PyRep::Integer(cid),
                    PyRep::String(blocked_event.into()),
                    PyRep::Integer(1),
                ],
                &kw,
            )
            .await
            .unwrap()
            .unwrap_err()
            .code,
            "InvalidArgument",
            "expected InvalidArgument without debug_assert panic for {blocked_event}"
        );
    }
    assert!(
        ctx.drain_notifications_for_char(cid)
            .into_iter()
            .all(|n| n.event != "OnTarget" && n.event != "OnSpecialFX"),
        "ballpark-scoped ForwardCharacterNotification must never emit"
    );

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

    let list_err = handle(&ctx, &mut sess, "slash", "SlashCmd", &[PyRep::String("/".into())], &kw)
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(list_err.code, "SlashError");
    assert!(list_err.message.starts_with("Commands: ["));
    assert!(list_err.message.contains("'giveisk'"));
    assert!(list_err.message.contains("'heal'"));
    assert!(list_err.message.contains("'setomega'"));

    let mut player_sess = SessionState {
        user_id: Some(99),
        role: service_roles::ROLE_PLAYER_DEFAULT,
        character_id: sess.character_id,
        ..Default::default()
    };
    let player_list_err =
        handle(&ctx, &mut player_sess, "slash", "SlashCmd", &[PyRep::String("/".into())], &kw)
            .await
            .unwrap()
            .unwrap_err();
    assert_eq!(player_list_err.code, "SlashError");
    assert_eq!(player_list_err.message, "Commands: []");
    let player_cmd_err = handle(
        &ctx,
        &mut player_sess,
        "slash",
        "SlashCmd",
        &[PyRep::String("/giveisk 100".into())],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(player_cmd_err.code, "InsufficientRole");

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

#[tokio::test]
async fn rowset_login_global_config_and_teleport_kill_all_mutants() {
    let ctx = test_ctx();
    let kw = HashMap::new();

    // 1. record_gm_audit exact payload & ok=false
    record_gm_audit(&ctx, 90_123_001, "/auditcheck", false);
    ctx.db.with_snapshot(|snap| {
        let last = snap.cluster_and_runtime.gm_audit_log.last().expect("audit entry");
        assert_eq!(last["char_id"].as_i64(), Some(90_123_001));
        assert_eq!(last["cmd"].as_str(), Some("/auditcheck"));
        assert_eq!(last["ok"].as_bool(), Some(false));
    });

    // 2. SetGlobalConfigValue non-GM rejection + positional write persisted to ctx & DB
    let mut player_sess = auth_session(77, 90_123_001, 98_000_001, 30_000_142);
    player_sess.role = service_roles::ROLE_PLAYER_DEFAULT;
    assert_eq!(
        handle(
            &ctx,
            &mut player_sess,
            "machoNet",
            "SetGlobalConfigValue",
            &[PyRep::String("k".into()), PyRep::Integer(1)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InsufficientRole"
    );

    let mut gm_sess = auth_session(1, 90_000_001, 98_000_001, 30_000_142);
    gm_sess.role = service_roles::ROLE_GMH | service_roles::ROLE_QA;
    assert_eq!(
        handle(
            &ctx,
            &mut gm_sess,
            "machoNet",
            "SetGlobalConfigValue",
            &[PyRep::String("pos_cfg".into()), PyRep::String("v1".into())],
            &kw,
        )
        .await
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        ctx.global_config_values.read().unwrap().get("pos_cfg"),
        Some(&PyRep::String("v1".into()))
    );
    assert_eq!(
        ctx.db.with_snapshot(|s| s.cluster_and_runtime.macho_global_config.get("pos_cfg").cloned()),
        Some(PyRep::String("v1".into()))
    );

    // 3. cache.Rowset: corpmgt__roles, roles, default, races/factions/bloodlines/ancestries, unknown
    for tbl in ["corpmgt__roles", "roles"] {
        let rep = handle(&ctx, &mut gm_sess, "cache", "Rowset", &[PyRep::String(tbl.into())], &kw)
            .await
            .unwrap()
            .unwrap();
        match rep {
            PyRep::Rowset { columns, rows } => {
                assert_eq!(columns, vec!["roleID".to_string(), "roleName".to_string()]);
                assert!(!rows.is_empty(), "{tbl} must contain role rows");
                assert_eq!(
                    rows.len(),
                    crate::domain::corp_governance::CORP_ROLE_DEFINITIONS.len()
                );
            }
            other => panic!("expected Rowset for {tbl}, got {other:?}"),
        }
    }
    let default_rep = handle(&ctx, &mut gm_sess, "cache", "Rowset", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(default_rep, PyRep::Rowset { rows, .. } if !rows.is_empty()));
    for tbl in ["races", "factions", "bloodlines", "ancestries"] {
        let rep = handle(&ctx, &mut gm_sess, "cache", "Rowset", &[PyRep::String(tbl.into())], &kw)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(rep, PyRep::Rowset { rows, .. } if !rows.is_empty()), "{tbl} empty");
    }
    assert_eq!(
        handle(
            &ctx,
            &mut gm_sess,
            "cache",
            "Rowset",
            &[PyRep::String("no_such_table".into())],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "UnknownCacheRowset"
    );

    // 4. authentication.Login success, session mutation, wrong password, banned account
    ctx.db.provision_gm_account("auth_login_user", "secret_pw", 90_123_100, "AuthLoginChar");
    let login_acc = ctx.db.with_snapshot(|s| s.accounts["auth_login_user"].clone());
    let mut login_sess = SessionState::new(501);
    login_sess.character_id = Some(90_123_100);
    login_sess.address = "127.0.0.1:5000".into();
    let login_rep = handle(
        &ctx,
        &mut login_sess,
        "authentication",
        "Login",
        &[PyRep::String("auth_login_user".into()), PyRep::String("secret_pw".into())],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(login_rep.dict_get("userID"), Some(&PyRep::Integer(login_acc.user_id as i64)));
    assert_eq!(login_rep.dict_get("role"), Some(&PyRep::Integer(login_acc.role as i64)));
    assert!(login_rep.dict_get("sessionChange").is_some());
    assert_eq!(login_sess.user_id, Some(login_acc.user_id));
    assert_eq!(login_sess.role, login_acc.role);
    assert_eq!(login_sess.character_id, None);

    assert_eq!(
        handle(
            &ctx,
            &mut login_sess,
            "authentication",
            "Login",
            &[PyRep::String("auth_login_user".into()), PyRep::String("wrong_pw".into())],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "AuthenticationFailed"
    );

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(a) = snap.accounts.get_mut("auth_login_user") {
            a.banned_until_unix = Some(u64::MAX);
        }
    });
    assert_eq!(
        handle(
            &ctx,
            &mut login_sess,
            "authentication",
            "Login",
            &[PyRep::String("auth_login_user".into()), PyRep::String("secret_pw".into())],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "UserBanned"
    );

    // 5. handle_slash_teleport: self cross-system, self same-system in-space, and remote target_cid != cid
    let mut sys_ids: Vec<u32> = ctx.sde.solar_systems.keys().copied().collect();
    sys_ids.sort_unstable();
    assert!(sys_ids.len() >= 2, "seed must have >= 2 solar systems");
    let sys_a = 30_000_142_u32;
    let sys_b = sys_ids.into_iter().find(|&s| s != sys_a).unwrap();

    let gm_cid = 90_123_201_i64;
    let other_cid = 90_123_202_i64;
    ctx.db.provision_gm_account("tp_gm", "pw", gm_cid, "TpGm");
    ctx.db.provision_gm_account("tp_other", "pw", other_cid, "TpOther");
    let gm_acct = ctx.db.with_snapshot(|s| s.accounts["tp_gm"].clone());
    let gm_ship = ctx.db.get_character(gm_cid).unwrap().active_ship_id;

    let mut tp_sess = auth_session(gm_acct.user_id, gm_cid, 98_000_001, sys_a);
    tp_sess.role = service_roles::ROLE_GMH | service_roles::ROLE_QA;
    tp_sess.station_id = Some(60_003_760);
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&gm_cid).unwrap();
        ch.solar_system_id = sys_a;
        ch.station_id = Some(60_003_760);
    });

    // Self cross-system teleport updates session solar_system_id & clears station_id & returns valid keyval
    let tp_res = handle(
        &ctx,
        &mut tp_sess,
        "slash",
        "SlashCmd",
        &[PyRep::String(format!("/teleport {sys_b}"))],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(tp_res.dict_get("command"), Some(&PyRep::String(format!("/teleport {sys_b}"))));
    assert_eq!(tp_res.dict_get("characterID"), Some(&PyRep::Integer(gm_cid)));
    assert_eq!(tp_res.dict_get("solarSystemID"), Some(&PyRep::Integer(sys_b as i64)));
    assert_eq!(tp_sess.solar_system_id, Some(sys_b));
    assert_eq!(tp_sess.station_id, None);
    assert_eq!(ctx.db.get_character(gm_cid).unwrap().solar_system_id, sys_b);
    if gm_ship > 0 {
        assert!(ctx.db.with_snapshot(|s| s.item_positions.contains_key(&gm_ship)));
    }

    // Remote target_cid != cid: GM session MUST remain on gm_cid / sys_b while target_cid moves to sys_b
    ctx.db.with_mut_snapshot(|snap| {
        let other = snap.characters.get_mut(&other_cid).unwrap();
        other.solar_system_id = sys_a;
        other.station_id = None;
    });
    let remote_tp = handle(
        &ctx,
        &mut tp_sess,
        "slash",
        "SlashCmd",
        &[PyRep::String(format!("/tr {other_cid} {sys_b}"))],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(remote_tp.dict_get("characterID"), Some(&PyRep::Integer(other_cid)));
    assert_eq!(remote_tp.dict_get("solarSystemID"), Some(&PyRep::Integer(sys_b as i64)));
    assert_eq!(tp_sess.character_id, Some(gm_cid));
    assert_eq!(tp_sess.solar_system_id, Some(sys_b));
    assert_eq!(ctx.db.get_character(other_cid).unwrap().solar_system_id, sys_b);

    // Same-system in-space teleport by GM succeeds and updates position
    let same_tp = handle(
        &ctx,
        &mut tp_sess,
        "slash",
        "SlashCmd",
        &[PyRep::String(format!("/moveme {sys_b}"))],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(same_tp.dict_get("solarSystemID"), Some(&PyRep::Integer(sys_b as i64)));
    assert_eq!(tp_sess.solar_system_id, Some(sys_b));
}

#[tokio::test]
async fn structure_slash_command_role_and_listing() {
    let expected_mask =
        service_roles::ROLE_ADMIN | service_roles::ROLE_GMH | service_roles::ROLE_GML;
    assert_eq!(
        required_slash_role("/structure state 1001 110"),
        expected_mask
    );

    for role in [
        service_roles::ROLE_ADMIN,
        service_roles::ROLE_GMH,
        service_roles::ROLE_GML,
    ] {
        assert!(
            remote_slash_commands_for_role(role).contains(&"structure"),
            "role 0x{role:x} must see 'structure' command"
        );
    }
    for denied in [
        service_roles::ROLE_QA,
        service_roles::ROLE_SPAWN,
        service_roles::ROLE_GMS,
        service_roles::ROLE_WORLDMOD,
        service_roles::ROLE_PLAYER_DEFAULT,
    ] {
        assert!(
            !remote_slash_commands_for_role(denied).contains(&"structure"),
            "role 0x{denied:x} must not see 'structure' command"
        );
    }

    let ctx = test_ctx();
    let mut denied_sess = auth_session(1, 90_000_001, 98_000_001, 30_000_142);
    denied_sess.role =
        service_roles::ROLE_QA | service_roles::ROLE_SPAWN | service_roles::ROLE_GMS;
    let err = handle(
        &ctx,
        &mut denied_sess,
        "slash",
        "SlashCmd",
        &[PyRep::String("/structure state 1001 110".into())],
        &HashMap::new(),
    )
    .await
    .expect("slash dispatch handled")
    .expect_err("non-ADMIN/GMH/GML role must be rejected");
    assert_eq!(err.code, "InsufficientRole");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gm_bridge_giveitem_createitem_spawnnpc_target_player_and_fail_closed() {
    use eve_db::InventoryFlag;
    use eve_destiny::{Ball, MovementMode};
    use glam::DVec3;

    let ctx = test_ctx();
    let kw = HashMap::new();
    let gm_cid = 90_000_001_i64;
    let player_cid = 90_000_002_i64;
    let gm_sys = 30_000_142_u32;
    let player_sys = 30_002_187_u32;
    let player_station = 60_008_494_i64;

    let (gm_uid, player_uid, player_ship) = ctx.db.with_mut_snapshot(|snap| {
        let gm_uid = snap.characters[&gm_cid].user_id;
        let p = snap.characters.get_mut(&player_cid).expect("player char");
        p.solar_system_id = player_sys;
        p.station_id = Some(player_station);
        p.structure_id = None;
        (gm_uid, p.user_id, p.active_ship_id)
    });
    let _ = ctx
        .db
        .acquire_online_lock(gm_uid, 10, Some(gm_cid), false, ctx.now_unix());
    let _ = ctx
        .db
        .acquire_online_lock(player_uid, 11, Some(player_cid), false, ctx.now_unix());

    let mut gm_sess = auth_session(10, gm_cid, 98_000_001, gm_sys);
    gm_sess.station_id = Some(60_003_760);
    gm_sess.role = service_roles::ROLE_ADMIN | service_roles::ROLE_GML | service_roles::ROLE_SPAWN;

    let mut player_sess = auth_session(11, player_cid, 98_000_002, player_sys);
    player_sess.station_id = Some(player_station);
    player_sess.role = service_roles::ROLE_PLAYER_DEFAULT;

    let _ = ctx.drain_notifications_for_char(gm_cid);
    let _ = ctx.drain_notifications_for_char(player_cid);
    let item_count_before = ctx.db.with_snapshot(|snap| snap.items.len());

    // Non-GM caller must be rejected cleanly with InsufficientRole.
    for cmd in [
        "/giveitem 90000002 34 10",
        "/createitem 90000002 587 1",
        "/spawnnpc 90000002 587",
    ] {
        let err = handle(
            &ctx,
            &mut player_sess,
            "slash",
            "SlashCmd",
            &[PyRep::String(cmd.into())],
            &kw,
        )
        .await
        .expect("slash handled")
        .expect_err("player must be denied GM slash commands");
        assert_eq!(err.code, "InsufficientRole", "{cmd}");
    }

    // Unknown target_cid fails closed with CharacterNotFound without creating items or notifications.
    for bad_cmd in [
        "/giveitem 999888777 34",
        "/giveitem 999888777 34 5",
        "/createitem 999888777 587 1",
        "/spawnnpc 999888777 587",
    ] {
        let err = handle(
            &ctx,
            &mut gm_sess,
            "slash",
            "SlashCmd",
            &[PyRep::String(bad_cmd.into())],
            &kw,
        )
        .await
        .expect("slash handled")
        .expect_err("unknown target_cid must fail closed");
        assert_eq!(err.code, "CharacterNotFound", "{bad_cmd}");
    }
    assert_eq!(
        ctx.db.with_snapshot(|snap| snap.items.len()),
        item_count_before,
        "failed targeted slash commands must not create items"
    );
    assert!(ctx.drain_notifications_for_char(gm_cid).is_empty());
    assert!(ctx.drain_notifications_for_char(player_cid).is_empty());

    // Docked /giveitem <player_cid> 34 50 spawns into player's station hangar and notifies ONLY player_cid.
    let rep_hangar = handle(
        &ctx,
        &mut gm_sess,
        "slash",
        "SlashCmd",
        &[PyRep::String(format!("/giveitem {player_cid} 34 50"))],
        &kw,
    )
    .await
    .expect("slash handled")
    .expect("targeted docked /giveitem ok");
    let hangar_item_id = rep_hangar.dict_get("itemID").and_then(|v| v.as_i64()).expect("itemID");
    assert_eq!(
        rep_hangar.dict_get("locationID").and_then(|v| v.as_i64()),
        Some(player_station)
    );
    ctx.db.with_snapshot(|snap| {
        let rec = snap.items.get(&hangar_item_id).expect("hangar item exists");
        assert_eq!(rec.owner_id, player_cid);
        assert_eq!(rec.location_id, player_station);
        assert_eq!(rec.flag, InventoryFlag::Hangar);
        assert_eq!(rec.quantity, 50);
    });
    assert!(
        ctx.drain_notifications_for_char(player_cid)
            .iter()
            .any(|n| n.event == "OnItemChange"),
        "target player must receive OnItemChange"
    );
    assert!(
        ctx.drain_notifications_for_char(gm_cid).is_empty(),
        "GM caller must not receive target player's item notifications"
    );

    // Undocked /createitem <player_cid> 34 7 spawns into player's active ship cargo.
    ctx.db.with_mut_snapshot(|snap| {
        let p = snap.characters.get_mut(&player_cid).expect("player char");
        p.station_id = None;
        p.structure_id = None;
    });
    let rep_cargo = handle(
        &ctx,
        &mut gm_sess,
        "slash",
        "SlashCmd",
        &[PyRep::String(format!("/createitem {player_cid} 34 7"))],
        &kw,
    )
    .await
    .expect("slash handled")
    .expect("targeted undocked /createitem ok");
    let cargo_item_id = rep_cargo.dict_get("itemID").and_then(|v| v.as_i64()).expect("itemID");
    assert_eq!(
        rep_cargo.dict_get("locationID").and_then(|v| v.as_i64()),
        Some(player_ship)
    );
    ctx.db.with_snapshot(|snap| {
        let rec = snap.items.get(&cargo_item_id).expect("cargo item exists");
        assert_eq!(rec.owner_id, player_cid);
        assert_eq!(rec.location_id, player_ship);
        assert_eq!(rec.flag, InventoryFlag::Cargo);
        assert_eq!(rec.quantity, 7);
    });
    assert!(
        ctx.drain_notifications_for_char(player_cid)
            .iter()
            .any(|n| n.event == "OnItemChange")
    );
    assert!(ctx.drain_notifications_for_char(gm_cid).is_empty());

    // /spawnnpc <player_cid> 587 spawns in player_sys near player_ship rather than GM's system.
    let _ = ctx
        .systems
        .get_or_spawn(player_sys)
        .spawn_ball(Ball {
            id: player_ship,
            type_id: 587,
            position: DVec3::new(5_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 200.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    let rep_npc = handle(
        &ctx,
        &mut gm_sess,
        "slash",
        "SlashCmd",
        &[PyRep::String(format!("/spawnnpc {player_cid} 587"))],
        &kw,
    )
    .await
    .expect("slash handled")
    .expect("targeted /spawnnpc ok");
    let npc_id = rep_npc.dict_get("npcID").and_then(|v| v.as_i64()).expect("npcID");
    assert_eq!(
        rep_npc.dict_get("solarSystemID").and_then(|v| v.as_i64()),
        Some(player_sys as i64)
    );
    let (_, npc_ball) = ctx
        .systems
        .get_or_spawn(player_sys)
        .get_ball_snapshot(npc_id)
        .await
        .expect("player system ok")
        .expect("NPC spawned in player system");
    assert!(
        (npc_ball.position - DVec3::new(6_000.0, 0.0, 0.0)).length() < 1.0,
        "NPC should spawn 1km from player ship, got {:?}",
        npc_ball.position
    );
    assert!(
        ctx.systems
            .get_or_spawn(gm_sys)
            .get_ball_snapshot(npc_id)
            .await
            .expect("gm system ok")
            .is_none(),
        "NPC must not spawn in GM's solar system {gm_sys}"
    );
}

#[tokio::test]
async fn gm_giveitem_typed_parser_rejects_invalid_args_and_never_wakes_cold_systems() {
    use eve_db::InventoryFlag;

    let ctx = test_ctx();
    let kw = HashMap::new();
    let gm_cid = 90_000_001_i64;
    let player_cid = 90_000_002_i64;
    let cold_sys = 30_002_187_u32;
    let player_station = 60_008_494_i64;

    let player_ship = ctx.db.with_mut_snapshot(|snap| {
        let p = snap.characters.get_mut(&player_cid).expect("player char");
        p.solar_system_id = cold_sys;
        p.station_id = Some(player_station);
        p.structure_id = None;
        p.active_ship_id
    });
    let mut gm_sess = auth_session(20, gm_cid, 98_000_001, 30_000_142);
    gm_sess.station_id = Some(60_003_760);
    gm_sess.role = service_roles::ROLE_GM_MASK;

    let items_before = ctx.db.with_snapshot(|s| s.items.len());
    for bad_cmd in [
        "/giveitem badtarget 34",
        "/giveitem 34 0",
        "/createitem 34 -5",
        "/giveitem 90000002 34 0",
        "/giveitem 1 2 3 4",
    ] {
        let err = handle(
            &ctx,
            &mut gm_sess,
            "slash",
            "SlashCmd",
            &[PyRep::String(bad_cmd.into())],
            &kw,
        )
        .await
        .expect("slash handled")
        .expect_err("invalid typed /giveitem args must reject");
        assert_eq!(err.code, "InvalidSlashArgument", "{bad_cmd}");
    }
    assert_eq!(ctx.db.with_snapshot(|s| s.items.len()), items_before);
    assert!(!ctx.systems.is_system_loaded(cold_sys));

    // Targeted docked and undocked item grants do not wake cold solar systems (#1542).
    let docked_rep = handle(
        &ctx,
        &mut gm_sess,
        "slash",
        "SlashCmd",
        &[PyRep::String(format!("/giveitem {player_cid} 34 50"))],
        &kw,
    )
    .await
    .expect("slash handled")
    .expect("docked /giveitem ok");
    let docked_id = docked_rep.dict_get("itemID").and_then(PyRep::as_i64).expect("itemID");
    assert!(!ctx.systems.is_system_loaded(cold_sys));

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&player_cid).expect("player").station_id = None;
    });
    let undocked_rep = handle(
        &ctx,
        &mut gm_sess,
        "slash",
        "SlashCmd",
        &[PyRep::String(format!("/createitem {player_cid} 484 1"))],
        &kw,
    )
    .await
    .expect("slash handled")
    .expect("undocked /createitem ok");
    let undocked_id = undocked_rep.dict_get("itemID").and_then(PyRep::as_i64).expect("itemID");
    assert!(!ctx.systems.is_system_loaded(cold_sys));

    ctx.db.with_snapshot(|snap| {
        let hangar_item = snap.items.get(&docked_id).expect("hangar item");
        assert_eq!((hangar_item.location_id, hangar_item.flag, hangar_item.quantity), (player_station, InventoryFlag::Hangar, 50));
        let cargo_item = snap.items.get(&undocked_id).expect("cargo item");
        assert_eq!((cargo_item.location_id, cargo_item.flag, cargo_item.quantity, cargo_item.singleton), (player_ship, InventoryFlag::Cargo, 1, true));
    });
}

#[tokio::test]
async fn slash_tr_does_not_wake_cold_origin_system_on_valid_or_invalid_target() {
    let ctx = test_ctx();
    let cid = 90_000_001_i64;
    let cold_origin_sys = 30_000_140_u32; // Maurasi
    let dest_sys = 30_000_144_u32; // Perimeter
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&cid).expect("pilot");
        ch.solar_system_id = cold_origin_sys;
        ch.station_id = None;
        ch.structure_id = None;
    });
    assert!(
        ctx.systems.get_handle(cold_origin_sys).is_none(),
        "precondition: origin system must be cold before /tr"
    );

    let mut sess = auth_session(1, cid, 98_000_001, cold_origin_sys);
    sess.role = service_roles::ROLE_GMH;

    let bad_err = handle(
        &ctx,
        &mut sess,
        "slash",
        "SlashCmd",
        &[PyRep::String("/tr me 99999999".into())],
        &kw,
    )
    .await
    .expect("slash handled")
    .expect_err("unknown location must fail without waking cold origin");
    assert_eq!(bad_err.code, "SolarSystemNotFound");
    assert!(
        ctx.systems.get_handle(cold_origin_sys).is_none(),
        "failed /tr lookup must not wake cold origin system"
    );
    assert!(
        ctx.systems.get_handle(0).is_none(),
        "system 0 must never be spawned"
    );

    let ok_rep = handle(
        &ctx,
        &mut sess,
        "slash",
        "SlashCmd",
        &[PyRep::String(format!("/tr me {dest_sys}"))],
        &kw,
    )
    .await
    .expect("slash handled")
    .expect("/tr me <dest_sys> succeeds from cold origin");
    assert_eq!(ok_rep.dict_get("solarSystemID").and_then(PyRep::as_i64), Some(i64::from(dest_sys)));
    assert_eq!(sess.solar_system_id, Some(dest_sys));
    assert!(
        ctx.systems.get_handle(cold_origin_sys).is_none(),
        "cross-system /tr from cold origin system must NOT spawn an actor for old_sys_id"
    );
    assert!(
        ctx.systems.get_handle(dest_sys).is_some(),
        "destination solar system must be woken"
    );
    assert!(
        ctx.systems.get_handle(0).is_none(),
        "system 0 must never be spawned"
    );
}

#[tokio::test]
async fn slash_tr_live_origin_abandons_drones_fighters_probes_and_aborts_self_destruct_while_cold_stays_cold() {
    let ctx = test_ctx();
    let live_sys = 30_000_142_u32; // Jita (live)
    let cold_sys = 30_000_140_u32; // Maurasi (kept cold)
    let dest_sys = 30_000_144_u32; // Perimeter
    let kw = HashMap::new();

    // 1. Live origin `/tr me <dest>`: deployed drone, fighter, probe, and self-destruct all tear down.
    let cid = 90_000_001_i64;
    let ship_id = ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&cid).expect("pilot");
        ch.solar_system_id = live_sys;
        ch.station_id = None;
        ch.structure_id = None;
        ch.active_ship_id
    });
    let drone_id = 100_891_010_i64;
    let fighter_id = 100_891_020_i64;
    let probe_id = 555_891_001_i64;
    let live_handle = ctx.systems.get_or_spawn(live_sys);
    for (bid, tid) in [(ship_id, 587_u32), (drone_id, 2454_u32), (fighter_id, 23057_u32)] {
        let _ = live_handle
            .spawn_ball_with_meta(
                eve_destiny::Ball {
                    id: bid,
                    type_id: tid,
                    position: glam::DVec3::new(500.0, 0.0, 0.0),
                    velocity: glam::DVec3::ZERO,
                    max_velocity: 350.0,
                    inertia: 1.0,
                    mass: 1_000_000.0,
                    radius: 50.0,
                    speed_fraction: 0.0,
                    mode: eve_destiny::MovementMode::Stop,
                },
                eve_destiny::CombatState::default(),
                ctx.build_ship_slim_meta(cid, tid),
            )
            .await;
    }
    ctx.db.with_mut_snapshot(|snap| {
        let mut f_item = eve_db::InventoryItemRecord::default();
        f_item.item_id = fighter_id;
        f_item.type_id = 23057;
        f_item.owner_id = cid;
        f_item.location_id = ship_id;
        f_item.flag = eve_db::InventoryFlag::Cargo;
        f_item.quantity = 9;
        f_item.singleton = true;
        snap.items.insert(fighter_id, f_item);
    });
    crate::services::drone_test_helpers::seed_active_drones(
        &ctx,
        [crate::system_actor::DroneSpaceState {
            ball_id: drone_id,
            drone_item_id: drone_id,
            type_id: 2454,
            group_id: 100,
            owner_char_id: cid,
            original_owner_char_id: cid,
            parent_ship_id: ship_id,
            solar_system_id: live_sys,
            target_id: Some(999),
            activity_state: 1,
            assist_char_id: None,
            guard_char_id: None,
            is_abandoned: false,
            is_sentry: false,
            bandwidth_used: 5.0,
            max_velocity: 1_500.0,
            optimal_range: 5_000.0,
            damage_vector: eve_destiny::DamageVector::default(),
            shield_repair_amount: 0.0,
            armor_repair_amount: 0.0,
            hull_repair_amount: 0.0,
            web_speed_factor: 1.0,
            neut_amount: 0.0,
            ecm_strength: 0.0,
            tp_sig_bonus_pct: 0.0,
        }],
    );
    crate::services::fighter_test_helpers::insert_active_fighter(
        &ctx,
        fighter_id,
        crate::router::ActiveFighterSquadron {
            ball_id: fighter_id,
            item_id: fighter_id,
            type_id: 23057,
            owner_char_id: cid,
            parent_ship_id: ship_id,
            tube_index: 0,
            squadron_size: 9,
            target_id: Some(999),
            is_abandoned: false,
            mwd_active: true,
        },
    );
    crate::probe_test_helpers::seed_probe(
        &ctx,
        crate::router::ActiveScannerProbe {
            probe_id,
            probe_type_id: 30013,
            owner_char_id: cid,
            ship_id,
            launcher_module_id: 555_891_000,
            solar_system_id: live_sys,
            position: [0.0; 3],
            destination: [0.0; 3],
            range_step: 2,
            scan_range_m: 1_000_000.0,
            base_sensor_strength: 10.0,
            max_flight_time_s: 300.0,
            launched_at_unix: 1_000,
            state: 1,
            warp_end_ft: None,
        },
    );
    crate::sp6_lifecycle::register_self_destruct_countdown(&ctx, ship_id, cid, live_sys, 1_000, 1_120);

    let mut sess = auth_session(1, cid, 98_000_001, live_sys);
    sess.role = service_roles::ROLE_GMH;
    let _ = handle(
        &ctx,
        &mut sess,
        "slash",
        "SlashCmd",
        &[PyRep::String(format!("/tr me {dest_sys}"))],
        &kw,
    )
    .await
    .expect("slash handled")
    .expect("/tr me <dest_sys> from live origin ok");

    let d_after = crate::services::drone_test_helpers::active_drone(&ctx, drone_id).expect("drone present");
    assert!(d_after.is_abandoned && d_after.owner_char_id == 0);
    assert!(
        crate::services::fighter_test_helpers::active_fighter(&ctx, fighter_id)
            .expect("fighter present")
            .is_abandoned
    );
    assert_eq!(crate::probe_test_helpers::get_probe(&ctx, probe_id).unwrap().state, 0);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&fighter_id].location_id, i64::from(live_sys));
        assert!(!snap.sp6_lifecycle.self_destructs.contains_key(&ship_id));
    });
    assert!(
        !ctx.combat_ephemeral
            .read()
            .unwrap()
            .self_destruct_countdowns
            .contains_key(&ship_id)
    );

    // 2. Remote `/tr <other_cid> <dest_sys>` on cold origin keeps cold_sys unloaded while clearing probes & self-destruct.
    let remote_cid = 90_891_555_i64;
    let remote_ship = 91_891_555_i64;
    let remote_probe = 555_891_555_i64;
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.get(&cid).cloned().expect("template char");
        ch.character_id = remote_cid;
        ch.solar_system_id = cold_sys;
        ch.station_id = None;
        ch.structure_id = None;
        ch.active_ship_id = remote_ship;
        snap.characters.insert(remote_cid, ch);

        let mut ship = eve_db::InventoryItemRecord::default();
        ship.item_id = remote_ship;
        ship.type_id = 587;
        ship.owner_id = remote_cid;
        ship.location_id = i64::from(cold_sys);
        ship.quantity = 1;
        ship.singleton = true;
        snap.items.insert(remote_ship, ship);
    });
    crate::probe_test_helpers::seed_probe(
        &ctx,
        crate::router::ActiveScannerProbe {
            probe_id: remote_probe,
            probe_type_id: 30013,
            owner_char_id: remote_cid,
            ship_id: remote_ship,
            launcher_module_id: 555_891_554,
            solar_system_id: cold_sys,
            position: [0.0; 3],
            destination: [0.0; 3],
            range_step: 2,
            scan_range_m: 1_000_000.0,
            base_sensor_strength: 10.0,
            max_flight_time_s: 300.0,
            launched_at_unix: 1_000,
            state: 1,
            warp_end_ft: None,
        },
    );
    crate::sp6_lifecycle::register_self_destruct_countdown(
        &ctx,
        remote_ship,
        remote_cid,
        cold_sys,
        1_000,
        1_120,
    );
    assert!(ctx.systems.get_handle(cold_sys).is_none());

    let _ = handle(
        &ctx,
        &mut sess,
        "slash",
        "SlashCmd",
        &[PyRep::String(format!("/tr {remote_cid} {dest_sys}"))],
        &kw,
    )
    .await
    .expect("slash handled")
    .expect("remote /tr <other_cid> <dest_sys> ok");

    assert!(
        ctx.systems.get_handle(cold_sys).is_none(),
        "remote /tr from cold origin must NOT wake old_sys_id"
    );
    assert_eq!(crate::probe_test_helpers::get_probe(&ctx, remote_probe).unwrap().state, 0);
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.sp6_lifecycle.self_destructs.contains_key(&remote_ship));
    });
    assert!(
        !ctx.combat_ephemeral
            .read()
            .unwrap()
            .self_destruct_countdowns
            .contains_key(&remote_ship)
    );
}

