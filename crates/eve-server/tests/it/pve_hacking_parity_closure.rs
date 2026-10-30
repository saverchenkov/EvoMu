//! Integration tests for V24 Hacking Node Types & Combat and Hacking Failure Outcomes
//! (`feat:pve.hacking-node-types`, `feat:pve.hacking-failure-outcome`).

#![forbid(unsafe_code)]

use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::pve_exploration::{
    ensure_system_pve_sites_seeded, with_pve_exploration_reg, HackingNodeKind, HackingTileState,
    HackingUtilityKind,
};
use eve_server::services::pve_hacking_board::{
    build_hacking_grid_procedural, hacking_tile_distance_indicator,
    resolve_fitted_analyzer_virus_stats,
};
use glam::DVec3;
use std::collections::HashMap;

const PILOT_1: i64 = 90_000_001;
const JITA_SYS: u32 = 30_000_142; // High-sec (0.95)
const LOWSEC_SYS: u32 = 30_002_904; // Tama (Low-sec 0.28)
const TYPE_HERON: u32 = 605; // +5 virusCoherenceBonus hull bonus
const TYPE_RIFTER: u32 = 587;
const TYPE_DATA_ANALYZER_I: u32 = 22_175;
const TYPE_RELIC_ANALYZER_I: u32 = 22_177;
const SKILL_HACKING: u32 = 21_718;

fn make_item(
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
    singleton: bool,
) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id,
        type_id,
        owner_id,
        location_id,
        flag,
        quantity,
        singleton,
        ..Default::default()
    }
}

async fn setup_space_pilot(
    ctx: &ServerContext,
    sess_id: u64,
    char_id: i64,
    ship_id: i64,
    ship_type_id: u32,
    sys_id: u32,
    pos: DVec3,
) -> SessionState {
    let kw = HashMap::new();
    let mut sess = SessionState::new(sess_id);
    ctx.dispatch_rpc(&mut sess, "authentication", None, "Login", &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())], &kw, None).await.expect("Login");
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None).await.expect("SelectCharacterID");

    sess.solar_system_id = Some(sys_id);
    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = Some(ship_id);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(ship_id, make_item(ship_id, ship_type_id, char_id, i64::from(sys_id), InventoryFlag::None, 1, true));
        snap.item_positions.insert(ship_id, pos.to_array());
    });

    let sys = ctx.systems.get_or_spawn(sys_id);
    let mut cs = ctx.build_ship_combat_state(char_id, ship_id, ship_type_id);
    cs.shield_hp = 50_000.0;
    cs.max_shield_hp = 50_000.0;
    let _ = sys
        .spawn_ball_with_meta(
            Ball {
                id: ship_id,
                type_id: ship_type_id,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 365.0,
                inertia: 1.0,
                mass: 1_200_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            cs,
            ctx.build_ship_slim_meta(char_id, ship_type_id),
        )
        .await;
    sess
}

fn build_controlled_v24_board() -> Vec<HackingTileState> {
    let mut tiles: Vec<HackingTileState> = (0..24_u32)
        .map(|idx| HackingTileState {
            tile_index: idx,
            revealed: idx == 0,
            blocked_by_defensive: false,
            kind: HackingNodeKind::Empty,
            coherence: 0,
            strength: 0,
            utility_drop: None,
        })
        .collect();
    let specs: &[(usize, HackingNodeKind, i32, i32, Option<HackingUtilityKind>)] = &[
        (1, HackingNodeKind::VirusSuppressor, 35, 10, None),
        (2, HackingNodeKind::RestorationNode, 35, 10, None),
        (7, HackingNodeKind::Firewall, 45, 12, None),
        (12, HackingNodeKind::Antivirus, 15, 20, None),
        (13, HackingNodeKind::DataCache, 0, 0, Some(HackingUtilityKind::PolymorphicShield)),
        (14, HackingNodeKind::DataCache, 0, 0, None),
        (18, HackingNodeKind::UtilitySubsystem, 0, 0, Some(HackingUtilityKind::KernelRot)),
        (19, HackingNodeKind::UtilitySubsystem, 0, 0, Some(HackingUtilityKind::SelfRepair)),
        (20, HackingNodeKind::UtilitySubsystem, 0, 0, Some(HackingUtilityKind::SecondaryVector)),
        (23, HackingNodeKind::SystemCore, 20, 10, None),
    ];
    for &(idx, ref kind, coh, strg, util) in specs {
        tiles[idx].kind = kind.clone();
        tiles[idx].coherence = coh;
        tiles[idx].strength = strg;
        tiles[idx].utility_drop = util;
    }
    tiles
}

// ============================================================================
// 1. V24 Hacking Node Types, Analyzer/Skill/Hull Stats & Combat Rules
// ============================================================================
#[tokio::test]
async fn test_hacking_node_types_analyzer_stats_and_v24_combat() {
    // parity: feat:pve.hacking-node-types
    eve_testkit::attest(&["feat:pve.hacking-node-types"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let ship_id = 750_001_001i64;
    let analyzer_id = 750_001_010i64;
    let mut sess = setup_space_pilot(&ctx, 9201, PILOT_1, ship_id, TYPE_HERON, JITA_SYS, DVec3::ZERO).await;

    ensure_system_pve_sites_seeded(&ctx, JITA_SYS).await;
    let (data_can_id, site_pos) = with_pve_exploration_reg(&ctx, |reg| {
        let site = reg.sites_by_system[&JITA_SYS]
            .iter()
            .find(|s| s.site_category == "Data" && !s.hackable_can_ids.is_empty())
            .expect("Seeded Data site in Jita");
        (site.hackable_can_ids[0], site.position)
    });
    let sys_handle = ctx.systems.get_or_spawn(JITA_SYS);
    let _ = sys_handle.set_ball_position(ship_id, DVec3::from_array(site_pos)).await;

    // 1. Without a fitted analyzer, StartNewGameInstance fails with AnalyzerModuleRequired
    let no_mod_err = ctx
        .dispatch_rpc(&mut sess, "hackingMgr", None, "StartNewGameInstance", &[PyRep::Integer(data_can_id)], &kw, None)
        .await
        .expect_err("Missing analyzer must fail");
    assert_eq!(no_mod_err.code, "AnalyzerModuleRequired");

    // 2. Fit Data Analyzer I (22175: 40 coh / 20 str) + Hacking V (+50 coh) on Heron (605: +5 hull coh) = 95 coh, 20 str
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(analyzer_id, make_item(analyzer_id, TYPE_DATA_ANALYZER_I, PILOT_1, ship_id, InventoryFlag::MedSlot0, 1, true));
        snap.module_online_states.insert(analyzer_id, true);
        snap.skills.entry(PILOT_1).or_default().insert(
            SKILL_HACKING,
            TrainedSkillRecord { type_id: SKILL_HACKING, level: 5, skill_points: 256_000.0 },
        );
    });
    let (coh, strg, slots, _range) = resolve_fitted_analyzer_virus_stats(&ctx, &sess, PILOT_1, false).expect("Analyzer stats");
    assert_eq!((coh, strg, slots), (90, 25, 3));

    // 3. Procedural board scales coherence/strength with difficulty (1..=5) and includes all V24 node kinds
    let grid_d1 = build_hacking_grid_procedural(data_can_id, false, 1, 42);
    let grid_d5 = build_hacking_grid_procedural(data_can_id, false, 5, 42);
    assert_eq!(grid_d5.len(), 24);
    let core_d1 = grid_d1.iter().find(|t| t.kind == HackingNodeKind::SystemCore).unwrap();
    let core_d5 = grid_d5.iter().find(|t| t.kind == HackingNodeKind::SystemCore).unwrap();
    assert!(core_d5.coherence > core_d1.coherence && core_d5.strength > core_d1.strength);
    for expected_kind in [
        HackingNodeKind::SystemCore,
        HackingNodeKind::Firewall,
        HackingNodeKind::Antivirus,
        HackingNodeKind::RestorationNode,
        HackingNodeKind::VirusSuppressor,
        HackingNodeKind::DataCache,
        HackingNodeKind::UtilitySubsystem,
        HackingNodeKind::Empty,
    ] {
        assert!(grid_d5.iter().any(|t| t.kind == expected_kind), "Board must include {expected_kind:?}");
    }

    // 4. StartNewGameInstance & install controlled V24 board to verify every node & utility rule via real RPCs
    let start_rep = ctx
        .dispatch_rpc(&mut sess, "hackingMgr", None, "StartNewGameInstance", &[PyRep::Integer(data_can_id)], &kw, None)
        .await
        .expect("StartNewGameInstance");
    assert_eq!(start_rep.dict_get("virusCoherence").and_then(|v| v.as_i64()), Some(90));
    let start_dist = start_rep.dict_get("distanceIndicator").and_then(|v| v.as_i64()).unwrap();
    assert!((1..=5).contains(&start_dist));

    with_pve_exploration_reg(&ctx, |reg| {
        let g = reg.hacking_games.get_mut(&(PILOT_1, data_can_id)).unwrap();
        g.tiles = build_controlled_v24_board();
        g.virus_coherence = 120;
        g.max_virus_coherence = 120;
        g.base_virus_strength = 25;
        g.utilities = vec![None; 4];
    });

    // Empty tile 6 shows distanceIndicator in 1..=5 to nearest core/utility/cache
    let click_6 = ctx
        .dispatch_rpc(&mut sess, "hackingMgr", None, "ClickedOnTile", &[PyRep::Integer(data_can_id), PyRep::Integer(6)], &kw, None)
        .await
        .expect("Click Empty tile 6");
    let expected_d6 = with_pve_exploration_reg(&ctx, |reg| {
        hacking_tile_distance_indicator(6, &reg.hacking_games[&(PILOT_1, data_can_id)].tiles)
    });
    assert_eq!(click_6.dict_get("distanceIndicator").and_then(|v| v.as_i64()), Some(i64::from(expected_d6)));

    // Reveal VirusSuppressor at tile 1 -> lowers effective virus strength by 15 (25 -> 10) on subsequent attacks, then kill it
    let click_1 = ctx
        .dispatch_rpc(&mut sess, "hackingMgr", None, "ClickedOnTile", &[PyRep::Integer(data_can_id), PyRep::Integer(1)], &kw, None)
        .await
        .expect("Click VirusSuppressor tile 1");
    assert_eq!(click_1.dict_get("nodeKind").and_then(|v| v.as_str()), Some("VirusSuppressor"));
    let kill_1 = ctx
        .dispatch_rpc(&mut sess, "hackingMgr", None, "ClickedOnTile", &[PyRep::Integer(data_can_id), PyRep::Integer(1)], &kw, None)
        .await
        .expect("Kill VirusSuppressor");
    assert_eq!(kill_1.dict_get("virusStrength").and_then(|v| v.as_i64()), Some(10));
    assert_eq!(kill_1.dict_get("nodeCoherence").and_then(|v| v.as_i64()), Some(0));

    // Reveal Firewall at tile 7 (coh 45 - 25 = 20), then RestorationNode at tile 2 (+20 heal to Firewall -> 40)
    let click_7 = ctx
        .dispatch_rpc(&mut sess, "hackingMgr", None, "ClickedOnTile", &[PyRep::Integer(data_can_id), PyRep::Integer(7)], &kw, None)
        .await
        .expect("Click Firewall tile 7");
    assert_eq!(click_7.dict_get("nodeCoherence").and_then(|v| v.as_i64()), Some(20));

    // Reveal RestorationNode at tile 2 (coh 35 - 25 = 10) -> heals Firewall (tile 7: 20 -> 40)
    let _ = ctx
        .dispatch_rpc(&mut sess, "hackingMgr", None, "ClickedOnTile", &[PyRep::Integer(data_can_id), PyRep::Integer(2)], &kw, None)
        .await
        .expect("Click RestorationNode tile 2");
    let fw_healed_coh = with_pve_exploration_reg(&ctx, |reg| reg.hacking_games[&(PILOT_1, data_can_id)].tiles[7].coherence);
    assert_eq!(fw_healed_coh, 40, "RestorationNode must heal revealed Firewall (+20 -> 40)");

    // One-shot Antivirus (tile 12, coh 15 <= 25 str); active RestorationNode heals itself (tile 2: 10 -> 30)
    let click_12 = ctx
        .dispatch_rpc(&mut sess, "hackingMgr", None, "ClickedOnTile", &[PyRep::Integer(data_can_id), PyRep::Integer(12)], &kw, None)
        .await
        .expect("One-shot Antivirus tile 12");
    assert_eq!(click_12.dict_get("nodeCoherence").and_then(|v| v.as_i64()), Some(0));
    let rn_coh = with_pve_exploration_reg(&ctx, |reg| reg.hacking_games[&(PILOT_1, data_can_id)].tiles[2].coherence);
    assert_eq!(rn_coh, 30, "RestorationNode heals revealed Defensive Subsystem (+20 -> 30)");

    // Kill RestorationNode (tile 2: 30 -> 5 -> 0)
    let _ = ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "ClickedOnTile", &[PyRep::Integer(data_can_id), PyRep::Integer(2)], &kw, None).await.unwrap();
    let _ = ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "ClickedOnTile", &[PyRep::Integer(data_can_id), PyRep::Integer(2)], &kw, None).await.unwrap();

    // DataCache at tile 13 yields PolymorphicShield into slot 0; DataCache at tile 14 conceals a Firewall
    let _ = ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "ClickedOnTile", &[PyRep::Integer(data_can_id), PyRep::Integer(13)], &kw, None).await.unwrap();
    let click_14 = ctx
        .dispatch_rpc(&mut sess, "hackingMgr", None, "ClickedOnTile", &[PyRep::Integer(data_can_id), PyRep::Integer(14)], &kw, None)
        .await
        .expect("Click DataCache 14 (concealed Firewall)");
    assert_eq!(click_14.dict_get("nodeKind").and_then(|v| v.as_str()), Some("Firewall"));

    // Collect UtilitySubsystems at tiles 18 (KernelRot), 19 (SelfRepair), 20 (SecondaryVector) and activate all 4
    for tid in [18_i64, 19, 20] {
        ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "ClickedOnTile", &[PyRep::Integer(data_can_id), PyRep::Integer(tid)], &kw, None).await.unwrap();
    }
    let use_ps = ctx
        .dispatch_rpc(&mut sess, "hackingMgr", None, "UsedUtilityElement", &[PyRep::Integer(data_can_id), PyRep::Integer(0), PyRep::Integer(0)], &kw, None)
        .await
        .expect("Use PolymorphicShield");
    assert_eq!(use_ps.dict_get("polymorphicShieldCharges").and_then(|v| v.as_i64()), Some(2));

    let coh_before_kr = with_pve_exploration_reg(&ctx, |reg| reg.hacking_games[&(PILOT_1, data_can_id)].tiles[7].coherence);
    let _ = ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "UsedUtilityElement", &[PyRep::Integer(data_can_id), PyRep::Integer(1), PyRep::Integer(7)], &kw, None).await.unwrap();
    let coh_after_kr = with_pve_exploration_reg(&ctx, |reg| reg.hacking_games[&(PILOT_1, data_can_id)].tiles[7].coherence);
    assert_eq!(coh_after_kr, (coh_before_kr / 2).max(1));

    let coh_before_sr = with_pve_exploration_reg(&ctx, |reg| reg.hacking_games[&(PILOT_1, data_can_id)].virus_coherence);
    let use_sr = ctx
        .dispatch_rpc(&mut sess, "hackingMgr", None, "UsedUtilityElement", &[PyRep::Integer(data_can_id), PyRep::Integer(2), PyRep::Integer(0)], &kw, None)
        .await
        .expect("Use SelfRepair");
    assert_eq!(use_sr.dict_get("virusCoherence").and_then(|v| v.as_i64()), Some(i64::from((coh_before_sr + 30).min(120))));

    let _ = ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "UsedUtilityElement", &[PyRep::Integer(data_can_id), PyRep::Integer(3), PyRep::Integer(7)], &kw, None).await.unwrap();
    let sv_active = with_pve_exploration_reg(&ctx, |reg| reg.hacking_games[&(PILOT_1, data_can_id)].secondary_vector_target);
    assert_eq!(sv_active, Some((7, 3)));
}

// ============================================================================
// 2. Hacking Failure Outcomes (Lockout, Self-Destruct, Ghost Detonation) & Core Kill Spew
// ============================================================================
#[tokio::test]
async fn test_hacking_failure_outcomes_and_core_kill_loot_spew() {
    // parity: feat:pve.hacking-failure-outcome
    eve_testkit::attest(&["feat:pve.hacking-failure-outcome"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let ship_id = 750_002_001i64;
    let relic_analyzer_id = 750_002_010i64;
    let data_analyzer_id = 750_002_011i64;
    let mut sess = setup_space_pilot(&ctx, 9202, PILOT_1, ship_id, TYPE_RIFTER, LOWSEC_SYS, DVec3::ZERO).await;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(relic_analyzer_id, make_item(relic_analyzer_id, TYPE_RELIC_ANALYZER_I, PILOT_1, ship_id, InventoryFlag::MedSlot0, 1, true));
        snap.items.insert(data_analyzer_id, make_item(data_analyzer_id, TYPE_DATA_ANALYZER_I, PILOT_1, ship_id, InventoryFlag::MedSlot1, 1, true));
        snap.module_online_states.insert(relic_analyzer_id, true);
        snap.module_online_states.insert(data_analyzer_id, true);
    });

    ensure_system_pve_sites_seeded(&ctx, LOWSEC_SYS).await;
    let (can_explode, can_win, site_pos) = with_pve_exploration_reg(&ctx, |reg| {
        let site = reg.sites_by_system[&LOWSEC_SYS]
            .iter()
            .find(|s| s.site_category == "Relic" && s.hackable_can_ids.len() >= 2)
            .expect("Seeded Relic site in low-sec");
        (site.hackable_can_ids[0], site.hackable_can_ids[1], site.position)
    });
    let sys_handle = ctx.systems.get_or_spawn(LOWSEC_SYS);
    let _ = sys_handle.set_ball_position(ship_id, DVec3::from_array(site_pos)).await;
    for can_id in [can_explode, can_win] {
        let _ = sys_handle
            .spawn_ball(Ball {
                id: can_id,
                type_id: 28_367,
                position: DVec3::from_array(site_pos),
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 10_000.0,
                radius: 500.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await;
    }

    // 1. Standard Relic container: 1st failure leaves container alive; 2nd failure self-destructs with AoE damage & locks out
    for attempt in 1..=2 {
        ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "StartNewGameInstance", &[PyRep::Integer(can_explode)], &kw, None).await.unwrap();
        with_pve_exploration_reg(&ctx, |reg| {
            let g = reg.hacking_games.get_mut(&(PILOT_1, can_explode)).unwrap();
            g.virus_coherence = 10;
            g.tiles[1].kind = HackingNodeKind::Antivirus;
            g.tiles[1].coherence = 50;
            g.tiles[1].strength = 25;
            g.tiles[1].revealed = false;
        });
        let fail_rep = ctx
            .dispatch_rpc(&mut sess, "hackingMgr", None, "ClickedOnTile", &[PyRep::Integer(can_explode), PyRep::Integer(1)], &kw, None)
            .await
            .expect("Fail hack attempt");
        assert_eq!(fail_rep.dict_get("status").and_then(|v| v.as_str()), Some("Failed"));
        if attempt == 1 {
            assert_eq!(fail_rep.dict_get("containerExploded"), Some(&PyRep::Bool(false)));
            assert!(sys_handle.get_ball_snapshot(can_explode).await.unwrap().is_some());
        } else {
            assert_eq!(fail_rep.dict_get("containerExploded"), Some(&PyRep::Bool(true)));
            let dmg = fail_rep.dict_get("explosionDamage").and_then(|v| v.as_f64()).unwrap();
            assert!((2_000.0..=5_000.0).contains(&dmg));
            assert!(sys_handle.get_ball_snapshot(can_explode).await.unwrap().is_none());
        }
    }
    let locked_err = ctx
        .dispatch_rpc(&mut sess, "hackingMgr", None, "StartNewGameInstance", &[PyRep::Integer(can_explode)], &kw, None)
        .await
        .expect_err("Destroyed container must reject further hack attempts");
    assert_eq!(locked_err.code, "HackingGameAlreadyFinished");

    // 2. Ghost Site failure triggers immediate container detonation, 8,000 HP low-sec AoE explosive damage & pirate ambush NPCs
    let (ghost_can_id, ghost_pos) = with_pve_exploration_reg(&ctx, |reg| {
        let site = reg.sites_by_system[&LOWSEC_SYS]
            .iter()
            .find(|s| s.site_category == "Ghost" && !s.hackable_can_ids.is_empty())
            .expect("Seeded Ghost site in low-sec");
        (site.hackable_can_ids[0], site.position)
    });
    let _ = sys_handle.set_ball_position(ship_id, DVec3::from_array(ghost_pos)).await;
    let _ = sys_handle
        .spawn_ball(Ball {
            id: ghost_can_id,
            type_id: 32_717,
            position: DVec3::from_array(ghost_pos),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 10_000.0,
            radius: 500.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "StartNewGameInstance", &[PyRep::Integer(ghost_can_id)], &kw, None).await.unwrap();
    with_pve_exploration_reg(&ctx, |reg| {
        let g = reg.hacking_games.get_mut(&(PILOT_1, ghost_can_id)).unwrap();
        g.virus_coherence = 10;
        g.tiles[1].kind = HackingNodeKind::Antivirus;
        g.tiles[1].coherence = 50;
        g.tiles[1].strength = 25;
        g.tiles[1].revealed = false;
    });
    let ghost_fail = ctx
        .dispatch_rpc(&mut sess, "hackingMgr", None, "ClickedOnTile", &[PyRep::Integer(ghost_can_id), PyRep::Integer(1)], &kw, None)
        .await
        .expect("Fail Ghost hack");
    assert_eq!(ghost_fail.dict_get("ghostDetonated"), Some(&PyRep::Bool(true)));
    assert_eq!(ghost_fail.dict_get("ghostExplosiveDamage").and_then(|v| v.as_f64()), Some(10_000.0));
    let PyRep::List(ambush_npcs) = ghost_fail.dict_get("ghostSpawnedNpcBallIDs").unwrap() else {
        panic!("Expected ghostSpawnedNpcBallIDs list");
    };
    assert!(!ambush_npcs.is_empty(), "Ghost detonation must spawn pirate ambush NPCs");

    // 3. Successful System Core kill opens container, triggers loot spew mini-containers, and locks out re-hacking
    let _ = sys_handle.set_ball_position(ship_id, DVec3::from_array(site_pos)).await;
    ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "StartNewGameInstance", &[PyRep::Integer(can_win)], &kw, None).await.unwrap();
    with_pve_exploration_reg(&ctx, |reg| {
        let g = reg.hacking_games.get_mut(&(PILOT_1, can_win)).unwrap();
        g.tiles[1].kind = HackingNodeKind::SystemCore;
        g.tiles[1].coherence = 15;
        g.tiles[1].strength = 10;
        g.tiles[1].revealed = false;
    });
    let win_rep = ctx
        .dispatch_rpc(&mut sess, "hackingMgr", None, "ClickedOnTile", &[PyRep::Integer(can_win), PyRep::Integer(1)], &kw, None)
        .await
        .expect("Kill SystemCore");
    assert_eq!(win_rep.dict_get("status").and_then(|v| v.as_str()), Some("Won"));
    let PyRep::List(spewed) = win_rep.dict_get("spewedContainers").unwrap() else {
        panic!("Expected spewedContainers list");
    };
    assert!(!spewed.is_empty(), "Core kill must trigger loot spew mini-containers");
    let rehack_err = ctx
        .dispatch_rpc(&mut sess, "hackingMgr", None, "StartNewGameInstance", &[PyRep::Integer(can_win)], &kw, None)
        .await
        .expect_err("Already hacked container cannot be re-hacked");
    assert_eq!(rehack_err.code, "HackingGameAlreadyFinished");
}

// ============================================================================
// 3. Negative Regressions: Ghost Blast >10km Zero Damage & Utility Preconditions
// ============================================================================
#[tokio::test]
async fn test_ghost_blast_radius_cutoff_and_utility_range_cloak_tile_guards() {
    // parity: feat:pve.hacking-node-types, feat:pve.hacking-failure-outcome, feat:pve.ghost-sites
    eve_testkit::attest(&[
        "feat:pve.hacking-node-types",
        "feat:pve.hacking-failure-outcome",
        "feat:pve.ghost-sites",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let ship_id = 750_009_001i64;
    let bystander_ship_id = 750_009_002i64;
    let data_analyzer_id = 750_009_010i64;
    let mut sess = setup_space_pilot(&ctx, 9209, PILOT_1, ship_id, TYPE_HERON, LOWSEC_SYS, DVec3::ZERO).await;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            data_analyzer_id,
            make_item(data_analyzer_id, TYPE_DATA_ANALYZER_I, PILOT_1, ship_id, InventoryFlag::MedSlot0, 1, true),
        );
        snap.module_online_states.insert(data_analyzer_id, true);
    });

    ensure_system_pve_sites_seeded(&ctx, LOWSEC_SYS).await;
    let (data_can_id, data_pos, ghost_can_id, ghost_pos) = with_pve_exploration_reg(&ctx, |reg| {
        let d = reg.sites_by_system[&LOWSEC_SYS]
            .iter()
            .find(|s| s.site_category == "Data" && !s.hackable_can_ids.is_empty())
            .unwrap();
        let g = reg.sites_by_system[&LOWSEC_SYS]
            .iter()
            .find(|s| s.site_category == "Ghost" && !s.hackable_can_ids.is_empty())
            .unwrap();
        (d.hackable_can_ids[0], d.position, g.hackable_can_ids[0], g.position)
    });

    let sys_handle = ctx.systems.get_or_spawn(LOWSEC_SYS);
    for (cid, pos) in [(data_can_id, data_pos), (ghost_can_id, ghost_pos)] {
        let _ = sys_handle
            .spawn_ball(Ball {
                id: cid,
                type_id: 32_717,
                position: DVec3::from_array(pos),
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 10_000.0,
                radius: 500.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await;
    }

    // 1. Utility precondition guards: unrevealed target tile, cloaked ship, and >5km range
    //    must all fail WITHOUT consuming the utility slot!
    let _ = sys_handle.set_ball_position(ship_id, DVec3::from_array(data_pos)).await;
    ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "StartNewGameInstance", &[PyRep::Integer(data_can_id)], &kw, None)
        .await
        .expect("StartNewGameInstance");
    with_pve_exploration_reg(&ctx, |reg| {
        let g = reg.hacking_games.get_mut(&(PILOT_1, data_can_id)).unwrap();
        g.tiles = build_controlled_v24_board();
        g.utilities = vec![
            Some(HackingUtilityKind::KernelRot),
            Some(HackingUtilityKind::SelfRepair),
            None,
        ];
    });

    // (a) Unrevealed tile 7 rejected for both UsedUtilityElement and UsedUtilityElementOnVirus; slot 0 intact
    let unrev1 = ctx
        .dispatch_rpc(
            &mut sess,
            "hackingMgr",
            None,
            "UsedUtilityElement",
            &[PyRep::Integer(data_can_id), PyRep::Integer(0), PyRep::Integer(7)],
            &kw,
            None,
        )
        .await
        .expect_err("Unrevealed tile must fail UsedUtilityElement");
    assert_eq!(unrev1.code, "TargetTileNotRevealed");
    let unrev2 = ctx
        .dispatch_rpc(
            &mut sess,
            "hackingMgr",
            None,
            "UsedUtilityElementOnVirus",
            &[PyRep::Integer(data_can_id), PyRep::Integer(0), PyRep::Integer(7)],
            &kw,
            None,
        )
        .await
        .expect_err("Unrevealed tile must fail UsedUtilityElementOnVirus");
    assert_eq!(unrev2.code, "TargetTileNotRevealed");
    assert_eq!(
        with_pve_exploration_reg(&ctx, |reg| reg.hacking_games[&(PILOT_1, data_can_id)].utilities[0]),
        Some(HackingUtilityKind::KernelRot),
        "Failed target tile validation must NOT consume utility slot"
    );

    // (b) Cloaked ship rejected; slot 1 intact
    sys_handle.try_update_combat_state(ship_id, |cs| cs.is_cloaked = true);
    let cloak_err = ctx
        .dispatch_rpc(
            &mut sess,
            "hackingMgr",
            None,
            "UsedUtilityElementOnVirus",
            &[PyRep::Integer(data_can_id), PyRep::Integer(1)],
            &kw,
            None,
        )
        .await
        .expect_err("Cloaked ship must not activate hacking utility");
    assert_eq!(cloak_err.code, "CannotHackWhileCloaked");
    sys_handle.try_update_combat_state(ship_id, |cs| cs.is_cloaked = false);

    // (c) Ship moved 12 km away (> 5 km Data Analyzer I range) rejected; slot 1 still intact!
    let _ = sys_handle
        .set_ball_position(ship_id, DVec3::from_array(data_pos) + DVec3::new(12_000.0, 0.0, 0.0))
        .await;
    let range_err = ctx
        .dispatch_rpc(
            &mut sess,
            "hackingMgr",
            None,
            "UsedUtilityElement",
            &[PyRep::Integer(data_can_id), PyRep::Integer(1), PyRep::Integer(0)],
            &kw,
            None,
        )
        .await
        .expect_err("Out-of-range ship must not activate hacking utility");
    assert_eq!(range_err.code, "OutOfRange");
    assert_eq!(
        with_pve_exploration_reg(&ctx, |reg| reg.hacking_games[&(PILOT_1, data_can_id)].utilities[1]),
        Some(HackingUtilityKind::SelfRepair),
        "Out-of-range utility activation must NOT consume utility slot"
    );

    // 2. Ghost Site Blast Radius Cutoff:
    //    - Bystander ship at 25 km (> 10 km blast radius) takes ZERO damage
    //    - Hacking ship moved to 25 km before QuitHackingAttempt ALSO takes ZERO damage (no teleport fallback!)
    let mut bystander_cs = ctx.build_ship_combat_state(PILOT_1, bystander_ship_id, TYPE_RIFTER);
    bystander_cs.shield_hp = 20_000.0;
    bystander_cs.max_shield_hp = 20_000.0;
    let _ = sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: bystander_ship_id,
                type_id: TYPE_RIFTER,
                position: DVec3::from_array(ghost_pos) + DVec3::new(25_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 365.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            bystander_cs,
        )
        .await;

    let _ = sys_handle.set_ball_position(ship_id, DVec3::from_array(ghost_pos)).await;
    ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "StartNewGameInstance", &[PyRep::Integer(ghost_can_id)], &kw, None)
        .await
        .expect("Start Ghost hack");
    // Move hacker's ship to 25 km away (> 10 km blast radius) and quit the Ghost hack -> detonates Ghost can
    let _ = sys_handle
        .set_ball_position(ship_id, DVec3::from_array(ghost_pos) + DVec3::new(25_000.0, 0.0, 0.0))
        .await;
    let shield_before = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap().shield_hp;
    ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "QuitHackingAttempt", &[PyRep::Integer(ghost_can_id)], &kw, None)
        .await
        .expect("Quit Ghost hack triggers detonation");

    let shield_after = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap().shield_hp;
    let bystander_shield_after = sys_handle.get_combat_state(bystander_ship_id).await.unwrap().unwrap().shield_hp;
    assert_eq!(
        shield_after, shield_before,
        "Hacker ship at 25 km (> 10 km blast radius) must take 0 Ghost Site detonation damage"
    );
    assert_eq!(
        bystander_shield_after, 20_000.0,
        "Bystander ship at 25 km (> 10 km blast radius) must take 0 Ghost Site detonation damage"
    );
}
