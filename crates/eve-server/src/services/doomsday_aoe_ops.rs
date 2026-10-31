//! `doomsday_aoe_ops` — Bosonic Field Generator (Cone), Spatial Phenomena Generator (GTFO),
//! and Reaper (MultiPoint) Titan Doomsday handlers (`feat:modules.doomsday-bosonic-field`,
//! `feat:modules.doomsday-gtfo-reaper`).

#![forbid(unsafe_code)]

use crate::router::{NotificationTarget, ServerContext};
use crate::services::tactical_modules::is_combatant_ship_or_entity_ball;
use eve_destiny::{DVec3, DamageVector};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::sync::{Mutex, OnceLock};

#[derive(Clone, Debug)]
struct BosonicActiveCycle {
    db_key: usize,
    sys_id: u32,
    char_id: i64,
    ship_id: i64,
    ship_type_id: u32,
    mod_type_id: u32,
    origin_pos: DVec3,
    dir_vec: DVec3,
    cone_len_m: f64,
    tick_dmg: DamageVector,
    started_at_unix: u64,
    warmup_secs: u64,
    total_ticks: usize,
    ticks_applied: usize,
    immobilize_until_unix: u64,
    orig_max_velocity: f64,
    orig_warp_scramble_strength: i32,
    orig_disallow_assistance: bool,
    mobility_restored: bool,
}

static ACTIVE_BOSONIC_CYCLES: OnceLock<Mutex<Vec<BosonicActiveCycle>>> = OnceLock::new();

fn active_bosonic_cycles() -> &'static Mutex<Vec<BosonicActiveCycle>> {
    ACTIVE_BOSONIC_CYCLES.get_or_init(|| Mutex::new(Vec::new()))
}

pub fn step_bosonic_doomsday_in_ballpark(
    ballpark: &mut eve_destiny::Ballpark,
    _sde_opt: Option<&std::sync::Arc<eve_sde::SdeRegistry>>,
    db_opt: Option<&std::sync::Arc<eve_db::WorldRepository>>,
    sys_id: u32,
    now_unix: u64,
    _destroyed_balls: &mut Vec<(i64, Option<i64>)>,
) {
    let db_key = db_opt.map(|d| d.instance_id() as usize).unwrap_or(0);
    let mut guard = active_bosonic_cycles()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    for cycle in guard.iter_mut() {
        if cycle.sys_id != sys_id || (db_key != 0 && cycle.db_key != db_key) {
            continue;
        }
        if now_unix >= cycle.immobilize_until_unix && !cycle.mobility_restored {
            if let Some(titan_ball) = ballpark.balls.get_mut(&cycle.ship_id) {
                titan_ball.max_velocity = cycle.orig_max_velocity;
            }
            if let Some(cs) = ballpark.combat_states.get_mut(&cycle.ship_id) {
                cs.warp_scramble_strength = cycle.orig_warp_scramble_strength;
                cs.disallow_assistance = cycle.orig_disallow_assistance;
            }
            cycle.mobility_restored = true;
        }
    }
    guard.retain(|c| !c.mobility_restored || c.ticks_applied < c.total_ticks);
}

fn apply_doomsday_capsuleer_crimewatch(
    ctx: &ServerContext,
    attacker_char_id: i64,
    attacker_ship_id: i64,
    sys_id: u32,
    victim_ball_id: i64,
    victim_type_id: u32,
    now_unix: u64,
) {
    let victim_char_opt = ctx.db.with_snapshot(|snap| {
        snap.characters
            .values()
            .find(|c| c.active_ship_id == victim_ball_id && c.character_id != attacker_char_id)
            .map(|c| c.character_id)
    });
    let Some(vcid) = victim_char_opt else {
        return;
    };
    {
        let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
        cw.get_or_create(attacker_char_id).pvp_timer_until_unix = now_unix.saturating_add(900);
        cw.get_or_create(vcid).pvp_timer_until_unix = now_unix.saturating_add(900);
    }
    let sys_sec = ctx
        .sde
        .solar_systems
        .get(&sys_id)
        .map(|s| s.security_status)
        .unwrap_or(0.0);
    if sys_sec <= 0.0 {
        return;
    }
    let v_suspect_or_crim = {
        let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
        let st = cw.get_or_create(vcid);
        st.is_suspect(now_unix) || st.is_criminal(now_unix)
    };
    let is_legal = ctx.db.with_snapshot(|snap| {
        crate::services::combat_crimewatch::is_legal_war_or_suspect_target(
            snap,
            attacker_char_id,
            vcid,
            now_unix,
        )
    }) || v_suspect_or_crim
        || crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
            rt.has_limited_engagement_with(vcid, attacker_char_id, now_unix)
        });
    if is_legal {
        return;
    }
    let is_pod = victim_type_id == 670;
    {
        let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
        cw.system_corruption_stages.insert(
            sys_id,
            crate::services::insurgency_services::insurgency_system_corruption_stage(ctx, sys_id),
        );
        let _ = cw.handle_combat_aggression_with_pod(
            attacker_char_id,
            vcid,
            attacker_ship_id,
            sys_id,
            sys_sec,
            false,
            is_pod,
            now_unix,
        );
        if sys_sec < 0.45 && is_pod {
            let st = cw.get_or_create(attacker_char_id);
            st.criminal_flag_until_unix = now_unix.saturating_add(900);
            st.suspect_flag_until_unix = 0;
        }
    }
    crate::services::combat_crimewatch::emit_crimewatch_timer_notifications(
        ctx,
        attacker_char_id,
        now_unix,
    );
    let old_sec = ctx.db.with_snapshot(|s| {
        s.characters
            .get(&attacker_char_id)
            .map(|c| c.security_status)
            .unwrap_or(0.0)
    });
    let loss = crate::combat::crimewatch::compute_sec_status_loss(old_sec, is_pod, sys_sec);
    let new_sec = (old_sec - loss).clamp(-10.0, 10.0);
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&attacker_char_id) {
            ch.security_status = new_sec;
        }
    });
}

/// Applies Bosonic Field Generator 1 Hz cone DoT ticks through the shared combat damage
/// and destruction pipeline (respecting tether, invulnerability, Upwell damage caps, and CrimeWatch).
pub async fn step_bosonic_doomsday_system(ctx: &ServerContext, sys_id: u32) {
    let db_key = ctx.instance_id as usize;
    let now_unix = ctx.now_unix_secs();
    let mut pending_batches: Vec<(usize, BosonicActiveCycle, usize)> = Vec::new();
    {
        let mut guard = active_bosonic_cycles()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        for (idx, cycle) in guard.iter_mut().enumerate() {
            if cycle.sys_id != sys_id || cycle.db_key != db_key {
                continue;
            }
            let elapsed = now_unix.saturating_sub(cycle.started_at_unix);
            if elapsed >= cycle.warmup_secs && cycle.ticks_applied < cycle.total_ticks {
                let target_ticks =
                    ((elapsed - cycle.warmup_secs + 1) as usize).min(cycle.total_ticks);
                let delta_ticks = target_ticks.saturating_sub(cycle.ticks_applied);
                if delta_ticks > 0 {
                    cycle.ticks_applied = target_ticks;
                    pending_batches.push((idx, cycle.clone(), delta_ticks));
                }
            }
        }
        guard.retain(|c| !c.mobility_restored || c.ticks_applied < c.total_ticks);
    }

    if pending_batches.is_empty() {
        return;
    }

    let sys = ctx.systems.get_or_spawn(sys_id);
    for (_, cycle, delta_ticks) in pending_batches {
        for _ in 0..delta_ticks {
            let balls = sys.list_balls().await.unwrap_or_default();
            for b in balls {
                if b.id == cycle.ship_id
                    || !is_combatant_ship_or_entity_ball(
                        Some(&ctx.sde),
                        Some(&ctx.db),
                        b.id,
                        b.type_id,
                    )
                {
                    continue;
                }
                let delta = b.position - cycle.origin_pos;
                let dist = delta.length();
                if dist <= 0.0
                    || dist > cycle.cone_len_m
                    || delta.normalize().dot(cycle.dir_vec) < BOSONIC_CONE_COS_HALF_ANGLE
                {
                    continue;
                }
                let (applied, destroyed) =
                    crate::services::station_structure::structure_lifecycle_rpc::apply_damage_structure_or_ball(
                        ctx,
                        &sys,
                        sys_id,
                        cycle.char_id,
                        b.id,
                        cycle.tick_dmg,
                        1.0,
                        now_unix,
                    )
                    .await
                    .ok()
                    .flatten()
                    .map(|(a, d, _)| (a, d))
                    .unwrap_or((0.0, false));
                if applied > 0.0 {
                    crate::services::combat_crimewatch::record_killmail_assist_action(
                        ctx,
                        b.id,
                        cycle.char_id,
                        cycle.ship_type_id,
                        cycle.mod_type_id,
                        applied,
                        false,
                        now_unix,
                    );
                    apply_doomsday_capsuleer_crimewatch(
                        ctx,
                        cycle.char_id,
                        cycle.ship_id,
                        sys_id,
                        b.id,
                        b.type_id,
                        now_unix,
                    );
                }
                if destroyed {
                    crate::services::combat_crimewatch::combat_missiles_aoe::handle_destroyed_ball_from_missile_or_aoe(
                        ctx,
                        sys_id,
                        cycle.char_id,
                        b.id,
                        b.type_id,
                        b.position,
                        now_unix,
                    )
                    .await;
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DoomsdayAoeMode {
    Cone,
    Spatial,
    MultiPoint,
}

pub async fn dispatch_superweapon_aoe_rpc(
    ctx: &ServerContext,
    session: &SessionState,
    mode: DoomsdayAoeMode,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let Some(ship_id) = session.ship_id else {
        return Err(BridgeRpcError {
            code: "NoActiveShip".into(),
            message: "Activating a superweapon requires an active Titan".into(),
        });
    };
    let Some(sys_id) = session.solar_system_id else {
        return Err(BridgeRpcError {
            code: "NotInSpace".into(),
            message: "Superweapons can only be activated in space".into(),
        });
    };
    let module_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
    match mode {
        DoomsdayAoeMode::Cone => {
            activate_cone_doomsday(ctx, char_id, ship_id, sys_id, module_id, args).await
        }
        DoomsdayAoeMode::Spatial => {
            activate_spatial_gtfo_doomsday(ctx, char_id, ship_id, sys_id, module_id, args).await
        }
        DoomsdayAoeMode::MultiPoint => {
            activate_multipoint_reaper_doomsday(ctx, char_id, ship_id, sys_id, module_id, args)
                .await
        }
    }
}

const DEFAULT_FUEL_COST: i32 = 50_000;
const BOSONIC_CONE_LENGTH_M: f64 = 30_000.0;
const BOSONIC_CONE_COS_HALF_ANGLE: f64 = 0.9659258262890683; // cos(15 deg)
const GTFO_SPHERE_RADIUS_M: f64 = 10_000.0;
const REAPER_BEAM_RADIUS_M: f64 = 5_000.0;

struct TitanDoomsdaySetup {
    sys: crate::system_actor::SystemHandle,
    all_balls: Vec<eve_destiny::Ball>,
    ship_pos: DVec3,
    ship_type_id: u32,
    mod_type_id: u32,
    dmg_vec: DamageVector,
    fuel_consumed: i32,
    max_range_m: f64,
    immob_secs: u64,
    no_jump_secs: u64,
    now_unix: u64,
    orig_max_velocity: f64,
    orig_warp_scramble_strength: i32,
    orig_disallow_assistance: bool,
}

fn validate_doomsday_target_points(
    ship_pos: DVec3,
    max_range_m: f64,
    points: &[DVec3],
) -> Result<(), BridgeRpcError> {
    for pt in points {
        if !pt.x.is_finite() || !pt.y.is_finite() || !pt.z.is_finite() {
            return Err(BridgeRpcError {
                code: "InvalidTargetPoint".into(),
                message: "Doomsday target coordinates must be finite".into(),
            });
        }
        if (*pt - ship_pos).length() > max_range_m {
            return Err(BridgeRpcError {
                code: "TargetOutOfRange".into(),
                message: format!("Doomsday target point exceeds max range ({max_range_m} m)"),
            });
        }
    }
    Ok(())
}

async fn validate_and_consume_titan_doomsday(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    sys_id: u32,
    module_id: i64,
    target_points: &[DVec3],
) -> Result<TitanDoomsdaySetup, BridgeRpcError> {
    let now_unix = ctx.now_unix_secs();
    let (ship_type_id, ship_group_id, mod_type_id) = ctx.db.with_snapshot(|snap| {
        let ship = snap.items.get(&ship_id).ok_or_else(|| BridgeRpcError {
            code: "ShipNotFound".into(),
            message: format!("Active ship {ship_id} not found"),
        })?;
        let s_group = ctx
            .sde
            .types
            .get(&ship.type_id)
            .map(|t| t.group_id)
            .unwrap_or(0);
        let module = snap.items.get(&module_id).ok_or_else(|| BridgeRpcError {
            code: "ModuleNotFound".into(),
            message: format!("Doomsday module {module_id} not found"),
        })?;
        let is_high_slot = matches!(
            module.flag,
            eve_db::InventoryFlag::HiSlot0
                | eve_db::InventoryFlag::HiSlot1
                | eve_db::InventoryFlag::HiSlot2
                | eve_db::InventoryFlag::HiSlot3
                | eve_db::InventoryFlag::HiSlot4
                | eve_db::InventoryFlag::HiSlot5
                | eve_db::InventoryFlag::HiSlot6
                | eve_db::InventoryFlag::HiSlot7
        );
        if module.location_id != ship_id || !is_high_slot {
            return Err(BridgeRpcError {
                code: "ModuleNotFitted".into(),
                message: format!("Module {module_id} must be fitted in a high slot on ship {ship_id}"),
            });
        }
        if !snap.module_online_states.get(&module_id).copied().unwrap_or(false) {
            return Err(BridgeRpcError {
                code: "ModuleNotOnline".into(),
                message: format!("Doomsday module {module_id} must be online"),
            });
        }
        Ok((ship.type_id, s_group, module.type_id))
    })?;

    if ship_group_id != 30 {
        return Err(BridgeRpcError {
            code: "ShipCannotActivateSuperweapon".into(),
            message: "Only Titan-class hulls (group 30) can activate Doomsday superweapons".into(),
        });
    }

    let mod_type = ctx.sde.types.get(&mod_type_id);
    let sys_sec = ctx
        .sde
        .solar_systems
        .get(&sys_id)
        .map(|s| s.security_status)
        .unwrap_or(0.0);
    let disallow_empire = mod_type
        .and_then(|t| t.default_attributes.get(&1074).copied())
        .unwrap_or(1.0)
        > 0.0;
    if sys_sec >= 0.45 && disallow_empire {
        return Err(BridgeRpcError {
            code: "CannotActivateInEmpireSpace".into(),
            message: format!(
                "Doomsday superweapons cannot be activated in high-security empire space (sec={sys_sec:.2})"
            ),
        });
    }

    let safety = crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
        rt.safety_levels
            .get(&char_id)
            .copied()
            .unwrap_or(crate::services::combat_crimewatch::SafetyLevel::Red)
    });
    let corr_stage5 =
        crate::services::insurgency_services::insurgency_system_corruption_stage(ctx, sys_id) >= 5;
    if sys_sec > 0.0 && !corr_stage5 && safety == crate::services::combat_crimewatch::SafetyLevel::Green {
        return Err(BridgeRpcError::user_error("CantDoInCurrentSafetyLevel"));
    }

    if ctx.is_module_in_reactivation_delay(ship_id, module_id) {
        return Err(BridgeRpcError {
            code: "ModuleInReactivationDelay".into(),
            message: "Doomsday module is currently in reactivation delay".into(),
        });
    }
    let max_range_m = mod_type
        .and_then(|t| t.default_attributes.get(&54).copied())
        .filter(|&r| r > 0.0)
        .unwrap_or(150_000.0);
    let immob_secs = (mod_type
        .and_then(|t| t.default_attributes.get(&2428).copied())
        .filter(|&v| v > 0.0)
        .unwrap_or(30_000.0)
        / 1000.0)
        .round() as u64;
    let no_jump_secs = (mod_type
        .and_then(|t| t.default_attributes.get(&2427).copied())
        .filter(|&v| v > 0.0)
        .unwrap_or(300_000.0)
        / 1000.0)
        .round() as u64;

    let sys = ctx.systems.get_or_spawn(sys_id);
    let all_balls = sys.list_balls().await.unwrap_or_default();
    let ship_pos = all_balls
        .iter()
        .find(|b| b.id == ship_id)
        .map(|b| b.position)
        .unwrap_or(DVec3::ZERO);

    validate_doomsday_target_points(ship_pos, max_range_m, target_points)?;
    let em_dmg = mod_type
        .and_then(|t| t.default_attributes.get(&114).copied())
        .unwrap_or(0.0);
    let ex_dmg = mod_type
        .and_then(|t| t.default_attributes.get(&116).copied())
        .unwrap_or(0.0);
    let ki_dmg = mod_type
        .and_then(|t| t.default_attributes.get(&117).copied())
        .unwrap_or(0.0);
    let th_dmg = mod_type
        .and_then(|t| t.default_attributes.get(&118).copied())
        .unwrap_or(0.0);
    let dmg_vec = if em_dmg + ex_dmg + ki_dmg + th_dmg > 0.0 {
        DamageVector::new(em_dmg, th_dmg, ki_dmg, ex_dmg)
    } else {
        DamageVector::new(10_000.0, 10_000.0, 10_000.0, 10_000.0)
    };

    let req_fuel_type = mod_type
        .and_then(|t| t.default_attributes.get(&713).copied())
        .map(|v| v as u32)
        .filter(|&ft| ft > 0);
    let req_fuel_qty = mod_type
        .and_then(|t| t.default_attributes.get(&714).copied())
        .map(|v| v as i32)
        .filter(|&q| q > 0)
        .unwrap_or(DEFAULT_FUEL_COST);

    ctx.db
        .with_mut_snapshot(|snap| {
            let matches_fuel = |tid: u32| match req_fuel_type {
                Some(ft) => tid == ft,
                None => matches!(tid, 16274 | 16275 | 17887..=17889),
            };
            let cand = snap
                .items
                .values()
                .find(|i| {
                    i.location_id == ship_id && matches_fuel(i.type_id) && i.quantity >= req_fuel_qty
                })
                .or_else(|| {
                    snap.items.values().find(|i| {
                        i.location_id == ship_id && matches_fuel(i.type_id) && i.quantity > 0
                    })
                })
                .map(|i| (i.item_id, i.quantity));
            match cand {
                Some((fid, avail)) if avail >= req_fuel_qty => {
                    let mut rem = 0;
                    if let Some(it) = snap.items.get_mut(&fid) {
                        it.quantity -= req_fuel_qty;
                        rem = it.quantity;
                    }
                    if rem <= 0 {
                        snap.items.remove(&fid);
                    }
                    Ok(())
                }
                Some((_, avail)) => Err(format!(
                    "Insufficient superweapon fuel: need {req_fuel_qty}, have {avail}"
                )),
                None => Err(format!(
                    "Missing required superweapon fuel (need {req_fuel_qty})"
                )),
            }
        })
        .map_err(|message| BridgeRpcError {
            code: "InsufficientFuel".into(),
            message,
        })?;

    // Immobilize & tether-block the Titan during Doomsday firing cycle
    let mut cs = sys
        .get_combat_state(ship_id)
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| ctx.build_type_combat_state(ship_type_id));
    let existing_orig = active_bosonic_cycles()
        .lock()
        .ok()
        .and_then(|g| {
            g.iter()
                .find(|c| c.ship_id == ship_id && !c.mobility_restored)
                .map(|c| {
                    (
                        c.orig_max_velocity,
                        c.orig_warp_scramble_strength,
                        c.orig_disallow_assistance,
                    )
                })
        });
    let orig_warp_scramble_strength = existing_orig
        .map(|o| o.1)
        .unwrap_or(cs.warp_scramble_strength);
    let orig_disallow_assistance = existing_orig
        .map(|o| o.2)
        .unwrap_or(cs.disallow_assistance);
    cs.warp_scramble_strength = cs.warp_scramble_strength.max(100);
    cs.disallow_assistance = true;
    cs.is_tethered = false;
    let orig_max_velocity =
        if let Ok(Some((_, mut titan_ball))) = sys.get_ball_snapshot(ship_id).await {
            let orig_v = existing_orig.map(|o| o.0).unwrap_or(titan_ball.max_velocity);
            titan_ball.max_velocity = 0.0;
            let _ = sys.spawn_ball_with_combat(titan_ball, cs).await;
            orig_v
        } else {
            let _ = sys.set_combat_state(ship_id, cs).await;
            0.0
        };
    {
        let db_key = ctx.instance_id as usize;
        let mut st = ctx
            .tactical_state
            .write()
            .unwrap_or_else(|e| e.into_inner());
        st.lance_caster_lock_until
            .insert((db_key, ship_id), now_unix + no_jump_secs);
    }

    ctx.crimewatch
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_create(char_id)
        .weapon_timer_until_unix = now_unix + 60;
    let reactiv_secs = (mod_type
        .and_then(|t| t.default_attributes.get(&669).copied())
        .filter(|&v| v > 0.0)
        .unwrap_or_else(|| {
            mod_type
                .and_then(|t| t.default_attributes.get(&73).copied())
                .unwrap_or(300_000.0)
        }))
        / 1000.0;
    ctx.record_module_reactivation_delay(ship_id, module_id, reactiv_secs);

    Ok(TitanDoomsdaySetup {
        sys,
        all_balls,
        ship_pos,
        ship_type_id,
        mod_type_id,
        dmg_vec,
        fuel_consumed: req_fuel_qty,
        max_range_m,
        immob_secs,
        no_jump_secs,
        now_unix,
        orig_max_velocity,
        orig_warp_scramble_strength,
        orig_disallow_assistance,
    })
}

fn parse_vec3_from_args(args: &[PyRep], start_idx: usize) -> Option<DVec3> {
    if let Some(PyRep::Tuple(vals) | PyRep::List(vals)) = args.get(start_idx) {
        if vals.len() >= 3 {
            if let (Some(x), Some(y), Some(z)) = (
                vals[0].as_f64(),
                vals[1].as_f64(),
                vals[2].as_f64(),
            ) {
                return Some(DVec3::new(x, y, z));
            }
        }
    }
    if let (Some(x), Some(y), Some(z)) = (
        args.get(start_idx).and_then(|a| a.as_f64()),
        args.get(start_idx + 1).and_then(|a| a.as_f64()),
        args.get(start_idx + 2).and_then(|a| a.as_f64()),
    ) {
        return Some(DVec3::new(x, y, z));
    }
    None
}

/// `superWeaponMgr.ActivateSlashModule` (`doomsdayConeDOT`) — Bosonic Field Generator cone AoE doomsday.
pub async fn activate_cone_doomsday(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    sys_id: u32,
    module_id: i64,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let dir_vec = if let Some(v) = parse_vec3_from_args(args, 1) {
        if !v.x.is_finite() || !v.y.is_finite() || !v.z.is_finite() {
            return Err(BridgeRpcError {
                code: "InvalidTargetPoint".into(),
                message: "Doomsday direction vector must be finite".into(),
            });
        }
        if v.length_squared() > 1e-6 {
            v.normalize()
        } else {
            DVec3::new(1.0, 0.0, 0.0)
        }
    } else {
        DVec3::new(1.0, 0.0, 0.0)
    };
    let setup =
        validate_and_consume_titan_doomsday(ctx, char_id, ship_id, sys_id, module_id, &[]).await?;
    let resolved_dir = if parse_vec3_from_args(args, 1).is_some() {
        dir_vec
    } else if let Some(tid) = args.get(1).and_then(|a| a.as_i64()) {
        setup
            .all_balls
            .iter()
            .find(|b| b.id == tid)
            .map(|b| (b.position - setup.ship_pos).normalize_or_zero())
            .filter(|v| v.length_squared() > 1e-6)
            .unwrap_or(dir_vec)
    } else {
        dir_vec
    };

    let mod_type = ctx.sde.types.get(&setup.mod_type_id);
    let cone_len_m = setup.max_range_m.max(BOSONIC_CONE_LENGTH_M);
    let warmup_secs = (mod_type
        .and_then(|t| t.default_attributes.get(&2262).copied())
        .filter(|&v| v > 0.0)
        .unwrap_or(10_000.0)
        / 1000.0)
        .round() as u64;
    let dmg_dur_ms = mod_type
        .and_then(|t| t.default_attributes.get(&2264).copied())
        .filter(|&v| v > 0.0)
        .unwrap_or(20_000.0);
    let dmg_cycle_ms = mod_type
        .and_then(|t| t.default_attributes.get(&2265).copied())
        .filter(|&v| v > 0.0)
        .unwrap_or(1_000.0);
    let total_ticks = ((dmg_dur_ms / dmg_cycle_ms).round() as usize).max(1);
    let tick_dmg = setup.dmg_vec;

    let in_cone_targets: Vec<i64> = setup
        .all_balls
        .iter()
        .filter(|b| {
            if b.id == ship_id
                || !is_combatant_ship_or_entity_ball(
                    Some(&ctx.sde),
                    Some(&ctx.db),
                    b.id,
                    b.type_id,
                )
            {
                return false;
            }
            let delta = b.position - setup.ship_pos;
            let dist = delta.length();
            dist > 0.0
                && dist <= cone_len_m
                && delta.normalize().dot(resolved_dir) >= BOSONIC_CONE_COS_HALF_ANGLE
        })
        .map(|b| b.id)
        .collect();

    {
        let mut guard = active_bosonic_cycles()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        guard.push(BosonicActiveCycle {
            db_key: ctx.instance_id as usize,
            sys_id,
            char_id,
            ship_id,
            ship_type_id: setup.ship_type_id,
            mod_type_id: setup.mod_type_id,
            origin_pos: setup.ship_pos,
            dir_vec: resolved_dir,
            cone_len_m,
            tick_dmg,
            started_at_unix: setup.now_unix,
            warmup_secs,
            total_ticks,
            ticks_applied: 0,
            immobilize_until_unix: setup.now_unix + setup.immob_secs,
            orig_max_velocity: setup.orig_max_velocity,
            orig_warp_scramble_strength: setup.orig_warp_scramble_strength,
            orig_disallow_assistance: setup.orig_disallow_assistance,
            mobility_restored: false,
        });
    }

    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnGodmaShipEffect",
        "charid",
        vec![
            PyRep::Integer(module_id),
            PyRep::String("superWeaponCone".into()),
            PyRep::Bool(true),
        ],
    );

    Ok(PyRep::keyval([
        ("moduleID", PyRep::Integer(module_id)),
        ("shipID", PyRep::Integer(ship_id)),
        ("mode", PyRep::String("Cone".into())),
        ("status", PyRep::String("WarmingUp".into())),
        ("warmupSeconds", PyRep::Real(warmup_secs as f64)),
        ("tickCount", PyRep::Integer(total_ticks as i64)),
        ("immobilizeSeconds", PyRep::Real(setup.immob_secs as f64)),
        ("tetherBlocked", PyRep::Bool(true)),
        ("jumpBlockSeconds", PyRep::Real(setup.no_jump_secs as f64)),
        ("fuelConsumed", PyRep::Integer(i64::from(setup.fuel_consumed))),
        ("totalDamageApplied", PyRep::Real(0.0)),
        (
            "hitTargetIDs",
            PyRep::List(in_cone_targets.into_iter().map(PyRep::Integer).collect()),
        ),
    ]))
}

/// `superWeaponMgr.ActivateSinglePointTargetedModule` (`doomsdayHOG`) — GTFO sphere teleport doomsday.
pub async fn activate_spatial_gtfo_doomsday(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    sys_id: u32,
    module_id: i64,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let sys_pre = ctx.systems.get_or_spawn(sys_id);
    let pre_balls = sys_pre.list_balls().await.unwrap_or_default();
    let ship_pos_pre = pre_balls
        .iter()
        .find(|b| b.id == ship_id)
        .map(|b| b.position)
        .unwrap_or(DVec3::ZERO);
    let center = if let Some(v) = parse_vec3_from_args(args, 1) {
        v
    } else if let Some(tid) = args.get(1).and_then(|a| a.as_i64()) {
        pre_balls
            .iter()
            .find(|b| b.id == tid)
            .map(|b| b.position)
            .unwrap_or(ship_pos_pre + DVec3::new(30_000.0, 0.0, 0.0))
    } else {
        ship_pos_pre + DVec3::new(30_000.0, 0.0, 0.0)
    };
    let setup = validate_and_consume_titan_doomsday(
        ctx,
        char_id,
        ship_id,
        sys_id,
        module_id,
        &[center],
    )
    .await?;
    let sphere_radius_m = ctx
        .sde
        .types
        .get(&setup.mod_type_id)
        .and_then(|t| t.default_attributes.get(&2279).copied())
        .filter(|&r| r > 0.0)
        .unwrap_or(GTFO_SPHERE_RADIUS_M);

    {
        let mut guard = active_bosonic_cycles()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        guard.push(BosonicActiveCycle {
            db_key: ctx.instance_id as usize,
            sys_id,
            char_id,
            ship_id,
            ship_type_id: setup.ship_type_id,
            mod_type_id: setup.mod_type_id,
            origin_pos: setup.ship_pos,
            dir_vec: DVec3::X,
            cone_len_m: 0.0,
            tick_dmg: DamageVector::default(),
            started_at_unix: setup.now_unix,
            warmup_secs: 0,
            total_ticks: 0,
            ticks_applied: 0,
            immobilize_until_unix: setup.now_unix + setup.immob_secs,
            orig_max_velocity: setup.orig_max_velocity,
            orig_warp_scramble_strength: setup.orig_warp_scramble_strength,
            orig_disallow_assistance: setup.orig_disallow_assistance,
            mobility_restored: false,
        });
    }

    let mut candidate_anchors: Vec<DVec3> = ctx
        .sde
        .celestials_in_system(sys_id)
        .iter()
        .map(|c| c.position)
        .filter(|p| p.distance(center) > 500_000.0)
        .collect();
    if candidate_anchors.is_empty() {
        candidate_anchors = setup
            .all_balls
            .iter()
            .filter(|b| {
                !is_combatant_ship_or_entity_ball(Some(&ctx.sde), Some(&ctx.db), b.id, b.type_id)
                    && b.position.distance(center) > 500_000.0
            })
            .map(|b| b.position)
            .collect();
    }

    let mut teleported_ids = Vec::new();
    for (idx, b) in setup.all_balls.iter().enumerate() {
        if b.id == ship_id || b.max_velocity <= 0.0 {
            continue;
        }
        let is_eligible_ship = ctx.sde.types.get(&b.type_id).is_some_and(|t| {
            t.category_id == 6
                && !matches!(t.group_id, 30 | 659)
                && t.default_attributes.get(&872).copied().unwrap_or(0.0) <= 0.0
        });
        if !is_eligible_ship {
            continue;
        }
        let target_char_opt = ctx.db.with_snapshot(|snap| {
            snap.characters
                .values()
                .find(|c| c.active_ship_id == b.id && c.station_id.is_none())
                .map(|c| c.character_id)
        });
        let Some(target_char_id) = target_char_opt else {
            continue;
        };
        if crate::services::space_destiny::is_location_change_invulnerable(
            ctx,
            target_char_id,
            setup.now_unix,
        ) {
            continue;
        }
        if let Ok(Some(cs)) = setup.sys.get_combat_state(b.id).await {
            if cs.is_tethered
                || cs.is_invulnerable()
                || cs.undock_invulnerable_remaining_ticks > 0
                || cs.warp_scramble_strength >= 50
                || cs.disallow_assistance
            {
                continue;
            }
        }
        if (b.position - center).length() <= sphere_radius_m {
            let mut hasher = RandomState::new().build_hasher();
            hasher.write_i64(b.id);
            hasher.write_usize(idx);
            hasher.write_u64(setup.now_unix);
            let h = hasher.finish();
            let dest = if !candidate_anchors.is_empty() {
                let anchor = candidate_anchors[(h as usize) % candidate_anchors.len()];
                let angle = ((h >> 16) as f64) * 0.001;
                let jitter = 15_000.0 + ((h >> 32) % 30_000) as f64;
                anchor + DVec3::new(angle.cos() * jitter, angle.sin() * jitter, 5_000.0)
            } else {
                let angle = ((h >> 8) as f64) * 0.001;
                let dist = 1_500_000.0 + ((h >> 24) % 25_000_000) as f64;
                center
                    + DVec3::new(
                        angle.cos() * dist,
                        angle.sin() * dist,
                        ((h % 1_000_000) as f64) - 500_000.0,
                    )
            };
            let _ = setup.sys.set_ball_position(b.id, dest).await;
            let _ = setup.sys.set_movement_mode(b.id, eve_destiny::MovementMode::Stop).await;
            apply_doomsday_capsuleer_crimewatch(
                ctx,
                char_id,
                ship_id,
                sys_id,
                b.id,
                b.type_id,
                setup.now_unix,
            );
            teleported_ids.push(b.id);
        }
    }

    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnGodmaShipEffect",
        "charid",
        vec![
            PyRep::Integer(module_id),
            PyRep::String("superWeaponSpatial".into()),
            PyRep::Bool(true),
        ],
    );

    Ok(PyRep::keyval([
        ("moduleID", PyRep::Integer(module_id)),
        ("shipID", PyRep::Integer(ship_id)),
        ("mode", PyRep::String("SpatialGTFO".into())),
        ("sphereRadiusMeters", PyRep::Real(sphere_radius_m)),
        ("immobilizeSeconds", PyRep::Real(setup.immob_secs as f64)),
        ("tetherBlocked", PyRep::Bool(true)),
        ("fuelConsumed", PyRep::Integer(i64::from(setup.fuel_consumed))),
        (
            "teleportedShipIDs",
            PyRep::List(teleported_ids.into_iter().map(PyRep::Integer).collect()),
        ),
    ]))
}

/// `superWeaponMgr.ActivateSlashModule` (`doomsdaySlash`) — Reaper sweeping beam path doomsday.
pub async fn activate_multipoint_reaper_doomsday(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    sys_id: u32,
    module_id: i64,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let sys_pre = ctx.systems.get_or_spawn(sys_id);
    let pre_balls = sys_pre.list_balls().await.unwrap_or_default();
    let ship_pos_pre = pre_balls
        .iter()
        .find(|b| b.id == ship_id)
        .map(|b| b.position)
        .unwrap_or(DVec3::ZERO);
    let (start_pt, end_pt) = if let (Some(s), Some(e)) =
        (parse_vec3_from_args(args, 1), parse_vec3_from_args(args, 4))
    {
        (s, e)
    } else if let (Some(PyRep::Tuple(s) | PyRep::List(s)), Some(PyRep::Tuple(e) | PyRep::List(e))) =
        (args.get(1), args.get(2))
    {
        let sv = DVec3::new(
            s.first().and_then(|v| v.as_f64()).unwrap_or(0.0),
            s.get(1).and_then(|v| v.as_f64()).unwrap_or(0.0),
            s.get(2).and_then(|v| v.as_f64()).unwrap_or(0.0),
        );
        let ev = DVec3::new(
            e.first().and_then(|v| v.as_f64()).unwrap_or(50_000.0),
            e.get(1).and_then(|v| v.as_f64()).unwrap_or(0.0),
            e.get(2).and_then(|v| v.as_f64()).unwrap_or(0.0),
        );
        (sv, ev)
    } else {
        (
            ship_pos_pre + DVec3::new(20_000.0, 0.0, 0.0),
            ship_pos_pre + DVec3::new(20_000.0, 0.0, 60_000.0),
        )
    };
    let setup = validate_and_consume_titan_doomsday(
        ctx,
        char_id,
        ship_id,
        sys_id,
        module_id,
        &[start_pt, end_pt],
    )
    .await?;
    let mod_type = ctx.sde.types.get(&setup.mod_type_id);
    let beam_radius_m = mod_type
        .and_then(|t| t.default_attributes.get(&2263).copied())
        .filter(|&r| r > 0.0)
        .unwrap_or(REAPER_BEAM_RADIUS_M);
    let sweep_cycles = (mod_type
        .and_then(|t| t.default_attributes.get(&2264).copied())
        .filter(|&v| v > 0.0)
        .unwrap_or(10_000.0)
        / mod_type
            .and_then(|t| t.default_attributes.get(&2265).copied())
            .filter(|&v| v > 0.0)
            .unwrap_or(1_000.0))
    .max(1.0);
    let sweep_dmg = DamageVector::new(
        setup.dmg_vec.em * sweep_cycles,
        setup.dmg_vec.thermal * sweep_cycles,
        setup.dmg_vec.kinetic * sweep_cycles,
        setup.dmg_vec.explosive * sweep_cycles,
    );

    {
        let mut guard = active_bosonic_cycles()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        guard.push(BosonicActiveCycle {
            db_key: ctx.instance_id as usize,
            sys_id,
            char_id,
            ship_id,
            ship_type_id: setup.ship_type_id,
            mod_type_id: setup.mod_type_id,
            origin_pos: setup.ship_pos,
            dir_vec: DVec3::X,
            cone_len_m: 0.0,
            tick_dmg: DamageVector::default(),
            started_at_unix: setup.now_unix,
            warmup_secs: 0,
            total_ticks: 0,
            ticks_applied: 0,
            immobilize_until_unix: setup.now_unix + setup.immob_secs,
            orig_max_velocity: setup.orig_max_velocity,
            orig_warp_scramble_strength: setup.orig_warp_scramble_strength,
            orig_disallow_assistance: setup.orig_disallow_assistance,
            mobility_restored: false,
        });
    }

    let seg = end_pt - start_pt;
    let seg_len_sq = seg.length_squared().max(1.0);
    let mut hit_targets = Vec::new();
    let mut total_applied = 0.0_f64;

    for b in &setup.all_balls {
        if b.id == ship_id
            || !is_combatant_ship_or_entity_ball(Some(&ctx.sde), Some(&ctx.db), b.id, b.type_id)
        {
            continue;
        }
        let t = ((b.position - start_pt).dot(seg) / seg_len_sq).clamp(0.0, 1.0);
        let closest = start_pt + seg * t;
        if (b.position - closest).length() <= beam_radius_m {
            let (app, destroyed) =
                crate::services::station_structure::structure_lifecycle_rpc::apply_damage_structure_or_ball(
                    ctx,
                    &setup.sys,
                    sys_id,
                    char_id,
                    b.id,
                    sweep_dmg,
                    10.0,
                    setup.now_unix,
                )
                .await
                .ok()
                .flatten()
                .map(|(a, d, _)| (a, d))
                .unwrap_or((0.0, false));
            total_applied += app;
            if app > 0.0 {
                crate::services::combat_crimewatch::record_killmail_assist_action(
                    ctx,
                    b.id,
                    char_id,
                    setup.ship_type_id,
                    setup.mod_type_id,
                    app,
                    false,
                    setup.now_unix,
                );
                apply_doomsday_capsuleer_crimewatch(
                    ctx,
                    char_id,
                    ship_id,
                    sys_id,
                    b.id,
                    b.type_id,
                    setup.now_unix,
                );
            }
            if destroyed {
                crate::services::combat_crimewatch::combat_missiles_aoe::handle_destroyed_ball_from_missile_or_aoe(
                    ctx,
                    sys_id,
                    char_id,
                    b.id,
                    b.type_id,
                    b.position,
                    setup.now_unix,
                )
                .await;
            }
            hit_targets.push(b.id);
        }
    }

    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnGodmaShipEffect",
        "charid",
        vec![
            PyRep::Integer(module_id),
            PyRep::String("superWeaponMultiPoint".into()),
            PyRep::Bool(true),
        ],
    );

    Ok(PyRep::keyval([
        ("moduleID", PyRep::Integer(module_id)),
        ("shipID", PyRep::Integer(ship_id)),
        ("mode", PyRep::String("MultiPointReaper".into())),
        ("beamRadiusMeters", PyRep::Real(beam_radius_m)),
        ("immobilizeSeconds", PyRep::Real(setup.immob_secs as f64)),
        ("tetherBlocked", PyRep::Bool(true)),
        ("fuelConsumed", PyRep::Integer(i64::from(setup.fuel_consumed))),
        ("totalDamageApplied", PyRep::Real(total_applied)),
        (
            "hitTargetIDs",
            PyRep::List(hit_targets.into_iter().map(PyRep::Integer).collect()),
        ),
    ]))
}
