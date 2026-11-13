//! Batch bookmark copy & move operations (`bookmark.CopyBookmarksToFolderAndSubfolder` / `MoveBookmarksToFolderAndSubfolder`)
//! with per-folder Access Group ACL enforcement.

use super::{
    effective_char_corp_and_alliance, extract_id_list, folder_access_level,
    has_corp_bookmark_admin_role, shared_bookmark_corp_id, AccessLevel,
};
use crate::router::ServerContext;
use eve_db::BookmarkRecord;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};

pub(super) fn copy_bookmarks_to_folder_and_subfolder(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    transfer_bookmarks_to_folder_and_subfolder(ctx, session, args, false)
}

pub(super) fn move_bookmarks_to_folder_and_subfolder(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    transfer_bookmarks_to_folder_and_subfolder(ctx, session, args, true)
}

fn transfer_bookmarks_to_folder_and_subfolder(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
    is_move: bool,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (_, char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let bm_ids = extract_id_list(args.first());
    if bm_ids.len() > 20 {
        return Some(Err(BridgeRpcError {
            code: "BookmarkCopyBatchLimitExceeded".into(),
            message: "Cannot copy or move more than 20 bookmarks in a single operation".into(),
            client_error: None,
        }));
    }
    let target_folder_id = args.get(1).and_then(|a| a.as_i64());
    let target_subfolder_id = args.get(2).and_then(|a| a.as_i64());

    let res = ctx.db.try_with_mut_snapshot(|snap| {
        let (caller_corp_id, _) = effective_char_corp_and_alliance(snap, session, char_id);
        let is_shared_target = if let Some(fid) = target_folder_id {
            if (fid == 0 || fid == char_id) && !snap.bookmark_folders.contains_key(&fid) {
                false
            } else {
                let Some(tf) = snap.bookmark_folders.get(&fid) else {
                    return Err(BridgeRpcError {
                        code: "FolderNotFound".into(),
                        message: format!("Bookmark folder {fid} not found"),
                        client_error: None,
                    });
                };
                if folder_access_level(snap, session, char_id, tf) < AccessLevel::Use {
                    return Err(BridgeRpcError {
                        code: "BookmarkFolderAccessDenied".into(),
                        message: "No write access to target bookmark folder".into(),
                        client_error: None,
                    });
                }
                tf.is_shared
            }
        } else {
            false
        };
        if let Some(sfid) = target_subfolder_id {
            if snap.bookmark_folders.contains_key(&sfid)
                || snap.bookmark_subfolder_parents.contains_key(&sfid)
            {
                if snap.bookmark_subfolder_parents.get(&sfid).copied() != target_folder_id {
                    return Err(BridgeRpcError {
                        code: "BookmarkFolderAccessDenied".into(),
                        message: "Target subfolder does not belong to target bookmark folder".into(),
                        client_error: None,
                    });
                }
            }
        }
        let current_in_folder = snap
            .bookmarks
            .values()
            .filter(|b| {
                b.folder_id == target_folder_id && (is_shared_target || b.owner_id == char_id)
            })
            .count();
        let added_count = if is_move {
            bm_ids
                .iter()
                .filter(|&&id| {
                    snap.bookmarks
                        .get(&id)
                        .is_some_and(|b| b.folder_id != target_folder_id)
                })
                .count()
        } else {
            bm_ids.len()
        };
        if current_in_folder + added_count > 500 {
            let code = if is_shared_target {
                "MaxSharedFolderBookmarksReached"
            } else {
                "MaxBookmarksReached"
            };
            return Err(BridgeRpcError {
                code: code.into(),
                message: "Bookmark folder capacity (500) exceeded".into(),
                client_error: None,
            });
        }
        let mut out_ids = Vec::new();
        for &bm_id in &bm_ids {
            let Some(orig) = snap.bookmarks.get(&bm_id).cloned() else {
                return Err(BridgeRpcError {
                    code: "BookmarkNotFound".into(),
                    message: format!("Bookmark {bm_id} not found"),
                    client_error: None,
                });
            };
            let can_access = if let Some(sf) =
                orig.folder_id.and_then(|fid| snap.bookmark_folders.get(&fid))
            {
                let src_lvl = folder_access_level(snap, session, char_id, sf);
                if is_move {
                    if !orig.is_shared && orig.owner_id == char_id {
                        true
                    } else if orig.owner_id == char_id {
                        src_lvl >= AccessLevel::Use
                    } else {
                        orig.is_shared && src_lvl >= AccessLevel::Manage
                    }
                } else if !orig.is_shared && orig.owner_id == char_id {
                    true
                } else {
                    src_lvl >= AccessLevel::View
                }
            } else {
                let orig_corp_id = shared_bookmark_corp_id(snap, &orig);
                let same_corp_shared =
                    orig.is_shared && caller_corp_id.is_some() && orig_corp_id == caller_corp_id;
                if is_move {
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
                }
            };
            if !can_access {
                return Err(BridgeRpcError {
                    code: "BookmarkAccessDenied".into(),
                    message: format!("No access to bookmark {bm_id}"),
                    client_error: None,
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
        Ok(out_ids) => Some(Ok(PyRep::keyval([
            (
                "bookmarkIDs",
                PyRep::List(out_ids.into_iter().map(PyRep::Integer).collect()),
            ),
            (
                "folderID",
                target_folder_id.map(PyRep::Integer).unwrap_or(PyRep::None),
            ),
        ]))),
        Err(e) => Some(Err(e)),
    }
}

#[cfg(test)]
#[path = "fleet_social_bookmarks_transfer_edges_tests.rs"]
mod fleet_social_bookmarks_transfer_edges_tests;
