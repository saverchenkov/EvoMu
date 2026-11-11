#![forbid(unsafe_code)]

use super::*;
use crate::router::ServerContext;
use eve_db::corp_roles::{ROLE_ACCOUNT_CAN_TAKE, ROLE_DELIVERIES_HANGAR_TAKE};
use eve_db::{
    InventoryFlag, InventoryItemRecord, IskCents, LoyaltyStoreOfferItemRecord,
    LoyaltyStoreOfferRecord, LpRewardLogEntry, corp_roles,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(eve_db::WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

fn docked_sess(char_id: i64, corp_id: i64, station_id: i64, role: u64) -> SessionState {
    let mut s = SessionState::new(8201);
    s.user_id = Some(char_id as u64);
    s.character_id = Some(char_id);
    s.corporation_id = Some(corp_id);
    s.station_id = Some(station_id);
    s.solar_system_id = Some(30_000_142);
    s.corp_role = role;
    s
}

#[test]
fn unauthenticated_errors_unknown_route_and_delayed_rewards_tuple() {
    let ctx = test_ctx();
    let mut unauth = SessionState::new(8202);
    let kw = HashMap::new();

    for (svc, meth) in [
        ("LPStoreMgr", "GetAvailableOffersFromCorp"),
        ("LPStoreMgr", "TakeOfferForCharacter"),
        ("LPStoreMgr", "TakeOfferForCorporation"),
        ("LPSvc", "GetAllMyCharacterWalletLPBalances"),
        ("LPSvc", "GetAllMyCorporationWalletLPBalances"),
        ("LPSvc", "TransferLPFromMyWalletToOtherCorp"),
        ("LPSvc", "TransferLPFromMyCorpWalletToOtherCorp"),
        ("LPSvc", "ExchangeConcordLP"),
        ("rewardMgr", "GetRewardLPLogs"),
        ("rewardMgr", "GetDelayedRewardsByGroupIDs"),
        ("RWManager", "solarsystem_contains_rw_instances"),
        ("RWManager", "get_closest_rw_stations"),
    ] {
        let res = handle(&ctx, &mut unauth, svc, meth, &[], &kw).expect("route exists");
        assert!(res.is_err(), "{svc}.{meth} must require character");
    }

    assert!(handle(&ctx, &mut unauth, "LPSvc", "GetRewardLPLogs", &[], &kw).is_none());

    // GetDelayedRewardsByGroupIDs with Tuple matching corp_id and log_id
    let cid = 90_000_001_i64;
    let mut auth = docked_sess(cid, 98_000_001, 60_003_760, 0);
    ctx.db.with_mut_snapshot(|snap| {
        snap.char_lp_logs.entry(cid).or_default().push(LpRewardLogEntry {
            log_id: 4242,
            corp_id: 1_000_035,
            amount: 500,
            reason: "Site".into(),
            timestamp_unix: ctx.now_unix(),
        });
    });
    let rep = handle(
        &ctx,
        &mut auth,
        "rewardMgr",
        "GetDelayedRewardsByGroupIDs",
        &[PyRep::Tuple(vec![PyRep::Integer(1_000_035), PyRep::Integer(4242)])],
        &kw,
    )
    .unwrap()
    .unwrap();
    match rep {
        PyRep::Dict(entries) => assert_eq!(entries.len(), 2),
        other => panic!("expected Dict, got {other:?}"),
    }
}

#[test]
fn take_offer_overflow_standing_bpc_corp_roles_and_multi_stack_tradein() {
    let ctx = test_ctx();
    let cid = 90_000_001_i64;
    let corp_id = 98_000_001_i64;
    let station_id = 60_003_760_i64;
    let npc_corp = 1_000_035_i64;
    let kw = HashMap::new();

    let bpo_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.category_id == 9)
        .map(|t| t.type_id)
        .unwrap_or(681);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.station_id = Some(station_id);
            ch.corporation_id = corp_id;
            ch.balance = IskCents(1_000_000_000);
            ch.loyalty_points.insert(npc_corp, 500_000);
        }
        snap.loyalty_store_offers.entry(npc_corp).or_default().extend([
            LoyaltyStoreOfferRecord {
                offer_id: 70_001,
                type_id: bpo_tid,
                quantity: 1,
                lp_cost: i64::MAX / 2,
                isk_cost: 100,
                ak_cost: None,
                required_items: vec![LoyaltyStoreOfferItemRecord { type_id: 34, quantity: 1 }],
            },
            LoyaltyStoreOfferRecord {
                offer_id: 70_002,
                type_id: bpo_tid,
                quantity: 1,
                lp_cost: 10,
                isk_cost: 100,
                ak_cost: None,
                required_items: vec![
                    LoyaltyStoreOfferItemRecord { type_id: 34, quantity: 3 },
                    LoyaltyStoreOfferItemRecord { type_id: 34, quantity: 2 },
                ],
            },
            LoyaltyStoreOfferRecord {
                offer_id: 70_003,
                type_id: 34,
                quantity: 1,
                lp_cost: 10,
                isk_cost: 100,
                ak_cost: Some(50),
                required_items: Vec::new(),
            },
        ]);
        for (iid, qty) in [(9_100_001_i64, 4), (9_100_002_i64, 3)] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: 34,
                    owner_id: cid,
                    location_id: station_id,
                    flag: InventoryFlag::Hangar,
                    quantity: qty,
                    singleton: false,
                    ..Default::default()
                },
            );
        }
    });

    let mut sess = docked_sess(cid, corp_id, station_id, 0);

    // 1. checked_mul overflow -> InvalidQuantity
    let err_ovf = handle(
        &ctx,
        &mut sess,
        "LPStoreMgr",
        "TakeOfferForCharacter",
        &[PyRep::Integer(npc_corp), PyRep::Integer(70_001), PyRep::Integer(4)],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_ovf.code, "InvalidQuantity");

    // 2. Blueprint qty > 1000 -> InvalidQuantity
    let err_bpc_cap = handle(
        &ctx,
        &mut sess,
        "LPStoreMgr",
        "TakeOfferForCharacter",
        &[PyRep::Integer(npc_corp), PyRep::Integer(70_002), PyRep::Integer(1_001)],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_bpc_cap.code, "InvalidQuantity");

    // 3. Multi-BPC tradein: duplicate req_items 3+2=5 removes stack 4 and decrements 3 -> 2
    let ok_bpc = handle(
        &ctx,
        &mut sess,
        "LPStoreMgr",
        "TakeOfferForCharacter",
        &[PyRep::Integer(npc_corp), PyRep::Integer(70_002), PyRep::Integer(1)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(ok_bpc.dict_get("offerID").and_then(PyRep::as_i64), Some(70_002));
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&9_100_001));
        assert_eq!(snap.items.get(&9_100_002).map(|i| i.quantity), Some(2));
        let minted_bpcs = snap
            .items
            .values()
            .filter(|i| i.type_id == bpo_tid && i.is_blueprint_copy)
            .count();
        assert_eq!(minted_bpcs, 1);
    });

    // 4. ak_cost > 0 fails without EverMarks, succeeds once credited
    let err_ak = handle(
        &ctx,
        &mut sess,
        "LPStoreMgr",
        "TakeOfferForCharacter",
        &[PyRep::Integer(npc_corp), PyRep::Integer(70_003), PyRep::Integer(1)],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_ak.code, "InsufficientAnalysisKredits");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.loyalty_points.insert(1_000_419, 100);
        }
        snap.seasons_and_campaigns
            .project_discovery_players
            .entry(cid)
            .or_default()
            .analysis_kredits = 100;
        if let Some(c) = snap.corporations.get_mut(&corp_id) {
            c.ceo_id = cid + 999;
            c.member_roles.clear();
            c.member_roles.insert(cid, ROLE_ACCOUNT_CAN_TAKE[0]);
            c.set_division_balance(1, IskCents(1_000_000));
        }
        snap.corp_lp_balances.insert((corp_id, npc_corp), 100);
    });
    assert!(handle(
        &ctx,
        &mut sess,
        "LPStoreMgr",
        "TakeOfferForCharacter",
        &[PyRep::Integer(npc_corp), PyRep::Integer(70_003), PyRep::Integer(1)],
        &kw,
    )
    .unwrap()
    .is_ok());

    // 5. Corp offer: missing ROLE_DELIVERIES_HANGAR_TAKE -> InsufficientCorporationRole; missing ROLE_ACCOUNT_CAN_TAKE[0] -> InsufficientCorporationRole
    let mut no_deliveries = docked_sess(cid, corp_id, station_id, ROLE_ACCOUNT_CAN_TAKE[0]);
    assert_eq!(
        handle(
            &ctx,
            &mut no_deliveries,
            "LPStoreMgr",
            "TakeOfferForCorporation",
            &[PyRep::Integer(npc_corp), PyRep::Integer(70_003), PyRep::Integer(1)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InsufficientCorporationRole"
    );
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.corporations.get_mut(&corp_id) {
            c.member_roles.insert(cid, ROLE_DELIVERIES_HANGAR_TAKE);
        }
    });
    let mut no_account = docked_sess(cid, corp_id, station_id, ROLE_DELIVERIES_HANGAR_TAKE);
    assert_eq!(
        handle(
            &ctx,
            &mut no_account,
            "LPStoreMgr",
            "TakeOfferForCorporation",
            &[PyRep::Integer(npc_corp), PyRep::Integer(70_003), PyRep::Integer(1)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InsufficientCorporationRole"
    );

    // 6. Triglavian/EDENCOM negative standing blocks offer
    let trig_corp = 1_000_298_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.loyalty_store_offers.entry(trig_corp).or_default().push(
            LoyaltyStoreOfferRecord {
                offer_id: 70_004,
                type_id: 34,
                quantity: 1,
                lp_cost: 5,
                isk_cost: 0,
                ak_cost: None,
                required_items: Vec::new(),
            },
        );
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.loyalty_points.insert(trig_corp, 100);
            ch.standings.insert(trig_corp, -2.0);
        }
    });
    // Dock at a station hosted by Triglavian Collective (1_000_298 -> 60_000_391) so station hosting passes and standing check fires.
    sess.station_id = Some(60_000_391);
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "LPStoreMgr",
            "TakeOfferForCharacter",
            &[PyRep::Integer(trig_corp), PyRep::Integer(70_004), PyRep::Integer(1)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InsufficientStanding"
    );
}

#[test]
fn lp_transfer_concord_exchange_and_station_service_errors() {
    let ctx = test_ctx();
    let cid = 90_000_001_i64;
    let corp_id = 98_000_001_i64;
    let kw = HashMap::new();
    let mut sess = docked_sess(cid, corp_id, 60_003_760, corp_roles::ROLE_DIRECTOR);

    sess.station_id = None;
    sess.structure_id = Some(1_000_000_000_001);
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "LPStoreMgr",
            "ExchangeConcordLP",
            &[PyRep::Integer(1_000_035), PyRep::Integer(100)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "MustBeDockedInStation"
    );
    sess.structure_id = None;
    sess.station_id = Some(60_003_760);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.loyalty_points.remove(&1_000_035);
        }
    });

    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "LPSvc",
            "ExchangeConcordLP",
            &[PyRep::Integer(1_000_035), PyRep::Integer(100_000)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "NoExistingTargetCorpLP"
    );
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.loyalty_points.insert(1_000_035, 10);
        }
        let mut target_corp = eve_db::CorporationRecord::default();
        target_corp.corporation_id = 98_000_002;
        target_corp.name = "Target Corp".into();
        target_corp.ticker = "TCRP".into();
        target_corp.ensure_wallet_divisions();
        snap.corporations.insert(98_000_002, target_corp);
    });
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "LPSvc",
            "ExchangeConcordLP",
            &[PyRep::Integer(1_000_035), PyRep::Integer(100_000)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InsufficientLoyaltyPoints"
    );

    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "LPSvc",
            "TransferLPFromMyWalletToOtherCorp",
            &[PyRep::Integer(98_000_002), PyRep::Integer(1_000_035), PyRep::Integer(999_999)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InsufficientLoyaltyPoints"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "LPSvc",
            "TransferLPFromMyCorpWalletToOtherCorp",
            &[PyRep::Integer(98_000_002), PyRep::Integer(1_000_035), PyRep::Integer(999_999)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InsufficientLoyaltyPoints"
    );

    if let Some((&no_lp_station, _)) = ctx.sde.stations.iter().find(|(_, st)| {
        !ctx.sde
            .station_operations
            .get(&st.operation_id)
            .is_some_and(|op| op.services.contains(&25))
    }) {
        sess.station_id = Some(no_lp_station);
        assert_eq!(
            handle(
                &ctx,
                &mut sess,
                "LPStoreMgr",
                "GetAvailableOffersFromCorp",
                &[PyRep::Integer(1_000_035)],
                &kw,
            )
            .unwrap()
            .unwrap_err()
            .code,
            "StationServiceUnavailable"
        );
    }

    let hosted = station_hosted_lp_corps(&ctx.sde, 60_003_760);
    assert!(!hosted.is_empty());
}

#[test]
fn lp_store_mission_tax_dedup_tradein_exclusions_roles_and_transfer_concord_happy_paths() {
    let ctx = test_ctx();
    let cid = 90_000_001_i64;
    let corp_id = 98_000_001_i64;
    let dest_corp_id = 98_000_002_i64;
    let station_id = 60_003_760_i64;
    let npc_corp = 1_000_035_i64;
    let kw = HashMap::new();
    let now = ctx.now_unix();

    ctx.db.with_mut_snapshot(|snap| {
        let mut corp_a = snap.corporations.get(&corp_id).cloned().unwrap_or_else(|| {
            let mut c = eve_db::CorporationRecord::default();
            c.corporation_id = corp_id;
            c
        });
        corp_a.tax_rate = 0.10;
        corp_a.ceo_id = cid + 999; // ensure non-CEO role matrix works strictly from corp_roles
        corp_a.set_division_balance(1, IskCents::ZERO);
        snap.corporations.insert(corp_id, corp_a);

        let mut corp_b = eve_db::CorporationRecord::default();
        corp_b.corporation_id = dest_corp_id;
        corp_b.ceo_id = cid + 998;
        snap.corporations.insert(dest_corp_id, corp_b);

        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.station_id = Some(station_id);
            ch.structure_id = None;
            ch.corporation_id = corp_id;
            ch.balance = IskCents::from_isk(1_000_000);
            ch.loyalty_points.insert(npc_corp, 5_000);
            ch.loyalty_points.insert(CONCORD_CORP_ID, 200);
        }

        // 1. record_lp_reward_log mission tax + dedup via mission_taxed_journal
        snap.wallet_journal.push(eve_db::WalletJournalEntry {
            entry_id: 88_001,
            character_id: cid,
            entry_type: "agent_mission_reward".into(),
            amount: IskCents::from_isk(100_000),
            balance_after: IskCents::from_isk(1_000_000),
            reason: "Agent mission reward".into(),
        });
        record_lp_reward_log(snap, cid, npc_corp, 500, "Mission reward".into(), now);
        assert_eq!(snap.characters[&cid].balance, IskCents::from_isk(990_000));
        assert_eq!(snap.corporations[&corp_id].division_balance(1), IskCents::from_isk(10_000));
        let tax_entries_after_first = snap
            .wallet_journal
            .iter()
            .filter(|e| e.character_id == cid && e.entry_type == "agent_mission_reward_corporation_tax")
            .count();
        assert_eq!(tax_entries_after_first, 1);

        // Calling a second time with same journal entry must NOT double-tax
        record_lp_reward_log(snap, cid, npc_corp, 250, "Mission bonus".into(), now + 1);
        assert_eq!(snap.characters[&cid].balance, IskCents::from_isk(990_000));
        assert_eq!(snap.corporations[&corp_id].division_balance(1), IskCents::from_isk(10_000));
        assert_eq!(
            snap.wallet_journal
                .iter()
                .filter(|e| e.character_id == cid && e.entry_type == "agent_mission_reward_corporation_tax")
                .count(),
            1
        );
        assert!(snap.char_lp_logs[&cid].len() >= 2);

        // Offer requiring 1x Tritanium (34), granting 3x Tritanium (34) per qty
        snap.loyalty_store_offers.entry(npc_corp).or_default().push(
            LoyaltyStoreOfferRecord {
                offer_id: 77_101,
                type_id: 34,
                quantity: 3,
                lp_cost: 100,
                isk_cost: 1_000,
                ak_cost: None,
                required_items: vec![LoyaltyStoreOfferItemRecord { type_id: 34, quantity: 1 }],
            },
        );
        // Excluded trade-in stacks: wrong location, wrong flag, fitted slot, contract-locked, courier_cargo
        snap.items.retain(|_, i| !(i.owner_id == cid && i.type_id == 34 && i.location_id == station_id));
        snap.items.insert(
            9_770_001,
            InventoryItemRecord {
                item_id: 9_770_001,
                type_id: 34,
                owner_id: cid,
                location_id: 60_003_761,
                flag: InventoryFlag::Hangar,
                quantity: 10,
                ..Default::default()
            },
        );
        snap.items.insert(
            9_770_002,
            InventoryItemRecord {
                item_id: 9_770_002,
                type_id: 34,
                owner_id: cid,
                location_id: station_id,
                flag: InventoryFlag::Cargo,
                quantity: 10,
                ..Default::default()
            },
        );
        snap.items.insert(
            9_770_003,
            InventoryItemRecord {
                item_id: 9_770_003,
                type_id: 34,
                owner_id: cid,
                location_id: station_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 10,
                ..Default::default()
            },
        );
        snap.items.insert(
            9_770_004,
            InventoryItemRecord {
                item_id: 9_770_004,
                type_id: 34,
                owner_id: cid,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 10,
                ..Default::default()
            },
        );
        snap.contracts.insert(
            9_770_901,
            eve_db::ContractRecord {
                contract_id: 9_770_901,
                issuer_id: cid,
                contract_type: "ItemExchange".into(),
                status: "Outstanding".into(),
                item_ids: vec![9_770_004],
                ..Default::default()
            },
        );
        snap.items.insert(
            9_770_005,
            InventoryItemRecord {
                item_id: 9_770_005,
                type_id: 34,
                owner_id: cid,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 10,
                ..Default::default()
            },
        );
        snap.item_custom_names.insert(9_770_005, "courier_cargo:pkg1".into());
    });

    let mut char_sess = docked_sess(cid, corp_id, station_id, 0);
    assert_eq!(
        handle(
            &ctx,
            &mut char_sess,
            "LPStoreMgr",
            "TakeOfferForCharacter",
            &[PyRep::Integer(npc_corp), PyRep::Integer(77_101), PyRep::Integer(2)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "MissingRequiredItems"
    );

    // Add valid trade-in stack of 2x Tritanium -> 3-arg TakeOfferForCharacter(npc_corp, 77_101, qty=2)
    // consumes 200 LP, 2_000 ISK, 2x Tritanium and mints 6x Tritanium!
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            9_770_006,
            InventoryItemRecord {
                item_id: 9_770_006,
                type_id: 34,
                owner_id: cid,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 2,
                ..Default::default()
            },
        );
    });
    let take_rep = handle(
        &ctx,
        &mut char_sess,
        "LPStoreMgr",
        "TakeOfferForCharacter",
        &[PyRep::Integer(npc_corp), PyRep::Integer(77_101), PyRep::Integer(2)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(take_rep.dict_get("offerID").and_then(|v| v.as_i64()), Some(77_101));
    assert_eq!(take_rep.dict_get("corpID").and_then(|v| v.as_i64()), Some(npc_corp));
    assert_eq!(take_rep.dict_get("deliveredTypeID").and_then(|v| v.as_i64()), Some(34));
    assert_eq!(take_rep.dict_get("quantity").and_then(|v| v.as_i64()), Some(6));
    assert_eq!(take_rep.dict_get("remainingLP").and_then(|v| v.as_i64()), Some(4_800));
    assert_eq!(take_rep.dict_get("balanceAfterISK").and_then(|v| v.as_f64()), Some(988_000.0));
    assert!(take_rep
        .dict_get("deliveredItemID")
        .and_then(|v| v.as_i64())
        .is_some_and(|id| id >= 100_000_000));
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&9_770_006));
        assert_eq!(snap.characters[&cid].loyalty_points[&npc_corp], 4_800);
        assert_eq!(snap.characters[&cid].balance, IskCents::from_isk(988_000));
        let minted_qty: i32 = snap
            .items
            .values()
            .filter(|it| {
                it.owner_id == cid
                    && it.type_id == 34
                    && it.location_id == station_id
                    && it.flag == InventoryFlag::Hangar
                    && it.item_id >= 100_000_000
            })
            .map(|it| it.quantity)
            .sum();
        assert_eq!(minted_qty, 6);
    });

    // 2. GetAllMyCharacterWalletLPBalances returns non-empty list
    let PyRep::List(char_bals) = handle(&ctx, &mut char_sess, "LPSvc", "GetAllMyCharacterWalletLPBalances", &[], &kw)
        .unwrap()
        .unwrap()
    else {
        panic!("expected list from GetAllMyCharacterWalletLPBalances");
    };
    assert!(
        char_bals.iter().any(|row| matches!(row, PyRep::Tuple(t) if t.as_slice() == [PyRep::Integer(npc_corp), PyRep::Integer(4_800)])),
        "expected (1_000_035, 4800) in {char_bals:?}"
    );

    // 3. TransferLPFromMyWalletToOtherCorp: invalid npc_corp_id <= 0, then happy path + corp balance check
    assert_eq!(
        handle(
            &ctx,
            &mut char_sess,
            "LPSvc",
            "TransferLPFromMyWalletToOtherCorp",
            &[PyRep::Integer(dest_corp_id), PyRep::Integer(0), PyRep::Integer(100)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidCorpID"
    );
    let xfer_char = handle(
        &ctx,
        &mut char_sess,
        "LPSvc",
        "TransferLPFromMyWalletToOtherCorp",
        &[PyRep::Integer(dest_corp_id), PyRep::Integer(npc_corp), PyRep::Integer(1_000)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(xfer_char.dict_get("transferredLP"), Some(&PyRep::Integer(1_000)));
    assert_eq!(xfer_char.dict_get("senderRemainingLP"), Some(&PyRep::Integer(3_800)));
    assert_eq!(xfer_char.dict_get("targetCorpLPBalance"), Some(&PyRep::Integer(1_000)));

    // 4. GetAllMyCorporationWalletLPBalances role matrix & cross-corp rejection + TransferLPFromMyCorpWalletToOtherCorp
    ctx.db.with_mut_snapshot(|snap| {
        let mut bals = load_corp_lp_balances(snap, corp_id);
        bals.insert(npc_corp, 2_000);
        save_corp_lp_balances(snap, corp_id, &bals);
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.set_division_balance(1, IskCents::from_isk(100_000));
        }
        // Corp-owned trade-in stack (`owner_id = corp_id != char_id`) in CorpDeliveries
        snap.items.insert(
            9_770_020,
            InventoryItemRecord {
                item_id: 9_770_020,
                type_id: 34,
                owner_id: corp_id,
                location_id: station_id,
                flag: InventoryFlag::CorpDeliveries,
                quantity: 1,
                ..Default::default()
            },
        );
    });

    // Unprivileged -> CrpAccessDenied; wrong target corp -> CrpAccessDenied
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.corporations.get_mut(&corp_id) {
            c.member_roles.insert(cid, 0);
        }
        snap.corp_role_assignments.remove(&(corp_id, cid));
        if let Some(g) = snap.corp_governance.get_mut(&corp_id) {
            g.member_titles.remove(&cid);
        }
    });
    assert_eq!(
        handle(&ctx, &mut char_sess, "LPSvc", "GetAllMyCorporationWalletLPBalances", &[], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "CrpAccessDenied"
    );
    let mut dir_sess = docked_sess(cid, corp_id, station_id, corp_roles::ROLE_DIRECTOR);
    assert_eq!(
        handle(
            &ctx,
            &mut dir_sess,
            "LPSvc",
            "GetAllMyCorporationWalletLPBalances",
            &[PyRep::Integer(dest_corp_id)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CrpAccessDenied"
    );

    // Non-CEO with ROLE_BRAND_MANAGER succeeds; CEO (without corp_role bitmask) succeeds
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.corporations.get_mut(&corp_id) {
            c.member_roles.insert(cid, corp_roles::ROLE_BRAND_MANAGER);
        }
    });
    let mut brand_sess = docked_sess(cid, corp_id, station_id, corp_roles::ROLE_BRAND_MANAGER);
    let PyRep::List(corp_bals) = handle(
        &ctx,
        &mut brand_sess,
        "LPSvc",
        "GetAllMyCorporationWalletLPBalances",
        &[],
        &kw,
    )
    .unwrap()
    .unwrap()
    else {
        panic!("expected list from GetAllMyCorporationWalletLPBalances");
    };
    assert_eq!(corp_bals, vec![PyRep::Tuple(vec![PyRep::Integer(npc_corp), PyRep::Integer(2_000)])]);

    // Corp take with corp-owned trade-in item (`it_owner != char_id`)
    let corp_take_roles = corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_BRAND_MANAGER | ROLE_ACCOUNT_CAN_TAKE[0] | ROLE_DELIVERIES_HANGAR_TAKE;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.corporations.get_mut(&corp_id) {
            c.member_roles.insert(cid, corp_take_roles);
        }
    });
    let mut corp_take_sess = docked_sess(
        cid,
        corp_id,
        station_id,
        corp_take_roles,
    );
    let corp_take_rep = handle(
        &ctx,
        &mut corp_take_sess,
        "LPStoreMgr",
        "TakeOfferForCorporation",
        &[PyRep::Integer(npc_corp), PyRep::Integer(77_101), PyRep::Integer(1)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(corp_take_rep.dict_get("offerID").and_then(|v| v.as_i64()), Some(77_101));
    assert_eq!(corp_take_rep.dict_get("corpID").and_then(|v| v.as_i64()), Some(npc_corp));
    assert_eq!(corp_take_rep.dict_get("deliveredTypeID").and_then(|v| v.as_i64()), Some(34));
    assert_eq!(corp_take_rep.dict_get("quantity").and_then(|v| v.as_i64()), Some(3));
    assert_eq!(corp_take_rep.dict_get("remainingLP").and_then(|v| v.as_i64()), Some(1_900));
    assert_eq!(corp_take_rep.dict_get("balanceAfterISK").and_then(|v| v.as_f64()), Some(99_000.0));
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&9_770_020));
    });

    // TransferLPFromMyCorpWalletToOtherCorp (1_900 corp LP remaining -> transfer 400 to dest_corp_id)
    let xfer_corp = handle(
        &ctx,
        &mut corp_take_sess,
        "LPSvc",
        "TransferLPFromMyCorpWalletToOtherCorp",
        &[PyRep::Integer(dest_corp_id), PyRep::Integer(npc_corp), PyRep::Integer(400)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(xfer_corp.dict_get("transferredLP"), Some(&PyRep::Integer(400)));
    assert_eq!(xfer_corp.dict_get("senderRemainingLP"), Some(&PyRep::Integer(1_500)));
    assert_eq!(xfer_corp.dict_get("targetCorpLPBalance"), Some(&PyRep::Integer(1_400)));

    // 5. ExchangeConcordLP happy path (100 CONCORD -> 80 Caldari Navy LP at 0.8 rate)
    let exch = handle(
        &ctx,
        &mut char_sess,
        "LPSvc",
        "ExchangeConcordLP",
        &[PyRep::Integer(npc_corp), PyRep::Integer(100)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(exch.dict_get("targetCorpID"), Some(&PyRep::Integer(npc_corp)));
    assert_eq!(exch.dict_get("concordLPSpent"), Some(&PyRep::Integer(100)));
    assert_eq!(exch.dict_get("convertedLP"), Some(&PyRep::Integer(80)));
    assert_eq!(exch.dict_get("remainingConcordLP"), Some(&PyRep::Integer(100)));
    assert_eq!(exch.dict_get("newTargetCorpLP"), Some(&PyRep::Integer(3_880)));
}
