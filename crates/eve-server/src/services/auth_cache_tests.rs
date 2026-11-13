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
    //    and spawn stopped in destination system with SetState queued.
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
    assert!(matches!(
        dest_evs.first(),
        Some(eve_destiny::DestinyEvent::SetState { observer_ball_id }) if *observer_ball_id == ship_id
    ));
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

    handle(
        &ctx,
        &mut sess,
        "slash",
        "SlashCmd",
        &[PyRep::String("/advancetime 3600".to_string())],
        &kw,
    )
    .await
    .unwrap()
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
}
