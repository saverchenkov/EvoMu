//! `lookup_svc` — Character, corporation, faction, owner, and location search (`lookupSvc`).
//!
//! Implements V24.01 candidate methods for search dialogs, slash commands, and war eligibility lookups.

#![forbid(unsafe_code)]

use std::collections::HashMap;

use crate::router::ServerContext;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};

fn matches_query(text: &str, query: &str, exact: bool) -> bool {
    if exact {
        text.eq_ignore_ascii_case(query)
    } else {
        text.to_ascii_lowercase().contains(&query.to_ascii_lowercase())
    }
}

fn parse_search_args(args: &[PyRep]) -> (String, bool) {
    let query = args
        .first()
        .and_then(|a| a.as_str())
        .unwrap_or("")
        .to_string();
    let exact = args
        .get(1)
        .map(|a| a.as_bool() == Some(true) || a.as_i64() == Some(1))
        .unwrap_or(false);
    (query, exact)
}

const DEFAULT_BANNED_WORDS: [&str; 6] = [
    "badword",
    "bannedword",
    "exploit",
    "scam",
    "hack",
    "profanity",
];

fn load_banned_words(ctx: &ServerContext) -> Vec<String> {
    let mut words: Vec<String> = DEFAULT_BANNED_WORDS.iter().map(|w| (*w).to_string()).collect();
    ctx.db.with_snapshot(|snap| {
        if let Some(extra) = snap.service_state_blobs.get("bannedwords:words") {
            for token in extra
                .split(|c: char| c == ',' || c == '\n' || c == ';')
                .map(|t| t.trim().to_ascii_lowercase())
                .filter(|t| !t.is_empty())
            {
                if !words.contains(&token) {
                    words.push(token);
                }
            }
        }
    });
    words
}

fn load_banned_regexes(ctx: &ServerContext) -> Vec<String> {
    let words = load_banned_words(ctx);
    let mut regexes = vec![format!(r"(?i)\b(?:{})\b", words.join("|"))];
    ctx.db.with_snapshot(|snap| {
        if let Some(extra) = snap.service_state_blobs.get("bannedwords:regexes") {
            for pat in extra
                .lines()
                .map(str::trim)
                .filter(|p| !p.is_empty())
            {
                if !regexes.iter().any(|r| r == pat) {
                    regexes.push(pat.to_string());
                }
            }
        }
    });
    regexes
}

pub fn is_word_allowed(s: &str) -> bool {
    let lower = s.to_lowercase();
    for b in &DEFAULT_BANNED_WORDS {
        if lower.contains(b) {
            return false;
        }
    }
    true
}

fn is_word_allowed_for_ctx(ctx: &ServerContext, s: &str) -> bool {
    let lower = s.to_lowercase();
    for b in load_banned_words(ctx) {
        if lower.contains(&b) {
            return false;
        }
    }
    true
}

fn handle_bannedwords_fetch_regexes(ctx: &ServerContext) -> Result<PyRep, BridgeRpcError> {
    let regexes: Vec<PyRep> = load_banned_regexes(ctx).into_iter().map(PyRep::String).collect();
    Ok(PyRep::List(regexes))
}

fn handle_bannedwords_check_words_allowed(
    ctx: &ServerContext,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    let target_arg = args
        .first()
        .or_else(|| kwargs.get("words"))
        .or_else(|| kwargs.get("word"));
    match target_arg {
        Some(PyRep::List(words)) | Some(PyRep::Tuple(words)) => {
            let results: Vec<PyRep> = words
                .iter()
                .map(|w| PyRep::Bool(is_word_allowed_for_ctx(ctx, w.as_str().unwrap_or(""))))
                .collect();
            Ok(PyRep::List(results))
        }
        Some(PyRep::String(s)) => {
            if is_word_allowed_for_ctx(ctx, s) {
                Ok(PyRep::None)
            } else {
                Err(BridgeRpcError {
                    code: "UserError".into(),
                    message: "LocationNameInvalidBannedWord".into(),
                })
            }
        }
        // DERIVED: V24 check_words_allowed returns None when given null or empty input.
        _ => Ok(PyRep::None),
    }
}

fn handle_bannedwords_check_chat_words_allowed(
    ctx: &ServerContext,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    let target_arg = args
        .first()
        .or_else(|| kwargs.get("words"))
        .or_else(|| kwargs.get("text"))
        .or_else(|| kwargs.get("word"));
    let is_allowed = match target_arg {
        Some(PyRep::List(words)) | Some(PyRep::Tuple(words)) => words
            .iter()
            .all(|w| is_word_allowed_for_ctx(ctx, w.as_str().unwrap_or(""))),
        Some(PyRep::String(s)) => is_word_allowed_for_ctx(ctx, s),
        _ => true,
    };
    if is_allowed {
        Ok(PyRep::None)
    } else {
        Err(BridgeRpcError {
            code: "UserError".into(),
            message: "ChatContentInvalidBannedWords".into(),
        })
    }
}

fn handle_bannedwords_check_search_words_allowed(
    ctx: &ServerContext,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    let target_arg = args
        .first()
        .or_else(|| kwargs.get("words"))
        .or_else(|| kwargs.get("searchStr"));
    let all_ok = match target_arg {
        Some(PyRep::List(words)) | Some(PyRep::Tuple(words)) => words
            .iter()
            .all(|w| is_word_allowed_for_ctx(ctx, w.as_str().unwrap_or(""))),
        Some(PyRep::String(s)) => is_word_allowed_for_ctx(ctx, s),
        _ => true,
    };
    Ok(PyRep::Bool(all_ok))
}

pub fn handle(
    ctx: &ServerContext,
    session: &SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    if service != "lookupSvc" && service != "bannedwords" && service != "search" {
        return None;
    }
    if let Err(e) = ServerContext::require_auth(session) {
        return Some(Err(e));
    }

    match (service, method) {
        ("lookupSvc", "LookupCharacters") => {
            let (query, exact) = parse_search_args(args);
            let results = ctx.db.with_snapshot(|snap| {
                snap.characters
                    .values()
                    .filter(|c| matches_query(&c.name, &query, exact))
                    .map(|c| {
                        PyRep::keyval([
                            ("characterID", PyRep::Integer(c.character_id)),
                            ("characterName", PyRep::String(c.name.clone())),
                            ("corporationID", PyRep::Integer(c.corporation_id)),
                            (
                                "allianceID",
                                c.alliance_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                            ),
                        ])
                    })
                    .collect::<Vec<PyRep>>()
            });
            Some(Ok(PyRep::List(results)))
        }

        ("lookupSvc", "LookupEvePlayerCharacters") => {
            let (query, exact) = parse_search_args(args);
            let results = ctx.db.with_snapshot(|snap| {
                snap.characters
                    .values()
                    .filter(|c| c.character_id >= 90_000_000 && matches_query(&c.name, &query, exact))
                    .map(|c| {
                        PyRep::keyval([
                            ("characterID", PyRep::Integer(c.character_id)),
                            ("characterName", PyRep::String(c.name.clone())),
                            ("corporationID", PyRep::Integer(c.corporation_id)),
                            (
                                "allianceID",
                                c.alliance_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                            ),
                        ])
                    })
                    .collect::<Vec<PyRep>>()
            });
            Some(Ok(PyRep::List(results)))
        }

        ("lookupSvc", "LookupCorporations") => {
            let query_str = args.first().and_then(|a| a.as_str());
            let from_id = args.first().and_then(|a| a.as_i64());
            let mut results = Vec::new();

            // Search DB player corporations
            ctx.db.with_snapshot(|snap| {
                for corp in snap.corporations.values() {
                    let matches = match (query_str, from_id) {
                        (Some(q), _) => {
                            matches_query(&corp.name, q, false)
                                || matches_query(&corp.ticker, q, false)
                        }
                        (None, Some(id)) => id == 0 || corp.corporation_id == id,
                        (None, None) => true,
                    };
                    if matches {
                        results.push(PyRep::keyval([
                            ("corporationID", PyRep::Integer(corp.corporation_id)),
                            ("corporationName", PyRep::String(corp.name.clone())),
                            ("tickerName", PyRep::String(corp.ticker.clone())),
                            ("corporationType", PyRep::Integer(2)),
                        ]));
                    }
                }
            });

            // Also search SDE NPC corporations
            for npc in ctx.sde.npc_corporations.values() {
                let matches = match (query_str, from_id) {
                    (Some(q), _) => {
                        matches_query(&npc.name, q, false)
                            || matches_query(&npc.ticker_name, q, false)
                    }
                    (None, Some(id)) => id == 0 || npc.corporation_id == id,
                    (None, None) => true,
                };
                if matches {
                    results.push(PyRep::keyval([
                        ("corporationID", PyRep::Integer(npc.corporation_id)),
                        ("corporationName", PyRep::String(npc.name.clone())),
                        ("tickerName", PyRep::String(npc.ticker_name.clone())),
                        ("corporationType", PyRep::Integer(1)),
                    ]));
                }
            }

            Some(Ok(PyRep::List(results)))
        }

        ("lookupSvc", "LookupFactions") => {
            let query_str = args.first().and_then(|a| a.as_str());
            let mut factions: Vec<_> = ctx
                .sde
                .factions
                .values()
                .filter(|f| query_str.map(|q| matches_query(&f.name, q, false)).unwrap_or(true))
                .collect();
            factions.sort_by_key(|f| f.faction_id);
            let results = factions
                .into_iter()
                .map(|f| {
                    PyRep::keyval([
                        ("factionID", PyRep::Integer(i64::from(f.faction_id))),
                        ("factionName", PyRep::String(f.name.clone())),
                    ])
                })
                .collect();
            Some(Ok(PyRep::List(results)))
        }

        ("lookupSvc", "LookupKnownLocationsByGroup") => {
            let group_id = args.first().and_then(|a| a.as_i64()).unwrap_or(5);
            let query = args.get(1).and_then(|a| a.as_str()).unwrap_or("");
            let exact = args
                .get(2)
                .map(|a| a.as_bool() == Some(true) || a.as_i64() == Some(1))
                .unwrap_or(false);

            let mut results = Vec::new();
            if group_id == 5 {
                // Solar systems
                let mut systems: Vec<_> = ctx
                    .sde
                    .solar_systems
                    .values()
                    .filter(|sys| matches_query(&sys.name, query, exact))
                    .collect();
                systems.sort_by_key(|s| s.solar_system_id);
                for sys in systems {
                    results.push(PyRep::keyval([
                        ("itemID", PyRep::Integer(i64::from(sys.solar_system_id))),
                        ("itemName", PyRep::String(sys.name.clone())),
                        ("typeID", PyRep::Integer(5)),
                        ("groupID", PyRep::Integer(5)),
                    ]));
                }
            } else if group_id == 15 {
                // Stations
                let mut stations: Vec<_> = ctx
                    .sde
                    .stations
                    .values()
                    .filter(|st| matches_query(&st.name, query, exact))
                    .collect();
                stations.sort_by_key(|st| st.station_id);
                for st in stations {
                    results.push(PyRep::keyval([
                        ("itemID", PyRep::Integer(st.station_id)),
                        ("itemName", PyRep::String(st.name.clone())),
                        ("typeID", PyRep::Integer(i64::from(st.station_type_id))),
                        ("groupID", PyRep::Integer(15)),
                    ]));
                }
            } else if group_id == 4 {
                // Constellations
                let mut constellations: Vec<_> = ctx
                    .sde
                    .constellations
                    .values()
                    .filter(|c| matches_query(&c.name, query, exact))
                    .collect();
                constellations.sort_by_key(|c| c.constellation_id);
                for c in constellations {
                    results.push(PyRep::keyval([
                        ("itemID", PyRep::Integer(i64::from(c.constellation_id))),
                        ("itemName", PyRep::String(c.name.clone())),
                        ("typeID", PyRep::Integer(4)),
                        ("groupID", PyRep::Integer(4)),
                    ]));
                }
            } else if group_id == 3 {
                // Regions
                let mut regions: Vec<_> = ctx
                    .sde
                    .regions
                    .values()
                    .filter(|r| matches_query(&r.name, query, exact))
                    .collect();
                regions.sort_by_key(|r| r.region_id);
                for r in regions {
                    results.push(PyRep::keyval([
                        ("itemID", PyRep::Integer(i64::from(r.region_id))),
                        ("itemName", PyRep::String(r.name.clone())),
                        ("typeID", PyRep::Integer(3)),
                        ("groupID", PyRep::Integer(3)),
                    ]));
                }
            }
            Some(Ok(PyRep::List(results)))
        }

        ("lookupSvc", "LookupLocationsByGroup") => {
            let group_id = args.first().and_then(|a| a.as_i64()).unwrap_or(5);
            let query = args.get(1).and_then(|a| a.as_str()).unwrap_or("");
            let exact = args
                .get(2)
                .map(|a| a.as_bool() == Some(true) || a.as_i64() == Some(1))
                .unwrap_or(false);

            let mut results = Vec::new();
            if group_id == 15 {
                let mut stations: Vec<_> = ctx
                    .sde
                    .stations
                    .values()
                    .filter(|st| matches_query(&st.name, query, exact))
                    .collect();
                stations.sort_by_key(|st| st.station_id);
                for st in stations {
                    results.push(PyRep::keyval([
                        ("itemID", PyRep::Integer(st.station_id)),
                        ("itemName", PyRep::String(st.name.clone())),
                        ("typeID", PyRep::Integer(i64::from(st.station_type_id))),
                        ("groupID", PyRep::Integer(15)),
                    ]));
                }
            } else {
                let mut systems: Vec<_> = ctx
                    .sde
                    .solar_systems
                    .values()
                    .filter(|sys| matches_query(&sys.name, query, exact))
                    .collect();
                systems.sort_by_key(|s| s.solar_system_id);
                for sys in systems {
                    results.push(PyRep::keyval([
                        ("itemID", PyRep::Integer(i64::from(sys.solar_system_id))),
                        ("itemName", PyRep::String(sys.name.clone())),
                        ("typeID", PyRep::Integer(5)),
                        ("groupID", PyRep::Integer(5)),
                    ]));
                }
            }
            Some(Ok(PyRep::List(results)))
        }

        ("lookupSvc", "LookupNoneNPCAccountOwners") => {
            let (query, exact) = parse_search_args(args);
            let mut results = Vec::new();

            // Player characters (group 1)
            ctx.db.with_snapshot(|snap| {
                for c in snap.characters.values() {
                    if c.character_id >= 90_000_000 && matches_query(&c.name, &query, exact) {
                        results.push(PyRep::keyval([
                            ("ownerID", PyRep::Integer(c.character_id)),
                            ("ownerName", PyRep::String(c.name.clone())),
                            ("groupID", PyRep::Integer(1)),
                        ]));
                    }
                }
                // Player corporations (group 2)
                for corp in snap.corporations.values() {
                    if corp.corporation_id >= 98_000_000 && matches_query(&corp.name, &query, exact) {
                        results.push(PyRep::keyval([
                            ("ownerID", PyRep::Integer(corp.corporation_id)),
                            ("ownerName", PyRep::String(corp.name.clone())),
                            ("groupID", PyRep::Integer(2)),
                        ]));
                    }
                }
            });

            Some(Ok(PyRep::List(results)))
        }

        ("lookupSvc", "LookupOwners") => {
            let (query, exact) = parse_search_args(args);
            let mut results = Vec::new();

            // Characters (group 1)
            ctx.db.with_snapshot(|snap| {
                for c in snap.characters.values() {
                    if matches_query(&c.name, &query, exact) {
                        results.push(PyRep::keyval([
                            ("ownerID", PyRep::Integer(c.character_id)),
                            ("ownerName", PyRep::String(c.name.clone())),
                            ("groupID", PyRep::Integer(1)),
                            ("typeID", PyRep::Integer(1373)),
                        ]));
                    }
                }
                // Corporations (group 2)
                for corp in snap.corporations.values() {
                    if matches_query(&corp.name, &query, exact)
                        || matches_query(&corp.ticker, &query, exact)
                    {
                        results.push(PyRep::keyval([
                            ("ownerID", PyRep::Integer(corp.corporation_id)),
                            ("ownerName", PyRep::String(corp.name.clone())),
                            ("groupID", PyRep::Integer(2)),
                            ("typeID", PyRep::Integer(2)),
                        ]));
                    }
                }
                // Alliances (group 32)
                for al in snap.alliances.values() {
                    if matches_query(&al.name, &query, exact)
                        || matches_query(&al.short_name, &query, exact)
                    {
                        results.push(PyRep::keyval([
                            ("ownerID", PyRep::Integer(al.alliance_id)),
                            ("ownerName", PyRep::String(al.name.clone())),
                            ("groupID", PyRep::Integer(32)),
                            ("typeID", PyRep::Integer(16159)),
                        ]));
                    }
                }
            });

            // SDE NPC corporations
            for npc in ctx.sde.npc_corporations.values() {
                if matches_query(&npc.name, &query, exact)
                    || matches_query(&npc.ticker_name, &query, exact)
                {
                    results.push(PyRep::keyval([
                        ("ownerID", PyRep::Integer(npc.corporation_id)),
                        ("ownerName", PyRep::String(npc.name.clone())),
                        ("groupID", PyRep::Integer(2)),
                        ("typeID", PyRep::Integer(2)),
                    ]));
                }
            }

            Some(Ok(PyRep::List(results)))
        }

        ("lookupSvc", "LookupPCOwners") => {
            let (query, exact) = parse_search_args(args);
            let mut results = Vec::new();

            ctx.db.with_snapshot(|snap| {
                // Player characters (group 1)
                for c in snap.characters.values() {
                    if c.character_id >= 90_000_000 && matches_query(&c.name, &query, exact) {
                        results.push(PyRep::keyval([
                            ("ownerID", PyRep::Integer(c.character_id)),
                            ("ownerName", PyRep::String(c.name.clone())),
                            ("groupID", PyRep::Integer(1)),
                        ]));
                    }
                }
                // Player corporations (group 2)
                for corp in snap.corporations.values() {
                    if corp.corporation_id >= 98_000_000 && matches_query(&corp.name, &query, exact) {
                        results.push(PyRep::keyval([
                            ("ownerID", PyRep::Integer(corp.corporation_id)),
                            ("ownerName", PyRep::String(corp.name.clone())),
                            ("groupID", PyRep::Integer(2)),
                        ]));
                    }
                }
            });

            Some(Ok(PyRep::List(results)))
        }

        ("lookupSvc", "LookupWarableCorporationsOrAlliances") => {
            let (query, exact) = parse_search_args(args);
            let caller_corp_id = session.corporation_id;
            let caller_alliance_id = session.alliance_id;
            let mut results = Vec::new();

            ctx.db.with_snapshot(|snap| {
                let effective_caller_alliance = caller_alliance_id.or_else(|| {
                    caller_corp_id
                        .and_then(|cid| snap.corporations.get(&cid))
                        .and_then(|c| c.alliance_id)
                });
                let is_disqualified = |target_id: i64| -> bool {
                    let caller_matches = |id: i64| {
                        id > 0
                            && (Some(id) == caller_corp_id
                                || Some(id) == effective_caller_alliance)
                    };
                    snap.wars.values().any(|w| {
                        w.active
                            && ((caller_matches(w.aggressor_corp_id)
                                && w.defender_corp_id == target_id)
                                || (w.aggressor_corp_id == target_id
                                    && caller_matches(w.defender_corp_id)))
                    })
                };
                let is_corp_war_eligible = |corp_id: i64| -> bool {
                    snap.structures
                        .values()
                        .any(|s| s.owner_corp_id == corp_id && s.state != "Destroyed")
                };

                // Standalone player corporations (group 2): must be war-eligible (own >= 1 non-destroyed Upwell structure),
                // not in an alliance, and not the caller's own corporation.
                for corp in snap.corporations.values() {
                    if corp.corporation_id >= 98_000_000
                        && Some(corp.corporation_id) != caller_corp_id
                        && corp.alliance_id.is_none()
                        && (effective_caller_alliance.is_none()
                            || corp.alliance_id != effective_caller_alliance)
                        && is_corp_war_eligible(corp.corporation_id)
                        && !is_disqualified(corp.corporation_id)
                        && (matches_query(&corp.name, &query, exact)
                            || matches_query(&corp.ticker, &query, exact))
                    {
                        results.push((
                            corp.corporation_id,
                            PyRep::keyval([
                                ("ownerID", PyRep::Integer(corp.corporation_id)),
                                ("ownerName", PyRep::String(corp.name.clone())),
                                ("groupID", PyRep::Integer(2)),
                            ]),
                        ));
                    }
                }
                // Alliances (group 32) with at least one war-eligible member corporation,
                // excluding caller's own alliance.
                for al in snap.alliances.values() {
                    let al_war_eligible = al
                        .member_corp_ids
                        .iter()
                        .any(|&cid| is_corp_war_eligible(cid))
                        || snap.corporations.values().any(|c| {
                            c.alliance_id == Some(al.alliance_id)
                                && is_corp_war_eligible(c.corporation_id)
                        });
                    if al.alliance_id >= 99_000_000
                        && Some(al.alliance_id) != effective_caller_alliance
                        && al_war_eligible
                        && !is_disqualified(al.alliance_id)
                        && (matches_query(&al.name, &query, exact)
                            || matches_query(&al.short_name, &query, exact))
                    {
                        results.push((
                            al.alliance_id,
                            PyRep::keyval([
                                ("ownerID", PyRep::Integer(al.alliance_id)),
                                ("ownerName", PyRep::String(al.name.clone())),
                                ("groupID", PyRep::Integer(32)),
                            ]),
                        ));
                    }
                }
            });

            results.sort_by_key(|(id, _)| *id);
            Some(Ok(PyRep::List(results.into_iter().map(|(_, rep)| rep).collect())))
        }

        ("bannedwords", "fetch_regexes") => Some(handle_bannedwords_fetch_regexes(ctx)),
        ("bannedwords", "check_words_allowed") => {
            Some(handle_bannedwords_check_words_allowed(ctx, args, kwargs))
        }
        ("bannedwords", "check_chat_words_allowed") => {
            Some(handle_bannedwords_check_chat_words_allowed(ctx, args, kwargs))
        }
        ("bannedwords", "check_search_words_allowed") => {
            Some(handle_bannedwords_check_search_words_allowed(ctx, args, kwargs))
        }

        ("search", "Query") => Some(handle_search_query(ctx, session, args, kwargs)),
        ("search", "QuickQuery") => Some(handle_search_quick_query(ctx, session, args, kwargs)),

        _ => None,
    }
}

fn flag_arg(v: Option<&PyRep>) -> bool {
    v.is_some_and(|r| r.as_bool() == Some(true) || r.as_i64().unwrap_or(0) != 0)
}

fn parse_search_query_params(
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
    is_quick: bool,
) -> Result<(String, Vec<i64>, bool, bool, bool), BridgeRpcError> {
    let query = args
        .first()
        .and_then(|a| a.as_str())
        .or_else(|| kwargs.get("searchStr").and_then(|a| a.as_str()))
        .unwrap_or("")
        .trim()
        .to_string();
    if query.is_empty() {
        return Err(BridgeRpcError {
            code: "SearchStringTooShort".into(),
            message: "Search string must contain at least 1 non-whitespace character".into(),
        });
    }
    if !is_word_allowed(&query) {
        return Err(BridgeRpcError {
            code: "UserError".into(),
            message: "LocationNameInvalidBannedWord".into(),
        });
    }
    let groups: Vec<i64> = match args.get(1).or_else(|| kwargs.get("groupIDList")) {
        Some(PyRep::List(list) | PyRep::Tuple(list)) => {
            list.iter().filter_map(|v| v.as_i64()).collect()
        }
        Some(PyRep::Integer(g)) => vec![*g],
        _ => vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11],
    };
    let groups = if groups.is_empty() {
        vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11]
    } else {
        groups
    };
    let exact = kwargs
        .get("exact")
        .map(|v| flag_arg(Some(v)))
        .unwrap_or_else(|| if is_quick { flag_arg(args.get(4)) } else { flag_arg(args.get(2)) });
    let hide_npc = kwargs
        .get("hideNPC")
        .map(|v| flag_arg(Some(v)))
        .unwrap_or_else(|| if is_quick { flag_arg(args.get(2)) } else { flag_arg(args.get(3)) });
    let only_alt = kwargs
        .get("onlyAltName")
        .map(|v| flag_arg(Some(v)))
        .unwrap_or_else(|| if is_quick { flag_arg(args.get(3)) } else { flag_arg(args.get(4)) });
    Ok((query, groups, exact, hide_npc, only_alt))
}

fn search_ids_for_group(
    ctx: &ServerContext,
    caller_char_id: Option<i64>,
    group_id: i64,
    query: &str,
    exact: bool,
    hide_npc: bool,
    only_alt: bool,
) -> Vec<i64> {
    let mut ids: Vec<i64> = match group_id {
        1 => {
            if hide_npc {
                Vec::new()
            } else {
                ctx.sde
                    .agents
                    .values()
                    .filter(|a| matches_query(&a.name, query, exact))
                    .map(|a| a.agent_id)
                    .collect()
            }
        }
        2 => ctx.db.with_snapshot(|snap| {
            snap.characters
                .values()
                .filter(|c| (!hide_npc || c.character_id >= 90_000_000) && matches_query(&c.name, query, exact))
                .map(|c| c.character_id)
                .collect()
        }),
        3 => {
            let mut out = ctx.db.with_snapshot(|snap| {
                snap.corporations
                    .values()
                    .filter(|c| {
                        if only_alt {
                            matches_query(&c.ticker, query, exact)
                        } else {
                            matches_query(&c.name, query, exact) || matches_query(&c.ticker, query, exact)
                        }
                    })
                    .map(|c| c.corporation_id)
                    .collect::<Vec<_>>()
            });
            if !hide_npc {
                for npc in ctx.sde.npc_corporations.values() {
                    let ok = if only_alt {
                        matches_query(&npc.ticker_name, query, exact)
                    } else {
                        matches_query(&npc.name, query, exact) || matches_query(&npc.ticker_name, query, exact)
                    };
                    if ok {
                        out.push(npc.corporation_id);
                    }
                }
            }
            out
        }
        4 | 32 => ctx.db.with_snapshot(|snap| {
            snap.alliances
                .values()
                .filter(|a| {
                    if only_alt {
                        matches_query(&a.short_name, query, exact)
                    } else {
                        matches_query(&a.name, query, exact) || matches_query(&a.short_name, query, exact)
                    }
                })
                .map(|a| a.alliance_id)
                .collect()
        }),
        5 | 19 => {
            if hide_npc {
                Vec::new()
            } else {
                ctx.sde
                    .factions
                    .values()
                    .filter(|f| matches_query(&f.name, query, exact))
                    .map(|f| i64::from(f.faction_id))
                    .collect()
            }
        }
        6 => ctx
            .sde
            .constellations
            .values()
            .filter(|c| matches_query(&c.name, query, exact))
            .map(|c| i64::from(c.constellation_id))
            .collect(),
        7 => ctx
            .sde
            .solar_systems
            .values()
            .filter(|s| matches_query(&s.name, query, exact))
            .map(|s| i64::from(s.solar_system_id))
            .collect(),
        8 => ctx
            .sde
            .regions
            .values()
            .filter(|r| matches_query(&r.name, query, exact))
            .map(|r| i64::from(r.region_id))
            .collect(),
        9 | 15 => ctx
            .sde
            .stations
            .values()
            .filter(|st| matches_query(&st.name, query, exact))
            .map(|st| st.station_id)
            .collect(),
        10 => ctx
            .sde
            .types
            .values()
            .filter(|t| t.published && matches_query(&t.name, query, exact))
            .map(|t| i64::from(t.type_id))
            .collect(),
        11 | 1404 | 1657 => {
            let Some(char_id) = caller_char_id else {
                return Vec::new();
            };
            ctx.db.with_snapshot(|snap| {
                snap.structures
                    .values()
                    .filter(|st| {
                        st.state != "Destroyed"
                            && matches_query(&st.name, query, exact)
                            && crate::services::station_structure::structure_upwell_ops::evaluate_character_structure_access_in_snap(
                                snap,
                                st.structure_id,
                                char_id,
                            )
                    })
                    .map(|st| st.structure_id)
                    .collect()
            })
        }
        _ => Vec::new(),
    };
    ids.sort_unstable();
    ids.dedup();
    ids.truncate(500);
    ids
}

/// `search.Query(searchStr, groupIDList, exact=0, hideNPC=0, onlyAltName=0)` (`searchUtil.pyj:306`).
pub fn handle_search_query(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let (query, groups, exact, hide_npc, only_alt) = parse_search_query_params(args, kwargs, false)?;
    let mut dict_entries = Vec::with_capacity(groups.len());
    for gid in groups {
        let ids = search_ids_for_group(ctx, Some(char_id), gid, &query, exact, hide_npc, only_alt);
        dict_entries.push((
            PyRep::Integer(gid),
            PyRep::List(ids.into_iter().map(PyRep::Integer).collect()),
        ));
    }
    Ok(PyRep::Dict(dict_entries))
}

/// `search.QuickQuery(searchStr, groupIDList, hideNPC=0, onlyAltName=0, exact=0)` (`searchUtil.pyj:327`).
pub fn handle_search_quick_query(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let (query, groups, exact, hide_npc, only_alt) = parse_search_query_params(args, kwargs, true)?;
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for gid in groups {
        for id in search_ids_for_group(ctx, Some(char_id), gid, &query, exact, hide_npc, only_alt) {
            if seen.insert(id) {
                out.push(PyRep::Integer(id));
            }
        }
    }
    Ok(PyRep::List(out))
}

#[cfg(test)]
#[path = "lookup_svc_tests.rs"]
mod lookup_svc_tests;

