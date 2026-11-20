#![forbid(unsafe_code)]

use super::*;
use crate::services::account_services::account_services_tests::{auth_session, test_ctx};
use eve_db::{IskCents, JumpCloneRecord, TrainedSkillRecord};

#[tokio::test]
async fn jump_clone_names_and_extract_skills_mutations() {
    let ctx = test_ctx();
    let mut sess = auth_session(1, 90_000_001, 98_000_001, 30_000_142);
    sess.station_id = Some(60_003_760);

    let clone_id = 777_001;
    ctx.db.with_mut_snapshot(|snap| {
        snap.jump_clones.entry(90_000_001).or_default().push(JumpCloneRecord {
            jump_clone_id: clone_id,
            character_id: 90_000_001,
            location_id: 60_003_760,
            implants: vec![],
        });
    });

    assert!(set_jump_clone_name(&ctx, &sess, &[]).is_err());
    assert!(set_jump_clone_name(
        &ctx,
        &sess,
        &[PyRep::Integer(clone_id), PyRep::String("x".repeat(101))]
    )
    .is_err());
    assert!(set_jump_clone_name(
        &ctx,
        &sess,
        &[PyRep::Integer(999_999), PyRep::String("My Clone".into())]
    )
    .is_err());

    assert_eq!(
        set_jump_clone_name(
            &ctx,
            &sess,
            &[PyRep::Integer(clone_id), PyRep::String("  Combat Clone  ".into())]
        )
        .unwrap(),
        PyRep::None
    );
    ctx.db.with_snapshot(|snap| {
        assert_eq!(name_of(snap, clone_id), "Combat Clone");
    });

    // Clear name
    assert_eq!(
        set_jump_clone_name(&ctx, &sess, &[PyRep::Integer(clone_id), PyRep::String("".into())]).unwrap(),
        PyRep::None
    );
    ctx.db.with_snapshot(|snap| {
        assert_eq!(name_of(snap, clone_id), "");
    });

    // ValidateInstallJumpClone returns V24.01 list of localization error labels (#637)
    let mut undocked = sess.clone();
    undocked.station_id = None;
    undocked.structure_id = None;
    let undocked_errs = validate_install_jump_clone(&ctx, &undocked, &[]).unwrap();
    assert!(matches!(&undocked_errs, PyRep::List(v) if v.iter().any(|x| x.as_str() == Some("UI/Medical/Errors/JumpCloneValidationNotDocked"))));

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.balance = IskCents(100);
        }
    });
    let funds_errs = validate_install_jump_clone(&ctx, &sess, &[]).unwrap();
    assert!(matches!(&funds_errs, PyRep::List(v) if v.iter().any(|x| matches!(x, PyRep::Tuple(t) if t.first().and_then(|s| s.as_str()) == Some("UI/Medical/Errors/JumpCloneValidationInsufficientFunds")))));

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.balance = IskCents::from_f64_isk(10_000_000.0);
        }
        snap.skills.entry(90_000_001).or_default().insert(
            24242,
            TrainedSkillRecord {
                type_id: 24242,
                level: 5,
                skill_points: 256_000.0,
            },
        );
    });
    assert_eq!(
        validate_install_jump_clone(&ctx, &sess, &[]).unwrap(),
        PyRep::List(vec![])
    );

    // GetShipCloneState
    assert!(get_ship_clone_state(&ctx, &sess, &[PyRep::Integer(999_888_777)]).is_err());
    let ship_id = ctx.db.with_snapshot(|s| s.characters.get(&90_000_001).map(|c| c.active_ship_id).unwrap_or(1));
    sess.ship_id = Some(ship_id);
    assert!(get_ship_clone_state(&ctx, &sess, &[PyRep::Integer(ship_id)]).is_ok());

    // Doll & attribute helpers
    assert!(!is_valid_doll_or_portrait_payload(&PyRep::None));
    assert!(!is_valid_doll_or_portrait_payload(&PyRep::String("doll_info_placeholder".into())));
    assert!(is_valid_doll_or_portrait_payload(&PyRep::String("doll:valid=1".into())));
    assert_eq!(
        serialize_doll_pyrep(&PyRep::Tuple(vec![PyRep::Integer(1), PyRep::String("a".into())])),
        "[1,a]"
    );

    ctx.db.with_snapshot(|snap| {
        let attrs = effective_character_attributes(&ctx.sde, snap, 90_000_001);
        assert!(attrs.0 > 0.0 && attrs.1 > 0.0);
        assert_eq!(lookup_effective_attr_from_tuple(attrs, 164), attrs.0);
        assert_eq!(lookup_effective_attr_from_tuple(attrs, 165), attrs.1);
        assert_eq!(lookup_effective_attr_from_tuple(attrs, 166), attrs.2);
        assert_eq!(lookup_effective_attr_from_tuple(attrs, 167), attrs.3);
        assert_eq!(lookup_effective_attr_from_tuple(attrs, 168), attrs.4);
    });

    assert!(alpha_skill_cap_for_character(&ctx.sde, 1, 3300) <= 5);
    let _ = skill_queue_rowset(&ctx, 90_000_001);
    emit_server_skills_changed(&ctx, 90_000_001, &[24242], Some("Test"));
    emit_server_skills_removed(&ctx, 90_000_001, &[(24242, 4)]);
    emit_station_jump_clone_invalidated(&ctx, 90_000_001, 60_003_760, true);
    emit_ship_clone_offered(&ctx, 90_000_001, 90_000_002, ship_id);
    emit_ship_clone_canceled(&ctx, 90_000_001, Some(90_000_002));
    emit_ship_clone_accepted(&ctx, 90_000_001, 90_000_002, ship_id);

    // ExtractSkills validation
    assert_eq!(
        extract_skills(&ctx, &sess, &[]).unwrap_err().code,
        "MissingSkillExtractor"
    );
    assert_eq!(
        extract_skills(
            &ctx,
            &sess,
            &[
                PyRep::Dict(vec![(PyRep::Integer(24242), PyRep::Real(500_000.0))]),
                PyRep::Integer(12345),
            ],
        )
        .unwrap_err()
        .code,
        "SkillCannotBeExtracted"
    );

    // handle_qa_move_me_to
    assert!(handle_qa_move_me_to(&ctx, &mut sess, 90_000_001, &[PyRep::Integer(999_999_999)])
        .await
        .is_err());
    let moved = handle_qa_move_me_to(&ctx, &mut sess, 90_000_001, &[PyRep::Integer(30000142)])
        .await
        .unwrap();
    assert_eq!(moved.dict_get("solarSystemID"), Some(&PyRep::Integer(30000142)));
}

#[test]
fn emit_server_skills_removed_marks_zero_level_negative_sp() {
    let ctx = test_ctx();
    let char_id = 90_000_001;
    let uid = ctx
        .db
        .with_snapshot(|s| s.characters.get(&char_id).map(|c| c.user_id).unwrap_or(1));
    ctx.db
        .acquire_online_lock(uid, 100, Some(char_id), true, ctx.now_unix())
        .unwrap();
    let _ = ctx.drain_notifications_for_char(char_id);

    // Full removal (level == 0) must emit negative SP so V24 `SkillsSvc._UpdateMySkillsAndReturnLevelsTrained`
    // enters the delete branch instead of inserting/updating `self.myskills[typeID]`.
    emit_server_skills_removed(&ctx, char_id, &[(3369, 0), (24242, 4)]);
    let notifs = ctx.drain_notifications_for_char(char_id);
    let rem = notifs
        .iter()
        .find(|n| n.event == "OnServerSkillsRemoved")
        .expect("OnServerSkillsRemoved emitted");
    let PyRep::Dict(entries) = &rem.args[0] else {
        panic!("skillInfos must be a dict");
    };
    let (_, full_info) = entries
        .iter()
        .find(|(k, _)| *k == PyRep::Integer(3369))
        .expect("full removal entry");
    assert_eq!(full_info.dict_get("skillPoints"), Some(&PyRep::Real(-1.0)));
    assert_eq!(full_info.dict_get("trainedSkillPoints"), Some(&PyRep::Real(-1.0)));
    assert_eq!(full_info.dict_get("virtualSkillLevel"), Some(&PyRep::None));

    let (_, partial_info) = entries
        .iter()
        .find(|(k, _)| *k == PyRep::Integer(24242))
        .expect("partial removal entry");
    assert!(
        partial_info
            .dict_get("trainedSkillPoints")
            .and_then(PyRep::as_f64)
            .unwrap_or(0.0)
            > 0.0
    );
}

#[test]
fn extract_skills_emptying_queue_removes_key_and_passes_sqlite_pre_roundtrip() {
    let ctx = test_ctx();
    let base_now = 1_800_000_000_u64;
    ctx.set_clock(std::sync::Arc::new(
        eve_db::VirtualServerClock::with_fixed_base(base_now),
    ));
    let char_id = 90_000_001_i64;
    let station_id = 60_003_760_i64;
    let mut sess = auth_session(1, char_id, 98_000_001, 30_000_142);
    sess.station_id = Some(station_id);
    sess.structure_id = None;

    let extractor_id = ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.clone_grade = 2;
            ch.skill_points = 7_000_000.0;
            ch.station_id = Some(station_id);
            ch.structure_id = None;
        }
        snap.next_entity_id += 1;
        let eid = snap.next_entity_id;
        snap.items.insert(
            eid,
            eve_db::InventoryItemRecord {
                item_id: eid,
                type_id: 40519,
                owner_id: char_id,
                location_id: station_id,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 2,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        let skills = snap.skills.entry(char_id).or_default();
        skills.clear();
        skills.insert(
            3327,
            TrainedSkillRecord {
                type_id: 3327,
                level: 5,
                skill_points: 256_000.0,
            },
        );
        skills.insert(
            3331,
            TrainedSkillRecord {
                type_id: 3331,
                level: 5,
                skill_points: 512_000.0,
            },
        );
        snap.paused_skill_queues.remove(&char_id);
        snap.skill_queues.insert(
            char_id,
            vec![eve_db::SkillQueueEntry {
                type_id: 3331,
                target_level: 5,
                start_sp: 400_000.0,
                end_sp: 512_000.0,
                duration_seconds: 3_600.0,
                started_at_unix: Some(base_now),
            }],
        );
        eid
    });

    // 1. Extracting 500,000 SP from 3331 drops its level from 5 -> 2, pruning the sole
    //    queued entry (3331 L5) and removing char_id from snap.skill_queues.
    extract_skills(
        &ctx,
        &sess,
        &[
            PyRep::Dict(vec![(PyRep::Integer(3331), PyRep::Real(500_000.0))]),
            PyRep::Integer(extractor_id),
        ],
    )
    .expect("first ExtractSkills succeeds");
    ctx.db.with_snapshot(|snap| {
        assert!(
            !snap.skill_queues.contains_key(&char_id),
            "ExtractSkills pruning the last queued entry must remove char_id from snap.skill_queues"
        );
    });
    assert_eq!(skill_queue_rowset(&ctx, char_id), PyRep::List(vec![]));
    let diffs_direct = eve_testkit::pre_check::check_persist_reload_equivalence(&ctx.db)
        .expect("SQLite PRE check after direct extraction");
    assert!(
        diffs_direct.is_empty(),
        "expected zero SQLite PRE diffs after direct extraction empties queue, got: {diffs_direct:?}"
    );
    let reloaded_direct = ctx
        .db
        .reload_snapshot_from_sqlite()
        .expect("reload snapshot from SQLite after direct extraction");
    assert!(!reloaded_direct.skill_queues.contains_key(&char_id));

    // 2. Extracting prerequisite Spaceship Command (3327 -> level 0) prunes the sole
    //    dependent queued entry Amarr Frigate (3331 L1) and removes char_id from snap.skill_queues.
    ctx.db.with_mut_snapshot(|snap| {
        let skills = snap.skills.entry(char_id).or_default();
        skills.clear();
        skills.insert(
            3327,
            TrainedSkillRecord {
                type_id: 3327,
                level: 5,
                skill_points: 256_000.0,
            },
        );
        skills.insert(
            3300,
            TrainedSkillRecord {
                type_id: 3300,
                level: 5,
                skill_points: 256_000.0,
            },
        );
        snap.skill_queues.insert(
            char_id,
            vec![eve_db::SkillQueueEntry {
                type_id: 3331,
                target_level: 1,
                start_sp: 0.0,
                end_sp: 500.0,
                duration_seconds: 3_600.0,
                started_at_unix: Some(base_now),
            }],
        );
    });
    extract_skills(
        &ctx,
        &sess,
        &[
            PyRep::Dict(vec![
                (PyRep::Integer(3327), PyRep::Real(256_000.0)),
                (PyRep::Integer(3300), PyRep::Real(244_000.0)),
            ]),
            PyRep::Integer(extractor_id),
        ],
    )
    .expect("second ExtractSkills succeeds");
    ctx.db.with_snapshot(|snap| {
        assert!(
            !snap.skill_queues.contains_key(&char_id),
            "ExtractSkills dropping the last prerequisite-dependent queued entry must remove char_id from snap.skill_queues"
        );
    });
    assert_eq!(skill_queue_rowset(&ctx, char_id), PyRep::List(vec![]));
    let diffs_prereq = eve_testkit::pre_check::check_persist_reload_equivalence(&ctx.db)
        .expect("SQLite PRE check after prerequisite extraction");
    assert!(
        diffs_prereq.is_empty(),
        "expected zero SQLite PRE diffs after prerequisite extraction empties queue, got: {diffs_prereq:?}"
    );
    let reloaded_prereq = ctx
        .db
        .reload_snapshot_from_sqlite()
        .expect("reload snapshot from SQLite after prerequisite extraction");
    assert!(!reloaded_prereq.skill_queues.contains_key(&char_id));
}
