//! Extra unit and mutation-killing tests for `services::social_corp_fleet::social_corp_projects`.

use crate::quasar_gateway::{STATUS_BAD_REQUEST, STATUS_NOT_FOUND, STATUS_PRECONDITION_FAILED};
use crate::router::ServerContext;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::make_fighter_ctx_and_session;
use crate::services::quasar_goal_tale_notices::CORP_GOAL_UUID_PREFIX;
use crate::services::social_corp_fleet::social_corp_projects::*;
use eve_db::{corp_roles, CorpWalletDivisionRecord, CorporationRecord, IskCents};
use eve_machonet::SessionState;
use eve_proto::quasar::{DynamicQuasarMessage, DynamicValue};
use eve_proto::PyRep;

fn setup_projects_ctx() -> (ServerContext, SessionState, i64, i64) {
    let (ctx, mut sess, char_id, _ship_id, _sys_id) = make_fighter_ctx_and_session();
    let corp_id = 98_100_020_i64;
    sess.corporation_id = Some(corp_id);
    sess.corp_role = corp_roles::ROLE_DIRECTOR;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
            ch.balance = IskCents::from_isk(10_000_000);
        }
        let mut corp = { let mut __corp = CorporationRecord::default();
            __corp.corporation_id = corp_id;
            __corp.name = "Extra Project Corp".into();
            __corp.ticker = "XPRC".into();
            __corp.ceo_id = char_id;
            __corp.members = [char_id].into_iter().collect();
            __corp.member_roles = [(char_id, corp_roles::ROLE_DIRECTOR)].into_iter().collect();
            __corp.set_division_balance(1, IskCents::from_isk(50_000_000));
            __corp };
        corp.ensure_wallet_divisions();
        snap.corporations.insert(corp_id, corp);
    });

    (ctx, sess, char_id, corp_id)
}

#[test]
fn uuid_edge_cases_and_effective_corp_member_roles_maturation() {
    // 1. Short/zero/negative UUIDs and freelance collision with CORP_GOAL_UUID_PREFIX
    assert_eq!(goal_uuid_to_project_id(&[0u8; 8]), None);
    assert_eq!(freelance_uuid_to_project_id(&[0u8; 8]), None);
    assert_eq!(goal_uuid_to_project_id(&corp_goal_uuid(0)), None);
    assert_eq!(goal_uuid_to_project_id(&corp_goal_uuid(-5)), None);

    let prefix_u64 = u64::from_be_bytes(*CORP_GOAL_UUID_PREFIX);
    let fl_colliding = freelance_uuid(prefix_u64, 777);
    assert_ne!(&fl_colliding[0..8], CORP_GOAL_UUID_PREFIX);
    assert_eq!(freelance_uuid_to_project_id(&fl_colliding), Some(777));
    assert_eq!(goal_uuid_to_project_id(&fl_colliding), None);

    // 2. effective_corp_member_roles: missing corp, role_assignments, pending_role_grants, and pending_title_role_grants
    let (ctx, _, ceo_id, corp_id) = setup_projects_ctx();
    let member_id = 90_888_001i64;
    let div_take_bit = 1u64 << 13; // inside HANGAR_DIV_MASK
    let director_bit = corp_roles::ROLE_DIRECTOR;

    ctx.db.with_mut_snapshot(|snap| {
        assert_eq!(
            effective_corp_member_roles(snap, 999_999_999, member_id, 42, 1_000),
            (false, 42)
        );
        assert!(effective_corp_member_roles(snap, corp_id, ceo_id, 0, 1_000).0);

        let ra = snap
            .corp_role_assignments
            .entry((corp_id, member_id))
            .or_default();
        ra.character_id = member_id;
        ra.roles = 1u64 << 5;

        let gov = snap.corp_governance.entry(corp_id).or_default();
        gov.pending_role_grants
            .insert(member_id, (director_bit, 2_000));
        gov.pending_title_role_grants
            .insert(member_id, (div_take_bit, 2_000));
        gov.member_titles.insert(member_id, 1); // title 1
        gov.titles
            .insert(1, ("Quartermaster".into(), div_take_bit | (1u64 << 6)));

        // Before maturation (t = 1_500): pending_role_grants not applied, unmatured title bit masked out
        let (is_ceo_before, roles_before) =
            effective_corp_member_roles(snap, corp_id, member_id, 0, 1_500);
        assert!(!is_ceo_before);
        assert_ne!(roles_before & (1u64 << 5), 0);
        assert_ne!(roles_before & (1u64 << 6), 0);
        assert_eq!(roles_before & director_bit, 0);
        assert_eq!(roles_before & div_take_bit, 0);

        // At/after maturation (t = 2_000): both pending grants and title bits are active
        let (_, roles_after) = effective_corp_member_roles(snap, corp_id, member_id, 0, 2_000);
        assert_ne!(roles_after & director_bit, 0);
        assert_ne!(roles_after & div_take_bit, 0);
    });
}

#[test]
fn parse_goal_contribution_config_and_create_options_all_branches() {
    // Test all field mappings and Manual ignoring filter_id
    let cases: &[(u32, &str, Option<i64>)] = &[
        (1, "Manual", None),
        (2, "DeliverItem", Some(34)),
        (37, "DeliverItem", Some(34)),
        (3, "DestroyNPCShips", Some(34)),
        (4, "DamageShips", Some(34)),
        (22, "MineMaterials", Some(34)),
        (6, "DestroyCapsuleerShips", Some(34)),
        (7, "CaptureFWComplexes", Some(34)),
        (8, "DefendFWComplexes", Some(34)),
        (9, "RemoteRepairArmor", Some(34)),
        (10, "RemoteRepairShield", Some(34)),
        (11, "ManufactureItems", Some(34)),
        (12, "ScanSignatures", Some(34)),
        (16, "SalvageWrecks", Some(34)),
        (17, "EarnLoyaltyPoints", Some(34)),
    ];
    for &(field_num, expected_matcher, expected_filter) in cases {
        let mut cfg = DynamicQuasarMessage::new("test.Cfg");
        let mut sub = DynamicQuasarMessage::new("test.Sub");
        let mut nested = DynamicQuasarMessage::new("test.Nested");
        nested.fields.insert(3, DynamicValue::Sint32(34));
        sub.fields
            .insert(2, DynamicValue::Repeated(vec![DynamicValue::Message(Box::new(nested))]));
        cfg.fields
            .insert(field_num, DynamicValue::Message(Box::new(sub)));
        let (m, f) = parse_goal_contribution_config(Some(&DynamicValue::Message(Box::new(cfg))));
        assert_eq!(m, expected_matcher);
        assert_eq!(f, expected_filter);
    }

    // Negative reward rejected
    let mut bad_req = DynamicQuasarMessage::new("test.Bad");
    bad_req.fields.insert(8, DynamicValue::Sint64(-100));
    let Err((bad_status, _)) = parse_goal_create_options(&bad_req, 10) else {
        panic!("expected negative reward error");
    };
    assert_eq!(bad_status, STATUS_BAD_REQUEST);

    // Contribution period auto_payout + expiry + max_per_character
    let mut req = DynamicQuasarMessage::new("test.Create");
    let mut period_msg = DynamicQuasarMessage::new("test.Period");
    period_msg.fields.insert(1, DynamicValue::Enum(1));
    let mut isk_msg = DynamicQuasarMessage::new("test.Isk");
    isk_msg.fields.insert(1, DynamicValue::Uint32(100_000));
    period_msg
        .fields
        .insert(2, DynamicValue::Message(Box::new(isk_msg)));
    req.fields
        .insert(8, DynamicValue::Repeated(vec![DynamicValue::Message(Box::new(period_msg))]));
    let mut exp_msg = DynamicQuasarMessage::new("test.Exp");
    exp_msg.fields.insert(1, DynamicValue::Int64(1_900_000_000));
    req.fields
        .insert(10, DynamicValue::Message(Box::new(exp_msg)));
    req.fields.insert(12, DynamicValue::Sint64(25));

    let parsed = parse_goal_create_options(&req, 10).unwrap();
    assert!(parsed.auto_payout);
    assert_eq!(parsed.escrow, IskCents::from_isk(100_000));
    assert_eq!(parsed.isk_per_unit, IskCents::from_isk(10_000));
    assert_eq!(parsed.expires_unix, Some(1_900_000_000));
    assert_eq!(parsed.max_per_character, Some(25));
}

#[test]
fn project_validation_limits_auto_payout_freelance_batches_and_expiry_sweep() {
    let (ctx, mut sess, char_id, corp_id) = setup_projects_ctx();
    let now = ctx.now_unix();

    // 1. Invalid target_quantity == 0 or negative escrow -> STATUS_BAD_REQUEST
    assert_eq!(
        create_canonical_project(
            &ctx.db,
            CanonicalProjectSpec {
                corp_id,
                creator_id: char_id,
                name: "Zero".into(),
                description: String::new(),
                target_quantity: 0,
                initial_quantity: 0,
                escrow: IskCents::ZERO,
                isk_per_unit: IskCents::ZERO,
                wallet_division: 1,
                matcher_type: "Any".into(),
                is_freelance: false,
                auto_payout_on_contribution: false,
                max_per_character: None,
                expires_unix: None,
                filter_id: None,
                batch_size: 1,
                now_unix: now,
                min_project_id: 101,
            },
        )
        .unwrap_err()
        .0,
        STATUS_BAD_REQUEST
    );

    // 2. Insufficient funds in division 2 -> STATUS_BAD_REQUEST
    assert_eq!(
        create_canonical_project(
            &ctx.db,
            CanonicalProjectSpec {
                corp_id,
                creator_id: char_id,
                name: "Unfunded Div2".into(),
                description: String::new(),
                target_quantity: 10,
                initial_quantity: 0,
                escrow: IskCents::from_isk(1_000_000),
                isk_per_unit: IskCents::from_isk(100_000),
                wallet_division: 2,
                matcher_type: "Any".into(),
                is_freelance: false,
                auto_payout_on_contribution: false,
                max_per_character: None,
                expires_unix: None,
                filter_id: None,
                batch_size: 1,
                now_unix: now,
                min_project_id: 101,
            },
        )
        .unwrap_err()
        .0,
        STATUS_BAD_REQUEST
    );

    // 3. Auto-payout Corp Goal with max_per_character cap and filter_id
    let (auto_pid, _) = create_canonical_project(
        &ctx.db,
        CanonicalProjectSpec {
            corp_id,
            creator_id: char_id,
            name: "Auto Payout Goal".into(),
            description: "Mine Tritanium".into(),
            target_quantity: 10,
            initial_quantity: 0,
            escrow: IskCents::from_isk(100_000),
            isk_per_unit: IskCents::from_isk(10_000),
            wallet_division: 1,
            matcher_type: "MineMaterials".into(),
            is_freelance: false,
            auto_payout_on_contribution: true,
            max_per_character: Some(4),
            expires_unix: None,
            filter_id: Some(34),
            batch_size: 1,
            now_unix: now,
            min_project_id: 200,
        },
    )
    .unwrap();

    // Wrong filter_id (35) is ignored; matching filter_id (34) with qty=10 is capped to max_per_character=4 and auto-paid!
    record_server_project_activity(&ctx, char_id, &["MineMaterials"], 10, Some(35));
    assert_eq!(
        ctx.db.with_snapshot(|s| s.corp_projects[&auto_pid].current_quantity),
        0
    );
    let bal_before = ctx.db.with_snapshot(|s| s.characters[&char_id].balance);
    record_server_project_activity(&ctx, char_id, &["MineMaterials"], 10, Some(34));
    let bal_after = ctx.db.with_snapshot(|s| s.characters[&char_id].balance);
    assert_eq!(bal_after.0 - bal_before.0, IskCents::from_isk(40_000).0);
    // Because auto_payout_on_contribution is true, manual redeem returns STATUS_PRECONDITION_FAILED (0 unredeemed)
    assert_eq!(
        redeem_corp_goal_reward_for_char(&ctx.db, auto_pid, char_id, now)
            .unwrap_err()
            .0,
        STATUS_PRECONDITION_FAILED
    );

    // 4. Freelance project batch redemption & error paths
    let (fl_pid, _) = create_canonical_project(
        &ctx.db,
        CanonicalProjectSpec {
            corp_id,
            creator_id: char_id,
            name: "Freelance Salvage".into(),
            description: String::new(),
            target_quantity: 20,
            initial_quantity: 0,
            escrow: IskCents::from_isk(200_000),
            isk_per_unit: IskCents::ZERO, // tests fallback rate = escrow / target_quantity
            wallet_division: 1,
            matcher_type: "SalvageWrecks".into(),
            is_freelance: true,
            auto_payout_on_contribution: false,
            max_per_character: None,
            expires_unix: Some(now + 60),
            filter_id: None,
            batch_size: 5,
            now_unix: now,
            min_project_id: 300,
        },
    )
    .unwrap();

    // Redeeming corp goal on freelance pid -> STATUS_NOT_FOUND; redeeming freelance on corp goal pid -> STATUS_NOT_FOUND
    assert_eq!(
        redeem_corp_goal_reward_for_char(&ctx.db, fl_pid, char_id, now)
            .unwrap_err()
            .0,
        STATUS_NOT_FOUND
    );
    assert_eq!(
        redeem_freelance_reward_for_char(&ctx.db, auto_pid, char_id, now)
            .unwrap_err()
            .0,
        STATUS_NOT_FOUND
    );
    // Uncommitted participant activity on freelance project is ignored
    record_server_project_activity(&ctx, char_id, &["SalvageWrecks"], 12, None);
    assert_eq!(
        redeem_freelance_reward_for_char(&ctx.db, fl_pid, char_id, now)
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );

    // Commit participant and record 12 units (batch_size=5 -> 2 redeemable batches = 100,000 ISK)
    ctx.db.with_mut_snapshot(|snap| {
        snap.alliance_war_state
            .project_details
            .get_mut(&fl_pid)
            .unwrap()
            .committed_participants
            .push(char_id);
    });
    record_server_project_activity(&ctx, char_id, &["SalvageWrecks"], 12, None);
    let (batches, paid_isk) =
        redeem_freelance_reward_for_char(&ctx.db, fl_pid, char_id, now).unwrap();
    assert_eq!((batches, paid_isk), (2, 100_000));
    // Second redeem with no new full batches -> STATUS_BAD_REQUEST
    assert_eq!(
        redeem_freelance_reward_for_char(&ctx.db, fl_pid, char_id, now)
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );

    // 5. Expiry sweep refunds remaining 100,000 ISK escrow and marks status Expired
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(det) = snap.alliance_war_state.project_details.get_mut(&fl_pid) {
            det.expires_unix = Some(now.saturating_sub(1));
        }
    });
    sweep_expired_corp_projects(&ctx);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            snap.alliance_war_state.project_details[&fl_pid].status,
            "Expired"
        );
        assert_eq!(snap.corp_projects[&fl_pid].escrow_remaining, IskCents::ZERO);
    });

    // 6. handle_edit_project error branches: ProjectNotFound, AccessDenied, CorpRoleDenied, and delete project
    assert_eq!(
        handle_edit_project(&ctx, &mut sess, &[PyRep::Integer(999_999)])
            .unwrap()
            .unwrap_err()
            .code,
        "ProjectNotFound"
    );
    sess.corporation_id = Some(98_999_999);
    assert_eq!(
        handle_edit_project(&ctx, &mut sess, &[PyRep::Integer(auto_pid)])
            .unwrap()
            .unwrap_err()
            .code,
        "AccessDenied"
    );
    sess.corporation_id = Some(corp_id);
    sess.corp_role = 0;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.corporations.get_mut(&corp_id) {
            c.ceo_id = 90_999_999;
            c.member_roles.clear();
        }
        snap.corp_projects.get_mut(&auto_pid).unwrap().escrow_remaining =
            IskCents::from_isk(60_000);
        if let Some(c) = snap.corporations.get_mut(&corp_id) {
            c.wallet_divisions = vec![CorpWalletDivisionRecord {
                division_id: 1,
                name: "Master".into(),
                balance: IskCents::ZERO,
            }];
        }
    });
    assert_eq!(
        handle_edit_project(&ctx, &mut sess, &[PyRep::Integer(auto_pid)])
            .unwrap()
            .unwrap_err()
            .code,
        "CorpRoleDenied"
    );

    let deleted = close_or_delete_canonical_project(&ctx.db, auto_pid, true, now);
    assert_eq!(deleted, Some((corp_id, IskCents::from_isk(60_000))));
    assert_eq!(
        close_or_delete_canonical_project(&ctx.db, auto_pid, true, now),
        None
    );
}
