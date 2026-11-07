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
