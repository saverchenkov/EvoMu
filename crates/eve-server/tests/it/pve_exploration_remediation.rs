//! Remediation tests for PvE Exploration and Petitioner RPCs:
//! - `feat:pve.escalations`: In-space NPC destruction triggers real escalation chance, journal entry, destination solar system, and notification.
//! - `feat:pve.expeditions`: Multi-step journal progression with dynamic steps, destination encounter spawning, and completion.
//! - `feat:pve.ghost-sites`: Real server countdown timer (90-120s), expired detonation damage, and covert pirate ambush.
//! - `feat:pve.sleeper-caches`: Limited (5604), Standard (5609), Superior (5632) ship restrictions, puzzles, and gas hazard mechanics.
//! - `rpc:petitioner.MayPetition`: Client expectations matching arg schema and error codes (-1 rate-limited, -2 banned/missing, -3 invalid category).
//! - `rpc:petitioner.GetCategoryProperties`: Returns category-specific property descriptor KeyVals.

#![forbid(unsafe_code)]

use std::collections::HashMap;
use glam::DVec3;
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::destroy_specialized_npc_and_queue_bounty;
use eve_server::services::pve_exploration::{
    advance_pve_exploration_clock, with_pve_exploration_reg, HackingNodeKind,
};
use eve_server::services::pve_expeditions::{
    advance_expedition_step, create_escalation_expedition,
};
use eve_server::services::pve_exploration_seeding::ensure_system_pve_sites_seeded;
use eve_server::services::pve_sleeper_caches::is_ghost_site_timer_expired;

async fn create_in_space_session(
    ctx: &ServerContext,
    session_id: u64,
    solar_system_id: u32,
    char_id: i64,
    ship_id: i64,
    ship_type_id: u32,
) -> SessionState {
    let kw = HashMap::new();
    let mut sess = SessionState::new(session_id);
    let _ = ctx.dispatch_rpc(&mut sess, "authentication", None, "Login", &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())], &kw, None).await;
    let _ = ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None).await;
    sess.solar_system_id = Some(solar_system_id);
    sess.ship_id = Some(ship_id);
    sess.character_id = Some(char_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = solar_system_id;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(ship_id, InventoryItemRecord {
            item_id: ship_id, type_id: ship_type_id, owner_id: char_id, location_id: solar_system_id as i64,
            flag: InventoryFlag::Hangar, quantity: 1, singleton: true, ..Default::default()
        });
        for (slot_off, tid, flag) in [(1_i64, 22175_u32, InventoryFlag::MedSlot0), (2, 22177, InventoryFlag::MedSlot1)] {
            snap.items.insert(ship_id * 10 + slot_off, InventoryItemRecord {
                item_id: ship_id * 10 + slot_off, type_id: tid, owner_id: char_id, location_id: ship_id,
                flag, quantity: 1, singleton: true, ..Default::default()
            });
        }
    });
    let mut cs = ctx.build_type_combat_state(ship_type_id);
    cs.shield_hp = 50_000.0;
    cs.max_shield_hp = 50_000.0;
    let _ = ctx.systems.get_or_spawn(solar_system_id).spawn_ball_with_combat(Ball {
        id: ship_id, type_id: ship_type_id, position: DVec3::ZERO, velocity: DVec3::ZERO,
        max_velocity: 365.0, inertia: 1.0, mass: 1_067_000.0, radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    }, cs).await;
    sess
}

async fn traverse_and_win_hack(ctx: &ServerContext, sess: &mut SessionState, char_id: i64, can_id: i64) {
    let kw = HashMap::new();
    with_pve_exploration_reg(ctx, |reg| {
        if let Some(game) = reg.hacking_games.get_mut(&(char_id, can_id)) {
            game.base_virus_strength = 200;
            game.virus_coherence = 2000;
            game.max_virus_coherence = 2000;
        }
    });
    let core_idx = with_pve_exploration_reg(ctx, |reg| {
        reg.hacking_games[&(char_id, can_id)].tiles.iter().find(|t| format!("{:?}", t.kind) == "SystemCore").unwrap().tile_index
    });
    let mut path: Vec<u32> = (1..=(core_idx / 6)).map(|r| r * 6).collect();
    path.extend(((core_idx / 6) * 6 + 1)..=core_idx);
    for idx in path {
        for _ in 0..5 {
            let Ok(r) = ctx.dispatch_rpc(sess, "hackingMgr", None, "ClickedOnTile", &[PyRep::Integer(can_id), PyRep::Integer(idx as i64)], &kw, None).await else {
                return;
            };
            if r.dict_get("status").and_then(|v| v.as_str()) == Some("Won") {
                return;
            }
            if r.dict_get("nodeCoherence").and_then(|v| v.as_i64()).unwrap_or(0) <= 0 {
                break;
            }
        }
    }
}

/// Tests that destroying in-space combat site NPCs triggers an escalation roll, creating
/// an expedition entry with destination system, expiry timer, and bookmark.
// parity: feat:pve.escalations
#[tokio::test]
async fn test_pve_escalation_npc_destruction_triggers_expedition() {
    let ctx = ServerContext::new();
    let sys_id = 30_000_142; // Jita
    let char_id = 90_000_001;
    let ship_id = 1_000_101;

    let _sess = create_in_space_session(&ctx, 77_001, sys_id, char_id, ship_id, 587).await;

    // Seed PvE combat anomaly / signature sites
    ensure_system_pve_sites_seeded(&ctx, sys_id).await;

    // Seed 3 gives SplitMix64 first draw 0.1135 < 0.20 (natural 20% escalation roll)
    ctx.set_mechanic_rng_seed(Some(3));

    // Find a combat site with active NPC balls
    let (site_id, active_balls) = with_pve_exploration_reg(&ctx, |reg| {
        let sites = reg.sites_by_system.get(&sys_id).cloned().unwrap_or_default();
        let s = sites
            .into_iter()
            .find(|site| site.site_category == "CombatAnomaly" && !site.active_npc_ball_ids.is_empty())
            .expect("combat anomaly with npc balls must be seeded");
        (s.site_id.clone(), s.active_npc_ball_ids.clone())
    });

    assert!(!active_balls.is_empty());

    // Destroy all wave NPCs to trigger site completion and escalation roll
    for npc_ball_id in active_balls {
        ctx.set_mechanic_rng_seed(Some(3));
        destroy_specialized_npc_and_queue_bounty(&ctx, sys_id, npc_ball_id, char_id).await;
    }

    // If site has next wave, destroy those as well until site is completed
    loop {
        let remaining = with_pve_exploration_reg(&ctx, |reg| {
            reg.sites_by_system
                .get(&sys_id)
                .and_then(|sites| sites.iter().find(|s| s.site_id == site_id))
                .map(|s| s.active_npc_ball_ids.clone())
        });
        match remaining {
            Some(balls) if !balls.is_empty() => {
                for b in balls {
                    ctx.set_mechanic_rng_seed(Some(3));
                    destroy_specialized_npc_and_queue_bounty(&ctx, sys_id, b, char_id).await;
                }
            }
            _ => break,
        }
    }

    // Verify escalation expedition was created for character
    let paths = with_pve_exploration_reg(&ctx, |reg| {
        reg.escalations_by_char.get(&char_id).cloned().unwrap_or_default()
    });
    assert!(!paths.is_empty(), "Escalation expedition must be created in character state");
    let entry = &paths[0];
    assert_eq!(entry.step_index, 1, "First step should be index 1");
    assert!(entry.total_steps >= 2, "Expeditions must have at least 2 steps");
    assert!(entry.destination_solar_system_id > 0, "Destination system must be resolved");
    assert!(entry.expires_at_sec > entry.created_at_sec, "Expiry timer must be in future");

    // Verify bookmark was placed in character journal/bookmarks
    let bms = ctx.db.with_snapshot(|snap| {
        snap.bookmarks
            .values()
            .filter(|b| b.owner_id == char_id && b.solar_system_id == entry.destination_solar_system_id)
            .cloned()
            .collect::<Vec<_>>()
    });
    assert!(!bms.is_empty(), "Expedition bookmark must be created for character");

    // Verify server Macho notification OnEscalatingPathMessage was emitted
    let pending = ctx.drain_notifications_for_char(char_id);
    assert!(
        pending.iter().any(|n| n.event == "OnEscalatingPathMessage"),
        "OnEscalatingPathMessage notification must be queued"
    );
}

/// Tests multi-step expedition progression, client RPC querying, and completion.
// parity: feat:pve.expeditions
#[tokio::test]
async fn test_pve_expedition_multi_step_progression_and_rpcs() {
    let ctx = ServerContext::new();
    let sys_id = 30_000_142;
    let char_id = 90_000_002;
    let ship_id = 1_000_102;

    let mut sess = create_in_space_session(&ctx, 77_002, sys_id, char_id, ship_id, 587).await;

    // Create an expedition directly
    let entry = create_escalation_expedition(
        &ctx,
        char_id,
        sys_id,
        2001,
        "DED 4/10 Outpost Escalation",
        [0.0, 0.0, 0.0],
    )
    .await
    .expect("escalation created");

    assert_eq!(entry.step_index, 1);
    assert!(entry.total_steps >= 2);
    let inst_id = entry.instance_id;

    // Query scanSvc.GetMyEscalatingPathDetails RPC
    let kw = HashMap::new();
    let details_rep = ctx
        .dispatch_rpc(&mut sess, "scanSvc", None, "GetMyEscalatingPathDetails", &[], &kw, None)
        .await
        .expect("GetMyEscalatingPathDetails RPC should succeed");

    match details_rep {
        PyRep::Rowset { columns, rows } => {
            assert_eq!(rows.len(), 1);
            let inst_idx = columns.iter().position(|c| c == "instanceID").unwrap();
            let step_idx = columns.iter().position(|c| c == "stepIndex").unwrap();
            assert_eq!(rows[0][inst_idx], PyRep::Integer(inst_id));
            assert_eq!(rows[0][step_idx], PyRep::Integer(1));
        }
        _ => panic!("Expected Rowset return from GetMyEscalatingPathDetails"),
    }

    // Query scanSvc.GetEscalatingPathDungeonsEntered RPC
    let entered_rep = ctx
        .dispatch_rpc(&mut sess, "scanSvc", None, "GetEscalatingPathDungeonsEntered", &[], &kw, None)
        .await
        .expect("GetEscalatingPathDungeonsEntered RPC should succeed");
    assert!(matches!(entered_rep, PyRep::List(_)));

    // Progress to Step 2
    let updated = advance_expedition_step(&ctx, char_id, inst_id)
        .await
        .expect("expedition advanced");
    assert_eq!(updated.unwrap().step_index, 2);

    // Progress to final step and beyond to complete
    let total = entry.total_steps;
    for step in 3..=total {
        let u = advance_expedition_step(&ctx, char_id, inst_id)
            .await
            .expect("expedition step advanced");
        assert_eq!(u.unwrap().step_index, step);
    }

    // Progressing at final step completes expedition
    let final_entry = advance_expedition_step(&ctx, char_id, inst_id)
        .await
        .expect("expedition complete");
    assert_eq!(final_entry.unwrap().step_index, total);

    // Verify it is removed from active paths
    let paths_after = with_pve_exploration_reg(&ctx, |reg| {
        reg.escalations_by_char.get(&char_id).cloned().unwrap_or_default()
    });
    assert!(paths_after.is_empty(), "Completed expedition should be cleared from active paths");
}

/// Tests Ghost Site server timer, detonation explosive damage, and covert pirate response.
// parity: feat:pve.ghost-sites
#[tokio::test]
async fn test_pve_ghost_site_timer_and_detonation_mechanics() {
    let ctx = ServerContext::new();
    let sys_id = 30_000_142;
    let char_id = 90_000_003;
    let ship_id = 1_000_103;

    let mut sess = create_in_space_session(&ctx, 77_003, sys_id, char_id, ship_id, 587).await;
    let kw = HashMap::new();
    ensure_system_pve_sites_seeded(&ctx, sys_id).await;

    // 1. Ghost Site hackable containers; the first hack attempt activates the hidden ghost site timer
    let (site_pos, cans) = with_pve_exploration_reg(&ctx, |reg| {
        let s = reg.sites_by_system[&sys_id].iter().find(|s| s.site_id == "GHO-30000142").unwrap();
        (s.position, s.hackable_can_ids.clone())
    });
    let _ = ctx.systems.get_or_spawn(sys_id).set_ball_position(ship_id, DVec3::from_array(site_pos) + DVec3::new(500.0, 0.0, 0.0)).await;
    let (can0_id, can1_id, can2_id) = (cans[0], cans[1], cans[2]);


    // 2. Hacking succeeds before timer expiry (t < 90s)
    let start_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "hackingMgr",
            None,
            "StartNewGameInstance",
            &[PyRep::Integer(can0_id)],
            &kw,
            None,
        )
        .await
        .expect("StartNewGameInstance before expiry");
    assert_eq!(start_rep.dict_get("status").and_then(|v| v.as_str()), Some("InProgress"));
    let timer_active = with_pve_exploration_reg(&ctx, |reg| {
        let site = reg.sites_by_system.get(&sys_id).and_then(|l| l.iter().find(|s| s.site_id == "GHO-30000142")).unwrap();
        site.ghost_timer_started_at.is_some() && !is_ghost_site_timer_expired(reg, sys_id, "GHO-30000142")
    });
    assert!(timer_active, "Ghost site timer must be started by the first hack attempt");

    traverse_and_win_hack(&ctx, &mut sess, char_id, can0_id).await;
    let win_status = with_pve_exploration_reg(&ctx, |reg| {
        reg.hacking_games[&(char_id, can0_id)].status.clone()
    });
    assert_eq!(win_status, "Won", "Hack before timer expiry must succeed");

    // 3. Detonates immediately on failed hack (status == 'Failed')
    let _ = ctx
        .dispatch_rpc(
            &mut sess,
            "hackingMgr",
            None,
            "StartNewGameInstance",
            &[PyRep::Integer(can1_id)],
            &kw,
            None,
        )
        .await
        .expect("StartNewGameInstance can1");
    with_pve_exploration_reg(&ctx, |reg| {
        if let Some(g) = reg.hacking_games.get_mut(&(char_id, can1_id)) {
            g.virus_coherence = 1;
            if let Some(t) = g.tiles.get_mut(1) {
                t.kind = HackingNodeKind::Firewall;
                t.coherence = 40;
                t.strength = 100;
            }
        }
    });
    let fail_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "hackingMgr",
            None,
            "ClickedOnTile",
            &[PyRep::Integer(can1_id), PyRep::Integer(1)],
            &kw,
            None,
        )
        .await
        .expect("Click tile causing failure");
    assert_eq!(fail_rep.dict_get("status").and_then(|v| v.as_str()), Some("Failed"));
    assert_eq!(fail_rep.dict_get("ghostDetonated").and_then(|v| v.as_bool()), Some(true));

    // 4. Detonates once clock advances past 90-120s
    with_pve_exploration_reg(&ctx, |reg| {
        if let Some(sites) = reg.sites_by_system.get_mut(&sys_id) {
            if let Some(s) = sites.iter_mut().find(|s| s.site_id == "GHO-30000142") {
                s.ghost_detonated = false;
            }
        }
    });
    advance_pve_exploration_clock(&ctx, 110);
    let expired_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "hackingMgr",
            None,
            "StartNewGameInstance",
            &[PyRep::Integer(can2_id)],
            &kw,
            None,
        )
        .await
        .expect("StartNewGameInstance after timer expiry");
    assert_eq!(expired_rep.dict_get("status").and_then(|v| v.as_str()), Some("GhostSiteDetonated"));
    assert!(expired_rep.dict_get("explosiveDamageApplied").and_then(|v| v.as_f64()).unwrap_or(0.0) >= 4_000.0);
    let PyRep::List(ambush_npcs) = expired_rep.dict_get("spawnedPirateNpcBallIDs").unwrap() else { panic!("expected list") };
    assert!(!ambush_npcs.is_empty(), "Pirate ambush NPCs must spawn on ghost detonation");
}

/// Tests Sleeper Cache tiers (Limited, Standard, Superior), ship mass restrictions,
/// spatial rift passage controls, and environmental gas cloud damage.
// parity: feat:pve.sleeper-caches
#[tokio::test]
async fn test_pve_sleeper_cache_tiers_hazards_and_rift_passage() {
    let ctx = ServerContext::new();
    let sys_id = 30_000_142;
    let char_id = 90_000_004;
    let frigate_ship_id = 1_000_104;

    let mut sess = create_in_space_session(&ctx, 77_004, sys_id, char_id, frigate_ship_id, 587).await;
    let kw = HashMap::new();
    ensure_system_pve_sites_seeded(&ctx, sys_id).await;

    // 1. Exercise all three tiers (Limited 5604/3546/2355, Standard 5609/3547, Superior 5632/3548) via CanEnterDungeonInCurrentShip RPC
    for did in [5604, 3546, 2355] {
        let rep = ctx.dispatch_rpc(&mut sess, "keeper", None, "CanEnterDungeonInCurrentShip", &[PyRep::Integer(did)], &kw, None).await.expect("CanEnter Limited");
        assert_eq!(rep.dict_get("canEnter").and_then(|v| v.as_bool()), Some(true));
    }
    for did in [5609, 3547] {
        let rep = ctx.dispatch_rpc(&mut sess, "keeper", None, "CanEnterDungeonInCurrentShip", &[PyRep::Integer(did)], &kw, None).await.expect("CanEnter Standard");
        assert_eq!(rep.dict_get("canEnter").and_then(|v| v.as_bool()), Some(true));
    }
    for did in [5632, 3548] {
        let rep = ctx.dispatch_rpc(&mut sess, "keeper", None, "CanEnterDungeonInCurrentShip", &[PyRep::Integer(did)], &kw, None).await.expect("CanEnter Superior");
        assert_eq!(rep.dict_get("canEnter").and_then(|v| v.as_bool()), Some(true));
    }

    // Switch to Cruiser (624, group 26, mass 11,000,000 kg)
    let cruiser_ship_id = 1_000_105;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) { ch.active_ship_id = cruiser_ship_id; }
        snap.items.insert(cruiser_ship_id, InventoryItemRecord {
            item_id: cruiser_ship_id, type_id: 624, owner_id: char_id, location_id: sys_id as i64,
            flag: InventoryFlag::Hangar, quantity: 1, singleton: true, ..Default::default()
        });
    });
    sess.ship_id = Some(cruiser_ship_id);
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle.spawn_ball(Ball {
        id: cruiser_ship_id, type_id: 624, position: DVec3::ZERO, velocity: DVec3::ZERO,
        max_velocity: 365.0, inertia: 1.0, mass: 11_000_000.0, radius: 100.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await;

    // Cruiser rejected in Limited (5604, 3546 & 2355)
    for did in [5604, 3546, 2355] {
        let rep = ctx.dispatch_rpc(&mut sess, "keeper", None, "CanEnterDungeonInCurrentShip", &[PyRep::Integer(did)], &kw, None).await.expect("Cruiser Limited");
        assert_eq!(rep.dict_get("canEnter").and_then(|v| v.as_bool()), Some(false));
    }
    // Cruiser allowed in Standard (5609 & 3547)
    for did in [5609, 3547] {
        let rep = ctx.dispatch_rpc(&mut sess, "keeper", None, "CanEnterDungeonInCurrentShip", &[PyRep::Integer(did)], &kw, None).await.expect("Cruiser Standard");
        assert_eq!(rep.dict_get("canEnter").and_then(|v| v.as_bool()), Some(true));
    }

    // Switch back to Frigate
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) { ch.active_ship_id = frigate_ship_id; }
    });
    sess.ship_id = Some(frigate_ship_id);

    // 2. Sleeper Cache signature hackable containers (seeded site state)
    let (sc_pos, sc_cans) = with_pve_exploration_reg(&ctx, |reg| {
        let s = reg.sites_by_system[&30_000_142].iter().find(|s| s.site_id == "SLC-30000142").unwrap();
        (s.position, s.hackable_can_ids.clone())
    });
    let _ = sys_handle.set_ball_position(frigate_ship_id, DVec3::from_array(sc_pos) + DVec3::new(500.0, 0.0, 0.0)).await;
    let rift_can_id = sc_cans[0];
    let gas_can_id = sc_cans[2];

    // 3. Attempting room traversal before hacking Rift Control fails
    let rift_err = ctx
        .dispatch_rpc(&mut sess, "keeper", None, "ActivateAccelerationGate", &[PyRep::String("SLC-30000142".into())], &kw, None)
        .await
        .expect_err("Unstabilized rift must reject room entry");
    assert_eq!(rift_err.code, "RiftPassageUnstable");

    // 4. Hack Rift Control (node 0) via hackingMgr
    ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "StartNewGameInstance", &[PyRep::Integer(rift_can_id)], &kw, None)
        .await
        .expect("StartNewGameInstance Rift");
    traverse_and_win_hack(&ctx, &mut sess, char_id, rift_can_id).await;
    let rift_stabilized = with_pve_exploration_reg(&ctx, |reg| {
        reg.sites_by_system.get(&sys_id).and_then(|l| l.iter().find(|s| s.site_id == "SLC-30000142")).unwrap().sleeper_rift_stabilized
    });
    assert!(rift_stabilized, "Hacking rift can 0 must stabilize rift passage");

    // 5. Enter room now succeeds, but applies hazard damage because gas cloud is still active
    let pre_shield = sys_handle.get_combat_state(frigate_ship_id).await.unwrap().unwrap().shield_hp;
    let _ = ctx.dispatch_rpc(&mut sess, "keeper", None, "ActivateAccelerationGate", &[PyRep::String("SLC-30000142".into())], &kw, None).await.expect("Enter room with active gas");
    let post_shield = sys_handle.get_combat_state(frigate_ship_id).await.unwrap().unwrap().shield_hp;
    assert!(post_shield < pre_shield, "Hazard damage must be applied when gas cloud is untoggled");

    // 6. Hack Gas Cloud node (node 2) to deactivate gas hazard
    ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "StartNewGameInstance", &[PyRep::Integer(gas_can_id)], &kw, None)
        .await
        .expect("StartNewGameInstance Gas Can");
    traverse_and_win_hack(&ctx, &mut sess, char_id, gas_can_id).await;
    let gas_active = with_pve_exploration_reg(&ctx, |reg| {
        reg.sites_by_system.get(&sys_id).and_then(|l| l.iter().find(|s| s.site_id == "SLC-30000142")).unwrap().sleeper_gas_cloud_active
    });
    assert!(!gas_active, "Hacking gas can 2 must disable gas hazard");
}

/// Tests petitioner.MayPetition RPC for valid categories, invalid category, banned, and rate-limiting.
// parity: rpc:petitioner.MayPetition
#[tokio::test]
async fn test_petitioner_may_petition_codes() {
    let ctx = ServerContext::new();
    let mut sess = create_in_space_session(&ctx, 77_005, 30_000_142, 90_000_001, 1_000_105, 587).await;
    let kw = HashMap::new();

    // 1. Normal petition for valid category 401, subcategory 0 -> Permitted (code 1)
    let res = ctx.dispatch_rpc(&mut sess, "petitioner", None, "MayPetition", &[PyRep::Integer(401), PyRep::Integer(0)], &kw, None).await.unwrap();
    assert_eq!(res, PyRep::Integer(1), "Valid category should be permitted with code 1");

    // 2. Invalid category (99999, 0) -> code -4
    let res_inv = ctx.dispatch_rpc(&mut sess, "petitioner", None, "MayPetition", &[PyRep::Integer(99999), PyRep::Integer(0)], &kw, None).await.unwrap();
    assert_eq!(res_inv, PyRep::Integer(-4), "Invalid category should return -4");

    // 3. Category disabled via store state (disabled_categories / zendesk_disabled) -> code -3
    eve_server::services::account_services::with_store(&ctx, |_, store| {
        store.disabled_categories.insert(401);
    });
    let res_dis = ctx.dispatch_rpc(&mut sess, "petitioner", None, "MayPetition", &[PyRep::Integer(401), PyRep::Integer(0)], &kw, None).await.unwrap();
    assert_eq!(res_dis, PyRep::Integer(-3), "Disabled category in store must return -3");
    eve_server::services::account_services::with_store(&ctx, |_, store| {
        store.disabled_categories.clear();
        store.zendesk_disabled = true;
    });
    let res_dis2 = ctx.dispatch_rpc(&mut sess, "petitioner", None, "MayPetition", &[PyRep::Integer(401), PyRep::Integer(0)], &kw, None).await.unwrap();
    assert_eq!(res_dis2, PyRep::Integer(-3), "Zendesk disabled in store must return -3");
    eve_server::services::account_services::with_store(&ctx, |_, store| {
        store.zendesk_disabled = false;
    });

    // 4. Rate-limited / cooldown (petition created within last 60 seconds) -> code -7
    let _ = ctx.dispatch_rpc(&mut sess, "petitioner", None, "CreatePetition", &[PyRep::String("First ticket".into()), PyRep::String("Help needed".into()), PyRep::Integer(401)], &kw, None).await.unwrap();
    let res_cd = ctx.dispatch_rpc(&mut sess, "petitioner", None, "MayPetition", &[PyRep::Integer(401), PyRep::Integer(0)], &kw, None).await.unwrap();
    assert_eq!(res_cd, PyRep::Integer(-7), "Recent petition within 60s cooldown must return -7");

    // 5. Unauthenticated / missing character -> code -6
    let mut unauth_sess = SessionState::new(88_999);
    let res_unauth = ctx.dispatch_rpc(&mut unauth_sess, "petitioner", None, "MayPetition", &[PyRep::Integer(401), PyRep::Integer(0)], &kw, None).await.unwrap();
    assert_eq!(res_unauth, PyRep::Integer(-6), "Missing character session must return -6");

    // 6. Banned account -> code -1
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(acc) = snap.accounts.get_mut("demo") {
            acc.banned_until_unix = Some(ctx.now_unix() + 3600);
        }
    });
    let res_ban = ctx.dispatch_rpc(&mut sess, "petitioner", None, "MayPetition", &[PyRep::Integer(401), PyRep::Integer(0)], &kw, None).await.unwrap();
    assert_eq!(res_ban, PyRep::Integer(-1), "Banned account must return -1");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(acc) = snap.accounts.get_mut("demo") {
            acc.banned_until_unix = None;
        }
    });

    // 7. Too many open tickets (>= 10 open petitions) -> code -2
    for i in 2..=10 {
        let _ = ctx.dispatch_rpc(&mut sess, "petitioner", None, "CreatePetition", &[PyRep::String(format!("Subject {i}")), PyRep::String("Help needed".into()), PyRep::Integer(401)], &kw, None).await.unwrap();
    }
    let res_limit = ctx.dispatch_rpc(&mut sess, "petitioner", None, "MayPetition", &[PyRep::Integer(401), PyRep::Integer(0)], &kw, None).await.unwrap();
    assert_eq!(res_limit, PyRep::Integer(-2), "Character with >= 10 open tickets must return -2");
}

/// Tests Combat Anomaly regional pirate faction spawning, combat states, multi-wave encounters, and final-wave loot container drop.
// parity: feat:pve.combat-anomalies
#[tokio::test]
async fn test_pve_combat_anomalies_regional_spawns_and_loot() {
    let ctx = ServerContext::new();
    let sys_id = 30_000_142; // Jita (Guristas)
    let char_id = 90_000_001;
    let ship_id = 1_000_110;

    let _sess = create_in_space_session(&ctx, 77_009, sys_id, char_id, ship_id, 587).await;
    ensure_system_pve_sites_seeded(&ctx, sys_id).await;

    // Verify regional Guristas NPC type (10962) and CombatState
    let (site_id, initial_npcs) = with_pve_exploration_reg(&ctx, |reg| {
        let sites = reg.sites_by_system.get(&sys_id).cloned().unwrap_or_default();
        let s = sites.into_iter().find(|site| site.site_category == "CombatAnomaly" && !site.active_npc_ball_ids.is_empty()).unwrap();
        (s.site_id, s.active_npc_ball_ids)
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    for &npc_id in &initial_npcs {
        let snap = sys_handle.get_ball_snapshot(npc_id).await.unwrap().expect("NPC ball must exist");
        assert_eq!(snap.1.type_id, 10_962, "The Forge anomalies must spawn Guristas NPCs (10962)");
        let cs = sys_handle.get_combat_state(npc_id).await.unwrap();
        assert!(cs.is_some(), "Spawned PvE NPC ball must have CombatState");
    }

    // Destroy Wave 1 NPCs via weapons damage with attacker
    for npc_id in initial_npcs {
        let dmg = eve_destiny::DamageVector { kinetic: 50_000.0, ..Default::default() };
        let _ = sys_handle.apply_damage_with_attacker(npc_id, dmg, Some(char_id)).await;
    }

    // Advance remaining waves to completion
    loop {
        let remaining = with_pve_exploration_reg(&ctx, |reg| {
            reg.sites_by_system.get(&sys_id).and_then(|l| l.iter().find(|s| s.site_id == site_id)).map(|s| s.active_npc_ball_ids.clone())
        }).unwrap_or_default();
        if remaining.is_empty() { break; }
        for npc_id in remaining {
            let dmg = eve_destiny::DamageVector { kinetic: 50_000.0, ..Default::default() };
            let _ = sys_handle.apply_damage_with_attacker(npc_id, dmg, Some(char_id)).await;
        }
    }

    // Verify site is marked cleared and OnCosmicAnomalyRemoved notification was dispatched
    let is_cleared = with_pve_exploration_reg(&ctx, |reg| {
        reg.sites_by_system.get(&sys_id).and_then(|l| l.iter().find(|s| s.site_id == site_id)).map(|s| s.cleared).unwrap_or(false)
    });
    assert!(is_cleared, "Combat anomaly site must be cleared after final wave");

    let notifs = ctx.pending_notifications.read().unwrap().get(&char_id).cloned().unwrap_or_default();
    assert!(notifs.iter().any(|n| n.event == "OnCosmicAnomalyRemoved"), "Final wave destruction must emit OnCosmicAnomalyRemoved");

    // Verify final-wave in-space loot cargo container ball (type_id: 3293) dropped
    let has_loot_container = ctx.db.with_snapshot(|snap| {
        snap.items.values().any(|item| item.type_id == 3293 && item.location_id == sys_id as i64)
    });
    assert!(has_loot_container, "Final wave must drop in-space cargo container ball (3293)");
}

/// Tests petitioner.GetCategoryProperties returns KeyVal property descriptors for categories.
// parity: rpc:petitioner.GetCategoryProperties
#[tokio::test]
async fn test_petitioner_get_category_properties_keyvals() {
    let ctx = ServerContext::new();
    let sys_id = 30_000_142;
    let char_id = 90_000_001;
    let ship_id = 1_000_106;

    let mut sess = create_in_space_session(&ctx, 77_006, sys_id, char_id, ship_id, 587).await;
    let kw = HashMap::new();

    // Query Category 401: Stuck in Space
    let res_401 = ctx
        .dispatch_rpc(&mut sess, "petitioner", None, "GetCategoryProperties", &[PyRep::Integer(401)], &kw, None)
        .await
        .expect("GetCategoryProperties should succeed");

    match res_401 {
        PyRep::List(props) => {
            assert_eq!(props.len(), 2, "Category 401 should return 2 property descriptors");
            assert!(props.iter().any(|p| p.dict_get("propertyName").and_then(|v| v.as_str()) == Some("SolarSystem")));
            assert!(props.iter().any(|p| p.dict_get("propertyName").and_then(|v| v.as_str()) == Some("Character")));
        }
        _ => panic!("Expected list of KeyVal properties"),
    }

    // Query Category 102: Reimbursement
    let res_102 = ctx
        .dispatch_rpc(&mut sess, "petitioner", None, "GetCategoryProperties", &[PyRep::Integer(102)], &kw, None)
        .await
        .expect("GetCategoryProperties should succeed");

    match res_102 {
        PyRep::List(props) => {
            assert_eq!(props.len(), 2, "Category 102 should return 2 property descriptors");
            assert!(props.iter().any(|p| p.dict_get("propertyName").and_then(|v| v.as_str()) == Some("ShipType")));
            assert!(props.iter().any(|p| p.dict_get("propertyName").and_then(|v| v.as_str()) == Some("SolarSystem")));
        }
        _ => panic!("Expected list of KeyVal properties"),
    }
}
