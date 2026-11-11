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
