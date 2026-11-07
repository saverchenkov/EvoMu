//! Semantic unit tests split from `character_skills_extra_unit_tests`.

#![forbid(unsafe_code)]

use super::character_skills_extra_unit_tests::*;

use crate::formulas::sp_for_skill_level;
use eve_db::TrainedSkillRecord;
use eve_machonet::SessionState;
use eve_proto::PyRep;

#[test]
fn apply_free_sp_inject_stack_and_auxiliary_skill_rpcs_cover_all_branches() {
    let mut ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let st_id = 60_003_760_i64;
    let sk_base = 941_020_u32;
    let sk_adv = 941_021_u32;
    let sk_stack = 941_022_u32;
    clone_skill_type(&mut ctx.sde, sk_base, 1.0, None);
    clone_skill_type(&mut ctx.sde, sk_adv, 1.0, Some((sk_base, 3)));
    clone_skill_type(&mut ctx.sde, sk_stack, 1.0, None);

    let mut sess = SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        corporation_id: Some(1_000_009),
        solar_system_id: Some(30_000_142),
        station_id: Some(st_id),
        ship_id: Some(100_000_001),
        ..Default::default()
    };

    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.clone_grade = 2;
        ch.skill_points = 100_000.0;
        ch.free_skill_points = 500_000.0;
        let smap = snap.skills.entry(char_id).or_default();
        smap.insert(
            sk_base,
            TrainedSkillRecord {
                type_id: sk_base,
                level: 5,
                skill_points: sp_for_skill_level(1.0, 5),
            },
        );
        smap.insert(
            sk_adv,
            TrainedSkillRecord {
                type_id: sk_adv,
                level: 0,
                skill_points: 0.0,
            },
        );
        smap.remove(&sk_stack);
    });

    // 1. ApplyFreeSkillPoints 2-arg validation errors
    assert_eq!(
        call_skills(
            &ctx,
            &mut sess,
            "skillMgr2",
            "ApplyFreeSkillPoints",
            &[PyRep::Integer(0), PyRep::Real(100.0)],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidSkillType"
    );
    assert_eq!(
        call_skills(
            &ctx,
            &mut sess,
            "skillMgr2",
            "ApplyFreeSkillPoints",
            &[PyRep::Integer(sk_base as i64), PyRep::Real(-10.0)],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidSkillPointAmount"
    );
    assert_eq!(
        call_skills(
            &ctx,
            &mut sess,
            "skillMgr2",
            "ApplyFreeSkillPoints",
            &[PyRep::Integer(34), PyRep::Real(100.0)],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidSkillType"
    );
    assert_eq!(
        call_skills(
            &ctx,
            &mut sess,
            "skillMgr2",
            "ApplyFreeSkillPoints",
            &[PyRep::Integer(sk_stack as i64), PyRep::Real(100.0)],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "SkillNotInjected"
    );
    assert_eq!(
        call_skills(
            &ctx,
            &mut sess,
            "skillMgr2",
            "ApplyFreeSkillPoints",
            &[PyRep::Integer(sk_base as i64), PyRep::Real(100.0)],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "SkillAlreadyAtMaxSP"
    );

    // Lower sk_base to level 1 so sk_adv fails prerequisite check on ApplyFreeSkillPoints
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.get_mut(&char_id).unwrap().insert(
            sk_base,
            TrainedSkillRecord {
                type_id: sk_base,
                level: 1,
                skill_points: 250.0,
            },
        );
    });
    assert_eq!(
        call_skills(
            &ctx,
            &mut sess,
            "skillMgr2",
            "ApplyFreeSkillPoints",
            &[PyRep::Integer(sk_adv as i64), PyRep::Real(100.0)],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "SkillPrerequisiteNotMet"
    );

    let queue_args = [PyRep::List(vec![
        PyRep::Tuple(vec![PyRep::Integer(sk_base as i64), PyRep::Integer(2)]),
        PyRep::Tuple(vec![PyRep::Integer(sk_base as i64), PyRep::Integer(3)]),
    ])];
    let _ = call_skills(&ctx, &mut sess, "skillMgr2", "SaveSkillQueue", &queue_args).unwrap().unwrap();
    let new_sp = call_skills(
        &ctx,
        &mut sess,
        "skillMgr2",
        "ApplyFreeSkillPoints",
        &[PyRep::Integer(sk_base as i64), PyRep::Real(1_500.0)],
    )
    .unwrap()
    .unwrap()
    .as_f64()
    .unwrap();
    assert!((new_sp - 1_750.0).abs() < 1e-6);
    ctx.db.with_snapshot(|snap| {
        let q = &snap.skill_queues[&char_id];
        assert_eq!((q.len(), q[0].target_level), (1, 3));
        assert!((q[0].start_sp - 1_750.0).abs() < 1e-6);
    });

    assert_eq!(
        call_skills(&ctx, &mut sess, "skillMgr2", "ApplyFreeSkillPoints", &[PyRep::Real(0.0)])
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidSkillPointAmount"
    );

    let stack_book_id = 941_900_i64;
    ctx.db.with_mut_snapshot(|snap| {
        put_skillbook(snap, stack_book_id, sk_stack, char_id, st_id, 3);
    });
    assert_eq!(
        call_skills(&ctx, &mut sess, "dogmaIM", "InjectSkillIntoBrain", &[]).unwrap().unwrap_err().code,
        "InvalidArgument"
    );
    let inj_ok = call_skills(
        &ctx,
        &mut sess,
        "dogmaIM",
        "InjectSkillIntoBrain",
        &[PyRep::Set(vec![PyRep::Integer(sk_stack as i64)])],
    )
    .unwrap()
    .unwrap();
    assert_eq!(inj_ok.dict_get("injected"), Some(&PyRep::Bool(true)));
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&stack_book_id].quantity), 2);

    for m in [
        "ApplyFreeSkillPointsToQueue",
        "ApplyFreeSkillPointsToSkills",
        "CharStartTrainingSkill",
        "CheckInjectionConstraints",
        "CombineSkillInjector",
        "SplitSkillInjector",
    ] {
        assert_eq!(call_skills(&ctx, &mut sess, "skillMgr2", m, &[]).unwrap().unwrap(), PyRep::None);
    }
    for m in ["GetFreeSkillPointsAppliedToQueue", "GetFreeSkillPointsAppliedToSkills"] {
        assert!(matches!(call_skills(&ctx, &mut sess, "skillMgr2", m, &[]).unwrap().unwrap(), PyRep::Dict(_)));
    }
    assert!(call_skills(&ctx, &mut sess, "skillMgr2", "GetEstimatedSkillPoints", &[]).unwrap().unwrap().as_f64().is_some());
    for m in ["GetRespecInfo", "GetImplants", "GetBoosters", "GetSkillChangesForISIS", "AbortTraining"] {
        assert!(call_skills(&ctx, &mut sess, "skillMgr2", m, &[]).unwrap().is_ok());
    }
}
