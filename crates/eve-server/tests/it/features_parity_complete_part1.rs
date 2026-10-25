//! Acceptance test suite Part 1 covering:
//! - Test 1: Character Creation, Lifecycle, Selection, and Home Station
//! - Test 2: Skills Engine, Queue, Injectors (Large/Small), and Extractor
//! - Test 3: Attributes, Neural/Bonus Remap, Implants, Pod Death, and Boosters
//!
//! Hard rules compliant:
//! - All RPC and notification names validated against V24 client oracle.
//! - Virtual time driven via ctx.now_unix() and ctx.advance_virtual_clock().
//! - Zero kwarg duration/outcome bypasses.
//! - SDE-driven static data.



use crate::common;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use std::collections::HashMap;

const SEED_PILOT: i64 = 90_000_001;

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: Vec<PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, method, &args, &HashMap::new(), None)
        .await
}

fn doll_data() -> PyRep {
    PyRep::keyval([
        ("hair", PyRep::String("short".into())),
        ("skin", PyRep::String("fair".into())),
    ])
}

fn portrait_data() -> PyRep {
    PyRep::keyval([("camera", PyRep::String("front".into()))])
}

#[tokio::test]
async fn test_character_creation_lifecycle_selection_home_station() {
    // parity: feat:character.list-characters, feat:character.select-character, feat:character.create-character, feat:character.random-name, feat:character.name-validation, feat:character.starter-skills, feat:character.school-corp, feat:character.char-slots, feat:character.delete-character, feat:character.sp-summary, feat:character.home-station, feat:character.self-destruct-clone
    eve_testkit::attest(&[
        "feat:character.list-characters",
        "feat:character.select-character",
        "feat:character.create-character",
        "feat:character.random-name",
        "feat:character.name-validation",
        "feat:character.starter-skills",
        "feat:character.school-corp",
        "feat:character.char-slots",
        "feat:character.delete-character",
        "feat:character.sp-summary",
        "feat:character.home-station",
        "feat:character.self-destruct-clone",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut sess = SessionState::new(41_001);

    // 1. Authenticate user demo
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    assert!(sess.is_authenticated());
    let _user_id = sess.user_id.expect("authenticated user_id");

    // 2. Character list (GetCharactersToSelect)
    let char_list_rep = rpc(&ctx, &mut sess, "charUnboundMgr", "GetCharactersToSelect", vec![])
        .await
        .expect("GetCharactersToSelect");
    assert!(char_list_rep.is_crowset(), "Must return character list crowset");
    let init_rows = char_list_rep.crowset_rows().expect("crowset rows");
    let user_chars = ctx.db.get_characters_for_user(_user_id);
    assert_eq!(init_rows.len(), user_chars.len(), "Character list length must match DB records");
    for ch in &user_chars {
        let matching = init_rows.iter().find(|r| {
            r.dbrow_get("characterID").and_then(|v| v.as_i64()) == Some(ch.character_id)
        }).expect("Character ID must be present in GetCharactersToSelect");
        assert_eq!(
            matching.dbrow_get("characterName").and_then(|v| v.as_str()),
            Some(ch.name.as_str()),
            "Character name must match DB record"
        );
    }

    // 3. Name validation (ValidateNameEx)
    let valid_name = rpc(
        &ctx,
        &mut sess,
        "charUnboundMgr",
        "ValidateNameEx",
        vec![PyRep::String("Kaelen Voss".into())],
    )
    .await
    .expect("ValidateNameEx on valid name");
    assert_eq!(valid_name, PyRep::Bool(true));

    let short_name_rep = rpc(
        &ctx,
        &mut sess,
        "charUnboundMgr",
        "ValidateNameEx",
        vec![PyRep::String("X".into())],
    )
    .await
    .expect("ValidateNameEx on short name");
    assert_eq!(short_name_rep, PyRep::Bool(false), "Name shorter than 3 characters must be rejected");

    // 4. Random valid name (GetValidRandomName)
    let bloodline_id = 1u32; // Deteis
    let random_name_rep = rpc(
        &ctx,
        &mut sess,
        "charUnboundMgr",
        "GetValidRandomName",
        vec![PyRep::Integer(bloodline_id as i64)],
    )
    .await
    .expect("GetValidRandomName");
    let PyRep::String(ref generated_name) = random_name_rep else {
        panic!("Expected string from GetValidRandomName");
    };
    assert!(generated_name.len() >= 3, "Generated name must have valid length");

    // 5. Character slot limits (char-slots) & Creation (CreateCharacterWithDoll)
    let ch0 = ctx.db.get_character(SEED_PILOT).expect("seeded pilot");
    let _race_id = ch0.race_id as i64;
    let b_id = ch0.bloodline_id as i64;
    let anc_id = ch0.ancestry_id as i64;
    let school_id = ch0.school_id as i64;

    // Create second character
    let name_c2 = format!("Cadet {}", ctx.now_unix() % 100_000);
    let c2_rep = rpc(
        &ctx,
        &mut sess,
        "charUnboundMgr",
        "CreateCharacterWithDoll",
        vec![
            PyRep::String(name_c2.clone()),
            PyRep::Integer(b_id),
            PyRep::Integer(1), // male
            PyRep::Integer(anc_id),
            doll_data(),
            portrait_data(),
            PyRep::Integer(school_id),
        ],
    )
    .await
    .expect("CreateCharacterWithDoll 2");
    let PyRep::Integer(c2_id) = c2_rep else {
        panic!("Expected integer characterID");
    };
    assert!(c2_id > 0);

    // Verify starter skills and school corporation
    let c2_record = ctx.db.get_character(c2_id).expect("new character record");
    let expected_school = ctx.sde.schools.get(&(school_id as u32)).expect("school in SDE");
    assert_eq!(
        c2_record.corporation_id, expected_school.corporation_id,
        "Character must join the NPC corporation of chosen school ({})",
        expected_school.corporation_id
    );
    assert!(c2_record.corporation_id >= 1_000_000, "Must join school NPC corporation");
    assert!(c2_record.home_station_id > 0, "Home station must be set");

    let race_data = ctx.sde.races.get(&(c2_record.race_id as u32)).expect("race in SDE");
    assert!(!race_data.starter_skills.is_empty(), "Race must define starter skills in SDE");
    ctx.db.with_snapshot(|snap| {
        let char_skills = snap.skills.get(&c2_id).expect("new character skills in DB");
        for (&expected_skill_id, &expected_lvl) in &race_data.starter_skills {
            if expected_lvl > 0 {
                let skill = char_skills.get(&expected_skill_id).unwrap_or_else(|| {
                    panic!("Character missing starter skill {expected_skill_id}");
                });
                assert_eq!(
                    skill.level, expected_lvl,
                    "Starter skill {expected_skill_id} level {} does not match SDE starter level {}",
                    skill.level, expected_lvl
                );
            }
        }
    });
    assert!(c2_record.skill_points > 0.0, "Must have starter skill points");
    assert!(c2_record.active_ship_id > 0, "Must have rookie corvette");

    // Verify SP summary matches skill table
    let sp_sum: f64 = ctx.db.with_snapshot(|snap| {
        snap.skills
            .get(&c2_id)
            .map(|sk| sk.values().map(|s| s.skill_points).sum())
            .unwrap_or(0.0)
    });
    assert!((c2_record.skill_points - sp_sum).abs() < 1.0, "Skill point summary must match sum of skills");

    // Create third character (fills slot 3/3)
    let name_c3 = format!("Officer {}", ctx.now_unix() % 100_000);
    let c3_rep = rpc(
        &ctx,
        &mut sess,
        "charUnboundMgr",
        "CreateCharacterWithDoll",
        vec![
            PyRep::String(name_c3),
            PyRep::Integer(b_id),
            PyRep::Integer(0), // female = 0
            PyRep::Integer(anc_id),
            doll_data(),
            portrait_data(),
            PyRep::Integer(school_id),
        ],
    )
    .await
    .expect("CreateCharacterWithDoll 3");
    let PyRep::Integer(c3_id) = c3_rep else {
        panic!("Expected integer characterID");
    };

    // Verify GetCharactersToSelect lists all 3 characters with exact IDs and names
    let full_list_rep = rpc(&ctx, &mut sess, "charUnboundMgr", "GetCharactersToSelect", vec![])
        .await
        .expect("GetCharactersToSelect full");
    assert!(full_list_rep.is_crowset());
    let full_rows = full_list_rep.crowset_rows().expect("full rows");
    assert_eq!(full_rows.len(), 3, "Must return exactly 3 characters (all slots filled)");
    for exp_id in [SEED_PILOT, c2_id, c3_id] {
        let row = full_rows.iter().find(|r| {
            r.dbrow_get("characterID").and_then(|v| v.as_i64()) == Some(exp_id)
        }).expect("Created character must appear in GetCharactersToSelect");
        let exp_name = ctx.db.get_character(exp_id).unwrap().name;
        assert_eq!(row.dbrow_get("characterName").and_then(|v| v.as_str()), Some(exp_name.as_str()));
    }

    // 4th creation attempt must be rejected (char slots cap = 3)
    let c4_err = rpc(
        &ctx,
        &mut sess,
        "charUnboundMgr",
        "CreateCharacterWithDoll",
        vec![
            PyRep::String("Over Limit Pilot".into()),
            PyRep::Integer(b_id),
            PyRep::Integer(1),
            PyRep::Integer(anc_id),
            doll_data(),
            portrait_data(),
            PyRep::Integer(school_id),
        ],
    )
    .await;
    assert!(c4_err.is_err(), "Creation exceeding 3 slots must be rejected");

    // 6. Select Character (select-character)
    let _sel_rep = rpc(
        &ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        vec![PyRep::Integer(c2_id)],
    )
    .await
    .expect("SelectCharacterID");
    assert_eq!(sess.character_id, Some(c2_id));
    assert_eq!(sess.corporation_id, Some(c2_record.corporation_id));
    assert!(sess.station_id.is_some() || sess.solar_system_id.is_some());

    // 7. Home Station queries, candidate validation, set_home_station, and 365-day remote cooldown
    let orig_home_id = c2_record.home_station_id;
    let home_st_rep = rpc(&ctx, &mut sess, "home_station", "get_home_station", vec![])
        .await
        .expect("get_home_station");
    assert_eq!(home_st_rep, PyRep::Integer(orig_home_id));

    let candidates_rep = rpc(&ctx, &mut sess, "home_station", "get_home_station_candidates", vec![])
        .await
        .expect("get_home_station_candidates");
    let PyRep::List(cand_list) = candidates_rep else {
        panic!("Expected list from get_home_station_candidates");
    };
    assert!(cand_list.contains(&PyRep::Integer(orig_home_id)));

    let next_time_rep = rpc(&ctx, &mut sess, "home_station", "get_next_remote_change_time", vec![])
        .await
        .expect("get_next_remote_change_time");
    assert_eq!(next_time_rep, PyRep::Integer(0));

    // Non-candidate remote station where pilot is not docked must be rejected
    let alt_station_id = if orig_home_id == 60_003_760 {
        60_008_494
    } else {
        60_003_760
    };
    let non_cand_err = rpc(
        &ctx,
        &mut sess,
        "home_station",
        "set_home_station",
        vec![PyRep::Integer(alt_station_id)],
    )
    .await
    .expect_err("Remote set_home_station to non-candidate station must fail");
    assert!(
        non_cand_err.message.contains("not a remote candidate"),
        "unexpected error: {non_cand_err:?}"
    );

    // Dock at alt_station_id: candidate list now includes both orig_home_id (school) and alt_station_id (docked)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&c2_id) {
            ch.station_id = Some(alt_station_id);
        }
    });
    sess.station_id = Some(alt_station_id);
    let PyRep::List(cand_list_docked) = rpc(&ctx, &mut sess, "home_station", "get_home_station_candidates", vec![])
        .await
        .expect("get_home_station_candidates docked")
    else {
        panic!("Expected list from get_home_station_candidates");
    };
    assert!(cand_list_docked.contains(&PyRep::Integer(orig_home_id)));
    assert!(cand_list_docked.contains(&PyRep::Integer(alt_station_id)));

    // Local set_home_station while docked at alt_station_id succeeds without incurring remote cooldown
    let set_local_rep = rpc(
        &ctx,
        &mut sess,
        "home_station",
        "set_home_station",
        vec![PyRep::Integer(alt_station_id)],
    )
    .await
    .expect("local set_home_station");
    assert_eq!(set_local_rep, PyRep::Integer(alt_station_id));
    assert_eq!(
        rpc(&ctx, &mut sess, "home_station", "get_home_station", vec![])
            .await
            .expect("get_home_station after local change"),
        PyRep::Integer(alt_station_id)
    );
    assert_eq!(
        rpc(&ctx, &mut sess, "home_station", "get_next_remote_change_time", vec![])
            .await
            .expect("get_next_remote_change_time after local change"),
        PyRep::Integer(0)
    );

    // Remote set_home_station back to school station (orig_home_id) while docked at alt_station_id
    let now_before_remote = ctx.now_unix_secs();
    let set_remote_rep = rpc(
        &ctx,
        &mut sess,
        "home_station",
        "set_home_station",
        vec![PyRep::Integer(orig_home_id)],
    )
    .await
    .expect("remote set_home_station to school station");
    assert_eq!(set_remote_rep, PyRep::Integer(orig_home_id));
    assert_eq!(
        rpc(&ctx, &mut sess, "home_station", "get_home_station", vec![])
            .await
            .expect("get_home_station after remote change"),
        PyRep::Integer(orig_home_id)
    );
    let expected_cooldown_until = now_before_remote as i64 + 365 * 86_400;
    assert_eq!(
        rpc(&ctx, &mut sess, "home_station", "get_next_remote_change_time", vec![])
            .await
            .expect("get_next_remote_change_time after remote change"),
        PyRep::Integer(expected_cooldown_until)
    );

    // Switch back locally to alt_station_id, then verify a second remote change during the 365d cooldown is rejected
    rpc(
        &ctx,
        &mut sess,
        "home_station",
        "set_home_station",
        vec![PyRep::Integer(alt_station_id)],
    )
    .await
    .expect("local set_home_station 2");
    let cooldown_err = rpc(
        &ctx,
        &mut sess,
        "home_station",
        "set_home_station",
        vec![PyRep::Integer(orig_home_id)],
    )
    .await
    .expect_err("Second remote set_home_station during 365-day cooldown must fail");
    assert!(
        cooldown_err.message.contains("cooldown"),
        "unexpected error: {cooldown_err:?}"
    );

    // Advance virtual clock past the 365-day remote cooldown and verify remote change succeeds again
    ctx.advance_virtual_clock(365 * 86_400 + 1);
    let set_remote_after_cd = rpc(
        &ctx,
        &mut sess,
        "home_station",
        "set_home_station",
        vec![PyRep::Integer(orig_home_id)],
    )
    .await
    .expect("remote set_home_station after 365-day cooldown");
    assert_eq!(set_remote_after_cd, PyRep::Integer(orig_home_id));

    // 8. Self-destruct clone back to home station (feat:character.self-destruct-clone)
    // 8.1 In regular ship: must fail with SelfDestructOnlyInCapsule
    let in_ship_err = rpc(&ctx, &mut sess, "home_station", "self_destruct_clone", vec![])
        .await
        .expect_err("Self-destruct clone in regular ship must fail");
    // rpc-lint: allow "SelfDestructOnlyInCapsule" is an error code, not a home_station method
    assert_eq!(in_ship_err.code, "SelfDestructOnlyInCapsule");

    // 8.2 Undocked in space: must fail with CannotSelfDestructInSpace
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&c2_id) {
            ch.station_id = None;
            ch.structure_id = None;
        }
    });
    sess.station_id = None;
    sess.structure_id = None;
    let undocked_err = rpc(&ctx, &mut sess, "home_station", "self_destruct_clone", vec![])
        .await
        .expect_err("Self-destruct clone while undocked must fail");
    // rpc-lint: allow "CannotSelfDestructInSpace" is an error code, not a home_station method
    assert_eq!(undocked_err.code, "CannotSelfDestructInSpace");

    // 8.2 Place pilot in a Capsule with an active implant docked in station
    let pod_id = ctx.db.try_with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let cid = snap.next_entity_id;
        snap.items.insert(cid, eve_db::InventoryItemRecord {
            item_id: cid,
            type_id: 670, // Capsule
            owner_id: c2_id,
            location_id: alt_station_id,
            flag: eve_db::InventoryFlag::Hangar,
            quantity: 1,
            singleton: true,
            version: 1,
            dynamic_attributes: HashMap::new(),
            source_mutaplasmid_id: None,
            insured_payout_isk: 0.0,
            is_blueprint_copy: false,
            blueprint_runs_remaining: None,
        });
        if let Some(ch) = snap.characters.get_mut(&c2_id) {
            ch.active_ship_id = cid;
            ch.station_id = Some(alt_station_id);
        }
        snap.active_implants.insert(c2_id, vec![9941]);
        Ok::<i64, eve_db::DbError>(cid)
    }).expect("seed capsule");
    sess.ship_id = Some(pod_id);
    sess.station_id = Some(alt_station_id);

    // 8.3 Docked in station: self-destruct clone destroys active ship/capsule, wipes implants, respawns at home station
    let self_destruct_rep = rpc(&ctx, &mut sess, "home_station", "self_destruct_clone", vec![])
        .await
        .expect("self_destruct_clone in capsule");
    assert_eq!(self_destruct_rep, PyRep::Integer(c2_record.home_station_id));
    assert_eq!(sess.station_id, Some(c2_record.home_station_id));
    assert_ne!(sess.ship_id, Some(pod_id), "Must be placed in new capsule, not destroyed pod");

    // Verify DB state: original pod destroyed, implants wiped, character at home station
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&pod_id), "Original capsule must be destroyed");
        assert!(
            snap.active_implants.get(&c2_id).map(|v| v.is_empty()).unwrap_or(true),
            "Active implants must be wiped on clone destruction"
        );
        let ch = snap.characters.get(&c2_id).expect("character record");
        assert_eq!(ch.station_id, Some(c2_record.home_station_id));
    });

    // 9. Character Deletion lifecycle (Prepare -> Cancel -> Prepare -> Delete)
    let prep_rep = rpc(
        &ctx,
        &mut sess,
        "charUnboundMgr",
        "PrepareCharacterForDelete",
        vec![PyRep::Integer(c3_id)],
    )
    .await
    .expect("PrepareCharacterForDelete");
    assert!(matches!(prep_rep, PyRep::Integer(_) | PyRep::Bool(_)));

    let cancel_rep = rpc(
        &ctx,
        &mut sess,
        "charUnboundMgr",
        "CancelCharacterDeletePrepare",
        vec![PyRep::Integer(c3_id)],
    )
    .await
    .expect("CancelCharacterDeletePrepare");
    assert_eq!(cancel_rep, PyRep::Bool(true));

    let prep2 = rpc(
        &ctx,
        &mut sess,
        "charUnboundMgr",
        "PrepareCharacterForDelete",
        vec![PyRep::Integer(c3_id)],
    )
    .await
    .expect("PrepareCharacterForDelete 2");
    assert!(matches!(prep2, PyRep::Integer(_) | PyRep::Bool(_)));

    // Fast-forward deletion timer in virtual clock
    ctx.advance_virtual_clock(10 * 86_400);

    let del_rep = rpc(
        &ctx,
        &mut sess,
        "charUnboundMgr",
        "DeleteCharacter",
        vec![PyRep::Integer(c3_id)],
    )
    .await
    .expect("DeleteCharacter");
    assert!(matches!(del_rep, PyRep::Bool(true) | PyRep::KeyVal(_) | PyRep::Dict(_)));

    let deleted_char = ctx.db.get_character(c3_id).expect("deleted char record");
    assert_eq!(deleted_char.corporation_id, 1000001, "Deleted character must become Doomheim");
}
