//! Protocol-agnostic combat health restoration, module damage clearing, and entity destruction
//! domain operations shared across gameplay services and GM `/heal` + `/kill` commands (`#1193`).
//!
//! All solar-system lookups use non-waking [`crate::system_actor::SolarSystemRegistry::get_handle`]
//! (`#1542`) and delegate to canonical SP6, ballpark, NPC AI, and death-pipeline primitives.

#![forbid(unsafe_code)]

use crate::combat::npc_ai::{
    spawn_npc_wreck_and_loot_in_snap, DestroyedNpcOutcome, NpcDestroyedOptions,
};
use crate::domain::DomainError;
use crate::router::notify_registry::BallparkScene;
use crate::router::{NotificationTarget, ServerContext};
use crate::system_actor::{AsteroidState, SystemHandle};
use eve_db::{
    AsteroidRecord, InventoryItemRecord, KillmailRecord, MintItemSpec, WorldStateSnapshot,
};
use eve_destiny::{Ball, CombatState, DestinyEvent, MovementMode, SlimItemMeta};
use glam::DVec3;
use std::collections::{HashMap, HashSet};

/// Target parameters for [`restore_ship_vital_state`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RestoreShipVitalsRequest {
    pub target_char_id: i64,
    pub target_ship_id: i64,
    pub ship_type_id: u32,
    pub sys_id: u32,
}

/// Restored ship vitals returned by [`restore_ship_vital_state`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RestoreShipVitalsOutcome {
    pub character_id: i64,
    pub ship_id: i64,
    pub shield_hp: f64,
    pub armor_hp: f64,
    pub hull_hp: f64,
    pub capacitor: f64,
}

/// Target specification for [`destroy_entity_in_system`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DestroyEntityRequest {
    pub target_id: i64,
    pub victim_char_id: i64,
    pub victim_ship_id: i64,
    pub sys_id: u32,
    pub killer_char_id: i64,
    pub in_db_items: bool,
}

/// Entity destruction outcome returned by [`destroy_entity_in_system`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DestroyEntityOutcome {
    pub target_id: i64,
    pub wreck_id: i64,
    pub killmail_id: i64,
    pub ejected_capsule_id: Option<i64>,
}

/// Ballpark payload for [`spawn_entity_in_ballpark`].
#[derive(Debug, Clone)]
pub enum SpawnBallPayload {
    /// Dynamic or static ball carrying explicit `CombatState` and `SlimItemMeta`.
    Meta {
        combat_state: CombatState,
        slim_meta: SlimItemMeta,
    },
    /// Harvestable asteroid ball with optional persisted `AsteroidRecord`.
    Asteroid {
        asteroid: AsteroidState,
        record: Option<AsteroidRecord>,
    },
}

/// Request specification for [`spawn_entity_in_ballpark`].
#[derive(Debug, Clone)]
pub struct SpawnEntityInBallparkRequest {
    pub entity_id: Option<i64>,
    pub db_item: Option<InventoryItemRecord>,
    pub ball: Ball,
    pub payload: SpawnBallPayload,
    pub mark_non_interactive: bool,
}

/// Request specification for [`despawn_entity_in_ballpark`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DespawnEntityInBallparkRequest {
    pub entity_id: i64,
    pub remove_from_db: bool,
    pub remove_warp_bubble: bool,
    pub clean_npc_runtime_system: Option<u32>,
}

/// Cleanup summary returned by [`despawn_entity_in_ballpark`].
#[derive(Debug, Clone)]
pub struct DespawnEntityInBallparkOutcome {
    pub entity_id: i64,
    pub removed_from_db: bool,
    pub removed_children: usize,
    pub removed_ball: Option<Ball>,
}

/// Request specification for [`spawn_wreck_with_loot_in_ballpark`].
#[derive(Debug, Clone)]
pub struct SpawnWreckWithLootRequest {
    pub solar_system_id: u32,
    pub owner_char_id: i64,
    pub pos: DVec3,
    pub now_unix: u64,
    pub wreck_type_id: u32,
    pub loot_entries: Vec<(u32, i32, bool)>,
    pub name_override: Option<String>,
}

/// Persisted loot item metadata returned by [`spawn_wreck_with_loot_in_ballpark`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpawnedWreckLootItem {
    pub item_id: i64,
    pub type_id: u32,
    pub quantity: i32,
}

/// Outcome returned by [`spawn_wreck_with_loot_in_ballpark`].
#[derive(Debug, Clone)]
pub struct SpawnWreckWithLootOutcome {
    pub wreck_id: i64,
    pub loot_items: Vec<SpawnedWreckLootItem>,
}

/// Atomically inserts any persisted DB space item/asteroid records and spawns the corresponding
/// ballpark ball on `handle`, rolling back DB records if the ballpark command fails (`#1193`).
pub async fn spawn_entity_in_ballpark(
    ctx: &ServerContext,
    handle: &SystemHandle,
    mut req: SpawnEntityInBallparkRequest,
) -> Result<i64, DomainError> {
    let needs_db_write = req.db_item.is_some()
        || matches!(
            &req.payload,
            SpawnBallPayload::Asteroid {
                record: Some(_),
                ..
            }
        );
    let entity_id = if needs_db_write || req.entity_id.is_none() {
        ctx.db.with_mut_snapshot(|snap| {
            let id = if let Some(item) = req.db_item.take() {
                snap.mint_inventory_item_with_id_in_snap(
                    req.entity_id,
                    MintItemSpec {
                        type_id: item.type_id,
                        owner_id: item.owner_id,
                        location_id: item.location_id,
                        flag: item.flag,
                        quantity: item.quantity,
                        singleton: item.singleton,
                        is_blueprint_copy: item.is_blueprint_copy,
                        blueprint_runs_remaining: item.blueprint_runs_remaining,
                        merge_existing_stack: false,
                    },
                )
                .map(|minted| minted.item_id)
                .map_err(|err| DomainError::invalid_input("SpawnFailed", err.to_string()))?
            } else {
                req.entity_id.unwrap_or_else(|| snap.alloc_entity_id())
            };
            if let SpawnBallPayload::Asteroid {
                ref mut record, ..
            } = req.payload
            {
                if let Some(mut ast_rec) = record.take() {
                    ast_rec.asteroid_id = id;
                    snap.asteroids.insert(id, ast_rec);
                }
            }
            Ok::<i64, DomainError>(id)
        })?
    } else {
        req.entity_id.unwrap_or(0)
    };
    if entity_id <= 0 {
        return Err(DomainError::invalid_input(
            "InvalidEntityId",
            "Spawned entity ID must be positive",
        ));
    }

    req.ball.id = entity_id;
    let spawn_res = match req.payload {
        SpawnBallPayload::Asteroid { mut asteroid, .. } => {
            asteroid.asteroid_id = entity_id;
            handle.spawn_asteroid(asteroid, req.ball).await.map(|_| ())
        }
        SpawnBallPayload::Meta {
            combat_state,
            slim_meta,
        } => handle
            .spawn_ball_with_meta(req.ball, combat_state, slim_meta)
            .await
            .map(|_| ()),
    };
    if let Err(err) = spawn_res {
        if needs_db_write {
            ctx.db.with_mut_snapshot(|snap| {
                snap.remove_system_cleanup_item(entity_id);
                snap.asteroids.remove(&entity_id);
            });
        }
        return Err(DomainError::internal("SpawnFailed", err));
    }

    if req.mark_non_interactive {
        let _ = handle
            .emit_destiny_event(
                entity_id,
                DestinyEvent::SetBallInteractive {
                    ball_id: entity_id,
                    is_interactive: false,
                },
            )
            .await;
    }

    Ok(entity_id)
}

/// Removes `entity_ids` from `WorldRepository` (including child items, asteroid records, positions,
/// and SP6 space metadata when `remove_from_db` is true), from live `handle` ballpark/warp bubbles,
/// and from optional NPC/container runtime caches (`#1193`).
pub async fn despawn_entities_in_ballpark(
    ctx: &ServerContext,
    handle: Option<&SystemHandle>,
    entity_ids: &[i64],
    remove_from_db: bool,
    remove_warp_bubble: bool,
    clean_npc_runtime_system: Option<u32>,
) -> Vec<DespawnEntityInBallparkOutcome> {
    if entity_ids.is_empty() {
        return Vec::new();
    }

    let db_results: Vec<(bool, usize)> = if remove_from_db {
        let id_set: HashSet<i64> = entity_ids.iter().copied().collect();
        ctx.db.with_mut_snapshot(|snap| {
            let mut children_by_parent: HashMap<i64, Vec<i64>> = HashMap::new();
            for item in snap.items.values() {
                if id_set.contains(&item.location_id) {
                    children_by_parent
                        .entry(item.location_id)
                        .or_default()
                        .push(item.item_id);
                }
            }
            entity_ids
                .iter()
                .map(|&id| {
                    let item_removed = snap.remove_system_cleanup_item(id).is_some();
                    let ast_removed = snap.asteroids.remove(&id).is_some();
                    snap.sp6_lifecycle.space_object_meta.remove(&id);
                    snap.sp6_lifecycle.ship_combat_hp.remove(&id);
                    let mut child_count = 0usize;
                    if let Some(children) = children_by_parent.remove(&id) {
                        for cid in children {
                            if snap.remove_system_cleanup_item(cid).is_some() {
                                child_count += 1;
                            }
                        }
                    }
                    (item_removed || ast_removed, child_count)
                })
                .collect()
        })
    } else {
        vec![(false, 0); entity_ids.len()]
    };

    if let Some(sys_id) = clean_npc_runtime_system {
        if let Ok(mut map) = ctx.npc_spawns.write() {
            if let Some(mgr) = map.get_mut(&sys_id) {
                for &id in entity_ids {
                    mgr.entities.remove(&id);
                }
            }
        }
    }
    if clean_npc_runtime_system.is_some() || remove_from_db {
        crate::services::combat_crimewatch::with_mut_combat_runtime(ctx, |rt| {
            for &id in entity_ids {
                if clean_npc_runtime_system.is_some() {
                    rt.npc_entities.remove(&id);
                    rt.destroyed_npc_cache.remove(&id);
                }
                if remove_from_db {
                    rt.container_lifetimes.remove(&id);
                }
            }
        });
    }

    let mut outcomes = Vec::with_capacity(entity_ids.len());
    for (&entity_id, (removed_from_db, removed_children)) in
        entity_ids.iter().zip(db_results.into_iter())
    {
        let removed_ball = if let Some(h) = handle {
            if remove_warp_bubble {
                let _ = h.remove_warp_bubble(entity_id).await;
            }
            h.remove_ball(entity_id).await
        } else {
            None
        };
        outcomes.push(DespawnEntityInBallparkOutcome {
            entity_id,
            removed_from_db,
            removed_children,
            removed_ball,
        });
    }
    outcomes
}

/// Removes a single entity from `WorldRepository`, `handle`, and runtime caches (`#1193`).
pub async fn despawn_entity_in_ballpark(
    ctx: &ServerContext,
    handle: Option<&SystemHandle>,
    req: DespawnEntityInBallparkRequest,
) -> DespawnEntityInBallparkOutcome {
    despawn_entities_in_ballpark(
        ctx,
        handle,
        &[req.entity_id],
        req.remove_from_db,
        req.remove_warp_bubble,
        req.clean_npc_runtime_system,
    )
    .await
    .into_iter()
    .next()
    .unwrap_or(DespawnEntityInBallparkOutcome {
        entity_id: req.entity_id,
        removed_from_db: false,
        removed_children: 0,
        removed_ball: None,
    })
}

pub(crate) async fn spawn_wreck_ball_in_ballpark(
    ctx: &ServerContext,
    handle: &SystemHandle,
    wreck_id: i64,
    wreck_type_id: u32,
    owner_id: i64,
    pos: DVec3,
    name_override: Option<String>,
) -> Result<(), DomainError> {
    let wreck_sde = ctx.sde.types.get(&wreck_type_id);
    let name = name_override
        .filter(|n| !n.is_empty())
        .or_else(|| wreck_sde.map(|t| t.name.clone()).filter(|n| !n.is_empty()));
    handle
        .spawn_ball_with_meta(
            Ball {
                id: wreck_id,
                type_id: wreck_type_id,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 50_000.0,
                radius: 30.0,
                speed_fraction: 0.0,
                mode: MovementMode::Rigid,
            },
            ctx.build_type_combat_state(wreck_type_id),
            SlimItemMeta {
                group_id: wreck_sde.map(|t| t.group_id).unwrap_or(186),
                category_id: wreck_sde.map(|t| t.category_id).unwrap_or(2),
                owner_id,
                char_id: None,
                corp_id: -1,
                alliance_id: -1,
                security_status: 0.0,
                is_cloaked: false,
                name,
                ..Default::default()
            },
        )
        .await
        .map(|_| ())
        .map_err(|err| DomainError::internal("SpawnFailed", err))
}

/// Inserts a wreck and its loot items in `WorldRepository` via
/// [`spawn_npc_wreck_and_loot_in_snap`] and spawns the matching rigid ballpark ball (`#1193`),
/// rolling back persisted DB rows if the ballpark spawn fails.
pub async fn spawn_wreck_with_loot_in_ballpark(
    ctx: &ServerContext,
    handle: &SystemHandle,
    req: SpawnWreckWithLootRequest,
) -> Result<SpawnWreckWithLootOutcome, DomainError> {
    let (wreck_id, mut loot_items) = ctx.db.with_mut_snapshot(|snap| {
        let wid = spawn_npc_wreck_and_loot_in_snap(
            snap,
            req.solar_system_id,
            req.owner_char_id,
            req.pos,
            req.now_unix,
            req.wreck_type_id,
            &req.loot_entries,
        );
        let items: Vec<SpawnedWreckLootItem> = snap
            .items
            .values()
            .filter(|it| it.location_id == wid)
            .map(|it| SpawnedWreckLootItem {
                item_id: it.item_id,
                type_id: it.type_id,
                quantity: it.quantity,
            })
            .collect();
        (wid, items)
    });
    loot_items.sort_by_key(|it| it.item_id);

    if let Err(err) = spawn_wreck_ball_in_ballpark(
        ctx,
        handle,
        wreck_id,
        req.wreck_type_id,
        req.owner_char_id,
        req.pos,
        req.name_override,
    )
    .await
    {
        ctx.db.with_mut_snapshot(|snap| {
            snap.remove_system_cleanup_item(wreck_id);
            snap.sp6_lifecycle.space_object_meta.remove(&wreck_id);
            for loot in &loot_items {
                snap.remove_system_cleanup_item(loot.item_id);
            }
        });
        return Err(err);
    }

    Ok(SpawnWreckWithLootOutcome {
        wreck_id,
        loot_items,
    })
}

/// Clears persisted ship hull/armor/shield/capacitor damage (`clear_ship_combat_damage_in_snap`)
/// and, when `clear_children` is true, also clears module/heat damage (`DAMAGE`, `ARMOR_DAMAGE`,
/// heat `1211`) on the ship and its fitted child items in `snap`.
pub fn clear_ship_and_fitted_damage_in_snap(
    snap: &mut WorldStateSnapshot,
    ship_id: i64,
    clear_children: bool,
) {
    if ship_id <= 0 {
        return;
    }
    crate::sp6_lifecycle::clear_ship_combat_damage_in_snap(snap, ship_id);
    if clear_children {
        for item in snap.items.values_mut() {
            if item.item_id == ship_id || item.location_id == ship_id {
                let r1 = item
                    .dynamic_attributes
                    .remove(&eve_sde::attr_ids::DAMAGE)
                    .is_some();
                let r2 = item
                    .dynamic_attributes
                    .remove(&eve_sde::attr_ids::ARMOR_DAMAGE)
                    .is_some();
                let r3 = item.dynamic_attributes.remove(&1211u16).is_some();
                if (r1 || r2 || r3) && item.item_id != ship_id {
                    item.version += 1;
                }
            }
        }
    }
}

/// Clears cached module `damage_hp` entries inside `ctx.ship_fittings` for `item_ids`.
pub fn clear_fitted_module_damage_in_cache(ctx: &ServerContext, item_ids: &[i64]) {
    if item_ids.is_empty() {
        return;
    }
    if let Ok(mut fits) = ctx.ship_fittings.write() {
        for fit in fits.values_mut() {
            for m in fit.modules.values_mut() {
                if item_ids.contains(&m.item_id) {
                    m.damage_hp = 0.0;
                }
            }
        }
    }
}

fn resolve_ship_vital_capacities(
    ctx: &ServerContext,
    ship_id: i64,
    ship_type_id: u32,
    fit_opt: Option<&eve_dogma::ShipFittingEngine>,
) -> (f64, f64, f64, f64, f64, f64) {
    let pick = |attrs: &HashMap<u16, f64>| {
        let g = |aid| attrs.get(&aid).copied().unwrap_or(0.0);
        (
            g(eve_sde::attr_ids::HP_STRUCT),
            g(eve_sde::attr_ids::ARMOR_HP),
            g(eve_sde::attr_ids::SHIELD_CAPACITY),
            g(eve_sde::attr_ids::CAPACITOR_CAPACITY),
            g(eve_sde::attr_ids::RECHARGE_RATE),
            g(eve_sde::attr_ids::SHIELD_RECHARGE_RATE),
        )
    };
    if let Some(fit) = fit_opt {
        let mut eff_attrs: HashMap<u16, f64> = [
            eve_sde::attr_ids::HP_STRUCT,
            eve_sde::attr_ids::ARMOR_HP,
            eve_sde::attr_ids::SHIELD_CAPACITY,
            eve_sde::attr_ids::CAPACITOR_CAPACITY,
            eve_sde::attr_ids::RECHARGE_RATE,
            eve_sde::attr_ids::SHIELD_RECHARGE_RATE,
        ]
        .into_iter()
        .map(|aid| (aid, fit.ship.get_attribute(aid, &ctx.sde)))
        .collect();
        crate::services::inventory_fitting::fitting_scanners_superweapons::apply_active_dbuffs_and_system_effects_to_ship_attrs(
            ctx,
            ship_id,
            &mut eff_attrs,
        );
        pick(&eff_attrs)
    } else if let Some(t) = ctx.sde.types.get(&ship_type_id) {
        pick(&t.default_attributes)
    } else {
        (0.0, 0.0, 0.0, 0.0, 0.0, 0.0)
    }
}

/// Restores ship shield, armor, hull, capacitor, module damage, and rack heat across live ballpark
/// state (only when the solar system is already loaded — never waking cold systems `#1542`),
/// SP6/DB damage attributes, fitting cache, and Destiny/Godma notifications.
pub async fn restore_ship_vital_state(
    ctx: &ServerContext,
    req: RestoreShipVitalsRequest,
) -> Result<RestoreShipVitalsOutcome, DomainError> {
    let (
        fit_max_hull,
        fit_max_armor,
        fit_max_shield,
        fit_max_cap,
        fit_cap_recharge_ms,
        fit_shield_recharge_ms,
        has_fit,
        pre_rack_heat,
        cached_damaged_mods,
    ) = {
        let fit_opt = (req.target_ship_id > 0 && req.target_char_id > 0)
            .then(|| ctx.fit(req.target_char_id, req.target_ship_id).ok())
            .flatten();
        let caps =
            resolve_ship_vital_capacities(ctx, req.target_ship_id, req.ship_type_id, fit_opt.as_ref());
        let has_fit = fit_opt.is_some();
        let pre_rack_heat = fit_opt
            .as_ref()
            .map(|f| f.rack_heat)
            .unwrap_or([0.0, 0.0, 0.0]);
        let cached_damaged_mods: Vec<(i64, f64)> = fit_opt
            .as_ref()
            .map(|f| {
                f.modules
                    .values()
                    .filter(|m| m.damage_hp > 0.0)
                    .map(|m| (m.item_id, m.damage_hp))
                    .collect()
            })
            .unwrap_or_default();
        (caps.0, caps.1, caps.2, caps.3, caps.4, caps.5, has_fit, pre_rack_heat, cached_damaged_mods)
    };

    let handle_opt = (req.target_ship_id > 0 && req.sys_id > 0)
        .then(|| ctx.systems.get_handle(req.sys_id))
        .flatten();
    let pre_cs = if let Some(ref handle) = handle_opt {
        handle
            .get_combat_state(req.target_ship_id)
            .await
            .ok()
            .flatten()
    } else {
        None
    };

    let pick_max = |live: f64, fallback: f64| if live > 0.0 { live } else { fallback };
    let unified_fallback_maxes = if let Some(ref cs) = pre_cs {
        (
            pick_max(cs.max_structure_hp, fit_max_hull),
            pick_max(cs.max_armor_hp, fit_max_armor),
            pick_max(cs.max_shield_hp, fit_max_shield),
        )
    } else {
        (fit_max_hull, fit_max_armor, fit_max_shield)
    };

    let (pre_unified, pre_dyn_shield_charge, pre_dyn_cap, mut repaired_child_attrs, pre_ship_heat) =
        ctx.db.with_snapshot(|snap| {
            let unified = (req.target_ship_id > 0).then(|| {
                crate::sp6_lifecycle::read_unified_ship_damage_state(
                    ctx,
                    snap,
                    req.target_ship_id,
                    req.ship_type_id,
                    Some(unified_fallback_maxes),
                )
            });
            let ship_it = snap.items.get(&req.target_ship_id);
            let dyn_shield = ship_it
                .and_then(|it| it.dynamic_attributes.get(&eve_sde::attr_ids::SHIELD_CHARGE).copied());
            let dyn_cap = ship_it.and_then(|it| it.dynamic_attributes.get(&18u16).copied());
            let ship_heat = ship_it
                .and_then(|it| it.dynamic_attributes.get(&1211u16).copied())
                .unwrap_or(0.0);
            let mut child_changes: Vec<(i64, u16, f64)> = Vec::new();
            if req.target_ship_id > 0 {
                for item in snap.items.values() {
                    if item.location_id == req.target_ship_id {
                        if let Some(&old_d) = item.dynamic_attributes.get(&eve_sde::attr_ids::DAMAGE) {
                            if old_d > 0.0 {
                                child_changes.push((item.item_id, eve_sde::attr_ids::DAMAGE, old_d));
                            }
                        }
                        if let Some(&old_h) = item.dynamic_attributes.get(&1211u16) {
                            if old_h > 0.0 {
                                child_changes.push((item.item_id, 1211u16, old_h));
                            }
                        }
                    }
                }
            }
            (unified, dyn_shield, dyn_cap, child_changes, ship_heat)
        });

    let mut combat_healed = eve_destiny::CombatState::default();
    if let Some(ref handle) = handle_opt {
        if let Some(ref cs) = pre_cs {
            let needs_cap_sync = fit_max_cap > 0.0 && cs.max_capacitor + 1e-6 < fit_max_cap;
            let needs_recharge_sync = (fit_cap_recharge_ms > 0.0
                && (cs.capacitor_recharge_time_ms <= 0.0
                    || (needs_cap_sync
                        && (cs.capacitor_recharge_time_ms - fit_cap_recharge_ms).abs() > 1e-3)))
                || (fit_shield_recharge_ms > 0.0 && cs.shield_recharge_time_ms <= 0.0);
            let needs_zero_hp_sync = (cs.max_shield_hp <= 0.0 && fit_max_shield > 0.0)
                || (cs.max_armor_hp <= 0.0 && fit_max_armor > 0.0)
                || (cs.max_structure_hp <= 0.0 && fit_max_hull > 0.0);
            if needs_cap_sync || needs_recharge_sync || needs_zero_hp_sync {
                let _ = handle
                    .modify_combat_state(req.target_ship_id, move |st| {
                        if needs_cap_sync {
                            st.max_capacitor = fit_max_cap;
                            st.base_max_capacitor = Some(fit_max_cap);
                        }
                        if fit_cap_recharge_ms > 0.0
                            && (st.capacitor_recharge_time_ms <= 0.0 || needs_cap_sync)
                        {
                            st.capacitor_recharge_time_ms = fit_cap_recharge_ms;
                            st.base_capacitor_recharge_time_ms = Some(fit_cap_recharge_ms);
                        }
                        if fit_shield_recharge_ms > 0.0 && st.shield_recharge_time_ms <= 0.0 {
                            st.shield_recharge_time_ms = fit_shield_recharge_ms;
                        }
                        if st.max_shield_hp <= 0.0 && fit_max_shield > 0.0 {
                            st.max_shield_hp = fit_max_shield;
                            st.base_max_shield_hp = Some(fit_max_shield);
                        }
                        if st.max_armor_hp <= 0.0 && fit_max_armor > 0.0 {
                            st.max_armor_hp = fit_max_armor;
                            st.base_max_armor_hp = Some(fit_max_armor);
                        }
                        if st.max_structure_hp <= 0.0 && fit_max_hull > 0.0 {
                            st.max_structure_hp = fit_max_hull;
                            st.base_max_structure_hp = Some(fit_max_hull);
                        }
                    })
                    .await;
            }
        }
        if let Ok(cs) = handle.heal_combat_state(req.target_ship_id).await {
            combat_healed = cs;
        }
        let _ = handle.flush_peer_observer_updates(None).await;
    }

    if req.target_ship_id > 0 {
        ctx.db.with_mut_snapshot(|snap| {
            clear_ship_and_fitted_damage_in_snap(snap, req.target_ship_id, true);
        });
    }
    if has_fit {
        let mut cached_damaged_ids = Vec::with_capacity(cached_damaged_mods.len());
        for (mod_id, dmg_hp) in cached_damaged_mods {
            cached_damaged_ids.push(mod_id);
            if !repaired_child_attrs
                .iter()
                .any(|&(id, aid, _)| id == mod_id && aid == eve_sde::attr_ids::DAMAGE)
            {
                repaired_child_attrs.push((mod_id, eve_sde::attr_ids::DAMAGE, dmg_hp));
            }
        }
        clear_fitted_module_damage_in_cache(ctx, &cached_damaged_ids);
        ctx.update_ship_rack_heat(req.target_char_id, req.target_ship_id, [0.0, 0.0, 0.0], true);
    }

    let eff_max_shield = combat_healed.max_shield_hp.max(fit_max_shield);
    let eff_max_armor = combat_healed.max_armor_hp.max(fit_max_armor);
    let eff_max_hull = combat_healed.max_structure_hp.max(fit_max_hull);
    let eff_max_cap = combat_healed.max_capacitor.max(fit_max_cap);
    let final_shield = pick_max(
        if combat_healed.max_shield_hp > 0.0 { combat_healed.shield_hp } else { 0.0 },
        eff_max_shield,
    );
    let final_armor = pick_max(
        if combat_healed.max_armor_hp > 0.0 { combat_healed.armor_hp } else { 0.0 },
        eff_max_armor,
    );
    let final_hull = pick_max(
        if combat_healed.max_structure_hp > 0.0 { combat_healed.structure_hp } else { 0.0 },
        eff_max_hull,
    );
    let final_cap = pick_max(
        if combat_healed.max_capacitor > 0.0 { combat_healed.capacitor } else { 0.0 },
        eff_max_cap,
    );

    if req.target_char_id > 0 && req.target_ship_id > 0 {
        let (old_shield, old_armor_dmg, old_hull_dmg) = if let Some(u) = pre_unified {
            let s = match pre_dyn_shield_charge {
                Some(dc) if pre_cs.as_ref().is_none_or(|c| c.max_shield_hp <= 0.0) => {
                    dc.clamp(0.0, final_shield.max(dc))
                }
                Some(dc) => u.shield_hp.min(dc).max(0.0),
                None => u.shield_hp,
            };
            (s, u.armor_damage, u.hull_damage)
        } else {
            (pre_dyn_shield_charge.unwrap_or(final_shield), 0.0, 0.0)
        };
        let old_cap = match (pre_cs.as_ref().filter(|c| c.max_capacitor > 0.0), pre_dyn_cap) {
            (Some(cs), Some(dc)) => cs.capacitor.min(dc).max(0.0),
            (Some(cs), None) => cs.capacitor.max(0.0),
            (None, Some(dc)) => dc.max(0.0),
            (None, None) => final_cap,
        };

        let mut attr_changes: Vec<(i64, u16, f64, f64)> = Vec::new();
        if final_shield > 0.0 || pre_dyn_shield_charge.is_some() {
            attr_changes.push((req.target_ship_id, eve_sde::attr_ids::SHIELD_CHARGE, final_shield, old_shield));
        }
        if final_armor > 0.0 || old_armor_dmg > 0.0 {
            attr_changes.push((req.target_ship_id, eve_sde::attr_ids::ARMOR_DAMAGE, 0.0, old_armor_dmg));
        }
        if final_hull > 0.0 || old_hull_dmg > 0.0 {
            attr_changes.push((req.target_ship_id, eve_sde::attr_ids::DAMAGE, 0.0, old_hull_dmg));
        }
        let emit_cap_sync = (final_cap > 0.0 || pre_dyn_cap.is_some()) && eff_max_cap > 0.0;
        if !emit_cap_sync && (final_cap > 0.0 || pre_dyn_cap.is_some()) {
            attr_changes.push((req.target_ship_id, 18u16, final_cap, old_cap));
        }
        if pre_ship_heat > 0.0 {
            attr_changes.push((req.target_ship_id, 1211u16, 0.0, pre_ship_heat));
        }
        for (idx, rack_attr) in [1175u16, 1176, 1177].into_iter().enumerate() {
            if pre_rack_heat[idx] > 0.0 {
                attr_changes.push((req.target_ship_id, rack_attr, 0.0, pre_rack_heat[idx]));
            }
        }
        repaired_child_attrs.sort_unstable_by_key(|&(id, aid, _)| (id, aid));
        for (mod_id, aid, old_val) in repaired_child_attrs {
            attr_changes.push((mod_id, aid, 0.0, old_val));
        }
        ctx.notify_module_attribute_changes(req.target_char_id, req.target_ship_id, &attr_changes);
        if emit_cap_sync {
            let _ = ctx.notify_ship_capacitor_change(
                req.target_char_id,
                req.target_ship_id,
                final_cap,
                eff_max_cap,
                Some(old_cap),
            );
        }
        if let Some(scene) = BallparkScene::try_solar_system(req.sys_id) {
            let _ = ctx.notify_damage_state_change(
                NotificationTarget::Character(req.target_char_id),
                scene,
                req.target_ship_id,
                1.0,
                1.0,
                1.0,
            );
        }
    }

    Ok(RestoreShipVitalsOutcome {
        character_id: req.target_char_id,
        ship_id: req.target_ship_id,
        shield_hp: final_shield,
        armor_hp: final_armor,
        hull_hp: final_hull,
        capacitor: final_cap,
    })
}

/// Destroys a player/DB ship (via [`crate::combat::death_pipeline::execute_death_pipeline`],
/// rejecting `sys_id == 0` with `NoSolarSystem`) or a space ball / tracked NPC (via
/// [`crate::combat::npc_ai::NpcSpawnManager::handle_npc_destroyed_detailed_with_options`] or
/// [`spawn_npc_wreck_and_loot_in_snap`]), never waking system `0` or cold solar systems for space balls (`#1542`).
pub async fn destroy_entity_in_system(
    ctx: &ServerContext,
    req: DestroyEntityRequest,
) -> Result<DestroyEntityOutcome, DomainError> {
    let handle_opt = (req.sys_id > 0)
        .then(|| ctx.systems.get_handle(req.sys_id))
        .flatten();
    if req.in_db_items {
        if req.sys_id == 0 {
            return Err(DomainError::precondition_failed(
                "NoSolarSystem",
                format!("Target {} is not in an active solar system", req.victim_ship_id),
            ));
        }
        let victim_pos = if let Some(ref handle) = handle_opt {
            handle
                .get_ball_snapshot(req.victim_ship_id)
                .await
                .ok()
                .flatten()
                .map(|(_, b)| b.position)
                .unwrap_or(DVec3::ZERO)
        } else {
            DVec3::ZERO
        };
        let outcome = Box::pin(crate::combat::death_pipeline::execute_death_pipeline(
            ctx,
            req.sys_id,
            req.victim_char_id,
            req.victim_ship_id,
            req.killer_char_id,
            victim_pos,
            ctx.now_unix(),
        ))
        .await
        .map_err(|e| DomainError::internal("KillFailed", e))?;
        if let Some(ref handle) = handle_opt {
            let _ = handle.flush_peer_observer_updates(None).await;
        }
        return Ok(DestroyEntityOutcome {
            target_id: req.victim_ship_id,
            wreck_id: outcome.wreck_item_id,
            killmail_id: outcome.killmail_id,
            ejected_capsule_id: outcome.ejected_capsule_id,
        });
    }

    let Some(handle) = handle_opt else {
        return Err(DomainError::not_found(
            "TargetNotFound",
            format!("Target entity {} not found", req.target_id),
        ));
    };
    let Some((_, ball)) = handle
        .get_ball_snapshot(req.victim_ship_id)
        .await
        .ok()
        .flatten()
    else {
        return Err(DomainError::not_found(
            "TargetNotFound",
            format!("Target entity {} not found", req.target_id),
        ));
    };
    let victim_pos = ball.position;
    let _ = despawn_entity_in_ballpark(
        ctx,
        Some(&handle),
        DespawnEntityInBallparkRequest {
            entity_id: req.victim_ship_id,
            remove_from_db: false,
            remove_warp_bubble: false,
            clean_npc_runtime_system: None,
        },
    )
    .await;
    ctx.systems
        .invoke_npc_death_hook(req.sys_id, req.victim_ship_id, Some(req.killer_char_id))
        .await;
    crate::services::fw_gameplay::on_space_entity_destroyed(
        ctx,
        req.sys_id,
        Some(req.killer_char_id),
        req.victim_ship_id,
        ball.type_id,
    )
    .await;

    let now_unix = ctx.now_unix();
    let tracked_res: Option<Result<DestroyedNpcOutcome, String>> = ctx
        .npc_spawns
        .write()
        .ok()
        .and_then(|mut spawns| {
            let mgr = spawns.get_mut(&req.sys_id)?;
            mgr.entities.contains_key(&req.victim_ship_id).then(|| {
                mgr.handle_npc_destroyed_detailed_with_options(
                    ctx,
                    req.sys_id,
                    req.victim_ship_id,
                    req.killer_char_id,
                    now_unix,
                    NpcDestroyedOptions {
                        skip_death_hook_duplicates: true,
                        live_pos_override: Some(victim_pos),
                    },
                )
            })
        });
    crate::services::combat_crimewatch::with_mut_combat_runtime(ctx, |rt| {
        rt.npc_entities.remove(&req.victim_ship_id);
        rt.destroyed_npc_cache.remove(&req.victim_ship_id);
    });

    let (wreck_id, killmail_id) = match tracked_res {
        Some(Ok(npc_out)) => {
            if npc_out.wreck_id > 0 && npc_out.wreck_type_id > 0 {
                let _ = spawn_wreck_ball_in_ballpark(
                    ctx,
                    &handle,
                    npc_out.wreck_id,
                    npc_out.wreck_type_id,
                    req.killer_char_id,
                    npc_out.wreck_pos,
                    None,
                )
                .await;
            }
            (npc_out.wreck_id, npc_out.killmail_id)
        }
        Some(Err(err)) => return Err(DomainError::internal("KillFailed", err)),
        None => {
            let in_abyssal = (32_000_000..=32_999_999).contains(&req.sys_id);
            let wt_id = if in_abyssal {
                0
            } else {
                ctx.sde
                    .resolve_wreck_type_id(ball.type_id, None)
                    .unwrap_or(26557)
            };
            let killmail_value_isk = ctx
                .sde
                .types
                .get(&ball.type_id)
                .map(|t| {
                    let bounty = t
                        .default_attributes
                        .get(&eve_sde::attr_ids::ENTITY_KILL_BOUNTY)
                        .copied()
                        .unwrap_or(0.0);
                    bounty.max(t.base_price).max(0.0)
                })
                .unwrap_or(0.0);
            let kid = ctx.db.with_mut_snapshot(|snap| {
                let kid = snap.alloc_entity_id();
                snap.record_killmail(
                    KillmailRecord {
                        kill_id: kid,
                        victim_character_id: 0,
                        attacker_character_id: req.killer_char_id,
                        solar_system_id: req.sys_id,
                        ship_type_id: ball.type_id,
                        total_value_isk: killmail_value_isk,
                        ..Default::default()
                    },
                    now_unix,
                );
                kid
            });
            let wid = if !in_abyssal && wt_id > 0 {
                spawn_wreck_with_loot_in_ballpark(
                    ctx,
                    &handle,
                    SpawnWreckWithLootRequest {
                        solar_system_id: req.sys_id,
                        owner_char_id: req.killer_char_id,
                        pos: victim_pos,
                        now_unix,
                        wreck_type_id: wt_id,
                        loot_entries: Vec::new(),
                        name_override: None,
                    },
                )
                .await
                .map(|o| o.wreck_id)
                .unwrap_or(0)
            } else {
                0
            };
            (wid, kid)
        }
    };
    let _ = handle.flush_peer_observer_updates(None).await;

    Ok(DestroyEntityOutcome {
        target_id: req.victim_ship_id,
        wreck_id,
        killmail_id,
        ejected_capsule_id: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::npc_ai::{NpcAiState, NpcCombatEntity, NpcSpawnManager};
    use eve_destiny::DamageVector;

    #[tokio::test]
    async fn heal_and_kill_do_not_wake_zero_or_cold_solar_system() {
        let ctx = ServerContext::new();
        let cid = 90_000_001_i64;
        let (ship_id, ship_type_id) = ctx.db.with_snapshot(|s| {
            let sid = s.characters[&cid].active_ship_id;
            (sid, s.items[&sid].type_id)
        });
        let cold_sys = 30_000_142_u32;
        assert!(ctx.systems.get_handle(0).is_none());
        assert!(ctx.systems.get_handle(cold_sys).is_none());

        ctx.db.with_mut_snapshot(|snap| {
            if let Some(it) = snap.items.get_mut(&ship_id) {
                it.dynamic_attributes.insert(eve_sde::attr_ids::DAMAGE, 33.0);
                it.dynamic_attributes.insert(1211u16, 12.0);
            }
        });

        let healed = restore_ship_vital_state(
            &ctx,
            RestoreShipVitalsRequest {
                target_char_id: cid,
                target_ship_id: ship_id,
                ship_type_id,
                sys_id: cold_sys,
            },
        )
        .await
        .expect("cold-system vital restore clears DB without waking system");
        assert_eq!(healed.ship_id, ship_id);
        assert!(ctx.systems.get_handle(cold_sys).is_none(), "heal must not wake cold system");
        ctx.db.with_snapshot(|snap| {
            let attrs = &snap.items[&ship_id].dynamic_attributes;
            assert!(!attrs.contains_key(&eve_sde::attr_ids::DAMAGE));
            assert!(!attrs.contains_key(&1211u16));
        });

        let _ = restore_ship_vital_state(
            &ctx,
            RestoreShipVitalsRequest {
                target_char_id: cid,
                target_ship_id: ship_id,
                ship_type_id,
                sys_id: 0,
            },
        )
        .await
        .expect("zero-system vital restore does not spawn sys 0");
        assert!(ctx.systems.get_handle(0).is_none());

        let db_zero_err = destroy_entity_in_system(
            &ctx,
            DestroyEntityRequest {
                target_id: ship_id,
                victim_char_id: cid,
                victim_ship_id: ship_id,
                sys_id: 0,
                killer_char_id: cid,
                in_db_items: true,
            },
        )
        .await
        .expect_err("DB entity kill in sys 0 must fail closed");
        assert_eq!(db_zero_err.code, "NoSolarSystem");
        assert!(ctx.systems.get_handle(0).is_none());

        let ball_cold_err = destroy_entity_in_system(
            &ctx,
            DestroyEntityRequest {
                target_id: 950_777_001,
                victim_char_id: cid,
                victim_ship_id: 950_777_001,
                sys_id: cold_sys,
                killer_char_id: cid,
                in_db_items: false,
            },
        )
        .await
        .expect_err("ball kill in cold system must fail closed without spawning system");
        assert_eq!(ball_cold_err.code, "TargetNotFound");
        assert!(ctx.systems.get_handle(cold_sys).is_none());
    }

    #[tokio::test]
    async fn destroy_entity_tracked_npc_uses_unified_spawn_manager_path() {
        let ctx = ServerContext::new();
        let cid = 90_000_001_i64;
        let sys_id = 30_000_142_u32;
        let npc_id = 950_888_001_i64;
        let npc_type_id = ctx
            .sde
            .types
            .iter()
            .find(|(_, t)| t.category_id == 11 && ctx.sde.resolve_wreck_type_id(t.type_id, None).is_some())
            .map(|(&tid, _)| tid)
            .unwrap_or(16_997);
        let npc_pos = DVec3::new(3_000.0, 0.0, 0.0);
        let handle = ctx.systems.get_or_spawn(sys_id);
        handle
            .spawn_ball_with_combat(
                Ball {
                    id: npc_id,
                    type_id: npc_type_id,
                    position: npc_pos,
                    velocity: DVec3::ZERO,
                    max_velocity: 200.0,
                    inertia: 1.0,
                    mass: 1_000_000.0,
                    radius: 50.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                ctx.build_type_combat_state(npc_type_id),
            )
            .await
            .unwrap();
        if let Ok(mut spawns) = ctx.npc_spawns.write() {
            spawns
                .entry(sys_id)
                .or_insert_with(NpcSpawnManager::new)
                .entities
                .insert(
                    npc_id,
                    NpcCombatEntity {
                        ball_id: npc_id,
                        type_id: npc_type_id,
                        faction_id: 500_010,
                        state: NpcAiState::Idle,
                        spawn_group_id: 0,
                        spawn_pos: npc_pos,
                        target_id: None,
                        bounty_isk: 12_500.0,
                        optimal_range: 5_000.0,
                        orbit_range: 3_000.0,
                        base_dps: 10.0,
                        damage_vector: DamageVector::default(),
                        last_attack_tick: 0,
                        has_shield_rep: false,
                        has_armor_rep: false,
                    },
                );
        }

        let pre_sec = ctx.db.with_snapshot(|s| s.characters[&cid].security_status);
        let out = destroy_entity_in_system(
            &ctx,
            DestroyEntityRequest {
                target_id: npc_id,
                victim_char_id: cid,
                victim_ship_id: npc_id,
                sys_id,
                killer_char_id: cid,
                in_db_items: false,
            },
        )
        .await
        .expect("tracked NPC destroy succeeds");
        assert!(out.wreck_id > 0);
        assert!(out.killmail_id > 0);
        assert!(ctx.npc_spawns.read().unwrap().get(&sys_id).is_none_or(|m| !m.entities.contains_key(&npc_id)));
        assert!(handle.get_ball_snapshot(npc_id).await.unwrap().is_none());
        assert!(handle.get_ball_snapshot(out.wreck_id).await.unwrap().is_some());
        ctx.db.with_snapshot(|snap| {
            assert!(snap.items.contains_key(&out.wreck_id));
            assert!(snap.killmails.contains_key(&out.killmail_id));
            assert!(snap.characters[&cid].security_status >= pre_sec);
        });
    }

    #[tokio::test]
    async fn destroy_entity_tracked_npc_credits_corp_project_payout_and_achievement_once() {
        use crate::services::social_corp_fleet::social_corp_projects::{
            create_canonical_project, CanonicalProjectSpec,
        };
        use eve_db::IskCents;

        let ctx = ServerContext::new();
        let cid = 90_000_001_i64;
        let sys_id = 30_000_142_u32;
        let npc_id = 950_888_002_i64;
        let npc_type_id = ctx
            .sde
            .types
            .iter()
            .find(|(_, t)| t.category_id == 11 && ctx.sde.resolve_wreck_type_id(t.type_id, None).is_some())
            .map(|(&tid, _)| tid)
            .unwrap_or(16_997);
        let spawn_pos = DVec3::new(3_000.0, 0.0, 0.0);
        let live_pos = DVec3::new(4_200.0, 1_100.0, -500.0);

        let corp_id = ctx.db.with_mut_snapshot(|snap| {
            let corp_id = snap.characters[&cid].corporation_id;
            let corp = snap.corporations.entry(corp_id).or_insert_with(|| {
                let mut rec = eve_db::CorporationRecord::default();
                rec.corporation_id = corp_id;
                rec.name = "Test Corp".into();
                rec.ticker = "TSTC".into();
                rec.ceo_id = cid;
                rec
            });
            corp.ensure_wallet_divisions();
            corp.set_division_balance(1, IskCents::from_isk(1_000_000));
            corp_id
        });
        let (pid, goal_uuid) = create_canonical_project(
            &ctx.db,
            CanonicalProjectSpec {
                corp_id,
                creator_id: cid,
                name: "Single NPC Kill Payout".into(),
                description: "Verify destroy_entity_in_system credits +1 once".into(),
                target_quantity: 5,
                initial_quantity: 0,
                escrow: IskCents::from_isk(50_000),
                isk_per_unit: IskCents::from_isk(10_000),
                wallet_division: 1,
                matcher_type: "DestroyNPCShips".into(),
                is_freelance: false,
                auto_payout_on_contribution: true,
                max_per_character: None,
                expires_unix: None,
                filter_id: None,
                batch_size: 1,
                now_unix: ctx.now_unix(),
                min_project_id: 9_100,
            },
        )
        .expect("create DestroyNPCShips corp project");
        let goal_hex = crate::services::quasar_progression_extra::uuid_to_hex(&goal_uuid);
        {
            let mut ggoals = ctx
                .quasar_gateway
                .progression
                .extra
                .generic_goals
                .write()
                .unwrap();
            ggoals.insert(
                goal_hex.clone(),
                crate::services::quasar_progression_extra::GenericGoalRecord {
                    uuid: goal_uuid,
                    creator_id: cid as u32,
                    name: "Single NPC Kill Payout".into(),
                    description: "Verify destroy_entity_in_system credits +1 once".into(),
                    desired_progress: 5,
                    current_progress: 0,
                    closed: false,
                    matcher_type: "DestroyNPCShips".into(),
                    created_unix: ctx.now_unix(),
                    ..Default::default()
                },
            );
        }
        let sub_sess = eve_machonet::SessionState {
            session_id: 99_101,
            user_id: Some(1),
            character_id: Some(cid),
            corporation_id: Some(corp_id),
            ..eve_machonet::SessionState::new(99_101)
        };
        let _notice_rx = ctx
            .quasar_gateway
            .subscribe_session(&sub_sess, Vec::new(), ctx.now_unix());

        let handle = ctx.systems.get_or_spawn(sys_id);
        handle
            .spawn_ball_with_combat(
                Ball {
                    id: npc_id,
                    type_id: npc_type_id,
                    position: live_pos,
                    velocity: DVec3::ZERO,
                    max_velocity: 200.0,
                    inertia: 1.0,
                    mass: 1_000_000.0,
                    radius: 50.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                ctx.build_type_combat_state(npc_type_id),
            )
            .await
            .unwrap();
        if let Ok(mut spawns) = ctx.npc_spawns.write() {
            spawns
                .entry(sys_id)
                .or_insert_with(NpcSpawnManager::new)
                .entities
                .insert(
                    npc_id,
                    NpcCombatEntity {
                        ball_id: npc_id,
                        type_id: npc_type_id,
                        faction_id: 500_010,
                        state: NpcAiState::Idle,
                        spawn_group_id: 0,
                        spawn_pos,
                        target_id: None,
                        bounty_isk: 0.0,
                        optimal_range: 5_000.0,
                        orbit_range: 3_000.0,
                        base_dps: 10.0,
                        damage_vector: DamageVector::default(),
                        last_attack_tick: 0,
                        has_shield_rep: false,
                        has_armor_rep: false,
                    },
                );
        }

        let conds_for = |ev: &str| -> Vec<(u32, u32)> {
            ctx.quasar_gateway
                .progression
                .achievements
                .definitions
                .iter()
                .flat_map(|d| {
                    d.conditions
                        .iter()
                        .filter(move |c| c.event_name == ev)
                        .map(move |c| (d.id, c.condition_id))
                })
                .collect()
        };
        let cond_val = |ach_id: u32, cond_id: u32| -> u64 {
            ctx.quasar_gateway
                .progression
                .achievements
                .character_achievements
                .read()
                .unwrap()
                .get(&(cid as u32))
                .and_then(|list| list.iter().find(|a| a.def.id == ach_id))
                .and_then(|a| a.condition_progress.get(&cond_id).copied())
                .unwrap_or(0)
        };
        let hostile_conds = conds_for("destroyHostileNPC");
        assert!(!hostile_conds.is_empty(), "expected destroyHostileNPC achievement condition");
        let group_conds = conds_for("entityGroupDestroyed");
        let hostile_before: Vec<u64> = hostile_conds.iter().map(|&(a, c)| cond_val(a, c)).collect();
        let group_before: Vec<u64> = group_conds.iter().map(|&(a, c)| cond_val(a, c)).collect();
        let bal_before = ctx.db.with_snapshot(|s| s.characters[&cid].balance);

        let out = destroy_entity_in_system(
            &ctx,
            DestroyEntityRequest {
                target_id: npc_id,
                victim_char_id: cid,
                victim_ship_id: npc_id,
                sys_id,
                killer_char_id: cid,
                in_db_items: false,
            },
        )
        .await
        .expect("tracked NPC destroy succeeds");

        let wreck_ball = handle
            .get_ball_snapshot(out.wreck_id)
            .await
            .unwrap()
            .expect("wreck ball spawned");
        assert_eq!(wreck_ball.1.position, live_pos, "wreck ball must use live NPC position");
        ctx.db.with_snapshot(|snap| {
            assert_eq!(
                snap.item_positions.get(&out.wreck_id).copied(),
                Some([live_pos.x, live_pos.y, live_pos.z]),
                "persisted wreck position must use live NPC position"
            );
            assert_eq!(snap.corp_projects[&pid].current_quantity, 1);
            assert_eq!(
                snap.characters[&cid].balance.raw_cents() - bal_before.raw_cents(),
                IskCents::from_isk(10_000).raw_cents(),
                "auto-payout must credit 1x 10,000 ISK per kill, not 2x"
            );
        });
        for (&(ach_id, cond_id), &prev) in hostile_conds.iter().zip(&hostile_before) {
            assert_eq!(
                cond_val(ach_id, cond_id),
                prev + 1,
                "destroyHostileNPC condition ({ach_id}, {cond_id}) must increment by 1"
            );
        }
        for (&(ach_id, cond_id), &prev) in group_conds.iter().zip(&group_before) {
            assert_eq!(
                cond_val(ach_id, cond_id),
                prev + 1,
                "entityGroupDestroyed condition ({ach_id}, {cond_id}) must increment by 1"
            );
        }
        let generic_progress = ctx
            .quasar_gateway
            .progression
            .extra
            .generic_goals
            .read()
            .unwrap()
            .get(&goal_hex)
            .map(|g| g.current_progress);
        assert_eq!(
            generic_progress,
            Some(1),
            "quasar generic_goals must synchronize +1 alongside snap.corp_projects"
        );
        let notices = ctx.quasar_gateway.poll_notices(sub_sess.session_id);
        let contrib_notices = notices
            .iter()
            .filter(|n| {
                n.payload
                    .as_ref()
                    .is_some_and(|p| p.type_url.ends_with("eve_public.goal.contribution.api.ContributedNotice"))
            })
            .count();
        let redeemed_notices = notices
            .iter()
            .filter(|n| {
                n.payload
                    .as_ref()
                    .is_some_and(|p| p.type_url.ends_with("eve_public.corporationgoal.api.RedeemedNotice"))
            })
            .count();
        assert_eq!(contrib_notices, 1, "expected 1 ContributedNotice, got {contrib_notices}");
        assert_eq!(redeemed_notices, 1, "expected 1 RedeemedNotice, got {redeemed_notices}");
    }

    #[tokio::test]
    async fn destroy_entity_untracked_spawnnpc_clears_destroyed_npc_cache_across_cycle_cleanup() {
        let ctx = ServerContext::new();
        let cid = 90_000_001_i64;
        let sys_id = 30_000_142_u32;
        let npc_type_id = ctx
            .sde
            .types
            .iter()
            .find(|(_, t)| t.category_id == 11 && ctx.sde.resolve_wreck_type_id(t.type_id, None).is_some())
            .map(|(&tid, _)| tid)
            .unwrap_or(16_997);
        let npc_id = crate::services::combat_crimewatch::combat_npc_ai::spawn_npc_entity_from_sde(
            &ctx,
            sys_id,
            npc_type_id,
            0,
            DVec3::new(1_500.0, 0.0, 0.0),
        )
        .await;
        assert!(crate::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| {
            rt.npc_entities.contains_key(&npc_id)
        }));

        let out = destroy_entity_in_system(
            &ctx,
            DestroyEntityRequest {
                target_id: npc_id,
                victim_char_id: cid,
                victim_ship_id: npc_id,
                sys_id,
                killer_char_id: cid,
                in_db_items: false,
            },
        )
        .await
        .expect("untracked /spawnnpc destroy succeeds");
        assert!(out.wreck_id > 0);

        crate::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| {
            assert!(!rt.npc_entities.contains_key(&npc_id));
            assert!(
                !rt.destroyed_npc_cache.contains_key(&npc_id),
                "destroy_entity_in_system must clear destroyed_npc_cache entry"
            );
        });

        let (items_before, km_before, standings_before) = ctx.db.with_snapshot(|snap| {
            (
                snap.items.len(),
                snap.killmails.len(),
                snap.characters[&cid].standings.clone(),
            )
        });
        crate::services::combat_npc_encounters::finalize_cycle_destroyed_npcs(&ctx, sys_id).await;
        ctx.db.with_snapshot(|snap| {
            assert_eq!(snap.items.len(), items_before, "finalize_cycle_destroyed_npcs must not spawn duplicate wreck/loot");
            assert_eq!(snap.killmails.len(), km_before);
            assert_eq!(snap.characters[&cid].standings, standings_before);
        });
        crate::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| {
            assert!(!rt.destroyed_npc_cache.contains_key(&npc_id));
        });
    }

    #[tokio::test]
    async fn spawn_despawn_and_wreck_with_loot_roundtrip_atomically() {
        let ctx = ServerContext::new();
        let sys_id = 30_000_142_u32;
        let cid = 90_000_001_i64;
        let handle = ctx
            .systems
            .wake_for_gm_command_async(sys_id, "domain_combat_ops_test")
            .await
            .expect("wake system");

        let ship_pos = DVec3::new(1_000.0, 2_000.0, 3_000.0);
        let spawned_id = spawn_entity_in_ballpark(
            &ctx,
            &handle,
            SpawnEntityInBallparkRequest {
                entity_id: None,
                db_item: Some(InventoryItemRecord {
                    item_id: 0,
                    type_id: 587,
                    owner_id: cid,
                    location_id: i64::from(sys_id),
                    flag: eve_db::InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                }),
                ball: Ball {
                    id: 0,
                    type_id: 587,
                    position: ship_pos,
                    velocity: DVec3::ZERO,
                    max_velocity: 300.0,
                    inertia: 1.0,
                    mass: 1_000_000.0,
                    radius: 40.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                payload: SpawnBallPayload::Meta {
                    combat_state: ctx.build_type_combat_state(587),
                    slim_meta: ctx.build_ship_slim_meta(cid, 587),
                },
                mark_non_interactive: true,
            },
        )
        .await
        .expect("spawn entity in ballpark");
        assert!(spawned_id > 0);
        assert!(ctx.db.with_snapshot(|s| s.items.contains_key(&spawned_id)));
        assert!(handle
            .get_ball_snapshot(spawned_id)
            .await
            .expect("ball query")
            .is_some());

        let wreck_out = spawn_wreck_with_loot_in_ballpark(
            &ctx,
            &handle,
            SpawnWreckWithLootRequest {
                solar_system_id: sys_id,
                owner_char_id: cid,
                pos: ship_pos + DVec3::new(500.0, 0.0, 0.0),
                now_unix: ctx.now_unix(),
                wreck_type_id: 26_557,
                loot_entries: vec![(34, 15, false)],
                name_override: Some("Test Wreck".to_string()),
            },
        )
        .await
        .expect("spawn wreck with loot");
        assert_eq!(wreck_out.loot_items.len(), 1);
        assert_eq!(wreck_out.loot_items[0].type_id, 34);
        assert_eq!(wreck_out.loot_items[0].quantity, 15);
        let (_, wreck_ball) = handle
            .get_ball_snapshot(wreck_out.wreck_id)
            .await
            .expect("wreck query")
            .expect("wreck ball live");
        assert_eq!(wreck_ball.mode, MovementMode::Rigid);

        let despawned_wreck = despawn_entity_in_ballpark(
            &ctx,
            Some(&handle),
            DespawnEntityInBallparkRequest {
                entity_id: wreck_out.wreck_id,
                remove_from_db: true,
                remove_warp_bubble: false,
                clean_npc_runtime_system: None,
            },
        )
        .await;
        assert!(despawned_wreck.removed_from_db);
        assert_eq!(despawned_wreck.removed_children, 1);
        assert!(despawned_wreck.removed_ball.is_some());
        ctx.db.with_snapshot(|snap| {
            assert!(!snap.items.contains_key(&wreck_out.wreck_id));
            assert!(!snap.items.contains_key(&wreck_out.loot_items[0].item_id));
        });

        let despawned_ship = despawn_entity_in_ballpark(
            &ctx,
            Some(&handle),
            DespawnEntityInBallparkRequest {
                entity_id: spawned_id,
                remove_from_db: true,
                remove_warp_bubble: false,
                clean_npc_runtime_system: None,
            },
        )
        .await;
        assert!(despawned_ship.removed_from_db);
        assert!(despawned_ship.removed_ball.is_some());
    }
}
