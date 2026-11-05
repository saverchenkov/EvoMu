//! Unit tests for `structure_profile_services` (BPS encoding, ACL group validation, domain tax resolution, owner group batch ops).

#![forbid(unsafe_code)]

use super::*;
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::{
    clone_char_with, test_ctx_with_clock,
};
use eve_db::{corp_roles, CorporationRecord, UpwellStructureRecord};

#[test]
fn service_setting_bps_encoding_and_acl_group_validation() {
    // Percentage tax rate: 0.0..=1.0 -> 0..=10_000 bps
    assert_eq!(encode_service_setting_bps(SETTING_MARKET_TAX, 0.025).unwrap(), 250);
    assert_eq!(encode_service_setting_bps(SETTING_MARKET_TAX, 1.0).unwrap(), 10_000);
    assert_eq!(
        encode_service_setting_bps(SETTING_MARKET_TAX, -0.01).unwrap_err().code,
        "InvalidStructureTaxRate"
    );
    assert_eq!(
        encode_service_setting_bps(SETTING_MARKET_TAX, 1.01).unwrap_err().code,
        "InvalidStructureTaxRate"
    );
    assert_eq!(
        encode_service_setting_bps(SETTING_MARKET_TAX, f64::NAN).unwrap_err().code,
        "InvalidStructureTaxRate"
    );

    // Flat ISK settings allow > 1.0
    assert_eq!(
        encode_service_setting_bps(SETTING_CLONINGBAY_TAX, 500_000.0).unwrap(),
        5_000_000_000
    );

    let mut ext = StructureExtendedState::default();
    ext.owner_groups.insert(
        101,
        ops::AccessGroupRecord {
            group_id: 101,
            creator_id: 90_000_001,
            owner_corp_id: 98_000_001,
            name: "Corp ACL".into(),
            description: String::new(),
            members: HashMap::new(),
        },
    );
    ext.owner_groups.insert(
        102,
        ops::AccessGroupRecord {
            group_id: 102,
            creator_id: 90_000_099,
            owner_corp_id: 0,
            name: "Foreign Personal ACL".into(),
            description: String::new(),
            members: HashMap::new(),
        },
    );

    let sess = SessionState::new(1);

    // Negative group ID -> AccessGroupNotFound
    let mut bad_neg = HashMap::new();
    bad_neg.insert(SETTING_MARKET_TAX, vec![(-1, 0.05)]);
    assert_eq!(
        validate_service_acl_groups(&ext, &sess, 90_000_001, 98_000_001, &bad_neg)
            .unwrap_err()
            .code,
        "AccessGroupNotFound"
    );

    // Foreign corp ownership -> StructureOwnershipViolation
    let mut valid_corp = HashMap::new();
    valid_corp.insert(SETTING_MARKET_TAX, vec![(101, 0.03), (0, 0.08)]);
    assert_eq!(
        validate_service_acl_groups(&ext, &sess, 90_000_001, 98_000_999, &valid_corp)
            .unwrap_err()
            .code,
        "StructureOwnershipViolation"
    );

    // Foreign personal ownership -> StructureOwnershipViolation
    let mut foreign_pers = HashMap::new();
    foreign_pers.insert(SETTING_MARKET_TAX, vec![(102, 0.03)]);
    assert_eq!(
        validate_service_acl_groups(&ext, &sess, 90_000_001, 98_000_001, &foreign_pers)
            .unwrap_err()
            .code,
        "StructureOwnershipViolation"
    );

    // Valid ACL update encodes bps and syncs minimum into profile.service_settings
    let encoded =
        validate_service_acl_groups(&ext, &sess, 90_000_001, 98_000_001, &valid_corp).unwrap();
    let mut prof = StructureAccessProfile::default();
    apply_service_acls_to_profile(&mut prof, encoded);
    assert_eq!(prof.service_settings.get(&SETTING_MARKET_TAX), Some(&300));
    assert_eq!(
        prof.service_acl_rules.get(&SETTING_MARKET_TAX),
        Some(&vec![(101, 300), (0, 800)])
    );
}

#[test]
fn domain_acl_evaluation_industry_tax_jump_bridge_and_owner_group_batch() {
    let ctx = test_ctx_with_clock(1_800_000_000);
    let admin_id = 910_801_i64;
    let member_id = 910_802_i64;
    let public_id = 910_803_i64;
    let blocked_id = 910_804_i64;
    let corp_id = 98_800_801_i64;
    let st_id = 1_000_000_980_001_i64;
    let jb_id = 1_000_000_980_002_i64;

    ctx.db.with_mut_snapshot(|snap| {
        for (cid, name) in [
            (admin_id, "ACL Admin"),
            (member_id, "ACL Member"),
            (public_id, "Public Pilot"),
            (blocked_id, "Blocked Pilot"),
        ] {
            let ch = clone_char_with(snap, cid, name, corp_id, 30_000_144, 100_000_001);
            snap.characters.insert(cid, ch);
        }
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                ceo_id: admin_id,
                ..Default::default()
            },
        );
        for (sid, tid) in [(st_id, 35_832), (jb_id, 35_841)] {
            snap.structures.insert(
                sid,
                UpwellStructureRecord {
                    structure_id: sid,
                    type_id: tid,
                    owner_corp_id: corp_id,
                    solar_system_id: 30_000_144,
                    name: format!("Struct {sid}"),
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
                },
            );
        }
    });

    let mut admin_sess = SessionState::new(1);
    admin_sess.user_id = Some(admin_id as u64);
    admin_sess.character_id = Some(admin_id);
    admin_sess.corporation_id = Some(corp_id);
    admin_sess.corp_role = corp_roles::ROLE_DIRECTOR;

    // Create owner group via handle_create_owner_group
    let PyRep::KeyVal(grp_kv) = handle_create_owner_group(
        &ctx,
        &admin_sess,
        &[
            PyRep::String("VIP Industry".into()),
            PyRep::String("Discounted".into()),
            PyRep::Bool(true),
        ],
    )
    .expect("create owner group") else {
        panic!("expected keyval");
    };
    let gid = grp_kv.get("groupID").and_then(|v| v.as_i64()).unwrap();

    // Add member_id (allowed) and blocked_id (blocked = true) to group
    apply_group_member_batch(
        &ctx,
        &admin_sess,
        &[
            PyRep::Integer(gid),
            PyRep::List(vec![PyRep::Integer(member_id)]),
            PyRep::Bool(false),
        ],
        true,
    )
    .expect("add member");
    apply_group_member_batch(
        &ctx,
        &admin_sess,
        &[
            PyRep::Integer(gid),
            PyRep::List(vec![PyRep::Integer(blocked_id)]),
            PyRep::Bool(true),
        ],
        true,
    )
    .expect("block member");

    // Cannot remove or block creator
    assert_eq!(
        apply_group_member_batch(
            &ctx,
            &admin_sess,
            &[
                PyRep::Integer(gid),
                PyRep::List(vec![PyRep::Integer(admin_id)]),
                PyRep::Bool(true),
            ],
            true,
        )
        .unwrap_err()
        .code,
        "InsufficientCorpRoles"
    );

    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        let mut prof = StructureAccessProfile::default();
        // VIP group (gid) pays 1% (100 bps), public (0) pays 5% (500 bps); blocked_id is -1 in gid
        prof.service_acl_rules
            .insert(SETTING_MANUFACTURING_TAX, vec![(gid, 100), (0, 500)]);
        prof.service_acl_rules
            .insert(SETTING_JUMP_BRIDGE_ACTIVATION, vec![(gid, 0)]);
        ext.access_profiles.insert(st_id, prof.clone());
        ext.access_profiles.insert(jb_id, prof);
        save_ext_state(snap, &ext);

        // Industry activity 1 (Manufacturing): member gets 1% (0.01), public gets 5% (0.05), blocked is denied
        let member_rate =
            check_industry_activity_acl_and_tax(snap, st_id, member_id, 1, 1_800_000_000)
                .unwrap()
                .unwrap();
        let pub_rate =
            check_industry_activity_acl_and_tax(snap, st_id, public_id, 1, 1_800_000_000)
                .unwrap()
                .unwrap();
        assert!((member_rate - 0.01).abs() < 1e-6);
        assert!((pub_rate - 0.05).abs() < 1e-6);
        assert_eq!(
            check_industry_activity_acl_and_tax(snap, st_id, blocked_id, 1, 1_800_000_000)
                .unwrap_err()
                .code,
            "StructureServiceAccessDenied"
        );

        // Jump bridge ACL: member allowed, public (not in gid) denied, blocked denied
        assert!(can_character_use_jump_bridge(snap, jb_id, member_id, 1_800_000_000));
        assert!(!can_character_use_jump_bridge(snap, jb_id, public_id, 1_800_000_000));
        assert!(!can_character_use_jump_bridge(snap, jb_id, blocked_id, 1_800_000_000));

        // evaluate_character_setting and evaluate_character_service_bps on Ansiblex jump bridge
        assert!(evaluate_character_setting(
            &ctx,
            snap,
            jb_id,
            member_id,
            SETTING_JUMP_BRIDGE_ACTIVATION
        ));
        assert!(!evaluate_character_setting(
            &ctx,
            snap,
            jb_id,
            public_id,
            SETTING_JUMP_BRIDGE_ACTIVATION
        ));
        assert_eq!(
            evaluate_character_service_bps(&ctx, snap, jb_id, member_id, SERVICE_JUMP_BRIDGE, 999),
            Some(0)
        );
        assert_eq!(
            evaluate_character_service_bps(&ctx, snap, jb_id, public_id, SERVICE_JUMP_BRIDGE, 999),
            None
        );
    });
}
