#![forbid(unsafe_code)]

use super::*;
use crate::services::air_npe_nodegraph_runner::{
    load_runner_state, on_start_tutorial, save_runner_state, AIR_NPE_STATE_ACTIVE,
};
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::make_fighter_ctx_and_session;
use crate::sp6_lifecycle::{load_sp6_state_from_snap, save_sp6_state_to_snap};
use eve_db::clock::VirtualServerClock;
use eve_db::{PersistedAggressionRecord, ServerClock, UpwellStructureRecord, WorldRepository};
use eve_sde::SdeRegistry;
use std::sync::Arc;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn npe_pocket_starter_system_fallback_and_combat_timer_gates() {
    let (ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let clock = Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000));
    ctx.set_clock(clock.clone());
    let now = ctx.now_unix();

    // 1. is_starter_or_school_solar_system: canonical SDE vs empty SDE hardcoded fallback
    assert!(is_starter_or_school_solar_system(&ctx, sys_id));
    assert!(!is_starter_or_school_solar_system(&ctx, 30_000_208));

    let empty_ctx = ServerContext::with_sde_and_repository(
        Arc::new(SdeRegistry::default()),
        Arc::new(WorldRepository::new_empty()),
    );
    assert!(is_starter_or_school_solar_system(&empty_ctx, 30_000_141));
    assert!(is_starter_or_school_solar_system(&empty_ctx, 30_000_142));
    assert!(!is_starter_or_school_solar_system(&empty_ctx, 30_000_208));

    // 2. spawn_or_get_npe_pocket with unknown dungeon_id uses fallback name
    let (unk_inst_id, _, _) = spawn_or_get_npe_pocket(&ctx, char_id, sys_id, 999_999);
    let d_name = with_dungeon_runtime_store(&ctx, |s| s.instances[&unk_inst_id].dungeon_name.clone());
    assert_eq!(d_name, "AIR NPE Pocket 999999");

    // 3. spawn_or_move_ship_in_pocket with unknown type_id falls back cleanly
    spawn_or_move_ship_in_pocket(&ctx, char_id, sys_id, ship_id + 99, 999_999_999);

    // 4. check_npe_snapshot_combat_timers for pvp/npc/weapon/criminal/aggression
    ctx.db.with_mut_snapshot(|snap| {
        assert!(check_npe_snapshot_combat_timers(snap, char_id, now).is_ok());

        for field in ["pvp", "npc", "weapon", "criminal"] {
            let mut sp6 = load_sp6_state_from_snap(snap);
            match field {
                "pvp" => sp6.pvp_timers_until.insert(char_id, now + 60),
                "npc" => sp6.npc_timers_until.insert(char_id, now + 60),
                "weapon" => sp6.weapon_timers_until.insert(char_id, now + 60),
                _ => sp6.criminal_timers_until.insert(char_id, now + 60),
            };
            save_sp6_state_to_snap(snap, &sp6);
            assert_eq!(
                check_npe_snapshot_combat_timers(snap, char_id, now).unwrap_err().code,
                "NpeBlockedInCombat"
            );
            save_sp6_state_to_snap(snap, &Default::default());
        }

        snap.crimewatch_aggressions.insert(
            (char_id, 90_000_002),
            PersistedAggressionRecord {
                aggressor_char_id: char_id,
                victim_char_id: 90_000_002,
                system_id: sys_id,
                started_unix: now,
                expires_unix: now + 60,
                is_legal: true,
                is_pod: false,
            },
        );
        assert_eq!(
            check_npe_snapshot_combat_timers(snap, char_id, now).unwrap_err().code,
            "NpeBlockedInCombat"
        );
        snap.crimewatch_aggressions.clear();
    });

    // 5. validate_npe_start_or_entry error gates
    assert_eq!(
        validate_npe_start_or_entry(&ctx, 999_888_777).unwrap_err().code,
        "CharacterNotFound"
    );

    // Non-starter system -> NpeStarterSystemRequired
    ctx.db.with_mut_snapshot(|snap| {
        let mut store = load_store(snap);
        let rec = store.by_character.entry(char_id).or_default();
        rec.tutorial_finished = Some(false);
        rec.tutorial_skipped = false;
        save_store(snap, &store, now);
        snap.characters.get_mut(&char_id).unwrap().solar_system_id = 30_000_208;
    });
    assert_eq!(
        validate_npe_start_or_entry(&ctx, char_id).unwrap_err().code,
        "NpeStarterSystemRequired"
    );

    // Restore starter system and mark tutorial completed -> TutorialNotActive
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().solar_system_id = sys_id;
        let mut store = load_store(snap);
        store.by_character.entry(char_id).or_default().tutorial_skipped = true;
        save_store(snap, &store, now);
    });
    assert_eq!(
        validate_npe_start_or_entry(&ctx, char_id).unwrap_err().code,
        "TutorialNotActive"
    );

    // Re-enable tutorial and check live crimewatch weapon timer + ballpark scramble
    ctx.db.with_mut_snapshot(|snap| {
        let mut store = load_store(snap);
        let rec = store.by_character.entry(char_id).or_default();
        rec.tutorial_finished = Some(false);
        rec.tutorial_skipped = false;
        save_store(snap, &store, now);
    });
    assert!(validate_npe_start_or_entry(&ctx, char_id).is_ok());

    ctx.crimewatch.write().unwrap().trigger_weapon_timer(char_id, now);
    assert_eq!(
        validate_npe_start_or_entry(&ctx, char_id).unwrap_err().code,
        "NpeBlockedInCombat"
    );
    clock.advance_secs(120); // expire weapon timer

    // Warp scramble in ballpark while in space -> WarpScrambled
    spawn_or_move_ship_in_pocket(&ctx, char_id, sys_id, ship_id, 596);
    {
        let sys_h = ctx.systems.get_or_spawn(sys_id);
        let mut bp = sys_h.ballpark.write().unwrap();
        if let Some(cs) = bp.combat_states.get_mut(&ship_id) {
            cs.warp_scramble_strength = 5;
            cs.warp_core_strength = 1;
        }
    }
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.station_id = None;
        ch.structure_id = None;
    });
    assert_eq!(
        validate_npe_start_or_entry(&ctx, char_id).unwrap_err().code,
        "WarpScrambled"
    );

    // Clear scramble and test require_active_completion cleanup gates
    {
        let sys_h = ctx.systems.get_or_spawn(sys_id);
        let mut bp = sys_h.ballpark.write().unwrap();
        if let Some(cs) = bp.combat_states.get_mut(&ship_id) {
            cs.warp_scramble_strength = 0;
        }
    }
    assert_eq!(
        try_cleanup_npe_pocket_and_restore_ui(&ctx, Some(&mut session), 999_888_777, true)
            .unwrap_err()
            .code,
        "CharacterNotFound"
    );
    // No active runner yet -> TutorialNotActive
    assert_eq!(
        try_cleanup_npe_pocket_and_restore_ui(&ctx, Some(&mut session), char_id, true)
            .unwrap_err()
            .code,
        "TutorialNotActive"
    );
    // Non-starter system -> NpeStarterSystemRequired
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().solar_system_id = 30_000_208;
    });
    assert_eq!(
        try_cleanup_npe_pocket_and_restore_ui(&ctx, Some(&mut session), char_id, false)
            .unwrap_err()
            .code,
        "NpeStarterSystemRequired"
    );
    // Cleanup succeeds when starter system restored and runner active
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().solar_system_id = sys_id;
    });
    on_start_tutorial(&ctx, &mut session, char_id, 1, Some(1));
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(mut r) = load_runner_state(snap, char_id) {
            r.state_id = AIR_NPE_STATE_ACTIVE;
            r.completed = false;
            r.skipped = false;
            r.moved_into_pocket = true;
            save_runner_state(snap, &r);
        }
    });
    let (_stopped, _) =
        try_cleanup_npe_pocket_and_restore_ui(&ctx, Some(&mut session), char_id, true).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn npe_dock_target_structure_home_and_cross_system_session_restore() {
    let (ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let other_starter_sys = ctx
        .sde
        .schools
        .values()
        .find(|s| s.solar_system_id != sys_id && s.solar_system_id > 0)
        .map(|s| s.solar_system_id)
        .unwrap_or(sys_id);
    let struct_id = 1_880_050_001_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                solar_system_id: other_starter_sys,
                owner_corp_id: 1_000_009,
                ..Default::default()
            },
        );
        // Unknown station returns None
        assert!(station_system_info(&ctx, snap, 9_999_999_999, 1, 1).is_none());
        // Structure returns is_struct = true
        let info = station_system_info(&ctx, snap, struct_id, 1, 1).unwrap();
        assert_eq!(info.0, other_starter_sys);
        assert!(info.3);

        // resolve_npe_dock_target fallback to home_station when origin invalid
        let home = snap.characters[&char_id].home_station_id;
        let res_home = resolve_npe_dock_target(&ctx, snap, Some(9_999_999_999), home, sys_id, 1, 1);
        assert!(res_home.is_some());

        // Fallback to first SDE station in expected_sys when both origin and home invalid
        let res_sys = resolve_npe_dock_target(&ctx, snap, Some(0), 0, sys_id, 1, 1);
        assert!(res_sys.is_some());

        // Returns None when expected_sys has no stations
        assert!(resolve_npe_dock_target(&ctx, snap, Some(0), 0, 99_999_999, 1, 1).is_none());
    });

    // Test cleanup restoring into Upwell structure in another starter system (tests structureid + solarsystemid2 change)
    on_start_tutorial(&ctx, &mut session, char_id, 1, Some(1));
    ctx.db.with_mut_snapshot(|snap| {
        let mut r = load_runner_state(snap, char_id).unwrap();
        r.moved_into_pocket = true;
        r.origin_station_id = Some(struct_id);
        r.origin_solar_system_id = Some(other_starter_sys);
        save_runner_state(snap, &r);
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.station_id = None;
        ch.structure_id = None;
        ch.solar_system_id = sys_id;
    });
    spawn_or_move_ship_in_pocket(&ctx, char_id, sys_id, ship_id, 596);
    session.station_id = None;
    session.structure_id = None;
    session.solar_system_id = Some(sys_id);

    let uid = ctx.db.with_snapshot(|snap| snap.characters[&char_id].user_id);
    let _ = ctx.db.acquire_online_lock(uid, 99, Some(char_id), true, ctx.now_unix());

    let _ = try_cleanup_npe_pocket_and_restore_ui(&ctx, Some(&mut session), char_id, false).unwrap();
    assert_eq!(session.structure_id, Some(struct_id));
    assert_eq!(session.station_id, None);
    assert_eq!(session.solar_system_id, Some(other_starter_sys));

    let pending = ctx.pending_notifications.read().unwrap();
    assert!(pending
        .get(&char_id)
        .is_some_and(|v| v.iter().any(|n| n.event == "OnSessionChanged")));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn npe_cleanup_exact_stopped_instances_master_graph_and_dungeon_teardown() {
    let (ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let orig_station = ctx
        .sde
        .stations
        .values()
        .find(|s| s.solar_system_id == sys_id)
        .map(|s| s.station_id)
        .unwrap_or(60_003_760);

    // Unknown character rejected when require_active_completion == true
    assert_eq!(
        try_cleanup_npe_pocket_and_restore_ui(&ctx, None, 9_999_888_777, true)
            .unwrap_err()
            .code,
        "CharacterNotFound"
    );

    // Spawn real pocket + balls + set distinct non-zero/non-one/non-neg1 client instance & master graph IDs
    let (inst_id, obj_balls, npc_balls) = spawn_or_get_npe_pocket(&ctx, char_id, sys_id, 55_001);
    assert!(!obj_balls.is_empty());
    assert!(!npc_balls.is_empty());
    on_start_tutorial(&ctx, &mut session, char_id, 1, Some(1));
    ctx.db.with_mut_snapshot(|snap| {
        let mut r = load_runner_state(snap, char_id).expect("runner");
        r.state_id = AIR_NPE_STATE_ACTIVE;
        r.completed = false;
        r.skipped = false;
        r.moved_into_pocket = true;
        r.origin_station_id = Some(orig_station);
        r.origin_solar_system_id = Some(sys_id);
        r.pocket_instance_id = Some(inst_id);
        r.pocket_solar_system_id = Some(sys_id);
        r.pocket_dungeon_id = Some(55_001);
        r.scene_client_instance_id = Some(88_101);
        r.act_client_instance_id = Some(88_102);
        r.master_client_instance_id = Some(88_103);
        r.master_client_graph_id = 77_001;
        save_runner_state(snap, &r);

        let ch = snap.characters.get_mut(&char_id).expect("char");
        ch.station_id = None;
        ch.structure_id = None;
        ch.solar_system_id = sys_id;
    });
    spawn_or_move_ship_in_pocket(&ctx, char_id, sys_id, ship_id, 596);
    session.station_id = None;
    session.structure_id = None;
    session.solar_system_id = Some(sys_id);

    let uid = ctx.db.with_snapshot(|snap| snap.characters[&char_id].user_id);
    let _ = ctx
        .db
        .acquire_online_lock(uid, 101, Some(char_id), true, ctx.now_unix());

    let (stopped, master_graph) =
        try_cleanup_npe_pocket_and_restore_ui(&ctx, Some(&mut session), char_id, true)
            .expect("active NPE cleanup");
    assert_eq!(stopped, vec![88_101, 88_102, 88_103]);
    assert_eq!(master_graph, Some(77_001));

    // Verify runner client IDs were cleared and dungeon instance completed/despawned
    ctx.db.with_snapshot(|snap| {
        let r = load_runner_state(snap, char_id).expect("runner");
        assert_eq!(r.scene_client_instance_id, None);
        assert_eq!(r.act_client_instance_id, None);
        assert_eq!(r.master_client_instance_id, None);
        assert_eq!(snap.characters[&char_id].station_id, Some(orig_station));
    });
    let (completed, despawned) = with_dungeon_runtime_store(&ctx, |store| {
        let inst = &store.instances[&inst_id];
        (inst.completed, inst.despawned)
    });
    assert!(completed && despawned);

    // Calling again while docked with require_active_completion == true is idempotent;
    // once marked completed it fails TutorialNotActive.
    let (stopped2, master2) =
        try_cleanup_npe_pocket_and_restore_ui(&ctx, Some(&mut session), char_id, true)
            .expect("repeat docked cleanup remains idempotent before completion");
    assert!(stopped2.is_empty());
    assert_eq!(master2, None);

    ctx.db.with_mut_snapshot(|snap| {
        let mut r = load_runner_state(snap, char_id).expect("runner");
        r.completed = true;
        save_runner_state(snap, &r);
    });
    assert_eq!(
        try_cleanup_npe_pocket_and_restore_ui(&ctx, Some(&mut session), char_id, true)
            .unwrap_err()
            .code,
        "TutorialNotActive"
    );

    let notices = ctx.pending_notifications.read().unwrap();
    let char_notices = notices.get(&char_id).expect("char notices");
    assert!(char_notices.iter().any(|n| {
        n.event == "OnDungeonExited"
            && n.args == vec![PyRep::Integer(55_001), PyRep::Integer(inst_id)]
    }));
}
