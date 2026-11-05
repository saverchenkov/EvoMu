//! Additional unit and mutation-killing tests for `fleet_social_bookmarks`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::fleet_social::{
    handle, is_bookmark_expired, sweep_expired_bookmarks_and_notify,
};
use eve_db::{
    corp_roles, BookmarkFolderRecord, BookmarkRecord, UpwellStructureRecord, WorldRepository,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

fn make_session(char_id: i64, corp_id: i64, sys_id: Option<u32>, role: u64) -> SessionState {
    let mut s = SessionState::new(701);
    s.user_id = Some(1);
    s.character_id = Some(char_id);
    s.corporation_id = Some(corp_id);
    s.solar_system_id = sys_id;
    s.corp_role = role;
    s
}

#[tokio::test]
async fn bookmark_location_station_structure_ship_and_quota_branches() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let corp_id = 98_000_001_i64;
    let sys_id = 30_000_142_u32;
    let mut sess = make_session(char_id, corp_id, None, corp_roles::ROLE_DIRECTOR);
    let kw = HashMap::new();

    // 1. NoSolarSystem error
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "bookmark",
            "BookmarkLocation",
            &[PyRep::Integer(60_003_760), PyRep::Integer(char_id), PyRep::String("Spot".into())],
            &kw
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "NoSolarSystem"
    );
    sess.solar_system_id = Some(sys_id);

    // 2. FolderNotFound when folder ID does not exist (args[1] is folder_id when args[0] is entity_id)
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "bookmark",
            "BookmarkLocation",
            &[
                PyRep::Integer(60_003_760),
                PyRep::Integer(999_888),
                PyRep::String("Spot".into()),
                PyRep::String("Memo".into()),
            ],
            &kw
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "FolderNotFound"
    );

    // 3. ShipNotInSpace when session.ship_id is set but not spawned in ballpark (and explicit_entity_id is None)
    sess.ship_id = Some(100_000_001);
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "bookmark",
            "BookmarkLocation",
            &[PyRep::String("InSpace".into()), PyRep::String("Memo".into())],
            &kw
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "ShipNotInSpace"
    );
    sess.ship_id = None;

    // 4. Station position fallback (session.station_id = Some(60_003_760)) + subfolderID & expiryHours
    sess.station_id = Some(60_003_760);
    let bm_res = handle(
        &ctx,
        &mut sess,
        "bookmark",
        "BookmarkLocation",
        &[
            PyRep::Integer(60_003_760),
            PyRep::Integer(char_id),
            PyRep::String("Jita Dock".into()),
            PyRep::String("Memo".into()),
            PyRep::Integer(42),
            PyRep::Integer(3),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(bm_kv) = bm_res else {
        panic!("expected KeyVal");
    };
    let bm_id = bm_kv["bookmarkID"].as_i64().unwrap();
    assert_eq!(ctx.db.with_snapshot(|s| s.bookmark_subfolders.get(&bm_id).copied()), Some(42));
    sess.station_id = None;

    // 5. Structure position fallback (session.structure_id when explicit_entity_id is None)
    let struct_id = 1_000_000_850_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                solar_system_id: sys_id,
                owner_corp_id: corp_id,
                type_id: 35832,
                name: "BM Struct".into(),
                position: [123.0, 456.0, 789.0],
                ..Default::default()
            },
        );
    });
    sess.structure_id = Some(struct_id);
    let st_bm = handle(
        &ctx,
        &mut sess,
        "beyonce",
        "BookmarkLocation",
        &[PyRep::String("Struct Spot".into()), PyRep::String("Memo".into())],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::Tuple(st_tup) = st_bm else {
        panic!("expected Tuple on beyonce.BookmarkLocation");
    };
    assert_eq!(st_tup[3], PyRep::Real(123.0));

    // 6. MaxBookmarksReached (500 personal bookmarks)
    ctx.db.with_mut_snapshot(|snap| {
        for i in 0..500 {
            let id = 800_000 + i;
            snap.bookmarks.insert(
                id,
                BookmarkRecord {
                    bookmark_id: id,
                    owner_id: char_id,
                    label: "Fill".into(),
                    solar_system_id: sys_id,
                    x: 0.0,
                    y: 0.0,
                    z: 0.0,
                    item_id: None,
                    memo: String::new(),
                    folder_id: None,
                    is_shared: false,
                },
            );
        }
    });
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "bookmark",
            "BookmarkLocation",
            &[PyRep::Integer(struct_id), PyRep::Integer(char_id), PyRep::String("OverQuota".into())],
            &kw
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "MaxBookmarksReached"
    );

    // Clear filler bookmarks, verify expiry helper on bm_id
    ctx.db.with_mut_snapshot(|snap| {
        snap.bookmarks.retain(|&k, _| k < 800_000);
    });
    assert!(!is_bookmark_expired(&ctx, bm_id, ctx.now_unix()));
    assert!(is_bookmark_expired(&ctx, bm_id, ctx.now_unix() + 20_000));
}

#[tokio::test]
async fn folder_management_known_folders_and_copy_move_batch_limits() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let other_char = 90_000_002_i64;
    let corp_id = 98_000_001_i64;
    let sys_id = 30_000_142_u32;
    let mut sess = make_session(char_id, corp_id, Some(sys_id), 0);
    let kw = HashMap::new();

    // 1. Non-director creating shared corp folder -> BookmarkAccessDenied
    ctx.db.with_mut_snapshot(|s| {
        if let Some(corp) = s.corporations.get_mut(&corp_id) {
            corp.ceo_id = other_char;
            corp.member_roles.insert(char_id, 0);
        }
    });
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "bookmark",
            "AddFolder",
            &[PyRep::Bool(false), PyRep::String("CorpShared".into())],
            &kw
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "BookmarkAccessDenied"
    );

    // 2. Create 5 personal root folders using Bool(true) first-arg form, 6th hits MaxPersonalFoldersReached
    let mut personal_fids = Vec::new();
    for i in 0..5 {
        let f = handle(
            &ctx,
            &mut sess,
            "bookmark",
            "AddFolder",
            &[
                PyRep::Bool(true),
                PyRep::String(format!("Personal {i}")),
                PyRep::String("Desc".into()),
            ],
            &kw,
        )
        .await
        .unwrap()
        .unwrap();
        let PyRep::KeyVal(kv) = f else {
            panic!("expected KeyVal");
        };
        personal_fids.push(kv["folderID"].as_i64().unwrap());
    }
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "bookmark",
            "AddFolder",
            &[PyRep::Bool(true), PyRep::String("Personal 6".into())],
            &kw
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "MaxPersonalFoldersReached"
    );

    // 3. Grant director role, create 4 shared folders, test AddToKnownFolders max 3 active limit
    sess.corp_role = corp_roles::ROLE_DIRECTOR;
    ctx.db.with_mut_snapshot(|s| {
        if let Some(corp) = s.corporations.get_mut(&corp_id) {
            corp.member_roles.insert(char_id, corp_roles::ROLE_DIRECTOR);
        }
    });
    let mut shared_fids = Vec::new();
    for i in 0..4 {
        let f = handle(
            &ctx,
            &mut sess,
            "bookmark",
            "AddFolder",
            &[PyRep::Bool(false), PyRep::String(format!("Shared {i}"))],
            &kw,
        )
        .await
        .unwrap()
        .unwrap();
        let PyRep::KeyVal(kv) = f else {
            panic!("expected KeyVal");
        };
        shared_fids.push(kv["folderID"].as_i64().unwrap());
    }
    // Remove auto-added known folders first, then add 3 and verify 4th fails with MaxActiveSharedFoldersReached
    for &fid in &shared_fids {
        let _ = handle(
            &ctx,
            &mut sess,
            "bookmark",
            "RemoveFromKnownFolders",
            &[PyRep::Integer(fid)],
            &kw,
        )
        .await;
    }
    for &fid in &shared_fids[..3] {
        handle(
            &ctx,
            &mut sess,
            "bookmark",
            "AddToKnownFolders",
            &[PyRep::Integer(fid)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap();
    }
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "bookmark",
            "AddToKnownFolders",
            &[PyRep::Integer(shared_fids[3])],
            &kw
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "MaxActiveSharedFoldersReached"
    );

    // 4. CopyBookmarksToFolderAndSubfolder batch limit (>20 IDs) -> BookmarkCopyBatchLimitExceeded
    let twenty_one: Vec<PyRep> = (1..=21).map(PyRep::Integer).collect();
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "bookmark",
            "CopyBookmarksToFolderAndSubfolder",
            &[PyRep::List(twenty_one), PyRep::Integer(personal_fids[0])],
            &kw
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "BookmarkCopyBatchLimitExceeded"
    );

    // 5. UpdateFolder with ACL list, CreateSubfolder, DeleteSubfolder, and AccessDenied for other pilot
    handle(
        &ctx,
        &mut sess,
        "bookmark",
        "UpdateFolder",
        &[
            PyRep::Integer(shared_fids[0]),
            PyRep::String("Shared Renamed".into()),
            PyRep::String("New Desc".into()),
            PyRep::List(vec![PyRep::Integer(555)]),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();

    let sub = handle(
        &ctx,
        &mut sess,
        "bookmark",
        "CreateSubfolder",
        &[PyRep::Integer(personal_fids[0]), PyRep::String("Sub 1".into())],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let sub_id = match sub {
        PyRep::KeyVal(kv) => kv["subfolderID"].as_i64().unwrap(),
        _ => panic!("expected KeyVal"),
    };

    let mut other_sess = make_session(other_char, 98_000_099, Some(sys_id), 0);
    assert_eq!(
        handle(
            &ctx,
            &mut other_sess,
            "bookmark",
            "DeleteSubfolder",
            &[PyRep::Integer(sub_id)],
            &kw
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "AccessDenied"
    );
    handle(
        &ctx,
        &mut sess,
        "bookmark",
        "DeleteSubfolder",
        &[PyRep::Integer(sub_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
}

#[tokio::test]
async fn update_delete_shared_bookmarks_sweep_notify_and_scan_result_branches() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let corp_id = 98_000_001_i64;
    let sys_id = 30_000_142_u32;
    let mut sess = make_session(char_id, corp_id, Some(sys_id), corp_roles::ROLE_DIRECTOR);
    let kw = HashMap::new();

    let shared_fid = 880_001_i64;
    let bm_id = 880_002_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.bookmark_folders.insert(
            shared_fid,
            BookmarkFolderRecord {
                folder_id: shared_fid,
                owner_id: char_id,
                folder_name: "CorpTac".into(),
                is_shared: true,
                corporation_id: Some(corp_id),
                ..Default::default()
            },
        );
        snap.bookmarks.insert(
            bm_id,
            BookmarkRecord {
                bookmark_id: bm_id,
                owner_id: char_id,
                label: "Shared Ping".into(),
                solar_system_id: sys_id,
                x: 10.0,
                y: 20.0,
                z: 30.0,
                item_id: None,
                memo: "Old".into(),
                folder_id: Some(shared_fid),
                is_shared: true,
            },
        );
    });

    // UpdateBookmark: set expiryHours = 3 (arg 4), then clear expiryHours = 0
    handle(
        &ctx,
        &mut sess,
        "bookmark",
        "UpdateBookmark",
        &[
            PyRep::Integer(bm_id),
            PyRep::String("Updated Ping".into()),
            PyRep::String("New Memo".into()),
            PyRep::Integer(shared_fid),
            PyRep::Integer(3),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(ctx.db.with_snapshot(|s| s.bookmark_expiries.contains_key(&bm_id)));

    handle(
        &ctx,
        &mut sess,
        "bookmark",
        "UpdateBookmark",
        &[
            PyRep::Integer(bm_id),
            PyRep::String("Permanent Ping".into()),
            PyRep::String("No Exp".into()),
            PyRep::Integer(shared_fid),
            PyRep::Integer(0),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(!ctx.db.with_snapshot(|s| s.bookmark_expiries.contains_key(&bm_id)));

    // Expire bookmark and run sweep_expired_bookmarks_and_notify
    ctx.db.with_mut_snapshot(|snap| {
        snap.bookmark_expiries.insert(bm_id, 100);
    });
    let swept = sweep_expired_bookmarks_and_notify(&ctx, 200);
    assert_eq!(swept, vec![bm_id]);

    // BookmarkScanResult validation: CantbookmarkEveSystem, NotInSameSystem, InvalidArgument, and SignatureNotFound
    sess.solar_system_id = Some(30_100_000);
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "scanMgr",
            "BookmarkScanResult",
            &[PyRep::String("SIG-1".into())],
            &kw
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "CantbookmarkEveSystem"
    );
    sess.solar_system_id = Some(sys_id);
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "scanMgr",
            "BookmarkScanResult",
            &[
                PyRep::Integer(30_002_187),
                PyRep::String("Amarr".into()),
                PyRep::String("Memo".into()),
                PyRep::String("SIG-1".into()),
            ],
            &kw
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "NotInSameSystem"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "scanMgr",
            "BookmarkScanResult",
            &[PyRep::Integer(sys_id as i64)],
            &kw
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidArgument"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "bookmark",
            "BookmarkScanResult",
            &[
                PyRep::Integer(sys_id as i64),
                PyRep::String("ABC-123".into()),
                PyRep::String("Relic".into()),
                PyRep::String("UNKNOWN-SIG".into()),
                PyRep::Integer(shared_fid),
            ],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "SignatureNotFound"
    );
}
