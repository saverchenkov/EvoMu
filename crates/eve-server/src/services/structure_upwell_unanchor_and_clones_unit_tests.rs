//! Semantic unit tests split from `structure_upwell_ops_extra_unit_tests`.

#![forbid(unsafe_code)]

use super::structure_upwell_ops_extra_unit_tests::*;
use crate::services::station_structure::structure_upwell_ops::*;
use crate::services::structure_profile_services::*;
use eve_db::{
    InventoryFlag, InventoryItemRecord,
    JumpCloneRecord,
};
use eve_machonet::{service_roles, SessionState};
use eve_proto::PyRep;
use std::collections::HashMap;

#[tokio::test(flavor = "current_thread")]
async fn unanchor_repair_guard_asset_safety_wormhole_locked_and_corp_fee_branches() {
    let ctx = test_ctx();
    let (ceo_id, member_id, outsider_id) = (8201, 8202, 8203);
    let (corp_id, ally_id, sid, dest_sid, wh_sid) = (
        98_820_001,
        99_820_001,
        10_000_960_001,
        10_000_960_002,
        10_000_960_003,
    );
    seed_corp_and_structure(
        &ctx,
        ceo_id,
        member_id,
        outsider_id,
        corp_id,
        ally_id,
        sid,
        35833,
        30_000_142,
    );

    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        let mut st2 = snap.structures[&sid].clone();
        st2.structure_id = dest_sid;
        st2.name = "Dest Fortizar".into();
        snap.structures.insert(dest_sid, st2);

        let mut st_wh = snap.structures[&sid].clone();
        st_wh.structure_id = wh_sid;
        st_wh.solar_system_id = 31_000_001; // Wormhole system
        snap.structures.insert(wh_sid, st_wh);

        // Set repair_until_unix > now on sid -> blocks Unanchor and blocks apply_pending_reinforce_schedule
        let mut ext = load_ext_state(snap);
        ext.lifecycle.insert(
            sid,
            StructureLifecycleState {
                repair_until_unix: Some(now + 600),
                ..Default::default()
            },
        );
        ext.pending_reinforce_schedule.insert(sid, (12, 3, now));
        save_ext_state(snap, &ext);
    });

    let mut ceo_sess = SessionState::new(760);
    ceo_sess.user_id = Some(ceo_id as u64);
    ceo_sess.character_id = Some(ceo_id);
    ceo_sess.corporation_id = Some(corp_id);
    ceo_sess.solar_system_id = Some(30_000_142);

    // 1. Unanchor while under repair fails with CannotUnanchorReinforcedStructure; CancelUnanchor when not unanchoring fails
    assert_eq!(
        call_upwell(
            &ctx,
            &mut ceo_sess,
            "structureDeployment",
            "Unanchor",
            &[PyRep::Integer(sid)],
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "CannotUnanchorReinforcedStructure"
    );
    assert_eq!(
        call_upwell(
            &ctx,
            &mut ceo_sess,
            "structureDeployment",
            "CancelUnanchor",
            &[PyRep::Integer(sid)],
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "StructureNotUnanchoring"
    );
    assert_eq!(
        call_upwell(
            &ctx,
            &mut ceo_sess,
            "structureDeployment",
            "RenameStructure",
            &[PyRep::Integer(sid), PyRep::String("   ".into())],
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidStructureName"
    );

    // 2. Personal asset safety while docked in target structure fails; wormhole structure fails
    ceo_sess.structure_id = Some(sid);
    assert_eq!(
        call_upwell(
            &ctx,
            &mut ceo_sess,
            "structureAssetSafety",
            "MovePersonalAssetsToSafety",
            &[PyRep::Integer(30_000_142), PyRep::Integer(sid)],
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "CannotAssetSafetyWhileDockedInTargetStructure"
    );
    ceo_sess.structure_id = None;
    assert_eq!(
        call_upwell(
            &ctx,
            &mut ceo_sess,
            "structureAssetSafety",
            "MovePersonalAssetsToSafety",
            &[PyRep::Integer(31_000_001), PyRep::Integer(wh_sid)],
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "AssetSafetyUnavailableInWormhole"
    );

    // 3. Container with a Locked child item is excluded from asset safety -> NoAssetsToMoveToSafety
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            960_101,
            InventoryItemRecord {
                item_id: 960_101,
                type_id: 23,
                owner_id: ceo_id,
                location_id: sid,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            960_102,
            InventoryItemRecord {
                item_id: 960_102,
                type_id: 34,
                owner_id: ceo_id,
                location_id: 960_101,
                flag: InventoryFlag::Locked,
                quantity: 10,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });
    assert_eq!(
        call_upwell(
            &ctx,
            &mut ceo_sess,
            "structureAssetSafety",
            "MovePersonalAssetsToSafety",
            &[PyRep::Integer(30_000_142), PyRep::Integer(sid)],
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "NoAssetsToMoveToSafety"
    );

    // 4. Corp asset safety: non-director member rejected with InsufficientCorpRoles; CEO succeeds and delivers corp wrap paying 0.5% fee from division 1
    let mut mem_sess = ceo_sess.clone();
    mem_sess.character_id = Some(member_id);
    mem_sess.corp_role = 0;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            960_201,
            InventoryItemRecord {
                item_id: 960_201,
                type_id: 4051,
                owner_id: corp_id,
                location_id: sid,
                flag: InventoryFlag::CorpSAG1,
                quantity: 1_000,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });
    assert_eq!(
        call_upwell(
            &ctx,
            &mut mem_sess,
            "structureAssetSafety",
            "MoveCorpAssetsToSafety",
            &[PyRep::Integer(30_000_142), PyRep::Integer(sid)],
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InsufficientCorpRoles"
    );

    let corp_wrap_rep = call_upwell(
        &ctx,
        &mut ceo_sess,
        "structureAssetSafety",
        "MoveCorpAssetsToSafety",
        &[PyRep::Integer(30_000_142), PyRep::Integer(sid)],
    )
    .await
    .unwrap()
    .unwrap();
    let corp_wrap_id = corp_wrap_rep.dict_get("wrapID").unwrap().as_i64().unwrap();

    // Non-GM MoveEjectTimeGM rejected with InsufficientRole; GM succeeds
    assert_eq!(
        call_upwell(
            &ctx,
            &mut ceo_sess,
            "structureAssetSafety",
            "MoveEjectTimeGM",
            &[PyRep::Integer(corp_wrap_id), PyRep::Integer(0)],
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InsufficientRole"
    );
    ceo_sess.role = service_roles::ROLE_GMH;
    call_upwell(&ctx, &mut ceo_sess, "structureAssetSafety", "MoveEjectTimeGM", &[PyRep::Integer(corp_wrap_id), PyRep::Integer(0)])
        .await
        .unwrap()
        .unwrap();
    ceo_sess.role = 0;

    let deliv = call_upwell(
        &ctx,
        &mut ceo_sess,
        "structureAssetSafety",
        "MoveSafetyWrapToStructure",
        &[PyRep::Integer(corp_wrap_id), PyRep::Integer(dest_sid)],
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(deliv.dict_get("destinationID"), Some(&PyRep::Integer(dest_sid)));

    assert_eq!(
        call_upwell(
            &ctx,
            &mut ceo_sess,
            "structureAssetSafety",
            "MoveSafetyWrapToStructure",
            &[PyRep::Integer(corp_wrap_id), PyRep::Integer(dest_sid)],
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "AssetSafetyWrapAlreadyDelivered"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn map_data_memoized_clones_and_profile_tax_entity_validations() {
    let ctx = test_ctx();
    let (ceo_id, member_id, outsider_id) = (8301, 8302, 8303);
    let (corp_id, ally_id, sid) = (98_830_001, 99_830_001, 10_000_970_001);
    seed_corp_and_structure(&ctx, ceo_id, member_id, outsider_id, corp_id, ally_id, sid, 35833, 30_000_142);

    let mut sess = SessionState::new(770);
    sess.user_id = Some(ceo_id as u64);
    sess.character_id = Some(ceo_id);
    sess.corporation_id = Some(corp_id);
    sess.solar_system_id = Some(30_000_142);

    assert_eq!(call_upwell(&ctx, &mut sess, "structureSettings", "clear_memoized", &[]).await.unwrap().unwrap(), PyRep::None);
    let map_data = crate::services::station_structure::structure_profiles_directory::handle(
        &ctx,
        &mut sess,
        "structureDirectory",
        "GetStructureMapData",
        &[PyRep::Integer(30_000_142)],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap();
    assert!(matches!(map_data, PyRep::List(ref l) if !l.is_empty()));

    assert_eq!(
        call_upwell(&ctx, &mut sess, "structureHangarViewMgr", "GetMyHangarViewState", &[])
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NotDockedInStructure"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.jump_clones.entry(ceo_id).or_default().push(JumpCloneRecord {
            jump_clone_id: 7001,
            character_id: ceo_id,
            location_id: sid,
            implants: Vec::new(),
        });
    });
    assert_eq!(
        call_upwell(&ctx, &mut sess, "jumpCloneSvc", "GetNumClonesInPilotsStructure", &[PyRep::Integer(sid)])
            .await
            .unwrap()
            .unwrap(),
        PyRep::Integer(1)
    );

    assert_eq!(
        configure_structure_access_profile(&ctx, &sess, sid, Some(false), &[(SERVICE_MARKET, 1.5)], &[], &[], &[])
            .unwrap_err()
            .code,
        "InvalidStructureTaxRate"
    );
    assert_eq!(
        configure_structure_access_profile(&ctx, &sess, sid, Some(false), &[], &[], &[999_888_777], &[])
            .unwrap_err()
            .code,
        "InvalidAccessEntity"
    );

    configure_structure_access_profile(
        &ctx,
        &sess,
        sid,
        Some(false),
        &[(SERVICE_MARKET, 0.05)],
        &[(1, true)],
        &[outsider_id],
        &[ally_id],
    )
    .expect("valid profile config");
    modify_structure_access_node(&ctx, &sess, sid, outsider_id, true, false).expect("remove blocked char");
    ctx.db.with_snapshot(|snap| {
        let ext = load_ext_state(snap);
        let prof = &ext.access_profiles[&sid];
        assert_eq!(prof.service_settings.get(&SERVICE_MARKET), Some(&500));
        assert!(!prof.blocked_characters.contains(&outsider_id));
        assert!(prof.allowed_alliances.contains(&ally_id));
    });
}

#[tokio::test(flavor = "current_thread")]
async fn upwell_ops_dispatch_reinforce_safety_wraps_gm_and_profile_sync_matrix() {
    use crate::services::structure_acl_defense_rules::{
        load_defense_settings, save_defense_settings, StructureDefenseSettings,
    };
    use eve_db::InventoryItemRecord;

    let ctx = test_ctx();
    let (ceo_id, member_id, outsider_id) = (8301_i64, 8302_i64, 8303_i64);
    let (corp_id, ally_id, sid, dest_sid, dead_sid, far_sid, sov_hub_id) = (
        98_830_001_i64,
        99_830_001_i64,
        10_000_970_001_i64,
        10_000_970_002_i64,
        10_000_970_003_i64,
        10_000_970_004_i64,
        10_000_970_005_i64,
    );
    seed_corp_and_structure(
        &ctx, ceo_id, member_id, outsider_id, corp_id, ally_id, sid, 35833, 30_000_142,
    );

    let now = ctx.now_unix();
    let (lock_wrap_id, corp_wrap_id, auto_wrap_id, root_bag_id, nested_locked_id, tritium_id) =
        (930_001_i64, 930_002_i64, 930_003_i64, 930_010_i64, 930_011_i64, 930_012_i64);

    ctx.db.with_mut_snapshot(|snap| {
        let mut st_dest = snap.structures[&sid].clone();
        st_dest.structure_id = dest_sid;
        snap.structures.insert(dest_sid, st_dest);

        let mut st_dead = snap.structures[&sid].clone();
        st_dead.structure_id = dead_sid;
        st_dead.state = "Destroyed".into();
        snap.structures.insert(dead_sid, st_dead);

        let mut st_far = snap.structures[&sid].clone();
        st_far.structure_id = far_sid;
        st_far.solar_system_id = 30_002_187; // Amarr system
        snap.structures.insert(far_sid, st_far);

        let mut hub = snap.structures[&sid].clone();
        hub.structure_id = sov_hub_id;
        hub.type_id = crate::services::sov_services::TYPE_SOVEREIGNTY_HUB;
        hub.solar_system_id = 30_000_144;
        snap.structures.insert(sov_hub_id, hub);

        // Nested container with locked child blocks MovePersonalAssetsToSafety
        snap.items.insert(
            root_bag_id,
            InventoryItemRecord {
                item_id: root_bag_id,
                type_id: 17366,
                owner_id: ceo_id,
                location_id: sid,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            nested_locked_id,
            InventoryItemRecord {
                item_id: nested_locked_id,
                type_id: 34,
                owner_id: ceo_id,
                location_id: root_bag_id,
                flag: InventoryFlag::Locked,
                quantity: 10,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            tritium_id,
            InventoryItemRecord {
                item_id: tritium_id,
                type_id: 34,
                owner_id: ceo_id,
                location_id: sid,
                flag: InventoryFlag::Hangar,
                quantity: 5,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );

        let mut ext = load_ext_state(snap);
        ext.structure_profile_id.insert(dest_sid, 777);
        ext.profiles_by_id.insert(
            777,
            StructureAccessProfile {
                profile_id: 777,
                owner_corp_id: corp_id,
                name: "SyncProf".into(),
                corp_only: false,
                defense_settings: Some(StructureDefenseSettings::default()),
                ..Default::default()
            },
        );
        let mut ds = load_defense_settings(snap, dest_sid);
        ds.known_attackers.push(outsider_id);
        save_defense_settings(snap, &ds);

        // Asset safety wraps for GetWrapNames + MoveSafetyWrapToStructure guards
        ext.asset_safety_wraps.insert(
            lock_wrap_id,
            AssetSafetyWrapRecord {
                wrap_id: lock_wrap_id,
                owner_id: outsider_id,
                is_corp: false,
                source_structure_id: sid,
                source_structure_name: "PersonalWrap".into(),
                solar_system_id: 30_000_142,
                created_unix: now - 100,
                deliverable_after_unix: now - 10,
                delivered_to_id: None,
                item_ids: vec![],
                estimated_value_isk: 1_000.0,
                nearest_npc_station_id: Some(60_003_760),
                auto_delivered: false,
                retrieval_fee_paid: false,
            },
        );
        ext.asset_safety_wraps.insert(
            corp_wrap_id,
            AssetSafetyWrapRecord {
                wrap_id: corp_wrap_id,
                owner_id: corp_id,
                is_corp: true,
                source_structure_id: sid,
                source_structure_name: "CorpWrap".into(),
                solar_system_id: 30_000_142,
                created_unix: now - 100,
                deliverable_after_unix: now - 10,
                delivered_to_id: None,
                item_ids: vec![],
                estimated_value_isk: 1_000_000.0,
                nearest_npc_station_id: Some(60_003_760),
                auto_delivered: false,
                retrieval_fee_paid: false,
            },
        );
        ext.asset_safety_wraps.insert(
            auto_wrap_id,
            AssetSafetyWrapRecord {
                wrap_id: auto_wrap_id,
                owner_id: ceo_id,
                is_corp: false,
                source_structure_id: sid,
                source_structure_name: "AutoWrap".into(),
                solar_system_id: 30_000_142,
                created_unix: now - 100,
                deliverable_after_unix: now - 10,
                delivered_to_id: Some(60_003_760),
                item_ids: vec![],
                estimated_value_isk: 100.0,
                nearest_npc_station_id: Some(60_003_760),
                auto_delivered: true,
                retrieval_fee_paid: false,
            },
        );
        save_ext_state(snap, &ext);
    });

    let mut ceo_sess = SessionState::new(830);
    ceo_sess.user_id = Some(ceo_id as u64);
    ceo_sess.character_id = Some(ceo_id);
    ceo_sess.corporation_id = Some(corp_id);
    ceo_sess.solar_system_id = Some(30_000_142);

    // Dispatch coverage across structureControl / structureProfiles / ownerGroupManager / structureDirectory / structureJumpBridgeMgr
    for (svc, meth, args) in [
        ("structureControl", "GetStructurePilot", vec![PyRep::Integer(sid)]),
        ("structureControl", "ReleaseControl", vec![PyRep::Integer(sid)]),
        ("structureControl", "BoardStructure", vec![PyRep::Integer(sid)]),
        ("structureControl", "EjectFromStructure", vec![PyRep::Integer(sid)]),
        ("structureProfiles", "GetProfiles", vec![]),
        ("structureProfiles", "GetProfileSettings", vec![PyRep::Integer(777)]),
        ("structureProfiles", "CreateProfile", vec![PyRep::String("P1".into())]),
        ("structureProfiles", "DuplicateProfile", vec![PyRep::Integer(777)]),
        ("structureProfiles", "SetDefaultProfile", vec![PyRep::Integer(777)]),
        ("structureProfiles", "SaveProfileSettings", vec![PyRep::Integer(777)]),
        ("structureProfiles", "UpdateProfile", vec![PyRep::Integer(777)]),
        ("structureProfiles", "ChangeProfiles", vec![PyRep::List(vec![PyRep::Integer(sid)]), PyRep::Integer(777)]),
        ("structureProfiles", "DeleteProfile", vec![PyRep::Integer(999_999)]),
        ("ownerGroupManager", "AddMembers", vec![PyRep::Integer(0), PyRep::List(vec![])]),
        ("ownerGroupManager", "DeleteMembers", vec![PyRep::Integer(0), PyRep::List(vec![])]),
        ("structureDirectory", "GetStructures", vec![]),
        ("structureDirectory", "GetCorporationStructures", vec![]),
        ("structureDirectory", "GetStructureInfo", vec![PyRep::Integer(sid)]),
        ("structureJumpBridgeMgr", "GetLinkedStructure", vec![PyRep::Integer(sid)]),
        ("structureJumpBridgeMgr", "GetJbStructureDestination", vec![PyRep::Integer(sid)]),
        ("structureDirectory", "GetJumpBridgesWithMyAccess", vec![]),
        ("structureDirectory", "GetNearbyJumpBridges", vec![]),
    ] {
        assert!(call_upwell(&ctx, &mut ceo_sess, svc, meth, &args).await.is_some());
    }

    // SovHub UpdateReinforceTimesFromClient triggers apply_adm_scaled_vulnerability_window_in_snap
    let _ = call_upwell(
        &ctx,
        &mut ceo_sess,
        "structureDeployment",
        "UpdateReinforceTimesFromClient",
        &[PyRep::Integer(sov_hub_id), PyRep::Integer(14), PyRep::Integer(2)],
    )
    .await;

    // StructureNotFound / StructureDestroyed / StructureOwnershipViolation
    assert_eq!(
        call_upwell(&ctx, &mut ceo_sess, "structureDeployment", "Unanchor", &[PyRep::Integer(999_888_777)]).await.unwrap().unwrap_err().code,
        "StructureNotFound"
    );
    assert_eq!(
        call_upwell(&ctx, &mut ceo_sess, "structureDeployment", "Unanchor", &[PyRep::Integer(dead_sid)]).await.unwrap().unwrap_err().code,
        "StructureDestroyed"
    );
    let mut outsider_sess = ceo_sess.clone();
    outsider_sess.character_id = Some(outsider_id);
    outsider_sess.corporation_id = Some(98_999_999);
    assert_eq!(
        call_upwell(&ctx, &mut outsider_sess, "structureDeployment", "Unanchor", &[PyRep::Integer(sid)]).await.unwrap().unwrap_err().code,
        "StructureOwnershipViolation"
    );
    assert_eq!(
        call_upwell(&ctx, &mut outsider_sess, "structureAssetSafety", "MoveCorpAssetsToSafety", &[PyRep::Integer(30_000_142), PyRep::Integer(sid)]).await.unwrap().unwrap_err().code,
        "StructureOwnershipViolation"
    );

    // MovePersonalAssetsToSafety skips locked nested root_bag_id while moving unlocked tritium_id
    let _ = call_upwell(&ctx, &mut ceo_sess, "structureAssetSafety", "MovePersonalAssetsToSafety", &[PyRep::Integer(30_000_142), PyRep::Integer(sid)]).await.unwrap().unwrap();
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&root_bag_id].location_id, sid);
        assert_ne!(snap.items[&tritium_id].location_id, sid);
    });

    // GetWrapNames integer + None + corp wrap + GM role visibility
    assert!(call_upwell(&ctx, &mut ceo_sess, "structureAssetSafety", "GetWrapNames", &[PyRep::None]).await.unwrap().is_ok());
    let gm_sess = {
        let mut s = ceo_sess.clone();
        s.role = service_roles::ROLE_GM_MASK;
        s
    };
    let gm_names = call_upwell(&ctx, &mut gm_sess.clone(), "structureAssetSafety", "GetWrapNames", &[PyRep::Integer(lock_wrap_id)]).await.unwrap().unwrap();
    let PyRep::Dict(entries) = gm_names else { panic!("expected dict") };
    assert_eq!(entries.len(), 1);
    let corp_names = call_upwell(&ctx, &mut ceo_sess, "structureAssetSafety", "GetWrapNames", &[PyRep::Integer(corp_wrap_id)]).await.unwrap().unwrap();
    let PyRep::Dict(corp_entries) = corp_names else { panic!("expected dict") };
    assert_eq!(corp_entries.len(), 1);

    // MoveSafetyWrapToStructure guards
    assert_eq!(
        call_upwell(&ctx, &mut ceo_sess, "structureAssetSafety", "MoveSafetyWrapToStructure", &[PyRep::Integer(corp_wrap_id), PyRep::Integer(dead_sid)]).await.unwrap().unwrap_err().code,
        "InvalidAssetSafetyDestination"
    );
    assert_eq!(
        call_upwell(&ctx, &mut ceo_sess, "structureAssetSafety", "MoveSafetyWrapToStructure", &[PyRep::Integer(lock_wrap_id), PyRep::Integer(dest_sid)]).await.unwrap().unwrap_err().code,
        "NotAssetSafetyWrapOwner"
    );
    let mut non_dir_sess = ceo_sess.clone();
    non_dir_sess.character_id = Some(member_id);
    assert_eq!(
        call_upwell(&ctx, &mut non_dir_sess, "structureAssetSafety", "MoveSafetyWrapToStructure", &[PyRep::Integer(corp_wrap_id), PyRep::Integer(dest_sid)]).await.unwrap().unwrap_err().code,
        "InsufficientCorpRoles"
    );
    assert_eq!(
        call_upwell(&ctx, &mut ceo_sess, "structureAssetSafety", "MoveSafetyWrapToStructure", &[PyRep::Integer(auto_wrap_id), PyRep::Integer(dest_sid)]).await.unwrap().unwrap_err().code,
        "InvalidAssetSafetyDestination"
    );
    assert_eq!(
        call_upwell(&ctx, &mut ceo_sess, "structureAssetSafety", "MoveSafetyWrapToStructure", &[PyRep::Integer(corp_wrap_id), PyRep::Integer(far_sid)]).await.unwrap().unwrap_err().code,
        "InvalidAssetSafetyDestination"
    );
    ctx.db.with_mut_snapshot(|s| {
        let mut ext = load_ext_state(s);
        let mut prof = ext.access_profiles.get(&dest_sid).cloned().unwrap_or_default();
        prof.corp_only = true;
        ext.access_profiles.insert(dest_sid, prof);
        save_ext_state(s, &ext);
    });
    assert_eq!(
        call_upwell(&ctx, &mut outsider_sess, "structureAssetSafety", "MoveSafetyWrapToStructure", &[PyRep::Integer(lock_wrap_id), PyRep::Integer(dest_sid)]).await.unwrap().unwrap_err().code,
        "InvalidAssetSafetyDestination"
    );
    // Successful corp wrap delivery charges corp wallet div1 and notifies character + corp
    assert!(
        call_upwell(&ctx, &mut ceo_sess, "structureAssetSafety", "MoveSafetyWrapToStructure", &[PyRep::Integer(corp_wrap_id), PyRep::Integer(sid)]).await.unwrap().is_ok()
    );
    assert!(
        call_upwell(&ctx, &mut ceo_sess, "structureAssetSafety", "MoveSafetyWrapToStructure", &[PyRep::Integer(corp_wrap_id), PyRep::Integer(sid)]).await.unwrap().unwrap_err().code == "AssetSafetyWrapAlreadyDelivered"
    );

    // sync_assigned_structures_for_profile_with_snap + sync_pending_profiles_in_ctx due profile
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        if ext.profiles_by_id.contains_key(&777) {
            sync_assigned_structures_for_profile_with_snap(snap, &mut ext, 777);
        }
        ext.pending_profile_applications.insert(sid, (777, now.saturating_sub(1)));
        save_ext_state(snap, &ext);
    });
    sync_pending_profiles_in_ctx(&ctx);
}

#[tokio::test(flavor = "current_thread")]
async fn transfer_structure_corp_ownership_full_branch_and_controller_matrix() {
    use eve_db::{CorporationRecord, InventoryItemRecord};

    let ctx = test_ctx();
    let (ceo_id, target_ceo_id, controller_id) = (8401_i64, 8402_i64, 8403_i64);
    let (corp_a, corp_b, corp_no_ally, ally_a, ally_b) =
        (98_840_001_i64, 98_840_002_i64, 98_840_003_i64, 99_840_001_i64, 99_840_002_i64);
    let (sid, sov_hub_id, reinf_sid, other_sys_sid) = (
        10_000_980_001_i64,
        10_000_980_002_i64,
        10_000_980_003_i64,
        10_000_980_004_i64,
    );
    seed_corp_and_structure(&ctx, ceo_id, controller_id, target_ceo_id, corp_a, ally_a, sid, 35833, 30_000_142);

    ctx.db.with_mut_snapshot(|snap| {
        let mut cb = CorporationRecord::new(corp_b, "TargetCorpB", "TCB", target_ceo_id);
        cb.alliance_id = Some(ally_b);
        snap.corporations.insert(corp_b, cb);

        let cn = CorporationRecord::new(corp_no_ally, "TargetNoAlly", "TNA", target_ceo_id);
        snap.corporations.insert(corp_no_ally, cn);

        let mut hub = snap.structures[&sid].clone();
        hub.structure_id = sov_hub_id;
        hub.type_id = crate::services::sov_services::TYPE_SOVEREIGNTY_HUB;
        snap.structures.insert(sov_hub_id, hub);

        let mut reinf = snap.structures[&sid].clone();
        reinf.structure_id = reinf_sid;
        reinf.state = "ArmorReinforced".into();
        snap.structures.insert(reinf_sid, reinf);

        let mut far = snap.structures[&sid].clone();
        far.structure_id = other_sys_sid;
        far.solar_system_id = 30_002_187;
        snap.structures.insert(other_sys_sid, far);

        // Fit module, nested charge inside module, service bay slot, quantum core room, and unauthorized controller
        for (iid, tid, loc, flag) in [
            (940_101, 35892, sid, InventoryFlag::LoSlot0),
            (940_102, 34, 940_101, InventoryFlag::Cargo),
            (940_103, 4051, sid, InventoryFlag::StructureFuel),
            (940_104, 56201, sid, InventoryFlag::QuantumCoreRoom),
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: corp_a,
                    location_id: loc,
                    flag,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }

        let mut ext = load_ext_state(snap);
        ext.active_pilot_by_structure.insert(sid, controller_id);
        // First give corp_b no default profile, then add a default profile for second transfer
        ext.profiles_by_id.insert(
            980,
            StructureAccessProfile {
                profile_id: 980,
                owner_corp_id: corp_no_ally,
                name: "DefaultNoAlly".into(),
                is_default: true,
                corp_only: false,
                ..Default::default()
            },
        );
        save_ext_state(snap, &ext);
    });

    ctx.db.with_mut_snapshot(|snap| {
        assert_eq!(
            transfer_structure_corp_ownership_in_snap(snap, 999_888_777, corp_a, corp_b, 30_000_142).unwrap_err().code,
            "ItemNotFound"
        );
        assert_eq!(
            transfer_structure_corp_ownership_in_snap(snap, sid, corp_b, corp_a, 30_000_142).unwrap_err().code,
            "CrpAccessDenied"
        );
        assert_eq!(
            transfer_structure_corp_ownership_in_snap(snap, other_sys_sid, corp_a, corp_b, 30_000_142).unwrap_err().code,
            "ItemNotInSpace"
        );
        assert_eq!(
            transfer_structure_corp_ownership_in_snap(snap, reinf_sid, corp_a, corp_b, 30_000_142).unwrap_err().code,
            "StructureTransferForbidden"
        );
        assert_eq!(
            transfer_structure_corp_ownership_in_snap(snap, sov_hub_id, corp_a, corp_b, 30_000_142).unwrap_err().code,
            "SovHubAllianceMismatch"
        );
        // Valid transfer corp_a -> corp_b (no default profile -> fallback corp_only=true, moves bays & mods, releases controller)
        let released = transfer_structure_corp_ownership_in_snap(snap, sid, corp_a, corp_b, 30_000_142).unwrap();
        assert!(!released.is_empty());
        assert_eq!(snap.structures[&sid].owner_corp_id, corp_b);
        assert_eq!(snap.items[&940_101].owner_id, corp_b);
        assert_eq!(snap.items[&940_102].owner_id, corp_b);
        assert_eq!(snap.items[&940_103].owner_id, corp_b);
        assert_eq!(snap.items[&940_104].owner_id, corp_b);
        assert!(load_ext_state(snap).access_profiles[&sid].corp_only);

        // Second transfer corp_b -> corp_no_ally picks up corp_no_ally's default profile 980
        transfer_structure_corp_ownership_in_snap(snap, sid, corp_b, corp_no_ally, 30_000_142).unwrap();
        assert_eq!(load_ext_state(snap).structure_profile_id.get(&sid), Some(&980));
    });
}

#[tokio::test(flavor = "current_thread")]
async fn structure_fuel_jettison_deliver_and_cargo_dropoff_branch_matrix() {
    use eve_db::{corp_roles, InventoryItemRecord};

    let ctx = test_ctx();
    let (ceo_id, member_id, outsider_id) = (8501_i64, 8502_i64, 8503_i64);
    let (corp_id, ally_id, sid, dead_sid, flex_sid, offline_sid) = (
        98_850_001_i64,
        99_850_001_i64,
        10_000_990_001_i64,
        10_000_990_002_i64,
        10_000_990_003_i64,
        10_000_990_004_i64,
    );
    seed_corp_and_structure(&ctx, ceo_id, member_id, outsider_id, corp_id, ally_id, sid, 35833, 30_000_142);

    let sys_id = 30_000_142_u32;
    let ceo_ship = 880_000 + ceo_id;
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    sys_h
        .spawn_ball_with_meta(
            eve_destiny::Ball {
                id: ceo_ship,
                type_id: 587,
                position: glam::DVec3::new(1_000_000.0, 0.0, 0.0),
                velocity: glam::DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 2.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            },
            Default::default(),
            Default::default(),
        )
        .await
        .unwrap();

    let (
        fuel_bay_item1,
        fuel_bay_item2,
        other_owner_fuel,
        non_fuel_in_bay,
        hangar_item_deliver,
        nonempty_box,
        box_child,
        locked_hangar_item,
        cargo_drop_id,
        fuel_bay_drop_id,
        bad_flag_drop_id,
        locked_drop_id,
    ) = (
        950_001_i64,
        950_002_i64,
        950_003_i64,
        950_004_i64,
        950_005_i64,
        950_006_i64,
        950_007_i64,
        950_008_i64,
        950_009_i64,
        950_010_i64,
        950_011_i64,
        950_012_i64,
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&sid).unwrap().fuel_blocks = 10;
        let mut dead = snap.structures[&sid].clone();
        dead.structure_id = dead_sid;
        dead.state = "Destroyed".into();
        snap.structures.insert(dead_sid, dead);

        let mut flex = snap.structures[&sid].clone();
        flex.structure_id = flex_sid;
        flex.type_id = 35_840;
        snap.structures.insert(flex_sid, flex);

        let mut off = snap.structures[&sid].clone();
        off.structure_id = offline_sid;
        off.state = "Anchoring".into();
        snap.structures.insert(offline_sid, off);

        let mut ext = load_ext_state(snap);
        ext.access_profiles.insert(
            sid,
            StructureAccessProfile {
                profile_id: 1,
                owner_corp_id: corp_id,
                corp_only: false,
                ..Default::default()
            },
        );
        save_ext_state(snap, &ext);

        for (iid, tid, owner, loc, flag, qty) in [
            (fuel_bay_item1, 4051, corp_id, sid, InventoryFlag::StructureFuel, 5),
            (fuel_bay_item2, 4051, corp_id, sid, InventoryFlag::StructureFuel, 5),
            (other_owner_fuel, 4051, ceo_id, sid, InventoryFlag::StructureFuel, 3),
            (non_fuel_in_bay, 34, corp_id, sid, InventoryFlag::StructureFuel, 10),
            (hangar_item_deliver, 34, ceo_id, sid, InventoryFlag::Hangar, 10),
            (nonempty_box, 17366, ceo_id, sid, InventoryFlag::Hangar, 1),
            (box_child, 34, ceo_id, nonempty_box, InventoryFlag::Cargo, 2),
            (locked_hangar_item, 34, ceo_id, sid, InventoryFlag::Hangar, 4),
            (cargo_drop_id, 34, ceo_id, ceo_ship, InventoryFlag::Cargo, 10),
            (fuel_bay_drop_id, 4051, ceo_id, ceo_ship, InventoryFlag::SpecializedFuelBay, 4),
            (bad_flag_drop_id, 34, ceo_id, ceo_ship, InventoryFlag::LoSlot0, 1),
            (locked_drop_id, 34, ceo_id, ceo_ship, InventoryFlag::Cargo, 1),
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: owner,
                    location_id: loc,
                    flag,
                    quantity: qty,
                    singleton: iid == nonempty_box,
                    version: 1,
                    ..Default::default()
                },
            );
        }
        snap.contracts.insert(
            777_001,
            eve_db::ContractRecord {
                contract_id: 777_001,
                issuer_id: ceo_id,
                contract_type: "ItemExchange".into(),
                status: "Outstanding".into(),
                item_ids: vec![locked_hangar_item, locked_drop_id],
                ..Default::default()
            },
        );
    });

    let mut ceo_sess = SessionState::new(850);
    ceo_sess.user_id = Some(ceo_id as u64);
    ceo_sess.character_id = Some(ceo_id);
    ceo_sess.corporation_id = Some(corp_id);
    ceo_sess.solar_system_id = Some(sys_id);
    ceo_sess.ship_id = Some(ceo_ship);

    // A. cargo dropoff matrix while in space
    let mut docked_sess = ceo_sess.clone();
    docked_sess.structure_id = Some(sid);
    assert_eq!(
        call_upwell(&ctx, &mut docked_sess, "structureCargoDelivery", "DropOffItems", &[PyRep::Integer(sid), PyRep::List(vec![PyRep::Integer(cargo_drop_id)])]).await.unwrap().unwrap_err().code,
        "MustBeInSpace"
    );
    assert_eq!(
        call_upwell(&ctx, &mut ceo_sess, "structureCargoDelivery", "DropOffItems", &[PyRep::Integer(sid), PyRep::List(vec![])]).await.unwrap().unwrap_err().code,
        "NoItemsSpecified"
    );
    assert_eq!(
        call_upwell(&ctx, &mut ceo_sess, "structureCargoDelivery", "DropOffItems", &[PyRep::Integer(flex_sid), PyRep::Integer(cargo_drop_id)]).await.unwrap().unwrap_err().code,
        "StructureNotDockable"
    );
    assert_eq!(
        call_upwell(&ctx, &mut ceo_sess, "structureCargoDelivery", "DropOffItems", &[PyRep::Integer(offline_sid), PyRep::Integer(cargo_drop_id)]).await.unwrap().unwrap_err().code,
        "StructureNotOperational"
    );
    assert_eq!(
        call_upwell(&ctx, &mut ceo_sess, "structureCargoDelivery", "DropOffItems", &[PyRep::Integer(sid), PyRep::Integer(bad_flag_drop_id)]).await.unwrap().unwrap_err().code,
        "ItemNotInShipCargo"
    );
    assert_eq!(
        call_upwell(&ctx, &mut ceo_sess, "structureCargoDelivery", "DropOffItems", &[PyRep::Integer(sid), PyRep::Integer(locked_drop_id)]).await.unwrap().unwrap_err().code,
        "ItemLocked"
    );
    assert!(
        call_upwell(&ctx, &mut ceo_sess, "structureCargoDelivery", "DropOffItems", &[PyRep::Integer(sid), PyRep::List(vec![PyRep::Integer(cargo_drop_id), PyRep::Integer(fuel_bay_drop_id)])]).await.unwrap().is_ok()
    );
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.items[&cargo_drop_id].location_id, sid);
        assert_eq!(s.items[&cargo_drop_id].flag, InventoryFlag::Deliveries);
        assert_eq!(s.items[&fuel_bay_drop_id].location_id, sid);
        assert_eq!(s.items[&fuel_bay_drop_id].flag, InventoryFlag::Deliveries);
    });

    // B. structureDeliveries.Deliver guards + success
    assert_eq!(
        call_upwell(&ctx, &mut ceo_sess, "structureDeliveries", "Deliver", &[PyRep::Integer(sid), PyRep::Integer(hangar_item_deliver), PyRep::Integer(member_id)]).await.unwrap().unwrap_err().code,
        "NotDockedInStructure"
    );
    assert_eq!(
        call_upwell(&ctx, &mut docked_sess, "structureDeliveries", "Deliver", &[PyRep::Integer(sid), PyRep::Integer(ceo_ship), PyRep::Integer(member_id)]).await.unwrap().unwrap_err().code,
        "CannotDeliverActiveShip"
    );
    assert_eq!(
        call_upwell(&ctx, &mut docked_sess, "structureDeliveries", "Deliver", &[PyRep::Integer(sid), PyRep::Integer(hangar_item_deliver), PyRep::Integer(999_888)]).await.unwrap().unwrap_err().code,
        "RecipientNotFound"
    );
    assert_eq!(
        call_upwell(&ctx, &mut docked_sess, "structureDeliveries", "Deliver", &[PyRep::Integer(sid), PyRep::Integer(888_777), PyRep::Integer(member_id)]).await.unwrap().unwrap_err().code,
        "ItemNotFound"
    );
    assert_eq!(
        call_upwell(&ctx, &mut docked_sess, "structureDeliveries", "Deliver", &[PyRep::Integer(sid), PyRep::Integer(bad_flag_drop_id), PyRep::Integer(member_id)]).await.unwrap().unwrap_err().code,
        "ItemNotInStructureHangar"
    );
    assert_eq!(
        call_upwell(&ctx, &mut docked_sess, "structureDeliveries", "Deliver", &[PyRep::Integer(sid), PyRep::Integer(locked_hangar_item), PyRep::Integer(member_id)]).await.unwrap().unwrap_err().code,
        "ItemLocked"
    );
    assert_eq!(
        call_upwell(&ctx, &mut docked_sess, "structureDeliveries", "Deliver", &[PyRep::Integer(sid), PyRep::Integer(nonempty_box), PyRep::Integer(member_id)]).await.unwrap().unwrap_err().code,
        "CannotDeliverNonEmptyContainer"
    );
    assert!(
        call_upwell(&ctx, &mut docked_sess, "structureDeliveries", "Deliver", &[PyRep::Integer(sid), PyRep::Integer(hangar_item_deliver), PyRep::Integer(member_id)]).await.unwrap().is_ok()
    );
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.items[&hangar_item_deliver].owner_id, member_id);
        assert_eq!(s.items[&hangar_item_deliver].flag, InventoryFlag::Deliveries);
    });

    // C. JettisonStructureFuel guards + explicit item / qty drain to 0 + OnStructureServiceChanged notify
    let kw = HashMap::new();
    assert_eq!(
        crate::services::station_structure::handle(&ctx, &mut docked_sess, "structure", "JettisonStructureFuel", &[PyRep::Integer(dead_sid), PyRep::Integer(1)], &kw).await.unwrap().unwrap_err().code,
        "StructureDestroyed"
    );
    let mut mem_sess = docked_sess.clone();
    mem_sess.character_id = Some(member_id);
    assert_eq!(
        crate::services::station_structure::handle(&ctx, &mut mem_sess, "structure", "JettisonStructureFuel", &[PyRep::Integer(sid), PyRep::Integer(1)], &kw).await.unwrap().unwrap_err().code,
        "InsufficientRoles"
    );
    assert_eq!(
        crate::services::station_structure::handle(&ctx, &mut docked_sess, "structure", "JettisonStructureFuel", &[PyRep::Integer(sid), PyRep::List(vec![PyRep::Integer(bad_flag_drop_id)])], &kw).await.unwrap().unwrap_err().code,
        "ItemNotInFuelBay"
    );
    assert_eq!(
        crate::services::station_structure::handle(&ctx, &mut docked_sess, "structure", "JettisonStructureFuel", &[PyRep::Integer(sid), PyRep::List(vec![PyRep::Integer(other_owner_fuel)])], &kw).await.unwrap().unwrap_err().code,
        "ItemOwnershipViolation"
    );
    assert_eq!(
        crate::services::station_structure::handle(&ctx, &mut docked_sess, "structure", "JettisonStructureFuel", &[PyRep::Integer(sid), PyRep::List(vec![PyRep::Integer(non_fuel_in_bay)])], &kw).await.unwrap().unwrap_err().code,
        "ItemNotFuel"
    );
    assert_eq!(
        crate::services::station_structure::handle(&ctx, &mut docked_sess, "structure", "JettisonStructureFuel", &[PyRep::Integer(sid), PyRep::Integer(34), PyRep::Integer(1)], &kw).await.unwrap().unwrap_err().code,
        "ItemNotFuel"
    );
    assert_eq!(
        crate::services::station_structure::handle(&ctx, &mut docked_sess, "structure", "JettisonStructureFuel", &[PyRep::Integer(sid), PyRep::Integer(4051), PyRep::Integer(999)], &kw).await.unwrap().unwrap_err().code,
        "InsufficientStructureFuel"
    );
    // Jettison explicit item1 via single integer arg inferring structure from item location, then drain remaining 5 via [sid, 5]
    assert!(
        crate::services::station_structure::handle(&ctx, &mut docked_sess, "structure", "JettisonStructureFuel", &[PyRep::Integer(fuel_bay_item1)], &kw).await.unwrap().is_ok()
    );
    assert!(
        crate::services::station_structure::handle(&ctx, &mut docked_sess, "structure", "JettisonStructureFuel", &[PyRep::Integer(sid), PyRep::Integer(5)], &kw).await.unwrap().is_ok()
    );
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.structures[&sid].fuel_blocks, 0);
        assert!(!s.items.contains_key(&fuel_bay_item2));
    });
    let _ = corp_roles::ROLE_DIRECTOR;
}


#[tokio::test]
async fn structure_cargo_fuel_extra_acl_service_offline_and_ship_deliver_guards() {
    use crate::services::station_structure::handle as dispatch_station_structure;

    let ctx = test_ctx();
    let (ceo_id, member_id, outsider_id, corp_id, ally_id, sid, offline_sid) = (
        860_001_i64,
        860_002_i64,
        860_003_i64,
        98_860_001_i64,
        99_860_001_i64,
        10_000_991_001_i64,
        10_000_991_002_i64,
    );
    let sys_id = 30_000_142_u32;
    seed_corp_and_structure(&ctx, ceo_id, member_id, outsider_id, corp_id, ally_id, sid, 35_833, sys_id);

    let ceo_ship = 960_001_i64;
    let out_ship = 960_002_i64;
    let fuel_item_id = 960_010_i64;
    let svc_mod_id = 960_011_i64;
    let cargo_drop_id = 960_012_i64;
    let out_cargo_id = 960_013_i64;

    let sys_h = ctx.systems.get_or_spawn(sys_id);
    for (ship, owner, c_id) in [(ceo_ship, ceo_id, corp_id), (out_ship, outsider_id, 98_999_999_i64)] {
        sys_h
            .spawn_ball_with_meta(
                eve_destiny::Ball {
                    id: ship,
                    type_id: 587,
                    position: glam::DVec3::new(1_000_000.0, 0.0, 0.0),
                    velocity: glam::DVec3::ZERO,
                    max_velocity: 350.0,
                    inertia: 2.0,
                    mass: 1_000_000.0,
                    radius: 40.0,
                    speed_fraction: 0.0,
                    mode: eve_destiny::MovementMode::Stop,
                },
                Default::default(),
                eve_destiny::SlimItemMeta {
                    owner_id: owner,
                    char_id: Some(owner),
                    corp_id: c_id,
                    group_id: 25,
                    category_id: 6,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
    }

    let now_unix = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        let mut off = snap.structures[&sid].clone();
        off.structure_id = offline_sid;
        off.state = "HullReinforced".into();
        off.reinforced_until_unix = now_unix.saturating_add(3_600);
        snap.structures.insert(offline_sid, off);

        let st = snap.structures.get_mut(&sid).unwrap();
        st.fuel_blocks = 5;

        let mut ext = load_ext_state(snap);
        ext.access_profiles.insert(
            sid,
            StructureAccessProfile {
                profile_id: 77,
                owner_corp_id: corp_id,
                corp_only: true,
                ..Default::default()
            },
        );
        save_ext_state(snap, &ext);

        for (iid, tid, owner, loc, flag, qty, singleton) in [
            (fuel_item_id, 4051, corp_id, sid, InventoryFlag::StructureFuel, 5, false),
            (svc_mod_id, 35_894, corp_id, sid, InventoryFlag::ServiceSlot0, 1, true),
            (cargo_drop_id, 34, ceo_id, ceo_ship, InventoryFlag::Cargo, 5, false),
            (out_cargo_id, 34, outsider_id, out_ship, InventoryFlag::Cargo, 5, false),
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: owner,
                    location_id: loc,
                    flag,
                    quantity: qty,
                    singleton,
                    version: 1,
                    ..Default::default()
                },
            );
        }
        snap.module_online_states.insert(svc_mod_id, true);
        snap.items.insert(
            ceo_ship,
            InventoryItemRecord {
                item_id: ceo_ship,
                type_id: 587,
                owner_id: ceo_id,
                location_id: sid,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    let mut ceo_sess = SessionState::new(860);
    ceo_sess.user_id = Some(ceo_id as u64);
    ceo_sess.character_id = Some(ceo_id);
    ceo_sess.corporation_id = Some(corp_id);
    ceo_sess.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
    ceo_sess.solar_system_id = Some(sys_id);
    ceo_sess.ship_id = Some(ceo_ship);

    // 1. Draining last fuel block offlines service module and emits OnStructureServiceChanged
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&ceo_id) {
            ch.station_id = None;
            ch.structure_id = Some(sid);
        }
    });
    let mut docked_ceo = ceo_sess.clone();
    docked_ceo.structure_id = Some(sid);
    let kw = HashMap::new();
    let j_res = dispatch_station_structure(
        &ctx,
        &mut docked_ceo,
        "structure",
        "JettisonStructureFuel",
        &[PyRep::Integer(sid), PyRep::Integer(4051), PyRep::Integer(5)],
        &kw,
    )
    .await
    .unwrap();
    j_res.expect("jettison fuel");

    // 2. Deliver cannot transfer pilot's currently active boarded ship
    assert_eq!(
        call_upwell(
            &ctx,
            &mut docked_ceo,
            "structureDeliveries",
            "Deliver",
            &[PyRep::Integer(sid), PyRep::Integer(ceo_ship), PyRep::Integer(corp_id)],
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "CannotDeliverActiveShip"
    );

    // 3. DropOffItems guards: unspawned ship (ShipNotInSpace), non-operational target, ACL denial
    ctx.db.with_mut_snapshot(|snap| {
        for cid in [ceo_id, outsider_id] {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.station_id = None;
                ch.structure_id = None;
                ch.solar_system_id = sys_id;
            }
        }
    });
    let mut no_ball_sess = ceo_sess.clone();
    no_ball_sess.ship_id = Some(999_444);
    assert_eq!(
        call_upwell(
            &ctx,
            &mut no_ball_sess,
            "structureCargoDelivery",
            "DropOffItems",
            &[PyRep::Integer(sid), PyRep::List(vec![PyRep::Integer(cargo_drop_id)])],
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "ShipNotInSpace"
    );

    assert_eq!(
        call_upwell(
            &ctx,
            &mut ceo_sess,
            "structureCargoDelivery",
            "DropOffItems",
            &[PyRep::Integer(offline_sid), PyRep::List(vec![PyRep::Integer(cargo_drop_id)])],
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "StructureNotOperational"
    );

    let mut out_sess = SessionState::new(861);
    out_sess.user_id = Some(outsider_id as u64);
    out_sess.character_id = Some(outsider_id);
    out_sess.corporation_id = Some(98_999_999);
    out_sess.solar_system_id = Some(sys_id);
    out_sess.ship_id = Some(out_ship);
    assert_eq!(
        call_upwell(
            &ctx,
            &mut out_sess,
            "structureCargoDelivery",
            "DropOffItems",
            &[PyRep::Integer(sid), PyRep::List(vec![PyRep::Integer(out_cargo_id)])],
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "StructureDockingAccessDenied"
    );
}
