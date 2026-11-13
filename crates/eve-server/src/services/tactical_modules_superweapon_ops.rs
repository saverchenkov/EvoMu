//! Titan Doomsday and Lancer Dreadnought Disruptive Lance activation & warmup mechanics (`#496`).

#![forbid(unsafe_code)]

use super::*;
use crate::router::RpcArgs;
use eve_db::lock_rank::AnyRwLock;

/// Rejects superweapon activation if `ship_id` is currently tethered (`CantActivateWhileTethered`, `#496`).
pub(crate) async fn check_superweapon_tether_guard(
    ctx: &ServerContext,
    sys_id: u32,
    ship_id: i64,
) -> Result<(), BridgeRpcError> {
    let sys = ctx.systems.get_or_spawn(sys_id);
    if sys
        .get_combat_state(ship_id)
        .await
        .ok()
        .flatten()
        .is_some_and(|cs| cs.is_tethered)
    {
        return Err(BridgeRpcError {
            code: "CantActivateWhileTethered".into(),
            message: "Cannot activate a Superweapon or Disruptive Lance while tethered".into(),
            client_error: None,
        });
    }
    Ok(())
}

/// Returns `true` when `target_id` / `target_type_id` is an eligible Capital/Supercapital hull or Structure
/// for a single-target Titan Doomsday (`!is_slash`, `#496`).
fn is_eligible_single_target_doomsday_target(
    ctx: &ServerContext,
    target_id: i64,
    fallback_type_id: u32,
) -> bool {
    let (is_struct, eff_type_id) = ctx.db.with_snapshot(|snap| {
        let st = snap.structures.contains_key(&target_id);
        let tid = snap
            .items
            .get(&target_id)
            .map(|i| i.type_id)
            .unwrap_or(fallback_type_id);
        (st, tid)
    });
    if is_struct {
        return true;
    }
    let Some(t) = ctx.sde.types.get(&eff_type_id) else {
        return false;
    };
    t.category_id == 65
        || matches!(
            t.group_id,
            30 | 365 | 485 | 513 | 547 | 659 | 883 | 902 | 1538 | 4594
        )
}

/// Handles Titan Doomsday and Lancer Dreadnought Disruptive Lance activation (`feat:modules.disruptive-lance`, `feat:modules.doomsday`).
pub async fn activate_superweapon_or_lance(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    sys_id: u32,
    module_id: i64,
    action_method: &str,
    target_id_opt: Option<i64>,
) -> Result<PyRep, BridgeRpcError> {
    activate_superweapon_with_args(
        ctx,
        char_id,
        ship_id,
        sys_id,
        module_id,
        action_method,
        target_id_opt,
        &[],
    )
    .await
}

fn parse_vec3_arg(args: &[PyRep], idx: usize) -> Result<Option<DVec3>, BridgeRpcError> {
    RpcArgs::positional(args)
        .opt_finite_vec3(idx)
        .map_err(|e| BridgeRpcError {
            code: "InvalidDirectionVector".into(),
            message: e.message,
            client_error: None,
        })
}

fn check_superweapon_fuel_available(
    ctx: &ServerContext,
    ship_id: i64,
    fuel_tid: u32,
    fuel_qty: i32,
    kind: &str,
) -> Result<(), BridgeRpcError> {
    ctx.db
        .with_snapshot(|snap| {
            let total_avail: i64 = snap
                .items
                .values()
                .filter(|i| {
                    i.location_id == ship_id
                        && i.type_id == fuel_tid
                        && matches!(i.flag as u16, 5 | 133)
                        && i.flag != eve_db::InventoryFlag::Locked
                        && i.quantity > 0
                        && !snap.is_item_locked_in_contract_or_job(i.item_id)
                })
                .map(|i| i64::from(i.quantity))
                .sum();
            if total_avail >= i64::from(fuel_qty) {
                Ok(())
            } else if total_avail > 0 {
                Err(format!(
                    "Insufficient {kind} fuel (type {fuel_tid}): need {fuel_qty}, have {total_avail}"
                ))
            } else {
                Err(format!(
                    "Missing required {kind} fuel (type {fuel_tid}, quantity {fuel_qty})"
                ))
            }
        })
        .map_err(|message| BridgeRpcError {
            code: "InsufficientFuel".into(),
            message,
            client_error: None,
        })
}

fn deduct_superweapon_fuel(
    ctx: &ServerContext,
    ship_id: i64,
    fuel_tid: u32,
    fuel_qty: i32,
    kind: &str,
) -> Result<(i32, Vec<(eve_db::InventoryFlag, i32)>), BridgeRpcError> {
    ctx.db
        .try_with_mut_snapshot(|snap| {
            let mut stacks: Vec<(u8, i64, eve_db::InventoryFlag, i32)> = snap
                .items
                .values()
                .filter(|i| {
                    i.location_id == ship_id
                        && i.type_id == fuel_tid
                        && matches!(i.flag as u16, 5 | 133)
                        && i.flag != eve_db::InventoryFlag::Locked
                        && i.quantity > 0
                        && !snap.is_item_or_ancestor_locked_or_impounded(i.item_id)
                })
                .map(|i| (if (i.flag as u16) == 133 { 0 } else { 1 }, i.item_id, i.flag, i.quantity))
                .collect();
            stacks.sort_by_key(|&(prio, iid, _, _)| (prio, iid));
            let total_avail: i64 = stacks.iter().map(|&(_, _, _, q)| i64::from(q)).sum();
            if total_avail < i64::from(fuel_qty) {
                return Err(if total_avail > 0 {
                    format!("Insufficient {kind} fuel (type {fuel_tid}): need {fuel_qty}, have {total_avail}")
                } else {
                    format!("Missing required {kind} fuel (type {fuel_tid}, quantity {fuel_qty})")
                });
            }
            let mut remaining = fuel_qty;
            let mut consumed_by_flag: Vec<(eve_db::InventoryFlag, i32)> = Vec::new();
            for (_, fid, flag, _) in stacks {
                if remaining <= 0 {
                    break;
                }
                let Some((owner_id, avail)) = snap.items.get(&fid).map(|it| (it.owner_id, it.quantity)) else {
                    continue;
                };
                let took = remaining.min(avail);
                if took <= 0 {
                    continue;
                }
                let opts = eve_db::ConsumeExactOptions::for_owner_exact(None, owner_id);
                snap.consume_exact_item_quantity(fid, took, opts)
                    .map_err(|e| e.to_string())?;
                remaining -= took;
                if let Some((_, q)) = consumed_by_flag.iter_mut().find(|(f, _)| *f == flag) {
                    *q += took;
                } else {
                    consumed_by_flag.push((flag, took));
                }
            }
            Ok((fuel_qty, consumed_by_flag))
        })
        .map_err(|message| BridgeRpcError {
            code: "InsufficientFuel".into(),
            message,
            client_error: None,
        })
}

fn refund_superweapon_fuel(
    ctx: &ServerContext,
    ship_id: i64,
    owner_char_id: i64,
    fuel_tid: u32,
    consumed_by_flag: &[(eve_db::InventoryFlag, i32)],
) {
    if consumed_by_flag.is_empty() {
        return;
    }
    ctx.db.with_mut_snapshot(|snap| {
        for &(flag, qty) in consumed_by_flag {
            if qty <= 0 {
                continue;
            }
            if let Some(existing) = snap
                .items
                .values_mut()
                .find(|i| i.location_id == ship_id && i.type_id == fuel_tid && i.flag == flag && !i.singleton)
            {
                existing.quantity += qty;
                existing.version += 1;
                continue;
            }
            snap.next_entity_id += 1;
            let new_id = snap.next_entity_id;
            snap.items.insert(
                new_id,
                eve_db::InventoryItemRecord {
                    item_id: new_id,
                    type_id: fuel_tid,
                    owner_id: owner_char_id,
                    location_id: ship_id,
                    flag,
                    quantity: qty,
                    singleton: false,
                    ..eve_db::InventoryItemRecord::default()
                },
            );
        }
    });
}

fn precheck_doomsday_structure_target(
    ctx: &ServerContext,
    sys_id: u32,
    attacker_char_id: i64,
    target_id: i64,
    now_unix: u64,
) -> Result<(), BridgeRpcError> {
    let is_upwell_or_sov =
        crate::services::station_structure::structure_lifecycle_rpc::validate_upwell_structure_attack_precheck(
            ctx,
            sys_id,
            attacker_char_id,
            target_id,
            now_unix,
        )?;
    if is_upwell_or_sov {
        let can_take_weapon_damage = ctx.db.with_snapshot(|snap| {
            let Some(st) = snap.structures.get(&target_id) else {
                return true;
            };
            st.type_id == crate::services::sov_gameplay::TYPE_MERCENARY_DEN
                || st.type_id == crate::services::sov_services::TYPE_ORBITAL_SKYHOOK
                || matches!(
                    st.type_id,
                    crate::services::pve_fob_observatory::FOB_GURISTAS_TYPE_ID
                        | crate::services::pve_fob_observatory::FOB_BLOOD_RAIDER_TYPE_ID
                        | crate::services::insurgency_services::PIRATE_STRONGHOLD_ANGEL_TYPE
                        | crate::services::insurgency_services::PIRATE_STRONGHOLD_GURISTAS_TYPE
                        | crate::services::fw_gameplay::TYPE_FW_IHUB
                )
                || snap.fw_runtime.supply_sites.contains_key(&target_id)
                || ctx.sde.types.get(&st.type_id).is_some_and(|t| t.group_id == 4644)
                || crate::services::station_structure::structure_lifecycle::structure_profile(ctx, st.type_id).is_some()
        });
        if !can_take_weapon_damage {
            return Err(BridgeRpcError {
                code: "StructureInvulnerable".into(),
                message: format!("Structure {target_id} is invulnerable to direct weapon and superweapon damage"),
                client_error: None,
            });
        }
    }
    if crate::services::insurgency_services::is_insurgency_stronghold_invulnerable(ctx, sys_id, target_id)
        || ctx.db.with_snapshot(|snap| {
            snap.items
                .get(&target_id)
                .and_then(|it| it.dynamic_attributes.get(&2110).copied())
                .is_some_and(|v| v <= 0.0)
        })
    {
        return Err(BridgeRpcError::user_error("TargetInvulnerable"));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn activate_superweapon_with_args(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    sys_id: u32,
    module_id: i64,
    action_method: &str,
    target_id_opt: Option<i64>,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    crate::services::inventory_fitting::fitting_cyno_portal::sweep_expired_cyno_beacons(ctx);
    if ctx.is_module_in_reactivation_delay(ship_id, module_id) {
        return Err(BridgeRpcError {
            code: "ModuleInReactivationDelay".into(),
            message: format!("Module {module_id} is in reactivation delay"),
            client_error: None,
        });
    }
    let (ship_type_id, ship_group_id, mod_type_id, has_active_siege) = ctx.db.with_snapshot(|snap| {
        let st = snap.items.get(&ship_id).map(|s| s.type_id).unwrap_or(0);
        let sg = ctx.sde.types.get(&st).map(|t| t.group_id).unwrap_or(0);
        let mt = snap
            .items
            .get(&module_id)
            .filter(|m| m.location_id == ship_id)
            .map(|m| m.type_id)
            .unwrap_or(0);
        let sieged = snap.items.values().any(|i| {
            i.location_id == ship_id
                && snap.module_active_states.contains(&i.item_id)
                && matches!(i.type_id, 20280 | 4292)
        });
        (st, sg, mt, sieged)
    });

    let is_lancer = ship_group_id == 4594 || matches!(mod_type_id, 77398..=77401);
    if ship_group_id != 30 && !is_lancer {
        return Err(BridgeRpcError {
            code: "InvalidShipGroup".into(),
            message: "Superweapons can only be activated on Titans (group 30) or Lancer Dreadnoughts (group 4594)".into(),
            client_error: None,
        });
    }
    let sys_sec = crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
        rt.modified_system_securities.get(&sys_id).copied()
    })
    .unwrap_or_else(|| {
        ctx.sde
            .solar_systems
            .get(&sys_id)
            .map(|s| s.security_status)
            .unwrap_or(0.0)
    });
    if sys_sec >= 0.45 {
        return Err(BridgeRpcError {
            code: "CannotActivateDoomsdayInHighSec".into(),
            message: format!("Superweapons and Disruptive Lances cannot be activated in high-sec (system {sys_id} security {sys_sec:.2})"),
            client_error: None,
        });
    }
    if !is_lancer && sys_sec > 0.0 {
        return Err(BridgeRpcError {
            code: "CannotActivateLanceInLowSec".into(),
            message: format!("Titan superweapons and AoE Lances can only be activated in null-sec or wormhole space (system {sys_id} security {sys_sec:.2})"),
            client_error: None,
        });
    }
    let Some(mod_type) = ctx.sde.types.get(&mod_type_id) else {
        return Err(BridgeRpcError {
            code: "ModuleNotFound".into(),
            message: format!("Superweapon module {module_id} not found on ship"),
            client_error: None,
        });
    };
    if mod_type.group_id != 588 {
        return Err(BridgeRpcError {
            code: "InvalidModuleGroup".into(),
            message: format!("Module {module_id} is not a Super Weapon (group 588)"),
            client_error: None,
        });
    }

    let sys = ctx.systems.get_or_spawn(sys_id);
    let now_unix = ctx.now_unix_secs();

    if is_lancer {
        if !has_active_siege {
            return Err(BridgeRpcError {
                code: "SiegeRequiredForLance".into(),
                message: "Disruptive Lance can only be activated while a Siege Module is active".into(),
                client_error: None,
            });
        }
        let skills = ctx.character_skill_levels(char_id);
        let op_lvl = skills.get(&77739).copied().unwrap_or(0);
        let lancer_lvl = skills
            .get(&77738)
            .copied()
            .or_else(|| skills.get(&76079).copied())
            .unwrap_or(0);
        if op_lvl == 0 || lancer_lvl == 0 {
            return Err(BridgeRpcError {
                code: "SkillMissing".into(),
                message: "Activating a Disruptive Lance requires Disruptive Lance Operation (77739) and Lancer Dreadnoughts (77738)".into(),
                client_error: None,
            });
        }
        if sys.get_ball_snapshot(ship_id).await.ok().flatten().is_none() {
            return Err(BridgeRpcError {
                code: "ShipNotInSpace".into(),
                message: "Disruptive Lance can only be activated while ship is in space".into(),
                client_error: None,
            });
        }
        crate::services::inventory_fitting::fitting_module_cycles::enforce_space_action_cloak_and_warp_guard(
            ctx,
            &sys,
            char_id,
            ship_id,
            "activate Disruptive Lance",
        )
        .await?;
        check_superweapon_tether_guard(ctx, sys_id, ship_id).await?;

        let ship_type_opt = ctx.sde.types.get(&ship_type_id);
        let base_cap = mod_type.default_attributes.get(&6).copied().unwrap_or(30_000.0);
        let cap_bonus_pct = ctx
            .sde
            .types
            .get(&77_739)
            .and_then(|t| t.default_attributes.get(&5419).copied())
            .unwrap_or(-5.0)
            .abs()
            / 100.0;
        let cap_cost = base_cap * (1.0 - cap_bonus_pct * (op_lvl.min(5) as f64));
        let cur_cap_opt = if let Ok(Some(cs)) = sys.get_combat_state(ship_id).await {
            if cs.capacitor + 1e-4 < cap_cost {
                return Err(BridgeRpcError {
                    code: "NotEnoughCapacitor".into(),
                    message: format!("Insufficient capacitor for Disruptive Lance: need {cap_cost:.1} GJ"),
                    client_error: None,
                });
            }
            Some(cs.capacitor)
        } else {
            None
        };

        let lance_fuel_tid = mod_type.default_attributes.get(&713).map(|&v| v as u32).unwrap_or(16274);
        let lance_fuel_qty = mod_type.default_attributes.get(&714).map(|&v| v.round() as i32).unwrap_or(20_000);
        check_superweapon_fuel_available(ctx, ship_id, lance_fuel_tid, lance_fuel_qty, "lance")?;

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
        let target_pos = target_id_opt
            .and_then(|tid| all_balls.iter().find(|b| b.id == tid).map(|b| b.position))
            .unwrap_or(ship_pos + DVec3::new(1.0, 0.0, 0.0));
        let beam_delta = target_pos - ship_pos;
        let beam_dir = if beam_delta.length_squared() > 1e-4 {
            beam_delta.normalize()
        } else {
            DVec3::new(1.0, 0.0, 0.0)
        };
        let beam_radius = mod_type.default_attributes.get(&2263).copied().unwrap_or(2_500.0);
        let neut_radius = mod_type.default_attributes.get(&2259).copied().unwrap_or(10_000.0).max(beam_radius);
        let caster_lock_secs = (mod_type.default_attributes.get(&73).copied().unwrap_or(300_000.0) / 1_000.0).round() as u64;

        // Validate Crimewatch safety across ALL eligible cone targets BEFORE deducting isotopes or draining capacitor (`#478`).
        let _ = crate::services::r6_t1_p0_helpers::collect_and_precheck_lancer_cone_targets(
            ctx,
            &sys,
            sys_id,
            char_id,
            ship_id,
            &all_balls,
            ship_pos,
            beam_dir,
            effective_range,
            neut_radius,
            now_unix,
        )
        .await?;

        let (consumed_fuel, consumed_stacks) =
            deduct_superweapon_fuel(ctx, ship_id, lance_fuel_tid, lance_fuel_qty, "lance")?;
        if let Some(cur_cap) = cur_cap_opt {
            let _ = sys.set_capacitor(ship_id, (cur_cap - cap_cost).max(0.0)).await;
        }

        let hit_targets = match crate::services::r6_t1_p0_helpers::execute_lancer_beam_attack(
            ctx,
            &sys,
            sys_id,
            char_id,
            ship_id,
            ship_type_id,
            mod_type_id,
            &all_balls,
            ship_pos,
            beam_dir,
            effective_range,
            beam_radius,
            neut_radius,
            total_dmg_vec,
            neut_amount_gj,
            victim_debuff_secs,
            now_unix,
        )
        .await
        {
            Ok(hits) => hits,
            Err(err) => {
                refund_superweapon_fuel(ctx, ship_id, char_id, lance_fuel_tid, &consumed_stacks);
                if let Some(cur_cap) = cur_cap_opt {
                    let _ = sys.set_capacitor(ship_id, cur_cap).await;
                }
                return Err(err);
            }
        };
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
        ctx.persist_server_runtime_to_db();

        return Ok(PyRep::keyval([
            ("moduleID", PyRep::Integer(module_id)),
            ("shipID", PyRep::Integer(ship_id)),
            ("slashMode", PyRep::Bool(true)),
            ("totalDamageApplied", PyRep::Real(lance_damage)),
            ("perTickDamage", PyRep::Real(per_tick_dmg.total())),
            ("capacitorCost", PyRep::Real(cap_cost)),
            ("effectiveRangeMeters", PyRep::Real(effective_range)),
            ("neutralizerAmountGJ", PyRep::Real(neut_amount_gj)),
            ("remoteRepairMultiplier", PyRep::Real(0.50)),
            ("remoteRepairImpedance", PyRep::Real(0.50)),
            ("jumpDockTetherCloakLockSeconds", PyRep::Real(victim_debuff_secs as f64)),
            ("casterLockSeconds", PyRep::Integer(300)),
            ("victimPointDurationSeconds", PyRep::Real(victim_debuff_secs as f64)),
            ("victimDebuffSeconds", PyRep::Integer(victim_debuff_secs as i64)),
            ("hitCount", PyRep::Integer(hit_targets.len() as i64)),
            ("hitTargetIDs", PyRep::List(hit_targets.into_iter().map(PyRep::Integer).collect())),
            ("fuelTypeID", PyRep::Integer(lance_fuel_tid as i64)),
            ("fuelConsumed", PyRep::Integer(consumed_fuel as i64)),
        ]));
    }

    let all_balls = sys.list_balls().await.unwrap_or_default();
    let Some(ship_ball) = all_balls.iter().find(|b| b.id == ship_id).cloned() else {
        return Err(BridgeRpcError {
            code: "ShipNotInSpace".into(),
            message: "Superweapon can only be activated while ship is in space".into(),
            client_error: None,
        });
    };
    crate::services::inventory_fitting::fitting_module_cycles::enforce_space_action_cloak_and_warp_guard(
        ctx,
        &sys,
        char_id,
        ship_id,
        "activate Superweapon",
    )
    .await?;
    check_superweapon_tether_guard(ctx, sys_id, ship_id).await?;

    let ship_pos = ship_ball.position;
    let is_slash = action_method == "ActivateSlashModule";

    let mut single_target_victim_char: Option<i64> = None;
    let mut single_target_is_player_struct = false;
    let single_target_ball = if !is_slash {
        let Some(tid) = target_id_opt else {
            return Err(BridgeRpcError {
                code: "TargetRequired".into(),
                message: "Single-point Doomsday requires a targetID".into(),
                client_error: None,
            });
        };
        if tid == ship_id {
            return Err(BridgeRpcError {
                code: "InvalidTarget".into(),
                message: "Cannot fire Doomsday at own ship".into(),
                client_error: None,
            });
        }
        let Some(tb) = all_balls.iter().find(|b| b.id == tid).cloned() else {
            return Err(BridgeRpcError {
                code: "TargetNotInSpace".into(),
                message: format!("Target {tid} is not in space"),
                client_error: None,
            });
        };
        if let Some(tcs) = sys.get_combat_state(tid).await.ok().flatten() {
            if tcs.is_cloaked {
                return Err(BridgeRpcError::user_error("CannotTargetCloaked"));
            }
            if tcs.is_tethered
                || tcs.is_invulnerable()
                || tcs.undock_invulnerable_remaining_ticks > 0
                || tcs.gate_cloak_remaining_ticks > 0
            {
                return Err(BridgeRpcError::user_error("TargetInvulnerable"));
            }
        }
        if crate::services::space_destiny::is_location_change_invulnerable(ctx, tid, now_unix) {
            return Err(BridgeRpcError::user_error("TargetInvulnerable"));
        }
        let locks = sys.get_target_locks(ship_id).await.unwrap_or_default();
        if !locks.iter().any(|l| l.target_id == tid && l.locked) {
            return Err(BridgeRpcError {
                code: "TargetNotLocked".into(),
                message: format!("Target {tid} is not locked"),
                client_error: None,
            });
        }
        if !is_eligible_single_target_doomsday_target(ctx, tid, tb.type_id) {
            return Err(BridgeRpcError {
                code: "InvalidTarget".into(),
                message: "Single-target Doomsdays can only target Capital ships or Structures".into(),
                client_error: None,
            });
        }
        let max_dd_range = mod_type
            .default_attributes
            .get(&54)
            .copied()
            .unwrap_or(250_000.0)
            .max(100_000.0);
        if (tb.position - ship_pos).length() > max_dd_range {
            return Err(BridgeRpcError {
                code: "TargetOutOfRange".into(),
                message: format!("Target {tid} exceeds Doomsday range ({max_dd_range:.0}m)"),
                client_error: None,
            });
        }
        let (vic_char_opt, is_p_struct) = ctx.db.with_snapshot(|snap| {
            let vc = snap
                .characters
                .values()
                .find(|c| c.active_ship_id == tid)
                .map(|c| c.character_id)
                .or_else(|| {
                    snap.items
                        .get(&tid)
                        .map(|i| i.owner_id)
                        .filter(|oid| snap.characters.contains_key(oid))
                });
            let ps = snap.structures.contains_key(&tid);
            (vc, ps)
        });
        if let Some(vcid) = vic_char_opt {
            if crate::services::space_destiny::is_location_change_invulnerable(ctx, vcid, now_unix) {
                return Err(BridgeRpcError {
                    code: "TargetInvulnerable".into(),
                    message: format!("Target character {vcid} is invulnerable"),
                    client_error: None,
                });
            }
        }
        crate::services::combat_crimewatch::crimewatch_timers::precheck_pvp_aggression_safety(
            ctx,
            char_id,
            sys_id,
            tid,
            tb.type_id == 670,
        )?;
        precheck_doomsday_structure_target(ctx, sys_id, char_id, tid, now_unix)?;
        single_target_victim_char = vic_char_opt;
        single_target_is_player_struct = is_p_struct;
        Some(tb)
    } else {
        None
    };

    let (slash_p0, slash_p1) = if is_slash {
        (parse_vec3_arg(args, 1)?, parse_vec3_arg(args, 2)?)
    } else {
        (None, None)
    };

    let fuel_tid = mod_type.default_attributes.get(&713).map(|&v| v as u32).unwrap_or(16274);
    let fuel_qty = mod_type.default_attributes.get(&714).map(|&v| v.round() as i32).unwrap_or(50_000);
    let dd_cap_cost = mod_type.default_attributes.get(&6).copied().unwrap_or(0.0);
    let dd_cur_cap_opt = if dd_cap_cost > 0.0 {
        if let Ok(Some(cs)) = sys.get_combat_state(ship_id).await {
            if cs.capacitor + 1e-4 < dd_cap_cost {
                return Err(BridgeRpcError {
                    code: "NotEnoughCapacitor".into(),
                    message: format!("Insufficient capacitor for Doomsday: need {dd_cap_cost:.1} GJ"),
                    client_error: None,
                });
            }
            Some(cs.capacitor)
        } else {
            None
        }
    } else {
        None
    };
    check_superweapon_fuel_available(ctx, ship_id, fuel_tid, fuel_qty, "superweapon")?;
    let (consumed_fuel, consumed_stacks) =
        deduct_superweapon_fuel(ctx, ship_id, fuel_tid, fuel_qty, "superweapon")?;
    if let Some(cur_cap) = dd_cur_cap_opt {
        let _ = sys.set_capacitor(ship_id, (cur_cap - dd_cap_cost).max(0.0)).await;
    }

    let em_dmg = mod_type.default_attributes.get(&114).copied().unwrap_or(0.0);
    let ex_dmg = mod_type.default_attributes.get(&116).copied().unwrap_or(0.0);
    let ki_dmg = mod_type.default_attributes.get(&117).copied().unwrap_or(0.0);
    let th_dmg = mod_type.default_attributes.get(&118).copied().unwrap_or(0.0);
    let dmg_vec = DamageVector::new(em_dmg, th_dmg, ki_dmg, ex_dmg);
    let total_raw_damage = dmg_vec.em + dmg_vec.thermal + dmg_vec.kinetic + dmg_vec.explosive;

    let mut hit_targets = Vec::new();
    let default_fwd = if ship_ball.velocity.length_squared() > 1e-4 {
        ship_ball.velocity.normalize()
    } else {
        DVec3::X
    };
    let beam_len = mod_type.default_attributes.get(&54).copied().unwrap_or(200_000.0);
    let beam_radius = mod_type.default_attributes.get(&2263).copied().unwrap_or(2_500.0);
    let warmup_s = mod_type.default_attributes.get(&2262).copied().unwrap_or(15_000.0) / 1000.0;
    let dmg_dur_ms = mod_type.default_attributes.get(&2264).copied().unwrap_or(15_000.0);
    let dmg_cycle_ms = mod_type.default_attributes.get(&2265).copied().unwrap_or(1_000.0).max(100.0);
    let num_ticks = (dmg_dur_ms / dmg_cycle_ms).round().max(1.0) as i32;
    let lock_secs = (mod_type.default_attributes.get(&2427).copied().unwrap_or(300_000.0) / 1000.0).round() as u64;

    if is_slash {
        let raw_dir = match (slash_p0, slash_p1) {
            (Some(p0), Some(p1)) if (p1 - p0).length_squared() > 1e-4 => Some(p1 - p0),
            (Some(p0), Some(p1)) => Some(if p0.length_squared() > 1e-4 { p0 } else { p1 }),
            (a, b) => a.or(b),
        };
        let beam_dir = raw_dir
            .map(|v| if v.length_squared() > 1e-4 { v.normalize() } else { default_fwd })
            .unwrap_or(default_fwd);
        if let Ok(balls) = sys.list_balls().await {
            for b in balls {
                if b.id != ship_id && is_combatant_ship_or_entity_ball(Some(&ctx.sde), Some(&ctx.db), b.id, b.type_id) {
                    let rel = b.position - ship_ball.position;
                    let al = rel.dot(beam_dir);
                    if (0.0..=beam_len).contains(&al) && (rel - beam_dir * al).length() <= beam_radius {
                        hit_targets.push(b.id);
                    }
                }
            }
        }
        ctx.tactical_state
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .lance_caster_lock_until
            .insert((db_key_of(ctx), ship_id), now_unix + lock_secs);
        let _ = sys
            .add_module_cycle(crate::system_actor::ActiveModuleCycle {
                ship_id,
                module_id,
                module_type_id: mod_type_id,
                owner_char_id: Some(char_id),
                effect_name: "superWeaponLanceWarmup".into(),
                repeat: num_ticks - 1,
                duration_s: warmup_s,
                immobilize_self: true,
                damage_vector: dmg_vec,
                lance_beam_dir: Some(beam_dir),
                ..Default::default()
            })
            .await;
    } else if let Some(tb) = single_target_ball {
        let (app, destroyed) =
            match crate::services::station_structure::structure_lifecycle_rpc::apply_damage_structure_or_ball(
                ctx, &sys, sys_id, char_id, tb.id, dmg_vec, 10.0, now_unix,
            )
            .await
            {
                Ok(res) => res.map(|(a, d, _)| (a, d)).unwrap_or((0.0, false)),
                Err(err) => {
                    refund_superweapon_fuel(ctx, ship_id, char_id, fuel_tid, &consumed_stacks);
                    if let Some(cur_cap) = dd_cur_cap_opt {
                        let _ = sys.set_capacitor(ship_id, cur_cap).await;
                    }
                    return Err(err);
                }
            };
        // Apply 300s jump/dock/tether/cloak lock and 30s self-immobilization cycle on single-target Doomsday (`#496`).
        ctx.tactical_state
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .lance_caster_lock_until
            .insert((db_key_of(ctx), ship_id), now_unix + lock_secs);
        let _ = sys
            .add_module_cycle(crate::system_actor::ActiveModuleCycle {
                ship_id,
                module_id,
                module_type_id: mod_type_id,
                owner_char_id: Some(char_id),
                effect_name: "superWeaponImmobilize".into(),
                repeat: 0,
                duration_s: 30.0,
                immobilize_self: true,
                ..Default::default()
            })
            .await;
        crate::services::combat_crimewatch::record_killmail_assist_action(
            ctx,
            tb.id,
            char_id,
            ship_type_id,
            mod_type_id,
            app,
            false,
            now_unix,
        );
        if let Some(vcid) = single_target_victim_char.filter(|&v| v != char_id) {
            crate::sp6_lifecycle::record_pvp_aggression_and_renew_logoff_timer(ctx, char_id, vcid, now_unix);
        } else if single_target_is_player_struct {
            crate::sp6_lifecycle::record_pvp_aggression_and_renew_logoff_timer(ctx, char_id, 0, now_unix);
        } else {
            crate::sp6_lifecycle::record_npc_aggression_timer(ctx, char_id, now_unix);
        }
        if destroyed {
            crate::services::combat_crimewatch::combat_missiles_aoe::handle_destroyed_ball_from_missile_or_aoe(
                ctx,
                sys_id,
                char_id,
                tb.id,
                tb.type_id,
                tb.position,
                now_unix,
            )
            .await;
        }
        hit_targets.push(tb.id);
    }

    ctx.crimewatch
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_create(char_id)
        .weapon_timer_until_unix = now_unix + 60;
    ctx.record_module_reactivation_delay(ship_id, module_id, lock_secs as f64);

    let godma_effect_id = crate::router::binding::effect_id_for_module_sde(
        &ctx.sde,
        mod_type_id,
        None,
    )
    .unwrap_or(if is_slash { 6472 } else { 4489 });
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnGodmaShipEffect",
        "charid",
        vec![
            PyRep::Integer(module_id),
            PyRep::Integer(godma_effect_id),
            PyRep::Bool(true),
        ],
    );
    if !is_slash {
        let tgt_id = hit_targets.first().copied().unwrap_or(0);
        let dmg_dict = PyRep::Dict(vec![
            (PyRep::String("damage".into()), PyRep::Real(total_raw_damage)),
            (PyRep::String("source".into()), PyRep::Integer(ship_id)),
            (PyRep::String("attackerID".into()), PyRep::Integer(ship_id)),
            (PyRep::String("target".into()), PyRep::Integer(tgt_id)),
            (PyRep::String("target_ID".into()), PyRep::Integer(tgt_id)),
            (
                PyRep::String("owner".into()),
                PyRep::Tuple(vec![PyRep::Integer(2), PyRep::Integer(char_id)]),
            ),
            (PyRep::String("weapon".into()), PyRep::Integer(mod_type_id as i64)),
            (PyRep::String("hitQuality".into()), PyRep::Integer(5)),
            (PyRep::String("attackType".into()), PyRep::String("me".into())),
            (PyRep::String("isWrecking".into()), PyRep::Bool(false)),
            (PyRep::String("isBanked".into()), PyRep::Bool(false)),
        ]);
        ctx.notify(
            NotificationTarget::Character(char_id),
            "OnDamageMessage",
            "charid",
            vec![dmg_dict.clone()],
        );
        ctx.notify(
            NotificationTarget::Character(char_id),
            "OnDamageMessages",
            "charid",
            vec![PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::String("AttackHit".into()),
                dmg_dict,
            ])])],
        );
    }

    Ok(PyRep::keyval([
        ("moduleID", PyRep::Integer(module_id)),
        ("shipID", PyRep::Integer(ship_id)),
        ("slashMode", PyRep::Bool(is_slash)),
        ("totalDamageApplied", PyRep::Real(if is_slash { 0.0 } else { total_raw_damage })),
        ("fuelConsumed", PyRep::Integer(consumed_fuel as i64)),
        ("warmupSeconds", PyRep::Real(if is_slash { warmup_s } else { 0.0 })),
        ("tickCount", PyRep::Integer(if is_slash { num_ticks as i64 } else { 1 })),
        ("immobilizeSeconds", PyRep::Real(if is_slash { lock_secs as f64 } else { 30.0 })),
        ("jumpBlockSeconds", PyRep::Real(lock_secs as f64)),
        ("jumpWarpSuppressionSeconds", PyRep::Real(0.0)),
        ("hitTargetIDs", PyRep::List(hit_targets.into_iter().map(PyRep::Integer).collect())),
    ]))
}

pub(crate) fn fire_lance_warmup_in_ballpark(
    ballpark: &mut eve_destiny::Ballpark,
    cycle: &crate::system_actor::ActiveModuleCycle,
    sde_opt: &Option<Arc<eve_sde::SdeRegistry>>,
    db_opt: &Option<Arc<eve_db::WorldRepository>>,
    tactical_state: &dyn AnyRwLock<TacticalModulesState>,
    pending_cycle_actions: &mut Vec<crate::system_actor::PendingCycleAction>,
    destroyed_balls: &mut Vec<(i64, Option<i64>)>,
) {
    let _ = tactical_state;
    let Some(ship_ball) = ballpark.balls.get(&cycle.ship_id) else {
        return;
    };
    let ship_pos = ship_ball.position;
    let attacker_ship_type_id = ship_ball.type_id;
    let beam_dir = cycle.lance_beam_dir.unwrap_or(DVec3::X);
    let mod_attrs = sde_opt
        .as_ref()
        .and_then(|s| s.types.get(&cycle.module_type_id))
        .map(|t| &t.default_attributes);
    let beam_len = mod_attrs.and_then(|a| a.get(&54).copied()).unwrap_or(200_000.0);
    let beam_radius = mod_attrs.and_then(|a| a.get(&2263).copied()).unwrap_or(2_500.0);
    let dmg_sig = mod_attrs.and_then(|a| a.get(&552).copied()).unwrap_or(5_000.0).max(1.0);
    let lock_s = mod_attrs.and_then(|a| a.get(&2427).copied()).unwrap_or(300_000.0) / 1000.0;
    let candidates: Vec<(i64, u32, DVec3)> = ballpark
        .balls
        .values()
        .filter(|b| {
            b.id != cycle.ship_id
                && is_combatant_ship_or_entity_ball(sde_opt.as_ref(), db_opt.as_ref(), b.id, b.type_id)
        })
        .map(|b| (b.id, b.type_id, b.position))
        .collect();
    let mut hits = Vec::new();
    for (bid, _tid, bpos) in candidates {
        let to_b = bpos - ship_pos;
        let along = to_b.dot(beam_dir);
        let perp = (to_b - beam_dir * along).length();
        if along > 0.0 && along <= beam_len && perp <= beam_radius {
            let skip_target = ballpark.combat_states.get(&bid).is_some_and(|v_cs| {
                v_cs.is_cloaked
                    || v_cs.is_tethered
                    || v_cs.is_invulnerable()
                    || v_cs.undock_invulnerable_remaining_ticks > 0
                    || v_cs.gate_cloak_remaining_ticks > 0
            });
            if skip_target {
                continue;
            }
            let target_sig = ballpark
                .combat_states
                .get(&bid)
                .map(|cs| cs.signature_radius.max(1.0))
                .unwrap_or(dmg_sig);
            let sig_scale = (target_sig / dmg_sig).min(1.0);
            let scaled_dmg = cycle.damage_vector.scaled(sig_scale);
            let mut applied_dmg = 0.0;
            if let Some((app, destroyed, _)) = ballpark.apply_damage_to_ball(bid, scaled_dmg) {
                applied_dmg = app;
                if destroyed {
                    destroyed_balls.push((bid, cycle.owner_char_id));
                }
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

#[cfg(test)]
#[path = "tactical_modules_superweapon_ops_edges_tests.rs"]
mod tactical_modules_superweapon_ops_edges_tests;
