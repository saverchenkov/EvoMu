//! Phase 8 (`W-3` & `W-4`) Complete PvE Exploration:
//! Combat Anomalies, Escalations, DED Complexes, Sansha Incursions,
//! Homefront Operations, Metaliminal Storms, Drifter Hives & Triglavian Sites.

use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, DamageVector, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::pve_exploration::with_pve_exploration_reg;
use eve_server::services::pve_special_sites::{
    advance_pve_special_clock, ensure_special_sites_spawned, record_system_interference_activity,
    with_pve_special_reg,
};
use glam::DVec3;
use std::collections::HashMap;

async fn clear_site_wave_npcs(ctx: &ServerContext, solar_system_id: u32, site_id: &str) {
    let ball_ids = with_pve_exploration_reg(ctx, |reg| {
        reg.sites_by_system.get(&solar_system_id).and_then(|l| l.iter().find(|s| s.site_id == site_id)).map(|s| s.active_npc_ball_ids.clone()).unwrap_or_default()
    });
    let sys_handle = ctx.systems.get_or_spawn(solar_system_id);
    for bid in ball_ids {
        let _ = sys_handle.apply_damage(bid, DamageVector { em: 50_000.0, thermal: 50_000.0, kinetic: 50_000.0, explosive: 50_000.0 }).await;
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
    ctx.dispatch_rpc(&mut sess, "authentication", None, "Login", &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())], &kw, None).await.expect("Login");
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_001)], &kw, None).await.expect("SelectCharacterID");

    sess.solar_system_id = Some(solar_system_id);
    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = Some(ship_id);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.station_id = None; ch.structure_id = None; ch.solar_system_id = solar_system_id; ch.active_ship_id = ship_id;
        }
        snap.items.insert(ship_id, InventoryItemRecord {
            item_id: ship_id, type_id: ship_type_id, owner_id: 90_000_001, location_id: solar_system_id as i64,
            flag: InventoryFlag::Hangar, quantity: 1, singleton: true, ..Default::default()
        });
    });

    let sys_handle = ctx.systems.get_or_spawn(solar_system_id);
    let _ = sys_handle.spawn_ball(Ball {
        id: ship_id, type_id: ship_type_id, position: DVec3::ZERO, velocity: DVec3::ZERO,
        max_velocity: 365.0, inertia: 1.0, mass: 1_067_000.0, radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await;
    sess
}

// ============================================================================
// 1. Combat Anomalies (True-Sec Tiers & Faction Waves), Escalations & Expeditions
// ============================================================================
#[tokio::test]
async fn test_pve_combat_anomalies_waves_and_escalations() {
    // parity: feat:pve.combat-anomalies, feat:pve.escalations, feat:pve.expeditions, rpc:dungeonExplorationMgr.GetMyEscalatingPathDetails
    eve_testkit::attest(&["feat:pve.combat-anomalies", "feat:pve.escalations", "feat:pve.expeditions"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let mut sess = create_in_space_session(&ctx, 8101, 30_000_142, 710_001_001, 587).await;

    // 1. Verify regional pirate faction anomalies across 4 empires
    for (sys, expected_kw) in [(30_000_142_u32, "Guristas"), (30_002_187, "Blood"), (30_002_659, "Serpentis"), (30_002_510, "Angel")] {
        let rep = ctx.dispatch_rpc(&mut sess, "scanSvc", None, "GetCombatAnomalyInstances", &[PyRep::Integer(sys as i64)], &kw, None).await.expect("GetCombatAnomalyInstances");
        let PyRep::List(list) = &rep else { panic!("Expected List") };
        assert!(!list.is_empty(), "Must spawn anomalies for system {sys}");
        let names: Vec<&str> = list.iter().filter_map(|x| x.dict_get("dungeonName").and_then(|v| v.as_str())).collect();
        assert!(names.iter().any(|n| n.contains(expected_kw)), "System {sys} must spawn {expected_kw} anomalies: {names:?}");
    }

    // 2. True-sec combat anomaly tiers: High-Sec (Hideaway), Low-Sec (Vigil/Haven), Null-Sec (Sanctum)
    let ls_rep = ctx.dispatch_rpc(&mut sess, "scanSvc", None, "GetCombatAnomalyInstances", &[PyRep::Integer(30_002_813)], &kw, None).await.expect("Low-sec combat anomalies");
    let PyRep::List(ls_list) = &ls_rep else { panic!("Expected List") };
    let ls_names: Vec<&str> = ls_list.iter().filter_map(|x| x.dict_get("dungeonName").and_then(|v| v.as_str())).collect();
    assert!(ls_names.iter().any(|n| n.contains("Vigil") || n.contains("Haven")), "Low-sec must spawn Vigil/Haven: {ls_names:?}");

    let ns_rep = ctx.dispatch_rpc(&mut sess, "scanSvc", None, "GetCombatAnomalyInstances", &[PyRep::Integer(30_002_904)], &kw, None).await.expect("Null-sec combat anomalies");
    let PyRep::List(ns_list) = &ns_rep else { panic!("Expected List") };
    let ns_names: Vec<&str> = ns_list.iter().filter_map(|x| x.dict_get("dungeonName").and_then(|v| v.as_str())).collect();
    assert!(ns_names.iter().any(|n| n.contains("Sanctum")), "Null-sec must spawn Sanctum: {ns_names:?}");

    // 3. Enter High-Sec Guristas Hideaway: Wave 1 -> Wave 2 -> Complete + Escalate
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

    // 4. Query dungeonExplorationMgr.GetMyEscalatingPathDetails & GetEscalatingPathDungeonsEntered
    let esc_rep = ctx.dispatch_rpc(&mut sess, "dungeonExplorationMgr", None, "GetMyEscalatingPathDetails", &[], &kw, None).await.expect("GetMyEscalatingPathDetails");
    let PyRep::List(esc_list) = &esc_rep else { panic!("Expected List from GetMyEscalatingPathDetails") };
    assert!(!esc_list.is_empty(), "Active escalation must be returned in journal");
    let first_esc = &esc_list[0];
    assert!(first_esc.dict_get("destinationSolarSystemID").is_some());
    assert_eq!(first_esc.dict_get("stepIndex").and_then(|v| v.as_i64()), Some(1));

    let entered_rep = ctx.dispatch_rpc(&mut sess, "dungeonExplorationMgr", None, "GetEscalatingPathDungeonsEntered", &[], &kw, None).await.expect("GetEscalatingPathDungeonsEntered");
    let PyRep::List(entered_list) = &entered_rep else { panic!("Expected List") };
    assert!(!entered_list.is_empty(), "Source dungeon must be recorded in entered dungeons");
}

// ============================================================================
// 2. Combat Signatures, DED Complexes, Ship Restrictions & Faction Loot
// ============================================================================
#[tokio::test]
async fn test_pve_combat_signatures_and_ded_complexes_restrictions_and_loot() {
    // parity: feat:pve.combat-signatures, feat:pve.ded-complexes
    eve_testkit::attest(&["feat:pve.combat-signatures", "feat:pve.ded-complexes"]);

    let ctx = ServerContext::new();
    ensure_special_sites_spawned(&ctx).await;
    let kw = HashMap::new();
    let mut sess = create_in_space_session(&ctx, 8102, 30_000_142, 710_001_002, 587).await;

    // 1. Verify DED 3/10 complex names across regional pirate factions
    for (sys, exp_name) in [(30_000_142_u32, "Guristas Guerilla Grounds"), (30_002_187, "Blood Raider Human Farm"), (30_002_659, "Serpentis Narcotic Warehouses"), (30_002_510, "Angel Creo-Corp Mining")] {
        ctx.db.with_mut_snapshot(|snap| { if let Some(ch) = snap.characters.get_mut(&90_000_001) { ch.solar_system_id = sys; } });
        sess.solar_system_id = Some(sys);
        let sigs_rep = ctx.dispatch_rpc(&mut sess, "scanSvc", None, "GetSignatureInstances", &[PyRep::Integer(sys as i64)], &kw, None).await.expect("GetSignatureInstances");
        let PyRep::List(sigs) = &sigs_rep else { panic!("Expected List") };
        let ded_sig_id = format!("DED-{sys}-3");
        assert!(sigs.iter().any(|s| s.dict_get("signatureID").and_then(|v| v.as_str()) == Some(&ded_sig_id)));

        let full_rep = ctx.dispatch_rpc(&mut sess, "scanSvc", None, "ScanSignature", &[PyRep::String(ded_sig_id), PyRep::Real(1.0)], &kw, None).await.expect("Scan DED");
        assert_eq!(full_rep.dict_get("siteName").and_then(|v| v.as_str()), Some(exp_name));
    }
    ctx.db.with_mut_snapshot(|snap| { if let Some(ch) = snap.characters.get_mut(&90_000_001) { ch.solar_system_id = 30_000_142; } });
    sess.solar_system_id = Some(30_000_142);
    let remote_scan_err = ctx.dispatch_rpc(&mut sess, "scanSvc", None, "ScanSignature", &[PyRep::String("DED-30002187-3".into()), PyRep::Real(1.0)], &kw, None).await.expect_err("Remote signature scan must fail");
    assert_eq!(remote_scan_err.code, "SignatureNotFound");
    assert_eq!(eve_server::services::pve_exploration::ded_complex_for_rating(&ctx.sde, 30_002_510, 8), (5203, "Angel Naval Shipyard"));

    // 2. Verify DED 1/10..10/10 ship size gate restrictions (`keeper.CanEnterDungeonInCurrentShip` & `EnterNextDungeonRoom`)
    let can_frigate = ctx.dispatch_rpc(&mut sess, "keeper", None, "CanEnterDungeonInCurrentShip", &[PyRep::Integer(1)], &kw, None).await.expect("CanEnterDungeonInCurrentShip Rifter in 1/10");
    assert_eq!(can_frigate.dict_get("canEnter").and_then(|v| v.as_bool()), Some(true));

    // Switch pilot into a Raven Battleship (type_id 638) -> rejected by DED 3/10 gate!
    let raven_ship_id = 710_001_099_i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) { ch.active_ship_id = raven_ship_id; }
        snap.items.insert(raven_ship_id, InventoryItemRecord {
            item_id: raven_ship_id, type_id: 638, owner_id: 90_000_001,
            location_id: 30_000_142, flag: InventoryFlag::Hangar, quantity: 1, singleton: true, ..Default::default()
        });
    });
    sess.ship_id = Some(raven_ship_id);
    let gate_err = ctx.dispatch_rpc(&mut sess, "keeper", None, "EnterNextDungeonRoom", &[PyRep::String("DED-30000142-3".into())], &kw, None).await.expect_err("Battleship must be rejected by DED 3/10 gate");
    assert_eq!(gate_err.code, "ShipSizeRestricted");

    // Switch back to Rifter (587) and clear all 3 rooms of DED 3/10 to drop 6th Tier Overseer's Personal Effects (19405)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) { ch.active_ship_id = 710_001_002; }
    });
    sess.ship_id = Some(710_001_002);
    for wave in 1..=3 {
        clear_site_wave_npcs(&ctx, 30_000_142, "DED-30000142-3").await;
        let step_rep = ctx.dispatch_rpc(&mut sess, "keeper", None, "EnterNextDungeonRoom", &[PyRep::String("DED-30000142-3".into())], &kw, None).await.expect("EnterNextDungeonRoom in Rifter");
        if wave == 3 {
            assert_eq!(step_rep.dict_get("completed").and_then(|v| v.as_bool()), Some(true));
            let PyRep::List(loot) = step_rep.dict_get("droppedLoot").expect("droppedLoot") else { panic!("Expected List of droppedLoot") };
            assert!(loot.iter().any(|item| item.dict_get("typeID").and_then(|v| v.as_i64()) == Some(19_405)));
        }
    }
}


// ============================================================================
// 4. Homefront Ops, Storms, Drifter Hives, Triglavian Sites & Interference
// ============================================================================
#[tokio::test]
async fn test_pve_homefront_storms_drifter_hives_triglavian_and_interference() {
    // parity: feat:pve.homefront-ops, feat:pve.metaliminal-storms, feat:pve.interference, rpc:dungeonInstanceCacheMgr.GetHomefrontSiteInstances, rpc:dungeonInstanceCacheMgr.GetTriglavianSiteInstances, rpc:map.GetAllRoamingWeatherSystems, rpc:solarsystemInterferenceMgr.GetLocalInterferenceState
    eve_testkit::attest(&["feat:pve.homefront-ops", "feat:pve.metaliminal-storms", "feat:pve.interference"]);

    let ctx = ServerContext::new();
    ensure_special_sites_spawned(&ctx).await;
    let kw = HashMap::new();
    let mut sess = create_in_space_session(&ctx, 8107, 30_000_142, 710_001_007, 587).await;

    // 1. Homefront Operations
    let hf_init = ctx.dispatch_rpc(&mut sess, "dungeonInstanceCacheMgr", None, "GetHomefrontSiteInstances", &[PyRep::Integer(30_000_142)], &kw, None).await.expect("GetHomefrontSiteInstances initial");
    let PyRep::Dict(hf_dict) = &hf_init else { panic!("Expected Dict") };
    let dungeons = hf_dict.iter().find(|(k, _)| k.as_i64() == Some(30_000_142)).and_then(|(_, v)| match v { PyRep::Dict(d) => Some(d), _ => None }).expect("Jita sites");
    let hf_inst_id = match &dungeons[0].1 { PyRep::List(ids) => ids[0].as_i64().unwrap(), _ => panic!() };
    let hf_boss_bid = with_pve_special_reg(&ctx, |reg| reg.homefront_sites[&30_000_142].iter().find(|s| s.instance_id == hf_inst_id).unwrap().boss_npc_ball_id);
    let jita_sys = ctx.systems.get_or_spawn(30_000_142);
    let _ = jita_sys.apply_damage(hf_boss_bid, DamageVector { em: 50_000.0, thermal: 50_000.0, kinetic: 50_000.0, explosive: 50_000.0 }).await;
    let complete_rep = ctx.dispatch_rpc(&mut sess, "keeper", None, "EnterNextDungeonRoom", &[PyRep::Integer(hf_inst_id), PyRep::Integer(5)], &kw, None).await.expect("Complete Homefront site");
    assert_eq!(complete_rep.dict_get("completed").and_then(|v| v.as_bool()), Some(true));
    assert!(complete_rep.dict_get("rewardPaidIsk").and_then(|v| v.as_i64()).unwrap_or(0) > 0);

    let hf_rep = ctx.dispatch_rpc(&mut sess, "dungeonInstanceCacheMgr", None, "GetHomefrontSiteInstances", &[PyRep::Integer(30_000_142)], &kw, None).await.expect("GetHomefrontSiteInstances after completion");
    let PyRep::Dict(hf_after_dict) = &hf_rep else { panic!("Expected Dict") };
    let dungeons_after = hf_after_dict.iter().find(|(k, _)| k.as_i64() == Some(30_000_142)).and_then(|(_, v)| match v { PyRep::Dict(d) => Some(d), _ => None }).expect("Jita sites after");
    let after_instances: Vec<i64> = dungeons_after.iter().flat_map(|(_, l)| match l { PyRep::List(ids) => ids.iter().filter_map(|i| i.as_i64()).collect::<Vec<_>>(), _ => Vec::new() }).collect();
    assert!(!after_instances.contains(&hf_inst_id), "Completed Homefront instance must not be returned");

    // 2. Metaliminal Roaming Storms
    let storms_before = ctx.dispatch_rpc(&mut sess, "map", None, "GetAllRoamingWeatherSystems", &[], &kw, None).await.expect("GetAllRoamingWeatherSystems initial");
    let PyRep::List(sb_list) = &storms_before else { panic!("Expected List") };
    assert!(!sb_list.is_empty(), "Must return roaming weather systems");
    let loc_before = sb_list[0].dict_get("locationID").and_then(|v| v.as_i64()).expect("locationID");
    let scene_type = sb_list[0].dict_get("sceneType").and_then(|v| v.as_i64()).expect("sceneType");
    assert!(scene_type == 1 || scene_type == 4, "sceneType must match client expectations (1 or 4)");
    advance_pve_special_clock(&ctx, 3_600);
    let storms_after = ctx.dispatch_rpc(&mut sess, "map", None, "GetAllRoamingWeatherSystems", &[], &kw, None).await.expect("GetAllRoamingWeatherSystems after clock step");
    let PyRep::List(sa_list) = &storms_after else { panic!("Expected List") };
    let loc_after = sa_list[0].dict_get("locationID").and_then(|v| v.as_i64()).expect("locationID after");
    assert_ne!(loc_before, loc_after, "Roaming Metaliminal storm systems must move");

    // 3. Wormhole Drifter Hives (Barbican)
    let hive_reg = eve_server::services::pve_special_sites::get_drifter_hive_for_system(&ctx, 31_000_001).expect("Barbican Hive");
    assert_eq!(hive_reg.dungeon_name, "Barbican Hive");
    assert_eq!(hive_reg.element_type_id, 34_557);
    let trig_wh = ctx.dispatch_rpc(&mut sess, "dungeonInstanceCacheMgr", None, "GetTriglavianSiteInstances", &[PyRep::Integer(31_000_001)], &kw, None).await.expect("GetTriglavianSiteInstances for Drifter Hive system");
    let PyRep::List(trig_wh_list) = &trig_wh else { panic!("Expected List") };
    assert!(trig_wh_list.is_empty(), "Drifter Hive wormhole systems must not return fake Triglavian sites");

    // 4. Triglavian Minor Victory Systems query & dynamic site completion
    // const-ok: No active invasion campaigns configured
    let vic_systems = ctx.dispatch_rpc(&mut sess, "map", None, "GetTriglavianMinorVictorySystems", &[], &kw, None).await.expect("GetTriglavianMinorVictorySystems");
    let PyRep::List(v_list) = &vic_systems else { panic!("Expected List") };
    assert!(v_list.is_empty(), "Triglavian minor victory systems must be empty when no dynamic invasion campaign is active");

    // Pure read query in Jita returns empty without fabricating sites
    let jita_trig_rep = ctx.dispatch_rpc(&mut sess, "dungeonInstanceCacheMgr", None, "GetTriglavianSiteInstances", &[PyRep::Integer(30_000_142)], &kw, None).await.expect("GetTriglavianSiteInstances for Jita");
    let PyRep::List(jita_trig_list) = &jita_trig_rep else { panic!("Expected List") };
    assert!(jita_trig_list.is_empty(), "Jita must not fabricate Triglavian sites on read query");

    // Seeded Triglavian site in Urlen (30000200)
    let urlen_sys = ctx.systems.get_or_spawn(30_000_200);
    with_pve_special_reg(&ctx, |reg| {
        eve_server::services::pve_special_sites::ensure_triglavian_sites_for_system(reg, 30_000_200);
    });
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) { ch.solar_system_id = 30_000_200; }
    });
    sess.solar_system_id = Some(30_000_200);
    let trig_rep = ctx.dispatch_rpc(&mut sess, "dungeonInstanceCacheMgr", None, "GetTriglavianSiteInstances", &[PyRep::Integer(30_000_200)], &kw, None).await.expect("GetTriglavianSiteInstances in Urlen");
    let PyRep::List(trig_list) = &trig_rep else { panic!("Expected List") };
    let trig_inst_id = trig_list[0].dict_get("instanceID").unwrap().as_i64().unwrap();
    let trig_boss_bid = trig_list[0].dict_get("bossNpcBallID").unwrap().as_i64().unwrap();

    let _ = urlen_sys.apply_damage(trig_boss_bid, DamageVector { em: 50_000.0, thermal: 50_000.0, kinetic: 50_000.0, explosive: 50_000.0 }).await;
    ctx.dispatch_rpc(&mut sess, "keeper", None, "EnterNextDungeonRoom", &[PyRep::Integer(trig_inst_id)], &kw, None).await.expect("Complete Observatory Flashpoint");
    let trig_after = ctx.dispatch_rpc(&mut sess, "dungeonInstanceCacheMgr", None, "GetTriglavianSiteInstances", &[PyRep::Integer(30_000_200)], &kw, None).await.expect("GetTriglavianSiteInstances after completion");
    let PyRep::List(trig_after_list) = &trig_after else { panic!("Expected List") };
    assert_eq!(trig_after_list[0].dict_get("completed").and_then(|v| v.as_bool()), Some(true));

    let (trig_standing, edencom_standing) = ctx.db.with_snapshot(|snap| {
        let ch = &snap.characters[&90_000_001];
        (ch.standings.get(&500_026).copied().unwrap_or(0.0), ch.standings.get(&500_027).copied().unwrap_or(0.0))
    });
    assert!(trig_standing > 0.0 && edencom_standing < 0.0);

    // 5. Solar System Interference
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) { ch.solar_system_id = 30_000_142; }
    });
    sess.solar_system_id = Some(30_000_142);
    let int_low = ctx.dispatch_rpc(&mut sess, "solarsystemInterferenceMgr", None, "GetLocalInterferenceState", &[PyRep::Integer(30_000_142)], &kw, None).await.expect("GetLocalInterferenceState initial Low");
    assert_eq!(int_low.dict_get("interferenceBand").and_then(|v| v.as_i64()), Some(1));

    record_system_interference_activity(&ctx, &mut sess, 30_000_142, 0.75);
    let int_high = ctx.dispatch_rpc(&mut sess, "solarsystemInterferenceMgr", None, "GetLocalInterferenceState", &[PyRep::Integer(30_000_142)], &kw, None).await.expect("GetLocalInterferenceState after activity");
    assert_eq!(int_high.dict_get("interferenceBand").and_then(|v| v.as_i64()), Some(3));
    assert_eq!(int_high.dict_get("bandLabel").and_then(|v| v.as_str()), Some("High"));
    let notifs = sess.drain_notifications();
    assert!(notifs.iter().any(|n| n.0 == "OnSolarsystemInterferenceChanged"));
}
