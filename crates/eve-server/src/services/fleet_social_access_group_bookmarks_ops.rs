//! `accessGroupBookmarkMgr` bookmark operation handlers:
//! - `BookmarkStaticLocation` (8-tuple)
//! - `GetMyActiveBookmarks` (3-tuple)
//! - `UpdateBookmark`
//! - `DeleteBookmarks` (list of deleted IDs)
//! - `CopyBookmarksToFolderAndSubfolder` (2-tuple of dict, folderID)
//! - `MoveBookmarksToFolderAndSubfolder` (2-tuple of list, folderID)

use super::fleet_social_access_group_bookmarks::{
    acl_folder_updates_arg, folder_access_level, AccessLevel, BOOKMARKS_ADDED,
    BOOKMARKS_REMOVED, BOOKMARKS_UPDATED,
};
use super::fleet_social_chat::extract_id_list;
use crate::router::{cache_notifications::NotificationTarget, ServerContext};
use eve_db::BookmarkRecord;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;

/// Formats active bookmarks as `(folders, bookmarks, subfolders)` 3-tuple consumed by
/// `BookmarkSvc.ACLPrimeBookmarks` (`eve/client/script/ui/shared/bookmarks/bookmarkSvc.pyj:164-169`).
pub(crate) fn acl_active_bookmarks(
    ctx: &ServerContext,
    session: &SessionState,
    char_id: i64,
    columns: &[&str],
    rows: Vec<Vec<PyRep>>,
) -> PyRep {
    let folder_ids: Vec<i64> = rows.iter().filter_map(|r| r[8].as_i64()).collect();
    let row_subfolder_ids: Vec<i64> = rows.iter().filter_map(|r| r[10].as_i64()).collect();
    let (folders, subfolders): (Vec<PyRep>, Vec<PyRep>) = ctx.db.with_snapshot(|snap| {
        let mut list: Vec<_> = snap
            .bookmark_folders
            .values()
            .filter(|f| {
                !snap.bookmark_subfolder_parents.contains_key(&f.folder_id)
                    && (folder_access_level(snap, session, char_id, f) >= AccessLevel::View
                        || folder_ids.contains(&f.folder_id))
            })
            .cloned()
            .collect();
        list.sort_by_key(|f| f.folder_id);
        let visible_fids: Vec<i64> = list.iter().map(|f| f.folder_id).collect();
        let mut sf_ids: Vec<i64> = snap
            .bookmark_subfolder_parents
            .iter()
            .filter_map(|(&sfid, &pfid)| visible_fids.contains(&pfid).then_some(sfid))
            .chain(row_subfolder_ids.iter().copied())
            .collect();
        sf_ids.sort_unstable();
        sf_ids.dedup();
        let subfolders = sf_ids
            .into_iter()
            .map(|id| {
                let parent_fid = snap.bookmark_subfolder_parents.get(&id).copied();
                let sub_name = snap
                    .bookmark_folders
                    .get(&id)
                    .map(|f| {
                        if let Some(pf) = parent_fid.and_then(|pfid| snap.bookmark_folders.get(&pfid)) {
                            let prefix = format!("{}/", pf.folder_name);
                            f.folder_name
                                .strip_prefix(&prefix)
                                .unwrap_or(&f.folder_name)
                                .to_string()
                        } else {
                            f.folder_name
                                .rsplit('/')
                                .next()
                                .unwrap_or(&f.folder_name)
                                .to_string()
                        }
                    })
                    .unwrap_or_else(|| format!("Subfolder {id}"));
                PyRep::keyval([
                    ("subfolderID", PyRep::Integer(id)),
                    ("subfolderName", PyRep::String(sub_name)),
                    (
                        "folderID",
                        parent_fid.map(PyRep::Integer).unwrap_or(PyRep::None),
                    ),
                ])
            })
            .collect();
        let folders = list
            .into_iter()
            .map(|f| {
                let is_active = if !f.is_shared {
                    f.owner_id == char_id
                } else {
                    snap.bookmark_folder_known_by
                        .get(&(f.folder_id, char_id))
                        .copied()
                        .unwrap_or(false)
                };
                let acl = snap
                    .bookmark_folder_acls
                    .get(&f.folder_id)
                    .cloned()
                    .unwrap_or_default();
                let access_level = folder_access_level(snap, session, char_id, &f) as i64;
                PyRep::keyval([
                    ("folderID", PyRep::Integer(f.folder_id)),
                    ("folderName", PyRep::String(f.folder_name)),
                    ("description", PyRep::String(f.description)),
                    ("ownerID", PyRep::Integer(f.owner_id)),
                    ("creatorID", PyRep::Integer(f.owner_id)),
                    ("isPersonal", PyRep::Bool(!f.is_shared)),
                    ("isShared", PyRep::Bool(f.is_shared)),
                    ("isActive", PyRep::Bool(is_active)),
                    ("accessLevel", PyRep::Integer(access_level)),
                    ("adminGroupID", f.admin_group_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
                    ("manageGroupID", f.manage_group_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
                    ("useGroupID", f.use_group_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
                    ("viewGroupID", f.view_group_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
                    (
                        "accessGroupIDs",
                        PyRep::List(acl.into_iter().map(PyRep::Integer).collect()),
                    ),
                    ("corporationID", f.corporation_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
                ])
            })
            .collect();
        (folders, subfolders)
    });
    let bookmarks: Vec<PyRep> = rows
        .into_iter()
        .map(|row| {
            let location = row[2].clone();
            let mut fields: Vec<(&str, PyRep)> = columns.iter().copied().zip(row).collect();
            fields.push(("locationID", location));
            PyRep::keyval(fields)
        })
        .collect();
    PyRep::Tuple(vec![PyRep::List(folders), PyRep::List(bookmarks), PyRep::List(subfolders)])
}

/// `accessGroupBookmarkMgr.BookmarkStaticLocation`
/// Returns 8-tuple `(bookmarkID, itemID/systemID, typeID, x, y, z, locationID, folderID)`
/// strictly consumed by `BookmarkSvc.ACLBookmarkLocation` (`unpack:8`).
pub(crate) async fn handle_bookmark_static_location(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let Some(sys_id) = session.solar_system_id else {
        return Err(BridgeRpcError {
            code: "NoSolarSystem".into(),
            message: "Session has no active solar system".into(),
        });
    };

    let (explicit_entity_id, folder_id, label_str, memo_str, expiry_hours, subfolder_id) =
        if args.first().and_then(|a| a.as_str()).is_some() {
            let lbl = args.first().and_then(|a| a.as_str()).unwrap_or("Safe Spot");
            let ent = args.get(1).and_then(|a| a.as_i64());
            let m = args.get(2).and_then(|a| a.as_str()).unwrap_or("");
            let fid = args.get(3).and_then(|a| a.as_i64());
            let sfid = args
                .get(4)
                .and_then(|a| a.as_i64())
                .or_else(|| kwargs.get("subfolderID").and_then(|v| v.as_i64()));
            let exp = args.get(5).and_then(|a| a.as_i64()).unwrap_or(0);
            (ent, fid, lbl, m, exp, sfid)
        } else {
            let ent = args.first().and_then(|a| a.as_i64());
            let fid = args.get(1).and_then(|a| a.as_i64());
            let lbl = args.get(2).and_then(|a| a.as_str()).unwrap_or("Safe Spot");
            let m = args.get(3).and_then(|a| a.as_str()).unwrap_or("");
            let exp = args.get(4).and_then(|a| a.as_i64()).unwrap_or(0);
            let sfid = kwargs
                .get("subfolderID")
                .and_then(|v| v.as_i64())
                .or_else(|| args.get(5).and_then(|a| a.as_i64()));
            (ent, fid, lbl, m, exp, sfid)
        };

    let label = label_str.trim();
    if label.is_empty() || label.chars().count() > 100 {
        return Err(BridgeRpcError {
            code: "InvalidBookmarkLabel".into(),
            message: "Bookmark label must be 1..=100 non-whitespace characters".into(),
        });
    }

    if let Some(fid) = folder_id {
        if fid != 0 && fid != char_id {
            let ok = ctx.db.with_snapshot(|s| {
                s.bookmark_folders.get(&fid).is_some_and(|f| {
                    folder_access_level(s, session, char_id, f) >= AccessLevel::Use
                })
            });
            if !ok {
                return Err(BridgeRpcError {
                    code: "UserError".into(),
                    message: format!("Folder {fid} not accessible for bookmark creation"),
                });
            }
        }
    }

    let (derived_type_id, (x, y, z)) = if let Some(ent_id) = explicit_entity_id {
        if ent_id == sys_id as i64 {
            (5, (0.0, 0.0, 0.0))
        } else if let Some(st) = ctx
            .sde
            .solar_systems
            .get(&sys_id)
            .and_then(|s| s.stations.iter().find(|st| st.station_id == ent_id))
        {
            (st.station_type_id as i64, (st.position.x, st.position.y, st.position.z))
        } else if let Some(st) = ctx.sde.stations.get(&ent_id).filter(|st| st.solar_system_id == sys_id) {
            (st.station_type_id as i64, (st.position.x, st.position.y, st.position.z))
        } else if let Some(jump) = ctx.sde.jumps.get(&ent_id).filter(|j| j.from_solar_system_id == sys_id) {
            (29624, (jump.position.x, jump.position.y, jump.position.z))
        } else if let Some(c) = ctx.sde.celestials_in_system(sys_id).into_iter().find(|c| c.celestial_id == ent_id) {
            (c.type_id as i64, (c.position.x, c.position.y, c.position.z))
        } else if let Some(st) = ctx.db.with_snapshot(|s| {
            s.structures
                .get(&ent_id)
                .filter(|st| st.solar_system_id == sys_id)
                .map(|st| (st.type_id as i64, (st.position[0], st.position[1], st.position[2])))
        }) {
            st
        } else if Some(ent_id) == session.ship_id {
            let handle = ctx.systems.get_or_spawn(sys_id);
            if let Ok(Some((_, ball))) = handle.get_ball_snapshot(ent_id).await {
                let ship_type = ctx.db.with_snapshot(|s| {
                    s.items.get(&ent_id).map(|it| it.type_id as i64).unwrap_or(5)
                });
                (ship_type, (ball.position.x, ball.position.y, ball.position.z))
            } else {
                return Err(BridgeRpcError {
                    code: "ShipNotInSpace".into(),
                    message: "Cannot bookmark active ship when absent from ballpark".into(),
                });
            }
        } else {
            let sys_handle = ctx.systems.get_or_spawn(sys_id);
            let is_cloaked = sys_handle
                .get_combat_state(ent_id)
                .await
                .ok()
                .flatten()
                .is_some_and(|cs| cs.is_cloaked);
            if !is_cloaked {
                if let Ok(Some((_, ball))) = sys_handle.get_ball_snapshot(ent_id).await {
                    let Some(caller_ship_id) = session.ship_id else {
                        return Err(BridgeRpcError {
                            code: "UserError".into(),
                            message: "Must be in space on grid to bookmark dynamic in-space entities".into(),
                        });
                    };
                    let caller_ball = sys_handle
                        .get_ball_snapshot(caller_ship_id)
                        .await
                        .ok()
                        .flatten()
                        .map(|(_, b)| b);
                    let Some(caller_ball) = caller_ball else {
                        return Err(BridgeRpcError {
                            code: "UserError".into(),
                            message: "Caller ship not found in ballpark".into(),
                        });
                    };
                    let dist = ball.position.distance(caller_ball.position);
                    if dist > 8_000_000.0 {
                        return Err(BridgeRpcError {
                            code: "UserError".into(),
                            message: format!("Entity {ent_id} is off-grid (distance > 8,000 km)"),
                        });
                    }
                    let ent_type = ctx.db.with_snapshot(|s| {
                        s.items
                            .get(&ent_id)
                            .filter(|it| it.location_id == sys_id as i64)
                            .map(|it| it.type_id as i64)
                    }).unwrap_or(5);
                    (ent_type, (ball.position.x, ball.position.y, ball.position.z))
                } else {
                    return Err(BridgeRpcError {
                        code: "UserError".into(),
                        message: format!("Entity {ent_id} not found or not visible in current solar system"),
                    });
                }
            } else {
                return Err(BridgeRpcError {
                    code: "UserError".into(),
                    message: format!("Entity {ent_id} is cloaked or not visible"),
                });
            }
        }
    } else if let Some(st_id) = session.station_id {
        let (typ, pos) = ctx
            .sde
            .stations
            .get(&st_id)
            .map(|st| (st.station_type_id as i64, (st.position.x, st.position.y, st.position.z)))
            .unwrap_or_else(|| {
                let pos = ctx
                    .sde
                    .solar_systems
                    .get(&sys_id)
                    .and_then(|s| s.stations.iter().find(|st| st.station_id == st_id))
                    .map(|st| (st.position.x, st.position.y, st.position.z))
                    .unwrap_or((0.0, 0.0, 0.0));
                (5, pos)
            });
        (typ, pos)
    } else if let Some(struct_id) = session.structure_id {
        let (typ, pos) = ctx
            .db
            .with_snapshot(|snap| {
                snap.structures
                    .get(&struct_id)
                    .map(|s| (s.type_id as i64, (s.position[0], s.position[1], s.position[2])))
            })
            .unwrap_or((5, (0.0, 0.0, 0.0)));
        (typ, pos)
    } else if let Some(ship_id) = session.ship_id {
        let handle = ctx.systems.get_or_spawn(sys_id);
        if let Ok(Some((_, ball))) = handle.get_ball_snapshot(ship_id).await {
            let ship_type = ctx.db.with_snapshot(|s| {
                s.items.get(&ship_id).map(|it| it.type_id as i64).unwrap_or(5)
            });
            (ship_type, (ball.position.x, ball.position.y, ball.position.z))
        } else {
            return Err(BridgeRpcError {
                code: "ShipNotInSpace".into(),
                message: "Cannot bookmark in-space position when ship ball is absent".into(),
            });
        }
    } else {
        (5, (0.0, 0.0, 0.0))
    };

    let now_unix = ctx.now_unix();
    let clamped_expiry = expiry_hours.clamp(0, 87600);
    let (bm_id, is_shared_folder, corp_id) = ctx.db.try_with_mut_snapshot(|snap| {
        let is_shared = folder_id
            .and_then(|fid| snap.bookmark_folders.get(&fid))
            .map(|f| f.is_shared)
            .unwrap_or(false);
        if is_shared {
            let shared_count = snap.bookmarks.values().filter(|b| b.folder_id == folder_id).count();
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
                memo: memo_str.to_string(),
                folder_id,
                is_shared,
            },
        );
        if clamped_expiry > 0 {
            let exp_secs = (clamped_expiry as u64).saturating_mul(3600);
            snap.bookmark_expiries.insert(id, now_unix.saturating_add(exp_secs));
        }
        if let Some(sfid) = subfolder_id {
            let Some(fid) = folder_id else {
                return Err(BridgeRpcError {
                    code: "UserError".into(),
                    message: "Subfolder bookmarking requires a parent folder".into(),
                });
            };
            if snap.bookmark_subfolder_parents.get(&sfid) != Some(&fid) {
                return Err(BridgeRpcError {
                    code: "UserError".into(),
                    message: format!("Subfolder {sfid} does not belong to folder {fid}"),
                });
            }
            snap.bookmark_subfolders.insert(id, sfid);
        }
        let cid = session
            .corporation_id
            .or_else(|| snap.characters.get(&char_id).map(|c| c.corporation_id));
        Ok((id, is_shared, cid))
    })?;

    if is_shared_folder {
        if let Some(cid) = corp_id {
            ctx.notify(
                NotificationTarget::Corporation(cid),
                "OnSharedBookmarksFolderUpdated",
                "corpid",
                acl_folder_updates_arg(
                    folder_id.unwrap_or(0),
                    BOOKMARKS_ADDED,
                    PyRep::List(vec![PyRep::Integer(bm_id)]),
                ),
            );
        }
    }

    Ok(PyRep::Tuple(vec![
        PyRep::Integer(bm_id),
        explicit_entity_id.map(PyRep::Integer).unwrap_or(PyRep::Integer(sys_id as i64)),
        PyRep::Integer(derived_type_id),
        PyRep::Real(x),
        PyRep::Real(y),
        PyRep::Real(z),
        PyRep::Integer(sys_id as i64),
        folder_id.map(PyRep::Integer).unwrap_or(PyRep::None),
    ]))
}

/// `accessGroupBookmarkMgr.GetMyActiveBookmarks`
pub(crate) async fn handle_get_my_active_bookmarks(
    ctx: &ServerContext,
    session: &SessionState,
    _args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let now_unix = ctx.now_unix();
    let rows = ctx.db.try_with_mut_snapshot(|snap| {
        let mut expired_ids = Vec::new();
        snap.bookmarks.retain(|id, b| {
            let exp = snap.bookmark_expiries.get(id).copied().unwrap_or(0);
            let expired = exp > 0 && now_unix >= exp;
            if expired && b.owner_id == char_id {
                expired_ids.push(*id);
                false
            } else {
                true
            }
        });
        for id in expired_ids {
            snap.bookmark_expiries.remove(&id);
            snap.bookmark_subfolders.remove(&id);
        }
        let mut rows = Vec::new();
        for b in snap.bookmarks.values() {
            let visible = if !b.is_shared {
                b.owner_id == char_id
            } else if let Some(fid) = b.folder_id {
                if let Some(f) = snap.bookmark_folders.get(&fid) {
                    let lvl = folder_access_level(snap, session, char_id, f);
                    let is_active = snap
                        .bookmark_folder_known_by
                        .get(&(fid, char_id))
                        .copied()
                        .unwrap_or(false);
                    lvl >= AccessLevel::View && is_active
                } else {
                    false
                }
            } else {
                false
            };

            if visible {
                let subfolder_id = snap.bookmark_subfolders.get(&b.bookmark_id).copied();
                rows.push(vec![
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
                    subfolder_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                ]);
            }
        }
        rows.sort_by_key(|r| r[0].as_i64().unwrap_or(0));
        Ok(rows)
    })?;

    const BM_COLUMNS: [&str; 11] = [
        "bookmarkID",
        "label",
        "solarSystemID",
        "x",
        "y",
        "z",
        "itemID",
        "memo",
        "folderID",
        "isShared",
        "subfolderID",
    ];
    Ok(acl_active_bookmarks(ctx, session, char_id, &BM_COLUMNS, rows))
}

/// `accessGroupBookmarkMgr.UpdateBookmark`
pub(crate) async fn handle_update_bookmark(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let Some(bm_id) = args.first().and_then(|a| a.as_i64()) else {
        return Err(BridgeRpcError {
            code: "InvalidArgument".into(),
            message: "UpdateBookmark requires bookmarkID".into(),
        });
    };

    let (folder_id_opt, subfolder_id_opt, memo_opt, label_opt, expiry_hours_opt) =
        if args.len() >= 7 {
            let fid = args.get(2).and_then(|a| a.as_i64());
            let sfid = args.get(3).and_then(|a| a.as_i64());
            let m = args.get(4).and_then(|a| a.as_str());
            let l = args.get(5).and_then(|a| a.as_str());
            let eh = args.get(6).and_then(|a| a.as_i64());
            (fid, sfid, m, l, eh)
        } else {
            let l = args.get(1).and_then(|a| a.as_str());
            let m = args.get(2).and_then(|a| a.as_str());
            let fid = args.get(3).and_then(|a| a.as_i64());
            let eh = args.get(4).and_then(|a| a.as_i64());
            (fid, None, m, l, eh)
        };

    let (is_shared, folder_id, corp_id, expiry_unix) = ctx.db.try_with_mut_snapshot(|snap| {
        let Some(bm_rec) = snap.bookmarks.get(&bm_id).cloned() else {
            return Err(BridgeRpcError {
                code: "UserError".into(),
                message: format!("Bookmark {bm_id} not found"),
            });
        };
        let can_edit = if bm_rec.owner_id == char_id {
            true
        } else if let Some(fid) = bm_rec.folder_id {
            if let Some(f) = snap.bookmark_folders.get(&fid) {
                folder_access_level(snap, session, char_id, f) >= AccessLevel::Manage
            } else {
                false
            }
        } else {
            false
        };
        if !can_edit {
            return Err(BridgeRpcError {
                code: "AccessDenied".into(),
                message: "Cannot edit bookmark without manage rights".into(),
            });
        }

        let new_is_shared = if let Some(new_fid) = folder_id_opt {
            if new_fid != 0 && new_fid != char_id {
                let Some(tf) = snap.bookmark_folders.get(&new_fid) else {
                    return Err(BridgeRpcError {
                        code: "UserError".into(),
                        message: format!("Destination folder {new_fid} not found"),
                    });
                };
                if folder_access_level(snap, session, char_id, tf) < AccessLevel::Use {
                    return Err(BridgeRpcError {
                        code: "UserError".into(),
                        message: "No use access to target bookmark folder".into(),
                    });
                }
                tf.is_shared
            } else {
                false
            }
        } else {
            bm_rec.is_shared
        };

        if let Some(new_fid) = folder_id_opt {
            if !new_is_shared && bm_rec.is_shared {
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
            } else if new_is_shared && bm_rec.folder_id != Some(new_fid) {
                let shared_count = snap
                    .bookmarks
                    .values()
                    .filter(|b| b.folder_id == Some(new_fid))
                    .count();
                if shared_count >= 500 {
                    return Err(BridgeRpcError {
                        code: "MaxSharedFolderBookmarksReached".into(),
                        message: "Shared folder has reached the maximum of 500 bookmarks".into(),
                    });
                }
            }
        }

        let bm = snap.bookmarks.get_mut(&bm_id).unwrap();
        if let Some(new_fid) = folder_id_opt {
            bm.folder_id = Some(new_fid);
            bm.is_shared = new_is_shared;
        }
        if let Some(sfid) = subfolder_id_opt {
            let eff_fid = bm.folder_id.unwrap_or(0);
            if eff_fid == 0 || eff_fid == char_id || snap.bookmark_subfolder_parents.get(&sfid) != Some(&eff_fid) {
                return Err(BridgeRpcError {
                    code: "UserError".into(),
                    message: format!("Subfolder {sfid} does not belong to folder {eff_fid}"),
                });
            }
            snap.bookmark_subfolders.insert(bm_id, sfid);
        } else if folder_id_opt.is_some() {
            snap.bookmark_subfolders.remove(&bm_id);
        }
        if let Some(l) = label_opt {
            bm.label = l.to_string();
        }
        if let Some(m) = memo_opt {
            bm.memo = m.to_string();
        }

        let (is_shared, fid) = (bm.is_shared, bm.folder_id);
        let now_unix = ctx.now_unix();
        let exp_unix = if let Some(eh) = expiry_hours_opt {
            let clamped = eh.clamp(0, 87600);
            if clamped > 0 {
                let exp = now_unix.saturating_add((clamped as u64).saturating_mul(3600));
                snap.bookmark_expiries.insert(bm_id, exp);
                exp
            } else {
                snap.bookmark_expiries.remove(&bm_id);
                0
            }
        } else {
            snap.bookmark_expiries.get(&bm_id).copied().unwrap_or(0)
        };

        let cid = session
            .corporation_id
            .or_else(|| snap.characters.get(&char_id).map(|c| c.corporation_id));
        Ok((is_shared, fid, cid, exp_unix))
    })?;

    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnRefreshBookmarks",
        "charid",
        vec![],
    );
    if is_shared {
        if let Some(cid) = corp_id {
            ctx.notify(
                NotificationTarget::Corporation(cid),
                "OnSharedBookmarksFolderUpdated",
                "corpid",
                acl_folder_updates_arg(
                    folder_id.unwrap_or(0),
                    BOOKMARKS_UPDATED,
                    PyRep::Integer(bm_id),
                ),
            );
        }
    }
    Ok(PyRep::keyval([
        ("bookmarkID", PyRep::Integer(bm_id)),
        ("updated", PyRep::Bool(true)),
        ("expiryUnix", PyRep::Integer(expiry_unix as i64)),
    ]))
}

/// `accessGroupBookmarkMgr.DeleteBookmarks`
pub(crate) async fn handle_delete_bookmarks(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let bm_ids = extract_id_list(args.first());
    if bm_ids.is_empty() {
        return Err(BridgeRpcError {
            code: "InvalidArgument".into(),
            message: "DeleteBookmarks requires bookmarkID".into(),
        });
    }
    let folder_id_arg = args.get(1).and_then(|a| a.as_i64());

    let (any_shared_folder, corp_id) = ctx.db.try_with_mut_snapshot(|snap| {
        let corp_id = session
            .corporation_id
            .or_else(|| snap.characters.get(&char_id).map(|c| c.corporation_id));
        let mut any_shared = folder_id_arg;
        for &bm_id in &bm_ids {
            let Some(bm) = snap.bookmarks.get(&bm_id) else {
                return Err(BridgeRpcError {
                    code: "UserError".into(),
                    message: format!("Bookmark {bm_id} not found"),
                });
            };
            let can_del = if bm.owner_id == char_id {
                true
            } else if let Some(fid) = bm.folder_id {
                if let Some(f) = snap.bookmark_folders.get(&fid) {
                    folder_access_level(snap, session, char_id, f) >= AccessLevel::Manage
                } else {
                    false
                }
            } else {
                false
            };
            if !can_del {
                return Err(BridgeRpcError {
                    code: "AccessDenied".into(),
                    message: "Cannot delete another character's bookmark without manage rights".into(),
                });
            }
            if bm.is_shared && any_shared.is_none() {
                any_shared = bm.folder_id;
            }
            snap.bookmarks.remove(&bm_id);
            snap.bookmark_expiries.remove(&bm_id);
            snap.bookmark_subfolders.remove(&bm_id);
        }
        Ok((any_shared, corp_id))
    })?;

    if let (Some(fid_opt), Some(cid)) = (any_shared_folder, corp_id) {
        ctx.notify(
            NotificationTarget::Corporation(cid),
            "OnSharedBookmarksFolderUpdated",
            "corpid",
            acl_folder_updates_arg(
                fid_opt,
                BOOKMARKS_REMOVED,
                PyRep::List(bm_ids.iter().copied().map(PyRep::Integer).collect()),
            ),
        );
    }

    Ok(PyRep::List(bm_ids.iter().copied().map(PyRep::Integer).collect()))
}

/// `accessGroupBookmarkMgr.CopyBookmarksToFolderAndSubfolder` / `MoveBookmarksToFolderAndSubfolder`
pub(crate) async fn handle_copy_move_bookmarks(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
    is_move: bool,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let (target_folder_id, target_subfolder_id, bm_ids) = if args.len() >= 3 {
        let fid = args[0].as_i64();
        let sfid = args[1].as_i64();
        let ids = extract_id_list(args.get(2));
        (fid, sfid, ids)
    } else {
        return Err(BridgeRpcError {
            code: "InvalidArgument".into(),
            message: "Move/CopyBookmarksToFolderAndSubfolder requires (folderID, subfolderID, bookmarkIDs, target)".into(),
        });
    };

    if bm_ids.len() > 20 {
        return Err(BridgeRpcError {
            code: "BookmarkCopyBatchLimitExceeded".into(),
            message: "Cannot copy or move more than 20 bookmarks in a single operation".into(),
        });
    }

    let (out_items, out_records) = ctx.db.try_with_mut_snapshot(|snap| {
        let is_shared_target = if let Some(fid) = target_folder_id {
            if fid != 0 && fid != char_id {
                let Some(tf) = snap.bookmark_folders.get(&fid) else {
                    return Err(BridgeRpcError {
                        code: "UserError".into(),
                        message: format!("Bookmark folder {fid} not found"),
                    });
                };
                if folder_access_level(snap, session, char_id, tf) < AccessLevel::Use {
                    return Err(BridgeRpcError {
                        code: "UserError".into(),
                        message: "No write access to target bookmark folder".into(),
                    });
                }
                tf.is_shared
            } else {
                false
            }
        } else {
            false
        };

        if let Some(sfid) = target_subfolder_id {
            let Some(fid) = target_folder_id else {
                return Err(BridgeRpcError {
                    code: "UserError".into(),
                    message: "Target subfolder requires a target folder".into(),
                });
            };
            if snap.bookmark_subfolder_parents.get(&sfid) != Some(&fid) {
                return Err(BridgeRpcError {
                    code: "UserError".into(),
                    message: format!("Subfolder {sfid} does not belong to target folder {fid}"),
                });
            }
        }

        if is_shared_target {
            let current_in_folder = snap
                .bookmarks
                .values()
                .filter(|b| b.folder_id == target_folder_id)
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
                return Err(BridgeRpcError {
                    code: "MaxSharedFolderBookmarksReached".into(),
                    message: "Shared folder capacity (500) exceeded".into(),
                });
            }
        } else {
            let current_personal = snap
                .bookmarks
                .values()
                .filter(|b| b.owner_id == char_id && !b.is_shared)
                .count();
            let added_count = if is_move {
                bm_ids
                    .iter()
                    .filter(|&&id| {
                        snap.bookmarks.get(&id).is_some_and(|b| b.is_shared || b.owner_id != char_id)
                    })
                    .count()
            } else {
                bm_ids.len()
            };
            if current_personal + added_count > 500 {
                return Err(BridgeRpcError {
                    code: "MaxBookmarksReached".into(),
                    message: "Personal bookmark capacity (500) exceeded".into(),
                });
            }
        }

        let mut out_ids = Vec::new();
        let mut out_recs = Vec::new();
        for &bm_id in &bm_ids {
            let Some(orig) = snap.bookmarks.get(&bm_id).cloned() else {
                return Err(BridgeRpcError {
                    code: "UserError".into(),
                    message: format!("Bookmark {bm_id} not found"),
                });
            };
            let can_access = if is_move {
                orig.owner_id == char_id
                    || orig.folder_id.is_some_and(|fid| {
                        snap.bookmark_folders.get(&fid).is_some_and(|f| {
                            folder_access_level(snap, session, char_id, f) >= AccessLevel::Manage
                        })
                    })
            } else {
                orig.owner_id == char_id
                    || orig.folder_id.is_some_and(|fid| {
                        snap.bookmark_folders.get(&fid).is_some_and(|f| {
                            folder_access_level(snap, session, char_id, f) >= AccessLevel::View
                        })
                    })
            };
            if !can_access {
                return Err(BridgeRpcError {
                    code: "UserError".into(),
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
                } else {
                    snap.bookmark_subfolders.remove(&bm_id);
                }
                out_ids.push(bm_id);
                out_recs.push(orig);
            } else {
                snap.next_entity_id += 1;
                let new_id = snap.next_entity_id;
                let new_rec = BookmarkRecord {
                    bookmark_id: new_id,
                    owner_id: char_id,
                    folder_id: target_folder_id,
                    is_shared: is_shared_target,
                    ..orig
                };
                snap.bookmarks.insert(new_id, new_rec.clone());
                if let Some(sfid) = target_subfolder_id {
                    snap.bookmark_subfolders.insert(new_id, sfid);
                }
                out_ids.push(new_id);
                out_recs.push(new_rec);
            }
        }
        Ok((out_ids, out_recs))
    })?;

    let folder_val = target_folder_id.map(PyRep::Integer).unwrap_or(PyRep::None);
    if is_move {
        let moved_list = out_items
            .into_iter()
            .map(|id| {
                PyRep::keyval([
                    ("bookmarkID", PyRep::Integer(id)),
                    ("folderID", folder_val.clone()),
                    (
                        "subfolderID",
                        target_subfolder_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                    ),
                ])
            })
            .collect();
        Ok(PyRep::Tuple(vec![PyRep::List(moved_list), PyRep::None]))
    } else {
        let copied_dict = out_records
            .into_iter()
            .map(|rec| {
                let kv = PyRep::keyval([
                    ("bookmarkID", PyRep::Integer(rec.bookmark_id)),
                    ("locationID", PyRep::Integer(rec.solar_system_id as i64)),
                    ("folderID", folder_val.clone()),
                    (
                        "subfolderID",
                        target_subfolder_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                    ),
                    ("label", PyRep::String(rec.label)),
                    ("memo", PyRep::String(rec.memo)),
                    ("x", PyRep::Real(rec.x)),
                    ("y", PyRep::Real(rec.y)),
                    ("z", PyRep::Real(rec.z)),
                ]);
                (PyRep::Integer(rec.bookmark_id), kv)
            })
            .collect();
        Ok(PyRep::Tuple(vec![PyRep::Dict(copied_dict), PyRep::None]))
    }
}
