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
    pub remote_optimal: f64,
    pub remote_falloff: f64,
    pub mutadaptive_spool_step: f64,
    pub mutadaptive_spool_max: f64,
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn prepare_ewar_and_capital_module(
    ctx: &ServerContext,
    sys: &crate::system_actor::SystemHandle, sys_id: u32,
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
                if is_mwd {
                    if let Ok(Some((_, sb))) = sys.get_ball_snapshot(ship_id).await {
                        if let Err(e) =
                            crate::services::pve_dungeon_runtime::check_deadspace_mwd_allowed(
                                ctx,
                                sys_id,
                                sb.position.to_array(),
                            )
                        {
                            return Some(Err(e));
                        }
                    }
                }
                let mut warp_scramble_points = 0i32;
                let mut disables_target_mwd_mjd = false;
                let mut warp_core_stab_bonus = 0i32;
                let mut hic_bubble_radius_m: Option<f64> = None;
                let mut hic_velocity_multiplier: Option<f64> = None;
                let mut hic_sig_multiplier: Option<f64> = None;
                let mut hic_mass_addition: Option<f64> = None;
                let mut mjd_jump_distance_m: Option<f64> = None;
                let grants_interdiction_nullification = mod_type.group_id == 4117;
                if grants_interdiction_nullification {
                    // Interdiction Nullifiers (`group 4117`) can only be activated on Interceptors (831),
                    // Blockade Runners (1202), Deep Space Transports (380), Covert Ops (830), Stealth Bombers (834),
                    // Force Recons (833), Combat Recons (906), Strategic Cruisers (963), Expedition Frigates (1283/2001),
                    // Luxury Yachts (1534), or hulls explicitly listed in `canFitShipGroup*`/`canFitShipType*`.
                    let mut hull_allowed = matches!(
                        ship_group_id,
                        831 | 1202 | 380 | 830 | 834 | 833 | 906 | 963 | 1283 | 2001 | 1534
                    );
                    if !hull_allowed {
                        for aid in [1298u16, 1299, 1300, 1301, 1872, 1879, 1880, 1881] {
                            if let Some(&g) = mod_type.default_attributes.get(&aid) {
                                if g as u32 == ship_group_id {
                                    hull_allowed = true;
                                }
                            }
                        }
                        for aid in [1302u16, 1303, 1304, 1305] {
                            if let Some(&t) = mod_type.default_attributes.get(&aid) {
                                if t as u32 == fit.ship.type_id {
                                    hull_allowed = true;
                                }
                            }
                        }
                    }
                    if !hull_allowed {
                        return Some(Err(BridgeRpcError {
                            code: "InvalidShipGroup".into(),
                            message: format!(
                                "Interdiction Nullifier cannot be activated on hull group {ship_group_id}"
                            ),
                        }));
                    }
                    if let Ok(Some((_, sb))) = sys.get_ball_snapshot(ship_id).await {
                        if matches!(
                            sb.mode,
                            eve_destiny::MovementMode::Warp { phase, .. }
                                if phase != eve_destiny::WarpPhase::Aligning
                        ) {
                            return Some(Err(BridgeRpcError {
                                code: "CannotActivateInWarp".into(),
                                message: "Interdiction Nullifier must be activated before entering the warp tunnel".into(),
                            }));
                        }
                    }
                }
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
                let is_cehe = matches!(m.type_id, 40714..=40718) || mod_type.effect_ids.contains(&6484);
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
                    if !matches!(ship_group_id, 894 | 4902) && fit.ship.type_id != 89_607 {
                        return Some(Err(BridgeRpcError {
                            code: "InvalidShipGroup".into(),
                            message: "Zero-Point Mass Entangler can only be activated on Heavy Interdiction Cruisers (894) or Odysseus (89607)".into(),
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
                            .filter(|b| {
                                b.id > 0
                                    && (ctx.sde.types.get(&b.type_id).is_some_and(|t| t.category_id == 6)
                                        || ctx.db.with_snapshot(|s| {
                                            s.items
                                                .get(&b.id)
                                                .and_then(|it| ctx.sde.types.get(&it.type_id))
                                                .is_some_and(|t| t.category_id == 6)
                                        }))
                            })
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
                    if target_id == ship_id {
                        return Some(Err(BridgeRpcError {
                            code: "CannotActivateOnSelf".into(),
                            message: "Cannot activate warp disruptor/scrambler on self".into(),
                        }));
                    }
                    let scram_eval = eve_dogma::eval_warp_scrambler_effects(&[&mod_state], &[0.0], &ctx.sde);
                    warp_scramble_points = scram_eval.scramble_strength.max(1);
                    disables_target_mwd_mjd = scram_eval.blocks_mwd;
                    let mut max_range = mod_state.get_attribute(attr_ids::MAX_RANGE, &ctx.sde);
                    if max_range <= 0.0 {
                        max_range = mod_type.default_attributes.get(&54).copied().unwrap_or(20_000.0);
                    }
                    let has_cat5_overload = mod_type.effect_ids.iter().any(|&eid| {
                        ctx.sde.effects.get(&eid).is_some_and(|e| e.effect_category == 5)
                    });
                    if is_overloaded && !has_cat5_overload {
                        let ov_bonus = mod_type.default_attributes.get(&1208).copied().unwrap_or(20.0);
                        max_range *= 1.0 + ov_bonus / 100.0;
                    }
                    if disables_target_mwd_mjd {
                        max_range *= crate::services::security_mgr::insurgency_suppression_web_scram_range_multiplier(
                            ctx, char_id, sys_id,
                        );
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
                    if target_id == ship_id {
                        return Some(Err(BridgeRpcError {
                            code: "CannotActivateOnSelf".into(),
                            message: "Cannot activate offensive EWAR module on self".into(),
                        }));
                    }
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
                        if mod_type.group_id == 65 {
                            let web_range_mult = crate::services::security_mgr::insurgency_suppression_web_scram_range_multiplier(
                                ctx, char_id, sys_id,
                            );
                            if web_range_mult > 1.0 {
                                max_range *= web_range_mult;
                                proj_mod_state.base_attributes.insert(54, max_range);
                            }
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
                        ship_id,
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
                    if mod_type.group_id == 379 {
                        if let Ok(Some(mut t_cs)) = sys.get_combat_state(target_id).await {
                            let base_sig = match t_cs.base_signature_radius {
                                Some(b) => b,
                                None => sys.get_ball_snapshot(target_id)
                                    .await
                                    .ok()
                                    .flatten()
                                    .and_then(|(_, b)| {
                                        ctx.sde
                                            .types
                                            .get(&b.type_id)
                                            .and_then(|t| t.default_attributes.get(&552).copied())
                                            .or(Some(b.radius.max(35.0)))
                                    })
                                    .unwrap_or(t_cs.signature_radius),
                            };
                            t_cs.signature_radius =
                                ctx.effective_target_signature_radius(target_id, base_sig);
                            let _ = sys.set_combat_state(target_id, t_cs).await;
                        }
                    }
                }

                // Remote Shield Booster (`41`), Remote Armor Repairer (`325`), Mutadaptive Remote Armor Repairer (`2018`),
                // Remote Hull Repairer (`585`), Remote Capacitor Transmitter (`67`) (`feat:modules.remote-*`)
                let mut remote_shield_amount = 0.0;
                let mut remote_armor_amount = 0.0;
                let mut remote_hull_amount = 0.0;
                let mut remote_cap_amount = 0.0;
                let mut remote_optimal = 0.0;
                let mut remote_falloff = 0.0;
                let mut mutadaptive_spool_step = 0.0;
                let mut mutadaptive_spool_max = 0.0;
                if matches!(mod_type.group_id, 41 | 325 | 2018 | 585 | 67 | 1697 | 1698) {
                    let Some(target_id) = target_id_opt else {
                        return Some(Err(BridgeRpcError {
                            code: "TargetRequired".into(),
                            message: "Remote assistance module requires a locked target".into(),
                        }));
                    };
                    if target_id == ship_id {
                        return Some(Err(BridgeRpcError {
                            code: "CannotActivateOnSelf".into(),
                            message: "Cannot activate remote assistance module on self".into(),
                        }));
                    }
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
                    let falloff = eve_dogma::get_module_falloff(&mod_state, &ctx.sde);
                    remote_optimal = max_range;
                    remote_falloff = falloff;
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
                    }
                    let has_cat5_overload = mod_type.effect_ids.iter().any(|&eid| {
                        ctx.sde.effects.get(&eid).is_some_and(|e| e.effect_category == 5)
                    });
                    let ov_m = if is_overloaded && !has_cat5_overload { 1.20 } else { 1.0 };
                    let rr_imp_m = crate::services::tactical_modules::incoming_remote_repair_multiplier(ctx, target_id);
                    match mod_type.group_id {
                        41 | 1697 => {
                            remote_shield_amount = mod_state.get_attribute(68, &ctx.sde) * ov_m * rr_imp_m;
                        }
                        325 | 2018 | 1698 => {
                            remote_armor_amount = mod_state.get_attribute(84, &ctx.sde) * ov_m * rr_imp_m;
                            if mod_type.group_id == 2018 {
                                mutadaptive_spool_step = mod_type.default_attributes.get(&2796).copied().unwrap_or(0.10);
                                mutadaptive_spool_max = mod_type.default_attributes.get(&2797).copied().unwrap_or(1.50);
                            }
                        }
                        585 => {
                            remote_hull_amount = mod_state.get_attribute(83, &ctx.sde) * ov_m * rr_imp_m;
                        }
                        67 => {
                            remote_cap_amount = mod_state.get_attribute(90, &ctx.sde) * ov_m;
                        }
                        _ => {}
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
                                && b.id > 0
                                && (ctx.sde.types.get(&b.type_id).is_some_and(|t| t.category_id == 6)
                                    || ctx.db.with_snapshot(|s| {
                                        s.items
                                            .get(&b.id)
                                            .and_then(|it| ctx.sde.types.get(&it.type_id))
                                            .is_some_and(|t| t.category_id == 6)
                                    }))
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
                                    ship_id,
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
        remote_optimal,
        remote_falloff,
        mutadaptive_spool_step,
        mutadaptive_spool_max,
    }))
}

pub(crate) fn ewar_jamming_type(
    mod_type: &eve_sde::InvType,
    charge_type_id: Option<u32>,
    disables_target_mwd_mjd: bool,
    hic_bubble_radius_m: Option<f64>,
) -> Option<&'static str> {
    match mod_type.group_id {
        52 => Some(if disables_target_mwd_mjd {
            "warpScramblerMWD"
        } else {
            "warpScrambler"
        }),
        899 if hic_bubble_radius_m.is_none() => Some(if charge_type_id == Some(45010) {
            "warpScramblerMWD"
        } else {
            "focusedWarpScrambler"
        }),
        65 | 1672 => Some("webify"),
        201 => Some("electronic"),
        208 => Some("ewRemoteSensorDamp"),
        291 => Some(
            if mod_type.effect_ids.contains(&6425) || mod_type.name.contains("Guidance") {
                "ewGuidanceDisrupt"
            } else {
                "ewTrackingDisrupt"
            },
        ),
        379 => Some("ewTargetPaint"),
        71 => Some("ewEnergyNeut"),
        68 => Some("ewEnergyVampire"),
        _ => None,
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_committed_ewar_and_system_wide_notifications(
    ctx: &ServerContext,
    sys_id: u32,
    char_id: i64,
    ship_id: i64,
    module_id: i64,
    module_type_id: u32,
    mod_type: &eve_sde::InvType,
    charge_type_id: Option<u32>,
    disables_target_mwd_mjd: bool,
    hic_bubble_radius_m: Option<f64>,
    target_id_opt: Option<i64>,
) {
    if mod_type.group_id == 1815 {
        let effect_id = mod_type.effect_ids.first().copied().unwrap_or(6753) as i64;
        let sw_args = vec![
            PyRep::Integer(effect_id),
            PyRep::Integer(module_id),
            PyRep::Integer(i64::from(module_type_id)),
        ];
        let sent = ctx.notify(
            crate::router::NotificationTarget::SolarSystem(sys_id),
            "OnSystemWideEffectStart",
            "solarsystemid2",
            sw_args.clone(),
        );
        if !sent.iter().any(|n| n.recipient_char_id == char_id) {
            ctx.notify(
                crate::router::NotificationTarget::Character(char_id),
                "OnSystemWideEffectStart",
                "charid",
                sw_args,
            );
        }
    }

    let Some(target_id) = target_id_opt.filter(|&tid| tid > 0 && tid != ship_id) else {
        return;
    };
    let Some(jam_type) = ewar_jamming_type(
        mod_type,
        charge_type_id,
        disables_target_mwd_mjd,
        hic_bubble_radius_m,
    ) else {
        return;
    };

    let ewar_args = vec![
        PyRep::Integer(ship_id),
        PyRep::Integer(module_id),
        PyRep::Integer(target_id),
        PyRep::String(jam_type.into()),
    ];
    ctx.active_ewar_starts
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .entry((ship_id, module_id))
        .or_default()
        .push((target_id, jam_type));
    ctx.notify(
        crate::router::NotificationTarget::Character(char_id),
        "OnEwarStart",
        "charid",
        ewar_args.clone(),
    );
    let target_owner = ctx
        .db
        .with_snapshot(|s| s.items.get(&target_id).map(|i| i.owner_id))
        .unwrap_or(0);
    if target_owner > 0 && target_owner != char_id {
        ctx.notify(
            crate::router::NotificationTarget::Character(target_owner),
            "OnEwarStart",
            "charid",
            ewar_args,
        );
    }
}
