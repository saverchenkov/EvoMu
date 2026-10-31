//! `structure_standup_weapons` — Upwell Structure Standup Weapon Modules (`feat:structures.point-defense-system`,
//! `feat:structures.standup-point-defense-battery`, `feat:structures.standup-guided-bomb-launcher`,
//! `feat:structures.standup-superweapon-arcing-vorton`).

#![forbid(unsafe_code)]

use crate::formulas::vorton_arc_targets_with_cap;
use crate::router::ServerContext;
use crate::system_actor::SystemHandle;
use eve_destiny::{Ball, DamageVector};
use eve_proto::{BridgeRpcError, PyRep};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

pub const GROUP_STANDUP_GUIDED_BOMB_LAUNCHER: u32 = 1328;
pub const GROUP_STANDUP_POINT_DEFENSE_BATTERY: u32 = 1330;
pub const GROUP_STANDUP_ARCING_VORTON: u32 = 1333;
pub const TYPE_KEEPSTAR: u32 = 35834;

fn immobilized_gunners() -> &'static RwLock<HashMap<(usize, i64), u64>> {
    static STATE: OnceLock<RwLock<HashMap<(usize, i64), u64>>> = OnceLock::new();
    STATE.get_or_init(|| RwLock::new(HashMap::new()))
}

fn standup_module_cycles() -> &'static RwLock<HashMap<(usize, i64), u64>> {
    static CYCLES: OnceLock<RwLock<HashMap<(usize, i64), u64>>> = OnceLock::new();
    CYCLES.get_or_init(|| RwLock::new(HashMap::new()))
}

fn is_hostile_to_structure(
    ctx: &ServerContext,
    structure_id: i64,
    structure_corp_id: i64,
    gunner_char_id: i64,
    sys_id: u32,
    ball_id: i64,
) -> bool {
    if ball_id == structure_id || ball_id < 100_000_000 {
        return false;
    }
    let now_unix = ctx.now_unix();
    let sys_sec = ctx.sde.solar_systems.get(&sys_id).map(|s| s.security_status).unwrap_or(0.0);

    let (target_char, target_corp) = ctx.db.with_snapshot(|snap| {
        if let Some(ch) = snap.characters.values().find(|c| c.active_ship_id == ball_id) {
            return (Some(ch.character_id), Some(ch.corporation_id));
        }
        if let Some(it) = snap.items.get(&ball_id) {
            if let Some(ch) = snap.characters.get(&it.owner_id) {
                return (Some(ch.character_id), Some(ch.corporation_id));
            }
            if snap.corporations.contains_key(&it.owner_id) {
                return (None, Some(it.owner_id));
            }
        }
        (None, None)
    });

    if target_corp.is_some_and(|c| c == structure_corp_id) {
        return false;
    }

    if let Some(tc) = target_char {
        if tc == gunner_char_id {
            return false;
        }
        let is_suspect_or_crim = {
            let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
            let st = cw.get_or_create(tc);
            st.is_suspect(now_unix) || st.is_criminal(now_unix)
        };
        if is_suspect_or_crim {
            return true;
        }
        let is_war = ctx.db.with_snapshot(|snap| {
            crate::services::combat_crimewatch::is_legal_war_or_suspect_target(snap, gunner_char_id, tc, now_unix)
        });
        if is_war {
            return true;
        }
        let has_le = crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
            rt.has_limited_engagement_with(tc, gunner_char_id, now_unix)
        });
        if has_le {
            return true;
        }
        if sys_sec >= 0.45 {
            return false;
        }
        return true;
    }

    if sys_sec >= 0.45 {
        return false;
    }
    true
}

fn read_omni_damage(mod_type: &eve_sde::InvType) -> DamageVector {
    let em = mod_type.default_attributes.get(&114).copied().unwrap_or(0.0);
    let exp = mod_type.default_attributes.get(&116).copied().unwrap_or(0.0);
    let kin = mod_type.default_attributes.get(&117).copied().unwrap_or(0.0);
    let therm = mod_type.default_attributes.get(&118).copied().unwrap_or(0.0);
    DamageVector { em, thermal: therm, kinetic: kin, explosive: exp }
}

/// Handles `dogmaIM.Activate` for Standup Point Defense Battery (1330),
/// Standup Guided Bomb Launcher (1328), and Standup Arcing Vorton Projector (1333).
#[allow(clippy::too_many_arguments)]
pub async fn try_activate_standup_weapon(
    ctx: &ServerContext,
    sys_handle: &SystemHandle,
    char_id: i64,
    structure_id: i64,
    sys_id: u32,
    module_id: i64,
    mod_type: &eve_sde::InvType,
    charge_type_id: Option<u32>,
    target_id_opt: Option<i64>,
    attacker_ball: &Ball,
) -> Option<Result<PyRep, BridgeRpcError>> {
    if !matches!(
        mod_type.group_id,
        GROUP_STANDUP_POINT_DEFENSE_BATTERY
            | GROUP_STANDUP_GUIDED_BOMB_LAUNCHER
            | GROUP_STANDUP_ARCING_VORTON
    ) {
        return None;
    }

    let sys_sec = ctx.sde.solar_systems.get(&sys_id).map(|s| s.security_status).unwrap_or(0.0);
    if sys_sec >= 0.45 && mod_type.default_attributes.get(&1970).is_some_and(|&v| v > 0.0) {
        return Some(Err(BridgeRpcError {
            code: "CannotActivateInHighSec".into(),
            message: format!("{} cannot be activated in high-security space", mod_type.name),
        }));
    }

    let now_unix = ctx.now_unix();
    let gunner_key = (ctx.instance_id as usize, structure_id);
    if let Ok(guard) = immobilized_gunners().read() {
        if let Some(&until) = guard.get(&gunner_key) {
            if now_unix < until {
                return Some(Err(BridgeRpcError {
                    code: "WeaponCycleImmobilized".into(),
                    message: "Structure gunner weapon cycle is currently immobilized".into(),
                }));
            }
        }
    }

    let mod_key = (ctx.instance_id as usize, module_id);
    if let Ok(guard) = standup_module_cycles().read() {
        if let Some(&until) = guard.get(&mod_key) {
            if now_unix < until {
                return Some(Err(BridgeRpcError {
                    code: "ModuleAlreadyCycling".into(),
                    message: format!("Module {module_id} is already cycling"),
                }));
            }
        }
    }

    let (st_type_id, st_corp_id) = match ctx.db.with_snapshot(|snap| {
        snap.structures
            .get(&structure_id)
            .map(|s| (s.type_id, s.owner_corp_id))
    }) {
        Some(v) => v,
        None => {
            return Some(Err(BridgeRpcError {
                code: "StructureNotFound".into(),
                message: format!("Structure {structure_id} not found"),
            }));
        }
    };

    let all_balls = sys_handle.list_balls().await.unwrap_or_default();

    match mod_type.group_id {
        GROUP_STANDUP_POINT_DEFENSE_BATTERY => {
            let aoe_radius = mod_type
                .default_attributes
                .get(&99)
                .or_else(|| mod_type.default_attributes.get(&54))
                .copied()
                .filter(|&r| r > 0.0)
                .unwrap_or(2_500.0);
            let cycle_secs = mod_type
                .default_attributes
                .get(&73)
                .map(|&ms| (ms / 1000.0).ceil() as u64)
                .unwrap_or(12);

            let dmg = DamageVector { em: 250.0, thermal: 250.0, kinetic: 250.0, explosive: 250.0 };
            let mut targets_hit = 0_i64;
            let mut total_applied = 0.0_f64;
            let mut destroyed_count = 0_i64;

            for b in &all_balls {
                if (b.position - attacker_ball.position).length() > aoe_radius {
                    continue;
                }
                if !is_hostile_to_structure(ctx, structure_id, st_corp_id, char_id, sys_id, b.id) {
                    continue;
                }
                if let Ok(Some((applied, destroyed, _))) = sys_handle.apply_damage(b.id, dmg).await {
                    targets_hit += 1;
                    total_applied += applied;
                    if destroyed {
                        destroyed_count += 1;
                    }
                }
            }

            if let Ok(mut guard) = standup_module_cycles().write() {
                guard.insert(mod_key, now_unix.saturating_add(cycle_secs));
            }

            Some(Ok(PyRep::keyval([
                ("moduleID", PyRep::Integer(module_id)),
                ("structureID", PyRep::Integer(structure_id)),
                ("gunnerID", PyRep::Integer(char_id)),
                ("aoeRadius", PyRep::Real(aoe_radius)),
                ("targetsHit", PyRep::Integer(targets_hit)),
                ("appliedDamage", PyRep::Real(total_applied)),
                ("destroyedCount", PyRep::Integer(destroyed_count)),
                ("cycleDurationSeconds", PyRep::Integer(cycle_secs as i64)),
            ])))
        }

        GROUP_STANDUP_GUIDED_BOMB_LAUNCHER => {
            let Some(tid) = target_id_opt else {
                return Some(Err(BridgeRpcError {
                    code: "TargetRequired".into(),
                    message: "Standup Guided Bomb Launcher requires a locked target".into(),
                }));
            };
            let locks = sys_handle.get_target_locks(structure_id).await.unwrap_or_default();
            if !locks.iter().any(|l| l.target_id == tid && l.locked) {
                return Some(Err(BridgeRpcError {
                    code: "TargetNotLocked".into(),
                    message: format!("Target {tid} is not locked"),
                }));
            }
            let Some((_, target_ball)) = sys_handle.get_ball_snapshot(tid).await.ok().flatten() else {
                return Some(Err(BridgeRpcError {
                    code: "TargetNotFound".into(),
                    message: format!("Target {tid} not in space"),
                }));
            };

            // Evaluate crimewatch & safety on primary target
            if let Err(e) = crate::services::combat_crimewatch::crimewatch_timers::evaluate_and_record_pvp_aggression(
                ctx, char_id, structure_id, sys_id, tid, false,
            ) {
                return Some(Err(e));
            }

            // Require loaded guided bomb charge (group 1548)
            let bomb_tid = match charge_type_id.or_else(|| {
                ctx.db.with_snapshot(|snap| {
                    snap.items.values().find(|i| {
                        i.location_id == structure_id && i.quantity > 0 && ctx.sde.types.get(&i.type_id).is_some_and(|t| t.group_id == 1548)
                    }).map(|i| i.type_id)
                })
            }) {
                Some(t) => t,
                None => {
                    return Some(Err(BridgeRpcError {
                        code: "NoAmmoLoaded".into(),
                        message: "Standup Guided Bomb Launcher requires a loaded guided bomb charge".into(),
                    }));
                }
            };
            let bomb_type = match ctx.sde.types.get(&bomb_tid) {
                Some(t) if t.group_id == 1548 => t,
                _ => {
                    return Some(Err(BridgeRpcError {
                        code: "IncompatibleAmmo".into(),
                        message: "Loaded charge is not a guided bomb".into(),
                    }));
                }
            };

            let consumed = ctx.db.with_mut_snapshot(|snap| {
                if let Some(it) = snap.items.values_mut().find(|i| i.location_id == structure_id && i.type_id == bomb_tid && i.quantity > 0) {
                    it.quantity -= 1;
                    true
                } else {
                    false
                }
            });
            if !consumed {
                return Some(Err(BridgeRpcError {
                    code: "NoAmmoLoaded".into(),
                    message: "No guided bombs remaining in structure".into(),
                }));
            }

            let aoe_radius = bomb_type
                .default_attributes
                .get(&99)
                .copied()
                .filter(|&r| r > 0.0)
                .unwrap_or(20_000.0);
            let explosion_radius = bomb_type
                .default_attributes
                .get(&654)
                .copied()
                .filter(|&r| r > 0.0)
                .unwrap_or(400.0);
            let base_dmg = read_omni_damage(bomb_type);
            let cycle_secs = mod_type
                .default_attributes
                .get(&51)
                .or_else(|| mod_type.default_attributes.get(&73))
                .map(|&ms| (ms / 1000.0).ceil() as u64)
                .unwrap_or(60);

            let mut targets_hit = 0_i64;
            let mut total_applied = 0.0_f64;
            let mut primary_sig_scale = 1.0_f64;

            for b in &all_balls {
                if (b.position - target_ball.position).length() > aoe_radius {
                    continue;
                }
                if !is_hostile_to_structure(ctx, structure_id, st_corp_id, char_id, sys_id, b.id) {
                    continue;
                }
                let sig = ctx
                    .sde
                    .types
                    .get(&b.type_id)
                    .and_then(|t| t.default_attributes.get(&552).copied())
                    .unwrap_or(125.0);
                let scale = (sig / explosion_radius).clamp(0.05, 1.0);
                if b.id == tid {
                    primary_sig_scale = scale;
                }
                let scaled_dmg = DamageVector {
                    em: base_dmg.em * scale,
                    thermal: base_dmg.thermal * scale,
                    kinetic: base_dmg.kinetic * scale,
                    explosive: base_dmg.explosive * scale,
                };
                if let Ok(Some((applied, _, _))) = sys_handle.apply_damage(b.id, scaled_dmg).await {
                    targets_hit += 1;
                    total_applied += applied;
                }
            }

            if let Ok(mut guard) = standup_module_cycles().write() {
                guard.insert(mod_key, now_unix.saturating_add(cycle_secs));
            }

            Some(Ok(PyRep::keyval([
                ("moduleID", PyRep::Integer(module_id)),
                ("targetID", PyRep::Integer(tid)),
                ("chargeTypeID", PyRep::Integer(bomb_tid as i64)),
                ("solarSystemID", PyRep::Integer(sys_id as i64)),
                ("aoeRadius", PyRep::Real(aoe_radius)),
                ("signatureScale", PyRep::Real(primary_sig_scale)),
                ("targetsHit", PyRep::Integer(targets_hit)),
                ("appliedDamage", PyRep::Real(total_applied)),
                ("cycleDurationSeconds", PyRep::Integer(cycle_secs as i64)),
            ])))
        }

        GROUP_STANDUP_ARCING_VORTON => {
            if st_type_id != TYPE_KEEPSTAR {
                return Some(Err(BridgeRpcError {
                    code: "CantFitToStructure".into(),
                    message: "Standup Arcing Vorton Projector can only be operated on a Keepstar".into(),
                }));
            }
            let Some(tid) = target_id_opt else {
                return Some(Err(BridgeRpcError {
                    code: "TargetRequired".into(),
                    message: "Standup Arcing Vorton Projector requires a locked primary target".into(),
                }));
            };
            let locks = sys_handle.get_target_locks(structure_id).await.unwrap_or_default();
            if !locks.iter().any(|l| l.target_id == tid && l.locked) {
                return Some(Err(BridgeRpcError {
                    code: "TargetNotLocked".into(),
                    message: format!("Target {tid} is not locked"),
                }));
            }
            let Some((_, target_ball)) = sys_handle.get_ball_snapshot(tid).await.ok().flatten() else {
                return Some(Err(BridgeRpcError {
                    code: "TargetNotFound".into(),
                    message: format!("Target {tid} not in space"),
                }));
            };

            // Evaluate crimewatch & safety
            if let Err(e) = crate::services::combat_crimewatch::crimewatch_timers::evaluate_and_record_pvp_aggression(
                ctx, char_id, structure_id, sys_id, tid, false,
            ) {
                return Some(Err(e));
            }

            let jump_radius = mod_type
                .default_attributes
                .get(&99)
                .copied()
                .filter(|&r| r > 0.0)
                .unwrap_or(50_000.0);
            let cycle_secs = mod_type
                .default_attributes
                .get(&73)
                .or_else(|| mod_type.default_attributes.get(&51))
                .map(|&ms| (ms / 1000.0).ceil() as u64)
                .unwrap_or(600);
            let dmg = read_omni_damage(mod_type);

            let candidates: Vec<(i64, DVec3)> = all_balls
                .iter()
                .filter(|b| b.id != tid && is_hostile_to_structure(ctx, structure_id, st_corp_id, char_id, sys_id, b.id))
                .map(|b| (b.id, b.position))
                .collect();
            let hit_ids = vorton_arc_targets_with_cap(tid, target_ball.position, &candidates, jump_radius, 4);

            let mut total_applied = 0.0_f64;
            let mut targets_hit = 0_i64;
            for hid in &hit_ids {
                if let Ok(Some((applied, _, _))) = sys_handle.apply_damage(*hid, dmg).await {
                    targets_hit += 1;
                    total_applied += applied;
                }
            }

            let immobilized_until = now_unix.saturating_add(cycle_secs);
            if let Ok(mut guard) = immobilized_gunners().write() {
                guard.insert(gunner_key, immobilized_until);
            }
            if let Ok(mut guard) = standup_module_cycles().write() {
                guard.insert(mod_key, immobilized_until);
            }

            Some(Ok(PyRep::keyval([
                ("moduleID", PyRep::Integer(module_id)),
                ("targetID", PyRep::Integer(tid)),
                ("targetsHit", PyRep::Integer(targets_hit)),
                ("secondaryTargetsHit", PyRep::Integer(targets_hit.saturating_sub(1))),
                ("appliedDamage", PyRep::Real(total_applied)),
                ("immobilizedUntilUnix", PyRep::Integer(immobilized_until as i64)),
                ("cycleDurationSeconds", PyRep::Integer(cycle_secs as i64)),
            ])))
        }

        _ => None,
    }
}
