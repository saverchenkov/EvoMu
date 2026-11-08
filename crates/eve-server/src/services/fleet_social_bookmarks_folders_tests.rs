//! Unit tests for `fleet_social_bookmarks` folders, subfolders, known folders, scan results, and batch copy/move.

use super::super::*;
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::{
    seed_multi_member_fleet, test_ctx_with_clock,
};
use eve_db::{CosmicSignatureRecord, TrainedSkillRecord};
use eve_machonet::SessionState;
use std::collections::HashMap;

#[tokio::test]
async fn bookmark_folders_subfolders_and_known_subscriptions_lifecycle() {
    let ctx = test_ctx_with_clock(1_800_002_000);
    let (_fleet_id, mut session) = seed_multi_member_fleet(&ctx).await;
    session.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
    let kw = HashMap::new();

    // AddFolder personal + shared corp folder
    let pf_res = handle_bookmarks(
        &ctx,
        &mut session,
        "bookmark",
        "AddFolder",
        &[PyRep::String("Personal Safes".into()), PyRep::Bool(false)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(pf_kv) = pf_res else { panic!() };
    let personal_fid = pf_kv.get("folderID").unwrap().as_i64().unwrap();

    let sf_res = handle_bookmarks(
        &ctx,
        &mut session,
        "bookmark",
        "AddFolder",
        &[PyRep::String("Corp Tactical".into()), PyRep::Bool(true)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(sf_kv) = sf_res else { panic!() };
    let shared_fid = sf_kv.get("folderID").unwrap().as_i64().unwrap();
    assert_eq!(sf_kv.get("isShared").unwrap(), &PyRep::Bool(true));

    // UpdateFolder renames personal folder
    let uf_res = handle_bookmarks(
        &ctx,
        &mut session,
        "bookmark",
        "UpdateFolder",
        &[PyRep::Integer(personal_fid), PyRep::String("Updated Safes".into())],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(uf_kv) = uf_res else { panic!() };
    assert_eq!(uf_kv.get("folderName").unwrap().as_str().unwrap(), "Updated Safes");

    // CreateSubfolder + rename subfolder + enforce max 5 personal subfolders
    let sub1 = handle_bookmarks(
        &ctx,
        &mut session,
        "bookmark",
        "CreateSubfolder",
        &[PyRep::Integer(personal_fid), PyRep::String("Lowsec".into())],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(sub1_kv) = sub1 else { panic!() };
    let sub1_id = sub1_kv.get("subfolderID").unwrap().as_i64().unwrap();
    assert_eq!(
        sub1_kv.get("folderName").unwrap().as_str().unwrap(),
        "Updated Safes/Lowsec"
    );

    // Rename existing subfolder by passing subfolder_id first
    let sub1_ren = handle_bookmarks(
        &ctx,
        &mut session,
        "bookmark",
        "CreateSubfolder",
        &[PyRep::Integer(sub1_id), PyRep::String("Nullsec".into())],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(sub1_ren_kv) = sub1_ren else { panic!() };
    assert_eq!(
        sub1_ren_kv.get("folderName").unwrap().as_str().unwrap(),
        "Updated Safes/Lowsec/Nullsec"
    );

    for idx in 2..=4 {
        handle_bookmarks(
            &ctx,
            &mut session,
            "bookmark",
            "CreateSubfolder",
            &[PyRep::Integer(personal_fid), PyRep::String(format!("Sub {idx}"))],
            &kw,
        )
        .await
        .unwrap()
        .unwrap();
    }
    assert_eq!(
        handle_bookmarks(
            &ctx,
            &mut session,
            "bookmark",
            "CreateSubfolder",
            &[PyRep::Integer(personal_fid), PyRep::String("Overflow".into())],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "MaxSubfoldersReached"
    );

    // GetFolders top-level vs scoped by parent folderID
    assert!(handle_bookmarks(&ctx, &mut session, "bookmark", "GetFolders", &[], &kw)
        .await
        .unwrap()
        .is_ok());
    assert!(handle_bookmarks(
        &ctx,
        &mut session,
        "bookmark",
        "GetFolders",
        &[PyRep::Integer(personal_fid)],
        &kw,
    )
    .await
    .unwrap()
    .is_ok());

    // AddToKnownFolders + RemoveFromKnownFolders by another corp member
    let mut member_sess = SessionState::new(3);
    member_sess.user_id = Some(90_000_002);
    member_sess.character_id = Some(90_000_002);
    member_sess.corporation_id = Some(98_000_001);
    let add_known = handle_bookmarks(
        &ctx,
        &mut member_sess,
        "bookmark",
        "AddToKnownFolders",
        &[PyRep::Integer(shared_fid), PyRep::Bool(true)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(ak_kv) = add_known else { panic!() };
    assert_eq!(ak_kv.get("subscribed").unwrap(), &PyRep::Bool(true));

    let rem_known = handle_bookmarks(
        &ctx,
        &mut member_sess,
        "bookmark",
        "RemoveFromKnownFolders",
        &[PyRep::Integer(shared_fid)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(rk_kv) = rem_known else { panic!() };
    assert_eq!(rk_kv.get("subscribed").unwrap(), &PyRep::Bool(false));

    // DeleteSubfolder & DeleteFolder cleanup
    assert!(handle_bookmarks(
        &ctx,
        &mut session,
        "bookmark",
        "DeleteSubfolder",
        &[PyRep::Integer(sub1_id)],
        &kw,
    )
    .await
    .unwrap()
    .is_ok());
    assert!(handle_bookmarks(
        &ctx,
        &mut session,
        "bookmark",
        "DeleteFolder",
        &[PyRep::Integer(personal_fid)],
        &kw,
    )
    .await
    .unwrap()
    .is_ok());
}

#[tokio::test]
async fn bookmark_scan_result_and_copy_move_batches() {
    let ctx = test_ctx_with_clock(1_800_003_000);
    let (_fleet_id, mut session) = seed_multi_member_fleet(&ctx).await;
    let kw = HashMap::new();

    // Seed cosmic signatures: one unresolved (0.5) and one resolved (1.0), plus a folder and bookmark
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(90_000_001).or_default().insert(
            3412,
            TrainedSkillRecord {
                type_id: 3412,
                level: 0,
                skill_points: 0.0,
            },
        );
        snap.cosmic_signatures.insert(
            30_000_142,
            vec![
                CosmicSignatureRecord {
                    signature_id: "LOW-101".into(),
                    solar_system_id: 30_000_142,
                    sig_group: "Relic".into(),
                    type_name: "Ruined Angel Monument".into(),
                    position: [1_000.0, 2_000.0, 3_000.0],
                    signal_strength: 0.4,
                    wormhole_type_id: None,
                    destination_system_id: None,
                    created_unix: 1_800_002_900,
                    lifetime_seconds: 3_600,
                    max_mass_kg: 0.0,
                    remaining_mass_kg: 0.0,
                    max_jump_mass_kg: 0.0,
                    collapsed: false,
                },
                CosmicSignatureRecord {
                    signature_id: "WH-202".into(),
                    solar_system_id: 30_000_142,
                    sig_group: "Wormhole".into(),
                    type_name: "Unstable Wormhole".into(),
                    position: [99_000.0, -5_000.0, 12_000.0],
                    signal_strength: 1.0,
                    wormhole_type_id: Some(30_000),
                    destination_system_id: Some(31_000_005),
                    created_unix: 1_800_002_900,
                    lifetime_seconds: 3_600,
                    max_mass_kg: 2_000_000_000.0,
                    remaining_mass_kg: 2_000_000_000.0,
                    max_jump_mass_kg: 300_000_000.0,
                    collapsed: false,
                },
            ],
        );
        snap.bookmark_folders.insert(
            820_001,
            BookmarkFolderRecord {
                folder_id: 820_001,
                owner_id: 90_000_001,
                folder_name: "Dest Folder".into(),
                is_shared: false,
                corporation_id: None,
                ..Default::default()
            },
        );
    });

    // Docked session rejected from BookmarkScanResult
    let mut docked = session.clone();
    docked.station_id = Some(60_003_760);
    assert_eq!(
        handle_bookmarks(
            &ctx,
            &mut docked,
            "scanMgr",
            "BookmarkScanResult",
            &[PyRep::String("WH-202".into())],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "NotInSpace"
    );

    // Unresolved signature rejected, missing signature rejected, resolved signature succeeds
    assert_eq!(
        handle_bookmarks(
            &ctx,
            &mut session,
            "scanMgr",
            "BookmarkScanResult",
            &[PyRep::String("MISSING".into())],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "SignatureNotFound"
    );
    assert_eq!(
        handle_bookmarks(
            &ctx,
            &mut session,
            "scanMgr",
            "BookmarkScanResult",
            &[PyRep::String("LOW-101".into())],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "SignatureNotResolved"
    );
    let scan_ok = handle_bookmarks(
        &ctx,
        &mut session,
        "beyonce",
        "BookmarkScanResult",
        &[
            PyRep::Integer(30_000_142),
            PyRep::String("C2 Hole".into()),
            PyRep::None,
            PyRep::String("WH-202".into()),
            PyRep::Integer(820_001),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::Tuple(scan_tup) = scan_ok else { panic!() };
    let scan_bm_id = scan_tup[0].as_i64().unwrap();
    assert_eq!(scan_tup[2].as_i64().unwrap(), 30_000);

    // CopyBookmarksToFolderAndSubfolder & MoveBookmarksToFolderAndSubfolder
    let copy_res = handle_bookmarks(
        &ctx,
        &mut session,
        "bookmark",
        "CopyBookmarksToFolderAndSubfolder",
        &[PyRep::List(vec![PyRep::Integer(scan_bm_id)]), PyRep::None],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(cp_kv) = copy_res else { panic!() };
    let PyRep::List(cp_ids) = cp_kv.get("bookmarkIDs").unwrap() else { panic!() };
    let copied_id = cp_ids[0].as_i64().unwrap();
    assert_ne!(copied_id, scan_bm_id);

    let move_res = handle_bookmarks(
        &ctx,
        &mut session,
        "bookmark",
        "MoveBookmarksToFolderAndSubfolder",
        &[
            PyRep::List(vec![PyRep::Integer(copied_id)]),
            PyRep::Integer(820_001),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(mv_kv) = move_res else { panic!() };
    assert_eq!(mv_kv.get("folderID").unwrap(), &PyRep::Integer(820_001));
    assert_eq!(
        ctx.db.with_snapshot(|s| s.bookmarks.get(&copied_id).and_then(|b| b.folder_id)),
        Some(820_001)
    );
}

#[tokio::test]
async fn bookmark_delete_folder_removes_folder_and_subfolder_bookmarks() {
    let ctx = test_ctx_with_clock(1_800_004_000);
    let mut session = SessionState::new(91);
    session.user_id = Some(1);
    session.character_id = Some(90_000_001);
    session.corporation_id = Some(98_000_001);
    session.corp_role = u64::MAX;
    let kw = HashMap::new();

    let fid = 820_100_i64;
    let sfid = 820_101_i64;
    let bm1 = 820_102_i64;
    let bm2 = 820_103_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.bookmark_folders.insert(
            fid,
            BookmarkFolderRecord {
                folder_id: fid,
                folder_name: "RootFolder".into(),
                owner_id: 90_000_001,
                is_shared: true,
                corporation_id: Some(98_000_001),
                ..Default::default()
            },
        );
        snap.bookmark_folders.insert(
            sfid,
            BookmarkFolderRecord {
                folder_id: sfid,
                folder_name: "SubFolder".into(),
                owner_id: 90_000_001,
                is_shared: true,
                corporation_id: Some(98_000_001),
                ..Default::default()
            },
        );
        snap.bookmark_subfolder_parents.insert(sfid, fid);
        snap.bookmarks.insert(
            bm1,
            BookmarkRecord {
                bookmark_id: bm1,
                owner_id: 90_000_001,
                solar_system_id: 30_000_142,
                folder_id: Some(fid),
                label: "InRoot".into(),
                x: 0.0,
                y: 0.0,
                z: 0.0,
                item_id: None,
                memo: String::new(),
                is_shared: true,
            },
        );
        snap.bookmarks.insert(
            bm2,
            BookmarkRecord {
                bookmark_id: bm2,
                owner_id: 90_000_001,
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
        snap.bookmark_subfolders.insert(bm2, sfid);
        snap.bookmark_expiries.insert(bm1, 1_800_010_000);
        snap.bookmark_expiries.insert(bm2, 1_800_010_000);
    });

    handle_bookmarks(
        &ctx,
        &mut session,
        "bookmark",
        "DeleteFolder",
        &[PyRep::Integer(fid)],
        &kw,
    )
    .await
    .unwrap()
    .expect("DeleteFolder ok");

    ctx.db.with_snapshot(|snap| {
        assert!(!snap.bookmark_folders.contains_key(&fid));
        assert!(!snap.bookmark_folders.contains_key(&sfid));
        assert!(!snap.bookmarks.contains_key(&bm1));
        assert!(!snap.bookmarks.contains_key(&bm2));
        assert!(!snap.bookmark_expiries.contains_key(&bm1));
        assert!(!snap.bookmark_expiries.contains_key(&bm2));
        assert!(!snap.bookmark_subfolders.contains_key(&bm2));
    });
}
