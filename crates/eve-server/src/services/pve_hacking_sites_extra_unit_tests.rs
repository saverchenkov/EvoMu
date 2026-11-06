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
