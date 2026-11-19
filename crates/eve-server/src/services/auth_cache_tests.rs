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
        PyRep::Tuple(vec![
            PyRep::Real(eve_machonet::V24_VERSION_NUMBER),
            PyRep::Integer(eve_machonet::V24_BOOT_BUILD as i64),
        ])
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
    let giveisk_usage = handle(
        &ctx,
        &mut sess,
        "slash",
        "SlashCmd",
        &[PyRep::String("/giveisk".into())],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(giveisk_usage.code, "InvalidSlashArgument");
    assert!(
        giveisk_usage.message.contains("Usage: /giveisk"),
        "bare /giveisk must return usage error, got: {giveisk_usage:?}"
    );
    let before_cents = ctx
        .db
        .with_snapshot(|s| s.characters.get(&cid).map(|c| c.balance.raw_cents()))
        .expect("character exists");
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
    let isk_me_rep = handle(
        &ctx,
        &mut sess,
        "slash",
        "SlashCmd",
        &[PyRep::String("/giveisk me 1000".into())],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(isk_me_rep.dict_get("newBalanceISK").is_some());
    let isk_cid_rep = handle(
        &ctx,
        &mut sess,
        "slash",
        "SlashCmd",
        &[PyRep::String(format!("/giveisk {cid} 500"))],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(isk_cid_rep.dict_get("newBalanceISK").is_some());
    let after_cents = ctx
        .db
        .with_snapshot(|s| s.characters.get(&cid).map(|c| c.balance.raw_cents()))
        .expect("character exists");
    assert_eq!(after_cents - before_cents, 400_000);
    for (bad_cmd, expected_code) in [
        ("/giveisk 0 100", "InvalidSlashArgument"),
        ("/giveisk badtarget 100", "InvalidSlashArgument"),
        ("/giveisk 100 200 300", "InvalidSlashArgument"),
        ("/giveisk 922337203685477580", "BalanceOverflow"),
    ] {
        let err = handle(
            &ctx,
            &mut sess,
            "slash",
            "SlashCmd",
            &[PyRep::String(bad_cmd.into())],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(err.code, expected_code, "for command '{bad_cmd}': {err:?}");
    }

    // /help and /help <command>
    let help_rep = handle(
        &ctx,
        &mut sess,
        "slash",
        "SlashCmd",
        &[PyRep::String("/help".into())],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let help_str = help_rep.as_str().expect("/help must return a string");
    assert!(help_str.contains("/giveisk") && help_str.contains("/load") && help_str.contains("/help"));
    let help_slash = handle(
        &ctx,
        &mut sess,
        "slash",
        "SlashCmd",
        &[PyRep::String("/help /".into())],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        help_slash
            .as_str()
            .is_some_and(|s| s.contains("Available slash commands")),
        "expected command summary from /help /, got: {help_slash:?}"
    );
    let help_load = handle(
        &ctx,
        &mut sess,
        "slash",
        "SlashCmd",
        &[PyRep::String("/help /load".into())],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        help_load
            .as_str()
            .is_some_and(|s| s.contains("/load") && s.contains("<typeID>")),
        "expected /load usage from /help /load, got: {help_load:?}"
    );
    let help_tidi = handle(
        &ctx,
        &mut sess,
        "slash",
        "SlashCmd",
        &[PyRep::String("/help tidi".into())],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        help_tidi
            .as_str()
            .is_some_and(|s| s.contains("/tidi: available to your role")),
        "expected role-aware summary from /help tidi, got: {help_tidi:?}"
    );
    let help_unknown = handle(
        &ctx,
        &mut sess,
        "slash",
        "SlashCmd",
        &[PyRep::String("/help nonexistent_cmd".into())],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        help_unknown
            .as_str()
            .is_some_and(|s| s.contains("Unknown slash command '/nonexistent_cmd'")),
        "expected unknown command response from /help nonexistent_cmd, got: {help_unknown:?}"
    );
    let mut gml_sess = sess.clone();
    gml_sess.role = service_roles::ROLE_GML;
    let help_role_denied = handle(
        &ctx,
        &mut gml_sess,
        "slash",
        "SlashCmd",
        &[PyRep::String("/help role".into())],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        help_role_denied
            .as_str()
            .is_some_and(|s| s.contains("/role: requires a higher role")),
        "ROLE_GML asking /help role must note insufficient role, got: {help_role_denied:?}"
    );

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

    // Issue #600: /createitem <ship> 1 must emit OnItemChange DBRow + change {3: 0}
    let cid = sess.character_id.unwrap();
    ctx.db
        .acquire_online_lock(uid, sess.session_id.max(1), Some(cid), true, ctx.now_unix())
        .expect("register online session for /createitem notif");
    let _ = ctx.drain_notifications_for_char(cid);
    let create_ship = handle(
        &ctx,
        &mut sess,
        "slash",
        "SlashCmd",
        &[PyRep::String("/createitem 587 1".into())],
        &kw,
    )
    .await
    .unwrap()
    .expect("/createitem 587 1 ok");
    let created_ship_id = create_ship.dict_get("itemID").and_then(PyRep::as_i64).unwrap();
    let notifs = ctx.drain_notifications_for_char(cid);
    let item_change = notifs
        .iter()
        .find(|n| n.event == "OnItemChange")
        .expect("OnItemChange must be emitted for /createitem");
    if let Some(PyRep::DBRow { columns: cells, .. }) = item_change.args.first() {
        assert_eq!(cells.first().and_then(|(_, v)| v.as_i64()), Some(created_ship_id));
        assert_eq!(cells.get(1).and_then(|(_, v)| v.as_i64()), Some(587));
        assert_eq!(cells.get(7).and_then(|(_, v)| v.as_i64()), Some(6)); // categoryID = Ship
    } else {
        panic!("expected DBRow in /createitem OnItemChange, got {:?}", item_change.args);
    }
    assert_eq!(
        item_change.args.get(1),
        Some(&PyRep::Dict(vec![(PyRep::Integer(3), PyRep::Integer(0))]))
    );
}

#[tokio::test]
async fn createitem_and_giveitem_set_singleton_for_single_modules_and_emit_on_item_change() {
    let ctx = test_ctx();
    let uid = 1_u64;
    let cid = 90_000_001_i64;
    let mut sess = auth_session(uid, cid, 98_000_001, 30_000_142);
    sess.role = service_roles::ROLE_GM_MASK;
    let kw = HashMap::new();
    let _ = ctx.db.acquire_online_lock(uid, sess.session_id, Some(cid), false, ctx.now_unix());
    let _ = ctx.drain_notifications_for_char(cid);

    let mod_res = handle(
        &ctx,
        &mut sess,
        "slash",
        "SlashCmd",
        &[PyRep::String("/createitem 484 1".into())],
        &kw,
    )
    .await
    .expect("handled")
    .expect("createitem 484 1");
    let mod_id = mod_res.dict_get("itemID").and_then(|v| v.as_i64()).unwrap();
    assert!(
        ctx.db.with_snapshot(|s| s.items[&mod_id].singleton),
        "single created module must have singleton=true"
    );
    let mod_notifs = ctx.drain_notifications_for_char(cid);
    assert!(
        mod_notifs.iter().any(|n| n.event == "OnItemChange"),
        "expected OnItemChange for /createitem, got: {:?}",
        mod_notifs.iter().map(|n| &n.event).collect::<Vec<_>>()
    );

    let cargo_res = handle(
        &ctx,
        &mut sess,
        "slash",
        "SlashCmd",
        &[PyRep::String("/giveitem 34 50".into())],
        &kw,
    )
    .await
    .expect("handled")
    .expect("giveitem 34 50");
    let cargo_id = cargo_res.dict_get("itemID").and_then(|v| v.as_i64()).unwrap();
    ctx.db.with_snapshot(|s| {
        assert!(!s.items[&cargo_id].singleton);
        assert_eq!(s.items[&cargo_id].quantity, 50);
    });
    let cargo_notifs = ctx.drain_notifications_for_char(cid);
    assert!(
        cargo_notifs.iter().any(|n| n.event == "OnItemChange"),
        "expected OnItemChange for /giveitem"
    );
}

fn keyval_i64(rep: &PyRep, key: &str) -> Option<i64> {
    match rep {
        PyRep::KeyVal(entries) => entries
            .iter()
            .find(|(k, _)| k.as_str() == key)
            .and_then(|(_, v)| v.as_i64()),
        PyRep::Object { args, .. } => args.as_dict().and_then(|d| {
            d.iter()
                .find(|(k, _)| matches!(k, PyRep::String(s) if s == key))
                .and_then(|(_, v)| v.as_i64())
        }),
        _ => None,
    }
}

#[tokio::test]
async fn slash_tr_moveme_translocate_and_teleport_support_me_and_location_types() {
    let ctx = test_ctx();
    let uid = 1_u64;
    let cid = 90_000_001_i64;
    let ship_id = ctx
        .db
        .with_snapshot(|s| s.characters.get(&cid).map(|c| c.active_ship_id))
        .unwrap_or(0);
    assert!(ship_id > 0);
    let kw = HashMap::new();

    // Verify ALL_REMOTE_SLASH_COMMANDS exposes moveme & translocate for QA/GM roles
    let qa_cmds = remote_slash_commands_for_role(service_roles::ROLE_QA);
    assert!(qa_cmds.contains(&"moveme") && qa_cmds.contains(&"translocate"));
    let gm_cmds = remote_slash_commands_for_role(service_roles::ROLE_GM_MASK);
    for expected in ["teleport", "tr", "moveme", "translocate"] {
        assert!(gm_cmds.contains(&expected), "missing {expected}");
    }

    // 1. /tr me <solarSystemID> (Jita -> Perimeter) and /tr <charID> <solarSystemID> back
    let mut sess = auth_session(uid, cid, 98_000_001, 30_000_142);
    sess.role = service_roles::ROLE_GMH;
    let res = handle(
        &ctx,
        &mut sess,
        "slash",
        "SlashCmd",
        &[PyRep::String("/tr me 30000144".into())],
        &kw,
    )
    .await
    .unwrap()
    .expect("/tr me 30000144");
    assert_eq!(keyval_i64(&res, "solarSystemID"), Some(30_000_144));
    assert_eq!(sess.solar_system_id, Some(30_000_144));
    assert_eq!(sess.station_id, None);

    let res_cid = handle(
        &ctx,
        &mut sess,
        "slash",
        "SlashCmd",
        &[PyRep::String(format!("/tr {cid} 30000142 noblock"))],
        &kw,
    )
    .await
    .unwrap()
    .expect("/tr <cid> 30000142 noblock");
    assert_eq!(keyval_i64(&res_cid, "solarSystemID"), Some(30_000_142));

    // 2. /moveme <stationID> while docked in Jita 4-4 undocks/moves ship in space near station
    sess.role = service_roles::ROLE_QA;
    let _ = ctx.db.dock_character(cid, 60_003_760, false);
    sess.station_id = Some(60_003_760);
    let target_station = ctx
        .sde
        .stations
        .values()
        .find(|s| s.solar_system_id == 30_000_144)
        .or_else(|| ctx.sde.stations.get(&60_003_760))
        .expect("station in SDE")
        .clone();
    let res_st = handle(
        &ctx,
        &mut sess,
        "slash",
        "SlashCmd",
        &[PyRep::String(format!("/moveme {}", target_station.station_id))],
        &kw,
    )
    .await
    .unwrap()
    .expect("/moveme <stationID>");
    assert_eq!(
        keyval_i64(&res_st, "solarSystemID"),
        Some(i64::from(target_station.solar_system_id))
    );
    assert_eq!(sess.station_id, None);
    let st_ball = ctx
        .systems
        .get_or_spawn(target_station.solar_system_id)
        .get_ball_snapshot(ship_id)
        .await
        .unwrap()
        .expect("ship spawned near target station")
        .1;
    assert!((st_ball.position - target_station.position).length() >= 4_500.0);

    // 3. /tr me <sameSystemStationID> while already in space updates Ballpark position
    let jita_st = ctx.sde.stations.get(&60_003_760).unwrap().clone();
    sess.role = service_roles::ROLE_GMH;
    let _ = handle(
        &ctx,
        &mut sess,
        "slash",
        "SlashCmd",
        &[PyRep::String("/tr me 30000142".into())],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let _ = handle(
        &ctx,
        &mut sess,
        "slash",
        "SlashCmd",
        &[PyRep::String("/tr me 60003760".into())],
        &kw,
    )
    .await
    .unwrap()
    .expect("/tr me same-system station");
    let same_sys_ball = ctx
        .systems
        .get_or_spawn(30_000_142)
        .get_ball_snapshot(ship_id)
        .await
        .unwrap()
        .expect("ship ball in Jita")
        .1;
    assert!((same_sys_ball.position - (jita_st.position + DVec3::new(5_000.0, 0.0, 0.0))).length() < 1.0);

    // 4. /translocate <structureID> (active structure succeeds; destroyed structure fails)
    let active_struct_id = 1_000_000_990_001_i64;
    let destroyed_struct_id = 1_000_000_990_002_i64;
    let struct_pos = [12_000_000.0, -3_000_000.0, 4_000_000.0];
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            active_struct_id,
            eve_db::UpwellStructureRecord {
                structure_id: active_struct_id,
                type_id: 35832,
                owner_corp_id: 98_000_001,
                solar_system_id: 30_000_144,
                name: "Test Astrahus".into(),
                position: struct_pos,
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 1.0,
                armor_hp: 1.0,
                hull_hp: 1.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        snap.structures.insert(
            destroyed_struct_id,
            eve_db::UpwellStructureRecord {
                structure_id: destroyed_struct_id,
                type_id: 35832,
                owner_corp_id: 98_000_001,
                solar_system_id: 30_000_144,
                name: "Dead Astrahus".into(),
                position: struct_pos,
                state: "Destroyed".into(),
                quantum_core_installed: false,
                fuel_blocks: 0,
                shield_hp: 0.0,
                armor_hp: 0.0,
                hull_hp: 0.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
    });
    sess.role = service_roles::ROLE_QA;
    let res_str = handle(
        &ctx,
        &mut sess,
        "slash",
        "SlashCmd",
        &[PyRep::String(format!("/translocate me {active_struct_id}"))],
        &kw,
    )
    .await
    .unwrap()
    .expect("/translocate me <structureID>");
    assert_eq!(keyval_i64(&res_str, "solarSystemID"), Some(30_000_144));
    let str_ball = ctx
        .systems
        .get_or_spawn(30_000_144)
        .get_ball_snapshot(ship_id)
        .await
        .unwrap()
        .unwrap()
        .1;
    assert!((str_ball.position - (DVec3::from_array(struct_pos) + DVec3::new(5_000.0, 0.0, 0.0))).length() < 1.0);

    let err_destroyed = handle(
        &ctx,
        &mut sess,
        "slash",
        "SlashCmd",
        &[PyRep::String(format!("/translocate {destroyed_struct_id}"))],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_destroyed.code, "StructureDestroyed");

    // 5. Stargate / celestial / live ball in current system
    if let Some((&gate_id, gate)) = ctx.sde.jumps.iter().next() {
        sess.role = service_roles::ROLE_GMH;
        let res_gate = handle(
            &ctx,
            &mut sess,
            "slash",
            "SlashCmd",
            &[PyRep::String(format!("/tr me {gate_id}"))],
            &kw,
        )
        .await
        .unwrap()
        .expect("/tr me <stargateID>");
        assert_eq!(
            keyval_i64(&res_gate, "solarSystemID"),
            Some(i64::from(gate.from_solar_system_id))
        );
    }
    let dyn_ball_id = 77_000_888_i64;
    let dyn_pos = DVec3::new(987_654.0, -123_456.0, 42_000.0);
    let cur_sys = sess.solar_system_id.unwrap();
    let _ = ctx
        .systems
        .get_or_spawn(cur_sys)
        .spawn_ball(Ball {
            id: dyn_ball_id,
            type_id: 587,
            position: dyn_pos,
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 0.5,
            mass: 1_000_000.0,
            radius: 100.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    sess.role = service_roles::ROLE_GMH;
    handle(
        &ctx,
        &mut sess,
        "slash",
        "SlashCmd",
        &[PyRep::String(format!("/tr me {dyn_ball_id}"))],
        &kw,
    )
    .await
    .unwrap()
    .expect("/tr me <live ballID>");
    let moved_ball = ctx
        .systems
        .get_or_spawn(cur_sys)
        .get_ball_snapshot(ship_id)
        .await
        .unwrap()
        .unwrap()
        .1;
    assert!((moved_ball.position - (dyn_pos + DVec3::new(2_000.0, 0.0, 0.0))).length() < 1.0);

    // 6. Negative cases: missing args, unknown ID, unknown target char, insufficient role
    for bad in ["/tr", "/moveme", "/tr me", "/tr me notanumber"] {
        let err = handle(
            &ctx,
            &mut sess,
            "slash",
            "SlashCmd",
            &[PyRep::String(bad.into())],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(err.code, "InvalidSlashArgument", "cmd {bad}");
    }
    let err_missing = handle(
        &ctx,
        &mut sess,
        "slash",
        "SlashCmd",
        &[PyRep::String("/tr me 999999999".into())],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_missing.code, "SolarSystemNotFound");

    let err_char = handle(
        &ctx,
        &mut sess,
        "slash",
        "SlashCmd",
        &[PyRep::String("/tr 999999998 30000142".into())],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_char.code, "CharacterNotFound");

    sess.role = service_roles::ROLE_PLAYER;
    let err_role = handle(
        &ctx,
        &mut sess,
        "slash",
        "SlashCmd",
        &[PyRep::String("/moveme 30000142".into())],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_role.code, "InsufficientRole");
}

#[tokio::test]
async fn createitem_and_giveitem_blueprint_emits_on_blueprints_updated() {
    // Issue #988: `/createitem` and `/giveitem` for blueprint types (`category_id == 9` /
    // `sde.blueprints.contains_key(&type_id)`) must emit `OnBlueprintsUpdated(ownerID)` alongside
    // `OnItemChange` so V24.01 `BlueprintService` invalidates its `@Memoize(15)` cache.
    let ctx = test_ctx();
    let uid = 1_u64;
    let cid = 90_000_001_i64;
    let mut sess = auth_session(uid, cid, 98_000_001, 30_000_142);
    sess.role = service_roles::ROLE_GM_MASK;
    let kw = HashMap::new();
    let _ = ctx
        .db
        .acquire_online_lock(uid, sess.session_id.max(1), Some(cid), true, ctx.now_unix());
    let _ = ctx.drain_notifications_for_char(cid);

    let bp_tid: u32 = if ctx.sde.blueprints.contains_key(&967) {
        967
    } else {
        *ctx.sde.blueprints.keys().next().expect("SDE has blueprints")
    };

    for slash_str in [
        format!("/createitem {bp_tid} 1"),
        format!("/giveitem {bp_tid} 1"),
    ] {
        let _ = ctx.drain_notifications_for_char(cid);
        let res = handle(
            &ctx,
            &mut sess,
            "slash",
            "SlashCmd",
            &[PyRep::String(slash_str.clone())],
            &kw,
        )
        .await
        .expect("handled")
        .expect("blueprint slash command ok");
        assert_eq!(res.dict_get("typeID"), Some(&PyRep::Integer(bp_tid as i64)));

        let notifs = ctx.drain_notifications_for_char(cid);
        assert!(
            notifs.iter().any(|n| n.event == "OnItemChange"),
            "expected OnItemChange for {slash_str}"
        );
        let bp_notif = notifs
            .iter()
            .find(|n| n.event == "OnBlueprintsUpdated")
            .expect("blueprint creation must emit OnBlueprintsUpdated");
        assert_eq!(bp_notif.id_type, "ownerID");
        assert_eq!(bp_notif.args, vec![PyRep::Integer(cid)]);
    }

    // Negative: non-blueprint items (ship 587, mineral 34) must NOT emit OnBlueprintsUpdated
    for non_bp_cmd in ["/createitem 587 1", "/giveitem 34 10"] {
        let _ = ctx.drain_notifications_for_char(cid);
        let _ = handle(
            &ctx,
            &mut sess,
            "slash",
            "SlashCmd",
            &[PyRep::String(non_bp_cmd.into())],
            &kw,
        )
        .await
        .expect("handled")
        .expect("non-bp slash ok");
        let notifs = ctx.drain_notifications_for_char(cid);
        assert!(notifs.iter().any(|n| n.event == "OnItemChange"));
        assert!(
            !notifs.iter().any(|n| n.event == "OnBlueprintsUpdated"),
            "non-blueprint command {non_bp_cmd} must not emit OnBlueprintsUpdated"
        );
    }

    // Negative: zero quantity and unprivileged player session fail without emitting notifications
    let _ = ctx.drain_notifications_for_char(cid);
    let zero_err = handle(
        &ctx,
        &mut sess,
        "slash",
        "SlashCmd",
        &[PyRep::String(format!("/createitem {bp_tid} 0"))],
        &kw,
    )
    .await
    .expect("handled")
    .expect_err("zero quantity must fail");
    assert_eq!(zero_err.code, "InvalidSlashArgument");
    assert!(ctx.drain_notifications_for_char(cid).is_empty());

    sess.role = service_roles::ROLE_PLAYER;
    let role_err = handle(
        &ctx,
        &mut sess,
        "slash",
        "SlashCmd",
        &[PyRep::String(format!("/createitem {bp_tid} 1"))],
        &kw,
    )
    .await
    .expect("handled")
    .expect_err("non-GM player must be rejected");
    assert_eq!(role_err.code, "InsufficientRole");
    assert!(ctx.drain_notifications_for_char(cid).is_empty());
}

#[tokio::test]
async fn slash_tr_mid_warp_stops_warp_and_flushes_pre_set_state_tick_gh994() {
    let ctx = test_ctx();
    let cid = 90_000_001_i64;
    let ship_id = ctx
        .db
        .with_snapshot(|s| s.characters.get(&cid).map(|c| c.active_ship_id))
        .unwrap_or(0);
    assert!(ship_id > 0);
    let kw = HashMap::new();
    let jita_sys = 30_000_142_u32;
    let perim_sys = 30_000_144_u32;

    // 1. Same-system /tr while mid-warp: must clear active_warps/return_warps, keep Stop +
    //    effects.Warping(0) + SetBallPosition queued for one atomic DoDestinyUpdate WITHOUT SetState
    //    (so DoBallClear does not wedge WarpLoop/signal_in_warp or reset Overview to 0 m, #1005/#1006),
    //    and leave the ball Stopped.
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&cid) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = jita_sys;
        }
    });
    let mut sess = auth_session(1, cid, 98_000_001, jita_sys);
    sess.role = service_roles::ROLE_GMH;
    let jita_handle = ctx.systems.get_or_spawn(jita_sys);
    let ball = eve_destiny::Ball {
        id: ship_id,
        type_id: 587,
        position: glam::DVec3::new(10_000.0, 0.0, 0.0),
        velocity: glam::DVec3::new(3.0 * eve_destiny::AU_METERS, 0.0, 0.0),
        max_velocity: 300.0,
        inertia: 0.5,
        mass: 1_000_000.0,
        radius: 50.0,
        speed_fraction: 1.0,
        mode: MovementMode::Warp {
            origin: glam::DVec3::ZERO,
            destination: glam::DVec3::new(1.0e12, 0.0, 0.0),
            warp_speed_au_s: 3.0,
            total_distance: 1.0e12,
            phase: eve_destiny::WarpPhase::Cruising,
            elapsed_warp_time: 4.0,
        },
    };
    let combat = ctx.build_ship_combat_state(cid, ship_id, 587);
    let slim = ctx.build_ship_slim_meta(cid, 587);
    assert!(jita_handle.spawn_ball_with_meta(ball, combat, slim).await.is_ok());
    // Consume initial spawn observer events so only post-warp-stop updates remain.
    let _ = jita_handle.drain_observer_updates(ship_id).await;
    ctx.active_warps
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(ship_id, cid);
    ctx.return_warps
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(ship_id);
    let mut jita_rx = jita_handle.subscribe();

    let same_res = handle(
        &ctx,
        &mut sess,
        "slash",
        "SlashCmd",
        &[PyRep::String("/tr me 60003760".into())],
        &kw,
    )
    .await
    .expect("handled")
    .expect("same-system /tr mid-warp succeeds");
    assert_eq!(keyval_i64(&same_res, "solarSystemID"), Some(i64::from(jita_sys)));
    assert!(!ctx.active_warps.lock().unwrap().contains_key(&ship_id));
    assert!(!ctx.return_warps.lock().unwrap().contains(&ship_id));
    assert!(
        jita_rx.try_recv().is_err(),
        "same-system /tr must not emit a premature TickBroadcast splitting Stop from SetBallPosition (#1005/#1006)"
    );

    let (_, post_evs, _) = jita_handle
        .drain_observer_updates(ship_id)
        .await
        .expect("post-teleport drain");
    assert!(!post_evs.iter().any(|ev| matches!(
        ev,
        eve_destiny::DestinyEvent::SetState { .. }
    )), "same-system /tr must update incrementally without SetState/DoBallClear (#1005/#1006)");
    assert!(post_evs.iter().any(|ev| matches!(
        ev,
        eve_destiny::DestinyEvent::SetBallMassive { ball_id, is_massive: true } if *ball_id == ship_id
    )));
    assert!(post_evs.iter().any(|ev| matches!(
        ev,
        eve_destiny::DestinyEvent::OnSpecialFX { ball_id, guid, is_active: false, .. }
            if *ball_id == ship_id && guid == "effects.Warping"
    )));
    let stop_idx = post_evs.iter().position(|ev| matches!(
        ev,
        eve_destiny::DestinyEvent::Stop { ball_id } if *ball_id == ship_id
    )).expect("post-teleport drain must include Stop");
    let pos_idx = post_evs.iter().position(|ev| matches!(
        ev,
        eve_destiny::DestinyEvent::SetBallPosition { ball_id, .. } if *ball_id == ship_id
    )).expect("post-teleport drain must include SetBallPosition");
    assert!(stop_idx < pos_idx, "Stop must precede SetBallPosition so client is out of warp before repositioning");
    let (_, same_snap) = jita_handle
        .get_ball_snapshot(ship_id)
        .await
        .unwrap()
        .expect("ship ball present after same-system /tr");
    assert_eq!(same_snap.mode, MovementMode::Stop);
    assert_eq!(same_snap.velocity, glam::DVec3::ZERO);

    // 2. Cross-system /tr while mid-warp: must flush warp-stop on old system before take_ball
    //    and spawn stopped in destination system without premature SetState (#1067).
    let mut warp_ball = same_snap;
    warp_ball.velocity = glam::DVec3::new(2.0 * eve_destiny::AU_METERS, 0.0, 0.0);
    warp_ball.mode = MovementMode::Warp {
        origin: glam::DVec3::ZERO,
        destination: glam::DVec3::new(1.0e12, 0.0, 0.0),
        warp_speed_au_s: 3.0,
        total_distance: 1.0e12,
        phase: eve_destiny::WarpPhase::Cruising,
        elapsed_warp_time: 2.0,
    };
    let combat2 = ctx.build_ship_combat_state(cid, ship_id, 587);
    let slim2 = ctx.build_ship_slim_meta(cid, 587);
    let _ = jita_handle.take_ball(ship_id).await;
    assert!(jita_handle.spawn_ball_with_meta(warp_ball, combat2, slim2).await.is_ok());
    let _ = jita_handle.drain_observer_updates(ship_id).await;
    ctx.active_warps
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(ship_id, cid);
    while jita_rx.try_recv().is_ok() {}

    let cross_res = handle(
        &ctx,
        &mut sess,
        "slash",
        "SlashCmd",
        &[PyRep::String(format!("/tr me {perim_sys}"))],
        &kw,
    )
    .await
    .expect("handled")
    .expect("cross-system /tr mid-warp succeeds");
    assert_eq!(keyval_i64(&cross_res, "solarSystemID"), Some(i64::from(perim_sys)));
    assert!(!ctx.active_warps.lock().unwrap().contains_key(&ship_id));
    assert!(!ctx.return_warps.lock().unwrap().contains(&ship_id));

    let old_bcast = jita_rx
        .try_recv()
        .expect("old system must emit warp-stop TickBroadcast before take_ball");
    assert_eq!(old_bcast.solar_system_id, jita_sys);
    let old_obs = old_bcast
        .observer_events
        .get(&ship_id)
        .expect("old system broadcast must target ship_id");
    assert!(old_obs.iter().any(|ev| matches!(
        ev,
        eve_destiny::DestinyEvent::OnSpecialFX { ball_id, guid, is_active: false, .. }
            if *ball_id == ship_id && guid == "effects.Warping"
    )));
    assert!(old_obs.iter().any(|ev| matches!(
        ev,
        eve_destiny::DestinyEvent::Stop { ball_id } if *ball_id == ship_id
    )));

    let perim_handle = ctx.systems.get_or_spawn(perim_sys);
    let (_, dest_snap) = perim_handle
        .get_ball_snapshot(ship_id)
        .await
        .unwrap()
        .expect("ship spawned in destination system");
    assert_eq!(dest_snap.mode, MovementMode::Stop);
    assert_eq!(dest_snap.velocity, glam::DVec3::ZERO);
    let (_, dest_evs, _) = perim_handle
        .drain_observer_updates(ship_id)
        .await
        .expect("destination drain");
    assert!(
        !dest_evs
            .iter()
            .any(|ev| matches!(ev, eve_destiny::DestinyEvent::SetState { .. })),
        "cross-system /tr must not push SetState before beyonce.MachoBindObject (#1067): {dest_evs:?}"
    );
}

#[tokio::test]
async fn slash_tr_same_system_zeroes_client_velocity_after_warp_and_subwarp_gh1025() {
    use eve_destiny::{Ball, DestinyEvent, MovementMode};
    use glam::DVec3;

    let ctx = test_ctx();
    let uid = 98_u64;
    let cid = 90_000_098_i64;
    let ship_id = 98_000_098_i64;
    let jita_id = 30_000_142_u32;
    let station_id = 60_003_760_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap
            .characters
            .get(&90_000_001)
            .cloned()
            .expect("seed char");
        ch.character_id = cid;
        ch.user_id = uid;
        ch.solar_system_id = jita_id;
        ch.station_id = None;
        ch.active_ship_id = ship_id;
        snap.characters.insert(cid, ch);

        snap.items.insert(
            ship_id,
            eve_db::InventoryItemRecord {
                item_id: ship_id,
                type_id: 32880,
                owner_id: cid,
                location_id: jita_id as i64,
                flag: eve_db::InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    let mut sess = auth_session(uid, cid, ship_id, jita_id);
    sess.role = service_roles::ROLE_GM_MASK | service_roles::ROLE_ADMIN;
    let kw = HashMap::new();
    let sys_h = ctx.systems.get_or_spawn(jita_id);

    let station_pos = ctx
        .sde
        .stations
        .get(&station_id)
        .map(|s| s.position)
        .expect("Jita 4-4 CNAP");
    let far_start = station_pos + DVec3::new(42.48 * eve_destiny::AU_METERS, 0.0, 0.0);

    sys_h
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 32880,
            position: far_start,
            velocity: DVec3::new(-252_511_539_724.72, 0.0, 0.0),
            max_velocity: 335.0,
            inertia: 3.919896,
            mass: 1_200_000.0,
            radius: 40.0,
            speed_fraction: 1.0,
            mode: MovementMode::new_warp(far_start, station_pos, 5.0),
        })
        .await
        .expect("spawn far warping Venture");
    {
        let mut bp = sys_h.ballpark.write().unwrap();
        bp.balls.get_mut(&ship_id).unwrap().velocity = DVec3::new(-252_511_539_724.72, 0.0, 0.0);
        bp.drain_observer_events(ship_id);
    }

    let tr_res = handle(
        &ctx,
        &mut sess,
        "slash",
        "SlashCmd",
        &[PyRep::String(format!("/tr me {station_id}"))],
        &kw,
    )
    .await;
    assert!(matches!(tr_res, Some(Ok(_))), "unexpected /tr res: {tr_res:?}");

    let snap_after_warp_tr = sys_h
        .get_ball_snapshot(ship_id)
        .await
        .unwrap()
        .expect("ship in system");
    assert!(matches!(snap_after_warp_tr.1.mode, MovementMode::Stop));
    assert_eq!(snap_after_warp_tr.1.velocity, DVec3::ZERO);
    assert!(
        snap_after_warp_tr.1.position.distance(station_pos) < 200_000.0,
        "ship must land on station grid (~95km), got dist={}",
        snap_after_warp_tr.1.position.distance(station_pos)
    );

    let (_, obs_evs, _) = sys_h
        .drain_observer_updates(ship_id)
        .await
        .expect("drain warp /tr events");
    let stop_idx = obs_evs
        .iter()
        .position(|e| matches!(e, DestinyEvent::Stop { ball_id } if *ball_id == ship_id))
        .expect("mid-warp /tr must emit Stop");
    let zero_vel_idx = obs_evs
        .iter()
        .position(|e| matches!(
            e,
            DestinyEvent::SetBallVelocity { ball_id, velocity }
                if *ball_id == ship_id && *velocity == [0.0, 0.0, 0.0]
        ))
        .expect("mid-warp /tr must emit SetBallVelocity([0,0,0])");
    let pos_idx = obs_evs
        .iter()
        .position(|e| matches!(e, DestinyEvent::SetBallPosition { ball_id, .. } if *ball_id == ship_id))
        .expect("mid-warp /tr must emit SetBallPosition");
    assert!(
        stop_idx < zero_vel_idx && zero_vel_idx < pos_idx,
        "expected Stop -> SetBallVelocity(0) -> SetBallPosition order: {obs_evs:?}"
    );

    // Same-system /tr from active subwarp motion must also zero velocity and emit SetBallVelocity(0).
    let subwarp_start = station_pos + DVec3::new(10_000_000.0, 0.0, 0.0);
    assert!(sys_h.set_ball_position(ship_id, subwarp_start).await);
    let _ = sys_h
        .set_movement_mode(ship_id, MovementMode::GotoDirection { direction: DVec3::X })
        .await;
    {
        let mut bp = sys_h.ballpark.write().unwrap();
        bp.balls.get_mut(&ship_id).unwrap().velocity = DVec3::new(335.0, 0.0, 0.0);
        bp.drain_observer_events(ship_id);
    }

    let tr_sub = handle(
        &ctx,
        &mut sess,
        "slash",
        "SlashCmd",
        &[PyRep::String(format!("/tr me {station_id}"))],
        &kw,
    )
    .await;
    assert!(matches!(tr_sub, Some(Ok(_))), "unexpected /tr subwarp res: {tr_sub:?}");
    let snap_after_sub_tr = sys_h
        .get_ball_snapshot(ship_id)
        .await
        .unwrap()
        .expect("ship in system");
    assert!(matches!(snap_after_sub_tr.1.mode, MovementMode::Stop));
    assert_eq!(snap_after_sub_tr.1.velocity, DVec3::ZERO);

    let (_, sub_evs, _) = sys_h
        .drain_observer_updates(ship_id)
        .await
        .expect("drain subwarp /tr events");
    assert!(
        sub_evs.iter().any(|e| matches!(
            e,
            DestinyEvent::SetBallVelocity { ball_id, velocity }
                if *ball_id == ship_id && *velocity == [0.0, 0.0, 0.0]
        )),
        "subwarp same-system /tr must emit SetBallVelocity([0,0,0]): {sub_evs:?}"
    );
    assert!(
        sub_evs.iter().any(|e| matches!(e, DestinyEvent::SetBallPosition { ball_id, .. } if *ball_id == ship_id)),
        "subwarp same-system /tr must emit SetBallPosition: {sub_evs:?}"
    );
}

#[tokio::test]
async fn machonet_get_time_stays_anchored_to_wallclock_across_advance_clock() {
    let ctx = test_ctx();
    let mut sess = auth_session(1, 90_000_001, 98_000_001, 30_000_142);
    sess.role = service_roles::ROLE_GML;
    let kw = HashMap::new();
    let five_secs_ticks: i64 = 5 * 10_000_000;

    let wall_before = crate::router::context::host_wall_blueos_filetime_ticks();
    let t_initial = handle(&ctx, &mut sess, "machoNet", "GetTime", &[], &kw)
        .await
        .unwrap()
        .unwrap()
        .as_i64()
        .unwrap();
    assert!(
        (t_initial - wall_before).abs() < five_secs_ticks,
        "initial machoNet.GetTime ({t_initial}) should track host wallclock ({wall_before})"
    );

    // #1031: timeControl.AdvanceClock and /advancetime advance ctx.clock() without emitting
    // DoSimClockRebase. machoNet.GetTime must keep returning host wallclock (matching CCP
    // blue.os.GetWallclockTimeNow()) so client ClockSyncDaemon never drives timeSyncAdjustFactor
    // to 0.95 (1.95x client clock speed vs 1.0x server physics).
    ctx.dispatch_rpc(
        &mut sess,
        "timeControl",
        None,
        "AdvanceClock",
        &[PyRep::Integer(120)],
        &kw,
        None,
    )
    .await
    .expect("AdvanceClock +120s");
    let wall_mid = crate::router::context::host_wall_blueos_filetime_ticks();
    let t_after_adv = handle(&ctx, &mut sess, "machoNet", "GetTime", &[], &kw)
        .await
        .unwrap()
        .unwrap()
        .as_i64()
        .unwrap();
    assert!(
        (t_after_adv - wall_mid).abs() < five_secs_ticks,
        "machoNet.GetTime ({t_after_adv}) must not shift with timeControl.AdvanceClock ({wall_mid})"
    );
    assert!(
        ctx.now_blueos_filetime_ticks() - t_after_adv >= 110 * 10_000_000,
        "server ctx.clock() should advance by +120s independently of machoNet.GetTime"
    );

    ctx.dispatch_rpc(
        &mut sess,
        "slash",
        None,
        "SlashCmd",
        &[PyRep::String("/advancetime 3600".to_string())],
        &kw,
        None,
    )
    .await
    .expect("/advancetime +3600s");
    let wall_after = crate::router::context::host_wall_blueos_filetime_ticks();
    let t_after_slash = handle(&ctx, &mut sess, "machoNet", "GetTime", &[], &kw)
        .await
        .unwrap()
        .unwrap()
        .as_i64()
        .unwrap();
    assert!(
        (t_after_slash - wall_after).abs() < five_secs_ticks,
        "machoNet.GetTime ({t_after_slash}) must stay anchored to wallclock after /advancetime ({wall_after})"
    );
    assert!(
        ctx.now_blueos_filetime_ticks() - t_after_slash >= 3_700 * 10_000_000,
        "server ctx.clock() should advance by +3720s total while machoNet.GetTime stays at wallclock"
    );

    // Rebased virtual clock advances (advance_virtual_clock / advance_secs) still propagate to machoNet.GetTime.
    ctx.advance_virtual_clock(60);
    let t_after_rebased = handle(&ctx, &mut sess, "machoNet", "GetTime", &[], &kw)
        .await
        .unwrap()
        .unwrap()
        .as_i64()
        .unwrap();
    assert!(
        t_after_rebased >= t_after_slash + 60 * 10_000_000
            && t_after_rebased < t_after_slash + 65 * 10_000_000,
        "advance_virtual_clock(60) should advance machoNet.GetTime by ~60s (before={t_after_slash}, after={t_after_rebased})"
    );
}

#[tokio::test]
async fn machonet_get_time_stays_anchored_to_wallclock_across_sqlite_restart() {
    let dir = std::env::temp_dir().join(format!(
        "evomu_get_time_restart_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock after epoch")
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let db_path = dir.join("world.sqlite");
    let sde = std::sync::Arc::clone(&test_ctx().sde);
    let kw = HashMap::new();
    let five_secs_ticks: i64 = 5 * 10_000_000;

    // Boot 1: /advancetime (+791_825s unrebased) + rebased advance_virtual_clock (+120s) + clean shutdown.
    {
        let db1 = std::sync::Arc::new(
            eve_db::WorldRepository::open_sqlite(&db_path).expect("open sqlite boot 1"),
        );
        let ctx1 = crate::router::ServerContext::with_sde_and_repository(
            std::sync::Arc::clone(&sde),
            db1,
        );
        ctx1.advance_secs_without_sim_clock_rebase(791_825);
        ctx1.advance_virtual_clock(120);
        assert_eq!(ctx1.clock().offset_secs(), 791_945);
        assert_eq!(
            ctx1.unrebased_clock_offset_secs
                .load(std::sync::atomic::Ordering::Acquire),
            791_825
        );
        crate::sp6_lifecycle::flush_all_systems_final_checkpoints(&ctx1).await;
    }

    // Boot 2: reopening SQLite restores both clock_offset_secs and unrebased_clock_offset_secs,
    // keeping machoNet.GetTime anchored to host wallclock + 120s (not +791_945s / +9.16d).
    {
        let db2 = std::sync::Arc::new(
            eve_db::WorldRepository::open_sqlite(&db_path).expect("open sqlite boot 2"),
        );
        assert_eq!(db2.clock().offset_secs(), 791_945);
        assert_eq!(
            db2.persisted_unrebased_clock_offset_secs()
                .expect("persisted unrebased offset"),
            791_825
        );
        let ctx2 = crate::router::ServerContext::with_sde_and_repository(
            std::sync::Arc::clone(&sde),
            db2,
        );
        assert_eq!(
            ctx2.unrebased_clock_offset_secs
                .load(std::sync::atomic::Ordering::Acquire),
            791_825
        );
        let mut sess = auth_session(1, 90_000_001, 98_000_001, 30_000_142);
        let wall_before = crate::router::context::host_wall_blueos_filetime_ticks();
        let reply = handle(&ctx2, &mut sess, "machoNet", "GetTime", &[], &kw)
            .await
            .expect("GetTime handled")
            .expect("GetTime succeeds");
        let t_restarted = reply.as_i64().expect("GetTime integer");
        let expected_ticks = wall_before + 120 * 10_000_000;
        assert!(
            (t_restarted - expected_ticks).abs() < five_secs_ticks,
            "machoNet.GetTime ({t_restarted}) after restart must stay at wallclock+120s ({expected_ticks}), not jump +9.16d"
        );
        assert!(
            ctx2.now_blueos_filetime_ticks() - t_restarted >= 791_800 * 10_000_000,
            "ctx2.clock() must still preserve the full +791_825s unrebased offset for timers/queues"
        );

        // Simulate legacy SQLite DB from before #1061 (only clock_offset_secs persisted).
        ctx2.db
            .with_sqlite_conn(|conn| {
                conn.execute(
                    "DELETE FROM world_meta WHERE meta_key = 'unrebased_clock_offset_secs'",
                    [],
                )
                .map_err(|e| e.to_string())?;
                Ok::<_, String>(())
            })
            .expect("delete unrebased_clock_offset_secs row");
    }

    // Boot 3: migration seeds unrebased_clock_offset_secs = clock_offset_secs so legacy DBs do not jump.
    {
        let db3 = std::sync::Arc::new(
            eve_db::WorldRepository::open_sqlite(&db_path).expect("open sqlite boot 3"),
        );
        assert_eq!(db3.clock().offset_secs(), 791_945);
        assert_eq!(
            db3.persisted_unrebased_clock_offset_secs()
                .expect("migrated unrebased offset"),
            791_945
        );
        let ctx3 = crate::router::ServerContext::with_sde_and_repository(sde, db3);
        let mut sess = auth_session(1, 90_000_001, 98_000_001, 30_000_142);
        let wall_before = crate::router::context::host_wall_blueos_filetime_ticks();
        let reply = handle(&ctx3, &mut sess, "machoNet", "GetTime", &[], &kw)
            .await
            .expect("GetTime handled")
            .expect("GetTime succeeds");
        let t_migrated = reply.as_i64().expect("GetTime integer");
        assert!(
            (t_migrated - wall_before).abs() < five_secs_ticks,
            "migrated legacy DB machoNet.GetTime ({t_migrated}) must stay anchored to host wallclock ({wall_before})"
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn gm_giveskill_sets_syncs_removes_targets_and_prunes_queue_gh1137_1138() {
    let ctx = test_ctx();
    let uid = 1_u64;
    let cid = 90_000_001_i64;
    let other_uid = 2_u64;
    let other_cid = 90_000_002_i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(mut ch) = snap.characters.get(&cid).cloned() {
            ch.character_id = other_cid;
            ch.user_id = other_uid;
            ch.name = "SkillTargetAlt".into();
            snap.characters.insert(other_cid, ch);
        }
    });

    let mut sess = auth_session(uid, cid, 98_000_001, 30_000_142);
    sess.role = service_roles::ROLE_GM_MASK;
    let kw = HashMap::new();
    ctx.db
        .acquire_online_lock(uid, sess.session_id.max(1), Some(cid), true, ctx.now_unix())
        .expect("online lock cid");
    ctx.db
        .acquire_online_lock(other_uid, 202, Some(other_cid), true, ctx.now_unix())
        .expect("online lock other_cid");
    let _ = ctx.drain_notifications_for_char(cid);
    let _ = ctx.drain_notifications_for_char(other_cid);

    // 1. Granting 3335 (Amarr Cruiser) emits OnServerSkillsChanged for 3335 only;
    //    3333 (Minmatar Cruiser, required by Rupture 629) stays absent until granted.
    let rep_amarr = ctx
        .dispatch_rpc(
            &mut sess,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String("/giveskill 3335 5".into())],
            &kw,
            None,
        )
        .await
        .expect("/giveskill 3335 5");
    assert_eq!(rep_amarr.dict_get("skillID"), Some(&PyRep::Integer(3335)));
    assert_eq!(
        rep_amarr.dict_get("skillName"),
        Some(&PyRep::String("Amarr Cruiser".into()))
    );
    assert_eq!(rep_amarr.dict_get("level"), Some(&PyRep::Integer(5)));

    let notifs = ctx.drain_notifications_for_char(cid);
    let skills_changed = notifs
        .iter()
        .find(|n| n.event == "OnServerSkillsChanged")
        .expect("OnServerSkillsChanged on /giveskill 3335 5");
    let PyRep::Dict(entries) = &skills_changed.args[0] else {
        panic!("OnServerSkillsChanged skillInfos must be Dict");
    };
    let (_, amarr_info) = entries
        .iter()
        .find(|(k, _)| *k == PyRep::Integer(3335))
        .expect("3335 in OnServerSkillsChanged");
    assert_eq!(
        amarr_info.dict_get("trainedSkillLevel"),
        Some(&PyRep::Integer(5))
    );
    assert_eq!(
        amarr_info.dict_get("effectiveSkillLevel"),
        Some(&PyRep::Integer(5))
    );
    assert_eq!(
        amarr_info.dict_get("trainedSkillPoints"),
        Some(&PyRep::Real(1_280_000.0))
    );

    let wire_skills = ctx
        .dispatch_rpc(
            &mut sess,
            "skillMgr2",
            None,
            "GetSkills",
            &[],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .expect("skillMgr2.GetSkills");
    let PyRep::Dict(wire_dict) = wire_skills else {
        panic!("normalized GetSkills must be Dict");
    };
    assert!(wire_dict.iter().any(|(k, _)| *k == PyRep::Integer(3335)));
    assert!(!wire_dict.iter().any(|(k, _)| *k == PyRep::Integer(3333)));

    let rep_minmatar = ctx
        .dispatch_rpc(
            &mut sess,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String("/giveskill 3333 5".into())],
            &kw,
            None,
        )
        .await
        .expect("/giveskill 3333 5");
    assert_eq!(
        rep_minmatar.dict_get("skillName"),
        Some(&PyRep::String("Minmatar Cruiser".into()))
    );
    let notifs_min = ctx.drain_notifications_for_char(cid);
    assert!(
        notifs_min.iter().any(|n| n.event == "OnServerSkillsChanged"),
        "expected OnServerSkillsChanged for 3333"
    );

    // 2. Removing a previously trained skill with level 0 must emit negative SP (-1.0)
    //    so V24 SkillsSvc deletes it instead of retaining the previous trained level.
    let _ = ctx
        .dispatch_rpc(
            &mut sess,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String("/setskill 3335 0".into())],
            &kw,
            None,
        )
        .await
        .expect("/setskill 3335 0");
    assert!(ctx.db.with_snapshot(|s| {
        s.skills
            .get(&cid)
            .is_none_or(|m| !m.contains_key(&3335))
    }));
    let notifs_rem = ctx.drain_notifications_for_char(cid);
    let rem = notifs_rem
        .iter()
        .find(|n| n.event == "OnServerSkillsRemoved")
        .expect("OnServerSkillsRemoved when removing trained skill");
    let PyRep::Dict(rem_entries) = &rem.args[0] else {
        panic!("OnServerSkillsRemoved skillInfos must be Dict");
    };
    let (_, rem_info) = rem_entries
        .iter()
        .find(|(k, _)| *k == PyRep::Integer(3335))
        .expect("3335 in OnServerSkillsRemoved");
    assert_eq!(rem_info.dict_get("skillPoints"), Some(&PyRep::Real(-1.0)));
    assert_eq!(
        rem_info.dict_get("trainedSkillPoints"),
        Some(&PyRep::Real(-1.0))
    );
    assert_eq!(rem_info.dict_get("virtualSkillLevel"), Some(&PyRep::None));

    // Repeating level 0 for an already-absent skill must not emit a bogus removal notice.
    let _ = ctx
        .dispatch_rpc(
            &mut sess,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String("/giveskill 3335 0".into())],
            &kw,
            None,
        )
        .await
        .expect("/giveskill 3335 0 repeat");
    assert!(
        !ctx.drain_notifications_for_char(cid)
            .iter()
            .any(|n| n.event == "OnServerSkillsRemoved"),
        "removing an already absent skill must not emit OnServerSkillsRemoved"
    );

    // 3. [me|<characterID>] targeting updates target_cid cleanly and sends notices to target_cid.
    let rep_other = ctx
        .dispatch_rpc(
            &mut sess,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String(format!("/giveskill {other_cid} 3300 3"))],
            &kw,
            None,
        )
        .await
        .expect("/giveskill <other_cid> 3300 3");
    assert_eq!(
        rep_other.dict_get("characterID"),
        Some(&PyRep::Integer(other_cid))
    );
    assert_eq!(
        ctx.db
            .with_snapshot(|s| s.skills.get(&other_cid).and_then(|m| m.get(&3300)).map(|r| r.level)),
        Some(3)
    );
    assert!(ctx
        .drain_notifications_for_char(other_cid)
        .iter()
        .any(|n| n.event == "OnServerSkillsChanged"));
    assert!(!ctx
        .drain_notifications_for_char(cid)
        .iter()
        .any(|n| n.event == "OnServerSkillsChanged"));

    let _ = ctx
        .dispatch_rpc(
            &mut sess,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String("/giveskill me 3300 1".into())],
            &kw,
            None,
        )
        .await
        .expect("/giveskill me 3300 1");
    assert_eq!(
        ctx.db
            .with_snapshot(|s| s.skills.get(&cid).and_then(|m| m.get(&3300)).map(|r| r.level)),
        Some(1)
    );
    let _ = ctx.drain_notifications_for_char(cid);

    // 4. Skill queue reconciliation drops completed levels, updates head timestamps,
    //    prunes downstream entries whose prerequisites break on /setskill 0, and emits OnNewSkillQueueSaved.
    let now_u = ctx.now_unix();
    let _ = ctx
        .dispatch_rpc(
            &mut sess,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String("/giveskill 3327 5".into())],
            &kw,
            None,
        )
        .await
        .expect("/giveskill 3327 5");
    let _ = ctx.drain_notifications_for_char(cid);
    ctx.db.with_mut_snapshot(|snap| {
        snap.paused_skill_queues.remove(&cid);
        snap.skill_queues.insert(
            cid,
            vec![
                eve_db::SkillQueueEntry {
                    type_id: 3300,
                    target_level: 2,
                    start_sp: 250.0,
                    end_sp: 1_414.213_562,
                    duration_seconds: 120.0,
                    started_at_unix: Some(now_u),
                },
                eve_db::SkillQueueEntry {
                    type_id: 3300,
                    target_level: 3,
                    start_sp: 1_414.213_562,
                    end_sp: 8_000.0,
                    duration_seconds: 300.0,
                    started_at_unix: None,
                },
                eve_db::SkillQueueEntry {
                    type_id: 3331,
                    target_level: 1,
                    start_sp: 0.0,
                    end_sp: 500.0,
                    duration_seconds: 60.0,
                    started_at_unix: None,
                },
            ],
        );
    });
    let _ = ctx
        .dispatch_rpc(
            &mut sess,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String("/giveskill 3300 2".into())],
            &kw,
            None,
        )
        .await
        .expect("/giveskill 3300 2");
    ctx.db.with_snapshot(|snap| {
        let q = &snap.skill_queues[&cid];
        assert_eq!(q.len(), 2);
        assert_eq!((q[0].type_id, q[0].target_level), (3300, 3));
        assert!(q[0].started_at_unix.is_some());
        assert_eq!((q[1].type_id, q[1].target_level), (3331, 1));
    });
    assert!(ctx
        .drain_notifications_for_char(cid)
        .iter()
        .any(|n| n.event == "OnNewSkillQueueSaved"));

    // Removing Spaceship Command (3327 -> 0) drops dependent Amarr Frigate (3331, req 3327 I).
    let _ = ctx
        .dispatch_rpc(
            &mut sess,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String("/setskill 3327 0".into())],
            &kw,
            None,
        )
        .await
        .expect("/setskill 3327 0");
    ctx.db.with_snapshot(|snap| {
        let q = &snap.skill_queues[&cid];
        assert_eq!(q.len(), 1);
        assert_eq!((q[0].type_id, q[0].target_level), (3300, 3));
    });
    assert!(ctx
        .drain_notifications_for_char(cid)
        .iter()
        .any(|n| n.event == "OnNewSkillQueueSaved"));

    // 5. Bare / malformed argument handling returns InvalidSlashArgument.
    for bad in [
        "/giveskill",
        "/setskill",
        "/giveskill 3300",
        "/giveskill badtarget 3300 3",
        "/giveskill 3300 3 extra",
    ] {
        let err = ctx
            .dispatch_rpc(
                &mut sess,
                "slash",
                None,
                "SlashCmd",
                &[PyRep::String(bad.into())],
                &kw,
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(err.code, "InvalidSlashArgument", "for {bad}");
    }
}

#[tokio::test]
async fn slash_moveme_from_docked_upwell_structure_undocks_ship_notifies_and_releases_control() {
    let mut ctx = test_ctx();
    let uid = 1_u64;
    let cid = 90_000_001_i64;
    let peer_cid = 90_000_002_i64;
    let struct_id = 1_001_896_944_936_i64;
    let ship_id = ctx
        .db
        .with_snapshot(|s| s.characters.get(&cid).map(|c| c.active_ship_id))
        .unwrap_or(0);
    assert!(ship_id > 0);
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            struct_id,
            eve_db::UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35_832,
                owner_corp_id: 98_000_001,
                solar_system_id: 30_000_144,
                name: "1DQ1-A Keepstar".into(),
                position: [12_000_000.0, -3_000_000.0, 4_000_000.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 500_000.0,
                armor_hp: 500_000.0,
                hull_hp: 500_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.solar_system_id = 30_000_144;
            ch.station_id = None;
            ch.structure_id = None;
        }
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.location_id = 30_000_144;
            ship.flag = InventoryFlag::None;
        }
        let mut peer = snap.characters[&cid].clone();
        peer.character_id = peer_cid;
        peer.user_id = 2;
        peer.solar_system_id = 30_000_144;
        peer.station_id = None;
        peer.structure_id = Some(struct_id);
        snap.characters.insert(peer_cid, peer);
    });
    let _ = ctx
        .db
        .dock_character(cid, struct_id, true)
        .expect("dock cid in Upwell structure");

    let mut sess = auth_session(uid, cid, 98_000_001, 30_000_144);
    sess.station_id = None;
    sess.structure_id = Some(struct_id);
    sess.ship_id = Some(ship_id);
    sess.role = service_roles::ROLE_GMH;
    let _ = ctx
        .db
        .acquire_online_lock(uid, sess.session_id.max(1), Some(cid), true, ctx.now_unix());
    let _ = ctx
        .db
        .acquire_online_lock(2, sess.session_id.max(1) + 1, Some(peer_cid), true, ctx.now_unix());
    let _ = ctx.drain_notifications_for_char(cid);
    let _ = ctx.drain_notifications_for_char(peer_cid);

    let res = handle(
        &ctx,
        &mut sess,
        "slash",
        "SlashCmd",
        &[PyRep::String("/moveme 30000142".into())],
        &kw,
    )
    .await
    .unwrap()
    .expect("/moveme 30000142 from docked Upwell structure");
    assert_eq!(keyval_i64(&res, "solarSystemID"), Some(30_000_142));
    assert_eq!(sess.solar_system_id, Some(30_000_142));
    assert_eq!(sess.station_id, None);
    assert_eq!(sess.structure_id, None);
    assert_eq!(sess.ship_id, Some(ship_id));

    ctx.db.with_snapshot(|snap| {
        let ch = &snap.characters[&cid];
        assert_eq!(ch.solar_system_id, 30_000_142);
        assert_eq!(ch.station_id, None);
        assert_eq!(ch.structure_id, None);
        let ship = &snap.items[&ship_id];
        assert_eq!(ship.location_id, 30_000_142);
        assert_eq!(ship.flag, InventoryFlag::None);
    });

    let notifs = ctx.drain_notifications_for_char(cid);
    let item_change = notifs
        .iter()
        .find(|n| n.event == "OnItemChange")
        .expect("/moveme from docked structure must emit OnItemChange");
    assert_eq!(item_change.id_type, "charid");
    assert_eq!(item_change.args.len(), 2);
    let change_dict = item_change.args[1]
        .as_dict()
        .expect("OnItemChange change dict");
    assert!(
        change_dict
            .iter()
            .any(|(k, v)| k == &PyRep::Integer(3) && v == &PyRep::Integer(struct_id)),
        "OnItemChange must record previous structure locationID {struct_id}: {change_dict:?}"
    );
    assert!(
        change_dict.iter().any(|(k, v)| {
            k == &PyRep::Integer(4) && v == &PyRep::Integer(InventoryFlag::Hangar as i64)
        }),
        "OnItemChange must record previous Hangar flagID: {change_dict:?}"
    );

    let peer_notifs = ctx.drain_notifications_for_char(peer_cid);
    let left_notif = peer_notifs
        .iter()
        .find(|n| n.event == "OnCharacterLeftStructure")
        .expect("peer docked in structure must receive OnCharacterLeftStructure");
    assert_eq!(left_notif.id_type, "stationid");
    assert_eq!(left_notif.args, vec![PyRep::Integer(cid)]);

    // Part 2: If pilot was controlling the Upwell structure while docked, `/moveme` must release
    // structure control cleanly and move the restored ship (never the structure) to space.
    let _ = ctx.systems.get_or_spawn(30_000_142).remove_ball(ship_id).await;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.solar_system_id = 30_000_144;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
            ch.corporation_id = 98_000_001;
        }
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.location_id = 30_000_144;
            ship.flag = InventoryFlag::None;
        }
        if let Some(corp) = snap.corporations.get_mut(&98_000_001) {
            corp.ceo_id = cid;
            corp.members.insert(cid);
        }
    });
    let _ = ctx
        .db
        .dock_character(cid, struct_id, true)
        .expect("re-dock in Upwell structure");
    sess.solar_system_id = Some(30_000_144);
    sess.station_id = None;
    sess.structure_id = Some(struct_id);
    sess.ship_id = Some(ship_id);
    sess.corporation_id = Some(98_000_001);
    sess.corp_role = 1;

    if !ctx.sde.types.contains_key(&35_832) {
        let reg = std::sync::Arc::make_mut(&mut ctx.sde);
        let mut st_type = reg.types.values().next().cloned().expect("base SDE type");
        st_type.type_id = 35_832;
        st_type.group_id = 1657;
        st_type.category_id = 65;
        st_type.name = "Astrahus".into();
        st_type.radius = 5_000.0;
        st_type.mass = 1.0e11;
        reg.types.insert(35_832, st_type);
    }

    let _ = crate::services::structure_control_ops::handle_take_control(
        &ctx,
        &mut sess,
        &[PyRep::Integer(struct_id)],
    )
    .await
    .expect("take structure control while docked");
    assert_eq!(sess.ship_id, Some(struct_id));

    let _ = ctx.drain_notifications_for_char(cid);
    let _ = ctx.drain_notifications_for_char(peer_cid);
    let _ = handle(
        &ctx,
        &mut sess,
        "slash",
        "SlashCmd",
        &[PyRep::String("/moveme 30000142".into())],
        &kw,
    )
    .await
    .unwrap()
    .expect("/moveme 30000142 while controlling structure");

    assert_eq!(sess.solar_system_id, Some(30_000_142));
    assert_eq!(sess.structure_id, None);
    assert_eq!(sess.station_id, None);
    assert_eq!(sess.ship_id, Some(ship_id));
    assert_eq!(
        crate::services::structure_control_ops::handle_get_structure_pilot(
            &ctx,
            &auth_session(2, peer_cid, 98_000_001, 30_000_144),
            &[PyRep::Integer(struct_id)],
        )
        .expect("get structure pilot"),
        PyRep::None
    );
    let jita_handle = ctx.systems.get_or_spawn(30_000_142);
    assert!(
        jita_handle
            .get_ball_snapshot(ship_id)
            .await
            .unwrap()
            .is_some(),
        "restored pilot ship ball must spawn in destination system"
    );
    assert!(
        jita_handle
            .get_ball_snapshot(struct_id)
            .await
            .unwrap()
            .is_none(),
        "Upwell structure ball must never be teleported into destination system"
    );
}

#[tokio::test]
async fn authentication_login_displacement_defers_logoff_for_live_session_and_logs_off_headless() {
    let ctx = test_ctx();
    let kw = HashMap::new();
    let sys_id = 30_000_142_u32;
    let cid = 90_171_501_i64;
    ctx.db
        .provision_gm_account("disp_user_1171", "secret_pw", cid, "DispPilot1171");
    let (uid, ship_id) = ctx.db.with_snapshot(|s| {
        (
            s.accounts["disp_user_1171"].user_id,
            s.characters[&cid].active_ship_id,
        )
    });
    let sys_handle = ctx.systems.get_or_spawn(sys_id);

    let place_in_space = || {
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.solar_system_id = sys_id;
                ch.station_id = None;
                ch.structure_id = None;
            }
            if let Some(ship) = snap.items.get_mut(&ship_id) {
                ship.location_id = sys_id as i64;
            }
        });
    };

    // 1. Live session S1 holds the account + char in space with a registered session closer.
    place_in_space();
    let _ = sys_handle
        .spawn_ball(eve_destiny::Ball {
            id: ship_id,
            type_id: 587,
            position: glam::DVec3::new(10_000.0, 0.0, 0.0),
            velocity: glam::DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 0.5,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await;

    let s1_sid = 91_710_u64;
    let mut sess_s1 = auth_session(uid, cid, 98_000_001, sys_id);
    sess_s1.session_id = s1_sid;
    sess_s1.ship_id = Some(ship_id);
    sess_s1.station_id = None;
    sess_s1.structure_id = None;
    let (s1_close_tx, mut s1_close_rx) =
        tokio::sync::mpsc::unbounded_channel::<(String, String)>();
    ctx.register_session_closer(s1_sid, s1_close_tx);
    ctx.db
        .acquire_online_lock(uid, s1_sid, Some(cid), false, ctx.now_unix())
        .expect("S1 lock");

    // S2 calls authentication.Login on the same account while S1 is live.
    let mut sess_s2 = eve_machonet::SessionState::new(91_711);
    let login_res = handle(
        &ctx,
        &mut sess_s2,
        "authentication",
        "Login",
        &[
            PyRep::String("disp_user_1171".into()),
            PyRep::String("secret_pw".into()),
        ],
        &kw,
    )
    .await
    .expect("Login handled")
    .expect("Login succeeds");
    assert_eq!(login_res.dict_get("userID"), Some(&PyRep::Integer(uid as i64)));

    // S1 must receive ACL_DISPLACED signal, while logoff_character_sync is deferred until S1 exits.
    let (kick_code, _) = s1_close_rx
        .try_recv()
        .expect("S1 must receive ACL_DISPLACED close notification");
    assert_eq!(kick_code, "ACL_DISPLACED");
    assert!(
        !crate::sp6_lifecycle::has_active_disconnected_ship(&ctx.db, cid),
        "displacing a live session must not insert disconnected_ships before S1 exits"
    );

    // Once S1 exits its task loop and runs cleanup (S2 has not selected cid yet), deferred logoff runs.
    ctx.cleanup_disconnected_client_session(&mut sess_s1).await;
    assert!(
        crate::sp6_lifecycle::has_active_disconnected_ship(&ctx.db, cid),
        "S1 disconnect cleanup must run deferred logoff when S2 does not own cid"
    );

    // 2. Headless session S3 (no registered closer) MUST trigger immediate logoff on displacement.
    place_in_space();
    let _ = sys_handle
        .spawn_ball(eve_destiny::Ball {
            id: ship_id,
            type_id: 587,
            position: glam::DVec3::new(10_000.0, 0.0, 0.0),
            velocity: glam::DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 0.5,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.sp6_lifecycle.disconnected_ships.remove(&cid);
    });
    let s3_sid = 91_712_u64;
    ctx.db
        .acquire_online_lock(uid, s3_sid, Some(cid), true, ctx.now_unix())
        .expect("S3 lock");

    let mut sess_s4 = eve_machonet::SessionState::new(91_713);
    handle(
        &ctx,
        &mut sess_s4,
        "authentication",
        "Login",
        &[
            PyRep::String("disp_user_1171".into()),
            PyRep::String("secret_pw".into()),
        ],
        &kw,
    )
    .await
    .expect("S4 Login handled")
    .expect("S4 Login succeeds");
    assert!(
        crate::sp6_lifecycle::has_active_disconnected_ship(&ctx.db, cid),
        "displacing a headless session without a closer must still log off synchronously"
    );
}
