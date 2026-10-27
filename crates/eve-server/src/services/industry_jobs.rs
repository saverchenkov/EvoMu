//! `industry_jobs` — Industry job installation, BOM consumption, ME/TE/Copy/Invention/Decryptor/T3/Reaction execution, & job monitor (`E-4`).

// parity: plan:E-4
// parity: feat:industry.install-job, feat:industry.manufacturing, feat:industry.research-me, feat:industry.research-te
// parity: feat:industry.copying, feat:industry.invention, feat:industry.decryptors, feat:industry.t3-invention
// parity: feat:industry.reactions, feat:industry.job-complete, feat:industry.job-cancel, feat:industry.job-list
// parity: feat:industry.job-slots, feat:industry.job-range, feat:industry.industry-monitor, feat:industry.corp-industry
// parity: feat:industry.blueprint-lock

#![forbid(unsafe_code)]

use crate::formulas::{
    compute_invention_probability, compute_manufacturing_material_qty,
    compute_manufacturing_time_seconds,
};
use crate::router::ServerContext;
use crate::services::industry_pi::{
    ATTR_BLUEPRINT_ME, ATTR_BLUEPRINT_TE, compute_industry_job_costs_for_char,
    emit_industry_job, handle_industry_job_queries, has_custom_facility_taxes,
    is_fulcrum_pirate_ship_bonus_applicable, is_structure_industry_service_online,
    load_job_cost_meta, resolve_decryptor_modifiers, resolve_facility_modifiers_and_system,
    resolve_structure_rig_bonus, restore_blueprint_flag, save_job_cost_meta,
    solar_system_jump_distance, sync_industry_job_pause_states,
};
use eve_db::{IndustryJobRecord, InventoryFlag, InventoryItemRecord, IskCents};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::TypeId;
use std::collections::HashMap;

pub fn handle_industry_jobs(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    if let Some(res) = handle_industry_job_queries(ctx, session, service, method, args, kwargs) {
        return Some(res);
    }
    match (service, method) {
        ("industryManager", "InstallJob") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(docked_id) = session.station_id.or(session.structure_id) else {
                return Some(Err(BridgeRpcError {
                    code: "NotDocked".into(),
                    message: "Installing an industry job requires being docked".into(),
                }));
            };

            let mut is_v24_call = false;
            let mut explicit_bp_item_id: Option<i64> = None;
            let mut explicit_facility_id: Option<i64> = None;
            let lookup_first = |k: &str| -> Option<&PyRep> {
                match args.first() {
                    Some(PyRep::KeyVal(kv)) => kv.get(k),
                    Some(PyRep::Dict(entries)) => entries
                        .iter()
                        .find(|(ek, _)| ek.as_str() == Some(k))
                        .map(|(_, ev)| ev),
                    _ => None,
                }
                .or_else(|| kwargs.get(k))
            };
            let decryptor_type_id: Option<TypeId> = lookup_first("decryptorTypeID")
                .or_else(|| lookup_first("decryptorID"))
                .and_then(|v| v.as_i64())
                .filter(|&id| id > 0)
                .map(|id| id as TypeId);
            let mut use_corp: bool = lookup_first("useCorp")
                .or_else(|| lookup_first("isCorpJob"))
                .and_then(|v| v.as_bool().or_else(|| v.as_i64().map(|n| n != 0)))
                .unwrap_or(false);
            let explicit_owner_id: Option<i64> = lookup_first("ownerID")
                .or_else(|| lookup_first("corporationID"))
                .and_then(|v| v.as_i64())
                .filter(|&id| id > 0);

            let (raw_bp_type_id, raw_activity_id, runs_raw) = match args.first() {
                Some(PyRep::KeyVal(_) | PyRep::Dict(_)) => {
                    is_v24_call = true;
                    explicit_bp_item_id = lookup_first("blueprintID")
                        .or_else(|| lookup_first("blueprintItemID"))
                        .and_then(|v| v.as_i64())
                        .filter(|&id| id > 0);
                    explicit_facility_id = lookup_first("facilityID")
                        .and_then(|v| v.as_i64())
                        .filter(|&id| id > 0);
                    let btid = lookup_first("blueprintTypeID").and_then(|v| v.as_i64()).unwrap_or(0);
                    let act = lookup_first("activityID").and_then(|v| v.as_i64()).unwrap_or(1);
                    let r = lookup_first("runs").and_then(|v| v.as_i64()).unwrap_or(1);
                    (Some(btid), act, r)
                }
                _ if args.len() >= 4 => {
                    is_v24_call = true;
                    explicit_bp_item_id = args.first().and_then(|a| a.as_i64()).filter(|&id| id > 0);
                    explicit_facility_id = args.get(4).and_then(|a| a.as_i64()).filter(|&id| id > 0);
                    (
                        args.get(1).and_then(|a| a.as_i64()),
                        args.get(2).and_then(|a| a.as_i64()).unwrap_or(1),
                        args.get(3).and_then(|a| a.as_i64()).unwrap_or(1),
                    )
                }
                _ => (
                    args.first().and_then(|a| a.as_i64()),
                    args.get(1).and_then(|a| a.as_i64()).unwrap_or(1),
                    args.get(2).and_then(|a| a.as_i64()).unwrap_or(1),
                ),
            };
            let station_id = explicit_facility_id.unwrap_or(docked_id);

            let resolved_bp_type_id = raw_bp_type_id.filter(|&id| id > 0).or_else(|| {
                explicit_bp_item_id.and_then(|iid| {
                    ctx.db.with_snapshot(|snap| snap.items.get(&iid).map(|it| it.type_id as i64))
                })
            });
            let Some(raw_bp_tid) = resolved_bp_type_id else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidIndustryJobParameters".into(),
                    message: "InstallJob requires an explicit blueprint typeID or blueprintItemID".into(),
                }));
            };
            let (Ok(bp_type_id), Ok(activity_id)) =
                (TypeId::try_from(raw_bp_tid), u8::try_from(raw_activity_id))
            else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidIndustryJobParameters".into(),
                    message: "Invalid blueprint typeID or activityID".into(),
                }));
            };

            if !(1..=1_000).contains(&runs_raw) || !matches!(activity_id, 1 | 3 | 4 | 5 | 8 | 9) {
                return Some(Err(BridgeRpcError {
                    code: "InvalidIndustryJobParameters".into(),
                    message: "Runs must be in 1..=1000 and activityID in {1, 3, 4, 5, 8, 9}".into(),
                }));
            }
            let (runs, runs_i32) = (runs_raw as u32, runs_raw as i32);

            let Some(bp) = ctx.sde.blueprints.get(&bp_type_id) else {
                return Some(Err(BridgeRpcError {
                    code: "UnknownBlueprint".into(),
                    message: format!("Blueprint type {bp_type_id} not found in SDE"),
                }));
            };
            let is_ancient_relic = ctx.sde.types.get(&bp_type_id).map(|t| t.group_id == 971).unwrap_or(false);

            // Enforce remote job range (`feat:industry.job-range`) when `station_id != docked_id`
            if station_id != docked_id {
                let current_sys = session.solar_system_id.unwrap_or(30_000_142);
                let range_check = ctx.db.with_snapshot(|snap| {
                    let Some((_, _, f_sys, _, _, _, _)) =
                        resolve_facility_modifiers_and_system(ctx, snap, station_id)
                    else {
                        return Err(BridgeRpcError {
                            code: "FacilityNotFound".into(),
                            message: format!("Industry facility {station_id} not found"),
                        });
                    };
                    if f_sys == current_sys {
                        return Ok(());
                    }
                    let jump_dist = solar_system_jump_distance(ctx, current_sys, f_sys).unwrap_or(999);
                    let remote_skill_id: TypeId = match activity_id {
                        1 => 24268, // Supply Chain Management
                        9 => 45750, // Remote Reactions
                        _ => 24270, // Scientific Networking
                    };
                    let remote_lvl = snap
                        .skills
                        .get(&char_id)
                        .and_then(|m| m.get(&remote_skill_id))
                        .map(|s| s.level)
                        .unwrap_or(0);
                    let max_jumps = u32::from(remote_lvl) * 5;
                    if jump_dist > max_jumps {
                        return Err(BridgeRpcError {
                            code: "FacilityOutOfRemoteRange".into(),
                            message: format!(
                                "Facility {station_id} in system {f_sys} is {jump_dist} jumps away (max {max_jumps})"
                            ),
                        });
                    }
                    Ok(())
                });
                if let Err(e) = range_check {
                    return Some(Err(e));
                }
            }

            let now_unix = ctx.now_unix();

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                sync_industry_job_pause_states(snap, now_unix);
                let is_alpha_clone = snap
                    .characters
                    .get(&char_id)
                    .is_some_and(|ch| ch.clone_grade <= 1);
                let (fac_type_id, _, fac_sys_id, _, mat_mod, time_mod, _) =
                    resolve_facility_modifiers_and_system(ctx, snap, station_id).unwrap_or((
                        1529, 1000035, 30000142, 10000002, 1.0, 1.0, 1.0,
                    ));
                if activity_id == 9 {
                    if !matches!(fac_type_id, 35835 | 35836) {
                        return Err(BridgeRpcError {
                            code: "FacilityDoesNotSupportReactions".into(),
                            message: format!(
                                "Facility {station_id} (type {fac_type_id}) does not support Reactions (requires Athanor 35835 or Tatara 35836)"
                            ),
                        });
                    }
                    let fac_sec = ctx
                        .sde
                        .solar_systems
                        .get(&fac_sys_id)
                        .map(|s| s.security_status)
                        .unwrap_or(1.0);
                    if fac_sec >= 0.45 {
                        return Err(BridgeRpcError {
                            code: "ReactionsForbiddenInHighSec".into(),
                            message: format!(
                                "Reactions cannot be run in high-sec (system {fac_sys_id} sec {fac_sec:.2} >= 0.45)"
                            ),
                        });
                    }
                }
                if !is_structure_industry_service_online(snap, station_id, activity_id) {
                    return Err(BridgeRpcError {
                        code: "FacilityServiceOffline".into(),
                        message: format!(
                            "Facility {station_id} industry/reaction service is offline or out of fuel"
                        ),
                    });
                }
                let char_corp_id = snap
                    .characters
                    .get(&char_id)
                    .map(|c| c.corporation_id)
                    .or(session.corporation_id)
                    .unwrap_or(0);
                if let Some(oid) = explicit_owner_id {
                    if !use_corp && oid == char_corp_id && char_corp_id > 0 {
                        use_corp = true;
                    }
                    let expected_owner = if use_corp { char_corp_id } else { char_id };
                    if oid != expected_owner || expected_owner <= 0 {
                        return Err(BridgeRpcError {
                            code: "OwnerAccessDenied".into(),
                            message: format!(
                                "Cannot install industry job for owner {oid} (caller {char_id}, corp {char_corp_id})"
                            ),
                        });
                    }
                }
                let job_owner_id = if use_corp {
                    if char_corp_id <= 0 {
                        return Err(BridgeRpcError {
                            code: "OwnerAccessDenied".into(),
                            message: "Character does not belong to a corporation".into(),
                        });
                    }
                    const FACTORY_ROLES: u64 = eve_db::corp_roles::ROLE_DIRECTOR
                        | eve_db::corp_roles::ROLE_FACTORY_MANAGER
                        | eve_db::corp_roles::ROLE_RENT_FACTORY_FACILITY
                        | eve_db::corp_roles::ROLE_RENT_RESEARCH_FACILITY;
                    let has_factory_role = session.has_gm_role()
                        || (session.corp_role & FACTORY_ROLES) != 0
                        || snap.corporations.get(&char_corp_id).is_some_and(|c| {
                            c.ceo_id == char_id
                                || c.member_roles
                                    .get(&char_id)
                                    .is_some_and(|&r| (r & FACTORY_ROLES) != 0)
                        })
                        || snap
                            .corp_role_assignments
                            .get(&(char_corp_id, char_id))
                            .is_some_and(|a| (a.roles & FACTORY_ROLES) != 0);
                    if !has_factory_role {
                        return Err(BridgeRpcError {
                            code: "CrpAccessDenied".into(),
                            message:
                                "Installing a corporation industry job requires Factory Manager or Director role"
                                    .into(),
                        });
                    }
                    char_corp_id
                } else {
                    char_id
                };

                let (rig_mat_bonus, _, _) =
                    resolve_structure_rig_bonus(ctx, snap, station_id, fac_sys_id);
                let fulcrum_bonus = is_fulcrum_pirate_ship_bonus_applicable(
                    ctx,
                    snap,
                    Some(char_id),
                    bp_type_id,
                    activity_id,
                    station_id,
                );
                let eff_mat_mod = if fulcrum_bonus { mat_mod * 0.94 } else { mat_mod };
                let eff_time_mod = if fulcrum_bonus { time_mod * 0.30 } else { time_mod };
                let eff_mat_bonus_pct = (1.0 - eff_mat_mod).max(0.0) * 100.0;
                let eff_time_bonus_pct = (1.0 - eff_time_mod).max(0.0) * 100.0;

                if activity_id == 1 {
                    let prod_grp = ctx
                        .sde
                        .types
                        .get(&bp.product_type_id)
                        .map(|t| t.group_id)
                        .unwrap_or(0);
                    let fac_sec = ctx
                        .sde
                        .solar_systems
                        .get(&fac_sys_id)
                        .map(|s| s.security_status)
                        .unwrap_or(1.0);
                    if matches!(prod_grp, 659 | 30) {
                        if fac_sec > 0.0 || fac_sys_id >= 31_000_000 {
                            return Err(BridgeRpcError {
                                code: "SupercapitalRequiresSovereignNullSec".into(),
                                message: format!(
                                    "Supercapitals require sovereign Null-Sec (system {fac_sys_id} sec {fac_sec:.2})"
                                ),
                            });
                        }
                        let has_sov = snap
                            .system_states
                            .get(&fac_sys_id)
                            .and_then(|s| s.sov_alliance_id)
                            .is_some();
                        let has_supercap_upgrade = snap.items.values().any(|i| {
                            i.location_id == fac_sys_id as i64
                                && matches!(i.type_id, 81623 | 2009)
                                && i.dynamic_attributes.get(&2).copied().unwrap_or(1.0) > 0.0
                        });
                        if !has_sov || !has_supercap_upgrade {
                            return Err(BridgeRpcError {
                                code: "SupercapitalRequiresSovereignNullSec".into(),
                                message: format!(
                                    "System {fac_sys_id} requires active sovereignty and an online Supercapital Construction Facilities upgrade"
                                ),
                            });
                        }
                        let has_supercap_shipyard = fac_type_id == 35827
                            && snap.items.values().any(|i| {
                                i.location_id == station_id
                                    && (164..=171).contains(&(i.flag as u16))
                                    && i.type_id == 35877
                                    && i.dynamic_attributes.get(&2).copied().unwrap_or(1.0) > 0.0
                            });
                        if !has_supercap_shipyard {
                            return Err(BridgeRpcError {
                                code: "SupercapitalRequiresSotiyoSupercapitalShipyard".into(),
                                message: format!(
                                    "Supercapitals require a Sotiyo (35827) with an online Standup Supercapital Shipyard I (35877) at {station_id}"
                                ),
                            });
                        }
                    } else if matches!(prod_grp, 485 | 4594 | 547 | 5120 | 1538 | 883 | 902 | 513) {
                        if fac_sec >= 0.45 && fac_sys_id < 31_000_000 {
                            return Err(BridgeRpcError {
                                code: "CapitalConstructionForbiddenInHighSec".into(),
                                message: format!(
                                    "Capital ship construction is forbidden in High-Sec (system {fac_sys_id} sec {fac_sec:.2})"
                                ),
                            });
                        }
                        let has_cap_shipyard = matches!(fac_type_id, 35826 | 35827 | 35833 | 35834)
                            || snap.items.values().any(|i| {
                                i.location_id == station_id
                                    && (164..=171).contains(&(i.flag as u16))
                                    && matches!(i.type_id, 35881 | 35877)
                                    && i.dynamic_attributes.get(&2).copied().unwrap_or(1.0) > 0.0
                            });
                        if !has_cap_shipyard {
                            return Err(BridgeRpcError {
                                code: "FacilityNotCapitalCapable".into(),
                                message: format!(
                                    "Facility {station_id} (type {fac_type_id}) is not a capital-capable shipyard facility"
                                ),
                            });
                        }
                    }
                }

                let char_skills = snap.skills.get(&char_id);
                let skill_lvl = |sid: TypeId| -> u8 {
                    char_skills.and_then(|m| m.get(&sid)).map(|s| s.level).unwrap_or(0)
                };
                let industry_lvl = skill_lvl(3380);
                let adv_industry_lvl = skill_lvl(3388);
                let mass_prod_lvl = skill_lvl(3387);
                let adv_mass_prod_lvl = skill_lvl(24625);
                let lab_op_lvl = skill_lvl(3406);
                let adv_lab_op_lvl = skill_lvl(24624);
                let reactions_lvl = skill_lvl(45746);
                let mass_react_lvl = skill_lvl(45748);
                let adv_mass_react_lvl = skill_lvl(45749);
                let sci1_lvl = skill_lvl(11448).max(skill_lvl(11454)).max(skill_lvl(20433));
                let sci2_lvl = skill_lvl(11452).max(skill_lvl(11442));
                let enc_lvl = skill_lvl(21791).max(skill_lvl(23121)).max(skill_lvl(3408));

                let max_slots = match activity_id {
                    1 => 1 + (mass_prod_lvl as usize) + (adv_mass_prod_lvl as usize),
                    9 => 1 + (mass_react_lvl as usize) + (adv_mass_react_lvl as usize),
                    _ => 1 + (lab_op_lvl as usize) + (adv_lab_op_lvl as usize),
                };
                let active_slots_used = snap
                    .industry_jobs
                    .values()
                    .filter(|j| {
                        j.installer_id == char_id
                            && matches!(j.status.as_str(), "Active" | "Paused")
                            && match activity_id {
                                1 => j.activity_id == 1,
                                9 => j.activity_id == 9,
                                _ => matches!(j.activity_id, 3 | 4 | 5 | 8),
                            }
                    })
                    .count();
                if active_slots_used >= max_slots {
                    return Err(BridgeRpcError {
                        code: "IndustrySlotsFull".into(),
                        message: format!(
                            "Character {char_id} has used all {active_slots_used}/{max_slots} slots for activity {activity_id}"
                        ),
                    });
                }

                let is_usable_industry_flag = |flag: InventoryFlag| -> bool {
                    flag != InventoryFlag::Locked
                        && !matches!(
                            flag as u16,
                            11..=34 | 36 | 92..=94 | 125..=132 | 158 | 164..=171
                        )
                };

                let matching_bps: Vec<InventoryItemRecord> = snap
                    .items
                    .values()
                    .filter(|i| {
                        (i.owner_id == char_id || (use_corp && i.owner_id == job_owner_id))
                            && (i.location_id == station_id || i.location_id == docked_id)
                            && i.type_id == bp_type_id
                            && explicit_bp_item_id.map(|eid| i.item_id == eid).unwrap_or(true)
                            && i.quantity >= 1
                            && is_usable_industry_flag(i.flag)
                            && !snap.is_item_locked_in_contract_or_job(i.item_id)
                    })
                    .cloned()
                    .collect();
                if matching_bps.is_empty() {
                    return Err(BridgeRpcError {
                        code: "BlueprintNotAvailable".into(),
                        message: format!("No unlocked blueprint {bp_type_id} at facility {station_id}"),
                    });
                }

                let bp_rec = match activity_id {
                    3 | 4 | 5 => matching_bps
                        .into_iter()
                        .find(|i| !i.is_blueprint_copy)
                        .ok_or_else(|| BridgeRpcError {
                            code: "CannotResearchOrCopyBPC".into(),
                            message: "ME/TE research and copying require a BPO".into(),
                        })?,
                    8 if is_ancient_relic => matching_bps.into_iter().next().unwrap(),
                    8 => matching_bps
                        .into_iter()
                        .find(|i| i.is_blueprint_copy && i.blueprint_runs_remaining.unwrap_or(0) >= 1)
                        .ok_or_else(|| BridgeRpcError {
                            code: "InventionRequiresBPC".into(),
                            message: "T2 Invention requires a BPC with at least 1 remaining run".into(),
                        })?,
                    _ => matching_bps
                        .into_iter()
                        .find(|i| !i.is_blueprint_copy || i.blueprint_runs_remaining.unwrap_or(0) >= runs_i32)
                        .ok_or_else(|| BridgeRpcError {
                            code: "InsufficientBlueprintRuns".into(),
                            message: format!("No BPO or BPC with {runs} remaining runs available"),
                        })?,
                };
                let bp_iid = bp_rec.item_id;
                let me_level = bp_rec
                    .dynamic_attributes
                    .get(&ATTR_BLUEPRINT_ME)
                    .map(|v| (*v as i64).clamp(0, 10) as u8)
                    .unwrap_or(0);
                let te_level = bp_rec
                    .dynamic_attributes
                    .get(&ATTR_BLUEPRINT_TE)
                    .map(|v| (*v as i64).clamp(0, 20) as u8)
                    .unwrap_or(0);

                let (dec_prob_mult, dec_me_mod, dec_te_mod, dec_runs_mod) =
                    if let Some(dtid) = decryptor_type_id {
                        resolve_decryptor_modifiers(ctx, dtid)?
                    } else {
                        (1.0, 0.0, 0.0, 0)
                    };

                let (product_type_id, base_inv_runs): (TypeId, i32) = if activity_id == 8 {
                    bp.activities
                        .get("invention")
                        .or_else(|| bp.activities.get("8"))
                        .and_then(|a| a.products.first())
                        .map(|p| (p.type_id, (p.quantity as i32).max(1)))
                        .unwrap_or((11372, 1))
                } else if activity_id == 9 {
                    let r_prod = bp
                        .activities
                        .get("reaction")
                        .or_else(|| bp.activities.get("9"))
                        .and_then(|a| a.products.first())
                        .map(|p| p.type_id)
                        .unwrap_or(bp.product_type_id);
                    (r_prod, 1)
                } else {
                    (bp.product_type_id, 1)
                };

                let invented_runs = (base_inv_runs + dec_runs_mod).max(1);
                let invented_me = (2.0 + dec_me_mod).clamp(0.0, 10.0);
                let invented_te = (4.0 + dec_te_mod).clamp(0.0, 20.0);

                if activity_id == 3 && (te_level >= 20 || u32::from(te_level) + 2 * runs > 20) {
                    return Err(BridgeRpcError {
                        code: "MaxTimeEfficiencyReached".into(),
                        message: "Blueprint Time Efficiency cannot exceed 20%".into(),
                    });
                }
                if activity_id == 4 && (me_level >= 10 || u32::from(me_level) + runs > 10) {
                    return Err(BridgeRpcError {
                        code: "MaxMaterialEfficiencyReached".into(),
                        message: "Blueprint Material Efficiency cannot exceed 10%".into(),
                    });
                }
                if activity_id == 5
                    && bp.max_production_limit > 0
                    && runs > bp.max_production_limit
                {
                    return Err(BridgeRpcError {
                        code: "MaxRunsPerCopyExceeded".into(),
                        message: format!(
                            "Requested {runs} copy runs exceeds blueprint maxProductionLimit {}",
                            bp.max_production_limit
                        ),
                    });
                }

                let adv_ind_mult = 1.0 - 0.03 * f64::from(adv_industry_lvl.min(5));
                let te_skill_mult = (1.0 - 0.05 * f64::from(skill_lvl(3403).min(5))) * adv_ind_mult;
                let me_skill_mult = (1.0 - 0.05 * f64::from(skill_lvl(3409).min(5))) * adv_ind_mult;
                let copy_skill_mult =
                    (1.0 - 0.05 * f64::from(skill_lvl(3402).min(5))) * adv_ind_mult;

                let duration_sec = match activity_id {
                    1 => compute_manufacturing_time_seconds(
                        runs,
                        bp.manufacturing_time_sec,
                        te_level,
                        industry_lvl,
                        adv_industry_lvl,
                        eff_time_bonus_pct,
                    ),
                    3 => (((bp.research_te_time_sec.max(1) as f64) * te_skill_mult * eff_time_mod)
                        .round()
                        .max(1.0) as u64)
                        .saturating_mul(runs as u64),
                    4 => (((bp.research_me_time_sec.max(1) as f64) * me_skill_mult * eff_time_mod)
                        .round()
                        .max(1.0) as u64)
                        .saturating_mul(runs as u64),
                    5 => (((bp.copy_time_sec.max(1) as f64) * copy_skill_mult * eff_time_mod)
                        .round()
                        .max(1.0) as u64)
                        .saturating_mul(runs as u64),
                    8 => (((bp.invention_time_sec.max(1) as f64) * eff_time_mod).round().max(1.0) as u64)
                        .saturating_mul(runs as u64),
                    9 => {
                        let base_react_sec = bp
                            .activities
                            .get("reaction")
                            .or_else(|| bp.activities.get("9"))
                            .map(|a| a.time)
                            .unwrap_or(10_800)
                            .max(1);
                        let react_skill_factor = (1.0 - 0.04 * (reactions_lvl.min(5) as f64)).max(0.20);
                        (((base_react_sec as f64) * react_skill_factor * eff_time_mod).round().max(1.0) as u64)
                            .saturating_mul(runs as u64)
                    }
                    _ => 600,
                };
                let trit_req =
                    compute_manufacturing_material_qty(runs, 32_000, me_level, eff_mat_bonus_pct, rig_mat_bonus);
                let inv_prob = compute_invention_probability(
                    bp.invention_base_probability,
                    sci1_lvl,
                    sci2_lvl,
                    enc_lvl,
                    dec_prob_mult,
                );

                let mut cost_meta = compute_industry_job_costs_for_char(
                    ctx,
                    snap,
                    Some(char_id),
                    bp_type_id,
                    activity_id,
                    runs,
                    station_id,
                    is_alpha_clone,
                )?;
                cost_meta.decryptor_type_id = decryptor_type_id;
                cost_meta.product_type_id = product_type_id;
                cost_meta.invented_runs = invented_runs;
                cost_meta.invented_me = invented_me;
                cost_meta.invented_te = invented_te;
                cost_meta.inv_prob = inv_prob;
                cost_meta.use_corp = use_corp;
                cost_meta.owner_id = job_owner_id;

                let use_full_eiv_debit = is_v24_call || has_custom_facility_taxes(snap, station_id);
                let (charged_facility_tax_isk, total_debit_isk) = if use_full_eiv_debit {
                    (cost_meta.facility_tax_isk, cost_meta.total_cost_isk)
                } else {
                    let legacy_tax = 1_000.0 * (runs as f64);
                    (legacy_tax, legacy_tax)
                };

                let total_debit_cents = IskCents::from_f64_isk(total_debit_isk);
                if use_corp && snap.corporations.contains_key(&job_owner_id) {
                    let corp_bal = snap
                        .corporations
                        .get(&job_owner_id)
                        .map(|c| c.master_wallet)
                        .unwrap_or(IskCents(0));
                    if corp_bal < total_debit_cents {
                        return Err(BridgeRpcError {
                            code: "InsufficientFunds".into(),
                            message: format!("Insufficient corporation funds ({total_debit_isk:.2} ISK)"),
                        });
                    }
                } else if let Some(ch) = snap.characters.get(&char_id) {
                    if ch.balance < total_debit_cents {
                        return Err(BridgeRpcError {
                            code: "InsufficientFunds".into(),
                            message: format!("Insufficient funds ({total_debit_isk:.2} ISK required)"),
                        });
                    }
                }

                let mut consumed_materials: Vec<(TypeId, i32)> = Vec::new();
                let mut consume_material = |snap: &mut eve_db::WorldStateSnapshot,
                                            mat_tid: TypeId,
                                            req_qty: i32,
                                            err_code: &str|
                 -> Result<(), BridgeRpcError> {
                    if req_qty <= 0 {
                        return Ok(());
                    }
                    let mut matching_stacks: Vec<(i64, i32)> = snap
                        .items
                        .values()
                        .filter(|i| {
                            (i.owner_id == char_id || (use_corp && i.owner_id == job_owner_id))
                                && (i.location_id == station_id || i.location_id == docked_id)
                                && i.type_id == mat_tid
                                && i.quantity > 0
                                && !i.singleton
                                && is_usable_industry_flag(i.flag)
                                && !snap.is_item_locked_in_contract_or_job(i.item_id)
                        })
                        .map(|i| (i.item_id, i.quantity))
                        .collect();
                    matching_stacks.sort_by_key(|(id, _)| *id);
                    let total_avail: i64 = matching_stacks.iter().map(|(_, q)| i64::from(*q)).sum();
                    if total_avail < i64::from(req_qty) {
                        return Err(BridgeRpcError {
                            code: err_code.into(),
                            message: format!("Missing {req_qty}x material type {mat_tid} at {station_id}"),
                        });
                    }
                    let mut rem = req_qty;
                    for (mid, _) in matching_stacks {
                        if rem <= 0 {
                            break;
                        }
                        if let Some(m_item) = snap.items.get_mut(&mid) {
                            let take = m_item.quantity.min(rem);
                            if m_item.quantity > take {
                                m_item.quantity -= take;
                                m_item.version += 1;
                            } else {
                                snap.items.remove(&mid);
                            }
                            rem -= take;
                        }
                    }
                    if let Some(entry) = consumed_materials.iter_mut().find(|(t, _)| *t == mat_tid) {
                        entry.1 = entry.1.saturating_add(req_qty);
                    } else {
                        consumed_materials.push((mat_tid, req_qty));
                    }
                    Ok(())
                };

                if activity_id == 1 {
                    if industry_lvl < 1 {
                        return Err(BridgeRpcError {
                            code: "MissingIndustrySkills".into(),
                            message: "Manufacturing requires Industry (3380) >= level 1".into(),
                        });
                    }
                    for mat in &bp.materials {
                        let req_qty = compute_manufacturing_material_qty(
                            runs,
                            mat.quantity,
                            me_level,
                            eff_mat_bonus_pct,
                            rig_mat_bonus,
                        ) as i32;
                        consume_material(snap, mat.type_id, req_qty, "InsufficientManufacturingMaterials")?;
                    }
                } else if activity_id == 8 {
                    if bp.invention_base_probability <= 0.0 {
                        return Err(BridgeRpcError {
                            code: "BlueprintNotInventable".into(),
                            message: format!("Blueprint/Relic {bp_type_id} does not support invention"),
                        });
                    }
                    if sci1_lvl < 1 || sci2_lvl < 1 || enc_lvl < 1 {
                        return Err(BridgeRpcError {
                            code: "MissingInventionSkills".into(),
                            message: "Invention requires science and encryption skills >= level 1".into(),
                        });
                    }
                    let req_dc = runs as i32;
                    let sde_inv_mats: Vec<(TypeId, i32)> = bp
                        .activities
                        .get("invention")
                        .or_else(|| bp.activities.get("8"))
                        .map(|a| {
                            a.materials
                                .iter()
                                .map(|m| (m.type_id, (m.quantity as i32).max(1) * req_dc))
                                .collect()
                        })
                        .unwrap_or_default();
                    let has_sde_mats = !sde_inv_mats.is_empty()
                        && sde_inv_mats.iter().all(|(tid, q)| {
                            let avail: i64 = snap
                                .items
                                .values()
                                .filter(|i| {
                                    (i.owner_id == char_id || (use_corp && i.owner_id == job_owner_id))
                                        && (i.location_id == station_id || i.location_id == docked_id)
                                        && i.type_id == *tid
                                        && !i.singleton
                                        && is_usable_industry_flag(i.flag)
                                        && !snap.is_item_locked_in_contract_or_job(i.item_id)
                                })
                                .map(|i| i64::from(i.quantity))
                                .sum();
                            avail >= i64::from(*q)
                        });
                    if has_sde_mats {
                        for (dc_type, q) in sde_inv_mats {
                            consume_material(snap, dc_type, q, "InsufficientInventionDatacores")?;
                        }
                    } else {
                        for dc_type in [20424_u32, 20172_u32] {
                            consume_material(snap, dc_type, req_dc, "InsufficientInventionDatacores")?;
                        }
                    }
                    if let Some(dtid) = decryptor_type_id {
                        consume_material(snap, dtid, req_dc, "InsufficientDecryptors")?;
                    }
                } else if activity_id == 9 {
                    if reactions_lvl < 1 {
                        return Err(BridgeRpcError {
                            code: "MissingReactionSkills".into(),
                            message: "Reactions require Reactions skill (45746) >= level 1".into(),
                        });
                    }
                    let react_mats: Vec<(TypeId, u64)> = bp
                        .activities
                        .get("reaction")
                        .or_else(|| bp.activities.get("9"))
                        .map(|a| a.materials.iter().map(|m| (m.type_id, m.quantity)).collect())
                        .unwrap_or_default();
                    if react_mats.is_empty() {
                        return Err(BridgeRpcError {
                            code: "BlueprintNotReactionFormula".into(),
                            message: format!("Type {bp_type_id} has no SDE reaction activity materials"),
                        });
                    }
                    for (mtid, mqty) in react_mats {
                        let req_qty =
                            compute_manufacturing_material_qty(runs, mqty, 0, eff_mat_bonus_pct, rig_mat_bonus) as i32;
                        consume_material(snap, mtid, req_qty, "InsufficientReactionMaterials")?;
                    }
                }
                cost_meta.consumed_materials = consumed_materials;

                if use_corp && snap.corporations.contains_key(&job_owner_id) {
                    if let Some(corp_mut) = snap.corporations.get_mut(&job_owner_id) {
                        corp_mut.ensure_wallet_divisions();
                        corp_mut.master_wallet = corp_mut
                            .master_wallet
                            .checked_sub(total_debit_cents)
                            .unwrap_or(IskCents(0));
                        corp_mut.wallet_divisions[0].balance = corp_mut.master_wallet;
                    }
                }

                snap.next_entity_id += 1;
                let jid = snap.next_entity_id;

                if use_corp && snap.corporations.contains_key(&job_owner_id) {
                    let corp_bal = snap
                        .corporations
                        .get(&job_owner_id)
                        .map(|c| c.master_wallet)
                        .unwrap_or(IskCents(0));
                    snap.append_journal_entry(eve_db::JournalEntryInput {
                        owner_id: job_owner_id,
                        entry_type: "IndustryJobInstallationFee",
                        amount: total_debit_cents,
                        balance_after: corp_bal,
                        reason: format!("Facility/job fee for industry job {jid}"),
                        meta: eve_db::JournalMeta {
                            ref_type_id: 56,
                            first_party_id: job_owner_id,
                            second_party_id: station_id,
                            account_key: 1000,
                            date_unix: ctx.now_unix(),
                            ..Default::default()
                        },
                    });
                } else {
                    if use_full_eiv_debit {
                        let base_cents = IskCents::from_f64_isk(cost_meta.base_job_cost_isk);
                        let tax_cents = IskCents::from_f64_isk(cost_meta.facility_tax_isk);
                        let scc_cents = IskCents::from_f64_isk(cost_meta.scc_surcharge_isk);
                        let alpha_cents = IskCents::from_f64_isk(cost_meta.alpha_clone_tax_isk);
                        if let Some(ch) = snap.characters.get_mut(&char_id) {
                            ch.balance = ch.balance.checked_sub(base_cents).unwrap_or(IskCents(0));
                            let b1 = ch.balance;
                            snap.append_journal(char_id, "IndustryJobInstallationFee", base_cents, b1, format!("Base job cost for industry job {jid}"));
                        }
                        if let Some(ch) = snap.characters.get_mut(&char_id) {
                            ch.balance = ch.balance.checked_sub(tax_cents).unwrap_or(IskCents(0));
                            let b2 = ch.balance;
                            snap.append_journal(char_id, "IndustryFacilityTax", tax_cents, b2, format!("Facility tax for industry job {jid}"));
                        }
                        if let Some(ch) = snap.characters.get_mut(&char_id) {
                            ch.balance = ch.balance.checked_sub(scc_cents).unwrap_or(IskCents(0));
                            let b3 = ch.balance;
                            snap.append_journal(char_id, "IndustrySccSurcharge", scc_cents, b3, format!("SCC surcharge for industry job {jid}"));
                        }
                        if alpha_cents.0 > 0 {
                            if let Some(ch) = snap.characters.get_mut(&char_id) {
                                ch.balance = ch.balance.checked_sub(alpha_cents).unwrap_or(IskCents(0));
                                let b4 = ch.balance;
                                snap.append_journal(char_id, "IndustryAlphaCloneTax", alpha_cents, b4, format!("Alpha clone tax for industry job {jid}"));
                            }
                        }
                    } else if let Some(ch) = snap.characters.get_mut(&char_id) {
                        ch.balance = ch.balance.checked_sub(total_debit_cents).unwrap_or(IskCents(0));
                        let bal_after = ch.balance;
                        snap.append_journal(char_id, "IndustryFacilityTax", total_debit_cents, bal_after, format!("Facility tax for industry job {jid}"));
                    }
                }

                let fac_tax_cents = IskCents::from_f64_isk(charged_facility_tax_isk);
                if fac_tax_cents.0 > 0 {
                    if let Some(struct_owner_corp) = snap.structures.get(&station_id).map(|s| s.owner_corp_id) {
                        if !use_corp || struct_owner_corp != job_owner_id {
                            if let Some(scorp) = snap.corporations.get_mut(&struct_owner_corp) {
                                scorp.ensure_wallet_divisions();
                                scorp.master_wallet = scorp
                                    .master_wallet
                                    .checked_add(fac_tax_cents)
                                    .unwrap_or(scorp.master_wallet);
                                scorp.wallet_divisions[0].balance = scorp.master_wallet;
                                let sbal = scorp.master_wallet;
                                snap.append_journal(
                                    struct_owner_corp,
                                    "IndustryFacilityTax",
                                    fac_tax_cents,
                                    sbal,
                                    format!("Structure industry facility tax for job {jid}"),
                                );
                            }
                        }
                    }
                }

                if let Some(bp_mut) = snap.items.get_mut(&bp_iid) {
                    cost_meta.blueprint_original_flag =
                        Some(bp_mut.flag).filter(|f| *f != InventoryFlag::Locked);
                    bp_mut.singleton = true;
                    bp_mut.flag = InventoryFlag::Locked;
                    bp_mut.version += 1;
                }

                snap.industry_jobs.insert(
                    jid,
                    IndustryJobRecord {
                        job_id: jid,
                        installer_id: char_id,
                        facility_id: station_id,
                        blueprint_item_id: bp_iid,
                        blueprint_type_id: bp_type_id,
                        activity_id,
                        runs,
                        product_type_id,
                        status: "Active".into(),
                        duration_seconds: duration_sec,
                        start_unix: now_unix,
                    },
                );
                save_job_cost_meta(snap, jid, &cost_meta);
                Ok((jid, bp_iid, product_type_id, duration_sec, trit_req, inv_prob, charged_facility_tax_isk, cost_meta))
            });

            match res {
                Ok((job_id, bp_iid, product_type_id, duration_sec, trit_req, inv_prob, charged_tax_isk, cost_meta)) => {
                    emit_industry_job(ctx, char_id, cost_meta.owner_id, job_id, bp_iid, 1, runs as i64);
                    Some(Ok(PyRep::keyval([
                        ("jobID", PyRep::Integer(job_id)), ("ownerID", PyRep::Integer(cost_meta.owner_id)),
                        ("blueprintID", PyRep::Integer(bp_iid)), ("blueprintItemID", PyRep::Integer(bp_iid)),
                        ("blueprintTypeID", PyRep::Integer(bp_type_id as i64)), ("facilityID", PyRep::Integer(station_id)),
                        ("activityID", PyRep::Integer(activity_id as i64)), ("runs", PyRep::Integer(runs as i64)),
                        ("productTypeID", PyRep::Integer(product_type_id as i64)), ("inventedRuns", PyRep::Integer(cost_meta.invented_runs as i64)),
                        ("inventedME", PyRep::Real(cost_meta.invented_me)), ("inventedTE", PyRep::Real(cost_meta.invented_te)),
                        ("durationSeconds", PyRep::Integer(duration_sec as i64)), ("tritaniumRequired", PyRep::Integer(trit_req as i64)),
                        ("inventionProbability", PyRep::Real(inv_prob)), ("probability", PyRep::Real(inv_prob)),
                        ("estimatedItemValue", PyRep::Real(cost_meta.eiv_isk)), ("estimatedItemValueISK", PyRep::Real(cost_meta.eiv_isk)),
                        ("systemCostIndex", PyRep::Real(cost_meta.system_cost_index)), ("structureCostModifier", PyRep::Real(cost_meta.structure_cost_modifier)),
                        ("baseJobCost", PyRep::Real(cost_meta.base_job_cost_isk)), ("facilityTaxRate", PyRep::Real(cost_meta.facility_tax_rate)),
                        ("facilityTaxISK", PyRep::Real(charged_tax_isk)), ("sccSurchargeISK", PyRep::Real(cost_meta.scc_surcharge_isk)),
                        ("alphaCloneTaxISK", PyRep::Real(cost_meta.alpha_clone_tax_isk)), ("totalJobCost", PyRep::Real(cost_meta.total_cost_isk)),
                        ("totalJobCostISK", PyRep::Real(cost_meta.total_cost_isk)),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("industryManager", "CancelJob") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let job_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let now_unix = ctx.now_unix();
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                sync_industry_job_pause_states(snap, now_unix);
                let Some(job) = snap.industry_jobs.get(&job_id).cloned() else {
                    return Err(BridgeRpcError {
                        code: "JobNotFound".into(),
                        message: format!("Industry job {job_id} does not exist"),
                    });
                };
                let meta = load_job_cost_meta(snap, job_id).unwrap_or_default();
                let char_corp = snap
                    .characters
                    .get(&char_id)
                    .map(|c| c.corporation_id)
                    .or(session.corporation_id)
                    .unwrap_or(0);
                if meta.use_corp && meta.owner_id > 0 && char_corp != meta.owner_id {
                    return Err(BridgeRpcError {
                        code: "JobOwnershipViolation".into(),
                        message: format!("Industry job {job_id} belongs to corporation {} and cannot be cancelled by ex-member {char_id}", meta.owner_id),
                    });
                }
                if job.installer_id != char_id {
                    let can_cancel_corp = meta.use_corp
                        && meta.owner_id == char_corp
                        && (session.has_gm_role()
                            || (session.corp_role
                                & (eve_db::corp_roles::ROLE_DIRECTOR
                                    | eve_db::corp_roles::ROLE_FACTORY_MANAGER
                                    | eve_db::corp_roles::ROLE_RENT_FACTORY_FACILITY
                                    | eve_db::corp_roles::ROLE_RENT_RESEARCH_FACILITY))
                                != 0
                            || snap.corporations.get(&char_corp).is_some_and(|c| {
                                c.ceo_id == char_id
                                    || c.member_roles.get(&char_id).is_some_and(|&r| {
                                        (r & (eve_db::corp_roles::ROLE_DIRECTOR
                                            | eve_db::corp_roles::ROLE_FACTORY_MANAGER
                                            | eve_db::corp_roles::ROLE_RENT_FACTORY_FACILITY
                                            | eve_db::corp_roles::ROLE_RENT_RESEARCH_FACILITY))
                                            != 0
                                    })
                            }));
                    if !can_cancel_corp {
                        return Err(BridgeRpcError {
                            code: "JobOwnershipViolation".into(),
                            message: format!(
                                "Industry job {job_id} does not belong to character {char_id}"
                            ),
                        });
                    }
                }
                if !matches!(job.status.as_str(), "Active" | "Paused") {
                    return Err(BridgeRpcError {
                        code: "InvalidJobState".into(),
                        message: format!("Industry job {job_id} is not Active"),
                    });
                }
                if let Some(j_mut) = snap.industry_jobs.get_mut(&job_id) {
                    j_mut.status = "Cancelled".into();
                }
                if let Some(bp_mut) = snap.items.get_mut(&job.blueprint_item_id) {
                    bp_mut.flag = restore_blueprint_flag(bp_mut.owner_id, &meta);
                    bp_mut.version += 1;
                }
                let refund_owner_id = if meta.use_corp && meta.owner_id > 0 {
                    meta.owner_id
                } else {
                    job.installer_id
                };
                let refund_flag = if meta.use_corp && refund_owner_id != job.installer_id {
                    InventoryFlag::CorpSAG1
                } else {
                    InventoryFlag::Hangar
                };
                for (mat_tid, mat_qty) in &meta.consumed_materials {
                    if *mat_qty <= 0 {
                        continue;
                    }
                    if let Some(existing) = snap.items.values_mut().find(|i| {
                        i.owner_id == refund_owner_id
                            && i.location_id == job.facility_id
                            && i.type_id == *mat_tid
                            && i.flag == refund_flag
                            && !i.singleton
                    }) {
                        existing.quantity = existing.quantity.saturating_add(*mat_qty);
                        existing.version += 1;
                    } else {
                        snap.next_entity_id += 1;
                        let new_id = snap.next_entity_id;
                        snap.items.insert(
                            new_id,
                            InventoryItemRecord {
                                item_id: new_id,
                                type_id: *mat_tid,
                                owner_id: refund_owner_id,
                                location_id: job.facility_id,
                                flag: refund_flag,
                                quantity: *mat_qty,
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
                Ok(job)
            });
            match res {
                Ok(job) => {
                    emit_industry_job(ctx, char_id, char_id, job_id, job.blueprint_item_id, 102, 0);
                    Some(Ok(PyRep::keyval([
                        ("jobID", PyRep::Integer(job_id)),
                        ("status", PyRep::String("Cancelled".into())),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("industryManager", "CompleteJob") | ("industryManager", "CompleteManyJobs") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let mut job_ids: Vec<i64> = match args.first() {
                Some(PyRep::List(ids) | PyRep::Tuple(ids)) => ids.iter().filter_map(|v| v.as_i64()).collect(),
                Some(v) => v.as_i64().into_iter().collect(),
                None => Vec::new(),
            };
            if job_ids.is_empty() {
                job_ids.push(0);
            }
            let now_unix = ctx.now_unix();
            let mut completed_kvs = Vec::with_capacity(job_ids.len());
            for job_id in job_ids {
                let res = ctx.db.try_with_mut_snapshot(|snap| {
                    sync_industry_job_pause_states(snap, now_unix);
                    let Some(job) = snap.industry_jobs.get(&job_id).cloned() else {
                        return Err(BridgeRpcError { code: "JobNotFound".into(), message: format!("Industry job {job_id} does not exist") });
                    };
                    let meta = load_job_cost_meta(snap, job_id).unwrap_or_default();
                    let char_corp = snap.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(0);
                    if meta.use_corp && meta.owner_id > 0 {
                        if char_corp != meta.owner_id {
                            return Err(BridgeRpcError { code: "JobOwnershipViolation".into(), message: format!("Industry job {job_id} belongs to corporation {} and cannot be delivered by ex-member {char_id}", meta.owner_id) });
                        }
                    } else if job.installer_id != char_id {
                        return Err(BridgeRpcError { code: "JobOwnershipViolation".into(), message: format!("Industry job {job_id} does not belong to character {char_id}") });
                    }
                    if job.status == "Paused" {
                        return Err(BridgeRpcError { code: "FacilityServiceOffline".into(), message: format!("Industry job {job_id} is paused because facility service is offline or out of fuel") });
                    }
                    if job.status != "Active" {
                        return Err(BridgeRpcError { code: "InvalidJobState".into(), message: format!("Industry job {job_id} is in state '{}', not Active", job.status) });
                    }
                    let ready_at = job.start_unix.saturating_add(job.duration_seconds);
                    if now_unix < ready_at {
                        return Err(BridgeRpcError { code: "JobNotYetComplete".into(), message: format!("Industry job {job_id} completes at {ready_at} (now {now_unix})") });
                    }

                    let docked_loc = session.station_id.or(session.structure_id);
                    let out_owner_id = if meta.use_corp && meta.owner_id > 0 { meta.owner_id } else { char_id };
                    let is_ancient_relic = ctx.sde.types.get(&job.blueprint_type_id).map(|t| t.group_id == 971).unwrap_or(false);
                    let bp_valid = snap.items.get(&job.blueprint_item_id).map(|bp| {
                        (bp.owner_id == char_id || bp.owner_id == out_owner_id)
                            && (bp.location_id == job.facility_id || Some(bp.location_id) == docked_loc)
                            && bp.flag == InventoryFlag::Locked
                            && (is_ancient_relic || !bp.is_blueprint_copy || !matches!(job.activity_id, 1 | 8) || bp.blueprint_runs_remaining.unwrap_or(0) >= job.runs as i32)
                    }).unwrap_or(false);
                    if !bp_valid {
                        return Err(BridgeRpcError { code: "BlueprintMissingOrUnlocked".into(), message: format!("Blueprint {} for job {job_id} is missing or unlocked", job.blueprint_item_id) });
                    }

                    if job.activity_id == 8 {
                        let mut remove_bpc = true;
                        if let Some(bp_mut) = snap.items.get_mut(&job.blueprint_item_id) {
                            let unlocked_flag = restore_blueprint_flag(bp_mut.owner_id, &meta);
                            if is_ancient_relic && !bp_mut.is_blueprint_copy {
                                if bp_mut.quantity > job.runs as i32 {
                                    bp_mut.quantity -= job.runs as i32;
                                    bp_mut.flag = unlocked_flag;
                                    bp_mut.version += 1;
                                    remove_bpc = false;
                                }
                            } else {
                                let rem = bp_mut.blueprint_runs_remaining.unwrap_or(1).saturating_sub(job.runs as i32);
                                if rem > 0 {
                                    bp_mut.blueprint_runs_remaining = Some(rem);
                                    bp_mut.flag = unlocked_flag;
                                    bp_mut.version += 1;
                                    remove_bpc = false;
                                }
                            }
                        }
                        if remove_bpc {
                            snap.items.remove(&job.blueprint_item_id);
                        }
                        let inv_prob = if meta.inv_prob > 0.0 {
                            meta.inv_prob
                        } else {
                            let char_skills = snap.skills.get(&char_id);
                            let skill_lvl = |sid: TypeId| -> u8 { char_skills.and_then(|m| m.get(&sid)).map(|s| s.level).unwrap_or(0) };
                            let sci1_lvl = skill_lvl(11448).max(skill_lvl(11454)).max(skill_lvl(20433));
                            let sci2_lvl = skill_lvl(11452).max(skill_lvl(11442));
                            let enc_lvl = skill_lvl(21791).max(skill_lvl(23121)).max(skill_lvl(3408));
                            let base_prob = ctx.sde.blueprints.get(&job.blueprint_type_id).map(|b| b.invention_base_probability).unwrap_or(0.30);
                            compute_invention_probability(base_prob, sci1_lvl, sci2_lvl, enc_lvl, 1.0)
                        };
                        let server_roll = ctx.next_invention_roll(job.job_id as u64, char_id as u64, now_unix);
                        if server_roll >= inv_prob {
                            if let Some(j_mut) = snap.industry_jobs.get_mut(&job_id) {
                                j_mut.status = "Failed".into();
                            }
                            return Ok(Ok((0, 0, job)));
                        }
                        if let Some(j_mut) = snap.industry_jobs.get_mut(&job_id) {
                            j_mut.status = "Delivered".into();
                        }
                        snap.next_entity_id += 1;
                        let bpc_item_id = snap.next_entity_id;
                        let out_me = if meta.invented_runs > 0 { meta.invented_me } else { 2.0 };
                        let out_te = if meta.invented_runs > 0 { meta.invented_te } else { 4.0 };
                        let out_runs = if meta.invented_runs > 0 { meta.invented_runs } else { 1 };
                        let mut dyn_attrs = HashMap::new();
                        dyn_attrs.insert(ATTR_BLUEPRINT_ME, out_me);
                        dyn_attrs.insert(ATTR_BLUEPRINT_TE, out_te);
                        let t2_bpc_type_id: TypeId = if job.product_type_id > 0 { job.product_type_id } else { 11372 };
                        snap.items.insert(
                            bpc_item_id,
                            InventoryItemRecord {
                                item_id: bpc_item_id,
                                type_id: t2_bpc_type_id,
                                owner_id: out_owner_id,
                                location_id: job.facility_id,
                                flag: InventoryFlag::Hangar,
                                quantity: 1,
                                singleton: true,
                                version: 1,
                                dynamic_attributes: dyn_attrs,
                                source_mutaplasmid_id: None,
                                insured_payout_isk: 0.0,
                                is_blueprint_copy: true,
                                blueprint_runs_remaining: Some(out_runs),
                            },
                        );
                        return Ok(Ok((bpc_item_id, t2_bpc_type_id, job)));
                    }

                    if let Some(j_mut) = snap.industry_jobs.get_mut(&job_id) {
                        j_mut.status = "Delivered".into();
                    }

                    let mut parent_dyn_attrs = HashMap::new();
                    let mut remove_consumed_bpc = false;
                    let mut bp_location_id = job.facility_id;
                    if let Some(bp_mut) = snap.items.get_mut(&job.blueprint_item_id) {
                        bp_location_id = bp_mut.location_id;
                        bp_mut.flag = restore_blueprint_flag(bp_mut.owner_id, &meta);
                        if matches!(job.activity_id, 1 | 9) && bp_mut.is_blueprint_copy {
                            let rem = bp_mut.blueprint_runs_remaining.unwrap_or(1).saturating_sub(job.runs as i32);
                            if rem <= 0 {
                                remove_consumed_bpc = true;
                            } else {
                                bp_mut.blueprint_runs_remaining = Some(rem);
                            }
                        } else if job.activity_id == 3 {
                            let cur_te = bp_mut.dynamic_attributes.get(&ATTR_BLUEPRINT_TE).copied().unwrap_or(0.0);
                            bp_mut.dynamic_attributes.insert(ATTR_BLUEPRINT_TE, (cur_te + 2.0 * (job.runs as f64)).min(20.0));
                        } else if job.activity_id == 4 {
                            let cur_me = bp_mut.dynamic_attributes.get(&ATTR_BLUEPRINT_ME).copied().unwrap_or(0.0);
                            bp_mut.dynamic_attributes.insert(ATTR_BLUEPRINT_ME, (cur_me + 1.0 * (job.runs as f64)).min(10.0));
                        }
                        parent_dyn_attrs = bp_mut.dynamic_attributes.clone();
                        bp_mut.version += 1;
                    }
                    if remove_consumed_bpc {
                        snap.items.remove(&job.blueprint_item_id);
                    }

                    if matches!(job.activity_id, 1 | 9) {
                        let prod_qty_per_run = ctx.sde.blueprints.get(&job.blueprint_type_id).and_then(|bp| {
                            let key = if job.activity_id == 9 { "reaction" } else { "manufacturing" };
                            bp.activities.get(key).and_then(|a| a.products.first()).map(|p| (p.quantity as i32).max(1))
                        }).unwrap_or(1);
                        let total_out_qty = (job.runs as i32).saturating_mul(prod_qty_per_run);
                        snap.next_entity_id += 1;
                        let prod_item_id = snap.next_entity_id;
                        snap.items.insert(
                            prod_item_id,
                            InventoryItemRecord {
                                item_id: prod_item_id,
                                type_id: job.product_type_id,
                                owner_id: out_owner_id,
                                location_id: bp_location_id,
                                flag: InventoryFlag::Hangar,
                                quantity: total_out_qty,
                                singleton: total_out_qty == 1,
                                version: 1,
                                dynamic_attributes: HashMap::new(),
                                source_mutaplasmid_id: None,
                                insured_payout_isk: 0.0,
                                is_blueprint_copy: false,
                                blueprint_runs_remaining: None,
                            },
                        );
                        Ok(Ok((prod_item_id, job.product_type_id, job)))
                    } else if job.activity_id == 5 {
                        snap.next_entity_id += 1;
                        let copy_item_id = snap.next_entity_id;
                        snap.items.insert(
                            copy_item_id,
                            InventoryItemRecord {
                                item_id: copy_item_id,
                                type_id: job.blueprint_type_id,
                                owner_id: out_owner_id,
                                location_id: bp_location_id,
                                flag: InventoryFlag::Hangar,
                                quantity: 1,
                                singleton: true,
                                version: 1,
                                dynamic_attributes: parent_dyn_attrs,
                                source_mutaplasmid_id: None,
                                insured_payout_isk: 0.0,
                                is_blueprint_copy: true,
                                blueprint_runs_remaining: Some(job.runs as i32),
                            },
                        );
                        Ok(Ok((copy_item_id, job.blueprint_type_id, job)))
                    } else {
                        Ok(Ok((job.blueprint_item_id, job.blueprint_type_id, job)))
                    }
                });

                match res {
                    Ok(Ok((prod_item_id, prod_type, job))) => {
                        emit_industry_job(ctx, char_id, char_id, job_id, job.blueprint_item_id, 101, job.runs as i64);
                        if job.activity_id == 1 {
                            ctx.quasar_gateway.progression.daily_goals.on_gameplay_event(char_id as u32, 5, 1, &ctx.quasar_gateway);
                            ctx.quasar_gateway.progression.career_goals.on_gameplay_event(char_id as u32, "manufacturing", 1, &ctx.quasar_gateway);
                            ctx.quasar_gateway.progression.extra.on_campaign_gameplay_event(char_id as u32, crate::services::quasar_campaign_state::CampaignEvent { method: "ManufactureItem", type_id: Some(job.product_type_id), solar_system_id: None, faction_id: None }, 1, ctx.now_unix(), &ctx.quasar_gateway);
                        }
                        if method == "CompleteJob" && job.activity_id == 8 && prod_item_id == 0 {
                            return Some(Err(BridgeRpcError {
                                code: "InventionAttemptFailed".into(),
                                message: "Invention roll failed to produce a tech II blueprint".into(),
                            }));
                        }
                        completed_kvs.push(PyRep::keyval([
                            ("jobID", PyRep::Integer(job_id)),
                            ("deliveredItemID", if prod_item_id > 0 { PyRep::Integer(prod_item_id) } else { PyRep::None }),
                            ("createdBpcItemID", if prod_item_id > 0 { PyRep::Integer(prod_item_id) } else { PyRep::None }),
                            ("productTypeID", PyRep::Integer(prod_type as i64)),
                            ("inventionSucceeded", PyRep::Bool(job.activity_id == 8 && prod_item_id > 0)),
                        ]));
                    }
                    Ok(Err(inv_err)) => return Some(Err(inv_err)),
                    Err(e) => return Some(Err(e)),
                }
            }
            if method == "CompleteManyJobs" {
                Some(Ok(PyRep::List(completed_kvs)))
            } else {
                Some(Ok(completed_kvs.into_iter().next().unwrap_or(PyRep::None)))
            }
        }

        _ => None,
    }
}
