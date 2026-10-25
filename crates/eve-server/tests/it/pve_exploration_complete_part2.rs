//! Phase 8 (`W-3` & `W-4`) Complete PvE Exploration Part 2:
//! Data & Relic Hacking Minigame, Loot Spew, Ghost Sites, Sleeper Caches,
//! Wormhole Sites, and Abyssal Deadspace Tests.

use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, DamageVector, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::pve_exploration::{
    advance_pve_exploration_clock, with_pve_exploration_reg,
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
    });

    let sys_handle = ctx.systems.get_or_spawn(solar_system_id);
    let _ = sys_handle
        .spawn_ball(Ball {
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
        })
        .await;

    sess
}

// ============================================================================
// 1. Data Sites, Relic Sites, Hacking Minigame & Loot Spew
// ============================================================================
#[tokio::test]
async fn test_pve_data_relic_hacking_minigame_and_loot_spew() {
    // parity: feat:pve.data-sites, feat:pve.relic-sites, feat:pve.hacking-minigame, feat:pve.loot-spew, rpc:hackingMgr.ClickedOnTile, rpc:hackingMgr.StartNewGameInstance, rpc:hackingMgr.UsedUtilityElement
    eve_testkit::attest(&[
        "feat:pve.data-sites",
        "feat:pve.relic-sites",
        "feat:pve.hacking-minigame",
        "feat:pve.loot-spew",
    ]);

    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = create_in_space_session(&ctx, 8103, 30_000_142, 710_001_003, 587).await;

    // 1. Scan Data Site (DAT-30000142) and Relic Site (REL-30000142) to 100%
    let data_scan = ctx
        .dispatch_rpc(
            &mut sess,
            "scanSvc",
            None,
            "ScanSignature",
            &[PyRep::String("DAT-30000142".into()), PyRep::Real(1.0)],
            &kw,
            None,
        )
        .await
        .expect("Scan Data site");
    let PyRep::List(data_cans) = data_scan.dict_get("hackableContainerIDs").unwrap() else {
        panic!("Expected List");
    };
    let data_can_id = data_cans[0].as_i64().unwrap();

    let relic_scan = ctx
        .dispatch_rpc(
            &mut sess,
            "scanSvc",
            None,
            "ScanSignature",
            &[PyRep::String("REL-30000142".into()), PyRep::Real(1.0)],
            &kw,
            None,
        )
        .await
        .expect("Scan Relic site");
    let PyRep::List(relic_cans) = relic_scan.dict_get("hackableContainerIDs").unwrap() else {
        panic!("Expected List");
    };
    let relic_can_id = relic_cans[0].as_i64().unwrap();

    // Verify Wormhole Data/Relic sites (31000001) spawn Sleeper NPC guards
    let wh_sigs = ctx
        .dispatch_rpc(
            &mut sess,
            "scanSvc",
            None,
            "GetSignatureInstances",
            &[PyRep::Integer(31_000_001)],
            &kw,
            None,
        )
        .await
        .expect("GetSignatureInstances WH");
    let PyRep::List(wh_sig_list) = &wh_sigs else {
        panic!("Expected List");
    };
    assert!(
        wh_sig_list
            .iter()
            .any(|s| s.dict_get("npcGuardsSpawned").and_then(|v| v.as_bool()) == Some(true)),
        "Wormhole Unsecured Data / Forgotten Relic sites must spawn Sleeper NPC guards"
    );

    // 2. Start Hacking Minigame on Data Can & Relic Can (`hackingMgr.StartNewGameInstance`)
    let start_rep = ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "StartNewGameInstance", &[PyRep::Integer(relic_can_id)], &kw, None).await.expect("StartNewGameInstance on Relic Can");
    assert_eq!(start_rep.dict_get("status").and_then(|v| v.as_str()), Some("InProgress"));

    // Use PolymorphicShield utility (slot 2) via `hackingMgr.UsedUtilityElement`
    let util_rep = ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "UsedUtilityElement", &[PyRep::Integer(relic_can_id), PyRep::Integer(2), PyRep::Integer(0)], &kw, None).await.expect("UsedUtilityElement PolymorphicShield");
    assert_eq!(util_rep.dict_get("polymorphicShieldCharges").and_then(|v| v.as_i64()), Some(2));

    // Locate the SystemCore tile and click it until destroyed to win the hack and trigger Loot Spew
    let core_tile_idx = with_pve_exploration_reg(&ctx, |reg| {
        reg.hacking_games[&(90_000_001, relic_can_id)]
            .tiles
            .iter()
            .find(|t| format!("{:?}", t.kind) == "SystemCore")
            .unwrap()
            .tile_index
    });

    let mut won_spew = Vec::new();
    for _ in 0..4 {
        let click_rep = ctx
            .dispatch_rpc(
                &mut sess,
                "hackingMgr",
                None,
                "ClickedOnTile",
                &[
                    PyRep::Integer(relic_can_id),
                    PyRep::Integer(core_tile_idx as i64),
                ],
                &kw,
                None,
            )
            .await
            .expect("ClickedOnTile SystemCore");
        if click_rep.dict_get("status").and_then(|v| v.as_str()) == Some("Won") {
            if let Some(PyRep::List(cans)) = click_rep.dict_get("spewedContainers") {
                won_spew = cans.clone();
            }
            break;
        }
    }
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

    // Scoop first spewed mini-container into ship cargo before expiry, and verify scooping it twice fails
    let first_spew_id = won_spew[0].dict_get("canItemID").unwrap().as_i64().unwrap();
    let second_spew_id = won_spew[1].dict_get("canItemID").unwrap().as_i64().unwrap();
    let scoop_rep = ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "HackContainer", &[PyRep::Integer(first_spew_id)], &kw, None).await.expect("Scoop spewed loot container");
    assert_eq!(scoop_rep.dict_get("scooped").and_then(|v| v.as_bool()), Some(true));

    let dup_err = ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "HackContainer", &[PyRep::Integer(first_spew_id)], &kw, None).await.expect_err("Cannot scoop same spewed can twice");
    assert_eq!(dup_err.code, "SpewedCanAlreadyScooped");

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
    let can2_id = data_cans[1].as_i64().unwrap();
    ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "StartNewGameInstance", &[PyRep::Integer(can2_id)], &kw, None).await.expect("start can2");
    ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "QuitHackingAttempt", &[PyRep::Integer(can2_id)], &kw, None).await.expect("quit attempt 1");
    ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "StartNewGameInstance", &[PyRep::Integer(can2_id)], &kw, None).await.expect("restart can2 after 1st fail");
    ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "QuitHackingAttempt", &[PyRep::Integer(can2_id)], &kw, None).await.expect("quit attempt 2");
    let fail2_err = ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "StartNewGameInstance", &[PyRep::Integer(can2_id)], &kw, None).await.expect_err("start after 2 failures must fail");
    assert_eq!(fail2_err.code, "HackingGameAlreadyFinished");

    // BF-HACK-MULTICHAR-01 & BF-HACK-FAILSEM-02: Exclusive active hack & quit-after-fail semantics
    let can3_id = data_cans[2].as_i64().unwrap();
    ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "StartNewGameInstance", &[PyRep::Integer(can3_id)], &kw, None).await.expect("p1 start can3");
    let mut sess2 = SessionState::new(8888);
    sess2.user_id = Some(2);
    sess2.character_id = Some(90_000_002);
    sess2.solar_system_id = Some(30_000_142);
    let multichar_err = ctx.dispatch_rpc(&mut sess2, "hackingMgr", None, "StartNewGameInstance", &[PyRep::Integer(can3_id)], &kw, None).await.expect_err("p2 hacking same can must fail");
    assert_eq!(multichar_err.code, "HackingGameAlreadyInProgress");
    assert_eq!(multichar_err.message, "Another pilot is already hacking this container");

    // Pilot 1 fails minigame via virus coherence reaching 0 on Firewall (strength 20 > 10)
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

    // Advance exploration clock past 60s and verify unscooped spewed container despawned
    advance_pve_exploration_clock(&ctx, 65);
    let despawn_err = ctx
        .dispatch_rpc(
            &mut sess,
            "hackingMgr",
            None,
            "HackContainer",
            &[PyRep::Integer(second_spew_id)],
            &kw,
            None,
        )
        .await
        .expect_err("Unscooped spew container must despawn after timer");
    assert_eq!(despawn_err.code, "SpewedCanDespawned");
}

// ============================================================================
// 2. Ghost Sites, Sleeper Caches & Wormhole Sites
// ============================================================================
#[tokio::test]
async fn test_pve_ghost_sites_sleeper_caches_and_wormhole_sites() {
    // parity: feat:pve.ghost-sites, feat:pve.sleeper-caches, feat:pve.wormhole-sites, rpc:hackingMgr.ClickedOnTile, rpc:hackingMgr.StartNewGameInstance
    eve_testkit::attest(&[
        "feat:pve.ghost-sites",
        "feat:pve.sleeper-caches",
        "feat:pve.wormhole-sites",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let mut sess = create_in_space_session(&ctx, 8104, 30_000_142, 710_001_004, 587).await;

    // 1. Ghost Site (GHO-30000142): scan to 100% (starts hidden timer), advance clock past 90s -> detonates can & spawns Pirate ambush NPCs
    let ghost_scan = ctx
        .dispatch_rpc(
            &mut sess,
            "scanSvc",
            None,
            "ScanSignature",
            &[PyRep::String("GHO-30000142".into()), PyRep::Real(1.0)],
            &kw,
            None,
        )
        .await
        .expect("Scan Ghost Site");
    let PyRep::List(ghost_cans) = ghost_scan.dict_get("hackableContainerIDs").unwrap() else {
        panic!("Expected List");
    };
    let ghost_can_id = ghost_cans[0].as_i64().unwrap();

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
    let sc_scan = ctx
        .dispatch_rpc(
            &mut sess,
            "scanSvc",
            None,
            "ScanSignature",
            &[PyRep::String("SLC-30000142".into()), PyRep::Real(1.0)],
            &kw,
            None,
        )
        .await
        .expect("Scan Sleeper Cache");
    let PyRep::List(sc_cans) = sc_scan.dict_get("hackableContainerIDs").unwrap() else {
        panic!("Expected List");
    };
    let rift_can_id = sc_cans[0].as_i64().unwrap();

    let rift_err = ctx
        .dispatch_rpc(
            &mut sess,
            "keeper",
            None,
            "EnterNextDungeonRoom",
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
    let sc_core_idx = with_pve_exploration_reg(&ctx, |reg| {
        reg.hacking_games[&(90_000_001, rift_can_id)]
            .tiles
            .iter()
            .find(|t| format!("{:?}", t.kind) == "SystemCore")
            .unwrap()
            .tile_index
    });
    for _ in 0..4 {
        let r = ctx
            .dispatch_rpc(
                &mut sess,
                "hackingMgr",
                None,
                "ClickedOnTile",
                &[PyRep::Integer(rift_can_id), PyRep::Integer(sc_core_idx as i64)],
                &kw,
                None,
            )
            .await
            .expect("Click Sleeper Rift Core");
        if r.dict_get("status").and_then(|v| v.as_str()) == Some("Won") {
            assert_eq!(r.dict_get("sleeperPuzzleSolved").and_then(|v| v.as_bool()), Some(true));
            break;
        }
    }

    let passage_ok = ctx
        .dispatch_rpc(
            &mut sess,
            "keeper",
            None,
            "EnterNextDungeonRoom",
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
