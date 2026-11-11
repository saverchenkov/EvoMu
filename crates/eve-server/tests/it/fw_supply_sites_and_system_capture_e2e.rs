//! Non-GM E2E integration tests for Shard 3 parity remediation (FW Supply Sites & System Capture).
#![forbid(unsafe_code)]
#![allow(dead_code)]

use crate::sov_and_insurgency_tail_helpers::*;
use eve_db::WorldRepository;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::fw_gameplay::{
    complex_base_capture_duration_seconds, complex_victory_points, pocket_pos_for_complex,
    TYPE_FW_IHUB,
};
use eve_server::services::fw_services::{
    compute_system_frontline_status, STATUS_FRONTLINE,
};
use eve_server::sp6_lifecycle::tick_background_cluster;
use glam::DVec3;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

fn cached_sde() -> Arc<eve_sde::SdeRegistry> {
    static FAST_SDE: OnceLock<Arc<eve_sde::SdeRegistry>> = OnceLock::new();
    Arc::clone(FAST_SDE.get_or_init(|| Arc::new(eve_sde::SdeRegistry::with_canonical_seed())))
}

fn fast_new_sqlite_ctx(label: &str) -> (PathBuf, Arc<ServerContext>) {
    static TMPL_SQLITE: OnceLock<PathBuf> = OnceLock::new();
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let tmpl = TMPL_SQLITE.get_or_init(|| {
        let p = std::env::temp_dir().join(format!("evomu_supply_tmpl_{}.sqlite", std::process::id()));
        let _ = std::fs::remove_file(&p);
        let repo = WorldRepository::open_sqlite(&p).expect("open sqlite template");
        drop(repo);
        p
    });
    let dir = std::env::temp_dir().join(format!(
        "evomu_shard3_{label}_{}_{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let db_path = dir.join(format!("{label}.sqlite"));
    std::fs::copy(tmpl, &db_path).expect("copy sqlite template");
    let repo = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open sqlite"));
    let ctx = Arc::new(
        ServerContext::with_sde_and_repository(cached_sde(), repo)
            .with_seeded_rng(eve_server::rng::TEST_MODE_DEFAULT_SEED),
    );
    (db_path, ctx)
}

fn fast_reload_ctx_from_sqlite(ctx: &ServerContext, path: &Path) -> Arc<ServerContext> {
    ctx.db.save_to_sqlite(path).expect("flush sqlite world");
    let repo = Arc::new(WorldRepository::open_sqlite(path).expect("reopen sqlite world"));
    let ctx2 = Arc::new(
        ServerContext::with_sde_and_repository(cached_sde(), repo)
            .with_seeded_rng(eve_server::rng::TEST_MODE_DEFAULT_SEED),
    );
    crate::common::pin_clock(&ctx2);
    ctx2
}

fn fast_forward_secs(ctx: &ServerContext, secs: u64) {
    ctx.clock().advance_secs(secs);
    let cur = ctx.now_unix_secs();
    ctx.db.with_mut_snapshot(|snap| {
        snap.service_state_blobs
            .insert("__server_now_unix".into(), cur.to_string());
    });
}

async fn fast_lock_player_target(
    ctx: &ServerContext,
    sess: &mut SessionState,
    sys_id: u32,
    ship_id: i64,
    target_id: i64,
) {
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let already = sys_h
        .get_target_locks(ship_id)
        .await
        .unwrap_or_default()
        .into_iter()
        .find(|l| l.target_id == target_id);
    if already.is_none() {
        call_rpc(
            ctx,
            sess,
            "dogmaIM",
            "AddTarget",
            &[PyRep::Integer(target_id)],
        )
        .await
        .expect("dogmaIM.AddTarget");
    }
    for _ in 0..8 {
        let locks = sys_h.get_target_locks(ship_id).await.unwrap_or_default();
        if locks.iter().any(|l| l.target_id == target_id && l.locked) {
            return;
        }
        let _ = sys_h.step_manual(5.0).await;
        fast_forward_secs(ctx, 5);
    }
    panic!("Failed to complete target lock on {target_id}");
}

async fn fast_destroy_ball_with_player_weapons(
    ctx: &ServerContext,
    pilot: &mut PlayerPilot,
    sys_id: u32,
    target_id: i64,
) {
    move_player_near_ball(ctx, &mut pilot.sess, sys_id, pilot.ship_id, target_id, 1_000.0).await;
    fast_lock_player_target(ctx, &mut pilot.sess, sys_id, pilot.ship_id, target_id).await;
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let guns: Vec<i64> = ctx.db.with_snapshot(|s| {
        s.items
            .values()
            .filter(|i| {
                i.location_id == pilot.ship_id
                    && ctx.sde.types.get(&i.type_id).is_some_and(|t| t.group_id == 55)
            })
            .map(|i| i.item_id)
            .collect()
    });
    for _ in 0..45 {
        let destroyed_struct = ctx
            .db
            .with_snapshot(|s| s.structures.get(&target_id).is_some_and(|st| st.state == "Destroyed"));
        let alive_ball = sys_h.get_ball_snapshot(target_id).await.ok().flatten().is_some();
        if destroyed_struct || !alive_ball {
            break;
        }
        let mut reloading = false;
        for &gid in &guns {
            let gone = ctx
                .db
                .with_snapshot(|s| s.structures.get(&target_id).is_some_and(|st| st.state == "Destroyed"))
                || sys_h.get_ball_snapshot(target_id).await.ok().flatten().is_none();
            if gone {
                return;
            }
            if let Err(e) = call_rpc(
                ctx,
                &mut pilot.sess,
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
                    "ModuleIsReloading" => reloading = true,
                    "ModuleAlreadyActive" => {}
                    "TargetNotLocked" => {
                        let still_alive = ctx.db.with_snapshot(|s| {
                            !s.structures.get(&target_id).is_some_and(|st| st.state == "Destroyed")
                        }) && sys_h.get_ball_snapshot(target_id).await.ok().flatten().is_some();
                        if !still_alive {
                            return;
                        }
                        fast_lock_player_target(ctx, &mut pilot.sess, sys_id, pilot.ship_id, target_id).await;
                    }
                    _ => panic!("Unexpected weapon Activate error on {target_id}: {e:?}"),
                }
            }
        }
        let step_secs = if reloading { 12 } else { 10 };
        let _ = sys_h.step_manual(step_secs as f64).await;
        fast_forward_secs(ctx, step_secs);
    }
}

// parity: feat:fw.supply-caches-depots
#[tokio::test]
async fn test_e2e_fw_supply_caches_depots_and_advantage_persistence() {
    eve_testkit::attest(&["feat:fw.supply-caches-depots"]);

    let (db_path, ctx) = fast_new_sqlite_ctx("fw_supply");
    crate::common::pin_clock(&ctx);

    let mut pilot =
        spawn_player_combat_pilot(&ctx, 81_001, "fw_supply_p1", "Caldari Raider", HURRICANE_TYPE, false).await;
    call_rpc(
        &ctx,
        &mut pilot.sess,
        "facWarMgr",
        "JoinFactionAsCharacter",
        &[PyRep::Integer(CALDARI)],
    )
    .await
    .expect("JoinFactionAsCharacter");

    let sys_id = ctx
        .db
        .with_snapshot(|s| {
            s.fw_system_occupations
                .values()
                .filter(|o| o.occupier_faction_id == GALLENTE && o.victory_points == 0)
                .map(|o| o.solar_system_id)
                .filter(|&sid| compute_system_frontline_status(&ctx, s, sid) == STATUS_FRONTLINE)
                .min()
        })
        .expect("Gallente frontline warzone system");
    let _gate_id = ctx
        .sde
        .stargates()
        .values()
        .find(|g| g.from_solar_system_id == pilot.sess.solar_system_id.unwrap() && g.to_solar_system_id == sys_id)
        .map(|g| i64::from(g.stargate_id))
        .unwrap_or(0);
    ctx.db.with_mut_snapshot(|s| {
        let ch = s.characters.get_mut(&pilot.char_id).unwrap();
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        let ship = s.items.get_mut(&pilot.ship_id).unwrap();
        ship.location_id = i64::from(sys_id);
        ship.flag = eve_db::InventoryFlag::Hangar;
    });
    pilot.sess.solar_system_id = Some(sys_id);
    pilot.sess.station_id = None;
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    if sys_h.get_ball_snapshot(pilot.ship_id).await.unwrap().is_none() {
        let pos = ctx
            .sde
            .stargates()
            .values()
            .find(|g| g.from_solar_system_id == sys_id)
            .map(|g| g.position)
            .unwrap_or(DVec3::ZERO)
            + DVec3::new(2_500.0, 0.0, 0.0);
        let _ = sys_h
            .spawn_ball(eve_destiny::Ball {
                id: pilot.ship_id,
                type_id: HURRICANE_TYPE,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 200.0,
                inertia: 3.0,
                mass: 1.5e7,
                radius: 200.0,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            })
            .await;
    }
    tick_background_cluster(&ctx).await;
    eve_server::services::fw_gameplay::ensure_warzone_sites_spawned(&ctx, sys_id).await;

    let (cache_id, cache_tid, has_core, fuel_qty) = ctx.db.with_snapshot(|s| {
        let (&sid, _) = s
            .fw_runtime
            .supply_sites
            .iter()
            .find(|(_, (ssys, kind, _))| *ssys == sys_id && kind == "SupplyCache")
            .expect("spawned SupplyCache");
        let st = &s.structures[&sid];
        (sid, st.type_id, st.quantum_core_installed, st.fuel_blocks)
    });
    assert_eq!(cache_tid, 93_877, "Warpath Supply Cache type must be 93877");
    assert_eq!(has_core, false, "Supply cache must not carry Upwell quantum core");
    assert_eq!(fuel_qty, 0, "Supply cache must not carry Upwell fuel blocks");

    let _ = ctx.drain_notifications_for_char(pilot.char_id);
    fast_destroy_ball_with_player_weapons(&ctx, &mut pilot, sys_id, cache_id).await;

    let (st_state, can_id, insignia_qty, supply_qty, caldari_adv) = ctx.db.with_snapshot(|s| {
        let state = s.structures[&cache_id].state.clone();
        let can_id = s
            .items
            .values()
            .find(|i| i.location_id == i64::from(sys_id) && i.type_id == 23)
            .map(|i| i.item_id)
            .expect("spawned loot cargo container");
        let ins_q = s
            .items
            .values()
            .filter(|i| i.location_id == can_id && i.type_id == 15_587)
            .map(|i| i.quantity)
            .sum::<i32>();
        let sup_q = s
            .items
            .values()
            .filter(|i| i.location_id == can_id && i.type_id == 88_103)
            .map(|i| i.quantity)
            .sum::<i32>();
        let (_, cal_adv) = s.fw_runtime.system_advantage.get(&sys_id).copied().unwrap_or((0.0, 0.0));
        (state, can_id, ins_q, sup_q, cal_adv)
    });
    assert_eq!(st_state, "Destroyed", "Supply Cache destroyed via weapons");
    assert_ne!(can_id, 0, "Cargo container spawned in space");
    assert_eq!(insignia_qty, 2, "Federation Navy Midshipman Insignia I dropped inside container");
    assert_eq!(supply_qty, 5, "Militia supply crates dropped inside container");
    assert_eq!(caldari_adv, 2.0, "Supply Cache destruction awards +2.0% Advantage");

    let ctx2 = fast_reload_ctx_from_sqlite(&ctx, &db_path);
    let persisted_adv = ctx2
        .db
        .with_snapshot(|s| s.fw_runtime.system_advantage.get(&sys_id).map(|v| v.1).unwrap_or(0.0));
    assert_eq!(persisted_adv, 2.0, "FW Advantage must persist across SQLite reload");
}

// parity: feat:fw.system-capture
// parity: notify:OnCaptureChanged
// parity: notify:OnFacWarBunkerVulnerable
// parity: notify:OnSolarSystemLPChange
// parity: notify:OnVictoryPointStateUpdated
#[tokio::test]
async fn test_e2e_fw_system_capture_ihub_siege_downtime_and_notifications() {
    eve_testkit::attest(&[
        "feat:fw.system-capture",
        "notify:OnCaptureChanged",
        "notify:OnFacWarBunkerVulnerable",
        "notify:OnSolarSystemLPChange",
        "notify:OnVictoryPointStateUpdated",
    ]);

    let (db_path, ctx) = fast_new_sqlite_ctx("fw_capture");
    crate::common::pin_clock(&ctx);

    let mut pilot =
        spawn_player_combat_pilot(&ctx, 81_002, "fw_cap_p1", "State Vanguard", HURRICANE_TYPE, false).await;
    call_rpc(
        &ctx,
        &mut pilot.sess,
        "facWarMgr",
        "JoinFactionAsCharacter",
        &[PyRep::Integer(CALDARI)],
    )
    .await
    .expect("JoinFactionAsCharacter");

    let target = ctx
        .db
        .with_snapshot(|s| {
            s.fw_system_occupations
                .values()
                .filter(|o| o.occupier_faction_id == GALLENTE && o.victory_points == 0)
                .map(|o| o.solar_system_id)
                .filter(|&sid| compute_system_frontline_status(&ctx, s, sid) == STATUS_FRONTLINE)
                .min()
        })
        .expect("Gallente frontline system");

    // Earn militia LP via live FW plexing & donate to I-Hub -> OnSolarSystemLPChange
    ctx.db.with_mut_snapshot(|s| {
        let ch = s.characters.get_mut(&pilot.char_id).unwrap();
        ch.solar_system_id = target;
        ch.station_id = None;
        let ship = s.items.get_mut(&pilot.ship_id).unwrap();
        ship.location_id = i64::from(target);
        ship.flag = eve_db::InventoryFlag::Hangar;
    });
    pilot.sess.solar_system_id = Some(target);
    pilot.sess.station_id = None;
    let sys_h = ctx.systems.get_or_spawn(target);
    let _ = sys_h
        .spawn_ball(eve_destiny::Ball {
            id: pilot.ship_id,
            type_id: HURRICANE_TYPE,
            position: DVec3::new(10_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 200.0,
            inertia: 3.0,
            mass: 1.5e7,
            radius: 200.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await;
    tick_background_cluster(&ctx).await;

    // Complete complex 1 through live gate activation, guard kill, and capture hold -> LP + VP + OnCaptureChanged
    let (gate1, tier1, gate1_pos) = ctx.db.with_snapshot(|s| {
        s.fw_runtime
            .fw_complexes
            .iter()
            .find(|(_, (csys, t, _, _, done))| *csys == target && !*done && t == "Large")
            .map(|(&gid, (_, t, ..))| {
                (
                    gid,
                    t.clone(),
                    DVec3::from_array(s.item_positions.get(&gid).copied().unwrap_or([0.0, 0.0, 0.0])),
                )
            })
            .expect("spawned FW complex")
    });
    sys_h.set_ball_position(pilot.ship_id, gate1_pos + DVec3::new(1_000.0, 0.0, 0.0)).await;
    call_rpc(
        &ctx,
        &mut pilot.sess,
        "keeper",
        "ActivateAccelerationGate",
        &[PyRep::Integer(gate1)],
    )
    .await
    .expect("ActivateAccelerationGate");
    fast_destroy_ball_with_player_weapons(&ctx, &mut pilot, target, gate1 + 500_000_000).await;
    sys_h.set_ball_position(pilot.ship_id, pocket_pos_for_complex(gate1)).await;
    let hold1 = complex_base_capture_duration_seconds(&tier1);
    let _ = ctx.drain_notifications_for_char(pilot.char_id);
    fast_forward_secs(&ctx, hold1 + 5);
    tick_background_cluster(&ctx).await;

    let vp_after_c1 = ctx.db.with_snapshot(|s| s.fw_system_occupations[&target].victory_points);
    assert_eq!(vp_after_c1, complex_victory_points(&tier1), "Complex capture adds exact tier VP");
    let c1_notifs = ctx.drain_notifications_for_char(pilot.char_id);
    assert!(
        c1_notifs.iter().any(|n| n.event == "OnCaptureChanged" && n.args.first().and_then(PyRep::as_i64) == Some(gate1)),
        "Complex capture emits OnCaptureChanged"
    );
    assert!(
        c1_notifs.iter().any(|n| n.event == "OnVictoryPointStateUpdated"),
        "Complex capture emits OnVictoryPointStateUpdated"
    );

    // Donate earned militia LP via facWarMgr.DonateLPsToSolarSystem -> OnSolarSystemLPChange
    let (lp_before_donate, friendly_fw_sys) = ctx.db.with_snapshot(|s| {
        let lp = s.characters[&pilot.char_id].loyalty_points.values().sum::<i64>();
        let cal_sys = s
            .fw_system_occupations
            .values()
            .filter(|o| o.occupier_faction_id == CALDARI)
            .map(|o| o.solar_system_id)
            .min()
            .expect("Caldari occupied warzone system");
        (lp, cal_sys)
    });
    assert!(lp_before_donate >= 1_000, "Complex capture awards genuine LP ({lp_before_donate})");
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&pilot.char_id).unwrap().solar_system_id = friendly_fw_sys;
    });
    pilot.sess.solar_system_id = Some(friendly_fw_sys);
    let donate_amt = lp_before_donate.min(45_000);
    let don_res = call_rpc(
        &ctx,
        &mut pilot.sess,
        "facWarMgr",
        "DonateLPsToSolarSystem",
        &[PyRep::Integer(i64::from(friendly_fw_sys)), PyRep::Integer(donate_amt)],
    )
    .await
    .expect("DonateLPsToSolarSystem");
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&pilot.char_id).unwrap().solar_system_id = target;
    });
    pilot.sess.solar_system_id = Some(target);
    assert_eq!(
        don_res.dict_get("totalLP").and_then(PyRep::as_i64),
        Some(donate_amt),
        "DonateLPsToSolarSystem updates system LP buffer"
    );
    let lp_notifs = ctx.drain_notifications_for_char(pilot.char_id);
    assert!(
        lp_notifs
            .iter()
            .any(|n| n.event == "OnSolarSystemLPChange" && n.args.get(1).and_then(PyRep::as_i64) == Some(donate_amt)),
        "DonateLPsToSolarSystem emits OnSolarSystemLPChange"
    );

    let advance_avoiding_downtime = |ctx: &eve_server::router::ServerContext, secs: u64| {
        fast_forward_secs(ctx, secs);
        let dt_sec = ctx.systems.downtime_utc_secs_of_day();
        let mod_day = ctx.now_unix() % 86_400;
        let since_dt = (86_400 + mod_day - dt_sec) % 86_400;
        if since_dt <= 1_000 {
            fast_forward_secs(ctx, 1_050 - since_dt);
        }
    };

    let find_next_c = || {
        ctx.db.with_snapshot(|s| {
            s.fw_runtime
                .fw_complexes
                .iter()
                .find(|(_, (csys, t, _, _, done))| {
                    *csys == target && !*done && (t == "Medium" || t == "Large" || t == "Open")
                })
                .map(|(&gid, (_, t, ..))| {
                    (
                        gid,
                        t.clone(),
                        DVec3::from_array(s.item_positions.get(&gid).copied().unwrap_or([0.0, 0.0, 0.0])),
                    )
                })
        })
    };

    let mut all_notifs = Vec::new();
    // Contested to 100% via live Frontline Battlefield & complex captures -> OnFacWarBunkerVulnerable
    for _ in 0..45 {
        all_notifs.extend(ctx.drain_notifications_for_char(pilot.char_id));
        let (vp_cur, thresh) = ctx.db.with_snapshot(|s| {
            let o = &s.fw_system_occupations[&target];
            (o.victory_points, o.victory_points_threshold)
        });
        if vp_cur >= thresh {
            break;
        }
        eve_server::services::fw_gameplay::ensure_warzone_sites_spawned(&ctx, target).await;
        let has_bf = ctx.db.with_snapshot(|s| {
            s.fw_runtime
                .battlefield_instances
                .iter()
                .any(|b| b.solar_system_id == target && !b.completed)
        });
        if has_bf {
            let z0 = DVec3::from_array(
                eve_server::services::fw_gameplay::battlefield_zone_centers_for_system(&ctx, target)[0],
            );
            sys_h.set_ball_position(pilot.ship_id, z0).await;
            advance_avoiding_downtime(&ctx, 905);
            tick_background_cluster(&ctx).await;
            all_notifs.extend(ctx.drain_notifications_for_char(pilot.char_id));
            let vp_now = ctx.db.with_snapshot(|s| s.fw_system_occupations[&target].victory_points);
            if vp_now >= thresh {
                break;
            }
            advance_avoiding_downtime(&ctx, 14_405);
            tick_background_cluster(&ctx).await;
            continue;
        }
        let Some((gid, tname, gpos)) = find_next_c() else {
            advance_avoiding_downtime(&ctx, 1_805);
            tick_background_cluster(&ctx).await;
            continue;
        };
        sys_h.set_ball_position(pilot.ship_id, gpos + DVec3::new(1_000.0, 0.0, 0.0)).await;
        let _ = call_rpc(&ctx, &mut pilot.sess, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(gid)]).await;
        if sys_h.get_ball_snapshot(gid + 500_000_000).await.ok().flatten().is_some() {
            fast_destroy_ball_with_player_weapons(&ctx, &mut pilot, target, gid + 500_000_000).await;
        }
        sys_h.set_ball_position(pilot.ship_id, pocket_pos_for_complex(gid)).await;
        let h_sec = complex_base_capture_duration_seconds(&tname);
        advance_avoiding_downtime(&ctx, h_sec + 5);
        tick_background_cluster(&ctx).await;
        all_notifs.extend(ctx.drain_notifications_for_char(pilot.char_id));
    }

    let vuln_notifs = all_notifs;
    assert!(
        vuln_notifs.iter().any(|n| n.event == "OnFacWarBunkerVulnerable"),
        "100% contested threshold must emit OnFacWarBunkerVulnerable"
    );

    // Siege and destroy FW I-Hub via player weapons -> occupier remains Gallente until daily downtime
    let (ihub_id, ihub_pos) = ctx.db.with_snapshot(|s| {
        let st = s
            .structures
            .values()
            .find(|st| st.solar_system_id == target && st.type_id == TYPE_FW_IHUB && st.state != "Destroyed")
            .expect("FW I-Hub");
        (st.structure_id, DVec3::from_array(st.position))
    });
    if sys_h.get_ball_snapshot(ihub_id).await.ok().flatten().is_none() {
        let _ = sys_h
            .spawn_ball(eve_destiny::Ball {
                id: ihub_id,
                type_id: TYPE_FW_IHUB,
                position: ihub_pos,
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 1e8,
                radius: 2_500.0,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            })
            .await;
    }
    fast_destroy_ball_with_player_weapons(&ctx, &mut pilot, target, ihub_id).await;
    assert_eq!(ctx.db.with_snapshot(|s| s.fw_system_occupations[&target].occupier_faction_id), GALLENTE);

    let dt_sec = ctx.systems.downtime_utc_secs_of_day();
    let until_dt = (86_400 + dt_sec - ctx.now_unix() % 86_400) % 86_400;
    ctx.advance_secs(until_dt + 20);
    assert_eq!(
        ctx.db.with_snapshot(|s| s.fw_system_occupations[&target].occupier_faction_id),
        CALDARI,
        "Occupier flips to Caldari after daily downtime"
    );

    let ctx2 = fast_reload_ctx_from_sqlite(&ctx, &db_path);
    assert_eq!(
        ctx2.db.with_snapshot(|s| s.fw_system_occupations[&target].occupier_faction_id),
        CALDARI,
        "Captured occupier must persist across SQLite reload"
    );
}

