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
    ATTR_BLUEPRINT_ME, ATTR_BLUEPRINT_TE,
    emit_industry_job, handle_industry_job_queries,
    is_fulcrum_pirate_ship_bonus_applicable, is_station_container_type,
    is_structure_industry_service_online_for_bp, is_valid_industry_output_flag,
    resolve_blueprint_activity_product, resolve_decryptor_modifiers,
    resolve_facility_modifiers_and_system, resolve_facility_modifiers_and_system_for_job,
    resolve_structure_rig_bonuses_for_job, save_job_cost_meta,
    solar_system_jump_distance, sync_industry_job_pause_states,
    validate_capital_construction_restrictions,
};
use eve_db::{IndustryJobRecord, InventoryFlag, InventoryItemRecord, IskCents};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::TypeId;
use std::collections::HashMap;

#[path = "industry_jobs_lifecycle.rs"]
mod industry_jobs_lifecycle;

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
                    client_error: None,
                }));
            };

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
            let explicit_use_corp: Option<bool> = lookup_first("useCorp")
                .or_else(|| lookup_first("isCorpJob"))
                .and_then(|v| v.as_bool().or_else(|| v.as_i64().map(|n| n != 0)));
            let mut use_corp: bool = explicit_use_corp.unwrap_or(false);
            // V24.01 `IndustryWnd` (`JobData.dump()`) unconditionally includes `'corporationID': session.corpid`
            // on all jobs (even personal ones), so only explicit top-level `ownerID` selects job owner.
            let explicit_owner_id = lookup_first("ownerID")
                .and_then(|v| v.as_i64())
                .filter(|&id| id > 0);
            let raw_account: Option<(i64, i64)> = lookup_first("account").and_then(|v| match v {
                PyRep::Tuple(items) | PyRep::List(items) if items.len() >= 2 => {
                    Some((items[0].as_i64()?, items[1].as_i64()?))
                }
                _ => None,
            });
            let explicit_input_flag = lookup_first("inputFlag").or_else(|| lookup_first("flagID")).and_then(|v| v.as_i64()).and_then(|f| u16::try_from(f).ok()).map(InventoryFlag::from_u16);
            let raw_input_container = lookup_first("inputLocationID").or_else(|| lookup_first("inputContainerID")).or_else(|| lookup_first("containerID")).and_then(|v| v.as_i64()).filter(|&id| id > 0);
            let raw_output_location = lookup_first("outputLocationID").or_else(|| lookup_first("outputContainerID")).and_then(|v| v.as_i64()).filter(|&id| id > 0);
            let explicit_output_flag = lookup_first("outputFlag").and_then(|v| v.as_i64()).and_then(|f| u16::try_from(f).ok()).map(InventoryFlag::from_u16);
            let raw_licensed_runs = lookup_first("licensedRuns")
                .or_else(|| lookup_first("licensed_runs"))
                .filter(|v| !matches!(v, PyRep::None));

            let (raw_bp_type_id, raw_activity_id, runs_raw) = match args.first() {
                Some(PyRep::KeyVal(_) | PyRep::Dict(_)) => {
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
                    let rpc = crate::router::RpcArgs::positional(args);
                    explicit_bp_item_id = match rpc.opt_positive_i64(0, "blueprintItemID") {
                        Ok(v) => v,
                        Err(e) => return Some(Err(e)),
                    };
                    explicit_facility_id = match rpc.opt_positive_i64(4, "facilityID") {
                        Ok(v) => v,
                        Err(e) => return Some(Err(e)),
                    };
                    let btid = match rpc.opt_i64(1, "blueprintTypeID") {
                        Ok(v) => v,
                        Err(e) => return Some(Err(e)),
                    };
                    let act = match rpc.opt_i64(2, "activityID") {
                        Ok(v) => v.unwrap_or(1),
                        Err(e) => return Some(Err(e)),
                    };
                    let runs = match rpc.opt_i64(3, "runs") {
                        Ok(v) => v.unwrap_or(1),
                        Err(e) => return Some(Err(e)),
                    };
                    (btid, act, runs)
                }
                _ => {
                    let rpc = crate::router::RpcArgs::positional(args);
                    let btid = match rpc.opt_i64(0, "blueprintTypeID") {
                        Ok(v) => v,
                        Err(e) => return Some(Err(e)),
                    };
                    let act = match rpc.opt_i64(1, "activityID") {
                        Ok(v) => v.unwrap_or(1),
                        Err(e) => return Some(Err(e)),
                    };
                    let runs = match rpc.opt_i64(2, "runs") {
                        Ok(v) => v.unwrap_or(1),
                        Err(e) => return Some(Err(e)),
                    };
                    (btid, act, runs)
                }
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
                    client_error: None,
                }));
            };
            let (Ok(bp_type_id), Ok(activity_id)) =
                (TypeId::try_from(raw_bp_tid), u8::try_from(raw_activity_id))
            else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidIndustryJobParameters".into(),
                    message: "Invalid blueprint typeID or activityID".into(),
                    client_error: None,
                }));
            };

            if !(1..=1_000).contains(&runs_raw) || !matches!(activity_id, 1 | 3 | 4 | 5 | 8 | 9) {
                return Some(Err(BridgeRpcError {
                    code: "InvalidIndustryJobParameters".into(),
                    message: "Runs must be in 1..=1000 and activityID in {1, 3, 4, 5, 8, 9}".into(),
                    client_error: None,
                }));
            }
            let (runs, runs_i32) = (runs_raw as u32, runs_raw as i32);

            let Some(bp) = ctx.sde.blueprints.get(&bp_type_id) else {
                return Some(Err(BridgeRpcError {
                    code: "UnknownBlueprint".into(),
                    message: format!("Blueprint type {bp_type_id} not found in SDE"),
                    client_error: None,
                }));
            };
            // V24.01 `IndustryWnd` (`JobData.dump()`) serializes `licensedRuns` on all jobs
            // regardless of activity; ignore it outside copying (activityID 5).
            let copy_licensed_runs: u32 = if activity_id == 5 {
                let parsed = match raw_licensed_runs {
                    Some(raw) => {
                        let Some(val) = raw.as_i64().filter(|&n| n >= 1) else {
                            return Some(Err(BridgeRpcError {
                                code: "InvalidIndustryJobParameters".into(),
                                message: "licensedRuns must be a positive integer".into(),
                                client_error: None,
                            }));
                        };
                        let (Ok(u_val), Ok(_)) = (u32::try_from(val), i32::try_from(val)) else {
                            return Some(Err(BridgeRpcError {
                                code: "MaxRunsPerCopyExceeded".into(),
                                message: "Requested licensedRuns exceeds maximum allowed copy runs".into(),
                                client_error: None,
                            }));
                        };
                        u_val
                    }
                    None => runs,
                };
                if bp.max_production_limit > 0 && parsed > bp.max_production_limit {
                    return Some(Err(BridgeRpcError {
                        code: "MaxRunsPerCopyExceeded".into(),
                        message: format!(
                            "Requested {parsed} copy runs exceeds blueprint maxProductionLimit {}",
                            bp.max_production_limit
                        ),
                        client_error: None,
                    }));
                }
                if bp.max_production_limit == 0 && parsed > 1_000 {
                    return Some(Err(BridgeRpcError {
                        code: "MaxRunsPerCopyExceeded".into(),
                        message: "Requested copy runs exceeds maximum limit 1000".into(),
                        client_error: None,
                    }));
                }
                parsed
            } else {
                0
            };
            let total_copy_runs: u32 = if activity_id == 5 {
                let Some(total) = runs.checked_mul(copy_licensed_runs).filter(|&t| t <= 500_000) else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidIndustryJobParameters".into(),
                        message: "Total copy runs (runs * licensedRuns) exceeds maximum allowed cap".into(),
                        client_error: None,
                    }));
                };
                total
            } else {
                runs
            };
            let is_ancient_relic = ctx.sde.types.get(&bp_type_id).map(|t| t.group_id == 971).unwrap_or(false);

            // Enforce remote job range (`feat:industry.job-range`) when `station_id != docked_id`
            if station_id != docked_id {
                let range_check = ctx.db.with_snapshot(|snap| {
                    let current_sys = session
                        .solar_system_id
                        .or_else(|| snap.characters.get(&char_id).map(|c| c.solar_system_id))
                        .or_else(|| session.station_id.and_then(|sid| ctx.sde.location_solar_system(sid)))
                        .or_else(|| session.structure_id.and_then(|sid| snap.structures.get(&sid).map(|s| s.solar_system_id)))
                        .unwrap_or(0);
                    let Some(fac) = resolve_facility_modifiers_and_system(ctx, snap, station_id)
                    else {
                        return Err(BridgeRpcError {
                            code: "FacilityNotFound".into(),
                            message: format!("Industry facility {station_id} not found"),
                            client_error: None,
                        });
                    };
                    let f_sys = fac.solar_system_id;
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
                            client_error: None,
                        });
                    }
                    Ok(())
                });
                if let Err(e) = range_check {
                    return Some(Err(e));
                }
            }

            let now_unix = ctx.now_unix();
            // Matured 24h hangar-division role grants take effect before corp hangar Take checks.
            let char_corp_for_promote = ctx.db.with_snapshot(|snap| {
                snap.characters
                    .get(&char_id)
                    .map(|c| c.corporation_id)
                    .or(session.corporation_id)
                    .unwrap_or(0)
            });
            if char_corp_for_promote > 0 {
                crate::services::fleet_social::fleet_social_corps::promote_due_pending_corp_grants(
                    ctx,
                    char_corp_for_promote,
                );
            }

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                sync_industry_job_pause_states(snap, now_unix);
                let is_alpha_clone = snap
                    .characters
                    .get(&char_id)
                    .is_some_and(|ch| ch.clone_grade <= 1);
                let fallback_sys = snap
                    .characters
                    .get(&char_id)
                    .map(|c| c.solar_system_id)
                    .unwrap_or_else(|| ctx.sde.solar_systems.keys().copied().next().unwrap_or(0));
                let job_target_prod_tid = resolve_blueprint_activity_product(bp, activity_id);
                let (fac_type_id, fac_sys_id, mat_mod, time_mod) =
                    resolve_facility_modifiers_and_system_for_job(
                        ctx,
                        snap,
                        station_id,
                        Some(activity_id),
                        job_target_prod_tid,
                    )
                    .map_or((1529, fallback_sys, 1.0, 1.0), |f| {
                        (f.type_id, f.solar_system_id, f.material_modifier, f.time_modifier)
                    });
                if activity_id == 9 {
                    if !matches!(fac_type_id, 35835 | 35836) {
                        return Err(BridgeRpcError {
                            code: "FacilityDoesNotSupportReactions".into(),
                            message: format!(
                                "Facility {station_id} (type {fac_type_id}) does not support Reactions (requires Athanor 35835 or Tatara 35836)"
                            ),
                            client_error: None,
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
                            client_error: None,
                        });
                    }
                    let react_prod_tid = bp.activities.get("reaction").or_else(|| bp.activities.get("9"))
                        .and_then(|a| a.products.first()).map(|p| p.type_id).unwrap_or(bp.product_type_id);
                    if matches!(bp_type_id, 90274..=90282) || ctx.sde.types.get(&react_prod_tid).is_some_and(|t| t.group_id == 4932) {
                        let has_composite = snap.items.values().any(|i| {
                            i.location_id == station_id && (164..=171).contains(&(i.flag as u16)) && i.type_id == 45537
                                && snap.module_online_states.get(&i.item_id).copied().unwrap_or(true) && i.dynamic_attributes.get(&2).copied().unwrap_or(1.0) > 0.0
                        });
                        if !has_composite {
                            return Err(BridgeRpcError {
                                code: "CompositeReactorRequired".into(),
                                message: format!("Unrefined Mineral reaction formula {bp_type_id} requires an online Standup Composite Reactor I (45537) at facility {station_id}"),
                                client_error: None,
                            });
                        }
                    }
                }
                if !is_structure_industry_service_online_for_bp(&ctx.sde, snap, station_id, activity_id, bp_type_id) {
                    return Err(BridgeRpcError {
                        code: "FacilityServiceOffline".into(),
                        message: format!(
                            "Facility {station_id} industry/reaction service is offline or out of fuel"
                        ),
                        client_error: None,
                    });
                }
                if snap.structures.contains_key(&station_id) {
                    let _ = crate::services::structure_profile_services::check_industry_activity_acl_and_tax(
                        snap,
                        station_id,
                        char_id,
                        activity_id,
                        now_unix,
                    )?;
                }
                let char_corp_id = snap
                    .characters
                    .get(&char_id)
                    .map(|c| c.corporation_id)
                    .or(session.corporation_id)
                    .unwrap_or(0);
                if explicit_use_corp.is_none() && explicit_owner_id.is_none() && char_corp_id > 0 {
                    if let Some(eid) = explicit_bp_item_id {
                        if snap
                            .items
                            .get(&eid)
                            .is_some_and(|i| i.owner_id == char_corp_id && i.owner_id != char_id)
                        {
                            use_corp = true;
                        }
                    }
                }
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
                            client_error: None,
                        });
                    }
                }
                let (job_owner_id, is_corp_ceo, eff_corp_roles) = if use_corp {
                    if char_corp_id <= 0 {
                        return Err(BridgeRpcError {
                            code: "OwnerAccessDenied".into(),
                            message: "Character does not belong to a corporation".into(),
                            client_error: None,
                        });
                    }
                    let (is_ceo, eff_roles) =
                        crate::services::inventory_bound::get_effective_corp_roles_at_location_in_snap_at(
                            snap,
                            char_corp_id,
                            char_id,
                            Some(station_id),
                            now_unix,
                        );
                    const FACTORY_ROLES: u64 = eve_db::corp_roles::ROLE_DIRECTOR
                        | eve_db::corp_roles::ROLE_FACTORY_MANAGER
                        | eve_db::corp_roles::ROLE_RENT_FACTORY_FACILITY
                        | eve_db::corp_roles::ROLE_RENT_RESEARCH_FACILITY;
                    let has_factory_role =
                        session.has_gm_role() || is_ceo || (eff_roles & FACTORY_ROLES) != 0;
                    if !has_factory_role {
                        return Err(BridgeRpcError {
                            code: "CrpAccessDenied".into(),
                            message:
                                "Installing a corporation industry job requires Factory Manager or Director role"
                                    .into(),
                            client_error: None,
                        });
                    }
                    (char_corp_id, is_ceo, eff_roles)
                } else {
                    (char_id, false, 0u64)
                };
                let (wallet_is_corp, wallet_corp_div, wallet_account_key, wallet_is_ceo, wallet_eff_roles): (bool, u8, i32, bool, u64) =
                    if let Some((w_owner, acc_key)) = raw_account {
                        if w_owner == char_id {
                            if acc_key != 1000 {
                                return Err(BridgeRpcError {
                                    code: "InvalidIndustryJobParameters".into(),
                                    message: format!("Invalid personal wallet account key {acc_key}"),
                                    client_error: None,
                                });
                            }
                            (false, 1, 1000, false, 0u64)
                        } else if char_corp_id > 0 && w_owner == char_corp_id {
                            let Some(div_id) = acc_key
                                .checked_sub(999)
                                .and_then(|d| u8::try_from(d).ok())
                                .filter(|&d| (1..=7).contains(&d))
                            else {
                                return Err(BridgeRpcError {
                                    code: "InvalidIndustryJobParameters".into(),
                                    message: format!("Invalid corporation wallet account key {acc_key}"),
                                    client_error: None,
                                });
                            };
                            if !snap.corporations.contains_key(&char_corp_id) {
                                return Err(BridgeRpcError {
                                    code: "OwnerAccessDenied".into(),
                                    message: format!("Corporation wallet {w_owner} is not accessible"),
                                    client_error: None,
                                });
                            }
                            let (w_ceo, w_roles) = if use_corp {
                                (is_corp_ceo, eff_corp_roles)
                            } else {
                                crate::services::inventory_bound::get_effective_corp_roles_at_location_in_snap_at(
                                    snap,
                                    char_corp_id,
                                    char_id,
                                    Some(station_id),
                                    now_unix,
                                )
                            };
                            (true, div_id, acc_key as i32, w_ceo, w_roles)
                        } else {
                            return Err(BridgeRpcError {
                                code: "OwnerAccessDenied".into(),
                                message: format!(
                                    "Cannot pay industry job fees from wallet owner {w_owner}"
                                ),
                                client_error: None,
                            });
                        }
                    } else {
                        (
                            use_corp && snap.corporations.contains_key(&job_owner_id),
                            1,
                            1000,
                            is_corp_ceo,
                            eff_corp_roles,
                        )
                    };

                let rig_mat_bonus = resolve_structure_rig_bonuses_for_job(
                    ctx,
                    snap,
                    station_id,
                    fac_sys_id,
                    Some(activity_id),
                    job_target_prod_tid,
                )
                .mat_bonus;
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

                validate_capital_construction_restrictions(
                    ctx,
                    snap,
                    bp,
                    activity_id,
                    fac_sys_id,
                    fac_type_id,
                    station_id,
                )?;

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
                        client_error: None,
                    });
                }

                let cs_state =
                    crate::services::inventory_fitting::fitting_containers_ships::read_container_ship_state_snap(snap);
                let ext =
                    crate::services::station_structure::structure_upwell_ops::load_ext_state(snap);

                let is_usable_industry_flag = |flag: InventoryFlag| -> bool {
                    !matches!(flag, InventoryFlag::Locked | InventoryFlag::Impounded)
                        && !matches!(
                            flag as u16,
                            11..=34
                                | 36
                                | 62
                                | 63
                                | 72
                                | 92..=94
                                | 125..=132
                                | 145
                                | 158
                                | 164..=172
                                | 180
                        )
                };
                let has_div_take_role = |is_container: bool, flag: InventoryFlag| -> bool {
                    if session.has_gm_role()
                        || is_corp_ceo
                        || eve_db::corp_roles::has_role(eff_corp_roles, eve_db::corp_roles::ROLE_DIRECTOR)
                    {
                        return true;
                    }
                    let flag_num = flag as u16;
                    if !(116..=122).contains(&flag_num) {
                        return false;
                    }
                    let div_idx = (flag_num - 116) as usize;
                    let req = if is_container {
                        eve_db::corp_roles::ROLE_CONTAINER_CAN_TAKE[div_idx]
                    } else {
                        eve_db::corp_roles::ROLE_HANGAR_CAN_TAKE[div_idx]
                    };
                    eve_db::corp_roles::has_role(eff_corp_roles, req)
                };
                let is_item_blocked = |s: &eve_db::WorldStateSnapshot, it: &InventoryItemRecord| -> bool {
                    it.type_id == 60
                        || !is_usable_industry_flag(it.flag)
                        || s.is_item_locked_in_contract_or_job(it.item_id)
                        || cs_state.alsc_locked_items.contains(&it.item_id)
                        || ext.asset_safety_wraps.values().any(|w| {
                            !w.retrieval_fee_paid
                                && (w.wrap_id == it.item_id
                                    || w.wrap_id == it.location_id
                                    || w.item_ids.contains(&it.item_id))
                        })
                };
                let can_use_industry_source_item =
                    |snap: &eve_db::WorldStateSnapshot, it: &InventoryItemRecord| -> bool {
                        if is_item_blocked(snap, it) {
                            return false;
                        }
                        if it.owner_id != char_id && !(use_corp && it.owner_id == job_owner_id) {
                            return false;
                        }
                        if it.location_id == station_id {
                            if use_corp && it.owner_id == job_owner_id {
                                return (116..=122).contains(&(it.flag as u16))
                                    && has_div_take_role(false, it.flag);
                            }
                            return it.owner_id == char_id && it.flag == InventoryFlag::Hangar;
                        }
                        // Walk parent containers/office up to station_id.
                        let Some(first_parent) = snap.items.get(&it.location_id) else {
                            return false;
                        };
                        if first_parent.type_id == 27 {
                            // Direct child of a corp office folder: loose hangar division item.
                            return use_corp
                                && it.owner_id == job_owner_id
                                && first_parent.owner_id == job_owner_id
                                && !is_item_blocked(snap, first_parent)
                                && (116..=122).contains(&(it.flag as u16))
                                && has_div_take_role(false, it.flag);
                        }
                        // Item inside a container: only Unlocked (64) or Hangar (4) is valid inside containers.
                        if !matches!(it.flag, InventoryFlag::Unlocked | InventoryFlag::Hangar) {
                            return false;
                        }
                        let mut cur_loc = it.location_id;
                        let mut top_container: Option<&InventoryItemRecord> = None;
                        for _ in 0..8 {
                            let Some(cont) = snap.items.get(&cur_loc) else {
                                return false;
                            };
                            if cont.type_id == 27 {
                                if !use_corp
                                    || cont.owner_id != job_owner_id
                                    || is_item_blocked(snap, cont)
                                {
                                    return false;
                                }
                                break;
                            }
                            if cont.owner_id != char_id
                                && !(use_corp && cont.owner_id == job_owner_id)
                            {
                                return false;
                            }
                            if cont.type_id == 3_468 || is_item_blocked(snap, cont) {
                                return false;
                            }
                            if let Some(req_pw) = cs_state.container_passwords.get(&cont.item_id) {
                                if !req_pw.is_empty()
                                    && !cs_state
                                        .unlocked_containers
                                        .contains(&(cont.item_id, char_id))
                                {
                                    return false;
                                }
                            }
                            if top_container.is_some()
                                && !matches!(
                                    cont.flag,
                                    InventoryFlag::Unlocked | InventoryFlag::Hangar
                                )
                            {
                                return false;
                            }
                            top_container = Some(cont);
                            if cont.location_id == station_id {
                                break;
                            }
                            cur_loc = cont.location_id;
                        }
                        let Some(root_cont) = top_container else {
                            return false;
                        };
                        if use_corp && root_cont.owner_id == job_owner_id {
                            if it.owner_id != job_owner_id && it.owner_id != char_id {
                                return false;
                            }
                            (116..=122).contains(&(root_cont.flag as u16))
                                && has_div_take_role(true, root_cont.flag)
                        } else {
                            root_cont.owner_id == char_id
                                && it.owner_id == char_id
                                && root_cont.flag == InventoryFlag::Hangar
                        }
                    };

                if let Some(ef) = explicit_input_flag {
                    if !is_usable_industry_flag(ef)
                        || (!use_corp && !matches!(ef, InventoryFlag::Hangar | InventoryFlag::Unlocked))
                        || (use_corp
                            && raw_input_container.filter(|&id| id != station_id).is_none()
                            && !matches!(ef, InventoryFlag::Hangar)
                            && !(116..=122).contains(&(ef as u16)))
                    {
                        return Err(BridgeRpcError {
                            code: "CrpAccessDenied".into(),
                            message: format!(
                                "Input flag {} cannot be used as an industry material source",
                                ef as u16
                            ),
                            client_error: None,
                        });
                    }
                    if use_corp
                        && raw_input_container.filter(|&id| id != station_id).is_none()
                        && (116..=122).contains(&(ef as u16))
                        && !has_div_take_role(false, ef)
                    {
                        let div_num = (ef as u16).saturating_sub(115);
                        return Err(BridgeRpcError {
                            code: "CrpAccessDenied".into(),
                            message: format!(
                                "Missing corporation hangar division {div_num} Take role"
                            ),
                            client_error: None,
                        });
                    }
                }
                if use_corp {
                    if let Some(cid) = raw_input_container.filter(|&id| id != station_id) {
                        if let Some(cont) = snap.items.get(&cid) {
                            if cont.type_id != 27
                                && cont.owner_id == job_owner_id
                                && (116..=122).contains(&(cont.flag as u16))
                                && !has_div_take_role(true, cont.flag)
                            {
                                let div_num = (cont.flag as u16).saturating_sub(115);
                                return Err(BridgeRpcError {
                                    code: "CrpAccessDenied".into(),
                                    message: format!(
                                        "Missing corporation container division {div_num} Take role"
                                    ),
                                    client_error: None,
                                });
                            }
                        }
                    }
                }

                let mut matching_bps: Vec<InventoryItemRecord> = snap
                    .items
                    .values()
                    .filter(|i| {
                        (i.owner_id == char_id || (use_corp && i.owner_id == job_owner_id))
                            && crate::services::industry_pi::is_industry_loc_at_facility(snap, i.location_id, station_id, docked_id, char_id, use_corp, job_owner_id)
                            && i.type_id == bp_type_id
                            && explicit_bp_item_id.map(|eid| i.item_id == eid).unwrap_or(true)
                            && i.quantity >= 1
                            && can_use_industry_source_item(snap, i)
                    })
                    .cloned()
                    .collect();
                matching_bps.sort_by_key(|i| (raw_input_container.is_some_and(|cid| i.location_id != cid), i.item_id));
                if matching_bps.is_empty() {
                    return Err(BridgeRpcError {
                        code: "BlueprintNotAvailable".into(),
                        message: format!("No unlocked blueprint {bp_type_id} at facility {station_id}"),
                        client_error: None,
                    });
                }

                let bp_rec = match activity_id {
                    3 | 4 | 5 => matching_bps
                        .into_iter()
                        .find(|i| !i.is_blueprint_copy)
                        .ok_or_else(|| BridgeRpcError {
                            code: "CannotResearchOrCopyBPC".into(),
                            message: "ME/TE research and copying require a BPO".into(),
                            client_error: None,
                        })?,
                    8 if is_ancient_relic => matching_bps
                        .into_iter()
                        .find(|i| i.quantity >= runs_i32)
                        .ok_or_else(|| BridgeRpcError {
                            code: "InsufficientBlueprintRuns".into(),
                            message: format!("No ancient relic stack with {runs} units available"),
                            client_error: None,
                        })?,
                    8 => matching_bps
                        .into_iter()
                        .find(|i| i.is_blueprint_copy && i.blueprint_runs_remaining.unwrap_or(0) >= 1)
                        .ok_or_else(|| BridgeRpcError {
                            code: "InventionRequiresBPC".into(),
                            message: "T2 Invention requires a BPC with at least 1 remaining run".into(),
                            client_error: None,
                        })?,
                    _ => matching_bps
                        .into_iter()
                        .find(|i| !i.is_blueprint_copy || i.blueprint_runs_remaining.unwrap_or(0) >= runs_i32)
                        .ok_or_else(|| BridgeRpcError {
                            code: "InsufficientBlueprintRuns".into(),
                            message: format!("No BPO or BPC with {runs} remaining runs available"),
                            client_error: None,
                        })?,
                };
                let mut bp_iid = bp_rec.item_id;
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
                        client_error: None,
                    });
                }
                if activity_id == 4 && (me_level >= 10 || u32::from(me_level) + runs > 10) {
                    return Err(BridgeRpcError {
                        code: "MaxMaterialEfficiencyReached".into(),
                        message: "Blueprint Material Efficiency cannot exceed 10%".into(),
                        client_error: None,
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
                    3 | 4 => {
                        const LVL_SECS: [u64; 10] = [105, 250, 595, 1_414, 3_360, 8_000, 19_000, 45_255, 107_700, 256_000];
                        let (cur_lvl, base_sec, skill_mult) = if activity_id == 4 {
                            (me_level as usize, bp.research_me_time_sec.max(1) as f64, me_skill_mult)
                        } else {
                            ((te_level / 2) as usize, bp.research_te_time_sec.max(1) as f64, te_skill_mult)
                        };
                        let raw_sum: u64 = LVL_SECS[cur_lvl.min(10)..(cur_lvl + runs as usize).min(10)].iter().sum();
                        ((raw_sum as f64) * (base_sec / 105.0) * skill_mult * eff_time_mod).round().max(1.0) as u64
                    }
                    5 => (((bp.copy_time_sec.max(1) as f64) * copy_skill_mult * eff_time_mod)
                        .round()
                        .max(1.0) as u64)
                        .saturating_mul(u64::from(total_copy_runs)),
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

                let research_step = if activity_id == 4 {
                    me_level as usize
                } else if activity_id == 3 {
                    (te_level / 2) as usize
                } else {
                    0
                };
                let cost_runs = if activity_id == 5 { total_copy_runs } else { runs };
                let mut cost_meta = super::industry_facilities_bps::compute_industry_job_costs_with_step(
                    ctx,
                    snap,
                    Some(char_id),
                    bp_type_id,
                    activity_id,
                    research_step,
                    cost_runs,
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

                let charged_facility_tax_isk = cost_meta.facility_tax_isk;
                let total_debit_isk = cost_meta.total_cost_isk;

                let total_debit_cents = IskCents::from_f64_isk(total_debit_isk);
                if wallet_is_corp {
                    let div_idx = usize::from(wallet_corp_div - 1);
                    if total_debit_cents.0 > 0
                        && !session.has_gm_role()
                        && !wallet_is_ceo
                        && !eve_db::corp_roles::has_role(wallet_eff_roles, eve_db::corp_roles::ROLE_DIRECTOR)
                        && !eve_db::corp_roles::has_role(wallet_eff_roles, eve_db::corp_roles::ROLE_ACCOUNT_CAN_TAKE[div_idx])
                    {
                        return Err(BridgeRpcError {
                            code: "CrpAccessDenied".into(),
                            message: format!(
                                "Paying corporation industry job fees requires Director or Division {wallet_corp_div} Account Take role"
                            ),
                            client_error: None,
                        });
                    }
                    let corp_bal = snap
                        .corporations
                        .get(&char_corp_id)
                        .map(|c| c.division_balance(wallet_corp_div))
                        .unwrap_or(IskCents(0));
                    if corp_bal < total_debit_cents {
                        return Err(BridgeRpcError {
                            code: "InsufficientFunds".into(),
                            message: format!("Insufficient corporation funds ({total_debit_isk:.2} ISK)"),
                            client_error: None,
                        });
                    }
                } else if let Some(ch) = snap.characters.get(&char_id) {
                    if ch.balance < total_debit_cents {
                        return Err(BridgeRpcError {
                            code: "InsufficientFunds".into(),
                            message: format!("Insufficient funds ({total_debit_isk:.2} ISK required)"),
                            client_error: None,
                        });
                    }
                }

                let input_container_id = raw_input_container.filter(|&id| id != station_id);
                let mut input_is_non_office_container = false;
                if let Some(cid) = input_container_id {
                    let Some(cont) = snap.items.get(&cid) else {
                        return Err(BridgeRpcError { code: "InvalidInputContainer".into(), message: format!("Input container {cid} does not exist"), client_error: None });
                    };
                    let pw_locked = cs_state
                        .container_passwords
                        .get(&cid)
                        .is_some_and(|pw| !pw.is_empty() && !cs_state.unlocked_containers.contains(&(cid, char_id)));
                    if is_item_blocked(snap, cont)
                        || cont.type_id == 3_468
                        || pw_locked
                        || (cont.owner_id != char_id && !(use_corp && cont.owner_id == job_owner_id))
                        || !crate::services::industry_pi::is_industry_loc_at_facility(snap, cont.location_id, station_id, docked_id, char_id, use_corp, job_owner_id)
                    {
                        return Err(BridgeRpcError { code: "InvalidInputContainer".into(), message: format!("Input container {cid} is inaccessible or not at facility {station_id}"), client_error: None });
                    }
                    input_is_non_office_container = cont.type_id != 27;
                    if input_is_non_office_container && use_corp && cont.owner_id == job_owner_id && !has_div_take_role(true, cont.flag) {
                        let div_num = (cont.flag as u16).saturating_sub(115);
                        return Err(BridgeRpcError {
                            code: "CrpAccessDenied".into(),
                            message: format!("Missing corporation container division {div_num} Take role"),
                            client_error: None,
                        });
                    }
                }
                let mut is_out_item_container = false;
                if let Some(oid) = raw_output_location.filter(|&id| id != station_id) {
                    let Some(out_cont) = snap.items.get(&oid) else {
                        return Err(BridgeRpcError { code: "InvalidOutputLocation".into(), message: format!("Output location {oid} does not exist"), client_error: None });
                    };
                    let is_allowed_cont = out_cont.type_id == 27
                        || (out_cont.singleton
                            && (is_station_container_type(out_cont.type_id)
                                || ctx.sde.types.get(&out_cont.type_id).is_some_and(|t| {
                                    t.category_id == 2 && matches!(t.group_id, 12 | 340 | 448 | 649)
                                })));
                    if !is_allowed_cont
                        || !is_usable_industry_flag(out_cont.flag)
                        || (out_cont.owner_id != char_id && !(use_corp && out_cont.owner_id == job_owner_id))
                        || !crate::services::industry_pi::is_industry_loc_at_facility(snap, out_cont.location_id, station_id, docked_id, char_id, use_corp, job_owner_id)
                    {
                        return Err(BridgeRpcError { code: "InvalidOutputLocation".into(), message: format!("Output location {oid} is not an accessible container or office at facility {station_id}"), client_error: None });
                    }
                    is_out_item_container = out_cont.type_id != 27;
                }
                if let Some(of) = explicit_output_flag {
                    if !is_valid_industry_output_flag(of, is_out_item_container, use_corp) {
                        return Err(BridgeRpcError {
                            code: "InvalidOutputLocation".into(),
                            message: format!(
                                "Output flag {} is not valid for industry delivery",
                                of as u16
                            ),
                            client_error: None,
                        });
                    }
                }
                let effective_input_flag = explicit_input_flag.unwrap_or_else(|| {
                    if !use_corp {
                        InventoryFlag::Hangar
                    } else if let Some(cid) = input_container_id {
                        snap.items
                            .get(&cid)
                            .map(|c| c.flag)
                            .filter(|f| (116..=122).contains(&(*f as u16)))
                            .unwrap_or(InventoryFlag::CorpSAG1)
                    } else if (116..=122).contains(&(bp_rec.flag as u16)) {
                        bp_rec.flag
                    } else {
                        snap.items.values().find(|i| i.owner_id == job_owner_id && i.location_id == station_id && (116..=122).contains(&(i.flag as u16))).map(|i| i.flag).unwrap_or(InventoryFlag::CorpSAG1)
                    }
                });
                if use_corp
                    && !input_is_non_office_container
                    && explicit_input_flag.is_some()
                    && (116..=122).contains(&(effective_input_flag as u16))
                    && !has_div_take_role(false, effective_input_flag)
                {
                    let div_num = (effective_input_flag as u16).saturating_sub(115);
                    return Err(BridgeRpcError {
                        code: "CrpAccessDenied".into(),
                        message: format!(
                            "Missing corporation hangar division {div_num} Take role"
                        ),
                        client_error: None,
                    });
                }
                let mut consumed_materials: Vec<(TypeId, i32)> = Vec::new();
                let mut consume_material = |snap: &mut eve_db::WorldStateSnapshot, mat_tid: TypeId, req_qty: i32, err_code: &str| -> Result<(), BridgeRpcError> {
                    if req_qty <= 0 { return Ok(()); }
                    let mut matching_stacks: Vec<(bool, i64, i32)> = snap.items.values().filter(|i| {
                        let loc_ok = input_container_id.map_or(i.location_id == station_id || i.location_id == bp_rec.location_id, |cid| i.location_id == cid);
                        let flag_ok = explicit_input_flag.map_or(true, |ef| input_is_non_office_container || i.flag == ef);
                        loc_ok && flag_ok && can_use_industry_source_item(snap, i) && i.type_id == mat_tid && i.quantity > 0 && !i.singleton && !snap.is_item_or_ancestor_locked_or_impounded(i.item_id)
                    }).map(|i| (i.owner_id != job_owner_id || i.flag != effective_input_flag, i.item_id, i.quantity)).collect();
                    matching_stacks.sort_by_key(|(is_fallback, id, _)| (*is_fallback, *id));
                    let total_avail: i64 = matching_stacks.iter().map(|(_, _, q)| i64::from(*q)).sum();
                    if total_avail < i64::from(req_qty) {
                        return Err(BridgeRpcError { code: err_code.into(), message: format!("Missing {req_qty}x material type {mat_tid} at {station_id}"), client_error: None });
                    }
                    let mut rem = req_qty;
                    for (_, mid, _) in matching_stacks {
                        if rem <= 0 {
                            break;
                        }
                        let Some((m_owner, m_avail)) = snap.items.get(&mid).map(|i| (i.owner_id, i.quantity)) else {
                            continue;
                        };
                        let take = m_avail.min(rem);
                        let mut mat_opts = eve_db::ConsumeExactOptions::for_character_stack(char_id);
                        mat_opts.expected_owner_id = m_owner;
                        // Corp hangar division roles were already verified by `has_corp_div_take` (which respects `session.corp_role`).
                        mat_opts.actor_char_id = if m_owner == char_id { Some(char_id) } else { None };
                        snap.consume_exact_item_quantity(mid, take, mat_opts).map_err(|e| BridgeRpcError {
                            code: err_code.into(),
                            message: e.to_string(),
                            client_error: None,
                        })?;
                        rem -= take;
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
                            client_error: None,
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
                            client_error: None,
                        });
                    }
                    if sci1_lvl < 1 || sci2_lvl < 1 || enc_lvl < 1 {
                        return Err(BridgeRpcError {
                            code: "MissingInventionSkills".into(),
                            message: "Invention requires science and encryption skills >= level 1".into(),
                            client_error: None,
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
                    if !sde_inv_mats.is_empty() {
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
                            client_error: None,
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
                            client_error: None,
                        });
                    }
                    for (mtid, mqty) in react_mats {
                        let req_qty =
                            compute_manufacturing_material_qty(runs, mqty, 0, eff_mat_bonus_pct, rig_mat_bonus) as i32;
                        consume_material(snap, mtid, req_qty, "InsufficientReactionMaterials")?;
                    }
                }
                cost_meta.consumed_materials = consumed_materials;

                snap.next_entity_id += 1;
                let jid = snap.next_entity_id;

                if wallet_is_corp {
                    let base_cents = IskCents::from_f64_isk(cost_meta.base_job_cost_isk);
                    let tax_cents = IskCents::from_f64_isk(cost_meta.facility_tax_isk);
                    let scc_cents = IskCents::from_f64_isk(cost_meta.scc_surcharge_isk);
                    let corp_bal = snap
                        .corporations
                        .get_mut(&char_corp_id)
                        .map(|c| {
                            let cur = c.division_balance(wallet_corp_div);
                            let next = cur.checked_sub(total_debit_cents).unwrap_or(IskCents(0));
                            c.set_division_balance(wallet_corp_div, next);
                            next
                        })
                        .unwrap_or(IskCents(0));
                    let now_ts = ctx.now_unix();
                    for (etype, fee, desc) in [
                        ("IndustryJobInstallationFee", base_cents, format!("Base job cost for industry job {jid}")),
                        ("IndustryFacilityTax", tax_cents, format!("Facility tax for industry job {jid}")),
                        ("IndustrySccSurcharge", scc_cents, format!("SCC surcharge for industry job {jid}")),
                    ] {
                        if etype != "IndustryJobInstallationFee" && fee.0 <= 0 {
                            continue;
                        }
                        snap.append_journal_entry(eve_db::JournalEntryInput {
                            owner_id: char_corp_id,
                            entry_type: etype,
                            amount: fee,
                            balance_after: corp_bal,
                            reason: desc,
                            meta: eve_db::JournalMeta {
                                ref_type_id: 56,
                                first_party_id: char_corp_id,
                                second_party_id: station_id,
                                account_key: wallet_account_key,
                                date_unix: now_ts,
                                ..Default::default()
                            },
                        });
                    }
                } else {
                    let base_cents = IskCents::from_f64_isk(cost_meta.base_job_cost_isk);
                    let tax_cents = IskCents::from_f64_isk(cost_meta.facility_tax_isk);
                    let scc_cents = IskCents::from_f64_isk(cost_meta.scc_surcharge_isk);
                    let alpha_cents = IskCents::from_f64_isk(cost_meta.alpha_clone_tax_isk);
                    let now_ts = ctx.now_unix();
                    for (etype, fee, desc) in [
                        ("IndustryJobInstallationFee", base_cents, format!("Base job cost for industry job {jid}")),
                        ("IndustryFacilityTax", tax_cents, format!("Facility tax for industry job {jid}")),
                        ("IndustrySccSurcharge", scc_cents, format!("SCC surcharge for industry job {jid}")),
                        ("IndustryAlphaCloneTax", alpha_cents, format!("Alpha clone tax for industry job {jid}")),
                    ] {
                        if etype == "IndustryAlphaCloneTax" && fee.0 <= 0 {
                            continue;
                        }
                        if let Some(ch) = snap.characters.get_mut(&char_id) {
                            ch.balance = ch.balance.checked_sub(fee).unwrap_or(IskCents(0));
                            let bal_after = ch.balance;
                            snap.append_journal_entry(eve_db::JournalEntryInput {
                                owner_id: char_id,
                                entry_type: etype,
                                amount: fee,
                                balance_after: bal_after,
                                reason: desc,
                                meta: eve_db::JournalMeta {
                                    ref_type_id: 56,
                                    first_party_id: char_id,
                                    second_party_id: station_id,
                                    account_key: 1000,
                                    date_unix: now_ts,
                                    ..Default::default()
                                },
                            });
                        }
                    }
                }

                let fac_tax_cents = IskCents::from_f64_isk(charged_facility_tax_isk);
                if fac_tax_cents.0 > 0 {
                    let fac_owner_corp = snap
                        .structures
                        .get(&station_id)
                        .map(|s| s.owner_corp_id)
                        .or_else(|| {
                            resolve_facility_modifiers_and_system(ctx, snap, station_id)
                                .map(|m| m.owner_corp_id)
                        });
                    if let Some(fac_corp) = fac_owner_corp {
                        if !use_corp || fac_corp != job_owner_id {
                            let scorp = snap.corporations.entry(fac_corp).or_insert_with(|| {
                                let npc = ctx.sde.npc_corporations.get(&fac_corp);
                                let name = npc
                                    .map(|c| c.name.clone())
                                    .unwrap_or_else(|| format!("Corporation {fac_corp}"));
                                let ticker = npc
                                    .map(|c| c.ticker_name.clone())
                                    .filter(|t| !t.is_empty())
                                    .unwrap_or_else(|| "NPC".into());
                                let ceo_id = npc
                                    .and_then(|c| c.ceo_id.filter(|&id| id > 0))
                                    .unwrap_or(fac_corp);
                                let mut rec =
                                    eve_db::CorporationRecord::new(fac_corp, name.clone(), ticker, ceo_id);
                                if let Some(nc) = npc {
                                    rec.tax_rate = nc.tax_rate;
                                    rec.hq_station_id = nc.station_id.filter(|&id| id > 0);
                                    rec.description = format!("NPC Corporation {name}");
                                }
                                rec
                            });
                            let sbal = scorp.credit_division(1, fac_tax_cents);
                            snap.append_journal(
                                fac_corp,
                                "IndustryFacilityTax",
                                fac_tax_cents,
                                sbal,
                                format!("Facility tax for industry job {jid}"),
                            );
                        }
                    }
                }

                let corp_office_id = crate::services::industry_pi::resolve_corp_office_at_facility(snap, use_corp, job_owner_id, station_id);
                let split_qty = if is_ancient_relic && activity_id == 8 { runs_i32 } else { 1 };
                let mut split_item: Option<InventoryItemRecord> = None;
                if let Some(bp_mut) = snap.items.get_mut(&bp_iid) {
                    cost_meta.blueprint_original_flag = Some(bp_mut.flag).filter(|f| *f != InventoryFlag::Locked);
                    let bp_loc = if bp_mut.location_id != station_id { bp_mut.location_id } else { corp_office_id.unwrap_or(bp_mut.location_id) };
                    cost_meta.blueprint_location_id = bp_loc;
                    cost_meta.output_location_id = raw_output_location.unwrap_or(bp_loc);
                    cost_meta.output_flag = explicit_output_flag;
                    cost_meta.location_id = corp_office_id.unwrap_or(bp_loc);
                    if bp_mut.quantity > split_qty {
                        bp_mut.quantity -= split_qty;
                        bp_mut.version += 1;
                        let mut cloned = bp_mut.clone();
                        cloned.quantity = split_qty;
                        cloned.singleton = !(is_ancient_relic && activity_id == 8);
                        cloned.flag = InventoryFlag::Locked;
                        cloned.version = 1;
                        split_item = Some(cloned);
                    } else {
                        if !(is_ancient_relic && activity_id == 8) {
                            bp_mut.singleton = true;
                        }
                        bp_mut.flag = InventoryFlag::Locked;
                        bp_mut.version += 1;
                    }
                }
                if let Some(mut cloned) = split_item {
                    snap.next_entity_id += 1;
                    let split_id = snap.next_entity_id;
                    cloned.item_id = split_id;
                    snap.items.insert(split_id, cloned);
                    bp_iid = split_id;
                }

                let licensed_runs = if activity_id == 8 && cost_meta.invented_runs > 0 { Some(cost_meta.invented_runs as u32) } else if activity_id == 5 { Some(copy_licensed_runs) } else { None };
                snap.industry_jobs.insert(jid, IndustryJobRecord {
                    job_id: jid, installer_id: char_id, facility_id: station_id, blueprint_item_id: bp_iid,
                    blueprint_type_id: bp_type_id, activity_id, runs, product_type_id, status: "Active".into(),
                    duration_seconds: duration_sec, start_unix: now_unix,
                    blueprint_location_id: cost_meta.blueprint_location_id, output_location_id: cost_meta.output_location_id,
                    cost: (cost_meta.total_cost_isk > 0.0).then_some(cost_meta.total_cost_isk), licensed_runs,
                    probability: (inv_prob > 0.0).then_some(inv_prob as f32), completed_unix: None, completed_character_id: None,
                });
                save_job_cost_meta(snap, jid, &cost_meta);
                let post_wallet_balance = if wallet_is_corp {
                    snap.corporations
                        .get(&char_corp_id)
                        .map(|c| c.division_balance(wallet_corp_div))
                        .unwrap_or(IskCents(0))
                } else {
                    snap.characters
                        .get(&char_id)
                        .map(|c| c.balance)
                        .unwrap_or(IskCents(0))
                };
                let wallet_owner_id = if wallet_is_corp { char_corp_id } else { char_id };
                Ok((
                    jid,
                    bp_iid,
                    product_type_id,
                    duration_sec,
                    trit_req,
                    inv_prob,
                    charged_facility_tax_isk,
                    cost_meta,
                    effective_input_flag,
                    input_container_id,
                    wallet_is_corp,
                    wallet_owner_id,
                    wallet_corp_div,
                    post_wallet_balance,
                ))
            });

            match res {
                Ok((
                    job_id,
                    bp_iid,
                    product_type_id,
                    duration_sec,
                    trit_req,
                    inv_prob,
                    charged_tax_isk,
                    cost_meta,
                    eff_in_flag,
                    in_cid,
                    wallet_is_corp,
                    wallet_owner_id,
                    wallet_corp_div,
                    post_wallet_balance,
                )) => {
                    ctx.notify_wallet_balance(
                        wallet_is_corp,
                        wallet_owner_id,
                        wallet_corp_div,
                        post_wallet_balance,
                    );
                    emit_industry_job(ctx, char_id, cost_meta.owner_id, job_id, bp_iid, 1, runs as i64);
                    crate::services::notify_emitters_character_services::emit_industry_materials_consumed(ctx, char_id, job_id, station_id, docked_id, cost_meta.owner_id, cost_meta.use_corp, eff_in_flag, in_cid, &cost_meta.consumed_materials);
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

        ("industryManager", "CancelJob")
        | ("industryManager", "CompleteJob")
        | ("industryManager", "CompleteManyJobs") => {
            industry_jobs_lifecycle::handle_cancel_or_complete_industry_job(
                ctx, session, service, method, args,
            )
        }
        _ => None,
    }
}

#[cfg(test)]
#[path = "industry_jobs_tests.rs"]
mod industry_jobs_tests;
#[path = "industry_jobs_cov_unit_tests.rs"]
#[cfg(test)]
mod industry_jobs_cov_unit_tests;
#[cfg(test)]
#[path = "industry_jobs_edges2_tests.rs"]
mod industry_jobs_edges2_tests;

