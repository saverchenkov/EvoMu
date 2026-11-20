//! Unit and integration tests for `pve_hacking_sites`.

use super::*;
use crate::router::ServerContext;
use eve_db::clock::VirtualServerClock;
use eve_db::WorldRepository;
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_sde::{SdeRegistry, SolarSystemId};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

#[test]
fn hacking_grid_geometry_and_blocking_tiles() {
    let grid = build_hacking_grid(99_001, true);
    assert_eq!(grid.len(), 24);
    assert!(grid.iter().any(|t| t.kind == HackingNodeKind::SystemCore));

    let (q0, r0) = hacking_tile_axial_coords(0);
    let (q1, r1) = hacking_tile_axial_coords(1);
    assert_eq!(q0, 0);
    assert_eq!(r0, 0);
    assert_eq!(q1, 1);
    assert_eq!(r1, 0);
    assert_eq!(hacking_hex_distance(0, 1), 1);
    assert!(are_hacking_tiles_adjacent(0, 1));
    assert!(!are_hacking_tiles_adjacent(0, 8));

    let mut fw = grid[1].clone();
    fw.kind = HackingNodeKind::Firewall;
    fw.coherence = 40;
    fw.strength = 10;
    assert!(is_blocking_defensive_tile(&fw));
    let mut breached = fw;
    breached.coherence = 0;
    assert!(!is_blocking_defensive_tile(&breached));

    let core_idx = grid
        .iter()
        .find(|t| t.kind == HackingNodeKind::SystemCore)
        .unwrap()
        .tile_index;
    assert_eq!(hacking_tile_distance_to_core(core_idx, &grid), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hacking_minigame_start_click_utility_progress_and_quit_rpc() {
    let sys_id: SolarSystemId = 30_000_142;
    let char_id: i64 = 90_040_201;
    let ship_id: i64 = 1_040_000_201;
    let can_id: i64 = 1_040_900_201;

    let mut sde = (*shared_sde()).clone();
    if !sde.types.contains_key(&22_588) {
        if let Some(mut relic_t) = sde.types.values().next().cloned() {
            relic_t.type_id = 22_588;
            relic_t.group_id = 538;
            relic_t.name = "Relic Analyzer I".into();
            relic_t.published = true;
            relic_t.default_attributes.insert(1909, 40.0);
            relic_t.default_attributes.insert(1910, 20.0);
            relic_t.default_attributes.insert(1911, 2.0);
            relic_t.default_attributes.insert(54, 5_000.0);
            sde.types.insert(22_588, relic_t);
        }
    }
    let sde = Arc::new(sde);
    let db = Arc::new(WorldRepository::new_empty());
    db.provision_gm_account("hacker", "pw", char_id, "Hacker");
    db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(
            ship_id,
            eve_db::InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: eve_db::InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            ship_id + 1,
            eve_db::InventoryItemRecord {
                item_id: ship_id + 1,
                type_id: 22_588,
                owner_id: char_id,
                location_id: ship_id,
                flag: eve_db::InventoryFlag::MedSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_450_000)));
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 587,
            position: DVec3::new(100_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 200.0,
            inertia: 1.0,
            mass: 1_100_000.0,
            radius: 35.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    let _ = sys_handle
        .spawn_ball(Ball {
            id: can_id,
            type_id: 22_270,
            position: DVec3::new(101_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1.0,
            radius: 200.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    ensure_system_pve_sites_seeded(&ctx, sys_id).await;
    crate::services::pve_exploration::with_pve_exploration_reg(&ctx, |reg| {
        reg.sites_by_system.entry(sys_id).or_default().push(
            crate::services::pve_exploration::PveSiteInstance {
                site_id: format!("RELIC_{sys_id}_1"),
                dungeon_id: 2010,
                dungeon_name: "Ruined Guristas Temple".into(),
                solar_system_id: sys_id,
                site_category: "Relic".into(),
                position: [100_000.0, 0.0, 0.0],
                probe_required: true,
                signal_strength: 1.0,
                current_room: 1,
                total_rooms: 1,
                current_wave: 1,
                total_waves: 1,
                hackable_can_ids: vec![can_id],
                ghost_detonate_after_sec: 120,
                created_unix: 1_775_450_000,
                lifetime_seconds: 86_400,
                ..Default::default()
            },
        );
        let spew = spawn_loot_spew_for_container(
            &ctx,
            reg,
            sys_id,
            can_id,
            true,
            [101_000.0, 0.0, 0.0],
        );
        assert!(!spew.is_empty());
    });

    let mut session = SessionState::new(403);
    session.user_id = Some(1);
    session.character_id = Some(char_id);
    session.solar_system_id = Some(sys_id);
    session.ship_id = Some(ship_id);
    let empty_kw = HashMap::new();

    // Unknown service/method returns None
    assert!(handle_pve_hacking(&ctx, &mut session, "", "", &[], &empty_kw)
        .await
        .is_none());

    // StartNewGameInstance
    let start_res = handle_pve_hacking(
        &ctx,
        &mut session,
        "hackingMgr",
        "StartNewGameInstance",
        &[PyRep::Integer(can_id)],
        &empty_kw,
    )
    .await
    .expect("handled")
    .expect("StartNewGameInstance succeeds");
    assert!(!matches!(start_res, PyRep::None));

    // ClickedOnTile (tile 1)
    let click_res = handle_pve_hacking(
        &ctx,
        &mut session,
        "hackingMgr",
        "ClickedOnTile",
        &[PyRep::Integer(1)],
        &empty_kw,
    )
    .await
    .expect("handled")
    .expect("ClickedOnTile succeeds");
    assert!(!matches!(click_res, PyRep::None));

    // values & GetMyHackProgress
    let vals = handle_pve_hacking(
        &ctx,
        &mut session,
        "hackingMgr",
        "values",
        &[PyRep::Integer(can_id)],
        &empty_kw,
    )
    .await
    .expect("handled")
    .expect("values succeeds");
    assert!(!matches!(vals, PyRep::None));

    let progress = handle_pve_hacking(
        &ctx,
        &mut session,
        "hackingMgr",
        "GetMyHackProgress",
        &[],
        &empty_kw,
    )
    .await
    .expect("handled")
    .expect("GetMyHackProgress succeeds");
    assert_eq!(progress.dict_get("containerID").and_then(|v| v.as_i64()), Some(can_id));
    assert_eq!(progress.dict_get("status").and_then(|v| v.as_str()), Some("InProgress"));

    // UsedUtilityElementOnVirus & QuitHackingAttempt
    with_pve_exploration_reg(&ctx, |reg| {
        if let Some(game) = reg.hacking_games.get_mut(&(char_id, can_id)) {
            game.utilities[0] =
                Some(crate::services::pve_exploration::HackingUtilityKind::SelfRepair);
        }
    });
    let util_res = handle_pve_hacking(
        &ctx,
        &mut session,
        "hackingMgr",
        "UsedUtilityElementOnVirus",
        &[PyRep::Integer(0)],
        &empty_kw,
    )
    .await
    .expect("handled")
    .expect("UsedUtilityElementOnVirus succeeds");
    assert!(!matches!(util_res, PyRep::None));

    let quit_res = handle_pve_hacking(
        &ctx,
        &mut session,
        "hackingMgr",
        "QuitHackingAttempt",
        &[],
        &empty_kw,
    )
    .await
    .expect("handled")
    .expect("QuitHackingAttempt succeeds");
    assert!(!matches!(quit_res, PyRep::None));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wave4_hacking_wins_not_double_counted_when_already_hacked() {
    let sys_id: SolarSystemId = 30_000_142;
    let (char_id, ship_id, can_id) = (90_040_999, 1_040_000_999, 1_040_900_999);
    let mut sde = (*shared_sde()).clone();
    if !sde.types.contains_key(&22_586) {
        if let Some(mut t) = sde.types.values().next().cloned() {
            t.type_id = 22_586;
            t.group_id = 538;
            t.name = "Data Analyzer I".into();
            t.published = true;
            t.default_attributes.insert(1909, 60.0);
            t.default_attributes.insert(1910, 30.0);
            t.default_attributes.insert(1911, 2.0);
            t.default_attributes.insert(54, 5_000.0);
            sde.types.insert(22_586, t);
        }
    }
    let db = Arc::new(WorldRepository::new_empty());
    db.provision_gm_account("hk_w4", "pw", char_id, "HackerW4");
    db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
        }
        snap.item_positions.insert(can_id, [10_200.0, 0.0, 0.0]);
        for (id, tid, loc, flag) in [
            (ship_id, 587_u32, sys_id as i64, eve_db::InventoryFlag::None),
            (ship_id + 1, 22_586, ship_id, eve_db::InventoryFlag::MedSlot0),
        ] {
            snap.items.insert(
                id,
                eve_db::InventoryItemRecord {
                    item_id: id,
                    type_id: tid,
                    owner_id: char_id,
                    location_id: loc,
                    flag,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
    });
    let ctx = ServerContext::with_sde_and_repository(Arc::new(sde), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_776_600_000)));
    let _ = ctx
        .systems
        .get_or_spawn(sys_id)
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 587,
            position: DVec3::new(10_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 200.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 30.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    with_pve_exploration_reg(&ctx, |reg| {
        reg.seeded_systems.insert(sys_id, true);
        reg.hacked_containers.insert((sys_id, can_id));
        reg.hacking_games.insert(
            (char_id, can_id),
            HackingGameSession {
                game_id: 99_901,
                character_id: char_id,
                solar_system_id: sys_id,
                container_id: can_id,
                site_id: format!("DATA_{sys_id}_W4"),
                is_relic: false,
                is_ghost_site: false,
                is_sleeper_cache_puzzle: false,
                virus_coherence: 60,
                max_virus_coherence: 60,
                base_virus_strength: 30,
                polymorphic_shield_charges: 0,
                secondary_vector_target: None,
                utilities: vec![None; 2],
                tiles: {
                    let mut grid = build_hacking_grid(can_id, false);
                    grid[1].revealed = true;
                    grid[1].kind = HackingNodeKind::SystemCore;
                    grid[1].coherence = 1;
                    grid[1].strength = 1;
                    grid
                },
                status: "InProgress".into(),
            },
        );
    });

    let mut session = SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        solar_system_id: Some(sys_id),
        ship_id: Some(ship_id),
        ..Default::default()
    };
    let kw = HashMap::new();
    let res = handle_pve_hacking(
        &ctx,
        &mut session,
        "hackingMgr",
        "ClickedOnTile",
        &[PyRep::Integer(can_id), PyRep::Integer(1)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(res.dict_get("status"), Some(&PyRep::String("Won".into())));
    assert_eq!(
        ctx.db.with_snapshot(|s| s.seasons_and_campaigns.hacking_wins.get(&char_id).copied()),
        None,
        "re-winning an already_hacked container must not increment hacking_wins or spawn duplicate spew"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn homefront_hacking_arrays_and_werpost_untether_and_retry() {
    use crate::services::pve_homefront_sites::{
        activate_homefront_acceleration_gate, get_homefront_runtime_state,
        is_homefront_hackable_structure, spawn_homefront_site_for_dungeon,
        HomefrontArchetypeProgress,
    };

    let sys_id: SolarSystemId = 30_000_142;
    let (char_id, ship_id) = (90_041_111, 1_040_001_111);
    let mut sde = (*shared_sde()).clone();
    if !sde.types.contains_key(&22_586) {
        if let Some(mut t) = sde.types.values().next().cloned() {
            t.type_id = 22_586;
            t.group_id = 538;
            t.name = "Data Analyzer I".into();
            t.published = true;
            t.default_attributes.insert(1909, 60.0);
            t.default_attributes.insert(1910, 30.0);
            t.default_attributes.insert(1911, 2.0);
            t.default_attributes.insert(54, 5_000.0);
            sde.types.insert(22_586, t);
        }
    }
    let db = Arc::new(WorldRepository::new_empty());
    db.provision_gm_account("hf_hacker", "pw", char_id, "HfHacker");
    db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
        }
        for (id, tid, loc, flag) in [
            (ship_id, 587_u32, sys_id as i64, eve_db::InventoryFlag::None),
            (ship_id + 1, 22_586, ship_id, eve_db::InventoryFlag::MedSlot0),
        ] {
            snap.items.insert(
                id,
                eve_db::InventoryItemRecord {
                    item_id: id,
                    type_id: tid,
                    owner_id: char_id,
                    location_id: loc,
                    flag,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
    });

    let ctx = ServerContext::with_sde_and_repository(Arc::new(sde), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_776_700_000)));
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let mut session = SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        solar_system_id: Some(sys_id),
        ship_id: Some(ship_id),
        ..Default::default()
    };
    let kw = HashMap::new();

    // 1. Salvage Research Werpost hacking via hackingMgr -> werpost_activated = true
    let sr = spawn_homefront_site_for_dungeon(&ctx, sys_id, 10_713).expect("spawn Salvage Research");
    let _ = sys_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 587,
            position: sr.site_position + DVec3::new(400.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    activate_homefront_acceleration_gate(&ctx, &session, sr.instance_id)
        .await
        .expect("enter Salvage Research gate");
    let werpost_id = ctx.db.with_snapshot(|snap| {
        match snap.dungeon_runtime.homefront_runtimes[&sr.instance_id].progress {
            eve_db::service_domains::pve_sites::HomefrontProgressRecord::SalvageResearch {
                werpost_ball_id,
                ..
            } => werpost_ball_id,
            _ => panic!("expected SalvageResearch"),
        }
    });
    let gate_err = is_homefront_hackable_structure(&ctx, 9_999_999, sys_id, werpost_id)
        .expect_err("unentered pilot must receive GateNotEntered");
    assert_eq!(gate_err.code, "GateNotEntered");
    assert!(matches!(
        is_homefront_hackable_structure(&ctx, char_id, sys_id, werpost_id),
        Ok(Some((false, _)))
    ));
    let (_, werpost_ball) = sys_handle
        .get_ball_snapshot(werpost_id)
        .await
        .unwrap()
        .expect("werpost ball spawned");
    let _ = sys_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 587,
            position: werpost_ball.position + DVec3::new(werpost_ball.radius + 300.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 200.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 30.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    let mut unentered_session = session.clone();
    unentered_session.character_id = Some(char_id + 900);
    unentered_session.ship_id = Some(ship_id + 900);
    let gate_rpc_err = handle_pve_hacking(
        &ctx,
        &mut unentered_session,
        "hackingMgr",
        "StartNewGameInstance",
        &[PyRep::Integer(werpost_id), PyRep::Integer(0), PyRep::Integer(1), PyRep::Integer(1)],
        &kw,
    )
    .await
    .expect("Homefront gate error returns RPC Some(Err)")
    .expect_err("unentered pilot must fail StartNewGameInstance with GateNotEntered");
    assert_eq!(gate_rpc_err.code, "GateNotEntered");
    assert!(
        with_pve_exploration_reg(&ctx, |reg| !reg
            .hacking_games
            .contains_key(&(char_id + 900, werpost_id))),
        "rejected GateNotEntered call must NOT insert hacking game state"
    );

    let start_wp = handle_pve_hacking(
        &ctx,
        &mut session,
        "hackingMgr",
        "StartNewGameInstance",
        &[PyRep::Integer(werpost_id), PyRep::Integer(0), PyRep::Integer(1), PyRep::Integer(1)],
        &kw,
    )
    .await
    .unwrap()
    .expect("start Werpost hack");
    assert_eq!(start_wp.dict_get("status"), Some(&PyRep::String("InProgress".into())));
    with_pve_exploration_reg(&ctx, |reg| {
        let game = reg.hacking_games.get_mut(&(char_id, werpost_id)).unwrap();
        assert_eq!(game.site_id, format!("HOMEFRONT_{werpost_id}"));
        game.tiles[1].revealed = true;
        game.tiles[1].kind = HackingNodeKind::SystemCore;
        game.tiles[1].coherence = 1;
        game.tiles[1].strength = 1;
    });
    let won_wp = handle_pve_hacking(
        &ctx,
        &mut session,
        "hackingMgr",
        "ClickedOnTile",
        &[PyRep::Integer(werpost_id), PyRep::Integer(1)],
        &kw,
    )
    .await
    .unwrap()
    .expect("win Werpost hack");
    assert_eq!(won_wp.dict_get("status"), Some(&PyRep::String("Won".into())));
    assert!(matches!(
        get_homefront_runtime_state(&ctx, sr.instance_id).unwrap().progress,
        HomefrontArchetypeProgress::SalvageResearch {
            werpost_activated: true,
            ..
        }
    ));
    assert!(
        is_homefront_hackable_structure(&ctx, char_id, sys_id, werpost_id)
            .expect("entered pilot check")
            .is_none(),
        "already-activated Werpost must no longer report as hackable"
    );

    // 2. Suspicious Signal: quitting twice must NOT permanently lock out array;
    //    hacking all 3 arrays clears relay is_tethered in Ballpark.
    let ss = spawn_homefront_site_for_dungeon(&ctx, sys_id, 10_367).expect("spawn Suspicious Signal");
    let _ = sys_handle
        .set_ball_position(ship_id, ss.site_position + DVec3::new(400.0, 0.0, 0.0))
        .await;
    activate_homefront_acceleration_gate(&ctx, &session, ss.instance_id)
        .await
        .expect("enter Suspicious Signal gate");
    let (relay_id, array_ids) = match get_homefront_runtime_state(&ctx, ss.instance_id).unwrap().progress {
        HomefrontArchetypeProgress::SuspiciousSignal {
            relay_ball_id,
            array_ball_ids,
            ..
        } => (relay_ball_id, array_ball_ids),
        other => panic!("expected SuspiciousSignal, got {other:?}"),
    };
    assert!(
        sys_handle
            .get_combat_state(relay_id)
            .await
            .unwrap()
            .unwrap()
            .is_tethered,
        "relay starts tethered before all 3 arrays are hacked"
    );

    let (_, arr0_ball) = sys_handle
        .get_ball_snapshot(array_ids[0])
        .await
        .unwrap()
        .expect("array 0 ball");
    let _ = sys_handle
        .set_ball_position(ship_id, arr0_ball.position + DVec3::new(arr0_ball.radius + 300.0, 0.0, 0.0))
        .await;
    for _ in 0..2 {
        handle_pve_hacking(
            &ctx,
            &mut session,
            "hackingMgr",
            "StartNewGameInstance",
            &[PyRep::Integer(array_ids[0]), PyRep::Integer(0), PyRep::Integer(1), PyRep::Integer(1)],
            &kw,
        )
        .await
        .unwrap()
        .expect("retryable Homefront array hack starts");
        handle_pve_hacking(&ctx, &mut session, "hackingMgr", "QuitHackingAttempt", &[], &kw)
            .await
            .unwrap()
            .expect("quit attempt");
    }

    for &arr_id in &array_ids {
        let (_, ab) = sys_handle
            .get_ball_snapshot(arr_id)
            .await
            .unwrap()
            .expect("array ball");
        let _ = sys_handle
            .set_ball_position(ship_id, ab.position + DVec3::new(ab.radius + 300.0, 0.0, 0.0))
            .await;
        handle_pve_hacking(
            &ctx,
            &mut session,
            "hackingMgr",
            "StartNewGameInstance",
            &[PyRep::Integer(arr_id), PyRep::Integer(0), PyRep::Integer(1), PyRep::Integer(1)],
            &kw,
        )
        .await
        .unwrap()
        .expect("start array hack");
        with_pve_exploration_reg(&ctx, |reg| {
            let game = reg.hacking_games.get_mut(&(char_id, arr_id)).unwrap();
            game.tiles[1].revealed = true;
            game.tiles[1].kind = HackingNodeKind::SystemCore;
            game.tiles[1].coherence = 1;
            game.tiles[1].strength = 1;
        });
        let won_arr = handle_pve_hacking(
            &ctx,
            &mut session,
            "hackingMgr",
            "ClickedOnTile",
            &[PyRep::Integer(arr_id), PyRep::Integer(1)],
            &kw,
        )
        .await
        .unwrap()
        .expect("win array hack");
        assert_eq!(won_arr.dict_get("status"), Some(&PyRep::String("Won".into())));
    }

    assert!(
        !sys_handle
            .get_combat_state(relay_id)
            .await
            .unwrap()
            .unwrap()
            .is_tethered,
        "all 3 arrays hacked via hackingMgr must clear relay is_tethered"
    );
    match get_homefront_runtime_state(&ctx, ss.instance_id).unwrap().progress {
        HomefrontArchetypeProgress::SuspiciousSignal {
            arrays_hacked,
            vulnerability_window_secs,
            ..
        } => {
            assert_eq!(arrays_hacked, [true; 3]);
            assert!(vulnerability_window_secs > 0);
        }
        other => panic!("expected SuspiciousSignal, got {other:?}"),
    }
    assert!(
        is_homefront_hackable_structure(&ctx, char_id, sys_id, array_ids[0])
            .expect("entered pilot check")
            .is_none(),
        "hacked array must not be re-hackable during open vulnerability window"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hacking_mgr_guards_reject_unauth_docked_invalid_target_and_no_game_without_mutating_db() {
    let sys_id: SolarSystemId = 30_000_142;
    let char_id: i64 = 90_042_301;
    let ship_id: i64 = 1_042_000_301;
    let can_id: i64 = 1_042_900_301;

    let mut sde = (*shared_sde()).clone();
    if !sde.types.contains_key(&22_586) {
        if let Some(mut t) = sde.types.values().next().cloned() {
            t.type_id = 22_586;
            t.group_id = 538;
            t.name = "Data Analyzer I".into();
            t.published = true;
            t.default_attributes.insert(1909, 60.0);
            t.default_attributes.insert(1910, 30.0);
            t.default_attributes.insert(1911, 2.0);
            t.default_attributes.insert(54, 5_000.0);
            sde.types.insert(22_586, t);
        }
    }
    let db = Arc::new(WorldRepository::new_empty());
    db.provision_gm_account("hk_guard", "pw", char_id, "HackerGuard");
    db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
        }
        for (id, tid, loc, flag) in [
            (ship_id, 587_u32, sys_id as i64, eve_db::InventoryFlag::None),
            (ship_id + 1, 22_586, ship_id, eve_db::InventoryFlag::MedSlot0),
        ] {
            snap.items.insert(
                id,
                eve_db::InventoryItemRecord {
                    item_id: id,
                    type_id: tid,
                    owner_id: char_id,
                    location_id: loc,
                    flag,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
    });
    let ctx = ServerContext::with_sde_and_repository(Arc::new(sde), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_776_800_000)));
    let kw = HashMap::new();

    let all_methods: &[(&str, Vec<PyRep>)] = &[
        ("StartNewGameInstance", vec![PyRep::Integer(60_003_760)]),
        ("ClickedOnTile", vec![PyRep::Integer(60_003_760), PyRep::Integer(1)]),
        ("QuitHackingAttempt", vec![PyRep::Integer(60_003_760)]),
        ("UsedUtilityElement", vec![PyRep::Integer(60_003_760), PyRep::Integer(0), PyRep::Integer(0)]),
        ("UsedUtilityElementOnVirus", vec![PyRep::Integer(60_003_760), PyRep::Integer(0)]),
        ("GetMyHackProgress", vec![PyRep::Integer(60_003_760)]),
        ("values", vec![PyRep::Integer(60_003_760)]),
    ];

    let epoch0 = ctx.db.state_epoch();
    let commits0 = ctx.db.tx_commit_count();

    // 1. Unauthenticated caller: all 7 methods reject with zero DB mutations.
    let mut unauth = SessionState::new(701);
    for (meth, args) in all_methods {
        let res = handle_pve_hacking(&ctx, &mut unauth, "hackingMgr", meth, args, &kw)
            .await
            .expect("handled");
        assert!(res.is_err(), "unauthenticated hackingMgr.{meth} must reject");
    }
    assert_eq!(ctx.db.state_epoch(), epoch0);
    assert_eq!(ctx.db.tx_commit_count(), commits0);

    // 2. Docked in NPC station: all 7 methods reject with ShipDocked and zero DB mutations.
    const DOCKED_CODE: &str = "ShipDocked";
    let mut docked = SessionState::new(702);
    docked.user_id = Some(1);
    docked.character_id = Some(char_id);
    docked.solar_system_id = Some(sys_id);
    docked.station_id = Some(60_003_760);
    docked.ship_id = Some(ship_id);
    for (meth, args) in all_methods {
        let err = handle_pve_hacking(&ctx, &mut docked, "hackingMgr", meth, args, &kw)
            .await
            .expect("handled")
            .expect_err(&format!("docked hackingMgr.{meth} must reject"));
        assert_eq!(err.code, DOCKED_CODE, "docked hackingMgr.{meth} must return {DOCKED_CODE}");
    }
    assert_eq!(ctx.db.state_epoch(), epoch0);
    assert_eq!(ctx.db.tx_commit_count(), commits0);

    // 3. Docked in Upwell structure: all 7 methods reject with ShipDocked and zero DB mutations.
    docked.station_id = None;
    docked.structure_id = Some(1_099_001_000_001);
    for (meth, args) in all_methods {
        let err = handle_pve_hacking(&ctx, &mut docked, "hackingMgr", meth, args, &kw)
            .await
            .expect("handled")
            .expect_err(&format!("structure-docked hackingMgr.{meth} must reject"));
        assert_eq!(err.code, DOCKED_CODE);
    }
    assert_eq!(ctx.db.state_epoch(), epoch0);
    assert_eq!(ctx.db.tx_commit_count(), commits0);

    // 4. Undocked in space, invalid target & no active game: all 7 methods reject with zero DB mutations.
    let mut space = SessionState::new(703);
    space.user_id = Some(1);
    space.character_id = Some(char_id);
    space.solar_system_id = Some(sys_id);
    space.ship_id = Some(ship_id);
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 587,
            position: DVec3::new(10_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 200.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 30.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    for (meth, args) in all_methods {
        let res = handle_pve_hacking(&ctx, &mut space, "hackingMgr", meth, args, &kw)
            .await
            .expect("handled");
        assert!(res.is_err(), "invalid-target hackingMgr.{meth} must reject");
    }
    for (meth, args) in [
        ("ClickedOnTile", vec![PyRep::Integer(1)]),
        ("QuitHackingAttempt", vec![]),
        ("UsedUtilityElement", vec![PyRep::Integer(0), PyRep::Integer(0)]),
        ("UsedUtilityElementOnVirus", vec![PyRep::Integer(0)]),
    ] {
        let res = handle_pve_hacking(&ctx, &mut space, "hackingMgr", meth, &args, &kw)
            .await
            .expect("handled");
        assert!(res.is_err(), "no-active-game hackingMgr.{meth} must reject");
    }
    assert_eq!(ctx.db.state_epoch(), epoch0, "rejected hacking calls must not bump state_epoch");
    assert_eq!(ctx.db.tx_commit_count(), commits0, "rejected hacking calls must not bump tx_commit_count");

    // 5. Valid undocked hacking session succeeds end-to-end, and getters remain read-pure.
    let _ = sys_handle
        .spawn_ball(Ball {
            id: can_id,
            type_id: 22_270,
            position: DVec3::new(10_500.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1.0,
            radius: 100.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    with_pve_exploration_reg(&ctx, |reg| {
        reg.seeded_systems.insert(sys_id, true);
        reg.sites_by_system.entry(sys_id).or_default().push(
            crate::services::pve_exploration::PveSiteInstance {
                site_id: "DATA_GUARD_1".into(),
                dungeon_id: 2011,
                dungeon_name: "Central Data Bank".into(),
                solar_system_id: sys_id,
                site_category: "Data".into(),
                position: [10_500.0, 0.0, 0.0],
                hackable_can_ids: vec![can_id],
                ..Default::default()
            },
        );
    });
    let start_rep = handle_pve_hacking(
        &ctx,
        &mut space,
        "hackingMgr",
        "StartNewGameInstance",
        &[PyRep::Integer(can_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("valid StartNewGameInstance succeeds");
    assert_eq!(start_rep.dict_get("status"), Some(&PyRep::String("InProgress".into())));

    let epoch_after_start = ctx.db.state_epoch();
    let commits_after_start = ctx.db.tx_commit_count();
    let prog = handle_pve_hacking(
        &ctx,
        &mut space,
        "hackingMgr",
        "GetMyHackProgress",
        &[PyRep::Integer(can_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("GetMyHackProgress succeeds");
    assert_eq!(prog.dict_get("status"), Some(&PyRep::String("InProgress".into())));
    let board = handle_pve_hacking(
        &ctx,
        &mut space,
        "hackingMgr",
        "values",
        &[PyRep::Integer(can_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("values succeeds");
    assert!(matches!(board, PyRep::List(ref l) if l.len() == 24));
    assert_eq!(ctx.db.state_epoch(), epoch_after_start, "hackingMgr getters must not bump state_epoch");
    assert_eq!(ctx.db.tx_commit_count(), commits_after_start, "hackingMgr getters must not bump tx_commit_count");

    let click_rep = handle_pve_hacking(
        &ctx,
        &mut space,
        "hackingMgr",
        "ClickedOnTile",
        &[PyRep::Integer(can_id), PyRep::Integer(1)],
        &kw,
    )
    .await
    .unwrap()
    .expect("ClickedOnTile succeeds");
    assert!(click_rep.dict_get("status").is_some());

    let quit_rep = handle_pve_hacking(
        &ctx,
        &mut space,
        "hackingMgr",
        "QuitHackingAttempt",
        &[PyRep::Integer(can_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("QuitHackingAttempt succeeds");
    assert_eq!(quit_rep, PyRep::Bool(true));

    // 6. BF-1354B-06 / BF-1354B-01 regressions:
    //    - 64-bit tile index truncation `(1i64 << 32) + idx` rejects with InvalidTileIndex
    //    - Missing solar_system_id rejects ClickedOnTile with NoSolarSystem
    //    - Cross-system QuitHackingAttempt & implicit 0-arg GetMyHackProgress reject with NotInSiteSystem
    //      without destroying the active game in the original system.
    let re_start = handle_pve_hacking(
        &ctx,
        &mut space,
        "hackingMgr",
        "StartNewGameInstance",
        &[PyRep::Integer(can_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("restart fresh game succeeds");
    assert_eq!(re_start.dict_get("status"), Some(&PyRep::String("InProgress".into())));
    with_pve_exploration_reg(&ctx, |reg| {
        if let Some(g) = reg.hacking_games.get_mut(&(char_id, can_id)) {
            if !g.utilities.is_empty() {
                g.utilities[0] = Some(HackingUtilityKind::KernelRot);
            }
        }
    });

    const INVALID_TILE_CODE: &str = "InvalidTileIndex";
    const HACKING_SVC: &str = "hackingMgr";
    let truncated_tile = PyRep::Integer((1i64 << 32) + 1);
    for (meth, args) in [
        ("ClickedOnTile", vec![PyRep::Integer(can_id), truncated_tile.clone()]),
        ("UsedUtilityElement", vec![PyRep::Integer(can_id), PyRep::Integer(0), truncated_tile.clone()]),
        ("UsedUtilityElementOnVirus", vec![PyRep::Integer(can_id), PyRep::Integer(0), truncated_tile]),
    ] {
        let err = handle_pve_hacking(&ctx, &mut space, HACKING_SVC, meth, &args, &kw)
            .await
            .unwrap()
            .expect_err(&format!("overflow tile index in {meth} must reject"));
        assert_eq!(err.code, INVALID_TILE_CODE);
    }

    let mut no_sys = space.clone();
    no_sys.solar_system_id = None;
    let no_sys_err = handle_pve_hacking(
        &ctx,
        &mut no_sys,
        "hackingMgr",
        "ClickedOnTile",
        &[PyRep::Integer(can_id), PyRep::Integer(1)],
        &kw,
    )
    .await
    .unwrap()
    .expect_err("missing solar_system_id in ClickedOnTile must reject");
    assert_eq!(no_sys_err.code, "NoSolarSystem");
    assert!(no_sys_err.client_error.is_some(), "new guard must emit typed ClientError");

    let mut other_sys = space.clone();
    other_sys.solar_system_id = Some(30_000_144);
    let epoch_before_cross = ctx.db.state_epoch();
    let commits_before_cross = ctx.db.tx_commit_count();
    let cross_prog_err = handle_pve_hacking(
        &ctx,
        &mut other_sys,
        "hackingMgr",
        "GetMyHackProgress",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .expect_err("implicit GetMyHackProgress across systems must reject");
    assert_eq!(cross_prog_err.code, "NotInSiteSystem");
    assert!(cross_prog_err.client_error.is_some());

    let cross_quit_err = handle_pve_hacking(
        &ctx,
        &mut other_sys,
        "hackingMgr",
        "QuitHackingAttempt",
        &[PyRep::Integer(can_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect_err("cross-system QuitHackingAttempt must reject");
    assert_eq!(cross_quit_err.code, "NotInSiteSystem");
    assert!(cross_quit_err.client_error.is_some());
    assert_eq!(ctx.db.state_epoch(), epoch_before_cross);
    assert_eq!(ctx.db.tx_commit_count(), commits_before_cross);
    assert!(read_pve_exploration_reg(&ctx, |reg| {
        reg.hacking_games
            .get(&(char_id, can_id))
            .is_some_and(|g| g.status == "InProgress")
    }));
}

fn hacking_test_sde_with_analyzers() -> Arc<SdeRegistry> {
    let mut sde = (*shared_sde()).clone();
    if !sde.types.contains_key(&22_586) {
        if let Some(mut t) = sde.types.values().next().cloned() {
            t.type_id = 22_586;
            t.group_id = 538;
            t.name = "Data Analyzer I".into();
            t.published = true;
            t.default_attributes.clear();
            t.default_attributes.insert(1909, 60.0);
            t.default_attributes.insert(1910, 30.0);
            t.default_attributes.insert(1911, 2.0);
            t.default_attributes.insert(54, 5_000.0);
            sde.types.insert(22_586, t);
        }
    }
    if !sde.types.contains_key(&22_588) {
        if let Some(mut t) = sde.types.values().next().cloned() {
            t.type_id = 22_588;
            t.group_id = 538;
            t.name = "Relic Analyzer I".into();
            t.published = true;
            t.default_attributes.clear();
            t.default_attributes.insert(183, 13_278.0);
            t.default_attributes.insert(1909, 40.0);
            t.default_attributes.insert(1910, 20.0);
            t.default_attributes.insert(1911, 2.0);
            t.default_attributes.insert(54, 5_000.0);
            sde.types.insert(22_588, t);
        }
    }
    Arc::new(sde)
}

async fn setup_hacking_guard_context(
    char_id: i64,
    ship_id: i64,
    analyzer_id: i64,
    can_id: i64,
    spawn_system: bool,
) -> (ServerContext, SessionState, SolarSystemId) {
    let sys_id: SolarSystemId = 30_000_142;
    let sde = hacking_test_sde_with_analyzers();
    let db = Arc::new(WorldRepository::new_empty());
    db.provision_gm_account(&format!("hk_guard_{char_id}"), "pw", char_id, "FailClosedPilot");
    db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
        }
        snap.item_positions.insert(can_id, [10_200.0, 0.0, 0.0]);
        for (id, tid, loc, flag) in [
            (ship_id, 587_u32, sys_id as i64, eve_db::InventoryFlag::None),
            (analyzer_id, 22_586, ship_id, eve_db::InventoryFlag::MedSlot0),
        ] {
            snap.items.insert(
                id,
                eve_db::InventoryItemRecord {
                    item_id: id,
                    type_id: tid,
                    owner_id: char_id,
                    location_id: loc,
                    flag,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
    });
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_776_900_000)));
    if spawn_system {
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        for (ball_id, type_id, pos, radius) in [
            (ship_id, 587_u32, DVec3::new(10_000.0, 0.0, 0.0), 30.0),
            (can_id, 22_270_u32, DVec3::new(10_200.0, 0.0, 0.0), 100.0),
        ] {
            let _ = sys_handle
                .spawn_ball(Ball {
                    id: ball_id,
                    type_id,
                    position: pos,
                    velocity: DVec3::ZERO,
                    max_velocity: 200.0,
                    inertia: 1.0,
                    mass: 1_000_000.0,
                    radius,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                })
                .await;
        }
    }
    with_pve_exploration_reg(&ctx, |reg| {
        reg.seeded_systems.insert(sys_id, true);
        reg.sites_by_system.entry(sys_id).or_default().push(
            crate::services::pve_exploration::PveSiteInstance {
                site_id: format!("DATA_{sys_id}_{char_id}"),
                dungeon_id: 2011,
                dungeon_name: "Central Data Bank".into(),
                solar_system_id: sys_id,
                site_category: "Data".into(),
                position: [10_200.0, 0.0, 0.0],
                hackable_can_ids: vec![can_id],
                ..Default::default()
            },
        );
    });
    let mut session = SessionState::new(790);
    session.user_id = Some(1);
    session.character_id = Some(char_id);
    session.solar_system_id = Some(sys_id);
    session.ship_id = Some(ship_id);
    (ctx, session, sys_id)
}

fn seed_vulnerable_active_board(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
    char_id: i64,
    can_id: i64,
) {
    with_pve_exploration_reg(ctx, |reg| {
        let mut tiles = build_hacking_grid(can_id, false);
        tiles[0].revealed = true;
        tiles[1].revealed = true;
        tiles[1].kind = HackingNodeKind::SystemCore;
        tiles[1].coherence = 1;
        tiles[1].strength = 1;
        reg.hacking_games.insert(
            (char_id, can_id),
            HackingGameSession {
                game_id: 99_910,
                character_id: char_id,
                solar_system_id: sys_id,
                container_id: can_id,
                site_id: format!("DATA_{sys_id}_{char_id}"),
                is_relic: false,
                is_ghost_site: false,
                is_sleeper_cache_puzzle: false,
                virus_coherence: 60,
                max_virus_coherence: 60,
                base_virus_strength: 30,
                polymorphic_shield_charges: 0,
                secondary_vector_target: None,
                utilities: vec![
                    Some(HackingUtilityKind::SelfRepair),
                    Some(HackingUtilityKind::KernelRot),
                ],
                tiles,
                status: "InProgress".into(),
            },
        );
    });
}

fn assert_active_board_untouched(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
    char_id: i64,
    can_id: i64,
) {
    assert!(read_pve_exploration_reg(ctx, |reg| {
        let game = &reg.hacking_games[&(char_id, can_id)];
        game.status == "InProgress"
            && game.tiles[1].coherence == 1
            && game.utilities[0].is_some()
            && game.utilities[1].is_some()
            && !reg.hacked_containers.contains(&(sys_id, can_id))
            && !reg.self_repair_turns.contains_key(&(char_id, can_id))
    }));
}

async fn assert_all_midgame_mutators_fail_closed(
    ctx: &ServerContext,
    session: &mut SessionState,
    can_id: i64,
    expected_code: &str,
) {
    let kw = HashMap::new();
    for (meth, args) in [
        ("ClickedOnTile", vec![PyRep::Integer(can_id), PyRep::Integer(1)]),
        (
            "UsedUtilityElement",
            vec![PyRep::Integer(can_id), PyRep::Integer(1), PyRep::Integer(1)],
        ),
        (
            "UsedUtilityElementOnVirus",
            vec![PyRep::Integer(can_id), PyRep::Integer(0)],
        ),
    ] {
        let err = handle_pve_hacking(ctx, session, "hackingMgr", meth, &args, &kw)
            .await
            .expect("handled")
            .expect_err(&format!("{meth} must fail closed with {expected_code}"));
        assert_eq!(err.code, expected_code, "{meth} unexpected error code");
        if expected_code != "AnalyzerModuleRequired" {
            assert!(
                err.client_error.is_some(),
                "{meth} must return typed ClientError for {expected_code}"
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn midgame_hacking_fails_closed_when_system_actor_is_unloaded() {
    let (char_id, ship_id, analyzer_id, can_id) =
        (90_043_101, 1_043_000_101, 1_043_000_102, 1_043_900_101);
    let (ctx, mut session, sys_id) =
        setup_hacking_guard_context(char_id, ship_id, analyzer_id, can_id, false).await;
    seed_vulnerable_active_board(&ctx, sys_id, char_id, can_id);

    assert_all_midgame_mutators_fail_closed(&ctx, &mut session, can_id, "ShipNotInSpace").await;
    assert_active_board_untouched(&ctx, sys_id, char_id, can_id);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn midgame_hacking_fails_closed_on_missing_ship_ball_missing_analyzer_and_no_active_ship() {
    let (char_id, ship_id, analyzer_id, can_id) =
        (90_043_201, 1_043_000_201, 1_043_000_202, 1_043_900_201);
    let (ctx, mut session, sys_id) =
        setup_hacking_guard_context(char_id, ship_id, analyzer_id, can_id, true).await;
    let kw = HashMap::new();

    // Start legitimately in space, then plant a 1-click SystemCore & utilities to verify fail-closed guards.
    let started = handle_pve_hacking(
        &ctx,
        &mut session,
        "hackingMgr",
        "StartNewGameInstance",
        &[PyRep::Integer(can_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("StartNewGameInstance succeeds");
    assert_eq!(started.dict_get("status"), Some(&PyRep::String("InProgress".into())));
    seed_vulnerable_active_board(&ctx, sys_id, char_id, can_id);

    // 1. Ship ball despawned mid-hack -> ShipNotInSpace across all 3 mutators.
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    assert!(sys_handle.remove_ball(ship_id).await.is_some());
    assert_all_midgame_mutators_fail_closed(&ctx, &mut session, can_id, "ShipNotInSpace").await;
    assert_active_board_untouched(&ctx, sys_id, char_id, can_id);

    // Re-spawn ship ball at in-range coordinates.
    let _ = sys_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 587,
            position: DVec3::new(10_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 200.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 30.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    // 2. Unequipped/swapped/removed analyzer mid-hack -> AnalyzerModuleRequired
    // (`resolve_fitted_analyzer_virus_stats` validates fitted slots 11..=34 in `snap.items`).
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(mod_item) = snap.items.get_mut(&analyzer_id) {
            mod_item.flag = eve_db::InventoryFlag::Cargo;
        }
    });
    assert_all_midgame_mutators_fail_closed(&ctx, &mut session, can_id, "AnalyzerModuleRequired")
        .await;
    assert_active_board_untouched(&ctx, sys_id, char_id, can_id);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(mod_item) = snap.items.get_mut(&analyzer_id) {
            mod_item.flag = eve_db::InventoryFlag::MedSlot0;
            mod_item.type_id = 22_588; // Relic Analyzer cannot satisfy an active Data hack
        }
    });
    assert_all_midgame_mutators_fail_closed(&ctx, &mut session, can_id, "AnalyzerModuleRequired")
        .await;
    assert_active_board_untouched(&ctx, sys_id, char_id, can_id);

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&analyzer_id);
    });
    assert_all_midgame_mutators_fail_closed(&ctx, &mut session, can_id, "AnalyzerModuleRequired")
        .await;
    assert_active_board_untouched(&ctx, sys_id, char_id, can_id);

    // Restore valid Data Analyzer I.
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            analyzer_id,
            eve_db::InventoryItemRecord {
                item_id: analyzer_id,
                type_id: 22_586,
                owner_id: char_id,
                location_id: ship_id,
                flag: eve_db::InventoryFlag::MedSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    // 3. NoActiveShip mid-hack (both explicit session.ship_id = Some(0) and session.ship_id = None with DB active_ship_id = 0).
    session.ship_id = Some(0);
    assert_all_midgame_mutators_fail_closed(&ctx, &mut session, can_id, "NoActiveShip").await;
    assert_active_board_untouched(&ctx, sys_id, char_id, can_id);

    session.ship_id = None;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.active_ship_id = 0;
        }
    });
    assert_all_midgame_mutators_fail_closed(&ctx, &mut session, can_id, "NoActiveShip").await;
    assert_active_board_untouched(&ctx, sys_id, char_id, can_id);

    // Restoring DB active_ship_id with session.ship_id = None resolves ship cleanly across read getters too.
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.active_ship_id = ship_id;
        }
    });
    let vals = handle_pve_hacking(
        &ctx,
        &mut session,
        "hackingMgr",
        "values",
        &[PyRep::Integer(can_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("values resolves active_ship_id when session.ship_id is None");
    assert!(matches!(vals, PyRep::List(ref l) if l.len() == 24));
    let prog = handle_pve_hacking(
        &ctx,
        &mut session,
        "hackingMgr",
        "GetMyHackProgress",
        &[PyRep::Integer(can_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("GetMyHackProgress resolves active_ship_id when session.ship_id is None");
    assert_eq!(prog.dict_get("status"), Some(&PyRep::String("InProgress".into())));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hacking_tile_coord_saturation_and_malformed_arg_rejections() {
    // Overflow-safe saturating coordinate extraction for huge i64 coordinates.
    assert_eq!(
        extract_tile_index(Some(&PyRep::Tuple(vec![
            PyRep::Integer(i64::MAX),
            PyRep::Integer(i64::MAX),
        ]))),
        Some(23)
    );
    assert_eq!(
        extract_tile_index(Some(&PyRep::List(vec![
            PyRep::Integer(i64::MIN),
            PyRep::Integer(i64::MIN),
        ]))),
        Some(0)
    );
    assert_eq!(
        extract_tile_index(Some(&PyRep::Tuple(vec![
            PyRep::Integer(0),
            PyRep::Integer(i64::MAX),
        ]))),
        Some(23)
    );
    assert_eq!(extract_tile_index(Some(&PyRep::String("bad".into()))), None);

    let (char_id, ship_id, analyzer_id, can_id) =
        (90_043_301, 1_043_000_301, 1_043_000_302, 1_043_900_301);
    let (ctx, mut session, sys_id) =
        setup_hacking_guard_context(char_id, ship_id, analyzer_id, can_id, true).await;
    seed_vulnerable_active_board(&ctx, sys_id, char_id, can_id);
    let kw = HashMap::new();

    for (meth, args, expected_code) in [
        (
            "ClickedOnTile",
            vec![PyRep::String("bad_can".into()), PyRep::Integer(1)],
            "TargetNotFound",
        ),
        (
            "ClickedOnTile",
            vec![PyRep::Integer(can_id), PyRep::String("bad_tile".into())],
            "InvalidTileIndex",
        ),
        ("ClickedOnTile", vec![PyRep::None], "InvalidTileIndex"),
        ("ClickedOnTile", vec![], "InvalidTileIndex"),
        (
            "UsedUtilityElement",
            vec![
                PyRep::String("bad_can".into()),
                PyRep::Integer(0),
                PyRep::Integer(1),
            ],
            "TargetNotFound",
        ),
        (
            "UsedUtilityElement",
            vec![
                PyRep::Integer(can_id),
                PyRep::String("bad_slot".into()),
                PyRep::Integer(1),
            ],
            "UtilitySlotEmpty",
        ),
        (
            "UsedUtilityElement",
            vec![
                PyRep::Integer(can_id),
                PyRep::Integer(0),
                PyRep::String("bad_tile".into()),
            ],
            "InvalidTileIndex",
        ),
        (
            "UsedUtilityElement",
            vec![PyRep::Integer(0), PyRep::String("bad_tile".into())],
            "InvalidTileIndex",
        ),
        ("UsedUtilityElement", vec![], "UtilitySlotEmpty"),
    ] {
        let err = handle_pve_hacking(&ctx, &mut session, "hackingMgr", meth, &args, &kw)
            .await
            .expect("handled")
            .expect_err(&format!("{meth} {args:?} must reject malformed args"));
        assert_eq!(err.code, expected_code, "{meth} {args:?}");
    }
    assert_active_board_untouched(&ctx, sys_id, char_id, can_id);

    // Valid 1-arg UsedUtilityElement([slot]) continues to work for virus-targeted utilities.
    let rep = handle_pve_hacking(
        &ctx,
        &mut session,
        "hackingMgr",
        "UsedUtilityElement",
        &[PyRep::Integer(0)],
        &kw,
    )
    .await
    .unwrap()
    .expect("1-arg UsedUtilityElement SelfRepair succeeds");
    assert_eq!(rep.dict_get("usedSlot"), Some(&PyRep::Integer(0)));
}
