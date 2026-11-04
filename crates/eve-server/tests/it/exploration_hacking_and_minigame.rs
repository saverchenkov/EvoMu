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
