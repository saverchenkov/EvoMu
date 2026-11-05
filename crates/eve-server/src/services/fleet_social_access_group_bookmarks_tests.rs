use super::*;
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::test_ctx_and_clock;

fn make_session(char_id: i64, corp_id: i64) -> SessionState {
    SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        corporation_id: Some(corp_id),
        alliance_id: Some(99_000_001),
        solar_system_id: Some(30_000_142),
        corp_role: u64::MAX,
        ..Default::default()
    }
}

#[test]
fn access_group_and_folder_access_level_logic() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let session = make_session(100_000_002, 1_000_009);
    ctx.db.with_mut_snapshot(|snap| {
        snap.access_groups.insert(5001, vec![100_000_002]);
        snap.access_groups.insert(5002, vec![1_000_009]);
        snap.access_groups.insert(5003, vec![99_000_001]);
    });

    ctx.db.with_snapshot(|snap| {
        assert!(is_in_access_group(snap, &session, 100_000_002, 5001));
        assert!(is_in_access_group(snap, &session, 100_000_002, 5002));
        assert!(is_in_access_group(snap, &session, 100_000_002, 5003));
        assert!(!is_in_access_group(snap, &session, 100_000_002, 9999));

        let personal = BookmarkFolderRecord {
            folder_id: 1,
            owner_id: 100_000_001,
            folder_name: "p".into(),
            is_shared: false,
            ..Default::default()
        };
        assert_eq!(folder_access_level(snap, &session, 100_000_001, &personal), AccessLevel::Admin);
        assert_eq!(folder_access_level(snap, &session, 100_000_002, &personal), AccessLevel::None);

        let shared_grp = BookmarkFolderRecord {
            folder_id: 2,
            owner_id: 100_000_001,
            folder_name: "s".into(),
            is_shared: true,
            manage_group_id: Some(5001),
            view_group_id: Some(5002),
            ..Default::default()
        };
        assert_eq!(folder_access_level(snap, &session, 100_000_002, &shared_grp), AccessLevel::Manage);
    });
}

#[tokio::test]
async fn folder_subfolder_and_known_folders_rpc_lifecycle() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let mut session = make_session(100_000_001, 1_000_009);
    let kw = HashMap::new();
    let svc = "accessGroupBookmarkMgr";

    assert!(handle(&ctx, &mut session, "other_svc", "AddFolder", &[], &kw).await.is_none());

    let added = handle(
        &ctx,
        &mut session,
        svc,
        "AddFolder",
        &[
            PyRep::Bool(true),
            PyRep::String("shared tactical".into()),
            PyRep::String("corp bookmarks".into()),
            PyRep::None,
            PyRep::None,
            PyRep::None,
            PyRep::None,
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let fid = added.dict_get("folderID").and_then(|v| v.as_i64()).unwrap();

    let updated = handle(
        &ctx,
        &mut session,
        svc,
        "UpdateFolder",
        &[
            PyRep::Integer(fid),
            PyRep::String("shared tactical v2".into()),
            PyRep::String("updated desc".into()),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(updated.dict_get("folderID").and_then(|v| v.as_i64()), Some(fid));

    let sub = handle(
        &ctx,
        &mut session,
        svc,
        "CreateSubfolder",
        &[PyRep::Integer(fid), PyRep::String("pings".into())],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let sfid = sub.dict_get("subfolderID").and_then(|v| v.as_i64()).unwrap();

    let sub_upd = handle(
        &ctx,
        &mut session,
        svc,
        "UpdateSubfolder",
        &[PyRep::Integer(fid), PyRep::Integer(sfid), PyRep::String("gate pings".into())],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(sub_upd.dict_get("subfolderID").and_then(|v| v.as_i64()), Some(sfid));

    let known = handle(
        &ctx,
        &mut session,
        svc,
        "AddToKnownFolders",
        &[PyRep::Integer(fid), PyRep::Bool(true)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(known, PyRep::Tuple(_)));

    let state_upd = handle(
        &ctx,
        &mut session,
        svc,
        "UpdateKnownFolderState",
        &[PyRep::Integer(fid), PyRep::Bool(false)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(state_upd, PyRep::Tuple(_)));

    let info = handle(
        &ctx,
        &mut session,
        svc,
        "GetFolderInfo",
        &[PyRep::Integer(fid)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(info.dict_get("folderID").and_then(|v| v.as_i64()), Some(fid));

    let search = handle(
        &ctx,
        &mut session,
        svc,
        "SearchFoldersWithAdminAccess",
        &[PyRep::String("tactical".into())],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(!matches!(search, PyRep::None));

    let rem_known = handle(
        &ctx,
        &mut session,
        svc,
        "RemoveFromKnownFolders",
        &[PyRep::Integer(fid)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(rem_known.dict_get("subscribed").and_then(|v| v.as_bool()), Some(false));

    let del_sub = handle(
        &ctx,
        &mut session,
        svc,
        "DeleteSubfolder",
        &[PyRep::Integer(fid), PyRep::Integer(sfid)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(del_sub, PyRep::List(_)));

    let del_folder = handle(
        &ctx,
        &mut session,
        svc,
        "DeleteFolder",
        &[PyRep::Integer(fid)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(del_folder.dict_get("deleted").and_then(|v| v.as_bool()), Some(true));
}
