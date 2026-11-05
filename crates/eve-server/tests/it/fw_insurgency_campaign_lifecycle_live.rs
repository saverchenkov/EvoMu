//! Live test suite for Factional Warfare Supply Caches/Depots, Pirate Insurgency lifecycle
//! & stargate spread, and post-surrender FW I-Hub downtime capture.
//!
//! Driven 100% through MachoNet RPCs, GM slash commands, live space combat, and cluster ticks.
//! Zero `with_mut_snapshot`, zero `with_mut_combat_runtime`, zero direct runtime mutations.

#![forbid(unsafe_code)]

use crate::common::{self, LivePilot};
use crate::esi_fw_sov_incursions_live::{lock_target, rpc, slash};
use eve_db::InventoryFlag;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::fw_gameplay::{SURRENDER_IHUB_VULNERABILITY_DELAY_SECS, TYPE_FW_IHUB};
use eve_server::services::insurgency_complex_ops::{load_ops_state, OutpostRaidSize};
use eve_server::services::insurgency_services::{
    read_insurgency_runtime, step_system_insurgency_tick, FORECASTING_DURATION_SECS,
};
use eve_server::sp6_lifecycle::tick_background_cluster;
use glam::DVec3;
use std::collections::HashSet;
use std::sync::Arc;

pub(crate) const CALDARI: i64 = 500_001;
pub(crate) const GALLENTE: i64 = 500_004;
pub(crate) const GURISTAS: i64 = 500_010;

fn sde_type_by_name(ctx: &ServerContext, name: &str) -> u32 {
    ctx.sde
        .types
        .values()
        .find(|t| t.name == name)
        .unwrap_or_else(|| panic!("SDE type '{name}' not found"))
        .type_id
}

async fn grant_prereq_skills(ctx: &ServerContext, sess: &mut SessionState, type_id: u32) {
    let mut pending = vec![type_id];
    let mut seen = HashSet::new();
    while let Some(tid) = pending.pop() {
        let reqs: Vec<u32> = ctx
            .sde
            .types
            .get(&tid)
            .map(|t| {
                [182_u16, 183, 184, 1285, 1289, 1290]
                    .iter()
                    .filter_map(|a| t.default_attributes.get(a))
                    .map(|v| v.round() as u32)
                    .filter(|&s| s > 0)
                    .collect()
            })
            .unwrap_or_default();
        for sk in reqs {
            if seen.insert(sk) {
                slash(ctx, sess, &format!("/giveskill {sk} 5")).await;
                pending.push(sk);
            }
        }
    }
}

pub(crate) async fn setup_enlisted_pilot(
    ctx: &ServerContext,
    sid: u64,
    char_id: i64,
    war_fac: i64,
    sys_id: u32,
) -> (LivePilot, Vec<i64>) {
    let hurricane = sde_type_by_name(ctx, "Hurricane");
    let gun_tid = sde_type_by_name(ctx, "425mm AutoCannon II");
    let gyro_tid = sde_type_by_name(ctx, "Gyrostabilizer II");
    let ammo_tid = sde_type_by_name(ctx, "EMP M");
    let mut pilot = common::gm_pilot_in_space(ctx, sid, char_id, hurricane, sys_id).await;
    grant_prereq_skills(ctx, &mut pilot.sess, gun_tid).await;
    grant_prereq_skills(ctx, &mut pilot.sess, gyro_tid).await;
    for sk in [3426_u32, 3413, 3318, 11207, 3429] {
        slash(ctx, &mut pilot.sess, &format!("/giveskill {sk} 5")).await;
    }
    rpc(
        ctx,
        &mut pilot.sess,
        "facWarMgr",
        "JoinFactionAsCharacter",
        &[PyRep::Integer(war_fac)],
    )
    .await
    .expect("JoinFactionAsCharacter");

    for _ in 0..6 {
        slash(ctx, &mut pilot.sess, &format!("/fit me {gun_tid}")).await;
    }
    for _ in 0..4 {
        slash(ctx, &mut pilot.sess, &format!("/fit me {gyro_tid}")).await;
    }
    let ship_id = pilot.ship_id;
    let (guns, all_mods): (Vec<i64>, Vec<i64>) = ctx.db.with_snapshot(|s| {
        let mut g: Vec<i64> = s
            .items
            .values()
            .filter(|i| i.location_id == ship_id && i.type_id == gun_tid)
            .map(|i| i.item_id)
            .collect();
        let mut m: Vec<i64> = s
            .items
            .values()
            .filter(|i| i.location_id == ship_id && (i.type_id == gun_tid || i.type_id == gyro_tid))
            .map(|i| i.item_id)
            .collect();
        g.sort_unstable();
        m.sort_unstable();
        (g, m)
    });
    for mid in all_mods {
        let _ = rpc(ctx, &mut pilot.sess, "dogmaIM", "SetModuleOnline", &[PyRep::Integer(mid)]).await;
    }
    let ammo_id = common::stock_ship_bay(
        ctx,
        &mut pilot,
        ammo_tid,
        20_000,
        InventoryFlag::Cargo as i64,
    )
    .await;
    let gun_reps: Vec<PyRep> = guns.iter().map(|&g| PyRep::Integer(g)).collect();
    rpc(
        ctx,
        &mut pilot.sess,
        "dogmaIM",
        "LoadAmmo",
        &[
            PyRep::Integer(ship_id),
            PyRep::List(gun_reps),
            PyRep::List(vec![PyRep::Integer(ammo_id)]),
        ],
    )
    .await
    .expect("LoadAmmo");
    ctx.advance_secs(11);
    let _ = rpc(ctx, &mut pilot.sess, "beyonce", "CmdStop", &[]).await;
    (pilot, guns)
}

pub(crate) async fn move_near(ctx: &ServerContext, sys_id: u32, ship_id: i64, target_id: i64) {
    let sys = ctx.systems.get_or_spawn(sys_id);
    let pos = sys
        .get_ball_snapshot(target_id)
        .await
        .unwrap()
        .expect("target ball")
        .1
        .position;
    sys.set_ball_position(ship_id, pos + DVec3::new(1_000.0, 0.0, 0.0))
        .await;
}

pub(crate) async fn destroy_target_live(
    ctx: &ServerContext,
    sess: &mut SessionState,
    sys_id: u32,
    ship_id: i64,
    guns: &[i64],
    target_id: i64,
) {
    let sys = ctx.systems.get_or_spawn(sys_id);
    move_near(ctx, sys_id, ship_id, target_id).await;
    lock_target(ctx, sess, sys_id, ship_id, target_id, 1_000.0).await;
    for _ in 0..120 {
        if ctx.db.with_snapshot(|s| {
            s.structures
                .get(&target_id)
                .is_some_and(|st| st.state == "Destroyed")
        }) {
            return;
        }
        let Some((_, t_ball)) = sys.get_ball_snapshot(target_id).await.unwrap() else {
            return;
        };
        if let Some((_, s_ball)) = sys.get_ball_snapshot(ship_id).await.unwrap() {
            if s_ball.position.distance(t_ball.position) > 2_000.0 {
                move_near(ctx, sys_id, ship_id, target_id).await;
            }
        }
        let mut reloading = false;
        for &gid in guns {
            if sys.get_ball_snapshot(target_id).await.unwrap().is_none() {
                return;
            }
            if let Err(e) = rpc(
                ctx,
                sess,
                "dogmaIM",
                "Activate",
                &[
                    PyRep::Integer(gid),
                    PyRep::String("targetAttack".into()),
                    PyRep::Integer(target_id),
                ],
            )
            .await
            {
                match e.code.as_str() {
                    "TargetNotFound"
                    | "TargetNotInSpace"
                    | "StructureAlreadyDestroyed"
                    | "StructureDestroyed" => return,
                    "TargetNotLocked" => {
                        lock_target(ctx, sess, sys_id, ship_id, target_id, 1_000.0).await;
                    }
                    "TargetOutOfRange" => move_near(ctx, sys_id, ship_id, target_id).await,
                    "ModuleIsReloading" => reloading = true,
                    "ModuleAlreadyActive" | "ModuleOnCooldown" => {}
                    _ => panic!(
                        "Activate weapon {gid} on {target_id} failed: {} ({})",
                        e.code, e.message
                    ),
                }
            }
        }
        let step_secs = if reloading { 11 } else { 4 };
        for _ in 0..step_secs {
            sys.step_manual(1.0).await.unwrap();
        }
        ctx.advance_secs(step_secs);
        slash(ctx, sess, "/heal").await;
    }
    panic!("target {target_id} was not destroyed after 120 volleys");
}

pub(crate) async fn fire_one_volley(
    ctx: &ServerContext,
    sess: &mut SessionState,
    sys_id: u32,
    ship_id: i64,
    gun_id: i64,
    target_id: i64,
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    move_near(ctx, sys_id, ship_id, target_id).await;
    lock_target(ctx, sess, sys_id, ship_id, target_id, 1_000.0).await;
    rpc(
        ctx,
        sess,
        "dogmaIM",
        "Activate",
        &[
            PyRep::Integer(gun_id),
            PyRep::String("targetAttack".into()),
            PyRep::Integer(target_id),
        ],
    )
    .await
}

pub(crate) async fn boost_siege_guns(ctx: &ServerContext, sess: &mut SessionState, guns: &[i64]) {
    for &gid in guns {
        slash(ctx, sess, &format!("/setattr {gid} 64 5000.0")).await;
    }
}

pub(crate) async fn hold_complex_to_pts(
    ctx: &ServerContext,
    sess: &mut SessionState,
    campaign_id: i64,
    sys_id: u32,
    ship_id: i64,
    is_pirate: bool,
    target_pts: i64,
) {
    slash(ctx, sess, &format!("/tr {sys_id}")).await;
    let _ = rpc(ctx, sess, "beyonce", "CmdStop", &[]).await;
    let _ = rpc(
        ctx,
        sess,
        "dungeonInstanceCacheMgr",
        "GetPirateInsurgencyInstances",
        &[PyRep::Integer(i64::from(sys_id))],
    )
    .await;
    let beacon_pos = ctx.db.with_snapshot(|s| {
        s.system_states[&sys_id]
            .fw_complex_position
            .expect("fw_complex_position")
    });
    let sys = ctx.systems.get_or_spawn(sys_id);
    sys.set_ball_position(ship_id, DVec3::from_array(beacon_pos))
        .await;
    sys.step_manual(1.0).await.unwrap();

    for _ in 0..30 {
        let cur_pts = read_insurgency_runtime(ctx, |rt| {
            rt.campaigns.get(&campaign_id).map_or(0, |c| {
                if is_pirate {
                    c.corruption_points.get(&sys_id).copied().unwrap_or(0)
                } else {
                    c.suppression_points.get(&sys_id).copied().unwrap_or(0)
                }
            })
        });
        if cur_pts >= target_pts {
            sys.set_ball_position(ship_id, DVec3::ZERO).await;
            return;
        }
        sys.step_manual(60.0).await.unwrap();
        ctx.advance_virtual_clock(60);
        step_system_insurgency_tick(ctx, sys_id).await;
    }
    panic!("System {sys_id} did not reach {target_pts} insurgency points");
}

pub(crate) async fn advance_system_insurgency_pts(
    ctx: &ServerContext,
    sess: &mut SessionState,
    campaign_id: i64,
    sys_id: u32,
    ship_id: i64,
    guns: &[i64],
    is_pirate: bool,
    target_pts: i64,
) {
    slash(ctx, sess, &format!("/tr {sys_id}")).await;
    let _ = rpc(ctx, sess, "beyonce", "CmdStop", &[]).await;
    tick_background_cluster(ctx).await;

    // Complete one available Moderate Outpost Raid via gate activation & live defender combat.
    let raid = ctx.db.with_snapshot(|s| {
        load_ops_state(s)
            .outpost_raids
            .values()
            .find(|r| {
                r.solar_system_id == sys_id
                    && !r.completed_on_cooldown
                    && r.size == OutpostRaidSize::Moderate
            })
            .cloned()
    });
    if let Some(r) = raid {
        move_near(ctx, sys_id, ship_id, r.gate_id).await;
        rpc(
            ctx,
            sess,
            "keeper",
            "ActivateAccelerationGate",
            &[PyRep::Integer(r.gate_id)],
        )
        .await
        .expect("ActivateAccelerationGate");
        let _ = rpc(ctx, sess, "beyonce", "CmdStop", &[]).await;
        let def_opt = ctx.db.with_snapshot(|s| {
            load_ops_state(s).outpost_raids.get(&r.gate_id).and_then(|cur| {
                if is_pirate {
                    cur.mordus_defender_ball_id
                } else {
                    cur.pirate_defender_ball_id
                }
            })
        });
        if let Some(def_id) = def_opt {
            boost_siege_guns(ctx, sess, guns).await;
            destroy_target_live(ctx, sess, sys_id, ship_id, guns, def_id).await;
        }
        ctx.advance_virtual_clock(r.size.base_capture_duration_secs() + 1);
        tick_background_cluster(ctx).await;
    }

    hold_complex_to_pts(ctx, sess, campaign_id, sys_id, ship_id, is_pirate, target_pts).await;
    tick_background_cluster(ctx).await;
}

// parity: feat:fw.supply-caches-depots
#[tokio::test]
async fn test_live_fw_supply_caches_depots_and_advantage() {
    eve_testkit::attest(&["feat:fw.supply-caches-depots"]);

    let ctx = Arc::new(ServerContext::new());
    common::pin_clock(&ctx);

    let sys_id = ctx
        .db
        .with_snapshot(|snap| {
            snap.fw_system_occupations
                .iter()
                .find(|(_, o)| o.occupier_faction_id == GALLENTE)
                .map(|(&s, _)| s)
        })
        .expect("Gallente FW system");

    let (mut pilot, guns) = setup_enlisted_pilot(&ctx, 77_101, 91_000_101, CALDARI, sys_id).await;
    let ship = pilot.ship_id;
    tick_background_cluster(&ctx).await;

    let (cache_id, depot_id) = ctx.db.with_snapshot(|snap| {
        let mut cache = 0i64;
        let mut depot = 0i64;
        for (&sid, &(ssys, ref kind, _)) in &snap.fw_runtime.supply_sites {
            if ssys == sys_id {
                if kind == "SupplyCache" {
                    cache = sid;
                } else if kind == "SupplyDepot" {
                    depot = sid;
                }
            }
        }
        (cache, depot)
    });
    assert!(cache_id > 0 && depot_id > 0, "SupplyCache and SupplyDepot must exist");

    // 1. Partial damage to SupplyCache leaves it in ShieldVulnerable state.
    fire_one_volley(&ctx, &mut pilot.sess, sys_id, ship, guns[0], cache_id)
        .await
        .expect("partial damage to cache");
    assert_eq!(
        ctx.db.with_snapshot(|s| s.structures[&cache_id].state.clone()),
        "ShieldVulnerable"
    );

    // 2. Destroy SupplyCache: drops 5x militia supply item (88_103) and awards +2% Advantage.
    destroy_target_live(&ctx, &mut pilot.sess, sys_id, ship, &guns, cache_id).await;
    let cache_can = ctx.db.with_snapshot(|s| {
        s.items
            .values()
            .find(|it| it.type_id == 88_103 && it.quantity == 5)
            .map(|it| it.location_id)
    });
    assert!(cache_can.is_some(), "Supply Cache must drop 5x militia supply items");
    let (occ_adv, att_adv) = ctx.db.with_snapshot(|s| {
        s.fw_runtime
            .system_advantage
            .get(&sys_id)
            .copied()
            .unwrap_or((0.0, 0.0))
    });
    assert_eq!((occ_adv, att_adv), (0.0, 2.0));

    // 3. Destroy SupplyDepot: drops 10x militia supply item (88_103) and awards +5% Advantage (+7% total).
    move_near(&ctx, sys_id, ship, depot_id).await;
    destroy_target_live(&ctx, &mut pilot.sess, sys_id, ship, &guns, depot_id).await;
    let depot_can = ctx.db.with_snapshot(|s| {
        s.items
            .values()
            .find(|it| it.type_id == 88_103 && it.quantity == 10)
            .map(|it| it.location_id)
    });
    assert!(depot_can.is_some(), "Supply Depot must drop 10x militia supply items");
    let (_, att_adv2) = ctx.db.with_snapshot(|s| {
        s.fw_runtime
            .system_advantage
            .get(&sys_id)
            .copied()
            .unwrap_or((0.0, 0.0))
    });
    assert_eq!(att_adv2, 7.0);
}

// parity: feat:fw.insurgency-lifecycle
// parity: feat:fw.insurgency-spread
#[tokio::test]
async fn test_live_insurgency_campaign_lifecycle_and_stargate_spread() {
    eve_testkit::attest(&[
        "feat:fw.insurgency-lifecycle",
        "feat:fw.insurgency-spread",
    ]);

    let ctx = Arc::new(ServerContext::new());
    common::pin_clock(&ctx);

    let hub_sys: u32 = 30_002_538;
    let (mut pilot, guns) = setup_enlisted_pilot(&ctx, 77_102, 91_000_102, GURISTAS, hub_sys).await;
    let ship = pilot.ship_id;
    tick_background_cluster(&ctx).await;

    // 1. Initial campaign is auto-seeded in Forecasting phase via authentic RPC.
    let PyRep::List(camps) =
        rpc(&ctx, &mut pilot.sess, "insurgencySolarsystem", "GetAllVisibleCampaigns", &[])
            .await
            .expect("GetAllVisibleCampaigns")
    else {
        panic!("expected list");
    };
    let camp_rep = camps.first().expect("campaign present");
    let campaign_id = camp_rep
        .dict_get("campaignID")
        .and_then(PyRep::as_i64)
        .expect("campaignID");
    assert_eq!(
        camp_rep.dict_get("phase").and_then(PyRep::as_str),
        Some("Forecasting")
    );

    // 2. Advancing virtual clock past 48h + cluster tick transitions campaign into Active phase.
    ctx.advance_virtual_clock(FORECASTING_DURATION_SECS + 1);
    tick_background_cluster(&ctx).await;

    let PyRep::List(camps2) =
        rpc(&ctx, &mut pilot.sess, "insurgencySolarsystem", "GetAllVisibleCampaigns", &[])
            .await
            .expect("GetAllVisibleCampaigns active")
    else {
        panic!("expected list");
    };
    let camp_rep2 = camps2
        .iter()
        .find(|c| c.dict_get("campaignID").and_then(PyRep::as_i64) == Some(campaign_id))
        .unwrap();
    assert_eq!(
        camp_rep2.dict_get("phase").and_then(PyRep::as_str),
        Some("Active")
    );

    // 3. Local snapshot RPC returns current system corruption & suppression state.
    let PyRep::Tuple(parts) = rpc(
        &ctx,
        &mut pilot.sess,
        "insurgencySolarsystem",
        "GetLocalCampaignClientSnapshot",
        &[],
    )
    .await
    .expect("GetLocalCampaignClientSnapshot")
    else {
        panic!("expected tuple");
    };
    assert_eq!(parts.first().and_then(PyRep::as_i64), Some(hub_sys as i64));

    // 4. Advancing Corruption to Stage 3 (>= 600 pts) triggers stargate-adjacent system spread.
    advance_system_insurgency_pts(&ctx, &mut pilot.sess, campaign_id, hub_sys, ship, &guns, true, 600).await;

    let PyRep::List(camps3) =
        rpc(&ctx, &mut pilot.sess, "insurgencySolarsystem", "GetAllVisibleCampaigns", &[])
            .await
            .expect("GetAllVisibleCampaigns post-spread")
    else {
        panic!("expected list");
    };
    let camp_rep3 = camps3
        .iter()
        .find(|c| c.dict_get("campaignID").and_then(PyRep::as_i64) == Some(campaign_id))
        .unwrap();
    let PyRep::List(systems_rep) = camp_rep3.dict_get("systems").expect("systems list") else {
        panic!("not list");
    };
    assert!(systems_rep.len() >= 2, "Campaign must have spread to adjacent systems");

    let spread_sys = systems_rep
        .iter()
        .filter_map(PyRep::as_i64)
        .map(|s| s as u32)
        .find(|&s| s != hub_sys)
        .unwrap();
    let is_adjacent = ctx
        .sde
        .jumps
        .values()
        .any(|j| j.from_solar_system_id == hub_sys && j.to_solar_system_id == spread_sys);
    assert!(
        is_adjacent,
        "Spread system {spread_sys} must be stargate-connected to {hub_sys}"
    );
}

// parity: feat:fw.system-capture
#[tokio::test]
async fn test_live_fw_system_capture_post_surrender_ihub_and_downtime() {
    eve_testkit::attest(&["feat:fw.system-capture"]);

    let ctx = Arc::new(ServerContext::new());
    common::pin_clock(&ctx);

    let hub_sys: u32 = 30_002_538;
    let orig_occ = ctx
        .db
        .with_snapshot(|s| s.fw_system_occupations[&hub_sys].occupier_faction_id);
    let emp_fac = match orig_occ {
        500_001 => 500_004,
        500_004 => 500_001,
        500_002 => 500_003,
        500_003 => 500_002,
        _ => CALDARI,
    };
    let (mut pir, _guns_pir) = setup_enlisted_pilot(&ctx, 77_107, 91_000_107, GURISTAS, hub_sys).await;
    let (mut emp, guns_emp) = setup_enlisted_pilot(&ctx, 77_108, 91_000_108, emp_fac, hub_sys).await;
    let (ship_pir, ship_emp) = (pir.ship_id, emp.ship_id);
    tick_background_cluster(&ctx).await;

    let campaign_id = read_insurgency_runtime(&ctx, |rt| rt.campaigns.values().next().unwrap().campaign_id);
    let ihub_id = ctx
        .db
        .with_snapshot(|snap| {
            snap.structures
                .values()
                .find(|st| {
                    st.solar_system_id == hub_sys
                        && st.type_id == TYPE_FW_IHUB
                        && st.state != "Destroyed"
                })
                .map(|st| st.structure_id)
        })
        .expect("FW Infrastructure Hub");

    // 1. I-Hub is invulnerable before post-surrender window or 100% contested.
    let invuln_err = fire_one_volley(&ctx, &mut emp.sess, hub_sys, ship_emp, guns_emp[0], ihub_id)
        .await
        .expect_err("I-Hub must be invulnerable");
    assert_eq!(invuln_err.code, "IHubInvulnerable");

    // 2. Win insurgency as Pirates to trigger 25h post-surrender I-Hub vulnerability timer.
    ctx.systems
        .get_or_spawn(hub_sys)
        .set_ball_position(ship_emp, DVec3::ZERO)
        .await;
    ctx.advance_virtual_clock(FORECASTING_DURATION_SECS + 1);
    tick_background_cluster(&ctx).await;
    hold_complex_to_pts(&ctx, &mut pir.sess, campaign_id, hub_sys, ship_pir, true, 1_000).await;
    let all_systems = read_insurgency_runtime(&ctx, |rt| rt.campaigns[&campaign_id].systems.clone());
    for s in all_systems.into_iter().filter(|&s| s != hub_sys).take(4) {
        hold_complex_to_pts(&ctx, &mut pir.sess, campaign_id, s, ship_pir, true, 1_000).await;
    }
    tick_background_cluster(&ctx).await;

    slash(&ctx, &mut emp.sess, &format!("/tr {hub_sys}")).await;
    let _ = rpc(&ctx, &mut emp.sess, "beyonce", "CmdStop", &[]).await;
    let early_err = fire_one_volley(&ctx, &mut emp.sess, hub_sys, ship_emp, guns_emp[0], ihub_id)
        .await
        .expect_err("I-Hub still invulnerable before 25h");
    assert_eq!(early_err.code, "IHubInvulnerable");

    // 3. Advance clock past 25h + cluster tick: I-Hub becomes vulnerable and can be destroyed in live combat.
    ctx.advance_virtual_clock(SURRENDER_IHUB_VULNERABILITY_DELAY_SECS + 1);
    tick_background_cluster(&ctx).await;

    boost_siege_guns(&ctx, &mut emp.sess, &guns_emp).await;
    destroy_target_live(&ctx, &mut emp.sess, hub_sys, ship_emp, &guns_emp, ihub_id).await;
    assert_eq!(
        ctx.db.with_snapshot(|s| s.structures[&ihub_id].state.clone()),
        "Destroyed"
    );
    assert_eq!(
        ctx.db.with_snapshot(|s| s.fw_system_occupations[&hub_sys].occupier_faction_id),
        orig_occ
    );

    // 4. GM /downtime command processes pending FW captures and flips system occupier to opposing militia.
    let dt_rep = slash(&ctx, &mut emp.sess, "/downtime").await;
    assert_eq!(
        dt_rep.dict_get("fwSystemsCaptured").and_then(PyRep::as_i64),
        Some(1)
    );

    let (occ_after, vp_after) = ctx.db.with_snapshot(|s| {
        let o = &s.fw_system_occupations[&hub_sys];
        (o.occupier_faction_id, o.victory_points)
    });
    assert_eq!((occ_after, vp_after), (emp_fac, 0));
}
