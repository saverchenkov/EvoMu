//! Corporation Governance, Shares, Dividends, Titles, Votes, Medals, Bulletins, Recruitment & Alliances (`SO-3`).

use crate::router::{cache_notifications::NotificationTarget, ServerContext};
use eve_db::{
    corp_roles, ChatChannelRecord, CorpTypeface, CorpVoteBallotRecord, CorpWalletDivisionRecord,
    CorporationRecord, IskCents, WalletJournalEntry,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::{HashMap, HashSet};
use crate::services::corp_gov_delegation::{
    handle_corp_rec_queries, handle_get_recruiters, handle_get_shares_by_shareholder,
    handle_get_titles,
};
pub(super) use crate::services::corp_gov_delegation::{
    handle_get_wallet_divisions_info, parse_title_update_rows,
};
#[cfg(test)]
pub(super) use crate::services::corp_gov_delegation::{
    parse_title_dict_entry, parse_title_row_cells,
};
use super::fleet_social_corps::{handle_insert_vote_case, with_corp_gov};
use super::fleet_social_notify::handle_corp_contacts_and_labels;
use crate::services::space_entity_ops::{handle_corp_bulletins, handle_corp_medals};
use super::fleet_social_persistence::{
    eval_vote_case_winner, handle_vote_queries, label_mask_from_ids,
};

pub use eve_db::CorpRecruitmentAd;

pub(crate) fn ensure_corp_rec_ads_populated_in_snap(snap: &mut eve_db::WorldStateSnapshot) {
    if snap.fleet_and_social_ops.corp_recruitment_ads.is_empty() {
        for (&cid, state) in &snap.corp_governance {
            for (aid, title, desc, mask, rec_id) in &state.recruitment_ads {
                snap.fleet_and_social_ops
                    .corp_recruitment_ads
                    .push(CorpRecruitmentAd {
                        ad_id: *aid,
                        corp_id: cid,
                        title: title.clone(),
                        description: desc.clone(),
                        type_mask: *mask,
                        recruiter_id: *rec_id,
                        recruiters: (*rec_id > 0).then_some(*rec_id).into_iter().collect(),
                        created_unix: 0,
                        expiry_unix: u64::MAX,
                        days: 14,
                        lang_mask: 0,
                    });
            }
        }
    }
}

/// V24 `appConst.corporationMaxCorpRecruiters` / `corpRecruitmentContainer.pyj` per-ad recruiter cap.
const MAX_CORP_AD_RECRUITERS: usize = 6;
const MAX_CORP_AD_RECRUITER_INPUT_ITEMS: usize = 64;
const MAX_RECRUITMENT_TITLE_CHARS: usize = 128;
const MAX_RECRUITMENT_TITLE_BYTES: usize = 512;
const MAX_RECRUITMENT_DESC_CHARS: usize = 4_000;
const MAX_RECRUITMENT_DESC_BYTES: usize = 16_000;

struct ParsedRecruitmentAdArgs {
    ad_id: Option<i64>,
    days: u32,
    added_days: u32,
    type_mask: u64,
    lang_mask: u64,
    has_lang_mask: bool,
    title: String,
    description: String,
    recruiters: Option<Vec<i64>>,
}

fn bounded_ad_str(
    val: Option<&PyRep>,
    label: &str,
    default: &str,
    max_chars: usize,
    max_bytes: usize,
) -> Result<String, BridgeRpcError> {
    let s = val.and_then(|a| a.as_str()).unwrap_or(default);
    crate::router::check_str_bounds(s, label, max_chars, max_bytes)?;
    Ok(s.to_string())
}

fn has_explicit_lang_mask(pos: Option<&PyRep>, kwargs: &HashMap<String, PyRep>) -> bool {
    pos.is_some_and(|v| !matches!(v, PyRep::None))
        || kwargs.get("langMask").is_some_and(|v| !matches!(v, PyRep::None))
        || kwargs.get("languageMask").is_some_and(|v| !matches!(v, PyRep::None))
}

fn parse_checked_u64_mask(
    val: Option<&PyRep>,
    field: &str,
    default: u64,
) -> Result<u64, BridgeRpcError> {
    match val {
        None | Some(PyRep::None) => Ok(default),
        Some(v) => v
            .as_i64()
            .and_then(|n| u64::try_from(n).ok())
            .ok_or_else(|| BridgeRpcError {
                code: "InvalidArgument".into(),
                message: format!("{field} must be a non-negative integer bitmask"),
                client_error: None,
            }),
    }
}

fn parse_checked_days(val: Option<&PyRep>, default: u32) -> Result<u32, BridgeRpcError> {
    match val {
        None | Some(PyRep::None) => Ok(default),
        Some(v) => v
            .as_i64()
            .and_then(|n| u32::try_from(n).ok())
            .filter(|&d| (1..=28).contains(&d))
            .ok_or_else(|| BridgeRpcError {
                code: "InvalidArgument".into(),
                message: "Recruitment ad duration (days) must be between 1 and 28".into(),
                client_error: None,
            }),
    }
}

fn parse_checked_added_days(val: Option<&PyRep>) -> Result<u32, BridgeRpcError> {
    match val {
        None | Some(PyRep::None) => Ok(0),
        Some(v) => v
            .as_i64()
            .and_then(|n| u32::try_from(n).ok())
            .filter(|&d| d <= 28)
            .ok_or_else(|| BridgeRpcError {
                code: "InvalidArgument".into(),
                message: "Recruitment ad addedDays must be between 0 and 28".into(),
                client_error: None,
            }),
    }
}

fn parse_recruiter_ids(val: Option<&PyRep>) -> Result<Option<Vec<i64>>, BridgeRpcError> {
    let Some(rep) = val else {
        return Ok(None);
    };
    if matches!(rep, PyRep::None) {
        return Ok(None);
    }
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    match rep {
        PyRep::List(items) | PyRep::Tuple(items) | PyRep::Set(items) | PyRep::PySet(items) => {
            if items.len() > MAX_CORP_AD_RECRUITER_INPUT_ITEMS {
                return Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: format!(
                        "Recruitment ad cannot have more than {MAX_CORP_AD_RECRUITERS} recruiters"
                    ),
                    client_error: None,
                });
            }
            for item in items {
                let Some(id) = item.as_i64().filter(|&id| id > 0) else {
                    return Err(BridgeRpcError {
                        code: "InvalidArgument".into(),
                        message: "Recruitment ad recruiter IDs must be positive character IDs".into(),
                        client_error: None,
                    });
                };
                if seen.insert(id) {
                    if out.len() >= MAX_CORP_AD_RECRUITERS {
                        return Err(BridgeRpcError {
                            code: "InvalidArgument".into(),
                            message: format!(
                                "Recruitment ad cannot have more than {MAX_CORP_AD_RECRUITERS} recruiters"
                            ),
                            client_error: None,
                        });
                    }
                    out.push(id);
                }
            }
        }
        other => {
            let Some(id) = other.as_i64().filter(|&id| id > 0) else {
                return Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "Recruitment ad recruiters must be a list of positive character IDs".into(),
                    client_error: None,
                });
            };
            out.push(id);
        }
    }
    Ok(Some(out))
}

fn parse_recruitment_ad_args(
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<ParsedRecruitmentAdArgs, BridgeRpcError> {
    if method == "CreateRecruitmentAd" {
        if args.first().and_then(|a| a.as_str()).is_some() {
            let title = bounded_ad_str(
                args.first(),
                "title",
                "Nullsec PvP & Industry",
                MAX_RECRUITMENT_TITLE_CHARS,
                MAX_RECRUITMENT_TITLE_BYTES,
            )?;
            let description = bounded_ad_str(
                args.get(1),
                "description",
                "Join our corp today",
                MAX_RECRUITMENT_DESC_CHARS,
                MAX_RECRUITMENT_DESC_BYTES,
            )?;
            let type_mask = parse_checked_u64_mask(
                args.get(2).or_else(|| kwargs.get("typeMask")),
                "typeMask",
                7,
            )?;
            let days = parse_checked_days(args.get(3).or_else(|| kwargs.get("days")), 14)?;
            let lang_mask = parse_checked_u64_mask(
                args.get(4)
                    .or_else(|| kwargs.get("langMask"))
                    .or_else(|| kwargs.get("languageMask")),
                "langMask",
                0,
            )?;
            return Ok(ParsedRecruitmentAdArgs {
                ad_id: None,
                days,
                added_days: 0,
                type_mask,
                lang_mask,
                has_lang_mask: true,
                title,
                description,
                recruiters: None,
            });
        }
        let is_v24 = args.len() >= 9
            || (args.len() >= 6
                && (args.get(5).and_then(|a| a.as_str()).is_some()
                    || args.get(4).is_some_and(|a| {
                        matches!(
                            a,
                            PyRep::List(_)
                                | PyRep::Tuple(_)
                                | PyRep::Set(_)
                                | PyRep::PySet(_)
                                | PyRep::None
                        )
                    })));
        if is_v24 {
            let days = parse_checked_days(args.first().or_else(|| kwargs.get("days")), 14)?;
            let type_mask = parse_checked_u64_mask(
                args.get(1).or_else(|| kwargs.get("typeMask")),
                "typeMask",
                0,
            )?;
            let lang_mask = parse_checked_u64_mask(
                args.get(2)
                    .or_else(|| kwargs.get("langMask"))
                    .or_else(|| kwargs.get("languageMask")),
                "langMask",
                0,
            )?;
            let description = bounded_ad_str(
                args.get(3),
                "description",
                "",
                MAX_RECRUITMENT_DESC_CHARS,
                MAX_RECRUITMENT_DESC_BYTES,
            )?;
            let recruiters = parse_recruiter_ids(args.get(4).or_else(|| kwargs.get("recruiters")))?;
            let title = bounded_ad_str(
                args.get(5),
                "title",
                "",
                MAX_RECRUITMENT_TITLE_CHARS,
                MAX_RECRUITMENT_TITLE_BYTES,
            )?;
            return Ok(ParsedRecruitmentAdArgs {
                ad_id: None,
                days,
                added_days: 0,
                type_mask,
                lang_mask,
                has_lang_mask: true,
                title,
                description,
                recruiters,
            });
        }
        let days = parse_checked_days(args.first().or_else(|| kwargs.get("days")), 14)?;
        let type_mask = parse_checked_u64_mask(
            args.get(1).or_else(|| kwargs.get("typeMask")),
            "typeMask",
            7,
        )?;
        let (title, description, lang_mask) = if args.get(6).and_then(|a| a.as_str()).is_some() {
            let lang = parse_checked_u64_mask(
                args.get(2).or_else(|| kwargs.get("langMask")),
                "langMask",
                0,
            )?;
            let desc = bounded_ad_str(
                args.get(3),
                "description",
                "",
                MAX_RECRUITMENT_DESC_CHARS,
                MAX_RECRUITMENT_DESC_BYTES,
            )?;
            let t = bounded_ad_str(
                args.get(6),
                "title",
                "",
                MAX_RECRUITMENT_TITLE_CHARS,
                MAX_RECRUITMENT_TITLE_BYTES,
            )?;
            (t, desc, lang)
        } else {
            let t = bounded_ad_str(
                args.get(2),
                "title",
                "Corp Recruitment Ad",
                MAX_RECRUITMENT_TITLE_CHARS,
                MAX_RECRUITMENT_TITLE_BYTES,
            )?;
            let desc = bounded_ad_str(
                args.get(3),
                "description",
                "Join our corp today",
                MAX_RECRUITMENT_DESC_CHARS,
                MAX_RECRUITMENT_DESC_BYTES,
            )?;
            let lang = parse_checked_u64_mask(
                args.get(4)
                    .or_else(|| kwargs.get("langMask"))
                    .or_else(|| kwargs.get("languageMask")),
                "langMask",
                0,
            )?;
            (t, desc, lang)
        };
        return Ok(ParsedRecruitmentAdArgs {
            ad_id: None,
            days,
            added_days: 0,
            type_mask,
            lang_mask,
            has_lang_mask: true,
            title,
            description,
            recruiters: None,
        });
    }

    let ad_id = args
        .first()
        .and_then(|a| a.as_i64())
        .filter(|&id| id > 0)
        .ok_or_else(|| BridgeRpcError {
            code: "InvalidArgument".into(),
            message: "UpdateRecruitmentAd requires a positive adID".into(),
            client_error: None,
        })?;
    let is_v24 = args.len() >= 7
        && (args.get(5).and_then(|a| a.as_str()).is_some()
            || args.get(4).is_some_and(|a| {
                matches!(
                    a,
                    PyRep::List(_)
                        | PyRep::Tuple(_)
                        | PyRep::Set(_)
                        | PyRep::PySet(_)
                        | PyRep::None
                )
            }));
    if is_v24 {
        let type_mask = parse_checked_u64_mask(
            args.get(1).or_else(|| kwargs.get("typeMask")),
            "typeMask",
            0,
        )?;
        let lang_arg = args.get(2);
        let has_lang_mask = has_explicit_lang_mask(lang_arg, kwargs);
        let lang_mask = parse_checked_u64_mask(
            lang_arg
                .or_else(|| kwargs.get("langMask"))
                .or_else(|| kwargs.get("languageMask")),
            "langMask",
            0,
        )?;
        let description = bounded_ad_str(
            args.get(3),
            "description",
            "",
            MAX_RECRUITMENT_DESC_CHARS,
            MAX_RECRUITMENT_DESC_BYTES,
        )?;
        let recruiters = parse_recruiter_ids(args.get(4).or_else(|| kwargs.get("recruiters")))?;
        let title = bounded_ad_str(
            args.get(5),
            "title",
            "",
            MAX_RECRUITMENT_TITLE_CHARS,
            MAX_RECRUITMENT_TITLE_BYTES,
        )?;
        let added_days = parse_checked_added_days(args.get(6).or_else(|| kwargs.get("addedDays")))?;
        return Ok(ParsedRecruitmentAdArgs {
            ad_id: Some(ad_id),
            days: 0,
            added_days,
            type_mask,
            lang_mask,
            has_lang_mask,
            title,
            description,
            recruiters,
        });
    }
    let type_mask = parse_checked_u64_mask(
        args.get(1).or_else(|| kwargs.get("typeMask")),
        "typeMask",
        7,
    )?;
    if args.get(6).and_then(|a| a.as_str()).is_some() {
        let lang_arg = args.get(2);
        let has_lang_mask = has_explicit_lang_mask(lang_arg, kwargs);
        let lang_mask = parse_checked_u64_mask(
            lang_arg
                .or_else(|| kwargs.get("langMask"))
                .or_else(|| kwargs.get("languageMask")),
            "langMask",
            0,
        )?;
        let description = bounded_ad_str(
            args.get(3),
            "description",
            "",
            MAX_RECRUITMENT_DESC_CHARS,
            MAX_RECRUITMENT_DESC_BYTES,
        )?;
        let title = bounded_ad_str(
            args.get(6),
            "title",
            "",
            MAX_RECRUITMENT_TITLE_CHARS,
            MAX_RECRUITMENT_TITLE_BYTES,
        )?;
        let added_days = parse_checked_added_days(args.get(7).or_else(|| kwargs.get("addedDays")))?;
        return Ok(ParsedRecruitmentAdArgs {
            ad_id: Some(ad_id),
            days: 0,
            added_days,
            type_mask,
            lang_mask,
            has_lang_mask,
            title,
            description,
            recruiters: None,
        });
    }
    let title = bounded_ad_str(
        args.get(2),
        "title",
        "Updated Corp Ad",
        MAX_RECRUITMENT_TITLE_CHARS,
        MAX_RECRUITMENT_TITLE_BYTES,
    )?;
    let description = bounded_ad_str(
        args.get(3),
        "description",
        "Updated description",
        MAX_RECRUITMENT_DESC_CHARS,
        MAX_RECRUITMENT_DESC_BYTES,
    )?;
    let lang_arg = args.get(4);
    let has_lang_mask = has_explicit_lang_mask(lang_arg, kwargs);
    let lang_mask = parse_checked_u64_mask(
        lang_arg
            .or_else(|| kwargs.get("langMask"))
            .or_else(|| kwargs.get("languageMask")),
        "langMask",
        0,
    )?;
    Ok(ParsedRecruitmentAdArgs {
        ad_id: Some(ad_id),
        days: 0,
        added_days: 0,
        type_mask,
        lang_mask,
        has_lang_mask,
        title,
        description,
        recruiters: None,
    })
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
    let rpc = crate::router::RpcArgs::positional(args);
    let name = match args.first() {
        None | Some(PyRep::None) => {
            return Some(Err(BridgeRpcError {
                code: "InvalidCorporationName".into(),
                message: "Corporation name is required".into(),
                client_error: None,
            }));
        }
        Some(_) => match rpc.req_str_max(0, "corporationName", 100, 400) {
            Ok(s) => {
                let trimmed = s.trim();
                if trimmed.is_empty() {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidCorporationName".into(),
                        message: "Corporation name is required".into(),
                        client_error: None,
                    }));
                }
                trimmed
            }
            Err(e) => return Some(Err(e)),
        },
    };
    let ticker = match rpc.opt_str_max(1, "ticker", 16, 64) {
        Ok(v) => v.unwrap_or("EVOMU"),
        Err(e) => return Some(Err(e)),
    };
    let (
        description,
        url,
        tax_rate,
        shape1,
        shape2,
        shape3,
        color1,
        color2,
        color3,
        typeface,
        apps_enabled,
        friendly_fire,
        lp_tax_rate,
        explicit_lp_tax,
    ) = if args.len() >= 5 || args.get(2).and_then(|a| a.as_str()).is_some() {
        let description = match rpc.opt_str_max(2, "description", 4_000, 16_000) {
            Ok(v) => v.unwrap_or(""),
            Err(e) => return Some(Err(e)),
        };
        let (url, tax) = if matches!(args.get(3), Some(PyRep::Real(_))) {
            let tax = match rpc.opt_finite_f64(3, "taxRate") {
                Ok(v) => v.unwrap_or(0.10).clamp(0.0, 1.0),
                Err(e) => return Some(Err(e)),
            };
            ("", tax)
        } else {
            let url = match rpc.opt_str_max(3, "url", 256, 1_024) {
                Ok(v) => v.unwrap_or(""),
                Err(e) => return Some(Err(e)),
            };
            let tax = match rpc.opt_finite_f64(4, "taxRate") {
                Ok(v) => v.unwrap_or(0.10).clamp(0.0, 1.0),
                Err(e) => return Some(Err(e)),
            };
            (url, tax)
        };
        let explicit_lp_tax = match rpc.opt_finite_f64(14, "loyaltyPointTaxRate") {
            Ok(v) => v.map(|x| x.clamp(0.0, 1.0)),
            Err(e) => return Some(Err(e)),
        };
        let lp_tax = explicit_lp_tax.unwrap_or(tax);
        let tf = match args.get(11) {
            Some(PyRep::None) | None => None,
            Some(PyRep::Integer(i)) => Some(CorpTypeface::Integer(*i)),
            Some(_) => match rpc.opt_str_max(11, "typeface", 64, 256) {
                Ok(Some(s)) => Some(CorpTypeface::String(s.to_string())),
                Ok(None) => None,
                Err(e) => return Some(Err(e)),
            },
        };
        let apps = args
            .get(12)
            .map(|a| match a {
                PyRep::Bool(b) => *b,
                PyRep::Integer(i) => *i != 0,
                _ => true,
            })
            .unwrap_or(true);
        let ff = args
            .get(13)
            .map(|a| match a {
                PyRep::Bool(b) => *b,
                PyRep::Integer(i) => *i != 0,
                _ => false,
            })
            .unwrap_or(false);
        (
            description,
            url,
            tax,
            args.get(5).and_then(|a| a.as_i64()).unwrap_or(415),
            args.get(6).and_then(|a| a.as_i64()).unwrap_or(416),
            args.get(7).and_then(|a| a.as_i64()).unwrap_or(417),
            args.get(8).and_then(|a| a.as_i64()).unwrap_or(671),
            args.get(9).and_then(|a| a.as_i64()).unwrap_or(672),
            args.get(10).and_then(|a| a.as_i64()).unwrap_or(673),
            tf,
            apps,
            ff,
            lp_tax,
            explicit_lp_tax,
        )
    } else {
        let tax = match rpc.opt_finite_f64(2, "taxRate") {
            Ok(v) => v.unwrap_or(0.10).clamp(0.0, 1.0),
            Err(e) => return Some(Err(e)),
        };
        (
            "",
            "",
            tax,
            415,
            416,
            417,
            671,
            672,
            673,
            None,
            true,
            false,
            tax,
            None,
        )
    };
    let reg_fee = IskCents::from_isk(1_599_800);
    let now_unix = ctx.now_unix();
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
        corp_rec.loyalty_point_tax_rate = explicit_lp_tax;
        corp_rec.typeface = typeface.clone();
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
        state.friendly_fire_legal = friendly_fire;
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
        let corp_hq_station = session.station_id.or(char_station_id);
        let allow_war = snap
            .fleet_and_social_ops
            .corp_allow_war
            .get(&cid)
            .copied()
            .unwrap_or(true);
        let mut raw_cols: Vec<(String, PyRep)> = vec![
            ("corporationID".into(), PyRep::Integer(cid)),
            ("corporationName".into(), PyRep::String(name.to_string())),
            ("ticker".into(), PyRep::String(ticker.to_string())),
            ("tickerName".into(), PyRep::String(ticker.to_string())),
            ("ceoID".into(), PyRep::Integer(char_id)),
            ("creatorID".into(), PyRep::Integer(char_id)),
            ("memberCount".into(), PyRep::Integer(1)),
            ("memberLimit".into(), PyRep::Integer(init_limit as i64)),
            ("taxRate".into(), PyRep::Real(tax_rate)),
            ("iskTaxRate".into(), PyRep::Real(tax_rate)),
            ("loyaltyPointTaxRate".into(), PyRep::Real(lp_tax_rate)),
            ("allianceID".into(), PyRep::None),
            ("stationID".into(), corp_hq_station.map(PyRep::Integer).unwrap_or(PyRep::None)),
            ("description".into(), PyRep::String(description.to_string())),
            ("url".into(), PyRep::String(url.to_string())),
            ("shares".into(), PyRep::Integer(1_000)),
            ("shape1".into(), PyRep::Integer(shape1)),
            ("shape2".into(), PyRep::Integer(shape2)),
            ("shape3".into(), PyRep::Integer(shape3)),
            ("color1".into(), PyRep::Integer(color1)),
            ("color2".into(), PyRep::Integer(color2)),
            ("color3".into(), PyRep::Integer(color3)),
            ("applicationsEnabled".into(), PyRep::Bool(apps_enabled)),
            ("isRecruiting".into(), PyRep::Bool(apps_enabled)),
            ("allowWar".into(), PyRep::Bool(allow_war)),
            ("friendlyFire".into(), PyRep::Bool(friendly_fire)),
            ("deleted".into(), PyRep::Integer(0)),
        ];
        if let Some(ref tf_val) = typeface {
            raw_cols.push((
                "typeface".into(),
                super::fleet_social_corps::corp_typeface_to_pyrep(tf_val),
            ));
        }
        let default_h = ["1st Division", "2nd Division", "3rd Division", "4th Division", "5th Division", "6th Division", "7th Division"];
        for i in 1..=7u8 {
            raw_cols.push((format!("division{i}"), PyRep::String(default_h[(i - 1) as usize].into())));
            let w_name = if i == 1 { "Master Wallet".into() } else { format!("Division {i}") };
            raw_cols.push((format!("walletDivision{i}"), PyRep::String(w_name)));
        }
        let corp_change: Vec<(PyRep, PyRep)> = raw_cols
            .into_iter()
            .map(|(k, v)| (PyRep::String(k), PyRep::Tuple(vec![PyRep::None, v])))
            .collect();
        Ok((old_corp_id, cid, released, corp_change))
    });

    match res {
        Ok((old_corp_id, corp_id, released, corp_change)) => {
            crate::services::structure_control_ops::notify_released_structure_controllers(ctx, &released);
            let old_corp_role = session.corp_role;
            let old_alliance_id = session.alliance_id;
            session.corporation_id = Some(corp_id);
            session.alliance_id = None;
            session.corp_role = u64::MAX;
            super::fleet_social_persistence::queue_forced_corp_sync(
                ctx,
                char_id,
                corp_id,
                None,
                u64::MAX,
            );
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
            let change = PyRep::Dict(corp_change);
            ctx.emit_typed(
                NotificationTarget::Corporation(corp_id),
                crate::router::notify_registry::OnCorporationChanged { corp_id, change },
            );
            let _ = crate::services::quasar_social_chat::on_character_local_chat_membership_refreshed(
                &ctx.quasar_gateway, &ctx.db, char_id as u32, now_unix,
            );
            crate::services::quasar_career_goals::record_career_sub_kind(
                ctx,
                char_id,
                "social",
                Some("join_corp"),
                1,
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

/// Resolves the caller's character and corporation context without mutating role state.
pub(super) fn resolve_char_corp(
    ctx: &ServerContext,
    session: &SessionState,
) -> Result<(i64, i64, u64), BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let now_unix = ctx.now_unix();
    let corp_id = ctx.db.with_snapshot(|s| {
        eve_db::CorpActor::resolve(s, &ctx.sde, char_id, now_unix).corp_id
    });
    Ok((corp_id, char_id, now_unix))
}

pub(super) fn handle_corp_gov(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (corp_id, char_id, now_unix) = match resolve_char_corp(ctx, session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let (has_director, has_comms, has_diplomat) = ctx.db.with_snapshot(|snap| {
        let actor =
            eve_db::CorpActor::resolve_for_corp(snap, &ctx.sde, corp_id, char_id, None, now_unix);
        (
            actor.is_director(),
            actor.is_director_or(corp_roles::ROLE_PERSONNEL_MANAGER | corp_roles::ROLE_CHAT_MANAGER),
            actor.is_director_or(corp_roles::ROLE_DIPLOMAT),
        )
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
                if eve_db::is_npc_corporation(&ctx.sde, corp_id) {
                    return Err(BridgeRpcError {
                        code: "CorpRoleDenied".into(),
                        message: "NPC corporations cannot pay dividends".into(),
                        client_error: None,
                    });
                }
                let (cur_bal, member_ids) = {
                    let actor = eve_db::CorpActor::resolve_for_corp(
                        snap, &ctx.sde, corp_id, char_id, None, now_unix,
                    );
                    let Some(corp) = snap.corporations.get_mut(&corp_id) else {
                        return Err(BridgeRpcError { code: "CorporationNotFound".into(), message: format!("Corporation {corp_id} not found"), client_error: None });
                    };
                    if !actor.is_director() {
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
                let (changes_out, sync_updates, matured_grants) = match ctx.db.try_with_mut_snapshot(|snap| {
                let matured_grants = super::fleet_social_corps::fold_due_pending_corp_grants_in_snap(snap, corp_id, now_unix);
                if snap.corporations.contains_key(&corp_id) {
                    snap.seed_default_corp_titles(corp_id);
                }
                let corp_ally = eve_db::corp_actor::resolve_corp_alliance_id(snap, &ctx.sde, corp_id, None);
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
                        let new_eff = eve_db::CorpActor::resolve_for_corp(
                            snap, &ctx.sde, corp_id, mid, None, now_unix,
                        )
                        .effective_roles;
                        sync_updates.push((mid, (corp_id, corp_ally, new_eff)));
                    }
                }
                    results.push((title_id, title_name.to_string(), roles_mask, prev));
                }
                Ok::<_, BridgeRpcError>((results, sync_updates, matured_grants))
            }) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            super::fleet_social_corps::emit_matured_corp_grants(ctx, corp_id, matured_grants);
            for (mid, entry) in sync_updates {
                if mid == char_id {
                    session.corp_role = entry.2;
                }
                super::fleet_social_persistence::queue_forced_corp_sync(
                    ctx, mid, entry.0, entry.1, entry.2,
                );
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
                let corp_ally = eve_db::corp_actor::resolve_corp_alliance_id(snap, &ctx.sde, corp_id, None);
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
                            let new_eff = eve_db::CorpActor::resolve_for_corp(
                                snap, &ctx.sde, corp_id, mid, None, now_unix,
                            )
                            .effective_roles;
                            sync_updates.push((mid, (corp_id, corp_ally, new_eff)));
                        }
                    }
                }
                Ok::<_, BridgeRpcError>((prev, sync_updates))
            }) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            for (mid, entry) in sync_updates {
                if mid == char_id {
                    session.corp_role = entry.2;
                }
                super::fleet_social_persistence::queue_forced_corp_sync(
                    ctx, mid, entry.0, entry.1, entry.2,
                );
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
                let corp_ally = eve_db::corp_actor::resolve_corp_alliance_id(snap, &ctx.sde, corp_id, None);
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
                let matured_grants = super::fleet_social_corps::fold_due_pending_corp_grants_in_snap(snap, corp_id, now_unix);
                let corp = snap.corporations.get_mut(&corp_id).unwrap();
                corp.ceo_id = next_ceo;
                if next_ceo != char_id {
                    corp.member_roles.insert(char_id, 0);
                }
                if next_ceo > 0 {
                    corp.member_roles.insert(next_ceo, u64::MAX);
                }
                snap.corp_role_assignments.remove(&(corp_id, char_id));
                Ok((next_ceo, corp_ally, matured_grants))
            });
            match res {
                Ok((next_ceo, corp_ally, matured_grants)) => {
                    super::fleet_social_corps::emit_matured_corp_grants(ctx, corp_id, matured_grants);
                    if next_ceo != char_id {
                        session.corp_role = 0;
                        super::fleet_social_persistence::queue_forced_corp_sync(
                            ctx, char_id, corp_id, corp_ally, 0,
                        );
                    }
                    if next_ceo > 0 {
                        super::fleet_social_persistence::queue_forced_corp_sync(
                            ctx,
                            next_ceo,
                            corp_id,
                            corp_ally,
                            u64::MAX,
                        );
                    }
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
            let res = ctx.db.try_with_mut_snapshot(|snap| -> Result<Vec<super::fleet_social_corps::MaturedCorpGrant>, BridgeRpcError> {
                let mut matured_grants = Vec::new();
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
                    let is_candidate_member = snap.corporations.get(&corp_id).ok_or_else(|| BridgeRpcError { code: "CorpNotFound".into(), message: "Corporation not found".into(), client_error: None })?.members.contains(&new_ceo_id);
                    if !is_candidate_member {
                        return Err(BridgeRpcError { code: "CandidateNotMember".into(), message: "CEO election candidate is not a member of the corporation".into(), client_error: None });
                    }
                    matured_grants = super::fleet_social_corps::fold_due_pending_corp_grants_in_snap(snap, corp_id, now_unix);
                    let corp = snap.corporations.get_mut(&corp_id).unwrap();
                    old_ceo_id = corp.ceo_id;
                    if old_ceo_id > 0 && old_ceo_id != new_ceo_id {
                        if corp.member_base_roles_opt(old_ceo_id) == Some(u64::MAX) {
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
                Ok(matured_grants)
            });
            match res {
                Ok(matured_grants) => {
                    super::fleet_social_corps::emit_matured_corp_grants(ctx, corp_id, matured_grants);
                    if is_ceo_vote {
                        let (corp_ally, old_ceo_roles) = ctx.db.with_snapshot(|s| {
                            let ally = eve_db::corp_actor::resolve_corp_alliance_id(
                                s, &ctx.sde, corp_id, None,
                            );
                            let r = if old_ceo_id > 0 && old_ceo_id != new_ceo_id {
                                eve_db::CorpActor::resolve_for_corp(
                                    s, &ctx.sde, corp_id, old_ceo_id, None, now_unix,
                                )
                                .effective_roles
                            } else {
                                0
                            };
                            (ally, r)
                        });
                        if old_ceo_id > 0 && old_ceo_id != new_ceo_id {
                            super::fleet_social_persistence::queue_forced_corp_sync(
                                ctx,
                                old_ceo_id,
                                corp_id,
                                corp_ally,
                                old_ceo_roles,
                            );
                        }
                        super::fleet_social_persistence::queue_forced_corp_sync(
                            ctx,
                            new_ceo_id,
                            corp_id,
                            corp_ally,
                            u64::MAX,
                        );
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
                eve_db::CorpActor::resolve_for_corp(
                    snap, &ctx.sde, corp_id, char_id, None, now_unix,
                )
                .is_director_or(corp_roles::ROLE_PERSONNEL_MANAGER)
            });
            if !has_role {
                return deny_role("Managing recruitment ads requires Personnel Manager or Director role");
            }
            let parsed = match parse_recruitment_ad_args(method, args, kwargs) {
                Ok(p) => p,
                Err(e) => return Some(Err(e)),
            };
            let (account_key, wallet_div) = super::active_corp_wallet_div(session);
            let ad_fee = IskCents::from_isk(250_000);
            let title_out = parsed.title.clone();
            let type_mask_out = parsed.type_mask;
            let tx_res = ctx.db.try_with_mut_snapshot(|snap| -> Result<i64, BridgeRpcError> {
                if method == "CreateRecruitmentAd" || parsed.added_days > 0 {
                    super::ensure_corp_wallet_take_access(
                        snap,
                        corp_id,
                        char_id,
                        None,
                        wallet_div,
                        now_unix,
                    )?;
                }
                if method == "UpdateRecruitmentAd" {
                    let id = parsed.ad_id.unwrap_or(1);
                    let Some(corp_ref) = snap.corporations.get(&corp_id) else {
                        return Err(BridgeRpcError {
                            code: "CorporationNotFound".into(),
                            message: format!("Corporation {corp_id} not found"),
                            client_error: None,
                        });
                    };
                    if let Some(ref recs) = parsed.recruiters {
                        if let Some(&bad_id) = recs
                            .iter()
                            .find(|rid| **rid != corp_ref.ceo_id && !corp_ref.members.contains(rid))
                        {
                            return Err(BridgeRpcError {
                                code: "InvalidArgument".into(),
                                message: format!(
                                    "Recruiter {bad_id} is not a member of corporation {corp_id}"
                                ),
                                client_error: None,
                            });
                        }
                    }
                    let Some(ad_idx) = snap
                        .fleet_and_social_ops
                        .corp_recruitment_ads
                        .iter()
                        .position(|a| a.ad_id == id && a.corp_id == corp_id)
                    else {
                        return Err(BridgeRpcError {
                            code: "RecruitmentAdNotFound".into(),
                            message: format!("Recruitment ad {id} not found"),
                            client_error: None,
                        });
                    };
                    if parsed.added_days > 0 {
                        let corp = snap.corporations.get_mut(&corp_id).expect("checked above");
                        if corp.debit_division(wallet_div, ad_fee).is_none() {
                            return Err(BridgeRpcError {
                                code: "NotEnoughISK".into(),
                                message: "Extending a corporation recruitment ad requires 250,000 ISK in the active corporation wallet division".into(),
                                client_error: None,
                            });
                        }
                        let bal = corp.division_balance(wallet_div);
                        snap.append_journal_entry(eve_db::ledger::JournalEntryInput {
                            owner_id: corp_id,
                            entry_type: "corp_recruitment_ad_fee",
                            amount: IskCents(-ad_fee.0),
                            balance_after: bal,
                            reason: "Corporation recruitment advertisement extension fee".into(),
                            meta: eve_db::ledger::JournalMeta::new(
                                eve_db::ledger::ref_types::CORPORATION_ACCOUNT_WITHDRAWAL,
                                char_id,
                                corp_id,
                                now_unix,
                            )
                            .with_account_key(account_key),
                        });
                    }
                    let ad = &mut snap.fleet_and_social_ops.corp_recruitment_ads[ad_idx];
                    ad.title = parsed.title.clone();
                    ad.description = parsed.description.clone();
                    ad.type_mask = parsed.type_mask;
                    if parsed.has_lang_mask {
                        ad.lang_mask = parsed.lang_mask;
                    }
                    if parsed.added_days > 0 {
                        let max_expiry = now_unix.saturating_add(28 * 86_400);
                        ad.days = ad.days.saturating_add(parsed.added_days).min(28);
                        ad.expiry_unix = ad
                            .expiry_unix
                            .max(now_unix)
                            .saturating_add(u64::from(parsed.added_days).saturating_mul(86_400))
                            .min(max_expiry);
                    }
                    if let Some(ref recs) = parsed.recruiters {
                        let primary_recruiter = recs.first().copied().unwrap_or(0);
                        ad.recruiter_id = primary_recruiter;
                        ad.recruiters = recs.clone();
                        if let Some(state) = snap.corp_governance.get_mut(&corp_id) {
                            if let Some(gov_ad) = state.recruitment_ads.iter_mut().find(|a| a.0 == id) {
                                gov_ad.1 = parsed.title.clone();
                                gov_ad.2 = parsed.description.clone();
                                gov_ad.3 = parsed.type_mask;
                                gov_ad.4 = primary_recruiter;
                            }
                        }
                    } else if let Some(state) = snap.corp_governance.get_mut(&corp_id) {
                        if let Some(gov_ad) = state.recruitment_ads.iter_mut().find(|a| a.0 == id) {
                            gov_ad.1 = parsed.title.clone();
                            gov_ad.2 = parsed.description.clone();
                            gov_ad.3 = parsed.type_mask;
                        }
                    }
                    return Ok(id);
                }

                let Some(corp) = snap.corporations.get_mut(&corp_id) else {
                    return Err(BridgeRpcError {
                        code: "CorporationNotFound".into(),
                        message: format!("Corporation {corp_id} not found"),
                        client_error: None,
                    });
                };
                if let Some(ref recs) = parsed.recruiters {
                    if let Some(&bad_id) = recs
                        .iter()
                        .find(|rid| **rid != corp.ceo_id && !corp.members.contains(rid))
                    {
                        return Err(BridgeRpcError {
                            code: "InvalidArgument".into(),
                            message: format!(
                                "Recruiter {bad_id} is not a member of corporation {corp_id}"
                            ),
                            client_error: None,
                        });
                    }
                }
                if corp.debit_division(wallet_div, ad_fee).is_none() {
                    return Err(BridgeRpcError {
                        code: "NotEnoughISK".into(),
                        message: "Creating a corporation recruitment ad requires 250,000 ISK in the active corporation wallet division".into(),
                        client_error: None,
                    });
                }
                let corp_bal = corp.division_balance(wallet_div);
                snap.append_journal_entry(eve_db::ledger::JournalEntryInput {
                    owner_id: corp_id,
                    entry_type: "corp_recruitment_ad_fee",
                    amount: IskCents(-ad_fee.0),
                    balance_after: corp_bal,
                    reason: "Corporation recruitment advertisement fee".into(),
                    meta: eve_db::ledger::JournalMeta::new(
                        eve_db::ledger::ref_types::CORPORATION_ACCOUNT_WITHDRAWAL,
                        char_id,
                        corp_id,
                        now_unix,
                    )
                    .with_account_key(account_key),
                });
                let id = snap
                    .fleet_and_social_ops
                    .corp_recruitment_ads
                    .iter()
                    .map(|a| a.ad_id)
                    .max()
                    .unwrap_or(0)
                    + 1;
                let default_recruiters = [char_id];
                let (recruiters, primary_recruiter) = match parsed.recruiters.as_deref() {
                    Some(recs) => (recs, recs.first().copied().unwrap_or(0)),
                    None => (&default_recruiters[..], char_id),
                };
                let expiry_unix =
                    now_unix.saturating_add(u64::from(parsed.days).saturating_mul(86_400));
                snap.fleet_and_social_ops
                    .corp_recruitment_ads
                    .push(CorpRecruitmentAd {
                        ad_id: id,
                        corp_id,
                        title: parsed.title.clone(),
                        description: parsed.description.clone(),
                        type_mask: parsed.type_mask,
                        recruiter_id: primary_recruiter,
                        recruiters: recruiters.to_vec(),
                        created_unix: now_unix,
                        expiry_unix,
                        days: parsed.days,
                        lang_mask: parsed.lang_mask,
                    });
                snap.corp_governance
                    .entry(corp_id)
                    .or_default()
                    .recruitment_ads
                    .push((
                        id,
                        parsed.title.clone(),
                        parsed.description.clone(),
                        parsed.type_mask,
                        primary_recruiter,
                    ));
                Ok(id)
            });
            let ad_id = match tx_res {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
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
                ("title", PyRep::String(title_out)),
                ("typeMask", PyRep::Integer(type_mask_out as i64)),
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
                eve_db::CorpActor::resolve_for_corp(
                    snap, &ctx.sde, corp_id, char_id, None, now_unix,
                )
                .is_director_or(corp_roles::ROLE_PERSONNEL_MANAGER)
            });
            if !has_role {
                return deny_role("Deleting recruitment ads requires Personnel Manager or Director role");
            }
            let ad_id = args.first().and_then(|a| a.as_i64()).unwrap_or(1);
            let del_res = ctx.db.try_with_mut_snapshot(|snap| {
                let before = snap.fleet_and_social_ops.corp_recruitment_ads.len();
                snap.fleet_and_social_ops
                    .corp_recruitment_ads
                    .retain(|a| a.ad_id != ad_id || a.corp_id != corp_id);
                if snap.fleet_and_social_ops.corp_recruitment_ads.len() == before {
                    return Err(BridgeRpcError {
                        code: "RecruitmentAdNotFound".into(),
                        message: format!("Recruitment ad {ad_id} not found"),
                        client_error: None,
                    });
                }
                if let Some(s) = snap.corp_governance.get_mut(&corp_id) {
                    s.recruitment_ads.retain(|a| a.0 != ad_id);
                }
                Ok(())
            });
            if let Err(e) = del_res {
                return Some(Err(e));
            }
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

use crate::router::rpc_registry::{rpc_entry, RpcDispatchHandler, RpcEntry};

fn dispatch_rpc(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    handle_corp_gov(ctx, session, service, method, args, kwargs)
}

pub(crate) const RPCS: &[RpcEntry] = &[
    rpc_entry("corpRegistry", "MoveCompanyShares", RpcDispatchHandler::Sync(dispatch_rpc)),
    rpc_entry("corpRegistry", "MovePrivateShares", RpcDispatchHandler::Sync(dispatch_rpc)),
    rpc_entry("corpRegistry", "GetSharesByShareholder", RpcDispatchHandler::Sync(dispatch_rpc)),
    rpc_entry("corpRegistry", "GetShareholders", RpcDispatchHandler::Sync(dispatch_rpc)),
    rpc_entry("corpRegistry", "GetMemberIDsWithMoreThanAvgShares", RpcDispatchHandler::Sync(dispatch_rpc)),
    rpc_entry("corpRegistry", "PayoutDividend", RpcDispatchHandler::Sync(dispatch_rpc)),
    rpc_entry("corpRegistry", "UpdateTitle", RpcDispatchHandler::Sync(dispatch_rpc)),
    rpc_entry("corpRegistry", "UpdateTitles", RpcDispatchHandler::Sync(dispatch_rpc)),
    rpc_entry("corpRegistry", "GetTitles", RpcDispatchHandler::Sync(dispatch_rpc)),
    rpc_entry("corpRegistry", "DeleteTitle", RpcDispatchHandler::Sync(dispatch_rpc)),
    rpc_entry("corpRegistry", "ResignFromCEO", RpcDispatchHandler::Sync(dispatch_rpc)),
    rpc_entry("corpRegistry", "GetNumberOfPotentialCEOs", RpcDispatchHandler::Sync(dispatch_rpc)),
    rpc_entry("voteManager", "CanRunForCEO", RpcDispatchHandler::Sync(dispatch_rpc)),
    rpc_entry("voteManager", "CanViewVotes", RpcDispatchHandler::Sync(dispatch_rpc)),
    rpc_entry("voteManager", "InsertVoteCase", RpcDispatchHandler::Sync(dispatch_rpc)),
    rpc_entry("corpRegistry", "InsertVoteCase", RpcDispatchHandler::Sync(dispatch_rpc)),
    rpc_entry("voteManager", "InsertVote", RpcDispatchHandler::Sync(dispatch_rpc)),
    rpc_entry("corpRegistry", "InsertVote", RpcDispatchHandler::Sync(dispatch_rpc)),
    rpc_entry("voteManager", "GetVotes", RpcDispatchHandler::Sync(dispatch_rpc)),
    rpc_entry("voteManager", "GetVoteCase", RpcDispatchHandler::Sync(dispatch_rpc)),
    rpc_entry("voteManager", "GetVoteCasesByCorporation", RpcDispatchHandler::Sync(dispatch_rpc)),
    rpc_entry("voteManager", "GetVoteCaseOptions", RpcDispatchHandler::Sync(dispatch_rpc)),
    rpc_entry("voteManager", "GetCorpSanctionedActions", RpcDispatchHandler::Sync(dispatch_rpc)),
    rpc_entry("voteManager", "ActivateSanctionedAction", RpcDispatchHandler::Sync(dispatch_rpc)),
    rpc_entry("corporationSvc", "CreateMedal", RpcDispatchHandler::Sync(dispatch_rpc)),
    rpc_entry("corporationSvc", "GiveMedalToCharacters", RpcDispatchHandler::Sync(dispatch_rpc)),
    rpc_entry("corporationSvc", "SetMedalStatus", RpcDispatchHandler::Sync(dispatch_rpc)),
    rpc_entry("corporationSvc", "GetAllCorpMedals", RpcDispatchHandler::Sync(dispatch_rpc)),
    rpc_entry("corporationSvc", "GetMedalDetails", RpcDispatchHandler::Sync(dispatch_rpc)),
    rpc_entry("corporationSvc", "GetMedalsReceived", RpcDispatchHandler::Sync(dispatch_rpc)),
    rpc_entry("corporationSvc", "GetRecipientsOfMedal", RpcDispatchHandler::Sync(dispatch_rpc)),
    rpc_entry("corporationSvc", "GetEmploymentRecord", RpcDispatchHandler::Sync(dispatch_rpc)),
    rpc_entry("corpRegistry", "AddBulletin", RpcDispatchHandler::Sync(dispatch_rpc)),
    rpc_entry("corpRegistry", "UpdateBulletinOrder", RpcDispatchHandler::Sync(dispatch_rpc)),
    rpc_entry("corpRegistry", "GetBulletins", RpcDispatchHandler::Sync(dispatch_rpc)),
    rpc_entry("corpRegistry", "DeleteBulletin", RpcDispatchHandler::Sync(dispatch_rpc)),
    rpc_entry("corpRecProxy", "CreateRecruitmentAd", RpcDispatchHandler::Sync(dispatch_rpc)),
    rpc_entry("corpRecProxy", "UpdateRecruitmentAd", RpcDispatchHandler::Sync(dispatch_rpc)),
    rpc_entry("corpRecProxy", "GetRecruiters", RpcDispatchHandler::Sync(dispatch_rpc)),
    rpc_entry("corpRecProxy", "GetRecruitmentAdsForCorporation", RpcDispatchHandler::Sync(dispatch_rpc)),
    rpc_entry("corpRecProxy", "GetRecruitmentAdsByCorpID", RpcDispatchHandler::Sync(dispatch_rpc)),
    rpc_entry("corpRecProxy", "GetRecruitmentAd", RpcDispatchHandler::Sync(dispatch_rpc)),
    rpc_entry("corpRecProxy", "SearchCorpAds", RpcDispatchHandler::Sync(dispatch_rpc)),
    rpc_entry("corpRecProxy", "DeleteRecruitmentAd", RpcDispatchHandler::Sync(dispatch_rpc)),
    rpc_entry("corpRegistry", "AddCorporateContact", RpcDispatchHandler::Sync(dispatch_rpc)),
    rpc_entry("corpRegistry", "EditCorporateContact", RpcDispatchHandler::Sync(dispatch_rpc)),
    rpc_entry("corpRegistry", "EditContactsRelationshipID", RpcDispatchHandler::Sync(dispatch_rpc)),
    rpc_entry("corpRegistry", "DeleteLabel", RpcDispatchHandler::Sync(dispatch_rpc)),
    rpc_entry("corpRegistry", "AssignLabels", RpcDispatchHandler::Sync(dispatch_rpc)),
    rpc_entry("corpRegistry", "RemoveLabels", RpcDispatchHandler::Sync(dispatch_rpc)),
    rpc_entry("corpRegistry", "RemoveCorporateContacts", RpcDispatchHandler::Sync(dispatch_rpc)),
    rpc_entry("corpRegistry", "GetCorporateContacts", RpcDispatchHandler::Sync(dispatch_rpc)),
    rpc_entry("corpRegistry", "CreateLabel", RpcDispatchHandler::Sync(dispatch_rpc)),
    rpc_entry("corpRegistry", "EditLabel", RpcDispatchHandler::Sync(dispatch_rpc)),
    rpc_entry("corpRegistry", "GetLabels", RpcDispatchHandler::Sync(dispatch_rpc)),
];

#[cfg(test)]
#[path = "fleet_social_corp_gov_tests.rs"]
pub(crate) mod fleet_social_corp_gov_tests;

#[cfg(test)]
#[path = "fleet_social_corp_gov_edges_tests.rs"]
mod fleet_social_corp_gov_edges_tests;
