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
    // No multiple-character-training purchases exist on this server.
    let training_details = PyRep::Tuple(vec![PyRep::None, PyRep::None]);

    let (rows, wars) = ctx.db.with_snapshot(|snap| {
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
                // DERIVED: no paperdoll recustomization is pending for any EvoMu character (state 0).
                ("paperdollState", PyRep::Integer(0)),
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
        (rows, wars)
    });
    Ok(PyRep::Tuple(vec![user_details, training_details, PyRep::List(rows), PyRep::List(wars)]))
}

fn util_row(cols: &[&str], vals: Vec<PyRep>) -> PyRep {
    PyRep::Object {
        type_name: "carbon.common.script.sys.row.Row".into(),
        args: Box::new(PyRep::Tuple(vec![
            PyRep::List(cols.iter().map(|c| PyRep::String((*c).into())).collect()),
            PyRep::List(vals),
        ])),
    }
}

fn build_dogma_item_info(
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag_id: i64,
    attrs_vec: Vec<(PyRep, PyRep)>,
    group_id: i64,
    category_id: i64,
    now_ft: i64,
    desc: &str,
) -> PyRep {
    let inv_row = eve_proto::DBRowDescriptor::inv_item_header().build_dbrow(vec![
        PyRep::Integer(item_id),
        PyRep::Integer(i64::from(type_id)),
        PyRep::Integer(owner_id),
        PyRep::Integer(location_id),
        PyRep::Integer(flag_id),
        PyRep::Integer(-1),
        PyRep::Integer(group_id),
        PyRep::Integer(category_id),
        PyRep::String(String::new()),
    ]);
    PyRep::keyval([
        ("itemID", PyRep::Integer(item_id)),
        ("invItem", inv_row),
        ("attributes", PyRep::Dict(attrs_vec)),
        ("activeEffects", PyRep::Dict(Vec::new())),
        ("time", PyRep::Integer(now_ft)),
        ("wallclockTime", PyRep::Integer(now_ft)),
        ("description", PyRep::String(desc.to_string())),
    ])
}

pub fn handle_dogma_get_all_info(
    ctx: &ServerContext,
    session: &SessionState,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let ship_id = session.ship_id.unwrap_or(0);
    let now_ft = unix_to_filetime(ctx.now_unix());
    let ship_attrs: Vec<(PyRep, PyRep)> = ctx
        .ship_fittings
        .read()
        .ok()
        .and_then(|f| f.get(&ship_id).cloned())
        .map(|eng| {
            let mut v: Vec<_> = eng
                .ship
                .base_attributes
                .into_iter()
                .map(|(aid, val)| (PyRep::Integer(i64::from(aid)), PyRep::Real(val)))
                .collect();
            v.sort_by_key(|(k, _)| match k {
                PyRep::Integer(i) => *i,
                _ => 0,
            });
            v
        })
        .unwrap_or_default();
    let (ship_type_id, ship_loc_id, char_loc_id, skill_entries) = ctx.db.with_snapshot(|snap| {
        let ship_rec = snap.items.get(&ship_id);
        let stype = ship_rec.map(|s| s.type_id).unwrap_or(606);
        let sloc = ship_rec
            .map(|s| s.location_id)
            .or(session.station_id)
            .unwrap_or(60_003_760);
        let cloc = if ship_id > 0 { ship_id } else { sloc };
        let skills: Vec<(u32, u8, f64)> = snap
            .skills
            .get(&char_id)
            .map(|m| {
                let mut v: Vec<_> = m
                    .values()
                    .map(|sk| (sk.type_id, sk.level, sk.skill_points))
                    .collect();
                v.sort_by_key(|(tid, _, _)| *tid);
                v
            })
            .unwrap_or_default();
        (stype, sloc, cloc, skills)
    });
    let (s_grp, s_cat) = ctx
        .sde
        .types
        .get(&ship_type_id)
        .map(|t| (i64::from(t.group_id), i64::from(t.category_id)))
        .unwrap_or((25, 6));
    let mut ship_info_entries = Vec::new();
    if ship_id > 0 {
        ship_info_entries.push((
            PyRep::Integer(ship_id),
            build_dogma_item_info(
                ship_id,
                ship_type_id,
                char_id,
                ship_loc_id,
                4,
                ship_attrs.clone(),
                s_grp,
                s_cat,
                now_ft,
                "ship",
            ),
        ));
    }
    let mut char_info_dict = vec![(
        PyRep::Integer(char_id),
        build_dogma_item_info(
            char_id,
            1373,
            1,
            char_loc_id,
            57,
            vec![
                (PyRep::Integer(164), PyRep::Real(20.0)),
                (PyRep::Integer(165), PyRep::Real(20.0)),
                (PyRep::Integer(166), PyRep::Real(20.0)),
                (PyRep::Integer(167), PyRep::Real(20.0)),
                (PyRep::Integer(168), PyRep::Real(20.0)),
            ],
            1,
            1,
            now_ft,
            "character",
        ),
    )];
    for (idx, (sk_tid, sk_lvl, sk_sp)) in skill_entries.into_iter().enumerate() {
        let sk_item_id = char_id
            .saturating_mul(1000)
            .saturating_add(idx as i64 + 1);
        let (g_id, c_id) = ctx
            .sde
            .types
            .get(&sk_tid)
            .map(|t| (i64::from(t.group_id), i64::from(t.category_id)))
            .unwrap_or((255, 16));
        char_info_dict.push((
            PyRep::Integer(sk_item_id),
            build_dogma_item_info(
                sk_item_id,
                sk_tid,
                char_id,
                char_id,
                7,
                vec![
                    (PyRep::Integer(276), PyRep::Real(sk_sp)),
                    (PyRep::Integer(280), PyRep::Integer(i64::from(sk_lvl))),
                ],
                g_id,
                c_id,
                now_ft,
                "skill",
            ),
        ));
    }
    Ok(PyRep::keyval([
        ("structureInfo", PyRep::None),
        ("shipInfo", PyRep::Dict(ship_info_entries)),
        (
            "charInfo",
            PyRep::Tuple(vec![PyRep::Dict(char_info_dict), PyRep::List(Vec::new())]),
        ),
        ("shipModifiedCharAttribs", PyRep::None),
        ("systemWideEffectsOnShip", PyRep::None),
        (
            "shipState",
            PyRep::Tuple(vec![
                PyRep::Dict(Vec::new()),
                PyRep::Dict(Vec::new()),
                PyRep::None,
                PyRep::Dict(Vec::new()),
            ]),
        ),
        ("shipID", PyRep::Integer(ship_id)),
        ("charID", PyRep::Integer(char_id)),
        ("shipAttributes", PyRep::Dict(ship_attrs)),
        ("moduleAttributes", PyRep::Dict(Vec::new())),
    ]))
}

pub fn handle_crimewatch_get_client_states(
    ctx: &ServerContext,
    session: &SessionState,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let now_unix = ctx.now_unix();
    let (wpn_ft, pvp_ft, npc_ft, crim_ft, engagements, criminals, suspects) = ctx
        .crimewatch
        .read()
        .ok()
        .map(|cw| {
            let to_ft = |u: u64| -> PyRep {
                if u > now_unix {
                    PyRep::Integer(unix_to_filetime(u))
                } else {
                    PyRep::None
                }
            };
            let (w, p, n, c) = cw
                .states
                .get(&char_id)
                .map(|st| {
                    let c_until = st.criminal_flag_until_unix.max(st.suspect_flag_until_unix);
                    let c_state = if st.criminal_flag_until_unix > now_unix {
                        2
                    } else if st.suspect_flag_until_unix > now_unix {
                        1
                    } else {
                        0
                    };
                    (
                        (
                            if st.weapon_timer_until_unix > now_unix {
                                1
                            } else {
                                0
                            },
                            to_ft(st.weapon_timer_until_unix),
                        ),
                        (
                            if st.pvp_timer_until_unix > now_unix {
                                1
                            } else {
                                0
                            },
                            to_ft(st.pvp_timer_until_unix),
                        ),
                        (0i64, PyRep::None),
                        (c_state, to_ft(c_until)),
                    )
                })
                .unwrap_or(((0, PyRep::None), (0, PyRep::None), (0, PyRep::None), (0, PyRep::None)));
            let mut eng = Vec::new();
            for rec in cw.aggression_mgr.records.values() {
                if rec.expires_unix > now_unix && rec.aggressor_char_id == char_id {
                    eng.push((
                        PyRep::Integer(rec.victim_char_id),
                        PyRep::Integer(unix_to_filetime(rec.expires_unix)),
                    ));
                }
            }
            let mut crims = Vec::new();
            let mut susps = Vec::new();
            for st in cw.states.values() {
                if st.criminal_flag_until_unix > now_unix {
                    crims.push(PyRep::Integer(st.character_id));
                } else if st.suspect_flag_until_unix > now_unix {
                    susps.push(PyRep::Integer(st.character_id));
                }
            }
            (w, p, n, c, eng, crims, susps)
        })
        .unwrap_or((
            (0, PyRep::None),
            (0, PyRep::None),
            (0, PyRep::None),
            (0, PyRep::None),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        ));
    let booster_timer = PyRep::Tuple(vec![PyRep::Integer(0), PyRep::None]);
    let combat_timers = PyRep::Tuple(vec![
        PyRep::Tuple(vec![PyRep::Integer(wpn_ft.0), wpn_ft.1]),
        PyRep::Tuple(vec![PyRep::Integer(pvp_ft.0), pvp_ft.1]),
        PyRep::Tuple(vec![PyRep::Integer(npc_ft.0), npc_ft.1]),
        PyRep::Tuple(vec![PyRep::Integer(crim_ft.0), crim_ft.1]),
        booster_timer,
    ]);
    Ok(PyRep::Tuple(vec![
        combat_timers,
        PyRep::Dict(engagements),
        PyRep::Tuple(vec![PyRep::List(criminals), PyRep::List(suspects)]),
        PyRep::Integer(100), // ShipSafetyLevel.SAFETY_LEVEL_FULL
    ]))
}

pub fn handle_corp_get_eve_owners(
    ctx: &ServerContext,
    session: &SessionState,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let corp_id = session.corporation_id.unwrap_or(1_000_044);
    let desc = eve_proto::DBRowDescriptor::new([
        ("ownerID", eve_proto::DbColumnType::I8),
        ("ownerName", eve_proto::DbColumnType::WStr),
        ("typeID", eve_proto::DbColumnType::I4),
        ("gender", eve_proto::DbColumnType::I4),
        ("tickerName", eve_proto::DbColumnType::WStr),
    ]);
    let rows = ctx.db.with_snapshot(|snap| {
        let mut chars: Vec<_> = snap
            .characters
            .values()
            .filter(|c| c.corporation_id == corp_id || c.character_id == char_id)
            .cloned()
            .collect();
        chars.sort_by_key(|c| c.character_id);
        chars
            .into_iter()
            .map(|c| {
                vec![
                    PyRep::Integer(c.character_id),
                    PyRep::String(c.name),
                    PyRep::Integer(1373),
                    PyRep::Integer(i64::from(c.gender)),
                    PyRep::None,
                ]
            })
            .collect()
    });
    Ok(desc.build_crowset(rows))
}

pub fn handle_corp_get_corporation(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let _ = ServerContext::require_character(session)?;
    let target_corp_id = args
        .first()
        .and_then(|a| a.as_i64())
        .or(session.corporation_id)
        .unwrap_or(1_000_044);
    let (name, ticker, ceo_id, station_id, alliance_id, tax_rate, desc, url, member_count) =
        ctx.db.with_snapshot(|snap| {
            if let Some(corp) = snap.corporations.get(&target_corp_id) {
                (
                    corp.name.clone(),
                    corp.ticker.clone(),
                    corp.ceo_id,
                    corp.hq_station_id.unwrap_or(60_003_760),
                    corp.alliance_id,
                    corp.tax_rate,
                    corp.description.clone(),
                    corp.url.clone(),
                    corp.members.len() as i64,
                )
            } else if let Some(npc) = ctx.sde.npc_corporations.get(&target_corp_id) {
                let st_id = ctx
                    .sde
                    .stations
                    .values()
                    .find(|s| s.owner_corporation_id == target_corp_id)
                    .map(|s| s.station_id)
                    .unwrap_or(60_003_760);
                (
                    npc.name.clone(),
                    npc.ticker_name.clone(),
                    1,
                    st_id,
                    None,
                    0.10,
                    npc.name.clone(),
                    String::new(),
                    100,
                )
            } else {
                (
                    format!("Corporation {target_corp_id}"),
                    "CORP".to_string(),
                    1,
                    60_003_760,
                    None,
                    0.05,
                    String::new(),
                    String::new(),
                    1,
                )
            }
        });
    let cols = [
        "corporationID",
        "corporationName",
        "tickerName",
        "etick",
        "ceoID",
        "creatorID",
        "stationID",
        "allianceID",
        "warFactionID",
        "taxRate",
        "loyaltyPointTaxRate",
        "minimumJoinStanding",
        "cittype",
        "hasgn",
        "sendCharTerminationMessage",
        "url",
        "description",
        "shares",
        "memberCount",
        "memberLimit",
        "allowedMemberRaceIDs",
        "graphicID",
        "shape1",
        "shape2",
        "shape3",
        "color1",
        "color2",
        "color3",
        "typeface",
        "division1",
        "division2",
        "division3",
        "division4",
        "division5",
        "division6",
        "division7",
        "walletDivision1",
        "walletDivision2",
        "walletDivision3",
        "walletDivision4",
        "walletDivision5",
        "walletDivision6",
        "walletDivision7",
        "deleted",
        "isRecruiting",
        "friendlyFire",
    ];
    let line = vec![
        PyRep::Integer(target_corp_id),
        PyRep::String(name),
        PyRep::String(ticker.clone()),
        PyRep::String(ticker),
        PyRep::Integer(ceo_id),
        PyRep::Integer(ceo_id),
        PyRep::Integer(station_id),
        alliance_id.map(PyRep::Integer).unwrap_or(PyRep::None),
        PyRep::None,
        PyRep::Real(tax_rate),
        PyRep::Real(0.0),
        PyRep::Real(0.0),
        PyRep::Integer(1),
        PyRep::Integer(0),
        PyRep::Integer(1),
        PyRep::String(url),
        PyRep::String(desc),
        PyRep::Integer(1000),
        PyRep::Integer(member_count),
        PyRep::Integer(600),
        PyRep::Integer(255),
        PyRep::Integer(0),
        PyRep::None,
        PyRep::None,
        PyRep::None,
        PyRep::None,
        PyRep::None,
        PyRep::None,
        PyRep::None,
        PyRep::String("1st Division".into()),
        PyRep::String("2nd Division".into()),
        PyRep::String("3rd Division".into()),
        PyRep::String("4th Division".into()),
        PyRep::String("5th Division".into()),
        PyRep::String("6th Division".into()),
        PyRep::String("7th Division".into()),
        PyRep::String("Master Wallet".into()),
        PyRep::String("2nd Wallet".into()),
        PyRep::String("3rd Wallet".into()),
        PyRep::String("4th Wallet".into()),
        PyRep::String("5th Wallet".into()),
        PyRep::String("6th Wallet".into()),
        PyRep::String("7th Wallet".into()),
        PyRep::Integer(0),
        PyRep::Integer(1),
        PyRep::Integer(0),
    ];
    Ok(util_row(&cols, line))
}

pub fn handle_corp_get_member(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, caller_id) = ServerContext::require_character(session)?;
    let target_char_id = args.first().and_then(|a| a.as_i64()).unwrap_or(caller_id);
    let now_ft = unix_to_filetime(ctx.now_unix());
    let (corp_id, roles, station_id) = ctx.db.with_snapshot(|snap| {
        let ch = snap.characters.get(&target_char_id);
        let cid = ch
            .map(|c| c.corporation_id)
            .or(session.corporation_id)
            .unwrap_or(1_000_044);
        let st_id = ch
            .and_then(|c| c.station_id)
            .or(session.station_id)
            .unwrap_or(60_003_760);
        let r = snap
            .corporations
            .get(&cid)
            .and_then(|corp| corp.member_roles.get(&target_char_id).copied())
            .unwrap_or(0);
        (cid, r as i64, st_id)
    });
    let cols = [
        "characterID",
        "corporationID",
        "divisionID",
        "squadronID",
        "title",
        "roles",
        "grantableRoles",
        "startDateTime",
        "baseID",
        "rolesAtHQ",
        "grantableRolesAtHQ",
        "rolesAtBase",
        "grantableRolesAtBase",
        "rolesAtOther",
        "grantableRolesAtOther",
        "titleMask",
        "accountKey",
        "blockRoles",
    ];
    let line = vec![
        PyRep::Integer(target_char_id),
        PyRep::Integer(corp_id),
        PyRep::Integer(0),
        PyRep::Integer(0),
        PyRep::String(String::new()),
        PyRep::Integer(roles),
        PyRep::Integer(0),
        PyRep::Integer(now_ft),
        PyRep::Integer(station_id),
        PyRep::Integer(roles),
        PyRep::Integer(0),
        PyRep::Integer(roles),
        PyRep::Integer(0),
        PyRep::Integer(roles),
        PyRep::Integer(0),
        PyRep::Integer(0),
        PyRep::Integer(1000),
        PyRep::None,
    ];
    Ok(util_row(&cols, line))
}

pub fn handle_skill_get_skills(
    ctx: &ServerContext,
    session: &SessionState,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let entries = ctx.db.with_snapshot(|snap| {
        let mut list: Vec<_> = snap
            .skills
            .get(&char_id)
            .map(|m| m.values().cloned().collect())
            .unwrap_or_default();
        list.sort_by_key(|sk| sk.type_id);
        list.into_iter()
            .map(|sk| {
                let rank = ctx
                    .sde
                    .types
                    .get(&sk.type_id)
                    .and_then(|t| t.default_attributes.get(&275).copied())
                    .unwrap_or(1.0);
                (
                    PyRep::Integer(i64::from(sk.type_id)),
                    PyRep::keyval([
                        ("typeID", PyRep::Integer(i64::from(sk.type_id))),
                        ("skillPoints", PyRep::Integer(sk.skill_points.round() as i64)),
                        ("trainedSkillLevel", PyRep::Integer(i64::from(sk.level))),
                        ("effectiveSkillLevel", PyRep::Integer(i64::from(sk.level))),
                        ("virtualSkillLevel", PyRep::None),
                        ("skillRank", PyRep::Real(rank)),
                    ]),
                )
            })
            .collect()
    });
    Ok(PyRep::Dict(entries))
}

pub fn handle_skill_get_queue_and_free_points(
    ctx: &ServerContext,
    session: &SessionState,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let now_ft = unix_to_filetime(ctx.now_unix());
    let (queue_items, free_sp) = ctx.db.with_snapshot(|snap| {
        let free = snap
            .characters
            .get(&char_id)
            .map(|c| c.free_skill_points.round() as i64)
            .unwrap_or(0);
        let q: Vec<PyRep> = snap
            .skill_queues
            .get(&char_id)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .enumerate()
            .map(|(pos, entry)| {
                let start_ft = entry
                    .started_at_unix
                    .map(unix_to_filetime)
                    .unwrap_or(now_ft);
                let end_ft = start_ft + (entry.duration_seconds.round() as i64) * 10_000_000;
                PyRep::keyval([
                    ("queuePosition", PyRep::Integer(pos as i64)),
                    ("trainingTypeID", PyRep::Integer(i64::from(entry.type_id))),
                    (
                        "trainingToLevel",
                        PyRep::Integer(i64::from(entry.target_level)),
                    ),
                    ("trainingStartSP", PyRep::Integer(entry.start_sp.round() as i64)),
                    (
                        "trainingDestinationSP",
                        PyRep::Integer(entry.end_sp.round() as i64),
                    ),
                    ("trainingStartTime", PyRep::Integer(start_ft)),
                    ("trainingEndTime", PyRep::Integer(end_ft)),
                ])
            })
            .collect();
        (q, free)
    });
    Ok(PyRep::Tuple(vec![
        PyRep::List(queue_items),
        PyRep::Integer(free_sp),
    ]))
}

