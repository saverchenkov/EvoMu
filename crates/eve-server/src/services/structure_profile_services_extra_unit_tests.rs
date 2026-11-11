#![allow(unused_imports)]
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
                { let mut __corp = CorporationRecord::default();
                    __corp.corporation_id = cid;
                    __corp.alliance_id = aid;
                    __corp },
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
fn w4_mutant_killers_market_access_rate_and_service_acl_branches() {
    let ctx = test_ctx_with_clock(1_800_000_000);
    let now_u = 1_800_000_000u64;
    let corp_id = 98_930_001i64;
    let foreign_corp = 98_930_002i64;
    let blocked_ally = 99_930_003i64;
    let ceo_id = 930_001i64;
    let director_id = 930_002i64;
    let station_mgr_id = 930_003i64;
    let grunt_id = 930_004i64;
    let foreign_pilot = 930_005i64;
    let blocked_char = 930_006i64;
    let blocked_corp_pilot = 930_007i64;
    let blocked_ally_pilot = 930_008i64;

    let fort_id = 1_000_000_930_001i64;
    let jb_id = 1_000_000_930_002i64;
    let destroyed_id = 1_000_000_930_003i64;
    let abandoned_id = 1_000_000_930_004i64;
    let unpowered_id = 1_000_000_930_005i64;
    let nocore_id = 1_000_000_930_006i64;
    let anchoring_id = 1_000_000_930_007i64;

    let mut market_mod_id = 0i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.insert(
            ceo_id,
            clone_char_with(snap, ceo_id, "CeoPilot", corp_id, 30_000_142, 10),
        );
        snap.characters.insert(
            director_id,
            clone_char_with(snap, director_id, "DirPilot", corp_id, 30_000_142, 10),
        );
        snap.characters.insert(
            station_mgr_id,
            clone_char_with(snap, station_mgr_id, "MgrPilot", corp_id, 30_000_142, 10),
        );
        snap.characters.insert(
            grunt_id,
            clone_char_with(snap, grunt_id, "GruntPilot", corp_id, 30_000_142, 10),
        );
        snap.characters.insert(
            foreign_pilot,
            clone_char_with(snap, foreign_pilot, "ForeignPilot", foreign_corp, 30_000_142, 10),
        );
        snap.characters.insert(
            blocked_char,
            clone_char_with(snap, blocked_char, "BlockedChar", foreign_corp, 30_000_142, 10),
        );
        snap.characters.insert(
            blocked_corp_pilot,
            clone_char_with(
                snap,
                blocked_corp_pilot,
                "BlockedCorpPilot",
                98_930_009,
                30_000_142,
                10,
            ),
        );
        snap.characters.insert(
            blocked_ally_pilot,
            clone_char_with(
                snap,
                blocked_ally_pilot,
                "BlockedAllyPilot",
                98_930_010,
                30_000_142,
                10,
            ),
        );

        let mut corp = CorporationRecord::default();
        corp.corporation_id = corp_id;
        corp.ceo_id = ceo_id;
        corp.member_roles.insert(director_id, corp_roles::ROLE_DIRECTOR);
        corp.member_roles
            .insert(station_mgr_id, corp_roles::ROLE_STATION_MANAGER);
        corp.member_roles.insert(grunt_id, 0);
        snap.corporations.insert(corp_id, corp);

        let mut fc = CorporationRecord::default();
        fc.corporation_id = foreign_corp;
        fc.ceo_id = foreign_pilot;
        snap.corporations.insert(foreign_corp, fc);

        let mut bc = CorporationRecord::default();
        bc.corporation_id = 98_930_009;
        bc.ceo_id = blocked_corp_pilot;
        snap.corporations.insert(98_930_009, bc);

        let mut ac = CorporationRecord::default();
        ac.corporation_id = 98_930_010;
        ac.ceo_id = blocked_ally_pilot;
        ac.alliance_id = Some(blocked_ally);
        snap.corporations.insert(98_930_010, ac);

        let mut ally = AllianceRecord::default();
        ally.alliance_id = blocked_ally;
        ally.member_corp_ids = HashSet::from([98_930_010]);
        snap.alliances.insert(blocked_ally, ally);

        snap.structures
            .insert(fort_id, make_upwell_struct(fort_id, 35_833, corp_id));
        snap.structures
            .insert(jb_id, make_upwell_struct(jb_id, 35_841, corp_id));

        let mut destroyed = make_upwell_struct(destroyed_id, 35_833, corp_id);
        destroyed.state = "Destroyed".into();
        snap.structures.insert(destroyed_id, destroyed);

        let mut abandoned = make_upwell_struct(abandoned_id, 35_833, corp_id);
        abandoned.fuel_blocks = 0;
        snap.structures.insert(abandoned_id, abandoned);

        let mut unpowered = make_upwell_struct(unpowered_id, 35_833, corp_id);
        unpowered.fuel_blocks = 0;
        snap.structures.insert(unpowered_id, unpowered);

        let mut nocore = make_upwell_struct(nocore_id, 35_833, corp_id);
        nocore.quantum_core_installed = false;
        snap.structures.insert(nocore_id, nocore);

        let mut anchoring = make_upwell_struct(anchoring_id, 35_833, corp_id);
        anchoring.state = "Anchoring".into();
        snap.structures.insert(anchoring_id, anchoring);

        // Fit online Market (35892), Reprocessing (35894), Cloning (35899), Industry (35878) on `fort_id`
        for (idx, mod_tid) in [35_892_u32, 35_894, 35_899, 35_878].into_iter().enumerate() {
            let mid = 930_500 + (idx as i64);
            if mod_tid == 35_892 {
                market_mod_id = mid;
            }
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

    // 1. `check_upwell_market_access_and_rate` branches
    ctx.db.with_snapshot(|snap| {
        // Non-structure ID returns Ok(None)
        assert_eq!(
            check_upwell_market_access_and_rate(
                &ctx,
                snap,
                60_003_760,
                foreign_pilot,
                now_u,
                "CustomOfflineCode",
            )
            .unwrap(),
            None
        );

        // Online market on corp structure with no custom profile allows corp member and denies foreign pilot
        let (owner, rate_default, bps_default) = check_upwell_market_access_and_rate(
            &ctx,
            snap,
            fort_id,
            grunt_id,
            now_u,
            "CustomOfflineCode",
        )
        .unwrap()
        .expect("online market default for owning corp member");
        assert_eq!(owner, corp_id);
        assert!(bps_default.is_none());
        assert!(rate_default >= 0.0);
        assert_eq!(
            check_upwell_market_access_and_rate(
                &ctx,
                snap,
                fort_id,
                foreign_pilot,
                now_u,
                "CustomOfflineCode",
            )
            .unwrap_err()
            .code,
            "StructureServiceAccessDenied"
        );
    });

    // Custom ACL tax (150 bps = 1.5%) vs blocked char while market IS online vs offline market rewrite
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        let mut prof = StructureAccessProfile {
            profile_id: 71,
            owner_corp_id: corp_id,
            ..Default::default()
        };
        prof.service_acl_rules
            .insert(SETTING_MARKET_TAX, vec![(0, 150)]);
        prof.blocked_characters.insert(blocked_char);
        prof.blocked_corporations.insert(98_930_009);
        prof.blocked_alliances.insert(blocked_ally);
        prof.service_acl_rules
            .insert(SETTING_JUMP_BRIDGE_ACTIVATION, vec![(0, 500_00)]);
        ext.access_profiles.insert(fort_id, prof.clone());
        ext.access_profiles.insert(jb_id, prof.clone());
        ext.access_profiles.insert(unpowered_id, prof.clone());

        let mut prof_due = prof.clone();
        prof_due.profile_id = 72;
        prof_due
            .service_acl_rules
            .insert(SETTING_MARKET_TAX, vec![(0, 220)]);
        ext.profiles_by_id.insert(71, prof);
        ext.profiles_by_id.insert(72, prof_due);
        ext.lifecycle
            .entry(abandoned_id)
            .or_default()
            .low_power_since_unix = Some(1_700_000_000);
        save_ext_state(snap, &ext);
    });

    ctx.db.with_snapshot(|snap| {
        // Online + custom rule: Ok(Some((corp_id, 0.020, Some(150)))) with 0.5% SCC surcharge
        let (owner, rate, bps) = check_upwell_market_access_and_rate(
            &ctx,
            snap,
            fort_id,
            foreign_pilot,
            now_u,
            "CustomOfflineCode",
        )
        .unwrap()
        .unwrap();
        assert_eq!(owner, corp_id);
        assert_eq!(bps, Some(150));
        assert!((rate - 0.020).abs() < 1e-6);

        // Online + blocked char: non-offline error code MUST NOT be rewritten to CustomOfflineCode!
        let err_blocked = check_upwell_market_access_and_rate(
            &ctx,
            snap,
            fort_id,
            blocked_char,
            now_u,
            "CustomOfflineCode",
        )
        .unwrap_err();
        assert_ne!(err_blocked.code, "CustomOfflineCode");

        // char_id == 0 skips char ACL unless profile disables Market general setting
        assert!(check_upwell_market_access_and_rate(
            &ctx,
            snap,
            fort_id,
            0,
            now_u,
            "CustomOfflineCode",
        )
        .is_ok());

        // CorpManagement ACL (`require_structure_service_access_in_snap`)
        assert!(require_structure_service_access_in_snap(
            Some(&ctx),
            snap,
            fort_id,
            ceo_id,
            StructureServiceKind::CorpManagement,
            now_u,
        )
        .is_ok());
        assert!(require_structure_service_access_in_snap(
            Some(&ctx),
            snap,
            fort_id,
            director_id,
            StructureServiceKind::CorpManagement,
            now_u,
        )
        .is_ok());
        assert!(require_structure_service_access_in_snap(
            Some(&ctx),
            snap,
            fort_id,
            station_mgr_id,
            StructureServiceKind::CorpManagement,
            now_u,
        )
        .is_ok());
        assert!(require_structure_service_access_in_snap(
            Some(&ctx),
            snap,
            fort_id,
            grunt_id,
            StructureServiceKind::CorpManagement,
            now_u,
        )
        .is_err());
        assert!(require_structure_service_access_in_snap(
            Some(&ctx),
            snap,
            fort_id,
            foreign_pilot,
            StructureServiceKind::CorpManagement,
            now_u,
        )
        .is_err());

        // JumpBridge: allowed pilot OK, blocked char/corp/ally Err; now_unix == 0 vs > 0
        assert!(require_structure_service_access_in_snap(
            Some(&ctx),
            snap,
            jb_id,
            foreign_pilot,
            StructureServiceKind::JumpBridge,
            now_u,
        )
        .is_ok());
        assert!(require_structure_service_access_in_snap(
            Some(&ctx),
            snap,
            jb_id,
            foreign_pilot,
            StructureServiceKind::JumpBridge,
            0,
        )
        .is_ok());
        for denied in [blocked_char, blocked_corp_pilot, blocked_ally_pilot] {
            assert!(
                require_structure_service_access_in_snap(
                    Some(&ctx),
                    snap,
                    jb_id,
                    denied,
                    StructureServiceKind::JumpBridge,
                    now_u,
                )
                .is_err(),
                "blocked pilot {denied} must be denied JumpBridge"
            );
        }

        // Repair: FullPower OK (Some(&ctx) & None), Abandoned / Unpowered / NoCore / Anchoring error
        let rep_ok = require_structure_service_access_in_snap(
            Some(&ctx),
            snap,
            fort_id,
            foreign_pilot,
            StructureServiceKind::Repair,
            now_u,
        )
        .unwrap();
        assert_eq!(rep_ok, Some(0));
        assert!(require_structure_service_access_in_snap(
            None,
            snap,
            fort_id,
            foreign_pilot,
            StructureServiceKind::Repair,
            now_u,
        )
        .is_ok());
        // LowPower (with quantum core) still permits Repair, while Abandoned / NoCore / Anchoring reject!
        assert!(require_structure_service_access_in_snap(
            Some(&ctx),
            snap,
            unpowered_id,
            foreign_pilot,
            StructureServiceKind::Repair,
            now_u,
        )
        .is_ok());
        for bad_sid in [abandoned_id, nocore_id, anchoring_id] {
            assert!(
                require_structure_service_access_in_snap(
                    Some(&ctx),
                    snap,
                    bad_sid,
                    foreign_pilot,
                    StructureServiceKind::Repair,
                    now_u,
                )
                .is_err(),
                "structure {bad_sid} must reject Repair"
            );
        }

        // Destroyed structure rejects EVERY StructureServiceKind arm cleanly
        for kind in [
            StructureServiceKind::Docking,
            StructureServiceKind::Market,
            StructureServiceKind::Reprocessing,
            StructureServiceKind::Repair,
            StructureServiceKind::CloneBay,
            StructureServiceKind::Industry { activity_id: 1 },
            StructureServiceKind::JumpBridge,
            StructureServiceKind::StructureControl,
            StructureServiceKind::CorpManagement,
        ] {
            assert!(
                require_structure_service_access_in_snap(
                    Some(&ctx),
                    snap,
                    destroyed_id,
                    ceo_id,
                    kind,
                    now_u,
                )
                .is_err(),
                "Destroyed must reject {kind:?}"
            );
        }

        // Industry activity IDs 1, 3..=5, 8, 9, fallback 99
        for aid in [1u8, 3, 4, 5, 8, 9, 11, 99] {
            assert!(require_structure_service_access_in_snap(
                Some(&ctx),
                snap,
                fort_id,
                foreign_pilot,
                StructureServiceKind::Industry { activity_id: aid },
                now_u,
            )
            .is_ok());
        }
    });

    // Offline market module -> `StructureMarketOffline` rewritten to `"CustomOfflineCode"`
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(market_mod_id, false);
        let offline_err = check_upwell_market_access_and_rate(
            &ctx,
            snap,
            fort_id,
            foreign_pilot,
            now_u,
            "CustomOfflineCode",
        )
        .unwrap_err();
        assert_eq!(offline_err.code, "CustomOfflineCode");

        // Re-online market, set due pending profile 72 -> `apply_due_profiles_and_check_upwell_market` applies it!
        snap.module_online_states.insert(market_mod_id, true);
        let mut ext = load_ext_state(snap);
        ext.pending_profile_applications.insert(fort_id, (72, now_u));
        save_ext_state(snap, &ext);
        let (_, new_rate, new_bps) = apply_due_profiles_and_check_upwell_market(
            &ctx,
            snap,
            fort_id,
            foreign_pilot,
            now_u,
            "CustomOfflineCode",
        )
        .unwrap()
        .unwrap();
        assert_eq!(new_bps, Some(220));
        assert!((new_rate - 0.027).abs() < 1e-6);

        // char_id == 0 with profile general_settings SETTING_MARKET_TAX = false -> fails even for char_id == 0
        let mut ext = load_ext_state(snap);
        ext.access_profiles
            .get_mut(&fort_id)
            .unwrap()
            .general_settings
            .insert(SETTING_MARKET_TAX, false);
        save_ext_state(snap, &ext);
        assert!(check_upwell_market_access_and_rate(
            &ctx,
            snap,
            fort_id,
            0,
            now_u,
            "CustomOfflineCode",
        )
        .is_err());
    });
}

