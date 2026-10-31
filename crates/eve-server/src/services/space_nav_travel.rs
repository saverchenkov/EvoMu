//! `space_nav_travel` — Upwell tethering, jump-drive range/fuel/fatigue rules and `beyonce.CmdGotoPoint`.
//!
//! Every limit is read from SDE dogma attributes (looked up by attribute name):
//! `tetheringRange`, `disallowTethering`, `canJump`, `jumpDriveRange`, `jumpDriveConsumptionType`,
//! `jumpDriveConsumptionAmount`, `disallowInHighSec`, `jumpFatigueMultiplier`.

#![forbid(unsafe_code)]

use crate::router::{BoundObjectEntry, NotificationTarget, ServerContext};
use eve_db::types::UpwellStructureRecord;
use eve_destiny::{DestinyEvent, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use glam::DVec3;
use std::collections::HashMap;

/// Security status at or above which a system counts as high-sec for jump-drive restrictions.
const HIGH_SEC_THRESHOLD: f64 = 0.45;
/// Jump Activation cooldown cap (EVE wiki `Jump_drives.md:232`).
const JUMP_ACTIVATION_CAP_MIN: f64 = 30.0;

fn err(code: &str, message: String) -> BridgeRpcError {
    BridgeRpcError {
        code: code.into(),
        message,
    }
}

fn unix_to_filetime(u: u64) -> i64 {
    if u == 0 {
        0
    } else {
        (u as i64 + 11_644_473_600) * 10_000_000
    }
}

/// Resolves a dogma attribute ID by its SDE name.
pub(crate) fn attr_id_by_name(ctx: &ServerContext, name: &str) -> Option<eve_sde::AttributeId> {
    ctx.sde
        .attributes
        .iter()
        .find(|(_, a)| a.name == name)
        .map(|(id, _)| *id)
}

/// Reads a type's SDE default attribute by attribute name.
pub(crate) fn type_attr(ctx: &ServerContext, type_id: u32, name: &str) -> Option<f64> {
    let attr_id = attr_id_by_name(ctx, name)?;
    ctx.sde
        .types
        .get(&type_id)
        .and_then(|t| t.default_attributes.get(&attr_id).copied())
}

/// Unified Upwell structure access check shared across docking, tethering, and Ansiblex jump gates.
pub(crate) fn structure_access_allowed(
    snap: &eve_db::WorldStateSnapshot,
    char_id: i64,
    _char_corp_id: i64,
    st: &UpwellStructureRecord,
) -> bool {
    crate::services::station_structure::structure_upwell_ops::evaluate_character_structure_access_in_snap(
        snap,
        st.structure_id,
        char_id,
    )
}

/// Re-evaluates the Upwell tether of `ship_id` (`feat:navigation.tether`, `feat:navigation.tether-break`).
///
/// Tethered when within `tetheringRange` of the docking ring (DERIVED: ring approximated by the
/// structure type's SDE radius) of a cored, accessible structure, with no weapons timer, no warp
/// scramble, no target locks, no active module carrying `disallowTethering` (cyno, siege, triage,
/// bastion, command bursts, ...) and not in warp. While tethered the ship is invulnerable
/// (`CombatState::is_invulnerable`) and repaired; DERIVED repair step: 25% of each layer per sync.
pub async fn sync_tether(ctx: &ServerContext, char_id: i64, sys_id: u32, ship_id: i64) {
    let disallow_attr = attr_id_by_name(ctx, "disallowTethering");
    let now = ctx.now_unix();
    let corp_id = ctx
        .db
        .get_character(char_id)
        .map(|c| c.corporation_id)
        .unwrap_or(0);
    let fit_active_ids: std::collections::HashSet<i64> = ctx
        .ship_fittings
        .read()
        .ok()
        .and_then(|fits| {
            fits.get(&ship_id).map(|fit| {
                fit.modules
                    .values()
                    .filter(|m| matches!(m.state, eve_dogma::ModuleState::Active | eve_dogma::ModuleState::Overloaded))
                    .map(|m| m.item_id)
                    .collect()
            })
        })
        .unwrap_or_default();
    let immobilizing_disallow = crate::services::space_navigation_advanced::check_immobilizing_module_active(
        ctx, ship_id, "tether",
    )
    .is_err()
        || crate::services::pve_crab_beacons::is_ship_crab_linked(ctx, ship_id, now);
    let (anchors, disallow_active, is_piloted) = ctx.db.with_snapshot(|snap| {
        let ext = crate::services::station_structure::structure_upwell_ops::load_ext_state(snap);
        let piloted = char_id > 0
            && snap
                .characters
                .get(&char_id)
                .is_some_and(|c| c.active_ship_id == ship_id && c.station_id.is_none() && c.structure_id.is_none());
        let anchors: Vec<(DVec3, f64)> = snap
            .structures
            .values()
            .filter(|st| {
                st.solar_system_id == sys_id
                    && st.quantum_core_installed
                    && crate::services::station_structure::structure_lifecycle::is_operational(&st.state)
                    && crate::services::station_structure::structure_lifecycle::power_state(ctx, snap, &ext, st, now)
                        != crate::services::station_structure::structure_lifecycle::PowerState::Abandoned
                    && structure_access_allowed(snap, char_id, corp_id, st)
            })
            .filter_map(|st| {
                let range = crate::services::station_structure::structure_lifecycle::tether_range_m(ctx, st.type_id);
                (range > 0.0).then(|| (DVec3::from_array(st.position), range))
            })
            .collect();
        let disallow = immobilizing_disallow
            || snap.items.values().any(|it| {
                if it.location_id != ship_id
                    || (!snap.module_active_states.contains(&it.item_id) && !fit_active_ids.contains(&it.item_id))
                {
                    return false;
                }
                let Some(ty) = ctx.sde.types.get(&it.type_id) else { return false };
                matches!(ty.group_id, 515 | 658 | 842 | 1770)
                    || disallow_attr
                        .and_then(|a| ty.default_attributes.get(&a).copied())
                        .is_some_and(|v| v != 0.0)
            });
        (anchors, disallow, piloted)
    });
    let handle = ctx.systems.get_or_spawn(sys_id);
    let Some(mut cs) = handle.get_combat_state(ship_id).await.ok().flatten() else {
        return;
    };
    if anchors.is_empty() && !cs.is_tethered {
        return;
    }
    let Some((_, ball)) = handle.get_ball_snapshot(ship_id).await.ok().flatten() else {
        return;
    };
    let weapons_timer = ctx
        .crimewatch
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .states
        .get(&char_id)
        .is_some_and(|st| st.is_weapon_timer_active(now) || st.is_pvp_active(now));
    let jump_timer = ctx.db.with_snapshot(|snap| {
        snap.characters.get(&char_id).is_some_and(|c| c.jump_cooldown_until_unix > now)
    });
    let scrambled = handle.is_ball_scrambled(ship_id).await.unwrap_or(false);
    let has_locks = handle
        .get_target_locks(ship_id)
        .await
        .map(|l| !l.is_empty())
        .unwrap_or(false);
    let has_fighters = ctx
        .active_fighters
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .values()
        .any(|f| f.parent_ship_id == ship_id && !f.is_abandoned);
    let in_range = anchors
        .iter()
        .any(|(pos, reach)| ball.position.distance(*pos) <= *reach);
    let should = is_piloted
        && in_range
        && !weapons_timer
        && !jump_timer
        && !scrambled
        && !has_locks
        && !has_fighters
        && !disallow_active
        && !matches!(ball.mode, MovementMode::Warp { .. });
    let tether_toggled = cs.is_tethered != should;
    let mut changed = tether_toggled;
    cs.is_tethered = should;
    if should
        && (cs.shield_hp < cs.max_shield_hp
            || cs.armor_hp < cs.max_armor_hp
            || cs.structure_hp < cs.max_structure_hp
            || cs.capacitor < cs.max_capacitor)
    {
        cs.shield_hp = (cs.shield_hp + cs.max_shield_hp * 0.25).min(cs.max_shield_hp);
        cs.armor_hp = (cs.armor_hp + cs.max_armor_hp * 0.25).min(cs.max_armor_hp);
        cs.structure_hp = (cs.structure_hp + cs.max_structure_hp * 0.25).min(cs.max_structure_hp);
        cs.capacitor = (cs.capacitor + cs.max_capacitor * 0.25).min(cs.max_capacitor);
        changed = true;
    }
    if changed {
        let _ = handle.set_combat_state(ship_id, cs).await;
    }
    if tether_toggled {
        let dbuffs = if should {
            // DERIVED: tether invulnerability dbuff collection (dbuffCollectionID 60) with 1h expiry
            vec![(60, 1.0, unix_to_filetime(now + 3600))]
        } else {
            Vec::new()
        };
        let _ = handle
            .emit_destiny_event(
                ship_id,
                DestinyEvent::OnDbuffUpdated {
                    ball_id: ship_id,
                    dbuffs,
                },
            )
            .await;
    }
}

/// Emits `CloakBall` and `OnShipStateUpdate` to the grid when the fitted cloak toggles (`destiny:CloakBall`, `destiny:OnShipStateUpdate`).
pub(crate) async fn emit_cloak_transition(ctx: &ServerContext, sys_id: u32, ship_id: i64, cloaked: bool) {
    let handle = ctx.systems.get_or_spawn(sys_id);
    let _ = handle
        .emit_destiny_event(
            ship_id,
            DestinyEvent::CloakBall {
                ball_id: ship_id,
                cloaked: u8::from(cloaked),
            },
        )
        .await;
    let time_ft = unix_to_filetime(ctx.now_unix());
    let (new_v, old_v) = if cloaked { (1.0, 0.0) } else { (0.0, 1.0) };
    let module_states: Vec<(i64, i64, u32, i64, f64, f64)> = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .filter(|i| {
                i.location_id == ship_id
                    && ctx.sde.types.get(&i.type_id).is_some_and(|t| t.group_id == 330)
            })
            .map(|i| (i.owner_id, i.item_id, 2, time_ft, new_v, old_v))
            .collect()
    });
    if !module_states.is_empty() {
        let _ = handle
            .emit_destiny_event(ship_id, DestinyEvent::OnShipStateUpdate { module_states })
            .await;
    }
}

/// `beyonce.CmdGotoPoint(x, y, z)`: steer toward an absolute point and emit `GotoPoint`.
pub(crate) async fn handle_cmd_goto_point(
    ctx: &ServerContext,
    session: &SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let sys_id = ServerContext::require_bound_system(bound_entry, super::BEYONCE_SVC)?;
    let ship_id = session
        .ship_id
        .ok_or_else(|| err("NoActiveShip", "Session has no active ship".into()))?;
    let coord = |i: usize| args.get(i).and_then(|a| a.as_f64()).filter(|v| v.is_finite());
    let (Some(x), Some(y), Some(z)) = (coord(0), coord(1), coord(2)) else {
        return Err(err("InvalidArguments", "CmdGotoPoint expects finite (x, y, z)".into()));
    };
    super::sync_ship_cloak_and_mjfg_from_fitting(ctx, char_id, sys_id, ship_id).await;
    let handle = ctx.systems.get_or_spawn(sys_id);
    let _ = crate::services::notify_emitters_batch8::break_gate_cloak_and_notify(ctx, &handle, char_id, ship_id).await;
    let res = handle
        .goto_point(ship_id, DVec3::new(x, y, z))
        .await
        .map(|tick| PyRep::Integer(tick as i64))
        .map_err(|e| err("DestinyCommandFailed", e))?;
    let _ = crate::services::pve_dungeon_runtime::on_dungeon_approach(
        ctx, char_id, sys_id, ship_id, None,
    )
    .await;
    Ok(res)
}

/// Jump-drive plan for one jump (`feat:navigation.jump-drive`).
#[derive(Debug, Clone)]
pub struct JumpDrivePlan {
    pub distance_ly: f64,
    pub fuel_type_id: u32,
    pub fuel_units: i32,
    pub fatigue_multiplier: f64,
}

fn skill_level(ctx: &ServerContext, char_id: i64, skill_name: &str) -> f64 {
    let Some(skill_id) = ctx
        .sde
        .types
        .values()
        .find(|t| t.name == skill_name)
        .map(|t| t.type_id)
    else {
        return 0.0;
    };
    ctx.db.with_snapshot(|snap| {
        snap.skills
            .get(&char_id)
            .and_then(|m| m.get(&skill_id))
            .map(|s| f64::from(s.level.min(5)))
            .unwrap_or(0.0)
    })
}

/// Light-year distance between two systems from SDE galactic coordinates.
pub fn distance_ly(ctx: &ServerContext, a: u32, b: u32) -> f64 {
    let pos = |s: u32| {
        ctx.sde
            .solar_systems
            .get(&s)
            .map(|x| x.galactic_pos_ly)
            .unwrap_or([0.0; 3])
    };
    let (p, q) = (pos(a), pos(b));
    ((p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2) + (p[2] - q[2]).powi(2)).sqrt()
}

/// Validates a jump-drive jump of `ship_type_id` from `cur_sys` to `tgt_sys` and computes its fuel.
///
/// Range = `jumpDriveRange` x (1 + 20% x Jump Drive Calibration); fuel = `jumpDriveConsumptionAmount`
/// x LY x (1 - 10% x Jump Fuel Conservation), rounded up, of type `jumpDriveConsumptionType`
/// (EVE wiki `Jump_drives.md:44-47`). Ships with `disallowInHighSec` cannot jump into high-sec.
pub fn plan_jump_drive(
    ctx: &ServerContext,
    char_id: i64,
    ship_type_id: u32,
    cur_sys: u32,
    tgt_sys: u32,
) -> Result<JumpDrivePlan, BridgeRpcError> {
    if (31_000_000..32_000_000).contains(&cur_sys) || (31_000_000..32_000_000).contains(&tgt_sys) {
        return Err(err(
            "JumpDriveForbiddenInWormholeSpace",
            format!("Jump drives cannot operate in or into wormhole space ({cur_sys} -> {tgt_sys})"),
        ));
    }
    if cur_sys == 30_100_000 || tgt_sys == 30_100_000 {
        return Err(err(
            "JumpDriveForbiddenInZarzakh",
            format!("Jump drives cannot operate in or into Zarzakh ({cur_sys} -> {tgt_sys})"),
        ));
    }
    if ctx.sde.solar_systems.get(&tgt_sys).map(|s| s.region_id) == Some(10_000_070) {
        return Err(err(
            "CannotJumpToPochven",
            format!("Region-wide cyno jammer prevents jump drives from entering Pochven system {tgt_sys}"),
        ));
    }
    if type_attr(ctx, ship_type_id, "canJump").unwrap_or(0.0) == 0.0 {
        return Err(err(
            "ShipHasNoJumpDrive",
            format!("Ship type {ship_type_id} has no jump drive (canJump = 0)"),
        ));
    }
    let tgt_sec = ctx
        .sde
        .solar_systems
        .get(&tgt_sys)
        .map(|s| s.security_status)
        .unwrap_or(0.0);
    if type_attr(ctx, ship_type_id, "disallowInHighSec").unwrap_or(0.0) != 0.0
        && tgt_sec >= HIGH_SEC_THRESHOLD
    {
        return Err(err(
            "JumpDriveHighSecForbidden",
            format!("Ship type {ship_type_id} may not jump into high-sec system {tgt_sys} ({tgt_sec:.2})"),
        ));
    }
    let base_range = type_attr(ctx, ship_type_id, "jumpDriveRange").unwrap_or(0.0);
    let max_range_ly = base_range * (1.0 + 0.20 * skill_level(ctx, char_id, "Jump Drive Calibration"));
    let distance_ly = distance_ly(ctx, cur_sys, tgt_sys);
    if distance_ly > max_range_ly {
        return Err(err(
            "JumpDistanceOutOfRange",
            format!("Distance {distance_ly:.2} LY exceeds jump range {max_range_ly:.2} LY"),
        ));
    }
    let fuel_type_id = type_attr(ctx, ship_type_id, "jumpDriveConsumptionType").unwrap_or(0.0) as u32;
    let per_ly = type_attr(ctx, ship_type_id, "jumpDriveConsumptionAmount").unwrap_or(0.0);
    let conservation = 1.0 - 0.10 * skill_level(ctx, char_id, "Jump Fuel Conservation");
    let fuel_units = (per_ly * distance_ly * conservation).ceil() as i32;
    let fatigue_multiplier = type_attr(ctx, ship_type_id, "jumpFatigueMultiplier").unwrap_or(1.0);
    Ok(JumpDrivePlan {
        distance_ly,
        fuel_type_id,
        fuel_units,
        fatigue_multiplier,
    })
}

/// Consumes `plan.fuel_units` of `plan.fuel_type_id` from the ship's fuel bay / cargo.
pub(crate) fn consume_jump_fuel(
    ctx: &ServerContext,
    ship_id: i64,
    plan: &JumpDrivePlan,
) -> Result<(), BridgeRpcError> {
    use eve_db::InventoryFlag;
    ctx.db.try_with_mut_snapshot(|snap| {
        let ids: Vec<i64> = snap
            .items
            .values()
            .filter(|i| {
                i.location_id == ship_id
                    && i.type_id == plan.fuel_type_id
                    && matches!(i.flag, InventoryFlag::SpecializedFuelBay | InventoryFlag::Cargo)
            })
            .map(|i| i.item_id)
            .collect();
        let avail: i32 = ids.iter().filter_map(|id| snap.items.get(id)).map(|i| i.quantity).sum();
        if avail < plan.fuel_units {
            return Err(err(
                "InsufficientFuel",
                format!(
                    "Jump requires {} units of type {}, only {avail} available",
                    plan.fuel_units, plan.fuel_type_id
                ),
            ));
        }
        let mut rem = plan.fuel_units;
        for id in ids {
            if rem <= 0 {
                break;
            }
            if let Some(item) = snap.items.get_mut(&id) {
                let take = item.quantity.min(rem);
                item.quantity -= take;
                item.version += 1;
                rem -= take;
            }
        }
        snap.items.retain(|_, i| !(i.type_id == plan.fuel_type_id && i.location_id == ship_id && i.quantity <= 0));
        Ok(())
    })
}

/// Current decayed jump fatigue (minutes) for `char_id` at `now`.
pub(crate) fn current_fatigue_min(ctx: &ServerContext, char_id: i64, now: u64) -> f64 {
    ctx.db.with_snapshot(|snap| {
        let raw = snap
            .characters
            .get(&char_id)
            .map(|c| c.jump_fatigue_minutes)
            .unwrap_or(0.0);
        let last = snap.character_last_jump_unix.get(&char_id).copied().unwrap_or(now);
        (raw - now.saturating_sub(last) as f64 / 60.0).max(0.0)
    })
}

/// Applies jump fatigue and the Jump Activation cooldown for a jump of `effective_ly`
/// (`feat:navigation.jump-fatigue`, `notify:OnJumpTimersUpdated`, EVE wiki `Jump_drives.md:215-233`);
/// returns `(cooldown_min, fatigue_min)`.
pub(crate) fn apply_jump_fatigue(ctx: &ServerContext, char_id: i64, effective_ly: f64) -> (f64, f64) {
    let now = ctx.now_unix();
    let before = current_fatigue_min(ctx, char_id, now);
    let (cd, fat) = crate::formulas::compute_jump_fatigue_and_cooldown_minutes(before, effective_ly, 0.0);
    let cd = cd.min(JUMP_ACTIVATION_CAP_MIN);
    let cd_until = now + (cd * 60.0).round() as u64;
    let fat_until = now + (fat * 60.0).round() as u64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.jump_fatigue_minutes = fat;
            ch.jump_cooldown_until_unix = cd_until;
            ch.jump_fatigue_until_unix = fat_until;
        }
        snap.character_last_jump_unix.insert(char_id, now);
    });
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnJumpTimersUpdated",
        "charid",
        vec![
            PyRep::Integer(unix_to_filetime(cd_until)),
            PyRep::Integer(unix_to_filetime(fat_until)),
            PyRep::Integer(unix_to_filetime(now)),
        ],
    );
    (cd, fat)
}

/// Rejects a jump while the Jump Activation cooldown is still running.
pub(crate) fn check_jump_activation(ctx: &ServerContext, char_id: i64) -> Result<(), BridgeRpcError> {
    let now = ctx.now_unix();
    let until = ctx.db.with_snapshot(|snap| {
        snap.characters
            .get(&char_id)
            .map(|c| c.jump_cooldown_until_unix)
            .unwrap_or(0)
    });
    if now < until {
        return Err(err(
            "JumpCooldownActive",
            format!("Jump activation timer active for {} more seconds", until - now),
        ));
    }
    Ok(())
}

/// `jumpTimers.GetTimers(charID)`: returns `(jumpActivation, jumpFatigue, lastUpdated)` in Windows FILETIME ticks
/// as consumed by `CrimewatchService.GetJumpTimers` (`eve/client/script/ui/services/crimewatchSvc.pyj:151`).
pub(crate) fn handle_get_jump_timers(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    if let Some(first) = args.first() {
        let Some(req_char) = first.as_i64() else {
            return Err(err("InvalidArguments", "GetTimers expects optional charID integer".into()));
        };
        if req_char != char_id {
            return Err(err(
                "CharacterNotInSession",
                format!("Requested charID {req_char} does not match session character {char_id}"),
            ));
        }
    }
    let (cd_until, fat_until, last_updated) = ctx.db.with_snapshot(|snap| {
        let (cd, fat) = snap
            .characters
            .get(&char_id)
            .map(|c| (c.jump_cooldown_until_unix, c.jump_fatigue_until_unix))
            .unwrap_or((0, 0));
        let last = snap.character_last_jump_unix.get(&char_id).copied().unwrap_or(0);
        (cd, fat, last)
    });
    Ok(PyRep::keyval([
        ("jumpActivation", PyRep::Integer(unix_to_filetime(cd_until))),
        ("jumpFatigue", PyRep::Integer(unix_to_filetime(fat_until))),
        ("lastUpdated", PyRep::Integer(unix_to_filetime(last_updated))),
    ]))
}

const BEYONCE_SVC: &str = "beyonce";

/// Resolves the in-space position of a navigation target within the specified solar system.
/// Looks up dynamic ballpark balls, SDE stations, SDE stargates, SDE celestials,
/// player structures, and character bookmarks.
pub(crate) async fn resolve_system_target_position(
    ctx: &ServerContext,
    sys_id: u32,
    char_id: i64,
    target_id: i64,
) -> Option<DVec3> {
    if target_id <= 0 {
        return None;
    }
    let handle = ctx.systems.get_or_spawn(sys_id);
    if let Ok(Some((_, tb))) = handle.get_ball_snapshot(target_id).await {
        return Some(tb.position);
    }
    if let Some(sys_rec) = ctx.sde.solar_systems.get(&sys_id) {
        if let Some(st) = sys_rec.stations.iter().find(|st| st.station_id == target_id) {
            return Some(st.position);
        }
    }
    if let Some(st) = ctx.sde.stations.get(&target_id).filter(|st| st.solar_system_id == sys_id) {
        return Some(st.position);
    }
    if let Some(jump) = ctx.sde.jumps.get(&target_id).filter(|j| j.from_solar_system_id == sys_id) {
        return Some(jump.position);
    }
    if let Some(c) = ctx.sde.celestials_in_system(sys_id).into_iter().find(|c| c.celestial_id == target_id) {
        return Some(c.position);
    }
    ctx.db.with_snapshot(|snap| {
        if let Some(b) = snap
            .bookmarks
            .get(&target_id)
            .filter(|b| b.owner_id == char_id && b.solar_system_id == sys_id)
        {
            return Some(DVec3::new(b.x, b.y, b.z));
        }
        if let Some(st) = snap.structures.get(&target_id).filter(|st| st.solar_system_id == sys_id) {
            return Some(DVec3::from_array(st.position));
        }
        None
    })
}

pub(crate) async fn handle_cmd_goto_direction(
    ctx: &ServerContext,
    session: &SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    if session.station_id.is_some() || session.structure_id.is_some() {
        return Err(BridgeRpcError {
            code: "NotInSpace".into(),
            message: "Cannot steer ship while docked".into(),
        });
    }
    let sys_id = ServerContext::require_bound_system(bound_entry, BEYONCE_SVC)?;
    let Some(ship_id) = session.ship_id else {
        return Err(BridgeRpcError {
            code: "NoActiveShip".into(),
            message: "Session has no active ship".into(),
        });
    };
    let x = args.first().and_then(|a| a.as_f64()).unwrap_or(0.0);
    let y = args.get(1).and_then(|a| a.as_f64()).unwrap_or(0.0);
    let z = args.get(2).and_then(|a| a.as_f64()).unwrap_or(0.0);
    if !x.is_finite() || !y.is_finite() || !z.is_finite() {
        return Err(BridgeRpcError {
            code: "InvalidArguments".into(),
            message: "Direction coordinates must be finite".into(),
        });
    }
    let dir = DVec3::new(x, y, z).normalize_or_zero();
    let direction = if dir.length_squared() > 1e-6 { dir } else { DVec3::X };
    let handle = ctx.systems.get_or_spawn(sys_id);
    let Some(_) = handle.get_ball_snapshot(ship_id).await.map_err(|e| BridgeRpcError {
        code: "DestinyCommandFailed".into(),
        message: e,
    })? else {
        return Err(BridgeRpcError {
            code: "ShipNotInSpace".into(),
            message: "Active ship is not spawned in Ballpark".into(),
        });
    };
    super::sync_ship_cloak_and_mjfg_from_fitting(ctx, char_id, sys_id, ship_id).await;
    let _ = crate::services::notify_emitters_batch8::break_gate_cloak_and_notify(ctx, &handle, char_id, ship_id).await;
    handle
        .set_movement_mode(ship_id, MovementMode::GotoDirection { direction })
        .await
        .map(|tick| PyRep::Integer(tick as i64))
        .map_err(|e| BridgeRpcError {
            code: "DestinyCommandFailed".into(),
            message: e,
        })
}

pub(crate) async fn handle_cmd_stop(
    ctx: &ServerContext,
    session: &SessionState,
    bound_entry: Option<&BoundObjectEntry>,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    if session.station_id.is_some() || session.structure_id.is_some() {
        return Err(BridgeRpcError {
            code: "NotInSpace".into(),
            message: "Cannot issue stop command while docked".into(),
        });
    }
    let sys_id = ServerContext::require_bound_system(bound_entry, BEYONCE_SVC)?;
    let Some(ship_id) = session.ship_id else {
        return Err(BridgeRpcError {
            code: "NoActiveShip".into(),
            message: "Session has no active ship".into(),
        });
    };
    let handle = ctx.systems.get_or_spawn(sys_id);
    let Some(_) = handle.get_ball_snapshot(ship_id).await.map_err(|e| BridgeRpcError {
        code: "DestinyCommandFailed".into(),
        message: e,
    })? else {
        return Err(BridgeRpcError {
            code: "ShipNotInSpace".into(),
            message: "Active ship is not spawned in Ballpark".into(),
        });
    };
    super::sync_ship_cloak_and_mjfg_from_fitting(ctx, char_id, sys_id, ship_id).await;
    let res = handle
        .set_movement_mode(ship_id, MovementMode::Stop)
        .await
        .map(|tick| PyRep::Integer(tick as i64))
        .map_err(|e| BridgeRpcError {
            code: "DestinyCommandFailed".into(),
            message: e,
        })?;
    let _ = crate::services::pve_dungeon_runtime::on_dungeon_approach(
        ctx, char_id, sys_id, ship_id, None,
    )
    .await;
    Ok(res)
}

pub(crate) async fn handle_cmd_set_speed_fraction(
    ctx: &ServerContext,
    session: &SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    if session.station_id.is_some() || session.structure_id.is_some() {
        return Err(BridgeRpcError {
            code: "NotInSpace".into(),
            message: "Cannot change speed fraction while docked".into(),
        });
    }
    let sys_id = ServerContext::require_bound_system(bound_entry, BEYONCE_SVC)?;
    let Some(ship_id) = session.ship_id else {
        return Err(BridgeRpcError {
            code: "NoActiveShip".into(),
            message: "Session has no active ship".into(),
        });
    };
    let fraction = args.first().and_then(|a| a.as_f64()).unwrap_or(1.0);
    if !fraction.is_finite() || fraction < 0.0 {
        return Err(BridgeRpcError {
            code: "InvalidSpeedFraction".into(),
            message: "Speed fraction must be a finite non-negative number".into(),
        });
    }
    let handle = ctx.systems.get_or_spawn(sys_id);
    let Some(_) = handle.get_ball_snapshot(ship_id).await.map_err(|e| BridgeRpcError {
        code: "DestinyCommandFailed".into(),
        message: e,
    })? else {
        return Err(BridgeRpcError {
            code: "ShipNotInSpace".into(),
            message: "Active ship is not spawned in Ballpark".into(),
        });
    };
    super::sync_ship_cloak_and_mjfg_from_fitting(ctx, char_id, sys_id, ship_id).await;
    if fraction > 0.0 {
        let _ = crate::services::notify_emitters_batch8::break_gate_cloak_and_notify(ctx, &handle, char_id, ship_id).await;
    }
    handle
        .set_speed_fraction(ship_id, fraction.clamp(0.0, 1.0))
        .await
        .map(|tick| PyRep::Integer(tick as i64))
        .map_err(|e| BridgeRpcError {
            code: "DestinyCommandFailed".into(),
            message: e,
        })
}

pub(crate) async fn handle_cmd_orbit_or_follow(
    ctx: &ServerContext,
    session: &SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    if session.station_id.is_some() || session.structure_id.is_some() {
        return Err(BridgeRpcError {
            code: "NotInSpace".into(),
            message: "Cannot orbit, approach, or keep at range while docked".into(),
        });
    }
    let sys_id = ServerContext::require_bound_system(bound_entry, BEYONCE_SVC)?;
    let Some(ship_id) = session.ship_id else {
        return Err(BridgeRpcError {
            code: "NoActiveShip".into(),
            message: "Session has no active ship".into(),
        });
    };
    let target_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
    if target_id <= 0 {
        return Err(BridgeRpcError {
            code: "InvalidTarget".into(),
            message: "Target ID must be a positive integer".into(),
        });
    }
    if target_id == ship_id {
        return Err(BridgeRpcError {
            code: "InvalidTarget".into(),
            message: "Cannot orbit, approach, or follow own ship".into(),
        });
    }
    let handle = ctx.systems.get_or_spawn(sys_id);
    let Some(_) = handle.get_ball_snapshot(ship_id).await.map_err(|e| BridgeRpcError {
        code: "DestinyCommandFailed".into(),
        message: e,
    })? else {
        return Err(BridgeRpcError {
            code: "ShipNotInSpace".into(),
            message: "Active ship is not spawned in Ballpark".into(),
        });
    };
    let Some(_) = resolve_system_target_position(ctx, sys_id, char_id, target_id).await else {
        return Err(BridgeRpcError {
            code: "TargetNotFound".into(),
            message: format!("Target {target_id} not found in system {sys_id}"),
        });
    };
    let default_range = if method == "CmdFollowBall" {
        0.0
    } else {
        2_500.0
    };
    let range = args.get(1).and_then(|a| a.as_f64()).unwrap_or(default_range);
    if !range.is_finite() || range < 0.0 {
        return Err(BridgeRpcError {
            code: "InvalidRange".into(),
            message: "Orbit or follow range must be a finite non-negative number".into(),
        });
    }
    let mode = if method == "CmdOrbit" {
        MovementMode::Orbit { target_id, range }
    } else {
        MovementMode::Follow { target_id, range }
    };
    super::sync_ship_cloak_and_mjfg_from_fitting(ctx, char_id, sys_id, ship_id).await;
    let _ = crate::services::notify_emitters_batch8::break_gate_cloak_and_notify(ctx, &handle, char_id, ship_id).await;
    let res = handle
        .set_movement_mode(ship_id, mode)
        .await
        .map(|tick| PyRep::Integer(tick as i64))
        .map_err(|e| BridgeRpcError {
            code: "DestinyCommandFailed".into(),
            message: e,
        })?;
    let _ = crate::services::pve_dungeon_runtime::on_dungeon_approach(
        ctx,
        char_id,
        sys_id,
        ship_id,
        Some(target_id),
    )
    .await;
    Ok(res)
}

pub(crate) async fn handle_cmd_align_to(
    ctx: &ServerContext,
    session: &SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    if session.station_id.is_some() || session.structure_id.is_some() {
        return Err(BridgeRpcError {
            code: "NotInSpace".into(),
            message: "Cannot align while docked".into(),
        });
    }
    let sys_id = ServerContext::require_bound_system(bound_entry, BEYONCE_SVC)?;
    let Some(ship_id) = session.ship_id else {
        return Err(BridgeRpcError {
            code: "NoActiveShip".into(),
            message: "Session has no active ship".into(),
        });
    };
    let bookmark_id = kwargs.get("bookmarkID").and_then(|v| v.as_i64());
    let target_id = kwargs
        .get("dstID")
        .and_then(|v| v.as_i64())
        .or_else(|| args.first().and_then(|a| a.as_i64()))
        .unwrap_or(0);
    if bookmark_id.is_none() && target_id <= 0 {
        return Err(BridgeRpcError {
            code: "InvalidTarget".into(),
            message: "Align target ID must be a positive integer".into(),
        });
    }
    if bookmark_id.is_none() && target_id == ship_id {
        return Err(BridgeRpcError {
            code: "InvalidTarget".into(),
            message: "Cannot align to own ship".into(),
        });
    }
    let handle = ctx.systems.get_or_spawn(sys_id);
    let Some((_, current_ball)) = handle.get_ball_snapshot(ship_id).await.map_err(|e| BridgeRpcError {
        code: "DestinyCommandFailed".into(),
        message: e,
    })? else {
        return Err(BridgeRpcError {
            code: "ShipNotInSpace".into(),
            message: "Active ship is not spawned in Ballpark".into(),
        });
    };
    let target_pos_opt = if let Some(bm_id) = bookmark_id {
        if (32_000_000..=32_999_999).contains(&sys_id) {
            return Err(BridgeRpcError {
                code: "CannotWarpInAbyssalSpace".into(),
                message: "Bookmarks and warp navigation are disabled inside Abyssal Deadspace".into(),
            });
        }
        if sys_id == 30_100_000 {
            return Err(BridgeRpcError::user_error("DunBlacklistCannotWarp"));
        }
        ctx.db.with_snapshot(|snap| {
            snap.bookmarks
                .get(&bm_id)
                .filter(|b| b.owner_id == char_id && b.solar_system_id == sys_id)
                .map(|b| DVec3::new(b.x, b.y, b.z))
        })
    } else {
        resolve_system_target_position(ctx, sys_id, char_id, target_id).await
    };
    let Some(target_pos) = target_pos_opt else {
        let reported_target = bookmark_id.unwrap_or(target_id);
        return Err(BridgeRpcError {
            code: "TargetNotFound".into(),
            message: format!("Target {reported_target} not found in system {sys_id}"),
        });
    };
    let dir = (target_pos - current_ball.position).normalize_or_zero();
    let direction = if dir.length_squared() > 1e-6 { dir } else { DVec3::X };
    super::sync_ship_cloak_and_mjfg_from_fitting(ctx, char_id, sys_id, ship_id).await;
    let _ = crate::services::notify_emitters_batch8::break_gate_cloak_and_notify(ctx, &handle, char_id, ship_id).await;
    let _ = handle.set_speed_fraction(ship_id, 1.0).await;
    let res = handle
        .set_movement_mode(ship_id, MovementMode::GotoDirection { direction })
        .await
        .map(|tick| PyRep::Integer(tick as i64))
        .map_err(|e| BridgeRpcError {
            code: "DestinyCommandFailed".into(),
            message: e,
        })?;
    let _ = crate::services::pve_dungeon_runtime::on_dungeon_approach(
        ctx,
        char_id,
        sys_id,
        ship_id,
        (target_id > 0).then_some(target_id),
    )
    .await;
    Ok(res)
}

pub(crate) async fn sync_ship_cloak_and_mjfg_from_fitting(
    ctx: &ServerContext,
    char_id: i64,
    sys_id: u32,
    ship_id: i64,
) {
    let (has_cloak_mod, active_cloak, active_cap_mjfg, db_ship_type_id) =
        ctx.db.with_snapshot(|snap| {
            let mut has_cloak = false;
            let mut active_cloak: Option<(i64, u32)> = None;
            let mut cap_mjfg = false;
            let ship_tid = snap.items.get(&ship_id).map(|i| i.type_id);
            for item in snap.items.values() {
                if item.owner_id == char_id
                    && item.location_id == ship_id
                    && matches!(item.flag as u16, 11..=34 | 125..=132)
                {
                    let is_active = snap.module_active_states.contains(&item.item_id);
                    let is_cloak = ctx
                        .sde
                        .types
                        .get(&item.type_id)
                        .map(|t| t.group_id == 330)
                        .unwrap_or(false);
                    if is_cloak {
                        has_cloak = true;
                        if is_active {
                            active_cloak = Some((item.item_id, item.type_id));
                        }
                    }
                    if item.type_id == 83485 && is_active {
                        cap_mjfg = true;
                    }
                }
            }
            (has_cloak, active_cloak, cap_mjfg, ship_tid)
        });

    sync_tether(ctx, char_id, sys_id, ship_id).await;

    let handle = ctx.systems.get_or_spawn(sys_id);
    let Some(mut cs) = handle.get_combat_state(ship_id).await.ok().flatten() else {
        return;
    };
    if !has_cloak_mod && !active_cap_mjfg && !cs.is_cloaked && !cs.is_capital_mjfg {
        return;
    }
    let Some((_, mut ball)) = handle.get_ball_snapshot(ship_id).await.ok().flatten() else {
        return;
    };

    let mut cs_changed = false;
    let mut ball_changed = false;
    let cloak_before = cs.is_cloaked;
    let ship_type_id = db_ship_type_id.unwrap_or(ball.type_id);

    let in_abyssal = (32_000_000..=32_999_999).contains(&sys_id);
    if let Some((cloak_item_id, cloak_type_id)) = active_cloak.filter(|_| !in_abyssal) {
        let is_covert = ctx
            .sde
            .types
            .get(&cloak_type_id)
            .map(|t| {
                t.effect_ids.iter().any(|&eid| {
                    ctx.sde
                        .effects
                        .get(&eid)
                        .is_some_and(|e| e.name == "cloakingWarpSafe" || e.is_warp_safe)
                })
            })
            .unwrap_or(false);
        let vel_attr_id =
            crate::services::space_navigation_advanced::max_velocity_cloak_attr_id(&ctx.sde);
        let vel_mult = if is_covert {
            1.0
        } else {
            vel_attr_id
                .and_then(|aid| {
                    ctx.sde
                        .types
                        .get(&cloak_type_id)
                        .and_then(|t| t.default_attributes.get(&aid).copied())
                })
                .unwrap_or(1.0)
        };
        let cloaking_skill = ctx
            .character_skill_levels(char_id)
            .get(&11579)
            .copied()
            .unwrap_or(0);
        let targeting_delay_s =
            crate::services::space_navigation_advanced::compute_decloak_targeting_delay_for_module(
                ctx,
                char_id,
                ship_id,
                ship_type_id,
                Some(cloak_item_id),
                cloak_type_id,
                cloaking_skill as u8,
            );
        if !cs.is_cloaked {
            let orig_v = cs.cloak_original_max_velocity.unwrap_or(ball.max_velocity);
            cs.cloak_original_max_velocity = Some(orig_v);
            ball.max_velocity = orig_v * vel_mult;
            cs.is_cloaked = true;
            cs.is_covert_cloak = is_covert;
            cs.cloak_stabilization_remaining_s = 900.0;
            cs.cloak_configured_targeting_delay_s = targeting_delay_s;
            cs_changed = true;
            ball_changed = true;
        } else if (cs.cloak_configured_targeting_delay_s - targeting_delay_s).abs() > 1e-6 {
            cs.cloak_configured_targeting_delay_s = targeting_delay_s;
            cs_changed = true;
        }
    } else if cs.is_cloaked && (cs.gate_cloak_remaining_ticks == 0 || in_abyssal) {
        cs.is_cloaked = false;
        cs.is_covert_cloak = false;
        cs.cloak_stabilization_remaining_s = 0.0;
        cs.decloak_targeting_delay_remaining_s = cs
            .decloak_targeting_delay_remaining_s
            .max(cs.cloak_configured_targeting_delay_s);
        if let Some(orig_v) = cs.cloak_original_max_velocity.take() {
            ball.max_velocity = orig_v;
            ball_changed = true;
        }
        cs_changed = true;
    }

    if active_cap_mjfg && !cs.is_capital_mjfg && cs.mjfg_spool_remaining_s <= 0.0 {
        cs.is_capital_mjfg = true;
        cs.mjfg_spool_remaining_s = 9.0;
        let orig_sig = cs.mjfg_original_sig_radius.unwrap_or(cs.signature_radius);
        cs.mjfg_original_sig_radius = Some(orig_sig);
        cs.signature_radius = orig_sig * 2.5;
        cs_changed = true;
    }

    let cloak_now = cs.is_cloaked;
    if ball_changed {
        let _ = handle.spawn_ball(ball).await;
    }
    if cs_changed {
        let _ = handle.set_combat_state(ship_id, cs).await;
    }
    if cloak_now != cloak_before {
        emit_cloak_transition(ctx, sys_id, ship_id, cloak_now).await;
    }
}

