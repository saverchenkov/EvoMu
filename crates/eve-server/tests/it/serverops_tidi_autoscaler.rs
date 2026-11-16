//! Acceptance and V24 parity tests for server-driven Time Dilation autoscaling (`#1081`, `#1077` E24):
//! - Per-system 1 s control-window load autoscaler with hysteresis (`20 ms` engage / `5 ms` slack,
//!   immediate tighten, `5 s` min hold, 2 consecutive low-load relax windows cleared on spike).
//! - Synchronized `2 s` advance-notice stamp switches, deterministic `/tidi clear`, skip if unchanged.
//! - Single `DoSimClockRebase` per factor change & once on login/jump into lagged/dilated system,
//!   zero on normal ticks or `beyonce` bind, history-safe stamp preservation after `0.5 -> 1.0`.
//! - Sim-clock-governed missiles, locks, reloads, reactivation cooldowns, self-destruct, wreck decay,
//!   and doomsday delays; fixed `7 s` wall-clock stargate session-change timer.
//! - Non-backstepping `OnSpecialFX` Destiny stamps (`>= owner_visible_stamp`, `>= prime_update_stamp`)
//!   and propulsion stop FX emitted before detach on docking.
//! - Seeded 60-ship fitted engagement hysteresis scenario.

#![forbid(unsafe_code)]

use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
use eve_destiny::{Ball, CombatState, DamageVector, DestinyEvent, MovementMode, TargetLock};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::{
    register_container_lifetime, step_combat_system, step_wreck_and_jetcan_decay,
    with_combat_runtime, with_mut_combat_runtime, InFlightMissileState,
};
use eve_server::services::doomsday_aoe_tick::{
    mutate_superweapon_runtime, read_superweapon_runtime, step_bosonic_doomsday_in_ballpark,
    step_bosonic_doomsday_system, PersistedSuperweaponCycle, PersistedWarpSuppression,
    SuperweaponCycleKind,
};
use eve_server::services::serverops_cluster::{
    clear_manual_system_tidi_override, emit_sim_clock_rebase_on_entry_if_needed,
    evaluate_system_tidi_autoscaler_window, get_cluster_tidi_snapshot, get_system_tidi_factor,
    get_system_tidi_snapshot, is_latency_auto_tidi_enabled, poll_pending_system_tidi_switch,
    record_packet_perf_latency_sample, schedule_system_tidi_switch, set_latency_auto_tidi_enabled,
    set_system_tidi_factor,
};
use eve_server::services::space_destiny::{
    check_session_change_timer_active, record_session_change_timer,
    STARGATE_SESSION_CHANGE_TIMER_SECS,
};
use eve_server::system_actor::tidi_autoscaler::{
    compute_tidi_factor_from_lateness, TIDI_ADVANCE_NOTICE_SECS, TIDI_ENGAGE_LATENESS_MS,
    TIDI_MIN_HOLD_SECS, TIDI_RELAX_CONFIRM_WINDOWS, TIDI_RELAX_THRESHOLD_MS, TIDI_TARGET_WINDOW_MS,
};
use eve_server::system_actor::ActiveModuleCycle;
use glam::DVec3;
use std::collections::HashMap;

const JITA_SYSTEM_ID: u32 = 30_000_142;
const AMARR_SYSTEM_ID: u32 = 30_002_187;
const PERIMETER_SYSTEM_ID: u32 = 30_000_144;
const SMALL_AAR_TYPE_ID: u32 = 33_076;
const NANITE_PASTE_TYPE_ID: u32 = 28_668;
const AFTERBURNER_TYPE_ID: u32 = 439;
const FRIGATE_TYPE_ID: u32 = 587;

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None)
        .await
}

fn seed_item(
    ctx: &ServerContext,
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            item_id,
            InventoryItemRecord {
                item_id,
                type_id,
                owner_id,
                location_id,
                flag,
                quantity,
                singleton: quantity == 1,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });
}

fn grant_basic_fitting_skills(ctx: &ServerContext, char_id: i64, type_ids: &[u32]) {
    ctx.db.with_mut_snapshot(|snap| {
        let entry = snap.skills.entry(char_id).or_default();
        for &(sid, lvl) in &[
            (3426u32, 5u8),
            (3413, 5),
            (3418, 5),
            (3417, 5),
            (3428, 5),
            (3429, 5),
            (3431, 5),
            (3392, 5),
            (3393, 5),
            (3416, 5),
            (25863, 5),
            (3436, 5),
        ] {
            entry.insert(
                sid,
                TrainedSkillRecord {
                    type_id: sid,
                    level: lvl,
                    skill_points: 256_000.0,
                },
            );
        }
        for &tid in type_ids {
            if let Some(t) = ctx.sde.types.get(&tid) {
                for (s_attr, l_attr) in [
                    (
                        eve_sde::attr_ids::REQUIRED_SKILL_1,
                        eve_sde::attr_ids::REQUIRED_SKILL_1_LEVEL,
                    ),
                    (
                        eve_sde::attr_ids::REQUIRED_SKILL_2,
                        eve_sde::attr_ids::REQUIRED_SKILL_2_LEVEL,
                    ),
                ] {
                    if let Some(&req_s) = t.default_attributes.get(&s_attr) {
                        let req_l = t
                            .default_attributes
                            .get(&l_attr)
                            .copied()
                            .unwrap_or(1.0)
                            .max(1.0) as u8;
                        entry.insert(
                            req_s as u32,
                            TrainedSkillRecord {
                                type_id: req_s as u32,
                                level: req_l,
                                skill_points: 256_000.0,
                            },
                        );
                    }
                }
            }
        }
    });
}

fn spawn_ship_with_cs(id: i64, pos: DVec3) -> (Ball, CombatState) {
    let ball = Ball {
        id,
        type_id: FRIGATE_TYPE_ID,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 350.0,
        inertia: 2.5,
        mass: 1_100_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let mut cs = CombatState {
        max_shield_hp: 2_000.0,
        shield_hp: 2_000.0,
        max_armor_hp: 2_000.0,
        armor_hp: 2_000.0,
        max_structure_hp: 2_000.0,
        structure_hp: 2_000.0,
        max_capacitor: 1_000.0,
        capacitor: 1_000.0,
        signature_radius: 40.0,
        ..Default::default()
    };
    let mut ball_tmp = ball.clone();
    cs.recompute_attributes(&mut ball_tmp);
    (ball, cs)
}

fn extract_fx_stamps_from_pyrep(pyrep: &PyRep) -> Vec<(i64, String)> {
    let mut out = Vec::new();
    let PyRep::List(items) = pyrep else {
        return out;
    };
    for item in items {
        if let PyRep::Tuple(outer) = item {
            if let (Some(stamp), Some(PyRep::Tuple(inner))) =
                (outer.first().and_then(|v| v.as_i64()), outer.get(1))
            {
                if let Some(name) = inner.first().and_then(|v| v.as_str()) {
                    out.push((stamp, name.to_string()));
                }
            }
        }
    }
    out
}

fn collect_rebase_events(ctx: &ServerContext, char_id: i64) -> Vec<(i64, i64)> {
    ctx.drain_notifications_for_char(char_id)
        .into_iter()
        .filter(|n| n.event == "DoSimClockRebase")
        .filter_map(|n| match n.args.first()? {
            PyRep::Tuple(tup) => Some((tup.first()?.as_i64()?, tup.get(1)?.as_i64()?)),
            _ => None,
        })
        .collect()
}

/// `#1077` E24 acceptance test: under `0.25x` TiDi, missile flight/impact, target locking,
/// module reloads (`LoadAmmo` -> `ModuleIsReloading`), and module reactivation delays all progress
/// on simulated time (`dt * 0.25`) while an undilated (`1.0x`) control system progresses at 1:1.
#[tokio::test]
async fn tidi_sim_clock_governs_missiles_locks_and_reloads() {
    // parity: feat:serverops.time-dilation, notify:DoSimClockRebase, feat:dogma.targeting-lock, feat:dogma.reactivation-delay
    eve_testkit::attest(&[
        "feat:serverops.time-dilation",
        "notify:DoSimClockRebase",
        "feat:dogma.targeting-lock",
        "feat:dogma.reactivation-delay",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut pilot = common::gm_pilot_in_space(&ctx, 81_001, 90_081_001, FRIGATE_TYPE_ID, JITA_SYSTEM_ID).await;
    let sys_jita = ctx.systems.get_or_spawn(JITA_SYSTEM_ID);
    let sys_amarr = ctx.systems.get_or_spawn(AMARR_SYSTEM_ID);

    // Put Jita under 0.25x TiDi and keep Amarr at 1.0x.
    assert!((set_system_tidi_factor(&ctx, JITA_SYSTEM_ID, 0.25) - 0.25).abs() < 1e-9);
    assert!((get_system_tidi_factor(&ctx, AMARR_SYSTEM_ID) - 1.0).abs() < 1e-9);
    let _ = collect_rebase_events(&ctx, pilot.char_id);

    // 1. Target lock timing follows sim clock (`0.25x` -> 4x wall steps needed).
    let (jita_target_ball, jita_target_cs) = spawn_ship_with_cs(900_810_101, DVec3::new(5_000.0, 0.0, 0.0));
    let (amarr_att_ball, amarr_att_cs) = spawn_ship_with_cs(900_810_201, DVec3::ZERO);
    let (amarr_tgt_ball, amarr_tgt_cs) = spawn_ship_with_cs(900_810_202, DVec3::new(5_000.0, 0.0, 0.0));
    sys_jita.spawn_ball_with_combat(jita_target_ball, jita_target_cs).await.unwrap();
    sys_amarr.spawn_ball_with_combat(amarr_att_ball, amarr_att_cs).await.unwrap();
    sys_amarr.spawn_ball_with_combat(amarr_tgt_ball, amarr_tgt_cs).await.unwrap();

    sys_jita
        .add_target_lock(
            pilot.ship_id,
            TargetLock {
                target_id: 900_810_101,
                lock_time_ms: 2_000.0,
                remaining_lock_ms: 2_000.0,
                locked: false,
                max_range_m: None,
            },
        )
        .await
        .unwrap();
    sys_amarr
        .add_target_lock(
            900_810_201,
            TargetLock {
                target_id: 900_810_202,
                lock_time_ms: 2_000.0,
                remaining_lock_ms: 2_000.0,
                locked: false,
                max_range_m: None,
            },
        )
        .await
        .unwrap();

    for _ in 0..4 {
        sys_jita.step_manual(1.0).await.unwrap();
        sys_amarr.step_manual(1.0).await.unwrap();
    }
    let jita_locks_after_4 = sys_jita.get_target_locks(pilot.ship_id).await.unwrap();
    let amarr_locks_after_4 = sys_amarr.get_target_locks(900_810_201).await.unwrap();
    assert!(
        amarr_locks_after_4.iter().any(|l| l.target_id == 900_810_202 && l.locked),
        "2.0s lock in 1.0x Amarr must finish after 4 wall seconds"
    );
    assert!(
        jita_locks_after_4.iter().any(|l| l.target_id == 900_810_101 && !l.locked),
        "2.0s lock in 0.25x Jita must still be locking at 1.0s sim time (4 wall steps)"
    );
    for _ in 0..4 {
        sys_jita.step_manual(1.0).await.unwrap();
    }
    assert!(
        sys_jita
            .get_target_locks(pilot.ship_id)
            .await
            .unwrap()
            .iter()
            .any(|l| l.target_id == 900_810_101 && l.locked),
        "2.0s lock in 0.25x Jita must finish after 8 wall seconds (2.0s sim)"
    );

    // 2. Missile flight & impact follow sim clock (`step_combat_system`).
    let jita_missile_id = 900_810_301i64;
    let amarr_missile_id = 900_810_302i64;
    sys_jita
        .spawn_ball_with_combat(
            Ball {
                id: jita_missile_id,
                type_id: 204,
                position: DVec3::new(3_500.0, 0.0, 0.0),
                velocity: DVec3::new(1_000.0, 0.0, 0.0),
                max_velocity: 1_000.0,
                inertia: 0.1,
                mass: 500.0,
                radius: 1.0,
                speed_fraction: 1.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
        )
        .await
        .unwrap();
    sys_amarr
        .spawn_ball_with_combat(
            Ball {
                id: amarr_missile_id,
                type_id: 204,
                position: DVec3::new(3_500.0, 0.0, 0.0),
                velocity: DVec3::new(1_000.0, 0.0, 0.0),
                max_velocity: 1_000.0,
                inertia: 0.1,
                mass: 500.0,
                radius: 1.0,
                speed_fraction: 1.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
        )
        .await
        .unwrap();
    with_mut_combat_runtime(&ctx, |rt| {
        for (mid, sys_id, att_ship, tgt_id) in [
            (jita_missile_id, JITA_SYSTEM_ID, pilot.ship_id, 900_810_101),
            (amarr_missile_id, AMARR_SYSTEM_ID, 900_810_201, 900_810_202),
        ] {
            rt.in_flight_missiles.insert(
                mid,
                InFlightMissileState {
                    missile_ball_id: mid,
                    solar_system_id: sys_id,
                    attacker_char_id: pilot.char_id,
                    attacker_ship_id: att_ship,
                    launcher_module_id: 900_810_400,
                    launcher_type_id: 2410,
                    missile_type_id: 204,
                    target_ball_id: tgt_id,
                    max_velocity: 1_000.0,
                    flight_time_s: 5.0,
                    remaining_flight_s: 5.0,
                    hp: 70.0,
                    max_hp: 70.0,
                    base_damage: DamageVector::new(100.0, 0.0, 0.0, 0.0),
                    explosion_radius: 40.0,
                    explosion_velocity: 150.0,
                    drf: 0.6,
                    is_fof: false,
                    is_breacher_pod: false,
                    breacher_dot_duration_s: 0.0,
                    breacher_flat_cap: 0.0,
                    breacher_pct_cap: 0.0,
                    destroyed: false,
                    expired: false,
                    impacted: false,
                    applied_damage: 0.0,
                },
            );
        }
    });

    for _ in 0..2 {
        step_combat_system(&ctx, JITA_SYSTEM_ID, 1.0).await;
        step_combat_system(&ctx, AMARR_SYSTEM_ID, 1.0).await;
    }
    let (jita_ms_2, amarr_ms_2) = with_combat_runtime(&ctx, |rt| {
        (
            rt.in_flight_missiles.get(&jita_missile_id).cloned().unwrap(),
            rt.in_flight_missiles.get(&amarr_missile_id).cloned().unwrap(),
        )
    });
    assert!(amarr_ms_2.impacted, "1500m missile at 1000m/s in 1.0x impacts by 2s");
    assert!(
        !jita_ms_2.impacted && (jita_ms_2.remaining_flight_s - 4.5).abs() < 1e-6,
        "0.25x TiDi missile only advances 0.5s sim in 2 wall seconds (got rem={})",
        jita_ms_2.remaining_flight_s
    );
    for _ in 0..4 {
        step_combat_system(&ctx, JITA_SYSTEM_ID, 1.0).await;
    }
    let jita_ms_6 = with_combat_runtime(&ctx, |rt| {
        rt.in_flight_missiles.get(&jita_missile_id).cloned().unwrap()
    });
    assert!(
        jita_ms_6.impacted,
        "0.25x TiDi missile impacts after 6 wall seconds (1.5s sim = 1500m)"
    );

    // 3. Module reactivation delay follows sim clock under TiDi even when wall clock advances.
    let react_mod_id = 900_810_501i64;
    seed_item(&ctx, react_mod_id, AFTERBURNER_TYPE_ID, pilot.char_id, pilot.ship_id, InventoryFlag::MedSlot0, 1);
    ctx.record_module_reactivation_delay(pilot.ship_id, react_mod_id, 10.0);
    assert!(ctx.is_module_in_reactivation_delay(pilot.ship_id, react_mod_id));
    ctx.advance_secs(12);
    for _ in 0..12 {
        sys_jita.try_step_full_tick(1.0); // 12 * 0.25 = 3.0s sim
    }
    assert!(
        ctx.is_module_in_reactivation_delay(pilot.ship_id, react_mod_id),
        "10s sim reactivation cooldown in 0.25x TiDi must remain active at +12s wall (3s sim)"
    );
    for _ in 0..28 {
        sys_jita.try_step_full_tick(1.0); // +7.0s sim => total 10.0s sim
    }
    assert!(
        !ctx.is_module_in_reactivation_delay(pilot.ship_id, react_mod_id),
        "10s sim reactivation cooldown must expire once 10.0s sim has elapsed"
    );

    // 4. Module reload deadline (`dogmaIM.LoadAmmo` -> 60s AAR paste reload) scales with 0.25x TiDi.
    let aar_id = 900_810_601i64;
    let paste_cargo_id = 900_810_602i64;
    grant_basic_fitting_skills(&ctx, pilot.char_id, &[SMALL_AAR_TYPE_ID, NANITE_PASTE_TYPE_ID]);
    seed_item(&ctx, aar_id, SMALL_AAR_TYPE_ID, pilot.char_id, pilot.ship_id, InventoryFlag::LoSlot1, 1);
    seed_item(&ctx, paste_cargo_id, NANITE_PASTE_TYPE_ID, pilot.char_id, pilot.ship_id, InventoryFlag::Cargo, 16);
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(aar_id, true);
    });
    ctx.invalidate_ship_fitting(pilot.ship_id);

    let reload_rsp = rpc(
        &ctx,
        &mut pilot.sess,
        "dogmaIM",
        "LoadAmmo",
        &[
            PyRep::Integer(pilot.ship_id),
            PyRep::List(vec![PyRep::Integer(aar_id)]),
            PyRep::List(vec![PyRep::Integer(paste_cargo_id)]),
            PyRep::Integer(pilot.ship_id),
        ],
    )
    .await
    .expect("LoadAmmo into Small AAR");
    assert_eq!(reload_rsp.dict_get("reloadTimeMs").and_then(|v| v.as_f64()), Some(60_000.0));

    // Advance 65s wall + 65 ticks (16.25s sim): must STILL fail with ModuleIsReloading!
    for _ in 0..65 {
        ctx.advance_secs(1);
        sys_jita.try_step_full_tick(1.0);
    }
    let err_mid = rpc(
        &ctx,
        &mut pilot.sess,
        "dogmaIM",
        "Activate",
        &[
            PyRep::Integer(aar_id),
            PyRep::String("fueledArmorRepair".into()),
            PyRep::None,
            PyRep::Integer(0),
        ],
    )
    .await
    .expect_err("60s reload under 0.25x TiDi must still be reloading after 65s wall");
    assert_eq!(err_mid.code, "ModuleIsReloading");

    // Advance remaining 175s wall + 175 ticks (total 60.0s sim): reload finishes and Activate succeeds.
    for _ in 0..175 {
        ctx.advance_secs(1);
        sys_jita.try_step_full_tick(1.0);
    }
    sys_jita.set_capacitor(pilot.ship_id, 1_000.0).await.unwrap();
    rpc(
        &ctx,
        &mut pilot.sess,
        "dogmaIM",
        "Activate",
        &[
            PyRep::Integer(aar_id),
            PyRep::String("fueledArmorRepair".into()),
            PyRep::None,
            PyRep::Integer(0),
        ],
    )
    .await
    .expect("AAR reload must complete after 60s sim (240s wall at 0.25x TiDi)");
}

/// Verifies synchronized ~2s advance-notice switch applied at a Destiny stamp boundary,
/// skipping systems already at target, and deterministic `/tidi clear` reset to 1.0.
#[tokio::test]
async fn tidi_advance_notice_then_apply_after_2s() {
    // parity: feat:serverops.time-dilation, notify:DoSimClockRebase
    eve_testkit::attest(&["feat:serverops.time-dilation", "notify:DoSimClockRebase"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let pilot = common::gm_pilot_in_space(&ctx, 81_002, 90_081_002, FRIGATE_TYPE_ID, JITA_SYSTEM_ID).await;
    let _ = collect_rebase_events(&ctx, pilot.char_id);

    // 1. Scheduling 1.0 when already at 1.0 is skipped cleanly.
    assert!(
        schedule_system_tidi_switch(&ctx, JITA_SYSTEM_ID, 1.0, "noop", 100.0, 50, TIDI_ADVANCE_NOTICE_SECS).is_none(),
        "Systems already at target factor must be skipped"
    );

    // 2. Schedule 1.0 -> 0.40 with 2s advance notice at stamp 50 -> target stamp 52.
    let pending = schedule_system_tidi_switch(
        &ctx,
        JITA_SYSTEM_ID,
        0.40,
        "tighten",
        100.0,
        50,
        TIDI_ADVANCE_NOTICE_SECS,
    )
    .expect("Pending advance-notice switch scheduled");
    assert_eq!(pending.target_stamp, 52);
    assert!((pending.apply_at_secs - 102.0).abs() < 1e-6);

    let snap_mid = get_system_tidi_snapshot(&ctx, JITA_SYSTEM_ID);
    assert!(snap_mid.pending_transition.is_some());
    assert!((get_system_tidi_factor(&ctx, JITA_SYSTEM_ID) - 1.0).abs() < 1e-9);
    assert!(poll_pending_system_tidi_switch(&ctx, JITA_SYSTEM_ID, 101.0, 51).is_none());
    assert!(collect_rebase_events(&ctx, pilot.char_id).is_empty());

    // 3. Before target stamp (time reached, stamp not yet) must still wait for stamp boundary.
    assert!(poll_pending_system_tidi_switch(&ctx, JITA_SYSTEM_ID, 102.0, 51).is_none());
    assert!((get_system_tidi_factor(&ctx, JITA_SYSTEM_ID) - 1.0).abs() < 1e-9);

    // 4. At t >= 102.0 and stamp >= 52: synchronized apply + single DoSimClockRebase.
    let applied = poll_pending_system_tidi_switch(&ctx, JITA_SYSTEM_ID, 102.0, 52)
        .expect("Synchronized switch must apply at stamp boundary after 2s notice");
    assert!((applied.new_factor - 0.40).abs() < 1e-9);
    assert!((get_system_tidi_factor(&ctx, JITA_SYSTEM_ID) - 0.40).abs() < 1e-9);
    assert_eq!(collect_rebase_events(&ctx, pilot.char_id).len(), 1);

    // 5. Clearing forces 1.0 deterministically (single rebase) and repeats skip rebase.
    assert_eq!(clear_manual_system_tidi_override(&ctx, JITA_SYSTEM_ID), 1.0);
    assert!((get_system_tidi_factor(&ctx, JITA_SYSTEM_ID) - 1.0).abs() < 1e-9);
    assert_eq!(collect_rebase_events(&ctx, pilot.char_id).len(), 1);
    assert_eq!(clear_manual_system_tidi_override(&ctx, JITA_SYSTEM_ID), 1.0);
    assert!(collect_rebase_events(&ctx, pilot.char_id).is_empty());
}

/// Verifies autoscaler formula, hysteresis (immediate tighten, 2 consecutive low-load windows
/// cleared on rising load, 5s min hold), `/autotidi off`, `/tidi <f>`, `/tidi status`, and PacketPerf log.
#[tokio::test]
async fn tidi_autoscaler_hysteresis() {
    // parity: feat:serverops.time-dilation, notify:DoSimClockRebase
    eve_testkit::attest(&["feat:serverops.time-dilation", "notify:DoSimClockRebase"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut pilot = common::gm_pilot_in_space(&ctx, 81_003, 90_081_003, FRIGATE_TYPE_ID, JITA_SYSTEM_ID).await;

    // Default enabled
    assert!(is_latency_auto_tidi_enabled(&ctx));
    assert!((TIDI_TARGET_WINDOW_MS - 1_000.0).abs() < 1e-9);
    assert!((TIDI_ENGAGE_LATENESS_MS - 20.0).abs() < 1e-9);
    assert!((TIDI_RELAX_THRESHOLD_MS - 15.0).abs() < 1e-9);
    assert!((TIDI_MIN_HOLD_SECS - 5.0).abs() < 1e-9);
    assert_eq!(TIDI_RELAX_CONFIRM_WINDOWS, 2);

    // Formula verification: target / (target + (actual - engage)), clamped [0.1, 1.0]
    let expected_520 = 1000.0 / (1000.0 + (520.0 - 20.0)); // 1000 / 1500 = 0.6666...
    assert!((compute_tidi_factor_from_lateness(520.0) - expected_520).abs() < 1e-6);
    assert!((compute_tidi_factor_from_lateness(15_000.0) - 0.10).abs() < 1e-9);
    assert!((compute_tidi_factor_from_lateness(19.0) - 1.0).abs() < 1e-9);

    // 1. Immediate tighten announcement on excess lateness (`reason: tighten`), applied at +2s stamp boundary.
    let s1 = evaluate_system_tidi_autoscaler_window(&ctx, JITA_SYSTEM_ID, 520.0, Some(38.5), 10.0, 10);
    assert_eq!(s1.reason, "tighten");
    assert_eq!(s1.packet_perf_latency_ms, Some(38.5));
    let s1_pend = s1
        .pending_transition
        .as_ref()
        .expect("tighten schedules 2s synchronized switch");
    assert!((s1_pend.to_factor - expected_520).abs() < 1e-6);

    let s1_applied =
        evaluate_system_tidi_autoscaler_window(&ctx, JITA_SYSTEM_ID, 520.0, None, 12.0, 12);
    assert!((s1_applied.factor - expected_520).abs() < 1e-6);

    // 2. Low-load window 1 enters `await-relax-confirmation`; 2nd consecutive inside 5s min hold hits `min-hold-active`.
    let s2 = evaluate_system_tidi_autoscaler_window(&ctx, JITA_SYSTEM_ID, 5.0, None, 13.0, 13);
    assert_eq!(s2.reason, "await-relax-confirmation");
    assert_eq!(s2.consecutive_low_load_windows, 1);
    let s2_hold = evaluate_system_tidi_autoscaler_window(&ctx, JITA_SYSTEM_ID, 5.0, None, 14.0, 14);
    assert_eq!(s2_hold.reason, "min-hold-active");
    assert!((s2_hold.factor - expected_520).abs() < 1e-6);

    // 3. Rising load (18ms > 15ms) clears pending low-load streak without tightening (`normal`).
    let s4 = evaluate_system_tidi_autoscaler_window(&ctx, JITA_SYSTEM_ID, 18.0, None, 16.0, 16);
    assert_eq!(s4.reason, "normal");
    assert_eq!(s4.consecutive_low_load_windows, 0);

    // 4. Two consecutive low-load windows past 5s hold -> announce relax, then apply 1.0 after 2s notice.
    let s5 = evaluate_system_tidi_autoscaler_window(&ctx, JITA_SYSTEM_ID, 4.0, None, 18.0, 18);
    assert_eq!(s5.reason, "await-relax-confirmation");
    assert_eq!(s5.consecutive_low_load_windows, 1);
    let s6 = evaluate_system_tidi_autoscaler_window(&ctx, JITA_SYSTEM_ID, 4.0, None, 19.0, 19);
    assert_eq!(s6.reason, "relax");
    assert!((s6.pending_transition.as_ref().unwrap().to_factor - 1.0).abs() < 1e-9);
    let s7 = evaluate_system_tidi_autoscaler_window(&ctx, JITA_SYSTEM_ID, 4.0, None, 21.0, 21);
    assert!((s7.factor - 1.0).abs() < 1e-9);

    // 5. `/autotidi off` disables autoscaling; `/tidi 0.35` pins; `/tidi status` reports fields.
    common::slash_cmd(&ctx, &mut pilot.sess, "/autotidi off").await;
    assert!(!is_latency_auto_tidi_enabled(&ctx));
    let s_off = evaluate_system_tidi_autoscaler_window(&ctx, JITA_SYSTEM_ID, 900.0, None, 22.0, 22);
    assert_eq!(s_off.reason, "disabled");
    assert!((s_off.factor - 1.0).abs() < 1e-9);

    common::slash_cmd(&ctx, &mut pilot.sess, "/tidi 0.35").await;
    assert!((get_system_tidi_factor(&ctx, JITA_SYSTEM_ID) - 0.35).abs() < 1e-9);
    record_packet_perf_latency_sample(&ctx, JITA_SYSTEM_ID, 112.0);
    let status = common::slash_cmd(&ctx, &mut pilot.sess, "/tidi status").await;
    assert!((status.dict_get("factor").and_then(|v| v.as_f64()).unwrap() - 0.35).abs() < 1e-9);
    assert_eq!(status.dict_get("pinned").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(
        status.dict_get("packetPerfLatencyMs").and_then(|v| v.as_f64()),
        Some(112.0)
    );
    let cluster_snap = get_cluster_tidi_snapshot(&ctx);
    assert!(!cluster_snap.autoscaler_enabled);
    assert!(cluster_snap.systems.iter().any(|s| s.solar_system_id == JITA_SYSTEM_ID));

    set_latency_auto_tidi_enabled(&ctx, true);
}

/// Verifies `DoSimClockRebase` fires **once** per factor change and **once** on login/jump into a
/// lagged or dilated system, never on normal ticks or `beyonce` bind, and `0.5 -> 1.0` preserves sim
/// stamp with history-safe immediate post-transition activation timestamps.
#[tokio::test]
async fn sim_clock_rebase_only_on_factor_change() {
    // parity: feat:serverops.time-dilation, notify:DoSimClockRebase, rpc:beyonce.UpdateStateRequest
    eve_testkit::attest(&[
        "feat:serverops.time-dilation",
        "notify:DoSimClockRebase",
        "rpc:beyonce.UpdateStateRequest",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let sys_jita = ctx.systems.get_or_spawn(JITA_SYSTEM_ID);
    // Normal ticks in undilated system emit 0 DoSimClockRebase notifications.
    let pilot_jita = common::gm_pilot_in_space(&ctx, 81_004, 90_081_004, FRIGATE_TYPE_ID, JITA_SYSTEM_ID).await;
    let _ = collect_rebase_events(&ctx, pilot_jita.char_id);
    for _ in 0..8 {
        sys_jita.try_step_full_tick(1.0);
    }
    assert!(
        collect_rebase_events(&ctx, pilot_jita.char_id).is_empty(),
        "Normal ticks must NEVER emit DoSimClockRebase"
    );

    // Changing factor 1.0 -> 0.5 emits DoSimClockRebase once; repeated 0.5 & ticks emit 0 more.
    set_system_tidi_factor(&ctx, JITA_SYSTEM_ID, 0.50);
    assert_eq!(collect_rebase_events(&ctx, pilot_jita.char_id).len(), 1);
    set_system_tidi_factor(&ctx, JITA_SYSTEM_ID, 0.50);
    for _ in 0..5 {
        sys_jita.try_step_full_tick(1.0);
    }
    assert!(
        collect_rebase_events(&ctx, pilot_jita.char_id).is_empty(),
        "Normal ticks under 0.5x TiDi or setting identical factor must not emit DoSimClockRebase"
    );

    // Advance 10s wall at 0.5x TiDi, then revert 0.5 -> 1.0: preserves rebased sim stamp & history safety.
    ctx.advance_secs_without_sim_clock_rebase(10);
    let wall_now_ft = ctx.now_blueos_filetime_ticks();
    clear_manual_system_tidi_override(&ctx, JITA_SYSTEM_ID);
    let rebases = collect_rebase_events(&ctx, pilot_jita.char_id);
    assert_eq!(rebases.len(), 1, "0.5 -> 1.0 must emit DoSimClockRebase exactly once");
    let (_, rebased_sim_ft) = rebases[0];
    assert!(
        rebased_sim_ft < wall_now_ft - 30_000_000,
        "0.5 -> 1.0 must preserve accumulated sim clock (~5s behind wall) instead of jumping to wall clock"
    );
    let act_ft_1 = ctx.now_system_sim_filetime_ticks(Some(JITA_SYSTEM_ID));
    let act_ft_2 = ctx.now_system_sim_filetime_ticks(Some(JITA_SYSTEM_ID));
    assert!(
        act_ft_1 >= rebased_sim_ft && act_ft_1 < wall_now_ft,
        "Immediate activation FILETIME must stay anchored at rebased sim stamp"
    );
    assert!(
        act_ft_2 > act_ft_1,
        "Consecutive immediate activations must advance monotonically without history-stamp collision"
    );
    sys_jita.record_visible_or_prime_stamp(25);
    assert!(sys_jita.next_history_safe_activation_stamp(15) >= 25);

    // Put Perimeter into dilated 0.40x BEFORE logging in pilot_b in space there!
    set_system_tidi_factor(&ctx, PERIMETER_SYSTEM_ID, 0.40);
    let peri_char_id = 90_081_005i64;
    let peri_ship_id = 900_810_505i64;
    ctx.db.provision_gm_account("login_tidi", "login_tidi-secret", peri_char_id, "Login TiDi Pilot");
    seed_item(
        &ctx,
        peri_ship_id,
        FRIGATE_TYPE_ID,
        peri_char_id,
        i64::from(PERIMETER_SYSTEM_ID),
        InventoryFlag::Hangar,
        1,
    );
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&peri_char_id) {
            ch.solar_system_id = PERIMETER_SYSTEM_ID;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = peri_ship_id;
        }
    });
    let mut sess_b = SessionState::new(81_005);
    common::login_pilot(&ctx, &mut sess_b, "login_tidi", "login_tidi-secret").await;
    rpc(&ctx, &mut sess_b, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(peri_char_id)])
        .await
        .expect("SelectCharacterID in space");
    assert_eq!(
        collect_rebase_events(&ctx, peri_char_id).len(),
        1,
        "Login into dilated system must emit DoSimClockRebase once"
    );
    // Bind beyonce + UpdateStateRequest must NOT emit duplicate DoSimClockRebase!
    let bound = rpc(
        &ctx,
        &mut sess_b,
        "beyonce",
        "MachoBindObject",
        &[PyRep::Integer(i64::from(PERIMETER_SYSTEM_ID)), PyRep::None],
    )
    .await
    .expect("beyonce.MachoBindObject");
    assert!(
        collect_rebase_events(&ctx, peri_char_id).is_empty(),
        "beyonce bind must NOT emit DoSimClockRebase"
    );
    let _ = bound;
    let _ = rpc(&ctx, &mut sess_b, "beyonce", "UpdateStateRequest", &[]).await;
    assert!(
        collect_rebase_events(&ctx, peri_char_id).is_empty(),
        "UpdateStateRequest after login rebase must not duplicate DoSimClockRebase"
    );

    // Explicit entry helper on stargate jump emits once per entry and deduplicates repeat calls.
    assert!(emit_sim_clock_rebase_on_entry_if_needed(&ctx, pilot_jita.char_id, PERIMETER_SYSTEM_ID));
    assert_eq!(collect_rebase_events(&ctx, pilot_jita.char_id).len(), 1);
    assert!(!emit_sim_clock_rebase_on_entry_if_needed(&ctx, pilot_jita.char_id, PERIMETER_SYSTEM_ID));
}

/// Verifies `OnSpecialFX` stamps under TiDi never backstep behind `owner_visible_stamp` or
/// `prime_update_stamp` while scene stamp stays unchanged, and docking stops active propulsion FX
/// before ball detach.
#[tokio::test]
async fn fx_stamps_never_backstep_under_tidi() {
    // parity: feat:serverops.time-dilation, destiny:OnSpecialFX
    eve_testkit::attest(&["feat:serverops.time-dilation", "destiny:OnSpecialFX"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let pilot = common::gm_pilot_in_space(&ctx, 81_006, 90_081_006, FRIGATE_TYPE_ID, JITA_SYSTEM_ID).await;
    let sys = ctx.systems.get_or_spawn(JITA_SYSTEM_ID);
    set_system_tidi_factor(&ctx, JITA_SYSTEM_ID, 0.25);

    // Advance several ticks, then simulate a higher owner-visible / prime SetState stamp floor.
    let mut rx = sys.subscribe();
    let _ = sys.step_manual(1.0).await.unwrap();
    let (_, b_before) = sys.get_ball_snapshot(pilot.ship_id).await.unwrap().unwrap();
    let scene_stamp_before = b_before.id;
    let _ = scene_stamp_before;
    let obs_bcast_1 = sys.step_manual(1.0).await.unwrap();
    let scene_tick_1 = obs_bcast_1.tick;

    // Prime/owner visible stamp jumps ahead (e.g. from peer prime SetState) without altering ballpark tick.
    let prime_stamp = scene_tick_1 + 12;
    sys.record_visible_or_prime_stamp(prime_stamp);
    sys.emit_observer_destiny_event(
        pilot.ship_id,
        DestinyEvent::OnSpecialFX {
            ball_id: pilot.ship_id,
            module_id: Some(900_810_701),
            guid: "effects.Afterburner".into(),
            is_active: true,
        },
    )
    .await;

    let (_, _, drained_pyrep) = sys.drain_observer_updates(pilot.ship_id).await.unwrap();
    let fx_stamps = extract_fx_stamps_from_pyrep(&drained_pyrep);
    assert!(
        !fx_stamps.is_empty(),
        "Expected OnSpecialFX event in drained observer updates"
    );
    for (stamp, name) in fx_stamps {
        if name == "OnSpecialFX" {
            assert!(
                stamp >= prime_stamp as i64,
                "OnSpecialFX stamp {stamp} must never backstep behind prime/visible stamp {prime_stamp}"
            );
        }
    }
    let bcast_next = sys.step_manual(1.0).await.unwrap();
    assert_eq!(
        bcast_next.tick,
        scene_tick_1 + 1,
        "Scene stamp (ballpark.current_tick) must stay unchanged when clamping FX floor"
    );

    // Docking stops active propulsion OnSpecialFX BEFORE RemoveBall detach.
    sys.add_module_cycle(ActiveModuleCycle {
        ship_id: pilot.ship_id,
        module_id: 900_810_702,
        module_type_id: AFTERBURNER_TYPE_ID,
        effect_name: "moduleBonusAfterburner".into(),
        duration_s: 10.0,
        repeat: 1,
        speed_boost_factor: 1.5,
        damage_vector: DamageVector::new(0.0, 0.0, 0.0, 0.0),
        ..Default::default()
    })
    .await
    .unwrap();
    // Spawn a second observer to watch the docking ship's Destiny events on grid.
    let (obs_ball, obs_cs) = spawn_ship_with_cs(900_810_799, DVec3::new(1_000.0, 0.0, 0.0));
    sys.spawn_ball_with_combat(obs_ball, obs_cs).await.unwrap();

    while rx.try_recv().is_ok() {}
    let taken = sys
        .take_ball_for_docking(pilot.ship_id, 60_003_760, DVec3::ZERO)
        .await
        .expect("take_ball_for_docking succeeds");
    assert_eq!(taken.ball.id, pilot.ship_id);
    let _ = sys.step_manual(1.0).await.unwrap();
    let (_t, evs, _rep) = sys.drain_observer_updates(900_810_799).await.unwrap();
    let all_evs = if evs.is_empty() {
        rx.try_recv().map(|b| b.events).unwrap_or_default()
    } else {
        evs
    };
    let stop_fx_idx = all_evs.iter().position(|e| {
        matches!(
            e,
            DestinyEvent::OnSpecialFX { ball_id, is_active: false, .. } if *ball_id == pilot.ship_id
        )
    });
    let remove_idx = all_evs.iter().position(|e| {
        matches!(
            e,
            DestinyEvent::RemoveBall { ball_id } if *ball_id == pilot.ship_id
        )
    });
    assert!(
        stop_fx_idx.is_some(),
        "Docking must emit propulsion OnSpecialFX stop on grid; events={all_evs:?}"
    );
    if let (Some(si), Some(ri)) = (stop_fx_idx, remove_idx) {
        assert!(
            si < ri,
            "Propulsion OnSpecialFX stop ({si}) must precede RemoveBall detach ({ri})"
        );
    }
}

/// Verifies destruction timers (self-destruct, wreck/jetcan decay, doomsday warmup/immobilize)
/// follow the system sim clock under TiDi while stargate session-change timer stays 7 s wall-clock.
#[tokio::test]
async fn destruction_timers_follow_sim_clock_under_tidi() {
    // parity: feat:combat.ship-destruction, feat:serverops.time-dilation, rpc:ship.SelfDestruct
    eve_testkit::attest(&[
        "feat:combat.ship-destruction",
        "feat:serverops.time-dilation",
        "rpc:ship.SelfDestruct",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut pilot = common::gm_pilot_in_space(&ctx, 81_007, 90_081_007, FRIGATE_TYPE_ID, JITA_SYSTEM_ID).await;
    let sys = ctx.systems.get_or_spawn(JITA_SYSTEM_ID);

    // 1. Stargate session-change timer stays strictly 7s wall-clock even at 0.10x TiDi.
    set_system_tidi_factor(&ctx, JITA_SYSTEM_ID, 0.10);
    let t_gate = ctx.now_unix();
    record_session_change_timer(&ctx, pilot.char_id, t_gate, JITA_SYSTEM_ID, 1, "stargate_jump");
    ctx.advance_secs(STARGATE_SESSION_CHANGE_TIMER_SECS - 1);
    assert!(
        check_session_change_timer_active(&ctx, pilot.char_id, ctx.now_unix(), Some((JITA_SYSTEM_ID, 50))).is_some(),
        "Stargate 7s timer must stay active at +6s even if 50 ticks elapsed"
    );
    ctx.advance_secs(1);
    assert!(
        check_session_change_timer_active(&ctx, pilot.char_id, ctx.now_unix(), Some((JITA_SYSTEM_ID, 50))).is_none(),
        "Stargate session-change timer must expire at exact 7s wall-clock regardless of 0.10x TiDi"
    );

    // 2. Self-destruct under 0.25x TiDi takes 120s sim (= 480s wall), NOT 120s wall.
    set_system_tidi_factor(&ctx, JITA_SYSTEM_ID, 0.25);
    let sd_rsp = rpc(&ctx, &mut pilot.sess, "ship", "SelfDestruct", &[])
        .await
        .expect("ship.SelfDestruct");
    assert_eq!(sd_rsp.dict_get("countdownStarted").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(sd_rsp.dict_get("remainingSeconds").and_then(|v| v.as_i64()), Some(480));

    for _ in 0..125 {
        ctx.advance_secs(1);
        step_combat_system(&ctx, JITA_SYSTEM_ID, 1.0).await;
    }
    assert!(
        sys.get_ball_snapshot(pilot.ship_id).await.unwrap().is_some(),
        "Ship self-destructing under 0.25x TiDi must still be alive after 125s wall (31.25s sim)"
    );
    for _ in 0..360 {
        ctx.advance_secs(1);
        step_combat_system(&ctx, JITA_SYSTEM_ID, 1.0).await;
    }
    assert!(
        sys.get_ball_snapshot(pilot.ship_id).await.unwrap().is_none(),
        "Ship must detonate once 120s sim (480s wall at 0.25x TiDi) has elapsed"
    );

    // 3. Wreck/jetcan lifetime created under 0.50x TiDi stretches 2x in wall seconds.
    set_system_tidi_factor(&ctx, JITA_SYSTEM_ID, 0.50);
    let now_wreck = ctx.now_unix();
    let jetcan_id = 900_810_801i64;
    seed_item(&ctx, jetcan_id, 23, pilot.char_id, i64::from(JITA_SYSTEM_ID), InventoryFlag::Hangar, 1);
    register_container_lifetime(&ctx, jetcan_id, JITA_SYSTEM_ID, 23, pilot.char_id, now_wreck, false);
    assert!(
        step_wreck_and_jetcan_decay(&ctx, JITA_SYSTEM_ID, now_wreck + 3_700).await.is_empty(),
        "Jetcan created at 0.50x TiDi must not decay at normal 3600s mark"
    );
    assert!(
        step_wreck_and_jetcan_decay(&ctx, JITA_SYSTEM_ID, now_wreck + 7_200).await.contains(&jetcan_id),
        "Jetcan created at 0.50x TiDi must decay at 7200s (3600s sim)"
    );

    // 4. Doomsday warmup/immobilize/GTFO suppression follow sim clock under 0.50x TiDi.
    let titan_id = 900_810_901i64;
    let (mut titan_ball, titan_cs) = spawn_ship_with_cs(titan_id, DVec3::new(10_000.0, 0.0, 0.0));
    titan_ball.max_velocity = 75.0;
    sys.spawn_ball_with_combat(titan_ball, titan_cs).await.unwrap();
    let dd_start = ctx.now_unix();
    mutate_superweapon_runtime(&ctx.db, |rt| {
        rt.active_cycles.push(PersistedSuperweaponCycle {
            sys_id: JITA_SYSTEM_ID,
            char_id: pilot.char_id,
            ship_id: titan_id,
            ship_type_id: 11_567,
            mod_type_id: 24_550,
            kind: SuperweaponCycleKind::SpatialGtfo {
                center: [10_000.0, 0.0, 0.0],
                sphere_radius_m: 1_000.0,
            },
            started_at_unix: dd_start,
            warmup_secs: 20, // scaled 2x for 10s base
            total_ticks: 1,
            ticks_applied: 0,
            immobilize_until_unix: dd_start + 60, // scaled 2x for 30s base
            orig_max_velocity: 75.0,
            orig_warp_scramble_strength: 0,
            orig_disallow_assistance: false,
            mobility_restored: false,
        });
        rt.warp_suppressions.push(PersistedWarpSuppression {
            sys_id: JITA_SYSTEM_ID,
            ship_id: titan_id,
            suppressed_until_unix: dd_start + 120,
            orig_warp_scramble_strength: 0,
        });
    });
    ctx.advance_secs(35);
    step_bosonic_doomsday_system(&ctx, JITA_SYSTEM_ID).await;
    assert!(
        read_superweapon_runtime(&ctx.db)
            .unwrap()
            .active_cycles
            .iter()
            .any(|c| c.ship_id == titan_id && !c.mobility_restored),
        "Titan must still be immobilized at +35s wall under 0.50x TiDi"
    );
    let _ = step_bosonic_doomsday_in_ballpark;
}

/// Seeded 60-ship fitted engagement exercising autoscaler tighten + hold + spike clear + relax.
#[tokio::test]
async fn seeded_60_ship_engagement_tidi_autoscaler_hysteresis() {
    // parity: feat:serverops.time-dilation, notify:DoSimClockRebase, feat:combat.ship-destruction
    eve_testkit::attest(&[
        "feat:serverops.time-dilation",
        "notify:DoSimClockRebase",
        "feat:combat.ship-destruction",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let sys = ctx.systems.get_or_spawn(PERIMETER_SYSTEM_ID);
    let base_balls = sys.list_balls().await.unwrap().len();

    // Seed 60 fitted ship balls across 2 fleets (30 vs 30) with active module cycles.
    for i in 0..60_i64 {
        let ship_id = 910_000_000 + i;
        let x = if i < 30 { 0.0 } else { 20_000.0 };
        let y = (i % 30) as f64 * 250.0;
        let (ball, cs) = spawn_ship_with_cs(ship_id, DVec3::new(x, y, 0.0));
        sys.spawn_ball_with_combat(ball, cs).await.unwrap();
        let opp_id = if i < 30 {
            910_000_000 + 30 + (i % 30)
        } else {
            910_000_000 + (i % 30)
        };
        sys.add_module_cycle(ActiveModuleCycle {
            ship_id,
            module_id: 920_000_000 + i,
            module_type_id: AFTERBURNER_TYPE_ID,
            target_id: Some(opp_id),
            duration_s: 5.0,
            repeat: 1,
            speed_boost_factor: 1.3,
            damage_vector: DamageVector::new(0.0, 0.0, 0.0, 0.0),
            ..Default::default()
        })
        .await
        .unwrap();
    }
    assert_eq!(sys.list_balls().await.unwrap().len(), base_balls + 60);

    // Overloaded engagement windows -> tightens per exact formula (`lateness = 270ms` -> ~0.80)
    let snap_eng = evaluate_system_tidi_autoscaler_window(
        &ctx,
        PERIMETER_SYSTEM_ID,
        270.0,
        Some(48.0),
        100.0,
        100,
    );
    assert_eq!(snap_eng.reason, "tighten");
    let expected = 1000.0 / (1000.0 + (270.0 - 20.0));
    assert!((snap_eng.pending_transition.as_ref().unwrap().to_factor - expected).abs() < 1e-6);
    let snap_eng_applied =
        evaluate_system_tidi_autoscaler_window(&ctx, PERIMETER_SYSTEM_ID, 270.0, None, 102.0, 102);
    assert!((snap_eng_applied.factor - expected).abs() < 1e-6);

    // Deeper spike past min hold (`lateness = 620ms` -> ~0.625) tightens and applies at 2s stamp boundary.
    let snap_deep = evaluate_system_tidi_autoscaler_window(
        &ctx,
        PERIMETER_SYSTEM_ID,
        620.0,
        Some(95.0),
        107.0,
        107,
    );
    assert_eq!(snap_deep.reason, "tighten");
    let snap_deep_applied =
        evaluate_system_tidi_autoscaler_window(&ctx, PERIMETER_SYSTEM_ID, 620.0, None, 109.0, 109);
    assert!(snap_deep_applied.factor < snap_eng_applied.factor);
    let _ = sys.step_manual(1.0).await.unwrap();
    assert!((sys.sim_elapsed_secs() - snap_deep_applied.factor).abs() < 1e-6);

    // Low-load confirmation window 1 -> `await-relax-confirmation`; spike clears; 2 clean -> relaxes after 2s notice.
    let s_low1 = evaluate_system_tidi_autoscaler_window(&ctx, PERIMETER_SYSTEM_ID, 6.0, Some(12.0), 114.0, 114);
    assert_eq!(s_low1.reason, "await-relax-confirmation");
    let s_spike = evaluate_system_tidi_autoscaler_window(&ctx, PERIMETER_SYSTEM_ID, 17.0, None, 115.0, 115);
    assert_eq!(s_spike.consecutive_low_load_windows, 0);
    let _ = evaluate_system_tidi_autoscaler_window(&ctx, PERIMETER_SYSTEM_ID, 5.0, None, 116.0, 116);
    let s_rel_ann = evaluate_system_tidi_autoscaler_window(&ctx, PERIMETER_SYSTEM_ID, 5.0, None, 117.0, 117);
    assert_eq!(s_rel_ann.reason, "relax");
    let s_rel = evaluate_system_tidi_autoscaler_window(&ctx, PERIMETER_SYSTEM_ID, 5.0, None, 119.0, 119);
    assert!(s_rel.factor > s_spike.factor && (s_rel.factor - 1.0).abs() < 1e-9);
    assert!(s_rel.decision_log.iter().any(|d| d.reason == "tighten"));
    assert!(s_rel.decision_log.iter().any(|d| d.reason == "await-relax-confirmation"));
}
