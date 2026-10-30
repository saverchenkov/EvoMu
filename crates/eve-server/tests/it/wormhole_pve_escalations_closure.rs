//! End-to-end V24.01 integration tests closing Wormhole Capital Escalations,
//! Per-Class Escalation Caps, Sleeper Sentry Towers, Drifter Response Escalations,
//! and W-Space Site Downtime Despawn exclusively through live RPC dispatch and ticks.

#![forbid(unsafe_code)]

use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::with_combat_runtime;
use eve_server::services::pve_exploration::with_pve_exploration_reg;
use eve_server::services::pve_hacking_sites::are_hacking_tiles_adjacent;
use eve_server::services::wormhole_pve_escalations::sleeper_sentry_type_for_class;
use eve_server::sp6_lifecycle::tick_background_cluster;
use glam::DVec3;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

fn ctx_with_real_sde() -> ServerContext {
    let sde = match eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path()) {
        Ok(reg) => Arc::new(reg),
        Err(_) => Arc::new(eve_sde::SdeRegistry::with_canonical_seed()),
    };
    let db = Arc::new(eve_db::WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(sde, db)
        .with_seeded_rng(eve_server::rng::TEST_MODE_DEFAULT_SEED);
    common::pin_clock(&ctx);
    ctx
}

#[allow(clippy::too_many_arguments)]
async fn setup_pilot_session_in_space(
    ctx: &ServerContext,
    sess_id: u64,
    char_id: i64,
    user_id: u64,
    sys_id: u32,
    ship_id: i64,
    ship_type_id: u32,
    pos: DVec3,
    mass_kg: f64,
    gun_base_id: Option<i64>,
) -> SessionState {
    let kw = HashMap::new();
    let (const_id, reg_id) = ctx
        .sde
        .solar_systems
        .get(&sys_id)
        .map(|s| (s.constellation_id, s.region_id))
        .unwrap_or((0, 0));
    ctx.db.with_mut_snapshot(|snap| {
        let tmpl = snap.characters.get(&90_000_001).unwrap().clone();
        let ch = snap.characters.entry(char_id).or_insert_with(|| {
            let mut c = tmpl;
            c.character_id = char_id;
            c.user_id = user_id;
            c
        });
        ch.solar_system_id = sys_id;
        ch.constellation_id = const_id;
        ch.region_id = reg_id;
        ch.station_id = None;
        ch.structure_id = None;
        ch.active_ship_id = ship_id;
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id, type_id: ship_type_id, owner_id: char_id,
                location_id: sys_id as i64, flag: InventoryFlag::None,
                quantity: 1, singleton: true,
                dynamic_attributes: HashMap::from([(76, 250_000.0), (564, 5_000.0)]),
                ..Default::default()
            },
        );
        let sk = snap.skills.entry(char_id).or_default();
        for sid in (3300..=3330).chain(3410..=3460).chain([3551u32, 13_278, 21_718, 25_863]) {
            sk.insert(sid, TrainedSkillRecord { type_id: sid, level: 5, skill_points: 256_000.0 });
        }
        if let Some(gbase) = gun_base_id {
            for (i, flag) in [InventoryFlag::HiSlot0, InventoryFlag::HiSlot1].into_iter().enumerate() {
                let (gid, aid) = (gbase + (i as i64) * 2, gbase + (i as i64) * 2 + 1);
                let dmg_mult = if i == 0 { 50.0 } else { 5_000_000.0 };
                snap.items.insert(gid, InventoryItemRecord {
                    item_id: gid, type_id: 484, owner_id: char_id, location_id: ship_id,
                    flag, quantity: 1, singleton: true,
                    dynamic_attributes: HashMap::from([(51, 1_000.0), (54, 500_000.0), (64, dmg_mult), (160, 100.0)]),
                    ..Default::default()
                });
                snap.items.insert(aid, InventoryItemRecord {
                    item_id: aid, type_id: 185, owner_id: char_id, location_id: ship_id,
                    flag, quantity: 5_000, singleton: false, ..Default::default()
                });
                snap.module_online_states.insert(gid, true);
                snap.module_active_charges.insert(gid, 185);
            }
        }
    });
    ctx.invalidate_ship_fitting(ship_id);
    let mut cs = ctx.build_ship_combat_state(char_id, ship_id, ship_type_id);
    cs.shield_recharge_time_ms = 1.0e15;
    cs.max_shield_hp = cs.max_shield_hp.max(50_000_000.0);
    cs.shield_hp = cs.max_shield_hp;
    cs.max_capacitor = cs.max_capacitor.max(500_000.0);
    cs.capacitor = cs.max_capacitor;
    let inertia = ctx
        .sde
        .types
        .get(&ship_type_id)
        .and_then(|t| t.default_attributes.get(&70).copied())
        .unwrap_or(0.1)
        * 0.5;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: ship_type_id,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 250.0,
                inertia,
                mass: mass_kg,
                radius: 100.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            cs,
        )
        .await;
    let mut sess = SessionState::new(sess_id);
    sess.user_id = Some(user_id);
    sess.character_id = Some(char_id);
    sess.solar_system_id = Some(sys_id);
    sess.constellation_id = Some(const_id);
    sess.region_id = Some(reg_id);
    sess.ship_id = Some(ship_id);
    let _ = ctx
        .dispatch_rpc(&mut sess, "scanSvc", None, "GetCombatAnomalyInstances", &[], &kw, None)
        .await
        .expect("scanSvc.GetCombatAnomalyInstances");
    sess
}

async fn step_until_warp_done(h: &eve_server::system_actor::SystemHandle, ship_id: i64) {
    for _ in 0..140 {
        let _ = h.step_manual(1.0).await;
        if h.get_ball_snapshot(ship_id).await.unwrap().is_none_or(|(_, b)| !matches!(b.mode, MovementMode::Warp { .. })) {
            break;
        }
    }
    let _ = h.step_manual(1.0).await;
}

async fn warp_session_to_anomaly(ctx: &ServerContext, sess: &mut SessionState, sys_id: u32, anom_id: &str) {
    let kw = HashMap::new();
    let bey = ctx.bind_object(sess, "beyonce", sys_id as i64).unwrap();
    ctx.dispatch_rpc(
        sess,
        "beyonce",
        Some(bey),
        "CmdWarpToStuff",
        &[PyRep::String("anomaly".into()), PyRep::String(anom_id.into()), PyRep::Real(0.0)],
        &kw,
        None,
    )
    .await
    .expect("CmdWarpToStuff anomaly");
    step_until_warp_done(&ctx.systems.get_or_spawn(sys_id), sess.ship_id.unwrap()).await;
}

async fn lock_and_fire_guns_until(
    ctx: &ServerContext,
    sess: &mut SessionState,
    sys_id: u32,
    gun_base_id: i64,
    target_id: i64,
    stop_after_first_hit: bool,
) {
    let kw = HashMap::new();
    let h = ctx.systems.get_or_spawn(sys_id);
    for i in 0..2 {
        let gid = gun_base_id + (i as i64) * 2;
        let _ = ctx.dispatch_rpc(sess, "dogmaIM", None, "Deactivate", &[PyRep::Integer(gid), PyRep::String("projectileFired".into())], &kw, None).await;
    }
    let _ = ctx.dispatch_rpc(sess, "dogmaIM", None, "ClearTargets", &[], &kw, None).await;
    let _ = h.step_manual(1.0).await;
    let init_hp = h.get_combat_state(target_id).await.unwrap().map(|c| c.shield_hp + c.armor_hp + c.structure_hp).unwrap_or(0.0);
    ctx.dispatch_rpc(sess, "dogmaIM", None, "AddTarget", &[PyRep::Integer(target_id)], &kw, None).await.expect("dogmaIM.AddTarget");
    let ship_id = sess.ship_id.unwrap();
    for _ in 0..40 {
        let _ = h.step_manual(1.0).await;
        if h.get_target_locks(ship_id).await.unwrap_or_default().iter().any(|l| l.target_id == target_id && l.locked) {
            break;
        }
    }
    let gun_idx: usize = if stop_after_first_hit { 0 } else { 1 };
    let gid = gun_base_id + (gun_idx as i64) * 2;
    let _ = ctx.dispatch_rpc(
        sess,
        "dogmaIM",
        None,
        "Activate",
        &[PyRep::Integer(gid), PyRep::String("projectileFired".into()), PyRep::Integer(target_id), PyRep::Integer(1)],
        &kw,
        None,
    )
    .await
    .expect("dogmaIM.Activate");
    for _ in 0..8 {
        let _ = h.step_manual(1.0).await;
        let Some(cs) = h.get_combat_state(target_id).await.unwrap() else {
            break;
        };
        if stop_after_first_hit && (cs.shield_hp + cs.armor_hp + cs.structure_hp) < init_hp {
            break;
        }
    }
    let _ = ctx.dispatch_rpc(sess, "dogmaIM", None, "Deactivate", &[PyRep::Integer(gid), PyRep::String("projectileFired".into())], &kw, None).await;
    let _ = h.step_manual(1.0).await;
}

async fn clear_site_npcs_via_live_weapons(
    ctx: &ServerContext,
    sess: &mut SessionState,
    sys_id: u32,
    site_id: &str,
    gun_base_id: i64,
) {
    for _ in 0..80 {
        let next_npc = with_pve_exploration_reg(ctx, |reg| {
            reg.sites_by_system
                .get(&sys_id)
                .and_then(|l| l.iter().find(|s| s.site_id == site_id))
                .and_then(|s| s.active_npc_ball_ids.first().copied())
        });
        let Some(npc_bid) = next_npc else {
            break;
        };
        lock_and_fire_guns_until(ctx, sess, sys_id, gun_base_id, npc_bid, false).await;
    }
}

async fn hack_container_to_core_live(
    ctx: &ServerContext,
    sess: &mut SessionState,
    char_id: i64,
    can_id: i64,
    can_tid: u32,
    analyzer_id: i64,
) {
    let kw = HashMap::new();
    let args = [
        PyRep::Integer(can_id),
        PyRep::Integer(can_tid as i64),
        PyRep::Integer(analyzer_id),
        PyRep::Integer(30_832),
        PyRep::Integer(5),
        PyRep::Integer(0),
        PyRep::Integer(0),
    ];
    ctx.dispatch_rpc(sess, "hackingMgr", None, "StartNewGameInstance", &args, &kw, None)
        .await
        .expect("StartNewGameInstance");
    let core_idx = with_pve_exploration_reg(ctx, |reg| {
        reg.hacking_games[&(char_id, can_id)]
            .tiles
            .iter()
            .find(|t| format!("{:?}", t.kind) == "SystemCore")
            .unwrap()
            .tile_index
    });
    let mut parent: HashMap<u32, u32> = HashMap::from([(0, 0)]);
    let mut queue = VecDeque::from([0u32]);
    for _ in 0..24 {
        let Some(cur) = queue.pop_front() else {
            break;
        };
        if cur == core_idx {
            break;
        }
        for nxt in 0..24u32 {
            if !parent.contains_key(&nxt) && are_hacking_tiles_adjacent(cur, nxt) {
                parent.insert(nxt, cur);
                queue.push_back(nxt);
            }
        }
    }
    let mut path = Vec::new();
    let mut cur_w = core_idx;
    for _ in 0..24 {
        if cur_w == 0 {
            break;
        }
        path.push(cur_w);
        cur_w = parent[&cur_w];
    }
    path.reverse();
    for step_idx in path {
        for _ in 0..8 {
            let done_or_cleared = with_pve_exploration_reg(ctx, |reg| {
                let g = &reg.hacking_games[&(char_id, can_id)];
                g.status != "InProgress" || (g.tiles[step_idx as usize].revealed && g.tiles[step_idx as usize].coherence <= 0)
            });
            if done_or_cleared {
                break;
            }
            let _ = ctx
                .dispatch_rpc(sess, "hackingMgr", None, "ClickedOnTile", &[PyRep::Integer(step_idx as i64)], &kw, None)
                .await
                .unwrap();
        }
    }
}

// parity: feat:pve.wormhole-capital-escalations, feat:pve.wormhole-drifter-response-escalation, feat:pve.wormhole-site-downtime-despawn, feat:pve.sleeper-sentry-towers, feat:pve.wormhole-site-capital-escalation-cap
#[tokio::test]
async fn test_wormhole_capital_escalations_sentries_drifter_relay_and_downtime_despawn() {
    eve_testkit::attest(&[
        "feat:pve.wormhole-capital-escalations",
        "feat:pve.wormhole-drifter-response-escalation",
        "feat:pve.wormhole-site-downtime-despawn",
        "feat:pve.sleeper-sentry-towers",
        "feat:pve.wormhole-site-capital-escalation-cap",
    ]);
    let ctx = ctx_with_real_sde();
    let kw = HashMap::new();
    let char_id = 90_000_001_i64;

    // -------------------------------------------------------------------------
    // 1. Sleeper Sentry Towers & Stasis Webifiers via live seeded anomalies & cluster ticks
    // -------------------------------------------------------------------------
    assert_eq!(sleeper_sentry_type_for_class(1), 30_460);
    assert_eq!(sleeper_sentry_type_for_class(3), 30_461);
    assert_eq!(sleeper_sentry_type_for_class(6), 30_462);

    let sentry_sys = 31_002_000_u32; // C5
    let target_ship_id = 640_000_001_i64;
    let mut sess = setup_pilot_session_in_space(
        &ctx,
        885_101,
        char_id,
        1001,
        sentry_sys,
        target_ship_id,
        644,
        DVec3::ZERO,
        103_600_000.0,
        None,
    )
    .await;
    let sentry_anom_id = format!("PVE-ANOM-{sentry_sys}-1");
    warp_session_to_anomaly(&ctx, &mut sess, sentry_sys, &sentry_anom_id).await;

    let sentry_sys_handle = ctx.systems.get_or_spawn(sentry_sys);
    let (sentry_bid, sentry_pos, sentry_sig) = {
        let bp = sentry_sys_handle.ballpark.read().unwrap();
        let b = bp.balls.values().find(|b| b.type_id == 30_462).expect("seeded Restless Sleeper Sentry");
        let cs = bp.combat_states.get(&b.id).expect("sentry combat state");
        (b.id, b.position, cs.signature_radius)
    };
    assert_eq!(sentry_sig, 50.0, "Sleeper sentry tower signature radius must be 50 m");

    let (opt_range, dmg_em, dmg_thm, dmg_kin, dmg_exp) = with_combat_runtime(&ctx, |rt| {
        let e = rt.npc_entities.get(&sentry_bid).expect("sentry npc entity");
        (e.optimal_range, e.damage_vector.em, e.damage_vector.thermal, e.damage_vector.kinetic, e.damage_vector.explosive)
    });
    assert!(opt_range >= 250_000.0 && dmg_em > 0.0 && dmg_thm > 0.0 && (dmg_kin, dmg_exp) == (0.0, 0.0));

    // Verify Sleeper stasis web reduces target ship velocity on live cluster tick
    let bey_sentry = ctx.bind_object(&mut sess, "beyonce", sentry_sys as i64).unwrap();
    ctx.dispatch_rpc(&mut sess, "beyonce", Some(bey_sentry), "CmdGotoDirection", &[PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)], &kw, None).await.unwrap();
    ctx.advance_secs(2);
    tick_background_cluster(&ctx).await;
    assert!(
        sentry_sys_handle.get_ball_snapshot(target_ship_id).await.unwrap().unwrap().1.speed_fraction < 1.0,
        "Seeded Sleeper wave must apply stasis web reducing target speed_fraction < 1.0"
    );

    // Move ship to 200 km from sentry (outside regular Sleeper 75 km range, inside sentry 250 km optimal)
    let total_hp = |cs: eve_destiny::CombatState| cs.shield_hp + cs.armor_hp + cs.structure_hp;
    let _ = sentry_sys_handle.set_ball_position(target_ship_id, sentry_pos + DVec3::new(200_000.0, 0.0, 0.0)).await;
    // Measure HP before advancing 15s (advance_secs steps cluster/system tick at t+15s)
    let pre_sentry_shot = total_hp(sentry_sys_handle.get_combat_state(target_ship_id).await.unwrap().unwrap());
    ctx.advance_secs(15);
    tick_background_cluster(&ctx).await;
    let after_first_shot = total_hp(sentry_sys_handle.get_combat_state(target_ship_id).await.unwrap().unwrap());
    assert!(after_first_shot < pre_sentry_shot, "Sleeper sentry fires at 200 km (<= 250 km)");

    // At +5s (< 15s cadence), sentry must NOT fire again
    ctx.advance_secs(5);
    tick_background_cluster(&ctx).await;
    let at_5s = total_hp(sentry_sys_handle.get_combat_state(target_ship_id).await.unwrap().unwrap());
    assert!((at_5s - after_first_shot).abs() < 1.0, "Sleeper sentry must respect 15s firing interval");

    // At +15s (>= 15s cadence), sentry fires again
    ctx.advance_secs(10);
    tick_background_cluster(&ctx).await;
    let at_15s = total_hp(sentry_sys_handle.get_combat_state(target_ship_id).await.unwrap().unwrap());
    assert!(at_15s < after_first_shot - 10.0, "Sleeper sentry fires again after 15s");

    // Beyond 250 km (260 km), sentry does NOT fire
    let _ = sentry_sys_handle.set_ball_position(target_ship_id, sentry_pos + DVec3::new(260_000.0, 0.0, 0.0)).await;
    ctx.advance_secs(15);
    tick_background_cluster(&ctx).await;
    let at_260km = total_hp(sentry_sys_handle.get_combat_state(target_ship_id).await.unwrap().unwrap());
    assert!((at_260km - at_15s).abs() < 1.0, "Sleeper sentry must not fire beyond 250 km");
    let _ = sentry_sys_handle.remove_ball(target_ship_id).await;

    // -------------------------------------------------------------------------
    // 2. C5/C6 Capital Escalations & Per-Class Escalation Cap via live CmdWarpToStuff & downtime ticks
    // -------------------------------------------------------------------------
    let c5_sys = 31_002_001_u32;
    let c5_anom_id = format!("PVE-ANOM-{c5_sys}-1");
    let c5_handle = ctx.systems.get_or_spawn(c5_sys);
    let count_avengers = |h: &eve_server::system_actor::SystemHandle| -> usize {
        h.ballpark.read().unwrap().balls.values().filter(|b| b.type_id == 37_472).count()
    };

    let cap_specs = [
        (885_201_u64, 90_000_011_i64, 2011_u64, 640_000_011_i64, 19_720_u32, 3_usize),  // 1st Dread -> 3
        (885_202, 90_000_012, 2012, 640_000_012, 19_726, 6),                             // 2nd Dread -> 6
        (885_203, 90_000_013, 2013, 640_000_013, 19_724, 6),                             // 3rd Dread -> capped at 6
        (885_204, 90_000_014, 2014, 640_000_014, 23_757, 9),                             // 1st Carrier -> 9
        (885_205, 90_000_015, 2015, 640_000_015, 37_604, 12),                            // 2nd FAX -> 12
        (885_206, 90_000_016, 2016, 640_000_016, 23_911, 12),                            // 3rd Carrier -> capped at 12
    ];
    let mut dread1_sess_opt = None;
    for (idx, (sid_u, cid, uid, shid, stype, expected_av)) in cap_specs.into_iter().enumerate() {
        let mut csess = setup_pilot_session_in_space(
            &ctx,
            sid_u,
            cid,
            uid,
            c5_sys,
            shid,
            stype,
            DVec3::new((idx as f64) * 5_000.0, 0.0, 0.0),
            1_200_000_000.0,
            None,
        )
        .await;
        warp_session_to_anomaly(&ctx, &mut csess, c5_sys, &c5_anom_id).await;
        assert_eq!(count_avengers(&c5_handle), expected_av, "Unexpected Upgraded Avenger count at step {idx}");
        if idx == 0 {
            dread1_sess_opt = Some(csess);
        }
    }

    let avenger_bid = c5_handle.ballpark.read().unwrap().balls.values().find(|b| b.type_id == 37_472).unwrap().id;
    assert_eq!(c5_handle.get_combat_state(avenger_bid).await.unwrap().unwrap().signature_radius, 30_000.0);
    let (av_web, av_scram, av_neut) = with_combat_runtime(&ctx, |rt| {
        let e = rt.npc_entities.get(&avenger_bid).unwrap();
        (e.web_speed_factor, e.scram_strength, e.neut_amount)
    });
    assert!(av_web < 0.0 && av_scram > 0.0 && av_neut > 0.0);

    // Warp 1st Dreadnought off-grid to a system celestial via live CmdWarpToStuff -> its 3 Avengers despawn (9 left)
    let mut dread1_sess = dread1_sess_opt.unwrap();
    let c5_cel_id = ctx.sde.celestials_in_system(c5_sys).first().unwrap().celestial_id as i64;
    let bey_c5 = ctx.bind_object(&mut dread1_sess, "beyonce", c5_sys as i64).unwrap();
    ctx.dispatch_rpc(&mut dread1_sess, "beyonce", Some(bey_c5), "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(c5_cel_id), PyRep::Real(0.0)], &kw, None).await.unwrap();
    for _ in 0..75 {
        let _ = c5_handle.step_manual(1.0).await;
    }
    assert_eq!(count_avengers(&c5_handle), 9, "Escalation Avengers must despawn when triggering capital warps off-grid");

    // Advance 24h across daily downtime via live tick_background_cluster -> counters reset & immediately regenerate for un-escalated on-grid capitals!
    ctx.advance_secs(86_400);
    tick_background_cluster(&ctx).await;
    let (dread_cnt_after_dt, car_cnt_after_dt) = with_pve_exploration_reg(&ctx, |reg| {
        let s = reg.sites_by_system.get(&c5_sys).unwrap().iter().find(|s| s.site_id == c5_anom_id).unwrap();
        (s.dread_escalation_triggers, s.carrier_escalation_triggers)
    });
    assert_eq!((dread_cnt_after_dt, car_cnt_after_dt), (1, 1), "Daily downtime resets (2, 2) -> (0, 0) and steps un-escalated capitals to (1, 1)");
    assert_eq!(count_avengers(&c5_handle), 15, "Escalation waves regenerate after downtime for on-grid capitals");

    // -------------------------------------------------------------------------
    // 3. Drifter Response Escalation via live weapon fire, hacking, and real MovementMode::Warp roam
    // -------------------------------------------------------------------------
    // Case A: Dreadnought on grid in C6 (31002450) -> 4 Upgraded Avengers -> clear site -> shoot relay -> Arithmos Tyrannos (37473)
    let c6_sys = 31_002_450_u32;
    let c6_anom_id = format!("PVE-ANOM-{c6_sys}-1");
    let (c6_dread_cid, c6_dread_sid, c6_guns) = (90_000_021_i64, 640_000_021_i64, 640_100_000_i64);
    let mut c6_sess = setup_pilot_session_in_space(
        &ctx,
        885_301,
        c6_dread_cid,
        2021,
        c6_sys,
        c6_dread_sid,
        19_722, // Naglfar Dreadnought
        DVec3::ZERO,
        1_200_000_000.0,
        Some(c6_guns),
    )
    .await;
    warp_session_to_anomaly(&ctx, &mut c6_sess, c6_sys, &c6_anom_id).await;
    let c6_handle = ctx.systems.get_or_spawn(c6_sys);
    assert_eq!(count_avengers(&c6_handle), 4, "Capital warp-in on C6 site spawns 4 Upgraded Avengers");

    // Clear all waves of the seeded C6 combat site + escalation Avengers using live weapon cycles
    clear_site_npcs_via_live_weapons(&ctx, &mut c6_sess, c6_sys, &c6_anom_id, c6_guns).await;
    let c6_avenger_ids: Vec<i64> = c6_handle.ballpark.read().unwrap().balls.values().filter(|b| b.type_id == 37_472).map(|b| b.id).collect();
    for av_id in c6_avenger_ids {
        lock_and_fire_guns_until(&ctx, &mut c6_sess, c6_sys, c6_guns, av_id, false).await;
    }
    let _ = c6_handle.step_manual(1.0).await;

    let (relay_bid_a, cap_on_grid_a) = with_pve_exploration_reg(&ctx, |reg| {
        let r = reg.active_relays.values().find(|r| r.solar_system_id == c6_sys).expect("Relay spawned in C6");
        (r.relay_ball_id, r.capital_was_on_grid)
    });
    assert!(cap_on_grid_a, "Relay must record capital on grid when C6 site finished");

    // Attack the relay via live dogmaIM.AddTarget + dogmaIM.Activate -> spawns Arithmos Tyrannos (37473)
    lock_and_fire_guns_until(&ctx, &mut c6_sess, c6_sys, c6_guns, relay_bid_a, true).await;
    let (arithmos_bid, arithmos_tid) = with_pve_exploration_reg(&ctx, |reg| {
        let r = reg.active_relays.get(&relay_bid_a).unwrap();
        (r.spawned_drifter_ball_id.unwrap(), r.spawned_drifter_type_id.unwrap())
    });
    assert_eq!(arithmos_tid, 37_473, "Attacking relay with capital on grid spawns Arithmos Tyrannos (37473)");
    assert_eq!(c6_handle.get_combat_state(arithmos_bid).await.unwrap().unwrap().warp_core_strength, 3);

    // Advance > 240s and step -> Arithmos initiates real MovementMode::Warp (not instant teleport) and warps across system!
    let pos_before_roam = c6_handle.get_ball_snapshot(arithmos_bid).await.unwrap().unwrap().1.position;
    ctx.advance_secs(245);
    let _ = c6_handle.step_manual(0.1).await;
    let mid_warp_ball = c6_handle.get_ball_snapshot(arithmos_bid).await.unwrap().unwrap().1;
    assert!(
        matches!(mid_warp_ball.mode, MovementMode::Warp { .. }),
        "Roaming Arithmos Tyrannos must enter real MovementMode::Warp, got {:?}",
        mid_warp_ball.mode
    );
    for _ in 0..60 {
        let _ = c6_handle.step_manual(1.0).await;
    }
    let pos_after_roam = c6_handle.get_ball_snapshot(arithmos_bid).await.unwrap().unwrap().1.position;
    assert!(pos_after_roam.distance(pos_before_roam) > 1_000_000.0, "Arithmos Tyrannos completes warp across system");

    // Warp Dreadnought to Arithmos's celestial grid via CmdWarpToStuff, destroy Arithmos via live weapons, and verify Blue Loot wreck!
    let bey_c6 = ctx.bind_object(&mut c6_sess, "beyonce", c6_sys as i64).unwrap();
    let near_cel_id = ctx
        .sde
        .celestials_in_system(c6_sys)
        .into_iter()
        .min_by(|a, b| a.position.distance(pos_after_roam).partial_cmp(&b.position.distance(pos_after_roam)).unwrap())
        .unwrap()
        .celestial_id as i64;
    ctx.dispatch_rpc(&mut c6_sess, "beyonce", Some(bey_c6), "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(near_cel_id), PyRep::Real(30_000.0)], &kw, None).await.unwrap();
    step_until_warp_done(&c6_handle, c6_dread_sid).await;
    lock_and_fire_guns_until(&ctx, &mut c6_sess, c6_sys, c6_guns, arithmos_bid, false).await;
    let dropped_blue_loot = ctx.db.with_snapshot(|snap| {
        snap.items.values().any(|it| it.owner_id == c6_dread_cid && matches!(it.type_id, 30_744..=30_747))
    });
    assert!(dropped_blue_loot, "Destroying Arithmos Tyrannos in space must drop Sleeper Blue Loot wreck");

    // Case B: Subcapital only on grid when C5 Sleeper Relic site (REL-31002005) finishes via live weapons + live hacking -> Hyleus Tyrannos (86498)
    let c5_relic_sys = 31_002_005_u32;
    let relic_sig_id = format!("REL-{c5_relic_sys}");
    let (sub_cid, sub_sid, sub_guns, analyzer_id) = (90_000_031_i64, 640_000_031_i64, 640_200_000_i64, 640_200_050_i64);
    let mut sub_sess = setup_pilot_session_in_space(
        &ctx,
        885_302,
        sub_cid,
        2031,
        c5_relic_sys,
        sub_sid,
        644, // Tempest subcapital Battleship
        DVec3::ZERO,
        105_000_000.0,
        Some(sub_guns),
    )
    .await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(sub_cid).or_default().insert(13_278, TrainedSkillRecord { type_id: 13_278, level: 5, skill_points: 256_000.0 });
        snap.items.insert(
            analyzer_id,
            InventoryItemRecord {
                item_id: analyzer_id,
                type_id: 30_832, // Relic Analyzer II
                owner_id: sub_cid,
                location_id: sub_sid,
                flag: InventoryFlag::MedSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(analyzer_id, true);
    });
    ctx.invalidate_ship_fitting(sub_sid);
    let (relic_did, relic_cans) = with_pve_exploration_reg(&ctx, |reg| {
        let s = reg.sites_by_system.get(&c5_relic_sys).unwrap().iter().find(|s| s.site_id == relic_sig_id).unwrap();
        (s.dungeon_id, s.hackable_can_ids.clone())
    });
    let c5_rel_handle = ctx.systems.get_or_spawn(c5_relic_sys);
    ctx.dispatch_rpc(&mut sub_sess, "custom_agency_provider", None, "warp_to_content", &[PyRep::keyval([("dungeonID", PyRep::Integer(relic_did as i64))])], &kw, None).await.unwrap();
    step_until_warp_done(&c5_rel_handle, sub_sid).await;

    clear_site_npcs_via_live_weapons(&ctx, &mut sub_sess, c5_relic_sys, &relic_sig_id, sub_guns).await;
    for can_id in relic_cans {
        hack_container_to_core_live(&ctx, &mut sub_sess, sub_cid, can_id, 21_741, analyzer_id).await;
    }
    let _ = c5_rel_handle.step_manual(1.0).await;

    let (relay_bid_b, cap_on_grid_b) = with_pve_exploration_reg(&ctx, |reg| {
        let r = reg.active_relays.values().find(|r| r.solar_system_id == c5_relic_sys).expect("Relay spawned on C5 Relic clear");
        (r.relay_ball_id, r.capital_was_on_grid)
    });
    assert!(!cap_on_grid_b, "No capital on grid when C5 Relic site finished");

    lock_and_fire_guns_until(&ctx, &mut sub_sess, c5_relic_sys, sub_guns, relay_bid_b, true).await;
    let (hyleus_bid, hyleus_tid) = with_pve_exploration_reg(&ctx, |reg| {
        let r = reg.active_relays.get(&relay_bid_b).unwrap();
        (r.spawned_drifter_ball_id.unwrap(), r.spawned_drifter_type_id.unwrap())
    });
    assert_eq!(hyleus_tid, 86_498, "Attacking relay without capital on grid spawns Hyleus Tyrannos (86498)");
    assert_eq!(c5_rel_handle.get_combat_state(hyleus_bid).await.unwrap().unwrap().warp_core_strength, 3);

    // -------------------------------------------------------------------------
    // 4. W-Space Site Downtime Despawn via live warp activation, live site clear, & 24h downtime ticks
    // -------------------------------------------------------------------------
    let dt_sys = 31_001_200_u32; // C3
    let (dt_cid, dt_sid, dt_guns) = (90_000_041_i64, 640_000_041_i64, 640_300_000_i64);
    let mut dt_sess = setup_pilot_session_in_space(
        &ctx,
        885_401,
        dt_cid,
        2041,
        dt_sys,
        dt_sid,
        644,
        DVec3::ZERO,
        105_000_000.0,
        Some(dt_guns),
    )
    .await;
    let done_site_id = format!("PVE-ANOM-{dt_sys}-1");
    let open_site_id = format!("PVE-ANOM-{dt_sys}-2");

    // Warp into both sites live; clear done_site_id via live weapon cycles while leaving open_site_id unfinished
    warp_session_to_anomaly(&ctx, &mut dt_sess, dt_sys, &open_site_id).await;
    warp_session_to_anomaly(&ctx, &mut dt_sess, dt_sys, &done_site_id).await;
    clear_site_npcs_via_live_weapons(&ctx, &mut dt_sess, dt_sys, &done_site_id, dt_guns).await;

    let site_exists = |id: &str| -> bool {
        with_pve_exploration_reg(&ctx, |reg| {
            reg.sites_by_system.get(&dt_sys).is_some_and(|l| l.iter().any(|s| s.site_id == id))
        })
    };
    assert!(site_exists(&done_site_id) && site_exists(&open_site_id));

    // Downtimes 1 & 2: both sites persist
    for dt_idx in 1..=2 {
        ctx.advance_secs(86_400);
        tick_background_cluster(&ctx).await;
        assert!(site_exists(&done_site_id) && site_exists(&open_site_id), "Both sites persist after downtime {dt_idx}");
    }

    // Downtime 3: completed site despawns, unfinished site persists
    ctx.advance_secs(86_400);
    tick_background_cluster(&ctx).await;
    assert!(!site_exists(&done_site_id), "Completed W-space site must despawn at 3rd downtime");
    assert!(site_exists(&open_site_id), "Unfinished W-space site must persist at 3rd downtime");

    // Downtime 4: unfinished site despawns
    ctx.advance_secs(86_400);
    tick_background_cluster(&ctx).await;
    assert!(!site_exists(&open_site_id), "Unfinished W-space site must despawn at 4th downtime");
}
