//! Additional edge-case and branch coverage tests for `services::structure_upwell_ops`.

#![forbid(unsafe_code)]

use super::*;
use crate::router::ServerContext;
use crate::services::station_structure::structure_lifecycle::{
    STATE_ANCHORING, STATE_ARMOR_REINFORCED, STATE_DESTROYED, STATE_SHIELD_VULNERABLE,
};
use eve_db::{
    AllianceRecord, CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents,
    UpwellStructureRecord,
};
use eve_machonet::{SessionState, service_roles};
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(eve_db::WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

fn make_session(char_id: i64, corp_id: i64, structure_id: Option<i64>, role: u64) -> SessionState {
    let mut s = SessionState::new(9750);
    s.user_id = Some(char_id as u64);
    s.character_id = Some(char_id);
    s.corporation_id = Some(corp_id);
    s.solar_system_id = Some(30_000_142);
    s.structure_id = structure_id;
    s.role = role;
    s.corp_role = u64::MAX;
    s
}

fn seed_fixture(ctx: &ServerContext, char_id: i64, corp_id: i64, sid: i64, sys_id: u32) {
    ctx.db.with_mut_snapshot(|snap| {
        let mut corp = { let mut __corp = CorporationRecord::default(); __corp.corporation_id = corp_id; __corp.name = format!("Edge Corp {corp_id}"); __corp.ticker = "EDG".into(); __corp.ceo_id = char_id; __corp.hq_station_id = Some(60_003_760); __corp.tax_rate = 0.05; __corp.members = HashSet::from([char_id]); let __mw = IskCents(500_000_000); if __mw.0 != 0 { __corp.set_division_balance(1, __mw); } else { __corp.ensure_wallet_divisions(); } __corp };
        corp.ensure_wallet_divisions();
        corp.set_division_balance(1, IskCents(500_000_000));
        snap.corporations.insert(corp_id, corp);

        let mut ch = snap.characters.values().next().cloned().expect("demo char");
        ch.character_id = char_id;
        ch.name = format!("Pilot {char_id}");
        ch.corporation_id = corp_id;
        ch.alliance_id = None;
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = Some(sid);
        ch.active_ship_id = 98_800_001;
        ch.home_station_id = sid;
        ch.balance = IskCents(500_000_000);
        snap.characters.insert(char_id, ch);

        snap.structures.insert(
            sid,
            UpwellStructureRecord {
                structure_id: sid,
                type_id: 35833, // Fortizar (Large)
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "Edge Fortizar".into(),
                position: [1_000_000_000.0, 0.0, 0.0],
                state: STATE_SHIELD_VULNERABLE.into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 500_000.0,
                armor_hp: 500_000.0,
                hull_hp: 500_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
    });
}

#[tokio::test(flavor = "current_thread")]
async fn deployment_guards_reinforce_schedule_and_pending_profile_edges() {
    let ctx = test_ctx();
    let sid = 10_000_930_001_i64;
    let char_id = 76_001_i64;
    let corp_id = 98_760_i64;
    seed_fixture(&ctx, char_id, corp_id, sid, 30_000_142);

    // reinforce_change_delay_secs: highsec vs nullsec/lowsec
    assert_eq!(reinforce_change_delay_secs(&ctx, 30_000_142), 30 * 86_400);
    if let Some(&low_sys) = ctx
        .sde
        .solar_systems
        .iter()
        .find(|(_, s)| s.security_status < 0.45)
        .map(|(id, _)| id)
    {
        assert_eq!(reinforce_change_delay_secs(&ctx, low_sys), 7 * 86_400);
    }

    let mut sess = make_session(char_id, corp_id, Some(sid), 1);
    let kw = HashMap::new();

    // Unanchor missing id / unknown / ownership violation / destroyed / reinforced
    assert_eq!(
        handle(&ctx, &mut sess, "structureDeployment", "Unanchor", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "MissingStructureId"
    );
    assert_eq!(
        handle(&ctx, &mut sess, "structureDeployment", "Unanchor", &[PyRep::Integer(99_999_999)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "StructureNotFound"
    );

    let mut foreign_sess = make_session(char_id + 1, corp_id + 1, Some(sid), 1);
    assert_eq!(
        handle(&ctx, &mut foreign_sess, "structureDeployment", "Unanchor", &[PyRep::Integer(sid)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "StructureOwnershipViolation"
    );

    // CancelUnanchor when not unanchoring -> StructureNotUnanchoring
    assert_eq!(
        handle(&ctx, &mut sess, "structureDeployment", "CancelUnanchor", &[PyRep::Integer(sid)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "StructureNotUnanchoring"
    );

    // Reinforced structure cannot unanchor
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&sid).unwrap().state = STATE_ARMOR_REINFORCED.into();
    });
    assert_eq!(
        handle(&ctx, &mut sess, "structureDeployment", "Unanchor", &[PyRep::Integer(sid)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "CannotUnanchorReinforcedStructure"
    );

    // RenameStructure validation
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "structureDeployment",
            "RenameStructure",
            &[PyRep::Integer(sid), PyRep::String("   ".into())],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidStructureName"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "structureDeployment",
            "RenameStructure",
            &[PyRep::Integer(sid), PyRep::String("x".repeat(101))],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidStructureName"
    );

    // UpdateReinforceTimesFromClient bounds
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "structureVulnerability",
            "UpdateReinforceTimesFromClient",
            &[PyRep::Integer(sid), PyRep::Integer(24), PyRep::Integer(0)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidReinforceSchedule"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "structureVulnerability",
            "UpdateReinforceTimesFromClient",
            &[PyRep::Integer(sid), PyRep::Integer(12), PyRep::Integer(7)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidReinforceSchedule"
    );

    // apply_pending_reinforce_schedule: blocked while reinforced, then applied once vulnerable and due
    let now = 1_800_000_000_u64;
    let prof = super::super::structure_lifecycle::structure_profile(&ctx, 35833).unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        {
            let st = snap.structures.get_mut(&sid).unwrap();
            st.shield_hp = prof.shield_hp;
            st.armor_hp = prof.armor_hp;
        }
        let mut ext = load_ext_state(snap);
        ext.pending_reinforce_schedule.insert(sid, (19, 5, now - 10));
        apply_pending_reinforce_schedule(&ctx, snap, &mut ext, sid, now);
        assert!(ext.pending_reinforce_schedule.contains_key(&sid));

        snap.structures.get_mut(&sid).unwrap().state = STATE_SHIELD_VULNERABLE.into();
        // Also seed pending_profile_applications and test before due vs due
        ext.profiles_by_id.insert(
            42,
            StructureAccessProfile {
                profile_id: 42,
                name: "Edge Profile".into(),
                ..Default::default()
            },
        );
        ext.pending_profile_applications.insert(sid, (42, now + 500));
        apply_pending_profile_applications(&mut ext, now);
        assert!(ext.pending_profile_applications.contains_key(&sid));

        apply_pending_reinforce_schedule(&ctx, snap, &mut ext, sid, now);
        assert!(!ext.pending_reinforce_schedule.contains_key(&sid));
        assert_eq!(snap.structure_vulnerability_windows.get(&sid), Some(&(19, 5)));

        apply_pending_profile_applications(&mut ext, now + 600);
        assert!(!ext.pending_profile_applications.contains_key(&sid));
        assert_eq!(ext.structure_profile_id.get(&sid), Some(&42));
        save_ext_state(snap, &ext);
    });

    // Destroyed structure check
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&sid).unwrap().state = STATE_DESTROYED.into();
    });
    assert_eq!(
        handle(&ctx, &mut sess, "structureDirectory", "GetStructureDescription", &[PyRep::Integer(sid)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "StructureNotFound"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn asset_safety_guards_move_wrap_cooldown_destination_and_gm_eject_edges() {
    let ctx = test_ctx();
    let sid = 10_000_930_002_i64;
    let sid_wh = 10_000_930_003_i64;
    let char_id = 76_002_i64;
    let corp_id = 98_761_i64;
    seed_fixture(&ctx, char_id, corp_id, sid, 30_000_142);

    ctx.db.with_mut_snapshot(|snap| {
        let mut wh = snap.structures[&sid].clone();
        wh.structure_id = sid_wh;
        wh.solar_system_id = 31_000_001; // wormhole
        snap.structures.insert(sid_wh, wh);
    });

    // 1. Personal MovePersonalAssetsToSafety fails if docked in target structure
    let mut docked_sess = make_session(char_id, corp_id, Some(sid), 1);
    let kw = HashMap::new();
    assert_eq!(
        handle(
            &ctx,
            &mut docked_sess,
            "structureAssetSafety",
            "MovePersonalAssetsToSafety",
            &[PyRep::Integer(30_000_142), PyRep::Integer(sid)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "CannotAssetSafetyWhileDockedInTargetStructure"
    );

    // 2. Wormhole structure returns AssetSafetyUnavailableInWormhole
    let mut space_sess = make_session(char_id, corp_id, None, 1);
    assert_eq!(
        handle(
            &ctx,
            &mut space_sess,
            "structureAssetSafety",
            "MovePersonalAssetsToSafety",
            &[PyRep::Integer(31_000_001), PyRep::Integer(sid_wh)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "AssetSafetyUnavailableInWormhole"
    );

    // 3. NoAssetsToMoveToSafety when only excluded items exist (active ship, locked item)
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            98_800_001,
            InventoryItemRecord {
                item_id: 98_800_001,
                type_id: 587,
                owner_id: char_id,
                location_id: sid,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            98_800_002,
            InventoryItemRecord {
                item_id: 98_800_002,
                type_id: 34,
                owner_id: char_id,
                location_id: sid,
                flag: InventoryFlag::Locked,
                quantity: 100,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });
    assert_eq!(
        handle(
            &ctx,
            &mut space_sess,
            "structureAssetSafety",
            "MovePersonalAssetsToSafety",
            &[PyRep::Integer(30_000_142), PyRep::Integer(sid)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "NoAssetsToMoveToSafety"
    );

    // 4. MoveCorpAssetsToSafety role & ownership guards
    let mut non_dir = make_session(char_id, corp_id, None, 1);
    non_dir.corp_role = 0;
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.get_mut(&corp_id).unwrap().ceo_id = char_id + 99;
    });
    assert_eq!(
        handle(
            &ctx,
            &mut non_dir,
            "structureAssetSafety",
            "MoveCorpAssetsToSafety",
            &[PyRep::Integer(30_000_142), PyRep::Integer(sid)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InsufficientCorpRoles"
    );

    // Restore CEO and add personal moveable item -> wrap -> test DeliverToStructure / MoveEjectTimeGM
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.get_mut(&corp_id).unwrap().ceo_id = char_id;
        snap.items.insert(
            98_800_010,
            InventoryItemRecord {
                item_id: 98_800_010,
                type_id: 587,
                owner_id: char_id,
                location_id: sid,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    let mv_res = handle(
        &ctx,
        &mut space_sess,
        "structureAssetSafety",
        "MovePersonalAssetsToSafety",
        &[PyRep::Integer(30_000_142), PyRep::Integer(sid)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let wrap_id = mv_res.dict_get("wrapID").and_then(PyRep::as_i64).unwrap();

    // Cooldown active before MoveEjectTimeGM
    assert_eq!(
        handle(
            &ctx,
            &mut space_sess,
            "structureAssetSafety",
            "MoveSafetyWrapToStructure",
            &[PyRep::Integer(wrap_id), PyRep::Integer(60_003_760)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "AssetSafetyCooldownActive"
    );

    // Non-GM MoveEjectTimeGM -> InsufficientRole
    assert_eq!(
        handle(
            &ctx,
            &mut space_sess,
            "structureAssetSafety",
            "MoveEjectTimeGM",
            &[PyRep::Integer(wrap_id), PyRep::Integer(0)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InsufficientRole"
    );

    // GM sets deliverable_after_unix = 0
    let mut gm_sess = make_session(char_id, corp_id, None, service_roles::ROLE_GML);
    let ej_res = handle(
        &ctx,
        &mut gm_sess,
        "structureAssetSafety",
        "MoveEjectTimeGM",
        &[PyRep::Integer(wrap_id), PyRep::Integer(0)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(ej_res, PyRep::Bool(true));

    // Invalid destination station in wrong solar system (Amarr 60_008_494)
    assert_eq!(
        handle(
            &ctx,
            &mut space_sess,
            "structureAssetSafety",
            "MoveSafetyWrapToStructure",
            &[PyRep::Integer(wrap_id), PyRep::Integer(60_008_494)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidAssetSafetyDestination"
    );

    // GetWrapNames with List & Integer + GetStructuresICanDeliverTo + GetMyHangarViewState not docked
    let wrap_names = handle(
        &ctx,
        &mut space_sess,
        "structureAssetSafety",
        "GetWrapNames",
        &[PyRep::Tuple(vec![PyRep::Integer(wrap_id)])],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(wrap_names, PyRep::Dict(_)));

    let wrap_single = handle(
        &ctx,
        &mut space_sess,
        "structureAssetSafety",
        "GetWrapNames",
        &[PyRep::Integer(wrap_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(wrap_single, PyRep::Dict(_)));

    let deliv_list = handle(
        &ctx,
        &mut space_sess,
        "structureAssetSafety",
        "GetStructuresICanDeliverTo",
        &[PyRep::Integer(30_000_142)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(deliv_list, PyRep::List(_)));

    assert_eq!(
        handle(
            &ctx,
            &mut space_sess,
            "structureHangarViewMgr",
            "GetMyHangarViewState",
            &[],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "NotDockedInStructure"
    );

    // Deliver wrap to Jita 4-4 (60_003_760), then repeat -> AssetSafetyWrapAlreadyDelivered
    let _ = handle(
        &ctx,
        &mut space_sess,
        "structureAssetSafety",
        "MoveSafetyWrapToStructure",
        &[PyRep::Integer(wrap_id), PyRep::Integer(60_003_760)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        handle(
            &ctx,
            &mut space_sess,
            "structureAssetSafety",
            "MoveSafetyWrapToStructure",
            &[PyRep::Integer(wrap_id), PyRep::Integer(60_003_760)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "AssetSafetyWrapAlreadyDelivered"
    );

    // sync_auto_delivered_asset_safety_wraps: seed another wrap older than 20 days
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        ext.asset_safety_wraps.insert(
            98_900_001,
            AssetSafetyWrapRecord {
                wrap_id: 98_900_001,
                owner_id: char_id,
                is_corp: false,
                source_structure_id: sid,
                source_structure_name: "Edge Fortizar".into(),
                solar_system_id: 30_000_142,
                nearest_npc_station_id: Some(60_003_760),
                item_ids: vec![],
                estimated_value_isk: 1_000_000.0,
                created_unix: 100,
                deliverable_after_unix: 200,
                delivered_to_id: None,
                auto_delivered: false,
                retrieval_fee_paid: false,
            },
        );
        save_ext_state(snap, &ext);
    });
    sync_auto_delivered_asset_safety_wraps(&ctx);
    ctx.db.with_snapshot(|snap| {
        let ext_after = load_ext_state(snap);
        assert!(ext_after.asset_safety_wraps[&98_900_001].auto_delivered);
        assert_eq!(ext_after.asset_safety_wraps[&98_900_001].delivered_to_id, Some(60_003_760));
    });
}

#[test]
fn service_id_market_eligibility_acl_credit_and_ownership_transfer_edges() {
    // 1. All 11 Standup service module mappings + unknown
    let expected = [
        (35892, 5),
        (35894, 10),
        (35899, 4),
        (35878, 22),
        (35891, 31),
        (35886, 34),
        (45537, 41),
        (45538, 42),
        (45539, 43),
        (45009, 14),
        (35913, 15),
    ];
    for (tid, svc_id) in expected {
        assert_eq!(service_id_for_module_type(tid), Some(svc_id));
    }
    assert_eq!(service_id_for_module_type(34), None);

    let ctx = test_ctx();
    let sid = 10_000_930_004_i64;
    let char_id = 76_003_i64;
    let other_char = 76_004_i64;
    let corp_a = 98_762_i64;
    let corp_b = 98_763_i64;
    seed_fixture(&ctx, char_id, corp_a, sid, 30_000_142);

    ctx.db.with_mut_snapshot(|snap| {
        let mut corp_b_rec = snap.corporations[&corp_a].clone();
        corp_b_rec.corporation_id = corp_b;
        corp_b_rec.members = HashSet::from([other_char]);
        snap.corporations.insert(corp_b, corp_b_rec);

        let mut ch_b = snap.characters[&char_id].clone();
        ch_b.character_id = other_char;
        ch_b.corporation_id = corp_b;
        ch_b.alliance_id = Some(99_111_001);
        snap.characters.insert(other_char, ch_b);

        snap.alliances.insert(
            99_111_001,
            AllianceRecord {
                alliance_id: 99_111_001,
                name: "Edge Alliance".into(),
                short_name: "EDGA".into(),
                executor_corp_id: corp_b,
                ..Default::default()
            },
        );
    });

    // structure_allows_market_orders: missing/no core/no fuel/medium hull/market module online
    assert!(ctx.db.with_snapshot(|snap| !structure_allows_market_orders(&ctx, snap, 999_999)));
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&sid).unwrap().quantum_core_installed = false;
        assert!(!structure_allows_market_orders(&ctx, snap, sid));
        snap.structures.get_mut(&sid).unwrap().quantum_core_installed = true;
        snap.structures.get_mut(&sid).unwrap().type_id = 35832; // Astrahus (Medium) cannot run market
        assert!(!structure_allows_market_orders(&ctx, snap, sid));
        snap.structures.get_mut(&sid).unwrap().type_id = 35833; // Fortizar
        // Still false without Standup Market Hub 35892 fitted & online
        assert!(!structure_allows_market_orders(&ctx, snap, sid));
        snap.module_online_states.insert(98_800_050, true);
        snap.items.insert(
            98_800_050,
            InventoryItemRecord {
                item_id: 98_800_050,
                type_id: 35892,
                owner_id: corp_a,
                location_id: sid,
                flag: InventoryFlag::ServiceSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        assert!(structure_allows_market_orders(&ctx, snap, sid));
    });

    // validate_and_classify_entity + configure_structure_access_profile invalid tax + modify node
    assert_eq!(
        ctx.db.with_snapshot(|s| validate_and_classify_entity(s, 777_666_555)).unwrap_err().code,
        "InvalidAccessEntity"
    );
    assert_eq!(
        ctx.db.with_snapshot(|s| validate_and_classify_entity(s, 99_111_001)).unwrap(),
        (true, false, false)
    );

    let sess = make_session(char_id, corp_a, Some(sid), 1);
    assert_eq!(
        configure_structure_access_profile(&ctx, &sess, sid, Some(true), &[(10, 1.5)], &[], &[], &[])
            .unwrap_err()
            .code,
        "InvalidStructureTaxRate"
    );

    configure_structure_access_profile(&ctx, &sess, sid, Some(false), &[(10, 0.02)], &[], &[], &[]).unwrap();
    modify_structure_access_node(&ctx, &sess, sid, other_char, true, true).unwrap();
    assert!(!ctx.db.with_snapshot(|s| evaluate_character_structure_access_in_snap_at(s, sid, other_char, Some(ctx.now_unix()))));
    modify_structure_access_node(&ctx, &sess, sid, other_char, true, false).unwrap();
    modify_structure_access_node(&ctx, &sess, sid, corp_b, false, true).unwrap();
    assert!(ctx.db.with_snapshot(|s| evaluate_character_structure_access_in_snap_at(s, sid, other_char, Some(ctx.now_unix()))));

    // credit_structure_owner_corp_div1: <= 0 no-op vs > 0 credits
    ctx.db.with_mut_snapshot(|s| {
        let before = s.corporations[&corp_a].division_balance(1);
        credit_structure_owner_corp_div1(s, corp_a, IskCents(0), 1, "MarketTax", char_id, "zero".into(), 1000);
        assert_eq!(s.corporations[&corp_a].division_balance(1), before);
        credit_structure_owner_corp_div1(s, corp_a, IskCents(50_000), 1, "MarketTax", char_id, "fee".into(), 1000);
        assert_eq!(s.corporations[&corp_a].division_balance(1), IskCents(before.0 + 50_000));
    });

    // transfer_structure_corp_ownership_in_snap: anchoring state rejected, wrong corp rejected, valid succeeds
    ctx.db.with_mut_snapshot(|s| {
        s.structures.get_mut(&sid).unwrap().state = STATE_ANCHORING.into();
        assert!(transfer_structure_corp_ownership_in_snap(s, sid, corp_a, corp_b, 30_000_142).is_err());
        s.structures.get_mut(&sid).unwrap().state = STATE_SHIELD_VULNERABLE.into();
        assert!(transfer_structure_corp_ownership_in_snap(s, sid, corp_b, corp_a, 30_000_142).is_err());
        transfer_structure_corp_ownership_in_snap(s, sid, corp_a, corp_b, 30_000_142).unwrap();
        assert_eq!(s.structures[&sid].owner_corp_id, corp_b);
        assert_eq!(s.items[&98_800_050].owner_id, corp_b);
    });
}

#[tokio::test(flavor = "current_thread")]
async fn mutants_cov_ext_state_safety_roles_and_profile_managers() {
    use eve_db::corp_roles::{
        ROLE_ACCOUNTANT, ROLE_DIRECTOR, ROLE_STARBASE_CONFIG, ROLE_STARBASE_OPERATOR,
        ROLE_STATION_MANAGER,
    };
    let ctx = test_ctx();
    let sid = 10_000_940_001_i64;
    let other_sid = 10_000_940_002_i64;
    let char_id = 77_101_i64;
    let other_char_id = 77_102_i64;
    let corp_id = 98_771_i64;
    let other_corp_id = 98_772_i64;
    seed_fixture(&ctx, char_id, corp_id, sid, 30_000_142);

    // 1. save_ext_state branches (empty snap + default -> noop; non-default -> writes; unchanged -> keeps existing raw text)
    ctx.db.with_mut_snapshot(|snap| {
        snap.item_custom_names.remove(&STRUCT_EXT_STATE_KEY);
        save_ext_state(snap, &StructureExtendedState::default());
        assert!(!snap.item_custom_names.contains_key(&STRUCT_EXT_STATE_KEY));

        let mut ext = StructureExtendedState::default();
        ext.descriptions.insert(sid, "Alpha Desc".into());
        save_ext_state(snap, &ext);
        assert!(snap.item_custom_names.contains_key(&STRUCT_EXT_STATE_KEY));
        assert_eq!(load_ext_state(snap).descriptions.get(&sid).map(String::as_str), Some("Alpha Desc"));

        // Non-canonical JSON with whitespace representing default state: load_ext_state == default -> save_ext_state must return early and keep raw string.
        snap.item_custom_names.insert(STRUCT_EXT_STATE_KEY, "  {  }  ".into());
        save_ext_state(snap, &StructureExtendedState::default());
        assert_eq!(snap.item_custom_names.get(&STRUCT_EXT_STATE_KEY).map(String::as_str), Some("  {  }  "));
        // Different state must overwrite the raw string.
        save_ext_state(snap, &ext);
        assert_ne!(snap.item_custom_names.get(&STRUCT_EXT_STATE_KEY).map(String::as_str), Some("  {  }  "));
    });

    // Seed second corp & foreign structure + give owner corp funds for recovery fee.
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.get_mut(&corp_id).unwrap().set_division_balance(1, eve_db::IskCents(1_000_000_000));
        let mut c2 = snap.corporations[&corp_id].clone();
        c2.corporation_id = other_corp_id;
        c2.ceo_id = other_char_id;
        c2.member_roles.clear();
        snap.corporations.insert(other_corp_id, c2);

        let mut st2 = snap.structures[&sid].clone();
        st2.structure_id = other_sid;
        st2.owner_corp_id = other_corp_id;
        snap.structures.insert(other_sid, st2);
    });

    // 2. MoveCorpAssetsToSafety & MoveSafetyWrapToStructure director checks (is_ceo / session.corp_role / member_roles)
    let kw = HashMap::new();
    let add_corp_item = |iid: i64| {
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: 34,
                    owner_id: corp_id,
                    location_id: sid,
                    flag: InventoryFlag::CorpSAG1,
                    quantity: 10,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
        });
    };

    // Not CEO, no director role -> rejected
    ctx.db.with_mut_snapshot(|snap| {
        let corp = snap.corporations.get_mut(&corp_id).unwrap();
        corp.ceo_id = other_char_id;
        corp.member_roles.clear();
        corp.member_roles.insert(char_id, ROLE_STATION_MANAGER);
    });
    add_corp_item(98_900_001);
    let mut sess = make_session(char_id, corp_id, None, 0);
    sess.corp_role = ROLE_STATION_MANAGER;
    assert_eq!(
        handle(&ctx, &mut sess, "structureAssetSafety", "MoveCorpAssetsToSafety", &[PyRep::Integer(30_000_142), PyRep::Integer(sid)], &kw)
            .await.unwrap().unwrap_err().code,
        "InsufficientCorpRoles"
    );

    // Non-CEO with DB + session ROLE_DIRECTOR -> MoveCorpAssetsToSafety succeeds
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations
            .get_mut(&corp_id)
            .unwrap()
            .member_roles
            .insert(char_id, ROLE_DIRECTOR);
    });
    sess.corp_role = ROLE_DIRECTOR;
    let mv_dir = handle(&ctx, &mut sess, "structureAssetSafety", "MoveCorpAssetsToSafety", &[PyRep::Integer(30_000_142), PyRep::Integer(sid)], &kw)
        .await.unwrap().unwrap();
    let wrap_dir = mv_dir.dict_get("wrapID").and_then(PyRep::as_i64).unwrap();
    assert_eq!(mv_dir.dict_get("ownerID").and_then(PyRep::as_i64), Some(corp_id));

    // Cooldown bypass via MoveEjectTimeGM (also check non-existent wrapID returns Err)
    let mut gm = make_session(char_id, corp_id, None, service_roles::ROLE_GML);
    assert!(
        handle(&ctx, &mut gm, "structureAssetSafety", "MoveEjectTimeGM", &[PyRep::Integer(999_999_999), PyRep::Integer(0)], &kw)
            .await.unwrap().is_err()
    );
    assert_eq!(
        handle(&ctx, &mut gm, "structureAssetSafety", "MoveEjectTimeGM", &[PyRep::Integer(wrap_dir), PyRep::Integer(123_456)], &kw)
            .await.unwrap().unwrap(),
        PyRep::Bool(true)
    );
    ctx.db.with_snapshot(|snap| {
        assert_eq!(load_ext_state(snap).asset_safety_wraps[&wrap_dir].deliverable_after_unix, 123_456);
    });

    // MoveSafetyWrapToStructure on corp wrap: non-CEO + ROLE_STATION_MANAGER rejected -> ROLE_DIRECTOR succeeds
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations
            .get_mut(&corp_id)
            .unwrap()
            .member_roles
            .insert(char_id, ROLE_STATION_MANAGER);
    });
    let mut non_dir = make_session(char_id, corp_id, None, 0);
    non_dir.corp_role = ROLE_STATION_MANAGER;
    assert_eq!(
        handle(&ctx, &mut non_dir, "structureAssetSafety", "MoveSafetyWrapToStructure", &[PyRep::Integer(wrap_dir), PyRep::Integer(60_003_760)], &kw)
            .await.unwrap().unwrap_err().code,
        "InsufficientCorpRoles"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations
            .get_mut(&corp_id)
            .unwrap()
            .member_roles
            .insert(char_id, ROLE_DIRECTOR);
    });
    let mut dir_only = make_session(char_id, corp_id, None, 0);
    dir_only.corp_role = ROLE_DIRECTOR;
    let del_dir = handle(&ctx, &mut dir_only, "structureAssetSafety", "MoveSafetyWrapToStructure", &[PyRep::Integer(wrap_dir), PyRep::Integer(60_003_760)], &kw)
        .await.unwrap().unwrap();
    assert_eq!(del_dir.dict_get("wrapID").and_then(PyRep::as_i64), Some(wrap_dir));

    // Non-CEO with session.corp_role == 0 and member_roles[char_id] == ROLE_DIRECTOR -> MoveCorpAssetsToSafety & Deliver succeed
    add_corp_item(98_900_002);
    ctx.db.with_mut_snapshot(|snap| {
        let corp = snap.corporations.get_mut(&corp_id).unwrap();
        corp.ceo_id = other_char_id;
        corp.member_roles.insert(char_id, ROLE_DIRECTOR);
    });
    let mut role_via_db = make_session(char_id, corp_id, None, 0);
    role_via_db.corp_role = 0;
    let mv_db = handle(&ctx, &mut role_via_db, "structureAssetSafety", "MoveCorpAssetsToSafety", &[PyRep::Integer(30_000_142), PyRep::Integer(sid)], &kw)
        .await.unwrap().unwrap();
    let wrap_db = mv_db.dict_get("wrapID").and_then(PyRep::as_i64).unwrap();
    assert_eq!(
        handle(&ctx, &mut gm, "structureAssetSafety", "MoveEjectTimeGM", &[PyRep::Integer(wrap_db), PyRep::Integer(0)], &kw)
            .await.unwrap().unwrap(),
        PyRep::Bool(true)
    );
    let del_db = handle(&ctx, &mut role_via_db, "structureAssetSafety", "MoveSafetyWrapToStructure", &[PyRep::Integer(wrap_db), PyRep::Integer(60_003_760)], &kw)
        .await.unwrap().unwrap();
    assert_eq!(del_db.dict_get("destinationID").and_then(PyRep::as_i64), Some(60_003_760));

    // CEO with 0 corp_role and empty member_roles -> MoveCorpAssetsToSafety & Deliver succeed
    add_corp_item(98_900_003);
    ctx.db.with_mut_snapshot(|snap| {
        let corp = snap.corporations.get_mut(&corp_id).unwrap();
        corp.ceo_id = char_id;
        corp.member_roles.clear();
    });
    let mv_ceo = handle(&ctx, &mut role_via_db, "structureAssetSafety", "MoveCorpAssetsToSafety", &[PyRep::Integer(30_000_142), PyRep::Integer(sid)], &kw)
        .await.unwrap().unwrap();
    let wrap_ceo = mv_ceo.dict_get("wrapID").and_then(PyRep::as_i64).unwrap();
    assert_eq!(
        handle(&ctx, &mut gm, "structureAssetSafety", "MoveEjectTimeGM", &[PyRep::Integer(wrap_ceo), PyRep::Integer(0)], &kw)
            .await.unwrap().unwrap(),
        PyRep::Bool(true)
    );
    assert!(
        handle(&ctx, &mut role_via_db, "structureAssetSafety", "MoveSafetyWrapToStructure", &[PyRep::Integer(wrap_ceo), PyRep::Integer(60_003_760)], &kw)
            .await.unwrap().is_ok()
    );

    // 3. require_structure_control_role_at: CEO / 4 CTRL_ROLES / member_roles / non-ctrl role / legacy empty corp
    ctx.db.with_mut_snapshot(|snap| {
        let mut s_plain = make_session(char_id, corp_id, None, 0);
        s_plain.corp_role = 0;

        // Legacy corp (ceo_id == 0 && member_roles.is_empty()) -> allowed
        {
            let corp = snap.corporations.get_mut(&corp_id).unwrap();
            corp.ceo_id = 0;
            corp.member_roles.clear();
        }
        let st = snap.structures[&sid].clone();
        assert!(require_structure_control_role_at(snap, char_id, &st, 1_800_000_000).is_ok());

        // ceo_id == 0 but member_roles non-empty (other_char has role) -> char_id with 0 role denied!
        snap.corporations.get_mut(&corp_id).unwrap().member_roles.insert(other_char_id, ROLE_DIRECTOR);
        assert!(require_structure_control_role_at(snap, char_id, &st, 1_800_000_000).is_err());

        // ceo_id == other_char_id, non-control role (ROLE_ACCOUNTANT) -> denied
        snap.corporations.get_mut(&corp_id).unwrap().ceo_id = other_char_id;
        snap.corporations.get_mut(&corp_id).unwrap().member_roles.insert(char_id, ROLE_ACCOUNTANT);
        assert!(require_structure_control_role_at(snap, char_id, &st, 1_800_000_000).is_err());

        // All 4 CTRL_ROLES via member_roles must succeed
        for role_bit in [ROLE_DIRECTOR, ROLE_STATION_MANAGER, ROLE_STARBASE_OPERATOR, ROLE_STARBASE_CONFIG] {
            snap.corporations.get_mut(&corp_id).unwrap().member_roles.insert(char_id, role_bit);
            assert!(require_structure_control_role_at(snap, char_id, &st, 1_800_000_000).is_ok());
        }

        // CEO allowed even without role bits
        snap.corporations.get_mut(&corp_id).unwrap().member_roles.clear();
        snap.corporations.get_mut(&corp_id).unwrap().ceo_id = char_id;
        assert!(require_structure_control_role_at(snap, char_id, &st, 1_800_000_000).is_ok());

        // 4. require_corp_profile_manager & require_structure_corp_manager
        snap.corporations.get_mut(&corp_id).unwrap().ceo_id = other_char_id;
        snap.corporations.get_mut(&corp_id).unwrap().member_roles.clear();
        s_plain.corp_role = 0;
        assert!(require_corp_profile_manager(snap, &s_plain, char_id, corp_id).is_err());
        s_plain.corp_role = ROLE_STARBASE_OPERATOR;
        assert!(require_corp_profile_manager(snap, &s_plain, char_id, corp_id).is_err());
        s_plain.corp_role = ROLE_DIRECTOR;
        assert!(require_corp_profile_manager(snap, &s_plain, char_id, corp_id).is_ok());
        s_plain.corp_role = ROLE_STATION_MANAGER;
        assert!(require_corp_profile_manager(snap, &s_plain, char_id, corp_id).is_ok());
        s_plain.corp_role = 0;
        snap.corporations.get_mut(&corp_id).unwrap().member_roles.insert(char_id, ROLE_STATION_MANAGER);
        assert!(require_corp_profile_manager(snap, &s_plain, char_id, corp_id).is_ok());
        snap.corporations.get_mut(&corp_id).unwrap().member_roles.clear();
        snap.corporations.get_mut(&corp_id).unwrap().ceo_id = char_id;
        assert!(require_corp_profile_manager(snap, &s_plain, char_id, corp_id).is_ok());

        // require_structure_corp_manager
        snap.corporations.get_mut(&corp_id).unwrap().ceo_id = other_char_id;
        s_plain.corp_role = ROLE_DIRECTOR;
        assert_eq!(require_structure_corp_manager(snap, &s_plain, char_id, sid).unwrap().structure_id, sid);
        assert_eq!(
            require_structure_corp_manager(snap, &s_plain, char_id, other_sid).unwrap_err().code,
            "StructureOwnershipViolation"
        );
        s_plain.corp_role = ROLE_STATION_MANAGER;
        assert_eq!(require_structure_corp_manager(snap, &s_plain, char_id, sid).unwrap().structure_id, sid);
        s_plain.corp_role = 0;
        assert!(require_structure_corp_manager(snap, &s_plain, char_id, sid).is_err());

        // 5. credit_structure_owner_corp_div1 (0 cents noop vs >0 appends wallet_journal entry & updates div1 balance)
        let before_bal = snap.corporations[&corp_id].division_balance(1);
        let before_j = snap.wallet_journal.len();
        credit_structure_owner_corp_div1(snap, corp_id, eve_db::IskCents(0), 42, "MarketEscrow", char_id, "zero".into(), 1_800_000_000);
        assert_eq!(snap.corporations[&corp_id].division_balance(1), before_bal);
        assert_eq!(snap.wallet_journal.len(), before_j);

        credit_structure_owner_corp_div1(snap, corp_id, eve_db::IskCents(75_000), 42, "MarketEscrow", char_id, "fee credit".into(), 1_800_000_000);
        assert_eq!(snap.corporations[&corp_id].division_balance(1), eve_db::IskCents(before_bal.0 + 75_000));
        assert_eq!(snap.wallet_journal.len(), before_j + 1);
        let entry = snap.wallet_journal.last().unwrap();
        assert_eq!(entry.character_id, corp_id);
        assert_eq!(entry.entry_type, "MarketEscrow");
        assert_eq!(entry.amount, eve_db::IskCents(75_000));
        assert_eq!(entry.reason, "fee credit");
    });
}

