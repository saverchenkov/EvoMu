//! `fitting_cyno_portal` — Cynosural Field Generators (`cynosuralGeneration` effect), fleet module
//! beacons (`beyonce.CmdJumpToFleetModuleBeacon`), Jump Portal Generators
//! (`beyonce.CmdBridgeToFleetModuleBeacon` / `beyonce.CmdJumpThroughFleet`), Conduit Jumps,
//! and cyno jamming (`beyonce.GetCynoJammerState`).
//!
// parity: rpc:beyonce.CmdJumpToFleetDeployableBeacon, feat:modules.industrial-cyno-rules
// parity: feat:navigation.jump-arrival-placement, feat:navigation.jump-drive-range-fuel

#![forbid(unsafe_code)]

use super::*;
use eve_destiny::{Ball, MovementMode};
use std::sync::{OnceLock, RwLock};

#[path = "fitting_conduit_jump.rs"]
mod fitting_conduit_jump;
pub use fitting_conduit_jump::*;

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
const ATTR_CONDUIT_CONSUMPTION: AttributeId = 3131;
const ATTR_JUMP_DRIVE_TARGET_BEACON_TYPELIST: AttributeId = 3317;
const ATTR_JUMP_PORTAL_PASSENGER_REQUIRED_ATTR: AttributeId = 3318;
const ATTR_IS_INDY_CONDUIT_PASSENGER: AttributeId = 3324;
const CYNO_EFFECT_NAME: &str = "cynosuralGeneration";

/// An active cynosural field lit by a fleet member's module (`OnFleetJumpBeaconModuleChange`).
#[derive(Debug, Clone)]
pub(crate) struct FleetModuleBeacon {
    pub beacon_id: i64,
    pub char_id: i64,
    pub ship_id: i64,
    pub beacon_type_id: TypeId,
    pub solar_system_id: u32,
    pub position: DVec3,
    pub expires_at_unix: u64,
}

pub fn is_ship_cyno_active(ctx: &ServerContext, ship_id: i64) -> bool {
    sweep_expired_cyno_beacons(ctx);
    let key = ctx_key(ctx);
    let now_unix = ctx.now_unix_secs();
    if let Ok(state) = cyno_state().read() {
        for (&(k, _), b) in &state.beacons {
            if k == key && b.ship_id == ship_id && b.expires_at_unix > now_unix {
                return true;
            }
        }
        for (&(k, _), br) in &state.bridges {
            if k == key && br.bridger_ship_id == ship_id && br.expires_at_unix > now_unix {
                return true;
            }
        }
    }
    false
}

/// An open jump portal (bridge) from a Titan / Black Ops / Rorqual to a fleet module beacon.
#[derive(Debug, Clone)]
pub(crate) struct ActiveJumpBridge {
    pub bridger_char_id: i64,
    pub bridger_ship_id: i64,
    pub portal_module_id: i64,
    pub portal_module_type_id: TypeId,
    pub origin_system_id: u32,
    pub beacon: FleetModuleBeacon,
    pub distance_ly: f64,
    pub expires_at_unix: u64,
    pub portal_ball_id: i64,
}

#[derive(Default)]
struct CynoPortalState {
    beacons: HashMap<(usize, i64), FleetModuleBeacon>,
    destroyed_beacons: HashMap<(usize, i64), FleetModuleBeacon>,
    bridges: HashMap<(usize, i64), ActiveJumpBridge>,
}

pub fn is_covert_beacon_type(beacon_type_id: TypeId) -> bool {
    matches!(beacon_type_id, 28646 | 28650 | 59630 | 59631)
}

pub(super) fn effective_beacon_field_type(ctx: &ServerContext, beacon_type_id: TypeId) -> TypeId {
    match beacon_type_id {
        56488 | 59630 => 59631,
        56099 | 57319 | 58906 => 57425,
        other => {
            if let Some(t) = ctx.sde.types.get(&other) {
                if t.group_id == 4093 {
                    return if type_attr(ctx, t, ATTR_IS_COVERT) > 0.0 {
                        59631
                    } else {
                        57425
                    };
                }
            }
            other
        }
    }
}

pub fn on_cyno_ship_destroyed(ctx: &ServerContext, ship_id: i64) {
    let key = ctx_key(ctx);
    let now = ctx.now_unix_secs();
    let mut st = cyno_state().write().unwrap_or_else(|e| e.into_inner());
    st.destroyed_beacons.retain(|_, b| b.expires_at_unix > now);
    let mut removed_bridges = Vec::new();
    st.bridges.retain(|&(k, _), br| {
        if k == key && br.bridger_ship_id == ship_id {
            removed_bridges.push((br.origin_system_id, br.bridger_ship_id, br.portal_module_id, br.portal_ball_id));
            false
        } else {
            true
        }
    });
    let matched: Vec<(usize, i64)> = st
        .beacons
        .iter()
        .filter(|&(&(k, _), b)| k == key && b.ship_id == ship_id && b.expires_at_unix > now)
        .map(|(&k, _)| k)
        .collect();
    for k in matched {
        if let Some(mut b) = st.beacons.remove(&k) {
            b.expires_at_unix = now + 15;
            st.destroyed_beacons.insert(k, b);
        }
    }
    drop(st);
    for (sys_id, bridger_ship_id, portal_mod_id, ball_id) in removed_bridges {
        let sys = ctx.systems.get_or_spawn(sys_id);
        if ball_id != 0 {
            sys.try_remove_ball(ball_id);
        }
        if portal_mod_id != 0 {
            sys.try_remove_module_cycle(bridger_ship_id, portal_mod_id);
        }
    }
}

fn cyno_state() -> &'static RwLock<CynoPortalState> {
    static STATE: OnceLock<RwLock<CynoPortalState>> = OnceLock::new();
    STATE.get_or_init(|| RwLock::new(CynoPortalState::default()))
}

fn ctx_key(ctx: &ServerContext) -> usize {
    ctx.instance_id as usize
}

/// Active cynosural field beacon counts per solar system for `map.GetBeaconCount` (excludes covert cynos).
pub(crate) fn active_cyno_beacon_counts(ctx: &ServerContext) -> HashMap<i64, i64> {
    let key = ctx_key(ctx);
    let now_unix = ctx.now_unix();
    let mut counts = HashMap::new();
    if let Ok(state) = cyno_state().read() {
        for (&(k, _), b) in &state.beacons {
            if k == key && b.expires_at_unix > now_unix && !is_covert_beacon_type(b.beacon_type_id) {
                *counts.entry(i64::from(b.solar_system_id)).or_insert(0) += 1;
            }
        }
    }
    counts
}

fn err(code: &str, message: impl Into<String>) -> BridgeRpcError {
    BridgeRpcError { code: code.into(), message: message.into() }
}

fn type_attr(ctx: &ServerContext, t: &eve_sde::InvType, aid: AttributeId) -> f64 {
    t.default_attributes
        .get(&aid)
        .copied()
        .unwrap_or_else(|| ctx.sde.attributes.get(&aid).map(|a| a.default_value).unwrap_or(0.0))
}



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

pub fn active_cyno_jammer(ctx: &ServerContext, sys_id: u32) -> Option<i64> {
    if sys_id == 30_100_000
        || ctx.sde.solar_systems.get(&sys_id).map(|s| s.region_id) == Some(10_000_070)
    {
        return Some(ctx.now_blueos_filetime_ticks());
    }
    if let Some(effects) = crate::services::pve_special_sites::get_incursion_effects_for_system(ctx, sys_id) {
        if effects.cyno_jammed {
            return Some(ctx.now_blueos_filetime_ticks());
        }
    }
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
    let now = ctx.now_unix();
    let jammed = ctx.db.with_snapshot(|snap| {
        let tenebrex_jam =
            crate::services::sov_gameplay::is_tenebrex_jammer_active_in_snap(snap, sys_id, now);
        let other_structure_jam = snap.structures.values().any(|st| {
            st.solar_system_id == sys_id
                && st.type_id != crate::services::sov_gameplay::TYPE_TENEBREX_CYNO_JAMMER
                && jammer_structure_types.contains(&st.type_id)
                && st.fuel_blocks > 0
                && matches!(st.state.as_str(), "ShieldVulnerable" | "Online")
        });
        let item_jam = snap.items.values().any(|i| {
            i.location_id == sys_id as i64
                && ctx
                    .sde
                    .types
                    .get(&i.type_id)
                    .is_some_and(|t| type_attr(ctx, t, ATTR_IS_CYNO_JAMMER) > 0.0 || t.type_id == 33700)
        });
        tenebrex_jam || other_structure_jam || item_jam
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

/// Computes effective isotope consumption per LY (`attr 868`) with `Jump Fuel Conservation (21610)`,
/// `Jump Freighters (29029)` (`group 902` only), and stacking-penalized online `Jump Drive Economizer` (`group 1299`) modules.
pub(super) fn effective_jump_drive_consumption_per_ly(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    ship_type: &eve_sde::InvType,
) -> f64 {
    let raw = type_attr(ctx, ship_type, ATTR_JUMP_DRIVE_CONSUMPTION_AMOUNT);
    let (jfc_lvl, jf_lvl, mut econ_bonuses) = ctx.db.with_snapshot(|s| {
        let sk = s.skills.get(&char_id);
        let jfc = sk.and_then(|m| m.get(&21610)).map(|r| r.level).unwrap_or(0);
        let jf = sk.and_then(|m| m.get(&29029)).map(|r| r.level).unwrap_or(0);
        let bonuses: Vec<f64> = s
            .items
            .values()
            .filter(|i| {
                i.location_id == ship_id
                    && matches!(i.flag as u16, 11..=34 | 92..=99 | 125..=132)
                    && s.module_online_states.get(&i.item_id).copied().unwrap_or(true)
            })
            .filter_map(|i| {
                let mt = ctx.sde.types.get(&i.type_id)?;
                if mt.group_id == 1299 || mt.effect_ids.contains(&5911) {
                    let pct = type_attr(ctx, mt, 1296);
                    (pct.abs() > 1e-6).then_some(pct / 100.0)
                } else {
                    None
                }
            })
            .collect();
        (jfc, jf, bonuses)
    });
    econ_bonuses.sort_by(|a, b| b.abs().partial_cmp(&a.abs()).unwrap_or(std::cmp::Ordering::Equal));
    let mut econ_mult = 1.0;
    for (idx, d) in econ_bonuses.into_iter().enumerate() {
        econ_mult *= 1.0 + d * eve_dogma::stacking_penalty_factor(idx);
    }
    let jfc_mult = 1.0 - 0.10 * (jfc_lvl.min(5) as f64);
    let jf_mult = if ship_type.group_id == 902 {
        1.0 - 0.10 * (jf_lvl.min(5) as f64)
    } else {
        1.0
    };
    raw * jfc_mult * jf_mult * econ_mult
}

/// Consumes `qty` units of `type_id` from items stored inside `ship_id` (fuel bay first, then cargo).
fn consume_from_ship(snap: &mut eve_db::WorldStateSnapshot, ship_id: i64, type_id: TypeId, qty: i64) -> Result<(), i64> {
    if qty <= 0 {
        return Ok(());
    }
    let mut stacks: Vec<(u8, i64)> = snap
        .items
        .values()
        .filter(|i| {
            i.location_id == ship_id
                && i.type_id == type_id
                && matches!(
                    i.flag,
                    eve_db::InventoryFlag::SpecializedFuelBay | eve_db::InventoryFlag::Cargo
                )
        })
        .map(|i| (if i.flag == eve_db::InventoryFlag::SpecializedFuelBay { 0 } else { 1 }, i.item_id))
        .collect();
    stacks.sort_by_key(|&(prio, id)| (prio, id));
    let available: i64 = stacks.iter().filter_map(|&(_, id)| snap.items.get(&id)).map(|i| i.quantity as i64).sum();
    if available < qty {
        return Err(available);
    }
    let mut remaining = qty;
    for (_, id) in stacks {
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

fn self_immobilizing_cycle(
    ship_id: i64,
    module_id: i64,
    module_type_id: TypeId,
    effect: &str,
    duration_s: f64,
) -> crate::system_actor::ActiveModuleCycle {
    crate::system_actor::ActiveModuleCycle {
        ship_id,
        module_id,
        module_type_id,
        effect_name: effect.into(),
        repeat: 1,
        duration_s,
        immobilize_self: true,
        ..Default::default()
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
        .any(|e| ctx.sde.effects.get(e).is_some_and(|d| d.name == CYNO_EFFECT_NAME))
        || type_attr(ctx, mod_type, ATTR_IS_INDUSTRIAL_CYNO) > 0.0
        || matches!(mod_type.type_id, 21096 | 28646 | 52694);
    if !is_cyno {
        return None;
    }
    Some(light_cyno(ctx, sys, char_id, ship_id, sys_id, module_id, mod_type).await)
}

#[allow(clippy::too_many_arguments)]
// parity: feat:ships.cynosural-beacon-spawning, feat:modules.covert-cyno-rules, feat:modules.industrial-cyno-rules
async fn light_cyno(
    ctx: &ServerContext,
    sys: &crate::system_actor::SystemHandle,
    char_id: i64,
    ship_id: i64,
    sys_id: u32,
    module_id: i64,
    mod_type: &eve_sde::InvType,
) -> Result<PyRep, BridgeRpcError> {
    if sys_id == 30_100_000 {
        return Err(err("CynoDisallowedInZarzakh", "Cynosural fields cannot be lit in Zarzakh"));
    }
    let ship_type_id = ctx.db.with_snapshot(|s| s.items.get(&ship_id).map(|i| i.type_id)).unwrap_or(0);
    let ship_type = ctx.sde.types.get(&ship_type_id).ok_or_else(|| err("ShipNotFound", "Active ship type unknown"))?;
    if !hull_allowed(ctx, mod_type, ship_type) {
        return Err(err(
            "CannotLightCynoInSystem",
            format!("{} cannot be operated from {}", mod_type.name, ship_type.name),
        ));
    }
    if ctx.sde.solar_systems.get(&sys_id).map(|s| s.region_id) == Some(10_000_070) {
        return Err(err(
            "CynoJammedInPochven",
            format!("A region-wide Triglavian cyno jammer blocks all cynosural fields in Pochven system {sys_id}"),
        ));
    }
    let sec = ctx.sde.solar_systems.get(&sys_id).map(|s| s.security_status).unwrap_or(0.0);
    if sec >= 0.45 {
        return Err(err(
            "CannotLightCynoInSystem",
            format!("Cynosural fields cannot be lit in high-security system {sys_id}"),
        ));
    }
    let is_covert = type_attr(ctx, mod_type, ATTR_IS_COVERT) > 0.0;
    let in_fleet = ctx.db.with_snapshot(|s| fleet_of(s, char_id).is_some());
    if !in_fleet {
        return Err(err("MustBeInFleet", "Activating a cynosural field requires fleet membership"));
    }
    if let Ok(Some((_, ball))) = sys.get_ball_snapshot(ship_id).await {
        if ball.velocity.length() >= 500.0 {
            return Err(err("ShipMovingTooFast", "Cannot activate cynosural field while moving at 500 m/s or faster"));
        }
    }
    if !is_covert && active_cyno_jammer(ctx, sys_id).is_some() {
        return Err(err("CynoJammed", format!("A cynosural jammer is active in system {sys_id}")));
    }
    let ship_pos = sys.get_ball_position_sync(ship_id).or_else(|| {
        ctx.db.with_snapshot(|snap| snap.item_positions.get(&ship_id).map(|p| glam::DVec3::from_array(*p)))
    });
    let is_inhibited = ctx.db.with_snapshot(|snap| {
        crate::services::structure_acl_defense_rules::is_cyno_inhibited(ctx, snap, sys_id, ship_pos)
    });
    if !is_covert && is_inhibited {
        return Err(err(
            "CynoInhibited",
            "A Mobile Cynosural Inhibitor is active within range (100 km), preventing cynosural field activation",
        ));
    }
    if is_ship_cyno_active(ctx, ship_id) {
        return Err(err(
            "CannotLightCynoInSystem",
            "A cynosural field is already active on this ship",
        ));
    }
    let beacon_type_id = beacon_type_for_module(ctx, mod_type)
        .ok_or_else(|| err("NoActiveBeacon", "No SDE beacon type matches this cynosural generator"))?;
    let (ship_vals, mod_vals) = evaluated_attrs(
        ctx,
        char_id,
        ship_id,
        &[3320],
        Some((module_id, &[attr_ids::DURATION, ATTR_CONSUMPTION_QUANTITY])),
    );
    if is_covert && ship_type.group_id == 963 && ship_vals[0] <= 0.0 {
        return Err(err(
            "CannotLightCynoInSystem",
            format!("{} requires a Covert Reconfiguration subsystem to light {}", ship_type.name, mod_type.name),
        ));
    }
    let base_dur_ms = type_attr(ctx, mod_type, attr_ids::DURATION);
    let duration_ms = if mod_vals[0] > 0.0 {
        mod_vals[0]
    } else {
        base_dur_ms
    };
    let fuel_type = type_attr(ctx, mod_type, ATTR_CONSUMPTION_TYPE) as TypeId;
    let cft_lvl = ctx.db.with_snapshot(|s| {
        s.skills.get(&char_id).and_then(|sk| sk.get(&21603)).map(|r| r.level).unwrap_or(0)
    });
    let req_skill = type_attr(ctx, mod_type, 182) as u32;
    let req_skill_lvl = type_attr(ctx, mod_type, 277) as i32;
    if req_skill != 0 && req_skill_lvl > 0 {
        let char_sk = ctx.db.with_snapshot(|s| {
            s.skills.get(&char_id).and_then(|sk| sk.get(&req_skill)).map(|r| r.level).unwrap_or(0)
        });
        if (char_sk as i32) < req_skill_lvl {
            return Err(err(
                "SkillMissing",
                format!("{} requires Cynosural Field Theory level {req_skill_lvl}", mod_type.name),
            ));
        }
    }
    let fuel_qty = {
        let raw_base = type_attr(ctx, mod_type, ATTR_CONSUMPTION_QUANTITY);
        let bonus_1296 = type_attr(ctx, ship_type, 1296);
        let role_mult = if ship_type.effect_ids.contains(&3526) || bonus_1296 < 0.0 {
            (1.0 + bonus_1296 / 100.0).max(0.0)
        } else {
            1.0
        };
        let cft_mult = 1.0 - 0.10 * (cft_lvl.min(5) as f64);
        let min_floor = if type_attr(ctx, mod_type, ATTR_IS_INDUSTRIAL_CYNO) > 0.0 {
            400.0
        } else if role_mult == 0.0 {
            0.0
        } else {
            1.0
        };
        (raw_base * role_mult * cft_mult).max(min_floor).ceil() as i64
    };
    let duration_s = duration_ms / 1000.0;
    sys.add_module_cycle(self_immobilizing_cycle(
        ship_id,
        module_id,
        mod_type.type_id,
        CYNO_EFFECT_NAME,
        duration_s,
    ))
    .await
    .map_err(|e| err("ModuleActivationFailed", e))?;
    if fuel_qty > 0 {
        if let Err(e) = consume_or_err(ctx, ship_id, fuel_type, fuel_qty, |have| {
            format!("Requires {fuel_qty} x type {fuel_type}, ship holds {have}")
        }) {
            let _ = sys.remove_module_cycle(ship_id, module_id).await;
            return Err(e);
        }
    }

    let ship_pos = sys.get_ball_position_sync(ship_id).unwrap_or(DVec3::ZERO);
    let spawn_radius = type_attr(ctx, mod_type, ATTR_CYNO_SPAWN_RADIUS);
    let position = ship_pos + DVec3::new(spawn_radius, 0.0, 0.0);
    let beacon_id = ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        snap.next_entity_id
    });
    let radius = ctx.sde.types.get(&beacon_type_id).map(|t| t.radius).unwrap_or(0.0);
    let beacon_meta = eve_destiny::SlimItemMeta {
        group_id: ctx.sde.types.get(&beacon_type_id).map(|t| t.group_id).unwrap_or(684),
        category_id: ctx.sde.types.get(&beacon_type_id).map(|t| t.category_id).unwrap_or(2),
        owner_id: char_id,
        char_id: Some(char_id),
        corp_id: ctx.db.with_snapshot(|s| s.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(0)),
        alliance_id: ctx.db.with_snapshot(|s| s.characters.get(&char_id).and_then(|c| c.alliance_id).unwrap_or(0)),
        security_status: 0.0,
        is_cloaked: false, ..Default::default()
    };
    let _ = sys
        .spawn_ball_with_meta(
            Ball {
                id: beacon_id,
                type_id: beacon_type_id,
                position,
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 1.0,
                radius,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            eve_destiny::CombatState::default(),
            beacon_meta,
        )
        .await;
    let now = ctx.now_unix_secs();
    let expires_at_unix = now + duration_s.round() as u64;
    let beacon = FleetModuleBeacon {
        beacon_id,
        char_id,
        ship_id,
        beacon_type_id,
        solar_system_id: sys_id,
        position,
        expires_at_unix,
    };
    cyno_state()
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .beacons
        .insert((ctx_key(ctx), ship_id), beacon);
    let mut cyno_sess = SessionState::new(0);
    cyno_sess.character_id = Some(char_id);
    cyno_sess.solar_system_id = Some(sys_id);
    crate::services::pve_special_sites::record_system_interference_activity(ctx, &mut cyno_sess, sys_id, 0.35);
    if let Some(fleet_id) = ctx.db.with_snapshot(|s| fleet_of(s, char_id)) {
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
        ("isCovert", PyRep::Bool(is_covert)),
        ("onGridOnly", PyRep::Bool(is_covert)),
        ("consumptionTypeID", PyRep::Integer(fuel_type as i64)),
        ("consumptionQuantity", PyRep::Integer(fuel_qty)),
        ("durationSeconds", PyRep::Real(duration_s)),
        ("expiresAtUnix", PyRep::Integer(expires_at_unix as i64)),
    ]))
}

fn cleanup_expired_beacons(ctx: &ServerContext, expired: Vec<(i64, u32, i64, TypeId)>) {
    for (exp_char, exp_sys, exp_beacon, exp_type) in expired {
        ctx.systems.get_or_spawn(exp_sys).try_remove_ball(exp_beacon);
        if let Some(fleet_id) = ctx.db.with_snapshot(|s| fleet_of(s, exp_char)) {
            ctx.notify(
                crate::router::NotificationTarget::Fleet(fleet_id),
                "OnFleetJumpBeaconModuleChange",
                "fleetid",
                vec![
                    PyRep::Integer(exp_char),
                    PyRep::Integer(exp_sys as i64),
                    PyRep::Integer(exp_beacon),
                    PyRep::Integer(exp_type as i64),
                    PyRep::Bool(false),
                ],
            );
        }
    }
}

pub(crate) fn live_beacon(ctx: &ServerContext, cyno_char_id: i64, beacon_id: i64, sys_id: u32) -> Result<FleetModuleBeacon, BridgeRpcError> {
    let (now, ctx_k) = (ctx.now_unix_secs(), ctx_key(ctx));
    let mut st = cyno_state().write().unwrap_or_else(|e| e.into_inner());
    let (mut expired_for_char, mut expired_beacons) = (None, Vec::new());
    st.beacons.retain(|&(k, _), b| {
        if k == ctx_k && b.expires_at_unix <= now {
            if b.char_id == cyno_char_id && expired_for_char.is_none() {
                expired_for_char = Some(b.beacon_id);
            }
            expired_beacons.push((b.char_id, b.solar_system_id, b.beacon_id, b.beacon_type_id));
            false
        } else {
            true
        }
    });
    let matched_b = st
        .beacons
        .iter()
        .find(|&(&(k, _), b)| {
            k == ctx_k && b.char_id == cyno_char_id && (beacon_id == 0 || b.beacon_id == beacon_id) && (sys_id == 0 || b.solar_system_id == sys_id)
        })
        .or_else(|| st.beacons.iter().find(|&(&(k, _), b)| k == ctx_k && b.char_id == cyno_char_id))
        .map(|(_, b)| b.clone());
    drop(st);
    cleanup_expired_beacons(ctx, expired_beacons);
    let Some(b) = matched_b else {
        if let Some(exp_id) = expired_for_char {
            return Err(err("NoActiveBeacon", format!("Cynosural field {exp_id} has collapsed")));
        }
        return Err(err("NoActiveBeacon", format!("Character {cyno_char_id} has no active cynosural field")));
    };
    if (beacon_id != 0 && b.beacon_id != beacon_id) || (sys_id != 0 && b.solar_system_id != sys_id) {
        return Err(err("NoActiveBeacon", format!("Beacon {beacon_id} in system {sys_id} is not active")));
    }
    Ok(b)
}

pub(super) struct JumpOutcome {
    pub dist_ly: f64,
    pub fuel_type: TypeId,
    pub fuel_qty: i64,
    pub cooldown_min: f64,
    pub fatigue_min: f64,
    pub beyonce_bound_id: u64,
}

/// Moves a ship and character to `to_sys` at `pos`, ejecting with outward velocity if overlapping a structure.
// parity: feat:ships.jump-drive-fatigue-accumulation, feat:navigation.jump-arrival-placement
pub async fn relocate_ship(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    from_sys: u32,
    to_sys: u32,
    pos: DVec3,
    dist_ly: f64,
    fatigue_mult: f64,
) -> Result<(f64, f64), BridgeRpcError> {
    let now = ctx.now_unix();
    let cur_fatigue = ctx.db.with_snapshot(|s| {
        s.characters
            .get(&char_id)
            .filter(|c| c.jump_fatigue_until_unix > now)
            .map(|c| (c.jump_fatigue_until_unix - now) as f64 / 60.0)
            .unwrap_or(0.0)
    });
    if let Some(_active) = crate::services::space_destiny::check_session_change_timer_active(ctx, char_id, now, None) {
        return Err(BridgeRpcError {
            code: "SessionChangeTimerActive".into(),
            message: "Session change timer is active".into(),
        });
    }

    let (cd_min, fat_min) = crate::formulas::compute_jump_fatigue_and_cooldown_minutes(
        cur_fatigue,
        dist_ly,
        1.0 - fatigue_mult.clamp(0.0, 1.0),
    );
    let cd_secs = ((cd_min * 60.0).round() as u64).max(1);
    let fat_secs = ((fat_min * 60.0).round() as u64).max(cd_secs + 1);
    let dest_sys_rec = ctx.sde.solar_systems.get(&to_sys);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.jump_fatigue_minutes = fat_min;
            ch.jump_cooldown_until_unix = now + cd_secs;
            ch.jump_fatigue_until_unix = now + fat_secs;
            ch.solar_system_id = to_sys;
            if let Some(sr) = dest_sys_rec {
                ch.constellation_id = sr.constellation_id;
                ch.region_id = sr.region_id;
            }
        }
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.location_id = to_sys as i64;
            sh.version += 1;
        }
        snap.character_last_jump_unix.insert(char_id, now);
    });
    ctx.notify(
        crate::router::NotificationTarget::Character(char_id),
        "OnJumpTimersUpdated",
        "charid",
        vec![
            PyRep::Integer(crate::sp6_lifecycle::unix_to_filetime(now + cd_secs)),
            PyRep::Integer(crate::sp6_lifecycle::unix_to_filetime(now + fat_secs)),
            PyRep::Integer(crate::sp6_lifecycle::unix_to_filetime(now)),
            PyRep::Bool(true),
        ],
    );
    if let Ok(mut bmap) = ctx.bound_objects.write() {
        for entry in bmap.values_mut() {
            if entry.character_id == char_id && entry.service == "beyonce" {
                entry.object_id = to_sys as i64;
            }
        }
    }
    let from = ctx.systems.get_or_spawn(from_sys);
    let old = from.remove_ball(ship_id).await;
    crate::sp6_lifecycle::abandon_drones_and_fighters_for_ship_sync(ctx, char_id, ship_id, from_sys);
    crate::services::space_destiny::space_warp_jump::deactivate_ship_modules_on_leave_space(ctx, char_id, ship_id);
    let type_id = old.as_ref().map(|b| b.type_id).unwrap_or_else(|| {
        ctx.db.with_snapshot(|s| s.items.get(&ship_id).map(|i| i.type_id)).unwrap_or(0)
    });
    let t = ctx.sde.types.get(&type_id);
    let dest_handle = ctx.systems.get_or_spawn(to_sys);
    let mut arrival_pos = pos;
    let mut arrival_vel = DVec3::ZERO;
    let ship_radius = old.as_ref().map(|b| b.radius).or(t.map(|t| t.radius)).unwrap_or(50.0);
    let mut obstacles: Vec<(DVec3, f64)> = ctx.db.with_snapshot(|snap| {
        snap.structures
            .values()
            .filter(|st| st.solar_system_id == to_sys && st.state != "Destroyed")
            .map(|st| {
                let s_radius = ctx
                    .sde
                    .types
                    .get(&st.type_id)
                    .map(|ty| ty.radius)
                    .unwrap_or(5_000.0)
                    .max(1_000.0);
                (DVec3::from_array(st.position), s_radius)
            })
            .collect()
    });
    if let Ok(dest_balls) = dest_handle.list_balls().await {
        for sb in dest_balls {
            let is_struct = ctx
                .sde
                .types
                .get(&sb.type_id)
                .is_some_and(|st| matches!(st.category_id, 65 | 40 | 23 | 3));
            if is_struct {
                obstacles.push((sb.position, sb.radius.max(500.0)));
            }
        }
    }
    for (s_pos, s_radius) in obstacles {
        let min_sep = s_radius + ship_radius + 100.0;
        let delta = arrival_pos - s_pos;
        let dist = delta.length();
        if dist < min_sep {
            let dir = if dist > 1e-3 { delta / dist } else { DVec3::X };
            arrival_pos = s_pos + dir * min_sep;
            arrival_vel = dir * 250.0;
        }
    }
    let ball = Ball {
        id: ship_id,
        type_id,
        position: arrival_pos,
        velocity: arrival_vel,
        max_velocity: old.as_ref().map(|b| b.max_velocity).unwrap_or(100.0),
        inertia: old.as_ref().map(|b| b.inertia).unwrap_or(1.0),
        mass: old.as_ref().map(|b| b.mass).or(t.map(|t| t.mass)).unwrap_or(1.0),
        radius: ship_radius,
        speed_fraction: if arrival_vel.length_squared() > 0.0 { 1.0 } else { 0.0 },
        mode: MovementMode::Stop,
    };
    let mut combat = ctx.build_ship_combat_state(char_id, ship_id, type_id);
    combat.is_cloaked = true;
    combat.gate_cloak_remaining_ticks = 60;
    combat.undock_invulnerable_remaining_ticks = 60;
    combat.sync_invuln_with_undock_timer = true;
    let meta = ctx.build_ship_slim_meta(char_id, type_id);
    let dest_tick = dest_handle.spawn_ball_with_meta(ball, combat, meta).await.unwrap_or(0);
    crate::services::space_destiny::record_session_change_timer(
        ctx,
        char_id,
        now,
        to_sys,
        dest_tick,
        "jump_drive",
    );
    Ok((cd_min, fat_min))
}

pub(super) async fn relocate_caller(
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
    let (cd_min, fat_min) = relocate_ship(ctx, char_id, ship_id, from_sys, to_sys, pos, dist_ly, fatigue_mult).await?;
    session.solar_system_id = Some(to_sys);
    crate::services::map_service::record_solar_system_jump(ctx, char_id, to_sys);
    let bound = ctx.bind_object(session, "beyonce", to_sys as i64)?;
    Ok((cd_min, fat_min, bound))
}

pub(super) async fn caller_in_space(
    ctx: &ServerContext,
    session: &SessionState,
    allow_bubbled: bool,
) -> Result<(i64, i64, u32, Ball), BridgeRpcError> {
    sweep_expired_cyno_beacons(ctx);
    let (_, char_id) = ServerContext::require_character(session)?;
    let ship_id = session.ship_id.ok_or_else(|| err("NoActiveShip", "Session has no active ship"))?;
    let sys_id = session.solar_system_id.ok_or_else(|| err("NoSolarSystem", "Session has no solarSystemID"))?;
    if (31_000_000..32_000_000).contains(&sys_id) {
        return Err(err(
            "JumpDriveForbiddenInWormholeSpace",
            format!("Jump drives and jump portals cannot operate in wormhole system {sys_id}"),
        ));
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
    let release_portal_mod = {
        let mut st = cyno_state().write().unwrap_or_else(|e| e.into_inner());
        let pmod = st
            .bridges
            .get(&(ctx_key(ctx), ship_id))
            .filter(|b| b.bridger_ship_id == ship_id && b.expires_at_unix <= now)
            .map(|b| b.portal_module_id);
        if pmod.is_some() {
            st.bridges.remove(&(ctx_key(ctx), ship_id));
        }
        pmod
    };
    if let Some(pmod_id) = release_portal_mod {
        let _ = handle.remove_module_cycle(ship_id, pmod_id).await;
    }
    let net_wcs_negative = handle
        .get_combat_state(ship_id)
        .await
        .ok()
        .flatten()
        .is_some_and(|cs| cs.warp_scramble_strength > cs.warp_core_strength);
    let is_blocked = if allow_bubbled {
        net_wcs_negative
    } else {
        net_wcs_negative || handle.is_ball_scrambled(ship_id).await.unwrap_or(false)
    };
    if is_blocked {
        return Err(err(
            "WarpScrambled",
            "Cannot jump or bridge while warp scrambled or inside a warp disruption bubble",
        ));
    }
    {
        let st = cyno_state().read().unwrap_or_else(|e| e.into_inner());
        if let Some(b) = st.bridges.get(&(ctx_key(ctx), ship_id)) {
            if b.bridger_ship_id == ship_id && b.expires_at_unix > now {
                return Err(err("JumpPortalOpen", "Cannot jump while own portal is open"));
            }
        }
    }
    crate::services::tactical_modules::check_ship_navigation_lock(ctx, ship_id, "jump")?;
    crate::services::space_navigation_advanced::check_immobilizing_module_active(ctx, ship_id, "jump")?;
    crate::services::combat_crimewatch::check_crimewatch_action_restriction(
        ctx, char_id, ship_id, sys_id, "jump_drive", now,
    )?;
    let cd = ctx.db.with_snapshot(|s| s.characters.get(&char_id).map(|c| c.jump_cooldown_until_unix).unwrap_or(0));
    if cd > now {
        return Err(err(
            "JumpReactivationCooldown",
            format!("Jump activation cooldown active for {} s", cd - now),
        ));
    }
    Ok((char_id, ship_id, sys_id, ball))
}

fn require_same_fleet(ctx: &ServerContext, a: i64, b: i64) -> Result<(), BridgeRpcError> {
    let ok = ctx.db.with_snapshot(|s| {
        let fa = fleet_of(s, a);
        fa.is_some() && fa == fleet_of(s, b)
    });
    if ok {
        Ok(())
    } else {
        Err(err("FleetNotInFleet", format!("Characters {a} and {b} are not in the same fleet")))
    }
}

pub(super) async fn resolve_fleet_beacon(
    ctx: &ServerContext,
    char_id: i64,
    cyno_char: i64,
    beacon_id: i64,
    target_sys: u32,
    is_deployable: bool,
) -> Result<FleetModuleBeacon, BridgeRpcError> {
    if !is_deployable {
        require_same_fleet(ctx, char_id, cyno_char)?;
        return live_beacon(ctx, cyno_char, beacon_id, target_sys);
    }
    if cyno_char != 0 && require_same_fleet(ctx, char_id, cyno_char).is_ok() {
        if let Ok(b) = live_beacon(ctx, cyno_char, beacon_id, target_sys) {
            return Ok(b);
        }
    }
    let now_unix = ctx.now_unix();
    let item_info = ctx.db.with_snapshot(|s| {
        let caller_fleet = fleet_of(s, char_id);
        let caller_corp = s.characters.get(&char_id).map(|c| c.corporation_id);
        let caller_alliance = s.characters.get(&char_id).and_then(|c| c.alliance_id);
        let dep_state = crate::services::mobile_deployables::read_deployable_state_snap(s);
        let is_mobile_cyno_type = |tid: u32| -> bool {
            matches!(tid, 57319 | 59630 | 58906)
                || ctx.sde.types.get(&tid).is_some_and(|t| t.group_id == 4093)
        };
        let is_deployable_allowed = |it: &eve_db::InventoryItemRecord| -> bool {
            if is_mobile_cyno_type(it.type_id) {
                let owner_fleet = dep_state
                    .deployables
                    .get(&it.item_id)
                    .and_then(|r| r.deployed_fleet_id)
                    .or_else(|| fleet_of(s, it.owner_id));
                return caller_fleet.is_some()
                    && (caller_fleet == owner_fleet || caller_fleet == fleet_of(s, it.owner_id));
            }
            if it.owner_id == char_id {
                return true;
            }
            let owner_fleet = fleet_of(s, it.owner_id);
            if caller_fleet.is_some() && caller_fleet == owner_fleet {
                return true;
            }
            let owner_corp = s.characters.get(&it.owner_id).map(|c| c.corporation_id).or(Some(it.owner_id));
            if caller_corp.is_some() && caller_corp == owner_corp {
                return true;
            }
            let owner_alliance = s
                .characters
                .get(&it.owner_id)
                .and_then(|c| c.alliance_id)
                .or_else(|| s.corporations.get(&it.owner_id).and_then(|c| c.alliance_id));
            caller_alliance.is_some() && caller_alliance == owner_alliance
        };
        let is_deployable_beacon = |it: &eve_db::InventoryItemRecord| {
            let is_mobile_cyno = is_mobile_cyno_type(it.type_id);
            if !is_mobile_cyno && !matches!(it.type_id, 56099 | 56488) {
                return false;
            }
            if !matches!(it.flag, eve_db::InventoryFlag::None | eve_db::InventoryFlag::Hangar)
                || !(30_000_000..=33_000_000).contains(&it.location_id)
            {
                return false;
            }
            if is_mobile_cyno {
                let Some(rec) = dep_state.deployables.get(&it.item_id) else {
                    return false;
                };
                if rec.is_unanchored || now_unix >= rec.expires_at_unix {
                    return false;
                }
                let spooled = rec.anchor_completed
                    || (rec.anchor_start_unix > 0
                        && now_unix >= rec.anchor_start_unix.saturating_add(rec.anchor_duration_secs));
                if !spooled {
                    return false;
                }
                if !is_covert_beacon_type(effective_beacon_field_type(ctx, it.type_id)) {
                    let bpos = s.item_positions.get(&it.item_id).copied().map(DVec3::from_array);
                    if crate::services::structure_acl_defense_rules::is_cyno_inhibited(
                        ctx,
                        s,
                        it.location_id as u32,
                        bpos,
                    ) {
                        return false;
                    }
                }
                let owner_fleet = fleet_of(s, it.owner_id);
                return it.owner_id == char_id || (caller_fleet.is_some() && caller_fleet == owner_fleet);
            }
            if let Some(rec) = dep_state.deployables.get(&it.item_id) {
                if rec.is_unanchored
                    || now_unix < rec.anchor_start_unix.saturating_add(rec.anchor_duration_secs)
                    || now_unix >= rec.expires_at_unix
                {
                    return false;
                }
            }
            if let Some(meta) = s.sp6_lifecycle.space_object_meta.get(&it.item_id) {
                if !meta.anchor_completed && meta.anchor_start_unix.is_some() {
                    return false;
                }
            }
            let owner_fleet = fleet_of(s, it.owner_id);
            it.owner_id == char_id || (caller_fleet.is_some() && caller_fleet == owner_fleet)
        };
        if beacon_id != 0 {
            if let Some(it) = s.items.get(&beacon_id) {
                if is_deployable_allowed(it)
                    && is_deployable_beacon(it)
                    && (target_sys == 0 || it.location_id == target_sys as i64)
                {
                    return Some((it.item_id, it.type_id, it.owner_id, it.location_id as u32));
                }
            }
        }
        s.items
            .values()
            .find(|it| {
                is_deployable_allowed(it)
                    && is_deployable_beacon(it)
                    && (target_sys == 0 || it.location_id == target_sys as i64)
                    && (beacon_id == 0 || it.item_id == beacon_id)
            })
            .map(|it| (it.item_id, it.type_id, it.owner_id, it.location_id as u32))
    });
    if let Some((b_id, b_type_id, b_owner, b_sys)) = item_info {
        let handle = ctx.systems.get_or_spawn(b_sys);
        let pos = handle
            .get_ball_snapshot(b_id)
            .await
            .ok()
            .flatten()
            .map(|(_, b)| b.position)
            .unwrap_or(DVec3::ZERO);
        Ok(FleetModuleBeacon {
            beacon_id: b_id,
            char_id: b_owner,
            ship_id: 0,
            beacon_type_id: b_type_id,
            solar_system_id: b_sys,
            position: pos,
            expires_at_unix: u64::MAX,
        })
    } else {
        Err(err(
            "NoActiveBeacon",
            format!("Deployable beacon {beacon_id} in system {target_sys} is not active"),
        ))
    }
}

pub(super) async fn resolve_fleet_beacon_or_destroyed(
    ctx: &ServerContext,
    char_id: i64,
    cyno_char: i64,
    beacon_id: i64,
    target_sys: u32,
    is_deployable: bool,
) -> Result<(FleetModuleBeacon, bool), BridgeRpcError> {
    match resolve_fleet_beacon(ctx, char_id, cyno_char, beacon_id, target_sys, is_deployable).await {
        Ok(b) => Ok((b, false)),
        Err(e) if !is_deployable && cyno_char != 0 && beacon_id != 0 => {
            let now = ctx.now_unix_secs();
            let ctx_k = ctx_key(ctx);
            let destroyed_b = cyno_state()
                .read()
                .unwrap_or_else(|p| p.into_inner())
                .destroyed_beacons
                .iter()
                .find(|&(&(k, _), b)| {
                    k == ctx_k
                        && b.char_id == cyno_char
                        && b.beacon_id == beacon_id
                        && (target_sys == 0 || b.solar_system_id == target_sys)
                        && b.expires_at_unix > now
                })
                .map(|(_, b)| b.clone());
            if let Some(db_b) = destroyed_b {
                Ok((db_b, true))
            } else {
                Err(e)
            }
        }
        Err(e) => Err(e),
    }
}

/// `beyonce.CmdJumpToFleetModuleBeacon` / `beyonce.CmdJumpToFleetDeployableBeacon`: jump drive to a fleet cyno.
// parity: feat:navigation.jump-drive-range-fuel, feat:navigation.jump-arrival-placement
pub(super) async fn jump_to_fleet_beacon(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
    is_deployable: bool,
) -> Result<JumpOutcome, BridgeRpcError> {
    let cyno_char = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
    let target_sys = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0) as u32;
    let beacon_id = args.get(2).and_then(|a| a.as_i64()).unwrap_or(0);
    if (31_000_000..32_000_000).contains(&target_sys) {
        return Err(err(
            "JumpDriveForbiddenInWormholeSpace",
            format!("Jump drives cannot target wormhole system {target_sys}"),
        ));
    }
    let (char_id, ship_id, sys_id, _) = caller_in_space(ctx, session, false).await?;
    if sys_id == 30_100_000 || target_sys == 30_100_000 {
        return Err(err("JumpDriveForbiddenInZarzakh", "Jump drives cannot operate in or into Zarzakh"));
    }
    if ctx.sde.solar_systems.get(&target_sys).map(|s| s.region_id) == Some(10_000_070) {
        return Err(err(
            "CannotJumpToPochven",
            format!("Region-wide cyno jammer prevents jump drives from entering Pochven system {target_sys}"),
        ));
    }
    if target_sys != 0 {
        let (vals, _) = evaluated_attrs(ctx, char_id, ship_id, &[ATTR_JUMP_DRIVE_RANGE], None);
        let dist = distance_ly(ctx, sys_id, target_sys);
        if dist > vals[0] {
            return Err(err(
                "JumpDistanceOutOfRange",
                format!("Distance {dist:.3} LY exceeds jump range {:.3} LY", vals[0]),
            ));
        }
    }
    let (beacon, landed_at_star) =
        resolve_fleet_beacon_or_destroyed(ctx, char_id, cyno_char, beacon_id, target_sys, is_deployable).await?;
    let dest_sec = ctx
        .sde
        .solar_systems
        .get(&beacon.solar_system_id)
        .map(|s| s.security_status)
        .unwrap_or(0.0);
    if dest_sec >= 0.45 {
        return Err(err(
            "CannotJumpToHighSec",
            format!("Jump drives cannot target high-security system {}", beacon.solar_system_id),
        ));
    }
    if (31_000_000..32_000_000).contains(&beacon.solar_system_id) {
        return Err(err(
            "JumpDriveForbiddenInWormholeSpace",
            format!("Jump drives cannot target wormhole system {}", beacon.solar_system_id),
        ));
    }
    if beacon.solar_system_id == 30_100_000 {
        return Err(err("JumpDriveForbiddenInZarzakh", "Jump drives cannot target Zarzakh"));
    }
    if ctx.sde.solar_systems.get(&beacon.solar_system_id).map(|s| s.region_id) == Some(10_000_070) {
        return Err(err(
            "CannotJumpToPochven",
            format!("Region-wide cyno jammer prevents jump drives from entering Pochven system {}", beacon.solar_system_id),
        ));
    }
    let ship_type_id = ctx.db.with_snapshot(|s| s.items.get(&ship_id).map(|i| i.type_id)).unwrap_or(0);
    let ship_type = ctx.sde.types.get(&ship_type_id).ok_or_else(|| err("ShipNotFound", "Ship type unknown"))?;
    let accepts_beacon = ship_accepts_beacon(ctx, ship_type, effective_beacon_field_type(ctx, beacon.beacon_type_id));
    if type_attr(ctx, ship_type, ATTR_CAN_JUMP) <= 0.0 || !accepts_beacon {
        return Err(err(
            "ShipCannotJumpToBeacon",
            format!("{} cannot jump to beacon type {}", ship_type.name, beacon.beacon_type_id),
        ));
    }
    let (vals, _) = evaluated_attrs(
        ctx,
        char_id,
        ship_id,
        &[ATTR_JUMP_DRIVE_RANGE, ATTR_JUMP_FATIGUE_MULTIPLIER, 898],
        None,
    );
    let dist = distance_ly(ctx, sys_id, beacon.solar_system_id);
    if dist > vals[0] {
        return Err(err(
            "JumpDistanceOutOfRange",
            format!("Distance {dist:.3} LY exceeds jump range {:.3} LY", vals[0]),
        ));
    }
    let fuel_type = type_attr(ctx, ship_type, ATTR_JUMP_DRIVE_CONSUMPTION_TYPE) as TypeId;
    let jdo_lvl = ctx.db.with_snapshot(|s| {
        s.skills.get(&char_id).and_then(|m| m.get(&3456)).map(|r| r.level).unwrap_or(0)
    });
    let effective_consumption = effective_jump_drive_consumption_per_ly(ctx, char_id, ship_id, ship_type);
    let fuel_qty = (effective_consumption * dist).ceil() as i64;
    let raw_cap_need = type_attr(ctx, ship_type, 898);
    let base_cap_need = if vals[2] > 0.0 { vals[2] } else { raw_cap_need };
    let cap_frac = if jdo_lvl > 0 && (vals[2] <= 0.0 || vals[2] >= raw_cap_need) {
        (base_cap_need * (1.0 - 0.05 * (jdo_lvl.min(5) as f64))).max(0.0)
    } else {
        base_cap_need.max(0.0)
    };
    let origin_sys = ctx.systems.get_or_spawn(sys_id);
    let mut post_jump_cap = None;
    if let Ok(Some(cs)) = origin_sys.get_combat_state(ship_id).await {
        let cap_needed = cs.max_capacitor * cap_frac;
        if cs.capacitor + 1e-4 < cap_needed {
            return Err(err(
                "NotEnoughCapacitor",
                format!("Jump requires {cap_needed:.1} GJ capacitor, ship has {:.1} GJ", cs.capacitor),
            ));
        }
        post_jump_cap = Some((cs.capacitor - cap_needed).max(0.0));
    }
    if crate::services::space_destiny::check_session_change_timer_active(ctx, char_id, ctx.now_unix(), None).is_some() {
        return Err(err("SessionChangeTimerActive", "Session change timer is active"));
    }
    consume_or_err(ctx, ship_id, fuel_type, fuel_qty, |have| {
        format!("Jump requires {fuel_qty} x type {fuel_type}, ship holds {have}")
    })?;
    let pos = if landed_at_star {
        star_arrival_position(ctx, beacon.solar_system_id)
    } else {
        random_arrival_position(ctx, beacon.position)
    };
    let (cooldown_min, fatigue_min, bound) =
        relocate_caller(ctx, session, char_id, ship_id, sys_id, beacon.solar_system_id, pos, dist, vals[1]).await?;
    if let Some(rem_cap) = post_jump_cap {
        let dest_sys = ctx.systems.get_or_spawn(beacon.solar_system_id);
        if let Ok(Some(mut dcs)) = dest_sys.get_combat_state(ship_id).await {
            dcs.capacitor = rem_cap;
            let _ = dest_sys.set_combat_state(ship_id, dcs).await;
        }
    }
    Ok(JumpOutcome {
        dist_ly: dist,
        fuel_type,
        fuel_qty,
        cooldown_min,
        fatigue_min,
        beyonce_bound_id: bound,
    })
}
