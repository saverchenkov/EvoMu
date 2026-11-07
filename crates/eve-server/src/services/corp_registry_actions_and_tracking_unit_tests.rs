//! Semantic unit tests split from `corp_registry_ops_extra_unit_tests`.

#![forbid(unsafe_code)]

use super::corp_registry_ops_extra_unit_tests::*;
use crate::services::corp_registry_ops::*;
use eve_db::snapshot_ext::MarketOrderMeta;
use eve_db::{
    corp_roles,
    IskCents, MarketOrderRecord,
};
use eve_proto::PyRep;

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
