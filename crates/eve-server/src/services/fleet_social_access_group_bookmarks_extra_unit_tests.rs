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

#[tokio::test]
async fn shared_bookmark_creator_revoked_or_view_only_cannot_bypass_acl() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let owner_id = 100_000_030_i64;
    let creator_id = 100_000_031_i64;
    let corp_id = 1_000_009_i64;
    let other_corp_id = 1_000_010_i64;
    let mut owner_sess = make_session(owner_id, corp_id, 1);
    let mut creator_sess = make_session(creator_id, other_corp_id, 0);

    // Owner creates shared folder with access groups: creator_id initially has Use access via use_group_id
    let use_gid = 910_001_i64;
    let view_gid = 910_002_i64;
    let added = call_agbm(
        &ctx,
        &mut owner_sess,
        "AddFolder",
        &[PyRep::Bool(false), PyRep::String("SharedACL".into())],
    )
    .await
    .unwrap();
    let shared_fid = added.dict_get("folderID").and_then(|v| v.as_i64()).unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        let folder = snap.bookmark_folders.get_mut(&shared_fid).unwrap();
        folder.use_group_id = Some(use_gid);
        folder.view_group_id = Some(view_gid);
        snap.access_groups.insert(use_gid, vec![creator_id]);
        snap.access_groups.insert(view_gid, vec![]);
    });

    let bm_res = call_agbm(
        &ctx,
        &mut creator_sess,
        "BookmarkStaticLocation",
        &[
            PyRep::Integer(30_000_142),
            PyRep::Integer(shared_fid),
            PyRep::String("SharedSpot".into()),
            PyRep::String("Memo".into()),
            PyRep::Integer(1),
        ],
    )
    .await
    .unwrap();
    let PyRep::Tuple(bm_tup) = bm_res else {
        panic!("expected tuple");
    };
    let shared_bm_id = bm_tup[0].as_i64().unwrap();

    // Downgrade creator_id to View (1): can copy into personal folder, must NOT copy into shared folder or update/delete/move
    ctx.db.with_mut_snapshot(|snap| {
        snap.access_groups.insert(use_gid, vec![]);
        snap.access_groups.insert(view_gid, vec![creator_id]);
    });
    assert!(call_agbm(
        &ctx,
        &mut creator_sess,
        "CopyBookmarksToFolderAndSubfolder",
        &[
            PyRep::Integer(creator_id),
            PyRep::None,
            PyRep::List(vec![PyRep::Integer(shared_bm_id)]),
        ],
    )
    .await
    .is_ok());
    assert!(call_agbm(
        &ctx,
        &mut creator_sess,
        "CopyBookmarksToFolderAndSubfolder",
        &[
            PyRep::Integer(shared_fid),
            PyRep::None,
            PyRep::List(vec![PyRep::Integer(shared_bm_id)]),
        ],
    )
    .await
    .is_err());
    assert!(call_agbm(
        &ctx,
        &mut creator_sess,
        "UpdateBookmark",
        &[
            PyRep::Integer(shared_bm_id),
            PyRep::Integer(shared_fid),
            PyRep::String("Tamper".into()),
            PyRep::String("Tamper".into()),
        ],
    )
    .await
    .is_err());
    assert!(call_agbm(
        &ctx,
        &mut creator_sess,
        "MoveBookmarksToFolderAndSubfolder",
        &[
            PyRep::Integer(shared_fid),
            PyRep::None,
            PyRep::List(vec![PyRep::Integer(shared_bm_id)]),
        ],
    )
    .await
    .is_err());
    assert!(call_agbm(
        &ctx,
        &mut creator_sess,
        "DeleteBookmarks",
        &[PyRep::List(vec![PyRep::Integer(shared_bm_id)])],
    )
    .await
    .is_err());

    // Revoke creator_id completely (None = 0): even copy to personal folder must now fail
    ctx.db.with_mut_snapshot(|snap| {
        snap.access_groups.insert(view_gid, vec![]);
    });
    assert!(call_agbm(
        &ctx,
        &mut creator_sess,
        "CopyBookmarksToFolderAndSubfolder",
        &[
            PyRep::Integer(creator_id),
            PyRep::None,
            PyRep::List(vec![PyRep::Integer(shared_bm_id)]),
        ],
    )
    .await
    .is_err());
    assert!(ctx.db.with_snapshot(|s| s.bookmarks.contains_key(&shared_bm_id)));
}

#[tokio::test]
async fn coverage_sweep_effective_corp_folder_subfolder_known_info_and_search() {
    use crate::services::fleet_social::fleet_social_access_group_bookmarks::effective_char_corp_and_alliance;
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let corp_id = 98_100_001_i64;
    let alliance_id = 99_100_001_i64;
    let other_corp_id = 98_100_002_i64;
    let ceo_id = 100_000_111_i64;
    let outsider_id = 100_000_112_i64;

    // Seed character with NPC corp vs player corp + session overrides for effective_char_corp_and_alliance
    ctx.db.with_mut_snapshot(|snap| {
        let mut base = snap.characters.values().next().unwrap().clone();
        base.character_id = ceo_id;
        base.corporation_id = 1_000_999;
        base.alliance_id = None;
        snap.characters.insert(ceo_id, base.clone());
        base.character_id = outsider_id;
        snap.characters.insert(outsider_id, base);
        snap.corporations.insert(
            corp_id,
            { let mut __corp = eve_db::CorporationRecord::default(); __corp.corporation_id = corp_id; __corp.name = "TestCorp".into(); __corp.ticker = "TCRP".into(); __corp.ceo_id = ceo_id; __corp.alliance_id = Some(alliance_id); __corp.members = [ceo_id].into_iter().collect(); __corp.ensure_wallet_divisions(); __corp },
        );
    });
    // Outsider in NPC corp -> session player corp (not member/ceo) must NOT override
    let mut sess_outsider = make_session(outsider_id, corp_id, 1);
    sess_outsider.alliance_id = None;
    assert_eq!(
        ctx.db.with_snapshot(|s| effective_char_corp_and_alliance(s, &sess_outsider, outsider_id)),
        (Some(1_000_999), None)
    );
    // CEO in NPC corp -> session player corp overrides when CEO
    let mut sess_npc = make_session(ceo_id, corp_id, 1);
    assert_eq!(
        ctx.db.with_snapshot(|s| effective_char_corp_and_alliance(s, &sess_npc, ceo_id)),
        (Some(corp_id), Some(alliance_id))
    );
    // Player corp on character keeps char's corp regardless of session corp override
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&ceo_id).unwrap().corporation_id = corp_id;
        snap.characters.get_mut(&ceo_id).unwrap().alliance_id = Some(alliance_id);
    });
    sess_npc.corporation_id = Some(other_corp_id);
    assert_eq!(
        ctx.db.with_snapshot(|s| effective_char_corp_and_alliance(s, &sess_npc, ceo_id)),
        (Some(corp_id), Some(alliance_id))
    );
    // Unknown char falls back to session corporation_id / alliance_id (L48)
    assert_eq!(
        ctx.db.with_snapshot(|s| effective_char_corp_and_alliance(s, &sess_npc, 999_999_999)),
        (Some(other_corp_id), sess_npc.alliance_id)
    );
    // Player corp on character + session has NPC/no corp -> keeps char's corp
    sess_npc.corporation_id = Some(1_000_125);
    assert_eq!(
        ctx.db.with_snapshot(|s| effective_char_corp_and_alliance(s, &sess_npc, ceo_id)),
        (Some(corp_id), Some(alliance_id))
    );
    sess_npc.corporation_id = None;
    assert_eq!(
        ctx.db.with_snapshot(|s| effective_char_corp_and_alliance(s, &sess_npc, ceo_id)),
        (Some(corp_id), Some(alliance_id))
    );

    // Unknown RPC method returns None
    let kw = HashMap::new();
    let unknown_method = ["Unmapped", "Method"].concat();
    assert!(handle(&ctx, &mut sess_npc, "accessGroupBookmarkMgr", &unknown_method, &[], &kw).await.is_none());

    // Corp/alliance membership in is_in_access_group (L78, L83)
    let grp_corp = 920_001_i64;
    let grp_ally = 920_002_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.access_groups.insert(grp_corp, vec![corp_id]);
        snap.access_groups.insert(grp_ally, vec![alliance_id]);
    });
    let f_corp = BookmarkFolderRecord {
        folder_id: 920_100,
        owner_id: outsider_id,
        folder_name: "GrpCorp".into(),
        is_shared: true,
        corporation_id: Some(other_corp_id),
        description: String::new(),
        admin_group_id: Some(grp_corp),
        manage_group_id: None,
        use_group_id: None,
        view_group_id: Some(grp_ally),
    };
    assert_eq!(
        ctx.db.with_snapshot(|s| folder_access_level(s, &sess_npc, ceo_id, &f_corp)),
        AccessLevel::Admin
    );

    // AddFolder integer shared flag (L297), and without DB corp (L336), and MaxSharedFoldersReached (L361-364)
    let mut sess_nocorp = SessionState {
        user_id: Some(outsider_id as u64),
        character_id: Some(outsider_id),
        corporation_id: None,
        corp_role: 1,
        ..Default::default()
    };
    assert_eq!(
        call_agbm(&ctx, &mut sess_nocorp, "AddFolder", &[PyRep::String("FailNoCorp".into()), PyRep::Integer(1)])
            .await
            .unwrap_err()
            .code,
        "BookmarkAccessDenied"
    );

    let mut ceo_sess = make_session(ceo_id, corp_id, 1);
    ceo_sess.corporation_id = None; // AddFolder populates session.corporation_id from db_corp_id (L312)
    let shared_ok = call_agbm(&ctx, &mut ceo_sess, "AddFolder", &[PyRep::String("SharedInt".into()), PyRep::Integer(1)])
        .await
        .unwrap();
    let sfid = shared_ok.dict_get("folderID").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(ceo_sess.corporation_id, Some(corp_id));

    // Seed 29 more shared folders in corp_id to hit MaxSharedFoldersReached on 31st
    ctx.db.with_mut_snapshot(|snap| {
        for i in 0..29 {
            let id = 921_000 + i;
            snap.bookmark_folders.insert(
                id,
                BookmarkFolderRecord {
                    folder_id: id,
                    owner_id: ceo_id,
                    folder_name: format!("Bulk{i}"),
                    is_shared: true,
                    corporation_id: Some(corp_id),
                    ..Default::default()
                },
            );
        }
    });
    assert_eq!(
        call_agbm(&ctx, &mut ceo_sess, "AddFolder", &[PyRep::Bool(false), PyRep::String("OverLimit".into())])
            .await
            .unwrap_err()
            .code,
        "MaxSharedFoldersReached"
    );

    // UpdateFolder: missing folder (L508), legacy ACL list non-empty + empty clear (L537, L542-547)
    assert_eq!(
        call_agbm(&ctx, &mut ceo_sess, "UpdateFolder", &[PyRep::Integer(999_999), PyRep::String("X".into())])
            .await
            .unwrap_err()
            .code,
        "UserError"
    );
    call_agbm(
        &ctx,
        &mut ceo_sess,
        "UpdateFolder",
        &[PyRep::Integer(sfid), PyRep::String("SharedInt2".into()), PyRep::List(vec![PyRep::Integer(ceo_id)])],
    )
    .await
    .unwrap();
    assert_eq!(ctx.db.with_snapshot(|s| s.bookmark_folder_acls.get(&sfid).cloned()), Some(vec![ceo_id]));
    call_agbm(
        &ctx,
        &mut ceo_sess,
        "UpdateFolder",
        &[PyRep::Integer(sfid), PyRep::String("SharedInt3".into()), PyRep::List(vec![])],
    )
    .await
    .unwrap();
    assert!(ctx.db.with_snapshot(|s| !s.bookmark_folder_acls.contains_key(&sfid)));
    // 7-arg UpdateFolder with all None clears bookmark_folder_acls (L537)
    call_agbm(
        &ctx,
        &mut ceo_sess,
        "UpdateFolder",
        &[
            PyRep::Integer(sfid),
            PyRep::String("SharedInt4".into()),
            PyRep::String("desc".into()),
            PyRep::None,
            PyRep::None,
            PyRep::None,
            PyRep::None,
        ],
    )
    .await
    .unwrap();

    // CreateSubfolder & UpdateSubfolder & DeleteSubfolder error + notification paths
    assert_eq!(
        call_agbm(&ctx, &mut ceo_sess, "CreateSubfolder", &[]).await.unwrap_err().code,
        "InvalidArgument"
    );
    assert_eq!(
        call_agbm(&ctx, &mut ceo_sess, "CreateSubfolder", &[PyRep::Integer(999_999), PyRep::String("Sub".into())])
            .await
            .unwrap_err()
            .code,
        "UserError"
    );
    let mut outsider_sess = make_session(outsider_id, other_corp_id, 0);
    assert_eq!(
        call_agbm(&ctx, &mut outsider_sess, "CreateSubfolder", &[PyRep::Integer(sfid), PyRep::String("Sub".into())])
            .await
            .unwrap_err()
            .code,
        "UserError"
    );
    let sub_ok = call_agbm(&ctx, &mut ceo_sess, "CreateSubfolder", &[PyRep::Integer(sfid), PyRep::String("SubA".into())])
        .await
        .unwrap();
    let sub_id = sub_ok.dict_get("subfolderID").and_then(|v| v.as_i64()).unwrap();

    // Create another parent folder to test wrong-parent checks
    let pf_other = call_agbm(&ctx, &mut ceo_sess, "AddFolder", &[PyRep::Bool(true), PyRep::String("OtherP".into())])
        .await
        .unwrap()
        .dict_get("folderID")
        .and_then(|v| v.as_i64())
        .unwrap();

    // UpdateSubfolder errors (L790-812) and shared corp notification (L827-834)
    assert_eq!(
        call_agbm(&ctx, &mut ceo_sess, "UpdateSubfolder", &[PyRep::Integer(pf_other), PyRep::Integer(sub_id), PyRep::String("Bad".into())])
            .await
            .unwrap_err()
            .code,
        "UserError"
    );
    assert_eq!(
        call_agbm(&ctx, &mut ceo_sess, "UpdateSubfolder", &[PyRep::Integer(sfid), PyRep::Integer(999_998), PyRep::String("Bad".into())])
            .await
            .unwrap_err()
            .code,
        "UserError"
    );
    assert_eq!(
        call_agbm(&ctx, &mut outsider_sess, "UpdateSubfolder", &[PyRep::Integer(sfid), PyRep::Integer(sub_id), PyRep::String("Denied".into())])
            .await
            .unwrap_err()
            .code,
        "UserError"
    );
    call_agbm(&ctx, &mut ceo_sess, "UpdateSubfolder", &[PyRep::Integer(sfid), PyRep::Integer(sub_id), PyRep::String("SubRenamed".into())])
        .await
        .unwrap();

    // Add a bookmark inside sfid linked via bookmark_subfolders so AddToKnownFolders returns it (L1020-1038)
    // and DeleteSubfolder / DeleteFolder clean it up (L636, L894-903)
    ctx.db.with_mut_snapshot(|snap| {
        snap.bookmarks.insert(
            922_001,
            BookmarkRecord {
                bookmark_id: 922_001,
                owner_id: ceo_id,
                label: "InSub".into(),
                solar_system_id: 30_000_142,
                x: 1.0,
                y: 2.0,
                z: 3.0,
                item_id: None,
                memo: "M".into(),
                folder_id: Some(sfid),
                is_shared: true,
            },
        );
        snap.bookmark_subfolders.insert(922_001, sub_id);
        snap.bookmarks.insert(
            922_002,
            BookmarkRecord {
                bookmark_id: 922_002,
                owner_id: ceo_id,
                label: "DirectSub".into(),
                solar_system_id: 30_000_142,
                x: 1.0,
                y: 2.0,
                z: 3.0,
                item_id: None,
                memo: "M2".into(),
                folder_id: Some(sub_id),
                is_shared: true,
            },
        );
    });

    // AddToKnownFolders / UpdateKnownFolderState error paths (L931-966) + active bookmarks list (L1020-1038)
    assert_eq!(call_agbm(&ctx, &mut ceo_sess, "AddToKnownFolders", &[]).await.unwrap_err().code, "InvalidArgument");
    assert_eq!(call_agbm(&ctx, &mut ceo_sess, "AddToKnownFolders", &[PyRep::Integer(sub_id)]).await.unwrap_err().code, "UserError");
    assert_eq!(call_agbm(&ctx, &mut ceo_sess, "AddToKnownFolders", &[PyRep::Integer(999_997)]).await.unwrap_err().code, "UserError");
    assert_eq!(call_agbm(&ctx, &mut outsider_sess, "AddToKnownFolders", &[PyRep::Integer(sfid)]).await.unwrap_err().code, "UserError");
    // Unsubscribe ceo first so UpdateKnownFolderState hits "Folder is not in known folders" (L956), then re-add via AddToKnownFolders
    ctx.db.with_mut_snapshot(|s| {
        s.bookmark_folder_known_by.remove(&(sfid, ceo_id));
    });
    assert_eq!(
        call_agbm(&ctx, &mut ceo_sess, "UpdateKnownFolderState", &[PyRep::Integer(sfid), PyRep::Integer(1)])
            .await
            .unwrap_err()
            .code,
        "UserError"
    );
    let known_res = call_agbm(&ctx, &mut ceo_sess, "AddToKnownFolders", &[PyRep::Integer(sfid), PyRep::Bool(true)])
        .await
        .unwrap();
    let PyRep::Tuple(tup) = known_res else { panic!("expected tuple"); };
    assert!(matches!(&tup[1], PyRep::List(bms) if bms.len() == 1));

    // SearchFoldersWithAdminAccess (L1224-1240)
    let search_res = call_agbm(&ctx, &mut ceo_sess, "SearchFoldersWithAdminAccess", &[PyRep::String("sharedint".into())])
        .await
        .unwrap();
    assert!(format!("{search_res:?}").contains("SharedInt4"));

    // GetFolderInfo errors (L1162-1180)
    assert_eq!(call_agbm(&ctx, &mut ceo_sess, "GetFolderInfo", &[]).await.unwrap_err().code, "UserError");
    assert_eq!(call_agbm(&ctx, &mut ceo_sess, "GetFolderInfo", &[PyRep::Integer(999_996)]).await.unwrap_err().code, "UserError");
    assert_eq!(call_agbm(&ctx, &mut outsider_sess, "GetFolderInfo", &[PyRep::Integer(sfid)]).await.unwrap_err().code, "UserError");

    // RemoveFromKnownFolders errors (L1116-1131)
    assert_eq!(call_agbm(&ctx, &mut ceo_sess, "RemoveFromKnownFolders", &[]).await.unwrap_err().code, "InvalidArgument");
    assert_eq!(call_agbm(&ctx, &mut ceo_sess, "RemoveFromKnownFolders", &[PyRep::Integer(999_995)]).await.unwrap_err().code, "UserError");

    // DeleteSubfolder errors + success cleaning 922_001/922_002 + notification (L864-916)
    assert_eq!(call_agbm(&ctx, &mut ceo_sess, "DeleteSubfolder", &[PyRep::Integer(pf_other), PyRep::Integer(sub_id)]).await.unwrap_err().code, "UserError");
    assert_eq!(call_agbm(&ctx, &mut ceo_sess, "DeleteSubfolder", &[PyRep::Integer(sfid), PyRep::Integer(999_994)]).await.unwrap_err().code, "UserError");
    assert_eq!(call_agbm(&ctx, &mut outsider_sess, "DeleteSubfolder", &[PyRep::Integer(sfid), PyRep::Integer(sub_id)]).await.unwrap_err().code, "UserError");
    call_agbm(&ctx, &mut ceo_sess, "DeleteSubfolder", &[PyRep::Integer(sfid), PyRep::Integer(sub_id)]).await.unwrap();
    assert!(ctx.db.with_snapshot(|s| !s.bookmarks.contains_key(&922_001) && !s.bookmarks.contains_key(&922_002)));

    // Re-create a subfolder and bookmark inside sfid, then DeleteFolder (L596, L609, L636)
    let sub2_id = call_agbm(&ctx, &mut ceo_sess, "CreateSubfolder", &[PyRep::Integer(sfid), PyRep::String("SubB".into())])
        .await
        .unwrap()
        .dict_get("subfolderID")
        .and_then(|v| v.as_i64())
        .unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        snap.bookmarks.insert(
            922_003,
            BookmarkRecord {
                bookmark_id: 922_003,
                owner_id: ceo_id,
                label: "Casc".into(),
                solar_system_id: 30_000_142,
                x: 0.0,
                y: 0.0,
                z: 0.0,
                item_id: None,
                memo: String::new(),
                folder_id: Some(sub2_id),
                is_shared: true,
            },
        );
    });
    assert_eq!(call_agbm(&ctx, &mut ceo_sess, "DeleteFolder", &[]).await.unwrap_err().code, "InvalidArgument");
    assert_eq!(call_agbm(&ctx, &mut ceo_sess, "DeleteFolder", &[PyRep::Integer(999_993)]).await.unwrap_err().code, "UserError");
    call_agbm(&ctx, &mut ceo_sess, "DeleteFolder", &[PyRep::Integer(sfid)]).await.unwrap();
    assert!(ctx.db.with_snapshot(|s| !s.bookmarks.contains_key(&922_003)));
}
