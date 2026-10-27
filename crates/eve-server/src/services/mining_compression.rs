//! `mining_compression` — In-Space & Structure Compression and Gas Decompression (`MN-16`).
//!
//! Extracted from `mining_advanced` (`R71-P1-5`) so both submodules remain well under 1,400 lines.
//! Implements:
//! - `inSpaceCompressionMgr.CompressItemInSpace`: 1:1 lossless compression in a ship's mining hold
//!   when in range of an active compressor module (`group_id == 4174`) with an active Industrial Core
//!   (`group_id == 515`) on a valid SDE hull.
//! - `structureCompressionMgr.CompressItemInStructure`: 1:1 lossless compression of asteroid ore and ice
//!   when docked at an Upwell structure with an online fitted Reprocessing service module (`group_id == 1322`).
//! - `structureCompressionMgr.DecompressGasInStructure` & `GetMyGasDecompressionEfficiency`:
//!   SDE-driven compressed gas (`group_id == 711`) decompression in Upwell structures.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use eve_db::{InventoryFlag, InventoryItemRecord, WorldStateSnapshot};
use eve_dogma::ShipFittingEngine;
use eve_machonet::SessionState;
use eve_proto::dbrow::{DBRowDescriptor, DbColumnType};
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::{InvType, SdeRegistry, TypeId};
use std::collections::HashMap;

/// Computes gas decompression efficiency in a structure from SDE attributes:
/// - `3262` (`gasDecompressionBaseEfficiency` = 0.80)
/// - `3261` (`structureGasDecompressionEfficiencyBonus`: +0.04 on Athanor 35835, +0.10 on Tatara 35836, 0.0 default)
/// - `3260` (`GasDecompressionEfficiencyBonus` = +1% per level of skill `62452` `Gas Decompression Efficiency`)
pub fn compute_gas_decompression_efficiency(
    ctx: &ServerContext,
    char_id: i64,
    structure_id_opt: Option<i64>,
) -> f64 {
    let base_eff = ctx
        .sde
        .attributes
        .get(&3262)
        .map(|a| a.default_value)
        .unwrap_or(0.80);
    let skill_bonus_pct_per_lvl = ctx
        .sde
        .types
        .get(&62452)
        .and_then(|t| t.default_attributes.get(&3260).copied())
        .unwrap_or(1.0);
    ctx.db.with_snapshot(|snap| {
        let skill_lvl = snap
            .skills
            .get(&char_id)
            .and_then(|m| m.get(&62452))
            .map(|s| s.level as f64)
            .unwrap_or(0.0);
        let struct_bonus = structure_id_opt
            .and_then(|sid| snap.structures.get(&sid))
            .and_then(|st| ctx.sde.types.get(&st.type_id))
            .and_then(|t| t.default_attributes.get(&3261).copied())
            .unwrap_or(0.0);
        let eff = base_eff + struct_bonus + skill_lvl * (skill_bonus_pct_per_lvl / 100.0);
        (eff * 10_000.0).round() / 10_000.0
    })
}

/// Resolves SDE hull restrictions (`canFitShipType1..4` `1302..=1305` and `canFitShipGroup01..04` `1298..=1301`)
/// for a compressor or industrial core module type.
pub fn resolve_compressor_hull_limits(mod_t: &InvType) -> (Vec<u32>, Vec<u32>) {
    let allowed_types: Vec<u32> = [1302u16, 1303, 1304, 1305]
        .into_iter()
        .filter_map(|aid| mod_t.default_attributes.get(&aid).copied())
        .filter(|&v| v > 0.0)
        .map(|v| v.round() as u32)
        .collect();
    let allowed_groups: Vec<u32> = [1298u16, 1299, 1300, 1301]
        .into_iter()
        .filter_map(|aid| mod_t.default_attributes.get(&aid).copied())
        .filter(|&v| v > 0.0)
        .map(|v| v.round() as u32)
        .collect();
    (allowed_types, allowed_groups)
}

/// Checks whether `mod_t` is permitted on a hull of `ship_type_id` (`ship_group_id`) per SDE fitting attributes.
fn matches_hull_sde(mod_t: &InvType, ship_type_id: TypeId, ship_group_id: u32) -> bool {
    let (allowed_types, allowed_groups) = resolve_compressor_hull_limits(mod_t);
    if allowed_types.is_empty() && allowed_groups.is_empty() {
        true
    } else {
        allowed_types.contains(&ship_type_id) || allowed_groups.contains(&ship_group_id)
    }
}

/// Checks whether a candidate fleet/self ship in `sys_id` has an active compressor module (`group_id == 4174`)
/// matching `item_type_id` via SDE `3255` (`compressibleItemsTypeList`) and satisfying SDE `3265`
/// (`activationRequiresActiveIndustrialCore`), returning its effective range in meters.
pub fn has_active_compressor_burst(
    sde: &SdeRegistry,
    snap: &WorldStateSnapshot,
    fittings_guard: &HashMap<i64, ShipFittingEngine>,
    cid: i64,
    sys_id: u32,
    item_type_id: TypeId,
    logistics_bonus_pct_per_lvl: f64,
) -> Option<(i64, f64)> {
    let ch = snap.characters.get(&cid)?;
    if ch.solar_system_id != sys_id || ch.station_id.is_some() || ch.structure_id.is_some() {
        return None;
    }
    let c_ship_id = ch.active_ship_id;
    let c_ship_type = snap.items.get(&c_ship_id)?.type_id;
    let c_ship_group = sde.types.get(&c_ship_type).map(|t| t.group_id).unwrap_or(0);
    let ship_fit_opt = fittings_guard.get(&c_ship_id);

    let is_mod_active = |mod_item: &InventoryItemRecord| -> bool {
        let active_in_fit = ship_fit_opt
            .and_then(|f| f.modules.get(&mod_item.item_id))
            .is_some_and(|fm| {
                matches!(
                    fm.state,
                    eve_dogma::ModuleState::Active | eve_dogma::ModuleState::Overloaded
                )
            });
        let active_in_db = snap.module_active_states.contains(&mod_item.item_id);
        active_in_fit || active_in_db
    };

    let has_active_industrial_core = snap.items.values().any(|it| {
        if it.location_id != c_ship_id
            || !(27..=34).contains(&(it.flag as u16))
            || !is_mod_active(it)
        {
            return false;
        }
        let Some(core_t) = sde.types.get(&it.type_id) else {
            return false;
        };
        core_t.group_id == 515 && matches_hull_sde(core_t, c_ship_type, c_ship_group)
    });

    let logistics_lvl = snap
        .skills
        .get(&cid)
        .and_then(|sk| sk.get(&62453))
        .map(|r| r.level.clamp(0, 5) as f64)
        .unwrap_or(0.0);
    let mut best_range: Option<f64> = None;
    for it in snap.items.values() {
        if it.location_id != c_ship_id
            || !(27..=34).contains(&(it.flag as u16))
            || !is_mod_active(it)
        {
            continue;
        }
        let Some(mod_t) = sde.types.get(&it.type_id) else {
            continue;
        };
        if mod_t.group_id != 4174 {
            continue;
        }
        if !matches_hull_sde(mod_t, c_ship_type, c_ship_group) {
            continue;
        }
        let requires_core = mod_t
            .default_attributes
            .get(&3265)
            .copied()
            .unwrap_or(0.0)
            > 0.0;
        if requires_core && !has_active_industrial_core {
            continue;
        }
        let type_list_id = mod_t
            .default_attributes
            .get(&3255)
            .copied()
            .unwrap_or(0.0)
            .round() as u32;
        let matches_category = sde
            .type_lists
            .get(&type_list_id)
            .zip(sde.types.get(&item_type_id))
            .is_some_and(|(tl, item_t)| tl.contains(item_t));
        if !matches_category {
            continue;
        }
        let base_range = mod_t
            .default_attributes
            .get(&54)
            .copied()
            .unwrap_or(44_000.0);
        let eff_range =
            base_range * (1.0 + (logistics_bonus_pct_per_lvl / 100.0) * logistics_lvl);
        best_range = Some(best_range.map_or(eff_range, |r| r.max(eff_range)));
    }
    best_range.map(|r| (c_ship_id, r))
}

/// Checks whether an Upwell structure has an online Reprocessing service module (`35899`, `35892`, `35894`,
/// or `group_id == 1322`) fitted in a service or module slot (`it.flag != InventoryFlag::Hangar`).
/// Unfitted service modules sitting in the hangar (`InventoryFlag::Hangar`, `flag_id == 4`) never enable
/// structure compression or decompression (`R73 P2-1`).
pub fn structure_has_online_reprocessing_service(
    sde: &SdeRegistry,
    snap: &WorldStateSnapshot,
    structure_id: i64,
    st_type_id: TypeId,
) -> bool {
    if snap.structures.get(&structure_id).is_some_and(|st| {
        !st.quantum_core_installed
            || st.fuel_blocks == 0
            || matches!(
                st.state.as_str(),
                "Destroyed" | "Anchoring" | "AnchorVulnerable" | "NoCore"
            )
    }) {
        return false;
    }
    let is_reproc_mod = |it: &InventoryItemRecord| -> bool {
        matches!(it.type_id, 35899 | 35892 | 35894)
            || sde.types.get(&it.type_id).is_some_and(|t| t.group_id == 1322)
    };
    let is_fitted_and_online = |it: &InventoryItemRecord| -> bool {
        it.flag != InventoryFlag::Hangar
            && (it.flag.is_service_slot() || matches!(it.flag as u16, 11..=34))
            && snap.module_online_states.get(&it.item_id).copied().unwrap_or(true)
    };
    let has_fitted_reproc = snap.items.values().any(|it| {
        it.location_id == structure_id && is_reproc_mod(it) && is_fitted_and_online(it)
    });
    if has_fitted_reproc {
        return true;
    }
    let has_any_reproc_item = snap
        .items
        .values()
        .any(|it| it.location_id == structure_id && is_reproc_mod(it));
    let is_refinery = sde
        .types
        .get(&st_type_id)
        .is_some_and(|t| t.group_id == 1406);
    is_refinery && !has_any_reproc_item
}

/// Handles `inSpaceCompressionMgr.CompressItemInSpace`.
pub async fn handle_in_space_compression_mgr(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;

    if session.station_id.is_some() || session.structure_id.is_some() {
        return Err(BridgeRpcError {
            code: "MustBeInSpace".into(),
            message: "CompressItemInSpace requires being undocked in space".into(),
        });
    }

    let Some(item_id) = args.first().and_then(|a| a.as_i64()) else {
        return Err(BridgeRpcError {
            code: "InvalidArgument".into(),
            message: "CompressItemInSpace requires itemID".into(),
        });
    };

    let ship_id = session.ship_id;
    let Some(sys_id) = session.solar_system_id else {
        return Err(BridgeRpcError {
            code: "MustBeInSpace".into(),
            message: "CompressItemInSpace requires a valid solarSystemID in space".into(),
        });
    };

    let (comp_type, item_type_id) = ctx.db.with_snapshot(|snap| {
        let Some(item) = snap.items.get(&item_id) else {
            return Err(BridgeRpcError {
                code: "ItemNotFound".into(),
                message: format!("Item {item_id} not found"),
            });
        };
        if item.owner_id != char_id || Some(item.location_id) != ship_id {
            return Err(BridgeRpcError {
                code: "CompressionFailed".into(),
                message: "Item must be owned by character and located on the active ship".into(),
            });
        }
        if item.flag == InventoryFlag::Locked || snap.is_item_locked_in_contract_or_job(item_id) {
            return Err(BridgeRpcError {
                code: "ItemLocked".into(),
                message: "Item is locked in a contract or job and cannot be compressed".into(),
            });
        }
        if item.quantity < 1 {
            return Err(BridgeRpcError {
                code: "CompressionFailed".into(),
                message: "Item has invalid quantity".into(),
            });
        }
        let in_mining_hold = matches!(
            item.flag,
            InventoryFlag::SpecializedOreHold
                | InventoryFlag::SpecializedIceHold
                | InventoryFlag::SpecializedGasHold
                | InventoryFlag::SpecializedMineralHold
        );
        if !in_mining_hold {
            return Err(BridgeRpcError {
                code: "NotInMiningHold".into(),
                message: "Material to compress in space must be in the ship's mining hold".into(),
            });
        }
        let Some(&comp_type) = ctx.sde.compressible_types.get(&item.type_id) else {
            return Err(BridgeRpcError {
                code: "CompressionFailed".into(),
                message: format!("Type {} is not compressible", item.type_id),
            });
        };
        let Some(item_t) = ctx.sde.types.get(&item.type_id) else {
            return Err(BridgeRpcError {
                code: "CompressionFailed".into(),
                message: format!("Unknown SDE type {}", item.type_id),
            });
        };
        Ok((comp_type, item_t.type_id))
    })?;

    let logistics_bonus_pct_per_lvl = ctx
        .sde
        .types
        .get(&62453)
        .and_then(|t| t.default_attributes.get(&3263).copied())
        .unwrap_or(10.0);

    let fittings_guard = ctx
        .ship_fittings
        .read()
        .map(|g| g.clone())
        .unwrap_or_default();
    let candidate_compressors: Vec<(i64, f64)> = ctx.db.with_snapshot(|snap| {
        let mut candidate_chars = vec![char_id];
        for fl in snap.fleets.values() {
            if fl.members.contains(&char_id) {
                for &m_id in &fl.members {
                    if !candidate_chars.contains(&m_id) {
                        candidate_chars.push(m_id);
                    }
                }
            }
        }
        candidate_chars
            .into_iter()
            .filter_map(|cid| {
                has_active_compressor_burst(
                    &ctx.sde,
                    snap,
                    &fittings_guard,
                    cid,
                    sys_id,
                    item_type_id,
                    logistics_bonus_pct_per_lvl,
                )
            })
            .collect()
    });

    let handle = ctx.systems.get_or_spawn(sys_id);
    if let Some(sid) = ship_id {
        if handle
            .get_combat_state(sid)
            .await
            .ok()
            .flatten()
            .is_some_and(|cs| cs.is_cloaked)
        {
            return Err(BridgeRpcError {
                code: "CannotCompressWhileCloaked".into(),
                message: "Cannot compress ore while ship is cloaked".into(),
            });
        }
    }
    let my_pos = match ship_id {
        Some(sid) => handle.get_ball_snapshot(sid).await.ok().flatten().map(|(_, b)| b.position),
        None => None,
    };
    let mut in_range_compressor = false;
    for (c_ship_id, max_range) in candidate_compressors {
        if handle
            .get_combat_state(c_ship_id)
            .await
            .ok()
            .flatten()
            .is_some_and(|cs| cs.is_cloaked)
        {
            continue;
        }
        if Some(c_ship_id) == ship_id {
            in_range_compressor = true;
            break;
        }
        if let Some(mpos) = my_pos {
            if let Ok(Some((_, cball))) = handle.get_ball_snapshot(c_ship_id).await {
                if (mpos - cball.position).length() <= max_range {
                    in_range_compressor = true;
                    break;
                }
            }
        }
    }

    if !in_range_compressor {
        return Err(BridgeRpcError {
            code: "NoActiveCompressorInRange".into(),
            message: "In-space compression requires a ship with an active Industrial Core and matching active Compressor module in range".into(),
        });
    }

    let (iid, ctype, qty, rem) = ctx.db.try_with_mut_snapshot(|snap| {
        if snap.is_item_locked_in_contract_or_job(item_id) {
            return Err(BridgeRpcError {
                code: "ItemLocked".into(),
                message: "Item is locked in a contract or job and cannot be compressed".into(),
            });
        }
        let Some(item) = snap.items.get_mut(&item_id) else {
            return Err(BridgeRpcError {
                code: "ItemNotFound".into(),
                message: format!("Item {item_id} not found"),
            });
        };
        if item.owner_id != char_id || Some(item.location_id) != ship_id || item.flag == InventoryFlag::Locked || item.quantity < 1 {
            return Err(BridgeRpcError {
                code: "CompressionFailed".into(),
                message: "Item cannot be compressed in space".into(),
            });
        }
        item.type_id = comp_type;
        item.version += 1;
        Ok((item.item_id, item.type_id, item.quantity, 0i32))
    })?;

    Ok(PyRep::keyval([
        ("itemID", PyRep::Integer(iid)),
        ("compressedTypeID", PyRep::Integer(ctype as i64)),
        ("quantity", PyRep::Integer(qty as i64)),
        ("compressedUnits", PyRep::Integer(qty as i64)),
        ("remainderUnits", PyRep::Integer(rem as i64)),
    ]))
}

/// Handles `structureCompressionMgr.*` (`CompressItemInStructure`, `DecompressGasInStructure`,
/// `GetMyGasDecompressionEfficiency`).
pub fn handle_structure_compression_mgr(
    ctx: &ServerContext,
    session: &mut SessionState,
    method: &str,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    match ("structureCompressionMgr", method) {
        ("structureCompressionMgr", "CompressItemInStructure") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if session.station_id.is_none() && session.structure_id.is_none() {
                return Some(Err(BridgeRpcError {
                    code: "NotDocked".into(),
                    message: "CompressItemInStructure requires being docked at an Upwell structure".into(),
                }));
            }
            let Some(structure_id) = session.structure_id else {
                return Some(Err(BridgeRpcError {
                    code: "MustBeInStructure".into(),
                    message: "CompressItemInStructure requires an Upwell structure with reprocessing, not an NPC station".into(),
                }));
            };
            let item_id = if args.len() >= 2 {
                if let Some(arg_sid) = args.first().and_then(|a| a.as_i64()) {
                    if arg_sid != structure_id {
                        return Some(Err(BridgeRpcError {
                            code: "InvalidStructure".into(),
                            message: format!("Cannot compress using structure {arg_sid} while docked at {structure_id}"),
                        }));
                    }
                }
                args.get(1).and_then(|a| a.as_i64()).unwrap_or(0)
            } else {
                args.first().and_then(|a| a.as_i64()).unwrap_or(0)
            };

            if item_id == 0 {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "CompressItemInStructure requires itemID".into(),
                }));
            }

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(st) = snap.structures.get(&structure_id) else {
                    return Err(BridgeRpcError {
                        code: "StructureNotFound".into(),
                        message: format!("Structure {structure_id} not found"),
                    });
                };
                let has_reproc_service = structure_has_online_reprocessing_service(
                    &ctx.sde,
                    snap,
                    structure_id,
                    st.type_id,
                );
                if st.fuel_blocks == 0 || !has_reproc_service {
                    return Err(BridgeRpcError {
                        code: "ReprocessingServiceOffline".into(),
                        message: format!(
                            "Structure {structure_id} does not have an online Reprocessing service"
                        ),
                    });
                }
                if !crate::services::station_structure::structure_upwell_ops::evaluate_character_structure_access_in_snap(
                    snap, structure_id, char_id,
                ) {
                    return Err(BridgeRpcError {
                        code: "AccessDenied".into(),
                        message: format!(
                            "Character {char_id} is denied access to structure {structure_id} by its access profile"
                        ),
                    });
                }

                if snap.is_item_locked_in_contract_or_job(item_id) {
                    return Err(BridgeRpcError {
                        code: "ItemLocked".into(),
                        message: format!("Item {item_id} is locked in a contract or job"),
                    });
                }

                let caller_corp = snap
                    .characters
                    .get(&char_id)
                    .map(|c| c.corporation_id)
                    .unwrap_or(0);
                let Some(item) = snap.items.get_mut(&item_id) else {
                    return Err(BridgeRpcError {
                        code: "CompressionFailed".into(),
                        message: format!("Item {item_id} not found"),
                    });
                };
                if item.owner_id != char_id && !(caller_corp > 0 && item.owner_id == caller_corp) {
                    return Err(BridgeRpcError {
                        code: "AccessDenied".into(),
                        message: "Character does not own this item".into(),
                    });
                }
                if item.flag == InventoryFlag::Locked {
                    return Err(BridgeRpcError {
                        code: "ItemLocked".into(),
                        message: format!("Item {item_id} is locked"),
                    });
                }
                let reachable = (item.location_id == structure_id
                    && ((item.owner_id == char_id && item.flag == InventoryFlag::Hangar)
                        || (caller_corp > 0
                            && item.owner_id == caller_corp
                            && matches!(
                                item.flag,
                                InventoryFlag::Hangar
                                    | InventoryFlag::CorpSAG1
                                    | InventoryFlag::CorpSAG2
                                    | InventoryFlag::CorpSAG3
                                    | InventoryFlag::CorpSAG4
                                    | InventoryFlag::CorpSAG5
                                    | InventoryFlag::CorpSAG6
                                    | InventoryFlag::CorpSAG7
                            ))))
                    || (Some(item.location_id) == session.ship_id
                        && matches!(
                            item.flag,
                            InventoryFlag::Cargo
                                | InventoryFlag::SpecializedOreHold
                                | InventoryFlag::SpecializedGasHold
                                | InventoryFlag::SpecializedIceHold
                        ));
                if !reachable || item.quantity < 1 {
                    return Err(BridgeRpcError {
                        code: "CompressionFailed".into(),
                        message: "Item cannot be compressed (invalid quantity or not in current structure hangar)".into(),
                    });
                }
                let Some(&comp_type) = ctx.sde.compressible_types.get(&item.type_id) else {
                    return Err(BridgeRpcError {
                        code: "CompressionFailed".into(),
                        message: format!("Type {} is not compressible", item.type_id),
                    });
                };
                let group_id = ctx
                    .sde
                    .types
                    .get(&item.type_id)
                    .map(|t| t.group_id)
                    .unwrap_or(0);
                if group_id == 711 || matches!(group_id, 1884 | 1920..=1923) {
                    return Err(BridgeRpcError {
                        code: "StructureCannotCompressType".into(),
                        message: "Gas and Moon Ore cannot be compressed in a structure; use an in-space compressor".into(),
                    });
                }

                item.type_id = comp_type;
                item.version += 1;
                Ok((item.item_id, item.type_id, item.quantity, 0i32))
            });

            match res {
                Ok((iid, ctype, qty, rem)) => Some(Ok(PyRep::keyval([
                    ("itemID", PyRep::Integer(iid)),
                    ("compressedTypeID", PyRep::Integer(ctype as i64)),
                    ("quantity", PyRep::Integer(qty as i64)),
                    ("compressedUnits", PyRep::Integer(qty as i64)),
                    ("remainderUnits", PyRep::Integer(rem as i64)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        ("structureCompressionMgr", "DecompressGasInStructure") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };

            if session.station_id.is_none() && session.structure_id.is_none() {
                return Some(Err(BridgeRpcError {
                    code: "NotDocked".into(),
                    message:
                        "DecompressGasInStructure requires being docked at an Upwell structure"
                            .into(),
                }));
            }
            let Some(structure_id) = session.structure_id else {
                return Some(Err(BridgeRpcError {
                    code: "MustBeInStructure".into(),
                    message: "DecompressGasInStructure requires an Upwell structure".into(),
                }));
            };

            let item_id = if args.len() >= 2 {
                if let Some(arg_sid) = args.first().and_then(|a| a.as_i64()) {
                    if arg_sid != structure_id {
                        return Some(Err(BridgeRpcError {
                            code: "InvalidStructure".into(),
                            message: format!(
                                "Cannot decompress gas using structure {arg_sid} while docked at {structure_id}"
                            ),
                        }));
                    }
                }
                args.get(1).and_then(|a| a.as_i64()).unwrap_or(0)
            } else {
                args.first().and_then(|a| a.as_i64()).unwrap_or(0)
            };

            if item_id == 0 {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "DecompressGasInStructure requires itemID".into(),
                }));
            }

            let efficiency =
                compute_gas_decompression_efficiency(ctx, char_id, Some(structure_id));

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(st) = snap.structures.get(&structure_id) else {
                    return Err(BridgeRpcError {
                        code: "StructureNotFound".into(),
                        message: format!("Structure {structure_id} not found"),
                    });
                };
                let has_reproc_service = structure_has_online_reprocessing_service(
                    &ctx.sde,
                    snap,
                    structure_id,
                    st.type_id,
                );
                if st.fuel_blocks == 0 || !has_reproc_service {
                    return Err(BridgeRpcError {
                        code: "ReprocessingServiceOffline".into(),
                        message: format!(
                            "Structure {structure_id} does not have an online Reprocessing service"
                        ),
                    });
                }

                let is_locked_in_contract_or_job = snap.is_item_locked_in_contract_or_job(item_id);

                let (raw_tid, decomp_qty) = {
                    let Some(item) = snap.items.get_mut(&item_id) else {
                        return Err(BridgeRpcError {
                            code: "ItemNotFound".into(),
                            message: format!("Item {item_id} not found"),
                        });
                    };

                    if item.owner_id != char_id {
                        return Err(BridgeRpcError {
                            code: "AccessDenied".into(),
                            message: "Character does not own this item".into(),
                        });
                    }

                    if item.flag == InventoryFlag::Locked || is_locked_in_contract_or_job {
                        return Err(BridgeRpcError {
                            code: "ItemLocked".into(),
                            message: format!(
                                "Item {item_id} is locked (e.g. in courier escrow or contract) and cannot be decompressed"
                            ),
                        });
                    }

                    if item.location_id != structure_id || item.flag != InventoryFlag::Hangar {
                        return Err(BridgeRpcError {
                            code: "ItemNotInStructureHangar".into(),
                            message: format!(
                                "Item {item_id} must be in your hangar at structure {structure_id} to decompress"
                            ),
                        });
                    }

                    let raw_type = ctx
                        .sde
                        .compressible_types
                        .iter()
                        .find(|&(_, &c_tid)| c_tid == item.type_id)
                        .map(|(&r_tid, _)| r_tid);

                    let Some(raw_tid) = raw_type else {
                        return Err(BridgeRpcError {
                            code: "NotCompressedItem".into(),
                            message: format!("Type {} is not a compressed item", item.type_id),
                        });
                    };

                    let is_gas = ctx
                        .sde
                        .types
                        .get(&raw_tid)
                        .is_some_and(|t| t.group_id == 711);
                    if !is_gas {
                        return Err(BridgeRpcError {
                            code: "NotCompressedGas".into(),
                            message: format!(
                                "Type {} (raw type {raw_tid}) is not compressed harvestable cloud gas (group 711)",
                                item.type_id
                            ),
                        });
                    }

                    let total_raw = ((item.quantity as f64) * efficiency).floor() as i64;
                    if total_raw <= 0 {
                        return Err(BridgeRpcError {
                            code: "InsufficientQuantityToDecompress".into(),
                            message: "Quantity too small to yield decompressed gas units".into(),
                        });
                    }
                    item.quantity = 0;
                    (raw_tid, total_raw)
                };

                snap.items.remove(&item_id);

                snap.next_entity_id += 1;
                let new_id = snap.next_entity_id;
                snap.items.insert(
                    new_id,
                    InventoryItemRecord {
                        item_id: new_id,
                        type_id: raw_tid,
                        owner_id: char_id,
                        location_id: structure_id,
                        flag: InventoryFlag::Hangar,
                        quantity: decomp_qty as i32,
                        singleton: false,
                        version: 1,
                        ..Default::default()
                    },
                );

                Ok((new_id, raw_tid, decomp_qty))
            });

            match res {
                Ok((new_iid, r_tid, d_qty)) => Some(Ok(PyRep::keyval([
                    ("itemID", PyRep::Integer(new_iid)),
                    ("decompressedTypeID", PyRep::Integer(r_tid as i64)),
                    ("decompressedQuantity", PyRep::Integer(d_qty)),
                    ("decompressedUnits", PyRep::Integer(d_qty)),
                    ("efficiency", PyRep::Real(efficiency)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        ("structureCompressionMgr", "GetMyGasDecompressionEfficiency") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if let Some(arg_sid) = args.first().and_then(|a| a.as_i64()) {
                if session.structure_id != Some(arg_sid) {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidStructure".into(),
                        message: format!(
                            "Cannot query gas decompression efficiency for structure {arg_sid} while not docked there"
                        ),
                    }));
                }
            }
            let eff = compute_gas_decompression_efficiency(ctx, char_id, session.structure_id);
            Some(Ok(PyRep::Real(eff)))
        }

        _ => None,
    }
}

/// Dispatches compression, decompression, and reprocessing RPCs.
pub async fn handle_compression(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("inSpaceCompressionMgr", "CompressItemInSpace") => {
            Some(handle_in_space_compression_mgr(ctx, session, args).await)
        }
        ("structureCompressionMgr", _) if !method.is_empty() => {
            handle_structure_compression_mgr(ctx, session, method, args)
        }
        ("corpMiningLedger", "GetObserversWithMiningEvents") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let sess_role = session.corp_role;
            let res = ctx.db.with_snapshot(|snap| {
                let caller_corp = snap.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(0);
                if caller_corp == 0 {
                    return Err(BridgeRpcError {
                        code: "NoCorporation".into(),
                        message: "Character does not belong to a corporation".into(),
                    });
                }
                let effective_role = snap
                    .corporations
                    .get(&caller_corp)
                    .and_then(|c| {
                        if char_id == c.ceo_id {
                            Some(eve_db::corp_roles::ROLE_DIRECTOR)
                        } else {
                            c.member_roles.get(&char_id).copied()
                        }
                    })
                    .unwrap_or(sess_role);
                let has_ledger_role = eve_db::corp_roles::has_role(effective_role, eve_db::corp_roles::ROLE_DIRECTOR)
                    || eve_db::corp_roles::has_role(effective_role, eve_db::corp_roles::ROLE_ACCOUNTANT)
                    || eve_db::corp_roles::has_role(effective_role, eve_db::corp_roles::ROLE_JUNIOR_ACCOUNTANT)
                    || eve_db::corp_roles::has_role(effective_role, eve_db::corp_roles::ROLE_STATION_MANAGER);
                if !has_ledger_role {
                    return Err(BridgeRpcError {
                        code: "CrpAccessDenied".into(),
                        message: "Corporate mining ledger requires Director, Accountant, Junior Accountant, or Station Manager role".into(),
                    });
                }
                let mut list = Vec::new();
                for (&obs_id, entries) in &snap.observer_mining_ledger {
                    if let Some(st) = snap.structures.get(&obs_id) {
                        if st.owner_corp_id == caller_corp && !entries.is_empty() {
                            let last_date = entries.last().map(|e| e.last_updated_date.clone()).unwrap_or_else(|| "2026-10-19".into());
                            list.push(vec![
                                PyRep::Integer(obs_id),
                                PyRep::String("structure".to_string()),
                                PyRep::String(last_date),
                            ]);
                        }
                    }
                }
                Ok(list)
            });
            match res {
                Ok(observers) => {
                    let descriptor = DBRowDescriptor::new([
                        ("observerID", DbColumnType::I8),
                        ("observerType", DbColumnType::Str),
                        ("lastUpdated", DbColumnType::Str),
                    ]);
                    Some(Ok(descriptor.build_crowset(observers)))
                }
                Err(e) => Some(Err(e)),
            }
        }
        ("corpMiningLedger", "GetObserverLedger") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(observer_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "GetObserverLedger requires observerID".into(),
                }));
            };
            let now = ctx.now_unix();
            let cutoff = now.saturating_sub(90 * 86_400);
            let sess_role = session.corp_role;
            let res = ctx.db.with_snapshot(|snap| {
                let caller_corp = snap.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(0);
                let Some(st) = snap.structures.get(&observer_id) else {
                    return Err(BridgeRpcError {
                        code: "CrpAccessDenied".into(),
                        message: format!("Observer structure {observer_id} not found or access denied"),
                    });
                };
                if st.owner_corp_id != caller_corp {
                    return Err(BridgeRpcError {
                        code: "CrpAccessDenied".into(),
                        message: format!(
                            "Character {char_id} (corp {caller_corp}) lacks ACL access to observer {observer_id} (owner corp {})",
                            st.owner_corp_id
                        ),
                    });
                }
                let effective_role = snap
                    .corporations
                    .get(&caller_corp)
                    .and_then(|c| {
                        if char_id == c.ceo_id {
                            Some(eve_db::corp_roles::ROLE_DIRECTOR)
                        } else {
                            c.member_roles.get(&char_id).copied()
                        }
                    })
                    .unwrap_or(sess_role);
                let has_ledger_role = eve_db::corp_roles::has_role(effective_role, eve_db::corp_roles::ROLE_DIRECTOR)
                    || eve_db::corp_roles::has_role(effective_role, eve_db::corp_roles::ROLE_ACCOUNTANT)
                    || eve_db::corp_roles::has_role(effective_role, eve_db::corp_roles::ROLE_JUNIOR_ACCOUNTANT)
                    || eve_db::corp_roles::has_role(effective_role, eve_db::corp_roles::ROLE_STATION_MANAGER);
                if !has_ledger_role {
                    return Err(BridgeRpcError {
                        code: "CrpAccessDenied".into(),
                        message: "Corporate mining ledger requires Director, Accountant, Junior Accountant, or Station Manager role".into(),
                    });
                }
                let raw = snap.observer_mining_ledger.get(&observer_id).cloned().unwrap_or_default();
                let mut agg_order: Vec<(i64, i64, TypeId, String)> = Vec::new();
                let mut agg_qty: HashMap<(i64, i64, TypeId, String), i64> = HashMap::new();
                for e in raw {
                    if e.timestamp_unix > 0 && e.timestamp_unix < cutoff {
                        continue;
                    }
                    let key = (e.character_id, e.recorded_corporation_id, e.type_id, e.last_updated_date);
                    if !agg_qty.contains_key(&key) {
                        agg_order.push(key.clone());
                    }
                    *agg_qty.entry(key).or_insert(0) += e.quantity;
                }
                let aggregated: Vec<(i64, i64, TypeId, i64, String)> = agg_order
                    .into_iter()
                    .map(|(cid, rcorp, tid, dt)| {
                        let q = agg_qty.get(&(cid, rcorp, tid, dt.clone())).copied().unwrap_or(0);
                        (cid, rcorp, tid, q, dt)
                    })
                    .collect();
                Ok(aggregated)
            });
            match res {
                Ok(entries) => {
                    let descriptor = DBRowDescriptor::new([
                        ("characterID", DbColumnType::I8),
                        ("recordedCorporationID", DbColumnType::I8),
                        ("typeID", DbColumnType::I4),
                        ("quantity", DbColumnType::I8),
                        ("lastUpdated", DbColumnType::Str),
                    ]);
                    let rows: Vec<Vec<PyRep>> = entries
                        .into_iter()
                        .map(|(cid, rcorp, tid, qty, dt)| {
                            vec![
                                PyRep::Integer(cid),
                                PyRep::Integer(rcorp),
                                PyRep::Integer(tid as i64),
                                PyRep::Integer(qty),
                                PyRep::String(dt),
                            ]
                        })
                        .collect();
                    Some(Ok(descriptor.build_crowset(rows)))
                }
                Err(e) => Some(Err(e)),
            }
        }
        ("characterMiningLedger", "GetCharacterLogs") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let now = ctx.now_unix();
            let cutoff = now.saturating_sub(90 * 86_400);
            let records = ctx.db.get_mining_ledger_with_waste(char_id);
            let mut agg_order: Vec<(String, u32, TypeId)> = Vec::new();
            let mut agg_qty: HashMap<(String, u32, TypeId), i64> = HashMap::new();
            let mut agg_waste: HashMap<(String, u32, TypeId), i64> = HashMap::new();
            for (r, waste) in records {
                if r.timestamp_unix > 0 && r.timestamp_unix < cutoff {
                    continue;
                }
                let key = (r.date, r.solar_system_id, r.type_id);
                if !agg_qty.contains_key(&key) {
                    agg_order.push(key.clone());
                }
                *agg_qty.entry(key.clone()).or_insert(0) += r.quantity;
                *agg_waste.entry(key).or_insert(0) += waste;
            }
            let items: Vec<PyRep> = agg_order
                .into_iter()
                .map(|(date, sys_id, type_id)| {
                    let key = (date.clone(), sys_id, type_id);
                    let qty = agg_qty.get(&key).copied().unwrap_or(0);
                    let waste = agg_waste.get(&key).copied().unwrap_or(0);
                    let (group_id, category_id) = ctx
                        .sde
                        .types
                        .get(&type_id)
                        .map(|t| (t.group_id as i64, t.category_id as i64))
                        .unwrap_or((0, 0));
                    PyRep::keyval([
                        ("date", PyRep::String(date)),
                        ("solarSystemID", PyRep::Integer(sys_id as i64)),
                        ("typeID", PyRep::Integer(type_id as i64)),
                        ("groupID", PyRep::Integer(group_id)),
                        ("categoryID", PyRep::Integer(category_id)),
                        ("quantity", PyRep::Integer(qty)),
                        ("quantityWasted", PyRep::Integer(waste)),
                        ("waste", PyRep::Integer(waste)),
                    ])
                })
                .collect();
            Some(Ok(PyRep::List(items)))
        }
        _ => None,
    }
}

/// DERIVED: Metenox Moon Drill hourly Magmatic Gas (`type_id == 81143`) reagent consumption
/// (`docs/spec/03_economy_industry_social.md:64`; `sde_space_components` `autoMoonMiner` stores
/// `<destinyServerBallClass>` without a numeric gas rate).
pub(crate) const METENOX_MAGMATIC_GAS_PER_CYCLE: u32 = 10;

/// Computes the Moon Materials (`group_id == 427`, e.g. `16633..=16653` from `sde.type_materials`)
/// harvested by a Metenox Moon Drill (`30,000 m³/hour` raw moon volume × `efficiency` = `0.40`)
/// over `cycles` hours for the moon's ore composition (`derive_moon_ores_from_sde`).
/// DERIVED: Metenox Moon Drill harvests `30,000 m³/hour` of the moon's composition at `40%` efficiency
/// and outputs only `group_id == 427` Moon Materials (`docs/eve-wiki/Metenox_Moon_Drill.md:15,30`).
pub(crate) fn derive_metenox_moon_materials(
    sde: &eve_sde::SdeRegistry,
    solar_system_id: u32,
    moon_id: i64,
    efficiency: f64,
    cycles: u32,
) -> Vec<(TypeId, u64)> {
    let composition = super::derive_moon_ores_from_sde(sde, solar_system_id, moon_id);
    let mut totals: std::collections::BTreeMap<TypeId, f64> = std::collections::BTreeMap::new();
    for (ore_tid, frac) in composition {
        let portion_vol = sde
            .types
            .get(&ore_tid)
            .map(|t| t.volume.max(0.1) * (t.portion_size.max(1) as f64))
            .unwrap_or(1_000.0);
        let portions = (30_000.0 * frac * (cycles as f64)) / portion_vol;
        if let Some(mats) = sde.type_materials.get(&ore_tid) {
            for mat in mats {
                let is_moon_material = sde
                    .types
                    .get(&mat.material_type_id)
                    .is_some_and(|t| t.group_id == 427);
                if is_moon_material {
                    *totals.entry(mat.material_type_id).or_insert(0.0) +=
                        portions * (mat.quantity as f64) * efficiency;
                }
            }
        }
    }
    totals
        .into_iter()
        .map(|(tid, qty)| (tid, qty.round() as u64))
        .filter(|&(_, qty)| qty > 0)
        .collect()
}

pub(crate) struct MetenoxDrillInspection {
    pub(crate) moon_id: i64,
    pub(crate) total_struct_fuel: u32,
    pub(crate) total_struct_gas: u32,
    pub(crate) primary_mat_tid: TypeId,
    pub(crate) hourly_yield: u64,
    pub(crate) hourly_materials: Vec<(TypeId, u64)>,
}

pub(crate) fn caller_has_structure_extraction_role(
    snap: &eve_db::WorldStateSnapshot,
    char_id: i64,
    corp_id: i64,
    session_corp_role: u64,
) -> bool {
    use eve_db::corp_roles::{ROLE_DIRECTOR, ROLE_STARBASE_CONFIG, ROLE_STATION_MANAGER};
    const MOON_ROLES: u64 = ROLE_DIRECTOR | ROLE_STATION_MANAGER | ROLE_STARBASE_CONFIG;
    if (session_corp_role & MOON_ROLES) != 0 {
        return true;
    }
    let Some(corp) = snap.corporations.get(&corp_id) else {
        return true;
    };
    if corp.ceo_id == 0 || corp.ceo_id == char_id {
        return true;
    }
    if corp.member_roles.is_empty() {
        return true;
    }
    let member_role = corp.member_roles.get(&char_id).copied().unwrap_or(0);
    (member_role & MOON_ROLES) != 0
}

pub(crate) fn inspect_metenox_drill(
    sde: &eve_sde::SdeRegistry,
    snap: &eve_db::WorldStateSnapshot,
    structure_id: i64,
    char_id: i64,
    efficiency: f64,
) -> Result<MetenoxDrillInspection, BridgeRpcError> {
    let Some(st) = snap.structures.get(&structure_id) else {
        return Err(BridgeRpcError {
            code: "StructureNotFound".into(),
            message: format!("Structure {structure_id} not found"),
        });
    };
    // SDE group 4744 (Upwell Moon Drill, type 81826 Metenox Moon Drill).
    if !sde.types.get(&st.type_id).is_some_and(|t| t.group_id == 4744) {
        return Err(BridgeRpcError {
            code: "InvalidStructureType".into(),
            message: format!(
                "Auto moon mining requires a Metenox Moon Drill (type 81826), got structure type {}",
                st.type_id
            ),
        });
    }
    let caller_corp = snap.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(0);
    let st_owner_corp = st.owner_corp_id;
    if st_owner_corp != caller_corp || !caller_has_structure_extraction_role(snap, char_id, caller_corp, 0) {
        return Err(BridgeRpcError {
            code: "AccessDenied".into(),
            message: "Character lacks corporation permissions for auto moon mining".into(),
        });
    }
    let st_sys_id = st.solar_system_id;
    let sec = sde.solar_systems.get(&st_sys_id).map(|s| s.security_status).unwrap_or(0.0);
    if sec >= 0.45 && st_sys_id < 31_000_000 {
        return Err(BridgeRpcError {
            code: "InvalidSystemSecurity".into(),
            message: format!(
                "Metenox Moon Drill cannot operate in High-Sec system {st_sys_id} (security {sec:.2}); requires Low-Sec (< 0.45), Null-Sec, or Wormhole space"
            ),
        });
    }
    let matches_reagent_owner = |item_owner_id: i64| -> bool {
        if st_owner_corp != 0 { item_owner_id == st_owner_corp } else { item_owner_id == char_id || item_owner_id == caller_corp }
    };
    let item_fuel_blocks: u32 = snap.items.values()
        .filter(|it| it.location_id == structure_id && matches_reagent_owner(it.owner_id) && it.quantity > 0 && it.flag != InventoryFlag::Locked && !snap.is_item_locked_in_contract_or_job(it.item_id) && sde.types.get(&it.type_id).is_some_and(|t| t.group_id == 1136))
        .fold(0u32, |acc, it| acc.saturating_add(it.quantity.max(0) as u32));
    let total_struct_fuel = st.fuel_blocks.saturating_add(item_fuel_blocks);
    let total_struct_gas: u32 = snap.items.values()
        .filter(|it| it.location_id == structure_id && matches_reagent_owner(it.owner_id) && it.quantity > 0 && it.flag != InventoryFlag::Locked && !snap.is_item_locked_in_contract_or_job(it.item_id) && it.type_id == 81143)
        .fold(0u32, |acc, it| acc.saturating_add(it.quantity.max(0) as u32));
    let Some(moon_id) = super::resolve_structure_moon_id(sde, st_sys_id, st.position) else {
        return Err(BridgeRpcError { code: "NoMoonInSystem".into(), message: format!("No SDE moon celestial found in system {st_sys_id}") });
    };
    let hourly_materials = derive_metenox_moon_materials(sde, st_sys_id, moon_id, efficiency, 1);
    let Some(&(primary_mat_tid, _)) = hourly_materials.first() else {
        return Err(BridgeRpcError { code: "NoMoonMaterials".into(), message: format!("Moon {moon_id} in system {st_sys_id} yields no group-427 moon materials") });
    };
    let hourly_yield: u64 = hourly_materials.iter().map(|&(_, q)| q).sum();
    Ok(MetenoxDrillInspection {
        moon_id, total_struct_fuel, total_struct_gas,
        primary_mat_tid, hourly_yield, hourly_materials,
    })
}

/// Computes the Upwell structure base reprocessing yield from SDE `35899` attribute `717` (`0.50`)
/// and the hull's SDE attribute `2722` `strRefiningYieldBonus` (`+2.0%` -> `0.51` for Athanor `35835`;
/// `+5.5%` -> `0.5275` for Tatara `35836`; `0.0%` -> `0.50` for other Upwell structures).
pub fn compute_structure_reprocessing_base_yield(sde: &SdeRegistry, st_type_id: TypeId) -> f64 {
    let service_base = sde
        .types
        .get(&35899)
        .and_then(|t| t.default_attributes.get(&717).copied())
        .unwrap_or(0.50);
    let bonus_pct = sde
        .types
        .get(&st_type_id)
        .and_then(|t| t.default_attributes.get(&2722).copied())
        .unwrap_or(0.0);
    ((service_base * (1.0 + bonus_pct / 100.0)) * 100_000.0).round() / 100_000.0
}

pub fn compute_structure_rigged_base_yield(
    sde: &SdeRegistry,
    snap: &WorldStateSnapshot,
    structure_id: i64,
    inv_type: Option<&InvType>,
) -> f64 {
    let Some(st) = snap.structures.get(&structure_id) else { return 0.50; };
    let unrigged = compute_structure_reprocessing_base_yield(sde, st.type_id);
    let hull_mult = unrigged / 0.50;
    let st_rig_size = sde.types.get(&st.type_id).and_then(|t| t.default_attributes.get(&1547).copied()).unwrap_or(2.0);
    let sec_attr_id: u16 = if st.solar_system_id >= 31_000_000 {
        2357
    } else {
        let sec = sde.solar_systems.get(&st.solar_system_id).map(|s| crate::services::mining_sites::displayed_security(s.security_status)).unwrap_or(1.0);
        if sec <= 0.0 { 2357 } else if sec < 0.5 { 2356 } else { 2355 }
    };
    let mut best_rig_yield: Option<f64> = None;
    for it in snap.items.values() {
        if it.location_id != structure_id || !matches!(it.flag, InventoryFlag::RigSlot0 | InventoryFlag::RigSlot1 | InventoryFlag::RigSlot2) { continue; }
        let Some(rig_t) = sde.types.get(&it.type_id) else { continue; };
        if !matches!(rig_t.group_id, 1941..=1945) { continue; }
        let rig_size = rig_t.default_attributes.get(&1547).copied().unwrap_or(0.0);
        if (rig_size - st_rig_size).abs() > 0.1 { continue; }
        let matches_item = match inv_type {
            None => true,
            Some(ty) => {
                let is_ice = ty.group_id == 465 || ty.group_id == 4033;
                let is_moon = matches!(ty.group_id, 1884 | 1920..=1923);
                let is_ast = (ty.category_id == 25 || ty.group_id == 4932) && !is_ice && !is_moon;
                match rig_t.group_id {
                    1941 => is_ast, 1942 => is_ice, 1943 => is_moon, 1944 | 1945 => is_ast || is_ice || is_moon, _ => false,
                }
            }
        };
        if !matches_item { continue; }
        let Some(&rig_base) = rig_t.default_attributes.get(&717) else { continue; };
        let sec_mult = rig_t.default_attributes.get(&sec_attr_id).copied().unwrap_or(1.0);
        let candidate = rig_base * sec_mult * hull_mult;
        if best_rig_yield.is_none_or(|b| candidate > b) { best_rig_yield = Some(candidate); }
    }
    best_rig_yield.unwrap_or(unrigged)
}

/// DERIVED: CCP stores Prismaticite & Unrefined Mineral outputs in `randomizedTypeMaterials` rather than
/// `sde_type_materials`; equal-ISK batch quantities (`524,288 ISK / base_price` per 100 units) are
/// derived from each SDE mineral's `base_price` (`34..=40`, `11399`).
pub fn resolve_reprocessable_materials(
    sde: &SdeRegistry,
    inv_type: &InvType,
    item_id: i64,
    next_entity_id: i64,
    rng_bits: u64,
    deterministic_quote: bool,
) -> Option<Vec<eve_sde::TypeMaterialEntry>> {
    if let Some(mats) = sde.type_materials.get(&inv_type.type_id).filter(|m| !m.is_empty()) {
        return Some(mats.clone());
    }
    let mut standard_minerals: Vec<&InvType> = sde.types.values().filter(|t| t.group_id == 18 && t.published && t.type_id <= 11399 && t.base_price > 0.0).collect();
    standard_minerals.sort_by_key(|t| t.type_id);
    if standard_minerals.is_empty() { return None; }
    if inv_type.group_id == 4932 {
        let mineral_name = inv_type.name.strip_prefix("Unrefined ")?;
        let min_t = standard_minerals.into_iter().find(|t| t.name == mineral_name)?;
        let qty = (524_288.0 / min_t.base_price.max(2.0)).round().max(1.0) as u64;
        return Some(vec![eve_sde::TypeMaterialEntry { material_type_id: min_t.type_id, quantity: qty }]);
    }
    if inv_type.group_id == 4915 || matches!(inv_type.type_id, 90041 | 90307) {
        let idx = if deterministic_quote {
            (item_id.unsigned_abs() as usize) % standard_minerals.len()
        } else {
            let seed = (item_id as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(next_entity_id as u64) ^ rng_bits;
            (seed as usize) % standard_minerals.len()
        };
        let min_t = standard_minerals[idx];
        let qty = (524_288.0 / min_t.base_price.max(2.0)).round().max(1.0) as u64;
        return Some(vec![eve_sde::TypeMaterialEntry { material_type_id: min_t.type_id, quantity: qty }]);
    }
    None
}

