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
use std::collections::{HashMap, HashSet};

#[cfg(test)]
#[path = "structure_profile_online_and_groups_unit_tests.rs"]
mod structure_profile_online_and_groups_unit_tests;

pub(super) fn make_upwell_struct(sid: i64, tid: u32, corp_id: i64) -> UpwellStructureRecord {
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
