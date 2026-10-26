//! Corporation Governance, Shares, Dividends, Titles, Votes, Medals, Bulletins, Recruitment & Alliances (`SO-3`).

use crate::router::{cache_notifications::NotificationTarget, ServerContext};
use eve_db::{
    corp_roles, ChatChannelRecord, CorpVoteBallotRecord, CorpWalletDivisionRecord,
    CorporationRecord, IskCents, WalletJournalEntry,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use super::fleet_social_corps::{handle_insert_vote_case, read_corp_gov, with_corp_gov};
use super::fleet_social_notify::handle_corp_contacts_and_labels;
use crate::services::space_entity_ops::{handle_corp_bulletins, handle_corp_medals};
use super::fleet_social_persistence::{
    eval_vote_case_winner, handle_vote_queries, label_mask_from_ids, sync_corp_contacts_esi,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorpRecruitmentAd {
    pub ad_id: i64,
    pub corp_id: i64,
    pub title: String,
    pub description: String,
    pub type_mask: u64,
    pub recruiter_id: i64,
    pub created_unix: u64,
    pub expiry_unix: u64,
    pub days: u32,
    pub lang_mask: u64,
}

fn load_corp_rec_ads(ctx: &ServerContext) -> Vec<CorpRecruitmentAd> {
    let mut ads: Vec<CorpRecruitmentAd> = ctx.db.with_snapshot(|snap| {
        snap.service_state_blobs
            .get("corp_recruitment_ads")
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default()
    });
    if ads.is_empty() {
        read_corp_gov(ctx, |g| {
            for (&cid, state) in g.iter() {
                for (aid, title, desc, mask, rec_id) in &state.recruitment_ads {
                    ads.push(CorpRecruitmentAd {
                        ad_id: *aid, corp_id: cid, title: title.clone(), description: desc.clone(),
                        type_mask: *mask, recruiter_id: *rec_id, created_unix: 0,
                        expiry_unix: u64::MAX, days: 30, lang_mask: 0,
                    });
                }
            }
        });
    }
    ads
}

fn save_corp_rec_ads(ctx: &ServerContext, ads: &[CorpRecruitmentAd]) {
    if let Ok(serialized) = serde_json::to_string(ads) {
        ctx.db.with_mut_snapshot(|snap| {
            snap.service_state_blobs.insert("corp_recruitment_ads".to_string(), serialized);
        });
    }
}

pub(super) fn handle_add_corporation(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (_, char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let now_unix = ctx.now_unix();
    if let Some(active_timer) = crate::services::space_destiny::check_session_change_timer_active(ctx, char_id, now_unix, None) {
        return Some(Err(BridgeRpcError {
            code: "SessionChangeInProgress".into(),
            message: format!("Cannot create corporation while 10s session-change timer ({}) is active", active_timer.reason),
        }));
    }
    let Some(name) = args
        .first()
        .and_then(|a| a.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return Some(Err(BridgeRpcError { code: "InvalidCorporationName".into(), message: "Corporation name is required".into() }));
    };
    let ticker = args.get(1).and_then(|a| a.as_str()).unwrap_or("EVOMU");
    let (description, url, tax_rate, shape1, shape2, shape3, color1, color2, color3, apps_enabled) =
        if args.get(2).and_then(|a| a.as_str()).is_some() {
            let tax = args.get(4).and_then(|a| a.as_f64()).unwrap_or(0.10).clamp(0.0, 1.0);
            let apps = args.get(12).map(|a| match a { PyRep::Bool(b) => *b, PyRep::Integer(i) => *i != 0, _ => true }).unwrap_or(true);
            (args.get(2).and_then(|a| a.as_str()).unwrap_or(""), args.get(3).and_then(|a| a.as_str()).unwrap_or(""), tax,
             args.get(5).and_then(|a| a.as_i64()).unwrap_or(415), args.get(6).and_then(|a| a.as_i64()).unwrap_or(416), args.get(7).and_then(|a| a.as_i64()).unwrap_or(417),
             args.get(8).and_then(|a| a.as_i64()).unwrap_or(671), args.get(9).and_then(|a| a.as_i64()).unwrap_or(672), args.get(10).and_then(|a| a.as_i64()).unwrap_or(673), apps)
        } else {
            let tax = args.get(2).and_then(|a| a.as_f64()).unwrap_or(0.10).clamp(0.0, 1.0);
            ("", "", tax, 415, 416, 417, 671, 672, 673, true)
        };
    let reg_fee = IskCents::from_isk(1_599_800);
    let now_unix = ctx.now_unix();

    let res = ctx.db.try_with_mut_snapshot(|snap| {
        let char_corp_id = snap
            .characters
            .get(&char_id)
            .map(|c| c.corporation_id)
            .unwrap_or(0);
        if snap
            .corporations
            .get(&char_corp_id)
            .is_some_and(|c| c.ceo_id == char_id)
        {
            return Err(BridgeRpcError {
                code: "AlreadyCEO".into(),
                message: "You are already the CEO of a corporation".into(),
            });
        }
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
        let char_station_id = ch.station_id;
        let new_bal = ch.balance.checked_sub(reg_fee).ok_or_else(|| BridgeRpcError {
            code: "InsufficientFunds".into(),
            message: "Not enough ISK to pay corporation registration fee (1,599,800 ISK)".into(),
        })?;
        ch.balance = new_bal;
        snap.append_journal(
            char_id,
            "corporation_registration_fee",
            IskCents(-reg_fee.0),
            new_bal,
            format!("Registered corporation {name}"),
        );

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
                description: description.to_string(),
                url: url.to_string(),
                hq_station_id: session.station_id.or(char_station_id),
            },
        );
        snap.chat_channels.insert(format!("corpid:{cid}"), ChatChannelRecord { channel_id: format!("corpid:{cid}"), name: name.to_string(), motd: String::new(), owner_id: Some(char_id), members: HashSet::from([char_id]), is_system_local: false, solar_system_id: None, is_private: false, operators: HashSet::from([char_id]), password: None, is_delayed_local: false });
        let state = snap.corp_governance.entry(cid).or_default();
        state.shares.insert(char_id, 1_000);
        state.company_shares = 0;
        state.logo = (shape1, shape2, shape3, color1);
        state.logo_extra_colors = (color2, color3);
        state.applications_enabled = Some(apps_enabled);
        state.accept_structures = true;
        state.accept_structures_set = true;
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
            let _ = crate::services::quasar_social_chat::on_character_local_chat_membership_refreshed(
                &ctx.quasar_gateway, &ctx.db, char_id as u32, now_unix,
            );
            let sys_id = session.solar_system_id.unwrap_or(0);
            crate::services::space_destiny::record_session_change_timer(
                ctx,
                char_id,
                now_unix,
                sys_id,
                0,
                "corp_change",
            );
            Some(Ok(PyRep::keyval([
                ("corporationID", PyRep::Integer(corp_id)), ("allianceID", PyRep::None),
                ("name", PyRep::String(name.to_string())), ("ticker", PyRep::String(ticker.to_string())),
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
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (_, char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let corp_id = session
        .corporation_id
        .filter(|&id| id > 0)
        .or_else(|| ctx.db.with_snapshot(|s| s.characters.get(&char_id).map(|c| c.corporation_id)))
        .unwrap_or(0);
    super::fleet_social_corps::promote_due_pending_corp_grants(ctx, corp_id);
    let now_unix = ctx.now_unix();
    let (has_director, has_comms, has_diplomat) = ctx.db.with_snapshot(|snap| {
        snap.corporations.get(&corp_id).map_or((false, false, false), |c| {
            let r = c.member_roles.get(&char_id).copied().unwrap_or(0);
            let dir = c.ceo_id == char_id || corp_roles::has_role(r, corp_roles::ROLE_DIRECTOR);
            let comms = dir || corp_roles::has_role(r, corp_roles::ROLE_PERSONNEL_MANAGER);
            let dip = dir || corp_roles::has_role(r, 1u64 << 42);
            (dir, comms, dip)
        })
    });
    let deny_role = |msg: &str| Some(Err(BridgeRpcError { code: "CorpRoleDenied".into(), message: msg.into() }));

    match (service, method) {
        ("corpRegistry", "MoveCompanyShares") => {
            if !has_director {
                return deny_role("MoveCompanyShares requires CEO or Director role");
            }
            let (target_corp_id, target_id, qty) = if args.len() >= 3 {
                (
                    args.first().and_then(|a| a.as_i64()).unwrap_or(corp_id),
                    args.get(1).and_then(|a| a.as_i64()).unwrap_or(char_id),
                    args.get(2).and_then(|a| a.as_i64()).unwrap_or(0),
                )
            } else {
                (
                    corp_id,
                    args.first().and_then(|a| a.as_i64()).unwrap_or(char_id),
                    args.get(1).and_then(|a| a.as_i64()).unwrap_or(0),
                )
            };
            if target_corp_id != corp_id {
                return deny_role("Cannot move company shares of another corporation");
            }
            if target_id <= 0 || qty <= 0 {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "Share transfer requires positive recipient ID and share count".into(),
                }));
            }
            let tx_res = ctx.db.try_with_mut_snapshot(|snap| {
                let state = snap.corp_governance.entry(corp_id).or_default();
                let corp_share_entry = state.shares.get(&corp_id).copied().unwrap_or(0);
                let char_share_entry = state.shares.get(&char_id).copied().unwrap_or(0);
                let (from_holder_id, old_f, new_f) = if corp_share_entry >= qty {
                    let from_count = state.shares.entry(corp_id).or_insert(0);
                    let of = *from_count;
                    *from_count -= qty;
                    let nf = *from_count;
                    if state.company_shares > 0 {
                        state.company_shares = (state.company_shares - qty).max(0);
                    }
                    (corp_id, of, nf)
                } else if state.company_shares >= qty && char_share_entry < qty {
                    let of = state.company_shares;
                    state.company_shares -= qty;
                    let nf = state.company_shares;
                    (corp_id, of, nf)
                } else if char_share_entry >= qty {
                    let from_count = state.shares.entry(char_id).or_insert(0);
                    let of = *from_count;
                    *from_count -= qty;
                    let nf = *from_count;
                    if state.company_shares > 0 {
                        state.company_shares = (state.company_shares - qty).max(0);
                    }
                    (char_id, of, nf)
                } else {
                    return Err(BridgeRpcError {
                        code: "InsufficientShares".into(),
                        message: "Not enough company shares to transfer".into(),
                    });
                };
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
                Ok((from_holder_id, old_f, new_f, old_t, new_t))
            });
            let (from_holder_id, old_from, new_from, old_to, new_to) = match tx_res {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if target_id != from_holder_id {
                let from_change = PyRep::Dict(vec![(
                    PyRep::String(("shares").into()),
                    PyRep::Tuple(vec![PyRep::Integer(old_from), PyRep::Integer(new_from)]),
                )]);
                ctx.notify(
                    NotificationTarget::Corporation(corp_id),
                    "OnShareChange",
                    "corpid",
                    vec![PyRep::Integer(from_holder_id), PyRep::Integer(corp_id), from_change],
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

        ("corpRegistry", "MovePrivateShares") => {
            let (target_corp_id, target_id, qty) = if args.len() >= 3 {
                (
                    args.first().and_then(|a| a.as_i64()).unwrap_or(corp_id),
                    args.get(1).and_then(|a| a.as_i64()).unwrap_or(char_id),
                    args.get(2).and_then(|a| a.as_i64()).unwrap_or(0),
                )
            } else {
                (
                    corp_id,
                    args.first().and_then(|a| a.as_i64()).unwrap_or(char_id),
                    args.get(1).and_then(|a| a.as_i64()).unwrap_or(0),
                )
            };
            if target_id <= 0 || qty <= 0 {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "Share transfer requires positive recipient ID and share count".into(),
                }));
            }
            let tx_res = ctx.db.try_with_mut_snapshot(|snap| {
                let state = snap.corp_governance.entry(target_corp_id).or_default();
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
                snap.corp_shareholders.insert(target_corp_id, holders);
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
                    NotificationTarget::Corporation(target_corp_id),
                    "OnShareChange",
                    "corpid",
                    vec![PyRep::Integer(char_id), PyRep::Integer(target_corp_id), from_change],
                );
            }
            let to_change = PyRep::Dict(vec![(
                PyRep::String(("shares").into()),
                PyRep::Tuple(vec![PyRep::Integer(old_to), PyRep::Integer(new_to)]),
            )]);
            let to_args = vec![PyRep::Integer(target_id), PyRep::Integer(target_corp_id), to_change];
            ctx.notify(NotificationTarget::Corporation(target_corp_id), "OnShareChange", "corpid", to_args.clone());
            let target_in_corp = ctx.db.with_snapshot(|snap| {
                snap.corporations.get(&target_corp_id).is_some_and(|c| c.members.contains(&target_id))
            });
            if !target_in_corp {
                ctx.notify(NotificationTarget::Character(target_id), "OnShareChange", "charid", to_args);
            }
            Some(Ok(PyRep::keyval([
                ("corporationID", PyRep::Integer(target_corp_id)),
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
            handle_insert_vote_case(ctx, corp_id, char_id, now_unix, args)
        }

        ("voteManager", "InsertVote") | ("corpRegistry", "InsertVote") => {
            let (case_id, option_id) = if args.len() >= 3 {
                (
                    args.get(1).and_then(|a| a.as_i64()).unwrap_or(1),
                    args.get(2).and_then(|a| a.as_i64()).unwrap_or(1),
                )
            } else {
                (
                    args.first().and_then(|a| a.as_i64()).unwrap_or(1),
                    args.get(1).and_then(|a| a.as_i64()).unwrap_or(1),
                )
            };
            let weight = match ctx.db.try_with_mut_snapshot(|snap| -> Result<i64, BridgeRpcError> {
                let vc = snap.corp_vote_cases.get(&case_id).ok_or_else(|| BridgeRpcError { code: "VoteCaseNotFound".into(), message: format!("Vote case {case_id} not found") })?;
                if vc.corporation_id != corp_id
                    || (args.len() >= 3 && args.first().and_then(|a| a.as_i64()).is_some_and(|cid| cid != vc.corporation_id))
                {
                    return Err(BridgeRpcError {
                        code: "CorpAccessDenied".into(),
                        message: "Vote case belongs to a different corporation".into(),
                    });
                }
                if now_unix > vc.end_ts { return Err(BridgeRpcError { code: "VoteCaseClosed".into(), message: "Voting period has closed for this case".into() }); }
                let vc_corp = vc.corporation_id;
                let sh_weight = snap
                    .corp_shareholders
                    .get(&vc_corp)
                    .and_then(|shs| shs.iter().find(|s| s.shareholder_id == char_id).map(|s| s.share_count))
                    .or_else(|| {
                        snap.corp_governance
                            .get(&vc_corp)
                            .and_then(|g| g.shares.get(&char_id).copied())
                    });
                let is_ceo_or_member = snap
                    .corporations
                    .get(&vc_corp)
                    .is_some_and(|c| c.ceo_id == char_id || c.members.contains(&char_id));
                let w = match sh_weight {
                    Some(s) if s > 0 => s,
                    _ if is_ceo_or_member => 1,
                    _ => {
                        return Err(BridgeRpcError {
                            code: "CorpAccessDenied".into(),
                            message: "Only corporation shareholders or members may vote".into(),
                        });
                    }
                };
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
            handle_vote_queries(ctx, corp_id, now_unix, method, args)
        }

        ("voteManager", "ActivateSanctionedAction") => {
            if !has_director { return deny_role("Activating sanctioned actions requires CEO or Director role"); }
            let case_id = if args.len() >= 2 && args.first().and_then(|a| a.as_i64()) == Some(corp_id) {
                args.get(1).and_then(|a| a.as_i64()).unwrap_or(1)
            } else {
                args.first().and_then(|a| a.as_i64()).unwrap_or(1)
            };
            let mut old_ceo_id = char_id;
            let mut new_ceo_id = char_id;
            let mut is_ceo_vote = false;
            let mut share_delta: Option<(i64, i64)> = None;
            let res = ctx.db.try_with_mut_snapshot(|snap| -> Result<(), BridgeRpcError> {
                let (passed, win_param, _) = {
                    let vc = snap.corp_vote_cases.get(&case_id).ok_or_else(|| BridgeRpcError { code: "VoteCaseNotFound".into(), message: format!("Vote case {case_id} not found") })?;
                    if vc.corporation_id != corp_id {
                        return Err(BridgeRpcError {
                            code: "CorpAccessDenied".into(),
                            message: "Vote case belongs to a different corporation".into(),
                        });
                    }
                    if vc.executed { return Err(BridgeRpcError { code: "VoteAlreadyActivated".into(), message: "Sanctioned action has already been activated".into() }); }
                    if now_unix < vc.end_ts { return Err(BridgeRpcError { code: "VoteStillOpen".into(), message: "Cannot activate sanctioned action before voting closes".into() }); }
                    eval_vote_case_winner(vc, &snap.corp_vote_ballots)
                };
                if !passed { return Err(BridgeRpcError { code: "VoteDidNotPass".into(), message: "Sanctioned action did not pass shareholder vote".into() }); }
                let vc = snap.corp_vote_cases.get_mut(&case_id).unwrap();
                vc.executed = true;
                vc.parameter = win_param;
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
                    new_ceo_id = win_param;
                    let corp = snap.corporations.get_mut(&corp_id).ok_or_else(|| BridgeRpcError { code: "CorpNotFound".into(), message: "Corporation not found".into() })?;
                    if !corp.members.contains(&new_ceo_id) {
                        return Err(BridgeRpcError { code: "CandidateNotMember".into(), message: "CEO election candidate is not a member of the corporation".into() });
                    }
                    old_ceo_id = corp.ceo_id;
                    corp.ceo_id = new_ceo_id;
                    corp.member_roles.insert(new_ceo_id, u64::MAX);
                } else if vc.vote_type != 5 && win_param > 0 {
                    if let Some(it) = snap.items.get_mut(&win_param) {
                        if it.owner_id == corp_id {
                            let is_unlock = vc.vote_type == 7 || vc.text.to_ascii_lowercase().contains("unlock");
                            it.flag = if is_unlock { eve_db::InventoryFlag::Hangar } else { eve_db::InventoryFlag::Locked };
                            it.version += 1;
                        }
                    }
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

        // parity: feat:character.medals-display, rpc:corporationSvc.CreateMedal, rpc:corporationSvc.GiveMedalToCharacters, rpc:corporationSvc.SetMedalStatus, rpc:corporationSvc.GetAllCorpMedals, rpc:corporationSvc.GetMedalDetails, rpc:corporationSvc.GetMedalsReceived, rpc:corporationSvc.GetRecipientsOfMedal
        ("corporationSvc", "CreateMedal")
        | ("corporationSvc", "GiveMedalToCharacters")
        | ("corporationSvc", "SetMedalStatus")
        | ("corporationSvc", "GetAllCorpMedals")
        | ("corporationSvc", "GetMedalDetails")
        | ("corporationSvc", "GetMedalsReceived")
        | ("corporationSvc", "GetRecipientsOfMedal") => {
            handle_corp_medals(ctx, corp_id, char_id, now_unix, has_comms, method, args)
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

        ("corpRegistry", "AddBulletin")
        | ("corpRegistry", "UpdateBulletinOrder")
        | ("corpRegistry", "GetBulletins")
        | ("corpRegistry", "DeleteBulletin") => {
            handle_corp_bulletins(ctx, session, corp_id, char_id, now_unix, has_comms, method, args)
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
            let (days, type_mask, title, desc, lang_mask) = if args.first().and_then(|a| a.as_str()).is_some() {
                let title = args.first().and_then(|a| a.as_str()).unwrap_or("Nullsec PvP & Industry");
                let desc = args.get(1).and_then(|a| a.as_str()).unwrap_or("Join our corp today");
                let type_mask = args.get(2).and_then(|a| a.as_i64()).unwrap_or(7) as u64;
                let days = args.get(3).and_then(|a| a.as_i64()).or_else(|| kwargs.get("days").and_then(|a| a.as_i64())).unwrap_or(30) as u32;
                let lang = args.get(4).and_then(|a| a.as_i64()).or_else(|| kwargs.get("langMask").and_then(|a| a.as_i64())).unwrap_or(0) as u64;
                (days, type_mask, title.to_string(), desc.to_string(), lang)
            } else {
                let days = args.first().and_then(|a| a.as_i64()).or_else(|| kwargs.get("days").and_then(|a| a.as_i64())).unwrap_or(30) as u32;
                let type_mask = args.get(1).and_then(|a| a.as_i64()).or_else(|| kwargs.get("typeMask").and_then(|a| a.as_i64())).unwrap_or(7) as u64;
                let title = args.get(2).and_then(|a| a.as_str()).unwrap_or("Updated Corp Ad");
                let desc = args.get(3).and_then(|a| a.as_str()).unwrap_or("Updated description");
                let lang = args.get(4).and_then(|a| a.as_i64()).or_else(|| kwargs.get("langMask").and_then(|a| a.as_i64())).unwrap_or(0) as u64;
                (days, type_mask, title.to_string(), desc.to_string(), lang)
            };
            let mut ads = load_corp_rec_ads(ctx);
            let ad_id = if method == "UpdateRecruitmentAd" {
                let id = args.first().and_then(|a| a.as_i64()).unwrap_or(1);
                if let Some(ad) = ads.iter_mut().find(|a| a.ad_id == id) {
                    ad.title = title.clone();
                    ad.description = desc.clone();
                    ad.type_mask = type_mask;
                    if lang_mask != 0 { ad.lang_mask = lang_mask; }
                }
                with_corp_gov(ctx, |g| {
                    if let Some(state) = g.get_mut(&corp_id) {
                        if let Some(ad) = state.recruitment_ads.iter_mut().find(|a| a.0 == id) {
                            ad.1 = title.clone();
                            ad.2 = desc.clone();
                            ad.3 = type_mask;
                        }
                    }
                });
                id
            } else {
                let id = ads.iter().map(|a| a.ad_id).max().unwrap_or(0) + 1;
                let expiry_unix = now_unix.saturating_add((days.max(1) as u64).saturating_mul(86_400));
                ads.push(CorpRecruitmentAd {
                    ad_id: id,
                    corp_id,
                    title: title.clone(),
                    description: desc.clone(),
                    type_mask,
                    recruiter_id: char_id,
                    created_unix: now_unix,
                    expiry_unix,
                    days,
                    lang_mask,
                });
                with_corp_gov(ctx, |g| {
                    g.entry(corp_id).or_default().recruitment_ads.push((id, title.clone(), desc.clone(), type_mask, char_id));
                });
                id
            };
            save_corp_rec_ads(ctx, &ads);
            ctx.notify(
                NotificationTarget::Corporation(corp_id),
                "OnCorporationRecruitmentAdChanged",
                "corpid",
                vec![PyRep::Integer(corp_id), PyRep::Integer(ad_id)],
            );
            Some(Ok(PyRep::keyval([
                ("adID", PyRep::Integer(ad_id)),
                ("corporationID", PyRep::Integer(corp_id)),
                ("title", PyRep::String(title)),
                ("typeMask", PyRep::Integer(type_mask as i64)),
            ])))
        }

        ("corpRecProxy", "GetRecruiters") => {
            let target_corp = args.first().and_then(|a| a.as_i64()).unwrap_or(corp_id);
            let target_ad = args.get(1).and_then(|a| a.as_i64()).unwrap_or(1);
            let ads = load_corp_rec_ads(ctx);
            let recruiters: Vec<PyRep> = ads.iter()
                .filter(|a| a.corp_id == target_corp && a.ad_id == target_ad)
                .map(|a| PyRep::Integer(a.recruiter_id))
                .collect();
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
            let filter_ad = if method == "GetRecruitmentAd" { args.first().and_then(|a| a.as_i64()) } else { None };
            let (filter_mask, filter_lang, min_members, max_members) = if method == "SearchCorpAds" {
                let mask = args.first().and_then(|a| a.as_i64()).or_else(|| kwargs.get("typeMask").and_then(|a| a.as_i64())).unwrap_or(0) as u64;
                let lang = args.get(1).and_then(|a| a.as_i64()).or_else(|| kwargs.get("langMask").and_then(|a| a.as_i64())).or_else(|| kwargs.get("languageMask").and_then(|a| a.as_i64())).unwrap_or(0) as u64;
                let min_m = args.get(2).and_then(|a| a.as_i64()).or_else(|| kwargs.get("minMembers").and_then(|a| a.as_i64()));
                let max_m = args.get(3).and_then(|a| a.as_i64()).or_else(|| kwargs.get("maxMembers").and_then(|a| a.as_i64()));
                (mask, lang, min_m, max_m)
            } else { (0, 0, None, None) };
            let ads = load_corp_rec_ads(ctx);
            let mut rows = Vec::new();
            for ad in &ads {
                if method == "SearchCorpAds" {
                    if now_unix >= ad.expiry_unix {
                        continue;
                    }
                    if filter_mask != 0 && (ad.type_mask & filter_mask) == 0 {
                        continue;
                    }
                    if filter_lang != 0 && (ad.lang_mask & filter_lang) == 0 {
                        continue;
                    }
                    let member_count = ctx.db.with_snapshot(|s| {
                        s.corporations.get(&ad.corp_id).map(|c| c.members.len()).unwrap_or(1) as i64
                    });
                    if let Some(min_m) = min_members {
                        if member_count < min_m { continue; }
                    }
                    if let Some(max_m) = max_members {
                        if max_m > 0 && member_count > max_m { continue; }
                    }
                } else {
                    if filter_corp.is_some_and(|fc| fc != ad.corp_id) {
                        continue;
                    }
                    if filter_ad.is_some_and(|fa| fa != ad.ad_id) {
                        continue;
                    }
                }
                rows.push(vec![
                    PyRep::Integer(ad.ad_id),
                    PyRep::Integer(ad.corp_id),
                    PyRep::String(ad.title.clone()),
                    PyRep::String(ad.description.clone()),
                    PyRep::Integer(ad.type_mask as i64),
                    PyRep::Integer(ad.recruiter_id),
                ]);
            }
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
            let mut ads = load_corp_rec_ads(ctx);
            ads.retain(|a| a.ad_id != ad_id || a.corp_id != corp_id);
            save_corp_rec_ads(ctx, &ads);
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
        | ("corpRegistry", "EditContactsRelationshipID")
        | ("corpRegistry", "DeleteLabel")
        | ("corpRegistry", "AssignLabels")
        | ("corpRegistry", "RemoveLabels") => {
            handle_corp_contacts_and_labels(ctx, corp_id, has_diplomat, method, args)
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
                let old = if s.accept_structures_set { s.accept_structures } else { true };
                s.accept_structures_set = true;
                s.accept_structures = accept;
                old
            });
            let change = PyRep::Dict(vec![(PyRep::String(("acceptStructures").into()), PyRep::Tuple(vec![PyRep::Bool(prev), PyRep::Bool(accept)]))]);
            ctx.notify(NotificationTarget::Corporation(corp_id), "OnCorporationChanged", "corpid", vec![PyRep::Integer(corp_id), change]);
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
