//! Integration test suite for `accessGroupBookmarkMgr` service (all 17 RPCs).
//!
//! Validates:
//! - Folder CRUD: AddFolder, UpdateFolder, DeleteFolder, GetFolderInfo, SearchFoldersWithAdminAccess
//! - Subfolder CRUD: CreateSubfolder, UpdateSubfolder, DeleteSubfolder
//! - Bookmark CRUD & Operations: BookmarkStaticLocation (8-tuple), GetMyActiveBookmarks (3-tuple),
//!   UpdateBookmark, CopyBookmarksToFolderAndSubfolder (2-tuple), MoveBookmarksToFolderAndSubfolder (2-tuple),
//!   DeleteBookmarks (list)
//! - Known folders subscription: AddToKnownFolders (3-tuple), UpdateKnownFolderState (3-tuple), RemoveFromKnownFolders

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::sync::Arc;

use crate::common;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;

const PILOT1: i64 = 90_000_001;
const PILOT2: i64 = 90_000_002;
const PILOT3: i64 = 90_000_003;
const CORP_ID: i64 = 98_000_001;

fn create_test_context() -> ServerContext {
    let sde = Arc::new(eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path()).expect("open sde"));
    let repo = Arc::new(eve_db::WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(sde, repo);
    common::pin_clock(&ctx);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch1) = snap.characters.get_mut(&PILOT1) { ch1.corporation_id = CORP_ID; }
        if let Some(ch2) = snap.characters.get_mut(&PILOT2) { ch2.corporation_id = CORP_ID; }
        if let Some(ch1) = snap.characters.get(&PILOT1).cloned() {
            let mut ch3 = ch1;
            ch3.character_id = PILOT3;
            ch3.name = "Intruder".into();
            ch3.corporation_id = 99_999_999;
            snap.characters.insert(PILOT3, ch3);
        }
        snap.access_groups.insert(1001, vec![PILOT1]);
        snap.access_groups.insert(1002, vec![PILOT1, PILOT2]);
        snap.access_groups.insert(1003, Vec::new());
        let member_roles = [(PILOT1, 1), (PILOT2, 0)].into_iter().collect();
        let members = [PILOT1, PILOT2].into_iter().collect();
        snap.corporations.insert(
            CORP_ID,
            eve_db::CorporationRecord {
                corporation_id: CORP_ID, name: "Alliance Command".into(),
                ticker: "ACMD".into(), ceo_id: PILOT1,
                members, member_roles, ..Default::default()
            },
        );
    });

    ctx
}

async fn rpc(ctx: &ServerContext, sess: &mut SessionState, svc: &str, method: &str, args: &[PyRep]) -> PyRep {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None)
        .await
        .unwrap_or_else(|e| panic!("{svc}.{method} failed: {e:?}"))
}

async fn rpc_err(ctx: &ServerContext, sess: &mut SessionState, svc: &str, method: &str, args: &[PyRep]) -> eve_proto::BridgeRpcError {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None)
        .await
        .expect_err(&format!("{svc}.{method} should have failed"))
}

async fn login_select(ctx: &ServerContext, char_id: i64, sid: u64) -> SessionState {
    let mut sess = SessionState::new(sid);
    if char_id == PILOT1 {
        common::login_pilot(ctx, &mut sess, "demo", "demo-secret").await;
        rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)]).await;
    } else if char_id == PILOT2 {
        common::login_pilot(ctx, &mut sess, "other", "other-secret").await;
        rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)]).await;
    } else {
        sess.user_id = Some(3);
        sess.character_id = Some(char_id);
    }
    sess
}

// parity: rpc:accessGroupBookmarkMgr.AddFolder
// parity: rpc:accessGroupBookmarkMgr.UpdateFolder
// parity: rpc:accessGroupBookmarkMgr.DeleteFolder
// parity: rpc:accessGroupBookmarkMgr.CreateSubfolder
// parity: rpc:accessGroupBookmarkMgr.UpdateSubfolder
// parity: rpc:accessGroupBookmarkMgr.DeleteSubfolder
// parity: rpc:accessGroupBookmarkMgr.GetFolderInfo
// parity: rpc:accessGroupBookmarkMgr.SearchFoldersWithAdminAccess
// parity: rpc:accessGroupBookmarkMgr.RemoveFromKnownFolders
#[tokio::test]
async fn test_access_group_bookmark_folders_and_subfolders_lifecycle() {
    let ctx = create_test_context();
    let mut sess1 = login_select(&ctx, PILOT1, 7101).await;
    sess1.corporation_id = Some(CORP_ID);

    let mut sess2 = login_select(&ctx, PILOT2, 7102).await;
    sess2.corporation_id = Some(CORP_ID);

    let mut sess3 = login_select(&ctx, PILOT3, 7103).await;
    sess3.corporation_id = Some(99_999_999);

    // 1. AddFolder: V24 7 args (isPersonal=False, name, desc, adminGID, manageGID, useGID, viewGID)
    // Pilot 1 creator/admin/manage (group 1001); Pilot 2 view rights (group 1002); Pilot 3 no rights.
    let add_res = rpc(&ctx, &mut sess1, "accessGroupBookmarkMgr", "AddFolder", &[
        PyRep::Bool(false), PyRep::String("Alliance Command Tactical".into()),
        PyRep::String("Tactical fleet folders".into()), PyRep::Integer(1001),
        PyRep::Integer(1001), PyRep::Integer(1002), PyRep::Integer(1002),
    ]).await;
    let PyRep::KeyVal(folder_kv) = &add_res else {
        panic!("expected KeyVal from AddFolder, got {add_res:?}");
    };
    let folder_id = folder_kv["folderID"].as_i64().expect("folderID integer");
    assert_eq!(folder_kv["folderName"], PyRep::String("Alliance Command Tactical".into()));
    assert_eq!(folder_kv["description"], PyRep::String("Tactical fleet folders".into()));
    assert_eq!(folder_kv["isShared"], PyRep::Bool(true));
    assert_eq!(folder_kv["isPersonal"], PyRep::Bool(false));
    assert_eq!(folder_kv["creatorID"], PyRep::Integer(PILOT1));
    assert_eq!(folder_kv["accessLevel"], PyRep::Integer(4));

    // Pilot 2 (NO corp roles) can also create a shared folder using access group 1002 where Pilot 2 is a member
    let p2_shared = rpc(&ctx, &mut sess2, "accessGroupBookmarkMgr", "AddFolder", &[
        PyRep::Bool(false), PyRep::String("Scout Recon Grid".into()),
        PyRep::String("No corp roles needed".into()), PyRep::Integer(1002),
        PyRep::Integer(1002), PyRep::Integer(1002), PyRep::Integer(1002),
    ]).await;
    let p2_sf_id = p2_shared.dict_get("folderID").and_then(|v| v.as_i64()).expect("p2_sf_id");
    // Pilot 3 (not in group 1001) cannot create a shared folder claiming adminGroupID 1001
    assert_eq!(rpc_err(&ctx, &mut sess3, "accessGroupBookmarkMgr", "AddFolder", &[
        PyRep::Bool(false), PyRep::String("Spoofed".into()), PyRep::String("".into()),
        PyRep::Integer(1001), PyRep::Integer(1001), PyRep::Integer(1001), PyRep::Integer(1001),
    ]).await.code, "BookmarkAccessDenied");
    // R-AGB-15: Unknown adminGroupID (999_999) is rejected with BookmarkAccessDenied and not auto-created
    assert_eq!(rpc_err(&ctx, &mut sess1, "accessGroupBookmarkMgr", "AddFolder", &[
        PyRep::Bool(false), PyRep::String("Unknown Group".into()), PyRep::String("".into()),
        PyRep::Integer(999_999), PyRep::Integer(999_999), PyRep::Integer(999_999), PyRep::Integer(999_999),
    ]).await.code, "BookmarkAccessDenied");
    assert!(!ctx.db.with_snapshot(|s| s.access_groups.contains_key(&999_999)), "unknown adminGroupID must not be inserted into access_groups");
    // R-AGB-15: Empty access group (1003) is rejected with BookmarkAccessDenied
    assert_eq!(rpc_err(&ctx, &mut sess1, "accessGroupBookmarkMgr", "AddFolder", &[
        PyRep::Bool(false), PyRep::String("Empty Group".into()), PyRep::String("".into()),
        PyRep::Integer(1003), PyRep::Integer(1003), PyRep::Integer(1003), PyRep::Integer(1003),
    ]).await.code, "BookmarkAccessDenied");
    // Personal folder with "Fleet" in name (must NOT appear in SearchFoldersWithAdminAccess)
    let p1_pers = rpc(&ctx, &mut sess1, "accessGroupBookmarkMgr", "AddFolder", &[
        PyRep::Bool(true), PyRep::String("Personal Fleet Notes".into()), PyRep::String("Private".into()),
        PyRep::None, PyRep::None, PyRep::None, PyRep::None,
    ]).await;
    let p1_pers_id = p1_pers.dict_get("folderID").and_then(|v| v.as_i64()).expect("p1_pers_id");

    // 2. Negative test (R-AGB-02): Unauthorized Pilot 3 cannot CreateSubfolder into folder_id
    let unauth_csf = rpc_err(&ctx, &mut sess3, "accessGroupBookmarkMgr", "CreateSubfolder", &[PyRep::Integer(folder_id), PyRep::String("Hostile Subfolder".into())]).await;
    assert_eq!(unauth_csf.code, "UserError");

    // 3. CreateSubfolder: Pilot 1 creates subfolder and verifies GetMyActiveBookmarks subfolders vs root folders
    let csf_res = rpc(&ctx, &mut sess1, "accessGroupBookmarkMgr", "CreateSubfolder", &[PyRep::Integer(folder_id), PyRep::String("Forward Operating Bases".into())]).await;
    let PyRep::KeyVal(csf_kv) = &csf_res else {
        panic!("expected KeyVal from CreateSubfolder, got {csf_res:?}");
    };
    let subfolder_id = csf_kv["subfolderID"].as_i64().expect("subfolderID");
    assert_eq!(csf_kv["parentFolderID"], PyRep::Integer(folder_id));
    assert_eq!(csf_kv["folderID"], PyRep::Integer(subfolder_id));
    assert_eq!(csf_kv["subfolderName"], PyRep::String("Forward Operating Bases".into()));
    assert_eq!(csf_kv["folderName"], PyRep::String("Alliance Command Tactical/Forward Operating Bases".into()));
    let active_after_sub = rpc(&ctx, &mut sess1, "accessGroupBookmarkMgr", "GetMyActiveBookmarks", &[]).await;
    let PyRep::Tuple(act_sub_parts) = &active_after_sub else { panic!("expected 3-tuple") };
    let PyRep::List(root_folders) = &act_sub_parts[0] else { panic!("expected root folders") };
    let PyRep::List(sub_folders) = &act_sub_parts[2] else { panic!("expected subfolders") };
    assert!(!root_folders.iter().any(|f| f.dict_get("folderID") == Some(&PyRep::Integer(subfolder_id))), "subfolder must not appear in root folders");
    let sf_entry = sub_folders.iter().find(|s| s.dict_get("subfolderID") == Some(&PyRep::Integer(subfolder_id))).expect("subfolder in subfolders sequence");
    assert_eq!(sf_entry.dict_get("folderID"), Some(&PyRep::Integer(folder_id)));
    assert_eq!(sf_entry.dict_get("subfolderName"), Some(&PyRep::String("Forward Operating Bases".into())));

    // R-AGB-11: Subfolder lifecycle guards (DeleteFolder/UpdateFolder/AddToKnownFolders/RemoveFromKnownFolders reject subfolder IDs)
    let del_sub_via_folder_err = rpc_err(&ctx, &mut sess1, "accessGroupBookmarkMgr", "DeleteFolder", &[PyRep::Integer(subfolder_id)]).await;
    assert_eq!(del_sub_via_folder_err.code, "UserError");
    let upd_sub_via_folder_err = rpc_err(&ctx, &mut sess1, "accessGroupBookmarkMgr", "UpdateFolder", &[PyRep::Integer(subfolder_id), PyRep::String("Sub".into())]).await;
    assert_eq!(upd_sub_via_folder_err.code, "UserError");
    let ak_sub_err = rpc_err(&ctx, &mut sess1, "accessGroupBookmarkMgr", "AddToKnownFolders", &[PyRep::Integer(subfolder_id), PyRep::Bool(true)]).await;
    assert_eq!(ak_sub_err.code, "UserError");
    let rk_sub_err = rpc_err(&ctx, &mut sess1, "accessGroupBookmarkMgr", "RemoveFromKnownFolders", &[PyRep::Integer(subfolder_id)]).await;
    assert_eq!(rk_sub_err.code, "UserError");

    // 4. Negative test (R-AGB-01): Unauthorized Pilot 3 cannot UpdateSubfolder
    let unauth_usf = rpc_err(&ctx, &mut sess3, "accessGroupBookmarkMgr", "UpdateSubfolder", &[PyRep::Integer(folder_id), PyRep::Integer(subfolder_id), PyRep::String("Hijacked".into())]).await;
    assert_eq!(unauth_usf.code, "UserError");

    // 5. UpdateSubfolder: V24 3 args (folderID, subfolderID, name) by authorized manager
    let usf_res = rpc(&ctx, &mut sess1, "accessGroupBookmarkMgr", "UpdateSubfolder", &[PyRep::Integer(folder_id), PyRep::Integer(subfolder_id), PyRep::String("Forward Staging Bases".into())]).await;
    let PyRep::KeyVal(usf_kv) = &usf_res else {
        panic!("expected KeyVal from UpdateSubfolder, got {usf_res:?}");
    };
    assert_eq!(usf_kv["subfolderID"], PyRep::Integer(subfolder_id));
    assert_eq!(usf_kv["folderID"], PyRep::Integer(subfolder_id));
    assert_eq!(usf_kv["parentFolderID"], PyRep::Integer(folder_id));
    assert_eq!(usf_kv["subfolderName"], PyRep::String("Forward Staging Bases".into()));
    assert_eq!(usf_kv["folderName"], PyRep::String("Alliance Command Tactical/Forward Staging Bases".into()));

    // 6. UpdateFolder: V24 7 args (folderID, name, desc, adminGID, manageGID, useGID, viewGID)
    let _ = ctx.drain_notifications_for_char(PILOT1);
    let upd_f_res = rpc(&ctx, &mut sess1, "accessGroupBookmarkMgr", "UpdateFolder", &[
        PyRep::Integer(folder_id), PyRep::String("Fleet Staging Tactical".into()),
        PyRep::String("Updated staging desc".into()), PyRep::Integer(1001),
        PyRep::Integer(1001), PyRep::Integer(1002), PyRep::Integer(1002),
    ]).await;
    assert_eq!(upd_f_res, PyRep::Integer(4));
    assert!(ctx.drain_notifications_for_char(PILOT1).iter().any(|n| n.event == "OnSharedBookmarksFolderUpdated"));

    // Verify AddFolder + UpdateFolder + CreateSubfolder + UpdateSubfolder persist across SQLite reload
    let ctx_reloaded = crate::sp6_logoff_and_server_tick::persist_and_reopen_sqlite(&ctx, "agb_folder_crud_reload");
    let mut sess1_re = login_select(&ctx_reloaded, PILOT1, 7111).await;
    let finfo_re = rpc(&ctx_reloaded, &mut sess1_re, "accessGroupBookmarkMgr", "GetFolderInfo", &[PyRep::Integer(folder_id)]).await;
    assert_eq!(finfo_re.dict_get("folderName"), Some(&PyRep::String("Fleet Staging Tactical".into())));
    assert_eq!(finfo_re.dict_get("description"), Some(&PyRep::String("Updated staging desc".into())));
    assert_eq!(finfo_re.dict_get("adminGroupID"), Some(&PyRep::Integer(1001)));
    assert_eq!(finfo_re.dict_get("viewGroupID"), Some(&PyRep::Integer(1002)));

    // Negative test: Pilot 2 (not admin) cannot UpdateFolder
    let unauth_upd = rpc_err(&ctx, &mut sess2, "accessGroupBookmarkMgr", "UpdateFolder", &[
        PyRep::Integer(folder_id), PyRep::String("Unauthorized Rename".into()),
        PyRep::String("".into()), PyRep::Integer(1002), PyRep::Integer(1002),
        PyRep::Integer(1002), PyRep::Integer(1002),
    ]).await;
    assert_eq!(unauth_upd.code, "UserError");

    // 7. GetFolderInfo: V24 1 arg (folderID) -> returns exact 1 folder record keyval
    let finfo_res = rpc(&ctx, &mut sess1, "accessGroupBookmarkMgr", "GetFolderInfo", &[PyRep::Integer(folder_id)]).await;
    let PyRep::KeyVal(fi_kv) = &finfo_res else {
        panic!("expected KeyVal from GetFolderInfo, got {finfo_res:?}");
    };
    assert_eq!(fi_kv.get("folderID"), Some(&PyRep::Integer(folder_id)));
    assert_eq!(fi_kv.get("folderName"), Some(&PyRep::String("Fleet Staging Tactical".into())));
    assert_eq!(fi_kv.get("description"), Some(&PyRep::String("Updated staging desc".into())));
    assert_eq!(fi_kv.get("creatorID"), Some(&PyRep::Integer(PILOT1)));
    assert_eq!(fi_kv.get("isPersonal"), Some(&PyRep::Bool(false)));
    assert_eq!(fi_kv.get("adminGroupID"), Some(&PyRep::Integer(1001)));
    assert_eq!(fi_kv.get("viewGroupID"), Some(&PyRep::Integer(1002)));
    assert_eq!(fi_kv.get("accessLevel"), Some(&PyRep::Integer(4)));

    // Negative test: unknown or unauthorized folderID raises UserError
    assert_eq!(rpc_err(&ctx, &mut sess1, "accessGroupBookmarkMgr", "GetFolderInfo", &[PyRep::Integer(999_999_999)]).await.code, "UserError");
    assert_eq!(rpc_err(&ctx, &mut sess3, "accessGroupBookmarkMgr", "GetFolderInfo", &[PyRep::Integer(folder_id)]).await.code, "UserError");

    // 8. SearchFoldersWithAdminAccess: case-insensitive name filter, shared folders only, admin access only
    let search_res = rpc(&ctx, &mut sess1, "accessGroupBookmarkMgr", "SearchFoldersWithAdminAccess", &[PyRep::String("fleet".into())]).await;
    let PyRep::Rowset { rows: sf_rows, .. } = &search_res else {
        panic!("expected Rowset from SearchFoldersWithAdminAccess, got {search_res:?}");
    };
    assert!(sf_rows.iter().any(|r| r.first() == Some(&PyRep::Integer(folder_id))), "matching admin shared folder returned");
    assert!(!sf_rows.iter().any(|r| r.first() == Some(&PyRep::Integer(p1_pers_id))), "personal folder must NOT be returned");
    assert!(!sf_rows.iter().any(|r| r.first() == Some(&PyRep::Integer(p2_sf_id))), "non-matching shared folder must NOT be returned");

    // Pilot 2 (has view on folder_id, but NOT admin) does NOT find folder_id, but DOES find p2_sf_id when searching "scout"
    let search_p2 = rpc(&ctx, &mut sess2, "accessGroupBookmarkMgr", "SearchFoldersWithAdminAccess", &[PyRep::String("fleet".into())]).await;
    let PyRep::Rowset { rows: p2_rows, .. } = &search_p2 else { panic!() };
    assert!(!p2_rows.iter().any(|r| r.first() == Some(&PyRep::Integer(folder_id))));
    let search_p2_own = rpc(&ctx, &mut sess2, "accessGroupBookmarkMgr", "SearchFoldersWithAdminAccess", &[PyRep::String("scout".into())]).await;
    let PyRep::Rowset { rows: p2_own_rows, .. } = &search_p2_own else { panic!() };
    assert!(p2_own_rows.iter().any(|r| r.first() == Some(&PyRep::Integer(p2_sf_id))));

    // 9. RemoveFromKnownFolders: pilot 2 unlinks folder
    let rem_res = rpc(
        &ctx,
        &mut sess2,
        "accessGroupBookmarkMgr",
        "RemoveFromKnownFolders",
        &[PyRep::Integer(folder_id)],
    )
    .await;
    assert_eq!(
        rem_res,
        PyRep::keyval([
            ("folderID", PyRep::Integer(folder_id)),
            ("subscribed", PyRep::Bool(false)),
        ])
    );

    // 10. DeleteSubfolder: V24 2 args (folderID, subfolderID)
    let dsf_res = rpc(
        &ctx,
        &mut sess1,
        "accessGroupBookmarkMgr",
        "DeleteSubfolder",
        &[PyRep::Integer(folder_id), PyRep::Integer(subfolder_id)],
    )
    .await;
    assert_eq!(dsf_res, PyRep::List(vec![PyRep::Integer(subfolder_id)]));

    // 11. DeleteFolder: V24 1 arg (folderID)
    let df_res = rpc(
        &ctx,
        &mut sess1,
        "accessGroupBookmarkMgr",
        "DeleteFolder",
        &[PyRep::Integer(folder_id)],
    )
    .await;
    assert_eq!(
        df_res,
        PyRep::keyval([
            ("folderID", PyRep::Integer(folder_id)),
            ("deleted", PyRep::Bool(true)),
        ])
    );
    assert!(ctx.db.with_snapshot(|s| !s.bookmark_folders.contains_key(&folder_id) && !s.bookmark_subfolder_parents.contains_key(&subfolder_id)));
}

// parity: rpc:accessGroupBookmarkMgr.BookmarkStaticLocation
// parity: rpc:accessGroupBookmarkMgr.GetMyActiveBookmarks
// parity: rpc:accessGroupBookmarkMgr.UpdateBookmark
// parity: rpc:accessGroupBookmarkMgr.CopyBookmarksToFolderAndSubfolder
// parity: rpc:accessGroupBookmarkMgr.MoveBookmarksToFolderAndSubfolder
// parity: rpc:accessGroupBookmarkMgr.AddToKnownFolders
// parity: rpc:accessGroupBookmarkMgr.UpdateKnownFolderState
// parity: rpc:accessGroupBookmarkMgr.DeleteBookmarks
#[tokio::test]
async fn test_access_group_bookmarks_creation_ops_and_subscriptions() {
    let ctx = create_test_context();
    let mut sess1 = login_select(&ctx, PILOT1, 7201).await;
    sess1.corporation_id = Some(CORP_ID);

    let mut sess2 = login_select(&ctx, PILOT2, 7202).await;
    sess2.corporation_id = Some(CORP_ID);

    let mut sess3 = login_select(&ctx, PILOT3, 7203).await;
    sess3.corporation_id = Some(99_999_999);

    rpc(&ctx, &mut sess1, "stationSvc", "Undock", &[]).await;
    let ship_id = sess1.ship_id.expect("ship_id after undock");
    let sys_id = sess1.solar_system_id.expect("solar_system_id after undock");

    // Create shared folder and subfolder using V24 args
    let sf_res = rpc(
        &ctx,
        &mut sess1,
        "accessGroupBookmarkMgr",
        "AddFolder",
        &[
            PyRep::Bool(false), PyRep::String("Tactical Gate Pings".into()),
            PyRep::String("Gate tactical locations".into()), PyRep::Integer(1001),
            PyRep::Integer(1001), PyRep::Integer(1002), PyRep::Integer(1002),
        ],
    )
    .await;
    let PyRep::KeyVal(sf_kv) = &sf_res else { panic!("expected KeyVal") };
    let folder_id = sf_kv["folderID"].as_i64().expect("folderID");

    let sub_res = rpc(
        &ctx,
        &mut sess1,
        "accessGroupBookmarkMgr",
        "CreateSubfolder",
        &[PyRep::Integer(folder_id), PyRep::String("Primary Perimeters".into())],
    )
    .await;
    let PyRep::KeyVal(sub_kv) = &sub_res else { panic!("expected KeyVal") };
    let subfolder_id = sub_kv["subfolderID"].as_i64().expect("subfolderID");

    // R-AGB-13: foreign subfolder rejection in BookmarkStaticLocation
    let mut bad_kw = HashMap::new();
    bad_kw.insert("subfolderID".into(), PyRep::Integer(999_999));
    let foreign_sf_err = ctx.dispatch_rpc(&mut sess1, "accessGroupBookmarkMgr", None, "BookmarkStaticLocation", &[PyRep::Integer(ship_id), PyRep::Integer(folder_id), PyRep::String("Safe".into()), PyRep::String("".into()), PyRep::Integer(0)], &bad_kw, None).await.expect_err("foreign sf");
    assert_eq!(foreign_sf_err.code, "UserError");

    // 1. BookmarkStaticLocation: V24 call with 5 positional args + kwargs["subfolderID"]
    // Returns 8-tuple (bookmarkID, itemID/systemID, typeID, x, y, z, locationID, folderID)
    let mut kw = HashMap::new();
    kw.insert("subfolderID".into(), PyRep::Integer(subfolder_id));
    let bm_res = ctx
        .dispatch_rpc(
            &mut sess1,
            "accessGroupBookmarkMgr",
            None,
            "BookmarkStaticLocation",
            &[
                PyRep::Integer(ship_id), PyRep::Integer(folder_id),
                PyRep::String("Perimeter Safe 1".into()),
                PyRep::String("150km off warp beacon".into()),
                PyRep::Integer(0),
            ],
            &kw,
            None,
        )
        .await
        .expect("BookmarkStaticLocation");

    let (bm_id, loc_id, f_id) = match &bm_res {
        PyRep::Tuple(parts) if parts.len() == 8 => (
            parts[0].as_i64().expect("bm_id"),
            parts[6].as_i64().expect("locationID"),
            parts[7].as_i64().expect("folderID"),
        ),
        other => panic!("expected 8-tuple from BookmarkStaticLocation, got {other:?}"),
    };
    assert!(bm_id > 0);
    assert_eq!(loc_id, sys_id as i64);
    assert_eq!(f_id, folder_id);

    // 2. GetMyActiveBookmarks: returns 3-tuple (folders, bookmarks, subfolders)
    let act_res = rpc(&ctx, &mut sess1, "accessGroupBookmarkMgr", "GetMyActiveBookmarks", &[]).await;
    let PyRep::Tuple(act_parts) = &act_res else {
        panic!("expected 3-tuple from GetMyActiveBookmarks, got {act_res:?}");
    };
    assert_eq!(act_parts.len(), 3);
    let PyRep::List(bms_list) = &act_parts[1] else { panic!("expected bookmarks list in [1]") };
    let bm_entry = bms_list
        .iter()
        .find(|b| b.dict_get("bookmarkID") == Some(&PyRep::Integer(bm_id)))
        .expect("created bookmark must be in active bookmarks list");
    assert_eq!(bm_entry.dict_get("label"), Some(&PyRep::String("Perimeter Safe 1".into())));
    assert_eq!(bm_entry.dict_get("folderID"), Some(&PyRep::Integer(folder_id)));
    assert_eq!(bm_entry.dict_get("subfolderID"), Some(&PyRep::Integer(subfolder_id)));
    assert_eq!(bm_entry.dict_get("isShared"), Some(&PyRep::Bool(true)));

    // 3. UpdateBookmark: V24 7 args (bookmarkID, ownerID, folderID, subfolderID, memo, label, expiryHours)
    let upd_bm_res = rpc(
        &ctx,
        &mut sess1,
        "accessGroupBookmarkMgr",
        "UpdateBookmark",
        &[
            PyRep::Integer(bm_id), PyRep::Integer(PILOT1), PyRep::Integer(folder_id),
            PyRep::Integer(subfolder_id), PyRep::String("Verified safe coordinates".into()),
            PyRep::String("Perimeter Safe 1 (Verified)".into()), PyRep::Integer(0),
        ],
    )
    .await;
    assert_eq!(
        upd_bm_res,
        PyRep::keyval([
            ("bookmarkID", PyRep::Integer(bm_id)),
            ("updated", PyRep::Bool(true)),
            ("expiryUnix", PyRep::Integer(0)),
        ])
    );

    // R-AGB-13: foreign subfolder rejection on update
    let bad_upd_sf = rpc_err(
        &ctx,
        &mut sess1,
        "accessGroupBookmarkMgr",
        "UpdateBookmark",
        &[PyRep::Integer(bm_id), PyRep::Integer(PILOT1), PyRep::Integer(folder_id), PyRep::Integer(999_999), PyRep::String("".into()), PyRep::String("".into()), PyRep::Integer(0)],
    ).await;
    assert_eq!(bad_upd_sf.code, "UserError");

    // Overflow resilience check (R-AGB-06): large expiry does not panic
    let oflow_res = rpc(
        &ctx,
        &mut sess1,
        "accessGroupBookmarkMgr",
        "UpdateBookmark",
        &[
            PyRep::Integer(bm_id), PyRep::Integer(PILOT1), PyRep::Integer(folder_id),
            PyRep::Integer(subfolder_id), PyRep::String("memo".into()),
            PyRep::String("label".into()), PyRep::Integer(i64::MAX),
        ],
    )
    .await;
    let PyRep::KeyVal(oflow_kv) = oflow_res else { panic!() };
    assert_eq!(oflow_kv.get("updated"), Some(&PyRep::Bool(true)));

    // 4. Create personal target folder, then CopyBookmarksToFolderAndSubfolder
    // V24 4 args: (folderID, subfolderID, bookmarkIDs, target)
    let p_res = rpc(
        &ctx,
        &mut sess1,
        "accessGroupBookmarkMgr",
        "AddFolder",
        &[
            PyRep::Bool(true), PyRep::String("Personal Safes".into()),
            PyRep::String("Private locations".into()),
            PyRep::None, PyRep::None, PyRep::None, PyRep::None,
        ],
    )
    .await;
    let PyRep::KeyVal(p_kv) = &p_res else { panic!() };
    let p_folder_id = p_kv["folderID"].as_i64().expect("p_folder_id");

    // R-AGB-13: foreign subfolder rejection on copy
    let foreign_cp_err = rpc_err(&ctx, &mut sess1, "accessGroupBookmarkMgr", "CopyBookmarksToFolderAndSubfolder", &[PyRep::Integer(folder_id), PyRep::Integer(999_999), PyRep::List(vec![PyRep::Integer(bm_id)]), PyRep::None]).await;
    assert_eq!(foreign_cp_err.code, "UserError");

    let copy_res = rpc(
        &ctx,
        &mut sess1,
        "accessGroupBookmarkMgr",
        "CopyBookmarksToFolderAndSubfolder",
        &[
            PyRep::Integer(p_folder_id),
            PyRep::None,
            PyRep::List(vec![PyRep::Integer(bm_id)]),
            PyRep::None,
        ],
    )
    .await;
    let copied_id = match &copy_res {
        PyRep::Tuple(parts) if parts.len() == 2 => {
            let PyRep::Dict(entries) = &parts[0] else { panic!("expected dict in [0]") };
            let (cid, _) = &entries[0];
            assert_eq!(parts[1], PyRep::None);
            cid.as_i64().unwrap()
        }
        other => panic!("expected 2-tuple from CopyBookmarksToFolderAndSubfolder, got {other:?}"),
    };
    assert_ne!(copied_id, bm_id);
    // R-AGB-13: copy to personal folder recomputes is_shared = false
    assert_eq!(ctx.db.with_snapshot(|s| s.bookmarks[&copied_id].is_shared), false);

    // 5. MoveBookmarksToFolderAndSubfolder: V24 4 args (folderID, subfolderID, bookmarkIDs, target)
    let move_res = rpc(
        &ctx,
        &mut sess1,
        "accessGroupBookmarkMgr",
        "MoveBookmarksToFolderAndSubfolder",
        &[PyRep::Integer(folder_id), PyRep::Integer(subfolder_id), PyRep::List(vec![PyRep::Integer(copied_id)]), PyRep::None],
    )
    .await;
    let PyRep::Tuple(parts) = &move_res else { panic!() };
    let PyRep::List(items) = &parts[0] else { panic!() };
    assert_eq!(items.len(), 1);
    // R-AGB-13: move to shared folder recomputes is_shared = true
    assert_eq!(ctx.db.with_snapshot(|s| s.bookmarks[&copied_id].is_shared), true);

    // P2: Moving a shared bookmark to a personal folder updates owner_id to the mover
    ctx.db.with_mut_snapshot(|s| {
        if let Some(b) = s.bookmarks.get_mut(&copied_id) {
            b.owner_id = PILOT2; // simulate created by another character
        }
    });
    let move_p_res = rpc(
        &ctx,
        &mut sess1,
        "accessGroupBookmarkMgr",
        "MoveBookmarksToFolderAndSubfolder",
        &[PyRep::Integer(p_folder_id), PyRep::None, PyRep::List(vec![PyRep::Integer(copied_id)]), PyRep::None],
    ).await;
    assert!(matches!(move_p_res, PyRep::Tuple(_)));
    assert_eq!(ctx.db.with_snapshot(|s| s.bookmarks[&copied_id].owner_id), PILOT1);
    assert_eq!(ctx.db.with_snapshot(|s| s.bookmarks[&copied_id].is_shared), false);

    // 6. AddToKnownFolders: Pilot 2 (has view group) subscribes
    // Client expectation: 3-tuple (folder_keyval, [bookmark_keyvals], [subfolder_keyvals])
    let add_known_res = rpc(&ctx, &mut sess2, "accessGroupBookmarkMgr", "AddToKnownFolders", &[PyRep::Integer(folder_id), PyRep::Bool(true)]).await;
    let PyRep::Tuple(ak_parts) = &add_known_res else { panic!("expected 3-tuple from AddToKnownFolders, got {add_known_res:?}") };
    assert_eq!(ak_parts.len(), 3);
    let PyRep::KeyVal(ak_folder) = &ak_parts[0] else { panic!("expected KeyVal in [0]") };
    assert_eq!(ak_folder.get("isActive"), Some(&PyRep::Bool(true)));
    assert_eq!(ak_folder.get("folderID"), Some(&PyRep::Integer(folder_id)));
    let PyRep::List(ak_bms) = &ak_parts[1] else { panic!("expected bookmarks list in [1]") };
    let PyRep::List(ak_subs) = &ak_parts[2] else { panic!("expected subfolders list in [2]") };
    assert!(ak_bms.iter().any(|b| b.dict_get("bookmarkID") == Some(&PyRep::Integer(bm_id))));
    assert!(ak_subs.iter().any(|s| s.dict_get("subfolderID") == Some(&PyRep::Integer(subfolder_id))));

    // Negative test: Pilot 3 (unauthorized, no view group) denied AddToKnownFolders (R-AGB-03)
    let unauth_ak = rpc_err(&ctx, &mut sess3, "accessGroupBookmarkMgr", "AddToKnownFolders", &[PyRep::Integer(folder_id), PyRep::Bool(true)]).await;
    assert_eq!(unauth_ak.code, "UserError");

    // 7. UpdateKnownFolderState: Pilot 2 toggles active state
    // Setting False hides the folder bookmarks from GetMyActiveBookmarks and returns empty bookmark/subfolder lists (R-AGB-03)
    let upd_known_false = rpc(&ctx, &mut sess2, "accessGroupBookmarkMgr", "UpdateKnownFolderState", &[PyRep::Integer(folder_id), PyRep::Bool(false)]).await;
    let PyRep::Tuple(uk_false_parts) = &upd_known_false else { panic!() };
    assert_eq!(uk_false_parts.len(), 3);
    assert_eq!(uk_false_parts[0].dict_get("isActive"), Some(&PyRep::Bool(false)));
    assert_eq!(uk_false_parts[1], PyRep::List(vec![]), "deactivated folder returns empty bookmarks list");
    assert_eq!(uk_false_parts[2], PyRep::List(vec![]), "deactivated folder returns empty subfolders list");
    let p2_bms_inactive = rpc(&ctx, &mut sess2, "accessGroupBookmarkMgr", "GetMyActiveBookmarks", &[]).await;
    let PyRep::Tuple(p2_inactive_parts) = &p2_bms_inactive else { panic!() };
    let PyRep::List(p2_inact_list) = &p2_inactive_parts[1] else { panic!() };
    assert!(!p2_inact_list.iter().any(|b| b.dict_get("folderID") == Some(&PyRep::Integer(folder_id))));
    let ctx_inact_re = crate::sp6_logoff_and_server_tick::persist_and_reopen_sqlite(&ctx, "agb_known_inactive_reload");
    assert_eq!(ctx_inact_re.db.with_snapshot(|s| s.bookmark_folder_known_by.get(&(folder_id, PILOT2)).copied()), Some(false));

    // Setting True restores the folder bookmarks and persists across SQLite reload
    let upd_known_true = rpc(&ctx, &mut sess2, "accessGroupBookmarkMgr", "UpdateKnownFolderState", &[PyRep::Integer(folder_id), PyRep::Bool(true)]).await;
    let PyRep::Tuple(uk_true_parts) = &upd_known_true else { panic!() };
    assert_eq!(uk_true_parts.len(), 3);
    assert_eq!(uk_true_parts[0].dict_get("isActive"), Some(&PyRep::Bool(true)));
    let p2_bms_active = rpc(&ctx, &mut sess2, "accessGroupBookmarkMgr", "GetMyActiveBookmarks", &[]).await;
    let PyRep::Tuple(p2_active_parts) = &p2_bms_active else { panic!() };
    let PyRep::List(p2_act_list) = &p2_active_parts[1] else { panic!() };
    assert!(p2_act_list.iter().any(|b| b.dict_get("folderID") == Some(&PyRep::Integer(folder_id))));
    let ctx_act_re = crate::sp6_logoff_and_server_tick::persist_and_reopen_sqlite(&ctx, "agb_known_active_reload");
    let mut sess2_re = login_select(&ctx_act_re, PILOT2, 7212).await;
    sess2_re.corporation_id = Some(CORP_ID);
    let p2_re_bms = rpc(&ctx_act_re, &mut sess2_re, "accessGroupBookmarkMgr", "GetMyActiveBookmarks", &[]).await;
    let PyRep::Tuple(p2_re_parts) = &p2_re_bms else { panic!() };
    let PyRep::List(p2_re_list) = &p2_re_parts[1] else { panic!() };
    assert!(p2_re_list.iter().any(|b| b.dict_get("bookmarkID") == Some(&PyRep::Integer(bm_id)) && b.dict_get("folderID") == Some(&PyRep::Integer(folder_id))), "bookmark folder assignment and active subscription survive SQLite reload");

    // Negative test: calling UpdateKnownFolderState on a folder not in known folders raises UserError (R-AGB-03)
    let not_known_err = rpc_err(&ctx, &mut sess3, "accessGroupBookmarkMgr", "UpdateKnownFolderState", &[PyRep::Integer(folder_id), PyRep::Bool(true)]).await;
    assert_eq!(not_known_err.code, "UserError");

    // 8. DeleteBookmarks: delete bookmarks (V24 2 args: bookmarkIDs, folderID)
    // R-AGB-14: Mismatched folderID argument is rejected with UserError
    let del_mismatch_err = rpc_err(
        &ctx,
        &mut sess1,
        "accessGroupBookmarkMgr",
        "DeleteBookmarks",
        &[PyRep::List(vec![PyRep::Integer(copied_id)]), PyRep::Integer(folder_id)],
    )
    .await;
    assert_eq!(del_mismatch_err.code, "UserError");

    // R-AGB-14: Deleting personal bookmark does NOT send OnSharedBookmarksFolderUpdated to corp
    let _ = ctx.drain_notifications_for_char(PILOT2);
    let del_p_res = rpc(
        &ctx,
        &mut sess1,
        "accessGroupBookmarkMgr",
        "DeleteBookmarks",
        &[PyRep::List(vec![PyRep::Integer(copied_id)]), PyRep::Integer(p_folder_id)],
    )
    .await;
    assert_eq!(del_p_res, PyRep::List(vec![PyRep::Integer(copied_id)]));
    assert!(
        !ctx.drain_notifications_for_char(PILOT2).iter().any(|n| n.event == "OnSharedBookmarksFolderUpdated"),
        "Deleting personal bookmark must not emit OnSharedBookmarksFolderUpdated"
    );

    // Deleting shared bookmark DOES emit OnSharedBookmarksFolderUpdated to corp
    let _ = ctx.drain_notifications_for_char(PILOT2);
    let del_shared_res = rpc(
        &ctx,
        &mut sess1,
        "accessGroupBookmarkMgr",
        "DeleteBookmarks",
        &[PyRep::List(vec![PyRep::Integer(bm_id)]), PyRep::Integer(folder_id)],
    )
    .await;
    assert_eq!(del_shared_res, PyRep::List(vec![PyRep::Integer(bm_id)]));
    assert!(
        ctx.drain_notifications_for_char(PILOT2).iter().any(|n| n.event == "OnSharedBookmarksFolderUpdated"),
        "Deleting shared bookmark must emit OnSharedBookmarksFolderUpdated"
    );

    let act_post_del = rpc(&ctx, &mut sess1, "accessGroupBookmarkMgr", "GetMyActiveBookmarks", &[]).await;
    let PyRep::Tuple(post_parts) = &act_post_del else { panic!() };
    let PyRep::List(post_bms) = &post_parts[1] else { panic!() };
    assert!(!post_bms.iter().any(|b| b.dict_get("bookmarkID") == Some(&PyRep::Integer(bm_id))));
    assert!(!post_bms.iter().any(|b| b.dict_get("bookmarkID") == Some(&PyRep::Integer(copied_id))));

    // 9. Temporary bookmark expiry (3h vs 48h, SQLite persistence before sweep, and server tick sweep `feat:bookmarks.bm-expiry`)
    // parity: feat:bookmarks.bm-expiry
    eve_testkit::attest(&["feat:bookmarks.bm-expiry"]);
    let exp_create = rpc(&ctx, &mut sess1, "accessGroupBookmarkMgr", "BookmarkStaticLocation", &[PyRep::Integer(ship_id), PyRep::Integer(folder_id), PyRep::String("Temp 3h Safe".into()), PyRep::String("3h".into()), PyRep::Integer(3)]).await;
    let PyRep::Tuple(exp_parts) = &exp_create else { panic!() };
    let temp_bm_id = exp_parts[0].as_i64().unwrap();
    let ctx_exp_re = crate::sp6_logoff_and_server_tick::persist_and_reopen_sqlite(&ctx, "agb_temp_bm_expiry_reload");
    common::pin_clock(&ctx_exp_re);
    assert!(ctx_exp_re.db.with_snapshot(|s| s.bookmark_expiries.get(&temp_bm_id).copied()).unwrap_or(0) > 0, "expiry_unix must survive SQLite reload");
    let mut sess1_exp_re = login_select(&ctx_exp_re, PILOT1, 7219).await;
    sess1_exp_re.corporation_id = Some(CORP_ID);
    let pre_exp_bms = rpc(&ctx_exp_re, &mut sess1_exp_re, "accessGroupBookmarkMgr", "GetMyActiveBookmarks", &[]).await;
    let PyRep::Tuple(pre_exp_parts) = &pre_exp_bms else { panic!() };
    let PyRep::List(pre_exp_list) = &pre_exp_parts[1] else { panic!() };
    assert!(pre_exp_list.iter().any(|b| b.dict_get("bookmarkID") == Some(&PyRep::Integer(temp_bm_id))), "unexpired temp bookmark must exist after SQLite reload");
    ctx_exp_re.advance_secs(3 * 3600 + 60);
    assert!(!ctx_exp_re.db.with_snapshot(|s| s.bookmarks.contains_key(&temp_bm_id)) && ctx_exp_re.db.with_snapshot(|s| s.bookmark_expiries.contains_key(&temp_bm_id)));
    let _ = (ctx.drain_notifications_for_char(PILOT1), ctx.drain_notifications_for_char(PILOT2));
    ctx.advance_secs(3 * 3600 + 60);
    assert!(!ctx.db.with_snapshot(|s| s.bookmarks.contains_key(&temp_bm_id)));
    assert!(ctx.drain_notifications_for_char(PILOT1).iter().any(|n| n.event == "OnRefreshBookmarks") && ctx.drain_notifications_for_char(PILOT2).iter().any(|n| n.event == "OnSharedBookmarksFolderUpdated"));
    let uk_after_exp = rpc(&ctx, &mut sess2, "accessGroupBookmarkMgr", "UpdateKnownFolderState", &[PyRep::Integer(folder_id), PyRep::Bool(true)]).await;
    let PyRep::Tuple(uk_exp_parts) = &uk_after_exp else { panic!() };
    let PyRep::List(uk_exp_bms) = &uk_exp_parts[1] else { panic!() };
    assert!(!uk_exp_bms.iter().any(|b| b.dict_get("bookmarkID") == Some(&PyRep::Integer(temp_bm_id))));
    let warp_err = rpc_err(&ctx, &mut sess2, "beyonce", "CmdWarpToStuff", &[PyRep::String("bookmark".into()), PyRep::Integer(temp_bm_id)]).await;
    assert!(warp_err.code.contains("Expired"));
}
