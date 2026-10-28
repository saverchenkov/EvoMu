#![allow(unused_imports)]
//! Track 4: Phase 8 Bookmarks (`SO-4`), Scanning & Dungeons (`SO-5`), and Wormholes (`SO-6`) Integration Tests.

use crate::common;

use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

const AU: f64 = 149_597_870_700.0;

pub(crate) fn is_keyval(rep: &PyRep) -> bool {
    matches!(rep, PyRep::KeyVal(_))
}

#[tokio::test]
async fn test_so4_bookmarks_folders_shared_folders_and_scan_bookmarks() {
    // parity: feat:bookmarks.create, feat:bookmarks.update-delete, feat:bookmarks.folders, feat:bookmarks.shared-folders, feat:scanning.scan-bookmark, rpc:accessGroupBookmarkMgr.AddFolder, rpc:accessGroupBookmarkMgr.AddToKnownFolders, rpc:accessGroupBookmarkMgr.CreateSubfolder, rpc:accessGroupBookmarkMgr.DeleteFolder, rpc:accessGroupBookmarkMgr.DeleteSubfolder, rpc:accessGroupBookmarkMgr.RemoveFromKnownFolders, rpc:corpRegistry.AddCorporation, rpc:corpRegistry.InsertApplication, rpc:corpRegistry.UpdateApplicationOffer, rpc:scanMgr.GetSystemScanMgr
    eve_testkit::attest(&[
        "feat:bookmarks.create",
        "feat:bookmarks.update-delete",
        "feat:bookmarks.folders",
        "feat:bookmarks.shared-folders",
        "feat:scanning.scan-bookmark",
    ]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();

    let mut sess1 = SessionState::new(401);
    common::login_pilot(&ctx, &mut sess1, "demo", "demo-secret").await;
    ctx.dispatch_rpc(&mut sess1, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_001)], &kw, None).await.expect("Select Pilot 1");

    let mut sess2 = SessionState::new(402);
    common::login_pilot(&ctx, &mut sess2, "other", "other-secret").await;
    ctx.dispatch_rpc(&mut sess2, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_002)], &kw, None).await.expect("Select Pilot 2");

    // 1. Folders & Subfolders (`AddFolder`, `DeleteFolder`, `CreateSubfolder`, `DeleteSubfolder`, per-character limits)
    let folder_rep =
        ctx.dispatch_rpc(&mut sess1, "accessGroupBookmarkMgr", None, "AddFolder", &[PyRep::String("Safe Spots".into())], &kw, None).await.expect("AddFolder");
    let PyRep::KeyVal(f_kv) = &folder_rep else { panic!("Expected KeyVal for AddFolder") };
    let folder_id = f_kv["folderID"].as_i64().unwrap();
    assert_eq!(f_kv["folderName"], PyRep::String("Safe Spots".into()));

    // Create 5 subfolders under folder_id (max 5 subfolders per folder)
    let mut subfolder_ids = Vec::new();
    for idx in 1..=5 {
        let sub_rep = ctx
            .dispatch_rpc(
                &mut sess1,
                "accessGroupBookmarkMgr",
                None,
                "CreateSubfolder",
                &[PyRep::Integer(folder_id), PyRep::String(format!("Sub {idx}"))],
                &kw,
                None,
            )
            .await
            .expect("CreateSubfolder");
        let PyRep::KeyVal(sub_kv) = &sub_rep else { panic!() };
        subfolder_ids.push(sub_kv["subfolderID"].as_i64().unwrap());
    }
    // 6th subfolder must be rejected by per-folder subfolder limit
    let sub_limit_err = ctx
        .dispatch_rpc(
            &mut sess1,
            "accessGroupBookmarkMgr",
            None,
            "CreateSubfolder",
            &[PyRep::Integer(folder_id), PyRep::String("Sub 6 Overflow".into())],
            &kw,
            None,
        )
        .await
        .expect_err("6th subfolder must fail with MaxSubfoldersReached");
    assert_eq!(sub_limit_err.code, "MaxSubfoldersReached");

    // Verify all 5 subfolders via GetFolderInfo, then DeleteSubfolder and verify removal
    let fi_before = ctx
        .dispatch_rpc(&mut sess1, "accessGroupBookmarkMgr", None, "GetFolderInfo", &[PyRep::Integer(folder_id)], &kw, None)
        .await
        .expect("GetFolderInfo before DeleteSubfolder");
    assert!(matches!(fi_before, PyRep::KeyVal(_)));
    assert_eq!(ctx.db.with_snapshot(|s| s.bookmark_subfolder_parents.values().filter(|&&p| p == folder_id).count()), 5);

    for &sf_id in &subfolder_ids[1..] {
        ctx.dispatch_rpc(&mut sess1, "accessGroupBookmarkMgr", None, "DeleteSubfolder", &[PyRep::Integer(sf_id)], &kw, None).await.expect("DeleteSubfolder");
    }
    let fi_after = ctx
        .dispatch_rpc(&mut sess1, "accessGroupBookmarkMgr", None, "GetFolderInfo", &[PyRep::Integer(folder_id)], &kw, None)
        .await
        .expect("GetFolderInfo after DeleteSubfolder");
    assert!(matches!(fi_after, PyRep::KeyVal(_)));
    assert_eq!(ctx.db.with_snapshot(|s| s.bookmark_subfolder_parents.values().filter(|&&p| p == folder_id).count()), 1);

    // Create 4 more personal root folders (total 5) and verify 6th personal folder fails with MaxPersonalFoldersReached
    let mut extra_folders = Vec::new();
    for idx in 2..=5 {
        let ef_rep = ctx
            .dispatch_rpc(&mut sess1, "accessGroupBookmarkMgr", None, "AddFolder", &[PyRep::String(format!("Personal Folder {idx}"))], &kw, None)
            .await
            .expect("AddFolder within limit");
        let PyRep::KeyVal(ef_kv) = &ef_rep else { panic!() };
        extra_folders.push(ef_kv["folderID"].as_i64().unwrap());
    }
    let root_limit_err = ctx
        .dispatch_rpc(&mut sess1, "accessGroupBookmarkMgr", None, "AddFolder", &[PyRep::String("Personal Folder 6 Overflow".into())], &kw, None)
        .await
        .expect_err("6th personal root folder must fail with MaxPersonalFoldersReached");
    assert_eq!(root_limit_err.code, "MaxPersonalFoldersReached");

    for ef_id in extra_folders {
        ctx.dispatch_rpc(&mut sess1, "accessGroupBookmarkMgr", None, "DeleteFolder", &[PyRep::Integer(ef_id)], &kw, None).await.expect("Delete extra folder");
    }

    // Create shared folders (is_personal = false) up to 30 and verify 31st fails with MaxSharedFoldersReached
    let prev_role = sess1.corp_role;
    sess1.corp_role |= eve_db::corp_roles::ROLE_DIRECTOR;
    let mut shared_folders = Vec::new();
    for idx in 1..=30 {
        let sf_rep = ctx
            .dispatch_rpc(
                &mut sess1,
                "accessGroupBookmarkMgr",
                None,
                "AddFolder",
                &[PyRep::String(format!("Shared Folder {idx}")), PyRep::Bool(true)],
                &kw,
                None,
            )
            .await
            .expect("AddFolder shared within limit");
        let PyRep::KeyVal(sf_kv) = &sf_rep else { panic!() };
        shared_folders.push(sf_kv["folderID"].as_i64().unwrap());
    }
    let shared_limit_err = ctx
        .dispatch_rpc(
            &mut sess1,
            "accessGroupBookmarkMgr",
            None,
            "AddFolder",
            &[PyRep::String("Shared Folder 31 Overflow".into()), PyRep::Bool(true)],
            &kw,
            None,
        )
        .await
        .expect_err("31st shared folder must fail with MaxSharedFoldersReached");
    assert_eq!(shared_limit_err.code, "MaxSharedFoldersReached");

    for sf_id in shared_folders {
        ctx.dispatch_rpc(&mut sess1, "accessGroupBookmarkMgr", None, "DeleteFolder", &[PyRep::Integer(sf_id)], &kw, None).await.expect("Delete shared folder");
    }
    sess1.corp_role = prev_role;

    // 2. Create Bookmark in Folder (`BookmarkLocation` & `accessGroupBookmarkMgr.BookmarkStaticLocation`) and verify stored coordinates, item, subfolder, and expiry
    let ship_id = sess1.ship_id.expect("Pilot 1 ship_id");
    ctx.dispatch_rpc(&mut sess1, "stationSvc", None, "Undock", &[], &kw, None).await.expect("Undock Pilot 1");
    let sys = ctx.systems.get_or_spawn(30_000_142);
    let expected_pos = DVec3::new(125_000.0, -45_000.0, 80_000.0);
    let _ = sys.set_ball_position(ship_id, expected_pos).await;

    let bm1_rep = ctx
        .dispatch_rpc(
            &mut sess1,
            "accessGroupBookmarkMgr",
            None,
            "BookmarkStaticLocation",
            &[
                PyRep::String("Deep Safe 1".into()),
                PyRep::Integer(ship_id),
                PyRep::String("100 AU off plane".into()),
                PyRep::Integer(folder_id),
                PyRep::Integer(subfolder_ids[0]),
                PyRep::Integer(3),
            ],
            &kw,
            None,
        )
        .await
        .expect("BookmarkStaticLocation");
    let bm_id = match &bm1_rep {
        PyRep::Tuple(parts) if parts.len() == 8 => parts[0].as_i64().unwrap(),
        PyRep::KeyVal(bm1_kv) => bm1_kv["bookmarkID"].as_i64().unwrap(),
        _ => panic!("Expected 8-tuple or KeyVal from BookmarkStaticLocation"),
    };
    assert!(bm_id > 0);

    // Verify stored coordinates, itemID, memo, folderID, and subfolderID via GetBookmarks and GetMyActiveBookmarks
    let bms_rep = ctx.dispatch_rpc(&mut sess1, "accessGroupBookmarkMgr", None, "GetMyActiveBookmarks", &[], &kw, None).await.expect("GetMyActiveBookmarks");
    let PyRep::Tuple(bms_tup) = &bms_rep else { panic!("Expected 3-tuple from GetMyActiveBookmarks") };
    let PyRep::List(bm_rows) = &bms_tup[1] else { panic!("Expected bookmarks list in GetMyActiveBookmarks[1]") };
    let bm_kv = bm_rows
        .iter()
        .filter_map(|r| if let PyRep::KeyVal(kv) = r { Some(kv) } else { None })
        .find(|kv| kv.get("bookmarkID") == Some(&PyRep::Integer(bm_id)))
        .expect("Created bookmark must appear in GetMyActiveBookmarks");
    assert_eq!(bm_kv["label"], PyRep::String("Deep Safe 1".into()));
    assert_eq!(bm_kv["solarSystemID"], PyRep::Integer(30_000_142));
    assert_eq!(bm_kv["locationID"], PyRep::Integer(30_000_142));
    assert_eq!(bm_kv["x"], PyRep::Real(expected_pos.x));
    assert_eq!(bm_kv["y"], PyRep::Real(expected_pos.y));
    assert_eq!(bm_kv["z"], PyRep::Real(expected_pos.z));
    assert_eq!(bm_kv["itemID"], PyRep::Integer(ship_id));
    assert_eq!(bm_kv["memo"], PyRep::String("100 AU off plane".into()));
    assert_eq!(bm_kv["folderID"], PyRep::Integer(folder_id));
    assert_eq!(bm_kv["subfolderID"], PyRep::Integer(subfolder_ids[0]));

    // 3. Update Bookmark (memo/label, note, folder, and expiry) via accessGroupBookmarkMgr.UpdateBookmark
    let update_bm = ctx
        .dispatch_rpc(
            &mut sess1,
            "accessGroupBookmarkMgr",
            None,
            "UpdateBookmark",
            &[
                PyRep::Integer(bm_id),
                PyRep::String("Deep Safe 1 (Checked)".into()),
                PyRep::String("Updated memo".into()),
                PyRep::Integer(folder_id),
                PyRep::Integer(48),
            ],
            &kw,
            None,
        )
        .await
        .expect("UpdateBookmark");
    let PyRep::KeyVal(upd_kv) = &update_bm else { panic!() };
    assert!(upd_kv["expiryUnix"].as_i64().unwrap_or(0) > 0);

    let bms_after_update = ctx.dispatch_rpc(&mut sess1, "bookmark", None, "GetBookmarks", &[], &kw, None).await.expect("GetBookmarks after UpdateBookmark");
    let PyRep::Rowset { rows: bm_rows_upd, .. } = &bms_after_update else { panic!() };
    let bm_row_upd = bm_rows_upd.iter().find(|r| r[0] == PyRep::Integer(bm_id)).unwrap();
    assert_eq!(bm_row_upd[1], PyRep::String("Deep Safe 1 (Checked)".into()));
    assert_eq!(bm_row_upd[7], PyRep::String("Updated memo".into()));

    // 4. Shared Bookmark Folders: create corporation via real RPCs, verify access-group rights (view/use vs manage/admin),
    // AddToKnownFolders / RemoveFromKnownFolders, and OnSharedBookmarksFolderUpdated notifications
    ctx.advance_secs(12);
    let create_corp_rep = ctx
        .dispatch_rpc(
            &mut sess1,
            "corpRegistry",
            None,
            "AddCorporation",
            &[PyRep::String("Cartography Syndicate".into()), PyRep::String("CARTO".into()), PyRep::Real(0.05)],
            &kw,
            None,
        )
        .await
        .expect("AddCorporation for shared bookmarks");
    let PyRep::KeyVal(cc_kv) = &create_corp_rep else { panic!() };
    let corp_id = cc_kv["corporationID"].as_i64().unwrap();

    let apply_rep = ctx
        .dispatch_rpc(&mut sess2, "corpRegistry", None, "InsertApplication", &[PyRep::Integer(corp_id), PyRep::String("Scout joining".into())], &kw, None)
        .await
        .expect("Pilot 2 InsertApplication");
    let PyRep::KeyVal(app_kv) = &apply_rep else { panic!() };
    let app_id = app_kv["applicationID"].as_i64().unwrap();
    ctx.dispatch_rpc(&mut sess1, "corpRegistry", None, "UpdateApplicationOffer", &[PyRep::Integer(app_id)], &kw, None)
        .await
        .expect("Pilot 1 UpdateApplicationOffer");

    // Pilot 2 (normal corp member with view/use rights only) cannot create a shared corp folder
    let p2_denied_shared = ctx
        .dispatch_rpc(&mut sess2, "accessGroupBookmarkMgr", None, "AddFolder", &[PyRep::String("Unauthorized Shared".into()), PyRep::Bool(true)], &kw, None)
        .await
        .expect_err("Regular member without access-group rights must be denied shared AddFolder");
    assert_eq!(p2_denied_shared.code, "BookmarkAccessDenied");

    // Pilot 1 (CEO) creates shared folder and shared bookmark, emitting OnSharedBookmarksFolderUpdated
    let _ = ctx.drain_notifications_for_char(90_000_002);
    let shared_folder_rep = ctx
        .dispatch_rpc(&mut sess1, "accessGroupBookmarkMgr", None, "AddFolder", &[PyRep::String("Corp Tacticals".into()), PyRep::Bool(true)], &kw, None)
        .await
        .expect("AddFolder shared");
    let PyRep::KeyVal(sf_kv) = &shared_folder_rep else { panic!() };
    let shared_folder_id = sf_kv["folderID"].as_i64().unwrap();
    assert_eq!(sf_kv["isShared"], PyRep::Bool(true));

    let shared_bm_rep = ctx
        .dispatch_rpc(
            &mut sess1,
            "accessGroupBookmarkMgr",
            None,
            "BookmarkStaticLocation",
            &[PyRep::String("Corp Gate Ping".into()), PyRep::Integer(ship_id), PyRep::String("Shared tactical".into()), PyRep::Integer(shared_folder_id)],
            &kw,
            None,
        )
        .await
        .expect("Create shared bookmark");
    let shared_bm_id = match &shared_bm_rep {
        PyRep::Tuple(parts) if parts.len() == 8 => parts[0].as_i64().unwrap(),
        PyRep::KeyVal(sbm1_kv) => sbm1_kv["bookmarkID"].as_i64().unwrap(),
        _ => panic!("Expected 8-tuple or KeyVal from BookmarkStaticLocation"),
    };

    // Pilot 2 subscribes to the shared folder via AddToKnownFolders and receives OnSharedBookmarksFolderUpdated
    let add_known_rep = ctx
        .dispatch_rpc(&mut sess2, "accessGroupBookmarkMgr", None, "AddToKnownFolders", &[PyRep::Integer(shared_folder_id)], &kw, None)
        .await
        .expect("AddToKnownFolders");
    let is_subscribed = match &add_known_rep {
        PyRep::Tuple(parts) => parts[0].dict_get("isActive") == Some(&PyRep::Bool(true)),
        PyRep::KeyVal(ak_kv) => ak_kv["subscribed"] == PyRep::Bool(true),
        _ => panic!("Expected 3-tuple or KeyVal from AddToKnownFolders"),
    };
    assert!(is_subscribed);
    let p2_shared_notifs = ctx.drain_notifications_for_char(90_000_002);
    assert!(p2_shared_notifs.iter().any(|n| n.event == "OnSharedBookmarksFolderUpdated"), "AddToKnownFolders must emit OnSharedBookmarksFolderUpdated");

    // Pilot 2 has view/use access to the shared bookmark, but cannot edit or delete it without manage/admin role
    let p2_active_bms =
        ctx.dispatch_rpc(&mut sess2, "accessGroupBookmarkMgr", None, "GetMyActiveBookmarks", &[], &kw, None).await.expect("Pilot 2 GetMyActiveBookmarks");
    let PyRep::Tuple(p2_tup) = &p2_active_bms else { panic!() };
    let PyRep::List(p2_bm_rows) = &p2_tup[1] else { panic!() };
    assert!(
        p2_bm_rows.iter().any(|r| matches!(r, PyRep::KeyVal(kv) if kv.get("bookmarkID") == Some(&PyRep::Integer(shared_bm_id)))),
        "Subscribed corp member must see shared bookmark"
    );

    let p2_edit_denied = ctx
        .dispatch_rpc(
            &mut sess2,
            "accessGroupBookmarkMgr",
            None,
            "UpdateBookmark",
            &[PyRep::Integer(shared_bm_id), PyRep::String("Tampered".into()), PyRep::String("No rights".into())],
            &kw,
            None,
        )
        .await
        .expect_err("View/use member without manage/admin rights must not edit another member's shared bookmark");
    assert_eq!(p2_edit_denied.code, "AccessDenied");

    // Issue #42: Legacy `bookmark.CreateSubfolder` and `bookmark.UpdateBookmark` must also enforce folder ACLs
    assert_eq!(
        ctx.dispatch_rpc(&mut sess2, "bookmark", None, "CreateSubfolder", &[PyRep::Integer(folder_id), PyRep::String("Unauthorized Sub".into())], &kw, None)
            .await
            .expect_err("Pilot 2 must not create a subfolder in Pilot 1's personal folder")
            .code,
        "AccessDenied"
    );
    let p2_own_bm_rep = ctx
        .dispatch_rpc(
            &mut sess2,
            "bookmark",
            None,
            "BookmarkLocation",
            &[PyRep::Integer(60_003_760), PyRep::Integer(90_000_002), PyRep::String("P2 Personal Spot".into()), PyRep::String("Memo".into())],
            &kw,
            None,
        )
        .await
        .expect("Pilot 2 BookmarkLocation");
    let PyRep::KeyVal(p2_own_bm_kv) = &p2_own_bm_rep else { panic!() };
    let p2_own_bm_id = p2_own_bm_kv["bookmarkID"].as_i64().unwrap();
    assert_eq!(
        ctx.dispatch_rpc(
            &mut sess2,
            "bookmark",
            None,
            "UpdateBookmark",
            &[PyRep::Integer(p2_own_bm_id), PyRep::String("P2 Personal Spot".into()), PyRep::String("Memo".into()), PyRep::Integer(folder_id)],
            &kw,
            None
        )
        .await
        .expect_err("Pilot 2 must not move bookmark into Pilot 1's personal folder")
        .code,
        "AccessDenied"
    );

    let p2_del_denied = ctx
        .dispatch_rpc(&mut sess2, "accessGroupBookmarkMgr", None, "DeleteBookmarks", &[PyRep::List(vec![PyRep::Integer(shared_bm_id)])], &kw, None)
        .await
        .expect_err("View/use member without manage/admin rights must not delete another member's shared bookmark");
    assert_eq!(p2_del_denied.code, "AccessDenied");

    // Grant Pilot 2 manage/admin role (ROLE_PERSONNEL_MANAGER = 128) and verify Pilot 2 can now edit and delete the shared bookmark
    ctx.dispatch_rpc(&mut sess1, "corpRegistry", None, "UpdateMember", &[PyRep::Integer(90_000_002), PyRep::Integer(128)], &kw, None)
        .await
        .expect("Grant Pilot 2 bookmark admin role");
    ctx.dispatch_rpc(
        &mut sess2,
        "accessGroupBookmarkMgr",
        None,
        "UpdateBookmark",
        &[PyRep::Integer(shared_bm_id), PyRep::String("Corp Gate Ping (Verified)".into()), PyRep::String("Admin updated".into())],
        &kw,
        None,
    )
    .await
    .expect("Admin member UpdateBookmark on shared bookmark");
    ctx.dispatch_rpc(&mut sess2, "accessGroupBookmarkMgr", None, "DeleteBookmarks", &[PyRep::List(vec![PyRep::Integer(shared_bm_id)])], &kw, None)
        .await
        .expect("Admin member DeleteBookmarks on shared bookmark");

    let p2_shared = ctx.dispatch_rpc(&mut sess2, "bookmark", None, "GetFolders", &[], &kw, None).await.expect("GetFolders");
    let PyRep::Rowset { rows: sf_rows, .. } = &p2_shared else { panic!() };
    assert!(
        sf_rows.iter().any(|r| r[0] == PyRep::Integer(shared_folder_id) && r[1] == PyRep::String("Corp Tacticals".into())),
        "GetFolders must return Corp Tacticals for Pilot 2"
    );

    // Pilot 2 unsubscribes via RemoveFromKnownFolders and receives OnSharedBookmarksFolderUpdated
    let rem_known_rep = ctx
        .dispatch_rpc(&mut sess2, "accessGroupBookmarkMgr", None, "RemoveFromKnownFolders", &[PyRep::Integer(shared_folder_id)], &kw, None)
        .await
        .expect("RemoveFromKnownFolders");
    let PyRep::KeyVal(rk_kv) = &rem_known_rep else { panic!() };
    assert_eq!(rk_kv["subscribed"], PyRep::Bool(false));
    let p2_unsub_notifs = ctx.drain_notifications_for_char(90_000_002);
    assert!(p2_unsub_notifs.iter().any(|n| n.event == "OnSharedBookmarksFolderUpdated"), "RemoveFromKnownFolders must emit OnSharedBookmarksFolderUpdated");

    // 5. Scan Bookmark: resolve a cosmic signature to 100% via V24 scanMgr.RequestScans and save via BookmarkScanResult
    let scan_mgr_rep = ctx.dispatch_rpc(&mut sess1, "scanMgr", None, "GetSystemScanMgr", &[], &kw, None).await.expect("GetSystemScanMgr");
    let PyRep::KeyVal(sm_kv) = &scan_mgr_rep else { panic!() };
    let scan_bound_id = sm_kv["boundID"].as_i64().unwrap() as u64;

    let (wh_sig_id, wh_pos) = ctx.db.with_snapshot(|snap| {
        let sig = snap.cosmic_signatures[&30_000_142].first().unwrap();
        (sig.signature_id.clone(), sig.position)
    });

    // Attempting BookmarkScanResult before resolving signature to 100% must fail with SignatureNotResolved
    let unres_err = ctx
        .dispatch_rpc(
            &mut sess1,
            "bookmark",
            None,
            "BookmarkScanResult",
            &[PyRep::String(wh_sig_id.clone()), PyRep::String("Unresolved Sig".into()), PyRep::Integer(folder_id)],
            &kw,
            None,
        )
        .await
        .expect_err("Bookmarking unresolved signature must fail");
    assert_eq!(unres_err.code, "SignatureNotResolved");

    // Fit Core Probe Launcher I + 8 probes on Pilot 1's ship, launch probes, and resolve signature via RequestScans
    ctx.db.with_mut_snapshot(|snap| {
        let skills = snap.skills.entry(90_000_001).or_default();
        for &sid in &[3402u32, 3412, 3413, 3426] {
            skills.insert(sid, eve_db::TrainedSkillRecord { type_id: sid, level: 5, skill_points: 256_000.0 });
        }
        snap.items.insert(
            440_001,
            eve_db::InventoryItemRecord {
                item_id: 440_001,
                type_id: 17938,
                owner_id: 90_000_001,
                location_id: ship_id,
                flag: eve_db::InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        snap.items.insert(
            440_002,
            eve_db::InventoryItemRecord {
                item_id: 440_002,
                type_id: 30013,
                owner_id: 90_000_001,
                location_id: ship_id,
                flag: eve_db::InventoryFlag::HiSlot0,
                quantity: 8,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });
    ctx.invalidate_ship_fitting(ship_id);

    let launch_rep = ctx
        .dispatch_rpc(
            &mut sess1,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(440_001), PyRep::String("launchScanners".into()), PyRep::None, PyRep::Integer(0)],
            &kw,
            None,
        )
        .await
        .expect("Launch probes in SO-4");
    let PyRep::KeyVal(lkv) = &launch_rep else { panic!() };
    let PyRep::List(pids) = &lkv["probeIDs"] else { panic!() };
    let probe_ids: Vec<i64> = pids.iter().filter_map(|p| p.as_i64()).collect();
    let center = DVec3::new(wh_pos[0], wh_pos[1], wh_pos[2]);
    let scan_radius = 0.25 * AU;
    let probes_dict = PyRep::Dict(
        probe_ids
            .iter()
            .map(|&pid| {
                (
                    PyRep::Integer(pid),
                    PyRep::keyval([
                        ("pos", PyRep::Tuple(vec![PyRep::Real(center.x), PyRep::Real(center.y), PyRep::Real(center.z)])),
                        ("scanRange", PyRep::Real(scan_radius)),
                    ]),
                )
            })
            .collect(),
    );
    for _ in 0..4 {
        ctx.dispatch_rpc(&mut sess1, "scanMgr", Some(scan_bound_id), "RequestScans", &[probes_dict.clone()], &kw, None).await.expect("RequestScans in SO-4");
    }

    // Bookmark the 100%-resolved signature via scanMgr.BookmarkScanResult, verify persisted coordinates, and warp to it
    let scan_bm = ctx
        .dispatch_rpc(
            &mut sess1,
            "scanMgr",
            None,
            "BookmarkScanResult",
            &[PyRep::String(wh_sig_id.clone()), PyRep::String("Resolved K162".into()), PyRep::Integer(folder_id)],
            &kw,
            None,
        )
        .await
        .expect("BookmarkScanResult");
    let PyRep::KeyVal(sbm_kv) = &scan_bm else { panic!() };
    let scan_bm_id = sbm_kv["bookmarkID"].as_i64().unwrap();

    let bms_with_scan = ctx.dispatch_rpc(&mut sess1, "bookmark", None, "GetBookmarks", &[], &kw, None).await.expect("GetBookmarks after BookmarkScanResult");
    let PyRep::Rowset { rows: bms_scan_rows, .. } = &bms_with_scan else { panic!() };
    let scan_row = bms_scan_rows.iter().find(|r| r[0] == PyRep::Integer(scan_bm_id)).expect("Scan bookmark must be persisted in GetBookmarks");
    assert_eq!(scan_row[1], PyRep::String("Resolved K162".into()));
    assert_eq!(scan_row[3], PyRep::Real(wh_pos[0]));
    assert_eq!(scan_row[4], PyRep::Real(wh_pos[1]));
    assert_eq!(scan_row[5], PyRep::Real(wh_pos[2]));
    assert_eq!(scan_row[8], PyRep::Integer(folder_id));

    ctx.dispatch_rpc(
        &mut sess1,
        "beyonce",
        None,
        "CmdWarpToStuff",
        &[PyRep::String("bookmark".into()), PyRep::Integer(scan_bm_id), PyRep::Real(0.0)],
        &kw,
        None,
    )
    .await
    .expect("CmdWarpToStuff to scan bookmark");
    let (_, scan_warp_ball) = sys.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!(matches!(scan_warp_ball.mode, MovementMode::Warp { .. }), "Ship must warp to scan result bookmark");

    // 6. Delete Bookmark and Folder, then verify removal in GetBookmarks and GetFolders
    let del_bm = ctx
        .dispatch_rpc(&mut sess1, "bookmark", None, "DeleteBookmarks", &[PyRep::List(vec![PyRep::Integer(bm_id)])], &kw, None)
        .await
        .expect("DeleteBookmarks");
    assert!(is_keyval(&del_bm));

    let bms_after_del = ctx.dispatch_rpc(&mut sess1, "bookmark", None, "GetBookmarks", &[], &kw, None).await.expect("GetBookmarks after DeleteBookmarks");
    let PyRep::Rowset { rows: bms_del_rows, .. } = &bms_after_del else { panic!() };
    assert!(!bms_del_rows.iter().any(|r| r[0] == PyRep::Integer(bm_id)), "Deleted bookmark must be removed");

    let del_folder =
        ctx.dispatch_rpc(&mut sess1, "accessGroupBookmarkMgr", None, "DeleteFolder", &[PyRep::Integer(folder_id)], &kw, None).await.expect("DeleteFolder");
    assert!(is_keyval(&del_folder));

    let folders_after_del = ctx.dispatch_rpc(&mut sess1, "bookmark", None, "GetFolders", &[], &kw, None).await.expect("GetFolders after DeleteFolder");
    let PyRep::Rowset { rows: f_del_rows, .. } = &folders_after_del else { panic!() };
    assert!(!f_del_rows.iter().any(|r| r[0] == PyRep::Integer(folder_id)), "Deleted folder must be removed");
}
