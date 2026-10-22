//! Corporation Governance, Shares, Dividends, Titles, Votes, Medals, Bulletins, Recruitment & Alliances (`SO-3`).

use crate::router::{cache_notifications::NotificationTarget, ServerContext};
use eve_db::{
    corp_roles, ChatChannelRecord, CorpWalletDivisionRecord, CorporationRecord, IskCents,
    WalletJournalEntry,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicI64, Ordering};

use super::fleet_social_corps::{CORP_GOV, EMP_HISTORY};

static NEXT_CORP_ID_SEQ: AtomicI64 = AtomicI64::new(100_000);

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
        let cid = 98_000_000 + snap.next_entity_id + NEXT_CORP_ID_SEQ.fetch_add(10_000, Ordering::Relaxed);
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
        Ok((cid, old_corp_id))
    });

    match res {
        Ok((corp_id, old_corp_id)) => {
            session.corporation_id = Some(corp_id);
            session.alliance_id = None;
            session.corp_role = u64::MAX;
            {
                let mut g = CORP_GOV.lock().unwrap();
                let state = g.entry(corp_id).or_default();
                state.shares.insert(char_id, 1_000);
                state.company_shares = 0;
                state.accept_structures = true;
                state.reinforce_hour = 18;
            }
            {
                let mut emp = EMP_HISTORY.lock().unwrap();
                let list = emp.entry(char_id).or_default();
                if list.is_empty() {
                    list.push((old_corp_id, now_unix.saturating_sub(86_400), Some(now_unix)));
                } else if let Some(last) = list.last_mut() {
                    last.2 = Some(now_unix);
                }
                list.push((corp_id, now_unix, None));
            }
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
    let (has_director, has_comms) = ctx.db.with_snapshot(|snap| {
        snap.corporations.get(&corp_id).map_or((true, true), |c| {
            let r = c.member_roles.get(&char_id).copied().unwrap_or(0);
            let dir = c.ceo_id == char_id || corp_roles::has_role(r, corp_roles::ROLE_DIRECTOR);
            let comms = dir || corp_roles::has_role(r, corp_roles::ROLE_PERSONNEL_MANAGER);
            (dir, comms)
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
            let (old_from, new_from, old_to, new_to) = {
                let mut g = CORP_GOV.lock().unwrap();
                let state = g.entry(corp_id).or_default();
                if state.shares.is_empty() {
                    state.shares.insert(char_id, 1_000);
                }
                let from_count = state.shares.entry(char_id).or_insert(0);
                if *from_count < qty {
                    return Some(Err(BridgeRpcError {
                        code: "InsufficientShares".into(),
                        message: "Not enough shares to transfer".into(),
                    }));
                }
                let old_f = *from_count;
                *from_count -= qty;
                let new_f = *from_count;
                let to_entry = state.shares.entry(target_id).or_insert(0);
                let old_t = *to_entry;
                *to_entry += qty;
                let new_t = *to_entry;
                (old_f, new_f, old_t, new_t)
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
            let mut g = CORP_GOV.lock().unwrap();
            let state = g.entry(corp_id).or_default();
            if state.shares.is_empty() {
                state.shares.insert(char_id, 1_000);
            }
            let rows: Vec<Vec<PyRep>> = state
                .shares
                .iter()
                .filter(|(&oid, _)| oid == target_sh_id)
                .map(|(&oid, &cnt)| vec![PyRep::Integer(oid), PyRep::Integer(corp_id), PyRep::Integer(cnt)])
                .collect();
            Some(Ok(PyRep::rowset(vec!["shareholderID", "corporationID", "shares"], rows)))
        }

        ("corpRegistry", "GetShareholders")
        | ("corpRegistry", "GetMemberIDsWithMoreThanAvgShares") => {
            let mut g = CORP_GOV.lock().unwrap();
            let state = g.entry(corp_id).or_default();
            if state.shares.is_empty() {
                state.shares.insert(char_id, 1_000);
            }
            if method == "GetMemberIDsWithMoreThanAvgShares" {
                let total: i64 = state.shares.values().sum();
                let avg = total / (state.shares.len().max(1) as i64);
                let ids: Vec<PyRep> = state
                    .shares
                    .iter()
                    .filter(|(_, &cnt)| cnt >= avg)
                    .map(|(&oid, _)| PyRep::Integer(oid))
                    .collect();
                return Some(Ok(PyRep::List(ids)));
            }
            let rows: Vec<Vec<PyRep>> = state
                .shares
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
            let shares_snapshot = {
                let mut g = CORP_GOV.lock().unwrap();
                let state = g.entry(corp_id).or_default();
                if state.shares.is_empty() {
                    state.shares.insert(char_id, 1_000);
                }
                state.shares.clone()
            };

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

                let recipients: Vec<(i64, i64)> = if to_shareholders {
                    let total_shares: i64 = shares_snapshot.values().sum::<i64>().max(1);
                    shares_snapshot
                        .into_iter()
                        .filter(|(_, s)| *s > 0)
                        .map(|(oid, s)| (oid, (payout_cents.0 * s) / total_shares))
                        .collect()
                } else {
                    let mcount = (corp.members.len() as i64).max(1);
                    corp.members
                        .iter()
                        .map(|&mid| (mid, payout_cents.0 / mcount))
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
            let prev = CORP_GOV.lock().unwrap().entry(corp_id).or_default().titles.insert(title_id, (title_name.to_string(), roles_mask));
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
            let rows: Vec<Vec<PyRep>> = CORP_GOV.lock().unwrap().get(&corp_id).map(|g| {
                g.titles.iter().map(|(&tid, (tname, rmask))| vec![PyRep::Integer(tid), PyRep::String(tname.clone()), PyRep::Integer(*rmask as i64)]).collect()
            }).unwrap_or_default();
            Some(Ok(PyRep::rowset(vec!["titleID", "titleName", "roles"], rows)))
        }

        ("corpRegistry", "DeleteTitle") => {
            if !has_director { return deny_role("Deleting corporation titles requires CEO or Director role"); }
            let title_id = args.first().and_then(|a| a.as_i64()).unwrap_or(1);
            let prev = CORP_GOV.lock().unwrap().get_mut(&corp_id).and_then(|g| g.titles.remove(&title_id));
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
            let vote_type = args.get(2).and_then(|a| a.as_i64()).unwrap_or(0); // 0 = CEO, 1 = War, 4 = Kick, 5 = Shares
            let target_id = args.get(3).and_then(|a| a.as_i64()).unwrap_or(char_id);
            let case_id = {
                let mut g = CORP_GOV.lock().unwrap();
                let state = g.entry(corp_id).or_default();
                let cid = (state.vote_cases.len() as i64) + 1;
                state.vote_cases.push((cid, text.to_string(), desc.to_string(), vote_type, target_id, now_unix + 86_400, false));
                cid
            };
            ctx.notify(NotificationTarget::Corporation(corp_id), "OnVoteCaseChanged", "corpid", vec![PyRep::Integer(corp_id), PyRep::Integer(case_id)]);
            ctx.notify(NotificationTarget::Corporation(corp_id), "OnSanctionedActionChanged", "corpid", vec![PyRep::Integer(corp_id), PyRep::Integer(case_id)]);
            Some(Ok(PyRep::keyval([
                ("voteCaseID", PyRep::Integer(case_id)),
                ("voteType", PyRep::Integer(vote_type)),
                ("parameter", PyRep::Integer(target_id)),
            ])))
        }

        ("voteManager", "InsertVote") | ("corpRegistry", "InsertVote") => {
            let case_id = args.first().and_then(|a| a.as_i64()).unwrap_or(1);
            let option_id = args.get(1).and_then(|a| a.as_i64()).unwrap_or(1);
            let weight = {
                let mut g = CORP_GOV.lock().unwrap();
                let state = g.entry(corp_id).or_default();
                let w = state.shares.get(&char_id).copied().unwrap_or(1).max(1);
                state.votes.insert((case_id, char_id), (option_id, w));
                w
            };
            ctx.notify(NotificationTarget::Corporation(corp_id), "OnVoteCast", "corpid", vec![PyRep::Integer(corp_id), PyRep::Integer(case_id)]);
            Some(Ok(PyRep::keyval([
                ("voteCaseID", PyRep::Integer(case_id)),
                ("optionID", PyRep::Integer(option_id)),
                ("weight", PyRep::Integer(weight)),
            ])))
        }

        ("voteManager", "GetVotes")
        | ("voteManager", "GetVoteCase")
        | ("voteManager", "GetVoteCasesByCorporation")
        | ("voteManager", "GetVoteCaseOptions")
        | ("voteManager", "GetCorpSanctionedActions") => {
            let g = CORP_GOV.lock().unwrap();
            let rows: Vec<Vec<PyRep>> = g
                .get(&corp_id)
                .map(|s| {
                    s.vote_cases
                        .iter()
                        .map(|(cid, txt, desc, vtype, param, end_ts, acted)| {
                            let yes_votes: i64 = s.votes.iter().filter(|&(&(vc, _), &(opt, _))| vc == *cid && opt == 1).map(|(_, &(_, w))| w).sum();
                            vec![
                                PyRep::Integer(*cid),
                                PyRep::String(txt.clone()),
                                PyRep::String(desc.clone()),
                                PyRep::Integer(*vtype),
                                PyRep::Integer(*param),
                                PyRep::Integer(*end_ts as i64),
                                PyRep::Integer(yes_votes),
                                PyRep::Bool(*acted),
                            ]
                        })
                        .collect()
                })
                .unwrap_or_default();
            Some(Ok(PyRep::rowset(
                vec!["voteCaseID", "voteCaseText", "description", "voteType", "parameter", "endDateTime", "votesFor", "inEffect"],
                rows,
            )))
        }

        ("voteManager", "ActivateSanctionedAction") => {
            if !has_director { return deny_role("Activating sanctioned actions requires CEO or Director role"); }
            let case_id = args.first().and_then(|a| a.as_i64()).unwrap_or(1);
            let act_res = {
                let mut g = CORP_GOV.lock().unwrap();
                let Some(state) = g.get_mut(&corp_id) else {
                    return Some(Err(BridgeRpcError { code: "VoteCaseNotFound".into(), message: format!("Vote case {case_id} not found") }));
                };
                let yes_votes: i64 = state.votes.iter().filter(|&(&(vc, _), &(opt, _))| vc == case_id && opt == 1).map(|(_, &(_, w))| w).sum();
                let no_votes: i64 = state.votes.iter().filter(|&(&(vc, _), &(opt, _))| vc == case_id && opt != 1).map(|(_, &(_, w))| w).sum();
                let Some(vc) = state.vote_cases.iter_mut().find(|c| c.0 == case_id) else {
                    return Some(Err(BridgeRpcError { code: "VoteCaseNotFound".into(), message: format!("Vote case {case_id} not found") }));
                };
                if vc.6 {
                    return Some(Err(BridgeRpcError { code: "VoteAlreadyActivated".into(), message: "Sanctioned action has already been activated".into() }));
                }
                if now_unix < vc.5 {
                    return Some(Err(BridgeRpcError { code: "VoteStillOpen".into(), message: "Cannot activate sanctioned action before voting closes".into() }));
                }
                if yes_votes <= no_votes || yes_votes == 0 {
                    return Some(Err(BridgeRpcError { code: "VoteDidNotPass".into(), message: "Sanctioned action did not pass shareholder vote".into() }));
                }
                vc.6 = true;
                let info = (vc.3, vc.4);
                let mut share_delta = None;
                if info.0 == 5 {
                    state.company_shares += info.1;
                    let entry = state.shares.entry(char_id).or_insert(0);
                    let old_s = *entry;
                    *entry += info.1;
                    share_delta = Some((old_s, *entry));
                }
                (info.0, info.1, share_delta)
            };
            ctx.notify(NotificationTarget::Corporation(corp_id), "OnSanctionedActionChanged", "corpid", vec![PyRep::Integer(corp_id), PyRep::Integer(case_id)]);
            if let Some((old_s, new_s)) = act_res.2 {
                let s_change = PyRep::Dict(vec![(PyRep::String(("shares").into()), PyRep::Tuple(vec![PyRep::Integer(old_s), PyRep::Integer(new_s)]))]);
                ctx.notify(NotificationTarget::Corporation(corp_id), "OnShareChange", "corpid", vec![PyRep::Integer(char_id), PyRep::Integer(corp_id), s_change]);
            }
            if act_res.0 == 0 {
                let new_ceo_id = act_res.1;
                let mut old_ceo_id = char_id;
                let _ = ctx.db.try_with_mut_snapshot(|snap| -> Result<(), BridgeRpcError> {
                    if let Some(corp) = snap.corporations.get_mut(&corp_id) {
                        old_ceo_id = corp.ceo_id;
                        corp.ceo_id = new_ceo_id;
                        corp.member_roles.insert(new_ceo_id, u64::MAX);
                    }
                    Ok(())
                });
                let change = PyRep::Dict(vec![(PyRep::String(("ceoID").into()), PyRep::Tuple(vec![PyRep::Integer(old_ceo_id), PyRep::Integer(new_ceo_id)]))]);
                ctx.notify(NotificationTarget::Corporation(corp_id), "OnCorporationChanged", "corpid", vec![PyRep::Integer(corp_id), change]);
            }
            Some(Ok(PyRep::keyval([("voteCaseID", PyRep::Integer(case_id)), ("activated", PyRep::Bool(true))])))
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
                Ok(())
            });
            if let Err(e) = res {
                return Some(Err(e));
            }
            let medal_id = {
                let mut g = CORP_GOV.lock().unwrap();
                let state = g.entry(corp_id).or_default();
                let mid = (state.medals.len() as i64) + 1;
                state.medals.push((mid, title.to_string(), desc.to_string(), char_id, now_unix));
                mid
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
            CORP_GOV.lock().unwrap().entry(corp_id).or_default().medal_recipients.push((medal_id, recipient_id, char_id, reason.to_string(), 2));
            ctx.notify(NotificationTarget::Corporation(corp_id), "OnCorporationMedalAdded", "corpid", vec![PyRep::Integer(medal_id), PyRep::Integer(recipient_id)]);
            ctx.notify(NotificationTarget::Character(recipient_id), "OnMedalIssued", "charid", vec![PyRep::Integer(medal_id), PyRep::Integer(corp_id), PyRep::Integer(char_id)]);
            Some(Ok(PyRep::keyval([
                ("medalID", PyRep::Integer(medal_id)),
                ("recipientID", PyRep::Integer(recipient_id)),
                ("status", PyRep::Integer(2)),
            ])))
        }

        ("corporationSvc", "SetMedalStatus") => {
            let medal_id = args.first().and_then(|a| a.as_i64()).unwrap_or(1);
            let new_status = args.get(1).and_then(|a| a.as_i64()).unwrap_or(3);
            {
                let mut g = CORP_GOV.lock().unwrap();
                for state in g.values_mut() {
                    for rec in &mut state.medal_recipients {
                        if rec.0 == medal_id && rec.1 == char_id {
                            rec.4 = new_status;
                        }
                    }
                }
            }
            ctx.notify(NotificationTarget::Character(char_id), "OnMedalStatusChanged", "charid", vec![PyRep::Integer(medal_id), PyRep::Integer(new_status)]);
            Some(Ok(PyRep::keyval([
                ("medalID", PyRep::Integer(medal_id)),
                ("status", PyRep::Integer(new_status)),
            ])))
        }

        ("corporationSvc", "GetAllCorpMedals")
        | ("corporationSvc", "GetMedalDetails")
        | ("corporationSvc", "GetMedalsReceived")
        | ("corporationSvc", "GetRecipientsOfMedal") => {
            let target_char = args.first().and_then(|a| a.as_i64()).unwrap_or(char_id);
            let g = CORP_GOV.lock().unwrap();
            if method == "GetMedalsReceived" || method == "GetRecipientsOfMedal" {
                let mut rows = Vec::new();
                for (&cid, state) in g.iter() {
                    for (mid, rid, iid, reason, status) in &state.medal_recipients {
                        if (method == "GetMedalsReceived" && *rid == target_char && *status > 1)
                            || (method == "GetRecipientsOfMedal" && *mid == target_char && cid == corp_id)
                        {
                            let title = state
                                .medals
                                .iter()
                                .find(|m| m.0 == *mid)
                                .map(|m| m.1.clone())
                                .unwrap_or_else(|| "Medal".into());
                            rows.push(vec![
                                PyRep::Integer(*mid),
                                PyRep::Integer(*rid),
                                PyRep::Integer(*iid),
                                PyRep::Integer(cid),
                                PyRep::String(title),
                                PyRep::String(reason.clone()),
                                PyRep::Integer(*status),
                            ]);
                        }
                    }
                }
                return Some(Ok(PyRep::rowset(
                    vec![
                        "medalID",
                        "wearerID",
                        "issuerID",
                        "corporationID",
                        "title",
                        "reason",
                        "status",
                    ],
                    rows,
                )));
            }
            let rows: Vec<Vec<PyRep>> = g
                .get(&corp_id)
                .map(|s| {
                    s.medals
                        .iter()
                        .map(|(mid, title, desc, creator, ts)| {
                            vec![
                                PyRep::Integer(*mid),
                                PyRep::String(title.clone()),
                                PyRep::String(desc.clone()),
                                PyRep::Integer(*creator),
                                PyRep::Integer(*ts as i64),
                            ]
                        })
                        .collect()
                })
                .unwrap_or_default();
            Some(Ok(PyRep::rowset(
                vec!["medalID", "title", "description", "creatorID", "date"],
                rows,
            )))
        }

        ("corporationSvc", "GetEmploymentRecord")
        | ("corporationSvc", "GetEmployementRecordAndCharacterTransfers") => {
            let target_id = args.first().and_then(|a| a.as_i64()).unwrap_or(char_id);
            let default_corp = ctx.db.with_snapshot(|snap| {
                snap.characters
                    .get(&target_id)
                    .map(|c| c.corporation_id)
                    .unwrap_or(corp_id)
            });
            let mut emp = EMP_HISTORY.lock().unwrap();
            let list = emp.entry(target_id).or_default();
            if list.is_empty() {
                list.push((default_corp, now_unix.saturating_sub(86_400), None));
            }
            if method == "GetEmployementRecordAndCharacterTransfers" {
                let rows: Vec<Vec<PyRep>> = list
                    .iter()
                    .enumerate()
                    .map(|(idx, &(cid, start_ts, _))| {
                        vec![
                            PyRep::Integer((idx + 1) as i64),
                            PyRep::Integer(cid),
                            PyRep::Integer(start_ts as i64),
                        ]
                    })
                    .collect();
                Some(Ok(PyRep::rowset(
                    vec!["recordID", "corporationID", "startDate"],
                    rows,
                )))
            } else {
                let rows: Vec<Vec<PyRep>> = list
                    .iter()
                    .map(|&(cid, start_ts, end_ts)| {
                        vec![
                            PyRep::Integer(cid),
                            PyRep::Integer(start_ts as i64),
                            end_ts.map(|e| PyRep::Integer(e as i64)).unwrap_or(PyRep::None),
                        ]
                    })
                    .collect();
                Some(Ok(PyRep::rowset(
                    vec!["corporationID", "startDate", "endDate"],
                    rows,
                )))
            }
        }

        ("corpRegistry", "AddBulletin") | ("corpRegistry", "UpdateBulletinOrder") => {
            if !has_comms { return deny_role("Managing corporation bulletins requires Communications Officer or Director role"); }
            let title = args.first().and_then(|a| a.as_str()).unwrap_or("Corporation Bulletin");
            let body = args.get(1).and_then(|a| a.as_str()).unwrap_or("All hands on deck");
            let is_alliance = matches!(args.get(2), Some(PyRep::Bool(true)));
            let owner_key = if is_alliance { session.alliance_id.unwrap_or(corp_id) } else { corp_id };
            let mut g = CORP_GOV.lock().unwrap();
            let state = g.entry(owner_key).or_default();
            let bid = (state.bulletins.len() as i64) + 1;
            state.bulletins.push((bid, title.to_string(), body.to_string(), char_id, now_unix));
            Some(Ok(PyRep::keyval([
                ("bulletinID", PyRep::Integer(bid)),
                ("title", PyRep::String(title.to_string())),
                ("body", PyRep::String(body.to_string())),
            ])))
        }

        ("corpRegistry", "GetBulletins") => {
            let owner_key = args.first().and_then(|a| a.as_i64()).unwrap_or(corp_id);
            let rows: Vec<Vec<PyRep>> = CORP_GOV.lock().unwrap().get(&owner_key).map(|s| {
                s.bulletins.iter().map(|(bid, title, body, aid, ts)| {
                    vec![PyRep::Integer(*bid), PyRep::String(title.clone()), PyRep::String(body.clone()), PyRep::Integer(*aid), PyRep::Integer(*ts as i64)]
                }).collect()
            }).unwrap_or_default();
            Some(Ok(PyRep::rowset(vec!["bulletinID", "title", "body", "editCharacterID", "editDateTime"], rows)))
        }

        ("corpRegistry", "DeleteBulletin") => {
            if !has_comms { return deny_role("Deleting corporation bulletins requires Communications Officer or Director role"); }
            let bid = args.first().and_then(|a| a.as_i64()).unwrap_or(1);
            if let Some(s) = CORP_GOV.lock().unwrap().get_mut(&corp_id) {
                s.bulletins.retain(|b| b.0 != bid);
            }
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
            let (title, desc, type_mask) = if method == "UpdateRecruitmentAd" {
                let type_mask = args.get(1).and_then(|a| a.as_i64()).unwrap_or(7) as u64;
                let title = args.get(2).and_then(|a| a.as_str()).unwrap_or("Updated Corp Ad");
                let desc = args.get(3).and_then(|a| a.as_str()).unwrap_or("Updated description");
                (title, desc, type_mask)
            } else if args.first().and_then(|a| a.as_str()).is_some() {
                let title = args.first().and_then(|a| a.as_str()).unwrap_or("Nullsec PvP & Industry");
                let desc = args.get(1).and_then(|a| a.as_str()).unwrap_or("Join our corp today");
                let type_mask = args.get(2).and_then(|a| a.as_i64()).unwrap_or(7) as u64;
                (title, desc, type_mask)
            } else {
                let type_mask = args.get(1).and_then(|a| a.as_i64()).unwrap_or(7) as u64;
                let title = args.get(2).and_then(|a| a.as_str()).unwrap_or("Nullsec PvP & Industry");
                let desc = args.get(3).and_then(|a| a.as_str()).unwrap_or("Join our corp today");
                (title, desc, type_mask)
            };
            let mut g = CORP_GOV.lock().unwrap();
            let state = g.entry(corp_id).or_default();
            let ad_id = if method == "UpdateRecruitmentAd" {
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
            };
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

        ("corpRecProxy", "GetRecruitmentAdsForCorporation")
        | ("corpRecProxy", "GetRecruitmentAdsByCorpID")
        | ("corpRecProxy", "GetRecruitmentAd")
        | ("corpRecProxy", "SearchCorpAds")
        | ("corpRecProxy", "GetRecruiters") => {
            let filter_mask = if method == "SearchCorpAds" {
                args.first().and_then(|a| a.as_i64()).unwrap_or(0) as u64
            } else {
                0
            };
            let g = CORP_GOV.lock().unwrap();
            let mut rows = Vec::new();
            for (&cid, state) in g.iter() {
                if method == "SearchCorpAds" || cid == corp_id {
                    for (aid, title, desc, mask, rec_id) in &state.recruitment_ads {
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
            Some(Ok(PyRep::rowset(
                vec![
                    "adID",
                    "corporationID",
                    "title",
                    "description",
                    "typeMask",
                    "recruiterID",
                ],
                rows,
            )))
        }

        ("corpRecProxy", "DeleteRecruitmentAd") => {
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
                    message: "Deleting recruitment ads requires Personnel Manager or Director role"
                        .into(),
                }));
            }
            let ad_id = args.first().and_then(|a| a.as_i64()).unwrap_or(1);
            if let Some(s) = CORP_GOV.lock().unwrap().get_mut(&corp_id) {
                s.recruitment_ads.retain(|a| a.0 != ad_id);
            }
            ctx.notify(
                NotificationTarget::Corporation(corp_id),
                "OnCorporationRecruitmentAdChanged",
                "corpid",
                vec![PyRep::Integer(corp_id), PyRep::Integer(ad_id)],
            );
            Some(Ok(PyRep::keyval([
                ("adID", PyRep::Integer(ad_id)),
                ("deleted", PyRep::Bool(true)),
            ])))
        }

        ("corpRegistry", "AddCorporateContact")
        | ("corpRegistry", "EditCorporateContact")
        | ("corpRegistry", "EditContactsRelationshipID") => {
            let has_role = ctx.db.with_snapshot(|snap| {
                snap.corporations.get(&corp_id).is_none_or(|c| {
                    let r = c.member_roles.get(&char_id).copied().unwrap_or(0);
                    char_id == c.ceo_id
                        || corp_roles::has_role(r, corp_roles::ROLE_DIRECTOR | (1u64 << 42))
                })
            });
            if !has_role {
                return Some(Err(BridgeRpcError {
                    code: "CorpRoleDenied".into(),
                    message: "Managing corporate contacts requires Diplomat or Director role".into(),
                }));
            }
            let contact_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let standing = args
                .get(1)
                .and_then(|a| a.as_f64())
                .unwrap_or(5.0)
                .clamp(-10.0, 10.0);
            let label_mask = args.get(2).and_then(|a| a.as_i64()).unwrap_or(0) as u64;
            CORP_GOV
                .lock()
                .unwrap()
                .entry(corp_id)
                .or_default()
                .contacts
                .insert(contact_id, (standing, label_mask));
            ctx.notify(
                NotificationTarget::Corporation(corp_id),
                "OnSetCorpStanding",
                "corpid",
                vec![PyRep::Integer(contact_id), PyRep::Real(standing)],
            );
            Some(Ok(PyRep::keyval([
                ("contactID", PyRep::Integer(contact_id)),
                ("relationshipID", PyRep::Real(standing)),
                ("labelMask", PyRep::Integer(label_mask as i64)),
            ])))
        }

        ("corpRegistry", "RemoveCorporateContacts") => {
            let has_role = ctx.db.with_snapshot(|snap| {
                snap.corporations.get(&corp_id).is_none_or(|c| {
                    let r = c.member_roles.get(&char_id).copied().unwrap_or(0);
                    char_id == c.ceo_id
                        || corp_roles::has_role(r, corp_roles::ROLE_DIRECTOR | (1u64 << 42))
                })
            });
            if !has_role {
                return Some(Err(BridgeRpcError {
                    code: "CorpRoleDenied".into(),
                    message: "Removing corporate contacts requires Diplomat or Director role".into(),
                }));
            }
            let cid = match args.first() {
                Some(PyRep::Integer(id)) => *id,
                Some(PyRep::List(items)) | Some(PyRep::Tuple(items)) => {
                    items.first().and_then(|v| v.as_i64()).unwrap_or(0)
                }
                _ => 0,
            };
            if let Some(s) = CORP_GOV.lock().unwrap().get_mut(&corp_id) {
                s.contacts.remove(&cid);
            }
            Some(Ok(PyRep::keyval([
                ("contactID", PyRep::Integer(cid)),
                ("removed", PyRep::Bool(true)),
            ])))
        }

        ("corpRegistry", "GetCorporateContacts") => {
            let rows: Vec<Vec<PyRep>> = CORP_GOV
                .lock()
                .unwrap()
                .get(&corp_id)
                .map(|s| {
                    s.contacts
                        .iter()
                        .map(|(&cid, &(st, lmask))| {
                            vec![
                                PyRep::Integer(cid),
                                PyRep::Real(st),
                                PyRep::Integer(lmask as i64),
                            ]
                        })
                        .collect()
                })
                .unwrap_or_default();
            Some(Ok(PyRep::rowset(
                vec!["contactID", "relationshipID", "labelMask"],
                rows,
            )))
        }

        ("corpRegistry", "CreateLabel") | ("corpRegistry", "EditLabel") => {
            let has_role = ctx.db.with_snapshot(|snap| {
                snap.corporations.get(&corp_id).is_none_or(|c| {
                    let r = c.member_roles.get(&char_id).copied().unwrap_or(0);
                    char_id == c.ceo_id
                        || corp_roles::has_role(r, corp_roles::ROLE_DIRECTOR | (1u64 << 42))
                })
            });
            if !has_role {
                return Some(Err(BridgeRpcError {
                    code: "CorpRoleDenied".into(),
                    message: "Managing corporate labels requires Director role".into(),
                }));
            }
            let mut g = CORP_GOV.lock().unwrap();
            let state = g.entry(corp_id).or_default();
            let (lid, name, color) = if method == "EditLabel"
                && args.first().and_then(|a| a.as_i64()).is_some()
            {
                let id = args.first().and_then(|a| a.as_i64()).unwrap_or(1);
                let existing = state
                    .labels
                    .get(&id)
                    .cloned()
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
                    .find(|id| !state.labels.contains_key(id))
                    .unwrap_or(1);
                (next_bit, n, c)
            };
            state.labels.insert(lid, (name.clone(), color));
            Some(Ok(PyRep::keyval([
                ("labelID", PyRep::Integer(lid)),
                ("name", PyRep::String(name)),
                ("color", PyRep::Integer(color as i64)),
            ])))
        }

        ("corpRegistry", "GetLabels") => {
            let rows: Vec<Vec<PyRep>> = CORP_GOV
                .lock()
                .unwrap()
                .get(&corp_id)
                .map(|s| {
                    s.labels
                        .iter()
                        .map(|(&lid, (name, color))| {
                            vec![
                                PyRep::Integer(lid),
                                PyRep::String(name.clone()),
                                PyRep::Integer(*color as i64),
                            ]
                        })
                        .collect()
                })
                .unwrap_or_default();
            Some(Ok(PyRep::rowset(vec!["labelID", "name", "color"], rows)))
        }

        ("corpRegistry", "DeleteLabel")
        | ("corpRegistry", "AssignLabels")
        | ("corpRegistry", "RemoveLabels") => {
            let has_role = ctx.db.with_snapshot(|snap| {
                snap.corporations.get(&corp_id).is_none_or(|c| {
                    let r = c.member_roles.get(&char_id).copied().unwrap_or(0);
                    char_id == c.ceo_id
                        || corp_roles::has_role(r, corp_roles::ROLE_DIRECTOR | (1u64 << 42))
                })
            });
            if !has_role {
                return Some(Err(BridgeRpcError {
                    code: "CorpRoleDenied".into(),
                    message: "Managing corporate labels requires Director role".into(),
                }));
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
            if let Some(s) = CORP_GOV.lock().unwrap().get_mut(&corp_id) {
                if method == "DeleteLabel" {
                    s.labels.remove(&lid);
                    for c in s.contacts.values_mut() {
                        c.1 &= !(lid as u64);
                    }
                } else {
                    for cid in contact_ids {
                        let entry = s.contacts.entry(cid).or_insert((0.0, 0));
                        if method == "AssignLabels" {
                            entry.1 |= lid as u64;
                        } else {
                            entry.1 &= !(lid as u64);
                        }
                    }
                }
            }
            Some(Ok(PyRep::keyval([("labelID", PyRep::Integer(lid))])))
        }

        ("corpRegistry", "SetCorpWelcomeMail") => {
            if !has_comms { return deny_role("Setting corporation welcome mail requires Communications Officer or Director role"); }
            let subject = args.first().and_then(|a| a.as_str()).unwrap_or("Welcome to the Corp");
            let body = args.get(1).and_then(|a| a.as_str()).unwrap_or("Fly safe o7");
            let enabled = args.get(2).and_then(|a| match a { PyRep::Bool(b) => Some(*b), PyRep::Integer(i) => Some(*i != 0), _ => None }).unwrap_or(true);
            CORP_GOV.lock().unwrap().entry(corp_id).or_default().welcome_mail = (enabled, subject.to_string(), body.to_string());
            ctx.notify(NotificationTarget::Corporation(corp_id), "OnCorporationWelcomeMailChanged", "corpid", vec![PyRep::Integer(char_id), PyRep::Integer(now_unix as i64)]);
            Some(Ok(PyRep::keyval([
                ("enabled", PyRep::Bool(enabled)),
                ("subject", PyRep::String(subject.to_string())),
                ("body", PyRep::String(body.to_string())),
            ])))
        }

        ("corpRegistry", "GetCorpWelcomeMail") => {
            let (enabled, subject, body) = CORP_GOV.lock().unwrap().get(&corp_id).map(|s| s.welcome_mail.clone()).unwrap_or((false, String::new(), String::new()));
            Some(Ok(PyRep::keyval([
                ("enabled", PyRep::Bool(enabled)),
                ("subject", PyRep::String(subject)),
                ("body", PyRep::String(body)),
            ])))
        }

        ("corpRegistry", "RegisterNewCorpMailRestrictionSettings") => {
            if !has_comms { return deny_role("Updating corporation mail restriction settings requires Communications Officer or Director role"); }
            let restricted = matches!(args.first(), Some(PyRep::Bool(true)) | Some(PyRep::Integer(1)));
            let prev = {
                let mut g = CORP_GOV.lock().unwrap();
                let s = g.entry(corp_id).or_default();
                std::mem::replace(&mut s.restrict_corp_mails, restricted)
            };
            let change = PyRep::Dict(vec![(PyRep::String(("restrictCorpMails").into()), PyRep::Tuple(vec![PyRep::Bool(prev), PyRep::Bool(restricted)]))]);
            ctx.notify(NotificationTarget::Corporation(corp_id), "OnCorporationChanged", "corpid", vec![PyRep::Integer(corp_id), change]);
            Some(Ok(PyRep::Bool(restricted)))
        }

        ("corpRegistry", "DoesCorpRestrictCorpMails") => {
            let restricted = CORP_GOV.lock().unwrap().get(&corp_id).is_some_and(|s| s.restrict_corp_mails);
            Some(Ok(PyRep::Bool(restricted)))
        }

        ("corpRegistry", "RegisterNewAcceptStructureSettings") => {
            if !has_director { return deny_role("Updating corporation structure transfer settings requires CEO or Director role"); }
            let accept = matches!(args.first(), Some(PyRep::Bool(true)) | Some(PyRep::Integer(1)));
            let prev = {
                let mut g = CORP_GOV.lock().unwrap();
                let s = g.entry(corp_id).or_default();
                std::mem::replace(&mut s.accept_structures, accept)
            };
            let change = PyRep::Dict(vec![(PyRep::String(("acceptStructures").into()), PyRep::Tuple(vec![PyRep::Bool(prev), PyRep::Bool(accept)]))]);
            ctx.notify(NotificationTarget::Corporation(corp_id), "OnCorporationChanged", "corpid", vec![PyRep::Integer(corp_id), change]);
            Some(Ok(PyRep::Bool(accept)))
        }

        ("corpRegistry", "DoesMyCorpAcceptStructures") => {
            let accept = CORP_GOV.lock().unwrap().get(&corp_id).map(|s| s.accept_structures).unwrap_or(true);
            Some(Ok(PyRep::Bool(accept)))
        }

        ("corpRegistry", "SetStructureReinforceDefault") => {
            if !has_director { return deny_role("Setting corporation structure reinforcement default requires CEO or Director role"); }
            let hour = args.first().and_then(|a| a.as_i64()).unwrap_or(18).clamp(0, 23) as u8;
            CORP_GOV.lock().unwrap().entry(corp_id).or_default().reinforce_hour = hour;
            Some(Ok(PyRep::Integer(hour as i64)))
        }

        ("corpRegistry", "GetStructureReinforceDefault") => {
            let hour = CORP_GOV
                .lock()
                .unwrap()
                .get(&corp_id)
                .map(|s| s.reinforce_hour)
                .unwrap_or(18);
            Some(Ok(PyRep::Integer(hour as i64)))
        }

        _ => super::handle_corp_gov_tail(
            ctx, session, service, method, args, corp_id, char_id, now_unix,
        ),
    }
}
