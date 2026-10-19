//! Security, authentication, authorization, and static data auditor-flagged rows closure suite.
//!
//! Validates:
//! - Account ban enforcement across MachoNet and SSO, SQLite persistence, and automatic ban expiration clearing.
//! - Account role bitmasks (ROLE_GMS, ROLE_GML, ROLE_GMH, ROLE_ADMIN, ROLE_WORLDMOD) and GM service checks.
//! - Single active session enforcement, displacement, and Alpha hardware concurrency limits.
//! - Slash GM /ban command live session termination and SQLite persistence.
//! - Lossless verification of all static SDE tables and client static resources.
//! - Full documentation reconciliation, status label checks, and RPC catalog alignment.

#![forbid(unsafe_code)]

mod common;

use eve_db::service_roles;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use rusqlite::OpenFlags;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

#[tokio::test]
async fn test_account_bans_real_enforcement_and_auto_clear() {
    // parity: feat:account.account-bans
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let now = ctx.now_unix();

    // 1. Provision user and verify active ban enforcement and persistence
    ctx.db.provision_player_account("ban_victim_1", "secret123");
    let acc_before = ctx
        .db
        .with_snapshot(|s| s.accounts.get("ban_victim_1").cloned())
        .expect("get account before ban");
    assert_eq!(acc_before.banned_until_unix, None);

    let ban_expiry = now + 7200;
    ctx.db
        .ban_account("ban_victim_1", ban_expiry, "Security Botting Violation")
        .expect("ban_account");

    let acc_banned = ctx
        .db
        .with_snapshot(|s| s.accounts.get("ban_victim_1").cloned())
        .expect("get account after ban");
    assert_eq!(acc_banned.banned_until_unix, Some(ban_expiry));
    assert_eq!(
        acc_banned.ban_reason.as_deref(),
        Some("Security Botting Violation")
    );

    // 2. Active ban rejected on DB authentication
    let auth_err = ctx
        .db
        .authenticate_at("ban_victim_1", "secret123", now)
        .expect_err("Active ban must reject authentication");
    match auth_err {
        eve_db::DbError::AccountBanned { reason, until_unix } => {
            assert!(reason.contains("Security Botting Violation"));
            assert_eq!(until_unix, ban_expiry);
        }
        other => panic!("Expected DbError::AccountBanned, got {other:?}"),
    }

    // 3. Active ban rejected on MachoNet authentication.Login RPC with UserBanned
    let mut sess = SessionState::new(20_101);
    let login_err = ctx
        .dispatch_rpc(
            &mut sess,
            "authentication",
            None,
            "Login",
            &[
                PyRep::String("ban_victim_1".into()),
                PyRep::String("secret123".into()),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("Banned account login must fail");
    assert_eq!(login_err.code, "UserBanned");
    assert!(login_err.message.contains("Security Botting Violation"));
    assert!(login_err.message.contains(&ban_expiry.to_string()));

    // 4. Session revocation check rejects active ban
    let revoke_reason = ctx
        .db
        .check_session_revoked(acc_banned.user_id, sess.session_id, now);
    assert_eq!(
        revoke_reason.as_ref().map(|(c, _)| c.as_str()),
        Some("UserBanned")
    );

    // 5. Expired ban is automatically cleared from memory and SQLite DB
    ctx.db.provision_player_account("ban_expired_1", "secret456");
    let past_expiry = now.saturating_sub(300);
    ctx.db
        .ban_account("ban_expired_1", past_expiry, "Temporary cooldown")
        .expect("ban account with past expiry");

    // Clear expired ban explicitly by username
    let cleared = ctx.db.clear_expired_ban("ban_expired_1", now);
    assert!(cleared, "clear_expired_ban must return true for expired ban");

    let acc_cleared = ctx
        .db
        .with_snapshot(|s| s.accounts.get("ban_expired_1").cloned())
        .expect("get account after expired ban clear");
    assert_eq!(acc_cleared.banned_until_unix, None);
    assert_eq!(acc_cleared.ban_reason, None);

    // Re-ban with past expiry to verify automatic clearing during authenticate_at & Login RPC
    ctx.db
        .ban_account("ban_expired_1", past_expiry, "Temporary cooldown 2")
        .expect("re-ban with past expiry");

    let auth_ok = ctx
        .db
        .authenticate_at("ban_expired_1", "secret456", now)
        .expect("authenticate_at must auto-clear expired ban and succeed");
    assert_eq!(auth_ok.username, "ban_expired_1");

    let mut sess_expired = SessionState::new(20_102);
    let login_rsp = ctx
        .dispatch_rpc(
            &mut sess_expired,
            "authentication",
            None,
            "Login",
            &[
                PyRep::String("ban_expired_1".into()),
                PyRep::String("secret456".into()),
            ],
            &kw,
            None,
        )
        .await
        .expect("Login RPC must auto-clear expired ban and succeed");
    assert!(matches!(login_rsp, PyRep::KeyVal(_)));
}

#[tokio::test]
async fn test_account_roles_bitmask_checks() {
    // parity: plan:S-9, feat:account.account-roles, feat:admin.gm-role-gating
    let ctx = ServerContext::new();
    let kw = HashMap::new();

    // 1. Verify canonical CCP 64-bit role bitmasks from serviceConst.pyj
    assert_eq!(service_roles::ROLE_PLAYER, 0x2000_0000_0000_0000);
    assert_eq!(service_roles::ROLE_LOGIN, 0x4000_0000_0000_0000);
    assert_eq!(
        service_roles::ROLE_PLAYER_DEFAULT,
        service_roles::ROLE_PLAYER | service_roles::ROLE_LOGIN
    );
    assert_eq!(service_roles::ROLE_GMS, 0x0000_0040_0000_0000);
    assert_eq!(service_roles::ROLE_WORLDMOD, 0x0002_0000_0000_0000);
    assert_eq!(service_roles::ROLE_GMH, 0x0020_0000_0000_0000);
    assert_eq!(service_roles::ROLE_GML, 0x0040_0000_0000_0000);
    assert_eq!(service_roles::ROLE_ADMIN, 0x0100_0000_0000_0000);

    // Standard player role mask must NOT intersect with ROLE_GM_MASK
    assert_eq!(
        service_roles::ROLE_PLAYER_DEFAULT & service_roles::ROLE_GM_MASK,
        0
    );

    // 2. Test persisted DB roles populated via authentication.Login (never mutating sess.role in memory)
    let mut sess_player = SessionState::new(20_201);
    let login_player_rep = ctx
        .dispatch_rpc(
            &mut sess_player,
            "authentication",
            None,
            "Login",
            &[
                PyRep::String("demo".into()),
                PyRep::String("demo-secret".into()),
            ],
            &kw,
            None,
        )
        .await
        .expect("Login demo");
    assert!(!sess_player.has_gm_role());
    assert!(login_player_rep.dict_get("sessionChange").is_some());

    let gm_roles = [
        ("role_gms_user", service_roles::ROLE_GMS, 90_000_801),
        ("role_gml_user", service_roles::ROLE_GML, 90_000_802),
        ("role_gmh_user", service_roles::ROLE_GMH, 90_000_803),
        ("role_admin_user", service_roles::ROLE_ADMIN, 90_000_804),
        ("role_wm_user", service_roles::ROLE_WORLDMOD, 90_000_805),
    ];

    for (idx, (uname, role_bit, cid)) in gm_roles.into_iter().enumerate() {
        ctx.db
            .provision_gm_account(uname, "gm-role-pass", cid, &format!("GM Role {idx}"));
        ctx.db
            .set_account_role(uname, service_roles::ROLE_PLAYER_DEFAULT | role_bit)
            .expect("set_account_role");
        let mut sess_gm = SessionState::new(20_210 + idx as u64);
        let login_gm_rep = ctx
            .dispatch_rpc(
                &mut sess_gm,
                "authentication",
                None,
                "Login",
                &[
                    PyRep::String(uname.into()),
                    PyRep::String("gm-role-pass".into()),
                ],
                &kw,
                None,
            )
            .await
            .expect("Login GM role account");
        assert!(
            sess_gm.has_gm_role(),
            "{uname} (0x{role_bit:016X}) loaded from DB must satisfy session.has_gm_role()"
        );
        assert!(login_gm_rep.dict_get("sessionChange").is_some());
    }

    // 3. Regular player session rejected on GM slash commands
    ctx.dispatch_rpc(
        &mut sess_player,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID on player session");

    let slash_err = ctx
        .dispatch_rpc(
            &mut sess_player,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String("/giveisk 1000".into())],
            &kw,
            None,
        )
        .await
        .expect_err("Non-GM player session calling slash command must be rejected");
    assert_eq!(slash_err.code, "InsufficientRole");

    // 4. Session with persisted ROLE_WORLDMOD is permitted to execute slash commands
    ctx.db
        .provision_gm_account("worldmod_pilot", "wm-secret", 90_000_004, "Worldmod Pilot");
    ctx.db
        .set_account_role(
            "worldmod_pilot",
            service_roles::ROLE_PLAYER_DEFAULT | service_roles::ROLE_WORLDMOD,
        )
        .expect("set_account_role worldmod_pilot");
    let mut sess_worldmod = SessionState::new(20_203);
    common::login_pilot(&ctx, &mut sess_worldmod, "worldmod_pilot", "wm-secret").await;
    ctx.dispatch_rpc(
        &mut sess_worldmod,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_004)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID on worldmod session");

    let worldmod_slash = ctx
        .dispatch_rpc(
            &mut sess_worldmod,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String("/giveisk 1000".into())],
            &kw,
            None,
        )
        .await
        .expect("ROLE_WORLDMOD session must be permitted to execute /giveisk");
    assert!(matches!(worldmod_slash, PyRep::KeyVal(_)));
}

#[tokio::test]
async fn test_multi_login_block_enforcement() {
    // parity: feat:account.multi-login-block
    let ctx = ServerContext::new();
    let kw = HashMap::new();

    // 1. Two sessions both call authentication.Login before either claims multiLoginBlocker.Login:
    //    whichever session calls multiLoginBlocker.Login first wins the lock, and the other gets False.
    ctx.db.provision_player_account("mlb_race_pilot", "race-pass");
    let mut race_s1 = SessionState::new(20_290);
    let mut race_s2 = SessionState::new(20_291);
    ctx.dispatch_rpc(
        &mut race_s1,
        "authentication",
        None,
        "Login",
        &[
            PyRep::String("mlb_race_pilot".into()),
            PyRep::String("race-pass".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("race_s1 authentication.Login");
    ctx.dispatch_rpc(
        &mut race_s2,
        "authentication",
        None,
        "Login",
        &[
            PyRep::String("mlb_race_pilot".into()),
            PyRep::String("race-pass".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("race_s2 authentication.Login");

    let race_s2_claim = ctx
        .dispatch_rpc(
            &mut race_s2,
            "multiLoginBlocker",
            None,
            "Login",
            &[PyRep::String("hw-race-02".into())],
            &kw,
            None,
        )
        .await
        .expect("race_s2 claims multiLoginBlocker.Login first");
    assert_eq!(race_s2_claim, PyRep::Bool(true));

    let race_s1_late = ctx
        .dispatch_rpc(
            &mut race_s1,
            "multiLoginBlocker",
            None,
            "Login",
            &[PyRep::String("hw-race-01".into())],
            &kw,
            None,
        )
        .await
        .expect("race_s1 calls multiLoginBlocker.Login second");
    assert_eq!(race_s1_late, PyRep::Bool(false));

    // 2. Session 1 logs in and acquires online lock on "demo"
    let mut sess1 = SessionState::new(20_301);
    let (sess1_closer_tx, mut sess1_closer_rx) =
        tokio::sync::mpsc::unbounded_channel::<(String, String)>();
    ctx.register_session_closer(sess1.session_id, sess1_closer_tx);

    common::login_pilot(&ctx, &mut sess1, "demo", "demo-secret").await;
    let mlb_sess1 = ctx
        .dispatch_rpc(
            &mut sess1,
            "multiLoginBlocker",
            None,
            "Login",
            &[PyRep::String("hw-blocker-01".into())],
            &kw,
            None,
        )
        .await
        .expect("multiLoginBlocker.Login on sess1");
    assert_eq!(mlb_sess1, PyRep::Bool(true));

    ctx.dispatch_rpc(
        &mut sess1,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID sess1");

    assert_eq!(
        ctx.db.get_online_session(1).map(|s| s.session_id),
        Some(sess1.session_id)
    );

    // 3. Concurrent Session 2 attempts to login to the same account
    let mut sess2 = SessionState::new(20_302);
    common::login_pilot(&ctx, &mut sess2, "demo", "demo-secret").await;

    // Without displace, multiLoginBlocker.Login returns False
    let mlb_refused = ctx
        .dispatch_rpc(
            &mut sess2,
            "multiLoginBlocker",
            None,
            "Login",
            &[PyRep::String("hw-blocker-02".into())],
            &kw,
            None,
        )
        .await
        .expect("multiLoginBlocker.Login without displace on sess2");
    assert_eq!(mlb_refused, PyRep::Bool(false));

    // Calling game RPC on sess2 fails with MultiLoginBlocked
    let char_err = ctx
        .dispatch_rpc(
            &mut sess2,
            "charUnboundMgr",
            None,
            "SelectCharacterID",
            &[PyRep::Integer(90_000_001)],
            &kw,
            None,
        )
        .await
        .expect_err("Concurrent session without lock must be rejected with MultiLoginBlocked");
    assert_eq!(char_err.code, "MultiLoginBlocked");

    // 4. Displace Session 1 from Session 2
    let mlb_displaced = ctx
        .dispatch_rpc(
            &mut sess2,
            "multiLoginBlocker",
            None,
            "Login",
            &[
                PyRep::String("hw-blocker-02".into()),
                PyRep::Bool(true),
            ],
            &kw,
            None,
        )
        .await
        .expect("multiLoginBlocker.Login with displace=true");
    assert_eq!(mlb_displaced, PyRep::Bool(true));

    // Session 1 closer receives immediate ACL_DISPLACED
    let (displaced_code, displaced_msg) = sess1_closer_rx
        .try_recv()
        .expect("Displaced session must receive immediate disconnect notification");
    assert_eq!(displaced_code, "ACL_DISPLACED");
    assert!(displaced_msg.contains("Displaced"));

    // Session 1 can no longer make RPC calls (SessionKicked)
    let kicked_err = ctx
        .dispatch_rpc(
            &mut sess1,
            "charUnboundMgr",
            None,
            "GetCharactersToSelect",
            &[],
            &kw,
            None,
        )
        .await
        .expect_err("Displaced session must be rejected");
    assert_eq!(kicked_err.code, "SessionKicked");

    // Session 2 now holds the online lock and can proceed
    assert_eq!(
        ctx.db.get_online_session(1).map(|s| s.session_id),
        Some(sess2.session_id)
    );
    ctx.dispatch_rpc(
        &mut sess2,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID on displaced sess2 must succeed");

    // 5. Alpha clone hardware concurrency restriction
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch1) = snap.characters.get_mut(&90_000_001) {
            ch1.clone_grade = 0; // Alpha
        }
        if let Some(ch2) = snap.characters.get_mut(&90_000_002) {
            ch2.clone_grade = 0; // Alpha
        }
    });

    let mut sess_other = SessionState::new(20_303);
    common::login_pilot(&ctx, &mut sess_other, "other", "other-secret").await;

    // Both "demo" (sess2) and "other" (sess_other) are Alpha clones on "hw-blocker-02"
    let alpha_concurrency = ctx
        .dispatch_rpc(
            &mut sess_other,
            "multiLoginBlocker",
            None,
            "Login",
            &[PyRep::String("hw-blocker-02".into())],
            &kw,
            None,
        )
        .await
        .expect("multiLoginBlocker.Login on second Alpha account");
    assert_eq!(
        alpha_concurrency,
        PyRep::Bool(false),
        "Alpha clone hardware concurrency must be rejected on identical computerhash"
    );

    // Restore clone grades to Omega (1) and verify Omega+Omega on same computerhash is allowed
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch1) = snap.characters.get_mut(&90_000_001) {
            ch1.clone_grade = 1;
        }
        if let Some(ch2) = snap.characters.get_mut(&90_000_002) {
            ch2.clone_grade = 1;
        }
    });
    let omega_concurrency = ctx
        .dispatch_rpc(
            &mut sess_other,
            "multiLoginBlocker",
            None,
            "Login",
            &[PyRep::String("hw-blocker-02".into())],
            &kw,
            None,
        )
        .await
        .expect("multiLoginBlocker.Login on second Omega account");
    assert_eq!(omega_concurrency, PyRep::Bool(true));
}

#[tokio::test]
async fn test_gm_ban_slash_command() {
    // parity: feat:admin.gm-ban
    let ctx = ServerContext::new();
    let kw = HashMap::new();

    // 1. Provision GM and target user
    ctx.db
        .provision_gm_account("gm_auditor", "gm-secret", 90_000_003, "Auditor GM");
    let mut gm_sess = SessionState::new(20_401);
    common::login_pilot(&ctx, &mut gm_sess, "gm_auditor", "gm-secret").await;
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
    .expect("SelectCharacterID GM");

    ctx.db
        .provision_player_account("bot_suspect", "botpass123");
    let mut target_sess = SessionState::new(20_402);
    let (target_closer_tx, mut target_closer_rx) =
        tokio::sync::mpsc::unbounded_channel::<(String, String)>();
    ctx.register_session_closer(target_sess.session_id, target_closer_tx);

    common::login_pilot(&ctx, &mut target_sess, "bot_suspect", "botpass123").await;
    let target_uid = target_sess.user_id.expect("target user_id");
    assert_eq!(
        ctx.db.get_online_session(target_uid).map(|s| s.session_id),
        Some(target_sess.session_id)
    );

    // 2. GM executes timed /ban by username with duration and reason
    let ban_rsp = ctx
        .dispatch_rpc(
            &mut gm_sess,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String("/ban bot_suspect 3600 Market manipulation bot".into())],
            &kw,
            None,
        )
        .await
        .expect("GM /ban execution");
    let PyRep::KeyVal(ban_kv) = ban_rsp else {
        panic!("Expected KeyVal from /ban");
    };
    assert_eq!(
        ban_kv.get("kickedSessionID"),
        Some(&PyRep::Integer(target_sess.session_id as i64)),
        "/ban must kick the active live session"
    );
    assert_eq!(ban_kv.get("permanent"), Some(&PyRep::Bool(false)));

    // Live session closer channel immediately receives UserBanned notification
    let (close_code, close_reason) = target_closer_rx
        .try_recv()
        .expect("Target live session must be immediately disconnected on /ban");
    assert_eq!(close_code, "UserBanned");
    assert!(close_reason.contains("Market manipulation bot"));

    // Online session must be removed from DB
    assert!(ctx.db.get_online_session(target_uid).is_none());

    // Target cannot log back in
    let mut relogin_sess = SessionState::new(20_403);
    let relogin_err = ctx
        .dispatch_rpc(
            &mut relogin_sess,
            "authentication",
            None,
            "Login",
            &[
                PyRep::String("bot_suspect".into()),
                PyRep::String("botpass123".into()),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("Banned account must fail login");
    assert_eq!(relogin_err.code, "UserBanned");

    // 3. GM /ban by numeric account user_id (permanent ban with duration 0)
    ctx.db
        .provision_player_account("bot_suspect_2", "botpass456");
    let target2 = ctx
        .db
        .with_snapshot(|s| s.accounts.get("bot_suspect_2").cloned())
        .expect("get bot_suspect_2");
    let ban_by_id_rsp = ctx
        .dispatch_rpc(
            &mut gm_sess,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String(format!(
                "/ban {} 0 Exploiting warp bug permanently",
                target2.user_id
            ))],
            &kw,
            None,
        )
        .await
        .expect("GM permanent /ban by user_id");
    assert_eq!(ban_by_id_rsp.dict_get("permanent"), Some(&PyRep::Bool(true)));

    let target2_reloaded = ctx
        .db
        .with_snapshot(|s| s.accounts.get("bot_suspect_2").cloned())
        .expect("target2 account reloaded");
    assert!(target2_reloaded.banned_until_unix.is_some());

    // 4. GM cannot ban another ROLE_ADMIN account
    let admin_ban_err = ctx
        .dispatch_rpc(
            &mut gm_sess,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String("/ban gm_auditor 3600 Self ban attempt".into())],
            &kw,
            None,
        )
        .await
        .expect_err("GM cannot ban ROLE_ADMIN");
    assert_eq!(admin_ban_err.code, "BanFailed");

    // 5. GM /unban restores account access
    ctx.dispatch_rpc(
        &mut gm_sess,
        "slash",
        None,
        "SlashCmd",
        &[PyRep::String("/unban bot_suspect Appeal accepted".into())],
        &kw,
        None,
    )
    .await
    .expect("GM /unban execution");

    let mut restored_sess = SessionState::new(20_404);
    let restored_login = ctx
        .dispatch_rpc(
            &mut restored_sess,
            "authentication",
            None,
            "Login",
            &[
                PyRep::String("bot_suspect".into()),
                PyRep::String("botpass123".into()),
            ],
            &kw,
            None,
        )
        .await
        .expect("Login after /unban must succeed");
    assert!(matches!(restored_login, PyRep::KeyVal(_)));
}

#[tokio::test]
async fn test_serverops_lossless_static_data() {
    // parity: feat:serverops.static-data
    let db_path = eve_sde::SdeRegistry::default_db_path();
    assert!(
        db_path.exists(),
        "SDE SQLite file at data/evomu_sde.sqlite must exist"
    );

    // 1. Direct SQLite verification of static tables and row counts
    let canon = std::fs::canonicalize(&db_path).unwrap_or_else(|_| db_path.clone());
    let uri = format!("file:{}?immutable=1", canon.display());
    let conn = rusqlite::Connection::open_with_flags(
        &uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .expect("open data/evomu_sde.sqlite directly");

    let count_table = |tbl: &str| -> i64 {
        conn.query_row(&format!("SELECT COUNT(*) FROM {tbl}"), [], |r| r.get(0))
            .unwrap_or_else(|e| panic!("Failed to count table {tbl}: {e}"))
    };

    assert_eq!(count_table("sde_resources"), 213);
    assert!(count_table("sde_types") >= 40_000);
    assert_eq!(count_table("sde_solar_systems"), 8_490);
    assert_eq!(count_table("sde_skin_materials"), 875);
    assert_eq!(count_table("sde_skin_licenses"), 11_877);
    assert_eq!(count_table("sde_skins"), 7_048);
    assert_eq!(count_table("sde_space_components"), 11_238);
    assert_eq!(count_table("sde_market_groups"), 2_114);
    assert_eq!(count_table("sde_dogma_attributes"), 2_871);
    assert_eq!(count_table("sde_dogma_effects"), 3_427);
    assert_eq!(count_table("sde_blueprints"), 5_100);
    assert_eq!(count_table("sde_npc_spawn_groups"), 392);

    // 2. Verify columns added in latest schema revisions exist and are queryable
    let type_desc_id: Option<i64> = conn
        .query_row(
            "SELECT description_id FROM sde_types WHERE type_id = 587",
            [],
            |r| r.get(0),
        )
        .expect("Query description_id on sde_types");
    assert!(type_desc_id.is_some());

    let skin_mat_row: (i64, i64, i64) = conn
        .query_row(
            "SELECT skin_material_id, material_set_id, display_name_id FROM sde_skin_materials LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .expect("Query sde_skin_materials schema");
    assert!(skin_mat_row.0 > 0);

    // 3. SdeRegistry in ServerContext verification
    let ctx = ServerContext::new();
    assert_eq!(ctx.sde.resources.len(), 213);
    assert!(ctx.sde.solar_systems.contains_key(&30_000_142)); // Jita
    assert!(ctx.sde.solar_systems.contains_key(&30_000_144)); // Perimeter
    assert_eq!(ctx.sde.attributes.len(), 2_871);
    assert_eq!(
        ctx.sde.attributes.get(&54).map(|a| a.name.as_str()),
        Some("maxRange")
    );
    assert_eq!(ctx.sde.effects.len(), 3_427);
    assert_eq!(
        ctx.sde.effects.get(&12).map(|e| e.name.as_str()),
        Some("hiPower")
    );
    assert_eq!(ctx.sde.blueprints.len(), 5_100);
    assert!(ctx.sde.blueprints.contains_key(&691));
    assert_eq!(ctx.sde.npc_spawn_groups.len(), 392);

    // Rifter (587) verification
    let rifter = ctx.sde.types.get(&587).expect("Rifter (587) in SdeRegistry");
    assert_eq!(rifter.name, "Rifter");
    assert_eq!(rifter.group_id, 25); // Frigate
    assert!(!ctx.sde.variations_of(587).is_empty());
}

#[tokio::test]
async fn test_s10_doc_reconciliation_and_labels() {
    // parity: plan:S-10
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let plan_md = std::fs::read_to_string(root.join("PLAN.md")).expect("read PLAN.md");
    let readme_md = std::fs::read_to_string(root.join("README.md")).expect("read README.md");
    let parity_matrix_md = std::fs::read_to_string(root.join("docs/index/PARITY_MATRIX.md"))
        .expect("read docs/index/PARITY_MATRIX.md");

    // 1. Prohibited status labels check
    for (name, doc) in [
        ("PLAN.md", &plan_md),
        ("README.md", &readme_md),
        ("docs/index/PARITY_MATRIX.md", &parity_matrix_md),
    ] {
        assert!(
            !doc.contains("Implemented & Verified")
                && !doc.contains("Partial Vertical Slice")
                && !doc.contains("**Completed")
                && !doc.contains("Completed (`D-")
                && !doc.contains("Implemented & e2e-tested")
                && !doc.contains("Verified Exit Criteria")
                && !doc.contains("evomu_sde_fixture.sqlite")
                && !doc.contains('\u{2588}')
                && !doc.contains('\u{2591}'),
            "S-10: {name} must use Rule 8 status labels and not contain prohibited labels or progress bars"
        );
        assert!(
            !doc.contains("src/router.rs") && !doc.contains("tests/e2e_journeys.rs"),
            "S-10: {name} must not reference deleted files"
        );
    }

    // 2. Scan services and catalog to verify exact counts
    let services_dir = root.join("crates/eve-server/src/services");
    let mut single_arms = 0usize;
    let mut multi_arms = 0usize;
    let mut total_handlers = 0usize;
    let mut service_methods: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();

    for entry in std::fs::read_dir(&services_dir).expect("read services dir").flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs")
            || path.file_name().and_then(|n| n.to_str()) == Some("mod.rs")
        {
            continue;
        }
        let src = std::fs::read_to_string(&path).expect("read service file");
        let raw_lines: Vec<&str> = src.lines().collect();
        let mut idx = 0;
        while idx < raw_lines.len() {
            let trimmed = raw_lines[idx].trim();
            let mut combined = trimmed.to_string();
            if combined.starts_with("(\"") {
                while !combined.contains("=>")
                    && idx + 1 < raw_lines.len()
                    && raw_lines[idx + 1].trim().starts_with("| (\"")
                {
                    idx += 1;
                    combined.push(' ');
                    combined.push_str(raw_lines[idx].trim());
                }
            }
            idx += 1;
            if !combined.starts_with("(\"") || !combined.contains("=>") {
                continue;
            }
            let head = combined.split("=>").next().unwrap_or("");
            if head.contains(" if ") {
                continue;
            }
            let mut pairs = Vec::new();
            let mut rest = head;
            while let Some(open) = rest.find("(\"") {
                let after_open = &rest[open + 2..];
                if let Some(q1) = after_open.find('"') {
                    let svc = &after_open[..q1];
                    let after_svc = after_open[q1 + 1..].trim_start();
                    if let Some(after_comma) = after_svc.strip_prefix(',') {
                        let after_comma = after_comma.trim_start();
                        if let Some(after_q2) = after_comma.strip_prefix('"') {
                            if let Some(q2_end) = after_q2.find('"') {
                                let method = &after_q2[..q2_end];
                                pairs.push((svc.to_string(), method.to_string()));
                                rest = &after_q2[q2_end + 1..];
                                continue;
                            }
                        }
                    }
                    rest = after_svc;
                    continue;
                }
                break;
            }
            if pairs.len() == 1 {
                single_arms += 1;
            } else if pairs.len() >= 2 {
                multi_arms += 1;
            }
            total_handlers += pairs.len();
            for (s, m) in pairs {
                service_methods.entry(s).or_default().insert(m);
            }
        }
    }

    let total_arms = single_arms + multi_arms;

    let catalog_raw = std::fs::read_to_string(root.join("docs/index/rpc_service_catalog.json"))
        .expect("read rpc_service_catalog.json");
    let catalog: serde_json::Value =
        serde_json::from_str(&catalog_raw).expect("parse rpc_service_catalog.json");
    let catalog_obj = catalog.as_object().expect("catalog object");

    let mut exact_v24_methods = 0usize;
    let mut total_v24_cands_on_explicit = 0usize;

    for (svc, methods) in &service_methods {
        if let Some(entry) = catalog_obj.get(svc) {
            let cands: BTreeSet<String> = entry
                .get("v24_client_candidate_methods")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|x| x.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            total_v24_cands_on_explicit += cands.len();
            let overlap = methods.intersection(&cands).count();
            if overlap > 0 {
                exact_v24_methods += overlap;
            }
        }
    }

    let expected_cand_ratio = format!("{exact_v24_methods}/{total_v24_cands_on_explicit}");
    let expected_handler_summary =
        format!("`{total_handlers}` `(service, method)` handlers across `{total_arms}` match arms");
    let expected_arm_breakdown =
        format!("`{single_arms}` single-method + `{multi_arms}` multi-method arms");
    for (name, doc) in [("PLAN.md", &plan_md), ("README.md", &readme_md)] {
        assert!(
            doc.contains(&expected_cand_ratio),
            "S-10: {name} must report live client candidate method ratio {expected_cand_ratio}"
        );
        assert!(
            doc.contains(&expected_handler_summary),
            "S-10: {name} must report live handler summary: {expected_handler_summary}"
        );
        assert!(
            doc.contains(&expected_arm_breakdown),
            "S-10: {name} must report live match arm breakdown: {expected_arm_breakdown}"
        );
        assert!(
            doc.contains("V24.01 client candidate methods")
                && doc.contains("`(service, method)` handlers across")
                && doc.contains("single-method +")
                && doc.contains("docs/ACTION_PLAN_STATUS.md"),
            "S-10: {name} must report handler routing summary and reference docs/ACTION_PLAN_STATUS.md"
        );
    }
}

#[tokio::test]
async fn test_s7_structures_skyhooks_pi_range_checks() {
    // parity: plan:S-7
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(20_701);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    // 1. Missing arguments reject cleanly without hardcoded fallbacks
    let missing_skin = ctx
        .dispatch_rpc(
            &mut sess,
            "shipCosmeticsMgr",
            None,
            "ApplySkinToShip",
            &[],
            &kw,
            None,
        )
        .await;
    assert_eq!(missing_skin.unwrap_err().code, "InvalidShipTypeForSkin");

    let missing_mail = ctx
        .dispatch_rpc(&mut sess, "mailMgr", None, "SendMail", &[], &kw, None)
        .await;
    assert_eq!(missing_mail.unwrap_err().code, "InvalidRecipient");

    // 2. PI read-only inspection and system range checks
    let planet_info = ctx
        .dispatch_rpc(
            &mut sess,
            "planetMgr",
            None,
            "GetPlanetInfo",
            &[PyRep::Integer(40_009_077)],
            &kw,
            None,
        )
        .await
        .expect("GetPlanetInfo");
    assert!(matches!(planet_info, PyRep::KeyVal(_)));

    let my_colonies = ctx
        .dispatch_rpc(
            &mut sess,
            "planetMgr",
            None,
            "GetPlanetsForChar",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetPlanetsForChar");
    assert!(matches!(my_colonies, PyRep::Rowset { .. }));

    // Colony and pin creation on invalid planet ID fails
    let invalid_planet = ctx
        .dispatch_rpc(
            &mut sess,
            "planetMgr",
            None,
            "UserUpdateNetwork",
            &[PyRep::Integer(99_999_999)],
            &kw,
            None,
        )
        .await;
    assert_eq!(invalid_planet.unwrap_err().code, "InvalidPlanetID");

    let invalid_pin_planet = ctx
        .dispatch_rpc(
            &mut sess,
            "planetMgr",
            None,
            "UserUpdateNetwork",
            &[
                PyRep::Integer(99_999_999),
                PyRep::List(vec![PyRep::Tuple(vec![
                    PyRep::Integer(1),
                    PyRep::Integer(1),
                    PyRep::Integer(2256),
                    PyRep::Real(0.1),
                    PyRep::Real(0.2),
                ])]),
            ],
            &kw,
            None,
        )
        .await;
    assert_eq!(invalid_pin_planet.unwrap_err().code, "InvalidPlanetID");

    // Colony and pin creation on planet in a different system (Perimeter 30000144 while in Jita 30000142) fails
    let perimeter_planet_id = ctx
        .sde
        .celestials_in_system(30_000_144)
        .into_iter()
        .find(|c| c.group_id == 7)
        .map(|c| c.celestial_id)
        .expect("Perimeter planet");
    let wrong_sys_planet = ctx
        .dispatch_rpc(
            &mut sess,
            "planetMgr",
            None,
            "UserUpdateNetwork",
            &[PyRep::Integer(perimeter_planet_id as i64)],
            &kw,
            None,
        )
        .await;
    assert_eq!(wrong_sys_planet.unwrap_err().code, "PlanetNotInCurrentSystem");

    let wrong_sys_pin = ctx
        .dispatch_rpc(
            &mut sess,
            "planetMgr",
            None,
            "UserUpdateNetwork",
            &[
                PyRep::Integer(perimeter_planet_id as i64),
                PyRep::List(vec![PyRep::Tuple(vec![
                    PyRep::Integer(1),
                    PyRep::Integer(1),
                    PyRep::Integer(2256),
                    PyRep::Real(0.1),
                    PyRep::Real(0.2),
                ])]),
            ],
            &kw,
            None,
        )
        .await;
    assert_eq!(wrong_sys_pin.unwrap_err().code, "PlanetNotInCurrentSystem");

    // 3. sovMgr.AcquireSkyhooks docked check
    let claim_docked = ctx
        .dispatch_rpc(&mut sess, "sovMgr", None, "AcquireSkyhooks", &[], &kw, None)
        .await;
    assert_eq!(claim_docked.unwrap_err().code, "ShipDocked");
}

#[tokio::test]
async fn test_z5_auditor_flagged_rows_closure_suite() {
    // Composite test closing the auditor-flagged ledger rows:
    // - feat:account.account-bans
    // - feat:account.account-roles
    // - feat:account.multi-login-block
    // - feat:admin.gm-ban
    // - feat:serverops.static-data
    // - plan:S-10
    // - plan:S-7

    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let now = ctx.now_unix();

    // A. Static SDE lossless check (feat:serverops.static-data)
    assert_eq!(ctx.sde.resources.len(), 213);
    assert!(!ctx.sde.types.is_empty());
    assert!(!ctx.sde.solar_systems.is_empty());
    assert_eq!(ctx.sde.attributes.len(), 2_871);
    assert_eq!(ctx.sde.effects.len(), 3_427);
    assert_eq!(ctx.sde.blueprints.len(), 5_100);
    assert_eq!(ctx.sde.npc_spawn_groups.len(), 392);

    // B. Role bitmasks and GM status loaded from DB via authentication.Login (feat:account.account-roles)
    ctx.db
        .provision_gm_account("z5_gm", "gm_secret", 90_000_003, "Z5 GM");
    let mut sess_gm = SessionState::new(20_901);
    common::login_pilot(&ctx, &mut sess_gm, "z5_gm", "gm_secret").await;
    assert!(sess_gm.has_gm_role());

    let mut sess_player = SessionState::new(20_902);
    common::login_pilot(&ctx, &mut sess_player, "demo", "demo-secret").await;
    assert!(!sess_player.has_gm_role());

    // C. Multi-login concurrency & displacement (feat:account.multi-login-block)
    ctx.db.provision_player_account("z5_pilot", "z5_secret");
    let mut s1 = SessionState::new(20_903);
    let (s1_close_tx, mut s1_close_rx) =
        tokio::sync::mpsc::unbounded_channel::<(String, String)>();
    ctx.register_session_closer(s1.session_id, s1_close_tx);

    common::login_pilot(&ctx, &mut s1, "z5_pilot", "z5_secret").await;
    let s1_uid = s1.user_id.expect("s1 user_id");
    assert_eq!(
        ctx.db.get_online_session(s1_uid).map(|s| s.session_id),
        Some(s1.session_id)
    );

    let mut s2 = SessionState::new(20_904);
    common::login_pilot(&ctx, &mut s2, "z5_pilot", "z5_secret").await;
    // Displace s1 from s2
    let mlb_res = ctx
        .dispatch_rpc(
            &mut s2,
            "multiLoginBlocker",
            None,
            "Login",
            &[PyRep::String("hw-z5".into()), PyRep::Bool(true)],
            &kw,
            None,
        )
        .await
        .expect("displace login");
    assert_eq!(mlb_res, PyRep::Bool(true));
    let (d_code, _) = s1_close_rx.try_recv().expect("s1 disconnected");
    assert_eq!(d_code, "ACL_DISPLACED");

    // D. GM slash ban with live session revocation (feat:admin.gm-ban)
    let mut gm_runner = sess_gm;
    ctx.dispatch_rpc(
        &mut gm_runner,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_003)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID GM");

    let (s2_close_tx, mut s2_close_rx) =
        tokio::sync::mpsc::unbounded_channel::<(String, String)>();
    ctx.register_session_closer(s2.session_id, s2_close_tx);

    let ban_exec = ctx
        .dispatch_rpc(
            &mut gm_runner,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String("/ban z5_pilot 3600 Z5 Closure Ban".into())],
            &kw,
            None,
        )
        .await
        .expect("/ban execution");
    assert!(matches!(ban_exec, PyRep::KeyVal(_)));

    let (ban_signal, _) = s2_close_rx.try_recv().expect("s2 disconnected on ban");
    assert_eq!(ban_signal, "UserBanned");
    assert!(ctx.db.get_online_session(s1_uid).is_none());

    // E. Ban enforcement and auto-expiry clearing (feat:account.account-bans)
    let mut s3 = SessionState::new(20_906);
    let ban_rejection = ctx
        .dispatch_rpc(
            &mut s3,
            "authentication",
            None,
            "Login",
            &[
                PyRep::String("z5_pilot".into()),
                PyRep::String("z5_secret".into()),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("Banned pilot login rejected");
    assert_eq!(ban_rejection.code, "UserBanned");

    // Set ban expiry in the past and verify auto-clear on authenticate_at
    ctx.db
        .ban_account("z5_pilot", now - 10, "Expired ban")
        .expect("set expired ban");
    let unbanned_auth = ctx
        .db
        .authenticate_at("z5_pilot", "z5_secret", now)
        .expect("auto-clear expired ban on authenticate");
    assert_eq!(unbanned_auth.username, "z5_pilot");
}
