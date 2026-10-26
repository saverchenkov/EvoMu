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
    if service == "corporationSvc" && method == "GetEmploymentRecord" {
        return super::fleet_social::handle(ctx, session, service, method, args, kwargs).await;
    }
    if service == "dogmaIM" && method == "Activate" && session.fleet_id.is_some() {
        if let Some(module_id) = args.first().and_then(|a| a.as_i64()) {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let ship_id = session.ship_id.unwrap_or(0);
            if ctx.get_or_rebuild_ship_fitting(char_id, ship_id).is_ok() {
                // Defer to full tactical_modules handling
                return None;
            }
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
                let fid = session.fleet_id.unwrap_or(0);
                let sys_id = session.solar_system_id.unwrap_or(0);
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
        if super::fleet_social::is_bookmark_expired(ctx, bm_id, now) {
            return Some(Err(BridgeRpcError {
                code: "BookmarkExpired".into(),
                message: format!("Bookmark {bm_id} has expired"),
            }));
        }
        let Some(sys_id) = session.solar_system_id else {
            return Some(Err(BridgeRpcError {
                code: "NoSolarSystem".into(),
                message: "Session has no active solar system".into(),
            }));
        };
        let bm = match ctx.db.with_snapshot(|s| {
            let b = s.bookmarks.get(&bm_id)?;
            if b.solar_system_id != sys_id {
                return None;
            }
            let can_use = b.owner_id == char_id
                || b.folder_id.is_some_and(|fid| {
                    s.bookmark_folders.get(&fid).is_some_and(|f| {
                        crate::services::fleet_social::fleet_social_access_group_bookmarks::folder_access_level(
                            s, session, char_id, f,
                        ) >= crate::services::fleet_social::fleet_social_access_group_bookmarks::AccessLevel::Use
                    })
                });
            if !can_use {
                return None;
            }
            Some(b.clone())
        }) {
            Some(b) => b,
            None => {
                return Some(Err(BridgeRpcError {
                    code: "BookmarkNotFound".into(),
                    message: format!("Bookmark {bm_id} not found"),
                }));
            }
        };
        if session.station_id.is_some() || session.structure_id.is_some() {
            return Some(Err(BridgeRpcError {
                code: "NotInSpace".into(),
                message: "Cannot warp while docked in station or structure".into(),
            }));
        }
        let ship_id = session
            .ship_id
            .or_else(|| {
                ctx.db
                    .with_snapshot(|s| s.characters.get(&char_id).map(|c| c.active_ship_id))
            })
            .unwrap_or(0);
        if let Err(e) = crate::services::tactical_modules::check_ship_navigation_lock(ctx, ship_id, "warp") {
            return Some(Err(e));
        }
        let dest = glam::DVec3::new(bm.x, bm.y, bm.z);
        let handle = ctx.systems.get_or_spawn(sys_id);
        if handle.is_ball_scrambled(ship_id).await.unwrap_or(false) {
            return Some(Err(BridgeRpcError {
                code: "WarpScrambled".into(),
                message: "Cannot warp while warp scrambled".into(),
            }));
        }
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
        if ((60_000_000..=69_999_999).contains(&bound_loc_id)
            || bound_loc_id >= 1_000_000_000_000)
            && session.station_id != Some(bound_loc_id)
            && session.structure_id != Some(bound_loc_id)
        {
            return Some(Err(BridgeRpcError {
                code: "BoundObjectOwnershipViolation".into(),
                message: format!(
                    "Character {char_id} is not docked at bound inventory location {bound_loc_id}"
                ),
            }));
        }
        let corp_id = session
            .corporation_id
            .or_else(|| {
                ctx.db
                    .with_snapshot(|snap| snap.characters.get(&char_id).map(|c| c.corporation_id))
            })
            .unwrap_or(0);
        let (is_ceo, roles) = super::inventory_fitting::get_effective_corp_roles_at_location(
            ctx,
            corp_id,
            char_id,
            Some(bound_loc_id),
        );
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
                    Some((fv, it.location_id))
                } else {
                    None
                }
            })
        });
        if let Some((flag_val, src_loc)) = src_flag_opt {
            let (is_ceo, roles) = super::inventory_fitting::get_effective_corp_roles_at_location(
                ctx,
                corp_id,
                char_id,
                Some(src_loc),
            );
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
            let caller_corp = session
                .corporation_id
                .filter(|&id| id > 0)
                .or_else(|| {
                    ctx.db
                        .with_snapshot(|s| s.characters.get(&char_id).map(|c| c.corporation_id))
                })
                .unwrap_or(0);
            let target_corp_id = args
                .first()
                .and_then(|a| a.as_i64())
                .unwrap_or(caller_corp);
            if caller_corp <= 0 || target_corp_id <= 0 || target_corp_id != caller_corp {
                return Some(Err(BridgeRpcError::user_error("CrpAccessDenied")));
            }
            let rows = ctx.db.with_snapshot(|snap| {
                let corp_id = target_corp_id;
                let mut member_ids: Vec<i64> = if let Some(corp) = snap.corporations.get(&corp_id) {
                    corp.members.iter().copied().collect()
                } else {
                    snap.characters
                        .values()
                        .filter(|c| c.corporation_id == corp_id)
                        .map(|c| c.character_id)
                        .collect()
                };
                if member_ids.is_empty() {
                    member_ids.push(char_id);
                }
                member_ids.sort_unstable();
                member_ids.dedup();
                member_ids
                    .into_iter()
                    .map(|mid| {
                        let ch = snap.characters.get(&mid);
                        let name = ch
                            .map(|c| c.name.clone())
                            .unwrap_or_else(|| format!("Pilot {mid}"));
                        // V24 `const.bloodlineTypes` mapping verified against SDE character group (group_id == 1)
                        let mapped_tid: u32 = match ch.map(|c| c.bloodline_id).unwrap_or(1) {
                            1 => 1373,
                            2 => 1374,
                            3 => 1375,
                            4 => 1376,
                            5 => 1377,
                            6 => 1378,
                            7 => 1379,
                            8 => 1380,
                            11 => 1383,
                            12 => 1384,
                            13 => 1385,
                            14 => 1386,
                            19 => 34574,
                            _ => 1373,
                        };
                        let type_id = ctx
                            .sde
                            .types
                            .get(&mapped_tid)
                            .filter(|t| t.group_id == 1)
                            .map(|t| t.type_id as i64)
                            .unwrap_or(1373);
                        let gender = ch.map(|c| c.gender as i64).unwrap_or(1);
                        vec![
                            PyRep::Integer(mid),
                            PyRep::String(name),
                            PyRep::Integer(type_id),
                            PyRep::Integer(gender),
                        ]
                    })
                    .collect()
            });

            let columns = vec![
                "ownerID".into(),
                "ownerName".into(),
                "typeID".into(),
                "gender".into(),
            ];
            Some(Ok(PyRep::Rowset { columns, rows }))
        }

        "GetRecentKills" => {
            let corp_id = session.corporation_id.unwrap_or(0);
            let columns = vec![
                "killID".into(),
                "victimCharacterID".into(),
                "victimCorporationID".into(),
                "victimShipTypeID".into(),
                "solarSystemID".into(),
                "killTime".into(),
            ];
            let rows = ctx.db.with_snapshot(|snap| {
                snap.killmails
                    .values()
                    .filter(|km| km.attacker_corp_id == corp_id)
                    .map(|km| vec![
                        PyRep::Integer(km.kill_id),
                        PyRep::Integer(km.victim_character_id),
                        PyRep::Integer(km.victim_corp_id),
                        PyRep::Integer(km.ship_type_id as i64),
                        PyRep::Integer(km.solar_system_id as i64),
                        PyRep::Integer(km.killed_unix as i64),
                    ])
                    .collect()
            });
            Some(Ok(PyRep::Rowset { columns, rows }))
        }

        "GetRecentLosses" => {
            let corp_id = session.corporation_id.unwrap_or(0);
            let columns = vec![
                "killID".into(),
                "victimCharacterID".into(),
                "victimCorporationID".into(),
                "victimShipTypeID".into(),
                "solarSystemID".into(),
                "killTime".into(),
            ];
            let rows = ctx.db.with_snapshot(|snap| {
                snap.killmails
                    .values()
                    .filter(|km| km.victim_corp_id == corp_id)
                    .map(|km| vec![
                        PyRep::Integer(km.kill_id),
                        PyRep::Integer(km.victim_character_id),
                        PyRep::Integer(km.victim_corp_id),
                        PyRep::Integer(km.ship_type_id as i64),
                        PyRep::Integer(km.solar_system_id as i64),
                        PyRep::Integer(km.killed_unix as i64),
                    ])
                    .collect()
            });
            Some(Ok(PyRep::Rowset { columns, rows }))
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
            let mut candidates: Vec<String> = Vec::new();
            if !words.is_empty() {
                let acronym: String = words
                    .iter()
                    .filter_map(|w| w.chars().find(|c| c.is_alphanumeric()))
                    .take(5)
                    .collect::<String>()
                    .to_uppercase();
                if !acronym.is_empty() {
                    candidates.push(acronym);
                }
            }
            let clean: String = name
                .chars()
                .filter(|c| c.is_alphanumeric())
                .collect::<String>()
                .to_uppercase();
            for len in [3usize, 4, 5] {
                if clean.chars().count() >= len {
                    candidates.push(clean.chars().take(len).collect());
                }
            }
            let consonants: String = clean
                .chars()
                .filter(|c| !"AEIOU".contains(*c))
                .take(4)
                .collect();
            if consonants.len() >= 2 {
                candidates.push(consonants);
            }
            let base: String = candidates
                .first()
                .map(|s| s.chars().take(3).collect())
                .unwrap_or_else(|| "CRP".into());
            for suffix in ["1", "2", "3", "X", "Z", "A", "B"] {
                let mut v = base.clone();
                v.push_str(suffix);
                v.truncate(5);
                candidates.push(v);
            }
            let taken: std::collections::HashSet<String> = ctx.db.with_snapshot(|snap| {
                snap.corporations
                    .values()
                    .map(|c| c.ticker.to_uppercase())
                    .chain(
                        ctx.sde
                            .npc_corporations
                            .values()
                            .map(|n| n.ticker_name.to_uppercase()),
                    )
                    .collect()
            });
            let mut seen = std::collections::HashSet::new();
            let mut suggestions = Vec::new();
            for cand in candidates {
                let t: String = cand.chars().take(5).collect();
                if !t.is_empty() && !taken.contains(&t) && seen.insert(t.clone()) {
                    suggestions.push(PyRep::keyval([("tickerName", PyRep::String(t))]));
                }
            }
            Some(Ok(PyRep::List(suggestions)))
        }

        "SetAccountKey" => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let now_unix = ctx.now_unix();
            if let Some(active_timer) = crate::services::space_destiny::check_session_change_timer_active(ctx, char_id, now_unix, None) {
                return Some(Err(BridgeRpcError {
                    code: "SessionChangeInProgress".into(),
                    message: format!("Cannot change account key while 10s session-change timer ({}) is active", active_timer.reason),
                }));
            }
            let key = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            if !(1000..=1006).contains(&key) {
                return Some(Err(BridgeRpcError {
                    code: "InvalidAccountKey".into(),
                    message: format!("Account key {key} must be between 1000 and 1006"),
                }));
            }
            let corp_id = session.corporation_id.unwrap_or(0);
            crate::services::fleet_social::fleet_social_corps::promote_due_pending_corp_grants(ctx, corp_id);
            let div_idx = (key - 1000) as u32;
            let can_use = ctx.db.with_snapshot(|snap| {
                let Some(corp) = snap.corporations.get(&corp_id) else {
                    return key == 1000;
                };
                if corp.ceo_id == char_id {
                    return true;
                }
                let role_mask = corp.member_roles.get(&char_id).copied().unwrap_or(0)
                    | snap.corp_role_assignments.get(&(corp_id, char_id)).map(|r| r.roles).unwrap_or(0)
                    | session.corp_role;
                let take_bit = 1u64 << (27 + div_idx);
                let query_bit = 1u64 << (20 + div_idx);
                role_mask == u64::MAX || (role_mask & (eve_db::corp_roles::ROLE_DIRECTOR | eve_db::corp_roles::ROLE_ACCOUNTANT | eve_db::corp_roles::ROLE_JUNIOR_ACCOUNTANT | eve_db::corp_roles::ROLE_TRADER | take_bit | query_bit)) != 0
            });
            if !can_use {
                return Some(Err(BridgeRpcError {
                    code: "CorpRoleDenied".into(),
                    message: format!("Character {char_id} lacks wallet division access for account key {key}"),
                }));
            }
            let old_key = session.corp_account_key as i64;
            session.corp_account_key = key as i32;
            crate::services::fleet_social::fleet_social_corps::with_corp_gov(ctx, |g| {
                g.entry(corp_id).or_default().member_account_keys.insert(char_id, key);
            });
            if old_key != key {
                ctx.notify(
                    crate::router::NotificationTarget::Character(char_id),
                    "OnSessionChanged",
                    "clientID",
                    vec![
                        PyRep::Bool(false),
                        PyRep::Dict(vec![(
                            PyRep::String("corpAccountKey".into()),
                            PyRep::Tuple(vec![PyRep::Integer(old_key), PyRep::Integer(key)]),
                        )]),
                    ],
                );
            }
            let sys_id = session.solar_system_id.unwrap_or(0);
            crate::services::space_destiny::record_session_change_timer(
                ctx,
                char_id,
                now_unix,
                sys_id,
                0,
                "set_account_key",
            );
            Some(Ok(PyRep::None))
        }

        "ExecuteActions" => {
            let first_is_corp = args
                .first()
                .and_then(|a| a.as_i64())
                .filter(|&id| id >= 98_000_000 || (1_000_000..2_000_000).contains(&id));
            let target_corp = first_is_corp.or(session.corporation_id);
            let (corp_id, char_id, has_director) =
                match crate::services::fleet_social::fleet_social_corps::require_corp_caller(
                    ctx,
                    session,
                    target_corp,
                ) {
                    Ok(v) => v,
                    Err(e) => return Some(Err(e)),
                };
            if !has_director {
                return Some(Err(BridgeRpcError {
                    code: "CorpRoleDenied".into(),
                    message: "CEO or Director role required to execute corporation member actions".into(),
                }));
            }

            let is_v24_member_batch = first_is_corp.is_none()
                && args.len() >= 2
                && matches!(
                    args.first(),
                    Some(PyRep::List(l)) | Some(PyRep::Tuple(l))
                        if !l.is_empty() && l.iter().all(|x| x.as_i64().is_some())
                );

            let now_unix = ctx.now_unix();
            if is_v24_member_batch {
                let target_ids: Vec<i64> = match args.first() {
                    Some(PyRep::List(l)) | Some(PyRep::Tuple(l)) => {
                        l.iter().filter_map(|x| x.as_i64()).collect()
                    }
                    _ => Vec::new(),
                };
                let remote_actions: Vec<PyRep> = match args.get(1) {
                    Some(PyRep::List(l)) | Some(PyRep::Tuple(l)) => l.clone(),
                    Some(single) => vec![single.clone()],
                    None => Vec::new(),
                };
                let res = ctx.db.try_with_mut_snapshot(|snap| {
                    let Some(corp) = snap.corporations.get(&corp_id) else {
                        return Err(BridgeRpcError {
                            code: "CorporationNotFound".into(),
                            message: format!("Corporation {corp_id} not found"),
                        });
                    };
                    let is_ceo = corp.ceo_id == char_id;
                    for &tid in &target_ids {
                        if !corp.members.contains(&tid) {
                            return Err(BridgeRpcError {
                                code: "NotCorpMember".into(),
                                message: format!(
                                    "Character {tid} is not a member of corporation {corp_id}"
                                ),
                            });
                        }
                    }
                    const HANGAR_DIV_MASK: u64 = ((1u64 << 14) - 1) << 13;
                    let dec = |m: u64| -> Vec<String> {
                        crate::esi::corp_governance_routes::decode_corp_roles(m).into_iter().map(String::from).collect()
                    };
                    let split_hangar = |old_m: u64, prev_p: u64, new_m: u64, touched: bool| -> (u64, u64, u64) {
                        let (held, desired) = (old_m & HANGAR_DIV_MASK, new_m & HANGAR_DIV_MASK);
                        let newly = desired & !held & !prev_p;
                        let pend = if touched { newly | (desired & prev_p) } else { prev_p };
                        let eff = if touched { (new_m & !HANGAR_DIV_MASK) | (desired & held) } else { old_m };
                        (eff, pend, newly)
                    };
                    let mut changed_members: Vec<(i64, u64, u64, u64)> = Vec::new();
                    for &tid in &target_ids {
                        let old_role = snap.corporations.get(&corp_id).and_then(|c| c.member_roles.get(&tid).copied()).unwrap_or(0);
                        let (prev_pending, prev_scoped) = snap.corp_governance.get(&corp_id).map(|g| {
                            (g.pending_role_grants.get(&tid).copied(), g.pending_scoped_role_grants.get(&tid).copied())
                        }).unwrap_or((None, None));
                        let prev_pending_hangar = prev_pending.map_or(0, |(pm, _)| pm & HANGAR_DIV_MASK);
                        let (prev_hq_p, prev_base_p, prev_other_p) = prev_scoped.map_or((0, 0, 0), |(h, b, o, _)| (h & HANGAR_DIV_MASK, b & HANGAR_DIV_MASK, o & HANGAR_DIV_MASK));
                        let mut role_rec = snap.corp_role_assignments.get(&(corp_id, tid)).cloned().unwrap_or(eve_db::snapshot_ext::CorpMemberRoleAssignment {
                            character_id: tid, roles: old_role, ..Default::default()
                        });
                        let (old_hq, old_base, old_other) = (role_rec.roles_at_hq, role_rec.roles_at_base, role_rec.roles_at_other);
                        let (mut new_role, mut new_hq, mut new_base, mut new_other) = (
                            old_role | prev_pending_hangar, old_hq | prev_hq_p, old_base | prev_base_p, old_other | prev_other_p,
                        );
                        let (mut role_touched, mut hq_touched, mut base_touched, mut other_touched) = (false, false, false, false);
                        for act in &remote_actions {
                            let (PyRep::Tuple(t) | PyRep::List(t)) = act else {
                                return Err(BridgeRpcError::user_error("InvalidParameter"));
                            };
                            let verb_mode: u8 = match t.first() {
                                Some(PyRep::String(s)) => match s.as_str() {
                                    "add" | "grant" | "give" => 1,
                                    "remove" | "revoke" | "take" | "clear" => 2,
                                    "set" => 3,
                                    _ => return Err(BridgeRpcError::user_error("InvalidParameter")),
                                },
                                Some(PyRep::Integer(1 | 4)) => 1,
                                Some(PyRep::Integer(2 | 5)) => 2,
                                Some(PyRep::Integer(3)) => 3,
                                _ => return Err(BridgeRpcError::user_error("InvalidParameter")),
                            };
                            let prop = t.get(1).and_then(|x| x.as_str()).unwrap_or("");
                            let val_i64 = t.get(2).and_then(|x| x.as_i64().or_else(|| x.as_bool().map(|b| if b { 1 } else { 0 }))).unwrap_or(0);
                            if val_i64 < 0 {
                                return Err(BridgeRpcError::user_error("CrpAccessDenied"));
                            }
                            let val_u64 = val_i64 as u64;
                            let apply_mask = |cur: u64| -> u64 {
                                match verb_mode { 1 => cur | val_u64, 2 => cur & !val_u64, _ => val_u64 }
                            };
                            let check_director = |cur: u64, next: u64| -> Result<u64, BridgeRpcError> {
                                if !is_ceo && (((cur ^ next) & eve_db::corp_roles::ROLE_DIRECTOR) != 0 || (val_u64 & eve_db::corp_roles::ROLE_DIRECTOR) != 0) {
                                    return Err(BridgeRpcError::user_error("CrpAccessDenied"));
                                }
                                Ok(next)
                            };
                            match prop {
                                "roles" | "role" => {
                                    role_touched = true;
                                    new_role = check_director(old_role, apply_mask(new_role))?;
                                }
                                "grantableRoles" => {
                                    role_rec.grantable_roles = check_director(role_rec.grantable_roles, apply_mask(role_rec.grantable_roles))?;
                                }
                                "rolesAtHQ" => {
                                    hq_touched = true;
                                    new_hq = check_director(old_hq, apply_mask(new_hq))?;
                                }
                                "grantableRolesAtHQ" => {
                                    role_rec.grantable_roles_at_hq = check_director(role_rec.grantable_roles_at_hq, apply_mask(role_rec.grantable_roles_at_hq))?;
                                }
                                "rolesAtBase" => {
                                    base_touched = true;
                                    new_base = check_director(old_base, apply_mask(new_base))?;
                                }
                                "grantableRolesAtBase" => {
                                    role_rec.grantable_roles_at_base = check_director(role_rec.grantable_roles_at_base, apply_mask(role_rec.grantable_roles_at_base))?;
                                }
                                "rolesAtOther" => {
                                    other_touched = true;
                                    new_other = check_director(old_other, apply_mask(new_other))?;
                                }
                                "grantableRolesAtOther" => {
                                    role_rec.grantable_roles_at_other = check_director(role_rec.grantable_roles_at_other, apply_mask(role_rec.grantable_roles_at_other))?;
                                }
                                "titleMask" | "titles" | "title" => {
                                    let state = snap.corp_governance.entry(corp_id).or_default();
                                    let cur_t = state.member_titles.get(&tid).copied().unwrap_or(0);
                                    state.member_titles.insert(tid, apply_mask(cur_t));
                                }
                                "accountKey" => {
                                    if (1000..=1006).contains(&val_i64) {
                                        snap.corp_governance.entry(corp_id).or_default().member_account_keys.insert(tid, val_i64);
                                    }
                                }
                                "baseID" => {
                                    let state = snap.corp_governance.entry(corp_id).or_default();
                                    if val_i64 > 0 { state.member_bases.insert(tid, val_i64); } else { state.member_bases.remove(&tid); }
                                }
                                "divisionID" => {
                                    snap.corp_governance.entry(corp_id).or_default().member_divisions.entry(tid).or_insert((0, 0)).0 = val_i64;
                                }
                                "squadronID" => {
                                    snap.corp_governance.entry(corp_id).or_default().member_divisions.entry(tid).or_insert((0, 0)).1 = val_i64;
                                }
                                _ => {}
                            }
                        }
                        let (effective_role, pending_hangar, newly_added_hangar) = split_hangar(old_role, prev_pending_hangar, new_role, role_touched);
                        let (eff_hq, pend_hq, newly_hq) = split_hangar(old_hq, prev_hq_p, new_hq, hq_touched);
                        let (eff_base, pend_base, newly_base) = split_hangar(old_base, prev_base_p, new_base, base_touched);
                        let (eff_other, pend_other, newly_other) = split_hangar(old_other, prev_other_p, new_other, other_touched);
                        role_rec.roles = effective_role;
                        role_rec.roles_at_hq = eff_hq;
                        role_rec.roles_at_base = eff_base;
                        role_rec.roles_at_other = eff_other;
                        if let Some(corp_mut) = snap.corporations.get_mut(&corp_id) {
                            corp_mut.member_roles.insert(tid, effective_role);
                        }
                        snap.corp_role_assignments.insert((corp_id, tid), role_rec);
                        snap.char_scoped_roles.insert(
                            tid,
                            eve_db::snapshot_ext::CharacterScopedRolesRecord {
                                roles: dec(effective_role),
                                roles_at_hq: dec(eff_hq),
                                roles_at_base: dec(eff_base),
                                roles_at_other: dec(eff_other),
                            },
                        );
                        let state = snap.corp_governance.entry(corp_id).or_default();
                        if role_touched {
                            if pending_hangar != 0 {
                                let ready_at = if newly_added_hangar != 0 { now_unix + 86_400 } else { prev_pending.map_or(now_unix + 86_400, |(_, eff)| eff) };
                                state.pending_role_grants.insert(tid, (effective_role | pending_hangar, ready_at));
                            } else {
                                state.pending_role_grants.remove(&tid);
                            }
                        }
                        if hq_touched || base_touched || other_touched {
                            if (pend_hq | pend_base | pend_other) != 0 {
                                let ready_at = if (newly_hq | newly_base | newly_other) != 0 { now_unix + 86_400 } else { prev_scoped.map_or(now_unix + 86_400, |(_, _, _, eff)| eff) };
                                state.pending_scoped_role_grants.insert(tid, (pend_hq, pend_base, pend_other, ready_at));
                            } else {
                                state.pending_scoped_role_grants.remove(&tid);
                            }
                        }
                        state.role_audit_log.push((char_id, tid, old_role, new_role, now_unix));
                        changed_members.push((tid, old_role, new_role, effective_role));
                    }
                    Ok(changed_members)
                });
                return match res {
                    Ok(changed_members) => {
                        for (tid, old_role, new_role, effective_role) in changed_members {
                            if session.character_id == Some(tid) {
                                session.corp_role = effective_role;
                            }
                            let change = PyRep::Dict(vec![(
                                PyRep::String("roles".into()),
                                PyRep::Tuple(vec![PyRep::Integer(old_role as i64), PyRep::Integer(new_role as i64)]),
                            )]);
                            ctx.notify(
                                crate::router::NotificationTarget::Corporation(corp_id),
                                "OnCorporationMemberChanged",
                                "corpid",
                                vec![PyRep::Integer(corp_id), PyRep::Integer(tid), change],
                            );
                        }
                        Some(Ok(PyRep::Bool(true)))
                    }
                    Err(e) => Some(Err(e)),
                };
            }

            let action_items = if first_is_corp.is_some() {
                match args.get(1) {
                    Some(PyRep::List(l)) | Some(PyRep::Tuple(l)) => l.clone(),
                    Some(single) => vec![single.clone()],
                    None => Vec::new(),
                }
            } else {
                match args.first() {
                    Some(PyRep::List(l)) | Some(PyRep::Tuple(l)) => l.clone(),
                    Some(single) => vec![single.clone()],
                    None => Vec::new(),
                }
            };

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let members_set = snap.corporations.get(&corp_id).map(|c| c.members.clone()).unwrap_or_default();
                let state = snap.corp_governance.entry(corp_id).or_default();

                for act in &action_items {
                    match act {
                        PyRep::Tuple(t) | PyRep::List(t) => {
                            let name = t.first().and_then(|x| x.as_str()).unwrap_or("");
                            match name {
                                "SetAccountKey" | "setAccountKey" => {
                                    if let (Some(target), Some(key)) = (t.get(1).and_then(|x| x.as_i64()), t.get(2).and_then(|x| x.as_i64())) {
                                        if !members_set.contains(&target) { return Err(BridgeRpcError::user_error("CrpAccessDenied")); }
                                        if (1000..=1006).contains(&key) {
                                            state.member_account_keys.insert(target, key);
                                        }
                                    }
                                }
                                "SetTitle" | "setTitle" => {
                                    if let (Some(target), Some(mask)) = (t.get(1).and_then(|x| x.as_i64()), t.get(2).and_then(|x| x.as_i64())) {
                                        if mask < 0 || !members_set.contains(&target) {
                                            return Err(BridgeRpcError::user_error("CrpAccessDenied"));
                                        }
                                        state.member_titles.insert(target, mask as u64);
                                    }
                                }
                                "SetNote" | "setNote" => {
                                    if let (Some(target), Some(note)) = (t.get(1).and_then(|x| x.as_i64()), t.get(2).and_then(|x| x.as_str())) {
                                        if !members_set.contains(&target) { return Err(BridgeRpcError::user_error("CrpAccessDenied")); }
                                        state.member_notes.insert(target, note.to_string());
                                    }
                                }
                                "SetFriendlyFire" | "setFriendlyFire" => {
                                    let flag = match t.get(1) {
                                        Some(PyRep::Bool(b)) => Some(*b),
                                        Some(PyRep::Integer(i)) => Some(*i != 0),
                                        _ => None,
                                    };
                                    if let Some(flag) = flag {
                                        state.pending_friendly_fire = Some((flag, now_unix + 86_400));
                                    }
                                }
                                other => {
                                    state.action_log.push((other.to_string(), char_id, now_unix));
                                }
                            }
                        }
                        PyRep::String(s) => {
                            state.action_log.push((s.to_string(), char_id, now_unix));
                        }
                        _ => {}
                    }
                }
                Ok(PyRep::Bool(true))
            });
            Some(res)
        }

        "Bind" => {
            let object_id = args
                .first()
                .and_then(|a| a.as_i64())
                .unwrap_or_else(|| session.corporation_id.unwrap_or(0));
            match ctx.bind_object(session, "corpRegistry", object_id) {
                Ok(bound_id) => Some(Ok(PyRep::Integer(bound_id as i64))),
                Err(e) => Some(Err(e)),
            }
        }

        "UnBind" | "Unbind" => {
            let bound_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0) as u64;
            ctx.bound_objects.write().unwrap_or_else(|e| e.into_inner()).remove(&bound_id);
            Some(Ok(PyRep::None))
        }

        "SetSessionCheck" => match ServerContext::require_character(session) {
            Ok(_) => Some(Ok(PyRep::None)),
            Err(e) => Some(Err(e)),
        },

        "clear_memoized" => Some(Ok(PyRep::None)),

        "columns" => Some(Ok(PyRep::List(vec![
            PyRep::String("corporationID".into()),
            PyRep::String("corporationName".into()),
            PyRep::String("ticker".into()),
        ]))),

        "header" => Some(Ok(PyRep::Tuple(vec![
            PyRep::String("corporationID".into()),
            PyRep::String("corporationName".into()),
            PyRep::String("ticker".into()),
        ]))),

        "itervalues" => Some(Ok(PyRep::List(Vec::new()))),

        _ => None,
    }
}

pub(crate) fn handle_get_corp_market_info(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (_, caller_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
    let corp_id = args.first().and_then(|a| a.as_i64()).or(session.corporation_id).unwrap_or(0);
    crate::services::fleet_social::fleet_social_corps::promote_due_pending_corp_grants(ctx, corp_id);
    let rows_res: Result<Vec<Vec<PyRep>>, BridgeRpcError> = ctx.db.with_snapshot(|snap| {
        let allowed = eve_db::corp_roles::ROLE_DIRECTOR
            | eve_db::corp_roles::ROLE_ACCOUNTANT
            | eve_db::corp_roles::ROLE_JUNIOR_ACCOUNTANT
            | eve_db::corp_roles::ROLE_TRADER;
        if let Some(corp) = snap.corporations.get(&corp_id) {
            let caller_role = corp.member_roles.get(&caller_id).copied().unwrap_or(0);
            if !corp.members.contains(&caller_id)
                || (caller_id != corp.ceo_id && caller_role != u64::MAX && (caller_role & allowed) == 0)
            {
                return Err(BridgeRpcError::user_error("CrpAccessDenied"));
            }
        } else {
            let caller_corp = session
                .corporation_id
                .or_else(|| snap.characters.get(&caller_id).map(|c| c.corporation_id))
                .unwrap_or(0);
            if corp_id <= 0 || corp_id != caller_corp || (session.corp_role & allowed) == 0 {
                return Err(BridgeRpcError::user_error("CrpAccessDenied"));
            }
        }
        #[derive(Default)]
        struct TypeMktSummary {
            sell_price: Option<f64>, sell_vol: i64, sell_date: i64, sell_station: i64,
            buy_price: Option<f64>, buy_vol: i64, buy_date: i64, buy_station: i64,
        }
        let mut by_type: std::collections::BTreeMap<u32, TypeMktSummary> = std::collections::BTreeMap::new();
        for o in snap.market_orders.values() {
            if o.vol_remaining <= 0 { continue; }
            let meta = snap.market_order_meta.get(&o.order_id);
            let is_corp = o.owner_id == corp_id || (meta.is_some_and(|m| m.is_corporation) && snap.characters.get(&o.owner_id).is_some_and(|c| c.corporation_id == corp_id));
            if !is_corp { continue; }
            let entry = by_type.entry(o.type_id).or_default();
            let price_isk = o.price.to_f64_isk();
            let issued_ts = meta.map(|m| m.issued_unix as i64).unwrap_or(0);
            if o.is_bid {
                entry.buy_vol += o.vol_remaining as i64;
                if entry.buy_price.is_none_or(|bp| price_isk > bp) {
                    entry.buy_price = Some(price_isk); entry.buy_date = issued_ts; entry.buy_station = o.station_id;
                }
            } else {
                entry.sell_vol += o.vol_remaining as i64;
                if entry.sell_price.is_none_or(|sp| price_isk < sp) {
                    entry.sell_price = Some(price_isk); entry.sell_date = issued_ts; entry.sell_station = o.station_id;
                }
            }
        }
        Ok(by_type.into_iter().map(|(tid, s)| vec![
            PyRep::Integer(tid as i64),
            s.sell_price.map(PyRep::Real).unwrap_or(PyRep::None), PyRep::Integer(s.sell_vol),
            s.sell_price.map(|_| PyRep::Integer(s.sell_date)).unwrap_or(PyRep::None),
            s.sell_price.map(|_| PyRep::Integer(s.sell_station)).unwrap_or(PyRep::None),
            s.buy_price.map(PyRep::Real).unwrap_or(PyRep::None), PyRep::Integer(s.buy_vol),
            s.buy_price.map(|_| PyRep::Integer(s.buy_date)).unwrap_or(PyRep::None),
            s.buy_price.map(|_| PyRep::Integer(s.buy_station)).unwrap_or(PyRep::None),
            PyRep::Integer(s.sell_vol), PyRep::Integer(s.buy_vol),
        ]).collect())
    });
    match rows_res {
        Ok(rows) => Some(Ok(PyRep::rowset(
            vec!["typeID", "sellPrice", "sellVolume", "sellDate", "sellStationID", "buyPrice", "buyVolume", "buyDate", "buyStationID", "sellQuantity", "buyQuantity"],
            rows,
        ))),
        Err(e) => Some(Err(e)),
    }
}

pub(crate) fn handle_get_info_window_data_for_char(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (_, caller_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
    let arg_id = args.first().and_then(|a| a.as_i64()).unwrap_or(caller_id);
    let res = ctx.db.with_snapshot(|snap| {
        let (target_char_id, corp_id_override) = if arg_id >= 98_000_000 && snap.corporations.contains_key(&arg_id) {
            (caller_id, Some(arg_id))
        } else {
            (arg_id, None)
        };
        let Some(ch) = snap.characters.get(&target_char_id) else {
            return Err(BridgeRpcError { code: "CharacterNotFound".into(), message: format!("Character {target_char_id} not found") });
        };
        let corp_id = corp_id_override.unwrap_or(ch.corporation_id);
        let is_npc_corp = corp_id < 98_000_000 || ctx.sde.npc_corporations.contains_key(&corp_id);
        let alliance_id = if is_npc_corp { None } else { snap.corporations.get(&corp_id).and_then(|c| c.alliance_id).or(ch.alliance_id) };
        let ticker = snap
            .corporations
            .get(&corp_id)
            .map(|c| c.ticker.clone())
            .or_else(|| ctx.sde.npc_corporations.get(&corp_id).map(|c| c.ticker_name.clone()))
            .unwrap_or_else(|| "CORP".into());
        let gov = snap.corp_governance.get(&corp_id);
        let tmask = gov.and_then(|g| g.member_titles.get(&target_char_id).copied()).unwrap_or(0);
        let mut active_titles = Vec::new();
        let mut title_slots: Vec<(String, PyRep)> = Vec::new();
        for idx in 1..=16i64 {
            let has_slot = (tmask & (1u64 << (idx - 1))) != 0 || tmask == idx as u64;
            let tname = gov.and_then(|g| g.titles.get(&idx).map(|(n, _)| n.clone()))
                .or_else(|| snap.corp_titles.get(&corp_id).and_then(|l| l.iter().find(|t| t.title_id == idx).map(|t| t.name.clone())))
                .unwrap_or_default();
            if has_slot && !tname.is_empty() {
                active_titles.push(tname.clone());
                title_slots.push((format!("title{idx}"), PyRep::String(tname)));
            } else {
                title_slots.push((format!("title{idx}"), PyRep::String(String::new())));
            }
        }
        let mut entries: HashMap<String, PyRep> = HashMap::from([
            ("corpID".into(), PyRep::Integer(corp_id)),
            ("corporationID".into(), PyRep::Integer(corp_id)),
            ("ticker".into(), PyRep::String(ticker)),
            ("allianceID".into(), alliance_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
            ("title".into(), PyRep::String(active_titles.join(", "))),
        ]);
        for (k, v) in title_slots { entries.insert(k, v); }
        Ok(PyRep::KeyVal(entries))
    });
    Some(res)
}

pub(crate) fn handle_get_member(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    if args.first().and_then(|a| a.as_i64()).is_some_and(|id| id >= 98_000_000) {
        return None;
    }
    let (corp_id, viewer_id, _) =
        match crate::services::fleet_social::fleet_social_corps::require_corp_caller(
            ctx, session, None,
        ) {
            Ok(v) => v,
            Err(e) => return Some(Err(e)),
        };
    let target_char_id = args.first().and_then(|a| a.as_i64()).unwrap_or(viewer_id);
    crate::services::fleet_social::fleet_social_corps::promote_due_pending_corp_grants(ctx, corp_id);
    let now_unix = ctx.now_unix();
    let res = ctx.db.with_snapshot(|snap| {
        let Some(corp) = snap.corporations.get(&corp_id) else {
            return Err(BridgeRpcError {
                code: "CorporationNotFound".into(),
                message: format!("Corporation {corp_id} not found"),
            });
        };
        if !corp.members.contains(&target_char_id) {
            return Err(BridgeRpcError {
                code: "NotCorpMember".into(),
                message: format!("Character {target_char_id} is not a member of corporation {corp_id}"),
            });
        }
        let ch = snap.characters.get(&target_char_id);
        let name = ch.map(|c| c.name.clone()).unwrap_or_else(|| format!("Pilot {target_char_id}"));
        let gov = snap.corp_governance.get(&corp_id);
        let mut roles = corp.member_roles.get(&target_char_id).copied().unwrap_or(0);
        if let Some(g) = gov {
            if let Some(&(p_mask, eff)) = g.pending_role_grants.get(&target_char_id) {
                if now_unix >= eff {
                    roles |= p_mask;
                }
            }
        }
        let ra = snap.corp_role_assignments.get(&(corp_id, target_char_id)).cloned().unwrap_or_default();
        let tmask = gov.and_then(|g| g.member_titles.get(&target_char_id).copied()).unwrap_or(0);
        let title_str = gov
            .and_then(|g| {
                let mut names = Vec::new();
                for idx in 1..=16i64 {
                    if (tmask & (1u64 << (idx - 1))) != 0 || tmask == idx as u64 {
                        if let Some((n, _)) = g.titles.get(&idx) {
                            names.push(n.clone());
                        }
                    }
                }
                (!names.is_empty()).then(|| names.join(", "))
            })
            .unwrap_or_else(|| if target_char_id == corp.ceo_id { "CEO".into() } else { String::new() });
        let acct_key = gov.and_then(|g| g.member_account_keys.get(&target_char_id).copied()).unwrap_or(1000);
        let start_dt = snap
            .employment_history
            .get(&target_char_id)
            .and_then(|h| h.iter().rev().find(|e| e.0 == corp_id).map(|e| e.1))
            .or_else(|| ch.map(|c| c.create_date_unix))
            .unwrap_or(0);
        let base_id = gov
            .and_then(|g| g.member_bases.get(&target_char_id).copied())
            .or(corp.hq_station_id)
            .or_else(|| ch.map(|c| c.home_station_id));
        let (div_id, sq_id) = gov.and_then(|g| g.member_divisions.get(&target_char_id).copied()).unwrap_or((0, 0));
        Ok(PyRep::keyval([
            ("characterID", PyRep::Integer(target_char_id)),
            ("corporationID", PyRep::Integer(corp_id)),
            ("name", PyRep::String(name)),
            ("roles", PyRep::Integer(roles as i64)),
            ("role", PyRep::Integer(roles as i64)),
            ("grantableRoles", PyRep::Integer(ra.grantable_roles as i64)),
            ("rolesAtHQ", PyRep::Integer(ra.roles_at_hq as i64)),
            ("grantableRolesAtHQ", PyRep::Integer(ra.grantable_roles_at_hq as i64)),
            ("rolesAtBase", PyRep::Integer(ra.roles_at_base as i64)),
            ("grantableRolesAtBase", PyRep::Integer(ra.grantable_roles_at_base as i64)),
            ("rolesAtOther", PyRep::Integer(ra.roles_at_other as i64)),
            ("grantableRolesAtOther", PyRep::Integer(ra.grantable_roles_at_other as i64)),
            ("titleMask", PyRep::Integer(tmask as i64)),
            ("title", PyRep::String(title_str)),
            ("baseID", base_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
            ("startDateTime", PyRep::Integer(start_dt as i64)),
            ("accountKey", PyRep::Integer(acct_key)),
            ("divisionID", PyRep::Integer(div_id)),
            ("squadronID", PyRep::Integer(sq_id)),
        ]))
    });
    Some(res)
}

pub(crate) fn handle_get_member_tracking_info(
    ctx: &ServerContext,
    session: &mut SessionState,
    method: &str,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    let target_cid = args
        .first()
        .and_then(|a| a.as_i64())
        .filter(|&id| id >= 98_000_000 || (1_000_000..2_000_000).contains(&id));
    let filter_char_id = args
        .first()
        .and_then(|a| a.as_i64())
        .filter(|&id| id > 0 && id < 98_000_000 && !(1_000_000..2_000_000).contains(&id));
    let (corp_id, _viewer_id, has_director) =
        match crate::services::fleet_social::fleet_social_corps::require_corp_caller(
            ctx, session, target_cid,
        ) {
            Ok(v) => v,
            Err(e) => return Some(Err(e)),
        };
    if method == "GetMemberTrackingInfo" && !has_director {
        return Some(Err(BridgeRpcError {
            code: "CorpRoleDenied".into(),
            message: "Director or Personnel Manager role required for full member tracking info".into(),
        }));
    }
    let now_unix = ctx.now_unix();
    crate::services::fleet_social::fleet_social_corps::promote_due_pending_corp_grants(ctx, corp_id);
    let caller_char = session.character_id;
    let rows = ctx.db.with_snapshot(|snap| {
        let Some(corp) = snap.corporations.get(&corp_id) else {
            return Vec::new();
        };
        let gov = snap.corp_governance.get(&corp_id);
        let mut mids: Vec<i64> = corp.members.iter().copied().collect();
        mids.sort_unstable();
        mids.into_iter()
            .filter_map(|mid| {
                if filter_char_id.is_some_and(|fid| fid != mid) {
                    return None;
                }
                let ch = snap.characters.get(&mid)?;
                let mut roles = corp.member_roles.get(&mid).copied().unwrap_or(0);
                if let Some(g) = gov {
                    if let Some(&(p_mask, eff)) = g.pending_role_grants.get(&mid) {
                        if now_unix >= eff {
                            roles |= p_mask;
                        }
                    }
                }
                let tmask = gov.and_then(|g| g.member_titles.get(&mid).copied()).unwrap_or(0);
                let title = if mid == corp.ceo_id { "CEO".to_string() } else { "Member".to_string() };
                let is_online = caller_char == Some(mid) || ctx.is_character_online(mid, ch.user_id);
                let stats = snap.character_login_stats.get(&mid);
                let logon_ts = stats
                    .map(|s| s.last_login_unix)
                    .filter(|&t| t > 0)
                    .or_else(|| is_online.then_some(now_unix))
                    .unwrap_or(0);
                let logoff_ts = stats.map(|s| s.last_logout_unix).filter(|&t| t > 0);
                let is_currently_online = is_online && logoff_ts.is_none_or(|off| logon_ts >= off);
                let loc_id = ch.station_id.or(ch.structure_id).unwrap_or(ch.solar_system_id as i64);
                let ship_type_id = snap.items.get(&ch.active_ship_id).map(|i| i.type_id as i64).unwrap_or(670);
                let (loc_rep, ship_rep) = if method == "GetMemberTrackingInfoSimple" && !has_director {
                    (PyRep::None, PyRep::None)
                } else {
                    (PyRep::Integer(loc_id), PyRep::Integer(ship_type_id))
                };
                let start_dt = snap
                    .employment_history
                    .get(&mid)
                    .and_then(|h| h.iter().rev().find(|e| e.0 == corp_id).map(|e| e.1))
                    .unwrap_or(ch.create_date_unix);
                let grantable = snap.corp_role_assignments.get(&(corp_id, mid)).map(|r| r.grantable_roles).unwrap_or(0);
                let base_id = gov.and_then(|g| g.member_bases.get(&mid).copied());
                Some(vec![
                    PyRep::Integer(mid),
                    PyRep::String(ch.name.clone()),
                    PyRep::Integer(roles as i64),
                    PyRep::Integer(roles as i64),
                    PyRep::String(title),
                    PyRep::Integer(tmask as i64),
                    PyRep::Integer(corp_id),
                    loc_rep,
                    ship_rep,
                    PyRep::Integer(logon_ts as i64),
                    logoff_ts.map(|t| PyRep::Integer(t as i64)).unwrap_or(PyRep::None),
                    PyRep::Integer(start_dt as i64),
                    base_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                    PyRep::Integer(grantable as i64),
                    PyRep::Bool(is_currently_online),
                ])
            })
            .collect()
    });
    Some(Ok(PyRep::rowset(
        vec![
            "characterID", "name", "role", "roles", "title", "titleMask", "corporationID",
            "locationID", "shipTypeID", "logonDateTime", "logoffDateTime", "startDateTime",
            "baseID", "grantableRoles", "online",
        ],
        rows,
    )))
}

