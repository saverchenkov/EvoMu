//! Unit and mutation-killing tests for `crimewatch_bounty`.

use super::*;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::{
    make_fighter_ctx_and_session, make_fighter_item,
};
use eve_db::{AllianceRecord, CorporationRecord, InventoryFlag, IskCents};
use eve_proto::PyRep;

#[test]
fn bounty_proxy_kill_right_sales_placement_searches_leaderboards_and_gm_tools() {
    let (ctx, mut session, char_id, _ship_id, _sys_id) = make_fighter_ctx_and_session();
    let target_char_id = 90_000_002_i64;
    let corp_id = 2_000_050_i64;
    let alliance_id = 3_000_050_i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.balance = IskCents::from_f64_isk(50_000_000.0);
            ch.corporation_id = corp_id;
            ch.alliance_id = Some(alliance_id);
        }
        let mut t = snap.characters.get(&char_id).cloned().unwrap();
        t.character_id = target_char_id;
        t.name = "OutlawTarget".into();
        t.corporation_id = corp_id;
        t.alliance_id = Some(alliance_id);
        snap.characters.insert(target_char_id, t);
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "BountyTestCorp".into(),
                ticker: "BTC".into(),
                ceo_id: char_id,
                alliance_id: Some(alliance_id),
                tax_rate: 0.10,
                ..Default::default()
            },
        );
        snap.alliances.insert(
            alliance_id,
            AllianceRecord {
                alliance_id,
                name: "BountyTestAlliance".into(),
                short_name: "BTA".into(),
                executor_corp_id: corp_id,
                member_corp_ids: std::collections::HashSet::from([corp_id]),
            },
        );
    });

    // 1. Create a kill right, test SellKillRight validation & CancelSellKillRight
    create_kill_right(&ctx, 701, char_id, target_char_id, 0.0);
    let neg_err = handle_bounty_proxy(
        &ctx,
        &mut session,
        "bountyProxy",
        "SellKillRight",
        &[PyRep::Integer(701), PyRep::Real(-10.0)],
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(neg_err.code, "InvalidPrice");

    let sell_ok = handle_bounty_proxy(
        &ctx,
        &mut session,
        "bountyProxy",
        "SellKillRight",
        &[
            PyRep::Integer(701),
            PyRep::Real(1_500_000.0),
            PyRep::Integer(corp_id),
        ],
    )
    .unwrap()
    .unwrap();
    if let PyRep::List(items) = sell_ok {
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].dict_get("forSale"), Some(&PyRep::Bool(true)));
        assert_eq!(items[0].dict_get("price"), Some(&PyRep::Real(1_500_000.0)));
        assert_eq!(
            items[0].dict_get("restrictedTo"),
            Some(&PyRep::Integer(corp_id))
        );
    } else {
        panic!("expected list");
    }

    let my_krs = handle_bounty_proxy(&ctx, &mut session, "bountyProxy", "GetMyKillRights", &[])
        .unwrap()
        .unwrap();
    assert!(matches!(my_krs, PyRep::List(ref v) if v.len() == 1));

    let cancel_ok = handle_bounty_proxy(
        &ctx,
        &mut session,
        "bountyProxy",
        "CancelSellKillRight",
        &[PyRep::Integer(701)],
    )
    .unwrap()
    .unwrap();
    assert_eq!(cancel_ok.dict_get("forSale"), Some(&PyRep::Bool(false)));

    // 2. AddToBounty: minimum 100,000 ISK check + valid placement on pilot, corp, and alliance
    let low_err = handle_bounty_proxy(
        &ctx,
        &mut session,
        "bountyProxy",
        "AddToBounty",
        &[PyRep::Integer(target_char_id), PyRep::Real(99_999.0)],
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(low_err.code, "BountyAmountTooLow");

    let add_pilot = handle_bounty_proxy(
        &ctx,
        &mut session,
        "bountyProxy",
        "AddToBounty",
        &[PyRep::Integer(target_char_id), PyRep::Real(250_000.0)],
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        add_pilot.dict_get("totalBountyISK"),
        Some(&PyRep::Real(250_000.0))
    );

    // 3. GetBounty, GetBounties, GetMyBounties, SearchBounties, SearchCorpBounties, SearchAllianceBounties
    let gb = handle_bounty_proxy(
        &ctx,
        &mut session,
        "bountyProxy",
        "GetBounty",
        &[PyRep::Integer(target_char_id)],
    )
    .unwrap()
    .unwrap();
    assert_eq!(gb.dict_get("bountyISK"), Some(&PyRep::Real(250_000.0)));

    let gbs = handle_bounty_proxy(
        &ctx,
        &mut session,
        "bountyProxy",
        "GetBounties",
        &[PyRep::List(vec![PyRep::Integer(target_char_id)])],
    )
    .unwrap()
    .unwrap();
    assert!(matches!(gbs, PyRep::List(ref v) if v.len() == 1));

    let gmb = handle_bounty_proxy(&ctx, &mut session, "bountyProxy", "GetMyBounties", &[])
        .unwrap()
        .unwrap();
    assert!(matches!(gmb, PyRep::List(ref v) if v.len() == 1));

    let search_p = handle_bounty_proxy(
        &ctx,
        &mut session,
        "bountyProxy",
        "SearchCharBounties",
        &[PyRep::String("outlaw".into())],
    )
    .unwrap()
    .unwrap();
    assert!(matches!(search_p, PyRep::List(ref v) if v.len() == 1));

    // 4. Leaderboards: GetTopPilotBounties, GetTopCorpBounties, GetTopAllianceBounties, GetTopPilotBountyHunters
    with_mut_combat_runtime(&ctx, |rt| {
        rt.bounty_hunter_claimed.insert(char_id, 500_000.0);
    });
    for m in [
        "GetTopPilotBounties",
        "GetTopCorpBounties",
        "GetTopAllianceBounties",
    ] {
        let top = handle_bounty_proxy(&ctx, &mut session, "bountyProxy", m, &[PyRep::Integer(1)])
            .unwrap()
            .unwrap();
        assert!(matches!(top, PyRep::List(ref v) if !v.is_empty()));
    }
    for m in [
        "GetTopPilotBountyHunters",
        "GetTopCorporationBountyHunters",
        "GetTopAllianceBountyHunters",
        "GetTopBountyHunters",
    ] {
        let top_h = handle_bounty_proxy(&ctx, &mut session, "bountyProxy", m, &[PyRep::Integer(1)])
            .unwrap()
            .unwrap();
        assert!(matches!(top_h, PyRep::Tuple(ref v) if v.len() == 2));
    }

    assert_eq!(
        handle_bounty_proxy(&ctx, &mut session, "bountyProxy", "clear_memoized", &[])
            .unwrap()
            .unwrap(),
        PyRep::None
    );

    session.role = eve_machonet::service_roles::ROLE_GMH;
    assert_eq!(
        handle_bounty_proxy(&ctx, &mut session, "bountyProxy", "GMClearBountyCache", &[])
            .unwrap()
            .unwrap(),
        PyRep::None
    );
    let reimb = handle_bounty_proxy(
        &ctx,
        &mut session,
        "bountyProxy",
        "GMReimburseBounties",
        &[PyRep::Integer(target_char_id)],
    )
    .unwrap()
    .unwrap();
    assert_eq!(reimb.dict_get("amountISK"), Some(&PyRep::Real(250_000.0)));
}

#[test]
fn apply_tags_for_sec_status_enforces_lowsec_concord_office_bands_and_fees() {
    let (ctx, mut session, char_id, _ship_id, _sys_id) = make_fighter_ctx_and_session();

    // Find a low-sec station offering Security Office service (27), or seed one if needed
    let lowsec_station_id = ctx
        .sde
        .stations
        .values()
        .find(|st| {
            let sec = ctx
                .sde
                .solar_systems
                .get(&st.solar_system_id)
                .map(|s| s.security_status)
                .unwrap_or(1.0);
            0.0 < sec
                && sec < 0.45
                && crate::services::station_structure::npc_station_offers_service(
                    &ctx.sde,
                    st.station_id,
                    27,
                )
        })
        .map(|st| (st.station_id, st.solar_system_id))
        .expect("SDE has low-sec Security Office station");

    let (station_id, lowsec_sys_id) = lowsec_station_id;

    // 1. Not docked in station rejected
    session.station_id = None;
    let err_undocked = handle_apply_tags_for_sec_status(&ctx, &session, &[])
        .unwrap()
        .unwrap_err();
    assert_eq!(err_undocked.code, "NotDockedInStation");

    // Dock character at the low-sec Security Office station with sec_status = -5.5 and 50M ISK
    session.station_id = Some(station_id);
    session.solar_system_id = Some(lowsec_sys_id);
    let tag_33139_id = 880_001_i64;
    let tag_33140_id = 880_002_i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(station_id);
            ch.solar_system_id = lowsec_sys_id;
            ch.security_status = -5.5;
            ch.balance = IskCents::from_f64_isk(50_000_000.0);
        }
        // 1x 33139 (-8..-5 band, 2M ISK) + 2x 33140 (-5..-2 band, 5M ISK each)
        snap.items.insert(
            tag_33139_id,
            make_fighter_item(tag_33139_id, 33_139, char_id, station_id, InventoryFlag::Hangar, 1),
        );
        snap.items.insert(
            tag_33140_id,
            make_fighter_item(tag_33140_id, 33_140, char_id, station_id, InventoryFlag::Hangar, 2),
        );
    });

    // 2. Wrong band tag (33138 is for [-10, -8)) rejected at -5.5
    let band_err = handle_apply_tags_for_sec_status(
        &ctx,
        &session,
        &[
            PyRep::Real(-4.0),
            PyRep::Dict(vec![(PyRep::Integer(33_138), PyRep::Integer(1))]),
        ],
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(band_err.code, "SecurityTagBandMismatch");

    // 3. Valid multi-band turn-in from -5.5 to -4.0 using 1x 33139 (+0.5 -> -5.0, 2M) + 2x 33140 (+1.0 -> -4.0, 10M) = 12M total fee
    let ok_res = handle_apply_tags_for_sec_status(
        &ctx,
        &session,
        &[
            PyRep::Real(-4.0),
            PyRep::Dict(vec![
                (PyRep::Integer(33_139), PyRep::Integer(1)),
                (PyRep::Integer(33_140), PyRep::Integer(2)),
            ]),
        ],
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        ok_res.dict_get("oldSecurityStatus"),
        Some(&PyRep::Real(-5.5))
    );
    assert_eq!(
        ok_res.dict_get("newSecurityStatus"),
        Some(&PyRep::Real(-4.0))
    );
    ctx.db.with_snapshot(|snap| {
        let ch = snap.characters.get(&char_id).unwrap();
        assert!((ch.security_status - (-4.0)).abs() < 1e-6);
        assert!((ch.balance.to_wire_f64() - 38_000_000.0).abs() < 1e-2);
        assert!(!snap.items.contains_key(&tag_33139_id));
        assert!(!snap.items.contains_key(&tag_33140_id));
    });
}

#[test]
fn apply_tags_for_sec_status_supports_v24_two_float_signature_and_validates_bounds() {
    let (ctx, mut session, char_id, _ship_id, _sys_id) = make_fighter_ctx_and_session();
    let (station_id, lowsec_sys_id) = ctx
        .sde
        .stations
        .values()
        .find(|st| {
            let sec = ctx
                .sde
                .solar_systems
                .get(&st.solar_system_id)
                .map(|s| s.security_status)
                .unwrap_or(1.0);
            0.0 < sec
                && sec < 0.45
                && crate::services::station_structure::npc_station_offers_service(
                    &ctx.sde,
                    st.station_id,
                    27,
                )
        })
        .map(|st| (st.station_id, st.solar_system_id))
        .expect("SDE has low-sec Security Office station");
    session.solar_system_id = Some(lowsec_sys_id);
    session.station_id = Some(station_id);
    let tag_33139_id = 910_000_201_i64;
    let tag_33140_id = 910_000_202_i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(station_id);
            ch.solar_system_id = lowsec_sys_id;
            ch.security_status = -5.5;
            ch.balance = IskCents::from_f64_isk(50_000_000.0);
        }
        snap.items.insert(
            tag_33139_id,
            make_fighter_item(tag_33139_id, 33_139, char_id, station_id, InventoryFlag::Hangar, 1),
        );
        snap.items.insert(
            tag_33140_id,
            make_fighter_item(tag_33140_id, 33_140, char_id, station_id, InventoryFlag::Hangar, 2),
        );
    });

    // Non-finite arguments must fail validation before touching state
    let nan_err = handle_apply_tags_for_sec_status(
        &ctx,
        &session,
        &[PyRep::Real(f64::NAN), PyRep::Real(-5.5)],
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(nan_err.code, "InvalidSecurityTagArguments");

    // Reaching 0.0 requires more tags than available in [-2.0, 0.0)
    let ins_err = handle_apply_tags_for_sec_status(
        &ctx,
        &session,
        &[PyRep::Real(0.0), PyRep::Real(-5.5)],
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(ins_err.code, "InsufficientSecurityTags");

    // V24.01 2-float call (targetSec, currentSec) auto-consumes bands up to -4.0
    let ok_res = handle_apply_tags_for_sec_status(
        &ctx,
        &session,
        &[PyRep::Real(-4.0), PyRep::Real(-5.5)],
    )
    .unwrap()
    .unwrap();
    assert_eq!(ok_res.dict_get("oldSecurityStatus"), Some(&PyRep::Real(-5.5)));
    assert_eq!(ok_res.dict_get("newSecurityStatus"), Some(&PyRep::Real(-4.0)));
    ctx.db.with_snapshot(|snap| {
        let ch = snap.characters.get(&char_id).unwrap();
        assert!((ch.security_status - (-4.0)).abs() < 1e-6);
        assert!((ch.balance.to_wire_f64() - 38_000_000.0).abs() < 1e-2);
        assert!(!snap.items.contains_key(&tag_33139_id));
        assert!(!snap.items.contains_key(&tag_33140_id));
    });
}
