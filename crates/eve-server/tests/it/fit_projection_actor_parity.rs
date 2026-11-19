//! End-to-end integration tests verifying `#922` (`[RFC-1 E-5] Fit as a derived cache`):
//! 1. Differential parity between live in-space `SystemActor` (`FitProjection`, `CombatState`,
//!    `Ball`) and a fresh `ctx.fit(char_id, ship_id)` projection across fit mutations in space.
//! 2. Active shield and armor hardeners reduce incoming EM damage for a pilot with NO skill
//!    queue, and deactivating them restores base resists cleanly (`OnDamageStateChange` + damage).
//! 3. Mid-fight ECM jamming breaks non-jammer target locks / stops weapons, and Tracking
//!    Disruption (TD) applied mid-fight takes effect on the next turret cycle boundary (and
//!    restores on the subsequent cycle after deactivation).
//! 4. Fitting and activating a 5MN Microwarpdrive on a battleship applies EVE's mass-scaled
//!    thrust formula `(speedFactor / 100) * speedBoostFactor / (mass + massAddition)` rather
//!    than a frigate-sized velocity multiplier.

#![forbid(unsafe_code)]

use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
use eve_destiny::{Ball, DVec3, MovementMode};
use eve_dogma::{CombatBaseline, CycleParams, FitProjection, PropulsionParams};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::rng::MechanicRng;
use eve_server::router::ServerContext;
use eve_server::system_actor::SystemHandle;
use eve_testkit::client_harness::{ClientErrorResponse, ClientHarness};
use std::collections::{HashMap, HashSet};

const NULL_SYS: u32 = 30_002_423;
const NULL_STATION: i64 = 60_012_730;
const JITA_SYS: u32 = 30_000_142;

const CONFESSOR: u32 = 34_317;
const STANCE_DEFENSE: i64 = 34_319;
const STANCE_SHARPSHOOTER: i64 = 34_321;
const STANCE_PROPULSION: i64 = 34_323;

const SMALL_FOCUSED_PULSE_LASER_II: u32 = 3_041;
const MULTIFREQUENCY_S: u32 = 246;
const SCORCH_S: u32 = 12_563;
const ONE_MN_AB_I: u32 = 439;
const MULTISPECTRUM_SHIELD_HARDENER_II: u32 = 2_281;
const EM_SHIELD_HARDENER_II: u32 = 2_301;
const EM_ARMOR_HARDENER_II: u32 = 11_642;
const HEAT_SINK_II: u32 = 2_364;
const MOBILE_DEPOT: u32 = 33_474;
const ROGUE_NN_602: u32 = 3_096;
const STANDARD_DROP_BOOSTER: u32 = 15_466;

const RAVEN: u32 = 638;
const SCORPION: u32 = 640;
const CRUCIFIER: u32 = 2_161;
const RIFTER: u32 = 587;
const FIVE_MN_MWD_I: u32 = 434;
const KESTREL: u32 = 602;
const LIGHT_MISSILE_LAUNCHER_II: u32 = 2_404;
const MJOLNIR_LIGHT_MISSILE: u32 = 212;
const MULTISPECTRAL_ECM_II: u32 = 2_567;
const TRACKING_DISRUPTOR_II: u32 = 2_109;
const OPTIMAL_RANGE_DISRUPTION_SCRIPT: u32 = 29_005;
const TRACKING_SPEED_DISRUPTION_SCRIPT: u32 = 29_007;

async fn rpc_harness_call(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    bound: Option<u64>,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, ClientErrorResponse> {
    let kw = HashMap::new();
    ClientHarness::new(common::SessionBorrowWireTransport {
        ctx,
        session: sess,
        preserve_char_notifications: true,
    })
    .call(svc, bound, method, args, &kw)
    .await
}

async fn rpc_ok(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    bound: Option<u64>,
    method: &str,
    args: &[PyRep],
) -> PyRep {
    rpc_harness_call(ctx, sess, svc, bound, method, args)
        .await
        .unwrap_or_else(|e| panic!("{svc}.{method}({args:?}) failed: {e:?}"))
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
    let singleton = quantity == 1 && !ctx.sde.types.get(&type_id).is_some_and(|t| t.category_id == 8);
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
                singleton,
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

fn grant_required_skills_recursive(ctx: &ServerContext, char_id: i64, type_ids: &[u32]) {
    let mut visited: HashSet<u32> = HashSet::new();
    let mut stack: Vec<u32> = type_ids.to_vec();
    let mut to_grant: HashMap<u32, u8> = HashMap::new();
    while let Some(tid) = stack.pop() {
        if !visited.insert(tid) {
            continue;
        }
        let Some(t) = ctx.sde.types.get(&tid) else {
            continue;
        };
        for &(sk_attr, lv_attr) in &[(182u16, 277u16), (183, 278), (184, 279), (1285, 1286), (1289, 1287), (1290, 1288)] {
            if let Some(&sk_val) = t.default_attributes.get(&sk_attr) {
                let sk_tid = sk_val.round() as u32;
                if sk_tid > 0 {
                    let req_lvl = t.default_attributes.get(&lv_attr).copied().unwrap_or(1.0).round().clamp(1.0, 5.0) as u8;
                    to_grant
                        .entry(sk_tid)
                        .and_modify(|cur| *cur = (*cur).max(req_lvl))
                        .or_insert(req_lvl);
                    stack.push(sk_tid);
                }
            }
        }
    }
    ctx.db.with_mut_snapshot(|snap| {
        let char_skills = snap.skills.entry(char_id).or_default();
        for (sk_tid, lvl) in to_grant {
            let sp = 256_000.0;
            char_skills
                .entry(sk_tid)
                .and_modify(|rec| {
                    if rec.level < lvl {
                        rec.level = lvl;
                        rec.skill_points = rec.skill_points.max(sp);
                    }
                })
                .or_insert(TrainedSkillRecord {
                    type_id: sk_tid,
                    level: lvl,
                    skill_points: sp,
                });
        }
    });
}

fn set_skills(ctx: &ServerContext, char_id: i64, skills: &[(u32, u8)]) {
    ctx.db.with_mut_snapshot(|snap| {
        let char_skills = snap.skills.entry(char_id).or_default();
        for &(skill_type_id, level) in skills {
            let sp = match level {
                0 => 0.0,
                1 => 250.0,
                2 => 1_415.0,
                3 => 8_000.0,
                4 => 45_255.0,
                _ => 256_000.0,
            };
            char_skills.insert(
                skill_type_id,
                TrainedSkillRecord {
                    type_id: skill_type_id,
                    level,
                    skill_points: sp,
                },
            );
        }
    });
}

fn prepare_docked_character_at(
    ctx: &ServerContext,
    char_id: i64,
    solar_system_id: u32,
    station_id: i64,
) {
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = solar_system_id;
            ch.station_id = Some(station_id);
            ch.structure_id = None;
        }
    });
}

async fn login_and_board_new_ship(
    ctx: &ServerContext,
    client_id: u64,
    username: &str,
    password: &str,
    char_id: i64,
    solar_system_id: u32,
    station_id: i64,
    ship_id: i64,
    ship_type_id: u32,
) -> SessionState {
    prepare_docked_character_at(ctx, char_id, solar_system_id, station_id);
    let mut sess = SessionState::new(client_id);
    common::login_pilot(ctx, &mut sess, username, password).await;
    rpc_ok(
        ctx,
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
    )
    .await;
    seed_item(
        ctx,
        ship_id,
        ship_type_id,
        char_id,
        station_id,
        InventoryFlag::Hangar,
        1,
    );
    rpc_ok(
        ctx,
        &mut sess,
        "ship",
        None,
        "AssembleShip",
        &[PyRep::Integer(ship_id)],
    )
    .await;
    ctx.advance_secs(12);
    rpc_ok(ctx, &mut sess, "ship", None, "Board", &[PyRep::Integer(ship_id)]).await;
    sess
}

async fn undock_and_settle(
    ctx: &ServerContext,
    sess: &mut SessionState,
    solar_system_id: u32,
) -> (SystemHandle, u64) {
    let rep = rpc_ok(ctx, sess, "stationSvc", None, "Undock", &[]).await;
    let beyonce_id = match rep {
        PyRep::KeyVal(kv) => kv
            .get("beyonceBoundID")
            .and_then(|v| v.as_i64())
            .expect("Undock must yield beyonceBoundID") as u64,
        other => panic!("Unexpected Undock reply: {other:?}"),
    };
    let sys = ctx.systems.get_or_spawn(solar_system_id);
    // Clear 30s undock invulnerability so weapons can immediately apply damage in combat tests.
    ctx.advance_secs(35);
    sys.step_manual(35.0).await.expect("step past undock invuln");
    (sys, beyonce_id)
}

async fn reposition_stationary_ball(sys: &SystemHandle, ball_id: i64, position: DVec3) {
    let (_, mut ball) = sys
        .get_ball_snapshot(ball_id)
        .await
        .expect("get_ball_snapshot")
        .expect("ball present");
    let cs = sys
        .get_combat_state(ball_id)
        .await
        .expect("get_combat_state")
        .expect("combat state present");
    ball.position = position;
    ball.velocity = DVec3::ZERO;
    ball.speed_fraction = 0.0;
    ball.mode = MovementMode::Stop;
    sys.spawn_ball_with_combat(ball, cs)
        .await
        .expect("reposition ball");
}

async fn lock_target(
    ctx: &ServerContext,
    sess: &mut SessionState,
    sys: &SystemHandle,
    source_ship_id: i64,
    target_id: i64,
) {
    rpc_ok(
        ctx,
        sess,
        "dogmaIM",
        None,
        "AddTarget",
        &[PyRep::Integer(target_id)],
    )
    .await;
    for _ in 0..35 {
        ctx.advance_secs(1);
        sys.step_manual(1.0).await.expect("step target lock");
        let locks = sys.get_target_locks(source_ship_id).await.expect("get_target_locks");
        if locks.iter().any(|l| l.target_id == target_id && l.locked) {
            return;
        }
    }
    panic!("target lock from {source_ship_id} -> {target_id} did not complete within 35s");
}

fn assert_close(actual: f64, expected: f64, tol: f64, label: &str) {
    assert!(
        (actual - expected).abs() <= tol,
        "{label}: actual={actual} expected={expected} (diff={}, tol={tol})",
        (actual - expected).abs()
    );
}

fn assert_baseline_eq(actual: &CombatBaseline, expected: &CombatBaseline, label: &str) {
    assert_close(actual.max_velocity, expected.max_velocity, 1e-6, &format!("{label}.max_velocity"));
    assert_close(actual.mass, expected.mass, 1e-6, &format!("{label}.mass"));
    assert_close(actual.agility, expected.agility, 1e-6, &format!("{label}.agility"));
    assert_close(actual.signature_radius, expected.signature_radius, 1e-6, &format!("{label}.signature_radius"));
    assert_close(actual.max_shield_hp, expected.max_shield_hp, 1e-6, &format!("{label}.max_shield_hp"));
    assert_close(actual.max_armor_hp, expected.max_armor_hp, 1e-6, &format!("{label}.max_armor_hp"));
    assert_close(actual.max_structure_hp, expected.max_structure_hp, 1e-6, &format!("{label}.max_structure_hp"));
    assert_close(actual.max_capacitor, expected.max_capacitor, 1e-6, &format!("{label}.max_capacitor"));
    assert_close(actual.shield_recharge_time_ms, expected.shield_recharge_time_ms, 1e-6, &format!("{label}.shield_recharge_time_ms"));
    assert_close(actual.capacitor_recharge_time_ms, expected.capacitor_recharge_time_ms, 1e-6, &format!("{label}.capacitor_recharge_time_ms"));
    assert_close(actual.warp_speed_au_s, expected.warp_speed_au_s, 1e-6, &format!("{label}.warp_speed_au_s"));
    assert_eq!(actual.max_locked_targets, expected.max_locked_targets, "{label}.max_locked_targets");
    assert_close(actual.max_target_range, expected.max_target_range, 1e-6, &format!("{label}.max_target_range"));
    assert_close(actual.scan_resolution, expected.scan_resolution, 1e-6, &format!("{label}.scan_resolution"));
    for ch in 0..4 {
        assert_close(actual.shield_resonance[ch], expected.shield_resonance[ch], 1e-9, &format!("{label}.shield_resonance[{ch}]"));
        assert_close(actual.armor_resonance[ch], expected.armor_resonance[ch], 1e-9, &format!("{label}.armor_resonance[{ch}]"));
        assert_close(actual.structure_resonance[ch], expected.structure_resonance[ch], 1e-9, &format!("{label}.structure_resonance[{ch}]"));
    }
}

fn assert_propulsion_eq(
    actual: Option<&PropulsionParams>,
    expected: Option<&PropulsionParams>,
    label: &str,
) {
    match (actual, expected) {
        (None, None) => {}
        (Some(a), Some(e)) => {
            assert_eq!(a.module_id, e.module_id, "{label}.prop.module_id");
            assert_eq!(a.module_type_id, e.module_type_id, "{label}.prop.module_type_id");
            assert_eq!(a.is_mwd, e.is_mwd, "{label}.prop.is_mwd");
            assert_eq!(a.is_overloaded, e.is_overloaded, "{label}.prop.is_overloaded");
            assert_close(a.speed_factor, e.speed_factor, 1e-9, &format!("{label}.prop.speed_factor"));
            assert_close(a.speed_boost_factor, e.speed_boost_factor, 1e-9, &format!("{label}.prop.speed_boost_factor"));
            assert_close(a.mass_addition, e.mass_addition, 1e-6, &format!("{label}.prop.mass_addition"));
            assert_close(a.signature_radius_bonus_pct, e.signature_radius_bonus_pct, 1e-9, &format!("{label}.prop.sig_bonus_pct"));
            assert_close(a.speed_boost, e.speed_boost, 1e-9, &format!("{label}.prop.speed_boost"));
        }
        _ => panic!("{label}: propulsion presence mismatch: actual={actual:?} expected={expected:?}"),
    }
}

fn assert_module_eq(actual: &CycleParams, expected: &CycleParams, label: &str) {
    assert_eq!(actual.module_type_id, expected.module_type_id, "{label}.module_type_id");
    assert_eq!(actual.flag_id, expected.flag_id, "{label}.flag_id");
    assert_eq!(actual.state, expected.state, "{label}.state");
    assert_eq!(actual.is_overloaded, expected.is_overloaded, "{label}.is_overloaded");
    assert_eq!(actual.charge_type_id, expected.charge_type_id, "{label}.charge_type_id");
    assert_close(actual.duration_ms, expected.duration_ms, 1e-6, &format!("{label}.duration_ms"));
    assert_close(actual.capacitor_need, expected.capacitor_need, 1e-6, &format!("{label}.capacitor_need"));
    assert_close(actual.damage_multiplier, expected.damage_multiplier, 1e-9, &format!("{label}.damage_multiplier"));
    assert_close(actual.optimal_range_m, expected.optimal_range_m, 1e-6, &format!("{label}.optimal_range_m"));
    assert_close(actual.falloff_m, expected.falloff_m, 1e-6, &format!("{label}.falloff_m"));
    assert_close(actual.tracking_speed, expected.tracking_speed, 1e-9, &format!("{label}.tracking_speed"));
    assert_close(actual.shield_boost_amount, expected.shield_boost_amount, 1e-6, &format!("{label}.shield_boost_amount"));
    assert_close(actual.armor_repair_amount, expected.armor_repair_amount, 1e-6, &format!("{label}.armor_repair_amount"));
    assert_close(actual.hull_repair_amount, expected.hull_repair_amount, 1e-6, &format!("{label}.hull_repair_amount"));
    assert_close(actual.prop_speed_boost, expected.prop_speed_boost, 1e-9, &format!("{label}.prop_speed_boost"));
    for ch in 0..4 {
        assert_close(actual.damage[ch], expected.damage[ch], 1e-6, &format!("{label}.damage[{ch}]"));
    }
}

async fn assert_actor_matches_fresh_fit(
    ctx: &ServerContext,
    sys: &SystemHandle,
    char_id: i64,
    ship_id: i64,
    label: &str,
) -> FitProjection {
    // Query actor state first so FIFO SystemActor commands (e.g. ApplyFitProjection queued via
    // try_send) are guaranteed to have been processed before reading latest_fit_projection.
    let cs = sys
        .get_combat_state(ship_id)
        .await
        .expect("get_combat_state")
        .expect("combat state exists");
    let (_, ball) = sys
        .get_ball_snapshot(ship_id)
        .await
        .expect("get_ball_snapshot")
        .expect("ball exists");

    let actor_proj = sys
        .latest_fit_projection(ship_id)
        .unwrap_or_else(|| panic!("{label}: SystemActor missing FitProjection for {ship_id}"));

    let assert_fit_matches = |fit: &eve_dogma::ShipFittingEngine, pass_label: &str| {
        let expected_proj = fit.to_projection(actor_proj.rev, &ctx.sde);

        assert_eq!(actor_proj.ship_id, expected_proj.ship_id, "{pass_label}: ship_id");
        assert_eq!(actor_proj.ship_type_id, expected_proj.ship_type_id, "{pass_label}: ship_type_id");
        assert_eq!(
            actor_proj.active_stance_type_id, expected_proj.active_stance_type_id,
            "{pass_label}: active_stance_type_id"
        );
        assert_eq!(
            actor_proj.last_stance_switch_ms, expected_proj.last_stance_switch_ms,
            "{pass_label}: last_stance_switch_ms"
        );
        assert_baseline_eq(&actor_proj.baseline, &expected_proj.baseline, pass_label);
        assert_propulsion_eq(
            actor_proj.propulsion.as_ref(),
            expected_proj.propulsion.as_ref(),
            pass_label,
        );

        let mut actor_keys: Vec<i64> = actor_proj.modules.keys().copied().collect();
        let mut exp_keys: Vec<i64> = expected_proj.modules.keys().copied().collect();
        actor_keys.sort_unstable();
        exp_keys.sort_unstable();
        assert_eq!(actor_keys, exp_keys, "{pass_label}: fitted module keys");
        for mid in exp_keys {
            assert_module_eq(
                &actor_proj.modules[&mid],
                &expected_proj.modules[&mid],
                &format!("{pass_label}.mod[{mid}]"),
            );
        }

        assert_close(cs.max_shield_hp, expected_proj.baseline.max_shield_hp, 1e-6, &format!("{pass_label}.cs.max_shield_hp"));
        assert_close(cs.max_armor_hp, expected_proj.baseline.max_armor_hp, 1e-6, &format!("{pass_label}.cs.max_armor_hp"));
        assert_close(cs.max_structure_hp, expected_proj.baseline.max_structure_hp, 1e-6, &format!("{pass_label}.cs.max_structure_hp"));
        assert_close(cs.max_capacitor, expected_proj.baseline.max_capacitor, 1e-6, &format!("{pass_label}.cs.max_capacitor"));
        assert_close(cs.shield_recharge_time_ms, expected_proj.baseline.shield_recharge_time_ms, 1e-6, &format!("{pass_label}.cs.shield_recharge_time_ms"));
        assert_close(cs.capacitor_recharge_time_ms, expected_proj.baseline.capacitor_recharge_time_ms, 1e-6, &format!("{pass_label}.cs.capacitor_recharge_time_ms"));
        for ch in 0..4 {
            assert_close(cs.shield_resonance[ch], expected_proj.baseline.shield_resonance[ch], 1e-9, &format!("{pass_label}.cs.shield_resonance[{ch}]"));
            assert_close(cs.armor_resonance[ch], expected_proj.baseline.armor_resonance[ch], 1e-9, &format!("{pass_label}.cs.armor_resonance[{ch}]"));
            assert_close(cs.structure_resonance[ch], expected_proj.baseline.structure_resonance[ch], 1e-9, &format!("{pass_label}.cs.structure_resonance[{ch}]"));
        }

        assert_close(ball.inertia, expected_proj.baseline.agility, 1e-6, &format!("{pass_label}.ball.inertia"));
        if let Some(prop) = expected_proj.propulsion {
            assert_close(
                ball.max_velocity,
                expected_proj.baseline.max_velocity * (1.0 + prop.speed_boost),
                1e-6,
                &format!("{pass_label}.ball.max_velocity(prop)"),
            );
            assert_close(
                ball.mass,
                expected_proj.baseline.mass + prop.mass_addition,
                1e-3,
                &format!("{pass_label}.ball.mass(prop)"),
            );
            assert_close(
                cs.signature_radius,
                expected_proj.baseline.signature_radius * (1.0 + prop.signature_radius_bonus_pct / 100.0),
                1e-6,
                &format!("{pass_label}.cs.signature_radius(prop)"),
            );
        } else {
            assert_close(
                ball.max_velocity,
                expected_proj.baseline.max_velocity,
                1e-6,
                &format!("{pass_label}.ball.max_velocity(base)"),
            );
            assert_close(
                ball.mass,
                expected_proj.baseline.mass,
                1e-3,
                &format!("{pass_label}.ball.mass(base)"),
            );
            assert_close(
                cs.signature_radius,
                expected_proj.baseline.signature_radius,
                1e-6,
                &format!("{pass_label}.cs.signature_radius(base)"),
            );
        }
    };

    let warm_fit = ctx
        .fit(char_id, ship_id)
        .unwrap_or_else(|e| panic!("{label} (warm): ctx.fit failed: {e:?}"));
    assert_fit_matches(&warm_fit, &format!("{label} (warm)"));

    // Evict cached ShipFittingEngine so ctx.fit rebuilds fitted modules, module states,
    // and tactical stance cold from WorldStateSnapshot and persisted fit inputs.
    ctx.ship_fittings
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&ship_id);
    let cold_fit = ctx
        .fit(char_id, ship_id)
        .unwrap_or_else(|e| panic!("{label} (cold): ctx.fit failed: {e:?}"));
    assert_fit_matches(&cold_fit, &format!("{label} (cold)"));

    actor_proj
}

fn find_seed_under(limit: f64) -> u64 {
    (1_u64..10_000)
        .find(|&s| MechanicRng::seeded(s).try_unit_f64().unwrap_or(1.0) < limit)
        .expect("deterministic seed under threshold")
}

#[tokio::test]
async fn fit_projection_actor_parity_differential_in_space_mutations() {
    // Verifies #922 criterion 1: after fitting/unfitting, charge load/swap/unload, stance
    // transitions, overload toggles, skill completion, booster consumption, and implant plug/destroy
    // in space, SystemActor's FitProjection + CombatState + Ball match fresh ctx.fit(ship).
    let ctx = ServerContext::new();
    let _guard = common::teardown_guard(&ctx);
    common::pin_clock(&ctx);

    let char_id = 90_000_001_i64;
    let ship_id = 922_100_001_i64;
    grant_required_skills_recursive(
        &ctx,
        char_id,
        &[
            CONFESSOR,
            SMALL_FOCUSED_PULSE_LASER_II,
            SCORCH_S,
            ONE_MN_AB_I,
            MULTISPECTRUM_SHIELD_HARDENER_II,
            HEAT_SINK_II,
            MOBILE_DEPOT,
            ROGUE_NN_602,
            STANDARD_DROP_BOOSTER,
        ],
    );
    // Fitting, Thermodynamics V, Biology V, Cybernetics V, Navigation V, Capacitor V.
    set_skills(
        &ctx,
        char_id,
        &[
            (3300, 5),  // Gunnery V
            (3303, 5),  // Small Energy Turret V
            (3310, 5),  // Sharpshooter V
            (3311, 5),  // Rapid Firing V
            (3312, 5),  // Trajectory Analysis V
            (3318, 5),  // Weapon Upgrades V
            (3319, 5),  // Energy Grid Upgrades V
            (3405, 5),  // Biology V
            (3411, 5),  // Cybernetics V
            (3413, 5),  // Power Grid Management V
            (3417, 5),  // Capacitor Systems Operation V
            (3418, 5),  // Capacitor Management V
            (3425, 5),  // Shield Upgrades V
            (3426, 5),  // CPU Management V
            (3449, 5),  // Navigation V
            (3450, 5),  // Afterburner V
            (3453, 5),  // Evasive Maneuvering V
            (11_584, 5), // Anchoring V
            (25_538, 5), // Neurotoxin Control V
            (28_164, 5), // Thermodynamics V
            // Start Target Management (3429) at level 1 so we can train level 2 in space.
            (3429, 1),
        ],
    );

    let mut sess = login_and_board_new_ship(
        &ctx,
        922_001,
        "demo",
        "demo-secret",
        char_id,
        NULL_SYS,
        NULL_STATION,
        ship_id,
        CONFESSOR,
    )
    .await;

    let laser_id = 922_100_010_i64;
    let ab_id = 922_100_011_i64;
    let hardener_id = 922_100_012_i64;
    let heatsink_id = 922_100_013_i64;
    let mf_ammo_id = 922_100_014_i64;
    let scorch_ammo_id = 922_100_015_i64;
    let depot_id = 922_100_016_i64;
    let implant_id = 922_100_017_i64;
    let booster_id = 922_100_018_i64;

    seed_item(&ctx, laser_id, SMALL_FOCUSED_PULSE_LASER_II, char_id, NULL_STATION, InventoryFlag::Hangar, 1);
    seed_item(&ctx, ab_id, ONE_MN_AB_I, char_id, NULL_STATION, InventoryFlag::Hangar, 1);
    seed_item(&ctx, hardener_id, MULTISPECTRUM_SHIELD_HARDENER_II, char_id, ship_id, InventoryFlag::Cargo, 1);
    seed_item(&ctx, heatsink_id, HEAT_SINK_II, char_id, ship_id, InventoryFlag::Cargo, 1);
    seed_item(&ctx, mf_ammo_id, MULTIFREQUENCY_S, char_id, ship_id, InventoryFlag::Cargo, 2);
    seed_item(&ctx, scorch_ammo_id, SCORCH_S, char_id, ship_id, InventoryFlag::Cargo, 2);
    seed_item(&ctx, depot_id, MOBILE_DEPOT, char_id, ship_id, InventoryFlag::Cargo, 1);
    seed_item(&ctx, implant_id, ROGUE_NN_602, char_id, ship_id, InventoryFlag::Cargo, 1);
    seed_item(&ctx, booster_id, STANDARD_DROP_BOOSTER, char_id, ship_id, InventoryFlag::Cargo, 1);

    rpc_ok(&ctx, &mut sess, "invbroker", None, "Add", &[
        PyRep::Integer(laser_id),
        PyRep::Integer(ship_id),
        PyRep::Integer(InventoryFlag::HiSlot0 as i64),
    ]).await;
    rpc_ok(&ctx, &mut sess, "invbroker", None, "Add", &[
        PyRep::Integer(ab_id),
        PyRep::Integer(ship_id),
        PyRep::Integer(InventoryFlag::MedSlot0 as i64),
    ]).await;

    let (sys, beyonce_id) = undock_and_settle(&ctx, &mut sess, NULL_SYS).await;
    rpc_ok(&ctx, &mut sess, "beyonce", Some(beyonce_id), "CmdStop", &[]).await;
    let (_, ship_ball) = sys.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    reposition_stationary_ball(&sys, ship_id, ship_ball.position).await;
    let base_proj = assert_actor_matches_fresh_fit(&ctx, &sys, char_id, ship_id, "after_undock").await;

    // Anchor Mobile Depot in space to unlock in-space fitting/unfitting.
    rpc_ok(&ctx, &mut sess, "posMgr", None, "AnchorStructure", &[
        PyRep::Integer(depot_id),
        PyRep::Tuple(vec![
            PyRep::Real(ship_ball.position.x + 350.0),
            PyRep::Real(ship_ball.position.y),
            PyRep::Real(ship_ball.position.z),
        ]),
    ]).await;
    ctx.advance_secs(65);
    sys.step_manual(65.0).await.unwrap();

    // 1. Fit Heat Sink II and Multispectrum Shield Hardener II in space near Mobile Depot.
    rpc_ok(&ctx, &mut sess, "invbroker", None, "Add", &[
        PyRep::Integer(heatsink_id),
        PyRep::Integer(ship_id),
        PyRep::Integer(InventoryFlag::LoSlot0 as i64),
    ]).await;
    let proj_after_hs = assert_actor_matches_fresh_fit(&ctx, &sys, char_id, ship_id, "fit_heatsink_in_space").await;
    assert!(proj_after_hs.modules[&laser_id].damage_multiplier > base_proj.modules[&laser_id].damage_multiplier);
    assert!(proj_after_hs.modules[&laser_id].duration_ms < base_proj.modules[&laser_id].duration_ms);

    rpc_ok(&ctx, &mut sess, "invbroker", None, "Add", &[
        PyRep::Integer(hardener_id),
        PyRep::Integer(ship_id),
        PyRep::Integer(InventoryFlag::MedSlot1 as i64),
    ]).await;
    assert_actor_matches_fresh_fit(&ctx, &sys, char_id, ship_id, "fit_hardener_in_space").await;

    // 2. Offline / online / unfit module in space.
    rpc_ok(&ctx, &mut sess, "dogmaIM", None, "TakeModuleOffline", &[
        PyRep::Integer(ship_id),
        PyRep::Integer(heatsink_id),
    ]).await;
    let proj_offline_hs = assert_actor_matches_fresh_fit(&ctx, &sys, char_id, ship_id, "offline_heatsink_in_space").await;
    assert_close(
        proj_offline_hs.modules[&laser_id].damage_multiplier,
        base_proj.modules[&laser_id].damage_multiplier,
        1e-9,
        "offlining Heat Sink must remove damage bonus",
    );

    rpc_ok(&ctx, &mut sess, "dogmaIM", None, "SetModuleOnline", &[
        PyRep::Integer(ship_id),
        PyRep::Integer(heatsink_id),
    ]).await;
    assert_actor_matches_fresh_fit(&ctx, &sys, char_id, ship_id, "online_heatsink_in_space").await;

    rpc_ok(&ctx, &mut sess, "invbroker", None, "Add", &[
        PyRep::Integer(heatsink_id),
        PyRep::Integer(ship_id),
        PyRep::Integer(InventoryFlag::Cargo as i64),
    ]).await;
    let proj_unfit = assert_actor_matches_fresh_fit(&ctx, &sys, char_id, ship_id, "unfit_heatsink_in_space").await;
    assert!(!proj_unfit.modules.contains_key(&heatsink_id));
    assert_close(
        proj_unfit.modules[&laser_id].damage_multiplier,
        base_proj.modules[&laser_id].damage_multiplier,
        1e-9,
        "unfitting Heat Sink must remove damage bonus",
    );

    // 3. Load / swap / unload charges in space.
    rpc_ok(&ctx, &mut sess, "dogmaIM", None, "LoadAmmo", &[
        PyRep::Integer(ship_id),
        PyRep::List(vec![PyRep::Integer(laser_id)]),
        PyRep::List(vec![PyRep::Integer(mf_ammo_id)]),
    ]).await;
    let proj_mf = assert_actor_matches_fresh_fit(&ctx, &sys, char_id, ship_id, "load_multifrequency").await;
    assert_eq!(proj_mf.modules[&laser_id].charge_type_id, Some(MULTIFREQUENCY_S));
    assert!(proj_mf.modules[&laser_id].damage[0] > 0.0);
    assert!(proj_mf.modules[&laser_id].optimal_range_m < base_proj.modules[&laser_id].optimal_range_m);

    rpc_ok(&ctx, &mut sess, "dogmaIM", None, "LoadAmmo", &[
        PyRep::Integer(ship_id),
        PyRep::List(vec![PyRep::Integer(laser_id)]),
        PyRep::List(vec![PyRep::Integer(scorch_ammo_id)]),
    ]).await;
    let proj_scorch = assert_actor_matches_fresh_fit(&ctx, &sys, char_id, ship_id, "swap_to_scorch").await;
    assert_eq!(proj_scorch.modules[&laser_id].charge_type_id, Some(SCORCH_S));
    assert!(proj_scorch.modules[&laser_id].optimal_range_m > proj_mf.modules[&laser_id].optimal_range_m);

    let loaded_mf_id = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .find(|i| {
                i.location_id == ship_id
                    && i.flag == InventoryFlag::HiSlot0
                    && i.type_id == MULTIFREQUENCY_S
            })
            .map(|i| i.item_id)
    });
    assert!(loaded_mf_id.is_none());

    rpc_ok(&ctx, &mut sess, "dogmaIM", None, "UnloadAmmo", &[
        PyRep::Integer(ship_id),
        PyRep::List(vec![PyRep::Integer(laser_id)]),
        PyRep::Tuple(vec![PyRep::Integer(ship_id), PyRep::Integer(InventoryFlag::Cargo as i64)]),
    ]).await;
    let proj_unloaded = assert_actor_matches_fresh_fit(&ctx, &sys, char_id, ship_id, "unload_ammo").await;
    assert_eq!(proj_unloaded.modules[&laser_id].charge_type_id, None);
    assert_close(proj_unloaded.modules[&laser_id].damage[0], 0.0, 1e-9, "unloaded turret has 0 base EM dmg");

    // Re-load Multifrequency S for stance and overload checks.
    let reloaded_mf_id = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .find(|i| i.location_id == ship_id && i.flag == InventoryFlag::Cargo && i.type_id == MULTIFREQUENCY_S)
            .map(|i| i.item_id)
            .expect("Multifrequency S in cargo")
    });
    rpc_ok(&ctx, &mut sess, "dogmaIM", None, "LoadAmmo", &[
        PyRep::Integer(ship_id),
        PyRep::List(vec![PyRep::Integer(laser_id)]),
        PyRep::List(vec![PyRep::Integer(reloaded_mf_id)]),
    ]).await;
    assert_actor_matches_fresh_fit(&ctx, &sys, char_id, ship_id, "reload_multifrequency").await;

    // 4. Stance switches in space (Sharpshooter -> Defense -> Propulsion).
    rpc_ok(&ctx, &mut sess, "shipStanceMgr", None, "SetShipStance", &[
        PyRep::Integer(ship_id),
        PyRep::Integer(STANCE_SHARPSHOOTER),
    ]).await;
    let proj_sharp = assert_actor_matches_fresh_fit(&ctx, &sys, char_id, ship_id, "stance_sharpshooter").await;
    assert_eq!(proj_sharp.active_stance_type_id, Some(STANCE_SHARPSHOOTER as u32));
    assert!(proj_sharp.baseline.max_target_range > base_proj.baseline.max_target_range);
    assert!(proj_sharp.modules[&laser_id].optimal_range_m > proj_mf.modules[&laser_id].optimal_range_m);

    ctx.advance_secs(12);
    rpc_ok(&ctx, &mut sess, "shipStanceMgr", None, "SetShipStance", &[
        PyRep::Integer(ship_id),
        PyRep::Integer(STANCE_DEFENSE),
    ]).await;
    let proj_def = assert_actor_matches_fresh_fit(&ctx, &sys, char_id, ship_id, "stance_defense").await;
    assert_eq!(proj_def.active_stance_type_id, Some(STANCE_DEFENSE as u32));
    assert!(proj_def.baseline.armor_resonance[0] < proj_sharp.baseline.armor_resonance[0]);

    ctx.advance_secs(12);
    rpc_ok(&ctx, &mut sess, "shipStanceMgr", None, "SetShipStance", &[
        PyRep::Integer(ship_id),
        PyRep::Integer(STANCE_PROPULSION),
    ]).await;
    let proj_prop_stance = assert_actor_matches_fresh_fit(&ctx, &sys, char_id, ship_id, "stance_propulsion").await;
    assert!(proj_prop_stance.baseline.agility < proj_def.baseline.agility);

    // 5. Module activation + overload toggles in space.
    rpc_ok(&ctx, &mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(hardener_id)]).await;
    let proj_hardener_on = assert_actor_matches_fresh_fit(&ctx, &sys, char_id, ship_id, "activate_hardener").await;
    assert!(proj_hardener_on.baseline.shield_resonance[0] < proj_prop_stance.baseline.shield_resonance[0]);

    rpc_ok(&ctx, &mut sess, "dogmaIM", None, "Overload", &[PyRep::Integer(ab_id)]).await;
    assert_actor_matches_fresh_fit(&ctx, &sys, char_id, ship_id, "pre_overload_ab").await;
    rpc_ok(&ctx, &mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(ab_id)]).await;
    let proj_ov_ab = assert_actor_matches_fresh_fit(&ctx, &sys, char_id, ship_id, "overload_ab").await;
    assert!(proj_ov_ab.propulsion.as_ref().is_some_and(|p| p.is_overloaded));

    rpc_ok(&ctx, &mut sess, "dogmaIM", None, "Overload", &[PyRep::Integer(hardener_id)]).await;
    for _ in 0..12 {
        ctx.advance_secs(1);
        sys.step_manual(1.0).await.unwrap();
    }
    let proj_ov_hard = assert_actor_matches_fresh_fit(&ctx, &sys, char_id, ship_id, "overload_hardener").await;
    assert!(proj_ov_hard.modules[&hardener_id].is_overloaded);
    assert!(proj_ov_hard.baseline.shield_resonance[0] < proj_hardener_on.baseline.shield_resonance[0]);

    rpc_ok(&ctx, &mut sess, "dogmaIM", None, "StopOverload", &[PyRep::Integer(ab_id)]).await;
    rpc_ok(&ctx, &mut sess, "dogmaIM", None, "StopOverload", &[PyRep::Integer(hardener_id)]).await;
    for _ in 0..12 {
        ctx.advance_secs(1);
        sys.step_manual(1.0).await.unwrap();
    }
    let proj_stop_ov = assert_actor_matches_fresh_fit(&ctx, &sys, char_id, ship_id, "stop_overload").await;
    assert!(!proj_stop_ov.modules[&hardener_id].is_overloaded);
    assert!(proj_stop_ov.propulsion.as_ref().is_some_and(|p| !p.is_overloaded));

    rpc_ok(&ctx, &mut sess, "dogmaIM", None, "Deactivate", &[PyRep::Integer(ab_id)]).await;
    rpc_ok(&ctx, &mut sess, "dogmaIM", None, "Deactivate", &[PyRep::Integer(hardener_id)]).await;
    for _ in 0..12 {
        ctx.advance_secs(1);
        sys.step_manual(1.0).await.unwrap();
    }
    let proj_deact = assert_actor_matches_fresh_fit(&ctx, &sys, char_id, ship_id, "deactivate_ab_hardener").await;
    assert!(proj_deact.propulsion.is_none());

    // 6. Skill training completion while flying in space (Target Management I -> II).
    assert_eq!(proj_deact.baseline.max_locked_targets, 3);
    // Keep a subsequent skill entry in the queue so `CheckAndSendNotifications` does not
    // leave an empty `skill_queues[char_id] = []` vector mismatched with SQLite reload (`PRE`).
    rpc_ok(&ctx, &mut sess, "skillMgr2", None, "SaveNewQueue", &[
        PyRep::List(vec![
            PyRep::Tuple(vec![PyRep::Integer(3429), PyRep::Integer(2)]),
            PyRep::Tuple(vec![PyRep::Integer(3429), PyRep::Integer(3)]),
        ]),
    ]).await;
    ctx.advance_secs(4_000);
    rpc_ok(&ctx, &mut sess, "skillMgr2", None, "CheckAndSendNotifications", &[]).await;
    assert_eq!(ctx.character_skill_levels(char_id).get(&3429).copied(), Some(2));
    let proj_skill = assert_actor_matches_fresh_fit(&ctx, &sys, char_id, ship_id, "skill_completed_in_space").await;
    assert_eq!(proj_skill.baseline.max_locked_targets, 4);

    // 7. Booster consumption and implant plug/destroy while in space.
    let tracking_before_booster = proj_skill.modules[&laser_id].tracking_speed;
    rpc_ok(&ctx, &mut sess, "dogmaIM", None, "UseBooster", &[PyRep::Integer(booster_id)]).await;
    let proj_booster = assert_actor_matches_fresh_fit(&ctx, &sys, char_id, ship_id, "use_booster_in_space").await;
    assert!(proj_booster.modules[&laser_id].tracking_speed > tracking_before_booster);

    let vel_before_implant = proj_booster.baseline.max_velocity;
    rpc_ok(&ctx, &mut sess, "dogmaIM", None, "InjectImplant", &[PyRep::Integer(implant_id)]).await;
    let proj_implant = assert_actor_matches_fresh_fit(&ctx, &sys, char_id, ship_id, "inject_implant_in_space").await;
    assert!(proj_implant.baseline.max_velocity > vel_before_implant);

    rpc_ok(&ctx, &mut sess, "dogmaIM", None, "DestroyImplant", &[PyRep::Integer(implant_id)]).await;
    let proj_no_implant = assert_actor_matches_fresh_fit(&ctx, &sys, char_id, ship_id, "destroy_implant_in_space").await;
    assert_close(
        proj_no_implant.baseline.max_velocity,
        vel_before_implant,
        1e-6,
        "destroying implant restores pre-implant max_velocity",
    );
}

#[tokio::test]
async fn fit_projection_actor_parity_active_hardener_no_skill_queue_and_deactivate() {
    // Verifies #922 criterion 2: active shield and armor hardeners reduce incoming EM damage
    // for a pilot with NO skill queue, and deactivating restores base resists and incoming damage.
    let ctx = ServerContext::new().with_seeded_rng(find_seed_under(0.9));
    let _guard = common::teardown_guard(&ctx);
    common::pin_clock(&ctx);

    let atk_char = 90_000_001_i64;
    let atk_ship = 922_200_001_i64;
    let def_char = 90_000_002_i64;
    let def_ship = 922_200_002_i64;

    grant_required_skills_recursive(
        &ctx,
        atk_char,
        &[KESTREL, LIGHT_MISSILE_LAUNCHER_II, MJOLNIR_LIGHT_MISSILE],
    );
    grant_required_skills_recursive(
        &ctx,
        def_char,
        &[RAVEN, EM_SHIELD_HARDENER_II, EM_ARMOR_HARDENER_II],
    );
    set_skills(&ctx, def_char, &[(3426, 5), (3413, 5), (3418, 5), (3417, 5), (3420, 5), (3394, 5)]);

    // Ensure the defender has NO skill queue whatsoever.
    ctx.db.with_mut_snapshot(|snap| {
        snap.skill_queues.remove(&def_char);
    });
    assert!(
        ctx.db.with_snapshot(|snap| snap.skill_queues.get(&def_char).is_none_or(|q| q.is_empty())),
        "defender must have no skill queue"
    );

    let mut atk_sess = login_and_board_new_ship(
        &ctx,
        922_201,
        "demo",
        "demo-secret",
        atk_char,
        NULL_SYS,
        NULL_STATION,
        atk_ship,
        KESTREL,
    )
    .await;
    let mut def_sess = login_and_board_new_ship(
        &ctx,
        922_202,
        "other",
        "other-secret",
        def_char,
        NULL_SYS,
        NULL_STATION,
        def_ship,
        RAVEN,
    )
    .await;

    let launcher_id = 922_200_010_i64;
    let ammo_id = 922_200_011_i64;
    let shield_hardener_id = 922_200_020_i64;
    let armor_hardener_id = 922_200_021_i64;

    seed_item(&ctx, launcher_id, LIGHT_MISSILE_LAUNCHER_II, atk_char, NULL_STATION, InventoryFlag::Hangar, 1);
    seed_item(&ctx, ammo_id, MJOLNIR_LIGHT_MISSILE, atk_char, atk_ship, InventoryFlag::Cargo, 20);
    seed_item(&ctx, shield_hardener_id, EM_SHIELD_HARDENER_II, def_char, NULL_STATION, InventoryFlag::Hangar, 1);
    seed_item(&ctx, armor_hardener_id, EM_ARMOR_HARDENER_II, def_char, NULL_STATION, InventoryFlag::Hangar, 1);

    rpc_ok(&ctx, &mut atk_sess, "invbroker", None, "Add", &[
        PyRep::Integer(launcher_id),
        PyRep::Integer(atk_ship),
        PyRep::Integer(InventoryFlag::HiSlot0 as i64),
    ]).await;
    rpc_ok(&ctx, &mut atk_sess, "dogmaIM", None, "LoadAmmo", &[
        PyRep::Integer(atk_ship),
        PyRep::List(vec![PyRep::Integer(launcher_id)]),
        PyRep::List(vec![PyRep::Integer(ammo_id)]),
    ]).await;
    rpc_ok(&ctx, &mut def_sess, "invbroker", None, "Add", &[
        PyRep::Integer(shield_hardener_id),
        PyRep::Integer(def_ship),
        PyRep::Integer(InventoryFlag::MedSlot0 as i64),
    ]).await;
    rpc_ok(&ctx, &mut def_sess, "invbroker", None, "Add", &[
        PyRep::Integer(armor_hardener_id),
        PyRep::Integer(def_ship),
        PyRep::Integer(InventoryFlag::LoSlot0 as i64),
    ]).await;

    let (sys, _) = undock_and_settle(&ctx, &mut atk_sess, NULL_SYS).await;
    let _ = undock_and_settle(&ctx, &mut def_sess, NULL_SYS).await;

    reposition_stationary_ball(&sys, atk_ship, DVec3::new(0.0, 0.0, 0.0)).await;
    reposition_stationary_ball(&sys, def_ship, DVec3::new(2_500.0, 0.0, 0.0)).await;
    lock_target(&ctx, &mut atk_sess, &sys, atk_ship, def_ship).await;

    let base_cs = sys.get_combat_state(def_ship).await.unwrap().unwrap();
    let base_shield_em_res = base_cs.shield_resonance[0];
    let base_armor_em_res = base_cs.armor_resonance[0];
    let full_shield = base_cs.max_shield_hp;
    let full_armor = base_cs.max_armor_hp;

    // Fire a deterministic 100% EM light missile (Mjolnir, zero turret roll variance) and measure
    // exact shield or armor damage dealt upon impact.
    #[allow(clippy::too_many_arguments)]
    async fn fire_single_missile_and_measure_dmg(
        ctx: &ServerContext,
        sys: &SystemHandle,
        atk_sess: &mut SessionState,
        launcher_id: i64,
        def_char: i64,
        def_ship: i64,
        test_armor: bool,
        full_shield: f64,
        full_armor: f64,
    ) -> (f64, bool) {
        let mut cs = sys.get_combat_state(def_ship).await.unwrap().unwrap();
        cs.shield_hp = if test_armor { 0.0 } else { full_shield };
        cs.armor_hp = full_armor;
        sys.set_combat_state(def_ship, cs).await.unwrap();
        let _ = ctx.drain_notifications_for_char(def_char);
        rpc_ok(ctx, atk_sess, "dogmaIM", None, "Activate", &[
            PyRep::Integer(launcher_id),
            PyRep::String("useMissiles".into()),
            PyRep::Integer(def_ship),
            PyRep::Integer(0),
        ]).await;
        let mut dmg = 0.0;
        let mut has_dsc = false;
        let mut ticks_stepped = 0;
        for _ in 0..8 {
            ctx.advance_secs(1);
            sys.step_manual(1.0).await.unwrap();
            ticks_stepped += 1;
            let notifs = ctx.drain_notifications_for_char(def_char);
            if notifs.iter().any(|n| n.event == "OnDamageStateChange") {
                has_dsc = true;
            }
            let cs_now = sys.get_combat_state(def_ship).await.unwrap().unwrap();
            let cur_dmg = if test_armor {
                full_armor - cs_now.armor_hp
            } else {
                full_shield - cs_now.shield_hp
            };
            if cur_dmg > 1.0 {
                dmg = cur_dmg;
                break;
            }
        }
        for _ in ticks_stepped..15 {
            ctx.advance_secs(1);
            sys.step_manual(1.0).await.unwrap();
        }
        (dmg, has_dsc)
    }

    // 1. Base shield shot (hardener OFF).
    let (base_shield_dmg, has_dsc_base) = fire_single_missile_and_measure_dmg(
        &ctx, &sys, &mut atk_sess, launcher_id, def_char, def_ship, false, full_shield, full_armor,
    ).await;
    assert!(base_shield_dmg > 1.0, "base missile hit must deal shield damage, got {base_shield_dmg}");
    assert!(has_dsc_base, "defender must receive OnDamageStateChange on hit");

    // 2. Activate EM Shield Hardener II with NO skill queue -> EM resonance drops by 55% and damage drops by 55%.
    rpc_ok(&ctx, &mut def_sess, "dogmaIM", None, "Activate", &[
        PyRep::Integer(shield_hardener_id),
    ]).await;
    let hard_cs = sys.get_combat_state(def_ship).await.unwrap().unwrap();
    assert_close(
        hard_cs.shield_resonance[0],
        base_shield_em_res * 0.45,
        1e-9,
        "active EM Shield Hardener II must multiply EM resonance by 0.45",
    );
    assert_actor_matches_fresh_fit(&ctx, &sys, def_char, def_ship, "active_shield_hardener").await;

    let (hardened_shield_dmg, has_dsc_hard) = fire_single_missile_and_measure_dmg(
        &ctx, &sys, &mut atk_sess, launcher_id, def_char, def_ship, false, full_shield, full_armor,
    ).await;
    assert_close(
        hardened_shield_dmg / base_shield_dmg,
        0.45,
        0.02,
        "active EM shield hardener must reduce pure-EM incoming damage proportionally to 0.45x resonance",
    );
    assert!(has_dsc_hard, "defender must receive OnDamageStateChange with hardener active");

    // 3. Deactivate EM Shield Hardener II -> base resists and incoming damage restored immediately.
    rpc_ok(&ctx, &mut def_sess, "dogmaIM", None, "Deactivate", &[
        PyRep::Integer(shield_hardener_id),
    ]).await;
    let deact_cs = sys.get_combat_state(def_ship).await.unwrap().unwrap();
    assert_close(
        deact_cs.shield_resonance[0],
        base_shield_em_res,
        1e-9,
        "deactivating EM Shield Hardener II must restore base EM resonance",
    );
    assert_actor_matches_fresh_fit(&ctx, &sys, def_char, def_ship, "deactivated_shield_hardener").await;

    let (restored_shield_dmg, _) = fire_single_missile_and_measure_dmg(
        &ctx, &sys, &mut atk_sess, launcher_id, def_char, def_ship, false, full_shield, full_armor,
    ).await;
    assert_close(
        restored_shield_dmg,
        base_shield_dmg,
        0.05,
        "turning off shield hardener must restore full base damage",
    );

    // 4. Repeat on armor (zero shield -> hits apply directly to armor): active EM Armor Hardener II & off.
    let (base_armor_dmg, has_dsc_armor_base) = fire_single_missile_and_measure_dmg(
        &ctx, &sys, &mut atk_sess, launcher_id, def_char, def_ship, true, full_shield, full_armor,
    ).await;
    assert!(base_armor_dmg > 1.0, "base missile hit on stripped shield must damage armor, got {base_armor_dmg}");
    assert!(has_dsc_armor_base, "defender must receive OnDamageStateChange on armor hit");

    rpc_ok(&ctx, &mut def_sess, "dogmaIM", None, "Activate", &[
        PyRep::Integer(armor_hardener_id),
    ]).await;
    let armor_hard_cs = sys.get_combat_state(def_ship).await.unwrap().unwrap();
    assert_close(
        armor_hard_cs.armor_resonance[0],
        base_armor_em_res * 0.45,
        1e-9,
        "active EM Armor Hardener II must multiply armor EM resonance by 0.45",
    );
    let (hardened_armor_dmg, has_dsc_armor_hard) = fire_single_missile_and_measure_dmg(
        &ctx, &sys, &mut atk_sess, launcher_id, def_char, def_ship, true, full_shield, full_armor,
    ).await;
    assert_close(
        hardened_armor_dmg / base_armor_dmg,
        0.45,
        0.02,
        "active EM armor hardener must reduce pure-EM incoming armor damage proportionally to 0.45x resonance",
    );
    assert!(has_dsc_armor_hard, "defender must receive OnDamageStateChange with armor hardener active");

    rpc_ok(&ctx, &mut def_sess, "dogmaIM", None, "Deactivate", &[
        PyRep::Integer(armor_hardener_id),
    ]).await;
    let armor_restored_cs = sys.get_combat_state(def_ship).await.unwrap().unwrap();
    assert_close(
        armor_restored_cs.armor_resonance[0],
        base_armor_em_res,
        1e-9,
        "deactivating EM Armor Hardener II must restore base armor EM resonance",
    );
    let (restored_armor_dmg, _) = fire_single_missile_and_measure_dmg(
        &ctx, &sys, &mut atk_sess, launcher_id, def_char, def_ship, true, full_shield, full_armor,
    ).await;
    assert_close(
        restored_armor_dmg,
        base_armor_dmg,
        0.05,
        "deactivating armor hardener must restore full base armor damage",
    );
}

#[tokio::test]
async fn fit_projection_actor_parity_ecm_jam_and_tracking_disruption_next_cycle() {
    // Verifies #922 criterion 3:
    // Part A: Mid-fight ECM jam drops non-jammer target lock and stops active turret cycle.
    // Part B: Mid-fight Tracking Disruption changes FitProjection immediately and takes effect on
    // the NEXT weapon cycle (not mid-cycle), and deactivating TD restores on the following cycle.
    let ecm_hit_seed = find_seed_under(0.15);
    let ctx = ServerContext::new().with_seeded_rng(ecm_hit_seed);
    let _guard = common::teardown_guard(&ctx);
    common::pin_clock(&ctx);

    let atk_char = 90_000_001_i64;
    let atk_ship = 922_300_001_i64;
    let ewar_char = 90_000_002_i64;
    let ewar_ship = 922_300_002_i64;
    let ally_target_id = 922_300_003_i64;

    grant_required_skills_recursive(
        &ctx,
        atk_char,
        &[RIFTER, SMALL_FOCUSED_PULSE_LASER_II, MULTIFREQUENCY_S],
    );
    set_skills(
        &ctx,
        atk_char,
        &[
            (3300, 5), // Gunnery V
            (3303, 5), // Small Energy Turret V
            (3310, 5), // Sharpshooter V
            (3311, 5), // Rapid Firing V
            (3312, 5), // Trajectory Analysis V
            (3426, 5),
            (3413, 5),
            (3417, 5),
            (3418, 5),
            (3428, 5), // Long Range Targeting V
        ],
    );
    grant_required_skills_recursive(
        &ctx,
        ewar_char,
        &[SCORPION, CRUCIFIER, MULTISPECTRAL_ECM_II, TRACKING_DISRUPTOR_II, OPTIMAL_RANGE_DISRUPTION_SCRIPT, TRACKING_SPEED_DISRUPTION_SCRIPT],
    );
    set_skills(
        &ctx,
        ewar_char,
        &[
            (3338, 5), // Caldari Battleship V (+15%/lvl ECM on Scorpion)
            (3331, 5), // Amarr Frigate V (+7.5%/lvl TD on Crucifier)
            (3426, 5),
            (3413, 5),
            (3417, 5),
            (3418, 5),
            (3427, 5), // Electronic Warfare V
            (3428, 5), // Long Range Targeting V
            (3434, 5), // Weapon Disruption V
            (19_759, 5), // Turret Destabilization V (+5%/lvl TD)
        ],
    );

    // --- Part A: Mid-fight ECM jam breaks target lock on ally & stops turret shooting ---
    let mut atk_sess = login_and_board_new_ship(
        &ctx,
        922_301,
        "demo",
        "demo-secret",
        atk_char,
        NULL_SYS,
        NULL_STATION,
        atk_ship,
        RIFTER,
    )
    .await;
    let mut ecm_sess = login_and_board_new_ship(
        &ctx,
        922_302,
        "other",
        "other-secret",
        ewar_char,
        NULL_SYS,
        NULL_STATION,
        ewar_ship,
        SCORPION,
    )
    .await;

    let laser_id = 922_300_010_i64;
    let ammo_id = 922_300_011_i64;
    let ecm_id = 922_300_020_i64;
    seed_item(&ctx, laser_id, SMALL_FOCUSED_PULSE_LASER_II, atk_char, NULL_STATION, InventoryFlag::Hangar, 1);
    seed_item(&ctx, ammo_id, MULTIFREQUENCY_S, atk_char, atk_ship, InventoryFlag::Cargo, 2);
    seed_item(&ctx, ecm_id, MULTISPECTRAL_ECM_II, ewar_char, NULL_STATION, InventoryFlag::Hangar, 1);

    rpc_ok(&ctx, &mut atk_sess, "invbroker", None, "Add", &[
        PyRep::Integer(laser_id),
        PyRep::Integer(atk_ship),
        PyRep::Integer(InventoryFlag::HiSlot0 as i64),
    ]).await;
    rpc_ok(&ctx, &mut atk_sess, "dogmaIM", None, "LoadAmmo", &[
        PyRep::Integer(atk_ship),
        PyRep::List(vec![PyRep::Integer(laser_id)]),
        PyRep::List(vec![PyRep::Integer(ammo_id)]),
    ]).await;
    rpc_ok(&ctx, &mut ecm_sess, "invbroker", None, "Add", &[
        PyRep::Integer(ecm_id),
        PyRep::Integer(ewar_ship),
        PyRep::Integer(InventoryFlag::MedSlot0 as i64),
    ]).await;

    let (sys, _) = undock_and_settle(&ctx, &mut atk_sess, NULL_SYS).await;
    let _ = undock_and_settle(&ctx, &mut ecm_sess, NULL_SYS).await;

    reposition_stationary_ball(&sys, atk_ship, DVec3::new(0.0, 0.0, 0.0)).await;
    reposition_stationary_ball(&sys, ewar_ship, DVec3::new(5_000.0, 0.0, 0.0)).await;
    // Spawn stationary target ship (Moa cruiser) within laser range for shooter to shoot.
    let mut ally_cs = ctx.build_type_combat_state(623);
    ally_cs.undock_invulnerable_remaining_ticks = 0;
    sys.spawn_ball_with_combat(
        Ball {
            id: ally_target_id,
            type_id: 623,
            position: DVec3::new(2_500.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 200.0,
            inertia: 3.0,
            mass: 12_000_000.0,
            radius: 130.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        ally_cs,
    )
    .await
    .unwrap();

    lock_target(&ctx, &mut atk_sess, &sys, atk_ship, ally_target_id).await;
    lock_target(&ctx, &mut ecm_sess, &sys, ewar_ship, atk_ship).await;

    let ally_shield_initial = sys.get_combat_state(ally_target_id).await.unwrap().unwrap().shield_hp;
    rpc_ok(&ctx, &mut atk_sess, "dogmaIM", None, "Activate", &[
        PyRep::Integer(laser_id),
        PyRep::String("targetHostile".into()),
        PyRep::Integer(ally_target_id),
        PyRep::Integer(1000),
    ]).await;
    assert!(sys.has_module_cycle(atk_ship, laser_id).await);
    let ally_shield_after_shot = sys.get_combat_state(ally_target_id).await.unwrap().unwrap().shield_hp;
    assert!(ally_shield_after_shot < ally_shield_initial, "shooter must damage ally before ECM");

    // Scorpion lands Multispectral ECM II mid-fight -> shooter loses ally lock & stops turret cycle.
    let _ = ctx.drain_notifications_for_char(atk_char);
    ctx.set_mechanic_rng_seed(Some(ecm_hit_seed));
    rpc_ok(&ctx, &mut ecm_sess, "dogmaIM", None, "Activate", &[
        PyRep::Integer(ecm_id),
        PyRep::String("targetHostile".into()),
        PyRep::Integer(atk_ship),
        PyRep::Integer(1),
    ]).await;

    let jammed_cs = sys.get_combat_state(atk_ship).await.unwrap().unwrap();
    assert!(jammed_cs.ecm_jammed_remaining_s > 0.0, "ECM must jam shooter");
    let atk_locks: Vec<i64> = sys
        .get_target_locks(atk_ship)
        .await
        .unwrap()
        .into_iter()
        .map(|l| l.target_id)
        .collect();
    assert!(
        !atk_locks.contains(&ally_target_id),
        "jammed shooter must lose non-jammer target lock on ally; locks={atk_locks:?}"
    );
    assert!(
        !sys.has_module_cycle(atk_ship, laser_id).await,
        "jammed shooter's turret cycle on unlocked target must deactivate"
    );
    let atk_notifs = ctx.drain_notifications_for_char(atk_char);
    assert!(
        atk_notifs.iter().any(|n| n.event == "OnJamStart"),
        "jammed shooter must receive OnJamStart"
    );
    let relock_err = rpc_harness_call(
        &ctx,
        &mut atk_sess,
        "dogmaIM",
        None,
        "AddTarget",
        &[PyRep::Integer(ally_target_id)],
    )
    .await
    .unwrap_err();
    assert!(!relock_err.code.is_empty(), "jammed shooter must not re-lock non-jammer target");

    // Advance several weapon cycles: ally must take zero further weapon damage.
    let ally_hp_at_jam = sys.get_combat_state(ally_target_id).await.unwrap().unwrap().shield_hp;
    for _ in 0..4 {
        sys.step_manual(3.5).await.unwrap();
    }
    let ally_hp_after_wait = sys.get_combat_state(ally_target_id).await.unwrap().unwrap().shield_hp;
    assert!(
        ally_hp_after_wait >= ally_hp_at_jam,
        "jammed ship must not deal turret damage across subsequent ticks: before={ally_hp_at_jam} after={ally_hp_after_wait}"
    );

    // Wait for ECM cycle + jam expiration before Part B.
    ctx.advance_secs(25);
    sys.step_manual(25.0).await.unwrap();

    // --- Part B: Tracking Disruption mid-fight takes effect on NEXT weapon cycle ---
    // Board Crucifier with 2x Tracking Disruptor II + Optimal Range Disruption Scripts for EWAR ship.
    let crucifier_ship = 922_300_030_i64;
    let td1_id = 922_300_031_i64;
    let td2_id = 922_300_032_i64;
    let script1_id = 922_300_033_i64;
    let script2_id = 922_300_034_i64;
    let track_script_id = 922_300_035_i64;

    // Dock ewar pilot cleanly in a second Crucifier? Even cleaner: spawn a fresh session on same ship? No, dock & re-undock!
    // Actually we can board & fit a Crucifier cleanly by putting ewar_char back at NULL_STATION via Docking or fresh Crucifier login:
    let mut td_sess = login_and_board_new_ship(
        &ctx,
        922_303,
        "other",
        "other-secret",
        ewar_char,
        NULL_SYS,
        NULL_STATION,
        crucifier_ship,
        CRUCIFIER,
    )
    .await;
    seed_item(&ctx, td1_id, TRACKING_DISRUPTOR_II, ewar_char, NULL_STATION, InventoryFlag::Hangar, 1);
    seed_item(&ctx, td2_id, TRACKING_DISRUPTOR_II, ewar_char, NULL_STATION, InventoryFlag::Hangar, 1);
    seed_item(&ctx, script1_id, OPTIMAL_RANGE_DISRUPTION_SCRIPT, ewar_char, crucifier_ship, InventoryFlag::Cargo, 1);
    seed_item(&ctx, script2_id, OPTIMAL_RANGE_DISRUPTION_SCRIPT, ewar_char, crucifier_ship, InventoryFlag::Cargo, 1);
    seed_item(&ctx, track_script_id, TRACKING_SPEED_DISRUPTION_SCRIPT, ewar_char, crucifier_ship, InventoryFlag::Cargo, 1);

    rpc_ok(&ctx, &mut td_sess, "invbroker", None, "Add", &[
        PyRep::Integer(td1_id),
        PyRep::Integer(crucifier_ship),
        PyRep::Integer(InventoryFlag::MedSlot0 as i64),
    ]).await;
    rpc_ok(&ctx, &mut td_sess, "invbroker", None, "Add", &[
        PyRep::Integer(td2_id),
        PyRep::Integer(crucifier_ship),
        PyRep::Integer(InventoryFlag::MedSlot1 as i64),
    ]).await;
    rpc_ok(&ctx, &mut td_sess, "dogmaIM", None, "LoadAmmo", &[
        PyRep::Integer(crucifier_ship),
        PyRep::List(vec![PyRep::Integer(td1_id)]),
        PyRep::List(vec![PyRep::Integer(script1_id)]),
    ]).await;
    rpc_ok(&ctx, &mut td_sess, "dogmaIM", None, "LoadAmmo", &[
        PyRep::Integer(crucifier_ship),
        PyRep::List(vec![PyRep::Integer(td2_id)]),
        PyRep::List(vec![PyRep::Integer(script2_id)]),
    ]).await;

    let _ = undock_and_settle(&ctx, &mut td_sess, NULL_SYS).await;
    // Place Crucifier at 3,850 m: inside Small Focused Pulse Laser II + Multifrequency S + Sharpshooter V optimal (3,937.5 m),
    // so Cycle 0 has P_hit = 1.0 (guaranteed hit!), while 2x TD II + Optimal Script drops optimal < 750 m & falloff < 600 m
    // -> P_hit < 1e-8 (guaranteed miss on Cycle 1!).
    reposition_stationary_ball(&sys, atk_ship, DVec3::new(0.0, 0.0, 0.0)).await;
    reposition_stationary_ball(&sys, crucifier_ship, DVec3::new(3_850.0, 0.0, 0.0)).await;

    lock_target(&ctx, &mut atk_sess, &sys, atk_ship, crucifier_ship).await;
    lock_target(&ctx, &mut td_sess, &sys, crucifier_ship, atk_ship).await;

    let base_proj = sys.latest_fit_projection(atk_ship).expect("atk FitProjection");
    let base_turret = &base_proj.modules[&laser_id];
    let cycle_dur_s = base_turret.duration_ms / 1000.0;
    assert!(
        base_turret.optimal_range_m > 3_850.0,
        "pre-TD optimal ({}) must cover 3,850 m",
        base_turret.optimal_range_m
    );

    // Fire Cycle 0 at t = 0 with baseline weapon stats (guaranteed hit inside optimal).
    let td_target_shield_0 = sys.get_combat_state(crucifier_ship).await.unwrap().unwrap().shield_hp;
    let _ = ctx.drain_notifications_for_char(atk_char);
    rpc_ok(&ctx, &mut atk_sess, "dogmaIM", None, "Activate", &[
        PyRep::Integer(laser_id),
        PyRep::String("targetHostile".into()),
        PyRep::Integer(crucifier_ship),
        PyRep::Integer(1000),
    ]).await;
    let td_target_shield_after_c0 = sys.get_combat_state(crucifier_ship).await.unwrap().unwrap().shield_hp;
    assert!(
        td_target_shield_after_c0 < td_target_shield_0,
        "Cycle 0 (pre-TD) inside optimal must hit: before={td_target_shield_0} after={td_target_shield_after_c0}"
    );
    let c0_notifs = ctx.drain_notifications_for_char(atk_char);
    assert!(c0_notifs.iter().any(|n| n.event == "OnDamageMessage"));

    // Mid-cycle (halfway through Cycle 0), Crucifier activates both Tracking Disruptor IIs on shooter.
    let half_cycle_s = cycle_dur_s * 0.4;
    sys.step_manual(half_cycle_s).await.unwrap();

    rpc_ok(&ctx, &mut td_sess, "dogmaIM", None, "Activate", &[
        PyRep::Integer(td1_id),
        PyRep::String("targetHostile".into()),
        PyRep::Integer(atk_ship),
        PyRep::Integer(10),
    ]).await;
    rpc_ok(&ctx, &mut td_sess, "dogmaIM", None, "Activate", &[
        PyRep::Integer(td2_id),
        PyRep::String("targetHostile".into()),
        PyRep::Integer(atk_ship),
        PyRep::Integer(10),
    ]).await;

    // FitProjection updates immediately with reduced optimal and falloff.
    let td_proj = assert_actor_matches_fresh_fit(&ctx, &sys, atk_char, atk_ship, "under_optimal_td").await;
    let td_turret = &td_proj.modules[&laser_id];
    assert!(
        td_turret.optimal_range_m < base_turret.optimal_range_m * 0.40,
        "2x TD II + Optimal Script must drop optimal dramatically: base={} td={}",
        base_turret.optimal_range_m,
        td_turret.optimal_range_m
    );
    assert!(
        td_turret.falloff_m < base_turret.falloff_m * 0.40,
        "2x TD II + Optimal Script must drop falloff dramatically: base={} td={}",
        base_turret.falloff_m,
        td_turret.falloff_m
    );

    // Step remaining time still BEFORE Cycle 0 completes: no spurious mid-cycle shot fires.
    let _ = ctx.drain_notifications_for_char(atk_char);
    sys.step_manual(half_cycle_s * 0.5).await.unwrap();
    let mid_cycle_notifs = ctx.drain_notifications_for_char(atk_char);
    assert!(
        !mid_cycle_notifs.iter().any(|n| n.event == "OnDamageMessage"),
        "TD applied mid-cycle must not trigger an immediate extra shot before next cycle boundary"
    );

    // Cross Cycle 0 -> Cycle 1 boundary: Cycle 1 syncs from FitProjection (with TD optimal/falloff) and misses!
    let shield_before_c1 = sys.get_combat_state(crucifier_ship).await.unwrap().unwrap().shield_hp;
    let _ = ctx.drain_notifications_for_char(atk_char);
    sys.step_manual(cycle_dur_s * 0.6 + 0.1).await.unwrap();
    let c1_notifs = ctx.drain_notifications_for_char(atk_char);
    assert!(
        c1_notifs.iter().any(|n| n.event == "OnDamageMessage"),
        "Cycle 1 must fire on cycle boundary under TD"
    );
    let shield_after_c1 = sys.get_combat_state(crucifier_ship).await.unwrap().unwrap().shield_hp;
    assert!(
        shield_after_c1 >= shield_before_c1,
        "Cycle 1 under 2x Optimal TD at 3,850 m must miss (no shield loss): before={shield_before_c1} after={shield_after_c1}"
    );
    assert!(
        sys.has_module_cycle(atk_ship, laser_id).await,
        "laser must remain cycling after TD miss"
    );

    // Swap script on td1 to Tracking Speed Disruption Script and verify tracking_speed changes in FitProjection!
    rpc_ok(&ctx, &mut td_sess, "dogmaIM", None, "Deactivate", &[PyRep::Integer(td1_id)]).await;
    rpc_ok(&ctx, &mut td_sess, "dogmaIM", None, "LoadAmmo", &[
        PyRep::Integer(crucifier_ship),
        PyRep::List(vec![PyRep::Integer(td1_id)]),
        PyRep::List(vec![PyRep::Integer(track_script_id)]),
    ]).await;
    ctx.advance_secs(2);
    rpc_ok(&ctx, &mut td_sess, "dogmaIM", None, "Activate", &[
        PyRep::Integer(td1_id),
        PyRep::String("targetHostile".into()),
        PyRep::Integer(atk_ship),
        PyRep::Integer(10),
    ]).await;
    let track_td_proj = assert_actor_matches_fresh_fit(&ctx, &sys, atk_char, atk_ship, "under_tracking_td").await;
    assert!(
        track_td_proj.modules[&laser_id].tracking_speed < base_turret.tracking_speed * 0.65,
        "Tracking Speed Disruption Script must reduce FitProjection tracking_speed: base={} td={}",
        base_turret.tracking_speed,
        track_td_proj.modules[&laser_id].tracking_speed
    );

    // Deactivate both TDs mid-Cycle-1 -> FitProjection immediately restores, and NEXT cycle (Cycle 2) hits again!
    rpc_ok(&ctx, &mut td_sess, "dogmaIM", None, "Deactivate", &[PyRep::Integer(td1_id)]).await;
    rpc_ok(&ctx, &mut td_sess, "dogmaIM", None, "Deactivate", &[PyRep::Integer(td2_id)]).await;
    let restored_proj = assert_actor_matches_fresh_fit(&ctx, &sys, atk_char, atk_ship, "after_td_deactivated").await;
    assert_close(restored_proj.modules[&laser_id].optimal_range_m, base_turret.optimal_range_m, 1e-6, "restored optimal");
    assert_close(restored_proj.modules[&laser_id].falloff_m, base_turret.falloff_m, 1e-6, "restored falloff");
    assert_close(restored_proj.modules[&laser_id].tracking_speed, base_turret.tracking_speed, 1e-9, "restored tracking");

    // Step across remaining Cycle 1 -> Cycle 2 boundary (enough time for 1 full cycle wrap): restored Cycle hits!
    let shield_before_c2 = sys.get_combat_state(crucifier_ship).await.unwrap().unwrap().shield_hp;
    let _ = ctx.drain_notifications_for_char(atk_char);
    sys.step_manual(cycle_dur_s + 0.2).await.unwrap();
    let shield_after_c2 = sys.get_combat_state(crucifier_ship).await.unwrap().unwrap().shield_hp;
    assert!(
        shield_after_c2 < shield_before_c2 - 0.1,
        "Cycle following TD deactivation must restore optimal and deal hit damage: before={shield_before_c2} after={shield_after_c2}"
    );
}

#[tokio::test]
async fn fit_projection_actor_parity_5mn_mwd_on_battleship_mass_scaled() {
    // Verifies #922 criterion 4: fitting and activating a 5MN Microwarpdrive on a battleship
    // yields a small velocity gain proportional to (speedFactor/100) * speedBoostFactor / (mass + massAddition),
    // NOT a massive frigate-sized speedup.
    let ctx = ServerContext::new();
    let _guard = common::teardown_guard(&ctx);
    common::pin_clock(&ctx);

    let char_id = 90_000_001_i64;
    let raven_id = 922_400_001_i64;
    let mwd_id = 922_400_010_i64;

    grant_required_skills_recursive(&ctx, char_id, &[RAVEN, FIVE_MN_MWD_I]);
    set_skills(
        &ctx,
        char_id,
        &[
            (3426, 5), // CPU Management V
            (3413, 5), // Power Grid Management V
            (3418, 5), // Capacitor Management V
            (3417, 5), // Capacitor Systems Operation V
            (3449, 5), // Navigation V
            (3454, 5), // High Speed Maneuvering V
            (3455, 5), // Acceleration Control V (+25% speedFactor)
        ],
    );

    let mut sess = login_and_board_new_ship(
        &ctx,
        922_401,
        "demo",
        "demo-secret",
        char_id,
        JITA_SYS,
        60_003_760,
        raven_id,
        RAVEN,
    )
    .await;
    seed_item(&ctx, mwd_id, FIVE_MN_MWD_I, char_id, 60_003_760, InventoryFlag::Hangar, 1);
    rpc_ok(&ctx, &mut sess, "invbroker", None, "Add", &[
        PyRep::Integer(mwd_id),
        PyRep::Integer(raven_id),
        PyRep::Integer(InventoryFlag::MedSlot0 as i64),
    ]).await;

    let (sys, beyonce_id) = undock_and_settle(&ctx, &mut sess, JITA_SYS).await;
    let base_proj = assert_actor_matches_fresh_fit(&ctx, &sys, char_id, raven_id, "raven_base_mwd_off").await;
    let base_vmax = base_proj.baseline.max_velocity;
    let base_mass = base_proj.baseline.mass;
    assert!(base_mass >= 99_000_000.0, "Raven mass must be battleship-scale (~99.3M kg)");

    // Command full speed forward and activate 5MN MWD I.
    rpc_ok(&ctx, &mut sess, "beyonce", Some(beyonce_id), "CmdGotoDirection", &[
        PyRep::Real(1.0),
        PyRep::Real(0.0),
        PyRep::Real(0.0),
    ]).await;
    rpc_ok(&ctx, &mut sess, "beyonce", Some(beyonce_id), "CmdSetSpeedFraction", &[
        PyRep::Real(1.0),
    ]).await;
    rpc_ok(&ctx, &mut sess, "dogmaIM", None, "Activate", &[
        PyRep::Integer(mwd_id),
    ]).await;

    let mwd_proj = assert_actor_matches_fresh_fit(&ctx, &sys, char_id, raven_id, "raven_5mn_mwd_active").await;
    let prop = mwd_proj.propulsion.expect("active 5MN MWD propulsion projection");
    let expected_boost = (prop.speed_factor / 100.0) * prop.speed_boost_factor / (base_mass + prop.mass_addition);
    assert_close(
        prop.speed_boost,
        expected_boost,
        1e-9,
        "5MN MWD on Raven must compute exact EVE thrust boost formula",
    );

    let (_, mwd_ball) = sys.get_ball_snapshot(raven_id).await.unwrap().unwrap();
    let expected_vmax = base_vmax * (1.0 + expected_boost);
    assert_close(
        mwd_ball.max_velocity,
        expected_vmax,
        1e-3,
        "Raven Ball::max_velocity with 5MN MWD must equal base_vmax * (1 + speed_boost)",
    );
    // Explicitly reject any unscaled +500% frigate-like velocity bug (~6x base_vmax).
    assert!(
        mwd_ball.max_velocity > base_vmax * 1.05 && mwd_ball.max_velocity < base_vmax * 1.20,
        "5MN MWD on 99.3M kg Raven must yield ~7-10% speedup (~{expected_vmax:.2}), NOT frigate-scale speedup: base={base_vmax:.2} active={:.2}",
        mwd_ball.max_velocity
    );

    // Accelerate in space across 25s (~2.5 MWD cycles so turning from the station undock vector
    // settles onto +X) and verify actual velocity exceeds base_vmax without overshooting cap.
    for _ in 0..25 {
        sys.step_manual(1.0).await.unwrap();
    }
    let (_, accel_ball) = sys.get_ball_snapshot(raven_id).await.unwrap().unwrap();
    let accel_speed = accel_ball.velocity.length();
    assert!(
        accel_speed > base_vmax && accel_speed <= expected_vmax + 1e-3,
        "accelerated velocity ({accel_speed:.4}) must exceed base_vmax ({base_vmax:.4}) without overshooting mass-scaled 5MN MWD cap ({expected_vmax:.4})"
    );

    // Deactivate 5MN MWD and verify base max_velocity and mass restore immediately.
    rpc_ok(&ctx, &mut sess, "dogmaIM", None, "Deactivate", &[
        PyRep::Integer(mwd_id),
    ]).await;
    let deact_proj = assert_actor_matches_fresh_fit(&ctx, &sys, char_id, raven_id, "raven_5mn_mwd_off").await;
    assert!(deact_proj.propulsion.is_none());
    let (_, off_ball) = sys.get_ball_snapshot(raven_id).await.unwrap().unwrap();
    assert_close(off_ball.max_velocity, base_vmax, 1e-3, "deactivating 5MN MWD restores base max_velocity");
    assert_close(off_ball.mass, base_mass, 1e-3, "deactivating 5MN MWD restores base hull mass");
}
