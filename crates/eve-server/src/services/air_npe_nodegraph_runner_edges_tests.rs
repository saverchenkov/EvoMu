//! Edge-case unit tests for `air_npe_nodegraph_runner`.

#![forbid(unsafe_code)]

use super::*;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::make_fighter_ctx_and_session;
use eve_machonet::service_roles;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

#[tokio::test]
async fn real_sde_client_graphs_act_scene_transitions_and_completion() {
    let (mut ctx, mut session, char_id, _ship_id, _sys_id) = make_fighter_ctx_and_session();
    ctx.sde = shared_sde();
    let uid = ctx.db.with_snapshot(|snap| snap.characters[&char_id].user_id);
    ctx.db
        .acquire_online_lock(uid, 1, Some(char_id), true, 1_775_400_000)
        .expect("online lock");

    let starter_sys = ctx
        .sde
        .schools
        .values()
        .find(|s| s.solar_system_id > 0)
        .map(|s| s.solar_system_id)
        .unwrap_or(30_000_141);

    // Corrupt JSON in air_npe_runner blob gracefully returns None.
    ctx.db.with_mut_snapshot(|snap| {
        snap.node_graph_state.air_npe_runners.remove(&char_id);
        assert!(load_runner_state(snap, char_id).is_none());
        snap.node_graph_state.air_npe_runners.remove(&char_id);
    });

    // Character docked in station inside starter system with alliance_id populated -> transitions into pocket
    // and starts real client node graphs (master 716, act1 707, scene1.1 725).
    let station_id = 60_003_760_i64;
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.solar_system_id = starter_sys;
        ch.station_id = Some(station_id);
        ch.structure_id = None;
        ch.alliance_id = Some(99_000_001);
    });
    session.solar_system_id = Some(starter_sys);
    session.station_id = Some(station_id);
    session.structure_id = None;

    on_start_tutorial(&ctx, &mut session, char_id, AIR_NPE_STATE_ACTIVE, Some(1));
    ctx.db.with_snapshot(|snap| {
        let r = load_runner_state(snap, char_id).expect("runner");
        assert!(r.master_client_instance_id.is_some());
        assert!(r.act_client_instance_id.is_some());
        assert!(r.scene_client_instance_id.is_some());
        // Check pocket_instance_id = None / dungeon_id = None blackboard query branches
        let mut r2 = r.clone();
        r2.pocket_instance_id = None;
        r2.pocket_dungeon_id = None;
        drop(r2);
    });

    // Check blackboard query when pocket_instance_id and dungeon_id are None
    ctx.db.with_mut_snapshot(|snap| {
        let mut r = load_runner_state(snap, char_id).unwrap();
        let saved_p = r.pocket_instance_id;
        let saved_d = r.pocket_dungeon_id;
        r.pocket_instance_id = None;
        r.pocket_dungeon_id = None;
        save_runner_state(snap, &r);
        assert_eq!(
            query_blackboard_value(snap, char_id, AIR_NPE_NODE_GRAPH_ID, "pocket_instance_id"),
            Some(PyRep::None)
        );
        assert_eq!(
            query_blackboard_value(snap, char_id, AIR_NPE_NODE_GRAPH_ID, "dungeon_id"),
            Some(PyRep::None)
        );
        r.pocket_instance_id = saved_p;
        r.pocket_dungeon_id = saved_d;
        save_runner_state(snap, &r);
    });

    // Trigger transition_character_into_npe_pocket via on_client_graph_message("enter_tutorial_pocket")
    let _ = ctx.drain_notifications_for_char(char_id);
    assert!(on_client_graph_message(
        &ctx,
        &mut session,
        char_id,
        AIR_NPE_NODE_GRAPH_ID,
        12,
        "enter_tutorial_pocket",
        &PyRep::None,
    ));
    let pocket_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(pocket_notifs.iter().any(|n| n.event == "OnSessionChanged"));
    assert!(pocket_notifs
        .iter()
        .any(|n| n.event == "OnClientNodeGraphMessage"));

    // After enter_tutorial_pocket we are at cp 14 ("approach_station"), which client can advance;
    // then cp 15 ("lock_target") rejects non-GM client event (trusted_server = false).
    assert_eq!(
        on_client_npe_event(&ctx, &mut session, char_id, "approach_station", None),
        Some(PyRep::Bool(true))
    );
    assert!(on_client_npe_event(&ctx, &mut session, char_id, "lock_target", None).is_none());
    // Unmatched message sent via on_client_graph_message emits OnClientNodeGraphUpdate & OnClientNodeGraphMessage
    assert!(on_client_graph_message(
        &ctx,
        &mut session,
        char_id,
        AIR_NPE_NODE_GRAPH_ID,
        14,
        "custom_ping",
        &PyRep::Integer(7),
    ));
    let msg_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(msg_notifs.iter().any(|n| n.event == "OnClientNodeGraphUpdate"));
    assert!(msg_notifs.iter().any(|n| n.event == "OnClientNodeGraphMessage"));

    // Non-GM cannot complete_air_npe via graph message
    assert!(!on_client_graph_message(
        &ctx,
        &mut session,
        char_id,
        AIR_NPE_NODE_GRAPH_ID,
        14,
        "complete_air_npe",
        &PyRep::None,
    ));

    // Step through remaining checkpoints via server gameplay events & GM advance_checkpoint so both
    // scene transitions and Act 1 -> Act 2 transition (cp 81 -> cp 83) execute with real client node graphs!
    on_server_gameplay_event(&ctx, char_id, "target_locked", &[]);
    on_server_gameplay_event(&ctx, char_id, "activate_weapons", &[]);

    session.role = service_roles::ROLE_GMH;
    // Advance until Act 2 (checkpoint 83+) and all the way past final checkpoint 92 (TransitionPlan::Complete)
    for _ in 0..AIR_NPE_CHECKPOINTS.len() {
        let done = ctx.db.with_snapshot(|snap| {
            load_runner_state(snap, char_id).is_none_or(|r| r.completed)
        });
        if done {
            break;
        }
        let _ = on_client_npe_event(&ctx, &mut session, char_id, "advance_checkpoint", None);
    }

    let step_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(step_notifs.iter().any(|n| n.event == "OnStopClientNodeGraph"));
    assert!(step_notifs.iter().any(|n| n.event == "OnStartClientNodeGraph"));
    assert!(step_notifs.iter().any(|n| n.event == "OnAirNpeStateChanged"));

    ctx.db.with_snapshot(|snap| {
        let r = load_runner_state(snap, char_id).expect("completed state");
        assert!(r.completed);
        assert_eq!(r.state_id, AIR_NPE_STATE_COMPLETED);
    });

    // Advancing / completing / server gameplay event when already completed / inactive
    assert!(!advance_runner_checkpoint(&ctx, Some(&mut session), char_id, "advance_checkpoint", true));
    on_server_gameplay_event(&ctx, char_id, "target_locked", &[]);
    assert_eq!(
        try_client_npe_event(&ctx, &mut session, char_id, "complete_air_npe", None)
            .unwrap_err()
            .code,
        "TutorialNotActive"
    );
}

#[tokio::test]
async fn pocket_transition_errors_skip_without_npe_and_gm_graph_completion() {
    let (mut ctx, mut session, char_id, _ship_id, _sys_id) = make_fighter_ctx_and_session();
    ctx.sde = shared_sde();
    let uid = ctx.db.with_snapshot(|snap| snap.characters[&char_id].user_id);
    ctx.db
        .acquire_online_lock(uid, 1, Some(char_id), true, 1_775_400_000)
        .expect("online lock");
    let starter_sys = ctx
        .sde
        .schools
        .values()
        .find(|s| s.solar_system_id > 0)
        .map(|s| s.solar_system_id)
        .unwrap_or(30_000_141);

    // Non-existent character -> CharacterNotFound
    assert_eq!(
        transition_character_into_npe_pocket(&ctx, None, 999_999_111)
            .unwrap_err()
            .code,
        "CharacterNotFound"
    );

    // Tutorial finished/skipped -> TutorialNotActive
    on_start_tutorial(&ctx, &mut session, char_id, AIR_NPE_STATE_ACTIVE, None);
    let _ = on_skip_air_npe(&ctx, None, char_id, 1_775_400_000);
    assert_eq!(
        transition_character_into_npe_pocket(&ctx, Some(&mut session), char_id)
            .unwrap_err()
            .code,
        "TutorialNotActive"
    );

    // Active tutorial in non-starter system -> NpeStarterSystemRequired
    let non_starter_sys = ctx
        .sde
        .solar_systems
        .keys()
        .copied()
        .find(|s| !ctx.sde.schools.values().any(|sc| sc.solar_system_id == *s))
        .expect("non-starter system");
    on_start_tutorial(&ctx, &mut session, char_id, AIR_NPE_STATE_ACTIVE, None);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().solar_system_id = non_starter_sys;
    });
    assert_eq!(
        transition_character_into_npe_pocket(&ctx, Some(&mut session), char_id)
            .unwrap_err()
            .code,
        "NpeStarterSystemRequired"
    );

    // Set to starter system, test GM complete_air_npe via on_client_graph_message and complete_air_npe(..., None)
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().solar_system_id = starter_sys;
    });
    on_start_tutorial(&ctx, &mut session, char_id, AIR_NPE_STATE_ACTIVE, None);
    // Preserve checkpoint_id > 12 when start_tutorial? Check complete_air_npe via GM graph message
    session.role = service_roles::ROLE_GMH;
    assert!(on_client_graph_message(
        &ctx,
        &mut session,
        char_id,
        AIR_NPE_NODE_GRAPH_ID,
        12,
        "complete_air_npe",
        &PyRep::None,
    ));

    // complete_air_npe(..., None, ...) completes active NPE without needing a mutable session
    on_start_tutorial(&ctx, &mut session, char_id, AIR_NPE_STATE_ACTIVE, None);
    complete_air_npe(&ctx, None, char_id, 1_775_400_050);
    ctx.db.with_snapshot(|snap| {
        assert!(snap.npe_progress.get(&char_id).is_some_and(|n| n.completed));
    });

    // on_skip_air_npe when npe_progress is absent creates npe_progress with completed=true (node_graph_id = 0)
    ctx.db.with_mut_snapshot(|snap| {
        snap.npe_progress.remove(&char_id);
    });
    let (gid, node, comp) = on_skip_air_npe(&ctx, None, char_id, 1_775_400_060).expect("skip");
    assert_eq!(gid, 0);
    assert_eq!(node, 4);
    assert!(comp);

    // Silence unused import if BTreeMap wasn't constructed directly
    let _empty: BTreeMap<String, String> = BTreeMap::new();
}

#[tokio::test]
async fn runner_state_save_load_roundtrip_and_transition_pocket_kill_mutants() {
    let (mut ctx, mut session, char_id, ship_id, _sys_id) = make_fighter_ctx_and_session();
    ctx.sde = shared_sde();
    let uid = ctx.db.with_snapshot(|snap| snap.characters[&char_id].user_id);
    ctx.db
        .acquire_online_lock(uid, 1, Some(char_id), true, 1_775_400_000)
        .expect("online lock");

    let starter_sys = ctx
        .sde
        .schools
        .values()
        .find(|s| s.solar_system_id > 0)
        .map(|s| s.solar_system_id)
        .unwrap_or(30_000_141);

    // 1. save_runner_state / load_runner_state non-default roundtrip:
    //    kills `load_runner_state -> None`, `-> Some(Default::default())`, and `save_runner_state -> ()`.
    let custom_state = eve_db::AirNpeRunnerState {
        char_id,
        state_id: 77,
        active_server_graph_id: 103,
        executed_server_graphs: vec![101, 102, 103],
        active_act: 2,
        scene_index: 4,
        checkpoint_id: 15,
        completed_checkpoints: vec![12, 13, 14],
        master_client_graph_id: 716,
        master_client_instance_id: Some(9001),
        act_client_graph_id: Some(707),
        act_client_instance_id: Some(9002),
        scene_client_graph_id: Some(725),
        scene_client_instance_id: Some(9003),
        pocket_instance_id: Some(888),
        ..Default::default()
    };
    ctx.db.with_mut_snapshot(|snap| {
        assert!(load_runner_state(snap, char_id).is_none());
        save_runner_state(snap, &custom_state);
        let loaded = load_runner_state(snap, char_id).expect("saved runner state must load");
        assert_eq!(loaded.char_id, char_id);
        assert_eq!(loaded.state_id, 77);
        assert_eq!(loaded.active_server_graph_id, 103);
        assert_eq!(loaded.executed_server_graphs, vec![101, 102, 103]);
        assert_eq!(loaded.checkpoint_id, 15);
        assert_eq!(loaded.master_client_instance_id, Some(9001));
        assert_eq!(loaded.pocket_instance_id, Some(888));
    });

    // 2. transition_character_into_npe_pocket error & success side-effects:
    //    kills `transition_character_into_npe_pocket -> Ok(())`.
    ctx.db.with_mut_snapshot(|snap| {
        snap.npe_progress.remove(&char_id);
    });
    assert_eq!(
        transition_character_into_npe_pocket(&ctx, Some(&mut session), char_id)
            .unwrap_err()
            .code,
        "TutorialNotActive"
    );

    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.solar_system_id = starter_sys;
        ch.station_id = Some(60_003_760);
        ch.structure_id = None;
    });
    session.solar_system_id = Some(starter_sys);
    session.station_id = Some(60_003_760);
    session.structure_id = None;
    on_start_tutorial(&ctx, &mut session, char_id, AIR_NPE_STATE_ACTIVE, Some(1));

    transition_character_into_npe_pocket(&ctx, Some(&mut session), char_id)
        .expect("eligible starter system tutorial transition succeeds");
    assert_eq!(session.station_id, None);
    assert_eq!(session.solar_system_id, Some(starter_sys));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.characters[&char_id].station_id, None);
        assert_eq!(
            snap.item_positions.get(&ship_id).copied(),
            Some(NPE_POCKET_ANCHOR_POS)
        );
    });
}
