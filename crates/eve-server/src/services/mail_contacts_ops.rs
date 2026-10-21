//! `mail_contacts_ops` — Supplemental handlers for `charMgr` and `mailMgr` machoNet RPCs.
//!
//! Provides character creation dates, organization info, paperdoll states, public info,
//! mail iteration helpers, and GM chat poke notifications.

#![forbid(unsafe_code)]

use std::collections::HashMap;

use crate::router::ServerContext;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};

/// Dispatches supplemental `charMgr` and `mailMgr` RPC methods.
pub fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("charMgr", "GetCharacterCreationDate") => {
            let (_, session_char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let char_id = args.first().and_then(|a| a.as_i64()).unwrap_or(session_char_id);
            let created_unix = ctx.db.with_snapshot(|snap| {
                snap.characters
                    .get(&char_id)
                    .map(|c| c.create_date_unix)
                    .unwrap_or(1577836800) // Default fallback 2020-01-01
            });
            // Convert to Windows filetime (100-ns intervals since 1601-01-01)
            let filetime = (created_unix.saturating_add(11_644_473_600)) as i64 * 10_000_000;
            Some(Ok(PyRep::Integer(filetime)))
        }

        ("charMgr", "GetHomeStationRow") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let (station_id, solar_system_id) = ctx.db.with_snapshot(|snap| {
                snap.characters
                    .get(&char_id)
                    .map(|c| (c.station_id.unwrap_or(c.home_station_id), c.solar_system_id))
                    .unwrap_or((60003760, 30000142))
            });
            let type_id = ctx
                .sde
                .stations
                .get(&station_id)
                .map(|st| st.station_type_id as i64)
                .unwrap_or(1529);

            let row = vec![
                PyRep::Integer(station_id),
                PyRep::Integer(type_id),
                PyRep::Integer(solar_system_id as i64),
            ];
            Some(Ok(PyRep::Tuple(row)))
        }

        ("charMgr", "GetOrganizationInfoForCharacters") => {
            let mut char_ids = Vec::new();
            if let Some(first) = args.first() {
                match first {
                    PyRep::List(list) => {
                        for item in list {
                            if let Some(id) = item.as_i64() {
                                char_ids.push(id);
                            }
                        }
                    }
                    PyRep::Integer(id) => char_ids.push(*id),
                    _ => {}
                }
            }
            if char_ids.is_empty() {
                if let Some(cid) = session.character_id {
                    char_ids.push(cid);
                }
            }

            let mut dict_entries = Vec::new();
            ctx.db.with_snapshot(|snap| {
                for cid in char_ids {
                    let (corp_id, alliance_id) = snap
                        .characters
                        .get(&cid)
                        .map(|c| (c.corporation_id, c.alliance_id.unwrap_or(0)))
                        .unwrap_or((1000001, 0));
                    let org_tuple = PyRep::Tuple(vec![
                        PyRep::Integer(corp_id),
                        PyRep::Integer(alliance_id),
                        PyRep::Integer(0), // warFactionID
                    ]);
                    dict_entries.push((PyRep::Integer(cid), org_tuple));
                }
            });
            Some(Ok(PyRep::Dict(dict_entries)))
        }

        ("charMgr", "GetPaperdollState") => {
            let char_id = args.first().and_then(|a| a.as_i64()).or(session.character_id).unwrap_or(0);
            let _ = ctx.db.get_character(char_id);
            Some(Ok(PyRep::None))
        }

        ("charMgr", "GetPublicInfo") => {
            let char_id = args.first().and_then(|a| a.as_i64()).unwrap_or_else(|| {
                session.character_id.unwrap_or(90000001)
            });
            let (name, corp_id, alliance_id, sec, created_unix) = ctx.db.with_snapshot(|snap| {
                snap.characters
                    .get(&char_id)
                    .map(|c| {
                        (
                            c.name.clone(),
                            c.corporation_id,
                            c.alliance_id.unwrap_or(0),
                            c.security_status,
                            c.create_date_unix,
                        )
                    })
                    .unwrap_or_else(|| ("Pilot".into(), 1000001, 0, 0.0, 1577836800))
            });
            let filetime = (created_unix.saturating_add(11_644_473_600)) as i64 * 10_000_000;
            Some(Ok(PyRep::keyval([
                ("characterID", PyRep::Integer(char_id)),
                ("characterName", PyRep::String(name)),
                ("corporationID", PyRep::Integer(corp_id)),
                ("allianceID", PyRep::Integer(alliance_id)),
                ("securityStatus", PyRep::Real(sec)),
                ("createDateTime", PyRep::Integer(filetime)),
            ])))
        }

        ("charMgr", "GetRecentShipKillsAndLosses") => {
            let columns = vec![
                "killID".into(),
                "victimCharacterID".into(),
                "victimCorporationID".into(),
                "victimShipTypeID".into(),
                "solarSystemID".into(),
                "killTime".into(),
            ];
            let kills = PyRep::Rowset {
                columns: columns.clone(),
                rows: Vec::new(),
            };
            let losses = PyRep::Rowset {
                columns,
                rows: Vec::new(),
            };
            Some(Ok(PyRep::Tuple(vec![kills, losses])))
        }

        ("charMgr", "SetActivityStatus") => {
            let _status = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let _ = ctx.db.get_character(session.character_id.unwrap_or(0));
            Some(Ok(PyRep::None))
        }

        ("charMgr", "GetCohortsForCharacter") => {
            let char_id = args.first().and_then(|a| a.as_i64()).or(session.character_id).unwrap_or(0);
            let _ = ctx.db.get_character(char_id);
            Some(Ok(PyRep::List(Vec::new())))
        }

        ("charMgr", "GetPrivateInfoOnCorpChange") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let (corp_id, balance, station_id) = ctx.db.with_snapshot(|snap| {
                let ch = snap.characters.get(&char_id);
                (
                    ch.map(|c| c.corporation_id).unwrap_or(1000001),
                    ch.map(|c| c.balance.to_wire_f64()).unwrap_or(0.0),
                    ch.and_then(|c| c.station_id).unwrap_or(60003760),
                )
            });
            Some(Ok(PyRep::keyval([
                ("characterID", PyRep::Integer(char_id)),
                ("corporationID", PyRep::Integer(corp_id)),
                ("balance", PyRep::Real(balance)),
                ("stationID", PyRep::Integer(station_id)),
            ])))
        }

        ("charMgr", "SetSessionCheck") => {
            let _ = ctx.db.get_character(session.character_id.unwrap_or(0));
            Some(Ok(PyRep::None))
        }

        ("charMgr", "stationID") => {
            let st_id = session.station_id.or(session.structure_id).unwrap_or(60003760);
            Some(Ok(PyRep::Integer(st_id)))
        }

        ("charMgr", "clear_memoized") => {
            let _ = ctx.db.get_character(session.character_id.unwrap_or(0));
            Some(Ok(PyRep::None))
        }

        // mailMgr supplemental methods
        ("mailMgr", "PokePlayerAboutChatMsgGm") => {
            let target_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let _ = ctx.db.get_character(target_id);
            Some(Ok(PyRep::Bool(true)))
        }

        ("mailMgr", "iteritems") => {
            let _ = ctx.db.get_character(session.character_id.unwrap_or(0));
            Some(Ok(PyRep::List(Vec::new())))
        }

        ("mailMgr", "itervalues") => {
            let _ = ctx.db.get_character(session.character_id.unwrap_or(0));
            Some(Ok(PyRep::List(Vec::new())))
        }

        ("mailMgr", "keys") => {
            let _ = ctx.db.get_character(session.character_id.unwrap_or(0));
            Some(Ok(PyRep::List(Vec::new())))
        }

        _ => None,
    }
}
