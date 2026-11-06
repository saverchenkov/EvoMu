//! Deep branch-coverage and mutation-killing unit tests for `structure_profile_services`.

#![forbid(unsafe_code)]

use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::{
    clone_char_with, test_ctx_with_clock,
};
use crate::services::station_structure::structure_upwell_ops::{
    load_ext_state, save_ext_state, AccessGroupRecord, StructureAccessProfile,
    StructureExtendedState,
};
use crate::services::structure_profile_services::*;
use eve_db::{
    corp_roles, AllianceRecord, CorporationRecord, InventoryFlag, InventoryItemRecord,
    UpwellStructureRecord,
};
use eve_machonet::{service_roles, SessionState};
use eve_proto::PyRep;
use std::collections::{HashMap, HashSet};

fn make_upwell_struct(sid: i64, tid: u32, corp_id: i64) -> UpwellStructureRecord {
    UpwellStructureRecord {
        structure_id: sid,
        type_id: tid,
        owner_corp_id: corp_id,
        solar_system_id: 30_000_142,
        name: format!("S-{sid}"),
        position: [0.0; 3],
        state: "ShieldVulnerable".into(),
        quantum_core_installed: true,
        fuel_blocks: 500,
        shield_hp: 1.0,
        armor_hp: 1.0,
        hull_hp: 1.0,
        reinforced_until_unix: 0,
        last_damage_tick: 0,
        last_damage_unix: 0,
        self_destruct_at_unix: None,
    }
}

#[test]
fn bps_encoding_boundaries_flat_isk_vs_percentage_and_acl_group_validation_matrix() {
    for flat_id in [
        SETTING_CORP_RENT_OFFICE,
        SETTING_CLONINGBAY_TAX,
        SETTING_JUMP_BRIDGE_ACTIVATION,
        35_894,
        15,
    ] {
        assert_eq!(encode_service_setting_bps(flat_id, 0.0).unwrap(), 0);
        assert_eq!(
            encode_service_setting_bps(flat_id, 1_000_000_000_000.0).unwrap(),
            10_000_000_000_000_000
        );
        for bad in [1_000_000_000_001.0, -0.0001, f64::INFINITY] {
            assert_eq!(
                encode_service_setting_bps(flat_id, bad).unwrap_err().code,
                "InvalidStructureTaxRate"
            );
        }
    }

    for pct_id in [
        SETTING_REPROCESSING_TAX,
        SETTING_MARKET_TAX,
        SETTING_INDUSTRY_TAX,
        SETTING_MANUFACTURING_TAX,
        SETTING_RESEARCH_TAX,
        SETTING_INVENTION_TAX,
    ] {
        assert_eq!(encode_service_setting_bps(pct_id, 0.0).unwrap(), 0);
        assert_eq!(encode_service_setting_bps(pct_id, 0.12345).unwrap(), 1_235);
        assert_eq!(encode_service_setting_bps(pct_id, 1.0).unwrap(), 10_000);
        for bad in [1.0001, -0.0001, f64::NEG_INFINITY] {
            assert_eq!(
                encode_service_setting_bps(pct_id, bad).unwrap_err().code,
                "InvalidStructureTaxRate"
            );
        }
    }

    let mut ext = StructureExtendedState::default();
    ext.owner_groups.insert(
        201,
        AccessGroupRecord {
            group_id: 201,
            creator_id: 9001,
            owner_corp_id: 9801,
            name: "Corp 9801 Group".into(),
            description: String::new(),
            members: HashMap::new(),
        },
    );
    ext.owner_groups.insert(
        202,
        AccessGroupRecord {
            group_id: 202,
            creator_id: 9002,
            owner_corp_id: 0,
            name: "Personal 9002 Group".into(),
            description: String::new(),
            members: HashMap::new(),
        },
    );

    let normal_sess = SessionState::new(1);
    let mut gm_sess = SessionState::new(2);
    gm_sess.role = service_roles::ROLE_GMH;

    let missing_grp = HashMap::from([(SETTING_MARKET_TAX, vec![(999, 0.05)])]);
    assert_eq!(
        validate_service_acl_groups(&ext, &normal_sess, 9001, 9801, &missing_grp)
            .unwrap_err()
            .code,
        "AccessGroupNotFound"
    );

    let pers_upd = HashMap::from([(SETTING_MARKET_TAX, vec![(202, 0.04)])]);
    assert_eq!(
        validate_service_acl_groups(&ext, &normal_sess, 9001, 9801, &pers_upd)
            .unwrap_err()
            .code,
        "StructureOwnershipViolation"
    );
    assert!(validate_service_acl_groups(&ext, &normal_sess, 9002, 9801, &pers_upd).is_ok());
    assert!(validate_service_acl_groups(&ext, &gm_sess, 9001, 9801, &pers_upd).is_ok());

    let corp_upd = HashMap::from([(SETTING_MARKET_TAX, vec![(201, 0.05), (0, 0.02)])]);
    let encoded = validate_service_acl_groups(&ext, &normal_sess, 9001, 0, &corp_upd).unwrap();

    let mut prof = StructureAccessProfile::default();
    apply_service_acls_to_profile(
        &mut prof,
        HashMap::from([(SETTING_REPROCESSING_TAX, Vec::new())]),
    );
    assert!(!prof.service_settings.contains_key(&SETTING_REPROCESSING_TAX));
    assert_eq!(
        prof.service_acl_rules.get(&SETTING_REPROCESSING_TAX),
        Some(&Vec::new())
    );
    apply_service_acls_to_profile(&mut prof, encoded);
    assert_eq!(prof.service_settings.get(&SETTING_MARKET_TAX), Some(&200));
}

#[test]
fn effective_profile_delayed_transitions_and_domain_acl_evaluation_branches() {
    let ctx = test_ctx_with_clock(1_800_000_000);
    let (char_direct_ally, char_corp_ally, char_blocked_corp, char_blocked_ally, char_unmatched) =
        (920_001_i64, 920_002, 920_003, 920_004, 920_005);
    let (corp_ok, corp_blk, corp_ally_blocked) = (98_920_001_i64, 98_920_002, 98_920_003);
    let (ally_ok, ally_blk, st_id) = (99_920_001_i64, 99_920_002, 1_000_000_920_001_i64);

    ctx.db.with_mut_snapshot(|snap| {
        for (cid, aid) in [
            (corp_ok, Some(ally_ok)),
            (corp_blk, None),
            (corp_ally_blocked, Some(ally_blk)),
        ] {
            snap.corporations.insert(
                cid,
                CorporationRecord {
                    corporation_id: cid,
                    alliance_id: aid,
                    ..Default::default()
                },
            );
        }
        for (cid, name, corp, ally) in [
            (char_direct_ally, "DirectAlly", corp_blk, Some(ally_ok)),
            (char_corp_ally, "CorpAlly", corp_ok, None),
            (char_blocked_corp, "BlkCorp", corp_blk, None),
            (char_blocked_ally, "BlkAlly", corp_ally_blocked, None),
            (char_unmatched, "Unmatched", 98_999_999, None),
        ] {
            let mut ch = clone_char_with(snap, cid, name, corp, 30_000_142, cid);
            ch.alliance_id = ally;
            snap.characters.insert(cid, ch);
        }
    });

    let mut ext = StructureExtendedState::default();
    let mut base_prof = StructureAccessProfile {
        profile_id: 10,
        name: "Base".into(),
        ..Default::default()
    };
    base_prof.service_settings.insert(SETTING_MARKET_TAX, 900);
    let mut pending_prof = StructureAccessProfile {
        profile_id: 20,
        name: "Pending".into(),
        ..Default::default()
    };
    pending_prof.service_settings.insert(SETTING_MARKET_TAX, 150);

    ext.access_profiles.insert(st_id, base_prof);
    ext.profiles_by_id.insert(20, pending_prof);
    ext.pending_profile_applications
        .insert(st_id, (20, 1_800_000_500));

    assert_eq!(effective_profile_for_structure(&ext, st_id, None).unwrap().profile_id, 10);
    assert_eq!(
        effective_profile_for_structure(&ext, st_id, Some(1_800_000_499)).unwrap().profile_id,
        10
    );
    assert_eq!(
        effective_profile_for_structure(&ext, st_id, Some(1_800_000_500)).unwrap().profile_id,
        20
    );

    ext.owner_groups.insert(
        301,
        AccessGroupRecord {
            group_id: 301,
            creator_id: char_direct_ally,
            owner_corp_id: corp_ok,
            name: "G301".into(),
            description: String::new(),
            members: HashMap::from([(ally_ok, 0), (corp_blk, -1), (ally_blk, -1)]),
        },
    );
    ext.owner_groups.insert(
        302,
        AccessGroupRecord {
            group_id: 302,
            creator_id: char_direct_ally,
            owner_corp_id: corp_ok,
            name: "G302".into(),
            description: String::new(),
            members: HashMap::from([(0, 0)]),
        },
    );

    let mut test_prof = StructureAccessProfile::default();
    test_prof
        .service_acl_rules
        .insert(SETTING_MANUFACTURING_TAX, vec![(301, 120)]);
    test_prof
        .service_acl_rules
        .insert(SETTING_RESEARCH_TAX, vec![(302, 350)]);
    test_prof
        .service_acl_rules
        .insert(SETTING_INVENTION_TAX, Vec::new());

    ctx.db.with_snapshot(|snap| {
        for any_of in [false, true] {
            assert_eq!(
                evaluate_domain_acl_on_profile(
                    snap,
                    &ext,
                    &test_prof,
                    char_corp_ally,
                    &[SETTING_MARKET_TAX],
                    any_of
                ),
                None
            );
        }
        assert_eq!(
            evaluate_domain_acl_on_profile(
                snap,
                &ext,
                &test_prof,
                char_corp_ally,
                &[SETTING_INVENTION_TAX],
                false
            ),
            Some(Err(()))
        );
        assert_eq!(
            evaluate_domain_acl_on_profile(
                snap,
                &ext,
                &test_prof,
                char_corp_ally,
                &[SETTING_MANUFACTURING_TAX, SETTING_RESEARCH_TAX],
                true
            ),
            Some(Ok(120))
        );
        assert_eq!(
            evaluate_domain_acl_on_profile(
                snap,
                &ext,
                &test_prof,
                char_direct_ally,
                &[SETTING_MANUFACTURING_TAX],
                false
            ),
            Some(Err(()))
        );
        assert_eq!(
            evaluate_domain_acl_on_profile(
                snap,
                &ext,
                &test_prof,
                char_direct_ally,
                &[SETTING_MANUFACTURING_TAX, SETTING_RESEARCH_TAX],
                true
            ),
            Some(Ok(350))
        );
        assert_eq!(
            evaluate_domain_acl_on_profile(
                snap,
                &ext,
                &test_prof,
                char_blocked_ally,
                &[SETTING_MANUFACTURING_TAX],
                false
            ),
            Some(Err(()))
        );
    });
}

#[test]
fn v24_service_online_matrix_candidate_settings_market_and_industry_checks() {
    let ctx = test_ctx_with_clock(1_800_000_000);
    let (pilot_id, corp_id) = (921_001_i64, 98_921_001_i64);
    let (fort_id, jb_id, cyno_id, metenox_id) = (
        1_000_000_921_001_i64,
        1_000_000_921_002,
        1_000_000_921_003,
        1_000_000_921_004,
    );

    ctx.db.with_mut_snapshot(|snap| {
        let ch = clone_char_with(snap, pilot_id, "ServicePilot", corp_id, 30_000_142, 10);
        snap.characters.insert(pilot_id, ch);
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                ceo_id: pilot_id,
                ..Default::default()
            },
        );
        for (sid, tid) in [
            (fort_id, 35_833),
            (jb_id, 35_841),
            (cyno_id, 35_840),
            (metenox_id, 81_826),
        ] {
            snap.structures.insert(sid, make_upwell_struct(sid, tid, corp_id));
        }
        for (idx, mod_tid) in [
            35_892_u32, 35_899, 35_894, 45_009, 35_878, 35_891, 35_886, 45_537,
        ]
        .into_iter()
        .enumerate()
        {
            let mid = 921_500 + (idx as i64);
            snap.items.insert(
                mid,
                InventoryItemRecord {
                    item_id: mid,
                    type_id: mod_tid,
                    owner_id: corp_id,
                    location_id: fort_id,
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
        assert!(!is_v24_or_legacy_service_online(&ctx, snap, 999_999_999, SERVICE_MARKET));
        assert!(is_v24_or_legacy_service_online(&ctx, snap, jb_id, SERVICE_JUMP_BRIDGE));
        assert!(is_v24_or_legacy_service_online(&ctx, snap, cyno_id, SERVICE_CYNO_BEACON));
        assert!(is_v24_or_legacy_service_online(
            &ctx,
            snap,
            metenox_id,
            SERVICE_AUTOMOONMINING
        ));
        assert!(!is_v24_or_legacy_service_online(&ctx, snap, jb_id, SERVICE_REPAIR));

        for svc in [
            SERVICE_REPROCESSING,
            SERVICE_MARKET,
            SERVICE_MEDICAL,
            SERVICE_REPAIR,
            SERVICE_JUMP_CLONE,
            SERVICE_MOONMINING,
            SERVICE_INDUSTRY,
            SERVICE_MANUFACTURING,
            SERVICE_MANUFACTURING_BASIC,
            SERVICE_MANUFACTURING_CAPITAL,
            SERVICE_MANUFACTURING_SUPERCAPITAL,
            SERVICE_LABORATORY,
            SERVICE_LABORATORY_RESEARCH_TIME,
            SERVICE_LABORATORY_RESEARCH_MATERIAL,
            SERVICE_LABORATORY_COPYING,
            SERVICE_LABORATORY_INVENTION,
            SERVICE_REACTIONS,
            SERVICE_REACTIONS_COMPOSITE,
            SERVICE_REACTIONS_BIOCHEMICAL,
            SERVICE_REACTIONS_HYBRID,
        ] {
            assert!(is_v24_or_legacy_service_online(&ctx, snap, fort_id, svc));
            assert_eq!(
                evaluate_character_service_bps(&ctx, snap, fort_id, pilot_id, svc, 420),
                Some(420)
            );
        }
        assert!(!is_v24_or_legacy_service_online(&ctx, snap, fort_id, SERVICE_DOCKING));
        assert!(!is_v24_or_legacy_service_online(&ctx, snap, fort_id, SERVICE_FITTING));
        assert!(!is_v24_or_legacy_service_online(&ctx, snap, fort_id, SERVICE_OFFICES));
        assert!(!is_v24_or_legacy_service_online(&ctx, snap, fort_id, 999_999));
    });

    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        let mut prof = StructureAccessProfile {
            profile_id: 55,
            ..Default::default()
        };
        prof.service_settings.insert(SETTING_INDUSTRY_TAX, 325);
        prof.general_settings.insert(SERVICE_REPAIR, false);
        prof.service_acl_rules
            .insert(SETTING_MARKET_TAX, vec![(0, 150)]);
        prof.service_acl_rules.insert(24, vec![(0, 250)]);
        ext.access_profiles.insert(fort_id, prof.clone());
        ext.profiles_by_id.insert(55, prof);
        ext.pending_profile_applications
            .insert(fort_id, (55, 1_800_000_000));
        save_ext_state(snap, &ext);

        assert_eq!(
            evaluate_character_service_bps(&ctx, snap, fort_id, pilot_id, SERVICE_REPAIR, 100),
            None
        );
        for act in [1_u8, 3, 4, 5, 8, 9, 11, 99] {
            let tax =
                check_industry_activity_acl_and_tax(snap, fort_id, pilot_id, act, 1_800_000_000)
                    .unwrap()
                    .unwrap();
            assert!((tax - 0.025).abs() < 1e-6);
        }
        assert!(evaluate_character_setting(
            &ctx,
            snap,
            fort_id,
            pilot_id,
            SETTING_DEFENSE_CAN_CONTROL_STRUCTURE
        ));
        assert!(!evaluate_character_setting(
            &ctx,
            snap,
            999_999_999,
            pilot_id,
            SETTING_HOUSING_CAN_DOCK
        ));
        assert!(check_upwell_market_access_and_rate(
            &ctx,
            snap,
            60_003_760,
            pilot_id,
            1_800_000_000,
            "MarketOffline"
        )
        .unwrap()
        .is_none());

        let (owner, rate, bps) = apply_due_profiles_and_check_upwell_market(
            &ctx,
            snap,
            fort_id,
            pilot_id,
            1_800_000_000,
            "MarketOffline",
        )
        .unwrap()
        .unwrap();
        assert_eq!(owner, corp_id);
        assert_eq!(bps, Some(150));
        assert!((rate - 0.02).abs() < 1e-6);
    });
}

#[test]
fn owner_group_batch_roles_linked_profile_sync_and_structure_fallback() {
    let ctx = test_ctx_with_clock(1_800_000_000);
    let (creator_id, mgr_id, outsider_id, target_char) = (922_001_i64, 922_002, 922_003, 922_004);
    let (corp_id, ally_id, st_id) = (98_922_001_i64, 99_922_001, 1_000_000_922_001_i64);

    ctx.db.with_mut_snapshot(|snap| {
        for (cid, name) in [
            (creator_id, "Creator"),
            (mgr_id, "Manager"),
            (outsider_id, "Outsider"),
            (target_char, "TargetChar"),
        ] {
            let ch = clone_char_with(snap, cid, name, corp_id, 30_000_142, 10);
            snap.characters.insert(cid, ch);
        }
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                ceo_id: creator_id,
                alliance_id: Some(ally_id),
                ..Default::default()
            },
        );
        snap.alliances.insert(
            ally_id,
            AllianceRecord {
                alliance_id: ally_id,
                executor_corp_id: corp_id,
                member_corp_ids: HashSet::from([corp_id]),
                ..Default::default()
            },
        );
        snap.structures
            .insert(st_id, make_upwell_struct(st_id, 35_832, corp_id));
    });

    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            validate_group_member_entity(snap, 0, false).unwrap(),
            (false, false, false)
        );
        assert!(validate_group_member_entity(snap, 0, true).is_err());
        assert_eq!(
            validate_group_member_entity(snap, ally_id, false).unwrap(),
            (true, false, false)
        );
        assert_eq!(
            validate_group_member_entity(snap, corp_id, false).unwrap(),
            (false, true, false)
        );
    });

    let mut creator_sess = SessionState::new(1);
    creator_sess.user_id = Some(creator_id as u64);
    creator_sess.character_id = Some(creator_id);
    creator_sess.corporation_id = Some(corp_id);
    creator_sess.corp_role = corp_roles::ROLE_DIRECTOR;

    let PyRep::KeyVal(g1_kv) = handle_create_owner_group(
        &ctx,
        &creator_sess,
        &[
            PyRep::String("Personal Group".into()),
            PyRep::String("Desc".into()),
            PyRep::Bool(false),
        ],
    )
    .unwrap() else {
        panic!("expected keyval");
    };
    let gid1 = g1_kv.get("groupID").and_then(|v| v.as_i64()).unwrap();

    assert_eq!(
        apply_group_member_batch(&ctx, &creator_sess, &[], true)
            .unwrap_err()
            .code,
        "MissingGroupId"
    );
    assert_eq!(
        apply_group_member_batch(&ctx, &creator_sess, &[PyRep::Integer(gid1)], true)
            .unwrap_err()
            .code,
        "MissingEntityId"
    );

    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        ext.owner_groups
            .get_mut(&gid1)
            .unwrap()
            .members
            .insert(mgr_id, 2);
        let prof = StructureAccessProfile {
            profile_id: 77,
            access_group_ids: HashSet::from([gid1]),
            ..Default::default()
        };
        ext.profiles_by_id.insert(77, prof.clone());
        ext.access_profiles.insert(st_id, prof);
        save_ext_state(snap, &ext);
    });

    let mut outsider_sess = SessionState::new(2);
    outsider_sess.user_id = Some(outsider_id as u64);
    outsider_sess.character_id = Some(outsider_id);
    assert_eq!(
        apply_group_member_batch(
            &ctx,
            &outsider_sess,
            &[PyRep::Integer(gid1), PyRep::Integer(target_char)],
            true,
        )
        .unwrap_err()
        .code,
        "InsufficientCorpRoles"
    );

    let mut mgr_sess = SessionState::new(3);
    mgr_sess.user_id = Some(mgr_id as u64);
    mgr_sess.character_id = Some(mgr_id);
    assert_eq!(
        apply_group_member_batch(
            &ctx,
            &mgr_sess,
            &[PyRep::Integer(gid1), PyRep::Integer(creator_id)],
            true,
        )
        .unwrap_err()
        .code,
        "InsufficientCorpRoles"
    );
    apply_group_member_batch(
        &ctx,
        &mgr_sess,
        &[
            PyRep::Integer(gid1),
            PyRep::Dict(vec![
                (PyRep::Integer(target_char), PyRep::Integer(0)),
                (PyRep::Integer(outsider_id), PyRep::Integer(-1)),
            ]),
        ],
        true,
    )
    .expect("manager adds dict members");

    apply_group_member_batch(
        &ctx,
        &creator_sess,
        &[PyRep::Integer(st_id), PyRep::Integer(target_char)],
        false,
    )
    .expect("structure_id fallback remove");
}
