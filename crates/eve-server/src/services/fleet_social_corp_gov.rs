//! Corporation Governance, Shares, Dividends, Titles, Votes, Medals, Bulletins, Recruitment & Alliances (`SO-3`).

use crate::router::{cache_notifications::NotificationTarget, ServerContext};
use eve_db::{
    corp_roles, ChatChannelRecord, CorpVoteBallotRecord, CorpWalletDivisionRecord,
    CorporationRecord, IskCents, WalletJournalEntry,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::{HashMap, HashSet};
use super::fleet_social_corps::{handle_insert_vote_case, read_corp_gov, with_corp_gov};
use super::fleet_social_notify::handle_corp_contacts_and_labels;
use crate::services::space_entity_ops::{handle_corp_bulletins, handle_corp_medals};
use super::fleet_social_persistence::{
    eval_vote_case_winner, handle_vote_queries, label_mask_from_ids,
};

pub use eve_db::CorpRecruitmentAd;

fn load_corp_rec_ads(ctx: &ServerContext) -> Vec<CorpRecruitmentAd> {
    let mut ads: Vec<CorpRecruitmentAd> = ctx
        .db
        .with_snapshot(|snap| snap.fleet_and_social_ops.corp_recruitment_ads.clone());
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
    ctx.db.with_mut_snapshot(|snap| {
        snap.fleet_and_social_ops.corp_recruitment_ads = ads.to_vec();
    });
}

fn handle_corp_rec_queries(
    ctx: &ServerContext,
    corp_id: i64,
    now_unix: u64,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let filter_corp = match method {
        "GetRecruitmentAdsByCorpID" => args.first().and_then(|a| a.as_i64()).or(Some(corp_id)),
        "GetRecruitmentAdsForCorporation" => Some(corp_id),
        _ => None,
    };
    let filter_ad = if method == "GetRecruitmentAd" { args.first().and_then(|a| a.as_i64()) } else { None };
    let (filter_mask, filter_lang, min_members, max_members) = if method == "SearchCorpAds" {
        let mask = args.first().and_then(|a| a.as_i64()).or_else(|| kwargs.get("typeMask").and_then(|a| a.as_i64())).unwrap_or(0) as u64;
        let lang = args.get(1).and_then(|a| a.as_i64()).or_else(|| kwargs.get("langMask").or_else(|| kwargs.get("languageMask")).and_then(|a| a.as_i64())).unwrap_or(0) as u64;
        let min_m = args.get(2).and_then(|a| a.as_i64()).or_else(|| kwargs.get("minMembers").and_then(|a| a.as_i64()));
        let max_m = args.get(3).and_then(|a| a.as_i64()).or_else(|| kwargs.get("maxMembers").and_then(|a| a.as_i64()));
        (mask, lang, min_m, max_m)
    } else { (0, 0, None, None) };
    let ads = load_corp_rec_ads(ctx);
    let mut rows = Vec::new();
    for ad in &ads {
        if method == "SearchCorpAds" {
            if now_unix >= ad.expiry_unix || (filter_mask != 0 && (ad.type_mask & filter_mask) == 0) || (filter_lang != 0 && (ad.lang_mask & filter_lang) == 0) {
                continue;
            }
            let member_count = ctx.db.with_snapshot(|s| s.corporations.get(&ad.corp_id).map(|c| c.members.len()).unwrap_or(1) as i64);
            if min_members.is_some_and(|min_m| member_count < min_m) || max_members.is_some_and(|max_m| max_m > 0 && member_count > max_m) {
                continue;
            }
        } else {
            if filter_corp.is_some_and(|fc| fc != ad.corp_id) || filter_ad.is_some_and(|fa| fa != ad.ad_id) {
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
            client_error: None,
        }));
    }
    let Some(name) = args
        .first()
        .and_then(|a| a.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return Some(Err(BridgeRpcError { code: "InvalidCorporationName".into(), message: "Corporation name is required".into(), client_error: None }));
    };
    let rpc = crate::router::RpcArgs::positional(args);
    let ticker = match rpc.opt_str(1, "ticker") {
        Ok(v) => v.unwrap_or("EVOMU"),
        Err(e) => return Some(Err(e)),
    };
    let (description, url, tax_rate, shape1, shape2, shape3, color1, color2, color3, apps_enabled) =
        if args.get(2).and_then(|a| a.as_str()).is_some() {
            let tax = match rpc.opt_finite_f64(4, "taxRate") {
                Ok(v) => v.unwrap_or(0.10).clamp(0.0, 1.0),
                Err(e) => return Some(Err(e)),
            };
            let apps = args.get(12).map(|a| match a { PyRep::Bool(b) => *b, PyRep::Integer(i) => *i != 0, _ => true }).unwrap_or(true);
            (args.get(2).and_then(|a| a.as_str()).unwrap_or(""), args.get(3).and_then(|a| a.as_str()).unwrap_or(""), tax,
             args.get(5).and_then(|a| a.as_i64()).unwrap_or(415), args.get(6).and_then(|a| a.as_i64()).unwrap_or(416), args.get(7).and_then(|a| a.as_i64()).unwrap_or(417),
             args.get(8).and_then(|a| a.as_i64()).unwrap_or(671), args.get(9).and_then(|a| a.as_i64()).unwrap_or(672), args.get(10).and_then(|a| a.as_i64()).unwrap_or(673), apps)
        } else {
            let tax = match rpc.opt_finite_f64(2, "taxRate") {
                Ok(v) => v.unwrap_or(0.10).clamp(0.0, 1.0),
                Err(e) => return Some(Err(e)),
            };
            ("", "", tax, 415, 416, 417, 671, 672, 673, true)
        };
    let reg_fee = IskCents::from_isk(1_599_800);
    let now_unix = ctx.now_unix();
    if let Some(cur_cid) = ctx.db.with_snapshot(|s| s.characters.get(&char_id).map(|c| c.corporation_id).or(session.corporation_id)).filter(|&id| id > 0) {
        super::fleet_social_corps::promote_due_pending_corp_grants(ctx, cur_cid);
    }
    let sess_in_space = session.station_id.is_none() && session.structure_id.is_none() && session.ship_id.is_some();

    let res = ctx.db.try_with_mut_snapshot(|snap| {
        let ch_ro = snap.characters.get(&char_id).ok_or_else(|| BridgeRpcError { code: "CharacterNotFound".into(), message: format!("Character {char_id} not found"), client_error: None })?;
        let invalid_st = session.structure_id.or(ch_ro.structure_id).is_some_and(|sid| {
            snap.structures.get(&sid).is_some_and(|st| matches!(st.state.as_str(), "Destroyed" | "Unanchored"))
        });
        let undocked = ch_ro.station_id.is_none() && ch_ro.structure_id.is_none() && session.station_id.is_none() && session.structure_id.is_none();
        if sess_in_space || undocked || invalid_st {
            return Err(BridgeRpcError { code: "MustBeDocked".into(), message: "You must be docked in a station or operational structure to create a corporation".into(), client_error: None });
        }
        let char_corp_id = ch_ro.corporation_id;
        if snap.corporations.get(&char_corp_id).is_some_and(|c| c.ceo_id == char_id) {
            return Err(BridgeRpcError { code: "AlreadyCEO".into(), message: "You are already the CEO of a corporation".into(), client_error: None });
        }
        if snap.corporations.contains_key(&char_corp_id) {
            if let Some(blocker) = super::fleet_social_corp_tail::member_kick_blocker(snap, char_corp_id, char_id, now_unix) {
                return Err(BridgeRpcError { code: blocker.into(), message: format!("Cannot leave current corporation to found a new one ({blocker})"), client_error: None });
            }
        }
        let corp_mgmt_lvl = snap.skills.get(&char_id).and_then(|m| m.get(&3363)).map(|s| s.level).unwrap_or(0);
        if corp_mgmt_lvl < 1 {
            return Err(BridgeRpcError { code: "MissingCorporationManagementSkill".into(), message: "Creating a corporation requires Corporation Management (3363) level 1".into(), client_error: None });
        }
        if snap
            .corporations
            .values()
            .any(|c| c.name.eq_ignore_ascii_case(name) || c.ticker.eq_ignore_ascii_case(ticker))
        {
            return Err(BridgeRpcError {
                code: "CorpNameOrTickerTaken".into(),
                message: "Corporation name or ticker is already in use".into(),
                client_error: None,
            });
        }
        let Some(ch) = snap.characters.get_mut(&char_id) else {
            return Err(BridgeRpcError {
                code: "CharacterNotFound".into(),
                message: format!("Character {char_id} not found"),
                client_error: None,
            });
        };
        let old_corp_id = ch.corporation_id;
        let char_station_id = ch.station_id;
        let new_bal = ch.balance.checked_sub(reg_fee).ok_or_else(|| BridgeRpcError {
            code: "InsufficientFunds".into(),
            message: "Not enough ISK to pay corporation registration fee (1,599,800 ISK)".into(),
            client_error: None,
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
        for old_corp in snap.corporations.values_mut() {
            old_corp.members.remove(&char_id);
            old_corp.member_roles.remove(&char_id);
        }
        if let Some(ch_mut) = snap.characters.get_mut(&char_id) {
            ch_mut.corporation_id = cid;
            ch_mut.alliance_id = None;
        }

        let wallet_divisions: Vec<CorpWalletDivisionRecord> = (1..=7).map(|d| CorpWalletDivisionRecord {
            division_id: d, name: if d == 1 { "Master Wallet".into() } else { format!("Division {d}") }, balance: IskCents(0),
        }).collect();

        let mut member_roles = HashMap::new();
        member_roles.insert(char_id, u64::MAX);

        let mut corp_rec = CorporationRecord::new(cid, name, ticker, char_id);
        corp_rec.tax_rate = tax_rate;
        corp_rec.members = HashSet::from([char_id]);
        corp_rec.member_roles = member_roles;
        corp_rec.wallet_divisions = wallet_divisions;
        corp_rec.description = description.to_string();
        corp_rec.url = url.to_string();
        corp_rec.hq_station_id = session.station_id.or(char_station_id);
        snap.corporations.insert(cid, corp_rec);
        snap.chat_channels.insert(format!("corpid:{cid}"), ChatChannelRecord { channel_id: format!("corpid:{cid}"), name: name.to_string(), motd: String::new(), owner_id: Some(char_id), members: HashSet::from([char_id]), is_system_local: false, solar_system_id: None, is_private: false, operators: HashSet::from([char_id]), password: None, is_delayed_local: false });
        let init_limit = crate::domain::corp_governance::corp_member_limit_from_skills(&ctx.sde, snap, char_id);
        snap.corp_member_limits.insert(cid, init_limit);
        let state = snap.corp_governance.entry(cid).or_default();
        state.member_limit = Some(init_limit as usize);
        state.shares.insert(cid, 1_000);
        state.company_shares = 1_000;
        state.logo = (shape1, shape2, shape3, color1);
        state.logo_extra_colors = (color2, color3);
        state.applications_enabled = Some(apps_enabled);
        state.accept_structures = true;
        state.accept_structures_set = true;
        state.reinforce_hour = 18;
        state.created_unix = now_unix;
        snap.corp_shareholders.insert(
            cid,
            vec![eve_db::snapshot_ext::CorpShareholderEntry {
                shareholder_id: cid,
                shareholder_type: "corporation".into(),
                share_count: 1_000,
            }],
        );
        snap.seed_default_corp_titles(cid);
        let released = super::fleet_social_corps::record_corp_join_in_snap(snap, char_id, old_corp_id, cid, now_unix);
        Ok((old_corp_id, cid, released))
    });

    match res {
        Ok((old_corp_id, corp_id, released)) => {
            crate::services::structure_control_ops::notify_released_structure_controllers(ctx, &released);
            let old_corp_role = session.corp_role;
            let old_alliance_id = session.alliance_id;
            session.corporation_id = Some(corp_id);
            session.alliance_id = None;
            session.corp_role = u64::MAX;
            let (act_ship, c_sys, c_dock) = ctx.db.with_snapshot(|s| {
                s.characters
                    .get(&char_id)
                    .map(|c| (c.active_ship_id, c.solar_system_id, c.station_id.or(c.structure_id)))
                    .unwrap_or((0, session.solar_system_id.unwrap_or(0), session.station_id.or(session.structure_id)))
            });
            if act_ship != 0 {
                let _ = ctx.execute_session_transition_sync(
                    Some(session),
                    crate::router::SessionTransitionSpec::for_kind(
                        char_id,
                        crate::router::TransitionKind::BoardShip { ship_id: act_ship },
                        "corp_change",
                    )
                    .with_apply_db(false)
                    .with_emit_on_session_changed(false),
                );
            }
            if old_corp_id != corp_id {
                ctx.invalidate_bound_objects_for_session_transition(
                    char_id,
                    c_sys,
                    c_dock,
                    "corp_change",
                );
            }
            let mut sess_changes = vec![
                (PyRep::String("corpid".into()), PyRep::Tuple(vec![PyRep::Integer(old_corp_id), PyRep::Integer(corp_id)])),
                (PyRep::String("corprole".into()), PyRep::Tuple(vec![PyRep::Integer(old_corp_role as i64), PyRep::Integer(u64::MAX as i64)])),
            ];
            if let Some(prev_ally) = old_alliance_id {
                sess_changes.push((PyRep::String("allianceid".into()), PyRep::Tuple(vec![PyRep::Integer(prev_ally), PyRep::None])));
            }
            ctx.emit_typed_with_id_type(
                NotificationTarget::Character(char_id),
                "clientID",
                crate::router::notify_registry::OnSessionChanged {
                    is_remote: false,
                    session: PyRep::Integer(session.session_id as i64),
                    change: PyRep::Dict(sess_changes),
                },
            );
            let change = PyRep::Dict(vec![
                (PyRep::String(("corporationID").into()), PyRep::Tuple(vec![PyRep::None, PyRep::Integer(corp_id)])),
                (PyRep::String(("corporationName").into()), PyRep::Tuple(vec![PyRep::None, PyRep::String(name.to_string())])),
                (PyRep::String(("tickerName").into()), PyRep::Tuple(vec![PyRep::None, PyRep::String(ticker.to_string())])),
                (PyRep::String(("ceoID").into()), PyRep::Tuple(vec![PyRep::None, PyRep::Integer(char_id)])),
            ]);
            ctx.emit_typed(
                NotificationTarget::Corporation(corp_id),
                crate::router::notify_registry::OnCorporationChanged { corp_id, change },
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
    let now_unix = ctx.now_unix();
    let res = ctx.db.with_snapshot(|snap| {
        let Some(corp) = snap.corporations.get(&corp_id) else {
            return Err(BridgeRpcError {
                code: "CorporationNotFound".into(),
                message: format!("Corporation {corp_id} not found"),
                client_error: None,
            });
        };
        let (is_ceo, caller_role) =
            snap.effective_corp_roles_at(corp_id, char_id, None, 0, now_unix);
        if !is_ceo
            && !corp_roles::has_role(caller_role, corp_roles::ROLE_ACCOUNTANT)
            && !corp_roles::has_role(caller_role, corp_roles::ROLE_JUNIOR_ACCOUNTANT)
        {
            return Err(BridgeRpcError {
                code: "CorpRoleDenied".into(),
                message: "Viewing corporation wallet divisions requires Accountant role".into(),
                client_error: None,
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
                    PyRep::Integer(999 + i64::from(d)),
                ]
            })
            .collect();
        Ok(rows)
    });
    match res {
        Ok(rows) => Some(Ok(PyRep::rowset(
            vec!["divisionID", "name", "balance", "key"],
            rows,
        ))),
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
        if !snap.corporations.contains_key(&corp_id) {
            return (false, false, false);
        }
        let (is_ceo, r) = crate::services::social_corp_fleet::social_corp_projects::effective_corp_member_roles(
            snap, corp_id, char_id, 0, now_unix,
        );
        let dir = is_ceo || (r & corp_roles::ROLE_DIRECTOR) != 0;
        let comms = dir || (r & (corp_roles::ROLE_PERSONNEL_MANAGER | corp_roles::ROLE_CHAT_MANAGER)) != 0;
        let dip = dir || (r & corp_roles::ROLE_DIPLOMAT) != 0;
        (dir, comms, dip)
    });
    let deny_role = |msg: &str| Some(Err(BridgeRpcError { code: "CorpRoleDenied".into(), message: msg.into(), client_error: None }));

    match (service, method) {
        ("corpRegistry", "MoveCompanyShares") => {
            if !has_director {
                return deny_role("MoveCompanyShares requires CEO or Director role");
            }
            let (target_corp_id, target_id, qty) = if args.len() >= 3 {
                (args.first().and_then(|a| a.as_i64()).unwrap_or(corp_id), args.get(1).and_then(|a| a.as_i64()).unwrap_or(char_id), args.get(2).and_then(|a| a.as_i64()).unwrap_or(0))
            } else {
                (corp_id, args.first().and_then(|a| a.as_i64()).unwrap_or(char_id), args.get(1).and_then(|a| a.as_i64()).unwrap_or(0))
            };
            if target_corp_id != corp_id {
                return deny_role("Cannot move company shares of another corporation");
            }
            if target_id <= 0 || qty <= 0 {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "Share transfer requires positive recipient ID and share count".into(),
                    client_error: None,
                }));
            }
            let tx_res = ctx.db.try_with_mut_snapshot(|snap| {
                if !snap.characters.contains_key(&target_id) && !snap.corporations.contains_key(&target_id) {
                    return Err(BridgeRpcError { code: "TargetNotFound".into(), message: format!("Share transfer recipient {target_id} does not exist"), client_error: None });
                }
                let state = snap.corp_governance.entry(corp_id).or_default();
                let from_count = state.shares.entry(corp_id).or_insert(0);
                if *from_count < qty {
                    return Err(BridgeRpcError {
                        code: "InsufficientShares".into(),
                        message: "Not enough company shares to transfer".into(),
                        client_error: None,
                    });
                }
                let old_f = *from_count;
                *from_count -= qty;
                let new_f = *from_count;
                state.company_shares = *from_count;
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
                        shareholder_type: if (98_000_000..99_000_000).contains(&sid) || sid == corp_id || snap.corporations.contains_key(&sid) {
                            "corporation".into()
                        } else {
                            "character".into()
                        },
                        share_count: cnt,
                    })
                    .collect();
                holders.sort_by_key(|s| s.shareholder_id);
                snap.corp_shareholders.insert(corp_id, holders);
                Ok((corp_id, old_f, new_f, old_t, new_t))
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
                ctx.emit_typed(
                    NotificationTarget::Corporation(corp_id),
                    crate::router::notify_registry::OnShareChange {
                        shareholder_id: from_holder_id,
                        corporation_id: corp_id,
                        change: from_change,
                    },
                );
            }
            let to_change = PyRep::Dict(vec![(
                PyRep::String(("shares").into()),
                PyRep::Tuple(vec![PyRep::Integer(old_to), PyRep::Integer(new_to)]),
            )]);
            let to_notif = crate::router::notify_registry::OnShareChange {
                shareholder_id: target_id,
                corporation_id: corp_id,
                change: to_change,
            };
            ctx.emit_typed(NotificationTarget::Corporation(corp_id), to_notif.clone());
            let target_in_corp = ctx.db.with_snapshot(|snap| {
                snap.corporations.get(&corp_id).is_some_and(|c| c.members.contains(&target_id))
            });
            if !target_in_corp {
                ctx.emit_typed_with_id_type(NotificationTarget::Character(target_id), "charid", to_notif);
            }
            Some(Ok(PyRep::keyval([("corporationID", PyRep::Integer(corp_id)), ("toShareholderID", PyRep::Integer(target_id)), ("sharesTransferred", PyRep::Integer(qty))])))
        }

        ("corpRegistry", "MovePrivateShares") => {
            let (target_corp_id, target_id, qty) = if args.len() >= 3 {
                (args.first().and_then(|a| a.as_i64()).unwrap_or(corp_id), args.get(1).and_then(|a| a.as_i64()).unwrap_or(char_id), args.get(2).and_then(|a| a.as_i64()).unwrap_or(0))
            } else {
                (corp_id, args.first().and_then(|a| a.as_i64()).unwrap_or(char_id), args.get(1).and_then(|a| a.as_i64()).unwrap_or(0))
            };
            if target_id <= 0 || qty <= 0 {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "Share transfer requires positive recipient ID and share count".into(),
                    client_error: None,
                }));
            }
            let tx_res = ctx.db.try_with_mut_snapshot(|snap| {
                if !snap.characters.contains_key(&target_id) && !snap.corporations.contains_key(&target_id) {
                    return Err(BridgeRpcError { code: "TargetNotFound".into(), message: format!("Share transfer recipient {target_id} does not exist"), client_error: None });
                }
                let state = snap.corp_governance.entry(target_corp_id).or_default();
                let from_count = state.shares.entry(char_id).or_insert(0);
                if *from_count < qty {
                    return Err(BridgeRpcError {
                        code: "InsufficientShares".into(),
                        message: "Not enough shares to transfer".into(),
                        client_error: None,
                    });
                }
                let old_f = *from_count;
                *from_count -= qty;
                let new_f = *from_count;
                let to_entry = state.shares.entry(target_id).or_insert(0);
                let old_t = *to_entry;
                *to_entry += qty;
                let new_t = *to_entry;
                if target_id == target_corp_id {
                    state.company_shares = *to_entry;
                }
                let mut holders: Vec<eve_db::snapshot_ext::CorpShareholderEntry> = state
                    .shares
                    .iter()
                    .filter(|&(_, &cnt)| cnt > 0)
                    .map(|(&sid, &cnt)| eve_db::snapshot_ext::CorpShareholderEntry {
                        shareholder_id: sid,
                        shareholder_type: if (98_000_000..99_000_000).contains(&sid) || sid == target_corp_id || snap.corporations.contains_key(&sid) {
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
                let from_change = PyRep::Dict(vec![(PyRep::String("shares".into()), PyRep::Tuple(vec![PyRep::Integer(old_from), PyRep::Integer(new_from)]))]);
                ctx.emit_typed(
                    NotificationTarget::Corporation(target_corp_id),
                    crate::router::notify_registry::OnShareChange {
                        shareholder_id: char_id,
                        corporation_id: target_corp_id,
                        change: from_change,
                    },
                );
            }
            let to_change = PyRep::Dict(vec![(PyRep::String("shares".into()), PyRep::Tuple(vec![PyRep::Integer(old_to), PyRep::Integer(new_to)]))]);
            let to_notif = crate::router::notify_registry::OnShareChange {
                shareholder_id: target_id,
                corporation_id: target_corp_id,
                change: to_change,
            };
            ctx.emit_typed(NotificationTarget::Corporation(target_corp_id), to_notif.clone());
            let target_in_corp = ctx.db.with_snapshot(|snap| {
                snap.corporations.get(&target_corp_id).is_some_and(|c| c.members.contains(&target_id))
            });
            if !target_in_corp {
                ctx.emit_typed_with_id_type(NotificationTarget::Character(target_id), "charid", to_notif);
            }
            Some(Ok(PyRep::keyval([("corporationID", PyRep::Integer(target_corp_id)), ("toShareholderID", PyRep::Integer(target_id)), ("sharesTransferred", PyRep::Integer(qty))])))
        }

        ("corpRegistry", "GetSharesByShareholder") => {
            handle_get_shares_by_shareholder(ctx, session.has_gm_role(), char_id, corp_id, args)
        }

        ("corpRegistry", "GetShareholders")
        | ("corpRegistry", "GetMemberIDsWithMoreThanAvgShares") => {
            let shares = with_corp_gov(ctx, |g| {
                g.get(&corp_id).map(|s| s.shares.clone()).unwrap_or_default()
            });
            if method == "GetMemberIDsWithMoreThanAvgShares" {
                let members = ctx.db.with_snapshot(|snap| snap.corporations.get(&corp_id).map(|c| c.members.clone()).unwrap_or_default());
                let member_shares: Vec<(i64, i64)> = shares.into_iter().filter(|&(oid, cnt)| members.contains(&oid) && cnt > 0).collect();
                if member_shares.is_empty() { return Some(Ok(PyRep::List(Vec::new()))); }
                let total: i64 = member_shares.iter().map(|(_, cnt)| *cnt).sum();
                let avg = total / (member_shares.len() as i64);
                let ids: Vec<PyRep> = member_shares.into_iter().filter(|&(_, cnt)| cnt >= avg).map(|(oid, _)| PyRep::Integer(oid)).collect();
                return Some(Ok(PyRep::List(ids)));
            }
            let items: Vec<(PyRep, Vec<PyRep>)> = ctx.db.with_snapshot(|snap| {
                shares
                    .iter()
                    .filter(|(_, &cnt)| cnt > 0)
                    .map(|(&oid, &cnt)| {
                        let shareholder_corp_id = snap
                            .characters
                            .get(&oid)
                            .map(|c| PyRep::Integer(c.corporation_id))
                            .unwrap_or(PyRep::None);
                        (
                            PyRep::Integer(oid),
                            vec![
                                PyRep::Integer(oid),
                                PyRep::Integer(corp_id),
                                shareholder_corp_id,
                                PyRep::Integer(cnt),
                            ],
                        )
                    })
                    .collect()
            });
            Some(Ok(PyRep::index_rowset(
                "shareholderID",
                vec![
                    "shareholderID",
                    "corporationID",
                    "shareholderCorporationID",
                    "shares",
                ],
                items,
            )))
        }

        ("corpRegistry", "PayoutDividend") => {
            let rpc = crate::router::RpcArgs::positional(args);
            let (to_shareholders, total_isk, raw_div) = if matches!(args.first(), Some(PyRep::Bool(_))) {
                let b = match rpc.req_bool(0, "payShareholders") {
                    Ok(v) => v,
                    Err(e) => return Some(Err(e)),
                };
                let amt = match rpc.req_positive_f64(1, "payoutAmount") {
                    Ok(v) => v,
                    Err(e) => return Some(Err(e)),
                };
                (b, amt, args.get(2))
            } else if matches!(args.get(1), Some(PyRep::Bool(_))) {
                let amt = match rpc.req_positive_f64(0, "payoutAmount") {
                    Ok(v) => v,
                    Err(e) => return Some(Err(e)),
                };
                let b = match rpc.req_bool(1, "payShareholders") {
                    Ok(v) => v,
                    Err(e) => return Some(Err(e)),
                };
                (b, amt, args.get(2))
            } else if args.len() >= 2 && matches!(args.first(), Some(PyRep::Integer(0 | 1))) && matches!(args.get(1), Some(PyRep::Real(_))) {
                let b = args.first().and_then(|a| a.as_i64()).unwrap_or(0) != 0;
                let amt = match rpc.req_positive_f64(1, "payoutAmount") {
                    Ok(v) => v,
                    Err(e) => return Some(Err(e)),
                };
                (b, amt, args.get(2))
            } else {
                let amt = match rpc.req_positive_f64(0, "payoutAmount") {
                    Ok(v) => v,
                    Err(e) => return Some(Err(e)),
                };
                (matches!(args.get(1), Some(PyRep::Bool(true)) | Some(PyRep::Integer(1))), amt, args.get(2))
            };
            let div_id: u8 = match raw_div {
                Some(_) => match rpc.req_i64(2, "divisionID") {
                    Ok(val) if (1..=7).contains(&val) => val as u8,
                    _ => return Some(Err(BridgeRpcError { code: "InvalidArgument".into(), message: "Wallet division ID must be between 1 and 7".into(), client_error: None })),
                },
                None => 1,
            };
            if total_isk <= 0.0 || !total_isk.is_finite() {
                return Some(Err(BridgeRpcError { code: "InvalidArgument".into(), message: "Dividend payout amount must be positive".into(), client_error: None }));
            }
            let payout_cents = IskCents::from_f64_isk(total_isk);
            if payout_cents.0 <= 0 {
                return Some(Err(BridgeRpcError { code: "InvalidArgument".into(), message: "Dividend payout amount must be positive".into(), client_error: None }));
            }

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let (cur_bal, member_ids) = {
                    let (is_ceo, caller_role) =
                        snap.effective_corp_roles_at(corp_id, char_id, None, 0, now_unix);
                    let Some(corp) = snap.corporations.get_mut(&corp_id) else {
                        return Err(BridgeRpcError { code: "CorporationNotFound".into(), message: format!("Corporation {corp_id} not found"), client_error: None });
                    };
                    if !is_ceo && !corp_roles::has_role(caller_role, corp_roles::ROLE_DIRECTOR) {
                        return Err(BridgeRpcError { code: "CorpRoleDenied".into(), message: "Paying dividends requires CEO or Director role".into(), client_error: None });
                    }
                    let cur_bal = corp.division_balance(div_id);
                    if cur_bal < payout_cents {
                        return Err(BridgeRpcError { code: "InsufficientFunds".into(), message: "Insufficient corporation wallet funds for dividend payout".into(), client_error: None });
                    }
                    let mids: Vec<i64> = corp.members.iter().copied().collect();
                    (cur_bal, mids)
                };
                let recipients: Vec<(i64, i64)> = if to_shareholders {
                    let state = snap.corp_governance.entry(corp_id).or_default();
                    let shares_snapshot = state.shares.clone();
                    let total_shares = shares_snapshot.iter().filter(|&(_, &s)| s > 0).try_fold(0i64, |acc, (_, &s)| acc.checked_add(s)).ok_or_else(|| BridgeRpcError { code: "ArithmeticOverflow".into(), message: "Total shares overflowed".into(), client_error: None })?;
                    if total_shares <= 0 {
                        Vec::new()
                    } else {
                        shares_snapshot
                            .into_iter()
                            .filter(|&(oid, s)| oid != corp_id && s > 0)
                            .filter_map(|(oid, s)| {
                                let c = (payout_cents.0 as i128 * s as i128) / (total_shares as i128);
                                i64::try_from(c).ok().filter(|&cents| cents > 0).map(|cents| (oid, cents))
                            })
                            .collect()
                    }
                } else {
                    let mcount = (member_ids.len() as i64).max(1);
                    let per_member = payout_cents.0 / mcount;
                    if per_member > 0 { member_ids.into_iter().map(|mid| (mid, per_member)).collect() } else { Vec::new() }
                };

                let mut actual_cents: i64 = 0;
                let mut credited_count: i64 = 0;
                for &(rid, cents) in &recipients {
                    let bal_after = if let Some(ch) = snap.characters.get_mut(&rid) {
                        ch.balance = IskCents(ch.balance.0.saturating_add(cents));
                        ch.balance
                    } else if let Some(tc) = snap.corporations.get_mut(&rid) {
                        let b = IskCents(tc.division_balance(1).0.saturating_add(cents));
                        tc.set_division_balance(1, b);
                        b
                    } else {
                        continue;
                    };
                    actual_cents = actual_cents.checked_add(cents).ok_or_else(|| BridgeRpcError { code: "ArithmeticOverflow".into(), message: "Dividend cents overflowed".into(), client_error: None })?;
                    credited_count += 1;
                    snap.next_entity_id += 1;
                    snap.wallet_journal.push(WalletJournalEntry {
                        entry_id: snap.next_entity_id, character_id: rid, entry_type: "CorporationDividendPayment".into(),
                        amount: IskCents(cents), balance_after: bal_after, reason: format!("Dividend payout from corporation {corp_id}"),
                    });
                }

                let new_bal_cents = cur_bal.0.checked_sub(actual_cents).ok_or_else(|| BridgeRpcError {
                    code: "InsufficientFunds".into(),
                    message: "Insufficient corporation wallet funds for dividend payout".into(),
                    client_error: None,
                })?;
                let new_bal = IskCents(new_bal_cents);
                if let Some(corp) = snap.corporations.get_mut(&corp_id) {
                    corp.set_division_balance(div_id, new_bal);
                }
                if actual_cents > 0 {
                    snap.next_entity_id += 1;
                    let corp_jid = snap.next_entity_id;
                    snap.wallet_journal.push(WalletJournalEntry {
                        entry_id: corp_jid, character_id: corp_id, entry_type: "CorporationDividendPayout".into(),
                        amount: IskCents(-actual_cents), balance_after: new_bal, reason: format!("Dividend payout from division {div_id}"),
                    });
                }
                Ok(credited_count)
            });

            match res {
                Ok(cnt) => Some(Ok(PyRep::keyval([("corporationID", PyRep::Integer(corp_id)), ("recipientCount", PyRep::Integer(cnt)), ("totalPayoutISK", PyRep::Real(total_isk))]))),
                Err(e) => Some(Err(e)),
            }
        }

        ("corpRegistry", "UpdateTitle") | ("corpRegistry", "UpdateTitles") => {
            if !has_director { return deny_role("Updating corporation titles requires CEO or Director role"); }
            let rows_to_update = parse_title_update_rows(args);
                let (changes_out, sync_updates) = match ctx.db.try_with_mut_snapshot(|snap| {
                if snap.corporations.contains_key(&corp_id) {
                    snap.seed_default_corp_titles(corp_id);
                }
                let corp_ally = snap.corporations.get(&corp_id).and_then(|c| c.alliance_id);
                let mut results = Vec::with_capacity(rows_to_update.len());
                let mut sync_updates = Vec::new();
                for (title_id, title_name, masks) in &rows_to_update {
                    let (title_id, title_name, [roles_mask, grantable_mask, roles_hq_mask, grantable_hq_mask, roles_base_mask, grantable_base_mask, roles_other_mask, grantable_other_mask]) = (*title_id, title_name.as_str(), *masks);
                let state = snap.corp_governance.entry(corp_id).or_default();
                let prev = state.titles.insert(title_id, (title_name.to_string(), roles_mask));
                let prev_rmask = prev.as_ref().map_or(0, |&(_, r)| r);
                let bit = eve_db::snapshot_ext::title_id_to_bit(title_id);
                let newly_delayed = (roles_mask & !prev_rmask) & corp_roles::CORP_DELAYED_DIVISION_ROLE_MASK;
                let dropped = (prev_rmask & !roles_mask) != 0;
                let holders: Vec<i64> = state
                    .member_titles
                    .iter()
                    .filter_map(|(&mid, &tmask)| ((tmask & bit) != 0).then_some(mid))
                    .collect();
                if newly_delayed != 0 || dropped {
                    for &mid in &holders {
                        if dropped { state.role_drop_timestamps.insert(mid, now_unix); }
                        if newly_delayed != 0 {
                            let cur_p = state.pending_title_role_grants.get(&mid).filter(|&&(_, e)| now_unix < e).map_or(0, |&(b, _)| b);
                            state.pending_title_role_grants.insert(mid, (cur_p | newly_delayed, now_unix + 86_400));
                        }
                    }
                }
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
                    roles_at_hq: dec(roles_hq_mask),
                    grantable_roles_at_hq: dec(grantable_hq_mask),
                    roles_at_base: dec(roles_base_mask),
                    grantable_roles_at_base: dec(grantable_base_mask),
                    roles_at_other: dec(roles_other_mask),
                    grantable_roles_at_other: dec(grantable_other_mask),
                });
                titles.sort_by_key(|t| t.title_id);
                if roles_mask != prev_rmask {
                    for mid in holders {
                        let (_, new_eff) =
                            snap.effective_corp_roles_at(corp_id, mid, None, 0, now_unix);
                        sync_updates.push((mid, (corp_id, corp_ally, new_eff)));
                    }
                }
                    results.push((title_id, title_name.to_string(), roles_mask, prev));
                }
                Ok::<_, BridgeRpcError>((results, sync_updates))
            }) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if !sync_updates.is_empty() {
                super::fleet_social_persistence::with_social_state(ctx, |s| {
                    for (mid, entry) in sync_updates {
                        if mid == char_id {
                            session.corp_role = entry.2;
                        }
                        s.forced_session_corp_sync.insert(mid, entry);
                    }
                });
            }
            let mut last_ret = (1_i64, String::new(), 0_u64);
            for (title_id, title_name, roles_mask, prev) in changes_out {
            let (old_name, old_roles) = prev.map_or((PyRep::None, PyRep::Integer(0)), |(n, r)| (PyRep::String(n), PyRep::Integer(r as i64)));
            let change = PyRep::Dict(vec![
                (PyRep::String(("titleName").into()), PyRep::Tuple(vec![old_name, PyRep::String(title_name.to_string())])),
                (PyRep::String(("roles").into()), PyRep::Tuple(vec![old_roles, PyRep::Integer(roles_mask as i64)])),
            ]);
            ctx.emit_typed(
                NotificationTarget::Corporation(corp_id),
                crate::router::notify_registry::OnTitleChanged {
                    corp_id,
                    title_id,
                    change,
                },
            );
                last_ret = (title_id, title_name, roles_mask);
            }
            Some(Ok(PyRep::keyval([
                ("titleID", PyRep::Integer(last_ret.0)),
                ("titleName", PyRep::String(last_ret.1)),
                ("roles", PyRep::Integer(last_ret.2 as i64)),
            ])))
        }

        ("corpRegistry", "GetTitles") => handle_get_titles(ctx, corp_id),

        ("corpRegistry", "DeleteTitle") => {
            if !has_director { return deny_role("Deleting corporation titles requires CEO or Director role"); }
            let title_id = args.first().and_then(|a| a.as_i64()).unwrap_or(1);
            let (prev, sync_updates) = match ctx.db.try_with_mut_snapshot(|snap| {
                if let Some(titles) = snap.corp_titles.get_mut(&corp_id) {
                    titles.retain(|t| t.title_id != title_id);
                }
                let corp_ally = snap.corporations.get(&corp_id).and_then(|c| c.alliance_id);
                let prev = snap.corp_governance.get_mut(&corp_id).and_then(|s| s.titles.remove(&title_id));
                let mut sync_updates = Vec::new();
                if let Some((_, prev_rmask)) = prev {
                    if prev_rmask != 0 {
                        let holders = if let Some(state) = snap.corp_governance.get_mut(&corp_id) {
                            let bit = eve_db::snapshot_ext::title_id_to_bit(title_id);
                            let holders: Vec<i64> = state
                                .member_titles
                                .iter()
                                .filter_map(|(&mid, &tmask)| ((tmask & bit) != 0 || tmask == title_id as u64).then_some(mid))
                                .collect();
                            for &mid in &holders {
                                state.role_drop_timestamps.insert(mid, now_unix);
                            }
                            holders
                        } else {
                            Vec::new()
                        };
                        for mid in holders {
                            let (_, new_eff) =
                                snap.effective_corp_roles_at(corp_id, mid, None, 0, now_unix);
                            sync_updates.push((mid, (corp_id, corp_ally, new_eff)));
                        }
                    }
                }
                Ok::<_, BridgeRpcError>((prev, sync_updates))
            }) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if !sync_updates.is_empty() {
                super::fleet_social_persistence::with_social_state(ctx, |s| {
                    for (mid, entry) in sync_updates {
                        if mid == char_id {
                            session.corp_role = entry.2;
                        }
                        s.forced_session_corp_sync.insert(mid, entry);
                    }
                });
            }
            let (old_name, old_roles) = prev.map_or((PyRep::None, PyRep::Integer(0)), |(n, r)| (PyRep::String(n), PyRep::Integer(r as i64)));
            let change = PyRep::Dict(vec![
                (PyRep::String(("titleName").into()), PyRep::Tuple(vec![old_name, PyRep::String(("").into())])),
                (PyRep::String(("roles").into()), PyRep::Tuple(vec![old_roles, PyRep::Integer(0)])),
            ]);
            ctx.emit_typed(
                NotificationTarget::Corporation(corp_id),
                crate::router::notify_registry::OnTitleChanged {
                    corp_id,
                    title_id,
                    change,
                },
            );
            Some(Ok(PyRep::keyval([("titleID", PyRep::Integer(title_id)), ("deleted", PyRep::Bool(true))])))
        }

        ("corpRegistry", "ResignFromCEO") => {
            let new_ceo_opt = args.first().and_then(|a| a.as_i64());
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(corp) = snap.corporations.get_mut(&corp_id) else {
                    return Err(BridgeRpcError { code: "CorporationNotFound".into(), message: format!("Corporation {corp_id} not found"), client_error: None });
                };
                if corp.ceo_id != char_id {
                    return Err(BridgeRpcError { code: "CorpRoleDenied".into(), message: "Only the current CEO can resign".into(), client_error: None });
                }
                let next_ceo = new_ceo_opt.filter(|&id| id > 0 && id != char_id && corp.members.contains(&id)).unwrap_or(0);
                if next_ceo == 0 && corp.members.len() > 1 {
                    return Err(BridgeRpcError {
                        code: "InvalidNewCEO".into(),
                        message: "Cannot resign as CEO without designating a valid member successor while corporation has other members".into(),
                        client_error: None,
                    });
                }
                let corp_ally = corp.alliance_id;
                corp.ceo_id = next_ceo;
                if next_ceo != char_id {
                    corp.member_roles.insert(char_id, 0);
                }
                if next_ceo > 0 {
                    corp.member_roles.insert(next_ceo, u64::MAX);
                }
                snap.corp_role_assignments.remove(&(corp_id, char_id));
                Ok((next_ceo, corp_ally))
            });
            match res {
                Ok((next_ceo, corp_ally)) => {
                    if next_ceo != char_id {
                        session.corp_role = 0;
                    }
                    super::fleet_social_persistence::with_social_state(ctx, |s| {
                        if next_ceo != char_id {
                            s.forced_session_corp_sync.insert(char_id, (corp_id, corp_ally, 0));
                        }
                        if next_ceo > 0 {
                            s.forced_session_corp_sync.insert(next_ceo, (corp_id, corp_ally, u64::MAX));
                        }
                    });
                    let change = PyRep::Dict(vec![(
                        PyRep::String(("ceoID").into()),
                        PyRep::Tuple(vec![PyRep::Integer(char_id), PyRep::Integer(next_ceo)]),
                    )]);
                    ctx.emit_typed(
                        NotificationTarget::Corporation(corp_id),
                        crate::router::notify_registry::OnCorporationChanged { corp_id, change },
                    );
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
                let vc = snap.corp_vote_cases.get(&case_id).ok_or_else(|| BridgeRpcError { code: "VoteCaseNotFound".into(), message: format!("Vote case {case_id} not found"), client_error: None })?;
                if vc.corporation_id != corp_id
                    || (args.len() >= 3 && args.first().and_then(|a| a.as_i64()).is_some_and(|cid| cid != vc.corporation_id))
                {
                    return Err(BridgeRpcError {
                        code: "CorpAccessDenied".into(),
                        message: "Vote case belongs to a different corporation".into(),
                        client_error: None,
                    });
                }
                if now_unix > vc.end_ts { return Err(BridgeRpcError { code: "VoteCaseClosed".into(), message: "Voting period has closed for this case".into(), client_error: None }); }
                let vc_corp = vc.corporation_id;
                let has_any_corp_shares = snap
                    .corp_shareholders
                    .get(&vc_corp)
                    .is_some_and(|shs| shs.iter().any(|s| s.shareholder_id != vc_corp && s.share_count > 0))
                    || snap
                        .corp_governance
                        .get(&vc_corp)
                        .is_some_and(|g| g.shares.iter().any(|(&oid, &s)| oid != vc_corp && s > 0));
                let sh_weight = snap
                    .corp_shareholders
                    .get(&vc_corp)
                    .and_then(|shs| shs.iter().find(|s| s.shareholder_id == char_id).map(|s| s.share_count))
                    .or_else(|| {
                        snap.corp_governance
                            .get(&vc_corp)
                            .and_then(|g| g.shares.get(&char_id).copied())
                    });
                let is_ceo = snap
                    .corporations
                    .get(&vc_corp)
                    .is_some_and(|c| c.ceo_id == char_id);
                let w = match sh_weight {
                    Some(s) if s > 0 => s,
                    None if !has_any_corp_shares && is_ceo => 1,
                    _ => {
                        return Err(BridgeRpcError {
                            code: "CorpAccessDenied".into(),
                            message: "Only corporation shareholders with > 0 shares may vote".into(),
                            client_error: None,
                        });
                    }
                };
                snap.corp_vote_ballots.insert((case_id, char_id), CorpVoteBallotRecord { case_id, voter_id: char_id, option_id, weight: w, voted_at_unix: now_unix });
                Ok(w)
            }) {
                Ok(w) => w,
                Err(e) => return Some(Err(e)),
            };
            ctx.emit_typed(
                NotificationTarget::Corporation(corp_id),
                crate::router::notify_registry::OnVoteCast {
                    corporation_id: corp_id,
                    vote_case_id: case_id,
                },
            );
            Some(Ok(PyRep::keyval([("voteCaseID", PyRep::Integer(case_id)), ("optionID", PyRep::Integer(option_id)), ("weight", PyRep::Integer(weight))])))
        }

        // parity: rpc:voteManager.GetVotes
        ("voteManager", "GetVotes") => handle_vote_queries(ctx, corp_id, now_unix, method, args),
        // parity: rpc:voteManager.GetVoteCase
        ("voteManager", "GetVoteCase") => handle_vote_queries(ctx, corp_id, now_unix, method, args),
        // parity: rpc:voteManager.GetVoteCasesByCorporation
        ("voteManager", "GetVoteCasesByCorporation") => handle_vote_queries(ctx, corp_id, now_unix, method, args),
        // parity: rpc:voteManager.GetVoteCaseOptions
        ("voteManager", "GetVoteCaseOptions") => handle_vote_queries(ctx, corp_id, now_unix, method, args),
        // parity: rpc:voteManager.GetCorpSanctionedActions
        ("voteManager", "GetCorpSanctionedActions") => handle_vote_queries(ctx, corp_id, now_unix, method, args),

        ("voteManager", "ActivateSanctionedAction") => {
            let case_id = if args.len() >= 2 && args.first().and_then(|a| a.as_i64()) == Some(corp_id) {
                args.get(1).and_then(|a| a.as_i64()).unwrap_or(1)
            } else {
                args.first().and_then(|a| a.as_i64()).unwrap_or(1)
            };
            let is_ceo_election_case = ctx
                .db
                .with_snapshot(|s| s.corp_vote_cases.get(&case_id).is_some_and(|vc| vc.vote_type == 0));
            if !has_director && !is_ceo_election_case {
                return deny_role("Activating sanctioned actions requires CEO or Director role");
            }
            let mut old_ceo_id = char_id;
            let mut new_ceo_id = char_id;
            let mut is_ceo_vote = false;
            let mut share_delta: Option<(i64, i64, i64)> = None;
            let res = ctx.db.try_with_mut_snapshot(|snap| -> Result<(), BridgeRpcError> {
                let (passed, win_param, _) = {
                    let vc = snap.corp_vote_cases.get(&case_id).ok_or_else(|| BridgeRpcError { code: "VoteCaseNotFound".into(), message: format!("Vote case {case_id} not found"), client_error: None })?;
                    if vc.corporation_id != corp_id {
                        return Err(BridgeRpcError {
                            code: "CorpAccessDenied".into(),
                            message: "Vote case belongs to a different corporation".into(),
                            client_error: None,
                        });
                    }
                    if vc.executed { return Err(BridgeRpcError { code: "VoteAlreadyActivated".into(), message: "Sanctioned action has already been activated".into(), client_error: None }); }
                    if now_unix < vc.end_ts { return Err(BridgeRpcError { code: "VoteStillOpen".into(), message: "Cannot activate sanctioned action before voting closes".into(), client_error: None }); }
                    eval_vote_case_winner(vc, &snap.corp_vote_ballots)
                };
                if !passed { return Err(BridgeRpcError { code: "VoteDidNotPass".into(), message: "Sanctioned action did not pass shareholder vote".into(), client_error: None }); }
                let (vote_type, vote_text) = {
                    let vc = snap.corp_vote_cases.get(&case_id).unwrap();
                    (vc.vote_type, vc.text.clone())
                };
                if vote_type == 5 && win_param <= 0 {
                    return Err(BridgeRpcError {
                        code: "InvalidShareCount".into(),
                        message: "Sanctioned share creation requires a positive share count".into(),
                        client_error: None,
                    });
                }
                let mut item_lock_flag_update: Option<eve_db::InventoryFlag> = None;
                if vote_type == 6 || vote_type == 7 || (vote_type != 0 && vote_type != 5 && win_param > 0) {
                    let is_explicit_item_vote = matches!(vote_type, 6 | 7);
                    let is_unlock = vote_type == 7 || vote_text.to_ascii_lowercase().contains("unlock");
                    if let Some(it) = snap.items.get(&win_param) {
                        if it.owner_id != corp_id {
                            if is_explicit_item_vote {
                                return Err(BridgeRpcError {
                                    code: "ItemLocked".into(),
                                    message: format!("Item {win_param} is not owned by corporation {corp_id}"),
                                    client_error: None,
                                });
                            }
                        } else {
                            if snap.is_item_locked_in_contract_or_job(win_param) {
                                return Err(BridgeRpcError {
                                    code: "ItemLocked".into(),
                                    message: format!("Item {win_param} is locked in an active contract or job"),
                                    client_error: None,
                                });
                            }
                            if matches!(it.flag, eve_db::InventoryFlag::Impounded)
                                || matches!(it.flag as u16, 36 | 72)
                                || it.type_id == 60
                            {
                                return Err(BridgeRpcError {
                                    code: if it.flag as u16 == 72 { "ImpoundedItem" } else { "AssetSafetyLocked" }.into(),
                                    message: format!("Item {win_param} cannot be modified by vote while impounded or in asset safety"),
                                    client_error: None,
                                });
                            }
                            if matches!(it.flag as u16, 11..=34 | 92..=94 | 125..=133 | 164..=172 | 180) {
                                return Err(BridgeRpcError {
                                    code: "ItemLocked".into(),
                                    message: format!("Fitted/structural item {win_param} cannot be altered by item lock vote"),
                                    client_error: None,
                                });
                            }
                            if let Some(parent) = snap.items.get(&it.location_id) {
                                crate::services::fitting_containers_ships::check_item_lock_impound_asset_safety_in_snap(snap, parent)?;
                            }
                            // Reject items in active unpaid asset safety wraps.
                            let in_active_wrap = crate::services::fitting_containers_ships::check_item_lock_impound_asset_safety_in_snap(snap, it);
                            if is_unlock {
                                // Only allow unlocking an item that is currently Locked (`63`) and not otherwise blocked
                                // (ignore the direct `Locked` flag on `it` itself while enforcing contract/job/impound/asset-safety/ancestor).
                                if it.flag != eve_db::InventoryFlag::Locked {
                                    return Err(BridgeRpcError {
                                        code: "ItemLocked".into(),
                                        message: format!("Item {win_param} is not blueprint-locked"),
                                        client_error: None,
                                    });
                                }
                                if let Err(err) = in_active_wrap {
                                    if err.code != "ItemLocked" {
                                        return Err(err);
                                    }
                                }
                                let loc_safe = (60_000_000..=69_999_999).contains(&it.location_id)
                                    || snap.structures.contains_key(&it.location_id)
                                    || snap.items.get(&it.location_id).is_some_and(|p| {
                                        p.owner_id == corp_id
                                            && (p.type_id == 27 || (60_000_000..=69_999_999).contains(&p.location_id) || snap.structures.contains_key(&p.location_id))
                                    });
                                if !loc_safe {
                                    return Err(BridgeRpcError {
                                        code: "ItemLocked".into(),
                                        message: format!("Item {win_param} location {} is not a safe corp hangar/office/container", it.location_id),
                                        client_error: None,
                                    });
                                }
                                item_lock_flag_update = Some(eve_db::InventoryFlag::Hangar);
                            } else {
                                in_active_wrap?;
                                if !matches!(it.flag as u16, 4 | 116..=122) {
                                    return Err(BridgeRpcError {
                                        code: "ItemLocked".into(),
                                        message: format!("Item {win_param} must be in a corporation hangar to be locked"),
                                        client_error: None,
                                    });
                                }
                                item_lock_flag_update = Some(eve_db::InventoryFlag::Locked);
                            }
                        }
                    } else if is_explicit_item_vote {
                        return Err(BridgeRpcError {
                            code: "ItemNotFound".into(),
                            message: format!("Item {win_param} not found"),
                            client_error: None,
                        });
                    }
                }
                let vc = snap.corp_vote_cases.get_mut(&case_id).unwrap();
                vc.executed = true;
                vc.parameter = win_param;
                if vc.vote_type == 5 {
                    if vc.parameter <= 0 {
                        return Err(BridgeRpcError { code: "InvalidArgument".into(), message: "Share creation amount must be positive".into(), client_error: None });
                    }
                    let state = snap.corp_governance.entry(corp_id).or_default();
                    let entry = state.shares.entry(corp_id).or_insert(0);
                    let old_s = *entry;
                    let new_s = entry.checked_add(vc.parameter).filter(|&s| s <= 10_000_000_000).ok_or_else(|| {
                        BridgeRpcError { code: "InvalidArgument".into(), message: "Share creation amount exceeds maximum limit".into(), client_error: None }
                    })?;
                    *entry = new_s;
                    state.company_shares = *entry;
                    share_delta = Some((corp_id, old_s, *entry));
                    let shareholders = snap.corp_shareholders.entry(corp_id).or_default();
                    if let Some(sh) = shareholders.iter_mut().find(|s| s.shareholder_id == corp_id) {
                        sh.share_count = sh.share_count.saturating_add(vc.parameter);
                    } else {
                        shareholders.push(eve_db::snapshot_ext::CorpShareholderEntry {
                            shareholder_id: corp_id,
                            shareholder_type: "corporation".into(),
                            share_count: vc.parameter,
                        });
                    }
                }
                if vc.vote_type == 0 {
                    is_ceo_vote = true;
                    new_ceo_id = win_param;
                    let corp = snap.corporations.get_mut(&corp_id).ok_or_else(|| BridgeRpcError { code: "CorpNotFound".into(), message: "Corporation not found".into(), client_error: None })?;
                    if !corp.members.contains(&new_ceo_id) {
                        return Err(BridgeRpcError { code: "CandidateNotMember".into(), message: "CEO election candidate is not a member of the corporation".into(), client_error: None });
                    }
                    old_ceo_id = corp.ceo_id;
                    if old_ceo_id > 0 && old_ceo_id != new_ceo_id {
                        if corp.member_roles.get(&old_ceo_id).copied() == Some(u64::MAX) {
                            corp.member_roles.insert(old_ceo_id, 0);
                        }
                    }
                    corp.ceo_id = new_ceo_id;
                    corp.member_roles.insert(new_ceo_id, u64::MAX);
                    if old_ceo_id > 0 && old_ceo_id != new_ceo_id {
                        snap.corp_role_assignments.remove(&(corp_id, old_ceo_id));
                    }
                } else if let Some(new_flag) = item_lock_flag_update {
                    if let Some(it) = snap.items.get_mut(&win_param) {
                        it.flag = new_flag;
                        it.version += 1;
                    }
                }
                Ok(())
            });
            match res {
                Ok(()) => {
                    if is_ceo_vote {
                        let (corp_ally, old_ceo_roles) = ctx.db.with_snapshot(|s| {
                            let ally = s.corporations.get(&corp_id).and_then(|c| c.alliance_id);
                            let r = if old_ceo_id > 0 && old_ceo_id != new_ceo_id {
                                s.effective_corp_roles_at(corp_id, old_ceo_id, None, 0, now_unix).1
                            } else {
                                0
                            };
                            (ally, r)
                        });
                        super::fleet_social_persistence::with_social_state(ctx, |s| {
                            if old_ceo_id > 0 && old_ceo_id != new_ceo_id {
                                s.forced_session_corp_sync
                                    .insert(old_ceo_id, (corp_id, corp_ally, old_ceo_roles));
                            }
                            s.forced_session_corp_sync
                                .insert(new_ceo_id, (corp_id, corp_ally, u64::MAX));
                        });
                        if session.character_id == Some(old_ceo_id) && old_ceo_id != new_ceo_id {
                            session.corp_role = old_ceo_roles;
                        } else if session.character_id == Some(new_ceo_id) {
                            session.corp_role = u64::MAX;
                        }
                    }
                    ctx.emit_typed(
                        NotificationTarget::Corporation(corp_id),
                        crate::router::notify_registry::OnSanctionedActionChanged {
                            corporation_id: corp_id,
                            vote_case_id: case_id,
                        },
                    );
                    if let Some((holder_id, old_s, new_s)) = share_delta {
                        let s_change = PyRep::Dict(vec![(PyRep::String(("shares").into()), PyRep::Tuple(vec![PyRep::Integer(old_s), PyRep::Integer(new_s)]))]);
                        ctx.emit_typed(
                            NotificationTarget::Corporation(corp_id),
                            crate::router::notify_registry::OnShareChange {
                                shareholder_id: holder_id,
                                corporation_id: corp_id,
                                change: s_change,
                            },
                        );
                    }
                    if is_ceo_vote {
                        let change = PyRep::Dict(vec![(PyRep::String(("ceoID").into()), PyRep::Tuple(vec![PyRep::Integer(old_ceo_id), PyRep::Integer(new_ceo_id)]))]);
                        ctx.emit_typed(
                            NotificationTarget::Corporation(corp_id),
                            crate::router::notify_registry::OnCorporationChanged { corp_id, change },
                        );
                    }
                    Some(Ok(PyRep::keyval([("voteCaseID", PyRep::Integer(case_id)), ("activated", PyRep::Bool(true))])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        // parity: rpc:corporationSvc.CreateMedal
        ("corporationSvc", "CreateMedal") => handle_corp_medals(ctx, corp_id, char_id, now_unix, has_comms, method, args),
        // parity: rpc:corporationSvc.GiveMedalToCharacters
        ("corporationSvc", "GiveMedalToCharacters") => handle_corp_medals(ctx, corp_id, char_id, now_unix, has_comms, method, args),
        // parity: feat:character.medals-display, rpc:corporationSvc.SetMedalStatus
        ("corporationSvc", "SetMedalStatus") => handle_corp_medals(ctx, corp_id, char_id, now_unix, has_comms, method, args),
        // parity: rpc:corporationSvc.GetAllCorpMedals
        ("corporationSvc", "GetAllCorpMedals") => handle_corp_medals(ctx, corp_id, char_id, now_unix, has_comms, method, args),
        // parity: rpc:corporationSvc.GetMedalDetails
        ("corporationSvc", "GetMedalDetails") => handle_corp_medals(ctx, corp_id, char_id, now_unix, has_comms, method, args),
        // parity: rpc:corporationSvc.GetMedalsReceived
        ("corporationSvc", "GetMedalsReceived") => handle_corp_medals(ctx, corp_id, char_id, now_unix, has_comms, method, args),
        // parity: rpc:corporationSvc.GetRecipientsOfMedal
        ("corporationSvc", "GetRecipientsOfMedal") => handle_corp_medals(ctx, corp_id, char_id, now_unix, has_comms, method, args),

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

        // parity: rpc:corpRegistry.AddBulletin
        ("corpRegistry", "AddBulletin") => handle_corp_bulletins(ctx, session, corp_id, char_id, now_unix, has_comms, method, args),
        // parity: rpc:corpRegistry.UpdateBulletinOrder
        ("corpRegistry", "UpdateBulletinOrder") => handle_corp_bulletins(ctx, session, corp_id, char_id, now_unix, has_comms, method, args),
        // parity: rpc:corpRegistry.GetBulletins
        ("corpRegistry", "GetBulletins") => handle_corp_bulletins(ctx, session, corp_id, char_id, now_unix, has_comms, method, args),
        // parity: rpc:corpRegistry.DeleteBulletin
        ("corpRegistry", "DeleteBulletin") => handle_corp_bulletins(ctx, session, corp_id, char_id, now_unix, has_comms, method, args),

        ("corpRecProxy", "CreateRecruitmentAd") | ("corpRecProxy", "UpdateRecruitmentAd") => {
            let has_role = ctx.db.with_snapshot(|snap| {
                if !snap.corporations.contains_key(&corp_id) {
                    return false;
                }
                let (is_ceo, r) = snap.effective_corp_roles_at(corp_id, char_id, None, 0, now_unix);
                is_ceo || (r & (corp_roles::ROLE_PERSONNEL_MANAGER | corp_roles::ROLE_DIRECTOR)) != 0
            });
            if !has_role {
                return deny_role("Managing recruitment ads requires Personnel Manager or Director role");
            }
            if method == "CreateRecruitmentAd" {
                let ad_fee = IskCents::from_isk(250_000);
                let _ = ctx.db.try_with_mut_snapshot(|snap| -> Result<(), BridgeRpcError> {
                    if let Some(corp) = snap.corporations.get_mut(&corp_id) {
                        if corp.debit_division(1, ad_fee).is_none() {
                            if let Some(ch) = snap.characters.get_mut(&char_id) {
                                if let Some(nb) = ch.balance.checked_sub(ad_fee) {
                                    ch.balance = nb;
                                }
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
                let Some(ad) = ads.iter_mut().find(|a| a.ad_id == id && a.corp_id == corp_id) else {
                    return Some(Err(BridgeRpcError { code: "RecruitmentAdNotFound".into(), message: format!("Recruitment ad {id} not found"), client_error: None }));
                };
                ad.title = title.clone();
                ad.description = desc.clone();
                ad.type_mask = type_mask;
                if lang_mask != 0 { ad.lang_mask = lang_mask; }
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
            ctx.emit_typed(
                NotificationTarget::Corporation(corp_id),
                crate::router::notify_registry::OnCorporationRecruitmentAdChanged {
                    corp_id: Some(corp_id),
                    ad_id: Some(ad_id),
                },
            );
            Some(Ok(PyRep::keyval([
                ("adID", PyRep::Integer(ad_id)),
                ("corporationID", PyRep::Integer(corp_id)),
                ("title", PyRep::String(title)),
                ("typeMask", PyRep::Integer(type_mask as i64)),
            ])))
        }

        ("corpRecProxy", "GetRecruiters") => handle_get_recruiters(ctx, corp_id, args),

        // parity: rpc:corpRecProxy.GetRecruitmentAdsForCorporation
        ("corpRecProxy", "GetRecruitmentAdsForCorporation") => handle_corp_rec_queries(ctx, corp_id, now_unix, method, args, kwargs),
        // parity: rpc:corpRecProxy.GetRecruitmentAdsByCorpID
        ("corpRecProxy", "GetRecruitmentAdsByCorpID") => handle_corp_rec_queries(ctx, corp_id, now_unix, method, args, kwargs),
        // parity: rpc:corpRecProxy.GetRecruitmentAd
        ("corpRecProxy", "GetRecruitmentAd") => handle_corp_rec_queries(ctx, corp_id, now_unix, method, args, kwargs),
        // parity: rpc:corpRecProxy.SearchCorpAds
        ("corpRecProxy", "SearchCorpAds") => handle_corp_rec_queries(ctx, corp_id, now_unix, method, args, kwargs),

        ("corpRecProxy", "DeleteRecruitmentAd") => {
            let has_role = ctx.db.with_snapshot(|snap| {
                if !snap.corporations.contains_key(&corp_id) {
                    return false;
                }
                let (is_ceo, r) = snap.effective_corp_roles_at(corp_id, char_id, None, 0, now_unix);
                is_ceo || (r & (corp_roles::ROLE_PERSONNEL_MANAGER | corp_roles::ROLE_DIRECTOR)) != 0
            });
            if !has_role {
                return deny_role("Deleting recruitment ads requires Personnel Manager or Director role");
            }
            let ad_id = args.first().and_then(|a| a.as_i64()).unwrap_or(1);
            let mut ads = load_corp_rec_ads(ctx);
            let before = ads.len();
            ads.retain(|a| a.ad_id != ad_id || a.corp_id != corp_id);
            if ads.len() == before {
                return Some(Err(BridgeRpcError { code: "RecruitmentAdNotFound".into(), message: format!("Recruitment ad {ad_id} not found"), client_error: None }));
            }
            save_corp_rec_ads(ctx, &ads);
            with_corp_gov(ctx, |g| {
                if let Some(s) = g.get_mut(&corp_id) {
                    s.recruitment_ads.retain(|a| a.0 != ad_id);
                }
            });
            ctx.emit_typed(
                NotificationTarget::Corporation(corp_id),
                crate::router::notify_registry::OnCorporationRecruitmentAdChanged {
                    corp_id: Some(corp_id),
                    ad_id: Some(ad_id),
                },
            );
            Some(Ok(PyRep::keyval([("adID", PyRep::Integer(ad_id)), ("deleted", PyRep::Bool(true))])))
        }

        // parity: rpc:corpRegistry.AddCorporateContact
        ("corpRegistry", "AddCorporateContact") => handle_corp_contacts_and_labels(ctx, corp_id, has_diplomat, method, args),
        // parity: rpc:corpRegistry.EditCorporateContact
        ("corpRegistry", "EditCorporateContact") => handle_corp_contacts_and_labels(ctx, corp_id, has_diplomat, method, args),
        // parity: rpc:corpRegistry.EditContactsRelationshipID
        ("corpRegistry", "EditContactsRelationshipID") => handle_corp_contacts_and_labels(ctx, corp_id, has_diplomat, method, args),
        // parity: rpc:corpRegistry.DeleteLabel
        ("corpRegistry", "DeleteLabel") => handle_corp_contacts_and_labels(ctx, corp_id, has_diplomat, method, args),
        // parity: rpc:corpRegistry.AssignLabels
        ("corpRegistry", "AssignLabels") => handle_corp_contacts_and_labels(ctx, corp_id, has_diplomat, method, args),
        // parity: rpc:corpRegistry.RemoveLabels
        ("corpRegistry", "RemoveLabels") => handle_corp_contacts_and_labels(ctx, corp_id, has_diplomat, method, args),

        ("corpRegistry", "RemoveCorporateContacts") => {
            if !has_diplomat {
                return deny_role("Removing corporate contacts requires Diplomat or Director role");
            }
            let cids: Vec<i64> = match args.first() {
                Some(PyRep::Integer(id)) => vec![*id],
                Some(PyRep::List(items)) | Some(PyRep::Tuple(items)) => {
                    items.iter().filter_map(|v| v.as_i64()).collect()
                }
                _ => Vec::new(),
            };
            let first_cid = cids.first().copied().unwrap_or(0);
            ctx.db.with_mut_snapshot(|snap| {
                if let Some(list) = snap.corp_contacts.get_mut(&corp_id) {
                    list.retain(|c| !cids.contains(&c.contact_id));
                }
            });
            Some(Ok(PyRep::keyval([
                ("contactID", PyRep::Integer(first_cid)),
                ("removed", PyRep::Bool(true)),
            ])))
        }

        ("corpRegistry", "GetCorporateContacts") => {
            let mut items: Vec<(PyRep, Vec<PyRep>)> = ctx.db.with_snapshot(|snap| {
                snap.corp_contacts
                    .get(&corp_id)
                    .map(|list| {
                        list.iter()
                            .map(|c| {
                                (
                                    PyRep::Integer(c.contact_id),
                                    vec![
                                        PyRep::Integer(c.contact_id),
                                        PyRep::Real(c.standing),
                                        PyRep::Integer(label_mask_from_ids(&c.label_ids)),
                                    ],
                                )
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            });
            items.sort_by_key(|(k, _)| k.as_i64());
            Some(Ok(PyRep::index_rowset(
                "contactID",
                vec!["contactID", "relationshipID", "labelMask"],
                items,
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
                    let n = args.get(1).and_then(|a| a.as_str()).unwrap_or(&existing.0).to_string();
                    let c = args.get(2).and_then(|a| a.as_i64()).map(|v| v as u32).unwrap_or(existing.1);
                    (id, n, c)
                } else {
                    let n = args.first().and_then(|a| a.as_str()).unwrap_or("Allies").to_string();
                    let c = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0x00FF00) as u32;
                    let next_bit = (0..30).map(|b| 1i64 << b).find(|id| !labels.iter().any(|(lid, _, _)| lid == id)).unwrap_or(1);
                    (next_bit, n, c)
                };
                labels.retain(|(id, _, _)| *id != lid);
                labels.push((lid, name.clone(), color));
                labels.sort_by_key(|(id, _, _)| *id);
                (lid, name, color)
            });
            Some(Ok(PyRep::keyval([("labelID", PyRep::Integer(lid)), ("name", PyRep::String(name)), ("color", PyRep::Integer(color as i64))])))
        }

        ("corpRegistry", "GetLabels") => {
            let mut rows: Vec<Vec<PyRep>> = ctx.db.with_snapshot(|snap| {
                snap.corp_contact_labels.get(&corp_id).map(|labels| {
                    labels.iter().map(|(lid, name, color)| vec![PyRep::Integer(*lid), PyRep::String(name.clone()), PyRep::Integer(*color as i64)]).collect()
                }).unwrap_or_default()
            });
            rows.sort_by_key(|r| r[0].as_i64());
            Some(Ok(PyRep::rowset(vec!["labelID", "name", "color"], rows)))
        }

        _ => super::fleet_social_corp_tail_dispatch::handle_corp_gov_tail(ctx, session, service, method, args, corp_id, char_id, now_unix),
    }
}

fn handle_get_shares_by_shareholder(
    ctx: &ServerContext,
    is_gm: bool,
    char_id: i64,
    corp_id: i64,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (target_sh_id, corp_shares) = match args {
        [PyRep::Bool(true) | PyRep::Integer(1)] => (corp_id, true),
        [PyRep::Bool(false) | PyRep::Integer(0)] | [] | [PyRep::None] => (char_id, false),
        [PyRep::Integer(n)] => (*n, *n == corp_id),
        [first, second, ..] => {
            let req_corp = matches!(first, PyRep::Bool(true) | PyRep::Integer(1));
            let explicit = second.as_i64().unwrap_or(if req_corp { corp_id } else { char_id });
            (explicit, req_corp || explicit == corp_id)
        }
        _ => (char_id, false),
    };
    if !is_gm {
        if corp_shares {
            if target_sh_id != corp_id || corp_id <= 0 {
                return Some(Err(BridgeRpcError {
                    code: "CrpAccessDenied".into(),
                    message: format!("Cannot view shares for entity {target_sh_id}"),
                    client_error: None,
                }));
            }
            let now_unix = ctx.now_unix();
            let can_view_corp_shares = ctx.db.with_snapshot(|snap| {
                if !snap.corporations.contains_key(&corp_id) {
                    return false;
                }
                let (is_ceo, eff_roles) =
                    crate::services::social_corp_fleet::social_corp_projects::effective_corp_member_roles(
                        snap, corp_id, char_id, 0, now_unix,
                    );
                is_ceo || (eff_roles & corp_roles::ROLE_DIRECTOR) != 0
            });
            if !can_view_corp_shares {
                return Some(Err(BridgeRpcError {
                    code: "CrpAccessDenied".into(),
                    message: "Only CEO or Directors can view corporation-held shares".into(),
                    client_error: None,
                }));
            }
        } else if target_sh_id != char_id {
            return Some(Err(BridgeRpcError {
                code: "CrpAccessDenied".into(),
                message: format!("Cannot view shares held by third-party entity {target_sh_id}"),
                client_error: None,
            }));
        }
    }
    let rows: Vec<Vec<PyRep>> = with_corp_gov(ctx, |g| {
        let mut out = Vec::new();
        for (&cid, cgov) in g.iter() {
            if let Some(&cnt) = cgov.shares.get(&target_sh_id) {
                if cnt > 0 {
                    out.push(vec![
                        PyRep::Integer(target_sh_id),
                        PyRep::Integer(cid),
                        PyRep::Integer(cnt),
                    ]);
                }
            }
        }
        out
    });
    Some(Ok(PyRep::rowset(
        vec!["shareholderID", "corporationID", "shares"],
        rows,
    )))
}

fn encode_corp_roles(names: &[String]) -> u64 {
    let mut mask = 0u64;
    for name in names {
        for &(bit, client_name, esi_name) in crate::esi::corp_governance_routes::CORP_ROLE_DEFINITIONS {
            if name == esi_name || name == client_name {
                mask |= bit;
            }
        }
    }
    mask
}

const TITLE_ROLE_COLS: [&str; 8] = [
    "roles",
    "grantableRoles",
    "rolesAtHQ",
    "grantableRolesAtHQ",
    "rolesAtBase",
    "grantableRolesAtBase",
    "rolesAtOther",
    "grantableRolesAtOther",
];

pub(super) fn parse_title_row_cells(cols: Option<&[String]>, cells: &[PyRep]) -> Option<(i64, String, [u64; 8])> {
    let col_idx = |name: &str, default_idx: usize| -> usize {
        cols.and_then(|c| c.iter().position(|n| n == name)).unwrap_or(default_idx)
    };
    let tid = cells.get(col_idx("titleID", 0)).and_then(PyRep::as_i64)?;
    if tid <= 0 {
        return None;
    }
    let tname = cells
        .get(col_idx("titleName", 1))
        .and_then(PyRep::as_str)
        .unwrap_or("")
        .to_string();
    let mut masks = [0u64; 8];
    for (i, col_name) in TITLE_ROLE_COLS.iter().enumerate() {
        let idx = col_idx(col_name, i + 2);
        masks[i] = (cells.get(idx).and_then(PyRep::as_i64).unwrap_or(0) as u64)
            & !corp_roles::ROLE_DIRECTOR;
    }
    Some((tid, tname, masks))
}

pub(super) fn parse_title_dict_entry(entry: &PyRep) -> Option<(i64, String, [u64; 8])> {
    let tid = entry.dict_get("titleID").and_then(PyRep::as_i64)?;
    if tid <= 0 {
        return None;
    }
    let tname = entry
        .dict_get("titleName")
        .and_then(PyRep::as_str)
        .unwrap_or("")
        .to_string();
    let mut masks = [0u64; 8];
    for (i, col_name) in TITLE_ROLE_COLS.iter().enumerate() {
        masks[i] = (entry.dict_get(col_name).and_then(PyRep::as_i64).unwrap_or(0) as u64)
            & !corp_roles::ROLE_DIRECTOR;
    }
    Some((tid, tname, masks))
}

pub(super) fn parse_title_update_rows(args: &[PyRep]) -> Vec<(i64, String, [u64; 8])> {
    if let Some(first) = args.first() {
        match first {
            PyRep::Rowset { columns, rows } => {
                let out: Vec<_> = rows
                    .iter()
                    .filter_map(|r| parse_title_row_cells(Some(columns), r))
                    .collect();
                if !out.is_empty() {
                    return out;
                }
            }
            PyRep::Object { args: obj_args, .. } => {
                if let (
                    Some(PyRep::List(hdr) | PyRep::Tuple(hdr)),
                    Some(PyRep::List(lines) | PyRep::Tuple(lines)),
                ) = (obj_args.dict_get("header"), obj_args.dict_get("lines"))
                {
                    let cols: Vec<String> = hdr
                        .iter()
                        .filter_map(|c| c.as_str().map(String::from))
                        .collect();
                    let out: Vec<_> = lines
                        .iter()
                        .filter_map(|line| match line {
                            PyRep::List(cells) | PyRep::Tuple(cells) => {
                                parse_title_row_cells(Some(&cols), cells)
                            }
                            _ => parse_title_dict_entry(line),
                        })
                        .collect();
                    if !out.is_empty() {
                        return out;
                    }
                }
                if let Some(single) = parse_title_dict_entry(first) {
                    return vec![single];
                }
            }
            PyRep::List(entries) | PyRep::Tuple(entries) => {
                let out: Vec<_> = entries
                    .iter()
                    .filter_map(|entry| match entry {
                        PyRep::List(cells) | PyRep::Tuple(cells) => {
                            parse_title_row_cells(None, cells)
                        }
                        _ => parse_title_dict_entry(entry),
                    })
                    .collect();
                if !out.is_empty() {
                    return out;
                }
            }
            _ => {
                if let Some(single) = parse_title_dict_entry(first) {
                    return vec![single];
                }
            }
        }
    }
    let title_id = args.first().and_then(|a| a.as_i64()).unwrap_or(1).max(1);
    let title_name = args
        .get(1)
        .and_then(|a| a.as_str())
        .unwrap_or("Squad Leader")
        .to_string();
    let mut masks = [0u64; 8];
    for i in 0..8 {
        masks[i] = (args.get(i + 2).and_then(|a| a.as_i64()).unwrap_or(0) as u64)
            & !corp_roles::ROLE_DIRECTOR;
    }
    vec![(title_id, title_name, masks)]
}

fn handle_get_titles(
    ctx: &ServerContext,
    corp_id: i64,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let mut by_id: std::collections::BTreeMap<i64, (String, [u64; 8])> =
        std::collections::BTreeMap::new();
    ctx.db.with_snapshot(|snap| {
        if snap.corporations.contains_key(&corp_id) {
            for i in 0..16 {
                by_id.insert(1i64 << i, (String::new(), [0; 8]));
            }
        }
        if let Some(entries) = snap.corp_titles.get(&corp_id) {
            for t in entries {
                by_id.insert(
                    t.title_id,
                    (
                        t.name.clone(),
                        [
                            encode_corp_roles(&t.roles),
                            encode_corp_roles(&t.grantable_roles),
                            encode_corp_roles(&t.roles_at_hq),
                            encode_corp_roles(&t.grantable_roles_at_hq),
                            encode_corp_roles(&t.roles_at_base),
                            encode_corp_roles(&t.grantable_roles_at_base),
                            encode_corp_roles(&t.roles_at_other),
                            encode_corp_roles(&t.grantable_roles_at_other),
                        ],
                    ),
                );
            }
        }
        if let Some(gov) = snap.corp_governance.get(&corp_id) {
            for (&tid, (tname, rmask)) in &gov.titles {
                by_id
                    .entry(tid)
                    .and_modify(|(n, masks)| {
                        *n = tname.clone();
                        masks[0] |= *rmask;
                    })
                    .or_insert_with(|| (tname.clone(), [*rmask, 0, 0, 0, 0, 0, 0, 0]));
            }
        }
    });
    let rows: Vec<Vec<PyRep>> = by_id
        .into_iter()
        .map(|(tid, (tname, masks))| {
            vec![
                PyRep::Integer(tid),
                PyRep::String(tname),
                PyRep::Integer(masks[0] as i64),
                PyRep::Integer(masks[1] as i64),
                PyRep::Integer(masks[2] as i64),
                PyRep::Integer(masks[3] as i64),
                PyRep::Integer(masks[4] as i64),
                PyRep::Integer(masks[5] as i64),
                PyRep::Integer(masks[6] as i64),
                PyRep::Integer(masks[7] as i64),
            ]
        })
        .collect();
    Some(Ok(PyRep::rowset(
        vec![
            "titleID",
            "titleName",
            "roles",
            "grantableRoles",
            "rolesAtHQ",
            "grantableRolesAtHQ",
            "rolesAtBase",
            "grantableRolesAtBase",
            "rolesAtOther",
            "grantableRolesAtOther",
        ],
        rows,
    )))
}

fn handle_get_recruiters(
    ctx: &ServerContext,
    corp_id: i64,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    let target_corp = args.first().and_then(|a| a.as_i64()).unwrap_or(corp_id);
    let target_ad = args.get(1).and_then(|a| a.as_i64()).unwrap_or(1);
    let ads = load_corp_rec_ads(ctx);
    let recruiters: Vec<PyRep> = ads
        .iter()
        .filter(|a| a.corp_id == target_corp && a.ad_id == target_ad)
        .map(|a| PyRep::Integer(a.recruiter_id))
        .collect();
    Some(Ok(PyRep::List(recruiters)))
}

#[cfg(test)]
#[path = "fleet_social_corp_gov_tests.rs"]
pub(crate) mod fleet_social_corp_gov_tests;

#[cfg(test)]
#[path = "fleet_social_corp_gov_edges_tests.rs"]
mod fleet_social_corp_gov_edges_tests;
