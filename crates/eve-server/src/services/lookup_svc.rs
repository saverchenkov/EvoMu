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

pub fn handle(
    ctx: &ServerContext,
    session: &SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    if service != "lookupSvc" && service != "bannedwords" {
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
            let mut results = Vec::new();

            ctx.db.with_snapshot(|snap| {
                // Player corporations (group 2)
                for corp in snap.corporations.values() {
                    if corp.corporation_id >= 98_000_000
                        && (matches_query(&corp.name, &query, exact)
                            || matches_query(&corp.ticker, &query, exact))
                    {
                        results.push(PyRep::keyval([
                            ("ownerID", PyRep::Integer(corp.corporation_id)),
                            ("ownerName", PyRep::String(corp.name.clone())),
                            ("groupID", PyRep::Integer(2)),
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
                        ]));
                    }
                }
            });

            Some(Ok(PyRep::List(results)))
        }

        ("bannedwords", "fetch_regexes") => {
            let regexes = vec![
                PyRep::String(r"(?i)\b(?:badword|bannedword|exploit|scam|hack|profanity)\b".into()),
            ];
            Some(Ok(PyRep::List(regexes)))
        }

        ("bannedwords", "check_words_allowed") => {
            let banned = ["badword", "bannedword", "exploit", "scam", "hack", "profanity"];
            let is_allowed = |s: &str| {
                let lower = s.to_lowercase();
                !banned.iter().any(|b| lower.contains(b))
            };
            let target_arg = args.first().or_else(|| _kwargs.get("words"));
            match target_arg {
                Some(PyRep::List(words)) | Some(PyRep::Tuple(words)) => {
                    let results: Vec<PyRep> = words
                        .iter()
                        .map(|w| PyRep::Bool(is_allowed(w.as_str().unwrap_or(""))))
                        .collect();
                    Some(Ok(PyRep::List(results)))
                }
                Some(PyRep::String(s)) => Some(Ok(PyRep::Bool(is_allowed(s)))),
                _ => Some(Ok(PyRep::Bool(true))),
            }
        }

        ("bannedwords", "check_search_words_allowed") => {
            let banned = ["badword", "bannedword", "exploit", "scam", "hack", "profanity"];
            let is_allowed = |s: &str| {
                let lower = s.to_lowercase();
                !banned.iter().any(|b| lower.contains(b))
            };
            let target_arg = args.first().or_else(|| _kwargs.get("words"));
            match target_arg {
                Some(PyRep::List(words)) | Some(PyRep::Tuple(words)) => {
                    let all_ok = words
                        .iter()
                        .all(|w| is_allowed(w.as_str().unwrap_or("")));
                    Some(Ok(PyRep::Bool(all_ok)))
                }
                Some(PyRep::String(s)) => Some(Ok(PyRep::Bool(is_allowed(s)))),
                _ => Some(Ok(PyRep::Bool(true))),
            }
        }

        _ => None,
    }
}
