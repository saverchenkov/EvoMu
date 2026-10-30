//! `character_skill_ops` — Automated server-side obsolete skill migration (`feat:skills.skill-remove-obsolete`).

#![forbid(unsafe_code)]

use crate::router::{NotificationTarget, ServerContext};
use eve_db::SkillHistoryEntry;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;

fn is_obsolete_skill(sde: &eve_sde::SdeRegistry, sk_id: u32) -> bool {
    match sde.types.get(&sk_id) {
        Some(t) => !t.published || t.category_id != 16,
        None => matches!(sk_id, 3374..=3379 | 12376 | 12383 | 12385..=12387),
    }
}

/// Automated server-side obsolete skill migration (`feat:skills.skill-remove-obsolete`).
///
/// Detects deprecated or unpublished skill types in the character's trained skills,
/// removes them, and refunds 100% of trained skill points to `free_skill_points`.
/// Emits `OnServerSkillsRemoved` and `OnFreeSkillPointsChanged` notifications and logs history event 38.
pub fn migrate_obsolete_skills_for_character(
    ctx: &ServerContext,
    char_id: i64,
) -> (usize, f64) {
    // Guard against incomplete SDE catalogs so missing entries never wipe valid character skills.
    let has_core_skills = ctx
        .sde
        .types
        .get(&3300)
        .is_some_and(|t| t.published && t.category_id == 16)
        && ctx
            .sde
            .types
            .get(&3327)
            .is_some_and(|t| t.published && t.category_id == 16);
    if !has_core_skills {
        return (0, 0.0);
    }

    let now_ft = ctx.now_blueos_filetime_ticks();

    let res: Result<_, BridgeRpcError> = ctx.db.try_with_mut_snapshot(|snap| {
        let Some(skills_map) = snap.skills.get_mut(&char_id) else {
            return Ok((Vec::new(), 0.0, 0.0));
        };

        let obsolete_candidates: Vec<u32> = skills_map
            .keys()
            .copied()
            .filter(|&sk_id| is_obsolete_skill(&ctx.sde, sk_id))
            .collect();

        if obsolete_candidates.is_empty() {
            return Ok((Vec::new(), 0.0, 0.0));
        }

        let mut to_refund = Vec::new();
        for sk_id in obsolete_candidates {
            if let Some(rec) = skills_map.remove(&sk_id) {
                to_refund.push((sk_id, rec.skill_points));
            }
        }

        let total_sp: f64 = to_refund.iter().map(|(_, sp)| *sp).sum();
        let ch = snap.characters.get_mut(&char_id).ok_or_else(|| BridgeRpcError {
            code: "CharacterNotFound".into(),
            message: format!("Character {char_id} not found"),
        })?;

        ch.skill_points = (ch.skill_points - total_sp).max(0.0);
        ch.free_skill_points += total_sp;

        // Record skill removal history entry (event 38)
        let hist = snap.skill_history.entry(char_id).or_default();
        for &(sk_id, _) in &to_refund {
            hist.push(SkillHistoryEntry {
                log_date_ft: now_ft,
                event_type_id: 38,
                skill_type_id: sk_id,
                skill_level: 0,
                absolute_points: 0.0,
            });
        }

        Ok((to_refund, total_sp, ch.free_skill_points))
    });

    match res {
        Ok((to_refund, total_sp, new_free_sp)) if !to_refund.is_empty() => {
            let removed_pairs: Vec<(u32, u8)> = to_refund.iter().map(|&(sk, _)| (sk, 0u8)).collect();
            super::jump_clone_names::emit_server_skills_removed(ctx, char_id, &removed_pairs);
            ctx.notify(
                NotificationTarget::Character(char_id),
                "OnFreeSkillPointsChanged",
                "charid",
                vec![PyRep::Real(new_free_sp)],
            );
            (to_refund.len(), total_sp)
        }
        _ => (0, 0.0),
    }
}

/// Dispatches skill ops, weekly Omega SP claim, and strip fitting RPCs.
pub fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        // parity: feat:skills.direct-purchase-inject
        ("skillMgr2", "PurchaseSkills") => Some(purchase_skills_direct(ctx, session, args, kwargs)),

        // parity: feat:skills.omega-weekly-free-sp
        ("storeManager", "can_purchase_offer") | ("storeManager", "buy_offer") => {
            let offer_id = args
                .first()
                .and_then(|a| a.as_i64())
                .or_else(|| kwargs.get("offer_id").or_else(|| kwargs.get("offerID")).and_then(|v| v.as_i64()))
                .unwrap_or(0);
            if offer_id == 1005 {
                Some(handle_omega_weekly_free_sp(ctx, session, method == "buy_offer"))
            } else {
                None
            }
        }

        // parity: feat:fitting.strip-fitting
        ("dogmaIM", "StripFitting") | ("ship", "StripFitting") | ("invbroker", "StripFitting") => {
            Some(strip_ship_fitting(ctx, session, args, kwargs))
        }

        _ => None,
    }
}

pub fn purchase_skills_direct_for_char(
    ctx: &ServerContext,
    char_id: i64,
    skill_ids: &[u32],
) -> Result<(Vec<u32>, i64, i64), BridgeRpcError> {
    if skill_ids.is_empty() {
        return Err(BridgeRpcError {
            code: "InvalidArgument".into(),
            message: "PurchaseSkills requires at least one skill typeID".into(),
        });
    }
    let now_unix = ctx.now_unix();
    let now_ft = ctx.now_blueos_filetime_ticks();

    let (purchased, total_cost_cents, new_bal_cents) = ctx.db.try_with_mut_snapshot(|snap| {
        let mut total_cost: i64 = 0;
        let mut costs = Vec::new();
        for &sk_id in skill_ids {
            let Some(st) = ctx.sde.types.get(&sk_id) else {
                return Err(BridgeRpcError {
                    code: "InvalidSkillType".into(),
                    message: format!("Type {sk_id} does not exist"),
                });
            };
            if st.category_id != 16 || !st.published {
                return Err(BridgeRpcError {
                    code: "InvalidSkillType".into(),
                    message: format!("Type {sk_id} ({}) is not a published skill", st.name),
                });
            }
            if snap
                .skills
                .get(&char_id)
                .is_some_and(|m| m.contains_key(&sk_id))
            {
                return Err(BridgeRpcError {
                    code: "SkillAlreadyTrained".into(),
                    message: format!("Skill {sk_id} is already injected"),
                });
            }
            if st.base_price <= 0.0 {
                return Err(BridgeRpcError {
                    code: "SkillUnavailableForPurchase".into(),
                    message: format!("Skill {sk_id} ({}) is drop-only and cannot be directly purchased", st.name),
                });
            }
            let npc_price_cents = snap
                .market_orders
                .values()
                .filter(|o| !o.is_bid && o.is_npc && o.vol_remaining > 0 && o.type_id == sk_id)
                .map(|o| o.price.0)
                .min()
                .ok_or_else(|| BridgeRpcError {
                    code: "SkillUnavailableForPurchase".into(),
                    message: format!("Skill {sk_id} is not seeded on the NPC market for direct purchase"),
                })?;
            let markup_cents = ((npc_price_cents as f64) * 1.30).round() as i64;
            total_cost = total_cost.saturating_add(markup_cents);
            costs.push((sk_id, markup_cents));
        }

        let ch = snap.characters.get_mut(&char_id).ok_or_else(|| BridgeRpcError {
            code: "CharacterNotFound".into(),
            message: format!("Character {char_id} not found"),
        })?;
        if ch.balance.0 < total_cost {
            return Err(BridgeRpcError {
                code: "NotEnoughISK".into(),
                message: format!(
                    "Insufficient ISK for direct skill purchase: need {:.2} ISK, have {:.2} ISK",
                    (total_cost as f64) / 100.0,
                    ch.balance.to_f64_isk()
                ),
            });
        }
        ch.balance = eve_db::IskCents(ch.balance.0 - total_cost);
        ch.version = ch.version.wrapping_add(1);
        let new_bal = ch.balance.0;

        let skills_map = snap.skills.entry(char_id).or_default();
        let hist = snap.skill_history.entry(char_id).or_default();
        let mut bought = Vec::new();
        for (sk_id, cost_c) in costs {
            skills_map.insert(
                sk_id,
                eve_db::TrainedSkillRecord {
                    type_id: sk_id,
                    level: 0,
                    skill_points: 0.0,
                },
            );
            hist.push(SkillHistoryEntry {
                log_date_ft: now_ft,
                event_type_id: 36,
                skill_type_id: sk_id,
                skill_level: 0,
                absolute_points: 0.0,
            });
            snap.next_entity_id += 1;
            let jid = snap.next_entity_id;
            snap.wallet_journal.push(eve_db::WalletJournalEntry {
                entry_id: jid,
                character_id: char_id,
                entry_type: "skill_purchase".into(),
                amount: eve_db::IskCents(-cost_c),
                balance_after: eve_db::IskCents(new_bal),
                reason: format!("Direct skill purchase and injection (type {sk_id})"),
            });
            snap.wallet_journal_timestamps.insert(jid, now_unix);
            bought.push(sk_id);
        }
        Ok((bought, total_cost, new_bal))
    })?;

    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnAccountChange",
        "charid",
        vec![
            PyRep::String("cash".into()),
            PyRep::Integer(char_id),
            PyRep::Real(eve_db::IskCents(new_bal_cents).to_f64_isk()),
        ],
    );
    let changes: Vec<(PyRep, PyRep)> = purchased
        .iter()
        .map(|&sk_id| {
            (
                PyRep::Integer(sk_id as i64),
                PyRep::keyval([
                    ("typeID", PyRep::Integer(sk_id as i64)),
                    ("skillLevel", PyRep::Integer(0)),
                    ("skillPoints", PyRep::Real(0.0)),
                ]),
            )
        })
        .collect();
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnServerSkillsChanged",
        "charid",
        vec![PyRep::Dict(changes)],
    );
    Ok((purchased, total_cost_cents, new_bal_cents))
}

fn purchase_skills_direct(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let mut skill_ids = Vec::new();
    let src = args
        .first()
        .or_else(|| kwargs.get("typeIDs"))
        .or_else(|| kwargs.get("typeID"));
    if let Some(rep) = src {
        match rep {
            PyRep::Integer(v) if *v > 0 => skill_ids.push(*v as u32),
            PyRep::List(l) | PyRep::Tuple(l) | PyRep::Set(l) => {
                for el in l {
                    if let Some(v) = el.as_i64().filter(|&x| x > 0) {
                        skill_ids.push(v as u32);
                    }
                }
            }
            _ => {}
        }
    }
    let (purchased, total_cost_cents, new_bal_cents) =
        purchase_skills_direct_for_char(ctx, char_id, &skill_ids)?;
    Ok(PyRep::keyval([
        (
            "purchasedSkillTypeIDs",
            PyRep::List(purchased.into_iter().map(|id| PyRep::Integer(id as i64)).collect()),
        ),
        ("totalCostISK", PyRep::Real(eve_db::IskCents(total_cost_cents).to_f64_isk())),
        ("newBalanceISK", PyRep::Real(eve_db::IskCents(new_bal_cents).to_f64_isk())),
    ]))
}

/// Tuesday 11:00 UTC downtime weekly window ID (`feat:skills.omega-weekly-free-sp`).
/// Epoch `0` is Thursday 1970-01-01 00:00:00 UTC; Tuesday 11:00 UTC is +471,600s from Thursday 00:00 UTC.
fn current_tuesday_downtime_week_id(now_unix: u64) -> u64 {
    now_unix.saturating_add(604_800 - 471_600) / 604_800
}

fn handle_omega_weekly_free_sp(
    ctx: &ServerContext,
    session: &mut SessionState,
    is_buy: bool,
) -> Result<PyRep, BridgeRpcError> {
    let (user_id, char_id) = ServerContext::require_character(session)?;
    let now_unix = ctx.now_unix();
    let week_id = current_tuesday_downtime_week_id(now_unix);
    let blob_key = format!("omega_weekly_sp_claim:{user_id}");

    if !is_buy {
        let can_claim = ctx.db.with_snapshot(|snap| {
            let Some(ch) = snap.characters.get(&char_id) else {
                return false;
            };
            let exp_opt = super::account_services::subscription_expiry_for_user(snap, ch.user_id);
            let is_omega = ch.clone_grade >= 2 && exp_opt.map(|u| u > now_unix).unwrap_or(true);
            if !is_omega {
                return false;
            }
            let last_week = snap
                .service_state_blobs
                .get(&blob_key)
                .and_then(|s| s.parse::<u64>().ok());
            last_week != Some(week_id)
        });
        return Ok(PyRep::Bool(can_claim));
    }

    let new_free_sp = ctx.db.try_with_mut_snapshot(|snap| {
        let exp_opt = super::account_services::subscription_expiry_for_user(snap, user_id);
        let ch = snap.characters.get_mut(&char_id).ok_or_else(|| BridgeRpcError {
            code: "CharacterNotFound".into(),
            message: format!("Character {char_id} not found"),
        })?;
        let is_omega = ch.clone_grade >= 2 && exp_opt.map(|u| u > now_unix).unwrap_or(true);
        if !is_omega {
            return Err(BridgeRpcError {
                code: "OmegaRequired".into(),
                message: "Omega clone state is required to claim the weekly free 15,000 SP offer".into(),
            });
        }
        let last_week = snap
            .service_state_blobs
            .get(&blob_key)
            .and_then(|s| s.parse::<u64>().ok());
        if last_week == Some(week_id) {
            return Err(BridgeRpcError {
                code: "WeeklyOfferAlreadyClaimed".into(),
                message: "Weekly free 15,000 SP offer has already been claimed this week".into(),
            });
        }
        ch.free_skill_points += 15_000.0;
        let updated_sp = ch.free_skill_points;
        snap.service_state_blobs
            .insert(blob_key, week_id.to_string());
        Ok(updated_sp)
    })?;

    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnFreeSkillPointsChanged",
        "charid",
        vec![PyRep::Real(new_free_sp)],
    );
    Ok(PyRep::keyval([
        ("offerID", PyRep::Integer(1005)),
        ("freeSkillPointsGranted", PyRep::Integer(15_000)),
        ("freeSkillPoints", PyRep::Real(new_free_sp)),
    ]))
}

fn strip_ship_fitting(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let Some(dock_id) = session.station_id.or(session.structure_id) else {
        return Err(BridgeRpcError {
            code: "CannotStripFittingInSpace".into(),
            message: "StripFitting requires the character to be docked in a station or structure".into(),
        });
    };
    let ship_id = args
        .first()
        .and_then(|a| a.as_i64())
        .or_else(|| kwargs.get("shipID").and_then(|v| v.as_i64()))
        .or(session.ship_id)
        .ok_or_else(|| BridgeRpcError {
            code: "NoActiveShip".into(),
            message: "No target ship specified for StripFitting".into(),
        })?;

    let stripped = ctx.db.try_with_mut_snapshot(|snap| {
        crate::services::structure_acl_defense_rules::validate_docked_ship_fitting_facility_in_snap(
            ctx, snap, dock_id,
        )?;
        let Some(ship) = snap.items.get(&ship_id) else {
            return Err(BridgeRpcError {
                code: "ShipNotFound".into(),
                message: format!("Ship {ship_id} not found"),
            });
        };
        if ship.owner_id != char_id {
            return Err(BridgeRpcError {
                code: "OwnershipViolation".into(),
                message: format!("Ship {ship_id} is not owned by character {char_id}"),
            });
        }
        if ship.location_id != dock_id {
            return Err(BridgeRpcError {
                code: "ShipNotAtStation".into(),
                message: format!("Ship {ship_id} is not docked at station/structure {dock_id}"),
            });
        }
        if snap.contracts.values().any(|c| {
            c.item_ids.contains(&ship_id)
                && (c.status.eq_ignore_ascii_case("outstanding")
                    || c.status.eq_ignore_ascii_case("inprogress")
                    || c.status.eq_ignore_ascii_case("in_progress"))
        }) {
            return Err(BridgeRpcError {
                code: "ItemLockedInContract".into(),
                message: format!("Ship {ship_id} is locked in an active contract"),
            });
        }

        // Collect all fitted modules + charges (11..=34, 164..=171) and drone-bay drones (87).
        // Rigs (92..=94) and T3 subsystems (125..=132) remain fitted.
        let candidate_ids: Vec<(i64, eve_db::InventoryFlag)> = snap
            .items
            .values()
            .filter(|i| {
                i.location_id == ship_id
                    && i.owner_id == char_id
                    && matches!(i.flag as u16, 11..=34 | 87 | 164..=171)
            })
            .map(|i| (i.item_id, i.flag))
            .collect();

        let mut moved = Vec::new();
        for (iid, old_flag) in candidate_ids {
            if let Some(it) = snap.items.get_mut(&iid) {
                it.location_id = dock_id;
                it.flag = eve_db::InventoryFlag::Hangar;
                it.version += 1;
                moved.push((it.clone(), ship_id, old_flag));
            }
            snap.module_online_states.remove(&iid);
            snap.module_active_states.remove(&iid);
            snap.module_active_charges.remove(&iid);
        }
        Ok(moved)
    })?;

    ctx.invalidate_ship_fitting(ship_id);
    if !stripped.is_empty() {
        let mut grouped: std::collections::BTreeMap<(i64, i64), Vec<PyRep>> =
            std::collections::BTreeMap::new();
        for (item, prev_loc, prev_flag) in &stripped {
            let (group_id, category_id) = ctx
                .sde
                .types
                .get(&item.type_id)
                .map(|t| (i64::from(t.group_id), i64::from(t.category_id)))
                .unwrap_or((0, 0));
            let singleton_val = if item.is_blueprint_copy {
                2
            } else if item.singleton {
                1
            } else {
                0
            };
            let qty_val = if item.is_blueprint_copy {
                -2
            } else if item.singleton {
                -1
            } else {
                i64::from(item.quantity)
            };
            grouped
                .entry((*prev_loc, *prev_flag as i64))
                .or_default()
                .push(PyRep::keyval([
                    ("itemID", PyRep::Integer(item.item_id)),
                    ("typeID", PyRep::Integer(i64::from(item.type_id))),
                    ("ownerID", PyRep::Integer(item.owner_id)),
                    ("locationID", PyRep::Integer(item.location_id)),
                    ("flagID", PyRep::Integer(item.flag as i64)),
                    ("quantity", PyRep::Integer(qty_val)),
                    ("stacksize", PyRep::Integer(i64::from(item.quantity))),
                    ("singleton", PyRep::Integer(singleton_val)),
                    ("groupID", PyRep::Integer(group_id)),
                    ("categoryID", PyRep::Integer(category_id)),
                    ("customInfo", PyRep::String(String::new())),
                ]));
            if matches!(*prev_flag as u16, 11..=34 | 164..=171) {
                ctx.notify(
                    NotificationTarget::Character(char_id),
                    "OnModuleUnfitted",
                    "charid",
                    vec![PyRep::Integer(ship_id), PyRep::Integer(item.item_id)],
                );
            }
        }
        for ((prev_loc, prev_flag), item_kvs) in grouped {
            let change_dict = PyRep::Dict(vec![
                (PyRep::Integer(3), PyRep::Integer(prev_loc)),
                (PyRep::Integer(4), PyRep::Integer(prev_flag)),
            ]);
            ctx.notify(
                NotificationTarget::Character(char_id),
                "OnItemsChanged",
                "charid",
                vec![PyRep::List(item_kvs), change_dict, PyRep::Integer(dock_id)],
            );
        }
    }

    let stripped_ids: Vec<PyRep> = stripped
        .into_iter()
        .map(|(it, _, _)| PyRep::Integer(it.item_id))
        .collect();
    Ok(PyRep::keyval([
        ("shipID", PyRep::Integer(ship_id)),
        ("strippedItemIDs", PyRep::List(stripped_ids)),
    ]))
}

/// Post-hook after `inventory_fitting::handle` for `feat:inventory.jetcan-capacity-and-despawn`.
pub async fn verify_in_space_container_proximity(
    ctx: &ServerContext,
    char_id: i64,
    container_loc_id: i64,
    container_id: i64,
) -> Result<(), BridgeRpcError> {
    if !(30_000_000..=33_000_000).contains(&container_loc_id) {
        return Ok(());
    }
    let sys_id = container_loc_id as u32;
    let (char_sys, docked, ship_id) = ctx.db.with_snapshot(|snap| {
        let ch = snap.characters.get(&char_id);
        (
            ch.map(|c| c.solar_system_id).unwrap_or(0),
            ch.and_then(|c| c.station_id.or(c.structure_id)),
            ch.map(|c| c.active_ship_id).unwrap_or(0),
        )
    });
    if docked.is_some() || char_sys != sys_id {
        return Err(BridgeRpcError {
            code: "NotInSameSolarSystem".into(),
            message: format!("Caller must be in space in solar system {sys_id} to open container {container_id}"),
        });
    }
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    if let (Ok(Some((_, ship_ball))), Ok(Some((_, cont_ball)))) = (
        sys_handle.get_ball_snapshot(ship_id).await,
        sys_handle.get_ball_snapshot(container_id).await,
    ) {
        let edge_dist = ((ship_ball.position - cont_ball.position).length() - ship_ball.radius - cont_ball.radius).max(0.0);
        if edge_dist > 2_500.0 {
            return Err(BridgeRpcError {
                code: "OutOfRange".into(),
                message: format!("Container {container_id} is out of 2,500m range ({edge_dist:.1}m)"),
            });
        }
    }
    Ok(())
}

pub async fn post_inventory_fitting_hook(
    ctx: &ServerContext,
    session: &SessionState,
    bound_entry: Option<&crate::router::BoundObjectEntry>,
    method: &str,
    args: &[PyRep],
) {
    let Some(char_id) = session.character_id else {
        return;
    };
    let now_unix = ctx.now_unix();

    match method {
        "Drop" => {
            let Some(sys_id) = session.solar_system_id else {
                return;
            };
            let new_containers: Vec<(i64, u32)> = ctx.db.with_mut_snapshot(|snap| {
                let candidates: Vec<(i64, u32)> = snap
                    .items
                    .values()
                    .filter(|i| {
                        i.location_id == sys_id as i64
                            && i.flag == eve_db::InventoryFlag::None
                            && !snap.sp6_lifecycle.space_object_meta.contains_key(&i.item_id)
                            && ctx
                                .sde
                                .types
                                .get(&i.type_id)
                                .is_some_and(|t| matches!(t.group_id, 12 | 340 | 448))
                    })
                    .map(|i| (i.item_id, i.type_id))
                    .collect();
                for &(iid, _) in &candidates {
                    crate::sp6_lifecycle::record_wreck_or_container_meta_in_snap(
                        snap, iid, now_unix, false,
                    );
                }
                candidates
            });
            for (iid, tid) in new_containers {
                crate::services::combat_crimewatch::register_container_lifetime(
                    ctx, iid, sys_id, tid, char_id, now_unix, false,
                );
            }
        }
        "ValidateItemCanBeOpened" | "ValidateItemListCanBeOpened" | "List" => {
            let mut opened_ids = Vec::new();
            if let Some(b) = bound_entry {
                opened_ids.push(b.object_id);
            }
            if let Some(first) = args.first() {
                if let Some(id) = first.as_i64() {
                    opened_ids.push(id);
                } else if let PyRep::List(l) | PyRep::Tuple(l) = first {
                    for el in l {
                        if let Some(id) = el.as_i64() {
                            opened_ids.push(id);
                        }
                    }
                }
            }
            let mut valid_ids = Vec::new();
            for cid in opened_ids {
                let loc_opt = ctx.db.with_snapshot(|s| s.items.get(&cid).map(|i| i.location_id));
                if let Some(loc_id) = loc_opt {
                    if verify_in_space_container_proximity(ctx, char_id, loc_id, cid).await.is_ok() {
                        valid_ids.push(cid);
                    }
                }
            }
            if !valid_ids.is_empty() {
                ctx.db.with_mut_snapshot(|snap| {
                    for cid in valid_ids {
                        if let Some(meta) = snap.sp6_lifecycle.space_object_meta.get_mut(&cid) {
                            if meta.anchor_start_unix.is_some() {
                                meta.anchor_start_unix = Some(now_unix);
                                meta.created_unix = Some(now_unix);
                            }
                        }
                    }
                });
            }
        }
        "Activate"
        | "Overload"
        | "Deactivate"
        | "TakeModuleOffline"
        | "DestroyFitting"
        | "Add"
        | "MultiAdd" => {
            if session.station_id.is_none() && session.structure_id.is_none() {
                if let (Some(sys_id), Some(ship_id)) = (session.solar_system_id, session.ship_id) {
                    crate::services::space_destiny::sync_ship_cloak_and_mjfg_from_fitting(
                        ctx, char_id, sys_id, ship_id,
                    )
                    .await;
                }
            }
        }
        _ => {}
    }
}
