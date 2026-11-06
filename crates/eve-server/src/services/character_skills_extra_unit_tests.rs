//! Deep branch-coverage and mutation-killing unit tests for `services::character_skills`.

#![forbid(unsafe_code)]

use crate::formulas::sp_for_skill_level;
use crate::router::ServerContext;
use crate::services::character_skills::{catch_up_skill_queue, handle};
use eve_db::{
    InventoryFlag, InventoryItemRecord, SkillQueueEntry, TrainedSkillRecord, WorldRepository,
    WorldStateSnapshot,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
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

fn clone_skill_type(
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
    rec.name = format!("ExtraSkill_{new_tid}");
    rec.default_attributes.clear();
    rec.default_attributes
        .insert(attr_ids::SKILL_TIME_CONSTANT, rank);
    rec.default_attributes
        .insert(attr_ids::PRIMARY_ATTRIBUTE, attr_ids::PERCEPTION as f64);
    rec.default_attributes
        .insert(attr_ids::SECONDARY_ATTRIBUTE, attr_ids::WILLPOWER as f64);
    if let Some((req_tid, req_lvl)) = prereq {
        rec.default_attributes
            .insert(eve_dogma::REQUIRED_SKILL_ATTR_IDS[0], req_tid as f64);
        rec.default_attributes
            .insert(eve_dogma::REQUIRED_SKILL_LEVEL_ATTR_IDS[0], req_lvl as f64);
    }
    reg.types.insert(new_tid, rec);
}

fn put_skillbook(
    snap: &mut WorldStateSnapshot,
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    quantity: i32,
) {
    snap.items.insert(
        item_id,
        InventoryItemRecord {
            item_id,
            type_id,
            owner_id,
            location_id,
            flag: InventoryFlag::Hangar,
            quantity,
            singleton: false,
            version: 1,
            ..Default::default()
        },
    );
}

fn call_skills(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    handle(ctx, session, service, method, args, &HashMap::new())
}

#[test]
fn catch_up_skill_queue_omega_lapse_alpha_caps_and_partial_level_crossing() {
    let mut ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let sk_tid = 941_001_u32;
    clone_skill_type(&mut ctx.sde, sk_tid, 1.0, None);

    // 1. Missing character is a no-op
    catch_up_skill_queue(&ctx, 999_999_999);

    // 2. Partial progress across intermediate level boundary (level 0 -> target_level 2, crosses level 1 = 250 SP)
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.clone_grade = 2;
        ch.skill_points = 10_000.0;
        ch.free_skill_points = 0.0;
        snap.skills.entry(char_id).or_default().insert(
            sk_tid,
            TrainedSkillRecord {
                type_id: sk_tid,
                level: 0,
                skill_points: 0.0,
            },
        );
        snap.skill_queues.insert(
            char_id,
            vec![SkillQueueEntry {
                type_id: sk_tid,
                target_level: 2,
                start_sp: 0.0,
                end_sp: sp_for_skill_level(1.0, 2),
                duration_seconds: 3_000.0,
                started_at_unix: Some(now.saturating_sub(600)), // ~300 SP gained -> crosses 250 SP (level 1), < 1415 SP (level 2)
            }],
        );
        snap.paused_skill_queues.remove(&char_id);
    });
    catch_up_skill_queue(&ctx, char_id);
    ctx.db.with_snapshot(|snap| {
        let sk = &snap.skills[&char_id][&sk_tid];
        assert_eq!(sk.level, 1);
        assert!(sk.skill_points >= 250.0 && sk.skill_points < 1415.0);
        assert_eq!(snap.skill_queues[&char_id].len(), 1);
    });

    // 3. Alpha clone at >= 5M SP pauses immediately on catch_up_skill_queue
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.clone_grade = 1;
        ch.skill_points = 5_000_000.0;
        if let Some(q) = snap.skill_queues.get_mut(&char_id).and_then(|q| q.first_mut()) {
            q.started_at_unix = Some(now.saturating_sub(10));
        }
        snap.paused_skill_queues.remove(&char_id);
    });
    catch_up_skill_queue(&ctx, char_id);
    assert!(ctx.db.with_snapshot(|s| s.paused_skill_queues.contains(&char_id)));

    // 4. Alpha clone mid-queue 5M SP cap hit during catch_up_skill_queue
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.clone_grade = 1;
        ch.skill_points = 4_999_750.0;
        ch.free_skill_points = 0.0;
        snap.skills.entry(char_id).or_default().insert(
            3300,
            TrainedSkillRecord {
                type_id: 3300,
                level: 0,
                skill_points: 0.0,
            },
        );
        snap.skill_queues.insert(
            char_id,
            vec![
                SkillQueueEntry {
                    type_id: 3300,
                    target_level: 1,
                    start_sp: 0.0,
                    end_sp: 250.0,
                    duration_seconds: 600.0,
                    started_at_unix: Some(now.saturating_sub(1_200)),
                },
                SkillQueueEntry {
                    type_id: 3300,
                    target_level: 2,
                    start_sp: 250.0,
                    end_sp: 1415.0,
                    duration_seconds: 2_000.0,
                    started_at_unix: None,
                },
            ],
        );
        snap.paused_skill_queues.remove(&char_id);
    });
    catch_up_skill_queue(&ctx, char_id);
    ctx.db.with_snapshot(|snap| {
        assert!((snap.characters[&char_id].skill_points - 5_000_000.0).abs() < 1e-3);
        assert!(snap.paused_skill_queues.contains(&char_id));
    });

    // 5. Omega subscription lapse downgrades clone_grade to 1 and pauses queue
    let uid = ctx.db.with_snapshot(|s| s.characters[&char_id].user_id);
    crate::services::account_services::with_store(&ctx, |_, store| {
        store.subscription_expiry.insert(uid, now.saturating_sub(60));
    });
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.clone_grade = 2;
        ch.skill_points = 100_000.0;
        snap.paused_skill_queues.insert(char_id);
    });
    catch_up_skill_queue(&ctx, char_id);
    assert_eq!(ctx.db.with_snapshot(|s| s.characters[&char_id].clone_grade), 1);
}

#[test]
fn save_skill_queue_mct_prereqs_alpha_caps_and_input_shapes() {
    let mut ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let char_b = 90_000_099_i64;
    let st_id = 60_003_760_i64;
    let base_sk = 941_010_u32;
    let adv_sk = 941_011_u32;
    clone_skill_type(&mut ctx.sde, base_sk, 1.0, None);
    clone_skill_type(&mut ctx.sde, adv_sk, 2.0, Some((base_sk, 2)));

    let mut sess = SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        corporation_id: Some(1_000_009),
        solar_system_id: Some(30_000_142),
        station_id: Some(st_id),
        ship_id: Some(100_000_001),
        ..Default::default()
    };

    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.clone_grade = 2;
        ch.skill_points = 50_000.0;
        ch.free_skill_points = 0.0;
        let mut cb = ch.clone();
        cb.character_id = char_b;
        cb.name = "Alt Trainer".into();
        snap.characters.insert(char_b, cb);

        // Give char_id both skills at level 0
        let smap = snap.skills.entry(char_id).or_default();
        smap.insert(
            base_sk,
            TrainedSkillRecord {
                type_id: base_sk,
                level: 0,
                skill_points: 0.0,
            },
        );
        smap.insert(
            adv_sk,
            TrainedSkillRecord {
                type_id: adv_sk,
                level: 0,
                skill_points: 0.0,
            },
        );

        // Start active training on char_b (same user_id = 1)
        snap.skill_queues.insert(
            char_b,
            vec![SkillQueueEntry {
                type_id: base_sk,
                target_level: 1,
                start_sp: 0.0,
                end_sp: 250.0,
                duration_seconds: 600.0,
                started_at_unix: Some(now),
            }],
        );
        snap.paused_skill_queues.remove(&char_b);
    });

    // 1. Without MCT, training on char_id while char_b is training fails with UserError
    let mct_err = call_skills(
        &ctx,
        &mut sess,
        "skillMgr2",
        "SaveSkillQueue",
        &[PyRep::Integer(base_sk as i64), PyRep::Integer(1)],
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(mct_err.code, "UserError");
    assert_eq!(mct_err.message, "CantTrainMultipleCharactersWithoutMCT");

    // 2. Grant active MCT slot -> SaveNewQueue succeeds
    ctx.db.with_mut_snapshot(|snap| {
        snap.service_state_blobs
            .insert("mct_slots:1".into(), serde_json::to_string(&vec![now + 86_400]).unwrap());
    });

    // 3. Queueing adv_sk without base_sk at level 2 fails with SkillPrerequisiteNotMet
    let pre_err = call_skills(
        &ctx,
        &mut sess,
        "skillMgr2",
        "SaveNewQueue",
        &[PyRep::List(vec![
            PyRep::Integer(adv_sk as i64),
            PyRep::Integer(1),
        ])],
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(pre_err.code, "SkillPrerequisiteNotMet");

    // 4. Queueing base_sk 1, base_sk 2, then adv_sk 1 in the same queue succeeds!
    let chain_ok = call_skills(
        &ctx,
        &mut sess,
        "skillMgr2",
        "SaveNewQueue",
        &[PyRep::List(vec![
            PyRep::Tuple(vec![PyRep::Integer(base_sk as i64), PyRep::Integer(1)]),
            PyRep::Tuple(vec![PyRep::Integer(base_sk as i64), PyRep::Integer(2)]),
            PyRep::Tuple(vec![PyRep::Integer(adv_sk as i64), PyRep::Integer(1)]),
        ])],
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        chain_ok.dict_get("typeID"),
        Some(&PyRep::Integer(adv_sk as i64))
    );

    // 5. Duplicate level in same queue fails with SkillAlreadyTrained
    let dup_err = call_skills(
        &ctx,
        &mut sess,
        "skillMgr2",
        "SaveSkillQueue",
        &[PyRep::List(vec![
            PyRep::Tuple(vec![PyRep::Integer(base_sk as i64), PyRep::Integer(1)]),
            PyRep::Tuple(vec![PyRep::Integer(base_sk as i64), PyRep::Integer(1)]),
        ])],
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(dup_err.code, "SkillAlreadyTrained");

    // 6. Alpha clone skill level cap & total SP cap on queue
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.clone_grade = 1;
        ch.skill_points = 4_999_900.0;
    });
    // Custom skill base_sk is not in Alpha clonegrade list -> alpha_cap is 0 -> AlphaCloneSkillLevelCapExceeded
    let alpha_lvl_err = call_skills(
        &ctx,
        &mut sess,
        "skillMgr2",
        "SaveSkillQueue",
        &[PyRep::Integer(base_sk as i64), PyRep::Integer(1)],
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(alpha_lvl_err.code, "AlphaCloneSkillLevelCapExceeded");

    // Gunnery (3300) is Alpha-trainable at level 1, but 4_999_900 + 250 > 5_000_000 -> AlphaCloneSpCapExceeded
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            3300,
            TrainedSkillRecord {
                type_id: 3300,
                level: 0,
                skill_points: 0.0,
            },
        );
    });
    let alpha_sp_err = call_skills(
        &ctx,
        &mut sess,
        "skillMgr2",
        "SaveSkillQueue",
        &[PyRep::Integer(3300), PyRep::Integer(1)],
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(alpha_sp_err.code, "AlphaCloneSpCapExceeded");
}

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
