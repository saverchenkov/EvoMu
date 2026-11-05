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
}
