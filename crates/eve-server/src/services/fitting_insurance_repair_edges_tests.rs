//! Comprehensive unit tests covering edge cases for `services::fitting_insurance_repair`.

use super::*;
use crate::router::ServerContext;
use crate::services::station_structure::structure_upwell_ops::{
    save_ext_state, AccessGroupRecord, StructureAccessProfile, StructureExtendedState,
};
use eve_db::{
    InventoryFlag, InventoryItemRecord, IskCents, UpwellStructureRecord, WorldRepository,
};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new().with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED);
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

fn find_station_with_service(sde: &SdeRegistry, svc_id: i64, want: bool) -> i64 {
    sde.stations
        .keys()
        .copied()
        .find(|&sid| {
            crate::services::station_structure::npc_station_offers_service(sde, sid, svc_id)
                == want
        })
        .unwrap_or(60_003_760)
}

fn make_session(char_id: i64, corp_id: i64, ship_id: Option<i64>, station_id: Option<i64>) -> SessionState {
    SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        corporation_id: Some(corp_id),
        ship_id,
        station_id,
        ..Default::default()
    }
}

#[test]
fn insure_ship_validation_error_paths_and_success_flows() {
    let ctx = test_ctx();
    let char_id = 90_000_001;
    let corp_id = 98_000_001;
    let ship_id = 100_700_001;
    let station_ok = find_station_with_service(&ctx.sde, 21, true);
    let station_no_ins = find_station_with_service(&ctx.sde, 21, false);
    let kwargs = HashMap::new();

    // 1. Unauthenticated -> error
    let mut bad_sess = SessionState::default();
    assert!(handle_insure_ship(&ctx, &mut bad_sess, &[], &kwargs).is_err());

    // 2. No active ship & no arg0 -> NoActiveShip
    let mut sess = make_session(char_id, corp_id, None, Some(station_ok));
    assert_eq!(
        handle_insure_ship(&ctx, &mut sess, &[], &kwargs)
            .unwrap_err()
            .code,
        "NoActiveShip"
    );

    // 3. Missing arg1 -> InsureShipInvalidCost
    sess.ship_id = Some(ship_id);
    assert_eq!(
        handle_insure_ship(&ctx, &mut sess, &[PyRep::Integer(ship_id)], &kwargs)
            .unwrap_err()
            .code,
        "InsureShipInvalidCost"
    );

    // 4. Docked at Upwell structure -> StationServiceUnavailable
    sess.structure_id = Some(1_000_000_000_001);
    assert_eq!(
        handle_insure_ship(
            &ctx,
            &mut sess,
            &[PyRep::Integer(ship_id), PyRep::Real(1.0)],
            &kwargs
        )
        .unwrap_err()
        .code,
        "StationServiceUnavailable"
    );
    sess.structure_id = None;

    // 5. Not docked -> NotDocked
    sess.station_id = None;
    assert_eq!(
        handle_insure_ship(
            &ctx,
            &mut sess,
            &[PyRep::Integer(ship_id), PyRep::Real(1.0)],
            &kwargs
        )
        .unwrap_err()
        .code,
        "NotDocked"
    );

    // 6. Station >= 1e12 or without insurance service -> StationServiceUnavailable
    sess.station_id = Some(1_000_000_000_005);
    assert_eq!(
        handle_insure_ship(
            &ctx,
            &mut sess,
            &[PyRep::Integer(ship_id), PyRep::Real(1.0)],
            &kwargs
        )
        .unwrap_err()
        .code,
        "StationServiceUnavailable"
    );
    sess.station_id = Some(station_no_ins);
    if !crate::services::station_structure::npc_station_offers_service(&ctx.sde, station_no_ins, 21) {
        assert_eq!(
            handle_insure_ship(
                &ctx,
                &mut sess,
                &[PyRep::Integer(ship_id), PyRep::Real(1.0)],
                &kwargs
            )
            .unwrap_err()
            .code,
            "StationServiceUnavailable"
        );
    }

    sess.station_id = Some(station_ok);

    // 7. ShipNotFound
    assert_eq!(
        handle_insure_ship(
            &ctx,
            &mut sess,
            &[PyRep::Integer(999_777_666), PyRep::Real(1.0)],
            &kwargs
        )
        .unwrap_err()
        .code,
        "ShipNotFound"
    );

    // 8. Non-ship category (e.g. Tritanium 34), unassembled (!singleton), impounded, wrong location, wrong owner
    let non_ship_id = 100_700_002;
    let unpackaged_ship = 100_700_003;
    let impounded_ship = 100_700_004;
    let away_ship = 100_700_005;
    let other_owner_ship = 100_700_006;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            non_ship_id,
            InventoryItemRecord {
                item_id: non_ship_id,
                type_id: 34,
                owner_id: char_id,
                location_id: station_ok,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            unpackaged_ship,
            InventoryItemRecord {
                item_id: unpackaged_ship,
                type_id: 587,
                owner_id: char_id,
                location_id: station_ok,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: false,
                ..Default::default()
            },
        );
        snap.items.insert(
            impounded_ship,
            InventoryItemRecord {
                item_id: impounded_ship,
                type_id: 587,
                owner_id: char_id,
                location_id: station_ok,
                flag: InventoryFlag::Impounded,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            away_ship,
            InventoryItemRecord {
                item_id: away_ship,
                type_id: 587,
                owner_id: char_id,
                location_id: station_ok + 1,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            other_owner_ship,
            InventoryItemRecord {
                item_id: other_owner_ship,
                type_id: 587,
                owner_id: 90_000_002,
                location_id: station_ok,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: station_ok,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.balance = IskCents::from_isk(1_000_000_000);
        }
    });

    assert_eq!(
        handle_insure_ship(&ctx, &mut sess, &[PyRep::Integer(non_ship_id), PyRep::Real(1.0)], &kwargs)
            .unwrap_err()
            .code,
        "InsureShipFailed"
    );
    assert_eq!(
        handle_insure_ship(&ctx, &mut sess, &[PyRep::Integer(unpackaged_ship), PyRep::Real(1.0)], &kwargs)
            .unwrap_err()
            .code,
        "InsureShipNotAssembled"
    );
    assert_eq!(
        handle_insure_ship(&ctx, &mut sess, &[PyRep::Integer(impounded_ship), PyRep::Real(1.0)], &kwargs)
            .unwrap_err()
            .code,
        "ShipNotInsureable"
    );
    assert_eq!(
        handle_insure_ship(&ctx, &mut sess, &[PyRep::Integer(away_ship), PyRep::Real(1.0)], &kwargs)
            .unwrap_err()
            .code,
        "ShipNotAtStation"
    );
    assert_eq!(
        handle_insure_ship(&ctx, &mut sess, &[PyRep::Integer(other_owner_ship), PyRep::Real(1.0)], &kwargs)
            .unwrap_err()
            .code,
        "OwnershipViolation"
    );

    // 9. Cost <= 0.0 and invalid >1.0 cost -> InsureShipInvalidCost
    assert_eq!(
        handle_insure_ship(&ctx, &mut sess, &[PyRep::Integer(ship_id), PyRep::Real(0.0)], &kwargs)
            .unwrap_err()
            .code,
        "InsureShipInvalidCost"
    );
    assert_eq!(
        handle_insure_ship(&ctx, &mut sess, &[PyRep::Integer(ship_id), PyRep::Real(123_456_789.0)], &kwargs)
            .unwrap_err()
            .code,
        "InsureShipInvalidCost"
    );

    // 10. Success via fraction 1.0 (Platinum)
    let ok = handle_insure_ship(
        &ctx,
        &mut sess,
        &[PyRep::Integer(ship_id), PyRep::Real(1.0)],
        &kwargs,
    )
    .unwrap();
    let cost_isk = ok.dict_get("costISK").and_then(|v| v.as_f64()).unwrap();
    assert_eq!(ok.dict_get("levelName").and_then(|v| v.as_str()), Some("Platinum"));

    // 11. Duplicate insurance without voidOld -> InsureShipFailedSingleContract
    assert_eq!(
        handle_insure_ship(
            &ctx,
            &mut sess,
            &[PyRep::Integer(ship_id), PyRep::Real(0.8)],
            &kwargs
        )
        .unwrap_err()
        .code,
        "InsureShipFailedSingleContract"
    );

    // 12. Renew with voidOld=true using exact cost > 1.0
    let mut kw_void = HashMap::new();
    kw_void.insert("voidOld".into(), PyRep::Bool(true));
    let ok2 = handle_insure_ship(
        &ctx,
        &mut sess,
        &[PyRep::Integer(ship_id), PyRep::Real(cost_isk)],
        &kw_void,
    )
    .unwrap();
    assert_eq!(ok2.dict_get("shipID").and_then(|v| v.as_i64()), Some(ship_id));

    // 13. Insufficient personal funds
    let broke_ship = 100_700_010;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            broke_ship,
            InventoryItemRecord {
                item_id: broke_ship,
                type_id: 587,
                owner_id: char_id,
                location_id: station_ok,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.characters.get_mut(&char_id).unwrap().balance = IskCents(1);
    });
    assert_eq!(
        handle_insure_ship(
            &ctx,
            &mut sess,
            &[PyRep::Integer(broke_ship), PyRep::Real(1.0)],
            &kwargs
        )
        .unwrap_err()
        .code,
        "InsufficientFunds"
    );

    // 14. Corp ship inside ShipHangar carrier at station (div 1 and div 2, plus insufficient corp funds)
    let carrier_id = 100_700_020;
    let corp_ship_id = 100_700_021;
    let actual_corp = ctx.db.with_mut_snapshot(|snap| {
        let cid = snap.characters[&char_id].corporation_id;
        let mut corp = snap
            .corporations
            .get(&cid)
            .cloned()
            .unwrap_or_else(|| snap.corporations.values().next().expect("demo corp").clone());
        corp.corporation_id = cid;
        corp.ceo_id = char_id;
        corp.ensure_wallet_divisions();
        corp.set_division_balance(1, IskCents::from_isk(100_000_000));
        if let Some(d1) = corp.wallet_divisions.first_mut() {
            d1.balance = IskCents::from_isk(100_000_000);
        }
        if let Some(d2) = corp.wallet_divisions.iter_mut().find(|d| d.division_id == 2) {
            d2.balance = IskCents::from_isk(100_000_000);
        }
        snap.corporations.insert(cid, corp);
        snap.items.insert(
            carrier_id,
            InventoryItemRecord {
                item_id: carrier_id,
                type_id: 23757, // Archon
                owner_id: cid,
                location_id: station_ok,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            corp_ship_id,
            InventoryItemRecord {
                item_id: corp_ship_id,
                type_id: 587,
                owner_id: cid,
                location_id: carrier_id,
                flag: InventoryFlag::ShipHangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        cid
    });
    sess.corporation_id = Some(actual_corp);
    sess.corp_account_key = 1000;
    let corp_ok1 = handle_insure_ship(
        &ctx,
        &mut sess,
        &[PyRep::Integer(corp_ship_id), PyRep::Real(0.5), PyRep::Bool(true), PyRep::Bool(false)],
        &kwargs,
    )
    .unwrap();
    assert_eq!(corp_ok1.dict_get("levelName").and_then(|v| v.as_str()), Some("Basic"));

    // Division 2 (corp_account_key = 1001) with voidOld=true
    sess.corp_account_key = 1001;
    let corp_ok2 = handle_insure_ship(
        &ctx,
        &mut sess,
        &[PyRep::Integer(corp_ship_id), PyRep::Real(0.7), PyRep::Bool(true), PyRep::Bool(true)],
        &kwargs,
    )
    .unwrap();
    assert_eq!(corp_ok2.dict_get("levelName").and_then(|v| v.as_str()), Some("Bronze"));

    // Insufficient corp div 2 funds
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(corp) = snap.corporations.get_mut(&actual_corp) {
            if let Some(d2) = corp.wallet_divisions.iter_mut().find(|d| d.division_id == 2) {
                d2.balance = IskCents(0);
            }
        }
    });
    assert_eq!(
        handle_insure_ship(
            &ctx,
            &mut sess,
            &[PyRep::Integer(corp_ship_id), PyRep::Real(0.9), PyRep::Bool(true), PyRep::Bool(true)],
            &kwargs
        )
        .unwrap_err()
        .code,
        "InsufficientFunds"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repair_all_npc_station_and_upwell_structure_paths() {
    let ctx = test_ctx();
    let char_id = 90_000_001;
    let other_char = 90_000_002;
    let corp_id = 98_000_001;
    let ship_id = 100_710_001;
    let station_ok = find_station_with_service(&ctx.sde, 13, true);
    let station_no_rep = find_station_with_service(&ctx.sde, 13, false);

    // 1. NotDocked -> NotDocked
    let mut sess = make_session(char_id, corp_id, Some(ship_id), None);
    assert_eq!(handle_repair_all(&ctx, &mut sess, &[]).unwrap_err().code, "NotDocked");

    // 2. Station without repair service -> StationServiceUnavailable
    if !crate::services::station_structure::npc_station_offers_service(&ctx.sde, station_no_rep, 13) {
        sess.station_id = Some(station_no_rep);
        assert_eq!(
            handle_repair_all(&ctx, &mut sess, &[]).unwrap_err().code,
            "StationServiceUnavailable"
        );
    }

    // 3. NoActiveShip -> NoActiveShip
    sess.station_id = Some(station_ok);
    sess.ship_id = None;
    assert_eq!(handle_repair_all(&ctx, &mut sess, &[]).unwrap_err().code, "NoActiveShip");
    sess.ship_id = Some(ship_id);

    // 4. Explicit item errors: ItemNotFound, ItemOwnershipViolation, ItemNotAtStation
    let mod_id = 100_710_010;
    let foreign_item = 100_710_011;
    let away_item = 100_710_012;
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().balance = IskCents::from_isk(100_000_000);
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: station_ok,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                dynamic_attributes: [(attr_ids::DAMAGE, 5.0), (attr_ids::ARMOR_DAMAGE, 10.0)]
                    .into_iter()
                    .collect(),
                ..Default::default()
            },
        );
        snap.items.insert(
            mod_id,
            InventoryItemRecord {
                item_id: mod_id,
                type_id: 519, // Small Armor Repairer or similar
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::MedSlot0,
                quantity: 1,
                singleton: true,
                dynamic_attributes: [(attr_ids::DAMAGE, 2.5)].into_iter().collect(),
                ..Default::default()
            },
        );
        snap.items.insert(
            foreign_item,
            InventoryItemRecord {
                item_id: foreign_item,
                type_id: 34,
                owner_id: other_char,
                location_id: station_ok,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            away_item,
            InventoryItemRecord {
                item_id: away_item,
                type_id: 34,
                owner_id: char_id,
                location_id: station_ok + 5,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                ..Default::default()
            },
        );
    });

    assert_eq!(
        handle_repair_all(&ctx, &mut sess, &[PyRep::Integer(999_888_777)])
            .unwrap_err()
            .code,
        "ItemNotFound"
    );
    assert_eq!(
        handle_repair_all(&ctx, &mut sess, &[PyRep::List(vec![PyRep::Integer(foreign_item)])])
            .unwrap_err()
            .code,
        "ItemOwnershipViolation"
    );
    assert_eq!(
        handle_repair_all(&ctx, &mut sess, &[PyRep::Tuple(vec![PyRep::Integer(away_item)])])
            .unwrap_err()
            .code,
        "ItemNotAtStation"
    );

    // 5. Add fitting module damage, rack heat, live combat_state damage, and persisted combat HP deficit
    let mut fit = ctx.get_or_rebuild_ship_fitting(char_id, ship_id).unwrap();
    if let Some(m) = fit.modules.values_mut().next() {
        m.damage_hp = 3.0;
    }
    fit.rack_heat = [12.0, 8.0, 4.0];
    ctx.store_ship_fitting(ship_id, fit);

    let sys = ctx.systems.get_or_spawn(30_000_142);
    let mut cs = ctx.build_ship_combat_state(char_id, ship_id, 587);
    cs.armor_hp = (cs.max_armor_hp - 50.0).max(0.0);
    cs.structure_hp = (cs.max_structure_hp - 20.0).max(0.0);
    sys.try_spawn_ball_with_combat(
        Ball {
            id: ship_id,
            type_id: 587,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 350.0,
            inertia: 2.0,
            mass: 1_100_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        cs,
    );

    // Repair at NPC station with integer arg (mod_id)
    let rep = handle_repair_all(&ctx, &mut sess, &[PyRep::Integer(mod_id)]).unwrap();
    assert_eq!(rep.dict_get("repaired").and_then(|v| v.as_bool()), Some(true));
    assert!(rep.dict_get("repairedModuleHP").and_then(|v| v.as_f64()).unwrap() > 0.0);
    assert!(rep.dict_get("repairCostISK").and_then(|v| v.as_f64()).unwrap() > 0.0);

    // Repair everything at NPC station (repairs remaining ship armor+hull damage from live_cs)
    let rep_all = handle_repair_all(&ctx, &mut sess, &[]).unwrap();
    assert_eq!(rep_all.dict_get("repairCostISK").and_then(|v| v.as_f64()), Some(35_000.0));

    // Undock/remove ball so live_cs no longer reports pre-dock ballpark deficit; undamaged -> 0.0
    let _ = sys.remove_ball(ship_id).await;
    let rep_clean = handle_repair_all(&ctx, &mut sess, &[]).unwrap();
    assert_eq!(rep_clean.dict_get("repairCostISK").and_then(|v| v.as_f64()), Some(0.0));

    // 6. Upwell structure repair: StructureNotFound, NotOperational, QuantumCoreRequired, Abandoned, ACL denied, Success with tax!
    let upwell_id = 1_000_710_999_001;
    sess.station_id = None;
    sess.structure_id = Some(upwell_id);

    // StructureNotFound
    assert_eq!(
        handle_repair_all(&ctx, &mut sess, &[]).unwrap_err().code,
        "StructureNotFound"
    );

    // Insert non-operational structure
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            upwell_id,
            UpwellStructureRecord {
                structure_id: upwell_id,
                type_id: 35_832, // Astrahus
                solar_system_id: 30_000_142,
                owner_corp_id: 98_000_002, // different corp
                name: "Repair Citadel".into(),
                state: "Anchoring".into(),
                quantum_core_installed: false,
                fuel_blocks: 100,
                ..Default::default()
            },
        );
    });
    assert_eq!(
        handle_repair_all(&ctx, &mut sess, &[]).unwrap_err().code,
        "StructureNotOperational"
    );

    // Online without Quantum Core -> QuantumCoreRequired
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&upwell_id).unwrap().state = "Online".into();
    });
    assert_eq!(
        handle_repair_all(&ctx, &mut sess, &[]).unwrap_err().code,
        "QuantumCoreRequired"
    );

    // Quantum core installed, ACL denied for other_char (owner_corp_id != caller_corp)
    ctx.db.with_mut_snapshot(|snap| {
        let st = snap.structures.get_mut(&upwell_id).unwrap();
        st.quantum_core_installed = true;
        let mut ext = StructureExtendedState::default();
        let mut prof = StructureAccessProfile::default();
        prof.profile_id = 55;
        prof.corp_only = true;
        prof.service_settings.insert(13, 500); // 5% repairshop tax
        ext.access_profiles.insert(upwell_id, prof);
        ext.structure_profile_id.insert(upwell_id, 55);
        ext.owner_groups.insert(
            1,
            AccessGroupRecord {
                group_id: 1,
                ..Default::default()
            },
        );
        save_ext_state(snap, &ext);
    });
    assert_eq!(
        handle_repair_all(&ctx, &mut sess, &[]).unwrap_err().code,
        "StructureRepairAccessDenied"
    );

    // Switch owner_corp_id to caller's corp and move ship into upwell_id with damage -> repairs & pays tax!
    ctx.db.with_mut_snapshot(|snap| {
        let caller_corp = snap.characters[&char_id].corporation_id;
        let mut corp = snap
            .corporations
            .get(&caller_corp)
            .cloned()
            .unwrap_or_else(|| snap.corporations.values().next().expect("demo corp").clone());
        corp.corporation_id = caller_corp;
        corp.ensure_wallet_divisions();
        snap.corporations.insert(caller_corp, corp);
        snap.structures.get_mut(&upwell_id).unwrap().owner_corp_id = caller_corp;
        let sh = snap.items.get_mut(&ship_id).unwrap();
        sh.location_id = upwell_id;
        sh.dynamic_attributes.insert(attr_ids::DAMAGE, 20.0);
    });
    let uid = ctx.db.with_snapshot(|s| s.characters.get(&char_id).map_or(1, |c| c.user_id));
    let _ = ctx.db.acquire_online_lock(uid, 1, Some(char_id), false, ctx.now_unix());
    let _ = ctx.drain_notifications_for_char(char_id);
    let upwell_rep = handle_repair_all(&ctx, &mut sess, &[]).unwrap();
    assert_eq!(upwell_rep.dict_get("repaired").and_then(|v| v.as_bool()), Some(true));
    assert!(upwell_rep.dict_get("repairCostISK").and_then(|v| v.as_f64()).unwrap() > 0.0);
    let rep_notes = ctx.drain_notifications_for_char(char_id);
    let acct_note = rep_notes
        .iter()
        .find(|n| n.event == "OnAccountChange")
        .expect("handle_repair_all must emit OnAccountChange when debiting ISK");
    assert_eq!(acct_note.args[0].as_str(), Some("cash"));
    assert_eq!(acct_note.args[1].as_i64(), Some(char_id));
    let expected_bal = ctx.db.with_snapshot(|s| s.characters[&char_id].balance.to_wire_f64());
    assert!((acct_note.args[2].as_f64().unwrap() - expected_bal).abs() < 1e-6);

    // Zero-cost RepairAll does not emit OnAccountChange
    let zero_rep = handle_repair_all(&ctx, &mut sess, &[]).unwrap();
    assert_eq!(zero_rep.dict_get("repaired").and_then(|v| v.as_bool()), Some(true));
    assert!(ctx.drain_notifications_for_char(char_id).iter().all(|n| n.event != "OnAccountChange"));

    // InsufficientFunds when balance is 0 and damage > 0
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().balance = IskCents(0);
        snap.items
            .get_mut(&ship_id)
            .unwrap()
            .dynamic_attributes
            .insert(attr_ids::ARMOR_DAMAGE, 50.0);
    });
    assert_eq!(
        handle_repair_all(&ctx, &mut sess, &[]).unwrap_err().code,
        "InsufficientFunds"
    );
}
