//! Extracted corporation/alliance governance delegation helpers (`corp_gov_delegation`).
//!
//! Pure structural extraction preserving existing corporation, session, and alliance authorization checks.

#![forbid(unsafe_code)]

use super::fleet_social::fleet_social_corps::{read_corp_gov, with_corp_gov};
use crate::router::notify_registry::{OnCapitalSystemChanged, OnCorpWalletBillsChanged, OnUpdateCapitalInfo};
use crate::router::{cache_notifications::NotificationTarget, RpcArgs, ServerContext};
use eve_db::{corp_roles, CorpRecruitmentAd};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, ClientError, PyRep, UserErrorKey};
use std::collections::{HashMap, HashSet};

pub(crate) fn load_corp_rec_ads(ctx: &ServerContext) -> Vec<CorpRecruitmentAd> {
    let mut ads: Vec<CorpRecruitmentAd> = ctx
        .db
        .with_snapshot(|snap| snap.fleet_and_social_ops.corp_recruitment_ads.clone());
    if ads.is_empty() {
        read_corp_gov(ctx, |g| {
            for (&cid, state) in g.iter() {
                for (aid, title, desc, mask, rec_id) in &state.recruitment_ads {
                    ads.push(CorpRecruitmentAd {
                        ad_id: *aid,
                        corp_id: cid,
                        title: title.clone(),
                        description: desc.clone(),
                        type_mask: *mask,
                        recruiter_id: *rec_id,
                        recruiters: (*rec_id > 0).then_some(*rec_id).into_iter().collect(),
                        created_unix: 0,
                        expiry_unix: u64::MAX,
                        days: 30,
                        lang_mask: 0,
                    });
                }
            }
        });
    }
    ads
}

pub(crate) fn handle_corp_rec_queries(
    ctx: &ServerContext,
    corp_id: i64,
    now_unix: u64,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let rpc = RpcArgs::new(args, kwargs);
    let (filter_corp, filter_ad, filter_mask, filter_lang, min_members, max_members) =
        match (|| -> Result<_, BridgeRpcError> {
            let filter_corp = match method {
                "GetRecruitmentAdsByCorpID" => rpc.opt_i64(0, "corporationID")?.or(Some(corp_id)),
                "GetRecruitmentAdsForCorporation" => Some(corp_id),
                _ => None,
            };
            let filter_ad = if method == "GetRecruitmentAd" {
                rpc.opt_i64(0, "adID")?
            } else {
                None
            };
            let (mask, lang, min_m, max_m) = if method == "SearchCorpAds" {
                (
                    rpc.opt_u64(0, "typeMask")?.unwrap_or(0),
                    match rpc.get_raw(1, "langMask") {
                        Some(_) => rpc.opt_u64(1, "langMask")?,
                        None => rpc.opt_u64(1, "languageMask")?,
                    }
                    .unwrap_or(0),
                    rpc.opt_i64(2, "minMembers")?,
                    rpc.opt_i64(3, "maxMembers")?,
                )
            } else {
                (0, 0, None, None)
            };
            Ok((filter_corp, filter_ad, mask, lang, min_m, max_m))
        })() {
            Ok(v) => v,
            Err(e) => return Some(Err(e)),
        };
    let ads = load_corp_rec_ads(ctx);
    let mut rows = Vec::new();
    for ad in &ads {
        if method == "SearchCorpAds" {
            if now_unix >= ad.expiry_unix
                || (filter_mask != 0 && (ad.type_mask & filter_mask) == 0)
                || (filter_lang != 0 && (ad.lang_mask & filter_lang) == 0)
            {
                continue;
            }
            let member_count = ctx.db.with_snapshot(|s| {
                s.corporations
                    .get(&ad.corp_id)
                    .map(|c| c.members.len())
                    .unwrap_or(1) as i64
            });
            if min_members.is_some_and(|min_m| member_count < min_m)
                || max_members.is_some_and(|max_m| max_m > 0 && member_count > max_m)
            {
                continue;
            }
        } else if filter_corp.is_some_and(|fc| fc != ad.corp_id)
            || filter_ad.is_some_and(|fa| fa != ad.ad_id)
        {
            continue;
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

pub(crate) fn handle_get_wallet_divisions_info(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (_, char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let is_gm = ServerContext::has_canonical_gm_role(session);
    let explicit_corp_id = match RpcArgs::positional(args).opt_i64(0, "corporationID") {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let now_unix = ctx.now_unix();
    let res = ctx.db.with_snapshot(|snap| {
        let caller_actor = eve_db::CorpActor::resolve(snap, &ctx.sde, char_id, now_unix);
        let corp_id = explicit_corp_id.unwrap_or(caller_actor.corp_id);
        if eve_db::is_npc_corporation(&ctx.sde, corp_id) {
            let msg = "NPC corporations do not expose player wallet divisions";
            return Err(BridgeRpcError::with_client_error(
                "CorpRoleDenied",
                msg,
                ClientError::CustomInfo(msg.into()),
            ));
        }
        let Some(corp) = snap.corporations.get(&corp_id) else {
            let msg = format!("Corporation {corp_id} not found");
            return Err(BridgeRpcError::with_client_error(
                "CorporationNotFound",
                msg.clone(),
                ClientError::CustomInfo(msg),
            ));
        };
        if !is_gm {
            let target_actor =
                eve_db::CorpActor::resolve_for_corp(snap, &ctx.sde, corp_id, char_id, None, now_unix);
            if corp_id != caller_actor.corp_id || target_actor.require_member().is_err() {
                let msg =
                    "Viewing corporation wallet divisions requires membership in the target corporation";
                return Err(BridgeRpcError::with_client_error(
                    "CorpRoleDenied",
                    msg,
                    ClientError::CustomInfo(msg.into()),
                ));
            }
            if !target_actor.is_ceo
                && !corp_roles::has_role(target_actor.effective_roles, corp_roles::ROLE_ACCOUNTANT)
                && !corp_roles::has_role(
                    target_actor.effective_roles,
                    corp_roles::ROLE_JUNIOR_ACCOUNTANT,
                )
            {
                let msg = "Viewing corporation wallet divisions requires Accountant role";
                return Err(BridgeRpcError::with_client_error(
                    "CorpRoleDenied",
                    msg,
                    ClientError::CustomInfo(msg.into()),
                ));
            }
        }
        let rows: Vec<Vec<PyRep>> = (1..=7u8)
            .map(|d| {
                let name = corp
                    .wallet_divisions
                    .iter()
                    .find(|w| w.division_id == d)
                    .map(|w| w.name.clone())
                    .unwrap_or_else(|| {
                        if d == 1 {
                            "Master Wallet".into()
                        } else {
                            format!("Division {d}")
                        }
                    });
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

pub(crate) fn handle_get_shares_by_shareholder(
    ctx: &ServerContext,
    is_gm: bool,
    char_id: i64,
    corp_id: i64,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    let rpc = RpcArgs::positional(args);
    let (target_sh_id, corp_shares) = if rpc.len() >= 2 {
        let req_corp = matches!(
            rpc.get_raw(0, "corpShares"),
            Some(PyRep::Bool(true) | PyRep::Integer(1))
        );
        let explicit = match rpc.opt_i64(1, "shareholderID") {
            Ok(v) => v.unwrap_or(if req_corp { corp_id } else { char_id }),
            Err(e) => return Some(Err(e)),
        };
        (explicit, req_corp || explicit == corp_id)
    } else {
        match rpc.get_raw(0, "shareholderID") {
            None | Some(PyRep::Bool(false) | PyRep::Integer(0)) => (char_id, false),
            Some(PyRep::Bool(true) | PyRep::Integer(1)) => (corp_id, true),
            Some(_) => match rpc.req_i64(0, "shareholderID") {
                Ok(n) => (n, n == corp_id),
                Err(e) => return Some(Err(e)),
            },
        }
    };
    if !is_gm {
        if corp_shares {
            if target_sh_id != corp_id || corp_id <= 0 {
                return Some(Err(BridgeRpcError::with_client_error(
                    "CrpAccessDenied",
                    format!("Cannot view shares for entity {target_sh_id}"),
                    ClientError::user(UserErrorKey::CrpAccessDenied),
                )));
            }
            let now_unix = ctx.now_unix();
            let can_view_corp_shares = ctx.db.with_snapshot(|snap| {
                eve_db::CorpActor::resolve_for_corp(
                    snap, &ctx.sde, corp_id, char_id, None, now_unix,
                )
                .is_director()
            });
            if !can_view_corp_shares {
                return Some(Err(BridgeRpcError::with_client_error(
                    "CrpAccessDenied",
                    "Only CEO or Directors can view corporation-held shares",
                    ClientError::user(UserErrorKey::CrpAccessDenied),
                )));
            }
        } else if target_sh_id != char_id {
            return Some(Err(BridgeRpcError::with_client_error(
                "CrpAccessDenied",
                format!("Cannot view shares held by third-party entity {target_sh_id}"),
                ClientError::user(UserErrorKey::CrpAccessDenied),
            )));
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

pub(crate) fn encode_corp_roles(names: &[String]) -> u64 {
    let mut mask = 0u64;
    for name in names {
        for &(bit, client_name, esi_name) in crate::esi::corp_governance_routes::CORP_ROLE_DEFINITIONS
        {
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

pub(crate) fn parse_title_row_cells(
    cols: Option<&[String]>,
    cells: &[PyRep],
) -> Option<(i64, String, [u64; 8])> {
    let col_idx = |name: &str, default_idx: usize| -> usize {
        cols.and_then(|c| c.iter().position(|n| n == name))
            .unwrap_or(default_idx)
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

pub(crate) fn parse_title_dict_entry(entry: &PyRep) -> Option<(i64, String, [u64; 8])> {
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

pub(crate) fn parse_title_update_rows(args: &[PyRep]) -> Vec<(i64, String, [u64; 8])> {
    let rpc = RpcArgs::positional(args);
    if let Some(first) = rpc.get_raw(0, "") {
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
    let title_id = rpc.opt_i64(0, "titleID").ok().flatten().unwrap_or(1).max(1);
    let title_name = rpc
        .opt_str(1, "titleName")
        .ok()
        .flatten()
        .unwrap_or("Squad Leader")
        .to_string();
    let mut masks = [0u64; 8];
    for (i, col_name) in TITLE_ROLE_COLS.iter().enumerate() {
        masks[i] = (rpc.opt_i64(i + 2, col_name).ok().flatten().unwrap_or(0) as u64)
            & !corp_roles::ROLE_DIRECTOR;
    }
    vec![(title_id, title_name, masks)]
}

pub(crate) fn handle_get_titles(
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

pub(crate) fn handle_get_recruiters(
    ctx: &ServerContext,
    corp_id: i64,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    let ads = load_corp_rec_ads(ctx);
    let rpc = RpcArgs::positional(args);
    let (target_corp, target_ad) = if rpc.len() >= 2 {
        let tc = match rpc.opt_i64(0, "corpID") {
            Ok(v) => v.unwrap_or(corp_id),
            Err(e) => return Some(Err(e)),
        };
        let ta = match rpc.opt_i64(1, "adID") {
            Ok(v) => v.unwrap_or(1),
            Err(e) => return Some(Err(e)),
        };
        (tc, ta)
    } else {
        let ta = match rpc.opt_i64(0, "adID") {
            Ok(v) => v.unwrap_or(1),
            Err(e) => return Some(Err(e)),
        };
        let tc = ads
            .iter()
            .find(|a| a.ad_id == ta)
            .map(|a| a.corp_id)
            .unwrap_or(corp_id);
        (tc, ta)
    };
    let recruiters: Vec<PyRep> = ads
        .iter()
        .find(|a| a.corp_id == target_corp && a.ad_id == target_ad)
        .map(|ad| {
            if !ad.recruiters.is_empty() {
                ad.recruiters
                    .iter()
                    .filter(|&&id| id > 0)
                    .map(|&id| PyRep::Integer(id))
                    .collect()
            } else if ad.recruiter_id > 0 {
                vec![PyRep::Integer(ad.recruiter_id)]
            } else {
                Vec::new()
            }
        })
        .unwrap_or_default();
    Some(Ok(PyRep::List(recruiters)))
}

pub(crate) fn require_corp_director(
    ctx: &ServerContext,
    session: &SessionState,
) -> Result<i64, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let now_unix = ctx.now_unix();
    let corp_id = ctx.db.with_snapshot(|snap| {
        let actor = eve_db::CorpActor::resolve(snap, &ctx.sde, char_id, now_unix);
        actor.is_director().then_some(actor.corp_id)
    });
    corp_id.ok_or_else(|| {
        let msg = "Director or CEO role is required";
        BridgeRpcError::with_client_error("CorpRoleDenied", msg, ClientError::CustomInfo(msg.into()))
    })
}

/// Returns the caller's alliance ID when the caller's corporation is that alliance's executor and caller is CEO/Director.
pub(crate) fn require_executor(
    ctx: &ServerContext,
    session: &SessionState,
) -> Result<i64, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let now_unix = ctx.now_unix();
    ctx.db.with_snapshot(|snap| {
        let actor = eve_db::CorpActor::resolve(snap, &ctx.sde, char_id, now_unix);
        // Resolve the corporation's alliance from DB corp records even when an enrolled
        // character is excluded from `corp.members`, so non-member/non-director errors take
        // precedence over `NotInAlliance` on an executor corporation.
        let corp_alliance_id = if actor.corp_id > 0 {
            eve_db::corp_actor::resolve_corp_alliance_id(
                snap,
                &ctx.sde,
                actor.corp_id,
                snap.characters.get(&char_id),
            )
        } else {
            None
        };
        match corp_alliance_id
            .filter(|&aid| aid > 0)
            .and_then(|aid| snap.alliances.get(&aid).map(|a| (aid, a.executor_corp_id)))
        {
            Some((aid, exec_corp)) if actor.corp_id > 0 && exec_corp == actor.corp_id => {
                if actor.is_director() {
                    Ok(aid)
                } else {
                    let msg = "Director or CEO role is required";
                    Err(BridgeRpcError::with_client_error(
                        "CorpRoleDenied",
                        msg,
                        ClientError::CustomInfo(msg.into()),
                    ))
                }
            }
            Some(_) => {
                let msg = "Only the executor corporation can manage the alliance";
                Err(BridgeRpcError::with_client_error(
                    "NotAllianceExecutor",
                    msg,
                    ClientError::CustomInfo(msg.into()),
                ))
            }
            None => {
                let msg = "Your corporation is not in an alliance";
                Err(BridgeRpcError::with_client_error(
                    "NotInAlliance",
                    msg,
                    ClientError::CustomInfo(msg.into()),
                ))
            }
        }
    })
}

pub(crate) fn read_alliance_sov_cfg(
    snap: &eve_db::WorldStateSnapshot,
    aid: i64,
    now_unix: u64,
) -> eve_db::AllianceSovConfig {
    let mut cfg = snap
        .cluster_and_runtime
        .alliance_sov_cfg
        .get(&aid)
        .cloned()
        .unwrap_or_default();
    if cfg.capital_valid_after_unix > 0 && now_unix >= cfg.capital_valid_after_unix {
        if let Some(pending) = cfg.pending_capital_system_id.take() {
            cfg.capital_system_id = Some(pending);
        }
        cfg.capital_valid_after_unix = 0;
    }
    if cfg.prime_valid_after_unix > 0 && now_unix >= cfg.prime_valid_after_unix {
        cfg.current_prime_hour = cfg.new_prime_hour;
        cfg.prime_valid_after_unix = 0;
    }
    cfg
}

pub(crate) fn write_alliance_sov_cfg(
    snap: &mut eve_db::WorldStateSnapshot,
    aid: i64,
    cfg: &eve_db::AllianceSovConfig,
) {
    snap.cluster_and_runtime
        .alliance_sov_cfg
        .insert(aid, cfg.clone());
}

pub(crate) fn resolve_alliance_id(
    session: &SessionState,
    bound_obj_id: Option<i64>,
) -> Result<i64, BridgeRpcError> {
    bound_obj_id
        .filter(|&id| id > 0)
        .or(session.alliance_id)
        .filter(|&id| id > 0)
        .ok_or_else(|| {
            let msg = "Character is not in an alliance";
            BridgeRpcError::with_client_error("NotInAlliance", msg, ClientError::CustomInfo(msg.into()))
        })
}

pub(crate) fn handle_get_capital_system_info(
    ctx: &ServerContext,
    session: &SessionState,
    bound_obj_id: Option<i64>,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let aid = resolve_alliance_id(session, bound_obj_id)?;
    let now_unix = ctx.now_unix();
    let cfg = ctx.db.with_snapshot(|snap| {
        let mut cfg = read_alliance_sov_cfg(snap, aid, now_unix);
        if cfg.capital_system_id.is_none() {
            let sov_sys = snap
                .system_states
                .iter()
                .filter(|(_, st)| st.sov_alliance_id == Some(aid))
                .map(|(&sys_id, _)| sys_id as i64)
                .min();
            cfg.capital_system_id = sov_sys.or_else(|| {
                snap.characters
                    .get(&char_id)
                    .map(|c| c.solar_system_id as i64)
                    .or(session.solar_system_id.map(i64::from))
            });
        }
        cfg
    });
    let valid_after = if cfg.capital_valid_after_unix > 0 {
        super::market_trading::unix_to_filetime(cfg.capital_valid_after_unix)
    } else {
        0
    };
    Ok(PyRep::keyval([
        (
            "currentCapitalSystem",
            cfg.capital_system_id
                .map(PyRep::Integer)
                .unwrap_or(PyRep::None),
        ),
        (
            "newCapitalSystem",
            cfg.pending_capital_system_id
                .map(PyRep::Integer)
                .unwrap_or(PyRep::None),
        ),
        ("newCapitalSystemValidAfter", PyRep::Integer(valid_after)),
    ]))
}

pub(crate) fn handle_set_capital_system(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let _ = ServerContext::require_character(session)?;
    let aid = require_executor(ctx, session)?;
    let rpc = RpcArgs::positional(args);
    let sys_id = rpc.req_u32(0, "solarSystemID")?;
    let _ = rpc.req_positive_i64(0, "solarSystemID")?;
    let sys_id_i64 = i64::from(sys_id);
    if !ctx.sde.solar_systems.contains_key(&sys_id) {
        let msg = format!("Unknown solar system {sys_id}");
        return Err(BridgeRpcError::with_client_error(
            "InvalidSolarSystem",
            msg.clone(),
            ClientError::CustomInfo(msg),
        ));
    }
    let now_unix = ctx.now_unix();
    let old_cap = ctx
        .db
        .try_with_mut_snapshot(|snap| -> Result<Option<i64>, BridgeRpcError> {
            let mut cfg = read_alliance_sov_cfg(snap, aid, now_unix);
            let prev = cfg.capital_system_id;
            cfg.pending_capital_system_id = Some(sys_id_i64);
            cfg.capital_valid_after_unix = now_unix + 7 * 86_400;
            write_alliance_sov_cfg(snap, aid, &cfg);
            Ok(prev)
        })?;
    ctx.emit_typed(NotificationTarget::Alliance(aid), OnUpdateCapitalInfo {});
    ctx.emit_typed(
        NotificationTarget::Alliance(aid),
        OnCapitalSystemChanged {
            change: PyRep::Tuple(vec![
                PyRep::Integer(aid),
                old_cap.map(PyRep::Integer).unwrap_or(PyRep::None),
                PyRep::Integer(sys_id_i64),
            ]),
        },
    );
    Ok(PyRep::Bool(true))
}

pub(crate) fn handle_cancel_capital_system_transition(
    ctx: &ServerContext,
    session: &SessionState,
) -> Result<PyRep, BridgeRpcError> {
    let _ = ServerContext::require_character(session)?;
    let aid = require_executor(ctx, session)?;
    let now_unix = ctx.now_unix();
    ctx.db
        .try_with_mut_snapshot(|snap| -> Result<(), BridgeRpcError> {
            let mut cfg = read_alliance_sov_cfg(snap, aid, now_unix);
            if cfg.pending_capital_system_id.is_none() {
                let msg = "No pending capital system transition to cancel";
                return Err(BridgeRpcError::with_client_error(
                    "NoCapitalSystemTransition",
                    msg,
                    ClientError::CustomInfo(msg.into()),
                ));
            }
            cfg.pending_capital_system_id = None;
            cfg.capital_valid_after_unix = 0;
            write_alliance_sov_cfg(snap, aid, &cfg);
            Ok(())
        })?;
    ctx.emit_typed(NotificationTarget::Alliance(aid), OnUpdateCapitalInfo {});
    Ok(PyRep::Bool(true))
}

pub(crate) fn handle_get_prime_time_info(
    ctx: &ServerContext,
    session: &SessionState,
    bound_obj_id: Option<i64>,
) -> Result<PyRep, BridgeRpcError> {
    let _ = ServerContext::require_character(session)?;
    let aid = resolve_alliance_id(session, bound_obj_id)?;
    let now_unix = ctx.now_unix();
    let cfg = ctx
        .db
        .with_snapshot(|snap| read_alliance_sov_cfg(snap, aid, now_unix));
    let valid_after = if cfg.prime_valid_after_unix > 0 {
        super::market_trading::unix_to_filetime(cfg.prime_valid_after_unix)
    } else {
        0
    };
    Ok(PyRep::keyval([
        ("currentPrimeHour", PyRep::Integer(cfg.current_prime_hour)),
        ("newPrimeHour", PyRep::Integer(cfg.new_prime_hour)),
        ("newPrimeHourValidAfter", PyRep::Integer(valid_after)),
    ]))
}

pub(crate) fn handle_set_prime_hour(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let _ = ServerContext::require_character(session)?;
    let aid = require_executor(ctx, session)?;
    let hour = RpcArgs::positional(args).req_i64(0, "hour")?;
    if !(0..=23).contains(&hour) {
        let msg = format!("Prime hour must be 0..=23, got {hour}");
        return Err(BridgeRpcError::with_client_error(
            "InvalidPrimeHour",
            msg.clone(),
            ClientError::CustomInfo(msg),
        ));
    }
    let now_unix = ctx.now_unix();
    ctx.db
        .try_with_mut_snapshot(|snap| -> Result<(), BridgeRpcError> {
            let mut cfg = read_alliance_sov_cfg(snap, aid, now_unix);
            cfg.new_prime_hour = hour;
            cfg.prime_valid_after_unix = now_unix + 48 * 3600;
            write_alliance_sov_cfg(snap, aid, &cfg);
            Ok(())
        })?;
    ctx.emit_typed(NotificationTarget::Alliance(aid), OnUpdateCapitalInfo {});
    Ok(PyRep::Bool(true))
}

pub(crate) fn handle_get_alliance_sov_structures_info(
    ctx: &ServerContext,
    session: &SessionState,
    bound_obj_id: Option<i64>,
) -> Result<PyRep, BridgeRpcError> {
    let _ = ServerContext::require_character(session)?;
    let aid = resolve_alliance_id(session, bound_obj_id)?;
    let now_unix = ctx.now_unix();
    let (structures_list, campaigns_list, scores_list) = ctx.db.with_snapshot(|snap| {
        let member_corps: HashSet<i64> = snap
            .alliances
            .get(&aid)
            .map(|a| a.member_corp_ids.clone())
            .unwrap_or_default();
        let mut sov_struct_ids = HashSet::new();
        let mut sov_structs: Vec<&eve_db::UpwellStructureRecord> = snap
            .structures
            .values()
            .filter(|s| {
                let owned_by_alliance = member_corps.contains(&s.owner_corp_id)
                    || snap
                        .system_states
                        .get(&s.solar_system_id)
                        .is_some_and(|st| st.sov_alliance_id == Some(aid));
                let is_sov_type = matches!(s.type_id, 32226 | 32458 | 81242)
                    || snap.sov_runtime.sov_hubs.contains_key(&s.structure_id);
                owned_by_alliance && is_sov_type
            })
            .collect();
        sov_structs.sort_by_key(|s| s.structure_id);

        let mut structures_out = Vec::with_capacity(sov_structs.len());
        for s in sov_structs {
            sov_struct_ids.insert(s.structure_id);
            let (vuln_start, vuln_end) = snap
                .sov_runtime
                .adm_vulnerability_windows
                .get(&s.structure_id)
                .map(|&(st, en)| {
                    (
                        super::market_trading::unix_to_filetime(st),
                        super::market_trading::unix_to_filetime(en),
                    )
                })
                .unwrap_or((0, 0));
            let adm = snap
                .system_states
                .get(&s.solar_system_id)
                .map(|st| {
                    1.0 + f64::from(st.military_index.min(5)) * 0.4
                        + f64::from(st.industrial_index.min(5)) * 0.4
                        + f64::from(st.strategic_index.min(5)) * 0.2
                })
                .unwrap_or(1.0);
            structures_out.push(PyRep::keyval([
                ("itemID", PyRep::Integer(s.structure_id)),
                ("typeID", PyRep::Integer(i64::from(s.type_id))),
                ("solarSystemID", PyRep::Integer(i64::from(s.solar_system_id))),
                ("allianceID", PyRep::Integer(aid)),
                ("vulnerabilityOccupancyLevel", PyRep::Real(adm)),
                ("vulnerableStartTime", PyRep::Integer(vuln_start)),
                ("vulnerableEndTime", PyRep::Integer(vuln_end)),
            ]));
        }

        let mut seen_campaigns = std::collections::BTreeMap::<i64, (u32, i64)>::new();
        for &(hub_id, sys_id, cap_aid) in snap.sov_runtime.command_nodes.values() {
            if sov_struct_ids.contains(&hub_id)
                || snap
                    .system_states
                    .get(&sys_id)
                    .is_some_and(|st| st.sov_alliance_id == Some(aid))
            {
                seen_campaigns.entry(hub_id).or_insert((sys_id, cap_aid));
            }
        }
        let mut campaigns_out = Vec::with_capacity(seen_campaigns.len());
        let mut scores_out = Vec::new();
        for (hub_id, (sys_id, _cap_aid)) in seen_campaigns {
            campaigns_out.push(PyRep::keyval([
                ("campaignID", PyRep::Integer(hub_id)),
                ("sourceItemID", PyRep::Integer(hub_id)),
                ("solarSystemID", PyRep::Integer(i64::from(sys_id))),
                ("eventType", PyRep::Integer(8)),
                (
                    "startTime",
                    PyRep::Integer(super::market_trading::unix_to_filetime(now_unix)),
                ),
            ]));
        }
        let mut score_hub_ids: Vec<i64> = snap
            .sov_runtime
            .command_node_scores
            .keys()
            .copied()
            .filter(|hub_id| {
                sov_struct_ids.contains(hub_id)
                    || snap
                        .structures
                        .get(hub_id)
                        .is_some_and(|s| member_corps.contains(&s.owner_corp_id))
            })
            .collect();
        score_hub_ids.sort_unstable();
        for hub_id in score_hub_ids {
            if let Some(entries) = snap.sov_runtime.command_node_scores.get(&hub_id) {
                for &(team_id, score) in entries {
                    scores_out.push(PyRep::keyval([
                        ("sourceItemID", PyRep::Integer(hub_id)),
                        ("teamID", PyRep::Integer(team_id)),
                        ("score", PyRep::Real(score)),
                    ]));
                }
            }
        }
        (structures_out, campaigns_out, scores_out)
    });
    Ok(PyRep::Tuple(vec![
        PyRep::List(structures_list),
        PyRep::List(campaigns_list),
        PyRep::List(scores_list),
    ]))
}

pub(crate) fn handle_get_alliance_bill_balance(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
    bound_obj_id: Option<i64>,
) -> Result<PyRep, BridgeRpcError> {
    let _ = ServerContext::require_character(session)?;
    let aid = resolve_alliance_id(session, bound_obj_id)?;
    let bill_id_opt = RpcArgs::positional(args)
        .get_raw(0, "billID")
        .and_then(|v| v.as_i64())
        .filter(|&id| id > 0);
    ctx.db.with_snapshot(|snap| {
        if let Some(bill_id) = bill_id_opt {
            let Some(note) = snap
                .owner_notes
                .get(&super::wallet_services_ext::bill_note_id(bill_id))
            else {
                let msg = format!("Bill {bill_id} not found");
                return Err(BridgeRpcError::with_client_error(
                    "BillNotFound",
                    msg.clone(),
                    ClientError::CustomInfo(msg),
                ));
            };
            let bill: super::wallet_services_ext::CorpBillRecord =
                serde_json::from_str(&note.note).map_err(|_| {
                    let msg = "Corrupt bill record";
                    BridgeRpcError::with_client_error(
                        "BillNotFound",
                        msg,
                        ClientError::CustomInfo(msg.into()),
                    )
                })?;
            if bill.debtor_id != aid && bill.creditor_id != aid {
                return Err(BridgeRpcError::with_client_error(
                    "AccessDenied",
                    "Bill does not belong to caller alliance",
                    ClientError::user(UserErrorKey::AccessDenied),
                ));
            }
            let bal = if bill.paid {
                0.0
            } else {
                eve_db::IskCents(bill.amount_cents).to_wire_f64()
            };
            Ok(PyRep::Real(bal))
        } else {
            let total_cents: i64 = snap
                .owner_notes
                .values()
                .filter(|n| n.character_id == -71)
                .filter_map(|n| {
                    serde_json::from_str::<super::wallet_services_ext::CorpBillRecord>(&n.note).ok()
                })
                .filter(|b| !b.paid && b.debtor_id == aid)
                .map(|b| b.amount_cents)
                .sum();
            Ok(PyRep::Real(eve_db::IskCents(total_cents).to_wire_f64()))
        }
    })
}

pub(crate) fn handle_get_alliance_bills(
    ctx: &ServerContext,
    session: &SessionState,
    receivable: bool,
    bound_obj_id: Option<i64>,
) -> Result<PyRep, BridgeRpcError> {
    let _ = ServerContext::require_character(session)?;
    let aid = resolve_alliance_id(session, bound_obj_id)?;
    let rows = ctx.db.with_snapshot(|snap| {
        let mut bills: Vec<super::wallet_services_ext::CorpBillRecord> = snap
            .owner_notes
            .values()
            .filter(|n| n.character_id == -71)
            .filter_map(|n| {
                serde_json::from_str::<super::wallet_services_ext::CorpBillRecord>(&n.note).ok()
            })
            .filter(|b| {
                if receivable {
                    b.creditor_id == aid
                } else {
                    !b.paid && b.debtor_id == aid
                }
            })
            .collect();
        bills.sort_by_key(|b| b.bill_id);
        bills
            .iter()
            .map(super::wallet_serverops::bill_to_row)
            .collect::<Vec<_>>()
    });
    Ok(PyRep::rowset(super::wallet_serverops::bill_columns(), rows))
}

pub(crate) fn handle_pay_alliance_bill(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let aid = require_executor(ctx, session)?;
    let rpc = RpcArgs::new(args, kwargs);
    let bill_id = rpc.req_positive_i64(0, "billID")?;
    let from_account_key = rpc
        .opt_i64(1, "fromAccountKey")?
        .unwrap_or(1000)
        .clamp(1000, 1006) as u16;
    let div_id = (from_account_key - 999) as u8;
    let now_unix = ctx.now_unix();
    ctx.db
        .try_with_mut_snapshot(|snap| -> Result<(), BridgeRpcError> {
            let corp_id = snap
                .characters
                .get(&char_id)
                .map(|c| c.corporation_id)
                .unwrap_or(0);
            let nid = super::wallet_services_ext::bill_note_id(bill_id);
            let Some(note) = snap.owner_notes.get(&nid) else {
                let msg = format!("Bill {bill_id} not found");
                return Err(BridgeRpcError::with_client_error(
                    "BillNotFound",
                    msg.clone(),
                    ClientError::CustomInfo(msg),
                ));
            };
            let mut bill: super::wallet_services_ext::CorpBillRecord =
                serde_json::from_str(&note.note).map_err(|_| {
                    let msg = "Corrupt bill record";
                    BridgeRpcError::with_client_error(
                        "BillNotFound",
                        msg,
                        ClientError::CustomInfo(msg.into()),
                    )
                })?;
            if bill.paid {
                let msg = "Bill is already paid";
                return Err(BridgeRpcError::with_client_error(
                    "BillAlreadyPaid",
                    msg,
                    ClientError::CustomInfo(msg.into()),
                ));
            }
            if bill.debtor_id != aid {
                return Err(BridgeRpcError::with_client_error(
                    "AccessDenied",
                    "Bill does not belong to caller alliance",
                    ClientError::user(UserErrorKey::AccessDenied),
                ));
            }
            super::market_trading::market_orders_corp_auth::require_corp_accountant_spend_role(
                snap,
                corp_id,
                char_id,
                from_account_key,
                now_unix,
                "CorpRoleRequired",
            )?;
            let amt = eve_db::IskCents(bill.amount_cents);
            let corp = snap.corporations.get_mut(&corp_id).ok_or_else(|| {
                let msg = "Executor corporation not found";
                BridgeRpcError::with_client_error(
                    "CorporationNotFound",
                    msg,
                    ClientError::CustomInfo(msg.into()),
                )
            })?;
            let bal_after = corp.debit_division(div_id, amt).ok_or_else(|| {
                BridgeRpcError::with_client_error(
                    "NotEnoughMoney",
                    "Insufficient corporation division ISK to pay alliance bill",
                    ClientError::user(UserErrorKey::NotEnoughMoney),
                )
            })?;
            snap.append_journal(
                corp_id,
                "BillPayment",
                amt,
                bal_after,
                format!("Paid alliance bill {bill_id}"),
            );
            let cred_corp = snap
                .alliances
                .get(&bill.creditor_id)
                .map(|a| a.executor_corp_id)
                .unwrap_or(bill.creditor_id);
            if let Some(cred) = snap.corporations.get_mut(&cred_corp) {
                cred.credit_division(1, amt);
            }
            bill.paid = true;
            bill.paid_by_division = Some(from_account_key);
            super::wallet_services_ext::save_json_note(
                snap,
                nid,
                -71,
                bill_id,
                format!("bill:{bill_id}"),
                &bill,
                now_unix,
            );
            Ok(())
        })?;
    ctx.emit_typed_with_id_type(
        NotificationTarget::Alliance(aid),
        "allianceid",
        OnCorpWalletBillsChanged {},
    );
    Ok(PyRep::Bool(true))
}

#[cfg(test)]
mod tests {
    use super::*;
    use eve_db::{AllianceRecord, CorporationRecord};

    fn setup_alliance_exec_ctx() -> (ServerContext, SessionState, i64, i64) {
        let ctx = ServerContext::new();
        let char_id = 90_550_001_i64;
        let corp_id = 98_550_001_i64;
        let aid = 99_550_001_i64;
        ctx.db.with_mut_snapshot(|snap| {
            let mut ch = snap.characters.values().next().unwrap().clone();
            ch.character_id = char_id;
            ch.corporation_id = corp_id;
            ch.alliance_id = Some(aid);
            snap.characters.insert(char_id, ch);

            let mut corp = CorporationRecord::default();
            corp.corporation_id = corp_id;
            corp.name = "Delegation Exec Corp".into();
            corp.ticker = "DEXEC".into();
            corp.ceo_id = char_id;
            corp.alliance_id = Some(aid);
            corp.members = HashSet::from([char_id]);
            corp.member_roles = HashMap::from([(char_id, u64::MAX)]);
            corp.ensure_wallet_divisions();
            snap.corporations.insert(corp_id, corp);

            snap.alliances.insert(
                aid,
                AllianceRecord {
                    alliance_id: aid,
                    name: "Delegation Alliance".into(),
                    short_name: "DALL".into(),
                    executor_corp_id: corp_id,
                    member_corp_ids: HashSet::from([corp_id]),
                },
            );
        });
        let mut sess = SessionState::new(9_550);
        sess.user_id = Some(9_550);
        sess.character_id = Some(char_id);
        sess.corporation_id = Some(corp_id);
        sess.alliance_id = Some(aid);
        sess.corp_role = u64::MAX;
        (ctx, sess, corp_id, aid)
    }

    #[test]
    fn set_capital_system_rejects_u32_truncation_and_non_positive_ids() {
        let (ctx, sess, _, aid) = setup_alliance_exec_ctx();
        let wrapped_jita = (1_i64 << 32) + 30_000_142;
        assert_eq!(
            handle_set_capital_system(&ctx, &sess, &[PyRep::Integer(wrapped_jita)])
                .unwrap_err()
                .code,
            "InvalidArgument"
        );
        for bad in [0_i64, -1_i64] {
            assert_eq!(
                handle_set_capital_system(&ctx, &sess, &[PyRep::Integer(bad)])
                    .unwrap_err()
                    .code,
                "InvalidArgument"
            );
        }
        assert_eq!(
            handle_set_capital_system(&ctx, &sess, &[PyRep::Integer(30_000_142)]).unwrap(),
            PyRep::Bool(true)
        );
        let stored = ctx
            .db
            .with_snapshot(|snap| read_alliance_sov_cfg(snap, aid, 0).pending_capital_system_id);
        assert_eq!(stored, Some(30_000_142));
    }

    #[test]
    fn get_alliance_bill_balance_handles_optional_and_non_int_contract_args() {
        let (ctx, sess, _, aid) = setup_alliance_exec_ctx();
        for arg in [
            vec![],
            vec![PyRep::None],
            vec![PyRep::Integer(0)],
            vec![PyRep::String("Test".into())],
        ] {
            let bal = handle_get_alliance_bill_balance(&ctx, &sess, &arg, Some(aid)).unwrap();
            assert_eq!(bal, PyRep::Real(0.0));
        }
    }

    #[test]
    fn search_corp_ads_rejects_negative_masks_and_accepts_language_mask_kwarg() {
        let (ctx, mut sess, corp_id, _) = setup_alliance_exec_ctx();
        let empty_kw = HashMap::new();
        assert_eq!(
            handle_corp_rec_queries(
                &ctx,
                corp_id,
                0,
                "SearchCorpAds",
                &[PyRep::Integer(-1)],
                &empty_kw,
            )
            .unwrap()
            .unwrap_err()
            .code,
            "InvalidArgument"
        );
        assert_eq!(
            handle_corp_rec_queries(
                &ctx,
                corp_id,
                0,
                "SearchCorpAds",
                &[PyRep::Integer(0), PyRep::Integer(-2)],
                &empty_kw,
            )
            .unwrap()
            .unwrap_err()
            .code,
            "InvalidArgument"
        );
        let bad_kw = HashMap::from([("languageMask".into(), PyRep::Integer(-3))]);
        assert_eq!(
            handle_corp_rec_queries(&ctx, corp_id, 0, "SearchCorpAds", &[PyRep::Integer(0)], &bad_kw)
                .unwrap()
                .unwrap_err()
                .code,
            "InvalidArgument"
        );
        let ok_kw = HashMap::from([("languageMask".into(), PyRep::Integer(4))]);
        assert!(
            handle_corp_rec_queries(&ctx, corp_id, 0, "SearchCorpAds", &[PyRep::Integer(0)], &ok_kw)
                .unwrap()
                .is_ok()
        );
        sess.role = eve_machonet::service_roles::ROLE_GMH;
        assert!(
            handle_get_wallet_divisions_info(&ctx, &mut sess, &[PyRep::Integer(corp_id)])
                .unwrap()
                .is_ok()
        );
    }
}
