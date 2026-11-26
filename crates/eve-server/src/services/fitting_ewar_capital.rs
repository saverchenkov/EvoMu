//! `fitting_ewar_capital` — EWAR, capital module, remote repair & `charFittingMgr`/`corpFittingMgr` handlers.

#![forbid(unsafe_code)]

use super::*;

#[derive(Clone, Debug)]
pub(crate) struct PendingWeaponAutoload {
    pub cargo_stack_id: i64,
    pub ammo_type_id: u32,
    pub owner_id: i64,
    pub total_take: i32,
    pub slot_allocations: Vec<(i64, eve_db::InventoryFlag, i32)>,
    pub consume_one_round_per_slot: bool,
}

#[derive(Default)]
pub(crate) struct DeferredPostCycleEffects {
    pub projected_assist_dist_m: Option<f64>,
    pub fuel_to_deduct: Option<(u32, i32)>,
    pub phenomena_debuffs: Option<(Vec<i64>, Vec<(u32, f64)>, u64)>,
    pub burst_projector: Option<(DVec3, f64, Vec<i64>)>,
    pub bastion_weapon_timer: bool,
    pub passive_targeter_lock: Option<eve_destiny::TargetLock>,
    pub cap_booster_charge_id: Option<i64>,
    pub ancillary_charge_to_consume: Option<(i64, i32)>,
    pub turret_charge_consumptions: Vec<(i64, i64)>,
    pub weapon_autoload: Option<PendingWeaponAutoload>,
    pub projected_ewar: Option<(i64, eve_dogma::ProjectedEwarEffect)>,
    pub burst_jammer_plan: Option<super::fitting_module_cycles::PreparedBurstJammerPlan>,
}

pub(crate) fn is_wear_only_non_singleton_crystal(item: &eve_db::InventoryItemRecord) -> bool {
    !item.singleton && item.quantity == 1 && item.source_mutaplasmid_id.is_none()
        && item.insured_payout_isk.is_finite() && item.insured_payout_isk == 0.0
        && !item.dynamic_attributes.is_empty()
        && item.dynamic_attributes.keys().all(|k| *k == eve_sde::attr_ids::DAMAGE || *k == eve_sde::attr_ids::HP_STRUCT)
}

fn is_weapon_autoload_stack_consumable(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    char_id: i64,
    ship_id: i64,
    item: &eve_db::InventoryItemRecord,
    mod_type: &eve_sde::InvType,
) -> bool {
    if item.location_id != ship_id || item.quantity <= 0 || item.owner_id != char_id {
        return false;
    }
    let is_consumable = match item.flag {
        eve_db::InventoryFlag::Cargo => {
            snap.is_item_consumable_by(item, char_id, None, Some(ship_id))
        }
        eve_db::InventoryFlag::SpecializedAmmoHold => {
            !snap.is_item_or_ancestor_locked_or_impounded(item.item_id)
                && !snap.is_item_locked_in_contract_or_job(ship_id)
                && !snap.is_item_or_ancestor_locked_or_impounded(ship_id)
                && snap.is_ship_colocated_with_character(char_id, ship_id, None)
        }
        _ => false,
    };
    if !is_consumable {
        return false;
    }
    let Some(ct) = ctx.sde.types.get(&item.type_id) else {
        return false;
    };
    let is_crystal_type = matches!(ct.group_id, 86 | 374 | 375 | 482 | 663);
    let is_crystal_exact = is_crystal_type
        && item.quantity == 1
        && (item.singleton || is_wear_only_non_singleton_crystal(item));
    if item.singleton && !is_crystal_exact {
        return false;
    }
    let policy = if is_crystal_exact {
        eve_db::ItemClassPolicy::ExactAssembledItem
    } else {
        eve_db::ItemClassPolicy::StackableCommodity
    };
    let opts = eve_db::ConsumeExactOptions {
        reject_fitted: false,
        active_ship_id: Some(ship_id),
        ..eve_db::ConsumeExactOptions::for_owner_exact(
            Some(char_id),
            char_id,
            policy,
        )
    };
    if snap.validate_item_for_consumption(item, &opts).is_err() {
        return false;
    }
    ServerContext::is_charge_compatible_with_weapon(mod_type, ct)
}

pub(crate) fn plan_weapon_autoload(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    module_id: i64,
    mod_type: &eve_sde::InvType,
) -> Result<Option<PendingWeaponAutoload>, BridgeRpcError> {
    ctx.db.with_snapshot(|snap| {
        let bank = snap
            .weapon_banks
            .get(&ship_id)
            .and_then(|b| b.get(&module_id))
            .cloned()
            .unwrap_or_else(|| vec![module_id]);
        let valid_slots: Vec<(i64, eve_db::InventoryFlag)> = bank
            .iter()
            .filter_map(|&tid| snap.items.get(&tid).map(|i| (tid, i.flag)))
            .collect();
        if valid_slots.is_empty() {
            return Ok(None);
        }

        let mut candidates: Vec<&eve_db::InventoryItemRecord> = snap
            .items
            .values()
            .filter(|i| {
                is_weapon_autoload_stack_consumable(ctx, snap, char_id, ship_id, i, mod_type)
            })
            .collect();
        candidates.sort_by_key(|i| i.item_id);

        let mut selected: Option<&eve_db::InventoryItemRecord> = None;
        let mut first_skill_err: Option<BridgeRpcError> = None;
        for cand in candidates {
            match ctx.check_type_skill_prerequisites_in_snapshot(snap, char_id, cand.type_id) {
                Ok(()) => {
                    selected = Some(cand);
                    break;
                }
                Err(e) => {
                    if first_skill_err.is_none() {
                        first_skill_err = Some(e);
                    }
                }
            }
        }
        let Some(chosen) = selected else {
            if let Some(err) = first_skill_err {
                return Err(err);
            }
            return Ok(None);
        };

        let is_guided_missile =
            crate::system_actor::is_guided_missile_launcher_group(i64::from(mod_type.group_id));
        let num_mods = valid_slots.len() as i32;
        if is_guided_missile && chosen.quantity < num_mods {
            return Err(BridgeRpcError::no_charges(mod_type.type_id as i64));
        }

        let charge_vol = ctx
            .sde
            .types
            .get(&chosen.type_id)
            .map(|t| t.volume.max(0.0001))
            .unwrap_or(0.0025);
        let cap_rounds = if mod_type.capacity > 0.0 {
            (mod_type.capacity / charge_vol).floor() as i32
        } else {
            1
        }
        .max(1);
        let total_take = (cap_rounds * num_mods).min(chosen.quantity).max(1);
        let base = total_take / num_mods;
        let rem = total_take % num_mods;
        let slot_allocations: Vec<(i64, eve_db::InventoryFlag, i32)> = valid_slots
            .into_iter()
            .enumerate()
            .filter_map(|(idx, (tid, tf))| {
                let give = base + if (idx as i32) < rem { 1 } else { 0 };
                (give > 0).then_some((tid, tf, give))
            })
            .collect();

        Ok(Some(PendingWeaponAutoload {
            cargo_stack_id: chosen.item_id,
            ammo_type_id: chosen.type_id,
            owner_id: chosen.owner_id,
            total_take,
            slot_allocations,
            consume_one_round_per_slot: false,
        }))
    })
}

pub(crate) struct CommittedWeaponAutoload {
    pub autoload_diffs: Vec<(
        Option<eve_db::InventoryItemRecord>,
        Option<eve_db::InventoryItemRecord>,
    )>,
    pub loaded_bank: Vec<i64>,
    pub cycle_consume_diffs: Vec<(
        eve_db::InventoryItemRecord,
        Option<eve_db::InventoryItemRecord>,
    )>,
    pub created_slot_charges: Vec<(i64, i64)>,
}

fn consume_cycle_assembled_charge(
    snap: &mut eve_db::WorldStateSnapshot,
    char_id: i64,
    ship_id: i64,
    mod_type_id: u32,
    charge_id: i64,
    qty: i32,
) -> Result<(eve_db::InventoryItemRecord, Option<eve_db::InventoryItemRecord>), BridgeRpcError> {
    let pre = snap.items.get(&charge_id).cloned().ok_or_else(|| {
        BridgeRpcError::no_charges_loaded(
            mod_type_id as i64,
            format!("Loaded charge {charge_id} not found"),
        )
    })?;
    let opts = eve_db::ConsumeExactOptions {
        reject_fitted: false,
        active_ship_id: Some(ship_id),
        ..eve_db::ConsumeExactOptions::for_owner_exact(
            Some(char_id),
            char_id,
            eve_db::ItemClassPolicy::ExactAssembledItem,
        )
    };
    let post = snap
        .consume_exact_item_quantity(charge_id, qty, opts)
        .map_err(|e| BridgeRpcError::no_charges_loaded(mod_type_id as i64, e.to_string()))?;
    Ok((pre, post))
}

pub(crate) fn commit_pending_weapon_autoload_in_snapshot(
    ctx: &ServerContext,
    snap: &mut eve_db::WorldStateSnapshot,
    char_id: i64,
    ship_id: i64,
    mod_type: &eve_sde::InvType,
    plan: &PendingWeaponAutoload,
) -> Result<CommittedWeaponAutoload, BridgeRpcError> {
    ctx.check_type_skill_prerequisites_in_snapshot(snap, char_id, plan.ammo_type_id)?;
    let Some(stack_pre) = snap.items.get(&plan.cargo_stack_id).cloned() else {
        return Err(BridgeRpcError::no_charges(mod_type.type_id as i64));
    };
    if stack_pre.type_id != plan.ammo_type_id
        || stack_pre.quantity < plan.total_take
        || !is_weapon_autoload_stack_consumable(ctx, snap, char_id, ship_id, &stack_pre, mod_type)
    {
        return Err(BridgeRpcError::no_charges(mod_type.type_id as i64));
    }
    for &(slot_id, slot_flag, _) in &plan.slot_allocations {
        if !snap.items.contains_key(&slot_id) {
            return Err(BridgeRpcError::no_charges(mod_type.type_id as i64));
        }
        let has_existing = snap.items.values().any(|i| {
            crate::services::inventory_fitting::fitting_module_cycles::is_slot_loaded_charge_item(
                ctx,
                mod_type,
                i,
                ship_id,
                slot_flag as u16,
                slot_id,
            )
        });
        if has_existing {
            return Err(BridgeRpcError::no_charges(mod_type.type_id as i64));
        }
    }

    let mut autoload_diffs = Vec::new();
    let mut loaded_bank = Vec::with_capacity(plan.slot_allocations.len());
    let mut created_slot_charges: Vec<(i64, i64)> = Vec::with_capacity(plan.slot_allocations.len());
    let pre_dynamic_attributes = stack_pre.dynamic_attributes.clone();
    let pre_source_mutaplasmid_id = stack_pre.source_mutaplasmid_id;

    let is_crystal_type = ctx.sde.types.get(&stack_pre.type_id).is_some_and(|ct| {
        matches!(ct.group_id, 86 | 374 | 375 | 482 | 663)
    });
    let is_single_crystal_move = is_crystal_type
        && stack_pre.quantity == 1
        && (stack_pre.singleton || is_wear_only_non_singleton_crystal(&stack_pre));
    if is_single_crystal_move && plan.total_take == 1 && plan.slot_allocations.len() == 1 {
        let (tid, tf, _) = plan.slot_allocations[0];
        let mut moved = stack_pre.clone();
        if let Some(st) = snap.items.get_mut(&plan.cargo_stack_id) {
            st.flag = tf;
            st.singleton = true;
            st.version += 1;
            st.dynamic_attributes = pre_dynamic_attributes.clone();
            st.source_mutaplasmid_id = pre_source_mutaplasmid_id;
            moved = st.clone();
        }
        snap.module_active_charges.insert(tid, plan.ammo_type_id);
        autoload_diffs.push((Some(stack_pre), Some(moved)));
        loaded_bank.push(tid);
        created_slot_charges.push((tid, plan.cargo_stack_id));
    } else {
        let stack_opts = eve_db::ConsumeExactOptions {
            reject_fitted: false,
            active_ship_id: Some(ship_id),
            ..eve_db::ConsumeExactOptions::for_owner_exact(
                Some(char_id),
                plan.owner_id,
                eve_db::ItemClassPolicy::StackableCommodity,
            )
        };
        let post_stack = snap
            .consume_exact_item_quantity(plan.cargo_stack_id, plan.total_take, stack_opts)
            .map_err(|_| BridgeRpcError::no_charges(mod_type.type_id as i64))?;
        autoload_diffs.push((Some(stack_pre), post_stack));
        for &(tid, tf, give) in &plan.slot_allocations {
            if give <= 0 {
                continue;
            }
            let cid = snap.alloc_entity_id();
            let new_rec = eve_db::InventoryItemRecord {
                item_id: cid,
                type_id: plan.ammo_type_id,
                owner_id: plan.owner_id,
                location_id: ship_id,
                flag: tf,
                quantity: give,
                singleton: is_crystal_type && give == 1,
                version: 1,
                ..Default::default()
            };
            snap.items.insert(cid, new_rec.clone());
            snap.module_active_charges.insert(tid, plan.ammo_type_id);
            autoload_diffs.push((None, Some(new_rec)));
            loaded_bank.push(tid);
            created_slot_charges.push((tid, cid));
        }
    }

    let mut cycle_consume_diffs = Vec::new();
    let mut final_slot_charges = Vec::with_capacity(created_slot_charges.len());
    if plan.consume_one_round_per_slot {
        for (tid, cid) in created_slot_charges {
            let (pre, post) =
                consume_cycle_assembled_charge(snap, char_id, ship_id, mod_type.type_id, cid, 1)?;
            if post.is_none() {
                snap.module_active_charges.remove(&tid);
            } else {
                final_slot_charges.push((tid, cid));
            }
            cycle_consume_diffs.push((pre, post));
        }
    } else {
        final_slot_charges = created_slot_charges;
    }

    Ok(CommittedWeaponAutoload {
        autoload_diffs,
        loaded_bank,
        cycle_consume_diffs,
        created_slot_charges: final_slot_charges,
    })
}

pub(crate) fn emit_committed_weapon_autoload_notifications(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    autoload_diffs: &[(
        Option<eve_db::InventoryItemRecord>,
        Option<eve_db::InventoryItemRecord>,
    )],
    loaded_bank: &[i64],
) {
    ctx.invalidate_and_push_fit(ship_id);
    crate::services::inventory_fitting::fitting_module_cycles::emit_ship_charge_sublocation_diffs(
        ctx,
        char_id,
        ship_id,
        autoload_diffs,
    );
    crate::services::notify_emitters_seasons_tactical::emit_charge_and_bank_ammo_loaded(
        ctx,
        char_id,
        ship_id,
        loaded_bank,
    );
}

pub(crate) fn commit_and_emit_weapon_autoload(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    mod_type: &eve_sde::InvType,
    plan: &PendingWeaponAutoload,
) -> Result<CommittedWeaponAutoload, BridgeRpcError> {
    let committed = ctx.db.try_with_mut_snapshot(|snap| {
        commit_pending_weapon_autoload_in_snapshot(ctx, snap, char_id, ship_id, mod_type, plan)
    })?;
    emit_committed_weapon_autoload_notifications(
        ctx,
        char_id,
        ship_id,
        &committed.autoload_diffs,
        &committed.loaded_bank,
    );
    Ok(committed)
}

/// Collects eligible ship fuel stacks for `fuel_type_id` in `SpecializedFuelBay` (`133`)
/// and `Cargo` (`5`), sorted with `SpecializedFuelBay` prioritized before `Cargo`, then `item_id`.
fn collect_ship_fuel_stacks(
    snap: &eve_db::WorldStateSnapshot,
    ship_id: i64,
    fuel_type_id: u32,
) -> Vec<(u8, i64, i32)> {
    let mut stacks: Vec<(u8, i64, i32)> = snap
        .items
        .values()
        .filter(|i| {
            i.location_id == ship_id
                && i.type_id == fuel_type_id
                && matches!(i.flag as u16, 5 | 133)
                && i.flag != eve_db::InventoryFlag::Locked
                && i.quantity > 0
                && snap
                    .validate_item_for_consumption(
                        i,
                        &eve_db::ConsumeExactOptions::for_owner_exact(
                            None,
                            i.owner_id,
                            eve_db::ItemClassPolicy::StackableCommodity,
                        ),
                    )
                    .is_ok()
        })
        .map(|i| (if (i.flag as u16) == 133 { 0 } else { 1 }, i.item_id, i.quantity))
        .collect();
    stacks.sort_by_key(|&(prio, iid, _)| (prio, iid));
    stacks
}

#[derive(Default)]
pub(crate) struct EwarCapitalPrep {
    pub warp_scramble_points: i32,
    pub disables_target_mwd_mjd: bool,
    pub warp_core_stab_bonus: i32,
    pub hic_bubble_radius_m: Option<f64>,
    pub hic_velocity_multiplier: Option<f64>,
    pub hic_sig_multiplier: Option<f64>,
    pub hic_mass_addition: Option<f64>,
    pub mjd_jump_distance_m: Option<f64>,
    pub mjfg_field_params: Option<(f64, usize, usize)>,
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
    pub grappler_optimal_m: f64,
    pub grappler_falloff_m: f64,
    pub grappler_speed_factor_pct: f64,
    pub deferred_post_cycle: DeferredPostCycleEffects,
}

/// Measures surface-to-surface distance between two balls sequentially so
/// callers avoid polling two `get_ball_snapshot` futures simultaneously.
#[inline(never)]
pub(crate) async fn measure_ball_surface_distance_m(
    sys: &crate::system_actor::SystemHandle,
    ship_id: i64,
    target_id: i64,
) -> Option<f64> {
    let (_, ship_ball) = sys.get_ball_snapshot(ship_id).await.ok().flatten()?;
    let (_, target_ball) = sys.get_ball_snapshot(target_id).await.ok().flatten()?;
    Some(ship_ball.surface_distance(&target_ball))
}

#[inline(never)]
async fn check_mwd_deadspace_allowed(
    ctx: &ServerContext,
    sys: &crate::system_actor::SystemHandle,
    sys_id: u32,
    ship_id: i64,
) -> Result<(), BridgeRpcError> {
    if let Ok(Some((_, sb))) = sys.get_ball_snapshot(ship_id).await {
        crate::services::pve_dungeon_runtime::check_deadspace_mwd_allowed(
            ctx,
            sys_id,
            sb.position.to_array(),
        )?;
    }
    Ok(())
}

#[inline(never)]
fn resolve_pending_weapon_autoload(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    module_id: i64,
    m: &eve_dogma::FittedModule,
    mod_type: &eve_sde::InvType,
    mod_state: &eve_dogma::DogmaItemState,
) -> Result<Option<PendingWeaponAutoload>, BridgeRpcError> {
    if !matches!(
        mod_type.group_id,
        53 | 55
            | 74
            | 1986
            | 4060
            | 506..=512
            | 524
            | 771
            | 862
            | 1245
            | 1674
            | 4500
            | 4805
            | 4807
    ) {
        return Ok(None);
    }
    let has_builtin_damage = [114u16, 116, 117, 118].iter().any(|aid| {
        mod_type.default_attributes.get(aid).copied().unwrap_or(0.0) > 0.0
            || mod_state.get_attribute(*aid, &ctx.sde) > 0.0
    });
    let uses_charge_groups = mod_type
        .default_attributes
        .keys()
        .any(|&aid| matches!(aid, 604 | 605 | 606 | 609 | 610));
    let requires_ammo = mod_type.capacity > 0.0 && (uses_charge_groups || !has_builtin_damage);
    if !requires_ammo {
        return Ok(None);
    }
    let has_slot_item = ctx.db.with_snapshot(|snap| {
        let bank = snap
            .weapon_banks
            .get(&ship_id)
            .and_then(|b| b.get(&module_id))
            .cloned()
            .unwrap_or_else(|| vec![module_id]);
        bank.iter().any(|&tid| {
            let tf = snap
                .items
                .get(&tid)
                .map(|i| i.flag as u16)
                .unwrap_or(m.flag_id);
            snap.items.values().any(|i| {
                crate::services::inventory_fitting::fitting_module_cycles::is_slot_loaded_charge_item(
                    ctx,
                    mod_type,
                    i,
                    ship_id,
                    tf,
                    tid,
                )
            })
        })
    });
    let mut pending_weapon_autoload = None;
    let mut has_loaded_ammo = has_slot_item;
    if !has_loaded_ammo {
        match plan_weapon_autoload(ctx, char_id, ship_id, module_id, mod_type) {
            Ok(Some(plan)) => {
                has_loaded_ammo = true;
                pending_weapon_autoload = Some(plan);
            }
            Ok(None) => {}
            Err(e) => {
                if !(m.charge_type_id.is_some() && m.charge_quantity > 0) {
                    return Err(e);
                }
            }
        }
    }
    if !has_loaded_ammo {
        has_loaded_ammo = (m.charge_type_id.is_some() && m.charge_quantity > 0)
            || ctx.db.with_snapshot(|snap| {
                let bank = snap
                    .weapon_banks
                    .get(&ship_id)
                    .and_then(|b| b.get(&module_id))
                    .cloned()
                    .unwrap_or_else(|| vec![module_id]);
                bank.iter().any(|&tid| snap.module_active_charges.contains_key(&tid))
            });
    }
    if !has_loaded_ammo {
        return Err(
            crate::services::combat_crimewatch::combat_weapons::weapon_no_ammo_loaded_error(
                Some(module_id),
                mod_type,
                None,
                format!("Weapon module {module_id} has no compatible ammunition loaded"),
            ),
        );
    }
    Ok(pending_weapon_autoload)
}

#[inline(never)]
async fn validate_interdiction_nullifier(
    sys: &crate::system_actor::SystemHandle,
    ship_id: i64,
    ship_group_id: u32,
    ship_type_id: u32,
    mod_type: &eve_sde::InvType,
) -> Result<(), BridgeRpcError> {
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
                if t as u32 == ship_type_id {
                    hull_allowed = true;
                }
            }
        }
    }
    if !hull_allowed {
        return Err(BridgeRpcError {
            code: "InvalidShipGroup".into(),
            message: format!(
                "Interdiction Nullifier cannot be activated on hull group {ship_group_id}"
            ),
            client_error: None,
        });
    }
    if let Ok(Some((_, sb))) = sys.get_ball_snapshot(ship_id).await {
        if matches!(
            sb.mode,
            eve_destiny::MovementMode::Warp { phase, .. }
                if phase != eve_destiny::WarpPhase::Aligning
        ) {
            return Err(BridgeRpcError {
                code: "CannotActivateInWarp".into(),
                message: "Interdiction Nullifier must be activated before entering the warp tunnel".into(),
                client_error: None,
            });
        }
    }
    Ok(())
}

#[inline(never)]
fn validate_adc_cehe_triage_restrictions(
    ship_group_id: u32,
    m: &eve_dogma::FittedModule,
    mod_type: &eve_sde::InvType,
    fit: &eve_dogma::ShipFittingEngine,
) -> Result<bool, BridgeRpcError> {
    // Assault Damage Control (`47254` T1, `47257` T2, `group_id == 60`, `effect 7012`, `feat:modules.assault-damage-control`)
    // & Capital Emergency Hull Energizer (`40714` T1, `40717` T2, `group_id == 60`, `effect 6484`, `feat:modules.capital-emergency-hull-energizer`)
    let is_adc = mod_type.group_id == 60 && mod_type.effect_ids.contains(&7012);
    if is_adc && !matches!(ship_group_id, 324 | 358) {
        return Err(BridgeRpcError {
            code: "InvalidShipGroup".into(),
            message: "Assault Damage Control can only be fitted/activated on Assault Frigates (324) or Heavy Assault Cruisers (358)".into(),
            client_error: None,
        });
    }
    let is_cehe = matches!(m.type_id, 40714..=40718) || mod_type.effect_ids.contains(&6484);
    if is_cehe && !matches!(ship_group_id, 547 | 1538 | 485 | 30 | 659 | 883 | 4594) {
        return Err(BridgeRpcError {
            code: "InvalidShipGroup".into(),
            message: "Capital Emergency Hull Energizer can only be activated on capital hulls".into(),
            client_error: None,
        });
    }
    let has_active_triage = fit.modules.values().any(|fm| {
        matches!(fm.type_id, 27951 | 4294)
            && matches!(fm.state, eve_dogma::ModuleState::Active | eve_dogma::ModuleState::Overloaded)
    });
    if has_active_triage && matches!(mod_type.group_id, 201 | 208 | 291 | 379 | 52 | 65 | 1672) {
        return Err(BridgeRpcError {
            code: "EwarDisabledInTriage".into(),
            message: "EWAR modules are disabled while Triage Module is active".into(),
            client_error: None,
        });
    }
    Ok(is_adc)
}

#[inline(never)]
async fn prepare_projected_assist_range(
    ctx: &ServerContext,
    sys: &crate::system_actor::SystemHandle,
    char_id: i64,
    ship_id: i64,
    mod_type: &eve_sde::InvType,
    mod_state: &eve_dogma::DogmaItemState,
    target_id_opt: Option<i64>,
) -> Result<f64, BridgeRpcError> {
    let Some(tid) = target_id_opt else {
        return Err(BridgeRpcError {
            code: "TargetRequired".into(),
            message: format!("{} requires a locked target", mod_type.name),
            client_error: None,
        });
    };
    let dist_m = measure_ball_surface_distance_m(sys, ship_id, tid)
        .await
        .unwrap_or(0.0);
    let optimal = mod_state.get_attribute(54, &ctx.sde);
    let falloff = mod_state.get_attribute(2044, &ctx.sde);
    if optimal > 0.0 && dist_m > optimal + 2.0 * falloff + 1.0 {
        return Err(BridgeRpcError {
            code: "OutOfRange".into(),
            message: format!("{} target at {dist_m:.1}m exceeds optimal ({optimal:.1}m) + 2*falloff ({falloff:.1}m)", mod_type.name),
            client_error: None,
        });
    }
    crate::services::tactical_modules::precheck_projected_remote_assistance_safety(
        ctx, char_id, tid,
    )?;
    Ok(dist_m)
}

#[inline(never)]
fn prepare_zero_point_mass_entangler(
    ship_group_id: u32,
    ship_type_id: u32,
    mod_state: &eve_dogma::DogmaItemState,
    sde: &eve_sde::SdeRegistry,
    prep: &mut EwarCapitalPrep,
) -> Result<(), BridgeRpcError> {
    // Zero-Point Mass Entangler (`49099`, `group_id == 2008`, `feat:modules.zero-point-mass-entangler`)
    if !matches!(ship_group_id, 894 | 4902) && ship_type_id != 89_607 {
        return Err(BridgeRpcError {
            code: "InvalidShipGroup".into(),
            message: "Zero-Point Mass Entangler can only be activated on Heavy Interdiction Cruisers (894) or Odysseus (89607)".into(),
            client_error: None,
        });
    }
    prep.self_disable_mwd_mjd = true;
    let mass_pct = mod_state.get_attribute(1131, sde);
    let mass_mult = if mass_pct != 0.0 { (1.0 + mass_pct / 100.0).clamp(0.05, 1.0) } else { 0.20 };
    let vel_pct = mod_state.get_attribute(2603, sde);
    let vel_mult = if vel_pct != 0.0 { (1.0 + vel_pct / 100.0).clamp(0.01, 1.0) } else { 0.05 };
    prep.self_mass_multiplier = Some(mass_mult);
    prep.hic_velocity_multiplier = Some(vel_mult);
    prep.warp_core_stab_bonus = 2;
    Ok(())
}

#[inline(never)]
fn prepare_tactical_mode_sync(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    ship_group_id: u32,
    m: &eve_dogma::FittedModule,
    mod_type: &eve_sde::InvType,
    mod_state: &eve_dogma::DogmaItemState,
    prep: &mut EwarCapitalPrep,
) -> Result<bool, BridgeRpcError> {
    let is_siege = matches!(m.type_id, 20280 | 4292) || mod_type.name.contains("Siege Module");
    let is_triage = matches!(m.type_id, 27951 | 4294) || mod_type.name.contains("Triage Module");
    let is_bastion = m.type_id == 33400 || mod_type.name.contains("Bastion Module");
    let is_phenomena = mod_type.group_id == 1815;

    if is_siege && !matches!(ship_group_id, 485 | 4594) {
        return Err(BridgeRpcError { code: "InvalidShipGroup".into(), message: "Siege Module can only be activated on Dreadnoughts (485) or Lancer Dreadnoughts (4594)".into(), client_error: None });
    }
    if is_triage && !matches!(ship_group_id, 1538 | 547) {
        return Err(BridgeRpcError { code: "InvalidShipGroup".into(), message: "Triage Module can only be activated on Force Auxiliaries (group 1538)".into(), client_error: None });
    }
    if is_bastion && ship_group_id != 900 {
        return Err(BridgeRpcError { code: "InvalidShipGroup".into(), message: "Bastion Module can only be activated on Marauders (group 900)".into(), client_error: None });
    }
    if is_phenomena && ship_group_id != 30 {
        return Err(BridgeRpcError { code: "InvalidShipGroup".into(), message: "Phenomena Generator can only be activated on Titans (group 30)".into(), client_error: None });
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
        let fuel_check = ctx.db.with_snapshot(|snap| {
            let total_avail: i64 = collect_ship_fuel_stacks(snap, ship_id, fuel_type_id)
                .into_iter()
                .map(|(_, _, q)| i64::from(q))
                .sum();
            if total_avail >= i64::from(fuel_qty_needed) {
                Ok(true)
            } else if total_avail > 0 {
                Err(format!("Insufficient fuel (type {fuel_type_id}): need {fuel_qty_needed}, have {total_avail}"))
            } else if require_strict {
                Err(format!("Missing required fuel (type {fuel_type_id}, quantity {fuel_qty_needed})"))
            } else {
                Ok(false)
            }
        });
        match fuel_check {
            Ok(true) => {
                prep.deferred_post_cycle.fuel_to_deduct = Some((fuel_type_id, fuel_qty_needed));
            }
            Ok(false) => {}
            Err(msg) => {
                return Err(BridgeRpcError { code: "InsufficientFuel".into(), message: msg, client_error: None });
            }
        }
    }

    let speed_factor_pct = mod_state.get_attribute(20, &ctx.sde);
    if speed_factor_pct > -99.9 && speed_factor_pct < 0.0 {
        prep.hic_velocity_multiplier = Some((1.0 + speed_factor_pct / 100.0).clamp(0.01, 1.0));
    } else {
        prep.immobilize_self = true;
    }
    prep.self_disallow_assistance = true;
    prep.self_disable_mwd_mjd = true;
    if is_siege || is_triage {
        prep.self_ewar_immune = true;
        prep.self_mass_multiplier = Some(10.0);
    }
    if is_bastion {
        prep.deferred_post_cycle.bastion_weapon_timer = true;
    }
    Ok(is_phenomena)
}

#[inline(never)]
async fn collect_phenomena_debuffs(
    ctx: &ServerContext,
    sys: &crate::system_actor::SystemHandle,
    mod_state: &eve_dogma::DogmaItemState,
) -> Option<(Vec<i64>, Vec<(u32, f64)>, u64)> {
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
    let mut phen_dbuffs: Vec<(u32, f64)> = Vec::new();
    for &(id_attr, val_attr) in &[(2468u16, 2469u16), (2470, 2471), (2472, 2473), (2536, 2537)] {
        let dbuff_id = mod_state.get_attribute(id_attr, &ctx.sde).round() as u32;
        let pct = mod_state.get_attribute(val_attr, &ctx.sde);
        if dbuff_id > 0
            && pct.abs() > 1e-9
            && ctx.sde.dbuff_collections.contains_key(&dbuff_id)
        {
            phen_dbuffs.push((dbuff_id, pct));
        }
    }
    if phen_dbuffs.is_empty() {
        None
    } else {
        let duration_secs = ((mod_state.get_attribute(73, &ctx.sde) / 1000.0).ceil() as u64).max(30);
        Some((dyn_ship_ids, phen_dbuffs, duration_secs))
    }
}

#[inline(never)]
fn prepare_propulsion_and_mjd(
    sde: &eve_sde::SdeRegistry,
    ship_group_id: u32,
    ship_type_id: u32,
    mod_type: &eve_sde::InvType,
    mod_state: &eve_dogma::DogmaItemState,
    is_mwd: bool,
    is_mjd: bool,
    prep: &mut EwarCapitalPrep,
) -> Result<(), BridgeRpcError> {
    // Propulsion Modules (`group_id == 46`: Afterburner & Microwarpdrive)
    if mod_type.group_id == 46 {
        if is_mwd {
            let sig_bonus_pct = mod_state.get_attribute(554, sde);
            if sig_bonus_pct > 0.0 {
                prep.hic_sig_multiplier = Some(1.0 + sig_bonus_pct / 100.0);
            }
        }
        let mass_add = mod_state.get_attribute(attr_ids::MASS_ADDITION, sde);
        if mass_add > 0.0 {
            prep.hic_mass_addition = Some(mass_add);
        }
    }

    // Micro Jump Drive / Field Generator (`group_id == 1189 | 1533 | 4769`)
    if is_mjd {
        let mut allowed_groups = Vec::new();
        let mut allowed_types = Vec::new();
        for (aid, &val) in &mod_type.default_attributes {
            if val <= 0.0 {
                continue;
            }
            if let Some(name) = sde.attributes.get(aid).map(|a| a.name.as_str()) {
                if name.starts_with("canFitShipGroup") {
                    allowed_groups.push(val as u32);
                } else if name.starts_with("canFitShipType") {
                    allowed_types.push(val as u32);
                }
            }
        }
        if (!allowed_groups.is_empty() || !allowed_types.is_empty())
            && !allowed_groups.contains(&ship_group_id)
            && !allowed_types.contains(&ship_type_id)
        {
            return Err(BridgeRpcError {
                code: "InvalidShipGroup".into(),
                message: format!(
                    "{} cannot be activated on ship group {}",
                    mod_type.name, ship_group_id
                ),
                client_error: None,
            });
        }
        let jump_dist = [2066u16, 2064u16]
            .into_iter()
            .find_map(|aid| {
                let v = mod_state.get_attribute(aid, sde);
                (v > 0.0).then_some(v)
            })
            .unwrap_or(100_000.0);
        prep.mjd_jump_distance_m = Some(jump_dist);
        let radius_m = mod_state.get_attribute(2067, sde);
        if radius_m > 0.0 {
            let extra_cap = mod_state.get_attribute(2832, sde);
            let max_ships = if extra_cap > 0.0 {
                (extra_cap as usize).saturating_add(1)
            } else {
                25
            };
            let cap_limit = mod_state.get_attribute(5686, sde);
            let max_caps = if cap_limit > 0.0 { cap_limit as usize } else { 0 };
            prep.mjfg_field_params = Some((radius_m, max_ships, max_caps));
        }
        let mjd_sig_pct = mod_state.get_attribute(973, sde);
        if mjd_sig_pct > 0.0 {
            prep.hic_sig_multiplier = Some(1.0 + mjd_sig_pct / 100.0);
        }
    }
    Ok(())
}

#[inline(never)]
async fn prepare_warp_disruptor_scrambler(
    ctx: &ServerContext,
    sys: &crate::system_actor::SystemHandle,
    sys_id: u32,
    char_id: i64,
    ship_id: i64,
    mod_type: &eve_sde::InvType,
    mod_state: &eve_dogma::DogmaItemState,
    target_id_opt: Option<i64>,
    is_overloaded: bool,
    prep: &mut EwarCapitalPrep,
) -> Result<(), BridgeRpcError> {
    let Some(target_id) = target_id_opt else {
        return Err(BridgeRpcError {
            code: "TargetRequired".into(),
            message: "Warp disruptor/scrambler requires a locked target".into(),
            client_error: None,
        });
    };
    if target_id == ship_id {
        return Err(BridgeRpcError {
            code: "CannotActivateOnSelf".into(),
            message: "Cannot activate warp disruptor/scrambler on self".into(),
            client_error: None,
        });
    }
    let scram_eval = eve_dogma::eval_warp_scrambler_effects(&[&mod_state], &[0.0], &ctx.sde);
    prep.warp_scramble_points = scram_eval.scramble_strength.max(1);
    prep.disables_target_mwd_mjd = scram_eval.blocks_mwd;
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
    if prep.disables_target_mwd_mjd {
        max_range *= crate::services::security_mgr::insurgency_suppression_web_scram_range_multiplier(
            ctx, char_id, sys_id,
        );
    }
    if let Some(dist) = measure_ball_surface_distance_m(sys, ship_id, target_id).await {
        if dist > max_range {
            return Err(BridgeRpcError::target_out_of_range(
                crate::services::inventory_fitting::resolve_target_display_name(ctx, target_id),
                mod_type.type_id as i64,
                dist,
                format!(
                    "Target {target_id} is at {dist:.1} m, exceeding module optimal range {max_range:.1} m"
                ),
            ));
        }
    }
    Ok(())
}

#[inline(never)]
fn check_active_industrial_core_requirement(
    ctx: &ServerContext,
    ship_id: i64,
    fit: &eve_dogma::ShipFittingEngine,
) -> Result<(), BridgeRpcError> {
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
        return Err(BridgeRpcError {
            code: "IndustrialCoreNotActive".into(),
            message: "Module requires an active Industrial Core (attribute 3265 activationRequiresActiveIndustrialCore)".into(),
            client_error: None,
        });
    }
    Ok(())
}

#[inline(never)]
async fn prepare_hic_wdfg(
    ctx: &ServerContext,
    sys: &crate::system_actor::SystemHandle,
    sys_id: u32,
    ship_id: i64,
    ship_group_id: u32,
    m: &eve_dogma::FittedModule,
    mod_type: &eve_sde::InvType,
    target_id_opt: Option<i64>,
    is_overloaded: bool,
    prep: &mut EwarCapitalPrep,
) -> Result<(), BridgeRpcError> {
    if ship_group_id != 894 {
        return Err(BridgeRpcError {
            code: "InvalidShipGroup".into(),
            message: "Warp Disruption Field Generator can only be activated on Heavy Interdiction Cruisers (group 894)".into(),
            client_error: None,
        });
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
            return Err(BridgeRpcError {
                code: "TargetRequired".into(),
                message: "Scripted Warp Disruption Field Generator requires a locked target".into(),
                client_error: None,
            });
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
        if let Some(dist) = measure_ball_surface_distance_m(sys, ship_id, target_id).await {
            if dist > scripted_max_range {
                return Err(BridgeRpcError::target_out_of_range(
                    crate::services::inventory_fitting::resolve_target_display_name(
                        ctx,
                        target_id,
                    ),
                    mod_type.type_id as i64,
                    dist,
                    format!(
                        "Target {target_id} is at {dist:.1} m, exceeding scripted WDFG range {scripted_max_range:.1} m"
                    ),
                ));
            }
        }
        // Infinite point (100 warp scramble strength) + MWD/MJD suppression, no bubble
        prep.warp_scramble_points = 100;
        prep.disables_target_mwd_mjd = true;
        prep.hic_bubble_radius_m = None;
    } else {
        // Unscripted mode: AoE Warp Disruption Bubble + HIC self-penalties
        if sys_id == 30_100_000 && mod_type.default_attributes.get(&5561).copied().unwrap_or(0.0) > 0.0 {
            return Err(BridgeRpcError::user_error("ModuleDisallowedInHazardSystem"));
        }
        prep.hic_bubble_radius_m = Some(base_wdfg_range);
        prep.hic_velocity_multiplier = Some(0.40);
        let sig_bonus_pct = mod_type
            .default_attributes
            .get(&554)
            .copied()
            .unwrap_or(50.0);
        prep.hic_sig_multiplier = Some(1.0 + sig_bonus_pct / 100.0);
        let mass_add = mod_type
            .default_attributes
            .get(&796)
            .copied()
            .unwrap_or(80_000_000.0);
        prep.hic_mass_addition = Some(mass_add);
    }
    Ok(())
}

#[inline(never)]
#[allow(clippy::too_many_arguments)]
async fn prepare_offensive_ewar_and_grappler(
    ctx: &ServerContext,
    sys: &crate::system_actor::SystemHandle,
    sys_id: u32,
    char_id: i64,
    ship_id: i64,
    module_id: i64,
    m: &eve_dogma::FittedModule,
    mod_type: &eve_sde::InvType,
    mod_state: &eve_dogma::DogmaItemState,
    target_id_opt: Option<i64>,
    is_overloaded: bool,
    prep: &mut EwarCapitalPrep,
) -> Result<(), BridgeRpcError> {
    let Some(target_id) = target_id_opt else {
        return Err(BridgeRpcError {
            code: "TargetRequired".into(),
            message: "EWAR module requires a locked target".into(),
            client_error: None,
        });
    };
    if target_id == ship_id {
        return Err(BridgeRpcError {
            code: "CannotActivateOnSelf".into(),
            message: "Cannot activate offensive EWAR module on self".into(),
            client_error: None,
        });
    }
    let distance_m = measure_ball_surface_distance_m(sys, ship_id, target_id)
        .await
        .unwrap_or(0.0);
    let mut proj_mod_state = mod_state.clone();
    let mut proj_source_tid = m.type_id;
    let mut proj_dist = distance_m;
    if mod_type.group_id == 65 || mod_type.group_id == 1672 {
        let mut max_range = mod_state.get_attribute(attr_ids::MAX_RANGE, &ctx.sde);
        if max_range <= 0.0 {
            max_range = mod_type.default_attributes.get(&54).copied().unwrap_or(10_000.0);
        }
        if mod_type.group_id == 65 {
            let web_range_mult =
                crate::services::security_mgr::insurgency_suppression_web_scram_range_multiplier(
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
            return Err(BridgeRpcError::target_out_of_range(
                crate::services::inventory_fitting::resolve_target_display_name(ctx, target_id),
                mod_type.type_id as i64,
                distance_m,
                format!(
                    "Target {target_id} is at {distance_m:.1} m, exceeding webifier range {max_range:.1} m"
                ),
            ));
        }
        let att = eve_dogma::compute_falloff_attenuation(distance_m, max_range, falloff);
        let raw_sf = mod_state.get_attribute(20, &ctx.sde);
        let eff_sf = if mod_type.group_id == 1672 {
            // Heavy Stasis Grappler (`feat:modules.stasis-grappler-mechanics`):
            // Full speedFactor within optimal (1 km), decaying across falloffEffectiveness (8-15 km) to 0%
            if distance_m <= max_range + 3.0 * falloff.max(1.0) {
                raw_sf * att
            } else {
                0.0
            }
        } else {
            raw_sf * att
        };
        prep.target_velocity_multiplier = Some((1.0 + eff_sf / 100.0).clamp(0.05, 1.0));
        if mod_type.group_id == 1672 {
            prep.grappler_optimal_m = max_range;
            prep.grappler_falloff_m = falloff;
            prep.grappler_speed_factor_pct = raw_sf;
            proj_mod_state.base_attributes.insert(20, eff_sf);
            proj_mod_state.base_attributes.insert(54, 1_000_000_000.0);
            proj_mod_state.modifiers.retain(|mm| mm.target_attribute_id != 20);
            proj_source_tid = 526;
            proj_dist = 0.0;
        }
    }
    prep.deferred_post_cycle.projected_ewar = Some((
        target_id,
        eve_dogma::ProjectedEwarEffect {
            source_module_id: module_id,
            source_module_type_id: proj_source_tid,
            charge_type_id: m.charge_type_id,
            distance: proj_dist,
            overloaded: is_overloaded,
            effective_module: proj_mod_state,
        },
    ));
    Ok(())
}

#[inline(never)]
#[allow(clippy::too_many_arguments)]
async fn prepare_remote_assistance(
    ctx: &ServerContext,
    sys: &crate::system_actor::SystemHandle,
    ship_id: i64,
    module_id: i64,
    m: &eve_dogma::FittedModule,
    mod_type: &eve_sde::InvType,
    mod_state: &eve_dogma::DogmaItemState,
    fit: &eve_dogma::ShipFittingEngine,
    target_id_opt: Option<i64>,
    is_overloaded: bool,
    prep: &mut EwarCapitalPrep,
) -> Result<(), BridgeRpcError> {
    let Some(target_id) = target_id_opt else {
        return Err(BridgeRpcError {
            code: "TargetRequired".into(),
            message: "Remote assistance module requires a locked target".into(),
            client_error: None,
        });
    };
    if target_id == ship_id {
        return Err(BridgeRpcError {
            code: "CannotActivateOnSelf".into(),
            message: "Cannot activate remote assistance module on self".into(),
            client_error: None,
        });
    }
    if let Ok(Some(t_cs)) = sys.get_combat_state(target_id).await {
        if t_cs.disallow_assistance {
            return Err(BridgeRpcError {
                code: "TargetDisallowsAssistance".into(),
                message: format!("Target {target_id} has disallowAssistance active (Siege/Triage/HIC bubble)"),
                client_error: None,
            });
        }
    }
    let mut max_range = mod_state.get_attribute(attr_ids::MAX_RANGE, &ctx.sde);
    if max_range <= 0.0 {
        max_range = mod_type.default_attributes.get(&54).copied().unwrap_or(20_000.0);
    }
    let falloff = eve_dogma::get_module_falloff(&mod_state, &ctx.sde);
    prep.remote_optimal = max_range;
    prep.remote_falloff = falloff;
    if let Some(dist) = measure_ball_surface_distance_m(sys, ship_id, target_id).await {
        if falloff <= 0.0 && dist > max_range && max_range > 0.0 {
            return Err(BridgeRpcError::target_out_of_range(
                crate::services::inventory_fitting::resolve_target_display_name(ctx, target_id),
                mod_type.type_id as i64,
                dist,
                format!(
                    "Target {target_id} is at {dist:.1} m, exceeding remote assistance range {max_range:.1} m"
                ),
            ));
        }
    }
    let has_cat5_overload = mod_type
        .effect_ids
        .iter()
        .any(|&eid| ctx.sde.effects.get(&eid).is_some_and(|e| e.effect_category == 5));
    let ov_m = if is_overloaded && !has_cat5_overload { 1.20 } else { 1.0 };
    let rr_imp_m = crate::services::tactical_modules::incoming_remote_repair_multiplier(ctx, target_id);
    match mod_type.group_id {
        41 | 1697 => prep.remote_shield_amount = mod_state.get_attribute(68, &ctx.sde) * ov_m * rr_imp_m,
        325 | 2018 | 1698 => {
            if mod_type.group_id == 2018 {
                let base_state = if m.dynamic_attribute_overrides.contains_key(&84) {
                    let mut clean_fit = fit.clone();
                    if let Some(fm) = clean_fit.modules.get_mut(&module_id) {
                        fm.dynamic_attribute_overrides.remove(&84);
                    }
                    clean_fit.build_effective_module_state(module_id, m.type_id, is_overloaded, &ctx.sde)
                } else {
                    mod_state.clone()
                };
                prep.remote_armor_amount = base_state.get_attribute(84, &ctx.sde) * ov_m * rr_imp_m;
                prep.mutadaptive_spool_step = mod_type.default_attributes.get(&2796).copied().unwrap_or(0.10);
                prep.mutadaptive_spool_max = mod_type.default_attributes.get(&2797).copied().unwrap_or(1.50);
            } else {
                prep.remote_armor_amount = mod_state.get_attribute(84, &ctx.sde) * ov_m * rr_imp_m;
            }
        }
        585 => prep.remote_hull_amount = mod_state.get_attribute(83, &ctx.sde) * ov_m * rr_imp_m,
        67 => prep.remote_cap_amount = mod_state.get_attribute(90, &ctx.sde) * ov_m,
        _ => {}
    }
    Ok(())
}

#[inline(never)]
async fn prepare_burst_projector(
    ctx: &ServerContext,
    sys: &crate::system_actor::SystemHandle,
    ship_id: i64,
    m: &eve_dogma::FittedModule,
    mod_state: &eve_dogma::DogmaItemState,
    target_id_opt: Option<i64>,
    prep: &mut EwarCapitalPrep,
) {
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
    if m.type_id == 40698 {
        let sf = mod_state.get_attribute(20, &ctx.sde).min(-30.0);
        prep.target_velocity_multiplier = Some((1.0 + sf / 100.0).clamp(0.05, 1.0));
    }
    prep.deferred_post_cycle.burst_projector = Some((burst_center, burst_radius, victims));
}

#[inline(never)]
#[allow(clippy::too_many_arguments)]
async fn prepare_ewar_and_capital_module_inner(
    ctx: &ServerContext,
    sys: &crate::system_actor::SystemHandle,
    sys_id: u32,
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
) -> Result<EwarCapitalPrep, BridgeRpcError> {
    if is_mwd {
        crate::router::box_fut(|| check_mwd_deadspace_allowed(ctx, sys, sys_id, ship_id)).await?;
    }
    let pending_weapon_autoload =
        resolve_pending_weapon_autoload(ctx, char_id, ship_id, module_id, m, mod_type, mod_state)?;

    let mut prep = EwarCapitalPrep {
        grants_interdiction_nullification: mod_type.group_id == 4117,
        deferred_post_cycle: DeferredPostCycleEffects {
            weapon_autoload: pending_weapon_autoload,
            ..Default::default()
        },
        ..Default::default()
    };

    if prep.grants_interdiction_nullification {
        crate::router::box_fut(|| {
            validate_interdiction_nullifier(sys, ship_id, ship_group_id, fit.ship.type_id, mod_type)
        })
        .await?;
    }

    prep.is_adc = validate_adc_cehe_triage_restrictions(ship_group_id, m, mod_type, fit)?;

    if matches!(mod_type.group_id, 290 | 209) {
        let dist_m = crate::router::box_fut(|| {
            prepare_projected_assist_range(ctx, sys, char_id, ship_id, mod_type, mod_state, target_id_opt)
        })
        .await?;
        prep.deferred_post_cycle.projected_assist_dist_m = Some(dist_m);
    }

    if mod_type.group_id == 2008 {
        prepare_zero_point_mass_entangler(
            ship_group_id,
            fit.ship.type_id,
            mod_state,
            &ctx.sde,
            &mut prep,
        )?;
    }

    // Siege (`20280`/`4292`), Triage (`27951`/`4294`), Bastion (`33400`), Industrial Core (`28583`/`58945`), Phenomena Generator (`group 1815`)
    if mod_type.group_id == 515 || mod_type.group_id == 1815 {
        let is_phenomena = prepare_tactical_mode_sync(
            ctx,
            char_id,
            ship_id,
            ship_group_id,
            m,
            mod_type,
            mod_state,
            &mut prep,
        )?;
        if is_phenomena {
            prep.deferred_post_cycle.phenomena_debuffs =
                crate::router::box_fut(|| collect_phenomena_debuffs(ctx, sys, mod_state)).await;
        }
    }

    if mod_type.group_id == 46 || is_mjd {
        prepare_propulsion_and_mjd(
            &ctx.sde,
            ship_group_id,
            fit.ship.type_id,
            mod_type,
            mod_state,
            is_mwd,
            is_mjd,
            &mut prep,
        )?;
    }

    // Warp Disruptor / Warp Scrambler (`group_id == 52`)
    if mod_type.group_id == 52 {
        crate::router::box_fut(|| {
            prepare_warp_disruptor_scrambler(
                ctx,
                sys,
                sys_id,
                char_id,
                ship_id,
                mod_type,
                mod_state,
                target_id_opt,
                is_overloaded,
                &mut prep,
            )
        })
        .await?;
    }

    // SDE attribute `3265` (`activationRequiresActiveIndustrialCore`, e.g. Shipboard Compressors `group_id == 4174`):
    // require an active Industrial Core (`group_id == 515`) fitted on the ship (`docs/eve-wiki/Compression.md:39`).
    if mod_type.default_attributes.get(&3265).copied().unwrap_or(0.0) > 0.0 {
        check_active_industrial_core_requirement(ctx, ship_id, fit)?;
    }

    // Heavy Interdictor Warp Disruption Field Generator (`group_id == 899`, e.g., `28654` T1, `4248` T2)
    if mod_type.group_id == 899 {
        crate::router::box_fut(|| {
            prepare_hic_wdfg(
                ctx,
                sys,
                sys_id,
                ship_id,
                ship_group_id,
                m,
                mod_type,
                target_id_opt,
                is_overloaded,
                &mut prep,
            )
        })
        .await?;
    }

    // Warp Core Stabilizer (`group_id == 315`, e.g., `Warp Core Stabilizer I` `10998`)
    if mod_type.group_id == 315 {
        let raw_wcs = mod_type
            .default_attributes
            .get(&105)
            .copied()
            .unwrap_or(-2.0);
        prep.warp_core_stab_bonus = (-raw_wcs).round().max(1.0) as i32;
    }

    // Weapon Disruptor (`group_id == 291`: Tracking Disruptor 2108/2109 & Guidance Disruptor 37543/37546),
    // Remote Sensor Dampener (`group_id == 208`), Target Painter (`group_id == 379`),
    // Stasis Webifier (`group_id == 65`), Heavy Stasis Grappler (`group_id == 1672`)
    if matches!(mod_type.group_id, 291 | 208 | 379 | 65 | 1672) {
        crate::router::box_fut(|| {
            prepare_offensive_ewar_and_grappler(
                ctx,
                sys,
                sys_id,
                char_id,
                ship_id,
                module_id,
                m,
                mod_type,
                mod_state,
                target_id_opt,
                is_overloaded,
                &mut prep,
            )
        })
        .await?;
    }

    // Remote Shield Booster (`41`), Remote Armor Repairer (`325`), Mutadaptive Remote Armor Repairer (`2018`),
    // Remote Hull Repairer (`585`), Remote Capacitor Transmitter (`67`) (`feat:modules.remote-*`)
    if matches!(mod_type.group_id, 41 | 325 | 2018 | 585 | 67 | 1697 | 1698) {
        crate::router::box_fut(|| {
            prepare_remote_assistance(
                ctx,
                sys,
                ship_id,
                module_id,
                m,
                mod_type,
                mod_state,
                fit,
                target_id_opt,
                is_overloaded,
                &mut prep,
            )
        })
        .await?;
    }

    // Supercarrier Burst Projectors (`group_id == 842`, `feat:modules.burst-projectors`)
    if mod_type.group_id == 842 {
        crate::router::box_fut(|| {
            prepare_burst_projector(ctx, sys, ship_id, m, mod_state, target_id_opt, &mut prep)
        })
        .await;
    }

    Ok(prep)
}

#[inline(never)]
#[allow(clippy::too_many_arguments)]
pub(crate) async fn prepare_ewar_and_capital_module(
    ctx: &ServerContext,
    sys: &crate::system_actor::SystemHandle,
    sys_id: u32,
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
    Some(
        crate::router::box_fut(|| {
            prepare_ewar_and_capital_module_inner(
                ctx,
                sys,
                sys_id,
                char_id,
                ship_id,
                ship_group_id,
                module_id,
                m,
                mod_type,
                mod_state,
                fit,
                target_id_opt,
                is_overloaded,
                is_mwd,
                is_mjd,
            )
        })
        .await,
    )
}

#[inline(never)]
pub(crate) async fn validate_passive_targeter_activation(
    ctx: &ServerContext,
    sys: &crate::system_actor::SystemHandle,
    char_id: i64,
    ship_id: i64,
    target_id: i64,
    fit: &eve_dogma::ShipFittingEngine,
) -> Result<eve_destiny::TargetLock, BridgeRpcError> {
    if target_id == ship_id {
        return Err(BridgeRpcError {
            code: "CannotActivateOnSelf".into(),
            message: "Cannot activate Passive Targeter on self".into(),
            client_error: None,
        });
    }
    if let Ok(Some(cs)) = sys.get_combat_state(ship_id).await {
        if cs.ecm_jammed_remaining_s > 0.0 && !cs.ecm_jammed_by.contains(&target_id) {
            return Err(super::fitting_dogma_im::ship_jammed_rpc_error(ctx, target_id));
        }
        if (cs.is_cloaked && cs.gate_cloak_remaining_ticks == 0)
            || cs.decloak_targeting_delay_remaining_s > 0.0
        {
            let key = if cs.is_cloaked && cs.gate_cloak_remaining_ticks == 0 {
                eve_proto::UserErrorKey::CantTargetWhileCloaked
            } else {
                eve_proto::UserErrorKey::DeniedTargetAfterCloak
            };
            return Err(BridgeRpcError::targeting_failed(
                Some(ship_id),
                Some(target_id),
                key,
                "cloak_or_decloak_delay",
                "Cannot acquire target lock while cloaked or during decloak delay",
            ));
        }
    }
    let Some((_, s_ball)) = sys.get_ball_snapshot(ship_id).await.ok().flatten() else {
        return Err(BridgeRpcError::targeting_failed(
            Some(ship_id),
            Some(target_id),
            eve_proto::UserErrorKey::CantShipNotPresent,
            "ship_not_in_space",
            "Ship is not present in space ballpark",
        ));
    };
    let Some((_, t_ball)) = sys.get_ball_snapshot(target_id).await.ok().flatten() else {
        return Err(BridgeRpcError::targeting_failed(
            Some(ship_id),
            Some(target_id),
            eve_proto::UserErrorKey::TargetCantValidate,
            "target_not_present",
            format!("Target {target_id} not found in space"),
        ));
    };
    let t_cs = sys.get_combat_state(target_id).await.ok().flatten().map(|mut cs| {
        cs.sync_action_breaks();
        cs
    });
    if let Some(ref cs) = t_cs {
        if cs.blocks_targeted_effect() {
            if cs.is_protected() {
                return Err(BridgeRpcError {
                    code: "TargetInvulnerable".into(),
                    message: format!("Target {target_id} is tethered or invulnerable"),
                    client_error: None,
                });
            }
            return Err(BridgeRpcError::targeting_failed(
                Some(ship_id),
                Some(target_id),
                eve_proto::UserErrorKey::DeniedTargetingTargetCloaked,
                "target_cloaked",
                format!("Target {target_id} is cloaked"),
            ));
        }
    }
    let max_range = fit
        .ship
        .get_attribute(attr_ids::MAX_TARGET_RANGE, &ctx.sde)
        .max(0.0);
    let surface_offset = s_ball.radius + t_ball.radius;
    let dist = ((s_ball.position - t_ball.position).length() - surface_offset).max(0.0);
    if dist > max_range {
        return Err(BridgeRpcError::target_out_of_range(
            crate::services::inventory_fitting::resolve_target_display_name(ctx, target_id),
            fit.ship.type_id as i64,
            dist,
            format!(
                "Target {target_id} at {dist:.0}m exceeds max targeting range {max_range:.0}m"
            ),
        ));
    }
    let ship_max = fit
        .ship
        .get_attribute(attr_ids::MAX_LOCKED_TARGETS, &ctx.sde)
        .max(1.0) as usize;
    let skills = ctx.character_skill_levels(char_id);
    let tm_lvl = skills.get(&3429).copied().unwrap_or(0);
    let atm_lvl = skills.get(&3430).copied().unwrap_or(0);
    let max_locked = eve_dogma::compute_effective_max_locked_targets(ship_max, tm_lvl, atm_lvl);
    let current_locks = sys.get_target_locks(ship_id).await.unwrap_or_default();
    if !current_locks.iter().any(|l| l.target_id == target_id) && current_locks.len() >= max_locked {
        return Err(BridgeRpcError {
            code: "TooManyLockedTargets".into(),
            message: format!("Max locked targets exceeded ({max_locked})"),
            client_error: None,
        });
    }
    let scan_res = fit
        .ship
        .get_attribute(attr_ids::SCAN_RESOLUTION, &ctx.sde)
        .max(1.0);
    let raw_sig = t_cs
        .map(|cs| cs.base_signature_radius.unwrap_or(cs.signature_radius))
        .filter(|&s| s > 0.0)
        .unwrap_or(0.0)
        .max(t_ball.radius)
        .max(35.0);
    let target_sig = ctx.effective_target_signature_radius(target_id, raw_sig);
    let lock_sec = eve_destiny::combat::compute_lock_time_seconds(scan_res, target_sig).max(0.5);
    let lock_time_ms = lock_sec * 1000.0;
    Ok(eve_destiny::TargetLock {
        target_id,
        lock_time_ms,
        remaining_lock_ms: lock_time_ms,
        locked: false,
        max_range_m: Some(max_range + surface_offset),
    })
}

#[inline(never)]
#[allow(clippy::too_many_arguments)]
pub(crate) async fn commit_post_cycle_activation_effects(
    ctx: &ServerContext,
    sys: &crate::system_actor::SystemHandle,
    char_id: i64,
    ship_id: i64,
    module_id: i64,
    m: &eve_dogma::FittedModule,
    mod_type: &eve_sde::InvType,
    mod_state: &eve_dogma::DogmaItemState,
    target_id_opt: Option<i64>,
    deferred: DeferredPostCycleEffects,
) -> Result<(), BridgeRpcError> {
    if deferred.weapon_autoload.is_some()
        || deferred.fuel_to_deduct.is_some()
        || deferred.cap_booster_charge_id.is_some()
        || deferred.ancillary_charge_to_consume.is_some()
        || !deferred.turret_charge_consumptions.is_empty()
    {
        let (committed_autoload, item_diffs) = ctx.db.try_with_mut_snapshot(
            |snap| -> Result<_, BridgeRpcError> {
                let mut diffs: Vec<(
                    eve_db::InventoryItemRecord,
                    Option<eve_db::InventoryItemRecord>,
                )> = Vec::new();
                let mut committed_autoload_info = None;
                if let Some(plan) = deferred.weapon_autoload.as_ref() {
                    let committed = commit_pending_weapon_autoload_in_snapshot(
                        ctx, snap, char_id, ship_id, mod_type, plan,
                    )?;
                    diffs.extend(committed.cycle_consume_diffs);
                    committed_autoload_info =
                        Some((committed.autoload_diffs, committed.loaded_bank));
                }
                if let Some((fuel_type_id, fuel_qty_needed)) = deferred.fuel_to_deduct {
                    if fuel_qty_needed > 0 {
                        let stacks = collect_ship_fuel_stacks(snap, ship_id, fuel_type_id);
                        let total_avail: i64 = stacks.iter().map(|&(_, _, q)| i64::from(q)).sum();
                        if total_avail < i64::from(fuel_qty_needed) {
                            return Err(BridgeRpcError {
                                code: "InsufficientFuel".into(),
                                message: format!(
                                    "Insufficient fuel (type {fuel_type_id}): needed {fuel_qty_needed}, available {total_avail}"
                                ),
                                client_error: None,
                            });
                        }
                        let mut remaining = fuel_qty_needed;
                        for (_, fid, _) in stacks {
                            if remaining <= 0 {
                                break;
                            }
                            let Some(pre) = snap.items.get(&fid).cloned() else {
                                continue;
                            };
                            let take = remaining.min(pre.quantity);
                            if take <= 0 {
                                continue;
                            }
                            let opts = eve_db::ConsumeExactOptions::for_owner_exact(
                                None,
                                pre.owner_id,
                                eve_db::ItemClassPolicy::StackableCommodity,
                            );
                            if let Ok(post) = snap.consume_exact_item_quantity(fid, take, opts) {
                                remaining -= take;
                                diffs.push((pre, post));
                            }
                        }
                        if remaining > 0 {
                            return Err(BridgeRpcError {
                                code: "InsufficientFuel".into(),
                                message: format!(
                                    "Insufficient fuel (type {fuel_type_id}): needed {fuel_qty_needed}, remaining {remaining}"
                                ),
                                client_error: None,
                            });
                        }
                    }
                }
                if let Some(cid) = deferred.cap_booster_charge_id {
                    let (pre, post) = consume_cycle_assembled_charge(
                        snap,
                        char_id,
                        ship_id,
                        mod_type.type_id,
                        cid,
                        1,
                    )?;
                    diffs.push((pre, post));
                }
                if let Some((cid, qty)) = deferred.ancillary_charge_to_consume {
                    let (pre, post) = consume_cycle_assembled_charge(
                        snap,
                        char_id,
                        ship_id,
                        mod_type.type_id,
                        cid,
                        qty,
                    )?;
                    diffs.push((pre, post));
                }
                for (turret_id, charge_id) in deferred.turret_charge_consumptions {
                    let (pre, post) = consume_cycle_assembled_charge(
                        snap,
                        char_id,
                        ship_id,
                        mod_type.type_id,
                        charge_id,
                        1,
                    )?;
                    if post.is_none() {
                        snap.module_active_charges.remove(&turret_id);
                    }
                    diffs.push((pre, post));
                }
                Ok((committed_autoload_info, diffs))
            },
        )?;
        if let Some((autoload_diffs, loaded_bank)) = committed_autoload {
            emit_committed_weapon_autoload_notifications(
                ctx,
                char_id,
                ship_id,
                &autoload_diffs,
                &loaded_bank,
            );
        }
        for (pre, post) in &item_diffs {
            if matches!(pre.flag as u16, 11..=34 | 125..=132) {
                crate::services::inventory_fitting::fitting_module_cycles::emit_charge_sublocation_quantity_change(
                    ctx,
                    char_id,
                    pre.location_id,
                    pre.flag,
                    pre.type_id,
                    pre.quantity,
                    post.as_ref().map_or(0, |p| p.quantity),
                );
            }
        }
    }

    if deferred.bastion_weapon_timer {
        let now_unix = ctx.now_unix_secs();
        let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
        let st = cw.get_or_create(char_id);
        st.weapon_timer_until_unix = st.weapon_timer_until_unix.max(now_unix + 60);
    }

    if let Some((target_id, effect)) = deferred.projected_ewar {
        ctx.upsert_projected_ewar_on_ship(ship_id, target_id, effect);
        if mod_type.group_id == 379 {
            let tp_mult = ctx.effective_target_painter_multiplier(target_id);
            sys.try_recompute_combat_attributes(target_id, |t_cs| {
                t_cs.tp_sig_multiplier = (tp_mult > 1.0 + 1e-6).then_some(tp_mult);
            });
        }
    }

    if let Some(burst_plan) = deferred.burst_jammer_plan {
        crate::router::box_fut(|| {
            super::fitting_module_cycles::apply_burst_jammer_plan(
                ctx,
                sys,
                char_id,
                ship_id,
                module_id,
                mod_state,
                burst_plan,
            )
        })
        .await;
    }

    if let Some(dist_m) = deferred.projected_assist_dist_m {
        crate::services::tactical_modules::apply_projected_remote_assistance(
            ctx,
            char_id,
            ship_id,
            module_id,
            mod_type,
            mod_state,
            target_id_opt,
            dist_m,
        )?;
    }

    if deferred.phenomena_debuffs.is_some()
        || deferred.burst_projector.is_some()
        || deferred.passive_targeter_lock.is_some()
    {
        crate::router::box_fut(|| {
            apply_deferred_actor_effects(
                ctx,
                sys,
                char_id,
                ship_id,
                module_id,
                m.type_id,
                mod_state,
                deferred.phenomena_debuffs,
                deferred.burst_projector,
                deferred.passive_targeter_lock,
            )
        })
        .await;
    }

    Ok(())
}

#[inline(never)]
#[allow(clippy::too_many_arguments)]
async fn apply_deferred_actor_effects(
    ctx: &ServerContext,
    sys: &crate::system_actor::SystemHandle,
    char_id: i64,
    ship_id: i64,
    module_id: i64,
    module_type_id: u32,
    mod_state: &eve_dogma::DogmaItemState,
    phenomena_debuffs: Option<(Vec<i64>, Vec<(u32, f64)>, u64)>,
    burst_projector: Option<(DVec3, f64, Vec<i64>)>,
    passive_targeter_lock: Option<eve_destiny::TargetLock>,
) {
    if let Some((dyn_ship_ids, phen_dbuffs, duration_secs)) = phenomena_debuffs {
        let now_unix = ctx.now_unix_secs();
        let expires_at_unix = now_unix.saturating_add(duration_secs);
        ctx.scanners_superweapons.with_ship_dbuffs_mut(|dbuffs| {
            for &sid in &dyn_ship_ids {
                let entries = dbuffs.entry(sid).or_default();
                entries.retain(|e| e.expires_at_unix > now_unix && e.source_module_id != module_id);
                for &(dbuff_id, pct) in &phen_dbuffs {
                    entries.push(super::fitting_scanners_superweapons::ActiveDbuffEntry {
                        dbuff_id,
                        value: pct,
                        expires_at_unix,
                        source_module_id: module_id,
                    });
                }
            }
        });
        super::fitting_scanners_superweapons::sync_ship_dbuffs_and_destiny(
            ctx,
            sys,
            &dyn_ship_ids,
            now_unix,
        )
        .await;
    }

    if let Some((burst_center, burst_radius, victims)) = burst_projector {
        match module_type_id {
            27678 => {
                for &vid in &victims {
                    let _ = sys.apply_ecm_jam_from(ship_id, vid, 20.0).await;
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
                let synth_tid = match module_type_id {
                    40696 => 12709,
                    40698 => 526,
                    40699 => 1968,
                    _ => 2108,
                };
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

    if let Some(lock) = passive_targeter_lock {
        let tid = lock.target_id;
        let _ = sys.add_target_lock(ship_id, lock).await;
        if let Some(scene) =
            crate::router::notify_registry::BallparkScene::try_solar_system(sys.solar_system_id)
        {
            ctx.emit_typed(
                crate::router::NotificationTarget::Character(char_id),
                crate::router::notify_registry::OnTarget {
                    scene,
                    arg0: PyRep::Integer(tid),
                    arg1: PyRep::String("locking".into()),
                    reason: None,
                },
            );
        }
    }
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
    // Phenomena Generators (`group_id == 1815`) render via Destiny `OnDbuffUpdated` (`Slot_TitanBurst`)
    // rather than `OnSystemWideEffectStart`, since their module types are absent from V24 `infoBubbles.static`.
    let _ = (sys_id, module_type_id, mod_type.group_id);

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

    let ewar_notif = crate::router::notify_registry::OnEwarStart {
        source_ball_id: ship_id,
        module_id,
        target_ball_id: target_id,
        jamming_type: jam_type.into(),
    };
    ctx.active_ewar_starts
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .entry((ship_id, module_id))
        .or_default()
        .push((target_id, jam_type));
    ctx.emit_typed(
        crate::router::NotificationTarget::Character(char_id),
        ewar_notif.clone(),
    );
    let target_owner = ctx
        .db
        .with_snapshot(|s| s.items.get(&target_id).map(|i| i.owner_id))
        .unwrap_or(0);
    if target_owner > 0 && target_owner != char_id {
        ctx.emit_typed(
            crate::router::NotificationTarget::Character(target_owner),
            ewar_notif,
        );
    }
}

#[cfg(test)]
#[path = "fitting_ewar_capital_tests.rs"]
mod fitting_ewar_capital_tests;


