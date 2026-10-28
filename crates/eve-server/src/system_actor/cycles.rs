//! Module cycle simulation, thermodynamics, RAH adaptation, and tackle reconciliation for `system_actor`.

#![forbid(unsafe_code)]

use crate::system_actor::*;
use eve_destiny::{BallId, Ballpark, MovementMode, TargetLock};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

pub(crate) fn step_rah_resonances(current_resonances: [f64; 4], dmg: [f64; 4], shift_pct: f64) -> [f64; 4] {
    // current_resonances is [em (267), exp (268), kin (269), therm (270)]
    // dmg is [em, therm, kin, exp] -> map to [em, exp, kin, therm]
    let d = [dmg[0].max(0.0), dmg[3].max(0.0), dmg[2].max(0.0), dmg[1].max(0.0)];
    let total_dmg: f64 = d.iter().sum();
    let d = if total_dmg > 1e-9 {
        d
    } else {
        [1.0, 0.0, 0.0, 0.0]
    };
    let total_d: f64 = d.iter().sum();
    let shares = [d[0] / total_d, d[1] / total_d, d[2] / total_d, d[3] / total_d];

    let mut res = [
        (1.0 - current_resonances[0]).clamp(0.0, 0.60),
        (1.0 - current_resonances[1]).clamp(0.0, 0.60),
        (1.0 - current_resonances[2]).clamp(0.0, 0.60),
        (1.0 - current_resonances[3]).clamp(0.0, 0.60),
    ];
    let shift = (shift_pct / 100.0).clamp(0.01, 0.20);
    let donors: Vec<usize> = (0..4).filter(|&i| shares[i] < 1e-6 && res[i] > 1e-6).collect();
    let donors = if donors.is_empty() {
        (0..4).filter(|&i| shares[i] < (res[i] / 0.60) && res[i] > 1e-6).collect::<Vec<usize>>()
    } else {
        donors
    };
    if donors.is_empty() {
        return current_resonances;
    }
    let per_donor = shift / (donors.len() as f64);
    let mut taken = 0.0;
    for &i in &donors {
        let take = res[i].min(per_donor);
        res[i] -= take;
        taken += take;
    }
    for i in 0..4 {
        if shares[i] > 1e-6 {
            res[i] = (res[i] + taken * shares[i]).min(0.60);
        }
    }
    [
        (1.0 - res[0]).clamp(0.40, 1.0),
        (1.0 - res[1]).clamp(0.40, 1.0),
        (1.0 - res[2]).clamp(0.40, 1.0),
        (1.0 - res[3]).clamp(0.40, 1.0),
    ]
}

pub fn on_ball_destroyed_by_damage(
    solar_system_id: SolarSystemId,
    ball_id: BallId,
    attacker_id: Option<CharacterId>,
    hook: Option<&NpcDeathHook>,
) -> Option<super::NpcDeathFuture> {
    hook.map(|h| h(solar_system_id, ball_id, attacker_id))
}

pub(crate) fn is_capital_remote_repair_module(
    module_type_id: u32,
    sde_opt: &Option<Arc<eve_sde::SdeRegistry>>,
) -> bool {
    sde_opt
        .as_ref()
        .and_then(|sde| sde.types.get(&module_type_id))
        .is_some_and(|t| {
            let req1 = t.default_attributes.get(&182).copied().unwrap_or(0.0) as u32;
            let pwr = t.default_attributes.get(&30).copied().unwrap_or(0.0);
            matches!(req1, 24568 | 24571 | 27936) || pwr >= 10_000.0
        })
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn step_cycles_and_capacitor(
    ballpark: &mut Ballpark,
    active_module_cycles: &mut Vec<ActiveModuleCycle>,
    pending_anchoring_disruptors: &mut Vec<PendingAnchoringDisruptor>,
    active_nanite_repairs: &mut Vec<ActiveNaniteRepair>,
    ship_rack_heat: &mut HashMap<BallId, [f64; 3]>,
    rah_incoming_damage: &mut HashMap<BallId, [f64; 4]>,
    base_scramble_strengths: &HashMap<BallId, i32>,
    base_core_strengths: &HashMap<BallId, i32>,
    sde_opt: &Option<Arc<eve_sde::SdeRegistry>>,
    db_opt: &Option<Arc<eve_db::WorldRepository>>,
    tactical_state: &std::sync::RwLock<crate::services::tactical_modules::TacticalModulesState>,
    pending_cycle_actions: &mut Vec<PendingCycleAction>,
    dt: f64,
    deactivation_hook: Option<&ModuleDeactivationHook>,
) -> Vec<(BallId, Option<CharacterId>)> {
    let mut destroyed_balls = Vec::new();
    crate::services::tactical_modules::step_wubble_probes_and_debuffs(
        ballpark,
        sde_opt,
        db_opt,
        tactical_state,
        dt,
    );

    // Exponential rack heat dissipation when no modules in a rack are overloaded (`feat:dogma.heat-rack-thermodynamics`)
    let ship_ids_with_heat: Vec<BallId> = ship_rack_heat.keys().copied().collect();
    for sid in ship_ids_with_heat {
        let Some(racks) = ship_rack_heat.get_mut(&sid) else {
            continue;
        };
        let mut changed = false;
        for (r_idx, rack_val) in racks.iter_mut().enumerate() {
            let has_ovl = active_module_cycles
                .iter()
                .any(|c| c.ship_id == sid && c.rack_idx == r_idx && c.is_overloaded);
            if !has_ovl && *rack_val > 0.0 {
                *rack_val = eve_dogma::thermodynamics::step_rack_heat(*rack_val, 0.0, 0.01, dt);
                if *rack_val < 1e-6 {
                    *rack_val = 0.0;
                }
                changed = true;
            }
        }
        if changed {
            let heat_snap = *racks;
            if let Some(db_ref) = db_opt {
                db_ref.with_mut_snapshot(|snap| {
                    if let Some(ship_item) = snap.items.get_mut(&sid) {
                        ship_item.dynamic_attributes.insert(1177, heat_snap[0]);
                        ship_item.dynamic_attributes.insert(1176, heat_snap[1]);
                        ship_item.dynamic_attributes.insert(1175, heat_snap[2]);
                    }
                });
            }
        }
    }

    // Advance in-space Nanite Repair Paste jobs (`feat:fitting.nanite-repair-paste-consumption`)
    let mut nr_idx = 0;
    while nr_idx < active_nanite_repairs.len() {
        active_nanite_repairs[nr_idx].elapsed_s += dt;
        let done = active_nanite_repairs[nr_idx].elapsed_s + 1e-6 >= active_nanite_repairs[nr_idx].duration_s;
        let mod_id = active_nanite_repairs[nr_idx].module_id;
        let rem_dmg = if done {
            0.0
        } else {
            let frac = (1.0 - active_nanite_repairs[nr_idx].elapsed_s / active_nanite_repairs[nr_idx].duration_s.max(0.1)).clamp(0.0, 1.0);
            active_nanite_repairs[nr_idx].initial_damage_hp * frac
        };
        if let Some(db_ref) = db_opt {
            db_ref.with_mut_snapshot(|snap| {
                if let Some(m_item) = snap.items.get_mut(&mod_id) {
                    if rem_dmg <= 1e-6 {
                        m_item.dynamic_attributes.remove(&eve_sde::attr_ids::DAMAGE);
                    } else {
                        m_item.dynamic_attributes.insert(eve_sde::attr_ids::DAMAGE, rem_dmg);
                    }
                }
            });
        }
        if done {
            active_nanite_repairs.remove(nr_idx);
        } else {
            nr_idx += 1;
        }
    }

    let mut idx_d = 0;
    while idx_d < pending_anchoring_disruptors.len() {
        pending_anchoring_disruptors[idx_d].remaining_s -= dt;
        if pending_anchoring_disruptors[idx_d].remaining_s <= 1e-6 {
            let done = pending_anchoring_disruptors.remove(idx_d);
            if let Some(ball) = ballpark.balls.get_mut(&done.id) {
                ball.mode = MovementMode::Field;
                ball.radius = done.radius.max(1.0);
            } else {
                let _ = ballpark.add_mobile_warp_disruptor(done.id, done.position, done.radius);
            }
            ballpark.add_warp_bubble(done.id, done.position, done.radius);
        } else {
            idx_d += 1;
        }
    }

    // Count active Capital remote repairers per target for Capital Remote Assistance Impedance
    let mut cap_remote_rep_ranks: HashMap<(BallId, i64), usize> = HashMap::new();
    {
        let mut by_target: HashMap<BallId, Vec<i64>> = HashMap::new();
        for c in active_module_cycles.iter() {
            if let Some(tid) = c.target_id {
                if (c.remote_shield_amount > 0.0 || c.remote_armor_amount > 0.0 || c.remote_hull_amount > 0.0)
                    && is_capital_remote_repair_module(c.module_type_id, sde_opt)
                {
                    by_target.entry(tid).or_default().push(c.module_id);
                }
            }
        }
        for (tid, mut mods) in by_target {
            mods.sort_unstable();
            for (rank, mid) in mods.into_iter().enumerate() {
                cap_remote_rep_ranks.insert((tid, mid), rank);
            }
        }
    }

    // Count overloaded modules per (ship_id, rack_idx)
    let mut overloaded_counts: HashMap<(BallId, usize), usize> = HashMap::new();
    for c in active_module_cycles.iter() {
        if c.is_overloaded {
            *overloaded_counts.entry((c.ship_id, c.rack_idx)).or_insert(0) += 1;
        }
    }

    let mut finished_indices = Vec::new();
    let mut burned_ship_modules: Vec<(BallId, i64)> = Vec::new();
    for (idx, cycle) in active_module_cycles.iter_mut().enumerate() {
        // If the source ship was destroyed / removed from space mid-cycle, cancel module cycle (`feat:combat.active-armor-repair-cycle-end`).
        if !ballpark.balls.contains_key(&cycle.ship_id) {
            finished_indices.push(idx);
            continue;
        }
        // [plan:DG-4] Targeted module cycles automatically deactivate if target leaves ballpark or exceeds maximum effective range.
        if let Some(target_id) = cycle.target_id {
            let source_ball = ballpark.balls.get(&cycle.ship_id);
            let target_ball = ballpark.balls.get(&target_id);
            let target_warping = target_ball
                .map(|b| matches!(b.mode, MovementMode::Warp { .. }))
                .unwrap_or(false);
            let out_of_range = if let (Some(sb), Some(tb), Some(max_r)) =
                (source_ball, target_ball, cycle.max_effective_range_m)
            {
                (sb.position - tb.position).length() > max_r
            } else {
                false
            };
            if target_ball.is_none() || target_warping || out_of_range {
                finished_indices.push(idx);
                continue;
            }
        }
        cycle.elapsed_s += dt;
        if cycle.elapsed_s + 1e-6 >= cycle.duration_s {
            let mod_grp = sde_opt
                .as_ref()
                .and_then(|sde| sde.types.get(&cycle.module_type_id))
                .map(|t| t.group_id)
                .unwrap_or(0);
            if mod_grp == 1122 {
                if let (Some(cid), Some(wid)) = (cycle.owner_char_id, cycle.target_id) {
                    pending_cycle_actions.push(PendingCycleAction::SalvageEnd {
                        char_id: cid,
                        ship_id: cycle.ship_id,
                        module_id: cycle.module_id,
                        module_type_id: cycle.module_type_id,
                        wreck_id: wid,
                        repeat: cycle.repeat,
                        effect_name: cycle.effect_name.clone(),
                    });
                }
            }
            // 1. Local armor repair and hull repair healing at cycle completion (`t = duration_s`, `feat:combat.active-armor-repair-cycle-end`)
            //    (Shield boosters apply at cycle start `t = 0`, `feat:combat.active-shield-boost-cycle-start`)
            if let Some(cs) = ballpark.combat_states.get_mut(&cycle.ship_id) {
                if cycle.armor_repair_amount > 0.0 {
                    cs.armor_hp = (cs.armor_hp + cycle.armor_repair_amount).min(cs.max_armor_hp);
                }
                if cycle.hull_repair_amount > 0.0 {
                    cs.structure_hp = (cs.structure_hp + cycle.hull_repair_amount).min(cs.max_structure_hp);
                }
            }

            // 1a. Reactive Armor Hardener resistance shifting (`feat:modules.reactive-armor-hardener`)
            if cycle.is_rah {
                let inc_dmg = rah_incoming_damage
                    .remove(&cycle.ship_id)
                    .unwrap_or([1.0, 0.0, 0.0, 0.0]);
                if let Some(db_ref) = db_opt {
                    let mut new_resonances = [0.85, 0.85, 0.85, 0.85];
                    db_ref.with_mut_snapshot(|snap| {
                        let cur = if let Some(m_item) = snap.items.get(&cycle.module_id) {
                            [
                                m_item.dynamic_attributes.get(&267).copied().unwrap_or(0.85),
                                m_item.dynamic_attributes.get(&268).copied().unwrap_or(0.85),
                                m_item.dynamic_attributes.get(&269).copied().unwrap_or(0.85),
                                m_item.dynamic_attributes.get(&270).copied().unwrap_or(0.85),
                            ]
                        } else {
                            [0.85, 0.85, 0.85, 0.85]
                        };
                        new_resonances = step_rah_resonances(cur, inc_dmg, cycle.rah_shift_amount);
                        if let Some(m_item) = snap.items.get_mut(&cycle.module_id) {
                            m_item.dynamic_attributes.insert(267, new_resonances[0]);
                            m_item.dynamic_attributes.insert(268, new_resonances[1]);
                            m_item.dynamic_attributes.insert(269, new_resonances[2]);
                            m_item.dynamic_attributes.insert(270, new_resonances[3]);
                        }
                        if let Some(s_item) = snap.items.get_mut(&cycle.ship_id) {
                            s_item.dynamic_attributes.insert(267, new_resonances[0]);
                            s_item.dynamic_attributes.insert(268, new_resonances[1]);
                            s_item.dynamic_attributes.insert(269, new_resonances[2]);
                            s_item.dynamic_attributes.insert(270, new_resonances[3]);
                        }
                    });
                    if let Some(cs) = ballpark.combat_states.get_mut(&cycle.ship_id) {
                        cs.armor_resonance = [
                            new_resonances[0].clamp(0.01, 1.0),
                            new_resonances[3].clamp(0.01, 1.0),
                            new_resonances[2].clamp(0.01, 1.0),
                            new_resonances[1].clamp(0.01, 1.0),
                        ];
                    }
                }
            }

            // 1b. Micro Jump Drive 100 km translation upon spool-up completion (`feat:modules.micro-jump-drive`)
            if let Some(jump_dist) = cycle.mjd_jump_distance_m {
                let scrambled = ballpark.is_ball_scrambled(cycle.ship_id)
                    || ballpark
                        .combat_states
                        .get(&cycle.ship_id)
                        .is_some_and(|cs| cs.is_mwd_mjd_disabled);
                if !scrambled {
                    let _ = ballpark.micro_jump(cycle.ship_id, jump_dist);
                }
            }

            // 1c. Auto-Targeting System (`feat:modules.auto-targeting-system`)
            if cycle.is_auto_targeter {
                if let Some(ship_ball) = ballpark.balls.get(&cycle.ship_id) {
                    let ship_pos = ship_ball.position;
                    let existing_locks = ballpark.get_target_locks(cycle.ship_id);
                    let mut lock_count = existing_locks.len();
                    let candidates: Vec<BallId> = ballpark
                        .balls
                        .values()
                        .filter(|b| {
                            b.id != cycle.ship_id
                                && !ballpark.is_static_ball(b.id)
                                && (b.position - ship_pos).length() <= cycle.auto_target_max_range
                                && !existing_locks.iter().any(|l| l.target_id == b.id)
                        })
                        .map(|b| b.id)
                        .collect();
                    for cand_id in candidates {
                        if lock_count >= cycle.auto_target_max_locks {
                            break;
                        }
                        ballpark.add_target_lock(
                            cycle.ship_id,
                            TargetLock {
                                target_id: cand_id,
                                lock_time_ms: 0.0,
                                remaining_lock_ms: 0.0,
                                locked: true,
                                max_range_m: Some(cycle.auto_target_max_range),
                            },
                        );
                        lock_count += 1;
                    }
                }
            }

            // 2. Target interactions: damage, cap drain, Nosferatu cap transfer, remote shield/armor/hull/cap (`DG-4`, `DG-5`)
            if let Some(target_id) = cycle.target_id {
                if cycle.damage_vector.total() > 0.0 {
                    let dmg_res = ballpark.apply_damage_to_ball(target_id, cycle.damage_vector);
                    let entry = rah_incoming_damage.entry(target_id).or_insert([0.0; 4]);
                    entry[0] += cycle.damage_vector.em;
                    entry[1] += cycle.damage_vector.thermal;
                    entry[2] += cycle.damage_vector.kinetic;
                    entry[3] += cycle.damage_vector.explosive;
                    if let Some((_, true, _)) = dmg_res {
                        let attacker_char_id = cycle.owner_char_id.or_else(|| {
                            ballpark.ball_slim_meta.get(&cycle.ship_id).and_then(|m| m.char_id.or(Some(m.owner_id)))
                        }).or_else(|| {
                            db_opt.as_ref().and_then(|db| db.with_snapshot(|s| {
                                s.characters.values().find(|c| c.active_ship_id == cycle.ship_id).map(|c| c.character_id)
                                    .or_else(|| s.items.get(&cycle.ship_id).map(|it| it.owner_id))
                            }))
                        });
                        destroyed_balls.push((target_id, attacker_char_id));
                    }
                }
                let src_cap_info = ballpark
                    .combat_states
                    .get(&cycle.ship_id)
                    .map(|s| (s.capacitor, s.max_capacitor));
                let impedance = if is_capital_remote_repair_module(cycle.module_type_id, sde_opt) {
                    let cap_rank = cap_remote_rep_ranks
                        .get(&(target_id, cycle.module_id))
                        .copied()
                        .unwrap_or(0);
                    1.0 / (1.0 + 0.25 * (cap_rank as f64))
                } else {
                    1.0
                };
                // Covers: mutadaptive remote armor repairer spool progression
                let spool_mult = if cycle.mutadaptive_spool_step > 0.0 {
                    let mult = 1.0
                        + ((cycle.current_spool_cycles as f64) * cycle.mutadaptive_spool_step)
                            .min(cycle.mutadaptive_spool_max);
                    cycle.current_spool_cycles = cycle.current_spool_cycles.saturating_add(1);
                    if let Some(db_ref) = db_opt {
                        let spooled_rep = cycle.remote_armor_amount * mult;
                        db_ref.with_mut_snapshot(|snap| {
                            if let Some(m_item) = snap.items.get_mut(&cycle.module_id) {
                                m_item.dynamic_attributes.insert(84, spooled_rep);
                            }
                        });
                    }
                    mult
                } else {
                    1.0
                };

                let is_sleeper_target = ballpark
                    .balls
                    .get(&target_id)
                    .is_some_and(|b| matches!(b.type_id, 30185..=30500));
                let mut nos_transferred = 0.0;
                if let Some(cs) = ballpark.combat_states.get_mut(&target_id) {
                    // Nosferatu transfer rule (`feat:combat.nosferatu-drain-cap`, `feat:modules.nosferatu-transfer-rule`, `feat:npc.sleeper-capacitor-immunity-and-leash`):
                    // Transfers at cycle end (`t = duration_s`) when target capacitor > self capacitor, or when Blood Raider hull bonus overrides the cap comparison;
                    // Sleepers have effectively infinite capacitor and are never drained by player neutralizers/nosferatus.
                    if cycle.cap_transfer_target > 0.0 {
                        let mut att = 1.0;
                        if cycle.remote_falloff > 0.0 || cycle.remote_optimal > 0.0 {
                            if let (Some(sb), Some(tb)) = (ballpark.balls.get(&cycle.ship_id), ballpark.balls.get(&target_id)) {
                                let dist = (sb.position - tb.position).length();
                                att = eve_dogma::compute_falloff_with_cutoff(dist, cycle.remote_optimal, cycle.remote_falloff);
                            }
                        }
                        let effective_transfer = cycle.cap_transfer_target * att;
                        if let Some((src_cap, src_max_cap)) = src_cap_info {
                            if is_sleeper_target
                                || cycle.nosferatu_blood_raider_override
                                || cs.capacitor > src_cap
                            {
                                let transferred = effective_transfer
                                    .min(if is_sleeper_target {
                                        effective_transfer
                                    } else {
                                        cs.capacitor
                                    })
                                    .min((src_max_cap - src_cap).max(0.0));
                                if !is_sleeper_target {
                                    cs.capacitor = (cs.capacitor - transferred).max(0.0);
                                }
                                nos_transferred = transferred;
                            }
                        }
                    }
                    // Remote armor / hull assistance at cycle end (shield/cap applied at cycle start; blocked if target has `disallow_assistance`)
                    if !cs.disallow_assistance {
                        let mut att = 1.0;
                        if cycle.remote_falloff > 0.0 || cycle.remote_optimal > 0.0 {
                            if let (Some(sb), Some(tb)) = (ballpark.balls.get(&cycle.ship_id), ballpark.balls.get(&target_id)) {
                                let dist = (sb.position - tb.position).length();
                                att = eve_dogma::compute_falloff_with_cutoff(dist, cycle.remote_optimal, cycle.remote_falloff);
                            }
                        }
                        if cycle.remote_armor_amount > 0.0 {
                            cs.armor_hp = (cs.armor_hp
                                + cycle.remote_armor_amount * att * spool_mult * impedance)
                                .min(cs.max_armor_hp);
                        }
                        if cycle.remote_hull_amount > 0.0 {
                            cs.structure_hp = (cs.structure_hp + cycle.remote_hull_amount * att * impedance)
                                .min(cs.max_structure_hp);
                        }
                    }
                }
                if nos_transferred > 0.0 {
                    if let Some(src_cs) = ballpark.combat_states.get_mut(&cycle.ship_id) {
                        src_cs.capacitor =
                            (src_cs.capacitor + nos_transferred).min(src_cs.max_capacitor);
                    }
                }
            }

            // 3. Thermodynamics & rack heat accumulation + slot-adjacency heat damage (`feat:dogma.heat-rack-thermodynamics`)
            // parity: feat:modules.emergency-hull-energizer
            let mut burned_out = false;
            if matches!(cycle.module_type_id, 40714 | 40717) {
                burned_out = true;
                if let Some(db_ref) = db_opt {
                    db_ref.with_mut_snapshot(|snap| {
                        if let Some(m_item) = snap.items.get_mut(&cycle.module_id) {
                            let max_hp = m_item
                                .dynamic_attributes
                                .get(&eve_sde::attr_ids::HP_STRUCT)
                                .copied()
                                .unwrap_or(40.0);
                            m_item
                                .dynamic_attributes
                                .insert(eve_sde::attr_ids::DAMAGE, max_hp);
                        }
                        snap.module_online_states.insert(cycle.module_id, false);
                        snap.module_active_states.remove(&cycle.module_id);
                    });
                }
            }
            if cycle.is_overloaded {
                let ovl_in_rack = overloaded_counts
                    .get(&(cycle.ship_id, cycle.rack_idx))
                    .copied()
                    .unwrap_or(1)
                    .max(1);
                let racks = ship_rack_heat.entry(cycle.ship_id).or_insert([0.0; 3]);
                let r_idx = cycle.rack_idx.min(2);
                racks[r_idx] = eve_dogma::thermodynamics::step_rack_heat(
                    racks[r_idx],
                    0.04 * (ovl_in_rack as f64),
                    0.01,
                    cycle.duration_s,
                );
                let current_rack_heat = racks[r_idx];
                let heat_snap = *racks;
                if let Some(db_ref) = db_opt {
                    db_ref.with_mut_snapshot(|snap| {
                        let owner_id = snap
                            .items
                            .get(&cycle.ship_id)
                            .map(|i| i.owner_id)
                            .unwrap_or(0);
                        let thermo_lvl = snap
                            .skills
                            .get(&owner_id)
                            .and_then(|sk| sk.get(&28164))
                            .map(|r| r.level)
                            .unwrap_or(0);
                        let eff_dmg = eve_dogma::thermodynamics::effective_heat_damage(
                            cycle.heat_damage_per_cycle,
                            thermo_lvl,
                        );
                        if let Some(ship_item) = snap.items.get_mut(&cycle.ship_id) {
                            ship_item.dynamic_attributes.insert(1177, heat_snap[0]);
                            ship_item.dynamic_attributes.insert(1176, heat_snap[1]);
                            ship_item.dynamic_attributes.insert(1175, heat_snap[2]);
                        }
                        let active_flag = snap
                            .items
                            .get(&cycle.module_id)
                            .map(|i| i.flag as u16)
                            .unwrap_or(0);
                        if let Some(m_item) = snap.items.get_mut(&cycle.module_id) {
                            let cur_dmg = m_item
                                .dynamic_attributes
                                .get(&eve_sde::attr_ids::DAMAGE)
                                .copied()
                                .unwrap_or(0.0);
                            let max_hp = m_item
                                .dynamic_attributes
                                .get(&eve_sde::attr_ids::HP_STRUCT)
                                .copied()
                                .unwrap_or(40.0);
                            let next_dmg = (cur_dmg + eff_dmg).min(max_hp);
                            m_item
                                .dynamic_attributes
                                .insert(eve_sde::attr_ids::DAMAGE, next_dmg);
                            if next_dmg >= max_hp - 1e-6 {
                                burned_out = true;
                            }
                        }
                        if burned_out {
                            snap.module_online_states.insert(cycle.module_id, false);
                            snap.module_active_states.remove(&cycle.module_id);
                        }
                        // Slot-distance-attenuated heat bleed onto adjacent modules in the same rack
                        if active_flag >= 11 && current_rack_heat > 0.0 {
                            let adj_ids: Vec<(i64, u16)> = snap
                                .items
                                .values()
                                .filter(|i| {
                                    i.location_id == cycle.ship_id
                                        && i.item_id != cycle.module_id
                                        && match (r_idx, i.flag as u16) {
                                            (0, 27..=34) => true,
                                            (1, 19..=26) => true,
                                            (2, 11..=18) => true,
                                            _ => false,
                                        }
                                })
                                .map(|i| (i.item_id, i.flag as u16))
                                .collect();
                            for (adj_id, adj_flag) in adj_ids {
                                let dist = (adj_flag as isize - active_flag as isize).unsigned_abs();
                                let att = eve_dogma::thermodynamics::heat_distance_attenuation(dist, 0.5);
                                let bleed_dmg = eff_dmg * current_rack_heat * att * 0.5;
                                if bleed_dmg > 1e-4 {
                                    let mut adj_burned = false;
                                    if let Some(adj_item) = snap.items.get_mut(&adj_id) {
                                        let cur = adj_item
                                            .dynamic_attributes
                                            .get(&eve_sde::attr_ids::DAMAGE)
                                            .copied()
                                            .unwrap_or(0.0);
                                        let max_hp = adj_item
                                            .dynamic_attributes
                                            .get(&eve_sde::attr_ids::HP_STRUCT)
                                            .copied()
                                            .unwrap_or(40.0);
                                        let next = (cur + bleed_dmg).min(max_hp);
                                        adj_item
                                            .dynamic_attributes
                                            .insert(eve_sde::attr_ids::DAMAGE, next);
                                        if next >= max_hp - 1e-6 {
                                            adj_burned = true;
                                        }
                                    }
                                    if adj_burned {
                                        snap.module_online_states.insert(adj_id, false);
                                        snap.module_active_states.remove(&adj_id);
                                        burned_ship_modules.push((cycle.ship_id, adj_id));
                                    }
                                }
                            }
                        }
                    });
                }
            }

            // 4. Weapon ammo & T2/Faction/Mining crystal volatility damage (`feat:modules.t2-crystal-volatility-destruction`)
            let mut crystal_shattered_empty = false;
            if let Some(db_ref) = db_opt {
                if !cycle.linked_turrets.is_empty() {
                    db_ref.with_mut_snapshot(|snap| {
                        for &turret_id in &cycle.linked_turrets {
                            let turret_flag = snap.items.get(&turret_id).map(|i| i.flag);
                            if let Some(t_flag) = turret_flag {
                                let charge_id_opt = snap
                                    .items
                                    .values()
                                    .find(|i| {
                                        i.location_id == cycle.ship_id
                                            && i.flag == t_flag
                                            && i.item_id != turret_id
                                            && (cycle.ammo_type_id.is_none()
                                                || Some(i.type_id) == cycle.ammo_type_id)
                                            && i.quantity > 0
                                    })
                                    .map(|i| i.item_id);
                                if let Some(cid) = charge_id_opt {
                                    if cycle.crystal_volatility_chance > 0.0
                                        && cycle.crystal_volatility_damage > 0.0
                                    {
                                        let mut destroy_stack = false;
                                        if let Some(c_item) = snap.items.get_mut(&cid) {
                                            let cur_dmg = c_item
                                                .dynamic_attributes
                                                .get(&eve_sde::attr_ids::DAMAGE)
                                                .copied()
                                                .unwrap_or(0.0);
                                            let max_hp = 1.0_f64;
                                            let next_dmg = cur_dmg + cycle.crystal_volatility_damage;
                                            if next_dmg >= max_hp - 1e-6 {
                                                if c_item.quantity > 1 {
                                                    c_item.quantity -= 1;
                                                    c_item
                                                        .dynamic_attributes
                                                        .insert(eve_sde::attr_ids::DAMAGE, 0.0);
                                                    c_item
                                                        .dynamic_attributes
                                                        .insert(eve_sde::attr_ids::HP_STRUCT, 1.0);
                                                } else {
                                                    destroy_stack = true;
                                                }
                                            } else {
                                                c_item
                                                    .dynamic_attributes
                                                    .insert(eve_sde::attr_ids::DAMAGE, next_dmg);
                                                c_item.dynamic_attributes.insert(
                                                    eve_sde::attr_ids::HP_STRUCT,
                                                    (max_hp - next_dmg).max(0.0),
                                                );
                                            }
                                        }
                                        if destroy_stack {
                                            snap.items.remove(&cid);
                                            crystal_shattered_empty = true;
                                        }
                                    } else if cycle.crystal_volatility_damage >= 0.0 {
                                        // Projectile / hybrid / missile charges consume 1 unit per shot
                                        if let Some(c_item) = snap.items.get_mut(&cid) {
                                            c_item.quantity = c_item.quantity.saturating_sub(1);
                                            if c_item.quantity == 0 {
                                                snap.items.remove(&cid);
                                                crystal_shattered_empty = true;
                                            }
                                        }
                                    }
                                } else if cycle.ammo_type_id.is_some()
                                    && cycle.damage_vector.total() > 0.0
                                {
                                    snap.items.retain(|_, i| {
                                        !(i.location_id == cycle.ship_id
                                            && i.flag == t_flag
                                            && i.item_id != turret_id
                                            && i.quantity <= 0)
                                    });
                                    crystal_shattered_empty = true;
                                }
                            }
                        }
                        if crystal_shattered_empty && cycle.crystal_volatility_damage == 0.0 {
                            if let Some(ammo_tid) = cycle.ammo_type_id {
                                let cargo_stack_opt = snap
                                    .items
                                    .values()
                                    .find(|i| {
                                        i.location_id == cycle.ship_id
                                            && matches!(
                                                i.flag,
                                                eve_db::InventoryFlag::Cargo
                                                    | eve_db::InventoryFlag::SpecializedAmmoHold
                                            )
                                            && i.type_id == ammo_tid
                                            && i.quantity > 0
                                    })
                                    .map(|i| (i.item_id, i.owner_id, i.quantity));
                                let num_mods = cycle.linked_turrets.len().max(1) as i32;
                                if let Some((stack_id, owner_id, avail_qty)) = cargo_stack_opt {
                                    if avail_qty >= num_mods {
                                        let charge_vol = sde_opt
                                            .as_ref()
                                            .and_then(|sde| sde.types.get(&ammo_tid))
                                            .map(|t| t.volume.max(0.0001))
                                            .unwrap_or(0.0025);
                                        let (rounds_per_mod, reload_time_ms) = sde_opt
                                            .as_ref()
                                            .and_then(|sde| sde.types.get(&cycle.module_type_id))
                                            .map(|mt| {
                                                let r = if mt.capacity > 0.0 {
                                                    (mt.capacity / charge_vol).floor() as i32
                                                } else {
                                                    1
                                                };
                                                let rt = mt
                                                    .default_attributes
                                                    .get(&1795)
                                                    .copied()
                                                    .unwrap_or(10_000.0);
                                                (r.max(1), rt)
                                            })
                                            .unwrap_or((1, 10_000.0));
                                        let actual_per_mod =
                                            rounds_per_mod.min(avail_qty / num_mods).max(1);
                                        let total_needed = actual_per_mod * num_mods;
                                        if let Some(st) = snap.items.get_mut(&stack_id) {
                                            if st.quantity > total_needed {
                                                st.quantity -= total_needed;
                                                st.version += 1;
                                            } else {
                                                snap.items.remove(&stack_id);
                                            }
                                        }
                                        for &tid in &cycle.linked_turrets {
                                            if let Some(tf) = snap.items.get(&tid).map(|i| i.flag) {
                                                snap.next_entity_id += 1;
                                                let new_cid = snap.next_entity_id;
                                                snap.items.insert(
                                                    new_cid,
                                                    eve_db::InventoryItemRecord {
                                                        item_id: new_cid,
                                                        type_id: ammo_tid,
                                                        owner_id,
                                                        location_id: cycle.ship_id,
                                                        flag: tf,
                                                        quantity: actual_per_mod,
                                                        singleton: false,
                                                        version: 1,
                                                        ..Default::default()
                                                    },
                                                );
                                            }
                                        }
                                        let now_unix = db_ref.clock().now_unix_secs();
                                        let reload_until =
                                            now_unix + (reload_time_ms / 1000.0).ceil() as u64;
                                        let mut map: HashMap<i64, u64> = snap
                                            .service_state_blobs
                                            .get("module_reload_until_unix")
                                            .and_then(|s| serde_json::from_str(s).ok())
                                            .unwrap_or_default();
                                        for &tid in &cycle.linked_turrets {
                                            map.insert(tid, reload_until);
                                        }
                                        if let Ok(json) = serde_json::to_string(&map) {
                                            snap.service_state_blobs
                                                .insert("module_reload_until_unix".into(), json);
                                        }
                                        if let Ok(ev_json) = serde_json::to_string(&(
                                            cycle.linked_turrets.clone(),
                                            ammo_tid,
                                            reload_time_ms,
                                        )) {
                                            snap.service_state_blobs.insert(
                                                format!("auto_reloaded_event:{}", cycle.module_id),
                                                ev_json,
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    });
                }
            }

            if cycle.repeat == 0 || burned_out || crystal_shattered_empty {
                finished_indices.push(idx);
            } else {
                cycle.elapsed_s -= cycle.duration_s;
                // Handle ASB, AAR, and Cap Booster charge consumption for the next cycle (`DG-4`)
                let mut stop_cycle = false;
                if cycle.is_asb {
                    let mut consumed_charge = false;
                    if let Some(db_ref) = db_opt {
                        db_ref.with_mut_snapshot(|snap| {
                            if let Some(m_flag) = snap.items.get(&cycle.module_id).map(|i| i.flag) {
                                if let Some(cid) = snap
                                    .items
                                    .values()
                                    .find(|i| {
                                        i.location_id == cycle.ship_id
                                            && i.flag == m_flag
                                            && i.item_id != cycle.module_id
                                            && i.quantity > 0
                                    })
                                    .map(|i| i.item_id)
                                {
                                    consumed_charge = true;
                                    let mut rem = 0;
                                    if let Some(ci) = snap.items.get_mut(&cid) {
                                        ci.quantity = ci.quantity.saturating_sub(1);
                                        rem = ci.quantity;
                                    }
                                    if rem == 0 {
                                        snap.items.remove(&cid);
                                    }
                                }
                            }
                        });
                    }
                    if consumed_charge {
                        cycle.cap_need_per_cycle = 0.0;
                    } else {
                        cycle.cap_need_per_cycle = cycle.asb_fallback_cap_need;
                    }
                } else if cycle.is_aar {
                    let mut fueled = false;
                    if let Some(db_ref) = db_opt {
                        db_ref.with_mut_snapshot(|snap| {
                            if let Some(m_flag) = snap.items.get(&cycle.module_id).map(|i| i.flag) {
                                if let Some(cid) = snap
                                    .items
                                    .values()
                                    .find(|i| {
                                        i.location_id == cycle.ship_id
                                            && i.flag == m_flag
                                            && i.item_id != cycle.module_id
                                            && i.type_id == 28668
                                            && i.quantity >= cycle.aar_paste_rate.max(1)
                                    })
                                    .map(|i| i.item_id)
                                {
                                    fueled = true;
                                    let mut rem = 0;
                                    if let Some(ci) = snap.items.get_mut(&cid) {
                                        ci.quantity -= cycle.aar_paste_rate.max(1);
                                        rem = ci.quantity;
                                    }
                                    if rem == 0 {
                                        snap.items.remove(&cid);
                                    }
                                }
                            }
                        });
                    }
                    let rep_amt = if fueled {
                        cycle.aar_base_armor_repair * 3.0
                    } else {
                        cycle.aar_base_armor_repair
                    };
                    if matches!(cycle.module_type_id, 41476..=41479) {
                        cycle.remote_armor_amount = rep_amt;
                    } else {
                        cycle.armor_repair_amount = rep_amt;
                    }
                } else if cycle.cap_booster_inject_amount > 0.0 {
                    let mut consumed_charge = false;
                    if let Some(db_ref) = db_opt {
                        db_ref.with_mut_snapshot(|snap| {
                            if let Some(m_flag) = snap.items.get(&cycle.module_id).map(|i| i.flag) {
                                if let Some(cid) = snap
                                    .items
                                    .values()
                                    .find(|i| {
                                        i.location_id == cycle.ship_id
                                            && i.flag == m_flag
                                            && i.item_id != cycle.module_id
                                            && i.quantity > 0
                                    })
                                    .map(|i| i.item_id)
                                {
                                    consumed_charge = true;
                                    let mut rem = 0;
                                    if let Some(ci) = snap.items.get_mut(&cid) {
                                        ci.quantity = ci.quantity.saturating_sub(1);
                                        rem = ci.quantity;
                                    }
                                    if rem == 0 {
                                        snap.items.remove(&cid);
                                    }
                                }
                            }
                        });
                    }
                    if consumed_charge {
                        if let Some(cs) = ballpark.combat_states.get_mut(&cycle.ship_id) {
                            cs.capacitor = (cs.capacitor + cycle.cap_booster_inject_amount)
                                .min(cs.max_capacitor);
                        }
                    } else {
                        stop_cycle = true;
                    }
                }

                if !stop_cycle && cycle.cap_need_per_cycle > 0.0 {
                    let mut has_cap = true;
                    if let Some(cs) = ballpark.combat_states.get_mut(&cycle.ship_id) {
                        if cs.capacitor >= cycle.cap_need_per_cycle {
                            cs.capacitor -= cycle.cap_need_per_cycle;
                        } else {
                            has_cap = false;
                        }
                    }
                    if !has_cap {
                        stop_cycle = true;
                    }
                }
                if stop_cycle {
                    finished_indices.push(idx);
                } else {
                    if matches!(mod_grp, 72 | 80 | 353 | 650) {
                        if let Some(cid) = cycle.owner_char_id {
                            pending_cycle_actions.push(PendingCycleAction::RepeatCycleStart {
                                char_id: cid,
                                ship_id: cycle.ship_id,
                                module_id: cycle.module_id,
                                module_type_id: cycle.module_type_id,
                                target_id: cycle.target_id,
                                duration_s: cycle.duration_s,
                                is_overloaded: cycle.is_overloaded,
                                effect_name: cycle.effect_name.clone(),
                            });
                        }
                    }
                    // Apply start-of-next-cycle (`t = 0`) effects: active shield boost, energy neutralizer drain, remote shield/cap
                    if cycle.shield_boost_amount > 0.0 {
                        if let Some(cs) = ballpark.combat_states.get_mut(&cycle.ship_id) {
                            let boost_mult = if cs.shield_boost_multiplier > 0.0 { cs.shield_boost_multiplier } else { 1.0 };
                            cs.shield_hp = (cs.shield_hp + cycle.shield_boost_amount * boost_mult).min(cs.max_shield_hp);
                        }
                    }
                    if let Some(target_id) = cycle.target_id {
                        let is_sleeper_target = ballpark
                            .balls
                            .get(&target_id)
                            .is_some_and(|b| matches!(b.type_id, 30185..=30500));
                        let impedance = if is_capital_remote_repair_module(cycle.module_type_id, sde_opt) {
                            let cap_rank = cap_remote_rep_ranks
                                .get(&(target_id, cycle.module_id))
                                .copied()
                                .unwrap_or(0);
                            1.0 / (1.0 + 0.25 * (cap_rank as f64))
                        } else {
                            1.0
                        };
                        if let Some(cs) = ballpark.combat_states.get_mut(&target_id) {
                            let mut att = 1.0;
                            if cycle.remote_falloff > 0.0 || cycle.remote_optimal > 0.0 {
                                if let (Some(sb), Some(tb)) = (ballpark.balls.get(&cycle.ship_id), ballpark.balls.get(&target_id)) {
                                    let dist = (sb.position - tb.position).length();
                                    att = eve_dogma::compute_falloff_with_cutoff(dist, cycle.remote_optimal, cycle.remote_falloff);
                                }
                            }
                            if cycle.cap_drain_target > 0.0 && !is_sleeper_target {
                                cs.capacitor = (cs.capacitor - cycle.cap_drain_target * att).max(0.0);
                            }
                            if !cs.disallow_assistance {
                                if cycle.remote_shield_amount > 0.0 {
                                    cs.shield_hp = (cs.shield_hp
                                        + cycle.remote_shield_amount * att * impedance)
                                        .min(cs.max_shield_hp);
                                }
                                if cycle.remote_cap_amount > 0.0 {
                                    cs.capacitor = (cs.capacitor + cycle.remote_cap_amount * att)
                                        .min(cs.max_capacitor);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    for (burned_ship_id, burned_mod_id) in burned_ship_modules {
        for (other_idx, c) in active_module_cycles.iter().enumerate() {
            if c.ship_id == burned_ship_id && c.module_id == burned_mod_id {
                finished_indices.push(other_idx);
            }
        }
    }
    finished_indices.sort_unstable();
    finished_indices.dedup();
    for idx in finished_indices.into_iter().rev() {
        let finished = active_module_cycles.remove(idx);
        cleanup_removed_module_cycle(ballpark, &finished, db_opt);
        emit_cycle_deactivation(&finished, ballpark, db_opt, deactivation_hook);
    }
    reconcile_tackle_and_bubbles(
        ballpark,
        active_module_cycles,
        base_scramble_strengths,
        base_core_strengths,
    );
    destroyed_balls
}

pub(crate) fn cleanup_removed_module_cycle(
    ballpark: &mut Ballpark,
    removed: &ActiveModuleCycle,
    db_opt: &Option<Arc<eve_db::WorldRepository>>,
) {
    if let Some(orig_v) = removed.original_max_velocity {
        if let Some(ball) = ballpark.balls.get_mut(&removed.ship_id) {
            ball.max_velocity = orig_v;
        }
    }
    if let Some(orig_tv) = removed.target_original_max_velocity {
        if let Some(tid) = removed.target_id {
            if let Some(t_ball) = ballpark.balls.get_mut(&tid) {
                t_ball.max_velocity = orig_tv;
            }
        }
    }
    if let Some(orig_m) = removed.original_mass {
        if let Some(ball) = ballpark.balls.get_mut(&removed.ship_id) {
            ball.mass = orig_m;
        }
    }
    if let Some(orig_sig) = removed.original_signature_radius {
        if let Some(cs) = ballpark.combat_states.get_mut(&removed.ship_id) {
            cs.signature_radius = orig_sig;
        }
    }
    if let Some((sr, ar, hr)) = removed.adc_saved_resists {
        if let Some(cs) = ballpark.combat_states.get_mut(&removed.ship_id) {
            cs.shield_resonance = sr;
            cs.armor_resonance = ar;
            cs.structure_resonance = hr;
        }
        if let Some(db_ref) = db_opt {
            db_ref.with_mut_snapshot(|snap| {
                if let Some(s_item) = snap.items.get_mut(&removed.ship_id) {
                    for aid in [267, 268, 269, 270, 271, 272, 273, 274, 109, 110, 111, 112, 113] {
                        s_item.dynamic_attributes.remove(&aid);
                    }
                }
            });
        }
    }
    if removed.mutadaptive_spool_step > 0.0 {
        if let Some(db_ref) = db_opt {
            db_ref.with_mut_snapshot(|snap| {
                if let Some(m_item) = snap.items.get_mut(&removed.module_id) {
                    m_item.dynamic_attributes.insert(84, removed.remote_armor_amount);
                }
            });
        }
    }
    if removed.is_rah {
        if let Some(db_ref) = db_opt {
            db_ref.with_mut_snapshot(|snap| {
                if let Some(m_item) = snap.items.get_mut(&removed.module_id) {
                    for aid in 267..=270 { m_item.dynamic_attributes.insert(aid, 0.85); }
                }
                if let Some(s_item) = snap.items.get_mut(&removed.ship_id) {
                    for aid in 267..=270 { s_item.dynamic_attributes.remove(&aid); }
                }
            });
        }
    }
    if removed.hic_bubble_radius_m.is_some() { ballpark.remove_warp_bubble(removed.module_id); }
    if removed.grants_interdiction_nullification { ballpark.set_interdiction_nullified(removed.ship_id, false); }
    if let Some(db_ref) = db_opt {
        db_ref.with_mut_snapshot(|snap| {
            snap.module_active_states.remove(&removed.module_id);
            snap.module_active_charges.remove(&removed.module_id);
        });
    }
}

pub(crate) fn emit_cycle_deactivation(
    cycle: &ActiveModuleCycle,
    ballpark: &Ballpark,
    db_opt: &Option<Arc<eve_db::WorldRepository>>,
    hook: Option<&ModuleDeactivationHook>,
) {
    let char_id = cycle.owner_char_id.or_else(|| {
        ballpark.ball_slim_meta.get(&cycle.ship_id).and_then(|m| m.char_id.or(if m.owner_id > 0 { Some(m.owner_id) } else { None }))
    }).or_else(|| {
        db_opt.as_ref().and_then(|db| db.with_snapshot(|s| {
            s.characters.values().find(|c| c.active_ship_id == cycle.ship_id).map(|c| c.character_id)
                .or_else(|| s.items.get(&cycle.ship_id).map(|it| it.owner_id))
        }))
    });
    if let (Some(cid), Some(h)) = (char_id, hook) {
        h(cid, cycle);
    }
}

/// Parity: [plan:DG-4] Entering warp halts active non-warp module cycles (weapons, scrams, webs, mining lasers, etc.)
/// Only modules that specifically grant interdiction nullification (warp core stabilizers / nullifiers) remain.
pub(crate) fn remove_non_warp_ship_module_cycles(
    ballpark: &mut Ballpark,
    active_module_cycles: &mut Vec<ActiveModuleCycle>,
    base_scramble_strengths: &HashMap<BallId, i32>,
    base_core_strengths: &HashMap<BallId, i32>,
    db_opt: &Option<Arc<eve_db::WorldRepository>>,
    ship_id: BallId,
    deactivation_hook: Option<&ModuleDeactivationHook>,
) {
    let mut removed_cycles = Vec::new();
    let mut i = 0;
    while i < active_module_cycles.len() {
        if active_module_cycles[i].ship_id == ship_id && !active_module_cycles[i].grants_interdiction_nullification {
            let removed = active_module_cycles.swap_remove(i);
            cleanup_removed_module_cycle(ballpark, &removed, db_opt);
            removed_cycles.push(removed);
        } else {
            i += 1;
        }
    }
    if !removed_cycles.is_empty() {
        reconcile_tackle_and_bubbles(
            ballpark,
            active_module_cycles,
            base_scramble_strengths,
            base_core_strengths,
        );
        for removed in &removed_cycles {
            emit_cycle_deactivation(removed, ballpark, db_opt, deactivation_hook);
        }
    }
}

pub(crate) fn remove_all_ship_module_cycles(
    ballpark: &mut Ballpark,
    active_module_cycles: &mut Vec<ActiveModuleCycle>,
    base_scramble_strengths: &HashMap<i64, i32>,
    base_core_strengths: &HashMap<i64, i32>,
    db_opt: &Option<Arc<eve_db::WorldRepository>>,
    ship_id: i64,
    deactivation_hook: Option<&ModuleDeactivationHook>,
) {
    let mut removed_cycles = Vec::new();
    let mut i = 0;
    while i < active_module_cycles.len() {
        if active_module_cycles[i].ship_id == ship_id {
            let removed = active_module_cycles.swap_remove(i);
            cleanup_removed_module_cycle(ballpark, &removed, db_opt);
            removed_cycles.push(removed);
        } else {
            i += 1;
        }
    }
    if let Some(db_ref) = db_opt {
        db_ref.with_mut_snapshot(|snap| {
            let ship_mod_ids: Vec<i64> = snap.items.values().filter(|it| it.location_id == ship_id).map(|it| it.item_id).collect();
            for mid in ship_mod_ids {
                snap.module_active_states.remove(&mid);
                snap.module_active_charges.remove(&mid);
            }
        });
    }
    if !removed_cycles.is_empty() {
        reconcile_tackle_and_bubbles(ballpark, active_module_cycles, base_scramble_strengths, base_core_strengths);
        for removed in &removed_cycles {
            emit_cycle_deactivation(removed, ballpark, db_opt, deactivation_hook);
        }
    }
}

impl SystemHandle {
    pub fn has_active_modules_or_cloak_sync(&self, ball_id: BallId) -> bool {
        if self.active_module_ships.read().unwrap_or_else(|e| e.into_inner()).contains(&ball_id) {
            return true;
        }
        let bp = self.ballpark.read().unwrap_or_else(|e| e.into_inner());
        bp.combat_states.get(&ball_id).is_some_and(|cs| cs.is_cloaked)
            || bp.ball_slim_meta.get(&ball_id).is_some_and(|m| m.is_cloaked)
    }

    pub fn set_last_auto_stepped_unix(&self, now_unix: u64) {
        self.last_auto_stepped_unix.store(now_unix, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn take_auto_stepped_at(&self, now_unix: u64) -> bool {
        now_unix > 0 && self.last_auto_stepped_unix.swap(0, std::sync::atomic::Ordering::SeqCst) == now_unix
    }

    pub fn try_step_full_tick(&self, dt: f64) -> u64 {
        let mut bp = self.ballpark.write().unwrap_or_else(|e| e.into_inner());
        let mut st = self.actor_state.write().unwrap_or_else(|e| e.into_inner());
        let SystemActorState {
            active_module_cycles, pending_anchoring_disruptors, active_nanite_repairs,
            ship_rack_heat, rah_incoming_damage, base_scramble_strengths, base_core_strengths,
            asteroids, asteroid_templates, sde_opt, db_opt, mechanic_rng_opt: _,
            sim_elapsed_secs, pending_cycle_actions, tactical_state,
        } = &mut *st;
        if dt > 0.0 && dt.is_finite() {
            *sim_elapsed_secs += dt;
        }
        respawn_due_asteroids(&mut bp, asteroids, asteroid_templates, sde_opt, db_opt);
        sync_player_slim_items(&mut bp, db_opt);
        let events = bp.step(dt);
        crate::services::pve_special_sites_abyssal::step_abyssal_hazards_and_pylons_in_ballpark(
            &mut bp,
            sde_opt.as_deref(),
            db_opt.as_ref(),
            self.solar_system_id,
        );
        let hook_guard = self.npc_death_hook.read().unwrap_or_else(|e| e.into_inner()).clone();
        let deact_hook_guard = self.module_deactivation_hook.read().unwrap_or_else(|e| e.into_inner()).clone();
        let destroyed = step_cycles_and_capacitor(
            &mut bp, active_module_cycles, pending_anchoring_disruptors, active_nanite_repairs,
            ship_rack_heat, rah_incoming_damage, base_scramble_strengths, base_core_strengths,
            sde_opt, db_opt, tactical_state, pending_cycle_actions, dt, deact_hook_guard.as_ref(),
        );
        for &(target_ball_id, owner_char_id) in &destroyed {
            if let Some(db) = db_opt.as_ref() {
                crate::services::pve_special_sites_abyssal::on_abyssal_room_npc_destroyed_in_ballpark(
                    &mut bp,
                    db,
                    self.solar_system_id,
                    target_ball_id,
                );
            }
            if let Some(fut) = on_ball_destroyed_by_damage(self.solar_system_id, target_ball_id, owner_char_id, hook_guard.as_ref()) {
                if let Ok(handle) = tokio::runtime::Handle::try_current() {
                    let _ = handle.spawn(fut);
                }
            }
        }
        let step_hook_guard = self.system_step_hook.read().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(hook) = step_hook_guard {
            let fut = hook(self.solar_system_id, dt, destroyed);
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                let _ = handle.spawn(fut);
            }
        }
        if let Some(db) = db_opt {
            for (&bid, cs) in &bp.combat_states {
                if !bp.is_static_ball(bid) && bid < 700_000_000 {
                    crate::sp6_lifecycle::persist_ship_combat_hp_in_db(db, bid, cs);
                }
            }
        }
        sync_ball_positions(&bp, &self.ball_positions, &self.scrambled_balls);
        {
            let mut mod_guard = self.active_module_ships.write().unwrap_or_else(|e| e.into_inner());
            mod_guard.clear();
            for c in active_module_cycles.iter() {
                mod_guard.insert(c.ship_id);
            }
        }
        let tick = bp.current_tick;
        let broadcast = crate::system_actor::build_tick_broadcast(self.solar_system_id, &mut bp, events);
        let _ = self.event_tx.send(broadcast);
        tick
    }

    #[allow(clippy::too_many_arguments)]
    pub fn prepare_disconnected_warp_sync(
        &self, ship_id: BallId, type_id: u32, radius: f64, mass: f64,
        inertia: f64, max_velocity: f64, warp_speed_au_s: f64, current_pos: DVec3, dest: DVec3,
    ) {
        let mut bp = self.ballpark.write().unwrap_or_else(|e| e.into_inner());
        if !bp.balls.contains_key(&ship_id) {
            bp.add_ball(eve_destiny::Ball {
                id: ship_id, type_id, position: current_pos, velocity: DVec3::ZERO,
                max_velocity, speed_fraction: 1.0, inertia, mass, radius, mode: MovementMode::Stop,
            });
        }
        let needs_warp_init = bp.balls.get(&ship_id).is_some_and(|b| {
            !matches!(b.mode, MovementMode::Warp { .. }) && (b.position - dest).length() > 5_000.0
        });
        if needs_warp_init {
            let start_pos = bp.balls.get(&ship_id).map(|b| b.position).unwrap_or(current_pos);
            let _ = bp.set_mode(ship_id, MovementMode::new_warp(start_pos, dest, warp_speed_au_s.max(1.5)));
        }
        sync_ball_positions(&bp, &self.ball_positions, &self.scrambled_balls);
    }

    pub fn reconcile_disconnected_warp_sync(&self, ship_id: BallId, dest: DVec3) -> (DVec3, bool) {
        let mut bp = self.ballpark.write().unwrap_or_else(|e| e.into_inner());
        if let Some(cs) = bp.combat_states.get_mut(&ship_id) {
            cs.undock_invulnerable_remaining_ticks = 0;
            cs.warp_exit_invulnerable_remaining_ticks = 0;
        }
        let arrived = bp.balls.get(&ship_id).is_none_or(|b| {
            !matches!(b.mode, MovementMode::Warp { .. }) || (b.position - dest).length() <= 2_500.0
        });
        if arrived {
            let _ = bp.set_ball_position(ship_id, dest);
            let _ = bp.set_mode(ship_id, MovementMode::Stop);
            if let Some(b) = bp.balls.get_mut(&ship_id) {
                b.velocity = DVec3::ZERO;
                b.speed_fraction = 0.0;
            }
        }
        sync_ball_positions(&bp, &self.ball_positions, &self.scrambled_balls);
        let final_pos = bp.balls.get(&ship_id).map(|b| b.position).unwrap_or(dest);
        (final_pos, arrived)
    }
}
