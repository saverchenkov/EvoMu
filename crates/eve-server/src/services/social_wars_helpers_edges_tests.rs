#![forbid(unsafe_code)]

use super::*;
use crate::router::context::BoundObjectEntry;
use crate::services::account_services::account_services_tests::test_ctx;
use crate::services::station_structure::structure_lifecycle::{structure_profile, HullClass};
use eve_db::{
    clock::VirtualServerClock, CorporationRecord, PersistedWarMeta, PersistedWarNegotiation,
    ServerClock, UpwellStructureRecord, WarRecord,
};
use std::sync::Arc;

fn find_upwell_types(ctx: &ServerContext) -> (u32, u32) {
    let mut normal = 35_832_u32;
    let mut flex = 35_841_u32;
    for &tid in ctx.sde.types.keys() {
        if let Some(p) = structure_profile(ctx, tid) {
            if p.hull_class == HullClass::Flex {
                flex = tid;
            } else {
                normal = tid;
            }
        }
    }
    (normal, flex)
}

#[test]
fn wars_helpers_hq_alliance_participation_and_wallet_branches() {
    let ctx = test_ctx();
    let (normal_tid, flex_tid) = find_upwell_types(&ctx);
    let highsec_sys = 30_000_142_u32; // Jita (0.9)
    let low_null_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.solar_system_id < 31_000_000 && s.security_status < 0.45)
        .map(|s| s.solar_system_id)
        .unwrap_or(30_000_208);

    // 1. err helper
    let e = err("TestWarError", "msg").unwrap().unwrap_err();
    assert_eq!(e.code, "TestWarError");
    assert_eq!(e.message, "msg");

    // 2. is_valid_war_hq branches
    let empty_corps = std::collections::HashMap::new();
    let base_hq = UpwellStructureRecord {
        structure_id: 1_880_010_001,
        owner_corp_id: 98_000_001,
        solar_system_id: highsec_sys,
        type_id: normal_tid,
        state: "ShieldVulnerable".into(),
        quantum_core_installed: true,
        ..Default::default()
    };
    assert!(is_valid_war_hq(&ctx, &empty_corps, 98_000_001, &base_hq));
    assert!(!is_valid_war_hq(&ctx, &empty_corps, 98_000_002, &base_hq)); // wrong owner

    let mut no_core = base_hq.clone();
    no_core.quantum_core_installed = false;
    assert!(!is_valid_war_hq(&ctx, &empty_corps, 98_000_001, &no_core));

    let mut bad_state = base_hq.clone();
    bad_state.state = "Anchoring".into();
    assert!(!is_valid_war_hq(&ctx, &empty_corps, 98_000_001, &bad_state));

    for st in [
        "ArmorReinforced",
        "ArmorVulnerable",
        "HullReinforced",
        "HullVulnerable",
    ] {
        let mut s = base_hq.clone();
        s.state = st.into();
        assert!(is_valid_war_hq(&ctx, &empty_corps, 98_000_001, &s));
    }

    let mut wh_hq = base_hq.clone();
    wh_hq.solar_system_id = 31_000_001;
    assert!(!is_valid_war_hq(&ctx, &empty_corps, 98_000_001, &wh_hq));

    let mut low_hq = base_hq.clone();
    low_hq.solar_system_id = low_null_sys;
    assert!(!is_valid_war_hq(&ctx, &empty_corps, 98_000_001, &low_hq));

    let mut flex_hq = base_hq.clone();
    flex_hq.type_id = flex_tid;
    assert!(!is_valid_war_hq(&ctx, &empty_corps, 98_000_001, &flex_hq));

    // 3. same_alliance, already_at_war, is_war_participant_corp, war_eligible, debit/credit_corp
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            98_100_001,
            { let mut __corp = CorporationRecord::default(); __corp.corporation_id = 98_100_001; __corp.alliance_id = Some(99_100_001); __corp.ensure_wallet_divisions(); __corp },
        );
        snap.corporations.insert(
            98_100_002,
            { let mut __corp = CorporationRecord::default(); __corp.corporation_id = 98_100_002; __corp.alliance_id = Some(99_100_001); __corp.ensure_wallet_divisions(); __corp },
        );
        snap.corporations.insert(
            98_100_003,
            { let mut __corp = CorporationRecord::default(); __corp.corporation_id = 98_100_003; __corp.alliance_id = Some(99_100_002); __corp.ensure_wallet_divisions(); __corp },
        );
        snap.corporations.insert(
            98_100_004,
            { let mut __corp = CorporationRecord::default(); __corp.corporation_id = 98_100_004; __corp.alliance_id = None; __corp.ensure_wallet_divisions(); __corp },
        );
        snap.corporations.insert(
            98_100_005,
            { let mut __corp = CorporationRecord::default(); __corp.corporation_id = 98_100_005; __corp.alliance_id = Some(0); __corp.ensure_wallet_divisions(); __corp },
        );

        assert!(same_alliance(snap, 98_100_001, 98_100_002));
        assert!(!same_alliance(snap, 98_100_001, 98_100_003));
        assert!(!same_alliance(snap, 98_100_004, 98_100_005));

        snap.wars.insert(
            10,
            WarRecord {
                war_id: 10,
                aggressor_corp_id: 98_100_001,
                defender_corp_id: 98_100_003,
                started_unix: 1_000,
                mutual: false,
                active: true,
            },
        );
        snap.wars.insert(
            11,
            WarRecord {
                war_id: 11,
                aggressor_corp_id: 98_100_004,
                defender_corp_id: 98_100_005,
                started_unix: 1_000,
                mutual: true,
                active: false,
            },
        );
        snap.war_state.negotiations.push(PersistedWarNegotiation {
            id: 1,
            war_id: 10,
            kind: NegKind::Ally,
            from_corp: 98_100_004,
            isk: IskCents::from_isk(1_000),
            message: "help".into(),
            status: "Accepted".into(),
            effective_unix: None,
        });

        assert!(already_at_war(snap, 98_100_001, 98_100_003));
        assert!(already_at_war(snap, 98_100_003, 98_100_001));
        assert!(!already_at_war(snap, 98_100_004, 98_100_005)); // inactive

        assert!(!is_war_participant_corp(snap, 999, 98_100_001, 2_000_000_000));
        assert!(is_war_participant_corp(snap, 10, 98_100_001, 2_000_000_000));
        assert!(is_war_participant_corp(snap, 10, 98_100_003, 2_000_000_000));
        assert!(is_war_participant_corp(snap, 10, 98_100_002, 2_000_000_000)); // same alliance as aggressor
        assert!(is_war_participant_corp(snap, 10, 98_100_004, 2_000_000_000)); // accepted ally
        assert!(!is_war_participant_corp(snap, 10, 98_100_005, 2_000_000_000));

        assert!(!war_eligible(snap, 98_100_003));
        snap.structures.insert(
            1_880_010_002,
            UpwellStructureRecord {
                structure_id: 1_880_010_002,
                owner_corp_id: 98_100_003,
                state: "Destroyed".into(),
                ..Default::default()
            },
        );
        assert!(!war_eligible(snap, 98_100_003));
        snap.structures.get_mut(&1_880_010_002).unwrap().state = "ShieldVulnerable".into();
        assert!(war_eligible(snap, 98_100_003));

        // debit_corp & credit_corp
        assert_eq!(
            debit_corp(snap, 999_999, IskCents::from_isk(10)).unwrap_err().code,
            "CorporationNotFound"
        );
        assert_eq!(
            debit_corp(snap, 98_100_001, IskCents::from_isk(500)).unwrap_err().code,
            "InsufficientFunds"
        );
        credit_corp(snap, 98_100_001, IskCents::from_isk(1_000));
        credit_corp(snap, 999_999, IskCents::from_isk(1_000)); // no-op
        debit_corp(snap, 98_100_001, IskCents::from_isk(400)).unwrap();
        assert_eq!(
            snap.corporations[&98_100_001].division_balance(1),
            IskCents::from_isk(600)
        );
    });
}

#[test]
fn wars_helpers_sync_notifications_negotiations_and_stat_id() {
    let ctx = test_ctx();
    let clock = Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000));
    ctx.set_clock(clock.clone());
    let (normal_tid, _) = find_upwell_types(&ctx);
    let highsec_sys = 30_000_142_u32;

    ctx.db.with_mut_snapshot(|snap| {
        for cid in [98_200_001, 98_200_002, 98_200_003, 98_200_004] {
            snap.corporations.insert(
                cid,
                { let mut __corp = CorporationRecord::default(); __corp.corporation_id = cid; __corp.ensure_wallet_divisions(); __corp },
            );
        }
        // Valid HQ for agg1 and defender structure for def1
        snap.structures.insert(
            1_880_020_001,
            UpwellStructureRecord {
                structure_id: 1_880_020_001,
                owner_corp_id: 98_200_001,
                solar_system_id: highsec_sys,
                type_id: normal_tid,
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                ..Default::default()
            },
        );
        snap.structures.insert(
            1_880_020_002,
            UpwellStructureRecord {
                structure_id: 1_880_020_002,
                owner_corp_id: 98_200_002,
                solar_system_id: highsec_sys,
                type_id: normal_tid,
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                ..Default::default()
            },
        );

        // War 101: active, valid HQ, eligible defender, not finished -> stays active
        snap.wars.insert(
            101,
            WarRecord {
                war_id: 101,
                aggressor_corp_id: 98_200_001,
                defender_corp_id: 98_200_002,
                started_unix: 1_800_000_000 - WAR_TIMER_SECS - 10,
                mutual: false,
                active: true,
            },
        );
        snap.war_state.wars.insert(
            101,
            PersistedWarMeta {
                war_hq_id: Some(1_880_020_001),
                open_for_allies: true,
                finish_at_unix: Some(1_800_100_000),
            },
        );

        // Negotiations for war 101
        snap.war_state.negotiations.push(PersistedWarNegotiation {
            id: 10,
            war_id: 101,
            kind: NegKind::Surrender,
            from_corp: 98_200_002,
            isk: IskCents::from_isk(5_000),
            message: "peace".into(),
            status: "Pending".into(),
            effective_unix: Some(1_800_050_000),
        });
        snap.war_state.negotiations.push(PersistedWarNegotiation {
            id: 11,
            war_id: 101,
            kind: NegKind::Ally,
            from_corp: 98_200_003,
            isk: IskCents::from_isk(2_000),
            message: "ally offer".into(),
            status: "Pending".into(),
            effective_unix: None,
        });
        snap.war_state.negotiations.push(PersistedWarNegotiation {
            id: 12,
            war_id: 101,
            kind: NegKind::Ally,
            from_corp: 98_200_004,
            isk: IskCents::from_isk(3_000),
            message: "other ally".into(),
            status: "Pending".into(),
            effective_unix: None,
        });
    });

    // 1. sync_wars early return when no war needs ending
    sync_wars(&ctx);
    ctx.db.with_snapshot(|snap| assert!(snap.wars[&101].active));

    // 2. neg_rows_for_caller branches
    assert_eq!(
        neg_rows_for_caller(&ctx, 101, NegKind::Surrender, 98_200_003).unwrap_err().code,
        "NotWarParticipant"
    );
    assert_eq!(
        neg_rows_for_caller(&ctx, 101, NegKind::Surrender, 98_200_001).unwrap().len(),
        1
    );
    // Non-participant sees only their own Ally offer
    assert_eq!(
        neg_rows_for_caller(&ctx, 101, NegKind::Ally, 98_200_003).unwrap().len(),
        1
    );
    // Participant sees all Ally offers
    assert_eq!(
        neg_rows_for_caller(&ctx, 101, NegKind::Ally, 98_200_001).unwrap().len(),
        2
    );

    // 3. emit_mutual_wars_updated & emit_war_changed
    let uid = ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&90_000_001).unwrap();
        ch.corporation_id = 98_200_001;
        ch.user_id
    });
    let _ = ctx.db.acquire_online_lock(uid, 1, Some(90_000_001), true, ctx.now_unix());
    emit_mutual_wars_updated(&ctx, 90_000_001, 98_200_001, 98_200_002);
    emit_mutual_wars_updated(&ctx, 90_000_001, 98_200_001, 98_200_001);
    emit_war_changed(&ctx, 999_999, vec![]); // missing war -> no-op
    emit_war_changed(&ctx, 101, vec![("mutual", PyRep::Bool(false), PyRep::Bool(true))]);
    {
        let pending = ctx.pending_notifications.read().unwrap();
        let list = pending.get(&90_000_001).unwrap();
        assert!(list.iter().any(|n| n.event == "OnMutualWarsUpdated_Remote"));
        assert!(list.iter().any(|n| n.event == "OnWarChanged"));
    }

    // 4. sync_wars ends war when finish_at_unix reached, HQ lost, or defender ineligible
    ctx.db.with_mut_snapshot(|snap| {
        // War 102: HQ invalid (missing)
        snap.wars.insert(
            102,
            WarRecord {
                war_id: 102,
                aggressor_corp_id: 98_200_001,
                defender_corp_id: 98_200_002,
                started_unix: 1_800_000_000,
                mutual: false,
                active: true,
            },
        );
        snap.war_state.wars.insert(
            102,
            PersistedWarMeta {
                war_hq_id: Some(9_999_999),
                open_for_allies: false,
                finish_at_unix: None,
            },
        );
        // War 103: non-mutual, defender 98_200_003 has no structures -> ineligible
        snap.wars.insert(
            103,
            WarRecord {
                war_id: 103,
                aggressor_corp_id: 98_200_001,
                defender_corp_id: 98_200_003,
                started_unix: 1_800_000_000,
                mutual: false,
                active: true,
            },
        );
    });
    clock.advance_secs(200_000); // passes 1_800_100_000 for war 101
    sync_wars(&ctx);
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.wars[&101].active);
        assert!(!snap.wars[&102].active);
        assert!(snap.war_state.wars[&102].finish_at_unix.is_some());
        assert!(!snap.wars[&103].active);
    });

    // 5. resolve_war_stat_id fallback chain: arg0 -> bound_obj_id -> ctx.bound_objects -> snap.wars -> 0
    let sess = SessionState {
        session_id: 55_001,
        character_id: Some(90_000_001),
        ..Default::default()
    };
    assert_eq!(resolve_war_stat_id(&ctx, &sess, Some(101), Some(102), 98_200_001), 101);
    assert_eq!(resolve_war_stat_id(&ctx, &sess, Some(0), Some(102), 98_200_001), 102);
    ctx.bound_objects.write().unwrap().insert(
        10,
        BoundObjectEntry {
            bound_id: 10,
            session_id: 55_001,
            user_id: 1,
            character_id: 90_000_001,
            service: "warStatisticMgr".into(),
            object_id: 777,
            oid_str: "warStatisticMgr:777".into(),
            ref_id: 1,
        },
    );
    assert_eq!(resolve_war_stat_id(&ctx, &sess, None, None, 98_200_001), 777);
    ctx.bound_objects.write().unwrap().clear();
    assert!(resolve_war_stat_id(&ctx, &sess, None, None, 98_200_001) > 0);
    assert_eq!(resolve_war_stat_id(&ctx, &sess, None, None, 99_999_999), 0);
}

#[test]
fn wars_helpers_allied_hq_ally_effective_time_sync_invalid_hq_and_crowset_rows() {
    let ctx = test_ctx();
    let clock = Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000));
    ctx.set_clock(clock.clone());
    let (normal_tid, _) = find_upwell_types(&ctx);
    let highsec_sys = 30_000_142_u32;

    let ally_id = 99_300_001_i64;
    let member_corp = 98_300_001_i64;
    let defender_corp = 98_300_002_i64;
    let helper_corp = 98_300_003_i64;
    let outsider_corp = 98_300_004_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(member_corp, {
            let mut c = CorporationRecord::default();
            c.corporation_id = member_corp;
            c.alliance_id = Some(ally_id);
            c.ensure_wallet_divisions();
            c
        });
        for cid in [defender_corp, helper_corp, outsider_corp] {
            snap.corporations.insert(cid, {
                let mut c = CorporationRecord::default();
                c.corporation_id = cid;
                c.ensure_wallet_divisions();
                c
            });
        }

        let valid_member_hq = UpwellStructureRecord {
            structure_id: 1_880_030_001,
            owner_corp_id: member_corp,
            solar_system_id: highsec_sys,
            type_id: normal_tid,
            state: "ShieldVulnerable".into(),
            quantum_core_installed: true,
            ..Default::default()
        };
        let mut nocore_member_hq = valid_member_hq.clone();
        nocore_member_hq.structure_id = 1_880_030_002;
        nocore_member_hq.quantum_core_installed = false;

        let mut destroyed_struct = valid_member_hq.clone();
        destroyed_struct.structure_id = 1_880_030_003;
        destroyed_struct.owner_corp_id = outsider_corp;
        destroyed_struct.state = "Destroyed".into();

        // 1. is_valid_war_hq for allied owner vs un-cored allied owner
        assert!(is_valid_war_hq(&ctx, &snap.corporations, ally_id, &valid_member_hq));
        assert!(!is_valid_war_hq(&ctx, &snap.corporations, ally_id, &nocore_member_hq));
        assert!(!is_valid_war_hq(&ctx, &snap.corporations, defender_corp, &valid_member_hq));

        snap.structures.insert(valid_member_hq.structure_id, valid_member_hq);
        snap.structures.insert(nocore_member_hq.structure_id, nocore_member_hq);
        snap.structures.insert(destroyed_struct.structure_id, destroyed_struct);

        // 2. war_eligible: Destroyed structure only -> false; active structure in member corp -> true for alliance
        assert!(!war_eligible(snap, outsider_corp));
        assert!(war_eligible(snap, ally_id));
        assert!(war_eligible(snap, member_corp));

        // 3. War 201 (valid HQ stays active) + War 202 (existing un-cored HQ must end via !is_valid_war_hq)
        snap.wars.insert(
            201,
            WarRecord {
                war_id: 201,
                aggressor_corp_id: ally_id,
                defender_corp_id: defender_corp,
                started_unix: 1_800_000_000,
                mutual: true,
                active: true,
            },
        );
        snap.war_state.wars.insert(
            201,
            PersistedWarMeta {
                war_hq_id: Some(1_880_030_001),
                open_for_allies: true,
                finish_at_unix: None,
            },
        );
        snap.wars.insert(
            202,
            WarRecord {
                war_id: 202,
                aggressor_corp_id: ally_id,
                defender_corp_id: defender_corp,
                started_unix: 1_800_000_000,
                mutual: true,
                active: true,
            },
        );
        snap.war_state.wars.insert(
            202,
            PersistedWarMeta {
                war_hq_id: Some(1_880_030_002),
                open_for_allies: false,
                finish_at_unix: None,
            },
        );

        // Accepted Ally negotiation effective at 1_800_001_000
        snap.war_state.negotiations.push(PersistedWarNegotiation {
            id: 555,
            war_id: 201,
            kind: NegKind::Ally,
            from_corp: helper_corp,
            isk: IskCents::from_isk(1_234),
            message: "join".into(),
            status: "Accepted".into(),
            effective_unix: Some(1_800_001_000),
        });

        // 4. is_war_participant_corp boundary at effective_unix
        assert!(!is_war_participant_corp(snap, 201, helper_corp, 1_800_000_999));
        assert!(is_war_participant_corp(snap, 201, helper_corp, 1_800_001_000));
        assert!(!is_war_participant_corp(snap, 201, outsider_corp, 1_800_001_000));
    });

    // 5. sync_wars deactivates ONLY war 202 (existing invalid HQ), keeps war 201 (valid HQ) active
    sync_wars(&ctx);
    ctx.db.with_snapshot(|snap| {
        assert!(snap.wars[&201].active);
        assert!(!snap.wars[&202].active);
    });

    // 6. neg_rows_for_caller exact cell values & war_crowset non-default output
    let rows = neg_rows_for_caller(&ctx, 201, NegKind::Ally, helper_corp).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].len(), 7);
    assert_eq!(rows[0][0], PyRep::Integer(555));
    assert_eq!(rows[0][1], PyRep::Integer(201));
    assert_eq!(rows[0][2], PyRep::Integer(helper_corp));
    assert_eq!(rows[0][5], PyRep::String("Accepted".into()));

    let crow = war_crowset(vec![vec![
        PyRep::Integer(9001),
        PyRep::Integer(ally_id),
        PyRep::Integer(defender_corp),
        PyRep::Integer(1_800_000_000),
        PyRep::Integer(1_800_000_000),
        PyRep::Integer(0),
        PyRep::Bool(true),
        PyRep::Bool(true),
        PyRep::Integer(1_880_030_001),
        PyRep::Bool(true),
        PyRep::Bool(true),
    ]]);
    assert_ne!(crow, PyRep::None);
    assert!(!format!("{crow:?}").is_empty());
}
