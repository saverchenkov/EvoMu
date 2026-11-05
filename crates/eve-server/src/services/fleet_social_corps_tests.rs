//! Unit tests for `fleet_social_corps` public info, updates, divisions, members, and cash transfers.

#[path = "fleet_social_corps_apps_tests.rs"]
mod fleet_social_corps_apps_tests;

use super::*;
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::{
    clone_char_with, test_ctx_with_clock,
};
use eve_db::{corp_roles, CorpWalletDivisionRecord, CorporationRecord, IskCents};
use eve_machonet::SessionState;
use std::collections::{HashMap, HashSet};

pub(crate) fn seed_player_corps(ctx: &ServerContext) -> (SessionState, SessionState) {
    ctx.db.with_mut_snapshot(|snap| {
        let mut c1 = clone_char_with(snap, 90_000_001, "CEO Prime", 98_000_001, 30_000_142, 100_000_001);
        c1.station_id = Some(60_003_760);
        c1.create_date_unix = 1_700_000_000;
        let mut c2 = clone_char_with(snap, 90_000_002, "Line Member", 98_000_001, 30_000_142, 100_000_002);
        c2.station_id = Some(60_003_760);
        c2.create_date_unix = 1_700_000_000;
        let mut c3 = clone_char_with(snap, 90_000_003, "Applicant Pilot", 98_000_002, 30_000_142, 100_000_003);
        c3.station_id = Some(60_003_760);
        c3.create_date_unix = 1_700_000_000;
        snap.characters.insert(90_000_001, c1);
        snap.characters.insert(90_000_002, c2);
        snap.characters.insert(90_000_003, c3);

        let mut corp1 = CorporationRecord {
            corporation_id: 98_000_001,
            name: "Alpha Holdings".into(),
            ticker: "ALPH".into(),
            ceo_id: 90_000_001,
            alliance_id: Some(99_000_001),
            tax_rate: 0.1,
            master_wallet: IskCents::from_isk(50_000_000),
            members: HashSet::from([90_000_001, 90_000_002]),
            member_roles: HashMap::from([
                (90_000_001, corp_roles::ROLE_DIRECTOR),
                (90_000_002, 0),
            ]),
            hq_station_id: Some(60_003_760),
            ..Default::default()
        };
        corp1.wallet_divisions = CorporationRecord::default_wallet_divisions(corp1.master_wallet);
        corp1.wallet_divisions.push(CorpWalletDivisionRecord {
            division_id: 7,
            name: "Reserve".into(),
            balance: IskCents::from_isk(1_000_000),
        });
        snap.corporations.insert(98_000_001, corp1);

        let mut corp2 = CorporationRecord {
            corporation_id: 98_000_002,
            name: "Beta Logistics".into(),
            ticker: "BETA".into(),
            ceo_id: 90_000_004,
            tax_rate: 0.05,
            master_wallet: IskCents::from_isk(20_000_000),
            members: HashSet::from([90_000_003, 90_000_004]),
            hq_station_id: Some(60_003_760),
            ..Default::default()
        };
        corp2.ensure_wallet_divisions();
        snap.corporations.insert(98_000_002, corp2);
    });

    let mut ceo_sess = SessionState::new(1);
    ceo_sess.user_id = Some(90_000_001);
    ceo_sess.character_id = Some(90_000_001);
    ceo_sess.corporation_id = Some(98_000_001);
    ceo_sess.corp_role = corp_roles::ROLE_DIRECTOR;
    ceo_sess.station_id = Some(60_003_760);

    let mut mem_sess = SessionState::new(2);
    mem_sess.user_id = Some(90_000_002);
    mem_sess.character_id = Some(90_000_002);
    mem_sess.corporation_id = Some(98_000_001);
    mem_sess.station_id = Some(60_003_760);

    (ceo_sess, mem_sess)
}

#[test]
fn corp_public_info_update_divisions_members_and_medals() {
    let ctx = test_ctx_with_clock(1_800_010_000);
    let (mut ceo_sess, mut mem_sess) = seed_player_corps(&ctx);
    let kw = HashMap::new();

    // GetPublicInfo / GetCorporation + NPC fallback (1_000_035 Caldari Navy)
    let PyRep::KeyVal(info) = handle_corps(&ctx, &mut ceo_sess, "corpRegistry", "GetCorporation", &[], &kw)
        .unwrap()
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(info.get("ticker").unwrap().as_str().unwrap(), "ALPH");
    assert!(handle_corps(
        &ctx,
        &mut ceo_sess,
        "corpmgr",
        "GetPublicInfo",
        &[PyRep::Integer(1_000_035)],
        &kw,
    )
    .unwrap()
    .is_ok());
    assert_eq!(
        handle_corps(&ctx, &mut ceo_sess, "corpmgr", "ceoID", &[], &kw)
            .unwrap()
            .unwrap()
            .as_i64()
            .unwrap(),
        90_000_001
    );
    assert_eq!(
        handle_corps(
            &ctx,
            &mut ceo_sess,
            "corpmgr",
            "GetCorporationIDForCharacter",
            &[PyRep::Integer(90_000_002)],
            &kw,
        )
        .unwrap()
        .unwrap()
        .as_i64()
        .unwrap(),
        98_000_001
    );

    // UpdateCorporation + UpdateLogo (CEO succeeds, Line Member denied)
    assert_eq!(
        handle_corps(
            &ctx,
            &mut mem_sess,
            "corpRegistry",
            "UpdateCorporation",
            &[PyRep::String("Nope".into())],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );
    let upd = handle_corps(
        &ctx,
        &mut ceo_sess,
        "corpRegistry",
        "UpdateCorporation",
        &[
            PyRep::String("Nullsec Alliance Executor".into()),
            PyRep::String("https://alphaholdings.example".into()),
            PyRep::Real(0.15),
            PyRep::Bool(false),
        ],
        &kw,
    )
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(u_kv) = upd else { panic!() };
    assert!((u_kv.get("taxRate").unwrap().as_f64().unwrap() - 0.15).abs() < 1e-6);

    let logo = handle_corps(
        &ctx,
        &mut ceo_sess,
        "corpRegistry",
        "UpdateLogo",
        &[
            PyRep::Integer(11),
            PyRep::Integer(22),
            PyRep::Integer(33),
            PyRep::Integer(44),
            PyRep::Integer(55),
            PyRep::Integer(66),
        ],
        &kw,
    )
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(l_kv) = logo else { panic!() };
    assert_eq!(l_kv.get("shape1").unwrap().as_i64().unwrap(), 11);

    // UpdateDivisionNames: single division ID (e.g., 1002 -> div 3) and 14-arg bulk update
    let div_single = handle_corps(
        &ctx,
        &mut ceo_sess,
        "corpRegistry",
        "UpdateDivisionNames",
        &[
            PyRep::Integer(1002),
            PyRep::String("Capital Hangar".into()),
            PyRep::String("Capital Wallet".into()),
        ],
        &kw,
    )
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(ds_kv) = div_single else { panic!() };
    assert_eq!(ds_kv.get("divisionID").unwrap().as_i64().unwrap(), 3);

    let bulk_args: Vec<PyRep> = (1..=7)
        .map(|i| PyRep::String(format!("Hangar {i}")))
        .chain((1..=7).map(|i| PyRep::String(format!("Wallet {i}"))))
        .collect();
    let div_bulk = handle_corps(
        &ctx,
        &mut ceo_sess,
        "corpmgr",
        "UpdateDivisionNames",
        &bulk_args,
        &kw,
    )
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(db_kv) = div_bulk else { panic!() };
    assert_eq!(db_kv.get("name").unwrap().as_str().unwrap(), "Hangar 1");

    // GetMembers / GetMembersPaged / GetMembersByIds / GetMemberIDsByQuery / AuditMember
    assert!(handle_corps(&ctx, &mut ceo_sess, "corpRegistry", "GetMembers", &[], &kw)
        .unwrap()
        .is_ok());
    assert!(handle_corps(
        &ctx,
        &mut ceo_sess,
        "corpRegistry",
        "GetMembersPaged",
        &[PyRep::Integer(1), PyRep::Integer(1)],
        &kw,
    )
    .unwrap()
    .is_ok());
    assert!(handle_corps(
        &ctx,
        &mut ceo_sess,
        "corpRegistry",
        "GetMembersByIds",
        &[PyRep::Integer(98_000_001), PyRep::List(vec![PyRep::Integer(90_000_001)])],
        &kw,
    )
    .unwrap()
    .is_ok());
    assert!(handle_corps(
        &ctx,
        &mut ceo_sess,
        "corpRegistry",
        "GetMemberIDsByQuery",
        &[PyRep::String("line".into())],
        &kw,
    )
    .unwrap()
    .is_ok());
    assert!(handle_corps(&ctx, &mut ceo_sess, "corpmgr", "AuditMember", &[], &kw)
        .unwrap()
        .is_ok());

    // promote_due_pending_corp_grants + get_character_medals_and_decorations helpers
    with_corp_gov(&ctx, |g| {
        let st = g.entry(98_000_001).or_default();
        st.pending_role_grants
            .insert(90_000_002, (corp_roles::ROLE_PERSONNEL_MANAGER, 1_800_009_999));
        st.pending_friendly_fire = Some((true, 1_800_009_999));
        st.medals.push((1, "Cross of Honor".into(), "Service".into(), 90_000_001, 1_800_010_000));
        st.medal_recipients.push((1, 90_000_002, 90_000_001, "Valor".into(), 2));
    });
    promote_due_pending_corp_grants(&ctx, 98_000_001);
    ctx.db.with_snapshot(|snap| {
        let role = snap.corporations.get(&98_000_001).unwrap().member_roles.get(&90_000_002).copied().unwrap_or(0);
        assert!(corp_roles::has_role(role, corp_roles::ROLE_PERSONNEL_MANAGER));
        let (medals, decors) = get_character_medals_and_decorations(snap, 90_000_002);
        assert_eq!(medals.len(), 1);
        assert_eq!(decors.len(), 1);
    });
}
