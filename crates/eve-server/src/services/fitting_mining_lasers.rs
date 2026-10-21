//! `fitting_mining_lasers` — Mining laser activation, `Deactivate`, `StopOverload`, rack overload, nanite repair & `miningScanMgr`.

#![forbid(unsafe_code)]

use super::*;

#[allow(clippy::too_many_arguments)]
pub(crate) async fn activate_mining_module(
    ctx: &ServerContext,
    session: &SessionState,
    char_id: i64,
    ship_id: i64,
    module_id: i64,
    mod_type: &eve_sde::InvType,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
                let items = ctx.db.get_items_for_character(char_id);
                let Some(mod_item) = items.iter().find(|i| i.item_id == module_id && i.owner_id == char_id).cloned() else {
                    return Some(Err(BridgeRpcError {
                        code: "ModuleNotFound".into(),
                        message: format!("Module {module_id} not found on character ship"),
                    }));
                };

                // Determine target asteroid ID
                let target_id_opt = args.get(2).and_then(|a| a.as_i64())
                    .or_else(|| args.get(1).and_then(|a| a.as_i64()))
                    .or_else(|| kwargs.get("target").and_then(|v| v.as_i64()));

                let target_id = match target_id_opt {
                    Some(tid) if tid > 0 => tid,
                    _ => {
                        let sys_id = session.solar_system_id.unwrap_or(0);
                        let any_ast = ctx.db.with_snapshot(|snap| {
                            snap.asteroids.values().find(|a| a.solar_system_id == sys_id && a.quantity > 0).map(|a| a.asteroid_id)
                        });
                        match any_ast {
                            Some(tid) => tid,
                            None => {
                                return Some(Err(BridgeRpcError {
                                    code: "InvalidTarget".into(),
                                    message: "Mining module requires a locked target asteroid".into(),
                                }));
                            }
                        }
                    }
                };

                let Some(asteroid) = ctx.db.get_asteroid(target_id) else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidTarget".into(),
                        message: format!("Target asteroid {target_id} not found"),
                    }));
                };
                if asteroid.quantity <= 0 {
                    return Some(Err(BridgeRpcError {
                        code: "AsteroidDepleted".into(),
                        message: format!("Target asteroid {target_id} is depleted"),
                    }));
                }

                // Target type validity: mining laser -> ore, ice harvester -> ice, gas scoop -> gas (feat:mining.mining-module-target-validity)
                let is_target_ice = asteroid.type_id == 16262
                    || ctx.sde.types.get(&asteroid.type_id).is_some_and(|t| t.group_id == 465);
                let is_target_gas = asteroid.type_id == 25268
                    || ctx.sde.types.get(&asteroid.type_id).is_some_and(|t| t.group_id == 711);
                let is_mod_ice = mod_type.group_id == 464
                    && (mod_type.type_id == 16278
                        || mod_type.type_id == 22229
                        || mod_type.name.contains("Ice"));
                let is_mod_gas = mod_type.group_id == 737 || mod_type.group_id == 4138;
                let is_mod_ore = !is_mod_ice && !is_mod_gas;

                if is_mod_ice && !is_target_ice {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidTargetType".into(),
                        message: "Ice harvesters can only harvest ice".into(),
                    }));
                }
                if is_mod_gas && !is_target_gas {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidTargetType".into(),
                        message: "Gas cloud scoops can only harvest gas clouds".into(),
                    }));
                }
                if is_mod_ore && (is_target_ice || is_target_gas) {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidTargetType".into(),
                        message: "Mining lasers and strip miners can only harvest ore".into(),
                    }));
                }

                // Mercoxit deep core restriction (feat:mining.mercoxit-deep-core-restriction)
                let is_mercoxit = asteroid.type_id == 11396
                    || ctx.sde.types.get(&asteroid.type_id).is_some_and(|t| t.group_id == 468);
                if is_mercoxit {
                    let is_deep_core = mod_type.name.contains("Deep Core")
                        || mod_type.default_attributes.get(&277) == Some(&11395.0)
                        || mod_type.default_attributes.get(&183) == Some(&11395.0);
                    let deep_core_skill = ctx.db.with_snapshot(|snap| {
                        snap.skills.get(&char_id).and_then(|m| m.get(&11395)).map(|s| s.level).unwrap_or(0)
                    });
                    if !is_deep_core || deep_core_skill == 0 {
                        return Some(Err(BridgeRpcError {
                            code: "MercoxitRequiresDeepCore".into(),
                            message: "Mercoxit requires a Deep Core mining laser or strip miner and the Deep Core Mining skill".into(),
                        }));
                    }
                }

                let mut in_warp = false;
                let is_cloaked = false;
                let mut ship_pos = DVec3::ZERO;
                let mut sys_id_val = session.solar_system_id.unwrap_or(asteroid.solar_system_id);
                if let Some(sys_id) = session.solar_system_id {
                    sys_id_val = sys_id;
                    let sys_handle = ctx.systems.get_or_spawn(sys_id);
                    if let Ok(Some((_, ship_ball))) = sys_handle.get_ball_snapshot(ship_id).await {
                        ship_pos = ship_ball.position;
                        if matches!(ship_ball.mode, eve_destiny::MovementMode::Warp { .. }) {
                            in_warp = true;
                        }
                    }
                }
                if in_warp {
                    return Some(Err(BridgeRpcError {
                        code: "CannotActivateWhileWarping".into(),
                        message: "Cannot activate mining lasers while in warp".into(),
                    }));
                }
                if is_cloaked {
                    return Some(Err(BridgeRpcError {
                        code: "CannotActivateWhileCloaked".into(),
                        message: "Cannot activate mining lasers while cloaked".into(),
                    }));
                }
                let field_range_mult = ctx.db.with_snapshot(|snap| {
                    let mut candidate_chars = vec![char_id];
                    for fl in snap.fleets.values() {
                        if fl.members.contains(&char_id) {
                            for &m_id in &fl.members {
                                if !candidate_chars.contains(&m_id) {
                                    candidate_chars.push(m_id);
                                }
                            }
                        }
                    }
                    for cand_id in candidate_chars {
                        let Some(ch) = snap.characters.get(&cand_id) else { continue };
                        if ch.solar_system_id != sys_id_val || ch.station_id.is_some() { continue }
                        let booster_ship_id = ch.active_ship_id;
                        for it in snap.items.values() {
                            if it.location_id != booster_ship_id || !(27..=34).contains(&(it.flag as u16)) { continue }
                            let is_burst = ctx.sde.types.get(&it.type_id).is_some_and(|t| t.group_id == 1770);
                            if !is_burst || !snap.module_active_states.contains(&it.item_id) { continue }
                            let charge_tid = snap.module_active_charges.get(&it.item_id).copied().or_else(|| {
                                snap.items.values().find(|ch_it| {
                                    ch_it.location_id == booster_ship_id
                                        && (ch_it.flag == it.flag || ch_it.flag == InventoryFlag::Cargo || ch_it.flag == InventoryFlag::SpecializedAmmoHold)
                                        && ch_it.quantity > 0
                                        && ctx.sde.types.get(&ch_it.type_id).is_some_and(|t| t.group_id == 1771)
                                }).map(|ch_it| ch_it.type_id)
                            });
                            if charge_tid == Some(42829) {
                                let md_lvl = snap.skills.get(&cand_id).and_then(|m| m.get(&24613)).map(|s| s.level).unwrap_or(0);
                                let t2 = if it.type_id == 42890 || ctx.sde.types.get(&it.type_id).is_some_and(|t| t.name.contains(" II")) { 1.25 } else { 1.0 };
                                return 1.0 + 0.30 * (1.0 + 0.10 * (md_lvl as f64)) * t2;
                            }
                        }
                    }
                    1.0
                });
                let max_range = mod_type.default_attributes.get(&54).copied().unwrap_or(10_000.0) * field_range_mult;
                let ast_pos = DVec3::new(asteroid.position.0, asteroid.position.1, asteroid.position.2);
                let distance = if ship_pos != DVec3::ZERO {
                    (ship_pos - ast_pos).length()
                } else {
                    0.0
                };
                if distance > max_range && ship_pos != DVec3::ZERO {
                    return Some(Err(BridgeRpcError {
                        code: "TargetOutOfRange".into(),
                        message: format!("Target asteroid is {distance:.1}m away (max range {max_range:.1}m)"),
                    }));
                }

                let cap_need = mod_type.default_attributes.get(&6).copied().unwrap_or(0.0);
                let mut fit = match ctx.get_or_rebuild_ship_fitting(char_id, ship_id) {
                    Ok(f) => f,
                    Err(e) => return Some(Err(e)),
                };
                let cap_capacity = fit.ship.get_attribute(eve_sde::attr_ids::CAPACITOR_CAPACITY, &ctx.sde);
                if cap_need > cap_capacity {
                    return Some(Err(BridgeRpcError {
                        code: "InsufficientCapacitor".into(),
                        message: format!("Module requires {cap_need} GJ capacitor, ship capacity is {cap_capacity} GJ"),
                    }));
                }

                let base_duration_ms = mod_type.default_attributes.get(&73).copied().unwrap_or(15_000.0);
                let base_amount = mod_type.default_attributes.get(&77).copied().unwrap_or(10.0);

                let (
                    mining_lvl,
                    astro_lvl,
                    ice_lvl,
                    mining_frig_lvl,
                    expedition_frig_lvl,
                    barge_lvl,
                    exhumer_lvl,
                    mining_destroyer_lvl,
                    spec_lvl,
                    implant_mult,
                ) = ctx.db.with_snapshot(|snap| {
                    let sk = snap.skills.get(&char_id);
                    let m = sk.and_then(|m| m.get(&3386)).map(|s| s.level).unwrap_or(0);
                    let a = sk.and_then(|m| m.get(&3410)).map(|s| s.level).unwrap_or(0);
                    let i = sk.and_then(|m| m.get(&16281)).map(|s| s.level).unwrap_or(0);
                    let mf = sk.and_then(|m| m.get(&32918)).map(|s| s.level).unwrap_or(0);
                    let ef = sk.and_then(|m| m.get(&33856)).map(|s| s.level).unwrap_or(0);
                    let b = sk.and_then(|m| m.get(&17940)).map(|s| s.level).unwrap_or(0);
                    let e = sk.and_then(|m| m.get(&22551)).map(|s| s.level).unwrap_or(0);
                    let md = sk.and_then(|m| m.get(&89238)).map(|s| s.level).unwrap_or(0);
                    let mut sp = 0u8;
                    for sid in [60377_u32, 60378, 60379, 60380, 60381, 46152, 46153, 46154, 46155, 12195, 12196] {
                        let lvl = sk.and_then(|m| m.get(&sid)).map(|s| s.level).unwrap_or(0);
                        sp = sp.max(lvl);
                    }
                    let mut imp_mult = 1.0;
                    if let Some(imps) = snap.active_implants.get(&char_id) {
                        for &imp_tid in imps {
                            if let Some(imp_type) = ctx.sde.types.get(&imp_tid) {
                                if let Some(&bonus) = imp_type.default_attributes.get(&434) {
                                    imp_mult *= 1.0 + bonus / 100.0;
                                }
                            }
                        }
                    }
                    (m, a, i, mf, ef, b, e, md, sp, imp_mult)
                });

                let ship_item_res = ctx.db.get_item(ship_id);
                let ship_type = ship_item_res.as_ref().and_then(|si| ctx.sde.types.get(&si.type_id));
                let mut ship_mining_mult = 1.0;
                let mut ship_gas_mult = 1.0;
                let mut ship_ice_duration_mult = 1.0;
                let mut ship_laser_duration_mult = 1.0;
                let mut ship_gas_duration_mult = 1.0;
                let mut ship_crit_chance_mult = 1.0;
                if let Some(st) = ship_type {
                    // Role bonuses from SDE ship dogma attributes
                    if let Some(&role_ore_pct) = st.default_attributes.get(&3177) {
                        ship_mining_mult *= 1.0 + role_ore_pct / 100.0;
                    } else if let Some(&role_ore_mult) = st.default_attributes.get(&207) {
                        if role_ore_mult > 0.0 {
                            ship_mining_mult *= role_ore_mult;
                        }
                    }
                    if let Some(&destroyer_role_pct) = st.default_attributes.get(&5986) {
                        ship_mining_mult *= 1.0 + destroyer_role_pct / 100.0;
                    }
                    if let Some(&legacy_ore_pct) = st.default_attributes.get(&1178) {
                        ship_mining_mult *= 1.0 + legacy_ore_pct / 100.0;
                    }
                    if let Some(&role_gas_pct) = st.default_attributes.get(&3239) {
                        ship_gas_mult *= 1.0 + role_gas_pct / 100.0;
                    } else if let Some(&legacy_gas_pct) = st.default_attributes.get(&1200) {
                        ship_gas_mult *= 1.0 + legacy_gas_pct / 100.0;
                    }
                    if let Some(&role_ore_dur_pct) = st.default_attributes.get(&3230) {
                        ship_laser_duration_mult *= (1.0 + role_ore_dur_pct / 100.0).max(0.1);
                    }
                    if let Some(&role_ice_dur_pct) = st.default_attributes.get(&3178) {
                        ship_ice_duration_mult *= (1.0 + role_ice_dur_pct / 100.0).max(0.1);
                    }
                    if let Some(&role_gas_dur_pct) = st.default_attributes.get(&3225) {
                        ship_gas_duration_mult *= (1.0 + role_gas_dur_pct / 100.0).max(0.1);
                    }
                    if let Some(&destroyer_gas_dur_pct) = st.default_attributes.get(&5952) {
                        ship_gas_duration_mult *= (1.0 + destroyer_gas_dur_pct / 100.0).max(0.1);
                    }
                    if let Some(&role_crit_pct) = st.default_attributes.get(&6048) {
                        ship_crit_chance_mult *= 1.0 + role_crit_pct / 100.0;
                    }
                    if let Some(&ice_crit_pct) = st.default_attributes.get(&6062) {
                        ship_crit_chance_mult *= 1.0 + ice_crit_pct / 100.0;
                    }

                    // Per-skill-level bonuses from SDE ship dogma attributes
                    if let Some(&frig_ore_pct) = st.default_attributes.get(&1842) {
                        ship_mining_mult *= 1.0 + (frig_ore_pct / 100.0) * (mining_frig_lvl as f64);
                    }
                    if let Some(&frig_gas_dur_pct) = st.default_attributes.get(&3237) {
                        ship_gas_duration_mult *=
                            (1.0 + (frig_gas_dur_pct / 100.0) * (mining_frig_lvl as f64)).max(0.1);
                    }
                    if let Some(&frig_ice_dur_pct) = st.default_attributes.get(&3240) {
                        ship_ice_duration_mult *=
                            (1.0 + (frig_ice_dur_pct / 100.0) * (mining_frig_lvl as f64)).max(0.1);
                    }
                    if let Some(&exp_ore_pct) = st.default_attributes.get(&3191) {
                        ship_mining_mult *=
                            1.0 + (exp_ore_pct / 100.0) * (expedition_frig_lvl as f64);
                    }
                    if let Some(&exp_ice_dur_pct) = st.default_attributes.get(&3167) {
                        ship_ice_duration_mult *=
                            (1.0 + (exp_ice_dur_pct / 100.0) * (expedition_frig_lvl as f64))
                                .max(0.1);
                    }
                    if let Some(&barge_ore_pct) = st.default_attributes.get(&3181) {
                        ship_mining_mult *= 1.0 + (barge_ore_pct / 100.0) * (barge_lvl as f64);
                    }
                    if let Some(&barge_ice_dur_pct) = st.default_attributes.get(&3182) {
                        ship_ice_duration_mult *=
                            (1.0 + (barge_ice_dur_pct / 100.0) * (barge_lvl as f64)).max(0.1);
                    }
                    if let Some(&barge_gas_dur_pct) = st.default_attributes.get(&3183) {
                        ship_gas_duration_mult *=
                            (1.0 + (barge_gas_dur_pct / 100.0) * (barge_lvl as f64)).max(0.1);
                    }
                    if let Some(&exh_ore_pct) = st.default_attributes.get(&3197) {
                        ship_mining_mult *= 1.0 + (exh_ore_pct / 100.0) * (exhumer_lvl as f64);
                    }
                    if let Some(&exh_ore_dur_pct) = st.default_attributes.get(&3193) {
                        ship_laser_duration_mult *=
                            (1.0 + (exh_ore_dur_pct / 100.0) * (exhumer_lvl as f64)).max(0.1);
                    }
                    if let Some(&exh_ice_dur_pct) = st.default_attributes.get(&3194) {
                        ship_ice_duration_mult *=
                            (1.0 + (exh_ice_dur_pct / 100.0) * (exhumer_lvl as f64)).max(0.1);
                    }
                    if let Some(&exh_gas_dur_pct) = st.default_attributes.get(&3226) {
                        ship_gas_duration_mult *=
                            (1.0 + (exh_gas_dur_pct / 100.0) * (exhumer_lvl as f64)).max(0.1);
                    }
                    if let Some(&dest_ore_pct) = st.default_attributes.get(&5820) {
                        ship_mining_mult *=
                            1.0 + (dest_ore_pct / 100.0) * (mining_destroyer_lvl as f64);
                    }
                }

                // Mining Laser Upgrades (`group_id == 546`, e.g., `22542` MLU I, `28576` MLU II) with EVE Dogma stacking penalty (`feat:mining.mining-upgrades`)
                let mut mlu_bonuses: Vec<f64> = Vec::new();
                let mut ice_cycle_bonus = 0.0;
                for item in &items {
                    if item.location_id == ship_id && (item.flag as u16) >= 11 && (item.flag as u16) <= 34 {
                        let is_online = fit
                            .modules
                            .get(&item.item_id)
                            .map(|m| m.state != eve_dogma::ModuleState::Offline)
                            .unwrap_or(true);
                        if !is_online {
                            continue;
                        }
                        if let Some(it) = ctx.sde.types.get(&item.type_id) {
                            if it.group_id == 546 {
                                if let Some(&b) = it.default_attributes.get(&434) {
                                    mlu_bonuses.push(b / 100.0);
                                }
                                if let Some(&b) = it.default_attributes.get(&780) {
                                    ice_cycle_bonus += b / 100.0;
                                }
                            } else if item.type_id == 22291 {
                                mlu_bonuses.push(0.09);
                            }
                        }
                    }
                }
                mlu_bonuses.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
                let mut mlu_mult = 1.0;
                for (i, &b) in mlu_bonuses.iter().enumerate() {
                    mlu_mult *= 1.0 + b * eve_dogma::stacking_penalty_factor(i);
                }

                let crystal_item_opt = items.iter().find(|i| {
                    i.location_id == ship_id
                        && i.flag == mod_item.flag
                        && ctx.sde.types.get(&i.type_id).map(|t| t.group_id == 482 || t.group_id == 663).unwrap_or(false)
                });
                let mut crystal_yield_mult = 1.0;
                let mut crystal_base_mult = 1.0;
                let mut crystal_matched = false;
                let mut crystal_damage_inflicted = 0.0;
                let mut crystal_destroyed = false;
                let mut crystal_id_to_update = None;
                let mut crystal_new_hp = 1.0;
                let mut crystal_waste_prob_bonus = 0.0;
                let mut crystal_waste_mult_bonus = 0.0;
                let mut crystal_duration_mult = 1.0;

                if let Some(crystal) = crystal_item_opt {
                    if let Some(c_type) = ctx.sde.types.get(&crystal.type_id) {
                        let ore_type_entry = ctx.sde.types.get(&asteroid.type_id);
                        let ore_name = ore_type_entry.map(|t| t.name.to_lowercase()).unwrap_or_default();
                        let crystal_name = c_type.name.to_lowercase();
                        let crystal_matches = if c_type.name.contains("Asteroid") || c_type.name.contains("Moon") {
                            if let Some(&list_id_f) = c_type.default_attributes.get(&3148) {
                                let list_id = list_id_f as u32;
                                ctx.sde.type_lists.get(&list_id).map(|tl| tl.included_type_ids.contains(&asteroid.type_id)).unwrap_or(false)
                            } else {
                                false
                            }
                        } else if let Some(&list_id_f) = c_type.default_attributes.get(&3148) {
                            let list_id = list_id_f as u32;
                            let in_list = ctx.sde.type_lists.get(&list_id).map(|tl| tl.included_type_ids.contains(&asteroid.type_id)).unwrap_or(false);
                            let ore_base = ore_name.split_whitespace().last().unwrap_or(&ore_name);
                            in_list && (crystal_name.contains(ore_base) || crystal_name.contains(&ore_name))
                        } else {
                            let ore_base = ore_name.split_whitespace().last().unwrap_or(&ore_name);
                            crystal_name.contains(ore_base) || crystal_name.contains(&ore_name)
                        };
                        crystal_matched = crystal_matches;
                        if crystal_matches {
                            let mult = c_type.default_attributes.get(&782).copied().unwrap_or(1.5);
                            crystal_base_mult = mult;
                            crystal_yield_mult = mult * (1.0 + 0.02 * (spec_lvl as f64));
                        } else {
                            crystal_base_mult = 1.0;
                            crystal_yield_mult = 1.0;
                        }
                        crystal_waste_prob_bonus = c_type
                            .default_attributes
                            .get(&3160)
                            .copied()
                            .or_else(|| c_type.default_attributes.get(&3154).copied())
                            .unwrap_or(0.0);
                        crystal_waste_mult_bonus = c_type
                            .default_attributes
                            .get(&3159)
                            .copied()
                            .or_else(|| c_type.default_attributes.get(&3153).copied())
                            .unwrap_or(0.0);
                        crystal_duration_mult = c_type
                            .default_attributes
                            .get(&3161)
                            .copied()
                            .unwrap_or(1.0);
                        let vol_damage = c_type.default_attributes.get(&784).copied().unwrap_or(0.025);
                        let current_hp = crystal.dynamic_attributes.get(&eve_sde::attr_ids::HP_STRUCT).copied().unwrap_or(1.0);
                        crystal_damage_inflicted = vol_damage;
                        let rem_hp = (current_hp - vol_damage).max(0.0);
                        crystal_new_hp = rem_hp;
                        crystal_id_to_update = Some(crystal.item_id);
                        if rem_hp <= 1e-6 {
                            crystal_destroyed = true;
                        }
                    }
                }

                // Evaluate active Fleet Mining Foreman Burst (`group_id == 1770`, charge `group_id == 1771`) in same solar system (`feat:mining.foreman-burst-charge-effects`)
                let (fleet_duration_mult, fleet_waste_mult, fleet_crit_bonus) = ctx.db.with_snapshot(|snap| {
                    let mut candidate_chars = vec![char_id];
                    for fl in snap.fleets.values() {
                        if fl.members.contains(&char_id) {
                            for &m_id in &fl.members {
                                if !candidate_chars.contains(&m_id) {
                                    candidate_chars.push(m_id);
                                }
                            }
                        }
                    }
                    let mut dur_mult: f64 = 1.0;
                    let mut waste_mult: f64 = 1.0;
                    let mut crit_bonus: f64 = 0.0;
                    for cand_id in candidate_chars {
                        let Some(ch) = snap.characters.get(&cand_id) else {
                            continue;
                        };
                        if ch.solar_system_id != sys_id_val || ch.station_id.is_some() {
                            continue;
                        }
                        let booster_ship_id = ch.active_ship_id;
                        for it in snap.items.values() {
                            if it.location_id != booster_ship_id || !(27..=34).contains(&(it.flag as u16)) {
                                continue;
                            }
                            let is_burst = ctx.sde.types.get(&it.type_id).is_some_and(|t| t.group_id == 1770);
                            if !is_burst || !snap.module_active_states.contains(&it.item_id) {
                                continue;
                            }
                            let charge_tid = snap.module_active_charges.get(&it.item_id).copied().or_else(|| {
                                snap.items.values().find(|ch_it| {
                                    ch_it.location_id == booster_ship_id
                                        && (ch_it.flag == it.flag || ch_it.flag == InventoryFlag::Cargo || ch_it.flag == InventoryFlag::SpecializedAmmoHold)
                                        && ch_it.quantity > 0
                                        && ctx.sde.types.get(&ch_it.type_id).is_some_and(|t| t.group_id == 1771)
                                }).map(|ch_it| ch_it.type_id)
                            }).unwrap_or(42830);

                            let md_lvl = snap.skills.get(&cand_id).and_then(|m| m.get(&24613)).map(|s| s.level).unwrap_or(0);
                            let t2 = if it.type_id == 42890 || ctx.sde.types.get(&it.type_id).is_some_and(|t| t.name.contains(" II")) { 1.25 } else { 1.0 };
                            let strength = (1.0 + 0.10 * (md_lvl as f64)) * t2;

                            match charge_tid {
                                42830 => {
                                    // Mining Laser Optimization: base -15% duration
                                    dur_mult = dur_mult.min((1.0 - 0.15 * strength).max(0.50));
                                }
                                90733 => {
                                    // Mining Laser Efficiency: lower residue, higher crit chance
                                    waste_mult = waste_mult.min(0.70);
                                    crit_bonus += 0.05 * strength;
                                }
                                _ => {}
                            }
                        }
                    }
                    (dur_mult, waste_mult, crit_bonus)
                });
                let fleet_boost_active = fleet_duration_mult < 1.0 || fleet_waste_mult < 1.0 || fleet_crit_bonus > 0.0;

                // Probabilistic mining critical hit check against SDE `miningCritChance` (`5967`) (`feat:mining.mining-crits`)
                let base_crit_chance = mod_item
                    .dynamic_attributes
                    .get(&5967)
                    .copied()
                    .or_else(|| mod_type.default_attributes.get(&5967).copied())
                    .unwrap_or(0.01)
                    + fleet_crit_bonus;
                let crit_chance = (base_crit_chance * ship_crit_chance_mult).clamp(0.0, 1.0);
                let crit_bonus = mod_item
                    .dynamic_attributes
                    .get(&5969)
                    .copied()
                    .or_else(|| mod_type.default_attributes.get(&5969).copied())
                    .unwrap_or(2.0);
                let cycle_count = {
                    let mut wcs = ctx
                        .weapon_cycle_state
                        .write()
                        .unwrap_or_else(|e| e.into_inner());
                    let entry = wcs.entry(module_id).or_insert((None, 0));
                    entry.1 = entry.1.saturating_add(1);
                    entry.1
                };
                let is_crit = crit_chance > 0.0 && csprng_unit_f64(ctx) < crit_chance;
                let crit_yield_mult = if is_crit { crit_bonus } else { 1.0 };

                let is_ice = mod_type.group_id == 464 && (mod_type.type_id == 16278 || mod_type.type_id == 22229 || mod_type.name.contains("Ice"));
                let is_gas = mod_type.group_id == 737 || mod_type.group_id == 4138;
                let (duration_ms, base_volume_yield) = if is_ice {
                    let duration = (base_duration_ms * (1.0 - 0.05 * (ice_lvl as f64) + ice_cycle_bonus) * ship_ice_duration_mult * fleet_duration_mult).max(1000.0);
                    (duration, base_amount)
                } else if is_gas {
                    (base_duration_ms * ship_gas_duration_mult * fleet_duration_mult, base_amount * ship_gas_mult)
                } else {
                    let skill_mult = (1.0 + 0.05 * (mining_lvl as f64)) * (1.0 + 0.05 * (astro_lvl as f64));
                    let vol = base_amount * skill_mult * implant_mult * ship_mining_mult * mlu_mult * crystal_yield_mult;
                    (base_duration_ms * ship_laser_duration_mult * crystal_duration_mult * fleet_duration_mult, vol)
                };

                let ore_unit_volume = ctx.sde.types.get(&asteroid.type_id).map(|t| t.volume).unwrap_or(0.1).max(0.001);
                let potential_units = ((base_volume_yield / ore_unit_volume).floor() as i64).max(1);

                let (actual_units, cycle_time_ms, depleted) = if asteroid.quantity < potential_units {
                    let actual = asteroid.quantity;
                    let partial_cycle = ((actual as f64) / (potential_units as f64) * duration_ms).round() as i64;
                    (actual, partial_cycle.max(1000), true)
                } else {
                    (potential_units, duration_ms as i64, asteroid.quantity == potential_units)
                };

                let bonus_crit_units = if is_crit {
                    ((actual_units as f64) * (crit_bonus - 1.0)).round().max(1.0) as i64
                } else {
                    0
                };

                let raw_waste_prob = mod_type
                    .default_attributes
                    .get(&3154)
                    .copied()
                    .unwrap_or(0.0)
                    + crystal_waste_prob_bonus;
                let norm_waste_prob = if raw_waste_prob > 1.0 {
                    raw_waste_prob / 100.0
                } else {
                    raw_waste_prob
                };
                let waste_prob = (norm_waste_prob * fleet_waste_mult).clamp(0.0, 1.0);
                let waste_mult = (mod_type
                    .default_attributes
                    .get(&3153)
                    .copied()
                    .unwrap_or(1.0)
                    + crystal_waste_mult_bonus)
                    .max(0.0);
                let waste_roll =
                    (((cycle_count.saturating_sub(1) * 37 + 10) % 100) as f64) / 100.0;
                let residue_units = if waste_prob > 0.0 && waste_roll < waste_prob {
                    ((actual_units as f64) * waste_mult).floor() as i64
                } else {
                    0
                };

                let (destination_flag, hold_capacity) = if is_ice {
                    let ice_cap = ship_type.and_then(|st| st.default_attributes.get(&3136).copied().or_else(|| st.default_attributes.get(&1556).copied())).unwrap_or(0.0);
                    if ice_cap > 0.0 {
                        (InventoryFlag::SpecializedIceHold, ice_cap)
                    } else {
                        (InventoryFlag::Cargo, ship_type.map(|st| st.capacity).unwrap_or(500.0))
                    }
                } else if is_gas {
                    let gas_cap = ship_type.and_then(|st| st.default_attributes.get(&1557).copied().or_else(|| st.default_attributes.get(&1556).copied())).unwrap_or(0.0);
                    if gas_cap > 0.0 {
                        (InventoryFlag::SpecializedGasHold, gas_cap)
                    } else {
                        (InventoryFlag::Cargo, ship_type.map(|st| st.capacity).unwrap_or(500.0))
                    }
                } else {
                    let ore_cap = ship_type.and_then(|st| st.default_attributes.get(&1556).copied().or_else(|| st.default_attributes.get(&1770).copied())).unwrap_or(0.0);
                    if ore_cap > 0.0 {
                        (InventoryFlag::SpecializedOreHold, ore_cap)
                    } else {
                        (InventoryFlag::Cargo, ship_type.map(|st| st.capacity).unwrap_or(500.0))
                    }
                };

                let used_volume: f64 = items.iter()
                    .filter(|i| i.location_id == ship_id && i.flag == destination_flag)
                    .map(|i| ctx.sde.types.get(&i.type_id).map(|t| t.volume).unwrap_or(0.0) * (i.quantity as f64))
                    .sum();
                let remaining_vol = (hold_capacity - used_volume).max(0.0);
                let max_fit_units = (remaining_vol / ore_unit_volume).floor() as i64;

                let total_cycle_units = actual_units + bonus_crit_units;
                let (rock_units_extracted, units_to_deposit, hold_full) = if max_fit_units < total_cycle_units {
                    let fit_capped = max_fit_units.max(0);
                    (fit_capped.min(actual_units), fit_capped, true)
                } else {
                    (actual_units, total_cycle_units, false)
                };

                let mut deposited_units_out = 0;
                let mut rock_depleted_out = depleted;
                let res: Result<(), eve_db::DbError> = ctx.db.try_with_mut_snapshot(|snap| {
                    if snap.items.contains_key(&module_id) {
                        snap.module_active_states.insert(module_id);
                    }
                    let total_removed_from_rock = rock_units_extracted + residue_units;
                    if let Some(ast) = snap.asteroids.get_mut(&target_id) {
                        ast.quantity = ast.quantity.saturating_sub(total_removed_from_rock);
                        rock_depleted_out = ast.quantity == 0;
                    }
                    if rock_depleted_out {
                        let mut newly_cleared_ids = Vec::new();
                        if let Some(anoms) = snap.cosmic_anomalies.get_mut(&sys_id_val) {
                            for anom in anoms.iter_mut() {
                                if !anom.cleared {
                                    let anom_pos = DVec3::from_array(anom.position);
                                    if anom_pos.distance(ast_pos) <= 50_000.0 {
                                        let any_remaining = snap.asteroids.values().any(|a| {
                                            a.solar_system_id == sys_id_val
                                                && a.quantity > 0
                                                && DVec3::new(
                                                    a.position.0,
                                                    a.position.1,
                                                    a.position.2,
                                                )
                                                .distance(anom_pos)
                                                    <= 50_000.0
                                        });
                                        if !any_remaining {
                                            anom.cleared = true;
                                            anom.remaining_volume = 0.0;
                                            newly_cleared_ids.push(anom.anomaly_id.clone());
                                        }
                                    }
                                }
                            }
                        }
                        if !newly_cleared_ids.is_empty() {
                            if let Some(sigs) = snap.cosmic_signatures.get_mut(&sys_id_val) {
                                for s in sigs.iter_mut() {
                                    if newly_cleared_ids.contains(&s.signature_id) {
                                        s.collapsed = true;
                                        s.signal_strength = 0.0;
                                    }
                                }
                            }
                        }
                    }
                    if units_to_deposit > 0 {
                        deposited_units_out = units_to_deposit;
                        let existing_opt = snap.items.values_mut().find(|i| {
                            i.location_id == ship_id
                                && i.flag == destination_flag
                                && i.type_id == asteroid.type_id
                                && !i.singleton
                                && !i.is_blueprint_copy
                        });
                        if let Some(existing) = existing_opt {
                            existing.quantity += units_to_deposit as i32;
                            existing.version += 1;
                        } else {
                            snap.next_entity_id += 1;
                            let new_id = snap.next_entity_id;
                            snap.items.insert(
                                new_id,
                                eve_db::InventoryItemRecord {
                                    item_id: new_id,
                                    type_id: asteroid.type_id,
                                    owner_id: char_id,
                                    location_id: ship_id,
                                    flag: destination_flag,
                                    quantity: units_to_deposit as i32,
                                    singleton: false,
                                    version: 1,
                                    dynamic_attributes: HashMap::new(),
                                    source_mutaplasmid_id: None,
                                    insured_payout_isk: 0.0,
                                    is_blueprint_copy: false,
                                    blueprint_runs_remaining: None,
                                },
                            );
                        }
                    }
                    if let Some(cid) = crystal_id_to_update {
                        if crystal_destroyed {
                            snap.items.remove(&cid);
                        } else if let Some(c_item) = snap.items.get_mut(&cid) {
                            c_item.dynamic_attributes.insert(eve_sde::attr_ids::HP_STRUCT, crystal_new_hp);
                            c_item.version += 1;
                        }
                    }
                    Ok(())
                });
                if let Err(e) = res {
                    return Some(Err(BridgeRpcError { code: "MiningFailed".into(), message: format!("{e:?}") }));
                }

                if deposited_units_out > 0 {
                    let _ = ctx.db.record_mining_ledger_entry(
                        char_id,
                        sys_id_val,
                        asteroid.type_id,
                        deposited_units_out,
                        ctx.now_unix_secs(),
                    );

                    // Guarded gas sites (wormhole reservoirs and guarded nebulae) spawn defenders; no guarded gas
                    // site is modelled yet (see `mining_sites::GAS_SITES`), so harvesting spawns no NPCs.
                }

                if rock_depleted_out {
                    let sys_handle = ctx.systems.get_or_spawn(sys_id_val);
                    sys_handle.try_remove_ball(target_id);
                }

                ctx.notify(
                    crate::router::NotificationTarget::Character(char_id),
                    "OnOreMined",
                    "character",
                    vec![
                        PyRep::Integer(asteroid.type_id as i64),
                        PyRep::Integer(deposited_units_out),
                        PyRep::Integer(destination_flag as i64),
                    ],
                );
                // `OnMiningLaserFiredLocal` is scattered by the V24 client itself
                // (`moonmining.schedulingSvc.SchedulingSvc.FireLaser`); never sent by the server.

                let _ = fit.activate_module(module_id, method == "Overload", &ctx.sde);
                let cpu_load = fit.ship.get_attribute(eve_sde::attr_ids::CPU_LOAD, &ctx.sde);
                ctx.store_ship_fitting(ship_id, fit);

                Some(Ok(PyRep::keyval([
                    ("moduleID", PyRep::Integer(module_id)),
                    ("targetID", PyRep::Integer(target_id)),
                    ("cycleTimeMs", PyRep::Integer(cycle_time_ms)),
                    ("yieldUnits", PyRep::Integer(deposited_units_out)),
                    ("normalUnits", PyRep::Integer(rock_units_extracted)),
                    ("bonusCritUnits", PyRep::Integer(bonus_crit_units)),
                    ("yieldVolume", PyRep::Real(deposited_units_out as f64 * ore_unit_volume)),
                    ("oreTypeID", PyRep::Integer(asteroid.type_id as i64)),
                    ("destinationFlag", PyRep::Integer(destination_flag as i64)),
                    ("flagID", PyRep::Integer(destination_flag as i64)),
                    ("depleted", PyRep::Bool(rock_depleted_out)),
                    ("holdFull", PyRep::Bool(hold_full)),
                    ("residueUnits", PyRep::Integer(residue_units)),
                    ("crystalMatched", PyRep::Bool(crystal_matched)),
                    ("crystalMultiplier", PyRep::Real(crystal_base_mult)),
                    ("crystalDamage", PyRep::Real(crystal_damage_inflicted)),
                    ("crystalDestroyed", PyRep::Bool(crystal_destroyed)),
                    ("implantYieldMultiplier", PyRep::Real(implant_mult)),
                    ("isCrit", PyRep::Bool(is_crit)),
                    ("critYieldMultiplier", PyRep::Real(crit_yield_mult)),
                    ("fleetBoostActive", PyRep::Bool(fleet_boost_active)),
                    ("mluCount", PyRep::Integer(mlu_bonuses.len() as i64)),
                    ("mluMultiplier", PyRep::Real(mlu_mult)),
                    ("cpuLoad", PyRep::Real(cpu_load)),
                ])))
}

pub(crate) async fn handle_mining_and_deactivate(
    ctx: &ServerContext,
    session: &mut SessionState,
    _bound_entry: Option<&BoundObjectEntry>,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("dogmaIM", "Deactivate") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let module_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let effect_name = args.get(1).and_then(|a| a.as_str()).unwrap_or("defaultEffect").to_string();
            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "No active ship in session".into(),
                }));
            };
            if let Some(sys_id) = session.solar_system_id {
                let sys = ctx.systems.get_or_spawn(sys_id);
                let _ = sys.remove_module_cycle(ship_id, module_id).await;
            }
            ctx.remove_projected_ewar_by_module(module_id);

            ctx.notify(
                crate::router::NotificationTarget::Character(char_id),
                "OnModulesDeactivating",
                "charid",
                vec![PyRep::List(vec![PyRep::Integer(module_id)])],
            );
            ctx.notify(
                crate::router::NotificationTarget::Character(char_id),
                "OnGodmaShipEffect",
                "charid",
                vec![PyRep::Integer(module_id), PyRep::String(effect_name), PyRep::Bool(false)],
            );

            ctx.db.with_mut_snapshot(|snap| {
                snap.module_active_states.remove(&module_id);
                snap.module_active_charges.remove(&module_id);
            });

            let items = ctx.db.get_items_for_character(char_id);
            let mod_item_opt = items.iter().find(|i| i.item_id == module_id && i.owner_id == char_id);
            let is_mining = mod_item_opt.and_then(|m| ctx.sde.types.get(&m.type_id))
                .map(|t| matches!(t.group_id, 54 | 464 | 483 | 737 | 2004 | 4138) || t.default_attributes.contains_key(&77))
                .unwrap_or(false);

            let mut fit = match ctx.get_or_rebuild_ship_fitting(char_id, ship_id) {
                Ok(f) => f,
                Err(e) => return Some(Err(e)),
            };
            let deact_res = fit.deactivate_module(module_id, &ctx.sde);
            if let Err(e) = deact_res {
                if !is_mining {
                    return Some(Err(BridgeRpcError { code: "DogmaDeactivateFailed".into(), message: e.to_string() }));
                }
            }
            if let Some(m_entry) = fit.modules.get(&module_id) {
                let mod_state = fit.build_effective_module_state(module_id, m_entry.type_id, false, &ctx.sde);
                let react_delay_ms = mod_state.get_attribute(669, &ctx.sde);
                if react_delay_ms > 0.0 {
                    ctx.record_module_reactivation_delay(ship_id, module_id, react_delay_ms / 1000.0);
                }
            }
            let max_vel = fit.ship.get_attribute(attr_ids::MAX_VELOCITY, &ctx.sde);
            ctx.store_ship_fitting(ship_id, fit);

            if mod_item_opt
                .and_then(|m| ctx.sde.types.get(&m.type_id))
                .is_some_and(|t| t.group_id == 1815)
            {
                if let Some(sys_id) = session.solar_system_id {
                    let sys = ctx.systems.get_or_spawn(sys_id);
                    let balls = sys.list_balls().await.unwrap_or_default();
                    let dyn_ship_ids: Vec<i64> = balls
                        .iter()
                        .filter(|b| b.id < 700_000_000)
                        .map(|b| b.id)
                        .collect();
                    ctx.db.with_mut_snapshot(|snap| {
                        for &sid in &dyn_ship_ids {
                            if let Some(s_item) = snap.items.get_mut(&sid) {
                                for aid in [37u16, 55, 109, 110, 111, 113, 263, 265, 267, 268, 269, 270, 271, 272, 273, 274, 552] {
                                    s_item.dynamic_attributes.remove(&aid);
                                }
                            }
                        }
                    });
                }
            }

            if is_mining {
                Some(Ok(PyRep::keyval([
                    ("moduleID", PyRep::Integer(module_id)),
                    ("active", PyRep::Bool(false)),
                ])))
            } else {
                Some(Ok(PyRep::keyval([
                    ("moduleID", PyRep::Integer(module_id)),
                    ("overloaded", PyRep::Bool(false)),
                    ("effectiveMaxVelocity", PyRep::Real(max_vel)),
                ])))
            }
        }

        ("dogmaIM", "StopOverload") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let module_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "No active ship in session".into(),
                }));
            };
            if let Some(sys_id) = session.solar_system_id {
                let sys = ctx.systems.get_or_spawn(sys_id);
                let _ = sys.update_module_overload(ship_id, module_id, false).await;
            }
            let mut fit = match ctx.get_or_rebuild_ship_fitting(char_id, ship_id) {
                Ok(f) => f,
                Err(e) => return Some(Err(e)),
            };
            if let Some(fm) = fit.modules.get_mut(&module_id) {
                if fm.state == eve_dogma::ModuleState::Overloaded {
                    fm.state = eve_dogma::ModuleState::Active;
                }
            }
            fit.recalculate_stats(&ctx.sde);
            let max_vel = fit.ship.get_attribute(attr_ids::MAX_VELOCITY, &ctx.sde);
            ctx.store_ship_fitting(ship_id, fit);

            ctx.notify(
                crate::router::NotificationTarget::Character(char_id),
                "OnHeatRemoved",
                "charid",
                vec![PyRep::Integer(module_id)],
            );
            ctx.notify(
                crate::router::NotificationTarget::Character(char_id),
                "OnModuleAttributeChanges",
                "charid",
                vec![PyRep::Integer(module_id)],
            );

            Some(Ok(PyRep::keyval([
                ("moduleID", PyRep::Integer(module_id)),
                ("overloaded", PyRep::Bool(false)),
                ("effectiveMaxVelocity", PyRep::Real(max_vel)),
            ])))
        }

        ("dogmaIM", "OverloadRack") | ("dogmaIM", "StopOverloadRack") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "No active ship in session".into(),
                }));
            };
            let arg_val = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let mut fit = match ctx.get_or_rebuild_ship_fitting(char_id, ship_id) {
                Ok(f) => f,
                Err(e) => return Some(Err(e)),
            };
            let target_rack_idx: usize = if let Some(fm) = fit.modules.get(&arg_val) {
                match fm.flag_id {
                    27..=34 => 0,
                    19..=26 => 1,
                    _ => 2,
                }
            } else {
                match arg_val as u16 {
                    27..=34 | 0 => 0,
                    19..=26 | 1 => 1,
                    _ => 2,
                }
            };
            let overloaded = method == "OverloadRack";
            let mut affected_modules = Vec::new();
            for (&mid, fm) in fit.modules.iter_mut() {
                let r_idx = match fm.flag_id {
                    27..=34 => 0,
                    19..=26 => 1,
                    11..=18 => 2,
                    _ => continue,
                };
                if r_idx == target_rack_idx && fm.state != eve_dogma::ModuleState::Offline {
                    affected_modules.push(mid);
                    if overloaded && fm.state == eve_dogma::ModuleState::Active {
                        fm.state = eve_dogma::ModuleState::Overloaded;
                    } else if !overloaded && fm.state == eve_dogma::ModuleState::Overloaded {
                        fm.state = eve_dogma::ModuleState::Active;
                    }
                }
            }
            fit.recalculate_stats(&ctx.sde);
            ctx.store_ship_fitting(ship_id, fit);

            if let Some(sys_id) = session.solar_system_id {
                let sys = ctx.systems.get_or_spawn(sys_id);
                for &mid in &affected_modules {
                    let _ = sys.update_module_overload(ship_id, mid, overloaded).await;
                }
            }
            let notif_name = if overloaded { "OnHeatAdded" } else { "OnHeatRemoved" };
            for &mid in &affected_modules {
                ctx.notify(
                    crate::router::NotificationTarget::Character(char_id),
                    notif_name,
                    "charid",
                    vec![PyRep::Integer(mid)],
                );
            }

            Some(Ok(PyRep::keyval([
                ("rackIndex", PyRep::Integer(target_rack_idx as i64)),
                ("overloaded", PyRep::Bool(overloaded)),
                (
                    "moduleIDs",
                    PyRep::List(affected_modules.into_iter().map(PyRep::Integer).collect()),
                ),
            ])))
        }

        ("dogmaIM", "RepairModule") | ("dogmaIM", "InitiateModuleRepair") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "No active ship in session".into(),
                }));
            };
            let module_id = args
                .get(1)
                .and_then(|a| a.as_i64())
                .or_else(|| {
                    args.first().and_then(|a| match a {
                        PyRep::List(v) | PyRep::Tuple(v) => v.first().and_then(|x| x.as_i64()),
                        other => other.as_i64(),
                    })
                })
                .unwrap_or(0);

            let (is_active, cur_dmg, max_hp, base_paste_cost, nanite_op_lvl, nanite_interfacing_lvl) =
                ctx.db.with_snapshot(|snap| {
                    let active = snap.module_active_states.contains(&module_id);
                    let m_opt = snap.items.get(&module_id);
                    let dmg = m_opt
                        .and_then(|i| i.dynamic_attributes.get(&attr_ids::DAMAGE).copied())
                        .unwrap_or(0.0);
                    let hp = m_opt
                        .and_then(|i| {
                            i.dynamic_attributes
                                .get(&attr_ids::HP_STRUCT)
                                .copied()
                                .or_else(|| {
                                    ctx.sde
                                        .types
                                        .get(&i.type_id)
                                        .and_then(|t| t.default_attributes.get(&attr_ids::HP_STRUCT).copied())
                                })
                        })
                        .unwrap_or(40.0);
                    let base_cost = m_opt
                        .and_then(|i| ctx.sde.types.get(&i.type_id))
                        .and_then(|t| t.default_attributes.get(&1231).copied())
                        .unwrap_or(4.0)
                        .max(1.0);
                    let sk = snap.skills.get(&char_id);
                    let op_lvl = sk.and_then(|m| m.get(&28879)).map(|r| r.level).unwrap_or(0);
                    let int_lvl = sk.and_then(|m| m.get(&28880)).map(|r| r.level).unwrap_or(0);
                    (active, dmg, hp, base_cost, op_lvl, int_lvl)
                });

            if is_active {
                return Some(Err(BridgeRpcError {
                    code: "CannotRepairActiveModule".into(),
                    message: "Module must be deactivated before initiating nanite repair".into(),
                }));
            }
            if cur_dmg >= max_hp - 1e-6 {
                return Some(Err(BridgeRpcError {
                    code: "ModuleBurnedOut".into(),
                    message: "Completely burned-out modules cannot be repaired with Nanite Repair Paste in space".into(),
                }));
            }
            if cur_dmg <= 1e-6 {
                return Some(Ok(PyRep::keyval([
                    ("moduleID", PyRep::Integer(module_id)),
                    ("pasteConsumed", PyRep::Integer(0)),
                    ("damageToRepair", PyRep::Real(0.0)),
                ])));
            }

            let effective_paste_per_hp =
                (base_paste_cost / max_hp.max(1.0)) * (1.0 - 0.05 * (nanite_op_lvl as f64));
            let paste_needed =
                eve_dogma::thermodynamics::nanite_paste_required(cur_dmg, effective_paste_per_hp)
                    .max(1) as i32;

            let deduct_res: Result<(), String> = ctx.db.try_with_mut_snapshot(|snap| {
                let paste_item_id = snap
                    .items
                    .values()
                    .find(|i| {
                        i.location_id == ship_id
                            && i.flag == InventoryFlag::Cargo
                            && i.type_id == 28668
                            && i.quantity >= paste_needed
                    })
                    .map(|i| i.item_id);
                let Some(pid) = paste_item_id else {
                    return Err(format!(
                        "Need {paste_needed} Nanite Repair Paste (type 28668) in ship cargo"
                    ));
                };
                let mut rem = 0;
                if let Some(p_item) = snap.items.get_mut(&pid) {
                    p_item.quantity -= paste_needed;
                    rem = p_item.quantity;
                }
                if rem == 0 {
                    snap.items.remove(&pid);
                }
                Ok(())
            });
            if let Err(msg) = deduct_res {
                return Some(Err(BridgeRpcError {
                    code: "NotEnoughNanitePaste".into(),
                    message: msg,
                }));
            }

            let repair_rate_hp_per_s = 0.67 * (1.0 + 0.20 * (nanite_interfacing_lvl as f64));
            let duration_s = (cur_dmg / repair_rate_hp_per_s).max(1.0);
            if let Some(sys_id) = session.solar_system_id {
                let sys = ctx.systems.get_or_spawn(sys_id);
                let _ = sys
                    .start_nanite_repair(crate::system_actor::ActiveNaniteRepair {
                        ship_id,
                        module_id,
                        initial_damage_hp: cur_dmg,
                        duration_s,
                        elapsed_s: 0.0,
                    })
                    .await;
            } else {
                ctx.db.with_mut_snapshot(|snap| {
                    if let Some(m_item) = snap.items.get_mut(&module_id) {
                        m_item.dynamic_attributes.remove(&attr_ids::DAMAGE);
                    }
                });
            }

            ctx.notify(
                crate::router::NotificationTarget::Character(char_id),
                "OnMultiModuleRepairStarted",
                "charid",
                vec![PyRep::List(vec![PyRep::Integer(module_id)])],
            );

            Some(Ok(PyRep::keyval([
                ("moduleID", PyRep::Integer(module_id)),
                ("pasteConsumed", PyRep::Integer(paste_needed as i64)),
                ("damageToRepair", PyRep::Real(cur_dmg)),
                ("repairRateHpPerSec", PyRep::Real(repair_rate_hp_per_s)),
            ])))
        }

        ("dogmaIM", "StopRepairModule") | ("dogmaIM", "StopModuleRepair") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "No active ship in session".into(),
                }));
            };
            let module_id = args
                .get(1)
                .and_then(|a| a.as_i64())
                .or_else(|| args.first().and_then(|a| a.as_i64()))
                .unwrap_or(0);
            let mut stopped = false;
            if let Some(sys_id) = session.solar_system_id {
                let sys = ctx.systems.get_or_spawn(sys_id);
                stopped = sys
                    .stop_nanite_repair(ship_id, module_id)
                    .await
                    .ok()
                    .flatten()
                    .is_some();
            }
            ctx.notify(
                crate::router::NotificationTarget::Character(char_id),
                "OnRepairDone",
                "charid",
                vec![PyRep::List(vec![PyRep::Integer(module_id)])],
            );
            Some(Ok(PyRep::keyval([
                ("moduleID", PyRep::Integer(module_id)),
                ("stopped", PyRep::Bool(stopped)),
            ])))
        }

        ("miningScanMgr", "perform_scan") => {
            if args.is_empty() && kwargs.is_empty() {
                return Some(Err(BridgeRpcError {
                    // rpc-lint: allow error code string literal after lowercase method name perform_scan
                    code: "UnimplementedRPC".into(),
                    message: "miningScanMgr.perform_scan requires explicit scanner moduleID or parameters".into(),
                }));
            }
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "No active ship in session".into(),
                }));
            };
            let items = ctx.db.get_items_for_character(char_id);
            let explicit_mod_id = args.first().and_then(|a| a.as_i64());
            let scanner_opt = items.iter().find(|i| {
                i.location_id == ship_id
                    && (i.flag as u16) >= 11
                    && (i.flag as u16) <= 34
                    && explicit_mod_id.map(|mid| i.item_id == mid).unwrap_or(true)
                    && ctx.sde.types.get(&i.type_id).map(|t| t.group_id == 49 || t.default_attributes.contains_key(&197)).unwrap_or(false)
            });
            let Some(scanner) = scanner_opt else {
                return Some(Err(BridgeRpcError {
                    code: "ModuleNotFound".into(),
                    message: "No survey scanner fitted to ship".into(),
                }));
            };
            let scanner_type = ctx.sde.types.get(&scanner.type_id);
            let scan_range = scanner_type
                .and_then(|t| t.default_attributes.get(&197).copied().or_else(|| t.default_attributes.get(&54).copied()))
                .unwrap_or(15_000.0);

            let sys_id = session.solar_system_id.unwrap_or(0);
            let mut ship_pos = DVec3::ZERO;
            if sys_id > 0 {
                let sys_handle = ctx.systems.get_or_spawn(sys_id);
                if let Ok(Some((_, ship_ball))) = sys_handle.get_ball_snapshot(ship_id).await {
                    ship_pos = ship_ball.position;
                }
            }

            let asteroids = ctx.db.get_asteroids_in_system(sys_id);
            let mut scanned_asteroids = Vec::new();
            for ast in asteroids {
                if ast.quantity <= 0 {
                    continue;
                }
                let ast_pos = DVec3::new(ast.position.0, ast.position.1, ast.position.2);
                let dist = if ship_pos != DVec3::ZERO {
                    (ship_pos - ast_pos).length()
                } else {
                    0.0
                };
                if dist <= scan_range || ship_pos == DVec3::ZERO {
                    let type_name = ctx.sde.types.get(&ast.type_id).map(|t| t.name.clone()).unwrap_or_else(|| format!("Asteroid {}", ast.type_id));
                    let vol = ctx.sde.types.get(&ast.type_id).map(|t| t.volume).unwrap_or(0.1);
                    ctx.notify(
                        crate::router::NotificationTarget::Character(char_id),
                        "OnAsteroidRevealed",
                        "character",
                        vec![
                            PyRep::Integer(ast.asteroid_id),
                            PyRep::Integer(ast.type_id as i64),
                            PyRep::Integer(ast.quantity),
                        ],
                    );
                    scanned_asteroids.push(PyRep::keyval([
                        ("asteroidID", PyRep::Integer(ast.asteroid_id)),
                        ("typeID", PyRep::Integer(ast.type_id as i64)),
                        ("typeName", PyRep::String(type_name)),
                        ("quantity", PyRep::Integer(ast.quantity)),
                        ("initialQuantity", PyRep::Integer(ast.initial_quantity)),
                        ("distance", PyRep::Real(dist)),
                        ("volume", PyRep::Real(vol)),
                    ]));
                }
            }

            Some(Ok(PyRep::keyval([
                ("scanRange", PyRep::Real(scan_range)),
                ("count", PyRep::Integer(scanned_asteroids.len() as i64)),
                ("asteroidCount", PyRep::Integer(scanned_asteroids.len() as i64)),
                ("asteroids", PyRep::List(scanned_asteroids)),
            ])))
        }


        _ => None,
    }
}
