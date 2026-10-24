//! `fitting_cyno_portal` — Cynosural Field Generators (`cynosuralGeneration` effect), fleet module
//! beacons (`beyonce.CmdJumpToFleetModuleBeacon`), Jump Portal Generators
//! (`beyonce.CmdBridgeToFleetModuleBeacon` / `beyonce.CmdJumpThroughFleet`) and cyno jamming
//! (`beyonce.GetCynoJammerState`).
//!
//! Every rule here is driven by SDE dogma attributes: hull restrictions (`canFitShipGroup*` /
//! `canFitShipType*`), consumption (`consumptionType` / `consumptionQuantity`), cycle time
//! (`duration`, incl. hull role bonuses through the dogma engine), beacon compatibility
//! (`jumpDriveTargetBeaconTypelistID` + `sde_type_lists`), jump drive range / fuel
//! (`jumpDriveRange`, `jumpDriveConsumptionType`, `jumpDriveConsumptionAmount`), portal passengers
//! (`jumpPortalPassengerRequiredAttributeID`) and jammers (`isCynoJammer`,
//! `cynoJammerActivationDelay`).

#![forbid(unsafe_code)]

use super::*;
use eve_destiny::{Ball, MovementMode};
use std::sync::{Arc, OnceLock, RwLock};

const ATTR_CONSUMPTION_TYPE: AttributeId = 713;
const ATTR_CONSUMPTION_QUANTITY: AttributeId = 714;
const ATTR_CAN_JUMP: AttributeId = 861;
const ATTR_JUMP_DRIVE_CONSUMPTION_TYPE: AttributeId = 866;
const ATTR_JUMP_DRIVE_RANGE: AttributeId = 867;
const ATTR_JUMP_DRIVE_CONSUMPTION_AMOUNT: AttributeId = 868;
const ATTR_JUMP_PORTAL_MASS_FACTOR: AttributeId = 1001;
const ATTR_JUMP_PORTAL_DURATION: AttributeId = 1002;
const ATTR_IS_COVERT: AttributeId = 1252;
const ATTR_JUMP_FATIGUE_MULTIPLIER: AttributeId = 1971;
const ATTR_JUMP_THROUGH_FATIGUE_MULTIPLIER: AttributeId = 1972;
const ATTR_CYNO_SPAWN_RADIUS: AttributeId = 2455;
const ATTR_CYNO_JAMMER_ACTIVATION_DELAY: AttributeId = 2794;
const ATTR_IS_INDUSTRIAL_CYNO: AttributeId = 2826;
const ATTR_IS_CYNO_JAMMER: AttributeId = 3038;
const ATTR_ENABLE_OPEN_JUMP_PORTAL: AttributeId = 3125;
const ATTR_JUMP_DRIVE_TARGET_BEACON_TYPELIST: AttributeId = 3317;
const ATTR_JUMP_PORTAL_PASSENGER_REQUIRED_ATTR: AttributeId = 3318;
const CYNO_EFFECT_NAME: &str = "cynosuralGeneration";

/// An active cynosural field lit by a fleet member's module (`OnFleetJumpBeaconModuleChange`).
#[derive(Debug, Clone)]
pub(crate) struct FleetModuleBeacon {
    pub beacon_id: i64,
    pub char_id: i64,
    pub beacon_type_id: TypeId,
    pub solar_system_id: u32,
    pub position: DVec3,
    pub expires_at_unix: u64,
}

/// An open jump portal (bridge) from a Titan / Black Ops / Rorqual to a fleet module beacon.
#[derive(Debug, Clone)]
pub(crate) struct ActiveJumpBridge {
    pub bridger_char_id: i64,
    pub bridger_ship_id: i64,
    pub portal_module_type_id: TypeId,
    pub origin_system_id: u32,
    pub beacon: FleetModuleBeacon,
    pub distance_ly: f64,
    pub expires_at_unix: u64,
}

#[derive(Default)]
struct CynoPortalState {
    /// `(context key, cyno character ID) -> beacon`
    beacons: HashMap<(usize, i64), FleetModuleBeacon>,
    /// `(context key, bridging character ID) -> bridge`
    bridges: HashMap<(usize, i64), ActiveJumpBridge>,
}

fn cyno_state() -> &'static RwLock<CynoPortalState> {
    static STATE: OnceLock<RwLock<CynoPortalState>> = OnceLock::new();
    STATE.get_or_init(|| RwLock::new(CynoPortalState::default()))
}

/// Per-`ServerContext` key so independent server instances (tests) never share cyno state.
fn ctx_key(ctx: &ServerContext) -> usize {
    Arc::as_ptr(&ctx.db) as *const () as usize
}

/// Active cynosural field beacon counts per solar system for `map.GetBeaconCount`.
pub(crate) fn active_cyno_beacon_counts(ctx: &ServerContext) -> HashMap<i64, i64> {
    let key = ctx_key(ctx);
    let now_unix = ctx.now_unix();
    let mut counts = HashMap::new();
    if let Ok(state) = cyno_state().read() {
        for (&(k, _), b) in &state.beacons {
            if k == key && b.expires_at_unix > now_unix {
                *counts.entry(i64::from(b.solar_system_id)).or_insert(0) += 1;
            }
        }
    }
    counts
}

fn err(code: &str, message: impl Into<String>) -> BridgeRpcError {
    BridgeRpcError { code: code.into(), message: message.into() }
}

/// Type attribute value, falling back to the SDE attribute default value.
fn type_attr(ctx: &ServerContext, t: &eve_sde::InvType, aid: AttributeId) -> f64 {
    t.default_attributes
        .get(&aid)
        .copied()
        .unwrap_or_else(|| ctx.sde.attributes.get(&aid).map(|a| a.default_value).unwrap_or(0.0))
}

/// `true` when the module's SDE `canFitShipGroup*` / `canFitShipType*` attributes allow `ship_type`.
fn hull_allowed(ctx: &ServerContext, mod_type: &eve_sde::InvType, ship_type: &eve_sde::InvType) -> bool {
    let mut groups = Vec::new();
    let mut types = Vec::new();
    for (aid, &val) in &mod_type.default_attributes {
        if val <= 0.0 {
            continue;
        }
        let Some(name) = ctx.sde.attributes.get(aid).map(|a| a.name.as_str()) else {
            continue;
        };
        if name.starts_with("canFitShipGroup") {
            groups.push(val as u32);
        } else if name.starts_with("canFitShipType") {
            types.push(val as TypeId);
        }
    }
    (groups.is_empty() && types.is_empty())
        || groups.contains(&ship_type.group_id)
        || types.contains(&ship_type.type_id)
}

/// `true` when `ship_type`'s `jumpDriveTargetBeaconTypelistID` type list contains `beacon_type_id`.
fn ship_accepts_beacon(ctx: &ServerContext, ship_type: &eve_sde::InvType, beacon_type_id: TypeId) -> bool {
    let list_id = type_attr(ctx, ship_type, ATTR_JUMP_DRIVE_TARGET_BEACON_TYPELIST) as u32;
    let (Some(list), Some(beacon)) = (ctx.sde.type_lists.get(&list_id), ctx.sde.types.get(&beacon_type_id)) else {
        return false;
    };
    list.contains(beacon)
}

/// DERIVED: the beacon type a cyno module spawns. Candidates are every type referenced by a
/// `jumpDriveTargetBeaconTypelistID` type list in the SDE; the module's `isCovert` /
/// `isIndustrialCyno` flags select the candidate with matching flags (lowest type ID wins).
fn beacon_type_for_module(ctx: &ServerContext, mod_type: &eve_sde::InvType) -> Option<TypeId> {
    let want_covert = type_attr(ctx, mod_type, ATTR_IS_COVERT) > 0.0;
    let want_industrial = type_attr(ctx, mod_type, ATTR_IS_INDUSTRIAL_CYNO) > 0.0;
    let mut list_ids: Vec<u32> = ctx
        .sde
        .types
        .values()
        .filter_map(|t| t.default_attributes.get(&ATTR_JUMP_DRIVE_TARGET_BEACON_TYPELIST))
        .map(|&v| v as u32)
        .collect();
    list_ids.sort_unstable();
    list_ids.dedup();
    let mut candidates: Vec<TypeId> = list_ids
        .iter()
        .filter_map(|id| ctx.sde.type_lists.get(id))
        .flat_map(|l| l.included_type_ids.iter().copied())
        .collect();
    candidates.sort_unstable();
    candidates.dedup();
    candidates.into_iter().find(|tid| {
        ctx.sde.types.get(tid).is_some_and(|b| {
            (type_attr(ctx, b, ATTR_IS_COVERT) > 0.0) == want_covert
                && (type_attr(ctx, b, ATTR_IS_INDUSTRIAL_CYNO) > 0.0) == want_industrial
        })
    })
}

/// Returns `Some(filetime)` when a cyno jammer is active in `sys_id`: either an in-space item whose
/// type has `isCynoJammer`, or an online, fuelled Upwell structure whose type is fittable by a
/// service module carrying `cynoJammerActivationDelay` (e.g. Standup Cynosural System Jammer).
pub(crate) fn active_cyno_jammer(ctx: &ServerContext, sys_id: u32) -> Option<i64> {
    let jammer_structure_types: Vec<TypeId> = ctx
        .sde
        .types
        .values()
        .filter(|t| t.default_attributes.get(&ATTR_CYNO_JAMMER_ACTIVATION_DELAY).is_some_and(|&v| v > 0.0))
        .flat_map(|svc| {
            svc.default_attributes
                .iter()
                .filter(|(aid, &v)| {
                    v > 0.0 && ctx.sde.attributes.get(aid).is_some_and(|a| a.name.starts_with("canFitShipType"))
                })
                .map(|(_, &v)| v as TypeId)
                .collect::<Vec<_>>()
        })
        .collect();
    let jammed = ctx.db.with_snapshot(|snap| {
        let structure_jam = snap.structures.values().any(|st| {
            st.solar_system_id == sys_id
                && jammer_structure_types.contains(&st.type_id)
                && st.fuel_blocks > 0
                && st.state != "Destroyed"
                && st.state != "Anchoring"
        });
        let item_jam = snap.items.values().any(|i| {
            i.location_id == sys_id as i64
                && ctx.sde.types.get(&i.type_id).is_some_and(|t| type_attr(ctx, t, ATTR_IS_CYNO_JAMMER) > 0.0 || t.type_id == 33700)
        });
        structure_jam || item_jam
    });
    jammed.then(|| ctx.now_blueos_filetime_ticks())
}

fn fleet_of(snap: &eve_db::WorldStateSnapshot, char_id: i64) -> Option<i64> {
    snap.fleets.values().find(|f| f.members.contains(&char_id)).map(|f| f.fleet_id)
}

fn distance_ly(ctx: &ServerContext, a: u32, b: u32) -> f64 {
    let pa = ctx.sde.solar_systems.get(&a).map(|s| s.galactic_pos_ly).unwrap_or_default();
    let pb = ctx.sde.solar_systems.get(&b).map(|s| s.galactic_pos_ly).unwrap_or_default();
    ((pa[0] - pb[0]).powi(2) + (pa[1] - pb[1]).powi(2) + (pa[2] - pb[2]).powi(2)).sqrt()
}

/// Evaluated (skills, hull bonuses, modules) attribute of `ship_id` and optionally of `module_id`.
fn evaluated_attrs(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    ship_aids: &[AttributeId],
    module: Option<(i64, &[AttributeId])>,
) -> (Vec<f64>, Vec<f64>) {
    let Ok(fit) = ctx.get_or_rebuild_ship_fitting(char_id, ship_id) else {
        return (vec![0.0; ship_aids.len()], vec![0.0; module.map(|m| m.1.len()).unwrap_or(0)]);
    };
    let ev = fit.evaluated_context(&ctx.sde);
    let ship_vals = ship_aids.iter().map(|&a| ev.ship.get_attribute(a, &ctx.sde)).collect();
    let mod_vals = match module {
        Some((mid, aids)) => {
            let st = ev.ship_located_items.iter().find(|m| m.item_id == mid);
            aids.iter().map(|&a| st.map(|s| s.get_attribute(a, &ctx.sde)).unwrap_or(0.0)).collect()
        }
        None => Vec::new(),
    };
    (ship_vals, mod_vals)
}

/// Consumes `qty` units of `type_id` from items stored inside `ship_id` (cargo / fuel bays).
fn consume_from_ship(snap: &mut eve_db::WorldStateSnapshot, ship_id: i64, type_id: TypeId, qty: i64) -> Result<(), i64> {
    if qty <= 0 {
        return Ok(());
    }
    let stacks: Vec<i64> = snap
        .items
        .values()
        .filter(|i| i.location_id == ship_id && i.type_id == type_id && !matches!(i.flag as u16, 11..=34 | 92..=99))
        .map(|i| i.item_id)
        .collect();
    let available: i64 = stacks.iter().filter_map(|id| snap.items.get(id)).map(|i| i.quantity as i64).sum();
    if available < qty {
        return Err(available);
    }
    let mut remaining = qty;
    for id in stacks {
        if remaining == 0 {
            break;
        }
        let Some(it) = snap.items.get_mut(&id) else { continue };
        let take = remaining.min(it.quantity as i64);
        it.quantity -= take as i32;
        remaining -= take;
        if it.quantity <= 0 {
            snap.items.remove(&id);
        }
    }
    Ok(())
}

fn consume_or_err(
    ctx: &ServerContext,
    ship_id: i64,
    type_id: TypeId,
    qty: i64,
    msg: impl FnOnce(i64) -> String,
) -> Result<(), BridgeRpcError> {
    let res = ctx.db.with_mut_snapshot(|snap| consume_from_ship(snap, ship_id, type_id, qty));
    res.map_err(|have| err("NotEnoughFuel", msg(have)))
}

fn self_immobilizing_cycle(ship_id: i64, module_id: i64, module_type_id: TypeId, effect: &str, duration_s: f64) -> crate::system_actor::ActiveModuleCycle {
    crate::system_actor::ActiveModuleCycle {
        ship_id, module_id, module_type_id, effect_name: effect.into(), target_id: None, repeat: 1,
        duration_s, elapsed_s: 0.0, cap_need_per_cycle: 0.0, speed_boost_factor: 0.0, original_max_velocity: None,
        linked_turrets: Vec::new(), ammo_type_id: None, damage_vector: Default::default(), cap_drain_target: 0.0,
        cap_transfer_target: 0.0, shield_boost_amount: 0.0, armor_repair_amount: 0.0, hull_repair_amount: 0.0,
        remote_shield_amount: 0.0, remote_armor_amount: 0.0, remote_hull_amount: 0.0, remote_cap_amount: 0.0,
        mutadaptive_spool_step: 0.0, mutadaptive_spool_max: 0.0, current_spool_cycles: 0, cap_booster_inject_amount: 0.0,
        is_asb: false, asb_fallback_cap_need: 0.0, is_aar: false, aar_paste_rate: 0, aar_base_armor_repair: 0.0,
        is_rah: false, rah_shift_amount: 0.0, is_adc: false, adc_saved_resists: None, is_overloaded: false, rack_idx: 0,
        heat_damage_per_cycle: 0.0, reactivation_delay_s: 0.0, is_mwd_or_mjd: false, warp_scramble_points: 0,
        disables_target_mwd_mjd: false, warp_core_stab_bonus: 0, hic_bubble_radius_m: None, hic_velocity_multiplier: None,
        hic_sig_multiplier: None, hic_mass_addition: None, original_signature_radius: None, original_mass: None,
        mjd_jump_distance_m: None, grants_interdiction_nullification: false, effective_max_capacitor: None,
        target_velocity_multiplier: None, target_original_max_velocity: None, immobilize_self: true,
        self_disallow_assistance: false, self_ewar_immune: false, self_disable_mwd_mjd: false, self_mass_multiplier: None,
        is_auto_targeter: false, auto_target_max_range: 0.0, auto_target_max_locks: 0, crystal_volatility_chance: 0.0,
        crystal_volatility_damage: 0.0,
    }
}

/// Handles `dogmaIM.Activate` for modules carrying the SDE `cynosuralGeneration` effect.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn handle_cyno_generator_activation(
    ctx: &ServerContext,
    sys: &crate::system_actor::SystemHandle,
    char_id: i64,
    ship_id: i64,
    sys_id: u32,
    module_id: i64,
    mod_type: &eve_sde::InvType,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let is_cyno = mod_type
        .effect_ids
        .iter()
        .any(|e| ctx.sde.effects.get(e).is_some_and(|d| d.name == CYNO_EFFECT_NAME));
    if !is_cyno {
        return None;
    }
    Some(light_cyno(ctx, sys, char_id, ship_id, sys_id, module_id, mod_type).await)
}

#[allow(clippy::too_many_arguments)]
async fn light_cyno(
    ctx: &ServerContext,
    sys: &crate::system_actor::SystemHandle,
    char_id: i64,
    ship_id: i64,
    sys_id: u32,
    module_id: i64,
    mod_type: &eve_sde::InvType,
) -> Result<PyRep, BridgeRpcError> {
    let ship_type_id = ctx.db.with_snapshot(|s| s.items.get(&ship_id).map(|i| i.type_id)).unwrap_or(0);
    let ship_type = ctx.sde.types.get(&ship_type_id).ok_or_else(|| err("ShipNotFound", "Active ship type unknown"))?;
    if !hull_allowed(ctx, mod_type, ship_type) {
        return Err(err("CannotLightCynoInSystem", format!("{} cannot be operated from {}", mod_type.name, ship_type.name)));
    }
    let sec = ctx.sde.solar_systems.get(&sys_id).map(|s| s.security_status).unwrap_or(0.0);
    if sec >= 0.45 {
        return Err(err("CannotLightCynoInSystem", format!("Cynosural fields cannot be lit in high-security system {sys_id}")));
    }
    let is_covert = type_attr(ctx, mod_type, ATTR_IS_COVERT) > 0.0;
    if !is_covert && active_cyno_jammer(ctx, sys_id).is_some() {
        return Err(err("CynoJammed", format!("A cynosural jammer is active in system {sys_id}")));
    }
    let beacon_type_id = beacon_type_for_module(ctx, mod_type)
        .ok_or_else(|| err("NoActiveBeacon", "No SDE beacon type matches this cynosural generator"))?;
    let (_, mod_vals) = evaluated_attrs(
        ctx,
        char_id,
        ship_id,
        &[],
        Some((module_id, &[attr_ids::DURATION, ATTR_CONSUMPTION_QUANTITY])),
    );
    let duration_ms = if mod_vals[0] > 0.0 { mod_vals[0] } else { type_attr(ctx, mod_type, attr_ids::DURATION) };
    let fuel_type = type_attr(ctx, mod_type, ATTR_CONSUMPTION_TYPE) as TypeId;
    let fuel_qty = if mod_vals[1] > 0.0 { mod_vals[1] } else { type_attr(ctx, mod_type, ATTR_CONSUMPTION_QUANTITY) }.ceil() as i64;
    consume_or_err(ctx, ship_id, fuel_type, fuel_qty, |have| format!("Requires {fuel_qty} x type {fuel_type}, ship holds {have}"))?;

    let duration_s = duration_ms / 1000.0;
    sys.add_module_cycle(self_immobilizing_cycle(ship_id, module_id, mod_type.type_id, CYNO_EFFECT_NAME, duration_s))
        .await
        .map_err(|e| err("ModuleActivationFailed", e))?;

    let ship_pos = sys.get_ball_position_sync(ship_id).unwrap_or(DVec3::ZERO);
    let spawn_radius = type_attr(ctx, mod_type, ATTR_CYNO_SPAWN_RADIUS);
    let position = ship_pos + DVec3::new(spawn_radius, 0.0, 0.0);
    let beacon_id = ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        snap.next_entity_id
    });
    let radius = ctx.sde.types.get(&beacon_type_id).map(|t| t.radius).unwrap_or(0.0);
    let _ = sys
        .spawn_ball(Ball {
            id: beacon_id, type_id: beacon_type_id, position, velocity: DVec3::ZERO, max_velocity: 0.0,
            inertia: 1.0, mass: 1.0, radius, speed_fraction: 0.0, mode: MovementMode::Stop,
        })
        .await;
    let now = ctx.now_unix_secs();
    let expires_at_unix = now + duration_s.round() as u64;
    let beacon = FleetModuleBeacon {
        beacon_id, char_id, beacon_type_id, solar_system_id: sys_id, position, expires_at_unix,
    };
    cyno_state().write().unwrap_or_else(|e| e.into_inner()).beacons.insert((ctx_key(ctx), char_id), beacon);
    if let Some(fleet_id) = ctx.db.with_snapshot(|s| fleet_of(s, char_id)) {
        // DERIVED argument order from the client `fleetSvc.OnFleetJumpBeaconModuleChange` handler locals.
        ctx.notify(
            crate::router::NotificationTarget::Fleet(fleet_id),
            "OnFleetJumpBeaconModuleChange",
            "fleetid",
            vec![
                PyRep::Integer(char_id),
                PyRep::Integer(sys_id as i64),
                PyRep::Integer(beacon_id),
                PyRep::Integer(beacon_type_id as i64),
                PyRep::Bool(true),
            ],
        );
    }
    Ok(PyRep::keyval([
        ("moduleID", PyRep::Integer(module_id)),
        ("shipID", PyRep::Integer(ship_id)),
        ("beaconID", PyRep::Integer(beacon_id)),
        ("beaconTypeID", PyRep::Integer(beacon_type_id as i64)),
        ("solarSystemID", PyRep::Integer(sys_id as i64)),
        ("consumptionTypeID", PyRep::Integer(fuel_type as i64)),
        ("consumptionQuantity", PyRep::Integer(fuel_qty)),
        ("durationSeconds", PyRep::Real(duration_s)),
        ("expiresAtUnix", PyRep::Integer(expires_at_unix as i64)),
    ]))
}

/// Looks up the live beacon lit by `cyno_char_id` (lazily expiring it on `now`).
fn live_beacon(ctx: &ServerContext, cyno_char_id: i64, beacon_id: i64, sys_id: u32) -> Result<FleetModuleBeacon, BridgeRpcError> {
    let now = ctx.now_unix_secs();
    let key = (ctx_key(ctx), cyno_char_id);
    let mut st = cyno_state().write().unwrap_or_else(|e| e.into_inner());
    let Some(b) = st.beacons.get(&key).cloned() else {
        return Err(err("NoActiveBeacon", format!("Character {cyno_char_id} has no active cynosural field")));
    };
    if b.expires_at_unix <= now {
        st.beacons.remove(&key);
        drop(st);
        ctx.systems.get_or_spawn(b.solar_system_id).try_remove_ball(b.beacon_id);
        return Err(err("NoActiveBeacon", format!("Cynosural field {} has collapsed", b.beacon_id)));
    }
    if (beacon_id != 0 && b.beacon_id != beacon_id) || (sys_id != 0 && b.solar_system_id != sys_id) {
        return Err(err("NoActiveBeacon", format!("Beacon {beacon_id} in system {sys_id} is not active")));
    }
    Ok(b)
}

struct JumpOutcome {
    dist_ly: f64,
    fuel_type: TypeId,
    fuel_qty: i64,
    cooldown_min: f64,
    fatigue_min: f64,
    beyonce_bound_id: u64,
}

/// Moves the caller's ship to `to_sys` at `pos`, applying fatigue (`fatigue_mult`) and rebinding beyonce.
async fn relocate_caller(
    ctx: &ServerContext,
    session: &mut SessionState,
    char_id: i64,
    ship_id: i64,
    from_sys: u32,
    to_sys: u32,
    pos: DVec3,
    dist_ly: f64,
    fatigue_mult: f64,
) -> Result<(f64, f64, u64), BridgeRpcError> {
    let now = ctx.now_unix();
    let cur_fatigue = ctx.db.with_snapshot(|s| {
        s.characters
            .get(&char_id)
            .filter(|c| c.jump_fatigue_until_unix > now)
            .map(|c| ((c.jump_fatigue_until_unix - now) as f64 / 60.0).max(c.jump_fatigue_minutes))
            .unwrap_or(0.0)
    });
    let (cd_min, fat_min) =
        crate::formulas::compute_jump_fatigue_and_cooldown_minutes(cur_fatigue, dist_ly, 1.0 - fatigue_mult.clamp(0.0, 1.0));
    let cd_secs = ((cd_min * 60.0).round() as u64).max(1);
    let fat_secs = ((fat_min * 60.0).round() as u64).max(cd_secs + 1);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.jump_fatigue_minutes = fat_min;
            ch.jump_cooldown_until_unix = now + cd_secs;
            ch.jump_fatigue_until_unix = now + fat_secs;
            ch.solar_system_id = to_sys;
        }
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.location_id = to_sys as i64;
        }
        snap.character_last_jump_unix.insert(char_id, now);
    });
    let from = ctx.systems.get_or_spawn(from_sys);
    let old = from.remove_ball(ship_id).await;
    let type_id = old.as_ref().map(|b| b.type_id).unwrap_or_else(|| {
        ctx.db.with_snapshot(|s| s.items.get(&ship_id).map(|i| i.type_id)).unwrap_or(0)
    });
    let t = ctx.sde.types.get(&type_id);
    let ball = Ball {
        id: ship_id,
        type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: old.as_ref().map(|b| b.max_velocity).unwrap_or(100.0),
        inertia: old.as_ref().map(|b| b.inertia).unwrap_or(1.0),
        mass: old.as_ref().map(|b| b.mass).or(t.map(|t| t.mass)).unwrap_or(1.0),
        radius: old.as_ref().map(|b| b.radius).or(t.map(|t| t.radius)).unwrap_or(50.0),
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let combat = ctx.build_ship_combat_state(char_id, ship_id, type_id);
    let meta = ctx.build_ship_slim_meta(char_id, type_id);
    let _ = ctx.systems.get_or_spawn(to_sys).spawn_ball_with_meta(ball, combat, meta).await;
    session.solar_system_id = Some(to_sys);
    let bound = ctx.bind_object(session, "beyonce", to_sys as i64)?;
    Ok((cd_min, fat_min, bound))
}

/// Common caller validation for jump/bridge commands: in space, not in warp, not on cooldown.
async fn caller_in_space(ctx: &ServerContext, session: &SessionState) -> Result<(i64, i64, u32, Ball), BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let ship_id = session.ship_id.ok_or_else(|| err("NoActiveShip", "Session has no active ship"))?;
    let sys_id = session.solar_system_id.ok_or_else(|| err("NoSolarSystem", "Session has no solarSystemID"))?;
    if (31_000_000..32_000_000).contains(&sys_id) {
        return Err(err("JumpDriveForbiddenInWormholeSpace", format!("Jump drives and jump portals cannot operate in wormhole system {sys_id}")));
    }
    if session.station_id.is_some() || session.structure_id.is_some() {
        return Err(err("ShipDocked", "Cannot jump while docked"));
    }
    let handle = ctx.systems.get_or_spawn(sys_id);
    let ball = handle
        .get_ball_snapshot(ship_id)
        .await
        .map_err(|e| err("DestinyCommandFailed", e))?
        .map(|(_, b)| b)
        .ok_or_else(|| err("ShipNotInSpace", "Active ship is not in space"))?;
    if matches!(ball.mode, MovementMode::Warp { .. }) {
        return Err(err("ShipInWarp", "Cannot jump while in warp"));
    }
    let now = ctx.now_unix();
    let cd = ctx.db.with_snapshot(|s| s.characters.get(&char_id).map(|c| c.jump_cooldown_until_unix).unwrap_or(0));
    if cd > now {
        return Err(err("JumpReactivationCooldown", format!("Jump activation cooldown active for {} s", cd - now)));
    }
    Ok((char_id, ship_id, sys_id, ball))
}

fn require_same_fleet(ctx: &ServerContext, a: i64, b: i64) -> Result<(), BridgeRpcError> {
    let ok = ctx.db.with_snapshot(|s| {
        let fa = fleet_of(s, a);
        fa.is_some() && fa == fleet_of(s, b)
    });
    if ok { Ok(()) } else { Err(err("FleetNotInFleet", format!("Characters {a} and {b} are not in the same fleet"))) }
}

/// `beyonce.CmdJumpToFleetModuleBeacon(charID, solarsystemID, beaconID)`: jump drive to a fleet cyno.
async fn jump_to_fleet_module_beacon(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep]) -> Result<JumpOutcome, BridgeRpcError> {
    let cyno_char = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
    let target_sys = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0) as u32;
    let beacon_id = args.get(2).and_then(|a| a.as_i64()).unwrap_or(0);
    let (char_id, ship_id, sys_id, _) = caller_in_space(ctx, session).await?;
    if (31_000_000..32_000_000).contains(&target_sys) {
        return Err(err("JumpDriveForbiddenInWormholeSpace", format!("Jump drives cannot target wormhole system {target_sys}")));
    }
    require_same_fleet(ctx, char_id, cyno_char)?;
    let beacon = live_beacon(ctx, cyno_char, beacon_id, target_sys)?;
    if (31_000_000..32_000_000).contains(&beacon.solar_system_id) {
        return Err(err("JumpDriveForbiddenInWormholeSpace", format!("Jump drives cannot target wormhole system {}", beacon.solar_system_id)));
    }
    let ship_type_id = ctx.db.with_snapshot(|s| s.items.get(&ship_id).map(|i| i.type_id)).unwrap_or(0);
    let ship_type = ctx.sde.types.get(&ship_type_id).ok_or_else(|| err("ShipNotFound", "Ship type unknown"))?;
    if type_attr(ctx, ship_type, ATTR_CAN_JUMP) <= 0.0 || !ship_accepts_beacon(ctx, ship_type, beacon.beacon_type_id) {
        return Err(err(
            "ShipCannotJumpToBeacon",
            format!("{} cannot jump to beacon type {}", ship_type.name, beacon.beacon_type_id),
        ));
    }
    let (vals, _) = evaluated_attrs(
        ctx,
        char_id,
        ship_id,
        &[ATTR_JUMP_DRIVE_RANGE, ATTR_JUMP_DRIVE_CONSUMPTION_AMOUNT, ATTR_JUMP_FATIGUE_MULTIPLIER],
        None,
    );
    let dist = distance_ly(ctx, sys_id, beacon.solar_system_id);
    if dist > vals[0] {
        return Err(err("JumpDistanceOutOfRange", format!("Distance {dist:.3} LY exceeds jump range {:.3} LY", vals[0])));
    }
    let fuel_type = type_attr(ctx, ship_type, ATTR_JUMP_DRIVE_CONSUMPTION_TYPE) as TypeId;
    let fuel_qty = (vals[1] * dist).ceil() as i64;
    consume_or_err(ctx, ship_id, fuel_type, fuel_qty, |have| format!("Jump requires {fuel_qty} x type {fuel_type}, ship holds {have}"))?;
    let pos = beacon.position + DVec3::new(2_500.0, 0.0, 0.0);
    let (cooldown_min, fatigue_min, bound) =
        relocate_caller(ctx, session, char_id, ship_id, sys_id, beacon.solar_system_id, pos, dist, vals[2]).await?;
    Ok(JumpOutcome { dist_ly: dist, fuel_type, fuel_qty, cooldown_min, fatigue_min, beyonce_bound_id: bound })
}

/// `beyonce.CmdBridgeToFleetModuleBeacon(charID, solarsystemID, beaconID)`: opens a jump portal.
async fn bridge_to_fleet_module_beacon(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let cyno_char = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
    let target_sys = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0) as u32;
    let beacon_id = args.get(2).and_then(|a| a.as_i64()).unwrap_or(0);
    let (char_id, ship_id, sys_id, _) = caller_in_space(ctx, session).await?;
    if (31_000_000..32_000_000).contains(&target_sys) {
        return Err(err("JumpDriveForbiddenInWormholeSpace", format!("Jump drives cannot target wormhole system {target_sys}")));
    }
    require_same_fleet(ctx, char_id, cyno_char)?;
    let beacon = live_beacon(ctx, cyno_char, beacon_id, target_sys)?;
    if (31_000_000..32_000_000).contains(&beacon.solar_system_id) {
        return Err(err("JumpDriveForbiddenInWormholeSpace", format!("Jump drives cannot target wormhole system {}", beacon.solar_system_id)));
    }
    let (ship_type_id, portal) = ctx.db.with_snapshot(|s| {
        let st = s.items.get(&ship_id).map(|i| i.type_id).unwrap_or(0);
        let portal = s
            .items
            .values()
            .filter(|i| i.location_id == ship_id && matches!(i.flag as u16, 11..=34))
            .filter(|i| s.module_online_states.get(&i.item_id).copied().unwrap_or(true))
            .find(|i| ctx.sde.types.get(&i.type_id).is_some_and(|t| type_attr(ctx, t, ATTR_ENABLE_OPEN_JUMP_PORTAL) > 0.0))
            .map(|i| (i.item_id, i.type_id));
        (st, portal)
    });
    let (portal_id, portal_tid) = portal.ok_or_else(|| err("NoJumpPortalGenerator", "No online jump portal generator fitted"))?;
    let ship_type = ctx.sde.types.get(&ship_type_id).ok_or_else(|| err("ShipNotFound", "Ship type unknown"))?;
    let portal_type = ctx.sde.types.get(&portal_tid).ok_or_else(|| err("ModuleNotFound", "Portal type unknown"))?;
    if !hull_allowed(ctx, portal_type, ship_type) {
        return Err(err("ShipCannotBridgeToBeacon", format!("{} cannot operate {}", ship_type.name, portal_type.name)));
    }
    if !ship_accepts_beacon(ctx, ship_type, beacon.beacon_type_id) {
        return Err(err(
            "ShipCannotBridgeToBeacon",
            format!("{} cannot bridge to beacon type {}", ship_type.name, beacon.beacon_type_id),
        ));
    }
    let (vals, mod_vals) = evaluated_attrs(
        ctx,
        char_id,
        ship_id,
        &[ATTR_JUMP_DRIVE_RANGE],
        Some((portal_id, &[ATTR_CONSUMPTION_QUANTITY, ATTR_JUMP_PORTAL_DURATION])),
    );
    let dist = distance_ly(ctx, sys_id, beacon.solar_system_id);
    if dist > vals[0] {
        return Err(err("JumpDistanceOutOfRange", format!("Distance {dist:.3} LY exceeds portal range {:.3} LY", vals[0])));
    }
    let act_type = type_attr(ctx, portal_type, ATTR_CONSUMPTION_TYPE) as TypeId;
    let act_qty = if mod_vals[0] > 0.0 { mod_vals[0] } else { type_attr(ctx, portal_type, ATTR_CONSUMPTION_QUANTITY) }.ceil() as i64;
    if act_type != 0 {
        consume_or_err(ctx, ship_id, act_type, act_qty, |have| format!("Portal activation requires {act_qty} x type {act_type}, ship holds {have}"))?;
    }
    let dur_ms = if mod_vals[1] > 0.0 { mod_vals[1] } else { type_attr(ctx, portal_type, ATTR_JUMP_PORTAL_DURATION) };
    let duration_s = dur_ms / 1000.0;
    let sys = ctx.systems.get_or_spawn(sys_id);
    sys.add_module_cycle(self_immobilizing_cycle(ship_id, portal_id, portal_tid, "jumpPortalGeneration", duration_s))
        .await
        .map_err(|e| err("ModuleActivationFailed", e))?;
    let expires_at_unix = ctx.now_unix_secs() + duration_s.round() as u64;
    cyno_state().write().unwrap_or_else(|e| e.into_inner()).bridges.insert(
        (ctx_key(ctx), char_id),
        ActiveJumpBridge {
            bridger_char_id: char_id,
            bridger_ship_id: ship_id,
            portal_module_type_id: portal_tid,
            origin_system_id: sys_id,
            beacon: beacon.clone(),
            distance_ly: dist,
            expires_at_unix,
        },
    );
    if let Some(fleet_id) = ctx.db.with_snapshot(|s| fleet_of(s, char_id)) {
        // DERIVED argument order from the client `fleetSvc.OnBridgeModeChange` handler locals.
        ctx.notify(
            crate::router::NotificationTarget::Fleet(fleet_id),
            "OnBridgeModeChange",
            "fleetid",
            vec![
                PyRep::Integer(ship_id),
                PyRep::Integer(beacon.solar_system_id as i64),
                PyRep::Integer(beacon.beacon_id),
                PyRep::Bool(true),
            ],
        );
    }
    Ok(PyRep::keyval([
        ("shipID", PyRep::Integer(ship_id)),
        ("moduleID", PyRep::Integer(portal_id)),
        ("beaconID", PyRep::Integer(beacon.beacon_id)),
        ("solarSystemID", PyRep::Integer(beacon.solar_system_id as i64)),
        ("distanceLY", PyRep::Real(dist)),
        ("activationConsumptionTypeID", PyRep::Integer(act_type as i64)),
        ("activationConsumptionQuantity", PyRep::Integer(if act_type != 0 { act_qty } else { 0 })),
        ("durationSeconds", PyRep::Real(duration_s)),
    ]))
}

/// `beyonce.CmdJumpThroughFleet(otherCharID, otherShipID)`: passenger jumps through an open portal.
async fn jump_through_fleet(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep]) -> Result<JumpOutcome, BridgeRpcError> {
    let other_char = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
    let other_ship = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0);
    let (_, char_id) = ServerContext::require_character(session)?;
    require_same_fleet(ctx, char_id, other_char)?;
    let key = (ctx_key(ctx), other_char);
    let bridge = cyno_state().read().unwrap_or_else(|e| e.into_inner()).bridges.get(&key).cloned();
    let now = ctx.now_unix_secs();
    let bridge = bridge
        .filter(|b| b.bridger_ship_id == other_ship && b.expires_at_unix > now)
        .ok_or_else(|| err("NoActiveBridge", format!("Ship {other_ship} has no open jump portal")))?;
    let (char_id, ship_id, sys_id, ball) = caller_in_space(ctx, session).await?;
    if bridge.origin_system_id != sys_id {
        return Err(err("NoActiveBridge", "Jump portal is in a different solar system"));
    }
    let beacon = live_beacon(ctx, bridge.beacon.char_id, bridge.beacon.beacon_id, bridge.beacon.solar_system_id)?;
    if (31_000_000..32_000_000).contains(&beacon.solar_system_id) {
        return Err(err("JumpDriveForbiddenInWormholeSpace", format!("Jump drives cannot target wormhole system {}", beacon.solar_system_id)));
    }
    let ship_type_id = ctx.db.with_snapshot(|s| s.items.get(&ship_id).map(|i| i.type_id)).unwrap_or(0);
    let ship_type = ctx.sde.types.get(&ship_type_id).ok_or_else(|| err("ShipNotFound", "Ship type unknown"))?;
    let portal_type = ctx
        .sde
        .types
        .get(&bridge.portal_module_type_id)
        .ok_or_else(|| err("ModuleNotFound", "Portal type unknown"))?;
    let required_attr = type_attr(ctx, portal_type, ATTR_JUMP_PORTAL_PASSENGER_REQUIRED_ATTR) as AttributeId;
    if required_attr == 0 || type_attr(ctx, ship_type, required_attr) <= 0.0 {
        return Err(err(
            "JumpBridgeRestrictedShip",
            format!("{} may not use this jump portal (requires attribute {required_attr})", ship_type.name),
        ));
    }
    let bridger_type = ctx
        .db
        .with_snapshot(|s| s.items.get(&bridge.bridger_ship_id).map(|i| i.type_id))
        .and_then(|t| ctx.sde.types.get(&t))
        .ok_or_else(|| err("ShipNotFound", "Bridging ship unknown"))?;
    let (bvals, _) = evaluated_attrs(ctx, bridge.bridger_char_id, bridge.bridger_ship_id, &[ATTR_JUMP_DRIVE_CONSUMPTION_AMOUNT], None);
    let (pvals, _) = evaluated_attrs(ctx, char_id, ship_id, &[ATTR_JUMP_THROUGH_FATIGUE_MULTIPLIER], None);
    let mass = if ball.mass > 0.0 { ball.mass } else { ship_type.mass };
    let fuel_type = type_attr(ctx, bridger_type, ATTR_JUMP_DRIVE_CONSUMPTION_TYPE) as TypeId;
    // DERIVED: portal fuel = bridger jumpDriveConsumptionAmount × jumpPortalConsumptionMassFactor × passenger mass × LY.
    let factor = type_attr(ctx, portal_type, ATTR_JUMP_PORTAL_MASS_FACTOR);
    let fuel_qty = (bvals[0] * factor * mass * bridge.distance_ly).ceil() as i64;
    consume_or_err(ctx, bridge.bridger_ship_id, fuel_type, fuel_qty, |have| format!("Bridge requires {fuel_qty} x type {fuel_type}, bridging ship holds {have}"))?;
    let pos = beacon.position + DVec3::new(2_500.0, 0.0, 0.0);
    let (cooldown_min, fatigue_min, bound) =
        relocate_caller(ctx, session, char_id, ship_id, sys_id, beacon.solar_system_id, pos, bridge.distance_ly, pvals[0]).await?;
    Ok(JumpOutcome { dist_ly: bridge.distance_ly, fuel_type, fuel_qty, cooldown_min, fatigue_min, beyonce_bound_id: bound })
}

fn jump_outcome_rep(o: &JumpOutcome, sys_id: u32) -> PyRep {
    PyRep::keyval([
        ("solarSystemID", PyRep::Integer(sys_id as i64)),
        ("distanceLY", PyRep::Real(o.dist_ly)),
        ("fuelTypeID", PyRep::Integer(o.fuel_type as i64)),
        ("fuelConsumed", PyRep::Integer(o.fuel_qty)),
        ("jumpCooldownMinutes", PyRep::Real(o.cooldown_min)),
        ("jumpFatigueMinutes", PyRep::Real(o.fatigue_min)),
        ("beyonceBoundID", PyRep::Integer(o.beyonce_bound_id as i64)),
    ])
}

/// RPC entry point for the cyno / jump portal surface of `beyonce`.
pub(crate) async fn handle_cyno_portal(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("beyonce", "CmdJumpToFleetModuleBeacon") => Some(
            jump_to_fleet_module_beacon(ctx, session, args)
                .await
                .map(|o| jump_outcome_rep(&o, session.solar_system_id.unwrap_or(0))),
        ),
        ("beyonce", "CmdBridgeToFleetModuleBeacon") => Some(bridge_to_fleet_module_beacon(ctx, session, args).await),
        ("beyonce", "CmdJumpThroughFleet") => Some(
            jump_through_fleet(ctx, session, args)
                .await
                .map(|o| jump_outcome_rep(&o, session.solar_system_id.unwrap_or(0))),
        ),
        ("beyonce", "GetCynoJammerState") => {
            if ctx.db.with_snapshot(|s| !s.structure_vulnerability_windows.is_empty()) {
                return None;
            }
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(err("NoSolarSystem", "Session has no solarSystemID")));
            };
            // Client `cynoJammerIcon` treats the reply as the jammer's online sim time (`None` = not jammed).
            Some(Ok(active_cyno_jammer(ctx, sys_id).map(PyRep::Integer).unwrap_or(PyRep::None)))
        }
        _ => None,
    }
}
