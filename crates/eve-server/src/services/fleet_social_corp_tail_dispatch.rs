//! Corporation Governance Tail RPC Dispatch (`SO-3`, `feat:corporation.*`).

#![forbid(unsafe_code)]

use crate::router::{cache_notifications::NotificationTarget, ServerContext};
use eve_db::corp_roles;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};

use super::fleet_social_corps::{promote_due_pending_corp_grants, read_corp_gov, with_corp_gov};

#[allow(clippy::too_many_arguments)]
pub fn handle_corp_gov_tail(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    corp_id: i64,
    char_id: i64,
    now_unix: u64,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let has_director = ctx.db.with_snapshot(|snap| {
        snap.corporations.get(&corp_id).map_or(false, |c| {
            let r = c.member_roles.get(&char_id).copied().unwrap_or(0);
            c.ceo_id == char_id || corp_roles::has_role(r, corp_roles::ROLE_DIRECTOR)
        })
    });
    let has_comms = ctx.db.with_snapshot(|snap| {
        snap.corporations.get(&corp_id).map_or(false, |c| {
            let r = c.member_roles.get(&char_id).copied().unwrap_or(0);
            c.ceo_id == char_id
                || corp_roles::has_role(r, corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_PERSONNEL_MANAGER | (1u64 << 55))
        })
    });
    let deny_role = |msg: &'static str| -> Option<Result<PyRep, BridgeRpcError>> {
        Some(Err(BridgeRpcError {
            code: "CorpRoleDenied".into(),
            message: msg.into(),
        }))
    };

    match (service, method) {
        ("corpRegistry", "GetPendingAutoKicks") => {
            let pending: Vec<(i64, u64)> = read_corp_gov(ctx, |g| {
                g.get(&corp_id)
                    .map(|g| g.pending_auto_kicks.iter().map(|(&k, &v)| (k, v)).collect())
                    .unwrap_or_default()
            });
            let rows: Vec<Vec<PyRep>> = pending
                .into_iter()
                .map(|(cid, ts)| vec![PyRep::Integer(cid), PyRep::Integer(ts as i64)])
                .collect();
            Some(Ok(PyRep::rowset(vec!["characterID", "queuedDateTime"], rows)))
        }

        ("corpRegistry", "SetCorpWelcomeMail") => {
            if !has_comms {
                return deny_role("Setting corporation welcome mail requires Communications Officer or Director role");
            }
            let subject = args.first().and_then(|a| a.as_str()).unwrap_or("Welcome to the Corp");
            let body = args.get(1).and_then(|a| a.as_str()).unwrap_or("Fly safe o7");
            let enabled = args
                .get(2)
                .and_then(|a| match a {
                    PyRep::Bool(b) => Some(*b),
                    PyRep::Integer(i) => Some(*i != 0),
                    _ => None,
                })
                .unwrap_or(true);
            with_corp_gov(ctx, |g| {
                g.entry(corp_id).or_default().welcome_mail = (enabled, subject.to_string(), body.to_string());
            });
            ctx.notify(
                NotificationTarget::Corporation(corp_id),
                "OnCorporationWelcomeMailChanged",
                "corpid",
                vec![PyRep::Integer(char_id), PyRep::Integer(now_unix as i64)],
            );
            Some(Ok(PyRep::keyval([
                ("enabled", PyRep::Bool(enabled)),
                ("subject", PyRep::String(subject.to_string())),
                ("body", PyRep::String(body.to_string())),
            ])))
        }

        ("corpRegistry", "GetCorpWelcomeMail") => {
            let (enabled, subject, body) = read_corp_gov(ctx, |g| {
                g.get(&corp_id)
                    .map(|s| s.welcome_mail.clone())
                    .unwrap_or((false, String::new(), String::new()))
            });
            Some(Ok(PyRep::keyval([
                ("enabled", PyRep::Bool(enabled)),
                ("subject", PyRep::String(subject)),
                ("body", PyRep::String(body)),
            ])))
        }

        ("corpRegistry", "RegisterNewCorpMailRestrictionSettings") => {
            if !has_comms {
                return deny_role("Updating corporation mail restriction settings requires Communications Officer or Director role");
            }
            let restricted = matches!(args.first(), Some(PyRep::Bool(true)) | Some(PyRep::Integer(1)));
            let prev = with_corp_gov(ctx, |g| {
                let s = g.entry(corp_id).or_default();
                std::mem::replace(&mut s.restrict_corp_mails, restricted)
            });
            let change = PyRep::Dict(vec![(
                PyRep::String(("restrictCorpMails").into()),
                PyRep::Tuple(vec![PyRep::Bool(prev), PyRep::Bool(restricted)]),
            )]);
            ctx.notify(
                NotificationTarget::Corporation(corp_id),
                "OnCorporationChanged",
                "corpid",
                vec![PyRep::Integer(corp_id), change],
            );
            Some(Ok(PyRep::Bool(restricted)))
        }

        ("corpRegistry", "DoesCorpRestrictCorpMails") => {
            let restricted = read_corp_gov(ctx, |g| g.get(&corp_id).is_some_and(|s| s.restrict_corp_mails));
            Some(Ok(PyRep::Bool(restricted)))
        }

        ("corpRegistry", "RegisterNewAcceptStructureSettings") => {
            if !has_director {
                return deny_role("Updating corporation structure transfer settings requires CEO or Director role");
            }
            let accept = matches!(args.first(), Some(PyRep::Bool(true)) | Some(PyRep::Integer(1)));
            let prev = with_corp_gov(ctx, |g| {
                let s = g.entry(corp_id).or_default();
                let old = if s.accept_structures_set { s.accept_structures } else { true };
                s.accept_structures_set = true;
                s.accept_structures = accept;
                old
            });
            let change = PyRep::Dict(vec![(
                PyRep::String(("acceptStructures").into()),
                PyRep::Tuple(vec![PyRep::Bool(prev), PyRep::Bool(accept)]),
            )]);
            ctx.notify(
                NotificationTarget::Corporation(corp_id),
                "OnCorporationChanged",
                "corpid",
                vec![PyRep::Integer(corp_id), change],
            );
            Some(Ok(PyRep::Bool(accept)))
        }

        ("corpRegistry", "DoesMyCorpAcceptStructures") => {
            let accept = read_corp_gov(ctx, |g| {
                g.get(&corp_id)
                    .map(|s| if s.accept_structures_set { s.accept_structures } else { true })
                    .unwrap_or(true)
            });
            Some(Ok(PyRep::Bool(accept)))
        }

        ("corpRegistry", "SetStructureReinforceDefault") => {
            if !has_director {
                return deny_role("Setting corporation structure reinforcement default requires CEO or Director role");
            }
            let hour = args.first().and_then(|a| a.as_i64()).unwrap_or(18).clamp(0, 23) as u8;
            with_corp_gov(ctx, |g| {
                g.entry(corp_id).or_default().reinforce_hour = hour;
            });
            Some(Ok(PyRep::Integer(hour as i64)))
        }

        ("corpRegistry", "GetStructureReinforceDefault") => {
            let hour = read_corp_gov(ctx, |g| g.get(&corp_id).map(|s| s.reinforce_hour).unwrap_or(18));
            Some(Ok(PyRep::Integer(hour as i64)))
        }

        ("corpRegistry", "GetDivisionNames")
        | ("corpmgr", "GetDivisionNames")
        | ("corporationSvc", "GetDivisionNames") => {
            let target_cid = args.first().and_then(|a| a.as_i64()).unwrap_or(corp_id);
            let default_h = [
                "1st Division",
                "2nd Division",
                "3rd Division",
                "4th Division",
                "5th Division",
                "6th Division",
                "7th Division",
            ];
            let default_w = [
                "Master Wallet",
                "2nd Wallet",
                "3rd Wallet",
                "4th Wallet",
                "5th Wallet",
                "6th Wallet",
                "7th Wallet",
            ];
            let entries = ctx.db.with_snapshot(|snap| {
                let h_divs = snap.corp_hangar_divisions.get(&target_cid);
                let w_divs = snap.corporations.get(&target_cid).map(|c| &c.wallet_divisions);
                let mut map = Vec::new();
                for i in 1..=7u8 {
                    let h_name = h_divs
                        .and_then(|m| m.get(&i).cloned())
                        .unwrap_or_else(|| default_h[(i - 1) as usize].to_string());
                    let w_name = w_divs
                        .and_then(|w| w.iter().find(|d| d.division_id == i).map(|d| d.name.clone()))
                        .unwrap_or_else(|| default_w[(i - 1) as usize].to_string());
                    map.push((format!("division{i}"), PyRep::String(h_name.clone())));
                    map.push((format!("{i}"), PyRep::String(h_name.clone())));
                    map.push((format!("{}", 114 + u16::from(i)), PyRep::String(h_name)));
                    map.push((format!("walletDivision{i}"), PyRep::String(w_name.clone())));
                    map.push((format!("{}", 7 + i), PyRep::String(w_name.clone())));
                    map.push((format!("{}", 999 + u16::from(i)), PyRep::String(w_name)));
                }
                map
            });
            Some(Ok(PyRep::KeyVal(entries.into_iter().collect())))
        }

        ("corpRegistry", "CharGetAllyBaseCost") => {
            let cost = ctx.db.with_snapshot(|snap| {
                ctx.sde.types.values().find_map(|t| {
                    let base = *t.default_attributes.get(&1820)?;
                    let per_lvl = t.default_attributes.get(&1821).copied().unwrap_or(0.0);
                    let lvl = snap
                        .skills
                        .get(&char_id)
                        .and_then(|m| m.get(&t.type_id))
                        .map(|s| s.level as f64)
                        .unwrap_or(0.0);
                    Some(base * (1.0 + per_lvl * lvl / 100.0))
                })
            });
            match cost {
                Some(c) => Some(Ok(PyRep::Real(c))),
                None => Some(Err(BridgeRpcError {
                    code: "SdeDataMissing".into(),
                    message: "No SDE type carries baseDefenderAllyCost".into(),
                })),
            }
        }

        ("corpRegistry", "UpdateAttributes") | ("corpRegistry", "UpdateCorporationAbilities") => {
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let mut skills_owner_id = char_id;
                if let Some(corp) = snap.corporations.get(&corp_id) {
                    let caller_role = corp
                        .member_roles
                        .get(&char_id)
                        .copied()
                        .unwrap_or(session.corp_role);
                    if corp.ceo_id != 0
                        && corp.ceo_id != char_id
                        && !eve_db::corp_roles::has_role(
                            caller_role,
                            eve_db::corp_roles::ROLE_DIRECTOR,
                        )
                    {
                        return Err(BridgeRpcError {
                            code: "CorpRoleDenied".into(),
                            message: "Updating corporation attributes requires CEO or Director role".into(),
                        });
                    }
                    if corp.ceo_id > 0 && snap.skills.contains_key(&corp.ceo_id) {
                        skills_owner_id = corp.ceo_id;
                    }
                }
                let member_limit = snap
                    .skills
                    .get(&skills_owner_id)
                    .map(|skills| {
                        skills
                            .values()
                            .map(|s| {
                                let per_lvl = ctx
                                    .sde
                                    .types
                                    .get(&s.type_id)
                                    .and_then(|t| t.default_attributes.get(&191).copied())
                                    .unwrap_or(0.0);
                                (per_lvl * s.level as f64) as i64
                            })
                            .sum::<i64>()
                    })
                    .unwrap_or(0)
                    .max(1);
                snap.corp_governance
                    .entry(corp_id)
                    .or_default()
                    .member_limit = Some(member_limit as usize);
                snap.corp_member_limits.insert(corp_id, member_limit);
                Ok(member_limit)
            });
            match res {
                Ok(member_limit) => Some(Ok(PyRep::keyval([
                    ("corporationID", PyRep::Integer(corp_id)),
                    ("memberLimit", PyRep::Integer(member_limit)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        ("corpStationMgr", "MoveCorpHQHere") => {
            let station_id = args
                .first()
                .and_then(|a| a.as_i64())
                .or(session.station_id)
                .or(session.structure_id)
                .unwrap_or(0);
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(corp) = snap.corporations.get_mut(&corp_id) else {
                    return Err(BridgeRpcError {
                        code: "CorporationNotFound".into(),
                        message: format!("Corporation {corp_id} not found"),
                    });
                };
                let caller_role = corp.member_roles.get(&char_id).copied().unwrap_or(0);
                if char_id != corp.ceo_id
                    && !corp_roles::has_role(caller_role, corp_roles::ROLE_DIRECTOR)
                {
                    return Err(BridgeRpcError {
                        code: "CorpRoleDenied".into(),
                        message: "Moving corporation HQ requires CEO or Director role".into(),
                    });
                }
                let has_office = snap.items.values().any(|i| {
                    i.owner_id == corp_id && i.location_id == station_id && i.type_id == 27
                });
                if !has_office {
                    return Err(BridgeRpcError {
                        code: "NoOfficeAtStation".into(),
                        message: format!("Corporation {corp_id} has no office at station {station_id}"),
                    });
                }
                let old_hq = corp.hq_station_id.replace(station_id);
                Ok(old_hq)
            });
            match res {
                Ok(old_hq) => {
                    let change = PyRep::Dict(vec![(
                        PyRep::String(("stationID").into()),
                        PyRep::Tuple(vec![
                            old_hq.map_or(PyRep::None, PyRep::Integer),
                            PyRep::Integer(station_id),
                        ]),
                    )]);
                    ctx.notify(
                        NotificationTarget::Corporation(corp_id),
                        "OnCorporationChanged",
                        "corpid",
                        vec![PyRep::Integer(corp_id), change],
                    );
                    Some(Ok(PyRep::keyval([
                        ("corporationID", PyRep::Integer(corp_id)),
                        ("stationID", PyRep::Integer(station_id)),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("corpmgr", "GetAssetInventory")
        | ("corpmgr", "GetAssetInventoryForLocation")
        | ("corpmgr", "SearchAssets") => {
            crate::services::station_structure::structure_upwell_ops::sync_auto_delivered_asset_safety_wraps(ctx);
            let (req_corp_id, loc_filter, which_filter, search_offset) = match method {
                "GetAssetInventory" => (
                    args.first().and_then(|a| a.as_i64()).unwrap_or(corp_id),
                    None,
                    args.get(1).and_then(|a| a.as_str()),
                    0usize,
                ),
                "GetAssetInventoryForLocation" => {
                    if args.len() >= 2 && args.get(1).and_then(|a| a.as_i64()).is_some() {
                        (
                            args.first().and_then(|a| a.as_i64()).unwrap_or(corp_id),
                            args.get(1).and_then(|a| a.as_i64()),
                            args.get(2).and_then(|a| a.as_str()),
                            0usize,
                        )
                    } else {
                        (
                            corp_id,
                            args.first().and_then(|a| a.as_i64()),
                            args.get(1).and_then(|a| a.as_str()),
                            0usize,
                        )
                    }
                }
                _ => {
                    if let Some(first_cid) = args
                        .first()
                        .and_then(|a| a.as_i64())
                        .filter(|_| args.get(1).and_then(|a| a.as_str()).is_some())
                    {
                        (first_cid, None, args.get(1).and_then(|a| a.as_str()), 1usize)
                    } else {
                        (corp_id, None, args.first().and_then(|a| a.as_str()), 0usize)
                    }
                }
            };
            let cat_filter = (method == "SearchAssets")
                .then(|| args.get(1 + search_offset).and_then(|a| a.as_i64()).filter(|&v| v > 0))
                .flatten();
            let grp_filter = (method == "SearchAssets")
                .then(|| args.get(2 + search_offset).and_then(|a| a.as_i64()).filter(|&v| v > 0))
                .flatten();
            let type_filter = (method == "SearchAssets")
                .then(|| args.get(3 + search_offset).and_then(|a| a.as_i64()).filter(|&v| v > 0))
                .flatten();
            let min_qty_filter = (method == "SearchAssets")
                .then(|| args.get(4 + search_offset).and_then(|a| a.as_i64()).filter(|&v| v > 0))
                .flatten();
            let res = ctx.db.with_snapshot(|snap| {
                let Some(corp) = snap.corporations.get(&req_corp_id) else {
                    return Err(BridgeRpcError {
                        code: "CorporationNotFound".into(),
                        message: format!("Corporation {req_corp_id} not found"),
                    });
                };
                let caller_role = corp.member_roles.get(&char_id).copied().unwrap_or(0);
                let allowed_roles = corp_roles::ROLE_DIRECTOR
                    | corp_roles::ROLE_ACCOUNTANT
                    | corp_roles::ROLE_JUNIOR_ACCOUNTANT
                    | corp_roles::ROLE_TRADER;
                if char_id != corp.ceo_id && caller_role != u64::MAX && (caller_role & allowed_roles) == 0 {
                    return Err(BridgeRpcError {
                        code: "CorpRoleDenied".into(),
                        message: "Viewing corporation asset inventory requires Director, Accountant, or Trader role".into(),
                    });
                }
                let gov_offices = snap.corp_governance.get(&req_corp_id).map(|g| &g.offices);
                let resolve_station = |raw_loc: i64| -> i64 {
                    if let Some(parent) = snap.items.get(&raw_loc).filter(|p| p.type_id == 27) {
                        return parent.location_id;
                    }
                    if let Some(offices) = gov_offices {
                        for (&k, &v) in offices {
                            if v == raw_loc {
                                return k;
                            }
                            if k == raw_loc && (60_000_000..65_000_000).contains(&v) {
                                return v;
                            }
                        }
                    }
                    raw_loc
                };
                let mut matched_items: Vec<(&eve_db::InventoryItemRecord, i64)> = snap
                    .items
                    .values()
                    .filter_map(|i| {
                        if i.owner_id != req_corp_id || i.type_id == 27 || i.quantity <= 0 {
                            return None;
                        }
                        let flag_id = i.flag as i64;
                        let which_ok = match which_filter {
                            Some("deliveries") => {
                                i.flag == eve_db::InventoryFlag::Deliveries
                                    || i.flag == eve_db::InventoryFlag::CorpDeliveries
                                    || flag_id == 62
                            }
                            Some("lockdown") => flag_id == 63,
                            Some("impounded") | Some("junk") => flag_id == 72,
                            Some("offices") => (115..=122).contains(&flag_id) || flag_id == 4,
                            _ => {
                                i.flag != eve_db::InventoryFlag::Deliveries
                                    && i.flag != eve_db::InventoryFlag::CorpDeliveries
                                    && flag_id != 62
                                    && flag_id != 63
                                    && flag_id != 72
                            }
                        };
                        if !which_ok {
                            return None;
                        }
                        let station_loc = resolve_station(i.location_id);
                        let sde_type = ctx.sde.types.get(&i.type_id);
                        let cat_ok =
                            cat_filter.is_none_or(|cid| sde_type.is_some_and(|t| t.category_id as i64 == cid));
                        let grp_ok =
                            grp_filter.is_none_or(|gid| sde_type.is_some_and(|t| t.group_id as i64 == gid));
                        let loc_ok = loc_filter.is_none_or(|loc| station_loc == loc || i.location_id == loc);
                        (loc_ok
                            && type_filter.is_none_or(|tid| i.type_id as i64 == tid)
                            && min_qty_filter.is_none_or(|mq| i.quantity as i64 >= mq)
                            && cat_ok
                            && grp_ok)
                            .then_some((i, station_loc))
                    })
                    .collect();
                matched_items.sort_by_key(|(i, _)| i.item_id);
                if method == "GetAssetInventory" {
                    let mut seen_locs = std::collections::HashSet::new();
                    matched_items.retain(|(_, st_loc)| seen_locs.insert(*st_loc));
                }
                let rows: Vec<Vec<PyRep>> = matched_items
                    .into_iter()
                    .map(|(i, station_loc)| {
                        let solar_system_id = ctx
                            .sde
                            .stations
                            .get(&station_loc)
                            .map(|st| st.solar_system_id as i64)
                            .or_else(|| snap.structures.get(&station_loc).map(|st| st.solar_system_id as i64))
                            .or_else(|| (30_000_000..=33_000_000).contains(&station_loc).then_some(station_loc))
                            .unwrap_or(0);
                        vec![
                            PyRep::Integer(i.item_id),
                            PyRep::Integer(i.type_id as i64),
                            PyRep::Integer(i.owner_id),
                            PyRep::Integer(station_loc),
                            PyRep::Integer(solar_system_id),
                            PyRep::Integer(i.flag as i64),
                            PyRep::Integer(i.quantity as i64),
                        ]
                    })
                    .collect();
                Ok(rows)
            });
            match res {
                Ok(rows) => Some(Ok(PyRep::rowset(
                    vec![
                        "itemID",
                        "typeID",
                        "ownerID",
                        "locationID",
                        "solarsystemID",
                        "flagID",
                        "quantity",
                    ],
                    rows,
                ))),
                Err(e) => Some(Err(e)),
            }
        }

        ("corpmgr", "GetAggressionSettings")
        | ("corpmgr", "GetAggressionSettingsForCorps")
        | ("corpRegistry", "GetAggressionSettings") => {
            let resolve_corp_agg = |target_corp_id: i64| -> PyRep {
                promote_due_pending_corp_grants(ctx, target_corp_id);
                let (ff, pending_val, pending_until) = read_corp_gov(ctx, |g| {
                    if let Some(s) = g.get(&target_corp_id) {
                        if let Some((pending_ff, eff_unix)) = s.pending_friendly_fire {
                            if now_unix >= eff_unix {
                                (pending_ff, None, None)
                            } else {
                                (s.friendly_fire_legal, Some(pending_ff), Some(eff_unix as i64))
                            }
                        } else {
                            (s.friendly_fire_legal, None, None)
                        }
                    } else {
                        (false, None, None)
                    }
                });
                let enable_after = match (ff, pending_val, pending_until) {
                    (_, Some(true), Some(ts)) => PyRep::Integer(ts),
                    (true, _, _) => PyRep::Integer(0),
                    _ => PyRep::None,
                };
                let disable_after = match (ff, pending_val, pending_until) {
                    (_, Some(false), Some(ts)) => PyRep::Integer(ts),
                    (false, _, _) => PyRep::Integer(0),
                    _ => PyRep::None,
                };
                PyRep::keyval([
                    ("corporationID", PyRep::Integer(target_corp_id)),
                    ("isFriendlyFireLegal", PyRep::Bool(ff)),
                    ("friendlyFireLegal", PyRep::Bool(ff)),
                    ("_enableAfter", enable_after),
                    ("_disableAfter", disable_after),
                    (
                        "pendingFriendlyFireLegal",
                        pending_val.map(PyRep::Bool).unwrap_or(PyRep::None),
                    ),
                    (
                        "changeAtTime",
                        pending_until.map(PyRep::Integer).unwrap_or(PyRep::None),
                    ),
                ])
            };
            if method == "GetAggressionSettingsForCorps" {
                let corp_ids: Vec<i64> = match args.first() {
                    Some(PyRep::List(items)) | Some(PyRep::Tuple(items)) => {
                        items.iter().filter_map(|x| x.as_i64()).collect()
                    }
                    Some(other) => other.as_i64().into_iter().collect(),
                    None => vec![corp_id],
                };
                let entries = corp_ids
                    .into_iter()
                    .map(|cid| (PyRep::Integer(cid), resolve_corp_agg(cid)))
                    .collect();
                Some(Ok(PyRep::Dict(entries)))
            } else {
                let target_corp_id = args.first().and_then(|a| a.as_i64()).unwrap_or(corp_id);
                Some(Ok(resolve_corp_agg(target_corp_id)))
            }
        }

        ("corpRegistry", "RegisterNewAggressionSettings") => {
            let is_ceo_or_dir = ctx.db.with_snapshot(|snap| {
                snap.corporations.get(&corp_id).is_some_and(|c| {
                    char_id == c.ceo_id
                        || corp_roles::has_role(
                            c.member_roles.get(&char_id).copied().unwrap_or(0),
                            corp_roles::ROLE_DIRECTOR,
                        )
                })
            });
            if !is_ceo_or_dir {
                return Some(Err(BridgeRpcError {
                    code: "CorpRoleDenied".into(),
                    message: "Only CEO or Director can change friendly fire settings".into(),
                }));
            }
            let ff = matches!(args.first(), Some(PyRep::Bool(true)) | Some(PyRep::Integer(1)));
            let effective_at = now_unix + 86_400;
            with_corp_gov(ctx, |g| {
                g.entry(corp_id).or_default().pending_friendly_fire = Some((ff, effective_at));
            });
            let settings_kv = PyRep::keyval([
                ("corporationID", PyRep::Integer(corp_id)),
                ("isFriendlyFireLegal", PyRep::Bool(ff)),
                ("friendlyFireLegal", PyRep::Bool(ff)),
                (
                    "_enableAfter",
                    if ff { PyRep::Integer(effective_at as i64) } else { PyRep::None },
                ),
                (
                    "_disableAfter",
                    if !ff {
                        PyRep::Integer(effective_at as i64)
                    } else {
                        PyRep::Integer(0)
                    },
                ),
                ("changeAtTime", PyRep::Integer(effective_at as i64)),
            ]);
            let notif_kv = PyRep::keyval([
                ("corporationID", PyRep::Integer(corp_id)),
                ("isFriendlyFireLegal", PyRep::Bool(ff)),
                ("changeAtTime", PyRep::Integer(effective_at as i64)),
            ]);
            ctx.notify(
                NotificationTarget::Corporation(corp_id),
                "OnCorpAggressionSettingsChange",
                "corpid",
                vec![notif_kv],
            );
            Some(Ok(settings_kv))
        }

        _ => None,
    }
}
