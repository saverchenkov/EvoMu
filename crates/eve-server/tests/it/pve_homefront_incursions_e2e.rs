//! Genuine non-GM end-to-end integration tests for PvE Shard 2 (Part 3):
//! - Homefront Operations, Drifter Hives, Triglavian Sites & Roaming Metaliminal Storms (6 tags)
//! - Sansha Incursion Fleet Payout Curves & Influence/Tale Notifications (3 tags)

use std::collections::HashMap;
use eve_proto::PyRep;
use eve_sde::SolarSystemId;
use eve_server::services::pve_incursion_lifecycle::{
    eligible_incursion_constellations, INCURSION_WITHDRAWAL_DURATION_SEC,
};
use eve_server::services::pve_incursion_sites::seed_incursion_for_constellation;
use eve_server::services::pve_special_sites::{
    advance_pve_special_clock, ensure_special_sites_spawned,
    read_pve_special_reg, with_pve_special_reg as with_incursion_reg,
};
use eve_server::services::pve_special_sites_combat::get_drifter_hive_for_system;
use crate::pve_e2e_common::{
    bind_service, buy_fit_combat_ship, buy_fit_combat_ship_with_ammo, buy_item_at_station,
    create_player_pilot, drain_all_notifications, ensure_skills_for_types,
    fleet_warp_and_engage_ball, fly_stargates_to, fresh_player_ctx, fund_pilot_isk,
    jump_through_wormhole_to, kv_get, login_existing_player, move_ship_to_ball,
    reload_ctx_from_sqlite, rpc_ok, rpc_res, undock_from_station, wait_for_warp_end,
    warp_and_engage_ball, JITA_STATION, JITA_SYS,
};

fn dict_first_inner_id(rep: &PyRep) -> Option<i64> {
    if let PyRep::Dict(top) = rep {
        for (_, sys_val) in top {
            match sys_val {
                PyRep::Dict(dngs) => {
                    for (_, list_rep) in dngs {
                        if let PyRep::List(ids) = list_rep {
                            if let Some(first) = ids.first().and_then(|v| v.as_i64()) {
                                return Some(first);
                            }
                        }
                    }
                }
                PyRep::List(entries) => {
                    for entry in entries {
                        if let Some(id) = entry.dict_get("instanceID").and_then(|v| v.as_i64()) {
                            return Some(id);
                        }
                    }
                }
                _ => {}
            }
        }
    }
    None
}

// parity: feat:pve.homefront-ops
// parity: rpc:dungeonInstanceCacheMgr.GetHomefrontSiteInstances
// parity: feat:pve.drifter-hives
// parity: feat:pve.triglavian-sites
// parity: rpc:dungeonInstanceCacheMgr.GetTriglavianSiteInstances
// parity: feat:pve.metaliminal-storm-movement
#[tokio::test]
async fn test_homefront_drifter_triglavian_and_metaliminal_storms_e2e() {
    let ctx = fresh_player_ctx();
    let (mut sess1, cid1, _) =
        create_player_pilot(&ctx, 52_001, "hf_lead_u1", "Homefront Lead", JITA_SYS).await;
    let (mut sess2, cid2, _) =
        create_player_pilot(&ctx, 52_002, "hf_wing_u2", "Homefront Wing", JITA_SYS).await;

    let (ship1, guns1) =
        buy_fit_combat_ship_with_ammo(&ctx, &mut sess1, cid1, JITA_STATION, 28_665, 2929, 12_779, 4, 8_000).await;
    let (ship2, guns2) =
        buy_fit_combat_ship_with_ammo(&ctx, &mut sess2, cid2, JITA_STATION, 28_665, 2929, 12_779, 4, 8_000).await;
    ensure_skills_for_types(&ctx, &mut sess1, cid1, JITA_STATION, &[33_400]).await;
    ensure_skills_for_types(&ctx, &mut sess2, cid2, JITA_STATION, &[33_400]).await;
    let bast1 = buy_item_at_station(&ctx, &mut sess1, cid1, JITA_STATION, 33_400, 1).await;
    rpc_ok(&ctx, &mut sess1, "invbroker", "Add", &[PyRep::Integer(bast1), PyRep::Integer(ship1), PyRep::Integer(31)]).await;
    let bast2 = buy_item_at_station(&ctx, &mut sess2, cid2, JITA_STATION, 33_400, 1).await;
    rpc_ok(&ctx, &mut sess2, "invbroker", "Add", &[PyRep::Integer(bast2), PyRep::Integer(ship2), PyRep::Integer(31)]).await;

    let fleet_rep = rpc_ok(
        &ctx,
        &mut sess1,
        "fleetMgr",
        "CreateFleet",
        &[PyRep::String("Homefront Ops Fleet".into())],
    )
    .await;
    let fleet_id = fleet_rep.as_i64().unwrap_or(1);
    let _ = rpc_ok(
        &ctx,
        &mut sess1,
        "fleetMgr",
        "Invite",
        &[
            PyRep::Integer(cid2),
            PyRep::None,
            PyRep::None,
            PyRep::Integer(4),
        ],
    )
    .await;
    let _ = rpc_ok(
        &ctx,
        &mut sess2,
        "fleetMgr",
        "AcceptInvite",
        &[PyRep::Integer(fleet_id)],
    )
    .await;

    ensure_special_sites_spawned(&ctx).await;

    // 1. Homefront Ops: query GetHomefrontSiteInstances, warp both fleet ships, clear boss & verify ISK split
    let hf_rep = rpc_ok(
        &ctx,
        &mut sess1,
        "dungeonInstanceCacheMgr",
        "GetHomefrontSiteInstances",
        &[PyRep::Integer(i64::from(JITA_SYS))],
    )
    .await;
    assert!(
        dict_first_inner_id(&hf_rep).is_some(),
        "GetHomefrontSiteInstances must return active high-sec Homefront site instance IDs"
    );

    let (hf_sys, hf_inst_id) = read_pve_special_reg(&ctx, |reg| {
        let (&sid, list) = reg
            .homefront_sites
            .iter()
            .find(|(_, l)| l.iter().any(|s| !s.completed))
            .expect("active homefront site");
        let s = list.iter().find(|s| !s.completed).unwrap();
        (sid, s.instance_id)
    });

    undock_from_station(&ctx, &mut sess1, JITA_SYS).await;
    undock_from_station(&ctx, &mut sess2, JITA_SYS).await;
    fly_stargates_to(&ctx, &mut sess1, hf_sys).await;
    fly_stargates_to(&ctx, &mut sess2, hf_sys).await;

    let b_oid1 = bind_service(&ctx, &mut sess1, "beyonce", PyRep::Integer(i64::from(hf_sys))).await as u64;
    let b_oid2 = bind_service(&ctx, &mut sess2, "beyonce", PyRep::Integer(i64::from(hf_sys))).await as u64;
    let empty = HashMap::new();
    let _ = ctx.dispatch_rpc(&mut sess1, "beyonce", Some(b_oid1), "CmdWarpToStuff", &[PyRep::String("dungeon".into()), PyRep::Integer(hf_inst_id)], &empty, None).await;
    wait_for_warp_end(&ctx, hf_sys, ship1).await;
    let _ = ctx.dispatch_rpc(&mut sess2, "beyonce", Some(b_oid2), "CmdWarpToStuff", &[PyRep::String("dungeon".into()), PyRep::Integer(hf_inst_id)], &empty, None).await;
    wait_for_warp_end(&ctx, hf_sys, ship2).await;

    let bal1_before = ctx.db.get_character(cid1).map(|c| c.balance.raw_cents()).unwrap_or(0);
    for _pass in 0..6 {
        let hf_boss = read_pve_special_reg(&ctx, |reg| {
            reg.homefront_sites[&hf_sys].iter().find(|s| s.instance_id == hf_inst_id).map(|s| s.boss_npc_ball_id).expect("homefront boss ball")
        });
        let hf_sys_h = ctx.systems.get_or_spawn(hf_sys);
        if hf_sys_h.get_ball_snapshot(hf_boss).await.ok().flatten().is_none() {
            break;
        }
        warp_and_engage_ball(&ctx, &mut sess1, hf_sys, ship1, &guns1, hf_boss, "dungeon", PyRep::Integer(hf_inst_id)).await;
    }
    let hf_completed = read_pve_special_reg(&ctx, |reg| {
        reg.homefront_sites[&hf_sys]
            .iter()
            .find(|s| s.instance_id == hf_inst_id)
            .is_some_and(|s| s.completed)
    });
    assert!(hf_completed, "destroying Homefront boss must complete site without manual bypass");

    let bal1_after = ctx.db.get_character(cid1).map(|c| c.balance.raw_cents()).unwrap_or(0);
    assert!(
        bal1_after > bal1_before,
        "Homefront site completion must pay fleet-scaled ISK share on grid"
    );

    // 2. Triglavian Minor Victory System: query GetTriglavianSiteInstances and clear Triglavian wave + boss
    let (trig_sys, trig_inst_id, trig_boss, trig_escorts) = read_pve_special_reg(&ctx, |reg| {
        let (&sid, list) = reg
            .triglavian_sites
            .iter()
            .find(|(_, l)| l.iter().any(|s| !s.completed))
            .expect("active Triglavian site");
        let s = list.iter().find(|s| !s.completed).unwrap();
        (sid, s.instance_id, s.boss_npc_ball_id, s.escort_ball_ids.clone())
    });
    fly_stargates_to(&ctx, &mut sess1, trig_sys).await;
    let trig_rep = rpc_ok(
        &ctx,
        &mut sess1,
        "dungeonInstanceCacheMgr",
        "GetTriglavianSiteInstances",
        &[PyRep::Integer(i64::from(trig_sys))],
    )
    .await;
    assert!(
        dict_first_inner_id(&trig_rep).is_some(),
        "GetTriglavianSiteInstances must return active Triglavian site instances"
    );
    for ebid in trig_escorts {
        warp_and_engage_ball(&ctx, &mut sess1, trig_sys, ship1, &guns1, ebid, "dungeon", PyRep::Integer(trig_inst_id)).await;
    }
    warp_and_engage_ball(&ctx, &mut sess1, trig_sys, ship1, &guns1, trig_boss, "dungeon", PyRep::Integer(trig_inst_id)).await;
    let trig_completed = read_pve_special_reg(&ctx, |reg| {
        reg.triglavian_sites[&trig_sys].iter().any(|s| s.completed)
    });
    assert!(trig_completed, "destroying Triglavian boss must complete site");

    // 3. Sentinel Drifter Hive: jump into Drifter Wormhole, clear room NPCs & warden via turret combat, verify hive completion
    let (mut sess3, cid3, _) = create_player_pilot(&ctx, 52_003, "hf_wing_u3", "Homefront Heavy 3", JITA_SYS).await;
    let (mut sess4, cid4, _) = create_player_pilot(&ctx, 52_004, "hf_wing_u4", "Homefront Heavy 4", JITA_SYS).await;
    let (ship3, guns3) = buy_fit_combat_ship_with_ammo(&ctx, &mut sess3, cid3, JITA_STATION, 28_665, 2929, 12_779, 4, 8_000).await;
    let (ship4, guns4) = buy_fit_combat_ship_with_ammo(&ctx, &mut sess4, cid4, JITA_STATION, 28_665, 2929, 12_779, 4, 8_000).await;
    ensure_skills_for_types(&ctx, &mut sess3, cid3, JITA_STATION, &[33_400]).await;
    ensure_skills_for_types(&ctx, &mut sess4, cid4, JITA_STATION, &[33_400]).await;
    let bast3 = buy_item_at_station(&ctx, &mut sess3, cid3, JITA_STATION, 33_400, 1).await;
    rpc_ok(&ctx, &mut sess3, "invbroker", "Add", &[PyRep::Integer(bast3), PyRep::Integer(ship3), PyRep::Integer(31)]).await;
    let bast4 = buy_item_at_station(&ctx, &mut sess4, cid4, JITA_STATION, 33_400, 1).await;
    rpc_ok(&ctx, &mut sess4, "invbroker", "Add", &[PyRep::Integer(bast4), PyRep::Integer(ship4), PyRep::Integer(31)]).await;
    for (cap_cid, s_ref) in [(cid3, &mut sess3), (cid4, &mut sess4)] {
        let _ = rpc_res(&ctx, &mut sess1, "fleetMgr", "Invite", &[PyRep::Integer(cap_cid), PyRep::Integer(4)]).await;
        let _ = rpc_res(&ctx, s_ref, "fleetMgr", "AcceptInvite", &[PyRep::Integer(fleet_id)]).await;
    }
    let drifter_sys: SolarSystemId = 31_000_001;
    eve_server::services::space_destiny::ensure_system_signatures_and_anomalies(&ctx, drifter_sys).await;
    ctx.advance_secs(3_601);
    eve_server::services::space_destiny::ensure_system_signatures_and_anomalies(&ctx, drifter_sys).await;
    let cur_pos = glam::DVec3::from_array(ctx.sde.solar_systems[&JITA_SYS].galactic_pos_ly);
    let (k_drf_sys, drifter_sig) = ctx.db.with_snapshot(|s| {
        s.cosmic_signatures.iter()
            .filter(|(&ksys, _)| ksys < 31_000_000 && ctx.sde.solar_systems.get(&ksys).is_some_and(|sys| sys.region_id != 10_000_070))
            .filter_map(|(&ksys, sigs)| {
                sigs.iter().find(|sig| sig.sig_group == "Wormhole" && !sig.collapsed && sig.destination_system_id == Some(drifter_sys) && sig.remaining_mass_kg > 0.0)
                    .map(|sig| (ksys, sig.signature_id.clone(), ctx.sde.solar_systems.get(&ksys).map(|sys| glam::DVec3::from_array(sys.galactic_pos_ly).distance(cur_pos)).unwrap_or(f64::MAX)))
            })
            .min_by(|a, b| a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(ksys, sid, _)| (ksys, sid))
    }).expect("materialized K-space entrance into Sentinel Drifter Hive");
    let _ = eve_server::services::space_destiny::ensure_system_signatures_and_anomalies(&ctx, k_drf_sys).await;
    tokio::join!(
        fly_stargates_to(&ctx, &mut sess1, k_drf_sys),
        fly_stargates_to(&ctx, &mut sess2, k_drf_sys),
        fly_stargates_to(&ctx, &mut sess3, k_drf_sys),
        fly_stargates_to(&ctx, &mut sess4, k_drf_sys),
    );
    for (s_ref, sid) in [(&mut sess1, ship1), (&mut sess2, ship2), (&mut sess3, ship3), (&mut sess4, ship4)] {
        jump_through_wormhole_to(&ctx, s_ref, sid, k_drf_sys, &drifter_sig, drifter_sys).await;
    }

    for _room in 1..=3 {
        let _ = rpc_res(&ctx, &mut sess1, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(i64::from(drifter_sys)), PyRep::Integer(1)]).await;
        let cur_npcs = get_drifter_hive_for_system(&ctx, drifter_sys)
            .and_then(|h| {
                h.rooms
                    .iter()
                    .find(|r| r.room_index == h.current_room)
                    .map(|r| r.npc_ball_ids.clone())
            })
            .unwrap_or_default();
        if _room == 3 {
            let tgt = PyRep::Integer(i64::from(drifter_sys));
            for (s_ref, sid, b_mod) in [
                (&mut sess1, ship1, bast1),
                (&mut sess2, ship2, bast2),
                (&mut sess3, ship3, bast3),
                (&mut sess4, ship4, bast4),
            ] {
                move_ship_to_ball(&ctx, s_ref, drifter_sys, sid, "dungeon", tgt.clone()).await;
                let d_oid = bind_service(&ctx, s_ref, "dogmaIM", PyRep::Integer(sid)).await as u64;
                let _ = ctx.dispatch_rpc(s_ref, "dogmaIM", Some(d_oid), "Activate", &[PyRep::Integer(b_mod)], &HashMap::new(), None).await;
            }
        }
        for nid in cur_npcs {
            let tgt = PyRep::Integer(i64::from(drifter_sys));
            fleet_warp_and_engage_ball(
                &ctx,
                &mut [
                    (&mut sess1, ship1, &guns1),
                    (&mut sess2, ship2, &guns2),
                    (&mut sess3, ship3, &guns3),
                    (&mut sess4, ship4, &guns4),
                ],
                drifter_sys,
                nid,
                "dungeon",
                tgt,
            )
            .await;
        }
    }
    let _ = rpc_res(&ctx, &mut sess1, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(i64::from(drifter_sys)), PyRep::Integer(1)]).await;
    let warden_id = get_drifter_hive_for_system(&ctx, drifter_sys).unwrap().warden_npc_ball_id;
    let w_tgt = PyRep::Integer(i64::from(drifter_sys));
    fleet_warp_and_engage_ball(
        &ctx,
        &mut [
            (&mut sess1, ship1, &guns1),
            (&mut sess2, ship2, &guns2),
            (&mut sess3, ship3, &guns3),
            (&mut sess4, ship4, &guns4),
        ],
        drifter_sys,
        warden_id,
        "dungeon",
        w_tgt,
    )
    .await;
    if !get_drifter_hive_for_system(&ctx, drifter_sys).is_some_and(|h| h.cleared) {
        let _ = rpc_res(&ctx, &mut sess1, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(i64::from(drifter_sys)), PyRep::Integer(1)]).await;
    }
    let hive_after = get_drifter_hive_for_system(&ctx, drifter_sys).unwrap();
    assert!(hive_after.cleared, "destroying Drifter Hive rooms & warden must clear hive");

    // 4. Metaliminal Roaming Storms: advance server clock past next_move_at_sec and verify gate-to-gate migration + SQLite persistence
    let before_centers: Vec<SolarSystemId> = read_pve_special_reg(&ctx, |reg| {
        reg.roaming_storms
            .iter()
            .map(|st| st.center_solar_system_id)
            .collect()
    });
    assert_eq!(
        before_centers.len(),
        8,
        "8 Metaliminal storms (2 per weather type) must seed across null-sec"
    );
    advance_pve_special_clock(&ctx, 48 * 3600);

    let after_centers: Vec<SolarSystemId> = read_pve_special_reg(&ctx, |reg| {
        reg.roaming_storms
            .iter()
            .map(|st| st.center_solar_system_id)
            .collect()
    });
    assert_ne!(
        before_centers, after_centers,
        "Metaliminal storms must roam along stargate graph after migration interval"
    );

    let ctx2 = reload_ctx_from_sqlite(&ctx);
    let reloaded_centers: Vec<SolarSystemId> = read_pve_special_reg(&ctx2, |reg| {
        reg.roaming_storms
            .iter()
            .map(|st| st.center_solar_system_id)
            .collect()
    });
    assert_eq!(reloaded_centers, after_centers);
    let _ = ship2;
}

async fn fire_lance_at(ctx: &eve_server::router::ServerContext, titan_sess: &mut eve_machonet::SessionState, sys_id: SolarSystemId, titan_id: i64, dd_mod: i64, target_id: i64) {
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    if let (Some((_, sb)), Some((_, tb))) = (sys_h.get_ball_snapshot(titan_id).await.ok().flatten(), sys_h.get_ball_snapshot(target_id).await.ok().flatten()) {
        let orig = PyRep::Tuple(vec![PyRep::Real(sb.position.x), PyRep::Real(sb.position.y), PyRep::Real(sb.position.z)]);
        let dest = PyRep::Tuple(vec![PyRep::Real(tb.position.x), PyRep::Real(tb.position.y), PyRep::Real(tb.position.z)]);
        rpc_ok(ctx, titan_sess, "superWeaponMgr", "ActivateSlashModule", &[PyRep::Integer(dd_mod), orig, dest]).await;
        for _ in 0..20 { let _ = sys_h.step_manual(1.0).await; }
    }
}

async fn clear_incursion_site(
    ctx: &eve_server::router::ServerContext,
    fleet: &mut [(&mut eve_machonet::SessionState, i64, &[i64])],
    sys_id: SolarSystemId,
    const_id: u32,
    site_id: &str,
    mut titan_dd: Option<(&mut eve_machonet::SessionState, i64, &[i64], i64)>,
) {
    let target_done = read_pve_special_reg(ctx, |reg| reg.incursions[&const_id].sites.iter().find(|s| s.site_id == site_id).map(|s| s.completed_count + 1).unwrap_or(1));
    let (mut completed, mut dd_fired) = (false, false);
    for _pass in 0..24 {
        let (done, b_id, e_ids) = read_pve_special_reg(ctx, |reg| {
            let inc = &reg.incursions[&const_id];
            let Some(s) = inc.sites.iter().find(|s| s.site_id == site_id) else { return (true, 0, Vec::new()); };
            (s.completed_count >= target_done || inc.mothership_defeated, s.boss_npc_ball_id, s.escort_spawns.iter().filter(|e| !e.destroyed).map(|e| e.npc_ball_id).collect::<Vec<_>>())
        });
        let sys_h = ctx.systems.get_or_spawn(sys_id);
        if done {
            completed = true;
            break;
        }
        let wt = PyRep::String(site_id.into());
        if !e_ids.is_empty() {
            if let Some((ref mut t_sess, t_id, t_guns, _)) = titan_dd {
                let mut f_all: Vec<(&mut eve_machonet::SessionState, i64, &[i64])> = Vec::with_capacity(fleet.len() + 1);
                for (s, id, g) in fleet.iter_mut() { f_all.push((&mut **s, *id, *g)); }
                f_all.push((t_sess, t_id, t_guns));
                for eid in e_ids { fleet_warp_and_engage_ball(ctx, &mut f_all, sys_id, eid, "dungeon", wt.clone()).await; }
            } else {
                for eid in e_ids { fleet_warp_and_engage_ball(ctx, fleet, sys_id, eid, "dungeon", wt.clone()).await; }
            }
            let _ = sys_h.step_manual(2.0).await;
            tokio::task::yield_now().await;
            continue;
        }
        if sys_h.get_ball_snapshot(b_id).await.ok().flatten().is_some() {
            if let Some((ref mut t_sess, t_id, t_guns, dd_mod)) = titan_dd {
                if !dd_fired { fire_lance_at(ctx, t_sess, sys_id, t_id, dd_mod, b_id).await; dd_fired = true; }
                let mut f_all: Vec<(&mut eve_machonet::SessionState, i64, &[i64])> = Vec::with_capacity(fleet.len() + 1);
                for (s, id, g) in fleet.iter_mut() { f_all.push((&mut **s, *id, *g)); }
                f_all.push((t_sess, t_id, t_guns));
                fleet_warp_and_engage_ball(ctx, &mut f_all, sys_id, b_id, "dungeon", wt).await;
            } else {
                fleet_warp_and_engage_ball(ctx, fleet, sys_id, b_id, "dungeon", wt).await;
            }
            let _ = sys_h.step_manual(2.0).await;
            tokio::task::yield_now().await;
        } else {
            let _ = rpc_res(ctx, fleet[0].0, "keeper", "ActivateAccelerationGate", &[wt]).await;
            let _ = sys_h.step_manual(2.0).await;
            tokio::task::yield_now().await;
        }
    }
    assert!(completed, "incursion site {site_id} must complete");
}

// parity: feat:pve.incursion-payout
// parity: notify:OnInfluenceUpdate
// parity: notify:OnTaleRemove
#[tokio::test]
async fn test_incursion_fleet_payout_and_influence_tale_notifications_e2e() {
    let ctx = fresh_player_ctx();
    let (mut sess1, cid1, _) = create_player_pilot(&ctx, 52_101, "inc_pilot_u1", "Incursion Runner", JITA_SYS).await;
    let (mut sess2, cid2, _) = create_player_pilot(&ctx, 52_102, "inc_pilot_u2", "Incursion Wing", JITA_SYS).await;
    let (mut titan_sess, titan_cid, _) = create_player_pilot(&ctx, 52_103, "inc_titan_u3", "Incursion Avatar", JITA_SYS).await;
    fund_pilot_isk(&ctx, &mut sess1, 20).await;
    fund_pilot_isk(&ctx, &mut sess2, 20).await;
    fund_pilot_isk(&ctx, &mut titan_sess, 120).await;

    let (ship1, guns1) = buy_fit_combat_ship_with_ammo(&ctx, &mut sess1, cid1, JITA_STATION, 28_665, 2929, 12_779, 4, 25_000).await;
    ensure_skills_for_types(&ctx, &mut sess2, cid2, JITA_STATION, &[22_428, 21_096]).await;
    let (ship2, _) = buy_fit_combat_ship(&ctx, &mut sess2, cid2, JITA_STATION, 22_428, 9327, 201, 2).await;
    let cyno_mod = buy_item_at_station(&ctx, &mut sess2, cid2, JITA_STATION, 21_096, 1).await;
    rpc_ok(&ctx, &mut sess2, "invbroker", "Add", &[PyRep::Integer(cyno_mod), PyRep::Integer(ship2), PyRep::Integer(30)]).await;
    let ozone = buy_item_at_station(&ctx, &mut sess2, cid2, JITA_STATION, 16_273, 1_500).await;
    rpc_ok(&ctx, &mut sess2, "invbroker", "Add", &[PyRep::Integer(ozone), PyRep::Integer(ship2), PyRep::Integer(5)]).await;

    ensure_skills_for_types(&ctx, &mut titan_sess, titan_cid, JITA_STATION, &[11_567, 40_631]).await;
    let (titan_id, titan_guns) = buy_fit_combat_ship_with_ammo(&ctx, &mut titan_sess, titan_cid, JITA_STATION, 11_567, 37_298, 17_686, 6, 5_000).await;
    let dd_mod = buy_item_at_station(&ctx, &mut titan_sess, titan_cid, JITA_STATION, 40_631, 1).await;
    rpc_ok(&ctx, &mut titan_sess, "invbroker", "Add", &[PyRep::Integer(dd_mod), PyRep::Integer(titan_id), PyRep::Integer(33)]).await;
    let iso_bay = buy_item_at_station(&ctx, &mut titan_sess, titan_cid, JITA_STATION, 16_274, 200_000).await;
    rpc_ok(&ctx, &mut titan_sess, "invbroker", "Add", &[PyRep::Integer(iso_bay), PyRep::Integer(titan_id), PyRep::Integer(133)]).await;
    let iso_cargo = buy_item_at_station(&ctx, &mut titan_sess, titan_cid, JITA_STATION, 16_274, 1_500).await;
    rpc_ok(&ctx, &mut titan_sess, "invbroker", "Add", &[PyRep::Integer(iso_cargo), PyRep::Integer(titan_id), PyRep::Integer(5)]).await;

    let mut d_pilots = Vec::new();
    for (uid, uname, cname) in [(52_104, "inc_dr1", "Incursion Dread 1"), (52_105, "inc_dr2", "Incursion Dread 2"), (52_106, "inc_dr3", "Incursion Dread 3")] {
        let (mut ds, dcid, _) = create_player_pilot(&ctx, uid, uname, cname, JITA_SYS).await;
        fund_pilot_isk(&ctx, &mut ds, 20).await;
        let (did, dguns) = buy_fit_combat_ship_with_ammo(&ctx, &mut ds, dcid, JITA_STATION, 19_720, 37_298, 17_686, 3, 100).await;
        ensure_skills_for_types(&ctx, &mut ds, dcid, JITA_STATION, &[20_280]).await;
        let bay = buy_item_at_station(&ctx, &mut ds, dcid, JITA_STATION, 16_274, 30_000).await;
        rpc_ok(&ctx, &mut ds, "invbroker", "Add", &[PyRep::Integer(bay), PyRep::Integer(did), PyRep::Integer(133)]).await;
        let cargo_iso = buy_item_at_station(&ctx, &mut ds, dcid, JITA_STATION, 16_274, 1_000).await;
        rpc_ok(&ctx, &mut ds, "invbroker", "Add", &[PyRep::Integer(cargo_iso), PyRep::Integer(did), PyRep::Integer(5)]).await;
        let stront = buy_item_at_station(&ctx, &mut ds, dcid, JITA_STATION, 16_275, 550).await;
        rpc_ok(&ctx, &mut ds, "invbroker", "Add", &[PyRep::Integer(stront), PyRep::Integer(did), PyRep::Integer(5)]).await;
        let sm = buy_item_at_station(&ctx, &mut ds, dcid, JITA_STATION, 20_280, 1).await;
        rpc_ok(&ctx, &mut ds, "invbroker", "Add", &[PyRep::Integer(sm), PyRep::Integer(did), PyRep::Integer(30)]).await;
        d_pilots.push((ds, dcid, did, dguns, sm));
    }

    let inc_const: u32 = 20_000_124;
    let mid_sys: SolarSystemId = 30_001_385;
    assert!(eligible_incursion_constellations(&ctx).iter().any(|&(c, _)| c == inc_const));
    with_incursion_reg(&ctx, |reg| seed_incursion_for_constellation(&ctx, reg, inc_const));
    ensure_special_sites_spawned(&ctx).await;

    let (vg_sys, vg_site_id, hq_sys, hq_site_id, ms_site_id) = read_pve_special_reg(&ctx, |reg| {
        let inc = &reg.incursions[&inc_const];
        let vg = inc.sites.iter().find(|s| s.tier == "Vanguard" && s.boss_type_id == 3492).or_else(|| inc.sites.iter().find(|s| s.tier == "Vanguard")).expect("Vanguard site");
        let hq = inc.sites.iter().find(|s| s.tier == "Headquarters" && s.dungeon_id == 5171).or_else(|| inc.sites.iter().find(|s| s.tier == "Headquarters")).expect("HQ site");
        let ms = inc.sites.iter().find(|s| s.tier == "Mothership").expect("Mothership site");
        (vg.solar_system_id, vg.site_id.clone(), hq.solar_system_id, hq.site_id.clone(), ms.site_id.clone())
    });

    // 1. Solo Vanguard run (1 pilot < min_pilots=5 => 0.20x payout scale) + OnInfluenceUpdate
    undock_from_station(&ctx, &mut sess1, JITA_SYS).await;
    fly_stargates_to(&ctx, &mut sess1, vg_sys).await;
    let _ = drain_all_notifications(&ctx, &mut sess1, cid1);
    let bal0 = ctx.db.get_character(cid1).unwrap().balance.raw_cents();
    clear_incursion_site(&ctx, &mut [(&mut sess1, ship1, &guns1)], vg_sys, inc_const, &vg_site_id, None).await;
    let bal1 = ctx.db.get_character(cid1).unwrap().balance.raw_cents();
    let isk_solo = bal1 - bal0;
    assert!(isk_solo > 0, "solo Vanguard clear must credit scaled ISK payout");
    let notes1 = drain_all_notifications(&ctx, &mut sess1, cid1);
    assert!(notes1.iter().any(|(ev, _)| ev == "OnInfluenceUpdate"), "site completion must emit OnInfluenceUpdate");

    // 2. Form fleet; stage Avatar + 3 Dreadnoughts at midpoint cyno, then 2-pilot fleet Vanguard run => 2x solo payout!
    let _ = rpc_ok(&ctx, &mut sess1, "fleetMgr", "CreateFleet", &[]).await;
    for (fcid, fsess) in [(cid2, &mut sess2), (titan_cid, &mut titan_sess)] {
        let _ = rpc_res(&ctx, &mut sess1, "fleetMgr", "Invite", &[PyRep::Integer(fcid), PyRep::Integer(4)]).await;
        let _ = rpc_res(&ctx, fsess, "fleetMgr", "AcceptInvite", &[]).await;
    }
    for (ds, dcid, _, _, _) in &mut d_pilots {
        let _ = rpc_res(&ctx, &mut sess1, "fleetMgr", "Invite", &[PyRep::Integer(*dcid), PyRep::Integer(4)]).await;
        let _ = rpc_res(&ctx, ds, "fleetMgr", "AcceptInvite", &[]).await;
    }
    undock_from_station(&ctx, &mut sess2, JITA_SYS).await;
    fly_stargates_to(&ctx, &mut sess2, mid_sys).await;
    ctx.advance_secs(35);
    let cyno_d = bind_service(&ctx, &mut sess2, "dogmaIM", PyRep::Integer(ship2)).await as u64;
    let cyno1 = ctx.dispatch_rpc(&mut sess2, "dogmaIM", Some(cyno_d), "Activate", &[PyRep::Integer(cyno_mod), PyRep::String("cynosuralGeneration".into())], &HashMap::new(), None).await.unwrap();
    let beacon1 = kv_get(&cyno1, "beaconID").and_then(|v| v.as_i64()).expect("midpoint cyno beacon");

    undock_from_station(&ctx, &mut titan_sess, JITA_SYS).await;
    for (ds, _, _, _, _) in &mut d_pilots { undock_from_station(&ctx, ds, JITA_SYS).await; }
    ctx.advance_secs(35);
    for s_ref in std::iter::once(&mut titan_sess).chain(d_pilots.iter_mut().map(|d| &mut d.0)) {
        let jita_b = bind_service(&ctx, s_ref, "beyonce", PyRep::Integer(i64::from(JITA_SYS))).await as u64;
        ctx.dispatch_rpc(s_ref, "beyonce", Some(jita_b), "CmdJumpToFleetModuleBeacon", &[PyRep::Integer(cid2), PyRep::Integer(i64::from(mid_sys)), PyRep::Integer(beacon1)], &HashMap::new(), None).await.unwrap();
        assert_eq!(s_ref.solar_system_id, Some(mid_sys));
    }
    for (ds, _, _, _, _) in &mut d_pilots { rpc_ok(&ctx, ds, "fleetMgr", "LeaveFleet", &[]).await; }
    let _ = rpc_res(&ctx, &mut sess2, "dogmaIM", "Deactivate", &[PyRep::Integer(cyno_mod)]).await;
    let _ = ctx.systems.get_or_spawn(mid_sys).step_manual(605.0).await;
    ctx.advance_secs(605);

    fly_stargates_to(&ctx, &mut sess2, vg_sys).await;
    move_ship_to_ball(&ctx, &mut sess2, vg_sys, ship2, "dungeon", PyRep::String(vg_site_id.clone())).await;
    clear_incursion_site(&ctx, &mut [(&mut sess1, ship1, &guns1)], vg_sys, inc_const, &vg_site_id, None).await;
    let bal2 = ctx.db.get_character(cid1).unwrap().balance.raw_cents();
    let isk_duo = bal2 - bal1;
    assert_eq!(isk_duo, isk_solo * 2, "2-pilot fleet on Vanguard grid must earn 2x solo payout per curve");

    // 3. Clear 4 HQ sites -> influence 0.0 drops cyno jammer; then light cyno & jump Avatar + 3 Dreadnoughts
    ctx.advance_secs(62);
    tokio::join!(
        fly_stargates_to(&ctx, &mut sess1, hq_sys),
        fly_stargates_to(&ctx, &mut sess2, hq_sys),
    );
    for _ in 0..4 {
        clear_incursion_site(&ctx, &mut [(&mut sess1, ship1, &guns1)], hq_sys, inc_const, &hq_site_id, None).await;
    }
    assert!(read_pve_special_reg(&ctx, |reg| reg.incursions[&inc_const].mothership_spawned));

    for (ds, dcid, _, _, _) in &mut d_pilots {
        let _ = rpc_res(&ctx, &mut sess1, "fleetMgr", "Invite", &[PyRep::Integer(*dcid), PyRep::Integer(4)]).await;
        let _ = rpc_res(&ctx, ds, "fleetMgr", "AcceptInvite", &[]).await;
    }
    move_ship_to_ball(&ctx, &mut sess2, hq_sys, ship2, "dungeon", PyRep::String(hq_site_id.clone())).await;
    ctx.advance_secs(35);
    let cyno2 = ctx.dispatch_rpc(&mut sess2, "dogmaIM", Some(cyno_d), "Activate", &[PyRep::Integer(cyno_mod), PyRep::String("cynosuralGeneration".into())], &HashMap::new(), None).await.unwrap();
    let beacon2 = kv_get(&cyno2, "beaconID").and_then(|v| v.as_i64()).expect("HQ cyno beacon");
    for s_ref in std::iter::once(&mut titan_sess).chain(d_pilots.iter_mut().map(|d| &mut d.0)) {
        let mid_b = bind_service(&ctx, s_ref, "beyonce", PyRep::Integer(i64::from(mid_sys))).await as u64;
        ctx.dispatch_rpc(s_ref, "beyonce", Some(mid_b), "CmdJumpToFleetModuleBeacon", &[PyRep::Integer(cid2), PyRep::Integer(i64::from(hq_sys)), PyRep::Integer(beacon2)], &HashMap::new(), None).await.unwrap();
        assert_eq!(s_ref.solar_system_id, Some(hq_sys));
        let hq_b = bind_service(&ctx, s_ref, "beyonce", PyRep::Integer(i64::from(hq_sys))).await as u64;
        let _ = ctx.dispatch_rpc(s_ref, "beyonce", Some(hq_b), "CmdSetSpeedFraction", &[PyRep::Real(0.1)], &HashMap::new(), None).await;
        let _ = ctx.systems.get_or_spawn(hq_sys).step_manual(2.0).await;
        let _ = ctx.dispatch_rpc(s_ref, "beyonce", Some(hq_b), "CmdStop", &[], &HashMap::new(), None).await;
        let _ = ctx.systems.get_or_spawn(hq_sys).step_manual(1.0).await;
    }

    // 4. Vargur + Avatar clear Mothership escorts, Vargur pulls back, 3 Sieged Dreads + Avatar fire Doomsday & destroy Kundalini -> OnTaleRemove
    let _ = drain_all_notifications(&ctx, &mut sess1, cid1);
    let ms_escorts = read_pve_special_reg(&ctx, |reg| {
        reg.incursions[&inc_const].sites.iter().find(|s| s.site_id == ms_site_id).unwrap().escort_spawns.iter().filter(|e| !e.destroyed).map(|e| e.npc_ball_id).collect::<Vec<_>>()
    });
    for eid in ms_escorts {
        fleet_warp_and_engage_ball(&ctx, &mut [(&mut sess1, ship1, &guns1), (&mut titan_sess, titan_id, &titan_guns)], hq_sys, eid, "dungeon", PyRep::String(ms_site_id.clone())).await;
    }
    move_ship_to_ball(&ctx, &mut sess1, hq_sys, ship1, "dungeon", PyRep::String(hq_site_id.clone())).await;
    move_ship_to_ball(&ctx, &mut titan_sess, hq_sys, titan_id, "dungeon", PyRep::String(ms_site_id.clone())).await;
    for (ds, _, did, _, sm) in &mut d_pilots {
        move_ship_to_ball(&ctx, ds, hq_sys, *did, "dungeon", PyRep::String(ms_site_id.clone())).await;
        let d_oid = bind_service(&ctx, ds, "dogmaIM", PyRep::Integer(*did)).await as u64;
        ctx.dispatch_rpc(ds, "dogmaIM", Some(d_oid), "Activate", &[PyRep::Integer(*sm)], &HashMap::new(), None).await.unwrap();
    }
    let [(ref mut s3, _, d1, ref dg1, _), (ref mut s4, _, d2, ref dg2, _), (ref mut s5, _, d3, ref dg3, _)] = d_pilots.as_mut_slice() else { unreachable!() };
    clear_incursion_site(&ctx, &mut [(s3, *d1, dg1), (s4, *d2, dg2), (s5, *d3, dg3)], hq_sys, inc_const, &ms_site_id, Some((&mut titan_sess, titan_id, &titan_guns, dd_mod))).await;
    assert!(read_pve_special_reg(&ctx, |reg| reg.incursions[&inc_const].mothership_defeated));

    let mut notes_end = drain_all_notifications(&ctx, &mut sess1, cid1);
    advance_pve_special_clock(&ctx, INCURSION_WITHDRAWAL_DURATION_SEC + 5);
    notes_end.extend(drain_all_notifications(&ctx, &mut sess1, cid1));
    assert!(notes_end.iter().any(|(ev, _)| ev == "OnTaleRemove"), "defeating Kundalini Manifest / ending incursion must emit OnTaleRemove");
    assert_eq!(read_pve_special_reg(&ctx, |reg| reg.incursions[&inc_const].state.clone()), "Ended");

    let ctx2 = reload_ctx_from_sqlite(&ctx);
    let (mut sess_r, _) = login_existing_player(&ctx2, 52_199, "inc_pilot_u1", "inc_pilot_u1-secret", cid1).await;
    let logs2 = rpc_ok(&ctx2, &mut sess_r, "incursion", "UpdateIncursionLPLog", &[]).await;
    assert!(matches!(&logs2, PyRep::List(rows) if rows.len() >= 6), "incursion LP history must persist in SQLite");
}
