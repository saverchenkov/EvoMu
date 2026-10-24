//! `fleet_social` — Domain 10: Fleets, Corporations, Chat (LSC/XMPP), Mail, Bookmarks, Scanning & Wormholes (`SO-1`..`SO-7`).

#![forbid(unsafe_code)]

use crate::router::{cache_notifications::NotificationTarget, ServerContext};
use eve_db::FleetRole;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;

#[path = "fleet_social_bookmarks.rs"]
mod fleet_social_bookmarks;
#[path = "fleet_social_access_group_bookmarks.rs"]
pub(crate) mod fleet_social_access_group_bookmarks;
#[path = "fleet_social_access_group_bookmarks_ops.rs"]
pub(crate) mod fleet_social_access_group_bookmarks_ops;
#[path = "fleet_social_calendar_ops.rs"]
pub(crate) mod fleet_social_calendar_ops;
pub(crate) use fleet_social_calendar_ops::*;
#[path = "fleet_social_chat.rs"]
pub(crate) mod fleet_social_chat;
#[path = "fleet_social_corp_gov.rs"]
mod fleet_social_corp_gov;
#[path = "fleet_social_corps.rs"]
pub mod fleet_social_corps;
#[path = "fleet_social_fleet_ops.rs"]
mod fleet_social_fleet_ops;
#[path = "fleet_social_fleets.rs"]
pub(crate) mod fleet_social_fleets;
#[path = "fleet_social_mail.rs"]
mod fleet_social_mail;
#[path = "fleet_social_notify.rs"]
pub mod fleet_social_notify;
#[path = "fleet_social_persistence.rs"]
pub mod fleet_social_persistence;
#[path = "fleet_social_scanning.rs"]
mod fleet_social_scanning;

pub use fleet_social_notify::stream_fleet_watchlist_damage;
pub use fleet_social_persistence::persist_social_state;
pub(crate) use fleet_social_persistence::{
    hydrate_social_state_from_db, update_calendar_response_from_esi,
};

pub const AU_IN_METERS: f64 = 149_597_870_700.0;

/// Character IDs currently in mailing list `list_id` (per-`ServerContext` `mailingListsMgr` state).
pub fn mailing_list_member_ids(ctx: &ServerContext, list_id: i64) -> Vec<i64> {
    ctx.social_extras
        .lock()
        .unwrap()
        .mailing_lists
        .get(&list_id)
        .map(|ml| ml.members.keys().copied().collect())
        .unwrap_or_default()
}

pub fn get_trashed_mails(ctx: &ServerContext) -> std::collections::HashSet<(i64, i64)> {
    let mut set = ctx.social_extras.lock().unwrap().trashed_mails.clone();
    ctx.db.with_snapshot(|snap| {
        set.extend(snap.trashed_mails.iter().copied());
    });
    set
}

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

pub(crate) fn is_bookmark_expired(ctx: &ServerContext, bookmark_id: i64, now_unix: u64) -> bool {
    ctx.db.with_snapshot(|snap| {
        snap.bookmark_expiries
            .get(&bookmark_id)
            .copied()
            .is_some_and(|exp| exp > 0 && now_unix >= exp)
    })
}

pub(crate) fn get_effective_corp_roles(
    ctx: &ServerContext,
    corp_id: i64,
    char_id: i64,
) -> (bool, u64) {
    let now_unix = ctx.now_unix();
    let (is_ceo, role_mask) = ctx.db.with_snapshot(|snap| {
        let (is_ceo, mut role_mask) = snap
            .corporations
            .get(&corp_id)
            .map(|c| {
                (
                    c.ceo_id == char_id,
                    c.member_roles.get(&char_id).copied().unwrap_or(0),
                )
            })
            .unwrap_or((false, 0));
        if let Some(g) = snap.corp_governance.get(&corp_id) {
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
    });
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
                Ok((office_id, st_id)) => {
                    let due_unix = ctx.now_unix().saturating_add(30 * 86_400);
                    let _ = super::wallet_serverops::create_bill(ctx, 2, corp_id, 1_000_035, 15_000.0, due_unix, 0.0, st_id, 27);
                    ctx.notify(
                        NotificationTarget::Corporation(corp_id),
                        "OnOfficeRentalChange",
                        "corpid",
                        vec![PyRep::Integer(corp_id), PyRep::Integer(office_id)],
                    );
                    Some(Ok(PyRep::keyval([
                        ("officeFolderID", PyRep::Integer(office_id)),
                        ("stationID", PyRep::Integer(st_id)),
                        ("corporationID", PyRep::Integer(corp_id)),
                        ("cost", PyRep::Real(15_000.0)),
                    ])))
                }
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
                Ok(()) => {
                    ctx.notify(
                        NotificationTarget::Corporation(corp_id),
                        "OnOfficeRentalChange",
                        "corpid",
                        vec![PyRep::Integer(corp_id), PyRep::None],
                    );
                    Some(Ok(PyRep::keyval([
                        ("stationID", PyRep::Integer(station_id)),
                        ("unrented", PyRep::Bool(true)),
                    ])))
                }
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
    // parity: rpc:beyonce.GetFormations
    if service == "beyonce" && method == "GetFormations" {
        let _ = match ServerContext::require_character(session) {
            Ok(v) => v,
            Err(e) => return Some(Err(e)),
        };
        let pos3 = |x: f64, y: f64, z: f64| {
            PyRep::Tuple(vec![PyRep::Real(x), PyRep::Real(y), PyRep::Real(z)])
        };
        return Some(Ok(PyRep::Tuple(vec![
            PyRep::Tuple(vec![
                PyRep::ascii_bytes("Diamond"),
                PyRep::Tuple(vec![
                    pos3(100.0, 0.0, 0.0),
                    pos3(-100.0, 0.0, 0.0),
                    pos3(0.0, 100.0, 0.0),
                    pos3(0.0, -100.0, 0.0),
                ]),
            ]),
            PyRep::Tuple(vec![
                PyRep::ascii_bytes("Arrow"),
                PyRep::Tuple(vec![
                    pos3(100.0, 0.0, -50.0),
                    pos3(-100.0, 0.0, -50.0),
                    pos3(200.0, 0.0, -100.0),
                    pos3(-200.0, 0.0, -100.0),
                ]),
            ]),
        ])));
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
            let old_before = fleet.member_details.get(&char_id).cloned().ok_or_else(|| BridgeRpcError {
                code: "MemberNotFound".into(),
                message: format!("Boss {char_id} is not in fleet {fleet_id}"),
            })?;
            let mut old_after = old_before.clone();
            old_after.job &= !2;
            fleet.member_details.insert(char_id, old_after.clone());

            let new_before = fleet.member_details.get(&new_boss_id).cloned().ok_or_else(|| BridgeRpcError {
                code: "MemberNotFound".into(),
                message: format!("Character {new_boss_id} is not in fleet {fleet_id}"),
            })?;
            let mut new_after = new_before.clone();
            new_after.job |= 2;
            fleet.member_details.insert(new_boss_id, new_after.clone());
            Ok((old_before, old_after, new_before, new_after))
        });
        return match res {
            Ok((old_before, old_after, new_before, new_after)) => {
                if let Some(ad) = ctx.fleet_extras
                    .lock()
                    .unwrap()
                    .adverts
                    .get_mut(&fleet_id)
                {
                    ad.leader_id = new_boss_id;
                }
                fleet_social_notify::emit_member_changed(ctx, fleet_id, &old_before, &old_after, false);
                fleet_social_notify::emit_member_changed(ctx, fleet_id, &new_before, &new_after, false);
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
    if service == "accessGroupBookmarkMgr" {
        return fleet_social_access_group_bookmarks::handle(
            ctx, session, service, method, args, kwargs,
        )
        .await;
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

pub(super) fn handle_get_wings(
    ctx: &ServerContext,
    session: &mut SessionState,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (_, _char_id) = match ServerContext::require_character(session) {
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
    let res = ctx.db.with_snapshot(|snap| {
        let Some(fleet) = snap.fleets.get(&fleet_id) else {
            return Err(BridgeRpcError {
                code: "FleetNotFound".into(),
                message: format!("Fleet {fleet_id} not found"),
            });
        };
        Ok(PyRep::Dict(fleet_social_notify::build_wings_dict(fleet)))
    });
    if let Err(e) = &res {
        if e.code == "NotInFleet" || e.code == "FleetNotFound" {
            session.fleet_id = None;
        }
    }
    Some(res)
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
    let (fleet_name, max_size, auto_join_squad, is_voice_enabled, is_loot_logging) = {
        let ex = ctx.fleet_extras.lock().unwrap();
        (
            ex.fleet_names
                .get(&fleet_id)
                .cloned()
                .unwrap_or_else(|| "New Eden Taskforce".into()),
            ex.max_sizes.get(&fleet_id).copied().unwrap_or(256),
            ex.auto_join_squads.get(&fleet_id).copied().flatten(),
            ex.voice_enabled.get(&fleet_id).copied().unwrap_or(false),
            ex.loot_logging.get(&fleet_id).copied().unwrap_or(false),
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
        let wings_dict = fleet_social_notify::build_wings_dict(fleet);
        let mut squad_rows = Vec::new();
        for &wid in &fleet.wings {
            if let Some(w) = fleet.wing_details.get(&wid) {
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
        let mut member_ids: Vec<i64> = fleet.member_details.keys().copied().collect();
        member_ids.sort_unstable();
        let mut members_dict = Vec::with_capacity(member_ids.len());
        for mid in member_ids {
            let Some(m) = fleet.member_details.get(&mid) else {
                continue;
            };
            let (sys_id_opt, ship_type_opt) = snap
                .characters
                .get(&m.character_id)
                .map(|c| {
                    let st = snap
                        .items
                        .get(&c.active_ship_id)
                        .map(|i| i.type_id as i64);
                    (Some(c.solar_system_id as i64), st)
                })
                .unwrap_or((session.solar_system_id.map(|s| s as i64), None));
            let is_boss = m.character_id == fleet.boss_id;
            let job = if is_boss { m.job | 2 } else { m.job & !2 };
            // V24 uses command bursts rather than legacy fleet booster slot assignments.
            let role_booster = 0i64;
            const SKILL_LEADERSHIP: u32 = 3348;
            const SKILL_WING_COMMAND: u32 = 11_574;
            const SKILL_FLEET_COMMAND: u32 = 24_764;
            let char_skills = snap.skills.get(&m.character_id);
            let skill_lvl = |type_id: u32| -> i64 {
                char_skills
                    .and_then(|map| map.get(&type_id))
                    .map(|s| s.level as i64)
                    .unwrap_or(0)
            };
            let skills = PyRep::List(vec![
                PyRep::Integer(skill_lvl(SKILL_LEADERSHIP)),
                PyRep::Integer(skill_lvl(SKILL_WING_COMMAND)),
                PyRep::Integer(skill_lvl(SKILL_FLEET_COMMAND)),
            ]);
            let opt_outs = PyRep::keyval([
                ("acceptsFleetWarp", PyRep::Bool(m.takes_fleet_warp)),
                ("acceptsRegroup", PyRep::Bool(m.accepts_regroup)),
                ("acceptsConduitJumps", PyRep::Bool(m.accepts_conduit_jumps)),
            ]);
            members_dict.push((
                PyRep::Integer(m.character_id),
                PyRep::keyval([
                    ("charID", PyRep::Integer(m.character_id)),
                    ("characterID", PyRep::Integer(m.character_id)),
                    ("clientID", PyRep::Integer(m.character_id)),
                    ("job", PyRep::Integer(job as i64)),
                    ("role", PyRep::Integer(fleet_social_fleets::role_to_i64(m.role))),
                    ("roleBooster", PyRep::Integer(role_booster)),
                    ("wingID", PyRep::Integer(m.wing_id.unwrap_or(-1))),
                    ("squadID", PyRep::Integer(m.squad_id.unwrap_or(-1))),
                    (
                        "shipTypeID",
                        ship_type_opt.map(PyRep::Integer).unwrap_or(PyRep::None),
                    ),
                    (
                        "solarSystemID",
                        sys_id_opt.map(PyRep::Integer).unwrap_or(PyRep::None),
                    ),
                    ("takesFleetWarp", PyRep::Bool(m.takes_fleet_warp)),
                    ("memberOptOuts", opt_outs),
                    ("skills", skills),
                    ("timestamp", PyRep::Integer(m.join_time_unix as i64)),
                ]),
            ));
        }

        let options = PyRep::keyval([
            ("isFreeMove", PyRep::Bool(fleet.free_move)),
            ("isRegistered", PyRep::Bool(fleet.is_registered)),
            ("isVoiceEnabled", PyRep::Bool(is_voice_enabled)),
            (
                "autoJoinSquadID",
                auto_join_squad.map(PyRep::Integer).unwrap_or(PyRep::None),
            ),
        ]);

        Ok(PyRep::keyval([
            ("fleetID", PyRep::Integer(fleet.fleet_id)),
            ("bossID", PyRep::Integer(fleet.boss_id)),
            ("fleetName", PyRep::String(fleet_name.clone())),
            ("name", PyRep::String(fleet_name)),
            ("motd", PyRep::String(fleet.motd.clone())),
            ("options", options),
            ("isFreeMove", PyRep::Bool(fleet.free_move)),
            ("isRegistered", PyRep::Bool(fleet.is_registered)),
            ("isLootLogging", PyRep::Bool(is_loot_logging)),
            ("maxSize", PyRep::Integer(max_size as i64)),
            ("wings", PyRep::Dict(wings_dict)),
            (
                "squads",
                PyRep::rowset(
                    vec!["squadID", "wingID", "name", "commanderID"],
                    squad_rows,
                ),
            ),
            ("members", PyRep::Dict(members_dict)),
        ]))
    });
    if let Err(e) = &res {
        if e.code == "NotInFleet" || e.code == "FleetNotFound" {
            session.fleet_id = None;
        }
    }
    Some(res)
}

pub(super) use fleet_social_notify::handle_fleet_watchlist_ops;

pub(super) fn handle_fleet_ops_tail(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("fleetObjectHandler", "GetJoinRequests") => {
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
            let res = ctx.db.with_snapshot(|snap| {
                let Some(f) = snap.fleets.get(&fleet_id) else {
                    return Err(BridgeRpcError {
                        code: "FleetNotFound".into(),
                        message: format!("Fleet {fleet_id} not found"),
                    });
                };
                let Some(caller_m) = f.member_details.get(&char_id) else {
                    return Err(BridgeRpcError {
                        code: "NotInFleet".into(),
                        message: "Caller is not in fleet".into(),
                    });
                };
                if f.boss_id != char_id && self::fleet_social_fleets::role_to_i64(caller_m.role) > 3 {
                    return Err(BridgeRpcError {
                        code: "FleetPermissionDenied".into(),
                        message: "Only Fleet Boss or a Commander can view join requests".into(),
                    });
                }
                Ok(())
            });
            match res {
                Ok(()) => {
                    let applicants = fleet_social_notify::get_fleet_join_requests(ctx, fleet_id);
                    let dict: Vec<(PyRep, PyRep)> = applicants
                        .into_iter()
                        .map(|aid| {
                            (
                                PyRep::Integer(aid),
                                PyRep::keyval([("charID", PyRep::Integer(aid))]),
                            )
                        })
                        .collect();
                    Some(Ok(PyRep::Dict(dict)))
                }
                Err(e) => {
                    if e.code == "NotInFleet" || e.code == "FleetNotFound" {
                        session.fleet_id = None;
                    }
                    Some(Err(e))
                }
            }
        }
        ("fleetObjectHandler", "RejectJoinRequest") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let fleet_id = session.fleet_id.unwrap_or(0);
            let applicant_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let is_boss = ctx.db.with_snapshot(|snap| {
                snap.fleets.get(&fleet_id).map(|f| f.boss_id == char_id).unwrap_or(false)
            });
            if !is_boss {
                return Some(Err(BridgeRpcError {
                    code: "FleetPermissionDenied".into(),
                    message: "Only the Fleet Boss can reject join requests".into(),
                }));
            }
            let _ = fleet_social_notify::get_fleet_join_requests(ctx, fleet_id);
            let removed = {
                let mut ex = ctx.fleet_extras.lock().unwrap();
                ex.join_requests.get_mut(&fleet_id).map(|set| set.remove(&applicant_id)).unwrap_or(false)
            };
            if !removed {
                return Some(Err(BridgeRpcError {
                    code: "FleetJoinRequestNotFound".into(),
                    message: format!("No pending join request from {applicant_id}"),
                }));
            }
            {
                let mut ex = ctx.fleet_extras.lock().unwrap();
                ex.invite_details.remove(&(fleet_id, applicant_id));
                ex.invite_timestamps.remove(&(fleet_id, applicant_id));
            }
            fleet_social_notify::sync_fleet_join_requests_to_db(ctx);
            ctx.notify(
                NotificationTarget::Character(applicant_id),
                "OnFleetJoinRejected",
                "charid",
                vec![PyRep::Integer(char_id)],
            );
            let remaining_requests = {
                let ex = ctx.fleet_extras.lock().unwrap();
                ex.join_requests
                    .get(&fleet_id)
                    .map(|set| {
                        set.iter()
                            .map(|&cid| (PyRep::Integer(cid), PyRep::keyval([("charID", PyRep::Integer(cid))])))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default()
            };
            ctx.notify(
                NotificationTarget::Character(char_id),
                "OnJoinRequestUpdate",
                "charid",
                vec![PyRep::Dict(remaining_requests)],
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
            let fleet_id = fleet_social_notify::character_active_fleet_id(ctx, char_id);
            session.fleet_id = fleet_id;
            Some(Ok(fleet_id.map(PyRep::Integer).unwrap_or(PyRep::None)))
        }
        ("fleetObjectHandler", "Reconnect") | ("fleetObjectHandler", "UpdateMemberInfo") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let fleet_id = fleet_social_notify::character_active_fleet_id(ctx, char_id);
            session.fleet_id = fleet_id;
            let (wid, sid, role) = ctx.db.with_snapshot(|snap| {
                fleet_id
                    .and_then(|fid| snap.fleets.get(&fid))
                    .and_then(|f| f.member_details.get(&char_id))
                    .map(|m| {
                        (
                            m.wing_id.unwrap_or(1),
                            m.squad_id.unwrap_or(1),
                            fleet_social_fleets::role_to_i64(m.role),
                        )
                    })
                    .unwrap_or((1, 1, 4))
            });
            Some(Ok(PyRep::keyval([
                ("characterID", PyRep::Integer(char_id)),
                (
                    "fleetID",
                    fleet_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                ),
                ("wingID", PyRep::Integer(wid)),
                ("squadID", PyRep::Integer(sid)),
                ("role", PyRep::Integer(role)),
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
    use fleet_social_corps::with_corp_gov;

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
            ctx.db.with_mut_snapshot(|snap| {
                snap.corp_governance
                    .entry(corp_id)
                    .or_default()
                    .member_limit = Some(member_limit as usize);
                snap.corp_member_limits.insert(corp_id, member_limit);
            });
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
                let old_hq = corp.hq_station_id.replace(station_id);
                Ok(old_hq)
            });
            match res {
                Ok(old_hq) => {
                    let change = PyRep::Dict(vec![(
                        PyRep::String(("stationID").into()),
                        PyRep::Tuple(vec![
                            old_hq.map_or(PyRep::None, PyRep::Integer),
                            PyRep::Integer(station_id),
                        ]),
                    )]);
                    ctx.notify(
                        NotificationTarget::Corporation(corp_id),
                        "OnCorporationChanged",
                        "corpid",
                        vec![PyRep::Integer(corp_id), change],
                    );
                    Some(Ok(PyRep::keyval([
                        ("corporationID", PyRep::Integer(corp_id)),
                        ("stationID", PyRep::Integer(station_id)),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }
        ("corpmgr", "GetAssetInventory")
        | ("corpmgr", "GetAssetInventoryForLocation")
        | ("corpmgr", "SearchAssets") => {
            let (req_corp_id, loc_filter, which_filter) = match method {
                "GetAssetInventory" => (
                    args.first().and_then(|a| a.as_i64()).unwrap_or(corp_id),
                    None,
                    args.get(1).and_then(|a| a.as_str()),
                ),
                "GetAssetInventoryForLocation" => {
                    if args.len() >= 2 && args.get(1).and_then(|a| a.as_i64()).is_some() {
                        (
                            args.first().and_then(|a| a.as_i64()).unwrap_or(corp_id),
                            args.get(1).and_then(|a| a.as_i64()),
                            args.get(2).and_then(|a| a.as_str()),
                        )
                    } else {
                        (
                            corp_id,
                            args.first().and_then(|a| a.as_i64()),
                            args.get(1).and_then(|a| a.as_str()),
                        )
                    }
                }
                _ => (corp_id, None, args.first().and_then(|a| a.as_str())),
            };
            let cat_filter = if method == "SearchAssets" {
                args.get(1).and_then(|a| a.as_i64()).filter(|&v| v > 0)
            } else { None };
            let grp_filter = if method == "SearchAssets" {
                args.get(2).and_then(|a| a.as_i64()).filter(|&v| v > 0)
            } else { None };
            let type_filter = if method == "SearchAssets" {
                args.get(3).and_then(|a| a.as_i64()).filter(|&v| v > 0)
            } else { None };
            let min_qty_filter = if method == "SearchAssets" {
                args.get(4).and_then(|a| a.as_i64()).filter(|&v| v > 0)
            } else { None };
            let res = ctx.db.with_snapshot(|snap| {
                let Some(corp) = snap.corporations.get(&req_corp_id) else {
                    return Err(BridgeRpcError {
                        code: "CorporationNotFound".into(),
                        message: format!("Corporation {req_corp_id} not found"),
                    });
                };
                let caller_role = corp.member_roles.get(&char_id).copied().unwrap_or(0);
                let allowed_roles = corp_roles::ROLE_DIRECTOR
                    | corp_roles::ROLE_ACCOUNTANT
                    | corp_roles::ROLE_JUNIOR_ACCOUNTANT
                    | corp_roles::ROLE_TRADER;
                if char_id != corp.ceo_id
                    && caller_role != u64::MAX
                    && (caller_role & allowed_roles) == 0
                {
                    return Err(BridgeRpcError {
                        code: "CorpRoleDenied".into(),
                        message: "Viewing corporation asset inventory requires Director, Accountant, or Trader role".into(),
                    });
                }
                let rows: Vec<Vec<PyRep>> = snap
                    .items
                    .values()
                    .filter(|i| {
                        let flag_id = i.flag as i64;
                        let which_ok = match which_filter {
                            Some("deliveries") => {
                                i.flag == eve_db::InventoryFlag::Deliveries
                                    || i.flag == eve_db::InventoryFlag::CorpDeliveries
                            }
                            Some("lockdown") => flag_id == 63,
                            Some("impounded") | Some("junk") => flag_id == 72,
                            Some("offices") => (116..=122).contains(&flag_id) || flag_id == 4,
                            _ => {
                                i.flag != eve_db::InventoryFlag::Deliveries
                                    && i.flag != eve_db::InventoryFlag::CorpDeliveries
                                    && flag_id != 63
                                    && flag_id != 72
                            }
                        };
                        let sde_type = ctx.sde.types.get(&i.type_id);
                        let cat_ok = cat_filter.is_none_or(|cid| sde_type.is_some_and(|t| t.category_id as i64 == cid));
                        let grp_ok = grp_filter.is_none_or(|gid| sde_type.is_some_and(|t| t.group_id as i64 == gid));
                        i.owner_id == req_corp_id
                            && which_ok
                            && loc_filter.is_none_or(|loc| i.location_id == loc)
                            && type_filter.is_none_or(|tid| i.type_id as i64 == tid)
                            && min_qty_filter.is_none_or(|mq| i.quantity as i64 >= mq)
                            && cat_ok
                            && grp_ok
                    })
                    .map(|i| {
                        let loc_id_u32 = i.location_id as u32;
                        let solar_system_id = ctx.sde.stations.get(&(loc_id_u32 as i64))
                            .map(|st| st.solar_system_id as i64)
                            .or_else(|| ctx.sde.stations.get(&(loc_id_u32 as i64)).map(|st| st.solar_system_id as i64))
                            .or_else(|| snap.structures.get(&i.location_id).map(|st| st.solar_system_id as i64))
                            .or_else(|| (30_000_000..=33_000_000).contains(&i.location_id).then_some(i.location_id))
                            .unwrap_or(30_000_142);
                        vec![
                            PyRep::Integer(i.item_id),
                            PyRep::Integer(i.type_id as i64),
                            PyRep::Integer(i.owner_id),
                            PyRep::Integer(i.location_id),
                            PyRep::Integer(solar_system_id),
                            PyRep::Integer(i.flag as i64),
                            PyRep::Integer(i.quantity as i64),
                        ]
                    })
                    .collect();
                Ok(rows)
            });
            match res {
                Ok(rows) => Some(Ok(PyRep::rowset(
                    vec!["itemID", "typeID", "ownerID", "locationID", "solarsystemID", "flagID", "quantity"],
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
            let (ff, pending_val, pending_until) = with_corp_gov(ctx, |g| {
                if let Some(s) = g.get_mut(&target_corp_id) {
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
                }
            });
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
            with_corp_gov(ctx, |g| {
                let state = g.entry(corp_id).or_default();
                state.pending_friendly_fire = Some((ff, effective_at));
            });
            let settings_kv = PyRep::keyval([
                ("corporationID", PyRep::Integer(corp_id)),
                ("isFriendlyFireLegal", PyRep::Bool(ff)),
                ("changeAtTime", PyRep::Integer(effective_at as i64)),
            ]);
            ctx.notify(
                NotificationTarget::Corporation(corp_id),
                "OnCorpAggressionSettingsChange",
                "corpid",
                vec![settings_kv.clone()],
            );
            Some(Ok(settings_kv))
        }
        _ => None,
    }
}
