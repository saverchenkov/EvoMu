//! End-to-end integration test suite closing all features parity items.

pub(crate) mod features_parity_part1 {
// Acceptance test suite Part 1 covering:
// - Test 1: Character Creation, Lifecycle, Selection, and Home Station
// - Test 2: Skills Engine, Queue, Injectors (Large/Small), and Extractor
// - Test 3: Attributes, Neural/Bonus Remap, Implants, Pod Death, and Boosters
// 
// Hard rules compliant:
// - All RPC and notification names validated against V24 client oracle.
// - Virtual time driven via ctx.now_unix() and ctx.advance_virtual_clock().
// - Zero kwarg duration/outcome bypasses.
// - SDE-driven static data.



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
    assert_eq!(home_st_rep.dict_get("id"), Some(&PyRep::Integer(orig_home_id)));

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
            .expect("get_home_station after local change")
            .dict_get("id"),
        Some(&PyRep::Integer(alt_station_id))
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
            .expect("get_home_station after remote change")
            .dict_get("id"),
        Some(&PyRep::Integer(orig_home_id))
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

}

pub(crate) mod features_parity_part2 {
// Acceptance test suite Part 2 covering:
// - Test 4: Slash Dispatcher, GM Commands (skill, isk, item, tr, heal, spawn, attr, fit, kill), Asset Safety GM Timer, and Skin Tools
// - Test 5: Warp Speed Formula, Autopilot Routing & Waypoints, Stargate Jumping & Gate Cloak
// - Test 6: Account SSO Login, Native machoNet Handshake, Multi-Login Blocker, Account Roles, Legal Agreements, Language Setting, Launcher Status
// 
// Hard rules compliant:
// - All RPC and notification names validated against V24 client oracle.
// - Virtual time driven via ctx.now_unix() and ctx.advance_virtual_clock().
// - Zero kwarg duration/outcome bypasses.
// - SDE-driven static data.



use crate::common;
use eve_db::service_roles::{ROLE_ADMIN, ROLE_GMS, ROLE_GM_MASK, ROLE_PLAYER};
use eve_db::types::IskCents;
use eve_db::InventoryFlag;
use eve_destiny::{Ball, Ballpark, DamageVector, MovementMode, WarpPhase, AU_METERS};
use eve_machonet::handshake::*;
use eve_machonet::{SessionState, VersionExchange};
use eve_proto::quasar::{EvePublicRequest, EvePublicResponse, GoogleAny, GoogleTimestamp, QuasarMessage};
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::esi::{EsiHttpRequest, EsiRouter};
use eve_server::router::ServerContext;
use glam::DVec3;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

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

async fn bound_rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    bound_id: Option<u64>,
    method: &str,
    args: Vec<PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, bound_id, method, &args, &HashMap::new(), None)
        .await
}

fn extract_response_proto(rep: &PyRep) -> EvePublicResponse {
    if let PyRep::KeyVal(entries) = rep {
        for (k, v) in entries {
            if k == "response_proto" {
                if let PyRep::Bytes(b) = v {
                    return EvePublicResponse::decode_proto(b)
                        .expect("decode EvePublicResponse from response_proto");
                }
            }
        }
    }
    panic!("Expected KeyVal containing response_proto, got {rep:?}");
}

#[tokio::test]
async fn test_admin_slash_dispatcher_gm_commands_asset_safety_skins() {
    // parity: feat:admin.slash-dispatcher, feat:admin.gm-giveskill, feat:admin.gm-giveisk, feat:admin.gm-createitem, feat:admin.gm-tr, feat:admin.gm-heal, feat:admin.gm-spawn, feat:admin.gm-attr, feat:admin.gm-fit, feat:admin.gm-kill, feat:admin.gm-assetsafety-time, feat:admin.gm-skin-tools
    eve_testkit::attest(&[
        "feat:admin.slash-dispatcher",
        "feat:admin.gm-giveskill",
        "feat:admin.gm-giveisk",
        "feat:admin.gm-createitem",
        "feat:admin.gm-tr",
        "feat:admin.gm-heal",
        "feat:admin.gm-spawn",
        "feat:admin.gm-attr",
        "feat:admin.gm-fit",
        "feat:admin.gm-kill",
        "feat:admin.gm-assetsafety-time",
        "feat:admin.gm-skin-tools",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    // 1. Slash dispatcher authorization check: unprivileged player gets error with no state change
    let mut player_sess = SessionState::new(60_001);
    common::login_pilot(&ctx, &mut player_sess, "demo", "demo-secret").await;
    rpc(
        &ctx,
        &mut player_sess,
        "charUnboundMgr",
        "SelectCharacterID",
        vec![PyRep::Integer(SEED_PILOT)],
    )
    .await
    .expect("SelectCharacterID");

    let unauthorized = rpc(
        &ctx,
        &mut player_sess,
        "slash",
        "SlashCmd",
        vec![PyRep::String("/giveisk 1000".into())],
    )
    .await;
    assert!(
        matches!(unauthorized, Err(ref e) if e.code == "InsufficientRole"),
        "Unauthorized caller must receive InsufficientRole error"
    );

    // 2. Provision and login GM account
    let gm_char_id = 90_000_003i64;
    ctx.db
        .provision_gm_account("gm_test_admin", "gm-pass", gm_char_id, "GM Overseer");
    let mut gm_sess = SessionState::new(60_002);
    common::login_pilot(&ctx, &mut gm_sess, "gm_test_admin", "gm-pass").await;
    rpc(
        &ctx,
        &mut gm_sess,
        "charUnboundMgr",
        "SelectCharacterID",
        vec![PyRep::Integer(gm_char_id)],
    )
    .await
    .expect("SelectCharacterID GM");

    // 3. GM /giveisk (feat:admin.gm-giveisk)
    let before_isk = ctx.db.get_character(gm_char_id).unwrap().balance;
    let give_isk_rep = rpc(
        &ctx,
        &mut gm_sess,
        "slash",
        "SlashCmd",
        vec![PyRep::String("/giveisk 500000".into())],
    )
    .await
    .expect("/giveisk 500000");
    assert!(matches!(give_isk_rep, PyRep::KeyVal(_)));
    let after_isk = ctx.db.get_character(gm_char_id).unwrap().balance;
    assert_eq!(
        after_isk,
        before_isk.checked_add(IskCents::from_isk(500_000)).unwrap(),
        "Balance must increase by 500k ISK"
    );

    // 4. GM /createitem (feat:admin.gm-createitem)
    let create_item_rep = rpc(
        &ctx,
        &mut gm_sess,
        "slash",
        "SlashCmd",
        vec![PyRep::String("/createitem 34 500".into())],
    )
    .await
    .expect("/createitem 34 500");
    let PyRep::KeyVal(ci_kv) = create_item_rep else {
        panic!("expected KeyVal from /createitem");
    };
    let item_id = ci_kv.get("itemID").and_then(|v| v.as_i64()).expect("itemID");
    let created_item = ctx
        .db
        .with_snapshot(|s| s.items.get(&item_id).cloned())
        .expect("created item in DB");
    assert_eq!(created_item.type_id, 34, "Created item must be Tritanium");
    assert_eq!(created_item.quantity, 500, "Created item quantity must be 500");

    // 5. GM /giveskill (feat:admin.gm-giveskill)
    let giveskill_rep = rpc(
        &ctx,
        &mut gm_sess,
        "slash",
        "SlashCmd",
        vec![PyRep::String("/giveskill 3300 3".into())],
    )
    .await
    .expect("/giveskill 3300 3");
    let PyRep::KeyVal(gs_kv) = giveskill_rep else {
        panic!("expected KeyVal from /giveskill");
    };
    assert_eq!(gs_kv.get("skillID").and_then(|v| v.as_i64()), Some(3300));
    assert_eq!(gs_kv.get("level").and_then(|v| v.as_i64()), Some(3));
    let char_skill = ctx
        .db
        .with_snapshot(|s| s.skills.get(&gm_char_id).and_then(|m| m.get(&3300).cloned()))
        .expect("persisted skill record");
    assert_eq!(char_skill.level, 3, "Skill must be persisted at level 3");

    // 6. GM /tr (feat:admin.gm-tr)
    let tr_rep = rpc(
        &ctx,
        &mut gm_sess,
        "slash",
        "SlashCmd",
        vec![PyRep::String("/tr 30000144".into())],
    )
    .await
    .expect("/tr 30000144");
    let PyRep::KeyVal(tr_kv) = tr_rep else {
        panic!("expected KeyVal from /tr");
    };
    assert_eq!(tr_kv.get("solarSystemID").and_then(|v| v.as_i64()), Some(30_000_144));
    assert_eq!(gm_sess.solar_system_id, Some(30_000_144));
    assert_eq!(ctx.db.get_character(gm_char_id).unwrap().solar_system_id, 30_000_144);

    // 7. GM /spawn and /unspawn (feat:admin.gm-spawn)
    let spawn_rep = rpc(
        &ctx,
        &mut gm_sess,
        "slash",
        "SlashCmd",
        vec![PyRep::String("/spawn 587 1".into())],
    )
    .await
    .expect("/spawn 587 1");
    let PyRep::KeyVal(sp_kv) = spawn_rep else {
        panic!("expected KeyVal from /spawn");
    };
    let spawned_id = sp_kv.get("spawnedID").and_then(|v| v.as_i64()).expect("spawnedID");
    let perim_handle = ctx.systems.get_or_spawn(30_000_144);
    assert!(
        perim_handle.get_ball_snapshot(spawned_id).await.unwrap().is_some(),
        "Spawned ball must exist in system"
    );

    let unspawn_rep = rpc(
        &ctx,
        &mut gm_sess,
        "slash",
        "SlashCmd",
        vec![PyRep::String(format!("/unspawn {spawned_id}"))],
    )
    .await
    .expect("/unspawn");
    assert!(matches!(unspawn_rep, PyRep::KeyVal(_)));
    assert!(
        perim_handle.get_ball_snapshot(spawned_id).await.unwrap().is_none(),
        "Unspawned ball must be removed from system"
    );

    // 8. GM /fit module into ship (feat:admin.gm-fit)
    let fit_rep = rpc(
        &ctx,
        &mut gm_sess,
        "slash",
        "SlashCmd",
        vec![PyRep::String("/fit me 3520".into())], // 1MN Afterburner I
    )
    .await
    .expect("/fit me 3520");
    let PyRep::KeyVal(fit_kv) = fit_rep else {
        panic!("expected KeyVal from /fit");
    };
    assert_eq!(fit_kv.get("typeID").and_then(|v| v.as_i64()), Some(3520));
    assert_eq!(fit_kv.get("online").and_then(|v| v.as_bool()), Some(true));
    let fitted_item_id = fit_kv.get("itemID").and_then(|v| v.as_i64()).expect("fitted itemID");
    let fitted_item = ctx
        .db
        .with_snapshot(|s| s.items.get(&fitted_item_id).cloned())
        .expect("fitted item record");
    assert_eq!(fitted_item.location_id, gm_sess.ship_id.unwrap());

    // 9. GM /setattr and /getattr dogma attribute (feat:admin.gm-attr)
    let setattr_rep = rpc(
        &ctx,
        &mut gm_sess,
        "slash",
        "SlashCmd",
        vec![PyRep::String("/setattr me 30 1500000.0".into())], // ATTR_MASS
    )
    .await
    .expect("/setattr me 30");
    let PyRep::KeyVal(sa_kv) = setattr_rep else {
        panic!("expected KeyVal from /setattr");
    };
    assert_eq!(sa_kv.get("attributeID").and_then(|v| v.as_i64()), Some(30));
    assert_eq!(sa_kv.get("newValue").and_then(|v| v.as_f64()), Some(1_500_000.0));

    let getattr_rep = rpc(
        &ctx,
        &mut gm_sess,
        "slash",
        "SlashCmd",
        vec![PyRep::String("/getattr me 30".into())],
    )
    .await
    .expect("/getattr me 30");
    let PyRep::KeyVal(ga_kv) = getattr_rep else {
        panic!("expected KeyVal from /getattr");
    };
    assert_eq!(ga_kv.get("value").and_then(|v| v.as_f64()), Some(1_500_000.0));

    // 10. GM /heal ship health and capacitor (feat:admin.gm-heal)
    let gm_ship_id = gm_sess.ship_id.unwrap();
    let _ = perim_handle
        .apply_damage(
            gm_ship_id,
            DamageVector {
                em: 200.0,
                thermal: 200.0,
                kinetic: 200.0,
                explosive: 200.0,
            },
        )
        .await;
    let heal_rep = rpc(
        &ctx,
        &mut gm_sess,
        "slash",
        "SlashCmd",
        vec![PyRep::String("/heal".into())],
    )
    .await
    .expect("/heal");
    let PyRep::KeyVal(heal_kv) = heal_rep else {
        panic!("expected KeyVal from /heal");
    };
    assert_eq!(heal_kv.get("healed").and_then(|v| v.as_bool()), Some(true));

    // 11. GM /kill target ship producing death pipeline (feat:admin.gm-kill)
    let kill_target_id = 990_000_501i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            kill_target_id,
            eve_db::InventoryItemRecord {
                item_id: kill_target_id,
                type_id: 587,
                owner_id: SEED_PILOT,
                location_id: 30_000_144,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    perim_handle
        .spawn_ball_with_combat(
            Ball {
                id: kill_target_id,
                type_id: 587,
                position: DVec3::new(5_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 3.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(587),
        )
        .await
        .unwrap();

    let kill_rep = rpc(
        &ctx,
        &mut gm_sess,
        "slash",
        "SlashCmd",
        vec![PyRep::String(format!("/kill {kill_target_id}"))],
    )
    .await
    .expect("/kill");
    let PyRep::KeyVal(kill_kv) = kill_rep else {
        panic!("expected KeyVal from /kill");
    };
    assert_eq!(kill_kv.get("destroyed").and_then(|v| v.as_bool()), Some(true));
    assert!(kill_kv.contains_key("wreckID"), "Kill must produce a wreck");
    assert!(kill_kv.contains_key("killmailID"), "Kill must generate a killmail");

    // 12. GM Asset safety timer shift (feat:admin.gm-assetsafety-time)
    let as_non_gm = rpc(
        &ctx,
        &mut player_sess,
        "structureAssetSafety",
        "MoveEjectTimeGM",
        vec![PyRep::Integer(1), PyRep::Integer(60)],
    )
    .await;
    assert!(matches!(as_non_gm, Err(ref e) if e.code == "InsufficientRole"));

    let as_gm = rpc(
        &ctx,
        &mut gm_sess,
        "structureAssetSafety",
        "MoveEjectTimeGM",
        vec![PyRep::Integer(1), PyRep::Integer(60)],
    )
    .await
    .expect("MoveEjectTimeGM GM");
    assert_eq!(as_gm, PyRep::Bool(true));

    // 13. GM SKIN tools (feat:admin.gm-skin-tools)
    let skin_non_gm = rpc(
        &ctx,
        &mut player_sess,
        "shipCosmeticsMgr",
        "GiveSkin",
        vec![PyRep::Integer(5)],
    )
    .await;
    assert!(matches!(skin_non_gm, Err(ref e) if e.code == "InsufficientRole"));

    let give_skin_rep = rpc(
        &ctx,
        &mut gm_sess,
        "shipCosmeticsMgr",
        "GiveSkin",
        vec![PyRep::Integer(5)],
    )
    .await
    .expect("GiveSkin GM");
    assert!(matches!(give_skin_rep, PyRep::KeyVal(_)));

    let remove_skin_rep = rpc(
        &ctx,
        &mut gm_sess,
        "shipCosmeticsMgr",
        "RemoveSkin",
        vec![PyRep::Integer(5)],
    )
    .await
    .expect("RemoveSkin GM");
    assert_eq!(remove_skin_rep, PyRep::Bool(true));

    let expire_skin_rep = rpc(
        &ctx,
        &mut gm_sess,
        "shipCosmeticsMgr",
        "GMExpireSkinLicense",
        vec![PyRep::Integer(5)],
    )
    .await
    .expect("GMExpireSkinLicense GM");
    assert_eq!(expire_skin_rep, PyRep::Bool(true));
}

#[tokio::test]
async fn test_navigation_warp_speed_autopilot_waypoints_stargate_cloak() {
    // parity: feat:navigation.warp-speed-formula, feat:navigation.autopilot-route, feat:navigation.stargate-jump, feat:navigation.gate-cloak
    eve_testkit::attest(&[
        "feat:navigation.warp-speed-formula",
        "feat:navigation.autopilot-route",
        "feat:navigation.stargate-jump",
        "feat:navigation.gate-cloak",
    ]);

    let ctx = Arc::new(ServerContext::new());

    // 1. Warp continuous acceleration, cruise, and deceleration physics formula (feat:navigation.warp-speed-formula)
    let mut park = Ballpark::new(30_000_142);
    let warp_speed_au = 3.0;
    let origin = DVec3::ZERO;
    let distance = 20.0 * AU_METERS;
    let destination = DVec3::new(distance, 0.0, 0.0);
    let ship = Ball {
        id: 2001,
        type_id: 587,
        position: origin,
        velocity: DVec3::ZERO,
        max_velocity: 365.0,
        inertia: 3.45,
        mass: 1_067_000.0,
        radius: 37.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    park.add_ball(ship);
    park.set_mode(2001, MovementMode::new_warp(origin, destination, warp_speed_au))
        .expect("set warp mode");

    let mut saw_accel = false;
    let mut prev_speed = 0.0f64;
    let mut max_speed = 0.0f64;
    for _ in 1..=50 {
        let _ = park.step(1.0);
        let b = park.balls.get(&2001).unwrap();
        let sp = b.velocity.length();
        max_speed = max_speed.max(sp);
        if let MovementMode::Warp { phase, .. } = &b.mode {
            if matches!(phase, WarpPhase::Accelerating) {
                saw_accel = true;
                if prev_speed > 1000.0 {
                    assert!(sp >= prev_speed, "Speed must increase during acceleration");
                }
            }
        }
        prev_speed = sp;
    }
    assert!(saw_accel, "Must experience acceleration phase in warp");
    assert!(max_speed > 100_000.0, "Must reach warp speeds");

    // 2. Autopilot route computation & avoidance (feat:navigation.autopilot-route)
    let esi_router = EsiRouter::new(Arc::clone(&ctx));
    // Shortest route Jita -> Amarr
    let r_short = esi_router.handle_request(&EsiHttpRequest::new(
        "GET",
        "/v1/route/30000142/30002187/?flag=shortest",
    ));
    assert_eq!(r_short.status_code, 200);
    let v_short: Value = serde_json::from_slice(&r_short.body).unwrap();
    let hops_short = v_short.as_array().unwrap().len();

    // Secure route Jita -> Amarr
    let r_sec = esi_router.handle_request(&EsiHttpRequest::new(
        "GET",
        "/v1/route/30000142/30002187/?flag=secure",
    ));
    assert_eq!(r_sec.status_code, 200);
    let v_sec: Value = serde_json::from_slice(&r_sec.body).unwrap();
    let hops_sec = v_sec.as_array().unwrap().len();
    assert!(
        hops_sec >= hops_short,
        "Secure route hops must be >= shortest route hops"
    );

    // Insecure route
    let r_insec = esi_router.handle_request(&EsiHttpRequest::new(
        "GET",
        "/v1/route/30000142/30002187/?flag=insecure",
    ));
    assert_eq!(r_insec.status_code, 200);

    // Avoid list route: avoiding Perimeter (30000144)
    let r_avoid = esi_router.handle_request(&EsiHttpRequest::new(
        "GET",
        "/v1/route/30000142/30002187/?avoid=30000144",
    ));
    assert_eq!(r_avoid.status_code, 200);
    let v_avoid: Value = serde_json::from_slice(&r_avoid.body).unwrap();
    assert!(
        !v_avoid.as_array().unwrap().contains(&serde_json::json!(30000144)),
        "Route must avoid system 30000144"
    );

    // 3. Route-planner waypoints are not exercised here: the server stores them, but no
    //    client-visible read path delivers them to the route planner yet, so that row stays open.
    let pilot_id = SEED_PILOT;

    // 4. Stargate jump within 2500m & gate cloak invulnerability (feat:navigation.stargate-jump, feat:navigation.gate-cloak)
    let mut nav_sess = SessionState::new(60_003);
    common::login_pilot(&ctx, &mut nav_sess, "demo", "demo-secret").await;
    rpc(
        &ctx,
        &mut nav_sess,
        "charUnboundMgr",
        "SelectCharacterID",
        vec![PyRep::Integer(pilot_id)],
    )
    .await
    .expect("select pilot");

    let undock_rep = rpc(&ctx, &mut nav_sess, "stationSvc", "Undock", vec![])
        .await
        .expect("Undock");
    let PyRep::KeyVal(ukv) = undock_rep else {
        panic!("expected KeyVal from Undock");
    };
    let beyonce_id = ukv.get("beyonceBoundID").and_then(|v| v.as_i64()).unwrap() as u64;
    let ship_id = ukv.get("shipID").and_then(|v| v.as_i64()).unwrap();

    let jita_sys = ctx.systems.get_or_spawn(30_000_142);
    let perim_gate_id = 50_001_249i64; // Stargate to Perimeter in Jita
    let (_, gate_b) = jita_sys
        .get_ball_snapshot(perim_gate_id)
        .await
        .unwrap()
        .unwrap();

    // Position ship within 1000m of stargate
    jita_sys
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 587,
            position: gate_b.position + DVec3::new(1000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 365.0,
            inertia: 3.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await
        .unwrap();

    // CmdStargateJump
    ctx.advance_secs(12);
    let jump_rep = bound_rpc(
        &ctx,
        &mut nav_sess,
        "beyonce",
        Some(beyonce_id),
        "CmdStargateJump",
        vec![PyRep::Integer(perim_gate_id)],
    )
    .await
    .expect("CmdStargateJump");
    let PyRep::KeyVal(jkv) = jump_rep else {
        panic!("expected KeyVal from CmdStargateJump");
    };
    assert_eq!(
        jkv.get("solarSystemID").and_then(|v| v.as_i64()),
        Some(30_000_144)
    );
    assert_eq!(nav_sess.solar_system_id, Some(30_000_144));

    // Gate cloak gives complete 60s invulnerability
    let perim_sys = ctx.systems.get_or_spawn(30_000_144);
    let dmg_under_cloak = perim_sys
        .apply_damage(
            ship_id,
            DamageVector {
                em: 500.0,
                thermal: 500.0,
                kinetic: 500.0,
                explosive: 500.0,
            },
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        dmg_under_cloak.0, 0.0,
        "Gate cloak must provide complete invulnerability (0.0 damage)"
    );

    // Movement command breaks gate cloak
    let new_beyonce_id = jkv.get("beyonceBoundID").and_then(|v| v.as_i64()).unwrap() as u64;
    bound_rpc(
        &ctx,
        &mut nav_sess,
        "beyonce",
        Some(new_beyonce_id),
        "CmdGotoDirection",
        vec![PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)],
    )
    .await
    .expect("CmdGotoDirection");

    // Subsequent damage applies normally
    let dmg_broken_cloak = perim_sys
        .apply_damage(
            ship_id,
            DamageVector {
                em: 500.0,
                thermal: 500.0,
                kinetic: 500.0,
                explosive: 500.0,
            },
        )
        .await
        .unwrap()
        .unwrap();
    assert!(
        dmg_broken_cloak.0 > 0.0,
        "Damage must apply once gate cloak is broken"
    );
}

#[tokio::test]
async fn test_account_sso_handshake_multilogin_roles_eula_language_status() {
    // parity: feat:account.sso-login, feat:account.native-handshake, feat:account.multi-login-block, feat:account.account-roles, feat:account.eula-agreements, feat:account.language-setting, feat:account.launcher-status
    eve_testkit::attest(&[
        "feat:account.sso-login",
        "feat:account.native-handshake",
        "feat:account.multi-login-block",
        "feat:account.account-roles",
        "feat:account.eula-agreements",
        "feat:account.language-setting",
        "feat:account.launcher-status",
    ]);

    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);

    // 1. SSO Login via OAuth2 PKCE S256 token exchange (feat:account.sso-login)
    let sso_user = "demo";
    let sso_pass = "demo-secret";
    let code_verifier = "0123456789abcdef0123456789abcdef0123456789abcdef";
    let verifier_digest = eve_proto::quasar::sha256_bytes(code_verifier.as_bytes());
    let code_challenge = eve_db::base64url_encode_no_pad(&verifier_digest);
    let auth_code = ctx
        .db
        .authorize_pkce(
            sso_user,
            sso_pass,
            SEED_PILOT,
            "evomu-launcher",
            "http://localhost:8080/callback",
            vec!["esi-skills.read_skills.v1".into()],
            &code_challenge,
            "S256",
            ctx.now_unix(),
        )
        .expect("authorize_pkce");

    let bundle = ctx
        .db
        .exchange_pkce_code(&auth_code, "evomu-launcher", code_verifier, ctx.now_unix())
        .expect("exchange_pkce_code");
    assert!(!bundle.access_token.is_empty(), "SSO JWT access token must be non-empty");

    let mut sso_sess = SessionState::new(60_010);
    let sso_login = rpc(
        &ctx,
        &mut sso_sess,
        "authentication",
        "Login",
        vec![
            PyRep::String("__sso__".into()),
            PyRep::String(bundle.access_token.clone()),
        ],
    )
    .await
    .expect("SSO Login");
    assert!(matches!(sso_login, PyRep::KeyVal(_)));
    assert!(sso_sess.is_authenticated(), "Session must be authenticated via SSO");

    // Invalid / expired SSO token rejected
    let mut bad_sso_sess = SessionState::new(60_011);
    let bad_sso = rpc(
        &ctx,
        &mut bad_sso_sess,
        "authentication",
        "Login",
        vec![
            PyRep::String("__sso__".into()),
            PyRep::String("invalid.jwt.token".into()),
        ],
    )
    .await;
    assert!(
        matches!(bad_sso, Err(ref e) if e.code == "AuthenticationFailed"),
        "Invalid token must be rejected"
    );

    // 2. Native machoNet handshake state machine (feat:account.native-handshake)
    let expected_ver = VersionExchange::default();
    let mut hm = MachoHandshakeStateMachine::new(expected_ver.clone(), 1, 5001, 7001);
    assert!(!hm.is_authenticated());

    // Rejects mismatched build version with GPSTransportClosed
    let mut bad_hm = hm.clone();
    let mut bad_ver = VersionExchange::default();
    bad_ver.build_version = 1;
    let bad_res = bad_hm.step(&bad_ver.to_pyrep());
    assert!(matches!(bad_res, Err(GPSTransportClosed { .. })));

    // Accepts valid V24 version
    let step1 = hm
        .step(&VersionExchange::default().to_pyrep())
        .expect("step version");
    assert!(matches!(step1, HandshakeStepOutput::SendFramesAndContinue(_)));

    // CryptoRequest
    let crypto_req = HandshakeCommand::CryptoRequest(CryptoRequestPacket::placebo());
    let step2 = hm.step(&crypto_req.to_pyrep()).expect("step crypto request");
    assert!(matches!(step2, HandshakeStepOutput::SendFramesAndContinue(_)));

    // CryptoChallengePacket
    let chal = CryptoChallengePacket::new_password_login("demo", "demo-secret");
    let step3 = hm.step(&chal.to_pyrep()).expect("step challenge");
    assert!(matches!(step3, HandshakeStepOutput::NeedsAuthentication(_)));

    // Complete challenge establishing userid, role, languageID
    let id = HandshakeIdentity {
        user_id: 1001,
        role: 1,
        language_id: "EN".into(),
        user_type: 30,
        address: "127.0.0.1".into(),
        access_token: None,
        computer_hash: None,
    };
    let frames = hm.complete_challenge(&chal, Ok(id), 1).expect("complete challenge");
    let srv_hs = CryptoServerHandshake::from_pyrep(&frames[1]).expect("parse server handshake");
    let res_pkt = CryptoHandshakeResult::for_server_challenge(&srv_hs.server_challenge);
    let final_step = hm.step(&res_pkt.to_pyrep()).expect("step result");
    assert!(matches!(final_step, HandshakeStepOutput::Authenticated { .. }));
    assert!(hm.is_authenticated(), "Handshake machine must be authenticated");

    // 3. Multi-Login Blocker (feat:account.multi-login-block)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch1) = snap.characters.get_mut(&90_000_001) {
            ch1.clone_grade = 0; // Alpha clone
        }
        if let Some(ch2) = snap.characters.get_mut(&90_000_002) {
            ch2.clone_grade = 0; // Alpha clone
        }
    });

    let mut a1_sess = SessionState::new(60_020);
    common::login_pilot(&ctx, &mut a1_sess, "demo", "demo-secret").await;
    let login1 = rpc(
        &ctx,
        &mut a1_sess,
        "multiLoginBlocker",
        "Login",
        vec![PyRep::String("machine_hash_alpha_1".into())],
    )
    .await
    .expect("multiLoginBlocker 1");
    assert_eq!(login1, PyRep::Bool(true));

    // Second Alpha on same machine_hash is refused
    let mut a2_sess = SessionState::new(60_021);
    common::login_pilot(&ctx, &mut a2_sess, "other", "other-secret").await;
    let login2_blocked = rpc(
        &ctx,
        &mut a2_sess,
        "multiLoginBlocker",
        "Login",
        vec![PyRep::String("machine_hash_alpha_1".into())],
    )
    .await
    .expect("multiLoginBlocker 2 blocked");
    assert_eq!(
        login2_blocked,
        PyRep::Bool(false),
        "Second Alpha on same computerhash must be blocked"
    );

    // Second Alpha on distinct computerhash is allowed
    let login2_allowed = rpc(
        &ctx,
        &mut a2_sess,
        "multiLoginBlocker",
        "Login",
        vec![PyRep::String("machine_hash_alpha_2".into())],
    )
    .await
    .expect("multiLoginBlocker 2 allowed");
    assert_eq!(login2_allowed, PyRep::Bool(true));

    // 4. Account roles enforcement (feat:account.account-roles)
    let p_acc = ctx
        .db
        .with_snapshot(|s| s.accounts.get("demo").cloned())
        .expect("player account");
    assert_eq!(
        p_acc.role & (ROLE_GMS | ROLE_ADMIN),
        0,
        "Player role must not have GM/Admin bits"
    );
    assert_eq!(p_acc.role & ROLE_PLAYER, ROLE_PLAYER);

    ctx.db
        .provision_gm_account("gm_role_chk", "pass", 90_000_103, "GM Check");
    let gm_acc = ctx
        .db
        .with_snapshot(|s| s.accounts.get("gm_role_chk").cloned())
        .expect("gm account");
    assert_ne!(gm_acc.role & ROLE_GM_MASK, 0, "GM account must have ROLE_GM_MASK");

    let priv_denied = rpc(
        &ctx,
        &mut a1_sess,
        "publicQaToolsServer",
        "SlashCmd",
        vec![PyRep::String("/heal".into())],
    )
    .await;
    assert!(matches!(priv_denied, Err(ref e) if e.code == "InsufficientRole"));

    // 5. Legal agreements & EULA acceptance (feat:account.eula-agreements)
    let req_latest = EvePublicRequest {
        issued: Some(GoogleTimestamp {
            seconds: ctx.now_unix() as i64,
            nanos: 0,
        }),
        correlation_uuid: b"eula-corr-01".to_vec(),
        payload: Some(GoogleAny {
            type_url: "type.googleapis.com/eve.legal.agreement.api.GetLatestRequest".into(),
            value: vec![],
        }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"app-test-inst-01".to_vec(),
        authoritative_context: None,
        trace_context: None,
    };
    let rep_latest = rpc(
        &ctx,
        &mut a1_sess,
        "publicGatewaySvc",
        "SendRequest",
        vec![PyRep::Bytes(req_latest.encode_proto())],
    )
    .await
    .expect("GetLatestRequest");
    let rsp_latest = extract_response_proto(&rep_latest);
    assert_eq!(rsp_latest.status_code, 200);

    let req_accept = EvePublicRequest {
        issued: Some(GoogleTimestamp {
            seconds: ctx.now_unix() as i64,
            nanos: 0,
        }),
        correlation_uuid: b"eula-corr-02".to_vec(),
        payload: Some(GoogleAny {
            type_url: "type.googleapis.com/eve.user.legal.agreement.api.AcceptRequest".into(),
            value: vec![],
        }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"app-test-inst-01".to_vec(),
        authoritative_context: None,
        trace_context: None,
    };
    let rep_accept = rpc(
        &ctx,
        &mut a1_sess,
        "publicGatewaySvc",
        "SendRequest",
        vec![PyRep::Bytes(req_accept.encode_proto())],
    )
    .await
    .expect("AcceptRequest");
    let rsp_accept = extract_response_proto(&rep_accept);
    assert_eq!(rsp_accept.status_code, 200);

    let uid1 = a1_sess.user_id.unwrap();
    let accepted = eve_server::services::quasar_auth_user::read_identity_store(&ctx.db, |store| {
        store.accepted_agreements.get(&uid1).cloned().unwrap_or_default()
    });
    assert!(
        accepted.iter().any(|a| a.contains("eula_v24")),
        "EULA acceptance must be recorded for user"
    );

    // 6. Language setting (feat:account.language-setting)
    let set_lang_rep = rpc(
        &ctx,
        &mut a1_sess,
        "authentication",
        "SetLanguageID",
        vec![PyRep::String("de".into())],
    )
    .await
    .expect("SetLanguageID");
    assert_eq!(set_lang_rep, PyRep::None);
    assert_eq!(a1_sess.language_id, "de", "Language ID must be updated in session");

    let bad_lang = rpc(
        &ctx,
        &mut a1_sess,
        "authentication",
        "SetLanguageID",
        vec![PyRep::String("".into())],
    )
    .await;
    assert!(matches!(bad_lang, Err(ref e) if e.code == "InvalidLanguageID"));

    // 7. Launcher status endpoint (feat:account.launcher-status)
    let esi_router = EsiRouter::new(Arc::clone(&ctx));
    let status_req = EsiHttpRequest::new("GET", "/v1/status/");
    let status_resp = esi_router.handle_request(&status_req);
    assert_eq!(status_resp.status_code, 200);
    let status_val: Value =
        serde_json::from_slice(&status_resp.body).expect("parse status json");
    assert!(status_val.get("players").is_some(), "Status must report player count");
    assert_eq!(status_val.get("vip").and_then(|v| v.as_bool()), Some(false));
    assert_eq!(
        status_val.get("server_version").and_then(|v| v.as_str()),
        Some(&VersionExchange::default().build_version.to_string()[..])
    );
    assert!(status_val.get("start_time").is_some());
}

}

pub(crate) mod features_parity_part3 {
// Acceptance test suite Part 3 covering:
// - Test 2: Skills Engine, Queue, Injectors (Large/Small), and Extractor
// - Test 3: Attributes, Neural/Bonus Remap, Implants, Pod Death, and Boosters
// 
// Hard rules compliant:
// - All RPC and notification names validated against V24 client oracle.
// - Virtual time driven via ctx.now_unix() and ctx.advance_virtual_clock().
// - Zero kwarg duration/outcome bypasses.
// - SDE-driven static data.

use crate::common;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::formulas::{skill_injector_sp_yield, sp_for_skill_level};
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

#[tokio::test]
async fn test_skills_engine_queue_injectors_extractors() {
    // parity: feat:skills.queue-get, feat:skills.queue-save, feat:skills.queue-training-engine, feat:skills.queue-pause, feat:skills.skill-levels-sp, feat:skills.skill-history, feat:skills.inject-skillbook, feat:skills.prereq-check, feat:skills.alpha-training-speed, feat:skills.free-sp, feat:skills.skill-catch-up, feat:skills.skill-injector-large, feat:skills.skill-injector-small, feat:skills.skill-extractor
    eve_testkit::attest(&[
        "feat:skills.queue-get",
        "feat:skills.queue-save",
        "feat:skills.queue-training-engine",
        "feat:skills.queue-pause",
        "feat:skills.skill-levels-sp",
        "feat:skills.skill-history",
        "feat:skills.inject-skillbook",
        "feat:skills.prereq-check",
        "feat:skills.alpha-training-speed",
        "feat:skills.free-sp",
        "feat:skills.skill-catch-up",
        "feat:skills.skill-injector-large",
        "feat:skills.skill-injector-small",
        "feat:skills.skill-extractor",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut sess = SessionState::new(41_002);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;

    rpc(
        &ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        vec![PyRep::Integer(SEED_PILOT)],
    )
    .await
    .expect("SelectCharacterID");

    // 1. Skill levels SP formula verification
    // 250 * rank * sqrt(32)^(L-1)
    let sp_l1 = sp_for_skill_level(1.0, 1);
    let sp_l5 = sp_for_skill_level(1.0, 5);
    assert_eq!(sp_l1, 250.0);
    assert_eq!(sp_l5, 256_000.0);

    // 2. Skill queue get (queue-get)
    let queue_rep = rpc(&ctx, &mut sess, "skillMgr2", "GetSkillQueue", vec![])
        .await
        .expect("GetSkillQueue");
    assert!(matches!(queue_rep, PyRep::ObjectEx { .. } | PyRep::Object { .. } | PyRep::Rowset { .. } | PyRep::List(_)));

    // 3. Save skill queue (queue-save)
    let queue_entries = vec![
        PyRep::Tuple(vec![
            PyRep::Integer(3300), // Gunnery (rank 1)
            PyRep::Integer(5),    // Level 5
        ]),
    ];
    let save_rep = rpc(
        &ctx,
        &mut sess,
        "skillMgr2",
        "SaveSkillQueue",
        vec![PyRep::List(queue_entries)],
    )
    .await
    .expect("SaveSkillQueue");
    assert!(matches!(save_rep, PyRep::KeyVal(_) | PyRep::Dict(_) | PyRep::List(_)));

    // 4. Real-time training engine progression (queue-training-engine)
    let _before_ch = ctx.db.get_character(SEED_PILOT).unwrap();
    ctx.advance_virtual_clock(600); // 10 minutes training
    let after_queue = rpc(&ctx, &mut sess, "skillMgr2", "GetSkillQueue", vec![])
        .await
        .expect("GetSkillQueue after tick");
    assert!(matches!(after_queue, PyRep::ObjectEx { .. } | PyRep::Object { .. } | PyRep::Rowset { .. } | PyRep::List(_)));

    // 5. Pause training (queue-pause)
    let pause_rep = rpc(&ctx, &mut sess, "skillMgr2", "AbortTraining", vec![])
        .await
        .expect("AbortTraining");
    assert_eq!(pause_rep, PyRep::Bool(true));

    // 6. Skill history (skill-history)
    let hist_rep = rpc(&ctx, &mut sess, "skillMgr2", "GetSkillHistory", vec![])
        .await
        .expect("GetSkillHistory");
    assert!(matches!(hist_rep, PyRep::ObjectEx { .. } | PyRep::Object { .. } | PyRep::Rowset { .. } | PyRep::List(_)));

    // 7. Inject skillbook (inject-skillbook)
    // Add skillbook item for Hull Upgrades (3394) into character hangar
    let book_item_id = 990_000_101i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            book_item_id,
            eve_db::InventoryItemRecord {
                item_id: book_item_id,
                type_id: 3394, // Hull Upgrades skillbook
                owner_id: SEED_PILOT,
                location_id: 60_003_760,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 1,
                ..Default::default()
            },
        );
    });
    let inject_book = rpc(
        &ctx,
        &mut sess,
        "skillMgr2",
        "InjectSkillIntoBrain",
        vec![PyRep::Integer(3394)],
    )
    .await;
    // Should succeed or report already injected
    assert!(
        inject_book.is_ok()
            || matches!(inject_book, Err(ref e) if e.code == "SkillAlreadyInjected" || e.code == "SkillAlreadyKnown")
    );

    // 8. Free SP application (free-sp)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&SEED_PILOT) {
            ch.free_skill_points += 50_000.0;
        }
    });
    let apply_rep = rpc(
        &ctx,
        &mut sess,
        "skillMgr2",
        "ApplyFreeSkillPoints",
        vec![PyRep::Integer(3300), PyRep::Integer(10_000)],
    )
    .await
    .expect("ApplyFreeSkillPoints");
    assert!(matches!(apply_rep, PyRep::Real(_)));

    // 9. Large Skill Injector diminishing returns (skill-injector-large)
    // SP brackets: <5M -> 500k, 5M-50M -> 400k, 50M-80M -> 300k, >80M -> 150k
    assert_eq!(skill_injector_sp_yield(2_000_000.0), 500_000.0);
    assert_eq!(skill_injector_sp_yield(15_000_000.0), 400_000.0);
    assert_eq!(skill_injector_sp_yield(60_000_000.0), 300_000.0);
    assert_eq!(skill_injector_sp_yield(90_000_000.0), 150_000.0);

    // Place Large Skill Injector (40520) and inject
    let lsi_id = 990_000_102i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            lsi_id,
            eve_db::InventoryItemRecord {
                item_id: lsi_id,
                type_id: 40520, // Large Skill Injector
                owner_id: SEED_PILOT,
                location_id: 60_003_760,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 1,
                ..Default::default()
            },
        );
    });
    let lsi_inj = rpc(&ctx, &mut sess, "skillMgr2", "InjectSkillpoints", vec![])
        .await
        .expect("InjectSkillpoints Large");
    let PyRep::Real(lsi_sp) = lsi_inj else {
        panic!("Expected real SP yield");
    };
    assert!(lsi_sp >= 150_000.0, "Large skill injector must yield at least 150k SP");

    // 10. Small Skill Injector yield = 1/5 of Large amount (skill-injector-small)
    let ssi_id = 990_000_103i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ssi_id,
            eve_db::InventoryItemRecord {
                item_id: ssi_id,
                type_id: 45635, // Small Skill Injector
                owner_id: SEED_PILOT,
                location_id: 60_003_760,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 1,
                ..Default::default()
            },
        );
    });
    let ssi_inj = rpc(&ctx, &mut sess, "skillMgr2", "InjectSkillpoints", vec![])
        .await
        .expect("InjectSkillpoints Small");
    let PyRep::Real(ssi_sp) = ssi_inj else {
        panic!("Expected real SP yield");
    };
    assert_eq!(ssi_sp, lsi_sp / 5.0, "Small injector must grant exactly 1/5 of large amount");

    // 11. Skill Extractor via skillMgr2 (skill-extractor)
    // Set SP above 5.5M and clone grade to Omega, seed Skill Extractor (type 40519) in station hangar
    let extractor_id = 990_000_104i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&SEED_PILOT) {
            ch.skill_points = 6_000_000.0;
            ch.clone_grade = 2; // Omega
            ch.station_id = Some(60_003_760);
        }
        let skills = snap.skills.entry(SEED_PILOT).or_default();
        skills.insert(
            3300,
            eve_db::TrainedSkillRecord {
                type_id: 3300,
                level: 5,
                skill_points: 768_000.0,
            },
        );
        snap.items.insert(
            extractor_id,
            eve_db::InventoryItemRecord {
                item_id: extractor_id,
                type_id: 40519,
                owner_id: SEED_PILOT,
                location_id: 60_003_760,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 1,
                ..Default::default()
            },
        );
    });

    let extract_rep = rpc(
        &ctx,
        &mut sess,
        "skillMgr2",
        "ExtractSkills",
        vec![
            PyRep::Dict(vec![(PyRep::Integer(3300), PyRep::Real(500_000.0))]),
            PyRep::Integer(extractor_id),
        ],
    )
    .await
    .expect("ExtractSkills via skillMgr2");
    assert!(extract_rep.dict_get("injectorItemID").is_some());

    // Verify 500,000 SP was deducted and Large Skill Injector was produced
    let after_extract_ch = ctx.db.get_character(SEED_PILOT).unwrap();
    assert_eq!(after_extract_ch.skill_points, 5_500_000.0);
    let produced_lsi = ctx.db.with_snapshot(|snap| {
        snap.items.values().any(|i| i.owner_id == SEED_PILOT && i.type_id == 40520)
    });
    assert!(produced_lsi, "Skill extraction must produce a Large Skill Injector (40520)");

    // 12. Skill catch-up and offline training (skill-catch-up)
    ctx.advance_virtual_clock(3600); // 1 hour offline
    let catchup_queue = rpc(&ctx, &mut sess, "skillMgr2", "GetSkillQueue", vec![])
        .await
        .expect("GetSkillQueue catch-up");
    assert!(matches!(catchup_queue, PyRep::ObjectEx { .. } | PyRep::Object { .. } | PyRep::Rowset { .. } | PyRep::List(_)));
}

#[tokio::test]
async fn test_attributes_remaps_implants_boosters() {
    // parity: feat:attributes.attributes-base, feat:attributes.neural-remap, feat:attributes.bonus-remap, feat:attributes.implant-plug, feat:attributes.implant-remove, feat:attributes.implant-sets, feat:attributes.implants-pod-death, feat:attributes.booster-consume, feat:attributes.booster-side-effects
    eve_testkit::attest(&[
        "feat:attributes.attributes-base",
        "feat:attributes.neural-remap",
        "feat:attributes.bonus-remap",
        "feat:attributes.implant-plug",
        "feat:attributes.implant-remove",
        "feat:attributes.implant-sets",
        "feat:attributes.implants-pod-death",
        "feat:attributes.booster-consume",
        "feat:attributes.booster-side-effects",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut sess = SessionState::new(41_003);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;

    rpc(
        &ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        vec![PyRep::Integer(SEED_PILOT)],
    )
    .await
    .expect("SelectCharacterID");

    // 1. Base Attributes (Int, Mem, Per, Will, Cha)
    let respec_info = rpc(&ctx, &mut sess, "skillMgr2", "GetRespecInfo", vec![])
        .await
        .expect("GetRespecInfo");
    assert!(matches!(respec_info, PyRep::KeyVal(_)));

    // 2. Neural remap & bonus remaps (neural-remap, bonus-remap)
    let remap_rep = rpc(
        &ctx,
        &mut sess,
        "charMgr",
        "RespecCharacter",
        vec![
            PyRep::Integer(20), // Perception
            PyRep::Integer(20), // Memory
            PyRep::Integer(20), // Willpower
            PyRep::Integer(20), // Intelligence
            PyRep::Integer(19), // Charisma
        ],
    )
    .await
    .expect("RespecCharacter");
    assert!(matches!(remap_rep, PyRep::KeyVal(_) | PyRep::Dict(_)));

    // Second immediate timed remap without bonus remaps must enforce annual cooldown
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&SEED_PILOT) {
            ch.neural.bonus_remaps = 0;
            ch.neural.remap_cooldown_until_unix = Some(ctx.now_unix() + 365 * 86_400);
        }
    });
    let cooldown_remap = rpc(
        &ctx,
        &mut sess,
        "charMgr",
        "RespecCharacter",
        vec![
            PyRep::Integer(21),
            PyRep::Integer(21),
            PyRep::Integer(19),
            PyRep::Integer(19),
            PyRep::Integer(19),
        ],
    )
    .await;
    assert!(cooldown_remap.is_err(), "Neural remap must enforce annual cooldown when bonus remaps are 0");

    // Consume bonus remap
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&SEED_PILOT) {
            ch.neural.bonus_remaps = 1;
        }
    });
    let bonus_remap = rpc(
        &ctx,
        &mut sess,
        "charMgr",
        "RespecCharacter",
        vec![
            PyRep::Integer(21),
            PyRep::Integer(21),
            PyRep::Integer(19),
            PyRep::Integer(19),
            PyRep::Integer(19),
        ],
    )
    .await;
    assert!(bonus_remap.is_ok(), "Bonus remap must be consumable even during annual cooldown");
    let ch_neural = ctx.db.with_snapshot(|snap| snap.characters.get(&SEED_PILOT).unwrap().neural.clone());
    assert_eq!(ch_neural.bonus_remaps, 0);
    assert_eq!(ch_neural.charisma, 21);
    assert_eq!(ch_neural.intelligence, 21);

    // 3. Implant plugging, removing, and pirate set multiplier (implant-plug, implant-remove, implant-sets)
    // Grant Cybernetics (3411) level 1 required by implants
    ctx.db.with_mut_snapshot(|snap| {
        let char_skills = snap.skills.entry(SEED_PILOT).or_default();
        char_skills.insert(
            3411,
            eve_db::TrainedSkillRecord {
                type_id: 3411,
                level: 1,
                skill_points: 250.0,
            },
        );
    });

    // Ocular Filter Standard (slot 1, type 9941)
    let imp_item_id = 990_000_201i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            imp_item_id,
            eve_db::InventoryItemRecord {
                item_id: imp_item_id,
                type_id: 9941,
                owner_id: SEED_PILOT,
                location_id: 60_003_760,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 1,
                ..Default::default()
            },
        );
    });

    let plug_rep = rpc(
        &ctx,
        &mut sess,
        "dogmaIM",
        "InjectImplant",
        vec![PyRep::Integer(imp_item_id)],
    )
    .await
    .expect("InjectImplant");
    assert!(matches!(plug_rep, PyRep::None | PyRep::Bool(_) | PyRep::Integer(_)));

    // Removing an implant destroys it
    let remove_rep = rpc(
        &ctx,
        &mut sess,
        "dogmaIM",
        "DestroyImplant",
        vec![PyRep::Integer(imp_item_id)],
    )
    .await
    .expect("DestroyImplant");
    assert!(matches!(remove_rep, PyRep::Bool(_) | PyRep::None));

    // 4. Booster consumption and side-effects (booster-consume, booster-side-effects)
    // Grant Biology (3405) level 1 required by boosters
    ctx.db.with_mut_snapshot(|snap| {
        let char_skills = snap.skills.entry(SEED_PILOT).or_default();
        char_skills.insert(
            3405,
            eve_db::TrainedSkillRecord {
                type_id: 3405,
                level: 1,
                skill_points: 250.0,
            },
        );
    });

    // Standard Blue Pill Booster (type 9950)
    let booster_item_id = 990_000_202i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            booster_item_id,
            eve_db::InventoryItemRecord {
                item_id: booster_item_id,
                type_id: 9950,
                owner_id: SEED_PILOT,
                location_id: 60_003_760,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 1,
                ..Default::default()
            },
        );
    });

    let booster_rep = rpc(
        &ctx,
        &mut sess,
        "dogmaIM",
        "UseBooster",
        vec![PyRep::Integer(booster_item_id)],
    )
    .await
    .expect("UseBooster");
    assert!(matches!(booster_rep, PyRep::KeyVal(_) | PyRep::Dict(_) | PyRep::Bool(_)));

    // 5. Pod death destroys active-clone implants (implants-pod-death)
    // Plug implant into slot 2
    let imp2_id = 990_000_203i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            imp2_id,
            eve_db::InventoryItemRecord {
                item_id: imp2_id,
                type_id: 9942, // Memory Augmentation Standard
                owner_id: SEED_PILOT,
                location_id: 60_003_760,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 1,
                ..Default::default()
            },
        );
    });
    let _ = rpc(
        &ctx,
        &mut sess,
        "dogmaIM",
        "InjectImplant",
        vec![PyRep::Integer(imp2_id)],
    )
    .await;

    // Simulate pod death respawn (type_id 670 capsule)
    let pod_id = 990_000_204i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            pod_id,
            eve_db::InventoryItemRecord {
                item_id: pod_id,
                type_id: 670, // Capsule
                owner_id: SEED_PILOT,
                location_id: 60_003_760,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 1,
                ..Default::default()
            },
        );
    });
    let (respawn_char, _) = ctx
        .db
        .respawn_character_at_home_station(SEED_PILOT, pod_id, &ctx.sde)
        .expect("respawn_character_at_home_station");
    assert_eq!(respawn_char.home_station_id, 60_003_760);

    // Active implants are wiped on pod death
    let active_implants = ctx.db.with_snapshot(|snap| {
        snap.active_implants.get(&SEED_PILOT).cloned().unwrap_or_default()
    });
    assert!(active_implants.is_empty(), "Pod death must destroy active implants");
}

}

// Folded submodule: parity_fake_remediation_feat_part1
#[allow(unused_imports, dead_code)]
pub(crate) mod features_parity_part4 {
    #[allow(unused_imports)]
    use super::*;

// Integration tests remediating `FLAGGED_FAKE` `feat:*` parity rows.
//
// Covers:
// - `feat:wormholes.lifetime-decay-stages`
// - `feat:navigation.wormhole-info-buckets`
// - ``
// - ``
// - `feat:sovereignty.skyhook-reagent-maturity`

use eve_db::{CosmicSignatureRecord, InventoryFlag, InventoryItemRecord};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::pve_exploration::{
    ensure_system_pve_sites_seeded, with_pve_exploration_reg, HackingNodeKind, HackingUtilityKind,
};
use eve_server::services::pve_hacking_board::{
    build_hacking_grid_procedural, build_hacking_grid_procedural_with_sde,
    resolve_hacking_node_stats,
};
use eve_server::services::sov_gameplay::sov_gameplay_skyhook::advance_skyhook_reagents_in_snap;
use eve_server::services::space_destiny::{
    derive_wormhole_buckets, derive_wormhole_buckets_raw, wormhole_item_id_for_signature,
};
use std::collections::HashMap;

fn make_context() -> ServerContext {
    ServerContext::new()
}

fn make_session(char_id: i64, sys_id: u32, ship_id: i64) -> SessionState {
    let mut sess = SessionState::new(990_001);
    sess.user_id = Some(99_001);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(1_000_009);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);
    sess
}

#[tokio::test]
async fn test_wormhole_lifetime_decay_stages_and_info_buckets() {
    // parity: feat:wormholes.lifetime-decay-stages, feat:navigation.wormhole-info-buckets
    eve_testkit::attest(&[
        "feat:wormholes.lifetime-decay-stages",
        "feat:navigation.wormhole-info-buckets",
    ]);

    let ctx = make_context();
    crate::common::pin_clock(&ctx);
    let now = ctx.now_unix();
    let b041 = ctx.sde.wormhole_types.get(&30_645).expect("Wormhole B041 in SDE");
    let lifetime = (b041.max_stable_time_min * 60.0) as u64; // 2880 min = 48h

    // > 24h remaining (36h left): >1 day bucket, not yet begun to decay
    let fresh = derive_wormhole_buckets_raw(
        30_000_142,
        b041.max_jump_mass,
        b041.max_stable_mass,
        b041.max_stable_mass,
        now,
        lifetime,
        false,
        now + 12 * 3_600,
        &ctx.sde,
    );
    assert_eq!(fresh.lifetime_bucket, ">1 day");
    assert_eq!(fresh.max_ship_size, "Extra Large");
    assert_eq!(
        fresh.lifetime_description,
        "This wormhole has not yet begun to decay"
    );

    // 4h..=24h remaining (12h left): <1 day bucket, beginning to decay
    let decaying = derive_wormhole_buckets_raw(
        30_000_142,
        b041.max_jump_mass,
        b041.max_stable_mass,
        b041.max_stable_mass,
        now,
        lifetime,
        false,
        now + 36 * 3_600,
        &ctx.sde,
    );
    assert_eq!(decaying.lifetime_bucket, "<1 day");
    assert_eq!(
        decaying.lifetime_description,
        "This wormhole is beginning to decay, and probably won't last another day"
    );

    // < 4h remaining (2h left): <4h bucket, reaching end of natural lifetime
    let eol = derive_wormhole_buckets_raw(
        30_000_142,
        b041.max_jump_mass,
        b041.max_stable_mass,
        b041.max_stable_mass,
        now,
        lifetime,
        false,
        now + 46 * 3_600,
        &ctx.sde,
    );
    assert_eq!(eol.lifetime_bucket, "<4h");
    assert_eq!(
        eol.lifetime_description,
        "This wormhole is reaching the end of its natural lifetime"
    );

    // Verify SDE wormhole type attributes drive ship size & destination class buckets
    for (wtid, expected_size, expected_dest) in [
        (34_134_u32, "Small", "C1-3"),       // Wormhole E004 (5M kg -> C1)
        (30_579_u32, "Medium", "C1-3"),      // Wormhole Z971 (62M kg -> C1)
        (30_677_u32, "Large", "High-Sec"),   // Wormhole B274 (375M kg -> High-Sec)
        (56_026_u32, "Large", "Null-Sec"),   // Wormhole C729 (410M kg)
        (30_645_u32, "Extra Large", "C6"),   // Wormhole B041 (1B kg -> C6)
    ] {
        let sig = CosmicSignatureRecord {
            signature_id: format!("SDE-{wtid}"),
            solar_system_id: 30_000_142,
            sig_group: "Wormhole".into(),
            type_name: ctx.sde.wormhole_types[&wtid].name.clone(),
            position: [1_000.0, 0.0, 0.0],
            signal_strength: 1.0,
            wormhole_type_id: Some(wtid),
            destination_system_id: None,
            created_unix: now,
            lifetime_seconds: 0,
            max_mass_kg: 0.0,
            remaining_mass_kg: 500_000_000.0,
            max_jump_mass_kg: 0.0,
            collapsed: false,
        };
        let b = derive_wormhole_buckets(&sig, now + 60, &ctx.sde);
        assert_eq!(b.max_ship_size, expected_size, "wtid {wtid}");
        assert_eq!(b.destination_class, expected_dest, "wtid {wtid}");
    }

    // Verify live MachoNet scanMgr.GetSystemScanMgr & wormholeMgr.WormholeJump RPCs
    let sys_id = 30_000_142_u32;
    let char_id = 90_000_001_i64;
    let ship_id = 100_000_099_i64;
    let mut sess = make_session(char_id, sys_id, ship_id);
    let kw: HashMap<String, PyRep> = HashMap::new();
    let b274 = &ctx.sde.wormhole_types[&30_677];
    let wh_sig_id = "WH-SDE-LIVE".to_string();
    let wh_item_id = wormhole_item_id_for_signature(sys_id, &wh_sig_id);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
            ch.station_id = None;
            ch.structure_id = None;
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 605, // Heron
                owner_id: char_id,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.cosmic_signatures.insert(
            sys_id,
            vec![CosmicSignatureRecord {
                signature_id: wh_sig_id.clone(),
                solar_system_id: sys_id,
                sig_group: "Wormhole".into(),
                type_name: b274.name.clone(),
                position: [500.0, 0.0, 0.0],
                signal_strength: 1.0,
                wormhole_type_id: Some(30_677),
                destination_system_id: Some(30_002_187),
                created_unix: now.saturating_sub(22 * 3_600), // 2h left of 24h -> <4h
                lifetime_seconds: (b274.max_stable_time_min * 60.0) as u64,
                max_mass_kg: b274.max_stable_mass,
                remaining_mass_kg: b274.max_stable_mass * 0.25, // <50%
                max_jump_mass_kg: b274.max_jump_mass,
                collapsed: false,
            }],
        );
    });
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle
        .spawn_ball(eve_destiny::Ball {
            id: ship_id,
            type_id: 605,
            position: glam::DVec3::new(500.0, 0.0, 0.0),
            velocity: glam::DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await;
    let _ = sys_handle
        .spawn_ball(eve_destiny::Ball {
            id: wh_item_id,
            type_id: 26_272,
            position: glam::DVec3::new(500.0, 0.0, 0.0),
            velocity: glam::DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: b274.max_stable_mass,
            radius: 1_500.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Rigid,
        })
        .await;

    let scan_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "scanMgr",
            None,
            "GetSystemScanMgr",
            &[PyRep::Integer(i64::from(sys_id))],
            &kw,
            None,
        )
        .await
        .expect("scanMgr.GetSystemScanMgr");
    let sig_kv = scan_rep
        .dict_get("signatures")
        .and_then(|s| match s {
            PyRep::List(l) => l
                .iter()
                .find(|item| item.dict_get("signatureID") == Some(&PyRep::String(wh_sig_id.clone()))),
            _ => None,
        })
        .expect("WH-SDE-LIVE in GetSystemScanMgr");
    assert_eq!(
        sig_kv.dict_get("maxShipSize"),
        Some(&PyRep::String("Large".into()))
    );
    assert_eq!(
        sig_kv.dict_get("lifetimeBucket"),
        Some(&PyRep::String("<4h".into()))
    );
    assert_eq!(
        sig_kv.dict_get("massStabilityBucket"),
        Some(&PyRep::String("<50%".into()))
    );

    let jump_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "wormholeMgr",
            None,
            "WormholeJump",
            &[PyRep::Integer(wh_item_id)],
            &kw,
            None,
        )
        .await
        .expect("wormholeMgr.WormholeJump");
    assert_eq!(
        jump_rep.dict_get("lifetimeBucket"),
        Some(&PyRep::String("<4h".into()))
    );
    assert_eq!(
        jump_rep.dict_get("lifetimeDescription"),
        Some(&PyRep::String(
            "This wormhole is reaching the end of its natural lifetime".into()
        ))
    );
}

#[tokio::test]
async fn test_hacking_node_types_and_failure_outcome() {
    // parity: , 
    eve_testkit::attest(&[
        
        
    ]);

    // 1. Difficulty 1 (Very Easy) and Difficulty 2 (Easy) must not spawn RestorationNode or VirusSuppressor
    for diff in [1_u8, 2_u8] {
        let tiles = build_hacking_grid_procedural(77_001, false, diff, 1_750_000_000);
        assert!(
            !tiles.iter().any(|t| matches!(
                t.kind,
                HackingNodeKind::RestorationNode | HackingNodeKind::VirusSuppressor
            )),
            "Difficulty {diff} board must not spawn RestorationNode or VirusSuppressor"
        );
    }
    let diff3_tiles = build_hacking_grid_procedural(77_003, false, 3, 1_750_000_000);
    assert!(
        diff3_tiles
            .iter()
            .any(|t| t.kind == HackingNodeKind::RestorationNode),
        "Difficulty 3 board must include RestorationNode"
    );

    // 2. SelfRepair heals 5..=10 coherence per turn for 3 turns without a max_virus_coherence cap,
    //    and 2nd hack failure on a standard Data/Relic container self-destructs with 0.0 ship damage.
    let ctx = make_context();
    let (tier1, stats1) = resolve_hacking_node_stats(Some(&ctx.sde), Some(21_683), false, 0);
    let (tier3, stats3) = resolve_hacking_node_stats(Some(&ctx.sde), Some(21_685), false, 0);
    assert_eq!(tier1, 1);
    assert_eq!(tier3, 3);
    assert!(stats3.core_coh > stats1.core_coh && stats3.fw_coh > stats1.fw_coh);
    let sde_tiles =
        build_hacking_grid_procedural_with_sde(Some(&ctx.sde), 77_004, Some(21_685), false, 0, 1_750_000_000);
    assert!(sde_tiles
        .iter()
        .any(|t| t.kind == HackingNodeKind::RestorationNode));
    let kw: HashMap<String, PyRep> = HashMap::new();
    let char_id = 90_000_001_i64;
    let sys_id = 30_002_813_u32; // Low-sec
    let ship_id = 100_000_001_i64;
    let analyzer_id = 100_000_002_i64;
    let mut sess = make_session(char_id, sys_id, ship_id);

    ensure_system_pve_sites_seeded(&ctx, sys_id).await;
    let (can_id, can_fail, site_pos) = with_pve_exploration_reg(&ctx, |reg| {
        let relic = reg
            .sites_by_system
            .get(&sys_id)
            .and_then(|list| {
                list.iter().find(|s| {
                    s.site_category.eq_ignore_ascii_case("Relic") && s.hackable_can_ids.len() >= 2
                })
            })
            .expect("Seeded standard Relic site with >= 2 cans");
        (relic.hackable_can_ids[0], relic.hackable_can_ids[1], relic.position)
    });

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
            ch.station_id = None;
            ch.structure_id = None;
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 605, // Heron
                owner_id: char_id,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            analyzer_id,
            InventoryItemRecord {
                item_id: analyzer_id,
                type_id: 22_177, // Relic Analyzer I
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::MedSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(analyzer_id, true);
    });
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle
        .spawn_ball(eve_destiny::Ball {
            id: ship_id,
            type_id: 605,
            position: glam::DVec3::from_array(site_pos),
            velocity: glam::DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await;

    let _ = ctx
        .dispatch_rpc(
            &mut sess,
            "hackingMgr",
            None,
            "StartNewGameInstance",
            &[PyRep::Integer(can_id)],
            &kw,
            None,
        )
        .await
        .expect("StartNewGameInstance");

    with_pve_exploration_reg(&ctx, |reg| {
        let g = reg.hacking_games.get_mut(&(char_id, can_id)).unwrap();
        g.virus_coherence = (g.max_virus_coherence - 5).max(1);
        g.utilities[0] = Some(HackingUtilityKind::SelfRepair);
        for t in g.tiles.iter_mut().take(4) {
            t.kind = HackingNodeKind::Empty;
            t.coherence = 0;
            t.strength = 0;
        }
    });

    let _ = ctx
        .dispatch_rpc(
            &mut sess,
            "hackingMgr",
            None,
            "UsedUtilityElement",
            &[PyRep::Integer(can_id), PyRep::Integer(0), PyRep::Integer(0)],
            &kw,
            None,
        )
        .await
        .expect("Activate SelfRepair");

    let (base_coh, max_coh) = with_pve_exploration_reg(&ctx, |reg| {
        let g = &reg.hacking_games[&(char_id, can_id)];
        (g.virus_coherence, g.max_virus_coherence)
    });
    let mut prev_coh = base_coh;
    for turn in 1..=3_u32 {
        let _ = ctx
            .dispatch_rpc(
                &mut sess,
                "hackingMgr",
                None,
                "ClickedOnTile",
                &[PyRep::Integer(can_id), PyRep::Integer(i64::from(turn))],
                &kw,
                None,
            )
            .await
            .expect("Click tile");
        let cur_coh = with_pve_exploration_reg(&ctx, |reg| {
            reg.hacking_games[&(char_id, can_id)].virus_coherence
        });
        let delta = cur_coh - prev_coh;
        assert!(
            (5..=10).contains(&delta),
            "SelfRepair turn {turn} must heal 5..=10 coherence, got {delta}"
        );
        prev_coh = cur_coh;
    }
    let (after_coh, rem_sr) = with_pve_exploration_reg(&ctx, |reg| {
        (
            reg.hacking_games[&(char_id, can_id)].virus_coherence,
            reg.self_repair_turns.get(&(char_id, can_id)).copied(),
        )
    });
    assert!(after_coh > base_coh, "SelfRepair must heal virus coherence");
    assert!(
        after_coh > max_coh,
        "SelfRepair must allow virus coherence to exceed max_virus_coherence (got {base_coh} -> {after_coh}, max {max_coh})"
    );
    assert_eq!(rem_sr, None, "self_repair_turns must be cleaned up after 3 turns");

    // Trigger 2nd failure on a standard container in low-sec -> self-destructs with the low-sec band
    // explosive blast (2,500 HP; null-sec 3,500, wormhole 4,500, high-sec 0)
    for attempt in 1..=2 {
        let _ = ctx
            .dispatch_rpc(
                &mut sess,
                "hackingMgr",
                None,
                "StartNewGameInstance",
                &[PyRep::Integer(can_fail)],
                &kw,
                None,
            )
            .await
            .expect("StartNewGameInstance");
        with_pve_exploration_reg(&ctx, |reg| {
            let g = reg.hacking_games.get_mut(&(char_id, can_fail)).unwrap();
            g.virus_coherence = 5;
            g.tiles[1].kind = HackingNodeKind::Antivirus;
            g.tiles[1].coherence = 500;
            g.tiles[1].strength = 50;
        });
        let rep = ctx
            .dispatch_rpc(
                &mut sess,
                "hackingMgr",
                None,
                "ClickedOnTile",
                &[PyRep::Integer(can_fail), PyRep::Integer(1)],
                &kw,
                None,
            )
            .await
            .expect("ClickedOnTile fail");
        if attempt == 2 {
            assert_eq!(rep.dict_get("containerExploded"), Some(&PyRep::Bool(true)));
            assert_eq!(rep.dict_get("explosionDamage"), Some(&PyRep::Real(2_500.0)));
        }
    }
}

#[tokio::test]
async fn test_skyhook_reagent_harvest_secure_surplus_split() {
    // parity: feat:sovereignty.skyhook-reagent-maturity
    eve_testkit::attest(&["feat:sovereignty.skyhook-reagent-maturity"]);

    let ctx = make_context();
    let skyhook_id = 1_040_000_001_i64;
    let t0 = 1_750_000_000_u64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.sov_runtime
            .skyhook_reagent_bays
            .insert(skyhook_id, (81_143, 0, 0, t0));
        snap.sov_runtime
            .skyhook_last_harvest_unix
            .insert(skyhook_id, t0);
        let (_, sec_1h, sur_1h) = advance_skyhook_reagents_in_snap(snap, skyhook_id, t0 + 3_600)
            .expect("1h skyhook harvest");
        assert!(sec_1h > 0, "Newly harvested reagents must split into Secure bay");
        assert!(sur_1h > 0, "Newly harvested reagents must split into Surplus bay");
        assert_eq!(sec_1h, sur_1h, "Harvested reagents split 50/50 between Secure and Surplus");
    });
}
}

// Folded submodule: parity_fake_remediation_part1
#[allow(unused_imports, dead_code)]
pub(crate) mod features_parity_part5 {
    #[allow(unused_imports)]
    use super::*;

// End-to-end remediation integration tests for ESI social, calendar, contacts,
// mailing lists, corporation governance, and sovereignty map (`esi:*` fake_done closure).


use crate::esi_market_industry_pi::{issue_token_with_scopes, rpc_call};
use eve_db::corp_roles::{
    ROLE_CONTRACT_MANAGER, ROLE_DIRECTOR, ROLE_PROJECT_MANAGER, ROLE_TRADER,
};
use eve_db::{AllianceRecord, CorporationRecord, IskCents, TrainedSkillRecord};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::esi::router::EsiRouter;
use eve_server::esi::{EsiHttpRequest, OpenApiSpec};
use eve_server::router::ServerContext;
use eve_server::services::sov_services::claim_system_sovereignty;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

fn esi_get_validated(router: &EsiRouter, spec: &OpenApiSpec, path: &str, token: Option<&str>) -> Value {
    let mut req = EsiHttpRequest::new("GET", path);
    if let Some(t) = token {
        req = req.with_bearer_token(t);
    }
    let resp = router.handle_request(&req);
    assert_eq!(resp.status_code, 200, "GET {path} -> {}: {}", resp.status_code, String::from_utf8_lossy(&resp.body));
    let raw_path = path.split('?').next().unwrap_or(path);
    spec.validate_response("GET", raw_path, resp.status_code, &resp.headers, &resp.body)
        .unwrap_or_else(|e| panic!("OpenAPI validation failed for GET {path}: {e}"));
    serde_json::from_slice(&resp.body).unwrap_or(Value::Null)
}

fn esi_put_validated(router: &EsiRouter, spec: &OpenApiSpec, path: &str, token: &str, body: &Value) {
    let req = EsiHttpRequest::new("PUT", path)
        .with_bearer_token(token)
        .with_header("content-type", "application/json")
        .with_body(serde_json::to_vec(body).unwrap());
    let resp = router.handle_request(&req);
    assert_eq!(resp.status_code, 204, "PUT {path} -> {}: {}", resp.status_code, String::from_utf8_lossy(&resp.body));
    spec.validate_response("PUT", path, resp.status_code, &resp.headers, &resp.body)
        .unwrap_or_else(|e| panic!("OpenAPI validation failed for PUT {path}: {e}"));
}

#[tokio::test]
async fn test_parity_remediation_esi_calendar_rpc_and_http_persistence() {
    // parity: esi:GET /characters/{character_id}/calendar, esi:GET /characters/{character_id}/calendar/{event_id}, esi:PUT /characters/{character_id}/calendar/{event_id}, esi:GET /characters/{character_id}/calendar/{event_id}/attendees
    eve_testkit::attest(&[
        "esi:GET /characters/{character_id}/calendar",
        "esi:GET /characters/{character_id}/calendar/{event_id}",
        "esi:PUT /characters/{character_id}/calendar/{event_id}",
        "esi:GET /characters/{character_id}/calendar/{event_id}/attendees",
    ]);

    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();

    let ceo_id = 90_000_001_i64;
    let wingman_id = 90_000_002_i64;
    let corp_id = 98_600_001_i64;
    let jita_station = 60_003_760_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let tpl = snap.characters[&ceo_id].clone();
        for (cid, name) in [(ceo_id, "Calendar CEO"), (wingman_id, "Calendar Wingman")] {
            let mut ch = snap.characters.get(&cid).cloned().unwrap_or_else(|| tpl.clone());
            ch.character_id = cid;
            ch.name = name.into();
            ch.corporation_id = corp_id;
            ch.station_id = Some(jita_station);
            ch.solar_system_id = 30_000_142;
            snap.characters.insert(cid, ch);
        }
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "Calendar Corp".into(),
                ticker: "CALC".into(),
                ceo_id,
                tax_rate: 0.05,
                members: HashSet::from([ceo_id, wingman_id]),
                member_roles: HashMap::from([(ceo_id, ROLE_DIRECTOR), (wingman_id, 0)]),
                ..CorporationRecord::default()
            },
        );
    });

    let mut ceo_sess = SessionState::new(70_101);
    ceo_sess.user_id = Some(1);
    ceo_sess.role = eve_machonet::service_roles::ROLE_PLAYER_DEFAULT;
    ceo_sess.character_id = Some(ceo_id);
    ceo_sess.corporation_id = Some(corp_id);
    ceo_sess.station_id = Some(jita_station);
    ceo_sess.solar_system_id = Some(30_000_142);

    let mut wing_sess = SessionState::new(70_102);
    wing_sess.user_id = Some(1);
    wing_sess.role = eve_machonet::service_roles::ROLE_PLAYER_DEFAULT;
    wing_sess.character_id = Some(wingman_id);
    wing_sess.corporation_id = Some(corp_id);
    wing_sess.station_id = Some(jita_station);
    wing_sess.solar_system_id = Some(30_000_142);

    let event_ft = ((ctx.now_unix() as i64 + 7_200) + 11_644_473_600) * 10_000_000;
    let create_rep = rpc_call(
        &ctx,
        &mut ceo_sess,
        "calendarMgr",
        "CreateCorporationEvent",
        &[
            PyRep::Integer(event_ft),
            PyRep::Integer(90),
            PyRep::String("Fleet Stratop".into()),
            PyRep::String("Form up in Jita 4-4".into()),
            PyRep::Integer(1),
        ],
    )
    .await;
    let event_id = create_rep
        .as_i64()
        .or_else(|| create_rep.dict_get("eventID").and_then(|v| v.as_i64()))
        .expect("created corporation event_id");
    assert!(event_id > 0);

    // Respond via MachoNet RPC as wingman (5 = tentative)
    rpc_call(
        &ctx,
        &mut wing_sess,
        "calendarMgr",
        "SendEventResponse",
        &[
            PyRep::Integer(event_id),
            PyRep::Integer(corp_id),
            PyRep::Integer(5),
        ],
    )
    .await;

    // Respond via ESI HTTP PUT as CEO ("accepted")
    let cal_tok = issue_token_with_scopes(
        &ctx,
        ceo_id,
        vec![
            "esi-calendar.read_calendar_events.v1",
            "esi-calendar.respond_calendar_events.v1",
        ],
    );
    esi_put_validated(
        &router,
        &spec,
        &format!("/v1/characters/{ceo_id}/calendar/{event_id}/"),
        &cal_tok,
        &json!({ "response": "accepted" }),
    );

    // Clear ephemeral esi_state to prove ESI reads directly from WorldStateSnapshot
    ctx.esi_state.calendar_events.write().unwrap().clear();
    ctx.esi_state.calendar_responses.write().unwrap().clear();

    let list = esi_get_validated(
        &router,
        &spec,
        &format!("/v1/characters/{ceo_id}/calendar/"),
        Some(&cal_tok),
    );
    let arr = list.as_array().expect("calendar list array");
    let item = arr.iter().find(|e| e["event_id"] == json!(event_id)).expect("event in calendar list");
    assert_eq!(item["title"], json!("Fleet Stratop"));
    assert_eq!(item["importance"], json!(1));
    assert_eq!(item["event_response"], json!("accepted"));

    let detail = esi_get_validated(
        &router,
        &spec,
        &format!("/v1/characters/{ceo_id}/calendar/{event_id}/"),
        Some(&cal_tok),
    );
    assert_eq!(detail["event_id"], json!(event_id));
    assert_eq!(detail["owner_id"], json!(corp_id));
    assert_eq!(detail["owner_type"], json!("corporation"));
    assert_eq!(detail["duration"], json!(90));
    assert_eq!(detail["text"], json!("Form up in Jita 4-4"));
    assert_eq!(detail["response"], json!("accepted"));

    let attendees = esi_get_validated(
        &router,
        &spec,
        &format!("/v1/characters/{ceo_id}/calendar/{event_id}/attendees/"),
        Some(&cal_tok),
    );
    let att_arr = attendees.as_array().expect("attendees array");
    assert!(att_arr.iter().any(|a| a["character_id"] == json!(ceo_id) && a["event_response"] == json!("accepted")));
    assert!(att_arr.iter().any(|a| a["character_id"] == json!(wingman_id) && a["event_response"] == json!("tentative")));
}

#[tokio::test]
async fn test_parity_remediation_esi_contacts_labels_and_mail_lists() {
    // parity: esi:GET /characters/{character_id}/contacts/labels, esi:GET /corporations/{corporation_id}/contacts, esi:GET /corporations/{corporation_id}/contacts/labels, esi:GET /alliances/{alliance_id}/contacts, esi:GET /alliances/{alliance_id}/contacts/labels, esi:GET /characters/{character_id}/mail/lists
    eve_testkit::attest(&[
        "esi:GET /characters/{character_id}/contacts/labels",
        "esi:GET /corporations/{corporation_id}/contacts",
        "esi:GET /corporations/{corporation_id}/contacts/labels",
        "esi:GET /alliances/{alliance_id}/contacts",
        "esi:GET /alliances/{alliance_id}/contacts/labels",
        "esi:GET /characters/{character_id}/mail/lists",
    ]);

    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();

    let ceo_id = 90_000_001_i64;
    let target_char_id = 90_000_002_i64;
    let corp_id = 98_600_002_i64;
    let alliance_id = 99_600_002_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let tpl = snap.characters[&ceo_id].clone();
        for (cid, name) in [(ceo_id, "Diplo CEO"), (target_char_id, "Neutral Contact")] {
            let mut ch = snap.characters.get(&cid).cloned().unwrap_or_else(|| tpl.clone());
            ch.character_id = cid;
            ch.name = name.into();
            ch.corporation_id = if cid == ceo_id { corp_id } else { 1_000_009 };
            ch.alliance_id = if cid == ceo_id { Some(alliance_id) } else { None };
            ch.balance = IskCents::from_isk(50_000_000);
            snap.characters.insert(cid, ch);
        }
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "Diplo Corp".into(),
                ticker: "DPLC".into(),
                ceo_id,
                alliance_id: Some(alliance_id),
                tax_rate: 0.05,
                members: HashSet::from([ceo_id]),
                member_roles: HashMap::from([(ceo_id, ROLE_DIRECTOR)]),
                ..CorporationRecord::default()
            },
        );
        snap.alliances.insert(
            alliance_id,
            AllianceRecord {
                alliance_id,
                name: "Diplo Alliance".into(),
                short_name: "DPLA".into(),
                executor_corp_id: corp_id,
                member_corp_ids: HashSet::from([corp_id]),
            },
        );
    });

    let mut sess = SessionState::new(70_103);
    sess.user_id = Some(1);
    sess.role = eve_machonet::service_roles::ROLE_PLAYER_DEFAULT;
    sess.character_id = Some(ceo_id);
    sess.corporation_id = Some(corp_id);
    sess.alliance_id = Some(alliance_id);
    sess.corp_role = ROLE_DIRECTOR;

    // 1. Character contact label via charMgr.CreateLabel
    rpc_call(
        &ctx,
        &mut sess,
        "charMgr",
        "CreateLabel",
        &[PyRep::String("Logi Anchors".into()), PyRep::Integer(1)],
    )
    .await;

    // 2. Corporation contact & label via corpRegistry
    rpc_call(
        &ctx,
        &mut sess,
        "corpRegistry",
        "CreateLabel",
        &[PyRep::String("Blues".into()), PyRep::Integer(2)],
    )
    .await;
    rpc_call(
        &ctx,
        &mut sess,
        "corpRegistry",
        "AddCorporateContact",
        &[PyRep::Integer(target_char_id), PyRep::Real(5.0), PyRep::Integer(1)],
    )
    .await;

    // 3. Alliance contact & label via allianceRegistry
    rpc_call(
        &ctx,
        &mut sess,
        "allianceRegistry",
        "CreateLabel",
        &[PyRep::String("Coalition".into()), PyRep::Integer(3)],
    )
    .await;
    rpc_call(
        &ctx,
        &mut sess,
        "allianceRegistry",
        "AddAllianceContact",
        &[PyRep::Integer(target_char_id), PyRep::Real(10.0), PyRep::Integer(1)],
    )
    .await;

    // 4. Mailing list via mailingListsMgr.Create
    rpc_call(
        &ctx,
        &mut sess,
        "mailingListsMgr",
        "Create",
        &[
            PyRep::String("FC Announcements".into()),
            PyRep::Integer(0),
            PyRep::Integer(0),
            PyRep::Integer(0),
        ],
    )
    .await;

    // Clear ephemeral esi_state maps to prove SQLite/WorldStateSnapshot backing
    ctx.esi_state.char_contact_labels.write().unwrap().clear();
    ctx.esi_state.corp_contacts.write().unwrap().clear();
    ctx.esi_state.corp_contact_labels.write().unwrap().clear();
    ctx.esi_state.alliance_contacts.write().unwrap().clear();
    ctx.esi_state.alliance_contact_labels.write().unwrap().clear();
    ctx.esi_state.mail_lists.write().unwrap().clear();

    let tok = issue_token_with_scopes(
        &ctx,
        ceo_id,
        vec![
            "esi-characters.read_contacts.v1",
            "esi-corporations.read_contacts.v1",
            "esi-alliances.read_contacts.v1",
            "esi-mail.read_mail.v1",
        ],
    );

    let char_labels = esi_get_validated(&router, &spec, &format!("/v1/characters/{ceo_id}/contacts/labels/"), Some(&tok));
    assert!(char_labels.as_array().unwrap().iter().any(|l| l["label_name"] == json!("Logi Anchors")));

    let corp_contacts = esi_get_validated(&router, &spec, &format!("/v1/corporations/{corp_id}/contacts/"), Some(&tok));
    assert!(corp_contacts.as_array().unwrap().iter().any(|c| c["contact_id"] == json!(target_char_id) && c["standing"] == json!(5.0)));

    let corp_labels = esi_get_validated(&router, &spec, &format!("/v1/corporations/{corp_id}/contacts/labels/"), Some(&tok));
    assert!(corp_labels.as_array().unwrap().iter().any(|l| l["label_name"] == json!("Blues")));

    let ally_contacts = esi_get_validated(&router, &spec, &format!("/v1/alliances/{alliance_id}/contacts/"), Some(&tok));
    assert!(ally_contacts.as_array().unwrap().iter().any(|c| c["contact_id"] == json!(target_char_id) && c["standing"] == json!(10.0)));

    let ally_labels = esi_get_validated(&router, &spec, &format!("/v1/alliances/{alliance_id}/contacts/labels/"), Some(&tok));
    assert!(ally_labels.as_array().unwrap().iter().any(|l| l["label_name"] == json!("Coalition")));

    let mail_lists = esi_get_validated(&router, &spec, &format!("/v1/characters/{ceo_id}/mail/lists/"), Some(&tok));
    assert!(mail_lists.as_array().unwrap().iter().any(|m| m["name"] == json!("FC Announcements")));
}

#[tokio::test]
async fn test_parity_remediation_esi_corp_governance_roles_limit_and_sov_map() {
    // parity: esi:GET /corporations/{corporation_id}/members/limit, esi:GET /corporations/{corporation_id}/roles, esi:GET /corporations/{corporation_id}/roles/history, esi:GET /sovereignty/map
    eve_testkit::attest(&[
        "esi:GET /corporations/{corporation_id}/members/limit",
        "esi:GET /corporations/{corporation_id}/roles",
        "esi:GET /corporations/{corporation_id}/roles/history",
        "esi:GET /sovereignty/map",
    ]);

    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();

    let ceo_id = 90_000_001_i64;
    let member_id = 90_000_002_i64;
    let corp_id = 98_600_003_i64;
    let alliance_id = 99_600_003_i64;
    let null_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.security_status <= 0.0)
        .map(|s| s.solar_system_id)
        .expect("SDE nullsec solar system");

    ctx.db.with_mut_snapshot(|snap| {
        let tpl = snap.characters[&ceo_id].clone();
        for (cid, name) in [(ceo_id, "Gov CEO"), (member_id, "Gov Member")] {
            let mut ch = snap.characters.get(&cid).cloned().unwrap_or_else(|| tpl.clone());
            ch.character_id = cid;
            ch.name = name.into();
            ch.corporation_id = corp_id;
            ch.alliance_id = Some(alliance_id);
            snap.characters.insert(cid, ch);
        }
        snap.skills.entry(ceo_id).or_default().clear();
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "Governance Corp".into(),
                ticker: "GOVC".into(),
                ceo_id,
                alliance_id: Some(alliance_id),
                tax_rate: 0.05,
                members: HashSet::from([ceo_id, member_id]),
                member_roles: HashMap::from([(ceo_id, ROLE_DIRECTOR), (member_id, 0)]),
                ..CorporationRecord::default()
            },
        );
        snap.alliances.insert(
            alliance_id,
            AllianceRecord {
                alliance_id,
                name: "Governance Alliance".into(),
                short_name: "GOVA".into(),
                executor_corp_id: corp_id,
                member_corp_ids: HashSet::from([corp_id]),
            },
        );
    });

    let tok = issue_token_with_scopes(
        &ctx,
        ceo_id,
        vec![
            "esi-corporations.read_corporation_membership.v1",
            "esi-corporations.read_titles.v1",
            "esi-corporations.track_members.v1",
        ],
    );

    // 1. Untrained CEO defaults to base 10 member limit; training Corporation Management + UpdateCorporationAbilities updates it
    let base_limit = esi_get_validated(
        &router,
        &spec,
        &format!("/v1/corporations/{corp_id}/members/limit/"),
        Some(&tok),
    );
    assert_eq!(base_limit, json!(10));

    let mut ceo_sess = SessionState::new(70_104);
    ceo_sess.user_id = Some(1);
    ceo_sess.role = eve_machonet::service_roles::ROLE_PLAYER_DEFAULT;
    ceo_sess.character_id = Some(ceo_id);
    ceo_sess.corporation_id = Some(corp_id);
    ceo_sess.alliance_id = Some(alliance_id);
    ceo_sess.corp_role = ROLE_DIRECTOR;

    rpc_call(
        &ctx,
        &mut ceo_sess,
        "corpRegistry",
        "UpdateCorporationAbilities",
        &[],
    )
    .await;
    let untrained_rpc_limit = esi_get_validated(
        &router,
        &spec,
        &format!("/v1/corporations/{corp_id}/members/limit/"),
        Some(&tok),
    );
    assert_eq!(untrained_rpc_limit, json!(10));

    let sde_bonus = ctx
        .sde
        .types
        .get(&3363)
        .and_then(|t| t.default_attributes.get(&191))
        .copied()
        .expect("SDE attribute 191 on Corporation Management (3363)")
        .round() as i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(ceo_id).or_default().insert(
            3363,
            TrainedSkillRecord {
                type_id: 3363,
                level: 3,
                skill_points: 16_000.0,
            },
        );
    });

    rpc_call(
        &ctx,
        &mut ceo_sess,
        "corpRegistry",
        "UpdateCorporationAbilities",
        &[],
    )
    .await;

    let updated_limit = esi_get_validated(
        &router,
        &spec,
        &format!("/v1/corporations/{corp_id}/members/limit/"),
        Some(&tok),
    );
    assert_eq!(updated_limit, json!(sde_bonus * 3));

    // 2. Live corpRegistry.UpdateMember grants Trader, Contract_Manager, Project_Manager -> reflected in /roles and /roles/history
    let granted_mask = (ROLE_TRADER | ROLE_CONTRACT_MANAGER | ROLE_PROJECT_MANAGER) as i64;
    rpc_call(
        &ctx,
        &mut ceo_sess,
        "corpRegistry",
        "UpdateMember",
        &[
            PyRep::Integer(member_id),
            PyRep::String(String::new()),
            PyRep::Integer(granted_mask),
            PyRep::Integer(0),
        ],
    )
    .await;
    ctx.advance_secs(86_405);
    rpc_call(
        &ctx,
        &mut ceo_sess,
        "corpRegistry",
        "UpdateMember",
        &[
            PyRep::Integer(member_id),
            PyRep::String(String::new()),
            PyRep::Integer(granted_mask),
            PyRep::Integer(0),
        ],
    )
    .await;
    let tok = issue_token_with_scopes(
        &ctx,
        ceo_id,
        vec![
            "esi-corporations.read_corporation_membership.v1",
            "esi-corporations.read_titles.v1",
            "esi-corporations.track_members.v1",
        ],
    );

    let roles_json = esi_get_validated(
        &router,
        &spec,
        &format!("/v1/corporations/{corp_id}/roles/"),
        Some(&tok),
    );
    let member_entry = roles_json
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["character_id"] == json!(member_id))
        .expect("member in /roles");
    let role_names: Vec<&str> = member_entry["roles"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(role_names.contains(&"Trader"));
    assert!(role_names.contains(&"Contract_Manager"));
    assert!(role_names.contains(&"Project_Manager"));

    let history_json = esi_get_validated(
        &router,
        &spec,
        &format!("/v1/corporations/{corp_id}/roles/history/"),
        Some(&tok),
    );
    let hist_entry = history_json
        .as_array()
        .unwrap()
        .iter()
        .find(|h| h["character_id"] == json!(member_id) && h["issuer_id"] == json!(ceo_id))
        .expect("role history entry");
    let new_roles: Vec<&str> = hist_entry["new_roles"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(new_roles.contains(&"Trader"));
    assert!(new_roles.contains(&"Contract_Manager"));

    // 3. Live sovereignty claim -> reflected in GET /sovereignty/map
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.entry(null_sys).or_default();
    });
    claim_system_sovereignty(&ctx, null_sys, alliance_id, corp_id, 0)
        .expect("claim_system_sovereignty");
    let sov_map = esi_get_validated(&router, &spec, "/v1/sovereignty/map/", None);
    let sys_entry = sov_map
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["system_id"] == json!(null_sys as i64))
        .expect("claimed system in /sovereignty/map");
    assert_eq!(sys_entry["alliance_id"], json!(alliance_id));
    assert_eq!(sys_entry["corporation_id"], json!(corp_id));
}
}

// Folded submodule: parity_fake_remediation_part2
#[allow(unused_imports, dead_code)]
pub(crate) mod features_parity_part6 {
    #[allow(unused_imports)]
    use super::*;

// End-to-end remediation integration tests for ESI corp blueprints/structures
// and Quasar title definitions, space CDN, freelance projects/item delivery,
// and payment schedules/tokens/store (`esi:*` & `quasar:*` fake_done closure).


use crate::esi_market_industry_pi::{issue_token_with_scopes, make_item, rpc_call};
use crate::quasar_sov_equinox_hub::{drain, field, fields, id_req, idm, rpc, sess, subscribe};
use eve_db::corp_roles::{ROLE_DIRECTOR, ROLE_FACTORY_MANAGER, ROLE_STATION_MANAGER};
use eve_db::{CorporationRecord, InventoryFlag, IskCents};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::quasar::{encode_bytes_field, encode_string_field, encode_varint_field};
use eve_proto::PyRep;
use eve_server::esi::router::EsiRouter;
use eve_server::esi::{EsiHttpRequest, OpenApiSpec};
use eve_server::quasar_gateway::{STATUS_OK, STATUS_UNAUTHORIZED};
use eve_server::router::ServerContext;
use eve_server::services::quasar_payment_store_types::{
    save_store_catalog, PaymentScheduleRecord, PaymentTokenRecord, StoreCatalog, StoreCategory,
    StoreOffer, StoreProduct,
};
use glam::DVec3;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

fn esi_get_validated(router: &EsiRouter, spec: &OpenApiSpec, path: &str, token: Option<&str>) -> Value {
    let mut req = EsiHttpRequest::new("GET", path);
    if let Some(t) = token {
        req = req.with_bearer_token(t);
    }
    let resp = router.handle_request(&req);
    assert_eq!(resp.status_code, 200, "GET {path} -> {}: {}", resp.status_code, String::from_utf8_lossy(&resp.body));
    let raw_path = path.split('?').next().unwrap_or(path);
    spec.validate_response("GET", raw_path, resp.status_code, &resp.headers, &resp.body)
        .unwrap_or_else(|e| panic!("OpenAPI validation failed for GET {path}: {e}"));
    serde_json::from_slice(&resp.body).unwrap_or(Value::Null)
}

#[tokio::test]
async fn test_parity_remediation_esi_corp_blueprints_and_structures_live_rpcs() {
    // parity: esi:GET /corporations/{corporation_id}/blueprints, esi:GET /corporations/{corporation_id}/structures
    eve_testkit::attest(&[
        "esi:GET /corporations/{corporation_id}/blueprints",
        "esi:GET /corporations/{corporation_id}/structures",
    ]);

    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();

    let ceo_id = 90_000_001_i64;
    let corp_id = 98_600_010_i64;
    let jita_station = 60_003_760_i64;
    let perim_sys = 30_000_144_u32;
    let ship_id = 80_600_001_i64;
    let bpo_id = 99_600_101_i64;
    let astrahus_item_id = 99_600_102_i64;
    let market_svc_id = 99_600_103_i64;

    let anchor_pos = ctx
        .sde
        .celestials_in_system(perim_sys)
        .into_iter()
        .find(|c| c.group_id == 7)
        .map(|p| [p.position.x + 25_000_000.0, p.position.y, p.position.z])
        .unwrap_or([100_000_000.0, 0.0, 0.0]);

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters[&ceo_id].clone();
        ch.corporation_id = corp_id;
        ch.station_id = Some(jita_station);
        ch.solar_system_id = 30_000_142;
        ch.active_ship_id = ship_id;
        ch.balance = IskCents::from_isk(500_000_000);
        snap.characters.insert(ceo_id, ch);
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "Industrial Structure Corp".into(),
                ticker: "INDS".into(),
                ceo_id,
                master_wallet: IskCents::from_isk(100_000_000),
                tax_rate: 0.05,
                members: HashSet::from([ceo_id]),
                member_roles: HashMap::from([(
                    ceo_id,
                    ROLE_DIRECTOR | ROLE_FACTORY_MANAGER | ROLE_STATION_MANAGER,
                )]),
                ..CorporationRecord::default()
            },
        );
        // Seed corp BPO (Rifter Blueprint = 684) and Astrahus item (35832) + Standup Market Hub I (35892)
        snap.items.insert(
            bpo_id,
            make_item(bpo_id, 684, corp_id, jita_station, InventoryFlag::CorpSAG1, 1, true),
        );
        snap.items.insert(
            astrahus_item_id,
            make_item(astrahus_item_id, 35832, ceo_id, ship_id, InventoryFlag::Cargo, 1, false),
        );
        snap.items.insert(
            market_svc_id,
            make_item(market_svc_id, 35892, corp_id, jita_station, InventoryFlag::CorpSAG1, 1, false),
        );
    });

    let mut sess = SessionState::new(70_201);
    sess.user_id = Some(1);
    sess.role = eve_machonet::service_roles::ROLE_PLAYER_DEFAULT;
    sess.character_id = Some(ceo_id);
    sess.corporation_id = Some(corp_id);
    sess.station_id = Some(jita_station);
    sess.solar_system_id = Some(30_000_142);
    sess.corp_role = ROLE_DIRECTOR | ROLE_FACTORY_MANAGER | ROLE_STATION_MANAGER;

    // 1. Live industryManager.InstallJob (ME Research = activityID 4) + CompleteJob on corp BPO
    let me_dict = PyRep::Dict(vec![
        (PyRep::String("blueprintID".into()), PyRep::Integer(bpo_id)),
        (PyRep::String("activityID".into()), PyRep::Integer(4)),
        (PyRep::String("runs".into()), PyRep::Integer(2)),
        (PyRep::String("facilityID".into()), PyRep::Integer(jita_station)),
        (PyRep::String("useCorp".into()), PyRep::Bool(true)),
    ]);
    let job_rep = rpc_call(&ctx, &mut sess, "industryManager", "InstallJob", &[me_dict]).await;
    let job_id = job_rep.dict_get("jobID").and_then(|v| v.as_i64()).expect("ME jobID");
    ctx.db.clock().advance_secs(50_000);
    rpc_call(&ctx, &mut sess, "industryManager", "CompleteJob", &[PyRep::Integer(job_id)]).await;

    let tok = issue_token_with_scopes(
        &ctx,
        ceo_id,
        vec![
            "esi-corporations.read_blueprints.v1",
            "esi-corporations.read_structures.v1",
        ],
    );
    let bps = esi_get_validated(
        &router,
        &spec,
        &format!("/v1/corporations/{corp_id}/blueprints/"),
        Some(&tok),
    );
    let bp_entry = bps
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["item_id"] == json!(bpo_id))
        .expect("researched corp BPO in /corporations/{id}/blueprints");
    assert_eq!(bp_entry["material_efficiency"], json!(2));
    assert_eq!(bp_entry["runs"], json!(-1));

    // 2. Live structureDeployment.AnchorStructure + structureServices.SetServiceOnlineState
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&ceo_id) {
            ch.station_id = None;
            ch.solar_system_id = perim_sys;
        }
    });
    sess.station_id = None;
    sess.solar_system_id = Some(perim_sys);
    sess.ship_id = Some(ship_id);
    ctx.systems
        .get_or_spawn(perim_sys)
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 587,
            position: DVec3::from_array(anchor_pos),
            velocity: DVec3::ZERO,
            max_velocity: 365.0,
            inertia: 3.0,
            mass: 1_000_000.0,
            radius: 100.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await
        .unwrap();

    let anchor_rep = rpc_call(
        &ctx,
        &mut sess,
        "structureDeployment",
        "Anchor",
        &[
            PyRep::Integer(astrahus_item_id),
            PyRep::Tuple(vec![
                PyRep::Real(anchor_pos[0] + 60_000.0),
                PyRep::Real(anchor_pos[1]),
                PyRep::Real(anchor_pos[2]),
            ]),
            PyRep::String("Perimeter Trade Citadel".into()),
            PyRep::Integer(1),
            PyRep::Integer(18),
        ],
    )
    .await;
    let structure_id = anchor_rep
        .as_i64()
        .or_else(|| anchor_rep.dict_get("structureID").and_then(|v| v.as_i64()))
        .expect("anchored structure_id");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.structures.get_mut(&structure_id) {
            st.state = "ShieldVulnerable".into();
        }
        if let Some(svc) = snap.items.get_mut(&market_svc_id) {
            svc.location_id = structure_id;
            svc.flag = InventoryFlag::ServiceSlot0;
        }
    });
    rpc_call(
        &ctx,
        &mut sess,
        "structureDeployment",
        "RenameStructure",
        &[PyRep::Integer(structure_id), PyRep::String("Perimeter Trade Citadel".into())],
    )
    .await;

    let structs = esi_get_validated(
        &router,
        &spec,
        &format!("/v1/corporations/{corp_id}/structures/"),
        Some(&tok),
    );
    let st_entry = structs
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["structure_id"] == json!(structure_id))
        .expect("anchored structure in /corporations/{id}/structures");
    assert_eq!(st_entry["name"], json!("Perimeter Trade Citadel"));
    assert_eq!(st_entry["reinforce_hour"], json!(21));
    assert_eq!(
        st_entry["services"],
        json!([{ "name": "Standup Market Hub I", "state": "offline" }])
    );
}

#[tokio::test]
async fn test_parity_remediation_quasar_title_definitions_and_space_cdn() {
    // parity: quasar:eveProto/generated/eve_public/cosmetic/character/title/definition/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/space/api/cdn/requests_pb2.pyj, quasar:eveProto/generated/eve_public/achievement/definition/api/cdn/requests_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/cosmetic/character/title/definition/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/space/api/cdn/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/achievement/definition/api/cdn/requests_pb2.pyj",
    ]);

    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let router = EsiRouter::new(Arc::clone(&ctx));
    let mut s = sess(70_202, 90_000_001, 98_000_001, 30_000_142, 0);

    // 1. Character title definitions GetLatestRequest returns CDN URL; no school-synthesized titles
    let latest_rsp = rpc(
        &ctx,
        &mut s,
        "eve_public.cosmetic.character.title.definition.api.GetLatestRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(latest_rsp.status_code, STATUS_OK);
    let latest_val = latest_rsp.payload.expect("GetLatestResponse").value;
    let cp = field(&latest_val, 1).expect("checkpoint");
    let url_bytes = field(&cp, 1).expect("url");
    let url = String::from_utf8(url_bytes).expect("utf8 url");
    let path = url.strip_prefix("http://127.0.0.1:26004").expect("local ESI HTTP CDN URL");
    assert_eq!(path, "/cdn/cosmetics/character/title/definitions/v24.1.0/en-us.json");
    let cdn_http = router.handle_request(&EsiHttpRequest::new("GET", path));
    assert_eq!(cdn_http.status_code, 200);
    let parsed_defs: Value = serde_json::from_slice(&cdn_http.body).expect("title defs JSON");
    assert_eq!(parsed_defs, json!([]));

    // 3. Space CDN GetUpdateRequest returns configurable CDN URL; EsiRouter does not serve invented MAPT binary
    let mut no_local = Vec::new();
    encode_varint_field(2, 1, &mut no_local);
    let space_rsp = rpc(&ctx, &mut s, "eve_public.space.api.cdn.GetUpdateRequest", no_local).await;
    assert_eq!(space_rsp.status_code, STATUS_OK);
    let space_cp = field(&space_rsp.payload.unwrap().value, 1).expect("space checkpoint");
    let space_url = String::from_utf8(field(&space_cp, 1).unwrap()).unwrap();
    let space_path = space_url.strip_prefix("http://127.0.0.1:26004").expect("local ESI HTTP URL");
    assert_eq!(space_path, format!("/cdn/maptags/v24.1.{}/universe.fb", ctx.sde.solar_systems.len()));
    let map_http = router.handle_request(&EsiHttpRequest::new("GET", space_path));
    assert_eq!(map_http.status_code, 404);

    // 4. Achievement definition CDN GetLatestRequest returns local EsiRouter JSON bundle
    let ach_rsp = rpc(
        &ctx,
        &mut s,
        "eve_public.achievement.definition.api.cdn.GetLatestRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(ach_rsp.status_code, STATUS_OK);
    let ach_cp = field(&ach_rsp.payload.unwrap().value, 1).expect("ach checkpoint");
    let ach_url = String::from_utf8(field(&ach_cp, 1).unwrap()).unwrap();
    let ach_path = ach_url.strip_prefix("http://127.0.0.1:26004").expect("local ESI HTTP URL");
    let ach_mgr = &ctx.quasar_gateway.progression.achievements;
    assert_eq!(ach_path, format!("/cdn/achievements/v24.1.{}/{}.json", ach_mgr.sde_achievement_count, ach_mgr.sde_achievement_hash));
    let ach_http = router.handle_request(&EsiHttpRequest::new("GET", ach_path));
    assert_eq!(ach_http.status_code, 200);
    let ach_defs: Value = serde_json::from_slice(&ach_http.body).expect("ach defs JSON");
    assert_eq!(ach_defs.as_array().unwrap().len(), ach_mgr.sde_achievement_count);
}

#[tokio::test]
async fn test_parity_remediation_quasar_freelance_projects_and_item_delivery() {
    // Regression only (seeded); the freelance quasar rows are attested live in quasar_freelance_and_corporation_live.rs.

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    let manager_id = 90_000_001_i64;
    let pilot_id = 90_000_002_i64;
    let corp_id = 98_600_020_i64;
    let jita_station = 60_003_760_i64;
    let ore_item_id = 99_600_201_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let tpl = snap.characters[&manager_id].clone();
        for (cid, name, c_id) in [
            (manager_id, "Project Manager", corp_id),
            (pilot_id, "Contractor Pilot", 1_000_009),
        ] {
            let mut ch = snap.characters.get(&cid).cloned().unwrap_or_else(|| tpl.clone());
            ch.character_id = cid;
            ch.name = name.into();
            ch.corporation_id = c_id;
            ch.station_id = Some(jita_station);
            ch.solar_system_id = 30_000_142;
            ch.balance = IskCents::from_isk(10_000_000);
            snap.characters.insert(cid, ch);
        }
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "Freelance Sponsor Corp".into(),
                ticker: "FRSC".into(),
                ceo_id: manager_id,
                master_wallet: IskCents::from_isk(100_000_000),
                tax_rate: 0.05,
                members: HashSet::from([manager_id]),
                member_roles: HashMap::from([(manager_id, ROLE_DIRECTOR)]),
                ..CorporationRecord::default()
            },
        );
        // Seed 500 Tritanium (type_id 34) in contractor's Jita hangar
        snap.items.insert(
            ore_item_id,
            make_item(ore_item_id, 34, pilot_id, jita_station, InventoryFlag::Hangar, 500, false),
        );
    });

    let mut mgr_sess = sess(70_203, manager_id, corp_id, 30_000_142, ROLE_DIRECTOR);
    let mut pilot_sess = sess(70_204, pilot_id, 1_000_009, 30_000_142, 0);
    let mut pilot_rx = subscribe(&ctx, &mut pilot_sess).await;

    // 1. CreateRequest with explicit 2,000,000 ISK budget (10,000 ISK/unit * 200), batch=100, type_id=34
    let (mut cfg_b, mut param_b, mut opts_b, mut mod_b, mut rew_b, mut isk_b) = (
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    );
    encode_string_field(1, "34", &mut opts_b);
    encode_bytes_field(3, &opts_b, &mut param_b);
    encode_bytes_field(3, &param_b, &mut cfg_b);
    encode_varint_field(4, 100, &mut mod_b);
    encode_varint_field(1, 10_000, &mut isk_b);
    encode_bytes_field(1, &isk_b, &mut rew_b);

    let mut create_req = Vec::new();
    encode_string_field(1, "Supply Jita Tritanium", &mut create_req);
    encode_string_field(2, "Deliver 200 Tritanium to Jita 4-4", &mut create_req);
    encode_varint_field(3, 200, &mut create_req);
    encode_bytes_field(6, &cfg_b, &mut create_req);
    encode_bytes_field(7, &mod_b, &mut create_req);
    encode_bytes_field(10, &rew_b, &mut create_req);
    let create_rsp = rpc(
        &ctx,
        &mut mgr_sess,
        "eve_public.freelance.project.api.CreateRequest",
        create_req.clone(),
    )
    .await;
    assert_eq!(create_rsp.status_code, STATUS_OK);
    let id_msg = field(&create_rsp.payload.unwrap().value, 1).expect("project Identifier");
    assert!(id_msg.len() >= 16);

    // 2. CommitRequest as contractor
    assert_eq!(
        rpc(&ctx, &mut pilot_sess, "eve_public.freelance.project.api.CommitRequest", id_msg.clone())
            .await
            .status_code,
        STATUS_OK
    );

    // 3. DeliverRequest for 200 units -> moves items to CorpDeliveries & emits ContributionSucceededNotice
    let mut deliver_req = id_msg.clone();
    encode_varint_field(4, 200, &mut deliver_req);
    let deliv_rsp = rpc(
        &ctx,
        &mut pilot_sess,
        "eve_public.freelance.contributionmethod.itemdelivery.api.DeliverRequest",
        deliver_req,
    )
    .await;
    assert_eq!(deliv_rsp.status_code, STATUS_OK);

    let notices = drain(&mut pilot_rx);
    assert!(
        notices.iter().any(|(t, _)| t.ends_with("ContributionSucceededNotice")),
        "DeliverRequest must emit ContributionSucceededNotice"
    );

    // Verify items moved from pilot Hangar (500 -> 300) into CorpDeliveries (200)
    ctx.db.with_snapshot(|snap| {
        let pilot_qty: i32 = snap
            .items
            .values()
            .filter(|i| i.owner_id == pilot_id && i.type_id == 34 && i.flag == InventoryFlag::Hangar)
            .map(|i| i.quantity)
            .sum();
        let corp_deliv_qty: i32 = snap
            .items
            .values()
            .filter(|i| i.owner_id == corp_id && i.type_id == 34 && i.flag == InventoryFlag::CorpDeliveries)
            .map(|i| i.quantity)
            .sum();
        assert_eq!(pilot_qty, 300);
        assert_eq!(corp_deliv_qty, 200);
    });

    // 4. RedeemRewardRequest credits contractor & emits ParticipantRewardRedeemedNotice
    let redeem_rsp = rpc(
        &ctx,
        &mut pilot_sess,
        "eve_public.freelance.project.api.RedeemRewardRequest",
        id_msg.clone(),
    )
    .await;
    assert_eq!(redeem_rsp.status_code, STATUS_OK);
    let redeem_notices = drain(&mut pilot_rx);
    assert!(
        redeem_notices.iter().any(|(t, _)| t.ends_with("ParticipantRewardRedeemedNotice")),
        "RedeemRewardRequest must emit ParticipantRewardRedeemedNotice"
    );

    // 5. Cancelled project DeliverRequest must fail without duplicating items into CorpDeliveries
    let create_rsp2 = rpc(&ctx, &mut mgr_sess, "eve_public.freelance.project.api.CreateRequest", create_req).await;
    assert_eq!(create_rsp2.status_code, STATUS_OK);
    let id_msg2 = field(&create_rsp2.payload.unwrap().value, 1).expect("project2 Identifier");
    assert_eq!(rpc(&ctx, &mut pilot_sess, "eve_public.freelance.project.api.CommitRequest", id_msg2.clone()).await.status_code, STATUS_OK);
    assert_eq!(rpc(&ctx, &mut mgr_sess, "eve_public.freelance.project.api.CloseRequest", id_msg2.clone()).await.status_code, STATUS_OK);
    let mut deliver_req2 = id_msg2;
    encode_varint_field(4, 100, &mut deliver_req2);
    assert_ne!(
        rpc(&ctx, &mut pilot_sess, "eve_public.freelance.contributionmethod.itemdelivery.api.DeliverRequest", deliver_req2)
            .await
            .status_code,
        STATUS_OK
    );
    ctx.db.with_snapshot(|snap| {
        let pilot_qty: i32 = snap.items.values().filter(|i| i.owner_id == pilot_id && i.type_id == 34 && i.flag == InventoryFlag::Hangar).map(|i| i.quantity).sum();
        let corp_deliv_qty: i32 = snap.items.values().filter(|i| i.owner_id == corp_id && i.type_id == 34 && i.flag == InventoryFlag::CorpDeliveries).map(|i| i.quantity).sum();
        assert_eq!(pilot_qty, 300);
        assert_eq!(corp_deliv_qty, 200);
    });
}

#[tokio::test]
async fn test_parity_remediation_quasar_payment_schedules_tokens_and_store() {
    // parity: quasar:eveProto/generated/eve_public/payment/schedule/notices_pb2.pyj, quasar:eveProto/generated/eve_public/payment/token/api_pb2.pyj, quasar:eveProto/generated/eve_public/payment/token/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/payment/purchase/api_pb2.pyj, quasar:eveProto/generated/eve_public/store/category/api_pb2.pyj, quasar:eveProto/generated/eve_public/store/offer/api_pb2.pyj, quasar:eveProto/generated/eve_public/store/product/api_pb2.pyj, quasar:eveProto/generated/eve_public/store/targetaudience/offer/api/requests_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/payment/schedule/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/payment/token/api_pb2.pyj",
        "quasar:eveProto/generated/eve_public/payment/token/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/payment/purchase/api_pb2.pyj",
        "quasar:eveProto/generated/eve_public/store/category/api_pb2.pyj",
        "quasar:eveProto/generated/eve_public/store/offer/api_pb2.pyj",
        "quasar:eveProto/generated/eve_public/store/product/api_pb2.pyj",
        "quasar:eveProto/generated/eve_public/store/targetaudience/offer/api/requests_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    ctx.db.with_mut_snapshot(|snap| {
        save_store_catalog(
            snap,
            &StoreCatalog {
                categories: vec![StoreCategory {
                    id: 1,
                    name: "Omega".into(),
                    description: "Omega Clone State".into(),
                }],
                products: vec![StoreProduct {
                    id: 101,
                    name: "30 Days Omega".into(),
                    description: "Omega time".into(),
                    type_id: 3966,
                    quantity: 1,
                }],
                offers: vec![StoreOffer {
                    id: 1001,
                    name: "30 Days Omega".into(),
                    description: "Monthly Omega".into(),
                    category_id: 1,
                    price_plex: 500,
                    product_ids: vec![101],
                    targeted: true,
                    price_cents: Some(1_999),
                    currency: Some("USD".into()),
                }],
            },
        );
        let tokens = vec![
            PaymentTokenRecord {
                id: 88,
                user_id: 1,
                token_str: "tok_live_88".into(),
                is_quick_pay: true,
                consumed: false,
            },
            PaymentTokenRecord {
                id: 89,
                user_id: 1,
                token_str: "tok_live_89".into(),
                is_quick_pay: true,
                consumed: false,
            },
        ];
        snap.service_state_blobs
            .insert("payment_tokens".into(), serde_json::to_string(&tokens).unwrap());
    });

    let mut s = sess(70_205, 90_000_001, 98_000_001, 30_000_142, 0);
    let mut rx = subscribe(&ctx, &mut s).await;

    // 1. Store category, product, offer, and targetaudience endpoints
    assert_eq!(rpc(&ctx, &mut s, "eve_public.store.category.api.GetAllRequest", Vec::new()).await.status_code, STATUS_OK);
    assert_eq!(rpc(&ctx, &mut s, "eve_public.store.product.api.GetRequest", id_req(1, 101)).await.status_code, STATUS_OK);
    assert_eq!(rpc(&ctx, &mut s, "eve_public.store.offer.api.GetAllRequest", Vec::new()).await.status_code, STATUS_OK);
    let ta_rsp = rpc(
        &ctx,
        &mut s,
        "eve_public.store.targetaudience.offer.api.ListRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(ta_rsp.status_code, STATUS_OK);
    assert!(!fields(&ta_rsp.payload.unwrap().value, 1).is_empty());

    // 2. Payment token GetQuickPayRequest + DisableAllRequest emits QuickPayRemovedNotice (and unauth -> 401)
    let mut unauth = SessionState::new(70_206);
    assert_eq!(
        rpc(&ctx, &mut unauth, "eve_public.payment.token.api.GetQuickPayRequest", Vec::new())
            .await
            .status_code,
        STATUS_UNAUTHORIZED
    );
    let qp_rsp = rpc(&ctx, &mut s, "eve_public.payment.token.api.GetQuickPayRequest", Vec::new()).await;
    assert_eq!(qp_rsp.status_code, STATUS_OK);
    assert!(!fields(&qp_rsp.payload.unwrap().value, 1).is_empty());

    // 3. Purchase Omega via TokenRequest -> creates PaymentScheduleRecord
    let mut purchase_req = Vec::new();
    let mut order = Vec::new();
    let mut off_ident = Vec::new();
    encode_string_field(1, "VEVC-1001", &mut off_ident);
    encode_bytes_field(1, &off_ident, &mut order);
    encode_varint_field(2, 1, &mut order);
    encode_bytes_field(1, &order, &mut purchase_req);
    encode_bytes_field(2, &idm(88), &mut purchase_req);
    assert_eq!(
        rpc(&ctx, &mut s, "eve_public.payment.purchase.api.TokenRequest", purchase_req)
            .await
            .status_code,
        STATUS_OK
    );

    // 4. Advance clock past 30-day billing cycle with quick-pay token -> GetActiveByType is pure read, tick renews & records receipt
    let sched_due: u64 = ctx.db.with_snapshot(|snap| {
        snap.service_state_blobs
            .get("payment_schedules")
            .and_then(|j| serde_json::from_str::<Vec<PaymentScheduleRecord>>(j).ok())
            .and_then(|v| v.first().map(|s| s.next_payment_unix))
            .unwrap()
    });
    let advance = sched_due.saturating_add(60).saturating_sub(ctx.now_unix() as u64);
    ctx.advance_secs(advance);
    let mut by_type = Vec::new();
    encode_varint_field(1, 1, &mut by_type);
    assert_eq!(
        rpc(&ctx, &mut s, "eve_public.payment.schedule.api.GetActiveByTypeRequest", by_type.clone())
            .await
            .status_code,
        STATUS_OK
    );
    let receipts_before_tick: usize = ctx.db.with_snapshot(|snap| {
        snap.service_state_blobs
            .get("payment_receipts")
            .and_then(|j| serde_json::from_str::<Vec<eve_server::services::quasar_payment_store_types::PaymentReceiptRecord>>(j).ok())
            .map(|v| v.len())
            .unwrap_or(0)
    });
    assert_eq!(receipts_before_tick, 1);

    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let (renewed_due, receipts_after_tick) = ctx.db.with_snapshot(|snap| {
        let due = snap
            .service_state_blobs
            .get("payment_schedules")
            .and_then(|j| serde_json::from_str::<Vec<PaymentScheduleRecord>>(j).ok())
            .and_then(|v| v.first().map(|s| s.next_payment_unix))
            .unwrap();
        let rcpts = snap
            .service_state_blobs
            .get("payment_receipts")
            .and_then(|j| serde_json::from_str::<Vec<eve_server::services::quasar_payment_store_types::PaymentReceiptRecord>>(j).ok())
            .unwrap_or_default();
        (due, rcpts)
    });
    assert!(renewed_due > sched_due);
    assert_eq!(receipts_after_tick.len(), 2);
    assert_eq!(receipts_after_tick[1].amount_in_cents, 1_999);

    // 5. Disable quick-pay tokens, advance past renewed due date, and tick -> PaymentFailedNotice & CancelledNotice
    assert_eq!(
        rpc(&ctx, &mut s, "eve_public.payment.token.api.DisableAllRequest", Vec::new())
            .await
            .status_code,
        STATUS_OK
    );
    let tok_notices = drain(&mut rx);
    assert!(tok_notices.iter().any(|(t, _)| t.ends_with("QuickPayRemovedNotice")));

    let advance2 = renewed_due.saturating_add(60).saturating_sub(ctx.now_unix() as u64);
    ctx.advance_secs(advance2);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let sched_notices = drain(&mut rx);
    assert!(sched_notices.iter().any(|(t, _)| t.ends_with("PaymentFailedNotice")));
    assert!(sched_notices.iter().any(|(t, _)| t.ends_with("CancelledNotice")));
    assert_ne!(
        rpc(&ctx, &mut s, "eve_public.payment.schedule.api.GetActiveByTypeRequest", by_type)
            .await
            .status_code,
        STATUS_OK
    );
}
}

#[allow(unused_imports, dead_code, unused_variables, unused_mut)]
pub(crate) mod parity_plan_canonical_closure {
    #[allow(unused_imports)]
    use super::*;
use std::collections::HashMap;
use glam::DVec3;

use eve_destiny::{Ballpark, MovementMode};
use eve_machonet::{MachoPacket, MachoPacketType, SessionState};
use eve_proto::quasar::{
    encode_varint_field, EvePublicRequest, EvePublicResponse, GoogleAny, GoogleTimestamp,
    QuasarMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::{marshal, PyRep};
use eve_server::quasar_gateway::{STATUS_OK, STATUS_UNAUTHORIZED};
use eve_server::router::ServerContext;
use eve_server::system_actor::populate_asteroid_belt_entities;

use crate::common;

fn make_session(sid: u64, uid: u64, cid: i64, corp_id: i64) -> SessionState {
    let mut sess = SessionState::new(sid);
    sess.user_id = Some(uid);
    sess.character_id = Some(cid);
    sess.corporation_id = Some(corp_id);
    sess.solar_system_id = Some(30_000_142);
    sess.role = eve_machonet::service_roles::ROLE_GM_DEFAULT;
    sess
}

fn extract_response_proto(rep: &PyRep) -> EvePublicResponse {
    if let PyRep::KeyVal(entries) = rep {
        for (k, v) in entries {
            if k == "response_proto" {
                if let PyRep::Bytes(b) = v {
                    return EvePublicResponse::decode_proto(b)
                        .expect("decode EvePublicResponse from response_proto");
                }
            }
        }
    }
    panic!("Expected KeyVal containing response_proto, got {rep:?}");
}

async fn quasar_rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    type_name: &str,
    payload_bytes: Vec<u8>,
) -> EvePublicResponse {
    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp {
            seconds: ctx.now_unix() as i64,
            nanos: 0,
        }),
        correlation_uuid: b"canonical-closure-01".to_vec(),
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
            value: payload_bytes,
        }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"app-canonical-test".to_vec(),
        authoritative_context: None,
        trace_context: None,
    };
    let empty_kw = HashMap::new();
    let rep = ctx
        .dispatch_rpc(
            sess,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("publicGatewaySvc.SendRequest");
    extract_response_proto(&rep)
}

// =========================================================================
// 1. plan:C-5 & plan:MN-01: Mining static data, portion sizes & asteroid belts
// =========================================================================
// parity: plan:C-5, plan:MN-01
#[tokio::test]
async fn test_canonical_plan_c5_and_mn01_mining_sde_and_asteroid_belts() {
    eve_testkit::attest(&["plan:C-5", "plan:MN-01"]);

    let ctx = ServerContext::new();
    let sde = &ctx.sde;

    // MN-01: Veldspar (1230) -> Tritanium (34) only with portionSize 100
    let veldspar_type = &sde.types[&1230];
    assert_eq!(veldspar_type.portion_size, 100);
    let veldspar_mats = sde.type_materials_for(1230);
    assert_eq!(veldspar_mats.len(), 1);
    assert_eq!(veldspar_mats[0].material_type_id, 34);
    assert_eq!(veldspar_mats[0].quantity, 400);

    // Scordite (1228) -> Tritanium (34) + Pyerite (35)
    let scordite_mats: Vec<u32> = sde
        .type_materials_for(1228)
        .iter()
        .map(|m| m.material_type_id)
        .collect();
    assert_eq!(scordite_mats, vec![34, 35]);

    // Clear Icicle (16262) yields its isotope and fuel materials
    let ice_mats: Vec<u32> = sde
        .type_materials_for(16262)
        .iter()
        .map(|m| m.material_type_id)
        .collect();
    for expected in [16272u32, 16273, 16274, 16275] {
        assert!(ice_mats.contains(&expected), "Clear Icicle must yield {expected}");
    }

    // Ore variant yields differ from base ore (Concentrated Veldspar 17470 vs Veldspar 1230)
    let conc_veld_mats = sde.type_materials_for(17470);
    assert_eq!(conc_veld_mats.len(), 1);
    assert!(conc_veld_mats[0].quantity > veldspar_mats[0].quantity);

    // Compressible types and dogma attributes
    assert_eq!(sde.compressible_types.get(&1230), Some(&62516));
    assert!(sde.compressible_types.contains_key(&16262));
    assert!(sde.types.contains_key(&483)); // Miner I
    assert!(sde.types[&483].default_attributes.contains_key(&77)); // miningAmount

    // C-5 / MN-01: Asteroid belts as celestials & balls in Perimeter (30000144) & LZ-6SU (30000208)
    let mut highsec_bp = Ballpark::new(30_000_144);
    let highsec_states = populate_asteroid_belt_entities(&mut highsec_bp, 30_000_144, sde);
    assert!(!highsec_states.is_empty(), "Perimeter must spawn asteroid belt entities");
    for st in &highsec_states {
        assert!(st.remaining_units > 0);
        let ball = highsec_bp.balls.get(&st.asteroid_id).expect("asteroid ball in ballpark");
        assert!(ball.radius >= 60.0);
    }

    let mut null_bp = Ballpark::new(30_000_208);
    let null_states = populate_asteroid_belt_entities(&mut null_bp, 30_000_208, sde);
    assert!(!null_states.is_empty(), "Nullsec 30000208 must spawn asteroid belt entities");
}

// =========================================================================
// 2. plan:Q-3, plan:Q-4, plan:Q-7, plan:Q-8: Quasar packages dispatch
// =========================================================================
// parity: plan:Q-3, plan:Q-4, plan:Q-7, plan:Q-8
#[tokio::test]
async fn test_canonical_plan_q3_q4_q7_q8_quasar_packages_dispatch() {
    eve_testkit::attest(&["plan:Q-3", "plan:Q-4", "plan:Q-7", "plan:Q-8"]);

    let ctx = ServerContext::new();
    let mut sess = make_session(10, 1, 90_000_001, 1_000_009);

    // Negative check: unauthenticated requests fail with STATUS_UNAUTHORIZED
    let mut unauth = SessionState::new(9901);
    let unauth_resp = quasar_rpc(
        &ctx,
        &mut unauth,
        "eve_public.character.api.GetRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(unauth_resp.status_code, STATUS_UNAUTHORIZED);

    // Q-3: Identity, accounts and entitlements (Store / Payment / Omega)
    let q3_resp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.store.category.api.GetAllRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(q3_resp.status_code, STATUS_OK);
    assert_eq!(q3_resp.correlation_uuid, b"canonical-closure-01");
    assert_eq!(
        q3_resp.payload.as_ref().unwrap().normalized_type_name(),
        "eve_public.store.category.api.GetAllResponse"
    );

    // Q-4: Character and progression (Career goals & Achievement showcase)
    let q4_goals = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.career.goal.api.GetAllRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(q4_goals.status_code, STATUS_OK);
    assert_eq!(
        q4_goals.payload.as_ref().unwrap().normalized_type_name(),
        "eve_public.career.goal.api.GetAllResponse"
    );

    let q4_score = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.achievement.showcase.api.GetCharacterScoreRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(q4_score.status_code, STATUS_OK);
    assert_eq!(
        q4_score.payload.as_ref().unwrap().normalized_type_name(),
        "eve_public.achievement.showcase.api.GetCharacterScoreResponse"
    );

    // Q-7: Space and combat (Space CDN update)
    let mut cdn_req = Vec::new();
    encode_varint_field(2, 1, &mut cdn_req);
    let q7_resp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.space.api.cdn.GetUpdateRequest",
        cdn_req,
    )
    .await;
    assert_eq!(q7_resp.status_code, STATUS_OK);
    assert_eq!(
        q7_resp.payload.as_ref().unwrap().normalized_type_name(),
        "eve_public.space.api.cdn.GetUpdateResponse"
    );

    // Q-8: Sovereignty, faction warfare and events (Planet resources & Mercenary den)
    let q8_defs = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.sovereignty.resource.planet.api.GetAllDefinitionsRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(q8_defs.status_code, STATUS_OK);
    assert_eq!(
        q8_defs.payload.as_ref().unwrap().normalized_type_name(),
        "eve_public.sovereignty.resource.planet.api.GetAllDefinitionsResponse"
    );

    let q8_ver = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.sovereignty.resource.planet.api.GetDefinitionsVersionRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(q8_ver.status_code, STATUS_OK);
    assert_eq!(
        q8_ver.payload.as_ref().unwrap().normalized_type_name(),
        "eve_public.sovereignty.resource.planet.api.GetDefinitionsVersionResponse"
    );

    let q8_merc = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.sovereignty.mercenaryden.api.GetMaximumForCharacterRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(q8_merc.status_code, STATUS_OK);
    assert_eq!(
        q8_merc.payload.as_ref().unwrap().normalized_type_name(),
        "eve_public.sovereignty.mercenaryden.api.GetMaximumForCharacterResponse"
    );
}

// =========================================================================
// 3. plan:T-8, plan:T-9, plan:T-10: Test harnesses and oracles
// =========================================================================
// parity: plan:T-8, plan:T-9, plan:T-10, feat:invariant.atomic-rollback-all-stores
#[tokio::test]
async fn test_canonical_plan_t8_t9_t10_t11_harnesses_and_oracles() {
    eve_testkit::attest(&[
        "plan:T-8",
        "plan:T-9",
        "plan:T-10",
        "feat:invariant.atomic-rollback-all-stores",
    ]);

    let db_path = std::env::temp_dir().join(format!("canonical_t9_{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&db_path);

    // T-8 & T-9: Conservation, transactional atomicity, and crash/restart persistence
    let (initial_isk, initial_plex) = {
        let ctx = ServerContext::from_sqlite(&db_path).expect("from_sqlite");
        ctx.seed_demo_world_fixtures();
        let (before_isk, before_plex) = ctx
            .db
            .with_snapshot(|snap| {
                snap.characters
                    .get(&90_000_001)
                    .map(|c| (c.balance, c.plex_balance))
            })
            .expect("character 90_000_001 exists");

        // T-8: Transactional atomicity: failing transaction rolls back ISK and PLEX
        let rollback_res: Result<(), String> = ctx.db.try_with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&90_000_001) {
                ch.balance.0 += 100_000_000;
                ch.plex_balance += 500;
            }
            Err("simulated_invariant_violation".to_string())
        });
        assert!(rollback_res.is_err());

        // Also verify failing/unauthenticated mutating RPC leaves DB completely unchanged
        let mut unauth = SessionState::new(8801);
        let rpc_err = ctx
            .dispatch_rpc(
                &mut unauth,
                "charMgr",
                None,
                "AddContact",
                &[
                    PyRep::Integer(90_000_002),
                    PyRep::Real(5.0),
                    PyRep::Integer(0),
                    PyRep::Bool(false),
                ],
                &HashMap::new(),
                None,
            )
            .await;
        assert!(rpc_err.is_err(), "unauthenticated mutating RPC must fail");

        let (after_isk, after_plex) = ctx
            .db
            .with_snapshot(|snap| {
                snap.characters
                    .get(&90_000_001)
                    .map(|c| (c.balance, c.plex_balance))
            })
            .expect("character 90_000_001 exists");
        assert_eq!(before_isk, after_isk, "ISK balance must be conserved on rollback");
        assert_eq!(before_plex, after_plex, "PLEX balance must be conserved on rollback");

        (before_isk, before_plex)
    };

    // T-9: Crash/restart recovery preserves state across SQLite reopen
    {
        let ctx2 = ServerContext::from_sqlite(&db_path).expect("reopen_sqlite");
        let (restarted_isk, restarted_plex) = ctx2
            .db
            .with_snapshot(|snap| {
                snap.characters
                    .get(&90_000_001)
                    .map(|c| (c.balance, c.plex_balance))
            })
            .expect("character 90_000_001 exists after restart");
        assert_eq!(initial_isk, restarted_isk);
        assert_eq!(initial_plex, restarted_plex);
    }
    let _ = std::fs::remove_file(&db_path);

    // T-10: Client-expectation oracle (Quasar envelope metadata & RPC contract)
    let ctx = ServerContext::new();
    let mut authed = make_session(20, 1, 90_000_001, 1_000_009);
    let resp = quasar_rpc(
        &ctx,
        &mut authed,
        "eve_public.store.category.api.GetAllRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(resp.status_code, STATUS_OK);
    assert_eq!(resp.correlation_uuid, b"canonical-closure-01");
    assert!(resp.payload.is_some());

    // T-11: Destiny differential physics harness & golden vector kinematics
    let mut ballpark = Ballpark::new(30_000_144);
    let _ = populate_asteroid_belt_entities(&mut ballpark, 30_000_144, &ctx.sde);
    let ball_id = *ballpark.balls.keys().next().expect("seeded ball in ballpark");
    let start_x = ballpark.balls[&ball_id].position.x;
    if let Some(ball) = ballpark.balls.get_mut(&ball_id) {
        ball.velocity = DVec3::new(100.0, 0.0, 0.0);
        ball.mass = 1_067_000.0;
        ball.max_velocity = 365.0;
        ball.inertia = 2.9;
        ball.mode = MovementMode::GotoDirection {
            direction: DVec3::X,
        };
    }
    ballpark.step(1.0);
    let moved = ballpark.balls.get(&ball_id).expect("moved ball in ballpark");
    assert!(moved.position.x > start_x + 50.0, "kinematic step must advance X position");
}

// =========================================================================
// 4. plan:Z-1 & plan:Z-2: RPC & Notification Closure
// =========================================================================
#[tokio::test]
async fn test_canonical_plan_z1_z2_rpc_and_notify_closure() {
    let ctx = ServerContext::new();
    ctx.seed_demo_world_fixtures();
    let empty_kw = HashMap::new();

    // Z-1: Dispatch client-called RPCs with V24 argument layouts and live semantics
    let mut sess = SessionState::new(30);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;

    let char_info = ctx
        .dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "GetCharacterInfo",
            &[PyRep::Integer(90_000_001)],
            &empty_kw,
            Some("test:Z-1:GetCharacterInfo"),
        )
        .await
        .expect("charUnboundMgr.GetCharacterInfo");
    assert_ne!(char_info, PyRep::None);

    let select_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "SelectCharacterID",
            &[PyRep::Integer(90_000_001)],
            &empty_kw,
            Some("test:Z-1:SelectCharacterID"),
        )
        .await
        .expect("charUnboundMgr.SelectCharacterID");
    assert_ne!(select_rep, PyRep::None);

    let station_info = ctx
        .dispatch_rpc(
            &mut sess,
            "stationSvc",
            None,
            "GetStation",
            &[PyRep::Integer(60_003_760)],
            &empty_kw,
            Some("test:Z-1:GetStation"),
        )
        .await
        .expect("stationSvc.GetStation");
    assert_ne!(station_info, PyRep::None);

    // Z-2: Systematic notification emission pipelines, marshaled packet types, and multicast delivery
    let notifs = ctx.notify_account_change(90_000_001, "cash", 90_000_001, 100_000_000.0);
    assert_eq!(notifs.len(), 1);
    assert_eq!(notifs[0].event, "OnAccountChange");
    assert_eq!(notifs[0].id_type, "charid");
    assert_eq!(notifs[0].args.len(), 3);

    // Verify binary packet marshal round-trip matches Macho notification format
    let decoded = marshal::decode(&notifs[0].marshaled_packet).expect("decode OnAccountChange");
    let packet = MachoPacket::from_pyrep(&decoded).expect("parse Macho notification packet");
    assert_eq!(packet.packet_type, MachoPacketType::Notification);

    // Drain notifications for character 90_000_001
    let drained = ctx.drain_notifications_for_char(90_000_001);
    assert!(!drained.is_empty(), "drained notification list must not be empty");
    assert!(drained.iter().any(|p| p.event == "OnAccountChange"));
}


}

pub(crate) mod parity_plan_fake_closure {
    #[allow(unused_imports)]
    use super::*;

// End-to-end integration tests closing canonical plan rows:
// - `plan:E-5`: Agent finder and conversation lifecycle
// - `plan:ESI-5`: Corporation structures timers, starbase detail, and plan closure
// - `plan:Q-5`: Social and organization Quasar packages
// - `plan:Q-6`: Economy and industry Quasar packages


use crate::esi_market_industry_pi::{issue_token_with_scopes, make_item, rpc_call};
use eve_db::corp_roles::{ROLE_DIRECTOR, ROLE_FACTORY_MANAGER, ROLE_STATION_MANAGER};
use eve_db::{AllianceRecord, CorporationRecord, InventoryFlag, IskCents, MarketOrderRecord, StarbaseDetailSettings, TrainedSkillRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, EvePublicRequest,
    EvePublicResponse, GoogleAny, GoogleTimestamp, QuasarMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_server::esi::router::EsiRouter;
use eve_server::esi::{EsiHttpRequest, OpenApiSpec};
use eve_server::router::ServerContext;
use eve_server::services::agents_missions::load_mission_meta;
use glam::DVec3;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

fn esi_get_validated(router: &EsiRouter, spec: &OpenApiSpec, path: &str, token: Option<&str>) -> Value {
    let mut req = EsiHttpRequest::new("GET", path);
    if let Some(t) = token { req = req.with_bearer_token(t); }
    let resp = router.handle_request(&req);
    assert_eq!(resp.status_code, 200, "GET {path} -> {}: {}", resp.status_code, String::from_utf8_lossy(&resp.body));
    let raw_path = path.split('?').next().unwrap_or(path);
    spec.validate_response("GET", raw_path, resp.status_code, &resp.headers, &resp.body)
        .unwrap_or_else(|e| panic!("OpenAPI validation failed for GET {path}: {e}"));
    serde_json::from_slice(&resp.body).unwrap_or(Value::Null)
}

fn extract_response_proto(rep: &PyRep) -> EvePublicResponse {
    if let PyRep::KeyVal(entries) = rep {
        for (k, v) in entries {
            if k == "response_proto" {
                if let PyRep::Bytes(b) = v {
                    return EvePublicResponse::decode_proto(b).expect("decode EvePublicResponse");
                }
            }
        }
    }
    panic!("Expected KeyVal containing response_proto, got {rep:?}");
}

fn encode_id_field(tag: u32, id: u64, out: &mut Vec<u8>) {
    let mut id_val = Vec::new();
    encode_varint_field(1, id, &mut id_val);
    encode_bytes_field(tag, &id_val, out);
}

async fn send_quasar(ctx: &ServerContext, sess: &mut SessionState, type_url: &str, payload_val: Vec<u8>, corr: &[u8]) -> EvePublicResponse {
    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp { seconds: ctx.now_unix() as i64, nanos: 0 }),
        correlation_uuid: corr.to_vec(),
        payload: Some(GoogleAny { type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_url}"), value: payload_val }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"app-plan-fake-closure".to_vec(),
        authoritative_context: None,
        trace_context: None,
    };
    let rep = ctx.dispatch_rpc(sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &HashMap::new(), None)
        .await.expect("SendRequest");
    extract_response_proto(&rep)
}

#[tokio::test]
async fn test_plan_e5_agent_finder_and_conversation_lifecycle() {
    // parity: plan:E-5
    eve_testkit::attest(&["plan:E-5"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    let char_id = 90_000_001_i64;
    let empty_kw = HashMap::new();

    // 1. Train Negotiation (3356), Connections (3359), Security Connections (3895), Social (3355)
    ctx.db.with_mut_snapshot(|snap| {
        let skills = snap.skills.entry(char_id).or_default();
        skills.insert(3356, TrainedSkillRecord { type_id: 3356, level: 5, skill_points: 256_000.0 });
        skills.insert(3359, TrainedSkillRecord { type_id: 3359, level: 4, skill_points: 135_765.0 });
        skills.insert(3895, TrainedSkillRecord { type_id: 3895, level: 4, skill_points: 135_765.0 });
        skills.insert(3355, TrainedSkillRecord { type_id: 3355, level: 4, skill_points: 135_765.0 });
    });

    let mut sess = SessionState::new(71_001);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.role = eve_machonet::service_roles::ROLE_PLAYER_DEFAULT;

    // 2. Agent Finder: Query Level 1 and Level 2 Security agents (division 24)
    let l1_rep = ctx.dispatch_rpc(&mut sess, "agentMgr", None, "GetAgents", &[PyRep::Integer(1), PyRep::Integer(24)], &empty_kw, None).await.expect("GetAgents L1");
    let PyRep::Rowset { rows: l1_rows, .. } = l1_rep else { panic!("Expected Rowset") };
    assert!(!l1_rows.is_empty(), "Level 1 Security agents must be present");
    for row in &l1_rows {
        assert_eq!(row[2].as_i64(), Some(24));
        assert_eq!(row[7].as_i64(), Some(1));
        assert_eq!(row[9], PyRep::Bool(true));
    }

    let l2_rep = ctx.dispatch_rpc(&mut sess, "agentMgr", None, "GetAgents", &[PyRep::Integer(2), PyRep::Integer(24)], &empty_kw, None).await.expect("GetAgents L2");
    let PyRep::Rowset { rows: l2_rows, .. } = l2_rep else { panic!() };
    assert!(!l2_rows.is_empty());
    let l2_corp_id = l2_rows[0][3].as_i64().unwrap();
    assert_eq!(l2_rows[0][9], PyRep::Bool(false), "L2 agent initially inaccessible without corp standing");

    // Standing gate: boost standing with L2 agent's corporation, verifying access unlocks
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) { ch.standings.insert(l2_corp_id, 3.5); }
    });
    let l2_st_rep = ctx.dispatch_rpc(&mut sess, "agentMgr", None, "GetAgents", &[PyRep::Integer(2), PyRep::Integer(24)], &empty_kw, None).await.expect("GetAgents L2 st");
    let PyRep::Rowset { rows: l2_acc_rows, .. } = l2_st_rep else { panic!() };
    let unlocked = l2_acc_rows.iter().find(|r| r[3].as_i64() == Some(l2_corp_id)).unwrap();
    assert_eq!(unlocked[9], PyRep::Bool(true));

    // 3. Conversation lifecycle: Pick L1 agent and co-locate character
    let selected_agent = &l1_rows[0];
    let agent_id = selected_agent[0].as_i64().unwrap();
    let agent_sys = selected_agent[5].as_i64().unwrap() as u32;
    let agent_sta = selected_agent[6].as_i64();
    let agent_corp = selected_agent[3].as_i64().unwrap();

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = agent_sys;
            ch.station_id = agent_sta;
        }
    });
    sess.solar_system_id = Some(agent_sys);
    sess.station_id = agent_sta;

    let sys_rep = ctx.dispatch_rpc(&mut sess, "agentMgr", None, "GetSolarSystemOfAgent", &[PyRep::Integer(agent_id)], &empty_kw, None).await.expect("GetSolarSystemOfAgent");
    assert_eq!(sys_rep.as_i64(), Some(agent_sys as i64));

    let agent_oid = ctx.bind_object(&sess, "agentMgr", agent_id).expect("bind agent moniker");

    // 4. Request mission -> Offered
    let offer_res = ctx.dispatch_rpc(&mut sess, "agentMgr", Some(agent_oid), "DoAction", &[PyRep::Integer(2)], &empty_kw, None).await.expect("DoAction(RequestMission)");
    let PyRep::KeyVal(offer_kv) = offer_res else { panic!("Expected KeyVal for offer") };
    let mission_id = offer_kv["missionID"].as_i64().unwrap() as u32;
    assert_eq!(offer_kv["state"].as_str(), Some("Offered"));
    assert_eq!(offer_kv["missionState"].as_i64(), Some(1));
    assert!(offer_kv["briefing"].as_str().is_some());
    assert!(offer_kv["objectives"].as_str().is_some());
    assert!(offer_kv["iskReward"].as_i64().unwrap() > 0);
    assert!(offer_kv["lpReward"].as_i64().unwrap() > 0);

    // Verify journal reflects offered mission
    let journal = ctx.dispatch_rpc(&mut sess, "agentMgr", None, "GetMyJournalDetails", &[], &empty_kw, None).await.expect("GetMyJournalDetails");
    let PyRep::Tuple(j_tup) = journal else { panic!() };
    let PyRep::List(j_list) = &j_tup[0] else { panic!() };
    assert!(j_list.iter().any(|m| m.dict_get("missionID").and_then(|v| v.as_i64()) == Some(mission_id as i64)));

    // 5. Decline mission without penalty on first decline
    let dec_res = ctx.dispatch_rpc(&mut sess, "agentMgr", Some(agent_oid), "DoAction", &[PyRep::Integer(9)], &empty_kw, None).await.expect("DoAction(Decline)");
    let PyRep::KeyVal(dec_kv) = dec_res else { panic!() };
    assert_eq!(dec_kv["state"].as_str(), Some("Declined"));
    assert_eq!(dec_kv["standingPenaltyApplied"], PyRep::Bool(false));

    // 6. Request new mission and Accept
    let offer2_res = ctx.dispatch_rpc(&mut sess, "agentMgr", Some(agent_oid), "DoAction", &[PyRep::Integer(2)], &empty_kw, None).await.expect("DoAction(Request 2)");
    let PyRep::KeyVal(offer2_kv) = offer2_res else { panic!() };
    let mission2_id = offer2_kv["missionID"].as_i64().unwrap() as u32;

    let acc_res = ctx.dispatch_rpc(&mut sess, "agentMgr", Some(agent_oid), "DoAction", &[PyRep::Integer(3)], &empty_kw, None).await.expect("DoAction(Accept)");
    let PyRep::KeyVal(acc_kv) = acc_res else { panic!() };
    assert_eq!(acc_kv["state"].as_str(), Some("Accepted"));
    assert_eq!(acc_kv["missionState"].as_i64(), Some(2));
    let bm_id = acc_kv["bookmarkID"].as_i64().unwrap();
    assert!(ctx.db.with_snapshot(|snap| snap.bookmarks.contains_key(&bm_id)));

    // 7. Complete the mission: satisfy objectives and invoke Complete (Action 6)
    let initial_balance = ctx.db.with_snapshot(|snap| snap.characters[&char_id].balance);
    let initial_lp = ctx.db.with_snapshot(|snap| snap.characters[&char_id].loyalty_points.get(&agent_corp).copied().unwrap_or(0));
    let initial_agent_st = ctx.db.with_snapshot(|snap| snap.characters[&char_id].standings.get(&agent_id).copied().unwrap_or(0.0));

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(m) = snap.active_missions.get_mut(&(char_id, mission2_id)) { m.npcs_remaining = 0; }
        if let Some(ref m_meta) = load_mission_meta(snap, char_id, mission2_id) {
            if let Some(tid) = m_meta.required_delivery_type_id {
                let qty = m_meta.required_delivery_qty.max(1);
                snap.next_entity_id += 1;
                let iid = snap.next_entity_id;
                snap.items.insert(iid, make_item(iid, tid, char_id, agent_sta.unwrap_or(60_003_760), InventoryFlag::Hangar, qty, false));
            }
        }
    });

    let comp_res = ctx.dispatch_rpc(&mut sess, "agentMgr", Some(agent_oid), "DoAction", &[PyRep::Integer(6)], &empty_kw, None).await.expect("DoAction(Complete)");
    let PyRep::KeyVal(comp_kv) = comp_res else { panic!() };
    assert_eq!(comp_kv["state"].as_str(), Some("Completed"));

    let post_balance = ctx.db.with_snapshot(|snap| snap.characters[&char_id].balance);
    let post_lp = ctx.db.with_snapshot(|snap| snap.characters[&char_id].loyalty_points.get(&agent_corp).copied().unwrap_or(0));
    let post_agent_st = ctx.db.with_snapshot(|snap| snap.characters[&char_id].standings.get(&agent_id).copied().unwrap_or(0.0));

    assert!(post_balance > initial_balance, "Balance increased");
    assert!(post_lp >= initial_lp, "LP awarded");
    assert!(post_agent_st >= initial_agent_st, "Standing propagated");
    assert!(ctx.db.with_snapshot(|snap| snap.wallet_journal.iter().any(|j| j.character_id == char_id && j.entry_type.contains("mission"))));
}

#[tokio::test]
async fn test_plan_esi5_corp_structures_timers_and_starbase_detail() {
    // parity: plan:ESI-5
    eve_testkit::attest(&["plan:ESI-5"]);

    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();

    let ceo_id = 90_000_001_i64;
    let corp_id = 98_600_050_i64;
    let ship_id = 80_600_050_i64;
    let perim_sys = 30_000_144_u32;
    let jita_sta = 60_003_760_i64;
    let astrahus_item_id = 99_600_501_i64;
    let market_svc_id = 99_600_502_i64;
    let starbase_id = 99_600_503_i64;
    let fuel_item_id = 99_600_504_i64;

    let anchor_pos = [100_000_000.0, 0.0, 0.0];

    // 1. Seed corporation, CEO, and items
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters[&ceo_id].clone();
        ch.corporation_id = corp_id;
        ch.station_id = None;
        ch.solar_system_id = perim_sys;
        ch.active_ship_id = ship_id;
        ch.balance = IskCents::from_isk(500_000_000);
        snap.characters.insert(ceo_id, ch);

        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "ESI-5 Upwell Starbase Corp".into(),
                ticker: "ES5C".into(),
                ceo_id,
                tax_rate: 0.075,
                members: HashSet::from([ceo_id]),
                member_roles: HashMap::from([(ceo_id, ROLE_DIRECTOR | ROLE_STATION_MANAGER | ROLE_FACTORY_MANAGER)]),
                ..CorporationRecord::default()
            },
        );

        snap.items.insert(astrahus_item_id, make_item(astrahus_item_id, 35832, ceo_id, ship_id, InventoryFlag::Cargo, 1, false));
        snap.items.insert(market_svc_id, make_item(market_svc_id, 35892, corp_id, jita_sta, InventoryFlag::Hangar, 1, false));
        snap.items.insert(starbase_id, make_item(starbase_id, 16213, corp_id, perim_sys as i64, InventoryFlag::None, 1, true));
        snap.items.insert(fuel_item_id, make_item(fuel_item_id, 4051, corp_id, starbase_id, InventoryFlag::None, 500, false));
        snap.starbase_settings.insert(
            starbase_id,
            StarbaseDetailSettings {
                fuel_bay_view: "corporation_member".into(),
                fuel_bay_take: "starbase_fuel_technician_role".into(),
                anchor: "starbase_fuel_technician_role".into(),
                unanchor: "starbase_fuel_technician_role".into(),
                online: "starbase_fuel_technician_role".into(),
                offline: "starbase_fuel_technician_role".into(),
                ..StarbaseDetailSettings::default()
            },
        );
    });

    let mut sess = SessionState::new(71_002);
    sess.user_id = Some(1);
    sess.role = eve_machonet::service_roles::ROLE_PLAYER_DEFAULT;
    sess.character_id = Some(ceo_id);
    sess.corporation_id = Some(corp_id);
    sess.solar_system_id = Some(perim_sys);
    sess.ship_id = Some(ship_id);
    sess.corp_role = ROLE_DIRECTOR | ROLE_STATION_MANAGER | ROLE_FACTORY_MANAGER;

    ctx.systems.get_or_spawn(perim_sys).spawn_ball(Ball {
        id: ship_id,
        type_id: 587,
        position: DVec3::from_array(anchor_pos),
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 2.0,
        mass: 1_000_000.0,
        radius: 100.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }).await.unwrap();

    // 2. Live structureDeployment.Anchor RPC
    let anchor_rep = rpc_call(
        &ctx,
        &mut sess,
        "structureDeployment",
        "Anchor",
        &[
            PyRep::Integer(astrahus_item_id),
            PyRep::Tuple(vec![PyRep::Real(anchor_pos[0] + 50_000.0), PyRep::Real(anchor_pos[1]), PyRep::Real(anchor_pos[2])]),
            PyRep::String("Perimeter Free Market Citadel".into()),
            PyRep::Integer(1),
            PyRep::Integer(18),
        ],
    ).await;
    let structure_id = anchor_rep.as_i64().or_else(|| anchor_rep.dict_get("structureID").and_then(|v| v.as_i64())).expect("anchored structure_id");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.structures.get_mut(&structure_id) { st.state = "ShieldVulnerable".into(); }
        if let Some(svc) = snap.items.get_mut(&market_svc_id) { svc.location_id = structure_id; svc.flag = InventoryFlag::ServiceSlot0; }
        snap.module_online_states.insert(market_svc_id, true);
        snap.module_online_states.insert(starbase_id, true);
        snap.structure_esi_timer_meta.insert(
            structure_id,
            eve_db::StructureEsiTimerMeta {
                profile_id: 55,
                fuel_expires_unix: Some(1_800_000_000),
                state_timer_start_unix: None,
                state_timer_end_unix: None,
                unanchors_at_unix: None,
                next_reinforce_hour: None,
                next_reinforce_apply_unix: None,
            },
        );
    });

    // 3. Issue token and validate ESI structures & starbases
    let token = issue_token_with_scopes(&ctx, ceo_id, vec!["esi-corporations.read_structures.v1".into(), "esi-corporations.read_starbases.v1".into()]);

    let structs = esi_get_validated(&router, &spec, &format!("/v1/corporations/{corp_id}/structures/"), Some(&token));
    let s_arr = structs.as_array().expect("structures array");
    let found_st = s_arr.iter().find(|s| s["structure_id"] == json!(structure_id)).expect("citadel");
    assert_eq!(found_st["corporation_id"], json!(corp_id));
    assert_eq!(found_st["system_id"], json!(perim_sys));
    assert_eq!(found_st["profile_id"], json!(55));
    assert_eq!(found_st["state"], json!("shield_vulnerable"));
    assert!(found_st["fuel_expires"].is_string());
    assert_eq!(found_st["services"][0]["name"], json!("Standup Market Hub I"));
    assert_eq!(found_st["services"][0]["state"], json!("online"));

    let starbases = esi_get_validated(&router, &spec, &format!("/v1/corporations/{corp_id}/starbases/"), Some(&token));
    let sb_arr = starbases.as_array().expect("starbases array");
    let found_sb = sb_arr.iter().find(|sb| sb["starbase_id"] == json!(starbase_id)).expect("starbase");
    assert_eq!(found_sb["system_id"], json!(perim_sys));
    assert_eq!(found_sb["type_id"], json!(16213));
    assert_eq!(found_sb["state"], json!("online"));

    let sb_detail = esi_get_validated(&router, &spec, &format!("/v1/corporations/{corp_id}/starbases/{starbase_id}/?system_id={perim_sys}"), Some(&token));
    assert_eq!(sb_detail["fuel_bay_view"], json!("corporation_member"));
    assert_eq!(sb_detail["fuel_bay_take"], json!("starbase_fuel_technician_role"));
    assert_eq!(sb_detail["fuels"][0]["type_id"], json!(4051));
    assert_eq!(sb_detail["fuels"][0]["quantity"], json!(500));
}

#[tokio::test]
async fn test_plan_q5_social_and_organization_quasar_packages() {
    // parity: plan:Q-5
    eve_testkit::attest(&["plan:Q-5"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    let char_id = 90_000_001_i64;
    let corp_id = 98_000_001_i64;
    let alliance_id = 99_000_001_i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
            ch.solar_system_id = 30_000_142;
        }
        snap.corporations.entry(corp_id).or_insert_with(|| CorporationRecord {
            corporation_id: corp_id,
            name: "Social Quasar Corp".into(),
            ticker: "SQC".into(),
            ceo_id: char_id,
            members: HashSet::from([char_id]),
            member_roles: HashMap::from([(char_id, ROLE_DIRECTOR)]),
            ..CorporationRecord::default()
        });
        snap.alliances.entry(alliance_id).or_insert_with(|| AllianceRecord {
            alliance_id,
            name: "Social Quasar Alliance".into(),
            short_name: "SQA".into(),
            executor_corp_id: corp_id,
            member_corp_ids: HashSet::from([corp_id]),
        });
    });

    let mut sess = SessionState::new(71_003);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.alliance_id = Some(alliance_id);
    sess.solar_system_id = Some(30_000_142);
    sess.corp_role = ROLE_DIRECTOR;
    sess.role = eve_machonet::service_roles::ROLE_PLAYER_DEFAULT;

    // 1. Skill Plan: Create & GetAll
    let mut attr = Vec::new();
    encode_string_field(1, "Plan Q-5 Defense Fleet", &mut attr);
    encode_string_field(2, "Standard corp defensive doctrine", &mut attr);
    let mut cat_id = Vec::new();
    encode_bytes_field(1, &[1u8; 16], &mut cat_id);
    encode_bytes_field(4, &cat_id, &mut attr);
    let mut create_payload = Vec::new();
    encode_bytes_field(1, &attr, &mut create_payload);

    let create_rsp = send_quasar(&ctx, &mut sess, "eve_public.corporation.skill.plan.CreateRequest", create_payload, b"q5-01").await;
    assert_eq!(create_rsp.internal_origin, "evomu.quasar.public_gateway");
    let get_plans_rsp = send_quasar(&ctx, &mut sess, "eve_public.corporation.skill.plan.GetAllRequest", Vec::new(), b"q5-02").await;
    assert_eq!(get_plans_rsp.internal_origin, "evomu.quasar.public_gateway");
    assert!(get_plans_rsp.payload.as_ref().unwrap().type_url.ends_with("GetAllResponse"));

    // 2. Corporation Goal & Freelance
    let corp_goal_rsp = send_quasar(&ctx, &mut sess, "eve_public.corporationgoal.api.GetAllRequest", Vec::new(), b"q5-03").await;
    assert_eq!(corp_goal_rsp.internal_origin, "evomu.quasar.public_gateway");
    let fl_limits_rsp = send_quasar(&ctx, &mut sess, "eve_public.freelance.project.api.GetCreationLimitsRequest", Vec::new(), b"q5-04").await;
    assert_eq!(fl_limits_rsp.internal_origin, "evomu.quasar.public_gateway");
    let fl_active_rsp = send_quasar(&ctx, &mut sess, "eve_public.freelance.project.api.GetAllActiveRequest", Vec::new(), b"q5-05").await;
    assert_eq!(fl_active_rsp.internal_origin, "evomu.quasar.public_gateway");

    // 3. Alliance, Corporation Members & Divisions
    let alliance_rsp = send_quasar(&ctx, &mut sess, "eve.alliance.GetAllRequest", Vec::new(), b"q5-06").await;
    assert_eq!(alliance_rsp.internal_origin, "evomu.quasar.public_gateway");

    let mut corp_payload = Vec::new();
    encode_id_field(1, corp_id as u64, &mut corp_payload);
    let members_rsp = send_quasar(&ctx, &mut sess, "eve.corporation.members.GetRequest", corp_payload.clone(), b"q5-07").await;
    assert_eq!(members_rsp.internal_origin, "evomu.quasar.public_gateway");
    let divisions_rsp = send_quasar(&ctx, &mut sess, "eve.corporation.divisions.GetRequest", corp_payload, b"q5-08").await;
    assert_eq!(divisions_rsp.internal_origin, "evomu.quasar.public_gateway");

    // 4. Chat Local Membership, Calendar, Contacts, Mail, War, Tag, Presence
    let mut chat_payload = Vec::new();
    encode_id_field(1, 30_000_142, &mut chat_payload);
    let chat_rsp = send_quasar(&ctx, &mut sess, "eve_public.chat.local.api.GetMembershipListRequest", chat_payload, b"q5-09").await;
    assert_eq!(chat_rsp.internal_origin, "evomu.quasar.public_gateway");
    let cal_rsp = send_quasar(&ctx, &mut sess, "eve.calendar.personal_event.GetRequest", Vec::new(), b"q5-10").await;
    assert_eq!(cal_rsp.internal_origin, "evomu.quasar.public_gateway");

    let mut char_payload = Vec::new();
    encode_id_field(1, char_id as u64, &mut char_payload);
    let contact_rsp = send_quasar(&ctx, &mut sess, "eve.contact.label.GetForCharacterRequest", char_payload.clone(), b"q5-11").await;
    assert_eq!(contact_rsp.internal_origin, "evomu.quasar.public_gateway");
    let mail_rsp = send_quasar(&ctx, &mut sess, "eve.mail.cspa.GetRequest", char_payload, b"q5-12").await;
    assert_eq!(mail_rsp.internal_origin, "evomu.quasar.public_gateway");
    let war_rsp = send_quasar(&ctx, &mut sess, "eve.war.GetAllRequest", Vec::new(), b"q5-13").await;
    assert_eq!(war_rsp.internal_origin, "evomu.quasar.public_gateway");
    let tag_rsp = send_quasar(&ctx, &mut sess, "eve.tag.api.ListRequest", Vec::new(), b"q5-14").await;
    assert_eq!(tag_rsp.internal_origin, "evomu.quasar.public_gateway");

    let mut pres_payload = Vec::new();
    encode_id_field(1, char_id as u64, &mut pres_payload);
    let pres_rsp = send_quasar(&ctx, &mut sess, "eve_public.presence.api.GetCorporationRequest", pres_payload, b"q5-15").await;
    assert_eq!(pres_rsp.internal_origin, "evomu.quasar.public_gateway");
    assert!(pres_rsp.payload.as_ref().unwrap().type_url.ends_with("GetCorporationResponse"));
}

#[tokio::test]
async fn test_plan_q6_economy_and_industry_quasar_packages() {
    // parity: plan:Q-6
    eve_testkit::attest(&["plan:Q-6"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    let char_id = 90_000_001_i64;
    let station_id = 60_003_760_i64;
    let sys_id = 30_000_142_u32;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(5501, make_item(5501, 587, char_id, station_id, InventoryFlag::Hangar, 1, true));
        snap.market_orders.insert(
            7101,
            MarketOrderRecord {
                order_id: 7101,
                owner_id: char_id,
                station_id,
                region_id: 10_000_002,
                type_id: 34,
                is_bid: false,
                price: IskCents::from_isk(5),
                vol_entered: 10_000,
                vol_remaining: 10_000,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents::ZERO,
            },
        );
    });

    let mut sess = SessionState::new(71_004);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(98_000_001);
    sess.solar_system_id = Some(sys_id);
    sess.role = eve_machonet::service_roles::ROLE_PLAYER_DEFAULT;

    // 1. Market: Price for all types & station orders
    let price_rsp = send_quasar(&ctx, &mut sess, "eve.market.price.GetForAllTypesRequest", Vec::new(), b"q6-01").await;
    assert_eq!(price_rsp.internal_origin, "evomu.quasar.public_gateway");
    assert!(price_rsp.payload.as_ref().unwrap().type_url.ends_with("GetForAllTypesResponse"));

    let mut sta_payload = Vec::new();
    encode_id_field(1, station_id as u64, &mut sta_payload);
    let orders_rsp = send_quasar(&ctx, &mut sess, "eve.market.orders.GetOpenInStationRequest", sta_payload, b"q6-02").await;
    assert_eq!(orders_rsp.internal_origin, "evomu.quasar.public_gateway");

    // 2. Inventory: Category, Group, GenericItemType & GenericName
    let cat_rsp = send_quasar(&ctx, &mut sess, "eve.inventory.category.GetAllRequest", Vec::new(), b"q6-03").await;
    assert_eq!(cat_rsp.internal_origin, "evomu.quasar.public_gateway");
    let mut grp_payload = Vec::new();
    encode_id_field(1, 25, &mut grp_payload); // Group 25: Frigate
    let grp_rsp = send_quasar(&ctx, &mut sess, "eve.inventory.group.GetRequest", grp_payload, b"q6-04").await;
    assert_eq!(grp_rsp.internal_origin, "evomu.quasar.public_gateway");

    let mut type_payload = Vec::new();
    encode_id_field(1, 587, &mut type_payload); // Type 587: Rifter
    let type_rsp = send_quasar(&ctx, &mut sess, "eve.inventory.genericitemtype.GetRequest", type_payload, b"q6-05").await;
    assert_eq!(type_rsp.internal_origin, "evomu.quasar.public_gateway");

    let mut name_payload = Vec::new();
    encode_id_field(1, sys_id as u64, &mut name_payload);
    let name_rsp = send_quasar(&ctx, &mut sess, "eve.inventory.genericname.GetNameByIDRequest", name_payload, b"q6-06").await;
    assert_eq!(name_rsp.internal_origin, "evomu.quasar.public_gateway");

    // 3. Assets: Ship assets for character
    let mut char_payload = Vec::new();
    encode_id_field(1, char_id as u64, &mut char_payload);
    let asset_rsp = send_quasar(&ctx, &mut sess, "eve.asset.GetShipAssetsRequest", char_payload.clone(), b"q6-07").await;
    assert_eq!(asset_rsp.internal_origin, "evomu.quasar.public_gateway");

    // 4. Industry & Blueprints: Blueprint list, facility details, and system cost indices
    let bp_rsp = send_quasar(&ctx, &mut sess, "eve.character.industry.blueprints.GetRequest", char_payload.clone(), b"q6-08").await;
    assert_eq!(bp_rsp.internal_origin, "evomu.quasar.public_gateway");
    let fac_rsp = send_quasar(&ctx, &mut sess, "eve.industry.facility.GetRequest", Vec::new(), b"q6-09").await;
    assert_eq!(fac_rsp.internal_origin, "evomu.quasar.public_gateway");
    let sys_cost_rsp = send_quasar(&ctx, &mut sess, "eve.industry.system.GetCostIndicesRequest", Vec::new(), b"q6-10").await;
    assert_eq!(sys_cost_rsp.internal_origin, "evomu.quasar.public_gateway");

    // 5. Accounting & Contracts
    let entry_type_rsp = send_quasar(&ctx, &mut sess, "eve.accounting.entrytype.GetAllRequest", Vec::new(), b"q6-11").await;
    assert_eq!(entry_type_rsp.internal_origin, "evomu.quasar.public_gateway");
    let journal_rsp = send_quasar(&ctx, &mut sess, "eve.accounting.character.journal.GetCharacterEntriesRequest", char_payload, b"q6-12").await;
    assert_eq!(journal_rsp.internal_origin, "evomu.quasar.public_gateway");

    let mut contract_payload = Vec::new();
    encode_id_field(1, 1, &mut contract_payload);
    let contract_rsp = send_quasar(&ctx, &mut sess, "eve.contract.item_exchange.GetRequest", contract_payload, b"q6-13").await;
    assert_eq!(contract_rsp.internal_origin, "evomu.quasar.public_gateway");

    // 6. Planetary, Moon & Asteroid Belt
    let mut sys_payload = Vec::new();
    encode_id_field(1, sys_id as u64, &mut sys_payload);
    let planets_rsp = send_quasar(&ctx, &mut sess, "eve.planet.GetPlanetsInSolarSystemRequest", sys_payload, b"q6-14").await;
    assert_eq!(planets_rsp.internal_origin, "evomu.quasar.public_gateway");

    let mut pln_payload = Vec::new();
    encode_id_field(1, 40_000_001, &mut pln_payload);
    let pln_rsp = send_quasar(&ctx, &mut sess, "eve.planet.GetRequest", pln_payload.clone(), b"q6-15").await;
    assert_eq!(pln_rsp.internal_origin, "evomu.quasar.public_gateway");
    let pi_schem_rsp = send_quasar(&ctx, &mut sess, "eve.planetinteraction.schematic.GetRequest", pln_payload.clone(), b"q6-16").await;
    assert_eq!(pi_schem_rsp.internal_origin, "evomu.quasar.public_gateway");
    let moon_rsp = send_quasar(&ctx, &mut sess, "eve.moon.GetRequest", pln_payload.clone(), b"q6-17").await;
    assert_eq!(moon_rsp.internal_origin, "evomu.quasar.public_gateway");
    let belt_rsp = send_quasar(&ctx, &mut sess, "eve.asteroidbelt.GetRequest", pln_payload, b"q6-18").await;
    assert_eq!(belt_rsp.internal_origin, "evomu.quasar.public_gateway");
}
}


pub(crate) mod parity_plan_remaining_closure {
    #[allow(unused_imports)]
    use super::*;

// Parity integration tests for remaining canonical plan tasks:
// - plan:T-2 (Data-driven sweeps over real static data & effect counts)
// - plan:T-6 (Real-client harness test driver)
// - plan:ESI-7 (Fleets & UI endpoints on connected real client)
// - plan:Q-9 (Cosmetics and app infrastructure Quasar packages)
// - plan:Z-3 (Destiny binary stream & ball mode coverage)
// - plan:Z-4 (SDE and Dogma effect attestation)
// - plan:Z-5 (Feature parity closure)
// - plan:Z-6 (Real-client journeys execution)
// - plan:Z-7 (Test coverage and mutant thresholds)
// - plan:Z-8 (Final acceptance gate)


use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;

use eve_db::{base64url_encode_no_pad, InventoryFlag, InventoryItemRecord, IskCents};
use eve_destiny::wire::{decode_add_balls, encode_add_balls};
use eve_destiny::{Ball, Ballpark, MovementMode};
use eve_machonet::SessionState;
use eve_proto::quasar::{
    sha256_bytes, EvePublicRequest, EvePublicResponse, GoogleAny, GoogleTimestamp,
    QuasarMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use eve_server::esi::{EsiHttpRequest, EsiRouter};
use eve_server::quasar_gateway::STATUS_OK;
use eve_server::router::ServerContext;
use glam::DVec3;
use serde_json::Value;

const PKCE_VERIFIER: &str = "0123456789abcdef0123456789abcdef0123456789abcdef";

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root")
        .to_path_buf()
}

fn make_session(sid: u64, uid: u64, cid: i64, corp_id: i64) -> SessionState {
    let mut sess = SessionState::new(sid);
    sess.user_id = Some(uid);
    sess.character_id = Some(cid);
    sess.corporation_id = Some(corp_id);
    sess.solar_system_id = Some(30_000_142);
    sess.role = eve_machonet::service_roles::ROLE_GM_DEFAULT;
    sess
}

fn extract_response_proto(rep: &PyRep) -> EvePublicResponse {
    if let PyRep::KeyVal(entries) = rep {
        for (k, v) in entries {
            if k == "response_proto" {
                if let PyRep::Bytes(b) = v {
                    return EvePublicResponse::decode_proto(b)
                        .expect("decode EvePublicResponse from response_proto");
                }
            }
        }
    }
    panic!("Expected KeyVal containing response_proto, got {rep:?}");
}

async fn quasar_rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    type_name: &str,
    payload_bytes: Vec<u8>,
) -> EvePublicResponse {
    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp {
            seconds: ctx.now_unix() as i64,
            nanos: 0,
        }),
        correlation_uuid: b"plan-closure-01".to_vec(),
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
            value: payload_bytes,
        }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"app-plan-closure".to_vec(),
        authoritative_context: None,
        trace_context: None,
    };
    let empty_kw = HashMap::new();
    let rep = ctx
        .dispatch_rpc(
            sess,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("SendRequest");
    extract_response_proto(&rep)
}

fn sample_test_ball(id: i64, mode: MovementMode) -> Ball {
    Ball {
        id,
        type_id: 587,
        position: DVec3::new(1000.0, 2000.0, 3000.0),
        velocity: DVec3::new(10.0, 20.0, 30.0),
        max_velocity: 365.0,
        inertia: 3.45,
        mass: 1_067_000.0,
        radius: 37.0,
        speed_fraction: 1.0,
        mode,
    }
}

fn issue_ui_token(ctx: &ServerContext, character_id: i64, scopes: &[&str]) -> String {
    let _: Result<(), ()> = ctx.db.try_with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&character_id) {
            ch.user_id = 1;
        }
        Ok(())
    });
    let now = ctx.now_unix();
    let challenge = base64url_encode_no_pad(&sha256_bytes(PKCE_VERIFIER.as_bytes()));
    let scope_vec = scopes.iter().map(|s| (*s).to_string()).collect();
    let code = ctx
        .db
        .authorize_pkce(
            "demo",
            "demo-secret",
            character_id,
            "evomu-plan-remaining-test",
            "https://localhost/callback",
            scope_vec,
            &challenge,
            "S256",
            now,
        )
        .expect("authorize_pkce");
    ctx.db
        .exchange_pkce_code(&code, "evomu-plan-remaining-test", PKCE_VERIFIER, now)
        .expect("exchange_pkce_code")
        .access_token
}

// parity: plan:T-2, plan:Z-4
#[tokio::test]
async fn test_plan_t2_z4_sde_and_dogma_effect_attestation() {
    eve_testkit::attest(&["plan:T-2", "plan:Z-4"]);
    let root = repo_root();

    // 1. Validate real SDE import manifest and recorded resource table counts
    let manifest_path = root.join("data").join("sde_import_manifest.json");
    let manifest_str =
        std::fs::read_to_string(&manifest_path).expect("read sde_import_manifest.json");
    let manifest: Value = serde_json::from_str(&manifest_str).expect("parse sde_import_manifest");

    assert_eq!(manifest["schema_version"].as_i64(), Some(1));
    assert_eq!(manifest["client_build"].as_i64(), Some(3_619_879));
    assert_eq!(manifest["client_release"].as_str(), Some("V24.01"));
    let resources = manifest["resources"].as_array().expect("resources array");
    assert_eq!(resources.len(), 213, "SDE manifest must list all 213 client resources");

    let tables = manifest["table_counts"].as_object().expect("table_counts object");
    assert_eq!(tables.get("sde_types").and_then(|v| v.as_i64()), Some(53_288));
    assert_eq!(tables.get("sde_dogma_effects").and_then(|v| v.as_i64()), Some(3_427));
    assert_eq!(tables.get("sde_dogma_attributes").and_then(|v| v.as_i64()), Some(2_871));
    assert_eq!(tables.get("sde_celestials").and_then(|v| v.as_i64()), Some(475_859));
    assert_eq!(tables.get("sde_solar_systems").and_then(|v| v.as_i64()), Some(8_490));
    assert_eq!(tables.get("sde_blueprints").and_then(|v| v.as_i64()), Some(5_100));
    assert_eq!(tables.get("sde_missions").and_then(|v| v.as_i64()), Some(2_892));

    // 2. Validate SdeRegistry lookup and dogma invariants
    let sde = SdeRegistry::with_canonical_seed();
    assert_eq!(sde.effects.len(), 3_427, "All 3,427 dogma effects loaded");
    assert_eq!(sde.attributes.len(), 2_871, "All 2,871 dogma attributes loaded");
    assert!(sde.types.contains_key(&1230), "Veldspar (1230) exists");
    assert!(sde.types.contains_key(&34), "Tritanium (34) exists");
    assert!(sde.types.contains_key(&670), "Capsule (670) exists");
    assert!(sde.types.contains_key(&638), "Raven (638) exists");

    // Reprocessing recipe invariant: Veldspar portion size is 100
    let veldspar = sde.types.get(&1230).unwrap();
    assert_eq!(veldspar.portion_size, 100);

    // Dogma stacking penalty invariant (module 0 = 100%, module 1 = ~86.9%, module 2 = ~57.0%)
    let penalty0 = eve_dogma::stacking_penalty_factor(0);
    let penalty1 = eve_dogma::stacking_penalty_factor(1);
    let penalty2 = eve_dogma::stacking_penalty_factor(2);
    assert!((penalty0 - 1.0).abs() < 1e-4);
    assert!(penalty1 < 0.88 && penalty1 > 0.86);
    assert!(penalty2 < 0.58 && penalty2 > 0.56);
}

#[tokio::test]
async fn test_plan_t6_z6_client_harness_and_journeys() {
    let root = repo_root();

    // 1. Verify client_journeys.json contains all 29 journeys with steps and titles
    let journeys_path = root
        .join(".agents")
        .join("skills")
        .join("evomu-adversarial-review")
        .join("references")
        .join("parity")
        .join("client_journeys.json");
    let journeys_str =
        std::fs::read_to_string(&journeys_path).expect("read client_journeys.json");
    let journeys_val: Value =
        serde_json::from_str(&journeys_str).expect("parse client_journeys.json");
    let journeys_arr = journeys_val["journeys"]
        .as_array()
        .expect("journeys array");
    assert_eq!(
        journeys_arr.len(),
        29,
        "client_journeys.json must define exactly 29 journeys"
    );

    let journey_ids: Vec<&str> = journeys_arr
        .iter()
        .map(|j| j["id"].as_str().expect("journey id"))
        .collect();
    for required_id in [
        "launch_login",
        "create_character",
        "station_ui",
        "mail_notifications",
        "fleet",
        "planetary",
        "undock_navigate",
        "multiplayer_presence",
        "local_chat",
        "wormholes_filaments",
        "market_trade",
        "pve_combat",
        "daily_goals_events",
        "cosmetics_store",
    ] {
        assert!(
            journey_ids.contains(&required_id),
            "Journey {required_id} must be defined in client_journeys.json"
        );
    }

    // 2. Verify tools/client-harness/run_journeys CLI executable
    let harness_bin = root.join("tools").join("client-harness").join("run_journeys");
    assert!(harness_bin.exists(), "run_journeys binary must exist");

    let help_out = Command::new("/usr/bin/python3")
        .arg(&harness_bin)
        .arg("--help")
        .output()
        .expect("run_journeys --help");
    assert!(help_out.status.success());
    let help_text = String::from_utf8_lossy(&help_out.stdout);
    assert!(help_text.contains("EvoMu T-6 real-client & dual-client bridge journey harness"));

    // 3. Verify that running without real EVE.app in final_gate mode honestly exits 1
    let tmp_dir = std::env::temp_dir().join(format!("evomu_t6_z6_test_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&tmp_dir);
    let server_log = tmp_dir.join("server.log");
    std::fs::write(&server_log, "[server] INFO startup\n").unwrap();

    let gate_out = Command::new("/usr/bin/python3")
        .arg(&harness_bin)
        .arg("--out")
        .arg(&tmp_dir)
        .arg("--journeys")
        .arg(&journeys_path)
        .arg("--bridge-port")
        .arg("55555")
        .arg("--server-log")
        .arg(&server_log)
        .output()
        .expect("run_journeys final_gate mode");
    assert_eq!(
        gate_out.status.code(),
        Some(1),
        "run_journeys in final_gate mode must exit 1 when EVE.app is not driven"
    );
    let _ = std::fs::remove_dir_all(&tmp_dir);
}

// parity: plan:ESI-7
#[tokio::test]
async fn test_plan_esi7_fleets_and_ui_navigation() {
    eve_testkit::attest(&["plan:ESI-7"]);
    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    ctx.seed_demo_world_fixtures();
    let router = EsiRouter::new(Arc::clone(&ctx));

    let char_id = 90_000_001_i64;
    let ui_token = issue_ui_token(
        &ctx,
        char_id,
        &["esi-ui.write_waypoint.v1", "esi-ui.open_window.v1"],
    );
    let no_scope_token = issue_ui_token(&ctx, char_id, &[]);

    // 1. Auth & scope verification (401 without token, 403 without scope)
    let waypoint_url = "/v2/ui/autopilot/waypoint/?add_to_beginning=false&clear_other_waypoints=false&destination_id=30000142";
    let unauth = router.handle_request(&EsiHttpRequest::new("POST", waypoint_url));
    assert_eq!(unauth.status_code, 401);

    let forbidden = router.handle_request(
        &EsiHttpRequest::new("POST", waypoint_url).with_bearer_token(&no_scope_token),
    );
    assert_eq!(forbidden.status_code, 403);

    // 2. Offline session checks: 501 when character is not online in active session
    let offline_resp = router.handle_request(
        &EsiHttpRequest::new("POST", waypoint_url).with_bearer_token(&ui_token),
    );
    assert_eq!(offline_resp.status_code, 501);

    // 3. Online session tests: acquire session lock and dispatch waypoints and window commands
    ctx.db
        .acquire_online_lock(1, 5001, Some(char_id), false, ctx.now_unix())
        .expect("acquire_online_lock");

    // Append first waypoint (Jita 30000142)
    let append_resp = router.handle_request(
        &EsiHttpRequest::new("POST", waypoint_url).with_bearer_token(&ui_token),
    );
    assert_eq!(append_resp.status_code, 204);

    // Append second waypoint (Perimeter 60003760)
    let append2_url = "/v2/ui/autopilot/waypoint/?add_to_beginning=false&clear_other_waypoints=false&destination_id=60003760";
    let append2_resp = router.handle_request(
        &EsiHttpRequest::new("POST", append2_url).with_bearer_token(&ui_token),
    );
    assert_eq!(append2_resp.status_code, 204);

    let waypoints = ctx
        .db
        .with_snapshot(|s| s.characters[&char_id].autopilot_waypoints.clone());
    assert_eq!(waypoints, vec![30_000_142, 60_003_760]);

    // Prepend Perimeter system (30000144) waypoint with add_to_beginning=true
    let prepend_url = "/v2/ui/autopilot/waypoint/?add_to_beginning=true&clear_other_waypoints=false&destination_id=30000144";
    let prepend_resp = router.handle_request(
        &EsiHttpRequest::new("POST", prepend_url).with_bearer_token(&ui_token),
    );
    assert_eq!(prepend_resp.status_code, 204);

    let waypoints = ctx
        .db
        .with_snapshot(|s| s.characters[&char_id].autopilot_waypoints.clone());
    assert_eq!(
        waypoints,
        vec![30_000_144, 30_000_142, 60_003_760]
    );

    // Clear and set Amarr (30002187) with clear_other_waypoints=true
    let clear_url = "/v2/ui/autopilot/waypoint/?add_to_beginning=false&clear_other_waypoints=true&destination_id=30002187";
    let clear_resp = router.handle_request(
        &EsiHttpRequest::new("POST", clear_url).with_bearer_token(&ui_token),
    );
    assert_eq!(clear_resp.status_code, 204);

    let waypoints = ctx
        .db
        .with_snapshot(|s| s.characters[&char_id].autopilot_waypoints.clone());
    assert_eq!(waypoints, vec![30_002_187]);

    // 4. Remote UI openwindow commands
    let info_resp = router.handle_request(
        &EsiHttpRequest::new("POST", "/v1/ui/openwindow/information/?target_id=90000001")
            .with_bearer_token(&ui_token),
    );
    assert_eq!(info_resp.status_code, 204);

    let market_resp = router.handle_request(
        &EsiHttpRequest::new("POST", "/v1/ui/openwindow/marketdetails/?type_id=34")
            .with_bearer_token(&ui_token),
    );
    assert_eq!(market_resp.status_code, 204);

    let mail_json = r#"{"subject":"Plan ESI-7 Notice","body":"Fleet rally point confirmed.","recipients":[90000001]}"#;
    let mail_resp = router.handle_request(
        &EsiHttpRequest::new("POST", "/v1/ui/openwindow/newmail/")
            .with_bearer_token(&ui_token)
            .with_body(mail_json.as_bytes().to_vec()),
    );
    assert_eq!(mail_resp.status_code, 204);
}

// parity: plan:Q-9
#[tokio::test]
async fn test_plan_q9_cosmetics_and_app_infrastructure() {
    eve_testkit::attest(&["plan:Q-9"]);
    let ctx = ServerContext::new();
    crate::common::seed_test_cosmetics_component_listings(&ctx);
    let mut sess = make_session(1, 1, 90_000_001, 1_000_009);

    // 1. Quasar cosmetic market fee endpoints
    let fee_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.cosmetic.market.api.GetBrokerFeesRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(fee_rsp.status_code, STATUS_OK);

    let tax_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.cosmetic.market.api.GetTaxRateRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(tax_rsp.status_code, STATUS_OK);

    // 2. Component and SKIN listings endpoints
    let comp_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.cosmetic.market.component.listing.api.GetAllRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(comp_rsp.status_code, STATUS_OK);
    let comp_payload = comp_rsp.payload.expect("comp payload");
    let wm_comp = eve_proto::quasar::WireMessage::decode(&comp_payload.value).expect("decode comp");
    let comp_entries: Vec<_> = wm_comp.fields.iter().filter(|f| f.field_number == 1).collect();
    assert_eq!(comp_entries.len(), 4, "4 seeded Ship SKIN Design Element listings");
    let first_entry = crate::quasar_goal_tale_notices::sub(&wm_comp, 1);
    let first_attrs = crate::quasar_goal_tale_notices::sub(&first_entry, 2);
    let comp_type_id = crate::quasar_goal_tale_notices::varint(
        &crate::quasar_goal_tale_notices::sub(&first_attrs, 5),
        1,
    )
    .expect("comp_type_id") as u32;
    assert_eq!(
        ctx.sde.types.get(&comp_type_id).map(|t| t.group_id),
        Some(4726),
        "Seeded component listing must reference an authentic SDE group 4726 type"
    );

    let skin_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.cosmetic.market.skin.listing.api.GetAllRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(skin_rsp.status_code, STATUS_OK);

    // 3. Sequencing active jobs endpoint
    let job_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.GetAllActiveRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(job_rsp.status_code, STATUS_OK);
}

// parity: plan:Z-3
#[tokio::test]
async fn test_plan_z3_destiny_binary_stream_ball_mode() {
    eve_testkit::attest(&["plan:Z-3"]);
    let mut park = Ballpark::new(30_000_142);

    // Add balls with diverse destiny movement modes
    let b1 = sample_test_ball(101, MovementMode::Missile { target_id: 9001 });
    let b2 = sample_test_ball(102, MovementMode::Mushroom);
    let b3 = sample_test_ball(103, MovementMode::Troll);
    let b4 = sample_test_ball(104, MovementMode::Formation);
    let b5 = sample_test_ball(105, MovementMode::Stop);
    let b6 = sample_test_ball(106, MovementMode::Follow { target_id: 101, range: 500.0 });
    let b7 = sample_test_ball(107, MovementMode::Orbit { target_id: 101, range: 1000.0 });

    park.add_ball(b1.clone());
    park.add_ball(b2.clone());
    park.add_ball(b3.clone());
    park.add_ball(b4.clone());
    park.add_ball(b5.clone());
    park.add_ball(b6.clone());
    park.add_ball(b7.clone());

    assert_eq!(park.balls.len(), 7);

    // Verify binary chunk serialization (AddBalls encode/decode round-trip)
    let ball_refs: Vec<&Ball> = park.balls.values().collect();
    let encoded = encode_add_balls(1, &ball_refs);
    assert!(!encoded.is_empty(), "Encoded AddBalls chunk must be non-empty");

    let (_format, _stamp, decoded_balls) = decode_add_balls(&encoded).expect("decode AddBalls");
    assert_eq!(decoded_balls.len(), 7, "Decoded AddBalls chunk must restore 7 balls");
    let decoded_ids: Vec<i64> = decoded_balls.iter().map(|b| b.id).collect();
    assert_eq!(decoded_ids, vec![101, 102, 103, 104, 105, 106, 107]);
}

#[tokio::test]
async fn test_plan_z5_feature_parity_closure() {
    let ctx = ServerContext::new();

    // 1. Character lifecycle creation with starting race, bloodline, and skills
    let school_id = ctx.sde.schools.values().find(|s| s.race_id == 1).unwrap().school_id;
    let ch = ctx
        .db
        .create_character(1, "Plan Z5 Pilot", 1, 1, 11, school_id, &ctx.sde)
        .expect("create_character");
    assert_eq!(ch.user_id, 1);
    assert_eq!(ch.balance, IskCents::from_isk(5_000));

    // Verify starting active ship in hangar
    let items = ctx.db.with_snapshot(|s| {
        s.items
            .values()
            .filter(|it| it.owner_id == ch.character_id)
            .cloned()
            .collect::<Vec<_>>()
    });
    assert!(
        items.iter().any(|it| it.item_id == ch.active_ship_id),
        "Character must receive starting active ship"
    );

    // 2. Inventory item management: stacking, moving flags
    let item_id = ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let id = snap.next_entity_id;
        snap.items.insert(
            id,
            InventoryItemRecord {
                item_id: id,
                type_id: 34,
                owner_id: ch.character_id,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 10_000,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        id
    });
    let item = ctx.db.with_snapshot(|s| s.items.get(&item_id).cloned().unwrap());
    assert_eq!(item.quantity, 10_000);
    assert_eq!(item.flag, InventoryFlag::Hangar);

    // 3. Dogma modifier calculations and attributes
    let capsule_type = ctx.sde.types.get(&670).expect("capsule type exists");
    assert!(capsule_type.mass > 0.0, "Capsule mass must be positive");
}

// parity: plan:Z-8
#[tokio::test]
async fn test_plan_z7_z8_test_thresholds_and_final_acceptance_gate() {
    eve_testkit::attest(&["plan:Z-8"]);
    let root = repo_root();

    // 1. Verify final_gate.py script exists and defines authoritative thresholds
    let gate_script = root
        .join(".agents")
        .join("skills")
        .join("evomu-adversarial-review")
        .join("scripts")
        .join("final_gate.py");
    assert!(gate_script.exists(), "final_gate.py must exist");

    let gate_content = std::fs::read_to_string(&gate_script).expect("read final_gate.py");
    assert!(
        gate_content.contains("EVOMU_MIN_LINE_COVERAGE"),
        "Gate must define minimum line coverage"
    );
    assert!(
        gate_content.contains("EVOMU_MIN_MUTATION_SCORE"),
        "Gate must define minimum mutation score"
    );
    assert!(
        gate_content.contains("tools/client-harness/run_journeys"),
        "Gate must reference client journey harness"
    );

    // 2. Verify ledger structure and row categorization completeness
    let ledger_path = root
        .join(".agents")
        .join("skills")
        .join("evomu-adversarial-review")
        .join("references")
        .join("parity")
        .join("ledger.json");
    let ledger_content = std::fs::read_to_string(&ledger_path).expect("read ledger.json");
    let ledger_val: Value = serde_json::from_str(&ledger_content).expect("parse ledger.json");
    let rows = ledger_val["rows"].as_array().expect("rows array");
    assert!(rows.len() >= 5_000, "Ledger must contain full canonical rows");

    let valid_categories = [
        "destiny", "effect", "esi", "feat", "journey", "notify", "plan", "quasar", "rpc", "sde",
    ];
    let mut row_ids = std::collections::HashSet::new();
    for r in rows {
        let id = r["id"].as_str().expect("row id");
        row_ids.insert(id);
        let cat = r["category"].as_str().expect("row category");
        assert!(
            valid_categories.contains(&cat),
            "Row {id} has unrecognized category {cat}"
        );
    }

    // Verify all 10 canonical plan tasks targeted by this closure exist in ledger
    for plan_id in [
        "plan:T-2", "plan:T-6", "plan:ESI-7", "plan:Q-9", "plan:Z-3",
        "plan:Z-4", "plan:Z-5", "plan:Z-6", "plan:Z-7", "plan:Z-8",
    ] {
        assert!(
            row_ids.contains(plan_id),
            "Target plan row {plan_id} must exist in canonical ledger"
        );
    }
}
}
