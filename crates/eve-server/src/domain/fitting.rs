//! Protocol-agnostic ship and structure fitting validation and transaction primitives (`#1193`).
//!
//! Shared by both GM `/fit` and live gameplay fitting/autofit flows (`inventory_fitting`,
//! `inventory_stacking`, `fitting_saved_apply`) so slot selection, online evaluation, and
//! persisted `module_online_states` updates follow one deterministic invariant path with zero
//! wire-protocol coupling.

#![forbid(unsafe_code)]

use crate::domain::items::{grant_item_in_snap, GrantItemRequest};
use crate::domain::DomainError;
use crate::router::ServerContext;
use eve_db::{InventoryFlag, InventoryItemRecord, WorldStateSnapshot};
use eve_dogma::ShipFittingEngine;
use eve_sde::{InvType, SdeRegistry};
use std::collections::HashSet;

/// Returns `true` when `err` represents a hull/slot fitting compatibility rejection from
/// [`validate_and_select_fit_slot`] (so RPC layers map it to typed fitting-validation facts).
pub fn is_fit_slot_validation_error(err: &DomainError) -> bool {
    err.code == "FitSlotValidationError"
}

fn fit_slot_validation_error(message: impl Into<String>) -> DomainError {
    DomainError::invalid_input("FitSlotValidationError", message)
}

/// Policy governing skill checks when bringing a fitted module online.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FitOnlinePolicy {
    /// Enforce both character skill prerequisites and ship CPU/PG capacity (`auto-slot` `/fit`,
    /// saved fitting apply, gameplay autofit).
    CheckSkillsAndResources,
    /// Skip character skill checks while still enforcing ship CPU/PG, burnout, and
    /// `maxGroupOnline` constraints (explicit-flag `/fit <ship> <type> <flag>` `#648`).
    SkipSkillsCheckResources,
}

/// Request to mint and fit a singleton module, subsystem, charge, or drone onto a target ship or
/// structure in a single database transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MintAndFitRequest {
    pub ship_id: i64,
    pub type_id: u32,
    pub explicit_flag: Option<InventoryFlag>,
    pub online_policy: FitOnlinePolicy,
}

/// Committed outcome returned by [`fit_newly_minted_item_tx`].
#[derive(Debug, Clone, PartialEq)]
pub struct MintAndFitOutcome {
    pub item: InventoryItemRecord,
    pub chosen_flag: InventoryFlag,
    pub is_online: bool,
    pub is_fitting_slot: bool,
    pub ship_category_id: u32,
}

/// Returns `true` for low/med/high/rig/subsystem/service fitting slot flags (`11..=34 | 92..=94 |
/// 125..=132 | 164..=171`).
pub fn is_fitting_slot_flag(flag: InventoryFlag) -> bool {
    matches!(flag as u16, 11..=34 | 92..=94 | 125..=132 | 164..=171)
}

/// Returns `true` for ship-hull module/subsystem fitting slot flags (`11..=34 | 92..=94 | 125..=132`).
pub fn is_ship_fitting_slot_flag(flag: InventoryFlag) -> bool {
    matches!(flag as u16, 11..=34 | 92..=94 | 125..=132)
}

/// Returns ordered candidate fitting slot flags inferred from `mod_type` dogma slot effects
/// (`hiPower` 12, `medPower` 13, `loPower` 11, `rigSlot` 2663, `subSystem` 3772, `serviceSlot`
/// 6306) or `category_id == 32`. Returns empty when `mod_type` carries no fitting-slot effect.
pub fn candidate_fitting_flags_for_type(mod_type: &InvType) -> Vec<InventoryFlag> {
    let effects = &mod_type.effect_ids;
    if effects.contains(&12) {
        vec![
            InventoryFlag::HiSlot0,
            InventoryFlag::HiSlot1,
            InventoryFlag::HiSlot2,
            InventoryFlag::HiSlot3,
            InventoryFlag::HiSlot4,
            InventoryFlag::HiSlot5,
            InventoryFlag::HiSlot6,
            InventoryFlag::HiSlot7,
        ]
    } else if effects.contains(&13) {
        vec![
            InventoryFlag::MedSlot0,
            InventoryFlag::MedSlot1,
            InventoryFlag::MedSlot2,
            InventoryFlag::MedSlot3,
            InventoryFlag::MedSlot4,
            InventoryFlag::MedSlot5,
            InventoryFlag::MedSlot6,
            InventoryFlag::MedSlot7,
        ]
    } else if effects.contains(&11) {
        vec![
            InventoryFlag::LoSlot0,
            InventoryFlag::LoSlot1,
            InventoryFlag::LoSlot2,
            InventoryFlag::LoSlot3,
            InventoryFlag::LoSlot4,
            InventoryFlag::LoSlot5,
            InventoryFlag::LoSlot6,
            InventoryFlag::LoSlot7,
        ]
    } else if effects.contains(&2663) {
        vec![
            InventoryFlag::RigSlot0,
            InventoryFlag::RigSlot1,
            InventoryFlag::RigSlot2,
        ]
    } else if mod_type.category_id == 32 || effects.contains(&3772) {
        if let Some(&req_slot_f) = mod_type.default_attributes.get(&1366) {
            vec![InventoryFlag::from_u16(req_slot_f as u16)]
        } else {
            vec![
                InventoryFlag::SubSystemSlot0,
                InventoryFlag::SubSystemSlot1,
                InventoryFlag::SubSystemSlot2,
                InventoryFlag::SubSystemSlot3,
                InventoryFlag::SubSystemSlot4,
            ]
        }
    } else if effects.contains(&6306) {
        (164_u16..=171).map(InventoryFlag::from_u16).collect()
    } else {
        Vec::new()
    }
}

/// Persists a fitted module's online state in `snap.module_online_states`.
pub fn record_fitted_module_online_state_in_snap(
    snap: &mut WorldStateSnapshot,
    item_id: i64,
    online: bool,
) {
    snap.module_online_states.insert(item_id, online);
}

/// Clears persisted runtime states (`online`, `active`, `active_charge`, `overload`) for a module
/// removed from a ship fitting slot.
pub fn clear_unfitted_module_state_in_snap(
    snap: &mut WorldStateSnapshot,
    ship_id: i64,
    item_id: i64,
) {
    snap.module_online_states.remove(&item_id);
    snap.clear_module_active_state(item_id);
    ServerContext::clear_persisted_module_overload(snap, ship_id, item_id);
}

/// Validates target hull compatibility and selects the fitting slot flag for `mod_type`.
///
/// When `explicit_flag` is `None`, candidate slots are filtered against `occupied_flags` and
/// simulated against `pre_fit_opt` (checking hull slot count, hardpoints, calibration, and ship
/// group/type restrictions) so failures abort before any DB item ID is consumed.
#[allow(clippy::too_many_arguments)]
pub fn validate_and_select_fit_slot(
    sde: &SdeRegistry,
    ship_id: i64,
    ship_type_id: u32,
    ship_cat_id: u32,
    mod_type: &InvType,
    explicit_flag: Option<InventoryFlag>,
    occupied_flags: &HashSet<InventoryFlag>,
    pre_fit_opt: Option<&ShipFittingEngine>,
    sim_item_id: i64,
) -> Result<InventoryFlag, DomainError> {
    let type_id = mod_type.type_id;
    let effects = &mod_type.effect_ids;
    let candidate_flags = candidate_fitting_flags_for_type(mod_type);
    let has_slot_effect = effects
        .iter()
        .any(|e| matches!(e, 11 | 12 | 13 | 2663 | 3772));
    let has_structure_slot_effect = has_slot_effect || effects.contains(&6306);
    let ship_group_id = sde.types.get(&ship_type_id).map(|t| t.group_id);

    if explicit_flag == Some(InventoryFlag::Cargo) {
        return Ok(InventoryFlag::Cargo);
    }
    if mod_type.category_id == 18 {
        return match explicit_flag {
            None | Some(InventoryFlag::DroneBay) => Ok(InventoryFlag::DroneBay),
            Some(other) => Err(fit_slot_validation_error(format!(
                "Drone type {type_id} cannot be fitted into slot flag {}",
                other as u16
            ))),
        };
    }
    if ship_cat_id == 6 && mod_type.category_id == 8 {
        return match explicit_flag {
            Some(ef) if matches!(ef as u16, 11..=34) => Ok(ef),
            _ => Err(fit_slot_validation_error(format!(
                "Charge type {type_id} ({}) requires an explicit module slot or Cargo flag",
                mod_type.name
            ))),
        };
    }
    if ship_cat_id == 6 && matches!(mod_type.category_id, 7 | 32) && has_slot_effect {
        if mod_type.category_id == 32 && ship_group_id != Some(963) {
            return Err(fit_slot_validation_error(format!(
                "Subsystem type {type_id} ({}) can only be fitted to Strategic Cruisers",
                mod_type.name
            )));
        }
        if let Some(ef) = explicit_flag {
            let raw = ef as u16;
            let slot_matches = match raw {
                11..=18 => effects.contains(&11),
                19..=26 => effects.contains(&13),
                27..=34 => effects.contains(&12),
                92..=94 => effects.contains(&2663),
                125..=132 => mod_type.category_id == 32 || effects.contains(&3772),
                _ => false,
            };
            if !slot_matches {
                return Err(fit_slot_validation_error(format!(
                    "Type {type_id} ({}) is incompatible with slot flag {raw}",
                    mod_type.name
                )));
            }
            return Ok(ef);
        }
        let mut chosen: Option<InventoryFlag> = None;
        let mut first_err: Option<String> = None;
        for cand in candidate_flags {
            if occupied_flags.contains(&cand) {
                continue;
            }
            if let Some(base_fit) = pre_fit_opt {
                let mut sim_fit = base_fit.clone();
                match sim_fit.fit_module_with_state(sim_item_id, type_id, cand as u16, false, sde) {
                    Ok(()) => {
                        chosen = Some(cand);
                        break;
                    }
                    Err(msg) => {
                        if first_err.is_none() {
                            first_err = Some(msg);
                        }
                    }
                }
            } else {
                chosen = Some(cand);
                break;
            }
        }
        return chosen.ok_or_else(|| {
            fit_slot_validation_error(first_err.unwrap_or_else(|| {
                format!("No free compatible fitting slot for module type {type_id} on ship {ship_id}")
            }))
        });
    }
    if ship_cat_id == 65 && mod_type.category_id == 66 && has_structure_slot_effect {
        if let Some(ef) = explicit_flag {
            if !matches!(ef as u16, 11..=34 | 92..=94 | 164..=171) {
                return Err(fit_slot_validation_error(format!(
                    "Structure module type {type_id} is incompatible with slot flag {}",
                    ef as u16
                )));
            }
            return Ok(ef);
        }
        if effects.contains(&6306) {
            return Ok((164_u16..=171)
                .map(InventoryFlag::from_u16)
                .find(|f| !occupied_flags.contains(f))
                .unwrap_or_else(|| InventoryFlag::from_u16(164)));
        }
        return Ok(candidate_flags
            .iter()
            .copied()
            .find(|f| !occupied_flags.contains(f))
            .or_else(|| candidate_flags.first().copied())
            .unwrap_or(InventoryFlag::LoSlot0));
    }

    Err(fit_slot_validation_error(format!(
        "Type {type_id} ({}) cannot be fitted onto target {ship_id}",
        mod_type.name
    )))
}

/// Evaluates whether fitting `type_id` into `flag` on `base_fit` can come online under `policy`,
/// falling back to `false` when slot fitting fails, skills are missing (when checked), or hull
/// CPU/PG capacity is exceeded.
pub fn evaluate_module_online_in_fit(
    sde: &SdeRegistry,
    base_fit: &ShipFittingEngine,
    sim_item_id: i64,
    type_id: u32,
    flag: InventoryFlag,
    policy: FitOnlinePolicy,
) -> bool {
    if !is_ship_fitting_slot_flag(flag) {
        return false;
    }
    if !sde
        .types
        .get(&type_id)
        .is_some_and(|t| matches!(t.category_id, 7 | 32))
    {
        return false;
    }
    let mut sim_fit = base_fit.clone();
    fit_and_try_online_in_engine(sde, &mut sim_fit, sim_item_id, type_id, flag, policy)
        .unwrap_or(false)
}

/// Fits a module into `engine` at `flag` and attempts to bring it online under `policy`.
///
/// Returns `Ok(true)` if fitted and onlined, `Ok(false)` if fitted offline because skills (when
/// checked), CPU/PG, or `maxGroupOnline` prevented onlining, or `Err(reason)` if slot fitting itself
/// failed.
pub fn fit_and_try_online_in_engine(
    sde: &SdeRegistry,
    engine: &mut ShipFittingEngine,
    module_item_id: i64,
    type_id: u32,
    flag: InventoryFlag,
    policy: FitOnlinePolicy,
) -> Result<bool, String> {
    engine.fit_module_with_state(module_item_id, type_id, flag as u16, false, sde)?;
    if !engine.modules.contains_key(&module_item_id) {
        return Ok(false);
    }
    let online_res = match policy {
        FitOnlinePolicy::CheckSkillsAndResources => engine.set_module_online(module_item_id, sde),
        FitOnlinePolicy::SkipSkillsCheckResources => {
            engine.set_module_online_skip_skills(module_item_id, sde)
        }
    };
    Ok(online_res.is_ok())
}

/// Atomically validates target hull + slot compatibility, mints a singleton item into the target
/// ship or structure, evaluates online eligibility against the ship fitting, and persists
/// `module_online_states` in a single `try_with_mut_snapshot` commit (`#1193`).
pub fn fit_newly_minted_item_tx(
    ctx: &ServerContext,
    req: MintAndFitRequest,
) -> Result<MintAndFitOutcome, DomainError> {
    let Some(mod_type) = ctx.sde.types.get(&req.type_id) else {
        return Err(DomainError::not_found(
            "TypeNotFound",
            format!("Module type {} not found in SDE", req.type_id),
        ));
    };

    let Some((ship_owner_id, ship_cat_id)) = ctx.db.with_snapshot(|snap| {
        snap.items.get(&req.ship_id).map(|ship_item| {
            let cat = ctx
                .sde
                .types
                .get(&ship_item.type_id)
                .map(|t| t.category_id)
                .unwrap_or(6);
            (ship_item.owner_id, cat)
        })
    }) else {
        return Err(DomainError::not_found(
            "ShipNotFound",
            format!("Target ship {} not found", req.ship_id),
        ));
    };

    // Build dogma simulation before opening the write lock to obey Systems -> DB lock hierarchy.
    let pre_fit_opt = if ship_cat_id == 6 {
        ctx.fit(ship_owner_id, req.ship_id).ok()
    } else {
        None
    };

    let outcome = ctx
        .db
        .try_with_mut_snapshot(|snap| -> Result<MintAndFitOutcome, DomainError> {
            let Some(ship_item) = snap.items.get(&req.ship_id).cloned() else {
                return Err(DomainError::not_found(
                    "ShipNotFound",
                    format!("Target ship {} not found", req.ship_id),
                ));
            };
            let live_cat_id = ctx
                .sde
                .types
                .get(&ship_item.type_id)
                .map(|t| t.category_id)
                .unwrap_or(6);
            let occupied_flags: HashSet<InventoryFlag> = snap
                .items
                .values()
                .filter(|i| i.location_id == req.ship_id)
                .map(|i| i.flag)
                .collect();
            let chosen_flag = validate_and_select_fit_slot(
                &ctx.sde,
                req.ship_id,
                ship_item.type_id,
                live_cat_id,
                mod_type,
                req.explicit_flag,
                &occupied_flags,
                pre_fit_opt.as_ref(),
                -(snap.next_entity_id + 1),
            )?;

            let granted = grant_item_in_snap(
                Some(&ctx.sde),
                snap,
                None,
                GrantItemRequest::explicit(
                    ship_item.owner_id,
                    req.ship_id,
                    chosen_flag,
                    req.type_id,
                    1,
                )
                .with_singleton(true)
                .with_merge_existing_stack(false),
            )?;

            let is_fitting_slot = is_fitting_slot_flag(chosen_flag);
            let is_online = if live_cat_id == 6 && is_ship_fitting_slot_flag(chosen_flag) {
                pre_fit_opt.as_ref().is_some_and(|base_fit| {
                    evaluate_module_online_in_fit(
                        &ctx.sde,
                        base_fit,
                        granted.item.item_id,
                        req.type_id,
                        chosen_flag,
                        req.online_policy,
                    )
                })
            } else {
                false
            };

            if is_fitting_slot && mod_type.category_id != 8 {
                record_fitted_module_online_state_in_snap(snap, granted.item.item_id, is_online);
            }

            Ok(MintAndFitOutcome {
                item: granted.item,
                chosen_flag,
                is_online,
                is_fitting_slot,
                ship_category_id: live_cat_id,
            })
        })?;

    ctx.invalidate_ship_fitting(req.ship_id);
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::account_services::account_services_tests::test_ctx;

    #[test]
    fn slot_flag_classification_and_candidates_match_sde_effects() {
        assert!(is_fitting_slot_flag(InventoryFlag::LoSlot0));
        assert!(is_fitting_slot_flag(InventoryFlag::HiSlot7));
        assert!(is_fitting_slot_flag(InventoryFlag::RigSlot2));
        assert!(is_fitting_slot_flag(InventoryFlag::SubSystemSlot4));
        assert!(is_fitting_slot_flag(InventoryFlag::ServiceSlot0));
        assert!(is_fitting_slot_flag(InventoryFlag::ServiceSlot7));
        assert!(!is_fitting_slot_flag(InventoryFlag::Cargo));
        assert!(!is_fitting_slot_flag(InventoryFlag::DroneBay));

        assert!(is_ship_fitting_slot_flag(InventoryFlag::SubSystemSlot4));
        assert!(!is_ship_fitting_slot_flag(InventoryFlag::ServiceSlot0));

        let ctx = test_ctx();
        assert_eq!(
            candidate_fitting_flags_for_type(&ctx.sde.types[&484])[0],
            InventoryFlag::HiSlot0
        );
        assert_eq!(
            candidate_fitting_flags_for_type(&ctx.sde.types[&439])[0],
            InventoryFlag::MedSlot0
        );
        assert_eq!(
            candidate_fitting_flags_for_type(&ctx.sde.types[&2048])[0],
            InventoryFlag::LoSlot0
        );
        assert_eq!(
            candidate_fitting_flags_for_type(&ctx.sde.types[&4369])[0],
            InventoryFlag::RigSlot0
        );
        assert_eq!(
            candidate_fitting_flags_for_type(&ctx.sde.types[&45586]),
            vec![InventoryFlag::SubSystemSlot1]
        );
        assert_eq!(
            candidate_fitting_flags_for_type(&ctx.sde.types[&35877]).len(),
            8
        );
        assert_eq!(
            candidate_fitting_flags_for_type(&ctx.sde.types[&35877])[0],
            InventoryFlag::ServiceSlot0
        );
        assert!(candidate_fitting_flags_for_type(&ctx.sde.types[&1201]).is_empty());
        assert!(candidate_fitting_flags_for_type(&ctx.sde.types[&34]).is_empty());
    }

    #[test]
    fn snap_online_and_unfit_cleanup_helpers_manage_runtime_maps() {
        let mut snap = WorldStateSnapshot::default();
        let ship_id = 100_000_001;
        let mod_id = 100_000_999;

        record_fitted_module_online_state_in_snap(&mut snap, mod_id, true);
        assert_eq!(snap.module_online_states.get(&mod_id), Some(&true));

        snap.module_active_states.insert(mod_id);
        snap.module_active_charges.insert(mod_id, 185);
        snap.ship_fit_inputs.insert(
            ship_id,
            eve_db::ShipFitInputsRecord {
                active_stance_type_id: None,
                last_stance_switch_ms: None,
                overloaded_modules: std::collections::BTreeSet::from([mod_id]),
            },
        );

        clear_unfitted_module_state_in_snap(&mut snap, ship_id, mod_id);
        assert!(!snap.module_online_states.contains_key(&mod_id));
        assert!(!snap.module_active_states.contains(&mod_id));
        assert!(!snap.module_active_charges.contains_key(&mod_id));
        assert!(!snap.ship_fit_inputs.contains_key(&ship_id));
    }
}
