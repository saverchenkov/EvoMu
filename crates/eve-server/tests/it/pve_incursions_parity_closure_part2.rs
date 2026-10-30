//! Part 2 integration tests for Sansha Incursion SDE Stargate-Graph Topology Roles,
//! Fleet Payout Scaling (Optimal, Over-Cap Diminishing Return, Hard-Cap Zero Cutoff),
//! `incursion.UpdateIncursionLPLog` Reconciliation, and `taleMgr` `PyRep::Dict` Shapes
//! (`feat:pve.incursion-payout`, `feat:pve.incursion-spawn-lifecycle`, `feat:pve.incursion-site-classes`, `feat:pve.tales`).

#![forbid(unsafe_code)]

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
