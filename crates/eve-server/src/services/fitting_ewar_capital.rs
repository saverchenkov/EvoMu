//! `fitting_ewar_capital` — EWAR, capital module, remote repair & `charFittingMgr`/`corpFittingMgr` handlers.

#![forbid(unsafe_code)]

use super::*;

pub(crate) struct EwarCapitalPrep {
    pub warp_scramble_points: i32,
    pub disables_target_mwd_mjd: bool,
    pub warp_core_stab_bonus: i32,
    pub hic_bubble_radius_m: Option<f64>,
    pub hic_velocity_multiplier: Option<f64>,
    pub hic_sig_multiplier: Option<f64>,
    pub hic_mass_addition: Option<f64>,
    pub mjd_jump_distance_m: Option<f64>,
    pub grants_interdiction_nullification: bool,
    pub target_velocity_multiplier: Option<f64>,
    pub immobilize_self: bool,
    pub self_disallow_assistance: bool,
    pub self_ewar_immune: bool,
    pub self_disable_mwd_mjd: bool,
    pub self_mass_multiplier: Option<f64>,
    pub is_adc: bool,
    pub remote_shield_amount: f64,
    pub remote_armor_amount: f64,
    pub remote_hull_amount: f64,
    pub remote_cap_amount: f64,
    pub mutadaptive_spool_step: f64,
    pub mutadaptive_spool_max: f64,
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn prepare_ewar_and_capital_module(
    ctx: &ServerContext,
    sys: &crate::system_actor::SystemHandle,
    char_id: i64,
    ship_id: i64,
    ship_group_id: u32,
    module_id: i64,
    m: &eve_dogma::FittedModule,
    mod_type: &eve_sde::InvType,
    mod_state: &eve_dogma::DogmaItemState,
    fit: &eve_dogma::ShipFittingEngine,
    target_id_opt: Option<i64>,
    is_overloaded: bool,
    is_mwd: bool,
    is_mjd: bool,
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<EwarCapitalPrep, BridgeRpcError>> {
                let mut warp_scramble_points = 0i32;
                let mut disables_target_mwd_mjd = false;
                let mut warp_core_stab_bonus = 0i32;
                let mut hic_bubble_radius_m: Option<f64> = None;
                let mut hic_velocity_multiplier: Option<f64> = None;
                let mut hic_sig_multiplier: Option<f64> = None;
                let mut hic_mass_addition: Option<f64> = None;
                let mut mjd_jump_distance_m: Option<f64> = None;
                let grants_interdiction_nullification = mod_type.group_id == 4117;
                let mut target_velocity_multiplier: Option<f64> = None;
                let mut immobilize_self = false;
                let mut self_disallow_assistance = false;
                let mut self_ewar_immune = false;
                let mut self_disable_mwd_mjd = false;
                let mut self_mass_multiplier: Option<f64> = None;

                // Assault Damage Control (`47254` T1, `47257` T2, `group_id == 60`, `effect 7012`, `feat:modules.assault-damage-control`)
                // & Capital Emergency Hull Energizer (`40714` T1, `40717` T2, `group_id == 60`, `effect 6484`, `feat:modules.capital-emergency-hull-energizer`)
                let is_adc = mod_type.group_id == 60 && mod_type.effect_ids.contains(&7012);
                if is_adc && !matches!(ship_group_id, 324 | 358) {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidShipGroup".into(),
                        message: "Assault Damage Control can only be fitted/activated on Assault Frigates (324) or Heavy Assault Cruisers (358)".into(),
                    }));
                }
                let is_cehe = matches!(m.type_id, 40714 | 40717) || mod_type.effect_ids.contains(&6484);
                if is_cehe && !matches!(ship_group_id, 547 | 1538 | 485 | 30 | 659 | 883 | 4594) {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidShipGroup".into(),
                        message: "Capital Emergency Hull Energizer can only be activated on capital hulls".into(),
                    }));
                }
                let has_active_triage = fit.modules.values().any(|fm| {
                    matches!(fm.type_id, 27951 | 4294)
                        && matches!(fm.state, eve_dogma::ModuleState::Active | eve_dogma::ModuleState::Overloaded)
                });
                if has_active_triage && matches!(mod_type.group_id, 201 | 208 | 291 | 379 | 52 | 65 | 1672) {
                    return Some(Err(BridgeRpcError {
                        code: "EwarDisabledInTriage".into(),
                        message: "EWAR modules are disabled while Triage Module is active".into(),
                    }));
                }
                if matches!(mod_type.group_id, 290 | 209) {
                    let mut dist_m = 0.0;
                    if let Some(tid) = target_id_opt {
                        if let (Ok(Some((_, sb))), Ok(Some((_, tb)))) = (sys.get_ball_snapshot(ship_id).await, sys.get_ball_snapshot(tid).await) {
                            dist_m = (sb.position - tb.position).length();
                        }
                    }
                    if let Err(e) = crate::services::tactical_modules::apply_projected_remote_assistance(
                        ctx, char_id, ship_id, module_id, mod_type, mod_state, target_id_opt, dist_m,
                    ) {
                        return Some(Err(e));
                    }
                }

                // Zero-Point Mass Entangler (`49099`, `group_id == 2008`, `feat:modules.zero-point-mass-entangler`)
                if mod_type.group_id == 2008 {
                    if ship_group_id != 894 {
                        return Some(Err(BridgeRpcError {
                            code: "InvalidShipGroup".into(),
                            message: "Zero-Point Mass Entangler can only be activated on Heavy Interdiction Cruisers (894)".into(),
                        }));
                    }
                    self_disable_mwd_mjd = true;
                    let mass_pct = mod_state.get_attribute(1131, &ctx.sde);
                    let mass_mult = if mass_pct != 0.0 { (1.0 + mass_pct / 100.0).clamp(0.05, 1.0) } else { 0.20 };
                    let vel_pct = mod_state.get_attribute(2603, &ctx.sde);
                    let vel_mult = if vel_pct != 0.0 { (1.0 + vel_pct / 100.0).clamp(0.01, 1.0) } else { 0.05 };
                    self_mass_multiplier = Some(mass_mult);
                    hic_velocity_multiplier = Some(vel_mult);
                    warp_core_stab_bonus = 2;
                }

                // Siege (`20280`/`4292`), Triage (`27951`/`4294`), Bastion (`33400`), Industrial Core (`28583`/`58945`), Phenomena Generator (`group 1815`)
                if mod_type.group_id == 515 || mod_type.group_id == 1815 {
                    let is_siege = matches!(m.type_id, 20280 | 4292) || mod_type.name.contains("Siege Module");
                    let is_triage = matches!(m.type_id, 27951 | 4294) || mod_type.name.contains("Triage Module");
                    let is_bastion = m.type_id == 33400 || mod_type.name.contains("Bastion Module");
                    let is_phenomena = mod_type.group_id == 1815;

                    if is_siege && !matches!(ship_group_id, 485 | 4594) {
                        return Some(Err(BridgeRpcError { code: "InvalidShipGroup".into(), message: "Siege Module can only be activated on Dreadnoughts (485) or Lancer Dreadnoughts (4594)".into() }));
                    }
                    if is_triage && !matches!(ship_group_id, 1538 | 547) {
                        return Some(Err(BridgeRpcError { code: "InvalidShipGroup".into(), message: "Triage Module can only be activated on Force Auxiliaries (group 1538)".into() }));
                    }
                    if is_bastion && ship_group_id != 900 {
                        return Some(Err(BridgeRpcError { code: "InvalidShipGroup".into(), message: "Bastion Module can only be activated on Marauders (group 900)".into() }));
                    }
                    if is_phenomena && ship_group_id != 30 {
                        return Some(Err(BridgeRpcError { code: "InvalidShipGroup".into(), message: "Phenomena Generator can only be activated on Titans (group 30)".into() }));
                    }

                    let fuel_type_id = mod_type.default_attributes.get(&713).map(|&v| v as u32).unwrap_or(0);
                    let base_fuel_qty = mod_type.default_attributes.get(&714).copied().unwrap_or(0.0);
                    let fuel_qty_needed = if is_siege {
                        let tac_lvl = ctx.character_skill_levels(char_id).get(&22043).copied().unwrap_or(0);
                        crate::services::tactical_modules::compute_siege_strontium_cost(tac_lvl) as i32
                    } else if is_triage {
                        let tac_lvl = ctx.character_skill_levels(char_id).get(&27906).copied().unwrap_or(0);
                        crate::services::tactical_modules::compute_triage_strontium_cost(tac_lvl) as i32
                    } else {
                        base_fuel_qty.round() as i32
                    };
                    if fuel_type_id > 0 && fuel_qty_needed > 0 {
                        let require_strict = is_siege || is_triage || is_phenomena;
                        let fuel_check = ctx.db.with_mut_snapshot(|snap| {
                            let cand_id = snap.items.values().find(|i| {
                                i.location_id == ship_id && (i.type_id == fuel_type_id || (is_phenomena && i.type_id == 16275)) && i.quantity > 0
                            }).map(|i| (i.item_id, i.quantity));
                            match cand_id {
                                Some((fid, avail)) if avail >= fuel_qty_needed => {
                                    let mut rem = 0;
                                    if let Some(f_it) = snap.items.get_mut(&fid) {
                                        f_it.quantity -= fuel_qty_needed;
                                        rem = f_it.quantity;
                                    }
                                    if rem == 0 {
                                        snap.items.remove(&fid);
                                    }
                                    Ok(true)
                                }
                                Some((fid, avail)) if is_phenomena && avail > 0 => {
                                    snap.items.remove(&fid);
                                    Ok(true)
                                }
                                Some((_, avail)) => Err(format!("Insufficient fuel (type {fuel_type_id}): need {fuel_qty_needed}, have {avail}")),
                                None if require_strict => Err(format!("Missing required fuel (type {fuel_type_id}, quantity {fuel_qty_needed})")),
                                None => Ok(false),
                            }
                        });
                        if let Err(msg) = fuel_check {
                            return Some(Err(BridgeRpcError { code: "InsufficientFuel".into(), message: msg }));
                        }
                    }

                    let speed_factor_pct = mod_state.get_attribute(20, &ctx.sde);
                    if speed_factor_pct > -99.9 && speed_factor_pct < 0.0 {
                        hic_velocity_multiplier = Some((1.0 + speed_factor_pct / 100.0).clamp(0.01, 1.0));
                    } else {
                        immobilize_self = true;
                    }
                    self_disallow_assistance = true;
                    self_disable_mwd_mjd = true;
                    if is_siege || is_triage {
                        self_ewar_immune = true;
                        self_mass_multiplier = Some(10.0);
                    }
                    if is_bastion {
                        let now_unix = ctx.now_unix_secs();
                        let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
                        let st = cw.get_or_create(char_id);
                        st.weapon_timer_until_unix = st.weapon_timer_until_unix.max(now_unix + 60);
                    }

                    if is_phenomena {
                        let balls = sys.list_balls().await.unwrap_or_default();
                        let dyn_ship_ids: Vec<i64> = balls
                            .iter()
                            .filter(|b| b.id < 700_000_000)
                            .map(|b| b.id)
                            .collect();
                        let mut dbuff_mods: Vec<(u16, f64)> = Vec::new();
                        for &(id_attr, val_attr) in &[(2468u16, 2469u16), (2470, 2471), (2472, 2473), (2536, 2537)] {
                            let dbuff_id = mod_state.get_attribute(id_attr, &ctx.sde) as u32;
                            let pct = mod_state.get_attribute(val_attr, &ctx.sde);
                            if dbuff_id > 0 && pct.abs() > 1e-9 {
                                if let Some(dbuff) = ctx.sde.dbuff_collections.get(&dbuff_id) {
                                    for &target_aid in &dbuff.item_modifiers {
                                        dbuff_mods.push((target_aid, pct));
                                    }
                                }
                            }
                        }
                        if !dbuff_mods.is_empty() {
                            ctx.db.with_mut_snapshot(|snap| {
                                for &sid in &dyn_ship_ids {
                                    if let Some(s_item) = snap.items.get_mut(&sid) {
                                        let s_tid = s_item.type_id;
                                        for &(aid, pct) in &dbuff_mods {
                                            let base_v = s_item
                                                .dynamic_attributes
                                                .get(&aid)
                                                .copied()
                                                .or_else(|| {
                                                    ctx.sde
                                                        .types
                                                        .get(&s_tid)
                                                        .and_then(|t| t.default_attributes.get(&aid).copied())
                                                })
                                                .unwrap_or(1.0);
                                            s_item
                                                .dynamic_attributes
                                                .insert(aid, base_v * (1.0 + pct / 100.0));
                                        }
                                    }
                                }
                            });
                        }
                    }
                }

                // Propulsion Modules (`group_id == 46`: Afterburner & Microwarpdrive)
                if mod_type.group_id == 46 {
                    if is_mwd {
                        let sig_bonus_pct = mod_state.get_attribute(554, &ctx.sde);
                        if sig_bonus_pct > 0.0 {
                            hic_sig_multiplier = Some(1.0 + sig_bonus_pct / 100.0);
                        }
                    }
                    let mass_add = mod_state.get_attribute(attr_ids::MASS_ADDITION, &ctx.sde);
                    if mass_add > 0.0 {
                        hic_mass_addition = Some(mass_add);
                    }
                }

                // Micro Jump Drive (`group_id == 1189 | 4769`, e.g., `Large Micro Jump Drive` `4383`, `Medium Micro Jump Drive` `33915`)
                if is_mjd {
                    let jump_dist = mod_type
                        .default_attributes
                        .get(&2064)
                        .copied()
                        .unwrap_or(100_000.0);
                    mjd_jump_distance_m = Some(jump_dist);
                    let mjd_sig_pct = mod_state.get_attribute(973, &ctx.sde);
                    if mjd_sig_pct > 0.0 {
                        hic_sig_multiplier = Some(1.0 + mjd_sig_pct / 100.0);
                    }
                }

                // Warp Disruptor / Warp Scrambler (`group_id == 52`)
                if mod_type.group_id == 52 {
                    let Some(target_id) = target_id_opt else {
                        return Some(Err(BridgeRpcError {
                            code: "TargetRequired".into(),
                            message: "Warp disruptor/scrambler requires a locked target".into(),
                        }));
                    };
                    let mut max_range = mod_state.get_attribute(attr_ids::MAX_RANGE, &ctx.sde);
                    if max_range <= 0.0 {
                        max_range = mod_type.default_attributes.get(&54).copied().unwrap_or(20_000.0);
                    }
                    if is_overloaded {
                        let ov_bonus = mod_type.default_attributes.get(&1208).copied().unwrap_or(20.0);
                        max_range *= 1.0 + ov_bonus / 100.0;
                    }
                    if let (Ok(Some((_, ship_ball))), Ok(Some((_, target_ball)))) = (
                        sys.get_ball_snapshot(ship_id).await,
                        sys.get_ball_snapshot(target_id).await,
                    ) {
                        let dist = (ship_ball.position - target_ball.position).length();
                        if dist > max_range {
                            return Some(Err(BridgeRpcError {
                                code: "TargetOutOfRange".into(),
                                message: format!(
                                    "Target {target_id} is at {dist:.1} m, exceeding module optimal range {max_range:.1} m"
                                ),
                            }));
                        }
                    }
                    let scram_eval = eve_dogma::eval_warp_scrambler_effects(&[&mod_state], &[0.0], &ctx.sde);
                    warp_scramble_points = scram_eval.scramble_strength.max(1);
                    disables_target_mwd_mjd = scram_eval.blocks_mwd;
                }

                // SDE attribute `3265` (`activationRequiresActiveIndustrialCore`, e.g. Shipboard Compressors `group_id == 4174`):
                // require an active Industrial Core (`group_id == 515`) fitted on the ship (`docs/eve-wiki/Compression.md:39`).
                if mod_type.default_attributes.get(&3265).copied().unwrap_or(0.0) > 0.0 {
                    let has_active_core_in_fit = fit.modules.values().any(|fm| {
                        (27..=34).contains(&fm.flag_id)
                            && matches!(
                                fm.state,
                                eve_dogma::ModuleState::Active | eve_dogma::ModuleState::Overloaded
                            )
                            && ctx.sde.types.get(&fm.type_id).is_some_and(|t| t.group_id == 515)
                    });
                    let has_active_core_in_db = ctx.db.with_snapshot(|snap| {
                        snap.items.values().any(|it| {
                            it.location_id == ship_id
                                && (27..=34).contains(&(it.flag as u16))
                                && snap.module_active_states.contains(&it.item_id)
                                && ctx.sde.types.get(&it.type_id).is_some_and(|t| t.group_id == 515)
                        })
                    });
                    if !has_active_core_in_fit && !has_active_core_in_db {
                        return Some(Err(BridgeRpcError {
                            code: "IndustrialCoreNotActive".into(),
                            message: "Module requires an active Industrial Core (attribute 3265 activationRequiresActiveIndustrialCore)".into(),
                        }));
                    }
                }

                // Heavy Interdictor Warp Disruption Field Generator (`group_id == 899`, e.g., `28654` T1, `4248` T2)
                if mod_type.group_id == 899 {
                    if ship_group_id != 894 {
                        return Some(Err(BridgeRpcError {
                            code: "InvalidShipGroup".into(),
                            message: "Warp Disruption Field Generator can only be activated on Heavy Interdiction Cruisers (group 894)".into(),
                        }));
                    }
                    let loaded_script_id = m.charge_type_id.filter(|&cid| {
                        cid == 29003
                            || cid == 45010
                            || ctx.sde.types.get(&cid).is_some_and(|t| t.group_id == 908)
                    });
                    let base_wdfg_range = mod_type
                        .default_attributes
                        .get(&103)
                        .copied()
                        .unwrap_or(16_000.0);

                    if let Some(script_tid) = loaded_script_id {
                        // Scripted mode: Focused Warp Disruption Script (29003) or Focused Warp Scrambling Script (45010)
                        let Some(target_id) = target_id_opt else {
                            return Some(Err(BridgeRpcError {
                                code: "TargetRequired".into(),
                                message: "Scripted Warp Disruption Field Generator requires a locked target".into(),
                            }));
                        };
                        let range_mult = if script_tid == 29003 {
                            1.50 // +50% optimal range -> 24km (T1) / 30km (T2)
                        } else if script_tid == 45010 {
                            1.20 // +20% optimal range -> 19.2km (T1) / 24km (T2)
                        } else {
                            1.50
                        };
                        let ov_mult = if is_overloaded { 1.20 } else { 1.0 };
                        let scripted_max_range = base_wdfg_range * range_mult * ov_mult;
                        if let (Ok(Some((_, ship_ball))), Ok(Some((_, target_ball)))) = (
                            sys.get_ball_snapshot(ship_id).await,
                            sys.get_ball_snapshot(target_id).await,
                        ) {
                            let dist = (ship_ball.position - target_ball.position).length();
                            if dist > scripted_max_range {
                                return Some(Err(BridgeRpcError {
                                    code: "TargetOutOfRange".into(),
                                    message: format!(
                                        "Target {target_id} is at {dist:.1} m, exceeding scripted WDFG range {scripted_max_range:.1} m"
                                    ),
                                }));
                            }
                        }
                        // Infinite point (100 warp scramble strength) + MWD/MJD suppression, no bubble
                        warp_scramble_points = 100;
                        disables_target_mwd_mjd = true;
                        hic_bubble_radius_m = None;
                    } else {
                        // Unscripted mode: AoE Warp Disruption Bubble + HIC self-penalties
                        hic_bubble_radius_m = Some(base_wdfg_range);
                        hic_velocity_multiplier = Some(0.40);
                        let sig_bonus_pct = mod_type
                            .default_attributes
                            .get(&554)
                            .copied()
                            .unwrap_or(50.0);
                        hic_sig_multiplier = Some(1.0 + sig_bonus_pct / 100.0);
                        let mass_add = mod_type
                            .default_attributes
                            .get(&796)
                            .copied()
                            .unwrap_or(80_000_000.0);
                        hic_mass_addition = Some(mass_add);
                    }
                }

                // Warp Core Stabilizer (`group_id == 315`, e.g., `Warp Core Stabilizer I` `10998`)
                if mod_type.group_id == 315 {
                    let raw_wcs = mod_type
                        .default_attributes
                        .get(&105)
                        .copied()
                        .unwrap_or(-2.0);
                    warp_core_stab_bonus = (-raw_wcs).round().max(1.0) as i32;
                }

                // Weapon Disruptor (`group_id == 291`: Tracking Disruptor 2108/2109 & Guidance Disruptor 37543/37546),
                // Remote Sensor Dampener (`group_id == 208`), Target Painter (`group_id == 379`),
                // Stasis Webifier (`group_id == 65`), Heavy Stasis Grappler (`group_id == 1672`)
                if matches!(mod_type.group_id, 291 | 208 | 379 | 65 | 1672) {
                    let Some(target_id) = target_id_opt else {
                        return Some(Err(BridgeRpcError {
                            code: "TargetRequired".into(),
                            message: "EWAR module requires a locked target".into(),
                        }));
                    };
                    let mut distance_m = 0.0;
                    if let (Ok(Some((_, ship_ball))), Ok(Some((_, target_ball)))) = (
                        sys.get_ball_snapshot(ship_id).await,
                        sys.get_ball_snapshot(target_id).await,
                    ) {
                        distance_m = (ship_ball.position - target_ball.position).length();
                    }
                    let mut proj_mod_state = mod_state.clone();
                    let mut proj_source_tid = m.type_id;
                    let mut proj_dist = distance_m;
                    if mod_type.group_id == 65 || mod_type.group_id == 1672 {
                        let mut max_range = mod_state.get_attribute(attr_ids::MAX_RANGE, &ctx.sde);
                        if max_range <= 0.0 {
                            max_range = mod_type.default_attributes.get(&54).copied().unwrap_or(10_000.0);
                        }
                        let falloff = mod_state
                            .get_attribute(2044, &ctx.sde)
                            .max(mod_state.get_attribute(attr_ids::FALLOFF, &ctx.sde));
                        if mod_type.group_id == 65 && falloff <= 0.0 && distance_m > max_range && max_range > 0.0 {
                            return Some(Err(BridgeRpcError {
                                code: "TargetOutOfRange".into(),
                                message: format!(
                                    "Target {target_id} is at {distance_m:.1} m, exceeding webifier range {max_range:.1} m"
                                ),
                            }));
                        }
                        let att = eve_dogma::compute_falloff_attenuation(distance_m, max_range, falloff);
                        let raw_sf = mod_state.get_attribute(20, &ctx.sde);
                        let eff_sf = if mod_type.group_id == 1672 {
                            // Heavy Stasis Grappler (`feat:modules.heavy-stasis-grappler-falloff`):
                            // Decays across optimal (1 km) and falloffEffectiveness (8-10 km) down to a -15% floor
                            if distance_m <= max_range + 3.0 * falloff.max(1.0) {
                                (raw_sf * att).min(-15.0)
                            } else {
                                0.0
                            }
                        } else {
                            raw_sf * att
                        };
                        target_velocity_multiplier = Some((1.0 + eff_sf / 100.0).clamp(0.05, 1.0));
                        if mod_type.group_id == 1672 {
                            proj_mod_state.base_attributes.insert(20, eff_sf);
                            proj_mod_state.base_attributes.insert(54, 1_000_000_000.0);
                            proj_mod_state.modifiers.retain(|mm| mm.target_attribute_id != 20);
                            proj_source_tid = 526;
                            proj_dist = 0.0;
                        }
                    }
                    ctx.upsert_projected_ewar_on_ship(
                        target_id,
                        eve_dogma::ProjectedEwarEffect {
                            source_module_id: module_id,
                            source_module_type_id: proj_source_tid,
                            charge_type_id: m.charge_type_id,
                            distance: proj_dist,
                            overloaded: is_overloaded,
                            effective_module: proj_mod_state,
                        },
                    );
                }

                // Remote Shield Booster (`41`), Remote Armor Repairer (`325`), Mutadaptive Remote Armor Repairer (`2018`),
                // Remote Hull Repairer (`585`), Remote Capacitor Transmitter (`67`) (`feat:modules.remote-*`)
                let mut remote_shield_amount = 0.0;
                let mut remote_armor_amount = 0.0;
                let mut remote_hull_amount = 0.0;
                let mut remote_cap_amount = 0.0;
                let mut mutadaptive_spool_step = 0.0;
                let mut mutadaptive_spool_max = 0.0;
                if matches!(mod_type.group_id, 41 | 325 | 2018 | 585 | 67 | 1697 | 1698) {
                    let Some(target_id) = target_id_opt else {
                        return Some(Err(BridgeRpcError {
                            code: "TargetRequired".into(),
                            message: "Remote assistance module requires a locked target".into(),
                        }));
                    };
                    if let Ok(Some(t_cs)) = sys.get_combat_state(target_id).await {
                        if t_cs.disallow_assistance {
                            return Some(Err(BridgeRpcError {
                                code: "TargetDisallowsAssistance".into(),
                                message: format!("Target {target_id} has disallowAssistance active (Siege/Triage/HIC bubble)"),
                            }));
                        }
                    }
                    let mut max_range = mod_state.get_attribute(attr_ids::MAX_RANGE, &ctx.sde);
                    if max_range <= 0.0 {
                        max_range = mod_type.default_attributes.get(&54).copied().unwrap_or(20_000.0);
                    }
                    let falloff = mod_state
                        .get_attribute(2044, &ctx.sde)
                        .max(mod_state.get_attribute(attr_ids::FALLOFF, &ctx.sde));
                    let mut att = 1.0;
                    if let (Ok(Some((_, ship_ball))), Ok(Some((_, target_ball)))) = (
                        sys.get_ball_snapshot(ship_id).await,
                        sys.get_ball_snapshot(target_id).await,
                    ) {
                        let dist = (ship_ball.position - target_ball.position).length();
                        if falloff <= 0.0 && dist > max_range && max_range > 0.0 {
                            return Some(Err(BridgeRpcError {
                                code: "TargetOutOfRange".into(),
                                message: format!(
                                    "Target {target_id} is at {dist:.1} m, exceeding remote assistance range {max_range:.1} m"
                                ),
                            }));
                        }
                        att = eve_dogma::compute_falloff_attenuation(dist, max_range, falloff);
                    }
                    let ov_m = if is_overloaded { 1.20 } else { 1.0 };
                    let rr_imp_m = crate::services::tactical_modules::incoming_remote_repair_multiplier(ctx, target_id);
                    match mod_type.group_id {
                        41 | 1697 => {
                            remote_shield_amount = mod_state.get_attribute(68, &ctx.sde) * att * ov_m * rr_imp_m;
                        }
                        325 | 2018 | 1698 => {
                            remote_armor_amount = mod_state.get_attribute(84, &ctx.sde) * att * ov_m * rr_imp_m;
                            if mod_type.group_id == 2018 {
                                mutadaptive_spool_step = mod_type.default_attributes.get(&2796).copied().unwrap_or(0.10);
                                mutadaptive_spool_max = mod_type.default_attributes.get(&2797).copied().unwrap_or(1.50);
                            }
                        }
                        585 => {
                            remote_hull_amount = mod_state.get_attribute(83, &ctx.sde) * att * ov_m * rr_imp_m;
                        }
                        67 => {
                            remote_cap_amount = mod_state.get_attribute(90, &ctx.sde) * att * ov_m;
                        }
                        _ => {}
                    }
                    // Crimewatch flag inheritance (`feat:modules.remote-shield`):
                    // Assisting a pilot with active weapon/PvP/Suspect/Criminal timers propagates those flags to the logistics pilot!
                    let target_owner_id = ctx
                        .db
                        .with_snapshot(|s| s.items.get(&target_id).map(|i| i.owner_id))
                        .unwrap_or(target_id);
                    if target_owner_id != char_id && target_owner_id > 0 {
                        let now_unix = ctx.now_unix_secs();
                        let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
                        if let Some(t_state) = cw.states.get(&target_owner_id).cloned() {
                            let l_state = cw.get_or_create(char_id);
                            if t_state.is_weapon_timer_active(now_unix) {
                                l_state.weapon_timer_until_unix =
                                    l_state.weapon_timer_until_unix.max(t_state.weapon_timer_until_unix);
                            }
                            if t_state.is_pvp_active(now_unix) {
                                l_state.pvp_timer_until_unix =
                                    l_state.pvp_timer_until_unix.max(t_state.pvp_timer_until_unix);
                            }
                            if t_state.is_suspect(now_unix) {
                                l_state.suspect_flag_until_unix =
                                    l_state.suspect_flag_until_unix.max(t_state.suspect_flag_until_unix);
                            }
                            if t_state.is_criminal(now_unix) {
                                l_state.criminal_flag_until_unix =
                                    l_state.criminal_flag_until_unix.max(t_state.criminal_flag_until_unix);
                            }
                        }
                    }
                }

                // Supercarrier Burst Projectors (`group_id == 842`, `feat:modules.burst-projectors`)
                if mod_type.group_id == 842 {
                    let burst_radius = mod_state.get_attribute(2279, &ctx.sde).max(15_000.0);
                    let mut burst_center = DVec3::ZERO;
                    if let Some(tid) = target_id_opt {
                        if let Ok(Some((_, t_ball))) = sys.get_ball_snapshot(tid).await {
                            burst_center = t_ball.position;
                        }
                    } else if let Ok(Some((_, s_ball))) = sys.get_ball_snapshot(ship_id).await {
                        burst_center = s_ball.position;
                    }
                    let all_balls = sys.list_balls().await.unwrap_or_default();
                    let victims: Vec<i64> = all_balls
                        .iter()
                        .filter(|b| {
                            b.id != ship_id
                                && b.id < 700_000_000
                                && (b.position - burst_center).length() <= burst_radius
                        })
                        .map(|b| b.id)
                        .collect();
                    match m.type_id {
                        27678 => {
                            for &vid in &victims {
                                let _ = sys.apply_ecm_jam(vid, 20.0).await;
                            }
                        }
                        40636 => {
                            let _ = sys.add_warp_bubble(module_id, burst_center, burst_radius).await;
                        }
                        40635 => {
                            let neut_amt = mod_state.get_attribute(97, &ctx.sde).max(1200.0);
                            for &vid in &victims {
                                if let Ok(Some(cs)) = sys.get_combat_state(vid).await {
                                    let _ = sys
                                        .set_capacitor(vid, (cs.capacitor - neut_amt).max(0.0))
                                        .await;
                                }
                            }
                        }
                        40699 | 40697 | 40698 | 40696 => {
                            let synth_tid = match m.type_id {
                                40696 => 12709,
                                40698 => 526,
                                40699 => 1968,
                                _ => 2108,
                            };
                            if m.type_id == 40698 {
                                let sf = mod_state.get_attribute(20, &ctx.sde).min(-30.0);
                                target_velocity_multiplier =
                                    Some((1.0 + sf / 100.0).clamp(0.05, 1.0));
                            }
                            let mut synth_mod = mod_state.clone();
                            synth_mod.base_attributes.insert(54, 1_000_000_000.0);
                            for &vid in &victims {
                                ctx.upsert_projected_ewar_on_ship(
                                    vid,
                                    eve_dogma::ProjectedEwarEffect {
                                        source_module_id: module_id,
                                        source_module_type_id: synth_tid,
                                        charge_type_id: None,
                                        distance: 0.0,
                                        overloaded: false,
                                        effective_module: synth_mod.clone(),
                                    },
                                );
                            }
                        }
                        _ => {}
                    }
                }


    Some(Ok(EwarCapitalPrep {
        warp_scramble_points,
        disables_target_mwd_mjd,
        warp_core_stab_bonus,
        hic_bubble_radius_m,
        hic_velocity_multiplier,
        hic_sig_multiplier,
        hic_mass_addition,
        mjd_jump_distance_m,
        grants_interdiction_nullification,
        target_velocity_multiplier,
        immobilize_self,
        self_disallow_assistance,
        self_ewar_immune,
        self_disable_mwd_mjd,
        self_mass_multiplier,
        is_adc,
        remote_shield_amount,
        remote_armor_amount,
        remote_hull_amount,
        remote_cap_amount,
        mutadaptive_spool_step,
        mutadaptive_spool_max,
    }))
}

pub(crate) async fn handle_fitting_mgr(
    ctx: &ServerContext,
    session: &mut SessionState,
    _bound_entry: Option<&BoundObjectEntry>,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("charFittingMgr", "SaveFitting") | ("corpFittingMgr", "SaveFitting") | ("allianceFittingMgr", "SaveFitting") | ("charFittingMgr", "SaveManyFittings") | ("corpFittingMgr", "SaveManyFittings") | ("allianceFittingMgr", "SaveManyFittings") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let default_owner = match service {
                "corpFittingMgr" => session.corporation_id.unwrap_or(char_id),
                "allianceFittingMgr" => session
                    .alliance_id
                    .or(session.corporation_id)
                    .unwrap_or(char_id),
                _ => char_id,
            };

            let parse_fit_items = |items_arg: Option<&PyRep>,
                                   ship_type_id: TypeId|
             -> Result<Vec<(TypeId, InventoryFlag, i32)>, BridgeRpcError> {
                if let Some(arg) = items_arg {
                    let entries = match arg {
                        PyRep::List(v) | PyRep::Tuple(v) => v,
                        _ => {
                            return Err(BridgeRpcError {
                                code: "InvalidFittingItems".into(),
                                message:
                                    "Fitting items must be a List or Tuple of (typeID, flagID, quantity)"
                                        .into(),
                            })
                        }
                    };
                    let mut out = Vec::with_capacity(entries.len());
                    for entry in entries {
                        let seq = match entry {
                            PyRep::Tuple(v) | PyRep::List(v) => v,
                            _ => {
                                return Err(BridgeRpcError {
                                    code: "InvalidFittingItem".into(),
                                    message:
                                        "Each fitting item must be a (typeID, flagID, quantity) tuple"
                                            .into(),
                                })
                            }
                        };
                        let Some(mod_type_id) =
                            seq.first().and_then(|v| v.as_i64()).filter(|&v| v > 0)
                        else {
                            return Err(BridgeRpcError {
                                code: "InvalidFittingItem".into(),
                                message: "Fitting item is missing a positive typeID".into(),
                            });
                        };
                        let Some(flag_raw) =
                            seq.get(1).and_then(|v| v.as_i64()).filter(|&v| v >= 0)
                        else {
                            return Err(BridgeRpcError {
                                code: "InvalidFittingItem".into(),
                                message: "Fitting item is missing a valid flagID".into(),
                            });
                        };
                        let qty_i64 = seq.get(2).and_then(|v| v.as_i64()).unwrap_or(1);
                        let Ok(qty) = i32::try_from(qty_i64) else {
                            return Err(BridgeRpcError {
                                code: "InvalidFittingItem".into(),
                                message: format!(
                                    "Fitting item quantity overflows i32 bounds, got {qty_i64}"
                                ),
                            });
                        };
                        if qty <= 0 {
                            return Err(BridgeRpcError {
                                code: "InvalidFittingItem".into(),
                                message: format!(
                                    "Fitting item quantity must be positive, got {qty}"
                                ),
                            });
                        }
                        let flag_u16 = u16::try_from(flag_raw).ok();
                        let flag =
                            flag_u16.map(InventoryFlag::from_u16).unwrap_or(InventoryFlag::None);
                        let valid_flag = flag_u16
                            .map(|f| {
                                matches!(f, 5 | 11..=34 | 87 | 92..=94 | 125..=132 | 158)
                                    && (flag as u16) == f
                            })
                            .unwrap_or(false);
                        if !valid_flag {
                            return Err(BridgeRpcError {
                                code: "InvalidFittingFlag".into(),
                                message: format!("Invalid fitting slot or bay flag {flag_raw}"),
                            });
                        }
                        out.push((mod_type_id as TypeId, flag, qty));
                    }
                    Ok(out)
                } else {
                    let char_items = ctx.db.get_items_for_character(char_id);
                    let active_ship_matches = session
                        .ship_id
                        .and_then(|sid| char_items.iter().find(|i| i.item_id == sid))
                        .map(|s| s.type_id == ship_type_id)
                        .unwrap_or(false);
                    if active_ship_matches {
                        let sid = session.ship_id.unwrap_or(0);
                        Ok(char_items
                            .into_iter()
                            .filter(|i| {
                                let is_mod_or_drone = ctx
                                    .sde
                                    .types
                                    .get(&i.type_id)
                                    .map(|t| {
                                        t.category_id == 7
                                            || t.category_id == 32
                                            || (i.flag as u16) == 87
                                    })
                                    .unwrap_or(false);
                                i.location_id == sid
                                    && is_mod_or_drone
                                    && matches!(i.flag as u16, 11..=34 | 87 | 92..=94 | 125..=132)
                            })
                            .map(|i| (i.type_id, i.flag, i.quantity))
                            .collect())
                    } else {
                        Ok(Vec::new())
                    }
                }
            };

            let validate_and_build_fit =
                |name: String,
                 raw_ship_type: i64,
                 description: String,
                 items_arg: Option<&PyRep>|
                 -> Result<
                    (String, TypeId, String, Vec<(TypeId, InventoryFlag, i32)>),
                    BridgeRpcError,
                > {
                    let ship_type_id = raw_ship_type as TypeId;
                    let is_valid_hull = ctx
                        .sde
                        .types
                        .get(&ship_type_id)
                        .map(|t| t.category_id == 6)
                        .unwrap_or(false);
                    if !is_valid_hull {
                        return Err(BridgeRpcError {
                            code: "InvalidShipType".into(),
                            message: format!("Type {ship_type_id} is not a valid ship hull"),
                        });
                    }
                    let parsed_items = parse_fit_items(items_arg, ship_type_id)?;
                    let all_skills_lvl5: HashMap<TypeId, u8> = ctx
                        .sde
                        .types
                        .values()
                        .filter(|t| t.category_id == 16)
                        .map(|t| (t.type_id, 5))
                        .collect();
                    let mut validator =
                        eve_dogma::ShipFittingEngine::from_sde(0, ship_type_id, &ctx.sde)
                            .with_character_skills(all_skills_lvl5);
                    for (idx, &(mod_type_id, flag, qty)) in parsed_items.iter().enumerate() {
                        let flag_u16 = flag as u16;
                        if matches!(flag_u16, 11..=34 | 92..=94 | 125..=132) {
                            if qty != 1 {
                                return Err(BridgeRpcError {
                                    code: "InvalidFittingQuantity".into(),
                                    message: format!(
                                        "Fitting slot {flag_u16} requires quantity 1, got {qty}"
                                    ),
                                });
                            }
                            if let Err(e) = validator.fit_and_online_module(
                                (idx as i64) + 1,
                                mod_type_id,
                                flag_u16,
                                &ctx.sde,
                            ) {
                                return Err(BridgeRpcError {
                                    code: "InvalidFittingModule".into(),
                                    message: e,
                                });
                            }
                        } else if !ctx.sde.types.contains_key(&mod_type_id) {
                            return Err(BridgeRpcError {
                                code: "UnknownItemType".into(),
                                message: format!(
                                    "Unknown item type {mod_type_id} in saved fitting"
                                ),
                            });
                        }
                    }
                    Ok((name, ship_type_id, description, parsed_items))
                };

            let extract_fit_from_rep = |rep: &PyRep| -> Result<
                (String, TypeId, String, Vec<(TypeId, InventoryFlag, i32)>),
                BridgeRpcError,
            > {
                let name = rep
                    .dict_get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Saved Fitting")
                    .to_string();
                let Some(raw_ship_type) = rep.dict_get("shipTypeID").and_then(|v| v.as_i64())
                else {
                    return Err(BridgeRpcError {
                        code: "InvalidShipType".into(),
                        message: "Fitting object requires shipTypeID".into(),
                    });
                };
                let description = rep
                    .dict_get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let items_rep = rep.dict_get("fitData").or_else(|| rep.dict_get("items"));
                validate_and_build_fit(name, raw_ship_type, description, items_rep)
            };

            let mut owner_id = default_owner;
            let mut fits_to_save: Vec<(String, TypeId, String, Vec<(TypeId, InventoryFlag, i32)>)> =
                Vec::new();

            if method == "SaveManyFittings" {
                let fittings_arg = if let Some(oid) = args.first().and_then(|a| a.as_i64()) {
                    if oid > 0 {
                        owner_id = oid;
                    }
                    args.get(1)
                } else {
                    args.first()
                };
                let Some(f_arg) = fittings_arg else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidArgument".into(),
                        message: "SaveManyFittings requires fittings collection".into(),
                    }));
                };
                match f_arg {
                    PyRep::Dict(entries) => {
                        for (_, fit_rep) in entries {
                            match extract_fit_from_rep(fit_rep) {
                                Ok(f) => fits_to_save.push(f),
                                Err(e) => return Some(Err(e)),
                            }
                        }
                    }
                    PyRep::List(list) | PyRep::Tuple(list) => {
                        for fit_rep in list {
                            match extract_fit_from_rep(fit_rep) {
                                Ok(f) => fits_to_save.push(f),
                                Err(e) => return Some(Err(e)),
                            }
                        }
                    }
                    _ => {
                        return Some(Err(BridgeRpcError {
                            code: "InvalidArgument".into(),
                            message:
                                "SaveManyFittings requires a Dict or List of fitting objects"
                                    .into(),
                        }));
                    }
                }
            } else if let Some(oid) = args.first().and_then(|a| a.as_i64()) {
                // V24 client signature: SaveFitting(ownerID, fitting)
                if oid > 0 {
                    owner_id = oid;
                }
                let Some(fit_rep) = args.get(1) else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidArgument".into(),
                        message: "SaveFitting(ownerID, fitting) requires fitting object".into(),
                    }));
                };
                match extract_fit_from_rep(fit_rep) {
                    Ok(f) => fits_to_save.push(f),
                    Err(e) => return Some(Err(e)),
                }
            } else if let Some(fit_rep) = args
                .first()
                .filter(|a| a.dict_get("shipTypeID").is_some())
            {
                match extract_fit_from_rep(fit_rep) {
                    Ok(f) => fits_to_save.push(f),
                    Err(e) => return Some(Err(e)),
                }
            } else {
                // Legacy positional signature: SaveFitting(name, shipTypeID, description?, items?)
                let name = args
                    .first()
                    .and_then(|a| a.as_str())
                    .unwrap_or("Rifter Combat Fit")
                    .to_string();
                let Some(raw_ship_type) = args.get(1).and_then(|a| a.as_i64()) else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidShipType".into(),
                        message: "SaveFitting requires shipTypeID".into(),
                    }));
                };
                let (description, items_arg) = match args.get(2) {
                    Some(PyRep::String(s)) => (s.clone(), args.get(3)),
                    Some(list_or_tuple @ (PyRep::List(_) | PyRep::Tuple(_))) => {
                        (format!("Saved via {service}"), Some(list_or_tuple))
                    }
                    _ => (format!("Saved via {service}"), args.get(3)),
                };
                match validate_and_build_fit(name, raw_ship_type, description, items_arg) {
                    Ok(f) => fits_to_save.push(f),
                    Err(e) => return Some(Err(e)),
                }
            }

            let is_corp_or_alliance = service == "corpFittingMgr"
                || service == "allianceFittingMgr"
                || owner_id != char_id;
            if service == "corpFittingMgr"
                && ctx.db.with_snapshot(|s| s.corporations.contains_key(&owner_id))
            {
                let (is_ceo, roles) = get_effective_corp_roles(ctx, owner_id, char_id);
                let has_role = is_ceo || eve_db::corp_roles::has_role(roles, eve_db::corp_roles::ROLE_DIRECTOR | (1u64 << 41));
                if !has_role {
                    return Some(Err(BridgeRpcError {
                        code: "CorpRoleDenied".into(),
                        message: "Saving corporation fittings requires Fitting Manager or Director role".into(),
                    }));
                }
            }
            let max_fittings: usize = if is_corp_or_alliance { 300 } else { 250 };

            let save_res = ctx.db.try_with_mut_snapshot(|snap| {
                let current_count = snap
                    .saved_fittings
                    .values()
                    .filter(|f| f.owner_id == owner_id)
                    .count();
                if current_count + fits_to_save.len() > max_fittings {
                    return Err(BridgeRpcError {
                        code: "OwnerMaxFittings".into(),
                        message: format!(
                            "Owner {owner_id} has {current_count} fittings; saving {} more would exceed maximum of {max_fittings}",
                            fits_to_save.len()
                        ),
                    });
                }
                let mut saved_ids = Vec::with_capacity(fits_to_save.len());
                for (name, ship_type_id, description, parsed_items) in fits_to_save {
                    snap.next_entity_id += 1;
                    let id = snap.next_entity_id;
                    snap.saved_fittings.insert(
                        id,
                        SavedFittingRecord {
                            fitting_id: id,
                            owner_id,
                            name,
                            ship_type_id,
                            description,
                            items: parsed_items,
                        },
                    );
                    saved_ids.push(id);
                }
                Ok(saved_ids)
            });

            match save_res {
                Ok(saved_ids) => {
                    ctx.notify(
                        crate::router::NotificationTarget::Character(char_id),
                        "OnFittingsUpdated",
                        "charid",
                        vec![PyRep::Integer(owner_id)],
                    );
                    if method == "SaveManyFittings" {
                        Some(Ok(PyRep::List(
                            saved_ids.into_iter().map(PyRep::Integer).collect(),
                        )))
                    } else {
                        Some(Ok(PyRep::Integer(saved_ids.first().copied().unwrap_or(0))))
                    }
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("charFittingMgr", "GetFittings")
        | ("corpFittingMgr", "GetFittings")
        | ("corpFittingMgr", "GetCommunityFittings")
        | ("allianceFittingMgr", "GetFittings") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let default_owner = match service {
                "corpFittingMgr" => session.corporation_id.unwrap_or(char_id),
                "allianceFittingMgr" => session
                    .alliance_id
                    .or(session.corporation_id)
                    .unwrap_or(char_id),
                _ => char_id,
            };
            let owner_id = args
                .first()
                .and_then(|a| a.as_i64())
                .filter(|&oid| oid > 0)
                .unwrap_or(default_owner);
            let mut rows: Vec<Vec<PyRep>> = ctx.db.with_snapshot(|snap| {
                let mut fittings: Vec<_> = snap
                    .saved_fittings
                    .values()
                    .filter(|f| f.owner_id == owner_id || (method == "GetCommunityFittings" && f.owner_id == 1))
                    .collect();
                fittings.sort_by_key(|f| f.fitting_id);
                fittings
                    .into_iter()
                    .map(|f| {
                        let item_tuples: Vec<PyRep> = f
                            .items
                            .iter()
                            .map(|&(tid, flg, qty)| {
                                PyRep::Tuple(vec![
                                    PyRep::Integer(tid as i64),
                                    PyRep::Integer(flg as i64),
                                    PyRep::Integer(qty as i64),
                                ])
                            })
                            .collect();
                        vec![
                            PyRep::Integer(f.fitting_id),
                            PyRep::String(f.name.clone()),
                            PyRep::Integer(f.ship_type_id as i64),
                            PyRep::String(f.description.clone()),
                            PyRep::List(item_tuples),
                        ]
                    })
                    .collect()
            });
            if method == "GetCommunityFittings" && rows.is_empty() {
                rows.push(vec![
                    PyRep::Integer(1),
                    PyRep::String("Community Starter Rifter".into()),
                    PyRep::Integer(587),
                    PyRep::String("CCP-curated community frigate fitting".into()),
                    PyRep::List(vec![
                        PyRep::Tuple(vec![PyRep::Integer(487), PyRep::Integer(27), PyRep::Integer(1)]),
                        PyRep::Tuple(vec![PyRep::Integer(438), PyRep::Integer(19), PyRep::Integer(1)]),
                    ]),
                ]);
                rows.push(vec![
                    PyRep::Integer(2),
                    PyRep::String("Community Starter Venture".into()),
                    PyRep::Integer(32880),
                    PyRep::String("CCP-curated community mining frigate fitting".into()),
                    PyRep::List(vec![
                        PyRep::Tuple(vec![PyRep::Integer(483), PyRep::Integer(27), PyRep::Integer(1)]),
                        PyRep::Tuple(vec![PyRep::Integer(483), PyRep::Integer(28), PyRep::Integer(1)]),
                    ]),
                ]);
            }
            Some(Ok(PyRep::rowset(
                vec!["fittingID", "name", "shipTypeID", "description", "items"],
                rows,
            )))
        }

        ("charFittingMgr", "UpdateNameAndDescription") | ("corpFittingMgr", "UpdateNameAndDescription") | ("allianceFittingMgr", "UpdateNameAndDescription") | ("charFittingMgr", "UpdateFitting") | ("corpFittingMgr", "UpdateFitting") | ("allianceFittingMgr", "UpdateFitting") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            // Supports (fittingID, ownerID, name, description) or (fittingID, name, description)
            let fid = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let (owner_filter, new_name, new_desc) = if args.len() >= 4 {
                (
                    args.get(1).and_then(|a| a.as_i64()),
                    args.get(2).and_then(|a| a.as_str()).unwrap_or("").to_string(),
                    args.get(3).and_then(|a| a.as_str()).unwrap_or("").to_string(),
                )
            } else {
                (
                    None,
                    args.get(1).and_then(|a| a.as_str()).unwrap_or("").to_string(),
                    args.get(2).and_then(|a| a.as_str()).unwrap_or("").to_string(),
                )
            };
            if new_name.trim().is_empty() {
                return Some(Err(BridgeRpcError {
                    code: "InvalidFittingName".into(),
                    message: "Fitting name cannot be empty".into(),
                }));
            }
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(fit) = snap.saved_fittings.get_mut(&fid) else {
                    return Err(BridgeRpcError {
                        code: "FittingNotFound".into(),
                        message: format!("Saved fitting {fid} not found"),
                    });
                };
                let allowed_owner = fit.owner_id == char_id
                    || Some(fit.owner_id) == session.corporation_id
                    || Some(fit.owner_id) == session.alliance_id;
                if !allowed_owner
                    || owner_filter.is_some_and(|oid| oid > 0 && oid != fit.owner_id)
                {
                    return Err(BridgeRpcError {
                        code: "FittingOwnershipViolation".into(),
                        message: format!("Not authorized to update fitting {fid}"),
                    });
                }
                fit.name = new_name.clone();
                fit.description = new_desc.clone();
                Ok(fit.owner_id)
            });
            match res {
                Ok(owner_id) => {
                    ctx.notify(
                        crate::router::NotificationTarget::Character(char_id),
                        "OnFittingsUpdated",
                        "charid",
                        vec![PyRep::Integer(owner_id)],
                    );
                    Some(Ok(PyRep::keyval([
                        ("fittingID", PyRep::Integer(fid)),
                        ("ownerID", PyRep::Integer(owner_id)),
                        ("name", PyRep::String(new_name)),
                        ("description", PyRep::String(new_desc)),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("charFittingMgr", "DeleteFitting") | ("corpFittingMgr", "DeleteFitting") | ("allianceFittingMgr", "DeleteFitting") | ("charFittingMgr", "DeleteManyFittings") | ("corpFittingMgr", "DeleteManyFittings") | ("allianceFittingMgr", "DeleteManyFittings") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let default_owner = match service {
                "corpFittingMgr" => session.corporation_id.unwrap_or(char_id),
                "allianceFittingMgr" => session
                    .alliance_id
                    .or(session.corporation_id)
                    .unwrap_or(char_id),
                _ => char_id,
            };
            if method == "DeleteManyFittings" {
                let (owner_filter, list_arg) = match (args.first(), args.get(1)) {
                    (Some(PyRep::Integer(oid)), Some(list @ (PyRep::List(_) | PyRep::Tuple(_)))) => {
                        (Some(*oid), Some(list))
                    }
                    (Some(list @ (PyRep::List(_) | PyRep::Tuple(_))), _) => (None, Some(list)),
                    _ => (None, None),
                };
                let target_ids: Vec<i64> = match list_arg {
                    Some(PyRep::List(v) | PyRep::Tuple(v)) => {
                        v.iter().filter_map(|x| x.as_i64()).collect()
                    }
                    _ => Vec::new(),
                };
                let deleted_ids = ctx
                    .db
                    .try_with_mut_snapshot(|snap| {
                        let mut removed = Vec::new();
                        for fid in target_ids {
                            let can_delete = snap.saved_fittings.get(&fid).is_some_and(|f| {
                                let owner_ok = f.owner_id == char_id
                                    || Some(f.owner_id) == session.corporation_id
                                    || Some(f.owner_id) == session.alliance_id;
                                owner_ok
                                    && owner_filter
                                        .map(|oid| oid <= 0 || oid == f.owner_id)
                                        .unwrap_or(true)
                            });
                            if can_delete {
                                snap.saved_fittings.remove(&fid);
                                removed.push(fid);
                            }
                        }
                        Ok::<_, std::convert::Infallible>(removed)
                    })
                    .unwrap();
                ctx.notify(
                    crate::router::NotificationTarget::Character(char_id),
                    "OnFittingsUpdated",
                    "charid",
                    vec![PyRep::Integer(owner_filter.unwrap_or(default_owner))],
                );
                Some(Ok(PyRep::List(
                    deleted_ids.into_iter().map(PyRep::Integer).collect(),
                )))
            } else {
                // Supports DeleteFitting(ownerID, fittingID) and DeleteFitting(fittingID)
                let (owner_filter, fid) = if args.len() >= 2 {
                    (
                        args.first().and_then(|a| a.as_i64()),
                        args.get(1).and_then(|a| a.as_i64()).unwrap_or(0),
                    )
                } else {
                    (None, args.first().and_then(|a| a.as_i64()).unwrap_or(0))
                };
                if service == "corpFittingMgr" {
                    let target_corp_id = owner_filter.or(session.corporation_id).unwrap_or(0);
                    if ctx.db.with_snapshot(|s| s.corporations.contains_key(&target_corp_id)) {
                        let (is_ceo, roles) = get_effective_corp_roles(ctx, target_corp_id, char_id);
                        let has_role = is_ceo || eve_db::corp_roles::has_role(roles, eve_db::corp_roles::ROLE_DIRECTOR | (1u64 << 41));
                        if !has_role {
                            return Some(Err(BridgeRpcError {
                                code: "CorpRoleDenied".into(),
                                message: "Deleting corporation fittings requires Fitting Manager or Director role".into(),
                            }));
                        }
                    }
                }
                let deleted = ctx
                    .db
                    .try_with_mut_snapshot(|snap| {
                        let can_delete = snap.saved_fittings.get(&fid).is_some_and(|f| {
                            let owner_ok = f.owner_id == char_id
                                || Some(f.owner_id) == session.corporation_id
                                || Some(f.owner_id) == session.alliance_id;
                            owner_ok
                                && owner_filter
                                    .map(|oid| oid <= 0 || oid == f.owner_id)
                                    .unwrap_or(true)
                        });
                        if can_delete {
                            snap.saved_fittings.remove(&fid);
                            Ok::<_, std::convert::Infallible>(true)
                        } else {
                            Ok::<_, std::convert::Infallible>(false)
                        }
                    })
                    .unwrap();
                if deleted {
                    ctx.notify(
                        crate::router::NotificationTarget::Character(char_id),
                        "OnFittingsUpdated",
                        "charid",
                        vec![PyRep::Integer(owner_filter.unwrap_or(default_owner))],
                    );
                }
                Some(Ok(PyRep::Bool(deleted)))
            }
        }


        _ => None,
    }
}
