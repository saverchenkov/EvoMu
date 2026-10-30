//! Authentic Incursion System, Payouts, Mothership Finale, Drifter Hives & Triglavian Sites.
//! Remediated integration tests for Phase 8 / PVE milestones.

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
