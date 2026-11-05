//! `tactical_modules` — Dogma and Combat Tactical Modules (`§3.12`, `DG-4`..`DG-6`, `feat:modules.*`).

#![forbid(unsafe_code)]

use crate::router::{NotificationTarget, ServerContext};
use crate::services::inventory_fitting::{scanners_superweapons_state, ActiveDbuffEntry};
use crate::system_actor::SystemHandle;
use eve_destiny::{Ball, DamageVector, DVec3, TargetLock};
use eve_dogma::{DogmaItemState, ShipFittingEngine};
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::{attr_ids, InvType, TypeId};
use std::collections::HashMap;
use std::sync::Arc;

pub use super::tactical_modules_probes::*;

#[derive(Debug, Clone)]
pub struct ProjectedRemoteAssistance {
    pub source_ship_id: i64,
    pub module_id: i64,
    pub module_type_id: u32,
    pub max_target_range_bonus_pct: f64,
    pub scan_resolution_bonus_pct: f64,
    pub sensor_strength_bonus_pct: f64,
    pub optimal_range_bonus_pct: f64,
    pub falloff_bonus_pct: f64,
    pub tracking_speed_bonus_pct: f64,
}

#[derive(Debug, Clone)]
pub struct ActiveEntosisLinkState {
    pub ship_id: i64,
    pub char_id: i64,
    pub side_id: i64,
    pub module_id: i64,
    pub target_id: i64,
    pub cycle_index: u32,
    pub cycle_duration_s: f64,
    pub started_unix: u64,
    pub expires_unix: u64,
    pub is_capital: bool,
}

#[derive(Default)]
pub struct TacticalModulesState {
    pub remote_assistance: HashMap<(usize, i64), Vec<ProjectedRemoteAssistance>>,
    pub lance_caster_lock_until: HashMap<(usize, i64), u64>,
    pub lance_victim_debuff_until: HashMap<(usize, i64), u64>,
    pub entosis_active_links: HashMap<(usize, i64), ActiveEntosisLinkState>,
    pub entosis_target_cycles: HashMap<(usize, i64, i64), u32>,
    pub pending_wubble_probes: Vec<(usize, PendingWubbleProbe)>,
    pub active_wubble_debuffs: HashMap<(usize, i64), ActiveWubbleDebuff>,
    pub tsb_self_penalty_until: HashMap<(usize, i64), u64>,
    pub clone_vat_bay_activated_ships: std::collections::HashSet<(usize, i64)>,
    pub active_interdiction_probes: HashMap<(usize, i64), super::tactical_module_mechanics::ActiveInterdictionProbe>,
    pub launcher_last_fire: HashMap<(usize, i64), super::tactical_module_mechanics::LauncherFireStamp>,
    pub system_sim_time_s: HashMap<(usize, u32), f64>,
}

pub fn is_clone_vat_bay_active_required(ctx: &ServerContext, ship_id: i64) -> bool {
    let db_key = db_key_of(ctx);
    ctx.tactical_state
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone_vat_bay_activated_ships
        .contains(&(db_key, ship_id))
}

fn db_key_of(ctx: &ServerContext) -> usize { db_ptr_key(&ctx.db) }
pub(crate) fn db_ptr_key(db: &Arc<eve_db::WorldRepository>) -> usize { Arc::as_ptr(db) as usize }

pub fn has_lance_caster_lock(ctx: &ServerContext, ship_id: i64) -> bool {
    let now_unix = ctx.now_unix_secs();
    let (db_key, inst_key) = (db_key_of(ctx), ctx.instance_id as usize);
    let st = ctx.tactical_state.read().unwrap_or_else(|e| e.into_inner());
    st.lance_caster_lock_until.get(&(db_key, ship_id)).or_else(|| st.lance_caster_lock_until.get(&(inst_key, ship_id))).is_some_and(|&until| now_unix < until)
}

/// Returns `Err` if `ship_id` is an anchored Upwell structure or is under a Disruptive Lance 300s caster lock, 60s victim debuff, or active Entosis Link cycle.
pub fn check_ship_navigation_lock(ctx: &ServerContext, ship_id: i64, action: &str) -> Result<(), BridgeRpcError> {
    crate::services::structure_control_ops::reject_if_controlling_structure(ctx, ship_id, action)?;
    let now_unix = ctx.now_unix_secs();
    let (db_key, inst_key) = (db_key_of(ctx), ctx.instance_id as usize);
    let st = ctx.tactical_state.read().unwrap_or_else(|e| e.into_inner());
    if let Some(&until) = st.lance_caster_lock_until.get(&(db_key, ship_id)).or_else(|| st.lance_caster_lock_until.get(&(inst_key, ship_id))) {
        if now_unix < until {
            return Err(BridgeRpcError { code: "LanceFiringLockActive".into(), message: format!("Cannot {action} while Disruptive Lance 5-minute firing lock is active ({}s remaining)", until - now_unix) });
        }
    }
    if let Some(&until) = st.lance_victim_debuff_until.get(&(db_key, ship_id)).or_else(|| st.lance_victim_debuff_until.get(&(inst_key, ship_id))) {
        if now_unix < until {
            return Err(BridgeRpcError { code: "DisruptiveLanceDebuffActive".into(), message: format!("Cannot {action} while Disruptive Lance debuff is active ({}s remaining)", until - now_unix) });
        }
    }
    if let Some(ent) = st.entosis_active_links.get(&(db_key, ship_id)).or_else(|| st.entosis_active_links.get(&(inst_key, ship_id))) {
        if now_unix < ent.expires_unix {
            return Err(BridgeRpcError { code: "EntosisLinkActive".into(), message: format!("Cannot {action} while Entosis Link cycle is active ({}s remaining)", ent.expires_unix - now_unix) });
        }
    }
    if crate::services::inventory_fitting::fitting_cyno_portal::is_ship_cyno_active(ctx, ship_id) {
        return Err(BridgeRpcError { code: "CynosuralFieldActive".into(), message: format!("Cannot {action} while Cynosural Field Generator is active") });
    }
    drop(st);
    if crate::services::sov_gameplay::is_ship_skyhook_linked(ctx, ship_id, now_unix) {
        return Err(BridgeRpcError { code: "SkyhookLinkActive".into(), message: format!("Cannot {action} while linked to an Orbital Skyhook Reagent Silo") });
    }
    if crate::services::pve_crab_beacons::is_ship_crab_linked(ctx, ship_id, now_unix) {
        return Err(BridgeRpcError { code: "CrabLinkActive".into(), message: format!("Cannot {action} while linked to a CONCORD Rogue Analysis Beacon") });
    }
    Ok(())
}

/// Returns the incoming remote repair multiplier for `target_ship_id` (`0.50` when under a Disruptive Lance victim debuff, `1.0` otherwise).
pub fn incoming_remote_repair_multiplier(ctx: &ServerContext, target_ship_id: i64) -> f64 {
    let now_unix = ctx.now_unix_secs();
    let (db_key, inst_key) = (db_key_of(ctx), ctx.instance_id as usize);
    let st = ctx.tactical_state.read().unwrap_or_else(|e| e.into_inner());
    if st.lance_victim_debuff_until.get(&(db_key, target_ship_id)).or_else(|| st.lance_victim_debuff_until.get(&(inst_key, target_ship_id))).is_some_and(|&u| now_unix < u) { 0.50 } else { 1.0 }
}

/// Clears Entosis Link warm-up state when `ship_id` unlocks `target_id` (or all targets when `target_id_opt` is `None`).
pub fn clear_entosis_warmup_on_unlock(ctx: &ServerContext, ship_id: i64, target_id_opt: Option<i64>) {
    let db_key = db_key_of(ctx);
    let mut st = ctx.tactical_state.write().unwrap_or_else(|e| e.into_inner());
    if let Some(tid) = target_id_opt {
        st.entosis_target_cycles.remove(&(db_key, tid, ship_id));
        if st.entosis_active_links.get(&(db_key, ship_id)).is_some_and(|e| e.target_id == tid) {
            st.entosis_active_links.remove(&(db_key, ship_id));
        }
    } else {
        st.entosis_target_cycles.retain(|&(k, _, sid), _| k != db_key || sid != ship_id);
        st.entosis_active_links.remove(&(db_key, ship_id));
    }
}

/// Removes any projected Remote Sensor Booster / Remote Tracking Computer entry from `(source_ship_id, source_module_id)`.
pub fn remove_projected_remote_assistance(ctx: &ServerContext, source_ship_id: i64, source_module_id: i64) {
    let db_key = db_key_of(ctx);
    let mut affected = Vec::new();
    {
        let mut st = ctx.tactical_state.write().unwrap_or_else(|e| e.into_inner());
        for (&(k, tid), list) in st.remote_assistance.iter_mut() {
            if k == db_key {
                let before = list.len();
                list.retain(|e| !(e.source_ship_id == source_ship_id && e.module_id == source_module_id));
                if list.len() != before { affected.push(tid); }
            }
        }
    }
    for tid in affected { ctx.invalidate_ship_fitting(tid); }
}

/// Validates and projects a Remote Sensor Booster (`group 290`) or Remote Tracking Computer (`group 209`) onto `target_id`.
#[allow(clippy::too_many_arguments)]
pub fn apply_projected_remote_assistance(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    module_id: i64,
    mod_type: &InvType,
    mod_state: &DogmaItemState,
    target_id_opt: Option<i64>,
    dist_m: f64,
) -> Result<(), BridgeRpcError> {
    let Some(target_id) = target_id_opt else {
        return Err(BridgeRpcError {
            code: "TargetRequired".into(),
            message: format!("{} requires a locked target", mod_type.name),
        });
    };
    let optimal = mod_state.get_attribute(54, &ctx.sde);
    let falloff = mod_state.get_attribute(2044, &ctx.sde);
    if optimal > 0.0 && dist_m > optimal + 2.0 * falloff + 1.0 {
        return Err(BridgeRpcError {
            code: "OutOfRange".into(),
            message: format!("{} target at {dist_m:.1}m exceeds optimal ({optimal:.1}m) + 2*falloff ({falloff:.1}m)", mod_type.name),
        });
    }
    let falloff_mult = if optimal <= 0.0 || dist_m <= optimal || falloff <= 0.0 {
        1.0
    } else {
        0.5_f64.powf(((dist_m - optimal) / falloff).powi(2))
    };

    // Propagate logistics aggression flags from target owner to logi pilot
    let target_owner_opt = ctx.db.with_snapshot(|snap| snap.items.get(&target_id).map(|i| i.owner_id));
    if let Some(target_owner_id) = target_owner_opt.filter(|&oid| oid > 0 && oid != char_id) {
        let now_unix = ctx.now_unix_secs();
        let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
        let (t_weap, t_pvp, t_susp, t_crim) = {
            let ts = cw.get_or_create(target_owner_id);
            (ts.weapon_timer_until_unix, ts.pvp_timer_until_unix, ts.suspect_flag_until_unix, ts.criminal_flag_until_unix)
        };
        let cs = cw.get_or_create(char_id);
        if t_weap > now_unix {
            cs.weapon_timer_until_unix = cs.weapon_timer_until_unix.max(t_weap);
        }
        if t_pvp > now_unix {
            cs.pvp_timer_until_unix = cs.pvp_timer_until_unix.max(t_pvp);
        }
        if t_susp > now_unix || t_crim > now_unix {
            cs.suspect_flag_until_unix = cs.suspect_flag_until_unix.max(t_susp).max(t_crim);
        }
    }

    let entry = if mod_type.group_id == 290 {
        ProjectedRemoteAssistance {
            source_ship_id: ship_id,
            module_id,
            module_type_id: mod_type.type_id,
            max_target_range_bonus_pct: mod_state.get_attribute(309, &ctx.sde) * falloff_mult,
            scan_resolution_bonus_pct: mod_state.get_attribute(566, &ctx.sde) * falloff_mult,
            sensor_strength_bonus_pct: mod_state.get_attribute(1027, &ctx.sde).max(mod_state.get_attribute(1028, &ctx.sde)) * falloff_mult,
            optimal_range_bonus_pct: 0.0,
            falloff_bonus_pct: 0.0,
            tracking_speed_bonus_pct: 0.0,
        }
    } else {
        ProjectedRemoteAssistance {
            source_ship_id: ship_id,
            module_id,
            module_type_id: mod_type.type_id,
            max_target_range_bonus_pct: 0.0,
            scan_resolution_bonus_pct: 0.0,
            sensor_strength_bonus_pct: 0.0,
            optimal_range_bonus_pct: mod_state.get_attribute(351, &ctx.sde) * falloff_mult,
            falloff_bonus_pct: mod_state.get_attribute(349, &ctx.sde) * falloff_mult,
            tracking_speed_bonus_pct: mod_state.get_attribute(767, &ctx.sde) * falloff_mult,
        }
    };
    let db_key = db_key_of(ctx);
    let mut st = ctx.tactical_state.write().unwrap_or_else(|e| e.into_inner());
    let list = st.remote_assistance.entry((db_key, target_id)).or_default();
    list.retain(|e| e.module_id != module_id);
    list.push(entry);
    drop(st);
    ctx.invalidate_ship_fitting(target_id);
    Ok(())
}

/// Applies V24 Bastion, Siege, Triage, CEHE, TSB, Entosis, Cargohold Rig, Wubble, and Remote Assistance Impedance attributes.
pub fn apply_tactical_module_ship_attributes(ctx: &ServerContext, ship_id: i64, attrs: &mut HashMap<u16, f64>) {
    let (owner_id, ship_type_id, active_mod_tids, online_mod_tids) = ctx.db.with_snapshot(|snap| {
        let Some(ship) = snap.items.get(&ship_id) else {
            return (0, 0, Vec::new(), Vec::new());
        };
        let mut act = Vec::new();
        let mut onl = Vec::new();
        for item in snap.items.values() {
            if item.location_id == ship_id && matches!(item.flag as u16, 11..=34 | 92..=94) {
                if snap.module_online_states.get(&item.item_id).copied().unwrap_or(true) {
                    onl.push(item.type_id);
                    if snap.module_active_states.contains(&item.item_id) {
                        act.push(item.type_id);
                    }
                }
            }
        }
        (ship.owner_id, ship.type_id, act, onl)
    });
    if ship_type_id == 0 {
        return;
    }

    if owner_id > 0 {
        if let Ok(fit) = ctx.get_or_rebuild_ship_fitting(owner_id, ship_id) {
            let eval_ctx = fit.evaluated_context(&ctx.sde);
            let dcd = eval_ctx.character.get_attribute(458, &ctx.sde);
            if dcd > 0.0 {
                attrs.insert(458, dcd);
            }
        }
    }

    let cargo_rig_count = online_mod_tids.iter().filter(|&&tid| ctx.sde.types.get(&tid).is_some_and(|t| t.group_id == 782)).count();
    if cargo_rig_count > 0 {
        if let Some(vel) = attrs.get_mut(&37) {
            *vel *= 0.90_f64.powi(cargo_rig_count as i32);
        }
    }

    if online_mod_tids.iter().any(|&tid| ctx.sde.types.get(&tid).is_some_and(|t| t.group_id == 1313)) {
        if let Some(vel) = attrs.get_mut(&37) {
            if *vel > 4_000.0 {
                *vel = 4_000.0;
            }
        }
    }

    for &mod_tid in active_mod_tids.iter() {
        if let Some(m_type) = ctx.sde.types.get(&mod_tid) {
            if m_type.effect_ids.contains(&6484) || matches!(mod_tid, 40714..=40718) {
                if let Some(&em) = m_type.default_attributes.get(&974) {
                    attrs.insert(113, em);
                }
                if let Some(&exp) = m_type.default_attributes.get(&975) {
                    attrs.insert(111, exp);
                }
                if let Some(&kin) = m_type.default_attributes.get(&976) {
                    attrs.insert(109, kin);
                }
                if let Some(&therm) = m_type.default_attributes.get(&977) {
                    attrs.insert(110, therm);
                }
            }
        }
    }

    let db_key = db_key_of(ctx);
    let now_unix = ctx.now_unix_secs();
    if ctx.tactical_state.read().unwrap_or_else(|e| e.into_inner()).tsb_self_penalty_until.get(&(db_key, ship_id)).is_some_and(|&u| now_unix < u) {
        if let Some(sr) = attrs.get_mut(&564) {
            *sr *= 0.75;
        }
        for s_aid in 208..=211 {
            if let Some(v) = attrs.get_mut(&s_aid) {
                *v *= 0.75;
            }
        }
    }

    let has_bastion = active_mod_tids.contains(&33400);
    let has_siege_t1 = active_mod_tids.contains(&20280);
    let has_siege_t2 = active_mod_tids.contains(&4292);
    let has_triage_t1 = active_mod_tids.contains(&27951);
    let has_triage_t2 = active_mod_tids.contains(&4294);

    if has_bastion {
        attrs.insert(2135, 0.05);
        attrs.insert(2045, 0.0);
        attrs.insert(2112, 1.0);
        attrs.insert(2113, 1.0);
        attrs.insert(2115, 1.0);
    } else if has_siege_t1 || has_siege_t2 {
        let ew_res = if has_siege_t2 { 0.30 } else { 0.40 };
        attrs.insert(2135, 0.20);
        attrs.insert(2045, 0.0);
        attrs.insert(2112, ew_res);
        attrs.insert(2113, ew_res);
        attrs.insert(2114, 1.0);
        attrs.insert(1795, 0.0);
        if let Some(ship_t) = ctx.sde.types.get(&ship_type_id) {
            attrs.insert(4, ship_t.mass * 10.0);
        }
    } else if has_triage_t1 || has_triage_t2 {
        let ew_res = if has_triage_t2 { 0.30 } else { 0.40 };
        attrs.insert(2135, 0.20);
        attrs.insert(2045, 0.0);
        attrs.insert(2112, ew_res);
        attrs.insert(2113, ew_res);
        attrs.insert(1795, 0.0);
        attrs.insert(1271, 0.0);
        let bonus_targets = if has_triage_t2 { 5.0 } else { 4.0 };
        let base_targets = ctx.sde.types.get(&ship_type_id).and_then(|t| t.default_attributes.get(&192).copied()).unwrap_or(7.0);
        let cur_targets = attrs.get(&192).copied().unwrap_or(base_targets);
        if cur_targets < base_targets + bonus_targets {
            attrs.insert(192, base_targets + bonus_targets);
        }
        if let Some(ship_t) = ctx.sde.types.get(&ship_type_id) {
            attrs.insert(4, ship_t.mass * 10.0);
        }
    }

    let assist_mult = attrs.get(&2135).copied().unwrap_or(1.0);
    let proj_list = ctx.tactical_state.read().unwrap_or_else(|e| e.into_inner()).remote_assistance.get(&(db_key, ship_id)).cloned().unwrap_or_default();
    if !proj_list.is_empty() {
        let rs_extractors: [(u16, fn(&ProjectedRemoteAssistance) -> f64); 2] = [
            (76u16, |p: &ProjectedRemoteAssistance| p.max_target_range_bonus_pct),
            (564u16, |p: &ProjectedRemoteAssistance| p.scan_resolution_bonus_pct),
        ];
        for (aid, extract) in rs_extractors {
            let mut bonuses: Vec<f64> = proj_list.iter().map(|p| extract(p) * assist_mult).filter(|&v| v.abs() > 1e-6).collect();
            bonuses.sort_by(|a, b| b.abs().partial_cmp(&a.abs()).unwrap_or(std::cmp::Ordering::Equal));
            for (i, bonus) in bonuses.into_iter().enumerate() {
                if let Some(v) = attrs.get_mut(&aid) {
                    *v *= 1.0 + (bonus * eve_dogma::stacking_penalty_factor(i)) / 100.0;
                }
            }
        }
        let mut sensor_bonuses: Vec<f64> = proj_list.iter().map(|p| p.sensor_strength_bonus_pct * assist_mult).filter(|&v| v.abs() > 1e-6).collect();
        sensor_bonuses.sort_by(|a, b| b.abs().partial_cmp(&a.abs()).unwrap_or(std::cmp::Ordering::Equal));
        for (i, bonus) in sensor_bonuses.into_iter().enumerate() {
            let pen = eve_dogma::stacking_penalty_factor(i);
            for s_aid in 208..=211 {
                if let Some(v) = attrs.get_mut(&s_aid) {
                    *v *= 1.0 + (bonus * pen) / 100.0;
                }
            }
        }
    }

    let db_key = db_key_of(ctx);
    let ptr_key = db_ptr_key(&ctx.db);
    let st_guard = ctx.tactical_state.read().unwrap_or_else(|e| e.into_inner());
    if let Some(debuff) = st_guard
        .active_wubble_debuffs
        .get(&(db_key, ship_id))
        .or_else(|| st_guard.active_wubble_debuffs.get(&(ptr_key, ship_id)))
    {
        if debuff.remaining_s > 0.0 {
            if let Some(v) = attrs.get_mut(&37) {
                *v *= debuff.velocity_multiplier;
            }
        }
    }
}

/// Applies projected Remote Tracking Computer bonuses (with stacking penalty and `remoteAssistanceImpedance`) to module attributes.
pub fn apply_projected_rtc_to_module_attrs(ctx: &ServerContext, ship_id: i64, mod_attrs: &mut HashMap<u16, f64>) {
    let db_key = db_key_of(ctx);
    let proj_list = ctx.tactical_state.read().unwrap_or_else(|e| e.into_inner()).remote_assistance.get(&(db_key, ship_id)).cloned().unwrap_or_default();
    if proj_list.is_empty() {
        return;
    }
    let active_mod_tids: Vec<u32> = ctx.db.with_snapshot(|snap| {
        snap.items.values().filter(|i| i.location_id == ship_id && snap.module_active_states.contains(&i.item_id)).map(|i| i.type_id).collect()
    });
    let assist_mult = if active_mod_tids.contains(&33400) {
        0.05
    } else if active_mod_tids.iter().any(|&tid| matches!(tid, 20280 | 4292 | 27951 | 4294)) {
        0.20
    } else {
        1.0
    };
    let rtc_extractors: [(u16, fn(&ProjectedRemoteAssistance) -> f64); 3] = [
        (54u16, |p: &ProjectedRemoteAssistance| p.optimal_range_bonus_pct),
        (158u16, |p: &ProjectedRemoteAssistance| p.falloff_bonus_pct),
        (160u16, |p: &ProjectedRemoteAssistance| p.tracking_speed_bonus_pct),
    ];
    for (aid, extract) in rtc_extractors {
        let mut bonuses: Vec<f64> = proj_list.iter().map(|p| extract(p) * assist_mult).filter(|&v| v.abs() > 1e-6).collect();
        bonuses.sort_by(|a, b| b.abs().partial_cmp(&a.abs()).unwrap_or(std::cmp::Ordering::Equal));
        for (i, bonus) in bonuses.into_iter().enumerate() {
            if let Some(v) = mod_attrs.get_mut(&aid) {
                *v *= 1.0 + (bonus * eve_dogma::stacking_penalty_factor(i)) / 100.0;
            }
        }
    }
}

/// Activates a Target Spectrum Breaker / Signature Radius Suppressor (`group_id == 1154`, e.g. `4409`).
pub async fn activate_target_spectrum_breaker(
    ctx: &ServerContext,
    sys: &SystemHandle,
    _char_id: i64,
    ship_id: i64,
    module_id: i64,
    mod_type: &InvType,
    mod_state: &DogmaItemState,
) -> Result<PyRep, BridgeRpcError> {
    let cap_need = mod_state.get_attribute(6, &ctx.sde);
    let duration_s = (mod_state.get_attribute(3115, &ctx.sde).max(mod_state.get_attribute(73, &ctx.sde)) / 1000.0).max(12.0);
    let react_delay_s = (mod_state.get_attribute(669, &ctx.sde) / 1000.0).max(150.0);

    if let Ok(Some(cs)) = sys.get_combat_state(ship_id).await {
        if cs.capacitor + 1e-4 < cap_need {
            return Err(BridgeRpcError {
                code: "NotEnoughCapacitor".into(),
                message: format!("Insufficient capacitor for {}: need {cap_need:.1} GJ", mod_type.name),
            });
        }
        let _ = sys.set_capacitor(ship_id, (cs.capacitor - cap_need).max(0.0)).await;
    }

    let all_balls = sys.list_balls().await.unwrap_or_default();
    let mut locking_ships: Vec<(i64, f64)> = Vec::new();
    for b in &all_balls {
        if b.id != ship_id && is_combatant_ship_or_entity_ball(Some(&ctx.sde), Some(&ctx.db), b.id, b.type_id) {
            let locks = sys.get_target_locks(b.id).await.unwrap_or_default();
            if locks.iter().any(|l| l.target_id == ship_id) {
                let sensor_str = ctx.db.with_snapshot(|snap| {
                    snap.items.get(&b.id).and_then(|it| ctx.sde.types.get(&it.type_id)).map(|t| {
                        (208u16..=211).filter_map(|aid| t.default_attributes.get(&aid).copied()).fold(20.0_f64, f64::max)
                    }).unwrap_or(20.0)
                });
                locking_ships.push((b.id, sensor_str));
            }
        }
    }

    let num_locking = locking_ships.len();
    let sig_mult = mod_state.get_attribute(3114, &ctx.sde).clamp(0.1, 0.95);
    let base_mod_strength = (1.0 - sig_mult) * 25.0;
    let effective_mod_strength = base_mod_strength * (num_locking.max(1) as f64);

    let mut broken_ship_ids = Vec::new();
    let mut max_prob = 0.0_f64;
    for (locker_id, sensor_str) in locking_ships {
        let prob = effective_mod_strength / (effective_mod_strength + sensor_str.max(1.0));
        max_prob = max_prob.max(prob);
        if prob >= 0.50 {
            let _ = sys.remove_target_lock(locker_id, ship_id).await;
            broken_ship_ids.push(locker_id);
        }
    }

    let db_key = db_key_of(ctx);
    let now_unix = ctx.now_unix_secs();
    ctx.tactical_state.write().unwrap_or_else(|e| e.into_inner()).tsb_self_penalty_until.insert((db_key, ship_id), now_unix + (duration_s.round() as u64).max(12));
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.insert(module_id);
    });
    ctx.record_module_reactivation_delay(ship_id, module_id, react_delay_s);
    ctx.invalidate_ship_fitting(ship_id);

    Ok(PyRep::keyval([
        ("moduleID", PyRep::Integer(module_id)),
        ("active", PyRep::Bool(true)),
        ("lockingShipCount", PyRep::Integer(num_locking as i64)),
        ("lockbreakProbability", PyRep::Real(max_prob)),
        ("brokenLocks", PyRep::Integer(broken_ship_ids.len() as i64)),
        ("brokenLockShipIDs", PyRep::List(broken_ship_ids.into_iter().map(PyRep::Integer).collect())),
        ("selfSensorMultiplier", PyRep::Real(0.75)),
        ("sigRadiusMultiplier", PyRep::Real(sig_mult)),
    ]))
}

/// Activates an Automated Targeting System (`group_id == 96`, e.g. `1182` / `1436`).
#[allow(clippy::too_many_arguments)]
pub async fn activate_auto_targeting_system(
    ctx: &ServerContext,
    sys: &SystemHandle,
    char_id: i64,
    ship_id: i64,
    module_id: i64,
    _mod_type: &InvType,
    mod_state: &DogmaItemState,
    fit: &ShipFittingEngine,
) -> Result<PyRep, BridgeRpcError> {
    let hostile_range = mod_state.get_attribute(143, &ctx.sde).max(50_000.0);

    let ship_max = fit.ship.get_attribute(attr_ids::MAX_LOCKED_TARGETS, &ctx.sde).max(1.0) as usize;
    let skills = ctx.character_skill_levels(char_id);
    let tm_lvl = skills.get(&3429).copied().unwrap_or(0);
    let atm_lvl = skills.get(&3430).copied().unwrap_or(0);
    let max_locked = eve_dogma::compute_effective_max_locked_targets(ship_max, tm_lvl, atm_lvl);
    let max_range = fit.ship.get_attribute(attr_ids::MAX_TARGET_RANGE, &ctx.sde).min(hostile_range);
    let scan_res = fit.ship.get_attribute(attr_ids::SCAN_RESOLUTION, &ctx.sde).max(1.0);

    let all_balls = sys.list_balls().await.unwrap_or_default();
    let ship_pos = all_balls.iter().find(|b| b.id == ship_id).map(|b| b.position).unwrap_or(DVec3::ZERO);
    let current_locks = sys.get_target_locks(ship_id).await.unwrap_or_default();
    let mut locked_count = current_locks.len();

    let fleet_members: Vec<i64> = ctx.db.with_snapshot(|snap| {
        let mut mems = vec![char_id];
        for fl in snap.fleets.values() {
            if fl.members.contains(&char_id) {
                mems.extend(fl.members.iter().copied());
            }
        }
        mems
    });

    let active_aggressors = sys.active_offensive_aggressors_on_sync(ship_id, Some(&ctx.sde));
    let mut candidates: Vec<(i64, f64, f64)> = Vec::new();
    for b in &all_balls {
        if b.id == ship_id || current_locks.iter().any(|l| l.target_id == b.id) {
            continue;
        }
        let dist = (b.position - ship_pos).length();
        if dist > max_range {
            continue;
        }
        if super::mining_phased_autotarget::is_auto_target_hostile_ball(ctx, sys, char_id, ship_id, b, &fleet_members, &active_aggressors).await {
            let sig = sys
                .get_combat_state(b.id)
                .await
                .ok()
                .flatten()
                .map(|cs| cs.signature_radius)
                .filter(|&s| s > 0.0)
                .or_else(|| {
                    ctx.sde
                        .types
                        .get(&b.type_id)
                        .and_then(|t| t.default_attributes.get(&552).copied())
                        .filter(|&s| s > 0.0)
                })
                .unwrap_or(b.radius);
            candidates.push((b.id, dist, sig));
        }
    }
    candidates.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));

    let mut auto_locked = Vec::new();
    for (tid, _dist, sig) in candidates {
        if locked_count >= max_locked { break; }
        let lock_time_ms = eve_destiny::combat::compute_lock_time_seconds(scan_res, sig) * 1000.0;
        if sys.add_target_lock(ship_id, TargetLock {
            target_id: tid,
            lock_time_ms,
            remaining_lock_ms: lock_time_ms,
            locked: false,
            max_range_m: Some(max_range),
        }).await.is_ok() {
            locked_count += 1;
            auto_locked.push((tid, lock_time_ms));
        }
    }
    ctx.db.with_mut_snapshot(|snap| { snap.module_active_states.insert(module_id); });
    Ok(PyRep::keyval([
        ("moduleID", PyRep::Integer(module_id)),
        ("active", PyRep::Bool(true)),
        ("maxLockedTargets", PyRep::Integer(max_locked as i64)),
        ("lockingCount", PyRep::Integer(auto_locked.len() as i64)),
        ("autoLockingTargetIDs", PyRep::List(auto_locked.iter().map(|&(id, _)| PyRep::Integer(id)).collect())),
        ("lockTimesMs", PyRep::List(auto_locked.iter().map(|&(_, ms)| PyRep::Real(ms)).collect())),
    ]))
}

/// Validates wormhole jump eligibility for `ship_id` (blocking Bastion and other immobilizing modules).
pub async fn prepare_ship_for_wormhole_jump(
    ctx: &ServerContext,
    _old_handle: &SystemHandle,
    _char_id: i64,
    ship_id: i64,
    _current_ball: &mut Ball,
) -> Result<bool, BridgeRpcError> {
    check_ship_navigation_lock(ctx, ship_id, "jump wormhole")?;

    let blocking_mod = ctx.db.with_snapshot(|snap| {
        let mut blocking = None;
        for item in snap.items.values() {
            if item.location_id == ship_id && snap.module_active_states.contains(&item.item_id) {
                if matches!(item.type_id, 33400 | 20280 | 4292 | 27951 | 4294 | 28583 | 42890 | 58945 | 58950 | 62590 | 62591 | 34593 | 34595) {
                    blocking = Some(item.type_id);
                    break;
                }
            }
        }
        blocking
    });

    if let Some(block_tid) = blocking_mod {
        return Err(BridgeRpcError { code: "CannotJumpWormholeWhileImmobilized".into(), message: format!("Cannot jump wormhole while immobilizing module (type {block_tid}) is active") });
    }
    Ok(false)
}

/// Handles Titan Doomsday and Lancer Dreadnought Disruptive Lance activation (`feat:modules.disruptive-lance`, `feat:modules.doomsday`).
pub async fn activate_superweapon_or_lance(
    ctx: &ServerContext, char_id: i64, ship_id: i64, sys_id: u32, module_id: i64, action_method: &str, target_id_opt: Option<i64>,
) -> Result<PyRep, BridgeRpcError> {
    activate_superweapon_with_args(ctx, char_id, ship_id, sys_id, module_id, action_method, target_id_opt, &[]).await
}

fn parse_vec3_arg(arg: Option<&PyRep>) -> Option<DVec3> {
    let items = match arg? { PyRep::Tuple(v) | PyRep::List(v) if v.len() >= 3 => v, _ => return None };
    Some(DVec3::new(
        items[0].as_f64().filter(|v| v.is_finite())?,
        items[1].as_f64().filter(|v| v.is_finite())?,
        items[2].as_f64().filter(|v| v.is_finite())?,
    ))
}

#[allow(clippy::too_many_arguments)]
pub async fn activate_superweapon_with_args(
    ctx: &ServerContext, char_id: i64, ship_id: i64, sys_id: u32, module_id: i64, action_method: &str, target_id_opt: Option<i64>, args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    if ctx.is_module_in_reactivation_delay(ship_id, module_id) {
        return Err(BridgeRpcError { code: "ModuleInReactivationDelay".into(), message: format!("Module {module_id} is in reactivation delay") });
    }
    let (ship_type_id, ship_group_id, mod_type_id, has_active_siege) = ctx.db.with_snapshot(|snap| {
        let st = snap.items.get(&ship_id).map(|s| s.type_id).unwrap_or(0);
        let sg = ctx.sde.types.get(&st).map(|t| t.group_id).unwrap_or(0);
        let mt = snap.items.get(&module_id).filter(|m| m.location_id == ship_id).map(|m| m.type_id).unwrap_or(0);
        let sieged = snap.items.values().any(|i| i.location_id == ship_id && snap.module_active_states.contains(&i.item_id) && matches!(i.type_id, 20280 | 4292));
        (st, sg, mt, sieged)
    });

    let is_lancer = ship_group_id == 4594 || matches!(mod_type_id, 77398..=77401);
    if ship_group_id != 30 && !is_lancer {
        return Err(BridgeRpcError { code: "InvalidShipGroup".into(), message: "Superweapons can only be activated on Titans (group 30) or Lancer Dreadnoughts (group 4594)".into() });
    }
    let sys_sec = ctx.sde.solar_systems.get(&sys_id).map(|s| s.security_status).unwrap_or(0.0);
    if sys_sec >= 0.45 {
        return Err(BridgeRpcError { code: "CannotActivateDoomsdayInHighSec".into(), message: format!("Superweapons and Disruptive Lances cannot be activated in high-sec (system {sys_id} security {sys_sec:.2})") });
    }
    if !is_lancer && sys_sec > 0.0 {
        return Err(BridgeRpcError { code: "CannotActivateLanceInLowSec".into(), message: format!("Titan superweapons and AoE Lances can only be activated in null-sec or wormhole space (system {sys_id} security {sys_sec:.2})") });
    }
    let Some(mod_type) = ctx.sde.types.get(&mod_type_id) else {
        return Err(BridgeRpcError { code: "ModuleNotFound".into(), message: format!("Superweapon module {module_id} not found on ship") });
    };
    if mod_type.group_id != 588 {
        return Err(BridgeRpcError { code: "InvalidModuleGroup".into(), message: format!("Module {module_id} is not a Super Weapon (group 588)") });
    }

    let sys = ctx.systems.get_or_spawn(sys_id);
    let now_unix = ctx.now_unix_secs();

    if is_lancer {
        if !has_active_siege {
            return Err(BridgeRpcError { code: "SiegeRequiredForLance".into(), message: "Disruptive Lance can only be activated while a Siege Module is active".into() });
        }
        let skills = ctx.character_skill_levels(char_id);
        let op_lvl = skills.get(&77739).copied().unwrap_or(0);
        let lancer_lvl = skills.get(&77738).copied().or_else(|| skills.get(&76079).copied()).unwrap_or(0);
        if op_lvl == 0 || lancer_lvl == 0 {
            return Err(BridgeRpcError { code: "SkillMissing".into(), message: "Activating a Disruptive Lance requires Disruptive Lance Operation (77739) and Lancer Dreadnoughts (77738)".into() });
        }

        let ship_type_opt = ctx.sde.types.get(&ship_type_id);
        let base_cap = mod_type.default_attributes.get(&6).copied().unwrap_or(30_000.0);
        let cap_bonus_pct = ctx.sde.types.get(&77_739).and_then(|t| t.default_attributes.get(&5419).copied()).unwrap_or(-5.0).abs() / 100.0;
        let cap_cost = base_cap * (1.0 - cap_bonus_pct * (op_lvl.min(5) as f64));
        let cur_cap_opt = if let Ok(Some(cs)) = sys.get_combat_state(ship_id).await {
            if cs.capacitor + 1e-4 < cap_cost {
                return Err(BridgeRpcError { code: "NotEnoughCapacitor".into(), message: format!("Insufficient capacitor for Disruptive Lance: need {cap_cost:.1} GJ") });
            }
            Some(cs.capacitor)
        } else {
            None
        };

        // Pre-validate and consume SDE-defined isotopes (attrs 713/714) BEFORE applying capacitor drain, beam damage, or debuffs (#203)
        let lance_fuel_tid = mod_type.default_attributes.get(&713).map(|&v| v as u32).unwrap_or(16274);
        let lance_fuel_qty = mod_type.default_attributes.get(&714).map(|&v| v.round() as i32).unwrap_or(20_000);
        let consumed_fuel = ctx.db.with_mut_snapshot(|snap| {
            let is_fuel = |i: &&eve_db::InventoryItemRecord| i.location_id == ship_id && i.type_id == lance_fuel_tid && matches!(i.flag as u16, 5 | 133) && i.quantity > 0;
            let cand = snap.items.values().find(|i| is_fuel(i) && (i.flag as u16) == 133 && i.quantity >= lance_fuel_qty)
                .or_else(|| snap.items.values().find(|i| is_fuel(i) && i.quantity >= lance_fuel_qty))
                .or_else(|| snap.items.values().find(|i| is_fuel(i)))
                .map(|i| (i.item_id, i.quantity));
            match cand {
                Some((fid, avail)) if avail >= lance_fuel_qty => {
                    let mut rem = 0;
                    if let Some(it) = snap.items.get_mut(&fid) { it.quantity -= lance_fuel_qty; rem = it.quantity; }
                    if rem <= 0 { snap.items.remove(&fid); }
                    Ok(lance_fuel_qty)
                }
                Some((_, avail)) => Err(format!("Insufficient lance fuel (type {lance_fuel_tid}): need {lance_fuel_qty}, have {avail}")),
                None => Err(format!("Missing required lance isotopes (type {lance_fuel_tid}, quantity {lance_fuel_qty})")),
            }
        }).map_err(|message| BridgeRpcError { code: "InsufficientFuel".into(), message })?;

        if let Some(cur_cap) = cur_cap_opt {
            let _ = sys.set_capacitor(ship_id, (cur_cap - cap_cost).max(0.0)).await;
        }

        let base_range = mod_type.default_attributes.get(&54).copied().unwrap_or(50_000.0);
        let range_bonus_pct = ship_type_opt.and_then(|t| t.default_attributes.get(&5418).copied()).unwrap_or(20.0);
        let dmg_bonus_pct = ship_type_opt.and_then(|t| t.default_attributes.get(&5417).copied()).unwrap_or(10.0);
        let effective_range = base_range * (1.0 + (range_bonus_pct / 100.0) * (lancer_lvl.min(5) as f64));
        let dmg_mult = 1.0 + (dmg_bonus_pct / 100.0) * (lancer_lvl.min(5) as f64);
        let em_base = mod_type.default_attributes.get(&114).copied().unwrap_or(0.0);
        let ex_base = mod_type.default_attributes.get(&116).copied().unwrap_or(0.0);
        let ki_base = mod_type.default_attributes.get(&117).copied().unwrap_or(0.0);
        let th_base = mod_type.default_attributes.get(&118).copied().unwrap_or(0.0);
        let dmg_dur_ms = mod_type.default_attributes.get(&2264).copied().unwrap_or(15_000.0);
        let dmg_cycle_ms = mod_type.default_attributes.get(&2265).copied().unwrap_or(1_000.0).max(100.0);
        let num_ticks = (dmg_dur_ms / dmg_cycle_ms).round().max(1.0);
        let per_tick_dmg = DamageVector::new(em_base * dmg_mult, th_base * dmg_mult, ki_base * dmg_mult, ex_base * dmg_mult);
        let total_dmg_vec = DamageVector::new(
            per_tick_dmg.em * num_ticks,
            per_tick_dmg.thermal * num_ticks,
            per_tick_dmg.kinetic * num_ticks,
            per_tick_dmg.explosive * num_ticks,
        );
        let lance_damage = total_dmg_vec.total();
        let neut_amount_gj = mod_type.default_attributes.get(&2260).copied().unwrap_or(35_000.0);
        let victim_debuff_secs = (mod_type.default_attributes.get(&5412).copied().unwrap_or(60_000.0) / 1000.0).round() as u64;

        let all_balls = sys.list_balls().await.unwrap_or_default();
        let ship_pos = all_balls.iter().find(|b| b.id == ship_id).map(|b| b.position).unwrap_or(DVec3::ZERO);
        let target_pos = target_id_opt.and_then(|tid| all_balls.iter().find(|b| b.id == tid).map(|b| b.position)).unwrap_or(ship_pos + DVec3::new(1.0, 0.0, 0.0));
        let beam_delta = target_pos - ship_pos;
        let beam_dir = if beam_delta.length_squared() > 1e-4 { beam_delta.normalize() } else { DVec3::new(1.0, 0.0, 0.0) };
        let beam_radius = mod_type.default_attributes.get(&2263).copied().unwrap_or(2_500.0);
        let neut_radius = mod_type.default_attributes.get(&2259).copied().unwrap_or(10_000.0).max(beam_radius);
        let caster_lock_secs = (mod_type.default_attributes.get(&73).copied().unwrap_or(300_000.0) / 1_000.0).round() as u64;
        let mut hit_targets = Vec::new();
        for b in all_balls {
            if b.id != ship_id && (is_combatant_ship_or_entity_ball(Some(&ctx.sde), Some(&ctx.db), b.id, b.type_id) || (b.id >= 1_895_000_000 && b.id < 1_900_000_000)) {
                let to_ball = b.position - ship_pos;
                let along_beam = to_ball.dot(beam_dir);
                let perp_dist = (to_ball - beam_dir * along_beam).length();
                if along_beam >= 100.0 && along_beam <= effective_range && perp_dist <= neut_radius {
                    if perp_dist <= beam_radius {
                        let (applied, destroyed) = crate::services::station_structure::structure_lifecycle_rpc::apply_damage_structure_or_ball(ctx, &sys, sys_id, char_id, b.id, total_dmg_vec, 10.0, now_unix).await.ok().flatten().map(|(a, d, _)| (a, d)).unwrap_or((0.0, false));
                        crate::services::combat_crimewatch::record_killmail_assist_action(ctx, b.id, char_id, ship_type_id, mod_type_id, applied, false, now_unix);
                        if destroyed {
                            crate::services::combat_crimewatch::combat_missiles_aoe::handle_destroyed_ball_from_missile_or_aoe(ctx, sys_id, char_id, b.id, b.type_id, b.position, now_unix).await;
                        }
                    }
                    if let Ok(Some(mut v_cs)) = sys.get_combat_state(b.id).await {
                        v_cs.capacitor = (v_cs.capacitor - neut_amount_gj).max(0.0);
                        v_cs.warp_scramble_strength = v_cs.warp_scramble_strength.max(100);
                        v_cs.is_mwd_mjd_disabled = true;
                        v_cs.is_tethered = false;
                        let _ = sys.set_combat_state(b.id, v_cs).await;
                    }
                    hit_targets.push(b.id);
                }
            }
        }
        let db_key = db_key_of(ctx);
        {
            let mut st = ctx.tactical_state.write().unwrap_or_else(|e| e.into_inner());
            st.lance_caster_lock_until.insert((db_key, ship_id), now_unix + caster_lock_secs);
            for &vid in &hit_targets {
                st.lance_victim_debuff_until.insert((db_key, vid), now_unix + victim_debuff_secs);
            }
        }
        ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner()).get_or_create(char_id).weapon_timer_until_unix = now_unix + 60;
        ctx.record_module_reactivation_delay(ship_id, module_id, caster_lock_secs as f64);

        return Ok(PyRep::keyval([
            ("moduleID", PyRep::Integer(module_id)), ("shipID", PyRep::Integer(ship_id)), ("slashMode", PyRep::Bool(true)),
            ("totalDamageApplied", PyRep::Real(lance_damage)), ("perTickDamage", PyRep::Real(per_tick_dmg.total())),
            ("capacitorCost", PyRep::Real(cap_cost)), ("effectiveRangeMeters", PyRep::Real(effective_range)),
            ("neutralizerAmountGJ", PyRep::Real(neut_amount_gj)), ("remoteRepairMultiplier", PyRep::Real(0.50)),
            ("remoteRepairImpedance", PyRep::Real(0.50)), ("jumpDockTetherCloakLockSeconds", PyRep::Real(victim_debuff_secs as f64)),
            ("casterLockSeconds", PyRep::Integer(300)), ("victimPointDurationSeconds", PyRep::Real(victim_debuff_secs as f64)),
            ("victimDebuffSeconds", PyRep::Integer(victim_debuff_secs as i64)), ("hitCount", PyRep::Integer(hit_targets.len() as i64)),
            ("hitTargetIDs", PyRep::List(hit_targets.into_iter().map(PyRep::Integer).collect())),
            ("fuelTypeID", PyRep::Integer(lance_fuel_tid as i64)),
            ("fuelConsumed", PyRep::Integer(consumed_fuel as i64)),
        ]));
    }

    let all_balls = sys.list_balls().await.unwrap_or_default();
    let ship_ball = all_balls.iter().find(|b| b.id == ship_id).cloned();
    let ship_pos = ship_ball.as_ref().map(|b| b.position).unwrap_or(DVec3::ZERO);
    let is_slash = action_method == "ActivateSlashModule";

    let single_target_ball = if !is_slash {
        let Some(tid) = target_id_opt else {
            return Err(BridgeRpcError { code: "TargetRequired".into(), message: "Single-point Doomsday requires a targetID".into() });
        };
        if tid == ship_id {
            return Err(BridgeRpcError { code: "InvalidTarget".into(), message: "Cannot fire Doomsday at own ship".into() });
        }
        let Some(tb) = all_balls.iter().find(|b| b.id == tid).cloned() else {
            return Err(BridgeRpcError { code: "TargetNotInSpace".into(), message: format!("Target {tid} is not in space") });
        };
        let locks = sys.get_target_locks(ship_id).await.unwrap_or_default();
        if !locks.is_empty() && !locks.iter().any(|l| l.target_id == tid && l.locked) {
            return Err(BridgeRpcError { code: "TargetNotLocked".into(), message: format!("Target {tid} is not locked") });
        }
        let max_dd_range = mod_type.default_attributes.get(&54).copied().unwrap_or(250_000.0).max(100_000.0);
        if (tb.position - ship_pos).length() > max_dd_range {
            return Err(BridgeRpcError { code: "TargetOutOfRange".into(), message: format!("Target {tid} exceeds Doomsday range ({max_dd_range:.0}m)") });
        }
        let vic_char_opt = ctx.db.with_snapshot(|snap| {
            snap.characters.values().find(|c| c.active_ship_id == tid).map(|c| c.character_id)
                .or_else(|| snap.items.get(&tid).map(|i| i.owner_id).filter(|oid| snap.characters.contains_key(oid)))
        });
        if let Some(vcid) = vic_char_opt {
            if crate::services::space_destiny::is_location_change_invulnerable(ctx, vcid, now_unix) {
                return Err(BridgeRpcError { code: "TargetInvulnerable".into(), message: format!("Target character {vcid} is invulnerable") });
            }
        }
        Some(tb)
    } else {
        None
    };

    if is_slash {
        for raw_arg in [args.get(1), args.get(2)].into_iter().flatten() {
            if matches!(raw_arg, PyRep::Tuple(_) | PyRep::List(_)) && parse_vec3_arg(Some(raw_arg)).is_none() {
                return Err(BridgeRpcError { code: "InvalidDirectionVector".into(), message: "Lance direction vector coordinates must be finite".into() });
            }
        }
    }

    let fuel_tid = mod_type.default_attributes.get(&713).map(|&v| v as u32).unwrap_or(16274);
    let fuel_qty = mod_type.default_attributes.get(&714).map(|&v| v.round() as i32).unwrap_or(50_000);
    let consumed_fuel = ctx.db.with_mut_snapshot(|snap| {
        let is_fuel = |i: &&eve_db::InventoryItemRecord| i.location_id == ship_id && i.type_id == fuel_tid && matches!(i.flag as u16, 5 | 133) && i.quantity > 0;
        let cand = snap.items.values().find(|i| is_fuel(i) && (i.flag as u16) == 133 && i.quantity >= fuel_qty)
            .or_else(|| snap.items.values().find(|i| is_fuel(i) && i.quantity >= fuel_qty))
            .or_else(|| snap.items.values().find(|i| is_fuel(i)))
            .map(|i| (i.item_id, i.quantity));
        match cand {
            Some((fid, avail)) if avail >= fuel_qty => {
                let mut rem = 0;
                if let Some(it) = snap.items.get_mut(&fid) { it.quantity -= fuel_qty; rem = it.quantity; }
                if rem == 0 { snap.items.remove(&fid); }
                Ok(fuel_qty)
            }
            Some((_, avail)) => Err(format!("Insufficient superweapon fuel (type {fuel_tid}): need {fuel_qty}, have {avail}")),
            None => Err(format!("Missing required superweapon fuel (type {fuel_tid}, quantity {fuel_qty})")),
        }
    }).map_err(|message| BridgeRpcError { code: "InsufficientFuel".into(), message })?;

    let em_dmg = mod_type.default_attributes.get(&114).copied().unwrap_or(0.0);
    let ex_dmg = mod_type.default_attributes.get(&116).copied().unwrap_or(0.0);
    let ki_dmg = mod_type.default_attributes.get(&117).copied().unwrap_or(0.0);
    let th_dmg = mod_type.default_attributes.get(&118).copied().unwrap_or(0.0);
    let dmg_vec = DamageVector::new(em_dmg, th_dmg, ki_dmg, ex_dmg);
    let total_raw_damage = dmg_vec.em + dmg_vec.thermal + dmg_vec.kinetic + dmg_vec.explosive;

    let mut hit_targets = Vec::new();
    let default_fwd = ship_ball.as_ref().map(|b| if b.velocity.length_squared() > 1e-4 { b.velocity.normalize() } else { DVec3::X }).unwrap_or(DVec3::X);
    let beam_len = mod_type.default_attributes.get(&54).copied().unwrap_or(200_000.0);
    let beam_radius = mod_type.default_attributes.get(&2263).copied().unwrap_or(2_500.0);
    let warmup_s = mod_type.default_attributes.get(&2262).copied().unwrap_or(15_000.0) / 1000.0;
    let dmg_dur_ms = mod_type.default_attributes.get(&2264).copied().unwrap_or(15_000.0);
    let dmg_cycle_ms = mod_type.default_attributes.get(&2265).copied().unwrap_or(1_000.0).max(100.0);
    let num_ticks = (dmg_dur_ms / dmg_cycle_ms).round().max(1.0) as i32;
    let lock_secs = (mod_type.default_attributes.get(&2427).copied().unwrap_or(300_000.0) / 1000.0).round() as u64;

    if is_slash {
        let raw_dir = match (parse_vec3_arg(args.get(1)), parse_vec3_arg(args.get(2))) {
            (Some(p0), Some(p1)) if (p1 - p0).length_squared() > 1e-4 => Some(p1 - p0),
            (Some(p0), Some(p1)) => Some(if p0.length_squared() > 1e-4 { p0 } else { p1 }),
            (a, b) => a.or(b),
        };
        let beam_dir = raw_dir.map(|v| if v.length_squared() > 1e-4 { v.normalize() } else { default_fwd }).unwrap_or(default_fwd);
        if let (Some(sb), Ok(balls)) = (&ship_ball, sys.list_balls().await) {
            for b in balls {
                if b.id != ship_id && is_combatant_ship_or_entity_ball(Some(&ctx.sde), Some(&ctx.db), b.id, b.type_id) {
                    let rel = b.position - sb.position;
                    let al = rel.dot(beam_dir);
                    if (0.0..=beam_len).contains(&al) && (rel - beam_dir * al).length() <= beam_radius { hit_targets.push(b.id); }
                }
            }
        }
        ctx.tactical_state.write().unwrap_or_else(|e| e.into_inner()).lance_caster_lock_until.insert((db_key_of(ctx), ship_id), now_unix + lock_secs);
        let _ = sys.add_module_cycle(crate::system_actor::ActiveModuleCycle {
            ship_id, module_id, module_type_id: mod_type_id, owner_char_id: Some(char_id),
            effect_name: "superWeaponLanceWarmup".into(), repeat: num_ticks - 1, duration_s: warmup_s,
            immobilize_self: true, damage_vector: dmg_vec, lance_beam_dir: Some(beam_dir), ..Default::default()
        }).await;
    } else if let Some(tb) = single_target_ball {
        let (app, destroyed) = crate::services::station_structure::structure_lifecycle_rpc::apply_damage_structure_or_ball(ctx, &sys, sys_id, char_id, tb.id, dmg_vec, 10.0, now_unix).await?.map(|(a, d, _)| (a, d)).unwrap_or((0.0, false));
        crate::services::combat_crimewatch::record_killmail_assist_action(ctx, tb.id, char_id, ship_type_id, mod_type_id, app, false, now_unix);
        if destroyed {
            crate::services::combat_crimewatch::combat_missiles_aoe::handle_destroyed_ball_from_missile_or_aoe(ctx, sys_id, char_id, tb.id, tb.type_id, tb.position, now_unix).await;
        }
        hit_targets.push(tb.id);
    }

    ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner()).get_or_create(char_id).weapon_timer_until_unix = now_unix + 60;
    ctx.record_module_reactivation_delay(ship_id, module_id, lock_secs as f64);

    ctx.notify(NotificationTarget::Character(char_id), "OnGodmaShipEffect", "charid", vec![PyRep::Integer(module_id), PyRep::String("superWeapon".into()), PyRep::Bool(true)]);
    if !is_slash {
        let tgt_id = hit_targets.first().copied().unwrap_or(0);
        let dmg_dict = PyRep::Dict(vec![
            (PyRep::String("damage".into()), PyRep::Real(total_raw_damage)), (PyRep::String("source".into()), PyRep::Integer(ship_id)),
            (PyRep::String("attackerID".into()), PyRep::Integer(ship_id)), (PyRep::String("target".into()), PyRep::Integer(tgt_id)),
            (PyRep::String("target_ID".into()), PyRep::Integer(tgt_id)), (PyRep::String("weapon".into()), PyRep::Integer(mod_type_id as i64)),
            (PyRep::String("hitQuality".into()), PyRep::Integer(5)), (PyRep::String("attackType".into()), PyRep::String("me".into())),
        ]);
        ctx.notify(NotificationTarget::Character(char_id), "OnDamageMessage", "charid", vec![dmg_dict.clone()]);
        ctx.notify(NotificationTarget::Character(char_id), "OnDamageMessages", "charid", vec![PyRep::List(vec![PyRep::Tuple(vec![dmg_dict])])]);
    }

    Ok(PyRep::keyval([
        ("moduleID", PyRep::Integer(module_id)), ("shipID", PyRep::Integer(ship_id)), ("slashMode", PyRep::Bool(is_slash)),
        ("totalDamageApplied", PyRep::Real(if is_slash { 0.0 } else { total_raw_damage })),
        ("fuelConsumed", PyRep::Integer(consumed_fuel as i64)),
        ("warmupSeconds", PyRep::Real(if is_slash { warmup_s } else { 0.0 })),
        ("tickCount", PyRep::Integer(if is_slash { num_ticks as i64 } else { 1 })),
        ("immobilizeSeconds", PyRep::Real(if is_slash { lock_secs as f64 } else { 30.0 })), ("jumpBlockSeconds", PyRep::Real(lock_secs as f64)),
        ("jumpWarpSuppressionSeconds", PyRep::Real(0.0)),
        ("hitTargetIDs", PyRep::List(hit_targets.into_iter().map(PyRep::Integer).collect())),
    ]))
}

pub(crate) fn fire_lance_warmup_in_ballpark(
    ballpark: &mut eve_destiny::Ballpark,
    cycle: &crate::system_actor::ActiveModuleCycle,
    sde_opt: &Option<Arc<eve_sde::SdeRegistry>>,
    db_opt: &Option<Arc<eve_db::WorldRepository>>,
    tactical_state: &std::sync::RwLock<TacticalModulesState>,
    pending_cycle_actions: &mut Vec<crate::system_actor::PendingCycleAction>,
    destroyed_balls: &mut Vec<(i64, Option<i64>)>,
) {
    let _ = tactical_state;
    let Some(ship_ball) = ballpark.balls.get(&cycle.ship_id) else { return };
    let ship_pos = ship_ball.position;
    let attacker_ship_type_id = ship_ball.type_id;
    let beam_dir = cycle.lance_beam_dir.unwrap_or(DVec3::X);
    let mod_attrs = sde_opt.as_ref().and_then(|s| s.types.get(&cycle.module_type_id)).map(|t| &t.default_attributes);
    let beam_len = mod_attrs.and_then(|a| a.get(&54).copied()).unwrap_or(200_000.0);
    let beam_radius = mod_attrs.and_then(|a| a.get(&2263).copied()).unwrap_or(2_500.0);
    let dmg_sig = mod_attrs.and_then(|a| a.get(&552).copied()).unwrap_or(5_000.0).max(1.0);
    let lock_s = mod_attrs.and_then(|a| a.get(&2427).copied()).unwrap_or(300_000.0) / 1000.0;
    let candidates: Vec<(i64, u32, DVec3)> = ballpark.balls.values().filter(|b| b.id != cycle.ship_id && is_combatant_ship_or_entity_ball(sde_opt.as_ref(), db_opt.as_ref(), b.id, b.type_id)).map(|b| (b.id, b.type_id, b.position)).collect();
    let mut hits = Vec::new();
    for (bid, _tid, bpos) in candidates {
        let to_b = bpos - ship_pos;
        let along = to_b.dot(beam_dir);
        let perp = (to_b - beam_dir * along).length();
        if along > 0.0 && along <= beam_len && perp <= beam_radius {
            let target_sig = ballpark.combat_states.get(&bid).map(|cs| cs.signature_radius.max(1.0)).unwrap_or(dmg_sig);
            let sig_scale = (target_sig / dmg_sig).min(1.0);
            let scaled_dmg = cycle.damage_vector.scaled(sig_scale);
            let mut applied_dmg = 0.0;
            if let Some((app, destroyed, _)) = ballpark.apply_damage_to_ball(bid, scaled_dmg) {
                applied_dmg = app;
                if destroyed { destroyed_balls.push((bid, cycle.owner_char_id)); }
            }
            if let Some(v_cs) = ballpark.combat_states.get_mut(&bid) {
                v_cs.is_tethered = false;
            }
            hits.push((bid, applied_dmg));
        }
    }
    if let Some(c_cs) = ballpark.combat_states.get_mut(&cycle.ship_id) {
        c_cs.lance_disrupted_remaining_s = c_cs.lance_disrupted_remaining_s.max((lock_s - 15.0).max(0.0));
        c_cs.is_tethered = false;
    }
    if let Some(c_ball) = ballpark.balls.get_mut(&cycle.ship_id) {
        c_ball.velocity = DVec3::ZERO;
        c_ball.speed_fraction = 0.0;
    }
    if let Some(attacker_char_id) = cycle.owner_char_id {
        pending_cycle_actions.push(crate::system_actor::PendingCycleAction::LanceBeamFired {
            attacker_char_id,
            attacker_ship_id: cycle.ship_id,
            attacker_ship_type_id,
            module_id: cycle.module_id,
            module_type_id: cycle.module_type_id,
            hits,
        });
    }
}

/// Handles Entosis Link (`group 1313`) activation, warm-up cycle, and contest rules.
pub async fn handle_entosis_link_activation(
    ctx: &ServerContext,
    sys: &SystemHandle,
    char_id: i64,
    ship_id: i64,
    module_id: i64,
    mod_type: &InvType,
    target_id_opt: Option<i64>,
) -> Result<PyRep, BridgeRpcError> {
    let Some(target_id) = target_id_opt else {
        return Err(BridgeRpcError { code: "TargetRequired".into(), message: "Entosis Link requires a locked target sovereignty structure or node".into() });
    };

    let (ship_group_id, side_id, is_vulnerable, adm, stront_avail_id) = ctx.db.with_snapshot(|snap| {
        let sg = snap.items.get(&ship_id).and_then(|s| ctx.sde.types.get(&s.type_id)).map(|t| t.group_id).unwrap_or(0);
        let ch = snap.characters.get(&char_id);
        let side = ch.and_then(|c| c.alliance_id).or_else(|| ch.map(|c| c.corporation_id)).unwrap_or(char_id);
        let sys_id = ch.map(|c| c.solar_system_id).unwrap_or(0);
        let adm_val = snap.system_states.get(&sys_id).map(|st| crate::services::sov_services::calculate_adm(st.military_index, st.industrial_index, st.strategic_index)).unwrap_or(1.0);
        let vuln_item = snap.items.get(&target_id).and_then(|i| i.dynamic_attributes.get(&2110).copied()).unwrap_or(1.0) > 0.0;
        let vuln_struct = snap.structures.get(&target_id).map(|s| !matches!(s.state.as_str(), "ShieldReinforced" | "ArmorReinforced" | "Invulnerable")).unwrap_or(true);
        let stront_id = snap.items.values().find(|i| i.location_id == ship_id && i.type_id == 16275 && i.quantity >= 1).map(|i| i.item_id);
        (sg, side, vuln_item && vuln_struct, adm_val, stront_id)
    });

    if ship_group_id == 831 {
        return Err(BridgeRpcError { code: "InterceptorCannotActivateEntosis".into(), message: "Interceptors (group 831) cannot activate Entosis Links".into() });
    }
    if !is_vulnerable {
        return Err(BridgeRpcError { code: "TargetNotVulnerable".into(), message: format!("Target {target_id} is outside its vulnerability window") });
    }

    let db_key = db_key_of(ctx);
    let now_unix = ctx.now_unix_secs();
    if let Some(existing) = ctx.tactical_state.read().unwrap_or_else(|e| e.into_inner()).entosis_active_links.get(&(db_key, ship_id)) {
        if now_unix < existing.expires_unix {
            if existing.module_id != module_id {
                return Err(BridgeRpcError { code: "MaxGroupActiveExceeded".into(), message: "Only 1 Entosis Link can be active per ship".into() });
            }
            return Err(BridgeRpcError { code: "ModuleAlreadyActive".into(), message: "Entosis Link is already cycling".into() });
        }
    }

    let Some(stront_item_id) = stront_avail_id else {
        return Err(BridgeRpcError { code: "InsufficientFuel".into(), message: "Entosis Link requires 1 Strontium Clathrates (16275) per cycle".into() });
    };
    ctx.db.with_mut_snapshot(|snap| {
        let mut rem = 0;
        if let Some(it) = snap.items.get_mut(&stront_item_id) {
            it.quantity -= 1;
            rem = it.quantity;
        }
        if rem <= 0 { snap.items.remove(&stront_item_id); }
        snap.module_active_states.insert(module_id);
    });

    let is_capital = matches!(ship_group_id, 30 | 485 | 547 | 659 | 883 | 1538 | 4594);
    let base_cycle_s = mod_type.default_attributes.get(&73).copied().unwrap_or(300_000.0) / 1000.0;
    let cycle_duration_s = if is_capital { base_cycle_s * 5.0 } else { base_cycle_s };

    if let Ok(Some((_, mut ball))) = sys.get_ball_snapshot(ship_id).await {
        if ball.max_velocity > 4_000.0 {
            ball.max_velocity = 4_000.0;
            let _ = sys.spawn_ball(ball).await;
        }
    }
    if let Ok(Some(mut cs)) = sys.get_combat_state(ship_id).await {
        cs.warp_scramble_strength = cs.warp_scramble_strength.max(100);
        if is_capital { cs.disallow_assistance = true; }
        let _ = sys.set_combat_state(ship_id, cs).await;
    }

    let (cycle_index, is_warmup, is_paused, capture_rate, capture_time_s) = {
        let mut st = ctx.tactical_state.write().unwrap_or_else(|e| e.into_inner());
        let c_idx = st.entosis_target_cycles.entry((db_key, target_id, ship_id)).and_modify(|c| *c += 1).or_insert(1);
        let cycle_index = *c_idx;
        let is_warmup = cycle_index == 1;
        let expires_unix = now_unix + (cycle_duration_s.round() as u64);
        st.entosis_active_links.insert((db_key, ship_id), ActiveEntosisLinkState {
            ship_id, char_id, side_id, module_id, target_id, cycle_index, cycle_duration_s, started_unix: now_unix, expires_unix, is_capital,
        });
        let mut active_sides = Vec::new();
        for (&(k, _), link) in st.entosis_active_links.iter() {
            if k == db_key && link.target_id == target_id && link.cycle_index >= 2 && now_unix < link.expires_unix && !active_sides.contains(&link.side_id) {
                active_sides.push(link.side_id);
            }
        }
        let paused = active_sides.len() > 1;
        let rate = if is_warmup || paused { 0.0 } else { 1.0 };
        (cycle_index, is_warmup, paused, rate, 600.0 * adm)
    };

    scanners_superweapons_state().write().unwrap_or_else(|e| e.into_inner()).entosis_contested_nodes.insert(target_id, ship_id);
    ctx.invalidate_ship_fitting(ship_id);

    Ok(PyRep::keyval([
        ("moduleID", PyRep::Integer(module_id)), ("targetID", PyRep::Integer(target_id)), ("active", PyRep::Bool(true)),
        ("strontiumConsumed", PyRep::Integer(1)), ("cycleDurationSeconds", PyRep::Real(cycle_duration_s)),
        ("maxVelocityCap", PyRep::Real(4_000.0)), ("disallowAssistance", PyRep::Bool(is_capital)),
        ("cannotWarp", PyRep::Bool(true)), ("cannotCloak", PyRep::Bool(true)), ("cannotDockOrJump", PyRep::Bool(true)),
        ("cycleIndex", PyRep::Integer(cycle_index as i64)), ("isWarmupCycle", PyRep::Bool(is_warmup)),
        ("isWarmup", PyRep::Bool(is_warmup)), ("progressDelta", PyRep::Real(capture_rate)),
        ("structureContested", PyRep::Bool(!is_warmup)), ("isPaused", PyRep::Bool(is_paused)), ("contested", PyRep::Bool(is_paused)),
        ("remoteAssistanceImpedance", PyRep::Real(if is_capital { 0.15 } else { 1.0 })),
        ("captureRateMultiplier", PyRep::Real(capture_rate)), ("captureTimeSeconds", PyRep::Real(capture_time_s)),
    ]))
}

/// Applies Command Burst (`group_id == 1770`) fleet `dbuffs` with V24 range, strength, duration, reload, Omega clone, and Industrial Core bonuses.
#[allow(clippy::too_many_arguments)]
pub async fn activate_command_burst(
    ctx: &ServerContext, sys: &SystemHandle, char_id: i64, ship_id: i64, sys_id: u32,
    module_id: i64, mod_type: &InvType, mod_state: &DogmaItemState, charge_tid_opt: Option<TypeId>, fit: &ShipFittingEngine,
) -> Result<PyRep, BridgeRpcError> {
    let (clone_grade, ship_type_id, ship_group_id, active_burst_count, cmd_rig_bonus, active_ind_core_tid) = ctx.db.with_snapshot(|snap| {
        let cg = snap.characters.get(&char_id).map(|c| c.clone_grade).unwrap_or(1);
        let s_tid = snap.items.get(&ship_id).map(|s| s.type_id).unwrap_or(0);
        let s_grp = ctx.sde.types.get(&s_tid).map(|t| t.group_id).unwrap_or(0);
        let (mut act_bursts, mut rig_bonus, mut ind_core) = (0usize, 0usize, None);
        for item in snap.items.values().filter(|i| i.location_id == ship_id && snap.module_online_states.get(&i.item_id).copied().unwrap_or(true)) {
            if let Some(t) = ctx.sde.types.get(&item.type_id) {
                if t.effect_ids.contains(&6766) { rig_bonus += t.default_attributes.get(&2542).copied().unwrap_or(1.0).round() as usize; }
                if snap.module_active_states.contains(&item.item_id) {
                    if t.group_id == 1770 && item.item_id != module_id { act_bursts += 1; }
                    if matches!(item.type_id, 28583 | 42890 | 58945 | 58950 | 62590 | 62591) { ind_core = Some(item.type_id); }
                }
            }
        }
        (cg, s_tid, s_grp, act_bursts, rig_bonus, ind_core)
    });

    if clone_grade == 0 { return Err(BridgeRpcError { code: "OmegaRequiredForCommandBurst".into(), message: "Alpha clones cannot activate Command Burst modules".into() }); }
    super::mining_phased_autotarget::ensure_command_burst_hull_allowed(&ctx.sde, mod_type, ship_type_id)?;
    let max_active_allowed = 2 + cmd_rig_bonus;
    if active_burst_count >= max_active_allowed { return Err(BridgeRpcError { code: "MaxGroupActiveExceeded".into(), message: format!("Cannot activate more than {max_active_allowed} Command Burst module(s) without additional Command Processor rigs") }); }

    let allowed_charge_groups: Vec<u32> = [604u16, 605, 606, 609, 610]
        .into_iter().filter_map(|aid| mod_type.default_attributes.get(&aid).map(|&v| v as u32)).filter(|&g| g > 0).collect();
    let is_valid_charge_type = |tid: TypeId| {
        ctx.sde.types.get(&tid).is_some_and(|t| {
            if allowed_charge_groups.is_empty() { matches!(t.group_id, 1769 | 1771 | 1772 | 1773 | 1774 | 4905) } else { allowed_charge_groups.contains(&t.group_id) }
        })
    };
    let (charge_tid, charge_item_id_opt) = ctx.db.with_snapshot(|snap| {
        let mod_flag = snap.items.get(&module_id).map(|i| i.flag);
        let is_burst_charge = |i: &eve_db::InventoryItemRecord| {
            i.location_id == ship_id && i.item_id != module_id && i.quantity > 0
                && is_valid_charge_type(i.type_id)
                && charge_tid_opt.is_none_or(|req| i.type_id == req)
        };
        if let Some(found) = snap.items.values().find(|i| Some(i.flag) == mod_flag && is_burst_charge(i)) {
            (Some(found.type_id), Some(found.item_id))
        } else {
            (None, None)
        }
    });
    let Some(c_tid) = charge_tid else {
        let has_wrong_group_in_slot = ctx.db.with_snapshot(|snap| {
            let mod_flag = snap.items.get(&module_id).map(|i| i.flag);
            snap.items.values().any(|i| i.location_id == ship_id && i.item_id != module_id && Some(i.flag) == mod_flag && i.quantity > 0)
        });
        if has_wrong_group_in_slot {
            return Err(BridgeRpcError { code: "InvalidChargeGroup".into(), message: format!("Loaded charge is incompatible with {}", mod_type.name) });
        }
        return Err(BridgeRpcError { code: "NoChargesLoaded".into(), message: "Command Burst module requires a loaded Command Burst charge".into() });
    };
    if !is_valid_charge_type(c_tid) {
        return Err(BridgeRpcError { code: "InvalidChargeGroup".into(), message: format!("Charge {c_tid} is incompatible with {}", mod_type.name) });
    }

    let skills = &fit.character_skills;
    let is_t2_module = mod_type.default_attributes.get(&422).copied().unwrap_or(1.0) >= 2.0;
    let is_mining_burst = matches!(mod_type.type_id, 42528 | 43551) || mod_type.name.contains("Mining Foreman");

    let (command_skill_id, specialist_skill_id) = if is_mining_burst { (22536u32, 22552u32) }
        else if matches!(mod_type.type_id, 42526 | 43555) || mod_type.name.contains("Shield") { (3350, 3351) }
        else if matches!(mod_type.type_id, 42525 | 43553) || mod_type.name.contains("Armor") { (3352, 3353) }
        else if matches!(mod_type.type_id, 42529 | 43556) || mod_type.name.contains("Skirmish") { (3349, 3355) }
        else { (20494, 20495) };
    let command_lvl = skills.get(&command_skill_id).copied().unwrap_or(0).min(5);
    let specialist_lvl = skills.get(&specialist_skill_id).copied()
        .or_else(|| mod_type.default_attributes.get(&184).and_then(|&sid| skills.get(&(sid as u32)).copied()))
        .unwrap_or(0).min(5);
    if is_t2_module && specialist_lvl == 0 {
        return Err(BridgeRpcError { code: "SkillMissing".into(), message: format!("Tech II Command Burst {} requires Specialist skill {specialist_skill_id}", mod_type.name) });
    }

    let wing_cmd_lvl = skills.get(&11574).or_else(|| skills.get(&11572)).copied().unwrap_or(0).min(5);
    let fleet_cmd_lvl = skills.get(&24764).or_else(|| skills.get(&3354)).copied().unwrap_or(0).min(5);
    let ind_cmd_lvl = skills.get(&29637).copied().unwrap_or(0).min(5) as f64;
    let cap_ind_lvl = skills.get(&28374).copied().unwrap_or(0).min(5) as f64;
    let ship_range_mult = match ship_group_id {
        30 | 547 | 659 | 1538 => 3.00, 540 => 2.00,
        883 => 1.50 * if is_mining_burst { 1.0 + 0.05 * cap_ind_lvl } else { 1.0 },
        941 if ship_type_id == 28606 => 2.00 * if is_mining_burst { 1.0 + 0.05 * ind_cmd_lvl } else { 1.0 },
        941 => 1.50 * if is_mining_burst { 1.0 + 0.05 * ind_cmd_lvl } else { 1.0 },
        419 | 963 => 1.50, _ => 1.0,
    };
    let ind_core_range_bonus = match active_ind_core_tid {
        Some(62590) => 0.45, Some(62591) => 0.60, Some(58945) => 0.75, Some(58950) => 1.00, Some(28583) => 2.00, Some(42890) => 2.50, _ => 0.0,
    };
    let aoe_range = 15_000.0 * ship_range_mult * (1.0 + 0.10 * (wing_cmd_lvl as f64)) * (1.0 + 0.05 * (fleet_cmd_lvl as f64)) * (1.0 + ind_core_range_bonus);
    let cycle_duration_s = 15.0 * (1.0 + 0.10 * (command_lvl as f64));

    let mod_strength_bonus = if is_t2_module { 0.25 } else { 0.0 };
    let ship_strength_bonus = match ship_group_id {
        540 => 0.04 * (skills.get(&33095).copied().unwrap_or(0).min(5) as f64),
        1534 => 0.02 * (skills.get(&37615).copied().unwrap_or(0).min(5) as f64),
        883 if is_mining_burst => 0.05 * cap_ind_lvl,
        941 if is_mining_burst && ship_type_id == 28606 => 0.03 * ind_cmd_lvl,
        941 if is_mining_burst => 0.02 * ind_cmd_lvl,
        _ => 0.0,
    };
    let has_mindlink = fit.implants.iter().any(|&imp_tid| ctx.sde.types.get(&imp_tid).is_some_and(|t| t.name.contains("Mindlink")));
    let mindlink_strength_bonus = if has_mindlink { 0.25 } else { 0.0 };
    let ind_core_strength_bonus = if is_mining_burst {
        match active_ind_core_tid {
            Some(62590) => 0.05, Some(62591) => 0.075, Some(58945) => 0.10, Some(58950) => 0.15, Some(28583) => 0.30, Some(42890) => 0.35, _ => 0.0,
        }
    } else { 0.0 };
    let total_strength_mult = (1.0 + 0.10 * (specialist_lvl as f64)) * (1.0 + mod_strength_bonus) * (1.0 + ship_strength_bonus) * (1.0 + mindlink_strength_bonus) * (1.0 + ind_core_strength_bonus);
    let buff_duration_s = 60.0 * (1.0 + 0.10 * (command_lvl as f64)) * (1.0 + if has_mindlink { 0.25 } else { 0.0 });
    let cbs_lvl = skills.get(&3354).or_else(|| skills.get(&43728)).copied().unwrap_or(0).min(5);
    let reload_time_s = 60.0 * (1.0 - 0.10 * (cbs_lvl as f64));
    let now_unix = ctx.now_unix_secs();
    let expires_at_unix = now_unix + (buff_duration_s.round() as u64);

    let mut raw_buffs: Vec<(u32, f64)> = Vec::new();
    if let Some(c_type) = ctx.sde.types.get(&c_tid) {
        for (id_attr, mult_attr, mod_val_attr) in [(2468u16, 2596u16, 2469u16), (2470, 2597, 2471), (2472, 2598, 2473), (2536, 2599, 2537)] {
            let dbuff_id = c_type.default_attributes.get(&id_attr).copied().unwrap_or(0.0) as u32;
            if dbuff_id > 0 {
                let mod_val = mod_state.get_attribute(mod_val_attr, &ctx.sde);
                let mult = c_type.default_attributes.get(&mult_attr).copied().unwrap_or(0.0);
                let val = if mod_val < 0.0 { mod_val } else { mult * total_strength_mult };
                raw_buffs.push((dbuff_id, val));
            }
        }
    }
    let primary_buff_strength = raw_buffs.first().map(|&(_, v)| v).unwrap_or(0.0);
    let candidate_ship_ids: Vec<(i64, i64)> = ctx.db.with_snapshot(|snap| {
        let mut fleet_chars = vec![char_id];
        for fl in snap.fleets.values().filter(|fl| fl.members.contains(&char_id)) {
            for &m_id in &fl.members {
                if !fleet_chars.contains(&m_id) && (fl.member_details.is_empty() || super::fleet_social::is_in_fleet_boost_scope(fl, char_id, m_id)) {
                    fleet_chars.push(m_id);
                }
            }
        }
        let mut ships: Vec<(i64, i64)> = fleet_chars.into_iter().filter_map(|cid| {
            let ch = snap.characters.get(&cid)?;
            (ch.solar_system_id == sys_id && ch.station_id.is_none() && ch.structure_id.is_none() && ch.active_ship_id > 0).then_some((cid, ch.active_ship_id))
        }).collect();
        if !ships.iter().any(|&(_, sid)| sid == ship_id) { ships.push((char_id, ship_id)); }
        ships
    });
    let sys_sec = ctx.sde.solar_systems.get(&sys_id).map(|s| s.security_status).unwrap_or(0.0);
    let now_unix_burst = ctx.now_unix();
    let safety = crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
        rt.safety_levels.get(&char_id).copied().unwrap_or(crate::services::combat_crimewatch::SafetyLevel::Green)
    });
    let mut trigger_suspect = false;
    let mut criminal_beneficiary: Option<i64> = None;
    let booster_pos = sys.get_ball_snapshot(ship_id).await.ok().flatten().map(|(_, b)| b.position).unwrap_or(DVec3::ZERO);
    let mut affected_ships = Vec::new();
    for (member_char_id, member_ship_id) in candidate_ship_ids {
        if affected_ships.len() >= 256 { break; }
        let in_range = if member_ship_id == ship_id { true }
            else if let Ok(Some((_, m_ball))) = sys.get_ball_snapshot(member_ship_id).await { (m_ball.position - booster_pos).length() <= aoe_range }
            else { false };
        if !in_range { continue; }
        if sys_sec > 0.0 && member_char_id != char_id {
            let (m_crim, m_susp) = {
                let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
                let st = cw.get_or_create(member_char_id);
                (st.is_criminal(now_unix_burst), st.is_suspect(now_unix_burst))
            };
            let has_unshared_le = crate::sp6_lifecycle::has_active_pvp_timer(ctx, member_char_id, now_unix_burst)
                && crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
                    let hle = rt.limited_engagements.get(&char_id);
                    rt.limited_engagements.get(&member_char_id).is_some_and(|m| {
                        m.iter().any(|(&pid, &exp)| pid != char_id && now_unix_burst <= exp && !hle.is_some_and(|h| h.get(&pid).is_some_and(|&he| now_unix_burst <= he)))
                    })
                });
            let (same_corp_or_alliance, is_outlaw) = ctx.db.with_snapshot(|snap| {
                let b = snap.characters.get(&char_id);
                let m = snap.characters.get(&member_char_id);
                let same = b.zip(m).is_some_and(|(b, m)| b.corporation_id == m.corporation_id || (b.alliance_id.is_some() && b.alliance_id == m.alliance_id));
                (same, m.is_some_and(|rec| rec.security_status <= -4.95))
            });
            let war_interference = !same_corp_or_alliance && crate::services::combat_crimewatch::crimewatch_timers::is_war_interference_assistance(ctx, char_id, member_char_id, now_unix_burst);
            if m_crim {
                if safety != crate::services::combat_crimewatch::SafetyLevel::Red { continue; }
                criminal_beneficiary.get_or_insert(member_char_id);
            } else if sys_sec >= 0.45 && (has_unshared_le || war_interference) {
                continue; // High-sec Command Bursts exclude unshared LE/war-engaged fleetmates without blocking
            } else if m_susp || (sys_sec >= 0.45 && is_outlaw) || (sys_sec < 0.45 && (has_unshared_le || war_interference)) {
                if safety == crate::services::combat_crimewatch::SafetyLevel::Green { continue; }
                trigger_suspect = true;
            }
        }
        affected_ships.push((member_char_id, member_ship_id));
    }
    let concord_suppressed = crate::services::combat_npc_police::is_concord_suppressed_in_system(ctx, sys_id);
    {
        let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
        cw.trigger_weapon_timer(char_id, now_unix_burst);
        if let Some(beneficiary_id) = criminal_beneficiary {
            let eff_sec = if concord_suppressed { 0.0 } else { f64::from(sys_sec) };
            let _ = cw.handle_criminal_remote_assistance(char_id, beneficiary_id, ship_id, sys_id, eff_sec, now_unix_burst);
        } else if trigger_suspect {
            cw.trigger_suspect_flag(char_id, now_unix_burst);
            let st = cw.get_or_create(char_id);
            st.pvp_timer_until_unix = st.pvp_timer_until_unix.max(now_unix_burst.saturating_add(900));
        }
    }
    crate::services::combat_crimewatch::crimewatch_timers::emit_crimewatch_timer_notifications(ctx, char_id, now_unix_burst);
    {
        let mut st = scanners_superweapons_state().write().unwrap_or_else(|e| e.into_inner());
        for &(_, target_sid) in &affected_ships {
            let list = st.ship_dbuffs.entry((ctx.instance_id, target_sid)).or_default();
            for &(dbuff_id, val) in &raw_buffs {
                list.retain(|e| !(e.dbuff_id == dbuff_id && e.source_module_id == module_id));
                list.push(ActiveDbuffEntry { dbuff_id, value: val, expires_at_unix, source_module_id: module_id });
            }
        }
    }
    let dbuff_id_reps: Vec<PyRep> = raw_buffs.iter().map(|&(id, _)| PyRep::Integer(id as i64)).collect();
    let affected_ship_reps: Vec<PyRep> = affected_ships.iter().map(|&(_, sid)| PyRep::Integer(sid)).collect();
    let affected_member_reps: Vec<PyRep> = affected_ships.iter().map(|&(cid, _)| PyRep::Integer(cid)).collect();
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.insert(module_id);
        let exhausted = charge_item_id_opt.is_some_and(|cid| {
            let zero = snap.items.get_mut(&cid).is_some_and(|ci| { ci.quantity = ci.quantity.saturating_sub(1); ci.quantity == 0 });
            if zero { snap.items.remove(&cid); }
            zero
        });
        if exhausted { snap.module_active_charges.remove(&module_id); } else { snap.module_active_charges.insert(module_id, c_tid); }
    });
    for &(member_cid, member_sid) in &affected_ships {
        ctx.invalidate_ship_fitting(member_sid);
        ctx.notify(NotificationTarget::Character(member_cid), "OnDbuffUpdated", "charid", vec![PyRep::Integer(member_sid), PyRep::List(dbuff_id_reps.clone())]);
    }
    crate::services::notify_emitters_space_dungeons::emit_fleet_boosted(ctx, char_id, mod_type.type_id, affected_ships.len());

    Ok(PyRep::keyval([
        ("moduleID", PyRep::Integer(module_id)), ("active", PyRep::Bool(true)), ("chargeTypeID", PyRep::Integer(c_tid as i64)),
        ("dbuffIDs", PyRep::List(dbuff_id_reps)), ("affectedShipIDs", PyRep::List(affected_ship_reps)), ("affectedMemberIDs", PyRep::List(affected_member_reps)),
        ("buffStrength", PyRep::Real(primary_buff_strength)), ("buffStrengthMultiplier", PyRep::Real(total_strength_mult)), ("burstStrengthMultiplier", PyRep::Real(total_strength_mult)),
        ("buffDurationSeconds", PyRep::Real(buff_duration_s)), ("durationSeconds", PyRep::Real(cycle_duration_s)), ("cycleDurationSeconds", PyRep::Real(cycle_duration_s)),
        ("reloadTimeSeconds", PyRep::Real(reload_time_s)), ("expiresAtUnix", PyRep::Integer(expires_at_unix as i64)),
        ("aoeRangeMeters", PyRep::Real(aoe_range)), ("aoeRange", PyRep::Real(aoe_range)),
    ]))
}

/// Overlays cycle-transient item attributes (RAH/ADC/EHE resonances, mutadaptive spool, rack heat) onto `ItemGetInfo` attributes.
pub fn apply_transient_item_attrs(ctx: &ServerContext, item_id: i64, merged_attrs: &mut HashMap<u16, f64>) {
    ctx.db.with_snapshot(|snap| if let Some(tr_attrs) = snap.transient_item_attributes.get(&item_id) { merged_attrs.extend(tr_attrs.iter().map(|(&k, &v)| (k, v))); });
}
