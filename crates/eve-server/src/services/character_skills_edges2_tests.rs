//! Edge-case, error-path, and helper unit tests for `services::character_skills`.

#![forbid(unsafe_code)]

use super::*;
use crate::formulas::sp_for_skill_level;
use crate::router::ServerContext;
use eve_db::{InventoryFlag, InventoryItemRecord, SkillHistoryEntry, SkillQueueEntry, TrainedSkillRecord, WorldRepository};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{attr_ids, InvType, SdeRegistry};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

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

fn seed_skill_type(
    sde: &mut Arc<SdeRegistry>,
    new_tid: u32,
    rank: f64,
    prereq: Option<(u32, u8)>,
) {
    let reg = Arc::make_mut(sde);
    let mut rec: InvType = reg
        .types
        .get(&3300)
        .cloned()
        .or_else(|| reg.types.values().next().cloned())
        .expect("base sde type");
    rec.type_id = new_tid;
    rec.group_id = 255;
    rec.category_id = 16;
    rec.published = true;
    rec.name = format!("EdgeSkill_{new_tid}");
    rec.default_attributes.clear();
    rec.default_attributes.insert(attr_ids::SKILL_TIME_CONSTANT, rank);
    rec.default_attributes
        .insert(attr_ids::PRIMARY_ATTRIBUTE, attr_ids::PERCEPTION as f64);
    rec.default_attributes
        .insert(attr_ids::SECONDARY_ATTRIBUTE, attr_ids::WILLPOWER as f64);
    if let Some((req_tid, req_lvl)) = prereq {
        rec.default_attributes
            .insert(eve_dogma::REQUIRED_SKILL_ATTR_IDS[0], f64::from(req_tid));
        rec.default_attributes
            .insert(eve_dogma::REQUIRED_SKILL_LEVEL_ATTR_IDS[0], f64::from(req_lvl));
    }
    reg.types.insert(new_tid, rec);
}

#[test]
fn catch_up_skill_queue_omega_lapse_paused_empty_future_and_midqueue_caps() {
    let mut ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let alt_char_id = 90_000_099_i64;
    let sk_a = 951_001_u32;
    let sk_b = 951_002_u32;
    let sk_c = 951_003_u32;
    seed_skill_type(&mut ctx.sde, sk_a, 1.0, None);
    seed_skill_type(&mut ctx.sde, sk_b, 1.0, None);
    seed_skill_type(&mut ctx.sde, sk_c, 1.0, None);
    {
        let sde = Arc::make_mut(&mut ctx.sde);
        if sde.clone_grades.is_empty() {
            sde.clone_grades.insert(
                1,
                eve_sde::CloneGradeInfo {
                    grade_id: 1,
                    alpha_skill_caps: HashMap::new(),
                },
            );
        }
        for cg in sde.clone_grades.values_mut() {
            cg.alpha_skill_caps.insert(sk_c, 5);
        }
    }

    let now = ctx.now_unix();
    // Acquire online lock so notifications for char_id are recorded.
    ctx.db.acquire_online_lock(1, 1, Some(char_id), true, now).unwrap();

    // 1. Paused queue + lapsed Omega subscription drops all user_id characters to Alpha and pauses them
    let uid = ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get(&char_id).unwrap().clone();
        let uid = ch.user_id;
        let mut alt = ch.clone();
        alt.character_id = alt_char_id;
        alt.clone_grade = 2;
        snap.characters.insert(alt_char_id, alt);
        snap.characters.get_mut(&char_id).unwrap().clone_grade = 2;
        snap.paused_skill_queues.insert(char_id);
        snap.paused_skill_queues.remove(&alt_char_id);
        uid
    });
    crate::services::account_services::with_store(&ctx, |_, store| {
        store
            .subscription_expiry
            .insert(uid, now.saturating_sub(10));
    });
    catch_up_skill_queue(&ctx, char_id);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.characters[&char_id].clone_grade, 1);
        assert_eq!(snap.characters[&alt_char_id].clone_grade, 1);
        assert!(snap.paused_skill_queues.contains(&char_id));
        assert!(snap.paused_skill_queues.contains(&alt_char_id));
    });
    let _ = ctx.drain_notifications_for_char(char_id);

    // 2. Empty queue + lapsed Omega drops clone_grade and emits OnSkillQueuePausedServer
    let uid_online = ctx.db.with_snapshot(|s| s.characters[&char_id].user_id);
    let _ = ctx.db.acquire_online_lock(uid_online, 1, Some(char_id), false, now);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().clone_grade = 2;
        snap.characters.get_mut(&alt_char_id).unwrap().clone_grade = 2;
        snap.paused_skill_queues.remove(&char_id);
        snap.paused_skill_queues.remove(&alt_char_id);
        snap.skill_queues.remove(&char_id);
    });
    catch_up_skill_queue(&ctx, char_id);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.characters[&char_id].clone_grade, 1);
        assert_eq!(snap.characters[&alt_char_id].clone_grade, 1);
        assert!(snap.paused_skill_queues.contains(&char_id));
        assert!(snap.paused_skill_queues.contains(&alt_char_id));
    });
    let notifs = ctx.drain_notifications_for_char(char_id);
    assert!(notifs.iter().any(|n| n.event == "OnSkillQueuePausedServer"));

    // 3. Future started_at_unix with active Omega -> no-op
    let uid = ctx.db.with_snapshot(|s| s.characters[&char_id].user_id);
    crate::services::account_services::with_store(&ctx, |_, store| {
        store.subscription_expiry.insert(uid, now + 86_400);
    });
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().clone_grade = 2;
        snap.paused_skill_queues.remove(&char_id);
        snap.skill_queues.insert(
            char_id,
            vec![SkillQueueEntry {
                type_id: sk_a,
                target_level: 1,
                start_sp: 0.0,
                end_sp: 250.0,
                duration_seconds: 500.0,
                started_at_unix: Some(now + 120),
            }],
        );
    });
    catch_up_skill_queue(&ctx, char_id);
    assert_eq!(
        ctx.db.with_snapshot(|s| s.skill_queues[&char_id][0].start_sp),
        0.0
    );

    // 4. Multi-entry queue where elapsed time completes entry 1, partially trains entry 2, leaves entry 3 untouched
    ctx.db.with_mut_snapshot(|snap| {
        snap.skill_queues.insert(
            char_id,
            vec![
                SkillQueueEntry {
                    type_id: sk_a,
                    target_level: 1,
                    start_sp: 0.0,
                    end_sp: 250.0,
                    duration_seconds: 100.0,
                    started_at_unix: Some(now.saturating_sub(1_200)),
                },
                SkillQueueEntry {
                    type_id: sk_b,
                    target_level: 3,
                    start_sp: 0.0,
                    end_sp: 8_000.0,
                    duration_seconds: 16_000.0,
                    started_at_unix: None,
                },
                SkillQueueEntry {
                    type_id: sk_c,
                    target_level: 1,
                    start_sp: 0.0,
                    end_sp: 250.0,
                    duration_seconds: 500.0,
                    started_at_unix: None,
                },
            ],
        );
    });
    catch_up_skill_queue(&ctx, char_id);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.skills[&char_id][&sk_a].level, 1);
        let q = &snap.skill_queues[&char_id];
        assert_eq!(q.len(), 2);
        assert_eq!(q[0].type_id, sk_b);
        assert!(q[0].start_sp > 0.0 && q[0].start_sp < 8_000.0);
        assert_eq!(q[1].type_id, sk_c);
        assert_eq!(q[1].start_sp, 0.0);
    });

    // 5. Alpha clone mid-queue hitting per-skill alpha cap (level 5 on non-alpha-capped or omega-only skill)
    let cap5_tid = ctx
        .sde
        .types
        .keys()
        .copied()
        .find(|&tid| {
            ctx.sde.types.get(&tid).is_some_and(|t| t.category_id == 16)
                && alpha_skill_cap_for_character(&ctx.sde, 1, tid) < 5
        });
    if let Some(capped_tid) = cap5_tid {
        let cap = alpha_skill_cap_for_character(&ctx.sde, 1, capped_tid);
        let over_lvl = (cap + 1).min(5);
        ctx.db.with_mut_snapshot(|snap| {
            let ch = snap.characters.get_mut(&char_id).unwrap();
            ch.clone_grade = 1;
            ch.skill_points = 100_000.0;
            ch.free_skill_points = 0.0;
            snap.paused_skill_queues.remove(&char_id);
            snap.skill_queues.insert(
                char_id,
                vec![
                    SkillQueueEntry {
                        type_id: sk_c,
                        target_level: 1,
                        start_sp: 0.0,
                        end_sp: 250.0,
                        duration_seconds: 100.0,
                        started_at_unix: Some(now.saturating_sub(1_200)),
                    },
                    SkillQueueEntry {
                        type_id: capped_tid,
                        target_level: over_lvl,
                        start_sp: 0.0,
                        end_sp: 1_415.0,
                        duration_seconds: 2_000.0,
                        started_at_unix: None,
                    },
                ],
            );
        });
        catch_up_skill_queue(&ctx, char_id);
        ctx.db.with_snapshot(|snap| {
            assert_eq!(snap.skills[&char_id][&sk_c].level, 1);
            assert!(snap.paused_skill_queues.contains(&char_id));
            assert_eq!(snap.skill_queues[&char_id].len(), 1);
            assert_eq!(snap.skill_queues[&char_id][0].type_id, capped_tid);
        });
    }
}

#[test]
fn skill_queries_save_shapes_and_end_of_training_edge_cases() {
    let mut ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let st_id = 60_003_760_i64;
    let kw = HashMap::new();
    let mut unauth = SessionState {
        user_id: Some(1),
        ..Default::default()
    };
    assert_eq!(
        handle(&ctx, &mut unauth, "skillMgr2", "GetAllSkills", &[], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "CharacterNotSelected"
    );

    let mut sess = char_session(char_id, st_id);
    let prereq_tid = 951_010_u32;
    let child_tid = 951_011_u32;
    seed_skill_type(&mut ctx.sde, prereq_tid, 1.0, None);
    seed_skill_type(&mut ctx.sde, child_tid, 1.0, Some((prereq_tid, 1)));

    // Empty skills & empty queue
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.remove(&char_id);
        snap.skill_queues.remove(&char_id);
        snap.paused_skill_queues.remove(&char_id);
    });
    match handle(&ctx, &mut sess, "skillMgr2", "GetAllSkills", &[], &kw).unwrap().unwrap() {
        PyRep::Dict(entries) => assert!(entries.is_empty()),
        other => panic!("expected empty Dict, got {other:?}"),
    }
    assert_eq!(
        handle(&ctx, &mut sess, "skillMgr2", "GetEndOfTraining", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::None
    );

    // Alpha clone with skill trained above alpha cap clamps active/effective/virtual level
    if let Some(capped_tid) = ctx.sde.types.keys().copied().find(|&tid| {
        ctx.sde.types.get(&tid).is_some_and(|t| t.category_id == 16)
            && alpha_skill_cap_for_character(&ctx.sde, 1, tid) < 5
    }) {
        let cap = alpha_skill_cap_for_character(&ctx.sde, 1, capped_tid);
        ctx.db.with_mut_snapshot(|snap| {
            let ch = snap.characters.get_mut(&char_id).unwrap();
            ch.clone_grade = 1;
            ch.race_id = 1;
            snap.skills.entry(char_id).or_default().insert(
                capped_tid,
                TrainedSkillRecord {
                    type_id: capped_tid,
                    level: 5,
                    skill_points: 256_000.0,
                },
            );
        });
        let all_sk = handle(&ctx, &mut sess, "skillMgr2", "GetAllSkills", &[], &kw)
            .unwrap()
            .unwrap();
        if let PyRep::Dict(entries) = all_sk {
            let (_, val) = entries
                .into_iter()
                .find(|(k, _)| k.as_i64() == Some(i64::from(capped_tid)))
                .unwrap();
            assert_eq!(val.dict_get("skillLevel").and_then(|v| v.as_i64()), Some(5));
            assert_eq!(
                val.dict_get("activeSkillLevel").and_then(|v| v.as_i64()),
                Some(i64::from(cap))
            );
            assert_eq!(
                val.dict_get("effectiveSkillLevel").and_then(|v| v.as_i64()),
                Some(i64::from(cap))
            );
        } else {
            panic!("expected Dict");
        }
        let row_sk = handle(&ctx, &mut sess, "skillMgr2", "GetSkills", &[], &kw)
            .unwrap()
            .unwrap();
        if let PyRep::Rowset { rows, .. } = row_sk {
            let r = rows
                .into_iter()
                .find(|r| r.first().and_then(|v| v.as_i64()) == Some(i64::from(capped_tid)))
                .unwrap();
            assert_eq!(r[1].as_i64(), Some(5));
            assert_eq!(r[3].as_i64(), Some(i64::from(cap)));
        } else {
            panic!("expected Rowset");
        }
    }

    // SaveSkillQueue: KeyVal payload ordering & prerequisite satisfied within same batch
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().clone_grade = 2;
        snap.skills.entry(char_id).or_default().clear();
        snap.skill_queues.remove(&char_id);
        snap.paused_skill_queues.remove(&char_id);
    });
    let kv_payload = PyRep::keyval([
        (
            "1",
            PyRep::keyval([
                ("typeID", PyRep::Integer(i64::from(child_tid))),
                ("targetLevel", PyRep::Integer(1)),
            ]),
        ),
        (
            "0",
            PyRep::keyval([
                ("trainingTypeID", PyRep::Integer(i64::from(prereq_tid))),
                ("trainingToLevel", PyRep::Integer(1)),
            ]),
        ),
    ]);
    let mut save_kw = HashMap::new();
    save_kw.insert("activate".into(), PyRep::Integer(0));
    let saved = handle(
        &ctx,
        &mut sess,
        "skillMgr2",
        "SaveSkillQueue",
        &[kv_payload],
        &save_kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(saved.dict_get("typeID").and_then(|v| v.as_i64()), Some(i64::from(child_tid)));
    // activate=0 paused queue -> GetEndOfTraining returns None
    assert_eq!(
        handle(&ctx, &mut sess, "skillMgr2", "GetEndOfTraining", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::None
    );

    // Unpause queue (head has started_at_unix == None) -> GetEndOfTraining computes FILETIME ticks
    ctx.db.with_mut_snapshot(|snap| {
        snap.paused_skill_queues.remove(&char_id);
    });
    let end_ft = handle(&ctx, &mut sess, "skillMgr2", "GetEndOfTraining", &[], &kw)
        .unwrap()
        .unwrap()
        .as_i64()
        .expect("end FILETIME");
    assert!(end_ft > 116_444_736_000_000_000);

    // Duplicate/lower level in same batch -> SkillAlreadyTrained
    let dup_err = handle(
        &ctx,
        &mut sess,
        "skillMgr2",
        "SaveSkillQueue",
        &[PyRep::List(vec![
            PyRep::Tuple(vec![PyRep::Integer(i64::from(prereq_tid)), PyRep::Integer(2)]),
            PyRep::Tuple(vec![PyRep::Integer(i64::from(prereq_tid)), PyRep::Integer(2)]),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(dup_err.code, "SkillAlreadyTrained");

    // Unknown or non-skill typeID -> InvalidSkillType
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "skillMgr2",
            "SaveSkillQueue",
            &[PyRep::Integer(587), PyRep::Integer(1)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidSkillType"
    );

    // Empty SaveSkillQueue clears queue and records event_type_id=38 abort history
    let cleared = handle(
        &ctx,
        &mut sess,
        "skillMgr2",
        "SaveSkillQueue",
        &[PyRep::List(vec![])],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(cleared, PyRep::List(vec![]));
    ctx.db.with_snapshot(|snap| {
        assert!(snap.skill_queues[&char_id].is_empty());
        assert!(snap.paused_skill_queues.contains(&char_id));
        assert!(snap.skill_history[&char_id].iter().any(|h| h.event_type_id == 38));
    });
}

#[test]
fn apply_free_sp_queue_sync_errors_and_inject_skill_into_brain_edges() {
    let mut ctx = test_ctx();
    ctx.set_clock(Arc::new(eve_db::clock::VirtualServerClock::with_fixed_base(1_800_000_000)));
    let char_id = 90_000_001_i64;
    let st_id = 60_003_760_i64;
    let kw = HashMap::new();
    let mut sess = char_session(char_id, st_id);

    let sk_a = 951_020_u32;
    let sk_b = 951_021_u32;
    seed_skill_type(&mut ctx.sde, sk_a, 1.0, None);
    seed_skill_type(&mut ctx.sde, sk_b, 1.0, Some((sk_a, 2)));

    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.clone_grade = 2;
        ch.skill_points = 0.0;
        ch.free_skill_points = 10_000.0;
        snap.skills.entry(char_id).or_default().clear();
        snap.skills.entry(char_id).or_default().insert(
            sk_a,
            TrainedSkillRecord {
                type_id: sk_a,
                level: 0,
                skill_points: 0.0,
            },
        );
        snap.skills.entry(char_id).or_default().insert(
            sk_b,
            TrainedSkillRecord {
                type_id: sk_b,
                level: 0,
                skill_points: 0.0,
            },
        );
        snap.skill_queues.insert(
            char_id,
            vec![
                SkillQueueEntry {
                    type_id: sk_a,
                    target_level: 1,
                    start_sp: 0.0,
                    end_sp: 250.0,
                    duration_seconds: 100.0,
                    started_at_unix: Some(ctx.now_unix()),
                },
                SkillQueueEntry {
                    type_id: sk_a,
                    target_level: 2,
                    start_sp: 250.0,
                    end_sp: sp_for_skill_level(1.0, 2),
                    duration_seconds: 1_000.0,
                    started_at_unix: None,
                },
            ],
        );
        snap.paused_skill_queues.remove(&char_id);
    });

    // 2-arg validation errors
    assert_eq!(
        handle(&ctx, &mut sess, "skillMgr2", "ApplyFreeSkillPoints", &[PyRep::Integer(0), PyRep::Real(100.0)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidSkillType"
    );
    assert_eq!(
        handle(&ctx, &mut sess, "skillMgr2", "ApplyFreeSkillPoints", &[PyRep::Integer(i64::from(sk_a)), PyRep::Real(-10.0)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidSkillPointAmount"
    );
    assert_eq!(
        handle(&ctx, &mut sess, "skillMgr2", "ApplyFreeSkillPoints", &[PyRep::Integer(587), PyRep::Real(100.0)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidSkillType"
    );
    assert_eq!(
        handle(&ctx, &mut sess, "skillMgr2", "ApplyFreeSkillPoints", &[PyRep::Integer(i64::from(sk_b)), PyRep::Real(100.0)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "SkillPrerequisiteNotMet"
    );

    // Apply 500 SP to sk_a -> completes level 1 entry in queue and partially advances level 2 entry
    let res_sp = handle(
        &ctx,
        &mut sess,
        "skillMgr2",
        "ApplyFreeSkillPoints",
        &[PyRep::Integer(i64::from(sk_a)), PyRep::Real(500.0)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(res_sp.as_f64(), Some(500.0));
    ctx.db.with_snapshot(|snap| {
        let q = &snap.skill_queues[&char_id];
        assert_eq!(q.len(), 1);
        assert_eq!(q[0].target_level, 2);
        assert_eq!(q[0].start_sp, 500.0);
        assert!(q[0].started_at_unix.is_some());
        assert!(q[0].duration_seconds < 1_000.0);
    });

    // 1-arg ApplyFreeSkillPoints: invalid amount -> InvalidSkillPointAmount
    assert_eq!(
        handle(&ctx, &mut sess, "skillMgr2", "ApplyFreeSkillPoints", &[PyRep::Real(0.0)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidSkillPointAmount"
    );

    // 1-arg partial application to sk_a level 2
    let total_sp = handle(
        &ctx,
        &mut sess,
        "skillMgr2",
        "ApplyFreeSkillPoints",
        &[PyRep::Real(200.0)],
        &kw,
    )
    .unwrap()
    .unwrap()
    .as_f64()
    .unwrap();
    assert!((total_sp - 700.0).abs() < 1e-6);

    // InsufficientFreeSP when free_skill_points is 0
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().free_skill_points = 0.0;
    });
    assert_eq!(
        handle(&ctx, &mut sess, "skillMgr2", "ApplyFreeSkillPoints", &[PyRep::Integer(i64::from(sk_a)), PyRep::Real(10.0)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "InsufficientFreeSP"
    );
    assert_eq!(
        handle(&ctx, &mut sess, "skillMgr2", "ApplyFreeSkillPoints", &[], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "InsufficientFreeSP"
    );

    // GetSkillHistory sorts chronologically by logDate
    ctx.db.with_mut_snapshot(|snap| {
        snap.skill_history.insert(
            char_id,
            vec![
                SkillHistoryEntry {
                    log_date_ft: 200,
                    event_type_id: 37,
                    skill_type_id: sk_a,
                    skill_level: 2,
                    absolute_points: 1_415.0,
                },
                SkillHistoryEntry {
                    log_date_ft: 100,
                    event_type_id: 36,
                    skill_type_id: sk_a,
                    skill_level: 0,
                    absolute_points: 0.0,
                },
            ],
        );
    });
    if let PyRep::Rowset { rows, .. } = handle(&ctx, &mut sess, "skillMgr2", "GetSkillHistory", &[], &kw).unwrap().unwrap() {
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0][0].as_i64(), Some(100));
        assert_eq!(rows[1][0].as_i64(), Some(200));
    } else {
        panic!("expected Rowset");
    }

    // InjectSkillIntoBrain edge cases
    assert_eq!(
        handle(&ctx, &mut sess, "skillMgr2", "InjectSkillIntoBrain", &[PyRep::List(vec![])], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    // Skill already known -> SkillAlreadyInjected
    assert_eq!(
        handle(&ctx, &mut sess, "skillMgr2", "InjectSkillIntoBrain", &[PyRep::Integer(i64::from(sk_a))], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "SkillAlreadyInjected"
    );
    // New skillbook with stack quantity = 2 decrements to 1 instead of deleting
    let sk_c = 951_022_u32;
    let sk_d = 951_023_u32;
    seed_skill_type(&mut ctx.sde, sk_c, 1.0, None);
    {
        let sde = Arc::make_mut(&mut ctx.sde);
        if sde.clone_grades.is_empty() {
            sde.clone_grades.insert(
                1,
                eve_sde::CloneGradeInfo {
                    grade_id: 1,
                    alpha_skill_caps: HashMap::new(),
                },
            );
        }
        for cg in sde.clone_grades.values_mut() {
            cg.alpha_skill_caps.insert(sk_c, 5);
        }
    }
    seed_skill_type(&mut ctx.sde, sk_d, 1.0, None);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            77_001_001,
            InventoryItemRecord {
                item_id: 77_001_001,
                type_id: sk_c,
                owner_id: char_id,
                location_id: st_id,
                flag: InventoryFlag::Hangar,
                quantity: 2,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            77_001_002,
            InventoryItemRecord {
                item_id: 77_001_002,
                type_id: 587,
                owner_id: char_id,
                location_id: st_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });
    // Non-skill item in inventory -> InvalidSkillType
    assert_eq!(
        handle(&ctx, &mut sess, "dogmaIM", "InjectSkillIntoBrain", &[PyRep::Integer(77_001_002)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidSkillType"
    );
    // Missing book for sk_d -> ItemNotFound
    assert_eq!(
        handle(&ctx, &mut sess, "dogmaIM", "InjectSkillIntoBrain", &[PyRep::Integer(i64::from(sk_d))], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "ItemNotFound"
    );
    // Inject from stack of 2 using Set wrapper
    let inj = handle(
        &ctx,
        &mut sess,
        "dogmaIM",
        "InjectSkillIntoBrain",
        &[PyRep::Set(vec![PyRep::Integer(77_001_001)])],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(inj.dict_get("injected").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(ctx.db.get_item(77_001_001).unwrap().quantity, 1);
    assert!(ctx.db.with_snapshot(|s| s.skills[&char_id].contains_key(&sk_c)));
}

#[test]
fn mutant_kill_dispatch_unknown_save_new_queue_and_inject_ship_cargo() {
    let mut ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let ship_id = 880_600_001_i64;
    let book_id = 880_600_002_i64;
    let sk_train = 951_801_u32;
    let sk_cargo = 951_802_u32;
    seed_skill_type(&mut ctx.sde, sk_train, 1.0, None);
    seed_skill_type(&mut ctx.sde, sk_cargo, 1.0, None);

    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.clone_grade = 2;
        ch.station_id = None;
        ch.structure_id = None;
        ch.solar_system_id = 30_000_142;
        ch.active_ship_id = ship_id;

        snap.skills.entry(char_id).or_default().insert(
            sk_train,
            TrainedSkillRecord {
                type_id: sk_train,
                level: 1,
                skill_points: 250.0,
            },
        );
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: 30_000_142,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            book_id,
            InventoryItemRecord {
                item_id: book_id,
                type_id: sk_cargo,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 2,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    let mut sess = SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        corporation_id: Some(1_000_009),
        solar_system_id: Some(30_000_142),
        station_id: None,
        structure_id: None,
        ship_id: Some(ship_id),
        ..Default::default()
    };
    let kw = HashMap::new();

    // Unknown service / method must return None
    assert!(handle(&ctx, &mut sess, "nonmatching_service", "GetAllSkills", &[], &kw).is_none());
    assert!(handle(&ctx, &mut sess, "skillMgr2", "unmatched_method", &[], &kw).is_none());

    // GetAllSkills must return non-empty Dict for trained skills
    match handle(&ctx, &mut sess, "skillMgr2", "GetAllSkills", &[], &kw).unwrap().unwrap() {
        PyRep::Dict(entries) => {
            assert!(entries.iter().any(|(k, _)| k.as_i64() == Some(i64::from(sk_train))));
        }
        other => panic!("expected Dict, got {other:?}"),
    }

    // SaveNewQueue saves active queue and returns non-empty payload
    let saved = handle(
        &ctx,
        &mut sess,
        "skillMgr2",
        "SaveNewQueue",
        &[PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Integer(i64::from(sk_train)),
            PyRep::Integer(2),
        ])])],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(saved.dict_get("typeID").and_then(|v| v.as_i64()), Some(i64::from(sk_train)));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.skill_queues[&char_id].len(), 1);
        assert_eq!(snap.skill_queues[&char_id][0].target_level, 2);
    });

    // InjectSkillIntoBrain from ship Cargo (in space, requires active_ship_id in ConsumeExactOptions)
    let inj = handle(
        &ctx,
        &mut sess,
        "skillMgr2",
        "InjectSkillIntoBrain",
        &[PyRep::Integer(book_id)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(inj.dict_get("typeID").and_then(|v| v.as_i64()), Some(i64::from(sk_cargo)));
    assert_eq!(inj.dict_get("injected").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(ctx.db.get_item(book_id).unwrap().quantity, 1);
    ctx.db.with_snapshot(|snap| {
        assert!(snap.skills[&char_id].contains_key(&sk_cargo));
        assert!(snap.skill_history[&char_id]
            .iter()
            .any(|h| h.event_type_id == 36 && h.skill_type_id == sk_cargo));
    });
}
