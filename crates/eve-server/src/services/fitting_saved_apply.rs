//! `fitting_saved_apply` — Fitting Ships from Saved Fittings / Multifit (`FitShips`).

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, HashMap, HashSet};
use crate::router::{BoundObjectEntry, ServerContext};
use eve_db::{InventoryFlag, SavedFittingRecord, TypeId};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, ClientError, PyRep, UserErrorKey};

pub async fn handle_fit_ships(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (_, char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let Some(station_id) = session.station_id.or(session.structure_id) else {
        return Some(Err(BridgeRpcError {
            code: "NotDocked".into(),
            message: "Fitting from saved fitting requires being docked".into(),
            client_error: None,
        }));
    };
    if station_id < 1_000_000_000_000
        && !crate::services::station_structure::npc_station_offers_service(&ctx.sde, station_id, 17)
    {
        return Some(Err(BridgeRpcError {
            code: "StationServiceUnavailable".into(),
            message: format!("Station {station_id} does not offer fitting facilities"),
            client_error: None,
        }));
    }
    let mut target_ship_ids: Vec<i64> = Vec::new();
    if let Some(PyRep::List(list) | PyRep::Tuple(list)) = args.first() {
        if list.iter().all(|el| el.as_i64().is_some()) {
            target_ship_ids.extend(list.iter().filter_map(|el| el.as_i64()));
        }
    }
    if target_ship_ids.is_empty() {
        if let Some(sid) = kwargs
            .get("shipID")
            .and_then(|v| v.as_i64())
            .or(session.ship_id)
        {
            target_ship_ids.push(sid);
        }
    }
    target_ship_ids.sort_unstable();
    target_ship_ids.dedup();
    let fitting_id_opt = if args.len() >= 2 && !target_ship_ids.is_empty() {
        args.get(1).and_then(|a| a.as_i64())
    } else {
        args.first()
            .and_then(|a| a.as_i64())
            .or_else(|| kwargs.get("fittingID").and_then(|v| v.as_i64()))
    };

    let mut desired_items: Vec<(TypeId, InventoryFlag, i32)> = Vec::new();
    let mut expected_ship_type_id: Option<u32> = None;
    if let Some(fid) = fitting_id_opt {
        let Some(saved) = ctx.db.with_snapshot(|s| s.saved_fittings.get(&fid).cloned()) else {
            return Some(Err(BridgeRpcError {
                code: "FittingNotFound".into(),
                message: format!("Fitting {fid} not found"),
                client_error: None,
            }));
        };
        let authorized = saved.owner_id == char_id
            || session
                .corporation_id
                .is_some_and(|cid| cid > 0 && saved.owner_id == cid)
            || session
                .alliance_id
                .is_some_and(|aid| aid > 0 && saved.owner_id == aid)
            || crate::services::inventory_fitting::fitting_mgr_service::is_community_fitting_owner(saved.owner_id);
        if !authorized {
            return Some(Err(BridgeRpcError {
                code: "AccessDenied".into(),
                message: format!("Not authorized to access fitting {fid}"),
                client_error: None,
            }));
        }
        desired_items = saved.items;
        if saved.ship_type_id > 0 {
            expected_ship_type_id = Some(saved.ship_type_id);
        }
    }
    if desired_items.is_empty() {
        let fit_data_arg = kwargs
            .get("fitData")
            .or_else(|| args.get(1))
            .or_else(|| args.first());
        if let Some(PyRep::List(list) | PyRep::Tuple(list)) = fit_data_arg {
            for el in list {
                if let PyRep::Tuple(seq) | PyRep::List(seq) = el {
                    if let (Some(tid), Some(flg)) = (
                        seq.first().and_then(|v| v.as_i64()),
                        seq.get(1).and_then(|v| v.as_i64()),
                    ) {
                        let Ok(u16_flg) = u16::try_from(flg) else {
                            return Some(Err(BridgeRpcError {
                                code: "InvalidFittingFlag".into(),
                                message: format!("Invalid flag {flg}"),
                                client_error: None,
                            }));
                        };
                        let is_allowed_flag = matches!(
                            u16_flg,
                            5 | 87 | 11..=34 | 92..=94 | 125..=132 | 158 | 133..=151 | 177 | 179 | 181 | 182 | 185
                        );
                        if !is_allowed_flag {
                            return Some(Err(BridgeRpcError {
                                code: "InvalidFittingFlag".into(),
                                message: format!("Flag {flg} is not a valid fitting destination"),
                                client_error: None,
                            }));
                        }
                        let qty_i64 = seq.get(2).and_then(|v| v.as_i64()).unwrap_or(1);
                        let Ok(qty) = i32::try_from(qty_i64) else {
                            return Some(Err(BridgeRpcError {
                                code: "InvalidQuantity".into(),
                                message: format!("Invalid quantity {qty_i64}"),
                                client_error: None,
                            }));
                        };
                        desired_items.push((
                            tid as TypeId,
                            InventoryFlag::from_u16(u16_flg),
                            qty.max(1),
                        ));
                    }
                }
            }
        }
    }

    let (_fitted_count, missing, _moved_items, failed_ship_id) =
        match validate_and_apply_fitting_with_failed_ship(
            ctx,
            char_id,
            station_id,
            &target_ship_ids,
            expected_ship_type_id,
            &desired_items,
            false,
        ) {
            Ok(v) => v,
            Err(e) => return Some(Err(e)),
        };
    let mut missing_rows: Vec<(TypeId, i64)> =
        missing.into_iter().filter(|&(_, qty)| qty > 0).collect();
    missing_rows.sort_by_key(|&(tid, _)| tid);
    let failed_to_load: Vec<PyRep> = missing_rows
        .into_iter()
        .map(|(tid, qty)| PyRep::Tuple(vec![PyRep::Integer(tid as i64), PyRep::Integer(qty)]))
        .collect();
    Some(Ok(PyRep::Tuple(vec![
        PyRep::List(failed_to_load),
        failed_ship_id.map(PyRep::Integer).unwrap_or(PyRep::None),
    ])))
}

/// Validates and fits `desired_items` from `char_id`'s Hangar at `station_id` onto `target_ship_ids`.
/// Returns `(fitted_count, missing_by_type, moved_items)`.
#[derive(Default)]
pub(crate) struct FitApplyOptions<'a> {
    pub emit_single_item_change: bool,
    pub preferred_items_by_type: Option<&'a HashMap<TypeId, Vec<i64>>>,
}

const V24_FIT_KV_KEYS: &[&str] = &[
    "modulesByFlag",
    "chargesByType",
    "dronesByType",
    "fightersByTypeID",
    "iceByType",
    "implantsByTypeID",
];

fn extract_keyval_entries(rep: &PyRep) -> Option<HashMap<&str, &PyRep>> {
    match rep {
        PyRep::KeyVal(map) => Some(map.iter().map(|(k, v)| (k.as_str(), v)).collect()),
        PyRep::Dict(entries) => Some(
            entries
                .iter()
                .filter_map(|(k, v)| k.as_str().map(|ks| (ks, v)))
                .collect(),
        ),
        PyRep::Object { args, .. } => extract_keyval_entries(args),
        PyRep::ObjectEx { dict, .. } => Some(
            dict.iter()
                .filter_map(|(k, v)| k.as_str().map(|ks| (ks, v)))
                .collect(),
        ),
        _ => None,
    }
}

fn is_v24_fitting_keyval(rep: &PyRep) -> bool {
    match rep {
        PyRep::KeyVal(map) => {
            map.is_empty() || V24_FIT_KV_KEYS.iter().any(|k| map.contains_key(*k))
        }
        PyRep::Object { type_name, args } => {
            type_name == "util.KeyVal"
                || type_name == "utillib.KeyVal"
                || is_v24_fitting_keyval(args)
        }
        PyRep::Dict(_) | PyRep::ObjectEx { .. } => extract_keyval_entries(rep)
            .is_some_and(|map| V24_FIT_KV_KEYS.iter().any(|k| map.contains_key(*k))),
        _ => false,
    }
}

fn extract_int_keyed_pairs(rep: &PyRep) -> Result<Vec<(i64, &PyRep)>, BridgeRpcError> {
    match rep {
        PyRep::None => Ok(Vec::new()),
        PyRep::Object { args, .. } => extract_int_keyed_pairs(args),
        PyRep::Dict(entries) | PyRep::ObjectEx { dict: entries, .. } => {
            let mut out = Vec::with_capacity(entries.len());
            for (k, v) in entries {
                let Some(id) = k.as_i64() else {
                    let msg = "Expected integer keys in V24 fitting dictionary".to_string();
                    return Err(BridgeRpcError::with_client_error(
                        "InvalidArgument",
                        msg.clone(),
                        ClientError::CustomInfo(msg),
                    ));
                };
                out.push((id, v));
            }
            Ok(out)
        }
        _ => {
            let msg = "Expected dictionary or defaultdict in V24 FitFitting payload".to_string();
            Err(BridgeRpcError::with_client_error(
                "InvalidArgument",
                msg.clone(),
                ClientError::CustomInfo(msg),
            ))
        }
    }
}

fn extract_item_id_set(rep: &PyRep) -> Result<Vec<i64>, BridgeRpcError> {
    let raw_items: &[PyRep] = match rep {
        PyRep::None => return Ok(Vec::new()),
        PyRep::PySet(items) | PyRep::Set(items) | PyRep::List(items) | PyRep::Tuple(items) => items,
        PyRep::Object { args, .. } => return extract_item_id_set(args),
        PyRep::ObjectEx { header, list, .. } => {
            if !list.is_empty() {
                list
            } else if let PyRep::Tuple(h_items) = header.as_ref() {
                if let Some(PyRep::Tuple(ctor_args)) = h_items.get(1) {
                    if let Some(first) = ctor_args.first() {
                        return extract_item_id_set(first);
                    }
                }
                return Ok(Vec::new());
            } else {
                return Ok(Vec::new());
            }
        }
        _ => {
            let msg = "Expected set or list of itemIDs in itemsToFit".to_string();
            return Err(BridgeRpcError::with_client_error(
                "InvalidArgument",
                msg.clone(),
                ClientError::CustomInfo(msg),
            ));
        }
    };
    let mut ids = Vec::with_capacity(raw_items.len());
    for elem in raw_items {
        let Some(id) = elem.as_i64().filter(|&v| v > 0) else {
            let msg = "Invalid itemID inside FitFitting itemsToFit set".to_string();
            return Err(BridgeRpcError::with_client_error(
                "InvalidArgument",
                msg.clone(),
                ClientError::CustomInfo(msg),
            ));
        };
        ids.push(id);
    }
    Ok(ids)
}

fn parse_v24_items_to_fit(rep: &PyRep) -> Result<HashMap<TypeId, Vec<i64>>, BridgeRpcError> {
    let mut by_type: HashMap<TypeId, Vec<i64>> = HashMap::new();
    for (tid_i64, set_rep) in extract_int_keyed_pairs(rep)? {
        let Some(tid) = u32::try_from(tid_i64).ok().filter(|&v| v > 0) else {
            let msg = format!("Invalid typeID {tid_i64} in itemsToFit");
            return Err(BridgeRpcError::with_client_error(
                "ItemNotFound",
                msg.clone(),
                ClientError::CustomInfo(msg),
            ));
        };
        let ids = extract_item_id_set(set_rep)?;
        by_type.entry(tid).or_default().extend(ids);
    }
    Ok(by_type)
}

fn parse_v24_type_qty_dict(
    rep: &PyRep,
    totals: &mut BTreeMap<TypeId, i64>,
) -> Result<(), BridgeRpcError> {
    for (tid_i64, qty_rep) in extract_int_keyed_pairs(rep)? {
        let Some(tid) = u32::try_from(tid_i64).ok().filter(|&v| v > 0) else {
            let msg = format!("Invalid typeID {tid_i64}");
            return Err(BridgeRpcError::with_client_error(
                "ItemNotFound",
                msg.clone(),
                ClientError::CustomInfo(msg),
            ));
        };
        let Some(qty) = qty_rep.as_i64() else {
            let msg = format!("Invalid quantity for typeID {tid}");
            return Err(BridgeRpcError::with_client_error(
                "InvalidQuantity",
                msg.clone(),
                ClientError::CustomInfo(msg),
            ));
        };
        if qty < 0 {
            let msg = format!("Invalid negative quantity {qty} for typeID {tid}");
            return Err(BridgeRpcError::with_client_error(
                "InvalidQuantity",
                msg.clone(),
                ClientError::CustomInfo(msg),
            ));
        }
        if qty == 0 {
            continue;
        }
        let next = totals
            .get(&tid)
            .copied()
            .unwrap_or(0)
            .checked_add(qty)
            .ok_or_else(|| {
                let msg = format!("Quantity overflow for typeID {tid}");
                BridgeRpcError::with_client_error(
                    "InvalidQuantity",
                    msg.clone(),
                    ClientError::CustomInfo(msg),
                )
            })?;
        totals.insert(tid, next);
    }
    Ok(())
}

fn parse_v24_fit_rigs_flag(rep: &PyRep) -> Result<bool, BridgeRpcError> {
    match rep {
        PyRep::Bool(b) => Ok(*b),
        PyRep::Integer(0) | PyRep::None => Ok(false),
        PyRep::Integer(1) => Ok(true),
        _ => {
            let msg = "Expected boolean fitRigs argument".to_string();
            Err(BridgeRpcError::with_client_error(
                "InvalidArgument",
                msg.clone(),
                ClientError::CustomInfo(msg),
            ))
        }
    }
}

fn append_bay_deltas(
    snap: &eve_db::WorldStateSnapshot,
    ship_id: i64,
    flag: InventoryFlag,
    desired_totals: &BTreeMap<TypeId, i64>,
    out: &mut Vec<(TypeId, InventoryFlag, i32)>,
) -> Result<(), BridgeRpcError> {
    if desired_totals.is_empty() {
        return Ok(());
    }
    let mut existing: HashMap<TypeId, i64> = HashMap::new();
    for item in snap.items.values() {
        if item.location_id == ship_id && item.flag == flag {
            *existing.entry(item.type_id).or_insert(0) += i64::from(item.quantity);
        }
    }
    for (&tid, &total_desired) in desired_totals {
        let already = existing.get(&tid).copied().unwrap_or(0);
        let needed = total_desired.saturating_sub(already);
        if needed <= 0 {
            continue;
        }
        let Ok(qty) = i32::try_from(needed) else {
            let msg = format!("Quantity {needed} exceeds i32 limit for typeID {tid}");
            return Err(BridgeRpcError::with_client_error(
                "InvalidQuantity",
                msg.clone(),
                ClientError::CustomInfo(msg),
            ));
        };
        out.push((tid, flag, qty));
    }
    Ok(())
}

fn build_v24_desired_items(
    ctx: &ServerContext,
    ship_id: i64,
    fit_kv_rep: &PyRep,
    cargo_rep: &PyRep,
    fit_rigs: bool,
) -> Result<Vec<(TypeId, InventoryFlag, i32)>, BridgeRpcError> {
    let Some(kv_fields) = extract_keyval_entries(fit_kv_rep) else {
        let msg = "Expected KeyVal or Dict for fittingObjKeyVal".to_string();
        return Err(BridgeRpcError::with_client_error(
            "InvalidArgument",
            msg.clone(),
            ClientError::CustomInfo(msg),
        ));
    };

    let mut modules_by_flag: Vec<(u16, InventoryFlag, TypeId)> = Vec::new();
    if let Some(&mod_rep) = kv_fields.get("modulesByFlag") {
        for (flag_i64, tid_rep) in extract_int_keyed_pairs(mod_rep)? {
            let Ok(flag_u16) = u16::try_from(flag_i64) else {
                let msg = format!("Invalid flag {flag_i64}");
                return Err(BridgeRpcError::with_client_error(
                    "InvalidFittingFlag",
                    msg.clone(),
                    ClientError::CustomInfo(msg),
                ));
            };
            if !fit_rigs && (92..=99).contains(&flag_u16) {
                continue;
            }
            let inv_flag = InventoryFlag::from_u16(flag_u16);
            if (inv_flag as u16) != flag_u16
                || !matches!(flag_u16, 11..=34 | 92..=94 | 125..=129)
            {
                let msg = format!("Flag {flag_u16} is not a valid fitting destination");
                return Err(BridgeRpcError::with_client_error(
                    "InvalidFittingFlag",
                    msg.clone(),
                    ClientError::CustomInfo(msg),
                ));
            }
            let Some(tid) = tid_rep
                .as_i64()
                .and_then(|v| u32::try_from(v).ok())
                .filter(|&v| v > 0)
            else {
                let msg = format!("Invalid typeID in modulesByFlag for flag {flag_u16}");
                return Err(BridgeRpcError::with_client_error(
                    "ItemNotFound",
                    msg.clone(),
                    ClientError::CustomInfo(msg),
                ));
            };
            modules_by_flag.push((flag_u16, inv_flag, tid));
        }
    }
    modules_by_flag.sort_by_key(|&(f, _, _)| f);

    let mut cargo_totals: BTreeMap<TypeId, i64> = BTreeMap::new();
    for key in [
        "chargesByType",
        "iceByType",
        "implantsByTypeID",
    ] {
        if let Some(&sub_rep) = kv_fields.get(key) {
            parse_v24_type_qty_dict(sub_rep, &mut cargo_totals)?;
        }
    }
    parse_v24_type_qty_dict(cargo_rep, &mut cargo_totals)?;

    let mut drone_totals: BTreeMap<TypeId, i64> = BTreeMap::new();
    if let Some(&sub_rep) = kv_fields.get("dronesByType") {
        parse_v24_type_qty_dict(sub_rep, &mut drone_totals)?;
    }

    let mut fighter_totals: BTreeMap<TypeId, i64> = BTreeMap::new();
    if let Some(&sub_rep) = kv_fields.get("fightersByTypeID") {
        parse_v24_type_qty_dict(sub_rep, &mut fighter_totals)?;
    }

    ctx.db.with_snapshot(|snap| {
        let mut desired_items = Vec::with_capacity(
            modules_by_flag.len() + cargo_totals.len() + drone_totals.len() + fighter_totals.len(),
        );
        for &(_flag_u16, inv_flag, tid) in &modules_by_flag {
            // V24 `Fitting.__init__` includes modules already fitted on `shipInv` in
            // `modulesByFlag`. Skip slots already fitted with the exact desired `typeID`
            // so partial fits succeed cleanly without double-counting limits.
            let already_fitted_same = snap.items.values().any(|it| {
                it.location_id == ship_id
                    && it.flag == inv_flag
                    && it.type_id == tid
                    && ctx.sde.types.get(&it.type_id).map(|t| t.category_id) != Some(8)
            });
            if !already_fitted_same {
                desired_items.push((tid, inv_flag, 1));
            }
        }
        append_bay_deltas(snap, ship_id, InventoryFlag::Cargo, &cargo_totals, &mut desired_items)?;
        append_bay_deltas(snap, ship_id, InventoryFlag::DroneBay, &drone_totals, &mut desired_items)?;
        append_bay_deltas(snap, ship_id, InventoryFlag::FighterBay, &fighter_totals, &mut desired_items)?;
        Ok(desired_items)
    })
}

fn decode_legacy_fit_data(
    ctx: &ServerContext,
    ship_id: i64,
    entries: &[PyRep],
) -> Result<Vec<(TypeId, InventoryFlag, i32)>, BridgeRpcError> {
    let (mut occupied_slots, low_cap, med_cap, hi_cap, rig_cap, sub_cap) =
        ctx.db.with_snapshot(|snap| {
            let mut occ = HashSet::<u16>::new();
            let ship_type_id = snap.items.get(&ship_id).map(|s| s.type_id).unwrap_or(0);
            for it in snap.items.values() {
                if it.location_id == ship_id
                    && matches!(it.flag as u16, 11..=34 | 92..=94 | 125..=132)
                    && ctx.sde.types.get(&it.type_id).map(|t| t.category_id) != Some(8)
                {
                    occ.insert(it.flag as u16);
                }
            }
            let mut engine =
                eve_dogma::ShipFittingEngine::from_sde(ship_id, ship_type_id, &ctx.sde);
            for it in snap.items.values() {
                if it.location_id == ship_id && matches!(it.flag as u16, 125..=132) {
                    let _ = engine.fit_module_with_state(
                        it.item_id,
                        it.type_id,
                        it.flag as u16,
                        true,
                        &ctx.sde,
                    );
                }
            }
            let low = engine
                .ship
                .get_attribute(eve_sde::attr_ids::LOW_SLOTS, &ctx.sde)
                .max(0.0) as u16;
            let med = engine
                .ship
                .get_attribute(eve_sde::attr_ids::MED_SLOTS, &ctx.sde)
                .max(0.0) as u16;
            let hi = engine
                .ship
                .get_attribute(eve_sde::attr_ids::HI_SLOTS, &ctx.sde)
                .max(0.0) as u16;
            let rig = engine
                .ship
                .get_attribute(eve_sde::attr_ids::RIG_SLOTS, &ctx.sde)
                .max(0.0) as u16;
            let sub = if ctx
                .sde
                .types
                .get(&ship_type_id)
                .is_some_and(|t| t.group_id == 963)
            {
                4_u16
            } else {
                0_u16
            };
            (occ, low, med, hi, rig, sub)
        });

    let mut desired_items = Vec::new();
    for entry in entries {
        let (PyRep::Tuple(seq) | PyRep::List(seq)) = entry else {
            continue;
        };
        if let [tid_rep, flg_rep, rest @ ..] = seq.as_slice() {
            if !rest.is_empty() {
                let (Some(tid), Some(flg)) = (tid_rep.as_i64(), flg_rep.as_i64()) else {
                    continue;
                };
                let Ok(tid_u32) = u32::try_from(tid) else {
                    let msg = format!("Invalid typeID {tid}");
                    return Err(BridgeRpcError::with_client_error(
                        "ItemNotFound",
                        msg.clone(),
                        ClientError::CustomInfo(msg),
                    ));
                };
                let Ok(u16_flg) = u16::try_from(flg) else {
                    let msg = format!("Invalid flag {flg}");
                    return Err(BridgeRpcError::with_client_error(
                        "InvalidFittingFlag",
                        msg.clone(),
                        ClientError::CustomInfo(msg),
                    ));
                };
                let is_allowed_flag = matches!(
                    u16_flg,
                    5 | 87
                        | 11..=34
                        | 92..=94
                        | 125..=132
                        | 158
                        | 133..=151
                        | 177
                        | 179
                        | 181
                        | 182
                        | 185
                );
                if !is_allowed_flag {
                    let msg = format!("Flag {flg} is not a valid fitting destination");
                    return Err(BridgeRpcError::with_client_error(
                        "InvalidFittingFlag",
                        msg.clone(),
                        ClientError::CustomInfo(msg),
                    ));
                }
                let qty_i64 = rest.split_first().and_then(|(q, _)| q.as_i64()).unwrap_or(1);
                let Some(qty) = i32::try_from(qty_i64).ok().filter(|&q| q > 0) else {
                    let msg = format!("Invalid quantity {qty_i64}");
                    return Err(BridgeRpcError::with_client_error(
                        "InvalidQuantity",
                        msg.clone(),
                        ClientError::CustomInfo(msg),
                    ));
                };
                if matches!(u16_flg, 11..=34 | 92..=94 | 125..=132)
                    && ctx.sde.types.get(&tid_u32).map(|t| t.category_id) != Some(8)
                {
                    occupied_slots.insert(u16_flg);
                }
                desired_items.push((tid_u32, InventoryFlag::from_u16(u16_flg), qty));
                continue;
            }
        }
        if let [tid_rep, qty_rep] = seq.as_slice() {
            let (Some(tid), Some(qty_i64)) = (tid_rep.as_i64(), qty_rep.as_i64()) else {
                continue;
            };
            let Ok(tid_u32) = u32::try_from(tid) else {
                let msg = format!("Invalid typeID {tid}");
                return Err(BridgeRpcError::with_client_error(
                    "ItemNotFound",
                    msg.clone(),
                    ClientError::CustomInfo(msg),
                ));
            };
            let Some(qty) = i32::try_from(qty_i64).ok().filter(|&q| q > 0) else {
                let msg = format!("Invalid quantity {qty_i64}");
                return Err(BridgeRpcError::with_client_error(
                    "InvalidQuantity",
                    msg.clone(),
                    ClientError::CustomInfo(msg),
                ));
            };
            let Some(item_type) = ctx.sde.types.get(&tid_u32) else {
                let msg = format!("Type {tid_u32} not found in SDE");
                return Err(BridgeRpcError::with_client_error(
                    "ItemNotFound",
                    msg.clone(),
                    ClientError::CustomInfo(msg),
                ));
            };
            if item_type.category_id == 18 {
                desired_items.push((tid_u32, InventoryFlag::DroneBay, qty));
                continue;
            }
            if item_type.category_id == 8 {
                desired_items.push((tid_u32, InventoryFlag::Cargo, qty));
                continue;
            }
            let effs = &item_type.effect_ids;
            let slot_range = if effs.contains(&11) {
                Some(11..(11 + low_cap.min(8)))
            } else if effs.contains(&13) {
                Some(19..(19 + med_cap.min(8)))
            } else if effs.contains(&12) {
                Some(27..(27 + hi_cap.min(8)))
            } else if effs.contains(&2663) {
                Some(92..(92 + rig_cap.min(3)))
            } else if effs.contains(&3772) {
                Some(125..(125 + sub_cap.min(4)))
            } else {
                None
            };
            if let Some(range) = slot_range {
                for _ in 0..qty {
                    let Some(free_flag) = range.clone().find(|f| !occupied_slots.contains(f)) else {
                        let msg = "SlotIndexExceedsHullSlots".to_string();
                        return Err(BridgeRpcError::with_client_error(
                            "UserError",
                            msg.clone(),
                            ClientError::CustomInfo(msg),
                        ));
                    };
                    occupied_slots.insert(free_flag);
                    desired_items.push((tid_u32, InventoryFlag::from_u16(free_flag), 1));
                }
            } else {
                desired_items.push((tid_u32, InventoryFlag::Cargo, qty));
            }
        }
    }
    Ok(desired_items)
}

/// Handles `invbroker.FitFitting` for both V24 7-arg positional calls
/// `(shipID, shipTypeID, itemsToFit, locationID, fittingObjKeyVal, cargoItemsByType, fitRigs)`
/// and legacy `fittingID` / `fitData` call forms.
pub(crate) fn handle_invbroker_fit_fitting(
    ctx: &ServerContext,
    session: &mut SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    raw_args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let Some(station_id) = session.station_id.or(session.structure_id) else {
        let msg = "Fitting requires being docked".to_string();
        return Err(BridgeRpcError::with_client_error(
            "NotDocked",
            msg,
            ClientError::user(UserErrorKey::MustBeDocked),
        ));
    };
    if station_id < 1_000_000_000_000
        && !crate::services::station_structure::npc_station_offers_service(
            &ctx.sde,
            station_id,
            17,
        )
    {
        let msg = format!("Station {station_id} does not offer fitting facilities");
        return Err(BridgeRpcError::with_client_error(
            "StationServiceUnavailable",
            msg.clone(),
            ClientError::CustomInfo(msg),
        ));
    }

    let first_pos_id = raw_args
        .split_first()
        .and_then(|(first, _)| first.as_i64())
        .filter(|&id| id > 0);
    let Some(ship_id) = kwargs
        .get("shipID")
        .and_then(|v| v.as_i64())
        .filter(|&id| id > 0)
        .or(first_pos_id)
        .or(session.ship_id)
    else {
        let msg = "FitFitting requires a valid shipID".to_string();
        return Err(BridgeRpcError::with_client_error(
            "InvalidFittingTarget",
            msg.clone(),
            ClientError::CustomInfo(msg),
        ));
    };

    if let Some(bound) = bound_entry.filter(|b| b.service == "invbroker") {
        if bound.object_id != ship_id && bound.object_id != station_id {
            let msg = format!(
                "Bound invbroker object {} cannot FitFitting onto ship {ship_id} at location {station_id}",
                bound.object_id
            );
            return Err(BridgeRpcError::with_client_error(
                "BoundObjectOwnershipViolation",
                msg,
                ClientError::user(UserErrorKey::AccessDenied),
            ));
        }
    }

    let mut desired_items: Vec<(TypeId, InventoryFlag, i32)> = Vec::new();
    let mut expected_ship_type_id: Option<u32> = None;
    let mut preferred_items_by_type: Option<HashMap<TypeId, Vec<i64>>> = None;
    let mut handled_v24 = false;

    let has_explicit_kw = kwargs.contains_key("fittingID") || kwargs.contains_key("fitData");
    if !has_explicit_kw {
        if let [_, ship_type_rep, items_to_fit_rep, location_rep, fit_kv_rep, rest @ ..] = raw_args
        {
            if is_v24_fitting_keyval(fit_kv_rep) {
                match ship_type_rep {
                    PyRep::None => {}
                    other => {
                        let Some(tid) = other
                            .as_i64()
                            .and_then(|v| u32::try_from(v).ok())
                            .filter(|&v| v > 0)
                        else {
                            let msg = "FitFitting requires a valid shipTypeID".to_string();
                            return Err(BridgeRpcError::with_client_error(
                                "InvalidFittingTarget",
                                msg.clone(),
                                ClientError::CustomInfo(msg),
                            ));
                        };
                        expected_ship_type_id = Some(tid);
                    }
                }
                match location_rep {
                    PyRep::None => {}
                    other => {
                        let Some(loc_id) = other.as_i64().filter(|&v| v > 0) else {
                            let msg = "FitFitting requires a valid locationID".to_string();
                            return Err(BridgeRpcError::with_client_error(
                                "InvalidItemLocation",
                                msg.clone(),
                                ClientError::CustomInfo(msg),
                            ));
                        };
                        if loc_id != station_id {
                            let msg = format!(
                                "FitFitting locationID {loc_id} does not match docked station {station_id}"
                            );
                            return Err(BridgeRpcError::with_client_error(
                                "InvalidItemLocation",
                                msg.clone(),
                                ClientError::CustomInfo(msg),
                            ));
                        }
                    }
                }
                let items_to_fit = parse_v24_items_to_fit(items_to_fit_rep)?;
                if !items_to_fit.is_empty() {
                    preferred_items_by_type = Some(items_to_fit);
                }
                let (cargo_rep, fit_rigs_rep) = match rest {
                    [] => (&PyRep::None, &PyRep::Bool(true)),
                    [cargo] => (cargo, &PyRep::Bool(true)),
                    [cargo, fit_rigs, ..] => (cargo, fit_rigs),
                };
                let fit_rigs = parse_v24_fit_rigs_flag(fit_rigs_rep)?;
                desired_items = build_v24_desired_items(ctx, ship_id, fit_kv_rep, cargo_rep, fit_rigs)?;
                handled_v24 = true;
            }
        }
    }

    if !handled_v24 {
        let pos_second = match raw_args {
            [_, second, ..] => Some(second),
            _ => None,
        };
        if let Some(fid) = kwargs
            .get("fittingID")
            .and_then(|v| v.as_i64())
            .or_else(|| pos_second.and_then(|v| v.as_i64()))
        {
            let Some(saved) = ctx.db.with_snapshot(|s| s.saved_fittings.get(&fid).cloned()) else {
                let msg = format!("Fitting {fid} not found");
                return Err(BridgeRpcError::with_client_error(
                    "FittingNotFound",
                    msg.clone(),
                    ClientError::CustomInfo(msg),
                ));
            };
            let authorized = saved.owner_id == char_id
                || session
                    .corporation_id
                    .is_some_and(|cid| cid > 0 && saved.owner_id == cid)
                || session
                    .alliance_id
                    .is_some_and(|aid| aid > 0 && saved.owner_id == aid)
                || crate::services::inventory_fitting::fitting_mgr_service::is_community_fitting_owner(
                    saved.owner_id,
                );
            if !authorized {
                let msg = format!("Not authorized to access fitting {fid}");
                return Err(BridgeRpcError::with_client_error(
                    "AccessDenied",
                    msg,
                    ClientError::user(UserErrorKey::AccessDenied),
                ));
            }
            desired_items = saved.items;
            if saved.ship_type_id > 0 {
                expected_ship_type_id = Some(saved.ship_type_id);
            }
        } else if let Some(PyRep::List(entries) | PyRep::Tuple(entries)) =
            kwargs.get("fitData").or(pos_second)
        {
            desired_items = decode_legacy_fit_data(ctx, ship_id, entries)?;
        }
    }

    let missing = if let Some(ref pref) = preferred_items_by_type {
        validate_and_apply_fitting_with_options(
            ctx,
            char_id,
            station_id,
            &[ship_id],
            expected_ship_type_id,
            &desired_items,
            FitApplyOptions {
                emit_single_item_change: true,
                preferred_items_by_type: Some(pref),
            },
        )?
        .1
    } else {
        validate_and_apply_fitting(
            ctx,
            char_id,
            station_id,
            &[ship_id],
            expected_ship_type_id,
            &desired_items,
            true,
        )?
        .1
    };
    let mut missing_rows: Vec<(TypeId, i64)> = missing.into_iter().collect();
    missing_rows.sort_by_key(|&(tid, _)| tid);
    Ok(PyRep::List(
        missing_rows
            .into_iter()
            .map(|(tid, qty)| PyRep::Tuple(vec![PyRep::Integer(tid as i64), PyRep::Integer(qty)]))
            .collect(),
    ))
}

pub(crate) fn validate_and_apply_fitting(
    ctx: &ServerContext,
    char_id: i64,
    station_id: i64,
    target_ship_ids: &[i64],
    expected_ship_type_id: Option<u32>,
    desired_items: &[(TypeId, InventoryFlag, i32)],
    emit_single_item_change: bool,
) -> Result<(i64, HashMap<TypeId, i64>, Vec<eve_db::InventoryItemRecord>), BridgeRpcError> {
    let (fitted_count, missing, moved_items, _failed_ship_id) =
        validate_and_apply_fitting_with_failed_ship(
            ctx,
            char_id,
            station_id,
            target_ship_ids,
            expected_ship_type_id,
            desired_items,
            emit_single_item_change,
        )?;
    Ok((fitted_count, missing, moved_items))
}

/// Validates and fits `desired_items` from `char_id`'s Hangar at `station_id` onto `target_ship_ids`.
/// Returns `(fitted_count, missing_by_type, moved_items, first_failed_ship_id)`.
pub(crate) fn validate_and_apply_fitting_with_failed_ship(
    ctx: &ServerContext,
    char_id: i64,
    station_id: i64,
    target_ship_ids: &[i64],
    expected_ship_type_id: Option<u32>,
    desired_items: &[(TypeId, InventoryFlag, i32)],
    emit_single_item_change: bool,
) -> Result<(i64, HashMap<TypeId, i64>, Vec<eve_db::InventoryItemRecord>, Option<i64>), BridgeRpcError> {
    validate_and_apply_fitting_with_options(
        ctx,
        char_id,
        station_id,
        target_ship_ids,
        expected_ship_type_id,
        desired_items,
        FitApplyOptions {
            emit_single_item_change,
            preferred_items_by_type: None,
        },
    )
}

fn validate_and_apply_fitting_with_options(
    ctx: &ServerContext,
    char_id: i64,
    station_id: i64,
    target_ship_ids: &[i64],
    expected_ship_type_id: Option<u32>,
    desired_items: &[(TypeId, InventoryFlag, i32)],
    options: FitApplyOptions<'_>,
) -> Result<(i64, HashMap<TypeId, i64>, Vec<eve_db::InventoryItemRecord>, Option<i64>), BridgeRpcError> {
    let emit_single_item_change = options.emit_single_item_change;
    let ship_check = ctx.db.with_snapshot(|snap| {
        if let Some(pref_map) = options.preferred_items_by_type {
            for item_ids in pref_map.values() {
                for &item_id in item_ids {
                    if snap
                        .items
                        .get(&item_id)
                        .is_some_and(|it| it.owner_id != char_id)
                    {
                        return Err(BridgeRpcError::from_user_key(
                            UserErrorKey::CantDoThatWithSomeoneElsesStuff,
                        ));
                    }
                }
            }
        }
        crate::services::structure_acl_defense_rules::validate_docked_ship_fitting_facility_in_snap(
            ctx, snap, station_id,
        )?;
        for &ship_id in target_ship_ids {
            let Some(sh) = snap.items.get(&ship_id) else {
                return Err(BridgeRpcError {
                    code: "ShipNotFound".into(),
                    message: format!("Target ship {ship_id} not found"),
                    client_error: None,
                });
            };
            let cat = ctx.sde.types.get(&sh.type_id).map(|t| t.category_id).unwrap_or(0);
            if sh.owner_id != char_id || sh.location_id != station_id || cat != 6 {
                return Err(BridgeRpcError {
                    code: "InvalidFittingTarget".into(),
                    message: format!("Target {ship_id} must be an owned ship at station {station_id}"),
                    client_error: None,
                });
            }
            if sh.flag == InventoryFlag::Locked || snap.is_item_or_ancestor_locked_or_impounded(ship_id) {
                return Err(BridgeRpcError {
                    code: "ItemLocked".into(),
                    message: format!("Target ship {ship_id} is locked in an active contract or container"),
                    client_error: None,
                });
            }
            if let Some(exp_type) = expected_ship_type_id {
                if sh.type_id != exp_type {
                    return Err(BridgeRpcError {
                        code: "UserError".into(),
                        message: "ShipTypeMismatch".into(),
                        client_error: None,
                    });
                }
            }
            let ship_type = ctx.sde.types.get(&sh.type_id);
            let mut slot_preview_engine = eve_dogma::ShipFittingEngine::from_sde(ship_id, sh.type_id, &ctx.sde);
            for it in snap.items.values() {
                if it.location_id == ship_id && matches!(it.flag as u16, 125..=132) {
                    let _ = slot_preview_engine.fit_module_with_state(it.item_id, it.type_id, it.flag as u16, true, &ctx.sde);
                }
            }
            for (idx, &(tid, flag, _)) in desired_items.iter().enumerate() {
                let flag_u16 = flag as u16;
                if matches!(flag_u16, 125..=132) {
                    let _ = slot_preview_engine.fit_module_with_state(
                        900_000_000 + idx as i64,
                        tid,
                        flag_u16,
                        true,
                        &ctx.sde,
                    );
                }
            }
            let low_slots = slot_preview_engine
                .ship
                .get_attribute(eve_sde::attr_ids::LOW_SLOTS, &ctx.sde)
                .max(0.0) as u16;
            let med_slots = slot_preview_engine
                .ship
                .get_attribute(eve_sde::attr_ids::MED_SLOTS, &ctx.sde)
                .max(0.0) as u16;
            let hi_slots = slot_preview_engine
                .ship
                .get_attribute(eve_sde::attr_ids::HI_SLOTS, &ctx.sde)
                .max(0.0) as u16;
            let rig_slots = slot_preview_engine
                .ship
                .get_attribute(eve_sde::attr_ids::RIG_SLOTS, &ctx.sde)
                .max(0.0) as u16;

            let occupied_mod_slots: HashSet<InventoryFlag> = snap
                .items
                .values()
                .filter(|i| {
                    i.location_id == ship_id
                        && matches!(i.flag as u16, 11..=34 | 92..=94 | 125..=132)
                        && ctx.sde.types.get(&i.type_id).map(|t| t.category_id) != Some(8)
                })
                .map(|i| i.flag)
                .collect();
            let occupied_charge_slots: HashSet<InventoryFlag> = snap
                .items
                .values()
                .filter(|i| {
                    i.location_id == ship_id
                        && matches!(i.flag as u16, 11..=34)
                        && ctx.sde.types.get(&i.type_id).map(|t| t.category_id) == Some(8)
                })
                .map(|i| i.flag)
                .collect();

            let mut planned_mod_slots = HashSet::new();
            let mut planned_charge_slots = HashSet::new();
            for &(tid, flag, _) in desired_items {
                let flag_u16 = flag as u16;
                if matches!(flag_u16, 11..=34 | 92..=94 | 125..=132) {
                    let is_charge = ctx.sde.types.get(&tid).map(|t| t.category_id) == Some(8);
                    let dup = if is_charge {
                        occupied_charge_slots.contains(&flag) || !planned_charge_slots.insert(flag)
                    } else {
                        occupied_mod_slots.contains(&flag) || !planned_mod_slots.insert(flag)
                    };
                    if dup {
                        return Err(BridgeRpcError {
                            code: "UserError".into(),
                            message: "SlotAlreadyOccupied".into(),
                            client_error: None,
                        });
                    }
                    if (11..=18).contains(&flag_u16) && flag_u16 - 11 >= low_slots {
                        return Err(BridgeRpcError {
                            code: "UserError".into(),
                            message: "SlotIndexExceedsHullSlots".into(),
                            client_error: None,
                        });
                    }
                    if (19..=26).contains(&flag_u16) && flag_u16 - 19 >= med_slots {
                        return Err(BridgeRpcError {
                            code: "UserError".into(),
                            message: "SlotIndexExceedsHullSlots".into(),
                            client_error: None,
                        });
                    }
                    if (27..=34).contains(&flag_u16) && flag_u16 - 27 >= hi_slots {
                        return Err(BridgeRpcError {
                            code: "UserError".into(),
                            message: "SlotIndexExceedsHullSlots".into(),
                            client_error: None,
                        });
                    }
                    if (92..=94).contains(&flag_u16) && flag_u16 - 92 >= rig_slots {
                        return Err(BridgeRpcError {
                            code: "UserError".into(),
                            message: "SlotIndexExceedsHullSlots".into(),
                            client_error: None,
                        });
                    }
                }
            }
            // SDE slot effects and rig size validation
            for &(tid, flag, req_qty) in desired_items {
                if req_qty <= 0 {
                    return Err(BridgeRpcError {
                        code: "InvalidQuantity".into(),
                        message: "Fitting item quantity must be positive".into(),
                        client_error: None,
                    });
                }
                let flag_u16 = flag as u16;
                if matches!(flag_u16, 11..=34 | 92..=94 | 125..=132) {
                    let Some(m_type) = ctx.sde.types.get(&tid) else {
                        return Err(BridgeRpcError {
                            code: "ItemNotFound".into(),
                            message: format!("Type {tid} not found in SDE"),
                            client_error: None,
                        });
                    };
                    if !matches!(m_type.category_id, 7 | 8 | 32) || (m_type.category_id != 8 && req_qty != 1) {
                        return Err(BridgeRpcError {
                            code: "ItemNotFittableModule".into(),
                            message: format!("Type {tid} (qty {req_qty}) cannot be fitted to slot {flag_u16}"),
                            client_error: None,
                        });
                    }
                    let req_eff = if m_type.category_id == 8 {
                        if !matches!(flag_u16, 11..=34) {
                            return Err(BridgeRpcError {
                                code: "UserError".into(),
                                message: format!(
                                    "CannotLoadChargeInSlot: Slot {flag_u16} cannot hold charge {tid}"
                                ),
                                client_error: None,
                            });
                        }
                        let fitted_mod_type = desired_items
                            .iter()
                            .find(|&&(mtid, mflag, _)| {
                                mflag == flag
                                    && ctx
                                        .sde
                                        .types
                                        .get(&mtid)
                                        .is_some_and(|t| t.category_id != 8)
                            })
                            .and_then(|&(mtid, _, _)| ctx.sde.types.get(&mtid))
                            .or_else(|| {
                                snap.items
                                    .values()
                                    .find(|i| {
                                        i.location_id == ship_id
                                            && i.flag == flag
                                            && ctx
                                                .sde
                                                .types
                                                .get(&i.type_id)
                                                .is_some_and(|t| t.category_id != 8)
                                    })
                                    .and_then(|i| ctx.sde.types.get(&i.type_id))
                            });
                        let Some(mod_type) = fitted_mod_type else {
                            return Err(BridgeRpcError {
                                code: "UserError".into(),
                                message: format!(
                                    "NoModuleInSlot: No fitted module in slot {flag_u16} for charge {tid}"
                                ),
                                client_error: None,
                            });
                        };
                        let allowed_groups: Vec<u32> = (604..=608)
                            .filter_map(|attr| {
                                mod_type.default_attributes.get(&attr).map(|&v| v as u32)
                            })
                            .filter(|&g| g > 0)
                            .collect();
                        if !allowed_groups.is_empty() && !allowed_groups.contains(&m_type.group_id)
                        {
                            return Err(BridgeRpcError {
                                code: "UserError".into(),
                                message: format!(
                                    "ChargeGroupMismatch: Charge group {} not compatible with module {}",
                                    m_type.group_id, mod_type.type_id
                                ),
                                client_error: None,
                            });
                        }
                        if let Some(&req_size) = mod_type.default_attributes.get(&128) {
                            if req_size > 0.0 {
                                let charge_size =
                                    m_type.default_attributes.get(&128).copied().unwrap_or(0.0);
                                if (charge_size - req_size).abs() > 0.01 {
                                    return Err(BridgeRpcError {
                                        code: "UserError".into(),
                                        message: format!(
                                            "ChargeSizeMismatch: Charge size {charge_size} != module chargeSize {req_size}"
                                        ),
                                        client_error: None,
                                    });
                                }
                            }
                        }
                        let charge_vol = m_type.volume;
                        if mod_type.capacity <= 0.0
                            || (charge_vol > 0.0
                                && (req_qty as f64) * charge_vol > mod_type.capacity + 1e-6)
                        {
                            return Err(BridgeRpcError {
                                code: "UserError".into(),
                                message: format!(
                                    "CapacityExceeded: Charge qty {req_qty} exceeds module {} capacity {}",
                                    mod_type.type_id, mod_type.capacity
                                ),
                                client_error: None,
                            });
                        }
                        0
                    } else {
                        match flag_u16 {
                            11..=18 => 11_u16, // loPower
                            19..=26 => 13_u16, // medPower
                            27..=34 => 12_u16, // hiPower
                            92..=94 => 2663_u16, // rigSlot
                            125..=132 => 3772_u16, // subSystem
                            _ => 0,
                        }
                    };
                    if req_eff > 0 && !m_type.effect_ids.contains(&req_eff) {
                        return Err(BridgeRpcError {
                            code: "UserError".into(),
                            message: format!("SlotEffectMismatch: Type {tid} lacks required slot effect {req_eff} for slot {flag_u16}"),
                            client_error: None,
                        });
                    }
                    if (matches!(flag_u16, 92..=94) || m_type.effect_ids.contains(&2663)) && m_type.default_attributes.contains_key(&eve_sde::attr_ids::RIG_SIZE) {
                        let hull_rig_size = ship_type
                            .and_then(|t| t.default_attributes.get(&eve_sde::attr_ids::RIG_SIZE))
                            .copied()
                            .unwrap_or(0.0) as u32;
                        let mod_rig_size = m_type
                            .default_attributes
                            .get(&eve_sde::attr_ids::RIG_SIZE)
                            .copied()
                            .unwrap_or(0.0) as u32;
                        if mod_rig_size > 0 && hull_rig_size > 0 && mod_rig_size != hull_rig_size {
                            return Err(BridgeRpcError {
                                code: "UserError".into(),
                                message: format!("RigSizeMismatch: module rigSize {mod_rig_size} != hull rigSize {hull_rig_size}"),
                                client_error: None,
                            });
                        }
                    }
                }
            }

            // Rig calibration budget check
            let hull_upgrade_cap = ship_type
                .and_then(|t| t.default_attributes.get(&eve_sde::attr_ids::UPGRADE_CAPACITY))
                .copied()
                .unwrap_or(400.0);
            let existing_cal: f64 = snap.items.values()
                .filter(|i| i.location_id == ship_id && matches!(i.flag as u16, 92..=94))
                .filter_map(|i| ctx.sde.types.get(&i.type_id))
                .filter_map(|t| t.default_attributes.get(&eve_sde::attr_ids::UPGRADE_COST).copied())
                .sum();
            let new_cal: f64 = desired_items.iter()
                .filter(|(_, flag, _)| matches!(*flag as u16, 92..=94))
                .filter_map(|(tid, _, q)| {
                    ctx.sde.types.get(tid)
                        .and_then(|t| t.default_attributes.get(&eve_sde::attr_ids::UPGRADE_COST).copied())
                        .map(|c| c * (*q as f64))
                })
                .sum();
            if existing_cal + new_cal > hull_upgrade_cap + 1e-4 {
                return Err(BridgeRpcError {
                    code: "UserError".into(),
                    message: format!("UpgradeCapacityExceeded: total calibration cost {:.1} exceeds capacity {hull_upgrade_cap}", existing_cal + new_cal),
                    client_error: None,
                });
            }

            // Hardpoints limits check (turrets 102, launchers 101)
            let max_turrets = slot_preview_engine
                .ship
                .get_attribute(eve_sde::attr_ids::TURRET_SLOTS_LEFT, &ctx.sde)
                .max(0.0) as usize;
            let existing_turrets = snap.items.values()
                .filter(|i| i.location_id == ship_id && matches!(i.flag as u16, 27..=34))
                .filter(|i| ctx.sde.types.get(&i.type_id).is_some_and(|t| t.effect_ids.contains(&42)))
                .count();
            let new_turrets = desired_items.iter()
                .filter(|(_, flag, _)| matches!(*flag as u16, 27..=34))
                .filter(|(tid, _, _)| ctx.sde.types.get(tid).is_some_and(|t| t.effect_ids.contains(&42)))
                .count();
            if existing_turrets + new_turrets > max_turrets {
                return Err(BridgeRpcError {
                    code: "UserError".into(),
                    message: format!("InsufficientTurretHardpoints: turret limit {max_turrets} exceeded"),
                    client_error: None,
                });
            }

            let max_launchers = slot_preview_engine
                .ship
                .get_attribute(eve_sde::attr_ids::LAUNCHER_SLOTS_LEFT, &ctx.sde)
                .max(0.0) as usize;
            let existing_launchers = snap.items.values()
                .filter(|i| i.location_id == ship_id && matches!(i.flag as u16, 27..=34))
                .filter(|i| ctx.sde.types.get(&i.type_id).is_some_and(|t| t.effect_ids.contains(&40)))
                .count();
            let new_launchers = desired_items.iter()
                .filter(|(_, flag, _)| matches!(*flag as u16, 27..=34))
                .filter(|(tid, _, _)| ctx.sde.types.get(tid).is_some_and(|t| t.effect_ids.contains(&40)))
                .count();
            if existing_launchers + new_launchers > max_launchers {
                return Err(BridgeRpcError {
                    code: "UserError".into(),
                    message: format!("InsufficientLauncherHardpoints: launcher limit {max_launchers} exceeded"),
                    client_error: None,
                });
            }

            // maxGroupFitted (1544 / 1548) and maxTypeFitted (2431 or 1548->1544)
            for &(tid, flag, _) in desired_items {
                let flag_u16 = flag as u16;
                if matches!(flag_u16, 11..=34 | 92..=94 | 125..=132) {
                    let Some(m_type) = ctx.sde.types.get(&tid) else { continue; };
                    let group_limit = m_type
                        .default_attributes
                        .get(&1548)
                        .or_else(|| m_type.default_attributes.get(&1544))
                        .copied();
                    if let Some(limit_f) = group_limit {
                        let limit = limit_f as usize;
                        let existing = snap.items.values()
                            .filter(|i| i.location_id == ship_id && matches!(i.flag as u16, 11..=34 | 92..=94 | 125..=132))
                            .filter(|i| ctx.sde.types.get(&i.type_id).is_some_and(|t| t.group_id == m_type.group_id))
                            .count();
                        let planned = desired_items.iter()
                            .filter(|(_t, f, _)| matches!(*f as u16, 11..=34 | 92..=94 | 125..=132))
                            .filter(|(t, _, _)| ctx.sde.types.get(t).is_some_and(|ty| ty.group_id == m_type.group_id))
                            .count();
                        if existing + planned > limit {
                            return Err(BridgeRpcError {
                                code: "UserError".into(),
                                message: format!("MaxGroupFittedExceeded for group {}: limit {limit}", m_type.group_id),
                                client_error: None,
                            });
                        }
                    }
                    let type_limit = m_type
                        .default_attributes
                        .get(&2431)
                        .or_else(|| {
                            if m_type.default_attributes.contains_key(&1548) {
                                m_type.default_attributes.get(&1544)
                            } else {
                                None
                            }
                        })
                        .copied();
                    if let Some(limit_f) = type_limit {
                        let limit = limit_f as usize;
                        let existing = snap.items.values()
                            .filter(|i| i.location_id == ship_id && matches!(i.flag as u16, 11..=34 | 92..=94 | 125..=132) && i.type_id == tid)
                            .count();
                        let planned = desired_items.iter()
                            .filter(|(t, f, _)| matches!(*f as u16, 11..=34 | 92..=94 | 125..=132) && *t == tid)
                            .count();
                        if existing + planned > limit {
                            return Err(BridgeRpcError {
                                code: "UserError".into(),
                                message: format!("MaxTypeFittedExceeded for type {tid}: limit {limit}"),
                                client_error: None,
                            });
                        }
                    }
                }
            }

            // Cargo capacity check (flag 5)
            let ship_cargo_cap = ship_type.map(|t| t.capacity).unwrap_or(0.0);
            let existing_cargo_vol: f64 = snap.items.values()
                .filter(|i| i.location_id == ship_id && i.flag == eve_db::InventoryFlag::Cargo)
                .map(|i| crate::services::inventory_fitting::fitting_containers_ships::effective_item_unit_volume(&ctx.sde, i) * (i.quantity as f64))
                .sum();
            let new_cargo_vol: f64 = desired_items.iter()
                .filter(|(_, flag, _)| *flag == eve_db::InventoryFlag::Cargo)
                .map(|(tid, _, qty)| {
                    let vol = ctx.sde.types.get(tid).map(|t| t.volume).unwrap_or(0.0);
                    vol * (*qty as f64)
                })
                .sum();
            if (new_cargo_vol > 0.0 || existing_cargo_vol > 0.0) && (existing_cargo_vol + new_cargo_vol > ship_cargo_cap + 1e-4) {
                return Err(BridgeRpcError {
                    code: "CapacityExceeded".into(),
                    message: format!("Cargo hold capacity {ship_cargo_cap} m³ exceeded: total cargo volume is {:.1} m³", existing_cargo_vol + new_cargo_vol),
                    client_error: None,
                });
            }

            // Drone bay check (flag 87)
            let ship_drone_cap = ship_type
                .and_then(|t| t.default_attributes.get(&eve_sde::attr_ids::DRONE_CAPACITY))
                .copied()
                .unwrap_or(0.0);
            let mut new_drone_vol: f64 = 0.0;
            for &(tid, flag, req_qty) in desired_items {
                if flag == eve_db::InventoryFlag::DroneBay {
                    let Some(d_type) = ctx.sde.types.get(&tid) else {
                        return Err(BridgeRpcError { code: "ItemNotFound".into(), message: format!("Type {tid} not found"), client_error: None });
                    };
                    if d_type.category_id != 18 {
                        return Err(BridgeRpcError {
                            code: "ItemNotADrone".into(),
                            message: format!("Type {tid} is category {}, not a drone (category 18)", d_type.category_id),
                            client_error: None,
                        });
                    }
                    new_drone_vol += d_type.volume * (req_qty as f64);
                }
            }
            let existing_drone_vol: f64 = snap.items.values()
                .filter(|i| i.location_id == ship_id && i.flag == eve_db::InventoryFlag::DroneBay)
                .map(|i| ctx.sde.types.get(&i.type_id).map(|t| t.volume).unwrap_or(0.0) * (i.quantity as f64))
                .sum();
            if (new_drone_vol > 0.0 || existing_drone_vol > 0.0) && (existing_drone_vol + new_drone_vol > ship_drone_cap + 1e-4) {
                return Err(BridgeRpcError {
                    code: "CapacityExceeded".into(),
                    message: format!("Drone bay capacity {ship_drone_cap} m³ exceeded: total drone volume is {:.1} m³", existing_drone_vol + new_drone_vol),
                    client_error: None,
                });
            }

            // Fighter bay check (flag 158)
            let ship_fighter_cap = ship_type
                .and_then(|t| t.default_attributes.get(&2055))
                .copied()
                .unwrap_or(0.0);
            let mut new_fighter_vol: f64 = 0.0;
            for &(tid, flag, req_qty) in desired_items {
                if flag as u16 == 158 {
                    if ship_fighter_cap <= 0.0 {
                        return Err(BridgeRpcError {
                            code: "ShipHasNoFighterBay".into(),
                            message: "Ship has no fighter bay capacity".into(),
                            client_error: None,
                        });
                    }
                    let Some(f_type) = ctx.sde.types.get(&tid) else {
                        return Err(BridgeRpcError { code: "ItemNotFound".into(), message: format!("Type {tid} not found"), client_error: None });
                    };
                    if f_type.category_id != 87 {
                        return Err(BridgeRpcError {
                            code: "ItemNotAFighter".into(),
                            message: format!("Type {tid} is category {}, not a fighter (category 87)", f_type.category_id),
                            client_error: None,
                        });
                    }
                    new_fighter_vol += f_type.volume * (req_qty as f64);
                }
            }
            if new_fighter_vol > 0.0 {
                let existing_fighter_vol: f64 = snap.items.values()
                    .filter(|i| i.location_id == ship_id && i.flag as u16 == 158)
                    .map(|i| ctx.sde.types.get(&i.type_id).map(|t| t.volume).unwrap_or(0.0) * (i.quantity as f64))
                    .sum();
                if existing_fighter_vol + new_fighter_vol > ship_fighter_cap + 1e-4 {
                    return Err(BridgeRpcError {
                        code: "CapacityExceeded".into(),
                        message: format!("Fighter bay capacity {ship_fighter_cap} m³ exceeded: total fighter volume is {:.1} m³", existing_fighter_vol + new_fighter_vol),
                        client_error: None,
                    });
                }
            }

            for &(tid, flag, _) in desired_items {
                let flag_u16 = flag as u16;
                let is_specialized_hold = matches!(flag_u16, 133..=151 | 177 | 179 | 181 | 182 | 185);
                if is_specialized_hold {
                    let Some(item_type) = ctx.sde.types.get(&tid) else {
                        return Err(BridgeRpcError {
                            code: "ItemNotFound".into(),
                            message: format!("Type {tid} not found"),
                            client_error: None,
                        });
                    };
                    let allowed_in_hold = match flag_u16 {
                        133 => matches!(item_type.group_id, 423 | 497 | 1136),
                        134 | 151 => item_type.category_id == 25 || matches!(item_type.group_id, 711 | 4168),
                        135 => matches!(item_type.group_id, 711 | 4168),
                        136 => item_type.group_id == 18,
                        143 => item_type.category_id == 8,
                        148 => item_type.group_id == 1027,
                        149 => matches!(item_type.category_id, 42 | 43),
                        177 => matches!(item_type.group_id, 25 | 324 | 830 | 831 | 834 | 893 | 1283 | 1527 | 2001),
                        179 => matches!(item_type.group_id, 4168 | 25 | 324 | 830 | 831 | 834 | 893 | 1283 | 1527 | 2001),
                        181 => item_type.category_id == 25,
                        150 | 185 => matches!(item_type.category_id, 23 | 39 | 40 | 41 | 42 | 43 | 46 | 65 | 66 | 2143)
                            || matches!(item_type.group_id, 423 | 1136 | 4729),
                        182 => if ship_type.map(|t| t.default_attributes.get(&5646).copied().unwrap_or(0.0) > 0.0).unwrap_or(false) {
                            matches!(item_type.category_id, 23 | 39 | 40 | 41 | 42 | 43 | 46 | 65 | 66 | 2143)
                                || matches!(item_type.group_id, 423 | 1136 | 4729)
                        } else {
                            item_type.category_id == 25
                        },
                        _ => true,
                    };
                    if !allowed_in_hold {
                        return Err(BridgeRpcError {
                            code: "InvalidItemTypeForHold".into(),
                            message: format!("Type {tid} cannot be placed in specialized hold flag {flag_u16}"),
                            client_error: None,
                        });
                    }
                }
            }
            let mut checked_spec_flags: Vec<u16> = desired_items.iter().map(|(_, f, _)| *f as u16).filter(|&fl| matches!(fl, 133..=151 | 177 | 179 | 181 | 182 | 185)).collect();
            checked_spec_flags.sort_unstable();
            checked_spec_flags.dedup();
            for flg in checked_spec_flags {
                let hold_capacity = match flg {
                    133 => ship_type.and_then(|t| t.default_attributes.get(&1549).copied()).unwrap_or(0.0),
                    134 | 151 => ship_type.and_then(|t| t.default_attributes.get(&1556).copied().or_else(|| t.default_attributes.get(&1770).copied())).unwrap_or(0.0),
                    136 => ship_type.and_then(|t| t.default_attributes.get(&1558).copied()).unwrap_or(0.0),
                    135 => ship_type.and_then(|t| t.default_attributes.get(&1557).copied().or_else(|| t.default_attributes.get(&1556).copied())).unwrap_or(0.0),
                    177 | 179 => ship_type.and_then(|t| t.default_attributes.get(&3020).copied().or_else(|| t.default_attributes.get(&5325).copied())).unwrap_or(0.0),
                    181 => ship_type.and_then(|t| t.default_attributes.get(&3136).copied().or_else(|| t.default_attributes.get(&1556).copied())).unwrap_or(0.0),
                    150 | 185 => ship_type.and_then(|t| t.default_attributes.get(&5646).copied()).unwrap_or(0.0),
                    182 => ship_type.and_then(|t| t.default_attributes.get(&5646).copied().or_else(|| t.default_attributes.get(&3227).copied())).unwrap_or(0.0),
                    143 => ship_type.and_then(|t| t.default_attributes.get(&1573).copied()).unwrap_or(0.0),
                    148 => ship_type.and_then(|t| t.default_attributes.get(&1646).copied()).unwrap_or(5_000.0),
                    149 => ship_type.and_then(|t| t.default_attributes.get(&1653).copied()).unwrap_or(0.0),
                    _ => 0.0,
                };
                if hold_capacity <= 0.0 {
                    return Err(BridgeRpcError {
                        code: "CapacityExceeded".into(),
                        message: format!("Ship does not have specialized hold flag {flg}"),
                        client_error: None,
                    });
                }
                let existing_vol: f64 = snap.items.values()
                    .filter(|i| i.location_id == ship_id && i.flag as u16 == flg)
                    .map(|i| ctx.sde.types.get(&i.type_id).map(|t| t.volume).unwrap_or(0.0) * (i.quantity as f64))
                    .sum();
                let new_vol: f64 = desired_items.iter()
                    .filter(|(_, f, _)| *f as u16 == flg)
                    .map(|(tid, _, qty)| ctx.sde.types.get(tid).map(|t| t.volume).unwrap_or(0.0) * (*qty as f64))
                    .sum();
                if existing_vol + new_vol > hold_capacity + 1e-4 {
                    return Err(BridgeRpcError {
                        code: "CapacityExceeded".into(),
                        message: format!("Specialized hold flag {flg} capacity {hold_capacity} m³ exceeded: total {:.1} m³", existing_vol + new_vol),
                        client_error: None,
                    });
                }
            }
        }
        let mut seen_mod_slots = std::collections::HashSet::new();
        let mut seen_charge_slots = std::collections::HashSet::new();
        for &(tid, flag, req_qty) in desired_items {
            if req_qty <= 0 {
                return Err(BridgeRpcError {
                    code: "InvalidQuantity".into(),
                    message: "Fitting item quantity must be positive".into(),
                    client_error: None,
                });
            }
            let flag_u16 = flag as u16;
            let is_allowed_flag = matches!(
                flag_u16,
                5 | 87 | 11..=34 | 92..=94 | 125..=132 | 158 | 133..=151 | 177 | 179 | 181 | 182 | 185
            );
            if !is_allowed_flag {
                return Err(BridgeRpcError {
                    code: "InvalidFittingFlag".into(),
                    message: format!("Flag {flag_u16} is not a valid fitting destination"),
                    client_error: None,
                });
            }
            if matches!(flag_u16, 11..=34 | 92..=94 | 125..=132) {
                let cat = ctx.sde.types.get(&tid).map(|t| t.category_id).unwrap_or(0);
                if !matches!(cat, 7 | 8 | 32) || (cat != 8 && req_qty != 1) {
                    return Err(BridgeRpcError {
                        code: "ItemNotFittableModule".into(),
                        message: format!("Type {tid} (qty {req_qty}) cannot be fitted to slot {flag_u16}"),
                        client_error: None,
                    });
                }
                let seen_ok = if cat == 8 {
                    seen_charge_slots.insert(flag_u16)
                } else {
                    seen_mod_slots.insert(flag_u16)
                };
                if !seen_ok {
                    return Err(BridgeRpcError {
                        code: "SlotAlreadyOccupied".into(),
                        message: format!("Duplicate assignment to slot flag {flag_u16}"),
                        client_error: None,
                    });
                }
            }
        }
        Ok(())
    });
    if let Err(e) = ship_check {
        return Err(e);
    }

    let mut sorted_desired = desired_items.to_vec();
    sorted_desired.sort_by_key(|&(tid, flag, _)| {
        let cat = ctx.sde.types.get(&tid).map(|t| t.category_id).unwrap_or(0);
        if cat == 8 {
            return (5u8, flag as u16);
        }
        match flag as u16 {
            125..=132 => (0u8, flag as u16),
            92..=94 => (1u8, flag as u16),
            11..=18 => (2u8, flag as u16),
            19..=26 => (3u8, flag as u16),
            27..=34 => (4u8, flag as u16),
            f => (6u8, f),
        }
    });

    let mut fitted_count = 0_i64;
    let mut missing: HashMap<TypeId, i64> = HashMap::new();
    let mut moved_items: Vec<eve_db::InventoryItemRecord> = Vec::new();
    let mut fitted_online_states: HashMap<i64, bool> = HashMap::new();
    let mut failed_ship_id: Option<i64> = None;
    let char_skills = ctx.character_skill_levels(char_id);
    let before_attrs_by_ship: HashMap<i64, std::collections::BTreeMap<u16, f64>> = target_ship_ids
        .iter()
        .filter_map(|&sid| {
            crate::services::notify_emitters_skills_trade_godma::snapshot_ship_fitting_attrs_for_ship(
                ctx, char_id, sid,
            )
            .map(|attrs| (sid, attrs))
        })
        .collect();
    ctx.db.with_mut_snapshot(|snap| {
        for &ship_id in target_ship_ids {
            let ship_type_id = snap.items.get(&ship_id).map(|s| s.type_id).unwrap_or(0);
            let mut engine = eve_dogma::ShipFittingEngine::from_sde(ship_id, ship_type_id, &ctx.sde)
                .with_character_skills(char_skills.clone());
            for it in snap.items.values() {
                if it.location_id == ship_id
                    && matches!(it.flag as u16, 11..=34 | 92..=94 | 125..=132)
                    && ctx.sde.types.get(&it.type_id).map(|t| t.category_id) != Some(8)
                {
                    let is_on = snap.module_online_states.get(&it.item_id).copied().unwrap_or(true);
                    let _ = engine.fit_module_with_state(it.item_id, it.type_id, it.flag as u16, is_on, &ctx.sde);
                }
            }

            if let Some(sh) = snap.items.get_mut(&ship_id) {
                sh.singleton = true;
                sh.version += 1;
            }
            let mut ship_fitted = false;
            let mut ship_failed = false;
            for &(tid, flag, req_qty) in &sorted_desired {
                let flag_u16 = flag as u16;
                let cat = ctx.sde.types.get(&tid).map(|t| t.category_id).unwrap_or(0);
                let is_mod = matches!(flag_u16, 11..=34 | 92..=94 | 125..=132) && cat != 8;
                if is_mod
                    && snap.items.values().any(|i| {
                        i.location_id == ship_id
                            && i.flag == flag
                            && ctx.sde.types.get(&i.type_id).map(|t| t.category_id) != Some(8)
                    })
                {
                    continue;
                }
                let stack_opts = eve_db::ConsumeExactOptions::for_owner_exact(
                    Some(char_id),
                    char_id,
                    eve_db::ItemClassPolicy::StackableCommodity,
                );
                let whole_singleton_opts = eve_db::ConsumeExactOptions {
                    reject_blueprint_copy: true,
                    ..eve_db::ConsumeExactOptions::for_owner_exact(
                        Some(char_id),
                        char_id,
                        eve_db::ItemClassPolicy::ExactBlueprintOrSingleton,
                    )
                };
                let is_valid_fit_stack =
                    |snap_ref: &eve_db::WorldStateSnapshot, it: &eve_db::InventoryItemRecord| -> bool {
                        if it.singleton {
                            it.quantity == 1
                                && snap_ref
                                    .validate_item_for_consumption(it, &whole_singleton_opts)
                                    .is_ok()
                        } else {
                            snap_ref
                                .validate_item_for_consumption(it, &stack_opts)
                                .is_ok()
                        }
                    };
                let preferred_ids = options
                    .preferred_items_by_type
                    .and_then(|map| map.get(&tid));
                let mut matching_stacks: Vec<(i64, i32)> = snap
                    .items
                    .values()
                    .filter(|i| {
                        i.owner_id == char_id
                            && i.location_id == station_id
                            && i.flag == InventoryFlag::Hangar
                            && i.type_id == tid
                            && i.type_id != 60
                            && i.quantity > 0
                            && !snap.is_item_in_alsc_lock(i.item_id)
                            && is_valid_fit_stack(snap, i)
                    })
                    .map(|i| (i.item_id, i.quantity))
                    .collect();
                matching_stacks.sort_by_key(|&(item_id, _)| {
                    let is_preferred = preferred_ids.is_some_and(|ids| ids.contains(&item_id));
                    (!is_preferred, item_id)
                });
                let total_avail: i64 = matching_stacks.iter().map(|(_, q)| *q as i64).sum();
                if total_avail == 0 {
                    *missing.entry(tid).or_insert(0) += req_qty as i64;
                    ship_failed = true;
                    continue;
                }
                let online = if is_mod {
                    let dummy_mod_id = snap.next_entity_id + 10_000 + flag_u16 as i64;
                    crate::domain::fitting::fit_and_try_online_in_engine(
                        &ctx.sde,
                        &mut engine,
                        dummy_mod_id,
                        tid,
                        flag,
                        crate::domain::fitting::FitOnlinePolicy::CheckSkillsAndResources,
                    )
                    .unwrap_or(false)
                } else {
                    true
                };
                let mut to_consume = req_qty;
                for (cid, avail) in matching_stacks {
                    if to_consume <= 0 {
                        break;
                    }
                    let take = avail.min(to_consume);
                    if take == avail && (!is_mod || take == 1) {
                        if !snap
                            .items
                            .get(&cid)
                            .is_some_and(|it| is_valid_fit_stack(snap, it))
                        {
                            continue;
                        }
                        if let Ok(mut moved) = snap.transfer_inventory_item(
                            cid,
                            eve_db::TransferItemSpec::move_whole(
                                Some(char_id),
                                Some(char_id),
                                None,
                                ship_id,
                                flag,
                            ),
                        ) {
                            moved.singleton = is_mod || take == 1;
                            if let Some(it) = snap.items.get_mut(&cid) {
                                it.singleton = moved.singleton;
                            }
                            moved_items.push(moved);
                            ship_fitted = true;
                            if is_mod {
                                crate::domain::fitting::record_fitted_module_online_state_in_snap(
                                    snap, cid, online,
                                );
                                fitted_online_states.insert(cid, online);
                            }
                        } else {
                            continue;
                        }
                    } else {
                        if snap
                            .consume_exact_item_quantity(cid, take, stack_opts.clone())
                            .is_err()
                        {
                            continue;
                        }
                        snap.next_entity_id += 1;
                        let split_id = snap.next_entity_id;
                        let new_item = eve_db::InventoryItemRecord {
                            item_id: split_id,
                            type_id: tid,
                            owner_id: char_id,
                            location_id: ship_id,
                            flag,
                            quantity: take,
                            singleton: is_mod || take == 1,
                            version: 1,
                            ..Default::default()
                        };
                        snap.items.insert(split_id, new_item.clone());
                        moved_items.push(new_item);
                        ship_fitted = true;
                        if is_mod {
                            crate::domain::fitting::record_fitted_module_online_state_in_snap(
                                snap, split_id, online,
                            );
                            fitted_online_states.insert(split_id, online);
                        }
                    }
                    to_consume -= take;
                }
                if to_consume > 0 {
                    *missing.entry(tid).or_insert(0) += to_consume as i64;
                    ship_failed = true;
                    continue;
                }
            }
            if ship_fitted {
                fitted_count += 1;
            }
            if ship_failed && failed_ship_id.is_none() {
                failed_ship_id = Some(ship_id);
            }
        }
    });

    for &ship_id in target_ship_ids {
        ctx.invalidate_ship_fitting(ship_id);
    }
    let mut fit_by_ship: HashMap<i64, Option<eve_dogma::ShipFittingEngine>> = HashMap::new();
    if fitted_count > 0 && !moved_items.is_empty() && emit_single_item_change {
        for item in &moved_items {
            if let Some(&online) = fitted_online_states.get(&item.item_id) {
                let fit_opt = fit_by_ship
                    .entry(item.location_id)
                    .or_insert_with(|| ctx.get_or_rebuild_ship_fitting(char_id, item.location_id).ok());
                crate::services::inventory_fitting::emit_module_fitted_notifications(
                    ctx,
                    char_id,
                    item.location_id,
                    item,
                    online,
                    false,
                    None,
                    fit_opt.as_ref(),
                    false,
                );
            }
        }
    }
    if fitted_count > 0 {
        for &ship_id in target_ship_ids {
            crate::services::notify_emitters_skills_trade_godma::emit_ship_fitting_attr_changes_for_ship(
                ctx,
                char_id,
                ship_id,
                before_attrs_by_ship.get(&ship_id),
                true,
            );
        }
    }
    Ok((fitted_count, missing, moved_items, failed_ship_id))
}

pub(crate) fn is_item_allowed_in_ship_hold(
    flag_u16: u16,
    item_type: &eve_sde::InvType,
    ship_type: &eve_sde::InvType,
) -> bool {
    match flag_u16 {
        5 => true,
        87 => item_type.category_id == 18,
        158 => item_type.category_id == 87,
        133 => matches!(item_type.group_id, 423 | 497 | 1136),
        134 | 151 => item_type.category_id == 25 || matches!(item_type.group_id, 711 | 4168),
        135 => matches!(item_type.group_id, 711 | 4168),
        136 => item_type.group_id == 18,
        143 => item_type.category_id == 8,
        148 => item_type.group_id == 1027,
        149 => matches!(item_type.category_id, 42 | 43),
        // Frigate Escape Bay
        177 | 179 => {
            item_type.group_id == 1246
                || (ship_type.default_attributes.get(&3020).copied().unwrap_or(0.0) > 0.0
                    && matches!(
                        item_type.group_id,
                        25 | 324 | 830 | 831 | 834 | 893 | 1283 | 1527 | 2001
                    ))
        }
        181 => item_type.group_id == 465,
        150 | 185 => {
            matches!(item_type.category_id, 23 | 39 | 40 | 41 | 42 | 43 | 46 | 65 | 66 | 2143)
                || matches!(item_type.group_id, 423 | 1136 | 4729)
        }
        182 => {
            if ship_type.default_attributes.get(&5646).copied().unwrap_or(0.0) > 0.0 {
                matches!(item_type.category_id, 23 | 39 | 40 | 41 | 42 | 43 | 46 | 65 | 66 | 2143)
                    || matches!(item_type.group_id, 423 | 1136 | 4729)
            } else {
                item_type.category_id == 25
            }
        }
        _ => true,
    }
}

pub(crate) fn get_ship_hold_capacity(
    flag_u16: u16,
    ship_type: &eve_sde::InvType,
    eval_cargo_cap: Option<f64>,
) -> f64 {
    match flag_u16 {
        5 => eval_cargo_cap.unwrap_or(ship_type.capacity),
        87 => ship_type.default_attributes.get(&eve_sde::attr_ids::DRONE_CAPACITY).copied().unwrap_or(0.0),
        158 => ship_type.default_attributes.get(&2055).copied().unwrap_or(0.0),
        133 => ship_type.default_attributes.get(&1549).copied().unwrap_or(0.0),
        134 | 151 => ship_type.default_attributes.get(&1556).copied()
            .or_else(|| ship_type.default_attributes.get(&1770).copied())
            .unwrap_or(0.0),
        136 => ship_type.default_attributes.get(&1558).copied().unwrap_or(0.0),
        135 => ship_type.default_attributes.get(&1557).copied()
            .or_else(|| ship_type.default_attributes.get(&1556).copied())
            .unwrap_or(0.0),
        177 | 179 => ship_type
            .default_attributes
            .get(&3020)
            .copied()
            .or_else(|| ship_type.default_attributes.get(&5325).copied())
            .unwrap_or(0.0),
        181 => ship_type.default_attributes.get(&3136).copied()
            .or_else(|| ship_type.default_attributes.get(&1556).copied())
            .unwrap_or(0.0),
        150 | 185 => ship_type.default_attributes.get(&5646).copied().unwrap_or(0.0),
        182 => ship_type.default_attributes.get(&5646).copied()
            .or_else(|| ship_type.default_attributes.get(&3227).copied())
            .unwrap_or(0.0),
        143 => ship_type.default_attributes.get(&1573).copied().unwrap_or(0.0),
        148 => ship_type.default_attributes.get(&1646).copied().unwrap_or(5_000.0),
        149 => ship_type.default_attributes.get(&1653).copied().unwrap_or(0.0),
        _ => 0.0,
    }
}

/// Encodes a ship fitting into an EVE DNA fitting string (`INV-3`):
/// `<ship_type_id>:<type_id_1>;<qty_1>:<type_id_2>;<qty_2>:...::`
pub fn fitting_to_dna(ship_type_id: TypeId, items: &[(TypeId, i32)]) -> String {
    let mut counts: HashMap<TypeId, i32> = HashMap::new();
    let mut order: Vec<TypeId> = Vec::new();
    for &(type_id, qty) in items {
        if qty <= 0 {
            continue;
        }
        if !counts.contains_key(&type_id) {
            order.push(type_id);
        }
        *counts.entry(type_id).or_insert(0) += qty;
    }
    let mut out = format!("{ship_type_id}:");
    for type_id in order {
        let qty = counts[&type_id];
        out.push_str(&format!("{type_id};{qty}:"));
    }
    out.push(':');
    out
}

/// Encodes a `SavedFittingRecord` into an EVE DNA fitting string (`INV-3`).
pub fn saved_fitting_to_dna(fitting: &SavedFittingRecord) -> String {
    let items: Vec<(TypeId, i32)> = fitting.items.iter().map(|&(t, _, q)| (t, q)).collect();
    fitting_to_dna(fitting.ship_type_id, &items)
}

/// Decodes an EVE DNA fitting string into `(ship_type_id, Vec<(TypeId, i32)>)` (`INV-3`).
/// Supports standard format `<ship_type_id>:<type_id_1>;<qty_1>:<type_id_2>;<qty_2>:...::`
pub fn dna_to_fitting(dna: &str) -> Result<(TypeId, Vec<(TypeId, i32)>), String> {
    let trimmed = dna.trim();
    if trimmed.is_empty() {
        return Err("Empty DNA string".into());
    }
    let mut parts = trimmed.split(':');
    let ship_str = parts.next().ok_or("Missing ship type ID")?;
    let ship_type_id = ship_str
        .parse::<TypeId>()
        .map_err(|e| format!("Invalid ship type ID '{ship_str}': {e}"))?;

    let mut items = Vec::new();
    for part in parts {
        let token = part.trim();
        if token.is_empty() {
            continue;
        }
        let (type_str, qty_str) = match token.split_once(';') {
            Some((t, q)) => (t.trim(), q.trim()),
            None => (token, "1"),
        };
        let type_id = type_str
            .parse::<TypeId>()
            .map_err(|e| format!("Invalid item type ID '{type_str}': {e}"))?;
        let qty = qty_str
            .parse::<i32>()
            .map_err(|e| format!("Invalid quantity '{qty_str}': {e}"))?;
        if qty > 0 {
            items.push((type_id, qty));
        }
    }
    Ok((ship_type_id, items))
}

#[cfg(test)]
#[path = "fitting_saved_apply_tests.rs"]
mod fitting_saved_apply_tests;

#[cfg(test)]
mod r5_fit_apply_policy_tests {
    use super::*;
    use eve_db::{CharacterRecord, InventoryItemRecord, SavedFittingRecord, WorldRepository};
    use eve_sde::SdeRegistry;
    use std::sync::{Arc, OnceLock};

    fn test_sde() -> Arc<SdeRegistry> {
        static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
        Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
    }

    #[tokio::test]
    async fn handle_fit_ships_rejects_bpc_and_courier_in_both_whole_and_split_branches() {
        let ctx = ServerContext::with_sde_and_repository(
            test_sde(),
            Arc::new(WorldRepository::with_demo_seed()),
        );
        let char_id = 96_050_001_i64;
        let station_id = 60_003_760_i64;
        let ship_id = 96_050_100_i64;
        let mod_bpc_id = 96_050_201_i64;
        let mod_courier_id = 96_050_202_i64;
        let ammo_exact_bpc_id = 96_050_203_i64;
        let ammo_split_courier_id = 96_050_204_i64;
        let clean_mod_id = 96_050_205_i64;
        let clean_ammo_id = 96_050_206_i64;

        ctx.db.with_mut_snapshot(|snap| {
            let seed = snap.characters.values().next().cloned().expect("seed char");
            snap.characters.insert(
                char_id,
                CharacterRecord {
                    character_id: char_id,
                    user_id: 1,
                    name: "FitApplyPolicyPilot".into(),
                    corporation_id: 1_000_009,
                    station_id: Some(station_id),
                    solar_system_id: 30_000_142,
                    active_ship_id: ship_id,
                    ..seed
                },
            );
            snap.items.insert(
                ship_id,
                InventoryItemRecord {
                    item_id: ship_id,
                    type_id: 587,
                    owner_id: char_id,
                    location_id: station_id,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.items.insert(
                mod_bpc_id,
                InventoryItemRecord {
                    item_id: mod_bpc_id,
                    type_id: 484,
                    owner_id: char_id,
                    location_id: station_id,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    is_blueprint_copy: true,
                    ..Default::default()
                },
            );
            snap.items.insert(
                mod_courier_id,
                InventoryItemRecord {
                    item_id: mod_courier_id,
                    type_id: 484,
                    owner_id: char_id,
                    location_id: station_id,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    ..Default::default()
                },
            );
            snap.item_custom_names
                .insert(mod_courier_id, "courier_cargo:51".into());
            snap.items.insert(
                ammo_exact_bpc_id,
                InventoryItemRecord {
                    item_id: ammo_exact_bpc_id,
                    type_id: 178,
                    owner_id: char_id,
                    location_id: station_id,
                    flag: InventoryFlag::Hangar,
                    quantity: 5,
                    is_blueprint_copy: true,
                    ..Default::default()
                },
            );
            snap.items.insert(
                ammo_split_courier_id,
                InventoryItemRecord {
                    item_id: ammo_split_courier_id,
                    type_id: 178,
                    owner_id: char_id,
                    location_id: station_id,
                    flag: InventoryFlag::Hangar,
                    quantity: 10,
                    ..Default::default()
                },
            );
            snap.item_custom_names
                .insert(ammo_split_courier_id, "courier_cargo:52".into());
            snap.saved_fittings.insert(
                502,
                SavedFittingRecord {
                    fitting_id: 502,
                    owner_id: char_id,
                    ship_type_id: 587,
                    name: "Policy Fit".into(),
                    description: "R5 policy check".into(),
                    items: vec![
                        (484, InventoryFlag::HiSlot0, 1),
                        (178, InventoryFlag::Cargo, 5),
                    ],
                },
            );
        });

        let mut session = SessionState::default();
        session.user_id = Some(1);
        session.character_id = Some(char_id);
        session.corporation_id = Some(1_000_009);
        session.station_id = Some(station_id);
        session.solar_system_id = Some(30_000_142);
        session.ship_id = Some(ship_id);

        let res = handle_fit_ships(
            &ctx,
            &mut session,
            &[PyRep::List(vec![PyRep::Integer(ship_id)]), PyRep::Integer(502)],
            &HashMap::new(),
        )
        .await
        .expect("handled")
        .expect("FitShips tuple");
        match res {
            PyRep::Tuple(parts) if parts.len() == 2 => {
                assert_eq!(parts[1], PyRep::Integer(ship_id));
                match &parts[0] {
                    PyRep::List(entries) => {
                        assert_eq!(entries.len(), 2);
                        assert!(entries.contains(&PyRep::Tuple(vec![
                            PyRep::Integer(484),
                            PyRep::Integer(1),
                        ])));
                        assert!(entries.contains(&PyRep::Tuple(vec![
                            PyRep::Integer(178),
                            PyRep::Integer(5),
                        ])));
                    }
                    other => panic!("expected missing list, got {other:?}"),
                }
            }
            other => panic!("expected FitShips tuple, got {other:?}"),
        }
        ctx.db.with_snapshot(|snap| {
            for (id, qty) in [
                (mod_bpc_id, 1),
                (mod_courier_id, 1),
                (ammo_exact_bpc_id, 5),
                (ammo_split_courier_id, 10),
            ] {
                assert_eq!(snap.items[&id].location_id, station_id);
                assert_eq!(snap.items[&id].flag, InventoryFlag::Hangar);
                assert_eq!(snap.items[&id].quantity, qty);
            }
        });

        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                clean_mod_id,
                InventoryItemRecord {
                    item_id: clean_mod_id,
                    type_id: 484,
                    owner_id: char_id,
                    location_id: station_id,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            snap.items.insert(
                clean_ammo_id,
                InventoryItemRecord {
                    item_id: clean_ammo_id,
                    type_id: 178,
                    owner_id: char_id,
                    location_id: station_id,
                    flag: InventoryFlag::Hangar,
                    quantity: 8,
                    ..Default::default()
                },
            );
        });

        let ok = handle_fit_ships(
            &ctx,
            &mut session,
            &[PyRep::List(vec![PyRep::Integer(ship_id)]), PyRep::Integer(502)],
            &HashMap::new(),
        )
        .await
        .expect("handled")
        .expect("clean fit");
        assert_eq!(ok, PyRep::Tuple(vec![PyRep::List(vec![]), PyRep::None]));
        ctx.db.with_snapshot(|snap| {
            assert_eq!(snap.items[&clean_mod_id].location_id, ship_id);
            assert_eq!(snap.items[&clean_mod_id].flag, InventoryFlag::HiSlot0);
            assert_eq!(snap.items[&clean_ammo_id].location_id, station_id);
            assert_eq!(snap.items[&clean_ammo_id].flag, InventoryFlag::Hangar);
            assert_eq!(snap.items[&clean_ammo_id].quantity, 3);
            assert!(snap.items.values().any(|i| {
                i.location_id == ship_id
                    && i.flag == InventoryFlag::Cargo
                    && i.type_id == 178
                    && i.quantity == 5
            }));
            for (id, qty) in [
                (mod_bpc_id, 1),
                (mod_courier_id, 1),
                (ammo_exact_bpc_id, 5),
                (ammo_split_courier_id, 10),
            ] {
                assert_eq!(snap.items[&id].location_id, station_id);
                assert_eq!(snap.items[&id].flag, InventoryFlag::Hangar);
                assert_eq!(snap.items[&id].quantity, qty);
            }
        });
    }
}


