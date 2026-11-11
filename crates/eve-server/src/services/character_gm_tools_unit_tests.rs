//! Unit and mutation-killing tests for `services::character_gm_tools`.

use super::*;
use crate::router::ServerContext;
use eve_db::WorldRepository;
use eve_machonet::SessionState;
use eve_machonet::service_roles::ROLE_GML;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn rowset_len(rep: &PyRep) -> usize {
    match rep {
        PyRep::Rowset { rows, .. } | PyRep::TupleSet { rows, .. } => rows.len(),
        PyRep::IndexRowset { items, .. } => items.len(),
        PyRep::List(items) | PyRep::Tuple(items) => items.len(),
        _ => rep.crowset_rows().map(|r| r.len()).unwrap_or(0),
    }
}

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

fn gm_session(char_id: i64, ship_id: i64) -> SessionState {
    SessionState {
        session_id: 77,
        user_id: Some(1),
        character_id: Some(char_id),
        corporation_id: Some(1_000_009),
        solar_system_id: Some(30_000_142),
        station_id: Some(60_003_760),
        ship_id: Some(ship_id),
        role: ROLE_GML | eve_machonet::service_roles::ROLE_QA,
        ..Default::default()
    }
}

#[test]
fn character_name_syntax_and_bloodline_name_pools_cover_all_branches() {
    assert!(is_valid_character_name_syntax("Aria Von-Kallio"));
    assert!(is_valid_character_name_syntax("D'Artagnan Rex"));
    assert!(!is_valid_character_name_syntax("Ab")); // < 3 chars
    assert!(!is_valid_character_name_syntax(&"A".repeat(38))); // > 37 chars
    assert!(!is_valid_character_name_syntax(" LeadingSpace"));
    assert!(!is_valid_character_name_syntax("-StartsDash"));
    assert!(!is_valid_character_name_syntax("EndsQuote'"));
    assert!(!is_valid_character_name_syntax("Double  Space"));
    assert!(!is_valid_character_name_syntax("One Two Three Four")); // > 2 spaces
    assert!(!is_valid_character_name_syntax("CCP Falcon")); // reserved prefix
    assert!(!is_valid_character_name_syntax("GM Tester"));
    assert!(!is_valid_character_name_syntax("Concord Officer")); // banned word
    assert!(!is_valid_character_name_syntax("Bad$Char"));

    for bid in [1, 2, 3, 4, 5, 6, 7, 8, 11, 12, 13, 14, 999] {
        let (firsts, lasts) = bloodline_name_pools(bid);
        assert!(!firsts.is_empty());
        assert!(!lasts.is_empty());
    }
}

#[test]
fn char_unbound_select_validate_random_name_and_create_character_work() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let ship_id = ctx.db.with_snapshot(|s| s.characters[&char_id].active_ship_id);
    let mut sess = gm_session(char_id, ship_id);
    let kw = HashMap::new();

    let select_list = handle_char_unbound(
        &ctx,
        &mut sess,
        "charUnboundMgr",
        "GetCharactersToSelect",
        &[],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert!(rowset_len(&select_list) > 0);

    assert_eq!(
        handle_char_unbound(
            &ctx,
            &mut sess,
            "charUnboundMgr",
            "ValidateNameEx",
            &[PyRep::String("Valid Capsuleer".into())],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        handle_char_unbound(
            &ctx,
            &mut sess,
            "charUnboundMgr",
            "ValidateNameEx",
            &[PyRep::String("CCP Invalid".into())],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(false)
    );

    // GetValidRandomName
    let rand_name = handle_char_unbound(
        &ctx,
        &mut sess,
        "charUnboundMgr",
        "GetValidRandomName",
        &[PyRep::Integer(1)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert!(rand_name.as_str().is_some_and(is_valid_character_name_syntax));
    assert_eq!(
        handle_char_unbound(
            &ctx,
            &mut sess,
            "charUnboundMgr",
            "GetValidRandomName",
            &[PyRep::Integer(-5)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidBloodline"
    );

    // CreateCharacterWithDoll validation and success
    assert_eq!(
        handle_char_unbound(
            &ctx,
            &mut sess,
            "charUnboundMgr",
            "CreateCharacterWithDoll",
            &[PyRep::String("Bad".into())],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidArgument"
    );
    let anc = ctx.sde.ancestries.values().next().unwrap();
    let bl = &ctx.sde.bloodlines[&anc.bloodline_id];
    let sch = ctx
        .sde
        .schools
        .values()
        .find(|s| s.race_id == bl.race_id)
        .unwrap();
    let doll_kv = PyRep::keyval([("hair", PyRep::Integer(1))]);
    let port_kv = PyRep::keyval([("pose", PyRep::Integer(2))]);
    let new_cid = handle_char_unbound(
        &ctx,
        &mut sess,
        "charUnboundMgr",
        "CreateCharacterWithDoll",
        &[
            PyRep::String("Vespera Kallio".into()),
            PyRep::Integer(bl.bloodline_id as i64),
            PyRep::Integer(0), // genderID
            PyRep::Integer(anc.ancestry_id as i64),
            doll_kv,
            port_kv,
            PyRep::Integer(sch.school_id as i64),
        ],
        &kw,
    )
    .unwrap()
    .unwrap()
    .as_i64()
    .unwrap();
    assert!(new_cid > 0);

    // SelectCharacterID
    let sel_res = handle_char_unbound(
        &ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        sel_res.dict_get("characterID"),
        Some(&PyRep::Integer(char_id))
    );
    assert_eq!(
        handle_char_unbound(
            &ctx,
            &mut sess,
            "charUnboundMgr",
            "SelectCharacterID",
            &[PyRep::Integer(999_999_999)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CharacterSelectionDenied"
    );
}

#[tokio::test]
async fn gm_slash_commands_and_qa_rpcs_execute_and_enforce_roles() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let ship_id = ctx.db.with_snapshot(|s| s.characters[&char_id].active_ship_id);
    let mut sess = gm_session(char_id, ship_id);

    // Non-GM role check on slash.SlashCmd & publicQaToolsServer
    let mut player_sess = sess.clone();
    player_sess.role = 0;
    assert_eq!(
        handle_gm_qa_rpcs(
            &ctx,
            &mut player_sess,
            "slash",
            "SlashCmd",
            &[PyRep::String("/heal".into())],
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InsufficientRole"
    );
    assert_eq!(
        handle_gm_qa_rpcs(
            &ctx,
            &mut player_sess,
            "publicQaToolsServer",
            "MoveMeTo",
            &[],
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InsufficientRole"
    );
    assert_eq!(
        handle_gm_qa_rpcs(
            &ctx,
            &mut player_sess,
            "publicQaToolsServer",
            "CanGiveItemForMultifit",
            &[],
        )
        .await
        .unwrap()
        .unwrap(),
        PyRep::Bool(false)
    );
    assert_eq!(
        handle_gm_qa_rpcs(
            &ctx,
            &mut sess,
            "publicQaToolsServer",
            "CanGiveItemForMultifit",
            &[],
        )
        .await
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );

    // /heal
    let healed = handle_gm_slash_ext(&ctx, &mut sess, char_id, "/heal")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(healed.dict_get("healed"), Some(&PyRep::Bool(true)));

    // /spawn & /unspawn
    let spawned = handle_gm_slash_ext(&ctx, &mut sess, char_id, "/spawn 606 1")
        .await
        .unwrap()
        .unwrap();
    let spawned_id = spawned.dict_get("spawnedID").unwrap().as_i64().unwrap();
    assert!(spawned_id > 0);

    let unspawned = handle_gm_slash_ext(
        &ctx,
        &mut sess,
        char_id,
        &format!("/unspawn {spawned_id}"),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        unspawned.dict_get("unspawnedID"),
        Some(&PyRep::Integer(spawned_id))
    );

    // /setattr & /getattr
    let set_res = handle_gm_slash_ext(&ctx, &mut sess, char_id, "/setattr me 37 450.5")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(set_res.dict_get("newValue"), Some(&PyRep::Real(450.5)));

    let get_res = handle_gm_slash_ext(&ctx, &mut sess, char_id, "/getattr me 37")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(get_res.dict_get("value"), Some(&PyRep::Real(450.5)));

    // /fit
    let fit_res = handle_gm_slash_ext(&ctx, &mut sess, char_id, "/fit me 2048")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(fit_res.dict_get("typeID"), Some(&PyRep::Integer(2048)));

    // /location, /whoami, /online, /contact, /tidi, /reinforce, /autotidi, /energy, /secstatus
    let loc_res = handle_gm_slash_ext(&ctx, &mut sess, char_id, "/location")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        loc_res.dict_get("characterID"),
        Some(&PyRep::Integer(char_id))
    );

    let who = handle_gm_slash_ext(&ctx, &mut sess, char_id, "/whoami")
        .await
        .unwrap()
        .unwrap();
    assert!(who.as_str().unwrap().contains("charID="));

    let onl = handle_gm_slash_ext(&ctx, &mut sess, char_id, "/online")
        .await
        .unwrap()
        .unwrap();
    assert!(onl.dict_get("onlineCount").is_some());

    let cnt = handle_gm_slash_ext(&ctx, &mut sess, char_id, "/contact 90000002 7.5")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(cnt.dict_get("standing"), Some(&PyRep::Real(7.5)));

    let tidi = handle_gm_slash_ext(&ctx, &mut sess, char_id, "/tidi 0.5")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(tidi, PyRep::Real(0.5));
    let _ = handle_gm_slash_ext(&ctx, &mut sess, char_id, "/tidi 1.0").await;

    let reinf = handle_gm_slash_ext(&ctx, &mut sess, char_id, "/reinforce")
        .await
        .unwrap()
        .unwrap();
    assert!(reinf.as_i64().unwrap() > 0);

    assert_eq!(
        handle_gm_slash_ext(&ctx, &mut sess, char_id, "/autotidi 50")
            .await
            .unwrap()
            .unwrap(),
        PyRep::Integer(50)
    );
    assert_eq!(
        handle_gm_slash_ext(&ctx, &mut sess, char_id, "/autotidi off")
            .await
            .unwrap()
            .unwrap(),
        PyRep::Integer(0)
    );

    let en = handle_gm_slash_ext(&ctx, &mut sess, char_id, "/energy 800")
        .await
        .unwrap()
        .unwrap();
    assert!(en.dict_get("available").is_some() || en.dict_get("current").is_some() || matches!(en, PyRep::KeyVal(_)));

    let sec = handle_gm_slash_ext(&ctx, &mut sess, char_id, "/secstatus 3.5")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(sec, PyRep::Real(3.5));

    // /kill on a spawned ship
    let sp2 = handle_gm_slash_ext(&ctx, &mut sess, char_id, "/spawn 606 1")
        .await
        .unwrap()
        .unwrap();
    let sp2_id = sp2.dict_get("spawnedID").unwrap().as_i64().unwrap();
    let kill_res = handle_gm_slash_ext(&ctx, &mut sess, char_id, &format!("/kill {sp2_id}"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(kill_res.dict_get("destroyed"), Some(&PyRep::Bool(true)));

    // Fitting a module onto a Capsule (0 slots) after ship destruction is cleanly rejected.
    assert_eq!(
        handle_gm_slash_ext(&ctx, &mut sess, char_id, r#"/fit "me" 2048"#)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "FittingValidationFailed"
    );
    // Board back into a clean Rifter (no pre-fitted Damage Control II, maxGroupFitted=1) for #610.
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&char_id) {
            ch.active_ship_id = ship_id;
        }
        s.items
            .retain(|_, it| !(it.location_id == ship_id && it.type_id == 2048));
    });
    ctx.invalidate_ship_fitting(ship_id);
    sess.ship_id = Some(ship_id);

    // Issue #610: /fit "me" <type_id> (quoted target from client svc_slash) fits module & emits OnItemChange
    let uid = ctx.db.with_snapshot(|s| s.characters.get(&char_id).map(|c| c.user_id)).unwrap_or(1);
    ctx.db
        .acquire_online_lock(uid, sess.session_id.max(1), Some(char_id), true, ctx.now_unix())
        .expect("register online session for /fit notif");
    let _ = ctx.drain_notifications_for_char(char_id);
    let current_ship_id = ctx.db.with_snapshot(|s| s.characters[&char_id].active_ship_id);
    let quoted_fit = handle_gm_slash_ext(&ctx, &mut sess, char_id, r#"/fit "me" 2048"#)
        .await
        .unwrap()
        .expect("quoted /fit \"me\" 2048 must succeed");
    assert_eq!(quoted_fit.dict_get("shipID"), Some(&PyRep::Integer(current_ship_id)));
    let fitted_id = quoted_fit.dict_get("itemID").and_then(PyRep::as_i64).unwrap();
    let fit_notifs = ctx.drain_notifications_for_char(char_id);
    let fit_item_change = fit_notifs
        .iter()
        .find(|n| n.event == "OnItemChange")
        .expect("OnItemChange must be emitted for /fit");
    if let Some(PyRep::DBRow { columns: cells, .. }) = fit_item_change.args.first() {
        assert_eq!(cells.first().and_then(|(_, v)| v.as_i64()), Some(fitted_id));
        assert_eq!(cells.get(1).and_then(|(_, v)| v.as_i64()), Some(2048));
        assert_eq!(cells.get(3).and_then(|(_, v)| v.as_i64()), Some(current_ship_id));
    } else {
        panic!("expected DBRow in /fit OnItemChange, got {:?}", fit_item_change.args);
    }
    assert_eq!(
        fit_item_change.args.get(1),
        Some(&PyRep::Dict(vec![
            (PyRep::Integer(3), PyRep::Integer(0)),
            (PyRep::Integer(4), PyRep::Integer(0)),
        ]))
    );

    // Issue #628: Quoted target arguments (e.g. from V24 svc_slash.py cmd_fit/cmd_loadcontainer)
    // Remove prior DCU II first because Damage Control modules enforce maxGroupFitted=1.
    ctx.db.with_mut_snapshot(|s| {
        s.items
            .retain(|_, it| !(it.location_id == current_ship_id && it.type_id == 2048));
    });
    ctx.invalidate_ship_fitting(current_ship_id);
    let cur_ship_id = current_ship_id;
    let _ = ctx.drain_notifications_for_char(char_id);
    let fit_quoted = handle_gm_slash_ext(&ctx, &mut sess, char_id, "/fit \"me\" 2048 ")
        .await
        .unwrap()
        .expect("/fit with quoted \"me\" and trailing space must succeed");
    assert_eq!(fit_quoted.dict_get("typeID"), Some(&PyRep::Integer(2048)));
    assert_eq!(fit_quoted.dict_get("shipID"), Some(&PyRep::Integer(cur_ship_id)));
    let fit_notifs_q = ctx.drain_notifications_for_char(char_id);
    assert!(
        fit_notifs_q.iter().any(|n| n.event == "OnItemChange"),
        "/fit should emit OnItemChange notification"
    );

    let set_quoted = handle_gm_slash_ext(&ctx, &mut sess, char_id, "/setattr \"me\" 9 512.5")
        .await
        .unwrap()
        .expect("/setattr with quoted \"me\" must succeed");
    assert_eq!(set_quoted.dict_get("itemID"), Some(&PyRep::Integer(cur_ship_id)));
    assert_eq!(set_quoted.dict_get("value"), Some(&PyRep::Real(512.5)));

    let get_quoted = handle_gm_slash_ext(&ctx, &mut sess, char_id, "/getattr \"me\" 9")
        .await
        .unwrap()
        .expect("/getattr with quoted \"me\" must succeed");
    assert_eq!(get_quoted.dict_get("value"), Some(&PyRep::Real(512.5)));

    // Issue #627: /load slash command (quoted "me", unquoted me, ship ID, default qty, and bare /load error)
    let _ = ctx.drain_notifications_for_char(char_id);
    let load_quoted = handle_gm_slash_ext(&ctx, &mut sess, char_id, "/load \"me\" 34 250 ")
        .await
        .unwrap()
        .expect("/load \"me\" 34 250 must succeed");
    let loaded_id_1 = load_quoted.dict_get("itemID").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(load_quoted.dict_get("typeID"), Some(&PyRep::Integer(34)));
    assert_eq!(load_quoted.dict_get("quantity"), Some(&PyRep::Integer(250)));
    assert_eq!(load_quoted.dict_get("locationID"), Some(&PyRep::Integer(cur_ship_id)));
    assert_eq!(
        load_quoted.dict_get("flagID"),
        Some(&PyRep::Integer(InventoryFlag::Cargo as i64))
    );
    let loaded_rec_1 = ctx.db.get_item(loaded_id_1).expect("loaded item persisted in db");
    assert_eq!(loaded_rec_1.quantity, 250);
    assert_eq!(loaded_rec_1.location_id, cur_ship_id);
    assert_eq!(loaded_rec_1.flag, InventoryFlag::Cargo);
    let load_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(
        load_notifs.iter().any(|n| n.event == "OnItemChange"),
        "/load should emit OnItemChange notification"
    );

    let load_default_qty = handle_gm_slash_ext(&ctx, &mut sess, char_id, "/load me 2048")
        .await
        .unwrap()
        .expect("/load me 2048 (default qty 1) must succeed");
    assert_eq!(load_default_qty.dict_get("quantity"), Some(&PyRep::Integer(1)));
    assert_eq!(load_default_qty.dict_get("typeID"), Some(&PyRep::Integer(2048)));

    let load_shorthand = handle_gm_slash_ext(&ctx, &mut sess, char_id, "/load 34 15")
        .await
        .unwrap()
        .expect("/load <typeID> <qty> shorthand must succeed");
    assert_eq!(load_shorthand.dict_get("quantity"), Some(&PyRep::Integer(15)));
    assert_eq!(load_shorthand.dict_get("locationID"), Some(&PyRep::Integer(cur_ship_id)));

    let load_explicit_ship = handle_gm_slash_ext(&ctx, &mut sess, char_id, &format!("/load {cur_ship_id} 34 42"))
        .await
        .unwrap()
        .expect("/load <ship_id> 34 42 must succeed");
    assert_eq!(load_explicit_ship.dict_get("quantity"), Some(&PyRep::Integer(42)));
    assert_eq!(load_explicit_ship.dict_get("locationID"), Some(&PyRep::Integer(cur_ship_id)));

    let load_bad = handle_gm_slash_ext(&ctx, &mut sess, char_id, "/load")
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(load_bad.code, "InvalidSlashArgument");
}

#[tokio::test]
async fn slash_fit_validates_online_state_and_emits_item_and_module_notifications() {
    let ctx = test_ctx();
    let char_id = 95_800_001_i64;
    let ship_id = 95_800_100_i64;
    let station_id = 60_003_760_i64;
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        ch.user_id = 1;
        ch.name = "SlashFitPilot".into();
        ch.station_id = Some(station_id);
        ch.solar_system_id = 30_000_142;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);
        snap.items.insert(
            ship_id,
            eve_db::InventoryItemRecord {
                item_id: ship_id,
                type_id: 587, // Rifter
                owner_id: char_id,
                location_id: station_id,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    let mut sess = gm_session(char_id, ship_id);
    let _ = ctx.db.acquire_online_lock(1, sess.session_id, Some(char_id), false, ctx.now_unix());
    let _ = ctx.drain_notifications_for_char(char_id);

    // Case A: Pilot lacks Gunnery / Small Projectile Turret -> fitted offline + notifications emitted
    let offline_fit = handle_gm_slash_ext(&ctx, &mut sess, char_id, "/fit 484")
        .await
        .expect("handled")
        .expect("/fit offline ok");
    let mod1_id = offline_fit.dict_get("itemID").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(offline_fit.dict_get("online"), Some(&PyRep::Bool(false)));
    assert_eq!(
        ctx.db.with_snapshot(|s| s.module_online_states.get(&mod1_id).copied()),
        Some(false)
    );
    let notifs1: Vec<String> = ctx
        .drain_notifications_for_char(char_id)
        .into_iter()
        .map(|n| n.event)
        .collect();
    assert!(notifs1.iter().any(|e| e == "OnItemChange"), "missing OnItemChange: {notifs1:?}");
    assert!(notifs1.iter().any(|e| e == "OnModuleFitted"), "missing OnModuleFitted: {notifs1:?}");
    assert!(
        notifs1.iter().any(|e| e == "OnModuleAttributeChanges"),
        "missing OnModuleAttributeChanges: {notifs1:?}"
    );
    assert!(
        !notifs1.iter().any(|e| e == "OnGodmaShipEffect"),
        "offline /fit must not emit OnGodmaShipEffect: {notifs1:?}"
    );

    // Case B: Grant skills -> fitted online + OnGodmaShipEffect emitted
    ctx.db.with_mut_snapshot(|snap| {
        let sk_map = snap.skills.entry(char_id).or_default();
        for sk in [3300_u32, 3301, 3302] {
            sk_map.insert(
                sk,
                eve_db::TrainedSkillRecord {
                    type_id: sk,
                    level: 5,
                    skill_points: 256_000.0,
                },
            );
        }
    });
    ctx.invalidate_ship_fitting(ship_id);
    let online_fit = handle_gm_slash_ext(&ctx, &mut sess, char_id, "/fit 484")
        .await
        .expect("handled")
        .expect("/fit online ok");
    let mod2_id = online_fit.dict_get("itemID").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(online_fit.dict_get("online"), Some(&PyRep::Bool(true)));
    assert_eq!(
        ctx.db.with_snapshot(|s| s.module_online_states.get(&mod2_id).copied()),
        Some(true)
    );
    let notifs2: Vec<String> = ctx
        .drain_notifications_for_char(char_id)
        .into_iter()
        .map(|n| n.event)
        .collect();
    assert!(notifs2.iter().any(|e| e == "OnItemChange"), "missing OnItemChange: {notifs2:?}");
    assert!(notifs2.iter().any(|e| e == "OnModuleFitted"), "missing OnModuleFitted: {notifs2:?}");
    assert!(
        notifs2.iter().any(|e| e == "OnModuleAttributeChanges"),
        "missing OnModuleAttributeChanges: {notifs2:?}"
    );
    assert!(
        notifs2.iter().any(|e| e == "OnGodmaShipEffect"),
        "online /fit must emit OnGodmaShipEffect: {notifs2:?}"
    );
}

#[tokio::test]
async fn gm_fit_enforces_hull_slots_and_hardpoints_and_spawn_asteroid_uses_system_owner() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let station_id = 60_003_760_i64;
    let sys_id = 30_000_142_u32;

    // 1. /fit validates hull slot capacity & turret hardpoints cleanly before inserting DB items.
    let target_ship_tid = if ctx.sde.types.contains_key(&32880) && ctx.sde.types.contains_key(&483) {
        32880_u32 // Venture: 3 hiSlots, 2 turret hardpoints
    } else {
        587_u32
    };
    let turret_tid = if target_ship_tid == 32880 { 483_u32 } else { 484_u32 };
    let max_turrets = ctx
        .sde
        .types
        .get(&target_ship_tid)
        .map(|t| {
            let hi = t.default_attributes.get(&eve_sde::attr_ids::HI_SLOTS).copied().unwrap_or(0.0) as usize;
            let tur = t.default_attributes.get(&eve_sde::attr_ids::TURRET_SLOTS_LEFT).copied().unwrap_or(0.0) as usize;
            hi.min(tur)
        })
        .unwrap_or(2);

    let test_ship_id = 1_000_991_101_i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.active_ship_id = test_ship_id;
            ch.station_id = Some(station_id);
        }
        snap.items.insert(
            test_ship_id,
            eve_db::InventoryItemRecord {
                item_id: test_ship_id,
                type_id: target_ship_tid,
                owner_id: char_id,
                location_id: station_id,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        let sk_map = snap.skills.entry(char_id).or_default();
        for sk in [3300_u32, 3301, 3302, 3386, 3392, 3426] {
            sk_map.insert(
                sk,
                eve_db::TrainedSkillRecord {
                    type_id: sk,
                    level: 5,
                    skill_points: 256_000.0,
                },
            );
        }
    });
    let mut sess = gm_session(char_id, test_ship_id);
    sess.solar_system_id = Some(sys_id);
    let _ = ctx.db.acquire_online_lock(1, sess.session_id, Some(char_id), false, ctx.now_unix());
    let _ = ctx.drain_notifications_for_char(char_id);

    for _ in 0..max_turrets {
        let ok = handle_gm_slash_ext(&ctx, &mut sess, char_id, &format!("/fit me {turret_tid}"))
            .await
            .expect("handled")
            .expect("turret within hardpoint limit must fit");
        assert_eq!(ok.dict_get("online"), Some(&PyRep::Bool(true)));
    }
    let fit_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(
        fit_notifs.iter().any(|n| {
            n.event == "OnModuleAttributeChanges"
                && matches!(n.args.first(), Some(PyRep::List(entries)) if entries.iter().any(|entry| {
                    matches!(entry, PyRep::Tuple(t) if t.get(2).and_then(|v| v.as_i64()) == Some(test_ship_id))
                }))
        }),
        "online /fit must emit ship CPU/PG load update notification"
    );

    let pre_count = ctx.db.with_snapshot(|s| {
        s.items.values().filter(|i| i.location_id == test_ship_id).count()
    });
    let over_err = handle_gm_slash_ext(&ctx, &mut sess, char_id, &format!("/fit me {turret_tid}"))
        .await
        .expect("handled")
        .expect_err("fitting beyond turret hardpoints / hiSlots must fail");
    assert_eq!(over_err.code, "FittingValidationFailed");
    let post_count = ctx.db.with_snapshot(|s| {
        s.items.values().filter(|i| i.location_id == test_ship_id).count()
    });
    assert_eq!(post_count, pre_count, "failed /fit must not insert dangling item");

    // 2. /spawn <asteroid> creates system-owned asteroid (ownerID=1, corpID=-1) and /unspawn removes snap.asteroids.
    let ast_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.category_id == 25)
        .map(|t| t.type_id)
        .unwrap_or(1228);
    sess.station_id = None;
    let handle = ctx.systems.get_or_spawn(sys_id);
    let spawned = handle_gm_slash_ext(&ctx, &mut sess, char_id, &format!("/spawn {ast_tid}"))
        .await
        .expect("handled")
        .expect("/spawn asteroid ok");
    let ast_id = spawned.dict_get("spawnedID").and_then(|v| v.as_i64()).unwrap();

    ctx.db.with_snapshot(|s| {
        assert_eq!(s.items[&ast_id].owner_id, 1, "asteroid DB owner_id must be ownerSystem (1)");
        assert!(s.asteroids.contains_key(&ast_id), "asteroid must be in snap.asteroids");
    });
    let slim_meta = handle
        .ballpark
        .read()
        .unwrap()
        .ball_slim_meta
        .get(&ast_id)
        .cloned()
        .expect("asteroid SlimItemMeta in ballpark");
    assert_eq!(slim_meta.owner_id, 1);
    assert_eq!(slim_meta.char_id, None);
    assert_eq!(slim_meta.corp_id, -1);
    assert_eq!(slim_meta.alliance_id, -1);
    {
        let bp = handle.ballpark.read().unwrap();
        assert_eq!(bp.balls[&ast_id].mode, eve_destiny::MovementMode::Rigid);
        assert!(bp.is_static_ball(ast_id));
    }

    let unspawned = handle_gm_slash_ext(&ctx, &mut sess, char_id, &format!("/unspawn {ast_id}"))
        .await
        .expect("handled")
        .expect("/unspawn asteroid ok");
    assert_eq!(unspawned.dict_get("removedFromDb"), Some(&PyRep::Bool(true)));
    ctx.db.with_snapshot(|s| {
        assert!(!s.items.contains_key(&ast_id));
        assert!(!s.asteroids.contains_key(&ast_id), "/unspawn must remove snap.asteroids entry");
    });

    // 3. /spawn <npc_type> registers live NPC AI in both npc_spawns and combat_ephemeral.npc_entities (#792).
    let npc_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.category_id == 11 && t.published)
        .or_else(|| ctx.sde.types.values().find(|t| t.category_id == 11))
        .map(|t| t.type_id);
    if let Some(ntid) = npc_tid {
        let sp_npc = handle_gm_slash_ext(&ctx, &mut sess, char_id, &format!("/spawn {ntid}"))
            .await
            .expect("handled")
            .expect("/spawn NPC ok");
        let npc_id = sp_npc.dict_get("spawnedID").and_then(|v| v.as_i64()).unwrap();
        let npc_meta = handle
            .ballpark
            .read()
            .unwrap()
            .ball_slim_meta
            .get(&npc_id)
            .cloned()
            .expect("NPC SlimItemMeta in ballpark");
        assert_eq!(npc_meta.category_id, 11);
        assert!(
            ctx.npc_spawns
                .read()
                .unwrap()
                .get(&sys_id)
                .is_some_and(|m| m.entities.contains_key(&npc_id)),
            "/spawn NPC must register in NpcSpawnManager"
        );
        assert!(
            crate::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| {
                rt.npc_entities.contains_key(&npc_id)
            }),
            "/spawn NPC must register in combat_ephemeral.npc_entities"
        );
        let _ = handle_gm_slash_ext(&ctx, &mut sess, char_id, &format!("/unspawn {npc_id}"))
            .await
            .expect("handled")
            .expect("/unspawn NPC ok");
        assert!(
            !crate::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| {
                rt.npc_entities.contains_key(&npc_id)
            }),
            "/unspawn NPC must remove from combat_ephemeral.npc_entities"
        );
    }
}
