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

fn is_hostile_to_structure(
    ctx: &ServerContext,
    structure_id: i64,
    structure_corp_id: i64,
    ball_id: i64,
) -> bool {
    if ball_id == structure_id {
        return false;
    }
    ctx.db.with_snapshot(|snap| {
        if let Some(ch) = snap.characters.values().find(|c| c.active_ship_id == ball_id) {
            return ch.corporation_id != structure_corp_id;
        }
        if let Some(it) = snap.items.get(&ball_id) {
            if it.owner_id == structure_corp_id {
                return false;
            }
            if let Some(ch) = snap.characters.get(&it.owner_id) {
                return ch.corporation_id != structure_corp_id;
            }
        }
        true
    })
}

fn read_omni_damage(
    ctx: &ServerContext,
    mod_type: &eve_sde::InvType,
    charge_type_id: Option<u32>,
    default_per_type: f64,
) -> DamageVector {
    let src = charge_type_id
        .and_then(|cid| ctx.sde.types.get(&cid))
        .unwrap_or(mod_type);
    let em = src.default_attributes.get(&114).copied().unwrap_or(0.0);
    let exp = src.default_attributes.get(&116).copied().unwrap_or(0.0);
    let kin = src.default_attributes.get(&117).copied().unwrap_or(0.0);
    let therm = src.default_attributes.get(&118).copied().unwrap_or(0.0);
    if em + exp + kin + therm > 0.0 {
        DamageVector { em, thermal: therm, kinetic: kin, explosive: exp }
    } else {
        DamageVector {
            em: default_per_type,
            thermal: default_per_type,
            kinetic: default_per_type,
            explosive: default_per_type,
        }
    }
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

    let now_unix = ctx.now_unix();
    let key = (ctx.instance_id as usize, structure_id);
    if let Ok(guard) = immobilized_gunners().read() {
        if let Some(&until) = guard.get(&key) {
            if now_unix < until {
                return Some(Err(BridgeRpcError {
                    code: "WeaponCycleImmobilized".into(),
                    message: "Structure gunner weapon cycle is currently immobilized".into(),
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
                .get(&54)
                .or_else(|| mod_type.default_attributes.get(&99))
                .copied()
                .filter(|&r| r > 0.0)
                .unwrap_or(7_500.0);
            let dmg = read_omni_damage(ctx, mod_type, charge_type_id, 250.0);
            let mut targets_hit = 0_i64;
            let mut total_applied = 0.0_f64;
            let mut destroyed_count = 0_i64;

            for b in &all_balls {
                if (b.position - attacker_ball.position).length() > aoe_radius {
                    continue;
                }
                if !is_hostile_to_structure(ctx, structure_id, st_corp_id, b.id) {
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

            Some(Ok(PyRep::keyval([
                ("moduleID", PyRep::Integer(module_id)),
                ("structureID", PyRep::Integer(structure_id)),
                ("gunnerID", PyRep::Integer(char_id)),
                ("aoeRadius", PyRep::Real(aoe_radius)),
                ("targetsHit", PyRep::Integer(targets_hit)),
                ("appliedDamage", PyRep::Real(total_applied)),
                ("destroyedCount", PyRep::Integer(destroyed_count)),
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

            let bomb_src = charge_type_id.and_then(|cid| ctx.sde.types.get(&cid)).unwrap_or(mod_type);
            let aoe_radius = bomb_src
                .default_attributes
                .get(&99)
                .copied()
                .filter(|&r| r > 0.0)
                .unwrap_or(10_000.0);
            let explosion_radius = bomb_src
                .default_attributes
                .get(&654)
                .copied()
                .filter(|&r| r > 0.0)
                .unwrap_or(400.0);
            let base_dmg = read_omni_damage(ctx, mod_type, charge_type_id, 1_500.0);

            let mut targets_hit = 0_i64;
            let mut total_applied = 0.0_f64;
            let mut primary_sig_scale = 1.0_f64;

            for b in &all_balls {
                if (b.position - target_ball.position).length() > aoe_radius {
                    continue;
                }
                if !is_hostile_to_structure(ctx, structure_id, st_corp_id, b.id) {
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

            Some(Ok(PyRep::keyval([
                ("moduleID", PyRep::Integer(module_id)),
                ("targetID", PyRep::Integer(tid)),
                ("solarSystemID", PyRep::Integer(sys_id as i64)),
                ("aoeRadius", PyRep::Real(aoe_radius)),
                ("signatureScale", PyRep::Real(primary_sig_scale)),
                ("targetsHit", PyRep::Integer(targets_hit)),
                ("appliedDamage", PyRep::Real(total_applied)),
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

            let jump_radius = mod_type
                .default_attributes
                .get(&99)
                .copied()
                .filter(|&r| r > 0.0)
                .unwrap_or(50_000.0);
            let cycle_secs = (mod_type
                .default_attributes
                .get(&51)
                .copied()
                .filter(|&d| d > 0.0)
                .unwrap_or(180_000.0)
                / 1_000.0)
                .ceil() as u64;
            let dmg = read_omni_damage(ctx, mod_type, charge_type_id, 25_000.0);

            let candidates: Vec<(i64, DVec3)> = all_balls
                .iter()
                .filter(|b| b.id != tid && is_hostile_to_structure(ctx, structure_id, st_corp_id, b.id))
                .map(|b| (b.id, b.position))
                .collect();
            let hit_ids = vorton_arc_targets_with_cap(tid, target_ball.position, &candidates, jump_radius, 5);

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
                guard.insert(key, immobilized_until);
            }

            Some(Ok(PyRep::keyval([
                ("moduleID", PyRep::Integer(module_id)),
                ("targetID", PyRep::Integer(tid)),
                ("targetsHit", PyRep::Integer(targets_hit)),
                ("secondaryTargetsHit", PyRep::Integer(targets_hit.saturating_sub(1))),
                ("appliedDamage", PyRep::Real(total_applied)),
                ("immobilizedUntilUnix", PyRep::Integer(immobilized_until as i64)),
            ])))
        }

        _ => None,
    }
}
