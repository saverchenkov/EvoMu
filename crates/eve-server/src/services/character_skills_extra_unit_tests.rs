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

pub(super) static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

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

pub(super) fn clone_skill_type(
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

pub(super) fn put_skillbook(
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

pub(super) fn call_skills(
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
fn save_new_queue_accepts_v24_dict_payload_activate_flag_and_paused_timestamps() {
    let mut ctx = test_ctx();
    let base_sk = 99_441u32;
    let adv_sk = 99_442u32;
    let char_id = 90_000_001i64;
    let now = 1_750_000_000u64;
    ctx.set_clock(Arc::new(eve_db::VirtualServerClock::with_fixed_base(now)));

    clone_skill_type(&mut ctx.sde, base_sk, 1.0, None);
    clone_skill_type(&mut ctx.sde, adv_sk, 1.0, Some((base_sk, 1)));

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.clone_grade = 2;
        }
        snap.skills.entry(char_id).or_default().remove(&base_sk);
        snap.skills.entry(char_id).or_default().remove(&adv_sk);
        snap.skill_queues.remove(&char_id);
        snap.paused_skill_queues.remove(&char_id);
    });

    let mut sess = SessionState::new(911);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);

    // 1. V24.01 `CommitTransaction` sends `{idx: (trainingTypeID, trainingToLevel)}` + `activate=False`.
    // Keys provided out of order (1 before 0) to verify position-key sorting & prereq ordering.
    let mut kw_pause = HashMap::new();
    kw_pause.insert("activate".to_string(), PyRep::Bool(false));
    let paused_dict = PyRep::Dict(vec![
        (
            PyRep::Integer(1),
            PyRep::Tuple(vec![PyRep::Integer(adv_sk as i64), PyRep::Integer(1)]),
        ),
        (
            PyRep::Integer(0),
            PyRep::Tuple(vec![PyRep::Integer(base_sk as i64), PyRep::Integer(1)]),
        ),
    ]);
    let saved_paused = handle(
        &ctx,
        &mut sess,
        "skillMgr2",
        "SaveNewQueue",
        &[paused_dict],
        &kw_pause,
    )
    .unwrap()
    .expect("SaveNewQueue dict with activate=False must succeed");
    assert_eq!(
        saved_paused.dict_get("typeID"),
        Some(&PyRep::Integer(adv_sk as i64))
    );

    ctx.db.with_snapshot(|snap| {
        assert!(snap.paused_skill_queues.contains(&char_id));
        let q = &snap.skill_queues[&char_id];
        assert_eq!(q.len(), 2);
        assert_eq!(q[0].type_id, base_sk);
        assert_eq!(q[1].type_id, adv_sk);
        assert!(q.iter().all(|e| e.started_at_unix.is_none()));
    });
    let eot_paused = handle(&ctx, &mut sess, "skillMgr2", "GetEndOfTraining", &[], &HashMap::new())
        .unwrap()
        .unwrap();
    assert_eq!(eot_paused, PyRep::None);

    let PyRep::List(rows) =
        handle(&ctx, &mut sess, "skillMgr2", "GetSkillQueue", &[], &HashMap::new())
            .unwrap()
            .unwrap()
    else {
        panic!("expected List of KeyVals from GetSkillQueue");
    };
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].dict_get("trainingStartTime"), Some(&PyRep::None));
    assert_eq!(rows[0].dict_get("trainingEndTime"), Some(&PyRep::None));

    // 2. V24.01 `CommitTransaction` with `activate=True` activates training on index 0.
    let _ = ctx.drain_notifications_for_char(char_id);
    let mut kw_active = HashMap::new();
    kw_active.insert("activate".to_string(), PyRep::Bool(true));
    let active_dict = PyRep::Dict(vec![
        (
            PyRep::Integer(0),
            PyRep::Tuple(vec![PyRep::Integer(base_sk as i64), PyRep::Integer(1)]),
        ),
        (
            PyRep::Integer(1),
            PyRep::Tuple(vec![PyRep::Integer(adv_sk as i64), PyRep::Integer(1)]),
        ),
    ]);
    handle(
        &ctx,
        &mut sess,
        "skillMgr2",
        "SaveNewQueue",
        &[active_dict],
        &kw_active,
    )
    .unwrap()
    .expect("SaveNewQueue dict with activate=True must succeed");

    let eot_active = handle(&ctx, &mut sess, "skillMgr2", "GetEndOfTraining", &[], &HashMap::new())
        .unwrap()
        .unwrap();
    assert!(eot_active.as_i64().is_some());

    let PyRep::Tuple(qf) = handle(
        &ctx,
        &mut sess,
        "skillMgr2",
        "GetSkillQueueAndFreePoints",
        &[],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap()
    else {
        panic!("expected Tuple");
    };
    let PyRep::List(qf_rows) = &qf[0] else {
        panic!("expected List of KeyVals in GetSkillQueueAndFreePoints[0]");
    };
    assert_eq!(qf_rows.len(), 2);
    let t0_start = qf_rows[0]
        .dict_get("trainingStartTime")
        .and_then(PyRep::as_i64)
        .expect("active trainingStartTime");
    let t0_end = qf_rows[0]
        .dict_get("trainingEndTime")
        .and_then(PyRep::as_i64)
        .expect("active trainingEndTime");
    let t1_start = qf_rows[1]
        .dict_get("trainingStartTime")
        .and_then(PyRep::as_i64)
        .expect("queued entry 1 trainingStartTime");
    assert_eq!(t1_start, t0_end);
    assert!(t0_end > t0_start);

    // 3. Advancing virtual clock past completion trains base_sk to level 1!
    ctx.set_clock(Arc::new(eve_db::VirtualServerClock::with_fixed_base(
        now + 10_000,
    )));
    catch_up_skill_queue(&ctx, char_id);
    ctx.db.with_snapshot(|snap| {
        let sk = snap
            .skills
            .get(&char_id)
            .and_then(|m| m.get(&base_sk))
            .expect("base_sk trained");
        assert!(sk.level >= 1);
    });
}
