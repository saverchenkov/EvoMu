//! Phase 8 (`W-3` & `W-4`) Complete PvE Exploration:
//! Combat Anomalies, Escalations, DED Complexes, Sansha Incursions,
//! Homefront Operations, Metaliminal Storms, Drifter Hives & Triglavian Sites.

use eve_db::{FleetRecord, InventoryFlag, InventoryItemRecord};
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
    eve_testkit::attest(&[
        "feat:pve.combat-anomalies",
        "feat:pve.escalations",
        "feat:pve.expeditions",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let mut sess = create_in_space_session(&ctx, 8101, 30_000_142, 710_001_001, 587).await;

    // 1. Verify regional pirate faction anomalies across 4 empires:
    // Caldari (Jita 30000142 -> Guristas), Amarr (30002187 -> Blood Raiders/Sansha),
    // Gallente (Dodixie 30002659 -> Serpentis), Minmatar (Rens 30002510 -> Angel Cartel)
    for (sys, expected_kw) in [
        (30_000_142_u32, "Guristas"),
        (30_002_187, "Blood"),
        (30_002_659, "Serpentis"),
        (30_002_510, "Angel"),
    ] {
        let rep = ctx
            .dispatch_rpc(&mut sess, "scanSvc", None, "GetCombatAnomalyInstances", &[PyRep::Integer(sys as i64)], &kw, None)
            .await
            .expect("GetCombatAnomalyInstances");
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

    // 1. Verify DED 3/10 complex names across regional pirate factions:
    // Caldari (Jita 30000142) -> Guristas Guerilla Grounds, Amarr (30002187) -> Blood Raider Human Farm,
    // Gallente (Dodixie 30002659) -> Serpentis Narcotic Warehouses, Minmatar (Rens 30002510) -> Angel Creo-Corp Mining
    for (sys, exp_name) in [
        (30_000_142_u32, "Guristas Guerilla Grounds"),
        (30_002_187, "Blood Raider Human Farm"),
        (30_002_659, "Serpentis Narcotic Warehouses"),
        (30_002_510, "Angel Creo-Corp Mining"),
    ] {
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&90_000_001) {
                ch.solar_system_id = sys;
            }
        });
        sess.solar_system_id = Some(sys);
        let sigs_rep = ctx.dispatch_rpc(&mut sess, "scanSvc", None, "GetSignatureInstances", &[PyRep::Integer(sys as i64)], &kw, None).await.expect("GetSignatureInstances");
        let PyRep::List(sigs) = &sigs_rep else { panic!("Expected List") };
        let ded_sig_id = format!("DED-{sys}-3");
        assert!(sigs.iter().any(|s| s.dict_get("signatureID").and_then(|v| v.as_str()) == Some(&ded_sig_id)));

        let full_rep = ctx.dispatch_rpc(&mut sess, "scanSvc", None, "ScanSignature", &[PyRep::String(ded_sig_id), PyRep::Real(1.0)], &kw, None).await.expect("Scan DED");
        assert_eq!(full_rep.dict_get("siteName").and_then(|v| v.as_str()), Some(exp_name));
    }
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.solar_system_id = 30_000_142;
        }
    });
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
// 3. Sansha Incursions, Fleet ISK + CONCORD LP Payouts, Effects & Mothership Finale
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
    ensure_special_sites_spawned(&ctx).await;
    let kw = HashMap::new();

    let (target_cid, hq_sys_id) = with_pve_special_reg(&ctx, |reg| {
        let (cid, inc) = reg.incursions.iter().next().unwrap();
        (*cid, inc.hq_solar_system_id)
    });
    let mut sess = create_in_space_session(&ctx, 8105, hq_sys_id, 710_001_005, 587).await;

    // 1. Query active incursion penalties/effects: cyno jammer active, shield penalties at high influence
    let eff_rep = ctx.dispatch_rpc(&mut sess, "incursion", None, "GetIncursionSystemEffects", &[PyRep::Integer(hq_sys_id as i64)], &kw, None).await.expect("GetIncursionSystemEffects");
    assert_eq!(eff_rep.dict_get("influence").and_then(|v| v.as_f64()), Some(1.0));
    assert_eq!(eff_rep.dict_get("cynoJammed").and_then(|v| v.as_bool()), Some(true));
    assert!(eff_rep.dict_get("shieldPenalty").and_then(|v| v.as_f64()).unwrap_or(0.0) > 0.0);

    // Mothership HQ site is locked at high influence (1.0)
    let inc_rep = ctx.dispatch_rpc(&mut sess, "incursion", None, "GetAllIncursionSites", &[PyRep::Integer(target_cid as i64)], &kw, None).await.expect("GetAllIncursionSites");
    assert_eq!(inc_rep.dict_get("influence").and_then(|v| v.as_f64()), Some(1.0));
    assert_eq!(inc_rep.dict_get("mothershipSpawned").and_then(|v| v.as_bool()), Some(false));

    // Verify unseeded incursion constellation returns IncursionNotFound without mutating state (BF-02)
    let unseeded_err = ctx.dispatch_rpc(&mut sess, "incursion", None, "GetAllIncursionSites", &[PyRep::Integer(20_000_050)], &kw, None).await.expect_err("Unseeded incursion constellation should not be found");
    assert_eq!(unseeded_err.code, "IncursionNotFound");

    // Verify Dogma penalties applied to ship in incursion system (BF-02)
    let mut ship_attrs = HashMap::from([(263, 1000.0), (265, 1000.0), (482, 1000.0), (64, 1.0)]);
    eve_server::services::inventory_fitting::fitting_scanners_superweapons::apply_active_dbuffs_and_system_effects_to_ship_attrs(&ctx, 710_001_005, &mut ship_attrs);
    assert_eq!(ship_attrs[&263], 500.0, "50% shield penalty at 1.0 influence");
    assert_eq!(ship_attrs[&265], 500.0, "50% armor penalty at 1.0 influence");
    assert_eq!(ship_attrs[&482], 500.0, "50% capacitor penalty at 1.0 influence");
    assert_eq!(ship_attrs[&64], 0.5, "50% damage penalty at 1.0 influence");

    let hq_sys = ctx.systems.get_or_spawn(hq_sys_id);
    let ms_boss_bid_init = with_pve_special_reg(&ctx, |reg| {
        reg.incursions[&target_cid].sites.iter().find(|s| s.dungeon_id == 5158).unwrap().boss_npc_ball_id
    });
    assert!(hq_sys.get_ball_snapshot(ms_boss_bid_init).await.unwrap().is_none(), "MS boss ball must not exist while locked");
    let lock_res = eve_server::services::pve_incursion_sites::on_incursion_boss_destroyed(&ctx, hq_sys_id, ms_boss_bid_init, Some(90_000_001)).await;
    assert_eq!(lock_res.unwrap().unwrap_err().code, "MothershipSiteLocked");
    let ms_boss_destroyed_pre = with_pve_special_reg(&ctx, |reg| {
        reg.incursions[&target_cid].sites.iter().find(|s| s.dungeon_id == 5158).unwrap().boss_destroyed
    });
    assert!(!ms_boss_destroyed_pre, "boss_destroyed must remain false when locked");

    // 2. Complete HQ sites (`INC-HQ-5175`) 4 times to drop influence from 1.0 -> 0.0 and unlock Mothership
    let (hq_site_pos, ms_site_pos) = with_pve_special_reg(&ctx, |reg| {
        let inc = &reg.incursions[&target_cid];
        (
            inc.sites.iter().find(|s| s.dungeon_id == 5175).unwrap().position,
            inc.sites.iter().find(|s| s.dungeon_id == 5158).unwrap().position,
        )
    });
    let exp_hq_pos = eve_server::services::pve_incursion_sites::derive_incursion_site_position(&ctx.sde, hq_sys_id, 3);
    let exp_ms_pos = eve_server::services::pve_incursion_sites::derive_incursion_site_position(&ctx.sde, hq_sys_id, 4);
    assert!(hq_site_pos.distance(exp_hq_pos) < 0.01, "HQ site position must match derive_incursion_site_position");
    assert!(ms_site_pos.distance(exp_ms_pos) < 0.01, "MS site position must match derive_incursion_site_position");
    let cels = ctx.sde.celestials_in_system(hq_sys_id);
    let hq_cel = cels.iter().find(|c| (hq_site_pos.distance(c.position) - (c.radius.max(50_000.0) + 150_000.0)).abs() < 1.0).expect("HQ site anchor celestial");
    assert!(hq_site_pos.distance(hq_cel.position) > hq_cel.radius, "HQ site position must be safely beyond celestial radius");
    hq_sys.set_ball_position(710_001_005, hq_site_pos).await;
    for i in 0..4 {
        let boss_bid = with_pve_special_reg(&ctx, |reg| {
            reg.incursions[&target_cid].sites.iter().find(|s| s.dungeon_id == 5175).unwrap().boss_npc_ball_id
        });
        if i == 0 {
            assert_eq!(boss_bid, 1_896_000_000 + (target_cid as i64) * 100 + 5175, "Unique boss_npc_ball_id per constellation site");
        }
        assert!(hq_sys.get_ball_snapshot(boss_bid).await.unwrap().is_some(), "Boss NPC ball must exist in system");
        if i == 1 {
            ctx.db.with_mut_snapshot(|snap| {
                if let Some(ch) = snap.characters.get_mut(&90_000_002) {
                    ch.solar_system_id = hq_sys_id;
                    ch.structure_id = Some(1_000_000_001);
                }
            });
        }
        let prev_balance = ctx.db.with_snapshot(|s| s.characters.get(&90_000_001).map(|c| c.balance.0).unwrap_or(0));
        let prev_lp = ctx.db.with_snapshot(|s| s.characters.get(&90_000_001).and_then(|c| c.loyalty_points.get(&1_000_125).copied()).unwrap_or(0));

        let (_, destroyed, _) = hq_sys.apply_damage_with_attacker(boss_bid, DamageVector { em: 50_000.0, thermal: 50_000.0, kinetic: 50_000.0, explosive: 50_000.0 }, Some(90_000_001)).await.unwrap().unwrap();
        assert!(destroyed, "Boss NPC ball must be destroyed by lethal damage");

        let cur_balance = ctx.db.with_snapshot(|s| s.characters.get(&90_000_001).map(|c| c.balance.0).unwrap_or(0));
        let cur_lp = ctx.db.with_snapshot(|s| s.characters.get(&90_000_001).and_then(|c| c.loyalty_points.get(&1_000_125).copied()).unwrap_or(0));
        assert_eq!(cur_balance - prev_balance, 6_300_000 * 100, "HQ payout ISK cents per pilot");
        assert_eq!(cur_lp - prev_lp, 1_400, "HQ payout CONCORD LP per pilot");

        if i == 1 {
            let p2_lp = ctx.db.with_snapshot(|s| s.characters.get(&90_000_002).and_then(|c| c.loyalty_points.get(&1_000_125).copied()).unwrap_or(0));
            assert_eq!(p2_lp, 0, "Docked pilot 2 in structure must not receive incursion payout");
        }
    }

    // 3. Mothership (`INC-MS-5158`) unlocked; destroy boss via apply_damage and complete site
    let (ms_boss_bid, ms_site_pos) = with_pve_special_reg(&ctx, |reg| {
        let s = reg.incursions[&target_cid].sites.iter().find(|s| s.dungeon_id == 5158).unwrap();
        (s.boss_npc_ball_id, s.position)
    });
    hq_sys.set_ball_position(710_001_005, ms_site_pos).await;
    assert_eq!(ms_boss_bid, 1_896_000_000 + (target_cid as i64) * 100 + 5158);
    assert!(hq_sys.get_ball_snapshot(ms_boss_bid).await.unwrap().is_some(), "MS boss NPC ball must exist in system");

    let prev_balance = ctx.db.with_snapshot(|s| s.characters.get(&90_000_001).map(|c| c.balance.0).unwrap_or(0));
    let prev_lp = ctx.db.with_snapshot(|s| s.characters.get(&90_000_001).and_then(|c| c.loyalty_points.get(&1_000_125).copied()).unwrap_or(0));

    let (_, ms_destroyed, _) = hq_sys.apply_damage_with_attacker(ms_boss_bid, DamageVector { em: 50_000.0, thermal: 50_000.0, kinetic: 50_000.0, explosive: 50_000.0 }, Some(90_000_001)).await.unwrap().unwrap();
    assert!(ms_destroyed, "Mothership boss NPC must be destroyed by lethal damage");

    let cur_balance = ctx.db.with_snapshot(|s| s.characters.get(&90_000_001).map(|c| c.balance.0).unwrap_or(0));
    let cur_lp = ctx.db.with_snapshot(|s| s.characters.get(&90_000_001).and_then(|c| c.loyalty_points.get(&1_000_125).copied()).unwrap_or(0));
    assert_eq!(cur_balance - prev_balance, 12_600_000 * 100, "Mothership payout ISK cents per pilot");
    assert_eq!(cur_lp - prev_lp, 2_800, "Mothership payout CONCORD LP per pilot");

    let inc_ended = with_pve_special_reg(&ctx, |reg| {
        let inc = &reg.incursions[&target_cid];
        (inc.influence, inc.state.clone(), inc.mothership_defeated)
    });
    assert_eq!(inc_ended.0, 0.0);
    assert_eq!(inc_ended.1, "Ended");
    assert!(inc_ended.2);

    // Verify incursion penalties clear when incursion ends
    let eff_ended = ctx.dispatch_rpc(&mut sess, "incursion", None, "GetIncursionSystemEffects", &[PyRep::Integer(hq_sys_id as i64)], &kw, None).await.expect("GetIncursionSystemEffects ended");
    assert_eq!(eff_ended.dict_get("cynoJammed").and_then(|v| v.as_bool()), Some(false));
    assert_eq!(eff_ended.dict_get("influence").and_then(|v| v.as_f64()), Some(0.0));

    // Verify GetIncursionLPLog returns accumulated payout logs
    let lp_logs = ctx.dispatch_rpc(&mut sess, "incursion", None, "GetIncursionLPLog", &[], &kw, None).await.expect("GetIncursionLPLog");
    let PyRep::List(log_list) = &lp_logs else { panic!("Expected List of LP logs") };
    assert_eq!(log_list.len(), 5, "Must have 4 HQ logs + 1 MS log");
    let expected_ms_site_id = format!("INC-{target_cid}-5158");
    assert_eq!(log_list.last().unwrap().dict_get("siteID").and_then(|v| v.as_str()), Some(expected_ms_site_id.as_str()));

    let concord_lp = ctx.db.with_snapshot(|snap| {
        snap.characters.get(&90_000_001).and_then(|c| c.loyalty_points.get(&1_000_125).copied()).unwrap_or(0)
    });
    assert!(concord_lp >= 7_000, "CONCORD LP balance must accumulate payouts: {concord_lp}");

    // Verify UpdateIncursionLPLog is unimplemented (R4)
    let unimpl_err = ctx.dispatch_rpc(&mut sess, "incursion", None, "UpdateIncursionLPLog", &[PyRep::String("INC-MS-5158".into())], &kw, None).await.expect_err("UpdateIncursionLPLog should be unimplemented");
    assert_eq!(unimpl_err.code, "UnimplementedRPC");
}

// ============================================================================
// 4. Homefront Ops, Storms, Drifter Hives, Triglavian Sites & Interference
// ============================================================================
#[tokio::test]
async fn test_pve_homefront_storms_drifter_hives_triglavian_and_interference() {
    // parity: feat:pve.homefront-ops, feat:pve.metaliminal-storms, feat:pve.drifter-hives, feat:pve.interference, rpc:dungeonInstanceCacheMgr.GetHomefrontSiteInstances, rpc:dungeonInstanceCacheMgr.GetTriglavianSiteInstances, rpc:map.GetAllRoamingWeatherSystems, rpc:solarsystemInterferenceMgr.GetLocalInterferenceState
    eve_testkit::attest(&[
        "feat:pve.homefront-ops",
        "feat:pve.metaliminal-storms",
        "feat:pve.drifter-hives",
        "feat:pve.interference",
    ]);

    let ctx = ServerContext::new();
    ensure_special_sites_spawned(&ctx).await;
    let kw = HashMap::new();
    let mut sess = create_in_space_session(&ctx, 8107, 30_000_142, 710_001_007, 587).await;

    // 1. Homefront Operations
    let hf_initial = ctx.dispatch_rpc(&mut sess, "dungeonInstanceCacheMgr", None, "GetHomefrontSiteInstances", &[PyRep::Integer(30_000_142)], &kw, None).await.expect("GetHomefrontSiteInstances initial");
    let PyRep::List(hf_init_list) = &hf_initial else { panic!("Expected List") };
    assert_eq!(hf_init_list[0].dict_get("completed").and_then(|v| v.as_bool()), Some(false));
    let hf_boss_bid = hf_init_list[0].dict_get("bossNpcBallID").unwrap().as_i64().unwrap();

    let jita_sys = ctx.systems.get_or_spawn(30_000_142);
    let _ = jita_sys.apply_damage(hf_boss_bid, DamageVector { em: 50_000.0, thermal: 50_000.0, kinetic: 50_000.0, explosive: 50_000.0 }).await;
    ctx.dispatch_rpc(&mut sess, "keeper", None, "EnterNextDungeonRoom", &[PyRep::Integer(710_001), PyRep::Integer(5)], &kw, None).await.expect("Complete Homefront site 710_001");

    let hf_rep = ctx.dispatch_rpc(&mut sess, "dungeonInstanceCacheMgr", None, "GetHomefrontSiteInstances", &[PyRep::Integer(30_000_142)], &kw, None).await.expect("GetHomefrontSiteInstances after completion");
    let PyRep::List(hf_list) = &hf_rep else { panic!("Expected List") };
    assert_eq!(hf_list[0].dict_get("completed").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(hf_list[0].dict_get("rewardPaidIsk").and_then(|v| v.as_i64()), Some(17_230_000));

    // 2. Metaliminal Roaming Storms
    let storms_before = ctx.dispatch_rpc(&mut sess, "map", None, "GetAllRoamingWeatherSystems", &[], &kw, None).await.expect("GetAllRoamingWeatherSystems initial");
    let PyRep::List(sb_list) = &storms_before else { panic!("Expected List") };
    let center_before = sb_list[0].dict_get("centerSolarSystemID").and_then(|v| v.as_i64()).unwrap();

    advance_pve_special_clock(&ctx, 3_600);
    let storms_after = ctx.dispatch_rpc(&mut sess, "map", None, "GetAllRoamingWeatherSystems", &[], &kw, None).await.expect("GetAllRoamingWeatherSystems after clock step");
    let PyRep::List(sa_list) = &storms_after else { panic!("Expected List") };
    let center_after = sa_list[0].dict_get("centerSolarSystemID").and_then(|v| v.as_i64()).unwrap();
    assert_ne!(center_before, center_after, "Roaming Metaliminal storm center must move between connected stargate systems");

    // 3. Wormhole Drifter Hives
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) { ch.solar_system_id = 31_000_001; }
    });
    sess.solar_system_id = Some(31_000_001);
    let wh_sys = ctx.systems.get_or_spawn(31_000_001);
    let hive_bid = with_pve_special_reg(&ctx, |reg| reg.drifter_hives[&31_000_001].warden_npc_ball_id);
    let _ = wh_sys.apply_damage(hive_bid, DamageVector { em: 50_000.0, thermal: 50_000.0, kinetic: 50_000.0, explosive: 50_000.0 }).await;
    ctx.dispatch_rpc(&mut sess, "keeper", None, "EnterNextDungeonRoom", &[PyRep::Integer(31_000_001)], &kw, None).await.expect("Complete Barbican Drifter Hive");
    let hive_rep = ctx.dispatch_rpc(&mut sess, "dungeonInstanceCacheMgr", None, "GetTriglavianSiteInstances", &[PyRep::Integer(31_000_001)], &kw, None).await.expect("GetTriglavianSiteInstances for Drifter Hive");
    assert_eq!(hive_rep.dict_get("dungeonName").and_then(|v| v.as_str()), Some("Barbican Hive"));
    assert_eq!(hive_rep.dict_get("elementTypeID").and_then(|v| v.as_i64()), Some(34_557));
    assert_eq!(hive_rep.dict_get("cleared").and_then(|v| v.as_bool()), Some(true));

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

// ============================================================================
// 6. Incursion Site Wave Completion Triggered on Boss Death
// ============================================================================
#[tokio::test]
async fn test_pve_incursion_wave_completion_on_boss_death() {
    // parity: feat:pve.incursion-system, feat:pve.incursion-payout
    eve_testkit::attest(&["feat:pve.incursion-system", "feat:pve.incursion-payout"]);

    let ctx = ServerContext::new();
    ensure_special_sites_spawned(&ctx).await;

    let (target_cid, vg_sys_id) = with_pve_special_reg(&ctx, |reg| {
        let (cid, inc) = reg.incursions.iter().next().unwrap();
        (*cid, inc.vanguard_solar_system_ids[0])
    });

    let _sess = create_in_space_session(&ctx, 8105, vg_sys_id, 710_001_006, 587).await;

    let (boss_bid, site_id, initial_inf, site_pos) = with_pve_special_reg(&ctx, |reg| {
        let inc = &reg.incursions[&target_cid];
        let site = inc.sites.iter().find(|s| s.tier == "Vanguard").unwrap();
        (site.boss_npc_ball_id, site.site_id.clone(), inc.influence, site.position)
    });

    let vg_sys = ctx.systems.get_or_spawn(vg_sys_id);
    vg_sys.set_ball_position(710_001_006, site_pos).await;
    assert!(vg_sys.get_ball_snapshot(boss_bid).await.unwrap().is_some(), "Vanguard boss ball must exist");

    // Setup fleet 940_001 and 4 pilots (P1 killer & P2 on-grid in fleet, P3 off-grid in fleet, P4 on-grid unaffiliated)
    ctx.db.with_mut_snapshot(|snap| {
        let p1_template = snap.characters[&90_000_001].clone();
        for (pid, fid, ship_id) in [
            (90_000_001, Some(940_001), 710_001_006),
            (90_000_002, Some(940_001), 710_001_008),
            (90_000_003, Some(940_001), 710_001_009),
            (90_000_004, None, 710_001_010),
        ] {
            let ch = snap.characters.entry(pid).or_insert_with(|| p1_template.clone());
            ch.character_id = pid; ch.solar_system_id = vg_sys_id; ch.station_id = None; ch.structure_id = None;
            ch.fleet_id = fid; ch.active_ship_id = ship_id;
        }
        snap.fleets.insert(940_001, FleetRecord {
            fleet_id: 940_001, boss_id: 90_000_001, members: vec![90_000_001, 90_000_002, 90_000_003], ..Default::default()
        });
    });

    for (bid, offset) in [
        (710_001_008, DVec3::new(5_000.0, 0.0, 0.0)),
        (710_001_009, DVec3::new(1_000_000.0, 0.0, 0.0)),
        (710_001_010, DVec3::new(10_000.0, 0.0, 0.0)),
    ] {
        let _ = vg_sys.spawn_ball(eve_server::services::pve_incursion_sites::make_site_npc_ball(bid, 587, site_pos + offset)).await;
    }

    let p_init: Vec<(i64, i64)> = [90_000_001, 90_000_002, 90_000_003, 90_000_004].into_iter().map(|pid| {
        ctx.db.with_snapshot(|s| {
            let ch = &s.characters[&pid];
            (ch.balance.0, ch.loyalty_points.get(&1_000_125).copied().unwrap_or(0))
        })
    }).collect();

    // NF-INC-CONSUME-BEFORE-CHECK: NPCs alive returns error without clearing boss_destroyed
    with_pve_special_reg(&ctx, |r| { r.incursions.get_mut(&target_cid).unwrap().sites.iter_mut().find(|s| s.site_id == site_id).unwrap().boss_destroyed = true; });
    let err_npc = eve_server::services::pve_incursion_sites::process_incursion_site_completion_and_payout(&ctx, Some(90_000_004), None, vg_sys_id, &site_id, Some(&[90_000_003])).await.unwrap_err();
    assert_eq!(err_npc.code, "IncursionSiteNpcsRemaining");
    assert!(with_pve_special_reg(&ctx, |r| r.incursions[&target_cid].sites.iter().find(|s| s.site_id == site_id).unwrap().boss_destroyed));
    with_pve_special_reg(&ctx, |r| { r.incursions.get_mut(&target_cid).unwrap().sites.iter_mut().find(|s| s.site_id == site_id).unwrap().boss_destroyed = false; });

    // NF-INC-UNATTRIB-STALL: unattributed boss death (killer = None) cleanly completes/respawns wave with 0 payout
    let (_, unattr_dead, _) = vg_sys.apply_damage_with_attacker(boss_bid, DamageVector { em: 50_000.0, thermal: 50_000.0, kinetic: 50_000.0, explosive: 50_000.0 }, None).await.unwrap().unwrap();
    assert!(unattr_dead, "Boss NPC ball must be destroyed by unattributed damage");
    let (count1, inf1, new_boss_bid) = with_pve_special_reg(&ctx, |r| {
        let inc = &r.incursions[&target_cid];
        let site = inc.sites.iter().find(|s| s.site_id == site_id).unwrap();
        (site.completed_count, inc.influence, site.boss_npc_ball_id)
    });
    assert_eq!(count1, 1, "Wave must advance on unattributed death");
    assert!(inf1 < initial_inf, "Influence must drop");
    assert!(vg_sys.get_ball_snapshot(new_boss_bid).await.unwrap().is_some(), "Next wave boss ball must spawn");
    for (i, pid) in [90_000_001, 90_000_002, 90_000_003, 90_000_004].into_iter().enumerate() {
        let bal = ctx.db.with_snapshot(|s| s.characters[&pid].balance.0);
        let lp = ctx.db.with_snapshot(|s| s.characters[&pid].loyalty_points.get(&1_000_125).copied().unwrap_or(0));
        assert_eq!((bal, lp), p_init[i], "Unattributed boss death must pay 0 ISK/LP");
    }

    // NF-INC-CONSUME-BEFORE-CHECK: 0 eligible pilots returns error without clearing boss_destroyed
    let _ = vg_sys.remove_ball(new_boss_bid).await;
    with_pve_special_reg(&ctx, |r| { r.incursions.get_mut(&target_cid).unwrap().sites.iter_mut().find(|s| s.site_id == site_id).unwrap().boss_destroyed = true; });
    let err_pilots = eve_server::services::pve_incursion_sites::process_incursion_site_completion_and_payout(&ctx, Some(90_000_004), None, vg_sys_id, &site_id, Some(&[90_000_003])).await.unwrap_err();
    assert_eq!(err_pilots.code, "NoEligiblePilotsOnGrid");
    assert!(with_pve_special_reg(&ctx, |r| r.incursions[&target_cid].sites.iter().find(|s| s.site_id == site_id).unwrap().boss_destroyed));

    // NF-INC-ATTRIB-01 & NF-REV-FLEET-TEST: kill boss via apply_damage_with_attacker with attributed killer P1; assert fleet receives payout
    let _ = vg_sys.spawn_ball(eve_server::services::pve_incursion_sites::make_site_npc_ball(new_boss_bid, 10_962, site_pos)).await;
    with_pve_special_reg(&ctx, |r| { r.incursions.get_mut(&target_cid).unwrap().sites.iter_mut().find(|s| s.site_id == site_id).unwrap().boss_destroyed = false; });
    let (_, p1_killed, _) = vg_sys.apply_damage_with_attacker(new_boss_bid, DamageVector { em: 50_000.0, thermal: 50_000.0, kinetic: 50_000.0, explosive: 50_000.0 }, Some(90_000_001)).await.unwrap().unwrap();
    assert!(p1_killed, "Boss killed via apply_damage_with_attacker with attributed killer P1");
    let count2 = with_pve_special_reg(&ctx, |r| r.incursions[&target_cid].sites.iter().find(|s| s.site_id == site_id).unwrap().completed_count);
    assert_eq!(count2, 2);

    let p_final: Vec<(i64, i64)> = [90_000_001, 90_000_002, 90_000_003, 90_000_004].into_iter().map(|pid| {
        ctx.db.with_snapshot(|s| (s.characters[&pid].balance.0, s.characters[&pid].loyalty_points.get(&1_000_125).copied().unwrap_or(0)))
    }).collect();

    assert_eq!(p_final[0].0 - p_init[0].0, 4_158_000 * 100, "P1 killer receives exact Vanguard payout");
    assert_eq!(p_final[0].1 - p_init[0].1, 560, "P1 killer receives exact Vanguard LP payout");
    assert_eq!(p_final[1].0 - p_init[1].0, 4_158_000 * 100, "P2 fleet-mate receives exact Vanguard payout");
    assert_eq!(p_final[1].1 - p_init[1].1, 560, "P2 fleet-mate receives exact Vanguard LP payout");
    assert_eq!((p_final[2].0, p_final[2].1), (p_init[2].0, p_init[2].1), "P3 off-grid receives zero");
    assert_eq!((p_final[3].0, p_final[3].1), (p_init[3].0, p_init[3].1), "P4 unaffiliated receives zero");
}

// ============================================================================
// 7. Incursion Site Double Payout Race Remediation (NF-INC-RACE-01)
// ============================================================================
#[tokio::test]
async fn test_pve_incursion_concurrent_completion_single_payout() {
    // parity: feat:pve.incursion-system, feat:pve.incursion-payout
    eve_testkit::attest(&["feat:pve.incursion-system", "feat:pve.incursion-payout"]);

    let ctx = ServerContext::new();
    ensure_special_sites_spawned(&ctx).await;

    let (target_cid, vg_sys_id) = with_pve_special_reg(&ctx, |reg| {
        let (cid, inc) = reg.incursions.iter().next().unwrap();
        (*cid, inc.vanguard_solar_system_ids[0])
    });

    let _sess = create_in_space_session(&ctx, 8106, vg_sys_id, 710_001_007, 587).await;

    let (boss_bid, site_id, site_pos) = with_pve_special_reg(&ctx, |reg| {
        let inc = &reg.incursions[&target_cid];
        let site = inc.sites.iter().find(|s| s.tier == "Vanguard").unwrap();
        (site.boss_npc_ball_id, site.site_id.clone(), site.position)
    });

    let vg_sys = ctx.systems.get_or_spawn(vg_sys_id);
    vg_sys.set_ball_position(710_001_007, site_pos).await;
    let _ = vg_sys.remove_ball(boss_bid).await;
    with_pve_special_reg(&ctx, |reg| {
        let inc = reg.incursions.get_mut(&target_cid).unwrap();
        let site = inc.sites.iter_mut().find(|s| s.site_id == site_id).unwrap();
        site.boss_destroyed = true;
    });

    let char_id = 90_000_001;
    let initial_balance = ctx.db.with_snapshot(|s| s.characters.get(&char_id).map(|c| c.balance.0).unwrap_or(0));

    let (res1, res2) = tokio::join!(
        eve_server::services::pve_incursion_sites::process_incursion_site_completion_and_payout(
            &ctx, Some(char_id), None, vg_sys_id, &site_id, None,
        ),
        eve_server::services::pve_incursion_sites::process_incursion_site_completion_and_payout(
            &ctx, Some(char_id), None, vg_sys_id, &site_id, None,
        ),
    );

    let successes = [res1.is_ok(), res2.is_ok()].into_iter().filter(|&x| x).count();
    let failures = [res1.is_err(), res2.is_err()].into_iter().filter(|&x| x).count();
    assert_eq!(successes, 1, "Exactly one concurrent attempt must succeed");
    assert_eq!(failures, 1, "Duplicate concurrent attempt must fail");

    let err = res1.err().or(res2.err()).unwrap();
    assert_eq!(err.code, "IncursionSiteNpcsRemaining");

    let completed_count = with_pve_special_reg(&ctx, |reg| {
        reg.incursions[&target_cid].sites.iter().find(|s| s.site_id == site_id).unwrap().completed_count
    });
    assert_eq!(completed_count, 1, "Site completed_count must only be incremented once");

    let final_balance = ctx.db.with_snapshot(|s| s.characters.get(&char_id).map(|c| c.balance.0).unwrap_or(0));
    assert!(final_balance > initial_balance, "Pilot must receive exactly one payout");
}
