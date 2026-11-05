use super::*;
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::test_ctx_and_clock;
use eve_db::BookmarkFolderRecord;

fn make_session(char_id: i64, corp_id: i64) -> SessionState {
    SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        corporation_id: Some(corp_id),
        solar_system_id: Some(30_000_142),
        corp_role: u64::MAX,
        ..Default::default()
    }
}

#[tokio::test]
async fn bookmark_static_location_and_active_bookmarks_ops() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let char_id = 100_000_001;
    let corp_id = 1_000_009;
    let session = make_session(char_id, corp_id);
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|snap| {
        snap.bookmark_folders.insert(
            101,
            BookmarkFolderRecord {
                folder_id: 101,
                owner_id: char_id,
                folder_name: "personal folder".into(),
                is_shared: false,
                ..Default::default()
            },
        );
        snap.bookmark_folders.insert(
            201,
            BookmarkFolderRecord {
                folder_id: 201,
                owner_id: char_id,
                folder_name: "shared folder".into(),
                is_shared: true,
                corporation_id: Some(corp_id),
                ..Default::default()
            },
        );
        snap.bookmark_folder_known_by.insert((201, char_id), true);
    });

    let created = handle_bookmark_static_location(
        &ctx,
        &session,
        &[
            PyRep::Integer(30_000_142),
            PyRep::Integer(101),
            PyRep::String("jita sun".into()),
            PyRep::String("memo text".into()),
            PyRep::Integer(24),
        ],
        &kw,
    )
    .await
    .unwrap();
    let bm_id = match &created {
        PyRep::Tuple(items) => items[0].as_i64().unwrap(),
        _ => panic!("expected 8-tuple"),
    };

    let active = handle_get_my_active_bookmarks(&ctx, &session, &[]).await.unwrap();
    match active {
        PyRep::Tuple(parts) => {
            assert_eq!(parts.len(), 3);
            assert!(matches!(&parts[1], PyRep::List(bms) if !bms.is_empty()));
        }
        _ => panic!("expected 3-tuple"),
    }

    let updated = handle_update_bookmark(
        &ctx,
        &session,
        &[
            PyRep::Integer(bm_id),
            PyRep::String("updated label".into()),
            PyRep::String("updated memo".into()),
            PyRep::Integer(201),
            PyRep::Integer(48),
        ],
    )
    .await
    .unwrap();
    assert_eq!(updated.dict_get("updated").and_then(|v| v.as_bool()), Some(true));

    let copied = handle_copy_move_bookmarks(
        &ctx,
        &session,
        &[
            PyRep::Integer(101),
            PyRep::None,
            PyRep::List(vec![PyRep::Integer(bm_id)]),
        ],
        false,
    )
    .await
    .unwrap();
    assert!(matches!(copied, PyRep::Tuple(_)));

    let moved = handle_copy_move_bookmarks(
        &ctx,
        &session,
        &[
            PyRep::Integer(101),
            PyRep::None,
            PyRep::List(vec![PyRep::Integer(bm_id)]),
        ],
        true,
    )
    .await
    .unwrap();
    assert!(matches!(moved, PyRep::Tuple(_)));

    let deleted = handle_delete_bookmarks(
        &ctx,
        &session,
        &[PyRep::List(vec![PyRep::Integer(bm_id)]), PyRep::Integer(101)],
    )
    .await
    .unwrap();
    assert_eq!(deleted, PyRep::List(vec![PyRep::Integer(bm_id)]));
}
