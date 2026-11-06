//! Extra branch-coverage and mutation-killing unit tests for `corp_registry_ops.rs`.

#![forbid(unsafe_code)]

use crate::router::{BoundObjectEntry, ServerContext};
use crate::services::account_services::account_services_tests::PyRepTestExt;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::{
    make_fighter_ctx_and_session, make_fighter_item,
};
use crate::services::corp_registry_ops::*;
use eve_db::snapshot_ext::MarketOrderMeta;
use eve_db::{
    corp_roles, CorporationRecord, FleetMemberRecord, FleetRecord, FleetRole, InventoryFlag,
    IskCents, MarketOrderRecord,
};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use std::collections::HashMap;

fn call_corp_reg(
    ctx: &ServerContext,
    sess: &mut SessionState,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    let kw = HashMap::new();
    handle(ctx, sess, "corpRegistry", method, args, &kw)
        .expect("corpRegistry method handled")
}

fn setup_ctx() -> (ServerContext, SessionState, i64, i64, i64, i64) {
    let (ctx, mut sess, ceo_id, ship_id, _sys_id) = make_fighter_ctx_and_session();
    let corp_id = 98_000_050_i64;
    let member_id = 90_000_002_i64;
    let station_id = 60_003_760_i64;
    sess.corporation_id = Some(corp_id);
    sess.station_id = Some(station_id);
    sess.corp_role = u64::MAX;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&ceo_id) {
            ch.corporation_id = corp_id;
            ch.station_id = Some(station_id);
        }
        if let Some(ch) = snap.characters.get_mut(&member_id) {
            ch.corporation_id = corp_id;
            ch.station_id = Some(station_id);
        }
        let mut corp = CorporationRecord {
            corporation_id: corp_id,
            name: "Vanguard Logistics Corp".into(),
            ticker: "VGLC".into(),
            ceo_id,
            master_wallet: IskCents::from_isk(50_000_000),
            members: [ceo_id, member_id].into_iter().collect(),
            member_roles: [(ceo_id, u64::MAX), (member_id, 0)].into_iter().collect(),
            hq_station_id: Some(station_id),
            ..Default::default()
        };
        corp.ensure_wallet_divisions();
        snap.corporations.insert(corp_id, corp);
    });
    (ctx, sess, ceo_id, member_id, corp_id, ship_id)
}

#[tokio::test]
async fn command_burst_dogma_activate_and_invbroker_hangar_branches() {
    let (ctx, mut sess, ceo_id, member_id, corp_id, _ship_id) = setup_ctx();
    let kw = HashMap::new();

    // corporationSvc.GetEmploymentRecord delegates to fleet_social::handle
    let emp = handle_corp_hangar_invbroker(
        &ctx,
        &mut sess,
        None,
        "corporationSvc",
        "GetEmploymentRecord",
        &[PyRep::Integer(ceo_id)],
        &kw,
    )
    .await;
    assert!(emp.is_some());

    // Command burst fallback when session.fleet_id is set and ship_id = 0 (no ship fitting)
    let burst_type_id = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 1770)
        .map(|t| t.type_id)
        .unwrap_or(42_526);
    let mod_id = 940_001_i64;
    let charge_id = 940_002_i64;
    let fake_ship_id = 940_003_i64;
    let wingmate_ship = 940_004_i64;
    let fleet_id = 8001_i64;
    let sys_id = sess.solar_system_id.unwrap_or(30_000_142);

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            mod_id,
            make_fighter_item(
                mod_id,
                burst_type_id,
                ceo_id,
                fake_ship_id,
                InventoryFlag::HiSlot0,
                1,
            ),
        );
        snap.items.insert(
            charge_id,
            make_fighter_item(
                charge_id,
                42_832,
                ceo_id,
                fake_ship_id,
                InventoryFlag::HiSlot0,
                100,
            ),
        );
        if let Some(m) = snap.characters.get_mut(&member_id) {
            m.solar_system_id = sys_id;
            m.station_id = None;
            m.active_ship_id = wingmate_ship;
        }
        let mut fl = FleetRecord {
            fleet_id,
            boss_id: ceo_id,
            ..Default::default()
        };
        fl.members.push(ceo_id);
        fl.members.push(member_id);
        fl.member_details.insert(
            ceo_id,
            FleetMemberRecord {
                character_id: ceo_id,
                role: FleetRole::FleetCommander,
                wing_id: None,
                squad_id: None,
                takes_fleet_warp: true,
                accepts_regroup: true,
                accepts_conduit_jumps: true,
                job: 2,
                role_name: "FC".into(),
                join_time_unix: 1000,
            },
        );
        fl.member_details.insert(
            member_id,
            FleetMemberRecord {
                character_id: member_id,
                role: FleetRole::SquadMember,
                wing_id: None,
                squad_id: None,
                takes_fleet_warp: true,
                accepts_regroup: true,
                accepts_conduit_jumps: true,
                job: 0,
                role_name: "Member".into(),
                join_time_unix: 1000,
            },
        );
        snap.fleets.insert(fleet_id, fl);
    });

    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_h
        .spawn_ball(Ball {
            id: fake_ship_id,
            type_id: 587,
            position: glam::DVec3::ZERO,
            velocity: glam::DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    let _ = sys_h
        .spawn_ball(Ball {
            id: wingmate_ship,
            type_id: 587,
            position: glam::DVec3::new(5_000.0, 0.0, 0.0),
            velocity: glam::DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    let mut burst_sess = sess.clone();
    burst_sess.fleet_id = Some(fleet_id);
    burst_sess.ship_id = Some(0);
    let burst_res = handle_corp_hangar_invbroker(
        &ctx,
        &mut burst_sess,
        None,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(mod_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(burst_res.dict_get("active"), Some(&PyRep::Bool(true)));
    assert_eq!(
        burst_res
            .dict_get("affectedMemberIDs")
            .and_then(|v| v.as_list())
            .map(|l| l.len()),
        Some(2)
    );

    // invbroker.List error branches: InvalidLocation and BoundObjectOwnershipViolation
    let mut no_loc_sess = sess.clone();
    no_loc_sess.station_id = None;
    no_loc_sess.structure_id = None;
    no_loc_sess.ship_id = None;
    let mut flag_kw = HashMap::new();
    flag_kw.insert("flag".into(), PyRep::Integer(116));
    let no_loc_err = handle_corp_hangar_invbroker(
        &ctx,
        &mut no_loc_sess,
        None,
        "invbroker",
        "List",
        &[],
        &flag_kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(no_loc_err.code, "InvalidLocation");

    let bound_other_station = BoundObjectEntry {
        bound_id: 1,
        session_id: sess.session_id,
        user_id: sess.user_id.unwrap_or(1),
        character_id: ceo_id,
        service: "invbroker".into(),
        object_id: 60_003_761,
        oid_str: "oid".into(),
        ref_id: 1,
    };
    let viol_err = handle_corp_hangar_invbroker(
        &ctx,
        &mut sess,
        Some(&bound_other_station),
        "invbroker",
        "List",
        &[PyRep::Integer(116)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(viol_err.code, "BoundObjectOwnershipViolation");

    // Seed BPC, singleton, and named items in division 1 (flag 116)
    let station_id = sess.station_id.unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        let mut bpc = make_fighter_item(940_010, 34, corp_id, station_id, InventoryFlag::CorpSAG1, 1);
        bpc.is_blueprint_copy = true;
        bpc.singleton = true;
        snap.items.insert(940_010, bpc);
        snap.item_custom_names.insert(940_010, "CustomBPC".into());
    });
    let listed = handle_corp_hangar_invbroker(
        &ctx,
        &mut sess,
        None,
        "invbroker",
        "List",
        &[],
        &flag_kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(listed, PyRep::ObjectEx { .. }));
}

#[test]
fn execute_actions_v24_member_batch_and_legacy_actions_branches() {
    let (ctx, mut sess, _ceo_id, member_id, corp_id, _ship_id) = setup_ctx();

    // Non-director cannot call ExecuteActions
    let mut grunt_sess = sess.clone();
    grunt_sess.character_id = Some(member_id);
    grunt_sess.corp_role = 0;
    assert_eq!(
        call_corp_reg(&ctx, &mut grunt_sess, "ExecuteActions", &[])
            .unwrap_err()
            .code,
        "CorpRoleDenied"
    );

    // Seed a title with hangar division role bit so titleMask grant triggers 24h pending_title_role_grants
    ctx.db.with_mut_snapshot(|snap| {
        let gov = snap.corp_governance.entry(corp_id).or_default();
        gov.titles
            .insert(1, ("Quartermaster".into(), 1u64 << 13)); // division 1 take role
    });

    // V24 member batch: non-member fails with NotCorpMember
    assert_eq!(
        call_corp_reg(
            &ctx,
            &mut sess,
            "ExecuteActions",
            &[
                PyRep::List(vec![PyRep::Integer(999_999)]),
                PyRep::List(vec![]),
            ],
        )
        .unwrap_err()
        .code,
        "NotCorpMember"
    );

    // V24 member batch: exercise all properties and verb modes (add/remove/set + integer 1/2/3)
    let actions = vec![
        PyRep::Tuple(vec![
            PyRep::String("add".into()),
            PyRep::String("roles".into()),
            PyRep::Integer((1i64 << 13) | 128), // delayed hangar role + immediate role
        ]),
        PyRep::Tuple(vec![
            PyRep::Integer(1),
            PyRep::String("grantableRoles".into()),
            PyRep::Integer(128),
        ]),
        PyRep::Tuple(vec![
            PyRep::String("set".into()),
            PyRep::String("rolesAtHQ".into()),
            PyRep::Integer(1i64 << 14),
        ]),
        PyRep::Tuple(vec![
            PyRep::Integer(3),
            PyRep::String("grantableRolesAtHQ".into()),
            PyRep::Integer(128),
        ]),
        PyRep::Tuple(vec![
            PyRep::String("grant".into()),
            PyRep::String("rolesAtBase".into()),
            PyRep::Integer(128),
        ]),
        PyRep::Tuple(vec![
            PyRep::String("give".into()),
            PyRep::String("grantableRolesAtBase".into()),
            PyRep::Integer(128),
        ]),
        PyRep::Tuple(vec![
            PyRep::String("add".into()),
            PyRep::String("rolesAtOther".into()),
            PyRep::Integer(128),
        ]),
        PyRep::Tuple(vec![
            PyRep::String("remove".into()),
            PyRep::String("grantableRolesAtOther".into()),
            PyRep::Integer(128),
        ]),
        PyRep::Tuple(vec![
            PyRep::String("set".into()),
            PyRep::String("titleMask".into()),
            PyRep::Integer(1),
        ]),
        PyRep::Tuple(vec![
            PyRep::String("set".into()),
            PyRep::String("accountKey".into()),
            PyRep::Integer(1003),
        ]),
        PyRep::Tuple(vec![
            PyRep::String("set".into()),
            PyRep::String("baseID".into()),
            PyRep::Integer(60_003_760),
        ]),
        PyRep::Tuple(vec![
            PyRep::String("set".into()),
            PyRep::String("divisionID".into()),
            PyRep::Integer(2),
        ]),
        PyRep::Tuple(vec![
            PyRep::String("set".into()),
            PyRep::String("squadronID".into()),
            PyRep::Integer(4),
        ]),
    ];
    let ok = call_corp_reg(
        &ctx,
        &mut sess,
        "ExecuteActions",
        &[
            PyRep::List(vec![PyRep::Integer(member_id)]),
            PyRep::List(actions),
        ],
    )
    .unwrap();
    assert_eq!(ok, PyRep::Bool(true));

    ctx.db.with_snapshot(|snap| {
        let gov = &snap.corp_governance[&corp_id];
        assert!(gov.pending_role_grants.contains_key(&member_id));
        assert!(gov.pending_scoped_role_grants.contains_key(&member_id));
        assert!(gov.pending_title_role_grants.contains_key(&member_id));
        assert_eq!(gov.member_account_keys.get(&member_id), Some(&1003));
        assert_eq!(gov.member_bases.get(&member_id), Some(&60_003_760));
        assert_eq!(gov.member_divisions.get(&member_id), Some(&(2, 4)));
    });

    // Non-CEO director cannot grant or revoke ROLE_DIRECTOR
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations
            .get_mut(&corp_id)
            .unwrap()
            .member_roles
            .insert(member_id, corp_roles::ROLE_DIRECTOR);
    });
    grunt_sess.corp_role = corp_roles::ROLE_DIRECTOR;
    assert_eq!(
        call_corp_reg(
            &ctx,
            &mut grunt_sess,
            "ExecuteActions",
            &[
                PyRep::List(vec![PyRep::Integer(member_id)]),
                PyRep::List(vec![PyRep::Tuple(vec![
                    PyRep::String("remove".into()),
                    PyRep::String("roles".into()),
                    PyRep::Integer(corp_roles::ROLE_DIRECTOR as i64),
                ])]),
            ],
        )
        .unwrap_err()
        .code,
        "CrpAccessDenied"
    );

    // Legacy ExecuteActions format: SetAccountKey, SetTitle, SetNote, SetFriendlyFire, custom string
    let legacy = call_corp_reg(
        &ctx,
        &mut sess,
        "ExecuteActions",
        &[
            PyRep::Integer(corp_id),
            PyRep::List(vec![
                PyRep::Tuple(vec![
                    PyRep::String("SetAccountKey".into()),
                    PyRep::Integer(member_id),
                    PyRep::Integer(1004),
                ]),
                PyRep::Tuple(vec![
                    PyRep::String("SetTitle".into()),
                    PyRep::Integer(member_id),
                    PyRep::Integer(3),
                ]),
                PyRep::Tuple(vec![
                    PyRep::String("SetNote".into()),
                    PyRep::Integer(member_id),
                    PyRep::String("Trusted logi".into()),
                ]),
                PyRep::Tuple(vec![
                    PyRep::String("SetFriendlyFire".into()),
                    PyRep::Bool(true),
                ]),
                PyRep::String("AuditAction".into()),
            ]),
        ],
    )
    .unwrap();
    assert_eq!(legacy, PyRep::Bool(true));
}

#[test]
fn set_account_key_timers_market_info_and_member_tracking_redaction() {
    let (ctx, mut sess, ceo_id, member_id, corp_id, _ship_id) = setup_ctx();
    let station_id = sess.station_id.unwrap();

    // Out-of-range account key -> InvalidAccountKey
    assert_eq!(
        call_corp_reg(&ctx, &mut sess, "SetAccountKey", &[PyRep::Integer(999)])
            .unwrap_err()
            .code,
        "InvalidAccountKey"
    );
    assert_eq!(
        call_corp_reg(&ctx, &mut sess, "SetAccountKey", &[PyRep::Integer(1007)])
            .unwrap_err()
            .code,
        "InvalidAccountKey"
    );

    // Non-CEO without accountant/division role denied on SetAccountKey and handle_get_corp_market_info
    let mut grunt_sess = sess.clone();
    grunt_sess.character_id = Some(member_id);
    grunt_sess.corp_role = 0;
    assert_eq!(
        call_corp_reg(&ctx, &mut grunt_sess, "SetAccountKey", &[PyRep::Integer(1001)])
            .unwrap_err()
            .code,
        "CorpRoleDenied"
    );
    assert!(handle_get_corp_market_info(&ctx, &mut grunt_sess, &[PyRep::Integer(corp_id)])
        .unwrap()
        .is_err());

    // SetAccountKey records a 10s session-change timer; immediate second call fails with SessionChangeInProgress
    assert!(call_corp_reg(&ctx, &mut sess, "SetAccountKey", &[PyRep::Integer(1001)]).is_ok());
    assert_eq!(
        call_corp_reg(&ctx, &mut sess, "SetAccountKey", &[PyRep::Integer(1002)])
            .unwrap_err()
            .code,
        "SessionChangeInProgress"
    );

    // Seed both a buy order and sell order for corp_id -> handle_get_corp_market_info aggregates min sell & max buy
    ctx.db.with_mut_snapshot(|snap| {
        snap.market_orders.insert(
            801,
            MarketOrderRecord {
                order_id: 801,
                type_id: 34,
                owner_id: corp_id,
                station_id,
                region_id: 10_000_002,
                price: IskCents::from_isk(12),
                vol_entered: 100,
                vol_remaining: 100,
                min_volume: 1,
                is_bid: false,
                is_npc: false,
                escrow: IskCents::ZERO,
            },
        );
        snap.market_orders.insert(
            802,
            MarketOrderRecord {
                order_id: 802,
                type_id: 34,
                owner_id: ceo_id,
                station_id,
                region_id: 10_000_002,
                price: IskCents::from_isk(9),
                vol_entered: 250,
                vol_remaining: 250,
                min_volume: 1,
                is_bid: true,
                is_npc: false,
                escrow: IskCents::from_isk(2250),
            },
        );
        snap.market_order_meta.insert(
            802,
            MarketOrderMeta {
                is_corporation: true,
                issued_unix: 1_700_000_000,
                duration_days: 90,
                range: "region".into(),
                wallet_division: Some(1),
                issued_by: Some(ceo_id),
                state: Some("open".into()),
            },
        );
    });
    let mkt = handle_get_corp_market_info(&ctx, &mut sess, &[PyRep::Integer(corp_id)])
        .unwrap()
        .unwrap();
    let PyRep::Rowset { rows, .. } = mkt else {
        panic!("expected Rowset");
    };
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][1], PyRep::Real(12.0));
    assert_eq!(rows[0][5], PyRep::Real(9.0));

    // GetMemberTrackingInfo requires director; GetMemberTrackingInfoSimple redacts locationID & shipTypeID for non-directors
    assert_eq!(
        handle_get_member_tracking_info(&ctx, &mut grunt_sess, "GetMemberTrackingInfo", &[])
            .unwrap()
            .unwrap_err()
            .code,
        "CorpRoleDenied"
    );
    let simple = handle_get_member_tracking_info(
        &ctx,
        &mut grunt_sess,
        "GetMemberTrackingInfoSimple",
        &[PyRep::Integer(ceo_id)],
    )
    .unwrap()
    .unwrap();
    let PyRep::Rowset { rows: s_rows, .. } = simple else {
        panic!("expected Rowset");
    };
    assert_eq!(s_rows.len(), 1);
    assert_eq!(s_rows[0][7], PyRep::None); // redacted locationID
    assert_eq!(s_rows[0][8], PyRep::None); // redacted shipTypeID
}
