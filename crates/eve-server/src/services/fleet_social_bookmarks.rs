//! Legacy bookmarks (`SO-4`: `bookmark`, `beyonce`).

use super::fleet_social_access_group_bookmarks::{folder_access_level, AccessLevel};
use super::fleet_social_access_group_bookmarks_ops::acl_active_bookmarks;
use super::fleet_social_chat::extract_id_list;
use super::{is_bookmark_expired_in_snap, sweep_expired_bookmarks_and_notify};
use crate::router::{cache_notifications::NotificationTarget, ServerContext};
use eve_db::{corp_roles, BookmarkFolderRecord, BookmarkRecord};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;


pub(crate) fn shared_bookmark_corp_id(
    snap: &eve_db::WorldStateSnapshot,
    bm: &BookmarkRecord,
) -> Option<i64> {
    bm.folder_id
        .and_then(|fid| snap.bookmark_folders.get(&fid))
        .and_then(|f| f.corporation_id)
        .or_else(|| snap.characters.get(&bm.owner_id).map(|c| c.corporation_id))
}

pub(crate) fn shared_folder_updates_arg(folder_id: i64, update_type: &str, update_args: PyRep) -> Vec<PyRep> {
    vec![PyRep::List(vec![PyRep::Tuple(vec![
        PyRep::Tuple(vec![PyRep::Integer(folder_id)]),
        PyRep::String(update_type.to_string()),
        update_args,
    ])])]
}

pub(crate) fn has_corp_bookmark_admin_role(
    snap: &eve_db::WorldStateSnapshot,
    session: &SessionState,
    char_id: i64,
    corp_id: i64,
) -> bool {
    if let Some(corp) = snap.corporations.get(&corp_id) {
        let role = corp.member_roles.get(&char_id).copied().unwrap_or(0);
        char_id == corp.ceo_id
            || corp_roles::has_role(role, corp_roles::ROLE_DIRECTOR)
            || corp_roles::has_role(role, corp_roles::ROLE_PERSONNEL_MANAGER)
    } else {
        corp_roles::has_role(session.corp_role, corp_roles::ROLE_DIRECTOR)
            || corp_roles::has_role(session.corp_role, corp_roles::ROLE_PERSONNEL_MANAGER)
    }
}

pub(super) async fn handle_bookmarks(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        // =====================================================================
        ("bookmark", "BookmarkLocation")
        | ("beyonce", "BookmarkLocation") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let label = args
                .first()
                .and_then(|a| a.as_str())
                .or_else(|| args.get(2).and_then(|a| a.as_str()))
                .unwrap_or("Safe Spot")
                .trim();
            if label.is_empty() || label.chars().count() > 100 {
                return Some(Err(BridgeRpcError {
                    code: "InvalidBookmarkLabel".into(),
                    message: "Bookmark label must be 1..=100 non-whitespace characters".into(),
                }));
            }
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "Session has no active solar system".into(),
                }));
            };
            if sys_id == 30_100_000 {
                return Some(Err(BridgeRpcError::user_error("CantbookmarkEveSystem")));
            }

            let (explicit_entity_id, memo, folder_id) = if args.first().and_then(|a| a.as_i64()).is_some() {
                (
                    args.first().and_then(|a| a.as_i64()),
                    args.get(3).and_then(|a| a.as_str()).unwrap_or("").to_string(),
                    args.get(1)
                        .and_then(|a| a.as_i64())
                        .or_else(|| _kwargs.get("subfolderID").and_then(|v| v.as_i64())),
                )
            } else if args.get(1).and_then(|a| a.as_str()).is_some() {
                (
                    args.get(3).and_then(|a| a.as_i64()),
                    args.get(1).and_then(|a| a.as_str()).unwrap_or("").to_string(),
                    args.get(2).and_then(|a| a.as_i64()),
                )
            } else {
                (
                    args.get(1).and_then(|a| a.as_i64()),
                    args.get(2).and_then(|a| a.as_str()).unwrap_or("").to_string(),
                    args.get(3).and_then(|a| a.as_i64()),
                )
            };
            let (subfolder_id, expiry_hours, _type_id, _location_id) = if service == "beyonce" {
                (
                    _kwargs.get("subfolderID").and_then(|v| v.as_i64()),
                    _kwargs
                        .get("expiryHours")
                        .or_else(|| _kwargs.get("expiry"))
                        .and_then(|v| v.as_i64())
                        .or_else(|| {
                            if args.len() == 5 {
                                args.get(4).and_then(|a| a.as_i64())
                            } else {
                                None
                            }
                        })
                        .unwrap_or(0),
                    args.get(4).and_then(|a| a.as_i64()).unwrap_or(5),
                    args.get(5)
                        .and_then(|a| a.as_i64())
                        .or_else(|| _kwargs.get("locationID").and_then(|v| v.as_i64()))
                        .unwrap_or(sys_id as i64),
                )
            } else {
                (args.get(4).and_then(|a| a.as_i64()).or_else(|| _kwargs.get("subfolderID").and_then(|v| v.as_i64())), args.get(5).and_then(|a| a.as_i64()).unwrap_or(0), 5, sys_id as i64)
            };
            // Validate bookmark folder access. Folder 0 or char_id represents
            // the pilot's root personal folder ("My Bookmarks"). Custom folders must exist
            // and belong to either the caller or their corporation (if shared).
            if let Some(fid) = folder_id {
                if fid != 0 && fid != char_id {
                    let ok = ctx.db.with_snapshot(|s| {
                        s.bookmark_folders.get(&fid).is_some_and(|f| {
                            f.owner_id == char_id || (f.is_shared && session.corporation_id == Some(f.owner_id))
                        })
                    });
                    if !ok {
                        return Some(Err(BridgeRpcError {
                            code: "FolderNotFound".into(),
                            message: format!("Folder {fid} not accessible"),
                        }));
                    }
                }
            }
            // Game mechanics: derive actual type_id and location_id from server-side state
            // rather than trusting raw client arguments, preventing type spoofing.
            let (derived_type_id, derived_loc_id) = if let Some(ent_id) = explicit_entity_id {
                ctx.db.with_snapshot(|s| {
                    s.items.get(&ent_id).map(|it| (it.type_id as i64, it.location_id))
                        .or_else(|| s.structures.get(&ent_id).map(|st| (st.type_id as i64, st.solar_system_id as i64)))
                }).unwrap_or((5, sys_id as i64))
            } else {
                (5, sys_id as i64)
            };

            let (x, y, z) = if let Some(ent_id) = explicit_entity_id {
                let sys_handle = ctx.systems.get_or_spawn(sys_id);
                if let Ok(Some((_, ball))) = sys_handle.get_ball_snapshot(ent_id).await {
                    (ball.position.x, ball.position.y, ball.position.z)
                } else if let Some(st) = ctx
                    .sde
                    .solar_systems
                    .get(&sys_id)
                    .and_then(|s| s.stations.iter().find(|st| st.station_id == ent_id))
                {
                    (st.position.x, st.position.y, st.position.z)
                } else {
                    (0.0, 0.0, 0.0)
                }
            } else if let Some(st_id) = session.station_id {
                ctx.sde
                    .solar_systems
                    .get(&sys_id)
                    .and_then(|s| s.stations.iter().find(|st| st.station_id == st_id))
                    .map(|st| (st.position.x, st.position.y, st.position.z))
                    .unwrap_or((0.0, 0.0, 0.0))
            } else if let Some(struct_id) = session.structure_id {
                ctx.db
                    .with_snapshot(|snap| {
                        snap.structures
                            .get(&struct_id)
                            .map(|s| (s.position[0], s.position[1], s.position[2]))
                    })
                    .unwrap_or((0.0, 0.0, 0.0))
            } else if let Some(ship_id) = session.ship_id {
                let handle = ctx.systems.get_or_spawn(sys_id);
                if let Ok(Some((_, ball))) = handle.get_ball_snapshot(ship_id).await {
                    (ball.position.x, ball.position.y, ball.position.z)
                } else {
                    return Some(Err(BridgeRpcError {
                        code: "ShipNotInSpace".into(),
                        message: "Cannot bookmark in-space position when ship ball is absent".into(),
                    }));
                }
            } else {
                (0.0, 0.0, 0.0)
            };

            let now_unix = ctx.now_unix();
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let is_shared_folder = folder_id
                    .and_then(|fid| snap.bookmark_folders.get(&fid))
                    .map(|f| f.is_shared)
                    .unwrap_or(false);
                if is_shared_folder {
                    let shared_count = snap
                        .bookmarks
                        .values()
                        .filter(|b| b.folder_id == folder_id)
                        .count();
                    if shared_count >= 500 {
                        return Err(BridgeRpcError {
                            code: "MaxSharedFolderBookmarksReached".into(),
                            message: "Shared folder has reached the maximum of 500 bookmarks".into(),
                        });
                    }
                } else {
                    let count = snap
                        .bookmarks
                        .values()
                        .filter(|b| b.owner_id == char_id && !b.is_shared)
                        .count();
                    if count >= 500 {
                        return Err(BridgeRpcError {
                            code: "MaxBookmarksReached".into(),
                            message: "Character has reached the maximum of 500 personal bookmarks".into(),
                        });
                    }
                }
                snap.next_entity_id += 1;
                let id = snap.next_entity_id;
                snap.bookmarks.insert(
                    id,
                    BookmarkRecord {
                        bookmark_id: id,
                        owner_id: char_id,
                        label: label.to_string(),
                        solar_system_id: sys_id,
                        x,
                        y,
                        z,
                        item_id: explicit_entity_id,
                        memo,
                        folder_id,
                        is_shared: is_shared_folder,
                    },
                );
                if expiry_hours > 0 {
                    snap.bookmark_expiries
                        .insert(id, now_unix + (expiry_hours as u64 * 3600));
                }
                if let Some(sfid) = subfolder_id {
                    snap.bookmark_subfolders.insert(id, sfid);
                }
                let corp_id = session
                    .corporation_id
                    .or_else(|| snap.characters.get(&char_id).map(|c| c.corporation_id));
                Ok((id, is_shared_folder, corp_id))
            });

            match res {
                Ok((bm_id, is_shared_folder, corp_id)) => {
                    if is_shared_folder {
                        if let Some(cid) = corp_id {
                            ctx.notify(
                                NotificationTarget::Corporation(cid),
                                "OnSharedBookmarksFolderUpdated",
                                "corpid",
                                shared_folder_updates_arg(
                                    folder_id.unwrap_or(0),
                                    "BookmarksCreated",
                                    PyRep::List(vec![PyRep::Integer(bm_id)]),
                                ),
                            );
                        }
                    }
                    if service == "beyonce" {
                        Some(Ok(PyRep::Tuple(vec![PyRep::Integer(bm_id), explicit_entity_id.map(PyRep::Integer).unwrap_or(PyRep::Integer(sys_id as i64)), PyRep::Integer(derived_type_id), PyRep::Real(x), PyRep::Real(y), PyRep::Real(z), PyRep::Integer(derived_loc_id), folder_id.map(PyRep::Integer).unwrap_or(PyRep::None)])))
                    } else {
                        Some(Ok(PyRep::keyval([("bookmarkID", PyRep::Integer(bm_id)), ("label", PyRep::String(label.to_string())), ("x", PyRep::Real(x)), ("y", PyRep::Real(y)), ("z", PyRep::Real(z))])))
                    }
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("bookmark", "GetBookmarks") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let now_unix = ctx.now_unix();
            sweep_expired_bookmarks_and_notify(ctx, now_unix);
            let rows = ctx.db.try_with_mut_snapshot(|snap| {
                let corp_id_opt = session
                    .corporation_id
                    .or_else(|| snap.characters.get(&char_id).map(|c| c.corporation_id));
                let list: Vec<Vec<PyRep>> = snap
                    .bookmarks
                    .values()
                    .filter(|b| {
                        if is_bookmark_expired_in_snap(snap, b.bookmark_id, now_unix) {
                            return false;
                        }
                        b.owner_id == char_id
                            || (b.is_shared
                                && corp_id_opt.is_some()
                                && shared_bookmark_corp_id(snap, b) == corp_id_opt)
                    })
                    .map(|b| {
                        let sf_opt = snap
                            .bookmark_subfolders
                            .get(&b.bookmark_id)
                            .copied();
                        vec![
                            PyRep::Integer(b.bookmark_id),
                            PyRep::String(b.label.clone()),
                            PyRep::Integer(b.solar_system_id as i64),
                            PyRep::Real(b.x),
                            PyRep::Real(b.y),
                            PyRep::Real(b.z),
                            b.item_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                            PyRep::String(b.memo.clone()),
                            b.folder_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                            PyRep::Bool(b.is_shared),
                            sf_opt.map(PyRep::Integer).unwrap_or(PyRep::None),
                        ]
                    })
                    .collect();
                Ok::<_, BridgeRpcError>(list)
            }).unwrap_or_default();
            const BM_COLUMNS: [&str; 11] = [
                "bookmarkID", "label", "solarSystemID", "x", "y", "z", "itemID", "memo", "folderID", "isShared",
                "subfolderID",
            ];
            if method == "GetMyActiveBookmarks" {
                return Some(Ok(acl_active_bookmarks(ctx, session, char_id, &BM_COLUMNS, rows)));
            }
            Some(Ok(PyRep::rowset(BM_COLUMNS.to_vec(), rows)))
        }

        ("bookmark", "UpdateBookmark") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let Some(bm_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "UpdateBookmark requires bookmarkID".into(),
                }));
            };
            let label_opt = args.get(1).and_then(|a| a.as_str());
            let memo_opt = args.get(2).and_then(|a| a.as_str());
            let folder_id_opt = args.get(3).and_then(|a| a.as_i64());
            let expiry_hours_opt = args.get(4).and_then(|a| a.as_i64());

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let corp_id = session
                    .corporation_id
                    .or_else(|| snap.characters.get(&char_id).map(|c| c.corporation_id));
                let bm_corp = snap
                    .bookmarks
                    .get(&bm_id)
                    .and_then(|b| shared_bookmark_corp_id(snap, b));
                let has_manage_access = corp_id.is_some_and(|cid| {
                    bm_corp == Some(cid)
                        && has_corp_bookmark_admin_role(snap, session, char_id, cid)
                });
                let target_folder_shared = if let Some(fid) = folder_id_opt {
                    if let Some(tf) = snap.bookmark_folders.get(&fid) {
                        if tf.owner_id != char_id
                            && folder_access_level(snap, session, char_id, tf) < AccessLevel::Use
                        {
                            return Err(BridgeRpcError {
                                code: "AccessDenied".into(),
                                message: "Cannot move bookmark into a folder without use/add access".into(),
                            });
                        }
                        Some(tf.is_shared)
                    } else if fid != 0 && fid != char_id {
                        return Err(BridgeRpcError {
                            code: "FolderNotFound".into(),
                            message: format!("Target folder {fid} not found"),
                        });
                    } else {
                        Some(false)
                    }
                } else {
                    None
                };
                let Some(bm) = snap.bookmarks.get_mut(&bm_id) else {
                    return Err(BridgeRpcError {
                        code: "BookmarkNotFound".into(),
                        message: format!("Bookmark {bm_id} not found"),
                    });
                };
                if bm.owner_id != char_id && !(bm.is_shared && has_manage_access) {
                    return Err(BridgeRpcError {
                        code: "AccessDenied".into(),
                        message: "Cannot edit another character's bookmark without manage/admin rights".into(),
                    });
                }
                if let Some(l) = label_opt {
                    bm.label = l.to_string();
                }
                if let Some(m) = memo_opt {
                    bm.memo = m.to_string();
                }
                if let Some(fid) = folder_id_opt {
                    bm.folder_id = Some(fid);
                }
                if let Some(shared) = target_folder_shared {
                    bm.is_shared = shared;
                }
                let (is_shared, fid) = (bm.is_shared, bm.folder_id);
                let exp_unix = if let Some(eh) = expiry_hours_opt {
                    if eh > 0 {
                        let exp = ctx.now_unix_secs() + (eh as u64) * 3600;
                        snap.bookmark_expiries.insert(bm_id, exp);
                        exp
                    } else {
                        snap.bookmark_expiries.remove(&bm_id);
                        0
                    }
                } else {
                    snap.bookmark_expiries.get(&bm_id).copied().unwrap_or(0)
                };
                Ok((is_shared, fid, corp_id, exp_unix))
            });
            match res {
                Ok((is_shared, folder_id, corp_id, expiry_unix)) => {
                    ctx.notify(
                        NotificationTarget::Character(char_id),
                        "OnRefreshBookmarks",
                        "charid",
                        vec![PyRep::Integer(bm_id)],
                    );
                    if is_shared {
                        if let Some(cid) = corp_id {
                            ctx.notify(
                                NotificationTarget::Corporation(cid),
                                "OnSharedBookmarksFolderUpdated",
                                "corpid",
                                shared_folder_updates_arg(
                                    folder_id.unwrap_or(0),
                                    "BookmarkUpdated",
                                    PyRep::Integer(bm_id),
                                ),
                            );
                        }
                    }
                    Some(Ok(PyRep::keyval([
                        ("bookmarkID", PyRep::Integer(bm_id)),
                        ("updated", PyRep::Bool(true)),
                        ("expiryUnix", PyRep::Integer(expiry_unix as i64)),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("bookmark", "DeleteBookmarks") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let bm_ids = extract_id_list(args.first());
            if bm_ids.is_empty() {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "DeleteBookmarks requires bookmarkID".into(),
                }));
            }
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let corp_id = session
                    .corporation_id
                    .or_else(|| snap.characters.get(&char_id).map(|c| c.corporation_id));
                let mut any_shared_folder = None;
                for &bm_id in &bm_ids {
                    let Some(bm) = snap.bookmarks.get(&bm_id) else {
                        return Err(BridgeRpcError {
                            code: "BookmarkNotFound".into(),
                            message: format!("Bookmark {bm_id} not found"),
                        });
                    };
                    let bm_corp = shared_bookmark_corp_id(snap, bm);
                    let has_manage_access = corp_id.is_some_and(|cid| {
                        bm_corp == Some(cid)
                            && has_corp_bookmark_admin_role(snap, session, char_id, cid)
                    });
                    if bm.owner_id != char_id && !(bm.is_shared && has_manage_access) {
                        return Err(BridgeRpcError {
                            code: "AccessDenied".into(),
                            message: "Cannot delete another character's bookmark without manage/admin rights".into(),
                        });
                    }
                    if bm.is_shared {
                        any_shared_folder = Some(bm.folder_id);
                    }
                    snap.bookmarks.remove(&bm_id);
                    snap.bookmark_expiries.remove(&bm_id);
                    snap.bookmark_subfolders.remove(&bm_id);
                }
                Ok((any_shared_folder, corp_id))
            });
            match res {
                Ok((any_shared_folder, corp_id)) => {
                    if let (Some(fid_opt), Some(cid)) = (any_shared_folder, corp_id) {
                        ctx.notify(
                            NotificationTarget::Corporation(cid),
                            "OnSharedBookmarksFolderUpdated",
                            "corpid",
                            shared_folder_updates_arg(
                                fid_opt.unwrap_or(0),
                                "BookmarksDeleted",
                                PyRep::List(bm_ids.iter().copied().map(PyRep::Integer).collect()),
                            ),
                        );
                    }
                    Some(Ok(PyRep::keyval([
                        ("bookmarkID", PyRep::Integer(bm_ids[0])),
                        ("deleted", PyRep::Bool(true)),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("bookmark", "CopyBookmarksToFolderAndSubfolder")
        | ("bookmark", "MoveBookmarksToFolderAndSubfolder") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let bm_ids = extract_id_list(args.first());
            if bm_ids.len() > 20 {
                return Some(Err(BridgeRpcError {
                    code: "BookmarkCopyBatchLimitExceeded".into(),
                    message: "Cannot copy or move more than 20 bookmarks in a single operation".into(),
                }));
            }
            let target_folder_id = args.get(1).and_then(|a| a.as_i64());
            let target_subfolder_id = args.get(2).and_then(|a| a.as_i64());
            let is_move = method.starts_with("Move");

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let caller_corp_id = session
                    .corporation_id
                    .or_else(|| snap.characters.get(&char_id).map(|c| c.corporation_id));
                let is_shared_target = if let Some(fid) = target_folder_id {
                    let Some(tf) = snap.bookmark_folders.get(&fid) else {
                        return Err(BridgeRpcError { code: "FolderNotFound".into(), message: format!("Bookmark folder {fid} not found") });
                    };
                    if tf.owner_id != char_id
                        && !(tf.is_shared && caller_corp_id.is_some() && tf.corporation_id == caller_corp_id)
                    {
                        return Err(BridgeRpcError { code: "BookmarkFolderAccessDenied".into(), message: "No write access to target bookmark folder".into() });
                    }
                    tf.is_shared
                } else {
                    false
                };
                let current_in_folder = snap
                    .bookmarks
                    .values()
                    .filter(|b| b.folder_id == target_folder_id && (is_shared_target || b.owner_id == char_id))
                    .count();
                let added_count = if is_move {
                    bm_ids
                        .iter()
                        .filter(|&&id| snap.bookmarks.get(&id).is_some_and(|b| b.folder_id != target_folder_id))
                        .count()
                } else {
                    bm_ids.len()
                };
                if current_in_folder + added_count > 500 {
                    let code = if is_shared_target { "MaxSharedFolderBookmarksReached" } else { "MaxBookmarksReached" };
                    return Err(BridgeRpcError { code: code.into(), message: "Bookmark folder capacity (500) exceeded".into() });
                }
                let mut out_ids = Vec::new();
                for &bm_id in &bm_ids {
                    let Some(orig) = snap.bookmarks.get(&bm_id).cloned() else {
                        return Err(BridgeRpcError {
                            code: "BookmarkNotFound".into(),
                            message: format!("Bookmark {bm_id} not found"),
                        });
                    };
                    let orig_corp_id = shared_bookmark_corp_id(snap, &orig);
                    let same_corp_shared = orig.is_shared && caller_corp_id.is_some() && orig_corp_id == caller_corp_id;
                    let can_access = if is_move {
                        orig.owner_id == char_id
                            || (same_corp_shared
                                && has_corp_bookmark_admin_role(
                                    snap,
                                    session,
                                    char_id,
                                    caller_corp_id.unwrap(),
                                ))
                    } else {
                        orig.owner_id == char_id || same_corp_shared
                    };
                    if !can_access {
                        return Err(BridgeRpcError {
                            code: "BookmarkAccessDenied".into(),
                            message: format!("No access to bookmark {bm_id}"),
                        });
                    }
                    if is_move {
                        if let Some(bm) = snap.bookmarks.get_mut(&bm_id) {
                            bm.folder_id = target_folder_id;
                            bm.is_shared = is_shared_target;
                        }
                        if let Some(sfid) = target_subfolder_id {
                            snap.bookmark_subfolders.insert(bm_id, sfid);
                        }
                        out_ids.push(bm_id);
                    } else {
                        snap.next_entity_id += 1;
                        let new_id = snap.next_entity_id;
                        snap.bookmarks.insert(
                            new_id,
                            BookmarkRecord {
                                bookmark_id: new_id,
                                owner_id: char_id,
                                folder_id: target_folder_id,
                                is_shared: is_shared_target,
                                ..orig
                            },
                        );
                        if let Some(sfid) = target_subfolder_id {
                            snap.bookmark_subfolders.insert(new_id, sfid);
                        }
                        out_ids.push(new_id);
                    }
                }
                Ok(out_ids)
            });

            match res {
                Ok(out_ids) => {
                    Some(Ok(PyRep::keyval([
                        ("bookmarkIDs", PyRep::List(out_ids.into_iter().map(PyRep::Integer).collect())),
                        ("folderID", target_folder_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("bookmark", "AddFolder")
        | ("bookmark", "UpdateFolder") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            if method == "UpdateFolder" {
                let folder_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
                let new_name = args.get(1).and_then(|a| a.as_str()).unwrap_or("Updated Folder");
                let acl_arg = args.get(2).map(|a| extract_id_list(Some(a)));
                let res = ctx.db.try_with_mut_snapshot(|snap| {
                    let Some(f) = snap.bookmark_folders.get_mut(&folder_id) else {
                        return Err(BridgeRpcError {
                            code: "FolderNotFound".into(),
                            message: format!("Folder {folder_id} not found"),
                        });
                    };
                    if f.owner_id != char_id {
                        return Err(BridgeRpcError {
                            code: "AccessDenied".into(),
                            message: "Only folder owner can rename folder".into(),
                        });
                    }
                    f.folder_name = new_name.to_string();
                    if let Some(acl) = acl_arg {
                        snap.bookmark_folder_acls.insert(folder_id, acl);
                    }
                    let effective_acl = snap
                        .bookmark_folder_acls
                        .get(&folder_id)
                        .cloned()
                        .unwrap_or_default();
                    Ok(effective_acl)
                });
                return match res {
                    Ok(acl) => Some(Ok(PyRep::keyval([
                        ("folderID", PyRep::Integer(folder_id)),
                        ("folderName", PyRep::String(new_name.to_string())),
                        (
                            "accessGroupIDs",
                            PyRep::List(acl.into_iter().map(PyRep::Integer).collect()),
                        ),
                    ]))),
                    Err(e) => Some(Err(e)),
                };
            }

            let acl_ids = extract_id_list(args.get(2));
            let (name, is_shared) = match args.first() {
                Some(PyRep::Bool(is_personal)) => (
                    args.get(1)
                        .and_then(|a| a.as_str())
                        .unwrap_or("Personal Locations"),
                    !*is_personal,
                ),
                _ => (
                    args.first().and_then(|a| a.as_str()).unwrap_or("New Folder"),
                    matches!(args.get(1), Some(PyRep::Bool(true)))
                        || args.get(1).and_then(|a| a.as_i64()).is_some_and(|v| v > 0),
                ),
            };
            if is_shared {
                let db_corp_id = ctx.db.with_snapshot(|snap| {
                    snap.characters
                        .get(&char_id)
                        .map(|c| c.corporation_id)
                        .filter(|cid| snap.corporations.contains_key(cid))
                });
                if let Some(cid) = db_corp_id {
                    session.corporation_id = Some(cid);
                }
                let corp_id = match session.corporation_id {
                    Some(c) => c,
                    None => {
                        return Some(Err(BridgeRpcError {
                            code: "NoCorporation".into(),
                            message: "Creating shared folder requires corporation membership".into(),
                        }))
                    }
                };
                let acl_for_shared = acl_ids.clone();
                let res = ctx.db.try_with_mut_snapshot(|snap| {
                    let has_rights = if let Some(corp) = snap.corporations.get(&corp_id) {
                        let role = corp.member_roles.get(&char_id).copied().unwrap_or(0);
                        char_id == corp.ceo_id
                            || corp_roles::has_role(role, corp_roles::ROLE_DIRECTOR)
                            || corp_roles::has_role(role, corp_roles::ROLE_PERSONNEL_MANAGER)
                    } else {
                        corp_roles::has_role(session.corp_role, corp_roles::ROLE_DIRECTOR)
                            || corp_roles::has_role(session.corp_role, corp_roles::ROLE_PERSONNEL_MANAGER)
                    };
                    if !has_rights {
                        return Err(BridgeRpcError {
                            code: "BookmarkAccessDenied".into(),
                            message: "Creating a shared bookmark folder requires corporation Director/CEO access-group rights".into(),
                        });
                    }
                    snap.next_entity_id += 1;
                    let id = snap.next_entity_id;
                    snap.bookmark_folders.insert(
                        id,
                        BookmarkFolderRecord {
                            folder_id: id,
                            owner_id: char_id,
                            folder_name: name.to_string(),
                            is_shared: true,
                            corporation_id: Some(corp_id),
                            ..Default::default()
                        },
                    );
                    if !acl_for_shared.is_empty() {
                        snap.bookmark_folder_acls.insert(id, acl_for_shared);
                    }
                    Ok(id)
                });
                return match res {
                    Ok(folder_id) => {
                        ctx.notify(
                            NotificationTarget::Corporation(corp_id),
                            "OnSharedBookmarksFolderUpdated",
                            "corpid",
                            shared_folder_updates_arg(folder_id, "FolderChanged", PyRep::Integer(corp_id)),
                        );
                        Some(Ok(PyRep::keyval([
                            ("folderID", PyRep::Integer(folder_id)),
                            ("folderName", PyRep::String(name.to_string())),
                            ("ownerID", PyRep::Integer(char_id)),
                            ("creatorID", PyRep::Integer(char_id)),
                            ("isPersonal", PyRep::Bool(false)),
                            ("isShared", PyRep::Bool(true)),
                            ("isActive", PyRep::Bool(true)),
                            ("accessLevel", PyRep::Integer(4)),
                            (
                                "accessGroupIDs",
                                PyRep::List(acl_ids.into_iter().map(PyRep::Integer).collect()),
                            ),
                            ("description", PyRep::String(String::new())),
                            ("corporationID", PyRep::Integer(corp_id)),
                        ])))
                    }
                    Err(e) => Some(Err(e)),
                };
            }

            let acl_for_personal = acl_ids.clone();
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let personal_root_count = snap
                    .bookmark_folders
                    .values()
                    .filter(|f| {
                        f.owner_id == char_id
                            && !f.is_shared
                            && !snap.bookmark_subfolder_parents.contains_key(&f.folder_id)
                            && !f.folder_name.contains('/')
                    })
                    .count();
                if personal_root_count >= 5 {
                    return Err(BridgeRpcError {
                        code: "MaxPersonalFoldersReached".into(),
                        message: "Character has reached the maximum of 5 personal bookmark folders".into(),
                    });
                }
                snap.next_entity_id += 1;
                let id = snap.next_entity_id;
                snap.bookmark_folders.insert(
                    id,
                    BookmarkFolderRecord {
                        folder_id: id,
                        owner_id: char_id,
                        folder_name: name.to_string(),
                        is_shared: false,
                        corporation_id: None,
                        ..Default::default()
                    },
                );
                if !acl_for_personal.is_empty() {
                    snap.bookmark_folder_acls.insert(id, acl_for_personal);
                }
                Ok(id)
            });
            match res {
                Ok(folder_id) => Some(Ok(PyRep::keyval([
                    ("folderID", PyRep::Integer(folder_id)),
                    ("folderName", PyRep::String(name.to_string())),
                    ("ownerID", PyRep::Integer(char_id)),
                    ("creatorID", PyRep::Integer(char_id)),
                    ("isPersonal", PyRep::Bool(true)),
                    ("isShared", PyRep::Bool(false)),
                    ("isActive", PyRep::Bool(true)),
                    ("accessLevel", PyRep::Integer(4)),
                    (
                        "accessGroupIDs",
                        PyRep::List(acl_ids.into_iter().map(PyRep::Integer).collect()),
                    ),
                    ("description", PyRep::String(String::new())),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        ("bookmark", "GetFolders") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let corp_id_opt = session.corporation_id;
            let target_fid = if method == "GetFolderInfo" {
                args.first().and_then(|a| a.as_i64())
            } else {
                None
            };
            let rows: Vec<Vec<PyRep>> = ctx.db.with_snapshot(|snap| {
                snap.bookmark_folders
                    .values()
                    .filter(|f| {
                        let visible = f.owner_id == char_id
                            || (f.is_shared && f.corporation_id == corp_id_opt)
                            || snap
                                .bookmark_folder_acls
                                .get(&f.folder_id)
                                .is_some_and(|acl| acl.contains(&char_id));
                        if !visible {
                            return false;
                        }
                        if let Some(tfid) = target_fid {
                            f.folder_id == tfid
                                || snap.bookmark_subfolder_parents.get(&f.folder_id) == Some(&tfid)
                        } else {
                            !snap.bookmark_subfolder_parents.contains_key(&f.folder_id)
                        }
                    })
                    .map(|f| {
                        let acl = snap
                            .bookmark_folder_acls
                            .get(&f.folder_id)
                            .cloned()
                            .unwrap_or_default();
                        let access_level = if f.owner_id == char_id || acl.contains(&char_id) {
                            4
                        } else if f.is_shared {
                            3
                        } else {
                            1
                        };
                        vec![
                            PyRep::Integer(f.folder_id),
                            PyRep::String(f.folder_name.clone()),
                            PyRep::Integer(f.owner_id),
                            PyRep::Bool(f.is_shared),
                            f.corporation_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                            PyRep::List(acl.into_iter().map(PyRep::Integer).collect()),
                            PyRep::Integer(access_level),
                        ]
                    })
                    .collect()
            });
            Some(Ok(PyRep::rowset(
                    vec![
                        "folderID", "folderName", "ownerID", "isShared", "corporationID", "accessGroupIDs", "accessLevel",
                    ],
                    rows,
                )))
        }

        ("bookmark", "DeleteFolder") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let Some(folder_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError { code: "InvalidArgument".into(), message: "DeleteFolder requires folderID".into() }));
            };
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(f) = snap.bookmark_folders.get(&folder_id).cloned() else {
                    return Err(BridgeRpcError { code: "FolderNotFound".into(), message: format!("Folder {folder_id} not found") });
                };
                if f.owner_id != char_id {
                    return Err(BridgeRpcError { code: "AccessDenied".into(), message: "Cannot delete another character's folder".into() });
                }
                let child_sfs: Vec<i64> = snap
                    .bookmark_subfolder_parents
                    .iter()
                    .filter_map(|(&sfid, &pfid)| (pfid == folder_id).then_some(sfid))
                    .collect();
                for sfid in &child_sfs {
                    snap.bookmark_folders.remove(sfid);
                    snap.bookmark_subfolder_parents.remove(sfid);
                }
                snap.bookmark_subfolders
                    .retain(|_, sfid| !child_sfs.contains(sfid));
                snap.bookmark_folders.remove(&folder_id);
                snap.bookmark_folder_acls.remove(&folder_id);
                snap.bookmark_folder_known_by
                    .retain(|&(fid, _), _| fid != folder_id);
                for bm in snap.bookmarks.values_mut() {
                    if bm.folder_id == Some(folder_id) {
                        bm.folder_id = None;
                    }
                }
                Ok(f.corporation_id)
            });
            match res {
                Ok(corp_id_opt) => {
                    if let Some(cid) = corp_id_opt {
                        ctx.notify(
                            NotificationTarget::Corporation(cid),
                            "OnSharedBookmarksFolderDeleted",
                            "corpid",
                            vec![PyRep::Integer(folder_id)],
                        );
                    }
                    Some(Ok(PyRep::keyval([
                        ("folderID", PyRep::Integer(folder_id)),
                        ("deleted", PyRep::Bool(true)),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("bookmark", "CreateSubfolder") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let Some(first_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "CreateSubfolder requires parent folderID".into(),
                }));
            };
            let sub_name = args.get(1).and_then(|a| a.as_str()).unwrap_or("Subfolder");
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                if method == "UpdateSubfolder" {
                    if let Some(&pfid) = snap.bookmark_subfolder_parents.get(&first_id) {
                        if let Some(parent) = snap.bookmark_folders.get(&pfid) {
                            if parent.owner_id != char_id
                                && folder_access_level(snap, session, char_id, parent)
                                    < AccessLevel::Manage
                            {
                                return Err(BridgeRpcError {
                                    code: "AccessDenied".into(),
                                    message:
                                        "Cannot update subfolder in another character's folder without manage/admin rights"
                                            .into(),
                                });
                            }
                        }
                        let parent_name = snap
                            .bookmark_folders
                            .get(&pfid)
                            .map(|p| p.folder_name.clone())
                            .unwrap_or_default();
                        let full_name = if parent_name.is_empty() {
                            sub_name.to_string()
                        } else {
                            format!("{parent_name}/{sub_name}")
                        };
                        if let Some(sf) = snap.bookmark_folders.get_mut(&first_id) {
                            sf.folder_name = full_name.clone();
                        }
                        return Ok((first_id, pfid, full_name));
                    }
                }
                let parent_folder_id = first_id;
                let Some(parent) = snap.bookmark_folders.get(&parent_folder_id).cloned() else {
                    return Err(BridgeRpcError {
                        code: "FolderNotFound".into(),
                        message: format!("Parent folder {parent_folder_id} not found"),
                    });
                };
                if parent.owner_id != char_id
                    && folder_access_level(snap, session, char_id, &parent) < AccessLevel::Manage
                {
                    return Err(BridgeRpcError {
                        code: "AccessDenied".into(),
                        message:
                            "Cannot create subfolder in another character's folder without manage/admin rights"
                                .into(),
                    });
                }
                let prefix = format!("{}/", parent.folder_name);
                let sub_count = snap
                    .bookmark_folders
                    .values()
                    .filter(|f| {
                        snap.bookmark_subfolder_parents.get(&f.folder_id) == Some(&parent_folder_id)
                            || (f.owner_id == char_id && f.folder_name.starts_with(&prefix))
                    })
                    .count();
                let max_subs = if parent.is_shared { 30 } else { 5 };
                if sub_count >= max_subs {
                    return Err(BridgeRpcError {
                        code: "MaxSubfoldersReached".into(),
                        message: format!("Maximum of {max_subs} subfolders reached"),
                    });
                }
                let full_name = format!("{}/{sub_name}", parent.folder_name);
                snap.next_entity_id += 1;
                let id = snap.next_entity_id;
                snap.bookmark_folders.insert(
                    id,
                    BookmarkFolderRecord {
                        folder_id: id,
                        owner_id: char_id,
                        folder_name: full_name.clone(),
                        is_shared: parent.is_shared,
                        corporation_id: parent.corporation_id,
                        ..Default::default()
                    },
                );
                snap.bookmark_subfolder_parents.insert(id, parent_folder_id);
                Ok((id, parent_folder_id, full_name))
            });
            match res {
                Ok((subfolder_id, parent_folder_id, full_name)) => Some(Ok(PyRep::keyval([
                    ("subfolderID", PyRep::Integer(subfolder_id)),
                    ("folderID", PyRep::Integer(subfolder_id)),
                    ("parentFolderID", PyRep::Integer(parent_folder_id)),
                    ("subfolderName", PyRep::String(sub_name.to_string())),
                    ("folderName", PyRep::String(full_name)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        ("bookmark", "DeleteSubfolder") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let Some(subfolder_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "DeleteSubfolder requires subfolderID".into(),
                }));
            };
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(f) = snap.bookmark_folders.get(&subfolder_id) else {
                    return Err(BridgeRpcError {
                        code: "SubfolderNotFound".into(),
                        message: format!("Subfolder {subfolder_id} not found"),
                    });
                };
                if f.owner_id != char_id {
                    return Err(BridgeRpcError {
                        code: "AccessDenied".into(),
                        message: "Cannot delete another character's subfolder".into(),
                    });
                }
                snap.bookmark_folders.remove(&subfolder_id);
                snap.bookmark_subfolder_parents.remove(&subfolder_id);
                snap.bookmark_subfolders
                    .retain(|_, sfid| *sfid != subfolder_id);
                for bm in snap.bookmarks.values_mut() {
                    if bm.folder_id == Some(subfolder_id) {
                        bm.folder_id = None;
                    }
                }
                Ok(())
            });
            match res {
                Ok(()) => Some(Ok(PyRep::keyval([
                    ("subfolderID", PyRep::Integer(subfolder_id)),
                    ("deleted", PyRep::Bool(true)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        ("bookmark", "AddToKnownFolders") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let Some(folder_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "AddToKnownFolders requires folderID".into(),
                }));
            };
            let active = args.get(1).map(|a| matches!(a, PyRep::Bool(true) | PyRep::Integer(1))).unwrap_or(true);
            let db_corp_id = ctx.db.with_snapshot(|snap| {
                snap.characters
                    .get(&char_id)
                    .map(|c| c.corporation_id)
                    .filter(|cid| snap.corporations.contains_key(cid))
            });
            if let Some(cid) = db_corp_id {
                session.corporation_id = Some(cid);
            }
            let check = ctx.db.with_snapshot(|snap| {
                let Some(f) = snap.bookmark_folders.get(&folder_id) else {
                    return Err(BridgeRpcError {
                        code: "FolderNotFound".into(),
                        message: format!("Shared folder {folder_id} not found"),
                    });
                };
                if !f.is_shared || f.corporation_id != session.corporation_id {
                    return Err(BridgeRpcError {
                        code: "BookmarkAccessDenied".into(),
                        message: "Character lacks access-group rights to subscribe to this shared bookmark folder".into(),
                    });
                }
                Ok((f.folder_name.clone(), f.corporation_id))
            });
            match check {
                Ok((folder_name, corp_id_opt)) => {
                    let sub_res = ctx.db.try_with_mut_snapshot(|snap| {
                        let current_active = snap
                            .bookmark_folder_known_by
                            .iter()
                            .filter(|(&(fid, cid), &act)| cid == char_id && fid != folder_id && act)
                            .count();
                        if active && current_active >= 3 {
                            return Err(BridgeRpcError {
                                code: "MaxActiveSharedFoldersReached".into(),
                                message: "Maximum of 3 online shared bookmark folders reached".into(),
                            });
                        }
                        snap.bookmark_folder_known_by
                            .insert((folder_id, char_id), active);
                        Ok(())
                    });
                    if let Err(e) = sub_res {
                        return Some(Err(e));
                    }
                    ctx.notify(
                        NotificationTarget::Character(char_id),
                        "OnSharedBookmarksFoldersAccessChanged",
                        "charid",
                        vec![PyRep::Dict(vec![(
                            PyRep::Integer(folder_id),
                            PyRep::Integer(if active { 1 } else { 0 }),
                        )])],
                    );
                    ctx.notify(
                        NotificationTarget::Character(char_id),
                        "OnSharedBookmarksFolderUpdated",
                        "charid",
                        shared_folder_updates_arg(folder_id, "FolderChanged", PyRep::Bool(active)),
                    );
                    if let Some(corp_id) = corp_id_opt {
                        ctx.notify(
                            NotificationTarget::Corporation(corp_id),
                            "OnSharedBookmarksFolderUpdated",
                            "corpid",
                            shared_folder_updates_arg(folder_id, "FolderChanged", PyRep::Integer(corp_id)),
                        );
                    }
                    Some(Ok(PyRep::keyval([
                        ("folderID", PyRep::Integer(folder_id)),
                        ("folderName", PyRep::String(folder_name)),
                        ("subscribed", PyRep::Bool(active)),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("bookmark", "RemoveFromKnownFolders") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let Some(folder_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "RemoveFromKnownFolders requires folderID".into(),
                }));
            };
            let exists = ctx.db.with_snapshot(|snap| snap.bookmark_folders.contains_key(&folder_id));
            if !exists {
                return Some(Err(BridgeRpcError {
                    code: "FolderNotFound".into(),
                    message: format!("Shared folder {folder_id} not found"),
                }));
            }
            ctx.db.with_mut_snapshot(|snap| {
                snap.bookmark_folder_known_by.remove(&(folder_id, char_id));
            });
            ctx.notify(
                NotificationTarget::Character(char_id),
                "OnSharedBookmarksFoldersAccessLost",
                "charid",
                vec![PyRep::List(vec![PyRep::Integer(folder_id)])],
            );
            ctx.notify(
                NotificationTarget::Character(char_id),
                "OnSharedBookmarksFolderUpdated",
                "charid",
                shared_folder_updates_arg(folder_id, "FolderChanged", PyRep::Bool(false)),
            );
            Some(Ok(PyRep::keyval([
                ("folderID", PyRep::Integer(folder_id)),
                ("subscribed", PyRep::Bool(false)),
            ])))
        }

        ("scanMgr", "BookmarkScanResult")
        | ("bookmark", "BookmarkScanResult")
        | ("beyonce", "BookmarkScanResult") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            // In EVE, cosmic signature scanning and bookmarking can only occur
            // while uncloaked in active space. Docked sessions (station or structure) are rejected.
            if session.solar_system_id.is_none() || session.station_id.is_some() || session.structure_id.is_some() {
                return Some(Err(BridgeRpcError { code: "NotInSpace".into(), message: "Must be in space to bookmark scans".into() }));
            }
            let sys_id = session.solar_system_id.unwrap();
            if sys_id == 30_100_000 {
                return Some(Err(BridgeRpcError::user_error("CantbookmarkEveSystem")));
            }
            let (sig_id, label, folder_id) = if let Some(s) = args.first().and_then(|a| a.as_str()) {
                (s, args.get(1).and_then(|a| a.as_str()).unwrap_or(s), args.get(2).and_then(|a| a.as_i64()))
            } else if let Some(s) = args.get(3).and_then(|a| a.as_str()) {
                // Prevent cross-system signature exploit: location argument must match caller's active solar system.
                if let Some(loc_arg) = args.first().and_then(|a| a.as_i64()) {
                    if u32::try_from(loc_arg).map_err(|_| ()).ok() != Some(sys_id) {
                        return Some(Err(BridgeRpcError { code: "NotInSameSystem".into(), message: "Different solar system".into() }));
                    }
                }
                let fid = args.get(4).and_then(|a| a.as_i64()).or_else(|| _kwargs.get("subfolderID").and_then(|v| v.as_i64())).or_else(|| args.get(5).and_then(|a| a.as_i64()));
                (s, args.get(1).and_then(|a| a.as_str()).unwrap_or(s), fid)
            } else {
                return Some(Err(BridgeRpcError { code: "InvalidArgument".into(), message: "Requires signatureID".into() }));
            };
            // Folder 0 or char_id represents root personal folder; custom non-root folders require ownership or shared access.
            if let Some(fid) = folder_id {
                if fid != 0 && fid != char_id {
                    let ok = ctx.db.with_snapshot(|s| {
                        s.bookmark_folders.get(&fid).is_some_and(|f| {
                            f.owner_id == char_id || (f.is_shared && session.corporation_id == Some(f.owner_id))
                        })
                    });
                    if !ok {
                        return Some(Err(BridgeRpcError {
                            code: "FolderNotFound".into(),
                            message: format!("Folder {fid} not accessible"),
                        }));
                    }
                }
            }
            // Game mechanics: Signatures must be 100% scanned to be bookmarked. Derive signature type
            // on the server (wormhole type or celestial default) rather than trusting client.
            let now_unix = ctx.now_unix();
            let sig_res = ctx.db.with_snapshot(|s| {
                s.cosmic_signatures.get(&sys_id).and_then(|sigs| {
                    sigs.iter().find(|s| s.signature_id == sig_id && (s.lifetime_seconds == 0 || now_unix < s.created_unix.saturating_add(s.lifetime_seconds))).map(|s| (s.position, ctx.effective_signature_signal_strength(char_id, sys_id, s), s.wormhole_type_id.map(|w| w as i64).unwrap_or(9988)))
                })
            });
            let Some((pos, signal_strength, sig_type_id)) = sig_res else {
                return Some(Err(BridgeRpcError { code: "SignatureNotFound".into(), message: format!("Cosmic signature {sig_id} not found") }));
            };
            if signal_strength < 1.0 {
                return Some(Err(BridgeRpcError { code: "SignatureNotResolved".into(), message: format!("Signature {sig_id} not resolved") }));
            }
            let bm_id = ctx.db.try_with_mut_snapshot(|snap| {
                let is_shared = folder_id.and_then(|fid| snap.bookmark_folders.get(&fid)).map(|f| f.is_shared).unwrap_or(false);
                snap.next_entity_id += 1;
                let id = snap.next_entity_id;
                snap.bookmarks.insert(id, BookmarkRecord { bookmark_id: id, owner_id: char_id, label: label.to_string(), solar_system_id: sys_id, x: pos[0], y: pos[1], z: pos[2], item_id: None, memo: format!("Scanned {sig_id}"), folder_id, is_shared });
                Ok::<_, std::convert::Infallible>(id)
            }).unwrap();
            if service == "beyonce" {
                Some(Ok(PyRep::Tuple(vec![PyRep::Integer(bm_id), PyRep::Integer(sys_id as i64), PyRep::Integer(sig_type_id), PyRep::Real(pos[0]), PyRep::Real(pos[1]), PyRep::Real(pos[2]), PyRep::Integer(sys_id as i64), folder_id.map(PyRep::Integer).unwrap_or(PyRep::None)])))
            } else {
                Some(Ok(PyRep::keyval([("bookmarkID", PyRep::Integer(bm_id)), ("signatureID", PyRep::String(sig_id.to_string())), ("label", PyRep::String(label.to_string())), ("x", PyRep::Real(pos[0])), ("y", PyRep::Real(pos[1])), ("z", PyRep::Real(pos[2]))])))
            }
        }

        _ => None,
    }
}
