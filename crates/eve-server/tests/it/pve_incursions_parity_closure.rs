//! Integration tests for Sansha Incursion Lifecycle, Site Classes, Influence, System Effects,
//! Fleet Payouts, and World Event Tales (`feat:pve.incursion-payout`, `feat:pve.incursion-spawn-lifecycle`,
//! `feat:pve.incursion-influence`, `feat:pve.incursion-system-effects`, `feat:pve.incursion-site-classes`, `feat:pve.tales`).

#![forbid(unsafe_code)]

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

    // 3. Verify multi-wave NPC spawning when Wave 1 escorts are destroyed
    let vg_sys_id = vg_sys_list[0];
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
    assert!(vg_boss_bid > 0);

    // 4. Unlock Mothership at low influence, kill it, verify withdrawal timer & respawn cadence
    let ms_boss_bid = with_pve_special_reg(&ctx, |reg| {
        let inc = reg.incursions.get_mut(&target_cid).unwrap();
        inc.influence = 0.05;
        inc.mothership_spawned = true;
        let ms = inc.sites.iter_mut().find(|s| s.tier == "Mothership").unwrap();
        ms.locked_until_low_influence = false;
        ms.boss_npc_ball_id
    });
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

    let (target_cid, hq_sys, vg_sys, vg_bid, as_sys, as_bid, hq_bid) =
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
                vg.boss_npc_ball_id,
                ass.solar_system_id,
                ass.boss_npc_ball_id,
                hq.boss_npc_ball_id,
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

    // 2. Completing Vanguard (-0.05), Assault (-0.20), and HQ (-0.25) reduces influence by site class
    let _ = ctx.systems.get_or_spawn(vg_sys).remove_ball(vg_bid).await;
    on_incursion_boss_destroyed(&ctx, vg_sys, vg_bid, Some(90_000_001)).await.unwrap().unwrap();
    let inf_after_vg = with_pve_special_reg(&ctx, |reg| reg.incursions[&target_cid].influence);
    assert!((inf_after_vg - 0.95).abs() < 1e-6, "Vanguard reduces influence by 0.05");

    let _ = ctx.systems.get_or_spawn(as_sys).remove_ball(as_bid).await;
    on_incursion_boss_destroyed(&ctx, as_sys, as_bid, Some(90_000_001)).await.unwrap().unwrap();
    let inf_after_as = with_pve_special_reg(&ctx, |reg| reg.incursions[&target_cid].influence);
    assert!((inf_after_as - 0.75).abs() < 1e-6, "Assault reduces influence by 0.20");

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
    let (vg2_sys, vg2_bid, second_hq_bid, ms_boss_id) = with_pve_special_reg(&ctx, |reg| {
        let inc = reg.incursions.get_mut(&target_cid).unwrap();
        inc.influence = 0.13;
        let vg2 = inc.sites.iter().find(|s| s.tier == "Vanguard" && !s.boss_destroyed).unwrap();
        let hq2 = inc.sites.iter().find(|s| s.tier == "Headquarters" && !s.boss_destroyed).unwrap();
        let ms = inc.sites.iter().find(|s| s.tier == "Mothership").unwrap();
        (vg2.solar_system_id, vg2.boss_npc_ball_id, hq2.boss_npc_ball_id, ms.boss_npc_ball_id)
    });
    let _ = ctx.systems.get_or_spawn(vg2_sys).remove_ball(vg2_bid).await;
    on_incursion_boss_destroyed(&ctx, vg2_sys, vg2_bid, Some(90_000_001)).await.unwrap().unwrap();
    let still_locked_at_008 = with_pve_special_reg(&ctx, |reg| {
        let inc = &reg.incursions[&target_cid];
        (inc.influence - 0.08).abs() < 1e-6 && !inc.mothership_spawned && inc.state != "Mobilizing"
    });
    assert!(still_locked_at_008, "Mothership must remain locked at 8% (0.08) influence");

    let _ = ctx.systems.get_or_spawn(hq_sys).remove_ball(second_hq_bid).await;
    on_incursion_boss_destroyed(&ctx, hq_sys, second_hq_bid, Some(90_000_001)).await.unwrap().unwrap();
    let ms_unlocked = with_pve_special_reg(&ctx, |reg| {
        let inc = &reg.incursions[&target_cid];
        inc.mothership_spawned && inc.influence <= 0.0 && inc.state == "Mobilizing"
    });
    assert!(ms_unlocked, "Dropping influence to 0.0 must unlock Mothership site and enter Mobilizing");

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

    let (target_cid, vg_sys_id, hq_sys_id, vg_boss_bid, site_pos) = with_pve_special_reg(&ctx, |reg| {
        let (&cid, inc) = reg.incursions.iter().next().unwrap();
        let vg = inc.sites.iter().find(|s| s.tier == "Vanguard").unwrap();
        (cid, vg.solar_system_id, inc.hq_solar_system_id, vg.boss_npc_ball_id, vg.position)
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
