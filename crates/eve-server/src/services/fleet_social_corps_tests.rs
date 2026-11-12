#![allow(unused_imports)]
//! Unit tests for `fleet_social_corps` public info, updates, divisions, members, and cash transfers.

#[cfg(test)]
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

        let mut corp1 = { let mut __corp = CorporationRecord::default();
            __corp.corporation_id = 98_000_001;
            __corp.name = "Alpha Holdings".into();
            __corp.ticker = "ALPH".into();
            __corp.ceo_id = 90_000_001;
            __corp.alliance_id = Some(99_000_001);
            __corp.tax_rate = 0.1;
            __corp.members = HashSet::from([90_000_001, 90_000_002]);
            __corp.member_roles = HashMap::from([
                (90_000_001, corp_roles::ROLE_DIRECTOR),
                (90_000_002, 0),
            ]);
            __corp.hq_station_id = Some(60_003_760);
            __corp.set_division_balance(1, IskCents::from_isk(50_000_000));
            __corp };
        corp1.wallet_divisions = CorporationRecord::default_wallet_divisions(corp1.master_wallet_balance());
        if let Some(div7) = corp1.wallet_divisions.iter_mut().find(|d| d.division_id == 7) {
            div7.name = "Reserve".into();
            div7.balance = IskCents::from_isk(1_000_000);
        }
        snap.corporations.insert(98_000_001, corp1);

        let mut corp2 = { let mut __corp = CorporationRecord::default();
            __corp.corporation_id = 98_000_002;
            __corp.name = "Beta Logistics".into();
            __corp.ticker = "BETA".into();
            __corp.ceo_id = 90_000_004;
            __corp.tax_rate = 0.05;
            __corp.members = HashSet::from([90_000_003, 90_000_004]);
            __corp.hq_station_id = Some(60_003_760);
            __corp.set_division_balance(1, IskCents::from_isk(20_000_000));
            __corp };
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
    assert_eq!(info.get("creatorID").and_then(|v| v.as_i64()), Some(90_000_001));
    assert_eq!(info.get("loyaltyPointTaxRate").and_then(|v| v.as_f64()), Some(0.1));
    assert_eq!(info.get("iskTaxRate").and_then(|v| v.as_f64()), Some(0.1));
    assert_eq!(info.get("friendlyFire").and_then(|v| v.as_bool()), Some(false));
    let PyRep::KeyVal(npc_info) = handle_corps(
        &ctx,
        &mut ceo_sess,
        "corpmgr",
        "GetPublicInfo",
        &[PyRep::Integer(1_000_035)],
        &kw,
    )
    .unwrap()
    .unwrap()
    else {
        panic!("expected KeyVal for NPC corp")
    };
    assert_eq!(npc_info.get("creatorID").and_then(|v| v.as_i64()), Some(3_004_069));
    assert!(npc_info.get("loyaltyPointTaxRate").and_then(|v| v.as_f64()).is_some());
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
    let PyRep::Rowset { columns: mem_cols, .. } = handle_corps(&ctx, &mut ceo_sess, "corpRegistry", "GetMembers", &[], &kw)
        .unwrap()
        .unwrap()
    else {
        panic!("expected Rowset from GetMembers")
    };
    assert!(mem_cols.contains(&"roles".to_string()));
    assert!(mem_cols.contains(&"grantableRoles".to_string()));
    assert!(mem_cols.contains(&"titleMask".to_string()));
    assert!(mem_cols.contains(&"startDateTime".to_string()));
    assert!(mem_cols.contains(&"blockRoles".to_string()));

    let paged_res = handle_corps(
        &ctx,
        &mut ceo_sess,
        "corpRegistry",
        "GetMembersPaged",
        &[PyRep::Integer(1), PyRep::Integer(1)],
        &kw,
    )
    .unwrap()
    .unwrap();
    let PyRep::ObjectEx { is_type_2: false, header: paged_hdr, .. } = paged_res else {
        panic!("expected PagedResultSet ObjectEx from GetMembersPaged");
    };
    let PyRep::Tuple(hdr_parts) = *paged_hdr else { panic!() };
    assert_eq!(hdr_parts[0], PyRep::Token("eve.common.script.util.pagedCollection.PagedResultSet".into()));
    let PyRep::Tuple(ctor_args) = &hdr_parts[1] else { panic!() };
    assert!(matches!(&ctor_args[0], PyRep::ObjectEx { is_type_2: true, list, .. } if list.len() == 1));
    assert_eq!(ctor_args[1].as_i64(), Some(2));
    assert_eq!(ctor_args[2].as_i64(), Some(0));
    assert_eq!(ctor_args[3].as_i64(), Some(1));

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
    let str_query = handle_corps(
        &ctx,
        &mut ceo_sess,
        "corpRegistry",
        "GetMemberIDsByQuery",
        &[PyRep::String("line".into())],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(str_query, PyRep::List(vec![PyRep::Integer(90_000_002)]));

    // Structured V24 GetMemberIDsByQuery: [property, op, val] with roles & characterID
    let structured = handle_corps(
        &ctx,
        &mut ceo_sess,
        "corpRegistry",
        "GetMemberIDsByQuery",
        &[PyRep::List(vec![PyRep::List(vec![
            PyRep::String("characterID".into()),
            PyRep::Integer(1),
            PyRep::Integer(90_000_001),
        ])])],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(structured, PyRep::List(vec![PyRep::Integer(90_000_001)]));

    assert!(handle_corps(&ctx, &mut ceo_sess, "corpmgr", "AuditMember", &[], &kw)
        .unwrap()
        .is_ok());

    with_corp_gov(&ctx, |g| {
        let log = &mut g.entry(98_000_001).or_default().role_audit_log;
        log.push((90_000_002, 90_000_001, 0, corp_roles::ROLE_ACCOUNTANT, 1_800_000_100));
        log.push((90_000_002, 90_000_001, corp_roles::ROLE_ACCOUNTANT, corp_roles::ROLE_TRADER, 1_800_000_500));
    });
    let audit_filtered = handle_corps(
        &ctx,
        &mut ceo_sess,
        "corpmgr",
        "AuditMember",
        &[
            PyRep::Integer(90_000_002),
            PyRep::Integer(1_800_000_200),
            PyRep::Integer(1_800_000_600),
            PyRep::Integer(1),
        ],
        &kw,
    )
    .unwrap()
    .unwrap();
    let PyRep::Rowset { rows: audit_rows, .. } = audit_filtered else { panic!() };
    assert_eq!(audit_rows.len(), 1);
    assert_eq!(audit_rows[0][4], PyRep::Integer(1_800_000_500));

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

#[test]
fn get_corporation_omits_zero_station_and_ceo_ids() {
    let ctx = test_ctx_with_clock(1_800_010_000);
    let (mut ceo_sess, _) = seed_player_corps(&ctx);
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|snap| {
        let corp = snap.corporations.get_mut(&98_000_001).unwrap();
        corp.hq_station_id = None;
        corp.alliance_id = Some(0);
    });

    let PyRep::KeyVal(info) = handle_corps(
        &ctx,
        &mut ceo_sess,
        "corpRegistry",
        "GetCorporation",
        &[PyRep::Integer(98_000_001)],
        &kw,
    )
    .unwrap()
    .unwrap()
    else {
        panic!("expected KeyVal for player corp");
    };
    assert_eq!(info.get("stationID"), Some(&PyRep::None), "missing HQ station must be None (#773)");
    assert_eq!(info.get("allianceID"), Some(&PyRep::None), "zero allianceID must be None (#773)");
    assert_eq!(info.get("ceoID"), Some(&PyRep::Integer(90_000_001)));

    let PyRep::KeyVal(npc_info) = handle_corps(
        &ctx,
        &mut ceo_sess,
        "corpmgr",
        "GetPublicInfo",
        &[PyRep::Integer(1_000_035)],
        &kw,
    )
    .unwrap()
    .unwrap()
    else {
        panic!("expected KeyVal for NPC corp");
    };
    let expected_cn_ceo = ctx
        .sde
        .npc_corporations
        .get(&1_000_035)
        .and_then(|c| c.ceo_id)
        .expect("Caldari Navy 1000035 ceo_id loaded from SDE");
    assert_eq!(expected_cn_ceo, 3_004_069);
    assert_eq!(
        npc_info.get("ceoID"),
        Some(&PyRep::Integer(expected_cn_ceo)),
        "NPC corp ceoID must be a valid SDE ceo_id ownerID, never None (#965)"
    );
    assert_eq!(
        npc_info.get("creatorID"),
        Some(&PyRep::Integer(expected_cn_ceo))
    );
    assert_eq!(
        handle_corps(
            &ctx,
            &mut ceo_sess,
            "corpmgr",
            "ceoID",
            &[PyRep::Integer(1_000_035)],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Integer(expected_cn_ceo)
    );

    // NPC corp whose SDE ceo_id is NULL (e.g. 1000173 Polaris Corporation) must fall back to corporationID
    let polaris_ceo = ctx
        .sde
        .npc_corporations
        .get(&1_000_173)
        .and_then(|c| c.ceo_id);
    assert_eq!(polaris_ceo, None);
    let PyRep::KeyVal(polaris_info) = handle_corps(
        &ctx,
        &mut ceo_sess,
        "corpmgr",
        "GetPublicInfo",
        &[PyRep::Integer(1_000_173)],
        &kw,
    )
    .unwrap()
    .unwrap()
    else {
        panic!("expected KeyVal for NPC corp 1000173");
    };
    assert_eq!(
        polaris_info.get("ceoID"),
        Some(&PyRep::Integer(1_000_173)),
        "NPC corp without SDE ceo_id must fall back to corporationID, never None (#965)"
    );
    assert_eq!(
        handle_corps(
            &ctx,
            &mut ceo_sess,
            "corpmgr",
            "ceoID",
            &[PyRep::Integer(1_000_173)],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Integer(1_000_173)
    );

    // #965 follow-up: Materialized NPC corporation in snap.corporations with ceo_id = 0 (e.g. 1000004 Ytiri)
    // must still resolve ceoID, ticker, and stationID from SDE npc_corporations and never return None!
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            1_000_004,
            CorporationRecord::new(1_000_004, "Ytiri", "NPC", 0),
        );
    });
    let expected_ytiri_ceo = ctx
        .sde
        .npc_corporations
        .get(&1_000_004)
        .and_then(|c| c.ceo_id.filter(|&id| id > 0))
        .unwrap_or(1_000_004);
    assert!(expected_ytiri_ceo > 0);
    for method in ["GetPublicInfo", "GetCorporations"] {
        let PyRep::KeyVal(ytiri_info) = handle_corps(
            &ctx,
            &mut ceo_sess,
            "corpmgr",
            method,
            &[PyRep::Integer(1_000_004)],
            &kw,
        )
        .unwrap()
        .unwrap()
        else {
            panic!("expected KeyVal for materialized NPC corp 1000004 in {method}");
        };
        assert_eq!(
            ytiri_info.get("ceoID"),
            Some(&PyRep::Integer(expected_ytiri_ceo)),
            "materialized NPC corp 1000004 with ceo_id=0 must resolve positive ceoID in {method} (#965)"
        );
        assert_ne!(ytiri_info.get("ticker"), Some(&PyRep::String("NPC".into())));
    }
    assert_eq!(
        handle_corps(
            &ctx,
            &mut ceo_sess,
            "corpmgr",
            "ceoID",
            &[PyRep::Integer(1_000_004)],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Integer(expected_ytiri_ceo)
    );
}

#[cfg(test)]
#[path = "fleet_social_corps_extra_unit_tests.rs"]
mod fleet_social_corps_extra_unit_tests;
