//! Unit and mutation-killing tests for `services::character_skills`.

use super::*;
#[cfg(test)]
#[path = "character_skills_helpers_cov_tests.rs"]
mod character_skills_helpers_cov_tests;
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

pub(super) fn test_ctx() -> ServerContext {
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

pub(super) fn char_session(char_id: i64, st_id: i64) -> SessionState {
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
    let PyRep::List(q_items) = q_rep else {
        panic!("expected List of KeyVals from GetSkillQueue");
    };
    assert_eq!(q_items.len(), 1);

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

    // Seed char has >5M SP, so Large injector yields 400k and Small yields 80k; <5M SP yields 500k and 100k.
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "skillMgr2",
            "GetDiminishedSpFromInjectors",
            &[PyRep::Integer(40520), PyRep::Integer(1), PyRep::Integer(0)],
            &kw
        )
        .unwrap()
        .unwrap(),
        PyRep::Integer(400_000)
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "skillMgr2",
            "GetDiminishedSpFromInjectors",
            &[PyRep::Integer(45635), PyRep::Integer(1), PyRep::Integer(0)],
            &kw
        )
        .unwrap()
        .unwrap(),
        PyRep::Integer(80_000)
    );
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.skill_points = 1_000_000.0;
        ch.free_skill_points = 0.0;
    });
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "skillMgr2",
            "GetDiminishedSpFromInjectors",
            &[PyRep::Integer(40520), PyRep::Integer(1), PyRep::Integer(0)],
            &kw
        )
        .unwrap()
        .unwrap(),
        PyRep::Integer(500_000)
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "skillMgr2",
            "GetDiminishedSpFromInjectors",
            &[PyRep::Integer(45635), PyRep::Integer(1), PyRep::Integer(0)],
            &kw
        )
        .unwrap()
        .unwrap(),
        PyRep::Integer(100_000)
    );
    let sp_of_trained = handle(
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
    .unwrap();
    assert!(sp_of_trained >= 0.0);
    // Untrained skill must return 0.0 SP and level 0 (not fake 50_000.0 or level 1).
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "skillMgr2",
            "GetSkillPoints",
            &[PyRep::Integer(999_888)],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Real(0.0)
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "skillMgr2",
            "GetSkillLevel",
            &[PyRep::Integer(new_skill_tid as i64)],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Integer(0)
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "skillMgr2",
            "GetSkillLevel",
            &[PyRep::Integer(999_888)],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Integer(0)
    );
    let bundle_info = handle(
        &ctx,
        &mut sess,
        "skillMgr2",
        "GetSkillBundleInfo",
        &[PyRep::Integer(39)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert!(matches!(bundle_info, PyRep::Tuple(ref els) if els.len() == 2 && els[0].as_str().is_some()));
    assert_eq!(
        handle(&ctx, &mut sess, "skillMgr2", "GetSkillBundleInfo", &[PyRep::Integer(999_999)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidSkillBundle"
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

#[test]
fn skill_training_multiplier_scales_queue_save_display_catch_up_tick_free_sp_and_char_select() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let st_id = 60_003_760_i64;
    let skill_tid = 3300_u32; // Gunnery (rank 1, no prerequisites)
    let mut sess = char_session(char_id, st_id);
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.clone_grade = 2;
            ch.free_skill_points = 5_000.0;
        }
        snap.skills.entry(char_id).or_default().insert(
            skill_tid,
            TrainedSkillRecord {
                type_id: skill_tid,
                level: 0,
                skill_points: 0.0,
            },
        );
        snap.skill_queues.remove(&char_id);
        snap.paused_skill_queues.remove(&char_id);
    });

    // 1. Save a 3-level queue at default 1.0x and inspect rowset, GetEndOfTraining, and char_selection.
    handle(
        &ctx,
        &mut sess,
        "skillMgr2",
        "SaveSkillQueue",
        &[PyRep::List(vec![
            PyRep::Tuple(vec![PyRep::Integer(i64::from(skill_tid)), PyRep::Integer(1)]),
            PyRep::Tuple(vec![PyRep::Integer(i64::from(skill_tid)), PyRep::Integer(2)]),
            PyRep::Tuple(vec![PyRep::Integer(i64::from(skill_tid)), PyRep::Integer(3)]),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap();

    let PyRep::List(q_1x) = handle(&ctx, &mut sess, "skillMgr2", "GetSkillQueue", &[], &kw)
        .unwrap()
        .unwrap()
    else {
        panic!("expected List from GetSkillQueue");
    };
    assert_eq!(q_1x.len(), 3);
    let dur0_1x = q_1x[0].dict_get("durationSeconds").unwrap().as_f64().unwrap();
    let dur1_1x = q_1x[1].dict_get("durationSeconds").unwrap().as_f64().unwrap();
    let dur2_1x = q_1x[2].dict_get("durationSeconds").unwrap().as_f64().unwrap();
    assert!(dur0_1x > 100.0);
    let eot_1x = handle(&ctx, &mut sess, "skillMgr2", "GetEndOfTraining", &[], &kw)
        .unwrap()
        .unwrap()
        .as_i64()
        .unwrap();
    let head_start_ft = q_1x[0]
        .dict_get("trainingStartTime")
        .unwrap()
        .as_i64()
        .unwrap();

    // 2. Raise skill_training_multiplier to 100.0 WITHOUT rewriting the pre-existing DB queue:
    //    rowset durations, GetEndOfTraining, and character selection must all scale dynamically 100x.
    ctx.feature_flags
        .write()
        .unwrap()
        .skill_training_multiplier = 100.0;

    let PyRep::List(q_100x) = handle(&ctx, &mut sess, "skillMgr2", "GetSkillQueue", &[], &kw)
        .unwrap()
        .unwrap()
    else {
        panic!("expected List from GetSkillQueue");
    };
    let dur0_100x = q_100x[0].dict_get("durationSeconds").unwrap().as_f64().unwrap();
    let dur1_100x = q_100x[1].dict_get("durationSeconds").unwrap().as_f64().unwrap();
    let dur2_100x = q_100x[2].dict_get("durationSeconds").unwrap().as_f64().unwrap();
    assert!((dur0_100x - dur0_1x / 100.0).abs() < 1e-3);
    assert!((dur1_100x - dur1_1x / 100.0).abs() < 1e-3);
    assert!((dur2_100x - dur2_1x / 100.0).abs() < 1e-3);

    let eot_100x = handle(&ctx, &mut sess, "skillMgr2", "GetEndOfTraining", &[], &kw)
        .unwrap()
        .unwrap()
        .as_i64()
        .unwrap();
    let total_1x_secs = (eot_1x - head_start_ft) as f64 / 10_000_000.0;
    let total_100x_secs = (eot_100x - head_start_ft) as f64 / 10_000_000.0;
    assert!((total_100x_secs - total_1x_secs / 100.0).abs() < 2.0);

    let PyRep::Tuple(sel_parts) =
        crate::services::char_selection::get_character_selection_data(&ctx, &sess).unwrap()
    else {
        panic!("expected tuple from get_character_selection_data");
    };
    let PyRep::List(sel_rows) = &sel_parts[2] else {
        panic!("expected List from get_character_selection_data");
    };
    let sel_row = sel_rows
        .iter()
        .find(|r| r.dict_get("characterID") == Some(&PyRep::Integer(char_id)))
        .unwrap();
    let sel_q_start = sel_row.dict_get("trainingStartTime").unwrap().as_i64().unwrap();
    let sel_q_end = sel_row.dict_get("queueEndTime").unwrap().as_i64().unwrap();
    let sel_total_secs = (sel_q_end - sel_q_start) as f64 / 10_000_000.0;
    assert!((sel_total_secs - total_1x_secs / 100.0).abs() < 2.0);

    // 3. Backdate start timestamp by only ~1/100th of 1.0x level-1 time: catch_up_skill_queue
    //    must complete level 1 at 100.0x rate (which would barely scratch level 1 at 1.0x).
    let now = ctx.now_unix();
    let elapsed_for_lvl1 = (dur0_100x.ceil() as u64) + 1;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(head) = snap.skill_queues.get_mut(&char_id).and_then(|q| q.first_mut()) {
            head.started_at_unix = Some(now.saturating_sub(elapsed_for_lvl1));
        }
    });
    catch_up_skill_queue(&ctx, char_id);
    ctx.db.with_snapshot(|snap| {
        assert!(
            snap.skills[&char_id][&skill_tid].level >= 1,
            "100x catch-up must finish level 1 within accelerated duration"
        );
        let q = &snap.skill_queues[&char_id];
        assert!(q.first().is_some_and(|e| e.target_level >= 2));
    });

    // 4. Newly saved queue at 100.0x persists accelerated duration_seconds, and ApplyFreeSkillPoints
    //    keeps remaining duration scaled by 100.0x (never reverting to 1.0x).
    let lvl2_sp = crate::formulas::sp_for_skill_level(1.0, 2);
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            skill_tid,
            TrainedSkillRecord {
                type_id: skill_tid,
                level: 2,
                skill_points: lvl2_sp,
            },
        );
        snap.skill_queues.remove(&char_id);
    });
    handle(
        &ctx,
        &mut sess,
        "skillMgr2",
        "SaveSkillQueue",
        &[PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Integer(i64::from(skill_tid)),
            PyRep::Integer(3),
        ])])],
        &kw,
    )
    .unwrap()
    .unwrap();
    let stored_lvl3_100x = ctx
        .db
        .with_snapshot(|s| s.skill_queues[&char_id][0].duration_seconds);
    assert!((stored_lvl3_100x - dur2_1x / 100.0).abs() < 1e-2);

    handle(
        &ctx,
        &mut sess,
        "skillMgr2",
        "ApplyFreeSkillPoints",
        &[PyRep::Integer(i64::from(skill_tid)), PyRep::Integer(500)],
        &kw,
    )
    .unwrap()
    .unwrap();
    let (after_free_sp, after_free_dur) = ctx.db.with_snapshot(|s| {
        let e = &s.skill_queues[&char_id][0];
        (e.start_sp, e.duration_seconds)
    });
    assert!((after_free_sp - (lvl2_sp + 500.0)).abs() < 1e-6);
    assert!(
        after_free_dur < stored_lvl3_100x && after_free_dur < dur2_1x / 50.0,
        "ApplyFreeSkillPoints must keep 100x remaining duration, got {after_free_dur}"
    );

    // 5. Background tick advance_skill_queues advances SP at 100.0x rate, and CharStartTrainingSkill
    //    stores 100.0x duration_seconds directly.
    ctx.advance_skill_queues(5.0);
    let after_tick_sp = ctx
        .db
        .with_snapshot(|s| s.skill_queues[&char_id][0].start_sp);
    let sp_gained_in_5s = after_tick_sp - after_free_sp;
    assert!(
        sp_gained_in_5s > 100.0,
        "expected >= 100 SP gained in 5s at 100x (vanilla 5s <= 5 SP), got {sp_gained_in_5s}"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.skill_queues.remove(&char_id);
        snap.skills.entry(char_id).or_default().insert(
            skill_tid,
            TrainedSkillRecord {
                type_id: skill_tid,
                level: 2,
                skill_points: lvl2_sp,
            },
        );
    });
    super::character_skills_helpers::handle_skill_tail(
        &ctx,
        &mut sess,
        "skillMgr2",
        "CharStartTrainingSkill",
        &[PyRep::Integer(i64::from(skill_tid))],
        &kw,
    )
    .unwrap()
    .unwrap();
    let started_dur = ctx
        .db
        .with_snapshot(|s| s.skill_queues[&char_id][0].duration_seconds);
    assert!((started_dur - dur2_1x / 100.0).abs() < 1e-2);
}
