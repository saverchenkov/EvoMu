//! `drones_salvage_tractor` — Salvage mechanics, Tractor Beam, and Drone Bandwidth & Control (`C-7`).

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, DamageVector, MovementMode};
use glam::DVec3;
use std::collections::HashMap;

/// Outcome of attempting to salvage a wreck entity.
#[derive(Debug, Clone, PartialEq)]
pub enum SalvageOutcome {
    Success {
        yielded_salvage_type_ids: Vec<u32>,
        yielded_quantities: Vec<i32>,
    },
    Failed {
        reason: &'static str,
    },
}

#[derive(Debug)]
enum CommitSalvageError {
    Capacity {
        ship_cap: f64,
        used_cargo: f64,
        salvage_vol: f64,
    },
    WreckMissing,
    Wal(String),
}

impl eve_db::FromWalError for CommitSalvageError {
    fn from_wal_error(err: String) -> Self {
        Self::Wal(err)
    }
}

fn target_not_in_space_error(message: String) -> eve_proto::BridgeRpcError {
    eve_proto::BridgeRpcError {
        code: "TargetNotInSpace".into(),
        message,
        client_error: None,
    }
}

fn resolve_ship_cargo_capacity(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
) -> Option<f64> {
    ctx.get_or_rebuild_ship_fitting(char_id, ship_id)
        .ok()
        .map(|fs| fs.ship.get_attribute(eve_sde::attr_ids::CAPACITY, &ctx.sde))
        .filter(|&c| c > 0.0)
        .or_else(|| {
            ctx.db.with_snapshot(|s| {
                s.items
                    .get(&ship_id)
                    .and_then(|it| ctx.sde.types.get(&it.type_id).map(|t| t.capacity))
                    .filter(|&c| c > 0.0)
            })
        })
}

fn salvage_vol_for_materials(ctx: &ServerContext, materials: &[(u32, i32)]) -> f64 {
    materials
        .iter()
        .map(|&(tid, qty)| {
            ctx.sde.types.get(&tid).map(|t| t.volume).unwrap_or(0.01) * (qty as f64)
        })
        .sum()
}

/// Commits yielded salvage items, optional loot-transfer jetcan, and wreck item removal, then
/// emits `OnItemChange` notifications after releasing the repository lock.
#[allow(clippy::too_many_arguments)]
fn commit_salvaged_wreck_inventory_and_notify(
    ctx: &ServerContext,
    sys_id: u32,
    char_id: i64,
    ship_id: i64,
    wreck_id: i64,
    wreck_pos: [f64; 3],
    salvage_materials: &[(u32, i32)],
) -> Result<Option<(i64, i64)>, CommitSalvageError> {
    let salvage_vol = salvage_vol_for_materials(ctx, salvage_materials);
    let ship_cap_opt = resolve_ship_cargo_capacity(ctx, char_id, ship_id);
    let (spawned_can_opt, item_mutations) = ctx.db.try_with_mut_snapshot(|snap| {
        let Some(old_wreck) = snap.items.get(&wreck_id).cloned() else {
            return Err(CommitSalvageError::WreckMissing);
        };
        if old_wreck.location_id != sys_id as i64 {
            return Err(CommitSalvageError::WreckMissing);
        }
        if let Some(ship_cap) = ship_cap_opt {
            let used_cargo: f64 = snap
                .items
                .values()
                .filter(|i| i.location_id == ship_id && i.flag == InventoryFlag::Cargo)
                .map(|i| {
                    ctx.sde.types.get(&i.type_id).map(|t| t.volume).unwrap_or(0.01)
                        * (i.quantity.max(1) as f64)
                })
                .sum();
            if used_cargo + salvage_vol > ship_cap + 1e-4 {
                return Err(CommitSalvageError::Capacity {
                    ship_cap,
                    used_cargo,
                    salvage_vol,
                });
            }
        }
        let wreck_owner = old_wreck.owner_id;
        let mut child_ids: Vec<i64> = snap
            .items
            .values()
            .filter(|i| i.location_id == wreck_id)
            .map(|i| i.item_id)
            .collect();
        child_ids.sort_unstable();
        let mut item_mutations: Vec<(Option<InventoryItemRecord>, Option<InventoryItemRecord>)> =
            Vec::with_capacity(child_ids.len() + salvage_materials.len() + 1);
        let can_id_opt = if !child_ids.is_empty() {
            snap.next_entity_id += 1;
            let can_id = snap.next_entity_id;
            snap.items.insert(
                can_id,
                InventoryItemRecord {
                    item_id: can_id,
                    type_id: 23,
                    owner_id: wreck_owner,
                    location_id: sys_id as i64,
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.item_positions.insert(can_id, wreck_pos);
            for cid in child_ids {
                if let Some(child) = snap.items.get_mut(&cid) {
                    let pre = child.clone();
                    child.location_id = can_id;
                    child.version += 1;
                    item_mutations.push((Some(pre), Some(child.clone())));
                }
            }
            Some((can_id, wreck_owner))
        } else {
            None
        };

        snap.items.remove(&wreck_id);
        snap.item_positions.remove(&wreck_id);
        item_mutations.push((Some(old_wreck), None));

        for &(tid, qty) in salvage_materials {
            snap.next_entity_id += 1;
            let iid = snap.next_entity_id;
            let rec = InventoryItemRecord {
                item_id: iid,
                type_id: tid,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: qty,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            };
            snap.items.insert(iid, rec.clone());
            item_mutations.push((None, Some(rec)));
        }
        Ok((can_id_opt, item_mutations))
    })?;

    for (pre, post) in &item_mutations {
        ctx.notify_item_mutation(char_id, pre.as_ref(), post.as_ref());
        if let (Some(pre_rec), Some(post_rec)) = (pre.as_ref(), post.as_ref()) {
            if pre_rec.location_id == wreck_id && post_rec.location_id != wreck_id {
                let (resolved_target, _) = ctx.resolve_item_notification_target(char_id, post_rec);
                if !matches!(
                    resolved_target,
                    crate::router::NotificationTarget::Character(cid) if cid == char_id
                ) {
                    let _ = ctx.notify_item_record_change_with_target(
                        crate::router::NotificationTarget::Character(char_id),
                        "charid",
                        post_rec,
                        vec![(3, eve_proto::PyRep::Integer(pre_rec.location_id))],
                    );
                }
            }
        }
    }

    Ok(spawned_can_opt)
}

/// Spawns any loot-holding cargo container ball, removes the salvaged wreck ball from `Ballpark`,
/// emits target-lost notices for locked observers, and flushes peer observer `DoDestinyUpdate`
/// frames immediately before module deactivation / `OnSpecialFX(start=0, active=0)` fires.
///
/// When `active_module_id_opt` is `Some(module_id)`, removes that active module cycle from the
/// system actor prior to `remove_ball` so the target-removal deactivation hook does not emit
/// `OnGodmaShipEffect(false)` / `OnSpecialFX(start=0)` before `RemoveBall` flushes.
#[allow(clippy::too_many_arguments)]
async fn finalize_salvaged_wreck_ball(
    ctx: &ServerContext,
    sys_handle: &crate::system_actor::SystemHandle,
    sys_id: u32,
    char_id: i64,
    ship_id: i64,
    wreck_id: i64,
    wreck_pos: [f64; 3],
    spawned_can_opt: Option<(i64, i64)>,
    active_module_id_opt: Option<i64>,
) -> bool {
    if let Some((can_id, wreck_owner)) = spawned_can_opt {
        crate::services::combat_crimewatch::register_container_lifetime(
            ctx,
            can_id,
            sys_id,
            23,
            wreck_owner,
            ctx.now_unix(),
            false,
        );
        let _ = sys_handle
            .spawn_ball_with_meta(
                Ball {
                    id: can_id,
                    type_id: 23,
                    position: DVec3::from_array(wreck_pos),
                    velocity: DVec3::ZERO,
                    max_velocity: 0.0,
                    inertia: 1.0,
                    mass: 1_000.0,
                    radius: 20.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                eve_destiny::CombatState::default(),
                eve_destiny::SlimItemMeta {
                    group_id: 12,
                    category_id: 2,
                    owner_id: wreck_owner,
                    char_id: None,
                    corp_id: -1,
                    alliance_id: -1,
                    security_status: 0.0,
                    is_cloaked: false,
                    name: ctx.sde.types.get(&23).map(|t| t.name.clone()),
                    ..Default::default()
                },
            )
            .await;
    }

    crate::services::combat_crimewatch::with_mut_combat_runtime(ctx, |rt| {
        rt.container_lifetimes.remove(&wreck_id);
    });

    let view = sys_handle.view();
    let affected_locks: Vec<(i64, bool)> = view
        .target_locks
        .iter()
        .filter_map(|(&src_id, locks)| {
            let matching: Vec<bool> = locks
                .iter()
                .filter(|l| l.target_id == wreck_id)
                .map(|l| l.locked)
                .collect();
            if matching.is_empty() {
                None
            } else {
                Some((src_id, matching.into_iter().any(|b| b)))
            }
        })
        .collect();

    let pre_removed_cycle = if let Some(module_id) = active_module_id_opt {
        sys_handle
            .remove_module_cycle(ship_id, module_id)
            .await
            .ok()
            .flatten()
            .is_some()
    } else {
        false
    };

    let _ = sys_handle.remove_ball(wreck_id).await;

    if !affected_locks.is_empty() {
        let pilot_ids: Vec<(i64, bool)> = ctx.db.with_snapshot(|s| {
            affected_locks
                .iter()
                .filter_map(|&(src_id, was_locked)| {
                    let cid = s
                        .characters
                        .values()
                        .find(|c| c.active_ship_id == src_id)
                        .map(|c| c.character_id)
                        .or_else(|| {
                            s.items
                                .get(&src_id)
                                .map(|i| i.owner_id)
                                .filter(|oid| s.characters.contains_key(oid))
                        })
                        .filter(|&c| c > 0)?;
                    Some((cid, was_locked))
                })
                .collect()
        });
        for (cid, was_locked) in pilot_ids {
            if was_locked {
                ctx.emit_typed(
                    crate::router::NotificationTarget::Character(cid),
                    crate::router::notify_registry::OnTarget {
                        arg0: eve_proto::PyRep::Integer(wreck_id),
                        arg1: eve_proto::PyRep::String("lost".into()),
                        reason: None,
                    },
                );
                ctx.emit_typed(
                    crate::router::NotificationTarget::Character(cid),
                    crate::router::notify_registry::OnMultiEvent {
                        events: vec![eve_proto::PyRep::Tuple(vec![
                            eve_proto::PyRep::Tuple(vec![
                                eve_proto::PyRep::String("OnTarget".into()),
                                eve_proto::PyRep::String("lost".into()),
                                eve_proto::PyRep::Integer(wreck_id),
                                eve_proto::PyRep::String(String::new()),
                            ]),
                            eve_proto::PyRep::Integer(0),
                        ])],
                    },
                );
            } else {
                ctx.emit_typed(
                    crate::router::NotificationTarget::Character(cid),
                    crate::router::notify_registry::FailLockTarget {
                        target_id: wreck_id,
                    },
                );
            }
        }
    }

    let _ = sys_handle.flush_peer_observer_updates(None).await;
    record_wreck_salvage_progression(ctx, sys_id, char_id);
    pre_removed_cycle
}

/// Executes a salvager cycle against a target wreck:
/// - Evaluates base chance + salvager access difficulty bonus.
/// - On success: removes wreck ball and item, grants salvage items (from SDE salvage materials).
/// - On failure: wreck remains intact for subsequent attempts.
pub async fn attempt_salvage_wreck(
    ctx: &ServerContext,
    sys_id: u32,
    char_id: i64,
    ship_id: i64,
    wreck_item_id: i64,
    salvager_access_bonus: f64,
    roll_u: f64,
) -> Result<SalvageOutcome, String> {
    let sys_handle = ctx.systems.get_or_spawn(sys_id);

    // Verify distance between ship and wreck is within salvager range (5,000 m)
    let ship_ball = sys_handle
        .get_ball_snapshot(ship_id)
        .await
        .map_err(|e| format!("Query ship failed: {e}"))?
        .map(|(_, b)| b)
        .ok_or_else(|| format!("Ship {ship_id} not found in space"))?;

    let wreck_ball = sys_handle
        .get_ball_snapshot(wreck_item_id)
        .await
        .map_err(|e| format!("Query wreck failed: {e}"))?
        .map(|(_, b)| b)
        .ok_or_else(|| format!("Wreck {wreck_item_id} not found in space"))?;

    let max_salvage_range = ctx
        .get_or_rebuild_ship_fitting(char_id, ship_id)
        .ok()
        .and_then(|fit| {
            fit.evaluated_modules
                .values()
                .filter(|ms| {
                    ctx.sde
                        .types
                        .get(&ms.type_id)
                        .is_some_and(|t| t.group_id == 1122)
                })
                .map(|ms| ms.get_attribute(54, &ctx.sde))
                .fold(None, |acc: Option<f64>, r| Some(acc.map_or(r, |a| a.max(r))))
        })
        .unwrap_or(5_000.0)
        .max(5_000.0);
    let dist = ship_ball.position.distance(wreck_ball.position);
    if dist > max_salvage_range {
        return Err(format!(
            "Wreck is {:.1} m away; salvager maximum range is 5,000 m",
            dist
        ));
    }

    let can_capacity = ctx
        .sde
        .types
        .get(&23)
        .map(|t| t.capacity)
        .filter(|&c| c > 0.0 && c <= 27_500.0)
        .unwrap_or(27_500.0);
    let loot_volume: f64 = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .filter(|i| i.location_id == wreck_item_id)
            .map(|i| {
                let unit_vol = ctx
                    .sde
                    .types
                    .get(&i.type_id)
                    .map(|t| t.volume)
                    .unwrap_or(1.0);
                unit_vol * (i.quantity.max(1) as f64)
            })
            .sum()
    });
    if loot_volume > can_capacity + 1e-4 {
        return Err("SalvageTooMuchLootBody".to_string());
    }

    // Base salvage chance: 20% + salvager access bonus (e.g. +30% for Salvager I = 50%)
    let chance = (0.20 + salvager_access_bonus).clamp(0.05, 0.95);

    if roll_u < chance {
        // Select salvage materials from SDE-derived racial/tier salvage pools (`SalvageMaterialPools`).
        let salvage_materials = ctx
            .sde
            .salvage_materials_for_wreck(wreck_ball.type_id, false);
        let wreck_pos = [
            wreck_ball.position.x,
            wreck_ball.position.y,
            wreck_ball.position.z,
        ];

        let spawned_can_opt = commit_salvaged_wreck_inventory_and_notify(
            ctx,
            sys_id,
            char_id,
            ship_id,
            wreck_item_id,
            wreck_pos,
            &salvage_materials,
        )
        .map_err(|err| match err {
            CommitSalvageError::Capacity { .. } => "NotEnoughCargoSpace".to_string(),
            CommitSalvageError::WreckMissing => "Wreck no longer exists".to_string(),
            CommitSalvageError::Wal(msg) => msg,
        })?;

        let _ = finalize_salvaged_wreck_ball(
            ctx,
            &sys_handle,
            sys_id,
            char_id,
            ship_id,
            wreck_item_id,
            wreck_pos,
            spawned_can_opt,
            None,
        )
        .await;

        let types = salvage_materials.iter().map(|&(t, _)| t).collect();
        let quantities = salvage_materials.iter().map(|&(_, q)| q).collect();

        Ok(SalvageOutcome::Success {
            yielded_salvage_type_ids: types,
            yielded_quantities: quantities,
        })
    } else {
        Ok(SalvageOutcome::Failed {
            reason: "Salvager cycle failed to extract materials",
        })
    }
}

fn record_wreck_salvage_progression(ctx: &ServerContext, sys_id: u32, char_id: i64) {
    crate::services::social_corp_fleet::social_corp_projects::record_server_project_activity(
        ctx,
        char_id,
        &["SalvageWrecks", "SalvageMaterials"],
        1,
        Some(sys_id as i64),
    );
    crate::quasar_gateway::isolate_side_effect("combat.drones_salvage_tractor.career_salvage", || {
        crate::services::quasar_career_goals::record_career_sub_kind(
            ctx,
            char_id,
            "salvage",
            None,
            1,
        );
    });
}

/// Validates that `wreck_id` is a salvageable wreck in `sys_id`, within jetcan loot capacity, and within salvager/drone range.
pub async fn validate_wreck_salvage_target(
    ctx: &ServerContext,
    sys_id: u32,
    char_id: i64,
    ship_id: i64,
    wreck_id: i64,
    salvager_type_id_opt: Option<u32>,
) -> Result<(u32, Option<Ball>), eve_proto::BridgeRpcError> {
    use eve_proto::BridgeRpcError;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let wreck_ball_opt = sys_handle.get_ball_snapshot(wreck_id).await.ok().flatten().map(|(_, b)| b);
    let (wreck_type_id, loot_volume) = ctx.db.with_snapshot(|snap| {
        let Some(it) = snap.items.get(&wreck_id) else {
            return Err(target_not_in_space_error(format!(
                "Target wreck {wreck_id} does not exist"
            )));
        };
        if it.location_id != sys_id as i64 {
            return Err(target_not_in_space_error(format!(
                "Target {wreck_id} is not in solar system {sys_id}"
            )));
        }
        let is_wreck = it.type_id == 26468
            || ctx
                .sde
                .types
                .get(&it.type_id)
                .is_some_and(|t| t.group_id == 186);
        if !is_wreck {
            return Err(BridgeRpcError {
                code: "InvalidSalvageTarget".into(),
                message: format!("Target {wreck_id} (type {}) is not a salvageable wreck", it.type_id),
                client_error: None,
            });
        }
        let vol: f64 = snap
            .items
            .values()
            .filter(|i| i.location_id == wreck_id)
            .map(|i| {
                let unit_vol = ctx
                    .sde
                    .types
                    .get(&i.type_id)
                    .map(|t| t.volume)
                    .unwrap_or(1.0);
                unit_vol * (i.quantity.max(1) as f64)
            })
            .sum();
        Ok((it.type_id, vol))
    })?;

    let can_capacity = ctx
        .sde
        .types
        .get(&23)
        .map(|t| t.capacity)
        .filter(|&c| c > 0.0 && c <= 27_500.0)
        .unwrap_or(27_500.0);
    if loot_volume > can_capacity + 1e-4 {
        return Err(BridgeRpcError {
            code: "SalvageTooMuchLootBody".into(),
            message: format!(
                "Salvaging wreck {wreck_id} failed: SalvageTooMuchLootBody ({loot_volume:.1} m³ exceeds cargo container capacity {can_capacity:.1} m³)"
            ),
            client_error: None,
        });
    }

    if let Some(salvager_tid) = salvager_type_id_opt {
        if let (Some((_, sb)), Some(ref wb)) = (
            sys_handle.get_ball_snapshot(ship_id).await.ok().flatten(),
            &wreck_ball_opt,
        ) {
            let eval_mod_range = ctx
                .get_or_rebuild_ship_fitting(char_id, ship_id)
                .ok()
                .and_then(|fit| {
                    fit.evaluated_modules
                        .values()
                        .filter(|ms| ms.type_id == salvager_tid)
                        .map(|ms| ms.get_attribute(54, &ctx.sde))
                        .fold(None, |acc: Option<f64>, r| Some(acc.map_or(r, |a| a.max(r))))
                })
                .filter(|&v| v > 0.0);
            let max_mod_range = eval_mod_range
                .or_else(|| {
                    ctx.sde
                        .types
                        .get(&salvager_tid)
                        .and_then(|t| t.default_attributes.get(&54).copied())
                })
                .unwrap_or(6_000.0)
                .max(6_000.0);
            let dist = (sb.position.distance(wb.position) - sb.radius - wb.radius).max(0.0);
            if dist > max_mod_range {
                return Err(eve_proto::BridgeRpcError::target_out_of_range(
                    crate::services::inventory_fitting::resolve_target_display_name(ctx, wreck_id),
                    salvager_tid as i64,
                    dist,
                    format!(
                        "Wreck {wreck_id} is {dist:.1} m away (exceeds salvager module range {max_mod_range:.1} m)"
                    ),
                ));
            }
        }
    } else {
        let Some(ref wb) = wreck_ball_opt else {
            return Err(target_not_in_space_error(format!(
                "Target wreck {wreck_id} is not in space in solar system {sys_id}"
            )));
        };
        if let Some((_, sb)) = sys_handle.get_ball_snapshot(ship_id).await.ok().flatten() {
            let max_drone_range =
                crate::services::combat_crimewatch::compute_drone_control_range(ctx, char_id, ship_id).max(60_000.0);
            let dist = sb.position.distance(wb.position);
            if dist > max_drone_range {
                return Err(eve_proto::BridgeRpcError::target_out_of_range(
                    crate::services::inventory_fitting::resolve_target_display_name(ctx, wreck_id),
                    0,
                    dist,
                    format!(
                        "Wreck {wreck_id} is {dist:.1} m away (exceeds salvage drone range {max_drone_range:.1} m)"
                    ),
                ));
            }
        }
    }
    Ok((wreck_type_id, wreck_ball_opt))
}

/// Core wreck salvaging cycle shared by Salvager modules (`25861`/`30836`) and Salvage Drones (`32787`)
/// (`feat:modules.salvager`, `feat:combat.wreck-size-salvage-difficulty`, `feat:drones.salvage-drones`, `feat:lifecycle.wreck-salvage_with_loot`).
pub async fn execute_wreck_salvage_cycle(
    ctx: &ServerContext,
    sys_id: u32,
    char_id: i64,
    ship_id: i64,
    wreck_id: i64,
    salvager_type_id_opt: Option<u32>,
) -> Result<eve_proto::PyRep, eve_proto::BridgeRpcError> {
    execute_wreck_salvage_cycle_for_module(
        ctx,
        sys_id,
        char_id,
        ship_id,
        wreck_id,
        salvager_type_id_opt,
        None,
    )
    .await
    .map(|(rep, _)| rep)
}

/// Core wreck salvaging cycle with optional active repeating `module_id` so `SalvageEnd` can
/// cleanly pre-remove its `ActiveModuleCycle` before `remove_ball` and flush `RemoveBall` prior
/// to module deactivation notifications (`#1361`). Returns `(result_keyval, cycle_pre_removed)`.
pub(crate) async fn execute_wreck_salvage_cycle_for_module(
    ctx: &ServerContext,
    sys_id: u32,
    char_id: i64,
    ship_id: i64,
    wreck_id: i64,
    salvager_type_id_opt: Option<u32>,
    active_module_id_opt: Option<i64>,
) -> Result<(eve_proto::PyRep, bool), eve_proto::BridgeRpcError> {
    use eve_proto::PyRep;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let (wreck_type_id, wreck_ball_opt) = validate_wreck_salvage_target(
        ctx,
        sys_id,
        char_id,
        ship_id,
        wreck_id,
        salvager_type_id_opt,
    )
    .await?;

    let wreck_difficulty_bonus = ctx
        .sde
        .types
        .get(&wreck_type_id)
        .and_then(|t| t.default_attributes.get(&901).copied())
        .unwrap_or(20.0);

    let fit_opt = ctx.get_or_rebuild_ship_fitting(char_id, ship_id).ok();
    let dogma_salvager_state = fit_opt.as_ref().and_then(|fit| {
        if let Some(tid) = salvager_type_id_opt {
            fit.evaluated_modules
                .values()
                .find(|ms| ms.type_id == tid)
                .or_else(|| {
                    fit.evaluated_drones
                        .values()
                        .find(|ds| ds.type_id == tid)
                })
        } else {
            fit.evaluated_drones
                .values()
                .find(|ds| ds.type_id == 32787 || ds.base_attributes.contains_key(&902))
        }
    });
    let has_dogma_skill_25863 = dogma_salvager_state.is_some_and(|st| {
        st.modifiers
            .iter()
            .any(|m| m.source_item_id == 25863 && m.target_attribute_id == 902)
    });
    let has_dogma_rig_902 = dogma_salvager_state.is_some_and(|st| {
        st.modifiers
            .iter()
            .any(|m| m.source_item_id != 25863 && m.target_attribute_id == 902)
    });

    let mod_access_bonus = dogma_salvager_state
        .filter(|st| {
            st.base_attributes.contains_key(&902)
                || st.modifiers.iter().any(|m| m.target_attribute_id == 902)
        })
        .map(|st| st.get_attribute(902, &ctx.sde))
        .or_else(|| {
            salvager_type_id_opt
                .and_then(|tid| ctx.sde.types.get(&tid))
                .and_then(|t| {
                    t.default_attributes
                        .get(&902)
                        .copied()
                        .or_else(|| t.default_attributes.get(&901).copied())
                })
        })
        .unwrap_or(3.0); // Salvage Drone I base access bonus per cycle

    let (real_salvaging_lv, legacy_salvaging_lv, rig_access_bonus) = ctx.db.with_snapshot(|snap| {
        let sk_map = snap.skills.get(&char_id);
        let sk_real = sk_map
            .and_then(|m| m.get(&25863))
            .map(|s| s.level as f64)
            .unwrap_or(0.0);
        let sk_legacy = sk_map
            .and_then(|m| m.get(&3438))
            .map(|s| s.level as f64)
            .unwrap_or(0.0);
        let rig_bonus: f64 = snap
            .items
            .values()
            .filter(|i| {
                i.location_id == ship_id
                    && matches!(
                        i.flag,
                        InventoryFlag::RigSlot0 | InventoryFlag::RigSlot1 | InventoryFlag::RigSlot2
                    )
            })
            .filter_map(|i| ctx.sde.types.get(&i.type_id))
            .filter_map(|t| {
                t.default_attributes
                    .get(&901)
                    .copied()
                    .or_else(|| (!has_dogma_rig_902).then(|| t.default_attributes.get(&902).copied()).flatten())
            })
            .sum();
        (sk_real, sk_legacy, rig_bonus)
    });

    let extra_skill_bonus = if has_dogma_skill_25863 {
        0.0
    } else if real_salvaging_lv > 0.0 {
        5.0 * real_salvaging_lv
    } else {
        5.0 * legacy_salvaging_lv
    };

    let access_chance_pct =
        mod_access_bonus + extra_skill_bonus + rig_access_bonus + wreck_difficulty_bonus;
    if access_chance_pct <= 0.0 {
        return Ok((
            PyRep::keyval([
                ("wreckID", PyRep::Integer(wreck_id)),
                ("wreckTypeID", PyRep::Integer(wreck_type_id as i64)),
                ("wreckDifficultyBonus", PyRep::Real(wreck_difficulty_bonus)),
                ("accessChancePct", PyRep::Real(access_chance_pct)),
                ("salvaged", PyRep::Bool(false)),
                ("reason", PyRep::String("InsufficientAccessChance".into())),
            ]),
            false,
        ));
    }

    // DERIVED: Uniform [0.0, 100.0) salvager cycle roll drawn directly from `ctx.mechanic_rng`.
    let roll_pct = ctx.mechanic_rng.try_unit_f64().unwrap_or(0.10) * 100.0;
    if roll_pct >= access_chance_pct {
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(w) = snap.items.get_mut(&wreck_id) {
                w.version += 1;
            }
        });
        return Ok((
            PyRep::keyval([
                ("wreckID", PyRep::Integer(wreck_id)),
                ("wreckTypeID", PyRep::Integer(wreck_type_id as i64)),
                ("wreckDifficultyBonus", PyRep::Real(wreck_difficulty_bonus)),
                ("accessChancePct", PyRep::Real(access_chance_pct)),
                ("rollPct", PyRep::Real(roll_pct)),
                ("salvaged", PyRep::Bool(false)),
                ("reason", PyRep::String("CycleRollFailed".into())),
            ]),
            false,
        ));
    }

    // Select T1, T2, or Sleeper salvage loot table from SDE-derived `SalvageMaterialPools`
    // (`sde_types` group 754/966 + `sde_blueprints` + `wrecks.static` + Dogma attribute `901`).
    let salvage_table = ctx.sde.salvage_materials_for_wreck(wreck_type_id, true);

    let yielded_type_ids: Vec<u32> = salvage_table.iter().map(|&(t, _)| t).collect();
    let wreck_pos = wreck_ball_opt
        .as_ref()
        .map(|b| [b.position.x, b.position.y, b.position.z])
        .or_else(|| ctx.db.with_snapshot(|s| s.item_positions.get(&wreck_id).copied()))
        .unwrap_or([0.0, 0.0, 0.0]);
    let spawned_can_opt = commit_salvaged_wreck_inventory_and_notify(
        ctx,
        sys_id,
        char_id,
        ship_id,
        wreck_id,
        wreck_pos,
        &salvage_table,
    )
    .map_err(|err| match err {
        CommitSalvageError::Capacity {
            ship_cap,
            used_cargo,
            salvage_vol,
        } => crate::services::inventory_fitting::ship_hold_capacity_error(
            InventoryFlag::Cargo as u16,
            ship_cap,
            used_cargo,
            salvage_vol,
        ),
        CommitSalvageError::WreckMissing => target_not_in_space_error(format!(
            "Target wreck {wreck_id} no longer exists in solar system {sys_id}"
        )),
        CommitSalvageError::Wal(message) => {
            <eve_proto::BridgeRpcError as eve_db::FromWalError>::from_wal_error(message)
        }
    })?;

    let cycle_pre_removed = finalize_salvaged_wreck_ball(
        ctx,
        &sys_handle,
        sys_id,
        char_id,
        ship_id,
        wreck_id,
        wreck_pos,
        spawned_can_opt,
        active_module_id_opt,
    )
    .await;

    Ok((
        PyRep::keyval([
            ("wreckID", PyRep::Integer(wreck_id)),
            ("wreckTypeID", PyRep::Integer(wreck_type_id as i64)),
            ("wreckDifficultyBonus", PyRep::Real(wreck_difficulty_bonus)),
            ("accessChancePct", PyRep::Real(access_chance_pct)),
            ("salvaged", PyRep::Bool(true)),
            (
                "salvageTypeIDs",
                PyRep::List(
                    yielded_type_ids
                        .into_iter()
                        .map(|t| PyRep::Integer(t as i64))
                        .collect(),
                ),
            ),
        ]),
        cycle_pre_removed,
    ))
}

/// Auto-repeating Salvager module (`effectSalvaging`) / Salvage Drone (`CmdSalvage`) cycle execution:
/// executes a single salvage cycle with capacitor check/consumption and, on `CycleRollFailed`,
/// schedules the next cycle attempt through the `SystemActor` `auto_repeat` module cycle timer
/// instead of running synchronous multi-cycle rolls in one call.
pub async fn execute_wreck_salvage_auto_repeat(
    ctx: &ServerContext,
    sys_id: u32,
    char_id: i64,
    ship_id: i64,
    wreck_id: i64,
    salvager_type_id_opt: Option<u32>,
) -> Result<eve_proto::PyRep, eve_proto::BridgeRpcError> {
    execute_wreck_salvage_cycle_scheduled(
        ctx,
        sys_id,
        char_id,
        ship_id,
        wreck_id,
        None,
        salvager_type_id_opt,
        true,
    )
    .await
}

/// Executes one timed salvage attempt (checking & deducting module capacitor) and optionally
/// queues the next `ActiveModuleCycle` (`auto_repeat`) when `reason == "CycleRollFailed"`.
#[allow(clippy::too_many_arguments)]
pub async fn execute_wreck_salvage_cycle_scheduled(
    ctx: &ServerContext,
    sys_id: u32,
    char_id: i64,
    ship_id: i64,
    wreck_id: i64,
    module_item_id_opt: Option<i64>,
    salvager_type_id_opt: Option<u32>,
    schedule_repeat_on_fail: bool,
) -> Result<eve_proto::PyRep, eve_proto::BridgeRpcError> {
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let mod_type_id = salvager_type_id_opt.unwrap_or(32787);
    let mod_sde = ctx.sde.types.get(&mod_type_id);
    let eval_state_opt = ctx
        .get_or_rebuild_ship_fitting(char_id, ship_id)
        .ok()
        .and_then(|fit| {
            module_item_id_opt
                .and_then(|mid| fit.module_state(mid, &ctx.sde))
                .or_else(|| {
                    fit.evaluated_modules
                        .values()
                        .find(|ms| ms.type_id == mod_type_id)
                        .cloned()
                })
                .or_else(|| {
                    fit.evaluated_drones
                        .values()
                        .find(|ds| ds.type_id == mod_type_id)
                        .cloned()
                })
        });
    let cap_cost = eval_state_opt
        .as_ref()
        .filter(|st| {
            st.base_attributes
                .contains_key(&eve_sde::attr_ids::CAPACITOR_NEED)
                || st
                    .modifiers
                    .iter()
                    .any(|m| m.target_attribute_id == eve_sde::attr_ids::CAPACITOR_NEED)
        })
        .map(|st| st.get_attribute(eve_sde::attr_ids::CAPACITOR_NEED, &ctx.sde))
        .or_else(|| {
            mod_sde.and_then(|t| {
                t.default_attributes
                    .get(&eve_sde::attr_ids::CAPACITOR_NEED)
                    .copied()
            })
        })
        .unwrap_or(0.0)
        .max(0.0);
    let duration_s = (eval_state_opt
        .as_ref()
        .map(|st| st.get_attribute(73, &ctx.sde))
        .filter(|&v| v > 0.0)
        .or_else(|| mod_sde.and_then(|t| t.default_attributes.get(&73).copied()))
        .unwrap_or(10_000.0)
        / 1_000.0)
        .max(1.0);

    if cap_cost > 0.0 {
        if let Ok(Some(cs)) = sys_handle.get_combat_state(ship_id).await {
            if cs.capacitor + 1e-6 < cap_cost {
                return Err(eve_proto::BridgeRpcError {
                    code: "NotEnoughCapacitor".into(),
                    message: format!(
                        "Ship {ship_id} lacks capacitor ({:.1}/{:.1} GJ) to cycle salvager",
                        cs.capacitor, cap_cost
                    ),
                    client_error: None,
                });
            }
            let _ = sys_handle
                .set_capacitor(ship_id, (cs.capacitor - cap_cost).max(0.0))
                .await;
        }
    }

    let rep =
        execute_wreck_salvage_cycle(ctx, sys_id, char_id, ship_id, wreck_id, salvager_type_id_opt)
            .await?;

    let is_roll_failed = rep.dict_get("salvaged").and_then(|v| v.as_bool()) == Some(false)
        && rep.dict_get("reason").and_then(|v| v.as_str()) == Some("CycleRollFailed");
    if schedule_repeat_on_fail && is_roll_failed {
        let resolved_mod_id = module_item_id_opt.unwrap_or(wreck_id);
        if !sys_handle.has_active_module_cycle_sync(ship_id, resolved_mod_id) {
            let _ = sys_handle
                .add_module_cycle(crate::system_actor::ActiveModuleCycle {
                    ship_id,
                    module_id: resolved_mod_id,
                    module_type_id: mod_type_id,
                    owner_char_id: Some(char_id),
                    effect_name: "effectSalvaging".to_string(),
                    target_id: Some(wreck_id),
                    repeat: 1000,
                    duration_s,
                    elapsed_s: 0.0,
                    cap_need_per_cycle: cap_cost,
                    ..Default::default()
                })
                .await;
        }
    }

    Ok(rep)
}

/// Simulates a tractor beam pull on a wreck towards the player ship:
/// - Maximum tractor range: 20,000 m.
/// - Tractor pull speed: 500 m/s.
/// - Stop distance: 1,200 m (so ship can easily loot/salvage).
/// Returns `(new_wreck_position, has_arrived)`.
pub fn step_tractor_beam(
    wreck_pos: DVec3,
    ship_pos: DVec3,
    tractor_velocity: f64,
    dt_sec: f64,
) -> (DVec3, bool) {
    let delta = ship_pos - wreck_pos;
    let dist = delta.length();
    let stop_distance = 1_200.0;

    if dist <= stop_distance {
        return (wreck_pos, true);
    }

    let dir = delta.normalize_or_zero();
    let travel = (tractor_velocity * dt_sec).min(dist - stop_distance);
    let new_pos = wreck_pos + dir * travel;
    let arrived = (ship_pos - new_pos).length() <= stop_distance + 1.0;
    (new_pos, arrived)
}

/// Validates combat drone bandwidth requirements against a ship's Dogma bandwidth limit.
/// Max 5 active combat drones, total bandwidth <= ship `droneBandwidth` attribute.
pub fn validate_drone_bandwidth(
    ctx: &ServerContext,
    ship_id: i64,
    requested_drone_type_ids: &[u32],
    currently_active_drone_type_ids: &[u32],
) -> Result<f64, String> {
    let total_count = requested_drone_type_ids.len() + currently_active_drone_type_ids.len();
    if total_count > 5 {
        return Err(format!(
            "Cannot control more than 5 active drones simultaneously (requested {total_count})"
        ));
    }

    let (ship_type_id, owner_id) = ctx
        .db
        .with_snapshot(|snap| snap.items.get(&ship_id).map(|s| (s.type_id, s.owner_id)))
        .ok_or_else(|| format!("Ship {ship_id} not found"))?;

    let sde_default_bw = ctx
        .sde
        .types
        .get(&ship_type_id)
        .and_then(|t| t.default_attributes.get(&eve_sde::attr_ids::DRONE_BANDWIDTH).copied());
    let fit_opt = if owner_id > 0 {
        let mut fit = ctx.get_or_rebuild_ship_fitting(owner_id, ship_id).ok();
        if let Some(ref f) = fit {
            if f.ship
                .base_attributes
                .get(&eve_sde::attr_ids::DRONE_BANDWIDTH)
                .copied()
                != sde_default_bw
            {
                ctx.invalidate_ship_fitting(ship_id);
                fit = ctx.get_or_rebuild_ship_fitting(owner_id, ship_id).ok();
            }
        }
        fit
    } else {
        None
    };

    let ship_bandwidth = if let Some(ref fit) = fit_opt {
        let has_bw = fit
            .ship
            .base_attributes
            .contains_key(&eve_sde::attr_ids::DRONE_BANDWIDTH)
            || fit
                .ship
                .modifiers
                .iter()
                .any(|m| m.target_attribute_id == eve_sde::attr_ids::DRONE_BANDWIDTH);
        if has_bw {
            fit.ship
                .get_attribute(eve_sde::attr_ids::DRONE_BANDWIDTH, &ctx.sde)
        } else {
            sde_default_bw.unwrap_or(25.0)
        }
    } else {
        sde_default_bw.unwrap_or(25.0)
    };

    let mut required_bandwidth = 0.0;
    for &dtid in currently_active_drone_type_ids.iter().chain(requested_drone_type_ids) {
        let drone_bw = fit_opt
            .as_ref()
            .and_then(|fit| {
                fit.evaluated_drones
                    .values()
                    .find(|d| d.type_id == dtid)
                    .filter(|d| {
                        d.base_attributes
                            .contains_key(&eve_sde::attr_ids::DRONE_BANDWIDTH_USED)
                    })
                    .map(|d| d.get_attribute(eve_sde::attr_ids::DRONE_BANDWIDTH_USED, &ctx.sde))
            })
            .or_else(|| {
                ctx.sde.types.get(&dtid).and_then(|t| {
                    t.default_attributes
                        .get(&eve_sde::attr_ids::DRONE_BANDWIDTH_USED)
                        .copied()
                })
            })
            .unwrap_or(5.0); // Light combat drones default to 5 Mbit/s
        required_bandwidth += drone_bw;
    }

    if required_bandwidth > ship_bandwidth + 1e-4 {
        return Err(format!(
            "Insufficient drone bandwidth: required {:.1} Mbit/s, ship maximum is {:.1} Mbit/s",
            required_bandwidth, ship_bandwidth
        ));
    }

    Ok(ship_bandwidth - required_bandwidth)
}

/// State of an active combat drone in space.
#[derive(Debug, Clone)]
pub struct ActiveDrone {
    pub ball_id: i64,
    pub drone_item_id: i64,
    pub type_id: u32,
    pub owner_char_id: i64,
    pub parent_ship_id: i64,
    pub target_id: Option<i64>,
    pub dps: f64,
}

/// Drone Controller managing launch, attack orders, and recall into DroneBay.
#[derive(Debug, Default)]
pub struct DroneController {
    pub active_drones: HashMap<i64, ActiveDrone>,
}

impl DroneController {
    pub fn new() -> Self {
        Self::default()
    }

    /// Launches specified drone items from ship's `DroneBay` into space balls.
    pub async fn launch_drones(
        &mut self,
        ctx: &ServerContext,
        sys_id: u32,
        char_id: i64,
        ship_id: i64,
        drone_item_ids: &[i64],
    ) -> Result<Vec<i64>, String> {
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        let ship_pos = sys_handle
            .get_ball_snapshot(ship_id)
            .await
            .map_err(|e| format!("Query ship failed: {e}"))?
            .map(|(_, b)| b.position)
            .unwrap_or(DVec3::ZERO);

        let active_types: Vec<u32> = self.active_drones.values().map(|d| d.type_id).collect();

        let drone_items: Vec<(i64, u32)> = ctx.db.with_snapshot(|snap| {
            let mut list = Vec::new();
            for &did in drone_item_ids {
                let item = snap
                    .items
                    .get(&did)
                    .ok_or_else(|| format!("Drone item {did} not found in inventory"))?;
                if item.owner_id != char_id || item.location_id != ship_id {
                    return Err(format!("Drone item {did} is not in ship's drone bay"));
                }
                list.push((did, item.type_id));
            }
            Ok::<_, String>(list)
        })?;

        let req_types: Vec<u32> = drone_items.iter().map(|&(_, tid)| tid).collect();
        validate_drone_bandwidth(ctx, ship_id, &req_types, &active_types)?;

        let mut fit_opt = ctx.get_or_rebuild_ship_fitting(char_id, ship_id).ok();
        let mut spawned_ball_ids = Vec::new();

        for (idx, (item_id, type_id)) in drone_items.into_iter().enumerate() {
            if let Some(ref mut f) = fit_opt {
                if !f.drones.iter().any(|&(id, _)| id == item_id) {
                    f.add_drone(item_id, type_id, &ctx.sde);
                }
            }
            let d_state_opt = fit_opt
                .as_ref()
                .and_then(|f| f.drone_state(item_id, &ctx.sde));
            let max_vel = d_state_opt
                .as_ref()
                .map(|ds| ds.get_attribute(37, &ctx.sde))
                .filter(|&v| v > 0.0)
                .unwrap_or(800.0);
            let eval_dps = d_state_opt
                .as_ref()
                .and_then(|ds| {
                    let raw = ds.get_attribute(114, &ctx.sde)
                        + ds.get_attribute(116, &ctx.sde)
                        + ds.get_attribute(117, &ctx.sde)
                        + ds.get_attribute(118, &ctx.sde);
                    let mult = ds.get_attribute(64, &ctx.sde).max(1.0);
                    let rof_s = (ds.get_attribute(51, &ctx.sde) / 1_000.0).max(1.0);
                    (raw > 0.0).then_some((raw * mult) / rof_s)
                })
                .unwrap_or(18.0);

            let ball_id = ctx.db.with_mut_snapshot(|snap| {
                snap.next_entity_id += 1;
                let bid = snap.next_entity_id;
                // Move item location to space
                if let Some(i) = snap.items.get_mut(&item_id) {
                    i.location_id = sys_id as i64;
                    i.flag = InventoryFlag::None;
                    i.version += 1;
                }
                bid
            });

            let offset = DVec3::new(
                ((idx as f64) * 100.0).cos() * 50.0,
                0.0,
                ((idx as f64) * 100.0).sin() * 50.0,
            );
            let d_pos = ship_pos + offset;

            let ball = Ball {
                id: ball_id,
                type_id,
                position: d_pos,
                velocity: DVec3::ZERO,
                max_velocity: max_vel,
                inertia: 0.5,
                mass: 10_000.0,
                radius: 10.0,
                speed_fraction: 1.0,
                mode: MovementMode::Stop,
            };

            let (pilot_corp_id, pilot_alliance_id) = ctx
                .db
                .get_character(char_id)
                .map(|ch| (ch.corporation_id, ch.alliance_id.unwrap_or(-1)))
                .unwrap_or((-1, -1));
            let sde_t = ctx.sde.types.get(&type_id);
            let drone_meta = eve_destiny::SlimItemMeta {
                group_id: sde_t.map(|t| t.group_id).unwrap_or(100),
                category_id: sde_t.map(|t| t.category_id).unwrap_or(18),
                owner_id: char_id,
                char_id: None,
                corp_id: pilot_corp_id,
                alliance_id: pilot_alliance_id,
                security_status: 0.0,
                is_cloaked: false,
                name: sde_t.map(|t| t.name.clone()).filter(|n| !n.is_empty()),
                ..Default::default()
            };
            let _ = sys_handle
                .spawn_ball_with_meta(ball, ctx.build_type_combat_state(type_id), drone_meta)
                .await;

            let active = ActiveDrone {
                ball_id,
                drone_item_id: item_id,
                type_id,
                owner_char_id: char_id,
                parent_ship_id: ship_id,
                target_id: None,
                dps: eval_dps,
            };

            self.active_drones.insert(ball_id, active);
            spawned_ball_ids.push(ball_id);
        }

        Ok(spawned_ball_ids)
    }

    /// Orders active drones to engage a hostile target.
    pub async fn engage_target(
        &mut self,
        ctx: &ServerContext,
        sys_id: u32,
        target_ball_id: i64,
    ) -> Result<usize, String> {
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        let mut count = 0;

        for drone in self.active_drones.values_mut() {
            drone.target_id = Some(target_ball_id);
            let _ = sys_handle
                .set_movement_mode(
                    drone.ball_id,
                    MovementMode::Orbit {
                        target_id: target_ball_id,
                        range: 1_000.0,
                    },
                )
                .await;

            // Apply drone damage
            let quarter = drone.dps * 0.25;
            let dmg = DamageVector {
                em: quarter,
                thermal: quarter,
                kinetic: quarter,
                explosive: quarter,
            };
            let _ = sys_handle.apply_damage(target_ball_id, dmg).await;
            count += 1;
        }

        Ok(count)
    }

    /// Recalls active drones into the ship's `DroneBay`.
    pub async fn recall_drones(
        &mut self,
        ctx: &ServerContext,
        sys_id: u32,
        ship_id: i64,
    ) -> Result<Vec<i64>, String> {
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        let mut recalled = Vec::new();

        let drone_entries: Vec<ActiveDrone> = self
            .active_drones
            .values()
            .filter(|d| d.parent_ship_id == ship_id)
            .cloned()
            .collect();

        for drone in drone_entries {
            self.active_drones.remove(&drone.ball_id);
            let _ = sys_handle.remove_ball(drone.ball_id).await;

            ctx.db.with_mut_snapshot(|snap| {
                if let Some(item) = snap.items.get_mut(&drone.drone_item_id) {
                    item.location_id = ship_id;
                    item.flag = InventoryFlag::DroneBay;
                    item.version += 1;
                }
            });

            recalled.push(drone.drone_item_id);
        }

        Ok(recalled)
    }

    /// Orders active combat drones to return and orbit their parent ship (`feat:drones.return-orbit`).
    pub async fn return_and_orbit(
        &mut self,
        ctx: &ServerContext,
        sys_id: u32,
        ship_id: i64,
    ) -> Result<usize, String> {
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        let mut count = 0;

        for drone in self.active_drones.values_mut() {
            if drone.parent_ship_id == ship_id {
                drone.target_id = None;
                let _ = sys_handle
                    .set_movement_mode(
                        drone.ball_id,
                        MovementMode::Orbit {
                            target_id: ship_id,
                            range: 1_000.0,
                        },
                    )
                    .await;
                count += 1;
            }
        }

        Ok(count)
    }
}

#[cfg(test)] #[path = "drones_salvage_tractor_unit_tests.rs"]
mod drones_salvage_tractor_unit_tests;

#[cfg(test)]
#[path = "drones_salvage_tractor_edges_tests.rs"]
mod drones_salvage_tractor_edges_tests;

