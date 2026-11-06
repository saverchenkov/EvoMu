//! Deep branch-coverage and mutation-killing unit tests for `fleet_social_access_group_bookmarks_ops`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::fleet_social::fleet_social_access_group_bookmarks_ops::{
    acl_active_bookmarks, handle_bookmark_static_location, handle_copy_move_bookmarks,
    handle_delete_bookmarks, handle_get_my_active_bookmarks, handle_update_bookmark,
};
use eve_db::{
    BookmarkFolderRecord, BookmarkRecord, InventoryFlag, InventoryItemRecord,
    UpwellStructureRecord, WorldRepository,
};
use eve_destiny::{Ball, CombatState, DVec3, MovementMode, SlimItemMeta};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn new_ctx() -> ServerContext {
    ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    )
}

fn make_sess(
    char_id: i64,
    corp_id: i64,
    sys_id: Option<u32>,
    station_id: Option<i64>,
    structure_id: Option<i64>,
    ship_id: Option<i64>,
) -> SessionState {
    SessionState {
        user_id: Some(char_id as u64),
        character_id: Some(char_id),
        corporation_id: Some(corp_id),
        solar_system_id: sys_id,
        station_id,
        structure_id,
        ship_id,
        corp_role: 0,
        ..Default::default()
    }
}

fn seed_access_group(ctx: &ServerContext, group_id: i64, members: &[i64]) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.access_groups.insert(group_id, members.to_vec());
    });
}

#[tokio::test]
async fn acl_active_bookmarks_and_static_location_all_entity_and_error_branches() {
    let ctx = new_ctx();
    let sys_id = 30_000_142u32;
    let char_id = 97_300_001i64;
    let other_char = 97_300_002i64;
    let corp_id = 1_000_009i64;
    let ship_id = 97_300_100i64;
    let struct_id = 1_040_300_001_000i64;
    let kw = HashMap::new();

    // 1. NoSolarSystem and Zarzakh (30_100_000) rejection
    let no_sys_sess = make_sess(char_id, corp_id, None, None, None, None);
    assert_eq!(
        handle_bookmark_static_location(&ctx, &no_sys_sess, &[], &kw).await.unwrap_err().code,
        "NoSolarSystem"
    );
    let zarzakh_sess = make_sess(char_id, corp_id, Some(30_100_000), None, None, None);
    assert_eq!(
        handle_bookmark_static_location(&ctx, &zarzakh_sess, &[], &kw).await.unwrap_err().code,
        "CantbookmarkEveSystem"
    );

    // 2. InvalidBookmarkLabel (empty / whitespace / > 100 chars)
    let sess = make_sess(char_id, corp_id, Some(sys_id), None, None, Some(ship_id));
    assert_eq!(
        handle_bookmark_static_location(&ctx, &sess, &[PyRep::String("   ".into())], &kw)
            .await
            .unwrap_err()
            .code,
        "InvalidBookmarkLabel"
    );
    let long_lbl = "x".repeat(101);
    assert_eq!(
        handle_bookmark_static_location(&ctx, &sess, &[PyRep::String(long_lbl)], &kw)
            .await
            .unwrap_err()
            .code,
        "InvalidBookmarkLabel"
    );

    // 3. Folder access check failure on BookmarkStaticLocation
    seed_access_group(&ctx, 501, &[char_id]);
    ctx.db.with_mut_snapshot(|snap| {
        snap.bookmark_folders.insert(
            301,
            BookmarkFolderRecord {
                folder_id: 301,
                owner_id: other_char,
                folder_name: "ViewOnlyFolder".into(),
                is_shared: true,
                view_group_id: Some(501),
                ..Default::default()
            },
        );
        snap.bookmark_folders.insert(
            302,
            BookmarkFolderRecord {
                folder_id: 302,
                owner_id: char_id,
                folder_name: "ParentFolder".into(),
                is_shared: true,
                corporation_id: Some(corp_id),
                ..Default::default()
            },
        );
        // Subfolders with different naming patterns for acl_active_bookmarks
        snap.bookmark_folders.insert(
            310,
            BookmarkFolderRecord {
                folder_id: 310,
                owner_id: char_id,
                folder_name: "ParentFolder/StrippedSub".into(),
                is_shared: true,
                ..Default::default()
            },
        );
        snap.bookmark_folders.insert(
            311,
            BookmarkFolderRecord {
                folder_id: 311,
                owner_id: char_id,
                folder_name: "OtherPrefix/SplitSub".into(),
                is_shared: true,
                ..Default::default()
            },
        );
        snap.bookmark_folders.insert(
            312,
            BookmarkFolderRecord {
                folder_id: 312,
                owner_id: char_id,
                folder_name: "Orphan/RsplitSub".into(),
                is_shared: true,
                ..Default::default()
            },
        );
        snap.bookmark_subfolder_parents.insert(310, 302);
        snap.bookmark_subfolder_parents.insert(311, 302);
        snap.bookmark_folder_known_by.insert((302, char_id), true);
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35832,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "Jita Astrahus".into(),
                position: [100.0, 200.0, 300.0],
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    assert_eq!(
        handle_bookmark_static_location(
            &ctx,
            &sess,
            &[
                PyRep::String("DeniedFolderBm".into()),
                PyRep::Integer(sys_id as i64),
                PyRep::String("memo".into()),
                PyRep::Integer(301),
            ],
            &kw,
        )
        .await
        .unwrap_err()
        .code,
        "UserError"
    );

    // 4. acl_active_bookmarks subfolder formatting (parent prefix strip, split_once, orphan rsplit, and missing folder fallback)
    let raw_rows = vec![vec![
        PyRep::Integer(1),
        PyRep::String("bm".into()),
        PyRep::Integer(sys_id as i64),
        PyRep::Real(0.0),
        PyRep::Real(0.0),
        PyRep::Real(0.0),
        PyRep::None,
        PyRep::String(String::new()),
        PyRep::Integer(302),
        PyRep::Bool(true),
        PyRep::Integer(312),
    ], vec![
        PyRep::Integer(2),
        PyRep::String("bm2".into()),
        PyRep::Integer(sys_id as i64),
        PyRep::Real(0.0),
        PyRep::Real(0.0),
        PyRep::Real(0.0),
        PyRep::None,
        PyRep::String(String::new()),
        PyRep::Integer(302),
        PyRep::Bool(true),
        PyRep::Integer(999), // missing from bookmark_folders -> "Subfolder 999"
    ]];
    let cols = [
        "bookmarkID", "label", "solarSystemID", "x", "y", "z", "itemID", "memo", "folderID", "isShared", "subfolderID",
    ];
    let acl_rep = acl_active_bookmarks(&ctx, &sess, char_id, &cols, raw_rows);
    let PyRep::Tuple(parts) = acl_rep else { panic!("expected tuple") };
    let PyRep::List(subfolders) = &parts[2] else { panic!("expected list") };
    assert_eq!(subfolders.len(), 4);

    // 5. Entity branches: station, jump gate, celestial, structure, active ship (absent vs present), dynamic entity (cloaked, off-grid, on-grid)
    let st_id = ctx.sde.solar_systems[&sys_id].stations[0].station_id;
    let jump_id = *ctx
        .sde
        .jumps
        .iter()
        .find(|(_, j)| j.from_solar_system_id == sys_id)
        .map(|(id, _)| id)
        .unwrap();
    let cel_id = ctx.sde.celestials_in_system(sys_id)[0].celestial_id;

    for ent in [st_id, jump_id, cel_id, struct_id] {
        let res = handle_bookmark_static_location(
            &ctx,
            &sess,
            &[PyRep::String("EntBm".into()), PyRep::Integer(ent), PyRep::String("m".into()), PyRep::Integer(302), PyRep::Integer(310), PyRep::Integer(12)],
            &kw,
        )
        .await
        .expect("bookmark entity");
        assert!(matches!(res, PyRep::Tuple(t) if t.len() == 8));
    }

    // Active ship absent from ballpark -> ShipNotInSpace
    assert_eq!(
        handle_bookmark_static_location(
            &ctx,
            &sess,
            &[PyRep::Integer(ship_id), PyRep::Integer(0), PyRep::String("ShipBm".into())],
            &kw,
        )
        .await
        .unwrap_err()
        .code,
        "ShipNotInSpace"
    );
    assert_eq!(
        handle_bookmark_static_location(&ctx, &sess, &[PyRep::None, PyRep::Integer(0), PyRep::String("Spot".into())], &kw)
            .await
            .unwrap_err()
            .code,
        "ShipNotInSpace"
    );

    // Station and Structure implicit bookmarking (when explicit_entity_id is None)
    let st_sess = make_sess(char_id, corp_id, Some(sys_id), Some(st_id), None, None);
    assert!(handle_bookmark_static_location(&ctx, &st_sess, &[PyRep::None, PyRep::Integer(0), PyRep::String("InStation".into())], &kw).await.is_ok());
    let str_sess = make_sess(char_id, corp_id, Some(sys_id), None, Some(struct_id), None);
    assert!(handle_bookmark_static_location(&ctx, &str_sess, &[PyRep::None, PyRep::Integer(0), PyRep::String("InStruct".into())], &kw).await.is_ok());

    // Spawn caller ship and test dynamic entity branches (cloaked, off-grid, on-grid)
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let spawn = |id: i64, pos: DVec3, cloaked: bool| {
        let sys_h = sys_h.clone();
        async move {
            let ball = Ball {
                id,
                type_id: 587,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1.0e6,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            };
            let mut cs = CombatState::default();
            cs.is_cloaked = cloaked;
            let _ = sys_h.spawn_ball_with_meta(ball, cs, SlimItemMeta::default()).await;
        }
    };
    spawn(ship_id, DVec3::ZERO, false).await;
    spawn(97_300_501, DVec3::new(1_000.0, 0.0, 0.0), true).await;
    spawn(97_300_502, DVec3::new(9_000_000.0, 0.0, 0.0), false).await;
    spawn(97_300_503, DVec3::new(5_000.0, 0.0, 0.0), false).await;

    // Cloaked dynamic entity rejected
    assert_eq!(
        handle_bookmark_static_location(&ctx, &sess, &[PyRep::Integer(97_300_501), PyRep::Integer(0), PyRep::String("Cloaked".into())], &kw)
            .await.unwrap_err().code,
        "UserError"
    );
    // Off-grid (> 8,000 km) dynamic entity rejected
    assert_eq!(
        handle_bookmark_static_location(&ctx, &sess, &[PyRep::Integer(97_300_502), PyRep::Integer(0), PyRep::String("OffGrid".into())], &kw)
            .await.unwrap_err().code,
        "UserError"
    );
    // On-grid dynamic entity succeeds
    assert!(
        handle_bookmark_static_location(&ctx, &sess, &[PyRep::Integer(97_300_503), PyRep::Integer(0), PyRep::String("grid spot".into())], &kw)
            .await.is_ok()
    );

    // Subfolder validation errors: subfolder without parent folder, subfolder with wrong parent folder
    let mut bad_kw = HashMap::new();
    bad_kw.insert("subfolderID".to_string(), PyRep::Integer(310));
    assert_eq!(
        handle_bookmark_static_location(&ctx, &sess, &[PyRep::Integer(ship_id), PyRep::None, PyRep::String("NoFolder".into())], &bad_kw)
            .await.unwrap_err().code,
        "UserError"
    );
    assert_eq!(
        handle_bookmark_static_location(&ctx, &sess, &[PyRep::Integer(ship_id), PyRep::Integer(302), PyRep::String("WrongParent".into()), PyRep::String("".into()), PyRep::Integer(0), PyRep::Integer(999)], &kw)
            .await.unwrap_err().code,
        "UserError"
    );
}

#[tokio::test]
async fn update_delete_copy_and_move_bookmarks_limits_and_access_branches() {
    let ctx = new_ctx();
    let sys_id = 30_000_142u32;
    let char_id = 97_300_010i64;
    let other_char = 97_300_011i64;
    let corp_id = 1_000_009i64;
    let sess = make_sess(char_id, corp_id, Some(sys_id), Some(60_003_760), None, None);

    seed_access_group(&ctx, 601, &[char_id]);
    ctx.db.with_mut_snapshot(|snap| {
        snap.bookmark_folders.insert(
            401,
            BookmarkFolderRecord {
                folder_id: 401,
                owner_id: other_char,
                folder_name: "OtherViewOnly".into(),
                is_shared: true,
                view_group_id: Some(601),
                ..Default::default()
            },
        );
        snap.bookmark_folders.insert(
            402,
            BookmarkFolderRecord {
                folder_id: 402,
                owner_id: char_id,
                folder_name: "MyShared".into(),
                is_shared: true,
                corporation_id: Some(corp_id),
                ..Default::default()
            },
        );
        snap.bookmark_subfolder_parents.insert(450, 402);
        snap.bookmarks.insert(
            1001,
            BookmarkRecord {
                bookmark_id: 1001,
                owner_id: other_char,
                label: "OtherBm".into(),
                solar_system_id: sys_id,
                x: 1.0,
                y: 2.0,
                z: 3.0,
                item_id: None,
                memo: "m".into(),
                folder_id: Some(401),
                is_shared: true,
            },
        );
        snap.bookmarks.insert(
            1002,
            BookmarkRecord {
                bookmark_id: 1002,
                owner_id: char_id,
                label: "MyBm".into(),
                solar_system_id: sys_id,
                x: 1.0,
                y: 2.0,
                z: 3.0,
                item_id: None,
                memo: "m".into(),
                folder_id: Some(402),
                is_shared: true,
            },
        );
    });

    // 1. UpdateBookmark error & 7-arg branches
    assert_eq!(handle_update_bookmark(&ctx, &sess, &[]).await.unwrap_err().code, "InvalidArgument");
    assert_eq!(
        handle_update_bookmark(&ctx, &sess, &[PyRep::Integer(999_999)]).await.unwrap_err().code,
        "UserError"
    );
    assert_eq!(
        handle_update_bookmark(&ctx, &sess, &[PyRep::Integer(1001), PyRep::String("x".into())])
            .await.unwrap_err().code,
        "AccessDenied"
    );
    // Destination folder not found & destination folder without Use access
    assert_eq!(
        handle_update_bookmark(&ctx, &sess, &[PyRep::Integer(1002), PyRep::String("x".into()), PyRep::String("m".into()), PyRep::Integer(9999)])
            .await.unwrap_err().code,
        "UserError"
    );
    assert_eq!(
        handle_update_bookmark(&ctx, &sess, &[PyRep::Integer(1002), PyRep::String("x".into()), PyRep::String("m".into()), PyRep::Integer(401)])
            .await.unwrap_err().code,
        "UserError"
    );
    // 7-arg UpdateBookmark with valid subfolder & expiry clearing (0 hours)
    let upd7 = handle_update_bookmark(
        &ctx,
        &sess,
        &[
            PyRep::Integer(1002),
            PyRep::Integer(char_id),
            PyRep::Integer(402),
            PyRep::Integer(450),
            PyRep::String("new memo".into()),
            PyRep::String("new label".into()),
            PyRep::Integer(0),
        ],
    )
    .await
    .unwrap();
    assert_eq!(upd7.dict_get("expiryUnix"), Some(&PyRep::Integer(0)));
    // Invalid subfolder in 7-arg UpdateBookmark
    assert_eq!(
        handle_update_bookmark(
            &ctx,
            &sess,
            &[
                PyRep::Integer(1002),
                PyRep::Integer(char_id),
                PyRep::Integer(402),
                PyRep::Integer(999),
                PyRep::String("m".into()),
                PyRep::String("l".into()),
                PyRep::Integer(0),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "UserError"
    );

    // 2. DeleteBookmarks error & success branches
    assert_eq!(handle_delete_bookmarks(&ctx, &sess, &[]).await.unwrap_err().code, "InvalidArgument");
    assert_eq!(
        handle_delete_bookmarks(&ctx, &sess, &[PyRep::List(vec![PyRep::Integer(999_999)])])
            .await.unwrap_err().code,
        "UserError"
    );
    assert_eq!(
        handle_delete_bookmarks(&ctx, &sess, &[PyRep::List(vec![PyRep::Integer(1002)]), PyRep::Integer(401)])
            .await.unwrap_err().code,
        "UserError"
    );
    assert_eq!(
        handle_delete_bookmarks(&ctx, &sess, &[PyRep::List(vec![PyRep::Integer(1002)]), PyRep::Integer(0)])
            .await.unwrap_err().code,
        "UserError"
    );
    assert_eq!(
        handle_delete_bookmarks(&ctx, &sess, &[PyRep::List(vec![PyRep::Integer(1001)])])
            .await.unwrap_err().code,
        "AccessDenied"
    );

    // 3. Copy/Move batch limit (>20), argument check, access check, and 500-bookmark cap
    assert_eq!(
        handle_copy_move_bookmarks(&ctx, &sess, &[PyRep::Integer(402)], false).await.unwrap_err().code,
        "InvalidArgument"
    );
    let twenty_one: Vec<PyRep> = (1..=21).map(PyRep::Integer).collect();
    assert_eq!(
        handle_copy_move_bookmarks(&ctx, &sess, &[PyRep::Integer(402), PyRep::None, PyRep::List(twenty_one)], false)
            .await.unwrap_err().code,
        "BookmarkCopyBatchLimitExceeded"
    );
    // View-only bookmark 1001 can be copied to 402, but cannot be moved!
    assert!(
        handle_copy_move_bookmarks(
            &ctx,
            &sess,
            &[PyRep::Integer(402), PyRep::Integer(450), PyRep::List(vec![PyRep::Integer(1001)])],
            false,
        )
        .await
        .is_ok()
    );
    assert_eq!(
        handle_copy_move_bookmarks(
            &ctx,
            &sess,
            &[PyRep::Integer(402), PyRep::Integer(450), PyRep::List(vec![PyRep::Integer(1001)])],
            true,
        )
        .await
        .unwrap_err()
        .code,
        "UserError"
    );

    // Seed 500 bookmarks in folder 402 to verify MaxSharedFolderBookmarksReached
    ctx.db.with_mut_snapshot(|snap| {
        for idx in 0..500 {
            let id = 50_000 + idx;
            snap.bookmarks.insert(
                id,
                BookmarkRecord {
                    bookmark_id: id,
                    owner_id: char_id,
                    label: "Cap".into(),
                    solar_system_id: sys_id,
                    x: 0.0,
                    y: 0.0,
                    z: 0.0,
                    item_id: None,
                    memo: String::new(),
                    folder_id: Some(402),
                    is_shared: true,
                },
            );
        }
    });
    assert_eq!(
        handle_copy_move_bookmarks(
            &ctx,
            &sess,
            &[PyRep::Integer(402), PyRep::None, PyRep::List(vec![PyRep::Integer(1001)])],
            false,
        )
        .await
        .unwrap_err()
        .code,
        "MaxSharedFolderBookmarksReached"
    );
    let _ = handle_get_my_active_bookmarks(&ctx, &sess, &[]).await.unwrap();
}
