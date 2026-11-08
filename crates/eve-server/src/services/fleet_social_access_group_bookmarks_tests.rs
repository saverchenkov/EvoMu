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

#[test]
fn folder_access_level_enforces_owner_groups_exclusions_and_strips_shared_owner_bypass() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let corp_a = 98_910_i64;
    let corp_b = 98_911_i64;
    let creator_id = 100_000_050_i64;
    let blocked_member = 100_000_051_i64;
    let ally_gid = 80_900_001_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            corp_a,
            eve_db::CorporationRecord {
                corporation_id: corp_a,
                ceo_id: blocked_member,
                ..Default::default()
            },
        );
        let template = snap.characters.values().next().cloned().unwrap();
        let mut c1 = template.clone();
        c1.character_id = creator_id;
        c1.corporation_id = corp_a;
        c1.alliance_id = None;
        snap.characters.insert(creator_id, c1);
        let mut c2 = template;
        c2.character_id = blocked_member;
        c2.corporation_id = corp_a;
        c2.alliance_id = None;
        snap.characters.insert(blocked_member, c2);
        let mut ext = crate::services::station_structure::structure_upwell_ops::load_ext_state(snap);
        ext.owner_groups.insert(
            ally_gid,
            crate::services::station_structure::structure_upwell_ops::AccessGroupRecord {
                group_id: ally_gid,
                creator_id,
                owner_corp_id: corp_a,
                name: "Bookmark ACL".into(),
                description: String::new(),
                members: [
                    (corp_a, crate::services::structure_acl_defense_rules::ACL_ROLE_MEMBER),
                    (
                        blocked_member,
                        crate::services::structure_acl_defense_rules::ACL_ROLE_EXCLUDED,
                    ),
                ]
                .into_iter()
                .collect(),
            },
        );
        crate::services::station_structure::structure_upwell_ops::save_ext_state(snap, &ext);
    });

    let acl_folder = BookmarkFolderRecord {
        folder_id: 99_001,
        folder_name: "ACL Shared".into(),
        owner_id: creator_id,
        corporation_id: Some(corp_a),
        is_shared: true,
        admin_group_id: Some(ally_gid),
        ..Default::default()
    };
    let legacy_corp_folder = BookmarkFolderRecord {
        folder_id: 99_002,
        folder_name: "Corp Shared".into(),
        owner_id: creator_id,
        corporation_id: Some(corp_a),
        is_shared: true,
        ..Default::default()
    };

    let mut creator_sess = SessionState::new(50);
    creator_sess.character_id = Some(creator_id);
    creator_sess.corporation_id = Some(corp_a);

    let mut blocked_sess = SessionState::new(51);
    blocked_sess.character_id = Some(blocked_member);
    blocked_sess.corporation_id = Some(corp_a);

    ctx.db.with_snapshot(|snap| {
        // Blocked character in owner_groups (ACL_ROLE_EXCLUDED = 3) must be denied even though corp_a is a member!
        assert!(!is_in_access_group(snap, &blocked_sess, blocked_member, ally_gid));
        assert_eq!(
            folder_access_level(snap, &blocked_sess, blocked_member, &acl_folder),
            AccessLevel::None
        );
        // Active corp creator has Admin while in corp_a
        assert_eq!(
            folder_access_level(snap, &creator_sess, creator_id, &acl_folder),
            AccessLevel::Admin
        );
    });

    // Ex-director leaves corp_a -> corp_b: must lose AccessLevel on BOTH ACL shared folder & legacy corp folder!
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&creator_id).unwrap().corporation_id = corp_b;
    });
    creator_sess.corporation_id = Some(corp_b);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            folder_access_level(snap, &creator_sess, creator_id, &acl_folder),
            AccessLevel::None
        );
        assert_eq!(
            folder_access_level(snap, &creator_sess, creator_id, &legacy_corp_folder),
            AccessLevel::None
        );
    });
}
