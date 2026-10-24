//! `accessGroupBookmarkMgr` — ACL-based Bookmark & Folder Service (17 RPC handlers).
//!
//! Fully implements V24.01 client expectations (`eve/client/script/ui/shared/bookmarks/bookmarkSvc.pyj`).
//!
//! Features:
//! - Strict V24 reply shapes matching `client_expectations.json`
//! - Folder creation, updates, and cascading deletion (personal <= 5, shared requires corp admin role)
//! - Subfolder management (max 5 personal, max 30 shared)
//! - Active/known shared folders subscriptions (max 3 active)
//! - Delegated bookmark operations (static location, active bookmarks, updates, delete, copy/move)
//! - Expiries, search, and notification emission

use super::fleet_social_access_group_bookmarks_ops as ops;
use super::fleet_social_bookmarks::has_corp_bookmark_admin_role;
use super::fleet_social_chat::extract_id_list;
use super::{is_bookmark_expired_in_snap, sweep_expired_bookmarks};
use crate::router::{cache_notifications::NotificationTarget, ServerContext};
use eve_db::BookmarkFolderRecord;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;

/// Check if a character (or their corp/alliance) has membership in an access group.
pub(crate) fn is_in_access_group(
    snap: &eve_db::WorldStateSnapshot,
    session: &SessionState,
    char_id: i64,
    group_id: i64,
) -> bool {
    if let Some(members) = snap.access_groups.get(&group_id) {
        if members.contains(&char_id) {
            return true;
        }
        if let Some(cid) = session.corporation_id {
            if members.contains(&cid) {
                return true;
            }
        }
        if let Some(aid) = session.alliance_id {
            if members.contains(&aid) {
                return true;
            }
        }
    }
    false
}

/// Dispatches all 17 `accessGroupBookmarkMgr` RPC methods.
pub(crate) async fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    if service != "accessGroupBookmarkMgr" {
        return None;
    }
    match method {
        // parity: rpc:accessGroupBookmarkMgr.BookmarkStaticLocation
        "BookmarkStaticLocation" => {
            Some(ops::handle_bookmark_static_location(ctx, session, args, kwargs).await)
        }

        // parity: rpc:accessGroupBookmarkMgr.GetMyActiveBookmarks
        "GetMyActiveBookmarks" => Some(ops::handle_get_my_active_bookmarks(ctx, session, args).await),

        // parity: rpc:accessGroupBookmarkMgr.UpdateBookmark
        "UpdateBookmark" => Some(ops::handle_update_bookmark(ctx, session, args).await),

        // parity: rpc:accessGroupBookmarkMgr.DeleteBookmarks
        "DeleteBookmarks" => Some(ops::handle_delete_bookmarks(ctx, session, args).await),

        // parity: rpc:accessGroupBookmarkMgr.CopyBookmarksToFolderAndSubfolder
        "CopyBookmarksToFolderAndSubfolder" => {
            Some(ops::handle_copy_move_bookmarks(ctx, session, args, false).await)
        }

        // parity: rpc:accessGroupBookmarkMgr.MoveBookmarksToFolderAndSubfolder
        "MoveBookmarksToFolderAndSubfolder" => {
            Some(ops::handle_copy_move_bookmarks(ctx, session, args, true).await)
        }

        // parity: rpc:accessGroupBookmarkMgr.AddFolder
        "AddFolder" => Some(handle_add_folder(ctx, session, args).await),

        // parity: rpc:accessGroupBookmarkMgr.UpdateFolder
        "UpdateFolder" => Some(handle_update_folder(ctx, session, args).await),

        // parity: rpc:accessGroupBookmarkMgr.DeleteFolder
        "DeleteFolder" => Some(handle_delete_folder(ctx, session, args).await),

        // parity: rpc:accessGroupBookmarkMgr.CreateSubfolder
        "CreateSubfolder" => Some(handle_create_subfolder(ctx, session, args).await),

        // parity: rpc:accessGroupBookmarkMgr.UpdateSubfolder
        "UpdateSubfolder" => Some(handle_update_subfolder(ctx, session, args).await),

        // parity: rpc:accessGroupBookmarkMgr.DeleteSubfolder
        "DeleteSubfolder" => Some(handle_delete_subfolder(ctx, session, args).await),

        // parity: rpc:accessGroupBookmarkMgr.AddToKnownFolders
        "AddToKnownFolders" => Some(handle_known_folders_state(ctx, session, args, true).await),

        // parity: rpc:accessGroupBookmarkMgr.UpdateKnownFolderState
        "UpdateKnownFolderState" => {
            Some(handle_known_folders_state(ctx, session, args, false).await)
        }

        // parity: rpc:accessGroupBookmarkMgr.RemoveFromKnownFolders
        "RemoveFromKnownFolders" => {
            Some(handle_remove_from_known_folders(ctx, session, args).await)
        }

        // parity: rpc:accessGroupBookmarkMgr.GetFolderInfo
        "GetFolderInfo" => Some(handle_get_folder_info(ctx, session, args).await),

        // parity: rpc:accessGroupBookmarkMgr.SearchFoldersWithAdminAccess
        "SearchFoldersWithAdminAccess" => {
            Some(handle_search_folders_with_admin_access(ctx, session, args).await)
        }

        _ => None,
    }
}

pub(crate) const FOLDER_UPDATED: i64 = 1;
pub(crate) const BOOKMARKS_ADDED: i64 = 2;
pub(crate) const BOOKMARKS_UPDATED: i64 = 3;
#[allow(dead_code)]
pub(crate) const BOOKMARKS_MOVED: i64 = 4;
pub(crate) const BOOKMARKS_REMOVED: i64 = 5;
pub(crate) const SUBFOLDER_ADDED: i64 = 6;
pub(crate) const SUBFOLDER_UPDATED: i64 = 7;
pub(crate) const SUBFOLDER_REMOVED: i64 = 8;

pub(crate) fn acl_folder_updates_arg(
    folder_id: i64,
    _update_type: i64,
    update_args: PyRep,
) -> Vec<PyRep> {
    vec![PyRep::List(vec![PyRep::Tuple(vec![
        PyRep::Tuple(vec![PyRep::Integer(folder_id)]),
        PyRep::String("FolderChanged".into()),
        update_args,
    ])])]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum AccessLevel {
    None = 0,
    View = 1,
    Use = 2,
    Manage = 3,
    Admin = 4,
}

pub(crate) fn folder_access_level(
    snap: &eve_db::WorldStateSnapshot,
    session: &SessionState,
    char_id: i64,
    f: &BookmarkFolderRecord,
) -> AccessLevel {
    // If this folder is a subfolder, resolve its access via its parent folder
    if let Some(&parent_id) = snap.bookmark_subfolder_parents.get(&f.folder_id) {
        if let Some(parent) = snap.bookmark_folders.get(&parent_id) {
            return folder_access_level(snap, session, char_id, parent);
        }
        return AccessLevel::None;
    }

    if !f.is_shared {
        return if f.owner_id == char_id {
            AccessLevel::Admin
        } else {
            AccessLevel::None
        };
    }

    if f.owner_id == char_id {
        return AccessLevel::Admin;
    }

    let mut level = AccessLevel::None;

    if f.admin_group_id.is_some_and(|gid| is_in_access_group(snap, session, char_id, gid)) {
        level = level.max(AccessLevel::Admin);
    }
    if f.manage_group_id.is_some_and(|gid| is_in_access_group(snap, session, char_id, gid)) {
        level = level.max(AccessLevel::Manage);
    }
    if f.use_group_id.is_some_and(|gid| is_in_access_group(snap, session, char_id, gid)) {
        level = level.max(AccessLevel::Use);
    }
    if f.view_group_id.is_some_and(|gid| is_in_access_group(snap, session, char_id, gid)) {
        level = level.max(AccessLevel::View);
    }

    // If any access group was configured, access is strictly governed by groups (no corp override)
    if f.admin_group_id.is_some()
        || f.manage_group_id.is_some()
        || f.use_group_id.is_some()
        || f.view_group_id.is_some()
    {
        return level;
    }

    // Legacy folder fallback (when no access groups are configured)
    if let (Some(fcid), Some(scid)) = (f.corporation_id, session.corporation_id) {
        if fcid == scid {
            if has_corp_bookmark_admin_role(snap, session, char_id, scid) {
                level = level.max(AccessLevel::Admin);
            } else {
                level = level.max(AccessLevel::Use);
            }
        }
    }
    if let Some(acl) = snap.bookmark_folder_acls.get(&f.folder_id) {
        if acl.contains(&char_id) {
            level = level.max(AccessLevel::Use);
        }
    }

    level
}

/// `accessGroupBookmarkMgr.AddFolder`
async fn handle_add_folder(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;

    let (is_personal, name, description, admin_gid, manage_gid, use_gid, view_gid, legacy_acls) =
        if args.len() >= 7 {
            let is_pers = args[0].as_bool().unwrap_or(false);
            let n = args[1].as_str().unwrap_or("New Folder");
            let desc = args[2].as_str().unwrap_or("");
            let a_gid = args[3].as_i64();
            let m_gid = args[4].as_i64();
            let u_gid = args[5].as_i64();
            let v_gid = args[6].as_i64();
            (is_pers, n, desc, a_gid, m_gid, u_gid, v_gid, Vec::new())
        } else {
            match args.first() {
                Some(PyRep::Bool(is_pers)) => {
                    let n = args.get(1).and_then(|a| a.as_str()).unwrap_or("Personal Locations");
                    let desc = args.get(2).and_then(|a| a.as_str()).unwrap_or("");
                    (*is_pers, n, desc, None, None, None, None, Vec::new())
                }
                _ => {
                    let n = args.first().and_then(|a| a.as_str()).unwrap_or("New Folder");
                    let is_shared = matches!(args.get(1), Some(PyRep::Bool(true)))
                        || args.get(1).and_then(|a| a.as_i64()).is_some_and(|v| v > 0);
                    let acls = extract_id_list(args.get(2));
                    (!is_shared, n, "", None, None, None, None, acls)
                }
            }
        };

    if !is_personal {
        let db_corp_id = ctx.db.with_snapshot(|snap| {
            snap.characters
                .get(&char_id)
                .map(|c| c.corporation_id)
                .filter(|cid| snap.corporations.contains_key(cid))
        });
        if let Some(cid) = db_corp_id {
            session.corporation_id = Some(cid);
        }
        let corp_id = session.corporation_id;
        if args.len() >= 7 {
            let Some(agid) = admin_gid else {
                return Err(BridgeRpcError {
                    code: "BookmarkAccessDenied".into(),
                    message: "Shared folder requires adminGroupID".into(),
                });
            };
            let has_admin_grp = ctx
                .db
                .with_snapshot(|snap| is_in_access_group(snap, session, char_id, agid));
            if !has_admin_grp {
                return Err(BridgeRpcError {
                    code: "BookmarkAccessDenied".into(),
                    message: "Caller lacks admin membership on adminGroupID".into(),
                });
            }
        } else {
            let has_rights = ctx.db.with_snapshot(|snap| {
                if let Some(cid) = corp_id {
                    has_corp_bookmark_admin_role(snap, session, char_id, cid)
                } else {
                    false
                }
            });
            if !has_rights {
                return Err(BridgeRpcError {
                    code: "BookmarkAccessDenied".into(),
                    message: "Only CEO or directors can create shared corp folders".into(),
                });
            }
        }

        let mut acl_ids = legacy_acls;
        for gid in [admin_gid, manage_gid, use_gid, view_gid].into_iter().flatten() {
            if !acl_ids.contains(&gid) {
                acl_ids.push(gid);
            }
        }

        let folder_id = ctx.db.try_with_mut_snapshot(|snap| {
            snap.next_entity_id += 1;
            let id = snap.next_entity_id;
            snap.bookmark_folders.insert(
                id,
                BookmarkFolderRecord {
                    folder_id: id,
                    owner_id: char_id,
                    folder_name: name.to_string(),
                    is_shared: true,
                    corporation_id: corp_id,
                    description: description.to_string(),
                    admin_group_id: admin_gid,
                    manage_group_id: manage_gid,
                    use_group_id: use_gid,
                    view_group_id: view_gid,
                },
            );
            if !acl_ids.is_empty() {
                snap.bookmark_folder_acls.insert(id, acl_ids.clone());
            }
            snap.bookmark_folder_known_by.insert((id, char_id), true);
            Ok(id)
        })?;

        if let Some(cid) = corp_id {
            ctx.notify(
                NotificationTarget::Corporation(cid),
                "OnSharedBookmarksFolderUpdated",
                "corpid",
                acl_folder_updates_arg(folder_id, FOLDER_UPDATED, PyRep::Integer(cid)),
            );
        }
        ctx.notify(
            NotificationTarget::Character(char_id),
            "OnSharedBookmarksFolderUpdated",
            "charid",
            acl_folder_updates_arg(folder_id, FOLDER_UPDATED, PyRep::Integer(folder_id)),
        );

        return Ok(PyRep::keyval([
            ("folderID", PyRep::Integer(folder_id)),
            ("folderName", PyRep::String(name.to_string())),
            ("description", PyRep::String(description.to_string())),
            ("ownerID", PyRep::Integer(char_id)),
            ("creatorID", PyRep::Integer(char_id)),
            ("isPersonal", PyRep::Bool(false)),
            ("isShared", PyRep::Bool(true)),
            ("isActive", PyRep::Bool(true)),
            ("accessLevel", PyRep::Integer(4)),
            ("adminGroupID", admin_gid.map(PyRep::Integer).unwrap_or(PyRep::None)),
            ("manageGroupID", manage_gid.map(PyRep::Integer).unwrap_or(PyRep::None)),
            ("useGroupID", use_gid.map(PyRep::Integer).unwrap_or(PyRep::None)),
            ("viewGroupID", view_gid.map(PyRep::Integer).unwrap_or(PyRep::None)),
            (
                "accessGroupIDs",
                PyRep::List(acl_ids.into_iter().map(PyRep::Integer).collect()),
            ),
            ("corporationID", corp_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
        ]));
    }

    let folder_id = ctx.db.try_with_mut_snapshot(|snap| {
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
        if personal_root_count >= 30 {
            return Err(BridgeRpcError {
                code: "MaxPersonalFoldersReached".into(),
                message: "Character has reached the maximum of 30 personal bookmark folders".into(),
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
                description: description.to_string(),
                admin_group_id: None,
                manage_group_id: None,
                use_group_id: None,
                view_group_id: None,
            },
        );
        snap.bookmark_folder_known_by.insert((id, char_id), true);
        Ok(id)
    })?;

    Ok(PyRep::keyval([
        ("folderID", PyRep::Integer(folder_id)),
        ("folderName", PyRep::String(name.to_string())),
        ("description", PyRep::String(description.to_string())),
        ("ownerID", PyRep::Integer(char_id)),
        ("creatorID", PyRep::Integer(char_id)),
        ("isPersonal", PyRep::Bool(true)),
        ("isShared", PyRep::Bool(false)),
        ("isActive", PyRep::Bool(true)),
        ("accessLevel", PyRep::Integer(4)),
        ("adminGroupID", PyRep::None),
        ("manageGroupID", PyRep::None),
        ("useGroupID", PyRep::None),
        ("viewGroupID", PyRep::None),
        ("accessGroupIDs", PyRep::List(vec![])),
        ("corporationID", PyRep::None),
    ]))
}

/// `accessGroupBookmarkMgr.UpdateFolder`
async fn handle_update_folder(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let folder_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
    let new_name = args.get(1).and_then(|a| a.as_str()).unwrap_or("Updated Folder");
    let desc_arg = args.get(2).and_then(|a| a.as_str());
    let admin_gid = args.get(3).and_then(|a| a.as_i64());
    let manage_gid = args.get(4).and_then(|a| a.as_i64());
    let use_gid = args.get(5).and_then(|a| a.as_i64());
    let view_gid = args.get(6).and_then(|a| a.as_i64());

    let (corp_id_opt, is_shared, eff_desc) = ctx.db.try_with_mut_snapshot(|snap| {
        if snap.bookmark_subfolder_parents.contains_key(&folder_id) {
            return Err(BridgeRpcError {
                code: "UserError".into(),
                message: "Cannot update subfolder via UpdateFolder; use UpdateSubfolder".into(),
            });
        }
        let Some(f_clone) = snap.bookmark_folders.get(&folder_id).cloned() else {
            return Err(BridgeRpcError {
                code: "UserError".into(),
                message: format!("Folder {folder_id} not found"),
            });
        };
        let lvl = folder_access_level(snap, session, char_id, &f_clone);
        if lvl < AccessLevel::Admin {
            return Err(BridgeRpcError {
                code: "UserError".into(),
                message: "Admin access-group rights required to update folder".into(),
            });
        }
        let f = snap.bookmark_folders.get_mut(&folder_id).unwrap();
        f.folder_name = new_name.to_string();
        if let Some(desc) = desc_arg {
            f.description = desc.to_string();
        }
        if args.len() >= 7 {
            f.admin_group_id = admin_gid;
            f.manage_group_id = manage_gid;
            f.use_group_id = use_gid;
            f.view_group_id = view_gid;
            let mut acl_ids = Vec::new();
            for gid in [admin_gid, manage_gid, use_gid, view_gid].into_iter().flatten() {
                if !acl_ids.contains(&gid) {
                    acl_ids.push(gid);
                }
            }
            if acl_ids.is_empty() {
                snap.bookmark_folder_acls.remove(&folder_id);
            } else {
                snap.bookmark_folder_acls.insert(folder_id, acl_ids);
            }
        } else if let Some(PyRep::List(acl_list)) = args.get(2) {
            let acl_ids: Vec<i64> = acl_list.iter().filter_map(|x| x.as_i64()).collect();
            if acl_ids.is_empty() {
                snap.bookmark_folder_acls.remove(&folder_id);
            } else {
                snap.bookmark_folder_acls.insert(folder_id, acl_ids);
            }
        }
        Ok((f.corporation_id, f.is_shared, f.description.clone()))
    })?;

    if is_shared {
        if let Some(cid) = corp_id_opt {
            ctx.notify(
                NotificationTarget::Corporation(cid),
                "OnSharedBookmarksFolderUpdated",
                "corpid",
                acl_folder_updates_arg(folder_id, FOLDER_UPDATED, PyRep::Integer(folder_id)),
            );
        }
        ctx.notify(
            NotificationTarget::Character(char_id),
            "OnSharedBookmarksFolderUpdated",
            "charid",
            acl_folder_updates_arg(folder_id, FOLDER_UPDATED, PyRep::Integer(folder_id)),
        );
    }

    let res_access = ctx.db.with_snapshot(|s| {
        s.bookmark_folders
            .get(&folder_id)
            .map(|f| folder_access_level(s, session, char_id, f) as i64)
            .unwrap_or(0)
    });

    if args.len() >= 7 {
        Ok(PyRep::Integer(res_access))
    } else {
        Ok(PyRep::keyval([
            ("folderID", PyRep::Integer(folder_id)),
            ("folderName", PyRep::String(new_name.to_string())),
            ("description", PyRep::String(eff_desc)),
            ("accessLevel", PyRep::Integer(res_access)),
        ]))
    }
}

/// `accessGroupBookmarkMgr.DeleteFolder`
async fn handle_delete_folder(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let Some(folder_id) = args.first().and_then(|a| a.as_i64()) else {
        return Err(BridgeRpcError {
            code: "InvalidArgument".into(),
            message: "DeleteFolder requires folderID".into(),
        });
    };
    let corp_id_opt = ctx.db.try_with_mut_snapshot(|snap| {
        if snap.bookmark_subfolder_parents.contains_key(&folder_id) {
            return Err(BridgeRpcError {
                code: "UserError".into(),
                message: "Cannot delete subfolder via DeleteFolder; use DeleteSubfolder".into(),
            });
        }
        let Some(f) = snap.bookmark_folders.get(&folder_id).cloned() else {
            return Err(BridgeRpcError {
                code: "UserError".into(),
                message: format!("Folder {folder_id} not found"),
            });
        };
        let lvl = folder_access_level(snap, session, char_id, &f);
        if lvl < AccessLevel::Admin {
            return Err(BridgeRpcError {
                code: "UserError".into(),
                message: "Admin rights required to delete folder".into(),
            });
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
        let bms_to_delete: Vec<i64> = snap
            .bookmarks
            .iter()
            .filter_map(|(&bid, bm)| {
                let in_root = bm.folder_id == Some(folder_id);
                let in_child = bm.folder_id.map_or(false, |fid| child_sfs.contains(&fid))
                    || snap.bookmark_subfolders.get(&bid).map_or(false, |sfid| child_sfs.contains(sfid));
                (in_root || in_child).then_some(bid)
            })
            .collect();
        for bid in &bms_to_delete {
            snap.bookmarks.remove(bid);
            snap.bookmark_subfolders.remove(bid);
            snap.bookmark_expiries.remove(bid);
        }
        snap.bookmark_subfolders.retain(|_, sfid| !child_sfs.contains(sfid));
        snap.bookmark_folders.remove(&folder_id);
        snap.bookmark_folder_acls.remove(&folder_id);
        snap.bookmark_folder_known_by.retain(|&(fid, _), _| fid != folder_id);
        Ok(f.corporation_id)
    })?;

    if let Some(cid) = corp_id_opt {
        ctx.notify(
            NotificationTarget::Corporation(cid),
            "OnSharedBookmarksFolderDeleted",
            "corpid",
            vec![PyRep::Integer(folder_id)],
        );
    }
    Ok(PyRep::keyval([
        ("folderID", PyRep::Integer(folder_id)),
        ("deleted", PyRep::Bool(true)),
    ]))
}

/// `accessGroupBookmarkMgr.CreateSubfolder`
async fn handle_create_subfolder(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let Some(parent_folder_id) = args.first().and_then(|a| a.as_i64()) else {
        return Err(BridgeRpcError {
            code: "InvalidArgument".into(),
            message: "CreateSubfolder requires parent folderID".into(),
        });
    };
    let sub_name = args.get(1).and_then(|a| a.as_str()).unwrap_or("Subfolder");

    let (id, full_name, is_shared, corp_id_opt) = ctx.db.try_with_mut_snapshot(|snap| {
        if snap.bookmark_subfolder_parents.contains_key(&parent_folder_id) {
            return Err(BridgeRpcError {
                code: "UserError".into(),
                message: "Cannot create a subfolder within a subfolder".into(),
            });
        }
        let Some(parent) = snap.bookmark_folders.get(&parent_folder_id).cloned() else {
            return Err(BridgeRpcError {
                code: "UserError".into(),
                message: format!("Parent folder {parent_folder_id} not found"),
            });
        };
        let lvl = folder_access_level(snap, session, char_id, &parent);
        if lvl < AccessLevel::Manage {
            return Err(BridgeRpcError {
                code: "UserError".into(),
                message: "Manage rights required on parent folder to create subfolder".into(),
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
        let full = format!("{}/{sub_name}", parent.folder_name);
        snap.next_entity_id += 1;
        let id = snap.next_entity_id;
        snap.bookmark_folders.insert(
            id,
            BookmarkFolderRecord {
                folder_id: id,
                owner_id: char_id,
                folder_name: full.clone(),
                is_shared: parent.is_shared,
                corporation_id: parent.corporation_id,
                ..Default::default()
            },
        );
        snap.bookmark_subfolder_parents.insert(id, parent_folder_id);
        Ok((id, full, parent.is_shared, parent.corporation_id))
    })?;

    if is_shared {
        if let Some(cid) = corp_id_opt {
            ctx.notify(
                NotificationTarget::Corporation(cid),
                "OnSharedBookmarksFolderUpdated",
                "corpid",
                acl_folder_updates_arg(parent_folder_id, SUBFOLDER_ADDED, PyRep::Integer(id)),
            );
        }
    }

    Ok(PyRep::keyval([
        ("subfolderID", PyRep::Integer(id)),
        ("folderID", PyRep::Integer(id)),
        ("parentFolderID", PyRep::Integer(parent_folder_id)),
        ("subfolderName", PyRep::String(sub_name.to_string())),
        ("folderName", PyRep::String(full_name)),
    ]))
}

/// `accessGroupBookmarkMgr.UpdateSubfolder`
async fn handle_update_subfolder(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let (parent_id_opt, subfolder_id, sub_name) = if args.len() >= 3 {
        (
            args[0].as_i64(),
            args[1].as_i64().unwrap_or(0),
            args[2].as_str().unwrap_or("Subfolder"),
        )
    } else {
        let first = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
        let n = args.get(1).and_then(|a| a.as_str()).unwrap_or("Subfolder");
        (None, first, n)
    };

    let (sfid, pfid, full_name, is_shared, corp_id_opt) = ctx.db.try_with_mut_snapshot(|snap| {
        let mut actual_sfid = subfolder_id;
        if parent_id_opt.is_none() && !snap.bookmark_subfolder_parents.contains_key(&actual_sfid) {
            let mut children: Vec<i64> = snap
                .bookmark_subfolder_parents
                .iter()
                .filter_map(|(&child_sf, &p)| if p == actual_sfid { Some(child_sf) } else { None })
                .collect();
            children.sort_unstable();
            if let Some(&child_sf) = children.first() {
                actual_sfid = child_sf;
            }
        }
        let pfid = snap.bookmark_subfolder_parents.get(&actual_sfid).copied().or(parent_id_opt).unwrap_or(0);
        if let Some(req_pfid) = parent_id_opt {
            if snap.bookmark_subfolder_parents.get(&actual_sfid) != Some(&req_pfid) {
                return Err(BridgeRpcError {
                    code: "UserError".into(),
                    message: "Subfolder does not belong to specified folder".into(),
                });
            }
        } else if !snap.bookmark_subfolder_parents.contains_key(&actual_sfid) {
            return Err(BridgeRpcError {
                code: "UserError".into(),
                message: format!("Subfolder {actual_sfid} not found"),
            });
        }
        let Some(parent) = snap.bookmark_folders.get(&pfid).cloned() else {
            return Err(BridgeRpcError {
                code: "UserError".into(),
                message: format!("Parent folder {pfid} not found"),
            });
        };
        let lvl = folder_access_level(snap, session, char_id, &parent);
        if lvl < AccessLevel::Manage {
            return Err(BridgeRpcError {
                code: "UserError".into(),
                message: "Manage rights required on parent folder to update subfolder".into(),
            });
        }
        let full = format!("{}/{sub_name}", parent.folder_name);
        if let Some(sf) = snap.bookmark_folders.get_mut(&actual_sfid) {
            sf.folder_name = full.clone();
        } else {
            return Err(BridgeRpcError {
                code: "UserError".into(),
                message: format!("Subfolder {actual_sfid} record not found"),
            });
        }
        Ok((actual_sfid, pfid, full, parent.is_shared, parent.corporation_id))
    })?;

    if is_shared {
        if let Some(cid) = corp_id_opt {
            ctx.notify(
                NotificationTarget::Corporation(cid),
                "OnSharedBookmarksFolderUpdated",
                "corpid",
                acl_folder_updates_arg(pfid, SUBFOLDER_UPDATED, PyRep::Integer(sfid)),
            );
        }
    }

    Ok(PyRep::keyval([
        ("subfolderID", PyRep::Integer(sfid)),
        ("folderID", PyRep::Integer(sfid)),
        ("parentFolderID", PyRep::Integer(pfid)),
        ("subfolderName", PyRep::String(sub_name.to_string())),
        ("folderName", PyRep::String(full_name)),
    ]))
}

/// `accessGroupBookmarkMgr.DeleteSubfolder`
async fn handle_delete_subfolder(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let (parent_id_opt, subfolder_id) = if args.len() >= 2 {
        (args[0].as_i64(), args[1].as_i64().unwrap_or(0))
    } else {
        let sid = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
        (None, sid)
    };

    let (sfid, pfid, is_shared, corp_id_opt) = ctx.db.try_with_mut_snapshot(|snap| {
        let pfid = snap.bookmark_subfolder_parents.get(&subfolder_id).copied().or(parent_id_opt).unwrap_or(0);
        if let Some(req_pfid) = parent_id_opt {
            if snap.bookmark_subfolder_parents.get(&subfolder_id) != Some(&req_pfid) {
                return Err(BridgeRpcError {
                    code: "UserError".into(),
                    message: "Subfolder does not belong to specified folder".into(),
                });
            }
        } else if !snap.bookmark_subfolder_parents.contains_key(&subfolder_id) {
            return Err(BridgeRpcError {
                code: "UserError".into(),
                message: format!("Subfolder {subfolder_id} not found"),
            });
        }
        let Some(parent) = snap.bookmark_folders.get(&pfid).cloned() else {
            return Err(BridgeRpcError {
                code: "UserError".into(),
                message: format!("Parent folder {pfid} not found"),
            });
        };
        let lvl = folder_access_level(snap, session, char_id, &parent);
        if lvl < AccessLevel::Manage {
            return Err(BridgeRpcError {
                code: "UserError".into(),
                message: "Manage rights required on parent folder to delete subfolder".into(),
            });
        }
        snap.bookmark_folders.remove(&subfolder_id);
        snap.bookmark_subfolder_parents.remove(&subfolder_id);
        let bms_to_delete: Vec<i64> = snap
            .bookmarks
            .iter()
            .filter_map(|(&bid, bm)| {
                let in_sub = bm.folder_id == Some(subfolder_id)
                    || snap.bookmark_subfolders.get(&bid) == Some(&subfolder_id);
                in_sub.then_some(bid)
            })
            .collect();
        for bid in &bms_to_delete {
            snap.bookmarks.remove(bid);
            snap.bookmark_subfolders.remove(bid);
            snap.bookmark_expiries.remove(bid);
        }
        snap.bookmark_subfolders.retain(|_, sfid| *sfid != subfolder_id);
        Ok((subfolder_id, pfid, parent.is_shared, parent.corporation_id))
    })?;

    if is_shared {
        if let Some(cid) = corp_id_opt {
            ctx.notify(
                NotificationTarget::Corporation(cid),
                "OnSharedBookmarksFolderUpdated",
                "corpid",
                acl_folder_updates_arg(pfid, SUBFOLDER_REMOVED, PyRep::Integer(sfid)),
            );
        }
    }

    Ok(PyRep::List(vec![PyRep::Integer(sfid)]))
}

/// `accessGroupBookmarkMgr.AddToKnownFolders` / `UpdateKnownFolderState`
async fn handle_known_folders_state(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
    is_add: bool,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let Some(folder_id) = args.first().and_then(|a| a.as_i64()) else {
        return Err(BridgeRpcError {
            code: "InvalidArgument".into(),
            message: "Known folders operation requires folderID".into(),
        });
    };
    let active = if is_add {
        args.get(1).map(|a| matches!(a, PyRep::Bool(true) | PyRep::Integer(1))).unwrap_or(true)
    } else {
        args.get(1).map(|a| matches!(a, PyRep::Bool(true) | PyRep::Integer(1))).unwrap_or(false)
    };

    let (folder_rec, access_lvl) = ctx.db.with_snapshot(|snap| {
        if snap.bookmark_subfolder_parents.contains_key(&folder_id) {
            return Err(BridgeRpcError {
                code: "UserError".into(),
                message: "Cannot add subfolder to known folders".into(),
            });
        }
        let Some(f) = snap.bookmark_folders.get(&folder_id) else {
            return Err(BridgeRpcError {
                code: "UserError".into(),
                message: format!("Folder {folder_id} not found"),
            });
        };
        if !is_add && !snap.bookmark_folder_known_by.contains_key(&(folder_id, char_id)) {
            return Err(BridgeRpcError {
                code: "UserError".into(),
                message: "Folder is not in known folders".into(),
            });
        }
        let lvl = folder_access_level(snap, session, char_id, f);
        if lvl < AccessLevel::View {
            return Err(BridgeRpcError {
                code: "UserError".into(),
                message: "Character lacks view access-group rights to subscribe to this folder".into(),
            });
        }
        Ok((f.clone(), lvl as i64))
    })?;

    let now_unix = ctx.now_unix();
    ctx.db.try_with_mut_snapshot(|snap| {
        sweep_expired_bookmarks(snap, now_unix);
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
        snap.bookmark_folder_known_by.insert((folder_id, char_id), active);
        Ok(())
    })?;

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
        acl_folder_updates_arg(folder_id, FOLDER_UPDATED, PyRep::Bool(active)),
    );
    if let Some(corp_id) = folder_rec.corporation_id {
        ctx.notify(
            NotificationTarget::Corporation(corp_id),
            "OnSharedBookmarksFolderUpdated",
            "corpid",
            acl_folder_updates_arg(folder_id, FOLDER_UPDATED, PyRep::Integer(corp_id)),
        );
    }

    let (bms, subfolders) = ctx.db.with_snapshot(|snap| {
        let bms: Vec<PyRep> = if !active {
            Vec::new()
        } else {
            snap.bookmarks
                .values()
                .filter(|b| {
                    b.folder_id == Some(folder_id)
                        && !is_bookmark_expired_in_snap(snap, b.bookmark_id, now_unix)
                })
                .map(|b| {
                    let sfid = snap.bookmark_subfolders.get(&b.bookmark_id).copied();
                    PyRep::keyval([
                        ("bookmarkID", PyRep::Integer(b.bookmark_id)),
                        ("ownerID", PyRep::Integer(b.owner_id)),
                        ("itemID", PyRep::Integer(b.item_id.unwrap_or(b.solar_system_id as i64))),
                        ("locationID", PyRep::Integer(b.solar_system_id as i64)),
                        ("folderID", PyRep::Integer(folder_id)),
                        ("subfolderID", sfid.map(PyRep::Integer).unwrap_or(PyRep::None)),
                        ("label", PyRep::String(b.label.clone())),
                        ("memo", PyRep::String(b.memo.clone())),
                        ("x", PyRep::Real(b.x)),
                        ("y", PyRep::Real(b.y)),
                        ("z", PyRep::Real(b.z)),
                    ])
                })
                .collect()
        };

        let subs: Vec<PyRep> = if !active {
            Vec::new()
        } else {
            snap.bookmark_subfolder_parents
                .iter()
                .filter_map(|(&sfid, &pfid)| {
                    if pfid == folder_id {
                        let sub_name = snap
                            .bookmark_folders
                            .get(&sfid)
                            .map(|f| {
                                let prefix = format!("{}/", folder_rec.folder_name);
                                f.folder_name
                                    .strip_prefix(&prefix)
                                    .or_else(|| f.folder_name.split_once('/').map(|(_, rest)| rest))
                                    .unwrap_or(&f.folder_name)
                                    .to_string()
                            })
                            .unwrap_or_else(|| format!("Subfolder {sfid}"));
                        Some(PyRep::keyval([
                            ("subfolderID", PyRep::Integer(sfid)),
                            ("folderID", PyRep::Integer(folder_id)),
                            ("subfolderName", PyRep::String(sub_name)),
                        ]))
                    } else {
                        None
                    }
                })
                .collect()
        };

        (bms, subs)
    });

    let acl = ctx.db.with_snapshot(|snap| {
        snap.bookmark_folder_acls.get(&folder_id).cloned().unwrap_or_default()
    });

    let folder_obj = PyRep::keyval([
        ("folderID", PyRep::Integer(folder_id)),
        ("folderName", PyRep::String(folder_rec.folder_name)),
        ("description", PyRep::String(folder_rec.description)),
        ("ownerID", PyRep::Integer(folder_rec.owner_id)),
        ("creatorID", PyRep::Integer(folder_rec.owner_id)),
        ("isPersonal", PyRep::Bool(!folder_rec.is_shared)),
        ("isShared", PyRep::Bool(folder_rec.is_shared)),
        ("isActive", PyRep::Bool(active)),
        ("accessLevel", PyRep::Integer(access_lvl)),
        ("adminGroupID", folder_rec.admin_group_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
        ("manageGroupID", folder_rec.manage_group_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
        ("useGroupID", folder_rec.use_group_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
        ("viewGroupID", folder_rec.view_group_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
        (
            "accessGroupIDs",
            PyRep::List(acl.into_iter().map(PyRep::Integer).collect()),
        ),
        ("corporationID", folder_rec.corporation_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
    ]);

    Ok(PyRep::Tuple(vec![
        folder_obj,
        PyRep::List(bms),
        PyRep::List(subfolders),
    ]))
}

/// `accessGroupBookmarkMgr.RemoveFromKnownFolders`
async fn handle_remove_from_known_folders(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let Some(folder_id) = args.first().and_then(|a| a.as_i64()) else {
        return Err(BridgeRpcError {
            code: "InvalidArgument".into(),
            message: "RemoveFromKnownFolders requires folderID".into(),
        });
    };
    let (exists, is_sub) = ctx.db.with_snapshot(|snap| {
        (
            snap.bookmark_folders.contains_key(&folder_id),
            snap.bookmark_subfolder_parents.contains_key(&folder_id),
        )
    });
    if !exists || is_sub {
        return Err(BridgeRpcError {
            code: "UserError".into(),
            message: format!("Folder {folder_id} not found"),
        });
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
        acl_folder_updates_arg(folder_id, FOLDER_UPDATED, PyRep::Bool(false)),
    );
    Ok(PyRep::keyval([
        ("folderID", PyRep::Integer(folder_id)),
        ("subscribed", PyRep::Bool(false)),
    ]))
}

/// `accessGroupBookmarkMgr.GetFolderInfo`
async fn handle_get_folder_info(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let Some(target_fid) = args.first().and_then(|a| a.as_i64()) else {
        return Err(BridgeRpcError {
            code: "UserError".into(),
            message: "GetFolderInfo requires folderID".into(),
        });
    };

    let (f, lvl) = ctx.db.with_snapshot(|snap| {
        let Some(f) = snap.bookmark_folders.get(&target_fid) else {
            return Err(BridgeRpcError {
                code: "UserError".into(),
                message: format!("Folder {target_fid} not found"),
            });
        };
        let lvl = folder_access_level(snap, session, char_id, f);
        if lvl < AccessLevel::View {
            return Err(BridgeRpcError {
                code: "UserError".into(),
                message: "Folder not visible".into(),
            });
        }
        Ok((f.clone(), lvl as i64))
    })?;

    let acl = ctx.db.with_snapshot(|snap| {
        snap.bookmark_folder_acls.get(&target_fid).cloned().unwrap_or_default()
    });

    Ok(PyRep::keyval([
        ("folderID", PyRep::Integer(f.folder_id)),
        ("folderName", PyRep::String(f.folder_name)),
        ("description", PyRep::String(f.description)),
        ("creatorID", PyRep::Integer(f.owner_id)),
        ("ownerID", PyRep::Integer(f.owner_id)),
        ("isPersonal", PyRep::Bool(!f.is_shared)),
        ("isShared", PyRep::Bool(f.is_shared)),
        ("adminGroupID", f.admin_group_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
        ("manageGroupID", f.manage_group_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
        ("useGroupID", f.use_group_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
        ("viewGroupID", f.view_group_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
        ("accessLevel", PyRep::Integer(lvl)),
        (
            "accessGroupIDs",
            PyRep::List(acl.into_iter().map(PyRep::Integer).collect()),
        ),
        ("corporationID", f.corporation_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
    ]))
}

/// `accessGroupBookmarkMgr.SearchFoldersWithAdminAccess`
async fn handle_search_folders_with_admin_access(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let search_text = args.first().and_then(|a| a.as_str()).unwrap_or("");

    let rows: Vec<Vec<PyRep>> = ctx.db.with_snapshot(|snap| {
        snap.bookmark_folders
            .values()
            .filter(|f| {
                f.is_shared
                    && !snap.bookmark_subfolder_parents.contains_key(&f.folder_id)
                    && (search_text.is_empty()
                        || f.folder_name.to_lowercase().contains(&search_text.to_lowercase()))
                    && folder_access_level(snap, session, char_id, f) >= AccessLevel::Admin
            })
            .map(|f| {
                let acl = snap.bookmark_folder_acls.get(&f.folder_id).cloned().unwrap_or_default();
                vec![
                    PyRep::Integer(f.folder_id),
                    PyRep::String(f.folder_name.clone()),
                    PyRep::Integer(f.owner_id),
                    PyRep::Bool(f.is_shared),
                    f.corporation_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                    PyRep::List(acl.into_iter().map(PyRep::Integer).collect()),
                    PyRep::Integer(4),
                ]
            })
            .collect()
    });

    Ok(PyRep::rowset(
        vec![
            "folderID", "folderName", "ownerID", "isShared", "corporationID",
            "accessGroupIDs", "accessLevel",
        ],
        rows,
    ))
}
