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

/// Dispatches all `bountyProxy` machoNet RPC calls.
pub fn handle_bounty_proxy(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("bountyProxy", "SellKillRight") => {
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
            let restricted_to = args.get(2).and_then(|a| a.as_i64()).filter(|&r| r > 0);
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let valid_target = restricted_to.is_none_or(|r| {
                    snap.characters.contains_key(&r)
                        || snap.corporations.contains_key(&r)
                        || snap.alliances.contains_key(&r)
                });
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
                let exp = snap
                    .combat_crimewatch_state
                    .kill_right_expires_unix
                    .get(&kr_id)
                    .copied()
                    .unwrap_or(0);
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

        ("bountyProxy", "CancelSellKillRight") => {
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
                Ok(()) => Some(Ok(PyRep::keyval([
                    ("killRightID", PyRep::Integer(kr_id)),
                    ("forSale", PyRep::Bool(false)),
                ]))),
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

        ("bountyProxy", "SearchBounties")
        | ("bountyProxy", "SearchCharBounties")
        | ("bountyProxy", "SearchCorpBounties")
        | ("bountyProxy", "SearchAllianceBounties") => {
            let (_, _char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
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
                if method == "SearchAllianceBounties" {
                    snap.alliances
                        .values()
                        .filter(|a| m(a.alliance_id, &a.name))
                        .map(|a| a.alliance_id)
                        .collect()
                } else if method == "SearchCorpBounties" {
                    snap.corporations
                        .values()
                        .filter(|c| m(c.corporation_id, &c.name))
                        .map(|c| c.corporation_id)
                        .collect()
                } else {
                    snap.characters
                        .values()
                        .filter(|c| m(c.character_id, &c.name))
                        .map(|c| c.character_id)
                        .collect()
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
                        let (name, corp_id, alli_id) = if let Some(ch) = snap.characters.get(&tid)
                        {
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
            Some(Ok(PyRep::List(rows)))
        }

        ("bountyProxy", "clear_memoized") => {
            // const-ok: memoized cache invalidation notification returns None
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            Some(Ok(PyRep::None))
        }

        ("bountyProxy", "GetTopPilotBounties")
        | ("bountyProxy", "GetTopCorpBounties")
        | ("bountyProxy", "GetTopAllianceBounties")
        | ("bountyProxy", "GetTopPilotBountyHunters")
        | ("bountyProxy", "GetTopCorporationBountyHunters")
        | ("bountyProxy", "GetTopAllianceBountyHunters")
        | ("bountyProxy", "GetTopBountyHunters") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let _page = args.first().and_then(|a| a.as_i64()).unwrap_or(1);
            let (is_hunters, is_pilot, is_corp, is_alliance) = (
                method.contains("Hunter"),
                method.contains("Pilot"),
                method.contains("Corp"),
                method.contains("Alliance"),
            );
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
                    if (is_pilot && snap.characters.contains_key(&id))
                        || (is_corp && snap.corporations.contains_key(&id))
                        || (is_alliance && snap.alliances.contains_key(&id))
                        || (!is_pilot && !is_corp && !is_alliance)
                    {
                        agg.insert(id, amt);
                    }
                }
                if is_corp || is_alliance {
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
                Some(Ok(PyRep::Tuple(vec![
                    PyRep::List(rows),
                    PyRep::Integer(ticks),
                ])))
            } else {
                Some(Ok(PyRep::List(rows)))
            }
        }

        ("bountyProxy", "GMClearBountyCache") => {
            if let Err(e) = ServerContext::require_gm_role(session) {
                return Some(Err(e));
            }
            Some(Ok(PyRep::None))
        }

        ("bountyProxy", "GMReimburseBounties") | ("bountyProxy", "ClearBounty") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let target_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let set_amt = args.get(1).and_then(|a| a.as_f64());
            let action = if method == "GMReimburseBounties" {
                "reimburse"
            } else if set_amt.is_some() {
                "set"
            } else {
                "clear"
            };
            match gm_admin_bounty_action(ctx, session, action, target_id, set_amt.unwrap_or(0.0)) {
                Ok(amount) => Some(Ok(PyRep::keyval([
                    ("action", PyRep::String(action.into())),
                    ("targetID", PyRep::Integer(target_id)),
                    ("amountISK", PyRep::Real(amount)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        _ => None,
    }
}
