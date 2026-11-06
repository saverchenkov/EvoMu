//! Extra branch-coverage and mutation-killing unit tests for `fleet_social_access_group_bookmarks.rs`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::fleet_social::fleet_social_access_group_bookmarks::{
    acl_folder_updates_arg, folder_access_level, handle, AccessLevel, FOLDER_UPDATED,
};
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::test_ctx_and_clock;
use eve_db::{BookmarkFolderRecord, BookmarkRecord};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use std::collections::HashMap;

fn make_session(char_id: i64, corp_id: i64, role: u64) -> SessionState {
    SessionState {
        user_id: Some(char_id as u64),
        character_id: Some(char_id),
        corporation_id: Some(corp_id),
        alliance_id: Some(99_000_001),
        solar_system_id: Some(30_000_142),
        corp_role: role,
        ..Default::default()
    }
}

async fn call_agbm(
    ctx: &ServerContext,
    sess: &mut SessionState,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    let kw = HashMap::new();
    handle(ctx, sess, "accessGroupBookmarkMgr", method, args, &kw)
        .await
        .expect("accessGroupBookmarkMgr method handled")
}

#[test]
fn folder_access_level_subfolders_groups_and_legacy_fallbacks() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let owner_id = 100_000_001_i64;
    let peer_id = 100_000_002_i64;
    let outsider_id = 100_000_003_i64;
    let corp_id = 1_000_009_i64;

    let peer_grunt = make_session(peer_id, corp_id, 0);
    let peer_dir = make_session(peer_id, corp_id, u64::MAX);
    let outsider_sess = make_session(outsider_id, 1_000_010, 0);

    ctx.db.with_mut_snapshot(|snap| {
        snap.access_groups.insert(601, vec![peer_id]); // admin
        snap.access_groups.insert(602, vec![peer_id]); // use
        snap.access_groups.insert(603, vec![peer_id]); // view
        snap.bookmark_folder_acls.insert(20, vec![outsider_id]);

        // Parent folder 10 (shared, use_group_id = 602) and child subfolder 11, orphan subfolder 12
        snap.bookmark_folders.insert(
            10,
            BookmarkFolderRecord {
                folder_id: 10,
                owner_id,
                folder_name: "Parent".into(),
                is_shared: true,
                use_group_id: Some(602),
                ..Default::default()
            },
        );
        snap.bookmark_subfolder_parents.insert(11, 10);
        snap.bookmark_subfolder_parents.insert(12, 9999);
    });

    let upd_arg = acl_folder_updates_arg(10, FOLDER_UPDATED, PyRep::Integer(1));
    assert_eq!(upd_arg.len(), 1);

    ctx.db.with_snapshot(|snap| {
        // Subfolder with existing parent inherits parent's access level; orphan subfolder gets None
        let sf_rec = BookmarkFolderRecord {
            folder_id: 11,
            owner_id: 0,
            is_shared: true,
            ..Default::default()
        };
        assert_eq!(
            folder_access_level(snap, &peer_grunt, peer_id, &sf_rec),
            AccessLevel::Use
        );
        let orphan_sf = BookmarkFolderRecord {
            folder_id: 12,
            owner_id,
            is_shared: true,
            ..Default::default()
        };
        assert_eq!(
            folder_access_level(snap, &peer_grunt, owner_id, &orphan_sf),
            AccessLevel::None
        );

        // Shared folder with admin_group_id vs view_group_id vs non-member corp director (no corp override when group configured)
        let admin_grp_f = BookmarkFolderRecord {
            folder_id: 13,
            owner_id,
            is_shared: true,
            admin_group_id: Some(601),
            ..Default::default()
        };
        assert_eq!(
            folder_access_level(snap, &peer_grunt, peer_id, &admin_grp_f),
            AccessLevel::Admin
        );

        let view_grp_f = BookmarkFolderRecord {
            folder_id: 14,
            owner_id,
            is_shared: true,
            corporation_id: Some(corp_id),
            view_group_id: Some(603),
            ..Default::default()
        };
        assert_eq!(
            folder_access_level(snap, &peer_grunt, peer_id, &view_grp_f),
            AccessLevel::View
        );
        assert_eq!(
            folder_access_level(snap, &outsider_sess, outsider_id, &view_grp_f),
            AccessLevel::None
        );

        // Legacy shared folder (no group IDs): same-corp director -> Admin, same-corp grunt -> Use, ACL member -> Use
        let legacy_f = BookmarkFolderRecord {
            folder_id: 20,
            owner_id,
            is_shared: true,
            corporation_id: Some(corp_id),
            ..Default::default()
        };
        assert_eq!(
            folder_access_level(snap, &peer_dir, peer_id, &legacy_f),
            AccessLevel::Admin
        );
        assert_eq!(
            folder_access_level(snap, &peer_grunt, peer_id, &legacy_f),
            AccessLevel::Use
        );
        assert_eq!(
            folder_access_level(snap, &outsider_sess, outsider_id, &legacy_f),
            AccessLevel::Use
        );
    });
}

#[tokio::test]
async fn add_update_delete_folder_limits_permissions_and_cascading_cleanup() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let owner_id = 100_000_010_i64;
    let viewer_id = 100_000_011_i64;
    let corp_id = 1_000_009_i64;
    let mut owner_sess = make_session(owner_id, corp_id, u64::MAX);
    let mut viewer_sess = make_session(viewer_id, 1_000_010, 0);

    ctx.db.with_mut_snapshot(|snap| {
        snap.access_groups.insert(701, vec![owner_id]);
        snap.access_groups.insert(702, vec![viewer_id]);
    });

    // 7-arg shared AddFolder without adminGroupID -> BookmarkAccessDenied
    assert_eq!(
        call_agbm(
            &ctx,
            &mut owner_sess,
            "AddFolder",
            &[
                PyRep::Bool(false),
                PyRep::String("SharedNoAdmin".into()),
                PyRep::String("".into()),
                PyRep::None,
                PyRep::None,
                PyRep::None,
                PyRep::None,
            ],
        )
        .await
        .unwrap_err()
        .code,
        "BookmarkAccessDenied"
    );

    // 7-arg shared AddFolder where caller is not in adminGroupID -> BookmarkAccessDenied
    assert_eq!(
        call_agbm(
            &ctx,
            &mut viewer_sess,
            "AddFolder",
            &[
                PyRep::Bool(false),
                PyRep::String("SharedNotMember".into()),
                PyRep::String("".into()),
                PyRep::Integer(701),
                PyRep::None,
                PyRep::None,
                PyRep::None,
            ],
        )
        .await
        .unwrap_err()
        .code,
        "BookmarkAccessDenied"
    );

    // Legacy 3-arg shared AddFolder by non-director -> BookmarkAccessDenied
    assert_eq!(
        call_agbm(
            &ctx,
            &mut viewer_sess,
            "AddFolder",
            &[
                PyRep::String("LegacyShared".into()),
                PyRep::Bool(true),
                PyRep::List(vec![PyRep::Integer(viewer_id)]),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "BookmarkAccessDenied"
    );

    // Valid 7-arg shared AddFolder by admin_group_id member
    let shared = call_agbm(
        &ctx,
        &mut owner_sess,
        "AddFolder",
        &[
            PyRep::Bool(false),
            PyRep::String("SharedOps".into()),
            PyRep::String("Ops".into()),
            PyRep::Integer(701),
            PyRep::Integer(701),
            PyRep::None,
            PyRep::Integer(702),
        ],
    )
    .await
    .unwrap();
    let shared_fid = shared.dict_get("folderID").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(shared.dict_get("isShared"), Some(&PyRep::Bool(true)));

    // 7-arg UpdateFolder returns Integer access level (4)
    let upd7 = call_agbm(
        &ctx,
        &mut owner_sess,
        "UpdateFolder",
        &[
            PyRep::Integer(shared_fid),
            PyRep::String("SharedOpsRenamed".into()),
            PyRep::String("NewDesc".into()),
            PyRep::Integer(701),
            PyRep::None,
            PyRep::None,
            PyRep::Integer(702),
        ],
    )
    .await
    .unwrap();
    assert_eq!(upd7, PyRep::Integer(4));

    // Viewer cannot UpdateFolder or DeleteFolder
    assert_eq!(
        call_agbm(
            &ctx,
            &mut viewer_sess,
            "UpdateFolder",
            &[PyRep::Integer(shared_fid), PyRep::String("Hack".into())],
        )
        .await
        .unwrap_err()
        .code,
        "UserError"
    );
    assert_eq!(
        call_agbm(
            &ctx,
            &mut viewer_sess,
            "DeleteFolder",
            &[PyRep::Integer(shared_fid)],
        )
        .await
        .unwrap_err()
        .code,
        "UserError"
    );

    // Personal folder limit: max 5 personal folders
    for i in 0..5 {
        call_agbm(
            &ctx,
            &mut viewer_sess,
            "AddFolder",
            &[PyRep::Bool(true), PyRep::String(format!("Pers_{i}"))],
        )
        .await
        .expect("create personal folder");
    }
    assert_eq!(
        call_agbm(
            &ctx,
            &mut viewer_sess,
            "AddFolder",
            &[PyRep::Bool(true), PyRep::String("Pers_Overflow".into())],
        )
        .await
        .unwrap_err()
        .code,
        "MaxPersonalFoldersReached"
    );

    // Create subfolder and bookmark inside shared_fid, then DeleteFolder cascades
    let sf = call_agbm(
        &ctx,
        &mut owner_sess,
        "CreateSubfolder",
        &[PyRep::Integer(shared_fid), PyRep::String("SubA".into())],
    )
    .await
    .unwrap();
    let sfid = sf.dict_get("subfolderID").and_then(|v| v.as_i64()).unwrap();

    // UpdateFolder & DeleteFolder reject subfolder IDs
    assert_eq!(
        call_agbm(
            &ctx,
            &mut owner_sess,
            "UpdateFolder",
            &[PyRep::Integer(sfid), PyRep::String("Bad".into())],
        )
        .await
        .unwrap_err()
        .code,
        "UserError"
    );
    assert_eq!(
        call_agbm(&ctx, &mut owner_sess, "DeleteFolder", &[PyRep::Integer(sfid)])
            .await
            .unwrap_err()
            .code,
        "UserError"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.bookmarks.insert(
            901,
            BookmarkRecord {
                bookmark_id: 901,
                owner_id,
                solar_system_id: 30_000_142,
                folder_id: Some(sfid),
                label: "InSub".into(),
                x: 0.0,
                y: 0.0,
                z: 0.0,
                item_id: None,
                memo: String::new(),
                is_shared: true,
            },
        );
    });
    call_agbm(&ctx, &mut owner_sess, "DeleteFolder", &[PyRep::Integer(shared_fid)])
        .await
        .expect("delete root folder");
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.bookmark_folders.contains_key(&sfid));
        assert!(!snap.bookmarks.contains_key(&901));
    });
}

#[tokio::test]
async fn subfolder_and_known_folders_edge_cases_and_delegated_ops() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let owner_id = 100_000_020_i64;
    let corp_id = 1_000_009_i64;
    let mut sess = make_session(owner_id, corp_id, u64::MAX);

    // Create personal root folder (max 5 subfolders allowed)
    let root = call_agbm(
        &ctx,
        &mut sess,
        "AddFolder",
        &[PyRep::Bool(true), PyRep::String("MyPersonal".into())],
    )
    .await
    .unwrap();
    let pfid = root.dict_get("folderID").and_then(|v| v.as_i64()).unwrap();

    let mut first_sfid = 0;
    for i in 0..5 {
        let sf = call_agbm(
            &ctx,
            &mut sess,
            "CreateSubfolder",
            &[PyRep::Integer(pfid), PyRep::String(format!("S{i}"))],
        )
        .await
        .unwrap();
        if i == 0 {
            first_sfid = sf.dict_get("subfolderID").and_then(|v| v.as_i64()).unwrap();
        }
    }
    // 6th subfolder on personal folder -> MaxSubfoldersReached
    assert_eq!(
        call_agbm(
            &ctx,
            &mut sess,
            "CreateSubfolder",
            &[PyRep::Integer(pfid), PyRep::String("S5".into())],
        )
        .await
        .unwrap_err()
        .code,
        "MaxSubfoldersReached"
    );
    // Cannot create subfolder inside a subfolder
    assert_eq!(
        call_agbm(
            &ctx,
            &mut sess,
            "CreateSubfolder",
            &[PyRep::Integer(first_sfid), PyRep::String("Nested".into())],
        )
        .await
        .unwrap_err()
        .code,
        "UserError"
    );

    // 2-arg UpdateSubfolder passing parent folder ID resolves first child subfolder
    let upd2 = call_agbm(
        &ctx,
        &mut sess,
        "UpdateSubfolder",
        &[PyRep::Integer(pfid), PyRep::String("RenamedFirst".into())],
    )
    .await
    .unwrap();
    assert_eq!(upd2.dict_get("subfolderID"), Some(&PyRep::Integer(first_sfid)));

    // 1-arg DeleteSubfolder
    let del1 = call_agbm(&ctx, &mut sess, "DeleteSubfolder", &[PyRep::Integer(first_sfid)])
        .await
        .unwrap();
    assert_eq!(del1, PyRep::List(vec![PyRep::Integer(first_sfid)]));

    // MaxActiveSharedFoldersReached when activating a 4th folder (pfid is already active #1)
    for i in 0..3 {
        let f = call_agbm(
            &ctx,
            &mut sess,
            "AddFolder",
            &[PyRep::String(format!("Shared_{i}")), PyRep::Bool(true)],
        )
        .await
        .unwrap();
        let fid = f.dict_get("folderID").and_then(|v| v.as_i64()).unwrap();
        if i < 2 {
            call_agbm(
                &ctx,
                &mut sess,
                "AddToKnownFolders",
                &[PyRep::Integer(fid), PyRep::Bool(true)],
            )
            .await
            .expect("activate folder");
        } else {
            // Unset known state inserted by AddFolder so we test AddToKnownFolders hitting 3-active cap
            ctx.db.with_mut_snapshot(|s| {
                s.bookmark_folder_known_by.remove(&(fid, owner_id));
            });
            assert_eq!(
                call_agbm(
                    &ctx,
                    &mut sess,
                    "AddToKnownFolders",
                    &[PyRep::Integer(fid), PyRep::Bool(true)],
                )
                .await
                .unwrap_err()
                .code,
                "MaxActiveSharedFoldersReached"
            );
        }
    }

    // Delegated bookmark operations through handle()
    let bm = call_agbm(
        &ctx,
        &mut sess,
        "BookmarkStaticLocation",
        &[
            PyRep::Integer(30_000_142),
            PyRep::Integer(pfid),
            PyRep::String("Spot".into()),
            PyRep::String("Memo".into()),
            PyRep::Integer(5),
        ],
    )
    .await
    .unwrap();
    let PyRep::Tuple(bm_tup) = bm else {
        panic!("expected 8-tuple from BookmarkStaticLocation");
    };
    let bid = bm_tup[0].as_i64().unwrap();
    assert!(call_agbm(&ctx, &mut sess, "GetMyActiveBookmarks", &[]).await.is_ok());
    assert!(call_agbm(
        &ctx,
        &mut sess,
        "UpdateBookmark",
        &[
            PyRep::Integer(bid),
            PyRep::Integer(pfid),
            PyRep::String("Spot2".into()),
            PyRep::String("Memo2".into()),
        ],
    )
    .await
    .is_ok());
    assert!(call_agbm(
        &ctx,
        &mut sess,
        "CopyBookmarksToFolderAndSubfolder",
        &[PyRep::Integer(pfid), PyRep::None, PyRep::List(vec![PyRep::Integer(bid)])],
    )
    .await
    .is_ok());
    assert!(call_agbm(
        &ctx,
        &mut sess,
        "MoveBookmarksToFolderAndSubfolder",
        &[PyRep::Integer(pfid), PyRep::None, PyRep::List(vec![PyRep::Integer(bid)])],
    )
    .await
    .is_ok());
    assert!(call_agbm(
        &ctx,
        &mut sess,
        "DeleteBookmarks",
        &[PyRep::List(vec![PyRep::Integer(bid)])],
    )
    .await
    .is_ok());
}
