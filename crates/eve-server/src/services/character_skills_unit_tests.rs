//! Unit and mutation-killing tests for `services::character_skills`.

use super::*;
use crate::router::ServerContext;
use eve_db::{InventoryFlag, InventoryItemRecord, SkillQueueEntry, TrainedSkillRecord, WorldRepository};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn rowset_len(rep: &PyRep) -> usize {
    match rep {
        PyRep::Rowset { rows, .. } | PyRep::TupleSet { rows, .. } => rows.len(),
        PyRep::IndexRowset { items, .. } => items.len(),
        PyRep::List(items) | PyRep::Tuple(items) => items.len(),
        _ => rep.crowset_rows().map(|r| r.len()).unwrap_or(0),
    }
}

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

fn char_session(char_id: i64, st_id: i64) -> SessionState {
    SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        corporation_id: Some(1_000_009),
        solar_system_id: Some(30_000_142),
        station_id: Some(st_id),
        ship_id: Some(100_000_001),
        ..Default::default()
    }
}

#[test]
fn catch_up_skill_queue_completes_finished_entries_and_advances_partial_sp() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let now = ctx.now_unix();
    let skill_tid = 3300_u32; // Gunnery (rank 1)

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.clone_grade = 2;
        }
        snap.skills.entry(char_id).or_default().insert(
            skill_tid,
            TrainedSkillRecord {
                type_id: skill_tid,
                level: 0,
                skill_points: 0.0,
            },
        );
        snap.skill_queues.insert(
            char_id,
            vec![
                SkillQueueEntry {
                    type_id: skill_tid,
                    target_level: 1,
                    start_sp: 0.0,
                    end_sp: 250.0,
                    duration_seconds: 100.0,
                    started_at_unix: Some(now.saturating_sub(600)), // plenty of time to finish level 1 and advance level 2
                },
                SkillQueueEntry {
                    type_id: skill_tid,
                    target_level: 2,
                    start_sp: 250.0,
                    end_sp: 1_415.0,
                    duration_seconds: 2_000.0,
                    started_at_unix: None,
                },
            ],
        );
        snap.paused_skill_queues.remove(&char_id);
    });

    catch_up_skill_queue(&ctx, char_id);

    ctx.db.with_snapshot(|snap| {
        let sk = &snap.skills[&char_id][&skill_tid];
        assert!(sk.level >= 1);
        assert!(sk.skill_points > 250.0);
        let q = &snap.skill_queues[&char_id];
        assert_eq!(q.len(), 1);
        assert_eq!(q[0].target_level, 2);
        assert!(q[0].start_sp > 250.0);
    });
}

#[test]
fn skill_queue_save_inject_and_query_handlers_enforce_all_constraints() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let st_id = 60_003_760_i64;
    let mut sess = char_session(char_id, st_id);
    let kw = HashMap::new();

    // Find an uninjected skill type from SDE with no prerequisites
    let new_skill_tid = ctx
        .sde
        .types
        .values()
        .find(|t| {
            t.category_id == 16
                && t.published
                && eve_dogma::REQUIRED_SKILL_ATTR_IDS
                    .iter()
                    .all(|a| !t.default_attributes.contains_key(a))
                && !ctx
                    .db
                    .with_snapshot(|s| s.skills.get(&char_id).is_some_and(|m| m.contains_key(&t.type_id)))
        })
        .map(|t| t.type_id)
        .unwrap_or(3402);

    // 1. InjectSkillIntoBrain without skillbook fails with ItemNotFound
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "skillMgr2",
            "InjectSkillIntoBrain",
            &[PyRep::Integer(new_skill_tid as i64)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "ItemNotFound"
    );

    // Place skillbook in station hangar and inject
    let book_item_id = 881_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().remove(&new_skill_tid);
        snap.items.insert(
            book_item_id,
            InventoryItemRecord {
                item_id: book_item_id,
                type_id: new_skill_tid,
                owner_id: char_id,
                location_id: st_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });
    let inj_res = handle(
        &ctx,
        &mut sess,
        "skillMgr2",
        "InjectSkillIntoBrain",
        &[PyRep::Integer(book_item_id)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(inj_res.dict_get("injected"), Some(&PyRep::Bool(true)));

    // Injecting again fails with SkillAlreadyInjected
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "skillMgr2",
            "InjectSkillIntoBrain",
            &[PyRep::Integer(new_skill_tid as i64)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "SkillAlreadyInjected"
    );

    // 2. SaveSkillQueue: non-skill type fails, valid skill succeeds, empty list pauses queue
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "skillMgr2",
            "SaveSkillQueue",
            &[PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(34), // Tritanium (category 4 != 16)
                PyRep::Integer(1),
            ])])],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidSkillType"
    );

    let saved = handle(
        &ctx,
        &mut sess,
        "skillMgr2",
        "SaveSkillQueue",
        &[PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Integer(new_skill_tid as i64),
            PyRep::Integer(1),
        ])])],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        saved.dict_get("typeID"),
        Some(&PyRep::Integer(new_skill_tid as i64))
    );
    assert_eq!(saved.dict_get("targetLevel"), Some(&PyRep::Integer(1)));

    // GetSkillQueue, GetSkillQueueAndFreePoints, GetEndOfTraining
    let q_rep = handle(&ctx, &mut sess, "skillMgr2", "GetSkillQueue", &[], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(rowset_len(&q_rep), 1);

    let q_free = handle(
        &ctx,
        &mut sess,
        "skillMgr2",
        "GetSkillQueueAndFreePoints",
        &[],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert!(matches!(q_free, PyRep::Tuple(_)));

    let eot = handle(&ctx, &mut sess, "skillMgr2", "GetEndOfTraining", &[], &kw)
        .unwrap()
        .unwrap();
    assert!(eot.as_i64().is_some());

    // Empty SaveSkillQueue pauses training
    let paused = handle(
        &ctx,
        &mut sess,
        "skillMgr2",
        "SaveSkillQueue",
        &[PyRep::List(vec![])],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(paused, PyRep::List(vec![]));
    assert_eq!(
        handle(&ctx, &mut sess, "skillMgr2", "GetEndOfTraining", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::None
    );

    // GetAllSkills, GetSkills, GetSkillHistory, GetSkillLevel, GetSkillPoints, GetDiminishedSpFromInjectors, GetSkillBundleInfo
    let all_sk = handle(&ctx, &mut sess, "skillMgr2", "GetAllSkills", &[], &kw)
        .unwrap()
        .unwrap();
    assert!(matches!(all_sk, PyRep::Dict(_)));

    let sk_rows = handle(&ctx, &mut sess, "skillMgr2", "GetSkills", &[], &kw)
        .unwrap()
        .unwrap();
    assert!(rowset_len(&sk_rows) > 0);

    let hist = handle(&ctx, &mut sess, "skillMgr2", "GetSkillHistory", &[], &kw)
        .unwrap()
        .unwrap();
    assert!(rowset_len(&hist) > 0);

    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "skillMgr2",
            "GetDiminishedSpFromInjectors",
            &[],
            &kw
        )
        .unwrap()
        .unwrap(),
        PyRep::Integer(500_000)
    );
    assert!(
        handle(
            &ctx,
            &mut sess,
            "skillMgr2",
            "GetSkillPoints",
            &[PyRep::Integer(new_skill_tid as i64)],
            &kw,
        )
        .unwrap()
        .unwrap()
        .as_f64()
        .is_some()
    );
    assert!(
        handle(
            &ctx,
            &mut sess,
            "skillMgr2",
            "GetSkillLevel",
            &[PyRep::Integer(new_skill_tid as i64)],
            &kw,
        )
        .unwrap()
        .unwrap()
        .as_i64()
        .is_some()
    );
    assert!(
        matches!(
            handle(&ctx, &mut sess, "skillMgr2", "GetSkillBundleInfo", &[], &kw)
                .unwrap()
                .unwrap(),
            PyRep::Tuple(_)
        )
    );
}

#[test]
fn apply_free_skill_points_and_alpha_cap_enforcement_work() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let mut sess = char_session(char_id, 60_003_760);
    let kw = HashMap::new();
    let skill_tid = 3300_u32;

    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.clone_grade = 2;
        ch.free_skill_points = 0.0;
        snap.skills.entry(char_id).or_default().insert(
            skill_tid,
            TrainedSkillRecord {
                type_id: skill_tid,
                level: 1,
                skill_points: 250.0,
            },
        );
    });

    // InsufficientFreeSP when free_skill_points == 0
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "skillMgr2",
            "ApplyFreeSkillPoints",
            &[PyRep::Integer(skill_tid as i64), PyRep::Integer(1_000)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InsufficientFreeSP"
    );

    // Grant 5,000 free SP and apply 1,200 to skill_tid (2-arg form)
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().free_skill_points = 5_000.0;
    });
    let new_sp = handle(
        &ctx,
        &mut sess,
        "skillMgr2",
        "ApplyFreeSkillPoints",
        &[PyRep::Integer(skill_tid as i64), PyRep::Integer(1_200)],
        &kw,
    )
    .unwrap()
    .unwrap()
    .as_f64()
    .unwrap();
    assert!((new_sp - 1_450.0).abs() < 1e-6);

    // Queue level 3 and apply remaining free SP via 1-arg queue form
    let _ = handle(
        &ctx,
        &mut sess,
        "skillMgr2",
        "SaveSkillQueue",
        &[PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Integer(skill_tid as i64),
            PyRep::Integer(3),
        ])])],
        &kw,
    )
    .unwrap()
    .unwrap();
    let total_sp = handle(
        &ctx,
        &mut sess,
        "skillMgr2",
        "ApplyFreeSkillPoints",
        &[PyRep::Real(1_000.0)],
        &kw,
    )
    .unwrap()
    .unwrap()
    .as_f64()
    .unwrap();
    assert!(total_sp > 0.0);

    // Alpha clone >= 5,000,000 SP cannot SaveSkillQueue
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.clone_grade = 1;
        ch.skill_points = 5_100_000.0;
    });
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "skillMgr2",
            "SaveSkillQueue",
            &[PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(skill_tid as i64),
                PyRep::Integer(4),
            ])])],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "AlphaCloneSpCapExceeded"
    );
}
