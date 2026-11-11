//! Edge-case and branch-coverage unit tests for `services::character_skills_helpers`.

use super::*;
use crate::formulas::sp_for_skill_level;
use crate::router::ServerContext;
use eve_db::{InventoryFlag, InventoryItemRecord, SkillQueueEntry, TrainedSkillRecord, WorldRepository};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{attr_ids, InvType, SdeRegistry};
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn test_ctx() -> ServerContext {
    ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    )
    .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
}

fn add_skill_type(sde: &mut Arc<SdeRegistry>, tid: u32, rank: f64, prereq: Option<(u32, u8)>) {
    let reg = Arc::make_mut(sde);
    let mut rec: InvType = reg
        .types
        .get(&3300)
        .cloned()
        .or_else(|| reg.types.values().next().cloned())
        .expect("base sde type");
    rec.type_id = tid;
    rec.group_id = 255;
    rec.category_id = 16;
    rec.published = true;
    rec.name = format!("HelperSkill_{tid}");
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
    reg.types.insert(tid, rec);
}

fn char_sess(char_id: i64, station_id: Option<i64>, ship_id: Option<i64>) -> SessionState {
    let mut s = SessionState::new(99);
    s.user_id = Some(1);
    s.character_id = Some(char_id);
    s.corporation_id = Some(1_000_009);
    s.solar_system_id = Some(30_000_142);
    s.station_id = station_id;
    s.ship_id = ship_id;
    s
}

#[test]
fn parse_skills_and_levels_and_check_prerequisites_cover_all_error_branches() {
    let mut ctx = test_ctx();
    let sk_base = 951_001_u32;
    let sk_req = 951_002_u32;
    add_skill_type(&mut ctx.sde, sk_base, 1.0, None);
    add_skill_type(&mut ctx.sde, sk_req, 2.0, Some((sk_base, 3)));

    // parse_skills_and_levels_arg: Dict, List/Tuple, and errors
    let d_ok = PyRep::Dict(vec![
        (PyRep::Integer(sk_req as i64), PyRep::Integer(2)),
        (PyRep::Integer(sk_base as i64), PyRep::Integer(1)),
    ]);
    assert_eq!(
        parse_skills_and_levels_arg(Some(&d_ok)).unwrap(),
        vec![(sk_base, 1), (sk_req, 2)]
    );
    let l_ok = PyRep::List(vec![PyRep::Tuple(vec![
        PyRep::Integer(sk_base as i64),
        PyRep::Integer(4),
    ])]);
    assert_eq!(
        parse_skills_and_levels_arg(Some(&l_ok)).unwrap(),
        vec![(sk_base, 4)]
    );

    for bad in [
        None,
        Some(PyRep::Integer(1)),
        Some(PyRep::Dict(vec![(PyRep::Integer(0), PyRep::Integer(1))])),
        Some(PyRep::Dict(vec![(PyRep::Integer(100), PyRep::Integer(6))])),
        Some(PyRep::List(vec![PyRep::Integer(100)])),
        Some(PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Integer(-1),
            PyRep::Integer(1),
        ])])),
        Some(PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Integer(100),
            PyRep::Integer(0),
        ])])),
    ] {
        assert!(parse_skills_and_levels_arg(bad.as_ref()).is_err());
    }

    // check_skill_prerequisites: missing type, non-skill type, unmet prereq, met prereq
    let mut skills = HashMap::new();
    assert_eq!(
        check_skill_prerequisites(&ctx, &skills, 999_999_888).unwrap_err().code,
        "InvalidSkillType"
    );
    assert_eq!(
        check_skill_prerequisites(&ctx, &skills, 34).unwrap_err().code, // Tritanium
        "InvalidSkillType"
    );
    assert_eq!(
        check_skill_prerequisites(&ctx, &skills, sk_req).unwrap_err().code,
        "SkillPrerequisiteNotMet"
    );
    skills.insert(
        sk_base,
        TrainedSkillRecord {
            type_id: sk_base,
            level: 3,
            skill_points: sp_for_skill_level(1.0, 3),
        },
    );
    assert!(check_skill_prerequisites(&ctx, &skills, sk_req).is_ok());
}

#[test]
fn free_sp_queue_and_skills_and_start_training_cover_alpha_omega_partial_and_mct() {
    let mut ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let st_id = 60_003_760_i64;
    let sk_a = 951_010_u32;
    let sk_b = 951_011_u32;
    let sk_req = 951_012_u32;
    add_skill_type(&mut ctx.sde, sk_a, 1.0, None);
    add_skill_type(&mut ctx.sde, sk_b, 1.0, None);
    add_skill_type(&mut ctx.sde, sk_req, 1.0, Some((sk_a, 5)));

    let mut sess = char_sess(char_id, Some(st_id), Some(100_000_001));
    let kw = HashMap::new();

    // 1. ApplyFreeSkillPointsToQueue: InsufficientFreeSP, AlphaCloneSpCapExceeded
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.clone_grade = 1;
        ch.skill_points = 5_000_000.0;
        ch.free_skill_points = 0.0;
    });
    assert_eq!(
        handle_skill_tail(&ctx, &mut sess, "skillMgr2", "ApplyFreeSkillPointsToQueue", &[], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "InsufficientFreeSP"
    );
    assert_eq!(
        handle_skill_tail(&ctx, &mut sess, "skillMgr2", "GetFreeSkillPointsAppliedToQueue", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::Dict(Vec::new())
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().free_skill_points = 50_000.0;
    });
    assert_eq!(
        handle_skill_tail(&ctx, &mut sess, "skillMgr2", "ApplyFreeSkillPointsToQueue", &[], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "AlphaCloneSpCapExceeded"
    );
    assert_eq!(
        handle_skill_tail(
            &ctx,
            &mut sess,
            "skillMgr2",
            "ApplyFreeSkillPointsToSkills",
            &[PyRep::Dict(vec![(PyRep::Integer(sk_a as i64), PyRep::Integer(1))])],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "AlphaCloneSpCapExceeded"
    );

    // 2. Partial + full queue free SP application as Omega
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.clone_grade = 2;
        ch.skill_points = 10_000.0;
        ch.free_skill_points = 3_000.0; // enough to finish sk_a L1 (250 SP) + partial sk_b L3
        let smap = snap.skills.entry(char_id).or_default();
        smap.insert(
            sk_a,
            TrainedSkillRecord {
                type_id: sk_a,
                level: 0,
                skill_points: 0.0,
            },
        );
        smap.insert(
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
                    started_at_unix: None,
                    duration_seconds: 100.0,
                    start_sp: 0.0,
                    end_sp: 250.0,
                },
                SkillQueueEntry {
                    type_id: sk_b,
                    target_level: 3,
                    started_at_unix: None,
                    duration_seconds: 1000.0,
                    start_sp: 0.0,
                    end_sp: sp_for_skill_level(1.0, 3),
                },
            ],
        );
        snap.paused_skill_queues.remove(&char_id);
    });
    let est_q = handle_skill_tail(&ctx, &mut sess, "skillMgr2", "GetFreeSkillPointsAppliedToQueue", &[], &kw)
        .unwrap()
        .unwrap();
    assert!(matches!(est_q, PyRep::Dict(ref d) if d.len() == 2));
    assert!(handle_skill_tail(&ctx, &mut sess, "skillMgr2", "ApplyFreeSkillPointsToQueue", &[], &kw)
        .unwrap()
        .is_ok());

    // 3. ApplyFreeSkillPointsToSkills: empty, not injected, unmet prereq, valid partial+queue update
    assert_eq!(
        handle_skill_tail(
            &ctx,
            &mut sess,
            "skillMgr2",
            "ApplyFreeSkillPointsToSkills",
            &[PyRep::Dict(Vec::new())],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidArgument"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().free_skill_points = 5_000.0;
    });
    assert_eq!(
        handle_skill_tail(
            &ctx,
            &mut sess,
            "skillMgr2",
            "ApplyFreeSkillPointsToSkills",
            &[PyRep::Dict(vec![(PyRep::Integer(999_888), PyRep::Integer(1))])],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CannotApplyFreePointsDoNotHaveSkill"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            sk_req,
            TrainedSkillRecord {
                type_id: sk_req,
                level: 0,
                skill_points: 0.0,
            },
        );
    });
    assert_eq!(
        handle_skill_tail(
            &ctx,
            &mut sess,
            "skillMgr2",
            "ApplyFreeSkillPointsToSkills",
            &[PyRep::Dict(vec![(PyRep::Integer(sk_req as i64), PyRep::Integer(1))])],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "SkillPrerequisiteNotMet"
    );
    assert_eq!(
        handle_skill_tail(&ctx, &mut sess, "skillMgr2", "GetFreeSkillPointsAppliedToSkills", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::Dict(Vec::new())
    );
    let est_skills = handle_skill_tail(
        &ctx,
        &mut sess,
        "skillMgr2",
        "GetFreeSkillPointsAppliedToSkills",
        &[PyRep::Dict(vec![(PyRep::Integer(sk_b as i64), PyRep::Integer(3))])],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert!(matches!(est_skills, PyRep::Dict(ref d) if !d.is_empty()));
    assert!(handle_skill_tail(
        &ctx,
        &mut sess,
        "skillMgr2",
        "ApplyFreeSkillPointsToSkills",
        &[PyRep::Dict(vec![(PyRep::Integer(sk_b as i64), PyRep::Integer(3))])],
        &kw,
    )
    .unwrap()
    .is_ok());

    // 4. CharStartTrainingSkill: invalid arg, item lookup, max level, MCT pause vs non-pause
    assert_eq!(
        handle_skill_tail(&ctx, &mut sess, "skillMgr2", "CharStartTrainingSkill", &[], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    let alt_char = 90_000_002_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.paused_skill_queues.remove(&alt_char);
        snap.items.insert(
            980_001,
            InventoryItemRecord {
                item_id: 980_001,
                type_id: sk_a,
                owner_id: char_id,
                location_id: st_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    assert!(handle_skill_tail(
        &ctx,
        &mut sess,
        "skillMgr2",
        "CharStartTrainingSkill",
        &[PyRep::Integer(980_001)],
        &kw,
    )
    .unwrap()
    .is_ok());
    // With MCT active, other char is NOT paused
    let future = ctx.now_unix() + 86_400;
    ctx.db.with_mut_snapshot(|snap| {
        let uid = snap.characters[&char_id].user_id;
        snap.cluster_and_runtime.mct_slots.insert(uid, vec![future]);
        snap.paused_skill_queues.remove(&alt_char);
    });
    assert!(handle_skill_tail(
        &ctx,
        &mut sess,
        "skillMgr2",
        "CharStartTrainingSkill",
        &[PyRep::Integer(sk_a as i64)],
        &kw,
    )
    .unwrap()
    .is_ok());
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.paused_skill_queues.contains(&alt_char));
    });
    // Alpha skill cap / Omega max level error
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().get_mut(&sk_a).unwrap().level = 5;
    });
    assert_eq!(
        handle_skill_tail(
            &ctx,
            &mut sess,
            "skillMgr2",
            "CharStartTrainingSkill",
            &[PyRep::Integer(sk_a as i64)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "SkillAlreadyAtMaxLevel"
    );
}

#[test]
fn injectors_split_combine_diminished_sp_bundle_and_sp_getters() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let st_id = 60_003_760_i64;
    let mut sess = char_sess(char_id, Some(st_id), None);
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.clone_grade = 2;
        ch.skill_points = 10_000_000.0;
        // Large injector stack (qty 3)
        snap.items.insert(
            990_001,
            InventoryItemRecord {
                item_id: 990_001,
                type_id: TYPE_LARGE_SKILL_INJECTOR,
                owner_id: char_id,
                location_id: st_id,
                flag: InventoryFlag::Hangar,
                quantity: 3,
                singleton: false,
                ..Default::default()
            },
        );
        // Small injector stack (qty 12)
        snap.items.insert(
            990_002,
            InventoryItemRecord {
                item_id: 990_002,
                type_id: TYPE_SMALL_SKILL_INJECTOR,
                owner_id: char_id,
                location_id: st_id,
                flag: InventoryFlag::Hangar,
                quantity: 12,
                singleton: false,
                ..Default::default()
            },
        );
        // Daily Alpha Injector
        snap.items.insert(
            990_003,
            InventoryItemRecord {
                item_id: 990_003,
                type_id: crate::services::implant_plugging::TYPE_DAILY_ALPHA_INJECTOR,
                owner_id: char_id,
                location_id: st_id,
                flag: InventoryFlag::Hangar,
                quantity: 2,
                singleton: false,
                ..Default::default()
            },
        );
        // Non-injector item (Tritanium)
        snap.items.insert(
            990_004,
            InventoryItemRecord {
                item_id: 990_004,
                type_id: 34,
                owner_id: char_id,
                location_id: st_id,
                flag: InventoryFlag::Hangar,
                quantity: 10,
                singleton: false,
                ..Default::default()
            },
        );
        // Unowned injector
        snap.items.insert(
            990_005,
            InventoryItemRecord {
                item_id: 990_005,
                type_id: TYPE_LARGE_SKILL_INJECTOR,
                owner_id: 90_000_002,
                location_id: st_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: false,
                ..Default::default()
            },
        );
    });

    // CheckInjectionConstraints errors and successes
    assert_eq!(
        handle_skill_tail(&ctx, &mut sess, "skillMgr2", "CheckInjectionConstraints", &[], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    assert_eq!(
        handle_skill_tail(
            &ctx,
            &mut sess,
            "skillMgr2",
            "CheckInjectionConstraints",
            &[PyRep::Integer(990_001), PyRep::Integer(0)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidSkillPointAmount"
    );
    assert_eq!(
        handle_skill_tail(
            &ctx,
            &mut sess,
            "skillMgr2",
            "CheckInjectionConstraints",
            &[PyRep::Integer(999_999_111), PyRep::Integer(1)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InjectorNotFound"
    );
    assert_eq!(
        handle_skill_tail(
            &ctx,
            &mut sess,
            "skillMgr2",
            "CheckInjectionConstraints",
            &[PyRep::Integer(990_005), PyRep::Integer(1)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InjectorNotAccessible"
    );
    assert_eq!(
        handle_skill_tail(
            &ctx,
            &mut sess,
            "skillMgr2",
            "CheckInjectionConstraints",
            &[PyRep::Integer(990_001), PyRep::Integer(99)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InsufficientInjectors"
    );
    assert_eq!(
        handle_skill_tail(
            &ctx,
            &mut sess,
            "skillMgr2",
            "CheckInjectionConstraints",
            &[PyRep::Integer(990_004), PyRep::Integer(1)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidSkillInjector"
    );
    // Daily alpha injector on Omega -> AlphaCloneRequired
    assert_eq!(
        handle_skill_tail(
            &ctx,
            &mut sess,
            "skillMgr2",
            "CheckInjectionConstraints",
            &[PyRep::Integer(990_003), PyRep::Integer(1)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "AlphaCloneRequired"
    );
    // Alpha checks: qty != 1 -> cooldown error; cooldown active -> cooldown error; clear -> Ok
    let now_ft = ctx.now_blueos_filetime_ticks();
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().clone_grade = 1;
        crate::services::implant_plugging::save_alpha_injector_cooldown(snap, char_id, now_ft);
    });
    assert_eq!(
        handle_skill_tail(
            &ctx,
            &mut sess,
            "skillMgr2",
            "CheckInjectionConstraints",
            &[PyRep::Integer(990_003), PyRep::Integer(2)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "DailyAlphaInjectorCooldownActive"
    );
    assert_eq!(
        handle_skill_tail(
            &ctx,
            &mut sess,
            "skillMgr2",
            "CheckInjectionConstraints",
            &[PyRep::Integer(990_003), PyRep::Integer(1)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "DailyAlphaInjectorCooldownActive"
    );
    assert!(handle_skill_tail(
        &ctx,
        &mut sess,
        "skillMgr2",
        "CheckInjectionConstraints",
        &[PyRep::Integer(990_001), PyRep::Integer(1)],
        &kw,
    )
    .unwrap()
    .is_ok());

    // Split & Combine injectors (partial + full + type error)
    assert_eq!(
        handle_skill_tail(
            &ctx,
            &mut sess,
            "skillMgr2",
            "SplitSkillInjector",
            &[PyRep::Integer(990_002), PyRep::Integer(1)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidSkillInjector"
    );
    assert!(handle_skill_tail(
        &ctx,
        &mut sess,
        "skillMgr2",
        "SplitSkillInjector",
        &[PyRep::Integer(990_001), PyRep::Integer(1)],
        &kw,
    )
    .unwrap()
    .is_ok());
    assert!(handle_skill_tail(
        &ctx,
        &mut sess,
        "skillMgr2",
        "SplitSkillInjector",
        &[PyRep::Integer(990_001), PyRep::Integer(2)],
        &kw,
    )
    .unwrap()
    .is_ok());
    assert_eq!(
        handle_skill_tail(
            &ctx,
            &mut sess,
            "skillMgr2",
            "CombineSkillInjector",
            &[PyRep::Integer(990_002), PyRep::Integer(4)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CombineSkillInjectorTooFewInjectors"
    );
    assert!(handle_skill_tail(
        &ctx,
        &mut sess,
        "skillMgr2",
        "CombineSkillInjector",
        &[PyRep::Integer(990_002), PyRep::Integer(5)],
        &kw,
    )
    .unwrap()
    .is_ok());

    // GetDiminishedSpFromInjectors
    assert_eq!(
        handle_skill_tail(
            &ctx,
            &mut sess,
            "skillMgr2",
            "GetDiminishedSpFromInjectors",
            &[PyRep::Integer(TYPE_LARGE_SKILL_INJECTOR as i64), PyRep::Integer(0)],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Integer(0)
    );
    assert_eq!(
        handle_skill_tail(
            &ctx,
            &mut sess,
            "skillMgr2",
            "GetDiminishedSpFromInjectors",
            &[
                PyRep::Integer(crate::services::implant_plugging::TYPE_DAILY_ALPHA_INJECTOR as i64),
                PyRep::Integer(2),
            ],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Integer(100_000)
    );
    let dim_mix = handle_skill_tail(
        &ctx,
        &mut sess,
        "skillMgr2",
        "GetDiminishedSpFromInjectors",
        &[
            PyRep::Integer(TYPE_SMALL_SKILL_INJECTOR as i64),
            PyRep::Integer(3),
            PyRep::Integer(1),
        ],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert!(matches!(dim_mix, PyRep::Integer(n) if n > 100_000));

    // GetEstimatedSkillPoints / GetSkillPoints / GetSkillLevel
    assert!(matches!(
        handle_skill_tail(&ctx, &mut sess, "skillMgr2", "GetEstimatedSkillPoints", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::Real(v) if v >= 10_000_000.0
    ));
    assert!(matches!(
        handle_skill_tail(&ctx, &mut sess, "skillMgr2", "GetSkillPoints", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::Real(v) if v >= 10_000_000.0
    ));
    assert_eq!(
        handle_skill_tail(
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

    // GetSkillBundleInfo: invalid arg, unknown id, valid id from catalog
    assert_eq!(
        handle_skill_tail(&ctx, &mut sess, "skillMgr2", "GetSkillBundleInfo", &[], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidSkillBundle"
    );
    assert_eq!(
        handle_skill_tail(
            &ctx,
            &mut sess,
            "skillMgr2",
            "GetSkillBundleInfo",
            &[PyRep::Integer(999_999_999)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidSkillBundle"
    );
    if let Some((&valid_bundle_id, _)) = skill_bundle_catalog(&ctx).iter().next() {
        let ok = handle_skill_tail(
            &ctx,
            &mut sess,
            "skillMgr2",
            "GetSkillBundleInfo",
            &[PyRep::Integer(valid_bundle_id)],
            &kw,
        )
        .unwrap()
        .unwrap();
        assert!(matches!(ok, PyRep::Tuple(ref t) if t.len() == 2));
    }

    // Delegated routes in handle_skill_tail
    for (svc, meth) in [
        ("skillMgr2", "GetImplants"),
        ("skillMgr2", "GetBoosters"),
        ("skillMgr2", "GetSkillChangesForISIS"),
    ] {
        assert!(handle_skill_tail(&ctx, &mut sess, svc, meth, &[], &kw).is_some());
    }
    assert!(handle_skill_tail(&ctx, &mut sess, "machoNet", "GetTime", &[], &kw).is_none());
}

#[test]
fn mutant_kill_injector_ship_cargo_active_ship_and_start_training_edges() {
    let mut ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let alt_char = 90_000_002_i64;
    let ship_id = 880_500_001_i64;
    let large_id = 880_500_002_i64;
    let small_id = 880_500_003_i64;
    let sk_first = 951_701_u32;
    let sk_second = 951_702_u32;
    add_skill_type(&mut ctx.sde, sk_first, 1.0, None);
    add_skill_type(&mut ctx.sde, sk_second, 1.0, None);

    let now = ctx.now_unix();
    ctx.db.acquire_online_lock(1, 701, Some(char_id), true, now).unwrap();

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters[&char_id].clone();
        ch.station_id = None;
        ch.structure_id = None;
        ch.solar_system_id = 30_000_142;
        ch.active_ship_id = ship_id;
        ch.clone_grade = 2;
        snap.characters.insert(char_id, ch.clone());

        let mut alt = ch;
        alt.character_id = alt_char;
        snap.characters.insert(alt_char, alt);

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
            large_id,
            InventoryItemRecord {
                item_id: large_id,
                type_id: TYPE_LARGE_SKILL_INJECTOR,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 2,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            small_id,
            InventoryItemRecord {
                item_id: small_id,
                type_id: TYPE_SMALL_SKILL_INJECTOR,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 6,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );

        let sk_map = snap.skills.entry(char_id).or_default();
        sk_map.insert(sk_first, TrainedSkillRecord { type_id: sk_first, level: 1, skill_points: 250.0 });
        sk_map.insert(sk_second, TrainedSkillRecord { type_id: sk_second, level: 1, skill_points: 250.0 });
        snap.skill_queues.insert(
            char_id,
            vec![SkillQueueEntry {
                type_id: sk_first,
                target_level: 2,
                started_at_unix: Some(now),
                duration_seconds: 100.0,
                start_sp: 250.0,
                end_sp: 1_415.0,
            }],
        );
        snap.paused_skill_queues.insert(char_id);
        snap.paused_skill_queues.remove(&alt_char);
        snap.cluster_and_runtime.mct_slots.clear();
    });

    // Pilot is in space (station_id = None, ship_id = Some(ship_id)); ship Cargo requires active_ship_id
    let mut sess = char_sess(char_id, None, Some(ship_id));
    let kw = HashMap::new();

    // Split 1 Large Skill Injector in ship cargo -> consumes 1 Large, increments Small in ship cargo (6 -> 11)
    assert_eq!(
        handle_skill_tail(
            &ctx,
            &mut sess,
            "skillMgr2",
            "SplitSkillInjector",
            &[PyRep::Integer(large_id), PyRep::Integer(1)],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::None
    );
    assert_eq!(ctx.db.get_item(large_id).unwrap().quantity, 1);
    assert_eq!(ctx.db.get_item(small_id).unwrap().quantity, 11);

    // Combine 5 Small Skill Injectors in ship cargo -> consumes 5 Small (11 -> 6), increments Large (1 -> 2)
    assert_eq!(
        handle_skill_tail(
            &ctx,
            &mut sess,
            "skillMgr2",
            "CombineSkillInjector",
            &[PyRep::Integer(small_id), PyRep::Integer(5)],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::None
    );
    assert_eq!(ctx.db.get_item(small_id).unwrap().quantity, 6);
    assert_eq!(ctx.db.get_item(large_id).unwrap().quantity, 2);

    // CharStartTrainingSkill prepends sk_second as head, clears prev head started_at_unix, pauses alt char & notifies
    let _ = ctx.drain_notifications_for_char(char_id);
    let _ = ctx.drain_notifications_for_char(alt_char);
    assert_eq!(
        handle_skill_tail(
            &ctx,
            &mut sess,
            "skillMgr2",
            "CharStartTrainingSkill",
            &[PyRep::Integer(i64::from(sk_second))],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::None
    );
    ctx.db.with_snapshot(|snap| {
        let q = &snap.skill_queues[&char_id];
        assert_eq!(q.len(), 2);
        assert_eq!(q[0].type_id, sk_second);
        assert!(q[0].started_at_unix.is_some());
        assert_eq!(q[1].type_id, sk_first);
        assert_eq!(q[1].started_at_unix, None);
        assert!(!snap.paused_skill_queues.contains(&char_id));
        assert!(snap.paused_skill_queues.contains(&alt_char));
    });
    assert!(ctx
        .drain_notifications_for_char(char_id)
        .iter()
        .any(|n| n.event == "OnSkillQueuePausedServer" && n.args == vec![PyRep::Bool(false)]));
    assert!(ctx.drain_notifications_for_char(alt_char).is_empty());
}
