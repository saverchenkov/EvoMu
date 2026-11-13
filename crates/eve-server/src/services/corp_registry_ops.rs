//! `corp_registry_ops` — Supplemental handlers for `corpRegistry` machoNet RPCs.
//!
//! Provides handlers for corp kill reports, suggested tickers, moniker binding,
//! owner lookup, division account keys, and introspection methods.

#![forbid(unsafe_code)]

use std::collections::HashMap;

use crate::router::{BoundObjectEntry, ServerContext};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, DBRowDescriptor, PyRep};

#[path = "corp_registry_burst_fallback.rs"]
mod corp_registry_burst_fallback;

fn item_to_packed_cells(ctx: &ServerContext, i: &eve_db::InventoryItemRecord) -> Vec<PyRep> {
    let (group_id, category_id) = ctx.sde.types.get(&i.type_id).map(|t| (t.group_id as i64, t.category_id as i64)).unwrap_or((0, 0));
    let qty = if i.is_blueprint_copy { -2 } else if i.singleton { -1 } else { i.quantity as i64 };
    let custom_name = ctx.db.with_snapshot(|snap| snap.item_custom_names.get(&i.item_id).cloned()).unwrap_or_default();
    vec![
        PyRep::Integer(i.item_id), PyRep::Integer(i.type_id as i64), PyRep::Integer(i.owner_id),
        PyRep::Integer(i.location_id), PyRep::Integer(i.flag as i64), PyRep::Integer(qty),
        PyRep::Integer(group_id), PyRep::Integer(category_id), PyRep::String(custom_name),
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
    if let Some(res) =
        corp_registry_burst_fallback::handle_fleet_burst_fallback(ctx, session, service, method, args)
            .await
    {
        return Some(res);
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
                client_error: None,
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
                client_error: None,
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
                client_error: None,
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
                    client_error: None,
                }));
            }
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
                return Some(Err(BridgeRpcError::from_user_key(eve_proto::UserErrorKey::CrpAccessDenied)));
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
            Some(Ok(build_corp_combat_log_rowset(ctx, |km| {
                km.attacker_corp_id == corp_id
            })))
        }

        "GetRecentLosses" => {
            let corp_id = session.corporation_id.unwrap_or(0);
            Some(Ok(build_corp_combat_log_rowset(ctx, |km| {
                km.victim_corp_id == corp_id
            })))
        }

        "GetSuggestedAllianceShortNames" => {
            let name = args.first().and_then(|a| a.as_str()).unwrap_or("Alliance");
            let words: Vec<&str> = name.split_whitespace().collect();
            let mut suggestions = Vec::new();
            if words.len() >= 3 {
                let acronym: String = words.iter().filter_map(|w| w.chars().next()).take(5).collect();
                suggestions.push(PyRep::keyval([("shortName", PyRep::String(acronym.to_uppercase()))]));
            }
            let clean: String = name.chars().filter(|c| c.is_alphanumeric()).collect();
            if clean.len() >= 3 {
                let prefix: String = clean.chars().take(4).collect();
                suggestions.push(PyRep::keyval([("shortName", PyRep::String(prefix.to_uppercase()))]));
            }
            if suggestions.is_empty() {
                suggestions.push(PyRep::keyval([("shortName", PyRep::String("ALLI".into()))]));
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
            let key = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            if !(1000..=1006).contains(&key) {
                return Some(Err(BridgeRpcError {
                    code: "InvalidAccountKey".into(),
                    message: format!("Account key {key} must be between 1000 and 1006"),
                    client_error: None,
                }));
            }
            let corp_id = session.corporation_id.unwrap_or(0);
            crate::services::fleet_social::fleet_social_corps::promote_due_pending_corp_grants(ctx, corp_id);
            let div_idx = (key - 1000) as u32;
            let can_use = ctx.db.with_snapshot(|snap| {
                if !snap.corporations.contains_key(&corp_id) {
                    return key == 1000;
                }
                let (is_ceo, role_mask) = crate::services::social_corp_fleet::social_corp_projects::effective_corp_member_roles(
                    snap, corp_id, char_id, 0, now_unix,
                );
                let take_bit = 1u64 << (27 + div_idx);
                is_ceo || role_mask == u64::MAX || (role_mask & (eve_db::corp_roles::ROLE_DIRECTOR | eve_db::corp_roles::ROLE_ACCOUNTANT | eve_db::corp_roles::ROLE_JUNIOR_ACCOUNTANT | take_bit)) != 0
            });
            if !can_use {
                return Some(Err(BridgeRpcError { code: "CorpRoleDenied".into(), message: format!("Character {char_id} lacks wallet division access for account key {key}"), client_error: None }));
            }
            let old_key = session.corp_account_key as i64;
            if old_key == key {
                return Some(Ok(PyRep::None));
            }
            if let Some(active_timer) = crate::services::space_destiny::check_session_change_timer_active(ctx, char_id, now_unix, None) {
                return Some(Err(BridgeRpcError {
                    code: "SessionChangeInProgress".into(),
                    message: format!("Cannot change account key while 10s session-change timer ({}) is active", active_timer.reason),
                    client_error: None,
                }));
            }
            session.corp_account_key = key as i32;
            crate::services::fleet_social::fleet_social_corps::with_corp_gov(ctx, |g| {
                g.entry(corp_id).or_default().member_account_keys.insert(char_id, key);
            });
            ctx.notify(
                crate::router::NotificationTarget::Character(char_id),
                "OnSessionChanged",
                "clientID",
                vec![PyRep::Bool(false), PyRep::Dict(vec![(PyRep::String("corpAccountKey".into()), PyRep::Tuple(vec![PyRep::Integer(old_key), PyRep::Integer(key)]))])],
            );
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
                    client_error: None,
                }));
            }

            let is_v24_member_batch = first_is_corp.is_none()
                && args.len() >= 2
                && matches!(
                    args.first(),
                    Some(PyRep::List(l)) | Some(PyRep::Tuple(l))
                        if !l.is_empty() && l.iter().all(|x| x.as_i64().is_some())
                );

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
                let now_unix = ctx.now_unix();
                let res = ctx.db.try_with_mut_snapshot(|snap| {
                    let (is_ceo, caller_roles) = snap.effective_corp_roles_at(
                        corp_id,
                        char_id,
                        None,
                        session.corp_role,
                        now_unix,
                    );
                    let Some(corp) = snap.corporations.get(&corp_id) else {
                        return Err(BridgeRpcError {
                            code: "CorporationNotFound".into(),
                            message: format!("Corporation {corp_id} not found"),
                            client_error: None,
                        });
                    };
                    let caller_is_director =
                        is_ceo || (caller_roles & eve_db::corp_roles::ROLE_DIRECTOR) != 0;
                    for &tid in &target_ids {
                        if !corp.members.contains(&tid) {
                            return Err(BridgeRpcError {
                                code: "NotCorpMember".into(),
                                message: format!(
                                    "Character {tid} is not a member of corporation {corp_id}"
                                ),
                                client_error: None,
                            });
                        }
                        if !is_ceo && corp.ceo_id == tid {
                            return Err(BridgeRpcError::from_user_key(eve_proto::UserErrorKey::CrpAccessDenied));
                        }
                    }
                    let caller_ra = snap
                        .corp_role_assignments
                        .get(&(corp_id, char_id))
                        .cloned()
                        .unwrap_or_default();
                    let (
                        caller_grantable_global,
                        caller_grantable_hq,
                        caller_grantable_base,
                        caller_grantable_other,
                    ) = if caller_is_director {
                        (u64::MAX, u64::MAX, u64::MAX, u64::MAX)
                    } else {
                        (
                            caller_ra.grantable_roles,
                            caller_ra.grantable_roles_at_hq,
                            caller_ra.grantable_roles_at_base,
                            caller_ra.grantable_roles_at_other,
                        )
                    };
                    const HANGAR_DIV_MASK: u64 = eve_db::corp_roles::CORP_DELAYED_DIVISION_ROLE_MASK;
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
                    let corp_ally = snap.corporations.get(&corp_id).and_then(|c| c.alliance_id);
                    let mut changed_members: Vec<(i64, u64, u64, u64, (u64, u64, u64, u64, u64, u64))> =
                        Vec::new();
                    for &tid in &target_ids {
                        let old_role = snap.corporations.get(&corp_id).and_then(|c| c.member_roles.get(&tid).copied()).unwrap_or(0);
                        let (prev_pending, prev_scoped, prev_title_mask, old_title_roles) = snap.corp_governance.get(&corp_id).map(|g| {
                            let tm = g.member_titles.get(&tid).copied().unwrap_or(0);
                            let tr = g.titles.iter().fold(0u64, |acc, (&t_id, &(_, r))| {
                                let bit = if (1..=64).contains(&t_id) { 1u64 << (t_id - 1) } else { 0 };
                                if (tm & bit) != 0 { acc | r } else { acc }
                            });
                            (
                                g.pending_role_grants.get(&tid).copied(),
                                g.pending_scoped_role_grants.get(&tid).copied(),
                                tm,
                                tr,
                            )
                        }).unwrap_or((None, None, 0, 0));
                        let prev_pending_hangar = prev_pending.map_or(0, |(pm, _)| pm & HANGAR_DIV_MASK);
                        let (prev_hq_p, prev_base_p, prev_other_p) = prev_scoped.map_or((0, 0, 0), |(h, b, o, _)| (h & HANGAR_DIV_MASK, b & HANGAR_DIV_MASK, o & HANGAR_DIV_MASK));
                        let mut role_rec = snap.corp_role_assignments.get(&(corp_id, tid)).cloned().unwrap_or(eve_db::snapshot_ext::CorpMemberRoleAssignment {
                            character_id: tid, roles: old_role, ..Default::default()
                        });
                        let (old_hq, old_base, old_other) = (role_rec.roles_at_hq, role_rec.roles_at_base, role_rec.roles_at_other);
                        let old_any_roles = old_role
                            | prev_pending_hangar
                            | old_hq
                            | prev_hq_p
                            | old_base
                            | prev_base_p
                            | old_other
                            | prev_other_p
                            | role_rec.grantable_roles
                            | role_rec.grantable_roles_at_hq
                            | role_rec.grantable_roles_at_base
                            | role_rec.grantable_roles_at_other
                            | old_title_roles;
                        let mut latest_title_roles = old_title_roles;
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
                                return Err(BridgeRpcError::from_user_key(eve_proto::UserErrorKey::CrpAccessDenied));
                            }
                            let val_u64 = val_i64 as u64;
                            let apply_mask = |cur: u64| -> u64 {
                                match verb_mode { 1 => cur | val_u64, 2 => cur & !val_u64, _ => val_u64 }
                            };
                            if !caller_is_director
                                && !matches!(
                                    prop,
                                    "roles" | "role" | "rolesAtHQ" | "rolesAtBase" | "rolesAtOther"
                                )
                            {
                                return Err(BridgeRpcError::from_user_key(eve_proto::UserErrorKey::CrpAccessDenied));
                            }
                            let check_scoped = |cur: u64, next: u64, grantable: u64| -> Result<u64, BridgeRpcError> {
                                if !is_ceo && (((cur ^ next) & eve_db::corp_roles::ROLE_DIRECTOR) != 0 || (val_u64 & eve_db::corp_roles::ROLE_DIRECTOR) != 0) {
                                    return Err(BridgeRpcError::from_user_key(eve_proto::UserErrorKey::CrpAccessDenied));
                                }
                                if !caller_is_director && ((cur ^ next) & !grantable) != 0 {
                                    return Err(BridgeRpcError::from_user_key(eve_proto::UserErrorKey::CrpAccessDenied));
                                }
                                Ok(next)
                            };
                            match prop {
                                "roles" | "role" => {
                                    role_touched = true;
                                    new_role = check_scoped(new_role, apply_mask(new_role), caller_grantable_global)?;
                                }
                                "grantableRoles" => {
                                    role_rec.grantable_roles = check_scoped(role_rec.grantable_roles, apply_mask(role_rec.grantable_roles), caller_grantable_global)?;
                                }
                                "rolesAtHQ" => {
                                    hq_touched = true;
                                    new_hq = check_scoped(new_hq, apply_mask(new_hq), caller_grantable_hq)?;
                                }
                                "grantableRolesAtHQ" => {
                                    role_rec.grantable_roles_at_hq = check_scoped(role_rec.grantable_roles_at_hq, apply_mask(role_rec.grantable_roles_at_hq), caller_grantable_hq)?;
                                }
                                "rolesAtBase" => {
                                    base_touched = true;
                                    new_base = check_scoped(new_base, apply_mask(new_base), caller_grantable_base)?;
                                }
                                "grantableRolesAtBase" => {
                                    role_rec.grantable_roles_at_base = check_scoped(role_rec.grantable_roles_at_base, apply_mask(role_rec.grantable_roles_at_base), caller_grantable_base)?;
                                }
                                "rolesAtOther" => {
                                    other_touched = true;
                                    new_other = check_scoped(new_other, apply_mask(new_other), caller_grantable_other)?;
                                }
                                "grantableRolesAtOther" => {
                                    role_rec.grantable_roles_at_other = check_scoped(role_rec.grantable_roles_at_other, apply_mask(role_rec.grantable_roles_at_other), caller_grantable_other)?;
                                }
                                "titleMask" | "titles" | "title" => {
                                    let state = snap.corp_governance.entry(corp_id).or_default();
                                    let cur_t = state.member_titles.get(&tid).copied().unwrap_or(prev_title_mask);
                                    let next_t = apply_mask(cur_t);
                                    let prev_tp = state.pending_title_role_grants.get(&tid).filter(|&&(_, e)| now_unix < e).map_or(0, |&(b, _)| b & HANGAR_DIV_MASK);
                                    let (mut old_td, mut new_td, mut old_tr_all, mut new_tr_all) = (0u64, 0u64, 0u64, 0u64);
                                    for (&t_id, &(_, tr)) in &state.titles {
                                        let bit = eve_db::snapshot_ext::title_id_to_bit(t_id);
                                        if (cur_t & bit) != 0 {
                                            old_td |= tr & HANGAR_DIV_MASK;
                                            old_tr_all |= tr;
                                        }
                                        if (next_t & bit) != 0 {
                                            new_td |= tr & HANGAR_DIV_MASK;
                                            new_tr_all |= tr;
                                        }
                                    }
                                    if !is_ceo && ((old_tr_all ^ new_tr_all) & eve_db::corp_roles::ROLE_DIRECTOR) != 0 {
                                        return Err(BridgeRpcError::from_user_key(eve_proto::UserErrorKey::CrpAccessDenied));
                                    }
                                    let newly_td = new_td & !(old_td & !prev_tp) & !(old_role & HANGAR_DIV_MASK) & !prev_tp;
                                    let still_td = newly_td | (new_td & prev_tp);
                                    if still_td != 0 {
                                        let r_at = if newly_td != 0 { now_unix + 86_400 } else { state.pending_title_role_grants.get(&tid).map_or(now_unix + 86_400, |&(_, e)| e) };
                                        state.pending_title_role_grants.insert(tid, (still_td, r_at));
                                    } else {
                                        state.pending_title_role_grants.remove(&tid);
                                    }
                                    state.member_titles.insert(tid, next_t);
                                    latest_title_roles = new_tr_all;
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
                        let new_any_roles = effective_role
                            | pending_hangar
                            | eff_hq
                            | pend_hq
                            | eff_base
                            | pend_base
                            | eff_other
                            | pend_other
                            | role_rec.grantable_roles
                            | role_rec.grantable_roles_at_hq
                            | role_rec.grantable_roles_at_base
                            | role_rec.grantable_roles_at_other
                            | latest_title_roles;
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
                        state.role_audit_log.push((tid, char_id, old_role, new_role, now_unix));
                        if (old_any_roles & !new_any_roles) != 0 {
                            state.role_drop_timestamps.insert(tid, now_unix);
                        }
                        changed_members.push((
                            tid,
                            old_role,
                            new_role,
                            effective_role,
                            (old_hq, eff_hq, old_base, eff_base, old_other, eff_other),
                        ));
                    }
                    let released_controllers =
                        crate::services::structure_control_ops::release_unauthorized_structure_controllers_in_snap(snap);
                    Ok((corp_ally, changed_members, released_controllers))
                });
                return match res {
                    Ok((corp_ally, changed_members, released_controllers)) => {
                        crate::services::structure_control_ops::notify_released_structure_controllers(
                            ctx,
                            &released_controllers,
                        );
                        for (
                            tid,
                            old_role,
                            new_role,
                            effective_role,
                            (old_hq, eff_hq, old_base, eff_base, old_other, eff_other),
                        ) in changed_members
                        {
                            if session.character_id == Some(tid) {
                                session.corp_role = effective_role;
                                if let Some((_, _, _, restored_ship)) = released_controllers.iter().find(|r| r.1 == tid) {
                                    if *restored_ship > 0 {
                                        let _ = ctx.execute_session_transition_sync(
                                            Some(session),
                                            crate::router::SessionTransitionSpec::for_kind(
                                                tid,
                                                crate::router::TransitionKind::BoardShip { ship_id: *restored_ship },
                                                "structure_control_release",
                                            )
                                            .with_apply_db(false)
                                            .with_emit_on_session_changed(false),
                                        );
                                    }
                                }
                            } else {
                                crate::services::fleet_social::fleet_social_persistence::with_social_state(ctx, |s| {
                                    s.forced_session_corp_sync
                                        .insert(tid, (corp_id, corp_ally, effective_role));
                                });
                            }
                            crate::services::fleet_social::fleet_social_corp_tail::emit_corp_role_session_changed(
                                ctx,
                                corp_id,
                                tid,
                                old_role,
                                effective_role,
                                old_hq,
                                eff_hq,
                                old_base,
                                eff_base,
                                old_other,
                                eff_other,
                            );
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

            enum LegacyNotice {
                AccountKey { tid: i64, old: i64, new: i64 },
                Title { tid: i64, old_mask: u64, new_mask: u64, old_roles: u64, new_roles: u64 },
                Note { tid: i64, old: String, new: String },
            }
            let now_unix = ctx.now_unix();
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let (is_ceo, caller_roles) = snap.effective_corp_roles_at(
                    corp_id,
                    char_id,
                    None,
                    session.corp_role,
                    now_unix,
                );
                let (is_director_or_ceo, corp_ally, members_set, member_roles_map) = snap
                    .corporations
                    .get(&corp_id)
                    .map(|c| {
                        let dir = is_ceo || (caller_roles & eve_db::corp_roles::ROLE_DIRECTOR) != 0;
                        (dir, c.alliance_id, c.members.clone(), c.member_roles.clone())
                    })
                    .unwrap_or_default();
                if !is_director_or_ceo {
                    return Err(BridgeRpcError::from_user_key(eve_proto::UserErrorKey::CrpAccessDenied));
                }
                const HANGAR_DIV_MASK: u64 = eve_db::corp_roles::CORP_DELAYED_DIVISION_ROLE_MASK;
                let mut notices: Vec<LegacyNotice> = Vec::new();

                for act in &action_items {
                    match act {
                        PyRep::Tuple(t) | PyRep::List(t) => {
                            let name = t.first().and_then(|x| x.as_str()).unwrap_or("");
                            match name {
                                "SetAccountKey" | "setAccountKey" => {
                                    if let (Some(target), Some(key)) = (t.get(1).and_then(|x| x.as_i64()), t.get(2).and_then(|x| x.as_i64())) {
                                        if !members_set.contains(&target) { return Err(BridgeRpcError::from_user_key(eve_proto::UserErrorKey::CrpAccessDenied)); }
                                        if (1000..=1006).contains(&key) {
                                            let state = snap.corp_governance.entry(corp_id).or_default();
                                            let old = state.member_account_keys.get(&target).copied().unwrap_or(1000);
                                            state.member_account_keys.insert(target, key);
                                            if old != key {
                                                notices.push(LegacyNotice::AccountKey { tid: target, old, new: key });
                                            }
                                        }
                                    }
                                }
                                "SetTitle" | "setTitle" => {
                                    if let (Some(target), Some(mask)) = (t.get(1).and_then(|x| x.as_i64()), t.get(2).and_then(|x| x.as_i64())) {
                                        if mask < 0 || !members_set.contains(&target) {
                                            return Err(BridgeRpcError::from_user_key(eve_proto::UserErrorKey::CrpAccessDenied));
                                        }
                                        let (_, old_eff_roles) = snap.effective_corp_roles_at(
                                            corp_id, target, None, 0, now_unix,
                                        );
                                        let next_t = mask as u64;
                                        let cur_t = {
                                            let state = snap.corp_governance.entry(corp_id).or_default();
                                            let cur_t = state.member_titles.get(&target).copied().unwrap_or(0);
                                            let prev_tp = state
                                                .pending_title_role_grants
                                                .get(&target)
                                                .filter(|&&(_, e)| now_unix < e)
                                                .map_or(0, |&(b, _)| b & HANGAR_DIV_MASK);
                                            let (mut old_td, mut new_td, mut old_tr_all, mut new_tr_all) =
                                                (0u64, 0u64, 0u64, 0u64);
                                            for (&t_id, &(_, tr)) in &state.titles {
                                                let bit = eve_db::snapshot_ext::title_id_to_bit(t_id);
                                                if (cur_t & bit) != 0 {
                                                    old_td |= tr & HANGAR_DIV_MASK;
                                                    old_tr_all |= tr;
                                                }
                                                if (next_t & bit) != 0 {
                                                    new_td |= tr & HANGAR_DIV_MASK;
                                                    new_tr_all |= tr;
                                                }
                                            }
                                            if !is_ceo
                                                && ((old_tr_all ^ new_tr_all)
                                                    & eve_db::corp_roles::ROLE_DIRECTOR)
                                                    != 0
                                            {
                                                return Err(BridgeRpcError::from_user_key(eve_proto::UserErrorKey::CrpAccessDenied));
                                            }
                                            let old_direct = member_roles_map.get(&target).copied().unwrap_or(0);
                                            let newly_td = new_td
                                                & !(old_td & !prev_tp)
                                                & !(old_direct & HANGAR_DIV_MASK)
                                                & !prev_tp;
                                            let still_td = newly_td | (new_td & prev_tp);
                                            if still_td != 0 {
                                                let r_at = if newly_td != 0 {
                                                    now_unix + 86_400
                                                } else {
                                                    state
                                                        .pending_title_role_grants
                                                        .get(&target)
                                                        .map_or(now_unix + 86_400, |&(_, e)| e)
                                                };
                                                state
                                                    .pending_title_role_grants
                                                    .insert(target, (still_td, r_at));
                                            } else {
                                                state.pending_title_role_grants.remove(&target);
                                            }
                                            if (old_tr_all & !new_tr_all) != 0 {
                                                state.role_drop_timestamps.insert(target, now_unix);
                                            }
                                            state.member_titles.insert(target, next_t);
                                            cur_t
                                        };
                                        let (_, new_eff_roles) = snap.effective_corp_roles_at(
                                            corp_id, target, None, 0, now_unix,
                                        );
                                        if cur_t != next_t || old_eff_roles != new_eff_roles {
                                            notices.push(LegacyNotice::Title {
                                                tid: target,
                                                old_mask: cur_t,
                                                new_mask: next_t,
                                                old_roles: old_eff_roles,
                                                new_roles: new_eff_roles,
                                            });
                                        }
                                    }
                                }
                                "SetNote" | "setNote" => {
                                    if let (Some(target), Some(note)) = (t.get(1).and_then(|x| x.as_i64()), t.get(2).and_then(|x| x.as_str())) {
                                        if !members_set.contains(&target) { return Err(BridgeRpcError::from_user_key(eve_proto::UserErrorKey::CrpAccessDenied)); }
                                        let state = snap.corp_governance.entry(corp_id).or_default();
                                        let old = state.member_notes.get(&target).cloned().unwrap_or_default();
                                        state.member_notes.insert(target, note.to_string());
                                        if old != note {
                                            notices.push(LegacyNotice::Note { tid: target, old, new: note.to_string() });
                                        }
                                    }
                                }
                                "SetFriendlyFire" | "setFriendlyFire" => {
                                    let flag = match t.get(1) {
                                        Some(PyRep::Bool(b)) => Some(*b),
                                        Some(PyRep::Integer(i)) => Some(*i != 0),
                                        _ => None,
                                    };
                                    if let Some(flag) = flag {
                                        snap.corp_governance.entry(corp_id).or_default().pending_friendly_fire = Some((flag, now_unix + 86_400));
                                    }
                                }
                                other => {
                                    snap.corp_governance.entry(corp_id).or_default().action_log.push((other.to_string(), char_id, now_unix));
                                }
                            }
                        }
                        PyRep::String(s) => {
                            snap.corp_governance.entry(corp_id).or_default().action_log.push((s.to_string(), char_id, now_unix));
                        }
                        _ => {}
                    }
                }
                Ok((corp_ally, notices))
            });
            match res {
                Ok((corp_ally, notices)) => {
                    for notice in notices {
                        match notice {
                            LegacyNotice::AccountKey { tid, old, new } => {
                                if session.character_id == Some(tid) {
                                    session.corp_account_key = new as i32;
                                }
                                let change = PyRep::Dict(vec![(
                                    PyRep::String("accountKey".into()),
                                    PyRep::Tuple(vec![PyRep::Integer(old), PyRep::Integer(new)]),
                                )]);
                                ctx.notify(
                                    crate::router::NotificationTarget::Corporation(corp_id),
                                    "OnCorporationMemberChanged",
                                    "corpid",
                                    vec![PyRep::Integer(corp_id), PyRep::Integer(tid), change],
                                );
                            }
                            LegacyNotice::Title { tid, old_mask, new_mask, old_roles, new_roles } => {
                                if session.character_id == Some(tid) {
                                    session.corp_role = new_roles;
                                } else if old_roles != new_roles {
                                    crate::services::fleet_social::fleet_social_persistence::with_social_state(ctx, |s| {
                                        s.forced_session_corp_sync
                                            .insert(tid, (corp_id, corp_ally, new_roles));
                                    });
                                }
                                crate::services::fleet_social::fleet_social_corp_tail::emit_corp_role_session_changed(
                                    ctx, corp_id, tid, old_roles, new_roles,
                                    0, 0, 0, 0, 0, 0,
                                );
                                let mut fields = vec![(
                                    PyRep::String("titleMask".into()),
                                    PyRep::Tuple(vec![
                                        PyRep::Integer(old_mask as i64),
                                        PyRep::Integer(new_mask as i64),
                                    ]),
                                )];
                                if old_roles != new_roles {
                                    fields.push((
                                        PyRep::String("roles".into()),
                                        PyRep::Tuple(vec![
                                            PyRep::Integer(old_roles as i64),
                                            PyRep::Integer(new_roles as i64),
                                        ]),
                                    ));
                                }
                                ctx.notify(
                                    crate::router::NotificationTarget::Corporation(corp_id),
                                    "OnCorporationMemberChanged",
                                    "corpid",
                                    vec![PyRep::Integer(corp_id), PyRep::Integer(tid), PyRep::Dict(fields)],
                                );
                            }
                            LegacyNotice::Note { tid, old, new } => {
                                let change = PyRep::Dict(vec![(
                                    PyRep::String("description".into()),
                                    PyRep::Tuple(vec![PyRep::String(old), PyRep::String(new)]),
                                )]);
                                ctx.notify(
                                    crate::router::NotificationTarget::Corporation(corp_id),
                                    "OnCorporationMemberChanged",
                                    "corpid",
                                    vec![PyRep::Integer(corp_id), PyRep::Integer(tid), change],
                                );
                            }
                        }
                    }
                    Some(Ok(PyRep::Bool(true)))
                }
                Err(e) => Some(Err(e)),
            }
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
            if let Err(e) = ServerContext::require_auth(session) {
                return Some(Err(e));
            }
            let bound_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0) as u64;
            if bound_id > 0 {
                if let Err(e) = ctx.release_bound_object(session, bound_id) {
                    return Some(Err(e));
                }
            }
            Some(Ok(PyRep::None))
        }

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
    let now_unix = ctx.now_unix();
    let is_npc_corp =
        (1_000_000..2_000_000).contains(&corp_id) || ctx.sde.npc_corporations.contains_key(&corp_id);
    let rows_res: Result<Vec<Vec<PyRep>>, BridgeRpcError> = ctx.db.with_snapshot(|snap| {
        if !is_npc_corp {
            let allowed = eve_db::corp_roles::ROLE_DIRECTOR
                | eve_db::corp_roles::ROLE_ACCOUNTANT
                | eve_db::corp_roles::ROLE_JUNIOR_ACCOUNTANT
                | eve_db::corp_roles::ROLE_TRADER;
            if let Some(corp) = snap.corporations.get(&corp_id) {
                let (is_ceo, caller_role) =
                    snap.effective_corp_roles_at(corp_id, caller_id, None, session.corp_role, now_unix);
                if !corp.members.contains(&caller_id)
                    || (!is_ceo && caller_role != u64::MAX && (caller_role & allowed) == 0)
                {
                    return Err(BridgeRpcError::from_user_key(eve_proto::UserErrorKey::CrpAccessDenied));
                }
            } else {
                let caller_corp = session
                    .corporation_id
                    .or_else(|| snap.characters.get(&caller_id).map(|c| c.corporation_id))
                    .unwrap_or(0);
                if corp_id <= 0 || corp_id != caller_corp || (session.corp_role & allowed) == 0 {
                    return Err(BridgeRpcError::from_user_key(eve_proto::UserErrorKey::CrpAccessDenied));
                }
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
            let raw_ts = meta.map(|m| m.issued_unix as i64).unwrap_or(0);
            let issued_ts = if raw_ts > 0 && raw_ts < 10_000_000_000 {
                crate::sp6_lifecycle::unix_to_filetime(raw_ts as u64)
            } else if raw_ts <= 0 {
                crate::sp6_lifecycle::unix_to_filetime(now_unix)
            } else {
                raw_ts
            };
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
            return Err(BridgeRpcError { code: "CharacterNotFound".into(), message: format!("Character {target_char_id} not found"), client_error: None });
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
    if args
        .first()
        .and_then(|a| a.as_i64())
        .is_some_and(|id| id >= 98_000_000 || (1_000_000..2_000_000).contains(&id))
    {
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
    if !ctx.sde.npc_corporations.contains_key(&corp_id) {
        crate::services::fleet_social::fleet_social_corps::promote_due_pending_corp_grants(
            ctx, corp_id,
        );
    }
    let now_unix = ctx.now_unix();
    let res = ctx.db.with_snapshot(|snap| {
        if let Some(corp) = snap.corporations.get(&corp_id) {
            if !corp.members.contains(&target_char_id) {
                return Err(BridgeRpcError {
                    code: "NotCorpMember".into(),
                    message: format!("Character {target_char_id} is not a member of corporation {corp_id}"),
                    client_error: None,
                });
            }
            let ch = snap.characters.get(&target_char_id);
            let name = ch.map(|c| c.name.clone()).unwrap_or_else(|| format!("Pilot {target_char_id}"));
            let gov = snap.corp_governance.get(&corp_id);
            let (_, roles) = snap.effective_corp_roles_at(corp_id, target_char_id, None, 0, now_unix);
            let ra = snap.corp_role_assignments.get(&(corp_id, target_char_id)).cloned().unwrap_or_default();
            let tmask = gov.and_then(|g| g.member_titles.get(&target_char_id).copied()).unwrap_or(0);
            let title_str = gov
                .and_then(|g| {
                    let mut matched: Vec<(i64, String)> = g
                        .titles
                        .iter()
                        .filter_map(|(&tid, (n, _))| {
                            let bit = eve_db::snapshot_ext::title_id_to_bit(tid);
                            (!n.is_empty() && (tmask & bit) != 0).then(|| (tid, n.clone()))
                        })
                        .collect();
                    matched.sort_by_key(|(tid, _)| *tid);
                    let names: Vec<String> = matched.into_iter().map(|(_, n)| n).collect();
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
            return Ok(PyRep::keyval([
                ("characterID", PyRep::Integer(target_char_id)), ("corporationID", PyRep::Integer(corp_id)),
                ("name", PyRep::String(name)), ("roles", PyRep::Integer(roles as i64)), ("role", PyRep::Integer(roles as i64)),
                ("grantableRoles", PyRep::Integer(ra.grantable_roles as i64)), ("rolesAtHQ", PyRep::Integer(ra.roles_at_hq as i64)),
                ("grantableRolesAtHQ", PyRep::Integer(ra.grantable_roles_at_hq as i64)), ("rolesAtBase", PyRep::Integer(ra.roles_at_base as i64)),
                ("grantableRolesAtBase", PyRep::Integer(ra.grantable_roles_at_base as i64)), ("rolesAtOther", PyRep::Integer(ra.roles_at_other as i64)),
                ("grantableRolesAtOther", PyRep::Integer(ra.grantable_roles_at_other as i64)), ("titleMask", PyRep::Integer(tmask as i64)),
                ("title", PyRep::String(title_str)), ("baseID", base_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
                ("startDateTime", PyRep::Integer(start_dt as i64)), ("accountKey", PyRep::Integer(acct_key)),
                ("divisionID", PyRep::Integer(div_id)), ("squadronID", PyRep::Integer(sq_id)),
                ("blockRoles", PyRep::Integer(0)), ("blocksRoles", PyRep::Integer(0)),
            ]));
        }

        // DERIVED: Starter/NPC corporations live in `ctx.sde.npc_corporations` rather than `snap.corporations`.
        let Some(npc) = ctx.sde.npc_corporations.get(&corp_id) else {
            return Err(BridgeRpcError {
                code: "CorporationNotFound".into(),
                message: format!("Corporation {corp_id} not found"),
                client_error: None,
            });
        };
        let ch = snap.characters.get(&target_char_id);
        let is_npc_member = ch.is_some_and(|c| c.corporation_id == corp_id)
            || (ch.is_none() && target_char_id == viewer_id && session.corporation_id == Some(corp_id));
        if !is_npc_member {
            return Err(BridgeRpcError {
                code: "NotCorpMember".into(),
                message: format!("Character {target_char_id} is not a member of corporation {corp_id}"),
                client_error: None,
            });
        }
        let name = ch.map(|c| c.name.clone()).unwrap_or_else(|| format!("Pilot {target_char_id}"));
        let start_dt = snap
            .employment_history
            .get(&target_char_id)
            .and_then(|h| h.iter().rev().find(|e| e.0 == corp_id).map(|e| e.1))
            .or_else(|| ch.map(|c| c.create_date_unix))
            .unwrap_or(0);
        let base_id = ch.map(|c| c.home_station_id).or(npc.station_id);
        Ok(PyRep::keyval([
            ("characterID", PyRep::Integer(target_char_id)), ("corporationID", PyRep::Integer(corp_id)),
            ("name", PyRep::String(name)), ("roles", PyRep::Integer(0)), ("role", PyRep::Integer(0)),
            ("grantableRoles", PyRep::Integer(0)), ("rolesAtHQ", PyRep::Integer(0)),
            ("grantableRolesAtHQ", PyRep::Integer(0)), ("rolesAtBase", PyRep::Integer(0)),
            ("grantableRolesAtBase", PyRep::Integer(0)), ("rolesAtOther", PyRep::Integer(0)),
            ("grantableRolesAtOther", PyRep::Integer(0)), ("titleMask", PyRep::Integer(0)),
            ("title", PyRep::String(String::new())), ("baseID", base_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
            ("startDateTime", PyRep::Integer(start_dt as i64)), ("accountKey", PyRep::Integer(1000)),
            ("divisionID", PyRep::Integer(0)), ("squadronID", PyRep::Integer(0)),
            ("blockRoles", PyRep::Integer(0)), ("blocksRoles", PyRep::Integer(0)),
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
    let target_cid = args.first().and_then(|a| a.as_i64()).filter(|&id| id >= 98_000_000 || (1_000_000..2_000_000).contains(&id));
    let filter_char_id = args.first().and_then(|a| a.as_i64()).filter(|&id| id > 0 && id < 98_000_000 && !(1_000_000..2_000_000).contains(&id));
    let (corp_id, _viewer_id, has_director) = match crate::services::fleet_social::fleet_social_corps::require_corp_caller(ctx, session, target_cid) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    if method == "GetMemberTrackingInfo" && !has_director {
        return Some(Err(BridgeRpcError { code: "CorpRoleDenied".into(), message: "Director or Personnel Manager role required for full member tracking info".into(), client_error: None }));
    }
    let now_unix = ctx.now_unix();
    crate::services::fleet_social::fleet_social_corps::promote_due_pending_corp_grants(ctx, corp_id);
    let caller_char = session.character_id;
    let rows = ctx.db.with_snapshot(|snap| {
        let Some(corp) = snap.corporations.get(&corp_id) else { return Vec::new(); };
        let gov = snap.corp_governance.get(&corp_id);
        let mut mids: Vec<i64> = corp.members.iter().copied().collect();
        mids.sort_unstable();
        mids.into_iter()
            .filter_map(|mid| {
                if filter_char_id.is_some_and(|fid| fid != mid) { return None; }
                let ch = snap.characters.get(&mid)?;
                let (_, roles) = snap.effective_corp_roles_at(corp_id, mid, None, 0, now_unix);
                let tmask = gov.and_then(|g| g.member_titles.get(&mid).copied()).unwrap_or(0);
                let title = if mid == corp.ceo_id { "CEO".to_string() } else { "Member".to_string() };
                let is_online = caller_char == Some(mid) || ctx.is_character_online(mid, ch.user_id);
                let stats = snap.character_login_stats.get(&mid);
                let logon_ts = stats.map(|s| s.last_login_unix).filter(|&t| t > 0).or_else(|| is_online.then_some(now_unix)).unwrap_or(0);
                let logoff_ts = stats.map(|s| s.last_logout_unix).filter(|&t| t > 0);
                let is_currently_online = is_online && logoff_ts.is_none_or(|off| logon_ts >= off);
                let loc_id = ch.station_id.or(ch.structure_id).unwrap_or(ch.solar_system_id as i64);
                let ship_type_id = snap.items.get(&ch.active_ship_id).map(|i| i.type_id as i64).unwrap_or(670);
                let (loc_rep, ship_rep) = if method == "GetMemberTrackingInfoSimple" && !has_director {
                    (PyRep::None, PyRep::None)
                } else {
                    (PyRep::Integer(loc_id), PyRep::Integer(ship_type_id))
                };
                let start_dt = snap.employment_history.get(&mid).and_then(|h| h.iter().rev().find(|e| e.0 == corp_id).map(|e| e.1)).unwrap_or(ch.create_date_unix);
                let grantable = snap.corp_role_assignments.get(&(corp_id, mid)).map(|r| r.grantable_roles).unwrap_or(0);
                let base_id = gov.and_then(|g| g.member_bases.get(&mid).copied());
                let last_online_rep = if is_currently_online {
                    PyRep::Real(-1.0)
                } else if let Some(off) = logoff_ts {
                    PyRep::Real(((now_unix.saturating_sub(off)) as f64 / 3600.0).max(0.01))
                } else if logon_ts > 0 {
                    PyRep::Real(((now_unix.saturating_sub(logon_ts)) as f64 / 3600.0).max(0.01))
                } else {
                    PyRep::None
                };
                let logon_rep = if logon_ts > 0 {
                    PyRep::Integer(crate::sp6_lifecycle::unix_to_filetime(logon_ts))
                } else {
                    PyRep::None
                };
                let logoff_rep = logoff_ts
                    .map(|t| PyRep::Integer(crate::sp6_lifecycle::unix_to_filetime(t)))
                    .unwrap_or(PyRep::None);
                let start_rep = PyRep::Integer(crate::sp6_lifecycle::unix_to_filetime(start_dt));
                let faction_rep = ch.war_faction_id.map(PyRep::Integer).unwrap_or(PyRep::None);
                Some(vec![
                    PyRep::Integer(mid), PyRep::String(ch.name.clone()), PyRep::Integer(roles as i64), PyRep::Integer(roles as i64),
                    PyRep::String(title), PyRep::Integer(tmask as i64), PyRep::Integer(corp_id), loc_rep, ship_rep,
                    logon_rep, logoff_rep,
                    start_rep, base_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                    PyRep::Integer(grantable as i64), PyRep::Bool(is_currently_online),
                    last_online_rep, faction_rep,
                ])
            })
            .collect()
    });
    Some(Ok(PyRep::rowset(
        vec![
            "characterID", "name", "role", "roles", "title", "titleMask", "corporationID",
            "locationID", "shipTypeID", "logonDateTime", "logoffDateTime", "startDateTime",
            "baseID", "grantableRoles", "online", "lastOnline", "factionID",
        ],
        rows,
    )))
}

#[path = "corp_registry_ops_tests.rs"]
#[cfg(test)]
mod corp_registry_ops_tests;

#[cfg(test)]
#[path = "corp_registry_ops_edges2_tests.rs"]
mod corp_registry_ops_edges2_tests;

fn build_corp_combat_log_rowset(
    ctx: &ServerContext,
    filter: impl Fn(&eve_db::KillmailRecord) -> bool,
) -> PyRep {
    let rows = crate::services::character_lifecycle::resolve_combat_log_entries(ctx, filter)
        .into_iter()
        .map(|e| {
            vec![
                PyRep::Integer(e.kill_id),
                PyRep::Integer(e.victim_character_id),
                PyRep::Integer(e.victim_corp_id),
                PyRep::Integer(e.victim_ship_type_id as i64),
                PyRep::Integer(e.solar_system_id as i64),
                PyRep::Integer(e.killed_unix as i64),
                PyRep::None,
                PyRep::Integer(e.victim_character_id),
                e.victim_alliance_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                PyRep::None,
                PyRep::Integer(e.victim_damage_taken),
                PyRep::Integer(e.final_character_id),
                PyRep::Integer(e.final_corp_id),
                e.final_alliance_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                PyRep::None,
                e.final_ship_type_id
                    .map(|t| PyRep::Integer(t as i64))
                    .unwrap_or(PyRep::None),
                e.final_weapon_type_id
                    .map(|t| PyRep::Integer(t as i64))
                    .unwrap_or(PyRep::None),
                PyRep::Real(e.final_security_status),
                PyRep::Integer(e.final_damage_done),
                PyRep::String(String::new()),
            ]
        })
        .collect();
    PyRep::Rowset {
        columns: vec![
            "killID".into(),
            "victimCharacterID".into(),
            "victimCorporationID".into(),
            "victimShipTypeID".into(),
            "solarSystemID".into(),
            "killTime".into(),
            "moonID".into(),
            "victimID".into(),
            "victimAllianceID".into(),
            "victimFactionID".into(),
            "victimDamageTaken".into(),
            "finalCharacterID".into(),
            "finalCorporationID".into(),
            "finalAllianceID".into(),
            "finalFactionID".into(),
            "finalShipTypeID".into(),
            "finalWeaponTypeID".into(),
            "finalSecurityStatus".into(),
            "finalDamageDone".into(),
            "killBlob".into(),
        ],
        rows,
    }
}


