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

    // Kill boss with attributed damage from pilot 90_000_001
    // Server-side death hook automatically handles site wave completion & payout without keeper RPC
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

    // Next wave boss must have cleanly respawned
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
        let hq_boss_bid = with_pve_special_reg(&ctx, |reg| {
            reg.incursions[&target_cid].sites.iter().find(|s| s.site_id == hq_site_id).unwrap().boss_npc_ball_id
        });
        assert!(hq_sys.get_ball_snapshot(hq_boss_bid).await.unwrap().is_some());
        let (_, dead, _) = hq_sys.apply_damage_with_attacker(
            hq_boss_bid,
            DamageVector { em: 100_000.0, thermal: 100_000.0, kinetic: 100_000.0, explosive: 100_000.0 },
            Some(90_000_001),
        ).await.unwrap().unwrap();
        assert!(dead, "HQ boss killed for wave {wave}");
    }

    // Now influence is 0.0; Mothership unlocks and spawns!
    let (inf_after_hq, ms_spawned, ms_boss_bid) = with_pve_special_reg(&ctx, |reg| {
        let inc = &reg.incursions[&target_cid];
        let ms = inc.sites.iter().find(|s| s.tier == "Mothership").unwrap();
        (inc.influence, inc.mothership_spawned, ms.boss_npc_ball_id)
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

    // Move player ship to Mothership site grid and kill The Kundalini Manifest
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
    let next_boss_bid = with_pve_special_reg(&ctx, |reg| {
        reg.incursions[&target_cid].sites.iter().find(|s| s.site_id == site_id).unwrap().boss_npc_ball_id
    });
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

    let (count1, next_boss2) = with_pve_special_reg(&ctx, |r| {
        let s = r.incursions[&target_cid].sites.iter().find(|s| s.site_id == site_id).unwrap();
        (s.completed_count, s.boss_npc_ball_id)
    });
    assert_eq!(count1, 2, "Smartbomb AoE boss destruction must advance wave");

    // 3. Synchronous weapon cycle destruction (NF-INC-CYCLE-ASYNC-01)
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
