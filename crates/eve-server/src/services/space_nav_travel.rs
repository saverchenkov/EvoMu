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

/// Docking-access rule shared with `beyonce.Dock`: explicit ACL denial, or a player-corp owner
/// (corp IDs >= 98,000,000) that is not the pilot's corporation, refuses access.
pub(crate) fn structure_access_allowed(
    snap: &eve_db::WorldStateSnapshot,
    char_id: i64,
    char_corp_id: i64,
    st: &UpwellStructureRecord,
) -> bool {
    let acl_denied = snap
        .structure_docking_acl_denied
        .get(&st.structure_id)
        .is_some_and(|s| s.contains(&char_id));
    !acl_denied && !(st.owner_corp_id >= 98_000_000 && st.owner_corp_id != char_corp_id)
}

/// Re-evaluates the Upwell tether of `ship_id` (`feat:navigation.tether`, `feat:navigation.tether-break`).
///
/// Tethered when within `tetheringRange` of the docking ring (DERIVED: ring approximated by the
/// structure type's SDE radius) of a cored, accessible structure, with no weapons timer, no warp
/// scramble, no target locks, no active module carrying `disallowTethering` (cyno, siege, triage,
/// bastion, command bursts, ...) and not in warp. While tethered the ship is invulnerable
/// (`CombatState::is_invulnerable`) and repaired; DERIVED repair step: 25% of each layer per sync.
pub(crate) async fn sync_tether(ctx: &ServerContext, char_id: i64, sys_id: u32, ship_id: i64) {
    let range_attr = attr_id_by_name(ctx, "tetheringRange");
    let disallow_attr = attr_id_by_name(ctx, "disallowTethering");
    let corp_id = ctx
        .db
        .get_character(char_id)
        .map(|c| c.corporation_id)
        .unwrap_or(0);
    let (anchors, disallow_active) = ctx.db.with_snapshot(|snap| {
        let anchors: Vec<(DVec3, f64)> = snap
            .structures
            .values()
            .filter(|st| {
                st.solar_system_id == sys_id
                    && st.quantum_core_installed
                    && st.state != "Destroyed"
                    && st.state != "Abandoned"
                    && structure_access_allowed(snap, char_id, corp_id, st)
            })
            .filter_map(|st| {
                let ty = ctx.sde.types.get(&st.type_id)?;
                let range = range_attr.and_then(|a| ty.default_attributes.get(&a).copied())?;
                (range > 0.0).then(|| (DVec3::from_array(st.position), ty.radius + range))
            })
            .collect();
        let disallow = snap.items.values().any(|it| {
            it.location_id == ship_id
                && snap.module_active_states.contains(&it.item_id)
                && disallow_attr
                    .and_then(|a| ctx.sde.types.get(&it.type_id)?.default_attributes.get(&a).copied())
                    .is_some_and(|v| v != 0.0)
        });
        (anchors, disallow)
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
    let now = ctx.now_unix();
    let weapons_timer = ctx
        .crimewatch
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .states
        .get(&char_id)
        .is_some_and(|st| st.is_weapon_timer_active(now));
    let scrambled = handle.is_ball_scrambled(ship_id).await.unwrap_or(false);
    let has_locks = handle
        .get_target_locks(ship_id)
        .await
        .map(|l| !l.is_empty())
        .unwrap_or(false);
    let in_range = anchors
        .iter()
        .any(|(pos, reach)| ball.position.distance(*pos) <= *reach);
    let should = in_range
        && !weapons_timer
        && !scrambled
        && !has_locks
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
            .filter(|i| i.location_id == ship_id && matches!(i.type_id, 11370 | 11578))
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
    let coord = |i: usize| args.get(i).and_then(|a| a.as_f64());
    let (Some(x), Some(y), Some(z)) = (coord(0), coord(1), coord(2)) else {
        return Err(err("InvalidArguments", "CmdGotoPoint expects (x, y, z)".into()));
    };
    super::sync_ship_cloak_and_mjfg_from_fitting(ctx, char_id, sys_id, ship_id).await;
    let handle = ctx.systems.get_or_spawn(sys_id);
    let _ = handle.break_gate_cloak(ship_id).await;
    handle
        .goto_point(ship_id, DVec3::new(x, y, z))
        .await
        .map(|tick| PyRep::Integer(tick as i64))
        .map_err(|e| err("DestinyCommandFailed", e))
}

/// Jump-drive plan for one jump (`feat:navigation.jump-drive`).
#[derive(Debug, Clone)]
pub(crate) struct JumpDrivePlan {
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
pub(crate) fn distance_ly(ctx: &ServerContext, a: u32, b: u32) -> f64 {
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
pub(crate) fn plan_jump_drive(
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
    Ok(PyRep::Tuple(vec![
        PyRep::Integer(unix_to_filetime(cd_until)),
        PyRep::Integer(unix_to_filetime(fat_until)),
        PyRep::Integer(unix_to_filetime(last_updated)),
    ]))
}
