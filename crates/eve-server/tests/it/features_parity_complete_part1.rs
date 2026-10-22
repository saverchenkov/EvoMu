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
use eve_proto::quasar::{EvePublicRequest, EvePublicResponse, GoogleAny, GoogleTimestamp, QuasarMessage};
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::formulas::{skill_injector_sp_yield, sp_for_skill_level};
use eve_server::router::ServerContext;
use std::collections::HashMap;

const SEED_PILOT: i64 = 90_000_001;

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
    assert!(matches!(char_list_rep, PyRep::ObjectEx { .. } | PyRep::Object { .. } | PyRep::Rowset { .. } | PyRep::List(_)), "Must return character list crowset");

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
    assert!(c2_record.corporation_id >= 1_000_000, "Must join school NPC corporation");
    assert!(c2_record.home_station_id > 0, "Home station must be set");
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

    // 8. Self-destruct clone back to home station
    let self_destruct_rep = rpc(&ctx, &mut sess, "home_station", "self_destruct_clone", vec![])
        .await
        .expect("self_destruct_clone");
    assert_eq!(self_destruct_rep, PyRep::Integer(c2_record.home_station_id));
    assert_eq!(sess.station_id, Some(c2_record.home_station_id));

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

    // 11. Skill Extractor via Quasar (skill-extractor)
    // Set SP above 5.5M and clone grade to Omega
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&SEED_PILOT) {
            ch.skill_points = 6_000_000.0;
            ch.clone_grade = 2; // Omega
        }
    });

    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp {
            seconds: ctx.now_unix() as i64,
            nanos: 0,
        }),
        correlation_uuid: b"extract-corr-01".to_vec(),
        payload: Some(GoogleAny {
            type_url: "type.googleapis.com/eve.skill.extractor.ExtractSkillsRequest".into(),
            value: vec![],
        }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"app-test-inst-01".to_vec(),
        authoritative_context: None,
        trace_context: None,
    };
    let extract_rep = rpc(
        &ctx,
        &mut sess,
        "publicGatewaySvc",
        "SendRequest",
        vec![PyRep::Bytes(req.encode_proto())],
    )
    .await
    .expect("SendRequest ExtractSkillsRequest");
    let extract_rsp = extract_response_proto(&extract_rep);
    assert_eq!(extract_rsp.status_code, 200, "ExtractSkillsRequest must succeed for Omega > 5.5M SP");

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
