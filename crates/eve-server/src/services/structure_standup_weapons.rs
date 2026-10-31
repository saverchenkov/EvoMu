//! `structure_standup_weapons` — Upwell Structure Standup Weapon Modules (`feat:structures.standup-point-defense-battery`,
//! `feat:structures.standup-guided-bomb-launcher`, `feat:structures.standup-superweapon-arcing-vorton`).

#![forbid(unsafe_code)]

use crate::formulas::vorton_arc_targets_with_cap;
use crate::router::ServerContext;
use crate::services::combat_crimewatch::{with_combat_runtime, SafetyLevel};
use crate::services::station_structure::structure_upwell_ops::{
    load_ext_state, save_ext_state, InFlightGuidedBombRecord,
};
use crate::services::station_structure::structure_lifecycle::{structure_profile, HullClass};
use crate::system_actor::SystemHandle;
use eve_db::{InventoryFlag, InventoryItemRecord, WorldStateSnapshot};
use eve_destiny::{Ball, CombatState, DamageVector, MovementMode};
use eve_proto::{BridgeRpcError, PyRep};
use glam::DVec3;

// parity: feat:structures.standup-guided-bomb-launcher, feat:structures.standup-point-defense-battery, feat:structures.standup-superweapon-arcing-vorton
pub const GROUP_STANDUP_GUIDED_BOMB_LAUNCHER: u32 = 1328;
pub const GROUP_STANDUP_POINT_DEFENSE_BATTERY: u32 = 1330;
pub const GROUP_STANDUP_ARCING_VORTON: u32 = 1333;
pub const GROUP_STANDUP_GUIDED_BOMB: u32 = 1548;
pub const GROUP_STANDUP_FLAK_AMMO: u32 = 4186;
pub const TYPE_KEEPSTAR: u32 = 35834;
pub const TYPE_PALATINE_KEEPSTAR: u32 = 40340;

fn err(code: &str, message: impl Into<String>) -> BridgeRpcError {
    BridgeRpcError { code: code.into(), message: message.into() }
}

/// Blocks `ReleaseControl`, `EjectFromStructure`, and `Undock` while a structure gunner is immobilized by a superweapon cycle.
pub fn ensure_gunner_not_immobilized(
    snap: &WorldStateSnapshot,
    char_id: i64,
    now_unix: u64,
) -> Result<(), BridgeRpcError> {
    let ext = load_ext_state(snap);
    let immobilized = ext.active_pilot_by_structure.iter().any(|(sid, &pid)| {
        pid == char_id
            && ext
                .gunner_immobilized_until_unix
                .get(sid)
                .is_some_and(|&until| now_unix < until)
    });
    if immobilized {
        return Err(err(
            "GunnerImmobilized",
            "Structure gunner is immobilized by an active Standup superweapon cycle",
        ));
    }
    Ok(())
}

fn read_omni_damage(t: &eve_sde::InvType) -> DamageVector {
    DamageVector {
        em: t.default_attributes.get(&114).copied().unwrap_or(0.0),
        thermal: t.default_attributes.get(&118).copied().unwrap_or(0.0),
        kinetic: t.default_attributes.get(&117).copied().unwrap_or(0.0),
        explosive: t.default_attributes.get(&116).copied().unwrap_or(0.0),
    }
}

fn standup_weapon_optimal_and_falloff(mod_type: &eve_sde::InvType) -> (f64, f64) {
    let default_opt = match mod_type.group_id {
        GROUP_STANDUP_POINT_DEFENSE_BATTERY => 2_500.0,
        GROUP_STANDUP_ARCING_VORTON => 250_000.0,
        _ => 0.0,
    };
    let default_falloff = match mod_type.group_id {
        GROUP_STANDUP_ARCING_VORTON => 50_000.0,
        _ => 0.0,
    };
    let optimal = mod_type
        .default_attributes
        .get(&54)
        .or_else(|| mod_type.default_attributes.get(&99))
        .copied()
        .filter(|&v| v > 0.0)
        .unwrap_or(default_opt);
    let falloff = mod_type
        .default_attributes
        .get(&158)
        .or_else(|| mod_type.default_attributes.get(&2044))
        .copied()
        .filter(|&v| v > 0.0)
        .unwrap_or(default_falloff);
    (optimal, falloff)
}

fn standup_falloff_multiplier(dist: f64, optimal: f64, falloff: f64) -> Option<f64> {
    if dist <= optimal {
        return Some(1.0);
    }
    if falloff <= 0.0 || dist > optimal + 2.0 * falloff {
        return None;
    }
    let over = (dist - optimal) / falloff;
    Some(0.5_f64.powf(over * over))
}

fn is_structure_ammo_bay(item: &InventoryItemRecord, st_corp_id: i64, mod_flag: InventoryFlag) -> bool {
    item.flag == mod_flag
        || (item.owner_id == st_corp_id
            && matches!(
                item.flag,
                InventoryFlag::StructureFuel
                    | InventoryFlag::Cargo
                    | InventoryFlag::SpecializedAmmoHold
                    | InventoryFlag::CorpSAG1
                    | InventoryFlag::CorpSAG2
                    | InventoryFlag::CorpSAG3
                    | InventoryFlag::CorpSAG4
                    | InventoryFlag::CorpSAG5
                    | InventoryFlag::CorpSAG6
                    | InventoryFlag::CorpSAG7
            ))
}

fn find_structure_charge_type(
    ctx: &ServerContext,
    structure_id: i64,
    st_corp_id: i64,
    module_id: i64,
    mod_flag: InventoryFlag,
    charge_group: u32,
    min_qty: i32,
    preferred_type: Option<u32>,
) -> Option<u32> {
    ctx.db.with_snapshot(|snap| {
        let matches_item = |i: &&InventoryItemRecord| {
            i.location_id == structure_id
                && i.item_id != module_id
                && i.quantity >= min_qty
                && is_structure_ammo_bay(i, st_corp_id, mod_flag)
                && preferred_type.is_none_or(|pt| i.type_id == pt)
                && ctx.sde.types.get(&i.type_id).is_some_and(|t| t.group_id == charge_group)
        };
        snap.items
            .values()
            .find(|i| i.flag == mod_flag && matches_item(i))
            .or_else(|| snap.items.values().find(matches_item))
            .map(|i| i.type_id)
    })
}

fn consume_structure_charge(
    ctx: &ServerContext,
    structure_id: i64,
    st_corp_id: i64,
    module_id: i64,
    mod_flag: InventoryFlag,
    charge_tid: u32,
    qty_needed: i32,
) -> bool {
    ctx.db.with_mut_snapshot(|snap| {
        let candidate_id = snap
            .items
            .values()
            .find(|i| {
                i.location_id == structure_id
                    && i.item_id != module_id
                    && i.flag == mod_flag
                    && i.type_id == charge_tid
                    && i.quantity >= qty_needed
            })
            .or_else(|| {
                snap.items.values().find(|i| {
                    i.location_id == structure_id
                        && i.item_id != module_id
                        && is_structure_ammo_bay(i, st_corp_id, mod_flag)
                        && i.type_id == charge_tid
                        && i.quantity >= qty_needed
                })
            })
            .map(|i| i.item_id);
        let Some(cid) = candidate_id else {
            return false;
        };
        let remove = if let Some(it) = snap.items.get_mut(&cid) {
            it.quantity -= qty_needed;
            it.quantity <= 0
        } else {
            false
        };
        if remove {
            snap.items.remove(&cid);
        }
        true
    })
}

fn resolve_victim_owner(ctx: &ServerContext, sys_handle: &SystemHandle, ball_id: i64) -> (Option<i64>, bool) {
    if let Some(dc) = with_combat_runtime(ctx, |rt| rt.active_drones_in_space.get(&ball_id).map(|d| d.owner_char_id)) {
        return (Some(dc), true);
    }
    if let Some(fc) = ctx.active_fighters.read().ok().and_then(|af| af.get(&ball_id).map(|f| f.owner_char_id)) {
        return (Some(fc), true);
    }
    let ch_opt = ctx.db.with_snapshot(|snap| {
        snap.characters
            .values()
            .find(|c| c.active_ship_id == ball_id)
            .map(|c| c.character_id)
            .or_else(|| {
                snap.items
                    .get(&ball_id)
                    .map(|i| i.owner_id)
                    .filter(|oid| snap.characters.contains_key(oid))
            })
    }).or_else(|| {
        let bp = sys_handle.ballpark.read().unwrap_or_else(|e| e.into_inner());
        let meta = bp.ball_slim_meta.get(&ball_id)?;
        meta.char_id.filter(|cid| ctx.db.with_snapshot(|s| s.characters.contains_key(cid)))
    });
    let is_sub = sys_handle
        .ballpark
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .ball_slim_meta
        .get(&ball_id)
        .is_some_and(|m| matches!(m.category_id, 18 | 87));
    (ch_opt, is_sub)
}

fn is_hostile_to_structure(
    ctx: &ServerContext,
    sys_handle: &SystemHandle,
    structure_id: i64,
    structure_corp_id: i64,
    gunner_char_id: i64,
    sys_id: u32,
    ball: &Ball,
) -> bool {
    if ball.id == structure_id {
        return false;
    }
    let is_fighter = ctx.active_fighters.read().is_ok_and(|af| af.contains_key(&ball.id));
    let is_drone = with_combat_runtime(ctx, |rt| rt.active_drones_in_space.contains_key(&ball.id));
    if let Some(bt) = ctx.sde.types.get(&ball.type_id) {
        if bt.group_id == GROUP_STANDUP_GUIDED_BOMB {
            return false;
        }
        if !is_fighter && !is_drone && !matches!(bt.category_id, 6 | 11 | 18 | 22 | 87) {
            return false;
        }
    } else if !is_fighter && !is_drone {
        return false;
    }

    let now_unix = ctx.now_unix();
    let sys_sec = ctx.sde.solar_systems.get(&sys_id).map(|s| s.security_status).unwrap_or(0.0);
    let (victim_char, _) = resolve_victim_owner(ctx, sys_handle, ball.id);
    let (st_alliance, gunner_fleet, target_corp, target_alliance, target_fleet) =
        ctx.db.with_snapshot(|snap| {
            let st_all = snap.corporations.get(&structure_corp_id).and_then(|c| c.alliance_id);
            let g_flt = snap.fleets.values().find(|f| f.members.contains(&gunner_char_id)).map(|f| f.fleet_id);
            let (t_corp, t_all, t_flt) = if let Some(vc) = victim_char {
                let c_rec = snap.characters.get(&vc);
                let corp = c_rec.map(|c| c.corporation_id);
                let all = c_rec.and_then(|c| c.alliance_id).or_else(|| corp.and_then(|cid| snap.corporations.get(&cid).and_then(|co| co.alliance_id)));
                let flt = snap.fleets.values().find(|f| f.members.contains(&vc)).map(|f| f.fleet_id);
                (corp, all, flt)
            } else if let Some(it) = snap.items.get(&ball.id) {
                let corp = snap.corporations.contains_key(&it.owner_id).then_some(it.owner_id);
                let all = corp.and_then(|cid| snap.corporations.get(&cid).and_then(|co| co.alliance_id));
                (corp, all, None)
            } else {
                (None, None, None)
            };
            (st_all, g_flt, t_corp, t_all, t_flt)
        });

    if target_corp == Some(structure_corp_id) {
        return false;
    }
    if st_alliance.is_some() && target_alliance == st_alliance {
        return false;
    }
    if gunner_fleet.is_some() && target_fleet == gunner_fleet {
        return false;
    }

    if let Some(tc) = victim_char {
        if tc == gunner_char_id || crate::services::space_destiny::is_location_change_invulnerable(ctx, tc, now_unix) {
            return false;
        }
        let is_suspect_or_crim = {
            let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
            let st = cw.get_or_create(tc);
            st.is_suspect(now_unix) || st.is_criminal(now_unix)
        };
        let is_war = ctx.db.with_snapshot(|snap| {
            crate::services::combat_crimewatch::is_legal_war_or_suspect_target(snap, gunner_char_id, tc, now_unix)
        });
        let has_le = with_combat_runtime(ctx, |rt| rt.has_limited_engagement_with(tc, gunner_char_id, now_unix));
        if is_suspect_or_crim || is_war || has_le {
            return true;
        }
        if sys_sec >= 0.45 {
            return false;
        }
        if sys_sec > 0.0 {
            let safety = with_combat_runtime(ctx, |rt| {
                rt.safety_levels.get(&gunner_char_id).copied().unwrap_or(SafetyLevel::Red)
            });
            if safety == SafetyLevel::Green || (safety == SafetyLevel::Yellow && ball.type_id == 670) {
                return false;
            }
        }
        return true;
    }

    sys_sec < 0.45
}

#[allow(clippy::too_many_arguments)]
async fn apply_standup_hit(
    ctx: &ServerContext,
    sys_handle: &SystemHandle,
    structure_id: i64,
    st_type_id: u32,
    weapon_type_id: u32,
    gunner_char_id: i64,
    sys_id: u32,
    ball: &Ball,
    dmg: DamageVector,
    neut_amt: f64,
    cycle_secs: f64,
    now_unix: u64,
) -> Option<(f64, bool)> {
    if sys_handle.get_combat_state(ball.id).await.ok().flatten().is_some_and(|cs| cs.is_cloaked) {
        return None;
    }
    let was_fighter = ctx.active_fighters.read().is_ok_and(|af| af.contains_key(&ball.id));
    let (victim_char_opt, is_sub_unit) = resolve_victim_owner(ctx, sys_handle, ball.id);
    if let Some(vcid) = victim_char_opt.filter(|&v| v != gunner_char_id) {
        let cw_target = if is_sub_unit { vcid } else { ball.id };
        if crate::services::combat_crimewatch::crimewatch_timers::evaluate_and_record_pvp_aggression(
            ctx,
            gunner_char_id,
            structure_id,
            sys_id,
            cw_target,
            ball.type_id == 670,
        )
        .is_err()
        {
            return None;
        }
        crate::sp6_lifecycle::record_pvp_aggression_and_renew_logoff_timer(ctx, gunner_char_id, vcid, now_unix);
        let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
        cw.get_or_create(gunner_char_id).pvp_timer_until_unix = now_unix.saturating_add(900);
        cw.get_or_create(vcid).pvp_timer_until_unix = now_unix.saturating_add(900);
    }

    let (applied, destroyed) = if dmg.total() > 0.0 {
        let (app, dest, _) =
            crate::services::station_structure::structure_lifecycle_rpc::apply_damage_structure_or_ball(
                ctx, sys_handle, sys_id, gunner_char_id, ball.id, dmg, cycle_secs, now_unix,
            )
            .await
            .ok()??;
        if was_fighter {
            crate::services::combat_crimewatch::apply_damage_to_fighter_squadron(ctx, sys_id, ball.id, dmg).await;
        }
        crate::services::combat_crimewatch::record_killmail_assist_action(
            ctx, ball.id, gunner_char_id, st_type_id, weapon_type_id, app, false, now_unix,
        );
        if let Some(vcid) = victim_char_opt.filter(|&v| v != gunner_char_id) {
            crate::services::combat_crimewatch::record_killmail_assist_action(
                ctx, vcid, gunner_char_id, st_type_id, weapon_type_id, app, false, now_unix,
            );
        }
        if dest && !was_fighter {
            crate::services::combat_crimewatch::combat_missiles_aoe::handle_destroyed_ball_from_missile_or_aoe(
                ctx, sys_id, gunner_char_id, ball.id, ball.type_id, ball.position, now_unix,
            )
            .await;
        }
        (app, dest)
    } else {
        (0.0, false)
    };

    if neut_amt > 0.0 && !destroyed {
        if let Ok(Some(cs)) = sys_handle.get_combat_state(ball.id).await {
            let _ = sys_handle.set_capacitor(ball.id, (cs.capacitor - neut_amt).max(0.0)).await;
        }
    }
    if victim_char_opt.is_none() && (applied > 0.0 || neut_amt > 0.0) {
        crate::sp6_lifecycle::record_npc_aggression_timer(ctx, gunner_char_id, now_unix);
    }
    Some((applied, destroyed))
}

async fn check_and_prepare_structure_cap(
    ctx: &ServerContext,
    sys_handle: &SystemHandle,
    structure_id: i64,
    st_type_id: u32,
    mod_type: &eve_sde::InvType,
) -> Result<(f64, f64), BridgeRpcError> {
    let cap_need = mod_type.default_attributes.get(&6).copied().unwrap_or(0.0);
    let sde_max_cap = ctx
        .sde
        .types
        .get(&st_type_id)
        .and_then(|t| t.default_attributes.get(&482).copied())
        .filter(|&c| c > 0.0)
        .unwrap_or(600_000.0);
    let mut cs = sys_handle.get_combat_state(structure_id).await.ok().flatten().unwrap_or_default();
    if cs.max_capacitor <= 0.0 {
        cs.max_capacitor = sde_max_cap;
        cs.capacitor = sde_max_cap;
        let _ = sys_handle.set_combat_state(structure_id, cs.clone()).await;
    }
    if cs.capacitor + 1e-6 < cap_need {
        return Err(err(
            "NotEnoughCapacitor",
            format!("Structure {structure_id} has {:.1} GJ capacitor; module requires {:.1} GJ", cs.capacitor, cap_need),
        ));
    }
    Ok((cs.capacitor, cap_need))
}

/// Steps in-flight Standup Guided Bombs (`group 1548`) in `sys_id` and detonates them on impact.
pub async fn step_guided_bombs_in_system(ctx: &ServerContext, sys_id: u32, dt: f64) {
    let bombs: Vec<InFlightGuidedBombRecord> = ctx.db.with_snapshot(|snap| {
        load_ext_state(snap)
            .in_flight_guided_bombs
            .values()
            .filter(|b| b.solar_system_id == sys_id)
            .cloned()
            .collect()
    });
    if bombs.is_empty() {
        return;
    }
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let now_unix = ctx.now_unix();
    let mut finished_ids = Vec::new();
    let mut updated = Vec::new();

    for mut rec in bombs {
        let Some(bomb_type) = ctx.sde.types.get(&rec.bomb_type_id) else {
            finished_ids.push(rec.bomb_id);
            let _ = sys_handle.remove_ball(rec.bomb_id).await;
            continue;
        };
        let bomb_snap = sys_handle.get_ball_snapshot(rec.bomb_id).await.ok().flatten();
        let bomb_dead = sys_handle
            .get_combat_state(rec.bomb_id)
            .await
            .ok()
            .flatten()
            .is_some_and(|cs| cs.max_structure_hp > 0.0 && cs.structure_hp <= 0.0);
        if bomb_snap.is_none() || bomb_dead {
            finished_ids.push(rec.bomb_id);
            let _ = sys_handle.remove_ball(rec.bomb_id).await;
            continue;
        }
        let (_, bomb_ball) = bomb_snap.unwrap();
        let Some((_, target_ball)) = sys_handle.get_ball_snapshot(rec.target_id).await.ok().flatten() else {
            finished_ids.push(rec.bomb_id);
            let _ = sys_handle.remove_ball(rec.bomb_id).await;
            continue;
        };

        let det_range = bomb_type.default_attributes.get(&108).copied().unwrap_or(50.0);
        let surface_dist = ((bomb_ball.position - target_ball.position).length()
            - target_ball.radius
            - bomb_ball.radius)
            .max(0.0);
        rec.remaining_flight_s -= dt;

        if surface_dist <= det_range.max(1_200.0) {
            finished_ids.push(rec.bomb_id);
            let _ = sys_handle.remove_ball(rec.bomb_id).await;
            let impact_pos = target_ball.position;
            let aoe_radius = bomb_type.default_attributes.get(&99).copied().filter(|&r| r > 0.0).unwrap_or(20_000.0);
            let exp_radius = bomb_type.default_attributes.get(&654).copied().filter(|&r| r > 0.0).unwrap_or(400.0);
            let base_dmg = read_omni_damage(bomb_type);
            let neut_amt = bomb_type.default_attributes.get(&97).copied().unwrap_or(0.0);
            let st_type_id = ctx
                .db
                .with_snapshot(|s| s.structures.get(&rec.structure_id).map(|st| st.type_id))
                .unwrap_or(TYPE_KEEPSTAR);
            let all_balls = sys_handle.list_balls().await.unwrap_or_default();

            for b in &all_balls {
                if (b.position - impact_pos).length() > aoe_radius {
                    continue;
                }
                if !is_hostile_to_structure(ctx, &sys_handle, rec.structure_id, rec.structure_corp_id, rec.gunner_char_id, sys_id, b) {
                    continue;
                }
                let sig = sys_handle
                    .get_combat_state(b.id)
                    .await
                    .ok()
                    .flatten()
                    .map(|cs| cs.signature_radius)
                    .filter(|&s| s > 0.0)
                    .or_else(|| ctx.sde.types.get(&b.type_id).and_then(|t| t.default_attributes.get(&552).copied()))
                    .unwrap_or_else(|| b.radius.max(35.0));
                let scale = (sig / exp_radius).clamp(0.05, 1.0);
                if base_dmg.total() > 0.0 || neut_amt > 0.0 {
                    let _ = apply_standup_hit(
                        ctx,
                        &sys_handle,
                        rec.structure_id,
                        st_type_id,
                        rec.bomb_type_id,
                        rec.gunner_char_id,
                        sys_id,
                        b,
                        base_dmg.scaled(scale),
                        neut_amt * scale,
                        12.0,
                        now_unix,
                    )
                    .await;
                }
            }
        } else if rec.remaining_flight_s <= 0.0 {
            finished_ids.push(rec.bomb_id);
            let _ = sys_handle.remove_ball(rec.bomb_id).await;
        } else {
            updated.push(rec);
        }
    }

    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        for bid in finished_ids {
            ext.in_flight_guided_bombs.remove(&bid);
        }
        for rec in updated {
            ext.in_flight_guided_bombs.insert(rec.bomb_id, rec);
        }
        save_ext_state(snap, &ext);
    });
}

/// Handles `dogmaIM.Activate` for Standup Point Defense Battery (`1330`),
/// Standup Guided Bomb Launcher (`1328`), and Standup Arcing Vorton Projector (`1333`).
#[allow(clippy::too_many_arguments)]
pub async fn try_activate_standup_weapon(
    ctx: &ServerContext,
    sys_handle: &SystemHandle,
    char_id: i64,
    structure_id: i64,
    sys_id: u32,
    module_id: i64,
    target_id_opt: Option<i64>,
    attacker_ball: &Ball,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let now_unix = ctx.now_unix();
    let lookup = ctx.db.with_snapshot(|snap| {
        let item_rec = snap.items.get(&module_id)?;
        let mod_type = ctx.sde.types.get(&item_rec.type_id)?;
        if !matches!(
            mod_type.group_id,
            GROUP_STANDUP_POINT_DEFENSE_BATTERY
                | GROUP_STANDUP_GUIDED_BOMB_LAUNCHER
                | GROUP_STANDUP_ARCING_VORTON
        ) {
            return None;
        }
        let online = snap.module_online_states.get(&module_id).copied().unwrap_or(false);
        let charge_tid = snap
            .items
            .values()
            .find(|i| {
                i.location_id == structure_id
                    && i.flag == item_rec.flag
                    && i.item_id != module_id
                    && i.quantity > 0
            })
            .map(|i| i.type_id);
        let ext = load_ext_state(snap);
        let active_gunner = ext.active_pilot_by_structure.get(&structure_id).copied();
        let immob_until = ext.gunner_immobilized_until_unix.get(&structure_id).copied().unwrap_or(0);
        let mod_until = ext.standup_module_cycle_until_unix.get(&module_id).copied().unwrap_or(0);
        let st_info = snap.structures.get(&structure_id).map(|s| (s.type_id, s.owner_corp_id, s.state.clone()));
        Some((
            item_rec.location_id,
            item_rec.flag,
            mod_type.clone(),
            online,
            charge_tid,
            active_gunner,
            immob_until,
            mod_until,
            st_info,
        ))
    })?;

    let (mod_loc_id, mod_flag, mod_type, is_online, slot_charge_tid, active_gunner, immob_until, mod_until, st_info) =
        lookup;

    let Some((st_type_id, st_corp_id, st_state)) = st_info else {
        return Some(Err(err("StructureNotFound", format!("Structure {structure_id} not found"))));
    };
    if st_state == "Destroyed" {
        return Some(Err(err("StructureDestroyed", format!("Structure {structure_id} is destroyed"))));
    }
    if active_gunner != Some(char_id) {
        return Some(Err(err("NotControllingStructure", "Character is not the active gunner of this structure")));
    }
    if mod_loc_id != structure_id || !(27..=34).contains(&(mod_flag as i32)) {
        return Some(Err(err("ModuleNotFitted", "Standup weapon must be fitted to a structure high slot")));
    }
    if !is_online {
        return Some(Err(err("ModuleNotOnline", format!("Module {module_id} is offline"))));
    }

    let sys_sec = ctx.sde.solar_systems.get(&sys_id).map(|s| s.security_status).unwrap_or(0.0);
    if sys_sec >= 0.45 && mod_type.default_attributes.get(&1970).is_some_and(|&v| v > 0.0) {
        return Some(Err(err(
            "CannotActivateInHighSec",
            format!("{} cannot be activated in high-security space", mod_type.name),
        )));
    }
    if now_unix < immob_until {
        return Some(Err(err(
            "WeaponCycleImmobilized",
            "Structure gunner weapon cycle is currently immobilized",
        )));
    }
    if now_unix < mod_until {
        return Some(Err(err("ModuleAlreadyCycling", format!("Module {module_id} is already cycling"))));
    }

    match mod_type.group_id {
        GROUP_STANDUP_POINT_DEFENSE_BATTERY => {
            if !structure_profile(ctx, st_type_id).is_some_and(|p| p.hull_class == HullClass::XLarge) {
                return Some(Err(err(
                    "RequiresXLStructure",
                    "Standup Point Defense Battery can only be activated on an XL Upwell structure",
                )));
            }
            let charge_group = mod_type.default_attributes.get(&604).copied().unwrap_or(GROUP_STANDUP_FLAK_AMMO as f64) as u32;
            let charge_rate = mod_type.default_attributes.get(&56).copied().unwrap_or(200.0).ceil().max(1.0) as i32;
            let Some(flak_tid) = find_structure_charge_type(
                ctx, structure_id, st_corp_id, module_id, mod_flag, charge_group, charge_rate, slot_charge_tid,
            ) else {
                return Some(Err(err(
                    "NoAmmoLoaded",
                    "Standup Point Defense Battery requires Standup Flak Rounds loaded in the weapon or structure ammo bay",
                )));
            };
            let Some(flak_type) = ctx.sde.types.get(&flak_tid) else {
                return Some(Err(err("IncompatibleAmmo", "Unknown Standup Flak Round charge type")));
            };
            let (cur_cap, cap_need) = match check_and_prepare_structure_cap(ctx, sys_handle, structure_id, st_type_id, &mod_type).await {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if !consume_structure_charge(ctx, structure_id, st_corp_id, module_id, mod_flag, flak_tid, charge_rate) {
                return Some(Err(err("NoAmmoLoaded", "Insufficient Standup Flak Rounds remaining")));
            }
            let _ = sys_handle.set_capacitor(structure_id, (cur_cap - cap_need).max(0.0)).await;

            let (optimal_range, falloff_range) = standup_weapon_optimal_and_falloff(&mod_type);
            let aoe_radius = optimal_range + 2.0 * falloff_range;
            let cycle_secs = mod_type
                .default_attributes
                .get(&73)
                .map(|&ms| (ms / 1000.0).ceil() as u64)
                .unwrap_or(12);
            let dmg_mult = mod_type.default_attributes.get(&64).copied().filter(|&m| m > 0.0).unwrap_or(1.0);
            let pulse_dmg = read_omni_damage(flak_type).scaled(dmg_mult);

            let all_balls = sys_handle.list_balls().await.unwrap_or_default();
            let mut targets_hit = 0_i64;
            let mut total_applied = 0.0_f64;
            let mut destroyed_count = 0_i64;

            for b in &all_balls {
                let dist = (b.position - attacker_ball.position).length();
                let Some(falloff_mult) = standup_falloff_multiplier(dist, optimal_range, falloff_range) else {
                    continue;
                };
                if !is_hostile_to_structure(ctx, sys_handle, structure_id, st_corp_id, char_id, sys_id, b) {
                    continue;
                }
                if let Some((applied, destroyed)) = apply_standup_hit(
                    ctx,
                    sys_handle,
                    structure_id,
                    st_type_id,
                    mod_type.type_id,
                    char_id,
                    sys_id,
                    b,
                    pulse_dmg.scaled(falloff_mult),
                    0.0,
                    cycle_secs as f64,
                    now_unix,
                )
                .await
                {
                    targets_hit += 1;
                    total_applied += applied;
                    if destroyed {
                        destroyed_count += 1;
                    }
                }
            }

            ctx.db.with_mut_snapshot(|snap| {
                let mut ext = load_ext_state(snap);
                ext.standup_module_cycle_until_unix.insert(module_id, now_unix.saturating_add(cycle_secs));
                save_ext_state(snap, &ext);
            });

            Some(Ok(PyRep::keyval([
                ("moduleID", PyRep::Integer(module_id)),
                ("structureID", PyRep::Integer(structure_id)),
                ("gunnerID", PyRep::Integer(char_id)),
                ("chargeTypeID", PyRep::Integer(flak_tid as i64)),
                ("aoeRadius", PyRep::Real(aoe_radius)),
                ("targetsHit", PyRep::Integer(targets_hit)),
                ("appliedDamage", PyRep::Real(total_applied)),
                ("destroyedCount", PyRep::Integer(destroyed_count)),
                ("cycleDurationSeconds", PyRep::Integer(cycle_secs as i64)),
            ])))
        }

        GROUP_STANDUP_GUIDED_BOMB_LAUNCHER => {
            let Some(tid) = target_id_opt else {
                return Some(Err(err("TargetRequired", "Standup Guided Bomb Launcher requires a locked target")));
            };
            let locks = sys_handle.get_target_locks(structure_id).await.unwrap_or_default();
            if !locks.iter().any(|l| l.target_id == tid && l.locked) {
                return Some(Err(err("TargetNotLocked", format!("Target {tid} is not locked"))));
            }
            let Some((_, target_ball)) = sys_handle.get_ball_snapshot(tid).await.ok().flatten() else {
                return Some(Err(err("TargetNotFound", format!("Target {tid} not in space"))));
            };

            let Some(bomb_tid) = find_structure_charge_type(
                ctx, structure_id, st_corp_id, module_id, mod_flag, GROUP_STANDUP_GUIDED_BOMB, 1, slot_charge_tid,
            ) else {
                return Some(Err(err(
                    "NoAmmoLoaded",
                    "Standup Guided Bomb Launcher requires a guided bomb loaded in the weapon or structure ammo bay",
                )));
            };
            let Some(bomb_type) = ctx.sde.types.get(&bomb_tid).filter(|t| t.group_id == GROUP_STANDUP_GUIDED_BOMB) else {
                return Some(Err(err("IncompatibleAmmo", "Loaded charge is not a Standup Guided Bomb")));
            };

            let bomb_vel = bomb_type.default_attributes.get(&37).copied().filter(|&v| v > 0.0).unwrap_or(2_500.0);
            let exp_delay_s = bomb_type.default_attributes.get(&281).copied().map(|ms| ms / 1000.0).filter(|&s| s > 0.0).unwrap_or(400.0);
            let launcher_cap = mod_type.default_attributes.get(&54).copied().filter(|&r| r > 0.0);
            let max_range = launcher_cap.map_or(bomb_vel * exp_delay_s, |c| c.min(bomb_vel * exp_delay_s));
            let dist = (target_ball.position - attacker_ball.position).length();
            if dist > max_range {
                return Some(Err(err(
                    "TargetOutOfRange",
                    format!("Target distance {:.0} m exceeds guided bomb maximum flight range {:.0} m", dist, max_range),
                )));
            }
            if !is_hostile_to_structure(ctx, sys_handle, structure_id, st_corp_id, char_id, sys_id, &target_ball) {
                return Some(Err(err("CantDoInCurrentSafetyLevel", "Cannot launch Standup Guided Bomb at friendly or safety-protected target")));
            }
            let (cur_cap, cap_need) = match check_and_prepare_structure_cap(ctx, sys_handle, structure_id, st_type_id, &mod_type).await {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if let Err(e) = crate::services::combat_crimewatch::crimewatch_timers::evaluate_and_record_pvp_aggression(
                ctx, char_id, structure_id, sys_id, tid, target_ball.type_id == 670,
            ) {
                return Some(Err(e));
            }
            if !consume_structure_charge(ctx, structure_id, st_corp_id, module_id, mod_flag, bomb_tid, 1) {
                return Some(Err(err("NoAmmoLoaded", "No guided bombs remaining in structure")));
            }
            let _ = sys_handle.set_capacitor(structure_id, (cur_cap - cap_need).max(0.0)).await;

            let aoe_radius = bomb_type.default_attributes.get(&99).copied().filter(|&r| r > 0.0).unwrap_or(20_000.0);
            let cycle_secs = mod_type
                .default_attributes
                .get(&51)
                .or_else(|| mod_type.default_attributes.get(&73))
                .map(|&ms| (ms / 1000.0).ceil() as u64)
                .unwrap_or(60);
            let bomb_hp = bomb_type.default_attributes.get(&9).copied().filter(|&h| h > 0.0).unwrap_or(4_000_000.0);
            let bomb_sig = bomb_type.default_attributes.get(&552).copied().filter(|&s| s > 0.0).unwrap_or(500.0);
            let dir = (target_ball.position - attacker_ball.position).normalize_or_zero();

            let (bomb_id, st_alliance) = ctx.db.with_mut_snapshot(|snap| {
                snap.next_entity_id += 1;
                let bid = snap.next_entity_id;
                let all_id = snap.corporations.get(&st_corp_id).and_then(|c| c.alliance_id).unwrap_or(-1);
                let mut ext = load_ext_state(snap);
                ext.standup_module_cycle_until_unix.insert(module_id, now_unix.saturating_add(cycle_secs));
                ext.in_flight_guided_bombs.insert(
                    bid,
                    InFlightGuidedBombRecord {
                        bomb_id: bid,
                        structure_id,
                        structure_corp_id: st_corp_id,
                        gunner_char_id: char_id,
                        solar_system_id: sys_id,
                        target_id: tid,
                        bomb_type_id: bomb_tid,
                        remaining_flight_s: exp_delay_s,
                    },
                );
                save_ext_state(snap, &ext);
                (bid, all_id)
            });

            let bomb_ball = Ball {
                id: bomb_id,
                type_id: bomb_tid,
                position: attacker_ball.position,
                velocity: dir * bomb_vel,
                max_velocity: bomb_vel,
                inertia: 0.05,
                mass: bomb_type.mass.max(1.0),
                radius: bomb_type.radius.max(25.0),
                speed_fraction: 1.0,
                mode: MovementMode::Follow { target_id: tid, range: 0.0 },
            };
            let bomb_cs = CombatState {
                structure_hp: bomb_hp,
                max_structure_hp: bomb_hp,
                signature_radius: bomb_sig,
                ..CombatState::default()
            };
            let _ = sys_handle.spawn_ball_with_meta(
                bomb_ball,
                bomb_cs,
                eve_destiny::SlimItemMeta {
                    group_id: GROUP_STANDUP_GUIDED_BOMB,
                    category_id: 8,
                    owner_id: st_corp_id,
                    char_id: Some(char_id),
                    corp_id: st_corp_id,
                    alliance_id: st_alliance,
                    security_status: 0.0,
                    is_cloaked: false,
                },
            ).await;

            Some(Ok(PyRep::keyval([
                ("moduleID", PyRep::Integer(module_id)),
                ("bombID", PyRep::Integer(bomb_id)),
                ("targetID", PyRep::Integer(tid)),
                ("chargeTypeID", PyRep::Integer(bomb_tid as i64)),
                ("solarSystemID", PyRep::Integer(sys_id as i64)),
                ("maxRange", PyRep::Real(max_range)),
                ("aoeRadius", PyRep::Real(aoe_radius)),
                ("inFlight", PyRep::Bool(true)),
                ("cycleDurationSeconds", PyRep::Integer(cycle_secs as i64)),
            ])))
        }

        GROUP_STANDUP_ARCING_VORTON => {
            if !matches!(st_type_id, TYPE_KEEPSTAR | TYPE_PALATINE_KEEPSTAR) {
                return Some(Err(err(
                    "CantFitToStructure",
                    "Standup Arcing Vorton Projector can only be operated on a Keepstar",
                )));
            }
            let Some(tid) = target_id_opt else {
                return Some(Err(err("TargetRequired", "Standup Arcing Vorton Projector requires a locked primary target")));
            };
            let locks = sys_handle.get_target_locks(structure_id).await.unwrap_or_default();
            if !locks.iter().any(|l| l.target_id == tid && l.locked) {
                return Some(Err(err("TargetNotLocked", format!("Target {tid} is not locked"))));
            }
            let Some((_, target_ball)) = sys_handle.get_ball_snapshot(tid).await.ok().flatten() else {
                return Some(Err(err("TargetNotFound", format!("Target {tid} not in space"))));
            };

            let (optimal_range, falloff_range) = standup_weapon_optimal_and_falloff(&mod_type);
            let max_range = optimal_range + 2.0 * falloff_range;
            let dist = (target_ball.position - attacker_ball.position).length();
            let Some(primary_falloff_mult) = standup_falloff_multiplier(dist, optimal_range, falloff_range) else {
                return Some(Err(err(
                    "TargetOutOfRange",
                    format!(
                        "Primary target distance {:.0} m exceeds Standup Arcing Vorton Projector optimal+2*falloff range {:.0} m",
                        dist, max_range
                    ),
                )));
            };
            if !is_hostile_to_structure(ctx, sys_handle, structure_id, st_corp_id, char_id, sys_id, &target_ball) {
                return Some(Err(err("CantDoInCurrentSafetyLevel", "Cannot fire Standup Arcing Vorton Projector at friendly or safety-protected target")));
            }
            let (cur_cap, cap_need) = match check_and_prepare_structure_cap(ctx, sys_handle, structure_id, st_type_id, &mod_type).await {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if let Err(e) = crate::services::combat_crimewatch::crimewatch_timers::evaluate_and_record_pvp_aggression(
                ctx, char_id, structure_id, sys_id, tid, target_ball.type_id == 670,
            ) {
                return Some(Err(e));
            }
            let _ = sys_handle.set_capacitor(structure_id, (cur_cap - cap_need).max(0.0)).await;

            let jump_radius = mod_type
                .default_attributes
                .get(&2105)
                .or_else(|| mod_type.default_attributes.get(&99))
                .copied()
                .filter(|&r| r > 0.0)
                .unwrap_or(20_000.0);
            let max_total = mod_type.default_attributes.get(&2104).copied().unwrap_or(5.0).max(1.0) as usize;
            let max_secondary = max_total.saturating_sub(1);
            let loss_per_hop = mod_type.default_attributes.get(&2106).copied().filter(|&l| l > 0.0).unwrap_or(0.85);
            let cycle_secs = mod_type
                .default_attributes
                .get(&73)
                .or_else(|| mod_type.default_attributes.get(&51))
                .map(|&ms| (ms / 1000.0).ceil() as u64)
                .unwrap_or(600);
            let immob_secs = mod_type
                .default_attributes
                .get(&1579)
                .map(|&ms| (ms / 1000.0).ceil() as u64)
                .filter(|&s| s > 0)
                .unwrap_or(cycle_secs);
            let base_dmg = read_omni_damage(&mod_type).scaled(primary_falloff_mult);

            let all_balls = sys_handle.list_balls().await.unwrap_or_default();
            let ball_map: std::collections::HashMap<i64, Ball> = all_balls.into_iter().map(|b| (b.id, b)).collect();
            let mut candidates: Vec<(i64, DVec3)> = Vec::new();
            for b in ball_map.values() {
                if b.id == tid || !is_hostile_to_structure(ctx, sys_handle, structure_id, st_corp_id, char_id, sys_id, b) {
                    continue;
                }
                if sys_handle.get_combat_state(b.id).await.ok().flatten().is_some_and(|cs| cs.is_cloaked) {
                    continue;
                }
                candidates.push((b.id, b.position));
            }
            let hit_ids = vorton_arc_targets_with_cap(tid, target_ball.position, &candidates, jump_radius, max_secondary);

            let mut total_applied = 0.0_f64;
            let mut targets_hit = 0_i64;
            for (hop_idx, hid) in hit_ids.iter().enumerate() {
                let Some(hb) = ball_map.get(hid) else {
                    continue;
                };
                let hop_dmg = base_dmg.scaled(loss_per_hop.powi(hop_idx as i32));
                if let Some((applied, _)) = apply_standup_hit(
                    ctx,
                    sys_handle,
                    structure_id,
                    st_type_id,
                    mod_type.type_id,
                    char_id,
                    sys_id,
                    hb,
                    hop_dmg,
                    0.0,
                    cycle_secs as f64,
                    now_unix,
                )
                .await
                {
                    targets_hit += 1;
                    total_applied += applied;
                }
            }

            let immobilized_until = now_unix.saturating_add(immob_secs);
            ctx.db.with_mut_snapshot(|snap| {
                let mut ext = load_ext_state(snap);
                ext.gunner_immobilized_until_unix.insert(structure_id, immobilized_until);
                ext.standup_module_cycle_until_unix.insert(module_id, now_unix.saturating_add(cycle_secs));
                save_ext_state(snap, &ext);
            });

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
