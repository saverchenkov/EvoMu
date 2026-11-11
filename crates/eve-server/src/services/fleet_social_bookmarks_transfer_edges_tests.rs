#![forbid(unsafe_code)]

use super::*;
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::{
    clone_char_with, test_ctx_with_clock,
};
use eve_db::{corp_roles, BookmarkFolderRecord, BookmarkRecord};
use eve_machonet::SessionState;
use eve_proto::PyRep;

fn make_sess(char_id: i64, corp_id: i64, role: u64) -> SessionState {
    SessionState {
        session_id: char_id as u64,
        user_id: Some(char_id as u64),
        character_id: Some(char_id),
        corporation_id: Some(corp_id),
        solar_system_id: Some(30_000_142),
        corp_role: role,
        ..Default::default()
    }
}

fn make_bm(id: i64, owner_id: i64, folder_id: Option<i64>, is_shared: bool) -> BookmarkRecord {
    BookmarkRecord {
        bookmark_id: id,
        owner_id,
        label: format!("BM-{id}"),
        solar_system_id: 30_000_142,
        x: 100.0,
        y: 200.0,
        z: 300.0,
        item_id: None,
        memo: "memo".into(),
        folder_id,
        is_shared,
    }
}

#[test]
fn transfer_bookmarks_validation_acl_capacity_and_subfolder_errors() {
    let ctx = test_ctx_with_clock(1_800_000_000);
    let owner_id = 90_000_201_i64;
    let peer_id = 90_000_202_i64;
    let admin_id = 90_000_203_i64;
    let outsider_id = 90_000_204_i64;
    let corp_id = 98_000_201_i64;
    let other_corp_id = 98_000_202_i64;

    let owner_sess = make_sess(owner_id, corp_id, 0);
    let peer_sess = make_sess(peer_id, corp_id, 0);
    let admin_sess = make_sess(admin_id, corp_id, corp_roles::ROLE_DIRECTOR);
    let outsider_sess = make_sess(outsider_id, other_corp_id, 0);

    let shared_use_fid = 820_001_i64;
    let shared_view_fid = 820_002_i64;
    let shared_manage_fid = 820_003_i64;
    let valid_sfid = 820_010_i64;
    let wrong_sfid = 820_011_i64;
    let full_personal_fid = 820_020_i64;
    let full_shared_fid = 820_021_i64;

    ctx.db.with_mut_snapshot(|snap| {
        for (cid, name, co) in [
            (owner_id, "Owner", corp_id),
            (peer_id, "Peer", corp_id),
            (admin_id, "Admin", corp_id),
            (outsider_id, "Outsider", other_corp_id),
        ] {
            let c = clone_char_with(snap, cid, name, co, 30_000_142, 100_000_000 + cid);
            snap.characters.insert(cid, c);
        }
        snap.corporations
            .entry(corp_id)
            .or_default()
            .member_roles
            .insert(admin_id, corp_roles::ROLE_DIRECTOR);

        snap.access_groups.insert(901, vec![owner_id, peer_id]); // View
        snap.access_groups.insert(902, vec![owner_id, peer_id, admin_id]); // Use
        snap.access_groups.insert(903, vec![owner_id, peer_id]); // Manage

        snap.bookmark_folders.insert(
            shared_use_fid,
            BookmarkFolderRecord {
                folder_id: shared_use_fid,
                owner_id,
                folder_name: "SharedUse".into(),
                is_shared: true,
                use_group_id: Some(902),
                ..Default::default()
            },
        );
        snap.bookmark_folders.insert(
            shared_view_fid,
            BookmarkFolderRecord {
                folder_id: shared_view_fid,
                owner_id,
                folder_name: "SharedView".into(),
                is_shared: true,
                view_group_id: Some(901),
                ..Default::default()
            },
        );
        snap.bookmark_folders.insert(
            shared_manage_fid,
            BookmarkFolderRecord {
                folder_id: shared_manage_fid,
                owner_id,
                folder_name: "SharedManage".into(),
                is_shared: true,
                manage_group_id: Some(903),
                ..Default::default()
            },
        );
        snap.bookmark_folders.insert(
            valid_sfid,
            BookmarkFolderRecord {
                folder_id: valid_sfid,
                owner_id,
                folder_name: "SharedUse/ValidSub".into(),
                is_shared: true,
                ..Default::default()
            },
        );
        snap.bookmark_folders.insert(
            wrong_sfid,
            BookmarkFolderRecord {
                folder_id: wrong_sfid,
                owner_id,
                folder_name: "SharedView/WrongSub".into(),
                is_shared: true,
                ..Default::default()
            },
        );
        snap.bookmark_subfolder_parents.insert(valid_sfid, shared_use_fid);
        snap.bookmark_subfolder_parents.insert(wrong_sfid, shared_view_fid);

        snap.bookmark_folders.insert(
            full_personal_fid,
            BookmarkFolderRecord {
                folder_id: full_personal_fid,
                owner_id,
                folder_name: "FullPersonal".into(),
                is_shared: false,
                ..Default::default()
            },
        );
        snap.bookmark_folders.insert(
            full_shared_fid,
            BookmarkFolderRecord {
                folder_id: full_shared_fid,
                owner_id,
                folder_name: "FullShared".into(),
                is_shared: true,
                use_group_id: Some(902),
                ..Default::default()
            },
        );
        // Fill personal (499 + 70_006 = 500) and shared (500) folders to capacity 500
        for i in 0..499_i64 {
            snap.bookmarks.insert(50_000 + i, make_bm(50_000 + i, owner_id, Some(full_personal_fid), false));
        }
        for i in 0..500_i64 {
            snap.bookmarks.insert(60_000 + i, make_bm(60_000 + i, owner_id, Some(full_shared_fid), true));
        }

        // Source bookmarks for ACL checks
        snap.bookmarks.insert(70_001, make_bm(70_001, owner_id, None, false));
        snap.bookmarks.insert(70_002, make_bm(70_002, owner_id, Some(shared_view_fid), true));
        snap.bookmarks.insert(70_003, make_bm(70_003, peer_id, Some(shared_view_fid), true));
        snap.bookmarks.insert(70_004, make_bm(70_004, owner_id, Some(shared_manage_fid), true));
        snap.bookmarks.insert(70_005, make_bm(70_005, owner_id, None, true)); // folderless corp shared
        snap.bookmarks.insert(70_006, make_bm(70_006, owner_id, Some(full_personal_fid), false));
    });

    // 1. Unauthorized
    let unauth = SessionState::default();
    assert_eq!(
        copy_bookmarks_to_folder_and_subfolder(&ctx, &unauth, &[]).unwrap().unwrap_err().code,
        "Unauthorized"
    );

    // 2. Batch limit > 20
    let too_many = PyRep::List((1..=21).map(PyRep::Integer).collect());
    assert_eq!(
        move_bookmarks_to_folder_and_subfolder(&ctx, &owner_sess, &[too_many]).unwrap().unwrap_err().code,
        "BookmarkCopyBatchLimitExceeded"
    );

    // 3. Target folder not found
    assert_eq!(
        copy_bookmarks_to_folder_and_subfolder(&ctx, &owner_sess, &[PyRep::List(vec![PyRep::Integer(70_001)]), PyRep::Integer(999_999)])
            .unwrap()
            .unwrap_err()
            .code,
        "FolderNotFound"
    );

    // 4. Target folder access denied (< Use)
    assert_eq!(
        copy_bookmarks_to_folder_and_subfolder(&ctx, &outsider_sess, &[PyRep::List(vec![PyRep::Integer(70_001)]), PyRep::Integer(shared_use_fid)])
            .unwrap()
            .unwrap_err()
            .code,
        "BookmarkFolderAccessDenied"
    );

    // 5. Target subfolder mismatch
    assert_eq!(
        copy_bookmarks_to_folder_and_subfolder(
            &ctx,
            &owner_sess,
            &[
                PyRep::List(vec![PyRep::Integer(70_001)]),
                PyRep::Integer(shared_use_fid),
                PyRep::Integer(wrong_sfid),
            ],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "BookmarkFolderAccessDenied"
    );

    // 6. Capacity 500 exceeded (personal vs shared)
    assert_eq!(
        copy_bookmarks_to_folder_and_subfolder(
            &ctx,
            &owner_sess,
            &[PyRep::List(vec![PyRep::Integer(70_001)]), PyRep::Integer(full_personal_fid)],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "MaxBookmarksReached"
    );
    assert_eq!(
        copy_bookmarks_to_folder_and_subfolder(
            &ctx,
            &owner_sess,
            &[PyRep::List(vec![PyRep::Integer(70_001)]), PyRep::Integer(full_shared_fid)],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "MaxSharedFolderBookmarksReached"
    );

    // Moving a bookmark already in full_personal_fid (added_count == 0) succeeds even at 500 capacity
    let same_folder_move = move_bookmarks_to_folder_and_subfolder(
        &ctx,
        &owner_sess,
        &[PyRep::List(vec![PyRep::Integer(70_006)]), PyRep::Integer(full_personal_fid)],
    )
    .unwrap()
    .unwrap();
    assert_eq!(same_folder_move.dict_get("folderID"), Some(&PyRep::Integer(full_personal_fid)));

    // 7. Bookmark not found
    assert_eq!(
        copy_bookmarks_to_folder_and_subfolder(&ctx, &owner_sess, &[PyRep::List(vec![PyRep::Integer(999_888)]), PyRep::Integer(0)])
            .unwrap()
            .unwrap_err()
            .code,
        "BookmarkNotFound"
    );

    // 8. Source ACL failures
    // Outsider copying from shared_view_fid (< View) -> BookmarkAccessDenied
    assert_eq!(
        copy_bookmarks_to_folder_and_subfolder(&ctx, &outsider_sess, &[PyRep::List(vec![PyRep::Integer(70_002)]), PyRep::None])
            .unwrap()
            .unwrap_err()
            .code,
        "BookmarkAccessDenied"
    );
    // Peer moving own shared bookmark 70_003 in shared_view_fid (< Use) -> BookmarkAccessDenied
    assert_eq!(
        move_bookmarks_to_folder_and_subfolder(&ctx, &peer_sess, &[PyRep::List(vec![PyRep::Integer(70_003)]), PyRep::Integer(0)])
            .unwrap()
            .unwrap_err()
            .code,
        "BookmarkAccessDenied"
    );
    // Peer moving owner's bookmark 70_002 in shared_view_fid (< Manage) -> BookmarkAccessDenied
    assert_eq!(
        move_bookmarks_to_folder_and_subfolder(&ctx, &peer_sess, &[PyRep::List(vec![PyRep::Integer(70_002)]), PyRep::Integer(0)])
            .unwrap()
            .unwrap_err()
            .code,
        "BookmarkAccessDenied"
    );
    // Peer (non-admin) moving folderless corp shared bookmark 70_005 -> BookmarkAccessDenied
    assert_eq!(
        move_bookmarks_to_folder_and_subfolder(&ctx, &peer_sess, &[PyRep::List(vec![PyRep::Integer(70_005)]), PyRep::Integer(0)])
            .unwrap()
            .unwrap_err()
            .code,
        "BookmarkAccessDenied"
    );

    // 9. Success paths:
    // Peer copies folderless corp shared bookmark 70_005 into shared_use_fid + valid_sfid
    let copy_ok = copy_bookmarks_to_folder_and_subfolder(
        &ctx,
        &peer_sess,
        &[
            PyRep::List(vec![PyRep::Integer(70_005)]),
            PyRep::Integer(shared_use_fid),
            PyRep::Integer(valid_sfid),
        ],
    )
    .unwrap()
    .unwrap();
    let new_id = match copy_ok.dict_get("bookmarkIDs").unwrap() {
        PyRep::List(l) => l[0].as_i64().unwrap(),
        _ => panic!("expected list"),
    };
    assert_ne!(new_id, 70_005);
    ctx.db.with_snapshot(|snap| {
        let bm = snap.bookmarks.get(&new_id).unwrap();
        assert_eq!(bm.owner_id, peer_id);
        assert_eq!(bm.folder_id, Some(shared_use_fid));
        assert!(bm.is_shared);
        assert_eq!(snap.bookmark_subfolders.get(&new_id), Some(&valid_sfid));
    });

    // Peer moves owner's bookmark 70_004 from shared_manage_fid (>= Manage) to personal root (char_id)
    let move_mgr_ok = move_bookmarks_to_folder_and_subfolder(
        &ctx,
        &peer_sess,
        &[PyRep::List(vec![PyRep::Integer(70_004)]), PyRep::Integer(peer_id)],
    )
    .unwrap()
    .unwrap();
    assert_eq!(move_mgr_ok.dict_get("folderID"), Some(&PyRep::Integer(peer_id)));

    // Corp admin moves folderless corp shared bookmark 70_005 into shared_use_fid + valid_sfid
    let move_admin_ok = move_bookmarks_to_folder_and_subfolder(
        &ctx,
        &admin_sess,
        &[
            PyRep::List(vec![PyRep::Integer(70_005)]),
            PyRep::Integer(shared_use_fid),
            PyRep::Integer(valid_sfid),
        ],
    )
    .unwrap()
    .unwrap();
    assert_eq!(move_admin_ok.dict_get("folderID"), Some(&PyRep::Integer(shared_use_fid)));
    ctx.db.with_snapshot(|snap| {
        let bm = snap.bookmarks.get(&70_005).unwrap();
        assert_eq!(bm.folder_id, Some(shared_use_fid));
        assert_eq!(snap.bookmark_subfolders.get(&70_005), Some(&valid_sfid));
    });

    // Owner moves personal bookmark 70_001 to None
    let move_personal = move_bookmarks_to_folder_and_subfolder(
        &ctx,
        &owner_sess,
        &[PyRep::List(vec![PyRep::Integer(70_001)])],
    )
    .unwrap()
    .unwrap();
    assert_eq!(move_personal.dict_get("folderID"), Some(&PyRep::None));
}
