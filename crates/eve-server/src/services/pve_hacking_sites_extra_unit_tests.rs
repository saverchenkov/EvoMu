//! Extra branch-coverage and mutation-killing unit tests for `pve_hacking_sites.rs`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::pve_exploration::{
    with_pve_exploration_reg, HackingNodeKind, HackingTileState, HackingUtilityKind,
    PveSiteInstance,
};
use crate::services::pve_hacking_sites::{
    are_hacking_tiles_adjacent, build_hacking_grid, hacking_hex_distance,
    hacking_tile_axial_coords, hacking_tile_distance_to_core, handle_pve_hacking,
    is_blocking_defensive_tile,
};
use eve_db::clock::VirtualServerClock;
use eve_db::{InventoryFlag, InventoryItemRecord, UpwellStructureRecord, WorldRepository};
use eve_destiny::{Ball, CombatState, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::{SdeRegistry, SolarSystemId};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn i(v: i64) -> PyRep {
    PyRep::Integer(v)
}

fn hacking_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| {
        let mut sde = SdeRegistry::with_canonical_seed();
        if let Some(base_t) = sde.types.values().next().cloned() {
            for (tid, name) in [(22_586_u32, "Data Analyzer I"), (22_588_u32, "Relic Analyzer I")] {
                let mut t = base_t.clone();
                t.type_id = tid;
                t.group_id = 538;
                t.name = name.into();
                t.published = true;
                t.default_attributes.insert(1909, 60.0);
                t.default_attributes.insert(1910, 30.0);
                t.default_attributes.insert(1911, 3.0);
                t.default_attributes.insert(54, 5_000.0);
                sde.types.insert(tid, t);
            }
        }
        Arc::new(sde)
    }))
}

async fn call_hacking(
    ctx: &ServerContext,
    session: &mut SessionState,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let kw = HashMap::new();
    handle_pve_hacking(ctx, session, "hackingMgr", method, args, &kw)
        .await
        .expect("hackingMgr method handled")
}

#[test]
fn grid_axial_distance_blocking_and_core_fallback_branches() {
    assert_eq!(hacking_tile_axial_coords(0), (0, 0));
    assert_eq!(hacking_tile_axial_coords(5), (5, 0));
    assert_eq!(hacking_tile_axial_coords(6), (0, 1));
    assert_eq!(hacking_tile_axial_coords(23), (5, 3));

    assert_eq!(hacking_hex_distance(0, 0), 0);
    assert_eq!(hacking_hex_distance(0, 6), 1);
    assert!(are_hacking_tiles_adjacent(0, 6));
    assert!(!are_hacking_tiles_adjacent(0, 0));
    assert!(!are_hacking_tiles_adjacent(0, 12));

    for kind in [
        HackingNodeKind::Firewall,
        HackingNodeKind::Antivirus,
        HackingNodeKind::VirusSuppressor,
        HackingNodeKind::RestorationNode,
    ] {
        let mut t = HackingTileState {
            tile_index: 1,
            kind,
            coherence: 20,
            strength: 10,
            revealed: true,
            blocked_by_defensive: false,
            utility_drop: None,
        };
        assert!(is_blocking_defensive_tile(&t));
        t.coherence = 0;
        assert!(!is_blocking_defensive_tile(&t));
    }
    for non_def in [
        HackingNodeKind::SystemCore,
        HackingNodeKind::DataCache,
        HackingNodeKind::UtilitySubsystem,
        HackingNodeKind::Empty,
    ] {
        let t = HackingTileState {
            tile_index: 2,
            kind: non_def,
            coherence: 50,
            strength: 20,
            revealed: true,
            blocked_by_defensive: false,
            utility_drop: None,
        };
        assert!(!is_blocking_defensive_tile(&t));
    }

    let mut grid = build_hacking_grid(123_456, false);
    for t in &mut grid {
        if t.kind == HackingNodeKind::SystemCore {
            t.kind = HackingNodeKind::Empty;
        }
    }
    assert_eq!(hacking_tile_distance_to_core(15, &grid), 0);
    assert!(hacking_tile_distance_to_core(0, &grid) >= 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn start_game_structure_locks_proximity_cloaking_and_ghost_expiry() {
    let sys_id: SolarSystemId = 30_000_142;
    let (char_id, peer_id, corp_id, enemy_corp_id) = (90_050_101, 90_050_102, 98_050_101, 98_050_102);
    let (ship_id, ghost_can_id, own_struct_id, enemy_struct_id) =
        (1_050_000_101, 1_050_900_001, 1_050_800_001, 1_050_800_002);

    let sde = hacking_sde();
    let db = Arc::new(WorldRepository::new_empty());
    db.provision_gm_account("hk1", "pw", char_id, "HackerOne");
    db.provision_gm_account("hk2", "pw", peer_id, "HackerTwo");
    db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        for (idx, tid, flag) in [(1, 22_586_u32, InventoryFlag::MedSlot0), (2, 22_588_u32, InventoryFlag::MedSlot1)] {
            snap.items.insert(
                ship_id + idx,
                InventoryItemRecord {
                    item_id: ship_id + idx,
                    type_id: tid,
                    owner_id: char_id,
                    location_id: ship_id,
                    flag,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        }
        for (sid, owner) in [(own_struct_id, corp_id), (enemy_struct_id, enemy_corp_id)] {
            snap.structures.insert(
                sid,
                UpwellStructureRecord {
                    structure_id: sid,
                    type_id: 35_832,
                    owner_corp_id: owner,
                    solar_system_id: sys_id,
                    name: "Astrahus".into(),
                    position: [50_000.0, 0.0, 0.0],
                    state: "Online".into(),
                    quantum_core_installed: true,
                    fuel_blocks: 100,
                    ..Default::default()
                },
            );
        }
    });

    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_776_000_000)));

    let mut sess = SessionState::new(601);
    assert!(call_hacking(&ctx, &mut sess, "StartNewGameInstance", &[]).await.is_err());
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    assert_eq!(call_hacking(&ctx, &mut sess, "StartNewGameInstance", &[]).await.unwrap_err().code, "NoSolarSystem");
    sess.solar_system_id = Some(sys_id);

    assert_eq!(call_hacking(&ctx, &mut sess, "StartNewGameInstance", &[PyRep::None]).await.unwrap_err().code, "TargetNotFound");
    assert_eq!(
        call_hacking(&ctx, &mut sess, "StartNewGameInstance", &[i(own_struct_id)]).await.unwrap_err().code,
        "CannotHackOwnStructure"
    );

    sess.station_id = Some(60_003_760);
    assert_eq!(
        call_hacking(&ctx, &mut sess, "StartNewGameInstance", &[i(enemy_struct_id)]).await.unwrap_err().code,
        "ShipDocked"
    );
    sess.station_id = None;
    sess.ship_id = None;
    assert_eq!(
        call_hacking(&ctx, &mut sess, "StartNewGameInstance", &[i(enemy_struct_id)]).await.unwrap_err().code,
        "NoActiveShip"
    );
    sess.ship_id = Some(ship_id);
    assert_eq!(
        call_hacking(&ctx, &mut sess, "StartNewGameInstance", &[i(enemy_struct_id)]).await.unwrap_err().code,
        "ShipNotInSpace"
    );

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let mut cs = CombatState::default();
    cs.is_cloaked = true;
    let _ = sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 587,
                position: DVec3::new(500_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 200.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 30.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            cs,
        )
        .await;
    assert_eq!(
        call_hacking(&ctx, &mut sess, "StartNewGameInstance", &[i(enemy_struct_id)]).await.unwrap_err().code,
        "CannotHackWhileCloaked"
    );
    sys_handle.try_update_combat_state(ship_id, |c| c.is_cloaked = false);
    assert_eq!(
        call_hacking(&ctx, &mut sess, "StartNewGameInstance", &[i(enemy_struct_id)]).await.unwrap_err().code,
        "OutOfRange"
    );

    let _ = sys_handle.set_ball_position(ship_id, DVec3::new(51_000.0, 0.0, 0.0)).await;
    let st_hack = call_hacking(&ctx, &mut sess, "StartNewGameInstance", &[i(1), i(enemy_struct_id)]).await.unwrap();
    assert_eq!(st_hack.dict_get("siteID"), Some(&PyRep::String(format!("STRUCTURE_{enemy_struct_id}"))));

    let mut peer_sess = sess.clone();
    peer_sess.character_id = Some(peer_id);
    assert_eq!(
        call_hacking(&ctx, &mut peer_sess, "StartNewGameInstance", &[i(enemy_struct_id)]).await.unwrap_err().code,
        "HackingGameAlreadyInProgress"
    );

    with_pve_exploration_reg(&ctx, |reg| {
        reg.hacking_games.remove(&(char_id, enemy_struct_id));
        reg.structure_char_failures.insert((char_id, enemy_struct_id), 2);
    });
    assert_eq!(
        call_hacking(&ctx, &mut sess, "StartNewGameInstance", &[i(enemy_struct_id)]).await.unwrap_err().code,
        "HackingGameAlreadyFinished"
    );

    with_pve_exploration_reg(&ctx, |reg| {
        reg.seeded_systems.insert(sys_id, true);
        let now = reg.current_now_sec.max(1_776_000_000);
        reg.sites_by_system.entry(sys_id).or_default().push(
            PveSiteInstance {
                site_id: "GHOST_EXP_1".into(),
                dungeon_id: 3001,
                dungeon_name: "Covert Research Facility".into(),
                solar_system_id: sys_id,
                site_category: "Ghost".into(),
                position: [51_000.0, 0.0, 0.0],
                hackable_can_ids: vec![ghost_can_id],
                ghost_timer_started_at: Some(now.saturating_sub(200)),
                ghost_detonate_after_sec: 90,
                ..Default::default()
            },
        );
    });
    let det_res = call_hacking(&ctx, &mut sess, "StartNewGameInstance", &[i(ghost_can_id)]).await.unwrap();
    assert_eq!(det_res.dict_get("status"), Some(&PyRep::String("GhostSiteDetonated".into())));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn utilities_tile_clicks_sleeper_puzzle_and_second_failure_explosion() {
    let sys_id: SolarSystemId = 30_000_142;
    let (char_id, ship_id) = (90_050_201, 1_050_000_201);
    let (sleeper_can_0, sleeper_can_1, sleeper_can_2, data_can_id) =
        (1_050_910_001, 1_050_910_002, 1_050_910_003, 1_050_920_001);

    let sde = hacking_sde();
    let db = Arc::new(WorldRepository::new_empty());
    db.provision_gm_account("hk_u", "pw", char_id, "HackerUtil");
    db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
        }
        for (id, tid, loc, flag) in [
            (ship_id, 587, sys_id as i64, InventoryFlag::None),
            (ship_id + 1, 22_586, ship_id, InventoryFlag::MedSlot0),
        ] {
            snap.items.insert(
                id,
                InventoryItemRecord {
                    item_id: id,
                    type_id: tid,
                    owner_id: char_id,
                    location_id: loc,
                    flag,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        }
    });

    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_776_100_000)));
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

    with_pve_exploration_reg(&ctx, |reg| {
        reg.seeded_systems.insert(sys_id, true);
        reg.sites_by_system.insert(
            sys_id,
            vec![
                PveSiteInstance {
                    site_id: "SLEEPER_CACHE_1".into(),
                    dungeon_id: 4001,
                    dungeon_name: "Standard Sleeper Cache".into(),
                    solar_system_id: sys_id,
                    site_category: "SleeperCache".into(),
                    position: [10_500.0, 0.0, 0.0],
                    hackable_can_ids: vec![sleeper_can_0, sleeper_can_1, sleeper_can_2],
                    sleeper_gas_cloud_active: true,
                    ..Default::default()
                },
                PveSiteInstance {
                    site_id: "DATA_SITE_1".into(),
                    dungeon_id: 4002,
                    dungeon_name: "Central Guristas Sparking Transmitter".into(),
                    solar_system_id: sys_id,
                    site_category: "Data".into(),
                    position: [10_500.0, 0.0, 0.0],
                    hackable_can_ids: vec![data_can_id],
                    ..Default::default()
                },
            ],
        );
    });

    let mut sess = SessionState::new(602);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);

    let not_started = call_hacking(&ctx, &mut sess, "GetMyHackProgress", &[i(sleeper_can_1)]).await.unwrap();
    assert_eq!(not_started.dict_get("status"), Some(&PyRep::String("NotStarted".into())));
    assert_eq!(call_hacking(&ctx, &mut sess, "values", &[]).await.unwrap_err().code, "TargetNotFound");

    call_hacking(&ctx, &mut sess, "StartNewGameInstance", &[i(sleeper_can_1)]).await.unwrap();

    with_pve_exploration_reg(&ctx, |reg| {
        let g = reg.hacking_games.get_mut(&(char_id, sleeper_can_1)).unwrap();
        g.utilities = vec![
            Some(HackingUtilityKind::PolymorphicShield),
            Some(HackingUtilityKind::KernelRot),
            Some(HackingUtilityKind::SecondaryVector),
            Some(HackingUtilityKind::SelfRepair),
        ];
        g.tiles[1].revealed = true;
        g.tiles[1].coherence = 40;
        g.tiles[2].revealed = false;
    });

    assert_eq!(
        call_hacking(&ctx, &mut sess, "UsedUtilityElement", &[i(sleeper_can_1), i(1), i(-1)]).await.unwrap_err().code,
        "InvalidTileIndex"
    );
    assert_eq!(
        call_hacking(&ctx, &mut sess, "ClickedOnTile", &[i(sleeper_can_1), i(-1)]).await.unwrap_err().code,
        "InvalidTileIndex"
    );
    assert_eq!(
        call_hacking(&ctx, &mut sess, "UsedUtilityElement", &[i(sleeper_can_1), i(1), i(2)]).await.unwrap_err().code,
        "TargetTileNotRevealed"
    );

    let u0 = call_hacking(&ctx, &mut sess, "UsedUtilityElementOnVirus", &[i(sleeper_can_1), i(0)]).await.unwrap();
    assert_eq!(u0.dict_get("polymorphicShieldCharges"), Some(&i(2)));
    assert_eq!(
        call_hacking(&ctx, &mut sess, "UsedUtilityElementOnVirus", &[i(sleeper_can_1), i(0)]).await.unwrap_err().code,
        "UtilitySlotEmpty"
    );

    assert!(call_hacking(&ctx, &mut sess, "UsedUtilityElement", &[i(sleeper_can_1), i(1), i(1)]).await.is_ok());
    assert!(call_hacking(
        &ctx,
        &mut sess,
        "UsedUtilityElement",
        &[i(2), PyRep::Tuple(vec![i(1), i(0)])]
    )
    .await
    .is_ok());
    assert!(call_hacking(&ctx, &mut sess, "UsedUtilityElement", &[i(3), i(0)]).await.is_ok());

    with_pve_exploration_reg(&ctx, |reg| {
        let g = reg.hacking_games.get_mut(&(char_id, sleeper_can_1)).unwrap();
        g.tiles[1].kind = HackingNodeKind::SystemCore;
        g.tiles[1].coherence = 10;
        g.tiles[1].strength = 5;
    });
    let win_res = call_hacking(
        &ctx,
        &mut sess,
        "ClickedOnTile",
        &[i(sleeper_can_1), PyRep::Tuple(vec![i(1), i(0)])],
    )
    .await
    .unwrap();
    assert_eq!(win_res.dict_get("status"), Some(&PyRep::String("Won".into())));
    assert_eq!(win_res.dict_get("sleeperPuzzleSolved"), Some(&PyRep::Bool(true)));
    assert!(with_pve_exploration_reg(&ctx, |reg| reg.sites_by_system[&sys_id][0].sleeper_sentry_disabled));

    with_pve_exploration_reg(&ctx, |reg| {
        reg.failure_counts.insert((sys_id, data_can_id), 1);
    });
    call_hacking(&ctx, &mut sess, "StartNewGameInstance", &[i(data_can_id)]).await.unwrap();
    assert_eq!(
        call_hacking(&ctx, &mut sess, "QuitHackingAttempt", &[i(data_can_id)]).await.unwrap(),
        PyRep::Bool(true)
    );
    assert!(with_pve_exploration_reg(&ctx, |reg| reg.hacked_containers.contains(&(sys_id, data_can_id))));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn validate_range_cloaked_docked_system_and_ship_fallback_branches() {
    let sys_id: SolarSystemId = 30_000_142;
    let other_sys_id: SolarSystemId = 30_000_144;
    let (char_id, ship_id, can_id) = (90_050_301, 1_050_000_301, 1_050_930_001);

    let sde = hacking_sde();
    let db = Arc::new(WorldRepository::new_empty());
    db.provision_gm_account("hk_rng", "pw", char_id, "RangePilot");
    db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
        }
        snap.item_positions.insert(can_id, [15_030.0, 0.0, 0.0]);
        for (id, tid, loc, flag) in [
            (ship_id, 587, sys_id as i64, InventoryFlag::None),
            (ship_id + 1, 22_586, ship_id, InventoryFlag::MedSlot0),
        ] {
            snap.items.insert(
                id,
                InventoryItemRecord {
                    item_id: id,
                    type_id: tid,
                    owner_id: char_id,
                    location_id: loc,
                    flag,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        }
    });

    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_776_200_000)));
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

    with_pve_exploration_reg(&ctx, |reg| {
        reg.seeded_systems.insert(sys_id, true);
        reg.sites_by_system.insert(
            sys_id,
            vec![PveSiteInstance {
                site_id: "DATA_RNG_1".into(),
                dungeon_id: 4100,
                dungeon_name: "Data Boundary Test".into(),
                solar_system_id: sys_id,
                site_category: "Data".into(),
                position: [15_030.0, 0.0, 0.0],
                hackable_can_ids: vec![can_id],
                ..Default::default()
            }],
        );
    });

    let mut sess = SessionState::new(603);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);

    // Exact surface distance = 5030.0 - 30.0 (ship radius) - 0.0 = 5000.0 <= max_range (5000.0) succeeds.
    call_hacking(&ctx, &mut sess, "StartNewGameInstance", &[i(can_id)])
        .await
        .unwrap();
    with_pve_exploration_reg(&ctx, |reg| {
        let g = reg.hacking_games.get_mut(&(char_id, can_id)).unwrap();
        g.utilities[0] = Some(HackingUtilityKind::SelfRepair);
        g.tiles[1].revealed = true;
    });

    // Docked via station_id or structure_id fails ClickedOnTile / UsedUtilityElementOnVirus.
    sess.structure_id = Some(1_099_000_001);
    assert_eq!(
        call_hacking(&ctx, &mut sess, "ClickedOnTile", &[i(can_id), i(1)])
            .await
            .unwrap_err()
            .code,
        "ShipDocked"
    );
    assert_eq!(
        call_hacking(&ctx, &mut sess, "UsedUtilityElementOnVirus", &[i(can_id), i(0)])
            .await
            .unwrap_err()
            .code,
        "ShipDocked"
    );
    sess.structure_id = None;

    // Different solar system fails validate_active_hacking_ship_and_range with NotInSiteSystem.
    sess.solar_system_id = Some(other_sys_id);
    assert_eq!(
        call_hacking(&ctx, &mut sess, "ClickedOnTile", &[i(can_id), i(1)])
            .await
            .unwrap_err()
            .code,
        "NotInSiteSystem"
    );
    sess.solar_system_id = Some(sys_id);

    // Cloaked fails mid-game ClickedOnTile and UsedUtilityElement.
    sys_handle.try_update_combat_state(ship_id, |c| c.is_cloaked = true);
    assert_eq!(
        call_hacking(&ctx, &mut sess, "ClickedOnTile", &[i(can_id), i(1)])
            .await
            .unwrap_err()
            .code,
        "CannotHackWhileCloaked"
    );
    assert_eq!(
        call_hacking(&ctx, &mut sess, "UsedUtilityElement", &[i(can_id), i(0), i(1)])
            .await
            .unwrap_err()
            .code,
        "CannotHackWhileCloaked"
    );
    sys_handle.try_update_combat_state(ship_id, |c| c.is_cloaked = false);

    // Fallback ship_id == None resolves character.active_ship_id and respects exact > 5000.0 threshold.
    sess.ship_id = None;
    let _ = sys_handle
        .set_ball_position(ship_id, DVec3::new(9_999.0, 0.0, 0.0))
        .await;
    assert_eq!(
        call_hacking(&ctx, &mut sess, "ClickedOnTile", &[i(can_id), i(1)])
            .await
            .unwrap_err()
            .code,
        "OutOfRange"
    );
    let _ = sys_handle
        .set_ball_position(ship_id, DVec3::new(10_000.0, 0.0, 0.0))
        .await;
    assert!(call_hacking(&ctx, &mut sess, "UsedUtilityElementOnVirus", &[i(0), i(can_id)])
        .await
        .is_ok());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn start_game_auto_resolve_destroyed_struct_and_ghost_timer_floor() {
    let sys_id: SolarSystemId = 30_000_142;
    let (char_id, peer_id, ship_id) = (90_050_401, 90_050_402, 1_050_000_401);
    let (ghost_can_id, dead_struct_id) = (1_050_940_001, 1_050_840_001);

    let sde = hacking_sde();
    let db = Arc::new(WorldRepository::new_empty());
    db.provision_gm_account("hk_g1", "pw", char_id, "GhostOne");
    db.provision_gm_account("hk_g2", "pw", peer_id, "GhostTwo");
    db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
        }
        for (id, tid, loc, flag) in [
            (ship_id, 587, sys_id as i64, InventoryFlag::None),
            (ship_id + 1, 22_586, ship_id, InventoryFlag::MedSlot0),
        ] {
            snap.items.insert(
                id,
                InventoryItemRecord {
                    item_id: id,
                    type_id: tid,
                    owner_id: char_id,
                    location_id: loc,
                    flag,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        }
        snap.structures.insert(
            dead_struct_id,
            UpwellStructureRecord {
                structure_id: dead_struct_id,
                type_id: 35_832,
                owner_corp_id: 98_099_999,
                solar_system_id: sys_id,
                name: "Wrecked Cit".into(),
                position: [20_000.0, 0.0, 0.0],
                state: "Destroyed".into(),
                ..Default::default()
            },
        );
    });

    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_776_300_000)));
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 587,
            position: DVec3::new(20_500.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 200.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 30.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    let base_now = with_pve_exploration_reg(&ctx, |reg| {
        reg.seeded_systems.insert(sys_id, true);
        let now = crate::services::pve_exploration::now_sec(reg);
        reg.sites_by_system.insert(
            sys_id,
            vec![PveSiteInstance {
                site_id: "GHOST_FLOOR".into(),
                dungeon_id: 3090,
                dungeon_name: "Lesser Sansha Covert".into(),
                solar_system_id: sys_id,
                site_category: "Ghost".into(),
                position: [20_500.0, 0.0, 0.0],
                hackable_can_ids: vec![ghost_can_id],
                ghost_timer_started_at: None,
                ghost_detonate_after_sec: 15, // First visit clamps effective floor via .max(90) -> 90s!
                ..Default::default()
            }],
        );
        now
    });

    let mut sess = SessionState::new(604);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.solar_system_id = Some(sys_id);

    // structure_id.is_some() -> ShipDocked, ship_id == Some(0) -> NoActiveShip
    sess.structure_id = Some(dead_struct_id);
    sess.ship_id = Some(0);
    assert_eq!(
        call_hacking(&ctx, &mut sess, "StartNewGameInstance", &[])
            .await
            .unwrap_err()
            .code,
        "ShipDocked"
    );
    sess.structure_id = None;
    assert_eq!(
        call_hacking(&ctx, &mut sess, "StartNewGameInstance", &[])
            .await
            .unwrap_err()
            .code,
        "NoActiveShip"
    );
    sess.ship_id = Some(ship_id);

    // Destroyed structure is skipped by struct_info filter -> TargetNotFound.
    assert_eq!(
        call_hacking(&ctx, &mut sess, "StartNewGameInstance", &[i(dead_struct_id)])
            .await
            .unwrap_err()
            .code,
        "TargetNotFound"
    );

    // Empty args auto-resolves ghost_can_id and clamps ghost_detonate_after_sec up to 90s floor.
    let started = call_hacking(&ctx, &mut sess, "StartNewGameInstance", &[])
        .await
        .unwrap();
    assert_eq!(started.dict_get("containerID"), Some(&i(ghost_can_id)));
    assert_eq!(started.dict_get("status"), Some(&PyRep::String("InProgress".into())));
    with_pve_exploration_reg(&ctx, |reg| {
        let s = &reg.sites_by_system[&sys_id][0];
        assert_eq!(s.ghost_detonate_after_sec, 90);
        assert_eq!(s.ghost_timer_started_at, Some(base_now));
    });

    // Second pilot blocked by HackingGameAlreadyInProgress on site container.
    let mut peer_sess = sess.clone();
    peer_sess.character_id = Some(peer_id);
    assert_eq!(
        call_hacking(&ctx, &mut peer_sess, "StartNewGameInstance", &[i(ghost_can_id)])
            .await
            .unwrap_err()
            .code,
        "HackingGameAlreadyInProgress"
    );

    // Mid-game Ghost timer reaches exact 90s boundary (now >= st + 90) -> ClickedOnTile detonates & fails.
    with_pve_exploration_reg(&ctx, |reg| {
        if let Some(s) = reg.sites_by_system.get_mut(&sys_id).and_then(|v| v.first_mut()) {
            s.ghost_timer_started_at = Some(base_now.saturating_sub(90));
        }
        let g = reg.hacking_games.get_mut(&(char_id, ghost_can_id)).unwrap();
        g.tiles[1].revealed = true;
    });
    let exp_click = call_hacking(&ctx, &mut sess, "ClickedOnTile", &[i(ghost_can_id), i(1)])
        .await
        .unwrap();
    assert_eq!(exp_click.dict_get("status"), Some(&PyRep::String("Failed".into())));
    assert_eq!(exp_click.dict_get("ghostDetonated"), Some(&PyRep::Bool(true)));

    // Already detonated ghost site rejects new StartNewGameInstance with HackingGameAlreadyFinished.
    with_pve_exploration_reg(&ctx, |reg| {
        if let Some(s) = reg.sites_by_system.get_mut(&sys_id).and_then(|v| v.first_mut()) {
            s.ghost_detonated = true;
            s.ghost_timer_started_at = None;
        }
    });
    assert_eq!(
        call_hacking(&ctx, &mut sess, "StartNewGameInstance", &[i(ghost_can_id)])
            .await
            .unwrap_err()
            .code,
        "HackingGameAlreadyFinished"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn click_blocking_self_repair_win_stats_structure_vulns_and_failures() {
    let sys_id: SolarSystemId = 30_000_142;
    let (char_id, corp_id, enemy_corp_id, ship_id) =
        (90_050_501, 98_050_501, 98_050_502, 1_050_000_501);
    let (data_can_id, sleeper_0, sleeper_1, sleeper_2, struct_id) = (
        1_050_950_001,
        1_050_950_010,
        1_050_950_011,
        1_050_950_012,
        1_050_850_001,
    );

    let sde = hacking_sde();
    let db = Arc::new(WorldRepository::new_empty());
    db.provision_gm_account("hk_combat", "pw", char_id, "CombatHacker");
    db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
        }
        snap.structure_vulnerability_windows.insert(struct_id, (14, 3));
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35_832,
                owner_corp_id: enemy_corp_id,
                solar_system_id: sys_id,
                name: "Raitaru".into(),
                position: [10_500.0, 0.0, 0.0],
                state: "Online".into(),
                ..Default::default()
            },
        );
        for (id, tid, loc, flag) in [
            (ship_id, 587, sys_id as i64, InventoryFlag::None),
            (ship_id + 1, 22_586, ship_id, InventoryFlag::MedSlot0),
        ] {
            snap.items.insert(
                id,
                InventoryItemRecord {
                    item_id: id,
                    type_id: tid,
                    owner_id: char_id,
                    location_id: loc,
                    flag,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        }
    });

    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_776_400_000)));
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 587,
            position: DVec3::new(10_500.0, 0.0, 0.0),
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
        reg.sites_by_system.insert(
            sys_id,
            vec![
                PveSiteInstance {
                    site_id: "DATA_COMBAT_1".into(),
                    dungeon_id: 4201,
                    dungeon_name: "Local Data Bank".into(),
                    solar_system_id: sys_id,
                    site_category: "Data".into(),
                    position: [10_500.0, 0.0, 0.0],
                    hackable_can_ids: vec![data_can_id],
                    ..Default::default()
                },
                PveSiteInstance {
                    site_id: "SLEEPER_ALL".into(),
                    dungeon_id: 4202,
                    dungeon_name: "Limited Sleeper Cache".into(),
                    solar_system_id: sys_id,
                    site_category: "SleeperCache".into(),
                    position: [10_500.0, 0.0, 0.0],
                    hackable_can_ids: vec![sleeper_0, sleeper_1, sleeper_2],
                    sleeper_gas_cloud_active: true,
                    ..Default::default()
                },
            ],
        );
    });

    let mut sess = SessionState::new(605);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);

    // ClickedOnTile without active game -> NoActiveHackingGame
    assert_eq!(
        call_hacking(&ctx, &mut sess, "ClickedOnTile", &[i(1)]).await.unwrap_err().code,
        "NoActiveHackingGame"
    );

    call_hacking(&ctx, &mut sess, "StartNewGameInstance", &[i(data_can_id)]).await.unwrap();
    with_pve_exploration_reg(&ctx, |reg| {
        let g = reg.hacking_games.get_mut(&(char_id, data_can_id)).unwrap();
        for t in &mut g.tiles {
            t.revealed = false;
            t.kind = HackingNodeKind::Empty;
            t.coherence = 0;
            t.strength = 0;
        }
        // Reveal tile 0 (Firewall, coherence 20 -> blocks tile 1 & 6), tile 1 unrevealed, tile 2 unrevealed.
        g.tiles[0].revealed = true;
        g.tiles[0].kind = HackingNodeKind::Firewall;
        g.tiles[0].coherence = 20;
        g.tiles[0].strength = 5;
        g.tiles[1].kind = HackingNodeKind::Firewall;
        g.tiles[1].coherence = 50;
        g.tiles[1].strength = 999; // Will kill virus on click
    });

    // Out-of-bounds tile index (24 or 100) -> InvalidTileIndex; non-adjacent tile 8 -> TileNotAccessible.
    assert_eq!(
        call_hacking(&ctx, &mut sess, "ClickedOnTile", &[i(data_can_id), i(24)])
            .await
            .unwrap_err()
            .code,
        "InvalidTileIndex"
    );
    assert_eq!(
        call_hacking(&ctx, &mut sess, "ClickedOnTile", &[i(data_can_id), i(8)])
            .await
            .unwrap_err()
            .code,
        "TileNotAccessible"
    );
    // Tile 1 is adjacent ONLY to blocking tile 0 (coherence > 0) -> TileNotAccessible!
    assert_eq!(
        call_hacking(&ctx, &mut sess, "ClickedOnTile", &[i(data_can_id), i(1)])
            .await
            .unwrap_err()
            .code,
        "TileNotAccessible"
    );

    // Clear blocker on tile 0 and also verify SelfRepair heals & decrements.
    with_pve_exploration_reg(&ctx, |reg| {
        let g = reg.hacking_games.get_mut(&(char_id, data_can_id)).unwrap();
        g.tiles[0].coherence = 0;
        g.tiles[2].kind = HackingNodeKind::Empty;
        g.tiles[2].coherence = 0;
        g.virus_coherence = 20;
        reg.self_repair_turns.insert((char_id, data_can_id), 1);
    });
    // values hides unrevealed tile 1 & reveals tile 0 with distanceIndicator.
    let vals = call_hacking(&ctx, &mut sess, "values", &[i(data_can_id)]).await.unwrap();
    if let PyRep::List(items) = vals {
        assert_eq!(items.len(), 24);
        assert_eq!(items[0].dict_get("revealed"), Some(&PyRep::Bool(true)));
        assert!(items[0].dict_get("distanceIndicator").is_some());
        assert_eq!(items[1].dict_get("revealed"), Some(&PyRep::Bool(false)));
        assert_eq!(items[1].dict_get("nodeType"), Some(&PyRep::String("Unrevealed".into())));
        assert_eq!(items[1].dict_get("coherence"), Some(&i(0)));
        assert!(items[1].dict_get("distanceIndicator").is_none());
    } else {
        panic!("expected PyRep::List from values");
    }

    // Reveal tile 1 via normal setup so we can check 1st failure (no explosion) & 2nd failure (explosion).
    with_pve_exploration_reg(&ctx, |reg| {
        let g = reg.hacking_games.get_mut(&(char_id, data_can_id)).unwrap();
        g.tiles[1].revealed = true;
    });
    let fail1 = call_hacking(&ctx, &mut sess, "ClickedOnTile", &[i(data_can_id), i(1)])
        .await
        .unwrap();
    assert_eq!(fail1.dict_get("status"), Some(&PyRep::String("Failed".into())));
    assert_eq!(fail1.dict_get("containerExploded"), Some(&PyRep::Bool(false)));
    assert!(with_pve_exploration_reg(&ctx, |reg| reg.self_repair_turns.is_empty()));

    // Clicking finished game returns HackingGameAlreadyFinished.
    assert_eq!(
        call_hacking(&ctx, &mut sess, "ClickedOnTile", &[i(data_can_id), i(1)])
            .await
            .unwrap_err()
            .code,
        "HackingGameAlreadyFinished"
    );

    // 2nd attempt ends in 2nd failure -> explosion!
    call_hacking(&ctx, &mut sess, "StartNewGameInstance", &[i(data_can_id)]).await.unwrap();
    with_pve_exploration_reg(&ctx, |reg| {
        let g = reg.hacking_games.get_mut(&(char_id, data_can_id)).unwrap();
        g.tiles[1].revealed = true;
        g.tiles[1].kind = HackingNodeKind::Firewall;
        g.tiles[1].coherence = 50;
        g.tiles[1].strength = 999;
    });
    let fail2 = call_hacking(&ctx, &mut sess, "ClickedOnTile", &[i(data_can_id), i(1)])
        .await
        .unwrap();
    assert_eq!(fail2.dict_get("status"), Some(&PyRep::String("Failed".into())));
    assert_eq!(fail2.dict_get("containerExploded"), Some(&PyRep::Bool(true)));
    assert_eq!(fail2.dict_get("containerDestroyed"), Some(&PyRep::Bool(true)));

    // Sleeper node 0 (rift) and node 2 (gas cloud) + hacking_wins increment + PyRep::List single-element arg.
    for (can, expect_rift, expect_gas) in [(sleeper_0, true, true), (sleeper_2, true, false)] {
        call_hacking(&ctx, &mut sess, "StartNewGameInstance", &[i(can)]).await.unwrap();
        with_pve_exploration_reg(&ctx, |reg| {
            let g = reg.hacking_games.get_mut(&(char_id, can)).unwrap();
            g.tiles[1].revealed = true;
            g.tiles[1].kind = HackingNodeKind::SystemCore;
            g.tiles[1].coherence = 1;
            g.tiles[1].strength = 1;
        });
        let win = call_hacking(
            &ctx,
            &mut sess,
            "ClickedOnTile",
            &[i(can), PyRep::List(vec![i(1)])],
        )
        .await
        .unwrap();
        assert_eq!(win.dict_get("status"), Some(&PyRep::String("Won".into())));
        assert_eq!(win.dict_get("sleeperPuzzleSolved"), Some(&PyRep::Bool(true)));
        with_pve_exploration_reg(&ctx, |reg| {
            let s = &reg.sites_by_system[&sys_id][1];
            assert_eq!(s.sleeper_rift_stabilized, expect_rift);
            assert_eq!(s.sleeper_gas_cloud_active, expect_gas);
        });
    }
    assert_eq!(
        ctx.db.with_snapshot(|s| s.seasons_and_campaigns.hacking_wins.get(&char_id).copied()),
        Some(2)
    );

    // Winning against an enemy structure returns reinforceHour / reinforceWeekday and increments hacking_wins.
    call_hacking(&ctx, &mut sess, "StartNewGameInstance", &[i(struct_id)]).await.unwrap();
    with_pve_exploration_reg(&ctx, |reg| {
        let g = reg.hacking_games.get_mut(&(char_id, struct_id)).unwrap();
        g.tiles[1].revealed = true;
        g.tiles[1].kind = HackingNodeKind::SystemCore;
        g.tiles[1].coherence = 1;
        g.tiles[1].strength = 1;
    });
    let st_win = call_hacking(&ctx, &mut sess, "ClickedOnTile", &[i(struct_id), i(1)])
        .await
        .unwrap();
    assert_eq!(st_win.dict_get("reinforceHour"), Some(&i(14)));
    assert_eq!(st_win.dict_get("reinforceWeekday"), Some(&i(3)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn quit_states_and_utility_error_and_redaction_paths() {
    let sys_id: SolarSystemId = 30_000_142;
    let (char_id, corp_id, enemy_corp_id, ship_id) =
        (90_050_601, 98_050_601, 98_050_602, 1_050_000_601);
    let (struct_id, ghost_can_id) = (1_050_860_001, 1_050_960_001);

    let sde = hacking_sde();
    let db = Arc::new(WorldRepository::new_empty());
    db.provision_gm_account("hk_util_err", "pw", char_id, "UtilErr");
    db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
        }
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35_832,
                owner_corp_id: enemy_corp_id,
                solar_system_id: sys_id,
                name: "Fortizar".into(),
                position: [10_500.0, 0.0, 0.0],
                state: "Online".into(),
                ..Default::default()
            },
        );
        for (id, tid, loc, flag) in [
            (ship_id, 587, sys_id as i64, InventoryFlag::None),
            (ship_id + 1, 22_586, ship_id, InventoryFlag::MedSlot0),
        ] {
            snap.items.insert(
                id,
                InventoryItemRecord {
                    item_id: id,
                    type_id: tid,
                    owner_id: char_id,
                    location_id: loc,
                    flag,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        }
    });

    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_776_500_000)));
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 587,
            position: DVec3::new(10_500.0, 0.0, 0.0),
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
        reg.sites_by_system.insert(
            sys_id,
            vec![PveSiteInstance {
                site_id: "GHOST_QUIT".into(),
                dungeon_id: 3099,
                dungeon_name: "Ghost Quit Test".into(),
                solar_system_id: sys_id,
                site_category: "Ghost".into(),
                position: [10_500.0, 0.0, 0.0],
                hackable_can_ids: vec![ghost_can_id],
                ghost_detonate_after_sec: 120,
                ..Default::default()
            }],
        );
    });

    let mut sess = SessionState::new(606);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);

    // No active game errors across Quit / GetMyHackProgress / UsedUtilityElement*
    assert_eq!(
        call_hacking(&ctx, &mut sess, "QuitHackingAttempt", &[]).await.unwrap_err().code,
        "NoActiveHackingGame"
    );
    assert_eq!(
        call_hacking(&ctx, &mut sess, "QuitHackingAttempt", &[i(struct_id)]).await.unwrap_err().code,
        "NoActiveHackingGame"
    );
    let prog0 = call_hacking(&ctx, &mut sess, "GetMyHackProgress", &[]).await.unwrap();
    assert_eq!(prog0.dict_get("containerID"), Some(&i(0)));
    assert_eq!(prog0.dict_get("status"), Some(&PyRep::String("NotStarted".into())));
    assert_eq!(call_hacking(&ctx, &mut sess, "values", &[i(struct_id)]).await.unwrap(), PyRep::List(vec![]));
    assert_eq!(
        call_hacking(&ctx, &mut sess, "UsedUtilityElement", &[i(0), i(0)]).await.unwrap_err().code,
        "NoActiveHackingGame"
    );
    assert_eq!(
        call_hacking(&ctx, &mut sess, "UsedUtilityElementOnVirus", &[i(0)]).await.unwrap_err().code,
        "TargetNotFound"
    );
    assert_eq!(
        call_hacking(&ctx, &mut sess, "UsedUtilityElementOnVirus", &[]).await.unwrap_err().code,
        "TargetNotFound"
    );

    call_hacking(&ctx, &mut sess, "StartNewGameInstance", &[i(struct_id)]).await.unwrap();
    with_pve_exploration_reg(&ctx, |reg| {
        let g = reg.hacking_games.get_mut(&(char_id, struct_id)).unwrap();
        for t in &mut g.tiles {
            t.revealed = false;
            t.coherence = 0;
        }
        g.utilities = vec![
            Some(HackingUtilityKind::KernelRot),
            Some(HackingUtilityKind::KernelRot),
            Some(HackingUtilityKind::SecondaryVector),
            Some(HackingUtilityKind::SelfRepair),
        ];
    });

    // Negative / out-of-bounds / unrevealed-target checks on UsedUtilityElementOnVirus & UsedUtilityElement
    assert_eq!(
        call_hacking(&ctx, &mut sess, "UsedUtilityElementOnVirus", &[i(struct_id), i(0), i(-1)])
            .await
            .unwrap_err()
            .code,
        "InvalidTileIndex"
    );
    assert_eq!(
        call_hacking(&ctx, &mut sess, "UsedUtilityElementOnVirus", &[i(struct_id), i(0), i(99)])
            .await
            .unwrap_err()
            .code,
        "InvalidTileIndex"
    );
    assert_eq!(
        call_hacking(&ctx, &mut sess, "UsedUtilityElementOnVirus", &[i(struct_id), i(0)])
            .await
            .unwrap_err()
            .code,
        "TargetTileNotRevealed"
    );
    assert_eq!(
        call_hacking(&ctx, &mut sess, "UsedUtilityElement", &[i(struct_id), i(0), i(99)])
            .await
            .unwrap_err()
            .code,
        "InvalidTileIndex"
    );
    assert_eq!(
        call_hacking(&ctx, &mut sess, "UsedUtilityElement", &[i(struct_id), i(9), i(0)])
            .await
            .unwrap_err()
            .code,
        "UtilitySlotEmpty"
    );

    // Reveal tile 3 with coherence 1 (KernelRot clamps .max(1) -> 1) and check "active" distinction.
    with_pve_exploration_reg(&ctx, |reg| {
        let g = reg.hacking_games.get_mut(&(char_id, struct_id)).unwrap();
        g.tiles[3].revealed = true;
        g.tiles[3].coherence = 1;
        g.status = "active".into();
    });
    assert!(
        call_hacking(&ctx, &mut sess, "UsedUtilityElementOnVirus", &[i(0)])
            .await
            .is_ok()
    );
    assert_eq!(
        with_pve_exploration_reg(&ctx, |reg| reg.hacking_games[&(char_id, struct_id)].tiles[3].coherence),
        1
    );
    // UsedUtilityElement requires strictly status == "InProgress" ("active" rejected).
    assert_eq!(
        call_hacking(&ctx, &mut sess, "UsedUtilityElement", &[i(struct_id), i(1), i(3)])
            .await
            .unwrap_err()
            .code,
        "HackingGameAlreadyFinished"
    );

    // Restore status = "InProgress", verify QuitHackingAttempt increments structure_char_failures once.
    with_pve_exploration_reg(&ctx, |reg| {
        let g = reg.hacking_games.get_mut(&(char_id, struct_id)).unwrap();
        g.status = "InProgress".into();
    });
    assert_eq!(
        call_hacking(&ctx, &mut sess, "QuitHackingAttempt", &[i(struct_id)]).await.unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        with_pve_exploration_reg(&ctx, |reg| reg.structure_char_failures.get(&(char_id, struct_id)).copied()),
        Some(1)
    );

    // Quitting a "Won" structure game does NOT increment structure_char_failures further.
    call_hacking(&ctx, &mut sess, "StartNewGameInstance", &[i(struct_id)]).await.unwrap();
    with_pve_exploration_reg(&ctx, |reg| {
        reg.hacking_games.get_mut(&(char_id, struct_id)).unwrap().status = "Won".into();
    });
    call_hacking(&ctx, &mut sess, "QuitHackingAttempt", &[i(struct_id)]).await.unwrap();
    assert_eq!(
        with_pve_exploration_reg(&ctx, |reg| reg.structure_char_failures.get(&(char_id, struct_id)).copied()),
        Some(1)
    );

    // Quitting an InProgress Ghost game marks hacked and triggers detonation.
    call_hacking(&ctx, &mut sess, "StartNewGameInstance", &[i(ghost_can_id)]).await.unwrap();
    assert_eq!(
        call_hacking(&ctx, &mut sess, "QuitHackingAttempt", &[i(ghost_can_id)]).await.unwrap(),
        PyRep::Bool(true)
    );
    assert!(with_pve_exploration_reg(&ctx, |reg| {
        reg.hacked_containers.contains(&(sys_id, ghost_can_id))
            && reg.sites_by_system[&sys_id][0].ghost_detonated
    }));
}

#[tokio::test]
async fn start_new_game_structure_failures_cleared_site_ghost_timer_math_and_in_progress_mutants() {
    let sde = hacking_sde();
    let sys_id = 30_000_144_u32;
    let char_id = 90_610_001_i64;
    let peer_id = 90_610_002_i64;
    let ship_id = 91_610_001_i64;
    let struct_id = 1_000_916_100_001_i64;
    let cleared_can = 1_890_610_001_i64;
    let data_can_ok = 1_890_610_002_i64;
    let data_can_hacked = 1_890_610_003_i64;
    let data_can_fail1 = 1_890_610_004_i64;
    let data_can_fail2 = 1_890_610_005_i64;
    let ghost_fresh_can = 1_890_610_006_i64;
    let ghost_det_can = 1_890_610_007_i64;
    let ghost_not_exp_can = 1_890_610_008_i64;
    let ghost_exact_exp_can = 1_890_610_009_i64;

    let db = Arc::new(WorldRepository::with_demo_seed());
    db.with_mut_snapshot(|snap| {
        let base_ch = snap.characters.values().next().unwrap().clone();
        for (cid, ship) in [(char_id, ship_id), (peer_id, 91_610_002_i64)] {
            let mut ch = base_ch.clone();
            ch.character_id = cid;
            ch.corporation_id = 98_610_001;
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship;
            snap.characters.insert(cid, ch);
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        for (idx, (tid, flag)) in [
            (22_586_u32, InventoryFlag::MedSlot0),
            (22_588_u32, InventoryFlag::MedSlot1),
        ]
        .into_iter()
        .enumerate()
        {
            snap.items.insert(
                91_610_100 + idx as i64,
                InventoryItemRecord {
                    item_id: 91_610_100 + idx as i64,
                    type_id: tid,
                    owner_id: char_id,
                    location_id: ship_id,
                    flag,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        }
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35_832,
                owner_corp_id: 98_610_999,
                solar_system_id: sys_id,
                name: "Enemy Upwell".into(),
                position: [20_000.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );
    });

    let ctx = ServerContext::with_sde_and_repository(sde, db);
    // Use small unix timestamp `190` so `st (100) + dur (90) == 190` differs from both `100 - 90 = 10` and `100 * 90 = 9000`
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(190)));
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 587,
            position: DVec3::new(20_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 200.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 30.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await
        .unwrap();

    let pos = [20_000.0, 0.0, 0.0];
    with_pve_exploration_reg(&ctx, |reg| {
        reg.seeded_systems.insert(sys_id, true);
        reg.sites_by_system.insert(
            sys_id,
            vec![
                // Cleared site must never match cleared_can even alongside active sites
                PveSiteInstance {
                    site_id: "CLEARED_SITE".into(),
                    dungeon_id: 3001,
                    dungeon_name: "Cleared Data".into(),
                    solar_system_id: sys_id,
                    site_category: "Data".into(),
                    position: pos,
                    hackable_can_ids: vec![cleared_can],
                    cleared: true,
                    ..Default::default()
                },
                PveSiteInstance {
                    site_id: "DATA_SITE".into(),
                    dungeon_id: 3002,
                    dungeon_name: "Active Data".into(),
                    solar_system_id: sys_id,
                    site_category: "Data".into(),
                    position: pos,
                    hackable_can_ids: vec![data_can_ok, data_can_hacked, data_can_fail1, data_can_fail2],
                    // Pre-set old ghost timestamp on Data site to prove site_category == "Ghost" guard holds
                    ghost_timer_started_at: Some(50),
                    ghost_detonate_after_sec: 90,
                    ..Default::default()
                },
                PveSiteInstance {
                    site_id: "GHOST_FRESH".into(),
                    dungeon_id: 3003,
                    dungeon_name: "Ghost Fresh".into(),
                    solar_system_id: sys_id,
                    site_category: "Ghost".into(),
                    position: pos,
                    hackable_can_ids: vec![ghost_fresh_can],
                    ghost_timer_started_at: None,
                    ghost_detonate_after_sec: 90,
                    ..Default::default()
                },
                PveSiteInstance {
                    site_id: "GHOST_DET".into(),
                    dungeon_id: 3004,
                    dungeon_name: "Ghost Detonated".into(),
                    solar_system_id: sys_id,
                    site_category: "Ghost".into(),
                    position: pos,
                    hackable_can_ids: vec![ghost_det_can],
                    ghost_detonated: true,
                    ghost_timer_started_at: Some(10),
                    ghost_detonate_after_sec: 90,
                    ..Default::default()
                },
                // st = 150, dur = 90, now = 190 -> st - 90 (60) <= 190 but st + 90 (240) > 190 (NOT expired!)
                PveSiteInstance {
                    site_id: "GHOST_NOT_EXP".into(),
                    dungeon_id: 3005,
                    dungeon_name: "Ghost Running".into(),
                    solar_system_id: sys_id,
                    site_category: "Ghost".into(),
                    position: pos,
                    hackable_can_ids: vec![ghost_not_exp_can],
                    ghost_timer_started_at: Some(150),
                    ghost_detonate_after_sec: 90,
                    ..Default::default()
                },
                // st = 100, dur = 90, now = 190 -> st + 90 == 190 (EXACT expiry, whereas st * 90 = 9000 > 190!)
                PveSiteInstance {
                    site_id: "GHOST_EXACT_EXP".into(),
                    dungeon_id: 3006,
                    dungeon_name: "Ghost Exact".into(),
                    solar_system_id: sys_id,
                    site_category: "Ghost".into(),
                    position: pos,
                    hackable_can_ids: vec![ghost_exact_exp_can],
                    ghost_timer_started_at: Some(100),
                    ghost_detonate_after_sec: 90,
                    ..Default::default()
                },
            ],
        );
        reg.hacked_containers.insert((sys_id, data_can_hacked));
        reg.hacked_containers.insert((sys_id, ghost_not_exp_can));
        reg.hacked_containers.insert((sys_id, ghost_exact_exp_can));
        reg.failure_counts.insert((sys_id, data_can_fail1), 1);
        reg.failure_counts.insert((sys_id, data_can_fail2), 2);
        // Active InProgress game by peer on data_can_fail1 vs Won game by peer on data_can_ok
        reg.hacking_games.insert(
            (peer_id, 1_890_610_999),
            crate::services::pve_exploration::HackingGameSession {
                game_id: 999_001,
                character_id: peer_id,
                solar_system_id: sys_id,
                container_id: 1_890_610_999,
                site_id: "OTHER".into(),
                is_relic: false,
                is_ghost_site: false,
                is_sleeper_cache_puzzle: false,
                virus_coherence: 60,
                max_virus_coherence: 60,
                base_virus_strength: 30,
                polymorphic_shield_charges: 0,
                secondary_vector_target: None,
                utilities: Vec::new(),
                tiles: Vec::new(),
                status: "InProgress".into(),
            },
        );
        reg.hacking_games.insert(
            (peer_id, data_can_ok),
            crate::services::pve_exploration::HackingGameSession {
                game_id: 999_002,
                character_id: peer_id,
                solar_system_id: sys_id,
                container_id: data_can_ok,
                site_id: "DATA_SITE".into(),
                is_relic: false,
                is_ghost_site: false,
                is_sleeper_cache_puzzle: false,
                virus_coherence: 60,
                max_virus_coherence: 60,
                base_virus_strength: 30,
                polymorphic_shield_charges: 0,
                secondary_vector_target: None,
                utilities: Vec::new(),
                tiles: Vec::new(),
                status: "Won".into(),
            },
        );
    });

    let mut sess = SessionState::new(610);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(98_610_001);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);

    // Structure failures == 1 succeeds; failures == 2 rejects (kills L441 `>= -> <`)
    with_pve_exploration_reg(&ctx, |reg| {
        reg.structure_char_failures.insert((char_id, struct_id), 1);
    });
    let st_ok = call_hacking(&ctx, &mut sess, "StartNewGameInstance", &[i(struct_id)])
        .await
        .unwrap();
    assert_eq!(st_ok.dict_get("status"), Some(&PyRep::String("InProgress".into())));
    with_pve_exploration_reg(&ctx, |reg| {
        reg.hacking_games.remove(&(char_id, struct_id));
        reg.structure_char_failures.insert((char_id, struct_id), 2);
    });
    assert_eq!(
        call_hacking(&ctx, &mut sess, "StartNewGameInstance", &[i(struct_id)])
            .await
            .unwrap_err()
            .code,
        "HackingGameAlreadyFinished"
    );

    // Cleared site rejects TargetNotFound even when active sibling sites exist (kills L447 `&& -> ||`, `!`)
    assert_eq!(
        call_hacking(&ctx, &mut sess, "StartNewGameInstance", &[i(cleared_can)])
            .await
            .unwrap_err()
            .code,
        "TargetNotFound"
    );

    // Non-Ghost hacked container fails even with stale ghost timer; Data failure_counts==1 succeeds while ==2 fails
    assert_eq!(
        call_hacking(&ctx, &mut sess, "StartNewGameInstance", &[i(data_can_hacked)])
            .await
            .unwrap_err()
            .code,
        "HackingGameAlreadyFinished"
    );
    assert_eq!(
        call_hacking(&ctx, &mut sess, "StartNewGameInstance", &[i(data_can_fail2)])
            .await
            .unwrap_err()
            .code,
        "HackingGameAlreadyFinished"
    );
    let f1_ok = call_hacking(&ctx, &mut sess, "StartNewGameInstance", &[i(data_can_fail1)])
        .await
        .unwrap();
    assert_eq!(f1_ok.dict_get("status"), Some(&PyRep::String("InProgress".into())));

    // Peer's Won game on data_can_ok + peer's InProgress game on other can must NOT block starting data_can_ok (kills L460)
    let ok_start = call_hacking(&ctx, &mut sess, "StartNewGameInstance", &[i(data_can_ok)])
        .await
        .unwrap();
    assert_eq!(ok_start.dict_get("status"), Some(&PyRep::String("InProgress".into())));

    // Fresh non-detonated Ghost site starts cleanly (kills L453 `&& -> ||`)
    let gf_start = call_hacking(&ctx, &mut sess, "StartNewGameInstance", &[i(ghost_fresh_can)])
        .await
        .unwrap();
    assert_eq!(gf_start.dict_get("status"), Some(&PyRep::String("InProgress".into())));

    // Detonated Ghost site rejects even with old timer & unhacked status (kills L449 `!`, L453 `== -> !=`)
    assert_eq!(
        call_hacking(&ctx, &mut sess, "StartNewGameInstance", &[i(ghost_det_can)])
            .await
            .unwrap_err()
            .code,
        "HackingGameAlreadyFinished"
    );

    // Ghost site at st=150, now=190 (elapsed 40 < 90) NOT expired -> still blocked by hacked_containers (kills L450 `&& -> ||`, L451 `+ -> -`)
    assert_eq!(
        call_hacking(&ctx, &mut sess, "StartNewGameInstance", &[i(ghost_not_exp_can)])
            .await
            .unwrap_err()
            .code,
        "HackingGameAlreadyFinished"
    );

    // Fresh Ghost site that IS already hacked with timer==None fails HackingGameAlreadyFinished (kills L449 `&& -> ||`)
    let ghost_hacked_no_timer = 1_890_610_010_i64;
    with_pve_exploration_reg(&ctx, |reg| {
        if let Some(list) = reg.sites_by_system.get_mut(&sys_id) {
            list.push(PveSiteInstance {
                site_id: "GHOST_NO_TIMER".into(),
                dungeon_id: 3007,
                dungeon_name: "Ghost No Timer".into(),
                solar_system_id: sys_id,
                site_category: "Ghost".into(),
                position: pos,
                hackable_can_ids: vec![ghost_hacked_no_timer],
                ghost_timer_started_at: None,
                ghost_detonate_after_sec: 90,
                ..Default::default()
            });
        }
        reg.hacked_containers.insert((sys_id, ghost_hacked_no_timer));
    });
    assert_eq!(
        call_hacking(&ctx, &mut sess, "StartNewGameInstance", &[i(ghost_hacked_no_timer)])
            .await
            .unwrap_err()
            .code,
        "HackingGameAlreadyFinished"
    );

    // Exact expiry boundary st=100, dur=90, now=190 bypasses finished guard & detonates instead of st*90=9000 (kills L451 `>= -> <`, `+ -> *`, L452 `!`)
    let g_exp = call_hacking(&ctx, &mut sess, "StartNewGameInstance", &[i(ghost_exact_exp_can)])
        .await
        .unwrap();
    assert_eq!(g_exp.dict_get("status"), Some(&PyRep::String("GhostSiteDetonated".into())));
}
