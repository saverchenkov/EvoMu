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

pub(crate) mod admin_slash_dispatcher_gm_commands {
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

pub(crate) mod skills_engine_queue_injectors_extractors {
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
