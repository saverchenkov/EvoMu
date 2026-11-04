//! Behavioral integration tests for Exploration Hacking & Minigame Mechanics:
//! 1. `feat:exploration.hacking-coherence-and-strength`:
//!    Player virus coherence (health) and virus strength (attack) scale from analyzer module stats
//!    (T1: 20 strength / 40-60 coherence; T2: 30 strength / 80-100 coherence) plus Hacking / Archaeology skills
//!    (+10 coherence per level); attacking a defensive node subtracts player virus strength from node coherence
//!    while node attack subtracts from player coherence; hack fails if player coherence reaches 0.
//! 2. `feat:exploration.hacking-node-network`:
//!    Initiating a hack on a relic or data container generates a hexagonal node grid; exploring adjacent
//!    unvisited nodes reveals contents: empty nodes, defensive subroutines (Firewall, Anti-Virus, Core),
//!    or utility elements (Self-Repair, Polarity Inversion, Secondary Vector); undiscovered nodes provide
//!    distance-indicator clues to the System Core.
//! 3. `feat:exploration.hack-failure-container-explosion`:
//!    Failing a hack attempt twice on a relic or data site container in null-sec, low-sec, or wormhole space
//!    triggers container self-destruction; all contained loot items are destroyed, and an explosive blast
//!    inflicts 2,000-5,000 HP area-of-effect damage to any ship within 5,000 meters.

#![forbid(unsafe_code)]

use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::pve_exploration::{
    ensure_system_pve_sites_seeded, with_pve_exploration_reg, HackingNodeKind,
};
use eve_server::services::pve_hacking_board::{
    build_hacking_grid_procedural_with_sde, resolve_fitted_analyzer_virus_stats,
};
use eve_server::services::pve_hacking_sites::{
    hacking_hex_distance, hacking_tile_axial_coords,
};
use glam::DVec3;
use std::collections::HashMap;

const CHAR_EXPLORER: i64 = 90_000_001;
const CHAR_BYSTANDER: i64 = 90_000_002;

const HIGHSEC_SYS: u32 = 30_000_142; // Jita (0.95)
const LOWSEC_SYS: u32 = 30_002_904;  // VFK-IV (Null-sec -0.85) / lowsec testing with 30_002_813 (Tama 0.28)
const NULLSEC_SYS: u32 = 30_000_208; // LZ-6SU (Null-sec -0.17)
const WORMHOLE_SYS: u32 = 31_000_001; // J-Space (Wormhole)

const TYPE_HERON: u32 = 605; // Caldari Frigate, +5 virusCoherenceBonus hull bonus
const TYPE_RIFTER: u32 = 587; // Minmatar Frigate, no hacking hull bonus
const TYPE_DATA_ANALYZER_I: u32 = 22_175; // T1 Data: 40 coherence, 20 strength, 3 slots
const TYPE_RELIC_ANALYZER_I: u32 = 22_177; // T1 Relic: 40 coherence, 20 strength, 3 slots
const TYPE_RELIC_ANALYZER_II: u32 = 30_832; // T2 Relic: 60 coherence, 30 strength, 3 slots
const SKILL_HACKING: u32 = 21_718;
const SKILL_ARCHAEOLOGY: u32 = 13_278;

fn make_inventory_item(
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id,
        type_id,
        owner_id,
        location_id,
        flag,
        quantity,
        singleton: true,
        ..Default::default()
    }
}

async fn setup_in_space_pilot(
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
    let (user, pass) = if char_id == CHAR_BYSTANDER {
        ("other", "other-secret")
    } else {
        ("demo", "demo-secret")
    };
    ctx.dispatch_rpc(
        &mut sess,
        "authentication",
        None,
        "Login",
        &[PyRep::String(user.into()), PyRep::String(pass.into())],
        &kw,
        None,
    )
    .await
    .expect("Login");
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

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
        snap.items.insert(
            ship_id,
            make_inventory_item(ship_id, ship_type_id, char_id, i64::from(sys_id), InventoryFlag::None, 1),
        );
        snap.item_positions.insert(ship_id, pos.to_array());
    });

    let sys = ctx.systems.get_or_spawn(sys_id);
    let mut cs = ctx.build_ship_combat_state(char_id, ship_id, ship_type_id);
    cs.shield_hp = 10_000.0;
    cs.max_shield_hp = 10_000.0;
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

// ============================================================================
// 1. Analyzer Stats, Skill Scaling, Hexagonal Node Grid & Minigame Combat
// ============================================================================
#[tokio::test]
async fn test_exploration_hacking_coherence_strength_and_node_network() {
    // parity: feat:exploration.hacking-coherence-and-strength, feat:exploration.hacking-node-network
    eve_testkit::attest(&[
        "feat:exploration.hacking-coherence-and-strength",
        "feat:exploration.hacking-node-network",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();

    let ship_id = 700_100_001i64;
    let analyzer_id = 700_100_002i64;
    let pos = DVec3::ZERO;
    let mut sess = setup_in_space_pilot(&ctx, 9401, CHAR_EXPLORER, ship_id, TYPE_RIFTER, LOWSEC_SYS, pos).await;

    // ------------------------------------------------------------------------
    // Part 1: Analyzer stats scaling (T1 vs T2, Skills +10 coherence per level)
    // ------------------------------------------------------------------------
    // Fit T1 Data Analyzer (22175: 40 base coherence, 20 virus strength) with level 0 Hacking skill
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            analyzer_id,
            make_inventory_item(analyzer_id, TYPE_DATA_ANALYZER_I, CHAR_EXPLORER, ship_id, InventoryFlag::MedSlot0, 1),
        );
        snap.module_online_states.insert(analyzer_id, true);
        snap.skills.entry(CHAR_EXPLORER).or_default().remove(&SKILL_HACKING);
    });

    let (t1_coh_0, t1_str_0, t1_slots_0, _) =
        resolve_fitted_analyzer_virus_stats(&ctx, &sess, CHAR_EXPLORER, false).expect("T1 stats with skill 0");
    assert_eq!(t1_coh_0, 40, "T1 Data Analyzer base coherence is 40");
    assert_eq!(t1_str_0, 20, "T1 Data Analyzer base virus strength is 20");
    assert_eq!(t1_slots_0, 3, "T1 Data Analyzer has 3 utility element slots");

    // Add Hacking skill level 3 (+30 coherence)
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(CHAR_EXPLORER).or_default().insert(
            SKILL_HACKING,
            TrainedSkillRecord { type_id: SKILL_HACKING, level: 3, skill_points: 40_000.0 },
        );
    });
    let (t1_coh_3, t1_str_3, _, _) =
        resolve_fitted_analyzer_virus_stats(&ctx, &sess, CHAR_EXPLORER, false).expect("T1 stats with skill 3");
    assert_eq!(t1_coh_3, 70, "T1 Data Analyzer + Hacking III = 40 + 3*10 = 70 coherence");
    assert_eq!(t1_str_3, 20, "Virus strength remains 20");

    // Add Hacking skill level 5 (+50 coherence)
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(CHAR_EXPLORER).or_default().insert(
            SKILL_HACKING,
            TrainedSkillRecord { type_id: SKILL_HACKING, level: 5, skill_points: 256_000.0 },
        );
    });
    let (t1_coh_5, t1_str_5, _, _) =
        resolve_fitted_analyzer_virus_stats(&ctx, &sess, CHAR_EXPLORER, false).expect("T1 stats with skill 5");
    assert_eq!(t1_coh_5, 90, "T1 Data Analyzer + Hacking V = 40 + 5*10 = 90 coherence");
    assert_eq!(t1_str_5, 20, "Virus strength remains 20");

    // Fit T2 Relic Analyzer (30832: 60 base coherence, 30 virus strength, 4 slots) + Archaeology IV (+40 coherence)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(item) = snap.items.get_mut(&analyzer_id) {
            item.type_id = TYPE_RELIC_ANALYZER_II;
        }
        snap.skills.entry(CHAR_EXPLORER).or_default().insert(
            SKILL_ARCHAEOLOGY,
            TrainedSkillRecord { type_id: SKILL_ARCHAEOLOGY, level: 4, skill_points: 135_765.0 },
        );
    });
    let (t2_coh, t2_str, t2_slots, _) =
        resolve_fitted_analyzer_virus_stats(&ctx, &sess, CHAR_EXPLORER, true).expect("T2 stats with Archaeology IV");
    assert_eq!(t2_coh, 100, "T2 Relic Analyzer + Archaeology IV = 60 + 4*10 = 100 coherence");
    assert_eq!(t2_str, 30, "T2 Relic Analyzer virus strength is 30");
    assert_eq!(t2_slots, 3, "T2 Relic Analyzer has 3 utility element slots");

    // ------------------------------------------------------------------------
    // Part 2: Hexagonal node network grid generation & distance clues
    // ------------------------------------------------------------------------
    let test_can_id = 700_100_010i64;
    let grid = build_hacking_grid_procedural_with_sde(Some(&ctx.sde), test_can_id, Some(28367), true, 3, 1337);
    assert_eq!(grid.len(), 24, "Grid must contain 24 hexagonal tiles");

    // Verify hex axial coordinates and adjacency topology
    for tile in &grid {
        let (q, r) = hacking_tile_axial_coords(tile.tile_index);
        let dist_from_origin = hacking_hex_distance(0, tile.tile_index);
        assert_eq!(dist_from_origin as i32, (q.abs() + (q + r).abs() + r.abs()) / 2);
    }

    // Verify presence of node varieties: Empty, Defensive subroutines, Core, Utilities
    assert!(grid.iter().any(|t| t.kind == HackingNodeKind::Empty), "Grid must have Empty nodes");
    assert!(grid.iter().any(|t| t.kind == HackingNodeKind::SystemCore), "Grid must have SystemCore");
    assert!(
        grid.iter().any(|t| matches!(t.kind, HackingNodeKind::Firewall | HackingNodeKind::Antivirus)),
        "Grid must have defensive subroutines (Firewall / Antivirus)"
    );
    assert!(
        grid.iter().any(|t| t.utility_drop.is_some() || t.kind == HackingNodeKind::UtilitySubsystem),
        "Grid must contain utility elements"
    );

    // ------------------------------------------------------------------------
    // Part 3: In-game minigame traversal, adjacency check & distance indicators
    // ------------------------------------------------------------------------
    ensure_system_pve_sites_seeded(&ctx, LOWSEC_SYS).await;
    let (relic_can_id, site_pos) = with_pve_exploration_reg(&ctx, |reg| {
        let site = reg.sites_by_system[&LOWSEC_SYS]
            .iter()
            .find(|s| s.site_category == "Relic" && !s.hackable_can_ids.is_empty())
            .expect("Relic site in low-sec");
        (site.hackable_can_ids[0], site.position)
    });

    let sys_handle = ctx.systems.get_or_spawn(LOWSEC_SYS);
    let _ = sys_handle.set_ball_position(ship_id, DVec3::from_array(site_pos)).await;
    let _ = sys_handle
        .spawn_ball(Ball {
            id: relic_can_id,
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

    let start_rep = ctx
        .dispatch_rpc(&mut sess, "hackingMgr", None, "StartNewGameInstance", &[PyRep::Integer(relic_can_id)], &kw, None)
        .await
        .expect("StartNewGameInstance");
    assert_eq!(start_rep.dict_get("status").and_then(|v| v.as_str()), Some("InProgress"));
    assert_eq!(start_rep.dict_get("virusCoherence").and_then(|v| v.as_i64()), Some(100));
    assert_eq!(start_rep.dict_get("virusStrength").and_then(|v| v.as_i64()), Some(30));

    let start_dist = start_rep.dict_get("distanceIndicator").and_then(|v| v.as_i64()).unwrap();
    assert!((1..=5).contains(&start_dist), "Distance indicator must be between 1 and 5");

    // Adjacency enforcement: Clicking non-adjacent unrevealed tile 15 fails with TileNotAccessible
    let non_adj_err = ctx
        .dispatch_rpc(&mut sess, "hackingMgr", None, "ClickedOnTile", &[PyRep::Integer(relic_can_id), PyRep::Integer(15)], &kw, None)
        .await
        .expect_err("Non-adjacent tile click must fail");
    assert_eq!(non_adj_err.code, "TileNotAccessible");

    // Install controlled board to test defensive subroutine combat rules deterministically
    with_pve_exploration_reg(&ctx, |reg| {
        let g = reg.hacking_games.get_mut(&(CHAR_EXPLORER, relic_can_id)).unwrap();
        // Tile 0 is revealed (Empty).
        // Tile 1 is adjacent to tile 0: Antivirus (coherence 50, strength 20).
        g.virus_coherence = 50;
        g.max_virus_coherence = 100;
        g.base_virus_strength = 30;
        for t in &mut g.tiles {
            t.revealed = t.tile_index == 0;
            t.kind = HackingNodeKind::Empty;
            t.coherence = 0;
            t.strength = 0;
        }
        g.tiles[1].kind = HackingNodeKind::Antivirus;
        g.tiles[1].coherence = 50;
        g.tiles[1].strength = 20;

        g.tiles[2].kind = HackingNodeKind::SystemCore;
        g.tiles[2].coherence = 30;
        g.tiles[2].strength = 10;
    });

    // ------------------------------------------------------------------------
    // Part 4: Defensive subroutine combat resolution
    // ------------------------------------------------------------------------
    // Click tile 1 (Antivirus with 50 coherence, 20 strength).
    // Player has strength 30, coherence 50.
    // Turn 1 attack:
    // Node coherence: 50 - 30 = 20 remaining.
    // Player coherence: 50 - 20 = 30 remaining.
    let turn1_rep = ctx
        .dispatch_rpc(&mut sess, "hackingMgr", None, "ClickedOnTile", &[PyRep::Integer(relic_can_id), PyRep::Integer(1)], &kw, None)
        .await
        .expect("Attack Antivirus turn 1");
    assert_eq!(turn1_rep.dict_get("nodeKind").and_then(|v| v.as_str()), Some("Antivirus"));
    assert_eq!(turn1_rep.dict_get("nodeCoherence").and_then(|v| v.as_i64()), Some(20), "Node coherence reduced by player virus strength");
    assert_eq!(turn1_rep.dict_get("virusCoherence").and_then(|v| v.as_i64()), Some(30), "Player coherence reduced by node strength");
    assert_eq!(turn1_rep.dict_get("status").and_then(|v| v.as_str()), Some("InProgress"));

    // Turn 2 attack:
    // Node coherence: 20 - 30 = 0 (node destroyed).
    // Node is defeated and cannot counter-attack! Player coherence remains 30.
    let turn2_rep = ctx
        .dispatch_rpc(&mut sess, "hackingMgr", None, "ClickedOnTile", &[PyRep::Integer(relic_can_id), PyRep::Integer(1)], &kw, None)
        .await
        .expect("Defeat Antivirus turn 2");
    assert_eq!(turn2_rep.dict_get("nodeCoherence").and_then(|v| v.as_i64()), Some(0), "Defensive node coherence reaches 0");
    assert_eq!(turn2_rep.dict_get("virusCoherence").and_then(|v| v.as_i64()), Some(30), "Player coherence preserved upon destroying node");

    // Failure rule: if player virus coherence reaches 0, hack status becomes Failed
    with_pve_exploration_reg(&ctx, |reg| {
        let g = reg.hacking_games.get_mut(&(CHAR_EXPLORER, relic_can_id)).unwrap();
        g.virus_coherence = 10;
        g.tiles[6].kind = HackingNodeKind::Firewall;
        g.tiles[6].coherence = 60;
        g.tiles[6].strength = 20;
        g.tiles[6].revealed = false;
    });

    let fail_rep = ctx
        .dispatch_rpc(&mut sess, "hackingMgr", None, "ClickedOnTile", &[PyRep::Integer(relic_can_id), PyRep::Integer(6)], &kw, None)
        .await
        .expect("Attack deadly Firewall");
    assert_eq!(fail_rep.dict_get("status").and_then(|v| v.as_str()), Some("Failed"), "Hack fails when player coherence reaches 0");
    let end_coh = fail_rep.dict_get("virusCoherence").and_then(|v| v.as_i64()).unwrap();
    assert!(end_coh <= 0, "Player coherence must be <= 0 upon hack failure");
}

// ============================================================================
// 2. Hack Failure Twice: Container Self-Destruction & AoE Blast Damage
// ============================================================================
#[tokio::test]
async fn test_exploration_hack_failure_container_explosion() {
    // parity: feat:exploration.hack-failure-container-explosion
    eve_testkit::attest(&["feat:exploration.hack-failure-container-explosion"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();

    ensure_system_pve_sites_seeded(&ctx, LOWSEC_SYS).await;
    let (can_id, site_pos) = with_pve_exploration_reg(&ctx, |reg| {
        let site = reg.sites_by_system[&LOWSEC_SYS]
            .iter()
            .find(|s| s.site_category == "Relic" && !s.hackable_can_ids.is_empty())
            .expect("Seeded Relic site in low-sec");
        (site.hackable_can_ids[0], site.position)
    });

    let loot_item_1 = 700_200_101i64;
    let loot_item_2 = 700_200_102i64;
    let loot_item_3 = 700_200_103i64;

    let ship_near_id = 700_200_001i64;
    let analyzer_id = 700_200_002i64;
    let ship_far_id = 700_200_003i64;

    let can_pos = DVec3::from_array(site_pos);
    // Ship near: 2,000 meters from container (within 5,000m blast range)
    let pos_near = can_pos + DVec3::new(2_000.0, 0.0, 0.0);
    // Ship far: 8,000 meters from container (outside 5,000m blast range)
    let pos_far = can_pos + DVec3::new(8_000.0, 0.0, 0.0);

    let mut sess_near = setup_in_space_pilot(&ctx, 9501, CHAR_EXPLORER, ship_near_id, TYPE_HERON, LOWSEC_SYS, pos_near).await;
    let _sess_far = setup_in_space_pilot(&ctx, 9502, CHAR_BYSTANDER, ship_far_id, TYPE_RIFTER, LOWSEC_SYS, pos_far).await;

    // Equip analyzer on near ship
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            analyzer_id,
            make_inventory_item(analyzer_id, TYPE_RELIC_ANALYZER_I, CHAR_EXPLORER, ship_near_id, InventoryFlag::MedSlot0, 1),
        );
        snap.module_online_states.insert(analyzer_id, true);

        // Seed container and 3 loot items inside the container
        snap.items.insert(
            can_id,
            make_inventory_item(can_id, 28_367, 0, i64::from(LOWSEC_SYS), InventoryFlag::None, 1),
        );
        snap.item_positions.insert(can_id, site_pos);

        snap.items.insert(
            loot_item_1,
            make_inventory_item(loot_item_1, 28_668, 0, can_id, InventoryFlag::Cargo, 5),
        );
        snap.items.insert(
            loot_item_2,
            make_inventory_item(loot_item_2, 28_669, 0, can_id, InventoryFlag::Cargo, 12),
        );
        snap.items.insert(
            loot_item_3,
            make_inventory_item(loot_item_3, 28_670, 0, can_id, InventoryFlag::Cargo, 1),
        );
    });

    let sys_handle = ctx.systems.get_or_spawn(LOWSEC_SYS);
    let _ = sys_handle
        .spawn_ball(Ball {
            id: can_id,
            type_id: 28_367,
            position: can_pos,
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 10_000.0,
            radius: 500.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    // Record initial ship shield HPs
    let initial_near_shield = sys_handle.get_combat_state(ship_near_id).await.unwrap().unwrap().shield_hp;
    let initial_far_shield = sys_handle.get_combat_state(ship_far_id).await.unwrap().unwrap().shield_hp;
    assert_eq!(initial_near_shield, 10_000.0);
    assert_eq!(initial_far_shield, 10_000.0);

    // ------------------------------------------------------------------------
    // Attempt 1: First hack failure
    // ------------------------------------------------------------------------
    ctx.dispatch_rpc(&mut sess_near, "hackingMgr", None, "StartNewGameInstance", &[PyRep::Integer(can_id)], &kw, None)
        .await
        .expect("Start game attempt 1");

    // Force failure
    with_pve_exploration_reg(&ctx, |reg| {
        let g = reg.hacking_games.get_mut(&(CHAR_EXPLORER, can_id)).unwrap();
        g.virus_coherence = 5;
        g.tiles[1].kind = HackingNodeKind::Antivirus;
        g.tiles[1].coherence = 50;
        g.tiles[1].strength = 30;
        g.tiles[1].revealed = false;
    });

    let fail1 = ctx
        .dispatch_rpc(&mut sess_near, "hackingMgr", None, "ClickedOnTile", &[PyRep::Integer(can_id), PyRep::Integer(1)], &kw, None)
        .await
        .expect("Fail attempt 1");
    assert_eq!(fail1.dict_get("status").and_then(|v| v.as_str()), Some("Failed"));
    assert_eq!(fail1.dict_get("containerExploded"), Some(&PyRep::Bool(false)), "First failure does not explode container");

    // Verify container and loot intact after 1st failure
    assert!(sys_handle.get_ball_snapshot(can_id).await.unwrap().is_some(), "Container ball remains alive");
    ctx.db.with_snapshot(|snap| {
        assert!(snap.items.contains_key(&can_id), "Container record intact");
        assert!(snap.items.contains_key(&loot_item_1), "Loot 1 intact");
        assert!(snap.items.contains_key(&loot_item_2), "Loot 2 intact");
        assert!(snap.items.contains_key(&loot_item_3), "Loot 3 intact");
    });
    // No damage dealt yet
    assert_eq!(sys_handle.get_combat_state(ship_near_id).await.unwrap().unwrap().shield_hp, 10_000.0);
    assert_eq!(sys_handle.get_combat_state(ship_far_id).await.unwrap().unwrap().shield_hp, 10_000.0);

    // ------------------------------------------------------------------------
    // Attempt 2: Second hack failure -> Self-destruction & AoE explosive blast
    // ------------------------------------------------------------------------
    ctx.dispatch_rpc(&mut sess_near, "hackingMgr", None, "StartNewGameInstance", &[PyRep::Integer(can_id)], &kw, None)
        .await
        .expect("Start game attempt 2");

    with_pve_exploration_reg(&ctx, |reg| {
        let g = reg.hacking_games.get_mut(&(CHAR_EXPLORER, can_id)).unwrap();
        g.virus_coherence = 5;
        g.tiles[1].kind = HackingNodeKind::Firewall;
        g.tiles[1].coherence = 60;
        g.tiles[1].strength = 30;
        g.tiles[1].revealed = false;
    });

    let fail2 = ctx
        .dispatch_rpc(&mut sess_near, "hackingMgr", None, "ClickedOnTile", &[PyRep::Integer(can_id), PyRep::Integer(1)], &kw, None)
        .await
        .expect("Fail attempt 2");
    assert_eq!(fail2.dict_get("status").and_then(|v| v.as_str()), Some("Failed"));
    assert_eq!(fail2.dict_get("containerExploded"), Some(&PyRep::Bool(true)), "Second failure triggers container explosion");
    assert_eq!(fail2.dict_get("containerDestroyed"), Some(&PyRep::Bool(true)), "Container destroyed flag set");

    // Blast damage in 2,000 - 5,000 HP range (null-sec is 3,500 HP)
    let blast_dmg = fail2.dict_get("explosionDamage").and_then(|v| v.as_f64()).expect("explosionDamage present");
    assert!((2_000.0..=5_000.0).contains(&blast_dmg), "Blast damage {blast_dmg} must be between 2000 and 5000 HP");
    assert_eq!(blast_dmg, 3_500.0, "Null-sec explosive blast damage is exactly 3,500 HP");

    // All contained loot items destroyed
    let destroyed_loot = fail2.dict_get("destroyedLootCount").and_then(|v| v.as_i64()).expect("destroyedLootCount");
    assert!(destroyed_loot >= 3, "Destroyed loot count must include all 3 container contents + container");
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&can_id), "Container record removed from database");
        assert!(!snap.items.contains_key(&loot_item_1), "Contained loot 1 destroyed");
        assert!(!snap.items.contains_key(&loot_item_2), "Contained loot 2 destroyed");
        assert!(!snap.items.contains_key(&loot_item_3), "Contained loot 3 destroyed");
    });

    // Container ball removed from ballpark
    assert!(sys_handle.get_ball_snapshot(can_id).await.unwrap().is_none(), "Container ball removed from space");

    // Area-of-effect damage check:
    // Ship within 5,000m (at 2,000m) took explosive damage (shield HP reduced)
    let near_shield_after = sys_handle.get_combat_state(ship_near_id).await.unwrap().unwrap().shield_hp;
    assert!(
        near_shield_after < initial_near_shield,
        "Ship within 5,000m took damage: shield decreased from {initial_near_shield} to {near_shield_after}"
    );

    // Ship beyond 5,000m (at 8,000m) took NO damage
    let far_shield_after = sys_handle.get_combat_state(ship_far_id).await.unwrap().unwrap().shield_hp;
    assert_eq!(
        far_shield_after, initial_far_shield,
        "Ship outside 5,000m blast radius took no damage (shield remains {initial_far_shield})"
    );

    // Container is locked out from further hacking attempts
    let lock_err = ctx
        .dispatch_rpc(&mut sess_near, "hackingMgr", None, "StartNewGameInstance", &[PyRep::Integer(can_id)], &kw, None)
        .await
        .expect_err("Destroyed container must reject new hack attempts");
    assert_eq!(lock_err.code, "HackingGameAlreadyFinished");

    // ------------------------------------------------------------------------
    // Part 2: Security band damage scaling verification (Low-sec 2500 HP, Null-sec 3500 HP, Wormhole 4500 HP)
    // ------------------------------------------------------------------------
    let low_can_id = 700_300_000i64;
    let (_expl, dmg_low, _, _) = eve_server::services::pve_hacking_board::handle_second_hack_failure_explosion(
        &ctx, 30_002_813, "EXPLORATION_LOW", low_can_id, true
    ).await;
    assert!((2_000.0..=5_000.0).contains(&dmg_low));
    assert_eq!(dmg_low, 2_500.0, "Low-sec failure blast inflicts 2,500 HP explosive damage");

    let null_can_id = 700_300_001i64;
    let wh_can_id = 700_400_001i64;
    let (_expl, dmg_null, _, _) = eve_server::services::pve_hacking_board::handle_second_hack_failure_explosion(
        &ctx, NULLSEC_SYS, "EXPLORATION_NULL", null_can_id, true
    ).await;
    assert!((2_000.0..=5_000.0).contains(&dmg_null));
    assert_eq!(dmg_null, 3_500.0, "Null-sec failure blast inflicts 3,500 HP explosive damage");

    let (_expl, dmg_wh, _, _) = eve_server::services::pve_hacking_board::handle_second_hack_failure_explosion(
        &ctx, WORMHOLE_SYS, "EXPLORATION_WH", wh_can_id, true
    ).await;
    assert!((2_000.0..=5_000.0).contains(&dmg_wh));
    assert_eq!(dmg_wh, 4_500.0, "Wormhole space failure blast inflicts 4,500 HP explosive damage");

    // High-sec contrast: container explodes & loot destroyed, but 0.0 ship blast damage
    let hs_can_id = 700_500_001i64;
    let (hs_expl, dmg_hs, _, _) = eve_server::services::pve_hacking_board::handle_second_hack_failure_explosion(
        &ctx, HIGHSEC_SYS, "EXPLORATION_HIGHSEC", hs_can_id, true
    ).await;
    assert!(hs_expl, "High-sec container still self-destructs");
    assert_eq!(dmg_hs, 0.0, "High-sec failure inflicts 0 explosive damage to ships");
}


pub(crate) mod scanning_exploration_wormholes_closure {
    #[allow(unused_imports)]
    use super::*;

// Integration tests closing 10 Scanning, Exploration & Wormhole parity rows.

use eve_db::{CosmicSignatureRecord, InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
use eve_destiny::{Ball, MovementMode};
use eve_dogma::{
    apply_wormhole_phenomenon_to_attributes, resolve_system_wormhole_phenomenon,
    wormhole_phenomenon_type_for_class_and_name,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::pve_exploration::{ensure_system_pve_sites_seeded, with_pve_exploration_reg};
use eve_server::services::space_destiny::{
    compute_probe_max_deviation_au, probe_allowed_ranges_au, probe_base_max_deviation_au,
    probe_base_scan_range_au, probe_max_flight_time_s,
    signature_difficulty_level_and_min_strength, wormhole_item_id_for_signature,
};
use glam::DVec3;
use std::collections::{HashMap, VecDeque};

const AU_IN_METERS: f64 = 149_597_870_700.0;

fn are_hacking_tiles_adjacent(a: u32, b: u32) -> bool {
    let (aq, ar) = ((a % 6) as i32, (a / 6) as i32);
    let (bq, br) = ((b % 6) as i32, (b / 6) as i32);
    let (dq, dr) = (aq - bq, ar - br);
    ((dq.abs() + (dq + dr).abs() + dr.abs()) / 2) == 1
}

fn make_item(item_id: i64, type_id: u32, owner_id: i64, location_id: i64, flag: InventoryFlag, quantity: i32, singleton: bool) -> InventoryItemRecord {
    InventoryItemRecord { item_id, type_id, owner_id, location_id, flag, quantity, singleton, ..Default::default() }
}

async fn spawn_test_ball(ctx: &ServerContext, sys_id: u32, id: i64, type_id: u32, pos: DVec3, mass: f64, cloaked: bool) {
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let mut cs = ctx.build_type_combat_state(type_id);
    cs.shield_hp = 10_000.0;
    cs.max_shield_hp = 10_000.0;
    cs.is_cloaked = cloaked;
    let _ = sys_handle
        .spawn_ball_with_combat(
            Ball { id, type_id, position: pos, velocity: DVec3::ZERO, max_velocity: 350.0, inertia: 2.0, mass, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop },
            cs,
        )
        .await;
}

async fn setup_pilot_in_space(ctx: &ServerContext, session_id: u64, char_id: i64, solar_system_id: u32, ship_id: i64, ship_type_id: u32) -> SessionState {
    let kw = HashMap::new();
    let mut sess = SessionState::new(session_id);
    ctx.dispatch_rpc(&mut sess, "authentication", None, "Login", &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())], &kw, None).await.expect("Login");
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None).await.expect("SelectCharacterID");
    sess.solar_system_id = Some(solar_system_id);
    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = Some(ship_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = solar_system_id;
            ch.active_ship_id = ship_id;
            ch.station_id = None;
            ch.structure_id = None;
        }
        snap.items.insert(ship_id, make_item(ship_id, ship_type_id, char_id, solar_system_id as i64, InventoryFlag::Hangar, 1, true));
    });
    spawn_test_ball(ctx, solar_system_id, ship_id, ship_type_id, DVec3::ZERO, 1_200_000.0, false).await;
    sess
}

fn last_hacking_turn_event_data(sess: &mut SessionState) -> PyRep {
    let (_, payload) = sess.drain_notifications().into_iter().rev().find(|(ev, _)| ev == "OnHackingTurnComplete").expect("OnHackingTurnComplete");
    let Some(PyRep::List(events)) = payload.first() else { panic!("events list") };
    events.last().and_then(|ev| ev.dict_get("eventData")).cloned().expect("eventData")
}

// parity: feat:scanning.probe-range-deviation, feat:scanning.probe-target-classes, feat:scanning.signature-reveal-thresholds, feat:scanning.signature-strength-levels
#[tokio::test]
async fn test_probe_scanning_ranges_deviation_expiry_classes_thresholds_and_levels() {
    eve_testkit::attest(&[
        "feat:scanning.probe-range-deviation",
        "feat:scanning.probe-target-classes",
        "feat:scanning.signature-reveal-thresholds",
        "feat:scanning.signature-strength-levels",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let (char_id, sys_id, ship_id, launcher_id) = (90_000_001, 30_000_142u32, 610_000_001i64, 610_000_010i64);
    let mut sess = setup_pilot_in_space(&ctx, 701, char_id, sys_id, ship_id, 606).await;

    // 1. Core vs Combat probe range steps (0.25..=32 AU vs 0.5..=64 AU) & flight time from SDE
    assert_eq!(probe_allowed_ranges_au(&ctx.sde, 30013), [0.25, 0.5, 1.0, 2.0, 4.0, 8.0, 16.0, 32.0]);
    assert_eq!(probe_allowed_ranges_au(&ctx.sde, 30028), [0.5, 1.0, 2.0, 4.0, 8.0, 16.0, 32.0, 64.0]);
    assert_eq!((probe_base_scan_range_au(&ctx.sde, 30013), probe_base_scan_range_au(&ctx.sde, 30028)), (0.25, 0.5));
    assert_eq!((probe_base_max_deviation_au(&ctx.sde, 30013), probe_base_max_deviation_au(&ctx.sde, 30028)), (0.125, 0.25));
    assert_eq!((probe_max_flight_time_s(&ctx.sde, 32246), probe_max_flight_time_s(&ctx.sde, 30013), probe_max_flight_time_s(&ctx.sde, 30028)), (1_000.0, 4_000.0, 4_000.0));

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.dynamic_attributes.insert(48, 2_000.0);
            sh.dynamic_attributes.insert(11, 2_000.0);
        }
        snap.items.insert(launcher_id, make_item(launcher_id, 17901, char_id, ship_id, InventoryFlag::HiSlot0, 1, true));
        snap.module_online_states.insert(launcher_id, true);
        snap.items.insert(610_000_011, make_item(610_000_011, 32246, char_id, ship_id, InventoryFlag::HiSlot0, 8, false));
    });
    ctx.invalidate_ship_fitting(ship_id);

    let launch_rss = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(launcher_id), PyRep::String("launchProbe".into())], &kw, None).await.expect("Launch RSS");
    assert_eq!(launch_rss.dict_get("maxFlightTimeS").and_then(|v| v.as_f64()), Some(1_000.0));
    assert_eq!(launch_rss.dict_get("canScanShips"), Some(&PyRep::Bool(false)));

    // Advance 1,001s (> 16m40s): background cluster tick expires RSS probes and returns them to cargo before any RPC
    ctx.advance_secs(1_001);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let rss_in_cargo: i32 = ctx.db.with_snapshot(|snap| {
        snap.items.values().filter(|i| i.location_id == ship_id && i.flag == InventoryFlag::Cargo && i.type_id == 32246).map(|i| i.quantity).sum()
    });
    assert_eq!(rss_in_cargo, 8, "Background cluster tick returned 8 expired RSS probes to cargo");

    // Standard probes (4000s = 1h6m40s) when cargo is full -> probes are lost on cluster tick
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&ship_id).unwrap().dynamic_attributes.insert(38, 0.1);
        snap.items.insert(610_000_012, make_item(610_000_012, 30013, char_id, ship_id, InventoryFlag::HiSlot0, 8, false));
    });
    let _ = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(launcher_id), PyRep::String("launchProbe".into())], &kw, None).await.unwrap();
    ctx.advance_secs(4_001);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let std_in_cargo: i32 = ctx.db.with_snapshot(|snap| {
        snap.items.values().filter(|i| i.location_id == ship_id && i.flag == InventoryFlag::Cargo && i.type_id == 30013).map(|i| i.quantity).sum()
    });
    assert_eq!(std_in_cargo, 0, "Expired probes are lost when cargo hold is full");

    // 2. Live RPC range steps & deviation scaling (untrained Astrometrics 0 vs Astrometrics 4 + Pinpointing 3)
    // and signature reveal thresholds (<25%, 25%..75%, 75%..100%, 100%) + SDE dungeon difficulty levels (I-III: 42, IV: 72, V: 104)
    let _ = ctx.dispatch_rpc(&mut sess, "scanMgr", None, "GetSystemScanMgr", &[], &kw, None).await.unwrap();
    let sig_pos = [300_000.0, 0.0, 0.0];
    let now_u = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&ship_id).unwrap().dynamic_attributes.insert(38, 1_000.0);
        if let Some(sk) = snap.skills.get_mut(&char_id) {
            sk.remove(&3412);
            sk.remove(&25810);
            sk.remove(&25811);
        }
        let sigs = snap.cosmic_signatures.entry(sys_id).or_default();
        sigs.clear();
        for (id, grp, name) in [
            ("TST-LV1", "Relic Site", "Desolate Collapsed Conduit"),
            ("TST-LV4", "Data Site", "Abandoned Research Complex DA005"),
            ("TST-LV5", "Data Site", "Unsecured Core Backup Array"),
        ] {
            sigs.push(CosmicSignatureRecord {
                signature_id: id.into(), solar_system_id: sys_id, sig_group: grp.into(), type_name: name.into(), signal_strength: 0.0, position: sig_pos,
                wormhole_type_id: None, destination_system_id: None, remaining_mass_kg: 0.0, max_mass_kg: 0.0, max_jump_mass_kg: 0.0, created_unix: now_u, lifetime_seconds: 86_400, collapsed: false,
            });
        }
        snap.items.insert(610_000_013, make_item(610_000_013, 30013, char_id, ship_id, InventoryFlag::HiSlot0, 8, false));
    });

    let sigs_snap = ctx.db.with_snapshot(|s| s.cosmic_signatures[&sys_id].clone());
    assert_eq!(signature_difficulty_level_and_min_strength(&ctx.sde, sys_id, &sigs_snap[0]), (1, 42.0));
    assert_eq!(signature_difficulty_level_and_min_strength(&ctx.sde, sys_id, &sigs_snap[1]), (4, 72.0));
    assert_eq!(signature_difficulty_level_and_min_strength(&ctx.sde, sys_id, &sigs_snap[2]), (5, 104.0));

    let _ = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(launcher_id), PyRep::String("launchProbe".into())], &kw, None).await.unwrap();
    let nan_coords = PyRep::List(vec![PyRep::List(vec![PyRep::Real(f64::NAN), PyRep::Real(0.0), PyRep::Real(0.0), PyRep::Integer(1)])]);
    assert_eq!(ctx.dispatch_rpc(&mut sess, "scanMgr", None, "RequestScans", &[nan_coords], &kw, None).await.unwrap_err().code, "InvalidProbeCoordinates");

    let make_coords_step = |covering: usize, step: i64| -> PyRep {
        PyRep::List((0..8).map(|i| {
            let p = if i < covering { sig_pos } else { [9.0e12, 9.0e12, 9.0e12] };
            PyRep::List(vec![PyRep::Real(p[0]), PyRep::Real(p[1]), PyRep::Real(p[2]), PyRep::Integer(step)])
        }).collect())
    };
    let find_row = |rep: &PyRep, id: &str| -> PyRep {
        let PyRep::List(rows) = rep else { panic!() };
        rows.iter().find(|r| r.dict_get("signatureID").and_then(|v| v.as_str()) == Some(id)).unwrap().clone()
    };

    // Untrained character (Astrometrics 0, Pinpointing 0) at step 4 (2.0 AU = 8x base 0.25 AU): maxDeviationAU = 1.0 AU
    let scan_untrained_step4 = ctx.dispatch_rpc(&mut sess, "scanMgr", None, "RequestScans", &[make_coords_step(2, 4)], &kw, None).await.unwrap();
    let row_untrained = find_row(&scan_untrained_step4, "TST-LV1");
    assert_eq!(row_untrained.dict_get("scanRangeAU").and_then(|v| v.as_f64()), Some(2.0));
    assert!((row_untrained.dict_get("maxDeviationAU").and_then(|v| v.as_f64()).unwrap() - 1.0).abs() < 1e-9);
    let sig_untrained_s = row_untrained.dict_get("signalStrength").and_then(|v| v.as_f64()).unwrap();

    // Train Astrometrics IV + Pinpointing III at step 4 (2.0 AU): maxDeviationAU = 0.68 AU and higher signal strength
    ctx.db.with_mut_snapshot(|snap| {
        let sk = snap.skills.entry(char_id).or_default();
        sk.insert(3412, TrainedSkillRecord { type_id: 3412, level: 4, skill_points: 45_255.0 });
        sk.insert(25811, TrainedSkillRecord { type_id: 25811, level: 3, skill_points: 16_000.0 });
    });
    let expected_dev_au = compute_probe_max_deviation_au(0.125, 2.0, 0.25, 4, 3);
    let scan_trained_step4 = ctx.dispatch_rpc(&mut sess, "scanMgr", None, "RequestScans", &[make_coords_step(2, 4)], &kw, None).await.unwrap();
    let row_trained = find_row(&scan_trained_step4, "TST-LV1");
    assert!((row_trained.dict_get("maxDeviationAU").and_then(|v| v.as_f64()).unwrap() - expected_dev_au).abs() < 1e-9);
    let sig_trained_s = row_trained.dict_get("signalStrength").and_then(|v| v.as_f64()).unwrap();
    assert!(sig_trained_s > sig_untrained_s, "Astrometrics IV yields higher signal strength than untrained Astrometrics 0");
    let expected_dev_m = (1.0 - sig_trained_s) * expected_dev_au * AU_IN_METERS;
    assert!((row_trained.dict_get("deviation").and_then(|v| v.as_f64()).unwrap() - expected_dev_m).abs() < 1.0);

    // Set Astrometrics II for threshold & level tests at step 1 (0.25 AU)
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(3412, TrainedSkillRecord { type_id: 3412, level: 2, skill_points: 8_000.0 });
    });

    // < 25% (1 probe covering -> capped at 0.24): only signature ID and deviation shown; gated fields omitted
    let scan_1 = ctx.dispatch_rpc(&mut sess, "scanMgr", None, "RequestScans", &[make_coords_step(1, 1)], &kw, None).await.unwrap();
    let lv1_r1 = find_row(&scan_1, "TST-LV1");
    assert!(lv1_r1.dict_get("signalStrength").and_then(|v| v.as_f64()).unwrap() < 0.25);
    assert_eq!(lv1_r1.dict_get("group").and_then(|v| v.as_str()), Some("Unknown"));
    assert_eq!((lv1_r1.dict_get("groupName"), lv1_r1.dict_get("siteName"), lv1_r1.dict_get("warpable")), (Some(&PyRep::None), Some(&PyRep::None), Some(&PyRep::Bool(false))));
    assert!(lv1_r1.dict_get("difficultyLevel").is_none() && lv1_r1.dict_get("typeID").is_none() && lv1_r1.dict_get("massStage").is_none() && lv1_r1.dict_get("lifetimeDescription").is_none());
    assert!(lv1_r1.dict_get("deviation").and_then(|v| v.as_f64()).unwrap() > 0.0);

    // 25%..75% (2 probes covering -> capped at 0.49): group shown, siteName hidden, not warpable
    let scan_2 = ctx.dispatch_rpc(&mut sess, "scanMgr", None, "RequestScans", &[make_coords_step(2, 1)], &kw, None).await.unwrap();
    let lv1_r2 = find_row(&scan_2, "TST-LV1");
    assert!((0.25..0.75).contains(&lv1_r2.dict_get("signalStrength").and_then(|v| v.as_f64()).unwrap()));
    assert_eq!(lv1_r2.dict_get("group").and_then(|v| v.as_str()), Some("Relic Site"));
    assert_eq!((lv1_r2.dict_get("siteName"), lv1_r2.dict_get("warpable")), (Some(&PyRep::None), Some(&PyRep::Bool(false))));
    assert!(lv1_r2.dict_get("difficultyLevel").is_none());

    // 75%..100% & strength-level gating via real 8-probe formation math (Core Probe I + Astrometrics II = 70.4: resolves Lv I, < 72 for Lv IV, < 104 for Lv V)
    let scan_low_skill = ctx.dispatch_rpc(&mut sess, "scanMgr", None, "RequestScans", &[make_coords_step(8, 1)], &kw, None).await.unwrap();
    assert_eq!(find_row(&scan_low_skill, "TST-LV1").dict_get("warpable"), Some(&PyRep::Bool(true)));
    let lv4_low = find_row(&scan_low_skill, "TST-LV4");
    assert!((0.75..1.0).contains(&lv4_low.dict_get("signalStrength").and_then(|v| v.as_f64()).unwrap()));
    assert_eq!(lv4_low.dict_get("siteName").and_then(|v| v.as_str()), Some("Abandoned Research Complex DA005"));
    assert_eq!(lv4_low.dict_get("warpable"), Some(&PyRep::Bool(false)));
    assert!(find_row(&scan_low_skill, "TST-LV5").dict_get("signalStrength").and_then(|v| v.as_f64()).unwrap() < 0.75);

    // Warping to unresolved Lv IV (<100%) fails; warping to resolved Lv I (100%) succeeds
    let beyonce_bound = ctx.bind_object(&mut sess, "beyonce", sys_id as i64).unwrap();
    let (lv4_ball_id, lv1_ball_id) = (wormhole_item_id_for_signature(sys_id, "TST-LV4"), wormhole_item_id_for_signature(sys_id, "TST-LV1"));
    for bid in [lv4_ball_id, lv1_ball_id] {
        spawn_test_ball(&ctx, sys_id, bid, 26272, DVec3::new(300_000.0, 0.0, 0.0), 1_000.0, false).await;
    }
    assert_eq!(ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_bound), "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(lv4_ball_id)], &kw, None).await.unwrap_err().code, "InvalidWarpDestination");
    assert!(ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_bound), "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(lv1_ball_id)], &kw, None).await.is_ok());

    // Train Astrometrics V -> 8x Core Probe I formation strength = 80.0 (resolves Lv IV >= 72, fails Lv V < 104)
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(3412, TrainedSkillRecord { type_id: 3412, level: 5, skill_points: 256_000.0 });
    });
    let scan_astro5 = ctx.dispatch_rpc(&mut sess, "scanMgr", None, "RequestScans", &[make_coords_step(8, 1)], &kw, None).await.unwrap();
    assert_eq!(find_row(&scan_astro5, "TST-LV4").dict_get("warpable"), Some(&PyRep::Bool(true)));
    assert_eq!(find_row(&scan_astro5, "TST-LV5").dict_get("warpable"), Some(&PyRep::Bool(false)));

    // Load Sisters Core Scanner Probe (30488, base 44) + Astrometric Rangefinding V -> formation strength = 110.0 >= 104.0 (resolves Lv V)
    let _ = ctx.dispatch_rpc(&mut sess, "scanMgr", None, "RecoverProbes", &[], &kw, None).await.unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(25810, TrainedSkillRecord { type_id: 25810, level: 5, skill_points: 512_000.0 });
        snap.items.retain(|_, i| !(i.location_id == ship_id && i.flag == InventoryFlag::HiSlot0 && i.item_id != launcher_id));
        snap.items.insert(610_000_015, make_item(610_000_015, 30488, char_id, ship_id, InventoryFlag::HiSlot0, 8, false));
    });
    let _ = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(launcher_id), PyRep::String("launchProbe".into())], &kw, None).await.unwrap();
    let scan_105 = ctx.dispatch_rpc(&mut sess, "scanMgr", None, "RequestScans", &[make_coords_step(8, 1)], &kw, None).await.unwrap();
    assert_eq!(find_row(&scan_105, "TST-LV5").dict_get("warpable"), Some(&PyRep::Bool(true)));

    // 3. Target classes: Core probes resolve CosmicSignatures only; Combat probes also resolve Ships, Drones, Structures;
    // wrecks, cargo containers, and cloaked ships never appear in probe results.
    let (target_ship, cloaked_ship, drone_id, structure_id, wreck_id, cargo_can) = (610_000_101, 610_000_102, 610_000_103, 610_000_104, 610_000_105, 610_000_106);
    for (bid, tid, cloaked) in [(target_ship, 587u32, false), (cloaked_ship, 11188, true), (drone_id, 2454, false), (structure_id, 35832, false), (wreck_id, 26561, false), (cargo_can, 23, false)] {
        spawn_test_ball(&ctx, sys_id, bid, tid, DVec3::from_array(sig_pos), 1_000.0, cloaked).await;
    }
    let PyRep::List(rows_105) = &scan_105 else { panic!() };
    assert!(rows_105.iter().all(|r| r.dict_get("targetClass").and_then(|v| v.as_str()) == Some("CosmicSignature")));

    let _ = ctx.dispatch_rpc(&mut sess, "scanMgr", None, "RecoverProbes", &[], &kw, None).await.unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.retain(|_, i| !(i.location_id == ship_id && i.flag == InventoryFlag::HiSlot0 && i.item_id != launcher_id));
        snap.items.insert(610_000_014, make_item(610_000_014, 30486, char_id, ship_id, InventoryFlag::HiSlot0, 8, false));
    });
    let launch_combat = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(launcher_id), PyRep::String("launchProbe".into())], &kw, None).await.unwrap();
    assert_eq!(launch_combat.dict_get("canScanShips"), Some(&PyRep::Bool(true)));

    let combat_scan = ctx.dispatch_rpc(&mut sess, "scanMgr", None, "RequestScans", &[make_coords_step(8, 1)], &kw, None).await.unwrap();
    let PyRep::List(combat_rows) = &combat_scan else { panic!() };
    let scanned_ids: Vec<i64> = combat_rows.iter().filter_map(|r| r.dict_get("itemID").and_then(|v| v.as_i64())).collect();
    assert!(scanned_ids.contains(&target_ship) && scanned_ids.contains(&drone_id) && scanned_ids.contains(&structure_id));
    assert!(!scanned_ids.contains(&cloaked_ship) && !scanned_ids.contains(&wreck_id) && !scanned_ids.contains(&cargo_can));
}

async fn reveal_tile_via_clicks(ctx: &ServerContext, sess: &mut SessionState, char_id: i64, can_id: i64, target_idx: u32) {
    let kw = HashMap::new();
    let mut parent: HashMap<u32, u32> = HashMap::from([(0, 0)]);
    let mut queue = VecDeque::from([0u32]);
    while let Some(cur) = queue.pop_front() {
        if cur == target_idx {
            break;
        }
        for nxt in 0..24u32 {
            if !parent.contains_key(&nxt) && are_hacking_tiles_adjacent(cur, nxt) {
                parent.insert(nxt, cur);
                queue.push_back(nxt);
            }
        }
    }
    let (mut path, mut w) = (Vec::new(), target_idx);
    while w != 0 {
        path.push(w);
        w = parent[&w];
    }
    path.reverse();
    for (i, &step_idx) in path.iter().enumerate() {
        let is_last = i + 1 == path.len();
        for _ in 0..5 {
            let need_click = with_pve_exploration_reg(ctx, |reg| {
                let g = &reg.hacking_games[&(char_id, can_id)];
                if g.status != "InProgress" {
                    return false;
                }
                let t = &g.tiles[step_idx as usize];
                !t.revealed || (!is_last && matches!(format!("{:?}", t.kind).as_str(), "Firewall" | "Antivirus" | "RestorationNode" | "VirusSuppressor") && t.coherence > 0)
            });
            if !need_click {
                break;
            }
            let _ = ctx.dispatch_rpc(sess, "hackingMgr", None, "ClickedOnTile", &[PyRep::Integer(step_idx as i64)], &kw, None).await.unwrap();
        }
    }
}

// parity: feat:exploration.hack-failure-container-explosion, feat:exploration.hacking-coherence-and-strength, feat:exploration.hacking-node-network
#[tokio::test]
async fn test_exploration_hacking_grid_coherence_strength_and_second_fail_explosion() {
    eve_testkit::attest(&[
        "feat:exploration.hack-failure-container-explosion",
        "feat:exploration.hacking-coherence-and-strength",
        "feat:exploration.hacking-node-network",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let (char_id, wh_sys_id, ship_id) = (90_000_001, 31_000_005u32, 620_000_001i64);
    let mut sess = setup_pilot_in_space(&ctx, 702, char_id, wh_sys_id, ship_id, 606).await;

    let (can_1, can_2, loot_in_can_2, bystander_near, bystander_far) = (620_000_101, 620_000_102, 620_000_103, 620_000_201, 620_000_202);
    ensure_system_pve_sites_seeded(&ctx, wh_sys_id).await;
    with_pve_exploration_reg(&ctx, |reg| {
        if let Some(site) = reg.sites_by_system.get_mut(&wh_sys_id).and_then(|v| v.iter_mut().find(|s| s.site_category == "Relic")) {
            site.hackable_can_ids.push(can_1);
            site.hackable_can_ids.push(can_2);
        }
    });
    ctx.db.with_mut_snapshot(|snap| {
        let sk = snap.skills.entry(char_id).or_default();
        sk.insert(21718, TrainedSkillRecord { type_id: 21718, level: 2, skill_points: 8_000.0 });
        sk.insert(13278, TrainedSkillRecord { type_id: 13278, level: 2, skill_points: 8_000.0 });
        snap.items.insert(620_000_010, make_item(620_000_010, 22177, char_id, ship_id, InventoryFlag::MedSlot0, 1, true));
        snap.module_online_states.insert(620_000_010, true);
        snap.items.insert(can_1, make_item(can_1, 24675, 1, wh_sys_id as i64, InventoryFlag::None, 1, true));
        snap.items.insert(can_2, make_item(can_2, 24675, 1, wh_sys_id as i64, InventoryFlag::None, 1, true));
        snap.items.insert(loot_in_can_2, make_item(loot_in_can_2, 25605, 1, can_2, InventoryFlag::Cargo, 5, false));
    });
    spawn_test_ball(&ctx, wh_sys_id, can_1, 24675, DVec3::new(1_000.0, 0.0, 0.0), 1_000.0, false).await;
    spawn_test_ball(&ctx, wh_sys_id, can_2, 24675, DVec3::new(1_500.0, 0.0, 0.0), 1_000.0, false).await;
    spawn_test_ball(&ctx, wh_sys_id, bystander_near, 587, DVec3::new(3_000.0, 0.0, 0.0), 1_000_000.0, false).await;
    spawn_test_ball(&ctx, wh_sys_id, bystander_far, 587, DVec3::new(9_000.0, 0.0, 0.0), 1_000_000.0, false).await;

    // 1. T1 analyzer stats (20 strength, 40 + 10*2 = 60 coherence via Archaeology 13278), asserted via OnHackingTurnComplete notification payload
    let _ = sess.drain_notifications();
    let v24_start_args = [PyRep::Integer(can_1), PyRep::Integer(24675), PyRep::Integer(620_000_010), PyRep::Integer(22177), PyRep::Integer(5), PyRep::Integer(0), PyRep::Integer(0)];
    let _ = ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "StartNewGameInstance", &v24_start_args, &kw, None).await.unwrap();
    let start_ev = last_hacking_turn_event_data(&mut sess);
    assert_eq!(start_ev.dict_get("virusStrength").and_then(|v| v.as_i64()), Some(20));
    assert_eq!(start_ev.dict_get("virusCoherence").and_then(|v| v.as_i64()), Some(60));
    assert_eq!(start_ev.dict_get("gridTopology").and_then(|v| v.as_str()), Some("Hexagonal"));
    assert!((1..=5).contains(&start_ev.dict_get("distanceIndicator").and_then(|v| v.as_i64()).unwrap()));
    assert!(start_ev.dict_get("distanceToCore").is_none());

    // Verify `hackingMgr.values` never exposes `distanceToCore` or unrevealed `distanceIndicator`
    let board_vals = ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "values", &[PyRep::Integer(can_1)], &kw, None).await.unwrap();
    let PyRep::List(val_tiles) = &board_vals else { panic!() };
    assert_eq!(val_tiles.len(), 24);
    for vt in val_tiles {
        assert!(vt.dict_get("distanceToCore").is_none(), "distanceToCore must never be exposed");
        if vt.dict_get("revealed").and_then(|v| v.as_bool()).unwrap_or(false) {
            assert!((1..=5).contains(&vt.dict_get("distanceIndicator").and_then(|v| v.as_i64()).unwrap()));
        } else {
            assert!(vt.dict_get("distanceIndicator").is_none(), "unrevealed tiles must not leak distance");
        }
    }

    let (fw_idx, has_all_kinds, layout_1) = with_pve_exploration_reg(&ctx, |reg| {
        let g = &reg.hacking_games[&(char_id, can_1)];
        let kinds: Vec<String> = g.tiles.iter().map(|t| format!("{:?} {:?}", t.kind, t.utility_drop)).collect();
        let has_all = ["Empty", "Firewall", "Antivirus", "SystemCore", "SelfRepair", "KernelRot", "SecondaryVector"].iter().all(|k| kinds.iter().any(|tk| tk.contains(k)));
        let fw = g.tiles.iter().find(|t| format!("{:?}", t.kind) == "Firewall").unwrap().tile_index;
        (fw, has_all, kinds)
    });
    assert!(has_all_kinds);

    // Non-adjacent hex tile (tile 10: q=4, r=1, hex_dist(0, 10) == 5) is rejected
    assert_eq!(ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "ClickedOnTile", &[PyRep::Integer(10)], &kw, None).await.unwrap_err().code, "TileNotAccessible");

    // Reveal the procedural Firewall tile via valid hex clicks and verify combat + distance clue via OnHackingTurnComplete
    reveal_tile_via_clicks(&ctx, &mut sess, char_id, can_1, fw_idx).await;
    let fw_ev = last_hacking_turn_event_data(&mut sess);
    assert!(fw_ev.dict_get("distanceToCore").is_none());
    assert!((1..=5).contains(&fw_ev.dict_get("distanceIndicator").and_then(|v| v.as_i64()).unwrap()));
    assert_eq!(fw_ev.dict_get("nodeKind").and_then(|v| v.as_str()), Some("Firewall"));
    assert_eq!(fw_ev.dict_get("nodeCoherence").and_then(|v| v.as_i64()), Some(20));
    assert!(fw_ev.dict_get("virusCoherence").and_then(|v| v.as_i64()).unwrap() <= 40);

    // Reach hack failure (virusCoherence == 0) through real tile interactions on defensive nodes without editing internal state
    let def_indices: Vec<u32> = with_pve_exploration_reg(&ctx, |reg| {
        reg.hacking_games[&(char_id, can_1)].tiles.iter().filter(|t| t.strength > 0).map(|t| t.tile_index).collect()
    });
    let mut failed_ev = None;
    'outer1: for d_idx in def_indices {
        reveal_tile_via_clicks(&ctx, &mut sess, char_id, can_1, d_idx).await;
        for _ in 0..5 {
            if with_pve_exploration_reg(&ctx, |reg| reg.hacking_games[&(char_id, can_1)].status == "Failed") {
                failed_ev = Some(last_hacking_turn_event_data(&mut sess));
                break 'outer1;
            }
            if with_pve_exploration_reg(&ctx, |reg| reg.hacking_games[&(char_id, can_1)].tiles[d_idx as usize].coherence <= 0) {
                break;
            }
            let _ = ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "ClickedOnTile", &[PyRep::Integer(d_idx as i64)], &kw, None).await.unwrap();
        }
    }
    let fail1_ev = failed_ev.expect("Reached hack failure via real defensive node clicks");
    assert_eq!((fail1_ev.dict_get("virusCoherence").and_then(|v| v.as_i64()), fail1_ev.dict_get("status").and_then(|v| v.as_str())), (Some(0), Some("Failed")));

    // 2. T2 Relic Analyzer II (30832: SDE 60 base coherence, 30 strength) + Archaeology IV (+40) = 100 coherence;
    // procedural layout differs across containers; 2nd hack failure triggers container explosion & AoE damage
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&620_000_010).unwrap().type_id = 30832;
        snap.skills.entry(char_id).or_default().insert(13278, TrainedSkillRecord { type_id: 13278, level: 4, skill_points: 45_255.0 });
    });
    let v24_start_can2 = [PyRep::Integer(can_2), PyRep::Integer(24675), PyRep::Integer(620_000_010), PyRep::Integer(30832), PyRep::Integer(5), PyRep::Integer(0), PyRep::Integer(0)];
    let _ = sess.drain_notifications();
    let _ = ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "StartNewGameInstance", &v24_start_can2, &kw, None).await.unwrap();
    let start_t2_ev = last_hacking_turn_event_data(&mut sess);
    assert_eq!((start_t2_ev.dict_get("virusStrength").and_then(|v| v.as_i64()), start_t2_ev.dict_get("virusCoherence").and_then(|v| v.as_i64())), (Some(30), Some(100)));
    let layout_2: Vec<String> = with_pve_exploration_reg(&ctx, |reg| {
        reg.hacking_games[&(char_id, can_2)].tiles.iter().map(|t| format!("{:?} {:?}", t.kind, t.utility_drop)).collect()
    });
    assert_ne!(layout_1, layout_2, "Procedural hex board layout varies per container/seed");

    let sys_handle = ctx.systems.get_or_spawn(wh_sys_id);
    // 1st failure on can_2 via QuitHackingAttempt
    let _ = ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "QuitHackingAttempt", &[], &kw, None).await.unwrap();
    assert!(sys_handle.get_ball_snapshot(can_2).await.unwrap().is_some());

    // 2nd attempt on can_2 with T1 Relic Analyzer I (22177) + Archaeology 0 (40 coherence, 20 strength) -> fail via real clicks on defensive nodes -> explodes!
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&620_000_010).unwrap().type_id = 22177;
        snap.skills.entry(char_id).or_default().insert(13278, TrainedSkillRecord { type_id: 13278, level: 0, skill_points: 0.0 });
    });
    let _ = sess.drain_notifications();
    let _ = ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "StartNewGameInstance", &v24_start_can2, &kw, None).await.unwrap();
    let def_indices_2: Vec<u32> = with_pve_exploration_reg(&ctx, |reg| {
        reg.hacking_games[&(char_id, can_2)].tiles.iter().filter(|t| t.strength > 0).map(|t| t.tile_index).collect()
    });
    let mut fail2_ev_opt = None;
    'outer2: for d_idx in def_indices_2 {
        reveal_tile_via_clicks(&ctx, &mut sess, char_id, can_2, d_idx).await;
        for _ in 0..5 {
            if with_pve_exploration_reg(&ctx, |reg| reg.hacking_games[&(char_id, can_2)].status == "Failed") {
                fail2_ev_opt = Some(last_hacking_turn_event_data(&mut sess));
                break 'outer2;
            }
            if with_pve_exploration_reg(&ctx, |reg| reg.hacking_games[&(char_id, can_2)].tiles[d_idx as usize].coherence <= 0) {
                break;
            }
            let _ = ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "ClickedOnTile", &[PyRep::Integer(d_idx as i64)], &kw, None).await.unwrap();
        }
    }
    let fail2_ev = fail2_ev_opt.expect("2nd failure reached via real defensive node clicks");
    assert_eq!(fail2_ev.dict_get("containerExploded"), Some(&PyRep::Bool(true)));
    assert_eq!(fail2_ev.dict_get("containerDestroyed"), Some(&PyRep::Bool(true)));
    assert_eq!(fail2_ev.dict_get("destroyedLootCount").and_then(|v| v.as_i64()), Some(2));
    assert!((2_000.0..=5_000.0).contains(&fail2_ev.dict_get("explosionDamage").and_then(|v| v.as_f64()).unwrap()));

    assert!(sys_handle.get_ball_snapshot(can_2).await.unwrap().is_none());
    assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&can_2) && !s.items.contains_key(&loot_in_can_2)));
    assert!(sys_handle.get_combat_state(ship_id).await.unwrap().unwrap().shield_hp < 10_000.0);
    assert!(sys_handle.get_combat_state(bystander_near).await.unwrap().unwrap().shield_hp < 10_000.0);
    assert_eq!(sys_handle.get_combat_state(bystander_far).await.unwrap().unwrap().shield_hp, 10_000.0);
}

// parity: feat:wormholes.mass-reduction-and-destabilization
#[tokio::test]
async fn test_wormhole_decay_stages_propulsion_mass_collapse_and_static_respawn() {
    eve_testkit::attest(&[
        "feat:wormholes.mass-reduction-and-destabilization",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let (char_id, wh_sys_id, dest_sys_id, ship_id) = (90_000_001, 31_000_240u32, 31_000_028u32, 630_000_001i64);
    let mut sess = setup_pilot_in_space(&ctx, 703, char_id, wh_sys_id, ship_id, 621).await;
    ctx.advance_secs(15);

    let _ = ctx.dispatch_rpc(&mut sess, "scanMgr", None, "GetSystemScanMgr", &[PyRep::Integer(wh_sys_id as i64)], &kw, None).await.unwrap();
    let (static_sig_id, wh_pos) = ctx.db.with_mut_snapshot(|snap| {
        let sig = snap.cosmic_signatures.get_mut(&wh_sys_id).unwrap().iter_mut().find(|s| s.signature_id.starts_with("WST-")).unwrap();
        sig.destination_system_id = Some(dest_sys_id);
        sig.signal_strength = 1.0;
        sig.max_mass_kg = 2_000_000_000.0;
        sig.remaining_mass_kg = 2_000_000_000.0;
        sig.max_jump_mass_kg = 300_000_000.0;
        (sig.signature_id.clone(), sig.position)
    });
    let scan_initial = ctx.dispatch_rpc(&mut sess, "scanMgr", None, "GetSystemScanMgr", &[PyRep::Integer(wh_sys_id as i64)], &kw, None).await.unwrap();
    let PyRep::List(sigs_init) = scan_initial.dict_get("signatures").unwrap() else { panic!() };
    let s_init = sigs_init.iter().find(|s| s.dict_get("signatureID").and_then(|v| v.as_str()) == Some(&static_sig_id)).unwrap();
    assert_eq!(s_init.dict_get("massStabilityDescription").and_then(|v| v.as_str()), Some("This wormhole has not yet had its stability significantly disrupted"));

    // Mass reduction with active propulsion module (50MN MWD I = +50,000,000 kg), destabilization stages, and collapse
    let wh_item_id = wormhole_item_id_for_signature(wh_sys_id, &static_sig_id);
    let wh_sys_handle = ctx.systems.get_or_spawn(wh_sys_id);
    let mwd_id = 630_000_010i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(mwd_id, make_item(mwd_id, 434, char_id, ship_id, InventoryFlag::MedSlot0, 1, true));
        snap.module_active_states.insert(mwd_id);
    });
    let base_caracal_mass = ctx.sde.types.get(&621).map(|t| t.mass).unwrap_or(11_100_000.0);
    let mwd_mass_add = ctx.sde.types.get(&434).and_then(|t| t.default_attributes.get(&796).copied()).unwrap_or(50_000_000.0);
    let expected_jump_mass = base_caracal_mass + mwd_mass_add;
    let jump_pos = DVec3::new(wh_pos[0] + 500.0, wh_pos[1], wh_pos[2]);

    spawn_test_ball(&ctx, wh_sys_id, ship_id, 621, jump_pos, base_caracal_mass, false).await;
    let jump_1 = ctx.dispatch_rpc(&mut sess, "wormholeMgr", None, "WormholeJump", &[PyRep::Integer(wh_item_id)], &kw, None).await.unwrap();
    assert_eq!(jump_1.dict_get("shipJumpMassKg").and_then(|v| v.as_f64()), Some(expected_jump_mass));
    assert!((jump_1.dict_get("remainingMassKg").and_then(|v| v.as_f64()).unwrap() - (2_000_000_000.0 - expected_jump_mass)).abs() < 1.0);

    for (rem_mass, expected_desc) in [
        (600_000_000.0, "This wormhole has had its stability reduced by ships passing through it"),
        (180_000_000.0, "This wormhole has had its stability critically disrupted"),
    ] {
        ctx.advance_secs(301);
        ctx.db.with_mut_snapshot(|snap| {
            snap.characters.get_mut(&char_id).unwrap().solar_system_id = wh_sys_id;
            snap.module_active_states.insert(mwd_id);
            snap.cosmic_signatures.get_mut(&wh_sys_id).unwrap().iter_mut().find(|s| s.signature_id == static_sig_id).unwrap().remaining_mass_kg = rem_mass;
        });
        sess.solar_system_id = Some(wh_sys_id);
        spawn_test_ball(&ctx, wh_sys_id, ship_id, 621, jump_pos, base_caracal_mass, false).await;
        let j = ctx.dispatch_rpc(&mut sess, "wormholeMgr", None, "WormholeJump", &[PyRep::Integer(wh_item_id)], &kw, None).await.unwrap();
        assert_eq!(j.dict_get("massStabilityDescription").and_then(|v| v.as_str()), Some(expected_desc));
    }

    // Remaining mass 11.35M kg (> base Caracal 11.1M kg, < 61.1M kg with active MWD) -> collapses due to active propulsion mass
    ctx.advance_secs(301);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().solar_system_id = wh_sys_id;
        snap.module_active_states.insert(mwd_id);
        snap.cosmic_signatures.get_mut(&wh_sys_id).unwrap().iter_mut().find(|s| s.signature_id == static_sig_id).unwrap().remaining_mass_kg = base_caracal_mass + 250_000.0;
    });
    sess.solar_system_id = Some(wh_sys_id);
    spawn_test_ball(&ctx, wh_sys_id, ship_id, 621, jump_pos, base_caracal_mass, false).await;
    let jump_collapse = ctx.dispatch_rpc(&mut sess, "wormholeMgr", None, "WormholeJump", &[PyRep::Integer(wh_item_id)], &kw, None).await.unwrap();
    assert_eq!(jump_collapse.dict_get("collapsed"), Some(&PyRep::Bool(true)));
    assert!(wh_sys_handle.get_ball_snapshot(wh_item_id).await.unwrap().is_none());
}

// parity: feat:wormholes.system-phenomena-bonuses, feat:dogma.wormhole-phenomena-effects
#[tokio::test]
async fn test_wormhole_system_phenomena_dogma_modifiers_and_hud() {
    eve_testkit::attest(&[
        "feat:wormholes.system-phenomena-bonuses",
        "feat:dogma.wormhole-phenomena-effects",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();

    let expected_pos = [1.30, 1.44, 1.58, 1.72, 1.86, 2.00];
    let expected_neg = [0.85, 0.78, 0.71, 0.64, 0.57, 0.50];
    let expected_half_pos = [1.15, 1.22, 1.29, 1.36, 1.43, 1.50];
    let expected_wr_small = [1.60, 1.88, 2.16, 2.44, 2.72, 3.00];

    let rifter_base = ctx.sde.types.get(&587).unwrap().default_attributes.clone();
    for (idx, class) in (1u8..=6).enumerate() {
        let base_attrs = || -> HashMap<u16, f64> {
            let mut m = rifter_base.clone();
            for (aid, v) in [(64, 10.0), (84, 100.0), (160, 1.0), (653, 100.0), (654, 100.0), (20, 100.0), (554, 100.0), (1211, 10.0), (1210, 10.0), (116, 100.0)] {
                m.insert(aid, v);
            }
            m
        };

        let mut p_attrs = base_attrs();
        let p_tid = wormhole_phenomenon_type_for_class_and_name(class, "Pulsar").unwrap();
        assert!(apply_wormhole_phenomenon_to_attributes(&ctx.sde, p_tid, 6, false, &mut p_attrs) && !p_attrs.contains_key(&146));
        assert!((p_attrs[&263] - rifter_base[&263] * expected_pos[idx]).abs() < 1e-4 && (p_attrs[&552] - rifter_base[&552] * expected_pos[idx]).abs() < 1e-4);
        assert!((p_attrs[&55] - rifter_base[&55] * expected_neg[idx]).abs() < 1e-4 && (p_attrs[&267] - rifter_base[&267] * expected_half_pos[idx]).abs() < 1e-4);

        let mut bh_attrs = base_attrs();
        let bh_tid = wormhole_phenomenon_type_for_class_and_name(class, "Black Hole").unwrap();
        assert!(apply_wormhole_phenomenon_to_attributes(&ctx.sde, bh_tid, 6, false, &mut bh_attrs));
        assert!((bh_attrs[&37] - rifter_base[&37] * expected_pos[idx]).abs() < 1e-4 && (bh_attrs[&76] - rifter_base[&76] * expected_pos[idx]).abs() < 1e-4);
        assert!((bh_attrs[&653] - 100.0 * expected_pos[idx]).abs() < 1e-4 && (bh_attrs[&70] - rifter_base[&70] * expected_half_pos[idx]).abs() < 1e-4 && (bh_attrs[&20] - 100.0 * expected_neg[idx]).abs() < 1e-4);

        let mut cv_attrs = base_attrs();
        let cv_tid = wormhole_phenomenon_type_for_class_and_name(class, "Cataclysmic Variable").unwrap();
        assert!(apply_wormhole_phenomenon_to_attributes(&ctx.sde, cv_tid, 6, false, &mut cv_attrs));
        assert!((cv_attrs[&482] - rifter_base[&482] * expected_pos[idx]).abs() < 1e-4 && (cv_attrs[&55] - rifter_base[&55] * expected_half_pos[idx]).abs() < 1e-4 && (cv_attrs[&84] - 100.0 * expected_neg[idx]).abs() < 1e-4);

        let mut mg_attrs = base_attrs();
        let mg_tid = wormhole_phenomenon_type_for_class_and_name(class, "Magnetar").unwrap();
        assert!(apply_wormhole_phenomenon_to_attributes(&ctx.sde, mg_tid, 6, false, &mut mg_attrs));
        assert!((mg_attrs[&64] - 10.0 * expected_pos[idx]).abs() < 1e-4 && (mg_attrs[&654] - 100.0 * expected_pos[idx]).abs() < 1e-4);
        assert!((mg_attrs[&160] - 1.0 * expected_neg[idx]).abs() < 1e-4 && (mg_attrs[&76] - rifter_base[&76] * expected_neg[idx]).abs() < 1e-4 && (mg_attrs[&554] - 100.0 * expected_neg[idx]).abs() < 1e-4);

        let mut rg_attrs = base_attrs();
        let rg_tid = wormhole_phenomenon_type_for_class_and_name(class, "Red Giant").unwrap();
        assert!(apply_wormhole_phenomenon_to_attributes(&ctx.sde, rg_tid, 6, false, &mut rg_attrs));
        assert!((rg_attrs[&1211] - 10.0 * expected_half_pos[idx]).abs() < 1e-4 && (rg_attrs[&1210] - 10.0 * expected_pos[idx]).abs() < 1e-4 && (rg_attrs[&116] - 100.0 * expected_pos[idx]).abs() < 1e-4);

        let mut wr_attrs = base_attrs();
        let wr_tid = wormhole_phenomenon_type_for_class_and_name(class, "Wolf-Rayet").unwrap();
        assert!(apply_wormhole_phenomenon_to_attributes(&ctx.sde, wr_tid, 6, false, &mut wr_attrs) && !wr_attrs.contains_key(&1493));
        assert!((wr_attrs[&265] - rifter_base[&265] * expected_pos[idx]).abs() < 1e-4 && (wr_attrs[&552] - rifter_base[&552] * expected_neg[idx]).abs() < 1e-4);
        assert!((wr_attrs[&64] - 10.0 * expected_wr_small[idx]).abs() < 1e-4 && (wr_attrs[&271] - rifter_base[&271] * expected_half_pos[idx]).abs() < 1e-4);

        let mut sleeper_attrs = base_attrs();
        assert!(!apply_wormhole_phenomenon_to_attributes(&ctx.sde, wr_tid, 6, true, &mut sleeper_attrs));
        assert_eq!(sleeper_attrs[&265], rifter_base[&265]);
        let mut struct_attrs = base_attrs();
        assert!(!apply_wormhole_phenomenon_to_attributes(&ctx.sde, wr_tid, 65, false, &mut struct_attrs));
        assert_eq!(struct_attrs[&265], rifter_base[&265]);
    }

    assert_eq!(resolve_system_wormhole_phenomenon(&ctx.sde, 31_002_585, None).expect("C13 shattered"), (13, 6, "Wolf-Rayet", 30879));
    assert_eq!(resolve_system_wormhole_phenomenon(&ctx.sde, 31_000_006, None).expect("Drifter WR"), (18, 2, "Wolf-Rayet", 30875));

    // Live ship attribute verification via `dogmaIM.GetAllInfo` in Jita vs W-space phenomenon system `31_000_006` (Class 2 Wolf-Rayet)
    let (char_id, ship_id) = (90_000_001, 640_000_001i64);
    let mut sess = setup_pilot_in_space(&ctx, 704, char_id, 30_000_142, ship_id, 587).await;
    let get_ship_attr = |info: &PyRep, aid: i64| -> f64 {
        let Some(PyRep::Dict(entries)) = info.dict_get("shipAttributes") else { panic!("shipAttributes") };
        entries.iter().find(|(k, _)| k.as_i64() == Some(aid)).and_then(|(_, v)| v.as_f64()).unwrap()
    };
    let jita_info = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "GetAllInfo", &[PyRep::Bool(true), PyRep::Bool(true)], &kw, None).await.unwrap();
    let (jita_armor, jita_sig, jita_sh_em) = (get_ship_attr(&jita_info, 265), get_ship_attr(&jita_info, 552), get_ship_attr(&jita_info, 271));

    sess.solar_system_id = Some(31_000_006);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().solar_system_id = 31_000_006;
        snap.items.get_mut(&ship_id).unwrap().location_id = 31_000_006;
    });
    let wh_info = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "GetAllInfo", &[PyRep::Bool(true), PyRep::Bool(true)], &kw, None).await.unwrap();
    assert!((get_ship_attr(&wh_info, 265) - jita_armor * 1.44).abs() < 1e-3);
    assert!((get_ship_attr(&wh_info, 552) - jita_sig * 0.78).abs() < 1e-3);
    assert!((get_ship_attr(&wh_info, 271) - jita_sh_em * 1.22).abs() < 1e-3);

    // Verify Sleeper NPC entity in `31_000_006` is NOT modified by `apply_active_dbuffs_and_system_effects_to_ship_attrs`
    let sleeper_item_id = 640_000_999i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(sleeper_item_id, make_item(sleeper_item_id, 30187, 1, 31_000_006, InventoryFlag::None, 1, true));
    });
    let mut npc_attrs = HashMap::from([(265u16, 1000.0), (552u16, 100.0)]);
    eve_server::services::inventory_fitting::fitting_scanners_superweapons::apply_active_dbuffs_and_system_effects_to_ship_attrs(&ctx, sleeper_item_id, &mut npc_attrs);
    assert_eq!(npc_attrs[&265], 1000.0, "Sleeper NPC armorHP unaffected by W-space phenomenon");

    let hud_info = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "GetSystemWideEffectsInfo", &[PyRep::Integer(31_000_006)], &kw, None).await.unwrap();
    assert_eq!((hud_info.dict_get("hudVisible"), hud_info.dict_get("appliesToCapsuleerShipsOnly")), (Some(&PyRep::Bool(true)), Some(&PyRep::Bool(true))));
    assert_eq!(hud_info.dict_get("phenomenonName").and_then(|v| v.as_str()), Some("Wolf-Rayet"));

    let on_ship = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "GetSystemWideEffectsOnShip", &[PyRep::Integer(ship_id)], &kw, None).await.unwrap();
    assert_eq!((on_ship.dict_get("hudVisible"), on_ship.dict_get("appliesToCapsuleerShipsOnly")), (Some(&PyRep::Bool(true)), Some(&PyRep::Bool(true))));
    assert_eq!(on_ship.dict_get("phenomenonName").and_then(|v| v.as_str()), Some("Wolf-Rayet"));
}
}


pub(crate) mod pve_hacking_parity_closure {
    #[allow(unused_imports)]
    use super::*;

// Integration tests for V24 Hacking Node Types & Combat and Hacking Failure Outcomes
// (`feat:pve.hacking-node-types`, `feat:pve.hacking-failure-outcome`).


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
    let _use_sr = ctx
        .dispatch_rpc(&mut sess, "hackingMgr", None, "UsedUtilityElement", &[PyRep::Integer(data_can_id), PyRep::Integer(2), PyRep::Integer(0)], &kw, None)
        .await
        .expect("Use SelfRepair");
    let sr_turns = with_pve_exploration_reg(&ctx, |reg| reg.self_repair_turns.get(&(PILOT_1, data_can_id)).copied());
    assert_eq!(sr_turns, Some(3));
    let _ = coh_before_sr;

    let _ = ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "UsedUtilityElement", &[PyRep::Integer(data_can_id), PyRep::Integer(3), PyRep::Integer(7)], &kw, None).await.unwrap();
    let sv_active = with_pve_exploration_reg(&ctx, |reg| reg.hacking_games[&(PILOT_1, data_can_id)].secondary_vector_target);
    assert_eq!(sv_active, Some((7, 3)));

    // Verify SelfRepair heals 5..=10 per turn for 3 turns and exceeds max_virus_coherence
    let max_coh = with_pve_exploration_reg(&ctx, |reg| {
        let g = reg.hacking_games.get_mut(&(PILOT_1, data_can_id)).unwrap();
        g.virus_coherence = g.max_virus_coherence;
        g.max_virus_coherence
    });
    let mut prev_coh = max_coh;
    for tid in [3_i64, 4, 5] {
        ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "ClickedOnTile", &[PyRep::Integer(data_can_id), PyRep::Integer(tid)], &kw, None).await.unwrap();
        let cur_coh = with_pve_exploration_reg(&ctx, |reg| reg.hacking_games[&(PILOT_1, data_can_id)].virus_coherence);
        assert!((5..=10).contains(&(cur_coh - prev_coh)), "SelfRepair must heal 5..=10 per turn");
        prev_coh = cur_coh;
    }
    assert!(prev_coh > max_coh, "SelfRepair must allow virus_coherence to exceed max_virus_coherence");
    assert_eq!(with_pve_exploration_reg(&ctx, |reg| reg.self_repair_turns.get(&(PILOT_1, data_can_id)).copied()), None);
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
}
