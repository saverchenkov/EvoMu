use super::*;
use crate::services::air_npe_nodegraph_runner::on_start_tutorial;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::make_fighter_ctx_and_session;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_air_npe_pocket_spawn_move_ship_and_cleanup() {
    let (ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    session.station_id = Some(60_003_760);

    // 1. Spawn NPE pocket and verify idempotency on second call
    let (inst_id, obj_ids, npc_ids) =
        spawn_or_get_npe_pocket(&ctx, char_id, sys_id, NPE_DUNGEON_SCENE_1_ID);
    assert!(inst_id > 0);
    assert_eq!(obj_ids.len(), 2);
    assert_eq!(npc_ids.len(), 1);

    let (inst_id2, obj_ids2, npc_ids2) =
        spawn_or_get_npe_pocket(&ctx, char_id, sys_id, NPE_DUNGEON_SCENE_1_ID);
    assert_eq!(inst_id, inst_id2);
    assert!(obj_ids2.is_empty());
    assert!(npc_ids2.is_empty());

    // 2. Spawn or move starter ship in pocket
    spawn_or_move_ship_in_pocket(&ctx, char_id, sys_id, ship_id, 596);
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let has_ship_ball = sys_handle
        .ballpark
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .balls
        .contains_key(&ship_id);
    assert!(has_ship_ball);

    // 3. Start tutorial so runner state exists, mark moved_into_pocket, then cleanup and restore UI
    on_start_tutorial(&ctx, &mut session, char_id, 1, Some(1));
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(mut r) = load_runner_state(snap, char_id) {
            r.moved_into_pocket = true;
            r.origin_station_id = Some(60_003_760);
            save_runner_state(snap, &r);
        }
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.station_id = None;
        }
    });
    session.station_id = None;

    let (stopped, stop_master) =
        cleanup_npe_pocket_and_restore_ui(&ctx, Some(&mut session), char_id);
    assert!(!stopped.is_empty());
    assert!(stop_master.is_some());
    assert_eq!(session.station_id, Some(60_003_760));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_air_npe_pocket_security_combat_timers_structure_dock_and_errors() {
    let (ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();

    // 1. is_starter_or_school_solar_system: with SDE schools vs empty SDE schools fallback
    assert!(is_starter_or_school_solar_system(&ctx, 30_000_142));
    let mut empty_schools_sde = (*ctx.sde).clone();
    empty_schools_sde.schools.clear();
    let ctx_empty_schools = ServerContext::with_sde_and_repository(
        std::sync::Arc::new(empty_schools_sde),
        std::sync::Arc::clone(&ctx.db),
    );
    assert!(is_starter_or_school_solar_system(&ctx_empty_schools, 30_000_141));
    assert!(is_starter_or_school_solar_system(&ctx_empty_schools, 30_005_305));
    assert!(!is_starter_or_school_solar_system(&ctx_empty_schools, 30_000_001));

    // 2. check_npe_snapshot_combat_timers: pvp, npc, weapon, criminal timers & crimewatch_aggressions
    let now = ctx.now_unix();
    ctx.db.with_snapshot(|snap| {
        assert!(check_npe_snapshot_combat_timers(snap, char_id, now).is_ok());
    });
    for field in ["pvp", "npc", "weapon", "criminal", "aggression_attacker", "aggression_victim"] {
        ctx.db.with_mut_snapshot(|snap| {
            let mut sp6 = crate::sp6_lifecycle::load_sp6_state_from_snap(snap);
            sp6.pvp_timers_until.clear();
            sp6.npc_timers_until.clear();
            sp6.weapon_timers_until.clear();
            sp6.criminal_timers_until.clear();
            snap.crimewatch_aggressions.clear();
            match field {
                "pvp" => {
                    sp6.pvp_timers_until.insert(char_id, now + 60);
                }
                "npc" => {
                    sp6.npc_timers_until.insert(char_id, now + 60);
                }
                "weapon" => {
                    sp6.weapon_timers_until.insert(char_id, now + 60);
                }
                "criminal" => {
                    sp6.criminal_timers_until.insert(char_id, now + 60);
                }
                "aggression_attacker" => {
                    snap.crimewatch_aggressions.insert(
                        (char_id, 90_000_002),
                        eve_db::PersistedAggressionRecord {
                            aggressor_char_id: char_id,
                            victim_char_id: 90_000_002,
                            system_id: sys_id,
                            started_unix: now,
                            expires_unix: now + 60,
                            is_legal: false,
                            is_pod: false,
                        },
                    );
                }
                _ => {
                    snap.crimewatch_aggressions.insert(
                        (90_000_002, char_id),
                        eve_db::PersistedAggressionRecord {
                            aggressor_char_id: 90_000_002,
                            victim_char_id: char_id,
                            system_id: sys_id,
                            started_unix: now,
                            expires_unix: now + 60,
                            is_legal: false,
                            is_pod: false,
                        },
                    );
                }
            }
            crate::sp6_lifecycle::save_sp6_state_to_snap(snap, &sp6);
        });
        ctx.db.with_snapshot(|snap| {
            assert_eq!(
                check_npe_snapshot_combat_timers(snap, char_id, now).unwrap_err().code,
                "NpeBlockedInCombat"
            );
        });
    }
    // Clear snapshot timers
    ctx.db.with_mut_snapshot(|snap| {
        let mut sp6 = crate::sp6_lifecycle::load_sp6_state_from_snap(snap);
        sp6.pvp_timers_until.clear();
        sp6.npc_timers_until.clear();
        sp6.weapon_timers_until.clear();
        sp6.criminal_timers_until.clear();
        snap.crimewatch_aggressions.clear();
        crate::sp6_lifecycle::save_sp6_state_to_snap(snap, &sp6);
    });

    // 3. check_npe_live_combat_or_scramble: live crimewatch weapon timer + warp scramble when in_space
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        cw.trigger_weapon_timer(char_id, now);
    }
    assert_eq!(
        check_npe_live_combat_or_scramble(&ctx, char_id, sys_id, ship_id, true, now)
            .unwrap_err()
            .code,
        "NpeBlockedInCombat"
    );
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        cw.states.remove(&char_id);
    }

    spawn_or_move_ship_in_pocket(&ctx, char_id, sys_id, ship_id, 596);
    {
        let sys_h = ctx.systems.get_or_spawn(sys_id);
        let mut bp = sys_h.ballpark.write().unwrap();
        bp.set_warp_scramble_strength(ship_id, 2);
    }
    // Scrambled in space -> WarpScrambled; docked (in_space=false) -> Ok
    assert_eq!(
        check_npe_live_combat_or_scramble(&ctx, char_id, sys_id, ship_id, true, now)
            .unwrap_err()
            .code,
        "WarpScrambled"
    );
    assert!(check_npe_live_combat_or_scramble(&ctx, char_id, sys_id, ship_id, false, now).is_ok());
    {
        let sys_h = ctx.systems.get_or_spawn(sys_id);
        let mut bp = sys_h.ballpark.write().unwrap();
        bp.set_warp_scramble_strength(ship_id, 0);
    }

    // 4. validate_npe_start_or_entry error paths
    assert_eq!(
        validate_npe_start_or_entry(&ctx, 99_999_999).unwrap_err().code,
        "CharacterNotFound"
    );
    // Non-starter system
    let non_starter_sys = ctx
        .sde
        .solar_systems
        .keys()
        .copied()
        .find(|&sid| !is_starter_or_school_solar_system(&ctx, sid))
        .unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().solar_system_id = non_starter_sys;
    });
    assert_eq!(
        validate_npe_start_or_entry(&ctx, char_id).unwrap_err().code,
        "NpeStarterSystemRequired"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().solar_system_id = sys_id;
    });

    // 5. try_cleanup_npe_pocket_and_restore_ui with require_active_completion = true
    assert_eq!(
        try_cleanup_npe_pocket_and_restore_ui(&ctx, None, 99_999_999, true)
            .unwrap_err()
            .code,
        "CharacterNotFound"
    );
    // Before tutorial starts -> TutorialNotActive
    assert_eq!(
        try_cleanup_npe_pocket_and_restore_ui(&ctx, None, char_id, true)
            .unwrap_err()
            .code,
        "TutorialNotActive"
    );

    // 6. Dock restoration into an Upwell structure in snap.structures
    let struct_id = 1_040_000_000_555_i64;
    on_start_tutorial(&ctx, &mut session, char_id, AIR_NPE_STATE_ACTIVE, Some(1));
    spawn_or_get_npe_pocket(&ctx, char_id, sys_id, NPE_DUNGEON_SCENE_1_ID);
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            struct_id,
            eve_db::types::UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35_832,
                owner_corp_id: 98_000_001,
                solar_system_id: sys_id,
                name: "NPE Astrahus".into(),
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );
        if let Some(mut r) = load_runner_state(snap, char_id) {
            r.moved_into_pocket = true;
            r.origin_station_id = Some(struct_id);
            save_runner_state(snap, &r);
        }
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.station_id = None;
            c.structure_id = None;
        }
    });
    session.station_id = None;
    session.structure_id = None;

    let (stopped, _) = try_cleanup_npe_pocket_and_restore_ui(&ctx, Some(&mut session), char_id, true)
        .expect("active cleanup into Upwell structure");
    assert!(!stopped.is_empty());
    assert_eq!(session.structure_id, Some(struct_id));
    assert_eq!(session.station_id, None);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.characters[&char_id].structure_id, Some(struct_id));
    });
}
