//! Bookmarks (`SO-4`: `bookmark`, `accessGroupBookmarkMgr`).

use super::fleet_social_chat::extract_id_list;
use crate::router::{cache_notifications::NotificationTarget, ServerContext};
use eve_db::{corp_roles, BookmarkFolderRecord, BookmarkRecord};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

#[derive(Clone, Debug, Default)]
pub(super) struct BookmarkExtrasState {
    pub(super) bookmark_expiry: HashMap<i64, u64>, // bookmark_id -> expiry_unix (0 = never)
    pub(super) bookmark_subfolder: HashMap<i64, i64>, // bookmark_id -> subfolder_id
    pub(super) active_shared_folders: HashMap<i64, HashMap<i64, bool>>, // char_id -> (folder_id -> is_active)
    pub(super) active_probes: HashMap<i64, Vec<(i64, [f64; 3], f64, u64)>>, // char_id -> [(probe_id, pos, radius_m, expiry_unix)]
    pub(super) escalations: HashMap<i64, Vec<(i64, String, u32, u64)>>, // char_id -> [(esc_id, dungeon_name, dest_sys_id, expiry_unix)]
}

pub(super) static BM_SCAN_STATE: LazyLock<Mutex<BookmarkExtrasState>> =
    LazyLock::new(|| Mutex::new(BookmarkExtrasState::default()));

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
        | ("accessGroupBookmarkMgr", "BookmarkStaticLocation") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let label = args.first().and_then(|a| a.as_str()).unwrap_or("Safe Spot").trim();
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

            let (explicit_entity_id, memo, folder_id) = if args.get(1).and_then(|a| a.as_str()).is_some() {
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
            let subfolder_id = args.get(4).and_then(|a| a.as_i64());
            let expiry_hours = args.get(5).and_then(|a| a.as_i64()).unwrap_or(0);

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
                let corp_id = session
                    .corporation_id
                    .or_else(|| snap.characters.get(&char_id).map(|c| c.corporation_id));
                Ok((id, is_shared_folder, corp_id))
            });

            match res {
                Ok((bm_id, is_shared_folder, corp_id)) => {
                    {
                        let mut st = BM_SCAN_STATE.lock().unwrap();
                        if expiry_hours > 0 {
                            st.bookmark_expiry
                                .insert(bm_id, now_unix + (expiry_hours as u64 * 3600));
                        }
                        if let Some(sfid) = subfolder_id {
                            st.bookmark_subfolder.insert(bm_id, sfid);
                        }
                    }
                    ctx.notify(
                        NotificationTarget::Character(char_id),
                        "OnBookmarkCreated",
                        "charid",
                        vec![PyRep::Integer(bm_id), PyRep::String(label.to_string())],
                    );
                    if is_shared_folder {
                        if let Some(cid) = corp_id {
                            ctx.notify(
                                NotificationTarget::Corporation(cid),
                                "OnSharedBookmarksFolderUpdated",
                                "corpid",
                                vec![
                                    folder_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                                    PyRep::Integer(bm_id),
                                ],
                            );
                        }
                    }
                    Some(Ok(PyRep::keyval([
                        ("bookmarkID", PyRep::Integer(bm_id)),
                        ("label", PyRep::String(label.to_string())),
                        ("x", PyRep::Real(x)),
                        ("y", PyRep::Real(y)),
                        ("z", PyRep::Real(z)),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("bookmark", "GetBookmarks")
        | ("accessGroupBookmarkMgr", "GetMyActiveBookmarks") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let now_unix = ctx.now_unix();
            let (expiries, subfolders) = {
                let st = BM_SCAN_STATE.lock().unwrap();
                (st.bookmark_expiry.clone(), st.bookmark_subfolder.clone())
            };
            let rows = ctx.db.try_with_mut_snapshot(|snap| {
                snap.bookmarks.retain(|id, _| {
                    expiries.get(id).map(|&exp| exp == 0 || now_unix < exp).unwrap_or(true)
                });
                let corp_id_opt = session
                    .corporation_id
                    .or_else(|| snap.characters.get(&char_id).map(|c| c.corporation_id));
                let list: Vec<Vec<PyRep>> = snap
                    .bookmarks
                    .values()
                    .filter(|b| {
                        if let Some(&exp) = expiries.get(&b.bookmark_id) {
                            if exp > 0 && now_unix >= exp {
                                return false;
                            }
                        }
                        b.owner_id == char_id || (b.is_shared && corp_id_opt.is_some())
                    })
                    .map(|b| {
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
                            subfolders
                                .get(&b.bookmark_id)
                                .copied()
                                .map(PyRep::Integer)
                                .unwrap_or(PyRep::None),
                        ]
                    })
                    .collect();
                Ok::<_, BridgeRpcError>(list)
            }).unwrap_or_default();
            Some(Ok(PyRep::rowset(
                vec![
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
                ],
                rows,
            )))
        }

        ("bookmark", "UpdateBookmark")
        | ("accessGroupBookmarkMgr", "UpdateBookmark") => {
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

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let corp_id = session
                    .corporation_id
                    .or_else(|| snap.characters.get(&char_id).map(|c| c.corporation_id));
                let Some(bm) = snap.bookmarks.get_mut(&bm_id) else {
                    return Err(BridgeRpcError {
                        code: "BookmarkNotFound".into(),
                        message: format!("Bookmark {bm_id} not found"),
                    });
                };
                if bm.owner_id != char_id && !bm.is_shared {
                    return Err(BridgeRpcError {
                        code: "AccessDenied".into(),
                        message: "Cannot edit another character's bookmark".into(),
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
                Ok((bm.is_shared, bm.folder_id, corp_id))
            });
            match res {
                Ok((is_shared, folder_id, corp_id)) => {
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
                                vec![
                                    folder_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                                    PyRep::Integer(bm_id),
                                ],
                            );
                        }
                    }
                    Some(Ok(PyRep::keyval([
                        ("bookmarkID", PyRep::Integer(bm_id)),
                        ("updated", PyRep::Bool(true)),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("bookmark", "DeleteBookmarks")
        | ("accessGroupBookmarkMgr", "DeleteBookmarks") => {
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
                    if bm.owner_id != char_id && !bm.is_shared {
                        return Err(BridgeRpcError {
                            code: "AccessDenied".into(),
                            message: "Cannot delete another character's bookmark".into(),
                        });
                    }
                    if bm.is_shared {
                        any_shared_folder = Some(bm.folder_id);
                    }
                    snap.bookmarks.remove(&bm_id);
                }
                Ok((any_shared_folder, corp_id))
            });
            match res {
                Ok((any_shared_folder, corp_id)) => {
                    ctx.notify(
                        NotificationTarget::Character(char_id),
                        "OnRefreshBookmarks",
                        "charid",
                        vec![PyRep::Integer(bm_ids[0])],
                    );
                    if let (Some(fid_opt), Some(cid)) = (any_shared_folder, corp_id) {
                        ctx.notify(
                            NotificationTarget::Corporation(cid),
                            "OnSharedBookmarksFolderUpdated",
                            "corpid",
                            vec![
                                fid_opt.map(PyRep::Integer).unwrap_or(PyRep::None),
                                PyRep::Integer(bm_ids[0]),
                            ],
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
        | ("accessGroupBookmarkMgr", "CopyBookmarksToFolderAndSubfolder")
        | ("bookmark", "MoveBookmarksToFolderAndSubfolder")
        | ("accessGroupBookmarkMgr", "MoveBookmarksToFolderAndSubfolder") => {
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
                let is_shared_target = if let Some(fid) = target_folder_id {
                    let Some(tf) = snap.bookmark_folders.get(&fid) else {
                        return Err(BridgeRpcError { code: "FolderNotFound".into(), message: format!("Bookmark folder {fid} not found") });
                    };
                    if tf.owner_id != char_id && !tf.is_shared {
                        return Err(BridgeRpcError { code: "BookmarkFolderAccessDenied".into(), message: "No write access to target bookmark folder".into() });
                    }
                    tf.is_shared
                } else {
                    false
                };
                if !is_move {
                    let current_in_folder = snap.bookmarks.values().filter(|b| b.folder_id == target_folder_id && (is_shared_target || b.owner_id == char_id)).count();
                    if current_in_folder + bm_ids.len() > 500 {
                        let code = if is_shared_target { "MaxSharedFolderBookmarksReached" } else { "MaxBookmarksReached" };
                        return Err(BridgeRpcError { code: code.into(), message: "Bookmark folder capacity (500) exceeded".into() });
                    }
                }
                let mut out_ids = Vec::new();
                for &bm_id in &bm_ids {
                    let Some(orig) = snap.bookmarks.get(&bm_id).cloned() else {
                        return Err(BridgeRpcError {
                            code: "BookmarkNotFound".into(),
                            message: format!("Bookmark {bm_id} not found"),
                        });
                    };
                    if orig.owner_id != char_id && !orig.is_shared {
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
                        out_ids.push(new_id);
                    }
                }
                Ok(out_ids)
            });

            match res {
                Ok(out_ids) => {
                    if let Some(sfid) = target_subfolder_id {
                        let mut st = BM_SCAN_STATE.lock().unwrap();
                        for &id in &out_ids {
                            st.bookmark_subfolder.insert(id, sfid);
                        }
                    }
                    Some(Ok(PyRep::keyval([
                        ("bookmarkIDs", PyRep::List(out_ids.into_iter().map(PyRep::Integer).collect())),
                        ("folderID", target_folder_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("bookmark", "AddFolder")
        | ("accessGroupBookmarkMgr", "AddFolder")
        | ("bookmark", "UpdateFolder")
        | ("accessGroupBookmarkMgr", "UpdateFolder") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            if method == "UpdateFolder" {
                let folder_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
                let new_name = args.get(1).and_then(|a| a.as_str()).unwrap_or("Updated Folder");
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
                    Ok(())
                });
                return match res {
                    Ok(()) => Some(Ok(PyRep::keyval([
                        ("folderID", PyRep::Integer(folder_id)),
                        ("folderName", PyRep::String(new_name.to_string())),
                    ]))),
                    Err(e) => Some(Err(e)),
                };
            }

            let name = args.first().and_then(|a| a.as_str()).unwrap_or("New Folder");
            let is_shared = matches!(args.get(1), Some(PyRep::Bool(true)))
                || args.get(1).and_then(|a| a.as_i64()).is_some_and(|v| v > 0);
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
                        },
                    );
                    Ok(id)
                });
                return match res {
                    Ok(folder_id) => {
                        ctx.notify(
                            NotificationTarget::Corporation(corp_id),
                            "OnSharedBookmarksFolderUpdated",
                            "corpid",
                            vec![PyRep::Integer(folder_id), PyRep::Integer(corp_id)],
                        );
                        Some(Ok(PyRep::keyval([
                            ("folderID", PyRep::Integer(folder_id)),
                            ("folderName", PyRep::String(name.to_string())),
                            ("isShared", PyRep::Bool(true)),
                            ("corporationID", PyRep::Integer(corp_id)),
                        ])))
                    }
                    Err(e) => Some(Err(e)),
                };
            }

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let personal_root_count = snap
                    .bookmark_folders
                    .values()
                    .filter(|f| f.owner_id == char_id && !f.is_shared && !f.folder_name.contains('/'))
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
                    },
                );
                Ok(id)
            });
            match res {
                Ok(folder_id) => Some(Ok(PyRep::keyval([
                    ("folderID", PyRep::Integer(folder_id)),
                    ("folderName", PyRep::String(name.to_string())),
                    ("isShared", PyRep::Bool(false)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        ("bookmark", "GetFolders")
        | ("accessGroupBookmarkMgr", "GetFolderInfo")
        | ("accessGroupBookmarkMgr", "SearchFoldersWithAdminAccess") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let corp_id_opt = session.corporation_id;
            let rows: Vec<Vec<PyRep>> = ctx.db.with_snapshot(|snap| {
                snap.bookmark_folders
                    .values()
                    .filter(|f| f.owner_id == char_id || (f.is_shared && f.corporation_id == corp_id_opt))
                    .map(|f| {
                        vec![
                            PyRep::Integer(f.folder_id),
                            PyRep::String(f.folder_name.clone()),
                            PyRep::Integer(f.owner_id),
                            PyRep::Bool(f.is_shared),
                            f.corporation_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                        ]
                    })
                    .collect()
            });
            Some(Ok(PyRep::rowset(
                vec!["folderID", "folderName", "ownerID", "isShared", "corporationID"],
                rows,
            )))
        }

        ("bookmark", "DeleteFolder")
        | ("accessGroupBookmarkMgr", "DeleteFolder") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let Some(folder_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "DeleteFolder requires folderID".into(),
                }));
            };
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(f) = snap.bookmark_folders.get(&folder_id).cloned() else {
                    return Err(BridgeRpcError {
                        code: "FolderNotFound".into(),
                        message: format!("Folder {folder_id} not found"),
                    });
                };
                if f.owner_id != char_id {
                    return Err(BridgeRpcError {
                        code: "AccessDenied".into(),
                        message: "Cannot delete another character's folder".into(),
                    });
                }
                snap.bookmark_folders.remove(&folder_id);
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

        ("bookmark", "CreateSubfolder")
        | ("accessGroupBookmarkMgr", "CreateSubfolder")
        | ("accessGroupBookmarkMgr", "UpdateSubfolder") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let Some(parent_folder_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "CreateSubfolder requires parent folderID".into(),
                }));
            };
            let sub_name = args.get(1).and_then(|a| a.as_str()).unwrap_or("Subfolder");
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(parent) = snap.bookmark_folders.get(&parent_folder_id).cloned() else {
                    return Err(BridgeRpcError {
                        code: "FolderNotFound".into(),
                        message: format!("Parent folder {parent_folder_id} not found"),
                    });
                };
                let prefix = format!("{}/", parent.folder_name);
                let sub_count = snap
                    .bookmark_folders
                    .values()
                    .filter(|f| f.owner_id == char_id && f.folder_name.starts_with(&prefix))
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
                    },
                );
                Ok((id, full_name))
            });
            match res {
                Ok((subfolder_id, full_name)) => Some(Ok(PyRep::keyval([
                    ("subfolderID", PyRep::Integer(subfolder_id)),
                    ("folderID", PyRep::Integer(subfolder_id)),
                    ("parentFolderID", PyRep::Integer(parent_folder_id)),
                    ("subfolderName", PyRep::String(sub_name.to_string())),
                    ("folderName", PyRep::String(full_name)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        ("bookmark", "DeleteSubfolder")
        | ("accessGroupBookmarkMgr", "DeleteSubfolder") => {
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

        ("bookmark", "AddToKnownFolders")
        | ("accessGroupBookmarkMgr", "AddToKnownFolders")
        | ("accessGroupBookmarkMgr", "UpdateKnownFolderState") => {
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
                    {
                        let mut st = BM_SCAN_STATE.lock().unwrap();
                        let map = st.active_shared_folders.entry(char_id).or_default();
                        let current_active = map.iter().filter(|(&fid, &act)| fid != folder_id && act).count();
                        if active && current_active >= 3 {
                            return Some(Err(BridgeRpcError {
                                code: "MaxActiveSharedFoldersReached".into(),
                                message: "Maximum of 3 online shared bookmark folders reached".into(),
                            }));
                        }
                        map.insert(folder_id, active);
                    }
                    ctx.notify(
                        NotificationTarget::Character(char_id),
                        "OnSharedBookmarksFolderUpdated",
                        "charid",
                        vec![PyRep::Integer(folder_id), PyRep::Bool(active)],
                    );
                    if let Some(corp_id) = corp_id_opt {
                        ctx.notify(
                            NotificationTarget::Corporation(corp_id),
                            "OnSharedBookmarksFolderUpdated",
                            "corpid",
                            vec![PyRep::Integer(folder_id), PyRep::Integer(corp_id)],
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

        ("bookmark", "RemoveFromKnownFolders")
        | ("accessGroupBookmarkMgr", "RemoveFromKnownFolders") => {
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
            if let Some(map) = BM_SCAN_STATE.lock().unwrap().active_shared_folders.get_mut(&char_id) {
                map.remove(&folder_id);
            }
            ctx.notify(
                NotificationTarget::Character(char_id),
                "OnSharedBookmarksFolderUpdated",
                "charid",
                vec![PyRep::Integer(folder_id), PyRep::Bool(false)],
            );
            Some(Ok(PyRep::keyval([
                ("folderID", PyRep::Integer(folder_id)),
                ("subscribed", PyRep::Bool(false)),
            ])))
        }

        ("scanMgr", "BookmarkScanResult") | ("bookmark", "BookmarkScanResult") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let Some(sig_id) = args.first().and_then(|a| a.as_str()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "BookmarkScanResult requires signatureID".into(),
                }));
            };
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "Session has no active solar system".into(),
                }));
            };
            let label = args.get(1).and_then(|a| a.as_str()).unwrap_or(sig_id);
            let folder_id = args.get(2).and_then(|a| a.as_i64());

            let sig_res = ctx.db.with_snapshot(|snap| {
                snap.cosmic_signatures.get(&sys_id).and_then(|sigs| {
                    sigs.iter()
                        .find(|s| s.signature_id == sig_id)
                        .map(|s| (s.position, s.signal_strength))
                })
            });
            let Some((pos, signal_strength)) = sig_res else {
                return Some(Err(BridgeRpcError {
                    code: "SignatureNotFound".into(),
                    message: format!("Cosmic signature {sig_id} not found in system {sys_id}"),
                }));
            };
            if signal_strength < 1.0 {
                return Some(Err(BridgeRpcError {
                    code: "SignatureNotResolved".into(),
                    message: format!("Cosmic signature {sig_id} must be resolved to 100% before bookmarking"),
                }));
            }
            let bm_id = ctx
                .db
                .try_with_mut_snapshot(|snap| {
                    let is_shared_folder = folder_id
                        .and_then(|fid| snap.bookmark_folders.get(&fid))
                        .map(|f| f.is_shared)
                        .unwrap_or(false);
                    snap.next_entity_id += 1;
                    let id = snap.next_entity_id;
                    snap.bookmarks.insert(
                        id,
                        BookmarkRecord {
                            bookmark_id: id,
                            owner_id: char_id,
                            label: label.to_string(),
                            solar_system_id: sys_id,
                            x: pos[0],
                            y: pos[1],
                            z: pos[2],
                            item_id: None,
                            memo: format!("Scanned cosmic signature {sig_id}"),
                            folder_id,
                            is_shared: is_shared_folder,
                        },
                    );
                    Ok::<_, std::convert::Infallible>(id)
                })
                .unwrap();
            ctx.notify(
                NotificationTarget::Character(char_id),
                "OnBookmarkCreated",
                "charid",
                vec![PyRep::Integer(bm_id), PyRep::String(label.to_string())],
            );
            Some(Ok(PyRep::keyval([
                ("bookmarkID", PyRep::Integer(bm_id)),
                ("signatureID", PyRep::String(sig_id.to_string())),
                ("label", PyRep::String(label.to_string())),
                ("x", PyRep::Real(pos[0])),
                ("y", PyRep::Real(pos[1])),
                ("z", PyRep::Real(pos[2])),
            ])))
        }

        _ => None,
    }
}
