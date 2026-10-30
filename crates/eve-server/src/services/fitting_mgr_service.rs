//! `fitting_mgr_service` — `charFittingMgr`, `corpFittingMgr`, and `allianceFittingMgr` RPC handlers.

#![forbid(unsafe_code)]

use super::*;

fn resolve_and_authorize_fitting_owner(
    ctx: &ServerContext,
    session: &SessionState,
    service: &str,
    char_id: i64,
    owner_filter: Option<i64>,
    action_label: &str,
) -> Result<i64, BridgeRpcError> {
    let expected_owner = if service.starts_with("corp") {
        session.corporation_id.unwrap_or(0)
    } else if service.starts_with("alliance") {
        session.alliance_id.unwrap_or(0)
    } else {
        char_id
    };
    let target_owner = owner_filter.filter(|&oid| oid > 0).unwrap_or(expected_owner);
    if target_owner != expected_owner {
        return Err(BridgeRpcError {
            code: "UserError".into(),
            message: "FittingOwnerMismatch".into(),
        });
    }
    if service.starts_with("corp") && ctx.db.with_snapshot(|s| s.corporations.contains_key(&target_owner)) {
        let (is_ceo, roles) = get_effective_corp_roles(ctx, target_owner, char_id);
        let has_role = is_ceo || eve_db::corp_roles::has_role(roles, eve_db::corp_roles::ROLE_DIRECTOR | (1u64 << 41));
        if !has_role {
            return Err(BridgeRpcError {
                code: "CorpRoleDenied".into(),
                message: format!("{action_label} corporation fittings requires Fitting Manager or Director role"),
            });
        }
    }
    Ok(target_owner)
}

pub(crate) async fn handle_fitting_mgr(
    ctx: &ServerContext,
    session: &mut SessionState,
    _bound_entry: Option<&BoundObjectEntry>,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("charFittingMgr", "SaveFitting") | ("corpFittingMgr", "SaveFitting") | ("allianceFittingMgr", "SaveFitting") | ("charFittingMgr", "SaveManyFittings") | ("corpFittingMgr", "SaveManyFittings") | ("allianceFittingMgr", "SaveManyFittings") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let default_owner = match service {
                "corpFittingMgr" => session.corporation_id.unwrap_or(char_id),
                "allianceFittingMgr" => session
                    .alliance_id
                    .or(session.corporation_id)
                    .unwrap_or(char_id),
                _ => char_id,
            };

            let parse_fit_items = |items_arg: Option<&PyRep>,
                                   ship_type_id: TypeId|
             -> Result<Vec<(TypeId, InventoryFlag, i32)>, BridgeRpcError> {
                if let Some(arg) = items_arg {
                    let entries = match arg {
                        PyRep::List(v) | PyRep::Tuple(v) => v,
                        _ => {
                            return Err(BridgeRpcError {
                                code: "InvalidFittingItems".into(),
                                message:
                                    "Fitting items must be a List or Tuple of (typeID, flagID, quantity)"
                                        .into(),
                            })
                        }
                    };
                    let mut out = Vec::with_capacity(entries.len());
                    for entry in entries {
                        let seq = match entry {
                            PyRep::Tuple(v) | PyRep::List(v) => v,
                            _ => {
                                return Err(BridgeRpcError {
                                    code: "InvalidFittingItem".into(),
                                    message:
                                        "Each fitting item must be a (typeID, flagID, quantity) tuple"
                                            .into(),
                                })
                            }
                        };
                        let Some(mod_type_id) =
                            seq.first().and_then(|v| v.as_i64()).filter(|&v| v > 0)
                        else {
                            return Err(BridgeRpcError {
                                code: "InvalidFittingItem".into(),
                                message: "Fitting item is missing a positive typeID".into(),
                            });
                        };
                        let Some(flag_raw) =
                            seq.get(1).and_then(|v| v.as_i64()).filter(|&v| v >= 0)
                        else {
                            return Err(BridgeRpcError {
                                code: "InvalidFittingItem".into(),
                                message: "Fitting item is missing a valid flagID".into(),
                            });
                        };
                        let qty_i64 = seq.get(2).and_then(|v| v.as_i64()).unwrap_or(1);
                        let Ok(qty) = i32::try_from(qty_i64) else {
                            return Err(BridgeRpcError {
                                code: "InvalidFittingItem".into(),
                                message: format!(
                                    "Fitting item quantity overflows i32 bounds, got {qty_i64}"
                                ),
                            });
                        };
                        if qty <= 0 {
                            return Err(BridgeRpcError {
                                code: "InvalidFittingItem".into(),
                                message: format!(
                                    "Fitting item quantity must be positive, got {qty}"
                                ),
                            });
                        }
                        let flag_u16 = u16::try_from(flag_raw).ok();
                        let flag =
                            flag_u16.map(InventoryFlag::from_u16).unwrap_or(InventoryFlag::None);
                        let valid_flag = flag_u16
                            .map(|f| {
                                matches!(f, 5 | 11..=34 | 87 | 92..=94 | 125..=132 | 158)
                                    && (flag as u16) == f
                            })
                            .unwrap_or(false);
                        if !valid_flag {
                            return Err(BridgeRpcError {
                                code: "InvalidFittingFlag".into(),
                                message: format!("Invalid fitting slot or bay flag {flag_raw}"),
                            });
                        }
                        out.push((mod_type_id as TypeId, flag, qty));
                    }
                    Ok(out)
                } else {
                    let char_items = ctx.db.get_items_for_character(char_id);
                    let active_ship_matches = session
                        .ship_id
                        .and_then(|sid| char_items.iter().find(|i| i.item_id == sid))
                        .map(|s| s.type_id == ship_type_id)
                        .unwrap_or(false);
                    if active_ship_matches {
                        let sid = session.ship_id.unwrap_or(0);
                        Ok(char_items
                            .into_iter()
                            .filter(|i| {
                                let is_mod_or_drone = ctx
                                    .sde
                                    .types
                                    .get(&i.type_id)
                                    .map(|t| {
                                        t.category_id == 7
                                            || t.category_id == 32
                                            || (i.flag as u16) == 87
                                    })
                                    .unwrap_or(false);
                                i.location_id == sid
                                    && is_mod_or_drone
                                    && matches!(i.flag as u16, 11..=34 | 87 | 92..=94 | 125..=132)
                            })
                            .map(|i| (i.type_id, i.flag, i.quantity))
                            .collect())
                    } else {
                        Ok(Vec::new())
                    }
                }
            };

            let validate_and_build_fit =
                |name: String,
                 raw_ship_type: i64,
                 description: String,
                 items_arg: Option<&PyRep>|
                 -> Result<
                    (String, TypeId, String, Vec<(TypeId, InventoryFlag, i32)>),
                    BridgeRpcError,
                > {
                    let ship_type_id = raw_ship_type as TypeId;
                    let is_valid_hull = ctx
                        .sde
                        .types
                        .get(&ship_type_id)
                        .map(|t| t.category_id == 6)
                        .unwrap_or(false);
                    if !is_valid_hull {
                        return Err(BridgeRpcError {
                            code: "InvalidShipType".into(),
                            message: format!("Type {ship_type_id} is not a valid ship hull"),
                        });
                    }
                    let parsed_items = parse_fit_items(items_arg, ship_type_id)?;
                    let all_skills_lvl5: HashMap<TypeId, u8> = ctx
                        .sde
                        .types
                        .values()
                        .filter(|t| t.category_id == 16)
                        .map(|t| (t.type_id, 5))
                        .collect();
                    let mut validator =
                        eve_dogma::ShipFittingEngine::from_sde(0, ship_type_id, &ctx.sde)
                            .with_character_skills(all_skills_lvl5);
                    for (idx, &(mod_type_id, flag, qty)) in parsed_items.iter().enumerate() {
                        let flag_u16 = flag as u16;
                        if matches!(flag_u16, 11..=34 | 92..=94 | 125..=132) {
                            if qty != 1 {
                                return Err(BridgeRpcError {
                                    code: "InvalidFittingQuantity".into(),
                                    message: format!(
                                        "Fitting slot {flag_u16} requires quantity 1, got {qty}"
                                    ),
                                });
                            }
                            if let Err(e) = validator.fit_and_online_module(
                                (idx as i64) + 1,
                                mod_type_id,
                                flag_u16,
                                &ctx.sde,
                            ) {
                                return Err(BridgeRpcError {
                                    code: "InvalidFittingModule".into(),
                                    message: e,
                                });
                            }
                        } else if !ctx.sde.types.contains_key(&mod_type_id) {
                            return Err(BridgeRpcError {
                                code: "UnknownItemType".into(),
                                message: format!(
                                    "Unknown item type {mod_type_id} in saved fitting"
                                ),
                            });
                        }
                    }
                    Ok((name, ship_type_id, description, parsed_items))
                };

            let extract_fit_from_rep = |rep: &PyRep| -> Result<
                (String, TypeId, String, Vec<(TypeId, InventoryFlag, i32)>),
                BridgeRpcError,
            > {
                let name = rep
                    .dict_get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Saved Fitting")
                    .to_string();
                let Some(raw_ship_type) = rep.dict_get("shipTypeID").and_then(|v| v.as_i64())
                else {
                    return Err(BridgeRpcError {
                        code: "InvalidShipType".into(),
                        message: "Fitting object requires shipTypeID".into(),
                    });
                };
                let description = rep
                    .dict_get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let items_rep = rep.dict_get("fitData").or_else(|| rep.dict_get("items"));
                validate_and_build_fit(name, raw_ship_type, description, items_rep)
            };

            let mut owner_id = default_owner;
            let mut fits_to_save: Vec<(String, TypeId, String, Vec<(TypeId, InventoryFlag, i32)>)> =
                Vec::new();

            if method == "SaveManyFittings" {
                let fittings_arg = if let Some(oid) = args.first().and_then(|a| a.as_i64()) {
                    if oid > 0 {
                        owner_id = oid;
                    }
                    args.get(1)
                } else {
                    args.first()
                };
                let Some(f_arg) = fittings_arg else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidArgument".into(),
                        message: "SaveManyFittings requires fittings collection".into(),
                    }));
                };
                match f_arg {
                    PyRep::Dict(entries) => {
                        for (_, fit_rep) in entries {
                            match extract_fit_from_rep(fit_rep) {
                                Ok(f) => fits_to_save.push(f),
                                Err(e) => return Some(Err(e)),
                            }
                        }
                    }
                    PyRep::List(list) | PyRep::Tuple(list) => {
                        for fit_rep in list {
                            match extract_fit_from_rep(fit_rep) {
                                Ok(f) => fits_to_save.push(f),
                                Err(e) => return Some(Err(e)),
                            }
                        }
                    }
                    _ => {
                        return Some(Err(BridgeRpcError {
                            code: "InvalidArgument".into(),
                            message:
                                "SaveManyFittings requires a Dict or List of fitting objects"
                                    .into(),
                        }));
                    }
                }
            } else if let Some(oid) = args.first().and_then(|a| a.as_i64()) {
                // V24 client signature: SaveFitting(ownerID, fitting)
                if oid > 0 {
                    owner_id = oid;
                }
                let Some(fit_rep) = args.get(1) else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidArgument".into(),
                        message: "SaveFitting(ownerID, fitting) requires fitting object".into(),
                    }));
                };
                match extract_fit_from_rep(fit_rep) {
                    Ok(f) => fits_to_save.push(f),
                    Err(e) => return Some(Err(e)),
                }
            } else if let Some(fit_rep) = args
                .first()
                .filter(|a| a.dict_get("shipTypeID").is_some())
            {
                match extract_fit_from_rep(fit_rep) {
                    Ok(f) => fits_to_save.push(f),
                    Err(e) => return Some(Err(e)),
                }
            } else {
                // Legacy positional signature: SaveFitting(name, shipTypeID, description?, items?)
                let name = args
                    .first()
                    .and_then(|a| a.as_str())
                    .unwrap_or("Rifter Combat Fit")
                    .to_string();
                let Some(raw_ship_type) = args.get(1).and_then(|a| a.as_i64()) else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidShipType".into(),
                        message: "SaveFitting requires shipTypeID".into(),
                    }));
                };
                let (description, items_arg) = match args.get(2) {
                    Some(PyRep::String(s)) => (s.clone(), args.get(3)),
                    Some(list_or_tuple @ (PyRep::List(_) | PyRep::Tuple(_))) => {
                        (format!("Saved via {service}"), Some(list_or_tuple))
                    }
                    _ => (format!("Saved via {service}"), args.get(3)),
                };
                match validate_and_build_fit(name, raw_ship_type, description, items_arg) {
                    Ok(f) => fits_to_save.push(f),
                    Err(e) => return Some(Err(e)),
                }
            }

            let expected_owner = match service {
                "corpFittingMgr" => session.corporation_id.unwrap_or(0),
                "allianceFittingMgr" => session.alliance_id.unwrap_or(0),
                _ => char_id,
            };
            if owner_id != expected_owner {
                return Some(Err(BridgeRpcError { code: "UserError".into(), message: "FittingOwnerMismatch".into() }));
            }

            let is_corp_or_alliance = service == "corpFittingMgr"
                || service == "allianceFittingMgr"
                || owner_id != char_id;
            if service == "corpFittingMgr"
                && ctx.db.with_snapshot(|s| s.corporations.contains_key(&owner_id))
            {
                let (is_ceo, roles) = get_effective_corp_roles(ctx, owner_id, char_id);
                let has_role = is_ceo || eve_db::corp_roles::has_role(roles, eve_db::corp_roles::ROLE_DIRECTOR | (1u64 << 41));
                if !has_role {
                    return Some(Err(BridgeRpcError {
                        code: "CorpRoleDenied".into(),
                        message: "Saving corporation fittings requires Fitting Manager or Director role".into(),
                    }));
                }
            }
            let max_fittings: usize = if is_corp_or_alliance { 300 } else { 250 };

            let save_res = ctx.db.try_with_mut_snapshot(|snap| {
                let current_count = snap
                    .saved_fittings
                    .values()
                    .filter(|f| f.owner_id == owner_id)
                    .count();
                if current_count + fits_to_save.len() > max_fittings {
                    return Err(BridgeRpcError {
                        code: "OwnerMaxFittings".into(),
                        message: format!(
                            "Owner {owner_id} has {current_count} fittings; saving {} more would exceed maximum of {max_fittings}",
                            fits_to_save.len()
                        ),
                    });
                }
                let mut saved_ids = Vec::with_capacity(fits_to_save.len());
                for (name, ship_type_id, description, parsed_items) in fits_to_save {
                    snap.next_entity_id += 1;
                    let id = snap.next_entity_id;
                    snap.saved_fittings.insert(
                        id,
                        SavedFittingRecord {
                            fitting_id: id,
                            owner_id,
                            name,
                            ship_type_id,
                            description,
                            items: parsed_items,
                        },
                    );
                    saved_ids.push(id);
                }
                Ok(saved_ids)
            });

            match save_res {
                Ok(saved_ids) => {
                    ctx.notify(
                        crate::router::NotificationTarget::Character(char_id),
                        "OnFittingsUpdated",
                        "charid",
                        vec![PyRep::Integer(owner_id)],
                    );
                    if method == "SaveManyFittings" {
                        let list: Vec<PyRep> = saved_ids.iter().copied().map(PyRep::Integer).collect();
                        ctx.notify(
                            crate::router::NotificationTarget::Character(char_id),
                            "OnFittingAdded",
                            "charid",
                            vec![PyRep::Integer(owner_id), PyRep::List(list.clone())],
                        );
                        Some(Ok(PyRep::List(list)))
                    } else {
                        let fid = saved_ids.first().copied().unwrap_or(0);
                        ctx.notify(
                            crate::router::NotificationTarget::Character(char_id),
                            "OnFittingAdded",
                            "charid",
                            vec![PyRep::Integer(owner_id), PyRep::Integer(fid)],
                        );
                        Some(Ok(PyRep::Integer(fid)))
                    }
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("charFittingMgr", "GetFittings")
        | ("corpFittingMgr", "GetFittings")
        | ("allianceFittingMgr", "GetFittings")
        | ("corpFittingMgr", "GetCommunityFittings") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let owner_id = if method == "GetCommunityFittings" {
                1i64
            } else {
                let expected_owner = match service {
                    "corpFittingMgr" => session.corporation_id.unwrap_or(0),
                    "allianceFittingMgr" => session.alliance_id.unwrap_or(0),
                    _ => char_id,
                };
                let default_owner = if expected_owner > 0 { expected_owner } else { char_id };
                let owner_id = args
                    .first()
                    .and_then(|a| a.as_i64())
                    .filter(|&oid| oid > 0)
                    .unwrap_or(default_owner);

                if (!args.is_empty() || method == "GetFittings") && owner_id != expected_owner {
                    return Some(Err(BridgeRpcError { code: "UserError".into(), message: "FittingOwnerMismatch".into() }));
                }
                owner_id
            };
            let rows: Vec<Vec<PyRep>> = ctx.db.with_snapshot(|snap| {
                let mut fittings: Vec<_> = snap
                    .saved_fittings
                    .values()
                    .filter(|f| f.owner_id == owner_id)
                    .collect();
                fittings.sort_by_key(|f| f.fitting_id);
                fittings
                    .into_iter()
                    .map(|f| {
                        let item_tuples: Vec<PyRep> = f
                            .items
                            .iter()
                            .map(|&(tid, flg, qty)| {
                                PyRep::Tuple(vec![
                                    PyRep::Integer(tid as i64),
                                    PyRep::Integer(flg as i64),
                                    PyRep::Integer(qty as i64),
                                ])
                            })
                            .collect();
                        vec![
                            PyRep::Integer(f.fitting_id),
                            PyRep::String(f.name.clone()),
                            PyRep::Integer(f.ship_type_id as i64),
                            PyRep::String(f.description.clone()),
                            PyRep::List(item_tuples),
                        ]
                    })
                    .collect()
            });
            Some(Ok(PyRep::rowset(
                vec!["fittingID", "name", "shipTypeID", "description", "items"],
                rows,
            )))
        }

        ("charFittingMgr", "UpdateNameAndDescription") | ("corpFittingMgr", "UpdateNameAndDescription") | ("allianceFittingMgr", "UpdateNameAndDescription") | ("charFittingMgr", "UpdateFitting") | ("corpFittingMgr", "UpdateFitting") | ("allianceFittingMgr", "UpdateFitting") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            // Supports (fittingID, ownerID, name, description) or (fittingID, name, description)
            let fid = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let (owner_filter, new_name, new_desc) = if args.len() >= 4 {
                (
                    args.get(1).and_then(|a| a.as_i64()),
                    args.get(2).and_then(|a| a.as_str()).unwrap_or("").to_string(),
                    args.get(3).and_then(|a| a.as_str()).unwrap_or("").to_string(),
                )
            } else {
                (
                    None,
                    args.get(1).and_then(|a| a.as_str()).unwrap_or("").to_string(),
                    args.get(2).and_then(|a| a.as_str()).unwrap_or("").to_string(),
                )
            };
            let target_owner = match resolve_and_authorize_fitting_owner(
                ctx,
                session,
                service,
                char_id,
                owner_filter,
                "Updating",
            ) {
                Ok(oid) => oid,
                Err(e) => return Some(Err(e)),
            };

            if new_name.trim().is_empty() {
                return Some(Err(BridgeRpcError {
                    code: "InvalidFittingName".into(),
                    message: "Fitting name cannot be empty".into(),
                }));
            }
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(fit) = snap.saved_fittings.get_mut(&fid) else {
                    return Err(BridgeRpcError {
                        code: "FittingNotFound".into(),
                        message: format!("Saved fitting {fid} not found"),
                    });
                };
                if fit.owner_id != target_owner {
                    return Err(BridgeRpcError {
                        code: "FittingOwnershipViolation".into(),
                        message: format!("Not authorized to update fitting {fid}"),
                    });
                }
                fit.name = new_name.clone();
                fit.description = new_desc.clone();
                Ok(fit.owner_id)
            });
            match res {
                Ok(owner_id) => {
                    ctx.notify(
                        crate::router::NotificationTarget::Character(char_id),
                        "OnFittingsUpdated",
                        "charid",
                        vec![PyRep::Integer(owner_id)],
                    );
                    Some(Ok(PyRep::keyval([
                        ("fittingID", PyRep::Integer(fid)),
                        ("ownerID", PyRep::Integer(owner_id)),
                        ("name", PyRep::String(new_name)),
                        ("description", PyRep::String(new_desc)),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("charFittingMgr", "DeleteFitting") | ("corpFittingMgr", "DeleteFitting") | ("allianceFittingMgr", "DeleteFitting") | ("charFittingMgr", "DeleteManyFittings") | ("corpFittingMgr", "DeleteManyFittings") | ("allianceFittingMgr", "DeleteManyFittings") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };

            if method == "DeleteManyFittings" {
                let (owner_filter, list_arg) = match (args.first(), args.get(1)) {
                    (Some(PyRep::Integer(oid)), Some(list @ (PyRep::List(_) | PyRep::Tuple(_)))) => {
                        (Some(*oid), Some(list))
                    }
                    (Some(list @ (PyRep::List(_) | PyRep::Tuple(_))), _) => (None, Some(list)),
                    _ => (None, None),
                };
                let target_owner = match resolve_and_authorize_fitting_owner(
                    ctx,
                    session,
                    service,
                    char_id,
                    owner_filter,
                    "Deleting",
                ) {
                    Ok(oid) => oid,
                    Err(e) => return Some(Err(e)),
                };
                let target_ids: Vec<i64> = match list_arg {
                    Some(PyRep::List(v) | PyRep::Tuple(v)) => {
                        v.iter().filter_map(|x| x.as_i64()).collect()
                    }
                    _ => Vec::new(),
                };
                let deleted_ids = ctx
                    .db
                    .try_with_mut_snapshot(|snap| {
                        let mut removed = Vec::new();
                        for fid in target_ids {
                            let can_delete = snap.saved_fittings.get(&fid).is_some_and(|f| {
                                f.owner_id == target_owner
                            });
                            if can_delete {
                                snap.saved_fittings.remove(&fid);
                                removed.push(fid);
                            }
                        }
                        Ok::<_, std::convert::Infallible>(removed)
                    })
                    .unwrap();
                ctx.notify(
                    crate::router::NotificationTarget::Character(char_id),
                    "OnFittingsUpdated",
                    "charid",
                    vec![PyRep::Integer(target_owner)],
                );
                let deleted_list: Vec<PyRep> = deleted_ids.iter().copied().map(PyRep::Integer).collect();
                if !deleted_list.is_empty() {
                    ctx.notify(
                        crate::router::NotificationTarget::Character(char_id),
                        "OnManyFittingsDeleted",
                        "charid",
                        vec![PyRep::Integer(target_owner), PyRep::List(deleted_list.clone())],
                    );
                }
                Some(Ok(PyRep::List(deleted_list)))
            } else {
                // Supports DeleteFitting(ownerID, fittingID) and DeleteFitting(fittingID)
                let (owner_filter, fid) = if args.len() >= 2 {
                    (
                        args.first().and_then(|a| a.as_i64()),
                        args.get(1).and_then(|a| a.as_i64()).unwrap_or(0),
                    )
                } else {
                    (None, args.first().and_then(|a| a.as_i64()).unwrap_or(0))
                };
                let target_owner = match resolve_and_authorize_fitting_owner(
                    ctx,
                    session,
                    service,
                    char_id,
                    owner_filter,
                    "Deleting",
                ) {
                    Ok(oid) => oid,
                    Err(e) => return Some(Err(e)),
                };
                let deleted = ctx
                    .db
                    .try_with_mut_snapshot(|snap| {
                        let can_delete = snap.saved_fittings.get(&fid).is_some_and(|f| {
                            f.owner_id == target_owner
                        });
                        if can_delete {
                            snap.saved_fittings.remove(&fid);
                            Ok::<_, std::convert::Infallible>(true)
                        } else {
                            Ok::<_, std::convert::Infallible>(false)
                        }
                    })
                    .unwrap();
                if deleted {
                    ctx.notify(
                        crate::router::NotificationTarget::Character(char_id),
                        "OnFittingsUpdated",
                        "charid",
                        vec![PyRep::Integer(target_owner)],
                    );
                    ctx.notify(
                        crate::router::NotificationTarget::Character(char_id),
                        "OnFittingDeleted",
                        "charid",
                        vec![PyRep::Integer(target_owner), PyRep::Integer(fid)],
                    );
                    if session.corporation_id == Some(target_owner) {
                        crate::services::notify_emitters_batch3::emit_community_fittings_updated(ctx);
                    }
                }
                Some(Ok(PyRep::Bool(deleted)))
            }
        }

        _ => None,
    }
}
