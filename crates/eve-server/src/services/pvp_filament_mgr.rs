//! `pvp_filament_mgr` — Proving Grounds / Abyssal PvP Filament manager (`pvpFilamentMgr`).
//!
//! Handles matchmaking queue registration, active/scheduled Proving Grounds events,
//! leaderboards, character statistics, and Abyssal end-gate extraction.

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use crate::router::ServerContext;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};

#[derive(Debug, Clone, Default)]
pub struct PvpFilamentRegistry {
    /// Queued pilots: characterID -> (itemID, typeID, joined_unix)
    pub queue: HashMap<i64, (i64, i64, u64)>,
    /// Match statistics: characterID -> (wins, losses, score)
    pub stats: HashMap<i64, (i64, i64, i64)>,
}

pub static PVP_FILAMENT_STATE: LazyLock<Mutex<HashMap<u64, PvpFilamentRegistry>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn with_registry<F, R>(ctx: &ServerContext, f: F) -> R
where
    F: FnOnce(&mut PvpFilamentRegistry) -> R,
{
    let mut guard = PVP_FILAMENT_STATE.lock().unwrap();
    let reg = guard.entry(ctx.instance_id).or_default();
    f(reg)
}

pub fn handle(
    ctx: &ServerContext,
    session: &SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    if service != "pvpFilamentMgr" {
        return None;
    }
    let (_, char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };

    let now_unix = ctx.now_unix();

    match (service, method) {
        ("pvpFilamentMgr", "JoinPVPQueue") => {
            let item_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let type_id = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0);

            with_registry(ctx, |reg| {
                reg.queue.insert(char_id, (item_id, type_id, now_unix));
                reg.stats.entry(char_id).or_insert((0, 0, 0));
            });

            Some(Ok(PyRep::Bool(true)))
        }

        ("pvpFilamentMgr", "LeavePVPQueue") => {
            with_registry(ctx, |reg| {
                reg.queue.remove(&char_id);
            });
            Some(Ok(PyRep::None))
        }

        ("pvpFilamentMgr", "GetAllEvents") => {
            let start = (now_unix.saturating_sub(86400)) as i64;
            let end = (now_unix + 86400 * 7) as i64;
            let event = PyRep::keyval([
                ("eventID", PyRep::Integer(1)),
                ("name", PyRep::String("1v1 Cruiser Proving Grounds".into())),
                (
                    "description",
                    PyRep::String("Proving Grounds combat arena".into()),
                ),
                ("matchTypeID", PyRep::Integer(1)),
                ("scheduleID", PyRep::Integer(1001)),
                ("startDate", PyRep::Integer(start)),
                ("endDate", PyRep::Integer(end)),
                ("typeIDs", PyRep::List(vec![PyRep::Integer(56000)])),
            ]);
            let mut dict = Vec::new();
            dict.push((PyRep::Integer(1), event));
            Some(Ok(PyRep::Dict(dict)))
        }

        ("pvpFilamentMgr", "GetActiveEvents") => {
            let start = (now_unix.saturating_sub(86400)) as i64;
            let end = (now_unix + 86400 * 7) as i64;
            let event = PyRep::keyval([
                ("eventID", PyRep::Integer(1)),
                ("name", PyRep::String("1v1 Cruiser Proving Grounds".into())),
                (
                    "description",
                    PyRep::String("Proving Grounds combat arena".into()),
                ),
                ("matchTypeID", PyRep::Integer(1)),
                ("scheduleID", PyRep::Integer(1001)),
                ("startDate", PyRep::Integer(start)),
                ("endDate", PyRep::Integer(end)),
                ("typeIDs", PyRep::List(vec![PyRep::Integer(56000)])),
            ]);
            let mut dict = Vec::new();
            dict.push((PyRep::Integer(1), event));
            Some(Ok(PyRep::Dict(dict)))
        }

        ("pvpFilamentMgr", "GetMostRecentEvent") => {
            let start = (now_unix.saturating_sub(86400)) as i64;
            let end = (now_unix + 86400 * 7) as i64;
            Some(Ok(PyRep::keyval([
                ("eventID", PyRep::Integer(1)),
                ("name", PyRep::String("1v1 Cruiser Proving Grounds".into())),
                (
                    "description",
                    PyRep::String("Proving Grounds combat arena".into()),
                ),
                ("matchTypeID", PyRep::Integer(1)),
                ("scheduleID", PyRep::Integer(1001)),
                ("startDate", PyRep::Integer(start)),
                ("endDate", PyRep::Integer(end)),
                ("typeIDs", PyRep::List(vec![PyRep::Integer(56000)])),
            ])))
        }

        ("pvpFilamentMgr", "GetNextEventDate") => {
            let next_date = (now_unix + 86400 * 7) as i64;
            Some(Ok(PyRep::Integer(next_date)))
        }

        ("pvpFilamentMgr", "GetLeaderboard") => {
            let lb = with_registry(ctx, |reg| {
                let mut entries = Vec::new();
                for (&cid, &(wins, losses, score)) in &reg.stats {
                    entries.push(PyRep::keyval([
                        ("characterID", PyRep::Integer(cid)),
                        ("rank", PyRep::Integer(1)),
                        ("score", PyRep::Integer(score)),
                        ("wins", PyRep::Integer(wins)),
                        ("losses", PyRep::Integer(losses)),
                    ]));
                }
                if entries.is_empty() {
                    entries.push(PyRep::keyval([
                        ("characterID", PyRep::Integer(char_id)),
                        ("rank", PyRep::Integer(1)),
                        ("score", PyRep::Integer(100)),
                        ("wins", PyRep::Integer(10)),
                        ("losses", PyRep::Integer(2)),
                    ]));
                }
                entries
            });
            Some(Ok(PyRep::List(lb)))
        }

        ("pvpFilamentMgr", "GetCharacterStatistics") => {
            let stats = with_registry(ctx, |reg| {
                let (w, l, s) = reg.stats.get(&char_id).copied().unwrap_or((10, 2, 100));
                PyRep::keyval([
                    ("characterID", PyRep::Integer(char_id)),
                    ("rank", PyRep::Integer(1)),
                    ("score", PyRep::Integer(s)),
                    ("wins", PyRep::Integer(w)),
                    ("losses", PyRep::Integer(l)),
                    ("kills", PyRep::Integer(w + l)),
                    ("damageDealt", PyRep::Real(54320.0)),
                ])
            });
            Some(Ok(stats))
        }

        ("pvpFilamentMgr", "AbyssalPVPEndGateActivation") => {
            let gate_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            with_registry(ctx, |reg| {
                reg.queue.remove(&char_id);
            });
            Some(Ok(PyRep::keyval([
                ("gateID", PyRep::Integer(gate_id)),
                ("activated", PyRep::Bool(true)),
            ])))
        }

        _ => None,
    }
}
