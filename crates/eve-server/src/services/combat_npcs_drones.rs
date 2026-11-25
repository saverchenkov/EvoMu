//! `combat_npcs_drones` — Salvagers, Tractor Beams, Complete Drone Lifecycle & NPC AI / Factions / Clone Soldiers / EWAR / Remote Repair / Bounties (`C-4`, `C-7`).

#![forbid(unsafe_code)]

use super::*;
use crate::router::notify_registry::{
    push_typed_session_notification, OnDroneSettingChanged, OnDroneStateChange,
};
use eve_destiny::{Ball, MovementMode};

impl DroneSpaceState {
    fn evaluated_drone_attribute(&self, ctx: &ServerContext, attr_id: u16) -> Option<f64> {
        let dyn_override = ctx
            .db
            .get_item(self.drone_item_id)
            .and_then(|r| r.dynamic_attributes.get(&attr_id).copied());
        if !self.is_abandoned && self.parent_ship_id > 0 && self.owner_char_id > 0 {
            if let Ok(mut fit) =
                ctx.get_or_rebuild_ship_fitting(self.owner_char_id, self.parent_ship_id)
            {
                let mut d_state_opt = fit
                    .drone_state(self.drone_item_id, &ctx.sde)
                    .or_else(|| fit.drone_state(self.ball_id, &ctx.sde));
                if d_state_opt.is_none() && ctx.sde.types.contains_key(&self.type_id) {
                    fit.add_drone(self.drone_item_id, self.type_id, &ctx.sde);
                    d_state_opt = fit.drone_state(self.drone_item_id, &ctx.sde);
                }
                if let Some(mut d_state) = d_state_opt {
                    if let Some(mut_val) = dyn_override {
                        d_state.base_attributes.insert(attr_id, mut_val);
                        return Some(d_state.get_attribute(attr_id, &ctx.sde));
                    }
                    if d_state.base_attributes.contains_key(&attr_id)
                        || d_state
                            .modifiers
                            .iter()
                            .any(|m| m.target_attribute_id == attr_id)
                    {
                        return Some(d_state.get_attribute(attr_id, &ctx.sde));
                    }
                }
            }
        }
        dyn_override.or_else(|| {
            ctx.sde
                .types
                .get(&self.type_id)
                .and_then(|t| t.default_attributes.get(&attr_id).copied())
        })
    }

    pub fn effective_tracking_speed(&self, ctx: &ServerContext) -> f64 {
        self.evaluated_drone_attribute(ctx, 160)
            .unwrap_or(1.0)
            .max(0.01)
    }

    pub fn effective_falloff_range(&self, ctx: &ServerContext) -> f64 {
        self.evaluated_drone_attribute(ctx, 158)
            .unwrap_or(5_000.0)
            .max(500.0)
    }

    pub fn effective_optimal_range(&self, ctx: &ServerContext) -> f64 {
        let sde_base = ctx
            .sde
            .types
            .get(&self.type_id)
            .and_then(|t| t.default_attributes.get(&54).copied());
        if let Some(eval_opt) = self.evaluated_drone_attribute(ctx, 54) {
            if sde_base.is_some_and(|sb| (self.optimal_range - sb).abs() < 1e-6) {
                return eval_opt.max(500.0);
            }
        }
        self.optimal_range.max(500.0)
    }

    pub fn cycle_duration_secs(&self, ctx: &ServerContext) -> u64 {
        let ms = self
            .evaluated_drone_attribute(ctx, 51)
            .or_else(|| self.evaluated_drone_attribute(ctx, 73))
            .unwrap_or(4_000.0)
            .max(1_000.0);
        ((ms / 1_000.0).round() as u64).clamp(1, 60)
    }

    /// Computes the turret-style hit multiplier `(0.05..=1.0]` for this drone engaging `target_ball`
    /// using `effective_tracking_speed` (`attr 160`), `effective_optimal_range` (`attr 54`), `effective_falloff_range` (`attr 158`),
    /// angular velocity `omega`, and target signature radius (`attr 552`).
    pub fn compute_combat_hit_multiplier(
        &self,
        ctx: &ServerContext,
        drone_ball: &Ball,
        target_ball: &Ball,
        target_sig_radius: f64,
    ) -> f64 {
        let raw_dist = drone_ball.position.distance(target_ball.position);
        let optimal = self.effective_optimal_range(ctx);
        let falloff = self.effective_falloff_range(ctx).max(1.0);
        if raw_dist > optimal + 3.0 * falloff {
            return 0.0;
        }
        let combat_dist = raw_dist;
        let range_term = ((combat_dist - optimal).max(0.0) / falloff).powi(2);

        let rel_pos = target_ball.position - drone_ball.position;
        let rel_vel = target_ball.velocity - drone_ball.velocity;
        let transversal_speed = if raw_dist > 1.0 {
            let radial_dir = rel_pos / raw_dist;
            let radial_vel = radial_dir * rel_vel.dot(radial_dir);
            let ortho_spd = (rel_vel - radial_vel).length();
            if ortho_spd > 1e-3 {
                ortho_spd
            } else {
                rel_vel.length()
            }
        } else {
            rel_vel.length()
        };
        let omega = transversal_speed / combat_dist.max(250.0);
        let tracking = self.effective_tracking_speed(ctx);
        let sig = target_sig_radius.max(target_ball.radius).max(25.0);
        let tracking_term = ((omega / tracking) * (40.0 / sig)).powi(2);

        0.5_f64.powf(tracking_term + range_term).clamp(0.05, 1.0)
    }
}

#[path = "combat_npcs_drones_cycle_ops.rs"]
mod cycle_ops;
pub use cycle_ops::{step_drone_auto_aggression_and_assist, step_engaged_drones_in_system};
#[cfg(any(test, feature = "testkit"))]
pub use cycle_ops::step_engaged_drones;
pub(crate) use cycle_ops::{clear_drone_web_cycle, is_recently_destroyed_in_persisted};

/// Evaluates effective drone control range in meters for `char_id` flying `ship_id` (`feat:drones.drone-control-range`).
/// Evaluates `charID` attribute `458` (`droneControlDistance`) via `ShipFittingEngine` (Drone Avionics `3437`,
/// Advanced Drone Avionics `23566`, and Drone Link Augmentors), preserving synthetic/legacy skill `23618` fallbacks.
pub fn compute_drone_control_range(ctx: &ServerContext, char_id: i64, ship_id: i64) -> f64 {
    let (da_lv, ada_real_lv, ada_legacy_lv, ship_base) = ctx.db.with_snapshot(|snap| {
        let skills = snap.skills.get(&char_id);
        let da = skills
            .and_then(|m| m.get(&3437))
            .map(|s| s.level as f64)
            .unwrap_or(0.0);
        let ada_real = skills
            .and_then(|m| m.get(&23566))
            .map(|s| s.level as f64)
            .unwrap_or(0.0);
        let ada_legacy = skills
            .and_then(|m| m.get(&23618))
            .map(|s| s.level as f64)
            .unwrap_or(0.0);
        let base = snap
            .items
            .get(&ship_id)
            .and_then(|it| ctx.sde.types.get(&it.type_id))
            .and_then(|t| t.default_attributes.get(&458).copied())
            .unwrap_or(20_000.0);
        (da, ada_real, ada_legacy, base)
    });
    let legacy_extra = if ada_real_lv == 0.0 {
        3_000.0 * ada_legacy_lv
    } else {
        0.0
    };
    let fallback_range = ship_base + 5_000.0 * da_lv + 3_000.0 * ada_real_lv + legacy_extra;
    if let Ok(fit) = ctx.get_or_rebuild_ship_fitting(char_id, ship_id) {
        let char_range = fit.get_character_effective_attribute(458, &ctx.sde);
        let ship_delta = ship_base - 20_000.0;
        return (char_range + ship_delta + legacy_extra).max(fallback_range);
    }
    fallback_range
}

fn tractor_target_required_error() -> BridgeRpcError {
    BridgeRpcError {
        code: "TargetRequired".into(),
        message: "Salvager / Tractor Beam requires a target wreck or container".into(),
        client_error: None,
    }
}

#[allow(clippy::too_many_arguments)]
async fn resolve_salvager_or_tractor_target(
    ctx: &ServerContext,
    sys_handle: &crate::system_actor::SystemHandle,
    char_id: i64,
    ship_id: i64,
    module_id: i64,
    attacker_ball: &Ball,
    wreck_id: i64,
) -> Result<(Ball, f64, Option<eve_dogma::DogmaItemState>), BridgeRpcError> {
    let Some((_, wreck_ball)) = sys_handle.get_ball_snapshot(wreck_id).await.ok().flatten() else {
        return Err(BridgeRpcError {
            code: "TargetNotInSpace".into(),
            message: format!("Target wreck/container {wreck_id} is not in space"),
            client_error: None,
        });
    };
    let dist = attacker_ball.position.distance(wreck_ball.position);
    let mod_state_opt = ctx
        .get_or_rebuild_ship_fitting(char_id, ship_id)
        .ok()
        .and_then(|fit| fit.module_state(module_id, &ctx.sde));
    Ok((wreck_ball, dist, mod_state_opt))
}

fn validate_tractor_ownership_and_range_readonly(
    ctx: &ServerContext,
    char_id: i64,
    mod_type: &eve_sde::InvType,
    mod_state_opt: Option<&eve_dogma::DogmaItemState>,
    wreck_id: i64,
    dist: f64,
) -> Result<(), BridgeRpcError> {
    let max_range = mod_state_opt
        .map(|ms| ms.get_attribute(54, &ctx.sde))
        .filter(|&v| v > 0.0)
        .or_else(|| mod_type.default_attributes.get(&54).copied())
        .unwrap_or(20_000.0)
        .max(20_000.0);
    if dist > max_range {
        return Err(BridgeRpcError::target_out_of_range(
            crate::services::inventory_fitting::resolve_target_display_name(ctx, wreck_id),
            mod_type.type_id as i64,
            dist,
            format!(
                "Wreck/can {wreck_id} is {dist:.1} m away (exceeds tractor beam range {max_range:.1} m)"
            ),
        ));
    }
    let now_unix = ctx.now_unix();
    let wreck_owner = ctx
        .db
        .with_snapshot(|snap| snap.items.get(&wreck_id).map(|i| i.owner_id))
        .unwrap_or(char_id);
    let is_abandoned = with_combat_runtime(ctx, |rt| {
        rt.container_lifetimes
            .get(&wreck_id)
            .map(|c| c.is_abandoned)
            .unwrap_or(false)
    }) || ctx
        .crimewatch
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .states
        .get(&wreck_owner)
        .is_some_and(|s| s.is_criminal(now_unix));
    let same_corp_or_fleet = ctx.db.with_snapshot(|snap| {
        let c1 = snap.characters.get(&char_id).map(|c| c.corporation_id);
        let c2 = snap.characters.get(&wreck_owner).map(|c| c.corporation_id);
        let same_corp = c1.is_some() && c1 == c2 && c1.unwrap_or(0) > 2_000_000;
        let same_fleet = snap
            .fleets
            .values()
            .any(|f| f.members.contains(&char_id) && f.members.contains(&wreck_owner));
        same_corp || same_fleet
    });
    if wreck_owner != char_id && wreck_owner != 0 && !is_abandoned && !same_corp_or_fleet {
        return Err(BridgeRpcError {
            code: "TractorBeamOwnershipRestricted".into(),
            message: "Cannot tractor a wreck or container owned by another pilot".into(),
            client_error: None,
        });
    }
    Ok(())
}

/// Read-only pre-commit validation for Tractor Beam (`group_id == 650`, `#1486`).
#[allow(clippy::too_many_arguments)]
pub async fn precheck_tractor_beam_activation(
    ctx: &ServerContext,
    sys_handle: &crate::system_actor::SystemHandle,
    char_id: i64,
    ship_id: i64,
    module_id: i64,
    mod_type: &eve_sde::InvType,
    target_id_opt: Option<i64>,
) -> Result<(), BridgeRpcError> {
    let Some(wreck_id) = target_id_opt else {
        return Err(tractor_target_required_error());
    };
    let Some((_, attacker_ball)) = sys_handle.get_ball_snapshot(ship_id).await.ok().flatten() else {
        return Ok(());
    };
    let (_, dist, mod_state_opt) = resolve_salvager_or_tractor_target(
        ctx,
        sys_handle,
        char_id,
        ship_id,
        module_id,
        &attacker_ball,
        wreck_id,
    )
    .await?;
    validate_tractor_ownership_and_range_readonly(
        ctx,
        char_id,
        mod_type,
        mod_state_opt.as_ref(),
        wreck_id,
        dist,
    )
}

/// Evaluates Salvager (`group_id == 1122`) or Tractor Beam (`group_id == 650`) module activation (`feat:modules.salvager`, `feat:combat.wreck-size-salvage-difficulty`, `feat:modules.tractor-beam`).
#[allow(clippy::too_many_arguments)]
pub async fn activate_salvager_or_tractor_module(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    sys_id: u32,
    module_id: i64,
    _m: &eve_dogma::FittedModule,
    mod_type: &eve_sde::InvType,
    attacker_ball: &Ball,
    target_id_opt: Option<i64>,
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let Some(wreck_id) = target_id_opt else {
        return Some(Err(tractor_target_required_error()));
    };
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let (wreck_ball, dist, mod_state_opt) = match resolve_salvager_or_tractor_target(
        ctx,
        &sys_handle,
        char_id,
        ship_id,
        module_id,
        attacker_ball,
        wreck_id,
    )
    .await
    {
        Ok(res) => res,
        Err(e) => return Some(Err(e)),
    };

    // 1. Tractor Beam (`group_id == 650`, `feat:modules.tractor-beam`)
    if mod_type.group_id == 650 {
        if let Err(e) = validate_tractor_ownership_and_range_readonly(
            ctx,
            char_id,
            mod_type,
            mod_state_opt.as_ref(),
            wreck_id,
            dist,
        ) {
            return Some(Err(e));
        }
        let tractor_vel = mod_state_opt
            .as_ref()
            .and_then(|ms| {
                let v624 = ms.get_attribute(624, &ctx.sde);
                if v624 > 0.0 {
                    return Some(v624);
                }
                if ms.modifiers.iter().any(|m| m.target_attribute_id == 1045) {
                    let base_1045 = mod_type
                        .default_attributes
                        .get(&1045)
                        .copied()
                        .unwrap_or(500.0)
                        .max(1.0);
                    let eff_1045 = ms.get_attribute(1045, &ctx.sde);
                    if eff_1045 > 0.0 {
                        return Some(eff_1045.max(1_000.0 * (eff_1045 / base_1045)));
                    }
                }
                None
            })
            .or_else(|| {
                mod_type
                    .default_attributes
                    .get(&624)
                    .copied()
                    .filter(|&v| v > 0.0)
            })
            .unwrap_or(1_000.0);
        let pull_seconds = mod_state_opt
            .as_ref()
            .map(|ms| ms.get_attribute(73, &ctx.sde))
            .filter(|&ms| ms > 0.0)
            .or_else(|| mod_type.default_attributes.get(&73).copied())
            .map(|ms| (ms / 1_000.0).max(1.0))
            .unwrap_or(5.0);
        let (new_pos, arrived) = crate::combat::drones_salvage_tractor::step_tractor_beam(
            wreck_ball.position,
            attacker_ball.position,
            tractor_vel,
            pull_seconds,
        );
        let _ = sys_handle.set_ball_position(wreck_id, new_pos).await;
        ctx.db.with_mut_snapshot(|snap| {
            if snap.items.contains_key(&wreck_id) {
                snap.item_positions
                    .insert(wreck_id, [new_pos.x, new_pos.y, new_pos.z]);
            }
        });
        return Some(Ok(PyRep::keyval([
            ("moduleID", PyRep::Integer(module_id)),
            ("targetID", PyRep::Integer(wreck_id)),
            ("newDistanceMeters", PyRep::Real(attacker_ball.position.distance(new_pos))),
            ("arrivedWithinLootRange", PyRep::Bool(arrived)),
        ])));
    }

    // 2. Salvager (`group_id == 1122`, `feat:modules.salvager`, `feat:combat.wreck-size-salvage-difficulty`)
    let max_range = mod_state_opt
        .as_ref()
        .map(|ms| ms.get_attribute(54, &ctx.sde))
        .filter(|&v| v > 0.0)
        .or_else(|| mod_type.default_attributes.get(&54).copied())
        .unwrap_or(5_000.0)
        .max(5_000.0);
    if dist > max_range {
        return Some(Err(BridgeRpcError::target_out_of_range(
            crate::services::inventory_fitting::resolve_target_display_name(ctx, wreck_id),
            mod_type.type_id as i64,
            dist,
            format!(
                "Wreck {wreck_id} is {dist:.1} m away (exceeds salvager range {max_range:.1} m)"
            ),
        )));
    }
    let res = crate::combat::drones_salvage_tractor::execute_wreck_salvage_cycle_scheduled(
        ctx,
        sys_id,
        char_id,
        ship_id,
        wreck_id,
        Some(module_id),
        Some(mod_type.type_id),
        true,
    )
    .await;
    Some(res)
}

pub use crate::combat::drones_salvage_tractor::execute_wreck_salvage_cycle;

/// Applies incoming weapon/NPC damage to a launched drone in space (`feat:drones.drone-hp-loss`).
/// If the drone's structure HP reaches 0, removes the drone ball from Ballpark and deletes its inventory record.
pub async fn apply_damage_to_drone_in_space(
    ctx: &ServerContext,
    solar_system_id: u32,
    drone_ball_id: i64,
    damage: DamageVector,
) -> (f64, bool) {
    let sys_handle = ctx.systems.get_or_spawn(solar_system_id);
    let Some((applied, destroyed, _)) = sys_handle.apply_damage(drone_ball_id, damage).await.ok().flatten() else {
        return (0.0, false);
    };
    if destroyed {
        let removed_drone = sys_handle.remove_active_drone(drone_ball_id).await.ok().flatten();
        let cleaned = cycle_ops::cleanup_destroyed_drone_db_item(
            ctx,
            solar_system_id,
            drone_ball_id,
            removed_drone.as_ref().map(|d| d.drone_item_id),
        );
        if cleaned {
            let _ = sys_handle.clear_drone_runtime(drone_ball_id).await;
            cycle_ops::clear_drone_web_cycle(&sys_handle, drone_ball_id).await;
            let _ = sys_handle.remove_ball(drone_ball_id).await;
        }
    }
    (applied, destroyed)
}

/// Removes a destroyed drone's DB item, world positions, and `space_object_meta` from `snap`.
pub(crate) fn purge_destroyed_drone_from_snap(
    snap: &mut eve_db::WorldStateSnapshot,
    drone_item_id: Option<i64>,
    drone_ball_id: i64,
) {
    if let Some(iid) = drone_item_id {
        snap.items.remove(&iid);
        snap.item_positions.remove(&iid);
        snap.sp6_lifecycle.space_object_meta.remove(&iid);
    }
    snap.item_positions.remove(&drone_ball_id);
    snap.sp6_lifecycle.space_object_meta.remove(&drone_ball_id);
}

/// Moves a live space drone item still located in `expected_sys_id` (`InventoryFlag::None`)
/// into `ship_id`'s `DroneBay` and clears its space position/meta.
pub(crate) fn move_drone_item_to_bay_in_snap(
    snap: &mut eve_db::WorldStateSnapshot,
    drone_item_id: i64,
    drone_ball_id: i64,
    expected_sys_id: u32,
    owner_char_id: Option<i64>,
    ship_id: i64,
    clear_meta_entirely: bool,
) -> bool {
    let Some(it) = snap.items.get_mut(&drone_item_id) else {
        return false;
    };
    if it.location_id != expected_sys_id as i64 || it.flag != InventoryFlag::None {
        return false;
    }
    if let Some(cid) = owner_char_id {
        it.owner_id = cid;
    }
    it.location_id = ship_id;
    it.flag = InventoryFlag::DroneBay;
    it.version += 1;
    snap.item_positions.remove(&drone_item_id);
    snap.item_positions.remove(&drone_ball_id);
    if clear_meta_entirely {
        snap.sp6_lifecycle.space_object_meta.remove(&drone_ball_id);
        snap.sp6_lifecycle.space_object_meta.remove(&drone_item_id);
    } else if let Some(meta) = snap.sp6_lifecycle.space_object_meta.get_mut(&drone_item_id) {
        meta.drone_ball_id = None;
    }
    true
}

pub(crate) fn pilot_drone_capacity(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    ship_fit: Option<&eve_dogma::ShipFittingEngine>,
) -> (usize, f64, f64) {
    let max_drones = ctx.db.with_snapshot(|s| {
        s.skills.get(&char_id)
            .and_then(|m| m.get(&3436))
            .map(|sk| (sk.level as usize).min(5))
            .unwrap_or(0)
    });
    let triage = ship_fit.is_some_and(|f| {
        f.modules.values().any(|m| matches!(m.state, eve_dogma::ModuleState::Active | eve_dogma::ModuleState::Overloaded) && matches!(m.type_id, 27951 | 4294))
    });
    let bw = if triage { 0.0 } else {
        ship_fit.map(|f| f.ship.get_attribute(1271, &ctx.sde)).filter(|&b| b > 0.0).unwrap_or_else(|| {
            ctx.db.with_snapshot(|s| s.items.get(&ship_id).and_then(|i| ctx.sde.types.get(&i.type_id)).and_then(|t| t.default_attributes.get(&1271).copied())).unwrap_or(50.0)
        })
    };
    let cap = ctx.db.with_snapshot(|s| {
        s.items.get(&ship_id).and_then(|i| i.dynamic_attributes.get(&283).copied())
    }).or_else(|| {
        ship_fit.map(|f| f.ship.get_attribute(283, &ctx.sde)).filter(|&c| c > 0.0)
    }).unwrap_or_else(|| {
        ctx.db.with_snapshot(|s| s.items.get(&ship_id).and_then(|i| ctx.sde.types.get(&i.type_id)).and_then(|t| t.default_attributes.get(&283).copied())).unwrap_or(0.0)
    });
    (max_drones, bw, cap)
}

struct PreparedLaunchDrone<'a> {
    source_item_id: i64,
    rec: InventoryItemRecord,
    sde_t: Option<&'a eve_sde::InvType>,
    is_sentry: bool,
    max_vel: f64,
    bw_used: f64,
    opt_range: f64,
    dmg_vec: DamageVector,
    d_pos: DVec3,
    sh_rep: f64,
    ar_rep: f64,
    hu_rep: f64,
    web_f: f64,
    neut_a: f64,
    ecm_s: f64,
    tp_s: f64,
    eff_hp: Option<f64>,
    eff_sh: Option<f64>,
    eff_ar: Option<f64>,
}

fn no_drone_management_code(max_drones_allowed: usize) -> &'static str {
    if max_drones_allowed == 0 {
        "NoDroneManagementAbilities"
    } else {
        "NoDroneManagementAbilitiesLeft"
    }
}

fn no_drone_management_error(max_drones_allowed: usize) -> BridgeRpcError {
    BridgeRpcError::user_error(no_drone_management_code(max_drones_allowed))
}

fn drone_launch_error(code: &str) -> BridgeRpcError {
    if code == "MaxBandwidthExceeded" {
        BridgeRpcError::from_user_key(eve_proto::UserErrorKey::MaxBandwidthExceeded)
    } else {
        BridgeRpcError::user_error(code)
    }
}

/// Validates that `did` is a launchable drone in `char_id`'s `ship_id` drone bay.
fn validate_drone_bay_item(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    char_id: i64,
    ship_id: i64,
    did: i64,
) -> Result<InventoryItemRecord, BridgeRpcError> {
    let Some(it) = snap.items.get(&did).cloned() else {
        return Err(BridgeRpcError {
            code: "DroneNotFound".into(),
            message: format!("Drone item {did} not found"),
            client_error: None,
        });
    };
    if it.owner_id != char_id
        || it.location_id != ship_id
        || it.flag != InventoryFlag::DroneBay
        || it.quantity <= 0
    {
        return Err(BridgeRpcError {
            code: "DroneNotInBay".into(),
            message: format!("Drone {did} is not in ship {ship_id}'s drone bay"),
            client_error: None,
        });
    }
    if !ctx.sde.types.get(&it.type_id).is_some_and(|t| t.category_id == 18) {
        return Err(BridgeRpcError {
            code: "NotADrone".into(),
            message: format!("Item {did} (type {}) is not a drone", it.type_id),
            client_error: None,
        });
    }
    let is_excavator = ctx
        .sde
        .types
        .get(&it.type_id)
        .is_some_and(|t| t.group_id == 101 && t.default_attributes.get(&1272).copied() == Some(25.0));
    if is_excavator {
        let ship_type = snap.items.get(&ship_id).map(|s| s.type_id).unwrap_or(0);
        let can_operate_excavator = ctx.sde.types.get(&ship_type).is_some_and(|t| {
            t.default_attributes.contains_key(&3223) || t.default_attributes.contains_key(&3224)
        });
        if !can_operate_excavator {
            return Err(BridgeRpcError {
                code: "InvalidShipForExcavator".into(),
                message: "Excavator mining drones can only be operated by a Capital Industrial Ship".into(),
                client_error: None,
            });
        }
    }
    Ok(it)
}

/// Resolves an attribute for a drone item, scaling dynamic mutated overrides by fitting ratio.
fn drone_effective_attribute(
    ctx: &ServerContext,
    ship_fit: Option<&eve_dogma::ShipFittingEngine>,
    rec: &InventoryItemRecord,
    sde_t: Option<&eve_sde::InvType>,
    aid: u16,
) -> Option<f64> {
    let sde_base = sde_t.and_then(|t| t.default_attributes.get(&aid).copied());
    let fit_eff = ship_fit
        .and_then(|f| f.drone_state(rec.item_id, &ctx.sde))
        .filter(|ds| {
            ds.base_attributes.contains_key(&aid)
                || ds.modifiers.iter().any(|m| m.target_attribute_id == aid)
        })
        .map(|ds| ds.get_attribute(aid, &ctx.sde));
    if let Some(&mut_val) = rec.dynamic_attributes.get(&aid) {
        if let (Some(fe), Some(sb)) = (fit_eff, sde_base.filter(|b| b.abs() > 1e-9)) {
            Some(mut_val * (fe / sb))
        } else {
            Some(mut_val)
        }
    } else {
        fit_eff.or(sde_base)
    }
}

/// Captures effective stats and spawn position for a drone about to be launched into space.
fn prepare_launch_drone<'a>(
    ctx: &'a ServerContext,
    ship_fit_opt: &mut Option<eve_dogma::ShipFittingEngine>,
    rec: InventoryItemRecord,
    d_pos: DVec3,
) -> PreparedLaunchDrone<'a> {
    if let Some(ref mut f) = ship_fit_opt {
        if !f.drones.iter().any(|&(id, _)| id == rec.item_id) {
            f.add_drone(rec.item_id, rec.type_id, &ctx.sde);
        }
    }
    let sde_t = ctx.sde.types.get(&rec.type_id);
    let attr = |aid: u16| drone_effective_attribute(ctx, ship_fit_opt.as_ref(), &rec, sde_t, aid);
    let raw_vel = attr(37).unwrap_or(1_500.0);
    let is_sentry = raw_vel < 1.0;
    let max_vel = if is_sentry { 0.0 } else { raw_vel };
    let bw_used = attr(1272).unwrap_or(5.0);
    let opt_range = attr(54).unwrap_or(5_000.0);
    let dmg_mult = attr(64).unwrap_or(1.0);
    let dmg_vec = DamageVector {
        em: attr(114).unwrap_or(0.0) * dmg_mult,
        explosive: attr(116).unwrap_or(0.0) * dmg_mult,
        kinetic: attr(117).unwrap_or(0.0) * dmg_mult,
        thermal: attr(118).unwrap_or(0.0) * dmg_mult,
    };
    let sh_rep = attr(68).unwrap_or(0.0);
    let ar_rep = attr(84).unwrap_or(0.0);
    let hu_rep = attr(83).unwrap_or(0.0);
    let web_f = attr(20).unwrap_or(0.0);
    let neut_a = attr(97).unwrap_or(0.0);
    let ecm_s = attr(238)
        .or_else(|| attr(239))
        .or_else(|| attr(240))
        .or_else(|| attr(241))
        .unwrap_or(0.0);
    let tp_s = attr(554).unwrap_or(0.0);
    let eff_hp = rec
        .dynamic_attributes
        .get(&9)
        .copied()
        .or_else(|| attr(9))
        .filter(|&v| v > 0.0);
    let eff_sh = rec
        .dynamic_attributes
        .get(&263)
        .copied()
        .or_else(|| attr(263))
        .filter(|&v| v > 0.0);
    let eff_ar = rec
        .dynamic_attributes
        .get(&265)
        .copied()
        .or_else(|| attr(265))
        .filter(|&v| v > 0.0);
    let source_item_id = rec.item_id;
    PreparedLaunchDrone {
        source_item_id,
        rec,
        sde_t,
        is_sentry,
        max_vel,
        bw_used,
        opt_range,
        dmg_vec,
        d_pos,
        sh_rep,
        ar_rep,
        hu_rep,
        web_f,
        neut_a,
        ecm_s,
        tp_s,
        eff_hp,
        eff_sh,
        eff_ar,
    }
}

/// Plans a V24 `LaunchFromShip` partial launch up to remaining active drone slots and ship
/// bandwidth, returning per-item launch counts and the limiting UserError key per truncated item.
fn plan_v24_launch_budget(
    requests: &[(i64, usize, f64)],
    cur_count: usize,
    max_drones_allowed: usize,
    cur_bw: f64,
    ship_bw: f64,
) -> (Vec<usize>, HashMap<i64, &'static str>, Option<&'static str>) {
    let mut sim_count = cur_count;
    let mut sim_bw = cur_bw;
    let mut counts = Vec::with_capacity(requests.len());
    let mut errors: HashMap<i64, &'static str> = HashMap::new();
    let mut first_error: Option<&'static str> = None;

    for &(source_item_id, req_qty, unit_bw) in requests {
        let mut launched_for_item = 0usize;
        for _ in 0..req_qty {
            if sim_count + 1 > max_drones_allowed {
                let code = no_drone_management_code(max_drones_allowed);
                errors.insert(source_item_id, code);
                first_error.get_or_insert(code);
                break;
            }
            if sim_bw + unit_bw > ship_bw + 1e-4 {
                let code = "MaxBandwidthExceeded";
                errors.insert(source_item_id, code);
                first_error.get_or_insert(code);
                break;
            }
            sim_count += 1;
            sim_bw += unit_bw;
            launched_for_item += 1;
        }
        counts.push(launched_for_item);
    }

    (counts, errors, first_error)
}

fn drone_rpc_err(code: &'static str, message: impl Into<String>) -> BridgeRpcError {
    BridgeRpcError {
        code: code.into(),
        message: message.into(),
        client_error: None,
    }
}

enum EntityDroneFilter {
    All,
    Ids(Vec<i64>),
    Invalid,
}

fn extract_entity_drone_filter_ids(
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> EntityDroneFilter {
    let raw = match args.first() {
        Some(PyRep::None) | None => kwargs.get("droneIDs"),
        other => other,
    };
    match raw {
        None | Some(PyRep::None) => EntityDroneFilter::All,
        Some(PyRep::List(items)) | Some(PyRep::Tuple(items)) => {
            if items.is_empty() {
                return EntityDroneFilter::All;
            }
            let ids: Vec<i64> = items
                .iter()
                .filter_map(PyRep::as_i64)
                .filter(|&id| id > 0)
                .collect();
            if ids.is_empty() {
                EntityDroneFilter::Invalid
            } else {
                EntityDroneFilter::Ids(ids)
            }
        }
        Some(PyRep::Integer(id)) => {
            if *id > 0 {
                EntityDroneFilter::Ids(vec![*id])
            } else {
                EntityDroneFilter::Invalid
            }
        }
        Some(_) => EntityDroneFilter::Invalid,
    }
}

fn normalize_forwarded_drone_args<'a>(args: &'a [PyRep]) -> &'a [PyRep] {
    if matches!(args.first(), Some(PyRep::None)) {
        &[]
    } else {
        args
    }
}

/// Validates that the caller is an undocked pilot with an active ship ball in space and is not actively cloaked.
async fn resolve_uncloaked_drone_ship(
    ctx: &ServerContext,
    session: &SessionState,
    action_label: &str,
) -> Result<
    (
        i64,
        i64,
        u32,
        crate::system_actor::SystemHandle,
        eve_destiny::Ball,
    ),
    BridgeRpcError,
> {
    let (char_id, sys_id) = ctx.require_undocked_space_system(
        session,
        "ShipNotInSpace",
        &format!("Cannot {action_label} while docked"),
        true,
        false,
    )?;
    let db_char = ctx.db.with_snapshot(|s| {
        s.characters
            .get(&char_id)
            .map(|c| (c.active_ship_id, c.solar_system_id))
    });
    let Some(ship_id) = session
        .ship_id
        .or_else(|| db_char.map(|(sid, _)| sid))
        .filter(|&id| id > 0)
    else {
        return Err(drone_rpc_err(
            "NoActiveShip",
            "Session has no active ship",
        ));
    };
    if let Some((db_ship_id, db_sys_id)) = db_char {
        if db_sys_id != sys_id || db_ship_id != ship_id {
            return Err(BridgeRpcError::ship_not_in_space(
                ship_id,
                Some(char_id),
                Some(sys_id),
                action_label,
                format!("Caller ship {ship_id} is not active in solar system {sys_id}"),
            ));
        }
    }
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let ship_ball = match sys_handle.get_ball_snapshot(ship_id).await {
        Ok(Some((_, b))) => b,
        Ok(None) => {
            return Err(BridgeRpcError::ship_not_in_space(
                ship_id,
                Some(char_id),
                Some(sys_id),
                action_label,
                "Active ship is not spawned in space",
            ));
        }
        Err(e) => {
            return Err(drone_rpc_err("DestinyCommandFailed", e));
        }
    };
    if crate::services::inventory_fitting::fitting_module_cycles::is_ship_actively_cloaked(
        ctx,
        &sys_handle,
        ship_id,
    )
    .await
    {
        return Err(drone_rpc_err(
            "CannotActivateCloaked",
            format!("Cannot {action_label} while cloaked"),
        ));
    }
    Ok((char_id, ship_id, sys_id, sys_handle, ship_ball))
}

fn drone_matches_owner_or_ship(d: &DroneSpaceState, char_id: i64, ship_id: i64, include_original_owner: bool) -> bool {
    d.owner_char_id == char_id
        || d.parent_ship_id == ship_id
        || (include_original_owner && d.original_owner_char_id == char_id)
}

#[derive(Clone, Copy)]
enum EmptyFilterMode {
    ControlledInSystem,
    AbandonedInSystem,
}

fn validate_entity_drone_presence(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    sys_id: u32,
    filter: &EntityDroneFilter,
    require_same_system: bool,
    require_controlled_filter: bool,
    allow_foreign_in_system_filter: bool,
    empty_mode: EmptyFilterMode,
) -> Result<(), BridgeRpcError> {
    match filter {
        EntityDroneFilter::Invalid => Err(drone_rpc_err(
            "DroneNotFound",
            "No matching drone found in space",
        )),
        EntityDroneFilter::Ids(ids) => {
            let matches_drone = |d: &DroneSpaceState| {
                let id_match = ids.contains(&d.ball_id) || ids.contains(&d.drone_item_id);
                if !id_match {
                    return false;
                }
                if require_same_system && d.solar_system_id != sys_id {
                    return false;
                }
                if require_controlled_filter {
                    return !d.is_abandoned
                        && drone_matches_owner_or_ship(d, char_id, ship_id, false);
                }
                allow_foreign_in_system_filter
                    || drone_matches_owner_or_ship(d, char_id, ship_id, true)
            };
            let found = if require_same_system {
                ctx.systems
                    .get_handle(sys_id)
                    .is_some_and(|h| h.view().active_drones_in_space.values().any(matches_drone))
            } else {
                ctx.systems
                    .active_system_handles()
                    .into_iter()
                    .any(|(_, h)| h.view().active_drones_in_space.values().any(matches_drone))
            };
            if found {
                Ok(())
            } else {
                Err(drone_rpc_err(
                    "DroneNotFound",
                    "No matching drone found in space",
                ))
            }
        }
        EntityDroneFilter::All => {
            let found = ctx.systems.get_handle(sys_id).is_some_and(|h| {
                h.view().active_drones_in_space.values().any(|d| {
                    if d.solar_system_id != sys_id {
                        return false;
                    }
                    match empty_mode {
                        EmptyFilterMode::ControlledInSystem => {
                            !d.is_abandoned
                                && drone_matches_owner_or_ship(d, char_id, ship_id, false)
                        }
                        EmptyFilterMode::AbandonedInSystem => {
                            (d.is_abandoned && d.original_owner_char_id == char_id)
                                || (d.owner_char_id == char_id && d.parent_ship_id == 0)
                        }
                    }
                })
            });
            if found {
                Ok(())
            } else {
                Err(drone_rpc_err(
                    "NoDronesInSpace",
                    "No matching drones in space",
                ))
            }
        }
    }
}

fn is_static_world_object(ctx: &ServerContext, sys_id: u32, id: i64) -> bool {
    ctx.sde.stations.contains_key(&id)
        || ctx.sde.stargates().contains_key(&id)
        || ctx
            .sde
            .celestials_in_system_slice(sys_id)
            .iter()
            .any(|c| c.celestial_id == id)
}

async fn validate_return_bay_filter(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    sys_id: u32,
    sys_handle: &crate::system_actor::SystemHandle,
    filter: &EntityDroneFilter,
) -> Result<(), BridgeRpcError> {
    match filter {
        EntityDroneFilter::Invalid | EntityDroneFilter::All => validate_entity_drone_presence(
            ctx,
            char_id,
            ship_id,
            sys_id,
            filter,
            false,
            false,
            false,
            EmptyFilterMode::ControlledInSystem,
        ),
        EntityDroneFilter::Ids(ids) => {
            let (matched_own, matched_foreign) = {
                let mut own = false;
                let mut foreign = false;
                for (_, h) in ctx.systems.active_system_handles() {
                    let view = h.view();
                    for &id in ids {
                        for d in view.active_drones_in_space.values() {
                            if d.ball_id == id || d.drone_item_id == id {
                                if drone_matches_owner_or_ship(d, char_id, ship_id, true) {
                                    own = true;
                                } else {
                                    foreign = true;
                                }
                            }
                        }
                    }
                }
                (own, foreign)
            };
            if matched_own {
                return Ok(());
            }
            if matched_foreign {
                return Err(drone_rpc_err(
                    "DroneNotFound",
                    "No matching drone found in space",
                ));
            }
            let known_non_drone = ctx.db.with_snapshot(|snap| {
                ids.iter().any(|&id| {
                    is_static_world_object(ctx, sys_id, id)
                        || snap.structures.contains_key(&id)
                        || snap.characters.contains_key(&id)
                        || snap.corporations.contains_key(&id)
                        || snap.items.contains_key(&id)
                })
            });
            if known_non_drone {
                return Err(drone_rpc_err(
                    "DroneNotFound",
                    "No matching drone found in space",
                ));
            }
            for &id in ids {
                if matches!(sys_handle.get_ball_snapshot(id).await, Ok(Some(_))) {
                    return Err(drone_rpc_err(
                        "DroneNotFound",
                        "No matching drone found in space",
                    ));
                }
            }
            // Destroyed drones already removed from space/DB are allowed to no-op cleanly in CmdReturnBay.
            Ok(())
        }
    }
}

use crate::router::rpc_registry::{rpc_entry, RpcBoxFuture, RpcDispatchHandler, RpcEntry};

fn dispatch_rpc<'a>(
    ctx: &'a ServerContext,
    session: &'a mut SessionState,
    service: &'a str,
    method: &'a str,
    args: &'a [PyRep],
    kwargs: &'a HashMap<String, PyRep>,
) -> RpcBoxFuture<'a> {
    Box::pin(handle_npcs_drones(ctx, session, service, method, args, kwargs))
}

pub(crate) const RPCS: &[RpcEntry] = &[
    rpc_entry("ship", "LaunchDrones", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("entity", "CmdReturnBay", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("entity", "CmdReturnHome", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("entity", "CmdReturnAndOrbit", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("entity", "CmdEngage", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("entity", "CmdAssist", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("entity", "CmdGuard", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("entity", "ChangeDroneSettings", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("entity", "SetAggressive", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("entity", "CmdAbandonDrone", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("ship", "ScoopToDroneBay", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("ship", "ScoopDrone", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("entity", "CmdReconnectToDrones", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("entity", "CmdSalvage", RpcDispatchHandler::Async(dispatch_rpc)),
];

/// Handles all V24 client Drone RPCs (`ship.LaunchDrones`, `ship.ScoopToDroneBay`, `entity.CmdReturnBay`,
/// `entity.CmdReturnHome`, `entity.CmdReturnAndOrbit`, `entity.CmdEngage`, `entity.CmdAssist`, `entity.CmdGuard`,
/// `entity.ChangeDroneSettings`, `entity.SetAggressive`, `entity.CmdAbandonDrone`, `entity.CmdReconnectToDrones`, `entity.CmdSalvage`).
pub async fn handle_npcs_drones(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    Box::pin(async move {
    match (service, method) {
        ("ship", "LaunchDrones") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if session.station_id.is_some() || session.structure_id.is_some() {
                return Some(Err(BridgeRpcError {
                    code: "CantLaunchDronesWhileDocked".into(),
                    message: "Cannot launch drones while docked".into(),
                    client_error: None,
                }));
            }
            let Some(ship_id) = session.ship_id.or_else(|| {
                ctx.db
                    .with_snapshot(|s| s.characters.get(&char_id).map(|c| c.active_ship_id))
            }) else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "Session has no active ship".into(),
                    client_error: None,
                }));
            };
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError { code: "NoSolarSystem".into(), message: "Session has no active solar system".into(), client_error: None }));
            };
            if let Err(e) = crate::services::tourney_service::check_tourney_drone_restricted(ctx, sys_id, char_id) {
                return Some(Err(e));
            }
            let sys_handle_guard = ctx.systems.get_or_spawn(sys_id);
            if let Err(e) = crate::services::inventory_fitting::fitting_module_cycles::enforce_space_action_cloak_and_warp_guard(
                ctx,
                &sys_handle_guard,
                char_id,
                ship_id,
                "launch drones",
            )
            .await
            {
                return Some(Err(e));
            }
            let mut has_tuple_entries = false;
            let mut seen_drones = std::collections::HashSet::new();
            let mut requested_launches: Vec<(i64, usize)> = Vec::new();
            match args.first() {
                Some(PyRep::List(items)) | Some(PyRep::Tuple(items)) => {
                    for it in items {
                        if let Some(id) = it.as_i64() {
                            if seen_drones.insert(id) {
                                requested_launches.push((id, 1));
                            }
                        } else if let PyRep::Tuple(pair) | PyRep::List(pair) = it {
                            has_tuple_entries = true;
                            if let Some(id) = pair.first().and_then(|v| v.as_i64()) {
                                let qty = pair
                                    .get(1)
                                    .and_then(|v| v.as_i64())
                                    .unwrap_or(1)
                                    .clamp(1, 50) as usize;
                                if seen_drones.insert(id) {
                                    requested_launches.push((id, qty));
                                }
                            }
                        }
                    }
                }
                Some(PyRep::Integer(id)) => {
                    requested_launches.push((*id, 1));
                }
                _ => {}
            }
            let mut ship_fit_opt = ctx.get_or_rebuild_ship_fitting(char_id, ship_id).ok();
            let snap_type_id = ctx.db.with_snapshot(|snap| snap.items.get(&ship_id).map(|s| s.type_id)).unwrap_or(0);
            if snap_type_id > 0 && ship_fit_opt.as_ref().map(|f| f.ship.type_id) != Some(snap_type_id) {
                ctx.invalidate_ship_fitting(ship_id);
                ship_fit_opt = ctx.get_or_rebuild_ship_fitting(char_id, ship_id).ok();
            }
            let (max_drones_allowed, ship_bw, _ship_drone_cap) = pilot_drone_capacity(ctx, char_id, ship_id, ship_fit_opt.as_ref());
            let (cur_count, cur_bw) = {
                let view = sys_handle_guard.view();
                let active: Vec<&DroneSpaceState> = view
                    .active_drones_in_space
                    .values()
                    .filter(|d| d.parent_ship_id == ship_id && !d.is_abandoned)
                    .collect();
                (active.len(), active.iter().map(|d| d.bandwidth_used).sum::<f64>())
            };

            // V24 `eveMisc.LaunchFromShip` passes `[(itemID, qty), ...]` and/or `(items, whoseBehalfID, ignoreWarning)`
            // and expects `{ source_item_id: [launched_id, ..., (error_tuple,)] }`, launching up to remaining
            // slots & bandwidth as long as at least one drone fits. Legacy single-arg ID lists remain all-or-nothing.
            let is_v24_launch = has_tuple_entries || args.len() >= 2;
            if !requested_launches.is_empty()
                && (max_drones_allowed == 0
                    || cur_count >= max_drones_allowed
                    || (!is_v24_launch && cur_count + requested_launches.len() > max_drones_allowed))
            {
                return Some(Err(no_drone_management_error(max_drones_allowed)));
            }

            let validated_requests: Vec<(InventoryItemRecord, usize)> = match ctx.db.with_snapshot(|snap| {
                let mut out = Vec::with_capacity(requested_launches.len());
                for &(did, req_qty) in &requested_launches {
                    let rec = validate_drone_bay_item(ctx, snap, char_id, ship_id, did)?;
                    let count = if req_qty > 1 {
                        (rec.quantity.max(1) as usize).min(req_qty)
                    } else {
                        1
                    };
                    out.push((rec, count));
                }
                Ok(out)
            }) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };

            let mut per_item_errors: HashMap<i64, &'static str> = HashMap::new();
            let drone_records: Vec<InventoryItemRecord> = if is_v24_launch {
                let mut budget_reqs = Vec::with_capacity(validated_requests.len());
                for (rec, count) in &validated_requests {
                    if let Some(ref mut f) = ship_fit_opt {
                        if !f.drones.iter().any(|&(id, _)| id == rec.item_id) {
                            f.add_drone(rec.item_id, rec.type_id, &ctx.sde);
                        }
                    }
                    let sde_t = ctx.sde.types.get(&rec.type_id);
                    let unit_bw = drone_effective_attribute(ctx, ship_fit_opt.as_ref(), rec, sde_t, 1272).unwrap_or(5.0);
                    budget_reqs.push((rec.item_id, *count, unit_bw));
                }
                let (counts, errors, first_error) = plan_v24_launch_budget(
                    &budget_reqs,
                    cur_count,
                    max_drones_allowed,
                    cur_bw,
                    ship_bw,
                );
                let total_selected: usize = counts.iter().sum();
                if total_selected == 0 && !validated_requests.is_empty() {
                    return Some(Err(drone_launch_error(
                        first_error.unwrap_or("NoDroneManagementAbilitiesLeft"),
                    )));
                }
                per_item_errors = errors;
                let mut selected = Vec::with_capacity(total_selected);
                for ((rec, _), launch_qty) in validated_requests.into_iter().zip(counts.into_iter()) {
                    for _ in 0..launch_qty {
                        selected.push(rec.clone());
                    }
                }
                selected
            } else {
                let total_requested: usize = validated_requests.iter().map(|(_, c)| *c).sum();
                if cur_count + total_requested > max_drones_allowed {
                    return Some(Err(no_drone_management_error(max_drones_allowed)));
                }
                let mut expanded = Vec::with_capacity(total_requested);
                for (rec, count) in validated_requests {
                    for _ in 0..count {
                        expanded.push(rec.clone());
                    }
                }
                let req_bw: f64 = expanded
                    .iter()
                    .map(|r| {
                        r.dynamic_attributes.get(&1272).copied().unwrap_or_else(|| {
                            ctx.sde
                                .types
                                .get(&r.type_id)
                                .and_then(|t| t.default_attributes.get(&1272).copied())
                                .unwrap_or(5.0)
                        })
                    })
                    .sum();
                if cur_bw + req_bw > ship_bw + 1e-4 {
                    return Some(Err(BridgeRpcError::from_user_key(
                        eve_proto::UserErrorKey::MaxBandwidthExceeded,
                    )));
                }
                expanded
            };

            let sys_handle = sys_handle_guard;
            let ship_pos = sys_handle
                .get_ball_snapshot(ship_id)
                .await
                .ok()
                .flatten()
                .map(|(_, b)| b.position)
                .unwrap_or(DVec3::ZERO);

            static NEXT_DRONE_RES_ID: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(-200_000);
            let mut prepared_drones: Vec<PreparedLaunchDrone<'_>> = drone_records
                .into_iter()
                .enumerate()
                .map(|(idx, rec)| {
                    let d_pos = ship_pos
                        + DVec3::new(
                            ((idx as f64) * 50.0).cos() * 40.0,
                            0.0,
                            ((idx as f64) * 50.0).sin() * 40.0,
                        );
                    prepare_launch_drone(ctx, &mut ship_fit_opt, rec, d_pos)
                })
                .collect();

            let mut res_ids: Vec<i64> = (0..prepared_drones.len())
                .map(|_| NEXT_DRONE_RES_ID.fetch_sub(1, std::sync::atomic::Ordering::Relaxed))
                .collect();
            let req_drones: Vec<(i64, DroneSpaceState)> = prepared_drones
                .iter()
                .enumerate()
                .map(|(i, d)| {
                    let rid = res_ids[i];
                    (
                        d.source_item_id,
                        DroneSpaceState {
                            ball_id: rid,
                            drone_item_id: d.rec.item_id,
                            type_id: d.rec.type_id,
                            group_id: d.sde_t.map(|t| t.group_id).unwrap_or(100),
                            owner_char_id: char_id,
                            original_owner_char_id: char_id,
                            parent_ship_id: ship_id,
                            solar_system_id: sys_id,
                            target_id: None,
                            activity_state: 0,
                            assist_char_id: None,
                            guard_char_id: None,
                            is_abandoned: false,
                            is_sentry: d.is_sentry,
                            bandwidth_used: d.bw_used,
                            max_velocity: d.max_vel,
                            optimal_range: d.opt_range,
                            damage_vector: d.dmg_vec,
                            shield_repair_amount: d.sh_rep,
                            armor_repair_amount: d.ar_rep,
                            hull_repair_amount: d.hu_rep,
                            web_speed_factor: d.web_f,
                            neut_amount: d.neut_a,
                            ecm_strength: d.ecm_s,
                            tp_sig_bonus_pct: d.tp_s,
                        },
                    )
                })
                .collect();
            let reserve_req = crate::system_actor::ReserveDroneLaunchesReq {
                ship_id,
                max_drones_allowed,
                max_bandwidth: ship_bw,
                partial_ok: is_v24_launch,
                drones: req_drones,
            };
            let reserve_outcome = match sys_handle.reserve_drone_launches(reserve_req).await {
                Ok(v) => v,
                Err(e) => {
                    return Some(Err(BridgeRpcError {
                        code: "DroneLaunchFailed".into(),
                        message: e,
                        client_error: None,
                    }));
                }
            };
            for &(item_id, code) in &reserve_outcome.per_item_errors {
                per_item_errors.entry(item_id).or_insert(code);
            }
            if let Some(code) = reserve_outcome.rejected_error {
                if is_v24_launch && !reserve_outcome.per_item_errors.is_empty() {
                    return Some(Err(drone_launch_error(code)));
                }
                if code == "MaxBandwidthExceeded" {
                    return Some(Err(BridgeRpcError::from_user_key(
                        eve_proto::UserErrorKey::MaxBandwidthExceeded,
                    )));
                }
                return Some(Err(no_drone_management_error(max_drones_allowed)));
            }
            if is_v24_launch {
                let mut prepared_slots: Vec<Option<PreparedLaunchDrone<'_>>> =
                    prepared_drones.into_iter().map(Some).collect();
                prepared_drones = reserve_outcome
                    .kept_indices
                    .iter()
                    .filter_map(|&idx| prepared_slots.get_mut(idx).and_then(Option::take))
                    .collect();
                res_ids = reserve_outcome.reserved_ids;
            }

            let db_launch = ctx.db.try_with_mut_snapshot(|snap| {
                if let Some(ch) = snap.characters.get(&char_id) {
                    if ch.station_id.is_some() || ch.structure_id.is_some() || ch.solar_system_id != sys_id || ch.active_ship_id != ship_id {
                        return Err(BridgeRpcError::ship_not_in_space(
                            ship_id,
                            Some(char_id),
                            Some(sys_id),
                            "LaunchDrones",
                            format!("Ship {ship_id} is no longer undocked in system {sys_id}"),
                        ));
                    }
                }
                let mut results = Vec::with_capacity(prepared_drones.len());
                for d in &prepared_drones {
                    let rec = &d.rec;
                    let d_pos = d.d_pos;
                    let cur_qty = {
                        let it = snap.items.get(&rec.item_id).ok_or_else(|| BridgeRpcError {
                            code: "DroneNotFound".into(),
                            message: format!("Drone item {} not found", rec.item_id),
                            client_error: None,
                        })?;
                        if it.owner_id != char_id || it.location_id != ship_id || it.flag != InventoryFlag::DroneBay || it.quantity <= 0 {
                            return Err(BridgeRpcError {
                                code: "DroneNotInBay".into(),
                                message: format!("Drone {} is no longer in ship {ship_id}'s drone bay", rec.item_id),
                                client_error: None,
                            });
                        }
                        it.quantity
                    };
                    let actual_item_id = if cur_qty > 1 {
                        let mut split_id = snap.alloc_entity_id();
                        while snap.items.contains_key(&split_id) {
                            split_id = snap.alloc_entity_id();
                        }
                        let mut split_rec = {
                            let it = snap.items.get_mut(&rec.item_id).expect("verified above");
                            it.quantity -= 1;
                            it.version += 1;
                            it.clone()
                        };
                        split_rec.item_id = split_id;
                        split_rec.location_id = sys_id as i64;
                        split_rec.flag = InventoryFlag::None;
                        split_rec.quantity = 1;
                        split_rec.singleton = true;
                        split_rec.version = 1;
                        snap.items.insert(split_id, split_rec);
                        split_id
                    } else {
                        let it = snap.items.get_mut(&rec.item_id).expect("verified above");
                        it.location_id = sys_id as i64;
                        it.flag = InventoryFlag::None;
                        it.singleton = true;
                        it.version += 1;
                        rec.item_id
                    };
                    let bid = actual_item_id;
                    crate::sp6_lifecycle::record_drone_ball_meta_in_snap(snap, actual_item_id, bid);
                    snap.item_positions.insert(actual_item_id, [d_pos.x, d_pos.y, d_pos.z]);
                    results.push((bid, actual_item_id, rec.type_id));
                }
                Ok(results)
            });
            let committed_launches = match db_launch {
                Ok(v) => v,
                Err(e) => {
                    let _ = sys_handle.finalize_drone_launches(Vec::new(), res_ids).await;
                    return Some(Err(e));
                }
            };

            let launch_mappings: Vec<(i64, i64, i64)> = committed_launches
                .iter()
                .enumerate()
                .map(|(i, &(bid, launched_drone_item_id, _))| {
                    (res_ids[i], bid, launched_drone_item_id)
                })
                .collect();
            let _ = sys_handle
                .finalize_drone_launches(launch_mappings, Vec::new())
                .await;

            let mut launched_ball_ids = Vec::with_capacity(prepared_drones.len());
            let mut launched_by_source: HashMap<i64, Vec<i64>> = HashMap::new();
            for (d, (ball_id, _launched_drone_item_id, _)) in prepared_drones.into_iter().zip(committed_launches.into_iter()) {
                let rec = d.rec;
                let sde_t = d.sde_t;
                let mut cs = ctx.build_type_combat_state(rec.type_id);
                if cs.structure_hp <= 0.0 {
                    cs.structure_hp = 200.0;
                    cs.max_structure_hp = 200.0;
                }
                if let Some(eff_hp) = d.eff_hp {
                    cs.structure_hp = eff_hp;
                    cs.max_structure_hp = eff_hp;
                }
                if let Some(eff_sh) = d.eff_sh {
                    cs.shield_hp = eff_sh;
                    cs.max_shield_hp = eff_sh;
                }
                if let Some(eff_ar) = d.eff_ar {
                    cs.armor_hp = eff_ar;
                    cs.max_armor_hp = eff_ar;
                }
                let is_excavator = matches!(rec.type_id, 41030 | 43681 | 41031 | 43682 | 90621 | 90622);
                let mass = if is_excavator { 10_000_000.0 } else { sde_t.map(|t| t.mass).filter(|&m| m > 0.0).unwrap_or(10_000.0) };
                let inertia = if is_excavator { 2.8 } else { sde_t.and_then(|t| t.default_attributes.get(&70).copied()).unwrap_or(0.5).clamp(0.1, 5.0) };
                let radius = if is_excavator { 50.0 } else { sde_t.map(|t| t.radius).filter(|&r| r > 0.0).unwrap_or(10.0) };
                cs.base_max_velocity = Some(d.max_vel);
                cs.base_mass = Some(mass);
                cs.base_inertia = Some(inertia);
                let (pilot_corp_id, pilot_alliance_id) = ctx
                    .db
                    .get_character(char_id)
                    .map(|ch| (ch.corporation_id, ch.alliance_id.unwrap_or(-1)))
                    .unwrap_or((
                        session.corporation_id.unwrap_or(-1),
                        session.alliance_id.unwrap_or(-1),
                    ));
                let drone_meta = eve_destiny::SlimItemMeta {
                    group_id: sde_t.map(|t| t.group_id).unwrap_or(100),
                    category_id: sde_t.map(|t| t.category_id).unwrap_or(18),
                    owner_id: char_id,
                    char_id: None,
                    corp_id: pilot_corp_id,
                    alliance_id: pilot_alliance_id,
                    security_status: 0.0,
                    is_cloaked: false,
                    name: sde_t.map(|t| t.name.clone()).filter(|n| !n.is_empty()),
                    ..Default::default()
                };
                let _ = sys_handle
                    .spawn_ball_with_meta(
                        Ball {
                            id: ball_id,
                            type_id: rec.type_id,
                            position: d.d_pos,
                            velocity: DVec3::ZERO,
                            max_velocity: d.max_vel,
                            inertia,
                            mass,
                            radius,
                            speed_fraction: if d.is_sentry { 0.0 } else { 1.0 },
                            mode: MovementMode::Stop,
                        },
                        cs,
                        drone_meta,
                    )
                    .await;
                let _ = sys_handle
                    .emit_destiny_event(
                        ball_id,
                        eve_destiny::DestinyEvent::OnDroneStateChange {
                            drone_id: ball_id,
                            owner_id: char_id,
                            controller_id: ship_id,
                            activity_state: 0,
                            type_id: rec.type_id,
                            controller_owner_id: char_id,
                            target_id: None,
                        },
                    )
                    .await;

                if let Some(scene) =
                    crate::router::notify_registry::BallparkScene::try_solar_system(sys_id)
                {
                    ctx.emit_typed(
                        NotificationTarget::Character(char_id),
                        OnDroneStateChange {
                            scene,
                            drone_id: ball_id,
                            owner_id: char_id,
                            controller_id: ship_id,
                            activity_state: 0,
                            drone_type_id: rec.type_id as i64,
                            controller_owner_id: char_id,
                            target_id: None,
                        },
                    );
                }
                launched_by_source
                    .entry(d.source_item_id)
                    .or_default()
                    .push(ball_id);
                launched_ball_ids.push(ball_id);
            }
            if is_v24_launch {
                let mut dict_entries = Vec::with_capacity(requested_launches.len());
                for &(source_id, _) in &requested_launches {
                    let mut seq: Vec<PyRep> = launched_by_source
                        .remove(&source_id)
                        .unwrap_or_default()
                        .into_iter()
                        .map(PyRep::Integer)
                        .collect();
                    if let Some(&err_code) = per_item_errors.get(&source_id) {
                        seq.push(PyRep::Tuple(vec![PyRep::String(err_code.to_string())]));
                    }
                    dict_entries.push((PyRep::Integer(source_id), PyRep::List(seq)));
                }
                Some(Ok(PyRep::Dict(dict_entries)))
            } else {
                Some(Ok(PyRep::List(
                    launched_ball_ids.into_iter().map(PyRep::Integer).collect(),
                )))
            }
        }

        ("entity", "CmdReturnBay") => {
            let (char_id, ship_id, sys_id, sys_handle, _) =
                match resolve_uncloaked_drone_ship(ctx, session, "recall drones").await {
                    Ok(v) => v,
                    Err(e) => return Some(Err(e)),
                };
            let filter = extract_entity_drone_filter_ids(args, kwargs);
            if let Err(e) = validate_return_bay_filter(
                ctx,
                char_id,
                ship_id,
                sys_id,
                &sys_handle,
                &filter,
            )
            .await
            {
                return Some(Err(e));
            }
            Some(
                crate::sp6_lifecycle::handle_cmd_return_bay(
                    ctx,
                    session,
                    normalize_forwarded_drone_args(args),
                    kwargs,
                )
                .await,
            )
        }

        ("entity", "CmdReturnHome") | ("entity", "CmdReturnAndOrbit") => {
            let (char_id, ship_id, sys_id, sys_handle, ship_ball) =
                match resolve_uncloaked_drone_ship(ctx, session, "command drones").await {
                    Ok(v) => v,
                    Err(e) => return Some(Err(e)),
                };
            if let Err(e) = crate::services::tourney_service::check_tourney_drone_restricted(ctx, sys_id, char_id) {
                return Some(Err(e));
            }
            let filter = extract_entity_drone_filter_ids(args, kwargs);
            if let Err(e) = validate_entity_drone_presence(
                ctx,
                char_id,
                ship_id,
                sys_id,
                &filter,
                true,
                true,
                false,
                EmptyFilterMode::ControlledInSystem,
            ) {
                return Some(Err(e));
            }
            let filter_drone_ids: Option<&[i64]> = match &filter {
                EntityDroneFilter::Ids(ids) => Some(ids.as_slice()),
                EntityDroneFilter::All => None,
                EntityDroneFilter::Invalid => unreachable!("validated by validate_entity_drone_presence"),
            };
            let ship_pos = ship_ball.position;
            let max_ctrl_range = compute_drone_control_range(ctx, char_id, ship_id);
            let drones: Vec<DroneSpaceState> = sys_handle
                .view()
                .active_drones_in_space
                .values()
                .filter(|d| {
                    d.solar_system_id == sys_id
                        && d.parent_ship_id == ship_id
                        && !d.is_abandoned
                        && filter_drone_ids
                            .map(|f| f.contains(&d.ball_id) || f.contains(&d.drone_item_id))
                            .unwrap_or(true)
                })
                .cloned()
                .collect();
            let mut count = 0;
            for d in drones {
                let Some((_, drone_ball)) = sys_handle.get_ball_snapshot(d.ball_id).await.ok().flatten() else {
                    continue;
                };
                if ship_pos.distance(drone_ball.position) > max_ctrl_range {
                    continue;
                }
                let target_bid = d.ball_id;
                let updated = sys_handle
                    .mutate_active_drones(move |drones, _, _| {
                        let Some(st) = drones.get_mut(&target_bid) else {
                            return (false, false);
                        };
                        if st.is_abandoned || st.solar_system_id != sys_id || st.parent_ship_id != ship_id {
                            return (false, false);
                        }
                        st.target_id = None;
                        st.activity_state = 0;
                        (true, true)
                    })
                    .await
                    .unwrap_or(false);
                if !updated {
                    continue;
                }
                cycle_ops::clear_drone_web_cycle(&sys_handle, d.ball_id).await;
                if d.is_sentry {
                    let _ = sys_handle.set_movement_mode(d.ball_id, MovementMode::Stop).await;
                } else {
                    let _ = sys_handle.set_speed_fraction(d.ball_id, 1.0).await;
                    let _ = sys_handle.set_movement_mode(d.ball_id, MovementMode::Orbit { target_id: ship_id, range: 1_000.0 }).await;
                }
                let _ = sys_handle.emit_destiny_event(d.ball_id, eve_destiny::DestinyEvent::OnDroneStateChange {
                    drone_id: d.ball_id, owner_id: char_id, controller_id: ship_id,
                    activity_state: 0, type_id: d.type_id, controller_owner_id: char_id, target_id: None,
                }).await;
                if let Some(scene) =
                    crate::router::notify_registry::BallparkScene::try_solar_system(sys_id)
                {
                    ctx.emit_typed(
                        NotificationTarget::Character(char_id),
                        OnDroneStateChange {
                            scene,
                            drone_id: d.ball_id,
                            owner_id: char_id,
                            controller_id: ship_id,
                            activity_state: 0,
                            drone_type_id: d.type_id as i64,
                            controller_owner_id: char_id,
                            target_id: None,
                        },
                    );
                }
                crate::services::notify_emitters_space_dungeons::emit_drone_activity_change(ctx, char_id, d.ball_id, 0, None);
                count += 1;
            }
            Some(Ok(PyRep::Integer(count as i64)))
        }

        ("entity", "CmdEngage") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "Session has no active ship".into(),
                    client_error: None,
                }));
            };
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError { code: "NoSolarSystem".into(), message: "Session has no active solar system".into(), client_error: None }));
            };
            if let Err(e) = crate::services::tourney_service::check_tourney_drone_restricted(ctx, sys_id, char_id) {
                return Some(Err(e));
            }
            let (target_id, filter_drones) = match (args.first(), args.get(1)) {
                (Some(PyRep::List(items) | PyRep::Tuple(items)), Some(PyRep::Integer(tid))) => {
                    (*tid, Some(items.iter().filter_map(|it| it.as_i64()).collect::<Vec<_>>()))
                }
                (Some(PyRep::Integer(tid)), Some(PyRep::List(items) | PyRep::Tuple(items))) => {
                    (*tid, Some(items.iter().filter_map(|it| it.as_i64()).collect::<Vec<_>>()))
                }
                (Some(PyRep::Integer(tid)), _) => (*tid, None),
                _ => (0, None),
            };
            let sys_handle = ctx.systems.get_or_spawn(sys_id);
            if session.station_id.is_some() || session.structure_id.is_some() {
                return Some(Err(BridgeRpcError {
                    code: "ShipDocked".into(),
                    message: "Cannot engage drones while docked".into(),
                    client_error: None,
                }));
            }
            if let Err(e) = crate::services::inventory_fitting::fitting_module_cycles::enforce_space_action_cloak_and_warp_guard(
                ctx,
                &sys_handle,
                char_id,
                ship_id,
                "engage drones",
            )
            .await
            {
                return Some(Err(e));
            }
            if sys_handle
                .get_combat_state(ship_id)
                .await
                .ok()
                .flatten()
                .is_some_and(|cs| cs.is_tethered)
            {
                return Some(Err(BridgeRpcError {
                    code: "CantActivateWhileTethered".into(),
                    message: "Cannot engage drones while tethered".into(),
                    client_error: None,
                }));
            }
            let ship_pos = sys_handle
                .get_ball_snapshot(ship_id)
                .await
                .ok()
                .flatten()
                .map(|(_, b)| b.position)
                .unwrap_or(DVec3::ZERO);
            let Some((_, target_ball)) = sys_handle.get_ball_snapshot(target_id).await.ok().flatten() else {
                return Some(Err(BridgeRpcError {
                    code: "TargetNotInSpace".into(),
                    message: format!("Target ball {target_id} is not in space"),
                    client_error: None,
                }));
            };

            let max_ctrl_range = compute_drone_control_range(ctx, char_id, ship_id);
            let target_dist = ship_pos.distance(target_ball.position);
            if target_dist > max_ctrl_range {
                return Some(Err(BridgeRpcError {
                    code: "OutOfDroneControlRange".into(),
                    message: format!(
                        "Target is {target_dist:.1} m away (exceeds drone control range {max_ctrl_range:.1} m)"
                    ),
                    client_error: None,
                }));
            }
            if let Some(mut target_cs) = sys_handle.get_combat_state(target_id).await.ok().flatten() {
                target_cs.sync_action_breaks();
                if target_cs.blocks_targeted_effect() {
                    if target_cs.is_protected() {
                        return Some(Err(BridgeRpcError::user_error("TargetInvulnerable")));
                    }
                    return Some(Err(BridgeRpcError::user_error("CannotTargetCloaked")));
                }
            }
            let locks = sys_handle.get_target_locks(ship_id).await.unwrap_or_default();
            if !locks.iter().any(|l| l.target_id == target_id && l.locked) {
                return Some(Err(BridgeRpcError {
                    code: "TargetNotLocked".into(),
                    message: format!("Target {target_id} is not locked"),
                    client_error: None,
                }));
            }

            let drones: Vec<DroneSpaceState> = sys_handle
                .view()
                .active_drones_in_space
                .values()
                .filter(|d| {
                    d.solar_system_id == sys_id
                        && d.parent_ship_id == ship_id
                        && !d.is_abandoned
                        && filter_drones
                            .as_ref()
                            .map(|f| f.contains(&d.ball_id) || f.contains(&d.drone_item_id))
                            .unwrap_or(true)
                })
                .cloned()
                .collect();

            let mut engaging_drones = Vec::new();
            for d in drones {
                let Some((_, drone_ball)) = sys_handle.get_ball_snapshot(d.ball_id).await.ok().flatten() else {
                    continue;
                };
                if ship_pos.distance(drone_ball.position) > max_ctrl_range {
                    continue;
                }
                engaging_drones.push((d, drone_ball));
            }
            let has_offensive = engaging_drones.iter().any(|(d, _)| d.group_id != 640 && (d.damage_vector.total() > 0.0 || matches!(d.group_id, 639 | 641 | 544 | 545)));
            let has_logi = engaging_drones.iter().any(|(d, _)| d.group_id == 640);
            if has_offensive {
                if let Err(e) = super::crimewatch_timers::evaluate_and_record_pvp_aggression(
                    ctx, char_id, ship_id, sys_id, target_id, target_ball.type_id == 670,
                ) {
                    return Some(Err(e));
                }
            }
            if has_logi {
                if let Err(e) = super::crimewatch_timers::evaluate_and_record_remote_assistance(
                    ctx, char_id, ship_id, sys_id, target_id,
                ) {
                    return Some(Err(e));
                }
            }

            let mut total_dmg_dealt = 0.0;
            let mut total_repaired = 0.0;
            let mut ewar_applied = false;
            let mut target_destroyed = false;
            let victim_char_opt = cycle_ops::resolve_victim_character_for_ball(ctx, target_id);
            let now_unix = ctx.now_unix();

            for (d, drone_ball) in engaging_drones {
                let target_bid = d.ball_id;
                let claim_res = sys_handle
                    .mutate_active_drones(move |drones, _, _| {
                        let Some(st) = drones.get_mut(&target_bid) else {
                            return (false, None);
                        };
                        if st.is_abandoned
                            || st.solar_system_id != sys_id
                            || st.parent_ship_id != ship_id
                            || st.owner_char_id != char_id
                        {
                            return (false, None);
                        }
                        let switched = st.target_id.is_some() && st.target_id != Some(target_id);
                        st.target_id = Some(target_id);
                        st.activity_state = 1;
                        (true, Some(switched))
                    })
                    .await
                    .ok()
                    .flatten();
                let Some(switched_target) = claim_res else {
                    continue;
                };
                if switched_target && d.web_speed_factor < 0.0 {
                    cycle_ops::clear_drone_web_cycle(&sys_handle, d.ball_id).await;
                }
                if !d.is_sentry {
                    let _ = sys_handle.set_speed_fraction(d.ball_id, 1.0).await;
                    let _ = sys_handle.set_movement_mode(d.ball_id, MovementMode::Orbit { target_id, range: 1_500.0 }).await;
                }
                let _ = sys_handle.emit_destiny_event(d.ball_id, eve_destiny::DestinyEvent::OnDroneStateChange {
                    drone_id: d.ball_id, owner_id: char_id, controller_id: ship_id, activity_state: 1,
                    type_id: d.type_id, controller_owner_id: char_id, target_id: Some(target_id),
                }).await;
                if let Some(scene) =
                    crate::router::notify_registry::BallparkScene::try_solar_system(sys_id)
                {
                    ctx.emit_typed(
                        NotificationTarget::Character(char_id),
                        OnDroneStateChange {
                            scene,
                            drone_id: d.ball_id,
                            owner_id: char_id,
                            controller_id: ship_id,
                            activity_state: 1,
                            drone_type_id: d.type_id as i64,
                            controller_owner_id: char_id,
                            target_id: Some(target_id),
                        },
                    );
                }
                crate::services::notify_emitters_space_dungeons::emit_drone_activity_change(ctx, char_id, d.ball_id, 1, Some("attack"));

                let still_engaged = sys_handle.active_drone(d.ball_id).is_some_and(|st| {
                    !st.is_abandoned
                        && st.parent_ship_id == ship_id
                        && st.activity_state == 1
                        && st.target_id == Some(target_id)
                });
                let next_ready = sys_handle.drone_next_ready_unix(d.ball_id).unwrap_or(0);
                if !still_engaged {
                    continue;
                }
                let cycle_ready = now_unix >= next_ready && !target_destroyed;
                let mut this_drone_ewar = false;
                if cycle_ready {
                    if d.group_id == 640 {
                        // Logistic drone (`feat:drones.logistic-drones`)
                        if let Ok(Some(mut tcs)) = sys_handle.get_combat_state(target_id).await {
                            if d.shield_repair_amount > 0.0 {
                                let before = tcs.shield_hp;
                                tcs.shield_hp = (tcs.shield_hp + d.shield_repair_amount).min(tcs.max_shield_hp);
                                total_repaired += (tcs.shield_hp - before).max(0.0);
                            }
                            if d.armor_repair_amount > 0.0 {
                                let before = tcs.armor_hp;
                                tcs.armor_hp = (tcs.armor_hp + d.armor_repair_amount).min(tcs.max_armor_hp);
                                total_repaired += (tcs.armor_hp - before).max(0.0);
                            }
                            let sh_rep = d.shield_repair_amount;
                            let ar_rep = d.armor_repair_amount;
                            let _ = sys_handle
                                .modify_combat_state(target_id, move |live_cs| {
                                    if sh_rep > 0.0 {
                                        live_cs.shield_hp =
                                            (live_cs.shield_hp + sh_rep).min(live_cs.max_shield_hp);
                                    }
                                    if ar_rep > 0.0 {
                                        live_cs.armor_hp =
                                            (live_cs.armor_hp + ar_rep).min(live_cs.max_armor_hp);
                                    }
                                })
                                .await;
                        }
                    } else if matches!(d.group_id, 639 | 641 | 544 | 545) {
                        // EWAR drone (`feat:drones.ewar-drones`)
                        let immune = cycle_ops::is_target_immune_to_drone_ewar(
                            ctx,
                            &sys_handle,
                            target_id,
                            target_ball.type_id,
                        )
                        .await;
                        if !immune {
                            if cycle_ops::apply_drone_web_cycle(
                                ctx,
                                &sys_handle,
                                &d,
                                &drone_ball,
                                target_id,
                                &target_ball,
                            )
                            .await
                            {
                                this_drone_ewar = true;
                            }
                            let ewar_dist = drone_ball.surface_distance(&target_ball);
                            let ewar_max_falloff_range =
                                d.effective_optimal_range(ctx) + d.effective_falloff_range(ctx);
                            if d.neut_amount > 0.0 && ewar_dist <= ewar_max_falloff_range {
                                let neut = d.neut_amount;
                                if sys_handle
                                    .modify_combat_state(target_id, move |tcs| {
                                        tcs.capacitor = (tcs.capacitor - neut).max(0.0);
                                    })
                                    .await
                                    .ok()
                                    .flatten()
                                    .is_some()
                                {
                                    this_drone_ewar = true;
                                }
                            }
                            if d.tp_sig_bonus_pct > 0.0 && ewar_dist <= ewar_max_falloff_range {
                                let tp_pct = d.tp_sig_bonus_pct;
                                let t_ball = target_ball.clone();
                                if sys_handle
                                    .modify_combat_state(target_id, move |tcs| {
                                        tcs.capture_missing_baselines(&t_ball);
                                        let tp_mult = 1.0 + (tp_pct.min(100.0) / 100.0);
                                        tcs.tp_sig_multiplier = Some(
                                            tcs.tp_sig_multiplier.map_or(tp_mult, |prev| prev.max(tp_mult)),
                                        );
                                    })
                                    .await
                                    .ok()
                                    .flatten()
                                    .is_some()
                                {
                                    this_drone_ewar = true;
                                }
                            }
                            if d.ecm_strength > 0.0
                                && cycle_ops::try_apply_drone_ecm_jam(
                                    ctx,
                                    &sys_handle,
                                    &d,
                                    &drone_ball,
                                    target_id,
                                    &target_ball,
                                )
                                .await
                            {
                                this_drone_ewar = true;
                            }
                        } else if d.web_speed_factor < 0.0 {
                            cycle_ops::clear_drone_web_cycle(&sys_handle, d.ball_id).await;
                        }
                        if this_drone_ewar {
                            ewar_applied = true;
                            record_killmail_assist_action(ctx, target_id, char_id, d.type_id, d.type_id, 0.0, true, now_unix);
                        }
                    } else if d.damage_vector.total() > 0.0 {
                        // Combat / Sentry / Gecko / Augmented / Mutated drone (`feat:drones.engage-target`, `feat:drones.gecko-augmented-mutated-drones`, `feat:drones.gecko-subverted-bandwidth`)
                        let target_sig = sys_handle
                            .get_combat_state(target_id)
                            .await
                            .ok()
                            .flatten()
                            .map(|cs| cs.signature_radius)
                            .unwrap_or(target_ball.radius.max(35.0));
                        let hit_mult = d.compute_combat_hit_multiplier(ctx, &drone_ball, &target_ball, target_sig);
                        let scaled_dmg = DamageVector {
                            em: d.damage_vector.em * hit_mult,
                            thermal: d.damage_vector.thermal * hit_mult,
                            kinetic: d.damage_vector.kinetic * hit_mult,
                            explosive: d.damage_vector.explosive * hit_mult,
                        };
                        let cyc_s = d.cycle_duration_secs(ctx).max(1) as f64;
                        if let Ok(Some((app, destroyed, _))) = crate::services::station_structure::structure_lifecycle_rpc::apply_damage_structure_or_ball(ctx, &sys_handle, d.solar_system_id, char_id, target_id, scaled_dmg, cyc_s, now_unix).await {
                            total_dmg_dealt += app;
                            if app > 0.0 {
                                super::combat_npc_ai::record_npc_threat_event(ctx, target_id, d.ball_id, app, false, false);
                            }
                            record_killmail_assist_action(ctx, target_id, char_id, d.type_id, d.type_id, app, false, now_unix);
                            if destroyed {
                                target_destroyed = true;
                            }
                        }
                    }
                }
                let is_ewar_only_drone =
                    matches!(d.group_id, 639 | 641 | 544 | 545) && d.damage_vector.total() <= 0.0;
                let cycle_acted = cycle_ready && (!is_ewar_only_drone || this_drone_ewar);
                let next_cycle_unix = if cycle_acted {
                    now_unix.saturating_add(d.cycle_duration_secs(ctx))
                } else {
                    next_ready
                };
                cycle_ops::finish_engaged_drone_cycle(
                    ctx, &sys_handle, d.ball_id, char_id, ship_id, target_id,
                    target_destroyed, cycle_acted, next_cycle_unix,
                )
                .await;
            }

            if total_dmg_dealt > 0.0 || ewar_applied || target_destroyed {
                cycle_ops::renew_drone_aggression_timers(ctx, char_id, victim_char_opt, now_unix);
            }
            if target_destroyed {
                cycle_ops::disengage_drones_targeting(ctx, sys_id, target_id);
                super::combat_missiles_aoe::handle_destroyed_ball_from_missile_or_aoe(
                    ctx,
                    sys_id,
                    char_id,
                    target_id,
                    target_ball.type_id,
                    target_ball.position,
                    now_unix,
                )
                .await;
            }

            Some(Ok(PyRep::keyval([
                ("targetID", PyRep::Integer(target_id)),
                ("totalDamageDealt", PyRep::Real(total_dmg_dealt)),
                ("totalRepairedHP", PyRep::Real(total_repaired)),
                ("ewarApplied", PyRep::Bool(ewar_applied)),
            ])))
        }

        ("entity", "CmdAssist") | ("entity", "CmdGuard") => {
            let (char_id, ship_id, sys_id, sys_handle, _) =
                match resolve_uncloaked_drone_ship(ctx, session, "command drones").await {
                    Ok(v) => v,
                    Err(e) => return Some(Err(e)),
                };
            let target_char_id = match RpcArgs::new(args, kwargs).req_positive_i64(0, "targetCharID") {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if target_char_id == char_id || is_static_world_object(ctx, sys_id, target_char_id) {
                return Some(Err(if target_char_id == char_id {
                    BridgeRpcError::invalid_target_self(
                        target_char_id,
                        "drone_assist_self",
                        "Invalid drone assist/guard target",
                    )
                } else {
                    BridgeRpcError::invalid_target(
                        Some(target_char_id),
                        None,
                        "Drones",
                        "invalid_assist_target",
                        "Invalid drone assist/guard target",
                    )
                }));
            }
            let drone_filter: Option<Vec<i64>> = match args.get(1) {
                Some(PyRep::List(items)) | Some(PyRep::Tuple(items)) => {
                    Some(items.iter().filter_map(PyRep::as_i64).collect())
                }
                _ => None,
            };
            let is_assist = method == "CmdAssist";
            let drone_filter_for_mut = drone_filter;
            let (updated, drone_events) = sys_handle
                .mutate_active_drones(move |drones, _, _| {
                    let mut count = 0i64;
                    let mut events = Vec::new();
                    for d in drones.values_mut() {
                        if d.solar_system_id == sys_id
                            && !d.is_abandoned
                            && (d.parent_ship_id == ship_id || d.owner_char_id == char_id)
                            && drone_filter_for_mut
                                .as_ref()
                                .map(|ids| ids.contains(&d.ball_id) || ids.contains(&d.drone_item_id))
                                .unwrap_or(true)
                        {
                            d.assist_char_id = is_assist.then_some(target_char_id);
                            d.guard_char_id = (!is_assist).then_some(target_char_id);
                            d.target_id = None;
                            d.activity_state = if is_assist { 6 } else { 7 };
                            count += 1;
                            events.push((d.ball_id, d.type_id));
                        }
                    }
                    (count > 0, (count, events))
                })
                .await
                .unwrap_or((0, Vec::new()));
            let st = if is_assist { 6 } else { 7 };
            for (bid, tid) in drone_events {
                cycle_ops::clear_drone_web_cycle(&sys_handle, bid).await;
                let _ = sys_handle.emit_destiny_event(bid, eve_destiny::DestinyEvent::OnDroneStateChange {
                    drone_id: bid, owner_id: char_id, controller_id: ship_id, activity_state: st,
                    type_id: tid, controller_owner_id: char_id, target_id: Some(target_char_id),
                }).await;
                crate::services::notify_emitters_space_dungeons::emit_drone_activity_change(
                    ctx,
                    char_id,
                    bid,
                    st,
                    Some(if is_assist { "assist" } else { "guard" }),
                );
            }
            Some(Ok(PyRep::keyval([
                ("mode", PyRep::String(method.into())),
                ("assignedToCharID", PyRep::Integer(target_char_id)),
                ("droneCount", PyRep::Integer(updated)),
            ])))
        }

        ("entity", "ChangeDroneSettings") | ("entity", "SetAggressive") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let _ = ctx
                .db
                .with_snapshot(|s| s.characters.get(&char_id).map(|c| c.character_id));
            let rpc = RpcArgs::new(args, kwargs);
            let parse_drone_flag = |idx: usize, kw_name: &str| -> Result<Option<bool>, BridgeRpcError> {
                match rpc.get_raw(idx, kw_name) {
                    Some(PyRep::Real(v)) if (*v - 0.0).abs() < 1e-9 => Ok(Some(false)),
                    Some(PyRep::Real(v)) if (*v - 1.0).abs() < 1e-9 => Ok(Some(true)),
                    Some(PyRep::Real(v)) => Err(BridgeRpcError {
                        code: "InvalidArgument".into(),
                        message: format!(
                            "{kw_name} must be boolean or 0..=1 (got float {v})"
                        ),
                        client_error: None,
                    }),
                    _ => rpc.opt_bool(idx, kw_name),
                }
            };
            let aggressive = match parse_drone_flag(0, "aggressive") {
                Ok(v) => v.unwrap_or(true),
                Err(e) => return Some(Err(e)),
            };
            let focus_fire = match parse_drone_flag(1, "focusFire") {
                Ok(v) => v.unwrap_or(true),
                Err(e) => return Some(Err(e)),
            };
            with_mut_combat_runtime(ctx, |rt| {
                rt.drone_settings.insert(
                    char_id,
                    DroneBehaviorSettings {
                        aggressive,
                        focus_fire,
                    },
                );
            });
            ctx.emit_typed(
                NotificationTarget::Character(char_id),
                OnDroneSettingChanged {
                    aggressive,
                    focus_fire,
                },
            );
            push_typed_session_notification(
                session,
                OnDroneSettingChanged {
                    aggressive,
                    focus_fire,
                },
            );
            Some(Ok(PyRep::keyval([
                ("aggressive", PyRep::Bool(aggressive)),
                ("focusFire", PyRep::Bool(focus_fire)),
            ])))
        }

        ("entity", "CmdAbandonDrone") => {
            let (char_id, ship_id, sys_id, _, _) =
                match resolve_uncloaked_drone_ship(ctx, session, "abandon drones").await {
                    Ok(v) => v,
                    Err(e) => return Some(Err(e)),
                };
            let filter = extract_entity_drone_filter_ids(args, kwargs);
            if let Err(e) = validate_entity_drone_presence(
                ctx,
                char_id,
                ship_id,
                sys_id,
                &filter,
                true,
                true,
                false,
                EmptyFilterMode::ControlledInSystem,
            ) {
                return Some(Err(e));
            }
            Some(
                crate::sp6_lifecycle::handle_cmd_abandon_drone(
                    ctx,
                    session,
                    normalize_forwarded_drone_args(args),
                    kwargs,
                )
                .await,
            )
        }

        ("ship", "ScoopToDroneBay") | ("ship", "ScoopDrone") => {
            Some(cycle_ops::handle_scoop_to_drone_bay(ctx, session, args, kwargs).await)
        }

        ("entity", "CmdReconnectToDrones") => {
            let (char_id, ship_id, sys_id, _, _) = match resolve_uncloaked_drone_ship(
                ctx,
                session,
                "reconnect to drones",
            )
            .await
            {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let filter = extract_entity_drone_filter_ids(args, kwargs);
            if let Err(e) = validate_entity_drone_presence(
                ctx,
                char_id,
                ship_id,
                sys_id,
                &filter,
                true,
                false,
                true,
                EmptyFilterMode::AbandonedInSystem,
            ) {
                return Some(Err(e));
            }
            Some(
                crate::sp6_lifecycle::handle_cmd_reconnect_to_drones(
                    ctx,
                    session,
                    normalize_forwarded_drone_args(args),
                    kwargs,
                )
                .await,
            )
        }

        ("entity", "CmdSalvage") => {
            let (char_id, ship_id, sys_id, sys_handle, _) = match resolve_uncloaked_drone_ship(
                ctx,
                session,
                "command salvage drones",
            )
            .await
            {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let has_salvage_drone = sys_handle.view().active_drones_in_space.values().any(|d| {
                d.solar_system_id == sys_id
                    && d.parent_ship_id == ship_id
                    && !d.is_abandoned
                    && (d.group_id == 1159 || d.type_id == 32787)
            });
            if !has_salvage_drone {
                return Some(Err(BridgeRpcError { code: "NoActiveSalvageDrones".into(), message: "No active Salvage Drones in space".into(), client_error: None }));
            }
            let Some(wreck_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError { code: "TargetRequired".into(), message: "CmdSalvage requires a target wreckID".into(), client_error: None }));
            };
            Some(crate::combat::drones_salvage_tractor::execute_wreck_salvage_auto_repeat(ctx, sys_id, char_id, ship_id, wreck_id, None).await)
        }

        _ => None,
    }
    }).await
}

#[cfg(test)]
#[path = "combat_npcs_drones_tests.rs"]
mod combat_npcs_drones_tests;

#[cfg(test)]
mod combat_npcs_drones_cluster_a_tests;
