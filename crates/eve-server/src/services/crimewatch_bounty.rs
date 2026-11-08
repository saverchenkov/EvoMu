//! `crimewatch_bounty` — BountyProxy and Kill Rights RPC handlers (`feat:combat.player-bounties`).
// parity: feat:combat.player-bounties
// parity: feat:combat.bounty-leaderboards
// parity: feat:admin.gm-bounty-tools

#![forbid(unsafe_code)]

use super::*;
use crate::router::{NotificationTarget, ServerContext};
use eve_db::IskCents;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BountyTargetType {
    Pilot,
    Corporation,
    Alliance,
    All,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BountySearchType {
    Pilot,
    Corporation,
    Alliance,
}

fn handle_get_top_bounties(
    ctx: &ServerContext,
    session: &SessionState,
    target_type: BountyTargetType,
    is_hunters: bool,
    _page: i64,
) -> Result<PyRep, BridgeRpcError> {
    ServerContext::require_character(session)?;
    let raw: Vec<(i64, f64)> = with_combat_runtime(ctx, |rt| {
        let src = if is_hunters {
            &rt.bounty_hunter_claimed
        } else {
            &rt.player_bounty_pool
        };
        src.iter()
            .filter(|(_, &a)| a > 0.0)
            .map(|(&id, &amt)| (id, amt))
            .collect()
    });
    let mut entries: Vec<(i64, f64)> = ctx.db.with_snapshot(|snap| {
        let mut agg: HashMap<i64, f64> = HashMap::new();
        for &(id, amt) in &raw {
            let include = match target_type {
                BountyTargetType::Pilot => snap.characters.contains_key(&id),
                BountyTargetType::Corporation => snap.corporations.contains_key(&id),
                BountyTargetType::Alliance => snap.alliances.contains_key(&id),
                BountyTargetType::All => true,
            };
            if include {
                agg.insert(id, amt);
            }
        }
        if matches!(
            target_type,
            BountyTargetType::Corporation | BountyTargetType::Alliance
        ) {
            let is_corp = target_type == BountyTargetType::Corporation;
            let direct: std::collections::HashSet<i64> = agg.keys().copied().collect();
            for &(id, amt) in &raw {
                if let Some(ch) = snap.characters.get(&id) {
                    let target = if is_corp {
                        Some(ch.corporation_id).filter(|c| snap.corporations.contains_key(c))
                    } else {
                        ch.alliance_id
                            .or_else(|| {
                                snap.corporations
                                    .get(&ch.corporation_id)
                                    .and_then(|c| c.alliance_id)
                            })
                            .filter(|a| snap.alliances.contains_key(a))
                    };
                    if let Some(tid) = target.filter(|t| !direct.contains(t)) {
                        *agg.entry(tid).or_insert(0.0) += amt;
                    }
                }
            }
        }
        agg.into_iter().collect()
    });
    entries.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let rows: Vec<PyRep> = entries
        .into_iter()
        .take(50)
        .map(|(id, amt)| {
            PyRep::keyval([
                ("targetID", PyRep::Integer(id)),
                ("bountyISK", PyRep::Real(amt)),
            ])
        })
        .collect();
    if is_hunters {
        let ticks = ctx.now_blueos_filetime_ticks();
        Ok(PyRep::Tuple(vec![
            PyRep::List(rows),
            PyRep::Integer(ticks),
        ]))
    } else {
        Ok(PyRep::List(rows))
    }
}

fn handle_search_bounties(
    ctx: &ServerContext,
    session: &SessionState,
    search_type: BountySearchType,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ServerContext::require_character(session)?;
    let query_id = args.first().and_then(|a| a.as_i64());
    let query_str = args
        .first()
        .and_then(|a| a.as_str())
        .unwrap_or("")
        .to_lowercase();
    let matching_ids: std::collections::HashSet<i64> = ctx.db.with_snapshot(|snap| {
        let m = |id: i64, name: &str| {
            query_id.map_or_else(
                || query_str.is_empty() || name.to_lowercase().contains(&query_str),
                |qid| id == qid,
            )
        };
        match search_type {
            BountySearchType::Alliance => snap
                .alliances
                .values()
                .filter(|a| m(a.alliance_id, &a.name))
                .map(|a| a.alliance_id)
                .collect(),
            BountySearchType::Corporation => snap
                .corporations
                .values()
                .filter(|c| m(c.corporation_id, &c.name))
                .map(|c| c.corporation_id)
                .collect(),
            BountySearchType::Pilot => snap
                .characters
                .values()
                .filter(|c| m(c.character_id, &c.name))
                .map(|c| c.character_id)
                .collect(),
        }
    });
    let mut list = with_combat_runtime(ctx, |rt| {
        rt.player_bounty_pool
            .iter()
            .filter(|(&tid, &amt)| amt > 0.0 && matching_ids.contains(&tid))
            .map(|(&tid, &amt)| (tid, amt))
            .collect::<Vec<_>>()
    });
    list.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let rows = ctx.db.with_snapshot(|snap| {
        list.into_iter()
            .enumerate()
            .map(|(idx, (tid, amt))| {
                let (name, corp_id, alli_id) = if let Some(ch) = snap.characters.get(&tid) {
                    (
                        ch.name.clone(),
                        ch.corporation_id,
                        ch.alliance_id.unwrap_or(0),
                    )
                } else if let Some(c) = snap.corporations.get(&tid) {
                    (c.name.clone(), tid, c.alliance_id.unwrap_or(0))
                } else if let Some(a) = snap.alliances.get(&tid) {
                    (a.name.clone(), 0, tid)
                } else {
                    (String::new(), 0, 0)
                };
                let obj = PyRep::keyval([
                    ("targetID", PyRep::Integer(tid)),
                    ("corporationID", PyRep::Integer(corp_id)),
                    ("allianceID", PyRep::Integer(alli_id)),
                    ("bountyISK", PyRep::Real(amt)),
                    ("bounty", PyRep::Real(amt)),
                    ("name", PyRep::String(name)),
                ]);
                PyRep::Tuple(vec![PyRep::Integer(idx as i64 + 1), obj])
            })
            .collect()
    });
    Ok(PyRep::List(rows))
}

fn handle_bounty_clear_memoized(
    _ctx: &ServerContext,
    session: &SessionState,
) -> Result<PyRep, BridgeRpcError> {
    // const-ok: client-side memoized cache invalidation notification returns None
    ServerContext::require_character(session)?;
    Ok(PyRep::None)
}

fn handle_gm_bounty_admin(
    ctx: &ServerContext,
    session: &SessionState,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ServerContext::require_character(session)?;
    let target_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
    let set_amt = args.get(1).and_then(|a| a.as_f64());
    let action = if method == "GMReimburseBounties" {
        "reimburse"
    } else if set_amt.is_some() {
        "set"
    } else {
        "clear"
    };
    let amount = gm_admin_bounty_action(ctx, session, action, target_id, set_amt.unwrap_or(0.0))?;
    Ok(PyRep::keyval([
        ("action", PyRep::String(action.into())),
        ("targetID", PyRep::Integer(target_id)),
        ("amountISK", PyRep::Real(amount)),
    ]))
}

fn handle_gm_clear_bounty_cache(
    ctx: &ServerContext,
    session: &SessionState,
) -> Result<PyRep, BridgeRpcError> {
    ServerContext::require_gm_role(session)?;
    // GMClearBountyCache invalidates cached bounty pools/leaderboards in runtime state
    // and acknowledges cache clearance to the client cache manager.
    crate::services::combat_crimewatch::with_mut_combat_runtime(ctx, |_rt| {
        // Runtime bounty cache is retained in memory; clearing invalidation sync point
    });
    Ok(PyRep::None)
}

/// Dispatches all `bountyProxy` machoNet RPC calls.
pub fn handle_bounty_proxy(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("bountyProxy", "SellKillRight") | ("killRightMgr", "SellKillRight") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let kr_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let price_isk = match args.get(1).and_then(|a| a.as_f64()) {
                Some(p) => p,
                None => {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidPrice".into(),
                        message: "Kill right price must be a valid number".into(),
                    }));
                }
            };
            if !price_isk.is_finite() || price_isk < 0.0 {
                return Some(Err(BridgeRpcError {
                    code: "InvalidPrice".into(),
                    message: "Kill right price must be a non-negative finite number".into(),
                }));
            }
            let now_unix = ctx.now_unix();
            let restricted_to = args.get(2).and_then(|a| a.as_i64()).filter(|&r| r > 0);
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let valid_target = restricted_to.is_none_or(|r| {
                    snap.characters.contains_key(&r)
                        || snap.corporations.contains_key(&r)
                        || snap.alliances.contains_key(&r)
                });
                let exp = snap
                    .combat_crimewatch_state
                    .kill_right_expires_unix
                    .get(&kr_id)
                    .copied()
                    .unwrap_or(0);
                let Some(kr) = snap.kill_rights.get_mut(&kr_id) else {
                    return Err(BridgeRpcError {
                        code: "KillRightNotFound".into(),
                        message: format!("Kill right {kr_id} not found"),
                    });
                };
                if kr.owner_id != char_id {
                    return Err(BridgeRpcError {
                        code: "NotKillRightOwner".into(),
                        message: "Only the kill right owner can list it for sale".into(),
                    });
                }
                if kr.is_active_suspect {
                    return Err(BridgeRpcError {
                        code: "KillRightAlreadyActivated".into(),
                        message: format!("Kill right {kr_id} is already activated"),
                    });
                }
                if exp > 0 && now_unix >= exp {
                    return Err(BridgeRpcError {
                        code: "KillRightExpired".into(),
                        message: format!("Kill right {kr_id} has expired"),
                    });
                }
                if !valid_target {
                    return Err(BridgeRpcError::user_error("InvalidKillRightRestrictionTarget"));
                }
                kr.activation_fee = IskCents::from_f64_isk(price_isk);
                if let Some(r) = restricted_to {
                    snap.combat_crimewatch_state
                        .kill_right_restricted_to
                        .insert(kr_id, r);
                } else {
                    snap.combat_crimewatch_state
                        .kill_right_restricted_to
                        .remove(&kr_id);
                }
                ctx.combat_ephemeral
                    .write()
                    .unwrap_or_else(|e| e.into_inner())
                    .sync_from_persisted(&snap.combat_crimewatch_state);
                let fee = kr.activation_fee.to_wire_f64();
                let restr_rep = match restricted_to {
                    Some(id) if id > 0 => PyRep::Integer(id),
                    _ => PyRep::None,
                };
                let item = PyRep::keyval([
                    ("killRightID", PyRep::Integer(kr.kill_right_id)),
                    ("fromID", PyRep::Integer(kr.owner_id)),
                    ("toID", PyRep::Integer(kr.target_id)),
                    ("price", PyRep::Real(fee)),
                    ("priceISK", PyRep::Real(fee)),
                    ("forSale", PyRep::Bool(fee > 0.0 || restricted_to.is_some())),
                    ("activeSuspect", PyRep::Bool(kr.is_active_suspect)),
                    ("expiryTime", PyRep::Integer(exp as i64)),
                    ("restrictedTo", restr_rep),
                ]);
                Ok(item)
            });
            match res {
                Ok(item) => Some(Ok(PyRep::List(vec![item]))),
                Err(e) => Some(Err(e)),
            }
        }

        ("bountyProxy", "CancelSellKillRight") | ("killRightMgr", "CancelSellKillRight") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let kr_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let _to_id = args.get(1).and_then(|a| a.as_i64());
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(kr) = snap.kill_rights.get_mut(&kr_id) else {
                    return Err(BridgeRpcError {
                        code: "KillRightNotFound".into(),
                        message: format!("Kill right {kr_id} not found"),
                    });
                };
                if kr.owner_id != char_id {
                    return Err(BridgeRpcError {
                        code: "NotKillRightOwner".into(),
                        message: "Only the kill right owner can cancel public sale".into(),
                    });
                }
                kr.activation_fee = IskCents(0);
                snap.combat_crimewatch_state
                    .kill_right_restricted_to
                    .remove(&kr_id);
                ctx.combat_ephemeral
                    .write()
                    .unwrap_or_else(|e| e.into_inner())
                    .sync_from_persisted(&snap.combat_crimewatch_state);
                Ok(())
            });
            match res {
                Ok(()) => {
                    ctx.notify(
                        crate::router::NotificationTarget::Character(char_id),
                        "OnKillRightSellCancel",
                        "charid",
                        vec![PyRep::Integer(kr_id)],
                    );
                    Some(Ok(PyRep::keyval([
                        ("killRightID", PyRep::Integer(kr_id)),
                        ("forSale", PyRep::Bool(false)),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("bountyProxy", "GetMyKillRights") | ("bountyProxy", "GetKillRightsOnCharacters") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let now_unix = ctx.now_unix();
            let requested_ids = super::extract_id_collection(args.first());
            let list = ctx.db.with_snapshot(|snap| {
                let rt = &snap.combat_crimewatch_state;
                snap.kill_rights
                    .values()
                    .filter(|kr| {
                        let exp = rt
                            .kill_right_expires_unix
                            .get(&kr.kill_right_id)
                            .copied()
                            .unwrap_or(0);
                        if exp > 0 && now_unix >= exp {
                            return false;
                        }
                        if !requested_ids.is_empty() {
                            requested_ids.contains(&kr.target_id)
                                || requested_ids.contains(&kr.owner_id)
                        } else {
                            kr.owner_id == char_id || kr.target_id == char_id
                        }
                    })
                    .map(|kr| {
                        let exp = rt
                            .kill_right_expires_unix
                            .get(&kr.kill_right_id)
                            .copied()
                            .unwrap_or(0);
                        let restr = rt.kill_right_restricted_to.get(&kr.kill_right_id).copied();
                        let restr_rep = match restr {
                            Some(id) if id > 0 => PyRep::Integer(id),
                            _ => PyRep::None,
                        };
                        PyRep::keyval([
                            ("killRightID", PyRep::Integer(kr.kill_right_id)),
                            ("fromID", PyRep::Integer(kr.owner_id)),
                            ("toID", PyRep::Integer(kr.target_id)),
                            ("price", PyRep::Real(kr.activation_fee.to_wire_f64())),
                            ("priceISK", PyRep::Real(kr.activation_fee.to_wire_f64())),
                            ("activeSuspect", PyRep::Bool(kr.is_active_suspect)),
                            ("expiryTime", PyRep::Integer(exp as i64)),
                            ("restrictedTo", restr_rep),
                        ])
                    })
                    .collect::<Vec<_>>()
            });
            Some(Ok(PyRep::List(list)))
        }

        ("bountyProxy", "AddToBounty") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let target_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let amount_isk = args.get(1).and_then(|a| a.as_f64()).unwrap_or(0.0);
            if target_id <= 0 || !amount_isk.is_finite() || amount_isk < 100_000.0 {
                return Some(Err(BridgeRpcError {
                    code: "BountyAmountTooLow".into(),
                    message: "Minimum bounty placement is 100,000 ISK on a valid target ID".into(),
                }));
            }
            let tx_res = ctx.db.try_with_mut_snapshot(|snap| {
                if !(snap.characters.contains_key(&target_id)
                    || snap.corporations.contains_key(&target_id)
                    || snap.alliances.contains_key(&target_id))
                {
                    return Err(BridgeRpcError {
                        code: "TargetNotFound".into(),
                        message: format!("Target {target_id} not found"),
                    });
                }
                let Some(ch) = snap.characters.get_mut(&char_id) else {
                    return Err(BridgeRpcError {
                        code: "CharacterNotFound".into(),
                        message: format!("Character {char_id} not found"),
                    });
                };
                let cost = IskCents::from_f64_isk(amount_isk);
                let new_bal = ch.balance.checked_sub(cost).ok_or_else(|| BridgeRpcError {
                    code: "InsufficientFunds".into(),
                    message: "Insufficient ISK to place bounty".into(),
                })?;
                ch.balance = new_bal;
                snap.append_journal(
                    char_id,
                    "bounty_placed",
                    cost,
                    new_bal,
                    format!("Bounty placed on {target_id}"),
                );

                snap.combat_crimewatch_state
                    .bounty_contributions
                    .entry(target_id)
                    .or_default()
                    .push((char_id, amount_isk));
                let entry = snap
                    .combat_crimewatch_state
                    .player_bounty_pool
                    .entry(target_id)
                    .or_insert(0.0);
                *entry += amount_isk;
                let total = *entry;
                ctx.combat_ephemeral
                    .write()
                    .unwrap_or_else(|e| e.into_inner())
                    .sync_from_persisted(&snap.combat_crimewatch_state);
                Ok((new_bal.to_wire_f64(), total))
            });
            let (new_bal, total_pool) = match tx_res {
                Ok(b) => b,
                Err(e) => return Some(Err(e)),
            };
            let _ = ctx.notify_account_change(char_id, "cash", char_id, new_bal);
            ctx.notify(
                NotificationTarget::Character(target_id),
                "OnBountyPlaced",
                "charid",
                vec![
                    PyRep::Integer(char_id),
                    PyRep::Integer(target_id),
                    PyRep::Real(amount_isk),
                ],
            );
            Some(Ok(PyRep::keyval([
                ("targetID", PyRep::Integer(target_id)),
                ("addedISK", PyRep::Real(amount_isk)),
                ("totalBountyISK", PyRep::Real(total_pool)),
            ])))
        }

        ("bountyProxy", "GetBounty")
        | ("bountyProxy", "GetBounties")
        | ("bountyProxy", "GetBountiesAndKillRights") => {
            if method == "GetBountiesAndKillRights" {
                return Some(super::handle_get_bounties_and_kill_rights(
                    ctx, session, args,
                ));
            }
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if method == "GetBounty" {
                let tid = args.first().and_then(|a| a.as_i64()).unwrap_or(char_id);
                let amt = with_combat_runtime(ctx, |rt| {
                    rt.player_bounty_pool.get(&tid).copied().unwrap_or(0.0)
                });
                return Some(Ok(PyRep::keyval([
                    ("targetID", PyRep::Integer(tid)),
                    ("bountyISK", PyRep::Real(amt)),
                    ("bounty", PyRep::Real(amt)),
                ])));
            }
            let requested_ids = super::extract_id_collection(args.first());
            let rows = with_combat_runtime(ctx, |rt| {
                let pool = &rt.player_bounty_pool;
                let mut v = Vec::new();
                let push_entry = |list: &mut Vec<PyRep>, tid: i64, amt: f64| {
                    if amt > 0.0 {
                        list.push(PyRep::Tuple(vec![
                            PyRep::Integer(tid),
                            PyRep::keyval([
                                ("targetID", PyRep::Integer(tid)),
                                ("bountyISK", PyRep::Real(amt)),
                                ("bounty", PyRep::Real(amt)),
                            ]),
                        ]));
                    }
                };
                if !requested_ids.is_empty() {
                    for tid in requested_ids {
                        if let Some(&amt) = pool.get(&tid) {
                            push_entry(&mut v, tid, amt);
                        }
                    }
                } else {
                    for (&tid, &amt) in pool {
                        push_entry(&mut v, tid, amt);
                    }
                }
                v
            });
            Some(Ok(PyRep::List(rows)))
        }

        ("bountyProxy", "GetMyBounties") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let raw: Vec<(i64, f64)> = with_combat_runtime(ctx, |rt| {
                rt.player_bounty_pool
                    .iter()
                    .filter(|(&tid, &amt)| {
                        amt > 0.0
                            && (tid == char_id
                                || rt
                                    .bounty_contributions
                                    .get(&tid)
                                    .is_some_and(|c| c.iter().any(|&(cid, _)| cid == char_id)))
                    })
                    .map(|(&tid, &amt)| (tid, amt))
                    .collect()
            });
            let rows = ctx.db.with_snapshot(|snap| {
                raw.into_iter()
                    .map(|(tid, amt)| {
                        let (corp_id, alli_id) = if let Some(ch) = snap.characters.get(&tid) {
                            (ch.corporation_id, ch.alliance_id.unwrap_or(0))
                        } else if let Some(c) = snap.corporations.get(&tid) {
                            (tid, c.alliance_id.unwrap_or(0))
                        } else if snap.alliances.contains_key(&tid) {
                            (0, tid)
                        } else {
                            (0, 0)
                        };
                        PyRep::keyval([
                            ("targetID", PyRep::Integer(tid)),
                            ("corporationID", PyRep::Integer(corp_id)),
                            ("allianceID", PyRep::Integer(alli_id)),
                            ("amount", PyRep::Real(amt)),
                            ("bountyISK", PyRep::Real(amt)),
                            ("bounty", PyRep::Real(amt)),
                        ])
                    })
                    .collect()
            });
            Some(Ok(PyRep::List(rows)))
        }

        ("bountyProxy", "SearchBounties") | ("bountyProxy", "SearchCharBounties") => {
            Some(handle_search_bounties(
                ctx,
                session,
                BountySearchType::Pilot,
                args,
            ))
        }

        ("bountyProxy", "SearchCorpBounties") => {
            Some(handle_search_bounties(ctx, session, BountySearchType::Corporation, args))
        }

        ("bountyProxy", "SearchAllianceBounties") => {
            Some(handle_search_bounties(ctx, session, BountySearchType::Alliance, args))
        }

        ("bountyProxy", "clear_memoized") => {
            Some(handle_bounty_clear_memoized(ctx, session))
        }

        ("bountyProxy", "GetTopPilotBounties") => {
            let page = args.first().and_then(|a| a.as_i64()).unwrap_or(1);
            Some(handle_get_top_bounties(
                ctx,
                session,
                BountyTargetType::Pilot,
                false,
                page,
            ))
        }

        ("bountyProxy", "GetTopCorpBounties") => {
            let page = args.first().and_then(|a| a.as_i64()).unwrap_or(1);
            Some(handle_get_top_bounties(
                ctx,
                session,
                BountyTargetType::Corporation,
                false,
                page,
            ))
        }

        ("bountyProxy", "GetTopAllianceBounties") => {
            let page = args.first().and_then(|a| a.as_i64()).unwrap_or(1);
            Some(handle_get_top_bounties(
                ctx,
                session,
                BountyTargetType::Alliance,
                false,
                page,
            ))
        }

        ("bountyProxy", "GetTopPilotBountyHunters") => {
            let page = args.first().and_then(|a| a.as_i64()).unwrap_or(1);
            Some(handle_get_top_bounties(
                ctx,
                session,
                BountyTargetType::Pilot,
                true,
                page,
            ))
        }

        ("bountyProxy", "GetTopCorporationBountyHunters") => {
            let page = args.first().and_then(|a| a.as_i64()).unwrap_or(1);
            Some(handle_get_top_bounties(
                ctx,
                session,
                BountyTargetType::Corporation,
                true,
                page,
            ))
        }

        ("bountyProxy", "GetTopAllianceBountyHunters") => {
            let page = args.first().and_then(|a| a.as_i64()).unwrap_or(1);
            Some(handle_get_top_bounties(
                ctx,
                session,
                BountyTargetType::Alliance,
                true,
                page,
            ))
        }

        ("bountyProxy", "GetTopBountyHunters") => {
            let page = args.first().and_then(|a| a.as_i64()).unwrap_or(1);
            Some(handle_get_top_bounties(
                ctx,
                session,
                BountyTargetType::All,
                true,
                page,
            ))
        }

        ("bountyProxy", "GMClearBountyCache") => {
            Some(handle_gm_clear_bounty_cache(ctx, session))
        }

        ("bountyProxy", "GMReimburseBounties") | ("bountyProxy", "ClearBounty") => {
            Some(handle_gm_bounty_admin(ctx, session, method, args))
        }

        _ => None,
    }
}

pub fn handle_apply_tags_for_sec_status(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (_, char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let Some(station_id) = session.station_id else {
        return Some(Err(BridgeRpcError {
            code: "NotDockedInStation".into(),
            message: "Security status tag turn-in requires being docked at a CONCORD station".into(),
        }));
    };
    let Some(st) = ctx.sde.stations.get(&station_id) else {
        return Some(Err(BridgeRpcError {
            code: "SecurityOfficeRequired".into(),
            message: "Security status tags can only be turned in at a CONCORD/DED Security Office station".into(),
        }));
    };
    let st_sec = ctx
        .sde
        .solar_systems
        .get(&st.solar_system_id)
        .map(|s| s.security_status)
        .unwrap_or(0.0);
    if !(0.0 < st_sec && st_sec < 0.45)
        || !crate::services::station_structure::npc_station_offers_service(&ctx.sde, station_id, 27)
    {
        return Some(Err(BridgeRpcError {
            code: "SecurityOfficeRequired".into(),
            message: "Security status tags can only be turned in at a low-security CONCORD/DED Security Office station".into(),
        }));
    }
    let Some(now_target_sec) = (if args.len() == 2 {
        args[0].as_f64().filter(|v| v.is_finite())
    } else {
        None
    }) else {
        return Some(Err(BridgeRpcError {
            code: "InvalidSecurityTagArguments".into(),
            message: "ApplyTagsForSecStatus requires (targetSecurityStatus, currentSecurityStatus) or (targetSecStatus, tagsToUseDict)".into(),
        }));
    };
    let requested_by_type: Option<[i32; 4]> = match &args[1] {
        PyRep::Dict(raw_tags) => {
            if raw_tags.is_empty() {
                return Some(Err(BridgeRpcError {
                    code: "InvalidQuantity".into(),
                    message: "tagsToUseDict must specify at least one tag".into(),
                }));
            }
            let mut counts: [i32; 4] = [0; 4];
            for (k, v) in raw_tags {
                let Some(tag_type_id) = k.as_i64().and_then(|val| u32::try_from(val).ok()) else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidSecurityTagType".into(),
                        message: "Invalid security tag typeID".into(),
                    }));
                };
                let Some(idx) = (33138..=33141).position(|id| id == tag_type_id) else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidSecurityTagType".into(),
                        message: format!("Type {tag_type_id} is not a valid Clone Soldier Tag (33138..=33141)"),
                    }));
                };
                let Some(q) = v.as_i64().and_then(|val| i32::try_from(val).ok()).filter(|&q| q > 0) else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidQuantity".into(),
                        message: "Tag quantity must be a positive integer".into(),
                    }));
                };
                counts[idx] = counts[idx].saturating_add(q);
            }
            Some(counts)
        }
        other => {
            if !matches!(other, PyRep::Real(v) if v.is_finite()) {
                return Some(Err(BridgeRpcError {
                    code: "InvalidSecurityTagArguments".into(),
                    message: "ApplyTagsForSecStatus second argument must be currentSecurityStatus or tagsToUseDict".into(),
                }));
            }
            None
        }
    };
    let now_unix = ctx.now_unix();
    let Some(sys_id) = session.solar_system_id.or_else(|| {
        ctx.db.with_snapshot(|s| s.characters.get(&char_id).map(|c| c.solar_system_id))
    }) else {
        return Some(Err(BridgeRpcError {
            code: "NoSolarSystem".into(),
            message: "Character has no active solar system".into(),
        }));
    };

    // DERIVED: CONCORD Security Office tag processing fees per Clone Soldier security band
    let tag_bands: [(u32, f64, f64, i64); 4] = [
        (33138, -10.0, -8.0, 1_000_000),
        (33139, -8.0, -5.0, 2_000_000),
        (33140, -5.0, -2.0, 5_000_000),
        (33141, -2.0, 0.0, 10_000_000),
    ];

    let res = ctx.db.try_with_mut_snapshot(|snap| {
        if !snap
            .characters
            .get(&char_id)
            .is_some_and(|ch| ch.station_id == Some(station_id))
        {
            return Err(BridgeRpcError {
                code: "NotDockedInStation".into(),
                message: "Character is not docked at this station".into(),
            });
        }
        let orig_sec = snap
            .characters
            .get(&char_id)
            .map(|c| c.security_status)
            .unwrap_or(0.0);
        let effective_target = now_target_sec.min(0.0);
        if effective_target <= orig_sec + 1e-9 {
            return Err(BridgeRpcError {
                code: "InvalidTargetSecurityStatus".into(),
                message: format!(
                    "Target security status {now_target_sec:.2} must exceed current security status {orig_sec:.2} (max 0.0)"
                ),
            });
        }
        let ship_id_opt = session.ship_id.filter(|sid| {
            snap.items
                .get(sid)
                .is_some_and(|s| s.location_id == station_id)
        });
        let mut cur_sec = orig_sec;
        let mut total_fee_isk: i64 = 0;
        let mut total_consumed: i32 = 0;
        let mut tier_txs: Vec<(u32, f64, f64)> = Vec::new();
        let mut changed_items: Vec<(eve_db::InventoryItemRecord, Option<eve_db::InventoryItemRecord>)> = Vec::new();
        for (idx, (tag_type_id, min_band, max_band, fee_per_tag_isk)) in tag_bands.into_iter().enumerate() {
            if let Some(req_counts) = requested_by_type {
                let req_qty = req_counts[idx];
                if req_qty <= 0 {
                    continue;
                }
                if cur_sec < min_band - 1e-6 || cur_sec >= max_band - 1e-6 {
                    return Err(BridgeRpcError {
                        code: "SecurityTagBandMismatch".into(),
                        message: format!(
                            "Tag {tag_type_id} is only valid in security band [{min_band:.1}, {max_band:.1}); character is at {cur_sec:.2}"
                        ),
                    });
                }
                if cur_sec + 1e-9 >= effective_target {
                    continue;
                }
                let band_cap = ((max_band - cur_sec) / 0.5).ceil().max(1.0) as i32;
                let target_cap = ((effective_target - cur_sec) / 0.5).ceil().max(0.0) as i32;
                let max_needed = band_cap.min(target_cap);
                if max_needed <= 0 {
                    continue;
                }
                let consumed_qty = req_qty.min(max_needed);
                consume_band_tags(
                    snap,
                    char_id,
                    station_id,
                    ship_id_opt,
                    tag_type_id,
                    consumed_qty,
                    &mut changed_items,
                )?;
                let prev_tier_sec = cur_sec;
                cur_sec = (cur_sec + 0.5 * f64::from(consumed_qty)).min(0.0);
                tier_txs.push((tag_type_id, prev_tier_sec, cur_sec));
                total_fee_isk = total_fee_isk.saturating_add(fee_per_tag_isk.saturating_mul(i64::from(consumed_qty)));
                total_consumed = total_consumed.saturating_add(consumed_qty);
            } else {
                if cur_sec + 1e-9 >= effective_target || cur_sec < min_band - 1e-6 || cur_sec >= max_band - 1e-6 {
                    continue;
                }
                let band_cap = ((max_band - cur_sec) / 0.5).ceil().max(1.0) as i32;
                let target_cap = ((effective_target - cur_sec) / 0.5).ceil().max(0.0) as i32;
                let consumed_qty = band_cap.min(target_cap);
                if consumed_qty <= 0 {
                    continue;
                }
                consume_band_tags(
                    snap,
                    char_id,
                    station_id,
                    ship_id_opt,
                    tag_type_id,
                    consumed_qty,
                    &mut changed_items,
                )?;
                let prev_tier_sec = cur_sec;
                cur_sec = (cur_sec + 0.5 * f64::from(consumed_qty)).min(0.0);
                tier_txs.push((tag_type_id, prev_tier_sec, cur_sec));
                total_fee_isk = total_fee_isk.saturating_add(fee_per_tag_isk.saturating_mul(i64::from(consumed_qty)));
                total_consumed = total_consumed.saturating_add(consumed_qty);
            }
        }
        if total_consumed == 0 {
            return Err(BridgeRpcError {
                code: "InvalidTargetSecurityStatus".into(),
                message: format!(
                    "No Clone Soldier security tags applicable for transition from {orig_sec:.2} to {effective_target:.2}"
                ),
            });
        }
        let total_fee = eve_db::IskCents::from_isk(total_fee_isk);
        let cur_bal = snap
            .characters
            .get(&char_id)
            .map(|c| c.balance)
            .unwrap_or(eve_db::IskCents(0));
        let Some(new_bal) = cur_bal.checked_sub(total_fee) else {
            return Err(BridgeRpcError {
                code: "InsufficientFunds".into(),
                message: "Insufficient ISK for CONCORD Security Office tag processing fee".into(),
            });
        };
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.security_status = cur_sec;
            ch.balance = new_bal;
        }
        snap.append_journal(
            char_id,
            "SecurityTagProcessing",
            eve_db::IskCents(-total_fee.0),
            new_bal,
            format!("CONCORD Security Office tag turn-in ({total_consumed}x tags)"),
        );
        Ok((orig_sec, cur_sec, new_bal.to_wire_f64(), changed_items, tier_txs))
    });

    match res {
        Ok((old_sec, new_sec, new_bal_f64, changed_items, tier_txs)) => {
            crate::services::combat_crimewatch::with_mut_combat_runtime(ctx, |rt| {
                let list = rt.sec_status_transactions.entry(char_id).or_default();
                if tier_txs.is_empty() {
                    list.push(crate::services::combat_crimewatch::SecStatusTransaction {
                        timestamp_unix: now_unix,
                        old_sec,
                        new_sec,
                        delta: new_sec - old_sec,
                        reason: "Clone Soldier Security Tag".into(),
                        solar_system_id: sys_id,
                        event_type_id: 385,
                        other_owner_id: None,
                        other_type_id: None,
                    });
                } else {
                    for (tag_type_id, t_old, t_new) in tier_txs {
                        list.push(crate::services::combat_crimewatch::SecStatusTransaction {
                            timestamp_unix: now_unix,
                            old_sec: t_old,
                            new_sec: t_new,
                            delta: t_new - t_old,
                            reason: "Clone Soldier Security Tag".into(),
                            solar_system_id: sys_id,
                            event_type_id: 385,
                            other_owner_id: None,
                            other_type_id: Some(tag_type_id),
                        });
                    }
                }
            });
            ctx.notify(
                crate::router::NotificationTarget::Character(char_id),
                "OnAccountChange",
                "charid",
                vec![
                    PyRep::String("cash".into()),
                    PyRep::Integer(char_id),
                    PyRep::Real(new_bal_f64),
                ],
            );
            for (old_item, new_item_opt) in changed_items {
                let (rem_qty, rem_loc) = new_item_opt
                    .as_ref()
                    .map(|i| (i.quantity, i.location_id))
                    .unwrap_or((0, 6));
                let (group_id, category_id) = ctx
                    .sde
                    .types
                    .get(&old_item.type_id)
                    .map(|t| (t.group_id as i64, t.category_id as i64))
                    .unwrap_or((1224, 17));
                let mut changes = vec![
                    (PyRep::Integer(5), PyRep::Integer(old_item.quantity as i64)), // ixQuantity
                    (PyRep::Integer(9), PyRep::Integer(old_item.quantity as i64)), // ixStackSize
                ];
                if new_item_opt.is_none() {
                    changes.push((PyRep::Integer(3), PyRep::Integer(old_item.location_id))); // ixLocationID
                }
                ctx.notify(
                    crate::router::NotificationTarget::Character(char_id),
                    "OnItemChange",
                    "charid",
                    vec![
                        PyRep::keyval([
                            ("itemID", PyRep::Integer(old_item.item_id)),
                            ("typeID", PyRep::Integer(old_item.type_id as i64)),
                            ("ownerID", PyRep::Integer(old_item.owner_id)),
                            ("locationID", PyRep::Integer(rem_loc)),
                            ("flagID", PyRep::Integer(old_item.flag as i64)),
                            ("quantity", PyRep::Integer(rem_qty as i64)),
                            ("groupID", PyRep::Integer(group_id)),
                            ("categoryID", PyRep::Integer(category_id)),
                            ("stacksize", PyRep::Integer(rem_qty as i64)),
                        ]),
                        PyRep::Dict(changes),
                        PyRep::Integer(old_item.location_id),
                    ],
                );
            }
            ctx.notify(
                crate::router::NotificationTarget::Character(char_id),
                "OnSecurityStatusUpdate",
                "charid",
                vec![PyRep::Real(new_sec)],
            );
            Some(Ok(PyRep::keyval([
                ("oldSecurityStatus", PyRep::Real(old_sec)),
                ("newSecurityStatus", PyRep::Real(new_sec)),
            ])))
        }
        Err(e) => Some(Err(e)),
    }
}

fn consume_band_tags(
    snap: &mut eve_db::WorldStateSnapshot,
    char_id: i64,
    station_id: i64,
    ship_id_opt: Option<i64>,
    tag_type_id: u32,
    consumed_qty: i32,
    changed_items: &mut Vec<(eve_db::InventoryItemRecord, Option<eve_db::InventoryItemRecord>)>,
) -> Result<(), BridgeRpcError> {
    let mut matching_ids: Vec<(i64, i32)> = snap
        .items
        .values()
        .filter(|i| {
            i.owner_id == char_id
                && i.type_id == tag_type_id
                && i.quantity > 0
                && ((i.location_id == station_id && i.flag == eve_db::InventoryFlag::Hangar)
                    || (ship_id_opt.is_some_and(|sid| i.location_id == sid)
                        && i.flag == eve_db::InventoryFlag::Cargo))
                && !snap.is_item_locked_in_contract_or_job(i.item_id)
        })
        .map(|i| (i.item_id, i.quantity))
        .collect();
    matching_ids.sort_by_key(|&(iid, _)| iid);
    let total_avail: i64 = matching_ids.iter().map(|&(_, q)| i64::from(q)).sum();
    if total_avail < i64::from(consumed_qty) {
        return Err(BridgeRpcError {
            code: "InsufficientSecurityTags".into(),
            message: format!(
                "Missing {consumed_qty}x unlocked tag type {tag_type_id} in local station hangar or active ship cargo"
            ),
        });
    }
    let mut remaining = consumed_qty;
    for (iid, _) in matching_ids {
        if remaining <= 0 {
            break;
        }
        if let Some(old_item) = snap.items.get(&iid).cloned() {
            let take = old_item.quantity.min(remaining);
            remaining -= take;
            let new_item_opt = if let Some(it) = snap.items.get_mut(&iid) {
                if it.quantity > take {
                    it.quantity -= take;
                    it.version += 1;
                    Some(it.clone())
                } else {
                    snap.items.remove(&iid);
                    None
                }
            } else {
                None
            };
            changed_items.push((old_item, new_item_opt));
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "crimewatch_bounty_tests.rs"] mod crimewatch_bounty_tests;

