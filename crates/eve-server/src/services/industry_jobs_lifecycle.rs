//! `industry_jobs_lifecycle` — Industry job cancellation and completion/delivery (`E-4`).

// parity: plan:E-4
// parity: feat:industry.job-complete, feat:industry.job-cancel, feat:industry.blueprint-lock

#![forbid(unsafe_code)]

use crate::formulas::compute_invention_probability;
use crate::router::ServerContext;
use crate::services::industry_pi::{
    ATTR_BLUEPRINT_ME, ATTR_BLUEPRINT_TE, emit_industry_job,
    finalize_completed_industry_job_response, load_job_cost_meta, restore_blueprint_flag,
    save_job_cost_meta, sync_industry_job_pause_states,
};
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::TypeId;
use std::collections::HashMap;

pub(super) fn handle_cancel_or_complete_industry_job(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
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
                        message: format!(
                            "Industry job {job_id} belongs to corporation {} and cannot be cancelled by ex-member {char_id}",
                            meta.owner_id
                        ),
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
                    j_mut.completed_unix = Some(now_unix);
                    j_mut.completed_character_id = Some(char_id);
                }
                let mut done_meta = meta.clone();
                done_meta.completed_unix = Some(now_unix);
                done_meta.completed_character_id = Some(char_id);
                save_job_cost_meta(snap, job_id, &done_meta);
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
                Some(PyRep::List(ids) | PyRep::Tuple(ids)) => {
                    ids.iter().filter_map(|v| v.as_i64()).collect()
                }
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
                        .unwrap_or(0);
                    if meta.use_corp && meta.owner_id > 0 {
                        if char_corp != meta.owner_id {
                            return Err(BridgeRpcError {
                                code: "JobOwnershipViolation".into(),
                                message: format!(
                                    "Industry job {job_id} belongs to corporation {} and cannot be delivered by ex-member {char_id}",
                                    meta.owner_id
                                ),
                            });
                        }
                    } else if job.installer_id != char_id {
                        return Err(BridgeRpcError {
                            code: "JobOwnershipViolation".into(),
                            message: format!(
                                "Industry job {job_id} does not belong to character {char_id}"
                            ),
                        });
                    }
                    if job.status == "Paused" {
                        return Err(BridgeRpcError {
                            code: "FacilityServiceOffline".into(),
                            message: format!(
                                "Industry job {job_id} is paused because facility service is offline or out of fuel"
                            ),
                        });
                    }
                    if job.status != "Active" {
                        return Err(BridgeRpcError {
                            code: "InvalidJobState".into(),
                            message: format!(
                                "Industry job {job_id} is in state '{}', not Active",
                                job.status
                            ),
                        });
                    }
                    let ready_at = job.start_unix.saturating_add(job.duration_seconds);
                    if now_unix < ready_at {
                        return Err(BridgeRpcError {
                            code: "JobNotYetComplete".into(),
                            message: format!(
                                "Industry job {job_id} completes at {ready_at} (now {now_unix})"
                            ),
                        });
                    }

                    let out_owner_id = if meta.use_corp && meta.owner_id > 0 {
                        meta.owner_id
                    } else {
                        char_id
                    };
                    let is_ancient_relic = ctx
                        .sde
                        .types
                        .get(&job.blueprint_type_id)
                        .map(|t| t.group_id == 971)
                        .unwrap_or(false);
                    let bp_valid = snap
                        .items
                        .get(&job.blueprint_item_id)
                        .map(|bp| {
                            let bp_at_fac = bp.location_id == job.facility_id
                                || bp.location_id == job.blueprint_location_id
                                || bp.location_id == meta.blueprint_location_id
                                || snap
                                    .items
                                    .get(&bp.location_id)
                                    .is_some_and(|p| p.location_id == job.facility_id);
                            (bp.owner_id == char_id || bp.owner_id == out_owner_id)
                                && bp_at_fac
                                && bp.flag == InventoryFlag::Locked
                                && (is_ancient_relic
                                    || !bp.is_blueprint_copy
                                    || !matches!(job.activity_id, 1 | 8)
                                    || bp.blueprint_runs_remaining.unwrap_or(0) >= job.runs as i32)
                        })
                        .unwrap_or(false);
                    if !bp_valid {
                        return Err(BridgeRpcError {
                            code: "BlueprintMissingOrUnlocked".into(),
                            message: format!(
                                "Blueprint {} for job {job_id} is missing or unlocked",
                                job.blueprint_item_id
                            ),
                        });
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
                                let rem = bp_mut
                                    .blueprint_runs_remaining
                                    .unwrap_or(1)
                                    .saturating_sub(job.runs as i32);
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
                            let skill_lvl = |sid: TypeId| -> u8 {
                                char_skills
                                    .and_then(|m| m.get(&sid))
                                    .map(|s| s.level)
                                    .unwrap_or(0)
                            };
                            let sci1_lvl =
                                skill_lvl(11448).max(skill_lvl(11454)).max(skill_lvl(20433));
                            let sci2_lvl = skill_lvl(11452).max(skill_lvl(11442));
                            let enc_lvl =
                                skill_lvl(21791).max(skill_lvl(23121)).max(skill_lvl(3408));
                            let base_prob = ctx
                                .sde
                                .blueprints
                                .get(&job.blueprint_type_id)
                                .map(|b| b.invention_base_probability)
                                .unwrap_or(0.30);
                            compute_invention_probability(base_prob, sci1_lvl, sci2_lvl, enc_lvl, 1.0)
                        };
                        let server_roll =
                            ctx.next_invention_roll(job.job_id as u64, char_id as u64, now_unix);
                        if server_roll >= inv_prob {
                            if let Some(j_mut) = snap.industry_jobs.get_mut(&job_id) {
                                j_mut.status = "Failed".into();
                                j_mut.completed_unix = Some(now_unix);
                                j_mut.completed_character_id = Some(char_id);
                            }
                            let mut fail_meta = meta.clone();
                            fail_meta.completed_unix = Some(now_unix);
                            fail_meta.completed_character_id = Some(char_id);
                            save_job_cost_meta(snap, job_id, &fail_meta);
                            return Ok(Ok((0, 0, job)));
                        }
                        if let Some(j_mut) = snap.industry_jobs.get_mut(&job_id) {
                            j_mut.status = "Delivered".into();
                            j_mut.completed_unix = Some(now_unix);
                            j_mut.completed_character_id = Some(char_id);
                        }
                        let mut done_meta = meta.clone();
                        done_meta.completed_unix = Some(now_unix);
                        done_meta.completed_character_id = Some(char_id);
                        save_job_cost_meta(snap, job_id, &done_meta);
                        snap.next_entity_id += 1;
                        let bpc_item_id = snap.next_entity_id;
                        let out_me = if meta.invented_runs > 0 { meta.invented_me } else { 2.0 };
                        let out_te = if meta.invented_runs > 0 { meta.invented_te } else { 4.0 };
                        let out_runs = if meta.invented_runs > 0 { meta.invented_runs } else { 1 };
                        let mut dyn_attrs = HashMap::new();
                        dyn_attrs.insert(ATTR_BLUEPRINT_ME, out_me);
                        dyn_attrs.insert(ATTR_BLUEPRINT_TE, out_te);
                        let t2_bpc_type_id: TypeId = if job.product_type_id > 0 {
                            job.product_type_id
                        } else {
                            11372
                        };
                        let (out_loc_id, out_flag) =
                            crate::services::industry_pi::resolve_industry_delivery_target(
                                snap,
                                &job,
                                &meta,
                                char_id,
                                out_owner_id,
                                job.facility_id,
                            );
                        snap.items.insert(
                            bpc_item_id,
                            InventoryItemRecord {
                                item_id: bpc_item_id,
                                type_id: t2_bpc_type_id,
                                owner_id: out_owner_id,
                                location_id: out_loc_id,
                                flag: out_flag,
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
                        j_mut.completed_unix = Some(now_unix);
                        j_mut.completed_character_id = Some(char_id);
                    }
                    let mut done_meta = meta.clone();
                    done_meta.completed_unix = Some(now_unix);
                    done_meta.completed_character_id = Some(char_id);
                    save_job_cost_meta(snap, job_id, &done_meta);

                    let mut parent_dyn_attrs = HashMap::new();
                    let mut remove_consumed_bpc = false;
                    let mut bp_location_id = job.facility_id;
                    if let Some(bp_mut) = snap.items.get_mut(&job.blueprint_item_id) {
                        bp_location_id = bp_mut.location_id;
                        bp_mut.flag = restore_blueprint_flag(bp_mut.owner_id, &meta);
                        if matches!(job.activity_id, 1 | 9) && bp_mut.is_blueprint_copy {
                            let rem = bp_mut
                                .blueprint_runs_remaining
                                .unwrap_or(1)
                                .saturating_sub(job.runs as i32);
                            if rem <= 0 {
                                remove_consumed_bpc = true;
                            } else {
                                bp_mut.blueprint_runs_remaining = Some(rem);
                            }
                        } else if job.activity_id == 3 {
                            let cur_te = bp_mut
                                .dynamic_attributes
                                .get(&ATTR_BLUEPRINT_TE)
                                .copied()
                                .unwrap_or(0.0);
                            bp_mut
                                .dynamic_attributes
                                .insert(ATTR_BLUEPRINT_TE, (cur_te + 2.0 * (job.runs as f64)).min(20.0));
                        } else if job.activity_id == 4 {
                            let cur_me = bp_mut
                                .dynamic_attributes
                                .get(&ATTR_BLUEPRINT_ME)
                                .copied()
                                .unwrap_or(0.0);
                            bp_mut
                                .dynamic_attributes
                                .insert(ATTR_BLUEPRINT_ME, (cur_me + 1.0 * (job.runs as f64)).min(10.0));
                        }
                        parent_dyn_attrs = bp_mut.dynamic_attributes.clone();
                        bp_mut.version += 1;
                    }
                    if remove_consumed_bpc {
                        snap.items.remove(&job.blueprint_item_id);
                    }
                    let (out_loc_id, out_flag) =
                        crate::services::industry_pi::resolve_industry_delivery_target(
                            snap,
                            &job,
                            &meta,
                            char_id,
                            out_owner_id,
                            bp_location_id,
                        );

                    if matches!(job.activity_id, 1 | 9) {
                        let prod_qty_per_run = ctx
                            .sde
                            .blueprints
                            .get(&job.blueprint_type_id)
                            .and_then(|bp| {
                                let key = if job.activity_id == 9 {
                                    "reaction"
                                } else {
                                    "manufacturing"
                                };
                                bp.activities
                                    .get(key)
                                    .and_then(|a| a.products.first())
                                    .map(|p| (p.quantity as i32).max(1))
                            })
                            .unwrap_or(1);
                        let total_out_qty = (job.runs as i32).saturating_mul(prod_qty_per_run);
                        snap.next_entity_id += 1;
                        let prod_item_id = snap.next_entity_id;
                        snap.items.insert(
                            prod_item_id,
                            InventoryItemRecord {
                                item_id: prod_item_id,
                                type_id: job.product_type_id,
                                owner_id: out_owner_id,
                                location_id: out_loc_id,
                                flag: out_flag,
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
                                location_id: out_loc_id,
                                flag: out_flag,
                                quantity: 1,
                                singleton: true,
                                version: 1,
                                dynamic_attributes: parent_dyn_attrs,
                                source_mutaplasmid_id: None,
                                insured_payout_isk: 0.0,
                                is_blueprint_copy: true,
                                blueprint_runs_remaining: Some(
                                    job.licensed_runs.unwrap_or(job.runs) as i32,
                                ),
                            },
                        );
                        Ok(Ok((copy_item_id, job.blueprint_type_id, job)))
                    } else {
                        Ok(Ok((job.blueprint_item_id, job.blueprint_type_id, job)))
                    }
                });
                match res {
                    Ok(Ok((prod_item_id, prod_type, job))) => {
                        match finalize_completed_industry_job_response(
                            ctx,
                            char_id,
                            method,
                            job_id,
                            prod_item_id,
                            prod_type,
                            &job,
                        ) {
                            Ok(kv) => completed_kvs.push(kv),
                            Err(e) => return Some(Err(e)),
                        }
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
