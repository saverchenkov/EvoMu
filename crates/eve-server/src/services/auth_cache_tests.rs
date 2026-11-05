//! Unit tests for `auth_cache` (`machoNet`, `cache`, `authentication`, `subscriptionMgr`, `slash`).

#![forbid(unsafe_code)]

use super::*;
use crate::services::account_services::account_services_tests::{
    auth_session, test_ctx, PyRepTestExt,
};
use eve_db::AccountRecord;
use eve_machonet::service_roles;
use eve_proto::PyRep;
use std::collections::HashMap;

#[tokio::test]
async fn machonet_cache_authentication_and_subscription_rpcs() {
    let ctx = test_ctx();
    let uid = 1_u64;
    let cid = 90_000_001_i64;
    let mut sess = auth_session(uid, cid, 98_000_001, 30_000_142);
    let kw = HashMap::new();

    let init = get_init_vals(&ctx);
    assert!(matches!(init, PyRep::Tuple(_)));

    assert_eq!(
        required_slash_role("/online"),
        service_roles::ROLE_QA | service_roles::ROLE_GM_MASK
    );
    assert_eq!(
        required_slash_role("/spawn 587"),
        service_roles::ROLE_SPAWN | service_roles::ROLE_GM_MASK
    );
    assert_eq!(
        required_slash_role("/role user GMH"),
        service_roles::ROLE_GMH
            | service_roles::ROLE_GMS
            | service_roles::ROLE_ADMIN
            | service_roles::ROLE_ROLEADMIN
    );
    assert_eq!(required_slash_role("/giveisk 100"), service_roles::ROLE_GM_MASK);

    // machoNet RPCs
    assert!(
        handle(&ctx, &mut sess, "machoNet", "GetServiceInfo", &[], &kw)
            .await
            .unwrap()
            .unwrap()
            .as_dict()
            .is_some()
    );
    assert!(
        handle(&ctx, &mut sess, "machoNet", "GetTime", &[], &kw)
            .await
            .unwrap()
            .unwrap()
            .as_i64()
            .unwrap()
            > 0
    );
    assert_eq!(
        handle(&ctx, &mut sess, "machoNet", "NoBlock", &[], &kw)
            .await
            .unwrap()
            .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "machoNet",
            "GetCachedObject",
            &[PyRep::String("nonexistent_obj".into())],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "CachedObjectNotFound"
    );

    // ForwardCharacterNotification & SetGlobalConfigValue require GM role
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "machoNet",
            "ForwardCharacterNotification",
            &[PyRep::Integer(cid)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InsufficientRole"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
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

    sess.role = service_roles::ROLE_GMH;
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "machoNet",
            "ForwardCharacterNotification",
            &[
                PyRep::Integer(cid),
                PyRep::String("OnNotificationReceived".into()),
                PyRep::List(vec![PyRep::Integer(1)]),
            ],
            &kw,
        )
        .await
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "machoNet",
            "SetGlobalConfigValue",
            &[PyRep::String("testKey".into()), PyRep::String("testVal".into())],
            &kw,
        )
        .await
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );

    // cache.GetServerVersionAndBuild, cache.Rowset, cache.Setting
    assert_eq!(
        handle(&ctx, &mut sess, "cache", "GetServerVersionAndBuild", &[], &kw)
            .await
            .unwrap()
            .unwrap(),
        PyRep::Tuple(vec![PyRep::Real(V24_01_VERSION), PyRep::Integer(V24_01_BUILD)])
    );
    for tbl in ["roles", "races", "factions", "bloodlines", "ancestries"] {
        assert!(handle(
            &ctx,
            &mut sess,
            "cache",
            "Rowset",
            &[PyRep::String(tbl.into())],
            &kw,
        )
        .await
        .unwrap()
        .is_ok());
    }
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "cache",
            "Rowset",
            &[PyRep::String("unknown_tbl".into())],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "UnknownCacheRowset"
    );

    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "cache",
            "Setting",
            &[PyRep::String("SP-Allowed-Extensions".into())],
            &kw,
        )
        .await
        .unwrap()
        .unwrap(),
        PyRep::String("txt,csv,log,png,jpg,dds,json,yaml".into())
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "cache",
            "Setting",
            &[
                PyRep::String("sec".into()),
                PyRep::String("k1".into()),
                PyRep::String("v1".into()),
            ],
            &kw,
        )
        .await
        .unwrap()
        .unwrap(),
        PyRep::String("v1".into())
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "cache",
            "Setting",
            &[
                PyRep::String("sec".into()),
                PyRep::String("k1".into()),
                PyRep::None,
            ],
            &kw,
        )
        .await
        .unwrap()
        .unwrap(),
        PyRep::None
    );

    // authentication.SetLanguageID, subscriptionMgr.GetCloneGrade, loginRewardFacilities.get_my_clone_grade
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "authentication",
            "SetLanguageID",
            &[PyRep::String("DE".into())],
            &kw,
        )
        .await
        .unwrap()
        .unwrap(),
        PyRep::None
    );
    assert_eq!(sess.language_id, "DE");

    assert!(
        handle(&ctx, &mut sess, "subscriptionMgr", "GetCloneGrade", &[], &kw)
            .await
            .unwrap()
            .is_ok()
    );
    assert!(
        handle(
            &ctx,
            &mut sess,
            "loginRewardFacilities",
            "get_my_clone_grade",
            &[],
            &kw,
        )
        .await
        .unwrap()
        .is_ok()
    );
}

#[tokio::test]
async fn gm_slash_commands_giveisk_ban_suspend_role_giveitem_setskill_teleport_and_heal() {
    let ctx = test_ctx();
    let uid = 1_u64;
    let cid = 90_000_001_i64;
    let mut sess = auth_session(uid, cid, 98_000_001, 30_000_142);
    let kw = HashMap::new();

    // Non-GM rejected and audited
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "slash",
            "SlashCmd",
            &[PyRep::String("/giveisk 1000".into())],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InsufficientRole"
    );

    sess.role = service_roles::ROLE_GMH | service_roles::ROLE_ADMIN | service_roles::ROLE_SPAWN;

    // /giveisk
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "slash",
            "SlashCmd",
            &[PyRep::String("/giveisk -5".into())],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidSlashArgument"
    );
    let isk_rep = handle(
        &ctx,
        &mut sess,
        "slash",
        "SlashCmd",
        &[PyRep::String("/giveisk 2500".into())],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(isk_rep.dict_get("newBalanceISK").is_some());

    // Seed target account for /ban, /unban, /suspend, /unsuspend, /role, /kick
    ctx.db.with_mut_snapshot(|snap| {
        snap.accounts.insert(
            "slash_target".into(),
            AccountRecord {
                user_id: 777,
                username: "slash_target".into(),
                password_token: String::new(),
                role: eve_db::service_roles::ROLE_PLAYER_DEFAULT,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
    });

    for cmd in [
        "/ban slash_target 3600 testing",
        "/unban slash_target",
        "/suspend slash_target 600 cooldown",
        "/unsuspend slash_target",
        "/role slash_target GML",
        "/kick slash_target bye",
        "/giveitem 34 50",
        "/setskill 3300 4",
        "/setskill 3300 0",
        "/spawnnpc 587",
        "/teleport 30000144",
        "/heal",
        "/createccpevent 150000000000000000 60 1 LiveEvent | Desc",
    ] {
        let res = handle(
            &ctx,
            &mut sess,
            "slash",
            "SlashCmd",
            &[PyRep::String(cmd.into())],
            &kw,
        )
        .await
        .unwrap();
        assert!(res.is_ok(), "slash command {cmd} failed: {res:?}");
    }

    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "slash",
            "SlashCmd",
            &[PyRep::String("/unknowncmd".into())],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "UnknownSlashCommand"
    );
}
