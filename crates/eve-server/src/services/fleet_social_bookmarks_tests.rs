//! Unit tests for `fleet_social_bookmarks` creation, retrieval, updates, and deletion.

#[cfg(test)]
#[path = "fleet_social_bookmarks_folders_tests.rs"]
mod fleet_social_bookmarks_folders_tests;

use super::*;
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::{
    clone_char_with, seed_multi_member_fleet, test_ctx_with_clock,
};
use eve_db::{corp_roles, BookmarkFolderRecord, UpwellStructureRecord};
use eve_machonet::SessionState;
use std::collections::HashMap;

#[test]
fn bookmark_helper_functions_check_shared_corp_and_admin_roles() {
    let ctx = test_ctx_with_clock(1_800_000_000);
    ctx.db.with_mut_snapshot(|snap| {
        snap.bookmark_folders.insert(
            810_001,
            BookmarkFolderRecord {
                folder_id: 810_001,
                owner_id: 90_000_002,
                folder_name: "Corp Shared".into(),
                is_shared: true,
                corporation_id: Some(98_000_001),
                ..Default::default()
            },
        );
        let npc_char = clone_char_with(snap, 90_000_010, "NPC Pilot", 1_000_035, 30_000_142, 100_000_010);
        snap.characters.insert(90_000_010, npc_char);
        let dir_char = clone_char_with(snap, 90_000_011, "Dir Pilot", 98_000_001, 30_000_142, 100_000_011);
        snap.characters.insert(90_000_011, dir_char);
        snap.corporations.entry(98_000_001).or_default().member_roles.insert(90_000_011, corp_roles::ROLE_DIRECTOR);

        let bm_folder = BookmarkRecord {
            bookmark_id: 1,
            owner_id: 90_000_011,
            label: "F".into(),
            solar_system_id: 30_000_142,
            x: 0.0,
            y: 0.0,
            z: 0.0,
            item_id: None,
            memo: String::new(),
            folder_id: Some(810_001),
            is_shared: true,
        };
        assert_eq!(shared_bookmark_corp_id(snap, &bm_folder), Some(98_000_001));

        let bm_unknown = BookmarkRecord {
            owner_id: 99_999_999,
            folder_id: None,
            ..bm_folder.clone()
        };
        assert_eq!(shared_bookmark_corp_id(snap, &bm_unknown), None);

        let mut sess = SessionState::new(1);
        sess.user_id = Some(90_000_011);
        sess.character_id = Some(90_000_011);
        sess.corporation_id = Some(98_000_001);
        sess.corp_role = corp_roles::ROLE_DIRECTOR;
        assert!(has_corp_bookmark_admin_role(snap, &sess, 90_000_011, 98_000_001));
        assert!(!has_corp_bookmark_admin_role(snap, &sess, 90_000_099, 98_000_001));
        let mut non_dir = sess.clone();
        non_dir.corp_role = 0;
        assert!(!has_corp_bookmark_admin_role(snap, &non_dir, 90_000_099, 98_000_999));
    });
}

#[tokio::test]
async fn bookmark_location_get_update_delete_and_expiry_sweep() {
    let ctx = test_ctx_with_clock(1_800_001_000);
    let (_fleet_id, mut session) = seed_multi_member_fleet(&ctx).await;
    let kw = HashMap::new();

    // Unauthenticated & missing label & EVE system errors
    let mut unauth = SessionState::new(2);
    assert_eq!(
        handle_bookmarks(&ctx, &mut unauth, "bookmark", "BookmarkLocation", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "Unauthorized"
    );
    assert_eq!(
        handle_bookmarks(
            &ctx,
            &mut session,
            "bookmark",
            "BookmarkLocation",
            &[PyRep::String("   ".into())],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidBookmarkLabel"
    );
    let mut eve_sess = session.clone();
    eve_sess.solar_system_id = Some(30_100_000);
    assert_eq!(
        handle_bookmarks(
            &ctx,
            &mut eve_sess,
            "bookmark",
            "BookmarkLocation",
            &[PyRep::String("Origin".into())],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "CantbookmarkEveSystem"
    );

    // Bookmark entity ball (100_000_002) via `bookmark` service with 2h expiry
    let bm1_res = handle_bookmarks(
        &ctx,
        &mut session,
        "bookmark",
        "BookmarkLocation",
        &[
            PyRep::String("Wingmate Spot".into()),
            PyRep::Integer(100_000_002),
            PyRep::String("on grid 15km".into()),
            PyRep::None,
            PyRep::None,
            PyRep::Integer(2),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(bm1_kv) = bm1_res else { panic!() };
    let bm1_id = bm1_kv.get("bookmarkID").unwrap().as_i64().unwrap();
    assert_eq!(bm1_kv.get("label").unwrap().as_str().unwrap(), "Wingmate Spot");
    assert!((bm1_kv.get("x").unwrap().as_f64().unwrap() - 15_000.0).abs() < 1e-6);

    // Bookmark current ship via `beyonce` service with itemID-first arg format & Upwell structure
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            1_000_000_123_456,
            UpwellStructureRecord {
                structure_id: 1_000_000_123_456,
                type_id: 35_832,
                owner_corp_id: 98_000_001,
                solar_system_id: 30_000_142,
                name: "Test Astrahus".into(),
                position: [50_000.0, 10_000.0, -25_000.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 500,
                shield_hp: 1_000_000.0,
                armor_hp: 1_000_000.0,
                hull_hp: 1_000_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
    });
    let bm2_res = handle_bookmarks(
        &ctx,
        &mut session,
        "beyonce",
        "BookmarkLocation",
        &[
            PyRep::Integer(1_000_000_123_456),
            PyRep::None,
            PyRep::String("Test Astrahus".into()),
            PyRep::String("fortizar dock".into()),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::Tuple(bm2_tup) = bm2_res else { panic!() };
    let bm2_id = bm2_tup[0].as_i64().unwrap();
    assert_eq!(bm2_tup[2].as_i64(), Some(35_832));
    assert_eq!(bm2_tup[3].as_f64(), Some(0.0));

    // GetBookmarks returns both active bookmarks
    let PyRep::Rowset { rows: rows_before, .. } =
        handle_bookmarks(&ctx, &mut session, "bookmark", "GetBookmarks", &[], &kw)
            .await
            .unwrap()
            .unwrap()
    else {
        panic!()
    };
    assert_eq!(rows_before.len(), 2);

    // UpdateBookmark renames bm2 and updates memo
    let upd_res = handle_bookmarks(
        &ctx,
        &mut session,
        "bookmark",
        "UpdateBookmark",
        &[
            PyRep::Integer(bm2_id),
            PyRep::String("Renamed Astrahus".into()),
            PyRep::String("updated note".into()),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(upd_kv) = upd_res else { panic!() };
    assert_eq!(upd_kv.get("updated").unwrap(), &PyRep::Bool(true));
    assert_eq!(
        ctx.db.with_snapshot(|s| s.bookmarks[&bm2_id].label.clone()),
        "Renamed Astrahus"
    );

    // Non-owner cannot UpdateBookmark on private bookmark
    let mut other_sess = SessionState::new(3);
    other_sess.user_id = Some(90_000_004);
    other_sess.character_id = Some(90_000_004);
    other_sess.corporation_id = Some(98_000_001);
    assert_eq!(
        handle_bookmarks(
            &ctx,
            &mut other_sess,
            "bookmark",
            "UpdateBookmark",
            &[PyRep::Integer(bm2_id), PyRep::String("Hack".into())],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "AccessDenied"
    );

    // Advance clock past 2h expiry (7201s) -> GetBookmarks sweeps expired bm1 while keeping bm2
    ctx.advance_secs(7_201);
    let PyRep::Rowset { rows: rows_after_exp, .. } =
        handle_bookmarks(&ctx, &mut session, "bookmark", "GetBookmarks", &[], &kw)
            .await
            .unwrap()
            .unwrap()
    else {
        panic!()
    };
    assert_eq!(rows_after_exp.len(), 1);
    assert!(ctx.db.with_snapshot(|s| !s.bookmarks.contains_key(&bm1_id)));

    // DeleteBookmarks removes bm2
    let del_res = handle_bookmarks(
        &ctx,
        &mut session,
        "bookmark",
        "DeleteBookmarks",
        &[PyRep::List(vec![PyRep::Integer(bm2_id)])],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(del_kv) = del_res else { panic!() };
    assert_eq!(del_kv.get("deleted").unwrap(), &PyRep::Bool(true));
    assert!(ctx.db.with_snapshot(|s| !s.bookmarks.contains_key(&bm2_id)));
}

#[tokio::test]
async fn active_bookmarks_and_get_bookmarks_populate_v24_creator_owner_dates_and_type_fields() {
    let ctx = test_ctx_with_clock(1_800_000_000);
    let char_id = 90_000_717_i64;
    let corp_id = 98_000_001_i64;
    let sys_id = 30_000_142_u32;
    let ship_id = 100_000_717_i64;
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = clone_char_with(snap, char_id, "BookmarkV24Pilot", corp_id, sys_id, ship_id);
        ch.station_id = None;
        ch.structure_id = None;
        snap.characters.insert(char_id, ch);
        snap.items.insert(
            ship_id,
            eve_db::InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: eve_db::InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    sys_h
        .spawn_ball(eve_destiny::Ball {
            id: ship_id,
            type_id: 587,
            position: glam::DVec3::new(1000.0, 2000.0, 3000.0),
            velocity: glam::DVec3::ZERO,
            max_velocity: 350.0,
            inertia: 2.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await
        .unwrap();

    let mut session = SessionState::new(1);
    session.user_id = Some(char_id as u64);
    session.character_id = Some(char_id);
    session.corporation_id = Some(corp_id);
    session.solar_system_id = Some(sys_id);
    session.ship_id = Some(ship_id);

    // Create one timed bookmark (3 hours) and one permanent bookmark
    handle_bookmarks(
        &ctx,
        &mut session,
        "bookmark",
        "BookmarkLocation",
        &[
            PyRep::Integer(sys_id as i64),
            PyRep::None,
            PyRep::String("Timed Spot".into()),
            PyRep::String("check creatorID".into()),
            PyRep::None,
            PyRep::Integer(3),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();

    for (svc, method) in [
        ("bookmark", "GetMyActiveBookmarks"),
        ("accessGroupBookmarkMgr", "GetMyActiveBookmarks"),
    ] {
        let rep = if svc == "bookmark" {
            handle_bookmarks(&ctx, &mut session, svc, method, &[], &kw)
                .await
                .unwrap()
                .unwrap()
        } else {
            crate::services::fleet_social::fleet_social_access_group_bookmarks::handle(
                &ctx, &mut session, svc, method, &[], &kw,
            )
            .await
            .unwrap()
            .unwrap()
        };
        let PyRep::Tuple(parts) = rep else {
            panic!("{svc}.{method}: expected 3-tuple, got {rep:?}");
        };
        let PyRep::List(bms) = &parts[1] else {
            panic!("{svc}.{method}: expected bookmarks list");
        };
        assert_eq!(bms.len(), 1);
        let bm = &bms[0];
        assert_eq!(bm.dict_get("creatorID"), Some(&PyRep::Integer(char_id)));
        assert_eq!(bm.dict_get("ownerID"), Some(&PyRep::Integer(char_id)));
        assert_eq!(bm.dict_get("locationID"), Some(&PyRep::Integer(sys_id as i64)));
        assert_eq!(bm.dict_get("typeID"), Some(&PyRep::Integer(5)));
        assert_eq!(bm.dict_get("note"), Some(&PyRep::String("check creatorID".into())));
        assert_eq!(bm.dict_get("flag"), Some(&PyRep::Integer(0)));
        assert!(bm.dict_get("created").and_then(|v| v.as_i64()).unwrap_or(0) >= 116_444_736_000_000_000);
        assert!(bm.dict_get("expiry").and_then(|v| v.as_i64()).unwrap_or(0) > 116_444_736_000_000_000);
    }

    let PyRep::Rowset { columns, rows } =
        handle_bookmarks(&ctx, &mut session, "bookmark", "GetBookmarks", &[], &kw)
            .await
            .unwrap()
            .unwrap()
    else {
        panic!("expected Rowset from bookmark.GetBookmarks");
    };
    assert_eq!(rows.len(), 1);
    for req_col in ["creatorID", "ownerID", "created", "expiry", "typeID", "note", "flag", "locationID"] {
        assert!(
            columns.iter().any(|c| c == req_col),
            "GetBookmarks rowset missing column {req_col}: {columns:?}"
        );
    }
}
