//! Corporation Governance, Shares, Dividends, Titles, Votes, Medals, Bulletins, Recruitment & Alliances (`SO-3`).

use crate::router::{cache_notifications::NotificationTarget, ServerContext};
use eve_db::{
    corp_roles, ChatChannelRecord, CorpWalletDivisionRecord, CorporationRecord, IskCents,
    CorpVoteBallotRecord, CorpVoteCaseRecord, WalletJournalEntry,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::{HashMap, HashSet};
use super::fleet_social_corps::{read_corp_gov, with_corp_gov};
use super::fleet_social_persistence::{
    label_ids_from_mask, label_mask_from_ids, sync_corp_contacts_esi,
};

pub(super) fn handle_add_corporation(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (_, char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let name = args.first().and_then(|a| a.as_str()).unwrap_or("EvoMu Industrial Corp");
    let ticker = args.get(1).and_then(|a| a.as_str()).unwrap_or("EVOMU");
    let tax_rate = args.get(2).and_then(|a| a.as_f64()).unwrap_or(0.10).clamp(0.0, 1.0);
    let reg_fee = IskCents::from_isk(1_599_800);
    let now_unix = ctx.now_unix();

    let res = ctx.db.try_with_mut_snapshot(|snap| {
        let corp_mgmt_lvl = snap
            .skills
            .get(&char_id)
            .and_then(|m| m.get(&3363))
            .map(|s| s.level)
            .unwrap_or(0);
        if corp_mgmt_lvl < 1 {
            return Err(BridgeRpcError {
                code: "MissingCorporationManagementSkill".into(),
                message: "Creating a corporation requires Corporation Management (3363) level 1".into(),
            });
        }
        if snap
            .corporations
            .values()
            .any(|c| c.name.eq_ignore_ascii_case(name) || c.ticker.eq_ignore_ascii_case(ticker))
        {
            return Err(BridgeRpcError {
                code: "CorpNameOrTickerTaken".into(),
                message: "Corporation name or ticker is already in use".into(),
            });
        }
        let Some(ch) = snap.characters.get_mut(&char_id) else {
            return Err(BridgeRpcError {
                code: "CharacterNotFound".into(),
                message: format!("Character {char_id} not found"),
            });
        };
        let old_corp_id = ch.corporation_id;
        let new_bal = ch.balance.checked_sub(reg_fee).ok_or_else(|| BridgeRpcError {
            code: "InsufficientFunds".into(),
            message: "Not enough ISK to pay corporation registration fee (1,599,800 ISK)".into(),
        })?;
        ch.balance = new_bal;

        snap.next_entity_id += 1;
        let cid = snap.allocate_corporation_id();
        if let Some(old_corp) = snap.corporations.get_mut(&old_corp_id) {
            old_corp.members.remove(&char_id);
            old_corp.member_roles.remove(&char_id);
        }
        if let Some(ch_mut) = snap.characters.get_mut(&char_id) {
            ch_mut.corporation_id = cid;
            ch_mut.alliance_id = None;
        }

        let wallet_divisions: Vec<CorpWalletDivisionRecord> = (1..=7)
            .map(|d| CorpWalletDivisionRecord {
                division_id: d,
                name: if d == 1 { "Master Wallet".into() } else { format!("Division {d}") },
                balance: IskCents(0),
            })
            .collect();

        let mut member_roles = HashMap::new();
        member_roles.insert(char_id, u64::MAX);

        snap.corporations.insert(
            cid,
            CorporationRecord {
                corporation_id: cid,
                name: name.to_string(),
                ticker: ticker.to_string(),
                ceo_id: char_id,
                alliance_id: None,
                tax_rate,
                master_wallet: IskCents(0),
                members: HashSet::from([char_id]),
                member_roles,
                applications: HashMap::new(),
                wallet_divisions,
                description: String::new(),
                url: String::new(),
                hq_station_id: Some(session.station_id.unwrap_or(60_003_760)),
            },
        );
        snap.chat_channels.insert(format!("corpid:{cid}"), ChatChannelRecord { channel_id: format!("corpid:{cid}"), name: name.to_string(), motd: String::new(), owner_id: Some(char_id), members: HashSet::from([char_id]), is_system_local: false, solar_system_id: None, is_private: false, operators: HashSet::from([char_id]), password: None, is_delayed_local: false });
        let state = snap.corp_governance.entry(cid).or_default();
        state.shares.insert(char_id, 1_000);
        state.company_shares = 0;
        state.accept_structures = true;
        state.reinforce_hour = 18;
        snap.corp_shareholders.insert(
            cid,
            vec![eve_db::snapshot_ext::CorpShareholderEntry {
                shareholder_id: char_id,
                shareholder_type: "character".into(),
                share_count: 1_000,
            }],
        );
        super::fleet_social_corps::record_corp_join_in_snap(snap, char_id, old_corp_id, cid, now_unix);
        Ok(cid)
    });

    match res {
        Ok(corp_id) => {
            session.corporation_id = Some(corp_id);
            session.alliance_id = None;
            session.corp_role = u64::MAX;
            let change = PyRep::Dict(vec![
                (PyRep::String(("corporationID").into()), PyRep::Tuple(vec![PyRep::None, PyRep::Integer(corp_id)])),
                (PyRep::String(("corporationName").into()), PyRep::Tuple(vec![PyRep::None, PyRep::String(name.to_string())])),
                (PyRep::String(("tickerName").into()), PyRep::Tuple(vec![PyRep::None, PyRep::String(ticker.to_string())])),
                (PyRep::String(("ceoID").into()), PyRep::Tuple(vec![PyRep::None, PyRep::Integer(char_id)])),
            ]);
            ctx.notify(
                NotificationTarget::Corporation(corp_id),
                "OnCorporationChanged",
                "corpid",
                vec![PyRep::Integer(corp_id), change],
            );
            Some(Ok(PyRep::keyval([
                ("corporationID", PyRep::Integer(corp_id)),
                ("allianceID", PyRep::None),
                ("name", PyRep::String(name.to_string())),
                ("ticker", PyRep::String(ticker.to_string())),
            ])))
        }
        Err(e) => Some(Err(e)),
    }
}

pub(super) fn handle_get_wallet_divisions_info(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (_, char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let corp_id = args.first().and_then(|a| a.as_i64()).or(session.corporation_id).unwrap_or(0);
    let res = ctx.db.with_snapshot(|snap| {
        let Some(corp) = snap.corporations.get(&corp_id) else {
            return Err(BridgeRpcError {
                code: "CorporationNotFound".into(),
                message: format!("Corporation {corp_id} not found"),
            });
        };
        let caller_role = corp.member_roles.get(&char_id).copied().unwrap_or(0);
        if char_id != corp.ceo_id
            && !corp_roles::has_role(caller_role, corp_roles::ROLE_ACCOUNTANT)
            && !corp_roles::has_role(caller_role, corp_roles::ROLE_JUNIOR_ACCOUNTANT)
        {
            return Err(BridgeRpcError {
                code: "CorpRoleDenied".into(),
                message: "Viewing corporation wallet divisions requires Accountant role".into(),
            });
        }
        let rows: Vec<Vec<PyRep>> = (1..=7u8)
            .map(|d| {
                let name = corp
                    .wallet_divisions
                    .iter()
                    .find(|w| w.division_id == d)
                    .map(|w| w.name.clone())
                    .unwrap_or_else(|| if d == 1 { "Master Wallet".into() } else { format!("Division {d}") });
                vec![
                    PyRep::Integer(d as i64),
                    PyRep::String(name),
                    PyRep::Real(corp.division_balance(d).to_wire_f64()),
                ]
            })
            .collect();
        Ok(rows)
    });
    match res {
        Ok(rows) => Some(Ok(PyRep::rowset(vec!["divisionID", "name", "balance"], rows))),
        Err(e) => Some(Err(e)),
    }
}

pub(super) fn handle_corp_gov(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (_, char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let corp_id = session.corporation_id.unwrap_or(1_000_009);
    let now_unix = ctx.now_unix();
    let (has_director, has_comms, has_diplomat) = ctx.db.with_snapshot(|snap| {
        snap.corporations.get(&corp_id).map_or((true, true, true), |c| {
            let r = c.member_roles.get(&char_id).copied().unwrap_or(0);
            let dir = c.ceo_id == char_id || corp_roles::has_role(r, corp_roles::ROLE_DIRECTOR);
            let comms = dir || corp_roles::has_role(r, corp_roles::ROLE_PERSONNEL_MANAGER);
            let dip = dir || corp_roles::has_role(r, 1u64 << 42);
            (dir, comms, dip)
        })
    });
    let deny_role = |msg: &str| Some(Err(BridgeRpcError { code: "CorpRoleDenied".into(), message: msg.into() }));

    match (service, method) {
        ("corpRegistry", "MoveCompanyShares") | ("corpRegistry", "MovePrivateShares") => {
            let (target_id, qty) = if args.len() >= 3 {
                (
                    args.get(1).and_then(|a| a.as_i64()).unwrap_or(char_id),
                    args.get(2).and_then(|a| a.as_i64()).unwrap_or(100).max(1),
                )
            } else {
                (
                    args.first().and_then(|a| a.as_i64()).unwrap_or(char_id),
                    args.get(1).and_then(|a| a.as_i64()).unwrap_or(100).max(1),
                )
            };
            let tx_res = ctx.db.try_with_mut_snapshot(|snap| {
                let state = snap.corp_governance.entry(corp_id).or_default();
                let from_count = state.shares.entry(char_id).or_insert(0);
                if *from_count < qty {
                    return Err(BridgeRpcError {
                        code: "InsufficientShares".into(),
                        message: "Not enough shares to transfer".into(),
                    });
                }
                let old_f = *from_count;
                *from_count -= qty;
                let new_f = *from_count;
                let to_entry = state.shares.entry(target_id).or_insert(0);
                let old_t = *to_entry;
                *to_entry += qty;
                let new_t = *to_entry;
                let mut holders: Vec<eve_db::snapshot_ext::CorpShareholderEntry> = state
                    .shares
                    .iter()
                    .filter(|&(_, &cnt)| cnt > 0)
                    .map(|(&sid, &cnt)| eve_db::snapshot_ext::CorpShareholderEntry {
                        shareholder_id: sid,
                        shareholder_type: if (98_000_000..99_000_000).contains(&sid) {
                            "corporation".into()
                        } else {
                            "character".into()
                        },
                        share_count: cnt,
                    })
                    .collect();
                holders.sort_by_key(|s| s.shareholder_id);
                snap.corp_shareholders.insert(corp_id, holders);
                Ok((old_f, new_f, old_t, new_t))
            });
            let (old_from, new_from, old_to, new_to) = match tx_res {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if target_id != char_id {
                let from_change = PyRep::Dict(vec![(
                    PyRep::String(("shares").into()),
                    PyRep::Tuple(vec![PyRep::Integer(old_from), PyRep::Integer(new_from)]),
                )]);
                ctx.notify(
                    NotificationTarget::Corporation(corp_id),
                    "OnShareChange",
                    "corpid",
                    vec![PyRep::Integer(char_id), PyRep::Integer(corp_id), from_change],
                );
            }
            let to_change = PyRep::Dict(vec![(
                PyRep::String(("shares").into()),
                PyRep::Tuple(vec![PyRep::Integer(old_to), PyRep::Integer(new_to)]),
            )]);
            let to_args = vec![PyRep::Integer(target_id), PyRep::Integer(corp_id), to_change];
            ctx.notify(NotificationTarget::Corporation(corp_id), "OnShareChange", "corpid", to_args.clone());
            let target_in_corp = ctx.db.with_snapshot(|snap| {
                snap.corporations.get(&corp_id).is_some_and(|c| c.members.contains(&target_id))
            });
            if !target_in_corp {
                ctx.notify(NotificationTarget::Character(target_id), "OnShareChange", "charid", to_args);
            }
            Some(Ok(PyRep::keyval([
                ("corporationID", PyRep::Integer(corp_id)),
                ("toShareholderID", PyRep::Integer(target_id)),
                ("sharesTransferred", PyRep::Integer(qty)),
            ])))
        }

        ("corpRegistry", "GetSharesByShareholder") => {
            let target_sh_id = if args.len() >= 2 {
                args.get(1).and_then(|a| a.as_i64()).unwrap_or(char_id)
            } else {
                args.first().and_then(|a| a.as_i64()).unwrap_or(char_id)
            };
            let rows: Vec<Vec<PyRep>> = with_corp_gov(ctx, |g| {
                let Some(state) = g.get(&corp_id) else {
                    return Vec::new();
                };
                state
                    .shares
                    .iter()
                    .filter(|(&oid, _)| oid == target_sh_id)
                    .map(|(&oid, &cnt)| vec![PyRep::Integer(oid), PyRep::Integer(corp_id), PyRep::Integer(cnt)])
                    .collect()
            });
            Some(Ok(PyRep::rowset(vec!["shareholderID", "corporationID", "shares"], rows)))
        }

        ("corpRegistry", "GetShareholders")
        | ("corpRegistry", "GetMemberIDsWithMoreThanAvgShares") => {
            let shares = with_corp_gov(ctx, |g| {
                g.get(&corp_id).map(|s| s.shares.clone()).unwrap_or_default()
            });
            if method == "GetMemberIDsWithMoreThanAvgShares" {
                if shares.is_empty() {
                    return Some(Ok(PyRep::List(Vec::new())));
                }
                let total: i64 = shares.values().sum();
                let avg = total / (shares.len().max(1) as i64);
                let ids: Vec<PyRep> = shares
                    .iter()
                    .filter(|(_, &cnt)| cnt >= avg)
                    .map(|(&oid, _)| PyRep::Integer(oid))
                    .collect();
                return Some(Ok(PyRep::List(ids)));
            }
            let rows: Vec<Vec<PyRep>> = shares
                .iter()
                .map(|(&oid, &cnt)| vec![PyRep::Integer(oid), PyRep::Integer(corp_id), PyRep::Integer(cnt)])
                .collect();
            Some(Ok(PyRep::rowset(vec!["shareholderID", "corporationID", "shares"], rows)))
        }

        ("corpRegistry", "PayoutDividend") => {
            let total_isk = args.first().and_then(|a| a.as_f64()).unwrap_or(0.0);
            let to_shareholders = matches!(args.get(1), Some(PyRep::Bool(true)) | Some(PyRep::Integer(1)));
            let div_id = args.get(2).and_then(|a| a.as_i64()).unwrap_or(1) as u8;
            if total_isk <= 0.0 || !total_isk.is_finite() {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "Dividend payout amount must be positive".into(),
                }));
            }
            let payout_cents = IskCents::from_f64_isk(total_isk);

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(corp) = snap.corporations.get_mut(&corp_id) else {
                    return Err(BridgeRpcError {
                        code: "CorporationNotFound".into(),
                        message: format!("Corporation {corp_id} not found"),
                    });
                };
                let caller_role = corp.member_roles.get(&char_id).copied().unwrap_or(0);
                if char_id != corp.ceo_id && !corp_roles::has_role(caller_role, corp_roles::ROLE_DIRECTOR) {
                    return Err(BridgeRpcError {
                        code: "CorpRoleDenied".into(),
                        message: "Paying dividends requires CEO or Director role".into(),
                    });
                }
                corp.ensure_wallet_divisions();
                let dpos = corp
                    .wallet_divisions
                    .iter()
                    .position(|d| d.division_id == div_id)
                    .unwrap_or(0);
                let cur_bal = corp.wallet_divisions[dpos].balance;
                let new_bal = cur_bal.checked_sub(payout_cents).ok_or_else(|| BridgeRpcError {
                    code: "InsufficientDivisionFunds".into(),
                    message: "Insufficient corporation wallet funds for dividend payout".into(),
                })?;
                corp.wallet_divisions[dpos].balance = new_bal;
                if div_id == 1 {
                    corp.master_wallet = new_bal;
                }
                let member_ids: Vec<i64> = corp.members.iter().copied().collect();

                let recipients: Vec<(i64, i64)> = if to_shareholders {
                    let state = snap.corp_governance.entry(corp_id).or_default();
                    let shares_snapshot = state.shares.clone();
                    let total_shares: i64 = shares_snapshot.values().sum::<i64>().max(1);
                    shares_snapshot
                        .into_iter()
                        .filter(|(_, s)| *s > 0)
                        .map(|(oid, s)| (oid, (payout_cents.0 * s) / total_shares))
                        .collect()
                } else {
                    let mcount = (member_ids.len() as i64).max(1);
                    member_ids
                        .into_iter()
                        .map(|mid| (mid, payout_cents.0 / mcount))
                        .collect()
                };

                for &(rid, cents) in &recipients {
                    if let Some(ch) = snap.characters.get_mut(&rid) {
                        ch.balance = IskCents(ch.balance.0 + cents);
                        let new_ch_bal = ch.balance;
                        snap.next_entity_id += 1;
                        let jid = snap.next_entity_id;
                        snap.wallet_journal.push(WalletJournalEntry {
                            entry_id: jid,
                            character_id: rid,
                            entry_type: "CorporationDividendPayment".into(),
                            amount: IskCents(cents),
                            balance_after: new_ch_bal,
                            reason: format!("Dividend payout from corporation {corp_id}"),
                        });
                    }
                }
                Ok(recipients.len() as i64)
            });

            match res {
                Ok(cnt) => Some(Ok(PyRep::keyval([
                    ("corporationID", PyRep::Integer(corp_id)),
                    ("recipientCount", PyRep::Integer(cnt)),
                    ("totalPayoutISK", PyRep::Real(total_isk)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        ("corpRegistry", "UpdateTitle") | ("corpRegistry", "UpdateTitles") => {
            if !has_director { return deny_role("Updating corporation titles requires CEO or Director role"); }
            let title_id = args.first().and_then(|a| a.as_i64()).unwrap_or(1).clamp(1, 16);
            let title_name = args.get(1).and_then(|a| a.as_str()).unwrap_or("Squad Leader");
            let roles_mask = args.get(2).and_then(|a| a.as_i64()).unwrap_or(0) as u64;
            let grantable_mask = args.get(3).and_then(|a| a.as_i64()).unwrap_or(0) as u64;
            let prev = ctx.db.with_mut_snapshot(|snap| {
                let prev = snap.corp_governance.entry(corp_id).or_default().titles.insert(title_id, (title_name.to_string(), roles_mask));
                let dec = |m: u64| -> Vec<String> {
                    crate::esi::corp_governance_routes::decode_corp_roles(m).into_iter().map(String::from).collect()
                };
                let titles = snap.corp_titles.entry(corp_id).or_default();
                titles.retain(|t| t.title_id != title_id);
                titles.push(eve_db::snapshot_ext::CorpTitleEntry {
                    title_id,
                    name: title_name.to_string(),
                    roles: dec(roles_mask),
                    grantable_roles: dec(grantable_mask),
                    ..Default::default()
                });
                prev
            });
            let (old_name, old_roles) = prev.map_or((PyRep::None, PyRep::Integer(0)), |(n, r)| (PyRep::String(n), PyRep::Integer(r as i64)));
            let change = PyRep::Dict(vec![
                (PyRep::String(("titleName").into()), PyRep::Tuple(vec![old_name, PyRep::String(title_name.to_string())])),
                (PyRep::String(("roles").into()), PyRep::Tuple(vec![old_roles, PyRep::Integer(roles_mask as i64)])),
            ]);
            ctx.notify(NotificationTarget::Corporation(corp_id), "OnTitleChanged", "corpid", vec![PyRep::Integer(corp_id), PyRep::Integer(title_id), change]);
            Some(Ok(PyRep::keyval([
                ("titleID", PyRep::Integer(title_id)),
                ("titleName", PyRep::String(title_name.to_string())),
                ("roles", PyRep::Integer(roles_mask as i64)),
            ])))
        }

        ("corpRegistry", "GetTitles") => {
            let rows: Vec<Vec<PyRep>> = read_corp_gov(ctx, |g| {
                g.get(&corp_id)
                    .map(|g| {
                        g.titles
                            .iter()
                            .map(|(&tid, (tname, rmask))| vec![PyRep::Integer(tid), PyRep::String(tname.clone()), PyRep::Integer(*rmask as i64)])
                            .collect()
                    })
                    .unwrap_or_default()
            });
            Some(Ok(PyRep::rowset(vec!["titleID", "titleName", "roles"], rows)))
        }

        ("corpRegistry", "DeleteTitle") => {
            if !has_director { return deny_role("Deleting corporation titles requires CEO or Director role"); }
            let title_id = args.first().and_then(|a| a.as_i64()).unwrap_or(1);
            let prev = ctx.db.with_mut_snapshot(|snap| {
                if let Some(titles) = snap.corp_titles.get_mut(&corp_id) {
                    titles.retain(|t| t.title_id != title_id);
                }
                snap.corp_governance.get_mut(&corp_id).and_then(|s| s.titles.remove(&title_id))
            });
            let (old_name, old_roles) = prev.map_or((PyRep::None, PyRep::Integer(0)), |(n, r)| (PyRep::String(n), PyRep::Integer(r as i64)));
            let change = PyRep::Dict(vec![
                (PyRep::String(("titleName").into()), PyRep::Tuple(vec![old_name, PyRep::String(("").into())])),
                (PyRep::String(("roles").into()), PyRep::Tuple(vec![old_roles, PyRep::Integer(0)])),
            ]);
            ctx.notify(NotificationTarget::Corporation(corp_id), "OnTitleChanged", "corpid", vec![PyRep::Integer(corp_id), PyRep::Integer(title_id), change]);
            Some(Ok(PyRep::keyval([("titleID", PyRep::Integer(title_id)), ("deleted", PyRep::Bool(true))])))
        }

        ("corpRegistry", "ResignFromCEO") => {
            let new_ceo_opt = args.first().and_then(|a| a.as_i64());
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(corp) = snap.corporations.get_mut(&corp_id) else {
                    return Err(BridgeRpcError { code: "CorporationNotFound".into(), message: format!("Corporation {corp_id} not found") });
                };
                if corp.ceo_id != char_id {
                    return Err(BridgeRpcError { code: "CorpRoleDenied".into(), message: "Only the current CEO can resign".into() });
                }
                let next_ceo = new_ceo_opt.filter(|id| corp.members.contains(id)).unwrap_or(0);
                corp.ceo_id = next_ceo;
                if next_ceo > 0 {
                    corp.member_roles.insert(next_ceo, u64::MAX);
                }
                Ok(next_ceo)
            });
            match res {
                Ok(next_ceo) => {
                    let change = PyRep::Dict(vec![(
                        PyRep::String(("ceoID").into()),
                        PyRep::Tuple(vec![PyRep::Integer(char_id), PyRep::Integer(next_ceo)]),
                    )]);
                    ctx.notify(NotificationTarget::Corporation(corp_id), "OnCorporationChanged", "corpid", vec![PyRep::Integer(corp_id), change]);
                    Some(Ok(PyRep::keyval([("corporationID", PyRep::Integer(corp_id)), ("newCeoID", PyRep::Integer(next_ceo))])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("corpRegistry", "GetNumberOfPotentialCEOs") => {
            let count = ctx.db.with_snapshot(|snap| {
                snap.corporations.get(&corp_id).map(|c| c.members.len().saturating_sub(1) as i64).unwrap_or(0)
            });
            Some(Ok(PyRep::Integer(count)))
        }

        ("voteManager", "CanRunForCEO") | ("voteManager", "CanViewVotes") => {
            let has_skill = ctx.db.with_snapshot(|snap| {
                snap.skills.get(&char_id).and_then(|m| m.get(&3363)).is_some_and(|s| s.level >= 1)
            });
            Some(Ok(PyRep::Bool(has_skill)))
        }

        ("voteManager", "InsertVoteCase") | ("corpRegistry", "InsertVoteCase") => {
            let text = args.first().and_then(|a| a.as_str()).unwrap_or("CEO Election");
            let desc = args.get(1).and_then(|a| a.as_str()).unwrap_or("Vote for new CEO");
            let vote_type = args.get(2).and_then(|a| a.as_i64()).unwrap_or(0);
            let target_id = args.get(3).and_then(|a| a.as_i64()).unwrap_or(char_id);
            let case_id = match ctx.db.try_with_mut_snapshot(|snap| -> Result<i64, BridgeRpcError> {
                if vote_type == 0 && !snap.corporations.get(&corp_id).map(|c| c.members.contains(&target_id)).unwrap_or(false) {
                    return Err(BridgeRpcError { code: "CandidateNotMember".into(), message: "CEO election candidate must be a member of the corporation".into() });
                }
                snap.next_entity_id += 1;
                let cid = snap.next_entity_id;
                snap.corp_vote_cases.insert(cid, CorpVoteCaseRecord {
                    case_id: cid, corporation_id: corp_id, vote_type, text: text.to_string(), description: desc.to_string(), parameter: target_id, start_ts: now_unix, end_ts: now_unix + 86_400, executed: false,
                });
                Ok(cid)
            }) {
                Ok(cid) => cid,
                Err(e) => return Some(Err(e)),
            };
            ctx.notify(NotificationTarget::Corporation(corp_id), "OnVoteCaseChanged", "corpid", vec![PyRep::Integer(corp_id), PyRep::Integer(case_id)]);
            ctx.notify(NotificationTarget::Corporation(corp_id), "OnSanctionedActionChanged", "corpid", vec![PyRep::Integer(corp_id), PyRep::Integer(case_id)]);
            Some(Ok(PyRep::keyval([("voteCaseID", PyRep::Integer(case_id)), ("voteType", PyRep::Integer(vote_type)), ("parameter", PyRep::Integer(target_id))])))
        }

        ("voteManager", "InsertVote") | ("corpRegistry", "InsertVote") => {
            let case_id = args.first().and_then(|a| a.as_i64()).unwrap_or(1);
            let option_id = args.get(1).and_then(|a| a.as_i64()).unwrap_or(1);
            let weight = match ctx.db.try_with_mut_snapshot(|snap| -> Result<i64, BridgeRpcError> {
                let vc = snap.corp_vote_cases.get(&case_id).ok_or_else(|| BridgeRpcError { code: "VoteCaseNotFound".into(), message: format!("Vote case {case_id} not found") })?;
                if now_unix > vc.end_ts { return Err(BridgeRpcError { code: "VoteCaseClosed".into(), message: "Voting period has closed for this case".into() }); }
                let w = snap.corp_shareholders.get(&corp_id).and_then(|shs| shs.iter().find(|s| s.shareholder_id == char_id).map(|s| s.share_count)).unwrap_or(1).max(1);
                snap.corp_vote_ballots.insert((case_id, char_id), CorpVoteBallotRecord { case_id, voter_id: char_id, option_id, weight: w, voted_at_unix: now_unix });
                Ok(w)
            }) {
                Ok(w) => w,
                Err(e) => return Some(Err(e)),
            };
            ctx.notify(NotificationTarget::Corporation(corp_id), "OnVoteCast", "corpid", vec![PyRep::Integer(corp_id), PyRep::Integer(case_id)]);
            Some(Ok(PyRep::keyval([("voteCaseID", PyRep::Integer(case_id)), ("optionID", PyRep::Integer(option_id)), ("weight", PyRep::Integer(weight))])))
        }

        ("voteManager", "GetVotes")
        | ("voteManager", "GetVoteCase")
        | ("voteManager", "GetVoteCasesByCorporation")
        | ("voteManager", "GetVoteCaseOptions")
        | ("voteManager", "GetCorpSanctionedActions") => {
            let target_corp = args.first().and_then(|a| a.as_i64()).unwrap_or(corp_id);
            if method == "GetVoteCaseOptions" {
                let case_id = args.get(1).or_else(|| args.first()).and_then(|a| a.as_i64()).unwrap_or(1);
                return Some(Ok(PyRep::rowset(vec!["optionID", "voteCaseID", "optionText", "parameter"], vec![
                    vec![PyRep::Integer(1), PyRep::Integer(case_id), PyRep::String("Yes / Accept".into()), PyRep::Integer(1)],
                    vec![PyRep::Integer(2), PyRep::Integer(case_id), PyRep::String("No / Reject".into()), PyRep::Integer(0)],
                ])));
            }
            if method == "GetVotes" {
                let case_id = args.get(1).or_else(|| args.first()).and_then(|a| a.as_i64()).unwrap_or(1);
                let rows = ctx.db.with_snapshot(|snap| {
                    snap.corp_vote_ballots.values().filter(|b| b.case_id == case_id)
                        .map(|b| vec![PyRep::Integer(b.voter_id), PyRep::Integer(b.case_id), PyRep::Integer(b.option_id), PyRep::Integer(b.weight)]).collect::<Vec<_>>()
                });
                return Some(Ok(PyRep::rowset(vec!["characterID", "voteCaseID", "optionID", "weight"], rows)));
            }
            let filter_cid = if method == "GetVoteCase" { args.first().and_then(|a| a.as_i64()) } else { None };
            let rows = ctx.db.with_snapshot(|snap| {
                snap.corp_vote_cases.values().filter(|c| {
                    if let Some(fcid) = filter_cid { return fcid == c.case_id; }
                    if c.corporation_id != target_corp { return false; }
                    if method == "GetCorpSanctionedActions" {
                        if c.executed { return false; }
                        let yes: i64 = snap.corp_vote_ballots.values().filter(|b| b.case_id == c.case_id && b.option_id == 1).map(|b| b.weight).sum();
                        let no: i64 = snap.corp_vote_ballots.values().filter(|b| b.case_id == c.case_id && b.option_id != 1).map(|b| b.weight).sum();
                        return yes > no && now_unix >= c.end_ts;
                    }
                    true
                }).map(|c| {
                    let yes: i64 = snap.corp_vote_ballots.values().filter(|b| b.case_id == c.case_id && b.option_id == 1).map(|b| b.weight).sum();
                    vec![PyRep::Integer(c.case_id), PyRep::String(c.text.clone()), PyRep::String(c.description.clone()), PyRep::Integer(c.vote_type), PyRep::Integer(c.parameter), PyRep::Integer(c.end_ts as i64), PyRep::Integer(yes), PyRep::Bool(c.executed)]
                }).collect::<Vec<_>>()
            });
            Some(Ok(PyRep::rowset(vec!["voteCaseID", "voteCaseText", "description", "voteType", "parameter", "endDateTime", "votesFor", "inEffect"], rows)))
        }

        ("voteManager", "ActivateSanctionedAction") => {
            if !has_director { return deny_role("Activating sanctioned actions requires CEO or Director role"); }
            let case_id = args.first().and_then(|a| a.as_i64()).unwrap_or(1);
            let mut old_ceo_id = char_id;
            let mut new_ceo_id = char_id;
            let mut is_ceo_vote = false;
            let mut share_delta: Option<(i64, i64)> = None;
            let res = ctx.db.try_with_mut_snapshot(|snap| -> Result<(), BridgeRpcError> {
                let vc = snap.corp_vote_cases.get_mut(&case_id).ok_or_else(|| BridgeRpcError { code: "VoteCaseNotFound".into(), message: format!("Vote case {case_id} not found") })?;
                if vc.executed { return Err(BridgeRpcError { code: "VoteAlreadyActivated".into(), message: "Sanctioned action has already been activated".into() }); }
                if now_unix < vc.end_ts { return Err(BridgeRpcError { code: "VoteStillOpen".into(), message: "Cannot activate sanctioned action before voting closes".into() }); }
                let yes: i64 = snap.corp_vote_ballots.values().filter(|b| b.case_id == case_id && b.option_id == 1).map(|b| b.weight).sum();
                let no: i64 = snap.corp_vote_ballots.values().filter(|b| b.case_id == case_id && b.option_id != 1).map(|b| b.weight).sum();
                if yes <= no || yes == 0 { return Err(BridgeRpcError { code: "VoteDidNotPass".into(), message: "Sanctioned action did not pass shareholder vote".into() }); }
                vc.executed = true;
                if vc.vote_type == 5 {
                    let state = snap.corp_governance.entry(corp_id).or_default();
                    state.company_shares += vc.parameter;
                    let entry = state.shares.entry(char_id).or_insert(0);
                    let old_s = *entry;
                    *entry += vc.parameter;
                    share_delta = Some((old_s, *entry));
                    let shareholders = snap.corp_shareholders.entry(corp_id).or_default();
                    if let Some(sh) = shareholders.iter_mut().find(|s| s.shareholder_id == char_id) {
                        sh.share_count += vc.parameter;
                    } else {
                        shareholders.push(eve_db::snapshot_ext::CorpShareholderEntry {
                            shareholder_id: char_id,
                            shareholder_type: "character".into(),
                            share_count: vc.parameter,
                        });
                    }
                }
                if vc.vote_type == 0 {
                    is_ceo_vote = true;
                    new_ceo_id = vc.parameter;
                    let corp = snap.corporations.get_mut(&corp_id).ok_or_else(|| BridgeRpcError { code: "CorpNotFound".into(), message: "Corporation not found".into() })?;
                    if !corp.members.contains(&new_ceo_id) {
                        return Err(BridgeRpcError { code: "CandidateNotMember".into(), message: "CEO election candidate is not a member of the corporation".into() });
                    }
                    old_ceo_id = corp.ceo_id;
                    corp.ceo_id = new_ceo_id;
                    corp.member_roles.insert(new_ceo_id, u64::MAX);
                }
                Ok(())
            });
            match res {
                Ok(()) => {
                    ctx.notify(NotificationTarget::Corporation(corp_id), "OnSanctionedActionChanged", "corpid", vec![PyRep::Integer(corp_id), PyRep::Integer(case_id)]);
                    if let Some((old_s, new_s)) = share_delta {
                        let s_change = PyRep::Dict(vec![(PyRep::String(("shares").into()), PyRep::Tuple(vec![PyRep::Integer(old_s), PyRep::Integer(new_s)]))]);
                        ctx.notify(NotificationTarget::Corporation(corp_id), "OnShareChange", "corpid", vec![PyRep::Integer(char_id), PyRep::Integer(corp_id), s_change]);
                    }
                    if is_ceo_vote {
                        let change = PyRep::Dict(vec![(PyRep::String(("ceoID").into()), PyRep::Tuple(vec![PyRep::Integer(old_ceo_id), PyRep::Integer(new_ceo_id)]))]);
                        ctx.notify(NotificationTarget::Corporation(corp_id), "OnCorporationChanged", "corpid", vec![PyRep::Integer(corp_id), change]);
                    }
                    Some(Ok(PyRep::keyval([("voteCaseID", PyRep::Integer(case_id)), ("activated", PyRep::Bool(true))])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("corporationSvc", "CreateMedal") => {
            let title = args.first().and_then(|a| a.as_str()).unwrap_or("Valorous Service Medal");
            let desc = args.get(1).and_then(|a| a.as_str()).unwrap_or("Awarded for fleet valor");
            let medal_fee = IskCents::from_isk(5_000_000);
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(ch) = snap.characters.get_mut(&char_id) else {
                    return Err(BridgeRpcError { code: "CharacterNotFound".into(), message: "Character not found".into() });
                };
                ch.balance = ch.balance.checked_sub(medal_fee).ok_or_else(|| BridgeRpcError {
                    code: "InsufficientFunds".into(),
                    message: "Creating a corporation medal costs 5,000,000 ISK".into(),
                })?;
                let state = snap.corp_governance.entry(corp_id).or_default();
                let mid = (state.medals.len() as i64) + 1;
                state.medals.push((mid, title.to_string(), desc.to_string(), char_id, now_unix));
                snap.corp_medals.entry(corp_id).or_default().push(eve_db::snapshot_ext::CorpMedalEntry {
                    medal_id: mid,
                    title: title.to_string(),
                    description: desc.to_string(),
                    creator_id: char_id,
                    created_at_unix: now_unix,
                });
                Ok(mid)
            });
            let medal_id = match res {
                Ok(mid) => mid,
                Err(e) => return Some(Err(e)),
            };
            ctx.notify(NotificationTarget::Corporation(corp_id), "OnCorporationMedalAdded", "corpid", vec![PyRep::Integer(medal_id), PyRep::Integer(corp_id)]);
            Some(Ok(PyRep::keyval([
                ("medalID", PyRep::Integer(medal_id)),
                ("title", PyRep::String(title.to_string())),
                ("description", PyRep::String(desc.to_string())),
            ])))
        }

        ("corporationSvc", "GiveMedalToCharacters") => {
            let medal_id = args.first().and_then(|a| a.as_i64()).unwrap_or(1);
            let recipient_id = match args.get(1) {
                Some(PyRep::Integer(id)) => *id,
                Some(PyRep::List(items)) | Some(PyRep::Tuple(items)) => items.first().and_then(|v| v.as_i64()).unwrap_or(char_id),
                _ => char_id,
            };
            let reason = args.get(2).and_then(|a| a.as_str()).unwrap_or("Combat distinction");
            ctx.db.with_mut_snapshot(|snap| {
                let state = snap.corp_governance.entry(corp_id).or_default();
                let (m_title, m_desc) = state.medals.iter().find(|m| m.0 == medal_id).map(|m| (m.1.clone(), m.2.clone())).unwrap_or_else(|| ("Medal".into(), String::new()));
                state.medal_recipients.push((medal_id, recipient_id, char_id, reason.to_string(), 2));
                snap.corp_issued_medals.entry(corp_id).or_default().push(eve_db::snapshot_ext::CorpIssuedMedalEntry {
                    medal_id, character_id: recipient_id, reason: reason.to_string(), status: "private".into(), issuer_id: char_id, issued_at_unix: now_unix,
                });
                snap.char_medals.entry(recipient_id).or_default().push(eve_db::snapshot_ext::CharacterMedalRecord {
                    medal_id, title: m_title, description: m_desc, corporation_id: corp_id, issuer_id: char_id, date_unix: now_unix, reason: reason.to_string(), status: "private".into(), graphics: Vec::new(),
                });
            });
            ctx.notify(NotificationTarget::Corporation(corp_id), "OnCorporationMedalAdded", "corpid", vec![PyRep::Integer(medal_id), PyRep::Integer(recipient_id)]);
            ctx.notify(NotificationTarget::Character(recipient_id), "OnMedalIssued", "charid", vec![PyRep::Integer(medal_id), PyRep::Integer(corp_id), PyRep::Integer(char_id)]);
            Some(Ok(PyRep::keyval([("medalID", PyRep::Integer(medal_id)), ("recipientID", PyRep::Integer(recipient_id)), ("status", PyRep::Integer(2))])))
        }

        ("corporationSvc", "SetMedalStatus") => {
            let medal_id = args.first().and_then(|a| a.as_i64()).unwrap_or(1);
            let new_status = args.get(1).and_then(|a| a.as_i64()).unwrap_or(3);
            let status_str = if new_status >= 3 { "public" } else { "private" };
            ctx.db.with_mut_snapshot(|snap| {
                for state in snap.corp_governance.values_mut() {
                    for rec in &mut state.medal_recipients {
                        if rec.0 == medal_id && rec.1 == char_id { rec.4 = new_status; }
                    }
                }
                for list in snap.corp_issued_medals.values_mut() {
                    for m in list {
                        if m.medal_id == medal_id && m.character_id == char_id { m.status = status_str.into(); }
                    }
                }
                if let Some(list) = snap.char_medals.get_mut(&char_id) {
                    for m in list {
                        if m.medal_id == medal_id { m.status = status_str.into(); }
                    }
                }
            });
            ctx.notify(NotificationTarget::Character(char_id), "OnMedalStatusChanged", "charid", vec![PyRep::Integer(medal_id), PyRep::Integer(new_status)]);
            Some(Ok(PyRep::keyval([("medalID", PyRep::Integer(medal_id)), ("status", PyRep::Integer(new_status))])))
        }

        ("corporationSvc", "GetAllCorpMedals")
        | ("corporationSvc", "GetMedalDetails")
        | ("corporationSvc", "GetMedalsReceived")
        | ("corporationSvc", "GetRecipientsOfMedal") => {
            let target_char = args.first().and_then(|a| a.as_i64()).unwrap_or(char_id);
            if method == "GetMedalsReceived" || method == "GetRecipientsOfMedal" {
                let rows = read_corp_gov(ctx, |g| {
                    let mut rows = Vec::new();
                    for (&cid, state) in g.iter() {
                        for (mid, rid, iid, reason, status) in &state.medal_recipients {
                            if (method == "GetMedalsReceived" && *rid == target_char && *status > 1)
                                || (method == "GetRecipientsOfMedal" && *mid == target_char && cid == corp_id)
                            {
                                let title = state.medals.iter().find(|m| m.0 == *mid).map(|m| m.1.clone()).unwrap_or_else(|| "Medal".into());
                                rows.push(vec![PyRep::Integer(*mid), PyRep::Integer(*rid), PyRep::Integer(*iid), PyRep::Integer(cid), PyRep::String(title), PyRep::String(reason.clone()), PyRep::Integer(*status)]);
                            }
                        }
                    }
                    rows
                });
                return Some(Ok(PyRep::rowset(vec!["medalID", "wearerID", "issuerID", "corporationID", "title", "reason", "status"], rows)));
            }
            let rows: Vec<Vec<PyRep>> = read_corp_gov(ctx, |g| {
                g.get(&corp_id)
                    .map(|s| s.medals.iter().map(|(mid, title, desc, creator, ts)| vec![PyRep::Integer(*mid), PyRep::String(title.clone()), PyRep::String(desc.clone()), PyRep::Integer(*creator), PyRep::Integer(*ts as i64)]).collect())
                    .unwrap_or_default()
            });
            Some(Ok(PyRep::rowset(vec!["medalID", "title", "description", "creatorID", "date"], rows)))
        }

        ("corporationSvc", "GetEmploymentRecord") => {
            let target_id = args.first().and_then(|a| a.as_i64()).unwrap_or(char_id);
            let mut rows = Vec::new();
            ctx.db.with_snapshot(|snap| {
                if let Some(list) = snap.employment_history.get(&target_id).filter(|l| !l.is_empty()) {
                    for &(cid, start_ts, end_ts) in list {
                        rows.push(vec![PyRep::Integer(cid), PyRep::Integer(start_ts as i64), end_ts.map(|e| PyRep::Integer(e as i64)).unwrap_or(PyRep::None)]);
                    }
                } else {
                    let (cid, start_ts) = snap.characters.get(&target_id).map(|c| (c.corporation_id, c.create_date_unix)).unwrap_or((corp_id, 0));
                    rows.push(vec![PyRep::Integer(cid), PyRep::Integer(start_ts as i64), PyRep::None]);
                }
            });
            Some(Ok(PyRep::rowset(vec!["corporationID", "startDate", "endDate"], rows)))
        }

        ("corpRegistry", "AddBulletin") => {
            if !has_comms { return deny_role("Managing corporation bulletins requires Communications Officer or Director role"); }
            let title = args.first().and_then(|a| a.as_str()).unwrap_or("Corporation Bulletin");
            let body = args.get(1).and_then(|a| a.as_str()).unwrap_or("All hands on deck");
            let is_alliance = matches!(args.get(2), Some(PyRep::Bool(true)));
            let owner_key = if is_alliance { session.alliance_id.unwrap_or(corp_id) } else { corp_id };
            let bid = with_corp_gov(ctx, |g| {
                let state = g.entry(owner_key).or_default();
                let bid = (state.bulletins.len() as i64) + 1;
                state.bulletins.push((bid, title.to_string(), body.to_string(), char_id, now_unix));
                bid
            });
            Some(Ok(PyRep::keyval([
                ("bulletinID", PyRep::Integer(bid)),
                ("title", PyRep::String(title.to_string())),
                ("body", PyRep::String(body.to_string())),
            ])))
        }

        ("corpRegistry", "UpdateBulletinOrder") => {
            if !has_comms { return deny_role("Managing corporation bulletins requires Communications Officer or Director role"); }
            let order_ids: Vec<i64> = match args.first() {
                Some(PyRep::List(l)) | Some(PyRep::Tuple(l)) => l.iter().filter_map(|x| x.as_i64()).collect(),
                _ => args.iter().filter_map(|x| x.as_i64()).collect(),
            };
            let is_alliance = matches!(args.get(1), Some(PyRep::Bool(true)));
            let owner_key = if is_alliance { session.alliance_id.unwrap_or(corp_id) } else { corp_id };
            with_corp_gov(ctx, |g| {
                let state = g.entry(owner_key).or_default();
                if !order_ids.is_empty() {
                    state.bulletins.sort_by_key(|(bid, ..)| {
                        order_ids.iter().position(|id| id == bid).unwrap_or(usize::MAX)
                    });
                }
            });
            Some(Ok(PyRep::Bool(true)))
        }

        ("corpRegistry", "GetBulletins") => {
            let owner_key = args.first().and_then(|a| a.as_i64()).unwrap_or(corp_id);
            let rows: Vec<Vec<PyRep>> = read_corp_gov(ctx, |g| {
                g.get(&owner_key)
                    .map(|s| {
                        s.bulletins
                            .iter()
                            .map(|(bid, title, body, aid, ts)| {
                                vec![
                                    PyRep::Integer(*bid),
                                    PyRep::String(title.clone()),
                                    PyRep::String(body.clone()),
                                    PyRep::Integer(*aid),
                                    PyRep::Integer(*ts as i64),
                                ]
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            });
            Some(Ok(PyRep::rowset(vec!["bulletinID", "title", "body", "editCharacterID", "editDateTime"], rows)))
        }

        ("corpRegistry", "DeleteBulletin") => {
            if !has_comms { return deny_role("Deleting corporation bulletins requires Communications Officer or Director role"); }
            let bid = args.first().and_then(|a| a.as_i64()).unwrap_or(1);
            with_corp_gov(ctx, |g| {
                if let Some(s) = g.get_mut(&corp_id) {
                    s.bulletins.retain(|b| b.0 != bid);
                }
            });
            Some(Ok(PyRep::keyval([("bulletinID", PyRep::Integer(bid)), ("deleted", PyRep::Bool(true))])))
        }

        ("corpRecProxy", "CreateRecruitmentAd") | ("corpRecProxy", "UpdateRecruitmentAd") => {
            let has_role = ctx.db.with_snapshot(|snap| {
                snap.corporations.get(&corp_id).is_none_or(|c| {
                    let r = c.member_roles.get(&char_id).copied().unwrap_or(0);
                    char_id == c.ceo_id
                        || corp_roles::has_role(r, corp_roles::ROLE_PERSONNEL_MANAGER)
                })
            });
            if !has_role {
                return Some(Err(BridgeRpcError {
                    code: "CorpRoleDenied".into(),
                    message: "Managing recruitment ads requires Personnel Manager or Director role"
                        .into(),
                }));
            }
            if method == "CreateRecruitmentAd" {
                let ad_fee = IskCents::from_isk(250_000);
                let _ = ctx.db.try_with_mut_snapshot(|snap| -> Result<(), BridgeRpcError> {
                    if let Some(corp) = snap.corporations.get_mut(&corp_id) {
                        corp.ensure_wallet_divisions();
                        if corp.wallet_divisions[0].balance >= ad_fee {
                            corp.wallet_divisions[0].balance =
                                corp.wallet_divisions[0].balance.checked_sub(ad_fee).unwrap();
                            corp.master_wallet = corp.wallet_divisions[0].balance;
                        } else if let Some(ch) = snap.characters.get_mut(&char_id) {
                            if let Some(nb) = ch.balance.checked_sub(ad_fee) {
                                ch.balance = nb;
                            }
                        }
                    }
                    Ok(())
                });
            }
            let (title, desc, type_mask) = if args.first().and_then(|a| a.as_str()).is_some() {
                let title = args.first().and_then(|a| a.as_str()).unwrap_or("Nullsec PvP & Industry");
                let desc = args.get(1).and_then(|a| a.as_str()).unwrap_or("Join our corp today");
                let type_mask = args.get(2).and_then(|a| a.as_i64()).unwrap_or(7) as u64;
                (title, desc, type_mask)
            } else {
                let type_mask = args.get(1).and_then(|a| a.as_i64()).unwrap_or(7) as u64;
                let title = args.get(2).and_then(|a| a.as_str()).unwrap_or("Updated Corp Ad");
                let desc = args.get(3).and_then(|a| a.as_str()).unwrap_or("Updated description");
                (title, desc, type_mask)
            };
            let ad_id = with_corp_gov(ctx, |g| {
                let state = g.entry(corp_id).or_default();
                if method == "UpdateRecruitmentAd" {
                    let id = args.first().and_then(|a| a.as_i64()).unwrap_or(1);
                    if let Some(ad) = state.recruitment_ads.iter_mut().find(|a| a.0 == id) {
                        ad.1 = title.to_string();
                        ad.2 = desc.to_string();
                        ad.3 = type_mask;
                    }
                    id
                } else {
                    let id = (state.recruitment_ads.len() as i64) + 1;
                    state
                        .recruitment_ads
                        .push((id, title.to_string(), desc.to_string(), type_mask, char_id));
                    id
                }
            });
            ctx.notify(
                NotificationTarget::Corporation(corp_id),
                "OnCorporationRecruitmentAdChanged",
                "corpid",
                vec![PyRep::Integer(corp_id), PyRep::Integer(ad_id)],
            );
            Some(Ok(PyRep::keyval([
                ("adID", PyRep::Integer(ad_id)),
                ("corporationID", PyRep::Integer(corp_id)),
                ("title", PyRep::String(title.to_string())),
                ("typeMask", PyRep::Integer(type_mask as i64)),
            ])))
        }

        ("corpRecProxy", "GetRecruiters") => {
            let target_corp = args.first().and_then(|a| a.as_i64()).unwrap_or(corp_id);
            let target_ad = args.get(1).and_then(|a| a.as_i64()).unwrap_or(1);
            let recruiters = read_corp_gov(ctx, |g| {
                let mut recruiters = Vec::new();
                if let Some(state) = g.get(&target_corp) {
                    if let Some((_, _, _, _, rec_id)) = state.recruitment_ads.iter().find(|a| a.0 == target_ad) {
                        recruiters.push(PyRep::Integer(*rec_id));
                    }
                }
                recruiters
            });
            Some(Ok(PyRep::List(recruiters)))
        }

        ("corpRecProxy", "GetRecruitmentAdsForCorporation")
        | ("corpRecProxy", "GetRecruitmentAdsByCorpID")
        | ("corpRecProxy", "GetRecruitmentAd")
        | ("corpRecProxy", "SearchCorpAds") => {
            let filter_corp = match method {
                "GetRecruitmentAdsByCorpID" => args.first().and_then(|a| a.as_i64()).or(Some(corp_id)),
                "GetRecruitmentAdsForCorporation" => Some(corp_id),
                _ => None,
            };
            let filter_ad = if method == "GetRecruitmentAd" {
                args.first().and_then(|a| a.as_i64())
            } else {
                None
            };
            let filter_mask = if method == "SearchCorpAds" {
                args.first().and_then(|a| a.as_i64()).unwrap_or(0) as u64
            } else {
                0
            };
            let rows = read_corp_gov(ctx, |g| {
                let mut rows = Vec::new();
                for (&cid, state) in g.iter() {
                    if filter_corp.is_none_or(|fc| fc == cid) {
                        for (aid, title, desc, mask, rec_id) in &state.recruitment_ads {
                            if filter_ad.is_some_and(|fa| fa != *aid) {
                                continue;
                            }
                            if filter_mask != 0 && (*mask & filter_mask) == 0 {
                                continue;
                            }
                            rows.push(vec![
                                PyRep::Integer(*aid),
                                PyRep::Integer(cid),
                                PyRep::String(title.clone()),
                                PyRep::String(desc.clone()),
                                PyRep::Integer(*mask as i64),
                                PyRep::Integer(*rec_id),
                            ]);
                        }
                    }
                }
                rows
            });
            Some(Ok(PyRep::rowset(
                vec!["adID", "corporationID", "title", "description", "typeMask", "recruiterID"],
                rows,
            )))
        }

        ("corpRecProxy", "DeleteRecruitmentAd") => {
            let has_role = ctx.db.with_snapshot(|snap| {
                snap.corporations.get(&corp_id).is_none_or(|c| {
                    let r = c.member_roles.get(&char_id).copied().unwrap_or(0);
                    char_id == c.ceo_id || corp_roles::has_role(r, corp_roles::ROLE_PERSONNEL_MANAGER)
                })
            });
            if !has_role {
                return deny_role("Deleting recruitment ads requires Personnel Manager or Director role");
            }
            let ad_id = args.first().and_then(|a| a.as_i64()).unwrap_or(1);
            with_corp_gov(ctx, |g| {
                if let Some(s) = g.get_mut(&corp_id) {
                    s.recruitment_ads.retain(|a| a.0 != ad_id);
                }
            });
            ctx.notify(
                NotificationTarget::Corporation(corp_id),
                "OnCorporationRecruitmentAdChanged",
                "corpid",
                vec![PyRep::Integer(corp_id), PyRep::Integer(ad_id)],
            );
            Some(Ok(PyRep::keyval([("adID", PyRep::Integer(ad_id)), ("deleted", PyRep::Bool(true))])))
        }

        ("corpRegistry", "AddCorporateContact")
        | ("corpRegistry", "EditCorporateContact")
        | ("corpRegistry", "EditContactsRelationshipID") => {
            if !has_diplomat {
                return deny_role("Managing corporate contacts requires Diplomat or Director role");
            }
            let contact_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let standing = args.get(1).and_then(|a| a.as_f64()).unwrap_or(5.0).clamp(-10.0, 10.0);
            let label_mask = args.get(2).and_then(|a| a.as_i64()).unwrap_or(0) as u64;
            ctx.db.with_mut_snapshot(|snap| {
                let list = snap.corp_contacts.entry(corp_id).or_default();
                list.retain(|c| c.contact_id != contact_id);
                list.push(eve_db::SocialContactRecord {
                    contact_id,
                    contact_type: "character".into(),
                    standing,
                    is_watched: false,
                    is_blocked: false,
                    label_ids: label_ids_from_mask(label_mask),
                });
                list.sort_by_key(|c| c.contact_id);
            });
            sync_corp_contacts_esi(ctx, corp_id);
            ctx.notify(NotificationTarget::Corporation(corp_id), "OnSetCorpStanding", "corpid", vec![PyRep::Integer(contact_id), PyRep::Real(standing)]);
            ctx.notify(
                NotificationTarget::Corporation(corp_id),
                "OnOrganizationContactsUpdated",
                "corpid",
                vec![
                    PyRep::Integer(corp_id),
                    PyRep::List(vec![PyRep::keyval([
                        ("contactID", PyRep::Integer(contact_id)),
                        ("standing", PyRep::Real(standing)),
                        ("labelMask", PyRep::Integer(label_mask as i64)),
                    ])]),
                ],
            );
            Some(Ok(PyRep::keyval([
                ("contactID", PyRep::Integer(contact_id)),
                ("relationshipID", PyRep::Real(standing)),
                ("labelMask", PyRep::Integer(label_mask as i64)),
            ])))
        }

        ("corpRegistry", "RemoveCorporateContacts") => {
            if !has_diplomat {
                return deny_role("Removing corporate contacts requires Diplomat or Director role");
            }
            let cid = match args.first() {
                Some(PyRep::Integer(id)) => *id,
                Some(PyRep::List(items)) | Some(PyRep::Tuple(items)) => {
                    items.first().and_then(|v| v.as_i64()).unwrap_or(0)
                }
                _ => 0,
            };
            ctx.db.with_mut_snapshot(|snap| {
                if let Some(list) = snap.corp_contacts.get_mut(&corp_id) {
                    list.retain(|c| c.contact_id != cid);
                }
            });
            sync_corp_contacts_esi(ctx, corp_id);
            Some(Ok(PyRep::keyval([
                ("contactID", PyRep::Integer(cid)),
                ("removed", PyRep::Bool(true)),
            ])))
        }

        ("corpRegistry", "GetCorporateContacts") => {
            let mut rows: Vec<Vec<PyRep>> = ctx.db.with_snapshot(|snap| {
                snap.corp_contacts
                    .get(&corp_id)
                    .map(|list| {
                        list.iter()
                            .map(|c| {
                                vec![
                                    PyRep::Integer(c.contact_id),
                                    PyRep::Real(c.standing),
                                    PyRep::Integer(label_mask_from_ids(&c.label_ids)),
                                ]
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            });
            rows.sort_by_key(|r| r[0].as_i64());
            Some(Ok(PyRep::rowset(
                vec!["contactID", "relationshipID", "labelMask"],
                rows,
            )))
        }

        ("corpRegistry", "CreateLabel") | ("corpRegistry", "EditLabel") => {
            if !has_diplomat {
                return deny_role("Managing corporate labels requires Director role");
            }
            let (lid, name, color) = ctx.db.with_mut_snapshot(|snap| {
                let labels = snap.corp_contact_labels.entry(corp_id).or_default();
                let (lid, name, color) = if method == "EditLabel"
                    && args.first().and_then(|a| a.as_i64()).is_some()
                {
                    let id = args.first().and_then(|a| a.as_i64()).unwrap_or(1);
                    let existing = labels
                        .iter()
                        .find(|(lid, _, _)| *lid == id)
                        .map(|(_, n, c)| (n.clone(), *c))
                        .unwrap_or_else(|| ("Allies".into(), 0x00FF00));
                    let n = args
                        .get(1)
                        .and_then(|a| a.as_str())
                        .unwrap_or(&existing.0)
                        .to_string();
                    let c = args
                        .get(2)
                        .and_then(|a| a.as_i64())
                        .map(|v| v as u32)
                        .unwrap_or(existing.1);
                    (id, n, c)
                } else {
                    let n = args
                        .first()
                        .and_then(|a| a.as_str())
                        .unwrap_or("Allies")
                        .to_string();
                    let c = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0x00FF00) as u32;
                    let next_bit = (0..30)
                        .map(|b| 1i64 << b)
                        .find(|id| !labels.iter().any(|(lid, _, _)| lid == id))
                        .unwrap_or(1);
                    (next_bit, n, c)
                };
                labels.retain(|(id, _, _)| *id != lid);
                labels.push((lid, name.clone(), color));
                labels.sort_by_key(|(id, _, _)| *id);
                (lid, name, color)
            });
            sync_corp_contacts_esi(ctx, corp_id);
            Some(Ok(PyRep::keyval([
                ("labelID", PyRep::Integer(lid)),
                ("name", PyRep::String(name)),
                ("color", PyRep::Integer(color as i64)),
            ])))
        }

        ("corpRegistry", "GetLabels") => {
            let mut rows: Vec<Vec<PyRep>> = ctx.db.with_snapshot(|snap| {
                snap.corp_contact_labels
                    .get(&corp_id)
                    .map(|labels| {
                        labels
                            .iter()
                            .map(|(lid, name, color)| {
                                vec![
                                    PyRep::Integer(*lid),
                                    PyRep::String(name.clone()),
                                    PyRep::Integer(*color as i64),
                                ]
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            });
            rows.sort_by_key(|r| r[0].as_i64());
            Some(Ok(PyRep::rowset(vec!["labelID", "name", "color"], rows)))
        }

        ("corpRegistry", "DeleteLabel")
        | ("corpRegistry", "AssignLabels")
        | ("corpRegistry", "RemoveLabels") => {
            if !has_diplomat {
                return deny_role("Managing corporate labels requires Director role");
            }
            let (lid, contact_ids): (i64, Vec<i64>) = match args.first() {
                Some(PyRep::List(items) | PyRep::Tuple(items)) => (
                    args.get(1).and_then(|a| a.as_i64()).unwrap_or(1),
                    items.iter().filter_map(|v| v.as_i64()).collect(),
                ),
                _ => {
                    let a0 = args.first().and_then(|a| a.as_i64()).unwrap_or(1);
                    let a1 = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0);
                    if a0 >= 1_000_000 && a1 > 0 && a1 < 1_000_000 {
                        (a1, vec![a0])
                    } else {
                        (a0, vec![a1])
                    }
                }
            };
            ctx.db.with_mut_snapshot(|snap| {
                if method == "DeleteLabel" {
                    if let Some(labels) = snap.corp_contact_labels.get_mut(&corp_id) {
                        labels.retain(|(id, _, _)| *id != lid);
                    }
                    if let Some(contacts) = snap.corp_contacts.get_mut(&corp_id) {
                        for c in contacts {
                            let mask = (label_mask_from_ids(&c.label_ids) as u64) & !(lid as u64);
                            c.label_ids = label_ids_from_mask(mask);
                        }
                    }
                } else {
                    let contacts = snap.corp_contacts.entry(corp_id).or_default();
                    for cid in contact_ids {
                        if let Some(entry) = contacts.iter_mut().find(|c| c.contact_id == cid) {
                            let mut mask = label_mask_from_ids(&entry.label_ids) as u64;
                            if method == "AssignLabels" {
                                mask |= lid as u64;
                            } else {
                                mask &= !(lid as u64);
                            }
                            entry.label_ids = label_ids_from_mask(mask);
                        } else if method == "AssignLabels" {
                            contacts.push(eve_db::SocialContactRecord {
                                contact_id: cid,
                                contact_type: "character".into(),
                                standing: 0.0,
                                is_watched: false,
                                is_blocked: false,
                                label_ids: label_ids_from_mask(lid as u64),
                            });
                        }
                    }
                    contacts.sort_by_key(|c| c.contact_id);
                }
            });
            sync_corp_contacts_esi(ctx, corp_id);
            Some(Ok(PyRep::keyval([("labelID", PyRep::Integer(lid))])))
        }

        ("corpRegistry", "SetCorpWelcomeMail") => {
            if !has_comms { return deny_role("Setting corporation welcome mail requires Communications Officer or Director role"); }
            let subject = args.first().and_then(|a| a.as_str()).unwrap_or("Welcome to the Corp");
            let body = args.get(1).and_then(|a| a.as_str()).unwrap_or("Fly safe o7");
            let enabled = args.get(2).and_then(|a| match a { PyRep::Bool(b) => Some(*b), PyRep::Integer(i) => Some(*i != 0), _ => None }).unwrap_or(true);
            with_corp_gov(ctx, |g| {
                g.entry(corp_id).or_default().welcome_mail = (enabled, subject.to_string(), body.to_string());
            });
            ctx.notify(NotificationTarget::Corporation(corp_id), "OnCorporationWelcomeMailChanged", "corpid", vec![PyRep::Integer(char_id), PyRep::Integer(now_unix as i64)]);
            Some(Ok(PyRep::keyval([
                ("enabled", PyRep::Bool(enabled)),
                ("subject", PyRep::String(subject.to_string())),
                ("body", PyRep::String(body.to_string())),
            ])))
        }

        ("corpRegistry", "GetCorpWelcomeMail") => {
            let (enabled, subject, body) = read_corp_gov(ctx, |g| {
                g.get(&corp_id).map(|s| s.welcome_mail.clone()).unwrap_or((false, String::new(), String::new()))
            });
            Some(Ok(PyRep::keyval([
                ("enabled", PyRep::Bool(enabled)),
                ("subject", PyRep::String(subject)),
                ("body", PyRep::String(body)),
            ])))
        }
        ("corpRegistry", "RegisterNewCorpMailRestrictionSettings") => {
            if !has_comms { return deny_role("Updating corporation mail restriction settings requires Communications Officer or Director role"); }
            let restricted = matches!(args.first(), Some(PyRep::Bool(true)) | Some(PyRep::Integer(1)));
            let prev = with_corp_gov(ctx, |g| {
                let s = g.entry(corp_id).or_default();
                std::mem::replace(&mut s.restrict_corp_mails, restricted)
            });
            let change = PyRep::Dict(vec![(PyRep::String(("restrictCorpMails").into()), PyRep::Tuple(vec![PyRep::Bool(prev), PyRep::Bool(restricted)]))]);
            ctx.notify(NotificationTarget::Corporation(corp_id), "OnCorporationChanged", "corpid", vec![PyRep::Integer(corp_id), change]);
            Some(Ok(PyRep::Bool(restricted)))
        }
        ("corpRegistry", "DoesCorpRestrictCorpMails") => {
            let restricted = read_corp_gov(ctx, |g| g.get(&corp_id).is_some_and(|s| s.restrict_corp_mails));
            Some(Ok(PyRep::Bool(restricted)))
        }
        ("corpRegistry", "RegisterNewAcceptStructureSettings") => {
            if !has_director { return deny_role("Updating corporation structure transfer settings requires CEO or Director role"); }
            let accept = matches!(args.first(), Some(PyRep::Bool(true)) | Some(PyRep::Integer(1)));
            let prev = with_corp_gov(ctx, |g| {
                let s = g.entry(corp_id).or_default();
                std::mem::replace(&mut s.accept_structures, accept)
            });
            let change = PyRep::Dict(vec![(PyRep::String(("acceptStructures").into()), PyRep::Tuple(vec![PyRep::Bool(prev), PyRep::Bool(accept)]))]);
            ctx.notify(NotificationTarget::Corporation(corp_id), "OnCorporationChanged", "corpid", vec![PyRep::Integer(corp_id), change]);
            Some(Ok(PyRep::Bool(accept)))
        }
        ("corpRegistry", "DoesMyCorpAcceptStructures") => {
            let accept = read_corp_gov(ctx, |g| g.get(&corp_id).map(|s| s.accept_structures).unwrap_or(true));
            Some(Ok(PyRep::Bool(accept)))
        }
        ("corpRegistry", "SetStructureReinforceDefault") => {
            if !has_director { return deny_role("Setting corporation structure reinforcement default requires CEO or Director role"); }
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
        _ => super::handle_corp_gov_tail(ctx, session, service, method, args, corp_id, char_id, now_unix),
    }
}
