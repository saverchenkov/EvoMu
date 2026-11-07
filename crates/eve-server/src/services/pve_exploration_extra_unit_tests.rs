//! Extra unit and mutation-killing tests for `services::pve_exploration`
//! covering registry tuple-map serde, NPC destruction core (wormhole wave triggers,
//! DED room gate rules, Data/Relic unhacked can guards, expedition steps),
//! fleet/character destruction notifications, wave advancement guards, and RPC handlers.

#![forbid(unsafe_code)]

use super::pve_exploration::*;
use crate::router::ServerContext;
use eve_db::clock::VirtualServerClock;
use eve_db::{
    CosmicAnomalyRecord, CosmicSignatureRecord, InventoryFlag, InventoryItemRecord,
    WorldRepository,
};
use eve_machonet::SessionState;
use eve_sde::{SdeRegistry, SolarSystemId};
use std::sync::{Arc, OnceLock};

pub(super) fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

pub(super) fn make_ctx_and_session(
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
