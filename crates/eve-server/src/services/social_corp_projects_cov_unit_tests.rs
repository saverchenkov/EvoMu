//! Comprehensive unit and mutation-killing tests for `social_corp_projects`.
//!
//! Covers UUID prefix collision handling, delayed 24h role/title maturation, Quasar
//! contribution/ISK/period/expiry option parsing, division wallet sync & 100-freelance cap,
//! corp-goal and batch-freelance reward math (including dust sweeps & overflow guards),
//! expiry sweeps + Quasar notices, server activity matching, and `handle_edit_project` ACLs.

#![forbid(unsafe_code)]

use super::*;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::make_fighter_ctx_and_session;
use crate::services::quasar_progression_extra::{uuid_to_hex, GenericGoalRecord};
use eve_db::{CharacterRecord, CorpWalletDivisionRecord, CorporationRecord, VirtualServerClock};
use std::sync::Arc;

fn build_projects_world(now_unix: u64) -> (ServerContext, SessionState, i64, i64) {
    let (ctx, mut sess, ceo_id, _ship_id, _sys_id) = make_fighter_ctx_and_session();
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(now_unix)));
    let corp_id = 98_220_001_i64;
    sess.corporation_id = Some(corp_id);
    sess.corp_role = corp_roles::ROLE_DIRECTOR;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&ceo_id) {
            ch.corporation_id = corp_id;
            ch.balance = IskCents::from_isk(10_000_000);
        }
        let mut corp = CorporationRecord {
            corporation_id: corp_id,
            name: "Project Coverage Corp".into(),
            ticker: "PCVC".into(),
            ceo_id,
            master_wallet: IskCents::from_isk(200_000_000),
            members: [ceo_id].into_iter().collect(),
            member_roles: [(ceo_id, corp_roles::ROLE_DIRECTOR)].into_iter().collect(),
            ..Default::default()
        };
        corp.ensure_wallet_divisions();
        corp.set_division_balance(3, IskCents::from_isk(50_000_000));
        snap.corporations.insert(corp_id, corp);
    });

    (ctx, sess, ceo_id, corp_id)
}

fn clone_pilot(snap: &mut eve_db::WorldStateSnapshot, cid: i64, corp_id: i64) {
    let mut ch: CharacterRecord = snap.characters.values().next().expect("seed char").clone();
    ch.character_id = cid;
    ch.user_id = cid as u64;
    ch.name = format!("ProjPilot_{cid}");
    ch.corporation_id = corp_id;
    ch.balance = IskCents::from_isk(5_000_000);
    snap.characters.insert(cid, ch);
}

#[test]
fn uuid_helpers_contributor_sync_wallet_sync_and_role_maturation_boundaries() {
    // 1. UUID boundary & prefix collision handling
    assert_eq!(goal_uuid_to_project_id(&[0u8; 15]), None);
    assert_eq!(goal_uuid_to_project_id(&[0u8; 17]), None);
    assert_eq!(freelance_uuid_to_project_id(&[0u8; 15]), None);
    assert_eq!(goal_uuid_to_project_id(&corp_goal_uuid(0)), None);
    assert_eq!(goal_uuid_to_project_id(&corp_goal_uuid(-9)), None);
    assert_eq!(freelance_uuid_to_project_id(&freelance_uuid(100, 0)), None);
    assert_eq!(freelance_uuid_to_project_id(&freelance_uuid(100, -1)), None);

    let colliding_ts = u64::from_be_bytes(*notices::CORP_GOAL_UUID_PREFIX);
    let fl_u = freelance_uuid(colliding_ts, 888);
    assert_eq!(&fl_u[0..8], &(colliding_ts.wrapping_add(1)).to_be_bytes());
    assert_eq!(freelance_uuid_to_project_id(&fl_u), Some(888));
    assert_eq!(goal_uuid_to_project_id(&fl_u), None);

    // 2. read_synced_corp_wallet_balance: div 1 syncs when master_wallet != div1 (both nonzero master & zero master)
    for (m_isk, d_isk, exp_isk) in [(7_500, 0, 7_500), (0, 4_200, 4_200)] {
        let mut c = CorporationRecord {
            corporation_id: 1,
            master_wallet: IskCents::from_isk(m_isk),
            wallet_divisions: vec![CorpWalletDivisionRecord {
                division_id: 1,
                name: "Master".into(),
                balance: IskCents::from_isk(d_isk),
            }],
            ..Default::default()
        };
        assert_eq!(read_synced_corp_wallet_balance(&mut c, 1), IskCents::from_isk(exp_isk));
        assert_eq!(c.division_balance(1), IskCents::from_isk(exp_isk));
    }

    // 3. sync_corp_project_contributors & effective_corp_member_roles exact maturation & title bitmask 1..=64
    let (ctx, _, ceo_id, corp_id) = build_projects_world(2_000);
    let mem_id = 91_000_001_i64;
    let delayed_bit = 1u64 << 14;
    let pm_bit = corp_roles::ROLE_PROJECT_MANAGER;
    let hi_title_bit = 1u64 << 50;

    ctx.db.with_mut_snapshot(|snap| {
        sync_corp_project_contributors(snap, 999_999); // missing pid safe no-op
        let mut det = PersistedProjectDetails::default();
        det.char_contributions.insert(300, 10);
        det.char_contributions.insert(100, 0); // zero filtered out
        det.char_contributions.insert(200, 5);
        snap.alliance_war_state.project_details.insert(55, det);
        sync_corp_project_contributors(snap, 55);
        let synced: Vec<(i64, u64)> = snap.corp_project_contributors[&55]
            .iter()
            .map(|e| (e.character_id, e.contributed_quantity))
            .collect();
        assert_eq!(synced, vec![(200, 5), (300, 10)]);

        assert_eq!(effective_corp_member_roles(snap, 404_404, mem_id, 77, 2_000), (false, 77));
        assert!(effective_corp_member_roles(snap, corp_id, ceo_id, 0, 2_000).0);

        snap.corp_role_assignments
            .entry((corp_id, mem_id))
            .or_default()
            .roles = 1u64 << 3;
        let gov = snap.corp_governance.entry(corp_id).or_default();
        gov.pending_role_grants.insert(mem_id, (pm_bit, 2_000));
        gov.pending_title_role_grants.insert(mem_id, (delayed_bit, 2_000));
        gov.member_titles.insert(mem_id, 1 | (1u64 << 63)); // titles 1 and 64
        gov.titles.insert(0, ("OutLow".into(), 1u64 << 10)); // tid 0 ignored
        gov.titles.insert(65, ("OutHigh".into(), 1u64 << 11)); // tid 65 ignored
        gov.titles.insert(1, ("T1".into(), delayed_bit | (1u64 << 4)));
        gov.titles.insert(64, ("T64".into(), hi_title_bit));

        // t = 1_999 (< 2_000): pending grant excluded, unmatured title bit masked out, tid 1 & 64 non-pending bits kept
        let (is_c, r_pre) = effective_corp_member_roles(snap, corp_id, mem_id, 0, 1_999);
        assert!(!is_c);
        assert_eq!(r_pre & pm_bit, 0);
        assert_eq!(r_pre & delayed_bit, 0);
        assert_eq!(r_pre & ((1u64 << 10) | (1u64 << 11)), 0);
        assert_ne!(r_pre & (1u64 << 3), 0);
        assert_ne!(r_pre & (1u64 << 4), 0);
        assert_ne!(r_pre & hi_title_bit, 0);

        // t = 2_000 (== matured_at): both pending grants and title delayed bits mature!
        let (_, r_mat) = effective_corp_member_roles(snap, corp_id, mem_id, 0, 2_000);
        assert_ne!(r_mat & pm_bit, 0);
        assert_ne!(r_mat & delayed_bit, 0);
    });
}

#[test]
fn quasar_id_extraction_isk_parsing_period_detection_and_all_matcher_mappings() {
    // 1. extract_first_positive_id_in_msg across all scalar & repeated variants and key ordering
    let mut empty_m = DynamicQuasarMessage::new("t.Empty");
    empty_m.fields.insert(1, DynamicValue::Uint64(0));
    empty_m.fields.insert(2, DynamicValue::Int64(-5));
    empty_m.fields.insert(3, DynamicValue::Bool(true));
    assert_eq!(extract_first_positive_id_in_msg(&empty_m), None);

    for (val, exp) in [
        (DynamicValue::Uint64(11), Some(11)),
        (DynamicValue::Int64(12), Some(12)),
        (DynamicValue::Sint64(13), Some(13)),
        (DynamicValue::Uint32(14), Some(14)),
        (DynamicValue::Int32(15), Some(15)),
        (DynamicValue::Sint32(16), Some(16)),
        (
            DynamicValue::Repeated(vec![
                DynamicValue::Uint64(0),
                DynamicValue::Int32(-2),
                DynamicValue::Bool(false),
                DynamicValue::Uint64(21),
            ]),
            Some(21),
        ),
        (DynamicValue::Repeated(vec![DynamicValue::Sint64(22)]), Some(22)),
        (DynamicValue::Repeated(vec![DynamicValue::Uint32(23)]), Some(23)),
        (DynamicValue::Repeated(vec![DynamicValue::Sint32(24)]), Some(24)),
    ] {
        let mut m = DynamicQuasarMessage::new("t.Id");
        m.fields.insert(1, DynamicValue::Uint64(0)); // skipped because 0
        m.fields.insert(2, val);
        assert_eq!(extract_first_positive_id_in_msg(&m), exp);
    }

    // 2. parse_goal_contribution_config across all field numbers + non-Message / unmatched fallbacks
    assert_eq!(
        parse_goal_contribution_config(Some(&DynamicValue::Uint64(9))),
        ("Any".into(), None)
    );
    let mut unmapped_m = DynamicQuasarMessage::new("t.Unmapped");
    unmapped_m.fields.insert(99, DynamicValue::Uint64(1));
    assert_eq!(
        parse_goal_contribution_config(Some(&DynamicValue::Message(Box::new(unmapped_m)))),
        ("Any".into(), None)
    );
    for &(fnum, matcher) in &[
        (1u32, "Manual"),
        (2, "DeliverItem"),
        (37, "DeliverItem"),
        (3, "DestroyNPCShips"),
        (31, "DestroyNPCShips"),
        (4, "DamageShips"),
        (26, "DamageShips"),
        (5, "MineMaterials"),
        (22, "MineMaterials"),
        (25, "MineMaterials"),
        (6, "DestroyCapsuleerShips"),
        (27, "DestroyCapsuleerShips"),
        (7, "CaptureFWComplexes"),
        (34, "CaptureFWComplexes"),
        (8, "DefendFWComplexes"),
        (35, "DefendFWComplexes"),
        (9, "RemoteRepairArmor"),
        (32, "RemoteRepairArmor"),
        (10, "RemoteRepairShield"),
        (33, "RemoteRepairShield"),
        (11, "ManufactureItems"),
        (13, "ManufactureItems"),
        (14, "ManufactureItems"),
        (38, "ManufactureItems"),
        (12, "ScanSignatures"),
        (28, "ScanSignatures"),
        (16, "SalvageWrecks"),
        (36, "SalvageWrecks"),
        (17, "EarnLoyaltyPoints"),
        (19, "EarnLoyaltyPoints"),
        (29, "EarnLoyaltyPoints"),
    ] {
        let mut cfg = DynamicQuasarMessage::new("t.Cfg");
        let mut outer = DynamicQuasarMessage::new("t.Outer");
        let mut inner = DynamicQuasarMessage::new("t.Inner");
        inner.fields.insert(1, DynamicValue::Int32(587));
        outer.fields.insert(4, DynamicValue::Message(Box::new(inner)));
        cfg.fields.insert(fnum, DynamicValue::Message(Box::new(outer)));
        let (got_m, got_f) =
            parse_goal_contribution_config(Some(&DynamicValue::Message(Box::new(cfg))));
        assert_eq!(got_m, matcher);
        assert_eq!(got_f, (matcher != "Manual").then_some(587));
    }

    // 3. parse_isk_value, has_contribution_period, and parse_goal_create_options branches
    assert!(parse_isk_value(&DynamicValue::Sint64(-1)).is_err());
    assert!(parse_isk_value(&DynamicValue::Int32(-1)).is_err());
    assert!(parse_isk_value(&DynamicValue::Uint64(u64::MAX)).is_err());
    assert!(parse_isk_value(&DynamicValue::Repeated(vec![
        DynamicValue::Int64(i64::MAX),
        DynamicValue::Int64(1),
    ]))
    .is_err());
    assert_eq!(parse_isk_value(&DynamicValue::Bool(true)).unwrap(), 0);
    assert_eq!(parse_isk_value(&DynamicValue::Int32(0)).unwrap(), 0);

    let mut msg_scalar2 = DynamicQuasarMessage::new("t.Scalar2");
    msg_scalar2.fields.insert(2, DynamicValue::Uint32(750));
    assert_eq!(parse_isk_value(&DynamicValue::Message(Box::new(msg_scalar2))).unwrap(), 750);

    let mut msg_zero2_fb1 = DynamicQuasarMessage::new("t.Fb1");
    msg_zero2_fb1.fields.insert(2, DynamicValue::Int64(0));
    msg_zero2_fb1.fields.insert(1, DynamicValue::Int64(900));
    assert_eq!(parse_isk_value(&DynamicValue::Message(Box::new(msg_zero2_fb1))).unwrap(), 900);

    for v1 in [
        DynamicValue::Enum(1),
        DynamicValue::Uint64(1),
        DynamicValue::Int64(1),
        DynamicValue::Uint32(1),
        DynamicValue::Int32(1),
    ] {
        let mut pm = DynamicQuasarMessage::new("t.Per");
        pm.fields.insert(1, v1);
        pm.fields
            .insert(2, DynamicValue::Message(Box::new(DynamicQuasarMessage::new("t.Sub"))));
        assert!(has_contribution_period(Some(&DynamicValue::Message(Box::new(pm)))));
    }
    assert!(!has_contribution_period(None));

    // no_expiry = true & unlimited = true suppress field 10 & field 26
    let mut req = DynamicQuasarMessage::new("t.Create");
    req.fields.insert(9, DynamicValue::Bool(true));
    let mut exp_m = DynamicQuasarMessage::new("t.Exp");
    exp_m.fields.insert(1, DynamicValue::Uint64(9_999_999));
    req.fields.insert(10, DynamicValue::Message(Box::new(exp_m.clone())));
    req.fields.insert(25, DynamicValue::Uint64(1));
    req.fields.insert(26, DynamicValue::Uint64(50));
    let opt1 = parse_goal_create_options(&req, 10).unwrap();
    assert_eq!(opt1.expires_unix, None);
    assert_eq!(opt1.max_per_character, None);
    assert_eq!(opt1.isk_per_unit, IskCents::ZERO);

    // Enable expiry & cap via field 10 (Uint64) and field 26 (Uint64)
    req.fields.remove(&9);
    req.fields.remove(&25);
    let opt2 = parse_goal_create_options(&req, 10).unwrap();
    assert_eq!(opt2.expires_unix, Some(9_999_999));
    assert_eq!(opt2.max_per_character, Some(50));
}

fn base_spec(corp_id: i64, creator_id: i64, name: &str, now_unix: u64) -> CanonicalProjectSpec {
    CanonicalProjectSpec {
        corp_id,
        creator_id,
        name: name.into(),
        description: String::new(),
        target_quantity: 5,
        initial_quantity: 0,
        escrow: IskCents::ZERO,
        isk_per_unit: IskCents::ZERO,
        wallet_division: 3,
        matcher_type: "Any".into(),
        is_freelance: false,
        auto_payout_on_contribution: false,
        max_per_character: None,
        expires_unix: None,
        filter_id: None,
        batch_size: 1,
        now_unix,
        min_project_id: 101,
    }
}

#[test]
fn freelance_cap_100_dust_sweep_pool_rates_and_redeem_error_paths() {
    let now = 1_760_000_000_u64;
    let (ctx, _, ceo_id, corp_id) = build_projects_world(now);
    let contributor = 91_200_001_i64;
    ctx.db.with_mut_snapshot(|snap| clone_pilot(snap, contributor, corp_id));

    // Negative target / negative escrow / negative isk_per_unit / missing corp / unfunded division
    for (c_id, tgt, esc, rate, div) in [
        (corp_id, 0_u64, IskCents::ZERO, IskCents::ZERO, 3_u8),
        (corp_id, 5, IskCents(-100), IskCents::ZERO, 3),
        (corp_id, 5, IskCents::ZERO, IskCents(-100), 3),
        (404_999_999, 5, IskCents::from_isk(1_000), IskCents::ZERO, 3),
        (corp_id, 5, IskCents::from_isk(1_000), IskCents::ZERO, 2),
    ] {
        let mut s = base_spec(c_id, 0, "Bad", now);
        s.target_quantity = tgt;
        s.escrow = esc;
        s.isk_per_unit = rate;
        s.wallet_division = div;
        assert_eq!(create_canonical_project(&ctx.db, s).unwrap_err().0, STATUS_BAD_REQUEST);
    }

    // Seed 99 active freelance projects, create 100th (dust-sweep 3-batch job), then 101st fails!
    for i in 0..99 {
        let mut s = base_spec(corp_id, 0, &format!("FL_{i}"), now);
        s.is_freelance = true;
        create_canonical_project(&ctx.db, s).unwrap();
    }
    let mut dust_s = base_spec(corp_id, 0, "DustSweepFL", now);
    dust_s.target_quantity = 3;
    dust_s.escrow = IskCents::from_isk(100);
    dust_s.matcher_type = "DestroyNPCShips".into();
    dust_s.is_freelance = true;
    dust_s.min_project_id = 500;
    let (dust_fl_pid, _) = create_canonical_project(&ctx.db, dust_s).unwrap();

    let mut s101 = base_spec(corp_id, ceo_id, "FL_101", now);
    s101.is_freelance = true;
    assert!(create_canonical_project(&ctx.db, s101)
        .unwrap_err()
        .1
        .contains("MaxFreelanceProjectsReached"));

    // Uncommitted participant ignored; commit contributor, contribute 2/3, redeem 2 batches (6,666 cents),
    // then contribute 3rd (triggers CompletedNotice) and verify dust sweep empties escrow!
    record_server_project_activity(&ctx, contributor, &["DestroyNPCShips"], 1, None);
    assert_eq!(
        redeem_freelance_reward_for_char(&ctx.db, dust_fl_pid, contributor, now).unwrap_err().0,
        STATUS_BAD_REQUEST
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.alliance_war_state
            .project_details
            .get_mut(&dust_fl_pid)
            .unwrap()
            .committed_participants
            .push(contributor);
    });
    record_server_project_activity(&ctx, contributor, &["DestroyNPCShips"], 2, None);
    assert_eq!(redeem_freelance_reward_for_char(&ctx.db, dust_fl_pid, contributor, now).unwrap().0, 2);
    assert_eq!(
        redeem_freelance_reward_for_char(&ctx.db, dust_fl_pid, contributor, now).unwrap_err().0,
        STATUS_BAD_REQUEST
    );
    assert_eq!(
        ctx.db.with_snapshot(|s| s.corp_projects[&dust_fl_pid].escrow_remaining.raw_cents()),
        3_334
    );
    record_server_project_activity(&ctx, contributor, &["DestroyNPCShips"], 1, None);
    assert_eq!(redeem_freelance_reward_for_char(&ctx.db, dust_fl_pid, contributor, now).unwrap().0, 1);
    assert_eq!(
        ctx.db.with_snapshot(|s| s.corp_projects[&dust_fl_pid].escrow_remaining),
        IskCents::ZERO
    );

    // Cross-type redeem errors & missing project errors
    assert_eq!(
        redeem_corp_goal_reward_for_char(&ctx.db, 999_999, contributor, now).unwrap_err().0,
        STATUS_NOT_FOUND
    );
    assert_eq!(
        redeem_corp_goal_reward_for_char(&ctx.db, dust_fl_pid, contributor, now).unwrap_err().0,
        STATUS_NOT_FOUND
    );
    assert_eq!(
        redeem_freelance_reward_for_char(&ctx.db, 999_999, contributor, now).unwrap_err().0,
        STATUS_NOT_FOUND
    );

    // Corp goal pool-derived unit rate (isk_per_unit == 0) + GenericGoalRecord sync + error guards
    let mut pool_s = base_spec(corp_id, ceo_id, "PoolRateGoal", now);
    pool_s.target_quantity = 4;
    pool_s.escrow = IskCents::from_isk(40_000);
    pool_s.min_project_id = 600;
    let (pool_goal_pid, goal_uuid) = create_canonical_project(&ctx.db, pool_s).unwrap();
    assert_eq!(
        redeem_freelance_reward_for_char(&ctx.db, pool_goal_pid, contributor, now).unwrap_err().0,
        STATUS_NOT_FOUND
    );

    ctx.quasar_gateway
        .progression
        .extra
        .generic_goals
        .write()
        .unwrap()
        .insert(
            uuid_to_hex(&goal_uuid),
            GenericGoalRecord {
                uuid: goal_uuid,
                creator_id: ceo_id as u32,
                name: "PoolRateGoal".into(),
                description: String::new(),
                desired_progress: 4,
                current_progress: 0,
                closed: false,
            },
        );

    assert_eq!(
        redeem_corp_goal_reward_for_char(&ctx.db, pool_goal_pid, contributor, now).unwrap_err().0,
        STATUS_PRECONDITION_FAILED
    );
    record_server_project_activity(&ctx, contributor, &["ScanSignatures"], 4, None);
    assert_eq!(
        ctx.quasar_gateway.progression.extra.generic_goals.read().unwrap()[&uuid_to_hex(&goal_uuid)]
            .current_progress,
        4
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&contributor).unwrap().balance = IskCents(i64::MAX);
    });
    assert_eq!(
        redeem_corp_goal_reward_for_char(&ctx.db, pool_goal_pid, contributor, now).unwrap_err().0,
        STATUS_PRECONDITION_FAILED
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&contributor).unwrap().balance = IskCents::from_isk(1_000);
        snap.alliance_war_state
            .project_details
            .get_mut(&pool_goal_pid)
            .unwrap()
            .char_contributions
            .insert(777_777, 1);
    });
    assert_eq!(
        redeem_corp_goal_reward_for_char(&ctx.db, pool_goal_pid, 777_777, now).unwrap_err().0,
        STATUS_PRECONDITION_FAILED
    );
    assert_eq!(
        redeem_corp_goal_reward_for_char(&ctx.db, pool_goal_pid, contributor, now).unwrap(),
        40_000
    );
    assert_eq!(
        redeem_corp_goal_reward_for_char(&ctx.db, pool_goal_pid, contributor, now).unwrap_err().0,
        STATUS_PRECONDITION_FAILED
    );
    assert_eq!(
        redeem_corp_goal_reward_for_char(&ctx.db, pool_goal_pid, 777_777, now).unwrap_err().0,
        STATUS_PRECONDITION_FAILED
    );
}

#[test]
fn activity_auto_payout_expired_notices_close_delete_and_edit_project_acls() {
    let now = 1_760_100_000_u64;
    let (ctx, mut sess, ceo_id, corp_id) = build_projects_world(now);
    let outsider = 91_300_001_i64;
    let pm_pilot = 91_300_002_i64;
    ctx.db.with_mut_snapshot(|snap| {
        clone_pilot(snap, outsider, 98_999_888);
        clone_pilot(snap, pm_pilot, corp_id);
        snap.corporations
            .get_mut(&corp_id)
            .unwrap()
            .member_roles
            .insert(pm_pilot, corp_roles::ROLE_PROJECT_MANAGER);
    });

    // 1. Pre-completed goal, Manual expiring goal, and Auto-payout filtered goal with max_per_character
    let mut done_s = base_spec(corp_id, ceo_id, "PreDone", now);
    done_s.initial_quantity = 5;
    done_s.min_project_id = 700;
    let (done_pid, _) = create_canonical_project(&ctx.db, done_s).unwrap();

    let mut man_s = base_spec(corp_id, ceo_id, "ManualGoal", now - 10);
    man_s.escrow = IskCents::from_isk(5_000);
    man_s.isk_per_unit = IskCents::from_isk(1_000);
    man_s.wallet_division = 1;
    man_s.matcher_type = "Manual".into();
    man_s.expires_unix = Some(now);
    man_s.min_project_id = 701;
    let (manual_pid, _) = create_canonical_project(&ctx.db, man_s).unwrap();

    let mut auto_s = base_spec(corp_id, ceo_id, "AutoFiltered", now);
    auto_s.target_quantity = 10;
    auto_s.escrow = IskCents::from_isk(50_000);
    auto_s.isk_per_unit = IskCents::from_isk(5_000);
    auto_s.wallet_division = 1;
    auto_s.matcher_type = "MineMaterials".into();
    auto_s.auto_payout_on_contribution = true;
    auto_s.max_per_character = Some(3);
    auto_s.filter_id = Some(34);
    auto_s.min_project_id = 702;
    let (auto_pid, _) = create_canonical_project(&ctx.db, auto_s).unwrap();

    // Wrong filter ignored; matching filter capped at max_per_character=3 & emits RedeemedNotice; 2nd call hits acc==0
    record_server_project_activity(&ctx, pm_pilot, &["MineMaterials"], 10, Some(35));
    assert_eq!(ctx.db.with_snapshot(|s| s.corp_projects[&auto_pid].current_quantity), 0);
    record_server_project_activity(&ctx, pm_pilot, &["MineMaterials"], 10, Some(34));
    record_server_project_activity(&ctx, pm_pilot, &["MineMaterials"], 10, Some(34));
    assert_eq!(ctx.db.with_snapshot(|s| s.corp_projects[&auto_pid].current_quantity), 3);

    // Delete auto_pid (refunds remaining 35,000 ISK) and verify second delete returns None
    assert_eq!(
        close_or_delete_canonical_project(&ctx.db, auto_pid, true, now),
        Some((corp_id, IskCents::from_isk(35_000)))
    );
    assert_eq!(close_or_delete_canonical_project(&ctx.db, auto_pid, true, now), None);

    ctx.db.with_mut_snapshot(|snap| {
        assert_eq!(snap.alliance_war_state.project_details[&done_pid].status, "Completed");
        assert!(record_server_project_activity_in_snap(snap, ceo_id, &["Any"], 0, None, now - 5).is_empty());
        assert!(record_server_project_activity_in_snap(snap, 0, &["Any"], 5, None, now - 5).is_empty());
        assert!(record_server_project_activity_in_snap(snap, outsider, &["Any"], 5, None, now - 5).is_empty());
        assert!(record_server_project_activity_in_snap(snap, ceo_id, &["Manual"], 5, None, now - 5).is_empty());
        snap.alliance_war_state.project_details.get_mut(&manual_pid).unwrap().uuid_bytes.clear();
    });

    emit_expired_quasar_notices(&ctx.db, None, &[(manual_pid, corp_id)], now);
    emit_expired_quasar_notices(&ctx.db, Some(&ctx.quasar_gateway), &[], now);
    emit_expired_quasar_notices(&ctx.db, Some(&ctx.quasar_gateway), &[(manual_pid, 0)], now);
    sweep_expired_corp_projects(&ctx);
    assert_eq!(
        ctx.db.with_snapshot(|s| s.alliance_war_state.project_details[&manual_pid].status.clone()),
        "Expired"
    );

    // 2. handle_edit_project: anon error, ProjectNotFound, AccessDenied, CorpRoleDenied, PM edit, GM edit
    let mut anon = SessionState::default();
    assert!(handle_edit_project(&ctx, &mut anon, &[PyRep::Integer(done_pid)]).unwrap().is_err());
    assert_eq!(
        handle_edit_project(&ctx, &mut sess, &[PyRep::Integer(999_999)]).unwrap().unwrap_err().code,
        "ProjectNotFound"
    );
    sess.corporation_id = Some(98_999_888);
    assert_eq!(
        handle_edit_project(&ctx, &mut sess, &[PyRep::Integer(done_pid)]).unwrap().unwrap_err().code,
        "AccessDenied"
    );

    sess.corporation_id = Some(corp_id);
    sess.character_id = Some(pm_pilot);
    sess.corp_role = 0;
    let pm_edit = handle_edit_project(
        &ctx,
        &mut sess,
        &[PyRep::Integer(done_pid), PyRep::None, PyRep::Integer((now + 900) as i64), PyRep::Integer(15)],
    )
    .unwrap()
    .unwrap();
    assert_eq!(pm_edit.dict_get("updated"), Some(&PyRep::Bool(true)));

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&outsider).unwrap().corporation_id = corp_id;
    });
    sess.character_id = Some(outsider);
    assert_eq!(
        handle_edit_project(&ctx, &mut sess, &[PyRep::Integer(done_pid)]).unwrap().unwrap_err().code,
        "CorpRoleDenied"
    );
    sess.role = eve_machonet::service_roles::ROLE_GM_MASK;
    assert!(handle_edit_project(&ctx, &mut sess, &[PyRep::Integer(done_pid), PyRep::String("GMEdited".into())])
        .unwrap()
        .is_ok());
}

