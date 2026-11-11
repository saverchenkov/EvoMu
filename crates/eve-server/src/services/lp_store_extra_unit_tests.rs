//! Extra unit and mutation-killing tests for `services::lp_store`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::lp_store::*;
use crate::services::pve_special_sites::{with_pve_special_reg, IncursionLPLogEntry};
use eve_db::{
    corp_roles, CorpMemberRoleAssignment, CorporationRecord, InventoryFlag, InventoryItemRecord,
    IskCents, WalletJournalEntry, WorldRepository,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::{NpcCorporationInfo, SdeRegistry};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn make_ctx() -> ServerContext {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let _ = db.acquire_online_lock(1, 501, Some(90_000_001), true, 1_775_400_000);
    ServerContext::with_sde_and_repository(shared_sde(), db)
}

fn make_session(char_id: i64, corp_id: i64, station_id: Option<i64>) -> SessionState {
    let mut s = SessionState::new(9401);
    s.user_id = Some(1);
    s.character_id = Some(char_id);
    s.corporation_id = Some(corp_id);
    s.station_id = station_id;
    s.solar_system_id = Some(30_000_142);
    s
}

fn call_lp(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    handle(ctx, sess, svc, method, args, &HashMap::new()).unwrap()
}

#[test]
fn mission_reward_corp_tax_player_vs_npc_and_station_hosted_corps() {
    let mut ctx = make_ctx();
    let char_id = 90_000_001_i64;
    let player_corp = 98_700_001_i64;
    let npc_corp = 1_000_035_i64;
    let fw_militia_corp = 1_000_180_i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = player_corp;
            ch.balance = IskCents::from_isk(500_000);
        }
        let mut corp = { let mut __corp = CorporationRecord::default();
            __corp.corporation_id = player_corp;
            __corp.name = "Tax Corp".into();
            __corp.ticker = "TAXC".into();
            __corp.ceo_id = char_id;
            __corp.tax_rate = 0.10;
            __corp };
        corp.ensure_wallet_divisions();
        snap.corporations.insert(player_corp, corp);
        for (eid, etype, amt, bal) in [
            (7001, "agent_mission_reward", 50_000, 450_000),
            (7002, "agent_mission_time_bonus_reward", 30_000, 480_000),
        ] {
            snap.wallet_journal.push(WalletJournalEntry {
                entry_id: eid, character_id: char_id, entry_type: etype.into(),
                amount: IskCents::from_isk(amt), balance_after: IskCents::from_isk(bal),
                reason: "Mission reward".into(),
            });
        }
        record_lp_reward_log(snap, char_id, npc_corp, 500, "Mission Completed".into(), 1000);
        record_lp_reward_log(snap, char_id, npc_corp, 500, "Mission Completed".into(), 1001);

        assert_eq!(snap.characters[&char_id].balance, IskCents::from_isk(492_000));
        assert_eq!(snap.corporations[&player_corp].division_balance(1), IskCents::from_isk(8_000));
        assert!(snap.wallet_journal.iter().any(|e| e.entry_type == "agent_mission_reward_corporation_tax"));
        assert!(snap.wallet_journal.iter().any(|e| e.entry_type == "agent_mission_time_bonus_reward_corporation_tax"));

        // NPC corp (11% tax) ignores < 100k ISK rewards and taxes >= 100k ISK rewards
        snap.characters.get_mut(&char_id).unwrap().corporation_id = npc_corp;
        for (eid, amt) in [(7003, 90_000), (7004, 200_000)] {
            snap.wallet_journal.push(WalletJournalEntry {
                entry_id: eid, character_id: char_id, entry_type: "agent_mission_reward".into(),
                amount: IskCents::from_isk(amt), balance_after: IskCents::from_isk(600_000),
                reason: "NPC mission".into(),
            });
        }
        record_lp_reward_log(snap, char_id, npc_corp, 100, "Mission Big".into(), 1002);
        assert_eq!(snap.characters[&char_id].balance, IskCents::from_isk(470_000));

        // FW militia NPC corp has 0% tax
        snap.characters.get_mut(&char_id).unwrap().corporation_id = fw_militia_corp;
        snap.wallet_journal.push(WalletJournalEntry {
            entry_id: 7005, character_id: char_id, entry_type: "agent_mission_reward".into(),
            amount: IskCents::from_isk(300_000), balance_after: IskCents::from_isk(770_000),
            reason: "FW mission".into(),
        });
        record_lp_reward_log(snap, char_id, fw_militia_corp, 100, "Mission FW".into(), 1003);
        assert_eq!(snap.characters[&char_id].balance, IskCents::from_isk(470_000));
    });

    assert!(station_hosted_lp_corps(&ctx.sde, 1_040_000_000_001).is_empty());
    assert!(station_hosted_lp_corps(&ctx.sde, 60_003_760).contains(&1_000_035));
    let sde_mut = Arc::make_mut(&mut ctx.sde);
    if let Some(jita) = sde_mut.stations.get(&60_003_760).cloned() {
        let mut fulcrum = jita;
        fulcrum.station_id = 60_015_187;
        sde_mut.stations.insert(60_015_187, fulcrum);
        let hosted = station_hosted_lp_corps(&ctx.sde, 60_015_187);
        for pirate_cid in [1_000_138, 1_000_436, 1_000_127, 1_000_437] {
            assert!(hosted.contains(&pirate_cid));
        }
    }
}

#[test]
fn take_offer_standing_militia_locked_tradein_and_corp_role_guards() {
    let ctx = make_ctx();
    let char_id = 90_000_001_i64;
    let player_corp = 98_700_002_i64;
    let station_id = 60_003_760_i64;
    let npc_corp = 1_000_035_i64;
    let mut sess = make_session(char_id, player_corp, Some(station_id));

    for bad_q in [0, i64::from(i32::MAX) + 1] {
        assert_eq!(
            call_lp(&ctx, &mut sess, "LPStoreMgr", "TakeOfferForCharacter", &[PyRep::Integer(npc_corp), PyRep::Integer(1), PyRep::Integer(bad_q)]).unwrap_err().code,
            "InvalidQuantity"
        );
    }
    sess.station_id = None;
    assert_eq!(
        call_lp(&ctx, &mut sess, "LPStoreMgr", "TakeOfferForCharacter", &[PyRep::Integer(npc_corp), PyRep::Integer(1), PyRep::Integer(1)]).unwrap_err().code,
        "UserError"
    );
    sess.structure_id = Some(1_040_000_000_001);
    assert_eq!(
        call_lp(&ctx, &mut sess, "LPStoreMgr", "TakeOfferForCharacter", &[PyRep::Integer(npc_corp), PyRep::Integer(1), PyRep::Integer(1)]).unwrap_err().code,
        "StationServiceUnavailable"
    );
    sess.structure_id = None;
    sess.station_id = Some(station_id);

    assert_eq!(
        call_lp(&ctx, &mut sess, "LPStoreMgr", "TakeOfferForCharacter", &[PyRep::Integer(1_000_125), PyRep::Integer(1), PyRep::Integer(1)]).unwrap_err().code,
        "InvalidStationCorpForLPStore"
    );
    assert_eq!(
        call_lp(&ctx, &mut sess, "LPStoreMgr", "TakeOfferForCharacter", &[PyRep::Integer(npc_corp), PyRep::Integer(999_999_999), PyRep::Integer(1)]).unwrap_err().code,
        "OfferNotFound"
    );

    ctx.db.with_mut_snapshot(|snap| {
        register_lp_store_offer(snap, LPOffer {
            offer_id: 778_001, corp_id: npc_corp, type_id: 34, quantity: 50,
            lp_cost: 200, isk_cost: IskCents::from_isk(500), ak_cost: 0,
            req_items: vec![(35, 10)], min_standing: Some(2.0),
        });
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = player_corp;
            ch.balance = IskCents::from_isk(100_000);
            ch.loyalty_points.insert(npc_corp, 1_000);
            ch.standings.insert(npc_corp, 0.5);
        }
        snap.corporations.insert(player_corp, { let mut __corp = CorporationRecord::default();
            __corp.corporation_id = player_corp;
            __corp.name = "Player Corp".into();
            __corp.ticker = "PLRC".into();
            __corp.ceo_id = 90_000_099;
            __corp });
    });

    assert_eq!(
        call_lp(&ctx, &mut sess, "LPStoreMgr", "TakeOfferForCharacter", &[PyRep::Integer(778_001), PyRep::Integer(1)]).unwrap_err().code,
        "InsufficientStanding"
    );

    // Raise standing to 3.0, clear any demo-seed type 35 items, and seed only damaged/locked/BPC/courier_cargo items -> MissingRequiredItems
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.retain(|_, it| !(it.owner_id == char_id && it.type_id == 35));
        snap.characters.get_mut(&char_id).unwrap().standings.insert(npc_corp, 3.0);
        snap.items.insert(910_001, InventoryItemRecord {
            item_id: 910_001, type_id: 35, owner_id: char_id, location_id: station_id,
            flag: InventoryFlag::Hangar, quantity: 20, is_blueprint_copy: true, ..Default::default()
        });
        snap.items.insert(910_002, InventoryItemRecord {
            item_id: 910_002, type_id: 35, owner_id: char_id, location_id: station_id,
            flag: InventoryFlag::Hangar, quantity: 20,
            dynamic_attributes: HashMap::from([(3, 5.0)]), ..Default::default()
        });
        snap.items.insert(910_003, InventoryItemRecord {
            item_id: 910_003, type_id: 35, owner_id: char_id, location_id: station_id,
            flag: InventoryFlag::Hangar, quantity: 20, ..Default::default()
        });
        snap.item_custom_names.insert(910_003, "courier_cargo:999".into());
    });
    assert_eq!(
        call_lp(&ctx, &mut sess, "LPStoreMgr", "TakeOfferForCharacter", &[PyRep::Integer(778_001), PyRep::Integer(1)]).unwrap_err().code,
        "MissingRequiredItems"
    );

    let ship_id = 910_010_i64;
    sess.ship_id = Some(ship_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(ship_id, InventoryItemRecord {
            item_id: ship_id, type_id: 587, owner_id: char_id, location_id: station_id,
            flag: InventoryFlag::Hangar, quantity: 1, singleton: true, ..Default::default()
        });
        snap.items.insert(910_011, InventoryItemRecord {
            item_id: 910_011, type_id: 35, owner_id: char_id, location_id: ship_id,
            flag: InventoryFlag::Cargo, quantity: 25, ..Default::default()
        });
        snap.items.insert(910_012, InventoryItemRecord {
            item_id: 910_012, type_id: 35, owner_id: player_corp, location_id: station_id,
            flag: InventoryFlag::CorpSAG1, quantity: 25, ..Default::default()
        });
    });
    assert_eq!(
        call_lp(&ctx, &mut sess, "LPStoreMgr", "TakeOfferForCorporation", &[PyRep::Integer(778_001), PyRep::Integer(1)]).unwrap_err().code,
        "InsufficientCorporationRole"
    );

    let ok_take = call_lp(&ctx, &mut sess, "LPStoreMgr", "TakeOfferForCharacter", &[PyRep::Integer(778_001), PyRep::Integer(1)])
        .expect("TakeOfferForCharacter from ship cargo");
    assert_eq!(ok_take.dict_get("remainingLP").and_then(|v| v.as_i64()), Some(800));
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&910_011].quantity), 15);
}

#[test]
fn concord_lp_exchange_transfers_corp_balances_and_delayed_rewards() {
    let mut ctx = make_ctx();
    let char_id = 90_000_001_i64;
    let player_corp = 98_700_003_i64;
    let other_corp = 98_700_004_i64;
    let station_id = 60_003_760_i64;
    let soe_corp = 1_000_130_i64;
    let pirate_corp = 1_000_135_i64;

    let sde_mut = Arc::make_mut(&mut ctx.sde);
    for (cid, name, ticker, fid) in [
        (soe_corp, "Sisters of EVE", "SOE", 500_016),
        (pirate_corp, "Mordu's Legion", "MRDU", 500_018),
    ] {
        sde_mut.npc_corporations.insert(cid, NpcCorporationInfo {
            corporation_id: cid, name: name.into(), ticker_name: ticker.into(),
            faction_id: Some(fid), solar_system_id: Some(30_000_142),
            station_id: Some(station_id), tax_rate: 0.0,
        });
    }

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = player_corp;
            ch.station_id = Some(station_id);
            ch.loyalty_points.insert(CONCORD_CORP_ID, 1_000);
        }
        snap.corporations.insert(player_corp, { let mut __corp = CorporationRecord::default();
            __corp.corporation_id = player_corp;
            __corp.name = "Player Corp".into();
            __corp.ticker = "PLRC".into();
            __corp.ceo_id = 90_000_099;
            __corp });
    });

    let mut sess = make_session(char_id, player_corp, Some(station_id));
    assert_eq!(
        call_lp(&ctx, &mut sess, "LPSvc", "GetAllMyCorporationWalletLPBalances", &[PyRep::Integer(other_corp)]).unwrap_err().message,
        "CrpAccessDenied"
    );
    assert_eq!(
        call_lp(&ctx, &mut sess, "LPSvc", "GetAllMyCorporationWalletLPBalances", &[]).unwrap_err().message,
        "CrpAccessDenied"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.corp_role_assignments.insert(
            (player_corp, char_id),
            CorpMemberRoleAssignment {
                character_id: char_id,
                roles: corp_roles::ROLE_JUNIOR_ACCOUNTANT,
                ..Default::default()
            },
        );
    });
    assert!(call_lp(&ctx, &mut sess, "LPSvc", "GetAllMyCorporationWalletLPBalances", &[]).is_ok());

    assert_eq!(
        call_lp(&ctx, &mut sess, "LPSvc", "TransferLPFromMyWalletToOtherCorp", &[]).unwrap_err().code,
        "InvalidTargetCorp"
    );
    assert_eq!(
        call_lp(&ctx, &mut sess, "LPSvc", "TransferLPFromMyWalletToOtherCorp", &[PyRep::Integer(player_corp), PyRep::Integer(0), PyRep::Integer(10)]).unwrap_err().code,
        "InvalidCorpID"
    );
    assert_eq!(
        call_lp(&ctx, &mut sess, "LPSvc", "TransferLPFromMyWalletToOtherCorp", &[PyRep::Integer(player_corp), PyRep::Integer(soe_corp), PyRep::Integer(0)]).unwrap_err().code,
        "InvalidLPAmount"
    );
    assert_eq!(
        call_lp(&ctx, &mut sess, "LPSvc", "TransferLPFromMyWalletToOtherCorp", &[PyRep::Integer(99_999_999), PyRep::Integer(soe_corp), PyRep::Integer(10)]).unwrap_err().code,
        "CorporationNotFound"
    );
    assert_eq!(
        call_lp(&ctx, &mut sess, "LPSvc", "TransferLPFromMyCorpWalletToOtherCorp", &[PyRep::Integer(player_corp), PyRep::Integer(soe_corp), PyRep::Integer(10)]).unwrap_err().code,
        "InvalidTargetCorp"
    );

    for bad_corp in [0, CONCORD_CORP_ID, 1_000_180, 1_000_139, 99_999_999, pirate_corp] {
        assert_eq!(
            call_lp(&ctx, &mut sess, "LPStoreMgr", "ExchangeConcordLP", &[PyRep::Integer(bad_corp), PyRep::Integer(100)]).unwrap_err().code,
            "InvalidTargetCorp"
        );
    }
    assert_eq!(
        call_lp(&ctx, &mut sess, "LPStoreMgr", "ExchangeConcordLP", &[PyRep::Integer(soe_corp), PyRep::Integer(1)]).unwrap_err().code,
        "InvalidLPAmount"
    );
    assert_eq!(
        call_lp(&ctx, &mut sess, "LPStoreMgr", "ExchangeConcordLP", &[PyRep::Integer(soe_corp), PyRep::Integer(100)]).unwrap_err().code,
        "NoExistingTargetCorpLP"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().loyalty_points.insert(soe_corp, 50);
    });
    let ex_soe = call_lp(&ctx, &mut sess, "LPStoreMgr", "ExchangeConcordLP", &[PyRep::Integer(soe_corp), PyRep::Integer(100)])
        .expect("0.4 rate CONCORD LP exchange");
    assert_eq!(ex_soe.dict_get("convertedLP").and_then(|v| v.as_i64()), Some(40));
    assert_eq!(ex_soe.dict_get("newTargetCorpLP").and_then(|v| v.as_i64()), Some(90));

    assert_eq!(
        call_lp(&ctx, &mut sess, "rewardMgr", "GetDelayedRewardsByGroupIDs", &[]).unwrap_err().code,
        "InvalidArguments"
    );
    with_pve_special_reg(&ctx, |reg| {
        reg.incursion_lp_logs.entry(char_id).or_default().push(IncursionLPLogEntry {
            log_id: 555, character_id: char_id, constellation_id: 20_000_020, solar_system_id: 30_000_142,
            site_id: "site-1".into(), tier: "Vanguard".into(), isk_earned: 10_000_000,
            concord_lp_earned: 1_400, fleet_size: 10, fleet_scale_factor: 1.0, timestamp_filetime: 123_456,
        });
    });
    ctx.db.with_mut_snapshot(|snap| {
        snap.fleet_and_social_ops.delayed_rewards.insert(
            char_id,
            HashMap::from([(
                "555".to_string(),
                vec![serde_json::json!({"isk": 5000, "lp": 250})],
            )]),
        );
    });
    let delayed = call_lp(&ctx, &mut sess, "rewardMgr", "GetDelayedRewardsByGroupIDs", &[PyRep::Integer(555)]).unwrap();
    if let PyRep::Dict(entries) = delayed {
        assert_eq!(entries.len(), 1);
        assert!(matches!(entries[0].1, PyRep::List(ref r) if r.len() == 2));
    } else {
        panic!("expected Dict");
    }
}

#[test]
fn rw_manager_returns_empty_site_queries() {
    let ctx = make_ctx();
    let mut unauth = SessionState::new(9402);
    assert!(call_lp(&ctx, &mut unauth, "RWManager", "solarsystem_contains_rw_instances", &[]).is_err());
    assert!(call_lp(&ctx, &mut unauth, "RWManager", "get_closest_rw_stations", &[PyRep::Integer(30_000_142)]).is_err());

    let mut sess = make_session(90_000_001, 98_700_001, Some(60_003_760));
    assert_eq!(
        call_lp(&ctx, &mut sess, "RWManager", "solarsystem_contains_rw_instances", &[]).unwrap(),
        PyRep::Bool(false)
    );
    assert_eq!(
        call_lp(
            &ctx,
            &mut sess,
            "RWManager",
            "solarsystem_contains_rw_instances",
            &[PyRep::Integer(30_000_142)],
        )
        .unwrap(),
        PyRep::Bool(false)
    );
    assert_eq!(
        call_lp(
            &ctx,
            &mut sess,
            "RWManager",
            "get_closest_rw_stations",
            &[PyRep::Integer(30_000_142)],
        )
        .unwrap(),
        PyRep::Dict(Vec::new())
    );
}
