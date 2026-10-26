//! `charUnboundMgr.GetCharacterSelectionData` — the character selection screen's data source in the
//! V24.01 client (`loginCharacterselection/charselData.pyj` `FetchCharacterInfoForUserID`).
//!
//! The client unpacks `(userDetails, trainingDetails, characterDetails, wars)`:
//! - `userDetails[0]`: `characterSlots`, `maxCharacterSlots`, `userName`, `creationDate`, `subscriptionEndTime`;
//! - `trainingDetails`: the 2-tuple of multiple-character-training end times;
//! - `characterDetails`: one row per character, read by attribute (`characterID`, `stationID`,
//!   `solarSystemID`, `balance`, `skillPoints`, `corporationID`, `allianceID`, `shipTypeID`, `raceID`,
//!   `bloodlineID`, `gender`, `skillTypeID`/`toLevel`/training times, `logoffDate`, ...);
//! - `wars`: rows with `characterID`, `warID`, `declaredByID`, `againstID`, `mutual`, `ally`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};

/// Character slots per account in V24 (three characters per account).
const ACCOUNT_CHARACTER_SLOTS: i64 = 3;

fn unix_to_filetime(unix_secs: u64) -> i64 {
    (unix_secs as i64 + 11_644_473_600).saturating_mul(10_000_000)
}

pub fn get_character_selection_data(
    ctx: &ServerContext,
    session: &SessionState,
) -> Result<PyRep, BridgeRpcError> {
    let user_id = ServerContext::require_auth(session)?;
    let chars = ctx.db.get_characters_for_user(user_id);
    let now_unix = ctx.now_unix();
    let user_name = ctx
        .db
        .with_snapshot(|s| s.accounts.values().find(|a| a.user_id == user_id).map(|a| a.username.clone()))
        .unwrap_or_default();
    // DERIVED: the account record has no creation time; the oldest character's creation date
    // stands in (or "now" for an account without characters).
    let created_unix = chars.iter().map(|c| c.create_date_unix).filter(|t| *t > 0).min().unwrap_or(now_unix);
    let user_details = PyRep::List(vec![PyRep::keyval([
        ("characterSlots", PyRep::Integer(ACCOUNT_CHARACTER_SLOTS)),
        ("maxCharacterSlots", PyRep::Integer(ACCOUNT_CHARACTER_SLOTS)),
        ("userName", PyRep::String(user_name)),
        ("creationDate", PyRep::Integer(unix_to_filetime(created_unix))),
        ("subscriptionEndTime", PyRep::None),
    ])]);
    let (training_details, rows, wars) = ctx.db.with_snapshot(|snap| {
        let store = super::account_services::load_store(snap);
        let mct_slots: Vec<u64> = snap
            .service_state_blobs
            .get(&format!("mct_slots:{user_id}"))
            .and_then(|b| serde_json::from_str(b).ok())
            .or_else(|| store.mct_slots.get(&user_id).cloned())
            .unwrap_or_default();
        let active_mct: Vec<u64> = mct_slots.into_iter().filter(|&exp| exp > now_unix).collect();
        let training_details = if active_mct.is_empty() {
            PyRep::Tuple(vec![PyRep::None, PyRep::None])
        } else {
            let end_times: Vec<PyRep> = active_mct
                .iter()
                .map(|&exp| {
                    let ft = unix_to_filetime(exp);
                    PyRep::keyval([
                        ("trainingEnds", PyRep::Integer(ft)),
                        ("trainingEndTime", PyRep::Integer(ft)),
                        ("endTime", PyRep::Integer(ft)),
                    ])
                })
                .collect();
            PyRep::Tuple(vec![PyRep::Integer(active_mct.len() as i64), PyRep::List(end_times)])
        };

        let mut rows = Vec::new();
        let mut wars = Vec::new();
        for c in &chars {
            let ship_type = snap
                .items
                .get(&c.active_ship_id)
                .map(|i| PyRep::Integer(i64::from(i.type_id)))
                .unwrap_or(PyRep::None);
            let queue_head = snap.skill_queues.get(&c.character_id).and_then(|q| q.first());
            let (skill_type, to_level, start_t, end_t, finish_sp, trained_sp) = match queue_head {
                Some(e) => {
                    let start = e.started_at_unix.unwrap_or(now_unix);
                    let end = start + e.duration_seconds.max(0.0) as u64;
                    (
                        PyRep::Integer(i64::from(e.type_id)),
                        PyRep::Integer(i64::from(e.target_level)),
                        PyRep::Integer(unix_to_filetime(start)),
                        PyRep::Integer(unix_to_filetime(end)),
                        PyRep::Real(e.end_sp),
                        PyRep::Real(e.start_sp),
                    )
                }
                None => (PyRep::None, PyRep::None, PyRep::None, PyRep::None, PyRep::None, PyRep::None),
            };
            let queue_end = snap.skill_queues.get(&c.character_id).filter(|q| !q.is_empty()).map(|q| {
                let start = q[0].started_at_unix.unwrap_or(now_unix);
                let total: f64 = q.iter().map(|e| e.duration_seconds.max(0.0)).sum();
                PyRep::Integer(unix_to_filetime(start + total as u64))
            });
            let unread = snap.mails.iter().filter(|m| m.recipient_id == c.character_id && !snap.read_mails.contains(&(c.character_id, m.message_id))).count();
            let is_resculpt_pending = store.resculpt_pending.contains(&c.character_id)
                || snap
                    .service_state_blobs
                    .get(&format!("resculpt_pending:{}", c.character_id))
                    .map(|s| s == "1" || s == "true")
                    .unwrap_or(false);
            let paperdoll_state = if is_resculpt_pending { 1 } else { 0 };
            rows.push(PyRep::keyval([
                ("characterID", PyRep::Integer(c.character_id)),
                ("characterName", PyRep::String(c.name.clone())),
                ("stationID", c.station_id.or(c.structure_id).map(PyRep::Integer).unwrap_or(PyRep::None)),
                ("solarSystemID", PyRep::Integer(i64::from(c.solar_system_id))),
                ("balance", PyRep::Real(c.balance.to_wire_f64())),
                // DERIVED: wallet deltas since the previous session are not tracked, so no change.
                ("balanceChange", PyRep::Real(0.0)),
                ("skillPoints", PyRep::Real(c.skill_points)),
                ("corporationID", PyRep::Integer(c.corporation_id)),
                ("allianceID", c.alliance_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
                ("factionID", c.war_faction_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
                ("unreadMailCount", PyRep::Integer(unread as i64)),
                ("shipTypeID", ship_type),
                ("raceID", PyRep::Integer(i64::from(c.race_id))),
                ("bloodlineID", PyRep::Integer(i64::from(c.bloodline_id))),
                ("gender", PyRep::Integer(i64::from(c.gender))),
                ("paperdollState", PyRep::Integer(paperdoll_state)),
                ("finishedSkills", PyRep::List(Vec::new())),
                ("skillTypeID", skill_type),
                ("toLevel", to_level),
                ("trainingStartTime", start_t),
                ("trainingEndTime", end_t),
                ("queueEndTime", queue_end.unwrap_or(PyRep::None)),
                ("finishSP", finish_sp),
                ("trainedSP", trained_sp),
                ("deletePrepareDateTime", PyRep::None),
                ("lockTypeID", PyRep::None),
                // DERIVED: logoff times are not stored; the creation date stands in so the client's
                // "most recently played first" sort is at least stable.
                ("logoffDate", PyRep::Integer(unix_to_filetime(c.create_date_unix.max(1)))),
            ]));
            for w in snap.wars.values().filter(|w| w.active) {
                let involved = w.aggressor_corp_id == c.corporation_id || w.defender_corp_id == c.corporation_id;
                if involved {
                    wars.push(PyRep::keyval([
                        ("characterID", PyRep::Integer(c.character_id)),
                        ("warID", PyRep::Integer(w.war_id)),
                        ("declaredByID", PyRep::Integer(w.aggressor_corp_id)),
                        ("againstID", PyRep::Integer(w.defender_corp_id)),
                        ("mutual", PyRep::Bool(w.mutual)),
                        ("ally", PyRep::Bool(false)),
                    ]));
                }
            }
        }
        (training_details, rows, wars)
    });
    Ok(PyRep::Tuple(vec![user_details, training_details, PyRep::List(rows), PyRep::List(wars)]))
}
