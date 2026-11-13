//! `death_pipeline` — Canonical ship destruction, wreck spawning, 50% loot drop, pod ejection, insurance payout, and killmail generation (`C-3`).

#![forbid(unsafe_code)]

use crate::formulas::compute_insurance_quote;
use crate::router::{NotificationTarget, ServerContext};
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, KillmailRecord};
use eve_destiny::{Ball, MovementMode, SlimItemMeta};
use eve_proto::PyRep;
use glam::DVec3;
use std::collections::HashMap;

/// Result summary of the completed death pipeline execution.
#[derive(Debug, Clone)]
pub struct DeathPipelineResult {
    pub wreck_item_id: i64,
    pub wreck_type_id: u32,
    pub loot_dropped_item_ids: Vec<i64>,
    pub loot_destroyed_item_ids: Vec<i64>,
    pub ejected_capsule_id: Option<i64>,
    pub insurance_payout_isk: f64,
    pub killmail_id: i64,
}

#[derive(Debug, Default)]
pub(crate) struct DestroyedShipLootOutcome {
    pub dropped_ids: Vec<i64>,
    pub destroyed_ids: Vec<i64>,
    pub dropped_km: Vec<eve_db::KillmailItemEntry>,
    pub destroyed_km: Vec<eve_db::KillmailItemEntry>,
    pub items_isk: f64,
    pub fw_items_isk: f64,
}

pub(crate) fn is_item_descendant_of_in_snap(
    snap: &eve_db::WorldStateSnapshot,
    item_id: i64,
    ancestor_id: i64,
) -> bool {
    let mut cur = item_id;
    for _ in 0..8 {
        let Some(it) = snap.items.get(&cur) else {
            return false;
        };
        if it.location_id == ancestor_id {
            return true;
        }
        if it.location_id <= 0 || it.location_id == cur {
            return false;
        }
        cur = it.location_id;
    }
    false
}

pub(crate) fn drain_nested_ship_fittings(
    ctx: &ServerContext,
    root_ship_id: i64,
    exclude_escape_bay: bool,
) -> Vec<(i64, i64, u32, u16)> {
    let nested_ship_ids: Vec<i64> = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .filter(|i| {
                !(exclude_escape_bay
                    && i.location_id == root_ship_id
                    && matches!(i.flag as u16, 177 | 179))
                    && ctx.sde.types.get(&i.type_id).is_some_and(|t| t.category_id == 6)
                    && is_item_descendant_of_in_snap(snap, i.item_id, root_ship_id)
            })
            .map(|i| i.item_id)
            .collect()
    });
    if nested_ship_ids.is_empty() {
        return Vec::new();
    }
    let mut guard = ctx.ship_fittings.write().unwrap_or_else(|e| e.into_inner());
    let mut out = Vec::new();
    for sid in nested_ship_ids {
        if let Some(fit) = guard.remove(&sid) {
            for m in fit.modules.values() {
                out.push((sid, m.item_id, m.type_id, m.flag_id));
            }
        }
    }
    out
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn resolve_destroyed_ship_contents_in_snap(
    snap: &mut eve_db::WorldStateSnapshot,
    sde: &eve_sde::SdeRegistry,
    ship_id: i64,
    wreck_id: i64,
    override_owner_id: Option<i64>,
    escape_frigate_id: Option<i64>,
    is_pod: bool,
    drop_threshold: f64,
    direct_child_filter: impl Fn(&InventoryItemRecord) -> bool,
    mut roll_fn: impl FnMut(i64) -> f64,
) -> DestroyedShipLootOutcome {
    let mut root_children: Vec<i64> = snap
        .items
        .values()
        .filter(|i| {
            i.location_id == ship_id
                && Some(i.item_id) != escape_frigate_id
                && direct_child_filter(i)
        })
        .map(|i| i.item_id)
        .collect();
    root_children.sort_unstable();

    let mut children_by_parent: HashMap<i64, Vec<i64>> = HashMap::new();
    for it in snap.items.values() {
        if it.location_id != ship_id
            && Some(it.location_id) != escape_frigate_id
            && snap.items.contains_key(&it.location_id)
        {
            children_by_parent
                .entry(it.location_id)
                .or_default()
                .push(it.item_id);
        }
    }
    for list in children_by_parent.values_mut() {
        list.sort_unstable();
    }

    let mut out = DestroyedShipLootOutcome::default();
    let mut visited = std::collections::HashSet::new();

    #[allow(clippy::too_many_arguments)]
    fn visit_item(
        iid: i64,
        survive_loc_id: i64,
        force_cargo_flag: bool,
        parent_survived: bool,
        snap: &mut eve_db::WorldStateSnapshot,
        sde: &eve_sde::SdeRegistry,
        children_by_parent: &HashMap<i64, Vec<i64>>,
        visited: &mut std::collections::HashSet<i64>,
        wreck_id: i64,
        override_owner_id: Option<i64>,
        is_pod: bool,
        drop_threshold: f64,
        roll_fn: &mut dyn FnMut(i64) -> f64,
        out: &mut DestroyedShipLootOutcome,
    ) {
        if !visited.insert(iid) {
            return;
        }
        let Some(item_snap) = snap.items.get(&iid).cloned() else {
            return;
        };
        if item_snap.type_id == 3468 {
            snap.items.remove(&iid);
            if let Some(kids) = children_by_parent.get(&iid) {
                for &cid in kids {
                    visit_item(
                        cid,
                        survive_loc_id,
                        true,
                        parent_survived,
                        snap,
                        sde,
                        children_by_parent,
                        visited,
                        wreck_id,
                        override_owner_id,
                        is_pod,
                        drop_threshold,
                        roll_fn,
                        out,
                    );
                }
            }
            return;
        }
        let sde_isk = sde
            .types
            .get(&item_snap.type_id)
            .map(|t| t.base_price.max(0.0))
            .unwrap_or(0.0);
        let qty_f = item_snap.quantity.max(1) as f64;
        out.fw_items_isk += sde_isk * qty_f;
        let unit_isk = if sde_isk > 0.0 { sde_isk } else { 100.0 };
        out.items_isk += unit_isk * qty_f;

        let is_rig_or_sub = matches!(item_snap.flag as u16, 89 | 92..=94 | 125..=132);
        let survives = parent_survived
            && wreck_id > 0
            && !is_pod
            && !is_rig_or_sub
            && roll_fn(iid) < drop_threshold;

        if survives {
            if let Some(it_mut) = snap.items.get_mut(&iid) {
                it_mut.location_id = survive_loc_id;
                if force_cargo_flag {
                    it_mut.flag = InventoryFlag::Cargo;
                }
                if let Some(oid) = override_owner_id {
                    it_mut.owner_id = oid;
                }
                it_mut.version += 1;
            }
            out.dropped_ids.push(iid);
            out.dropped_km.push(eve_db::KillmailItemEntry {
                type_id: item_snap.type_id,
                flag: item_snap.flag as u16,
                qty_dropped: item_snap.quantity,
                qty_destroyed: 0,
                singleton: item_snap.singleton,
            });
            if let Some(kids) = children_by_parent.get(&iid) {
                for &cid in kids {
                    visit_item(
                        cid,
                        iid,
                        false,
                        true,
                        snap,
                        sde,
                        children_by_parent,
                        visited,
                        wreck_id,
                        override_owner_id,
                        is_pod,
                        drop_threshold,
                        roll_fn,
                        out,
                    );
                }
            }
        } else {
            snap.items.remove(&iid);
            if let Some(kids) = children_by_parent.get(&iid) {
                for &cid in kids {
                    visit_item(
                        cid,
                        survive_loc_id,
                        false,
                        false,
                        snap,
                        sde,
                        children_by_parent,
                        visited,
                        wreck_id,
                        override_owner_id,
                        is_pod,
                        drop_threshold,
                        roll_fn,
                        out,
                    );
                }
            }
            out.destroyed_ids.push(iid);
            out.destroyed_km.push(eve_db::KillmailItemEntry {
                type_id: item_snap.type_id,
                flag: item_snap.flag as u16,
                qty_dropped: 0,
                qty_destroyed: item_snap.quantity,
                singleton: item_snap.singleton,
            });
        }
    }

    for iid in root_children {
        visit_item(
            iid,
            wreck_id,
            true,
            true,
            snap,
            sde,
            &children_by_parent,
            &mut visited,
            wreck_id,
            override_owner_id,
            is_pod,
            drop_threshold,
            &mut roll_fn,
            &mut out,
        );
    }
    out
}

/// Synchronously executes the canonical EVE death pipeline (DB mutations, drone/fighter
/// abandonment, disconnected-ship state cleanup, session-change timer/notification, and
/// non-blocking `SystemHandle` ball updates).
pub fn execute_death_pipeline_sync(
    ctx: &ServerContext,
    sys_id: u32,
    victim_char_id: i64,
    victim_ship_id: i64,
    attacker_char_id: i64,
    victim_pos: DVec3,
    rng_seed: u64,
) -> Result<DeathPipelineResult, String> {
    execute_death_pipeline_sync_inner(
        ctx,
        sys_id,
        victim_char_id,
        victim_ship_id,
        attacker_char_id,
        victim_pos,
        rng_seed,
        false,
    )
}

pub fn execute_death_pipeline_sync_inner(
    ctx: &ServerContext,
    sys_id: u32,
    victim_char_id: i64,
    victim_ship_id: i64,
    attacker_char_id: i64,
    victim_pos: DVec3,
    rng_seed: u64,
    is_self_destruct: bool,
) -> Result<DeathPipelineResult, String> {
    let (victim_ship_type_id, base_price, insured_payout, victim_char_id) =
        ctx.db.with_snapshot(|snap| {
            let ship = snap
                .items
                .get(&victim_ship_id)
                .ok_or_else(|| format!("Ship {victim_ship_id} not found in world repository"))?;
            let cat_id = ctx
                .sde
                .types
                .get(&ship.type_id)
                .map(|t| t.category_id)
                .unwrap_or(6);
            if cat_id != 6 {
                return Err(format!(
                    "Item {victim_ship_id} (type {}, category {cat_id}) is not a ship",
                    ship.type_id
                ));
            }
            let base = ctx
                .sde
                .types
                .get(&ship.type_id)
                .map(|t| t.base_price)
                .unwrap_or(230_000.0);
            let resolved_vic = if victim_char_id > 0 {
                victim_char_id
            } else {
                snap.characters
                    .values()
                    .find(|c| c.active_ship_id == victim_ship_id)
                    .map(|c| c.character_id)
                    .or_else(|| {
                        snap.sp6_lifecycle
                            .disconnected_ships
                            .values()
                            .find(|d| d.ship_id == victim_ship_id)
                            .map(|d| d.char_id)
                    })
                    .unwrap_or(ship.owner_id)
            };
            Ok::<_, String>((ship.type_id, base, ship.insured_payout_isk, resolved_vic))
        })?;

    let is_pod = victim_ship_type_id == 670
        || victim_ship_type_id == 33328
        || ctx
            .sde
            .types
            .get(&victim_ship_type_id)
            .map(|t| t.group_id == 29)
            .unwrap_or(false);
    let wreck_type_id = if is_pod {
        25
    } else {
        ctx.sde
            .resolve_wreck_type_id(victim_ship_type_id, None)
            .unwrap_or(26557)
    };

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let now_unix = ctx.now_unix();
    let is_criminal = ctx
        .crimewatch
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .states
        .get(&victim_char_id)
        .map(|s| s.is_criminal(now_unix))
        .unwrap_or(false);
    let rt_contract = crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
        rt.insurance_contracts.get(&victim_ship_id).cloned()
    });
    let cs_contract =
        crate::services::inventory_fitting::fitting_containers_ships::read_container_ship_state(ctx)
            .insurance_contracts
            .get(&victim_ship_id)
            .cloned();
    let (calculated_insurance_payout, ins_owner_id, ins_is_corp) = if is_pod || is_criminal || is_self_destruct {
        (0.0, victim_char_id, false)
    } else if let Some(c) = rt_contract {
        if c.end_unix > now_unix {
            let oid = if c.owner_id > 0 { c.owner_id } else { victim_char_id };
            let is_corp = ctx.db.with_snapshot(|s| s.corporations.contains_key(&oid));
            (c.payout_isk, oid, is_corp)
        } else {
            (compute_insurance_quote(base_price, 0.40).1, victim_char_id, false)
        }
    } else if let Some(c) = cs_contract {
        if c.end_unix > now_unix {
            let oid = if c.owner_id > 0 { c.owner_id } else { victim_char_id };
            let is_corp = ctx.db.with_snapshot(|s| s.corporations.contains_key(&oid));
            (c.payout_isk, oid, is_corp)
        } else {
            (compute_insurance_quote(base_price, 0.40).1, victim_char_id, false)
        }
    } else if insured_payout > 0.0 {
        (insured_payout, victim_char_id, false)
    } else {
        (compute_insurance_quote(base_price, 0.40).1, victim_char_id, false)
    };
    let hull_cs = ctx.build_type_combat_state(victim_ship_type_id);
    let hull_total_hp = (hull_cs.max_shield_hp + hull_cs.max_armor_hp + hull_cs.max_structure_hp).max(1.0);
    let hull_base_isk = if base_price > 0.0 {
        base_price
    } else if is_pod {
        1_000.0
    } else {
        230_000.0
    };

    let drop_threshold = if is_self_destruct {
        0.0
    } else {
        crate::services::security_mgr::insurgency_pvp_loot_drop_threshold(
            ctx,
            sys_id,
            attacker_char_id,
            victim_char_id,
            is_self_destruct,
        )
    };

    let active_fighter_item_ids: Vec<i64> = ctx
        .active_fighters
        .read()
        .ok()
        .map(|fighters| {
            fighters
                .values()
                .filter(|f| {
                    (f.owner_char_id == victim_char_id || f.parent_ship_id == victim_ship_id)
                        && !f.is_abandoned
                })
                .map(|f| f.item_id)
                .collect()
        })
        .unwrap_or_default();

    let (
        wreck_item_id,
        dropped_ids,
        destroyed_ids,
        dropped_entries,
        destroyed_entries,
        total_value_isk,
        ejected_cap_id,
        killmail_id,
        vic_corp,
        vic_all,
        att_corp,
        att_all,
        att_ship_type_id,
        ins_notify,
        victim_piloting_ship,
        nested_ship_ids,
    ) = ctx.db.try_with_mut_snapshot(|snap| {
        let ship = snap
            .items
            .get(&victim_ship_id)
            .cloned()
            .ok_or_else(|| format!("Ship {victim_ship_id} not found in world repository"))?;
        let cat_id = ctx
            .sde
            .types
            .get(&ship.type_id)
            .map(|t| t.category_id)
            .unwrap_or(6);
        if cat_id != 6 {
            return Err(format!(
                "Item {victim_ship_id} (type {}, category {cat_id}) is not a ship",
                ship.type_id
            ));
        }

        let victim_piloting_ship = snap
            .characters
            .get(&victim_char_id)
            .is_some_and(|v| {
                v.active_ship_id == victim_ship_id
                    && !snap.structures.contains_key(&v.active_ship_id)
            })
            || snap
                .sp6_lifecycle
                .disconnected_ships
                .get(&victim_char_id)
                .is_some_and(|d| d.ship_id == victim_ship_id);

        for &fighter_iid in &active_fighter_item_ids {
            if let Some(fit_it) = snap.items.get_mut(&fighter_iid) {
                if fit_it.location_id == victim_ship_id {
                    fit_it.location_id = sys_id as i64;
                    fit_it.flag = InventoryFlag::None;
                }
            }
        }

        let nested_ship_ids: Vec<i64> = snap
            .items
            .values()
            .filter(|i| {
                i.location_id == victim_ship_id
                    && ctx.sde.types.get(&i.type_id).is_some_and(|t| t.category_id == 6)
            })
            .map(|i| i.item_id)
            .collect();

        snap.next_entity_id += 1;
        let wreck_id = snap.next_entity_id;

        crate::sp6_lifecycle::record_wreck_or_container_meta_in_snap(
            snap, wreck_id, now_unix, is_criminal,
        );
        snap.items.insert(
            wreck_id,
            InventoryItemRecord {
                item_id: wreck_id,
                type_id: wreck_type_id,
                owner_id: victim_char_id,
                location_id: sys_id as i64,
                flag: if is_pod { InventoryFlag::None } else { InventoryFlag::Cargo },
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        snap.item_positions
            .insert(wreck_id, [victim_pos.x, victim_pos.y, victim_pos.z]);

        let courier_cids: Vec<i64> = snap
            .contracts
            .values()
            .filter(|c| {
                c.contract_type == "Courier"
                    && c.status == "InProgress"
                    && c.item_ids
                        .iter()
                        .any(|iid| is_item_descendant_of_in_snap(snap, *iid, victim_ship_id))
            })
            .map(|c| c.contract_id)
            .collect();
        for cid in courier_cids {
            snap.fail_courier_contract(cid, now_unix);
        }
        let wrap_ids: Vec<i64> = snap
            .items
            .values()
            .filter(|i| i.location_id == victim_ship_id && i.type_id == 3468)
            .map(|i| i.item_id)
            .collect();
        for wid in wrap_ids {
            let nested_ids: Vec<i64> = snap
                .items
                .values()
                .filter(|i| i.location_id == wid)
                .map(|i| i.item_id)
                .collect();
            for nid in nested_ids {
                if let Some(nit) = snap.items.get_mut(&nid) {
                    nit.location_id = victim_ship_id;
                    nit.owner_id = victim_char_id;
                    nit.flag = InventoryFlag::Cargo;
                }
            }
            snap.items.remove(&wid);
        }

        let escape_frigate_id: Option<i64> = if !is_pod && victim_piloting_ship {
            let has_feb = ctx
                .sde
                .types
                .get(&victim_ship_type_id)
                .map(|t| {
                    t.default_attributes.get(&3020).copied().unwrap_or(0.0) > 0.0
                        || matches!(t.group_id, 27 | 898 | 900)
                })
                .unwrap_or(false);
            if has_feb {
                snap.items
                    .values()
                    .find(|i| {
                        i.location_id == victim_ship_id
                            && i.singleton
                            && matches!(i.flag as u16, 177 | 179)
                            && ctx.sde.types.get(&i.type_id).is_some_and(|t| {
                                matches!(
                                    t.group_id,
                                    25 | 324 | 830 | 831 | 834 | 893 | 1283 | 1527 | 2001
                                )
                            })
                    })
                    .map(|i| i.item_id)
            } else {
                None
            }
        } else {
            None
        };

        let loot_out = resolve_destroyed_ship_contents_in_snap(
            snap,
            &ctx.sde,
            victim_ship_id,
            wreck_id,
            None,
            escape_frigate_id,
            is_pod,
            drop_threshold,
            |i| {
                matches!(
                    i.flag as u16,
                    4 | 5 | 11..=34 | 87 | 89 | 90 | 92..=94 | 125..=185
                )
            },
            |iid| {
                let entropy = if rng_seed != 0 {
                    rng_seed
                } else {
                    let mut rng_buf = [0u8; 8];
                    let _ = getrandom::getrandom(&mut rng_buf);
                    u64::from_le_bytes(rng_buf)
                };
                let mixed = (entropy ^ (iid as u64)).wrapping_mul(6364136223846793005);
                ((mixed ^ (mixed >> 32)) >> 32) as f64 / 4294967296.0
            },
        );
        let dropped = loot_out.dropped_ids;
        let destroyed = loot_out.destroyed_ids;
        let mut dropped_km = loot_out.dropped_km;
        let mut destroyed_km = loot_out.destroyed_km;
        let mut items_isk = loot_out.items_isk;
        if let Some(esc_id) = escape_frigate_id {
            if let Some(esc_item) = snap.items.get(&esc_id) {
                dropped_km.push(eve_db::KillmailItemEntry {
                    type_id: esc_item.type_id,
                    flag: esc_item.flag as u16,
                    qty_dropped: 0,
                    qty_destroyed: 0,
                    singleton: true,
                });
            }
        }
        if is_pod {
            let char_booster_items: Vec<i64> = snap
                .items
                .values()
                .filter(|i| i.location_id == victim_char_id && i.flag == InventoryFlag::Booster)
                .map(|i| i.item_id)
                .collect();
            for bid in char_booster_items {
                snap.items.remove(&bid);
            }
            let mut imp_tids: Vec<u32> = snap
                .active_implants
                .remove(&victim_char_id)
                .unwrap_or_default();
            let char_imp_items: Vec<(i64, u32)> = snap
                .items
                .values()
                .filter(|i| i.location_id == victim_char_id && i.flag == InventoryFlag::Implant)
                .map(|i| (i.item_id, i.type_id))
                .collect();
            for (iid, tid) in char_imp_items {
                snap.items.remove(&iid);
                if !imp_tids.contains(&tid) {
                    imp_tids.push(tid);
                }
            }
            for imp_tid in imp_tids {
                let imp_isk = ctx
                    .sde
                    .types
                    .get(&imp_tid)
                    .map(|t| t.base_price)
                    .filter(|&p| p > 0.0)
                    .unwrap_or(1_000.0);
                items_isk += imp_isk;
                destroyed_km.push(eve_db::KillmailItemEntry {
                    type_id: imp_tid,
                    flag: InventoryFlag::Implant as u16,
                    qty_dropped: 0,
                    qty_destroyed: 1,
                    singleton: true,
                });
            }
        }

        snap.items.remove(&victim_ship_id);
        snap.item_positions.remove(&victim_ship_id);
        snap.insurance_contracts.remove(&victim_ship_id);
        snap.sp6_lifecycle.ship_combat_hp.remove(&victim_ship_id);

        let cap_id = if !is_pod && victim_piloting_ship {
            if let Some(esc_id) = escape_frigate_id {
                if let Some(esc_item) = snap.items.get_mut(&esc_id) {
                    esc_item.location_id = sys_id as i64;
                    esc_item.flag = InventoryFlag::None;
                    esc_item.owner_id = victim_char_id;
                    esc_item.version += 1;
                }
                snap.item_positions
                    .insert(esc_id, [victim_pos.x, victim_pos.y, victim_pos.z]);
                let _ = crate::router::apply_character_transition_in_snap(
                    snap,
                    &ctx.sde,
                    victim_char_id,
                    crate::router::TransitionLocation::KeepCurrent,
                    Some(esc_id),
                    false,
                    false,
                );
                Some(esc_id)
            } else {
                snap.next_entity_id += 1;
                let cid = snap.next_entity_id;
                snap.items.insert(
                    cid,
                    InventoryItemRecord {
                        item_id: cid,
                        type_id: 670,
                        owner_id: victim_char_id,
                        location_id: sys_id as i64,
                        flag: InventoryFlag::None,
                        quantity: 1,
                        singleton: true,
                        version: 1,
                        dynamic_attributes: HashMap::new(),
                        source_mutaplasmid_id: None,
                        insured_payout_isk: 0.0,
                        is_blueprint_copy: false,
                        blueprint_runs_remaining: None,
                    },
                );
                snap.item_positions
                    .insert(cid, [victim_pos.x, victim_pos.y, victim_pos.z]);
                let _ = crate::router::apply_character_transition_in_snap(
                    snap,
                    &ctx.sde,
                    victim_char_id,
                    crate::router::TransitionLocation::KeepCurrent,
                    Some(cid),
                    false,
                    false,
                );
                Some(cid)
            }
        } else {
            None
        };

        let ins_notify = if calculated_insurance_payout > 0.0 {
            let payout_cents = IskCents::from_f64_isk(calculated_insurance_payout);
            let (journal_char_id, bal_after) = if ins_is_corp {
                if let Some(corp) = snap.corporations.get_mut(&ins_owner_id) {
                    let bal = corp.credit_division(1, payout_cents);
                    (ins_owner_id, bal)
                } else {
                    (ins_owner_id, payout_cents)
                }
            } else if let Some(vic) = snap.characters.get_mut(&ins_owner_id) {
                vic.balance = vic.balance.checked_add(payout_cents).unwrap_or(vic.balance);
                (ins_owner_id, vic.balance)
            } else {
                (ins_owner_id, payout_cents)
            };
            snap.append_journal_entry(eve_db::ledger::JournalEntryInput {
                owner_id: journal_char_id,
                entry_type: "insurance_payout",
                amount: payout_cents,
                balance_after: bal_after,
                reason: format!("Insurance payout for loss of ship {victim_ship_id}"),
                meta: eve_db::ledger::JournalMeta::new(
                    eve_db::ledger::ref_types::INSURANCE,
                    1_000_132,
                    journal_char_id,
                    now_unix,
                )
                .with_context(eve_db::ledger::JournalContext::Type, i64::from(victim_ship_type_id)),
            });
            snap.next_entity_id += 1;
            let mid = snap.next_entity_id;
            let subject = format!("Insurance Payout for Ship {victim_ship_id}");
            snap.mails.push(eve_db::MailRecord {
                message_id: mid,
                sender_id: 1_000_132,
                recipient_id: victim_char_id,
                subject: subject.clone(),
                body: format!(
                    "Pend Insurance has credited {:.2} ISK to your account following the destruction of ship {}.",
                    calculated_insurance_payout, victim_ship_id
                ),
            });
            snap.mail_timestamps.insert(mid, now_unix);
            snap.mail_assigned_labels.insert((victim_char_id, mid), vec![1]);
            Some((ins_is_corp, journal_char_id, bal_after.to_wire_f64(), mid, subject))
        } else {
            None
        };

        let total_val = hull_base_isk + items_isk;
        let (vic_c, vic_a) = snap
            .characters
            .get(&victim_char_id)
            .map(|c| (c.corporation_id, c.alliance_id))
            .unwrap_or((1_000_009, None));
        let (att_c, att_a, att_stid) = snap
            .characters
            .get(&attacker_char_id)
            .map(|c| {
                let st = snap
                    .items
                    .get(&c.active_ship_id)
                    .map(|i| i.type_id)
                    .unwrap_or(victim_ship_type_id);
                (c.corporation_id, c.alliance_id, st)
            })
            .unwrap_or((vic_c, vic_a, victim_ship_type_id));

        snap.next_entity_id += 1;
        let kid = snap.next_entity_id;
        snap.record_killmail(
            KillmailRecord {
                kill_id: kid,
                victim_character_id: victim_char_id,
                attacker_character_id: attacker_char_id,
                solar_system_id: sys_id,
                ship_type_id: victim_ship_type_id,
                total_value_isk: total_val,
                ..Default::default()
            },
            now_unix,
        );

        Ok::<_, String>((
            wreck_id,
            dropped,
            destroyed,
            dropped_km,
            destroyed_km,
            total_val,
            cap_id,
            kid,
            vic_c,
            vic_a,
            att_c,
            att_a,
            att_stid,
            ins_notify,
            victim_piloting_ship,
            nested_ship_ids,
        ))
    })?;

    crate::services::combat_crimewatch::with_mut_combat_runtime(ctx, |rt| {
        rt.insurance_contracts.remove(&victim_ship_id);
    });
    crate::services::inventory_fitting::fitting_containers_ships::write_container_ship_state(
        ctx,
        |st| {
            st.insurance_contracts.remove(&victim_ship_id);
        },
    );
    {
        let mut fittings = ctx
            .ship_fittings
            .write()
            .unwrap_or_else(|e| e.into_inner());
        fittings.remove(&victim_ship_id);
        for nsid in &nested_ship_ids {
            if Some(*nsid) != ejected_cap_id {
                fittings.remove(nsid);
            }
        }
    }
    let _ = drain_nested_ship_fittings(ctx, victim_ship_id, !is_pod);
    let _ = crate::services::notify_emitters_space_dungeons::flush_ship_all_ewar_sync(
        ctx,
        victim_char_id,
        Some(sys_id),
        victim_ship_id,
    );
    crate::services::fleet_social::fleet_social_finder_tactical::clear_destroyed_target_tag(
        ctx,
        victim_ship_id,
    );
    crate::sp6_lifecycle::abandon_drones_and_fighters_for_ship_sync(
        ctx,
        victim_char_id,
        victim_ship_id,
        sys_id,
    );
    crate::services::inventory_fitting::fitting_cyno_portal::on_cyno_ship_destroyed(
        ctx,
        victim_ship_id,
    );
    crate::services::doomsday_aoe_ops::cancel_aoe_superweapon_cycles_for_ship(ctx, victim_ship_id);
    crate::services::combat_crimewatch::with_mut_combat_runtime(ctx, |rt| {
        rt.record_destroyed_item_id(victim_ship_id);
    });

    // Commit ballpark state (remove destroyed ship ball, spawn wreck ball, spawn capsule/escape
    // ship ball, and queue incremental `RemoveBall` + `AddBalls2` handoff on `cid` without
    // `SetState`) BEFORE emitting any notifications (`ctx.notify`), because `ctx.notify`
    // immediately wakes `macho_listener`'s `notify_rx.recv()` loop (`#970`).
    crate::services::quasar_cosmetics_live::strip_ship_cosmetics_for_ships(&ctx.db, &[victim_ship_id]);
    crate::services::notify_emitters_seasons_tactical::on_deployable_deactivated(ctx, None, victim_ship_id);
    sys_handle.try_remove_ball(victim_ship_id);
    let (wreck_grp, wreck_cat) = if is_pod {
        ctx.sde
            .types
            .get(&25)
            .map(|t| (t.group_id, t.category_id))
            .unwrap_or((14, 2))
    } else {
        (186, 2)
    };
    sys_handle.try_spawn_ball_with_meta(
        Ball {
            id: wreck_item_id,
            type_id: wreck_type_id,
            position: victim_pos,
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: if is_pod { 80.0 } else { 10_000.0 },
            radius: if is_pod { 5.0 } else { 50.0 },
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        eve_destiny::CombatState {
            shield_hp: 0.0,
            max_shield_hp: 0.0,
            armor_hp: 0.0,
            max_armor_hp: 0.0,
            structure_hp: 500.0,
            max_structure_hp: 500.0,
            ..eve_destiny::CombatState::default()
        },
        SlimItemMeta {
            group_id: wreck_grp,
            category_id: wreck_cat,
            owner_id: victim_char_id,
            char_id: None,
            corp_id: -1,
            alliance_id: -1,
            security_status: 0.0,
            is_cloaked: false,
            name: ctx.sde.types.get(&wreck_type_id).map(|t| t.name.clone()),
            ..Default::default()
        },
    );
    if let Some(cid) = ejected_cap_id {
        let esc_type_id = ctx
            .db
            .with_snapshot(|s| s.items.get(&cid).map(|i| i.type_id))
            .unwrap_or(670);
        let esc_sde = ctx.sde.types.get(&esc_type_id);
        let esc_slim = ctx.build_ship_slim_meta(victim_char_id, esc_type_id);
        sys_handle.try_spawn_ball_with_meta(
            Ball {
                id: cid,
                type_id: esc_type_id,
                position: victim_pos,
                velocity: DVec3::ZERO,
                max_velocity: esc_sde
                    .and_then(|t| t.default_attributes.get(&37).copied())
                    .unwrap_or(335.0),
                inertia: esc_sde
                    .and_then(|t| t.default_attributes.get(&70).copied())
                    .unwrap_or(0.25),
                mass: esc_sde.map(|t| t.mass.max(1_000.0)).unwrap_or(16_000.0),
                radius: esc_sde.map(|t| t.radius.max(15.0)).unwrap_or(15.0),
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_ship_combat_state(victim_char_id, cid, esc_type_id),
            esc_slim,
        );
        sys_handle.try_handoff_in_space_observer(victim_ship_id, cid, &[wreck_item_id, cid]);
    }

    if let Some((ins_corp, owner_id, bal_wire, mid, subject)) = ins_notify {
        if ins_corp {
            let _ = ctx.notify(
                crate::router::NotificationTarget::Corporation(owner_id),
                "OnAccountChange",
                "corpid",
                vec![
                    eve_proto::PyRep::String("cash".into()),
                    eve_proto::PyRep::Integer(owner_id),
                    eve_proto::PyRep::Real(bal_wire),
                ],
            );
        } else {
            let _ = ctx.notify_account_change(owner_id, "cash", owner_id, bal_wire);
        }
        let sent = crate::services::mail_notify::SentMail {
            message_id: mid,
            sender_id: 1_000_132,
            to_character_ids: &[victim_char_id],
            to_list_id: None,
            to_corp_or_alliance_id: None,
            title: &subject,
            status_mask: crate::services::mail_notify::MAIL_STATUS_AUTOMATED,
        };
        crate::services::mail_notify::emit_mail_sent(ctx, &sent, &[victim_char_id]);
    }

    if !is_self_destruct && attacker_char_id > 0 && victim_char_id > 0 && attacker_char_id != victim_char_id {
        let _ = crate::services::combat_crimewatch::claim_player_bounty_on_kill(
            ctx,
            attacker_char_id,
            victim_char_id,
            victim_ship_type_id,
        );
        // Security status penalty for illegal ship or pod kill (R-SECLOSS-PER-ACTIVATION)
        if attacker_char_id != 1_000_125 {
            let sys_sec = ctx.sde.solar_systems.get(&sys_id).map(|s| s.security_status).unwrap_or(1.0);
            if sys_sec > 0.0 {
                let is_legal = ctx.db.with_snapshot(|snap| {
                    crate::services::combat_crimewatch::is_legal_war_or_suspect_target(snap, attacker_char_id, victim_char_id, now_unix)
                }) || {
                    let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
                    let vst = cw.get_or_create(victim_char_id);
                    vst.is_suspect(now_unix) || vst.is_criminal(now_unix)
                } || crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
                    rt.has_limited_engagement_with(victim_char_id, attacker_char_id, now_unix)
                });
                if !is_legal {
                    let is_pod_kill = matches!(victim_ship_type_id, 670 | 33_328);
                    let corr_stage5 = sys_sec < 0.45
                        && crate::services::insurgency_services::insurgency_system_corruption_stage(
                            ctx, sys_id,
                        ) >= 5;
                    if !(is_pod_kill && corr_stage5) {
                        let is_new_agg = crate::services::combat_crimewatch::crimewatch_timers::is_new_pvp_aggression(
                            ctx,
                            attacker_char_id,
                            victim_char_id,
                            is_pod_kill,
                            now_unix,
                        );
                        if is_new_agg {
                            crate::services::combat_crimewatch::crimewatch_timers::apply_illegal_pvp_sec_status_loss(
                                ctx,
                                attacker_char_id,
                                victim_char_id,
                                sys_id,
                                sys_sec,
                                is_pod_kill,
                                now_unix,
                            );
                        }
                        // Grant 30-day Kill Right on illegal Highsec ship/pod kills and Lowsec capsule kills
                        if sys_sec >= 0.45 || is_pod_kill {
                            let rt_expiries = crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
                                rt.kill_right_expires_unix.clone()
                            });
                            let has_kr = ctx.db.with_snapshot(|snap| {
                                snap.kill_rights.values().any(|kr| {
                                    kr.owner_id == victim_char_id
                                        && kr.target_id == attacker_char_id
                                        && !kr.is_active_suspect
                                        && rt_expiries
                                            .get(&kr.kill_right_id)
                                            .or_else(|| {
                                                snap.combat_crimewatch_state
                                                    .kill_right_expires_unix
                                                    .get(&kr.kill_right_id)
                                            })
                                            .is_some_and(|&exp| exp > now_unix)
                                })
                            });
                            if !has_kr {
                                let kr_id = ctx.db.with_mut_snapshot(|snap| {
                                    snap.next_entity_id += 1;
                                    snap.next_entity_id
                                });
                                crate::services::combat_crimewatch::create_kill_right(
                                    ctx,
                                    kr_id,
                                    victim_char_id,
                                    attacker_char_id,
                                    0.0,
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    let km_hash = crate::services::combat_crimewatch::compute_killmail_esi_hash(
        victim_char_id,
        attacker_char_id,
        victim_ship_type_id,
        now_unix,
    );
    crate::services::combat_crimewatch::with_mut_combat_runtime(ctx, |rt| {
        let mut km_attackers: Vec<eve_db::KillmailAttackerEntry> = Vec::new();
        for key in [victim_ship_id, victim_char_id] {
            if let Some(list) = rt.aggression_assist_log.remove(&key) {
                for a in list {
                    if now_unix.saturating_sub(a.last_action_unix)
                        <= crate::services::combat_crimewatch::KILLMAIL_ASSIST_WINDOW_SECS
                        && !km_attackers.iter().any(|x| x.character_id == a.attacker_char_id)
                    {
                        km_attackers.push(eve_db::KillmailAttackerEntry {
                            character_id: a.attacker_char_id,
                            corporation_id: a.corporation_id,
                            alliance_id: a.alliance_id,
                            ship_type_id: a.ship_type_id,
                            weapon_type_id: a.weapon_type_id,
                            damage_done: a.damage_done,
                            final_blow: a.attacker_char_id == attacker_char_id,
                            security_status: 0.0,
                        });
                    }
                }
            }
        }
        if !km_attackers.iter().any(|a| a.character_id == attacker_char_id) {
            km_attackers.push(eve_db::KillmailAttackerEntry {
                character_id: attacker_char_id,
                corporation_id: att_corp,
                alliance_id: att_all,
                ship_type_id: att_ship_type_id,
                weapon_type_id: att_ship_type_id,
                damage_done: hull_total_hp,
                final_blow: true,
                security_status: 0.0,
            });
        }
        let assist_dmg: f64 = km_attackers.iter().map(|a| a.damage_done).sum();
        rt.detailed_killmails.insert(
            killmail_id,
            eve_db::DetailedKillmail {
                kill_id: killmail_id,
                kill_time_unix: now_unix,
                solar_system_id: sys_id,
                victim_character_id: victim_char_id,
                victim_corporation_id: vic_corp,
                victim_alliance_id: vic_all,
                victim_ship_type_id,
                victim_damage_taken: assist_dmg.max(hull_total_hp),
                victim_position: [victim_pos.x, victim_pos.y, victim_pos.z],
                attackers: km_attackers,
                dropped_items: dropped_entries,
                destroyed_items: destroyed_entries,
                total_value_isk,
                killmail_hash: km_hash,
            },
        );
    });

    crate::services::notify_emitters_combat_ess_progression::emit_kill_notification(ctx, victim_char_id);
    crate::services::notify_emitters_combat_ess_progression::emit_kill_notification(ctx, attacker_char_id);
    if !is_pod {
        crate::services::notify_emitters_fleet_combat::emit_ship_death(ctx, victim_char_id);
    }

    if is_pod {
        let _ = crate::services::structure_control_ops::release_all_structure_control_for_character(
            ctx,
            None,
            victim_char_id,
        );
        crate::services::notify_emitters_combat_ess_progression::emit_pod_death(ctx, victim_char_id);
        crate::services::notify_emitters_combat_ess_progression::emit_player_pod_death(ctx, victim_char_id);
        crate::services::notify_emitters_combat_ess_progression::emit_pod_killed(ctx, victim_char_id, victim_ship_id);
        let cleared_zarzakh_gate = ctx.db.with_mut_snapshot(|snap| {
            snap.zarzakh_gate_locks
                .remove(&victim_char_id)
                .map(|(gid, _)| gid)
        });
        if let Some(locked_gate_id) = cleared_zarzakh_gate {
            crate::services::quasar_sov_services::publish_stargate_lock_deactivated_notice(
                &ctx.quasar_gateway,
                victim_char_id,
                30_100_000,
                locked_gate_id,
                now_unix,
            );
        }
        crate::services::station_structure::structure_lifecycle::prepare_home_station_respawn(
            ctx,
            victim_char_id,
        );
        let _ = ctx
            .db
            .respawn_character_at_home_station(victim_char_id, victim_ship_id, &ctx.sde);
        crate::services::notify_emitters_combat_ess_progression::emit_player_pod_respawn(ctx, victim_char_id);
    }

    crate::services::combat_crimewatch::combat_wrecks_killmails::register_container_lifetime(
        ctx,
        wreck_item_id,
        sys_id,
        wreck_type_id,
        victim_char_id,
        now_unix,
        is_criminal,
    );

    crate::sp6_lifecycle::with_mut_sp6_state(&ctx.db, |st, snap| {
        if is_pod {
            st.disconnected_ships.remove(&victim_char_id);
        } else if let Some(cid) = ejected_cap_id {
            let esc_tid = snap.items.get(&cid).map(|i| i.type_id).unwrap_or(670);
            if let Some(disc) = st.disconnected_ships.get_mut(&victim_char_id) {
                if disc.ship_id == victim_ship_id {
                    disc.ship_id = cid;
                    disc.ship_type_id = esc_tid;
                    disc.emergency_warp_dest = None;
                }
            }
        }
    });

    if let Some(cid) = ejected_cap_id {
        if let Some(cap_item) = ctx.db.get_item(cid) {
            let _ = ctx.notify_item_record_change(
                victim_char_id,
                &cap_item,
                vec![
                    (3, eve_proto::PyRep::Integer(0)),
                    (4, eve_proto::PyRep::Integer(0)),
                ],
            );
        }
        crate::services::notify_emitters_combat_ess_progression::emit_damage_state_change(
            ctx,
            victim_char_id,
            cid,
            1.0,
            1.0,
            1.0,
        );
    }

    if is_pod || victim_piloting_ship {
        let (new_sys_id, new_dock_id) = ctx
            .db
            .with_snapshot(|s| {
                s.characters
                    .get(&victim_char_id)
                    .map(|c| (c.solar_system_id, c.station_id.or(c.structure_id)))
            })
            .unwrap_or((sys_id, None));
        let death_reason = if is_pod { "podding" } else { "ship_loss" };
        crate::services::space_destiny::record_session_change_timer(
            ctx,
            victim_char_id,
            now_unix,
            new_sys_id,
            0,
            death_reason,
        );
        ctx.invalidate_bound_objects_for_session_transition(
            ctx_victim_for_invalidation(victim_char_id),
            new_sys_id,
            new_dock_id,
            death_reason,
        );

        if is_pod {
            crate::services::map_service::record_solar_system_visit(ctx, victim_char_id, new_sys_id);
        }
    }

    let total_damage = base_price.max(1000.0);
    crate::services::pvp_filament_mgr::record_proving_grounds_combat_result_if_queued(
        ctx,
        attacker_char_id,
        victim_char_id,
        sys_id,
        total_damage,
    );
    let fw_net_isk = if is_pod {
        0.0
    } else {
        (total_value_isk - calculated_insurance_payout).max(0.0)
    };
    crate::services::fw_gameplay::on_player_ship_destroyed_in_combat_detailed(
        ctx,
        sys_id,
        attacker_char_id,
        victim_char_id,
        victim_ship_type_id,
        fw_net_isk,
        killmail_id,
    );
    if !is_pod && attacker_char_id > 0 && attacker_char_id != victim_char_id {
        crate::services::social_corp_fleet::social_corp_projects::record_server_project_activity(
            ctx,
            attacker_char_id,
            &["DestroyCapsuleerShips"],
            1,
            Some(sys_id as i64),
        );
    }

    Ok(DeathPipelineResult {
        wreck_item_id,
        wreck_type_id,
        loot_dropped_item_ids: dropped_ids,
        loot_destroyed_item_ids: destroyed_ids,
        ejected_capsule_id: ejected_cap_id,
        insurance_payout_isk: calculated_insurance_payout,
        killmail_id,
    })
}

#[inline]
const fn ctx_victim_for_invalidation(victim_char_id: i64) -> i64 {
    victim_char_id
}

/// Canonical unified ball destruction router (`#918`, `#835`, `#858`).
///
/// Routes any ball reaching 0 structure HP — whether from a weapon/turret module cycle,
/// missile/smartbomb/bomb/doomsday impact, or NPC AI tick — through one entry point:
/// 1. Active player drones in space (`rt.active_drones_in_space`) -> cleaned up without ship death.
/// 2. Extended NPC entities (`rt.npc_entities`) -> cleaned up without player ship death.
/// 3. Ships (`category_id == 6` in `snap.items`, whether piloted, disconnected, or unpiloted) ->
///    routed through `execute_death_pipeline_sync` (producing wreck, loot roll, insurance payout,
///    and killmail; only piloted/disconnected ships spawn a capsule or transition the character).
/// 4. Non-ship space objects (fighters, wrecks, jetcans, deployables, POCOs) -> cleaned up via
///    `cleanup_destroyed_non_ship_ball_sync`.
pub fn on_ball_destroyed_sync(
    ctx: &ServerContext,
    sys_id: u32,
    ball_id: i64,
    killer_id_opt: Option<i64>,
    fallback_pos: DVec3,
) -> Result<Option<DeathPipelineResult>, String> {
    if ctx.db.with_snapshot(|s| s.structures.contains_key(&ball_id)) {
        return Ok(None);
    }
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let pos = sys_handle
        .get_ball_position_sync(ball_id)
        .or_else(|| {
            ctx.db
                .with_snapshot(|s| s.item_positions.get(&ball_id).copied())
                .map(DVec3::from_array)
        })
        .unwrap_or(fallback_pos);

    // 1. Active player drone in space
    if cleanup_destroyed_player_drone_sync(ctx, &sys_handle, ball_id) {
        return Ok(None);
    }

    // 2. Extended NPC entity
    let removed_ext_npc = if crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
        rt.npc_entities.contains_key(&ball_id)
    }) {
        crate::services::combat_crimewatch::with_mut_combat_runtime(ctx, |rt| {
            let removed = rt.npc_entities.remove(&ball_id).is_some();
            if removed {
                rt.record_destroyed_item_id(ball_id);
            }
            removed
        })
    } else {
        false
    };
    if removed_ext_npc {
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.remove(&ball_id);
            snap.item_positions.remove(&ball_id);
            snap.sp6_lifecycle.space_object_meta.remove(&ball_id);
        });
        sys_handle.try_remove_ball(ball_id);
        return Ok(None);
    }

    // 3. Ship (`category_id == 6` in `snap.items`, piloted, disconnected, or unpiloted)
    let ship_victim_and_killer_opt = ctx.db.with_snapshot(|snap| {
        let item = snap.items.get(&ball_id)?;
        let is_ship = ctx
            .sde
            .types
            .get(&item.type_id)
            .is_some_and(|t| t.category_id == 6);
        if !is_ship {
            return None;
        }
        let vic = snap
            .characters
            .values()
            .find(|c| c.active_ship_id == ball_id)
            .map(|c| c.character_id)
            .or_else(|| {
                snap.sp6_lifecycle
                    .disconnected_ships
                    .values()
                    .find(|d| d.ship_id == ball_id)
                    .map(|d| d.char_id)
            })
            .unwrap_or(item.owner_id);
        let resolved_killer = killer_id_opt.map(|kid| {
            snap.characters
                .get(&kid)
                .map(|c| c.character_id)
                .or_else(|| {
                    snap.characters
                        .values()
                        .find(|c| c.active_ship_id == kid)
                        .map(|c| c.character_id)
                })
                .unwrap_or(kid)
        });
        Some((vic, resolved_killer))
    });

    if let Some((victim_char_id, resolved_killer_opt)) = ship_victim_and_killer_opt {
        let now_unix = ctx.now_unix();
        let direct_player_killer = resolved_killer_opt.filter(|&k| {
            k >= 90_000_000
                && !crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
                    rt.npc_entities.contains_key(&k)
                })
        });
        let final_blow_id = direct_player_killer.unwrap_or_else(|| {
            crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
                let mut latest: Option<(u64, i64)> = None;
                for key in [ball_id, victim_char_id] {
                    if let Some(list) = rt.aggression_assist_log.get(&key) {
                        for e in list {
                            if now_unix.saturating_sub(e.last_action_unix)
                                <= crate::services::combat_crimewatch::KILLMAIL_ASSIST_WINDOW_SECS
                                && e.attacker_char_id != victim_char_id
                                && latest.is_none_or(|(ts, _)| e.last_action_unix >= ts)
                            {
                                latest = Some((e.last_action_unix, e.attacker_char_id));
                            }
                        }
                    }
                }
                latest.map(|(_, cid)| cid)
            })
            .unwrap_or_else(|| resolved_killer_opt.unwrap_or(0))
        });

        let death_res = execute_death_pipeline_sync(
            ctx,
            sys_id,
            victim_char_id,
            ball_id,
            final_blow_id,
            pos,
            now_unix,
        )?;
        return Ok(Some(death_res));
    }

    // 4. Non-ship space object (fighter, wreck, jetcan, deployable, POCO)
    cleanup_destroyed_non_ship_ball_sync(ctx, sys_id, ball_id);
    Ok(None)
}

/// Async counterpart of [`on_ball_destroyed_sync`] (`#918`, `#835`, `#858`).
pub async fn on_ball_destroyed(
    ctx: &ServerContext,
    sys_id: u32,
    ball_id: i64,
    killer_id_opt: Option<i64>,
    fallback_pos: DVec3,
) -> Result<Option<DeathPipelineResult>, String> {
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let pos = sys_handle
        .get_ball_snapshot(ball_id)
        .await
        .ok()
        .flatten()
        .map(|(_, b)| b.position)
        .unwrap_or(fallback_pos);
    on_ball_destroyed_sync(ctx, sys_id, ball_id, killer_id_opt, pos)
}

/// Synchronous cleanup of a destroyed non-ship ball (fighter, wreck, jetcan, deployable, POCO).
/// Cleans up a destroyed player drone from runtime combat state, inventory, and the
/// solar system `Ballpark`, and emits V24 destruction notifications (`OnItemChange`
/// tombstone, `OnDroneStateChange(0, 0, 0, ...)`, `OnDroneActivityChange(0, None)`,
/// and `OnDroneControlLost`) so the client removes the drone from `michelle.stateByDroneID`,
/// `DronesWindow`, and `TargetInBar` (`#1008`, `#1020`).
pub(crate) fn cleanup_destroyed_player_drone_sync(
    ctx: &ServerContext,
    sys_handle: &crate::system_actor::SystemHandle,
    ball_id: i64,
) -> bool {
    let removed_drone = crate::services::combat_crimewatch::with_mut_combat_runtime(ctx, |rt| {
        let d = rt.active_drones_in_space.remove(&ball_id);
        if let Some(ref drone) = d {
            rt.active_drone_miners.remove(&ball_id);
            rt.drone_next_ready_unix.remove(&ball_id);
            rt.record_destroyed_item_id(drone.ball_id);
            rt.record_destroyed_item_id(drone.drone_item_id);
        }
        d
    });
    let Some(d) = removed_drone else {
        return false;
    };
    let removed_item = ctx.db.with_mut_snapshot(|snap| {
        let item = snap.items.remove(&d.drone_item_id);
        snap.item_positions.remove(&d.ball_id);
        snap.sp6_lifecycle.space_object_meta.remove(&d.ball_id);
        item
    });
    sys_handle.try_remove_ball(ball_id);
    if d.owner_char_id > 0 {
        if let Some(ref old_item) = removed_item {
            ctx.emit_item_deleted_routed(d.owner_char_id, old_item);
        }
        ctx.notify(
            NotificationTarget::Character(d.owner_char_id),
            "OnDroneStateChange",
            "charid",
            vec![
                PyRep::Integer(d.drone_item_id),
                PyRep::Integer(0),
                PyRep::Integer(0),
                PyRep::Integer(0),
                PyRep::Integer(d.type_id as i64),
                PyRep::Integer(0),
                PyRep::None,
            ],
        );
        crate::services::notify_emitters_space_dungeons::emit_drone_activity_change(
            ctx,
            d.owner_char_id,
            d.drone_item_id,
            0,
            None,
        );
        ctx.notify(
            NotificationTarget::Character(d.owner_char_id),
            "OnDroneControlLost",
            "charid",
            vec![PyRep::Integer(d.drone_item_id)],
        );
    }
    true
}

pub fn cleanup_destroyed_non_ship_ball_sync(ctx: &ServerContext, sys_id: u32, ball_id: i64) {
    let sys_handle = ctx.systems.get_or_spawn(sys_id);

    if cleanup_destroyed_player_drone_sync(ctx, &sys_handle, ball_id) {
        return;
    }

    let removed_fighter = if let Ok(mut fighters) = ctx.active_fighters.write() {
        let key = fighters
            .iter()
            .find(|(_, f)| f.ball_id == ball_id || f.item_id == ball_id)
            .map(|(k, _)| *k);
        key.and_then(|k| fighters.remove(&k))
    } else {
        None
    };
    if let Some(f) = removed_fighter {
        if let Ok(mut loads) = ctx.fighter_tube_loads.write() {
            loads.remove(&(f.parent_ship_id, f.tube_index));
        }
        crate::services::combat_crimewatch::with_mut_combat_runtime(ctx, |rt| {
            rt.record_destroyed_item_id(f.ball_id);
            rt.record_destroyed_item_id(f.item_id);
        });
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.remove(&f.item_id);
            snap.item_positions.remove(&f.ball_id);
            snap.sp6_lifecycle.space_object_meta.remove(&f.ball_id);
        });
        sys_handle.try_remove_ball(ball_id);
        return;
    }

    let is_poco = ctx.db.with_snapshot(|snap| {
        snap.corp_customs_offices
            .values()
            .any(|v| v.iter().any(|o| o.office_id == ball_id))
    });
    if is_poco {
        crate::services::combat_crimewatch::with_mut_combat_runtime(ctx, |rt| {
            rt.record_destroyed_item_id(ball_id);
        });
        ctx.db.with_mut_snapshot(|snap| {
            for v in snap.corp_customs_offices.values_mut() {
                v.retain(|o| o.office_id != ball_id);
            }
            snap.items.remove(&ball_id);
            snap.item_positions.remove(&ball_id);
            snap.sp6_lifecycle.space_object_meta.remove(&ball_id);
        });
        sys_handle.try_remove_ball(ball_id);
        return;
    }

    let is_destroyable_space_item = ctx.db.with_snapshot(|snap| {
        snap.items.get(&ball_id).is_some_and(|it| {
            it.location_id == i64::from(sys_id)
                && ctx
                    .sde
                    .types
                    .get(&it.type_id)
                    .is_some_and(|t| t.category_id != 6)
        })
    });
    if is_destroyable_space_item {
        crate::services::combat_crimewatch::with_mut_combat_runtime(ctx, |rt| {
            rt.container_lifetimes.remove(&ball_id);
            rt.record_destroyed_item_id(ball_id);
        });
        crate::services::notify_emitters_seasons_tactical::on_deployable_deactivated(
            ctx,
            None,
            ball_id,
        );
        ctx.db.with_mut_snapshot(|snap| {
            let child_ids: Vec<i64> = snap
                .items
                .values()
                .filter(|it| it.location_id == ball_id)
                .map(|it| it.item_id)
                .collect();
            for cid in child_ids {
                snap.items.remove(&cid);
                snap.item_positions.remove(&cid);
            }
            snap.items.remove(&ball_id);
            snap.item_positions.remove(&ball_id);
            snap.sp6_lifecycle.space_object_meta.remove(&ball_id);
        });
        sys_handle.try_remove_ball(ball_id);
    }
}

/// Executes the full EVE death pipeline when a ship's structure HP reaches 0:
/// 1. SDE wreck type resolution (`ctx.sde.wrecks` or fallback 26557).
/// 2. 50% loot drop / destroyed roll for all fitted and cargo items.
/// 3. Capsule/pod ejection with session ship update (or clone respawn if pod was destroyed).
/// 4. Insurance payout calculation and wallet balance crediting.
/// 5. Killmail generation and persistence.
/// 6. Physical wreck and capsule ball spawning in the solar system Ballpark.
pub async fn execute_death_pipeline(
    ctx: &ServerContext,
    sys_id: u32,
    victim_char_id: i64,
    victim_ship_id: i64,
    attacker_char_id: i64,
    victim_pos: DVec3,
    rng_seed: u64,
) -> Result<DeathPipelineResult, String> {
    execute_death_pipeline_sync(
        ctx,
        sys_id,
        victim_char_id,
        victim_ship_id,
        attacker_char_id,
        victim_pos,
        rng_seed,
    )
}

/// Triggers self-destruct for a ship or capsule in space, executing the canonical death pipeline
/// with the character as their own attacker (or the most recent PvP attacker within the 15-minute
/// assist window) (`feat:ships.self-destruct`).
pub async fn execute_self_destruct(
    ctx: &ServerContext,
    sys_id: u32,
    char_id: i64,
    ship_id: i64,
    pos: DVec3,
    rng_seed: u64,
) -> Result<DeathPipelineResult, String> {
    let now_unix = ctx.now_unix();
    let final_blow_char_id = crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
        let mut latest: Option<(u64, i64)> = None;
        for key in [ship_id, char_id] {
            if let Some(list) = rt.aggression_assist_log.get(&key) {
                for e in list {
                    if now_unix.saturating_sub(e.last_action_unix)
                        <= crate::services::combat_crimewatch::KILLMAIL_ASSIST_WINDOW_SECS
                        && e.attacker_char_id != char_id
                        && latest.is_none_or(|(ts, _)| e.last_action_unix >= ts)
                    {
                        latest = Some((e.last_action_unix, e.attacker_char_id));
                    }
                }
            }
        }
        latest.map(|(_, cid)| cid).unwrap_or(char_id)
    });
    execute_death_pipeline_sync_inner(
        ctx,
        sys_id,
        char_id,
        ship_id,
        final_blow_char_id,
        pos,
        rng_seed,
        true,
    )
}

/// Validates live ship ownership, active ship, undocked state, and system presence before
/// executing a scheduled self-destruct, denying uncontested self-destruct insurance & loot
/// while crediting recent PvP aggressors within the assist window (#539).
pub fn execute_live_self_destruct_sync(
    ctx: &ServerContext,
    sys_id: u32,
    char_id: i64,
    ship_id: i64,
    pos: DVec3,
    now_unix: u64,
) -> Result<Option<DeathPipelineResult>, String> {
    let still_valid = crate::sp6_lifecycle::with_mut_sp6_state(&ctx.db, |st, snap| {
        st.self_destructs.remove(&ship_id);
        let Some(ship) = snap.items.get(&ship_id) else {
            return false;
        };
        if ship.owner_id != char_id || ship.location_id != i64::from(sys_id) {
            return false;
        }
        let Some(ch) = snap.characters.get(&char_id) else {
            return false;
        };
        ch.solar_system_id == sys_id
            && ch.station_id.is_none()
            && ch.structure_id.is_none()
            && ch.active_ship_id == ship_id
    });
    crate::services::combat_crimewatch::with_mut_combat_runtime(ctx, |rt| {
        rt.self_destruct_countdowns.remove(&ship_id);
    });
    if !still_valid {
        return Ok(None);
    }

    let last_aggressor = crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
        let mut latest: Option<(u64, i64)> = None;
        for key in [ship_id, char_id] {
            if let Some(list) = rt.aggression_assist_log.get(&key) {
                for e in list {
                    if now_unix.saturating_sub(e.last_action_unix)
                        <= crate::services::combat_crimewatch::KILLMAIL_ASSIST_WINDOW_SECS
                        && e.attacker_char_id > 0
                        && e.attacker_char_id != char_id
                        && latest.is_none_or(|(ts, _)| e.last_action_unix >= ts)
                    {
                        latest = Some((e.last_action_unix, e.attacker_char_id));
                    }
                }
            }
        }
        latest.map(|(_, cid)| cid)
    });
    let effective_attacker = last_aggressor.unwrap_or(char_id);
    let is_uncontested = last_aggressor.is_none();

    execute_death_pipeline_sync_inner(
        ctx,
        sys_id,
        char_id,
        ship_id,
        effective_attacker,
        pos,
        now_unix,
        is_uncontested,
    )
    .map(Some)
}

/// Tracks 2-minute self-destruct countdown timers and cancellations (`feat:ships.self-destruct`).
#[derive(Debug, Default, Clone)]
pub struct SelfDestructManager {
    /// Active self-destruct sequences: `ship_id -> (character_id, system_id, detonation_unix)`
    pub active_timers: HashMap<i64, (i64, u32, u64)>,
}

impl SelfDestructManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Initiates a 2-minute (120s) self-destruct sequence on a ship.
    pub fn start_self_destruct(
        &mut self,
        ship_id: i64,
        char_id: i64,
        sys_id: u32,
        now_unix: u64,
    ) -> u64 {
        let detonation_time = now_unix.saturating_add(120);
        self.active_timers.insert(ship_id, (char_id, sys_id, detonation_time));
        detonation_time
    }

    /// Cancels an active self-destruct sequence (`AbortSelfDestruct`). Returns true if a timer was cancelled.
    pub fn abort_self_destruct(&mut self, ship_id: i64) -> bool {
        self.active_timers.remove(&ship_id).is_some()
    }

    /// Returns true if the ship is currently counting down to self-destruct.
    pub fn is_self_destructing(&self, ship_id: i64) -> bool {
        self.active_timers.contains_key(&ship_id)
    }

    /// Returns remaining seconds until detonation, if self-destruct is active.
    pub fn remaining_seconds(&self, ship_id: i64, now_unix: u64) -> Option<u64> {
        self.active_timers
            .get(&ship_id)
            .map(|&(_, _, det)| det.saturating_sub(now_unix))
    }

    /// Detonates any ships whose self-destruct timer has expired.
    pub async fn step_timers(
        &mut self,
        ctx: &ServerContext,
        now_unix: u64,
        rng_seed: u64,
    ) -> Vec<(i64, Result<DeathPipelineResult, String>)> {
        let mut ready = Vec::new();
        self.active_timers.retain(|&ship_id, &mut (char_id, sys_id, det_unix)| {
            if now_unix >= det_unix {
                ready.push((ship_id, char_id, sys_id));
                false
            } else {
                true
            }
        });

        let mut results = Vec::new();
        for (ship_id, char_id, sys_id) in ready {
            let sys_handle = ctx.systems.get_or_spawn(sys_id);
            let pos = sys_handle
                .get_ball_snapshot(ship_id)
                .await
                .ok()
                .flatten()
                .map(|(_, b)| b.position)
                .unwrap_or(DVec3::ZERO);
            let res = execute_self_destruct(ctx, sys_id, char_id, ship_id, pos, rng_seed).await;
            results.push((ship_id, res));
        }
        results
    }
}

#[cfg(test)] #[path = "death_pipeline_unit_tests.rs"]
mod death_pipeline_unit_tests;


