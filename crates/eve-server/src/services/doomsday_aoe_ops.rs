//! `doomsday_aoe_ops` — Bosonic Field Generator (Cone), Spatial Phenomena Generator (GTFO),
//! and Reaper (MultiPoint) Titan Doomsday activation handlers (`feat:modules.doomsday-bosonic-field`,
//! `feat:modules.doomsday-gtfo-reaper`, `plan:D-6`).

#![forbid(unsafe_code)]

use crate::router::{NotificationTarget, ServerContext};
use crate::services::doomsday_aoe_tick::{
    GTFO_WARP_SUPPRESSION_SECS, PersistedSuperweaponCycle, SuperweaponCycleKind,
    mutate_superweapon_runtime, read_superweapon_runtime,
};
use eve_destiny::{DVec3, DamageVector, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};

pub use crate::services::doomsday_aoe_tick::{
    step_bosonic_doomsday_in_ballpark, step_bosonic_doomsday_system,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DoomsdayAoeMode {
    Cone,
    Spatial,
    MultiPoint,
}

const DEFAULT_FUEL_COST: i32 = 50_000;
const BOSONIC_CONE_LENGTH_M: f64 = 30_000.0;
const GTFO_SPHERE_RADIUS_M: f64 = 10_000.0;
const REAPER_BEAM_RADIUS_M: f64 = 5_000.0;
const MAX_REAPER_ARC_LENGTH_M: f64 = 100_000.0;
const SUPERWEAPON_NEUT_PULSE_RADIUS_M: f64 = 10_000.0;
const SUPERWEAPON_NEUT_PULSE_GJ: f64 = 35_000.0;

/// Cancels any scheduled or firing AoE Titan superweapon cycles (`Cone`, `SpatialGtfo`, `ReaperSweep`)
/// belonging to `ship_id` when the caster dies or leaves space (`#499`).
pub fn cancel_aoe_superweapon_cycles_for_ship(ctx: &ServerContext, ship_id: i64) {
    let has_cycles = ctx
        .db
        .with_snapshot(|s| !s.superweapon_runtime.active_cycles.is_empty());
    if !has_cycles {
        return;
    }
    mutate_superweapon_runtime(&ctx.db, |rt| {
        rt.active_cycles.retain(|c| c.ship_id != ship_id);
    });
}

pub async fn dispatch_superweapon_aoe_rpc(
    ctx: &ServerContext,
    session: &SessionState,
    mode: DoomsdayAoeMode,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    if session.station_id.is_some() || session.structure_id.is_some() {
        return Err(BridgeRpcError::user_error("CantDoThatWhileDocked"));
    }
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

struct TitanDoomsdaySetup {
    sys: crate::system_actor::SystemHandle,
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

async fn apply_superweapon_aoe_neut_pulse(
    ctx: &ServerContext,
    sys: &crate::system_actor::SystemHandle,
    sys_id: u32,
    all_balls: &[eve_destiny::Ball],
    titan_ship_id: i64,
    titan_pos: DVec3,
    mod_type_id: u32,
) {
    let mod_type = ctx.sde.types.get(&mod_type_id);
    let neut_radius_m = mod_type
        .and_then(|t| t.default_attributes.get(&2259).copied())
        .filter(|&r| r > 0.0)
        .unwrap_or(SUPERWEAPON_NEUT_PULSE_RADIUS_M);
    let neut_amount_gj = mod_type
        .and_then(|t| t.default_attributes.get(&2260).copied())
        .unwrap_or(SUPERWEAPON_NEUT_PULSE_GJ)
        .max(SUPERWEAPON_NEUT_PULSE_GJ);

    for b in all_balls {
        if b.id == titan_ship_id || (b.position - titan_pos).length() > neut_radius_m {
            continue;
        }
        let is_ship = ctx
            .sde
            .types
            .get(&b.type_id)
            .is_some_and(|t| t.category_id == 6 && t.group_id != 29);
        if !is_ship {
            continue;
        }
        if let Ok(Some(mut cs)) = sys.get_combat_state(b.id).await {
            cs.sync_action_breaks();
            if cs.is_cloaked
                || cs.gate_cloak_remaining_ticks > 0
                || cs.is_tethered
                || cs.is_invulnerable()
                || cs.undock_invulnerable_remaining_ticks > 0
            {
                continue;
            }
            if crate::services::inventory_fitting::fitting_ship_forcefield::is_pos_forcefield_protected(
                ctx,
                sys_id,
                b.id,
                b.position,
            )
            .await
            {
                continue;
            }
            let new_cap = (cs.capacitor - neut_amount_gj).max(0.0);
            let _ = sys.set_capacitor(b.id, new_cap).await;
        }
    }
}

async fn validate_and_consume_titan_doomsday<F>(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    sys_id: u32,
    module_id: i64,
    build_targets: F,
) -> Result<(TitanDoomsdaySetup, Vec<DVec3>), BridgeRpcError>
where
    F: FnOnce(DVec3, &[eve_destiny::Ball]) -> Result<Vec<DVec3>, BridgeRpcError>,
{
    let now_unix = ctx.now_unix_secs();
    let (ship_type_id, ship_group_id, mod_type_id) = ctx.db.with_snapshot(|snap| {
        if let Some(ch) = snap.characters.get(&char_id) {
            if ch.station_id.is_some() || ch.structure_id.is_some() {
                return Err(BridgeRpcError::user_error("CantDoThatWhileDocked"));
            }
        }
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
                message: format!(
                    "Module {module_id} must be fitted in a high slot on ship {ship_id}"
                ),
            });
        }
        if !snap
            .module_online_states
            .get(&module_id)
            .copied()
            .unwrap_or(false)
        {
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
    // AoE superweapons with `disallowInEmpireSpace` (attr 1074) are prohibited in ALL Empire space
    // (both Highsec >= 0.45 AND Lowsec 0.0 < sec < 0.45; `Titans.md`).
    if sys_sec > 0.0 && disallow_empire {
        return Err(BridgeRpcError {
            code: "CannotActivateInEmpireSpace".into(),
            message: format!(
                "AoE Doomsday superweapons cannot be activated in empire space (highsec/lowsec, sec={sys_sec:.2})"
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
    if sys_sec > 0.0
        && !corr_stage5
        && safety == crate::services::combat_crimewatch::SafetyLevel::Green
    {
        return Err(BridgeRpcError::user_error("CantDoInCurrentSafetyLevel"));
    }

    if ctx.is_module_in_reactivation_delay(ship_id, module_id) {
        return Err(BridgeRpcError {
            code: "ModuleInReactivationDelay".into(),
            message: "Doomsday module is currently in reactivation delay".into(),
        });
    }

    let sys = ctx.systems.get_or_spawn(sys_id);
    let all_balls = sys.list_balls().await.unwrap_or_default();
    let Some(titan_ball_initial) = all_balls.iter().find(|b| b.id == ship_id).cloned() else {
        return Err(BridgeRpcError {
            code: "ShipNotInSpace".into(),
            message: format!("Active Titan {ship_id} is not in space in solar system {sys_id}"),
        });
    };
    if matches!(titan_ball_initial.mode, MovementMode::Warp { .. }) {
        return Err(BridgeRpcError::user_error("CantActivateInWarp"));
    }
    let ship_pos = titan_ball_initial.position;
    if crate::services::inventory_fitting::fitting_ship_forcefield::is_pos_forcefield_protected(
        ctx,
        sys_id,
        ship_id,
        ship_pos,
    )
    .await
    {
        return Err(BridgeRpcError::user_error("CantActivateInForcefield"));
    }

    let mut cs = sys
        .get_combat_state(ship_id)
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| ctx.build_type_combat_state(ship_type_id));
    cs.sync_action_breaks();
    if cs.is_cloaked || cs.gate_cloak_remaining_ticks > 0 {
        return Err(BridgeRpcError::user_error("CantActivateWhileCloaked"));
    }
    if cs.is_tethered {
        return Err(BridgeRpcError::user_error("CantActivateWhileTethered"));
    }
    let cap_need = mod_type
        .and_then(|t| t.default_attributes.get(&6).copied())
        .filter(|&v| v > 0.0)
        .unwrap_or(0.0);
    if cap_need > 0.0 && cs.capacitor < cap_need {
        return Err(BridgeRpcError::user_error("NotEnoughCapacitor"));
    }

    let max_range_m = mod_type
        .and_then(|t| t.default_attributes.get(&54).copied())
        .filter(|&r| r > 0.0)
        .unwrap_or(150_000.0);
    let resolved_targets = build_targets(ship_pos, &all_balls)?;
    validate_doomsday_target_points(ship_pos, max_range_m, &resolved_targets)?;

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
        .try_with_mut_snapshot(|snap| {
            let matches_fuel = |tid: u32| match req_fuel_type {
                Some(ft) => tid == ft,
                None => matches!(tid, 16274 | 16275 | 17887..=17889),
            };
            let mut eligible_stacks: Vec<(u8, i64, i32)> = snap
                .items
                .values()
                .filter(|i| {
                    i.location_id == ship_id
                        && matches!(i.flag as u16, 5 | 133)
                        && i.flag != eve_db::InventoryFlag::Locked
                        && i.quantity > 0
                        && matches_fuel(i.type_id)
                        && !snap.is_item_or_ancestor_locked_or_impounded(i.item_id)
                })
                .map(|i| (if i.flag as u16 == 133 { 0 } else { 1 }, i.item_id, i.quantity))
                .collect();
            eligible_stacks.sort_by_key(|&(prio, iid, _)| (prio, iid));
            let total_avail: i64 = eligible_stacks.iter().map(|&(_, _, q)| q as i64).sum();
            if total_avail < req_fuel_qty as i64 {
                return Err(if total_avail > 0 {
                    format!("Insufficient superweapon fuel: need {req_fuel_qty}, have {total_avail}")
                } else {
                    format!("Missing required superweapon fuel (need {req_fuel_qty})")
                });
            }
            let mut remaining = req_fuel_qty;
            for (_, fid, _) in eligible_stacks {
                if remaining <= 0 {
                    break;
                }
                let Some((owner_id, avail)) = snap.items.get(&fid).map(|it| (it.owner_id, it.quantity)) else {
                    continue;
                };
                let take = remaining.min(avail);
                if take <= 0 {
                    continue;
                }
                let opts = eve_db::ConsumeExactOptions::for_owner_exact(None, owner_id);
                snap.consume_exact_item_quantity(fid, take, opts)
                    .map_err(|e| e.to_string())?;
                remaining -= take;
            }
            Ok(())
        })
        .map_err(|message| BridgeRpcError {
            code: "InsufficientFuel".into(),
            message,
        })?;

    let existing_orig = read_superweapon_runtime(&ctx.db).and_then(|rt| {
        rt.active_cycles
            .iter()
            .find(|c| c.ship_id == ship_id && !c.mobility_restored)
            .map(|c| {
                (
                    c.orig_max_velocity,
                    c.orig_warp_scramble_strength,
                    c.orig_disallow_assistance,
                )
            })
    });
    let orig_warp_scramble_strength = existing_orig.map(|o| o.1).unwrap_or(0);
    let orig_disallow_assistance = existing_orig.map(|o| o.2).unwrap_or(false);
    cs.warp_scramble_strength = cs.warp_scramble_strength.max(100);
    cs.disallow_assistance = true;
    cs.is_tethered = false;
    if cap_need > 0.0 {
        cs.capacitor = (cs.capacitor - cap_need).max(0.0);
    }
    let orig_max_velocity =
        if let Ok(Some((_, mut titan_ball))) = sys.get_ball_snapshot(ship_id).await {
            let orig_v = existing_orig.map(|o| o.0).unwrap_or(titan_ball.max_velocity);
            titan_ball.max_velocity = 0.0;
            let _ = sys.spawn_ball_with_combat(titan_ball, cs).await;
            orig_v
        } else {
            let _ = sys.set_combat_state(ship_id, cs).await;
            titan_ball_initial.max_velocity
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

    apply_superweapon_aoe_neut_pulse(
        ctx,
        &sys,
        sys_id,
        &all_balls,
        ship_id,
        ship_pos,
        mod_type_id,
    )
    .await;

    Ok((
        TitanDoomsdaySetup {
            sys,
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
        },
        resolved_targets,
    ))
}

fn parse_vec3_from_args(args: &[PyRep], start_idx: usize) -> Option<DVec3> {
    if let Some(PyRep::Tuple(vals) | PyRep::List(vals)) = args.get(start_idx) {
        if vals.len() >= 3 {
            if let (Some(x), Some(y), Some(z)) =
                (vals[0].as_f64(), vals[1].as_f64(), vals[2].as_f64())
            {
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
    let (setup, _) = validate_and_consume_titan_doomsday(
        ctx,
        char_id,
        ship_id,
        sys_id,
        module_id,
        |_, _| Ok(Vec::new()),
    )
    .await?;
    let resolved_dir = if parse_vec3_from_args(args, 1).is_some() {
        dir_vec
    } else if let Some(tid) = args.get(1).and_then(|a| a.as_i64()) {
        setup
            .sys
            .get_ball_snapshot(tid)
            .await
            .ok()
            .flatten()
            .map(|(_, b)| (b.position - setup.ship_pos).normalize_or_zero())
            .filter(|v| v.length_squared() > 1e-6)
            .unwrap_or(dir_vec)
    } else {
        dir_vec
    };

    let mod_type = ctx.sde.types.get(&setup.mod_type_id);
    let cone_len_m = setup.max_range_m.max(BOSONIC_CONE_LENGTH_M);
    let sig_radius_m = mod_type
        .and_then(|t| t.default_attributes.get(&552).copied())
        .filter(|&s| s > 0.0)
        .unwrap_or(10_000.0);
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

    mutate_superweapon_runtime(&ctx.db, |rt| {
        rt.active_cycles.push(PersistedSuperweaponCycle {
            sys_id,
            char_id,
            ship_id,
            ship_type_id: setup.ship_type_id,
            mod_type_id: setup.mod_type_id,
            kind: SuperweaponCycleKind::Cone {
                origin_pos: setup.ship_pos.to_array(),
                dir_vec: resolved_dir.to_array(),
                cone_len_m,
                tick_dmg: [
                    tick_dmg.em,
                    tick_dmg.thermal,
                    tick_dmg.kinetic,
                    tick_dmg.explosive,
                ],
                sig_radius_m,
            },
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
    });

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
        ("signatureRadiusMeters", PyRep::Real(sig_radius_m)),
        ("immobilizeSeconds", PyRep::Real(setup.immob_secs as f64)),
        ("tetherBlocked", PyRep::Bool(true)),
        ("jumpBlockSeconds", PyRep::Real(setup.no_jump_secs as f64)),
        ("fuelConsumed", PyRep::Integer(i64::from(setup.fuel_consumed))),
        ("neutPulseAmountGJ", PyRep::Real(SUPERWEAPON_NEUT_PULSE_GJ)),
        ("totalDamageApplied", PyRep::Real(0.0)),
    ]))
}

/// `superWeaponMgr.ActivateSinglePointTargetedModule` (`doomsdayHOG`) — GTFO 15s delayed subcapital sphere teleport.
pub async fn activate_spatial_gtfo_doomsday(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    sys_id: u32,
    module_id: i64,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let explicit_center = parse_vec3_from_args(args, 1);
    let target_ball_id = if explicit_center.is_none() {
        args.get(1).and_then(|a| a.as_i64())
    } else {
        None
    };

    let (setup, resolved) = validate_and_consume_titan_doomsday(
        ctx,
        char_id,
        ship_id,
        sys_id,
        module_id,
        |ship_pos, balls| {
            let center = if let Some(v) = explicit_center {
                v
            } else if let Some(tid) = target_ball_id {
                balls
                    .iter()
                    .find(|b| b.id == tid)
                    .map(|b| b.position)
                    .ok_or_else(|| BridgeRpcError {
                        code: "TargetNotFound".into(),
                        message: format!("Target ball {tid} not found in space"),
                    })?
            } else {
                ship_pos + DVec3::new(30_000.0, 0.0, 0.0)
            };
            Ok(vec![center])
        },
    )
    .await?;
    let center = resolved[0];
    let mod_type = ctx.sde.types.get(&setup.mod_type_id);
    let sphere_radius_m = mod_type
        .and_then(|t| t.default_attributes.get(&2279).copied())
        .filter(|&r| r > 0.0)
        .unwrap_or(GTFO_SPHERE_RADIUS_M);
    let warmup_secs = (mod_type
        .and_then(|t| t.default_attributes.get(&2262).copied())
        .filter(|&v| v > 0.0)
        .unwrap_or(15_000.0)
        / 1000.0)
        .round() as u64;

    mutate_superweapon_runtime(&ctx.db, |rt| {
        rt.active_cycles.push(PersistedSuperweaponCycle {
            sys_id,
            char_id,
            ship_id,
            ship_type_id: setup.ship_type_id,
            mod_type_id: setup.mod_type_id,
            kind: SuperweaponCycleKind::SpatialGtfo {
                center: center.to_array(),
                sphere_radius_m,
            },
            started_at_unix: setup.now_unix,
            warmup_secs,
            total_ticks: 1,
            ticks_applied: 0,
            immobilize_until_unix: setup.now_unix + setup.immob_secs,
            orig_max_velocity: setup.orig_max_velocity,
            orig_warp_scramble_strength: setup.orig_warp_scramble_strength,
            orig_disallow_assistance: setup.orig_disallow_assistance,
            mobility_restored: false,
        });
    });
    let _ = &setup.sys;

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
        ("status", PyRep::String("WarmingUp".into())),
        ("warmupSeconds", PyRep::Real(warmup_secs as f64)),
        ("sphereRadiusMeters", PyRep::Real(sphere_radius_m)),
        (
            "warpSuppressionSeconds",
            PyRep::Real(GTFO_WARP_SUPPRESSION_SECS as f64),
        ),
        ("immobilizeSeconds", PyRep::Real(setup.immob_secs as f64)),
        ("tetherBlocked", PyRep::Bool(true)),
        ("fuelConsumed", PyRep::Integer(i64::from(setup.fuel_consumed))),
        ("neutPulseAmountGJ", PyRep::Real(SUPERWEAPON_NEUT_PULSE_GJ)),
    ]))
}

/// `superWeaponMgr.ActivateSlashModule` (`doomsdaySlash`) — Reaper 10s warmup + 10s progressive sweeping beam.
pub async fn activate_multipoint_reaper_doomsday(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    sys_id: u32,
    module_id: i64,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let parsed_pair: Option<(DVec3, DVec3)> = if let (Some(s), Some(e)) =
        (parse_vec3_from_args(args, 1), parse_vec3_from_args(args, 4))
    {
        Some((s, e))
    } else if let (Some(PyRep::Tuple(s) | PyRep::List(s)), Some(PyRep::Tuple(e) | PyRep::List(e))) =
        (args.get(1), args.get(2))
    {
        if let (Some(sx), Some(sy), Some(sz), Some(ex), Some(ey), Some(ez)) = (
            s.first().and_then(|v| v.as_f64()),
            s.get(1).and_then(|v| v.as_f64()),
            s.get(2).and_then(|v| v.as_f64()),
            e.first().and_then(|v| v.as_f64()),
            e.get(1).and_then(|v| v.as_f64()),
            e.get(2).and_then(|v| v.as_f64()),
        ) {
            Some((DVec3::new(sx, sy, sz), DVec3::new(ex, ey, ez)))
        } else {
            None
        }
    } else {
        None
    };

    let (setup, resolved) = validate_and_consume_titan_doomsday(
        ctx,
        char_id,
        ship_id,
        sys_id,
        module_id,
        |ship_pos, _| {
            let (start_pt, end_pt) = parsed_pair.unwrap_or((
                ship_pos + DVec3::new(20_000.0, 0.0, 0.0),
                ship_pos + DVec3::new(20_000.0, 0.0, 60_000.0),
            ));
            if !start_pt.x.is_finite()
                || !start_pt.y.is_finite()
                || !start_pt.z.is_finite()
                || !end_pt.x.is_finite()
                || !end_pt.y.is_finite()
                || !end_pt.z.is_finite()
            {
                return Err(BridgeRpcError {
                    code: "InvalidTargetPoint".into(),
                    message: "Reaper sweep coordinates must be finite".into(),
                });
            }
            let arc_len = (end_pt - start_pt).length();
            if arc_len > MAX_REAPER_ARC_LENGTH_M {
                return Err(BridgeRpcError {
                    code: "SweepArcTooLong".into(),
                    message: format!(
                        "Reaper sweep arc ({arc_len:.0} m) exceeds {MAX_REAPER_ARC_LENGTH_M:.0} m maximum"
                    ),
                });
            }
            Ok(vec![start_pt, end_pt])
        },
    )
    .await?;
    let start_pt = resolved[0];
    let end_pt = resolved[1];
    let arc_len = (end_pt - start_pt).length();

    let mod_type = ctx.sde.types.get(&setup.mod_type_id);
    let beam_radius_m = mod_type
        .and_then(|t| t.default_attributes.get(&2263).copied())
        .filter(|&r| r > 0.0)
        .unwrap_or(REAPER_BEAM_RADIUS_M);
    let sig_radius_m = mod_type
        .and_then(|t| t.default_attributes.get(&552).copied())
        .filter(|&s| s > 0.0)
        .unwrap_or(3_500.0);
    let warmup_secs = (mod_type
        .and_then(|t| t.default_attributes.get(&2262).copied())
        .filter(|&v| v > 0.0)
        .unwrap_or(10_000.0)
        / 1000.0)
        .round() as u64;
    let dmg_dur_ms = mod_type
        .and_then(|t| t.default_attributes.get(&2264).copied())
        .filter(|&v| v > 0.0)
        .unwrap_or(10_000.0);
    let dmg_cycle_ms = mod_type
        .and_then(|t| t.default_attributes.get(&2265).copied())
        .filter(|&v| v > 0.0)
        .unwrap_or(1_000.0);
    let total_ticks = ((dmg_dur_ms / dmg_cycle_ms).round() as usize).max(1);
    let sweep_cycles = (total_ticks as f64).max(1.0);
    let sweep_dmg = DamageVector::new(
        setup.dmg_vec.em * sweep_cycles,
        setup.dmg_vec.thermal * sweep_cycles,
        setup.dmg_vec.kinetic * sweep_cycles,
        setup.dmg_vec.explosive * sweep_cycles,
    );

    mutate_superweapon_runtime(&ctx.db, |rt| {
        rt.active_cycles.push(PersistedSuperweaponCycle {
            sys_id,
            char_id,
            ship_id,
            ship_type_id: setup.ship_type_id,
            mod_type_id: setup.mod_type_id,
            kind: SuperweaponCycleKind::ReaperSweep {
                start_pt: start_pt.to_array(),
                end_pt: end_pt.to_array(),
                beam_radius_m,
                sweep_dmg: [
                    sweep_dmg.em,
                    sweep_dmg.thermal,
                    sweep_dmg.kinetic,
                    sweep_dmg.explosive,
                ],
                sig_radius_m,
                already_hit_balls: Vec::new(),
            },
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
    });

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
        ("status", PyRep::String("WarmingUp".into())),
        ("warmupSeconds", PyRep::Real(warmup_secs as f64)),
        ("tickCount", PyRep::Integer(total_ticks as i64)),
        ("beamRadiusMeters", PyRep::Real(beam_radius_m)),
        ("arcLengthMeters", PyRep::Real(arc_len)),
        ("signatureRadiusMeters", PyRep::Real(sig_radius_m)),
        ("immobilizeSeconds", PyRep::Real(setup.immob_secs as f64)),
        ("tetherBlocked", PyRep::Bool(true)),
        ("fuelConsumed", PyRep::Integer(i64::from(setup.fuel_consumed))),
        ("neutPulseAmountGJ", PyRep::Real(SUPERWEAPON_NEUT_PULSE_GJ)),
        ("totalDamageApplied", PyRep::Real(0.0)),
    ]))
}

#[cfg(test)]
#[path = "doomsday_aoe_ops_tests.rs"]
mod doomsday_aoe_ops_tests;

