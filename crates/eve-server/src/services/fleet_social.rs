//! `fleet_social` — Domain 10: Fleets, Corporations, Chat (LSC/XMPP), Mail, Bookmarks, Scanning & Wormholes (`SO-1`..`SO-7`).

#![forbid(unsafe_code)]

use crate::router::{cache_notifications::NotificationTarget, ServerContext};
use eve_db::FleetRole;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;

#[path = "fleet_social_bookmarks.rs"]
mod fleet_social_bookmarks;
#[path = "fleet_social_chat.rs"]
mod fleet_social_chat;
#[path = "fleet_social_corp_gov.rs"]
mod fleet_social_corp_gov;
#[path = "fleet_social_corps.rs"]
pub(crate) mod fleet_social_corps;
#[path = "fleet_social_fleet_ops.rs"]
mod fleet_social_fleet_ops;
#[path = "fleet_social_fleets.rs"]
mod fleet_social_fleets;
#[path = "fleet_social_mail.rs"]
mod fleet_social_mail;
#[path = "fleet_social_scanning.rs"]
mod fleet_social_scanning;

pub const AU_IN_METERS: f64 = 149_597_870_700.0;

#[allow(dead_code)]
pub(crate) fn is_in_fleet_boost_scope(
    fleet: &eve_db::FleetRecord,
    booster_char_id: i64,
    target_char_id: i64,
) -> bool {
    if booster_char_id == target_char_id {
        return true;
    }
    let Some(booster) = fleet.member_details.get(&booster_char_id) else {
        return false;
    };
    let Some(target) = fleet.member_details.get(&target_char_id) else {
        return false;
    };
    match booster.role {
        FleetRole::FleetCommander => true,
        FleetRole::WingCommander => booster.wing_id.is_some() && booster.wing_id == target.wing_id,
        FleetRole::SquadCommander => {
            booster.squad_id.is_some() && booster.squad_id == target.squad_id
        }
        FleetRole::SquadMember => false,
    }
}

#[allow(dead_code)]
pub(crate) fn is_bookmark_expired(bookmark_id: i64, now_unix: u64) -> bool {
    fleet_social_bookmarks::BM_SCAN_STATE
        .lock()
        .unwrap()
        .bookmark_expiry
        .get(&bookmark_id)
        .copied()
        .is_some_and(|exp| now_unix >= exp)
}

pub(crate) fn get_effective_corp_roles(
    ctx: &ServerContext,
    corp_id: i64,
    char_id: i64,
) -> (bool, u64) {
    let now_unix = ctx.now_unix();
    let (is_ceo, mut role_mask) = ctx.db.with_snapshot(|snap| {
        snap.corporations
            .get(&corp_id)
            .map(|c| {
                (
                    c.ceo_id == char_id,
                    c.member_roles.get(&char_id).copied().unwrap_or(0),
                )
            })
            .unwrap_or((false, 0))
    });
    if let Some(g) = fleet_social_corps::CORP_GOV.lock().unwrap().get(&corp_id) {
        if let Some(&(pending_mask, eff_unix)) = g.pending_role_grants.get(&char_id) {
            if now_unix >= eff_unix {
                role_mask |= pending_mask;
            }
        }
        if let Some(&tmask) = g.member_titles.get(&char_id) {
            for (&tid, &(_, troles)) in &g.titles {
                if (tmask & (1u64 << (tid.rem_euclid(16)))) != 0 {
                    role_mask |= troles;
                }
            }
        }
    }
    (is_ceo, role_mask)
}

pub(super) fn parse_dest_id(args: &[PyRep], kwargs: &HashMap<String, PyRep>) -> Option<i64> {
    args.first()
        .and_then(|a| a.as_i64())
        .or_else(|| kwargs.get("itemID").and_then(|a| a.as_i64()))
        .or_else(|| kwargs.get("destinationID").and_then(|a| a.as_i64()))
}

fn handle_office_manager(
    ctx: &ServerContext,
    session: &mut SessionState,
    method: &str,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (_, _char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let corp_id = session.corporation_id.unwrap_or(0);

    match method {
        "Bind" | "SetSessionCheck" => Some(Ok(PyRep::None)),

        "GetPriceQuote" => {
            Some(Ok(PyRep::Real(15_000.0)))
        }

        "RentOffice" => {
            let station_id = args
                .first()
                .and_then(|a| a.as_i64())
                .or(session.station_id)
                .unwrap_or(60_003_760);

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(corp) = snap.corporations.get_mut(&corp_id) else {
                    return Err(BridgeRpcError {
                        code: "CorporationNotFound".into(),
                        message: format!("Corporation {corp_id} not found"),
                    });
                };
                let quote = 15_000.0;
                let fee = eve_db::IskCents((quote * 100.0) as i64);
                if !corp.wallet_divisions.is_empty() {
                    if corp.wallet_divisions[0].balance.0 < fee.0 {
                        return Err(BridgeRpcError {
                            code: "NotEnoughISK".into(),
                            message: "Not enough ISK in corporation master wallet to rent office".into(),
                        });
                    }
                    corp.wallet_divisions[0].balance =
                        corp.wallet_divisions[0].balance.checked_sub(fee).unwrap_or(eve_db::IskCents(0));
                } else if corp.master_wallet.0 < fee.0 {
                    return Err(BridgeRpcError {
                        code: "NotEnoughISK".into(),
                        message: "Not enough ISK in corporation master wallet to rent office".into(),
                    });
                }
                corp.master_wallet = corp.master_wallet.checked_sub(fee).unwrap_or(eve_db::IskCents(0));

                let office_id = snap
                    .items
                    .keys()
                    .max()
                    .copied()
                    .unwrap_or(920_000_000)
                    .max(920_000_000)
                    + 1;

                snap.items.insert(
                    office_id,
                    eve_db::InventoryItemRecord {
                        item_id: office_id,
                        type_id: 27,
                        owner_id: corp_id,
                        location_id: station_id,
                        flag: eve_db::InventoryFlag::Hangar,
                        quantity: 1,
                        singleton: true,
                        version: 1,
                        ..Default::default()
                    },
                );

                Ok((office_id, station_id))
            });

            match res {
                Ok((office_id, st_id)) => Some(Ok(PyRep::keyval([
                    ("officeFolderID", PyRep::Integer(office_id)),
                    ("stationID", PyRep::Integer(st_id)),
                    ("corporationID", PyRep::Integer(corp_id)),
                    ("cost", PyRep::Real(15_000.0)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        "GetMyCorporationsOffice" => {
            let station_id = args
                .first()
                .and_then(|a| a.as_i64())
                .or(session.station_id)
                .unwrap_or(60_003_760);

            let found = ctx.db.with_snapshot(|snap| {
                snap.items.values().find_map(|i| {
                    if i.owner_id == corp_id && i.location_id == station_id && i.type_id == 27 {
                        Some(i.item_id)
                    } else {
                        None
                    }
                })
            });

            match found {
                Some(oid) => Some(Ok(PyRep::keyval([
                    ("officeFolderID", PyRep::Integer(oid)),
                    ("stationID", PyRep::Integer(station_id)),
                    ("corporationID", PyRep::Integer(corp_id)),
                ]))),
                None => Some(Err(BridgeRpcError {
                    code: "NoOfficeAtStation".into(),
                    message: format!("Corporation {corp_id} has no office at station {station_id}"),
                })),
            }
        }

        "GetMyCorporationsOffices" => {
            let rows = ctx.db.with_snapshot(|snap| {
                snap.items
                    .values()
                    .filter(|i| i.owner_id == corp_id && i.type_id == 27)
                    .map(|i| {
                        vec![
                            PyRep::Integer(i.item_id),
                            PyRep::Integer(i.location_id),
                            PyRep::Integer(corp_id),
                        ]
                    })
                    .collect::<Vec<_>>()
            });
            Some(Ok(PyRep::rowset(
                vec!["officeFolderID", "stationID", "corporationID"],
                rows,
            )))
        }

        "GetCorporationsWithOffices" => {
            let station_id = args
                .first()
                .and_then(|a| a.as_i64())
                .or(session.station_id)
                .unwrap_or(60_003_760);

            let rows = ctx.db.with_snapshot(|snap| {
                snap.items
                    .values()
                    .filter(|i| i.location_id == station_id && i.type_id == 27)
                    .map(|i| {
                        vec![
                            PyRep::Integer(i.owner_id),
                            PyRep::Integer(i.item_id),
                            PyRep::Integer(station_id),
                        ]
                    })
                    .collect::<Vec<_>>()
            });
            Some(Ok(PyRep::rowset(
                vec!["corporationID", "officeFolderID", "stationID"],
                rows,
            )))
        }

        "GetEmptyOfficeCount" => {
            let station_id = args
                .first()
                .and_then(|a| a.as_i64())
                .or(session.station_id)
                .unwrap_or(60_003_760);

            let occupied = ctx.db.with_snapshot(|snap| {
                snap.items
                    .values()
                    .filter(|i| i.location_id == station_id && i.type_id == 27)
                    .count()
            });
            let empty = (16i64 - occupied as i64).max(0);
            Some(Ok(PyRep::Integer(empty)))
        }

        "UnrentOffice" => {
            let station_id = args
                .first()
                .and_then(|a| a.as_i64())
                .or(session.station_id)
                .unwrap_or(60_003_760);

            use eve_db::InventoryFlag;
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let office_to_remove: Vec<i64> = snap
                    .items
                    .values()
                    .filter(|i| {
                        i.owner_id == corp_id && i.location_id == station_id && i.type_id == 27
                    })
                    .map(|i| i.item_id)
                    .collect();

                if office_to_remove.is_empty() {
                    return Err(BridgeRpcError {
                        code: "NoOfficeAtStation".into(),
                        message: format!("Corporation {corp_id} has no office at station {station_id}"),
                    });
                }

                for oid in office_to_remove {
                    snap.items.remove(&oid);
                }

                // Impound all corporation division hangars at this station to Deliveries (flag 173)
                for item in snap.items.values_mut() {
                    if item.owner_id == corp_id
                        && item.location_id == station_id
                        && matches!(
                            item.flag,
                            InventoryFlag::CorpSAG1
                                | InventoryFlag::CorpSAG2
                                | InventoryFlag::CorpSAG3
                                | InventoryFlag::CorpSAG4
                                | InventoryFlag::CorpSAG5
                                | InventoryFlag::CorpSAG6
                                | InventoryFlag::CorpSAG7
                        )
                    {
                        item.flag = InventoryFlag::Deliveries;
                        item.version += 1;
                    }
                }

                Ok(())
            });

            match res {
                Ok(()) => Some(Ok(PyRep::keyval([
                    ("stationID", PyRep::Integer(station_id)),
                    ("unrented", PyRep::Bool(true)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        _ => None,
    }
}

pub async fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    if service == "officeManager" {
        return handle_office_manager(ctx, session, method, args);
    }
    if service == "beyonce" && method == "GetFormations" && session.fleet_id.is_some() {
        let _ = match ServerContext::require_character(session) {
            Ok(v) => v,
            Err(e) => return Some(Err(e)),
        };
        let rows = vec![
            vec![PyRep::Integer(0), PyRep::String("Point".into()), PyRep::Real(0.0)],
            vec![PyRep::Integer(1), PyRep::String("Plane".into()), PyRep::Real(5_000.0)],
            vec![PyRep::Integer(2), PyRep::String("Sphere".into()), PyRep::Real(10_000.0)],
            vec![PyRep::Integer(3), PyRep::String("Arrow".into()), PyRep::Real(5_000.0)],
            vec![PyRep::Integer(4), PyRep::String("Wall".into()), PyRep::Real(5_000.0)],
            vec![PyRep::Integer(5), PyRep::String("Relative".into()), PyRep::Real(0.0)],
        ];
        return Some(Ok(PyRep::rowset(
            vec!["formationID", "formationName", "defaultSpacing"],
            rows,
        )));
    }
    if service == "fleetObjectHandler" && method == "MassMoveMembers" {
        let (_, char_id) = match ServerContext::require_character(session) {
            Ok(v) => v,
            Err(e) => return Some(Err(e)),
        };
        let Some(fleet_id) = session.fleet_id else {
            return Some(Err(BridgeRpcError {
                code: "NotInFleet".into(),
                message: "Character is not in a fleet".into(),
            }));
        };
        let char_ids: Vec<i64> = match args.first() {
            Some(PyRep::List(items)) | Some(PyRep::Tuple(items)) => {
                items.iter().filter_map(|v| v.as_i64()).collect()
            }
            Some(PyRep::Integer(id)) => vec![*id],
            _ => Vec::new(),
        };
        let new_wing_id = args.get(1).and_then(|a| a.as_i64()).unwrap_or(1);
        let new_squad_id = args.get(2).and_then(|a| a.as_i64()).unwrap_or(1);
        let res = ctx.db.try_with_mut_snapshot(|snap| {
            let Some(fleet) = snap.fleets.get_mut(&fleet_id) else {
                return Err(BridgeRpcError {
                    code: "FleetNotFound".into(),
                    message: format!("Fleet {fleet_id} not found"),
                });
            };
            let caller_role = fleet
                .member_details
                .get(&char_id)
                .map(|m| fleet_social_fleets::role_to_i64(m.role))
                .unwrap_or(4);
            if char_id != fleet.boss_id && caller_role > 2 {
                return Err(BridgeRpcError {
                    code: "FleetPermissionDenied".into(),
                    message: "MassMoveMembers requires commander privileges".into(),
                });
            }
            let mut moved = 0i64;
            for tid in &char_ids {
                if let Some(m) = fleet.member_details.get_mut(tid) {
                    m.wing_id = Some(new_wing_id);
                    m.squad_id = Some(new_squad_id);
                    m.role = FleetRole::SquadMember;
                    m.role_name = "Squad Member".into();
                    moved += 1;
                }
            }
            Ok(moved)
        });
        return match res {
            Ok(moved) => {
                for tid in &char_ids {
                    ctx.notify(
                        NotificationTarget::Fleet(fleet_id),
                        "OnFleetMemberChanged",
                        "fleetid",
                        vec![
                            PyRep::Integer(*tid),
                            PyRep::Integer(new_wing_id),
                            PyRep::Integer(new_squad_id),
                            PyRep::Integer(4),
                        ],
                    );
                }
                Some(Ok(PyRep::keyval([("movedCount", PyRep::Integer(moved))])))
            }
            Err(e) => Some(Err(e)),
        };
    }
    if service == "fleetObjectHandler" && method == "MakeLeader" {
        let (_, char_id) = match ServerContext::require_character(session) {
            Ok(v) => v,
            Err(e) => return Some(Err(e)),
        };
        let Some(fleet_id) = session.fleet_id else {
            return Some(Err(BridgeRpcError {
                code: "NotInFleet".into(),
                message: "Character is not in a fleet".into(),
            }));
        };
        let Some(new_boss_id) = args.first().and_then(|a| a.as_i64()) else {
            return Some(Err(BridgeRpcError {
                code: "InvalidArgument".into(),
                message: "MakeLeader requires target characterID".into(),
            }));
        };
        let res = ctx.db.try_with_mut_snapshot(|snap| {
            let Some(fleet) = snap.fleets.get_mut(&fleet_id) else {
                return Err(BridgeRpcError {
                    code: "FleetNotFound".into(),
                    message: format!("Fleet {fleet_id} not found"),
                });
            };
            if fleet.boss_id != char_id {
                return Err(BridgeRpcError {
                    code: "FleetPermissionDenied".into(),
                    message: "Only the current Fleet Boss can transfer leadership".into(),
                });
            }
            if !fleet.member_details.contains_key(&new_boss_id) {
                return Err(BridgeRpcError {
                    code: "MemberNotFound".into(),
                    message: format!("Character {new_boss_id} is not in fleet {fleet_id}"),
                });
            }
            fleet.boss_id = new_boss_id;
            Ok(())
        });
        return match res {
            Ok(()) => {
                if let Some(ad) = fleet_social_fleets::FLEET_EXTRAS
                    .lock()
                    .unwrap()
                    .adverts
                    .get_mut(&fleet_id)
                {
                    ad.leader_id = new_boss_id;
                }
                ctx.notify(
                    NotificationTarget::Fleet(fleet_id),
                    "OnChangeFleetBoss",
                    "fleetid",
                    vec![PyRep::Integer(char_id), PyRep::Integer(new_boss_id)],
                );
                Some(Ok(PyRep::keyval([
                    ("fleetID", PyRep::Integer(fleet_id)),
                    ("newBossID", PyRep::Integer(new_boss_id)),
                ])))
            }
            Err(e) => Some(Err(e)),
        };
    }

    if let Some(res) =
        fleet_social_fleets::handle_fleets(ctx, session, service, method, args, kwargs).await
    {
        return Some(res);
    }
    if let Some(res) = fleet_social_corps::handle_corps(ctx, session, service, method, args, kwargs)
    {
        return Some(res);
    }
    if let Some(res) =
        fleet_social_chat::handle_chat_and_social(ctx, session, service, method, args, kwargs)
    {
        return Some(res);
    }
    if let Some(res) =
        fleet_social_mail::handle_mail_and_calendar(ctx, session, service, method, args, kwargs)
    {
        return Some(res);
    }
    if let Some(res) =
        fleet_social_bookmarks::handle_bookmarks(ctx, session, service, method, args, kwargs).await
    {
        return Some(res);
    }
    fleet_social_scanning::handle_scanning_and_wormholes(
        ctx, session, service, method, args, kwargs,
    )
    .await
}

pub(super) fn handle_get_init_state(
    ctx: &ServerContext,
    session: &mut SessionState,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (_, char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let fleet_id = match session.fleet_id {
        Some(fid) => fid,
        None => {
            return Some(Err(BridgeRpcError {
                code: "NotInFleet".into(),
                message: "Character is not in a fleet".into(),
            }))
        }
    };
    let (fleet_name, max_size) = {
        let ex = fleet_social_fleets::FLEET_EXTRAS.lock().unwrap();
        (
            ex.fleet_names
                .get(&fleet_id)
                .cloned()
                .unwrap_or_else(|| "New Eden Taskforce".into()),
            ex.max_sizes.get(&fleet_id).copied().unwrap_or(256),
        )
    };
    let res = ctx.db.with_snapshot(|snap| {
        let Some(fleet) = snap.fleets.get(&fleet_id) else {
            return Err(BridgeRpcError {
                code: "FleetNotFound".into(),
                message: format!("Fleet {fleet_id} not found"),
            });
        };
        if !fleet.member_details.contains_key(&char_id) {
            return Err(BridgeRpcError {
                code: "NotInFleet".into(),
                message: "Character is no longer in this fleet".into(),
            });
        }
        let mut wing_rows = Vec::new();
        let mut squad_rows = Vec::new();
        for &wid in &fleet.wings {
            if let Some(w) = fleet.wing_details.get(&wid) {
                wing_rows.push(vec![
                    PyRep::Integer(w.wing_id),
                    PyRep::String(w.name.clone()),
                    w.commander_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                ]);
                for &sid in &w.squad_ids {
                    if let Some(s) = fleet.squad_details.get(&sid) {
                        squad_rows.push(vec![
                            PyRep::Integer(s.squad_id),
                            PyRep::Integer(s.wing_id),
                            PyRep::String(s.name.clone()),
                            s.commander_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                        ]);
                    }
                }
            }
        }
        let member_rows: Vec<Vec<PyRep>> = fleet
            .member_details
            .values()
            .map(|m| {
                let (sys_id, ship_type_id) = snap
                    .characters
                    .get(&m.character_id)
                    .map(|c| {
                        let st = snap
                            .items
                            .get(&c.active_ship_id)
                            .map(|i| i.type_id)
                            .unwrap_or(606);
                        (c.solar_system_id, st)
                    })
                    .unwrap_or((30_000_142, 606));
                vec![
                    PyRep::Integer(m.character_id),
                    PyRep::Integer(fleet_social_fleets::role_to_i64(m.role)),
                    PyRep::Integer(m.wing_id.unwrap_or(1)),
                    PyRep::Integer(m.squad_id.unwrap_or(1)),
                    PyRep::Bool(m.takes_fleet_warp),
                    PyRep::Integer(ship_type_id as i64),
                    PyRep::Integer(sys_id as i64),
                ]
            })
            .collect();

        Ok(PyRep::keyval([
            ("fleetID", PyRep::Integer(fleet.fleet_id)),
            ("bossID", PyRep::Integer(fleet.boss_id)),
            ("fleetName", PyRep::String(fleet_name)),
            ("motd", PyRep::String(fleet.motd.clone())),
            ("isFreeMove", PyRep::Bool(fleet.free_move)),
            ("isRegistered", PyRep::Bool(fleet.is_registered)),
            ("maxSize", PyRep::Integer(max_size as i64)),
            (
                "wings",
                PyRep::rowset(vec!["wingID", "name", "commanderID"], wing_rows),
            ),
            (
                "squads",
                PyRep::rowset(
                    vec!["squadID", "wingID", "name", "commanderID"],
                    squad_rows,
                ),
            ),
            (
                "members",
                PyRep::rowset(
                    vec![
                        "characterID",
                        "role",
                        "wingID",
                        "squadID",
                        "takesFleetWarp",
                        "shipTypeID",
                        "solarSystemID",
                    ],
                    member_rows,
                ),
            ),
        ]))
    });
    if let Err(e) = &res {
        if e.code == "NotInFleet" || e.code == "FleetNotFound" {
            session.fleet_id = None;
        }
    }
    Some(res)
}

pub(super) fn handle_fleet_ops_tail(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("fleetObjectHandler", "GetJoinRequests") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let fleet_id = session.fleet_id.unwrap_or(0);
            let rows = ctx.db.with_snapshot(|snap| {
                snap.fleets
                    .get(&fleet_id)
                    .map(|f| {
                        f.invites
                            .iter()
                            .map(|&cid| vec![PyRep::Integer(cid), PyRep::Integer(fleet_id)])
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default()
            });
            Some(Ok(PyRep::rowset(vec!["characterID", "fleetID"], rows)))
        }
        ("fleetObjectHandler", "RejectJoinRequest") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let fleet_id = session.fleet_id.unwrap_or(0);
            let applicant_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            fleet_social_fleets::FLEET_EXTRAS
                .lock()
                .unwrap()
                .invite_details
                .remove(&(fleet_id, applicant_id));
            ctx.db
                .try_with_mut_snapshot(|snap| -> Result<(), BridgeRpcError> {
                    if let Some(f) = snap.fleets.get_mut(&fleet_id) {
                        f.invites.remove(&applicant_id);
                    }
                    Ok(())
                })
                .unwrap();
            ctx.notify(
                NotificationTarget::Character(applicant_id),
                "OnFleetJoinRejected",
                "charid",
                vec![PyRep::Integer(fleet_id)],
            );
            Some(Ok(PyRep::keyval([
                ("characterID", PyRep::Integer(applicant_id)),
                ("rejected", PyRep::Bool(true)),
            ])))
        }
        ("fleetObjectHandler", "GetFleetID") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let fleet_id = session.fleet_id.or_else(|| {
                ctx.db.with_snapshot(|snap| {
                    snap.fleets
                        .values()
                        .find(|f| f.members.contains(&char_id))
                        .map(|f| f.fleet_id)
                })
            });
            Some(Ok(fleet_id.map(PyRep::Integer).unwrap_or(PyRep::None)))
        }
        ("fleetObjectHandler", "Reconnect") | ("fleetObjectHandler", "UpdateMemberInfo") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            Some(Ok(PyRep::keyval([
                ("characterID", PyRep::Integer(char_id)),
                (
                    "fleetID",
                    session.fleet_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                ),
            ])))
        }
        _ => None,
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn handle_corp_gov_tail(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    corp_id: i64,
    char_id: i64,
    now_unix: u64,
) -> Option<Result<PyRep, BridgeRpcError>> {
    use eve_db::corp_roles;
    use fleet_social_corps::CORP_GOV;

    match (service, method) {
        ("corpRegistry", "CharGetAllyBaseCost") => {
            let cost = ctx.db.with_snapshot(|snap| {
                ctx.sde.types.values().find_map(|t| {
                    let base = *t.default_attributes.get(&1820)?;
                    let per_lvl = t.default_attributes.get(&1821).copied().unwrap_or(0.0);
                    let lvl = snap
                        .skills
                        .get(&char_id)
                        .and_then(|m| m.get(&t.type_id))
                        .map(|s| s.level as f64)
                        .unwrap_or(0.0);
                    Some(base * (1.0 + per_lvl * lvl / 100.0))
                })
            });
            match cost {
                Some(c) => Some(Ok(PyRep::Real(c))),
                None => Some(Err(BridgeRpcError {
                    code: "SdeDataMissing".into(),
                    message: "No SDE type carries baseDefenderAllyCost".into(),
                })),
            }
        }
        ("corpRegistry", "UpdateAttributes") | ("corpRegistry", "UpdateCorporationAbilities") => {
            let member_limit = ctx.db.with_snapshot(|snap| {
                snap.skills
                    .get(&char_id)
                    .map(|skills| {
                        skills
                            .values()
                            .map(|s| {
                                let per_lvl = ctx
                                    .sde
                                    .types
                                    .get(&s.type_id)
                                    .and_then(|t| t.default_attributes.get(&191).copied())
                                    .unwrap_or(0.0);
                                (per_lvl * s.level as f64) as i64
                            })
                            .sum::<i64>()
                    })
                    .unwrap_or(0)
            })
            .max(1);
            CORP_GOV
                .lock()
                .unwrap()
                .entry(corp_id)
                .or_default()
                .member_limit = Some(member_limit as usize);
            Some(Ok(PyRep::keyval([
                ("corporationID", PyRep::Integer(corp_id)),
                ("memberLimit", PyRep::Integer(member_limit)),
            ])))
        }
        ("corpStationMgr", "MoveCorpHQHere") => {
            let station_id = args
                .first()
                .and_then(|a| a.as_i64())
                .or(session.station_id)
                .or(session.structure_id)
                .unwrap_or(60_003_760);
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(corp) = snap.corporations.get_mut(&corp_id) else {
                    return Err(BridgeRpcError {
                        code: "CorporationNotFound".into(),
                        message: format!("Corporation {corp_id} not found"),
                    });
                };
                let caller_role = corp.member_roles.get(&char_id).copied().unwrap_or(0);
                if char_id != corp.ceo_id
                    && !corp_roles::has_role(caller_role, corp_roles::ROLE_DIRECTOR)
                {
                    return Err(BridgeRpcError {
                        code: "CorpRoleDenied".into(),
                        message: "Moving corporation HQ requires CEO or Director role".into(),
                    });
                }
                let has_office = snap.items.values().any(|i| {
                    i.owner_id == corp_id && i.location_id == station_id && i.type_id == 27
                });
                if !has_office {
                    return Err(BridgeRpcError {
                        code: "NoOfficeAtStation".into(),
                        message: format!("Corporation {corp_id} has no office at station {station_id}"),
                    });
                }
                corp.hq_station_id = Some(station_id);
                Ok(())
            });
            match res {
                Ok(()) => {
                    ctx.notify(
                        NotificationTarget::Corporation(corp_id),
                        "OnCorporationChanged",
                        "corpid",
                        vec![PyRep::Integer(corp_id), PyRep::Integer(station_id)],
                    );
                    Some(Ok(PyRep::keyval([
                        ("corporationID", PyRep::Integer(corp_id)),
                        ("stationID", PyRep::Integer(station_id)),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }
        ("corpStationMgr", "DoStandingCheckForStationService") => {
            let allowed = ctx.db.with_snapshot(|snap| {
                snap.characters.contains_key(&char_id) && snap.corporations.contains_key(&corp_id)
            });
            Some(Ok(PyRep::Bool(allowed)))
        }
        ("corpmgr", "GetAssetInventory")
        | ("corpmgr", "GetAssetInventoryForLocation")
        | ("corpmgr", "SearchAssets") => {
            let loc_filter = args
                .first()
                .and_then(|a| a.as_i64())
                .filter(|&v| v >= 30_000_000);
            let type_filter = args.get(1).and_then(|a| a.as_i64()).or_else(|| {
                args.first()
                    .and_then(|a| a.as_i64())
                    .filter(|&v| v > 0 && v < 30_000_000)
            });
            let res = ctx.db.with_snapshot(|snap| {
                let Some(corp) = snap.corporations.get(&corp_id) else {
                    return Err(BridgeRpcError {
                        code: "CorporationNotFound".into(),
                        message: format!("Corporation {corp_id} not found"),
                    });
                };
                let caller_role = corp.member_roles.get(&char_id).copied().unwrap_or(0);
                if char_id != corp.ceo_id
                    && !corp_roles::has_role(caller_role, corp_roles::ROLE_DIRECTOR)
                {
                    return Err(BridgeRpcError {
                        code: "CorpRoleDenied".into(),
                        message: "Viewing corporation asset inventory requires Director role".into(),
                    });
                }
                let rows: Vec<Vec<PyRep>> = snap
                    .items
                    .values()
                    .filter(|i| {
                        i.owner_id == corp_id
                            && loc_filter.is_none_or(|loc| i.location_id == loc)
                            && type_filter.is_none_or(|tid| i.type_id as i64 == tid)
                    })
                    .take(100)
                    .map(|i| {
                        vec![
                            PyRep::Integer(i.item_id),
                            PyRep::Integer(i.type_id as i64),
                            PyRep::Integer(i.owner_id),
                            PyRep::Integer(i.location_id),
                            PyRep::Integer(i.flag as i64),
                            PyRep::Integer(i.quantity as i64),
                        ]
                    })
                    .collect();
                Ok(rows)
            });
            match res {
                Ok(rows) => Some(Ok(PyRep::rowset(
                    vec!["itemID", "typeID", "ownerID", "locationID", "flagID", "quantity"],
                    rows,
                ))),
                Err(e) => Some(Err(e)),
            }
        }
        ("corpmgr", "GetAggressionSettings")
        | ("corpmgr", "GetAggressionSettingsForCorps")
        | ("corpRegistry", "GetAggressionSettings") => {
            let target_corp_id = args
                .first()
                .and_then(|a| a.as_i64())
                .unwrap_or(corp_id);
            let mut g = CORP_GOV.lock().unwrap();
            let (ff, pending_val, pending_until) = if let Some(s) = g.get_mut(&target_corp_id) {
                if let Some((pending_ff, eff_unix)) = s.pending_friendly_fire {
                    if now_unix >= eff_unix {
                        s.friendly_fire_legal = pending_ff;
                        s.pending_friendly_fire = None;
                    }
                }
                (
                    s.friendly_fire_legal,
                    s.pending_friendly_fire.map(|(v, _)| v),
                    s.pending_friendly_fire.map(|(_, u)| u as i64),
                )
            } else {
                (target_corp_id >= 98_000_000, None, None)
            };
            Some(Ok(PyRep::keyval([
                ("corporationID", PyRep::Integer(target_corp_id)),
                ("isFriendlyFireLegal", PyRep::Bool(ff)),
                ("friendlyFireLegal", PyRep::Bool(ff)),
                (
                    "pendingFriendlyFireLegal",
                    pending_val.map(PyRep::Bool).unwrap_or(PyRep::None),
                ),
                (
                    "changeAtTime",
                    pending_until.map(PyRep::Integer).unwrap_or(PyRep::None),
                ),
            ])))
        }
        ("corpRegistry", "RegisterNewAggressionSettings") => {
            let is_ceo_or_dir = ctx.db.with_snapshot(|snap| {
                snap.corporations.get(&corp_id).is_some_and(|c| {
                    char_id == c.ceo_id
                        || corp_roles::has_role(
                            c.member_roles.get(&char_id).copied().unwrap_or(0),
                            corp_roles::ROLE_DIRECTOR,
                        )
                })
            });
            if !is_ceo_or_dir {
                return Some(Err(BridgeRpcError {
                    code: "CorpRoleDenied".into(),
                    message: "Only CEO or Director can change friendly fire settings".into(),
                }));
            }
            let ff = matches!(args.first(), Some(PyRep::Bool(true)) | Some(PyRep::Integer(1)));
            let effective_at = now_unix + 86_400;
            {
                let mut g = CORP_GOV.lock().unwrap();
                let state = g.entry(corp_id).or_default();
                state.pending_friendly_fire = Some((ff, effective_at));
            }
            ctx.notify(
                NotificationTarget::Corporation(corp_id),
                "OnCorpAggressionSettingsChange",
                "corpid",
                vec![
                    PyRep::Integer(corp_id),
                    PyRep::Bool(ff),
                    PyRep::Integer(effective_at as i64),
                ],
            );
            Some(Ok(PyRep::keyval([
                ("corporationID", PyRep::Integer(corp_id)),
                ("isFriendlyFireLegal", PyRep::Bool(ff)),
                ("changeAtTime", PyRep::Integer(effective_at as i64)),
            ])))
        }
        _ => None,
    }
}
