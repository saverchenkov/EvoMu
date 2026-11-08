//! Semantic unit tests split from `fleet_social_corp_tail_dispatch_extra_unit_tests`.

#![forbid(unsafe_code)]

use super::fleet_social_corp_tail_dispatch_extra_unit_tests::*;
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::test_ctx_and_clock;
use eve_db::{
    corp_roles, CorporationRecord, InventoryFlag,
    InventoryItemRecord,
};
use eve_proto::PyRep;
use std::collections::{HashMap, HashSet};

#[test]
fn asset_inventory_which_filters_search_assets_and_aggression_timer_promotion() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let ceo_id = 100_200_001i64;
    let trader_id = 100_200_002i64;
    let plain_id = 100_200_003i64;
    let corp_id = 98_300_002i64;
    let station_id = *ctx.sde.stations.keys().next().unwrap_or(&60_003_760);
    let office_item_id = 930_001i64;

    ctx.db.with_mut_snapshot(|snap| {
        let base_ch = snap.characters.values().next().unwrap().clone();
        for cid in [ceo_id, trader_id, plain_id] {
            let mut ch = base_ch.clone();
            ch.character_id = cid;
            ch.corporation_id = corp_id;
            snap.characters.insert(cid, ch);
        }

        let mut roles = HashMap::new();
        roles.insert(trader_id, corp_roles::ROLE_TRADER);
        snap.corporations.insert(
            corp_id,
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = corp_id;
                __corp.name = "AssetSearchCorp".into();
                __corp.ticker = "ASCP".into();
                __corp.ceo_id = ceo_id;
                __corp.members = HashSet::from([ceo_id, trader_id, plain_id]);
                __corp.member_roles = roles;
                __corp },
        );
        snap.items.insert(
            office_item_id,
            InventoryItemRecord {
                item_id: office_item_id,
                type_id: 27,
                owner_id: corp_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        for (iid, tid, loc, flag, qty) in [
            (930_010, 34, station_id, InventoryFlag::Deliveries, 100),
            (930_011, 34, station_id, InventoryFlag::Locked, 200),
            (930_012, 34, station_id, InventoryFlag::Impounded, 300),
            (930_013, 34, office_item_id, InventoryFlag::CorpSAG2, 400),
            (930_014, 587, 30_000_142, InventoryFlag::Hangar, 1),
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: corp_id,
                    location_id: loc,
                    flag,
                    quantity: qty,
                    singleton: qty == 1,
                    ..Default::default()
                },
            );
        }
    });

    let mut plain_sess = make_session(plain_id, corp_id, 0);
    let mut trader_sess = make_session(trader_id, corp_id, corp_roles::ROLE_TRADER);
    let mut ceo_sess = make_session(ceo_id, corp_id, u64::MAX);

    let e_nocorp = call_mgr(
        &ctx,
        &mut ceo_sess,
        "GetAssetInventory",
        &[PyRep::Integer(999_777_666)],
        corp_id,
        ceo_id,
        1000,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(e_nocorp.code, "CorporationNotFound");

    let e_denied = call_mgr(
        &ctx,
        &mut plain_sess,
        "GetAssetInventory",
        &[PyRep::Integer(corp_id)],
        corp_id,
        plain_id,
        1000,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(e_denied.code, "CorpRoleDenied");

    for which in ["deliveries", "lockdown", "impounded", "junk", "offices", "other"] {
        let r = call_mgr(
            &ctx,
            &mut trader_sess,
            "GetAssetInventoryForLocation",
            &[
                PyRep::Integer(corp_id),
                PyRep::Integer(station_id),
                PyRep::String(which.into()),
            ],
            corp_id,
            trader_id,
            1000,
        )
        .unwrap()
        .unwrap();
        assert!(!matches!(r, PyRep::None));
    }

    let r_2arg = call_mgr(
        &ctx,
        &mut trader_sess,
        "GetAssetInventoryForLocation",
        &[PyRep::Integer(station_id), PyRep::String("offices".into())],
        corp_id,
        trader_id,
        1000,
    )
    .unwrap()
    .unwrap();
    assert!(!matches!(r_2arg, PyRep::None));

    let s_with_corp = call_mgr(
        &ctx,
        &mut trader_sess,
        "SearchAssets",
        &[
            PyRep::Integer(corp_id),
            PyRep::String("offices".into()),
            PyRep::Integer(4),
            PyRep::Integer(18),
            PyRep::Integer(34),
            PyRep::Integer(100),
        ],
        corp_id,
        trader_id,
        1000,
    )
    .unwrap()
    .unwrap();
    assert!(!matches!(s_with_corp, PyRep::None));

    let s_no_corp = call_mgr(
        &ctx,
        &mut trader_sess,
        "SearchAssets",
        &[PyRep::String("deliveries".into()), PyRep::Integer(0)],
        corp_id,
        trader_id,
        1000,
    )
    .unwrap()
    .unwrap();
    assert!(!matches!(s_no_corp, PyRep::None));

    let e_agg = call_reg(
        &ctx,
        &mut plain_sess,
        "RegisterNewAggressionSettings",
        &[PyRep::Bool(true)],
        corp_id,
        plain_id,
        1000,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(e_agg.code, "CorpRoleDenied");

    let reg_false = call_reg(
        &ctx,
        &mut ceo_sess,
        "RegisterNewAggressionSettings",
        &[PyRep::Bool(false)],
        corp_id,
        ceo_id,
        1_700_000_000,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        reg_false.dict_get("_disableAfter").and_then(|v| v.as_i64()),
        Some(1_700_086_400)
    );

    let _ = call_reg(
        &ctx,
        &mut ceo_sess,
        "RegisterNewAggressionSettings",
        &[PyRep::Integer(1)],
        corp_id,
        ceo_id,
        1_700_000_000,
    )
    .unwrap()
    .unwrap();

    let before_eff = call_reg(
        &ctx,
        &mut ceo_sess,
        "GetAggressionSettings",
        &[PyRep::Integer(corp_id)],
        corp_id,
        ceo_id,
        1_700_001_000,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        before_eff
            .dict_get("pendingFriendlyFireLegal")
            .and_then(|v| v.as_bool()),
        Some(true)
    );
    assert_eq!(
        before_eff.dict_get("changeAtTime").and_then(|v| v.as_i64()),
        Some(1_700_086_400)
    );

    let after_eff = call_mgr(
        &ctx,
        &mut ceo_sess,
        "GetAggressionSettingsForCorps",
        &[PyRep::List(vec![PyRep::Integer(corp_id), PyRep::Integer(999_999)])],
        corp_id,
        ceo_id,
        1_700_086_400,
    )
    .unwrap()
    .unwrap();
    if let PyRep::Dict(entries) = after_eff {
        assert_eq!(entries.len(), 2);
        let (_, kv) = &entries[0];
        assert_eq!(kv.dict_get("isFriendlyFireLegal").and_then(|v| v.as_bool()), Some(true));
        assert_eq!(kv.dict_get("pendingFriendlyFireLegal"), Some(&PyRep::None));
    }
}
