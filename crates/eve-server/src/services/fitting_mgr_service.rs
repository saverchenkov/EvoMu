//! `fitting_mgr_service` — `charFittingMgr`, `corpFittingMgr`, and `allianceFittingMgr` RPC handlers.

#![forbid(unsafe_code)]

use super::*;

/// V24 `appConst.COMMUNITY_FITTING_CORP = 1000282` (`Community Fittings` NPC corporation),
/// plus legacy curated community owner ID `1`.
const COMMUNITY_FITTING_CORP_ID: i64 = 1_000_282;
/// V24 `eve/common/lib/appConst.pyj::maxCharFittings`.
const MAX_CHAR_FITTINGS: usize = 500;
/// V24 `eve/common/lib/appConst.pyj::maxCorpFittings` and `maxAllianceFittings`.
const MAX_CORP_OR_ALLIANCE_FITTINGS: usize = 600;

pub(crate) fn is_community_fitting_owner(owner_id: i64) -> bool {
    owner_id == 1 || owner_id == COMMUNITY_FITTING_CORP_ID
}

fn can_manage_community_fittings(session: &SessionState) -> bool {
    session.has_gm_role() || (session.role & eve_machonet::service_roles::ROLE_CONTENT) != 0
}

fn community_role_denied(action_label: &str) -> BridgeRpcError {
    BridgeRpcError {
        code: "AccessDenied".into(),
        message: format!("{action_label} community fittings requires GM or Content role"),
        client_error: None,
    }
}

fn has_corp_fitting_manager_role(is_ceo: bool, roles: u64) -> bool {
    // `ROLE_FITTING_MANAGER` (1 << 59) or `ROLE_DIRECTOR` (checked by `has_role`).
    is_ceo || eve_db::corp_roles::has_role(roles, eve_db::corp_roles::ROLE_FITTING_MANAGER)
}

fn deny_corp_fitting_role(message: impl Into<String>) -> BridgeRpcError {
    crate::services::account_services::err("CorpRoleDenied", message)
}

fn is_npc_corporation_id(ctx: &ServerContext, corp_id: i64) -> bool {
    corp_id <= 0
        || (1_000_000..2_000_000).contains(&corp_id)
        || ctx.sde.npc_corporations.contains_key(&corp_id)
}

fn authorize_player_corp_fitting_member(
    ctx: &ServerContext,
    corp_id: i64,
    char_id: i64,
    action_label: &str,
    scope_noun: &str,
    expected_alliance_id: Option<i64>,
) -> Result<(), BridgeRpcError> {
    if is_npc_corporation_id(ctx, corp_id) {
        return Err(deny_corp_fitting_role(format!(
            "{action_label} {scope_noun} fittings is not permitted for NPC corporations"
        )));
    }
    let excluded_member = ctx.db.with_snapshot(|s| -> Result<bool, BridgeRpcError> {
        let Some(corp) = s.corporations.get(&corp_id) else {
            return Err(deny_corp_fitting_role(format!(
                "{action_label} {scope_noun} fittings requires a registered player corporation"
            )));
        };
        if let Some(alliance_id) = expected_alliance_id {
            let Some(alliance) = s.alliances.get(&alliance_id) else {
                return Err(crate::services::account_services::err(
                    "UserError",
                    "NotInAlliance",
                ));
            };
            if !alliance.member_corp_ids.contains(&corp_id) && corp.alliance_id != Some(alliance_id) {
                return Err(crate::services::account_services::err(
                    "UserError",
                    "FittingOwnerMismatch",
                ));
            }
        }
        Ok(is_excluded_corp_member(corp, char_id))
    })?;
    if excluded_member {
        return Err(deny_corp_fitting_role(format!(
            "{action_label} {scope_noun} fittings requires membership in the corporation"
        )));
    }
    let (is_ceo, roles) = get_effective_corp_roles(ctx, corp_id, char_id);
    if !has_corp_fitting_manager_role(is_ceo, roles) {
        return Err(deny_corp_fitting_role(format!(
            "{action_label} {scope_noun} fittings requires Fitting Manager or Director role"
        )));
    }
    Ok(())
}

fn is_excluded_corp_member(corp: &eve_db::CorporationRecord, char_id: i64) -> bool {
    corp.ceo_id != char_id && !corp.members.is_empty() && !corp.members.contains(&char_id)
}

fn resolve_caller_corp_and_alliance(
    snap: &eve_db::WorldStateSnapshot,
    session: &SessionState,
    char_id: i64,
) -> (i64, i64) {
    let caller_corp = snap
        .characters
        .get(&char_id)
        .map(|c| c.corporation_id)
        .or(session.corporation_id)
        .unwrap_or(0);
    let corp_opt = snap.corporations.get(&caller_corp);
    let caller_alliance = corp_opt
        .and_then(|c| c.alliance_id)
        .or_else(|| {
            if corp_opt.is_none() {
                snap.characters.get(&char_id).and_then(|c| c.alliance_id)
            } else {
                None
            }
        })
        .or_else(|| {
            snap.alliances
                .values()
                .find(|a| a.member_corp_ids.contains(&caller_corp))
                .map(|a| a.alliance_id)
        })
        .or_else(|| {
            if corp_opt.is_none() && !snap.characters.contains_key(&char_id) {
                session.alliance_id
            } else {
                None
            }
        })
        .unwrap_or(0);
    (caller_corp, caller_alliance)
}

fn resolve_and_authorize_fitting_owner(
    ctx: &ServerContext,
    session: &SessionState,
    service: &str,
    char_id: i64,
    owner_filter: Option<i64>,
    action_label: &str,
) -> Result<i64, BridgeRpcError> {
    let (caller_corp, caller_alliance) = ctx
        .db
        .with_snapshot(|snap| resolve_caller_corp_and_alliance(snap, session, char_id));
    let expected_owner = if service.starts_with("corp") {
        caller_corp
    } else if service.starts_with("alliance") {
        caller_alliance
    } else {
        char_id
    };
    let target_owner = owner_filter.filter(|&oid| oid > 0).unwrap_or(expected_owner);
    if service == "corpFittingMgr" && is_community_fitting_owner(target_owner) {
        if !can_manage_community_fittings(session) {
            return Err(community_role_denied(action_label));
        }
        return Ok(target_owner);
    }
    if expected_owner <= 0 {
        return Err(crate::services::account_services::err(
            "UserError",
            if service.starts_with("alliance") {
                "NotInAlliance"
            } else {
                "NotInCorporation"
            },
        ));
    }
    if target_owner != expected_owner {
        return Err(BridgeRpcError {
            code: "UserError".into(),
            message: "FittingOwnerMismatch".into(),
            client_error: None,
        });
    }
    if service.starts_with("corp") {
        authorize_player_corp_fitting_member(ctx, target_owner, char_id, action_label, "corporation", None)?;
    }
    if service.starts_with("alliance") {
        authorize_player_corp_fitting_member(
            ctx,
            caller_corp,
            char_id,
            action_label,
            "alliance",
            Some(target_owner),
        )?;
    }
    Ok(target_owner)
}

use crate::router::rpc_registry::{rpc_entry, RpcBoxFuture, RpcDispatchHandler, RpcEntry};

fn dispatch_rpc<'a>(
    ctx: &'a ServerContext,
    session: &'a mut SessionState,
    bound_entry: Option<&'a BoundObjectEntry>,
    service: &'a str,
    method: &'a str,
    args: &'a [PyRep],
    kwargs: &'a HashMap<String, PyRep>,
) -> RpcBoxFuture<'a> {
    Box::pin(handle_fitting_mgr(ctx, session, bound_entry, service, method, args, kwargs))
}

pub(crate) const RPCS: &[RpcEntry] = &[
    rpc_entry("charFittingMgr", "SaveFitting", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("charFittingMgr", "SaveManyFittings", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("corpFittingMgr", "SaveFitting", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("corpFittingMgr", "SaveManyFittings", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("allianceFittingMgr", "SaveFitting", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("allianceFittingMgr", "SaveManyFittings", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("charFittingMgr", "GetFittings", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("corpFittingMgr", "GetFittings", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("allianceFittingMgr", "GetFittings", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("corpFittingMgr", "GetCommunityFittings", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("charFittingMgr", "UpdateNameAndDescription", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("charFittingMgr", "UpdateFitting", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("corpFittingMgr", "UpdateNameAndDescription", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("corpFittingMgr", "UpdateFitting", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("allianceFittingMgr", "UpdateNameAndDescription", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("allianceFittingMgr", "UpdateFitting", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("charFittingMgr", "DeleteFitting", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("charFittingMgr", "DeleteManyFittings", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("corpFittingMgr", "DeleteFitting", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("corpFittingMgr", "DeleteManyFittings", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("allianceFittingMgr", "DeleteFitting", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("allianceFittingMgr", "DeleteManyFittings", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
];

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
        ("charFittingMgr", "SaveFitting") | ("charFittingMgr", "SaveManyFittings") => {
            handle_save_fitting(ctx, session, service, method, args).await
        }
        ("corpFittingMgr", "SaveFitting") | ("corpFittingMgr", "SaveManyFittings") => {
            handle_save_fitting(ctx, session, service, method, args).await
        }
        ("allianceFittingMgr", "SaveFitting") | ("allianceFittingMgr", "SaveManyFittings") => {
            handle_save_fitting(ctx, session, service, method, args).await
        }

        // parity: rpc:charFittingMgr.GetFittings
        ("charFittingMgr", "GetFittings") => handle_get_fittings(ctx, session, service, method, args),
        // parity: rpc:corpFittingMgr.GetFittings
        ("corpFittingMgr", "GetFittings") => handle_get_fittings(ctx, session, service, method, args),
        // parity: rpc:allianceFittingMgr.GetFittings
        ("allianceFittingMgr", "GetFittings") => handle_get_fittings(ctx, session, service, method, args),
        // parity: rpc:corpFittingMgr.GetCommunityFittings
        ("corpFittingMgr", "GetCommunityFittings") => handle_get_fittings(ctx, session, service, method, args),


        ("charFittingMgr", "UpdateNameAndDescription") | ("charFittingMgr", "UpdateFitting") => {
            handle_update_fitting(ctx, session, service, args).await
        }
        ("corpFittingMgr", "UpdateNameAndDescription") | ("corpFittingMgr", "UpdateFitting") => {
            handle_update_fitting(ctx, session, service, args).await
        }
        ("allianceFittingMgr", "UpdateNameAndDescription") | ("allianceFittingMgr", "UpdateFitting") => {
            handle_update_fitting(ctx, session, service, args).await
        }

        ("charFittingMgr", "DeleteFitting") | ("charFittingMgr", "DeleteManyFittings") => {
            handle_delete_fitting(ctx, session, service, method, args).await
        }
        ("corpFittingMgr", "DeleteFitting") | ("corpFittingMgr", "DeleteManyFittings") => {
            handle_delete_fitting(ctx, session, service, method, args).await
        }
        ("allianceFittingMgr", "DeleteFitting") | ("allianceFittingMgr", "DeleteManyFittings") => {
            handle_delete_fitting(ctx, session, service, method, args).await
        }

        _ => None,
    }
}

fn handle_get_fittings(
    ctx: &ServerContext,
    session: &SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (_, char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let is_community_query = method == "GetCommunityFittings";
    let owner_id = if is_community_query {
        COMMUNITY_FITTING_CORP_ID
    } else {
        let (caller_corp, caller_alliance, corp_exists, alliance_exists, excluded_member) =
            ctx.db.with_snapshot(|snap| {
                let (corp_id, ally_id) = resolve_caller_corp_and_alliance(snap, session, char_id);
                let corp_opt = snap.corporations.get(&corp_id);
                let excluded = corp_opt.is_some_and(|corp| is_excluded_corp_member(corp, char_id));
                let ally_ok = ally_id > 0 && snap.alliances.contains_key(&ally_id);
                (corp_id, ally_id, corp_opt.is_some(), ally_ok, excluded)
            });
        let expected_owner = match service {
            "corpFittingMgr" => caller_corp,
            "allianceFittingMgr" => caller_alliance,
            _ => char_id,
        };
        let default_owner = if expected_owner > 0 { expected_owner } else { char_id };
        let owner_id = args
            .first()
            .and_then(|a| a.as_i64())
            .filter(|&oid| oid > 0)
            .unwrap_or(default_owner);

        if (!args.is_empty() || method == "GetFittings") && owner_id != expected_owner {
            return Some(Err(BridgeRpcError { code: "UserError".into(), message: "FittingOwnerMismatch".into(), client_error: None }));
        }
        // In V24.02, opening Fitting Management (`fittingSvc.GetAllFittingsForCharacter` / `GetAllFittings`)
        // calls `corpFittingMgr.GetFittings(session.corpid)` alongside `charFittingMgr.GetFittings` and
        // `corpFittingMgr.GetCommunityFittings`, including for NPC-corp pilots whose corporation has no
        // player corporation row. Return `{}` cleanly for the caller's own NPC corporation without
        // querying `saved_fittings`, while keeping all mutations and non-NPC/alliance paths fail-closed.
        if service == "corpFittingMgr"
            && method == "GetFittings"
            && caller_corp > 0
            && is_npc_corporation_id(ctx, caller_corp)
            && owner_id == caller_corp
            && !excluded_member
        {
            return Some(Ok(PyRep::Dict(Vec::new())));
        }
        let is_shared_fitting_service = matches!(service, "corpFittingMgr" | "allianceFittingMgr");
        if is_shared_fitting_service
            && (is_npc_corporation_id(ctx, caller_corp) || !corp_exists || excluded_member)
        {
            return Some(Err(deny_corp_fitting_role(
                "Viewing corporation or alliance fittings requires player corporation membership",
            )));
        }
        if service == "allianceFittingMgr" && !alliance_exists {
            return Some(Err(crate::services::account_services::err(
                "UserError",
                "NotInAlliance",
            )));
        }
        owner_id
    };
    let entries: Vec<(PyRep, PyRep)> = ctx.db.with_snapshot(|snap| {
        let mut fittings: Vec<_> = snap
            .saved_fittings
            .values()
            .filter(|f| {
                if is_community_query {
                    is_community_fitting_owner(f.owner_id)
                } else {
                    f.owner_id == owner_id
                }
            })
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
                (
                    PyRep::Integer(f.fitting_id),
                    PyRep::keyval([
                        ("fittingID", PyRep::Integer(f.fitting_id)),
                        ("ownerID", PyRep::Integer(f.owner_id)),
                        ("shipTypeID", PyRep::Integer(f.ship_type_id as i64)),
                        ("name", PyRep::String(f.name.clone())),
                        ("description", PyRep::String(f.description.clone())),
                        ("fitData", PyRep::List(item_tuples.clone())),
                        ("items", PyRep::List(item_tuples)),
                    ]),
                )
            })
            .collect()
    });
    Some(Ok(PyRep::Dict(entries)))
}

async fn handle_save_fitting(
    ctx: &ServerContext,
    session: &SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (_, char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let (caller_corp, caller_alliance) = ctx
        .db
        .with_snapshot(|snap| resolve_caller_corp_and_alliance(snap, session, char_id));
    let default_owner = match service {
        "corpFittingMgr" => if caller_corp > 0 { caller_corp } else { char_id },
        "allianceFittingMgr" => if caller_alliance > 0 {
            caller_alliance
        } else if caller_corp > 0 {
            caller_corp
        } else {
            char_id
        },
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
                        client_error: None,
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
                            client_error: None,
                        })
                    }
                };
                let Some(mod_type_id) =
                    seq.first().and_then(|v| v.as_i64()).filter(|&v| v > 0)
                else {
                    return Err(BridgeRpcError {
                        code: "InvalidFittingItem".into(),
                        message: "Fitting item is missing a positive typeID".into(),
                        client_error: None,
                    });
                };
                let Some(flag_raw) =
                    seq.get(1).and_then(|v| v.as_i64()).filter(|&v| v >= 0)
                else {
                    return Err(BridgeRpcError {
                        code: "InvalidFittingItem".into(),
                        message: "Fitting item is missing a valid flagID".into(),
                        client_error: None,
                    });
                };
                let qty_i64 = seq.get(2).and_then(|v| v.as_i64()).unwrap_or(1);
                let Ok(qty) = i32::try_from(qty_i64) else {
                    return Err(BridgeRpcError {
                        code: "InvalidFittingItem".into(),
                        message: format!(
                            "Fitting item quantity overflows i32 bounds, got {qty_i64}"
                        ),
                        client_error: None,
                    });
                };
                if qty <= 0 {
                    return Err(BridgeRpcError {
                        code: "InvalidFittingItem".into(),
                        message: format!(
                            "Fitting item quantity must be positive, got {qty}"
                        ),
                        client_error: None,
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
                        client_error: None,
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
                    client_error: None,
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
                            client_error: None,
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
                            client_error: None,
                        });
                    }
                } else if !ctx.sde.types.contains_key(&mod_type_id) {
                    return Err(BridgeRpcError {
                        code: "UnknownItemType".into(),
                        message: format!(
                            "Unknown item type {mod_type_id} in saved fitting"
                        ),
                        client_error: None,
                    });
                }
            }
            Ok((name, ship_type_id, description, parsed_items))
        };

    const MAX_FITTING_NAME_CHARS: usize = 100;
    const MAX_FITTING_NAME_BYTES: usize = 400;
    const MAX_FITTING_DESC_CHARS: usize = 1_000;
    const MAX_FITTING_DESC_BYTES: usize = 4_000;

    let extract_fit_from_rep = |rep: &PyRep| -> Result<
        (String, TypeId, String, Vec<(TypeId, InventoryFlag, i32)>),
        BridgeRpcError,
    > {
        let name = match rep.dict_get("name") {
            Some(v) if !matches!(v, PyRep::None) => crate::router::check_str_rep_max(
                v,
                "name",
                MAX_FITTING_NAME_CHARS,
                MAX_FITTING_NAME_BYTES,
            )?
            .to_string(),
            _ => "Saved Fitting".to_string(),
        };
        let Some(raw_ship_type) = rep.dict_get("shipTypeID").and_then(|v| v.as_i64())
        else {
            return Err(BridgeRpcError {
                code: "InvalidShipType".into(),
                message: "Fitting object requires shipTypeID".into(),
                client_error: None,
            });
        };
        let description = match rep.dict_get("description") {
            Some(v) if !matches!(v, PyRep::None) => crate::router::check_str_rep_max(
                v,
                "description",
                MAX_FITTING_DESC_CHARS,
                MAX_FITTING_DESC_BYTES,
            )?
            .to_string(),
            _ => String::new(),
        };
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
                client_error: None,
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
                    client_error: None,
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
                client_error: None,
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
        let name = match args.first() {
            Some(v) if !matches!(v, PyRep::None) => crate::router::check_str_rep_max(
                v,
                "name",
                MAX_FITTING_NAME_CHARS,
                MAX_FITTING_NAME_BYTES,
            )
            .map(str::to_string),
            _ => Ok("Rifter Combat Fit".to_string()),
        };
        let name = match name {
            Ok(v) => v,
            Err(e) => return Some(Err(e)),
        };
        let Some(raw_ship_type) = args.get(1).and_then(|a| a.as_i64()) else {
            return Some(Err(BridgeRpcError {
                code: "InvalidShipType".into(),
                message: "SaveFitting requires shipTypeID".into(),
                client_error: None,
            }));
        };
        let (description, items_arg) = match args.get(2) {
            Some(desc_rep @ (PyRep::String(_) | PyRep::Token(_) | PyRep::Bytes(_))) => {
                match crate::router::check_str_rep_max(
                    desc_rep,
                    "description",
                    MAX_FITTING_DESC_CHARS,
                    MAX_FITTING_DESC_BYTES,
                ) {
                    Ok(s) => (s.to_string(), args.get(3)),
                    Err(e) => return Some(Err(e)),
                }
            }
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

    let owner_id = match resolve_and_authorize_fitting_owner(
        ctx,
        session,
        service,
        char_id,
        Some(owner_id),
        "Saving",
    ) {
        Ok(oid) => oid,
        Err(e) => return Some(Err(e)),
    };
    let is_corp_or_alliance = service == "corpFittingMgr"
        || service == "allianceFittingMgr"
        || owner_id != char_id;
    let max_fittings: usize = if is_corp_or_alliance {
        MAX_CORP_OR_ALLIANCE_FITTINGS
    } else {
        MAX_CHAR_FITTINGS
    };

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
                client_error: None,
            });
        }
        let mut saved_fits = Vec::with_capacity(fits_to_save.len());
        for (name, ship_type_id, description, parsed_items) in fits_to_save {
            snap.next_entity_id += 1;
            let id = snap.next_entity_id;
            let rec = SavedFittingRecord {
                fitting_id: id,
                owner_id,
                name,
                ship_type_id,
                description,
                items: parsed_items,
            };
            saved_fits.push(crate::router::notify_registry::UpdatedFittingKeyVal::from_saved_fitting(&rec));
            snap.saved_fittings.insert(id, rec);
        }
        Ok(saved_fits)
    });

    match save_res {
        Ok(saved_fits) => {
            if is_community_fitting_owner(owner_id) {
                crate::services::notify_emitters_character_services::emit_community_fittings_updated(ctx);
            }
            for fit_kv in &saved_fits {
                ctx.emit_typed(
                    crate::router::NotificationTarget::Character(char_id),
                    crate::router::notify_registry::OnFittingsUpdated {
                        fitting: crate::router::notify_registry::UpdatedFittingPayload::Fitting(
                            fit_kv.clone(),
                        ),
                    },
                );
            }
            let saved_ids: Vec<i64> = saved_fits.iter().map(|f| f.fitting_id).collect();
            // Emit one OnFittingAdded(ownerID, fitID) per saved fitting with scalar fitID so V24
            // fittingSvc.OnFittingAdded takes its isinstance(fitID, (int, long)) branch and
            // invalidates self.fittings[ownerID] (V24's list branch checks `in` instead of `not in`).
            for &fid in &saved_ids {
                ctx.emit_typed(
                    crate::router::NotificationTarget::Character(char_id),
                    crate::router::notify_registry::OnFittingAdded {
                        owner_id,
                        fit_id: fid,
                    },
                );
            }
            if method == "SaveManyFittings" {
                let list: Vec<PyRep> = saved_ids.into_iter().map(PyRep::Integer).collect();
                Some(Ok(PyRep::List(list)))
            } else {
                let fid = saved_ids.first().copied().unwrap_or(0);
                Some(Ok(PyRep::Integer(fid)))
            }
        }
        Err(e) => Some(Err(e)),
    }
}

async fn handle_update_fitting(
    ctx: &ServerContext,
    session: &SessionState,
    service: &str,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (_, char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let rpc = crate::router::RpcArgs::positional(args);
    // Supports (fittingID, ownerID, name, description), (ownerID, fittingID, name, description), or (fittingID, name, description)
    let (fid, owner_filter, new_name, new_desc) = if args.len() >= 4 {
        let a0 = match rpc.req_positive_i64(0, "fittingOrOwnerID") {
            Ok(v) => v,
            Err(e) => return Some(Err(e)),
        };
        let a1 = match rpc.req_positive_i64(1, "ownerOrFittingID") {
            Ok(v) => v,
            Err(e) => return Some(Err(e)),
        };
        let (fid, oid) = ctx.db.with_snapshot(|s| {
            if s.saved_fittings.contains_key(&a0) {
                (a0, a1)
            } else if s.saved_fittings.contains_key(&a1)
                || is_community_fitting_owner(a0)
                || s.corporations.contains_key(&a0)
                || s.characters.contains_key(&a0)
                || s.alliances.contains_key(&a0)
            {
                (a1, a0)
            } else {
                (a0, a1)
            }
        });
        let name = match rpc.opt_str_max(2, "name", 100, 400) {
            Ok(v) => v.unwrap_or("").to_string(),
            Err(e) => return Some(Err(e)),
        };
        let desc = match rpc.opt_str_max(3, "description", 1_000, 4_000) {
            Ok(v) => v.unwrap_or("").to_string(),
            Err(e) => return Some(Err(e)),
        };
        (fid, Some(oid), name, desc)
    } else {
        let fid = match rpc.req_positive_i64(0, "fittingID") {
            Ok(v) => v,
            Err(e) => return Some(Err(e)),
        };
        let inferred_owner = if service == "corpFittingMgr" {
            ctx.db.with_snapshot(|s| {
                s.saved_fittings
                    .get(&fid)
                    .map(|f| f.owner_id)
                    .filter(|&oid| is_community_fitting_owner(oid))
            })
        } else {
            None
        };
        let name = match rpc.opt_str_max(1, "name", 100, 400) {
            Ok(v) => v.unwrap_or("").to_string(),
            Err(e) => return Some(Err(e)),
        };
        let desc = match rpc.opt_str_max(2, "description", 1_000, 4_000) {
            Ok(v) => v.unwrap_or("").to_string(),
            Err(e) => return Some(Err(e)),
        };
        (fid, inferred_owner, name, desc)
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
            client_error: None,
        }));
    }
    let res = ctx.db.try_with_mut_snapshot(|snap| {
        let Some(fit) = snap.saved_fittings.get_mut(&fid) else {
            return Err(BridgeRpcError {
                code: "FittingNotFound".into(),
                message: format!("Saved fitting {fid} not found"),
                client_error: None,
            });
        };
        if fit.owner_id != target_owner {
            return Err(BridgeRpcError {
                code: "FittingOwnershipViolation".into(),
                message: format!("Not authorized to update fitting {fid}"),
                client_error: None,
            });
        }
        fit.name = new_name.clone();
        fit.description = new_desc.clone();
        let fit_kv = crate::router::notify_registry::UpdatedFittingKeyVal::from_saved_fitting(fit);
        Ok((fit.owner_id, fit_kv))
    });
    match res {
        Ok((owner_id, fit_kv)) => {
            if is_community_fitting_owner(owner_id) {
                crate::services::notify_emitters_character_services::emit_community_fittings_updated(ctx);
            }
            ctx.emit_typed(
                crate::router::NotificationTarget::Character(char_id),
                crate::router::notify_registry::OnFittingsUpdated {
                    fitting: crate::router::notify_registry::UpdatedFittingPayload::Fitting(fit_kv),
                },
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

async fn handle_delete_fitting(
    ctx: &ServerContext,
    session: &SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (_, char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };

    if method == "DeleteManyFittings" {
        let (owner_filter, list_arg) = match (args.first(), args.get(1)) {
            (Some(PyRep::Integer(oid)), Some(list @ (PyRep::List(_) | PyRep::Tuple(_)))) => {
                (Some(*oid), Some(list))
            }
            (Some(PyRep::Integer(oid)), None) => (Some(*oid), None),
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
        let Some(PyRep::List(items) | PyRep::Tuple(items)) = list_arg else {
            return Some(Err(crate::services::account_services::err(
                "UserError",
                "InvalidFittingList",
            )));
        };
        let mut target_ids = Vec::with_capacity(items.len());
        for item in items {
            let Some(fid) = item.as_i64().filter(|&id| id > 0) else {
                return Some(Err(crate::services::account_services::err(
                    "UserError",
                    "InvalidFittingList",
                )));
            };
            target_ids.push(fid);
        }
        let deleted_ids = match ctx.db.try_with_mut_snapshot(|snap| {
            for &fid in &target_ids {
                if let Some(fit) = snap.saved_fittings.get(&fid) {
                    if fit.owner_id != target_owner {
                        return Err(crate::services::account_services::err(
                            "FittingOwnershipViolation",
                            format!("Not authorized to delete fitting {fid}"),
                        ));
                    }
                }
            }
            let mut removed = Vec::new();
            for fid in target_ids {
                if snap.saved_fittings.remove(&fid).is_some() {
                    removed.push(fid);
                }
            }
            Ok(removed)
        }) {
            Ok(ids) => ids,
            Err(e) => return Some(Err(e)),
        };
        ctx.emit_typed(
            crate::router::NotificationTarget::Character(char_id),
            crate::router::notify_registry::OnFittingsUpdated {
                fitting: crate::router::notify_registry::UpdatedFittingPayload::Cleared,
            },
        );
        let deleted_list: Vec<PyRep> = deleted_ids.iter().copied().map(PyRep::Integer).collect();
        if !deleted_list.is_empty() {
            if is_community_fitting_owner(target_owner) {
                crate::services::notify_emitters_character_services::emit_community_fittings_updated(ctx);
            }
            ctx.emit_typed(
                crate::router::NotificationTarget::Character(char_id),
                crate::router::notify_registry::OnManyFittingsDeleted {
                    owner_id: target_owner,
                    fit_ids: deleted_ids,
                },
            );
        }
        Some(Ok(PyRep::List(deleted_list)))
    } else {
        let rpc = crate::router::RpcArgs::positional(args);
        // Supports DeleteFitting(ownerID, fittingID), DeleteFitting(fittingID, ownerID), and DeleteFitting(fittingID)
        let (owner_filter, fid) = if args.len() >= 2 {
            let a0 = match rpc.req_positive_i64(0, "ownerOrFittingID") {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let a1 = match rpc.req_positive_i64(1, "fittingOrOwnerID") {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            ctx.db.with_snapshot(|s| {
                if s.saved_fittings.contains_key(&a0) && !s.saved_fittings.contains_key(&a1) {
                    (Some(a1), a0)
                } else {
                    (Some(a0), a1)
                }
            })
        } else {
            let fid = match rpc.req_positive_i64(0, "fittingID") {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let inferred = if service == "corpFittingMgr" {
                ctx.db.with_snapshot(|s| {
                    s.saved_fittings
                        .get(&fid)
                        .map(|f| f.owner_id)
                        .filter(|&oid| is_community_fitting_owner(oid))
                })
            } else {
                None
            };
            (inferred, fid)
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
        let deleted = match ctx.db.try_with_mut_snapshot(|snap| {
            let Some(fit) = snap.saved_fittings.get(&fid) else {
                if args.len() < 2
                    && service != "charFittingMgr"
                    && (fid == target_owner
                        || snap.corporations.contains_key(&fid)
                        || snap.alliances.contains_key(&fid))
                {
                    return Err(crate::services::account_services::err(
                        "UserError",
                        if fid != target_owner {
                            "FittingOwnerMismatch"
                        } else {
                            "InvalidFittingID"
                        },
                    ));
                }
                return Ok(false);
            };
            if fit.owner_id != target_owner {
                return Err(crate::services::account_services::err(
                    "FittingOwnershipViolation",
                    format!("Not authorized to delete fitting {fid}"),
                ));
            }
            snap.saved_fittings.remove(&fid);
            Ok(true)
        }) {
            Ok(v) => v,
            Err(e) => return Some(Err(e)),
        };
        if deleted {
            if is_community_fitting_owner(target_owner) {
                crate::services::notify_emitters_character_services::emit_community_fittings_updated(ctx);
            }
            ctx.emit_typed(
                crate::router::NotificationTarget::Character(char_id),
                crate::router::notify_registry::OnFittingsUpdated {
                    fitting: crate::router::notify_registry::UpdatedFittingPayload::Cleared,
                },
            );
            ctx.emit_typed(
                crate::router::NotificationTarget::Character(char_id),
                crate::router::notify_registry::OnFittingDeleted {
                    owner_id: target_owner,
                    fit_id: fid,
                },
            );
        }
        Some(Ok(PyRep::Bool(deleted)))
    }
}

#[cfg(test)]
#[path = "fitting_mgr_service_tests.rs"]
mod fitting_mgr_service_tests;

