#![forbid(unsafe_code)]

use super::*;
use crate::router::ServerContext;
use eve_db::NpeProgressRecord;
use eve_machonet::{SessionState, service_roles::ROLE_GMH};
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

const ERR_CHARACTER_NOT_FOUND: &str = "CharacterNotFound";
const ERR_INSUFFICIENT_ROLE: &str = "InsufficientRole";
const ERR_TUTORIAL_NOT_ACTIVE: &str = "TutorialNotActive";

fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(eve_db::WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

fn auth_sess(char_id: i64, sys_id: u32, is_gm: bool) -> SessionState {
    let mut s = SessionState::new(8101);
    s.user_id = Some(char_id as u64);
    s.character_id = Some(char_id);
    s.corporation_id = Some(98_000_001);
    s.solar_system_id = Some(sys_id);
    if is_gm {
        s.role |= ROLE_GMH;
    }
    s
}

#[test]
fn unauthenticated_calls_and_unknown_dispatch_paths() {
    let ctx = test_ctx();
    let mut unauth = SessionState::new(8102);
    let kw = HashMap::new();

    for (svc, meth) in [
        ("operationsManager", "get_tutorial_state"),
        ("survey", "GetActiveSurveyID"),
        ("eventLog", "LogClientEvent"),
    ] {
        let res = handle(&ctx, &mut unauth, svc, meth, &[], &kw)
            .expect("route must be dispatched");
        assert!(res.is_err(), "{svc}.{meth} must fail without character_id");
    }

    assert!(handle(&ctx, &mut unauth, "eventLog", "get_tutorial_state", &[], &kw).is_none());
    assert!(handle(&ctx, &mut unauth, "survey", "LogClientEvent", &[], &kw).is_none());
}

#[test]
fn tutorial_completion_eligibility_dungeon_and_spawnpoint_helpers() {
    let ctx = test_ctx();
    let cid = 90_000_001_i64;
    let unknown_cid = 999_888_777_i64;
    let now = ctx.now_unix();

    ctx.db.with_mut_snapshot(|snap| {
        // 1. NPE node_graph 716 node >= 4 but !completed => false; != 716 node == 4 => true; node == 3 => false
        snap.npe_progress.insert(
            cid,
            NpeProgressRecord {
                character_id: cid,
                node_graph_id: 716,
                current_node_id: 4,
                completed: false,
                career_goal_progress: HashMap::new(),
                claimed_goals: HashSet::new(),
            },
        );
        assert!(can_character_play_tutorial(snap, cid));
        snap.npe_progress.get_mut(&cid).unwrap().node_graph_id = 715;
        assert!(!can_character_play_tutorial(snap, cid));
        snap.npe_progress.get_mut(&cid).unwrap().current_node_id = 3;
        assert!(can_character_play_tutorial(snap, cid));

        // 2. tutorial_skipped = true prevents playing
        sync_air_npe_skip(snap, cid, now);
        assert!(!can_character_play_tutorial(snap, cid));

        // 3. tutorial_finished = Some(true) prevents playing even without skipped
        let mut store = load_store(snap);
        let rec = store.by_character.entry(cid).or_default();
        rec.tutorial_skipped = false;
        rec.tutorial_finished = Some(true);
        save_store(snap, &store, now);
        assert!(!can_character_play_tutorial(snap, cid));

        // 4. tutorial_finished = None with incomplete NPE => true
        let mut store2 = load_store(snap);
        store2.by_character.get_mut(&cid).unwrap().tutorial_finished = None;
        save_store(snap, &store2, now);
        assert!(can_character_play_tutorial(snap, cid));

        // 5. Missing ops record: depends on NPE presence/completion
        assert!(!can_character_play_tutorial(snap, unknown_cid));
        snap.npe_progress.insert(
            unknown_cid,
            NpeProgressRecord {
                character_id: unknown_cid,
                node_graph_id: 715,
                current_node_id: 1,
                completed: false,
                career_goal_progress: HashMap::new(),
                claimed_goals: HashSet::new(),
            },
        );
        assert!(can_character_play_tutorial(snap, unknown_cid));
        snap.npe_progress.remove(&unknown_cid);
    });
    assert!(!is_operation_site_or_system(&ctx, cid, 0));
    assert!(!is_operation_site_or_system(&ctx, unknown_cid, 30_000_142));

    // Missing dungeon in SDE returns None; missing school/char returns None
    ctx.db.with_mut_snapshot(|snap| {
        let mut store = load_store(snap);
        let rec = store.by_character.entry(cid).or_default();
        rec.active_operation_id = Some(77_777);
        rec.active_dungeon_id = Some(999_999_888);
        rec.active_operation_system_id = Some(30_000_142);
        save_store(snap, &store, now);
    });
    assert!(active_operation_dungeon_for_character(&ctx, cid).is_none());
    assert!(is_operation_site_or_system(&ctx, cid, 77_777));
    assert!(is_operation_site_or_system(&ctx, cid, 999_999_888));
    assert_eq!(
        resolve_operation_spawnpoint_solar_system(&ctx, cid, Some(77_777)),
        Some(30_000_142)
    );
    assert_eq!(
        resolve_operation_spawnpoint_solar_system(&ctx, cid, None),
        Some(30_000_142)
    );
    assert_ne!(
        resolve_operation_spawnpoint_solar_system(&ctx, cid, Some(88_888)),
        Some(30_000_142)
    );
    assert!(resolve_operation_spawnpoint_solar_system(&ctx, unknown_cid + 1, None).is_none());

    // Valid dungeon in SDE without dungeon_asteroids hits fallback dungeon_id * 100 + 1
    if let Some((&did, _)) = ctx
        .sde
        .dungeons
        .iter()
        .find(|(id, _)| !ctx.sde.dungeon_asteroids.contains_key(id))
    {
        ctx.db.with_mut_snapshot(|snap| {
            snap.npe_progress.remove(&cid);
            let mut store = load_store(snap);
            let rec = store.by_character.get_mut(&cid).unwrap();
            rec.active_dungeon_id = Some(did);
            rec.active_site_id = Some(did as i64 * 10 + 1);
            save_store(snap, &store, now);
        });
        let info = active_operation_dungeon_for_character(&ctx, cid).expect("dungeon info");
        assert_eq!(info.1, did * 100 + 1);
    }
}

#[test]
fn category_completion_progress_activate_and_task_manipulation_edges() {
    let ctx = test_ctx();
    let cid = 90_000_001_i64;
    let kw = HashMap::new();
    let mut sess = auth_sess(cid, 30_000_142, false);
    let mut gm = auth_sess(cid, 30_000_142, true);

    let op_defs = get_operation_defs(&ctx.sde);
    let op_id = op_defs
        .sde_operations
        .iter()
        .copied()
        .find(|oid| {
            op_defs.are_prerequisites_met(*oid, &HashSet::new())
                && op_defs.op_tasks.get(oid).is_some_and(|t| !t.is_empty())
                && op_defs.op_default_category.contains_key(oid)
        })
        .unwrap_or_else(|| *op_defs.sde_operations.iter().next().unwrap());
    let cat_id = op_defs
        .op_default_category
        .get(&op_id)
        .copied()
        .unwrap_or_else(|| *op_defs.sde_categories.iter().next().unwrap());
    let task_id = op_defs
        .op_tasks
        .get(&op_id)
        .and_then(|t| t.first().copied())
        .unwrap_or_else(|| *op_defs.sde_tasks.iter().next().unwrap());

    // Empty record progress & category queries
    let empty_prog = handle(&ctx, &mut sess, "operationsManager", "get_character_progress", &[], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(empty_prog, PyRep::Dict(Vec::new()));
    assert_eq!(
        handle(&ctx, &mut sess, "operationsManager", "get_active_category_id", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::None
    );

    // complete_category_for_character error paths
    let r = handle(
        &ctx,
        &mut gm,
        "operationsManager",
        "complete_category_for_character",
        &[PyRep::Integer(0), PyRep::Integer(cat_id)],
        &kw,
    )
    .unwrap();
    assert_eq!(r.unwrap_err().code, "CustomInfo");
    let r = handle(
        &ctx,
        &mut gm,
        "operationsManager",
        "complete_category_for_character",
        &[PyRep::Integer(cid), PyRep::Integer(0)],
        &kw,
    )
    .unwrap();
    assert_eq!(r.unwrap_err().code, "CustomInfo");
    let r = handle(
        &ctx,
        &mut gm,
        "operationsManager",
        "complete_category_for_character",
        &[PyRep::Integer(888_777_666), PyRep::Integer(cat_id)],
        &kw,
    )
    .unwrap();
    assert_eq!(r.unwrap_err().code, ERR_CHARACTER_NOT_FOUND);

    // activate_operation_in_solar_system error guards
    let r = handle(
        &ctx,
        &mut sess,
        "operationsManager",
        "activate_operation_in_solar_system",
        &[PyRep::Integer(0)],
        &kw,
    )
    .unwrap();
    assert_eq!(r.unwrap_err().code, "CustomInfo");
    let r = handle(
        &ctx,
        &mut sess,
        "operationsManager",
        "activate_operation_in_solar_system",
        &[PyRep::Integer(op_id), PyRep::Integer(30_002_187)],
        &kw,
    )
    .unwrap();
    assert_eq!(r.unwrap_err().code, "CustomInfo");
    let r = handle(
        &ctx,
        &mut sess,
        "operationsManager",
        "activate_operation_in_solar_system",
        &[PyRep::Integer(op_id), PyRep::Integer(30_000_142), PyRep::Integer(0)],
        &kw,
    )
    .unwrap();
    assert_eq!(r.unwrap_err().code, "CustomInfo");

    // Activate valid operation with explicit category (use gm in case prerequisites exist)
    let act_res = handle(
        &ctx,
        &mut gm,
        "operationsManager",
        "activate_operation_in_solar_system",
        &[PyRep::Integer(op_id), PyRep::Integer(30_000_142), PyRep::Integer(cat_id)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(act_res, PyRep::Bool(true));
    assert_eq!(
        handle(&ctx, &mut sess, "operationsManager", "get_active_category_id", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::Integer(cat_id)
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "operationsManager",
            "get_category_state",
            &[PyRep::Integer(cat_id)],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Integer(2)
    );

    // block_task / unblock_task invalid + 1-arg & 3-arg forms
    assert_eq!(
        handle(&ctx, &mut sess, "operationsManager", "block_task", &[PyRep::Integer(0)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "CustomInfo"
    );
    assert_eq!(
        handle(&ctx, &mut sess, "operationsManager", "unblock_task", &[PyRep::Integer(0)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "CustomInfo"
    );
    assert!(handle(
        &ctx,
        &mut sess,
        "operationsManager",
        "block_task",
        &[PyRep::Integer(cat_id), PyRep::Integer(op_id), PyRep::Integer(task_id)],
        &kw,
    )
    .unwrap()
    .is_ok());

    let prog = handle(&ctx, &mut sess, "operationsManager", "get_character_progress", &[], &kw)
        .unwrap()
        .unwrap();
    assert!(matches!(
        prog,
        PyRep::Dict(ref pairs)
            if pairs.iter().any(|(k, v)| k == &PyRep::Integer(task_id) && v == &PyRep::Integer(0))
    ));

    assert!(handle(
        &ctx,
        &mut sess,
        "operationsManager",
        "unblock_task",
        &[PyRep::Integer(task_id)],
        &kw,
    )
    .unwrap()
    .is_ok());

    // skip_task invalid + wrong system + wrong operation
    assert_eq!(
        handle(&ctx, &mut sess, "operationsManager", "skip_task", &[PyRep::Integer(0)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "CustomInfo"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "operationsManager",
            "skip_task",
            &[PyRep::Integer(cat_id), PyRep::Integer(op_id + 999), PyRep::Integer(task_id)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CustomInfo"
    );
    let mut wrong_sys = auth_sess(cid, 30_002_187, false);
    assert_eq!(
        handle(
            &ctx,
            &mut wrong_sys,
            "operationsManager",
            "skip_task",
            &[PyRep::Integer(task_id)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CustomInfo"
    );

    // GM completes category and clears active category & operation
    assert_eq!(
        handle(
            &ctx,
            &mut gm,
            "operationsManager",
            "complete_category_for_character",
            &[PyRep::Integer(cid), PyRep::Integer(cat_id)],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "operationsManager",
            "get_category_state",
            &[PyRep::Integer(cat_id)],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Integer(3)
    );
}

#[tokio::test]
async fn start_site_guards_client_events_and_event_log() {
    let ctx = test_ctx();
    let cid = 90_000_001_i64;
    let kw = HashMap::new();
    let mut sess = auth_sess(cid, 30_000_142, false);
    let mut gm = auth_sess(cid, 30_000_142, true);

    let r = handle(&ctx, &mut sess, "operationsManager", "start_site", &[PyRep::Integer(123)], &kw)
        .unwrap();
    assert_eq!(r.unwrap_err().code, "CustomInfo");

    for ev in [
        "complete_air_npe",
        "checkpoint_reached",
        "advance_checkpoint",
        "objective_completed",
    ] {
        let r = handle(
            &ctx,
            &mut sess,
            "operationsManager",
            "process_client_event",
            &[PyRep::String(ev.into())],
            &kw,
        )
        .unwrap();
        assert_eq!(r.unwrap_err().code, ERR_INSUFFICIENT_ROLE);
    }

    assert!(handle(
        &ctx,
        &mut sess,
        "operationsManager",
        "process_client_event",
        &[PyRep::String("set_career_path".into()), PyRep::Integer(3)],
        &kw,
    )
    .unwrap()
    .is_ok());

    assert!(handle(
        &ctx,
        &mut sess,
        "operationsManager",
        "process_client_event",
        &[
            PyRep::String("set_mission_avoidance_systems".into()),
            PyRep::List(vec![
                PyRep::Integer(30_000_142),
                PyRep::Integer(0),
                PyRep::Integer(30_000_142),
            ]),
        ],
        &kw,
    )
    .unwrap()
    .is_ok());
    assert!(handle(
        &ctx,
        &mut sess,
        "operationsManager",
        "process_client_event",
        &[
            PyRep::String("add_mission_avoidance_system".into()),
            PyRep::Integer(30_000_144),
        ],
        &kw,
    )
    .unwrap()
    .is_ok());

    assert!(handle(
        &ctx,
        &mut sess,
        "operationsManager",
        "process_client_event",
        &[PyRep::String("skip_tutorial".into())],
        &kw,
    )
    .unwrap()
    .is_ok());
    let r = handle(
        &ctx,
        &mut sess,
        "operationsManager",
        "process_client_event",
        &[PyRep::String("start_tutorial".into())],
        &kw,
    )
    .unwrap();
    assert_eq!(r.unwrap_err().code, ERR_TUTORIAL_NOT_ACTIVE);
    if let Some(starter_sys) = ctx
        .sde
        .schools
        .values()
        .map(|s| s.solar_system_id)
        .find(|&id| id > 0)
    {
        ctx.db.with_mut_snapshot(|snap| {
            snap.npe_progress.remove(&cid);
            let mut store = load_store(snap);
            let rec = store.by_character.entry(cid).or_default();
            rec.tutorial_skipped = false;
            rec.tutorial_finished = Some(false);
            save_store(snap, &store, ctx.now_unix());
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.solar_system_id = starter_sys;
            }
        });
        assert!(handle(
            &ctx,
            &mut gm,
            "operationsManager",
            "process_client_event",
            &[PyRep::String("start_tutorial".into())],
            &kw,
        )
        .unwrap()
        .is_ok());
    }

    for meth in ["LogClientEvent", "LogClientStats", "LogPlayerRequestedDisconnect"] {
        let ok = handle(
            &ctx,
            &mut sess,
            "eventLog",
            meth,
            &[PyRep::String("test_event".into())],
            &kw,
        )
        .unwrap()
        .unwrap();
        assert_eq!(ok, PyRep::None);
    }
    assert_eq!(
        handle(&ctx, &mut sess, "survey", "GetActiveSurveyID", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::None
    );
    assert_eq!(
        handle(&ctx, &mut sess, "survey", "PerformSurveyChecks", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::List(Vec::new())
    );
    assert_eq!(
        handle(&ctx, &mut sess, "survey", "ClaimSurveyRewards", &[PyRep::Integer(1)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidSurveyID"
    );
}

#[test]
fn activate_operation_station_structure_system_filters_and_start_site_execution() {
    let ctx = test_ctx();
    let cid = 90_000_001_i64;
    let op_sys = 30_000_142_u32;
    let other_sys = 30_002_187_u32;
    let kw = HashMap::new();
    let op_defs = get_operation_defs(&ctx.sde);

    // Find an SDE operation that resolves cleanly with `start_site`
    let char_race = ctx.db.with_snapshot(|s| s.characters[&cid].race_id);
    let (op_id, site_id) = op_defs
        .sde_operations
        .iter()
        .copied()
        .find_map(|oid| {
            let sid = op_defs
                .op_site_ids
                .get(&oid)
                .copied()
                .filter(|&s| s > 0)
                .unwrap_or_else(|| i64::from(*ctx.sde.dungeons.keys().next().unwrap()));
            op_defs
                .resolve_site_dungeon_id(&ctx.sde, sid, char_race)
                .map(|_| (oid, sid))
        })
        .expect("resolve op + site");

    let same_station = ctx
        .sde
        .stations
        .values()
        .find(|s| s.solar_system_id == op_sys)
        .map(|s| s.station_id)
        .unwrap_or(60_003_760);
    let other_station = ctx
        .sde
        .stations
        .values()
        .find(|s| s.solar_system_id != op_sys)
        .map(|s| s.station_id)
        .unwrap();
    let same_struct = 1_000_000_811_001_i64;
    let other_struct = 1_000_000_811_002_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut st_same = eve_db::UpwellStructureRecord::default();
        st_same.structure_id = same_struct;
        st_same.type_id = 35_832;
        st_same.owner_corp_id = 98_000_001;
        st_same.solar_system_id = op_sys;
        st_same.state = "ShieldVulnerable".into();
        snap.structures.insert(same_struct, st_same);

        let mut st_other = eve_db::UpwellStructureRecord::default();
        st_other.structure_id = other_struct;
        st_other.type_id = 35_832;
        st_other.owner_corp_id = 98_000_001;
        st_other.solar_system_id = other_sys;
        st_other.state = "ShieldVulnerable".into();
        snap.structures.insert(other_struct, st_other);
    });

    // Unmatched service returns None (kills `handle -> Some(Ok(Default::default()))`)
    let no_svc = ["unmatched", "_svc"].concat();
    assert!(handle(&ctx, &mut auth_sess(cid, op_sys, true), &no_svc, "noop", &[], &kw).is_none());

    let mut gm = auth_sess(cid, op_sys, true);

    // 1. Station in SAME solar system stays docked after activate_operation_in_solar_system (`s.solar_system_id == op_sys`)
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&cid).unwrap();
        ch.solar_system_id = op_sys;
        ch.station_id = Some(same_station);
        ch.structure_id = None;
    });
    assert_eq!(
        handle(
            &ctx,
            &mut gm,
            "operationsManager",
            "activate_operation_in_solar_system",
            &[PyRep::Integer(op_id), PyRep::Integer(i64::from(op_sys))],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(ctx.db.with_snapshot(|s| s.characters[&cid].station_id), Some(same_station));

    // 2. Station in OTHER solar system clears station_id to Space
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&cid).unwrap();
        ch.station_id = Some(other_station);
        ch.structure_id = None;
    });
    handle(
        &ctx,
        &mut gm,
        "operationsManager",
        "activate_operation_in_solar_system",
        &[PyRep::Integer(op_id), PyRep::Integer(i64::from(op_sys))],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(ctx.db.with_snapshot(|s| s.characters[&cid].station_id), None);

    // 3. Structure in SAME solar system keeps structure_id; structure in OTHER clears structure_id
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&cid).unwrap();
        ch.station_id = None;
        ch.structure_id = Some(same_struct);
    });
    handle(
        &ctx,
        &mut gm,
        "operationsManager",
        "activate_operation_in_solar_system",
        &[PyRep::Integer(op_id), PyRep::Integer(i64::from(op_sys))],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(ctx.db.with_snapshot(|s| s.characters[&cid].structure_id), Some(same_struct));

    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&cid).unwrap();
        ch.station_id = None;
        ch.structure_id = Some(other_struct);
    });
    handle(
        &ctx,
        &mut gm,
        "operationsManager",
        "activate_operation_in_solar_system",
        &[PyRep::Integer(op_id), PyRep::Integer(i64::from(op_sys))],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(ctx.db.with_snapshot(|s| s.characters[&cid].structure_id), None);

    // 4. Undocked `start_site` succeeds, updates active_site_id, and registers dungeon instance
    let dungeons_before = ctx.db.with_snapshot(|s| s.dungeon_runtime.instances.len());
    let start_res = handle(
        &ctx,
        &mut gm,
        "operationsManager",
        "start_site",
        &[PyRep::Integer(op_id), PyRep::Integer(site_id)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(start_res, PyRep::Bool(true));
    ctx.db.with_snapshot(|snap| {
        let store = load_store(snap);
        assert_eq!(store.by_character[&cid].active_site_id, Some(site_id));
        assert!(snap.dungeon_runtime.instances.len() > dungeons_before);
    });
}
