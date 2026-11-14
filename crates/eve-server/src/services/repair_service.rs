//! `repair_service` — Station & Upwell structure repairshop (`repairSvc.GetRepairQuotes`,
//! `repairSvc.RepairItemsInStation`, `repairSvc.RepairItemsInStructure`).

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::station_structure::structure_lifecycle as life;
use crate::services::station_structure::structure_upwell_ops::{
    load_ext_state, StructureExtendedState,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};

fn ship_combat_damage(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    ship_id: i64,
    max_hull: f64,
    max_armor: f64,
    max_shield: f64,
) -> (f64, bool) {
    let type_id = snap.items.get(&ship_id).map(|i| i.type_id).unwrap_or(0);
    let unified = crate::sp6_lifecycle::read_unified_ship_damage_state(
        ctx,
        snap,
        ship_id,
        type_id,
        Some((max_hull, max_armor, max_shield)),
    );
    (
        unified.hull_damage + unified.armor_damage + unified.shield_damage,
        unified.shield_damage > 0.0,
    )
}

fn item_repair_metrics(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    item: &eve_db::InventoryItemRecord,
) -> (f64, f64, f64) {
    let mut dmg = item
        .dynamic_attributes
        .get(&eve_sde::attr_ids::DAMAGE)
        .copied()
        .unwrap_or(0.0)
        + item
            .dynamic_attributes
            .get(&eve_sde::attr_ids::ARMOR_DAMAGE)
            .copied()
            .unwrap_or(0.0);
    if let Ok(fits) = ctx.ship_fittings.read() {
        if let Some(fit) = fits.get(&item.location_id) {
            if let Some(m) = fit.modules.values().find(|m| m.item_id == item.item_id) {
                dmg = dmg.max(m.damage_hp);
            }
        }
    }
    let t_opt = ctx.sde.types.get(&item.type_id);
    let is_ship = t_opt.is_some_and(|t| t.category_id == 6);
    let max_hull = t_opt
        .and_then(|t| t.default_attributes.get(&eve_sde::attr_ids::HP_STRUCT).copied())
        .unwrap_or(0.0);
    let max_armor = t_opt
        .and_then(|t| t.default_attributes.get(&eve_sde::attr_ids::ARMOR_HP).copied())
        .unwrap_or(0.0);
    let max_shield = t_opt
        .and_then(|t| t.default_attributes.get(&eve_sde::attr_ids::SHIELD_CAPACITY).copied())
        .unwrap_or(0.0);
    let mut has_shield_dmg = false;
    if is_ship {
        let (combat_dmg, shield_damaged) =
            ship_combat_damage(ctx, snap, item.item_id, max_hull, max_armor, max_shield);
        dmg = dmg.max(combat_dmg);
        has_shield_dmg = shield_damaged;
    }
    let max_hp = if t_opt.is_some() {
        let base = max_hull + max_armor + if has_shield_dmg { max_shield } else { 0.0 };
        base.max(40.0)
    } else {
        100.0
    };
    // DERIVED: linear basePrice repair scaling factors (7.5e-6 per HP for ships, 1.25e-3 per HP
    // for modules/drones); live EVE Online computes repair quotes from material/market valuation.
    let base_price = t_opt
        .map(|t| t.base_price)
        .filter(|p| *p > 0.0)
        .unwrap_or(10_000.0);
    let repair_modifier = if is_ship { 7.5e-6 } else { 1.25e-3 };
    let cost_per_unit = (base_price * repair_modifier).max(0.01);
    (dmg, max_hp, cost_per_unit)
}

fn validate_structure_repair_access(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    _ext: &StructureExtendedState,
    sid: i64,
    char_id: i64,
    now: u64,
) -> Result<(), BridgeRpcError> {
    crate::services::structure_profile_services::require_structure_service_access_in_snap(
        Some(ctx),
        snap,
        sid,
        char_id,
        crate::services::structure_profile_services::StructureServiceKind::Repair,
        now,
    )?;
    Ok(())
}

fn is_repairable_ship_child_flag(flag: eve_db::InventoryFlag) -> bool {
    matches!(flag as u16, 11..=34 | 87 | 92..=94 | 125..=132 | 158)
}

fn check_item_repair_ownership_and_role(
    snap: &eve_db::WorldStateSnapshot,
    it: &eve_db::InventoryItemRecord,
    loc_id: Option<i64>,
    char_id: i64,
    corp_id: Option<i64>,
    active_ship: Option<i64>,
    corp_roles_at_loc: Option<(bool, u64)>,
) -> Result<(), BridgeRpcError> {
    if it.owner_id == char_id {
        return Ok(());
    }
    if Some(it.owner_id) != corp_id {
        return Err(BridgeRpcError {
            code: "NotItemOwner".into(),
            message: format!("Character {char_id} does not own item {}", it.item_id),
            client_error: None,
        });
    }
    if Some(it.item_id) == active_ship || Some(it.location_id) == active_ship {
        return Ok(());
    }
    let (is_ceo, roles) = corp_roles_at_loc.unwrap_or((false, 0));
    if is_ceo || (roles & eve_db::corp_roles::ROLE_DIRECTOR) != 0 {
        return Ok(());
    }
    let eff_flag = if Some(it.location_id) == loc_id {
        it.flag
    } else {
        snap.items.get(&it.location_id).map(|p| p.flag).unwrap_or(it.flag)
    };
    let f = eff_flag as i64;
    if (116..=122).contains(&f) {
        let div_idx = ((f - 116) as usize).min(6);
        let req_mask = eve_db::corp_roles::ROLE_HANGAR_CAN_TAKE[div_idx]
            | eve_db::corp_roles::ROLE_CONTAINER_CAN_TAKE[div_idx];
        if (roles & req_mask) != 0 {
            return Ok(());
        }
        return Err(BridgeRpcError {
            code: "CorpRoleDenied".into(),
            message: format!(
                "Character {char_id} lacks corporate hangar division {} access role to repair item {}",
                div_idx + 1,
                it.item_id
            ),
            client_error: None,
        });
    }
    Err(BridgeRpcError {
        code: "CorpRoleDenied".into(),
        message: format!(
            "Character {char_id} lacks Director role to repair corporate item {}",
            it.item_id
        ),
        client_error: None,
    })
}

fn resolve_structure_repair_location(
    ctx: &ServerContext,
    session: &SessionState,
    char_id: i64,
) -> Option<i64> {
    if let Some(sid) = session.structure_id {
        return Some(sid);
    }
    if let Some(sta_id) = session.station_id.filter(|&s| s >= 1_000_000_000_000) {
        return Some(sta_id);
    }
    let (sys_id, ship_id) = (session.solar_system_id?, session.ship_id?);
    let sys = ctx.systems.get_or_spawn(sys_id);
    let (tethered, ship_pos) = {
        let bp = sys.ballpark.read().ok()?;
        (
            bp.combat_states.get(&ship_id).is_some_and(|cs| cs.is_tethered),
            bp.balls.get(&ship_id).map(|b| b.position)?,
        )
    };
    if !tethered {
        return None;
    }
    let now = ctx.now_unix();
    ctx.db.with_snapshot(|snap| {
        let ext = load_ext_state(snap);
        snap.structures
            .values()
            .filter(|st| {
                if st.solar_system_id != sys_id {
                    return false;
                }
                if validate_structure_repair_access(ctx, snap, &ext, st.structure_id, char_id, now)
                    .is_err()
                {
                    return false;
                }
                let max_range = crate::services::station_structure::structure_lifecycle::tether_range_m(ctx, st.type_id);
                max_range > 0.0
                    && glam::DVec3::from_array(st.position).distance(ship_pos) <= max_range
            })
            .min_by(|a, b| {
                let da = glam::DVec3::from_array(a.position).distance(ship_pos);
                let db = glam::DVec3::from_array(b.position).distance(ship_pos);
                da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|s| s.structure_id)
    })
}

fn resolve_structure_repair_tax_bps(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    repair_sid: Option<i64>,
    char_id: i64,
    now: u64,
) -> Result<i64, BridgeRpcError> {
    let Some(sid) = repair_sid else {
        return Ok(0);
    };
    if sid < 1_000_000_000_000 && !snap.structures.contains_key(&sid) {
        return Ok(0);
    }
    let bps = crate::services::structure_profile_services::require_structure_service_access_in_snap(
        Some(ctx),
        snap,
        sid,
        char_id,
        crate::services::structure_profile_services::StructureServiceKind::Repair,
        now,
    )?
    .unwrap_or(0);
    Ok(bps.max(0))
}

/// `repairSvc.GetRepairQuotes(itemIDs)` (`base_repairshop.pyj:65`): returns a `Dict` mapping each
/// requested `itemID` to a `List` of `KeyVal` damage quote rows (`itemID`, `typeID`, `groupID`,
/// `damage`, `maxHealth`, `costToRepairOneUnitOfDamage`, `totalRepairCost`).
pub fn handle_repair_get_quotes(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    life::sync_all_structures(ctx);
    if let Some(station_id) = session.station_id.or(session.structure_id) {
        if station_id < 1_000_000_000_000
            && !ctx.db.with_snapshot(|s| s.structures.contains_key(&station_id))
            && !crate::services::station_structure::npc_station_offers_service(&ctx.sde, station_id, 13)
        {
            return Err(BridgeRpcError {
                code: "StationServiceUnavailable".into(),
                message: format!("Station {station_id} does not offer repair facilities"),
                client_error: None,
            });
        }
    }
    let mut item_ids: Vec<i64> = match args.first() {
        Some(PyRep::List(list) | PyRep::Tuple(list)) => {
            list.iter().filter_map(|v| v.as_i64()).collect()
        }
        Some(PyRep::Integer(id)) => vec![*id],
        _ => Vec::new(),
    };
    if item_ids.is_empty() {
        if let Some(sid) = session.ship_id {
            item_ids.push(sid);
        }
    }
    let repair_sid = resolve_structure_repair_location(ctx, session, char_id);
    let dock_loc = session.station_id.or(session.structure_id);
    if dock_loc.is_none() && repair_sid.is_none() {
        return Err(BridgeRpcError {
            code: "NotInStation".into(),
            message: "Must be docked at a station or structure, or tethered to an Upwell structure".into(),
            client_error: None,
        });
    }
    let active_ship = session.ship_id;
    let corp_id = session.corporation_id.or_else(|| {
        ctx.db.with_snapshot(|snap| snap.characters.get(&char_id).map(|c| c.corporation_id))
    });
    let role_loc = dock_loc.or(repair_sid);
    let corp_roles_at_loc = corp_id.map(|cid| {
        crate::services::inventory_fitting::get_effective_corp_roles_at_location(
            ctx,
            cid,
            char_id,
            role_loc,
        )
    });
    let now = ctx.now_unix();
    let dict_entries = ctx.db.with_snapshot(|snap| -> Result<_, BridgeRpcError> {
        let tax_bps = resolve_structure_repair_tax_bps(ctx, snap, repair_sid, char_id, now)?;
        let is_upwell = repair_sid.is_some_and(|s| s >= 1_000_000_000_000 || snap.structures.contains_key(&s));
        let mut out = Vec::with_capacity(item_ids.len());
        for req_id in item_ids {
            let Some(root) = snap.items.get(&req_id) else {
                out.push((PyRep::Integer(req_id), PyRep::List(Vec::new())));
                continue;
            };
            check_item_repair_ownership_and_role(
                snap,
                root,
                role_loc,
                char_id,
                corp_id,
                active_ship,
                corp_roles_at_loc,
            )?;
            if dock_loc.is_none() {
                let on_active = Some(root.item_id) == active_ship
                    || (Some(root.location_id) == active_ship && is_repairable_ship_child_flag(root.flag));
                if !on_active {
                    return Err(BridgeRpcError {
                        code: "NotDockedInStructure".into(),
                        message: "Must be docked inside the structure to quote or repair hangar items".into(),
                        client_error: None,
                    });
                }
            } else if Some(root.item_id) != active_ship && Some(root.location_id) != dock_loc {
                let Some(parent) = snap.items.get(&root.location_id) else {
                    return Err(BridgeRpcError {
                        code: "ItemNotAtLocation".into(),
                        message: format!("Item {req_id} is not located at dockable location"),
                        client_error: None,
                    });
                };
                let parent_is_ship = ctx.sde.types.get(&parent.type_id).is_some_and(|t| t.category_id == 6);
                if !parent_is_ship || !is_repairable_ship_child_flag(root.flag) {
                    return Err(BridgeRpcError {
                        code: "ItemInsideContainer".into(),
                        message: format!("Item {req_id} is inside a container or cargo hold and must be unpacked to hangar"),
                        client_error: None,
                    });
                }
                if Some(parent.item_id) != active_ship && Some(parent.location_id) != dock_loc {
                    return Err(BridgeRpcError {
                        code: "ItemNotAtLocation".into(),
                        message: format!("Item {req_id} is not located at dockable location"),
                        client_error: None,
                    });
                }
            }
            let req_is_ship = ctx.sde.types.get(&root.type_id).is_some_and(|t| t.category_id == 6);
            let mut sub_items: Vec<&eve_db::InventoryItemRecord> = snap
                .items
                .values()
                .filter(|i| {
                    (i.owner_id == char_id || Some(i.owner_id) == corp_id)
                        && (i.item_id == req_id
                            || (req_is_ship
                                && i.location_id == req_id
                                && is_repairable_ship_child_flag(i.flag)))
                })
                .collect();
            sub_items.sort_by_key(|i| i.item_id);
            let mut rows = Vec::new();
            for it in sub_items {
                let (dmg, max_hp, cpu) = item_repair_metrics(ctx, snap, it);
                let gid = ctx.sde.types.get(&it.type_id).map(|t| i64::from(t.group_id)).unwrap_or(0);
                let base_cents = eve_db::IskCents::from_f64_isk(cpu).0;
                let (tax_unit_cents, unit_cents) = if is_upwell {
                    let t = base_cents.saturating_mul(tax_bps) / 10_000;
                    (t, t)
                } else {
                    (0, base_cents)
                };
                let base_cost = if is_upwell {
                    0.0
                } else {
                    (base_cents.saturating_mul(dmg.ceil() as i64) as f64) / 100.0
                };
                let total_cents = unit_cents.saturating_mul(dmg.ceil() as i64);
                let unit_cost = (unit_cents as f64) / 100.0;
                let total = (total_cents as f64) / 100.0;
                rows.push(PyRep::keyval([
                    ("itemID", PyRep::Integer(it.item_id)),
                    ("typeID", PyRep::Integer(i64::from(it.type_id))),
                    ("groupID", PyRep::Integer(gid)),
                    ("damage", PyRep::Real(dmg)),
                    ("maxHealth", PyRep::Integer(max_hp.round() as i64)),
                    ("costToRepairOneUnitOfDamage", PyRep::Real(unit_cost)),
                    ("baseCost", PyRep::Real(base_cost)),
                    ("totalRepairCost", PyRep::Real(total)),
                    ("discount", PyRep::Real(0.0)),
                    ("serviceCharge", PyRep::Real((tax_unit_cents.saturating_mul(dmg.ceil() as i64) as f64) / 100.0)),
                ]));
            }
            out.push((PyRep::Integer(req_id), PyRep::List(rows)));
        }
        Ok(out)
    })?;
    Ok(PyRep::Dict(dict_entries))
}

/// `repairSvc.RepairItemsInStation(itemIDs, iskRepairValue)` and
/// `repairSvc.RepairItemsInStructure(itemIDs)` (`base_repairshop.pyj:69,71`).
pub fn handle_repair_items_in_location(
    ctx: &ServerContext,
    session: &SessionState,
    is_structure: bool,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    life::sync_all_structures(ctx);
    let repair_sid = resolve_structure_repair_location(ctx, session, char_id);
    let loc_id = if is_structure {
        repair_sid.ok_or_else(|| BridgeRpcError {
            code: "NotInStructure".into(),
            message: "RepairItemsInStructure requires being docked in or tethered at an accessible operational Upwell structure".into(),
            client_error: None,
        })?
    } else {
        let sid = session.station_id.or(session.structure_id).ok_or_else(|| BridgeRpcError {
            code: "NotInStation".into(),
            message: "RepairItemsInStation requires being docked in a station or structure".into(),
            client_error: None,
        })?;
        if sid < 1_000_000_000_000
            && !ctx.db.with_snapshot(|s| s.structures.contains_key(&sid))
            && !crate::services::station_structure::npc_station_offers_service(&ctx.sde, sid, 13)
        {
            return Err(BridgeRpcError {
                code: "StationServiceUnavailable".into(),
                message: format!("Station {sid} does not offer repair facilities"),
                client_error: None,
            });
        }
        sid
    };
    let item_ids: Vec<i64> = match args.first() {
        Some(PyRep::List(list) | PyRep::Tuple(list)) => {
            list.iter().filter_map(|v| v.as_i64()).collect()
        }
        Some(PyRep::Integer(id)) => vec![*id],
        _ => Vec::new(),
    };
    if item_ids.is_empty() {
        return Err(BridgeRpcError {
            code: "InvalidArgument".into(),
            message: "RepairItems requires a non-empty list of itemIDs".into(),
            client_error: None,
        });
    }
    let isk_budget = args.get(1).and_then(|a| a.as_f64());
    if let Some(v) = isk_budget {
        if !v.is_finite() || v < 0.0 {
            return Err(BridgeRpcError {
                code: "InvalidArgument".into(),
                message: "iskRepairValue must be a finite non-negative number".into(),
                client_error: None,
            });
        }
    }
    let corp_id = session.corporation_id.or_else(|| {
        ctx.db.with_snapshot(|snap| snap.characters.get(&char_id).map(|c| c.corporation_id))
    });
    let corp_roles_at_loc = corp_id.map(|cid| {
        crate::services::inventory_fitting::get_effective_corp_roles_at_location(
            ctx,
            cid,
            char_id,
            Some(loc_id),
        )
    });
    let is_docked_at_loc = session.station_id == Some(loc_id) || session.structure_id == Some(loc_id);
    let active_ship = session.ship_id;
    let now_unix = ctx.now_unix();
    let struct_loc = (loc_id >= 1_000_000_000_000).then_some(loc_id).or(repair_sid);
    let (repaired_ids, total_cost, new_bal) = ctx.db.try_with_mut_snapshot(|snap| {
        let tax_bps = resolve_structure_repair_tax_bps(ctx, snap, struct_loc, char_id, now_unix)?;
        let is_upwell = struct_loc.is_some_and(|s| s >= 1_000_000_000_000 || snap.structures.contains_key(&s));
        let mut target_ids = Vec::new();
        let mut base_total_cents = eve_db::IskCents::ZERO;
        let mut total_cents = eve_db::IskCents::ZERO;
        for &req_id in &item_ids {
            let Some(root) = snap.items.get(&req_id) else {
                return Err(BridgeRpcError {
                    code: "ItemNotFound".into(),
                    message: format!("Item {req_id} not found"),
                    client_error: None,
                });
            };
            check_item_repair_ownership_and_role(
                snap,
                root,
                Some(loc_id),
                char_id,
                corp_id,
                active_ship,
                corp_roles_at_loc,
            )?;
            if !is_docked_at_loc {
                let on_active = Some(root.item_id) == active_ship
                    || (Some(root.location_id) == active_ship && is_repairable_ship_child_flag(root.flag));
                if !is_structure || !on_active {
                    return Err(BridgeRpcError {
                        code: "NotDockedInStructure".into(),
                        message: format!(
                            "Must be docked inside structure {loc_id} to repair hangar items"
                        ),
                        client_error: None,
                    });
                }
            } else if root.item_id != active_ship.unwrap_or(0) && root.location_id != loc_id {
                let Some(parent) = snap.items.get(&root.location_id) else {
                    return Err(BridgeRpcError {
                        code: "ItemNotAtLocation".into(),
                        message: format!("Item {req_id} is not located at dockable location {loc_id}"),
                        client_error: None,
                    });
                };
                let parent_is_ship = ctx.sde.types.get(&parent.type_id).is_some_and(|t| t.category_id == 6);
                if !parent_is_ship || !is_repairable_ship_child_flag(root.flag) {
                    return Err(BridgeRpcError {
                        code: "ItemInsideContainer".into(),
                        message: format!(
                            "Item {req_id} is inside a container or cargo hold and must be unpacked to hangar before repair"
                        ),
                        client_error: None,
                    });
                }
                if Some(parent.item_id) != active_ship && parent.location_id != loc_id {
                    return Err(BridgeRpcError {
                        code: "ItemNotAtLocation".into(),
                        message: format!("Item {req_id} is not located at dockable location {loc_id}"),
                        client_error: None,
                    });
                }
            }
            let req_is_ship = ctx.sde.types.get(&root.type_id).is_some_and(|t| t.category_id == 6);
            for it in snap.items.values() {
                if (it.owner_id == char_id || Some(it.owner_id) == corp_id)
                    && (it.item_id == req_id
                        || (req_is_ship
                            && it.location_id == req_id
                            && is_repairable_ship_child_flag(it.flag)))
                    && !target_ids.contains(&it.item_id)
                {
                    let (dmg, _, cpu) = item_repair_metrics(ctx, snap, it);
                    let dmg_units = dmg.ceil() as i64;
                    if dmg_units > 0 {
                        let base_cents = eve_db::IskCents::from_f64_isk(cpu).0;
                        let unit_cents = if is_upwell {
                            base_cents
                                .checked_mul(tax_bps)
                                .map(|p| p / 10_000)
                                .ok_or_else(|| BridgeRpcError {
                                    code: "ArithmeticOverflow".into(),
                                    message: "Repair service tax calculation overflowed".into(),
                                    client_error: None,
                                })?
                        } else {
                            let item_base = eve_db::IskCents(
                                base_cents.checked_mul(dmg_units).ok_or_else(|| BridgeRpcError {
                                    code: "ArithmeticOverflow".into(),
                                    message: "Item base repair cost overflowed".into(),
                                    client_error: None,
                                })?,
                            );
                            base_total_cents = base_total_cents.checked_add(item_base).ok_or_else(|| {
                                BridgeRpcError {
                                    code: "ArithmeticOverflow".into(),
                                    message: "Total base repair cost overflowed IskCents".into(),
                                    client_error: None,
                                }
                            })?;
                            base_cents
                        };
                        let item_cents = eve_db::IskCents(
                            unit_cents.checked_mul(dmg_units).ok_or_else(|| BridgeRpcError {
                                code: "ArithmeticOverflow".into(),
                                message: "Item total repair cost overflowed".into(),
                                client_error: None,
                            })?,
                        );
                        total_cents = total_cents.checked_add(item_cents).ok_or_else(|| {
                            BridgeRpcError {
                                code: "ArithmeticOverflow".into(),
                                message: "Total repair bill overflowed IskCents".into(),
                                client_error: None,
                            }
                        })?;
                    }
                    if it.item_id == req_id || dmg > 0.0 {
                        target_ids.push(it.item_id);
                    }
                }
            }
        }
        if total_cents.0 > 0 {
            if let Some(budget) = isk_budget {
                let budget_cents = eve_db::IskCents::from_f64_isk(budget);
                if budget_cents < total_cents {
                    return Err(BridgeRpcError {
                        code: "InsufficientFunds".into(),
                        message: format!(
                            "Provided iskRepairValue {budget:.2} is less than repair cost {:.2}",
                            total_cents.to_f64_isk()
                        ),
                        client_error: None,
                    });
                }
            }
        }
        let ch = snap.characters.get_mut(&char_id).ok_or_else(|| BridgeRpcError {
            code: "CharacterNotFound".into(),
            message: format!("Character {char_id} not found"),
            client_error: None,
        })?;
        let fee = total_cents;
        if fee.0 > 0 {
            ch.balance = ch.balance.checked_sub(fee).ok_or_else(|| BridgeRpcError {
                code: "InsufficientFunds".into(),
                message: "Insufficient ISK to pay repair bill".into(),
                client_error: None,
            })?;
            let bal = ch.balance;
            snap.append_journal(
                char_id,
                "RepairBill",
                eve_db::IskCents(-fee.0),
                bal,
                format!("Repaired {} item(s) at {loc_id}", target_ids.len()),
            );
            let tax_cents = eve_db::IskCents(fee.0.saturating_sub(base_total_cents.0));
            if tax_cents.0 > 0 {
                if let Some(sid) = struct_loc {
                    if let Some(owner_corp_id) = snap.structures.get(&sid).map(|st| st.owner_corp_id) {
                        if let Some(corp) = snap.corporations.get_mut(&owner_corp_id) {
                            let new_corp_bal = corp.division_balance(1).checked_add(tax_cents).ok_or_else(|| {
                                BridgeRpcError {
                                    code: "ArithmeticOverflow".into(),
                                    message: "Corporate master wallet overflow on repair tax credit".into(),
                                    client_error: None,
                                }
                            })?;
                            corp.set_division_balance(1, new_corp_bal);
                            snap.next_entity_id += 1;
                            let jid = snap.next_entity_id;
                            let desc = format!("Structure repairshop tax from {char_id} at {sid}");
                            snap.corp_wallet_journals
                                .entry((owner_corp_id, 1))
                                .or_default()
                                .push(eve_db::CorpWalletJournalRecord {
                                    id: jid,
                                    date_unix: now_unix,
                                    ref_type: "RepairBill".into(),
                                    first_party_id: Some(char_id),
                                    second_party_id: Some(owner_corp_id),
                                    amount: tax_cents.to_wire_f64(),
                                    balance: new_corp_bal.to_wire_f64(),
                                    description: desc.clone(),
                                    reason: Some(desc.clone()),
                                });
                            snap.append_journal(owner_corp_id, "RepairBill", tax_cents, new_corp_bal, desc);
                        }
                    }
                }
            }
        }
        let bal = snap.characters.get(&char_id).map(|c| c.balance.to_wire_f64()).unwrap_or(0.0);
        for tid in &target_ids {
            crate::sp6_lifecycle::clear_ship_combat_damage_in_snap(snap, *tid);
        }
        Ok((target_ids, total_cents.to_f64_isk(), bal))
    })?;
    if let Ok(mut fits) = ctx.ship_fittings.write() {
        for fit in fits.values_mut() {
            for m in fit.modules.values_mut() {
                if repaired_ids.contains(&m.item_id) {
                    m.damage_hp = 0.0;
                }
            }
        }
    }
    for &iid in &repaired_ids {
        if let Some(sys_id) = session.solar_system_id.or_else(|| ctx.systems.find_ball_system_sync(iid)) {
            if let Ok(mut bp) = ctx.systems.get_or_spawn(sys_id).ballpark.write() {
                if let Some(cs) = bp.combat_states.get_mut(&iid) {
                    cs.shield_hp = cs.max_shield_hp;
                    cs.armor_hp = cs.max_armor_hp;
                    cs.structure_hp = cs.max_structure_hp;
                }
            }
        }
    }
    if total_cost > 0.0 {
        ctx.emit_typed(
            crate::router::NotificationTarget::Character(char_id),
            crate::router::notify_registry::OnAccountChange {
                account_key: "cash".into(),
                owner_id: char_id,
                balance: new_bal,
            },
        );
    }
    for iid in &repaired_ids {
        ctx.emit_typed(
            crate::router::NotificationTarget::Character(char_id),
            crate::router::notify_registry::OnModuleRepaired { item_id: *iid },
        );
    }
    Ok(PyRep::keyval([
        ("locationID", PyRep::Integer(loc_id)),
        ("repairedItemIDs", PyRep::List(repaired_ids.into_iter().map(PyRep::Integer).collect())),
        ("totalCostISK", PyRep::Real(total_cost)),
    ]))
}

#[cfg(test)]
#[path = "repair_service_tests.rs"]
mod repair_service_tests;

