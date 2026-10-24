//! Track 4: Phase 8 Bookmarks (`SO-4`), Scanning & Dungeons (`SO-5`), and Wormholes (`SO-6`) Integration Tests.

use crate::common;

use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

const AU: f64 = 149_597_870_700.0;

fn is_keyval(rep: &PyRep) -> bool {
    matches!(rep, PyRep::KeyVal(_))
}

#[tokio::test]
async fn test_so4_bookmarks_folders_shared_folders_and_scan_bookmarks() {
    // parity: feat:bookmarks.create, feat:bookmarks.update-delete, feat:bookmarks.folders, feat:bookmarks.shared-folders, feat:scanning.scan-bookmark, rpc:accessGroupBookmarkMgr.AddFolder, rpc:accessGroupBookmarkMgr.AddToKnownFolders, rpc:accessGroupBookmarkMgr.CreateSubfolder, rpc:accessGroupBookmarkMgr.DeleteFolder, rpc:accessGroupBookmarkMgr.DeleteSubfolder, rpc:accessGroupBookmarkMgr.RemoveFromKnownFolders, rpc:corpRegistry.AddCorporation, rpc:corpRegistry.InsertApplication, rpc:corpRegistry.UpdateApplicationOffer, rpc:scanMgr.GetSystemScanMgr
    eve_testkit::attest(&["feat:bookmarks.create", "feat:bookmarks.update-delete", "feat:bookmarks.folders", "feat:bookmarks.shared-folders", "feat:scanning.scan-bookmark"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();

    let mut sess1 = SessionState::new(401);
    common::login_pilot(&ctx, &mut sess1, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut sess1,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
    .expect("Select Pilot 1");

    let mut sess2 = SessionState::new(402);
    common::login_pilot(&ctx, &mut sess2, "other", "other-secret").await;
    ctx.dispatch_rpc(
        &mut sess2,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_002)],
        &kw,
        None,
    )
    .await
    .expect("Select Pilot 2");

    // 1. Folders & Subfolders (`AddFolder`, `DeleteFolder`, `CreateSubfolder`, `DeleteSubfolder`, per-character limits)
    let folder_rep = ctx
        .dispatch_rpc(
            &mut sess1,
            "accessGroupBookmarkMgr",
            None,
            "AddFolder",
            &[PyRep::String("Safe Spots".into())],
            &kw,
            None,
        )
        .await
        .expect("AddFolder");
    let PyRep::KeyVal(f_kv) = &folder_rep else {
        panic!("Expected KeyVal for AddFolder")
    };
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
                &[
                    PyRep::Integer(folder_id),
                    PyRep::String(format!("Sub {idx}")),
                ],
                &kw,
                None,
            )
            .await
            .expect("CreateSubfolder");
        let PyRep::KeyVal(sub_kv) = &sub_rep else {
            panic!()
        };
        subfolder_ids.push(sub_kv["subfolderID"].as_i64().unwrap());
    }
    // 6th subfolder must be rejected by per-folder subfolder limit
    let sub_limit_err = ctx
        .dispatch_rpc(
            &mut sess1,
            "accessGroupBookmarkMgr",
            None,
            "CreateSubfolder",
            &[
                PyRep::Integer(folder_id),
                PyRep::String("Sub 6 Overflow".into()),
            ],
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
        ctx.dispatch_rpc(&mut sess1, "accessGroupBookmarkMgr", None, "DeleteSubfolder", &[PyRep::Integer(sf_id)], &kw, None)
            .await
            .expect("DeleteSubfolder");
    }
    let fi_after = ctx
        .dispatch_rpc(&mut sess1, "accessGroupBookmarkMgr", None, "GetFolderInfo", &[PyRep::Integer(folder_id)], &kw, None)
        .await
        .expect("GetFolderInfo after DeleteSubfolder");
    assert!(matches!(fi_after, PyRep::KeyVal(_)));
    assert_eq!(ctx.db.with_snapshot(|s| s.bookmark_subfolder_parents.values().filter(|&&p| p == folder_id).count()), 1);

    // Create 29 more personal root folders (total 30) and verify 31st personal folder fails with MaxPersonalFoldersReached
    let mut extra_folders = Vec::new();
    for idx in 2..=30 {
        let ef_rep = ctx
            .dispatch_rpc(&mut sess1, "accessGroupBookmarkMgr", None, "AddFolder", &[PyRep::String(format!("Personal Folder {idx}"))], &kw, None)
            .await
            .expect("AddFolder within limit");
        let PyRep::KeyVal(ef_kv) = &ef_rep else { panic!() };
        extra_folders.push(ef_kv["folderID"].as_i64().unwrap());
    }
    let root_limit_err = ctx
            .dispatch_rpc(&mut sess1, "accessGroupBookmarkMgr", None, "AddFolder", &[PyRep::String("Personal Folder 31 Overflow".into())], &kw, None)
        .await
        .expect_err("31st personal root folder must fail with MaxPersonalFoldersReached");
    assert_eq!(root_limit_err.code, "MaxPersonalFoldersReached");

    for ef_id in extra_folders {
        ctx.dispatch_rpc(&mut sess1, "accessGroupBookmarkMgr", None, "DeleteFolder", &[PyRep::Integer(ef_id)], &kw, None)
            .await
            .expect("Delete extra folder");
    }

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
    let bms_rep = ctx
        .dispatch_rpc(&mut sess1, "accessGroupBookmarkMgr", None, "GetMyActiveBookmarks", &[], &kw, None)
        .await
        .expect("GetMyActiveBookmarks");
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

    let bms_after_update = ctx
        .dispatch_rpc(&mut sess1, "bookmark", None, "GetBookmarks", &[], &kw, None)
        .await
        .expect("GetBookmarks after UpdateBookmark");
    let PyRep::Rowset { rows: bm_rows_upd, .. } = &bms_after_update else { panic!() };
    let bm_row_upd = bm_rows_upd.iter().find(|r| r[0] == PyRep::Integer(bm_id)).unwrap();
    assert_eq!(bm_row_upd[1], PyRep::String("Deep Safe 1 (Checked)".into()));
    assert_eq!(bm_row_upd[7], PyRep::String("Updated memo".into()));

    // 4. Shared Bookmark Folders: create corporation via real RPCs, verify access-group rights (view/use vs manage/admin),
    // AddToKnownFolders / RemoveFromKnownFolders, and OnSharedBookmarksFolderUpdated notifications
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
    assert!(
        p2_shared_notifs.iter().any(|n| n.event == "OnSharedBookmarksFolderUpdated"),
        "AddToKnownFolders must emit OnSharedBookmarksFolderUpdated"
    );

    // Pilot 2 has view/use access to the shared bookmark, but cannot edit or delete it without manage/admin role
    let p2_active_bms = ctx
        .dispatch_rpc(&mut sess2, "accessGroupBookmarkMgr", None, "GetMyActiveBookmarks", &[], &kw, None)
        .await
        .expect("Pilot 2 GetMyActiveBookmarks");
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

    let p2_del_denied = ctx
        .dispatch_rpc(
            &mut sess2,
            "accessGroupBookmarkMgr",
            None,
            "DeleteBookmarks",
            &[PyRep::List(vec![PyRep::Integer(shared_bm_id)])],
            &kw,
            None,
        )
        .await
        .expect_err("View/use member without manage/admin rights must not delete another member's shared bookmark");
    assert_eq!(p2_del_denied.code, "AccessDenied");

    // Grant Pilot 2 manage/admin role (ROLE_PERSONNEL_MANAGER = 128) and verify Pilot 2 can now edit and delete the shared bookmark
    ctx.dispatch_rpc(
        &mut sess1,
        "corpRegistry",
        None,
        "UpdateMember",
        &[PyRep::Integer(90_000_002), PyRep::Integer(128)],
        &kw,
        None,
    )
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
    ctx.dispatch_rpc(
        &mut sess2,
        "accessGroupBookmarkMgr",
        None,
        "DeleteBookmarks",
        &[PyRep::List(vec![PyRep::Integer(shared_bm_id)])],
        &kw,
        None,
    )
    .await
    .expect("Admin member DeleteBookmarks on shared bookmark");

    let p2_shared = ctx
        .dispatch_rpc(&mut sess2, "bookmark", None, "GetFolders", &[], &kw, None)
        .await
        .expect("GetFolders");
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
    assert!(
        p2_unsub_notifs.iter().any(|n| n.event == "OnSharedBookmarksFolderUpdated"),
        "RemoveFromKnownFolders must emit OnSharedBookmarksFolderUpdated"
    );

    // 5. Scan Bookmark: resolve a cosmic signature to 100% via V24 scanMgr.RequestScans and save via BookmarkScanResult
    let scan_mgr_rep = ctx
        .dispatch_rpc(&mut sess1, "scanMgr", None, "GetSystemScanMgr", &[], &kw, None)
        .await
        .expect("GetSystemScanMgr");
    let PyRep::KeyVal(sm_kv) = &scan_mgr_rep else {
        panic!()
    };
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
            &[
                PyRep::String(wh_sig_id.clone()),
                PyRep::String("Unresolved Sig".into()),
                PyRep::Integer(folder_id),
            ],
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
            skills.insert(
                sid,
                eve_db::TrainedSkillRecord {
                    type_id: sid,
                    level: 5,
                    skill_points: 256_000.0,
                },
            );
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
            &[
                PyRep::Integer(440_001),
                PyRep::String("launchScanners".into()),
                PyRep::None,
                PyRep::Integer(0),
            ],
            &kw,
            None,
        )
        .await
        .expect("Launch probes in SO-4");
    let PyRep::KeyVal(lkv) = &launch_rep else {
        panic!()
    };
    let PyRep::List(pids) = &lkv["probeIDs"] else {
        panic!()
    };
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
                        (
                            "pos",
                            PyRep::Tuple(vec![
                                PyRep::Real(center.x),
                                PyRep::Real(center.y),
                                PyRep::Real(center.z),
                            ]),
                        ),
                        ("scanRange", PyRep::Real(scan_radius)),
                    ]),
                )
            })
            .collect(),
    );
    for _ in 0..4 {
        ctx.dispatch_rpc(
            &mut sess1,
            "scanMgr",
            Some(scan_bound_id),
            "RequestScans",
            &[probes_dict.clone()],
            &kw,
            None,
        )
        .await
        .expect("RequestScans in SO-4");
    }

    // Bookmark the 100%-resolved signature via scanMgr.BookmarkScanResult, verify persisted coordinates, and warp to it
    let scan_bm = ctx
        .dispatch_rpc(
            &mut sess1,
            "scanMgr",
            None,
            "BookmarkScanResult",
            &[
                PyRep::String(wh_sig_id.clone()),
                PyRep::String("Resolved K162".into()),
                PyRep::Integer(folder_id),
            ],
            &kw,
            None,
        )
        .await
        .expect("BookmarkScanResult");
    let PyRep::KeyVal(sbm_kv) = &scan_bm else { panic!() };
    let scan_bm_id = sbm_kv["bookmarkID"].as_i64().unwrap();

    let bms_with_scan = ctx
        .dispatch_rpc(&mut sess1, "bookmark", None, "GetBookmarks", &[], &kw, None)
        .await
        .expect("GetBookmarks after BookmarkScanResult");
    let PyRep::Rowset { rows: bms_scan_rows, .. } = &bms_with_scan else { panic!() };
    let scan_row = bms_scan_rows
        .iter()
        .find(|r| r[0] == PyRep::Integer(scan_bm_id))
        .expect("Scan bookmark must be persisted in GetBookmarks");
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

    let bms_after_del = ctx
        .dispatch_rpc(&mut sess1, "bookmark", None, "GetBookmarks", &[], &kw, None)
        .await
        .expect("GetBookmarks after DeleteBookmarks");
    let PyRep::Rowset { rows: bms_del_rows, .. } = &bms_after_del else { panic!() };
    assert!(!bms_del_rows.iter().any(|r| r[0] == PyRep::Integer(bm_id)), "Deleted bookmark must be removed");

    let del_folder = ctx
        .dispatch_rpc(&mut sess1, "accessGroupBookmarkMgr", None, "DeleteFolder", &[PyRep::Integer(folder_id)], &kw, None)
        .await
        .expect("DeleteFolder");
    assert!(is_keyval(&del_folder));

    let folders_after_del = ctx
        .dispatch_rpc(&mut sess1, "bookmark", None, "GetFolders", &[], &kw, None)
        .await
        .expect("GetFolders after DeleteFolder");
    let PyRep::Rowset { rows: f_del_rows, .. } = &folders_after_del else { panic!() };
    assert!(!f_del_rows.iter().any(|r| r[0] == PyRep::Integer(folder_id)), "Deleted folder must be removed");
}

#[tokio::test]
async fn test_so5_scanning_dscan_probes_and_acceleration_gates() {
    // parity: feat:scanning.dscan, feat:scanning.probe-launch, feat:scanning.probe-scan-math, feat:scanning.probe-recall, feat:navigation.acceleration-gates, rpc:charUnboundMgr.SelectCharacterID, rpc:dogmaIM.LoadAmmo, rpc:keeper.ActivateAccelerationGate, rpc:scanMgr.GetSystemScanMgr
    eve_testkit::attest(&["feat:scanning.dscan", "feat:scanning.probe-launch", "feat:scanning.probe-scan-math", "feat:scanning.probe-recall", "feat:navigation.acceleration-gates"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();

    let mut sess = SessionState::new(501);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
    .expect("Select Pilot 1");

    let char_id = 90_000_001i64;
    let ship_id = sess.ship_id.expect("ship_id");

    // Grant Astrometrics (3412) and CPU Management (3426) for Core Probe Launcher I (17938)
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.retain(|_, i| i.location_id != ship_id);
        let skills = snap.skills.entry(char_id).or_default();
        for &sid in &[3402u32, 3412, 3413, 3426] {
            skills.insert(
                sid,
                eve_db::TrainedSkillRecord {
                    type_id: sid,
                    level: 5,
                    skill_points: 256_000.0,
                },
            );
        }
        snap.items.insert(
            550_001,
            eve_db::InventoryItemRecord {
                item_id: 550_001,
                type_id: 17938,
                owner_id: char_id,
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
            550_002,
            eve_db::InventoryItemRecord {
                item_id: 550_002,
                type_id: 30013,
                owner_id: char_id,
                location_id: ship_id,
                flag: eve_db::InventoryFlag::Cargo,
                quantity: 16,
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

    // Undock into Jita (30000142)
    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");

    let sys = ctx.systems.get_or_spawn(30_000_142);
    let _ = sys.set_ball_position(ship_id, DVec3::ZERO).await;

    let target_ball = Ball {
        id: 500_000_002,
        type_id: 603, // Merlin
        position: DVec3::new(0.0, 0.0, 10_000_000.0), // 10,000 km straight ahead (+Z)
        velocity: DVec3::ZERO,
        max_velocity: 200.0,
        inertia: 3.0,
        mass: 5_000_000.0,
        radius: 80.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let recon_ball = Ball {
        id: 500_000_003,
        type_id: 11961, // Huginn (Combat Recon Ship, group 906 -> immune to D-scan!)
        position: DVec3::new(0.0, 0.0, 5_000_000.0),
        velocity: DVec3::ZERO,
        max_velocity: 200.0,
        inertia: 3.0,
        mass: 12_000_000.0,
        radius: 120.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let cloaked_ball = Ball {
        id: 500_000_004,
        type_id: 603,
        position: DVec3::new(0.0, 0.0, 4_000_000.0),
        velocity: DVec3::ZERO,
        max_velocity: 200.0,
        inertia: 3.0,
        mass: 5_000_000.0,
        radius: 80.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let _ = sys.spawn_ball(target_ball).await;
    let _ = sys.spawn_ball(recon_ball).await;
    let mut cloaked_cs = eve_destiny::CombatState::default();
    cloaked_cs.is_cloaked = true;
    let _ = sys.spawn_ball_with_combat(cloaked_ball, cloaked_cs).await;

    // Bind scanMgr via real V24 scanMgr.GetSystemScanMgr
    let scan_mgr_rep = ctx
        .dispatch_rpc(&mut sess, "scanMgr", None, "GetSystemScanMgr", &[], &kw, None)
        .await
        .expect("GetSystemScanMgr");
    let PyRep::KeyVal(sm_kv) = &scan_mgr_rep else {
        panic!("Expected KeyVal from GetSystemScanMgr")
    };
    let scan_bound_id = sm_kv["boundID"].as_i64().unwrap() as u64;

    // 1. D-Scan via real V24 scanMgr.ConeScan(scanangle, scanRange, x, y, z)
    let dscan_360 = ctx
        .dispatch_rpc(
            &mut sess,
            "scanMgr",
            Some(scan_bound_id),
            "ConeScan",
            &[
                PyRep::Real(360.0),
                PyRep::Real(14.3 * AU),
                PyRep::Real(0.0),
                PyRep::Real(0.0),
                PyRep::Real(1.0),
            ],
            &kw,
            None,
        )
        .await
        .expect("ConeScan 360");
    let PyRep::List(hits_360) = &dscan_360 else {
        panic!("Expected List from ConeScan")
    };
    let has_hit = |hits: &[PyRep], item_id: i64| -> bool {
        hits.iter().any(|h| {
            let PyRep::KeyVal(kv) = h else {
                return false;
            };
            kv.iter()
                .any(|(k, v)| k == "id" && v.as_i64() == Some(item_id))
        })
    };
    assert!(
        has_hit(hits_360, 500_000_002),
        "Merlin must appear in 360 ConeScan"
    );
    assert!(
        !has_hit(hits_360, 500_000_003),
        "Combat Recon Huginn (group 906) must be immune to ConeScan"
    );
    assert!(
        !has_hit(hits_360, 500_000_004),
        "Cloaked ship must be immune to ConeScan"
    );

    // Narrow 15-degree cone directly forward (+Z) detects Merlin
    let dscan_15_fwd = ctx
        .dispatch_rpc(
            &mut sess,
            "scanMgr",
            Some(scan_bound_id),
            "ConeScan",
            &[
                PyRep::Real(15.0),
                PyRep::Real(14.3 * AU),
                PyRep::Real(0.0),
                PyRep::Real(0.0),
                PyRep::Real(1.0),
            ],
            &kw,
            None,
        )
        .await
        .expect("ConeScan 15 forward");
    let PyRep::List(hits_15_fwd) = &dscan_15_fwd else {
        panic!()
    };
    assert!(
        has_hit(hits_15_fwd, 500_000_002),
        "Merlin must appear in 15-deg forward ConeScan"
    );

    // Narrow 15-degree cone backward (-Z) misses Merlin, and short range (5,000 km < 10,000 km) also misses Merlin
    let dscan_15_back = ctx
        .dispatch_rpc(
            &mut sess,
            "scanMgr",
            Some(scan_bound_id),
            "ConeScan",
            &[PyRep::Real(15.0), PyRep::Real(14.3 * AU), PyRep::Real(0.0), PyRep::Real(0.0), PyRep::Real(-1.0)],
            &kw,
            None,
        )
        .await
        .expect("ConeScan 15 backward");
    let PyRep::List(hits_15_back) = &dscan_15_back else { panic!() };
    assert!(!has_hit(hits_15_back, 500_000_002), "Merlin must NOT appear in 15-deg backward ConeScan");

    let dscan_short = ctx
        .dispatch_rpc(
            &mut sess,
            "scanMgr",
            Some(scan_bound_id),
            "ConeScan",
            &[PyRep::Real(360.0), PyRep::Real(5_000_000.0), PyRep::Real(0.0), PyRep::Real(0.0), PyRep::Real(1.0)],
            &kw,
            None,
        )
        .await
        .expect("ConeScan short range");
    let PyRep::List(hits_short) = &dscan_short else { panic!() };
    assert!(!has_hit(hits_short, 500_000_002), "Merlin at 10,000 km must NOT appear in 5,000 km ConeScan");

    // 2. Load 8 Core Scanner Probe I (30013) into Core Probe Launcher I (17938) via dogmaIM.LoadAmmo
    let load_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "LoadAmmo",
            &[PyRep::Integer(ship_id), PyRep::List(vec![PyRep::Integer(550_001)]), PyRep::List(vec![PyRep::Integer(550_002)])],
            &kw,
            None,
        )
        .await
        .expect("LoadAmmo Core Scanner Probes");
    let PyRep::KeyVal(load_kv) = &load_rep else { panic!() };
    assert_eq!(load_kv["totalLoaded"], PyRep::Integer(8), "Core Probe Launcher I (0.8 m3) must load 8 probes");

    // Activate Core Probe Launcher I via dogmaIM.Activate to deploy an 8-probe Pinpoint formation
    let act_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(550_001), PyRep::String("launchScanners".into()), PyRep::None, PyRep::Integer(0)],
            &kw,
            None,
        )
        .await
        .expect("Activate Core Probe Launcher I");
    let PyRep::KeyVal(akv) = &act_rep else { panic!("Expected KeyVal from probe launch") };
    let PyRep::List(probe_id_reps) = &akv["probeIDs"] else { panic!("Expected probeIDs list") };
    let launched_probe_ids: Vec<i64> = probe_id_reps.iter().filter_map(|p| p.as_i64()).collect();
    assert_eq!(launched_probe_ids.len(), 8);

    // Verify all 8 probes exist as space entities in Destiny ballpark and have finite lifetime (max_flight_time_s > 0)
    for &pid in &launched_probe_ids {
        assert!(sys.get_ball_snapshot(pid).await.unwrap().is_some(), "Launched probe {pid} must exist in space");
    }
    {
        let pmap = ctx.active_scanner_probes.read().unwrap();
        for &pid in &launched_probe_ids {
            assert!(pmap[&pid].max_flight_time_s > 0.0, "Launched probe must have finite lifetime");
        }
    }

    // 2nd launch attempt when launcher is empty must fail with NoProbesLoaded
    let empty_err = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(550_001), PyRep::String("launchScanners".into()), PyRep::None, PyRep::Integer(0)],
            &kw,
            None,
        )
        .await
        .expect_err("Launch must fail when launcher is empty");
    assert_eq!(empty_err.code, "NoProbesLoaded");

    // 3. Probe Scan Math via real V24 scanMgr.RequestScans(probes_dict):
    // First test partial 2-probe wide-range scan (trilateration cap 0.49, deviation > 0.0), then 8-probe pinpoint to 100% (deviation == 0.0)
    let (wh_sig_id, wh_pos) = ctx.db.with_snapshot(|snap| {
        let sig = snap.cosmic_signatures[&30_000_142].iter().find(|s| s.sig_group == "Wormhole").unwrap();
        (sig.signature_id.clone(), sig.position)
    });

    let center = DVec3::new(wh_pos[0], wh_pos[1], wh_pos[2]);
    let partial_dict = PyRep::Dict(
        launched_probe_ids
            .iter()
            .enumerate()
            .map(|(idx, &pid)| {
                let p = if idx < 2 { center + DVec3::new((idx as f64) * 0.2 * AU, 0.0, 0.0) } else { DVec3::new(50.0 * AU, 0.0, 0.0) };
                (
                    PyRep::Integer(pid),
                    PyRep::keyval([
                        ("pos", PyRep::Tuple(vec![PyRep::Real(p.x), PyRep::Real(p.y), PyRep::Real(p.z)])),
                        ("scanRange", PyRep::Real(2.0 * AU)),
                    ]),
                )
            })
            .collect(),
    );
    let partial_rep = ctx
        .dispatch_rpc(&mut sess, "scanMgr", Some(scan_bound_id), "RequestScans", &[partial_dict], &kw, None)
        .await
        .expect("RequestScans partial 2-probe");
    let PyRep::List(partial_rows) = &partial_rep else { panic!() };
    let wh_partial = partial_rows
        .iter()
        .find_map(|r| match r {
            PyRep::KeyVal(kv) if kv.iter().any(|(k, v)| k == "id" && v.as_str() == Some(&wh_sig_id)) => Some(kv),
            _ => None,
        })
        .expect("Wormhole must appear in partial scan results");
    let partial_str = wh_partial.iter().find(|(k, _)| *k == "signalStrength").unwrap().1.as_f64().unwrap();
    let partial_dev = wh_partial.iter().find(|(k, _)| *k == "deviation").unwrap().1.as_f64().unwrap();
    assert!(partial_str > 0.0 && partial_str <= 0.49, "2-probe scan must be capped at <= 49% trilateration, got {partial_str}");
    assert!(partial_dev > 0.0, "Unresolved scan result must have positive deviation, got {partial_dev}");

    let scan_radius = 0.25 * AU;
    let d = scan_radius * 0.4;
    let offsets = [
        DVec3::ZERO,
        DVec3::new(d, 0.0, 0.0),
        DVec3::new(-d, 0.0, 0.0),
        DVec3::new(0.0, d, 0.0),
        DVec3::new(0.0, -d, 0.0),
        DVec3::new(0.0, 0.0, d),
        DVec3::new(0.0, 0.0, -d),
        DVec3::new(d * 0.5, d * 0.5, d * 0.5),
    ];
    let probes_dict = PyRep::Dict(
        launched_probe_ids
            .iter()
            .enumerate()
            .map(|(idx, &pid)| {
                let p = center + offsets[idx];
                (
                    PyRep::Integer(pid),
                    PyRep::keyval([
                        ("pos", PyRep::Tuple(vec![PyRep::Real(p.x), PyRep::Real(p.y), PyRep::Real(p.z)])),
                        ("scanRange", PyRep::Real(scan_radius)),
                    ]),
                )
            })
            .collect(),
    );

    let mut full_rows = Vec::new();
    for _ in 0..4 {
        let full_rep = ctx
            .dispatch_rpc(&mut sess, "scanMgr", Some(scan_bound_id), "RequestScans", &[probes_dict.clone()], &kw, None)
            .await
            .expect("RequestScans Pinpoint");
        let PyRep::List(rows) = full_rep else { panic!() };
        full_rows = rows;
    }
    let wh_full = full_rows
        .iter()
        .find_map(|r| match r {
            PyRep::KeyVal(kv) if kv.iter().any(|(k, v)| k == "id" && v.as_str() == Some(&wh_sig_id)) => Some(kv),
            _ => None,
        })
        .unwrap();
    assert_eq!(wh_full["signalStrength"], PyRep::Real(1.0));
    assert_eq!(wh_full["deviation"], PyRep::Real(0.0));

    let scan_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(scan_notifs.iter().any(|n| n.event == "OnSystemScanStarted"), "RequestScans must emit OnSystemScanStarted");
    assert!(scan_notifs.iter().any(|n| n.event == "OnSystemScanStopped"), "RequestScans must emit OnSystemScanStopped");

    // 4. Probe Reconnect, Recall, and Expiry after lifetime via real V24 scanMgr RPCs
    {
        let mut pmap = ctx.active_scanner_probes.write().unwrap();
        if let Some(p) = pmap.get_mut(&launched_probe_ids[0]) {
            p.state = 0;
        }
    }
    let recon_rep = ctx
        .dispatch_rpc(&mut sess, "scanMgr", Some(scan_bound_id), "ReconnectToLostProbes", &[], &kw, None)
        .await
        .expect("ReconnectToLostProbes");
    let PyRep::KeyVal(recon_kv) = &recon_rep else { panic!() };
    let PyRep::List(recon_list) = &recon_kv["reconnectedProbeIDs"] else { panic!() };
    assert!(recon_list.contains(&PyRep::Integer(launched_probe_ids[0])));

    // Recover 7 probes (0..7) back to ship (launcher / cargo) via RecoverProbes, leaving probe 7 in space to test lifetime expiry
    let recover_ids: Vec<PyRep> = launched_probe_ids[0..7]
        .iter()
        .map(|&id| PyRep::Integer(id))
        .collect();
    let recover_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "scanMgr",
            Some(scan_bound_id),
            "RecoverProbes",
            &[PyRep::List(recover_ids)],
            &kw,
            None,
        )
        .await
        .expect("RecoverProbes");
    let PyRep::KeyVal(rec_kv) = &recover_rep else {
        panic!()
    };
    let PyRep::List(recovered_list) = &rec_kv["recoveredProbeIDs"] else {
        panic!()
    };
    assert_eq!(
        recovered_list.len(),
        7,
        "Must recover all 7 targeted active probes"
    );
    assert_eq!(rec_kv["recoveredCount"], PyRep::Integer(7));

    // Advance virtual clock past max_flight_time_s (3600s) -> remaining probe in space expires!
    ctx.advance_virtual_clock(4_000);
    let expired_err = ctx
        .dispatch_rpc(&mut sess, "scanMgr", Some(scan_bound_id), "RequestScans", &[probes_dict], &kw, None)
        .await
        .expect_err("Unrecovered probe must expire after its max_flight_time_s (3600s) has elapsed");
    assert_eq!(expired_err.code, "NoActiveProbes");

    ctx.dispatch_rpc(
        &mut sess,
        "scanMgr",
        Some(scan_bound_id),
        "DestroyProbe",
        &[PyRep::Integer(launched_probe_ids[7])],
        &kw,
        None,
    )
    .await
    .expect("DestroyProbe");

    // Verify ship (launcher + cargo hold) now has 8 (initial remaining) + 7 (recovered) = 15 probes
    let ship_probes: i32 = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .filter(|i| i.location_id == ship_id && i.type_id == 30013)
            .map(|i| i.quantity)
            .sum()
    });
    assert_eq!(
        ship_probes, 15,
        "Ship must contain 15 Core Scanner Probes after recovering 7"
    );

    // 5. Acceleration Gate via real V24 keeper.ActivateAccelerationGate with SDE dungeon ship restrictions and gate keys
    let gate_ball = Ball {
        id: 500_000_100,
        type_id: 17831, // SDE Acceleration Gate (group 366)
        position: DVec3::new(1_000.0, 0.0, 0.0), // 1,000m from ship (within 2,500m)
        velocity: DVec3::ZERO,
        max_velocity: 0.0,
        inertia: 0.0,
        mass: 100_000_000.0,
        radius: 200.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let _ = sys.spawn_ball(gate_ball).await;

    ctx.db.with_mut_snapshot(|snap| {
        let mut gate_attrs = HashMap::new();
        gate_attrs.insert(1001, 111.0); // dungeonID = 111 (difficulty 1)
        gate_attrs.insert(1002, 2076.0); // requiredKeyTypeID = 2076 ("Gate Key")
        gate_attrs.insert(1003, 1.0); // consumeKey = true
        snap.items.insert(
            500_000_100,
            eve_db::InventoryItemRecord {
                item_id: 500_000_100,
                type_id: 17831,
                owner_id: 1,
                location_id: 30_000_142,
                flag: eve_db::InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: gate_attrs,
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });

    // Cruiser (Vexor, type 626, group 26) must be rejected by difficulty-1 dungeon gate (ShipRestrictedByDungeonGate)
    let cruiser_ball = Ball {
        id: 500_000_150,
        type_id: 626, // Vexor Cruiser (group 26)
        position: DVec3::new(1_200.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 210.0,
        inertia: 4.5,
        mass: 11_000_000.0,
        radius: 130.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let _ = sys.spawn_ball(cruiser_ball).await;
    sess.ship_id = Some(500_000_150);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().active_ship_id = 500_000_150;
    });
    let cruiser_err = ctx
        .dispatch_rpc(
            &mut sess,
            "keeper",
            None,
            "ActivateAccelerationGate",
            &[PyRep::Integer(500_000_100)],
            &kw,
            None,
        )
        .await
        .expect_err("Cruiser must be rejected by difficulty-1 Frigate/Destroyer dungeon gate");
    assert_eq!(cruiser_err.code, "ShipRestrictedByDungeonGate");

    // Capital ship (Avatar Titan, type 11567, group 30) must be rejected with ShipTooLargeForGate
    let titan_ball = Ball {
        id: 500_000_200,
        type_id: 11567, // Avatar Titan (group 30)
        position: DVec3::new(1_500.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 70.0,
        inertia: 0.05,
        mass: 2_400_000_000.0,
        radius: 7_000.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let _ = sys.spawn_ball(titan_ball).await;
    sess.ship_id = Some(500_000_200);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().active_ship_id = 500_000_200;
    });
    let capital_err = ctx
        .dispatch_rpc(
            &mut sess,
            "keeper",
            None,
            "ActivateAccelerationGate",
            &[PyRep::Integer(500_000_100)],
            &kw,
            None,
        )
        .await
        .expect_err("Titan must be rejected by acceleration gate");
    assert_eq!(capital_err.code, "ShipTooLargeForGate");

    // Switch back to Rifter Frigate (ship_id, type 587, group 25): allowed by dungeon difficulty 1,
    // verify OutsideGateActivationRange (> 2,500m from gate) and MissingGateKey before placing Gate Key (2076) in cargo
    let _ = sys.remove_ball(500_000_150).await;
    let _ = sys.remove_ball(500_000_200).await;
    sess.ship_id = Some(ship_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().active_ship_id = ship_id;
    });
    let _ = sys.set_ball_position(ship_id, DVec3::new(5_000.0, 0.0, 0.0)).await;
    let range_err = ctx
        .dispatch_rpc(&mut sess, "keeper", None, "ActivateAccelerationGate", &[PyRep::Integer(500_000_100)], &kw, None)
        .await
        .expect_err("Activating acceleration gate > 2,500m away must fail");
    assert_eq!(range_err.code, "OutsideGateActivationRange");
    let _ = sys.set_ball_position(ship_id, DVec3::ZERO).await;

    // Difficulty-2 SDE dungeon gate rejects Battleship (Scorpion, type 640, group 27) with ShipRestrictedByDungeonGate
    let diff2_did = *ctx.sde.dungeons.iter().find(|(_, d)| d.difficulty == Some(2)).unwrap().0;
    let bs_ball = Ball {
        id: 500_000_250, type_id: 640, position: DVec3::new(1_200.0, 0.0, 0.0), velocity: DVec3::ZERO,
        max_velocity: 120.0, inertia: 0.12, mass: 103_000_000.0, radius: 340.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    };
    let _ = sys.spawn_ball(bs_ball).await;
    sess.ship_id = Some(500_000_250);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().active_ship_id = 500_000_250;
        snap.items.get_mut(&500_000_100).unwrap().dynamic_attributes.insert(1001, diff2_did as f64);
    });
    let bs_err = ctx
        .dispatch_rpc(&mut sess, "keeper", None, "ActivateAccelerationGate", &[PyRep::Integer(500_000_100)], &kw, None)
        .await
        .expect_err("Battleship must be rejected by difficulty-2 dungeon gate");
    assert_eq!(bs_err.code, "ShipRestrictedByDungeonGate");
    let _ = sys.remove_ball(500_000_250).await;
    sess.ship_id = Some(ship_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().active_ship_id = ship_id;
        snap.items.get_mut(&500_000_100).unwrap().dynamic_attributes.insert(1001, 111.0);
    });

    let no_key_err = ctx
        .dispatch_rpc(
            &mut sess,
            "keeper",
            None,
            "ActivateAccelerationGate",
            &[PyRep::Integer(500_000_100)],
            &kw,
            None,
        )
        .await
        .expect_err("Activating locked acceleration gate without Gate Key in cargo must fail");
    assert_eq!(no_key_err.code, "MissingGateKey");

    // Place 1x SDE Gate Key (type 2076) in Rifter cargo and activate gate via keeper.ActivateAccelerationGate
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            550_099,
            eve_db::InventoryItemRecord {
                item_id: 550_099,
                type_id: 2076,
                owner_id: char_id,
                location_id: ship_id,
                flag: eve_db::InventoryFlag::Cargo,
                quantity: 1,
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

    let gate_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "keeper",
            None,
            "ActivateAccelerationGate",
            &[PyRep::Integer(500_000_100)],
            &kw,
            None,
        )
        .await
        .expect("keeper.ActivateAccelerationGate with Frigate and Gate Key");
    let PyRep::KeyVal(gate_kv) = &gate_rep else {
        panic!("Expected KeyVal from ActivateAccelerationGate")
    };
    assert_eq!(gate_kv["dungeonID"], PyRep::Integer(111));
    assert_eq!(gate_kv["requiredKeyTypeID"], PyRep::Integer(2076));
    assert_eq!(gate_kv["keyConsumed"], PyRep::Bool(true));

    // Verify Gate Key was consumed from cargo and ship entered warp to next pocket
    ctx.db.with_snapshot(|snap| {
        assert!(
            !snap.items.contains_key(&550_099),
            "Gate Key must be consumed from cargo upon activation"
        );
    });
    let (_, ship_warping) = sys.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!(
        matches!(ship_warping.mode, MovementMode::Warp { .. }),
        "Ship must enter Warp on acceleration gate activation"
    );
    for _ in 0..30 {
        let _ = sys.step_manual(1.0).await;
    }
    let (_, ship_landed) = sys.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!(
        ship_landed
            .position
            .distance(DVec3::new(500_000.0, 0.0, 0.0))
            < 2_500.0,
        "Ship must land at acceleration gate pocket destination, got {:?}",
        ship_landed.position
    );
}
