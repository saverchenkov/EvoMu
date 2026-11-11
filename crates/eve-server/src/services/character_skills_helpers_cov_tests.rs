//! Targeted unit tests covering branches in `services::character_skills_helpers`.

use super::{char_session, test_ctx};
use super::super::character_skills_helpers::handle_skill_tail;
use crate::services::implant_plugging::{
    save_alpha_injector_cooldown, ALPHA_INJECTOR_COOLDOWN_TICKS, TYPE_DAILY_ALPHA_INJECTOR,
    TYPE_LARGE_SKILL_INJECTOR, TYPE_SMALL_SKILL_INJECTOR,
};
use eve_db::{
    InventoryFlag, InventoryItemRecord, SkillQueueEntry, TrainedSkillRecord,
};
use eve_proto::PyRep;
use eve_sde::{attr_ids, TypeId};
use std::collections::HashMap;

fn call_tail(
    ctx: &crate::router::ServerContext,
    sess: &mut eve_machonet::SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Option<Result<PyRep, eve_proto::BridgeRpcError>> {
    handle_skill_tail(ctx, sess, service, method, args, &HashMap::new())
}

#[test]
fn tail_dispatch_routes_known_and_unknown_methods() {
    let ctx = test_ctx();
    let mut sess = char_session(90_000_001, 60_003_760);

    assert!(call_tail(&ctx, &mut sess, "skillMgr2", "GetImplants", &[])
        .expect("GetImplants")
        .is_ok());
    assert!(call_tail(&ctx, &mut sess, "skillMgr2", "GetBoosters", &[])
        .expect("GetBoosters")
        .is_ok());
    assert!(
        call_tail(&ctx, &mut sess, "skillMgr2", "GetSkillChangesForISIS", &[])
            .expect("GetSkillChangesForISIS")
            .is_ok()
    );
    assert!(call_tail(&ctx, &mut sess, "dogmaIM", "InjectImplant", &[])
        .expect("InjectImplant")
        .is_err());
    assert!(call_tail(&ctx, &mut sess, "dogmaIM", "DestroyImplant", &[])
        .expect("DestroyImplant")
        .is_err());
    assert!(call_tail(&ctx, &mut sess, "dogmaIM", "UseBooster", &[])
        .expect("UseBooster")
        .is_err());
    assert!(call_tail(&ctx, &mut sess, "", "", &[]).is_none());
}

#[test]
fn free_sp_skills_validation_prereqs_and_apply_branches() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let mut sess = char_session(char_id, 60_003_760);
    let gunnery = 3300_u32;
    let small_proj = 3301_u32;

    // parse_skills_and_levels_arg branches via GetFreeSkillPointsAppliedToSkills & ApplyFreeSkillPointsToSkills
    assert_eq!(
        call_tail(&ctx, &mut sess, "skillMgr2", "GetFreeSkillPointsAppliedToSkills", &[])
            .unwrap()
            .unwrap(),
        PyRep::Dict(Vec::new())
    );
    assert_eq!(
        call_tail(
            &ctx,
            &mut sess,
            "skillMgr2",
            "GetFreeSkillPointsAppliedToSkills",
            &[PyRep::None],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidArgument"
    );
    assert_eq!(
        call_tail(
            &ctx,
            &mut sess,
            "skillMgr2",
            "GetFreeSkillPointsAppliedToSkills",
            &[PyRep::List(vec![PyRep::Integer(3300)])],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidArgument"
    );
    assert_eq!(
        call_tail(
            &ctx,
            &mut sess,
            "skillMgr2",
            "GetFreeSkillPointsAppliedToSkills",
            &[PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(0),
                PyRep::Integer(1),
            ])])],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidSkillType"
    );
    assert_eq!(
        call_tail(
            &ctx,
            &mut sess,
            "skillMgr2",
            "GetFreeSkillPointsAppliedToSkills",
            &[PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(3300),
                PyRep::Integer(6),
            ])])],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidSkillLevel"
    );

    // Apply with empty targets -> InvalidArgument
    assert_eq!(
        call_tail(
            &ctx,
            &mut sess,
            "skillMgr2",
            "ApplyFreeSkillPointsToSkills",
            &[PyRep::Dict(Vec::new())],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidArgument"
    );

    // Apply with zero free SP -> InsufficientFreeSP
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.clone_grade = 2;
            ch.free_skill_points = 0.0;
        }
        snap.skills.entry(char_id).or_default().insert(
            gunnery,
            TrainedSkillRecord {
                type_id: gunnery,
                level: 0,
                skill_points: 0.0,
            },
        );
    });
    assert_eq!(
        call_tail(
            &ctx,
            &mut sess,
            "skillMgr2",
            "ApplyFreeSkillPointsToSkills",
            &[PyRep::Dict(vec![(PyRep::Integer(3300), PyRep::Integer(1))])],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InsufficientFreeSP"
    );

    // Alpha clone >= 5M total SP -> AlphaCloneSpCapExceeded
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.clone_grade = 1;
            ch.free_skill_points = 5_000.0;
            ch.skill_points = 5_000_100.0;
        }
    });
    assert_eq!(
        call_tail(
            &ctx,
            &mut sess,
            "skillMgr2",
            "ApplyFreeSkillPointsToSkills",
            &[PyRep::Dict(vec![(PyRep::Integer(3300), PyRep::Integer(1))])],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "AlphaCloneSpCapExceeded"
    );

    // Unknown/uninjected skill & prerequisite check
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.clone_grade = 2;
            ch.skill_points = 0.0;
            ch.free_skill_points = 10_000.0;
        }
        let m = snap.skills.entry(char_id).or_default();
        m.remove(&small_proj);
        m.insert(
            gunnery,
            TrainedSkillRecord {
                type_id: gunnery,
                level: 0,
                skill_points: 0.0,
            },
        );
    });
    assert_eq!(
        call_tail(
            &ctx,
            &mut sess,
            "skillMgr2",
            "ApplyFreeSkillPointsToSkills",
            &[PyRep::Dict(vec![(
                PyRep::Integer(small_proj as i64),
                PyRep::Integer(1),
            )])],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CannotApplyFreePointsDoNotHaveSkill"
    );

    // Non-skill type -> InvalidSkillType, then skill with Gunnery prerequisite -> SkillPrerequisiteNotMet
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            34,
            TrainedSkillRecord {
                type_id: 34,
                level: 0,
                skill_points: 0.0,
            },
        );
    });
    assert_eq!(
        call_tail(
            &ctx,
            &mut sess,
            "skillMgr2",
            "ApplyFreeSkillPointsToSkills",
            &[PyRep::Dict(vec![(PyRep::Integer(34), PyRep::Integer(1))])],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidSkillType"
    );

    let prereq_skill = ctx
        .sde
        .types
        .values()
        .filter(|t| {
            t.category_id == 16
                && t.published
                && t.default_attributes
                    .get(&attr_ids::REQUIRED_SKILL_1)
                    .copied()
                    .map(|r| r as TypeId == gunnery)
                    .unwrap_or(false)
        })
        .map(|t| t.type_id)
        .min();
    if let Some(req_tid) = prereq_skill {
        ctx.db.with_mut_snapshot(|snap| {
            snap.skills.entry(char_id).or_default().insert(
                req_tid,
                TrainedSkillRecord {
                    type_id: req_tid,
                    level: 0,
                    skill_points: 0.0,
                },
            );
        });
        assert_eq!(
            call_tail(
                &ctx,
                &mut sess,
                "skillMgr2",
                "ApplyFreeSkillPointsToSkills",
                &[PyRep::Dict(vec![(
                    PyRep::Integer(req_tid as i64),
                    PyRep::Integer(1),
                )])],
            )
            .unwrap()
            .unwrap_err()
            .code,
            "SkillPrerequisiteNotMet"
        );
    }

    // Apply free SP to skill partially in queue + completing queue entry 1
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.clone_grade = 2;
            ch.free_skill_points = 500.0;
            ch.skill_points = 0.0;
        }
        snap.skill_queues.insert(
            char_id,
            vec![
                SkillQueueEntry {
                    type_id: gunnery,
                    target_level: 1,
                    started_at_unix: None,
                    duration_seconds: 100.0,
                    start_sp: 0.0,
                    end_sp: 250.0,
                },
                SkillQueueEntry {
                    type_id: gunnery,
                    target_level: 2,
                    started_at_unix: None,
                    duration_seconds: 1000.0,
                    start_sp: 250.0,
                    end_sp: 1415.0,
                },
            ],
        );
    });
    let est = call_tail(
        &ctx,
        &mut sess,
        "skillMgr2",
        "GetFreeSkillPointsAppliedToSkills",
        &[PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Integer(gunnery as i64),
            PyRep::Integer(2),
        ])])],
    )
    .unwrap()
    .expect("GetFreeSkillPointsAppliedToSkills");
    assert!(matches!(est, PyRep::Dict(ref d) if !d.is_empty()));

    let apply_res = call_tail(
        &ctx,
        &mut sess,
        "skillMgr2",
        "ApplyFreeSkillPointsToSkills",
        &[PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Integer(gunnery as i64),
            PyRep::Integer(2),
        ])])],
    )
    .unwrap();
    assert!(apply_res.is_ok());

    ctx.db.with_snapshot(|snap| {
        let ch = &snap.characters[&char_id];
        assert!((ch.free_skill_points - 0.0).abs() < 1e-6);
        assert!((ch.skill_points - 500.0).abs() < 1e-6);
        let q = &snap.skill_queues[&char_id];
        // Target level 1 finished (removed), target level 2 updated start_sp = 500.0
        assert_eq!(q.len(), 1);
        assert_eq!(q[0].target_level, 2);
        assert!((q[0].start_sp - 500.0).abs() < 1e-6);
        assert!(q[0].duration_seconds < 1000.0);
        assert!(snap
            .skill_history
            .get(&char_id)
            .is_some_and(|h| h.iter().any(|e| e.event_type_id == 307)));
    });
}

#[test]
fn free_sp_queue_get_and_apply_cover_alpha_skip_partial_and_errors() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let mut sess = char_session(char_id, 60_003_760);
    let gunnery = 3300_u32;

    // Zero budget -> empty Dict on Get, InsufficientFreeSP on Apply
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.clone_grade = 1;
            ch.free_skill_points = 0.0;
            ch.skill_points = 0.0;
        }
    });
    assert_eq!(
        call_tail(&ctx, &mut sess, "skillMgr2", "GetFreeSkillPointsAppliedToQueue", &[])
            .unwrap()
            .unwrap(),
        PyRep::Dict(Vec::new())
    );
    assert_eq!(
        call_tail(&ctx, &mut sess, "skillMgr2", "ApplyFreeSkillPointsToQueue", &[])
            .unwrap()
            .unwrap_err()
            .code,
        "InsufficientFreeSP"
    );

    // Alpha cap reached -> AlphaCloneSpCapExceeded
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.clone_grade = 1;
            ch.free_skill_points = 1000.0;
            ch.skill_points = 5_000_000.0;
        }
    });
    assert_eq!(
        call_tail(&ctx, &mut sess, "skillMgr2", "ApplyFreeSkillPointsToQueue", &[])
            .unwrap()
            .unwrap_err()
            .code,
        "AlphaCloneSpCapExceeded"
    );

    // Alpha clone with capped entry skipped and partial entry updated
    let capped_tid = ctx.db.with_mut_snapshot(|snap| {
        let race_id = snap.characters.get(&char_id).map(|c| c.race_id).unwrap_or(1);
        let omega_tid = ctx
            .sde
            .types
            .values()
            .filter(|t| t.category_id == 16 && t.published)
            .map(|t| t.type_id)
            .filter(|&tid| {
                crate::services::character_skills::alpha_skill_cap_for_character(
                    &ctx.sde, race_id, tid,
                ) < 5
            })
            .min()
            .unwrap_or(gunnery);
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.clone_grade = 1;
            ch.free_skill_points = 600.0;
            ch.skill_points = 0.0;
        }
        snap.skills.entry(char_id).or_default().insert(
            gunnery,
            TrainedSkillRecord {
                type_id: gunnery,
                level: 0,
                skill_points: 0.0,
            },
        );
        snap.skill_queues.insert(
            char_id,
            vec![
                SkillQueueEntry {
                    type_id: omega_tid,
                    target_level: 5,
                    started_at_unix: None,
                    duration_seconds: 5000.0,
                    start_sp: 45_255.0,
                    end_sp: 256_000.0,
                },
                SkillQueueEntry {
                    type_id: gunnery,
                    target_level: 1,
                    started_at_unix: None,
                    duration_seconds: 100.0,
                    start_sp: 0.0,
                    end_sp: 250.0,
                },
                SkillQueueEntry {
                    type_id: gunnery,
                    target_level: 2,
                    started_at_unix: None,
                    duration_seconds: 1000.0,
                    start_sp: 250.0,
                    end_sp: 1415.0,
                },
            ],
        );
        omega_tid
    });

    let get_res = call_tail(&ctx, &mut sess, "skillMgr2", "GetFreeSkillPointsAppliedToQueue", &[])
        .unwrap()
        .expect("GetFreeSkillPointsAppliedToQueue");
    assert_eq!(
        get_res,
        PyRep::Dict(vec![(PyRep::Integer(gunnery as i64), PyRep::Integer(600))])
    );

    assert!(
        call_tail(&ctx, &mut sess, "skillMgr2", "ApplyFreeSkillPointsToQueue", &[])
            .unwrap()
            .is_ok()
    );
    ctx.db.with_snapshot(|snap| {
        let q = &snap.skill_queues[&char_id];
        // omega_tid level 5 skipped, level 1 completed & removed, level 2 partially applied (250 + 350 = 600)
        assert_eq!(q.len(), 2);
        assert_eq!(q[0].type_id, capped_tid);
        assert_eq!(q[1].target_level, 2);
        assert!((q[1].start_sp - 600.0).abs() < 1e-6);
    });
}

#[test]
fn char_start_training_skill_covers_item_lookup_caps_reordering_and_mct() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let other_char_id = 90_000_002_i64;
    let mut sess = char_session(char_id, 60_003_760);
    let gunnery = 3300_u32;

    // Missing/zero arg -> InvalidArgument
    assert_eq!(
        call_tail(&ctx, &mut sess, "skillMgr2", "CharStartTrainingSkill", &[])
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidArgument"
    );

    // > u32::MAX not in items -> SkillNotInjected
    assert_eq!(
        call_tail(
            &ctx,
            &mut sess,
            "skillMgr2",
            "CharStartTrainingSkill",
            &[PyRep::Integer(i64::from(u32::MAX) + 50)],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "SkillNotInjected"
    );

    // AlphaSkillCapExceeded & SkillAlreadyAtMaxLevel
    let mut skill_item_id = 0_i64;
    ctx.db.with_mut_snapshot(|snap| {
        let user_id = snap.characters[&char_id].user_id;
        let mut other = snap.characters[&char_id].clone();
        other.character_id = other_char_id;
        other.user_id = user_id;
        other.name = "Alt Char".into();
        snap.characters.insert(other_char_id, other);

        snap.next_entity_id += 1;
        skill_item_id = snap.next_entity_id;
        snap.items.insert(
            skill_item_id,
            InventoryItemRecord {
                item_id: skill_item_id,
                type_id: gunnery,
                owner_id: char_id,
                location_id: char_id,
                flag: InventoryFlag::Skill,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.clone_grade = 1;
        }
        let cap = crate::services::character_skills::alpha_skill_cap_for_character(
            &ctx.sde,
            snap.characters[&char_id].race_id,
            gunnery,
        );
        snap.skills.entry(char_id).or_default().insert(
            gunnery,
            TrainedSkillRecord {
                type_id: gunnery,
                level: cap,
                skill_points: 100_000.0,
            },
        );
    });

    let err_cap = call_tail(
        &ctx,
        &mut sess,
        "skillMgr2",
        "CharStartTrainingSkill",
        &[PyRep::Integer(skill_item_id)],
    )
    .unwrap()
    .unwrap_err();
    assert!(matches!(
        err_cap.code.as_str(),
        "AlphaSkillCapExceeded" | "SkillAlreadyAtMaxLevel"
    ));

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.clone_grade = 2;
        }
        snap.skills.entry(char_id).or_default().insert(
            gunnery,
            TrainedSkillRecord {
                type_id: gunnery,
                level: 5,
                skill_points: 256_000.0,
            },
        );
    });
    assert_eq!(
        call_tail(
            &ctx,
            &mut sess,
            "skillMgr2",
            "CharStartTrainingSkill",
            &[PyRep::Integer(gunnery as i64)],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "SkillAlreadyAtMaxLevel"
    );

    // Active MCT slot prevents pausing other_char_id; without MCT pauses other_char_id
    let now_unix = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        let user_id = snap.characters[&char_id].user_id;
        snap.skills.entry(char_id).or_default().insert(
            gunnery,
            TrainedSkillRecord {
                type_id: gunnery,
                level: 1,
                skill_points: 250.0,
            },
        );
        snap.paused_skill_queues.insert(char_id);
        snap.paused_skill_queues.remove(&other_char_id);
        snap.skill_queues.insert(
            char_id,
            vec![
                SkillQueueEntry {
                    type_id: 3413,
                    target_level: 1,
                    started_at_unix: Some(now_unix),
                    duration_seconds: 100.0,
                    start_sp: 0.0,
                    end_sp: 250.0,
                },
                SkillQueueEntry {
                    type_id: gunnery,
                    target_level: 2,
                    started_at_unix: None,
                    duration_seconds: 500.0,
                    start_sp: 250.0,
                    end_sp: 1415.0,
                },
            ],
        );
        snap.cluster_and_runtime.mct_slots.insert(user_id, vec![now_unix + 3600]);
    });
    assert!(call_tail(
        &ctx,
        &mut sess,
        "skillMgr2",
        "CharStartTrainingSkill",
        &[PyRep::Integer(skill_item_id)],
    )
    .unwrap()
    .is_ok());

    ctx.db.with_snapshot(|snap| {
        let q = &snap.skill_queues[&char_id];
        assert_eq!(q[0].type_id, gunnery);
        assert!(q[1].started_at_unix.is_none());
        assert!(!snap.paused_skill_queues.contains(&other_char_id));
        assert!(!snap.paused_skill_queues.contains(&char_id));
    });

    // Clear MCT -> pauses other_char_id
    ctx.db.with_mut_snapshot(|snap| {
        let user_id = snap.characters[&char_id].user_id;
        snap.cluster_and_runtime.mct_slots.remove(&user_id);
    });
    assert!(call_tail(
        &ctx,
        &mut sess,
        "skillMgr2",
        "CharStartTrainingSkill",
        &[PyRep::Integer(gunnery as i64)],
    )
    .unwrap()
    .is_ok());
    ctx.db.with_snapshot(|snap| {
        assert!(snap.paused_skill_queues.contains(&other_char_id));
    });
}

#[test]
fn skill_injectors_constraints_split_combine_and_diminished_sp() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let st_id = 60_003_760_i64;
    let mut sess = char_session(char_id, st_id);

    // CheckInjectionConstraints argument & validation errors
    assert_eq!(
        call_tail(&ctx, &mut sess, "skillMgr2", "CheckInjectionConstraints", &[])
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    assert_eq!(
        call_tail(
            &ctx,
            &mut sess,
            "skillMgr2",
            "CheckInjectionConstraints",
            &[PyRep::Integer(1), PyRep::Integer(0)],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidSkillPointAmount"
    );
    assert_eq!(
        call_tail(
            &ctx,
            &mut sess,
            "skillMgr2",
            "CheckInjectionConstraints",
            &[PyRep::Integer(999_999_888), PyRep::Integer(1)],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InjectorNotFound"
    );

    let now_ft = ctx.now_blueos_filetime_ticks();
    let mut other_owner_id = 0_i64;
    let mut singleton_inj_id = 0_i64;
    let mut bad_type_id = 0_i64;
    let mut daily_alpha_id = 0_i64;
    let mut large_inj_id = 0_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.retain(|_, it| {
            !(it.owner_id == char_id
                && (it.type_id == TYPE_LARGE_SKILL_INJECTOR
                    || it.type_id == TYPE_SMALL_SKILL_INJECTOR))
        });
        let mut alloc = |tid: u32, owner: i64, qty: i32, singleton: bool| {
            snap.next_entity_id += 1;
            let id = snap.next_entity_id;
            snap.items.insert(
                id,
                InventoryItemRecord {
                    item_id: id,
                    type_id: tid,
                    owner_id: owner,
                    location_id: st_id,
                    flag: InventoryFlag::Hangar,
                    quantity: qty,
                    singleton,
                    ..Default::default()
                },
            );
            id
        };
        other_owner_id = alloc(TYPE_LARGE_SKILL_INJECTOR, 999_999, 1, false);
        singleton_inj_id = alloc(TYPE_LARGE_SKILL_INJECTOR, char_id, 1, true);
        bad_type_id = alloc(34, char_id, 10, false);
        daily_alpha_id = alloc(TYPE_DAILY_ALPHA_INJECTOR, char_id, 5, false);
        large_inj_id = alloc(TYPE_LARGE_SKILL_INJECTOR, char_id, 2, false);

        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.clone_grade = 2; // Omega
        }
    });

    assert_eq!(
        call_tail(
            &ctx,
            &mut sess,
            "skillMgr2",
            "CheckInjectionConstraints",
            &[PyRep::Integer(other_owner_id), PyRep::Integer(1)],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InjectorNotAccessible"
    );
    assert_eq!(
        call_tail(
            &ctx,
            &mut sess,
            "skillMgr2",
            "CheckInjectionConstraints",
            &[PyRep::Integer(singleton_inj_id), PyRep::Integer(1)],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InsufficientInjectors"
    );
    assert_eq!(
        call_tail(
            &ctx,
            &mut sess,
            "skillMgr2",
            "CheckInjectionConstraints",
            &[PyRep::Integer(bad_type_id), PyRep::Integer(1)],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidSkillInjector"
    );
    assert_eq!(
        call_tail(
            &ctx,
            &mut sess,
            "skillMgr2",
            "CheckInjectionConstraints",
            &[PyRep::Integer(daily_alpha_id), PyRep::Integer(1)],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "AlphaCloneRequired"
    );

    // Alpha clone constraints: qty != 1, cooldown active, then clear cooldown -> Ok
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.clone_grade = 1;
        }
    });
    assert_eq!(
        call_tail(
            &ctx,
            &mut sess,
            "skillMgr2",
            "CheckInjectionConstraints",
            &[PyRep::Integer(daily_alpha_id), PyRep::Integer(2)],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "DailyAlphaInjectorCooldownActive"
    );

    ctx.db.with_mut_snapshot(|snap| {
        save_alpha_injector_cooldown(snap, char_id, now_ft - ALPHA_INJECTOR_COOLDOWN_TICKS / 2);
    });
    assert_eq!(
        call_tail(
            &ctx,
            &mut sess,
            "skillMgr2",
            "CheckInjectionConstraints",
            &[PyRep::Integer(daily_alpha_id), PyRep::Integer(1)],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "DailyAlphaInjectorCooldownActive"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.owner_notes.clear();
        snap.item_custom_names.clear();
        snap.service_state_blobs.clear();
    });
    assert_eq!(
        call_tail(
            &ctx,
            &mut sess,
            "skillMgr2",
            "CheckInjectionConstraints",
            &[PyRep::Integer(daily_alpha_id), PyRep::Integer(1)],
        )
        .unwrap()
        .unwrap(),
        PyRep::None
    );

    // SplitSkillInjector: errors + partial split -> full split merging into small stack
    assert_eq!(
        call_tail(
            &ctx,
            &mut sess,
            "skillMgr2",
            "SplitSkillInjector",
            &[PyRep::Integer(daily_alpha_id), PyRep::Integer(1)],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidSkillInjector"
    );
    assert_eq!(
        call_tail(
            &ctx,
            &mut sess,
            "skillMgr2",
            "SplitSkillInjector",
            &[PyRep::Integer(large_inj_id), PyRep::Integer(i64::MAX)],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidSkillPointAmount"
    );
    assert!(call_tail(
        &ctx,
        &mut sess,
        "skillMgr2",
        "SplitSkillInjector",
        &[PyRep::Integer(large_inj_id), PyRep::Integer(1)],
    )
    .unwrap()
    .is_ok());
    // Second split consumes remaining 1 large and merges 5 more small into the 5-small stack = 10
    assert!(call_tail(
        &ctx,
        &mut sess,
        "skillMgr2",
        "SplitSkillInjector",
        &[PyRep::Integer(large_inj_id), PyRep::Integer(1)],
    )
    .unwrap()
    .is_ok());

    let small_id = ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&large_inj_id));
        snap.items
            .values()
            .find(|it| {
                it.owner_id == char_id
                    && it.location_id == st_id
                    && it.type_id == TYPE_SMALL_SKILL_INJECTOR
            })
            .map(|it| {
                assert_eq!(it.quantity, 10);
                it.item_id
            })
            .expect("small stack created")
    });

    // CombineSkillInjector: errors + partial combine -> full combine into existing large stack
    assert_eq!(
        call_tail(
            &ctx,
            &mut sess,
            "skillMgr2",
            "CombineSkillInjector",
            &[PyRep::Integer(small_id), PyRep::Integer(4)],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CombineSkillInjectorTooFewInjectors"
    );
    assert_eq!(
        call_tail(
            &ctx,
            &mut sess,
            "skillMgr2",
            "CombineSkillInjector",
            &[PyRep::Integer(daily_alpha_id), PyRep::Integer(5)],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidSkillInjector"
    );
    assert!(call_tail(
        &ctx,
        &mut sess,
        "skillMgr2",
        "CombineSkillInjector",
        &[PyRep::Integer(small_id), PyRep::Integer(5)],
    )
    .unwrap()
    .is_ok());
    assert!(call_tail(
        &ctx,
        &mut sess,
        "skillMgr2",
        "CombineSkillInjector",
        &[PyRep::Integer(small_id), PyRep::Integer(5)],
    )
    .unwrap()
    .is_ok());

    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&small_id));
        let large_qty = snap
            .items
            .values()
            .filter(|it| {
                !it.singleton
                    && it.owner_id == char_id
                    && it.location_id == st_id
                    && it.type_id == TYPE_LARGE_SKILL_INJECTOR
            })
            .map(|it| it.quantity)
            .sum::<i32>();
        assert_eq!(large_qty, 2);
    });

    // GetDiminishedSpFromInjectors: qty 0, fixed non-diminishing, small/large + non_dim_remaining
    assert_eq!(
        call_tail(
            &ctx,
            &mut sess,
            "skillMgr2",
            "GetDiminishedSpFromInjectors",
            &[
                PyRep::Integer(TYPE_LARGE_SKILL_INJECTOR as i64),
                PyRep::Integer(0),
            ],
        )
        .unwrap()
        .unwrap(),
        PyRep::Integer(0)
    );
    assert_eq!(
        call_tail(
            &ctx,
            &mut sess,
            "skillMgr2",
            "GetDiminishedSpFromInjectors",
            &[
                PyRep::Integer(TYPE_DAILY_ALPHA_INJECTOR as i64),
                PyRep::Integer(2),
            ],
        )
        .unwrap()
        .unwrap(),
        PyRep::Integer(100_000)
    );
    let dim_small = call_tail(
        &ctx,
        &mut sess,
        "skillMgr2",
        "GetDiminishedSpFromInjectors",
        &[
            PyRep::Integer(TYPE_SMALL_SKILL_INJECTOR as i64),
            PyRep::Integer(2),
            PyRep::Integer(1),
        ],
    )
    .unwrap()
    .unwrap();
    assert!(matches!(dim_small, PyRep::Integer(v) if v > 100_000));
}

#[test]
fn skill_points_level_and_bundle_info_cover_fallbacks() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let mut sess = char_session(char_id, 60_003_760);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.skill_points = 12_345.0;
        }
        snap.skills.remove(&char_id);
        snap.skill_queues.remove(&char_id);
    });

    // Fallback when skills map is empty -> ch.skill_points
    assert_eq!(
        call_tail(&ctx, &mut sess, "skillMgr2", "GetEstimatedSkillPoints", &[])
            .unwrap()
            .unwrap(),
        PyRep::Real(12_345.0)
    );
    assert_eq!(
        call_tail(&ctx, &mut sess, "skillMgr2", "GetSkillPoints", &[])
            .unwrap()
            .unwrap(),
        PyRep::Real(12_345.0)
    );
    assert_eq!(
        call_tail(&ctx, &mut sess, "skillMgr2", "GetSkillLevel", &[])
            .unwrap()
            .unwrap(),
        PyRep::Integer(0)
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            3300,
            TrainedSkillRecord {
                type_id: 3300,
                level: 3,
                skill_points: 8_000.0,
            },
        );
    });
    assert_eq!(
        call_tail(
            &ctx,
            &mut sess,
            "skillMgr2",
            "GetEstimatedSkillPoints",
            &[PyRep::Integer(3300)],
        )
        .unwrap()
        .unwrap(),
        PyRep::Real(8_000.0)
    );
    assert_eq!(
        call_tail(
            &ctx,
            &mut sess,
            "skillMgr2",
            "GetSkillPoints",
            &[PyRep::Integer(3300)],
        )
        .unwrap()
        .unwrap(),
        PyRep::Real(8_000.0)
    );
    assert_eq!(
        call_tail(
            &ctx,
            &mut sess,
            "skillMgr2",
            "GetSkillLevel",
            &[PyRep::Integer(3300)],
        )
        .unwrap()
        .unwrap(),
        PyRep::Integer(3)
    );

    // GetSkillBundleInfo errors + catalog check if available
    assert_eq!(
        call_tail(&ctx, &mut sess, "skillMgr2", "GetSkillBundleInfo", &[])
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidSkillBundle"
    );
    assert_eq!(
        call_tail(
            &ctx,
            &mut sess,
            "skillMgr2",
            "GetSkillBundleInfo",
            &[PyRep::Integer(999_999_999)],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidSkillBundle"
    );
    if let Ok(cfsd) = ctx.sde.load_cfsd_nested("res:/staticdata/crates.fsdbinary") {
        if let Some(bid) = cfsd.values().find_map(|v| v.get("skillBundleID").and_then(|b| b.as_i64())).filter(|&b| b > 0) {
            let res = call_tail(
                &ctx,
                &mut sess,
                "skillMgr2",
                "GetSkillBundleInfo",
                &[PyRep::Integer(bid)],
            )
            .unwrap()
            .expect("valid bundle id");
            assert!(matches!(res, PyRep::Tuple(ref t) if t.len() == 2));
        }
    }
}
