//! E2E integration tests closing `OnDungeonInstanceAdded`, `OnDungeonInstanceRemoved`,
//! `OnFacWarBunkerVulnerable`, and `OnInfluenceUpdate` in Batch 3.

use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, MovementMode, TargetLock};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

const PILOT_A: i64 = 90_000_001;
const PILOT_B: i64 = 90_000_002;
const SYS_JITA: u32 = 30_000_142;

fn new_ctx() -> ServerContext {
    let ctx = ServerContext::new();
    ctx.set_clock_paused(true);
    ctx
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None)
        .await
}

fn drain_all(ctx: &ServerContext, char_id: i64) -> Vec<(String, Vec<PyRep>)> {
    ctx.drain_notifications_for_char(char_id)
        .into_iter()
        .map(|n| (n.event, n.args))
        .collect()
}

fn dict_get<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match rep {
        PyRep::KeyVal(entries) => entries.get(key),
        PyRep::Dict(entries) => entries
            .iter()
            .find(|(k, _)| k.as_str() == Some(key))
            .map(|(_, v)| v),
        _ => None,
    }
}

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
        version: 1,
        dynamic_attributes: HashMap::new(),
        source_mutaplasmid_id: None,
        insured_payout_isk: 0.0,
        is_blueprint_copy: false,
        blueprint_runs_remaining: None,
    }
}

async fn login_in_space(
    ctx: &ServerContext,
    sid: u64,
    user: &str,
    pass: &str,
    char_id: i64,
    ship_id: i64,
    ship_type_id: u32,
    sys_id: u32,
    pos: DVec3,
) -> SessionState {
    let mut sess = SessionState::new(sid);
    common::login_pilot(ctx, &mut sess, user, pass).await;
    rpc(
        ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
    )
    .await
    .unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            make_item(
                ship_id,
                ship_type_id,
                char_id,
                i64::from(sys_id),
                InventoryFlag::None,
                1,
                true,
            ),
        );
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
        }
    });
    sess.station_id = None;
    sess.structure_id = None;
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    sys_h
        .spawn_ball_with_meta(
            Ball {
                id: ship_id,
                type_id: ship_type_id,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Rigid,
            },
            ctx.build_ship_combat_state(char_id, ship_id, ship_type_id),
            eve_destiny::SlimItemMeta {
                group_id: 25,
                category_id: 6,
                owner_id: char_id,
                char_id: Some(char_id),
                corp_id: 1000009,
                alliance_id: -1,
                security_status: 0.0,
                is_cloaked: false,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    sess
}

// parity: notify:OnDungeonInstanceAdded, notify:OnDungeonInstanceRemoved
#[tokio::test]
async fn test_batch3_dungeon_instance_added_and_removed() {
    eve_testkit::attest(&[
        "notify:OnDungeonInstanceAdded",
        "notify:OnDungeonInstanceRemoved",
    ]);

    let ctx = new_ctx();
    let dest_sys =
        eve_server::services::pve_expeditions::find_escalation_destination_system(&ctx, SYS_JITA);
    let _dest_sess = login_in_space(
        &ctx,
        92_040,
        "other",
        "other-secret",
        PILOT_B,
        640_000_502,
        587,
        dest_sys,
        DVec3::ZERO,
    )
    .await;

    let (ship_a, gun_a, gun_b, gun_c) = (640_000_501_i64, 640_000_510_i64, 640_000_512_i64, 640_000_514_i64);
    let mut sess_a = login_in_space(
        &ctx,
        92_041,
        "demo",
        "demo-secret",
        PILOT_A,
        ship_a,
        638,
        SYS_JITA,
        DVec3::ZERO,
    )
    .await;
    ctx.db.with_mut_snapshot(|snap| {
        for (gid, aid, flag) in [
            (gun_a, 640_000_511_i64, InventoryFlag::HiSlot0),
            (gun_b, 640_000_513_i64, InventoryFlag::HiSlot1),
            (gun_c, 640_000_515_i64, InventoryFlag::HiSlot2),
        ] {
            snap.items.insert(gid, make_item(gid, 490, PILOT_A, ship_a, flag, 1, true));
            snap.items.insert(aid, make_item(aid, 206, PILOT_A, ship_a, flag, 500, false));
            snap.module_online_states.insert(gid, true);
        }
    });
    ctx.invalidate_ship_fitting(ship_a);

    let esc_seed = (1..10_000_u64)
        .find(|&s| {
            let r = eve_server::rng::MechanicRng::seeded(s);
            (0..4).all(|_| r.roll(0.20))
        })
        .expect("escalation seed");

    let anoms = rpc(&ctx, &mut sess_a, "scanSvc", "GetCombatAnomalyInstances", &[])
        .await
        .unwrap();
    let site_id = match anoms {
        PyRep::List(ref list) => list
            .iter()
            .find_map(|item| dict_get(item, "siteID").and_then(|v| v.as_str()).map(str::to_string)),
        _ => None,
    }
    .expect("Jita must have combat anomalies");

    let sys_jita = ctx.systems.get_or_spawn(SYS_JITA);
    for _ in 0..30 {
        let npc_ids: Vec<i64> = eve_server::services::pve_exploration::with_pve_exploration_reg(&ctx, |reg| {
            reg.sites_by_system
                .get(&SYS_JITA)
                .and_then(|l| l.iter().find(|s| s.site_id == site_id))
                .map(|s| s.active_npc_ball_ids.clone())
                .unwrap_or_default()
        });
        if npc_ids.is_empty() {
            break;
        }
        for nid in npc_ids {
            if let Ok(Some((_, nb))) = sys_jita.get_ball_snapshot(nid).await {
                sys_jita
                    .set_ball_position(ship_a, nb.position + DVec3::new(1_000.0, 0.0, 0.0))
                    .await;
            }
            let _ = sys_jita
                .add_target_lock(
                    ship_a,
                    TargetLock {
                        target_id: nid,
                        lock_time_ms: 100.0,
                        remaining_lock_ms: 0.0,
                        locked: true,
                        max_range_m: None,
                    },
                )
                .await;
            for _ in 0..5 {
                if sys_jita.get_ball_snapshot(nid).await.ok().flatten().is_none() {
                    break;
                }
                ctx.mechanic_rng.set_seed(Some(esc_seed));
                for gid in [gun_a, gun_b, gun_c] {
                    if sys_jita.get_ball_snapshot(nid).await.ok().flatten().is_none() {
                        break;
                    }
                    let _ = rpc(
                        &ctx,
                        &mut sess_a,
                        "dogmaIM",
                        "Activate",
                        &[
                            PyRep::Integer(gid),
                            PyRep::String("projectileFired".into()),
                            PyRep::Integer(nid),
                        ],
                    )
                    .await;
                }
                if sys_jita.get_ball_snapshot(nid).await.ok().flatten().is_none() {
                    break;
                }
                ctx.advance_secs(5);
                let _ = sys_jita.step_manual(5.0).await;
            }
        }
    }

    let esc = eve_server::services::pve_exploration::with_pve_exploration_reg(&ctx, |reg| {
        reg.escalations_by_char
            .get(&PILOT_A)
            .and_then(|l| l.first().cloned())
    })
    .expect("anomaly must escalate");
    let dest_notifs = drain_all(&ctx, PILOT_B);
    assert!(
        dest_notifs.iter().any(|(e, _)| e == "OnDungeonInstanceAdded"),
        "OnDungeonInstanceAdded must be emitted in destination system"
    );

    // Move Pilot A to dest_sys and clear the expedition site via real weapon fire
    let sys_dest = ctx.systems.get_or_spawn(dest_sys);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.solar_system_id = dest_sys;
        }
    });
    sess_a.solar_system_id = Some(dest_sys);
    sys_dest
        .spawn_ball_with_meta(
            Ball {
                id: ship_a,
                type_id: 638,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Rigid,
            },
            ctx.build_ship_combat_state(PILOT_A, ship_a, 638),
            eve_destiny::SlimItemMeta {
                group_id: 25,
                category_id: 6,
                owner_id: PILOT_A,
                char_id: Some(PILOT_A),
                corp_id: 1000009,
                alliance_id: -1,
                security_status: 0.0,
                is_cloaked: false,
                ..Default::default()
            },
        )
        .await
        .unwrap();

    for _ in 0..30 {
        let npc_ids: Vec<i64> = eve_server::services::pve_exploration::with_pve_exploration_reg(&ctx, |reg| {
            reg.sites_by_system
                .get(&dest_sys)
                .and_then(|l| l.iter().find(|s| s.expedition_instance_id == Some(esc.instance_id)))
                .map(|s| s.active_npc_ball_ids.clone())
                .unwrap_or_default()
        });
        if npc_ids.is_empty() {
            break;
        }
        for nid in npc_ids {
            if let Ok(Some((_, nb))) = sys_dest.get_ball_snapshot(nid).await {
                sys_dest
                    .set_ball_position(ship_a, nb.position + DVec3::new(1_000.0, 0.0, 0.0))
                    .await;
            }
            let _ = sys_dest
                .add_target_lock(
                    ship_a,
                    TargetLock {
                        target_id: nid,
                        lock_time_ms: 100.0,
                        remaining_lock_ms: 0.0,
                        locked: true,
                        max_range_m: None,
                    },
                )
                .await;
            for _ in 0..5 {
                if sys_dest.get_ball_snapshot(nid).await.ok().flatten().is_none() {
                    break;
                }
                for gid in [gun_a, gun_b, gun_c] {
                    if sys_dest.get_ball_snapshot(nid).await.ok().flatten().is_none() {
                        break;
                    }
                    let _ = rpc(
                        &ctx,
                        &mut sess_a,
                        "dogmaIM",
                        "Activate",
                        &[
                            PyRep::Integer(gid),
                            PyRep::String("projectileFired".into()),
                            PyRep::Integer(nid),
                        ],
                    )
                    .await;
                }
                if sys_dest.get_ball_snapshot(nid).await.ok().flatten().is_none() {
                    break;
                }
                ctx.advance_secs(5);
                let _ = sys_dest.step_manual(5.0).await;
            }
        }
    }
    let dest_after = drain_all(&ctx, PILOT_B);
    assert!(
        dest_after.iter().any(|(e, _)| e == "OnDungeonInstanceRemoved"),
        "OnDungeonInstanceRemoved must be emitted when expedition step completes"
    );
}

// parity: notify:OnFacWarBunkerVulnerable
#[tokio::test]
async fn test_batch3_facwar_bunker_vulnerable() {
    eve_testkit::attest(&["notify:OnFacWarBunkerVulnerable"]);

    let ctx = new_ctx();
    let ship_a = 640_000_601_i64;
    let mut sess_a = login_in_space(
        &ctx,
        92_050,
        "demo",
        "demo-secret",
        PILOT_A,
        ship_a,
        587,
        SYS_JITA,
        DVec3::ZERO,
    )
    .await;

    let _ = rpc(
        &ctx,
        &mut sess_a,
        "fwWarzoneSolarsystem",
        "GetAllWarzonesOccupationStates",
        &[],
    )
    .await
    .unwrap();
    let (fw_sys, attacker_fac) = ctx.db.with_snapshot(|snap| {
        snap.fw_system_occupations
            .iter()
            .filter(|(&sys, _)| {
                eve_server::services::fw_services::compute_system_frontline_status(&ctx, snap, sys)
                    == eve_server::services::fw_services::STATUS_FRONTLINE
            })
            .max_by_key(|(_, occ)| occ.victory_points)
            .map(|(&sys, occ)| {
                let atk = match occ.occupier_faction_id {
                    500_001 => 500_004,
                    500_004 => 500_001,
                    500_002 => 500_003,
                    _ => 500_002,
                };
                (sys, atk)
            })
            .expect("Frontline FW system occupation")
    });
    rpc(
        &ctx,
        &mut sess_a,
        "fwCharacterEnlistmentMgr",
        "CreateMyDirectEnlistment",
        &[PyRep::Integer(attacker_fac)],
    )
    .await
    .unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.solar_system_id = fw_sys;
        }
    });
    sess_a.solar_system_id = Some(fw_sys);
    let sys_fw = ctx.systems.get_or_spawn(fw_sys);
    let zone_pos = DVec3::from_array(eve_server::services::fw_gameplay::BATTLEFIELD_ZONE_CENTERS[0]);
    sys_fw
        .spawn_ball_with_meta(
            Ball {
                id: ship_a,
                type_id: 587,
                position: zone_pos,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Rigid,
            },
            ctx.build_ship_combat_state(PILOT_A, ship_a, 587),
            eve_destiny::SlimItemMeta {
                group_id: 25,
                category_id: 6,
                owner_id: PILOT_A,
                char_id: Some(PILOT_A),
                corp_id: 1000009,
                alliance_id: -1,
                security_status: 0.0,
                is_cloaked: false,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    drain_all(&ctx, PILOT_A);

    // Capture Frontline Battlefields (2,000 VP each; 4h respawn cooldown) until 100% contested
    for _ in 0..40 {
        eve_server::services::fw_gameplay::tick_fw_gameplay(&ctx, ctx.now_unix()).await;
        ctx.clock().advance_secs(905);
        eve_server::services::fw_gameplay::tick_fw_gameplay(&ctx, ctx.now_unix()).await;
        let vuln = ctx
            .db
            .with_snapshot(|s| s.fw_runtime.ihub_vulnerable.get(&fw_sys).copied().unwrap_or(false));
        if vuln {
            break;
        }
        ctx.clock().advance_secs(eve_server::services::fw_gameplay::BATTLEFIELD_RESPAWN_COOLDOWN_SECS + 5);
    }
    let fw_notifs = drain_all(&ctx, PILOT_A);
    assert!(
        fw_notifs.iter().any(|(e, _)| e == "OnFacWarBunkerVulnerable"),
        "OnFacWarBunkerVulnerable must be emitted when FW system reaches 100% contested"
    );
}

// parity: notify:OnInfluenceUpdate
#[tokio::test]
async fn test_batch3_incursion_influence_update() {
    eve_testkit::attest(&["notify:OnInfluenceUpdate"]);

    let ctx = new_ctx();
    let mut gm_sess = login_in_space(
        &ctx,
        92_060,
        "demo",
        "demo-secret",
        PILOT_A,
        640_000_701,
        587,
        SYS_JITA,
        DVec3::ZERO,
    )
    .await;

    eve_server::services::pve_special_sites::advance_pve_special_clock(&ctx, 1);
    let (inc_sys, site_id, boss_id) = eve_server::services::pve_special_sites::with_pve_special_reg(&ctx, |reg| {
        reg.incursions.values().next().and_then(|inc| {
            inc.sites
                .iter()
                .find(|s| !s.locked_until_low_influence)
                .map(|s| (s.solar_system_id, s.site_id.clone(), s.boss_npc_ball_id))
        })
    })
    .expect("spawned incursion site");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.solar_system_id = inc_sys;
        }
    });
    gm_sess.solar_system_id = Some(inc_sys);
    drain_all(&ctx, PILOT_A);

    for _ in 0..10 {
        let esc_ids: Vec<i64> = eve_server::services::pve_special_sites::with_pve_special_reg(&ctx, |reg| {
            reg.incursions
                .values()
                .flat_map(|inc| inc.sites.iter())
                .find(|s| s.site_id == site_id)
                .map(|s| {
                    s.escort_spawns
                        .iter()
                        .filter(|e| !e.destroyed)
                        .map(|e| e.npc_ball_id)
                        .collect()
                })
                .unwrap_or_default()
        });
        if esc_ids.is_empty() {
            break;
        }
        for eid in esc_ids {
            let _ = eve_server::services::pve_incursion_sites::on_incursion_boss_destroyed(
                &ctx,
                inc_sys,
                eid,
                Some(PILOT_A),
            )
            .await;
        }
    }
    let sys_inc = ctx.systems.get_or_spawn(inc_sys);
    sys_inc.remove_ball(boss_id).await;
    eve_server::services::pve_incursion_sites::on_incursion_boss_destroyed(
        &ctx,
        inc_sys,
        boss_id,
        Some(PILOT_A),
    )
    .await
    .expect("boss destroyed")
    .expect("site completed");
    let inc_notifs = drain_all(&ctx, PILOT_A);
    let inf_ev = inc_notifs
        .iter()
        .find(|(e, _)| e == "OnInfluenceUpdate")
        .expect("OnInfluenceUpdate must be emitted on incursion site completion");
    let inf_val = dict_get(&inf_ev.1[1], "influence")
        .and_then(|v| v.as_f64())
        .expect("influence float");
    assert!((0.0..1.0).contains(&inf_val));
}
