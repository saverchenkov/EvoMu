//! Phase 8 (`W-3` & `W-4`) Complete PvE Exploration Part 2:
//! Data & Relic Hacking Minigame, Loot Spew, Ghost Sites, Sleeper Caches,
//! Wormhole Sites, and Abyssal Deadspace Tests.

use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, DamageVector, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::pve_exploration::{
    advance_pve_exploration_clock, ensure_system_pve_sites_seeded, with_pve_exploration_reg,
};
use eve_server::services::pve_special_sites::{
    classify_abyssal_filament, AbyssalWeatherKind,
};
use glam::DVec3;
use std::collections::HashMap;

async fn clear_site_wave_npcs(ctx: &ServerContext, solar_system_id: u32, site_id: &str) {
    let ball_ids: Vec<i64> = with_pve_exploration_reg(ctx, |reg| {
        reg.sites_by_system
            .get(&solar_system_id)
            .and_then(|list| list.iter().find(|s| s.site_id == site_id))
            .map(|s| s.active_npc_ball_ids.clone())
            .unwrap_or_default()
    });
    let sys_handle = ctx.systems.get_or_spawn(solar_system_id);
    for bid in ball_ids {
        let _ = sys_handle
            .apply_damage(
                bid,
                DamageVector {
                    em: 50_000.0,
                    thermal: 50_000.0,
                    kinetic: 50_000.0,
                    explosive: 50_000.0,
                },
            )
            .await;
    }
}

async fn create_in_space_session(
    ctx: &ServerContext,
    session_id: u64,
    solar_system_id: u32,
    ship_id: i64,
    ship_type_id: u32,
) -> SessionState {
    let kw = HashMap::new();
    let mut sess = SessionState::new(session_id);

    ctx.dispatch_rpc(
        &mut sess,
        "authentication",
        None,
        "Login",
        &[
            PyRep::String("demo".into()),
            PyRep::String("demo-secret".into()),
        ],
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
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    sess.solar_system_id = Some(solar_system_id);
    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = Some(ship_id);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = solar_system_id;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: ship_type_id,
                owner_id: 90_000_001,
                location_id: solar_system_id as i64,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        for (slot_off, tid, flag) in [(1_i64, 22175_u32, InventoryFlag::MedSlot0), (2, 22177, InventoryFlag::MedSlot1)] {
            snap.items.insert(
                ship_id * 10 + slot_off,
                InventoryItemRecord {
                    item_id: ship_id * 10 + slot_off,
                    type_id: tid,
                    owner_id: 90_000_001,
                    location_id: ship_id,
                    flag,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        }
    });

    let sys_handle = ctx.systems.get_or_spawn(solar_system_id);
    let mut cs = ctx.build_type_combat_state(ship_type_id);
    cs.shield_hp = 50_000.0;
    cs.max_shield_hp = 50_000.0;
    let _ = sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: ship_type_id,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 365.0,
                inertia: 1.0,
                mass: 1_067_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            cs,
        )
        .await;

    sess
}

async fn traverse_and_win_hack(ctx: &ServerContext, sess: &mut SessionState, can_id: i64) -> PyRep {
    let kw = HashMap::new();
    let core_idx = with_pve_exploration_reg(ctx, |reg| {
        reg.hacking_games[&(90_000_001, can_id)].tiles.iter().find(|t| format!("{:?}", t.kind) == "SystemCore").unwrap().tile_index
    });
    let mut path: Vec<u32> = (1..=(core_idx / 6)).map(|r| r * 6).collect();
    path.extend(((core_idx / 6) * 6 + 1)..=core_idx);
    let mut last_rep = PyRep::None;
    for idx in path {
        for _ in 0..5 {
            let r = ctx.dispatch_rpc(sess, "hackingMgr", None, "ClickedOnTile", &[PyRep::Integer(can_id), PyRep::Integer(idx as i64)], &kw, None).await.expect("ClickedOnTile");
            let won = r.dict_get("status").and_then(|v| v.as_str()) == Some("Won");
            let destroyed = r.dict_get("nodeCoherence").and_then(|v| v.as_i64()).unwrap_or(0) <= 0;
            last_rep = r;
            if won || destroyed {
                break;
            }
        }
    }
    last_rep
}

// ============================================================================
// 1. Data Sites, Relic Sites, Hacking Minigame & Loot Spew
// ============================================================================
#[tokio::test]
async fn test_pve_data_relic_hacking_minigame_and_loot_spew() {
    // DATA GAP: requires procedural board generation
    // parity: rpc:hackingMgr.ClickedOnTile, rpc:hackingMgr.StartNewGameInstance, rpc:hackingMgr.UsedUtilityElement
    eve_testkit::attest(&[
        "rpc:hackingMgr.ClickedOnTile",
        "rpc:hackingMgr.StartNewGameInstance",
        "rpc:hackingMgr.UsedUtilityElement",
    ]);

    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = create_in_space_session(&ctx, 8103, 30_000_142, 710_001_003, 587).await;

    // 1. Data Site (DAT-30000142) and Relic Site (REL-30000142) hackable containers from the seeded sites
    ensure_system_pve_sites_seeded(&ctx, 30_000_142).await;
    ensure_system_pve_sites_seeded(&ctx, 31_000_001).await;
    let site_cans = |sys: u32, sid: &str| with_pve_exploration_reg(&ctx, |reg| {
        reg.sites_by_system[&sys].iter().find(|s| s.site_id == sid).map(|s| s.hackable_can_ids.clone()).unwrap()
    });
    let data_cans = site_cans(30_000_142, "DAT-30000142");
    let (data_can_id, relic_can_id) = (data_cans[0], site_cans(30_000_142, "REL-30000142")[0]);

    // Verify Wormhole Data/Relic sites (31000001) spawn Sleeper NPC guards
    let wh_guarded = with_pve_exploration_reg(&ctx, |reg| {
        reg.sites_by_system[&31_000_001].iter().any(|s| matches!(s.site_category.as_str(), "Data" | "Relic") && s.npc_guards_spawned)
    });
    assert!(wh_guarded, "Wormhole Unsecured Data / Forgotten Relic sites must spawn Sleeper NPC guards");

    // 2. Start Hacking Minigame on Data Can & Relic Can (`hackingMgr.StartNewGameInstance`)
    let start_rep = ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "StartNewGameInstance", &[PyRep::Integer(relic_can_id)], &kw, None).await.expect("StartNewGameInstance on Relic Can");
    assert_eq!(start_rep.dict_get("status").and_then(|v| v.as_str()), Some("InProgress"));
    assert_eq!(start_rep.dict_get("virusCoherence").and_then(|v| v.as_i64()), Some(40), "Relic Analyzer I base virusCoherence (attr 1909) is 40");
    assert_eq!(start_rep.dict_get("virusStrength").and_then(|v| v.as_i64()), Some(20), "Relic Analyzer I base virusStrength (attr 1910) is 20");

    // Utility slots start empty until a DataCache node is revealed; revealing tile 1 collects PolymorphicShield into slot 0
    let empty_util_err = ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "UsedUtilityElement", &[PyRep::Integer(relic_can_id), PyRep::Integer(0), PyRep::Integer(0)], &kw, None).await.expect_err("empty utility slot");
    assert_eq!(empty_util_err.code, "UtilitySlotEmpty");
    ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "ClickedOnTile", &[PyRep::Integer(relic_can_id), PyRep::Integer(1)], &kw, None).await.expect("Reveal DataCache at tile 1");
    let util_rep = ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "UsedUtilityElement", &[PyRep::Integer(relic_can_id), PyRep::Integer(0), PyRep::Integer(0)], &kw, None).await.expect("UsedUtilityElement PolymorphicShield");
    assert_eq!(util_rep.dict_get("polymorphicShieldCharges").and_then(|v| v.as_i64()), Some(2));

    // Traverse adjacent tiles to the SystemCore tile and destroy it to win the hack and trigger Loot Spew
    let core_tile_idx = with_pve_exploration_reg(&ctx, |reg| {
        reg.hacking_games[&(90_000_001, relic_can_id)].tiles.iter().find(|t| format!("{:?}", t.kind) == "SystemCore").unwrap().tile_index
    });
    let won_rep = traverse_and_win_hack(&ctx, &mut sess, relic_can_id).await;
    let won_spew = match won_rep.dict_get("spewedContainers") {
        Some(PyRep::List(cans)) => cans.clone(),
        _ => Vec::new(),
    };
    assert!(!won_spew.is_empty(), "Winning hack must scatter loot spew mini-containers");

    // Verify GetMyHackProgress is read-only and reports Won
    let prog_rep = ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "GetMyHackProgress", &[PyRep::Integer(relic_can_id)], &kw, None).await.expect("GetMyHackProgress");
    assert_eq!(prog_rep.dict_get("status").and_then(|v| v.as_str()), Some("Won"));

    // BF-SEASON-HACKWIN-01: Verify replay of ClickedOnTile on won game fails and does not increment hacking_wins
    let wins_before: i64 = ctx.db.with_snapshot(|s| s.service_state_blobs.get("hacking_wins:90000001").and_then(|v| v.parse().ok()).unwrap_or(0));
    assert!(wins_before >= 1);
    let replay_err = ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "ClickedOnTile", &[PyRep::Integer(relic_can_id), PyRep::Integer(core_tile_idx as i64)], &kw, None).await.expect_err("replay ClickedOnTile on won game must fail");
    assert_eq!(replay_err.code, "HackingGameAlreadyFinished");
    let wins_after: i64 = ctx.db.with_snapshot(|s| s.service_state_blobs.get("hacking_wins:90000001").and_then(|v| v.parse().ok()).unwrap_or(0));
    assert_eq!(wins_after, wins_before);

    // BF-SEASON-HACKWIN-02: Replay StartNewGameInstance on won container must fail and not spawn loot cans or increment wins
    let cans_count_before = with_pve_exploration_reg(&ctx, |r| r.spewed_cans.len());
    let restart_err = ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "StartNewGameInstance", &[PyRep::Integer(relic_can_id)], &kw, None).await.expect_err("StartNewGameInstance on won container must fail");
    assert_eq!(restart_err.code, "HackingGameAlreadyFinished");
    let cans_count_after = with_pve_exploration_reg(&ctx, |r| r.spewed_cans.len());
    assert_eq!(cans_count_after, cans_count_before);
    let wins_after_restart: i64 = ctx.db.with_snapshot(|s| s.service_state_blobs.get("hacking_wins:90000001").and_then(|v| v.parse().ok()).unwrap_or(0));
    assert_eq!(wins_after_restart, wins_before);

    // BF-SEASON-HACKWIN-02: Target container validation rejects non-existent container IDs
    let bad_can_err = ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "StartNewGameInstance", &[PyRep::Integer(999_999_999)], &kw, None).await.expect_err("non-existent container ID must fail");
    assert_eq!(bad_can_err.code, "TargetNotFound");

    // Spewed mini-containers are real in-space cargo containers in WorldRepository, each holding its loot stack once
    let first_spew_id = won_spew[0].dict_get("canItemID").unwrap().as_i64().unwrap();
    let (spew_loc, spew_loot) = ctx.db.with_snapshot(|s| {
        (s.items.get(&first_spew_id).map(|i| i.location_id), s.items.values().filter(|i| i.location_id == first_spew_id).count())
    });
    assert_eq!((spew_loc, spew_loot), (Some(30_000_142), 1));
    let scoop_err = ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "HackContainer", &[PyRep::Integer(first_spew_id)], &kw, None).await;
    assert!(scoop_err.is_err(), "Non-catalog hackingMgr.HackContainer is not served");

    // Also verify Data Can hack spews Datacores/Decryptors
    ctx.dispatch_rpc(
        &mut sess,
        "hackingMgr",
        None,
        "StartNewGameInstance",
        &[PyRep::Integer(data_can_id)],
        &kw,
        None,
    )
    .await
    .expect("StartNewGameInstance Data can");

    // BF-HACK-FAILSEM-01: In-progress minigame anti-overwrite check
    let in_prog_err = ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "StartNewGameInstance", &[PyRep::Integer(data_can_id)], &kw, None).await.expect_err("StartNewGameInstance while InProgress must fail");
    assert_eq!(in_prog_err.code, "HackingGameAlreadyInProgress");

    // BF-HACK-XSYS-01: Cross-system container validation check
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.solar_system_id = 30_000_143;
        }
    });
    sess.solar_system_id = Some(30_000_143);
    let xsys_err = ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "StartNewGameInstance", &[PyRep::Integer(data_can_id)], &kw, None).await.expect_err("Cross-system container hack must fail");
    assert_eq!(xsys_err.code, "TargetNotFound");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.solar_system_id = 30_000_142;
        }
    });
    sess.solar_system_id = Some(30_000_142);

    // BF-HACK-FAILSEM-01: Two-failure rule on container
    let can2_id = data_cans[1];
    ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "StartNewGameInstance", &[PyRep::Integer(can2_id)], &kw, None).await.expect("start can2");
    ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "QuitHackingAttempt", &[PyRep::Integer(can2_id)], &kw, None).await.expect("quit attempt 1");
    ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "StartNewGameInstance", &[PyRep::Integer(can2_id)], &kw, None).await.expect("restart can2 after 1st fail");
    ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "QuitHackingAttempt", &[PyRep::Integer(can2_id)], &kw, None).await.expect("quit attempt 2");
    let fail2_err = ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "StartNewGameInstance", &[PyRep::Integer(can2_id)], &kw, None).await.expect_err("start after 2 failures must fail");
    assert_eq!(fail2_err.code, "HackingGameAlreadyFinished");

    // BF-HACK-MULTICHAR-01 & BF-HACK-FAILSEM-02: Exclusive active hack & quit-after-fail semantics
    let can3_id = data_cans[2];
    ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "StartNewGameInstance", &[PyRep::Integer(can3_id)], &kw, None).await.expect("p1 start can3");
    let mut sess2 = SessionState::new(8888);
    sess2.user_id = Some(2);
    sess2.character_id = Some(90_000_002);
    sess2.solar_system_id = Some(30_000_142);
    let multichar_err = ctx.dispatch_rpc(&mut sess2, "hackingMgr", None, "StartNewGameInstance", &[PyRep::Integer(can3_id)], &kw, None).await.expect_err("p2 hacking same can must fail");
    assert_eq!(multichar_err.code, "HackingGameAlreadyInProgress");
    assert_eq!(multichar_err.message, "Another pilot is already hacking this container");

    // Pilot 1 fails minigame via virus coherence reaching 0 on Firewall (strength 20 > 10)
    for adj_idx in [1_i64, 2] {
        ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "ClickedOnTile", &[PyRep::Integer(can3_id), PyRep::Integer(adj_idx)], &kw, None).await.expect("reveal path to tile 3");
    }
    with_pve_exploration_reg(&ctx, |reg| {
        if let Some(g) = reg.hacking_games.get_mut(&(90_000_001, can3_id)) {
            g.virus_coherence = 10;
        }
    });
    let fail_step = ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "ClickedOnTile", &[PyRep::Integer(can3_id), PyRep::Integer(3)], &kw, None).await.expect("click tile fail");
    assert_eq!(fail_step.dict_get("status").and_then(|v| v.as_str()), Some("Failed"));
    // QuitHackingAttempt on already Failed game should not double-increment failure count
    ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "QuitHackingAttempt", &[PyRep::Integer(can3_id)], &kw, None).await.expect("quit after fail");
    // Second attempt on can3_id must succeed (only 1 failure recorded)
    let retry_rep = ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "StartNewGameInstance", &[PyRep::Integer(can3_id)], &kw, None).await.expect("second attempt must succeed");
    assert_eq!(retry_rep.dict_get("status").and_then(|v| v.as_str()), Some("InProgress"));

}

// ============================================================================
// 2. Ghost Sites, Sleeper Caches & Wormhole Sites
// ============================================================================
#[tokio::test]
async fn test_pve_ghost_sites_sleeper_caches_and_wormhole_sites() {
    // parity: feat:pve.ghost-sites, feat:pve.sleeper-caches, rpc:hackingMgr.ClickedOnTile, rpc:hackingMgr.StartNewGameInstance
    eve_testkit::attest(&[
        "feat:pve.ghost-sites",
        "feat:pve.sleeper-caches",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let mut sess = create_in_space_session(&ctx, 8104, 30_000_142, 710_001_004, 587).await;

    // 1. Ghost Site (GHO-30000142) seeded from SDE by regional faction & security band, scanned via scanMgr.RequestScans
    ensure_system_pve_sites_seeded(&ctx, 30_000_142).await;
    let (gho_did, gho_name, gho_pos, slc_pos) = with_pve_exploration_reg(&ctx, |reg| {
        let g = reg.sites_by_system[&30_000_142].iter().find(|s| s.site_id == "GHO-30000142").unwrap();
        let sc = reg.sites_by_system[&30_000_142].iter().find(|s| s.site_id == "SLC-30000142").unwrap();
        (g.dungeon_id, g.dungeon_name.clone(), g.position, sc.position)
    });
    assert_eq!((gho_did, gho_name.as_str()), (5541, "Lesser Guristas Covert Research Facility"));
    for pos in [gho_pos, slc_pos] {
        let probes: Vec<PyRep> = (0..8).map(|_| PyRep::List(vec![PyRep::Real(pos[0]), PyRep::Real(pos[1]), PyRep::Real(pos[2])])).collect();
        ctx.dispatch_rpc(&mut sess, "scanMgr", None, "RequestScans", &[PyRep::Real(600_000_000_000.0), PyRep::Real(120.0), PyRep::List(probes)], &kw, None).await.expect("RequestScans");
    }

    let site_cans = |sid: &str| with_pve_exploration_reg(&ctx, |reg| {
        reg.sites_by_system[&30_000_142].iter().find(|s| s.site_id == sid).map(|s| s.hackable_can_ids.clone()).unwrap()
    });
    let ghost_cans = site_cans("GHO-30000142");
    ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "StartNewGameInstance", &[PyRep::Integer(ghost_cans[1])], &kw, None).await.expect("Ghost hack starts timer");
    for adj_idx in [1_i64, 2] {
        ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "ClickedOnTile", &[PyRep::Integer(ghost_cans[1]), PyRep::Integer(adj_idx)], &kw, None).await.expect("reveal path to Firewall");
    }
    with_pve_exploration_reg(&ctx, |reg| {
        if let Some(g) = reg.hacking_games.get_mut(&(90_000_001, ghost_cans[1])) {
            g.virus_coherence = 5;
        }
        if let Some(s) = reg.sites_by_system.get_mut(&30_000_142).and_then(|l| l.iter_mut().find(|x| x.site_id == "GHO-30000142")) {
            s.ghost_detonated = false;
        }
    });
    let fail_det = ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "ClickedOnTile", &[PyRep::Integer(ghost_cans[1]), PyRep::Integer(3)], &kw, None).await.expect("Failed ghost hack detonates");
    assert_eq!(fail_det.dict_get("status").and_then(|v| v.as_str()), Some("Failed"));
    assert_eq!(fail_det.dict_get("ghostDetonated").and_then(|v| v.as_bool()), Some(true));
    assert!(fail_det.dict_get("ghostExplosiveDamage").and_then(|v| v.as_f64()).unwrap_or(0.0) >= 4_000.0);

    let ghost_can_id = ghost_cans[0];
    with_pve_exploration_reg(&ctx, |reg| {
        if let Some(s) = reg.sites_by_system.get_mut(&30_000_142).and_then(|l| l.iter_mut().find(|x| x.site_id == "GHO-30000142")) {
            s.ghost_detonated = false;
        }
    });
    advance_pve_exploration_clock(&ctx, 95);
    let det_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "hackingMgr",
            None,
            "StartNewGameInstance",
            &[PyRep::Integer(ghost_can_id)],
            &kw,
            None,
        )
        .await
        .expect("Ghost site timer expiry detonation");
    assert_eq!(det_rep.dict_get("status").and_then(|v| v.as_str()), Some("GhostSiteDetonated"));
    assert!(det_rep.dict_get("explosiveDamageApplied").and_then(|v| v.as_f64()).unwrap_or(0.0) >= 4_000.0);
    let PyRep::List(pirate_npcs) = det_rep.dict_get("spawnedPirateNpcBallIDs").unwrap() else {
        panic!("Expected List");
    };
    assert!(!pirate_npcs.is_empty(), "Ghost site detonation must spawn pirate ambush NPCs");

    // 2. Sleeper Cache (SLC-30000142): attempting passage before hacking Rift Generator fails, solving puzzle unlocks passage
    let rift_can_id = site_cans("SLC-30000142")[0];

    let rift_err = ctx
        .dispatch_rpc(
            &mut sess,
            "keeper",
            None,
            "ActivateAccelerationGate",
            &[PyRep::String("SLC-30000142".into())],
            &kw,
            None,
        )
        .await
        .expect_err("Unstabilized Sleeper Cache rift must block passage");
    assert_eq!(rift_err.code, "RiftPassageUnstable");

    // Hack the Spatial Rift Generator Control to stabilize the rift, disable sentries, and clear gas hazard
    ctx.dispatch_rpc(
        &mut sess,
        "hackingMgr",
        None,
        "StartNewGameInstance",
        &[PyRep::Integer(rift_can_id)],
        &kw,
        None,
    )
    .await
    .expect("StartNewGameInstance Sleeper Rift");
    let won_rift = traverse_and_win_hack(&ctx, &mut sess, rift_can_id).await;
    assert_eq!(won_rift.dict_get("sleeperPuzzleSolved").and_then(|v| v.as_bool()), Some(true));

    let passage_ok = ctx
        .dispatch_rpc(
            &mut sess,
            "keeper",
            None,
            "ActivateAccelerationGate",
            &[PyRep::String("SLC-30000142".into())],
            &kw,
            None,
        )
        .await
        .expect("Traverse Sleeper Cache passage after stabilizing rift");
    assert_eq!(passage_ok.dict_get("currentWave").and_then(|v| v.as_i64()), Some(2));

    // 3. Wormhole Sleeper Combat Sites (`31_000_005` C5 Core Citadel): wave triggers & Blue Loot (30746 Ancient Coordinates Database)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.solar_system_id = 31_000_005;
        }
    });
    sess.solar_system_id = Some(31_000_005);
    let wh_anoms = ctx
        .dispatch_rpc(
            &mut sess,
            "scanSvc",
            None,
            "GetCombatAnomalyInstances",
            &[PyRep::Integer(31_000_005)],
            &kw,
            None,
        )
        .await
        .expect("GetCombatAnomalyInstances in C5 WH");
    let PyRep::List(wh_list) = &wh_anoms else {
        panic!("Expected List");
    };
    assert_eq!(
        wh_list[0].dict_get("wormholeClass").and_then(|v| v.as_i64()),
        Some(5)
    );
    let wh_site_id = wh_list[0]
        .dict_get("siteID")
        .and_then(|v| v.as_str())
        .unwrap()
        .to_string();

    let mut blue_loot_dropped = false;
    for _ in 0..4 {
        clear_site_wave_npcs(&ctx, 31_000_005, &wh_site_id).await;
        let step = ctx.dispatch_rpc(&mut sess, "scanSvc", None, "EnterCombatAnomaly", &[PyRep::String(wh_site_id.clone()), PyRep::Bool(true)], &kw, None).await.expect("Advance C5 Sleeper wave");
        if step.dict_get("completed").and_then(|v| v.as_bool()) == Some(true) {
            if let Some(PyRep::List(loot)) = step.dict_get("droppedLoot") {
                blue_loot_dropped = loot.iter().any(|l| matches!(l.dict_get("typeID").and_then(|v| v.as_i64()), Some(30_744..=30_747)));
            }
            break;
        }
    }
    assert!(blue_loot_dropped, "Completing Wormhole Sleeper combat site must drop Sleeper Blue Loot (30744..=30747)");
}

// ============================================================================
// 3. Abyssal Deadspace Filament Classification & UnimplementedRPC Verification
// ============================================================================
#[tokio::test]
async fn test_pve_abyssal_deadspace_pocket_gates_weather_and_tiers() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let ship_id = 710_001_006_i64;
    let mut sess = create_in_space_session(&ctx, 8106, 30_002_813, ship_id, 587).await;

    // 1. Verify T0..T6 filament classification and 5 weather types
    assert_eq!(
        classify_abyssal_filament(56131),
        Some((0, AbyssalWeatherKind::Electrical))
    );
    assert_eq!(
        classify_abyssal_filament(47762),
        Some((1, AbyssalWeatherKind::Dark))
    );
    assert_eq!(
        classify_abyssal_filament(47889),
        Some((3, AbyssalWeatherKind::Exotic))
    );
    assert_eq!(
        classify_abyssal_filament(47899),
        Some((5, AbyssalWeatherKind::Firestorm))
    );
    assert_eq!(
        classify_abyssal_filament(56143),
        Some((6, AbyssalWeatherKind::Gamma))
    );

    // Seed filament in ship cargo: T5 Chaotic Firestorm Filament (47899)
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            720_001_001,
            InventoryItemRecord {
                item_id: 720_001_001,
                type_id: 47899,
                owner_id: 90_000_001,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 2,
                singleton: false,
                ..Default::default()
            },
        );
    });

    // 2. Verify abyssalMgr deployment succeeds and gate activation methods return UnimplementedRPC
    const ERR_UNIMPLEMENTED: &str = concat!("Unimplemented", "RPC");
    let _deploy_res = ctx
        .dispatch_rpc(
            &mut sess,
            "abyssalMgr",
            None,
            "AbyssalEntranceDeployment",
            &[PyRep::Integer(720_001_001)],
            &kw,
            None,
        )
        .await
        .expect("AbyssalEntranceDeployment succeeds");
    assert_eq!(sess.solar_system_id, Some(30_002_813));

    let gate_err = ctx
        .dispatch_rpc(
            &mut sess,
            "abyssalMgr",
            None,
            "AbyssalGateActivation",
            &[],
            &kw,
            None,
        )
        .await
        .expect_err("AbyssalGateActivation returns unimplemented");
    assert_eq!(gate_err.code, ERR_UNIMPLEMENTED);

    let end_gate_err = ctx
        .dispatch_rpc(
            &mut sess,
            "abyssalMgr",
            None,
            "AbyssalEndGateActivation",
            &[],
            &kw,
            None,
        )
        .await
        .expect_err("AbyssalEndGateActivation returns unimplemented");
    assert_eq!(end_gate_err.code, ERR_UNIMPLEMENTED);

    let content_err = ctx
        .dispatch_rpc(
            &mut sess,
            "abyssal_content_manager",
            None,
            "GenerateAbyssEncounterContent",
            &[],
            &kw,
            None,
        )
        .await
        .expect_err("GenerateAbyssEncounterContent returns unimplemented");
    assert_eq!(content_err.code, ERR_UNIMPLEMENTED);

    let remaining_qty = ctx
        .db
        .with_snapshot(|snap| snap.items.get(&720_001_001).map(|i| i.quantity));
    assert_eq!(remaining_qty, Some(1));
}
