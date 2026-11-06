//! Deep branch-coverage and mutation-killing unit tests for `services::station_structure::structure_upwell_ops`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::station_structure::structure_lifecycle::STATE_SHIELD_VULNERABLE;
use crate::services::station_structure::structure_upwell_ops::*;
use crate::services::structure_acl_defense_rules::{
    load_defense_settings, save_defense_settings, StructureDefenseSettings,
};
use crate::services::structure_profile_services::*;
use eve_db::{
    AllianceRecord, CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents,
    JumpCloneRecord, UpwellStructureRecord, WorldRepository,
};
use eve_machonet::{service_roles, SessionState};
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::SdeRegistry;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

async fn call_upwell(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    handle(ctx, session, service, method, args, &HashMap::new()).await
}

fn seed_corp_and_structure(
    ctx: &ServerContext,
    ceo_id: i64,
    member_id: i64,
    outsider_id: i64,
    corp_id: i64,
    ally_id: i64,
    sid: i64,
    type_id: u32,
    sys_id: u32,
) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.alliances.insert(
            ally_id,
            AllianceRecord {
                alliance_id: ally_id,
                name: "Upwell Ally".into(),
                short_name: "UPWA".into(),
                executor_corp_id: corp_id,
                member_corp_ids: HashSet::from([corp_id]),
                ..Default::default()
            },
        );
        let mut corp = CorporationRecord {
            corporation_id: corp_id,
            name: "Upwell Owner Corp".into(),
            ticker: "UOC".into(),
            ceo_id,
            alliance_id: Some(ally_id),
            hq_station_id: Some(60_003_760),
            tax_rate: 0.05,
            master_wallet: IskCents(500_000_000),
            members: HashSet::from([ceo_id, member_id]),
            ..Default::default()
        };
        corp.ensure_wallet_divisions();
        corp.set_division_balance(1, IskCents(500_000_000));
        snap.corporations.insert(corp_id, corp);

        let base_ch = snap.characters.values().next().cloned().unwrap();
        for (cid, c_corp, c_ally) in [
            (ceo_id, corp_id, Some(ally_id)),
            (member_id, corp_id, Some(ally_id)),
            (outsider_id, 98_999_999, None),
        ] {
            let mut ch = base_ch.clone();
            ch.character_id = cid;
            ch.corporation_id = c_corp;
            ch.alliance_id = c_ally;
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = 880_000 + cid;
            ch.balance = IskCents(500_000_000);
            snap.characters.insert(cid, ch);
        }

        snap.structures.insert(
            sid,
            UpwellStructureRecord {
                structure_id: sid,
                type_id,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "Extra Fortizar".into(),
                position: [1_000_000.0, 0.0, 0.0],
                state: STATE_SHIELD_VULNERABLE.into(),
                quantum_core_installed: true,
                fuel_blocks: 500,
                shield_hp: 10_000_000.0,
                armor_hp: 10_000_000.0,
                hull_hp: 10_000_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
    });
}

#[test]
fn service_id_mappings_market_order_rules_and_acl_group_access_matrix() {
    let ctx = test_ctx();
    for (tid, expected_svc) in [
        (35892, Some(SERVICE_MARKET)),
        (35894, Some(SERVICE_JUMP_CLONE)),
        (35899, Some(SERVICE_REPROCESSING)),
        (35878, Some(SERVICE_MANUFACTURING_BASIC)),
        (35891, Some(SERVICE_LABORATORY_RESEARCH_TIME)),
        (35886, Some(SERVICE_LABORATORY_INVENTION)),
        (45537, Some(SERVICE_REACTIONS_COMPOSITE)),
        (45538, Some(SERVICE_REACTIONS_BIOCHEMICAL)),
        (45539, Some(SERVICE_REACTIONS_HYBRID)),
        (45009, Some(SERVICE_MOONMINING)),
        (35913, Some(SERVICE_JUMP_BRIDGE)),
        (999_999, None),
    ] {
        assert_eq!(service_id_for_module_type(tid), expected_svc);
    }

    assert_eq!(reinforce_change_delay_secs(&ctx, 30_000_142), 30 * 86_400);
    assert_eq!(reinforce_change_delay_secs(&ctx, 31_000_001), 7 * 86_400);

    let (ceo_id, member_id, outsider_id) = (8101, 8102, 8103);
    let (corp_id, ally_id, fort_id, astra_id) = (98_810_001, 99_810_001, 10_000_950_001, 10_000_950_002);
    seed_corp_and_structure(&ctx, ceo_id, member_id, outsider_id, corp_id, ally_id, fort_id, 35833, 30_000_142);

    ctx.db.with_mut_snapshot(|snap| {
        let mut astra = snap.structures[&fort_id].clone();
        astra.structure_id = astra_id;
        astra.type_id = 35832;
        snap.structures.insert(astra_id, astra);

        for (idx, sid) in [(1, fort_id), (2, astra_id)] {
            let mid = 950_100 + idx;
            snap.items.insert(
                mid,
                InventoryItemRecord {
                    item_id: mid,
                    type_id: 35892,
                    owner_id: corp_id,
                    location_id: sid,
                    flag: InventoryFlag::from_u16(164),
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.module_online_states.insert(mid, true);
        }
    });

    ctx.db.with_snapshot(|snap| {
        assert!(structure_allows_market_orders(&ctx, snap, fort_id));
        assert!(!structure_allows_market_orders(&ctx, snap, astra_id));
        assert!(is_structure_service_online(&ctx, snap, fort_id, SERVICE_MARKET));
    });

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&fort_id).unwrap().fuel_blocks = 0;
    });
    ctx.db.with_snapshot(|snap| {
        assert!(!structure_allows_market_orders(&ctx, snap, fort_id));
    });

    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        let mut existing_ds = load_defense_settings(snap, fort_id);
        existing_ds.known_attackers.push(outsider_id);
        save_defense_settings(snap, &existing_ds);

        let mut ext = load_ext_state(snap);
        let mut grp = AccessGroupRecord {
            group_id: 501,
            creator_id: ceo_id,
            owner_corp_id: corp_id,
            name: "Friends".into(),
            description: String::new(),
            members: HashMap::new(),
        };
        grp.members.insert(outsider_id, 0); // allowed via group
        grp.members.insert(member_id, -1); // explicitly blocked via group (-1)
        ext.owner_groups.insert(501, grp);

        let prof = StructureAccessProfile {
            profile_id: 701,
            owner_corp_id: corp_id,
            name: "GroupProfile".into(),
            access_group_ids: HashSet::from([501]),
            defense_settings: Some(StructureDefenseSettings::default()),
            ..Default::default()
        };
        ext.profiles_by_id.insert(701, prof);
        ext.pending_profile_applications.insert(fort_id, (701, now + 300));
        save_ext_state(snap, &ext);
    });

    // Before due time, sync_pending_profiles_in_ctx is a no-op
    sync_pending_profiles_in_ctx(&ctx);
    assert!(ctx
        .db
        .with_snapshot(|s| load_ext_state(s).pending_profile_applications.contains_key(&fort_id)));

    ctx.advance_secs(310);
    sync_pending_profiles_in_ctx(&ctx);
    ctx.db.with_snapshot(|snap| {
        let ext = load_ext_state(snap);
        assert!(!ext.pending_profile_applications.contains_key(&fort_id));
        // Existing known_attackers must be preserved when template defense_settings.known_attackers was empty
        assert!(load_defense_settings(snap, fort_id)
            .known_attackers
            .contains(&outsider_id));
        // outsider_id is allowed via group 501 (member role 0 >= 0)
        assert!(evaluate_character_structure_access(&ctx, snap, fort_id, outsider_id));
        // member_id is blocked via group 501 (member role == -1)
        assert!(!evaluate_character_structure_access(&ctx, snap, fort_id, member_id));
        // Random character 999999 is not in group 501 and not in owner corp -> denied
        assert!(!evaluate_character_structure_access(&ctx, snap, fort_id, 999_999));
    });
}

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
    let map_data = call_upwell(&ctx, &mut sess, "structureDirectory", "GetStructureMapData", &[PyRep::Integer(30_000_142)])
        .await
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
