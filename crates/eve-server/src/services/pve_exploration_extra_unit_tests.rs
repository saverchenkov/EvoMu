//! Extra unit and mutation-killing tests for `services::pve_exploration`
//! covering registry tuple-map serde, NPC destruction core (wormhole wave triggers,
//! DED room gate rules, Data/Relic unhacked can guards, expedition steps),
//! fleet/character destruction notifications, wave advancement guards, and RPC handlers.

#![forbid(unsafe_code)]

use super::pve_exploration::*;
use crate::router::ServerContext;
use eve_db::clock::VirtualServerClock;
use eve_db::{
    CosmicAnomalyRecord, CosmicSignatureRecord, FleetRecord, InventoryFlag, InventoryItemRecord,
    WorldRepository,
};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{SdeRegistry, SolarSystemId};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn make_ctx_and_session(
    sys_id: SolarSystemId,
    char_id: i64,
    ship_id: i64,
    ship_type_id: u32,
) -> (ServerContext, SessionState) {
    let db = Arc::new(WorldRepository::new_empty());
    db.provision_gm_account(&format!("u_{char_id}"), "pw", char_id, "Pilot");
    db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: ship_type_id,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    let ctx = ServerContext::with_sde_and_repository(shared_sde(), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_776_000_000)));
    let mut sess = SessionState::new(901);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);
    (ctx, sess)
}

#[test]
fn registry_tuple_map_serde_seq_clock_and_category_lookup() {
    let (ctx, _) = make_ctx_and_session(30_000_142, 90_100_001, 100_100_001, 587);
    assert!(ctx_pve_key(&ctx) != 0);

    // Default now_sec fallback when current_now_sec == 0
    let mut empty_reg = PveExplorationRegistry::default();
    assert_eq!(now_sec(&empty_reg), 1_775_000_000);
    empty_reg.sim_clock_sec = 1_800_000_000;
    assert_eq!(now_sec(&empty_reg), 1_800_000_000);
    assert_eq!(next_pve_seq(&mut empty_reg), 1);
    assert_eq!(next_pve_seq(&mut empty_reg), 2);

    // Persist and reload tuple-keyed maps via owner_notes
    with_pve_exploration_reg(&ctx, |reg| {
        reg.failure_counts.insert((30_000_142, 5001), 2);
        reg.self_repair_turns.insert((90_100_001, 5001), 3);
        reg.structure_char_failures.insert((90_100_001, 7001), 1);
        reg.hacking_games.insert(
            (90_100_001, 5001),
            HackingGameSession {
                game_id: 42,
                character_id: 90_100_001,
                solar_system_id: 30_000_142,
                container_id: 5001,
                site_id: "DATA-1".into(),
                is_relic: false,
                is_ghost_site: true,
                is_sleeper_cache_puzzle: false,
                virus_coherence: 70,
                max_virus_coherence: 80,
                base_virus_strength: 25,
                polymorphic_shield_charges: 2,
                secondary_vector_target: Some((3, 2)),
                utilities: vec![
                    Some(HackingUtilityKind::SelfRepair),
                    Some(HackingUtilityKind::KernelRot),
                    None,
                ],
                tiles: vec![HackingTileState {
                    tile_index: 0,
                    revealed: true,
                    blocked_by_defensive: false,
                    kind: HackingNodeKind::SystemCore,
                    coherence: 50,
                    strength: 10,
                    utility_drop: Some(HackingUtilityKind::PolymorphicShield),
                }],
                status: "Active".into(),
            },
        );
        reg.sites_by_system.insert(
            30_000_142,
            vec![PveSiteInstance {
                site_id: "SITE-CAT-1".into(),
                dungeon_id: 777,
                site_category: "CombatSignature".into(),
                solar_system_id: 30_000_142,
                ..Default::default()
            }],
        );
    });

    with_pve_exploration_reg(&ctx, |reg| {
        assert_eq!(reg.failure_counts.get(&(30_000_142, 5001)), Some(&2));
        assert_eq!(reg.self_repair_turns.get(&(90_100_001, 5001)), Some(&3));
        assert_eq!(reg.structure_char_failures.get(&(90_100_001, 7001)), Some(&1));
        let g = reg.hacking_games.get(&(90_100_001, 5001)).expect("game persisted");
        assert_eq!(g.game_id, 42);
        assert_eq!(g.tiles[0].kind, HackingNodeKind::SystemCore);
    });

    assert_eq!(
        pve_site_category_and_dungeon_id(&ctx.db, ctx.now_unix(), 30_000_142, "SITE-CAT-1"),
        ("CombatSignature".to_string(), 777)
    );
    assert_eq!(
        pve_site_category_and_dungeon_id(&ctx.db, ctx.now_unix(), 30_000_142, "MISSING"),
        (String::new(), 0)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn npc_destruction_core_wormhole_wave_trigger_ded_gate_and_data_can_guards() {
    let wh_sys: SolarSystemId = 31_000_005;
    let char_id = 90_100_002;
    let (ctx, _) = make_ctx_and_session(wh_sys, char_id, 100_100_002, 587);

    // Seed a WormholeCombat site (2 waves, 2 NPCs in wave 1), a DED site (2 waves), and a Data site with unhacked can
    with_pve_exploration_reg(&ctx, |reg| {
        reg.sites_by_system.insert(
            wh_sys,
            vec![
                PveSiteInstance {
                    site_id: "WH-SITE-1".into(),
                    dungeon_id: 901,
                    dungeon_name: "Frontier Stronghold".into(),
                    solar_system_id: wh_sys,
                    site_category: "WormholeCombat".into(),
                    wormhole_class: Some(5),
                    position: [10_000.0, 0.0, 0.0],
                    current_wave: 1,
                    total_waves: 2,
                    active_npc_ball_ids: vec![800_001, 800_002],
                    ..Default::default()
                },
                PveSiteInstance {
                    site_id: "DED-SITE-1".into(),
                    dungeon_id: 902,
                    dungeon_name: "DED 4/10".into(),
                    solar_system_id: wh_sys,
                    site_category: "DED".into(),
                    ded_rating: Some(4),
                    position: [20_000.0, 0.0, 0.0],
                    current_wave: 1,
                    total_waves: 2,
                    active_npc_ball_ids: vec![800_010],
                    ..Default::default()
                },
                PveSiteInstance {
                    site_id: "DATA-SITE-1".into(),
                    dungeon_id: 903,
                    dungeon_name: "Central Data Site".into(),
                    solar_system_id: wh_sys,
                    site_category: "Data".into(),
                    position: [30_000.0, 0.0, 0.0],
                    current_wave: 1,
                    total_waves: 1,
                    active_npc_ball_ids: vec![800_020],
                    hackable_can_ids: vec![900_001],
                    ..Default::default()
                },
            ],
        );
    });
    ctx.db.with_mut_snapshot(|snap| {
        snap.cosmic_anomalies.entry(wh_sys).or_default().push(CosmicAnomalyRecord {
            anomaly_id: "WH-SITE-1".into(),
            solar_system_id: wh_sys,
            name: "Frontier Stronghold".into(),
            anomaly_type: "Combat".into(),
            position: [10_000.0, 0.0, 0.0],
            signal_strength: 1.0,
            ore_types: Vec::new(),
            remaining_volume: 0.0,
            created_unix: 1_776_000_000,
            lifetime_seconds: 86_400,
            cleared: false,
        });
        snap.cosmic_signatures.entry(wh_sys).or_default().push(CosmicSignatureRecord {
            signature_id: "DED-SITE-1".into(),
            solar_system_id: wh_sys,
            type_name: "DED 4/10".into(),
            sig_group: "Combat".into(),
            position: [20_000.0, 0.0, 0.0],
            signal_strength: 1.0,
            wormhole_type_id: None,
            destination_system_id: None,
            created_unix: 1_776_000_000,
            lifetime_seconds: 86_400,
            max_mass_kg: 0.0,
            remaining_mass_kg: 0.0,
            max_jump_mass_kg: 0.0,
            collapsed: false,
        });
    });

    let destruct = |bid: i64, drop_sleeper: bool| {
        handle_pve_site_npc_destruction_core(
            &ctx.systems,
            &ctx.db,
            &ctx.sde,
            &ctx.mechanic_rng,
            &ctx.combat_ephemeral,
            wh_sys,
            bid,
            char_id,
            drop_sleeper,
        )
    };

    // 1. Unknown ball returns (None, None, None)
    assert!(matches!(destruct(999_999, false).await, (None, None, None)));

    // 2. WormholeCombat: killing first active ball (800_001, pos == 0 = wave trigger) drops sleeper wreck
    // and immediately spawns wave 2 while keeping leftover ball 800_002!
    let wh_res = destruct(800_001, true).await;
    assert!(matches!(wh_res, (None, None, None)));
    let remaining_wh_balls = with_pve_exploration_reg(&ctx, |reg| {
        let s = reg.sites_by_system.get(&wh_sys).unwrap().iter().find(|s| s.site_id == "WH-SITE-1").unwrap();
        assert_eq!(s.current_wave, 2);
        assert!(s.active_npc_ball_ids.contains(&800_002));
        s.active_npc_ball_ids.clone()
    });

    // Kill all remaining balls in WH-SITE-1 -> completes C5 site, drops Blue Loot, spawns relay, marks anomaly cleared
    for (idx, bid) in remaining_wh_balls.iter().copied().enumerate() {
        let (cleared_id, _, _) = destruct(bid, false).await;
        if idx + 1 < remaining_wh_balls.len() {
            assert_eq!(cleared_id, None);
        } else {
            assert_eq!(cleared_id.as_deref(), Some("WH-SITE-1"));
        }
    }
    assert!(ctx.db.with_snapshot(|s| s.cosmic_anomalies[&wh_sys][0].cleared));

    // 3. DED site: clearing wave 1 of 2 does NOT auto-advance wave (waits for gate activation)
    let ded_w1 = destruct(800_010, false).await;
    assert!(matches!(ded_w1, (None, None, None)));
    with_pve_exploration_reg(&ctx, |reg| {
        let s = reg.sites_by_system.get_mut(&wh_sys).unwrap().iter_mut().find(|s| s.site_id == "DED-SITE-1").unwrap();
        assert_eq!(s.current_wave, 1);
        assert!(!s.cleared);
        // Move to final wave and give 1 overseer ball
        s.current_wave = 2;
        s.active_npc_ball_ids = vec![800_011];
    });
    let (ded_cleared, _, _) = destruct(800_011, false).await;
    assert_eq!(ded_cleared.as_deref(), Some("DED-SITE-1"));
    assert!(ctx.db.with_snapshot(|s| s.cosmic_signatures[&wh_sys][0].collapsed));

    // 4. Data site with unhacked can: killing last NPC does NOT mark site cleared until can is hacked
    let data_unhacked = destruct(800_020, false).await;
    assert!(matches!(data_unhacked, (None, None, None)));
    with_pve_exploration_reg(&ctx, |reg| {
        let s = reg.sites_by_system.get_mut(&wh_sys).unwrap().iter_mut().find(|s| s.site_id == "DATA-SITE-1").unwrap();
        assert!(!s.cleared);
        s.active_npc_ball_ids.push(800_021);
        reg.hacked_containers.insert((wh_sys, 900_001));
    });
    let (data_cleared, _, _) = destruct(800_021, false).await;
    assert_eq!(data_cleared.as_deref(), Some("DATA-SITE-1"));
}

#[test]
fn notify_pve_destruction_events_dispatches_fleet_and_escalation_packets() {
    let sys_id: SolarSystemId = 30_000_142;
    let killer_id = 90_100_010;
    let wingman_id = 90_100_011;
    let fleet_id = 77_001;
    let (ctx, _) = make_ctx_and_session(sys_id, killer_id, 100_100_010, 587);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&killer_id) {
            ch.fleet_id = Some(fleet_id);
        }
        snap.fleets.insert(
            fleet_id,
            FleetRecord {
                fleet_id,
                boss_id: killer_id,
                members: vec![killer_id, wingman_id],
                ..Default::default()
            },
        );
    });
    with_pve_exploration_reg(&ctx, |reg| {
        reg.sites_by_system.insert(
            sys_id,
            vec![PveSiteInstance {
                site_id: "ANOM-NOTIF-1".into(),
                dungeon_id: 4321,
                solar_system_id: sys_id,
                site_category: "CombatAnomaly".into(),
                ..Default::default()
            }],
        );
    });

    let esc = EscalatingPathEntry {
        instance_id: 55_001,
        character_id: killer_id,
        source_dungeon_id: 4321,
        expedition_dungeon_id: 4322,
        expedition_name: "Escalated Outpost".into(),
        destination_solar_system_id: sys_id,
        step_index: 1,
        total_steps: 4,
        journal_text: "Follow the trail".into(),
        created_at_sec: 1_776_000_000,
        expires_at_sec: 1_800_000_000,
        dungeons_entered: Vec::new(),
        journal_entry_template_id: 99,
    };

    notify_pve_destruction_events(
        &ctx.db,
        &ctx.pending_notifications,
        &ctx.notify_tx,
        killer_id,
        Some("ANOM-NOTIF-1".into()),
        Some(esc.clone()),
        Some(esc),
    );

    let queues = ctx.pending_notifications.read().unwrap();
    let killer_events: Vec<&str> = queues
        .get(&killer_id)
        .unwrap()
        .iter()
        .map(|n| n.event.as_str())
        .collect();
    assert!(killer_events.contains(&"OnCosmicAnomalyRemoved"));
    assert!(killer_events.contains(&"OnDungeonCompleted"));
    assert!(killer_events.contains(&"OnEscalatingPathChange"));
    assert!(killer_events.contains(&"OnEscalatingPathMessage"));
    assert!(killer_events.contains(&"OnFleetEscalatingPathMessage"));

    let wingman_events: Vec<&str> = queues
        .get(&wingman_id)
        .unwrap()
        .iter()
        .map(|n| n.event.as_str())
        .collect();
    assert!(wingman_events.contains(&"OnFleetEscalatingPathMessage"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn advance_site_wave_or_complete_and_exploration_rpc_edge_cases() {
    let sys_id: SolarSystemId = 30_000_142;
    let char_id = 90_100_020;
    let ship_id = 100_100_020;
    let (ctx, mut sess) = make_ctx_and_session(sys_id, char_id, ship_id, 587);
    let kw = HashMap::new();

    // 1. Docked guard
    sess.station_id = Some(60_003_760);
    assert_eq!(
        advance_site_wave_or_complete(&ctx, &mut sess, sys_id, "ANY")
            .await
            .unwrap_err()
            .code,
        "ShipDocked"
    );
    sess.station_id = None;

    // 2. Wrong system guard
    assert_eq!(
        advance_site_wave_or_complete(&ctx, &mut sess, 30_000_144, "ANY")
            .await
            .unwrap_err()
            .code,
        "NotInSiteSystem"
    );

    // 3. Site not found
    assert_eq!(
        advance_site_wave_or_complete(&ctx, &mut sess, sys_id, "NONEXISTENT-SITE")
            .await
            .unwrap_err()
            .code,
        "SiteNotFound"
    );

    // Seed custom sites for wave/completion & RPC tests
    let live_ball_id = 888_101;
    let gate_ball_id = 888_202;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle
        .spawn_ball(Ball {
            id: live_ball_id,
            type_id: 587,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 100.0,
            inertia: 1.0,
            mass: 1_000.0,
            radius: 20.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    with_pve_exploration_reg(&ctx, |reg| {
        reg.escalations_by_char.insert(
            char_id,
            vec![EscalatingPathEntry {
                instance_id: 701,
                character_id: char_id,
                source_dungeon_id: 100,
                expedition_dungeon_id: 5604,
                expedition_name: "Owned Expedition".into(),
                destination_solar_system_id: sys_id,
                step_index: 1,
                total_steps: 3,
                journal_text: "Step 1".into(),
                created_at_sec: 1_776_000_000,
                expires_at_sec: 1_900_000_000,
                dungeons_entered: Vec::new(),
                journal_entry_template_id: 1,
            }],
        );
        let sites = reg.sites_by_system.entry(sys_id).or_default();
        sites.push(PveSiteInstance {
            site_id: "CLEARED-1".into(),
            dungeon_id: 501,
            solar_system_id: sys_id,
            site_category: "CombatAnomaly".into(),
            cleared: true,
            ..Default::default()
        });
        sites.push(PveSiteInstance {
            site_id: "UNHACKED-GHOST".into(),
            dungeon_id: 502,
            solar_system_id: sys_id,
            site_category: "Ghost".into(),
            hackable_can_ids: vec![910_001],
            ..Default::default()
        });
        sites.push(PveSiteInstance {
            site_id: "LIVE-WAVE-1".into(),
            dungeon_id: 503,
            solar_system_id: sys_id,
            site_category: "CombatAnomaly".into(),
            current_wave: 1,
            total_waves: 2,
            active_npc_ball_ids: vec![live_ball_id],
            ..Default::default()
        });
        sites.push(PveSiteInstance {
            site_id: "SLEEPER-CACHE-1".into(),
            dungeon_id: 5604,
            dungeon_name: "Limited Sleeper Cache".into(),
            solar_system_id: sys_id,
            site_category: "SleeperCache".into(),
            sleeper_rift_stabilized: true,
            current_wave: 1,
            total_waves: 1,
            active_npc_ball_ids: vec![live_ball_id],
            ..Default::default()
        });
        sites.push(PveSiteInstance {
            site_id: "DED-GATE-SITE".into(),
            dungeon_id: 504,
            dungeon_name: "DED Complex".into(),
            solar_system_id: sys_id,
            site_category: "DED".into(),
            ded_rating: Some(5),
            gate_ball_id: Some(gate_ball_id),
            current_room: 1,
            total_rooms: 2,
            current_wave: 1,
            total_waves: 2,
            ..Default::default()
        });
        sites.push(PveSiteInstance {
            site_id: "EXP-OWNED".into(),
            dungeon_id: 505,
            dungeon_name: "Owned Expedition".into(),
            solar_system_id: sys_id,
            site_category: "Expedition".into(),
            expedition_instance_id: Some(701),
            probe_required: false,
            ..Default::default()
        });
        sites.push(PveSiteInstance {
            site_id: "EXP-FOREIGN".into(),
            dungeon_id: 506,
            dungeon_name: "Foreign Expedition".into(),
            solar_system_id: sys_id,
            site_category: "Expedition".into(),
            expedition_instance_id: Some(999),
            probe_required: false,
            ..Default::default()
        });
    });

    assert_eq!(
        advance_site_wave_or_complete(&ctx, &mut sess, sys_id, "CLEARED-1")
            .await
            .unwrap_err()
            .code,
        "SiteAlreadyCleared"
    );
    assert_eq!(
        advance_site_wave_or_complete(&ctx, &mut sess, sys_id, "UNHACKED-GHOST")
            .await
            .unwrap_err()
            .code,
        "SiteContainersUnhacked"
    );
    assert_eq!(
        advance_site_wave_or_complete(&ctx, &mut sess, sys_id, "LIVE-WAVE-1")
            .await
            .unwrap_err()
            .code,
        "WaveNpcsRemaining"
    );

    // Stabilized SleeperCache ignores live_ball_id and completes
    let sc_done = advance_site_wave_or_complete(&ctx, &mut sess, sys_id, "SLEEPER-CACHE-1")
        .await
        .expect("stabilized sleeper cache completes");
    assert_eq!(sc_done.dict_get("completed"), Some(&PyRep::Bool(true)));

    // Remove live_ball_id -> LIVE-WAVE-1 advances to wave 2 (completed = false)
    let _ = sys_handle.remove_ball(live_ball_id).await;
    let w2 = advance_site_wave_or_complete(&ctx, &mut sess, sys_id, "LIVE-WAVE-1")
        .await
        .expect("advances to wave 2");
    assert_eq!(w2.dict_get("currentWave"), Some(&PyRep::Integer(2)));
    assert_eq!(w2.dict_get("completed"), Some(&PyRep::Bool(false)));

    // GetCombatAnomalyInstances includes EXP-OWNED and excludes EXP-FOREIGN & CLEARED-1
    let PyRep::List(anom_list) = handle_pve_exploration(
        &ctx,
        &mut sess,
        "scanSvc",
        "GetCombatAnomalyInstances",
        &[PyRep::Integer(sys_id as i64)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap() else {
        panic!("expected list");
    };
    let site_ids: Vec<String> = anom_list
        .iter()
        .filter_map(|r| r.dict_get("siteID")?.as_str().map(str::to_string))
        .collect();
    assert!(site_ids.contains(&"EXP-OWNED".to_string()));
    assert!(!site_ids.contains(&"EXP-FOREIGN".to_string()));
    assert!(!site_ids.contains(&"CLEARED-1".to_string()));

    // GetDungeonShipRestrictions for SleeperCache dungeon IDs (Limited=5604, Standard=5609, Superior=5632) and by site_id
    for (did, expected_name) in [
        (5604, "Limited Sleeper Cache"),
        (5609, "Standard Sleeper Cache"),
        (5632, "Superior Sleeper Cache"),
    ] {
        let rep = handle_pve_exploration(&ctx, &mut sess, "keeper", "GetDungeonShipRestrictions", &[PyRep::Integer(did)], &kw)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(rep.dict_get("dungeonName").and_then(|v| v.as_str()), Some(expected_name));
    }
    let by_site = handle_pve_exploration(&ctx, &mut sess, "keeper", "GetDungeonShipRestrictions", &[PyRep::String("DED-GATE-SITE".into())], &kw)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(by_site.dict_get("dedRating"), Some(&PyRep::Integer(5)));

    // ActivateAccelerationGate with unknown integer gate returns None; with DED gate_ball_id resolves site
    assert!(handle_pve_exploration(&ctx, &mut sess, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(999_888_777)], &kw)
        .await
        .is_none());

    let gate_res = handle_pve_exploration(&ctx, &mut sess, "beyonce", "ActivateAccelerationGate", &[PyRep::Integer(gate_ball_id)], &kw)
        .await;
    assert!(gate_res.is_some());

    // GetDungeonObjectivesData missing -> DungeonNotFound
    assert_eq!(
        handle_pve_exploration(&ctx, &mut sess, "keeper", "GetDungeonObjectivesData", &[PyRep::String("NO-SUCH-DUNGEON".into())], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "DungeonNotFound"
    );

    // GetMyEscalatingPathDetails with extra args -> InvalidArguments
    assert!(handle_pve_exploration(&ctx, &mut sess, "scanSvc", "GetMyEscalatingPathDetails", &[PyRep::Integer(1)], &kw)
        .await
        .unwrap()
        .is_err());
}
