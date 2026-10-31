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
pub use fleet_social_calendar_ops::create_ccp_calendar_event;
pub(crate) use fleet_social_calendar_ops::*;
#[path = "fleet_social_chat.rs"]
pub(crate) mod fleet_social_chat;
#[path = "fleet_social_corp_gov.rs"]
mod fleet_social_corp_gov;
#[path = "fleet_social_corp_tail.rs"]
pub mod fleet_social_corp_tail;
#[path = "fleet_social_corp_tail_dispatch.rs"]
pub(crate) mod fleet_social_corp_tail_dispatch;
#[path = "fleet_social_corps.rs"]
pub mod fleet_social_corps;
#[path = "fleet_social_finder_tactical.rs"]
pub mod fleet_social_finder_tactical;
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
pub(crate) mod fleet_social_scanning;

pub use fleet_social_notify::stream_fleet_watchlist_damage;
pub use fleet_social_persistence::{hydrate_social_state_from_db, persist_social_state};
pub use fleet_social_scanning::build_signal_tracker_signature_kv;
pub(crate) use fleet_social_persistence::update_calendar_response_from_esi;
pub use fleet_social_corp_tail::{
    execute_pending_auto_kicks_for_docked_char, execute_pending_auto_kicks_on_downtime,
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

pub(crate) fn is_bookmark_expired_in_snap(
    snap: &eve_db::WorldStateSnapshot,
    bookmark_id: i64,
    now_unix: u64,
) -> bool {
    snap.bookmark_expiries.get(&bookmark_id).is_some_and(|&exp| exp > 0 && now_unix >= exp)
}

pub(crate) fn is_bookmark_expired(ctx: &ServerContext, bookmark_id: i64, now_unix: u64) -> bool {
    ctx.db.with_snapshot(|snap| is_bookmark_expired_in_snap(snap, bookmark_id, now_unix))
}

/// Sweep all expired bookmarks (`expiry_unix > 0 && now_unix >= expiry_unix`) from `snap.bookmarks`
/// and `snap.bookmark_subfolders` while retaining `snap.bookmark_expiries` (persisted in SQLite).
pub(crate) fn sweep_expired_bookmarks(snap: &mut eve_db::WorldStateSnapshot, now_unix: u64) -> Vec<i64> {
    let mut expired_ids = Vec::new();
    let expiries = &snap.bookmark_expiries;
    snap.bookmarks.retain(|id, _| {
        let exp = expiries.get(id).copied().unwrap_or(0);
        let keep = exp == 0 || now_unix < exp;
        if !keep {
            expired_ids.push(*id);
        }
        keep
    });
    for id in &expired_ids {
        snap.bookmark_subfolders.remove(id);
    }
    expired_ids
}

/// Sweeps expired bookmarks in `ctx.db` and emits `OnRefreshBookmarks` / `OnSharedBookmarksFolderUpdated`.
pub(crate) fn sweep_expired_bookmarks_and_notify(ctx: &ServerContext, now_unix: u64) -> Vec<i64> {
    let (expired_ids, owners, shared_folder_bms) = ctx.db.with_mut_snapshot(|snap| {
        let mut owners = std::collections::BTreeSet::new();
        let mut shared_folder_bms: HashMap<(i64, i64), Vec<i64>> = HashMap::new();
        for (&id, bm) in &snap.bookmarks {
            if snap.bookmark_expiries.get(&id).is_some_and(|&exp| exp > 0 && now_unix >= exp) {
                owners.insert(bm.owner_id);
                if bm.is_shared {
                    let fid = bm.folder_id.unwrap_or(0);
                    let corp_opt = bm.folder_id.and_then(|f| snap.bookmark_folders.get(&f)).and_then(|f| f.corporation_id)
                        .or_else(|| snap.characters.get(&bm.owner_id).map(|c| c.corporation_id));
                    if let Some(cid) = corp_opt {
                        shared_folder_bms.entry((cid, fid)).or_default().push(id);
                    }
                }
            }
        }
        (sweep_expired_bookmarks(snap, now_unix), owners, shared_folder_bms)
    });
    for owner_id in owners {
        ctx.notify(NotificationTarget::Character(owner_id), "OnRefreshBookmarks", "charid", vec![]);
    }
    for ((corp_id, fid), bms) in shared_folder_bms {
        let args = fleet_social_access_group_bookmarks::acl_folder_updates_arg(
            fid,
            fleet_social_access_group_bookmarks::BOOKMARKS_REMOVED,
            PyRep::List(bms.into_iter().map(PyRep::Integer).collect()),
        );
        ctx.notify(NotificationTarget::Corporation(corp_id), "OnSharedBookmarksFolderUpdated", "corpid", args);
    }
    expired_ids
}

pub(crate) fn get_effective_corp_roles(
    ctx: &ServerContext,
    corp_id: i64,
    char_id: i64,
) -> (bool, u64) {
    fleet_social_corps::promote_due_pending_corp_grants(ctx, corp_id);
    let now_unix = ctx.now_unix();
    ctx.db.with_snapshot(|snap| {
        crate::services::social_corp_fleet::social_corp_projects::effective_corp_member_roles(
            snap, corp_id, char_id, 0, now_unix,
        )
    })
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
    let (_, char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let corp_id = session
        .corporation_id
        .or_else(|| ctx.db.with_snapshot(|snap| snap.characters.get(&char_id).map(|c| c.corporation_id)))
        .unwrap_or(0);
    let default_station_id = session
        .station_id
        .or_else(|| ctx.db.with_snapshot(|snap| snap.characters.get(&char_id).and_then(|c| c.station_id)))
        .unwrap_or(0);

    match method {
        "Bind" | "SetSessionCheck" => Some(Ok(PyRep::None)),

        "GetPriceQuote" => {
            let station_id = args.first().and_then(|a| a.as_i64()).unwrap_or(default_station_id);
            let occupied = ctx.db.with_snapshot(|snap| {
                snap.items.values().filter(|i| i.location_id == station_id && i.type_id == 27).count()
            });
            let quote = 10_000.0 * (1.0 + (occupied as f64) * 0.5);
            Some(Ok(PyRep::Real(quote)))
        }

        "RentOffice" => {
            let station_id = args.first().and_then(|a| a.as_i64()).unwrap_or(default_station_id);
            let (is_ceo, roles) = get_effective_corp_roles(ctx, corp_id, char_id);
            let effective_roles = session.corp_role | roles;
            if !is_ceo && !eve_db::corp_roles::has_role(effective_roles, eve_db::corp_roles::ROLE_RENT_OFFICE) {
                return Some(Err(BridgeRpcError {
                    code: "CorpRoleDenied".into(),
                    message: "Renting an office requires Director or Rent Office role".into(),
                }));
            }

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                if station_id <= 0
                    || (station_id >= 1_000_000_000_000
                        && !snap.structures.contains_key(&station_id))
                {
                    return Err(BridgeRpcError {
                        code: "StationNotFound".into(),
                        message: format!("Station or structure {station_id} not found"),
                    });
                }
                if station_id == 60_015_187 {
                    // Corporation offices at The Fulcrum require the renting corporation itself
                    // to be enlisted in a pirate insurgency militia (`500_010` Guristas | `500_011` Angels).
                    let corp_fac = snap
                        .fw_runtime
                        .corp_war_factions
                        .get(&corp_id)
                        .copied()
                        .or_else(|| {
                            ctx.sde
                                .npc_corporations
                                .get(&corp_id)
                                .and_then(|c| c.faction_id.map(i64::from))
                        });
                    if !matches!(corp_fac, Some(500_010 | 500_011)) {
                        return Err(BridgeRpcError {
                            code: "FulcrumOfficeRequiresPirateEnlistment".into(),
                            message: "Renting a corporation office at The Fulcrum requires a pirate-enlisted corporation".into(),
                        });
                    }
                }
                let Some(corp) = snap.corporations.get_mut(&corp_id) else {
                    return Err(BridgeRpcError {
                        code: "CorporationNotFound".into(),
                        message: format!("Corporation {corp_id} not found"),
                    });
                };
                let occupied = snap.items.values().filter(|i| i.location_id == station_id && i.type_id == 27).count();
                if occupied >= 16 {
                    return Err(BridgeRpcError {
                        code: "StationOfficesFull".into(),
                        message: format!("Station {station_id} has reached the 16-office capacity limit"),
                    });
                }
                let already_rented = snap.items.values().any(|i| i.owner_id == corp_id && i.location_id == station_id && i.type_id == 27);
                if already_rented {
                    return Err(BridgeRpcError {
                        code: "OfficeAlreadyRented".into(),
                        message: format!("Corporation {corp_id} already rents an office at station {station_id}"),
                    });
                }
                let quote = 10_000.0 * (1.0 + (occupied as f64) * 0.5);
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

                let office_id = snap.items.keys().max().copied().unwrap_or(920_000_000).max(920_000_000) + 1;
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
                snap.corp_governance.entry(corp_id).or_default().offices.insert(station_id, office_id);
                Ok((office_id, station_id, quote))
            });

            match res {
                Ok((office_id, st_id, quote)) => {
                    let due_unix = ctx.now_unix().saturating_add(30 * 86_400);
                    let _ = super::wallet_serverops::create_bill(ctx, 2, corp_id, 1_000_035, quote, due_unix, 0.0, st_id, 27);
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
                        ("cost", PyRep::Real(quote)),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        "GetMyCorporationsOffice" => {
            let station_id = args
                .first()
                .and_then(|a| a.as_i64())
                .unwrap_or(default_station_id);

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
                .unwrap_or(default_station_id);

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
                .unwrap_or(default_station_id);

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
                .unwrap_or(default_station_id);
            let (is_ceo, roles) = get_effective_corp_roles(ctx, corp_id, char_id);
            let effective_roles = session.corp_role | roles;
            if !is_ceo
                && !eve_db::corp_roles::has_role(
                    effective_roles,
                    eve_db::corp_roles::ROLE_RENT_OFFICE,
                )
            {
                return Some(Err(BridgeRpcError {
                    code: "CorpRoleDenied".into(),
                    message: "Unrenting an office requires Director or Rent Office role".into(),
                }));
            }

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
                if let Some(gov) = snap.corp_governance.get_mut(&corp_id) {
                    gov.offices.remove(&station_id);
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
        let pos3 = |x: f64, y: f64, z: f64| PyRep::Tuple(vec![PyRep::Real(x), PyRep::Real(y), PyRep::Real(z)]);
        return Some(Ok(PyRep::Tuple(vec![
            PyRep::Tuple(vec![PyRep::ascii_bytes("Diamond"), PyRep::Tuple(vec![pos3(100.0, 0.0, 0.0), pos3(-100.0, 0.0, 0.0), pos3(0.0, 100.0, 0.0), pos3(0.0, -100.0, 0.0)])]),
            PyRep::Tuple(vec![PyRep::ascii_bytes("Arrow"), PyRep::Tuple(vec![pos3(100.0, 0.0, -50.0), pos3(-100.0, 0.0, -50.0), pos3(200.0, 0.0, -100.0), pos3(-200.0, 0.0, -100.0)])]),
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
            if new_wing_id <= 0 || !fleet.wing_details.contains_key(&new_wing_id) {
                return Err(BridgeRpcError {
                    code: "WingNotFound".into(),
                    message: format!("Wing {new_wing_id} not found"),
                });
            }
            let sq = if new_squad_id <= 0 { None } else { fleet.squad_details.get(&new_squad_id) }
                .ok_or_else(|| BridgeRpcError {
                    code: "SquadNotFound".into(),
                    message: format!("Squad {new_squad_id} not found"),
                })?;
            if sq.wing_id != new_wing_id {
                return Err(BridgeRpcError {
                    code: "SquadNotFound".into(),
                    message: format!("Squad {new_squad_id} does not belong to wing {new_wing_id}"),
                });
            }
            let is_only_member = fleet.member_details.len() == 1;
            let mut changes = Vec::new();
            for tid in &char_ids {
                if let Some(m) = fleet.member_details.get_mut(tid) {
                    let before = m.clone();
                    let old_wing = m.wing_id;
                    let old_squad = m.squad_id;
                    let old_role = m.role;
                    m.wing_id = Some(new_wing_id);
                    m.squad_id = Some(new_squad_id);
                    m.role = FleetRole::SquadMember;
                    m.role_name = "Squad Member".into();
                    changes.push((before, m.clone()));

                    if old_role == FleetRole::WingCommander {
                        if let Some(w) = old_wing.and_then(|w| fleet.wing_details.get_mut(&w)) {
                            if w.commander_id == Some(*tid) { w.commander_id = None; }
                        }
                    }
                    if old_role == FleetRole::SquadCommander {
                        if let Some(s) = old_squad.and_then(|s| fleet.squad_details.get_mut(&s)) {
                            if s.commander_id == Some(*tid) { s.commander_id = None; }
                        }
                    }
                }
            }
            Ok((changes, is_only_member))
        });
        return match res {
            Ok((changes, is_only_member)) => {
                let moved = changes.len() as i64;
                for (old_m, new_m) in &changes {
                    fleet_social_notify::emit_member_changed(ctx, fleet_id, old_m, new_m, is_only_member);
                }
                Some(Ok(PyRep::keyval([("movedCount", PyRep::Integer(moved))])))
            }
            Err(e) => Some(Err(e)),
        };
    }
    if matches!(service, "fleetObjectHandler" | "fleetMgr") && method == "MakeLeader" {
        // parity: feat:fleet.boss-handover
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

    if let Some(res) = fleet_social_fleets::handle_fleets(ctx, session, service, method, args, kwargs).await { return Some(res); }
    if let Some(res) = fleet_social_corps::handle_corps(ctx, session, service, method, args, kwargs) { return Some(res); }
    if let Some(res) = fleet_social_chat::handle_chat_and_social(ctx, session, service, method, args, kwargs) { return Some(res); }
    if let Some(res) = fleet_social_mail::handle_mail_and_calendar(ctx, session, service, method, args, kwargs) { return Some(res); }
    if service == "accessGroupBookmarkMgr" { return fleet_social_access_group_bookmarks::handle(ctx, session, service, method, args, kwargs).await; }
    if let Some(res) = fleet_social_bookmarks::handle_bookmarks(ctx, session, service, method, args, kwargs).await { return Some(res); }
    fleet_social_scanning::handle_scanning_and_wormholes(ctx, session, service, method, args, kwargs).await
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
    let (is_voice_enabled, is_loot_logging) =
        fleet_social_notify::get_fleet_options(ctx, fleet_id);
    let (fleet_name, max_size, auto_join_squad, target_tags_dict) = {
        let ex = ctx.fleet_extras.lock().unwrap();
        let tags = ex
            .target_tags
            .get(&fleet_id)
            .map(|m| {
                m.iter()
                    .map(|(&tid, tag)| (PyRep::Integer(tid), PyRep::String(tag.clone())))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        (
            ex.fleet_names
                .get(&fleet_id)
                .cloned()
                .unwrap_or_else(|| "New Eden Taskforce".into()),
            ex.max_sizes.get(&fleet_id).copied().unwrap_or(256),
            ex.auto_join_squads.get(&fleet_id).copied().flatten(),
            tags,
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
            ("targetTags", PyRep::Dict(target_tags_dict)),
        ]))
    });
    if let Err(e) = &res {
        if e.code == "NotInFleet" || e.code == "FleetNotFound" {
            session.fleet_id = None;
        }
    }
    Some(res)
}

pub(super) use fleet_social_finder_tactical::handle_fleet_watchlist_ops;


