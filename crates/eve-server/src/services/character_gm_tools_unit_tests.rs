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
    assert!(
        handle_gm_qa_rpcs(
            &ctx,
            &mut player_sess,
            "publicQaToolsServer",
            "MoveMeTo",
            &[],
        )
        .await
        .is_none(),
        "publicQaToolsServer.MoveMeTo must not be intercepted by handle_gm_qa_rpcs"
    );
    assert!(
        handle_gm_qa_rpcs(
            &ctx,
            &mut sess,
            "publicQaToolsServer",
            "CanGiveItemForMultifit",
            &[],
        )
        .await
        .is_none(),
        "publicQaToolsServer.CanGiveItemForMultifit must not be intercepted by handle_gm_qa_rpcs"
    );
    assert_eq!(
        crate::services::jump_clone_names::handle_public_qa_tools(
            &ctx,
            &mut player_sess,
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
        crate::services::jump_clone_names::handle_public_qa_tools(
            &ctx,
            &mut player_sess,
            "CanGiveItemForMultifit",
            &[],
        )
        .await
        .unwrap()
        .unwrap(),
        PyRep::Bool(false)
    );
    assert_eq!(
        crate::services::jump_clone_names::handle_public_qa_tools(
            &ctx,
            &mut sess,
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
        Some(&PyRep::Dict(vec![(PyRep::Integer(3), PyRep::Integer(0))]))
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
    assert!(
        matches!(
            &load_quoted,
            PyRep::List(items)
                if items.first() == Some(&PyRep::Integer(loaded_id_1))
                    && items
                        .get(1)
                        .and_then(|kv| kv.dict_get("itemID"))
                        == Some(&PyRep::Integer(loaded_id_1))
        ),
        "/load must return [itemID, KeyVal] so PlugEmIn/dna.Load [0] indexing and '%s' formatting both succeed: {load_quoted:?}"
    );
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

    // V24 menu_implant.PlugEmIn runs `/load me 20499` and reads res[0]
    let load_implant = handle_gm_slash_ext(&ctx, &mut sess, char_id, "/load me 20499")
        .await
        .unwrap()
        .expect("/load me 20499 must succeed");
    let implant_id = load_implant.dict_get("itemID").and_then(|v| v.as_i64()).unwrap();
    assert!(
        matches!(
            &load_implant,
            PyRep::List(items)
                if items.first() == Some(&PyRep::Integer(implant_id))
                    && items
                        .get(1)
                        .and_then(|kv| kv.dict_get("typeID"))
                        == Some(&PyRep::Integer(20499))
        ),
        "/load me 20499 must return integer itemID at [0] inside a list: {load_implant:?}"
    );
    assert_eq!(load_implant.dict_get("typeID"), Some(&PyRep::Integer(20499)));
    let loaded_implant_rec = ctx.db.get_item(implant_id).expect("loaded implant persisted in db");
    assert_eq!(loaded_implant_rec.type_id, 20499);
    assert_eq!(loaded_implant_rec.quantity, 1);
    assert_eq!(loaded_implant_rec.flag, InventoryFlag::Cargo);

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
        .test_with_ballpark_async(move |bp| bp.ball_slim_meta.get(&ast_id).cloned())
        .await
        .expect("asteroid SlimItemMeta in ballpark");
    assert_eq!(slim_meta.owner_id, 1);
    assert_eq!(slim_meta.char_id, None);
    assert_eq!(slim_meta.corp_id, -1);
    assert_eq!(slim_meta.alliance_id, -1);
    {
        let (ast_mode, ast_static) = handle
            .test_with_ballpark_async(move |bp| (bp.balls[&ast_id].mode.clone(), bp.is_static_ball(ast_id)))
            .await;
        assert_eq!(ast_mode, eve_destiny::MovementMode::Rigid);
        assert!(ast_static);
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

    // #983: /spawn <ship_type> creates an unpiloted hull in space (SlimItemMeta.char_id == None)
    // so V24.01 menusvc offers 'Board Ship' on the spawned hull.
    let sp_hull = handle_gm_slash_ext(&ctx, &mut sess, char_id, "/spawn 11567")
        .await
        .expect("handled")
        .expect("/spawn ship hull ok");
    let hull_id = sp_hull.dict_get("spawnedID").and_then(|v| v.as_i64()).unwrap();
    let hull_meta = handle
        .test_with_ballpark_async(move |bp| bp.ball_slim_meta.get(&hull_id).cloned())
        .await
        .expect("spawned ship SlimItemMeta in ballpark");
    assert_eq!(hull_meta.owner_id, char_id);
    assert_eq!(
        hull_meta.char_id, None,
        "/spawn hull must be unpiloted (char_id == None) so menusvc shows Board Ship (#983)"
    );
    {
        let (interactive_override, raw_hull) = handle
            .test_with_ballpark_async(move |bp| {
                let ov = bp.ball_interactive_overrides.get(&hull_id).copied();
                let hull_ball = bp.balls.get(&hull_id).expect("spawned hull ball in ballpark");
                let mut raw_hull = Vec::new();
                eve_destiny::wire::encode_single_ball_with_park(
                    &mut raw_hull,
                    hull_ball,
                    bp.current_tick as u32,
                    Some(&hull_meta),
                    Some(bp),
                );
                (ov, raw_hull)
            })
            .await;
        assert_eq!(
            interactive_override,
            Some(false),
            "/spawn unpiloted ship must record ball_interactive_overrides[hull_id] = false (#983)"
        );
        assert_eq!(
            raw_hull[37] & eve_destiny::wire::BALL_FLAG_IS_INTERACTIVE,
            0,
            "encoded unpiloted hull must clear BALL_FLAG_IS_INTERACTIVE (#983)"
        );
    }
    let _ = handle_gm_slash_ext(&ctx, &mut sess, char_id, &format!("/unspawn {hull_id}"))
        .await
        .expect("handled")
        .expect("/unspawn hull ok");

    // 3. /spawn <npc_type> registers live NPC AI in both npc_spawns and combat NPC entity tracking (#792).
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
            .test_with_ballpark_async(move |bp| bp.ball_slim_meta.get(&npc_id).cloned())
            .await
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
            crate::combat::npc_ai::npc_test_helpers::has_npc_entity(&ctx, npc_id),
            "/spawn NPC must register in combat NPC entity tracking"
        );
        let _ = handle_gm_slash_ext(&ctx, &mut sess, char_id, &format!("/unspawn {npc_id}"))
            .await
            .expect("handled")
            .expect("/unspawn NPC ok");
        assert!(
            !crate::combat::npc_ai::npc_test_helpers::has_npc_entity(&ctx, npc_id),
            "/unspawn NPC must remove from combat NPC entity tracking"
        );
    }
}

#[tokio::test]
async fn gm_heal_syncs_godma_damage_cap_heat_and_flushes_self_destiny_update() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let ship_id = ctx.db.with_snapshot(|s| s.characters[&char_id].active_ship_id);
    let ship_tid = ctx.db.with_snapshot(|s| s.items[&ship_id].type_id);
    let sys_id = 30_000_142_u32;

    let mod_id = 980_949_001_i64;
    let mod_tid = 2048_u32; // Damage Control II (or fallback valid module)
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            mod_id,
            eve_db::InventoryItemRecord {
                item_id: mod_id,
                type_id: mod_tid,
                owner_id: char_id,
                location_id: ship_id,
                flag: eve_db::InventoryFlag::LoSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    let mut sess = gm_session(char_id, ship_id);
    sess.solar_system_id = Some(sys_id);
    sess.station_id = None;
    let uid = ctx
        .db
        .with_snapshot(|s| s.characters.get(&char_id).map(|c| c.user_id))
        .unwrap_or(1);
    ctx.db
        .acquire_online_lock(uid, sess.session_id.max(1), Some(char_id), true, ctx.now_unix())
        .expect("register online session for /heal notif");

    let handle = ctx.systems.get_or_spawn(sys_id);
    handle
        .spawn_ball(eve_destiny::Ball {
            id: ship_id,
            type_id: ship_tid,
            position: glam::DVec3::ZERO,
            velocity: glam::DVec3::ZERO,
            max_velocity: 350.0,
            inertia: 2.5,
            mass: 1_200_000.0,
            radius: 45.0,
            speed_fraction: 1.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await
        .unwrap();
    let damaged_cs = eve_destiny::CombatState {
        shield_hp: 120.0,
        max_shield_hp: 500.0,
        armor_hp: 80.0,
        max_armor_hp: 400.0,
        structure_hp: 90.0,
        max_structure_hp: 300.0,
        capacitor: 45.0,
        max_capacitor: 250.0,
        ..Default::default()
    };
    handle.set_combat_state(ship_id, damaged_cs.clone()).await.unwrap();
    crate::sp6_lifecycle::persist_ship_combat_hp_in_db(&ctx.db, ship_id, &damaged_cs);
    let _ = handle.drain_observer_updates(ship_id).await.unwrap();
    let mut rx = handle.subscribe();

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.dynamic_attributes
                .insert(eve_sde::attr_ids::SHIELD_CHARGE, 120.0);
            ship.dynamic_attributes
                .insert(eve_sde::attr_ids::ARMOR_DAMAGE, 320.0);
            ship.dynamic_attributes
                .insert(eve_sde::attr_ids::DAMAGE, 210.0);
            ship.dynamic_attributes
                .insert(18, 45.0);
            ship.dynamic_attributes.insert(1211, 5.0);
        }
        if let Some(m) = snap.items.get_mut(&mod_id) {
            m.dynamic_attributes
                .insert(eve_sde::attr_ids::DAMAGE, 12.5);
            m.dynamic_attributes.insert(1211, 7.5);
        }
    });
    if let Ok(mut fit) = ctx.get_or_rebuild_ship_fitting(char_id, ship_id) {
        if let Some(m) = fit.modules.get_mut(&mod_id) {
            m.damage_hp = 12.5;
        }
        fit.rack_heat = [0.4, 0.2, 0.1];
        ctx.store_ship_fitting(ship_id, fit);
    }

    // Negative permission check: plain player role must be denied without modifying state.
    let mut player_sess = sess.clone();
    player_sess.role = eve_db::service_roles::ROLE_PLAYER_DEFAULT;
    let _ = ctx.drain_notifications_for_char(char_id);
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
        crate::services::auth_cache::handle(
            &ctx,
            &mut player_sess,
            "slash",
            "SlashCmd",
            &[PyRep::String("/heal".into())],
            &HashMap::new(),
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InsufficientRole"
    );
    assert!(ctx.drain_notifications_for_char(char_id).is_empty());

    let expected_cap = ctx
        .fit(char_id, ship_id)
        .expect("fit for healed ship")
        .ship
        .get_attribute(eve_sde::attr_ids::CAPACITOR_CAPACITY, &ctx.sde)
        .max(250.0);
    assert_eq!(expected_cap, 287.5);

    // Live `/heal` in space must restore HP/cap, flush DoDestinyUpdate to self (`None` exclusion),
    // and push `OnModuleAttributeChanges` for ship gauges + repaired modules.
    let heal_res = handle_gm_slash_ext(&ctx, &mut sess, char_id, "/heal")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(heal_res.dict_get("healed"), Some(&PyRep::Bool(true)));
    assert_eq!(heal_res.dict_get("shieldHP"), Some(&PyRep::Real(500.0)));
    assert_eq!(heal_res.dict_get("armorHP"), Some(&PyRep::Real(400.0)));
    assert_eq!(heal_res.dict_get("hullHP"), Some(&PyRep::Real(300.0)));
    assert_eq!(heal_res.dict_get("capacitor"), Some(&PyRep::Real(expected_cap)));

    let broadcast = rx
        .try_recv()
        .expect("GH #949: /heal must flush DoDestinyUpdate immediately without excluding session.ship_id");
    let self_destiny_evs = broadcast
        .observer_events
        .get(&ship_id)
        .expect("healed pilot ship observer must receive flushed Destiny events");
    assert!(
        self_destiny_evs.iter().any(|ev| matches!(
            ev,
            eve_destiny::DestinyEvent::OnDamageStateChange {
                ball_id,
                shield_ratio,
                armor_ratio,
                structure_ratio,
            } if *ball_id == ship_id
                && (*shield_ratio - 1.0).abs() < 1e-6
                && (*armor_ratio - 1.0).abs() < 1e-6
                && (*structure_ratio - 1.0).abs() < 1e-6
        )),
        "expected full [1.0, 1.0, 1.0] OnDamageStateChange in flushed self Destiny update: {self_destiny_evs:?}"
    );

    let notifs = ctx.drain_notifications_for_char(char_id);
    let mut seen_changes: HashMap<(i64, i64), (f64, f64)> = HashMap::new();
    for n in notifs.iter().filter(|n| n.event == "OnModuleAttributeChanges") {
        let Some(PyRep::List(entries)) = n.args.first() else {
            panic!("expected List in OnModuleAttributeChanges: {n:?}");
        };
        for entry in entries {
            let PyRep::Tuple(t) = entry else {
                panic!("expected Tuple in OnModuleAttributeChanges: {entry:?}");
            };
            assert_eq!(t.first().and_then(|v| v.as_str()), Some("OnModuleAttributeChange"));
            assert_eq!(t.get(1).and_then(|v| v.as_i64()), Some(char_id));
            assert!(
                t.get(4).and_then(|v| v.as_i64()).unwrap_or(0) > 0,
                "timestamp must be non-zero filetime integer: {t:?}"
            );
            assert_eq!(t.get(7), Some(&PyRep::None), "scatterAttr must be None: {t:?}");
            let iid = t[2].as_i64().unwrap();
            let aid = t[3].as_i64().unwrap();
            let new_v = t[5].as_f64().unwrap();
            let old_v = t[6].as_f64().unwrap();
            seen_changes.insert((iid, aid), (new_v, old_v));
        }
    }
    assert_eq!(
        seen_changes.get(&(ship_id, i64::from(eve_sde::attr_ids::SHIELD_CHARGE))),
        Some(&(500.0, 120.0)),
        "missing shieldCharge (264) snap in OnModuleAttributeChanges: {seen_changes:?}"
    );
    assert_eq!(
        seen_changes.get(&(ship_id, i64::from(eve_sde::attr_ids::ARMOR_DAMAGE))),
        Some(&(0.0, 320.0)),
        "missing armorDamage (266) clear in OnModuleAttributeChanges: {seen_changes:?}"
    );
    assert_eq!(
        seen_changes.get(&(ship_id, i64::from(eve_sde::attr_ids::DAMAGE))),
        Some(&(0.0, 210.0)),
        "missing hull damage (3) clear in OnModuleAttributeChanges: {seen_changes:?}"
    );
    assert_eq!(
        seen_changes.get(&(ship_id, 18)),
        Some(&(expected_cap, 45.0)),
        "missing capacitor charge (18) restore in OnModuleAttributeChanges: {seen_changes:?}"
    );
    assert_eq!(seen_changes.get(&(ship_id, 1211)), Some(&(0.0, 5.0)));
    for rack_aid in [1175_i64, 1176, 1177] {
        assert_eq!(
            seen_changes.get(&(ship_id, rack_aid)).map(|(n, _)| *n),
            Some(0.0),
            "rack heat {rack_aid} must reset to 0.0"
        );
    }
    assert_eq!(
        seen_changes.get(&(mod_id, i64::from(eve_sde::attr_ids::DAMAGE))),
        Some(&(0.0, 12.5))
    );
    assert_eq!(seen_changes.get(&(mod_id, 1211)), Some(&(0.0, 7.5)));

    ctx.db.with_snapshot(|snap| {
        let ship = &snap.items[&ship_id];
        for aid in [
            eve_sde::attr_ids::DAMAGE,
            eve_sde::attr_ids::ARMOR_DAMAGE,
            eve_sde::attr_ids::SHIELD_CHARGE,
            18,
            1211,
        ] {
            assert!(!ship.dynamic_attributes.contains_key(&aid), "ship dynamic attr {aid} must be cleared");
        }
        let m = &snap.items[&mod_id];
        assert!(!m.dynamic_attributes.contains_key(&eve_sde::attr_ids::DAMAGE));
        assert!(!m.dynamic_attributes.contains_key(&1211));
        assert!(!snap.sp6_lifecycle.ship_combat_hp.contains_key(&ship_id));
    });
    let fit_after = ctx.get_or_rebuild_ship_fitting(char_id, ship_id).unwrap();
    assert_eq!(fit_after.rack_heat, [0.0, 0.0, 0.0]);
    assert_eq!(fit_after.modules.get(&mod_id).map(|m| m.damage_hp), Some(0.0));

    // Docked / auth_cache fallback parity: `auth_cache::handle` shares `execute_gm_heal` and
    // clears docked DB dynamic attributes while notifying godma with effective fitted maxes.
    let docked_ship_id = 980_949_002_i64;
    let docked_char_id = 90_000_002_i64;
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters[&char_id].clone();
        ch.character_id = docked_char_id;
        ch.active_ship_id = docked_ship_id;
        ch.station_id = Some(60_003_760);
        ch.solar_system_id = sys_id;
        snap.characters.insert(docked_char_id, ch);
        let mut sh = eve_db::InventoryItemRecord {
            item_id: docked_ship_id,
            type_id: ship_tid,
            owner_id: docked_char_id,
            location_id: 60_003_760,
            flag: eve_db::InventoryFlag::Hangar,
            quantity: 1,
            singleton: true,
            version: 1,
            ..Default::default()
        };
        sh.dynamic_attributes
            .insert(eve_sde::attr_ids::SHIELD_CHARGE, 10.0);
        sh.dynamic_attributes
            .insert(eve_sde::attr_ids::ARMOR_DAMAGE, 30.0);
        sh.dynamic_attributes.insert(eve_sde::attr_ids::DAMAGE, 40.0);
        sh.dynamic_attributes.insert(18, 20.0);
        snap.items.insert(docked_ship_id, sh);
    });
    let mut docked_sess = gm_session(docked_char_id, docked_ship_id);
    docked_sess.session_id = 78;
    docked_sess.solar_system_id = Some(sys_id);
    let docked_uid = ctx
        .db
        .with_snapshot(|s| s.characters.get(&docked_char_id).map(|c| c.user_id))
        .unwrap_or(1);
    ctx.db
        .acquire_online_lock(
            docked_uid,
            docked_sess.session_id.max(1),
            Some(docked_char_id),
            true,
            ctx.now_unix(),
        )
        .expect("register online session for docked /heal notif");
    let _ = ctx.drain_notifications_for_char(docked_char_id);
    let auth_heal = crate::services::auth_cache::handle(
        &ctx,
        &mut docked_sess,
        "slash",
        "SlashCmd",
        &[PyRep::String("/heal".into())],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(auth_heal.dict_get("healed"), Some(&PyRep::Bool(true)));
    assert!(
        auth_heal.dict_get("shieldHP").and_then(|v| v.as_f64()).unwrap_or(0.0) > 10.0,
        "docked /heal must report fitted shield max rather than 0.0: {auth_heal:?}"
    );
    ctx.db.with_snapshot(|snap| {
        assert!(snap.items[&docked_ship_id].dynamic_attributes.is_empty());
    });
    let docked_notifs = ctx.drain_notifications_for_char(docked_char_id);
    assert!(
        docked_notifs.iter().any(|n| n.event == "OnModuleAttributeChanges"),
        "auth_cache /heal must emit OnModuleAttributeChanges: {docked_notifs:?}"
    );
}

#[tokio::test]
async fn fit_rejects_non_fittable_types_without_mutating_inventory() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let ship_id = 1_855_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            eve_db::InventoryItemRecord {
                item_id: ship_id,
                type_id: 587, // Rifter
                owner_id: char_id,
                location_id: 60_003_760,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.active_ship_id = ship_id;
        }
    });
    let mut sess = gm_session(char_id, ship_id);
    let (pre_item_count, pre_next_id) =
        ctx.db.with_snapshot(|snap| (snap.items.len(), snap.next_entity_id));

    for bad_cmd in [
        "/fit me 3657",
        "/fit me 3657 27",
        "/fit me 34",
        "/fit me 2048 27",
    ] {
        let err = handle_gm_slash_ext(&ctx, &mut sess, char_id, bad_cmd)
            .await
            .expect("slash handled")
            .expect_err("non-fittable or wrong-slot /fit must fail");
        assert_eq!(err.code, "FittingValidationFailed", "cmd: {bad_cmd}");
    }

    let (post_item_count, post_next_id) =
        ctx.db.with_snapshot(|snap| (snap.items.len(), snap.next_entity_id));
    assert_eq!(post_item_count, pre_item_count);
    assert_eq!(post_next_id, pre_next_id);

    // Explicit Cargo flag (5) remains allowed for valid SDE types.
    let cargo_rep = handle_gm_slash_ext(&ctx, &mut sess, char_id, "/fit me 3657 5")
        .await
        .expect("slash handled")
        .expect("/fit me 3657 5 into explicit Cargo flag must succeed");
    assert_eq!(cargo_rep.dict_get("flagID"), Some(&PyRep::Integer(5)));
}

#[tokio::test]
async fn spawn_1230_veldspar_queues_static_add_balls2_wire_and_slim_item() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let sys_id = 30_000_142_u32;
    let ship_id = ctx.db.with_snapshot(|s| s.characters[&char_id].active_ship_id);
    let far_ship_id = 1_855_099_i64;

    let mut sess = gm_session(char_id, ship_id);
    sess.station_id = None;
    sess.solar_system_id = Some(sys_id);

    let handle = ctx.systems.get_or_spawn(sys_id);
    let origin = glam::DVec3::new(10_000.0, 2_000.0, -5_000.0);
    handle
        .spawn_ball(eve_destiny::Ball {
            id: ship_id,
            type_id: 587,
            position: origin,
            velocity: glam::DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await
        .unwrap();
    handle
        .spawn_ball(eve_destiny::Ball {
            id: far_ship_id,
            type_id: 587,
            position: glam::DVec3::new(100_000_000.0, 0.0, 0.0),
            velocity: glam::DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await
        .unwrap();
    let _ = handle.drain_observer_updates(ship_id).await.unwrap();
    let _ = handle.drain_observer_updates(far_ship_id).await.unwrap();

    let spawn_rep = handle_gm_slash_ext(&ctx, &mut sess, char_id, "/spawn 1230")
        .await
        .expect("slash handled")
        .expect("/spawn 1230 must succeed");
    let ast_id = spawn_rep
        .dict_get("spawnedID")
        .and_then(|v| v.as_i64())
        .expect("spawnedID");
    assert_eq!(spawn_rep.dict_get("itemID"), Some(&PyRep::Integer(ast_id)));
    assert_eq!(spawn_rep.dict_get("typeID"), Some(&PyRep::Integer(1230)));
    assert_eq!(spawn_rep.dict_get("quantity"), Some(&PyRep::Integer(1)));
    assert_eq!(
        spawn_rep.dict_get("solarSystemID"),
        Some(&PyRep::Integer(i64::from(sys_id)))
    );

    let ship_grid = {
        let (ship_grid, far_grid, is_static_ast, has_grid_id, in_grid_members, ast_ball) = handle
            .test_with_ballpark_async(move |bp| {
                let ship_grid = bp.ball_grid_ids.get(&ship_id).copied().expect("ship grid");
                let far_grid = bp.ball_grid_ids.get(&far_ship_id).copied().expect("far grid");
                let is_static_ast = bp.is_static_ball(ast_id);
                let has_grid_id = bp.ball_grid_ids.contains_key(&ast_id);
                let in_grid_members = bp.grids.values().any(|g| g.members.contains(&ast_id));
                let ast_ball = bp.balls[&ast_id].clone();
                (ship_grid, far_grid, is_static_ast, has_grid_id, in_grid_members, ast_ball)
            })
            .await;
        assert_ne!(ship_grid, far_grid, "observers must occupy distinct system grids");
        assert!(is_static_ast, "spawned asteroid must be static");
        assert!(
            !has_grid_id,
            "static spawned asteroid must not be inserted into ball_grid_ids"
        );
        assert!(
            !in_grid_members,
            "static spawned asteroid must not be inserted into GridBubble.members"
        );
        assert_eq!(ast_ball.type_id, 1230);
        assert_eq!(ast_ball.mode, eve_destiny::MovementMode::Rigid);
        assert!(ast_ball.radius >= 200.0, "asteroid radius must be >= 200m");
        assert!(
            (ast_ball.position - origin - glam::DVec3::new(1_500.0, 0.0, 0.0)).length() < 1e-6,
            "asteroid must spawn 1500m ahead of observer ship origin"
        );
        ship_grid
    };

    let (_tick, events, wire_rep) = handle.drain_observer_updates(ship_id).await.unwrap();
    assert!(
        events.iter().any(|ev| {
            matches!(
                ev,
                eve_destiny::DestinyEvent::AddBalls2 { balls }
                    if balls.iter().any(|b| b.id == ast_id && b.type_id == 1230)
            )
        }),
        "same-grid observer must receive AddBalls2 for /spawn 1230: {events:?}"
    );

    let mut verified_wire_slim = false;
    if let PyRep::List(updates) = &wire_rep {
        for entry in updates {
            if let PyRep::Tuple(outer) = entry {
                if let Some(PyRep::Tuple(action)) = outer.get(1) {
                    if action.first() == Some(&PyRep::String("AddBalls2".into())) {
                        if let Some(PyRep::Tuple(args)) = action.get(1) {
                            if let Some(PyRep::Tuple(payload)) = args.first() {
                                if let (
                                    Some(PyRep::Bytes(state_bytes)),
                                    Some(PyRep::List(extra_list)),
                                ) = (payload.first(), payload.get(1))
                                {
                                    assert!(!state_bytes.is_empty());
                                    for item in extra_list {
                                        if let PyRep::Tuple(pair) = item {
                                            if let Some(slim) = pair.first() {
                                                if slim.dict_get("itemID")
                                                    == Some(&PyRep::Integer(ast_id))
                                                {
                                                    assert_eq!(
                                                        slim.dict_get("typeID"),
                                                        Some(&PyRep::Integer(1230))
                                                    );
                                                    assert_eq!(
                                                        slim.dict_get("groupID"),
                                                        Some(&PyRep::Integer(462))
                                                    );
                                                    assert_eq!(
                                                        slim.dict_get("categoryID"),
                                                        Some(&PyRep::Integer(25))
                                                    );
                                                    assert_eq!(
                                                        slim.dict_get("ownerID"),
                                                        Some(&PyRep::Integer(1))
                                                    );
                                                    assert_eq!(
                                                        slim.dict_get("charID"),
                                                        Some(&PyRep::None)
                                                    );
                                                    assert_eq!(
                                                        slim.dict_get("corpID"),
                                                        Some(&PyRep::None)
                                                    );
                                                    assert_eq!(
                                                        slim.dict_get("allianceID"),
                                                        Some(&PyRep::None)
                                                    );
                                                    let wire_radius = slim
                                                        .dict_get("radius")
                                                        .and_then(|v| v.as_f64())
                                                        .unwrap_or(0.0);
                                                    assert!(wire_radius >= 200.0);
                                                    verified_wire_slim = true;
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    assert!(
        verified_wire_slim,
        "wire AddBalls2 must carry valid Veldspar SlimItem: {wire_rep:?}"
    );

    let (_far_tick, far_events, _) = handle.drain_observer_updates(far_ship_id).await.unwrap();
    assert!(
        far_events.iter().any(|ev| {
            matches!(
                ev,
                eve_destiny::DestinyEvent::AddBalls2 { balls }
                    if balls.iter().any(|b| b.id == ast_id && b.type_id == 1230)
            )
        }),
        "same-system observer in separate grid must receive static AddBalls2 fan-out: {far_events:?}"
    );

    let unspawn = handle_gm_slash_ext(&ctx, &mut sess, char_id, &format!("/unspawn {ast_id}"))
        .await
        .expect("slash handled")
        .expect("/unspawn asteroid ok");
    assert_eq!(unspawn.dict_get("removedFromDb"), Some(&PyRep::Bool(true)));

    let (_tick2, rem_events, _) = handle.drain_observer_updates(ship_id).await.unwrap();
    assert!(
        rem_events.iter().any(|ev| {
            matches!(ev, eve_destiny::DestinyEvent::RemoveBalls { ball_ids } if ball_ids == &[ast_id])
        }),
        "/unspawn must emit RemoveBalls([{ast_id}]) to ship observer: {rem_events:?}"
    );
    let (_far_tick2, far_rem_events, _) = handle.drain_observer_updates(far_ship_id).await.unwrap();
    assert!(
        far_rem_events.iter().any(|ev| {
            matches!(ev, eve_destiny::DestinyEvent::RemoveBalls { ball_ids } if ball_ids == &[ast_id])
        }),
        "/unspawn must emit RemoveBalls([{ast_id}]) to all system grid observers: {far_rem_events:?}"
    );

    // Spawning another static asteroid near `ship_id` must allow `ship_grid` to delete cleanly
    // via `contract_or_delete_grid` once dynamic `ship_id` is removed.
    let spawn_rep2 = handle_gm_slash_ext(&ctx, &mut sess, char_id, "/spawn 1230")
        .await
        .expect("slash handled")
        .expect("second /spawn 1230 ok");
    let ast2_id = spawn_rep2.dict_get("spawnedID").and_then(|v| v.as_i64()).unwrap();
    assert!(handle.remove_ball(ship_id).await.is_some());
    let (has_ast2, has_ship_grid) = handle
        .test_with_ballpark_async(move |bp| {
            (bp.balls.contains_key(&ast2_id), bp.grids.contains_key(&ship_grid))
        })
        .await;
    assert!(has_ast2);
    assert!(
        !has_ship_grid,
        "empty dynamic grid must delete via contract_or_delete_grid even near static asteroid"
    );
}

#[tokio::test]
async fn spawn_1230_fails_loudly_and_rolls_back_db_when_actor_command_fails() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let sys_id = 30_000_142_u32;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.active_ship_id = 0;
        }
    });
    let mut sess = gm_session(char_id, 0);
    sess.station_id = None;
    sess.ship_id = None;
    sess.solar_system_id = Some(sys_id);

    let (pre_items, pre_asteroids) =
        ctx.db.with_snapshot(|s| (s.items.len(), s.asteroids.len()));
    let handle = ctx.systems.get_or_spawn(sys_id);
    handle
        .panic_next_command_for_test
        .store(true, std::sync::atomic::Ordering::SeqCst);

    let err = handle_gm_slash_ext(&ctx, &mut sess, char_id, "/spawn 1230")
        .await
        .expect("slash handled")
        .expect_err("failed actor spawn must return SpawnFailed rather than ok=true");
    assert_eq!(err.code, "SpawnFailed");

    let (post_items, post_asteroids) =
        ctx.db.with_snapshot(|s| (s.items.len(), s.asteroids.len()));
    assert_eq!(post_items, pre_items, "failed /spawn must roll back snap.items");
    assert_eq!(
        post_asteroids, pre_asteroids,
        "failed /spawn must roll back snap.asteroids"
    );
}

#[tokio::test]
async fn spawn_multi_npc_rollback_cleans_db_ballpark_and_npc_runtime_state() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let sys_id = 30_000_142_u32;
    let npc_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.category_id == 11 && t.published)
        .or_else(|| ctx.sde.types.values().find(|t| t.category_id == 11))
        .map(|t| t.type_id)
        .expect("SDE must contain at least one category 11 NPC type");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.active_ship_id = 0;
        }
    });
    let mut sess = gm_session(char_id, 0);
    sess.station_id = None;
    sess.ship_id = None;
    sess.solar_system_id = Some(sys_id);

    let (pre_items, pre_next_id) =
        ctx.db.with_snapshot(|s| (s.items.len(), s.next_entity_id));
    let first_npc_id = pre_next_id + 1;
    let second_npc_id = pre_next_id + 2;

    let handle = ctx.systems.get_or_spawn(sys_id);
    // Allow NPC #1 to spawn and register in npc_spawns/combat_runtime, then panic NPC #2.
    handle
        .panic_on_command_countdown_for_test
        .store(2, std::sync::atomic::Ordering::SeqCst);

    let err = handle_gm_slash_ext(&ctx, &mut sess, char_id, &format!("/spawn {npc_tid} 2"))
        .await
        .expect("slash handled")
        .expect_err("mid-batch NPC spawn failure must return SpawnFailed");
    assert_eq!(err.code, "SpawnFailed");

    ctx.db.with_snapshot(|s| {
        assert_eq!(s.items.len(), pre_items, "failed multi-NPC /spawn must roll back snap.items");
        assert!(!s.items.contains_key(&first_npc_id));
        assert!(!s.items.contains_key(&second_npc_id));
    });
    {
        let (has_first, has_second) = handle
            .test_with_ballpark_async(move |bp| {
                (
                    bp.balls.contains_key(&first_npc_id),
                    bp.balls.contains_key(&second_npc_id),
                )
            })
            .await;
        assert!(!has_first);
        assert!(!has_second);
    }
    assert!(
        ctx.npc_spawns
            .read()
            .unwrap()
            .get(&sys_id)
            .is_none_or(|m| !m.entities.contains_key(&first_npc_id) && !m.entities.contains_key(&second_npc_id)),
        "rollback must clean ctx.npc_spawns for prior and failed NPCs"
    );
    assert!(
        !crate::combat::npc_ai::npc_test_helpers::has_npc_entity(&ctx, first_npc_id)
            && !crate::combat::npc_ai::npc_test_helpers::has_npc_entity(&ctx, second_npc_id),
        "rollback must clean combat NPC entity tracking for prior and failed NPCs"
    );
}

#[tokio::test]
async fn gm_bridge_slash_commands_target_explicit_player_across_systems() {
    let ctx = test_ctx();
    let gm_cid = 90_000_003_i64;
    let player_cid = 90_000_001_i64;
    let gm_sys = 30_000_142_u32;
    let gm_station = 60_003_760_i64;
    let player_sys = 30_002_187_u32;
    let gm_ship_id = 1_000_992_101_i64;
    let player_ship_id = 1_000_992_102_i64;

    let target_ship_tid = if ctx.sde.types.contains_key(&32880) && ctx.sde.types.contains_key(&483) {
        32880_u32
    } else {
        587_u32
    };
    let turret_tid = if target_ship_tid == 32880 { 483_u32 } else { 484_u32 };

    ctx.db.with_mut_snapshot(|snap| {
        let mut gm_ch = snap.characters[&player_cid].clone();
        gm_ch.character_id = gm_cid;
        gm_ch.name = "Admin GM".into();
        gm_ch.solar_system_id = gm_sys;
        gm_ch.station_id = Some(gm_station);
        gm_ch.structure_id = None;
        gm_ch.active_ship_id = gm_ship_id;
        snap.characters.insert(gm_cid, gm_ch);

        if let Some(pch) = snap.characters.get_mut(&player_cid) {
            pch.solar_system_id = player_sys;
            pch.station_id = None;
            pch.structure_id = None;
            pch.active_ship_id = player_ship_id;
            pch.security_status = 0.0;
        }
        snap.items.insert(
            gm_ship_id,
            eve_db::InventoryItemRecord {
                item_id: gm_ship_id,
                type_id: target_ship_tid,
                owner_id: gm_cid,
                location_id: gm_station,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            player_ship_id,
            eve_db::InventoryItemRecord {
                item_id: player_ship_id,
                type_id: target_ship_tid,
                owner_id: player_cid,
                location_id: player_sys as i64,
                flag: eve_db::InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        let sk_map = snap.skills.entry(player_cid).or_default();
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

    let player_handle = ctx.systems.get_or_spawn(player_sys);
    let ship_ball = Ball {
        id: player_ship_id,
        type_id: target_ship_tid,
        position: DVec3::new(10_000.0, 20_000.0, 30_000.0),
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let ship_combat = ctx.build_type_combat_state(target_ship_tid);
    let ship_meta = ctx.build_ship_slim_meta(player_cid, target_ship_tid);
    player_handle
        .spawn_ball_with_meta(ship_ball, ship_combat, ship_meta)
        .await
        .expect("spawn player ship ball");

    let mut gm_sess = gm_session(gm_cid, gm_ship_id);
    let _ = ctx
        .db
        .acquire_online_lock(1, 501, Some(player_cid), true, ctx.now_unix());

    let spawn_res = handle_gm_slash_ext(&ctx, &mut gm_sess, gm_cid, &format!("/spawn {player_cid} 587 1"))
        .await
        .expect("handled")
        .expect("/spawn target player");
    assert_eq!(
        spawn_res.dict_get("solarSystemID"),
        Some(&PyRep::Integer(i64::from(player_sys)))
    );
    let spawned_id = spawn_res.dict_get("spawnedID").and_then(|v| v.as_i64()).unwrap();
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&spawned_id].owner_id, player_cid);
        assert_eq!(snap.items[&spawned_id].location_id, i64::from(player_sys));
    });
    assert!(player_handle.get_ball_snapshot(spawned_id).await.ok().flatten().is_some());

    let wreck_res = handle_gm_slash_ext(
        &ctx,
        &mut gm_sess,
        gm_cid,
        &format!("/spawnwreck {player_cid} 587 1"),
    )
    .await
    .expect("handled")
    .expect("/spawnwreck target player");
    assert_eq!(
        wreck_res.dict_get("solarSystemID"),
        Some(&PyRep::Integer(i64::from(player_sys)))
    );

    let fit_res = handle_gm_slash_ext(
        &ctx,
        &mut gm_sess,
        gm_cid,
        &format!("/fit {player_cid} {turret_tid}"),
    )
    .await
    .expect("handled")
    .expect("/fit target player");
    assert_eq!(fit_res.dict_get("shipID"), Some(&PyRep::Integer(player_ship_id)));
    let fitted_id = fit_res.dict_get("itemID").and_then(|v| v.as_i64()).unwrap();
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&fitted_id].owner_id, player_cid);
        assert_eq!(snap.items[&fitted_id].location_id, player_ship_id);
    });

    let setattr_res = handle_gm_slash_ext(
        &ctx,
        &mut gm_sess,
        gm_cid,
        &format!("/setattr {player_cid} 37 777"),
    )
    .await
    .expect("handled")
    .expect("/setattr target player");
    assert_eq!(setattr_res.dict_get("itemID"), Some(&PyRep::Integer(player_ship_id)));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&player_ship_id].dynamic_attributes.get(&37), Some(&777.0));
        assert!(!snap.items[&gm_ship_id].dynamic_attributes.contains_key(&37));
    });

    let load_res = handle_gm_slash_ext(
        &ctx,
        &mut gm_sess,
        gm_cid,
        &format!("/load {player_cid} 34 250"),
    )
    .await
    .expect("handled")
    .expect("/load target player");
    let loaded_id = match &load_res {
        PyRep::List(items) => items[0].as_i64().unwrap(),
        other => panic!("expected /load List reply, got {other:?}"),
    };
    ctx.db.with_snapshot(|snap| {
        let rec = &snap.items[&loaded_id];
        assert_eq!(rec.owner_id, player_cid);
        assert_eq!(rec.location_id, player_ship_id);
        assert_eq!(rec.flag, eve_db::InventoryFlag::Cargo);
        assert_eq!(rec.quantity, 250);
    });

    handle_gm_slash_ext(&ctx, &mut gm_sess, gm_cid, &format!("/energy {player_cid} 42"))
        .await
        .expect("handled")
        .expect("/energy target player");
    let sec_res = handle_gm_slash_ext(&ctx, &mut gm_sess, gm_cid, &format!("/secstatus {player_cid} -2.5"))
        .await
        .expect("handled")
        .expect("/secstatus target player");
    assert_eq!(sec_res, PyRep::Real(-2.5));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.characters[&player_cid].security_status, -2.5);
        assert_ne!(snap.characters[&gm_cid].security_status, -2.5);
    });
}

#[tokio::test]
async fn gm_advancetime_heal_cap_upgrade_and_gm_fit_process_inv_change() {
    let ctx = test_ctx();
    let char_id = 90_000_001i64;
    let sys_id = 30_000_142u32;
    let ship_id = ctx.db.with_snapshot(|s| s.characters[&char_id].active_ship_id);
    let mut sess = gm_session(char_id, ship_id);
    sess.station_id = None;
    sess.structure_id = None;
    sess.solar_system_id = Some(sys_id);
    ctx.db
        .acquire_online_lock(1, sess.session_id.max(1), Some(char_id), true, ctx.now_unix())
        .expect("online lock");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
        }
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.location_id = i64::from(sys_id);
            ship.flag = eve_db::InventoryFlag::None;
        }
        crate::sp6_lifecycle::enter_space_in_snap(snap, ship_id, sys_id);
    });

    // 1. /heal with stale lower max_capacitor (e.g. CombatState::default() or unboarded hull)
    // must upgrade SystemActor max_capacitor from the fitted ship and emit OnCapacitorChange (#1268).
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let mut stale_cs = eve_destiny::CombatState::default();
    stale_cs.max_capacitor = 50.0;
    stale_cs.capacitor = 10.0;
    stale_cs.capacitor_recharge_time_ms = 150_000.0;
    sys_h.try_spawn_ball_with_combat(
        eve_destiny::Ball {
            id: ship_id,
            type_id: 587,
            position: glam::DVec3::ZERO,
            velocity: glam::DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        },
        stale_cs,
    );
    let _ = ctx.drain_notifications_for_char(char_id);

    handle_gm_slash_ext(&ctx, &mut sess, char_id, "/heal")
        .await
        .expect("handled")
        .expect("/heal succeeds");
    let healed_cs = sys_h.get_combat_state(ship_id).await.unwrap().expect("cs");
    assert!(healed_cs.max_capacitor >= 250.0 - 1e-6);
    assert!((healed_cs.capacitor - healed_cs.max_capacitor).abs() < 1e-6);
    let heal_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(heal_notifs.iter().any(|n| n.event == "OnModuleAttributeChanges"));
    assert!(heal_notifs.iter().any(|n| n.event == "OnCapacitorChange"));

    // 2. /advancetime me 6000 recharges stationary drained ship capacitor & emits OnCapacitorChange (#1268).
    let _ = sys_h
        .modify_combat_state(ship_id, |cs| {
            cs.capacitor = 10.0;
        })
        .await;
    let _ = ctx.drain_notifications_for_char(char_id);
    handle_gm_slash_ext(&ctx, &mut sess, char_id, "/advancetime me 6000")
        .await
        .expect("handled")
        .expect("/advancetime me 6000 succeeds");
    let recharged_cs = sys_h.get_combat_state(ship_id).await.unwrap().expect("cs");
    assert!(recharged_cs.capacitor > 245.0, "expected recharged cap > 245, got {}", recharged_cs.capacitor);
    let adv_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(adv_notifs.iter().any(|n| n.event == "OnCapacitorChange"));
    assert!(!adv_notifs.iter().any(|n| n.event == "DoSimClockRebase"));

    // 3. /fit "me" 2048 emits OnItemChange, OnMultipleItemChange, ProcessInventoryChange before OnModuleFitted (#1252).
    ctx.db.with_mut_snapshot(|s| {
        s.items.retain(|_, it| !(it.location_id == ship_id && it.type_id == 2048));
    });
    ctx.invalidate_ship_fitting(ship_id);
    let _ = ctx.drain_notifications_for_char(char_id);
    handle_gm_slash_ext(&ctx, &mut sess, char_id, r#"/fit "me" 2048"#)
        .await
        .expect("handled")
        .expect("/fit succeeds");
    let fit_notifs = ctx.drain_notifications_for_char(char_id);
    let proc_idx = fit_notifs
        .iter()
        .position(|n| n.event == "ProcessInventoryChange")
        .expect("ProcessInventoryChange emitted on /fit");
    let fitted_idx = fit_notifs
        .iter()
        .position(|n| n.event == "OnModuleFitted")
        .expect("OnModuleFitted emitted on /fit");
    assert!(proc_idx < fitted_idx, "ProcessInventoryChange must precede OnModuleFitted");
    assert!(fit_notifs.iter().any(|n| n.event == "OnItemChange"));
    assert!(fit_notifs.iter().any(|n| n.event == "OnMultipleItemChange"));
}

#[tokio::test]
async fn gm_heal_uses_system_sim_clock_after_advancetime_for_attr_changes() {
    let ctx = test_ctx();
    let char_id = 90_000_001i64;
    let sys_id = 30_000_142u32;
    let ship_id = ctx.db.with_snapshot(|s| s.characters[&char_id].active_ship_id);
    let mut sess = gm_session(char_id, ship_id);
    sess.station_id = None;
    sess.structure_id = None;
    sess.solar_system_id = Some(sys_id);
    ctx.db
        .acquire_online_lock(1, sess.session_id.max(1), Some(char_id), true, ctx.now_unix())
        .expect("online lock");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
        }
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.location_id = i64::from(sys_id);
            ship.flag = eve_db::InventoryFlag::None;
        }
        crate::sp6_lifecycle::enter_space_in_snap(snap, ship_id, sys_id);
    });
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let mut cs = eve_destiny::CombatState::default();
    cs.max_capacitor = 168_750.0;
    cs.capacitor = 0.0;
    cs.max_shield_hp = 325_000.0;
    cs.shield_hp = 260_000.0;
    sys_h.try_spawn_ball_with_combat(
        eve_destiny::Ball {
            id: ship_id,
            type_id: 11567,
            position: glam::DVec3::ZERO,
            velocity: glam::DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        },
        cs,
    );

    // Advance virtual clock by 6,000s (as `advance_server_time(6000)` does) without rebasing client sim clock.
    ctx.advance_secs_without_sim_clock_rebase(6_000);
    let _ = ctx.drain_notifications_for_char(char_id);
    let sim_before = ctx.now_system_sim_filetime_ticks_for_ship(char_id, ship_id);

    handle_gm_slash_ext(&ctx, &mut sess, char_id, "/heal")
        .await
        .expect("handled")
        .expect("/heal succeeds");

    let sim_after = ctx.now_system_sim_filetime_ticks_for_ship(char_id, ship_id);
    let wall_after = ctx.now_blueos_filetime_ticks();
    assert!(
        wall_after - sim_after >= 50_000_000_000,
        "virtual clock should remain ~6000s ahead of unrebased client sim clock"
    );

    let notifs = ctx.drain_notifications_for_char(char_id);
    let mut checked = 0usize;
    for n in notifs.iter().filter(|n| n.event == "OnModuleAttributeChanges") {
        if let Some(PyRep::List(entries)) = n.args.first() {
            for entry in entries {
                if let PyRep::Tuple(t) | PyRep::List(t) = entry {
                    let ts = t[4].as_i64().expect("timestamp");
                    assert!(
                        ts >= sim_before && ts <= sim_after,
                        "expected /heal OnModuleAttributeChange ts in [{sim_before}, {sim_after}], got {ts} (wall_after={wall_after})"
                    );
                    checked += 1;
                }
            }
        }
    }
    assert!(checked >= 2, "expected capacitor + shield attr changes from /heal");
}

#[tokio::test]
async fn select_character_id_switch_logs_off_prev_character_and_expires_space_hold() {
    let ctx = test_ctx();
    ctx.set_clock(Arc::new(eve_db::VirtualServerClock::with_fixed_base(
        1_800_000_000,
    )));
    let user_id = 1_u64;
    let sys_id = 30_000_142_u32;
    let station_id = 60_003_760_i64;
    let char_a = 90_000_001_i64;
    let ship_a = 100_000_001_i64;
    let char_b = 90_099_171_i64;
    let ship_b = 100_099_171_i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch_a) = snap.characters.get_mut(&char_a) {
            ch_a.user_id = user_id;
            ch_a.solar_system_id = sys_id;
            ch_a.station_id = None;
            ch_a.structure_id = None;
            ch_a.active_ship_id = ship_a;
        }
        if let Some(sh_a) = snap.items.get_mut(&ship_a) {
            sh_a.owner_id = char_a;
            sh_a.location_id = i64::from(sys_id);
            sh_a.flag = eve_db::InventoryFlag::None;
        }

        let mut ch_b = snap.characters[&char_a].clone();
        ch_b.character_id = char_b;
        ch_b.name = "Alt Char B".into();
        ch_b.user_id = user_id;
        ch_b.solar_system_id = sys_id;
        ch_b.station_id = Some(station_id);
        ch_b.structure_id = None;
        ch_b.active_ship_id = ship_b;
        snap.characters.insert(ch_b.character_id, ch_b);

        let mut sh_b = snap.items[&ship_a].clone();
        sh_b.item_id = ship_b;
        sh_b.owner_id = char_b;
        sh_b.location_id = station_id;
        sh_b.flag = eve_db::InventoryFlag::Hangar;
        snap.items.insert(sh_b.item_id, sh_b);
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle
        .spawn_ball(eve_destiny::Ball {
            id: ship_a,
            type_id: 587,
            position: glam::DVec3::new(10_000.0, 0.0, 0.0),
            velocity: glam::DVec3::ZERO,
            max_velocity: 350.0,
            inertia: 2.5,
            mass: 1_200_000.0,
            radius: 40.0,
            speed_fraction: 1.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await
        .expect("spawn char_a ship in space");

    let mut sess = SessionState::new(71_171);
    sess.user_id = Some(user_id);
    ctx.db
        .acquire_online_lock(user_id, sess.session_id, None, true, ctx.now_unix())
        .expect("acquire session lock");

    let kw = HashMap::new();
    handle_char_unbound(
        &ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(char_a)],
        &kw,
    )
    .expect("SelectCharacterID handled")
    .expect("select char_a succeeds");
    assert_eq!(sess.character_id, Some(char_a));
    assert!(sys_handle.get_ball_snapshot(ship_a).await.unwrap().is_some());

    // Switch on the same session to char_b -> char_a must enter disconnect hold cleanly.
    handle_char_unbound(
        &ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(char_b)],
        &kw,
    )
    .expect("SelectCharacterID switch handled")
    .expect("select char_b succeeds");

    assert_eq!(sess.character_id, Some(char_b));
    assert!(!ctx.is_character_online(char_a, user_id));
    assert!(ctx.is_character_online(char_b, user_id));
    assert!(
        crate::sp6_lifecycle::has_active_disconnected_ship(&ctx.db, char_a),
        "switching character in SelectCharacterID must place prev_cid ship in disconnect hold"
    );

    // Advance virtual clock and step cluster ticks past disconnect hold + emergency warp.
    for _ in 0..3 {
        ctx.advance_secs(30);
        crate::sp6_lifecycle::tick_background_cluster_with_step(&ctx, 30).await;
    }

    assert!(
        sys_handle.get_ball_snapshot(ship_a).await.unwrap().is_none(),
        "prev_cid ship ball must be removed from space once disconnect hold expires"
    );
}

#[tokio::test]
async fn select_character_id_reconnect_preserves_prelogoff_hold_return_warp() {
    let ctx = test_ctx();
    ctx.set_clock(Arc::new(eve_db::VirtualServerClock::with_fixed_base(
        1_800_000_000,
    )));
    let user_id = 1_u64;
    let sys_id = 30_000_142_u32;
    let station_id = 60_003_760_i64;
    let char_a = 90_000_001_i64;
    let ship_a = 100_000_001_i64;
    let char_b = 90_099_172_i64;
    let ship_b = 100_099_172_i64;
    let pre_pos = glam::DVec3::new(150_000.0, 20_000.0, -30_000.0);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch_a) = snap.characters.get_mut(&char_a) {
            ch_a.user_id = user_id;
            ch_a.solar_system_id = sys_id;
            ch_a.station_id = None;
            ch_a.structure_id = None;
            ch_a.active_ship_id = ship_a;
        }
        if let Some(sh_a) = snap.items.get_mut(&ship_a) {
            sh_a.owner_id = char_a;
            sh_a.location_id = i64::from(sys_id);
            sh_a.flag = eve_db::InventoryFlag::None;
        }
        let mut ch_b = snap.characters[&char_a].clone();
        ch_b.character_id = char_b;
        ch_b.name = "Alt Char B2".into();
        ch_b.user_id = user_id;
        ch_b.solar_system_id = sys_id;
        ch_b.station_id = Some(station_id);
        ch_b.structure_id = None;
        ch_b.active_ship_id = ship_b;
        snap.characters.insert(ch_b.character_id, ch_b);

        let mut sh_b = snap.items[&ship_a].clone();
        sh_b.item_id = ship_b;
        sh_b.owner_id = char_b;
        sh_b.location_id = station_id;
        sh_b.flag = eve_db::InventoryFlag::Hangar;
        snap.items.insert(sh_b.item_id, sh_b);
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle
        .spawn_ball(eve_destiny::Ball {
            id: ship_a,
            type_id: 587,
            position: pre_pos,
            velocity: glam::DVec3::ZERO,
            max_velocity: 350.0,
            inertia: 2.5,
            mass: 1_200_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await
        .expect("spawn char_a ship in space");

    let mut sess = SessionState::new(71_172);
    sess.user_id = Some(user_id);
    ctx.db
        .acquire_online_lock(user_id, sess.session_id, None, true, ctx.now_unix())
        .expect("acquire session lock");

    let kw = HashMap::new();
    handle_char_unbound(
        &ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(char_a)],
        &kw,
    )
    .expect("SelectCharacterID handled")
    .expect("select char_a");

    // Switch away -> char_a enters disconnect emergency warp from `pre_pos`.
    handle_char_unbound(
        &ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(char_b)],
        &kw,
    )
    .expect("SelectCharacterID switch handled")
    .expect("switch to char_b");

    // Advance enough for outbound emergency warp to finish (~20s) while keeping the 60s hold active.
    ctx.advance_secs(45);
    crate::sp6_lifecycle::tick_background_cluster_with_step(&ctx, 45).await;
    assert!(
        crate::sp6_lifecycle::has_active_disconnected_ship(&ctx.db, char_a),
        "disconnect hold must remain active before 60s expiry"
    );

    // Switching back to char_a via SelectCharacterID must NOT lose `disconnected_ships[char_a]`
    // during `reconnect_character_in_space_sync`'s internal `on_advance_secs(0)`.
    handle_char_unbound(
        &ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(char_a)],
        &kw,
    )
    .expect("SelectCharacterID reconnect handled")
    .expect("reconnect char_a");

    assert!(ctx.is_character_online(char_a, user_id));
    let recon_ball = sys_handle
        .get_ball_snapshot(ship_a)
        .await
        .unwrap()
        .expect("reconnected ball in space")
        .1;
    match recon_ball.mode {
        eve_destiny::MovementMode::Warp { destination, .. } => {
            assert!(
                destination.distance(pre_pos) < 1.0,
                "reconnect via SelectCharacterID must preserve return warp to pre_warp_position {pre_pos:?}, got {destination:?}"
            );
        }
        other => panic!("expected return Warp back to pre_warp_position, got {other:?}"),
    }
}

#[tokio::test]
async fn select_character_id_displaced_clears_all_session_identity_and_skips_reconnect() {
    let ctx = test_ctx();
    let user_id = 1_u64;
    let sys_id = 30_000_142_u32;
    let station_id = 60_003_760_i64;
    let char_a = 90_000_001_i64;
    let ship_a = 100_000_001_i64;
    let char_b = 90_099_181_i64;
    let s1_id = 71_181_u64;
    let s2_id = 71_182_u64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch_b = snap.characters[&char_a].clone();
        ch_b.character_id = char_b;
        ch_b.name = "Displaced Target Char".into();
        ch_b.user_id = user_id;
        ch_b.solar_system_id = sys_id;
        ch_b.station_id = Some(station_id);
        snap.characters.insert(ch_b.character_id, ch_b);
    });

    ctx.db
        .acquire_online_lock(user_id, s1_id, Some(char_a), true, ctx.now_unix())
        .expect("s1 lock");
    let displaced = ctx
        .db
        .acquire_online_lock(user_id, s2_id, None, true, ctx.now_unix())
        .expect("s2 displaces s1")
        .expect("displaced record");
    assert_eq!(displaced.session_id, s1_id);

    let mut sess = SessionState::new(s1_id);
    sess.user_id = Some(user_id);
    sess.character_id = Some(char_a);
    sess.corporation_id = Some(1_000_001);
    sess.alliance_id = Some(99_000_001);
    sess.war_faction_id = Some(500_001);
    sess.corp_role = 7;
    sess.fleet_id = Some(88_001);
    sess.ship_id = Some(ship_a);
    sess.solar_system_id = Some(sys_id);
    sess.station_id = Some(station_id);

    let kw = HashMap::new();
    let err = handle_char_unbound(
        &ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(char_b)],
        &kw,
    )
    .expect("handled")
    .expect_err("displaced session with s2 holding None must be rejected");

    assert_eq!(err.code, "SessionKicked");
    assert!(
        matches!(
            err.client_error.as_deref(),
            Some(eve_proto::ClientError::SessionMismatch(_))
        ),
        "expected typed SessionMismatch client_error, got {:?}",
        err.client_error
    );
    assert_eq!(sess.character_id, None);
    assert_eq!(sess.corporation_id, None);
    assert_eq!(sess.alliance_id, None);
    assert_eq!(sess.war_faction_id, None);
    assert_eq!(sess.corp_role, 0);
    assert_eq!(sess.fleet_id, None);
    assert_eq!(sess.ship_id, None);
    assert_eq!(sess.solar_system_id, None);
    assert_eq!(sess.station_id, None);
    assert_eq!(sess.structure_id, None);

    // Also when S2 has already selected char_b and queued notifications on char_b,
    // direct SelectCharacterID returns CharacterAlreadyOnline, full dispatch_rpc returns
    // SessionKicked, and neither drains char_b's notifications.
    ctx.db
        .acquire_online_lock(user_id, s2_id, Some(char_b), false, ctx.now_unix())
        .expect("s2 binds char_b");
    let emitted = ctx.notify(
        crate::router::NotificationTarget::Character(char_b),
        "OnAccountChange",
        "charid",
        vec![PyRep::ascii_bytes("cash"), PyRep::Integer(1), PyRep::Real(2.0)],
    );
    assert_eq!(emitted.len(), 1);
    let err2 = handle_char_unbound(
        &ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(char_b)],
        &kw,
    )
    .expect("handled")
    .expect_err("direct SelectCharacterID on char already online on S2 must fail");
    assert_eq!(err2.code, "CharacterAlreadyOnline");
    let err2_rpc = ctx
        .dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "SelectCharacterID",
            &[PyRep::Integer(char_b)],
            &kw,
            None,
        )
        .await
        .expect_err("displaced session dispatch_rpc must fail with SessionKicked");
    assert_eq!(err2_rpc.code, "SessionKicked");
    assert_eq!(
        ctx.drain_notifications_for_char(char_b).len(),
        1,
        "rejected displaced SelectCharacterID must not drain char_b notifications"
    );
}

#[tokio::test]
async fn select_character_id_displaced_vs_character_already_online_contracts() {
    let ctx = test_ctx();
    let user_id = 1_u64;
    let char_a = 90_000_001_i64;
    let char_b = 90_099_182_i64;
    let s1_id = 71_183_u64;
    let s2_id = 71_184_u64;
    let s_unrevoked_id = 71_185_u64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch_b = snap.characters[&char_a].clone();
        ch_b.character_id = char_b;
        ch_b.name = "Alt Online Contract".into();
        ch_b.user_id = user_id;
        ch_b.station_id = Some(60_003_760);
        snap.characters.insert(ch_b.character_id, ch_b);
    });

    // Case 1: S1 owned lock and was displaced by S2 (which now holds char_a).
    // Direct service handler enforces CharacterAlreadyOnline (clearing S1's stale selection
    // identity), while full ctx.dispatch_rpc rejects displaced S1 with SessionKicked.
    ctx.db
        .acquire_online_lock(user_id, s1_id, Some(char_a), true, ctx.now_unix())
        .expect("s1 lock");
    ctx.db
        .acquire_online_lock(user_id, s2_id, Some(char_a), true, ctx.now_unix())
        .expect("s2 displaces s1 and selects char_a");

    let mut sess_s1 = SessionState::new(s1_id);
    sess_s1.user_id = Some(user_id);
    sess_s1.character_id = Some(char_a);
    sess_s1.corporation_id = Some(1_000_001);
    sess_s1.corp_role = 1;
    let kw = HashMap::new();
    let err1 = handle_char_unbound(
        &ctx,
        &mut sess_s1,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(char_a)],
        &kw,
    )
    .expect("handled")
    .expect_err("direct SelectCharacterID on char already online elsewhere must fail");
    assert_eq!(err1.code, "CharacterAlreadyOnline");
    assert_eq!(sess_s1.character_id, None);
    assert_eq!(sess_s1.corporation_id, None);
    assert_eq!(sess_s1.corp_role, 0);

    let err_rpc = ctx
        .dispatch_rpc(
            &mut sess_s1,
            "charUnboundMgr",
            None,
            "SelectCharacterID",
            &[PyRep::Integer(char_a)],
            &kw,
            None,
        )
        .await
        .expect_err("full dispatch_rpc on displaced S1 must fail with SessionKicked");
    assert_eq!(err_rpc.code, "SessionKicked");

    // Case 2: Unrevoked session trying to pick char_b when S2 holds (s2_id, Some(char_a))
    // must get SessionKicked, while picking char_a keeps CharacterAlreadyOnline.
    let mut sess_other = SessionState::new(s_unrevoked_id);
    sess_other.user_id = Some(user_id);
    let err_same = handle_char_unbound(
        &ctx,
        &mut sess_other,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(char_a)],
        &kw,
    )
    .expect("handled")
    .expect_err("same-char conflict returns CharacterAlreadyOnline");
    assert_eq!(err_same.code, "CharacterAlreadyOnline");

    let err_diff = handle_char_unbound(
        &ctx,
        &mut sess_other,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(char_b)],
        &kw,
    )
    .expect("handled")
    .expect_err("different-char lock mismatch returns SessionKicked");
    assert_eq!(err_diff.code, "SessionKicked");
    assert!(matches!(
        err_diff.client_error.as_deref(),
        Some(eve_proto::ClientError::SessionMismatch(_))
    ));
}

#[tokio::test]
async fn displaced_s1_cleanup_and_cluster_tick_respect_s2_reconnect_hold_and_ownership() {
    let ctx = Arc::new(test_ctx());
    ctx.set_clock(Arc::new(eve_db::VirtualServerClock::with_fixed_base(
        1_800_000_000,
    )));
    let user_id = 1_u64;
    let sys_id = 30_000_142_u32;
    let char_a = 90_000_001_i64;
    let ship_a = 100_000_001_i64;
    let s1_id = 71_191_u64;
    let s2_id = 71_192_u64;
    let pos = glam::DVec3::new(120_000.0, 0.0, 0.0);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_a) {
            ch.user_id = user_id;
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_a;
        }
        if let Some(sh) = snap.items.get_mut(&ship_a) {
            sh.owner_id = char_a;
            sh.location_id = i64::from(sys_id);
            sh.flag = eve_db::InventoryFlag::None;
        }
    });
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle
        .spawn_ball(eve_destiny::Ball {
            id: ship_a,
            type_id: 587,
            position: pos,
            velocity: glam::DVec3::ZERO,
            max_velocity: 350.0,
            inertia: 2.5,
            mass: 1_200_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await
        .expect("spawn ball");

    ctx.db
        .acquire_online_lock(user_id, s1_id, Some(char_a), true, ctx.now_unix())
        .expect("s1 acquires lock");
    let displaced = ctx
        .db
        .acquire_online_lock(user_id, s2_id, None, true, ctx.now_unix())
        .expect("s2 displaces s1")
        .expect("displaced record");
    ctx.disconnect_displaced_session(s2_id, &displaced);

    // S1 disconnects cleanly -> char_a enters disconnect hold while S2 prepares SelectCharacterID.
    let mut sess_s1 = SessionState::new(s1_id);
    sess_s1.user_id = Some(user_id);
    sess_s1.character_id = Some(char_a);
    ctx.cleanup_disconnected_client_session(&mut sess_s1).await;
    assert!(crate::sp6_lifecycle::has_active_disconnected_ship(
        &ctx.db, char_a
    ));

    // Advance 30s so emergency warp ran, keeping disconnect hold alive.
    ctx.advance_secs(30);
    crate::sp6_lifecycle::tick_background_cluster_with_step(&ctx, 30).await;
    assert!(crate::sp6_lifecycle::has_active_disconnected_ship(
        &ctx.db, char_a
    ));

    // Simulate active reconnect hold (S2 holds online_lock + reconnect_hold_sessions[char_a]) across cluster tick:
    // the disconnect hold must NOT be pruned until reconnect consumes it and drops the guard.
    let reconnect_guard =
        crate::router::binding::ReconnectHoldGuard::acquire(&ctx, char_a, s2_id);
    ctx.db
        .acquire_online_lock(user_id, s2_id, Some(char_a), false, ctx.now_unix())
        .expect("s2 acquires char_a lock");
    ctx.advance_secs(1);
    crate::sp6_lifecycle::tick_background_cluster_with_step(&ctx, 1).await;
    assert!(
        crate::sp6_lifecycle::has_active_disconnected_ship(&ctx.db, char_a),
        "cluster_tick must preserve disconnect hold while ReconnectHoldGuard is held by S2"
    );
    drop(reconnect_guard);

    // Now run S2 SelectCharacterID(char_a) and verify it restores return-warp cleanly.
    let mut sess_s2 = SessionState::new(s2_id);
    sess_s2.user_id = Some(user_id);
    let kw = HashMap::new();
    handle_char_unbound(
        &ctx,
        &mut sess_s2,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(char_a)],
        &kw,
    )
    .expect("handled")
    .expect("S2 selects char_a cleanly");
    assert!(ctx.is_character_online(char_a, user_id));
    assert!(!crate::sp6_lifecycle::has_active_disconnected_ship(
        &ctx.db, char_a
    ));
    assert_eq!(ctx.reconnect_hold_session(char_a), None);

    // Late S1 cleanup retry must NOT emergency-warp S2's ship or drop S2's ball.
    ctx.cleanup_disconnected_client_session(&mut sess_s1).await;
    assert!(!crate::sp6_lifecycle::has_active_disconnected_ship(
        &ctx.db, char_a
    ));
    let ball = sys_handle
        .get_ball_snapshot(ship_a)
        .await
        .unwrap()
        .expect("ship remains in space under S2")
        .1;
    assert!(
        matches!(ball.mode, eve_destiny::MovementMode::Warp { destination, .. } if destination.distance(pos) < 1.0),
        "expected return Warp to original pos {pos:?}, got {:?}",
        ball.mode
    );
}

#[test]
fn displaced_docked_character_selection_runs_logoff_teardown_only_when_offline() {
    let ctx = test_ctx();
    let user_id = 1_u64;
    let char_id = 90_000_001_i64;
    let s1_id = 71_291_u64;
    let s2_id = 71_292_u64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.user_id = user_id;
            ch.station_id = Some(60_003_760);
            ch.structure_id = None;
        }
    });
    ctx.db
        .acquire_online_lock(user_id, s1_id, Some(char_id), true, ctx.now_unix())
        .expect("s1 acquires lock");
    ctx.db
        .acquire_online_lock(user_id, s2_id, None, true, ctx.now_unix())
        .expect("s2 displaces s1 while char_id is still offline");

    ctx.notify(
        crate::router::NotificationTarget::Character(char_id),
        "OnAccountChange",
        "charid",
        vec![],
    );
    let mut sess_s1 = SessionState::new(s1_id);
    sess_s1.user_id = Some(user_id);
    sess_s1.character_id = Some(char_id);
    sess_s1.station_id = Some(60_003_760);

    let err = super::finalize_displaced_character_selection(&ctx, &mut sess_s1, user_id, char_id);
    assert_eq!(err.code, "SessionKicked");
    assert_eq!(sess_s1.character_id, None);
    assert!(
        ctx.drain_notifications_for_char(char_id).is_empty(),
        "P1-4: docked displaced mid-reconnect must invoke logoff_character_sync when offline"
    );

    // When S2 already brought char_id online, S1 teardown must NOT wipe S2's notifications.
    ctx.db
        .acquire_online_lock(user_id, s2_id, Some(char_id), true, ctx.now_unix())
        .expect("s2 owns char_id online");
    ctx.notify(
        crate::router::NotificationTarget::Character(char_id),
        "OnAccountChange",
        "charid",
        vec![],
    );
    let mut sess_s1_late = SessionState::new(s1_id);
    sess_s1_late.user_id = Some(user_id);
    let err2 =
        super::finalize_displaced_character_selection(&ctx, &mut sess_s1_late, user_id, char_id);
    assert_eq!(err2.code, "SessionKicked");
    assert_eq!(
        ctx.drain_notifications_for_char(char_id).len(),
        1,
        "P1-4: docked displaced teardown must skip logoff_character_sync if survivor is online"
    );

    // P2-5: active ban (`UserBanned`) on displaced character selection must be preserved, not masked as SessionKicked.
    let banned_uid = 778_u64;
    let banned_cid = 90_000_002_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.accounts.insert(
            "ban_select_target".into(),
            eve_db::AccountRecord {
                user_id: banned_uid,
                username: "ban_select_target".into(),
                password_token: String::new(),
                role: eve_db::service_roles::ROLE_PLAYER_DEFAULT,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
        if let Some(ch) = snap.characters.get_mut(&banned_cid) {
            ch.user_id = banned_uid;
        }
    });
    ctx.db
        .ban_account("ban_select_target", ctx.now_unix() + 3_600, "policy ban")
        .expect("ban account");
    let mut sess_banned = SessionState::new(71_293);
    sess_banned.user_id = Some(banned_uid);
    sess_banned.character_id = Some(banned_cid);
    let err_ban = super::finalize_displaced_character_selection(
        &ctx,
        &mut sess_banned,
        banned_uid,
        banned_cid,
    );
    assert_eq!(
        err_ban.code, "UserBanned",
        "banned session teardown must preserve UserBanned instead of SessionKicked"
    );
}
