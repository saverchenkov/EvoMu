//! Complete Sansha Incursion System, Payouts & Mothership Finale Integration Tests.

use eve_db::{FleetRecord, InventoryFlag, InventoryItemRecord, IskCents};
use eve_destiny::{Ball, DamageVector, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::pve_incursion_sites::{
    get_incursion_effects_for_system, on_incursion_boss_destroyed,
};
use eve_server::services::pve_special_sites::{
    ensure_special_sites_spawned, with_pve_special_reg,
};
use glam::DVec3;
use std::collections::HashMap;

async fn create_in_space_session(
    ctx: &ServerContext,
    session_id: u64,
    char_id: i64,
    solar_system_id: u32,
    ship_id: i64,
    ship_type_id: u32,
) -> SessionState {
    let kw = HashMap::new();
    let mut sess = SessionState::new(session_id);
    ctx.dispatch_rpc(&mut sess, "authentication", None, "Login", &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())], &kw, None).await.expect("Login");
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None).await.expect("SelectCharacterID");

    sess.solar_system_id = Some(solar_system_id);
    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = Some(ship_id);

    ctx.db.with_mut_snapshot(|snap| {
        let p1_template = snap.characters[&90_000_001].clone();
        let ch = snap.characters.entry(char_id).or_insert_with(|| p1_template);
        ch.character_id = char_id;
        ch.station_id = None;
        ch.structure_id = None;
        ch.solar_system_id = solar_system_id;
        ch.active_ship_id = ship_id;

        snap.items.insert(ship_id, InventoryItemRecord {
            item_id: ship_id,
            type_id: ship_type_id,
            owner_id: char_id,
            location_id: solar_system_id as i64,
            flag: InventoryFlag::Hangar,
            quantity: 1,
            singleton: true,
            ..Default::default()
        });
    });

    let sys_handle = ctx.systems.get_or_spawn(solar_system_id);
    let _ = sys_handle.spawn_ball(Ball {
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
    }).await;
    sess
}

// ============================================================================
// 1. Constellation System Effects, Encounters & taleMgr World Event RPCs
// ============================================================================
#[tokio::test]
async fn test_pve_incursion_constellation_system_effects_and_tale_mgr() {
    // parity: rpc:taleMgr.GetGlobalWorldEventTales, rpc:taleMgr.get_active_tales_by_template
    eve_testkit::attest(&[
        "rpc:taleMgr.GetGlobalWorldEventTales",
        "rpc:taleMgr.get_active_tales_by_template",
    ]);

    let ctx = ServerContext::new();
    ensure_special_sites_spawned(&ctx).await;
    let kw = HashMap::new();

    let (target_cid, hq_sys_id, staging_sys_id) = with_pve_special_reg(&ctx, |reg| {
        let (cid, inc) = reg.incursions.iter().next().unwrap();
        (*cid, inc.hq_solar_system_id, inc.staging_solar_system_id)
    });
    let mut sess = create_in_space_session(&ctx, 8105, 90_000_001, hq_sys_id, 710_001_005, 587).await;

    // 1. V24 World Event RPCs via taleMgr
    let tales_rep = ctx
        .dispatch_rpc(&mut sess, "taleMgr", None, "GetGlobalWorldEventTales", &[], &kw, None)
        .await
        .expect("GetGlobalWorldEventTales");
    let PyRep::List(tales) = tales_rep else { panic!("Expected List of tales") };
    assert!(!tales.is_empty(), "Active incursion tales must be returned");

    let tale = tales
        .iter()
        .find(|t| t.dict_get("constellationID").and_then(|v| v.as_i64()) == Some(target_cid as i64))
        .expect("Target incursion tale must exist");
    assert_eq!(tale.dict_get("templateID").and_then(|v| v.as_i64()), Some(5158));
    assert_eq!(tale.dict_get("influence").and_then(|v| v.as_f64()), Some(1.0));
    assert_eq!(tale.dict_get("state").and_then(|v| v.as_str()), Some("Establishing"));
    assert_eq!(tale.dict_get("managerSolarSystemID").and_then(|v| v.as_i64()), Some(staging_sys_id as i64));
    assert_eq!(tale.dict_get("hasFinalEncounter").and_then(|v| v.as_bool()), Some(false));

    // Verify sceneTypesByLocationID mapping has staging, vanguard, assault, and HQ
    let PyRep::Dict(scene_map) = tale.dict_get("sceneTypesByLocationID").expect("sceneTypesByLocationID") else { panic!("Expected Dict") };
    assert!(scene_map.iter().any(|(k, v)| k.as_i64() == Some(staging_sys_id as i64) && v.as_str() == Some("incursionStaging")));
    assert!(scene_map.iter().any(|(k, v)| k.as_i64() == Some(hq_sys_id as i64) && v.as_str() == Some("incursionHeavyInfestation")));

    // Verify sites list in tale contains staging, vanguard, assault, headquarters, mothership
    let PyRep::List(sites) = tale.dict_get("sites").expect("sites list") else { panic!("Expected List of sites") };
    assert!(sites.iter().any(|s| s.dict_get("tier").and_then(|v| v.as_str()) == Some("Staging")));
    assert!(sites.iter().any(|s| s.dict_get("tier").and_then(|v| v.as_str()) == Some("Vanguard")));
    assert!(sites.iter().any(|s| s.dict_get("tier").and_then(|v| v.as_str()) == Some("Assault")));
    assert!(sites.iter().any(|s| s.dict_get("tier").and_then(|v| v.as_str()) == Some("Headquarters")));
    let ms_site = sites.iter().find(|s| s.dict_get("tier").and_then(|v| v.as_str()) == Some("Mothership")).expect("Mothership site");
    assert_eq!(ms_site.dict_get("available").and_then(|v| v.as_bool()), Some(false), "Mothership must be locked at 1.0 influence");

    // 2. taleMgr.get_active_tales_by_template
    let active_tales = ctx
        .dispatch_rpc(&mut sess, "taleMgr", None, "get_active_tales_by_template", &[PyRep::Integer(5158)], &kw, None)
        .await
        .expect("get_active_tales_by_template");
    let PyRep::List(active_list) = active_tales else { panic!("Expected List") };
    assert!(!active_list.is_empty(), "Active tales list by template must not be empty");

    // 3. Sansha Encounter Compositions & Site Distribution in Constellation
    with_pve_special_reg(&ctx, |reg| {
        let inc = &reg.incursions[&target_cid];
        assert_eq!(inc.sites.len(), 8, "Constellation must seed staging, 2 vanguard, 2 assault, 2 HQ, and mothership");
        for site in &inc.sites {
            assert!(site.boss_type_id > 0, "Site {} must have authentic Sansha boss type ID", site.site_id);
            assert!(!site.escort_spawns.is_empty(), "Site {} must have Sansha escort spawns", site.site_id);
            for esc in &site.escort_spawns {
                assert!(esc.type_id > 0, "Escort spawn must have authentic Sansha type ID");
                assert_ne!(esc.npc_ball_id, site.boss_npc_ball_id);
            }
        }
    });

    // 4. Incursion System Effects & Dogma Attributes
    let eff = get_incursion_effects_for_system(&ctx, hq_sys_id).expect("incursion effects");
    assert_eq!(eff.influence, 1.0);
    assert!(eff.cyno_jammed);
    assert_eq!(eff.shield_penalty, 0.50);
    assert_eq!(eff.damage_penalty, 0.50);

    let mut ship_attrs = HashMap::from([(263, 1000.0), (265, 1000.0), (482, 1000.0), (64, 1.0)]);
    eve_server::services::inventory_fitting::fitting_scanners_superweapons::apply_active_dbuffs_and_system_effects_to_ship_attrs(&ctx, 710_001_005, &mut ship_attrs);
    assert_eq!(ship_attrs[&263], 500.0, "50% shield penalty at 1.0 influence");
    assert_eq!(ship_attrs[&265], 500.0, "50% armor penalty at 1.0 influence");
    assert_eq!(ship_attrs[&482], 500.0, "50% capacitor penalty at 1.0 influence");
    assert_eq!(ship_attrs[&64], 0.5, "50% damage penalty at 1.0 influence");

    // 5. Incursion client RPC contracts
    let lp_log = ctx
        .dispatch_rpc(&mut sess, "incursion", None, "GetIncursionLPLog", &[], &kw, None)
        .await
        .expect("GetIncursionLPLog");
    assert_eq!(lp_log, PyRep::List(vec![]));

    let update_lp = ctx
        .dispatch_rpc(&mut sess, "incursion", None, "UpdateIncursionLPLog", &[PyRep::String("INC-MS-5158".into())], &kw, None)
        .await
        .expect("UpdateIncursionLPLog");
    assert_eq!(update_lp, PyRep::None);

    // Invented non-client RPCs must return UnimplementedRPC
    for non_client_rpc in ["GetIncursionSystemEffects", "GetAllIncursionSites", "GetActiveIncursionData", "GetAllIncursions"] {
        let err = ctx
            .dispatch_rpc(&mut sess, "incursion", None, non_client_rpc, &[PyRep::Integer(hq_sys_id as i64)], &kw, None)
            .await
            .expect_err(&format!("{non_client_rpc} must be rejected as non-client RPC"));
        assert_eq!(err.code, "UnimplementedRPC", "{non_client_rpc} should not be registered");
    }
}

// ============================================================================
// 2. Site Payouts Scaled by Fleet Size & Constellation Influence
// ============================================================================
#[tokio::test]
async fn test_pve_incursion_payout_fleet_and_influence_scaling() {
    let ctx = ServerContext::new();
    ensure_special_sites_spawned(&ctx).await;
    let kw = HashMap::new();

    let (target_cid, vg_sys_id) = with_pve_special_reg(&ctx, |reg| {
        let (cid, inc) = reg.incursions.iter().next().unwrap();
        (*cid, inc.vanguard_solar_system_ids[0])
    });

    let (boss_bid, site_id, site_pos) = with_pve_special_reg(&ctx, |reg| {
        let inc = &reg.incursions[&target_cid];
        let site = inc.sites.iter().find(|s| s.tier == "Vanguard").unwrap();
        (site.boss_npc_ball_id, site.site_id.clone(), site.position)
    });

    let vg_sys = ctx.systems.get_or_spawn(vg_sys_id);
    let mut sess = create_in_space_session(&ctx, 8106, 90_000_001, vg_sys_id, 710_001_006, 587).await;
    vg_sys.set_ball_position(710_001_006, site_pos).await;

    // Setup 5-pilot fleet 940_001 (optimal fleet size for Vanguard: 5 pilots)
    let pids = [90_000_001, 90_000_002, 90_000_003, 90_000_004, 90_000_005];
    ctx.db.with_mut_snapshot(|snap| {
        let p1_template = snap.characters[&90_000_001].clone();
        for &pid in &pids {
            let ch = snap.characters.entry(pid).or_insert_with(|| p1_template.clone());
            ch.character_id = pid;
            ch.solar_system_id = vg_sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.fleet_id = Some(940_001);
            ch.active_ship_id = 710_000_000 + pid;
        }
        snap.fleets.insert(940_001, FleetRecord {
            fleet_id: 940_001,
            boss_id: 90_000_001,
            members: pids.to_vec(),
            ..Default::default()
        });
    });

    // Spawn ships for on-grid pilots
    for &pid in &pids {
        let ship_id = 710_000_000 + pid;
        let _ = vg_sys.spawn_ball(Ball {
            id: ship_id,
            type_id: 587,
            position: site_pos + DVec3::new((pid % 500) as f64, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 365.0,
            inertia: 1.0,
            mass: 1_067_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        }).await;
    }

    let p_inits: HashMap<i64, (i64, i64)> = pids
        .iter()
        .map(|&pid| {
            (
                pid,
                (
                    ctx.db.with_snapshot(|s| s.characters[&pid].balance.0),
                    ctx.db
                        .with_snapshot(|s| s.characters[&pid].loyalty_points.get(&1_000_125).copied().unwrap_or(0)),
                ),
            )
        })
        .collect();

    // Kill boss with attributed damage from pilot 90_000_001 after clearing escort waves
    // Server-side death hook automatically handles site wave completion & payout without keeper RPC
    crate::common::clear_incursion_site_escort_waves(&ctx, vg_sys_id, &site_id).await;
    let (_, dead, _) = vg_sys.apply_damage_with_attacker(
        boss_bid,
        DamageVector { em: 100_000.0, thermal: 100_000.0, kinetic: 100_000.0, explosive: 100_000.0 },
        Some(90_000_001),
    ).await.unwrap().unwrap();
    assert!(dead, "Boss ball must be destroyed");

    // Influence was 1.0; delta for Vanguard is 0.05 -> new influence is 0.95
    // influence_scale = (1.0 - 0.95 * 0.50) = 0.525.
    // 5 pilots in 5-pilot Vanguard -> fleet_scale = 1.0. Total scale = 0.525.
    // base_isk = 10,395,000 -> isk = round(10,395,000 * 0.525) = 5,457,375 ISK.
    // base_lp = 1,400 -> lp = round(1,400 * 0.525) = 735 LP.
    let p_after_isk = ctx.db.with_snapshot(|s| s.characters[&90_000_001].balance.0);
    let p_after_lp = ctx.db.with_snapshot(|s| s.characters[&90_000_001].loyalty_points.get(&1_000_125).copied().unwrap_or(0));
    let isk_diff = p_after_isk - p_inits[&90_000_001].0;
    let lp_diff = p_after_lp - p_inits[&90_000_001].1;

    assert_eq!(isk_diff, 5_457_375 * 100, "ISK payout must scale with constellation influence (5,457,375 ISK)");
    assert_eq!(lp_diff, 735, "CONCORD LP payout must scale with constellation influence (735 LP)");

    // Verify all 5 on-grid fleet members received the payout
    for &pid in &pids {
        let (bal, lp) = ctx.db.with_snapshot(|s| (s.characters[&pid].balance.0, s.characters[&pid].loyalty_points.get(&1_000_125).copied().unwrap_or(0)));
        assert_eq!(bal - p_inits[&pid].0, 5_457_375 * 100, "All fleet members in site must receive identical payout");
        assert_eq!(lp - p_inits[&pid].1, 735, "All fleet members in site must receive identical LP");
    }

    // Verify Wallet Journal entry
    let journal = ctx.db.with_snapshot(|s| s.wallet_journal.clone());
    assert!(journal.iter().any(|j| j.character_id == 90_000_001 && j.entry_type == "CorporateRewardPayout" && j.amount == IskCents::from_isk(5_457_375)));

    // Verify Incursion LP Log
    let logs_rep = ctx.dispatch_rpc(&mut sess, "incursion", None, "GetIncursionLPLog", &[], &kw, None).await.expect("GetIncursionLPLog");
    let PyRep::List(logs) = logs_rep else { panic!("Expected List") };
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0].dict_get("siteID").and_then(|v| v.as_str()), Some(site_id.as_str()));
    assert_eq!(logs[0].dict_get("iskEarned").and_then(|v| v.as_i64()), Some(5_457_375));
    assert_eq!(logs[0].dict_get("concordLpEarned").and_then(|v| v.as_i64()), Some(735));
    assert_eq!(logs[0].dict_get("fleetSize").and_then(|v| v.as_i64()), Some(5));

    // Next wave boss must have cleanly respawned once escort waves clear
    crate::common::clear_incursion_site_escort_waves(&ctx, vg_sys_id, &site_id).await;
    let (next_boss_bid, wave_count) = with_pve_special_reg(&ctx, |reg| {
        let s = reg.incursions[&target_cid].sites.iter().find(|s| s.site_id == site_id).unwrap();
        (s.boss_npc_ball_id, s.completed_count)
    });
    assert_eq!(wave_count, 1);
    assert!(vg_sys.get_ball_snapshot(next_boss_bid).await.unwrap().is_some(), "Next wave boss must exist in space");

    // Overstack fleet testing: 11 pilots in Vanguard (1 over max_optimal_pilots=10)
    // over_scale = 1.0 - 1 * 0.12 = 0.88; influence_scale = 1.0 - 0.90 * 0.50 = 0.55
    // total_scale = 0.88 * 0.55 = 0.484 -> 5,031,180 ISK (vs 5,717,250 at full scale)
    let overstack_pids: Vec<i64> = (90_000_001..=90_000_011).collect();
    ctx.db.with_mut_snapshot(|snap| {
        let template = snap.characters[&90_000_001].clone();
        for &pid in &overstack_pids {
            let ch = snap.characters.entry(pid).or_insert_with(|| template.clone());
            ch.character_id = pid; ch.solar_system_id = vg_sys_id; ch.fleet_id = Some(940_002);
            ch.active_ship_id = 720_000_000 + pid;
        }
        snap.fleets.insert(940_002, FleetRecord {
            fleet_id: 940_002, boss_id: 90_000_001, members: overstack_pids.clone(), ..Default::default()
        });
    });
    for &pid in &overstack_pids {
        let _ = vg_sys.spawn_ball(Ball {
            id: 720_000_000 + pid, type_id: 587, position: site_pos + DVec3::new((pid % 500) as f64, 0.0, 0.0),
            velocity: DVec3::ZERO, max_velocity: 365.0, inertia: 1.0, mass: 1_067_000.0, radius: 40.0,
            speed_fraction: 0.0, mode: MovementMode::Stop,
        }).await;
    }

    let p_pre_overstack = ctx.db.with_snapshot(|s| s.characters[&90_000_001].balance.0);
    let (_, dead2, _) = vg_sys.apply_damage_with_attacker(
        next_boss_bid,
        DamageVector { em: 100_000.0, thermal: 100_000.0, kinetic: 100_000.0, explosive: 100_000.0 },
        Some(90_000_001),
    ).await.unwrap().unwrap();
    assert!(dead2);
    let p_post_overstack = ctx.db.with_snapshot(|s| s.characters[&90_000_001].balance.0);
    let overstack_isk_earned = (p_post_overstack - p_pre_overstack) / 100;
    assert_eq!(overstack_isk_earned, 5_031_180, "11 pilots (1 over optimal 10) experiences 12% overstack decay: 5,031,180 ISK");
}

// ============================================================================
// 3. Kundalini Manifest Mothership Finale & Incursion Conclusion
// ============================================================================
#[tokio::test]
async fn test_pve_incursion_mothership_multiwave_and_conclusion() {
    let ctx = ServerContext::new();
    ensure_special_sites_spawned(&ctx).await;

    let (target_cid, hq_sys_id) = with_pve_special_reg(&ctx, |reg| {
        let (cid, inc) = reg.incursions.iter().next().unwrap();
        (*cid, inc.hq_solar_system_id)
    });
    let _sess = create_in_space_session(&ctx, 8107, 90_000_001, hq_sys_id, 710_001_005, 587).await;
    let hq_sys = ctx.systems.get_or_spawn(hq_sys_id);

    // Initial check: Mothership site is locked at 1.0 influence
    let (ms_boss_bid_init, ms_pos) = with_pve_special_reg(&ctx, |reg| {
        let s = reg.incursions[&target_cid].sites.iter().find(|s| s.tier == "Mothership").unwrap();
        (s.boss_npc_ball_id, s.position)
    });
    assert!(hq_sys.get_ball_snapshot(ms_boss_bid_init).await.unwrap().is_none(), "Mothership ball must NOT spawn while locked");

    let lock_attempt = on_incursion_boss_destroyed(&ctx, hq_sys_id, ms_boss_bid_init, Some(90_000_001)).await;
    assert_eq!(lock_attempt.unwrap().unwrap_err().code, "MothershipSiteLocked");

    // Complete HQ site waves to bring influence from 1.0 down to 0.0
    let (hq_site_id, hq_pos) = with_pve_special_reg(&ctx, |reg| {
        let s = reg.incursions[&target_cid].sites.iter().find(|s| s.tier == "Headquarters").unwrap();
        (s.site_id.clone(), s.position)
    });
    hq_sys.set_ball_position(710_001_005, hq_pos).await;

    // Each HQ completion drops influence by delta (0.25)
    // 4 completions drop 1.0 -> 0.0
    for wave in 0..4 {
        let hq_boss_bid = crate::common::clear_incursion_site_escort_waves(&ctx, hq_sys_id, &hq_site_id).await;
        assert!(hq_sys.get_ball_snapshot(hq_boss_bid).await.unwrap().is_some());
        let (_, dead, _) = hq_sys.apply_damage_with_attacker(
            hq_boss_bid,
            DamageVector { em: 100_000.0, thermal: 100_000.0, kinetic: 100_000.0, explosive: 100_000.0 },
            Some(90_000_001),
        ).await.unwrap().unwrap();
        assert!(dead, "HQ boss killed for wave {wave}");
    }

    // Now influence is 0.0; Mothership unlocks and spawns!
    let (inf_after_hq, ms_spawned, ms_site_id, ms_boss_bid) = with_pve_special_reg(&ctx, |reg| {
        let inc = &reg.incursions[&target_cid];
        let ms = inc.sites.iter().find(|s| s.tier == "Mothership").unwrap();
        (inc.influence, inc.mothership_spawned, ms.site_id.clone(), ms.boss_npc_ball_id)
    });
    assert_eq!(inf_after_hq, 0.0, "Constellation influence must reach 0.0");
    assert!(ms_spawned, "Mothership encounter must unlock when influence reaches 0.0");

    // Verify The Kundalini Manifest ball exists in space with type_id 2192!
    let ms_ball_snap = hq_sys.get_ball_snapshot(ms_boss_bid).await.unwrap().expect("Kundalini Manifest ball must be spawned");
    assert_eq!(ms_ball_snap.1.type_id, 2192, "Mothership boss must be The Kundalini Manifest (type 2192)");

    // Verify escort fleet spawned in space
    let escort_bids = with_pve_special_reg(&ctx, |reg| {
        reg.incursions[&target_cid].sites.iter().find(|s| s.tier == "Mothership").unwrap()
            .escort_spawns.iter().map(|e| (e.npc_ball_id, e.type_id)).collect::<Vec<_>>()
    });
    assert!(!escort_bids.is_empty(), "Mothership escort fleet must be configured");
    for (e_bid, e_tid) in &escort_bids {
        let e_snap = hq_sys.get_ball_snapshot(*e_bid).await.unwrap().expect("Escort ball must exist in space");
        assert_eq!(e_snap.1.type_id, *e_tid, "Escort ball type ID must match authentic Sansha type");
    }

    // Move player ship to Mothership site grid, clear escorts, and kill The Kundalini Manifest
    crate::common::clear_incursion_site_escort_waves(&ctx, hq_sys_id, &ms_site_id).await;
    hq_sys.set_ball_position(710_001_005, ms_pos).await;
    let pre_ms_bal = ctx.db.with_snapshot(|s| s.characters[&90_000_001].balance.0);
    let pre_ms_lp = ctx.db.with_snapshot(|s| s.characters[&90_000_001].loyalty_points.get(&1_000_125).copied().unwrap_or(0));

    let (_, ms_dead, _) = hq_sys.apply_damage_with_attacker(
        ms_boss_bid,
        DamageVector { em: 100_000.0, thermal: 100_000.0, kinetic: 100_000.0, explosive: 100_000.0 },
        Some(90_000_001),
    ).await.unwrap().unwrap();
    assert!(ms_dead, "The Kundalini Manifest must be destroyed");

    // Verify incursion conclusion state (enters Withdrawing on a timer after mothership dies)
    let (final_inf, final_state, ms_defeated) = with_pve_special_reg(&ctx, |reg| {
        let inc = &reg.incursions[&target_cid];
        (inc.influence, inc.state.clone(), inc.mothership_defeated)
    });
    assert_eq!(final_inf, 0.0);
    assert_eq!(final_state, "Withdrawing", "Incursion state must be Withdrawing upon defeating mothership");
    assert!(ms_defeated, "mothership_defeated must be true");

    // Verify escorts cleared from space
    for (e_bid, _) in escort_bids {
        assert!(hq_sys.get_ball_snapshot(e_bid).await.unwrap().is_none(), "Escort ball {e_bid} must be removed from space");
    }

    // Verify Mothership payout disbursed (63,000,000 ISK base at 0.0 influence * 0.20 solo fleet scale = 12,600,000 ISK, 2,800 LP)
    let post_ms_bal = ctx.db.with_snapshot(|s| s.characters[&90_000_001].balance.0);
    let post_ms_lp = ctx.db.with_snapshot(|s| s.characters[&90_000_001].loyalty_points.get(&1_000_125).copied().unwrap_or(0));
    assert_eq!((post_ms_bal - pre_ms_bal) / 100, 12_600_000, "Mothership payout ISK must be 12,600,000 ISK scaled for solo pilot");
    assert_eq!(post_ms_lp - pre_ms_lp, 2_800, "Mothership payout CONCORD LP must be 2,800 LP scaled for solo pilot");

    // Verify incursion penalties withdrew from system
    let eff_ended = get_incursion_effects_for_system(&ctx, hq_sys_id);
    assert!(eff_ended.is_none(), "Incursion system effects must withdraw when incursion ends");
}

// ============================================================================
// 4. Incursion Concurrency Single Payout & AoE Progression
// ============================================================================
#[tokio::test]
async fn test_pve_incursion_concurrency_and_aoe_progression() {
    let ctx = ServerContext::new();
    ensure_special_sites_spawned(&ctx).await;

    let (target_cid, vg_sys_id) = with_pve_special_reg(&ctx, |reg| {
        let (cid, inc) = reg.incursions.iter().next().unwrap();
        (*cid, inc.vanguard_solar_system_ids[0])
    });

    let (boss_bid, site_id, site_pos) = with_pve_special_reg(&ctx, |reg| {
        let site = reg.incursions[&target_cid].sites.iter().find(|s| s.tier == "Vanguard").unwrap();
        (site.boss_npc_ball_id, site.site_id.clone(), site.position)
    });

    let vg_sys = ctx.systems.get_or_spawn(vg_sys_id);
    let p1_ship_id = 710_001_006;
    let mut p1_sess = create_in_space_session(&ctx, 8109, 90_000_001, vg_sys_id, p1_ship_id, 587).await;
    vg_sys.set_ball_position(p1_ship_id, site_pos).await;
    crate::common::clear_incursion_site_escort_waves(&ctx, vg_sys_id, &site_id).await;
    vg_sys.set_ball_position(boss_bid, site_pos + DVec3::new(100.0, 0.0, 0.0)).await;

    // 1. Double payout race concurrency protection (NF-INC-RACE-01)
    let char_id = 90_000_001;
    let initial_balance = ctx.db.with_snapshot(|s| s.characters.get(&char_id).map(|c| c.balance.0).unwrap_or(0));
    let _ = vg_sys.remove_ball(boss_bid).await;
    with_pve_special_reg(&ctx, |reg| {
        let inc = reg.incursions.get_mut(&target_cid).unwrap();
        let site = inc.sites.iter_mut().find(|s| s.site_id == site_id).unwrap();
        site.boss_destroyed = true;
    });

    let (res1, res2) = tokio::join!(
        eve_server::services::pve_incursion_sites::process_incursion_site_completion_and_payout(&ctx, Some(char_id), None, vg_sys_id, &site_id, None),
        eve_server::services::pve_incursion_sites::process_incursion_site_completion_and_payout(&ctx, Some(char_id), None, vg_sys_id, &site_id, None),
    );
    assert_eq!([res1.is_ok(), res2.is_ok()].into_iter().filter(|&x| x).count(), 1, "Exactly one concurrent attempt must succeed");
    assert_eq!([res1.is_err(), res2.is_err()].into_iter().filter(|&x| x).count(), 1, "Duplicate concurrent attempt must fail");
    let final_balance = ctx.db.with_snapshot(|s| s.characters.get(&char_id).map(|c| c.balance.0).unwrap_or(0));
    assert!(final_balance > initial_balance, "Pilot must receive exactly one payout");

    // 2. Smartbomb AoE Boss Destruction (NF-INC-AOE-ID-FILTER-01)
    let next_boss_bid = crate::common::clear_incursion_site_escort_waves(&ctx, vg_sys_id, &site_id).await;
    vg_sys.set_ball_position(next_boss_bid, site_pos + DVec3::new(100.0, 0.0, 0.0)).await;

    let smartbomb_id = 202_005;
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(smartbomb_id, InventoryItemRecord {
            item_id: smartbomb_id, type_id: 3993, owner_id: 90_000_001, location_id: p1_ship_id,
            flag: InventoryFlag::HiSlot1, quantity: 1, singleton: true, ..Default::default()
        });
        s.module_online_states.insert(smartbomb_id, true);
    });
    let mut cs = vg_sys.get_combat_state(next_boss_bid).await.unwrap().unwrap();
    cs.shield_hp = 0.0; cs.armor_hp = 0.0; cs.structure_hp = 1.0;
    let _ = vg_sys.set_combat_state(next_boss_bid, cs).await;

    let sb_res = eve_server::services::combat_crimewatch::dispatch_combat_rpc(
        &ctx, &mut p1_sess, "dogmaIM", "Activate", &[PyRep::Integer(smartbomb_id)], &Default::default()
    ).await.expect("Smartbomb Activate");
    assert!(matches!(sb_res, PyRep::Dict(_) | PyRep::KeyVal(_)));

    let count1 = with_pve_special_reg(&ctx, |r| {
        r.incursions[&target_cid].sites.iter().find(|s| s.site_id == site_id).unwrap().completed_count
    });
    assert_eq!(count1, 2, "Smartbomb AoE boss destruction must advance wave");

    // 3. Synchronous weapon cycle destruction (NF-INC-CYCLE-ASYNC-01)
    let next_boss2 = crate::common::clear_incursion_site_escort_waves(&ctx, vg_sys_id, &site_id).await;
    vg_sys.set_ball_position(next_boss2, site_pos + DVec3::new(100.0, 0.0, 0.0)).await;
    let mut cs2 = vg_sys.get_combat_state(next_boss2).await.unwrap().unwrap();
    cs2.shield_hp = 0.0; cs2.armor_hp = 0.0; cs2.structure_hp = 1.0;
    let _ = vg_sys.set_combat_state(next_boss2, cs2).await;
    let cycle = eve_server::system_actor::ActiveModuleCycle {
        ship_id: p1_ship_id, module_id: 202_006, module_type_id: 12056, owner_char_id: Some(90_000_001),
        effect_name: "projectileFired".into(), target_id: Some(next_boss2), repeat: 1, duration_s: 1.0, elapsed_s: 0.99,
        damage_vector: DamageVector { em: 50.0, thermal: 50.0, kinetic: 50.0, explosive: 50.0 }, ..Default::default()
    };
    vg_sys.add_module_cycle(cycle).await.unwrap();
    let _ = vg_sys.step_manual(1.0).await.unwrap();
    let count2 = with_pve_special_reg(&ctx, |r| r.incursions[&target_cid].sites.iter().find(|s| s.site_id == site_id).unwrap().completed_count);
    assert_eq!(count2, 3, "Synchronous weapon cycle destruction must advance incursion wave");
}

pub(crate) mod incursion_spawn_lifecycle_roles_map {
    // Integration tests for Sansha Incursion Lifecycle, Site Classes, Influence, System Effects,
    // Fleet Payouts, and World Event Tales (`feat:pve.incursion-payout`, `feat:pve.incursion-spawn-lifecycle`,
    // `feat:pve.incursion-influence`, `feat:pve.incursion-system-effects`, `feat:pve.incursion-site-classes`, `feat:pve.tales`).


    use eve_db::{FleetRecord, InventoryFlag, InventoryItemRecord};
    use eve_destiny::{Ball, DamageVector, MovementMode};
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_server::router::ServerContext;
    use eve_server::services::inventory_fitting::fitting_scanners_superweapons::apply_active_dbuffs_and_system_effects_to_ship_attrs;
    use eve_server::services::pve_incursion_lifecycle::{
        advance_incursion_lifecycles, advance_incursion_wave_on_escort_kill,
        compute_incursion_system_effects, get_incursion_system_wide_dbuffs, register_world_event_tale,
        WorldEventTaleRecord, INCURSION_RESPAWN_CADENCE_SEC, INCURSION_WITHDRAWAL_DURATION_SEC,
    };
    use eve_server::services::pve_incursion_sites::{
        get_incursion_effects_for_system, on_incursion_boss_destroyed,
    };
    use eve_server::services::pve_special_sites::{
        advance_pve_special_clock, ensure_special_sites_spawned, with_pve_special_reg,
    };
    use glam::DVec3;
    use std::collections::{HashMap, HashSet};

    async fn create_in_space_pilot(
        ctx: &ServerContext,
        session_id: u64,
        user: &str,
        pass: &str,
        char_id: i64,
        solar_system_id: u32,
        ship_id: i64,
        ship_type_id: u32,
        pos: DVec3,
    ) -> SessionState {
        let kw = HashMap::new();
        let mut sess = SessionState::new(session_id);
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

        sess.solar_system_id = Some(solar_system_id);
        sess.station_id = None;
        sess.structure_id = None;
        sess.ship_id = Some(ship_id);

        ctx.db.with_mut_snapshot(|snap| {
            let p1_template = snap.characters[&90_000_001].clone();
            let ch = snap.characters.entry(char_id).or_insert_with(|| p1_template);
            ch.character_id = char_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = solar_system_id;
            ch.active_ship_id = ship_id;

            snap.items.insert(
                ship_id,
                InventoryItemRecord {
                    item_id: ship_id,
                    type_id: ship_type_id,
                    owner_id: char_id,
                    location_id: i64::from(solar_system_id),
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            snap.item_positions.insert(ship_id, pos.to_array());
        });

        let sys_handle = ctx.systems.get_or_spawn(solar_system_id);
        let _ = sys_handle
            .spawn_ball(Ball {
                id: ship_id,
                type_id: ship_type_id,
                position: pos,
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
    // 1. Spawn Lifecycle, Constellation Roles, Site Classes & Map RPCs
    // ============================================================================
    #[tokio::test]
    async fn test_incursion_spawn_lifecycle_roles_and_map_rpcs() {
        // parity: feat:pve.incursion-spawn-lifecycle, feat:pve.incursion-site-classes
        eve_testkit::attest(&[
            "feat:pve.incursion-spawn-lifecycle",
            "feat:pve.incursion-site-classes",
        ]);

        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        ensure_special_sites_spawned(&ctx).await;
        let kw = HashMap::new();

        // 1. Verify eligible constellations with distinct staging/vanguard/assault/HQ roles
        let (target_cid, staging_sys, vg_sys_list, as_sys_list, hq_sys, vg_site_id, vg_boss_bid, vg_escorts) =
            with_pve_special_reg(&ctx, |reg| {
                assert_eq!(reg.incursions.len(), 3, "3 concurrent incursions seeded");
                let (&cid, inc) = reg.incursions.iter().next().expect("active incursion");
                assert_ne!(cid, 20_000_020, "Trade hub constellation must not host incursion");
                let mut role_systems = HashSet::new();
                assert!(role_systems.insert(inc.staging_solar_system_id));
                for &s in &inc.vanguard_solar_system_ids {
                    assert!(role_systems.insert(s), "Vanguard system must be distinct");
                }
                for &s in &inc.assault_solar_system_ids {
                    assert!(role_systems.insert(s), "Assault system must be distinct");
                }
                assert!(role_systems.insert(inc.hq_solar_system_id), "HQ system must be distinct");

                // Verify every site spawns ONLY in a system with the matching role
                for site in &inc.sites {
                    match site.tier.as_str() {
                        "Staging" => assert_eq!(site.solar_system_id, inc.staging_solar_system_id),
                        "Vanguard" => {
                            assert!(inc.vanguard_solar_system_ids.contains(&site.solar_system_id));
                            assert_eq!(site.min_pilots, 5);
                            assert_eq!(site.max_optimal_pilots, 10);
                        }
                        "Assault" => {
                            assert!(inc.assault_solar_system_ids.contains(&site.solar_system_id));
                            assert_eq!(site.min_pilots, 10);
                            assert_eq!(site.max_optimal_pilots, 20);
                        }
                        "Headquarters" => {
                            assert_eq!(site.solar_system_id, inc.hq_solar_system_id);
                            assert_eq!(site.min_pilots, 20);
                            assert_eq!(site.max_optimal_pilots, 40);
                        }
                        "Mothership" => {
                            assert_eq!(site.solar_system_id, inc.hq_solar_system_id);
                            assert_eq!(site.min_pilots, 40);
                            assert_eq!(site.max_optimal_pilots, 80);
                        }
                        other => panic!("Unexpected site tier {other}"),
                    }
                }
                let vg = inc.sites.iter().find(|s| s.tier == "Vanguard").expect("Vanguard site");
                let esc_ids: Vec<i64> = vg.escort_spawns.iter().map(|e| e.npc_ball_id).collect();
                (
                    cid,
                    inc.staging_solar_system_id,
                    inc.vanguard_solar_system_ids.clone(),
                    inc.assault_solar_system_ids.clone(),
                    inc.hq_solar_system_id,
                    vg.site_id.clone(),
                    vg.boss_npc_ball_id,
                    esc_ids,
                )
            });

        let mut sess = create_in_space_pilot(
            &ctx,
            8201,
            "demo",
            "demo-secret",
            90_000_001,
            hq_sys,
            720_001_001,
            587,
            DVec3::ZERO,
        )
        .await;

        // 2. Verify map.GetSystemsInIncursions and map.GetIncursionGlobalReport reflect live state
        let sys_rep = ctx
            .dispatch_rpc(&mut sess, "map", None, "GetSystemsInIncursions", &[], &kw, None)
            .await
            .expect("GetSystemsInIncursions");
        let PyRep::List(sys_entries) = &sys_rep else {
            panic!("Expected List from GetSystemsInIncursions");
        };
        assert!(sys_entries.iter().any(|e| {
            e.dict_get("solarSystemID").and_then(|v| v.as_i64()) == Some(i64::from(staging_sys))
                && e.dict_get("sceneType").and_then(|v| v.as_i64()) == Some(1)
                && e.dict_get("templateNameID").and_then(|v| v.as_i64()) == Some(564_035)
        }));
        assert!(sys_entries.iter().any(|e| {
            e.dict_get("solarSystemID").and_then(|v| v.as_i64()) == Some(i64::from(vg_sys_list[0]))
                && e.dict_get("sceneType").and_then(|v| v.as_i64()) == Some(2)
        }));
        assert!(sys_entries.iter().any(|e| {
            e.dict_get("solarSystemID").and_then(|v| v.as_i64()) == Some(i64::from(as_sys_list[0]))
                && e.dict_get("sceneType").and_then(|v| v.as_i64()) == Some(3)
        }));
        assert!(sys_entries.iter().any(|e| {
            e.dict_get("solarSystemID").and_then(|v| v.as_i64()) == Some(i64::from(hq_sys))
                && e.dict_get("sceneType").and_then(|v| v.as_i64()) == Some(4)
        }));

        let report_rep = ctx
            .dispatch_rpc(&mut sess, "map", None, "GetIncursionGlobalReport", &[], &kw, None)
            .await
            .expect("GetIncursionGlobalReport");
        let PyRep::List(reports) = &report_rep else {
            panic!("Expected List from GetIncursionGlobalReport");
        };
        let rep_row = reports
            .iter()
            .find(|r| r.dict_get("constellationID").and_then(|v| v.as_i64()) == Some(i64::from(target_cid)))
            .expect("target constellation in report");
        assert_eq!(rep_row.dict_get("state").and_then(|v| v.as_str()), Some("Establishing"));
        assert_eq!(rep_row.dict_get("influence").and_then(|v| v.as_f64()), Some(1.0));
        assert_eq!(rep_row.dict_get("hasBoss").and_then(|v| v.as_bool()), Some(false));

        // 3. Verify multi-wave NPC spawning & boss wave gate: boss is NOT spawned in Wave 1 and early completion is rejected
        let vg_sys_id = vg_sys_list[0];
        let vg_handle = ctx.systems.get_or_spawn(vg_sys_id);
        assert!(
            vg_handle.get_ball_snapshot(vg_boss_bid).await.unwrap().is_none(),
            "Vanguard boss must NOT spawn in Wave 1 before escort waves are cleared"
        );
        let early_err = on_incursion_boss_destroyed(&ctx, vg_sys_id, vg_boss_bid, Some(90_000_001))
            .await
            .expect("site matched")
            .expect_err("early boss kill on Wave 1 must be rejected");
        assert_eq!(early_err.code, "IncursionSiteNpcsRemaining");

        with_pve_special_reg(&ctx, |reg| {
            let inc = reg.incursions.get_mut(&target_cid).unwrap();
            let site = inc.sites.iter_mut().find(|s| s.site_id == vg_site_id).unwrap();
            for esc in &mut site.escort_spawns {
                esc.destroyed = true;
            }
        });
        assert!(!vg_escorts.is_empty());
        advance_incursion_wave_on_escort_kill(&ctx.systems, &ctx.db, &ctx.sde, vg_sys_id, &vg_site_id).await;
        let wave_after = with_pve_special_reg(&ctx, |reg| {
            reg.incursion_site_waves.get(&vg_site_id).map(|w| w.current_wave).unwrap_or(1)
        });
        assert_eq!(wave_after, 2, "Destroying wave 1 escorts must spawn wave 2 reinforcements");
        assert!(vg_handle.get_ball_snapshot(vg_boss_bid).await.unwrap().is_none(), "Boss still deferred on Wave 2 of 3");

        // 4. Unlock Mothership at low influence, clear its escort fleet, kill it, verify withdrawal timer & respawn cadence
        let (ms_site_id, ms_boss_bid) = with_pve_special_reg(&ctx, |reg| {
            let inc = reg.incursions.get_mut(&target_cid).unwrap();
            inc.influence = 0.05;
            inc.mothership_spawned = true;
            let ms = inc.sites.iter_mut().find(|s| s.tier == "Mothership").unwrap();
            ms.locked_until_low_influence = false;
            (ms.site_id.clone(), ms.boss_npc_ball_id)
        });
        crate::common::clear_incursion_site_escort_waves(&ctx, hq_sys, &ms_site_id).await;
        let ms_res = on_incursion_boss_destroyed(&ctx, hq_sys, ms_boss_bid, Some(90_000_001))
            .await
            .expect("Mothership site matched")
            .expect("Mothership completion succeeded");
        assert!(ms_res.incursion_ended);
        assert!(with_pve_special_reg(&ctx, |reg| reg.incursions[&target_cid].mothership_defeated));

        let state_after_kill = with_pve_special_reg(&ctx, |reg| reg.incursions[&target_cid].state.clone());
        assert_eq!(state_after_kill, "Withdrawing", "Mothership death enters Withdrawing state");

        // Advance clock past 24-hour withdrawal duration
        advance_pve_special_clock(&ctx, INCURSION_WITHDRAWAL_DURATION_SEC + 60);
        let state_after_withdraw = with_pve_special_reg(&ctx, |reg| reg.incursions[&target_cid].state.clone());
        assert_eq!(state_after_withdraw, "Ended", "Incursion ends after withdrawal timer expires");

        // Verify ended incursion disappears from map.GetSystemsInIncursions
        let sys_rep_ended = ctx
            .dispatch_rpc(&mut sess, "map", None, "GetSystemsInIncursions", &[], &kw, None)
            .await
            .expect("GetSystemsInIncursions after withdrawal");
        let PyRep::List(sys_entries_ended) = &sys_rep_ended else {
            panic!("Expected List");
        };
        assert!(
            !sys_entries_ended
                .iter()
                .any(|e| e.dict_get("constellationID").and_then(|v| v.as_i64()) == Some(i64::from(target_cid))),
            "Ended incursion systems must be removed from GetSystemsInIncursions"
        );
        let rep_ended = ctx
            .dispatch_rpc(&mut sess, "map", None, "GetIncursionGlobalReport", &[], &kw, None)
            .await
            .expect("GetIncursionGlobalReport after withdrawal");
        let PyRep::List(reports_ended) = &rep_ended else {
            panic!("Expected List");
        };
        assert!(
            !reports_ended
                .iter()
                .any(|r| r.dict_get("constellationID").and_then(|v| v.as_i64()) == Some(i64::from(target_cid))),
            "Ended incursion must be excluded from GetIncursionGlobalReport"
        );

        // Advance clock past 12-hour respawn cadence -> new incursion spawns
        advance_pve_special_clock(&ctx, INCURSION_RESPAWN_CADENCE_SEC + 60);
        let active_count_after_cadence = with_pve_special_reg(&ctx, |reg| {
            reg.incursions.values().filter(|i| i.state != "Ended").count()
        });
        assert_eq!(
            active_count_after_cadence, 3,
            "A new incursion must spawn once the 12-hour respawn cadence elapses"
        );
    }

    // ============================================================================
    // 2. Influence Reduction, Regeneration, Mothership Unlock & System DBuffs
    // ============================================================================
    #[tokio::test]
    async fn test_incursion_influence_regen_mothership_unlock_and_system_effects() {
        // parity: feat:pve.incursion-influence, feat:pve.incursion-system-effects
        eve_testkit::attest(&[
            "feat:pve.incursion-influence",
            "feat:pve.incursion-system-effects",
        ]);

        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        ensure_special_sites_spawned(&ctx).await;

        let (target_cid, hq_sys, vg_sys, vg_sid, as_sys, as_sid, hq_sid) =
            with_pve_special_reg(&ctx, |reg| {
                let (&cid, inc) = reg.incursions.iter().next().unwrap();
                assert_eq!(inc.influence, 1.0, "Influence must start at 100% (1.0)");
                let vg = inc.sites.iter().find(|s| s.tier == "Vanguard").unwrap();
                let ass = inc.sites.iter().find(|s| s.tier == "Assault").unwrap();
                let hq = inc.sites.iter().find(|s| s.tier == "Headquarters").unwrap();
                (
                    cid,
                    inc.hq_solar_system_id,
                    vg.solar_system_id,
                    vg.site_id.clone(),
                    ass.solar_system_id,
                    ass.site_id.clone(),
                    hq.site_id.clone(),
                )
            });

        let _sess = create_in_space_pilot(
            &ctx,
            8202,
            "demo",
            "demo-secret",
            90_000_001,
            hq_sys,
            720_001_002,
            587,
            DVec3::ZERO,
        )
        .await;

        // 1. Verify full Sansha system-wide dbuffs & penalties at 1.0 influence
        let dbuffs_full = get_incursion_system_wide_dbuffs(&ctx, hq_sys);
        assert_eq!(dbuffs_full.len(), 2, "Shield & Armor resistance system dbuffs active");
        assert!((dbuffs_full[0].1 - 50.0).abs() < 1e-6);

        let eff_full = get_incursion_effects_for_system(&ctx, hq_sys).expect("active effects");
        let comp_full = compute_incursion_system_effects(&ctx, hq_sys).expect("computed effects");
        assert_eq!(eff_full.bounty_penalty, 0.50);
        assert_eq!(comp_full.lp_penalty, 0.50);

        let mut attrs_full = HashMap::from([(263, 1000.0), (265, 1000.0), (482, 1000.0), (64, 1.0)]);
        apply_active_dbuffs_and_system_effects_to_ship_attrs(&ctx, 720_001_002, &mut attrs_full);
        assert_eq!(attrs_full[&263], 500.0);
        assert_eq!(attrs_full[&64], 0.5);

        // 2. Completing Vanguard (-0.05), Assault (-0.20), and HQ (-0.25) after clearing waves reduces influence by site class
        let vg_bid = crate::common::clear_incursion_site_escort_waves(&ctx, vg_sys, &vg_sid).await;
        let _ = ctx.systems.get_or_spawn(vg_sys).remove_ball(vg_bid).await;
        on_incursion_boss_destroyed(&ctx, vg_sys, vg_bid, Some(90_000_001)).await.unwrap().unwrap();
        let inf_after_vg = with_pve_special_reg(&ctx, |reg| reg.incursions[&target_cid].influence);
        assert!((inf_after_vg - 0.95).abs() < 1e-6, "Vanguard reduces influence by 0.05");

        let as_bid = crate::common::clear_incursion_site_escort_waves(&ctx, as_sys, &as_sid).await;
        let _ = ctx.systems.get_or_spawn(as_sys).remove_ball(as_bid).await;
        on_incursion_boss_destroyed(&ctx, as_sys, as_bid, Some(90_000_001)).await.unwrap().unwrap();
        let inf_after_as = with_pve_special_reg(&ctx, |reg| reg.incursions[&target_cid].influence);
        assert!((inf_after_as - 0.75).abs() < 1e-6, "Assault reduces influence by 0.20");

        let hq_bid = crate::common::clear_incursion_site_escort_waves(&ctx, hq_sys, &hq_sid).await;
        let _ = ctx.systems.get_or_spawn(hq_sys).remove_ball(hq_bid).await;
        on_incursion_boss_destroyed(&ctx, hq_sys, hq_bid, Some(90_000_001)).await.unwrap().unwrap();
        let inf_after_hq = with_pve_special_reg(&ctx, |reg| reg.incursions[&target_cid].influence);
        assert!((inf_after_hq - 0.50).abs() < 1e-6, "HQ reduces influence by 0.25");

        // 3. Influence regenerates over time (+2% / hour) when sites are not run
        advance_pve_special_clock(&ctx, 2 * 3_600);
        advance_incursion_lifecycles(&ctx);
        let inf_after_regen = with_pve_special_reg(&ctx, |reg| reg.incursions[&target_cid].influence);
        assert!(
            (inf_after_regen - 0.54).abs() < 1e-6,
            "2 hours idle must regenerate +0.04 influence (0.50 -> 0.54), got {inf_after_regen}"
        );

        // 4. Lower influence to 0.50 -> system effects & dbuffs scale proportionally (25% penalty)
        with_pve_special_reg(&ctx, |reg| {
            reg.incursions.get_mut(&target_cid).unwrap().influence = 0.50;
        });
        let dbuffs_half = get_incursion_system_wide_dbuffs(&ctx, hq_sys);
        assert!((dbuffs_half[0].1 - 25.0).abs() < 1e-6, "DBuff scales linearly with influence");
        let mut attrs_half = HashMap::from([(263, 1000.0), (265, 1000.0), (482, 1000.0), (64, 1.0)]);
        apply_active_dbuffs_and_system_effects_to_ship_attrs(&ctx, 720_001_002, &mut attrs_half);
        assert_eq!(attrs_half[&263], 750.0, "25% shield penalty at 0.50 influence");
        assert_eq!(attrs_half[&64], 0.75, "25% damage penalty at 0.50 influence");

        // 5. At 0.08 influence (> 0%), Mothership stays locked; dropping to 0.0 (0%) unlocks Mothership -> Mobilizing
        let (vg2_sys, vg2_sid, second_hq_sid, ms_sid) = with_pve_special_reg(&ctx, |reg| {
            let inc = reg.incursions.get_mut(&target_cid).unwrap();
            inc.influence = 0.13;
            let vg2 = inc.sites.iter().find(|s| s.tier == "Vanguard" && !s.boss_destroyed).unwrap();
            let hq2 = inc.sites.iter().find(|s| s.tier == "Headquarters" && !s.boss_destroyed).unwrap();
            let ms = inc.sites.iter().find(|s| s.tier == "Mothership").unwrap();
            (vg2.solar_system_id, vg2.site_id.clone(), hq2.site_id.clone(), ms.site_id.clone())
        });
        let vg2_bid = crate::common::clear_incursion_site_escort_waves(&ctx, vg2_sys, &vg2_sid).await;
        let _ = ctx.systems.get_or_spawn(vg2_sys).remove_ball(vg2_bid).await;
        on_incursion_boss_destroyed(&ctx, vg2_sys, vg2_bid, Some(90_000_001)).await.unwrap().unwrap();
        let still_locked_at_008 = with_pve_special_reg(&ctx, |reg| {
            let inc = &reg.incursions[&target_cid];
            (inc.influence - 0.08).abs() < 1e-6 && !inc.mothership_spawned && inc.state != "Mobilizing"
        });
        assert!(still_locked_at_008, "Mothership must remain locked at 8% (0.08) influence");

        let second_hq_bid = crate::common::clear_incursion_site_escort_waves(&ctx, hq_sys, &second_hq_sid).await;
        let _ = ctx.systems.get_or_spawn(hq_sys).remove_ball(second_hq_bid).await;
        on_incursion_boss_destroyed(&ctx, hq_sys, second_hq_bid, Some(90_000_001)).await.unwrap().unwrap();
        let ms_unlocked = with_pve_special_reg(&ctx, |reg| {
            let inc = &reg.incursions[&target_cid];
            inc.mothership_spawned && inc.influence <= 0.0 && inc.state == "Mobilizing"
        });
        assert!(ms_unlocked, "Dropping influence to 0.0 must unlock Mothership site and enter Mobilizing");

        let ms_boss_id = crate::common::clear_incursion_site_escort_waves(&ctx, hq_sys, &ms_sid).await;
        let _ = ctx.systems.get_or_spawn(hq_sys).remove_ball(ms_boss_id).await;
        on_incursion_boss_destroyed(&ctx, hq_sys, ms_boss_id, Some(90_000_001)).await.unwrap().unwrap();
        assert!(
            get_incursion_system_wide_dbuffs(&ctx, hq_sys).is_empty(),
            "Sansha system dbuffs must be removed on withdrawal"
        );
        let mut attrs_after = HashMap::from([(263, 1000.0), (265, 1000.0), (482, 1000.0), (64, 1.0)]);
        apply_active_dbuffs_and_system_effects_to_ship_attrs(&ctx, 720_001_002, &mut attrs_after);
        assert_eq!(attrs_after[&263], 1000.0, "Ship shield unaffected after incursion withdrawal");
        assert_eq!(attrs_after[&64], 1.0, "Ship damage unaffected after incursion withdrawal");
    }

    // ============================================================================
    // 3. On-Grid-Only Fleet Payouts & World Event Tales
    // ============================================================================
    #[tokio::test]
    async fn test_incursion_payout_on_grid_only_and_world_event_tales() {
        // parity: feat:pve.incursion-payout, feat:pve.tales
        eve_testkit::attest(&["feat:pve.incursion-payout", "feat:pve.tales"]);

        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        ensure_special_sites_spawned(&ctx).await;
        let kw = HashMap::new();

        let (target_cid, vg_sys_id, hq_sys_id, vg_site_id, site_pos) = with_pve_special_reg(&ctx, |reg| {
            let (&cid, inc) = reg.incursions.iter().next().unwrap();
            let vg = inc.sites.iter().find(|s| s.tier == "Vanguard").unwrap();
            (cid, vg.solar_system_id, inc.hq_solar_system_id, vg.site_id.clone(), vg.position)
        });

        // Fleet 950_001: Pilot 1 & Pilot 2 on grid at site; Pilot 3 in same system 800 km off-grid; Pilot 4 in HQ system
        let p1 = 90_000_001;
        let p2 = 90_000_002;
        let uid3 = ctx.db.provision_player_account("incp3", "incp3-secret");
        let p3_offgrid = ctx
            .db
            .create_character(uid3, "Incursion Offgrid Pilot", 1, 1, 11, 17, &ctx.sde)
            .expect("create p3")
            .character_id;
        let uid4 = ctx.db.provision_player_account("incp4", "incp4-secret");
        let p4_other_sys = ctx
            .db
            .create_character(uid4, "Incursion Other Sys Pilot", 1, 1, 11, 17, &ctx.sde)
            .expect("create p4")
            .character_id;

        let mut sess1 = create_in_space_pilot(&ctx, 8211, "demo", "demo-secret", p1, vg_sys_id, 720_010_001, 587, site_pos).await;
        let _sess2 = create_in_space_pilot(
            &ctx,
            8212,
            "other",
            "other-secret",
            p2,
            vg_sys_id,
            720_010_002,
            587,
            site_pos + DVec3::new(2_500.0, 0.0, 0.0),
        )
        .await;
        let _sess3 = create_in_space_pilot(
            &ctx,
            8213,
            "incp3",
            "incp3-secret",
            p3_offgrid,
            vg_sys_id,
            720_010_003,
            587,
            site_pos + DVec3::new(800_000.0, 0.0, 0.0),
        )
        .await;
        let _sess4 = create_in_space_pilot(&ctx, 8214, "incp4", "incp4-secret", p4_other_sys, hq_sys_id, 720_010_004, 587, DVec3::ZERO).await;

        ctx.db.with_mut_snapshot(|snap| {
            let members = vec![p1, p2, p3_offgrid, p4_other_sys];
            for &pid in &members {
                if let Some(ch) = snap.characters.get_mut(&pid) {
                    ch.fleet_id = Some(950_001);
                }
            }
            snap.fleets.insert(
                950_001,
                FleetRecord {
                    fleet_id: 950_001,
                    boss_id: p1,
                    members,
                    ..Default::default()
                },
            );
        });

        let init_state: HashMap<i64, (i64, i64)> = [p1, p2, p3_offgrid, p4_other_sys]
            .iter()
            .map(|&pid| {
                ctx.db.with_snapshot(|s| {
                    let ch = &s.characters[&pid];
                    (pid, (ch.balance.0, ch.loyalty_points.get(&1_000_125).copied().unwrap_or(0)))
                })
            })
            .collect();

        let vg_boss_bid = crate::common::clear_incursion_site_escort_waves(&ctx, vg_sys_id, &vg_site_id).await;
        let vg_sys = ctx.systems.get_or_spawn(vg_sys_id);
        let _ = vg_sys
            .apply_damage_with_attacker(
                vg_boss_bid,
                DamageVector {
                    em: 1.0e7,
                    thermal: 1.0e7,
                    kinetic: 1.0e7,
                    explosive: 1.0e7,
                },
                Some(p1),
            )
            .await
            .expect("kill vanguard boss");
        let _ = vg_sys.remove_ball(vg_boss_bid).await;
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        if let Some(res) = on_incursion_boss_destroyed(&ctx, vg_sys_id, vg_boss_bid, Some(p1)).await {
            res.expect("incursion payout succeeded");
        }

        // Verify ONLY on-grid pilots (p1, p2) were paid ISK + CONCORD LP
        for &on_grid_pid in &[p1, p2] {
            let (bal_after, lp_after) = ctx.db.with_snapshot(|s| {
                let ch = &s.characters[&on_grid_pid];
                (ch.balance.0, ch.loyalty_points.get(&1_000_125).copied().unwrap_or(0))
            });
            assert!(bal_after > init_state[&on_grid_pid].0, "On-grid pilot {on_grid_pid} must receive ISK");
            assert!(lp_after > init_state[&on_grid_pid].1, "On-grid pilot {on_grid_pid} must receive CONCORD LP");
        }
        for &excluded_pid in &[p3_offgrid, p4_other_sys] {
            let (bal_after, lp_after) = ctx.db.with_snapshot(|s| {
                let ch = &s.characters[&excluded_pid];
                (ch.balance.0, ch.loyalty_points.get(&1_000_125).copied().unwrap_or(0))
            });
            assert_eq!(bal_after, init_state[&excluded_pid].0, "Off-grid/out-of-system pilot {excluded_pid} must not receive ISK");
            assert_eq!(lp_after, init_state[&excluded_pid].1, "Off-grid/out-of-system pilot {excluded_pid} must not receive LP");
        }

        // Verify taleMgr.GetGlobalWorldEventTales & get_active_tales_by_template return active world events + system state
        let custom_tale_id = register_world_event_tale(
            &ctx,
            WorldEventTaleRecord {
                tale_id: 0,
                template_id: 5301,
                template_class_id: "pirateInvasion".into(),
                solar_system_id: vg_sys_id,
                constellation_id: target_cid,
                aggressor_faction_id: 500_010,
                state: "InvasionContested".into(),
                influence: 0.65,
                start_unix: ctx.now_unix().saturating_sub(3_600),
                end_unix: ctx.now_unix() + 86_400,
            },
        );

        let all_tales_rep = ctx
            .dispatch_rpc(&mut sess1, "taleMgr", None, "GetGlobalWorldEventTales", &[], &kw, None)
            .await
            .expect("GetGlobalWorldEventTales");
        let PyRep::List(all_tales) = &all_tales_rep else {
            panic!("Expected List");
        };
        assert!(all_tales.iter().any(|t| t.dict_get("templateID").and_then(|v| v.as_i64()) == Some(5158)));
        assert!(all_tales.iter().any(|t| t.dict_get("templateID").and_then(|v| v.as_i64()) == Some(5201)));
        assert!(all_tales.iter().any(|t| {
            t.dict_get("taleID").and_then(|v| v.as_i64()) == Some(custom_tale_id)
                && t.dict_get("systemState").and_then(|v| v.as_str()) == Some("InvasionContested")
        }));

        let inv_tales_rep = ctx
            .dispatch_rpc(
                &mut sess1,
                "taleMgr",
                None,
                "get_active_tales_by_template",
                &[PyRep::Integer(5301)],
                &kw,
                None,
            )
            .await
            .expect("get_active_tales_by_template");
        let PyRep::List(inv_tales) = &inv_tales_rep else {
            panic!("Expected List");
        };
        assert_eq!(inv_tales.len(), 1);
        assert_eq!(
            inv_tales[0].dict_get("systemState").and_then(|v| v.as_str()),
            Some("InvasionContested")
        );
    }
}

pub(crate) mod incursion_sde_stargate_topology_roles {
    // Part 2 integration tests for Sansha Incursion SDE Stargate-Graph Topology Roles,
    // Fleet Payout Scaling (Optimal, Over-Cap Diminishing Return, Hard-Cap Zero Cutoff),
    // `incursion.UpdateIncursionLPLog` Reconciliation, and `taleMgr` `PyRep::Dict` Shapes
    // (`feat:pve.incursion-payout`, `feat:pve.incursion-spawn-lifecycle`, `feat:pve.incursion-site-classes`, `feat:pve.tales`).


    use eve_db::{FleetRecord, InventoryFlag, InventoryItemRecord};
    use eve_destiny::{Ball, MovementMode};
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_server::router::ServerContext;
    use eve_server::services::pve_incursion_lifecycle::{
        assign_constellation_system_roles, STORM_TALE_TEMPLATE_ID,
    };
    use eve_server::services::pve_incursion_sites::on_incursion_boss_destroyed;
    use eve_server::services::pve_special_sites::{ensure_special_sites_spawned, with_pve_special_reg};
    use glam::DVec3;
    use std::collections::{HashMap, HashSet, VecDeque};

    async fn spawn_fleet_pilot_on_grid(
        ctx: &ServerContext,
        idx: usize,
        solar_system_id: u32,
        pos: DVec3,
    ) -> (i64, SessionState) {
        let kw = HashMap::new();
        let uname = format!("incscale_{idx}");
        let uid = ctx.db.provision_player_account(&uname, "scale-secret");
        let char_id = ctx
            .db
            .create_character(uid, &format!("Incursion Scale Pilot {idx}"), 1, 1, 11, 17, &ctx.sde)
            .expect("create incursion pilot")
            .character_id;
        let ship_id = 721_000_000 + (idx as i64);

        let mut sess = SessionState::new(8400 + (idx as u64));
        ctx.dispatch_rpc(
            &mut sess,
            "authentication",
            None,
            "Login",
            &[PyRep::String(uname.into()), PyRep::String("scale-secret".into())],
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

        sess.solar_system_id = Some(solar_system_id);
        sess.station_id = None;
        sess.structure_id = None;
        sess.ship_id = Some(ship_id);

        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.station_id = None;
                ch.structure_id = None;
                ch.solar_system_id = solar_system_id;
                ch.active_ship_id = ship_id;
            }
            snap.items.insert(
                ship_id,
                InventoryItemRecord {
                    item_id: ship_id,
                    type_id: 587,
                    owner_id: char_id,
                    location_id: i64::from(solar_system_id),
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            snap.item_positions.insert(ship_id, pos.to_array());
        });

        let sys_handle = ctx.systems.get_or_spawn(solar_system_id);
        let _ = sys_handle
            .spawn_ball(Ball {
                id: ship_id,
                type_id: 587,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 365.0,
                inertia: 1.0,
                mass: 1_067_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await;
        (char_id, sess)
    }

    fn intra_constellation_bfs_distances(
        ctx: &ServerContext,
        constellation_id: u32,
        start_sys: u32,
    ) -> HashMap<u32, u32> {
        let const_set: HashSet<u32> = ctx
            .sde
            .solar_systems
            .values()
            .filter(|s| s.constellation_id == constellation_id)
            .map(|s| s.solar_system_id)
            .collect();
        let mut dist = HashMap::new();
        dist.insert(start_sys, 0_u32);
        let mut queue = VecDeque::new();
        queue.push_back(start_sys);
        for _ in 0..const_set.len() {
            let Some(cur) = queue.pop_front() else {
                break;
            };
            let d = dist[&cur];
            for j in ctx.sde.jumps.values() {
                if j.from_solar_system_id == cur
                    && const_set.contains(&j.to_solar_system_id)
                    && !dist.contains_key(&j.to_solar_system_id)
                {
                    dist.insert(j.to_solar_system_id, d + 1);
                    queue.push_back(j.to_solar_system_id);
                }
            }
        }
        dist
    }

    // ============================================================================
    // 1. SDE Stargate-Graph Constellation Topology Roles & taleMgr PyRep::Dict Shapes
    // ============================================================================
    #[tokio::test]
    async fn test_incursion_sde_stargate_topology_roles_and_world_event_dict_shapes() {
        // parity: feat:pve.incursion-spawn-lifecycle, feat:pve.incursion-site-classes, feat:pve.tales
        eve_testkit::attest(&[
            "feat:pve.incursion-spawn-lifecycle",
            "feat:pve.incursion-site-classes",
            "feat:pve.tales",
        ]);

        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        ensure_special_sites_spawned(&ctx).await;

        // 1. Verify Kimotoro (20000020) stargate topology role assignment uses real SDE jumps:
        //    - 30000145 (New Caldari) has 4 external constellation stargates -> Staging
        //    - 30000141 (Kisogo, sorted index 2!) is a degree-1 dead-end pocket 4 hops from Staging -> HQ
        let mut kimotoro_systems: Vec<u32> = ctx
            .sde
            .solar_systems
            .values()
            .filter(|s| s.constellation_id == 20_000_020)
            .map(|s| s.solar_system_id)
            .collect();
        kimotoro_systems.sort_unstable();
        let (staging, vanguard, assault, hq) =
            assign_constellation_system_roles(&ctx.sde, 20_000_020, &kimotoro_systems);
        assert_eq!(
            staging, 30_000_145,
            "Staging must be border system New Caldari (30000145) with external gates, not sorted-ID first"
        );
        assert_eq!(
            hq, 30_000_141,
            "HQ must be degree-1 dead-end pocket Kisogo (30000141), not sorted-ID last"
        );
        let hops = intra_constellation_bfs_distances(&ctx, 20_000_020, staging);
        let max_vg_hop = vanguard.iter().map(|s| hops[s]).max().unwrap();
        let min_as_hop = assault.iter().map(|s| hops[s]).min().unwrap();
        assert!(
            max_vg_hop <= min_as_hop && min_as_hop <= hops[&hq],
            "BFS hop distance ordering Staging -> Vanguard ({max_vg_hop}) <= Assault ({min_as_hop}) <= HQ ({}) must hold",
            hops[&hq]
        );

        // Also verify all 3 live seeded incursions satisfy BFS hop ordering over real SDE stargates
        with_pve_special_reg(&ctx, |reg| {
            for (&cid, inc) in &reg.incursions {
                let d = intra_constellation_bfs_distances(&ctx, cid, inc.staging_solar_system_id);
                let vg_max = inc.vanguard_solar_system_ids.iter().filter_map(|s| d.get(s)).max().copied().unwrap_or(0);
                let as_min = inc.assault_solar_system_ids.iter().filter_map(|s| d.get(s)).min().copied().unwrap_or(vg_max);
                let hq_d = d.get(&inc.hq_solar_system_id).copied().unwrap_or(as_min);
                assert!(
                    vg_max <= as_min && as_min <= hq_d,
                    "Seeded incursion constellation {cid} must follow stargate BFS hop ordering"
                );
            }
        });

        // 2. Verify taleMgr returns subscriptable PyRep::Dict entries (NOT util.KeyVal) including active Metaliminal Storms
        let kw = HashMap::new();
        let (_, mut sess) = spawn_fleet_pilot_on_grid(&ctx, 1, 30_000_142, DVec3::ZERO).await;
        let tales_rep = ctx
            .dispatch_rpc(&mut sess, "taleMgr", None, "GetGlobalWorldEventTales", &[], &kw, None)
            .await
            .expect("GetGlobalWorldEventTales");
        let PyRep::List(tales) = &tales_rep else {
            panic!("Expected PyRep::List from GetGlobalWorldEventTales");
        };
        assert!(!tales.is_empty());
        for entry in tales {
            assert!(
                matches!(entry, PyRep::Dict(_)),
                "World event tale entry must be a subscriptable PyRep::Dict, got {entry:?}"
            );
        }
        assert!(
            tales.iter().any(|t| {
                t.dict_get("templateID").and_then(|v| v.as_i64()) == Some(STORM_TALE_TEMPLATE_ID)
                    && t.dict_get("templateClassID").and_then(|v| v.as_str()) == Some("metaliminalStorm")
            }),
            "GetGlobalWorldEventTales must include active Metaliminal Roaming Storms"
        );

        let storm_tales_rep = ctx
            .dispatch_rpc(
                &mut sess,
                "taleMgr",
                None,
                "get_active_tales_by_template",
                &[PyRep::Integer(STORM_TALE_TEMPLATE_ID)],
                &kw,
                None,
            )
            .await
            .expect("get_active_tales_by_template(5203)");
        let PyRep::List(storm_tales) = &storm_tales_rep else {
            panic!("Expected List for storm tales");
        };
        assert!(!storm_tales.is_empty(), "Active storm tales must be returned for template 5203");
    }

    // ============================================================================
    // 2. Fleet Payout Scaling: Optimal (<=10), Over-Cap (12), Hard-Cap (>15 -> 0), & LP Log Reconciliation
    // ============================================================================
    #[tokio::test]
    async fn test_incursion_fleet_payout_optimal_overcap_and_hard_cap_cutoff() {
        // parity: feat:pve.incursion-payout
        eve_testkit::attest(&["feat:pve.incursion-payout"]);

        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        ensure_special_sites_spawned(&ctx).await;
        let kw = HashMap::new();

        // Collect 3 Vanguard sites across the seeded constellations so each fleet-size tier runs cleanly
        let vg_sites: Vec<(u32, u32, String, i64, DVec3)> = with_pve_special_reg(&ctx, |reg| {
            reg.incursions
                .iter()
                .flat_map(|(&cid, inc)| {
                    inc.sites
                        .iter()
                        .filter(|s| s.tier == "Vanguard")
                        .map(move |s| (cid, s.solar_system_id, s.site_id.clone(), s.boss_npc_ball_id, s.position))
                })
                .take(3)
                .collect()
        });
        assert_eq!(vg_sites.len(), 3, "Need 3 Vanguard sites for optimal / overcap / hard-cap tests");

        // Pin constellation influence to 0.05 before each Vanguard site (delta 0.05 -> new_influence 0.0 -> 1.0x influence_scale)
        // so the comparison across Cases A/B/C strictly isolates fleet-size scaling.
        // Case A: Optimal fleet (5 on-grid pilots <= 10 optimal) -> full 1.0x payout (10,395,000 ISK, 1,400 LP)
        let (cid_a, sys_a, site_a, _, pos_a) = vg_sites[0].clone();
        with_pve_special_reg(&ctx, |reg| reg.incursions.get_mut(&cid_a).unwrap().influence = 0.05);
        let mut pilots_a = Vec::new();
        let mut lead_sess = None;
        for i in 0..5 {
            let (pid, s) = spawn_fleet_pilot_on_grid(&ctx, 10 + i, sys_a, pos_a + DVec3::new((i as f64) * 500.0, 0.0, 0.0)).await;
            if lead_sess.is_none() {
                lead_sess = Some(s);
            }
            pilots_a.push(pid);
        }
        let mut lead_sess = lead_sess.unwrap();
        ctx.db.with_mut_snapshot(|snap| {
            for &pid in &pilots_a {
                snap.characters.get_mut(&pid).unwrap().fleet_id = Some(960_001);
            }
            snap.fleets.insert(
                960_001,
                FleetRecord {
                    fleet_id: 960_001,
                    boss_id: pilots_a[0],
                    members: pilots_a.clone(),
                    ..Default::default()
                },
            );
        });
        let bal_before_a = ctx.db.with_snapshot(|s| s.characters[&pilots_a[0]].balance.0);
        let boss_a = crate::common::clear_incursion_site_escort_waves(&ctx, sys_a, &site_a).await;
        let _ = ctx.systems.get_or_spawn(sys_a).remove_ball(boss_a).await;
        let res_a = on_incursion_boss_destroyed(&ctx, sys_a, boss_a, Some(pilots_a[0]))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(res_a.isk_per_pilot, 10_395_000, "Optimal <=10 Vanguard fleet receives 100% ISK");
        assert_eq!(res_a.concord_lp_per_pilot, 1_400, "Optimal <=10 Vanguard fleet receives 100% CONCORD LP");
        let bal_after_a = ctx.db.with_snapshot(|s| s.characters[&pilots_a[0]].balance.0);
        assert_eq!(bal_after_a - bal_before_a, 10_395_000 * 100);

        // Verify incursion.UpdateIncursionLPLog reconciles and returns recorded LP log entries
        let lp_reconciled = ctx
            .dispatch_rpc(
                &mut lead_sess,
                "incursion",
                None,
                "UpdateIncursionLPLog",
                &[PyRep::Integer(i64::from(cid_a))],
                &kw,
                None,
            )
            .await
            .expect("UpdateIncursionLPLog");
        let PyRep::List(lp_rows) = &lp_reconciled else {
            panic!("Expected reconciled LP log list, got {lp_reconciled:?}");
        };
        assert_eq!(lp_rows.len(), 1, "Caller pilot receives reconciled LP log entry for site");
        assert_eq!(lp_rows[0].dict_get("fleetSize").and_then(|v| v.as_i64()), Some(5));
        assert_eq!(lp_rows[0].dict_get("siteID").and_then(|v| v.as_str()), Some(site_a.as_str()));

        // Case B: Over-optimal fleet (12 on-grid pilots: 10 < 12 <= 15 hard cap) -> diminishing return (< 1.0x, > 0)
        let (cid_b, sys_b, site_b, _, pos_b) = vg_sites[1].clone();
        with_pve_special_reg(&ctx, |reg| reg.incursions.get_mut(&cid_b).unwrap().influence = 0.05);
        let mut pilots_b = Vec::new();
        for i in 0..12 {
            let (pid, _) = spawn_fleet_pilot_on_grid(&ctx, 30 + i, sys_b, pos_b + DVec3::new((i as f64) * 300.0, 0.0, 0.0)).await;
            pilots_b.push(pid);
        }
        ctx.db.with_mut_snapshot(|snap| {
            for &pid in &pilots_b {
                snap.characters.get_mut(&pid).unwrap().fleet_id = Some(960_002);
            }
            snap.fleets.insert(
                960_002,
                FleetRecord {
                    fleet_id: 960_002,
                    boss_id: pilots_b[0],
                    members: pilots_b.clone(),
                    ..Default::default()
                },
            );
        });
        let boss_b = crate::common::clear_incursion_site_escort_waves(&ctx, sys_b, &site_b).await;
        let _ = ctx.systems.get_or_spawn(sys_b).remove_ball(boss_b).await;
        let res_b = on_incursion_boss_destroyed(&ctx, sys_b, boss_b, Some(pilots_b[0]))
            .await
            .unwrap()
            .unwrap();
        assert!(
            res_b.isk_per_pilot > 0 && res_b.isk_per_pilot < res_a.isk_per_pilot,
            "12-pilot overcap Vanguard payout ({}) must diminish below optimal ({})",
            res_b.isk_per_pilot,
            res_a.isk_per_pilot
        );
        assert!(res_b.concord_lp_per_pilot > 0 && res_b.concord_lp_per_pilot < res_a.concord_lp_per_pilot);

        // Case C: Beyond hard-cap fleet (16 on-grid pilots > 15 Vanguard cutoff) -> 0 ISK and 0 CONCORD LP!
        let (cid_c, sys_c, site_c, _, pos_c) = vg_sites[2].clone();
        with_pve_special_reg(&ctx, |reg| reg.incursions.get_mut(&cid_c).unwrap().influence = 0.05);
        let mut pilots_c = Vec::new();
        for i in 0..16 {
            let (pid, _) = spawn_fleet_pilot_on_grid(&ctx, 60 + i, sys_c, pos_c + DVec3::new((i as f64) * 250.0, 0.0, 0.0)).await;
            pilots_c.push(pid);
        }
        ctx.db.with_mut_snapshot(|snap| {
            for &pid in &pilots_c {
                snap.characters.get_mut(&pid).unwrap().fleet_id = Some(960_003);
            }
            snap.fleets.insert(
                960_003,
                FleetRecord {
                    fleet_id: 960_003,
                    boss_id: pilots_c[0],
                    members: pilots_c.clone(),
                    ..Default::default()
                },
            );
        });
        let bal_before_c = ctx.db.with_snapshot(|s| s.characters[&pilots_c[0]].balance.0);
        let lp_before_c = ctx
            .db
            .with_snapshot(|s| s.characters[&pilots_c[0]].loyalty_points.get(&1_000_125).copied().unwrap_or(0));
        let boss_c = crate::common::clear_incursion_site_escort_waves(&ctx, sys_c, &site_c).await;
        let _ = ctx.systems.get_or_spawn(sys_c).remove_ball(boss_c).await;
        let res_c = on_incursion_boss_destroyed(&ctx, sys_c, boss_c, Some(pilots_c[0]))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(res_c.isk_per_pilot, 0, "Exceeding Vanguard 15-pilot hard cap must pay 0 ISK");
        assert_eq!(res_c.concord_lp_per_pilot, 0, "Exceeding Vanguard 15-pilot hard cap must pay 0 LP");
        let (bal_after_c, lp_after_c) = ctx.db.with_snapshot(|s| {
            let ch = &s.characters[&pilots_c[0]];
            (ch.balance.0, ch.loyalty_points.get(&1_000_125).copied().unwrap_or(0))
        });
        assert_eq!(bal_after_c, bal_before_c, "Hard-capped pilot wallet balance must remain unchanged");
        assert_eq!(lp_after_c, lp_before_c, "Hard-capped pilot CONCORD LP must remain unchanged");
    }

    #[tokio::test]
    async fn test_incursion_boss_killed_before_final_wave_escorts_completes_on_last_escort() {
        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        ensure_special_sites_spawned(&ctx).await;

        let (cid, sys_id, site_id, boss_bid, pos) = with_pve_special_reg(&ctx, |reg| {
            let inc = reg.incursions.values().next().expect("seeded incursion");
            let site = inc.sites.iter().find(|s| s.tier == "Vanguard").expect("Vanguard site");
            (inc.constellation_id, site.solar_system_id, site.site_id.clone(), site.boss_npc_ball_id, site.position)
        });
        let (pilot_id, _) = spawn_fleet_pilot_on_grid(&ctx, 80, sys_id, pos).await;

        // Advance Wave 1 -> Wave 2 -> Wave 3 (final wave) so boss and final-wave escorts are both active
        for _ in 0..2 {
            let escorts: Vec<i64> = with_pve_special_reg(&ctx, |reg| {
                reg.incursions[&cid]
                    .sites
                    .iter()
                    .find(|s| s.site_id == site_id)
                    .unwrap()
                    .escort_spawns
                    .iter()
                    .filter(|e| !e.destroyed)
                    .map(|e| e.npc_ball_id)
                    .collect()
            });
            for eid in escorts {
                let _ = on_incursion_boss_destroyed(&ctx, sys_id, eid, Some(pilot_id)).await;
            }
        }

        let final_escorts: Vec<i64> = with_pve_special_reg(&ctx, |reg| {
            let w = reg.incursion_site_waves.get(&site_id).unwrap();
            assert_eq!(w.current_wave, w.total_waves);
            reg.incursions[&cid]
                .sites
                .iter()
                .find(|s| s.site_id == site_id)
                .unwrap()
                .escort_spawns
                .iter()
                .filter(|e| !e.destroyed)
                .map(|e| e.npc_ball_id)
                .collect()
        });
        assert!(!final_escorts.is_empty());

        // Kill boss BEFORE final-wave escorts: must record boss_destroyed=true without soft-locking
        let _ = ctx.systems.get_or_spawn(sys_id).remove_ball(boss_bid).await;
        let err = on_incursion_boss_destroyed(&ctx, sys_id, boss_bid, Some(pilot_id))
            .await
            .expect("site matched")
            .expect_err("site must not complete while final-wave escorts remain");
        assert_eq!(err.code, "IncursionSiteNpcsRemaining");

        let boss_flag = with_pve_special_reg(&ctx, |reg| {
            reg.incursions[&cid]
                .sites
                .iter()
                .find(|s| s.site_id == site_id)
                .unwrap()
                .boss_destroyed
        });
        assert!(boss_flag, "boss_destroyed must be recorded when boss dies on final wave");

        // Killing the remaining final-wave escorts must complete the site on the last escort kill
        let mut completion = None;
        for eid in final_escorts {
            let _ = ctx.systems.get_or_spawn(sys_id).remove_ball(eid).await;
            if let Some(res) = on_incursion_boss_destroyed(&ctx, sys_id, eid, Some(pilot_id)).await {
                completion = Some(res.expect("site completion on last escort kill"));
            }
        }
        let done = completion.expect("site must complete when last final-wave escort dies");
        assert_eq!(done.site_id, site_id);
    }

    #[tokio::test]
    async fn test_incursion_hq_sites_5171_and_5175_have_disjoint_npc_ball_ids() {
        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        ensure_special_sites_spawned(&ctx).await;

        let (cid, hq_sys_id, site_5171_id, site_5175_id, pos_5171) = with_pve_special_reg(&ctx, |reg| {
            let inc = reg.incursions.values().next().expect("seeded incursion");
            let s5171 = inc.sites.iter().find(|s| s.dungeon_id == 5171).expect("HQ site 5171");
            let s5175 = inc.sites.iter().find(|s| s.dungeon_id == 5175).expect("HQ site 5175");
            assert_eq!(s5171.solar_system_id, s5175.solar_system_id);
            (
                inc.constellation_id,
                s5171.solar_system_id,
                s5171.site_id.clone(),
                s5175.site_id.clone(),
                s5171.position,
            )
        });

        let (pilot_id, _) = spawn_fleet_pilot_on_grid(&ctx, 81, hq_sys_id, pos_5171).await;
        let boss_5171 = crate::common::clear_incursion_site_escort_waves(&ctx, hq_sys_id, &site_5171_id).await;
        let _ = ctx.systems.get_or_spawn(hq_sys_id).remove_ball(boss_5171).await;
        let res_5171 = on_incursion_boss_destroyed(&ctx, hq_sys_id, boss_5171, Some(pilot_id))
            .await
            .expect("site 5171 matched")
            .expect("site 5171 completed");
        assert_eq!(res_5171.site_id, site_5171_id);

        // Verify site 5175 in the same HQ system was untouched and has completely disjoint NPC ball IDs
        with_pve_special_reg(&ctx, |reg| {
            let inc = &reg.incursions[&cid];
            let s5171 = inc.sites.iter().find(|s| s.site_id == site_5171_id).unwrap();
            let s5175 = inc.sites.iter().find(|s| s.site_id == site_5175_id).unwrap();
            assert_eq!(s5171.completed_count, 1);
            assert_eq!(s5175.completed_count, 0);
            assert!(!s5175.boss_destroyed);
            assert!(s5175.escort_spawns.iter().all(|e| !e.destroyed));

            let ids_5171: std::collections::HashSet<i64> = std::iter::once(s5171.boss_npc_ball_id)
                .chain(s5171.escort_spawns.iter().map(|e| e.npc_ball_id))
                .collect();
            let ids_5175: std::collections::HashSet<i64> = std::iter::once(s5175.boss_npc_ball_id)
                .chain(s5175.escort_spawns.iter().map(|e| e.npc_ball_id))
                .collect();
            assert!(ids_5171.is_disjoint(&ids_5175), "HQ sites 5171 and 5175 must have disjoint NPC ball IDs");
        });
    }
}

pub(crate) mod pve_incursion_constellation_system_effects {
    // Authentic Incursion System, Payouts, Mothership Finale, Drifter Hives & Triglavian Sites.
    // behavioral verification integration tests for Phase 8 / PVE milestones.

    use eve_db::{FleetRecord, InventoryFlag, InventoryItemRecord};
    use eve_destiny::{Ball, DamageVector, MovementMode};
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_server::router::ServerContext;
    use eve_server::services::pve_incursion_sites::{
        get_incursion_effects_for_system, on_incursion_boss_destroyed,
    };
    use eve_server::services::pve_special_sites::{
        ensure_special_sites_spawned, get_drifter_hive_for_system, with_pve_special_reg,
    };
    use glam::DVec3;
    use std::collections::HashMap;

    async fn create_in_space_session(
        ctx: &ServerContext,
        session_id: u64,
        char_id: i64,
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

        sess.solar_system_id = Some(solar_system_id);
        sess.station_id = None;
        sess.structure_id = None;
        sess.ship_id = Some(ship_id);

        ctx.db.with_mut_snapshot(|snap| {
            let p1_template = snap.characters[&90_000_001].clone();
            let ch = snap.characters.entry(char_id).or_insert_with(|| p1_template);
            ch.character_id = char_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = solar_system_id;
            ch.active_ship_id = ship_id;

            snap.items.insert(
                ship_id,
                InventoryItemRecord {
                    item_id: ship_id,
                    type_id: ship_type_id,
                    owner_id: char_id,
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
    // 1. Constellation System Effects, Encounters & taleMgr World Event RPCs
    // ============================================================================
    #[tokio::test]
    async fn test_pve_incursion_constellation_system_effects_and_tales() {
        // parity: feat:pve.incursion-system
        eve_testkit::attest(&["feat:pve.incursion-system"]);

        let ctx = ServerContext::new();
        ensure_special_sites_spawned(&ctx).await;
        let kw = HashMap::new();

        let (target_cid, hq_sys_id, staging_sys_id) = with_pve_special_reg(&ctx, |reg| {
            let (cid, inc) = reg.incursions.iter().next().unwrap();
            (*cid, inc.hq_solar_system_id, inc.staging_solar_system_id)
        });
        let mut sess = create_in_space_session(&ctx, 8201, 90_000_001, hq_sys_id, 710_002_001, 587).await;

        // 1. V24 World Event RPCs via taleMgr
        let tales_rep = ctx
            .dispatch_rpc(&mut sess, "taleMgr", None, "GetGlobalWorldEventTales", &[], &kw, None)
            .await
            .expect("GetGlobalWorldEventTales");
        let PyRep::List(tales) = tales_rep else { panic!("Expected List of tales") };
        assert!(!tales.is_empty(), "Active incursion tales must be returned");

        let tale = tales
            .iter()
            .find(|t| t.dict_get("constellationID").and_then(|v| v.as_i64()) == Some(target_cid as i64))
            .expect("Target incursion tale must exist");
        assert_eq!(tale.dict_get("templateID").and_then(|v| v.as_i64()), Some(5158));
        assert_eq!(tale.dict_get("influence").and_then(|v| v.as_f64()), Some(1.0));
        assert_eq!(tale.dict_get("state").and_then(|v| v.as_str()), Some("Establishing"));
        assert_eq!(tale.dict_get("managerSolarSystemID").and_then(|v| v.as_i64()), Some(staging_sys_id as i64));
        assert_eq!(tale.dict_get("hasFinalEncounter").and_then(|v| v.as_bool()), Some(false));

        // Verify sceneTypesByLocationID mapping has staging and HQ
        let PyRep::Dict(scene_map) = tale.dict_get("sceneTypesByLocationID").expect("sceneTypesByLocationID") else { panic!("Expected Dict") };
        assert!(scene_map.iter().any(|(k, v)| k.as_i64() == Some(staging_sys_id as i64) && v.as_str() == Some("incursionStaging")));
        assert!(scene_map.iter().any(|(k, v)| k.as_i64() == Some(hq_sys_id as i64) && v.as_str() == Some("incursionHeavyInfestation")));

        // Verify sites list in tale contains staging, vanguard, assault, headquarters, mothership
        let PyRep::List(sites) = tale.dict_get("sites").expect("sites list") else { panic!("Expected List of sites") };
        assert!(sites.iter().any(|s| s.dict_get("tier").and_then(|v| v.as_str()) == Some("Staging")));
        assert!(sites.iter().any(|s| s.dict_get("tier").and_then(|v| v.as_str()) == Some("Vanguard")));
        assert!(sites.iter().any(|s| s.dict_get("tier").and_then(|v| v.as_str()) == Some("Assault")));
        assert!(sites.iter().any(|s| s.dict_get("tier").and_then(|v| v.as_str()) == Some("Headquarters")));
        let ms_site = sites.iter().find(|s| s.dict_get("tier").and_then(|v| v.as_str()) == Some("Mothership")).expect("Mothership site");
        assert_eq!(ms_site.dict_get("available").and_then(|v| v.as_bool()), Some(false), "Mothership locked at 1.0 influence");

        // 2. taleMgr.get_active_tales_by_template
        let active_tales = ctx
            .dispatch_rpc(&mut sess, "taleMgr", None, "get_active_tales_by_template", &[PyRep::Integer(5158)], &kw, None)
            .await
            .expect("get_active_tales_by_template");
        let PyRep::List(active_list) = active_tales else { panic!("Expected List") };
        assert!(!active_list.is_empty(), "Active tales list by template must not be empty");

        // 3. Sansha Encounter Compositions & Site Distribution
        with_pve_special_reg(&ctx, |reg| {
            let inc = &reg.incursions[&target_cid];
            assert_eq!(inc.sites.len(), 8, "Constellation must seed staging, 2 vanguard, 2 assault, 2 HQ, mothership");
            for site in &inc.sites {
                assert!(site.boss_type_id > 0, "Site must have authentic Sansha boss type ID");
                assert!(!site.escort_spawns.is_empty(), "Site must have Sansha escort spawns");
            }
        });

        // 4. Incursion System Effects & Dogma Attributes
        let eff = get_incursion_effects_for_system(&ctx, hq_sys_id).expect("incursion effects");
        assert_eq!(eff.influence, 1.0);
        assert!(eff.cyno_jammed);
        assert_eq!(eff.shield_penalty, 0.50);
        assert_eq!(eff.damage_penalty, 0.50);

        let mut ship_attrs = HashMap::from([(263, 1000.0), (265, 1000.0), (482, 1000.0), (64, 1.0)]);
        eve_server::services::inventory_fitting::fitting_scanners_superweapons::apply_active_dbuffs_and_system_effects_to_ship_attrs(&ctx, 710_002_001, &mut ship_attrs);
        assert_eq!(ship_attrs[&263], 500.0, "50% shield penalty at 1.0 influence");
        assert_eq!(ship_attrs[&265], 500.0, "50% armor penalty at 1.0 influence");
        assert_eq!(ship_attrs[&482], 500.0, "50% capacitor penalty at 1.0 influence");
        assert_eq!(ship_attrs[&64], 0.5, "50% damage penalty at 1.0 influence");

        // 5. Incursion client RPC contracts & rejection of non-catalog RPCs
        let lp_log = ctx
            .dispatch_rpc(&mut sess, "incursion", None, "GetIncursionLPLog", &[], &kw, None)
            .await
            .expect("GetIncursionLPLog");
        assert_eq!(lp_log, PyRep::List(vec![]));

        let update_lp = ctx
            .dispatch_rpc(&mut sess, "incursion", None, "UpdateIncursionLPLog", &[PyRep::String("INC-SYS-5158".into())], &kw, None)
            .await
            .expect("UpdateIncursionLPLog");
        assert_eq!(update_lp, PyRep::None);

        for non_client_rpc in ["GetIncursionSystemEffects", "GetAllIncursionSites"] {
            let err = ctx
                .dispatch_rpc(&mut sess, "incursion", None, non_client_rpc, &[PyRep::Integer(hq_sys_id as i64)], &kw, None)
                .await
                .expect_err("non-client RPC must fail");
            assert_eq!(err.code, "UnimplementedRPC");
        }
    }

    // ============================================================================
    // 2. Site Payouts Scaled by Fleet Size & Constellation Influence
    // ============================================================================
    #[tokio::test]
    async fn test_pve_incursion_multiwave_fleet_payout_and_influence() {
        // parity: feat:pve.incursion-payout
        eve_testkit::attest(&["feat:pve.incursion-payout"]);

        let ctx = ServerContext::new();
        ensure_special_sites_spawned(&ctx).await;
        let kw = HashMap::new();

        let (target_cid, vg_sys_id) = with_pve_special_reg(&ctx, |reg| {
            let (cid, inc) = reg.incursions.iter().next().unwrap();
            (*cid, inc.vanguard_solar_system_ids[0])
        });

        let (boss_bid, site_id, site_pos) = with_pve_special_reg(&ctx, |reg| {
            let inc = &reg.incursions[&target_cid];
            let site = inc.sites.iter().find(|s| s.tier == "Vanguard").unwrap();
            (site.boss_npc_ball_id, site.site_id.clone(), site.position)
        });

        let vg_sys = ctx.systems.get_or_spawn(vg_sys_id);
        let mut sess = create_in_space_session(&ctx, 8202, 90_000_001, vg_sys_id, 710_002_002, 587).await;
        vg_sys.set_ball_position(710_002_002, site_pos).await;

        // 5-pilot fleet (optimal size 5 for Vanguard)
        let pids = [90_000_001, 90_000_002, 90_000_003, 90_000_004, 90_000_005];
        ctx.db.with_mut_snapshot(|snap| {
            let p1_template = snap.characters[&90_000_001].clone();
            for &pid in &pids {
                let ch = snap.characters.entry(pid).or_insert_with(|| p1_template.clone());
                ch.character_id = pid;
                ch.solar_system_id = vg_sys_id;
                ch.station_id = None;
                ch.structure_id = None;
                ch.fleet_id = Some(941_001);
                ch.active_ship_id = 710_000_000 + pid;
            }
            snap.fleets.insert(941_001, FleetRecord {
                fleet_id: 941_001,
                boss_id: 90_000_001,
                members: pids.to_vec(),
                ..Default::default()
            });
        });

        for &pid in &pids {
            let ship_id = 710_000_000 + pid;
            let _ = vg_sys.spawn_ball(Ball {
                id: ship_id,
                type_id: 587,
                position: site_pos + DVec3::new((pid % 500) as f64, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 365.0,
                inertia: 1.0,
                mass: 1_067_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            }).await;
        }

        let p_inits: HashMap<i64, (i64, i64)> = pids
            .iter()
            .map(|&pid| {
                (
                    pid,
                    (
                        ctx.db.with_snapshot(|s| s.characters[&pid].balance.0),
                        ctx.db.with_snapshot(|s| s.characters[&pid].loyalty_points.get(&1_000_125).copied().unwrap_or(0)),
                    ),
                )
            })
            .collect();

        // Kill boss in space with attacker attribution after clearing escort waves
        crate::common::clear_incursion_site_escort_waves(&ctx, vg_sys_id, &site_id).await;
        let (_, dead, _) = vg_sys.apply_damage_with_attacker(
            boss_bid,
            DamageVector { em: 100_000.0, thermal: 100_000.0, kinetic: 100_000.0, explosive: 100_000.0 },
            Some(90_000_001),
        ).await.unwrap().unwrap();
        assert!(dead, "Boss ball must be destroyed");

        // Influence was 1.0; delta for Vanguard is 0.05 -> new influence is 0.95
        // influence_scale = (1.0 - 0.95 * 0.50) = 0.525.
        // 5 pilots in Vanguard -> fleet_scale = 1.0. Total scale = 0.525.
        // base_isk = 10,395,000 -> isk = 5,457,375 ISK. base_lp = 1,400 -> lp = 735 LP.
        for &pid in &pids {
            let (bal, lp) = ctx.db.with_snapshot(|s| (s.characters[&pid].balance.0, s.characters[&pid].loyalty_points.get(&1_000_125).copied().unwrap_or(0)));
            assert_eq!(bal - p_inits[&pid].0, 5_457_375 * 100, "All fleet members must receive identical payout");
            assert_eq!(lp - p_inits[&pid].1, 735, "All fleet members must receive identical LP");
        }

        let journal = ctx.db.with_snapshot(|s| s.wallet_journal.clone());
        assert!(journal.iter().any(|j| j.character_id == 90_000_001 && j.entry_type == "CorporateRewardPayout"));

        let logs_rep = ctx.dispatch_rpc(&mut sess, "incursion", None, "GetIncursionLPLog", &[], &kw, None).await.expect("GetIncursionLPLog");
        let PyRep::List(logs) = logs_rep else { panic!("Expected List") };
        assert!(!logs.is_empty());
        assert_eq!(logs[0].dict_get("iskEarned").and_then(|v| v.as_i64()), Some(5_457_375));

        // Next wave boss respawned once escort waves clear
        let next_boss_bid = crate::common::clear_incursion_site_escort_waves(&ctx, vg_sys_id, &site_id).await;
        assert!(vg_sys.get_ball_snapshot(next_boss_bid).await.unwrap().is_some());

        // Double payout race concurrency protection
        let (res1, res2) = tokio::join!(
            eve_server::services::pve_incursion_sites::process_incursion_site_completion_and_payout(&ctx, Some(90_000_001), None, vg_sys_id, &site_id, None),
            eve_server::services::pve_incursion_sites::process_incursion_site_completion_and_payout(&ctx, Some(90_000_001), None, vg_sys_id, &site_id, None),
        );
        assert_eq!([res1.is_ok(), res2.is_ok()].into_iter().filter(|&x| x).count(), 0, "Boss is alive in space, manual trigger without boss death fails");
    }

    // ============================================================================
    // 3. Kundalini Manifest Mothership Finale & Incursion Conclusion
    // ============================================================================
    #[tokio::test]
    async fn test_pve_incursion_mothership_hq_unlock_and_conclusion() {
        // parity: feat:pve.incursion-mothership
        eve_testkit::attest(&["feat:pve.incursion-mothership"]);

        let ctx = ServerContext::new();
        ensure_special_sites_spawned(&ctx).await;

        let (target_cid, hq_sys_id) = with_pve_special_reg(&ctx, |reg| {
            let (cid, inc) = reg.incursions.iter().next().unwrap();
            (*cid, inc.hq_solar_system_id)
        });
        let _sess = create_in_space_session(&ctx, 8203, 90_000_001, hq_sys_id, 710_002_003, 587).await;
        let hq_sys = ctx.systems.get_or_spawn(hq_sys_id);

        // Initial check: Mothership site locked at 1.0 influence
        let ms_boss_bid_init = with_pve_special_reg(&ctx, |reg| {
            reg.incursions[&target_cid].sites.iter().find(|s| s.tier == "Mothership").unwrap().boss_npc_ball_id
        });
        assert!(hq_sys.get_ball_snapshot(ms_boss_bid_init).await.unwrap().is_none());

        let lock_attempt = on_incursion_boss_destroyed(&ctx, hq_sys_id, ms_boss_bid_init, Some(90_000_001)).await;
        assert_eq!(lock_attempt.unwrap().unwrap_err().code, "MothershipSiteLocked");

        // Complete HQ site waves to bring influence from 1.0 down to 0.0
        let (hq_site_id, hq_pos) = with_pve_special_reg(&ctx, |reg| {
            let s = reg.incursions[&target_cid].sites.iter().find(|s| s.tier == "Headquarters").unwrap();
            (s.site_id.clone(), s.position)
        });
        hq_sys.set_ball_position(710_002_003, hq_pos).await;

        for _wave in 0..4 {
            let hq_boss_bid = crate::common::clear_incursion_site_escort_waves(&ctx, hq_sys_id, &hq_site_id).await;
            assert!(hq_sys.get_ball_snapshot(hq_boss_bid).await.unwrap().is_some());
            let (_, dead, _) = hq_sys.apply_damage_with_attacker(
                hq_boss_bid,
                DamageVector { em: 100_000.0, thermal: 100_000.0, kinetic: 100_000.0, explosive: 100_000.0 },
                Some(90_000_001),
            ).await.unwrap().unwrap();
            assert!(dead);
        }

        // Mothership unlocks and spawns
        let (inf_after_hq, ms_spawned, ms_site_id, ms_boss_bid, ms_pos) = with_pve_special_reg(&ctx, |reg| {
            let inc = &reg.incursions[&target_cid];
            let ms = inc.sites.iter().find(|s| s.tier == "Mothership").unwrap();
            (inc.influence, inc.mothership_spawned, ms.site_id.clone(), ms.boss_npc_ball_id, ms.position)
        });
        assert_eq!(inf_after_hq, 0.0);
        assert!(ms_spawned);

        // Verify The Kundalini Manifest ball exists in space with type_id 2192
        let ms_ball_snap = hq_sys.get_ball_snapshot(ms_boss_bid).await.unwrap().expect("Kundalini Manifest spawned");
        assert_eq!(ms_ball_snap.1.type_id, 2192);

        let escort_bids = with_pve_special_reg(&ctx, |reg| {
            reg.incursions[&target_cid].sites.iter().find(|s| s.tier == "Mothership").unwrap()
                .escort_spawns.iter().map(|e| (e.npc_ball_id, e.type_id)).collect::<Vec<_>>()
        });
        assert!(!escort_bids.is_empty());
        for (e_bid, e_tid) in &escort_bids {
            let e_snap = hq_sys.get_ball_snapshot(*e_bid).await.unwrap().expect("Escort ball spawned");
            assert_eq!(e_snap.1.type_id, *e_tid);
        }

        // Kill The Kundalini Manifest in space after clearing escorts
        crate::common::clear_incursion_site_escort_waves(&ctx, hq_sys_id, &ms_site_id).await;
        hq_sys.set_ball_position(710_002_003, ms_pos).await;
        let pre_ms_bal = ctx.db.with_snapshot(|s| s.characters[&90_000_001].balance.0);
        let (_, ms_dead, _) = hq_sys.apply_damage_with_attacker(
            ms_boss_bid,
            DamageVector { em: 100_000.0, thermal: 100_000.0, kinetic: 100_000.0, explosive: 100_000.0 },
            Some(90_000_001),
        ).await.unwrap().unwrap();
        assert!(ms_dead);

        // Verify conclusion (enters Withdrawing on timer after mothership dies)
        let (final_state, ms_defeated) = with_pve_special_reg(&ctx, |reg| {
            let inc = &reg.incursions[&target_cid];
            (inc.state.clone(), inc.mothership_defeated)
        });
        assert_eq!(final_state, "Withdrawing");
        assert!(ms_defeated);

        // Escorts removed from space
        for (e_bid, _) in escort_bids {
            assert!(hq_sys.get_ball_snapshot(e_bid).await.unwrap().is_none());
        }

        // Payout disbursed
        let post_ms_bal = ctx.db.with_snapshot(|s| s.characters[&90_000_001].balance.0);
        assert_eq!((post_ms_bal - pre_ms_bal) / 100, 12_600_000);

        // Penalties withdrawn
        assert!(get_incursion_effects_for_system(&ctx, hq_sys_id).is_none());
    }

    // ============================================================================
    // 4. Drifter Hives Multi-room Progression & Authentic Loot
    // ============================================================================
    #[tokio::test]
    async fn test_pve_drifter_hives_multiroom_progression_and_loot() {
        // parity: feat:pve.drifter-hives
        eve_testkit::attest(&["feat:pve.drifter-hives"]);

        let ctx = ServerContext::new();
        ensure_special_sites_spawned(&ctx).await;
        let kw = HashMap::new();

        // Verify 5 Drifter Hives initialized across wormhole systems 31_000_001..=31_000_005
        let expected_elements = [
            (31_000_001, 34_557, "Barbican Hive"),
            (31_000_002, 34_558, "Vidette Hive"),
            (31_000_003, 34_559, "Conflux Hive"),
            (31_000_004, 34_560, "Redoubt Hive"),
            (31_000_005, 34_556, "Sentinel Hive"),
        ];

        for &(sys_id, elem_tid, name) in &expected_elements {
            let hive = get_drifter_hive_for_system(&ctx, sys_id).expect("hive must exist");
            assert_eq!(hive.dungeon_name, name);
            assert_eq!(hive.element_type_id, elem_tid);
            assert_eq!(hive.total_rooms, 3);
            assert_eq!(hive.rooms.len(), 3);
            assert_eq!(hive.current_room, 1);
            assert!(!hive.cleared);

            // Verify authentic SDE NPC types in rooms: Cruisers (47958/47959), Battleships (47956/47957), Warden (37473)
            assert!(hive.rooms[0].npc_type_ids.contains(&47958) || hive.rooms[0].npc_type_ids.contains(&47959));
            assert!(hive.rooms[1].npc_type_ids.contains(&47956) || hive.rooms[1].npc_type_ids.contains(&47957));
            assert!(hive.rooms[2].npc_type_ids.contains(&37473));
        }

        // Step-by-step room progression in Barbican Hive (31_000_001)
        let char_id = 90_000_001;
        let mut sess = create_in_space_session(&ctx, 8204, char_id, 31_000_001, 710_002_004, 587).await;
        let wh_sys = ctx.systems.get_or_spawn(31_000_001);
        let hive_init = get_drifter_hive_for_system(&ctx, 31_000_001).unwrap();

        // Destroy Room 1 cruisers
        for &npc_bid in &hive_init.rooms[0].npc_ball_ids {
            let _ = wh_sys.apply_damage_with_attacker(
                npc_bid,
                DamageVector { em: 50_000.0, thermal: 50_000.0, kinetic: 50_000.0, explosive: 50_000.0 },
                Some(char_id),
            ).await;
        }

        // Room 1 wave clearance in space naturally advances progression to Room 2
        let hive_after_r1 = get_drifter_hive_for_system(&ctx, 31_000_001).unwrap();
        assert_eq!(hive_after_r1.current_room, 2);
        assert!(hive_after_r1.rooms[0].cleared);
        assert!(!hive_after_r1.rooms[1].cleared);

        // Verify Room 2 battleships were automatically spawned into space
        for &npc_bid in &hive_after_r1.rooms[1].npc_ball_ids {
            let snap = wh_sys.get_ball_snapshot(npc_bid).await.unwrap();
            assert!(snap.is_some(), "Room 2 battleship must be automatically spawned");
        }

        // Acceleration gate (client RPC keeper.EnterNextDungeonRoom) refuses to jump while Room 2 wave is alive
        let gate_blocked = ctx.dispatch_rpc(&mut sess, "keeper", None, "EnterNextDungeonRoom", &[PyRep::Integer(31_000_001)], &kw, None).await;
        assert!(gate_blocked.is_err(), "Acceleration gate must be gated by Room 2 wave-clear");
        assert_eq!(get_drifter_hive_for_system(&ctx, 31_000_001).unwrap().current_room, 2);

        // Destroy Room 2 battleships in space
        for &npc_bid in &hive_after_r1.rooms[1].npc_ball_ids {
            let _ = wh_sys.apply_damage_with_attacker(
                npc_bid,
                DamageVector { em: 50_000.0, thermal: 50_000.0, kinetic: 50_000.0, explosive: 50_000.0 },
                Some(char_id),
            ).await;
        }

        // Room 2 wave clearance in space naturally advances progression to Room 3
        let hive_after_r2 = get_drifter_hive_for_system(&ctx, 31_000_001).unwrap();
        assert_eq!(hive_after_r2.current_room, 3);
        assert!(hive_after_r2.rooms[1].cleared);
        assert!(!hive_after_r2.rooms[2].cleared);

        // Verify Artemis Tyrannos Warden was automatically spawned into space
        let warden_bid = hive_init.warden_npc_ball_id;
        let warden_snap = wh_sys.get_ball_snapshot(warden_bid).await.unwrap();
        assert!(warden_snap.is_some(), "Artemis Tyrannos Warden must be automatically spawned");

        // Artemis Tyrannos Warden space destruction in final room
        let (_, warden_dead, _) = wh_sys.apply_damage_with_attacker(
            warden_bid,
            DamageVector { em: 50_000.0, thermal: 50_000.0, kinetic: 50_000.0, explosive: 50_000.0 },
            Some(char_id),
        ).await.unwrap().unwrap();
        assert!(warden_dead, "Artemis Tyrannos Warden destroyed");

        // Hive state completed and final room marked cleared
        let hive_final = get_drifter_hive_for_system(&ctx, 31_000_001).unwrap();
        assert!(hive_final.cleared, "Hive must be marked cleared");
        assert_eq!(hive_final.current_room, 3);
        assert!(hive_final.rooms[2].cleared);

        // Acceleration gate on a hive cleared by the in-space warden kill rejects re-completion (no double loot)
        let gate_done = ctx.dispatch_rpc(&mut sess, "keeper", None, "EnterNextDungeonRoom", &[PyRep::Integer(31_000_001)], &kw, None).await;
        assert!(gate_done.is_err(), "Cleared hive must reject further gate activation");

        // Verify authentic loot deposited: Barbican Element (34557) & Sleeper Data Library (30745)
        let has_element = ctx.db.with_snapshot(|s| {
            s.items.values().any(|i| i.type_id == 34_557 && i.owner_id == char_id)
        });
        let has_blue_loot = ctx.db.with_snapshot(|s| {
            s.items.values().any(|i| i.type_id == 30_745 && i.owner_id == char_id)
        });
        assert!(has_element, "Barbican Element (34557) must be awarded");
        assert!(has_blue_loot, "Sleeper Data Library (30745) must be awarded");

        // GetTriglavianSiteInstances returns clean empty list in wormhole systems
        let trig_wh = ctx.dispatch_rpc(&mut sess, "dungeonInstanceCacheMgr", None, "GetTriglavianSiteInstances", &[PyRep::Integer(31_000_001)], &kw, None).await.expect("GetTriglavianSiteInstances");
        let PyRep::Dict(trig_wh_dict) = trig_wh else { panic!("Expected Dict") };
        let PyRep::Dict(trig_wh_dng) = &trig_wh_dict[0].1 else { panic!("Expected inner Dict") };
        assert!(trig_wh_dng.is_empty(), "Drifter Hive must not overload Triglavian site instances");
    }

    // ============================================================================
    // 5. Triglavian Sites & Minor Victory Systems
    // ============================================================================
    #[tokio::test]
    async fn test_pve_triglavian_sites_and_minor_victory_systems() {
        // parity: feat:pve.triglavian-sites, rpc:map.GetEdencomFortressSystems, rpc:map.GetEdencomMinorVictorySystems
        // parity: rpc:map.GetTriglavianMinorVictorySystems, rpc:dungeonInstanceCacheMgr.GetTriglavianSiteInstances
        eve_testkit::attest(&[
            "feat:pve.triglavian-sites",
            "rpc:map.GetEdencomFortressSystems",
            "rpc:map.GetEdencomMinorVictorySystems",
            "rpc:map.GetTriglavianMinorVictorySystems",
            "rpc:dungeonInstanceCacheMgr.GetTriglavianSiteInstances",
        ]);

        let ctx = ServerContext::new();
        ensure_special_sites_spawned(&ctx).await;
        let kw = HashMap::new();
        let char_id = 90_000_001;
        let mut sess = create_in_space_session(&ctx, 8205, char_id, 30_000_142, 710_002_005, 587).await;

        // 1. Permanent Chapter 3 invasion systems
        let fort_sys = ctx.dispatch_rpc(&mut sess, "map", None, "GetEdencomFortressSystems", &[], &kw, None).await.expect("GetEdencomFortressSystems");
        let PyRep::List(f_list) = fort_sys else { panic!("Expected List") };
        assert_eq!(f_list.len(), 53, "All 53 authentic Fortress systems returned");
        assert!(f_list.contains(&PyRep::Integer(30_002_253)), "Arshat is a Fortress system");

        let eden_vic = ctx.dispatch_rpc(&mut sess, "map", None, "GetEdencomMinorVictorySystems", &[], &kw, None).await.expect("GetEdencomMinorVictorySystems");
        let PyRep::List(e_list) = eden_vic else { panic!("Expected List") };
        assert_eq!(e_list.len(), 84, "All 84 authentic EDENCOM Minor Victory systems returned");
        assert!(e_list.contains(&PyRep::Integer(30_002_048)) && e_list.contains(&PyRep::Integer(30_003_074)));

        let vic_systems = ctx.dispatch_rpc(&mut sess, "map", None, "GetTriglavianMinorVictorySystems", &[], &kw, None).await.expect("GetTriglavianMinorVictorySystems");
        let PyRep::List(v_list) = vic_systems else { panic!("Expected List") };
        assert_eq!(v_list.len(), 28, "All 28 authentic Triglavian Minor Victory systems returned");
        assert!(v_list.contains(&PyRep::Integer(30_003_076)), "Gammel is seeded as permanent Triglavian Minor Victory system");
        assert!(with_pve_special_reg(&ctx, |reg| reg.triglavian_sites.keys().all(|&s| ctx.sde.solar_systems.get(&s).map(|ss| ss.region_id) != Some(10_000_070))));
        assert!(!with_pve_special_reg(&ctx, |reg| reg.triglavian_sites.contains_key(&30_002_253)), "Fortress systems have no invasion sites");

        // 2. Pure read query in Jita (30000142) returns empty without fabricating sites
        let jita_trig = ctx.dispatch_rpc(&mut sess, "dungeonInstanceCacheMgr", None, "GetTriglavianSiteInstances", &[PyRep::Integer(30_000_142)], &kw, None).await.expect("GetTriglavianSiteInstances in Jita");
        let PyRep::Dict(j_dict) = jita_trig else { panic!("Expected Dict") };
        let PyRep::Dict(j_dng) = &j_dict[0].1 else { panic!("Expected inner Dict") };
        assert!(j_dng.is_empty(), "Jita must not fabricate Triglavian sites");

        // 3. EDENCOM minor victory system Bei (30002048) contains authentic EDENCOM sites & ships
        let bei_trig = ctx.dispatch_rpc(&mut sess, "dungeonInstanceCacheMgr", None, "GetTriglavianSiteInstances", &[PyRep::Integer(30_002_048)], &kw, None).await.expect("GetTriglavianSiteInstances in Bei");
        let PyRep::Dict(b_dict) = bei_trig else { panic!("Expected Dict") };
        let PyRep::Dict(b_dng) = &b_dict[0].1 else { panic!("Expected inner Dict") };
        assert_eq!(b_dng.len(), 2, "Bei must contain 2 EDENCOM site instances");
        with_pve_special_reg(&ctx, |reg| {
            let site = &reg.triglavian_sites[&30_002_048][0];
            assert_eq!(site.boss_type_id, 52_963, "Boss must be Republic Fleet Tempest (cat 11)");
            assert!(site.escort_type_ids.contains(&52_961)); // Republic Fleet Hurricane
            assert!(site.escort_type_ids.contains(&52_957)); // Republic Fleet Claw
        });

        // 4. Triglavian minor victory system Gammel (30003076) contains authentic Triglavian sites & ships
        let gammel_sys = ctx.systems.get_or_spawn(30_003_076);
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) { ch.solar_system_id = 30_003_076; }
        });
        sess.solar_system_id = Some(30_003_076);

        let gammel_trig = ctx.dispatch_rpc(&mut sess, "dungeonInstanceCacheMgr", None, "GetTriglavianSiteInstances", &[PyRep::Integer(30_003_076)], &kw, None).await.expect("GetTriglavianSiteInstances in Gammel");
        let PyRep::Dict(g_dict) = gammel_trig else { panic!("Expected Dict") };
        let PyRep::Dict(g_dng) = &g_dict[0].1 else { panic!("Expected inner Dict") };
        assert_eq!(g_dng.len(), 2, "Gammel must contain 2 Triglavian site instances");

        let (boss_bid, beacon_bid, esc_bid) = with_pve_special_reg(&ctx, |reg| {
            let site = &reg.triglavian_sites[&30_003_076][0];
            assert_eq!(site.boss_type_id, 48_122, "Boss must be authentic Striking Leshak");
            assert!(site.escort_type_ids.contains(&48_086)); // Ghosting Damavik
            assert!(site.escort_type_ids.contains(&48_092)); // Striking Damavik
            let esc = reg.triglavian_sites[&30_003_076][1].escort_ball_ids[0];
            (site.boss_npc_ball_id, site.beacon_ball_id, esc)
        });

        // Verify boss and beacon are automatically spawned in space with CombatState
        let boss_snap = gammel_sys.get_ball_snapshot(boss_bid).await.unwrap();
        assert!(boss_snap.is_some(), "Boss must be automatically spawned in space");
        let boss_cs = gammel_sys.get_combat_state(boss_bid).await.unwrap();
        assert!(boss_cs.is_some() && boss_cs.unwrap().max_structure_hp > 0.0, "Boss has CombatState");
        let beacon_snap = gammel_sys.get_ball_snapshot(beacon_bid).await.unwrap();
        assert!(beacon_snap.is_some(), "Beacon must be automatically spawned in space");
        let gammel_balls = gammel_sys.list_balls().await.unwrap();
        assert!(!gammel_balls.iter().any(|b| b.type_id == 10_962 && b.position.length() == 0.0), "No sentinel marker ball");
        assert!(gammel_sys.get_ball_snapshot(esc_bid).await.unwrap().is_some(), "Escort must be spawned before destruction");
        let (_, esc_dead, _) = gammel_sys.apply_damage(esc_bid, DamageVector { em: 50_000.0, thermal: 50_000.0, kinetic: 50_000.0, explosive: 50_000.0 }).await.unwrap().unwrap();
        assert!(esc_dead, "Escort must be destroyed by lethal damage");
        let _ = ctx.dispatch_rpc(&mut sess, "dungeonInstanceCacheMgr", None, "GetTriglavianSiteInstances", &[PyRep::Integer(30_003_076)], &kw, None).await;
        assert!(gammel_sys.get_ball_snapshot(esc_bid).await.unwrap().is_none(), "Destroyed escort must not be re-spawned");

        let pre_bal = ctx.db.with_snapshot(|s| s.characters[&char_id].balance.0);

        // Destroy Triglavian Leshak boss in space with killer attribution (automatically spawned)
        let (_, dead, _) = gammel_sys.apply_damage_with_attacker(
            boss_bid,
            DamageVector { em: 50_000.0, thermal: 50_000.0, kinetic: 50_000.0, explosive: 50_000.0 },
            Some(char_id),
        ).await.unwrap().unwrap();
        assert!(dead, "Triglavian boss destroyed");

        // Verify site completion reflected in cache query (completed instance removed)
        let trig_after = ctx
            .dispatch_rpc(&mut sess, "dungeonInstanceCacheMgr", None, "GetTriglavianSiteInstances", &[PyRep::Integer(30_003_076)], &kw, None)
            .await
            .expect("GetTriglavianSiteInstances after boss kill");
        let PyRep::Dict(after_dict) = trig_after else { panic!("Expected Dict") };
        let PyRep::Dict(after_dng) = &after_dict[0].1 else { panic!("Expected inner Dict") };
        assert_eq!(after_dng.len(), 1, "Completed site instance removed from active instances");

        // Verify rewards: ISK payout, Sealed Survey Database (48121) cargo drop, and dual standings change
        let post_bal = ctx.db.with_snapshot(|s| s.characters[&char_id].balance.0);
        assert!(post_bal > pre_bal, "Pilot must receive ISK bounty for Triglavian site completion");

        let has_survey_db = ctx.db.with_snapshot(|s| {
            s.items.values().any(|i| i.type_id == 48_121 && i.owner_id == char_id)
        });
        assert!(has_survey_db, "Sealed Survey Database (48121) cargo drop must be awarded");

        let (trig_standing, edencom_standing) = ctx.db.with_snapshot(|snap| {
            let ch = &snap.characters[&char_id];
            (ch.standings.get(&500_026).copied().unwrap_or(0.0), ch.standings.get(&500_027).copied().unwrap_or(0.0))
        });
        assert!(edencom_standing > 0.0, "EDENCOM standing must increase (+0.15)");
        assert!(trig_standing < 0.0, "Triglavian standing must decrease (-0.15)");
    }

    #[tokio::test]
    async fn test_p1_track_d_regressions_issues_12_55_56_57() {
        let ctx1 = ServerContext::new();
        let ctx2 = ServerContext::new();
        let kw = HashMap::new();

        // 1. Issue #55: ServerContext-scoped tactical and session-change timer state isolation
        eve_server::services::space_destiny::record_session_change_timer(&ctx1, 90_000_001, ctx1.now_unix_secs(), 30_000_142, 1, "jump");
        assert!(eve_server::services::space_destiny::get_session_change_timer(&ctx1, 90_000_001).is_some());
        assert!(eve_server::services::space_destiny::get_session_change_timer(&ctx2, 90_000_001).is_none());

        ctx1.tactical_state
            .write()
            .unwrap()
            .lance_caster_lock_until
            .insert((ctx1.instance_id as usize, 750_000_101), ctx1.now_unix_secs() + 300);
        assert!(eve_server::services::tactical_modules::check_ship_navigation_lock(&ctx1, 750_000_101, "warp").is_err());
        assert!(eve_server::services::tactical_modules::check_ship_navigation_lock(&ctx2, 750_000_101, "warp").is_ok());

        // 2. Issue #56: Probe scan system scoping, coordinate/destination redaction below 1.0, and per-character scan progress
        ctx1.db.with_mut_snapshot(|snap| {
            if let Some(ch2) = snap.characters.get_mut(&90_000_002) {
                ch2.user_id = 1;
            }
        });
        let mut sess_a = create_in_space_session(&ctx1, 9501, 90_000_001, 30_000_142, 750_000_201, 587).await;

        // Cross-system scan query rejected
        let remote_err = ctx1
            .dispatch_rpc(&mut sess_a, "scanMgr", None, "GetSystemScanMgr", &[PyRep::Integer(30_002_187)], &kw, None)
            .await
            .expect_err("Remote solar system scan query must be denied");
        assert_eq!(remote_err.code, "AccessDenied");

        // Seed an unscanned signature (strength 0.0) with a destination system
        let now_u = ctx1.now_unix();
        ctx1.db.with_mut_snapshot(|snap| {
            snap.cosmic_signatures.entry(30_000_142).or_default().push(eve_db::CosmicSignatureRecord {
                signature_id: "SEC-56".into(),
                solar_system_id: 30_000_142,
                sig_group: "Wormhole".into(),
                type_name: "Unstable Wormhole".into(),
                signal_strength: 0.0,
                position: [123_456.0, 234_567.0, 345_678.0],
                wormhole_type_id: Some(30_643),
                destination_system_id: Some(31_000_005),
                remaining_mass_kg: 2_000_000_000.0,
                max_mass_kg: 2_000_000_000.0,
                max_jump_mass_kg: 300_000_000.0,
                created_unix: now_u,
                lifetime_seconds: 86_400,
                collapsed: false,
            });
        });

        // Unscanned signature redacts coordinates, group, type, and destinationSystemID
        let scan_unscanned = ctx1
            .dispatch_rpc(&mut sess_a, "scanMgr", None, "GetSystemScanMgr", &[PyRep::Integer(30_000_142)], &kw, None)
            .await
            .expect("GetSystemScanMgr");
        let Some(PyRep::List(sigs_a0)) = scan_unscanned.dict_get("signatures") else { panic!("signatures list") };
        let sec_sig_a0 = sigs_a0.iter().find(|s| s.dict_get("signatureID") == Some(&PyRep::String("SEC-56".into()))).expect("SEC-56");
        assert_eq!(sec_sig_a0.dict_get("x"), Some(&PyRep::None));
        assert_eq!(sec_sig_a0.dict_get("destinationSystemID"), Some(&PyRep::None));
        assert_eq!(sec_sig_a0.dict_get("group"), Some(&PyRep::String("Unknown".into())));

        // Record 100% scan progress for Pilot A only; Pilot B still sees 0.0 and redacted coordinates
        ctx1.record_character_scan_progress(90_000_001, 30_000_142, "SEC-56", 1.0);
        let scan_a1 = ctx1
            .dispatch_rpc(&mut sess_a, "scanMgr", None, "GetSystemScanMgr", &[PyRep::Integer(30_000_142)], &kw, None)
            .await
            .unwrap();
        let Some(PyRep::List(sigs_a1)) = scan_a1.dict_get("signatures") else { panic!() };
        let sec_sig_a1 = sigs_a1.iter().find(|s| s.dict_get("signatureID") == Some(&PyRep::String("SEC-56".into()))).unwrap();
        assert_eq!(sec_sig_a1.dict_get("x"), Some(&PyRep::Real(123_456.0)));
        assert_eq!(sec_sig_a1.dict_get("destinationSystemID"), Some(&PyRep::Integer(31_000_005)));

        let mut sess_b = create_in_space_session(&ctx1, 9502, 90_000_002, 30_000_142, 750_000_202, 587).await;
        let scan_b1 = ctx1
            .dispatch_rpc(&mut sess_b, "scanMgr", None, "GetSystemScanMgr", &[PyRep::Integer(30_000_142)], &kw, None)
            .await
            .unwrap();
        let Some(PyRep::List(sigs_b1)) = scan_b1.dict_get("signatures") else { panic!() };
        let sec_sig_b1 = sigs_b1.iter().find(|s| s.dict_get("signatureID") == Some(&PyRep::String("SEC-56".into()))).unwrap();
        assert_eq!(sec_sig_b1.dict_get("x"), Some(&PyRep::None));
        assert_eq!(sec_sig_b1.dict_get("destinationSystemID"), Some(&PyRep::None));

        // 3. Issue #57: Drifter Hive NPCs have SDE-derived CombatState (> default 450/450/350 HP)
        ensure_special_sites_spawned(&ctx1).await;
        let hive = get_drifter_hive_for_system(&ctx1, 31_000_001).expect("Barbican hive");
        let wh_sys = ctx1.systems.get_or_spawn(31_000_001);
        let first_npc_bid = hive.rooms[0].npc_ball_ids[0];
        let npc_cs = wh_sys.get_combat_state(first_npc_bid).await.unwrap().expect("Drifter NPC must have CombatState");
        assert!(npc_cs.max_shield_hp >= 600.0 && npc_cs.max_armor_hp >= 600.0 && npc_cs.max_structure_hp >= 600.0);

        // Homefront completion ignores spoofed fleet_count when solo
        let hf_inst_id = with_pve_special_reg(&ctx1, |reg| {
            let s = &reg.homefront_sites[&30_000_142][0];
            (s.instance_id, s.boss_npc_ball_id, s.base_pilot_isk, s.optimal_fleet_size)
        });
        let jita_sys = ctx1.systems.get_or_spawn(30_000_142);
        jita_sys.try_remove_ball(hf_inst_id.1);
        let hf_done = eve_server::services::pve_special_sites::complete_special_dungeon_instance(
            &ctx1,
            &mut sess_b,
            hf_inst_id.0,
            hf_inst_id.3, // spoofed optimal fleet size while solo
        )
        .await
        .expect("complete_special_dungeon_instance");
        let expected_solo_isk = ((hf_inst_id.2 as f64) * (1.0 / (hf_inst_id.3 as f64))).round() as i64;
        assert_eq!(hf_done.dict_get("rewardPaidIsk"), Some(&PyRep::Integer(expected_solo_isk)));
    }
}
