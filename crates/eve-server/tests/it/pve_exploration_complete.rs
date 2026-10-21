//! Phase 8 (`W-3` & `W-4`) Complete PvE Exploration, DED Complexes, Escalations, Hacking,
//! Loot Spew, Ghost Sites, Sleeper Caches, Wormhole Sites, Sansha Incursions, Abyssal Deadspace,
//! Homefront Ops, Metaliminal Storms, Drifter Hives, Triglavian Sites & Interference Tests.
//!
//! Ledger tags live inside each `#[tokio::test]` body next to its `eve_testkit::attest` call.

use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::attr_ids;
use eve_server::router::ServerContext;
use eve_server::services::pve_exploration::{
    advance_pve_exploration_clock, with_pve_exploration_reg,
};
use eve_server::services::pve_special_sites::{
    advance_pve_special_clock, classify_abyssal_filament, record_system_interference_activity,
    with_pve_special_reg, AbyssalWeatherKind,
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
        let _ = sys_handle.remove_ball(bid).await;
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
// 1. Combat Anomalies (True-Sec Tiers & Multi-Wave Triggers), Escalations & Expeditions
// ============================================================================
#[tokio::test]
async fn test_pve_combat_anomalies_waves_and_escalations() {
    // parity: feat:pve.combat-anomalies, feat:pve.escalations, feat:pve.expeditions
    eve_testkit::attest(&[
        "feat:pve.combat-anomalies",
        "feat:pve.escalations",
        "feat:pve.expeditions",
    ]);

    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = create_in_space_session(&ctx, 8101, 30_000_142, 710_001_001, 587).await;

    // 1. Verify true-sec combat anomaly tiers across High-Sec (Jita 30000142), Low-Sec (Tama 30002813), Null-Sec (VFK-IV 30002904)
    let hs_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "scanSvc",
            None,
            "GetCombatAnomalyInstances",
            &[PyRep::Integer(30_000_142)],
            &kw,
            None,
        )
        .await
        .expect("High-sec combat anomalies");
    let PyRep::List(hs_list) = &hs_rep else {
        panic!("Expected List of combat anomalies");
    };
    assert!(!hs_list.is_empty());
    let hs_names: Vec<&str> = hs_list
        .iter()
        .filter_map(|x| x.dict_get("dungeonName").and_then(|v| v.as_str()))
        .collect();
    assert!(
        hs_names.iter().any(|n| n.contains("Hideaway")),
        "High-sec must spawn Hideaway anomalies: {hs_names:?}"
    );

    let ls_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "scanSvc",
            None,
            "GetCombatAnomalyInstances",
            &[PyRep::Integer(30_002_813)],
            &kw,
            None,
        )
        .await
        .expect("Low-sec combat anomalies");
    let PyRep::List(ls_list) = &ls_rep else {
        panic!("Expected List");
    };
    let ls_names: Vec<&str> = ls_list
        .iter()
        .filter_map(|x| x.dict_get("dungeonName").and_then(|v| v.as_str()))
        .collect();
    assert!(
        ls_names.iter().any(|n| n.contains("Vigil") || n.contains("Haven")),
        "Low-sec must spawn Vigil/Haven anomalies: {ls_names:?}"
    );

    let ns_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "scanSvc",
            None,
            "GetCombatAnomalyInstances",
            &[PyRep::Integer(30_002_904)],
            &kw,
            None,
        )
        .await
        .expect("Null-sec combat anomalies");
    let PyRep::List(ns_list) = &ns_rep else {
        panic!("Expected List");
    };
    let ns_names: Vec<&str> = ns_list
        .iter()
        .filter_map(|x| x.dict_get("dungeonName").and_then(|v| v.as_str()))
        .collect();
    assert!(
        ns_names.iter().any(|n| n.contains("Sanctum")),
        "Null-sec must spawn Sanctum anomalies: {ns_names:?}"
    );

    // 2. Enter High-Sec Angel Hideaway (2 waves): verify WaveNpcsRemaining when hostiles are alive, then destroy Wave 1 -> Wave 2 -> Complete + Escalate
    let site_id = "PVE-ANOM-30000142-1";
    let alive_err = ctx.dispatch_rpc(&mut sess, "scanSvc", None, "EnterCombatAnomaly", &[PyRep::String(site_id.into()), PyRep::Bool(true)], &kw, None).await.expect_err("Cannot advance wave while NPCs are alive");
    assert_eq!(alive_err.code, "WaveNpcsRemaining");

    clear_site_wave_npcs(&ctx, 30_000_142, site_id).await;
    let wave2_rep = ctx.dispatch_rpc(&mut sess, "scanSvc", None, "EnterCombatAnomaly", &[PyRep::String(site_id.into()), PyRep::Bool(true)], &kw, None).await.expect("Advance to Wave 2");
    assert_eq!(wave2_rep.dict_get("currentWave").and_then(|v| v.as_i64()), Some(2));
    assert_eq!(wave2_rep.dict_get("completed").and_then(|v| v.as_bool()), Some(false));

    clear_site_wave_npcs(&ctx, 30_000_142, site_id).await;
    let done_rep = ctx.dispatch_rpc(&mut sess, "scanSvc", None, "EnterCombatAnomaly", &[PyRep::String(site_id.into()), PyRep::Bool(true)], &kw, None).await.expect("Clear final Wave 2");
    assert_eq!(done_rep.dict_get("completed").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(done_rep.dict_get("escalated").and_then(|v| v.as_bool()), Some(true));

    // Verify OnCosmicAnomalyRemoved and OnEscalatingPathMessage notifications were pushed
    let notifs = sess.drain_notifications();
    assert!(notifs.iter().any(|n| n.0 == "OnCosmicAnomalyRemoved"));
    assert!(notifs.iter().any(|n| n.0 == "OnEscalatingPathMessage"));

    // 3. Query dungeonExplorationMgr.GetMyEscalatingPathDetails & GetEscalatingPathDungeonsEntered
    let esc_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "dungeonExplorationMgr",
            None,
            "GetMyEscalatingPathDetails",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetMyEscalatingPathDetails");
    let PyRep::List(esc_list) = &esc_rep else {
        panic!("Expected List from GetMyEscalatingPathDetails");
    };
    assert!(!esc_list.is_empty(), "Active escalation must be returned in journal");
    let first_esc = &esc_list[0];
    assert!(
        first_esc
            .dict_get("destinationSolarSystemID")
            .and_then(|v| v.as_i64())
            .unwrap_or(0)
            > 30_000_000
    );
    assert!(
        esc_list.iter().any(|e| {
            e.dict_get("journalText")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .contains("Chasing the Dragon")
        }),
        "Escalation journal must contain 'Chasing the Dragon'"
    );

    let entered_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "dungeonExplorationMgr",
            None,
            "GetEscalatingPathDungeonsEntered",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetEscalatingPathDungeonsEntered");
    let PyRep::List(entered_list) = &entered_rep else {
        panic!("Expected List");
    };
    assert!(!entered_list.is_empty());

    // Advance exploration clock past 24h (86,401s) and verify expedition expires
    advance_pve_exploration_clock(&ctx, 86_401);
    let expired_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "dungeonExplorationMgr",
            None,
            "GetMyEscalatingPathDetails",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetMyEscalatingPathDetails after expiry");
    let PyRep::List(expired_list) = &expired_rep else {
        panic!("Expected List");
    };
    assert!(expired_list.is_empty(), "Expired expedition must be removed from journal");
}

#[tokio::test]
async fn test_pve_combat_signatures_and_ded_complexes_restrictions_and_loot() {
    // parity: feat:pve.combat-signatures, feat:pve.ded-complexes
    eve_testkit::attest(&["feat:pve.combat-signatures", "feat:pve.ded-complexes"]);

    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = create_in_space_session(&ctx, 8102, 30_000_142, 710_001_002, 587).await;

    // 1. Before scanning to 100%, DED combat signature is hidden from overview and unwarpable
    let sigs_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "scanSvc",
            None,
            "GetSignatureInstances",
            &[PyRep::Integer(30_000_142)],
            &kw,
            None,
        )
        .await
        .expect("GetSignatureInstances");
    let PyRep::List(sigs) = &sigs_rep else {
        panic!("Expected List");
    };
    let ded_sig = sigs
        .iter()
        .find(|s| {
            s.dict_get("signatureID")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .starts_with("DED-")
        })
        .expect("Must have DED signature");
    assert_eq!(ded_sig.dict_get("signalStrength").and_then(|v| v.as_f64()), Some(0.0));
    assert_eq!(ded_sig.dict_get("overviewVisible").and_then(|v| v.as_bool()), Some(false));
    assert_eq!(ded_sig.dict_get("warpable").and_then(|v| v.as_bool()), Some(false));
    assert_eq!(
        ded_sig.dict_get("siteName").and_then(|v| v.as_str()),
        Some("Unidentified Cosmic Signature")
    );

    // Partial scan (50%) still does not appear on the overview
    let partial_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "scanSvc",
            None,
            "ScanSignature",
            &[
                PyRep::String("DED-30000142-3".into()),
                PyRep::Real(0.50),
            ],
            &kw,
            None,
        )
        .await
        .expect("Partial ScanSignature");
    assert_eq!(partial_rep.dict_get("overviewVisible").and_then(|v| v.as_bool()), Some(false));

    // Scan to 100% reveals DED 3/10 Guristas Guerilla Grounds and makes it overview-visible
    let full_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "scanSvc",
            None,
            "ScanSignature",
            &[
                PyRep::String("DED-30000142-3".into()),
                PyRep::Real(0.60),
            ],
            &kw,
            None,
        )
        .await
        .expect("Full 100% ScanSignature");
    assert_eq!(full_rep.dict_get("signalStrength").and_then(|v| v.as_f64()), Some(1.0));
    assert_eq!(full_rep.dict_get("overviewVisible").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(full_rep.dict_get("warpable").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(full_rep.dict_get("dedRating").and_then(|v| v.as_i64()), Some(3));
    assert_eq!(
        full_rep.dict_get("siteName").and_then(|v| v.as_str()),
        Some("Guristas Guerilla Grounds")
    );

    // 2. Verify DED 1/10..10/10 ship size gate restrictions (`keeper.CanEnterDungeonInCurrentShip` & `EnterNextDungeonRoom`)
    // Rifter (587, Frigate group 25) is allowed into DED 1/10
    let can_frigate = ctx
        .dispatch_rpc(
            &mut sess,
            "keeper",
            None,
            "CanEnterDungeonInCurrentShip",
            &[PyRep::Integer(1)],
            &kw,
            None,
        )
        .await
        .expect("CanEnterDungeonInCurrentShip Rifter in 1/10");
    assert_eq!(can_frigate.dict_get("canEnter").and_then(|v| v.as_bool()), Some(true));

    // Switch pilot into a Raven Battleship (type_id 638, group 27, mass ~99,300,000 kg) -> rejected by DED 3/10 gate!
    let raven_ship_id = 710_001_099_i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.active_ship_id = raven_ship_id;
        }
        snap.items.insert(
            raven_ship_id,
            InventoryItemRecord {
                item_id: raven_ship_id,
                type_id: 638,
                owner_id: 90_000_001,
                location_id: 30_000_142,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    sess.ship_id = Some(raven_ship_id);
    let gate_err = ctx
        .dispatch_rpc(
            &mut sess,
            "keeper",
            None,
            "EnterNextDungeonRoom",
            &[PyRep::String("DED-30000142-3".into())],
            &kw,
            None,
        )
        .await
        .expect_err("Battleship must be rejected by DED 3/10 gate");
    assert_eq!(gate_err.code, "ShipSizeRestricted");

    // Switch back to Rifter (587) and clear all 3 waves/rooms of DED 3/10 to drop 6th Tier Overseer's Personal Effects (19405)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.active_ship_id = 710_001_002;
        }
    });
    sess.ship_id = Some(710_001_002);
    for wave in 1..=3 {
        clear_site_wave_npcs(&ctx, 30_000_142, "DED-30000142-3").await;
        let step_rep = ctx.dispatch_rpc(&mut sess, "keeper", None, "EnterNextDungeonRoom", &[PyRep::String("DED-30000142-3".into())], &kw, None).await.expect("EnterNextDungeonRoom in Rifter");
        if wave == 3 {
            assert_eq!(step_rep.dict_get("completed").and_then(|v| v.as_bool()), Some(true));
            let PyRep::List(loot) = step_rep.dict_get("droppedLoot").expect("droppedLoot") else {
                panic!("Expected List of droppedLoot");
            };
            assert!(
                loot.iter().any(|item| item.dict_get("typeID").and_then(|v| v.as_i64()) == Some(19_405)),
                "Completing DED 3/10 must drop 6th Tier Overseer's Personal Effects (19405)"
            );
        }
    }
}

// ============================================================================
// 3. Data Sites, Relic Sites, Hacking Minigame & Loot Spew
// ============================================================================
#[tokio::test]
async fn test_pve_data_relic_hacking_minigame_and_loot_spew() {
    // parity: feat:pve.data-sites, feat:pve.relic-sites, feat:pve.hacking-minigame, feat:pve.loot-spew
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
    let start_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "hackingMgr",
            None,
            "StartNewGameInstance",
            &[PyRep::Integer(relic_can_id)],
            &kw,
            None,
        )
        .await
        .expect("StartNewGameInstance on Relic Can");
    assert_eq!(start_rep.dict_get("status").and_then(|v| v.as_str()), Some("InProgress"));

    // Use PolymorphicShield utility (slot 2) via `hackingMgr.UsedUtilityElement`
    let util_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "hackingMgr",
            None,
            "UsedUtilityElement",
            &[
                PyRep::Integer(relic_can_id),
                PyRep::Integer(2),
                PyRep::Integer(0),
            ],
            &kw,
            None,
        )
        .await
        .expect("UsedUtilityElement PolymorphicShield");
    assert_eq!(
        util_rep
            .dict_get("polymorphicShieldCharges")
            .and_then(|v| v.as_i64()),
        Some(2)
    );

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

#[tokio::test]
async fn test_pve_ghost_sites_sleeper_caches_and_wormhole_sites() {
    // parity: feat:pve.ghost-sites, feat:pve.sleeper-caches, feat:pve.wormhole-sites
    eve_testkit::attest(&[
        "feat:pve.ghost-sites",
        "feat:pve.sleeper-caches",
        "feat:pve.wormhole-sites",
    ]);

    let ctx = ServerContext::new();
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
// 5. Sansha Incursions, Fleet ISK + CONCORD LP Payouts & Mothership HQ Finale
// ============================================================================
#[tokio::test]
async fn test_pve_incursions_payouts_and_mothership_finale() {
    // parity: feat:pve.incursion-system, feat:pve.incursion-payout, feat:pve.incursion-mothership
    eve_testkit::attest(&[
        "feat:pve.incursion-system",
        "feat:pve.incursion-payout",
        "feat:pve.incursion-mothership",
    ]);

    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = create_in_space_session(&ctx, 8105, 30_000_145, 710_001_005, 587).await;

    // 1. Query active constellation incursion and verify Mothership HQ site is locked at high influence (1.0)
    let inc_rep = ctx.dispatch_rpc(&mut sess, "incursion", None, "GetAllIncursionSites", &[PyRep::Integer(20_000_020)], &kw, None).await.expect("GetAllIncursionSites");
    assert_eq!(inc_rep.dict_get("influence").and_then(|v| v.as_f64()), Some(1.0));
    assert_eq!(inc_rep.dict_get("mothershipSpawned").and_then(|v| v.as_bool()), Some(false));

    let locked_err = ctx.dispatch_rpc(&mut sess, "incursion", None, "UpdateIncursionLPLog", &[PyRep::String("INC-MS-5158".into())], &kw, None).await.expect_err("Mothership site must be locked while influence > 0.25");
    assert_eq!(locked_err.code, "MothershipSiteLocked");

    // Verify UpdateIncursionLPLog rejects when HQ boss NPC ball is still alive in space
    let npcs_alive_err = ctx.dispatch_rpc(&mut sess, "incursion", None, "UpdateIncursionLPLog", &[PyRep::String("INC-HQ-5175".into())], &kw, None).await.expect_err("Cannot claim incursion payout before destroying site boss NPC");
    assert_eq!(npcs_alive_err.code, "IncursionSiteNpcsRemaining");

    // 2. Complete HQ sites (`INC-HQ-5175`, -0.25 influence each) 3 times to drop influence from 1.0 -> 0.25 and unlock Mothership
    let hq_sys = ctx.systems.get_or_spawn(30_000_145);
    for _ in 0..3 {
        let boss_bid = with_pve_special_reg(&ctx, |reg| {
            reg.incursions[&20_000_020].sites.iter().find(|s| s.site_id == "INC-HQ-5175").unwrap().boss_npc_ball_id
        });
        let _ = hq_sys.remove_ball(boss_bid).await;
        let hq_pay = ctx.dispatch_rpc(&mut sess, "incursion", None, "UpdateIncursionLPLog", &[PyRep::String("INC-HQ-5175".into()), PyRep::List(vec![PyRep::Integer(90_000_001)])], &kw, None).await.expect("Complete HQ incursion site");
        assert_eq!(hq_pay.dict_get("iskPerPilot").and_then(|v| v.as_i64()), Some(31_500_000));
        assert_eq!(hq_pay.dict_get("concordLpPerPilot").and_then(|v| v.as_i64()), Some(7_000));
    }

    // 3. Mothership (`INC-MS-5158`, The Kundalini Manifest) is now unlocked at influence <= 0.25; destroy boss and complete it to end the incursion
    let ms_boss_bid = with_pve_special_reg(&ctx, |reg| {
        reg.incursions[&20_000_020].sites.iter().find(|s| s.site_id == "INC-MS-5158").unwrap().boss_npc_ball_id
    });
    let _ = hq_sys.remove_ball(ms_boss_bid).await;
    let ms_pay = ctx.dispatch_rpc(&mut sess, "incursion", None, "UpdateIncursionLPLog", &[PyRep::String("INC-MS-5158".into()), PyRep::List(vec![PyRep::Integer(90_000_001)])], &kw, None).await.expect("Complete Mothership HQ site");
    assert_eq!(ms_pay.dict_get("iskPerPilot").and_then(|v| v.as_i64()), Some(63_000_000));
    assert_eq!(ms_pay.dict_get("concordLpPerPilot").and_then(|v| v.as_i64()), Some(14_000));
    assert_eq!(ms_pay.dict_get("influence").and_then(|v| v.as_f64()), Some(0.0));
    assert_eq!(ms_pay.dict_get("state").and_then(|v| v.as_str()), Some("Ended"));
    assert_eq!(ms_pay.dict_get("incursionEnded").and_then(|v| v.as_bool()), Some(true));

    let concord_lp = ctx.db.with_snapshot(|snap| {
        snap.characters
            .get(&90_000_001)
            .and_then(|c| c.loyalty_points.get(&1_000_125).copied())
            .unwrap_or(0)
    });
    assert!(
        concord_lp >= 35_000,
        "CONCORD LP balance must accumulate HQ + Mothership payouts: {concord_lp}"
    );
}

// ============================================================================
// 6. Abyssal Deadspace Pocket, Gates, Weather Dbuffs & Filament Tiers (T0-T6)
// ============================================================================
#[tokio::test]
async fn test_pve_abyssal_deadspace_pocket_gates_weather_and_tiers() {
    // parity: feat:pve.abyssal-pocket, feat:pve.abyssal-gates, feat:pve.abyssal-weather, feat:pve.abyssal-tiers
    eve_testkit::attest(&[
        "feat:pve.abyssal-pocket",
        "feat:pve.abyssal-gates",
        "feat:pve.abyssal-weather",
        "feat:pve.abyssal-tiers",
    ]);

    let ctx = ServerContext::new();
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

    // Seed 2 filaments in ship cargo: a T5 Chaotic Firestorm Filament (47899) and a T6 Cataclysmic Gamma Filament (56143)
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            720_001_001,
            InventoryItemRecord {
                item_id: 720_001_001,
                type_id: 47899,
                owner_id: 90_000_001,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                ..Default::default()
            },
        );
        snap.items.insert(
            720_001_002,
            InventoryItemRecord {
                item_id: 720_001_002,
                type_id: 56143,
                owner_id: 90_000_001,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                ..Default::default()
            },
        );
    });

    // 2. Activate T5 Chaotic Firestorm Filament (`47899`) -> enters Room 1 of 3-room Abyssal pocket with Firestorm weather
    let enter_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "abyssal_content_manager",
            None,
            "ActivateAbyssalKey",
            &[PyRep::Integer(47899)],
            &kw,
            None,
        )
        .await
        .expect("ActivateAbyssalKey T5 Firestorm");
    assert_eq!(enter_rep.dict_get("tier").and_then(|v| v.as_i64()), Some(5));
    assert_eq!(
        enter_rep.dict_get("weather").and_then(|v| v.as_str()),
        Some("Firestorm")
    );
    assert_eq!(
        enter_rep.dict_get("weatherPenaltyPct").and_then(|v| v.as_f64()),
        Some(70.0)
    );
    assert_eq!(
        enter_rep.dict_get("timerSeconds").and_then(|v| v.as_i64()),
        Some(1_200)
    );
    assert_eq!(sess.solar_system_id, Some(32_000_001));

    // Verify Firestorm weather modified ship's ARMOR_HP (+50%) and ARMOR_THERMAL_DAMAGE_RESONANCE (+70% resonance)
    let (boosted_armor, boosted_therm_res) = ctx.db.with_snapshot(|snap| {
        let ship = &snap.items[&ship_id];
        (
            ship.dynamic_attributes[&attr_ids::ARMOR_HP],
            ship.dynamic_attributes[&attr_ids::ARMOR_THERMAL_DAMAGE_RESONANCE],
        )
    });
    assert!(boosted_armor > 400.0);
    assert!(boosted_therm_res > 0.65);

    // 3. Transition Room 1 -> Room 2 -> Room 3 via `AbyssalGateActivation`, then exit via `AbyssalEndGateActivation`
    let r2 = ctx
        .dispatch_rpc(
            &mut sess,
            "abyssal_content_manager",
            None,
            "AbyssalGateActivation",
            &[PyRep::Bool(true)],
            &kw,
            None,
        )
        .await
        .expect("AbyssalGateActivation Room 1 -> 2");
    assert_eq!(r2.dict_get("currentRoom").and_then(|v| v.as_i64()), Some(2));

    let r3 = ctx
        .dispatch_rpc(
            &mut sess,
            "abyssal_content_manager",
            None,
            "AbyssalGateActivation",
            &[PyRep::Bool(true)],
            &kw,
            None,
        )
        .await
        .expect("AbyssalGateActivation Room 2 -> 3");
    assert_eq!(r3.dict_get("currentRoom").and_then(|v| v.as_i64()), Some(3));

    let exit_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "abyssal_content_manager",
            None,
            "AbyssalEndGateActivation",
            &[],
            &kw,
            None,
        )
        .await
        .expect("AbyssalEndGateActivation exit to origin");
    assert_eq!(exit_rep.dict_get("completed").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(
        exit_rep
            .dict_get("returnedSolarSystemID")
            .and_then(|v| v.as_i64()),
        Some(30_002_813)
    );
    assert_eq!(
        exit_rep.dict_get("rewardTypeID").and_then(|v| v.as_i64()),
        Some(48_121)
    );

    // 4. Activate second filament (56143) and expire the 20-minute collapse timer -> ship & pod destroyed
    ctx.dispatch_rpc(
        &mut sess,
        "abyssal_content_manager",
        None,
        "ActivateAbyssalKey",
        &[PyRep::Integer(56143)],
        &kw,
        None,
    )
    .await
    .expect("Activate second filament");
    advance_pve_special_clock(&ctx, 1_205);
    let exp_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "abyssal_content_manager",
            None,
            "AbyssalContentExpiration",
            &[],
            &kw,
            None,
        )
        .await
        .expect("AbyssalContentExpiration");
    assert_eq!(exp_rep.dict_get("shipDestroyed").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(exp_rep.dict_get("podDestroyed").and_then(|v| v.as_bool()), Some(true));
}

#[tokio::test]
async fn test_pve_homefront_storms_drifter_hives_triglavian_and_interference() {
    // parity: feat:pve.homefront-ops, feat:pve.metaliminal-storms, feat:pve.drifter-hives, feat:pve.triglavian-sites, feat:pve.interference
    eve_testkit::attest(&[
        "feat:pve.homefront-ops",
        "feat:pve.metaliminal-storms",
        "feat:pve.drifter-hives",
        "feat:pve.triglavian-sites",
        "feat:pve.interference",
    ]);

    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = create_in_space_session(&ctx, 8107, 30_000_142, 710_001_007, 587).await;

    // 1. Homefront Operations (`dungeonInstanceCacheMgr.GetHomefrontSiteInstances` is read-only; completion via `keeper.EnterNextDungeonRoom`)
    let hf_initial = ctx.dispatch_rpc(&mut sess, "dungeonInstanceCacheMgr", None, "GetHomefrontSiteInstances", &[PyRep::Integer(30_000_142)], &kw, None).await.expect("GetHomefrontSiteInstances read-only");
    let PyRep::List(hf_init_list) = &hf_initial else { panic!("Expected List") };
    assert_eq!(hf_init_list[0].dict_get("completed").and_then(|v| v.as_bool()), Some(false));

    let jita_sys = ctx.systems.get_or_spawn(30_000_142);
    let _ = jita_sys.remove_ball(1_897_710_001).await;
    ctx.dispatch_rpc(&mut sess, "keeper", None, "EnterNextDungeonRoom", &[PyRep::Integer(710_001), PyRep::Integer(5)], &kw, None).await.expect("Complete Homefront site 710_001");

    let hf_rep = ctx.dispatch_rpc(&mut sess, "dungeonInstanceCacheMgr", None, "GetHomefrontSiteInstances", &[PyRep::Integer(30_000_142)], &kw, None).await.expect("GetHomefrontSiteInstances after completion");
    let PyRep::List(hf_list) = &hf_rep else { panic!("Expected List") };
    assert_eq!(hf_list[0].dict_get("completed").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(hf_list[0].dict_get("rewardPaidIsk").and_then(|v| v.as_i64()), Some(17_230_000));

    // 2. Metaliminal Roaming Storms (`map.GetAllRoamingWeatherSystems` read-only; moved by `advance_pve_special_clock`)
    let storms_before = ctx.dispatch_rpc(&mut sess, "map", None, "GetAllRoamingWeatherSystems", &[], &kw, None).await.expect("GetAllRoamingWeatherSystems initial");
    let PyRep::List(sb_list) = &storms_before else { panic!("Expected List") };
    let center_before = sb_list[0].dict_get("centerSolarSystemID").and_then(|v| v.as_i64()).unwrap();

    advance_pve_special_clock(&ctx, 3_600);
    let storms_after = ctx.dispatch_rpc(&mut sess, "map", None, "GetAllRoamingWeatherSystems", &[], &kw, None).await.expect("GetAllRoamingWeatherSystems after clock step");
    let PyRep::List(sa_list) = &storms_after else { panic!("Expected List") };
    let center_after = sa_list[0].dict_get("centerSolarSystemID").and_then(|v| v.as_i64()).unwrap();
    assert_ne!(center_before, center_after, "Roaming Metaliminal storm center must move between connected stargate systems");

    // 3. Wormhole Drifter Hives (`31_000_001` Barbican Hive 5648 -> drops Barbican Element 34557)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.solar_system_id = 31_000_001;
        }
    });
    sess.solar_system_id = Some(31_000_001);
    let wh_sys = ctx.systems.get_or_spawn(31_000_001);
    let _ = wh_sys.remove_ball(1_898_005_648).await;
    ctx.dispatch_rpc(&mut sess, "keeper", None, "EnterNextDungeonRoom", &[PyRep::Integer(31_000_001)], &kw, None).await.expect("Complete Barbican Drifter Hive");
    let hive_rep = ctx.dispatch_rpc(&mut sess, "dungeonInstanceCacheMgr", None, "GetTriglavianSiteInstances", &[PyRep::Integer(31_000_001)], &kw, None).await.expect("GetTriglavianSiteInstances for Drifter Hive");
    assert_eq!(hive_rep.dict_get("dungeonName").and_then(|v| v.as_str()), Some("Barbican Hive"));
    assert_eq!(hive_rep.dict_get("elementTypeID").and_then(|v| v.as_i64()), Some(34_557));
    assert_eq!(hive_rep.dict_get("cleared").and_then(|v| v.as_bool()), Some(true));

    // 4. Triglavian Pochven / EDENCOM Flashpoint Sites (`8922` Observatory Flashpoint -> dual standings delta)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.solar_system_id = 30_000_142;
        }
    });
    sess.solar_system_id = Some(30_000_142);
    let _ = jita_sys.remove_ball(1_897_720_001).await;
    ctx.dispatch_rpc(&mut sess, "keeper", None, "EnterNextDungeonRoom", &[PyRep::Integer(720_001)], &kw, None).await.expect("Complete Observatory Flashpoint");
    let trig_rep = ctx.dispatch_rpc(&mut sess, "dungeonInstanceCacheMgr", None, "GetTriglavianSiteInstances", &[PyRep::Integer(30_000_142)], &kw, None).await.expect("GetTriglavianSiteInstances");
    let PyRep::List(trig_list) = &trig_rep else { panic!("Expected List") };
    assert_eq!(trig_list[0].dict_get("completed").and_then(|v| v.as_bool()), Some(true));
    let (trig_standing, edencom_standing) = ctx.db.with_snapshot(|snap| {
        let ch = &snap.characters[&90_000_001];
        (ch.standings.get(&500_026).copied().unwrap_or(0.0), ch.standings.get(&500_027).copied().unwrap_or(0.0))
    });
    assert!(trig_standing > 0.0 && edencom_standing < 0.0);

    // 5. Solar System Interference (`solarsystemInterferenceMgr.GetLocalInterferenceState` read-only; mutated by server activity)
    let int_low = ctx.dispatch_rpc(&mut sess, "solarsystemInterferenceMgr", None, "GetLocalInterferenceState", &[PyRep::Integer(30_000_142)], &kw, None).await.expect("GetLocalInterferenceState initial Low");
    assert_eq!(int_low.dict_get("interferenceBand").and_then(|v| v.as_i64()), Some(1));

    record_system_interference_activity(&ctx, &mut sess, 30_000_142, 0.75);
    let int_high = ctx.dispatch_rpc(&mut sess, "solarsystemInterferenceMgr", None, "GetLocalInterferenceState", &[PyRep::Integer(30_000_142)], &kw, None).await.expect("GetLocalInterferenceState after CRAB/cyno activity");
    assert_eq!(int_high.dict_get("interferenceBand").and_then(|v| v.as_i64()), Some(3));
    assert_eq!(int_high.dict_get("bandLabel").and_then(|v| v.as_str()), Some("High"));
    let notifs = sess.drain_notifications();
    assert!(
        notifs.iter().any(|n| n.0 == "OnSolarsystemInterferenceChanged"),
        "Crossing interference band threshold must emit OnSolarsystemInterferenceChanged"
    );
}
