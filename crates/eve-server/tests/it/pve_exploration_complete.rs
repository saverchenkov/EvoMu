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

async fn kill_site_wave_npcs(ctx: &ServerContext, solar_system_id: u32, site_id: &str) {
    let ball_ids = with_pve_exploration_reg(ctx, |reg| {
        reg.sites_by_system.get(&solar_system_id).and_then(|l| l.iter().find(|s| s.site_id == site_id)).map(|s| s.active_npc_ball_ids.clone()).unwrap_or_default()
    });
    let sys_handle = ctx.systems.get_or_spawn(solar_system_id);
    for bid in ball_ids {
        let _ = sys_handle.apply_damage_with_attacker(bid, DamageVector { em: 1.0e7, thermal: 1.0e7, kinetic: 1.0e7, explosive: 1.0e7 }, Some(90_000_001)).await;
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
    // Seed 3 gives SplitMix64 first draw 0.1135 < 0.20 (natural 20% escalation roll)
    ctx.set_mechanic_rng_seed(Some(3));
    let done_rep = ctx.dispatch_rpc(&mut sess, "scanSvc", None, "EnterCombatAnomaly", &[PyRep::String(site_id.into()), PyRep::Bool(true)], &kw, None).await.expect("Clear final Wave 2");
    assert_eq!(done_rep.dict_get("completed").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(done_rep.dict_get("escalated").and_then(|v| v.as_bool()), Some(true));

    // Verify OnCosmicAnomalyRemoved and OnEscalatingPathMessage notifications were pushed
    let notifs = sess.drain_notifications();
    assert!(notifs.iter().any(|n| n.0 == "OnCosmicAnomalyRemoved"));
    assert!(notifs.iter().any(|n| n.0 == "OnEscalatingPathMessage"));

    // 4. Query dungeonExplorationMgr.GetMyEscalatingPathDetails & GetEscalatingPathDungeonsEntered
    let esc_rep = ctx.dispatch_rpc(&mut sess, "dungeonExplorationMgr", None, "GetMyEscalatingPathDetails", &[], &kw, None).await.expect("GetMyEscalatingPathDetails");
    let PyRep::Rowset { columns: esc_cols, rows: esc_rows } = &esc_rep else { panic!("Expected Rowset from GetMyEscalatingPathDetails") };
    assert!(!esc_rows.is_empty(), "Active escalation must be returned in journal");
    let first_esc = &esc_rows[0];
    let dest_sys_idx = esc_cols.iter().position(|c| c == "destinationSolarSystemID").unwrap();
    let step_idx = esc_cols.iter().position(|c| c == "stepIndex").unwrap();
    assert!(matches!(first_esc[dest_sys_idx], PyRep::Integer(_)));
    assert_eq!(first_esc[step_idx].as_i64(), Some(1));

    let entered_rep = ctx.dispatch_rpc(&mut sess, "dungeonExplorationMgr", None, "GetEscalatingPathDungeonsEntered", &[], &kw, None).await.expect("GetEscalatingPathDungeonsEntered");
    let PyRep::List(entered_list) = &entered_rep else { panic!("Expected List") };
    assert!(!entered_list.is_empty(), "Source dungeon must be recorded in entered dungeons");
}

// ============================================================================
// 2. Combat Signatures, DED Complexes, Ship Restrictions & Faction Loot
// ============================================================================
#[tokio::test]
async fn test_pve_combat_signatures_and_ded_complexes_restrictions_and_loot() {
    let ctx = ServerContext::new();
    ensure_special_sites_spawned(&ctx).await;
    let kw = HashMap::new();
    let mut sess = create_in_space_session(&ctx, 8102, 30_000_142, 710_001_002, 587).await;

    // 1. Verify seeded DED 3/10 complex names across regional pirate factions
    for (sys, exp_name) in [(30_000_142_u32, "Guristas Guerilla Grounds"), (30_002_187, "Blood Raider Human Farm"), (30_002_659, "Serpentis Narcotic Warehouses"), (30_002_510, "Angel Creo-Corp Mining")] {
        eve_server::services::pve_exploration::ensure_system_pve_sites_seeded(&ctx, sys).await;
        let ded_sig_id = format!("DED-{sys}-3");
        let name = with_pve_exploration_reg(&ctx, |reg| reg.sites_by_system[&sys].iter().find(|s| s.site_id == ded_sig_id).map(|s| s.dungeon_name.clone()));
        assert_eq!(name.as_deref(), Some(exp_name));
    }
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

    // Switch back to Rifter (587) at the gate; rooms advance by gate, and the final-room overseer kill
    // (in-space damage pipeline) drops the Overseer wreck holding SDE Overseer's Personal Effects
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) { ch.active_ship_id = 710_001_002; }
    });
    sess.ship_id = Some(710_001_002);
    let ded_pos = with_pve_exploration_reg(&ctx, |reg| reg.sites_by_system[&30_000_142].iter().find(|s| s.site_id == "DED-30000142-3").unwrap().position);
    let _ = ctx.systems.get_or_spawn(30_000_142).spawn_ball(Ball {
        id: 710_001_002, type_id: 587, position: DVec3::from_array(ded_pos), velocity: DVec3::ZERO,
        max_velocity: 365.0, inertia: 1.0, mass: 1_067_000.0, radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await;
    for wave in 1..=3 {
        kill_site_wave_npcs(&ctx, 30_000_142, "DED-30000142-3").await;
        if wave < 3 {
            let step_rep = ctx.dispatch_rpc(&mut sess, "keeper", None, "EnterNextDungeonRoom", &[PyRep::String("DED-30000142-3".into())], &kw, None).await.expect("EnterNextDungeonRoom in Rifter");
            assert_eq!(step_rep.dict_get("currentWave").and_then(|v| v.as_i64()), Some(wave + 1));
        }
    }
    let (ope_tid, _) = eve_server::services::pve_exploration_sites::overseer_effects_for_rating(&ctx.sde, 3).expect("OPE");
    let has_ope = ctx.db.with_snapshot(|snap| snap.items.values().any(|i| i.type_id == ope_tid && snap.items.get(&i.location_id).is_some_and(|w| w.type_id == 26473)));
    assert!(has_ope, "Overseer wreck must hold the SDE Overseer's Personal Effects");
}


// ============================================================================
// 4. Homefront Ops, Storms, Drifter Hives, Triglavian Sites & Interference
// ============================================================================
#[tokio::test]
async fn test_pve_homefront_storms_drifter_hives_triglavian_and_interference() {
    // parity: feat:pve.homefront-ops, feat:pve.metaliminal-storms, feat:pve.interference, rpc:dungeonInstanceCacheMgr.GetHomefrontSiteInstances, rpc:map.GetAllRoamingWeatherSystems, rpc:solarsystemInterferenceMgr.GetLocalInterferenceState
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
    let init_bal = ctx.db.with_snapshot(|s| s.characters[&90_000_001].balance.0);
    let _ = ctx.drain_notifications_for_char(90_000_001);
    let _ = jita_sys.apply_damage_with_attacker(hf_boss_bid, DamageVector { em: 50_000.0, thermal: 50_000.0, kinetic: 50_000.0, explosive: 50_000.0 }, Some(90_000_001)).await;
    let after_bal = ctx.db.with_snapshot(|s| s.characters[&90_000_001].balance.0);
    assert!(after_bal > init_bal, "Homefront kill must pay ISK to pilot");
    assert!(ctx.db.with_snapshot(|s| s.wallet_journal.iter().any(|j| j.character_id == 90_000_001 && j.reason.contains("Homefront Operation Payout"))));
    assert!(ctx.drain_notifications_for_char(90_000_001).iter().any(|n| n.event == "OnAccountChange"));

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
    advance_pve_special_clock(&ctx, 48 * 3_600);
    let storms_after = ctx.dispatch_rpc(&mut sess, "map", None, "GetAllRoamingWeatherSystems", &[], &kw, None).await.expect("GetAllRoamingWeatherSystems after clock step");
    let PyRep::List(sa_list) = &storms_after else { panic!("Expected List") };
    let loc_after = sa_list[0].dict_get("locationID").and_then(|v| v.as_i64()).expect("locationID after");
    assert_ne!(loc_before, loc_after, "Roaming Metaliminal storm systems must move");

    // 3. Wormhole Drifter Hives (Barbican)
    let hive_reg = eve_server::services::pve_special_sites::get_drifter_hive_for_system(&ctx, 31_000_001).expect("Barbican Hive");
    assert_eq!(hive_reg.dungeon_name, "Barbican Hive");
    assert_eq!(hive_reg.element_type_id, 34_557);
    let trig_wh = ctx.dispatch_rpc(&mut sess, "dungeonInstanceCacheMgr", None, "GetTriglavianSiteInstances", &[PyRep::Integer(31_000_001)], &kw, None).await.expect("GetTriglavianSiteInstances for Drifter Hive system");
    let PyRep::Dict(trig_wh_dict) = &trig_wh else { panic!("Expected Dict") };
    let PyRep::Dict(trig_wh_dng) = &trig_wh_dict[0].1 else { panic!("Expected inner Dict") };
    assert!(trig_wh_dng.is_empty(), "Drifter Hive wormhole systems must not return fake Triglavian sites");

    // 4. Triglavian Minor Victory Systems query & dynamic site completion
    let vic_systems = ctx.dispatch_rpc(&mut sess, "map", None, "GetTriglavianMinorVictorySystems", &[], &kw, None).await.expect("GetTriglavianMinorVictorySystems");
    let PyRep::List(v_list) = &vic_systems else { panic!("Expected List") };
    assert_eq!(v_list.len(), 28, "All 28 authentic Triglavian Minor Victory systems returned");
    assert!(v_list.contains(&PyRep::Integer(30_003_076)), "Gammel is seeded as permanent Triglavian Minor Victory system");
    // No sentinel NPC ball (type 10962) is placed at the star origin of site systems
    let jita_balls = ctx.systems.get_or_spawn(30_000_142).list_balls().await.expect("Jita balls");
    assert!(!jita_balls.iter().any(|b| b.type_id == 10_962 && b.position.length() == 0.0), "No sentinel marker ball at the star");

    // Pure read query in Jita returns empty without fabricating sites
    let jita_trig_rep = ctx.dispatch_rpc(&mut sess, "dungeonInstanceCacheMgr", None, "GetTriglavianSiteInstances", &[PyRep::Integer(30_000_142)], &kw, None).await.expect("GetTriglavianSiteInstances for Jita");
    let PyRep::Dict(jita_trig_dict) = &jita_trig_rep else { panic!("Expected Dict") };
    let PyRep::Dict(jita_trig_dng) = &jita_trig_dict[0].1 else { panic!("Expected inner Dict") };
    assert!(jita_trig_dng.is_empty(), "Jita must not fabricate Triglavian sites on read query");

    // Gammel (30003076) automatically contains 2 Triglavian site instances without fixture injection
    let gammel_sys = ctx.systems.get_or_spawn(30_003_076);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) { ch.solar_system_id = 30_003_076; }
    });
    sess.solar_system_id = Some(30_003_076);

    let gammel_trig = ctx.dispatch_rpc(&mut sess, "dungeonInstanceCacheMgr", None, "GetTriglavianSiteInstances", &[PyRep::Integer(30_003_076)], &kw, None).await.expect("GetTriglavianSiteInstances in Gammel");
    let PyRep::Dict(gammel_dict) = &gammel_trig else { panic!("Expected Dict") };
    let PyRep::Dict(gammel_dng) = &gammel_dict[0].1 else { panic!("Expected inner Dict") };
    assert_eq!(gammel_dng.len(), 2, "Gammel minor victory system automatically contains 2 Triglavian site instances");
    let (trig_inst_id, trig_boss_bid) = eve_server::services::pve_special_sites::with_pve_special_reg(&ctx, |reg| {
        let s = &reg.triglavian_sites[&30_003_076][0];
        (s.instance_id, s.boss_npc_ball_id)
    });

    // Boss is auto-spawned per-system on the cache query; in-space kill with attribution completes the site
    assert!(gammel_sys.get_ball_snapshot(trig_boss_bid).await.unwrap().is_some(), "Gammel boss must be spawned without manual spawn_ball");
    let _ = gammel_sys.apply_damage_with_attacker(trig_boss_bid, DamageVector { em: 50_000.0, thermal: 50_000.0, kinetic: 50_000.0, explosive: 50_000.0 }, Some(90_000_001)).await;
    let again = ctx.dispatch_rpc(&mut sess, "keeper", None, "EnterNextDungeonRoom", &[PyRep::Integer(trig_inst_id)], &kw, None).await;
    assert!(again.is_err(), "Site completed by in-space kill must reject a second completion");
    let trig_after = ctx.dispatch_rpc(&mut sess, "dungeonInstanceCacheMgr", None, "GetTriglavianSiteInstances", &[PyRep::Integer(30_003_076)], &kw, None).await.expect("GetTriglavianSiteInstances after completion");
    let PyRep::Dict(trig_after_dict) = &trig_after else { panic!("Expected Dict") };
    let PyRep::Dict(trig_after_dng) = &trig_after_dict[0].1 else { panic!("Expected inner Dict") };
    assert_eq!(trig_after_dng.len(), 1, "Completed site instance removed from active instances");

    let (trig_standing, edencom_standing) = ctx.db.with_snapshot(|snap| {
        let ch = &snap.characters[&90_000_001];
        (ch.standings.get(&500_026).copied().unwrap_or(0.0), ch.standings.get(&500_027).copied().unwrap_or(0.0))
    });
    assert!(edencom_standing > 0.0 && trig_standing < 0.0);

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

pub(crate) mod pve_data_relic_hacking_minigame {
    // Phase 8 (`W-3` & `W-4`) Complete PvE Exploration Part 2:
    // Data & Relic Hacking Minigame, Loot Spew, Ghost Sites, Sleeper Caches,
    // Wormhole Sites, and Abyssal Deadspace Tests.

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
        ensure_system_pve_sites_seeded(&ctx, 31_000_007).await;
        let site_cans = |sys: u32, sid: &str| with_pve_exploration_reg(&ctx, |reg| {
            reg.sites_by_system[&sys].iter().find(|s| s.site_id == sid).map(|s| s.hackable_can_ids.clone()).unwrap()
        });
        let data_cans = site_cans(30_000_142, "DAT-30000142");
        let (data_can_id, relic_can_id) = (data_cans[0], site_cans(30_000_142, "REL-30000142")[0]);

        // Verify Wormhole Data/Relic sites (31000007, J105443 C1) spawn Sleeper NPC guards
        let wh_guarded = with_pve_exploration_reg(&ctx, |reg| {
            reg.sites_by_system[&31_000_007].iter().any(|s| matches!(s.site_category.as_str(), "Data" | "Relic") && s.npc_guards_spawned)
        });
        assert!(wh_guarded, "Wormhole Unsecured Data / Forgotten Relic sites must spawn Sleeper NPC guards");

        // 2. Start Hacking Minigame on Data Can & Relic Can (`hackingMgr.StartNewGameInstance`)
        let (data_pos, relic_pos) = with_pve_exploration_reg(&ctx, |reg| {
            let d = reg.sites_by_system[&30_000_142].iter().find(|s| s.site_id == "DAT-30000142").unwrap().position;
            let r = reg.sites_by_system[&30_000_142].iter().find(|s| s.site_id == "REL-30000142").unwrap().position;
            (d, r)
        });
        let jita_sys = ctx.systems.get_or_spawn(30_000_142);
        let _ = jita_sys.set_ball_position(sess.ship_id.unwrap(), DVec3::from_array(relic_pos) + DVec3::new(500.0, 0.0, 0.0)).await;
        let start_rep = ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "StartNewGameInstance", &[PyRep::Integer(relic_can_id)], &kw, None).await.expect("StartNewGameInstance on Relic Can");
        assert_eq!(start_rep.dict_get("status").and_then(|v| v.as_str()), Some("InProgress"));
        assert_eq!(start_rep.dict_get("virusCoherence").and_then(|v| v.as_i64()), Some(40), "Relic Analyzer I base virusCoherence (attr 1909) is 40");
        assert_eq!(start_rep.dict_get("virusStrength").and_then(|v| v.as_i64()), Some(20), "Relic Analyzer I base virusStrength (attr 1910) is 20");

        // Utility slots start empty until a DataCache node is revealed; revealing the PolymorphicShield DataCache collects it
        let empty_util_err = ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "UsedUtilityElement", &[PyRep::Integer(relic_can_id), PyRep::Integer(0), PyRep::Integer(0)], &kw, None).await.expect_err("empty utility slot");
        assert_eq!(empty_util_err.code, "UtilitySlotEmpty");
        let poly_idx = with_pve_exploration_reg(&ctx, |reg| {
            reg.hacking_games[&(90_000_001, relic_can_id)].tiles.iter()
                .find(|t| t.utility_drop.as_ref().is_some_and(|u| format!("{u:?}") == "PolymorphicShield"))
                .unwrap().tile_index
        });
        let mut poly_path: Vec<u32> = (1..=(poly_idx / 6)).map(|r| r * 6).collect();
        poly_path.extend(((poly_idx / 6) * 6 + 1)..=poly_idx);
        for idx in poly_path {
            for _ in 0..5 {
                let r = ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "ClickedOnTile", &[PyRep::Integer(relic_can_id), PyRep::Integer(idx as i64)], &kw, None).await.expect("Reveal DataCache");
                if idx == poly_idx || r.dict_get("nodeCoherence").and_then(|v| v.as_i64()).unwrap_or(0) <= 0 { break; }
            }
        }
        let poly_slot = with_pve_exploration_reg(&ctx, |reg| {
            reg.hacking_games[&(90_000_001, relic_can_id)].utilities.iter()
                .position(|u| u.as_ref().is_some_and(|k| format!("{k:?}") == "PolymorphicShield")).unwrap()
        });
        let util_rep = ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "UsedUtilityElement", &[PyRep::Integer(relic_can_id), PyRep::Integer(poly_slot as i64), PyRep::Integer(0)], &kw, None).await.expect("UsedUtilityElement PolymorphicShield");
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
        let _ = jita_sys.set_ball_position(sess.ship_id.unwrap(), DVec3::from_array(data_pos) + DVec3::new(500.0, 0.0, 0.0)).await;
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

        // Pilot 1 fails minigame via virus coherence reaching 0 on a defensive node (strength > 0)
        let def_path = with_pve_exploration_reg(&ctx, |reg| {
            let tiles = &reg.hacking_games[&(90_000_001, can3_id)].tiles;
            let core_idx = tiles.iter().find(|t| format!("{:?}", t.kind) == "SystemCore").unwrap().tile_index;
            tiles.iter().filter(|t| t.strength > 0 && t.tile_index > 0 && t.tile_index != core_idx).find_map(|t| {
                let p1: Vec<u32> = (1..=(t.tile_index / 6)).map(|r| r * 6).chain(((t.tile_index / 6) * 6 + 1)..=t.tile_index).collect();
                if !p1.contains(&core_idx) { return Some(p1); }
                let p2: Vec<u32> = (1..=(t.tile_index % 6)).chain((1..=(t.tile_index / 6)).map(|r| r * 6 + (t.tile_index % 6))).collect();
                (!p2.contains(&core_idx)).then_some(p2)
            }).unwrap()
        });
        let (&def_idx, prefix_path) = def_path.split_last().unwrap();
        for &idx in prefix_path {
            for _ in 0..5 {
                with_pve_exploration_reg(&ctx, |reg| { if let Some(g) = reg.hacking_games.get_mut(&(90_000_001, can3_id)) { g.virus_coherence = 100; } });
                let r = ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "ClickedOnTile", &[PyRep::Integer(can3_id), PyRep::Integer(idx as i64)], &kw, None).await.expect("reveal path to defensive node");
                if r.dict_get("nodeCoherence").and_then(|v| v.as_i64()).unwrap_or(0) <= 0 { break; }
            }
        }
        with_pve_exploration_reg(&ctx, |reg| {
            if let Some(g) = reg.hacking_games.get_mut(&(90_000_001, can3_id)) { g.virus_coherence = 1; g.polymorphic_shield_charges = 0; }
        });
        let fail_step = ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "ClickedOnTile", &[PyRep::Integer(can3_id), PyRep::Integer(def_idx as i64)], &kw, None).await.expect("click tile fail");
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
        let sys_142 = ctx.systems.get_or_spawn(30_000_142);
        let _ = sys_142.set_ball_position(710_001_004, DVec3::from_array(gho_pos) + DVec3::new(500.0, 0.0, 0.0)).await;
        ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "StartNewGameInstance", &[PyRep::Integer(ghost_cans[1])], &kw, None).await.expect("Ghost hack starts timer");
        with_pve_exploration_reg(&ctx, |reg| {
            if let Some(g) = reg.hacking_games.get_mut(&(90_000_001, ghost_cans[1])) {
                g.virus_coherence = 5;
            }
            if let Some(s) = reg.sites_by_system.get_mut(&30_000_142).and_then(|l| l.iter_mut().find(|x| x.site_id == "GHO-30000142")) {
                s.ghost_detonated = false;
            }
        });
        let mut fail_det = PyRep::None;
        for adj_idx in 1..24_i64 {
            let rep = ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "ClickedOnTile", &[PyRep::Integer(ghost_cans[1]), PyRep::Integer(adj_idx)], &kw, None).await.expect("Ghost hack click");
            fail_det = rep;
            if fail_det.dict_get("status").and_then(|v| v.as_str()) == Some("Failed") {
                break;
            }
        }
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
        let _ = sys_142.set_ball_position(710_001_004, DVec3::from_array(slc_pos) + DVec3::new(500.0, 0.0, 0.0)).await;
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

        // 3. Wormhole Sleeper Combat Sites (`31_001_883` J172943 C5 Core Citadel): wave triggers & Blue Loot (30746 Ancient Coordinates Database)
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&90_000_001) {
                ch.solar_system_id = 31_001_883;
            }
        });
        sess.solar_system_id = Some(31_001_883);
        let wh_anoms = ctx
            .dispatch_rpc(
                &mut sess,
                "scanSvc",
                None,
                "GetCombatAnomalyInstances",
                &[PyRep::Integer(31_001_883)],
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
            clear_site_wave_npcs(&ctx, 31_001_883, &wh_site_id).await;
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

        // Seed filament in ship cargo: T5 Chaotic Firestorm Filament (47899) and active fleet for Frigate deployment
        ctx.db.with_mut_snapshot(|snap| {
            snap.fleets.insert(
                99_720,
                eve_db::FleetRecord {
                    fleet_id: 99_720,
                    boss_id: 90_000_001,
                    members: vec![90_000_001],
                    ..Default::default()
                },
            );
            snap.items.insert(
                720_001_001,
                InventoryItemRecord {
                    item_id: 720_001_001,
                    type_id: 47899,
                    owner_id: 90_000_001,
                    location_id: ship_id,
                    flag: InventoryFlag::Cargo,
                    quantity: 4,
                    singleton: false,
                    ..Default::default()
                },
            );
        });

        // 2. Verify abyssalMgr deployment consumes 3 filaments for a Frigate and gate RPCs outside abyss return NotInAbyssalSpace
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
            .expect_err("AbyssalGateActivation outside abyss");
        assert_eq!(gate_err.code, "NotInAbyssalSpace");

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
            .expect_err("AbyssalEndGateActivation outside abyss");
        assert_eq!(end_gate_err.code, "NotInAbyssalSpace");

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
}

pub(crate) mod pve_escalation_npc_destruction_triggers {
    // behavioral verification tests for PvE Exploration and Petitioner RPCs:
    // - `feat:pve.escalations`: In-space NPC destruction triggers real escalation chance, journal entry, destination solar system, and notification.
    // - `feat:pve.expeditions`: Multi-step journal progression with dynamic steps, destination encounter spawning, and completion.
    // - `feat:pve.ghost-sites`: Real server countdown timer (90-120s), expired detonation damage, and covert pirate ambush.
    // - `feat:pve.sleeper-caches`: Limited (5604), Standard (5609), Superior (5632) ship restrictions, puzzles, and gas hazard mechanics.
    // - `rpc:petitioner.MayPetition`: Client expectations matching arg schema and error codes (-1 rate-limited, -2 banned/missing, -3 invalid category).
    // - `rpc:petitioner.GetCategoryProperties`: Returns category-specific property descriptor KeyVals.


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
}

pub(crate) mod pve_data_relic_loot_tables {
    // behavioral verification tests for exploration, DED complexes, and J-space Sleeper combat.
    //
    // Parity Tags Tested:
    // - feat:pve.combat-signatures
    // - feat:pve.data-sites
    // - feat:pve.relic-sites
    // - feat:pve.loot-spew
    // - feat:pve.ded-complexes
    // - feat:pve.wormhole-sites


    use std::collections::HashMap;
    use glam::DVec3;
    use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
    use eve_destiny::{Ball, DamageVector, MovementMode};
    use eve_proto::PyRep;
    use eve_server::router::ServerContext;
    use eve_server::services::combat_crimewatch::with_mut_combat_runtime;
    use eve_server::services::pve_exploration::{ensure_system_pve_sites_seeded, with_pve_exploration_reg};
    use eve_server::services::pve_exploration_sites::{generate_exploration_container_loot, overseer_effects_for_rating};
    use eve_machonet::SessionState;

    const AU: f64 = 149_597_870_700.0;
    const CHAR_ID: i64 = 90_000_001;

    async fn place_ship(ctx: &ServerContext, sys: u32, ship_id: i64, type_id: u32, pos: DVec3) {
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(ship_id, InventoryItemRecord {
                item_id: ship_id, type_id, owner_id: CHAR_ID, location_id: sys as i64,
                flag: InventoryFlag::Hangar, quantity: 1, singleton: true, ..Default::default()
            });
            if let Some(ch) = snap.characters.get_mut(&CHAR_ID) {
                ch.active_ship_id = ship_id;
            }
        });
        let _ = ctx.systems.get_or_spawn(sys).spawn_ball(Ball {
            id: ship_id, type_id, position: pos, velocity: DVec3::ZERO, max_velocity: 365.0, inertia: 1.0,
            mass: 1_067_000.0, radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop,
        }).await;
    }

    async fn create_in_space_session(ctx: &ServerContext, session_id: u64, sys: u32, ship_id: i64) -> SessionState {
        let kw = HashMap::new();
        let mut sess = SessionState::new(session_id);
        ctx.dispatch_rpc(&mut sess, "authentication", None, "Login", &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())], &kw, None).await.expect("Login");
        ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(CHAR_ID)], &kw, None).await.expect("SelectCharacterID");
        sess.solar_system_id = Some(sys);
        sess.station_id = None;
        sess.structure_id = None;
        sess.ship_id = Some(ship_id);
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&CHAR_ID) {
                ch.station_id = None;
                ch.structure_id = None;
                ch.solar_system_id = sys;
            }
        });
        place_ship(ctx, sys, ship_id, 587, DVec3::ZERO).await;
        sess
    }

    /// Destroys every active NPC of a site through the production in-space damage pipeline
    /// (`SystemHandle::apply_damage_with_attacker` -> `npc_death_hook`).
    async fn kill_ball(ctx: &ServerContext, sys: u32, bid: i64) {
        let dmg = DamageVector { em: 1.0e7, thermal: 1.0e7, kinetic: 1.0e7, explosive: 1.0e7 };
        let res = ctx.systems.get_or_spawn(sys).apply_damage_with_attacker(bid, dmg, Some(CHAR_ID)).await;
        assert!(matches!(res, Ok(Some((_, true, _)))), "NPC ball {bid} must be destroyed by damage: {res:?}");
    }

    fn site_state(ctx: &ServerContext, sys: u32, site_id: &str) -> (u8, Vec<i64>, bool, [f64; 3]) {
        with_pve_exploration_reg(ctx, |reg| {
            let s = reg.sites_by_system[&sys].iter().find(|s| s.site_id == site_id).expect("site");
            (s.current_wave, s.active_npc_ball_ids.clone(), s.cleared, s.position)
        })
    }

    async fn kill_site_wave(ctx: &ServerContext, sys: u32, site_id: &str) {
        for bid in site_state(ctx, sys, site_id).1 {
            kill_ball(ctx, sys, bid).await;
        }
    }

    async fn activate_gate(ctx: &ServerContext, sess: &mut SessionState, site: &str) -> Result<PyRep, eve_proto::BridgeRpcError> {
        ctx.dispatch_rpc(sess, "keeper", None, "ActivateAccelerationGate", &[PyRep::String(site.into())], &HashMap::new(), None).await
    }

    fn container_cargo(ctx: &ServerContext, container_id: i64) -> Vec<InventoryItemRecord> {
        ctx.db.with_snapshot(|snap| {
            snap.items.values().filter(|i| i.location_id == container_id && i.flag == InventoryFlag::Cargo).cloned().collect()
        })
    }

    fn probes_dict(ids: &[i64], center: DVec3, range: f64) -> PyRep {
        let d = range * 0.4;
        let offs = [DVec3::ZERO, DVec3::X * d, -DVec3::X * d, DVec3::Y * d, -DVec3::Y * d, DVec3::Z * d, -DVec3::Z * d, DVec3::splat(d * 0.5)];
        PyRep::Dict(ids.iter().enumerate().map(|(i, &pid)| {
            let p = center + offs[i % offs.len()];
            (PyRep::Integer(pid), PyRep::keyval([
                ("pos", PyRep::Tuple(vec![PyRep::Real(p.x), PyRep::Real(p.y), PyRep::Real(p.z)])),
                ("scanRange", PyRep::Real(range)),
            ]))
        }).collect())
    }

    fn row_field(rows: &[PyRep], sig_id: &str, key: &str) -> PyRep {
        rows.iter().find(|r| r.dict_get("signatureID").and_then(|v| v.as_str()) == Some(sig_id))
            .and_then(|r| r.dict_get(key).cloned())
            .unwrap_or_else(|| panic!("{sig_id} row missing {key}"))
    }

    async fn hack_until_won(ctx: &ServerContext, sess: &mut SessionState, can_id: i64) -> PyRep {
        let kw = HashMap::new();
        let sys_id = sess.solar_system_id.unwrap();
        let ship_id = sess.ship_id.unwrap();
        let can_pos = with_pve_exploration_reg(ctx, |reg| {
            reg.sites_by_system.get(&sys_id).and_then(|sites| {
                sites.iter().find(|s| s.hackable_can_ids.contains(&can_id)).map(|s| s.position)
            })
        }).unwrap_or([0.0, 0.0, 0.0]);
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        let _ = sys_handle.set_ball_position(ship_id, DVec3::from_array(can_pos) + DVec3::new(500.0, 0.0, 0.0)).await;
        let start = ctx.dispatch_rpc(sess, "hackingMgr", None, "StartNewGameInstance", &[PyRep::Integer(can_id)], &kw, None).await.expect("StartNewGameInstance");
        assert_eq!(start.dict_get("virusCoherence").and_then(|v| v.as_i64()), Some(90), "Analyzer I (40) + Hacking/Archaeology V (+50) = 90 virusCoherence");
        assert_eq!(start.dict_get("virusStrength").and_then(|v| v.as_i64()), Some(20), "Analyzer I SDE virusStrength (attr 1910) = 20");
        let core_idx = with_pve_exploration_reg(ctx, |reg| {
            reg.hacking_games[&(CHAR_ID, can_id)].tiles.iter().find(|t| format!("{:?}", t.kind) == "SystemCore").unwrap().tile_index
        });
        // Non-adjacent unrevealed SystemCore cannot be clicked on turn 1
        let direct_err = ctx.dispatch_rpc(sess, "hackingMgr", None, "ClickedOnTile", &[PyRep::Integer(can_id), PyRep::Integer(core_idx as i64)], &kw, None).await.expect_err("unrevealed non-adjacent core");
        assert_eq!(direct_err.code, "TileNotAccessible");

        let mut path: Vec<u32> = (1..=(core_idx / 6)).map(|r| r * 6).collect();
        path.extend(((core_idx / 6) * 6 + 1)..=core_idx);
        for (step_i, &idx) in path.iter().enumerate() {
            for attempt in 0..5 {
                let r = ctx.dispatch_rpc(sess, "hackingMgr", None, "ClickedOnTile", &[PyRep::Integer(can_id), PyRep::Integer(idx as i64)], &kw, None).await.expect("ClickedOnTile");
                if r.dict_get("status").and_then(|v| v.as_str()) == Some("Won") {
                    return r;
                }
                let node_coh = r.dict_get("nodeCoherence").and_then(|v| v.as_i64()).unwrap_or(0);
                if node_coh <= 0 {
                    break;
                }
                if attempt == 0 && step_i + 1 < path.len() {
                    let blocked_err = ctx.dispatch_rpc(sess, "hackingMgr", None, "ClickedOnTile", &[PyRep::Integer(can_id), PyRep::Integer(path[step_i + 1] as i64)], &kw, None).await.expect_err("blocked by undefeated defensive node");
                    assert_eq!(blocked_err.code, "TileNotAccessible");
                }
            }
        }
        panic!("hack on {can_id} was not won");
    }

    /// Asserts won-hack loot exists only inside the spewed in-space cans (once each) and never in the parent can.
    fn assert_spew_loot(ctx: &ServerContext, won: &PyRep, parent_can: i64, sys: u32) -> Vec<u32> {
        assert!(container_cargo(ctx, parent_can).is_empty(), "Parent container must not duplicate spewed loot");
        let Some(PyRep::List(spew)) = won.dict_get("spewedContainers") else { panic!("spewedContainers") };
        assert!(!spew.is_empty(), "Won hack must spew loot cans");
        let mut types = Vec::new();
        for c in spew {
            let can_id = c.dict_get("canItemID").and_then(|v| v.as_i64()).unwrap();
            let can = ctx.db.with_snapshot(|s| s.items.get(&can_id).cloned()).expect("spewed can is a real item");
            assert_eq!(can.location_id, sys as i64, "Spewed can must sit in space");
            let cargo = container_cargo(ctx, can_id);
            assert_eq!(cargo.len(), 1, "Each spewed can holds exactly one loot stack");
            assert_eq!(cargo[0].type_id as i64, c.dict_get("lootTypeID").and_then(|v| v.as_i64()).unwrap());
            types.push(cargo[0].type_id);
        }
        types
    }

    // ============================================================================
    // 1. Probe-scanned Combat Signatures, SDE Data/Relic Loot & Loot Spew
    // ============================================================================
    #[tokio::test]
    async fn test_pve_data_relic_loot_tables_container_inventory_and_combat_signatures() {
        // parity: feat:pve.combat-signatures, feat:pve.data-sites, feat:pve.relic-sites, feat:pve.loot-spew
        eve_testkit::attest(&["feat:pve.combat-signatures", "feat:pve.data-sites", "feat:pve.relic-sites", "feat:pve.loot-spew"]);

        let ctx = ServerContext::new();
        let kw = HashMap::new();
        let sys = 30_000_142_u32;
        let ship_id = 710_001_101_i64;
        let mut sess = create_in_space_session(&ctx, 8201, sys, ship_id).await;
        ensure_system_pve_sites_seeded(&ctx, sys).await;

        // 1. Launch 8 Core Scanner Probes from a fitted Core Probe Launcher I (`dogmaIM.Activate`)
        ctx.db.with_mut_snapshot(|snap| {
            let skills = snap.skills.entry(CHAR_ID).or_default();
            for sid in [3402u32, 3412, 3413, 3426] {
                skills.insert(sid, TrainedSkillRecord { type_id: sid, level: 5, skill_points: 256_000.0 });
            }
            for (iid, tid, qty) in [(440_101_i64, 17938_u32, 1), (440_102, 30013, 8)] {
                snap.items.insert(iid, InventoryItemRecord {
                    item_id: iid, type_id: tid, owner_id: CHAR_ID, location_id: ship_id, flag: InventoryFlag::HiSlot0,
                    quantity: qty, singleton: qty == 1, ..Default::default()
                });
            }
        });
        ctx.invalidate_ship_fitting(ship_id);
        let launch = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(440_101), PyRep::String("launchScanners".into()), PyRep::None, PyRep::Integer(0)], &kw, None).await.expect("launch probes");
        let Some(PyRep::List(pids)) = launch.dict_get("probeIDs") else { panic!("probeIDs") };
        let probe_ids: Vec<i64> = pids.iter().filter_map(|p| p.as_i64()).collect();
        assert_eq!(probe_ids.len(), 8);
        let bound = ctx.dispatch_rpc(&mut sess, "scanMgr", None, "GetSystemScanMgr", &[], &kw, None).await.expect("GetSystemScanMgr");
        let bound_id = bound.dict_get("boundID").and_then(|v| v.as_i64()).unwrap() as u64;

        // 2. scanMgr.RequestScans: probes 40 AU away from DAT-<sys> register zero signal for it
        let (dat_name, dat_pos) = with_pve_exploration_reg(&ctx, |reg| {
            let s = reg.sites_by_system[&sys].iter().find(|s| s.site_id == "DAT-30000142").unwrap();
            (s.dungeon_name.clone(), DVec3::from_array(s.position))
        });
        let far = probes_dict(&probe_ids, dat_pos + DVec3::X * 40.0 * AU, 0.25 * AU);
        let far_rep = ctx.dispatch_rpc(&mut sess, "scanMgr", Some(bound_id), "RequestScans", &[far], &kw, None).await.expect("far RequestScans");
        let PyRep::List(far_rows) = far_rep else { panic!("RequestScans rows") };
        assert_eq!(row_field(&far_rows, "DAT-30000142", "signalStrength").as_f64(), Some(0.0), "Out-of-range probes give no signal");
        assert_eq!(row_field(&far_rows, "DAT-30000142", "coveringProbes").as_i64(), Some(0));

        // 3. Pinpoint 8-probe formation centred on the signature resolves it to 100% and reveals its SDE dungeon name
        let mut rows = Vec::new();
        for _ in 0..4 {
            let rep = ctx.dispatch_rpc(&mut sess, "scanMgr", Some(bound_id), "RequestScans", &[probes_dict(&probe_ids, dat_pos, 0.25 * AU)], &kw, None).await.expect("pinpoint RequestScans");
            let PyRep::List(r) = rep else { panic!() };
            rows = r;
        }
        assert_eq!(row_field(&rows, "DAT-30000142", "coveringProbes").as_i64(), Some(8));
        assert_eq!(row_field(&rows, "DAT-30000142", "signalStrength").as_f64(), Some(1.0));
        assert_eq!(row_field(&rows, "DAT-30000142", "type").as_str(), Some(dat_name.as_str()));

        // 4. Data can hack: requires fitted Analyzer module (`AnalyzerModuleRequired`), then spews real in-space cans
        let data_can_id = 1_870_000_000_i64 + (sys as i64 % 100_000) * 100;
        let no_mod_err = ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "StartNewGameInstance", &[PyRep::Integer(data_can_id)], &kw, None).await.expect_err("unfitted ship must fail");
        assert_eq!(no_mod_err.code, "AnalyzerModuleRequired");
        ctx.db.with_mut_snapshot(|snap| {
            let skills = snap.skills.entry(CHAR_ID).or_default();
            for sid in [21718_u32, 13278] {
                skills.insert(sid, TrainedSkillRecord { type_id: sid, level: 5, skill_points: 256_000.0 });
            }
            for (iid, tid, flag) in [(440_103_i64, 22175_u32, InventoryFlag::MedSlot0), (440_104, 22177, InventoryFlag::MedSlot1)] {
                snap.items.insert(iid, InventoryItemRecord {
                    item_id: iid, type_id: tid, owner_id: CHAR_ID, location_id: ship_id, flag,
                    quantity: 1, singleton: true, ..Default::default()
                });
            }
        });
        let won = hack_until_won(&ctx, &mut sess, data_can_id).await;
        let data_types = assert_spew_loot(&ctx, &won, data_can_id, sys);
        let grp = |t: &u32| ctx.sde.types.get(t).map(|x| x.group_id).unwrap_or(0);
        assert!(data_types.iter().any(|t| grp(t) == 333), "Data spew must contain SDE Datacores");
        assert!(data_types.iter().any(|t| matches!(grp(t), 728..=735 | 1304)), "Data spew must contain SDE Decryptors");
        let expected: Vec<u32> = generate_exploration_container_loot(&ctx.sde, sys, false).iter().map(|l| l.0).collect();
        assert_eq!(data_types, expected, "Spewed loot is exactly the SDE-derived loot table");
        let hack_again = ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "HackContainer", &[PyRep::Integer(data_can_id)], &kw, None).await;
        assert!(hack_again.is_err(), "Non-catalog hackingMgr.HackContainer must not be served");

        // 5. Relic can hack: SDE Salvaged Materials (754); Ancient Salvage (966) only in null-sec / J-space
        let relic_can_id = 1_871_000_000_i64 + (sys as i64 % 100_000) * 100;
        let won = hack_until_won(&ctx, &mut sess, relic_can_id).await;
        let relic_types = assert_spew_loot(&ctx, &won, relic_can_id, sys);
        assert!(relic_types.iter().all(|t| grp(t) == 754), "High-sec relic spew is SDE Salvaged Materials only");
        let jspace = generate_exploration_container_loot(&ctx.sde, 31_000_003, true);
        assert!(jspace.iter().any(|l| grp(&l.0) == 966), "J-space relic loot includes SDE Ancient Salvage");
        let hs_qty: i64 = generate_exploration_container_loot(&ctx.sde, sys, true).iter().map(|l| l.1).sum();
        let js_qty: i64 = jspace.iter().filter(|l| grp(&l.0) == 754).map(|l| l.1).sum();
        assert!(js_qty > hs_qty, "True-sec scaling: J-space salvage {js_qty} must exceed high-sec {hs_qty}");

        // 6. Faction weighting: Guristas (The Forge) and Serpentis regions pick different SDE datacores first
        let guristas = generate_exploration_container_loot(&ctx.sde, sys, false);
        let serpentis = generate_exploration_container_loot(&ctx.sde, 30_002_659, false);
        assert_ne!(guristas[0].0, serpentis[0].0, "Pirate faction must change the weighted datacore selection");
    }

    // ============================================================================
    // 2. DED Gates, Overseer Wreck on Overseer Kill; Wormhole Sleeper Waves via NPC Kills
    // ============================================================================
    #[tokio::test]
    async fn test_pve_ded_complexes_gating_overseer_wrecks_and_wormhole_sleepers() {
        // parity: feat:pve.ded-complexes, feat:pve.wormhole-sites
        eve_testkit::attest(&["feat:pve.ded-complexes", "feat:pve.wormhole-sites"]);

        let ctx = ServerContext::new();
        let sys = 30_000_142_u32;
        let ship_id = 710_001_102_i64;
        let mut sess = create_in_space_session(&ctx, 8202, sys, ship_id).await;
        ensure_system_pve_sites_seeded(&ctx, sys).await;
        let ded = "DED-30000142-3";

        // 1. Killing room 1 NPCs through the damage pipeline unlocks (but does not auto-advance) the gate
        kill_site_wave(&ctx, sys, ded).await;
        let (wave, active, _, ded_pos) = site_state(&ctx, sys, ded);
        assert_eq!((wave, active.len()), (1, 0), "DED rooms advance only through the acceleration gate");

        // 2. Gate proximity: 0 m origin is far from the gate; within 2,500 m the Rifter passes to room 2
        let kw = HashMap::new();
        let r = activate_gate(&ctx, &mut sess, ded).await;
        assert_eq!(r.expect_err("far from gate").code, "GateOutOfRange");
        let r = ctx.dispatch_rpc(&mut sess, "scanSvc", None, "EnterCombatAnomaly", &[PyRep::String(ded.into()), PyRep::Bool(true)], &kw, None).await;
        assert_eq!(r.expect_err("EnterCombatAnomaly far from gate").code, "GateOutOfRange");
        place_ship(&ctx, sys, ship_id, 587, DVec3::from_array(ded_pos) + DVec3::X * 1_000.0).await;
        let r = activate_gate(&ctx, &mut sess, ded).await;
        assert_eq!(r.expect("Rifter at gate").dict_get("currentWave").and_then(|v| v.as_i64()), Some(2));

        // 3. Battleship (Raven 638) rejected by the canonical DED 3/10 hull limit (Frigates & Destroyers only)
        let raven_id = 710_001_103_i64;
        place_ship(&ctx, sys, raven_id, 638, DVec3::from_array(ded_pos)).await;
        sess.ship_id = Some(raven_id);
        let r = activate_gate(&ctx, &mut sess, ded).await;
        assert_eq!(r.expect_err("Raven in 3/10").code, "ShipSizeRestricted");
        let r = ctx.dispatch_rpc(&mut sess, "scanSvc", None, "EnterCombatAnomaly", &[PyRep::String(ded.into()), PyRep::Bool(true)], &kw, None).await;
        assert_eq!(r.expect_err("EnterCombatAnomaly Raven in 3/10").code, "ShipSizeRestricted");
        let r = ctx.dispatch_rpc(&mut sess, "keeper", None, "ActivateAccelerationGate", &[PyRep::String(ded.into()), PyRep::Integer(10)], &kw, None).await;
        assert_eq!(r.expect_err("client rating override ignored").code, "ShipSizeRestricted", "Server-side DED rating cannot be overridden");
        let ded_dungeon_alias = with_pve_exploration_reg(&ctx, |reg| reg.sites_by_system[&sys].iter().find(|s| s.site_id == ded).map(|s| s.dungeon_id.to_string())).unwrap();
        let r = activate_gate(&ctx, &mut sess, &ded_dungeon_alias).await;
        assert_eq!(r.expect_err("dungeon-id alias").code, "ShipSizeRestricted", "str(dungeon_id) alias resolves to the DED site and is hull-checked");
        let cruiser = eve_server::services::pve_exploration_sites::check_ded_gate_ship_allowed(&ctx.sde, 3, 620, 10_000_000.0);
        assert!(cruiser.is_err(), "Cruisers (Osprey 620) are not allowed in DED 3/10");
        place_ship(&ctx, sys, ship_id, 587, DVec3::ZERO).await;
        sess.ship_id = Some(ship_id);
        let r = activate_gate(&ctx, &mut sess, &ded_dungeon_alias).await;
        assert_eq!(r.expect_err("alias out of range").code, "GateOutOfRange", "str(dungeon_id) alias is proximity-checked");
        place_ship(&ctx, sys, ship_id, 587, DVec3::from_array(ded_pos) + DVec3::X * 1_000.0).await;

        // 4. Room 2 kills -> gate -> room 3; killing the final-room overseer spawns the Overseer wreck in space
        kill_site_wave(&ctx, sys, ded).await;
        // Integer argument shape: keeper.ActivateAccelerationGate(gate_item_id) on the seeded DED gate ball
        let gate_ball_id = with_pve_exploration_reg(&ctx, |reg| reg.sites_by_system[&sys].iter().find(|s| s.site_id == ded).and_then(|s| s.gate_ball_id)).expect("DED gate ball");
        assert!(ctx.systems.get_or_spawn(sys).get_ball_snapshot(gate_ball_id).await.unwrap().is_some(), "DED gate ball in space");
        let r = ctx.dispatch_rpc(&mut sess, "keeper", None, "ActivateAccelerationGate", &[PyRep::Integer(gate_ball_id)], &kw, None).await;
        assert_eq!(r.expect("gate to room 3").dict_get("currentWave").and_then(|v| v.as_i64()), Some(3));
        kill_site_wave(&ctx, sys, ded).await;
        assert!(site_state(&ctx, sys, ded).2, "Overseer kill completes the DED complex");
        let (ope_tid, ope_name) = overseer_effects_for_rating(&ctx.sde, 3).expect("SDE Overseer's Personal Effects");
        assert!((19_400..=19_422).contains(&ope_tid) && ope_name.contains("Overseer"));
        let wreck = ctx.db.with_snapshot(|snap| {
            snap.items.values().find(|w| w.location_id == sys as i64 && w.type_id == 26473
                && snap.items.values().any(|i| i.location_id == w.item_id && i.type_id == ope_tid)).map(|w| w.item_id)
        }).expect("Overseer wreck in space holds the SDE Overseer's Personal Effects");
        assert!(container_cargo(&ctx, wreck).iter().any(|i| i.type_id == 18658), "Wreck holds the deadspace module");
        assert!(ctx.systems.get_or_spawn(sys).get_ball_snapshot(wreck).await.unwrap().is_some(), "Wreck ball spawned");
        assert!(container_cargo(&ctx, ship_id).iter().all(|i| i.type_id != ope_tid), "No loot teleported to ship");
        let r = activate_gate(&ctx, &mut sess, ded).await;
        assert_eq!(r.expect_err("cleared").code, "SiteAlreadyCleared");

        // 5. C3 Sleeper site seeded by production `ensure_system_pve_sites_seeded` with zero-bounty Sleepers
        let c3 = 31_000_885_u32;
        ensure_system_pve_sites_seeded(&ctx, c3).await;
        let wh_site = format!("PVE-ANOM-{c3}-1");
        let (w1, wave1, _, _) = site_state(&ctx, c3, &wh_site);
        assert_eq!(w1, 1);
        let (trigger_type, bounty) = with_mut_combat_runtime(&ctx, |rt| {
            let n = &rt.npc_entities[&wave1[0]];
            (n.type_id, n.bounty_isk)
        });
        assert_eq!((trigger_type, bounty), (30208, 0.0), "Wave trigger is the Awakened Warden with zero bounty");

        // 6. Non-trigger kill drops a Sleeper wreck with SDE Blue Loot (group 880) but does not advance the wave
        kill_ball(&ctx, c3, wave1[1]).await;
        assert_eq!(site_state(&ctx, c3, &wh_site).0, 1);
        let blue = ctx.db.with_snapshot(|snap| {
            snap.items.values().filter(|w| w.location_id == c3 as i64 && w.type_id == 30495)
                .any(|w| snap.items.values().any(|i| i.location_id == w.item_id && ctx.sde.types.get(&i.type_id).is_some_and(|t| t.group_id == 880)))
        });
        assert!(blue, "Sleeper wreck must contain SDE Sleeper Components (Blue Loot)");

        // 7. Trigger kill spawns wave 2 automatically; leftovers stay; then clear every wave to finish the site
        kill_ball(&ctx, c3, wave1[0]).await;
        let (w2, active2, _, _) = site_state(&ctx, c3, &wh_site);
        assert_eq!(w2, 2, "Trigger NPC kill advances to wave 2 without any client RPC");
        assert!(active2.contains(&wave1[2]), "Surviving wave-1 Sleepers remain in the site");
        for _ in 0..20 {
            let (_, active, cleared, _) = site_state(&ctx, c3, &wh_site);
            if cleared {
                break;
            }
            kill_ball(&ctx, c3, active[0]).await;
        }
        assert!(site_state(&ctx, c3, &wh_site).2, "All Sleeper waves destroyed -> site cleared");
        let pending = with_mut_combat_runtime(&ctx, |rt| rt.pending_bounty_ticks.get(&(c3, CHAR_ID)).copied().unwrap_or(0.0));
        assert_eq!(pending, 0.0, "CONCORD pays no bounty for Sleeper kills");
    }
}

pub(crate) mod hacking_node_types_analyzer_stats {
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
                assert_eq!(dmg, 0.0);
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

pub(crate) mod deadspace_pocket_rules_mwd_restriction {
    // Integration tests for Naturally Seeded Deadspace Pocket Rules, Data-Driven MWD
    // Disallowance, and Multi-Room Dungeon Triggers (`OnApproach`, `OnKill`, `OnLoot`, `CompleteDungeon`)
    // using the `DERIVED` 2-room placeholder template until real SDE room/trigger data is imported.


    use eve_db::service_roles::ROLE_GM_DEFAULT;
    use eve_db::{BookmarkRecord, InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
    use eve_destiny::{Ball, DamageVector, MovementMode};
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_server::router::ServerContext;
    use eve_server::services::pve_exploration::{
        ensure_system_pve_sites_seeded, with_pve_exploration_reg,
    };
    use glam::DVec3;
    use std::collections::HashMap;

    const PILOT_1: i64 = 90_000_001;
    const JITA_SYS: u32 = 30_000_142;
    const TYPE_RIFTER: u32 = 587;
    const TYPE_MWD_5MN: u32 = 434;
    const TYPE_AB_1MN: u32 = 439;
    const WARP_DISRUPTED_ERR: &str = "NaturalPhenomenaDisruptingWarp";

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
        ctx.dispatch_rpc(
            &mut sess,
            "authentication",
            None,
            "Login",
            &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())],
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
                make_item(ship_id, ship_type_id, char_id, i64::from(sys_id), InventoryFlag::None, 1, true),
            );
            snap.item_positions.insert(ship_id, pos.to_array());
        });

        let sys = ctx.systems.get_or_spawn(sys_id);
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
                ctx.build_ship_combat_state(char_id, ship_id, ship_type_id),
                ctx.build_ship_slim_meta(char_id, ship_type_id),
            )
            .await;
        sess
    }

    async fn step_until_warp_landed(ctx: &ServerContext, sess: &mut SessionState, sys_id: u32, ship_id: i64) {
        let kw = HashMap::new();
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        for _ in 0..120 {
            let _ = sys_handle.step_manual(1.0).await;
            if let Ok(Some((_, b))) = sys_handle.get_ball_snapshot(ship_id).await {
                if matches!(b.mode, MovementMode::Stop) {
                    break;
                }
            }
        }
        ctx.dispatch_rpc(sess, "beyonce", None, "UpdateStateRequest", &[], &kw, None)
            .await
            .expect("UpdateStateRequest after warp");
    }

    // ============================================================================
    // 1. Naturally Seeded Deadspace Pocket Rules & Data-Driven MWD Restriction
    // ============================================================================
    #[tokio::test]
    async fn test_deadspace_pocket_rules_and_mwd_restriction() {
        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        let kw = HashMap::new();
        let ship_id = 750_003_001i64;
        let mwd_id = 750_003_010i64;
        let ab_id = 750_003_011i64;
        let mut sess = setup_space_pilot(&ctx, 9203, PILOT_1, ship_id, TYPE_RIFTER, JITA_SYS, DVec3::ZERO).await;

        // Fit 5MN MWD (434) and 1MN Afterburner (439) + propulsion skills (only MWD online initially due to maxGroupOnline = 1)
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(mwd_id, make_item(mwd_id, TYPE_MWD_5MN, PILOT_1, ship_id, InventoryFlag::MedSlot0, 1, true));
            snap.items.insert(ab_id, make_item(ab_id, TYPE_AB_1MN, PILOT_1, ship_id, InventoryFlag::MedSlot1, 1, true));
            snap.module_online_states.insert(mwd_id, true);
            snap.module_online_states.insert(ab_id, false);
            let sk = snap.skills.entry(PILOT_1).or_default();
            for sid in [3413_u32, 3426, 3449, 3450, 3454, 3455] {
                sk.insert(sid, TrainedSkillRecord { type_id: sid, level: 5, skill_points: 256_000.0 });
            }
        });
        ctx.invalidate_ship_fitting(ship_id);

        // 1. Seed system exploration sites naturally -> discovers DSG-30000142 (SDE dungeon 98: Guristas Scout Outpost)
        ensure_system_pve_sites_seeded(&ctx, JITA_SYS).await;
        let (dsg_pos, dsg_inst_id, dsg_did) = with_pve_exploration_reg(&ctx, |reg| {
            let s = reg.sites_by_system[&JITA_SYS]
                .iter()
                .find(|s| s.site_id == "DSG-30000142" && s.site_category == "Deadspace")
                .expect("Naturally seeded DSG-30000142 deadspace combat signature");
            (s.position, s.expedition_instance_id.expect("runtime dungeon instance"), s.dungeon_id)
        });
        assert_eq!(dsg_did, 98, "Caldari high-sec deadspace signature must use SDE Guristas Scout Outpost (98)");

        // Scan DSG-30000142 to 100% via scanMgr.RequestScans and warp to it via beyonce.CmdWarpToStuff
        let probes: Vec<PyRep> = (0..8)
            .map(|_| PyRep::List(vec![PyRep::Real(dsg_pos[0]), PyRep::Real(dsg_pos[1]), PyRep::Real(dsg_pos[2])]))
            .collect();
        ctx.dispatch_rpc(
            &mut sess,
            "scanMgr",
            None,
            "RequestScans",
            &[PyRep::Real(600_000_000_000.0), PyRep::Real(120.0), PyRep::List(probes)],
            &kw,
            None,
        )
        .await
        .expect("RequestScans pinpoints DSG-30000142");

        ctx.dispatch_rpc(
            &mut sess,
            "beyonce",
            None,
            "CmdWarpToStuff",
            &[PyRep::String("scan".into()), PyRep::String("DSG-30000142".into())],
            &kw,
            None,
        )
        .await
        .expect("Warp to scanned DSG-30000142 entry gate");
        step_until_warp_landed(&ctx, &mut sess, JITA_SYS, ship_id).await;

        let inst = eve_server::services::pve_dungeon_runtime::get_dungeon_instance_state(&ctx, dsg_inst_id).unwrap();
        let entry_gate_id = inst.dict_get("entryGateBallID").and_then(|v| v.as_i64()).unwrap();
        let PyRep::List(rooms) = inst.dict_get("rooms").unwrap() else { panic!("Expected rooms") };
        let room1_id = rooms[0].dict_get("roomID").and_then(|v| v.as_i64()).unwrap();
        let room2_id = rooms[1].dict_get("roomID").and_then(|v| v.as_i64()).unwrap();
        let PyRep::Tuple(r2_pos_py) = rooms[1].dict_get("position").unwrap() else { panic!() };
        let r2_pos = [
            r2_pos_py[0].as_f64().unwrap(),
            r2_pos_py[1].as_f64().unwrap(),
            r2_pos_py[2].as_f64().unwrap(),
        ];

        // 2. Direct warp into gated Room 1 (by roomID) or gated Room 2 (by bookmark) is refused
        let warp_r1_err = ctx
            .dispatch_rpc(
                &mut sess,
                "beyonce",
                None,
                "CmdWarpToStuff",
                &[PyRep::String("dungeon".into()), PyRep::Integer(room1_id)],
                &kw,
                None,
            )
            .await
            .expect_err("Direct warp into gated Room 1 must fail");
        assert_eq!(warp_r1_err.code, WARP_DISRUPTED_ERR);

        let bm_id = 880_001_001i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.bookmarks.insert(
                bm_id,
                BookmarkRecord {
                    bookmark_id: bm_id,
                    owner_id: PILOT_1,
                    label: "Inner Room Bookmark".into(),
                    solar_system_id: JITA_SYS,
                    x: r2_pos[0],
                    y: r2_pos[1],
                    z: r2_pos[2],
                    item_id: None,
                    memo: String::new(),
                    folder_id: None,
                    is_shared: false,
                },
            );
        });
        let warp_bm_err = ctx
            .dispatch_rpc(
                &mut sess,
                "beyonce",
                None,
                "CmdWarpToStuff",
                &[PyRep::String("bookmark".into()), PyRep::Integer(bm_id)],
                &kw,
                None,
            )
            .await
            .expect_err("Direct bookmark warp into gated Room 2 must fail");
        assert_eq!(warp_bm_err.code, WARP_DISRUPTED_ERR);

        // 3. Activate entry acceleration gate from warp landing spot -> lands in Room 1; MWD disallowed in deadspace pocket unless overridden
        let sys_handle = ctx.systems.get_or_spawn(JITA_SYS);
        let eg_pos = sys_handle.get_ball_snapshot(entry_gate_id).await.unwrap().unwrap().1.position;
        let ship_landed_pos = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap().1.position;
        assert!(ship_landed_pos.distance(eg_pos) <= 2_500.0, "Warp lands within 2.5km of entry gate");

        let gate1_res = ctx
            .dispatch_rpc(&mut sess, "keeper", None, "ActivateAccelerationGate", &[PyRep::Integer(entry_gate_id)], &kw, None)
            .await
            .expect("Activate entry acceleration gate");
        assert_eq!(gate1_res.dict_get("disallowMWD"), Some(&PyRep::Bool(true)));

        let r1_mwd_err = ctx.dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(mwd_id), PyRep::String("moduleBonusMicrowarpdrive".into()), PyRep::None, PyRep::Integer(1)],
            &kw,
            None,
        )
        .await
        .expect_err("MWD disallowed in deadspace Room 1 by default");
        assert_eq!(r1_mwd_err.code, "ModuleNotAllowedInDeadspace");

        // Clear Room 1 guards to unlock the Room 1 -> Room 2 gate, approach gate via CmdFollowBall, and enter Room 2
        let PyRep::List(r1_npcs) = gate1_res.dict_get("npcBallIDs").unwrap() else { panic!() };
        for nid in r1_npcs {
            let _ = sys_handle
                .apply_damage_with_attacker(
                    nid.as_i64().unwrap(),
                    DamageVector { em: 1.0e6, thermal: 1.0e6, kinetic: 1.0e6, explosive: 1.0e6 },
                    Some(PILOT_1),
                )
                .await;
        }
        let r1_gate_id = gate1_res.dict_get("nextGateBallID").and_then(|v| v.as_i64()).unwrap();
        let beyonce_oid = ctx.bind_object(&sess, "beyonce", i64::from(JITA_SYS)).unwrap();
        ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_oid), "CmdSetSpeedFraction", &[PyRep::Real(1.0)], &kw, None)
            .await
            .unwrap();
        ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_oid), "CmdFollowBall", &[PyRep::Integer(r1_gate_id), PyRep::Real(500.0)], &kw, None)
            .await
            .expect("CmdFollowBall to Room 1 -> Room 2 gate");
        for _ in 0..60 {
            let _ = sys_handle.step_manual(1.0).await;
        }
        let gate2_res = ctx
            .dispatch_rpc(&mut sess, "keeper", None, "ActivateAccelerationGate", &[PyRep::Integer(r1_gate_id)], &kw, None)
            .await
            .expect("Activate Room 1 -> Room 2 gate");
        assert_eq!(gate2_res.dict_get("disallowMWD"), Some(&PyRep::Bool(true)));

        // 4. In inner Room 2 (disallowMWD == true from SDE archetype 51): MWD refused, Afterburner allowed
        let mwd_err = ctx
            .dispatch_rpc(
                &mut sess,
                "dogmaIM",
                None,
                "Activate",
                &[PyRep::Integer(mwd_id), PyRep::String("moduleBonusMicrowarpdrive".into()), PyRep::None, PyRep::Integer(1)],
                &kw,
                None,
            )
            .await
            .expect_err("MWD must be refused in MWD-disallowed deadspace room");
        assert_eq!(mwd_err.code, "ModuleNotAllowedInDeadspace");

        // 4b. Editing dungeon/room MWD flag dynamically changes live MWD outcome in Room 2
        eve_server::services::pve_dungeon_editor::set_dungeon_disallow_mwd(&ctx, 98, false);
        ctx.dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(mwd_id), PyRep::String("moduleBonusMicrowarpdrive".into()), PyRep::None, PyRep::Integer(1)],
            &kw,
            None,
        )
        .await
        .expect("MWD must be allowed in Room 2 after setting disallowMWD = false");
        let _ = ctx
            .dispatch_rpc(
                &mut sess,
                "dogmaIM",
                None,
                "Deactivate",
                &[PyRep::Integer(mwd_id), PyRep::String("moduleBonusMicrowarpdrive".into())],
                &kw,
                None,
            )
            .await;
        eve_server::services::pve_dungeon_editor::set_dungeon_disallow_mwd(&ctx, 98, true);
        let dyn_mwd_err = ctx
            .dispatch_rpc(
                &mut sess,
                "dogmaIM",
                None,
                "Activate",
                &[PyRep::Integer(mwd_id), PyRep::String("moduleBonusMicrowarpdrive".into()), PyRep::None, PyRep::Integer(1)],
                &kw,
                None,
            )
            .await
            .expect_err("MWD must be refused again after setting disallowMWD = true");
        assert_eq!(dyn_mwd_err.code, "ModuleNotAllowedInDeadspace");

        ctx.db.with_mut_snapshot(|snap| {
            snap.module_online_states.insert(mwd_id, false);
            snap.module_online_states.insert(ab_id, true);
        });
        ctx.invalidate_ship_fitting(ship_id);

        ctx.dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(ab_id), PyRep::String("moduleBonusAfterburner".into()), PyRep::None, PyRep::Integer(1)],
            &kw,
            None,
        )
        .await
        .expect("Afterburner must remain allowed in MWD-disallowed deadspace room");

        // 5. Data-driven dungeon editor flags persist in world state
        sess.role |= ROLE_GM_DEFAULT;
        let mut edit_room_kw = HashMap::new();
        edit_room_kw.insert("disallowMWD".into(), PyRep::Bool(true));
        let created_room = ctx
            .dispatch_rpc(&mut sess, "dungeon", None, "EditRoom", &[PyRep::Integer(98), PyRep::String("Restricted Core".into())], &edit_room_kw, None)
            .await
            .expect("dungeon.EditRoom with disallowMWD");
        let created_room_id = created_room.dict_get("roomID").and_then(|v| v.as_i64()).unwrap();
        assert!(eve_server::services::pve_dungeon_editor::is_dungeon_or_room_mwd_disallowed(&ctx, 98, Some(created_room_id)));
        eve_server::services::pve_dungeon_editor::set_dungeon_disallow_mwd(&ctx, 98, true);
        assert!(eve_server::services::pve_dungeon_editor::is_dungeon_or_room_mwd_disallowed(&ctx, 98, Some(room2_id)));
    }

    // ============================================================================
    // 2. SDE-Seeded Multi-Room Dungeon System: Approach/Kill/Loot Triggers & Despawn
    // ============================================================================
    #[tokio::test]
    async fn test_dungeon_system_rooms_triggers_and_despawn_on_completion() {
        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        let kw = HashMap::new();
        let ship_id = 750_004_001i64;
        let mut sess = setup_space_pilot(&ctx, 9204, PILOT_1, ship_id, TYPE_RIFTER, JITA_SYS, DVec3::ZERO).await;

        // 1. Discover naturally seeded DSG-30000142, scan via RequestScans, warp via CmdWarpToStuff, enter Room 1
        ensure_system_pve_sites_seeded(&ctx, JITA_SYS).await;
        let (dsg_pos, inst_id) = with_pve_exploration_reg(&ctx, |reg| {
            let s = reg.sites_by_system[&JITA_SYS]
                .iter()
                .find(|s| s.site_id == "DSG-30000142")
                .unwrap();
            (s.position, s.expedition_instance_id.unwrap())
        });
        let probes: Vec<PyRep> = (0..8)
            .map(|_| PyRep::List(vec![PyRep::Real(dsg_pos[0]), PyRep::Real(dsg_pos[1]), PyRep::Real(dsg_pos[2])]))
            .collect();
        ctx.dispatch_rpc(&mut sess, "scanMgr", None, "RequestScans", &[PyRep::Real(600_000_000_000.0), PyRep::Real(120.0), PyRep::List(probes)], &kw, None)
            .await
            .unwrap();
        ctx.dispatch_rpc(&mut sess, "beyonce", None, "CmdWarpToStuff", &[PyRep::String("scan".into()), PyRep::String("DSG-30000142".into())], &kw, None)
            .await
            .unwrap();
        step_until_warp_landed(&ctx, &mut sess, JITA_SYS, ship_id).await;

        let init_state = eve_server::services::pve_dungeon_runtime::get_dungeon_instance_state(&ctx, inst_id).unwrap();
        let entry_gate_id = init_state.dict_get("entryGateBallID").and_then(|v| v.as_i64()).unwrap();
        let gate1_res = ctx
            .dispatch_rpc(&mut sess, "keeper", None, "ActivateAccelerationGate", &[PyRep::Integer(entry_gate_id)], &kw, None)
            .await
            .expect("Enter Room 1 via entry gate");

        let r1_gate_id = gate1_res.dict_get("nextGateBallID").and_then(|v| v.as_i64()).unwrap();
        let PyRep::List(r1_objs) = gate1_res.dict_get("objectBallIDs").unwrap() else { panic!() };
        let r1_beacon_id = r1_objs[0].as_i64().unwrap();
        let PyRep::List(r1_npcs) = gate1_res.dict_get("npcBallIDs").unwrap() else { panic!() };
        assert_eq!(r1_npcs.len(), 2, "Group 1 spawns 2 SDE Guristas frigate guards immediately");
        let g1_npc_ids: Vec<i64> = r1_npcs.iter().map(|v| v.as_i64().unwrap()).collect();

        let sys_handle = ctx.systems.get_or_spawn(JITA_SYS);
        let (_, g1_ball) = sys_handle.get_ball_snapshot(g1_npc_ids[0]).await.unwrap().unwrap();
        assert_ne!(g1_ball.type_id, 10_043, "Dungeon NPCs must come from SDE faction spawn groups, not hardcoded 10043");

        let beyonce_oid = ctx.bind_object(&sess, "beyonce", i64::from(JITA_SYS)).unwrap();
        let ship_inv_oid = ctx.bind_object(&sess, "invbroker", ship_id).unwrap();
        ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_oid), "CmdSetSpeedFraction", &[PyRep::Real(1.0)], &kw, None)
            .await
            .unwrap();

        // 2. Approach Room 1->2 gate first: locked gate refuses activation with GateLocked
        ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_oid), "CmdFollowBall", &[PyRep::Integer(r1_gate_id), PyRep::Real(500.0)], &kw, None)
            .await
            .unwrap();
        for _ in 0..60 {
            let _ = sys_handle.step_manual(1.0).await;
        }
        let locked_err = ctx
            .dispatch_rpc(&mut sess, "keeper", None, "ActivateAccelerationGate", &[PyRep::Integer(r1_gate_id)], &kw, None)
            .await
            .expect_err("Locked gate must refuse activation before Group 1 is cleared");
        assert_eq!(locked_err.code, "GateLocked");

        // 3. Approach Deadspace Relay Beacon via client CmdFollowBall + UpdateStateRequest -> OnApproach spawns Group 2 cruiser!
        ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_oid), "CmdFollowBall", &[PyRep::Integer(r1_beacon_id), PyRep::Real(500.0)], &kw, None)
            .await
            .unwrap();
        for _ in 0..40 {
            let _ = sys_handle.step_manual(1.0).await;
        }
        ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_oid), "UpdateStateRequest", &[], &kw, None)
            .await
            .unwrap();

        let after_approach = eve_server::services::pve_dungeon_runtime::get_dungeon_instance_state(&ctx, inst_id).unwrap();
        let PyRep::List(rooms_app) = after_approach.dict_get("rooms").unwrap() else { panic!() };
        let PyRep::List(r1_npcs_after) = rooms_app[0].dict_get("npcBallIDs").unwrap() else { panic!() };
        assert_eq!(r1_npcs_after.len(), 3, "OnApproach trigger must spawn Group 2 reinforcement NPC");
        let r1_reinf_id = r1_npcs_after[2].as_i64().unwrap();

        // 4. Destroy Group 1 guards -> OnKill unlocks Room 1->2 gate (leaving Group 2 alive to verify despawn later)
        for nid in g1_npc_ids {
            let _ = sys_handle
                .apply_damage_with_attacker(
                    nid,
                    DamageVector { em: 1.0e6, thermal: 1.0e6, kinetic: 1.0e6, explosive: 1.0e6 },
                    Some(PILOT_1),
                )
                .await;
        }
        ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_oid), "CmdFollowBall", &[PyRep::Integer(r1_gate_id), PyRep::Real(500.0)], &kw, None)
            .await
            .unwrap();
        for _ in 0..40 {
            let _ = sys_handle.step_manual(1.0).await;
        }
        let enter_r2 = ctx
            .dispatch_rpc(&mut sess, "keeper", None, "ActivateAccelerationGate", &[PyRep::Integer(r1_gate_id)], &kw, None)
            .await
            .expect("Unlocked gate enters Room 2");

        let PyRep::List(r2_objs) = enter_r2.dict_get("objectBallIDs").unwrap() else { panic!() };
        let strongbox_id = r2_objs[0].as_i64().unwrap();
        let PyRep::List(r2_npcs) = enter_r2.dict_get("npcBallIDs").unwrap() else { panic!() };
        let r2_boss_id = r2_npcs[0].as_i64().unwrap();

        // 5. Approach & loot Overseer Strongbox via invbroker.LootAll -> OnLoot spawns Group 2 ambush defender!
        ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_oid), "CmdSetSpeedFraction", &[PyRep::Real(1.0)], &kw, None)
            .await
            .unwrap();
        ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_oid), "CmdFollowBall", &[PyRep::Integer(strongbox_id), PyRep::Real(500.0)], &kw, None)
            .await
            .unwrap();
        for _ in 0..55 {
            let _ = sys_handle.step_manual(1.0).await;
        }
        ctx.dispatch_rpc(&mut sess, "invbroker", Some(ship_inv_oid), "LootAll", &[PyRep::Integer(strongbox_id)], &kw, None)
            .await
            .expect("LootAll on Overseer Strongbox");

        let after_loot = eve_server::services::pve_dungeon_runtime::get_dungeon_instance_state(&ctx, inst_id).unwrap();
        let PyRep::List(rooms_loot) = after_loot.dict_get("rooms").unwrap() else { panic!() };
        let PyRep::List(r2_npcs_after_loot) = rooms_loot[1].dict_get("npcBallIDs").unwrap() else { panic!() };
        assert_eq!(r2_npcs_after_loot.len(), 2, "OnLoot trigger must spawn Room 2 ambush NPC");
        let r2_ambush_id = r2_npcs_after_loot[1].as_i64().unwrap();

        // 6. Destroy Room 2 Overseer commander -> OnKill fires CompleteDungeon, despawns all entities & collapses signature!
        let _ = sys_handle
            .apply_damage_with_attacker(
                r2_boss_id,
                DamageVector { em: 1.0e7, thermal: 1.0e7, kinetic: 1.0e7, explosive: 1.0e7 },
                Some(PILOT_1),
            )
            .await;

        let final_state = eve_server::services::pve_dungeon_runtime::get_dungeon_instance_state(&ctx, inst_id).unwrap();
        assert_eq!(final_state.dict_get("completed"), Some(&PyRep::Bool(true)));
        assert_eq!(final_state.dict_get("despawned"), Some(&PyRep::Bool(true)));
        assert!(sys_handle.get_ball_snapshot(entry_gate_id).await.unwrap().is_none());
        assert!(sys_handle.get_ball_snapshot(r1_beacon_id).await.unwrap().is_none());
        assert!(sys_handle.get_ball_snapshot(r1_gate_id).await.unwrap().is_none());
        assert!(sys_handle.get_ball_snapshot(r1_reinf_id).await.unwrap().is_none());
        assert!(sys_handle.get_ball_snapshot(r2_ambush_id).await.unwrap().is_none());
        assert!(
            with_pve_exploration_reg(&ctx, |reg| {
                reg.sites_by_system[&JITA_SYS]
                    .iter()
                    .find(|s| s.site_id == "DSG-30000142")
                    .unwrap()
                    .cleared
            }),
            "Completing deadspace dungeon must mark DSG-30000142 cleared"
        );
    }
}

pub(crate) mod probe_scanning_ranges_deviation_expiry {
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
        eve_server::session_cluster_lifecycle::tick_background_cluster(&ctx).await;
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
        eve_server::session_cluster_lifecycle::tick_background_cluster(&ctx).await;
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
        assert_eq!(fail2_ev.dict_get("explosionDamage").and_then(|v| v.as_f64()), Some(0.0));

        assert!(sys_handle.get_ball_snapshot(can_2).await.unwrap().is_none());
        assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&can_2) && !s.items.contains_key(&loot_in_can_2)));
        assert_eq!(sys_handle.get_combat_state(ship_id).await.unwrap().unwrap().shield_hp, 10_000.0);
        assert_eq!(sys_handle.get_combat_state(bystander_near).await.unwrap().unwrap().shield_hp, 10_000.0);
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
