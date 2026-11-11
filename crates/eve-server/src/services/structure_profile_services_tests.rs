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
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = corp_id;
                __corp.ceo_id = admin_id;
                __corp },
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

#[test]
fn require_structure_service_access_enforces_lifecycle_acl_and_corp_management() {
    let ctx = test_ctx_with_clock(1_800_000_000);
    let corp_id = 98_444_001;
    let ceo_id = 94_001;
    let pending_dir_id = 94_002;
    let public_id = 94_003;
    let blocked_id = 94_004;
    let st_id = 1_000_000_044_001;
    let destroyed_id = 1_000_000_044_002;
    let now = ctx.now_unix();

    ctx.db.with_mut_snapshot(|snap| {
        let mut corp = CorporationRecord::new(corp_id, "ACL Guard Corp", "AGDC", ceo_id);
        corp.members = std::collections::HashSet::from([ceo_id, pending_dir_id]);
        corp.member_roles
            .insert(pending_dir_id, corp_roles::ROLE_DIRECTOR);
        snap.corporations.insert(corp_id, corp);
        // Pending 24h role stasis masks ROLE_DIRECTOR until now + 86_400
        snap.corp_governance
            .entry(corp_id)
            .or_default()
            .pending_role_grants
            .insert(pending_dir_id, (corp_roles::ROLE_DIRECTOR, now + 86_400));

        let c_ceo = clone_char_with(snap, ceo_id, "CEO", corp_id, 30_000_142, 0);
        let c_pdir = clone_char_with(snap, pending_dir_id, "PendingDir", corp_id, 30_000_142, 0);
        let c_pub = clone_char_with(snap, public_id, "Public", 1_000_009, 30_000_142, 0);
        let c_blk = clone_char_with(snap, blocked_id, "Blocked", 1_000_009, 30_000_142, 0);
        snap.characters.insert(ceo_id, c_ceo);
        snap.characters.insert(pending_dir_id, c_pdir);
        snap.characters.insert(public_id, c_pub);
        snap.characters.insert(blocked_id, c_blk);

        snap.structures.insert(
            st_id,
            UpwellStructureRecord {
                structure_id: st_id,
                type_id: 35_834, // Keepstar (market-capable)
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142,
                name: "Guard Citadel".into(),
                position: [0.0; 3],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 0, // Reprocessing requires fuel_blocks > 0
                shield_hp: 1.0,
                armor_hp: 1.0,
                hull_hp: 1.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        snap.structures.insert(
            destroyed_id,
            UpwellStructureRecord {
                structure_id: destroyed_id,
                type_id: 35_834,
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142,
                name: "Wrecked Citadel".into(),
                position: [0.0; 3],
                state: "Destroyed".into(),
                quantum_core_installed: false,
                fuel_blocks: 0,
                shield_hp: 0.0,
                armor_hp: 0.0,
                hull_hp: 0.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );

        // Online Standup Market Hub I (35892) at slot 164 without allowing public docking yet? No, allow public docking + block blocked_id + grant CloneBay at 200 ISK
        snap.items.insert(
            800_001,
            eve_db::InventoryItemRecord {
                item_id: 800_001,
                type_id: 35_892,
                owner_id: corp_id,
                location_id: st_id,
                flag: eve_db::InventoryFlag::ServiceSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(800_001, true);

        let mut ext = ops::load_ext_state(snap);
        let mut prof = ops::StructureAccessProfile::default();
        prof.allowed_characters.insert(public_id);
        prof.blocked_characters.insert(blocked_id);
        prof.service_settings.insert(13, 150); // Repair tax bps
        ext.access_profiles.insert(st_id, prof);
        ops::save_ext_state(snap, &ext);
    });

    ctx.db.with_snapshot(|snap| {
        // 1. Destroyed structure rejects both CorpManagement and Repair/Docking
        assert_eq!(
            require_structure_service_access_in_snap(
                Some(&ctx),
                snap,
                destroyed_id,
                ceo_id,
                StructureServiceKind::CorpManagement,
                now
            )
            .unwrap_err()
            .code,
            "StructureDestroyed"
        );
        assert_eq!(
            require_structure_service_access_in_snap(
                Some(&ctx),
                snap,
                destroyed_id,
                ceo_id,
                StructureServiceKind::Repair,
                now
            )
            .unwrap_err()
            .code,
            "StructureNotOperational"
        );

        // 2. Missing fuel fails Reprocessing check while Repair allows ACL member + returns 150 bps
        assert_eq!(
            require_structure_service_access_in_snap(
                Some(&ctx),
                snap,
                st_id,
                public_id,
                StructureServiceKind::Reprocessing,
                now
            )
            .unwrap_err()
            .code,
            "ReprocessingServiceOffline"
        );
        assert_eq!(
            require_structure_service_access_in_snap(
                Some(&ctx),
                snap,
                st_id,
                public_id,
                StructureServiceKind::Repair,
                now
            )
            .unwrap(),
            Some(150)
        );

        // 3. Blocked pilot is rejected for CloneBay and Docking; allowed pilot passes Docking
        assert_eq!(
            require_structure_service_access_in_snap(
                Some(&ctx),
                snap,
                st_id,
                blocked_id,
                StructureServiceKind::CloneBay,
                now
            )
            .unwrap_err()
            .code,
            "StructureAccessDenied"
        );
        assert!(require_structure_service_access_in_snap(
            Some(&ctx),
            snap,
            st_id,
            public_id,
            StructureServiceKind::Docking,
            now
        )
        .is_ok());

        // 4. CorpManagement rejects external pilot AND rejects pending_dir_id before 24h stasis,
        //    then allows pending_dir_id at now + 86_400 and always allows CEO.
        assert_eq!(
            require_structure_service_access_in_snap(
                Some(&ctx),
                snap,
                st_id,
                public_id,
                StructureServiceKind::CorpManagement,
                now
            )
            .unwrap_err()
            .code,
            "StructureOwnershipViolation"
        );
        assert_eq!(
            require_structure_service_access_in_snap(
                Some(&ctx),
                snap,
                st_id,
                pending_dir_id,
                StructureServiceKind::CorpManagement,
                now
            )
            .unwrap_err()
            .code,
            "InsufficientCorpRoles"
        );
        assert!(require_structure_service_access_in_snap(
            Some(&ctx),
            snap,
            st_id,
            pending_dir_id,
            StructureServiceKind::CorpManagement,
            now + 86_400
        )
        .is_ok());
        assert!(require_structure_service_access_in_snap(
            Some(&ctx),
            snap,
            st_id,
            ceo_id,
            StructureServiceKind::CorpManagement,
            now
        )
        .is_ok());
    });
}

#[test]
fn test_check_service_domain_acl_and_tax_and_alliance_rule_slice_precedence() {
    use crate::services::structure_acl_defense_rules::{
        ACL_ROLE_ADMIN, ACL_ROLE_EXCLUDED, ACL_ROLE_MEMBER,
    };

    let ctx = test_ctx_with_clock(1_800_000_000);
    let now = ctx.now_unix();
    let corp_a = 98_500_001_i64;
    let corp_b = 98_500_002_i64;
    let corp_neutral = 98_500_003_i64;
    let ally_id = 99_500_001_i64;
    let pilot_a = 950_001_i64;
    let pilot_b = 950_002_i64;
    let pilot_c = 950_003_i64;
    let pilot_neutral = 950_004_i64;
    let unknown_pilot = 999_999_i64;
    let st_id = 1_000_500_001_i64;
    let g_allow = 701_i64;
    let g_excl_corp = 702_i64;
    let g_excl_ally = 703_i64;
    let g_allow_corp_a = 704_i64;
    let delayed_prof_id = 880_i64;

    ctx.db.with_mut_snapshot(|snap| {
        for (cid, aid) in [
            (corp_a, Some(ally_id)),
            (corp_b, Some(ally_id)),
            (corp_neutral, None),
        ] {
            let mut corp = CorporationRecord::new(cid, format!("Corp {cid}"), "CP", pilot_a);
            corp.alliance_id = aid;
            snap.corporations.insert(cid, corp);
        }

        // pilot_a intentionally keeps alliance_id = None to test corp.alliance_id fallback in pilot_corp_and_ally
        let mut ch_a = clone_char_with(snap, pilot_a, "PilotA", corp_a, 30_000_142, 0);
        ch_a.alliance_id = None;
        let mut ch_b = clone_char_with(snap, pilot_b, "PilotB", corp_a, 30_000_142, 0);
        ch_b.alliance_id = Some(ally_id);
        let mut ch_c = clone_char_with(snap, pilot_c, "PilotC", corp_b, 30_000_142, 0);
        ch_c.alliance_id = Some(ally_id);
        let mut ch_n = clone_char_with(snap, pilot_neutral, "Neutral", corp_neutral, 30_000_142, 0);
        ch_n.alliance_id = None;
        for ch in [ch_a, ch_b, ch_c, ch_n] {
            snap.characters.insert(ch.character_id, ch);
        }

        let mut ext = load_ext_state(snap);
        ext.owner_groups.insert(
            g_allow,
            ops::AccessGroupRecord {
                group_id: g_allow,
                creator_id: pilot_a,
                owner_corp_id: corp_a,
                name: "Ally Allow".into(),
                description: String::new(),
                members: HashMap::from([(ally_id, ACL_ROLE_MEMBER)]),
            },
        );
        ext.owner_groups.insert(
            g_excl_corp,
            ops::AccessGroupRecord {
                group_id: g_excl_corp,
                creator_id: pilot_a,
                owner_corp_id: corp_a,
                name: "CorpB Excl".into(),
                description: String::new(),
                members: HashMap::from([(corp_b, ACL_ROLE_EXCLUDED)]),
            },
        );
        ext.owner_groups.insert(
            g_excl_ally,
            ops::AccessGroupRecord {
                group_id: g_excl_ally,
                creator_id: pilot_a,
                owner_corp_id: corp_a,
                name: "Ally Excl".into(),
                description: String::new(),
                members: HashMap::from([(ally_id, ACL_ROLE_EXCLUDED)]),
            },
        );
        ext.owner_groups.insert(
            g_allow_corp_a,
            ops::AccessGroupRecord {
                group_id: g_allow_corp_a,
                creator_id: pilot_a,
                owner_corp_id: corp_a,
                name: "CorpA Admin".into(),
                description: String::new(),
                members: HashMap::from([(corp_a, ACL_ROLE_ADMIN)]),
            },
        );

        let mut active_prof = StructureAccessProfile::default();
        active_prof
            .service_acl_rules
            .insert(SETTING_REPROCESSING_TAX, vec![(g_allow, 350), (g_excl_corp, 100)]);
        active_prof
            .service_acl_rules
            .insert(SETTING_MANUFACTURING_TAX, vec![(g_allow_corp_a, 150), (g_excl_ally, 50)]);
        ext.access_profiles.insert(st_id, active_prof);

        let mut delayed_prof = StructureAccessProfile {
            profile_id: delayed_prof_id,
            ..Default::default()
        };
        delayed_prof
            .service_acl_rules
            .insert(SETTING_REPROCESSING_TAX, vec![(g_allow, 200)]);
        ext.profiles_by_id.insert(delayed_prof_id, delayed_prof);
        ext.pending_profile_applications
            .insert(st_id, (delayed_prof_id, now + 600));
        save_ext_state(snap, &ext);
    });

    ctx.db.with_snapshot(|snap| {
        // pilot_a inherits ally_id from corp_a.alliance_id -> allowed at active 350 bps before apply_at
        assert_eq!(
            check_service_domain_acl_and_tax(
                snap,
                st_id,
                pilot_a,
                &[SETTING_REPROCESSING_TAX],
                false,
                Some(now),
                "CustomReprocDenied",
                "reprocessing",
            )
            .unwrap(),
            Some(350)
        );
        assert_eq!(
            check_service_domain_acl_and_tax(
                snap,
                st_id,
                pilot_b,
                &[SETTING_REPROCESSING_TAX],
                false,
                Some(now),
                "CustomReprocDenied",
                "reprocessing",
            )
            .unwrap(),
            Some(350)
        );
        // At apply_at (now + 600), pending profile takes effect -> 200 bps
        assert_eq!(
            check_service_domain_acl_and_tax(
                snap,
                st_id,
                pilot_a,
                &[SETTING_REPROCESSING_TAX],
                false,
                Some(now + 600),
                "CustomReprocDenied",
                "reprocessing",
            )
            .unwrap(),
            Some(200)
        );

        // pilot_c matches ally_id in g_allow, but corp_b exclusion in g_excl_corp overrides it
        let err_c = check_service_domain_acl_and_tax(
            snap,
            st_id,
            pilot_c,
            &[SETTING_REPROCESSING_TAX],
            false,
            Some(now),
            "CustomReprocDenied",
            "reprocessing",
        )
        .unwrap_err();
        assert_eq!(err_c.code, "CustomReprocDenied");
        assert!(err_c.message.contains("reprocessing"));

        // pilot_a (via corp_a.alliance_id fallback) and pilot_b are excluded when alliance is excluded
        for pid in [pilot_a, pilot_b] {
            let err_ally = check_service_domain_acl_and_tax(
                snap,
                st_id,
                pid,
                &[SETTING_MANUFACTURING_TAX],
                false,
                Some(now),
                "CustomMfgDenied",
                "manufacturing",
            )
            .unwrap_err();
            assert_eq!(err_ally.code, "CustomMfgDenied");
            assert!(err_ally.message.contains("manufacturing"));
        }

        // Unknown character resolves to (0, 0) and is denied; neutral pilot not in g_allow is denied
        for pid in [unknown_pilot, pilot_neutral] {
            assert_eq!(
                check_service_domain_acl_and_tax(
                    snap,
                    st_id,
                    pid,
                    &[SETTING_REPROCESSING_TAX],
                    false,
                    Some(now),
                    "CustomReprocDenied",
                    "reprocessing",
                )
                .unwrap_err()
                .code,
                "CustomReprocDenied"
            );
        }

        // Unknown structure ID, empty rule slice, and unconfigured setting each return Ok(None)
        assert_eq!(
            check_service_domain_acl_and_tax(
                snap,
                999_999_999,
                pilot_a,
                &[SETTING_REPROCESSING_TAX],
                false,
                Some(now),
                "CustomReprocDenied",
                "reprocessing",
            )
            .unwrap(),
            None
        );
        assert_eq!(
            check_service_domain_acl_and_tax(
                snap,
                st_id,
                pilot_a,
                &[],
                false,
                Some(now),
                "CustomReprocDenied",
                "reprocessing",
            )
            .unwrap(),
            None
        );
        assert_eq!(
            check_service_domain_acl_and_tax(
                snap,
                st_id,
                pilot_a,
                &[SETTING_CYNO_BEACON],
                false,
                Some(now),
                "CustomReprocDenied",
                "cyno",
            )
            .unwrap(),
            None
        );
    });
}

#[test]
fn test_require_structure_service_access_market_industry_jump_bridge_and_control_branches() {
    use crate::services::structure_acl_defense_rules::ACL_ROLE_MEMBER;

    let ctx = test_ctx_with_clock(1_800_000_000);
    let now = ctx.now_unix();
    let owner_corp = 98_600_001_i64;
    let allowed_corp = 98_600_002_i64;
    let neutral_corp = 98_600_003_i64;
    let blocked_corp = 98_600_004_i64;
    let ally_blocked_corp = 98_600_005_i64;
    let ally_ok = 99_600_001_i64;
    let ally_blocked = 99_600_009_i64;

    let dir_id = 960_001_i64;
    let member_no_role = 960_002_i64;
    let allowed_id = 960_003_i64;
    let outsider_id = 960_004_i64;
    let blocked_char = 960_005_i64;
    let blocked_corp_pilot = 960_006_i64;
    let blocked_ally_pilot = 960_007_i64;
    let neg_standing_pilot = 960_008_i64;

    let fortizar_id = 1_000_600_001_i64;
    let fortizar_market_disabled_id = 1_000_600_002_i64;
    let athanor_id = 1_000_600_003_i64;
    let jb_id = 1_000_600_004_i64;
    let jb_fallback_id = 1_000_600_005_i64;
    let anchored_id = 1_000_600_006_i64;
    let uncored_id = 1_000_600_007_i64;
    let abandoned_id = 1_000_600_008_i64;
    let destroyed_id = 1_000_600_009_i64;

    let g_mkt = 711_i64;
    let g_ind = 712_i64;
    let g_jb = 713_i64;
    let g_def = 714_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut own = CorporationRecord::new(owner_corp, "Owner Corp", "OWN", dir_id);
        own.alliance_id = Some(ally_ok);
        own.members = std::collections::HashSet::from([dir_id, member_no_role]);
        own.member_roles.insert(dir_id, corp_roles::ROLE_DIRECTOR);
        snap.corporations.insert(owner_corp, own);

        for (cid, aid) in [
            (allowed_corp, Some(ally_ok)),
            (neutral_corp, None),
            (blocked_corp, None),
            (ally_blocked_corp, Some(ally_blocked)),
        ] {
            let mut c = CorporationRecord::new(cid, format!("C{cid}"), "CP", dir_id);
            c.alliance_id = aid;
            snap.corporations.insert(cid, c);
        }

        for (pid, cid, aid_opt) in [
            (dir_id, owner_corp, Some(ally_ok)),
            (member_no_role, owner_corp, Some(ally_ok)),
            (allowed_id, allowed_corp, None),
            (outsider_id, neutral_corp, None),
            (blocked_char, neutral_corp, None),
            (blocked_corp_pilot, blocked_corp, None),
            (blocked_ally_pilot, ally_blocked_corp, None), // alliance resolved via corp
            (neg_standing_pilot, neutral_corp, None),
        ] {
            let mut ch = clone_char_with(snap, pid, &format!("Pilot{pid}"), cid, 30_000_142, 0);
            ch.alliance_id = aid_opt;
            if pid == neg_standing_pilot {
                ch.standings.insert(owner_corp, -5.0);
            }
            snap.characters.insert(pid, ch);
        }

        let mk_struct = |sid: i64, tid: u32, state: &str, cored: bool, fuel: u32| {
            UpwellStructureRecord {
                structure_id: sid,
                type_id: tid,
                owner_corp_id: owner_corp,
                solar_system_id: 30_000_142,
                name: format!("Struct-{sid}"),
                position: [0.0; 3],
                state: state.into(),
                quantum_core_installed: cored,
                fuel_blocks: fuel,
                shield_hp: 1.0,
                armor_hp: 1.0,
                hull_hp: 1.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            }
        };
        snap.structures.insert(fortizar_id, mk_struct(fortizar_id, 35_833, "ShieldVulnerable", true, 100));
        snap.structures.insert(fortizar_market_disabled_id, mk_struct(fortizar_market_disabled_id, 35_833, "ShieldVulnerable", true, 100));
        snap.structures.insert(athanor_id, mk_struct(athanor_id, 35_835, "ShieldVulnerable", true, 100));
        snap.structures.insert(jb_id, mk_struct(jb_id, 35_841, "ShieldVulnerable", true, 100));
        snap.structures.insert(jb_fallback_id, mk_struct(jb_fallback_id, 35_841, "ShieldVulnerable", true, 100));
        snap.structures.insert(anchored_id, mk_struct(anchored_id, 35_833, "Anchoring", true, 100));
        snap.structures.insert(uncored_id, mk_struct(uncored_id, 35_833, "ShieldVulnerable", false, 100));
        snap.structures.insert(abandoned_id, mk_struct(abandoned_id, 35_835, "ShieldVulnerable", true, 0));
        snap.structures.insert(destroyed_id, mk_struct(destroyed_id, 35_833, "Destroyed", false, 0));

        for (item_id, sid) in [(880_001_i64, fortizar_id), (880_002_i64, fortizar_market_disabled_id)] {
            snap.items.insert(
                item_id,
                eve_db::InventoryItemRecord {
                    item_id,
                    type_id: 35_892, // Standup Market Hub I
                    owner_id: owner_corp,
                    location_id: sid,
                    flag: eve_db::InventoryFlag::ServiceSlot0,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            snap.module_online_states.insert(item_id, true);
        }

        let mut ext = load_ext_state(snap);
        for (gid, members) in [
            (g_mkt, HashMap::from([(allowed_id, ACL_ROLE_MEMBER)])),
            (g_ind, HashMap::from([(allowed_id, ACL_ROLE_MEMBER)])),
            (g_jb, HashMap::from([(allowed_id, ACL_ROLE_MEMBER)])),
            (g_def, HashMap::from([(allowed_id, ACL_ROLE_MEMBER)])),
        ] {
            ext.owner_groups.insert(
                gid,
                ops::AccessGroupRecord {
                    group_id: gid,
                    creator_id: dir_id,
                    owner_corp_id: owner_corp,
                    name: format!("G{gid}"),
                    description: String::new(),
                    members,
                },
            );
        }

        let mut fort_prof = StructureAccessProfile::default();
        fort_prof.allowed_characters.extend([allowed_id, outsider_id, neg_standing_pilot]);
        fort_prof.service_acl_rules.insert(SETTING_MARKET_TAX, vec![(g_mkt, 250)]);
        fort_prof.defense_group_ids.insert(g_def);
        ext.access_profiles.insert(fortizar_id, fort_prof);

        let mut mkt_off_prof = StructureAccessProfile::default();
        mkt_off_prof.allowed_characters.insert(allowed_id);
        mkt_off_prof.general_settings.insert(SETTING_MARKET_TAX, false);
        ext.access_profiles.insert(fortizar_market_disabled_id, mkt_off_prof);

        let mut ath_prof = StructureAccessProfile::default();
        ath_prof.allowed_characters.extend([allowed_id, outsider_id]);
        ath_prof.blocked_characters.insert(blocked_char);
        ath_prof.service_acl_rules.insert(SETTING_MANUFACTURING_TAX, vec![(g_ind, 110)]);
        ath_prof.service_acl_rules.insert(SETTING_RESEARCH_TAX, vec![(g_ind, 220)]);
        ath_prof.service_acl_rules.insert(SETTING_INVENTION_TAX, vec![(g_ind, 330)]);
        ath_prof.service_acl_rules.insert(SETTING_REACTION_COMPOSITE_TAX, vec![(g_ind, 440)]);
        ath_prof.service_acl_rules.insert(SETTING_INDUSTRY_TAX, vec![(g_ind, 550)]);
        ext.access_profiles.insert(athanor_id, ath_prof);

        let mut jb_prof = StructureAccessProfile::default();
        jb_prof.blocked_characters.insert(blocked_char);
        jb_prof.blocked_corporations.insert(blocked_corp);
        jb_prof.blocked_alliances.insert(ally_blocked);
        jb_prof.service_acl_rules.insert(SETTING_JUMP_BRIDGE_ACTIVATION, vec![(g_jb, 420)]);
        ext.access_profiles.insert(jb_id, jb_prof);

        let mut jb_fb_prof = StructureAccessProfile::default();
        jb_fb_prof.allowed_characters.insert(allowed_id);
        ext.access_profiles.insert(jb_fallback_id, jb_fb_prof);

        ext.lifecycle.insert(
            abandoned_id,
            ops::StructureLifecycleState {
                low_power_since_unix: Some(now - 8 * 86_400),
                ..Default::default()
            },
        );
        save_ext_state(snap, &ext);
    });

    ctx.db.with_snapshot(|snap| {
        // 1. StructureNotFound vs CloneBay StructureAccessDenied; Destroyed variants
        assert_eq!(
            require_structure_service_access_in_snap(Some(&ctx), snap, 999_888_777, allowed_id, StructureServiceKind::Market, now).unwrap_err().code,
            "StructureNotFound"
        );
        assert_eq!(
            require_structure_service_access_in_snap(Some(&ctx), snap, 999_888_777, allowed_id, StructureServiceKind::CloneBay, now).unwrap_err().code,
            "StructureAccessDenied"
        );
        for (kind, expected) in [
            (StructureServiceKind::Market, "StructureMarketOffline"),
            (StructureServiceKind::Reprocessing, "ReprocessingServiceOffline"),
            (StructureServiceKind::CloneBay, "StructureAccessDenied"),
            (StructureServiceKind::Industry { activity_id: 1 }, "StructureServiceAccessDenied"),
            (StructureServiceKind::JumpBridge, "StructureServiceAccessDenied"),
            (StructureServiceKind::StructureControl, "StructureDestroyed"),
        ] {
            assert_eq!(
                require_structure_service_access_in_snap(Some(&ctx), snap, destroyed_id, dir_id, kind, now).unwrap_err().code,
                expected
            );
        }

        // 2. Market service: ACL allow (250 bps), ACL deny, general_settings disabled, negative standing, offline
        assert_eq!(
            require_structure_service_access_in_snap(Some(&ctx), snap, fortizar_id, allowed_id, StructureServiceKind::Market, now).unwrap(),
            Some(250)
        );
        assert_eq!(
            require_structure_service_access_in_snap(Some(&ctx), snap, fortizar_id, outsider_id, StructureServiceKind::Market, now).unwrap_err().code,
            "StructureServiceAccessDenied"
        );
        assert_eq!(
            require_structure_service_access_in_snap(Some(&ctx), snap, fortizar_id, neg_standing_pilot, StructureServiceKind::Market, now).unwrap_err().code,
            "StructureServiceAccessDenied"
        );
        assert_eq!(
            require_structure_service_access_in_snap(Some(&ctx), snap, fortizar_market_disabled_id, allowed_id, StructureServiceKind::Market, now).unwrap_err().code,
            "StructureServiceAccessDenied"
        );
        assert_eq!(
            require_structure_service_access_in_snap(Some(&ctx), snap, uncored_id, allowed_id, StructureServiceKind::Market, now).unwrap_err().code,
            "StructureMarketOffline"
        );

        // 3. Industry activities (1, 3/4/5, 8, 9/11, default) + denied & docking-blocked
        for (activity_id, expected_bps) in [(1, 110), (3, 220), (4, 220), (5, 220), (8, 330), (9, 440), (11, 440), (99, 110)] {
            assert_eq!(
                require_structure_service_access_in_snap(
                    Some(&ctx),
                    snap,
                    athanor_id,
                    allowed_id,
                    StructureServiceKind::Industry { activity_id },
                    now,
                )
                .unwrap(),
                Some(expected_bps)
            );
        }
        for denied in [outsider_id, blocked_char] {
            assert_eq!(
                require_structure_service_access_in_snap(
                    Some(&ctx),
                    snap,
                    athanor_id,
                    denied,
                    StructureServiceKind::Industry { activity_id: 1 },
                    now,
                )
                .unwrap_err()
                .code,
                "StructureServiceAccessDenied"
            );
        }

        // 4. JumpBridge: blocked char/corp/ally, ACL allow/deny, absent-ACL fallback
        for blk in [blocked_char, blocked_corp_pilot, blocked_ally_pilot, outsider_id] {
            assert_eq!(
                require_structure_service_access_in_snap(Some(&ctx), snap, jb_id, blk, StructureServiceKind::JumpBridge, now).unwrap_err().code,
                "StructureServiceAccessDenied"
            );
        }
        assert_eq!(
            require_structure_service_access_in_snap(Some(&ctx), snap, jb_id, allowed_id, StructureServiceKind::JumpBridge, now).unwrap(),
            Some(420)
        );
        assert_eq!(
            require_structure_service_access_in_snap(Some(&ctx), snap, jb_fallback_id, allowed_id, StructureServiceKind::JumpBridge, now).unwrap(),
            None
        );
        assert_eq!(
            require_structure_service_access_in_snap(Some(&ctx), snap, jb_fallback_id, outsider_id, StructureServiceKind::JumpBridge, now).unwrap_err().code,
            "StructureServiceAccessDenied"
        );

        // 5. StructureControl & Repair operational/core/abandoned guards
        assert_eq!(
            require_structure_service_access_in_snap(Some(&ctx), snap, anchored_id, dir_id, StructureServiceKind::StructureControl, now).unwrap_err().code,
            "StructureNotOperational"
        );
        assert_eq!(
            require_structure_service_access_in_snap(Some(&ctx), snap, uncored_id, dir_id, StructureServiceKind::StructureControl, now).unwrap_err().code,
            "StructureNotOperational"
        );
        assert_eq!(
            require_structure_service_access_in_snap(Some(&ctx), snap, fortizar_id, dir_id, StructureServiceKind::StructureControl, now).unwrap(),
            None
        );
        assert_eq!(
            require_structure_service_access(&ctx, snap, fortizar_id, allowed_id, StructureServiceKind::StructureControl).unwrap(),
            None
        );
        assert_eq!(
            require_structure_service_access_in_snap(Some(&ctx), snap, fortizar_id, outsider_id, StructureServiceKind::StructureControl, now).unwrap_err().code,
            "DefenseAccessDenied"
        );
        assert_eq!(
            require_structure_service_access_in_snap(Some(&ctx), snap, fortizar_id, member_no_role, StructureServiceKind::StructureControl, now).unwrap_err().code,
            "CorpRoleDenied"
        );

        assert_eq!(
            require_structure_service_access_in_snap(Some(&ctx), snap, anchored_id, dir_id, StructureServiceKind::Repair, now).unwrap_err().code,
            "StructureNotOperational"
        );
        assert_eq!(
            require_structure_service_access_in_snap(Some(&ctx), snap, uncored_id, dir_id, StructureServiceKind::Repair, now).unwrap_err().code,
            "QuantumCoreRequired"
        );
        assert_eq!(
            require_structure_service_access_in_snap(None, snap, uncored_id, dir_id, StructureServiceKind::Repair, now).unwrap_err().code,
            "QuantumCoreRequired"
        );
        assert_eq!(
            require_structure_service_access_in_snap(Some(&ctx), snap, abandoned_id, dir_id, StructureServiceKind::Repair, now).unwrap_err().code,
            "StructureAbandoned"
        );
    });
}


