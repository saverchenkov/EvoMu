//! `corp_registry_ops` — Supplemental handlers for `corpRegistry` machoNet RPCs.
//!
//! Provides handlers for corp kill reports, suggested tickers, moniker binding,
//! owner lookup, division account keys, and introspection methods.

#![forbid(unsafe_code)]

use std::collections::HashMap;

use crate::router::{BoundObjectEntry, ServerContext};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, DBRowDescriptor, PyRep};

fn item_to_packed_cells(ctx: &ServerContext, i: &eve_db::InventoryItemRecord) -> Vec<PyRep> {
    let (group_id, category_id) = ctx
        .sde
        .types
        .get(&i.type_id)
        .map(|t| (t.group_id as i64, t.category_id as i64))
        .unwrap_or((0, 0));
    let qty = if i.is_blueprint_copy {
        -2
    } else if i.singleton {
        -1
    } else {
        i.quantity as i64
    };
    let custom_name = ctx
        .db
        .with_snapshot(|snap| snap.item_custom_names.get(&i.item_id).cloned())
        .unwrap_or_default();
    vec![
        PyRep::Integer(i.item_id),
        PyRep::Integer(i.type_id as i64),
        PyRep::Integer(i.owner_id),
        PyRep::Integer(i.location_id),
        PyRep::Integer(i.flag as i64),
        PyRep::Integer(qty),
        PyRep::Integer(group_id),
        PyRep::Integer(category_id),
        PyRep::String(custom_name),
    ]
}

/// Intercepts `invbroker.List` and `invbroker.Add` when operating on corporation hangar divisions (`116..=122`)
/// to enforce 24h-delayed division query/take roles (`feat:corporation.hangar-divisions`), `corpFittingMgr` role gates,
/// `corporationSvc` employment records, and fleet Command Burst scope (`feat:fleet.fleet-boost-scope`).
pub async fn handle_corp_hangar_invbroker(
    ctx: &ServerContext,
    session: &mut SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    if service == "corporationSvc"
        && matches!(
            method,
            "GetEmploymentRecord" | "GetEmployementRecordAndCharacterTransfers"
        )
    {
        return super::fleet_social::handle(ctx, session, service, method, args, kwargs).await;
    }
    if service == "dogmaIM" && method == "Activate" && session.fleet_id.is_some() {
        if let Some(module_id) = args.first().and_then(|a| a.as_i64()) {
            let burst_info = ctx.db.with_snapshot(|snap| {
                let mod_item = snap.items.get(&module_id)?;
                let mod_type = ctx.sde.types.get(&mod_item.type_id)?;
                if mod_type.group_id != 1770 {
                    return None;
                }
                let ship_id = mod_item.location_id;
                let charge_tid = snap
                    .items
                    .values()
                    .find(|i| {
                        i.location_id == ship_id
                            && i.item_id != module_id
                            && i.flag == mod_item.flag
                            && i.quantity > 0
                    })
                    .map(|i| i.type_id);
                let aoe_range = mod_type
                    .default_attributes
                    .get(&54)
                    .copied()
                    .unwrap_or(15_000.0)
                    .max(15_000.0);
                Some((ship_id, charge_tid, aoe_range))
            });
            if let Some((ship_id, charge_tid_opt, aoe_range)) = burst_info {
                let (_, char_id) = match ServerContext::require_character(session) {
                    Ok(v) => v,
                    Err(e) => return Some(Err(e)),
                };
                let fid = session.fleet_id.unwrap_or(0);
                let sys_id = session.solar_system_id.unwrap_or(30_000_142);
                let candidates: Vec<(i64, i64)> = ctx.db.with_snapshot(|snap| {
                    let mut out = vec![(char_id, ship_id)];
                    if let Some(fl) = snap.fleets.get(&fid) {
                        for &m_id in &fl.members {
                            if m_id != char_id
                                && super::fleet_social::is_in_fleet_boost_scope(fl, char_id, m_id)
                            {
                                if let Some(ch) = snap.characters.get(&m_id) {
                                    if ch.solar_system_id == sys_id
                                        && ch.station_id.is_none()
                                        && ch.active_ship_id > 0
                                    {
                                        out.push((m_id, ch.active_ship_id));
                                    }
                                }
                            }
                        }
                    }
                    out
                });
                let sys = ctx.systems.get_or_spawn(sys_id);
                let booster_pos = sys
                    .get_ball_snapshot(ship_id)
                    .await
                    .ok()
                    .flatten()
                    .map(|(_, b)| b.position)
                    .unwrap_or(glam::DVec3::ZERO);
                let mut affected_members = Vec::new();
                let mut affected_ships = Vec::new();
                for (mcid, msid) in candidates {
                    let in_range = if msid == ship_id {
                        true
                    } else if let Ok(Some((_, mb))) = sys.get_ball_snapshot(msid).await {
                        (mb.position - booster_pos).length() <= aoe_range
                    } else {
                        true
                    };
                    if in_range {
                        affected_members.push(PyRep::Integer(mcid));
                        affected_ships.push(PyRep::Integer(msid));
                    }
                }
                ctx.db.with_mut_snapshot(|snap| {
                    snap.module_active_states.insert(module_id);
                    if let Some(ctid) = charge_tid_opt {
                        snap.module_active_charges.insert(module_id, ctid);
                    }
                });
                return Some(Ok(PyRep::keyval([
                    ("moduleID", PyRep::Integer(module_id)),
                    ("active", PyRep::Bool(true)),
                    (
                        "chargeTypeID",
                        charge_tid_opt
                            .map(|c| PyRep::Integer(c as i64))
                            .unwrap_or(PyRep::None),
                    ),
                    ("affectedMemberIDs", PyRep::List(affected_members)),
                    ("affectedShipIDs", PyRep::List(affected_ships)),
                ])));
            }
        }
    }
    if service == "beyonce"
        && method == "CmdWarpToStuff"
        && args.first().and_then(|v| v.as_str()) == Some("bookmark")
    {
        let (_, char_id) = match ServerContext::require_character(session) {
            Ok(v) => v,
            Err(e) => return Some(Err(e)),
        };
        let bm_id = args.get(1).and_then(|v| v.as_i64()).unwrap_or(0);
        let min_range = args.get(2).and_then(|v| v.as_f64()).unwrap_or(0.0);
        let now = ctx.now_unix();
        if super::fleet_social::is_bookmark_expired(bm_id, now) {
            return Some(Err(BridgeRpcError {
                code: "BookmarkExpired".into(),
                message: format!("Bookmark {bm_id} has expired"),
            }));
        }
        let bm = match ctx.db.with_snapshot(|s| s.bookmarks.get(&bm_id).cloned()) {
            Some(b) => b,
            None => {
                return Some(Err(BridgeRpcError {
                    code: "BookmarkNotFound".into(),
                    message: format!("Bookmark {bm_id} not found"),
                }));
            }
        };
        let sys_id = session.solar_system_id.unwrap_or(bm.solar_system_id);
        let ship_id = session
            .ship_id
            .or_else(|| {
                ctx.db
                    .with_snapshot(|s| s.characters.get(&char_id).map(|c| c.active_ship_id))
            })
            .unwrap_or(0);
        let dest = glam::DVec3::new(bm.x, bm.y, bm.z);
        let handle = ctx.systems.get_or_spawn(sys_id);
        let cur_pos = handle
            .get_ball_snapshot(ship_id)
            .await
            .ok()
            .flatten()
            .map(|(_, b)| b.position)
            .unwrap_or(glam::DVec3::ZERO);
        let dir = (dest - cur_pos).normalize_or_zero();
        let land_pos = if min_range > 0.0 && dir.length_squared() > 0.0 {
            dest - dir * min_range
        } else {
            dest
        };
        let _ = handle
            .set_movement_mode(
                ship_id,
                eve_destiny::MovementMode::new_warp(cur_pos, land_pos, 3.0),
            )
            .await;
        return Some(Ok(PyRep::keyval([
            ("bookmarkID", PyRep::Integer(bm_id)),
            ("warping", PyRep::Bool(true)),
        ])));
    }
    if service == "corpFittingMgr"
        && matches!(
            method,
            "SaveFitting"
                | "SaveManyFittings"
                | "DeleteFitting"
                | "DeleteManyFittings"
                | "UpdateFitting"
                | "UpdateNameAndDescription"
        )
    {
        let (_, char_id) = match ServerContext::require_character(session) {
            Ok(v) => v,
            Err(e) => return Some(Err(e)),
        };
        let owner_id = args
            .first()
            .and_then(|a| a.as_i64())
            .or(session.corporation_id)
            .unwrap_or(char_id);
        if owner_id >= 2_000_000 {
            let (is_ceo, roles) = super::fleet_social::get_effective_corp_roles(ctx, owner_id, char_id);
            let has_role = is_ceo
                || eve_db::corp_roles::has_role(
                    roles,
                    eve_db::corp_roles::ROLE_DIRECTOR | (1u64 << 41),
                );
            if !has_role {
                return Some(Err(BridgeRpcError {
                    code: "CorpRoleDenied".into(),
                    message: "Managing corporation fittings requires Fitting Manager or Director role"
                        .into(),
                }));
            }
        }
        return None;
    }
    if service != "invbroker" {
        return None;
    }
    if method == "List" {
        let list_flag_filter = args
            .first()
            .and_then(|a| a.as_i64())
            .or_else(|| kwargs.get("flag").and_then(|v| v.as_i64()))
            .filter(|&f| (116..=122).contains(&f))?;
        let (_, char_id) = match ServerContext::require_character(session) {
            Ok(v) => v,
            Err(e) => return Some(Err(e)),
        };
        let Some(bound_loc_id) = bound_entry
            .map(|b| b.object_id)
            .or(session.station_id)
            .or(session.structure_id)
            .or(session.ship_id)
        else {
            return Some(Err(BridgeRpcError {
                code: "InvalidLocation".into(),
                message: "invbroker.List requires a bound inventory location or active station/ship".into(),
            }));
        };
        let corp_id = session
            .corporation_id
            .or_else(|| {
                ctx.db
                    .with_snapshot(|snap| snap.characters.get(&char_id).map(|c| c.corporation_id))
            })
            .unwrap_or(0);
        let (is_ceo, roles) = super::fleet_social::get_effective_corp_roles(ctx, corp_id, char_id);
        let div_idx = (list_flag_filter - 116) as u32;
        let req_query_role = 1u64 << (20 + div_idx);
        if !is_ceo && !eve_db::corp_roles::has_role(roles, req_query_role) {
            return Some(Err(BridgeRpcError {
                code: "CorpRoleDenied".into(),
                message: format!("Corporation role denied for division {} query", div_idx + 1),
            }));
        }
        let rows = ctx.db.with_snapshot(|snap| {
            snap.items
                .values()
                .filter(|i| {
                    i.location_id == bound_loc_id
                        && i.owner_id == corp_id
                        && (i.flag as i64) == list_flag_filter
                })
                .map(|i| item_to_packed_cells(ctx, i))
                .collect()
        });
        return Some(Ok(DBRowDescriptor::inv_item_header().build_crowset(rows)));
    }
    if method == "Add" {
        let item_id = args.first().and_then(|a| a.as_i64())?;
        let (_, char_id) = match ServerContext::require_character(session) {
            Ok(v) => v,
            Err(e) => return Some(Err(e)),
        };
        let corp_id = session
            .corporation_id
            .or_else(|| {
                ctx.db
                    .with_snapshot(|snap| snap.characters.get(&char_id).map(|c| c.corporation_id))
            })
            .unwrap_or(0);
        let src_flag_opt = ctx.db.with_snapshot(|snap| {
            snap.items.get(&item_id).and_then(|it| {
                let fv = it.flag as i64;
                if it.owner_id == corp_id && (116..=122).contains(&fv) {
                    Some(fv)
                } else {
                    None
                }
            })
        });
        if let Some(flag_val) = src_flag_opt {
            let (is_ceo, roles) = super::fleet_social::get_effective_corp_roles(ctx, corp_id, char_id);
            let div_idx = (flag_val - 116) as u32;
            let req_take_role = 1u64 << (13 + div_idx);
            if !is_ceo && !eve_db::corp_roles::has_role(roles, req_take_role) {
                return Some(Err(BridgeRpcError {
                    code: "CorpRoleDenied".into(),
                    message: format!("Corporation role denied for division {} take", div_idx + 1),
                }));
            }
            ctx.db.with_mut_snapshot(|snap| {
                if let Some(it) = snap.items.get_mut(&item_id) {
                    it.owner_id = char_id;
                }
            });
        }
    }
    None
}

/// Dispatches supplemental `corpRegistry` RPC methods.
pub fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    if service != "corpRegistry" {
        return None;
    }

    match method {
        "GetEveOwners" => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let (corp_id, char_name, corp_name) = ctx.db.with_snapshot(|snap| {
                let ch = snap.characters.get(&char_id);
                let corp_id = ch.map(|c| c.corporation_id).unwrap_or(1000001);
                let char_name = ch.map(|c| c.name.clone()).unwrap_or_else(|| "Pilot".into());
                let corp_name = snap
                    .corporations
                    .get(&corp_id)
                    .map(|c| c.name.clone())
                    .unwrap_or_else(|| "Corporation".into());
                (corp_id, char_name, corp_name)
            });

            let columns = vec![
                "ownerID".into(),
                "ownerName".into(),
                "typeID".into(),
                "gender".into(),
                "corporationID".into(),
            ];
            let rows = vec![
                vec![
                    PyRep::Integer(char_id),
                    PyRep::String(char_name),
                    PyRep::Integer(1373), // Character
                    PyRep::Integer(0),
                    PyRep::Integer(corp_id),
                ],
                vec![
                    PyRep::Integer(corp_id),
                    PyRep::String(corp_name),
                    PyRep::Integer(2), // Corporation
                    PyRep::Integer(0),
                    PyRep::Integer(corp_id),
                ],
            ];
            Some(Ok(PyRep::Rowset {
                columns,
                rows,
            }))
        }

        "GetRecentKills" => {
            let columns = vec![
                "killID".into(),
                "victimCharacterID".into(),
                "victimCorporationID".into(),
                "victimShipTypeID".into(),
                "solarSystemID".into(),
                "killTime".into(),
            ];
            Some(Ok(PyRep::Rowset {
                columns,
                rows: Vec::new(),
            }))
        }

        "GetRecentLosses" => {
            let columns = vec![
                "killID".into(),
                "victimCharacterID".into(),
                "victimCorporationID".into(),
                "victimShipTypeID".into(),
                "solarSystemID".into(),
                "killTime".into(),
            ];
            Some(Ok(PyRep::Rowset {
                columns,
                rows: Vec::new(),
            }))
        }

        "GetSuggestedAllianceShortNames" => {
            let name = args.first().and_then(|a| a.as_str()).unwrap_or("Alliance");
            let words: Vec<&str> = name.split_whitespace().collect();
            let mut suggestions = Vec::new();
            if words.len() >= 3 {
                let acronym: String = words.iter().filter_map(|w| w.chars().next()).take(5).collect();
                suggestions.push(PyRep::String(acronym.to_uppercase()));
            }
            let clean: String = name.chars().filter(|c| c.is_alphanumeric()).collect();
            if clean.len() >= 3 {
                let prefix: String = clean.chars().take(4).collect();
                suggestions.push(PyRep::String(prefix.to_uppercase()));
            }
            if suggestions.is_empty() {
                suggestions.push(PyRep::String("ALLI".into()));
            }
            Some(Ok(PyRep::List(suggestions)))
        }

        "GetSuggestedTickerNames" => {
            let name = args.first().and_then(|a| a.as_str()).unwrap_or("Corporation");
            let words: Vec<&str> = name.split_whitespace().collect();
            let mut suggestions = Vec::new();
            if words.len() >= 3 {
                let acronym: String = words.iter().filter_map(|w| w.chars().next()).take(5).collect();
                suggestions.push(PyRep::String(acronym.to_uppercase()));
            }
            let clean: String = name.chars().filter(|c| c.is_alphanumeric()).collect();
            if clean.len() >= 3 {
                let prefix: String = clean.chars().take(4).collect();
                suggestions.push(PyRep::String(prefix.to_uppercase()));
            }
            if suggestions.is_empty() {
                suggestions.push(PyRep::String("CORP".into()));
            }
            Some(Ok(PyRep::List(suggestions)))
        }

        "SetAccountKey" => {
            let key = args.first().and_then(|a| a.as_i64()).unwrap_or(1000);
            if !(1000..=1006).contains(&key) {
                return Some(Err(BridgeRpcError {
                    code: "InvalidAccountKey".into(),
                    message: format!("Account key {key} must be between 1000 and 1006"),
                }));
            }
            Some(Ok(PyRep::None))
        }

        "ExecuteActions" => {
            Some(Ok(PyRep::Bool(true)))
        }

        "Bind" => {
            let bound_id = args.first().and_then(|a| a.as_i64()).unwrap_or(1);
            Some(Ok(PyRep::Integer(bound_id)))
        }

        "UnBind" | "Unbind" => {
            Some(Ok(PyRep::None))
        }

        "SetSessionCheck" => {
            Some(Ok(PyRep::None))
        }

        "clear_memoized" => {
            Some(Ok(PyRep::None))
        }

        "columns" => {
            let cols = vec![
                PyRep::String("corporationID".into()),
                PyRep::String("corporationName".into()),
                PyRep::String("ticker".into()),
            ];
            Some(Ok(PyRep::List(cols)))
        }

        "header" => {
            let h = vec![
                PyRep::String("corporationID".into()),
                PyRep::String("corporationName".into()),
                PyRep::String("ticker".into()),
            ];
            Some(Ok(PyRep::Tuple(h)))
        }

        "itervalues" => {
            Some(Ok(PyRep::List(Vec::new())))
        }

        _ => None,
    }
}
