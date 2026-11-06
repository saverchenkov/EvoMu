//! Comprehensive branch-coverage and mutation-killing tests for `jump_clone_names.rs`.

#![forbid(unsafe_code)]

use crate::services::account_services::account_services_tests::{auth_session, test_ctx};
use crate::services::implant_plugging::DYN_ATTR_BOOSTER_EXPIRY_UNIX;
use crate::services::jump_clone_names::{
    alpha_skill_cap_for_character, effective_character_attributes,
    effective_character_attributes_at, emit_server_skills_changed, emit_server_skills_removed,
    emit_ship_clone_accepted, emit_ship_clone_canceled, emit_station_jump_clone_invalidated,
    extract_skills, get_ship_clone_state, handle_qa_move_me_to,
    is_valid_doll_or_portrait_payload, lookup_effective_attr_from_tuple, name_of,
    serialize_doll_pyrep, set_jump_clone_name, skill_queue_rowset, validate_install_jump_clone,
};
use eve_db::{
    InventoryFlag, InventoryItemRecord, IskCents, JumpCloneRecord, SkillQueueEntry,
    TrainedSkillRecord,
};
use eve_destiny::{Ball, MovementMode};
use eve_proto::PyRep;
use glam::DVec3;
use std::collections::HashMap;

#[test]
fn jump_clone_name_boundaries_validation_skills_and_ship_clone_state() {
    let ctx = test_ctx();
    let char_id = 90_000_001i64;
    let mut sess = auth_session(1, char_id, 98_000_001, 30_000_142);
    sess.station_id = None;
    sess.structure_id = Some(1_000_000_000_001); // docked in structure

    let clone_id = 888_001i64;
    let owned_vat_ship_id = 888_500i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.jump_clones.entry(char_id).or_default().push(JumpCloneRecord {
            jump_clone_id: clone_id,
            character_id: char_id,
            location_id: owned_vat_ship_id,
            implants: vec![],
        });
        snap.items.insert(
            owned_vat_ship_id,
            InventoryItemRecord {
                item_id: owned_vat_ship_id,
                type_id: 28606, // Rorqual
                owner_id: char_id,
                location_id: 30_000_142,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    // Exact 100-character boundary succeeds; 101 fails
    let exact_100 = "A".repeat(100);
    assert_eq!(
        set_jump_clone_name(
            &ctx,
            &sess,
            &[PyRep::Integer(clone_id), PyRep::String(exact_100.clone())]
        )
        .unwrap(),
        PyRep::None
    );
    ctx.db.with_snapshot(|snap| {
        assert_eq!(name_of(snap, clone_id), exact_100);
    });

    // ValidateInstallJumpClone exact 90_000_000 cents boundary and Infomorph skill sum (24242 + 33407 + 73910)
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.balance = IskCents(89_999_999);
    });
    assert_eq!(
        validate_install_jump_clone(&ctx, &sess, &[]).unwrap_err().code,
        "NotEnoughMoney"
    );

    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.balance = IskCents(90_000_000);
        snap.skills.remove(&char_id);
    });
    // 1 existing clone >= 0 max_clones -> JumpCloneLimitReached
    assert_eq!(
        validate_install_jump_clone(&ctx, &sess, &[]).unwrap_err().code,
        "JumpCloneLimitReached"
    );

    // Add level 1 in 33407 (max_clones = 1, still == clone_count 1 -> JumpCloneLimitReached),
    // then add level 1 in 73910 (max_clones = 2 > clone_count 1 -> Ok(true))
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            33407,
            TrainedSkillRecord {
                type_id: 33407,
                level: 1,
                skill_points: 1_000.0,
            },
        );
    });
    assert_eq!(
        validate_install_jump_clone(&ctx, &sess, &[]).unwrap_err().code,
        "JumpCloneLimitReached"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            73910,
            TrainedSkillRecord {
                type_id: 73910,
                level: 1,
                skill_points: 1_000.0,
            },
        );
    });
    assert_eq!(
        validate_install_jump_clone(&ctx, &sess, &[]).unwrap(),
        PyRep::Bool(true)
    );

    // GetShipCloneState on owned non-active ship returns the clone row with exact 100-char label
    let state_rs =
        get_ship_clone_state(&ctx, &sess, &[PyRep::Integer(owned_vat_ship_id)]).unwrap();
    assert!(format!("{state_rs:?}").contains(&exact_100));
}

#[test]
fn doll_serialization_effective_attributes_implants_and_boosters() {
    let ctx = test_ctx();
    let char_id = 90_000_001i64;

    // Doll & portrait payload validation
    assert!(is_valid_doll_or_portrait_payload(&PyRep::keyval([(
        "hair",
        PyRep::Integer(1)
    )])));
    assert!(!is_valid_doll_or_portrait_payload(&PyRep::KeyVal(
        HashMap::new()
    )));
    assert!(is_valid_doll_or_portrait_payload(&PyRep::Dict(vec![(
        PyRep::String("k".into()),
        PyRep::Integer(1)
    )])));
    assert!(!is_valid_doll_or_portrait_payload(&PyRep::Dict(vec![])));
    assert!(is_valid_doll_or_portrait_payload(&PyRep::Object {
        type_name: "utillib.KeyVal".into(),
        args: Box::new(PyRep::None)
    }));
    assert!(is_valid_doll_or_portrait_payload(&PyRep::List(vec![
        PyRep::Integer(1)
    ])));
    assert!(!is_valid_doll_or_portrait_payload(&PyRep::List(vec![])));
    assert!(is_valid_doll_or_portrait_payload(&PyRep::String(
        "portrait:ok".into()
    )));
    assert!(is_valid_doll_or_portrait_payload(&PyRep::String(
        "skin=dark".into()
    )));
    assert!(!is_valid_doll_or_portrait_payload(&PyRep::String(
        "portrait_info_placeholder".into()
    )));
    assert!(!is_valid_doll_or_portrait_payload(&PyRep::String(
        "plain_no_prefix".into()
    )));

    // serialize_doll_pyrep across all PyRep variants
    let kv_rep = PyRep::keyval([
        ("b", PyRep::Bool(true)),
        ("a", PyRep::Real(1.5)),
        ("n", PyRep::None),
    ]);
    assert_eq!(serialize_doll_pyrep(&kv_rep), "a=1.5&b=true&n=");
    let dict_rep = PyRep::Dict(vec![(PyRep::String("x".into()), PyRep::Integer(42))]);
    assert_eq!(serialize_doll_pyrep(&dict_rep), "x=42");
    assert_eq!(
        serialize_doll_pyrep(&PyRep::Object {
            type_name: "C".into(),
            args: Box::new(PyRep::None)
        }),
        "paperdoll"
    );

    // effective_character_attributes_at: unknown character defaults to (19, 20, 20, 20, 20)
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            effective_character_attributes_at(&ctx.sde, snap, 777_777_777, 1000.0),
            (19.0, 20.0, 20.0, 20.0, 20.0)
        );
    });

    // Seed plugged-in Implant item + active Booster item + expired Booster item
    let base_attrs = ctx
        .db
        .with_snapshot(|snap| effective_character_attributes(&ctx.sde, snap, char_id));
    ctx.db.with_mut_snapshot(|snap| {
        snap.service_state_blobs
            .insert("__server_now_unix".into(), "2000.0".into());
        snap.items.insert(
            910_001,
            InventoryItemRecord {
                item_id: 910_001,
                type_id: 9899,
                owner_id: char_id,
                location_id: char_id,
                flag: InventoryFlag::Implant,
                quantity: 1,
                singleton: true,
                dynamic_attributes: HashMap::from([
                    (175, 3.0),
                    (176, 4.0),
                    (177, 5.0),
                    (178, 6.0),
                    (179, 7.0),
                ]),
                version: 1,
                ..Default::default()
            },
        );
        // Active cerebral accelerator (expires at 3000.0 > 2000.0)
        snap.items.insert(
            910_002,
            InventoryItemRecord {
                item_id: 910_002,
                type_id: 9900,
                owner_id: char_id,
                location_id: char_id,
                flag: InventoryFlag::Booster,
                quantity: 1,
                singleton: true,
                dynamic_attributes: HashMap::from([
                    (DYN_ATTR_BOOSTER_EXPIRY_UNIX, 3000.0),
                    (164, 2.0),
                    (165, 2.0),
                    (166, 2.0),
                    (167, 2.0),
                    (168, 2.0),
                ]),
                version: 1,
                ..Default::default()
            },
        );
        // Expired cerebral accelerator (expired at 1500.0 <= 2000.0)
        snap.items.insert(
            910_003,
            InventoryItemRecord {
                item_id: 910_003,
                type_id: 9900,
                owner_id: char_id,
                location_id: char_id,
                flag: InventoryFlag::Booster,
                quantity: 1,
                singleton: true,
                dynamic_attributes: HashMap::from([
                    (DYN_ATTR_BOOSTER_EXPIRY_UNIX, 1500.0),
                    (175, 50.0),
                ]),
                version: 1,
                ..Default::default()
            },
        );
    });

    ctx.db.with_snapshot(|snap| {
        let boosted = effective_character_attributes(&ctx.sde, snap, char_id);
        assert_eq!(boosted.0, base_attrs.0 + 3.0 + 2.0);
        assert_eq!(boosted.1, base_attrs.1 + 4.0 + 2.0);
        assert_eq!(boosted.2, base_attrs.2 + 5.0 + 2.0);
        assert_eq!(boosted.3, base_attrs.3 + 6.0 + 2.0);
        assert_eq!(boosted.4, base_attrs.4 + 7.0 + 2.0);

        for (aid, expected) in [
            (175, boosted.0),
            (176, boosted.1),
            (177, boosted.2),
            (178, boosted.3),
            (179, boosted.4),
            (999, boosted.1),
        ] {
            assert_eq!(lookup_effective_attr_from_tuple(boosted, aid), expected);
        }
    });

    // Notification helpers edge cases (empty slices, self-offerer)
    emit_server_skills_changed(&ctx, char_id, &[], None);
    emit_server_skills_changed(&ctx, char_id, &[999_999], None);
    emit_server_skills_removed(&ctx, char_id, &[]);
    emit_station_jump_clone_invalidated(&ctx, char_id, 60_003_760, false);
    emit_ship_clone_canceled(&ctx, char_id, Some(char_id));
    emit_ship_clone_accepted(&ctx, char_id, char_id, 100);
    assert_eq!(alpha_skill_cap_for_character(&ctx.sde, 999, 999_999_999), 0);
}

#[tokio::test]
async fn extract_skills_guards_prerequisites_queue_reconciliation_and_qa_move() {
    let ctx = test_ctx();
    let char_id = 90_000_001i64;
    let station_id = 60_003_760i64;
    let extractor_id = 920_001i64;
    let mut sess = auth_session(1, char_id, 98_000_001, 30_000_142);
    sess.station_id = Some(station_id);

    // 1. All 5 non-extractable skills rejected
    for non_ext in [24242, 33407, 3411, 2505, 2495] {
        assert_eq!(
            extract_skills(
                &ctx,
                &sess,
                &[PyRep::Integer(non_ext), PyRep::Integer(extractor_id)],
            )
            .unwrap_err()
            .code,
            "SkillCannotBeExtracted"
        );
    }

    // 2. Empty skills dict, duplicate skill entry, and non-positive SP rejected
    assert_eq!(
        extract_skills(
            &ctx,
            &sess,
            &[PyRep::Dict(vec![]), PyRep::Integer(extractor_id)],
        )
        .unwrap_err()
        .code,
        "InvalidArgument"
    );
    assert_eq!(
        extract_skills(
            &ctx,
            &sess,
            &[
                PyRep::Dict(vec![
                    (PyRep::Integer(3300), PyRep::Real(250_000.0)),
                    (PyRep::Integer(3300), PyRep::Real(250_000.0)),
                ]),
                PyRep::Integer(extractor_id),
            ],
        )
        .unwrap_err()
        .code,
        "InvalidSkillPointAmount"
    );
    assert_eq!(
        extract_skills(
            &ctx,
            &sess,
            &[
                PyRep::Dict(vec![(PyRep::Integer(3300), PyRep::Real(-10.0))]),
                PyRep::Integer(extractor_id),
            ],
        )
        .unwrap_err()
        .code,
        "InvalidSkillPointAmount"
    );

    // 3. Undocked session -> NotDocked
    let mut undocked = sess.clone();
    undocked.station_id = None;
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.station_id = None;
        ch.structure_id = None;
    });
    assert_eq!(
        extract_skills(
            &ctx,
            &undocked,
            &[PyRep::Integer(3300), PyRep::Integer(extractor_id)],
        )
        .unwrap_err()
        .code,
        "NotDocked"
    );

    // 4. Dock character, test missing extractor, locked extractor, Alpha clone, wrong total SP, and < 5.5M allocated SP
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.station_id = Some(station_id);
        ch.clone_grade = 1; // Alpha initially
        ch.skill_points = 5_400_000.0;
    });
    assert_eq!(
        extract_skills(
            &ctx,
            &sess,
            &[PyRep::Integer(3300), PyRep::Integer(extractor_id)],
        )
        .unwrap_err()
        .code,
        "MissingSkillExtractor"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            extractor_id,
            InventoryItemRecord {
                item_id: extractor_id,
                type_id: 40519,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Locked,
                quantity: 2,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });
    assert_eq!(
        extract_skills(
            &ctx,
            &sess,
            &[PyRep::Integer(3300), PyRep::Integer(extractor_id)],
        )
        .unwrap_err()
        .code,
        "MissingSkillExtractor"
    );

    // Put extractor in Hangar; Alpha clone rejected -> OmegaCloneRequired
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&extractor_id).unwrap().flag = InventoryFlag::Hangar;
    });
    assert_eq!(
        extract_skills(
            &ctx,
            &sess,
            &[PyRep::Integer(3300), PyRep::Integer(extractor_id)],
        )
        .unwrap_err()
        .code,
        "OmegaCloneRequired"
    );

    // Upgrade to Omega; test wrong total SP (400_000 != 500_000) and < 5,500,000 SP floor
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().clone_grade = 2;
    });
    assert_eq!(
        extract_skills(
            &ctx,
            &sess,
            &[
                PyRep::Dict(vec![(PyRep::Integer(3300), PyRep::Real(400_000.0))]),
                PyRep::Integer(extractor_id),
            ],
        )
        .unwrap_err()
        .code,
        "InvalidSkillPointAmount"
    );
    assert_eq!(
        extract_skills(
            &ctx,
            &sess,
            &[PyRep::Integer(3300), PyRep::Integer(extractor_id)],
        )
        .unwrap_err()
        .code,
        "InsufficientSkillPoints"
    );

    // Give 6,000,000 SP, train skill 3300 (Gunnery) to 500,000 SP, plus dependent skill 3301 (Small Hybrid Turret, requires 3300 lvl 1)
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().skill_points = 6_000_000.0;
        let sk = snap.skills.entry(char_id).or_default();
        sk.insert(
            3300,
            TrainedSkillRecord {
                type_id: 3300,
                level: 5,
                skill_points: 500_000.0,
            },
        );
        sk.insert(
            3301,
            TrainedSkillRecord {
                type_id: 3301,
                level: 1,
                skill_points: 1_000.0,
            },
        );
    });
    // Extracting all 500,000 SP from 3300 drops it to level 0 while 3301 is trained -> SkillPrerequisiteRequired!
    assert_eq!(
        extract_skills(
            &ctx,
            &sess,
            &[PyRep::Integer(3300), PyRep::Integer(extractor_id)],
        )
        .unwrap_err()
        .code,
        "SkillPrerequisiteRequired"
    );

    // Remove dependent skill 3301, add a skill queue entry for 3300, and extract 500,000 SP -> succeeds, mints 40520, decrements extractor stack from 2 to 1!
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.get_mut(&char_id).unwrap().retain(|&k, _| k == 3300);
        snap.skill_queues
            .entry(char_id)
            .or_default()
            .push(SkillQueueEntry {
                type_id: 3300,
                target_level: 5,
                start_sp: 400_000.0,
                end_sp: 500_000.0,
                duration_seconds: 60.0,
                started_at_unix: Some(1_000),
            });
    });
    let _ = skill_queue_rowset(&ctx, char_id);
    let ext_ok = extract_skills(
        &ctx,
        &sess,
        &[PyRep::Integer(3300), PyRep::Integer(extractor_id)],
    )
    .unwrap();
    let inj_id = ext_ok.dict_get("injectorItemID").and_then(|v| v.as_i64()).unwrap();
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&extractor_id].quantity, 1);
        assert_eq!(snap.items[&inj_id].type_id, 40520);
        assert_eq!(snap.characters[&char_id].skill_points, 5_600_000.0);
    });

    // handle_qa_move_me_to with station ID string and active ship ball relocation
    let ship_id = ctx.db.with_snapshot(|s| s.characters[&char_id].active_ship_id);
    let old_sys = ctx.systems.get_or_spawn(30_000_142);
    let _ = old_sys
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 587,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    let moved = handle_qa_move_me_to(
        &ctx,
        &mut sess,
        char_id,
        &[PyRep::String("60008494".into())], // Amarr VIII station -> moves to Amarr solar system 30002187
    )
    .await
    .unwrap();
    assert_eq!(moved.dict_get("moved"), Some(&PyRep::Bool(true)));
    assert_eq!(moved.dict_get("solarSystemID"), Some(&PyRep::Integer(30_002_187)));
}
