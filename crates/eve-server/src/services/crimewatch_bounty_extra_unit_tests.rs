//! Additional branch-coverage and mutation-killing unit tests for `crimewatch_bounty.rs`.

#![forbid(unsafe_code)]

use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::{
    make_fighter_ctx_and_session, make_fighter_item,
};
use crate::services::combat_crimewatch::*;
use eve_db::{AllianceRecord, CorporationRecord, InventoryFlag, IskCents};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};

fn call_bounty_rpc(
    ctx: &crate::router::ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    handle_bounty_proxy(ctx, session, service, method, args)
}

fn call_sec_tags_rpc(
    ctx: &crate::router::ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    handle_apply_tags_for_sec_status(ctx, session, args)
}

#[test]
fn kill_right_sale_restrictions_expiry_and_queries_kill_mutations() {
    assert_eq!(BountyTargetType::Pilot, BountyTargetType::Pilot);
    assert_ne!(BountyTargetType::Corporation, BountyTargetType::Alliance);
    assert_ne!(BountyTargetType::All, BountyTargetType::Pilot);
    assert_eq!(BountySearchType::Pilot, BountySearchType::Pilot);
    assert_ne!(BountySearchType::Corporation, BountySearchType::Alliance);

    let (ctx, mut session, char_id, _ship_id, _sys_id) = make_fighter_ctx_and_session();
    let other_char_id = 90_000_077_i64;
    let target_char_id = 90_000_078_i64;
    let corp_id = 2_000_077_i64;
    let ally_id = 3_000_077_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let base = snap.characters.get(&char_id).cloned().expect("base char");
        let mut c2 = base.clone();
        c2.character_id = other_char_id;
        c2.name = "OtherPilot".into();
        snap.characters.insert(other_char_id, c2);

        let mut c3 = base;
        c3.character_id = target_char_id;
        c3.name = "SuspectTarget".into();
        snap.characters.insert(target_char_id, c3);

        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "RestrictionCorp".into(),
                ticker: "RCRP".into(),
                ceo_id: char_id,
                alliance_id: Some(ally_id),
                ..Default::default()
            },
        );
        snap.alliances.insert(
            ally_id,
            AllianceRecord {
                alliance_id: ally_id,
                name: "RestrictionAlliance".into(),
                short_name: "RALL".into(),
                executor_corp_id: corp_id,
                member_corp_ids: std::collections::HashSet::from([corp_id]),
            },
        );
    });

    // Unauthenticated session is rejected across endpoints
    let mut unauth = SessionState::default();
    for m in [
        "SellKillRight", "CancelSellKillRight", "GetMyKillRights", "AddToBounty", "GetBounty",
        "GetMyBounties", "SearchBounties", "clear_memoized", "GetTopPilotBounties", "ClearBounty",
    ] {
        let res = call_bounty_rpc(&ctx, &mut unauth, "bountyProxy", m, &[]).unwrap();
        assert!(res.is_err(), "{m} should reject unauthenticated session");
    }
    assert!(call_bounty_rpc(&ctx, &mut session, "petitioner", "GetBounty", &[]).is_none());

    // SellKillRight validation branches
    create_kill_right(&ctx, 1801, char_id, target_char_id, 0.0);
    let err_missing_price =
        call_bounty_rpc(&ctx, &mut session, "killRightMgr", "SellKillRight", &[PyRep::Integer(1801)])
            .unwrap()
            .unwrap_err();
    assert_eq!(err_missing_price.code, "InvalidPrice");

    let err_nan_price =
        call_bounty_rpc(&ctx, &mut session, "killRightMgr", "SellKillRight", &[PyRep::Integer(1801), PyRep::Real(f64::NAN)])
            .unwrap()
            .unwrap_err();
    assert_eq!(err_nan_price.code, "InvalidPrice");

    let err_not_found =
        call_bounty_rpc(&ctx, &mut session, "bountyProxy", "SellKillRight", &[PyRep::Integer(999_999), PyRep::Real(100.0)])
            .unwrap()
            .unwrap_err();
    assert_eq!(err_not_found.code, "KillRightNotFound");

    // Non-owner cannot sell or cancel
    let mut other_sess = session.clone();
    other_sess.character_id = Some(other_char_id);
    let err_not_owner = call_bounty_rpc(
        &ctx,
        &mut other_sess,
        "bountyProxy",
        "SellKillRight",
        &[PyRep::Integer(1801), PyRep::Real(100.0)],
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_not_owner.code, "NotKillRightOwner");

    let err_cancel_not_owner = call_bounty_rpc(
        &ctx,
        &mut other_sess,
        "killRightMgr",
        "CancelSellKillRight",
        &[PyRep::Integer(1801)],
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_cancel_not_owner.code, "NotKillRightOwner");

    let err_cancel_missing = call_bounty_rpc(
        &ctx,
        &mut session,
        "killRightMgr",
        "CancelSellKillRight",
        &[PyRep::Integer(999_999)],
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_cancel_missing.code, "KillRightNotFound");

    // Invalid restriction target rejected
    let err_bad_restr = call_bounty_rpc(
        &ctx,
        &mut session,
        "bountyProxy",
        "SellKillRight",
        &[PyRep::Integer(1801), PyRep::Real(0.0), PyRep::Integer(888_888_888)],
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_bad_restr.code, "InvalidKillRightRestrictionTarget");

    // Zero price with valid alliance restriction sets forSale = true
    let ok_zero_restr = call_bounty_rpc(
        &ctx,
        &mut session,
        "killRightMgr",
        "SellKillRight",
        &[PyRep::Integer(1801), PyRep::Real(0.0), PyRep::Integer(ally_id)],
    )
    .unwrap()
    .unwrap();
    if let PyRep::List(list) = ok_zero_restr {
        assert_eq!(list[0].dict_get("forSale"), Some(&PyRep::Bool(true)));
        assert_eq!(list[0].dict_get("restrictedTo"), Some(&PyRep::Integer(ally_id)));
    } else {
        panic!("expected List");
    }

    // Zero price with no restriction sets forSale = false and clears restrictedTo
    let ok_zero_public = call_bounty_rpc(
        &ctx,
        &mut session,
        "bountyProxy",
        "SellKillRight",
        &[PyRep::Integer(1801), PyRep::Real(0.0), PyRep::Integer(0)],
    )
    .unwrap()
    .unwrap();
    if let PyRep::List(list) = ok_zero_public {
        assert_eq!(list[0].dict_get("forSale"), Some(&PyRep::Bool(false)));
        assert_eq!(list[0].dict_get("restrictedTo"), Some(&PyRep::None));
    } else {
        panic!("expected List");
    }

    // Active suspect kill right cannot be listed
    ctx.db.with_mut_snapshot(|snap| {
        snap.kill_rights.get_mut(&1801).unwrap().is_active_suspect = true;
    });
    let err_active = call_bounty_rpc(
        &ctx,
        &mut session,
        "bountyProxy",
        "SellKillRight",
        &[PyRep::Integer(1801), PyRep::Real(500.0)],
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_active.code, "KillRightAlreadyActivated");

    // Expired kill right cannot be listed and is filtered out of GetMyKillRights / GetKillRightsOnCharacters
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.kill_rights.get_mut(&1801).unwrap().is_active_suspect = false;
        snap.combat_crimewatch_state.kill_right_expires_unix.insert(1801, now);
    });
    let err_exp = call_bounty_rpc(
        &ctx,
        &mut session,
        "bountyProxy",
        "SellKillRight",
        &[PyRep::Integer(1801), PyRep::Real(500.0)],
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_exp.code, "KillRightExpired");

    // Create a second non-expired kill right between other_char_id and target_char_id
    create_kill_right(&ctx, 1802, other_char_id, target_char_id, 250_000.0);
    ctx.db.with_mut_snapshot(|snap| {
        snap.combat_crimewatch_state.kill_right_expires_unix.insert(1802, now + 3600);
        snap.combat_crimewatch_state.kill_right_restricted_to.insert(1802, other_char_id);
    });

    let my_empty = call_bounty_rpc(&ctx, &mut session, "bountyProxy", "GetMyKillRights", &[])
        .unwrap()
        .unwrap();
    assert_eq!(my_empty, PyRep::List(vec![]));

    let on_chars = call_bounty_rpc(
        &ctx,
        &mut session,
        "bountyProxy",
        "GetKillRightsOnCharacters",
        &[PyRep::List(vec![PyRep::Integer(target_char_id)])],
    )
    .unwrap()
    .unwrap();
    if let PyRep::List(list) = on_chars {
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].dict_get("killRightID"), Some(&PyRep::Integer(1802)));
        assert_eq!(list[0].dict_get("restrictedTo"), Some(&PyRep::Integer(other_char_id)));
    } else {
        panic!("expected List");
    }
}

#[test]
fn bounty_leaderboards_aggregation_searches_and_gm_actions_kill_mutations() {
    let (ctx, mut session, char_id, _ship_id, _sys_id) = make_fighter_ctx_and_session();
    let (pilot_a, pilot_b, pilot_c) = (90_000_101_i64, 90_000_102_i64, 90_000_103_i64);
    let (corp_agg, corp_direct) = (2_000_101_i64, 2_000_102_i64);
    let (ally_agg, ally_direct) = (3_000_101_i64, 3_000_102_i64);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.balance = IskCents::from_f64_isk(500_000.0);
        }
        let base = snap.characters.get(&char_id).cloned().unwrap();

        let mut pa = base.clone();
        pa.character_id = pilot_a;
        pa.name = "Alpha Outlaw".into();
        pa.corporation_id = corp_agg;
        pa.alliance_id = None;
        snap.characters.insert(pilot_a, pa);

        let mut pb = base.clone();
        pb.character_id = pilot_b;
        pb.name = "Beta Outlaw".into();
        pb.corporation_id = corp_agg;
        pb.alliance_id = Some(ally_agg);
        snap.characters.insert(pilot_b, pb);

        let mut pc = base;
        pc.character_id = pilot_c;
        pc.name = "Gamma Outlaw".into();
        pc.corporation_id = corp_direct;
        pc.alliance_id = Some(ally_direct);
        snap.characters.insert(pilot_c, pc);

        snap.corporations.insert(
            corp_agg,
            CorporationRecord {
                corporation_id: corp_agg,
                name: "Aggregated Pirate Corp".into(),
                ticker: "APC".into(),
                ceo_id: pilot_a,
                alliance_id: Some(ally_agg),
                ..Default::default()
            },
        );
        snap.corporations.insert(
            corp_direct,
            CorporationRecord {
                corporation_id: corp_direct,
                name: "Direct Pirate Corp".into(),
                ticker: "DPC".into(),
                ceo_id: pilot_c,
                alliance_id: Some(ally_direct),
                ..Default::default()
            },
        );
        snap.alliances.insert(
            ally_agg,
            AllianceRecord {
                alliance_id: ally_agg,
                name: "Aggregated Pirate Alliance".into(),
                short_name: "APA".into(),
                executor_corp_id: corp_agg,
                member_corp_ids: std::collections::HashSet::from([corp_agg]),
            },
        );
        snap.alliances.insert(
            ally_direct,
            AllianceRecord {
                alliance_id: ally_direct,
                name: "Direct Pirate Alliance".into(),
                short_name: "DPA".into(),
                executor_corp_id: corp_direct,
                member_corp_ids: std::collections::HashSet::from([corp_direct]),
            },
        );
    });

    assert_eq!(
        call_bounty_rpc(&ctx, &mut session, "bountyProxy", "AddToBounty", &[PyRep::Integer(0), PyRep::Real(100_000.0)])
            .unwrap()
            .unwrap_err()
            .code,
        "BountyAmountTooLow"
    );
    assert_eq!(
        call_bounty_rpc(&ctx, &mut session, "bountyProxy", "AddToBounty", &[PyRep::Integer(777_777_777), PyRep::Real(100_000.0)])
            .unwrap()
            .unwrap_err()
            .code,
        "TargetNotFound"
    );
    assert_eq!(
        call_bounty_rpc(&ctx, &mut session, "bountyProxy", "AddToBounty", &[PyRep::Integer(pilot_a), PyRep::Real(600_000.0)])
            .unwrap()
            .unwrap_err()
            .code,
        "InsufficientFunds"
    );

    let ok_exact = call_bounty_rpc(
        &ctx,
        &mut session,
        "bountyProxy",
        "AddToBounty",
        &[PyRep::Integer(pilot_a), PyRep::Real(100_000.0)],
    )
    .unwrap()
    .unwrap();
    assert_eq!(ok_exact.dict_get("totalBountyISK"), Some(&PyRep::Real(100_000.0)));

    with_mut_combat_runtime(&ctx, |rt| {
        rt.player_bounty_pool.insert(pilot_b, 250_000.0);
        rt.player_bounty_pool.insert(pilot_c, 900_000.0);
        rt.player_bounty_pool.insert(corp_direct, 400_000.0);
        rt.player_bounty_pool.insert(ally_direct, 500_000.0);
        rt.player_bounty_pool.insert(999_888_777, 0.0);
    });

    // GetTopCorpBounties: corp_direct stays 400k, corp_agg aggregates pilot_a (100k) + pilot_b (250k) = 350k
    let top_corps = call_bounty_rpc(&ctx, &mut session, "bountyProxy", "GetTopCorpBounties", &[PyRep::Integer(1)])
        .unwrap()
        .unwrap();
    if let PyRep::List(rows) = top_corps {
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].dict_get("targetID"), Some(&PyRep::Integer(corp_direct)));
        assert_eq!(rows[0].dict_get("bountyISK"), Some(&PyRep::Real(400_000.0)));
        assert_eq!(rows[1].dict_get("targetID"), Some(&PyRep::Integer(corp_agg)));
        assert_eq!(rows[1].dict_get("bountyISK"), Some(&PyRep::Real(350_000.0)));
    } else {
        panic!("expected List");
    }

    // GetTopAllianceBounties: ally_direct stays 500k, ally_agg aggregates pilot_a + pilot_b = 350k
    let top_allies = call_bounty_rpc(&ctx, &mut session, "bountyProxy", "GetTopAllianceBounties", &[PyRep::Integer(1)])
        .unwrap()
        .unwrap();
    if let PyRep::List(rows) = top_allies {
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].dict_get("targetID"), Some(&PyRep::Integer(ally_direct)));
        assert_eq!(rows[0].dict_get("bountyISK"), Some(&PyRep::Real(500_000.0)));
        assert_eq!(rows[1].dict_get("targetID"), Some(&PyRep::Integer(ally_agg)));
        assert_eq!(rows[1].dict_get("bountyISK"), Some(&PyRep::Real(350_000.0)));
    } else {
        panic!("expected List");
    }

    // Search by exact ID vs substring across Pilot, Corp, Alliance
    let search_by_id = call_bounty_rpc(&ctx, &mut session, "bountyProxy", "SearchBounties", &[PyRep::Integer(pilot_b)])
        .unwrap()
        .unwrap();
    if let PyRep::List(rows) = search_by_id {
        assert_eq!(rows.len(), 1);
        if let PyRep::Tuple(pair) = &rows[0] {
            assert_eq!(pair[0], PyRep::Integer(1));
            assert_eq!(pair[1].dict_get("targetID"), Some(&PyRep::Integer(pilot_b)));
            assert_eq!(pair[1].dict_get("corporationID"), Some(&PyRep::Integer(corp_agg)));
            assert_eq!(pair[1].dict_get("allianceID"), Some(&PyRep::Integer(ally_agg)));
        } else {
            panic!("expected Tuple");
        }
    } else {
        panic!("expected List");
    }

    let search_corp = call_bounty_rpc(&ctx, &mut session, "bountyProxy", "SearchCorpBounties", &[PyRep::String("direct".into())])
        .unwrap()
        .unwrap();
    assert!(matches!(search_corp, PyRep::List(ref l) if l.len() == 1));

    let search_ally = call_bounty_rpc(&ctx, &mut session, "bountyProxy", "SearchAllianceBounties", &[PyRep::String("direct".into())])
        .unwrap()
        .unwrap();
    assert!(matches!(search_ally, PyRep::List(ref l) if l.len() == 1));

    let all_bounties = call_bounty_rpc(&ctx, &mut session, "bountyProxy", "GetBounties", &[])
        .unwrap()
        .unwrap();
    assert!(matches!(all_bounties, PyRep::List(ref l) if l.len() == 5));

    let b_and_kr = call_bounty_rpc(
        &ctx,
        &mut session,
        "bountyProxy",
        "GetBountiesAndKillRights",
        &[PyRep::List(vec![PyRep::Integer(pilot_a)])],
    )
    .unwrap()
    .unwrap();
    assert!(matches!(b_and_kr, PyRep::Tuple(ref t) if t.len() == 2));

    assert_eq!(
        call_bounty_rpc(&ctx, &mut session, "bountyProxy", "GMClearBountyCache", &[])
            .unwrap()
            .unwrap_err()
            .code,
        "CustomError"
    );

    session.role = eve_machonet::service_roles::ROLE_GMH;
    let set_res = call_bounty_rpc(
        &ctx,
        &mut session,
        "bountyProxy",
        "ClearBounty",
        &[PyRep::Integer(pilot_a), PyRep::Real(750_000.0)],
    )
    .unwrap()
    .unwrap();
    assert_eq!(set_res.dict_get("action"), Some(&PyRep::String("set".into())));
    assert_eq!(set_res.dict_get("amountISK"), Some(&PyRep::Real(750_000.0)));

    let clr_res = call_bounty_rpc(&ctx, &mut session, "bountyProxy", "ClearBounty", &[PyRep::Integer(pilot_a)])
        .unwrap()
        .unwrap();
    assert_eq!(clr_res.dict_get("action"), Some(&PyRep::String("clear".into())));
}

#[test]
fn apply_tags_for_sec_status_validation_cargo_partial_stacks_and_fees_kill_mutations() {
    let (ctx, mut session, char_id, ship_id, _sys_id) = make_fighter_ctx_and_session();

    let (lowsec_station_id, lowsec_sys_id) = ctx
        .sde
        .stations
        .values()
        .find_map(|st| {
            let sec = ctx.sde.solar_systems.get(&st.solar_system_id)?.security_status;
            (0.0 < sec
                && sec < 0.45
                && crate::services::station_structure::npc_station_offers_service(&ctx.sde, st.station_id, 27))
            .then_some((st.station_id, st.solar_system_id))
        })
        .expect("lowsec Security Office station");

    let highsec_station_id = ctx
        .sde
        .stations
        .values()
        .find_map(|st| {
            let sec = ctx.sde.solar_systems.get(&st.solar_system_id)?.security_status;
            (sec >= 0.45).then_some(st.station_id)
        })
        .expect("highsec station");

    session.station_id = Some(69_999_999);
    assert_eq!(call_sec_tags_rpc(&ctx, &session, &[]).unwrap().unwrap_err().code, "SecurityOfficeRequired");

    session.station_id = Some(highsec_station_id);
    assert_eq!(call_sec_tags_rpc(&ctx, &session, &[]).unwrap().unwrap_err().code, "SecurityOfficeRequired");

    session.station_id = Some(lowsec_station_id);
    session.solar_system_id = Some(lowsec_sys_id);
    assert_eq!(
        call_sec_tags_rpc(&ctx, &session, &[PyRep::Real(0.0)]).unwrap().unwrap_err().code,
        "InvalidSecurityTagArguments"
    );
    assert_eq!(
        call_sec_tags_rpc(&ctx, &session, &[PyRep::Real(0.0), PyRep::Integer(1)]).unwrap().unwrap_err().code,
        "InvalidSecurityTagArguments"
    );
    assert_eq!(
        call_sec_tags_rpc(&ctx, &session, &[PyRep::Real(0.0), PyRep::Dict(vec![])]).unwrap().unwrap_err().code,
        "InvalidQuantity"
    );
    assert_eq!(
        call_sec_tags_rpc(
            &ctx,
            &session,
            &[PyRep::Real(0.0), PyRep::Dict(vec![(PyRep::String("bad".into()), PyRep::Integer(1))])]
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidSecurityTagType"
    );
    assert_eq!(
        call_sec_tags_rpc(
            &ctx,
            &session,
            &[PyRep::Real(0.0), PyRep::Dict(vec![(PyRep::Integer(33_137), PyRep::Integer(1))])]
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidSecurityTagType"
    );
    assert_eq!(
        call_sec_tags_rpc(
            &ctx,
            &session,
            &[PyRep::Real(0.0), PyRep::Dict(vec![(PyRep::Integer(33_141), PyRep::Integer(0))])]
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidQuantity"
    );
    assert_eq!(
        call_sec_tags_rpc(
            &ctx,
            &session,
            &[PyRep::Real(0.0), PyRep::Dict(vec![(PyRep::Integer(33_141), PyRep::Integer(1))])]
        )
        .unwrap()
        .unwrap_err()
        .code,
        "NotDockedInStation"
    );

    let cargo_tag_id = 890_101_i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(lowsec_station_id);
            ch.solar_system_id = lowsec_sys_id;
            ch.security_status = -1.0;
            ch.balance = IskCents::from_f64_isk(5_000_000.0);
        }
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.location_id = lowsec_station_id;
            ship.flag = InventoryFlag::Hangar;
        }
    });

    assert_eq!(
        call_sec_tags_rpc(
            &ctx,
            &session,
            &[PyRep::Real(-1.0), PyRep::Dict(vec![(PyRep::Integer(33_141), PyRep::Integer(1))])]
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidTargetSecurityStatus"
    );
    assert_eq!(
        call_sec_tags_rpc(
            &ctx,
            &session,
            &[PyRep::Real(0.0), PyRep::Dict(vec![(PyRep::Integer(33_141), PyRep::Integer(2))])]
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InsufficientSecurityTags"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            cargo_tag_id,
            make_fighter_item(cargo_tag_id, 33_141, char_id, ship_id, InventoryFlag::Cargo, 5),
        );
    });
    assert_eq!(
        call_sec_tags_rpc(
            &ctx,
            &session,
            &[PyRep::Real(0.0), PyRep::Dict(vec![(PyRep::Integer(33_141), PyRep::Integer(5))])]
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InsufficientFunds"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().balance = IskCents::from_f64_isk(25_000_000.0);
    });
    let res = call_sec_tags_rpc(
        &ctx,
        &session,
        &[PyRep::Real(0.0), PyRep::Dict(vec![(PyRep::Integer(33_141), PyRep::Integer(5))])],
    )
    .unwrap()
    .unwrap();
    assert_eq!(res.dict_get("newSecurityStatus"), Some(&PyRep::Real(0.0)));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&cargo_tag_id].quantity, 3);
        assert!((snap.characters[&char_id].balance.to_wire_f64() - 5_000_000.0).abs() < 1e-2);
    });
}
