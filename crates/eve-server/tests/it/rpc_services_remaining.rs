#![forbid(unsafe_code)]


#[allow(unused_imports, dead_code, unused_variables, unused_mut)]
mod rpc_missing_services_ext_a {
    use super::*;

// State-backed integration tests for 15 newly implemented V24 RPCs across
// `contractProxy`, `repairSvc`, `structure`, `search`, `i2`, `jumpCloneSvc`,
// `warpVectorMgr`, and `devIndexManager`.

use eve_db::{
    CharacterRecord, ContractRecord, CorporationRecord, InventoryFlag, InventoryItemRecord,
    IskCents, SystemDynamicState, UpwellStructureRecord,
};
use eve_destiny::Ball;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::attr_ids;
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

fn dict_get<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match rep {
        PyRep::Dict(entries) => entries
            .iter()
            .find(|(k, _)| matches!(k, PyRep::String(s) if s == key))
            .map(|(_, v)| v),
        PyRep::KeyVal(entries) => entries
            .iter()
            .find(|(k, _)| k.as_str() == key)
            .map(|(_, v)| v),
        _ => None,
    }
}

async fn call_rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None).await
}

async fn call_rpc_kwargs(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: HashMap<String, PyRep>,
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &kwargs, None).await
}

fn make_item(
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
    singleton: bool,
    is_bpc: bool,
) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id,
        type_id,
        owner_id,
        location_id,
        flag,
        quantity,
        singleton,
        version: 1,
        dynamic_attributes: HashMap::new(),
        source_mutaplasmid_id: None,
        insured_payout_isk: 0.0,
        is_blueprint_copy: is_bpc,
        blueprint_runs_remaining: if is_bpc { Some(10) } else { None },
    }
}

fn make_char(char_id: i64, corp_id: i64, station_id: Option<i64>, structure_id: Option<i64>, ship_id: i64) -> CharacterRecord {
    CharacterRecord {
        character_id: char_id, user_id: 700, name: "Valkyrie SearchPilot".into(),
        corporation_id: corp_id, alliance_id: Some(99_000_777), war_faction_id: None, fleet_id: None, autopilot_waypoints: Vec::new(),
        station_id, structure_id,
        solar_system_id: 30_000_142, constellation_id: 20_000_020, region_id: 10_000_002,
        active_ship_id: ship_id, balance: IskCents::from_f64_isk(100_000_000.0),
        plex_balance: 0, loyalty_points: HashMap::new(), standings: HashMap::new(),
        security_status: 1.5, clone_grade: 2, home_station_id: 60_003_760,
        race_id: 1, bloodline_id: 1, ancestry_id: 1, school_id: 1,
        neural: eve_db::NeuralAttributes::default(), skill_points: 1_000_000.0,
        activity_status: None,
        free_skill_points: 0.0, last_clone_jump_unix: 1_700_000_000, jump_fatigue_minutes: 0.0,
        jump_cooldown_until_unix: 0, jump_fatigue_until_unix: 0,
        description: String::new(), title: String::new(), gender: 1,
        create_date_unix: 1_700_000_000, version: 1, system_epoch: 1,
    }
}

fn make_contract(
    contract_id: i64,
    issuer_id: i64,
    contract_type: &str,
    status: &str,
    station_id: i64,
    price: f64,
    reward: f64,
    collateral: f64,
    assignee_id: i64,
    corp_id: i64,
) -> ContractRecord {
    ContractRecord {
        contract_id, issuer_id, acceptor_id: None, contract_type: contract_type.into(),
        status: status.into(), start_station_id: station_id, end_station_id: station_id,
        price: IskCents::from_f64_isk(price), reward: IskCents::from_f64_isk(reward), collateral: IskCents::from_f64_isk(collateral),
        item_ids: Vec::new(), expiration_unix: 2_000_000_000, issuer_corp_id: corp_id, assignee_id,
        availability: if assignee_id > 0 { "Personal".into() } else { "Public".into() },
        buyout: IskCents(0), current_bid: IskCents(0), current_bidder_id: None,
        requested_items: Vec::new(), bid_history: Vec::new(), date_issued_unix: 1_700_000_000,
        title: format!("Contract {contract_id}"), for_corp: false, num_days: 14,
    }
}

fn seed_char_and_corp(ctx: &ServerContext, char_id: i64, corp_id: i64, station_id: Option<i64>, structure_id: Option<i64>, ship_id: i64) {
    ctx.db.with_mut_snapshot(|snap| {
        let mut roles = HashMap::new();
        roles.insert(char_id, eve_db::corp_roles::ROLE_DIRECTOR | eve_db::corp_roles::ROLE_CONTRACT_MANAGER | eve_db::corp_roles::ROLE_STATION_MANAGER);
        snap.accounts.insert("pilot_demo".into(), eve_db::AccountRecord {
            user_id: 700, username: "pilot_demo".into(), password_token: "secret".into(),
            role: eve_db::service_roles::ROLE_PLAYER_DEFAULT, banned_until_unix: None, ban_reason: None,
        });
        snap.corporations.insert(corp_id, { let mut __corp = CorporationRecord::default();
            __corp.corporation_id = corp_id;
            __corp.name = "Apex Vanguard Logistics".into();
            __corp.ticker = "APXVL".into();
            __corp.ceo_id = char_id;
            __corp.alliance_id = Some(99_000_777);
            __corp.tax_rate = 0.05;
            __corp.members = [char_id].into_iter().collect();
            __corp.member_roles = roles;
            __corp.set_division_balance(1, IskCents::from_f64_isk(50_000_000.0));
            __corp });
        snap.characters.insert(char_id, make_char(char_id, corp_id, station_id, structure_id, ship_id));
        let loc = station_id.or(structure_id).unwrap_or(30_000_142);
        snap.items.insert(ship_id, make_item(ship_id, 587, char_id, loc, InventoryFlag::Hangar, 1, true, false));
    });
}

#[tokio::test]
async fn contract_proxy_notifications_bulk_delete_and_dockable_items() {
    // parity: rpc:contractProxy.DeleteContractNotification, rpc:contractProxy.DeleteNotification
    // parity: rpc:contractProxy.DeleteMultipleContracts, rpc:contractProxy.GetItemsInDockableLocation
    eve_testkit::attest(&[
        "rpc:contractProxy.DeleteContractNotification",
        "rpc:contractProxy.DeleteNotification",
        "rpc:contractProxy.DeleteMultipleContracts",
        "rpc:contractProxy.GetItemsInDockableLocation",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let char_id = 95_100_001i64;
    let other_id = 95_100_002i64;
    let corp_id = 98_100_001i64;
    let station_id = 60_003_760i64;
    let ship_id = 950_100_001i64;
    seed_char_and_corp(&ctx, char_id, corp_id, Some(station_id), None, ship_id);

    let mut sess = SessionState::new(81_001);
    sess.user_id = Some(700);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.station_id = Some(station_id);
    sess.ship_id = Some(ship_id);
    sess.solar_system_id = Some(30_000_142);
    let _ = ctx.db.acquire_online_lock(700, sess.session_id, Some(char_id), true, ctx.now_unix());

    let item_ore = 950_100_010i64;
    let item_bpc = 950_100_011i64;
    let item_escrow = 950_100_012i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(item_ore, make_item(item_ore, 34, char_id, station_id, InventoryFlag::Hangar, 2500, false, false));
        snap.items.insert(item_bpc, make_item(item_bpc, 688, char_id, station_id, InventoryFlag::Hangar, 1, true, true));
        snap.items.insert(item_escrow, make_item(item_escrow, 35, 0, station_id, InventoryFlag::Locked, 500, false, false));

        let mut c1 = make_contract(7001, other_id, "Auction", "Outstanding", station_id, 1_000_000.0, 0.0, 0.0, 0, corp_id);
        c1.current_bid = IskCents::from_f64_isk(2_000_000.0);
        c1.current_bidder_id = Some(other_id);
        c1.buyout = IskCents::from_f64_isk(5_000_000.0);
        c1.bid_history = vec![(char_id, IskCents::from_f64_isk(1_500_000.0), 1_700_000_010)];
        snap.contracts.insert(7001, c1);

        let c2 = make_contract(7002, other_id, "ItemExchange", "Outstanding", station_id, 500_000.0, 0.0, 0.0, char_id, corp_id);
        snap.contracts.insert(7002, c2);

        let mut c3 = make_contract(7003, char_id, "Courier", "Outstanding", station_id, 0.0, 250_000.0, 1_000_000.0, 0, corp_id);
        c3.item_ids = vec![item_escrow];
        snap.contracts.insert(7003, c3);
    });

    // 1. GetItemsInDockableLocation returns unlocked station hangar items
    let dock_items = call_rpc(&ctx, &mut sess, "contractProxy", "GetItemsInDockableLocation", &[PyRep::Integer(station_id), PyRep::Integer(0)])
        .await.expect("GetItemsInDockableLocation");
    let PyRep::List(items_list) = dock_items else { panic!("expected list") };
    assert_eq!(items_list.len(), 2, "must include Tritanium + BPC, excluding active ship and locked escrow item");
    assert_eq!(dict_get(&items_list[0], "itemID"), Some(&PyRep::Integer(item_ore)));
    assert_eq!(dict_get(&items_list[0], "stacksize"), Some(&PyRep::Integer(2500)));
    assert_eq!(dict_get(&items_list[0], "categoryID"), Some(&PyRep::Integer(4)));
    assert_eq!(dict_get(&items_list[1], "itemID"), Some(&PyRep::Integer(item_bpc)));
    assert_eq!(dict_get(&items_list[1], "singleton"), Some(&PyRep::Integer(2)));

    // 2. DeleteContractNotification removes outbid auction 7001 from GetMyBids
    let bids_before = call_rpc(&ctx, &mut sess, "contractProxy", "GetMyBids", &[PyRep::Bool(false)]).await.expect("GetMyBids before");
    assert!(matches!(&bids_before, PyRep::List(l) if l.len() == 1));
    let del_c_notif = call_rpc(&ctx, &mut sess, "contractProxy", "DeleteContractNotification", &[PyRep::Integer(7001), PyRep::Integer(0)])
        .await.expect("DeleteContractNotification");
    assert_eq!(del_c_notif, PyRep::Bool(true));
    let bids_after = call_rpc(&ctx, &mut sess, "contractProxy", "GetMyBids", &[PyRep::Bool(false)]).await.expect("GetMyBids after");
    assert!(matches!(&bids_after, PyRep::List(l) if l.is_empty()));
    assert_eq!(ctx.db.with_snapshot(|s| s.contracts[&7001].bid_history.len()), 1, "bid_history must not be mutated");

    // 3. DeleteNotification dismisses assigned contract 7002 from GetLoginInfo().assignedToMe
    let login_before = call_rpc(&ctx, &mut sess, "contractProxy", "GetLoginInfo", &[]).await.expect("GetLoginInfo before");
    assert!(matches!(dict_get(&login_before, "assignedToMe"), Some(PyRep::List(l)) if l.len() == 1));
    let del_notif = call_rpc(&ctx, &mut sess, "contractProxy", "DeleteNotification", &[PyRep::Integer(7002), PyRep::Integer(0)])
        .await.expect("DeleteNotification");
    assert_eq!(del_notif, PyRep::Bool(true));
    let login_after = call_rpc(&ctx, &mut sess, "contractProxy", "GetLoginInfo", &[]).await.expect("GetLoginInfo after");
    assert!(matches!(dict_get(&login_after, "assignedToMe"), Some(PyRep::List(l)) if l.is_empty()));
    assert_eq!(ctx.db.with_snapshot(|s| s.contracts[&7002].assignee_id), char_id, "assignee_id must remain intact");

    // 4. DeleteMultipleContracts returns (deletedIDs, failedIDs), refunds reward, and unlocks item_escrow
    let bal_before = ctx.db.get_character(char_id).unwrap().balance.0;
    let multi_res = call_rpc(&ctx, &mut sess, "contractProxy", "DeleteMultipleContracts", &[PyRep::List(vec![PyRep::Integer(7003), PyRep::Integer(7001), PyRep::Integer(999999)])])
        .await.expect("DeleteMultipleContracts");
    assert_eq!(multi_res, PyRep::Tuple(vec![PyRep::List(vec![PyRep::Integer(7003)]), PyRep::List(vec![PyRep::Integer(7001), PyRep::Integer(999999)])]));
    let bal_after = ctx.db.get_character(char_id).unwrap().balance.0;
    assert_eq!(bal_after - bal_before, IskCents::from_f64_isk(250_000.0).0);
    let restored_item = ctx.db.get_item(item_escrow).unwrap();
    assert_eq!(restored_item.owner_id, char_id);
    assert_eq!(restored_item.flag, InventoryFlag::Hangar);
}

#[tokio::test]
async fn repair_svc_quotes_station_structure_and_fuel_jettison() {
    // parity: rpc:repairSvc.GetRepairQuotes, rpc:repairSvc.RepairItemsInStation
    // parity: rpc:repairSvc.RepairItemsInStructure, rpc:structure.JettisonStructureFuel
    // parity: feat:structures.fuel-jettison
    eve_testkit::attest(&[
        "rpc:repairSvc.GetRepairQuotes",
        "rpc:repairSvc.RepairItemsInStation",
        "rpc:repairSvc.RepairItemsInStructure",
        "rpc:structure.JettisonStructureFuel",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let char_id = 95_200_001i64;
    let corp_id = 98_200_001i64;
    let station_id = 60_003_760i64;
    let structure_id = 1_045_000_000_001i64;
    let ship_id = 950_200_001i64;
    let mod_id = 950_200_002i64;
    let struct_item_id = 950_200_003i64;
    seed_char_and_corp(&ctx, char_id, corp_id, Some(station_id), None, ship_id);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.dynamic_attributes.insert(attr_ids::DAMAGE, 40.0);
            ship.dynamic_attributes.insert(attr_ids::ARMOR_DAMAGE, 20.0);
        }
        let mut mod_rec = make_item(mod_id, 440, char_id, ship_id, InventoryFlag::MedSlot0, 1, true, false);
        mod_rec.dynamic_attributes.insert(attr_ids::DAMAGE, 15.0);
        snap.items.insert(mod_id, mod_rec);

        let mut st_rec = make_item(struct_item_id, 587, char_id, structure_id, InventoryFlag::Hangar, 1, true, false);
        st_rec.dynamic_attributes.insert(attr_ids::DAMAGE, 25.0);
        snap.items.insert(struct_item_id, st_rec);

        snap.structures.insert(structure_id, UpwellStructureRecord {
            structure_id, type_id: 35832, owner_corp_id: corp_id, solar_system_id: 30_000_142,
            name: "Jita Perimeter Astrahus".into(), position: [150_000.0, 0.0, 0.0], state: "ShieldVulnerable".into(),
            quantum_core_installed: true, fuel_blocks: 500, shield_hp: 1_000_000.0, armor_hp: 1_000_000.0,
            hull_hp: 1_000_000.0, reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
        });
        snap.items.insert(950_200_099, make_item(950_200_099, 35895, corp_id, structure_id, InventoryFlag::ServiceSlot0, 1, true, false));
        snap.module_online_states.insert(950_200_099, true);
    });

    let mut sess = SessionState::new(81_002);
    sess.user_id = Some(700);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.station_id = Some(station_id);
    sess.ship_id = Some(ship_id);
    sess.solar_system_id = Some(30_000_142);
    let _ = ctx.db.acquire_online_lock(700, sess.session_id, Some(char_id), true, ctx.now_unix());

    // 1. GetRepairQuotes returns Dict[itemID -> List[KeyVal]]
    let quotes = call_rpc(&ctx, &mut sess, "repairSvc", "GetRepairQuotes", &[PyRep::List(vec![PyRep::Integer(ship_id)])])
        .await.expect("GetRepairQuotes");
    let PyRep::Dict(q_map) = quotes else { panic!("expected dict") };
    let ship_quotes = &q_map.iter().find(|(k, _)| k == &PyRep::Integer(ship_id)).unwrap().1;
    let PyRep::List(q_rows) = ship_quotes else { panic!("expected quote list") };
    assert_eq!(q_rows.len(), 2, "ship + fitted module damage quotes");
    assert_eq!(dict_get(&q_rows[0], "itemID"), Some(&PyRep::Integer(ship_id)));
    assert_eq!(dict_get(&q_rows[0], "damage"), Some(&PyRep::Real(60.0)));
    assert!(matches!(dict_get(&q_rows[0], "costToRepairOneUnitOfDamage"), Some(PyRep::Real(c)) if *c > 0.0));
    assert!(matches!(dict_get(&q_rows[0], "maxHealth"), Some(PyRep::Integer(h)) if *h > 0));

    // 2. RepairItemsInStation deducts ISK, clears damage on ship + module, and emits OnModuleRepaired
    let bal0 = ctx.db.get_character(char_id).unwrap().balance.0;
    let _ = call_rpc(&ctx, &mut sess, "repairSvc", "RepairItemsInStation", &[PyRep::List(vec![PyRep::Integer(ship_id)]), PyRep::Real(5_000_000.0)])
        .await.expect("RepairItemsInStation");
    let bal1 = ctx.db.get_character(char_id).unwrap().balance.0;
    assert!(bal1 < bal0, "RepairItemsInStation must debit repair bill from character wallet");
    assert!(!ctx.db.get_item(ship_id).unwrap().dynamic_attributes.contains_key(&attr_ids::DAMAGE));
    assert!(!ctx.db.get_item(mod_id).unwrap().dynamic_attributes.contains_key(&attr_ids::DAMAGE));
    let notifs = ctx.drain_notifications_for_char(char_id);
    assert!(notifs.iter().any(|n| n.event == "OnModuleRepaired"));

    // 3. Dock in structure and test RepairItemsInStructure + structure.JettisonStructureFuel
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = Some(structure_id);
        }
    });
    sess.station_id = None;
    sess.structure_id = Some(structure_id);

    let _ = call_rpc(&ctx, &mut sess, "repairSvc", "RepairItemsInStructure", &[PyRep::List(vec![PyRep::Integer(struct_item_id)])])
        .await.expect("RepairItemsInStructure");
    assert!(!ctx.db.get_item(struct_item_id).unwrap().dynamic_attributes.contains_key(&attr_ids::DAMAGE));

    // Verify unauthorized member without Station Manager role is rejected
    let member_char_id = 95_200_002i64;
    let member_ship_id = 950_200_004i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.insert(
            member_char_id,
            make_char(member_char_id, corp_id, None, Some(structure_id), member_ship_id),
        );
        if let Some(c) = snap.corporations.get_mut(&corp_id) {
            c.members.insert(member_char_id);
            c.member_roles.insert(member_char_id, 0);
        }
    });

    let mut member_sess = SessionState::new(81_003);
    member_sess.user_id = Some(701);
    member_sess.character_id = Some(member_char_id);
    member_sess.corporation_id = Some(corp_id);
    member_sess.structure_id = Some(structure_id);
    member_sess.solar_system_id = Some(30_000_142);
    member_sess.corp_role = 0;

    let unauth_err = call_rpc(
        &ctx,
        &mut member_sess,
        "structure",
        "JettisonStructureFuel",
        &[PyRep::Integer(structure_id), PyRep::Integer(4247), PyRep::Integer(120)],
    )
    .await
    .expect_err("JettisonStructureFuel without Station Manager role must fail");
    // rpc-lint: allow error code assertion
    assert_eq!(unauth_err.code, "InsufficientRoles");

    // Grant Station Manager role (1 << 11) to member
    const ROLE_STATION_MANAGER: u64 = 1 << 11;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.corporations.get_mut(&corp_id) {
            c.member_roles.insert(member_char_id, ROLE_STATION_MANAGER);
        }
    });
    member_sess.corp_role = ROLE_STATION_MANAGER;

    let jet_res = call_rpc(
        &ctx,
        &mut member_sess,
        "structure",
        "JettisonStructureFuel",
        &[PyRep::Integer(structure_id), PyRep::Integer(4247), PyRep::Integer(120)],
    )
    .await
    .expect("JettisonStructureFuel with Station Manager role");
    let Some(PyRep::Integer(cont_id)) = dict_get(&jet_res, "containerID") else { panic!("containerID") };
    assert_eq!(dict_get(&jet_res, "remainingFuelBlocks"), Some(&PyRep::Integer(380)));
    let PyRep::List(jettisoned_ids) = dict_get(&jet_res, "jettisonedItemIDs").unwrap() else { panic!("jettisonedItemIDs") };
    assert_eq!(jettisoned_ids.len(), 1);
    let Some(PyRep::Integer(spawned_fuel_id)) = jettisoned_ids.first() else { panic!("fuel_id") };
    let moved_fuel = ctx.db.get_item(*spawned_fuel_id).unwrap();
    assert_eq!(moved_fuel.location_id, *cont_id);
    assert_eq!(moved_fuel.flag, InventoryFlag::Cargo);
    assert_eq!(moved_fuel.quantity, 120);

    let bad_fuel = call_rpc(
        &ctx,
        &mut member_sess,
        "structure",
        "JettisonStructureFuel",
        &[PyRep::Integer(structure_id), PyRep::Integer(34), PyRep::Integer(50)],
    )
    .await;
    assert!(matches!(bad_fuel, Err(e) if e.code == "ItemNotFuel"));
}

#[tokio::test]
async fn search_i2_jump_clone_reset_warp_vector_and_dev_indices() {
    // parity: rpc:search.Query, rpc:search.QuickQuery, rpc:i2.GetItem, rpc:i2.IsPrimed
    // parity: rpc:jumpCloneSvc.ResetLastCloneJumpTime, rpc:warpVectorMgr.UseVector
    // parity: rpc:devIndexManager.GetDevelopmentIndicesForSystem
    eve_testkit::attest(&[
        "rpc:search.Query",
        "rpc:search.QuickQuery",
        "rpc:i2.GetItem",
        "rpc:i2.IsPrimed",
        "rpc:jumpCloneSvc.ResetLastCloneJumpTime",
        "rpc:warpVectorMgr.UseVector",
        "rpc:devIndexManager.GetDevelopmentIndicesForSystem",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let char_id = 95_300_001i64;
    let corp_id = 98_300_001i64;
    let ship_id = 950_300_001i64;
    let vector_id = 950_300_099i64;
    seed_char_and_corp(&ctx, char_id, corp_id, None, None, ship_id);

    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.insert(30_000_142, SystemDynamicState {
            solar_system_id: 30_000_142, sov_alliance_id: Some(99_000_777), military_index: 4,
            industrial_index: 3, strategic_index: 5, ..Default::default()
        });
    });

    let sys_handle = ctx.systems.get_or_spawn(30_000_142);
    let _ = sys_handle.spawn_ball(Ball {
        id: ship_id, type_id: 587, position: DVec3::ZERO, velocity: DVec3::ZERO,
        max_velocity: 300.0, inertia: 1.0, mass: 1_000_000.0, radius: 40.0,
        speed_fraction: 0.0, mode: eve_destiny::MovementMode::Stop,
    }).await;
    let _ = sys_handle.spawn_ball(Ball {
        id: vector_id, type_id: 23, position: DVec3::new(10_000.0, 0.0, 0.0), velocity: DVec3::ZERO,
        max_velocity: 0.0, inertia: 1.0, mass: 1_000.0, radius: 50.0,
        speed_fraction: 0.0, mode: eve_destiny::MovementMode::Stop,
    }).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(vector_id, eve_db::InventoryItemRecord {
            item_id: vector_id, type_id: 77_825, owner_id: char_id, location_id: ship_id,
            flag: eve_db::InventoryFlag::Cargo, quantity: 1, singleton: false, version: 1,
            dynamic_attributes: std::collections::HashMap::new(), source_mutaplasmid_id: None,
            insured_payout_isk: 0.0, is_blueprint_copy: false, blueprint_runs_remaining: None,
        });
    });

    let mut sess = SessionState::new(81_003);
    sess.user_id = Some(700);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.ship_id = Some(ship_id);
    sess.solar_system_id = Some(30_000_142);
    let _ = ctx.db.acquire_online_lock(700, sess.session_id, Some(char_id), true, ctx.now_unix());

    // 1. search.Query & search.QuickQuery
    let q_res = call_rpc(&ctx, &mut sess, "search", "Query", &[PyRep::String("Jita".into()), PyRep::List(vec![PyRep::Integer(7), PyRep::Integer(9)])])
        .await.expect("search.Query");
    let PyRep::Dict(q_dict) = q_res else { panic!("expected dict") };
    let sys_matches = &q_dict.iter().find(|(k, _)| k == &PyRep::Integer(7)).unwrap().1;
    assert!(matches!(sys_matches, PyRep::List(l) if l.contains(&PyRep::Integer(30_000_142))));

    let mut kw = HashMap::new();
    kw.insert("exact".into(), PyRep::Integer(1));
    let qq_res = call_rpc_kwargs(&ctx, &mut sess, "search", "QuickQuery", &[PyRep::String("Valkyrie SearchPilot".into()), PyRep::List(vec![PyRep::Integer(2)])], kw)
        .await.expect("search.QuickQuery");
    assert_eq!(qq_res, PyRep::List(vec![PyRep::Integer(char_id)]));

    // 2. i2.IsPrimed & i2.GetItem require GM role
    // rpc-lint: allow devtools svc_dgmattr.pyj AttributeInspector remote call
    let err_prime = call_rpc(&ctx, &mut sess, "i2", "IsPrimed", &[PyRep::Integer(ship_id)]).await;
    assert!(matches!(err_prime, Err(e) if e.code == "GMRoleRequired"));
    // rpc-lint: allow devtools svc_dgmattr.pyj AttributeInspector remote call
    let err_item = call_rpc(&ctx, &mut sess, "i2", "GetItem", &[PyRep::Integer(ship_id)]).await;
    assert!(matches!(err_item, Err(e) if e.code == "GMRoleRequired"));

    sess.role = eve_db::service_roles::ROLE_ADMIN;
    // rpc-lint: allow devtools svc_dgmattr.pyj AttributeInspector remote call
    let primed_yes = call_rpc(&ctx, &mut sess, "i2", "IsPrimed", &[PyRep::Integer(ship_id)])
        .await.expect("i2.IsPrimed(ship_id)");
    assert_eq!(primed_yes, PyRep::Bool(true));
    // rpc-lint: allow devtools svc_dgmattr.pyj AttributeInspector remote call
    let primed_no = call_rpc(&ctx, &mut sess, "i2", "IsPrimed", &[PyRep::Integer(999_999_999)])
        .await.expect("i2.IsPrimed(missing)");
    assert_eq!(primed_no, PyRep::Bool(false));

    // rpc-lint: allow devtools svc_dgmattr.pyj AttributeInspector remote call
    let item_rep = call_rpc(&ctx, &mut sess, "i2", "GetItem", &[PyRep::Integer(ship_id)])
        .await.expect("i2.GetItem(ship_id)");
    assert_eq!(dict_get(&item_rep, "typeID"), Some(&PyRep::Integer(587)));
    assert_eq!(dict_get(&item_rep, "itemID"), Some(&PyRep::Integer(ship_id)));

    // 3. jumpCloneSvc.ResetLastCloneJumpTime requires GM role
    sess.role = eve_db::service_roles::ROLE_PLAYER_DEFAULT;
    let reset_denied = call_rpc(&ctx, &mut sess, "jumpCloneSvc", "ResetLastCloneJumpTime", &[]).await;
    assert!(matches!(reset_denied, Err(e) if e.code == "GMRoleRequired"));

    sess.role = eve_db::service_roles::ROLE_ADMIN;
    assert_eq!(ctx.db.get_character(char_id).unwrap().last_clone_jump_unix, 1_700_000_000);
    let _ = call_rpc(&ctx, &mut sess, "jumpCloneSvc", "ResetLastCloneJumpTime", &[]).await.expect("ResetLastCloneJumpTime");
    assert_eq!(ctx.db.get_character(char_id).unwrap().last_clone_jump_unix, 0);
    let jc_state = call_rpc(&ctx, &mut sess, "jumpCloneSvc", "GetCloneState", &[]).await.expect("GetCloneState");
    assert_eq!(dict_get(&jc_state, "timeLastJump"), Some(&PyRep::None));

    // 4. warpVectorMgr.UseVector enforces scram check and moves ship ball along vector
    sys_handle.try_add_mobile_warp_disruptor(888_001, DVec3::ZERO, 20_000.0);
    let scram_err = call_rpc(&ctx, &mut sess, "warpVectorMgr", "UseVector", &[PyRep::Integer(vector_id)]).await;
    assert!(matches!(scram_err, Err(e) if e.code == "WarpScrambled"));
    sys_handle.try_remove_warp_bubble(888_001);

    let missing_vec = call_rpc(&ctx, &mut sess, "warpVectorMgr", "UseVector", &[PyRep::Integer(888_888_888)]).await;
    assert!(matches!(missing_vec, Err(e) if e.code == "InvalidVectorItem"));

    let vec_res = call_rpc(&ctx, &mut sess, "warpVectorMgr", "UseVector", &[PyRep::Integer(vector_id)]).await.expect("warpVectorMgr.UseVector");
    assert_eq!(dict_get(&vec_res, "shipID"), Some(&PyRep::Integer(ship_id)));
    let (_, after_ball) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    match after_ball.mode {
        eve_destiny::MovementMode::Warp { destination, .. } => {
            assert!(destination.x > 50_000.0, "ship ball must warp along vector (+X)");
        }
        ref other => panic!("ship ball must enter warp along vector (+X), got {other:?}"),
    }

    // 5. devIndexManager.GetDevelopmentIndicesForSystem returns 1583, 1584, 1615
    let dev_res = call_rpc(&ctx, &mut sess, "devIndexManager", "GetDevelopmentIndicesForSystem", &[PyRep::Integer(30_000_142)])
        .await.expect("GetDevelopmentIndicesForSystem");
    let PyRep::Dict(dev_map) = dev_res else { panic!("expected dict") };
    let mil = &dev_map.iter().find(|(k, _)| k == &PyRep::Integer(1583)).unwrap().1;
    assert_eq!(dict_get(mil, "level"), Some(&PyRep::Integer(4)));
    assert_eq!(dict_get(mil, "increasing"), Some(&PyRep::Integer(1)));
    let strat = &dev_map.iter().find(|(k, _)| k == &PyRep::Integer(1615)).unwrap().1;
    assert_eq!(dict_get(strat, "level"), Some(&PyRep::Integer(5)));
}

}


#[allow(unused_imports, dead_code, unused_variables, unused_mut)]
mod rpc_missing_services_ext_b {
    use super::*;

// Integration test suite for final 20 missing RPC rows.
//
// Validates real V24 argument layouts, persistent state mutations,
// and live-server return shapes for all 20 endpoints.


use crate::common;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

const PILOT1: i64 = 90_000_001;
const PILOT2: i64 = 90_000_002;

#[tokio::test]
async fn test_alert_stat_logger_queue_and_tunneling_rpcs() {
    // parity: rpc:alert.LogModeChanged, rpc:alert.SendProxyStackTraceAlert
    // parity: rpc:clientStatLogger.LogString, rpc:externalQueueMgr.PublishProxyEvent
    // parity: rpc:machoNet.ReloadClientCodeHash, rpc:tcpRawProxyService.GetESPTunnelingAddressByNodeID
    eve_testkit::attest(&[
        "rpc:alert.LogModeChanged",
        "rpc:alert.SendProxyStackTraceAlert",
        "rpc:clientStatLogger.LogString",
        "rpc:externalQueueMgr.PublishProxyEvent",
        "rpc:machoNet.ReloadClientCodeHash",
        "rpc:tcpRawProxyService.GetESPTunnelingAddressByNodeID",
        ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let kw = HashMap::new();

    let mut sess1 = SessionState::new(1001);
    common::login_pilot(&ctx, &mut sess1, "demo", "demo-secret").await;
    sess1.role |= eve_db::service_roles::ROLE_GMH | eve_db::service_roles::ROLE_ADMIN;
    ctx.dispatch_rpc(
        &mut sess1,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(PILOT1)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID sess1");

    // 1. rpc:alert.LogModeChanged
    let log_mode_res = ctx
        .dispatch_rpc(
            &mut sess1,
            "alert",
            None,
            "LogModeChanged",
            &[PyRep::Integer(201), PyRep::String("I".into())],
            &kw,
            None,
        )
        .await
        .expect("alert.LogModeChanged");
    assert_eq!(log_mode_res, PyRep::None);

    let get_mode_res = ctx
        .dispatch_rpc(
            &mut sess1,
            "alert",
            None,
            "GetLogModeForError",
            &[PyRep::Integer(201)],
            &kw,
            None,
        )
        .await
        .expect("alert.GetLogModeForError");
    match get_mode_res {
        PyRep::Tuple(items) => {
            assert_eq!(items[0], PyRep::Integer(201));
            assert_eq!(items[1], PyRep::ascii_bytes("I"));
        }
        other => panic!("Unexpected GetLogModeForError shape: {other:?}"),
    }
    let persisted_modes = ctx.db.with_snapshot(|s| {
        s.cluster_and_runtime.alert_log_modes.clone()
    });
    assert!(!persisted_modes.is_empty(), "alert_log_modes must be persisted");
    assert!(persisted_modes.contains_key("201"));

    // 2. rpc:alert.SendProxyStackTraceAlert
    let proxy_alert_res = ctx
        .dispatch_rpc(
            &mut sess1,
            "alert",
            None,
            "SendProxyStackTraceAlert",
            &[
                PyRep::Tuple(vec![PyRep::Integer(777_888), PyRep::String("proxy_key".into())]),
                PyRep::String("Traceback:\n  File foo.py, in bar\nError".into()),
                PyRep::String("B".into()),
            ],
            &kw,
            None,
        )
        .await
        .expect("alert.SendProxyStackTraceAlert");
    match proxy_alert_res {
        PyRep::Tuple(items) => {
            assert!(items[0].as_i64().unwrap_or(0) > 0);
            assert_eq!(items[1], PyRep::ascii_bytes("B"));
        }
        other => panic!("Unexpected SendProxyStackTraceAlert shape: {other:?}"),
    }
    let proxy_traces = ctx.db.with_snapshot(|s| {
        s.cluster_and_runtime.alert_proxy_traces.as_ref().map(|v| v.to_string())
    });
    assert!(proxy_traces.is_some(), "alert_proxy_traces must be persisted");
    assert!(proxy_traces.unwrap().contains("777888"));

    // 3. localizationServer.UpdateLocalizationQAWrap (DERIVED role-gated boolean toggle + GetAllTextChanges wrapping)
    let mut unpriv_loc_sess = SessionState::new(902_099);
    common::login_pilot(&ctx, &mut unpriv_loc_sess, "other", "other-secret").await;
    unpriv_loc_sess.role = 0;
    let unpriv_qa_err = ctx
        .dispatch_rpc(
            &mut unpriv_loc_sess,
            "localizationServer",
            None,
            "UpdateLocalizationQAWrap",
            &[PyRep::Bool(true)],
            &kw,
            None,
        )
        .await
        .expect_err("unprivileged UpdateLocalizationQAWrap must fail with RoleRequired");
    assert_eq!(unpriv_qa_err.code, "RoleRequired");

    ctx.db.with_mut_snapshot(|snap| {
        let mut overrides = std::collections::BTreeMap::new();
        overrides.insert(99991_i64, "Overridden QA String".to_string());
        snap.owner_notes.insert(
            -8_960_000_000_000,
            eve_db::OwnerNoteRecord {
                note_id: -8_960_000_000_000,
                character_id: 0,
                item_id: 0,
                label: "localization_overrides".into(),
                note: serde_json::to_string(&overrides).unwrap(),
                timestamp_unix: 0,
            },
        );
    });

    // Enable QA wrap for sess1 and verify GetAllTextChanges wraps localized strings
    assert_eq!(
        ctx.dispatch_rpc(&mut sess1, "localizationServer", None, "UpdateLocalizationQAWrap", &[PyRep::Bool(true)], &kw, None).await.expect("UpdateLocalizationQAWrap(true)"),
        PyRep::Bool(true)
    );
    let wrapped_changes = ctx.dispatch_rpc(&mut sess1, "localizationServer", None, "GetAllTextChanges", &[], &kw, None).await.expect("GetAllTextChanges wrapped");
    assert_eq!(
        match &wrapped_changes {
            PyRep::Dict(entries) => entries.iter().find(|(k, _)| k.as_i64() == Some(99991)).map(|(_, v)| v),
            _ => None,
        },
        Some(&PyRep::String("[QA:99991] Overridden QA String".into()))
    );
    assert_eq!(
        ctx.dispatch_rpc(&mut sess1, "localizationServer", None, "UpdateLocalizationQAWrap", &[PyRep::Bool(false)], &kw, None).await.expect("UpdateLocalizationQAWrap(false)"),
        PyRep::Bool(false)
    );
    let unwrapped_changes = ctx.dispatch_rpc(&mut sess1, "localizationServer", None, "GetAllTextChanges", &[], &kw, None).await.expect("GetAllTextChanges unwrapped");
    assert_eq!(
        match &unwrapped_changes {
            PyRep::Dict(entries) => entries.iter().find(|(k, _)| k.as_i64() == Some(99991)).map(|(_, v)| v),
            _ => None,
        },
        Some(&PyRep::String("Overridden QA String".into()))
    );

    // 4. rpc:localizationServer.ReloadFSDPickle
    let reload_res = ctx
        .dispatch_rpc(
            &mut sess1,
            "localizationServer",
            None,
            "ReloadFSDPickle",
            &[PyRep::String("fsd_english.pickle".into())],
            &kw,
            None,
        )
        .await;
    assert!(reload_res.is_err(), "ReloadFSDPickle must return an error until implemented");
    if let Err(e) = reload_res {
        assert_eq!(e.code, "UnimplementedRPC");
    }

    // 5. rpc:clientStatLogger.LogString
    let log_str_res = ctx
        .dispatch_rpc(
            &mut sess1,
            "clientStatLogger",
            None,
            "LogString",
            &[PyRep::String("jump_monitor:jump_done:30000142".into())],
            &kw,
            None,
        )
        .await
        .expect("clientStatLogger.LogString");
    assert_eq!(log_str_res, PyRep::None);
    let logged_strings = ctx.db.with_snapshot(|s| {
        s.cluster_and_runtime.client_stat_logger_strings.clone()
    });
    assert!(logged_strings.iter().any(|s| s.contains("jump_monitor:jump_done:30000142")));

    // 6. rpc:externalQueueMgr.PublishProxyEvent (7 args)
    let queue_res = ctx
        .dispatch_rpc(
            &mut sess1,
            "externalQueueMgr",
            None,
            "PublishProxyEvent",
            &[
                PyRep::String("proxy_session_init".into()),
                PyRep::String("session_token_123".into()),
                PyRep::Integer(1),
                PyRep::Integer(2),
                PyRep::None,
                PyRep::None,
                PyRep::Dict(vec![]),
            ],
            &kw,
            None,
        )
        .await
        .expect("externalQueueMgr.PublishProxyEvent");
    assert_eq!(queue_res, PyRep::None);
    let queue_blob = ctx.db.with_snapshot(|s| {
        serde_json::to_string(&s.cluster_and_runtime.external_queue_events).ok()
    });
    assert!(ctx.db.with_snapshot(|s| !s.cluster_and_runtime.external_queue_events.is_empty()));
    assert!(queue_blob.unwrap().contains("proxy_session_init"));

    // 7. rpc:machoNet.ReloadClientCodeHash (0 args)
    let hash_res = ctx
        .dispatch_rpc(
            &mut sess1,
            "machoNet",
            None,
            "ReloadClientCodeHash",
            &[],
            &kw,
            None,
        )
        .await
        .expect("machoNet.ReloadClientCodeHash");
    match hash_res {
        PyRep::String(s) => assert!(!s.is_empty()),
        other => panic!("Unexpected ReloadClientCodeHash shape: {other:?}"),
    }
    let has_hash = ctx.db.with_snapshot(|s| {
        s.cluster_and_runtime.macho_client_code_hash.is_some()
    });
    assert!(has_hash);

    // 8. rpc:tcpRawProxyService.GetESPTunnelingAddressByNodeID (0 args -> (default_addr, node_map))
    ctx.db.with_mut_snapshot(|s| {
        let mut table = std::collections::BTreeMap::new();
        table.insert("42".to_string(), "10.0.0.42:28042".to_string());
        s.cluster_and_runtime.cluster_node_table = Some(table);
    });
    let tunnel_res = ctx
        .dispatch_rpc(
            &mut sess1,
            "tcpRawProxyService",
            None,
            "GetESPTunnelingAddressByNodeID",
            &[],
            &kw,
            None,
        )
        .await
        .expect("tcpRawProxyService.GetESPTunnelingAddressByNodeID");
    match tunnel_res {
        PyRep::Tuple(items) => {
            assert_eq!(items.len(), 2);
            assert!(items[0].as_str().is_some());
            match &items[1] {
                PyRep::Dict(map) => {
                    let entry = map.iter().find(|(k, _)| k.as_i64() == Some(42));
                    assert_eq!(entry.map(|(_, v)| v), Some(&PyRep::String("10.0.0.42:28042".into())));
                }
                other => panic!("Unexpected node map shape: {other:?}"),
            }
        }
        other => panic!("Unexpected GetESPTunnelingAddressByNodeID shape: {other:?}"),
    }

    // 9. rpc:loadService.StartLoad (1 arg: loadSpec)
    let target_sys = sess1.solar_system_id.unwrap_or(30_000_142);
    let elapsed_before = ctx.systems.get_system_sim_elapsed(target_sys);
    let start_load_res = ctx
        .dispatch_rpc(
            &mut sess1,
            "loadService",
            None,
            "StartLoad",
            &[PyRep::Dict(vec![
                (PyRep::String("profile".into()), PyRep::String("load_profile_std".into())),
                (PyRep::String("ticks".into()), PyRep::Integer(4)),
            ])],
            &kw,
            None,
        )
        .await
        .expect("loadService.StartLoad");
    assert_eq!(start_load_res, PyRep::None);
    let elapsed_after = ctx.systems.get_system_sim_elapsed(target_sys);
    assert!(
        elapsed_after >= elapsed_before + 4.0,
        "StartLoad must step target system (before={elapsed_before}, after={elapsed_after})"
    );
    let load_state = ctx.db.with_snapshot(|s| s.cluster_and_runtime.load_service_state.clone());
    assert!(load_state.active);
    assert_eq!(load_state.ticks_executed, 4);

    // 10. rpc:loadService.StopLoad
    let stop_load_res = ctx
        .dispatch_rpc(&mut sess1, "loadService", None, "StopLoad", &[], &kw, None)
        .await
        .expect("loadService.StopLoad");
    assert_eq!(stop_load_res, PyRep::None);
    let load_active2 = ctx.db.with_snapshot(|s| s.cluster_and_runtime.load_service_state.active);
    assert!(!load_active2);
}

#[tokio::test]
async fn test_session_mgr_user_svc_and_db2_rpcs() {
    // parity: rpc:sessionMgr.EndAllGameSessions, rpc:sessionMgr.RemoveSessionsFromServer
    // parity: rpc:userSvc.ProcessEVESessionCreated, rpc:DB2.CallProc
    // DATA GAP: Triaged NA in docs/parity/triage.json (dev/QA/cluster RPCs): rpc:debug.Exec
    eve_testkit::attest(&[
        "rpc:sessionMgr.EndAllGameSessions",
        "rpc:sessionMgr.RemoveSessionsFromServer",
        "rpc:userSvc.ProcessEVESessionCreated",
        "rpc:DB2.CallProc",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let kw = HashMap::new();

    let mut sess1 = SessionState::new(1001);
    common::login_pilot(&ctx, &mut sess1, "demo", "demo-secret").await;
    sess1.role |= eve_db::service_roles::ROLE_ADMIN;
    ctx.dispatch_rpc(
        &mut sess1,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(PILOT1)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID sess1");

    let mut sess2 = SessionState::new(1002);
    common::login_pilot(&ctx, &mut sess2, "other", "other-secret").await;
    ctx.dispatch_rpc(
        &mut sess2,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(PILOT2)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID sess2");

    // 11. rpc:loadService.SetConfig
    let set_config_res = ctx
        .dispatch_rpc(
            &mut sess1,
            "loadService",
            None,
            "SetConfig",
            &[PyRep::String("concurrency".into()), PyRep::Integer(16)],
            &kw,
            None,
        )
        .await
        .expect("loadService.SetConfig");
    assert_eq!(set_config_res, PyRep::None);

    // 12. rpc:loadService.GetTotalStats
    let stats_res = ctx
        .dispatch_rpc(&mut sess1, "loadService", None, "GetTotalStats", &[], &kw, None)
        .await
        .expect("loadService.GetTotalStats");
    assert_eq!(stats_res.dict_get("active"), Some(&PyRep::Bool(false)));
    assert!(stats_res.dict_get("sde_types_count").and_then(|v| v.as_i64()).unwrap_or(0) > 0);
    assert!(stats_res.dict_get("sde_solar_systems_count").and_then(|v| v.as_i64()).unwrap_or(0) > 0);
    let cfg = stats_res.dict_get("config").expect("config dict");
    assert_eq!(cfg.dict_get("concurrency"), Some(&PyRep::Integer(16)));

    // 13. rpc:sessionMgr.EndAllGameSessions
    let (tx_victim, mut rx_victim) = tokio::sync::mpsc::unbounded_channel();
    ctx.register_session_closer(2002, tx_victim);
    assert!(ctx.session_closers.read().unwrap().contains_key(&2002));
    let end_sessions_res = ctx
        .dispatch_rpc(&mut sess1, "sessionMgr", None, "EndAllGameSessions", &[], &kw, None)
        .await
        .expect("sessionMgr.EndAllGameSessions");
    assert_eq!(end_sessions_res, PyRep::None);
    assert_eq!(
        rx_victim.try_recv().expect("victim session must receive EndAllGameSessions"),
        ("EndAllGameSessions".into(), "All game sessions ended by administrator".into())
    );
    assert!(!ctx.session_closers.read().unwrap().contains_key(&2002));
    let active_empty = ctx.db.with_snapshot(|s| s.cluster_and_runtime.active_sessions.is_empty());
    assert!(active_empty);

    // 14. rpc:sessionMgr.RemoveSessionsFromServer
    let unpriv_rm_err = ctx
        .dispatch_rpc(&mut sess2, "sessionMgr", None, "RemoveSessionsFromServer", &[PyRep::Integer(1), PyRep::List(vec![PyRep::Integer(1001)])], &kw, None)
        .await;
    assert!(unpriv_rm_err.is_err(), "Unprivileged session must be rejected for RemoveSessionsFromServer");

    let (tx_1002, mut rx_1002) = tokio::sync::mpsc::unbounded_channel();
    let (tx_1003, mut rx_1003) = tokio::sync::mpsc::unbounded_channel();
    ctx.register_session_closer(1002, tx_1002);
    ctx.register_session_closer(1003, tx_1003);
    ctx.db.with_mut_snapshot(|s| {
        s.cluster_and_runtime.active_sessions = vec![1001, 1002, 1003];
    });
    let rm_sessions_res = ctx
        .dispatch_rpc(&mut sess1, "sessionMgr", None, "RemoveSessionsFromServer", &[PyRep::Integer(1), PyRep::List(vec![PyRep::Integer(1002), PyRep::Integer(1003)])], &kw, None)
        .await
        .expect("sessionMgr.RemoveSessionsFromServer");
    assert_eq!(rm_sessions_res, PyRep::None);
    assert_eq!(rx_1002.try_recv().expect("1002 closed").0, "RemoveSession");
    assert_eq!(rx_1003.try_recv().expect("1003 closed").0, "RemoveSession");
    assert!(!ctx.session_closers.read().unwrap().contains_key(&1002));
    assert!(!ctx.session_closers.read().unwrap().contains_key(&1003));
    let remaining_sessions = ctx.db.with_snapshot(|s| s.cluster_and_runtime.active_sessions.clone());
    assert!(remaining_sessions.contains(&1001));
    assert!(!remaining_sessions.contains(&1002));

    // 15. rpc:userSvc.ProcessEVESessionCreated
    let wrong_char_err = ctx
        .dispatch_rpc(&mut sess2, "userSvc", None, "ProcessEVESessionCreated", &[PyRep::Integer(PILOT1)], &kw, None)
        .await
        .expect_err("foreign character_id must be rejected even when session.character_id is set");
    assert_eq!(wrong_char_err.message, "CharacterDoesNotBelongToUser");

    let process_sess_res = ctx
        .dispatch_rpc(&mut sess1, "userSvc", None, "ProcessEVESessionCreated", &[PyRep::Integer(PILOT1), PyRep::Integer(1001)], &kw, None)
        .await
        .expect("userSvc.ProcessEVESessionCreated");
    assert_eq!(process_sess_res, PyRep::None);
    let entries = ctx.db.with_snapshot(|s| s.cluster_and_runtime.user_sessions.clone());
    assert!(entries.iter().any(|e| {
        e["user_id"].as_u64() == sess1.user_id
            && e["session_id"].as_u64() == Some(1001)
            && e["character_id"].as_i64() == Some(PILOT1)
    }));

    // 16. rpc:cache.Rowset
    let cache_rowset_res = ctx
        .dispatch_rpc(
            &mut sess1,
            "cache",
            None,
            "Rowset",
            &[PyRep::String("roles".into())],
            &kw,
            None,
        )
        .await
        .expect("cache.Rowset");
    match cache_rowset_res {
        PyRep::Rowset { columns, rows } => {
            assert!(columns.contains(&"roleID".to_string()));
            assert!(columns.contains(&"roleName".to_string()));
            assert!(!rows.is_empty());
        }
        other => panic!("Unexpected cache.Rowset shape: {other:?}"),
    }

    // 17. rpc:cache.Setting
    let cache_setting_res = ctx
        .dispatch_rpc(
            &mut sess1,
            "cache",
            None,
            "Setting",
            &[PyRep::String("zsystem".into()), PyRep::String("SP-Allowed-Extensions".into())],
            &kw,
            None,
        )
        .await
        .expect("cache.Setting");
    match cache_setting_res {
        PyRep::String(exts) => {
            assert!(exts.contains("txt"));
            assert!(exts.contains("csv"));
        }
        other => panic!("Unexpected cache.Setting shape: {other:?}"),
    }
    let custom_set_res = ctx
        .dispatch_rpc(
            &mut sess1,
            "cache",
            None,
            "Setting",
            &[
                PyRep::String("zsystem".into()),
                PyRep::String("CustomTestKey".into()),
                PyRep::String("CustomTestVal".into()),
            ],
            &kw,
            None,
        )
        .await
        .expect("cache.Setting update");
    assert_eq!(custom_set_res, PyRep::String("CustomTestVal".into()));

    // 18. rpc:raffleMgr.QA_SeedRaffles
    // Verify unprivileged rejection
    let unpriv_seed_err = ctx
        .dispatch_rpc(&mut sess2, "raffleMgr", None, "QA_SeedRaffles", &[PyRep::Integer(1)], &kw, None)
        .await;
    assert!(unpriv_seed_err.is_err(), "Unprivileged session must be rejected for QA_SeedRaffles");

    let raffle_seed_res = ctx
        .dispatch_rpc(
            &mut sess1,
            "raffleMgr",
            None,
            "QA_SeedRaffles",
            &[PyRep::Integer(2)],
            &kw,
            None,
        )
        .await
        .expect("raffleMgr.QA_SeedRaffles");
    match raffle_seed_res {
        PyRep::List(items) => {
            assert_eq!(items.len(), 2);
        }
        other => panic!("Unexpected QA_SeedRaffles shape: {other:?}"),
    }
    let (seeded_notes, item_is_escrowed) = ctx.db.with_snapshot(|s| {
        let count = s.owner_notes.values().filter(|n| n.label.starts_with("raffle:")).count();
        let escrowed = s.items.values().any(|it| {
            it.owner_id == -52_568 && it.location_id == -52_568 && it.flag == eve_db::InventoryFlag::Locked
        });
        (count, escrowed)
    });
    assert!(seeded_notes >= 2);
    assert!(item_is_escrowed, "Seeded raffle item must be escrowed at -52_568 with Locked flag");

    // 19. rpc:DB2.CallProc (2 args: proc_name, role_mask)
    let mut unauth = SessionState::new(9998);
    let attack_args = [
        PyRep::String("zcluster.Attacks_Insert".into()),
        PyRep::String("attack_data".into()),
        PyRep::String("test".into()),
    ];
    let unauth_attacks_err = ctx
        .dispatch_rpc(&mut unauth, "DB2", None, "CallProc", &attack_args, &kw, None)
        .await;
    assert!(unauth_attacks_err.is_err(), "Unauthenticated session must be rejected for DB2.CallProc zcluster.Attacks_Insert");

    let mut unpriv = SessionState::new(9999);
    unpriv.user_id = Some(8888);
    unpriv.role = eve_db::service_roles::ROLE_PLAYER_DEFAULT;
    let auth_err = ctx
        .dispatch_rpc(&mut unpriv, "DB2", None, "CallProc", &[PyRep::String("zcluster.Nodes_PolarisID".into())], &kw, None)
        .await;
    assert!(auth_err.is_err(), "Non-admin must be rejected for DB2.CallProc zcluster.Nodes_PolarisID");

    let attacks_ok = ctx
        .dispatch_rpc(&mut unpriv, "DB2", None, "CallProc", &attack_args, &kw, None)
        .await
        .expect("Authenticated non-admin must be allowed to call DB2.CallProc zcluster.Attacks_Insert");
    assert_eq!(attacks_ok, PyRep::None);

    let polaris_res = ctx
        .dispatch_rpc(
            &mut sess1,
            "DB2",
            None,
            "CallProc",
            &[PyRep::String("zcluster.Nodes_PolarisID".into())],
            &kw,
            None,
        )
        .await
        .expect("DB2.CallProc polaris");
    assert_eq!(polaris_res, PyRep::Integer(1));

    let users_res = ctx
        .dispatch_rpc(
            &mut sess1,
            "DB2",
            None,
            "CallProc",
            &[
                PyRep::String("zuser.Users_SelectByRole".into()),
                PyRep::Integer(eve_db::service_roles::ROLE_PLAYER_DEFAULT as i64),
            ],
            &kw,
            None,
        )
        .await
        .expect("DB2.CallProc select users");
    match users_res {
        PyRep::Rowset { columns, rows } => {
            assert!(columns.contains(&"userID".to_string()));
            assert!(columns.contains(&"userName".to_string()));
            assert!(!rows.is_empty());
        }
        other => panic!("Unexpected Users_SelectByRole shape: {other:?}"),
    }

    // 20. rpc:debug.Exec (5 args + kwargs)
    let debug_auth_err = ctx
        .dispatch_rpc(&mut unpriv, "debug", None, "Exec", &[PyRep::String("print('hello')".into())], &kw, None)
        .await;
    assert!(debug_auth_err.is_err(), "Non-admin/programmer must be rejected for debug.Exec");

    let debug_err = ctx
        .dispatch_rpc(
            &mut sess1,
            "debug",
            None,
            "Exec",
            &[
                PyRep::String("self.stats()".into()),
                PyRep::Integer(1),
                PyRep::None,
                PyRep::String("extra".into()),
                PyRep::Bool(true),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("debug.Exec must fail closed with UnimplementedRPC");
    assert_eq!(debug_err.code, "UnimplementedRPC");
}

}


#[allow(unused_imports, dead_code, unused_variables, unused_mut)]
mod rpc_notification_services_ext {
    use super::*;

// Parity test suite for Full RPC & Notification Closure (`Z-1` & `Z-2`).


use crate::common;

use eve_machonet::{MachoPacket, MachoPacketType, SessionState};
use eve_proto::{marshal, PyRep};
use eve_server::router::{
    cache_notifications::{NotificationTarget, MAX_PENDING_NOTIFICATIONS_PER_CHAR},
    ServerContext,
};
use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;

fn root_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

#[tokio::test]
// parity: plan:P-2, plan:P-3, plan:P-4
async fn test_systematic_rpc_catalog_dispatch_and_wire_formatting() {
    // parity: rpc:charUnboundMgr.GetCharacterInfo, rpc:charUnboundMgr.SelectCharacterID, rpc:corpRegistry.AddCorporation, rpc:account.GetWalletDivisionsInfo, rpc:marketProxy.GetCharOrders, rpc:marketProxy.GetOrders, rpc:slash.SlashCmd, rpc:stationSvc.GetStation
    let ctx = ServerContext::new();
    ctx.seed_demo_world_fixtures();
    let empty_kw = HashMap::new();

    // 1. Load the authoritative RPC service catalog (216 machoNet services)
    let catalog_path = root_dir().join("docs/index/rpc_service_catalog.json");
    let catalog_raw = std::fs::read_to_string(&catalog_path)
        .unwrap_or_else(|_| panic!("Failed to read {}", catalog_path.display()));
    let catalog: serde_json::Value =
        serde_json::from_str(&catalog_raw).expect("Parse rpc_service_catalog.json");
    let catalog_map = catalog.as_object().expect("Catalog JSON must be an object");

    assert_eq!(
        catalog_map.len(),
        216,
        "Authoritative catalog must contain exactly 216 services"
    );

    // 2. Setup an authenticated pilot session (character 90000001 in solar system 30000142, docked at 60003760)
    let mut sess = SessionState::new(101);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &empty_kw,
        Some("test:Z-1:SelectCharacterID"),
    )
    .await
    .expect("SelectCharacterID");

    assert_eq!(sess.character_id, Some(90_000_001));
    assert_eq!(sess.solar_system_id, Some(30_000_142));

    // 3. Systematically sweep every catalog service and every client candidate method (1,230 methods across 183 services)
    let mut total_candidate_methods = 0usize;
    let mut total_services_tested = 0usize;
    let mut legacy_services_tested = 0usize;
    let mut concrete_ok_methods = 0usize;
    let mut domain_error_methods = 0usize;
    let mut unimplemented_methods = 0usize;

    for (service_name, service_meta) in catalog_map {
        total_services_tested += 1;
        let surface_cat = service_meta
            .get("surface_category")
            .and_then(|v| v.as_str())
            .unwrap_or("v24_active");
        let is_legacy = surface_cat == "crucible_legacy_only";
        if is_legacy {
            legacy_services_tested += 1;
        }

        let cands: Vec<String> = service_meta
            .get("v24_client_candidate_methods")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|s| s.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();

        for method in &cands {
            total_candidate_methods += 1;

            // Dispatch each candidate method with empty arguments on an authenticated session copy
            let mut sweep_sess = sess.clone();
            let res = ctx
                .dispatch_rpc(
                    &mut sweep_sess,
                    service_name,
                    None,
                    method,
                    &[],
                    &empty_kw,
                    Some("test:Z-1:SystematicSweep"),
                )
                .await;

            match res {
                Ok(pyrep) => {
                    concrete_ok_methods += 1;
                    // Verify wire-level MachoPacket CallRsp encoding and binary marshal round-trip
                    let (pkt, marshaled) = ServerContext::build_macho_call_response_packet(
                        Some(service_name),
                        101,
                        total_candidate_methods as u64,
                        sess.user_id,
                        pyrep.clone(),
                    );
                    assert_eq!(pkt.packet_type, MachoPacketType::CallRsp);
                    let decoded_rep = marshal::decode(&marshaled).expect("Decode CallRsp wire bytes");
                    let decoded_pkt =
                        MachoPacket::from_pyrep(&decoded_rep).expect("Parse CallRsp MachoPacket");
                    assert_eq!(decoded_pkt.packet_type, MachoPacketType::CallRsp);
                    assert_eq!(decoded_pkt.user_id, sess.user_id);
                }
                Err(err) => {
                    assert!(
                        !err.code.is_empty(),
                        "RPC error code must not be empty for {service_name}.{method}"
                    );
                    assert!(
                        !err.message.is_empty(),
                        "RPC error message must not be empty for {service_name}.{method}"
                    );

                    if err.code == "UnimplementedRPC" {
                        unimplemented_methods += 1;
                    } else {
                        domain_error_methods += 1;
                    }

                    // For legacy Crucible services, verify they strictly return UnimplementedRPC
                    if is_legacy {
                        assert_eq!(
                            err.code, "UnimplementedRPC",
                            "Legacy service {service_name}.{method} must return UnimplementedRPC"
                        );
                    }

                    // Verify wire-level MachoPacket ErrorResponse encoding and binary marshal round-trip
                    let (pkt, marshaled) = ServerContext::build_macho_error_response_packet(
                        Some(service_name),
                        101,
                        total_candidate_methods as u64,
                        sess.user_id,
                        &err.code,
                        &err.message,
                    );
                    assert_eq!(pkt.packet_type, MachoPacketType::ErrorResponse);
                    let decoded_rep =
                        marshal::decode(&marshaled).expect("Decode ErrorResponse wire bytes");
                    let decoded_pkt = MachoPacket::from_pyrep(&decoded_rep)
                        .expect("Parse ErrorResponse MachoPacket");
                    assert_eq!(decoded_pkt.packet_type, MachoPacketType::ErrorResponse);
                }
            }
        }
    }

    assert_eq!(total_services_tested, 216);
    assert_eq!(legacy_services_tested, 23);
    assert_eq!(
        total_candidate_methods, 1230,
        "Must systematically sweep all 1,230 cataloged candidate methods"
    );
    assert!(concrete_ok_methods > 0, "Must have concrete handled RPC methods");
    assert!(domain_error_methods > 0, "Must have domain error RPC methods");
    assert!(unimplemented_methods > 0, "Must have structured UnimplementedRPC fallback methods");

    // Re-dock pilot at Jita 4-4 for station-bound service testing following the systematic navigation sweep
    let _ = ctx.db.dock_character(90_000_001, 60_003_760, false); ctx.advance_secs(12);

    // 4. Concrete functional verification across major domain services:
    // a) charUnboundMgr.GetCharactersToSelect
    let chars_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "GetCharactersToSelect",
            &[],
            &empty_kw,
            None,
        )
        .await
        .expect("charUnboundMgr.GetCharactersToSelect");
    assert!(matches!(
        chars_rep,
        PyRep::Rowset { .. } | PyRep::List(_) | PyRep::Object { .. } | PyRep::ObjectEx { .. }
    ));

    // b) charUnboundMgr.GetCharacterInfo
    let char_info = ctx
        .dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "GetCharacterInfo",
            &[PyRep::Integer(90_000_001)],
            &empty_kw,
            None,
        )
        .await
        .expect("charUnboundMgr.GetCharacterInfo");
    assert!(matches!(char_info, PyRep::KeyVal(_) | PyRep::Object { .. }));

    // c) stationSvc.GetStation
    let st_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "stationSvc",
            None,
            "GetStation",
            &[PyRep::Integer(60_003_760)],
            &empty_kw,
            None,
        )
        .await
        .expect("stationSvc.GetStation");
    assert!(matches!(st_rep, PyRep::KeyVal(_)));

    // d) fleetMgr.CreateFleet & GetFleet
    let fleet_rep = ctx
        .dispatch_rpc(&mut sess, "fleetMgr", None, "CreateFleet", &[], &empty_kw, None)
        .await
        .expect("fleetMgr.CreateFleet");
    assert!(matches!(fleet_rep, PyRep::KeyVal(_)));
    let get_fleet = ctx
        .dispatch_rpc(&mut sess, "fleetMgr", None, "GetFleet", &[], &empty_kw, None)
        .await
        .expect("fleetMgr.GetFleet");
    assert!(matches!(get_fleet, PyRep::KeyVal(_)));

    // e) corpmgr.GetMembers, corpRegistry.AddCorporation, & account.GetWalletDivisionsInfo
    let corp_members = ctx
        .dispatch_rpc(&mut sess, "corpmgr", None, "GetMembers", &[], &empty_kw, None)
        .await
        .expect("corpmgr.GetMembers");
    assert!(matches!(corp_members, PyRep::Rowset { .. }));

    let create_corp = ctx
        .dispatch_rpc(
            &mut sess,
            "corpRegistry",
            None,
            "AddCorporation",
            &[
                PyRep::String("Z1 Closure Corp".into()),
                PyRep::String("Z1CC".into()),
                PyRep::String("Z1 Corp Description".into()),
            ],
            &empty_kw,
            None,
        )
        .await
        .expect("corpRegistry.AddCorporation");
    assert!(matches!(create_corp, PyRep::KeyVal(_)));

    let corp_wallets = ctx
        .dispatch_rpc(
            &mut sess,
            "account",
            None,
            "GetWalletDivisionsInfo",
            &[],
            &empty_kw,
            None,
        )
        .await
        .expect("account.GetWalletDivisionsInfo");
    assert!(matches!(corp_wallets, PyRep::Rowset { .. }));

    // f) LSC.JoinChannel & SendMessage
    let join_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "LSC",
            None,
            "JoinChannel",
            &[PyRep::String("test_chan".into())],
            &empty_kw,
            None,
        )
        .await
        .expect("LSC.JoinChannel");
    assert!(matches!(join_rep, PyRep::KeyVal(_)));

    let msg_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "LSC",
            None,
            "SendMessage",
            &[
                PyRep::String("test_chan".into()),
                PyRep::String("Hello from Z-1!".into()),
            ],
            &empty_kw,
            None,
        )
        .await
        .expect("LSC.SendMessage");
    assert!(matches!(msg_rep, PyRep::KeyVal(_)));

    // g) mailMgr.GetHeaders
    let mail_headers = ctx
        .dispatch_rpc(&mut sess, "mailMgr", None, "GetHeaders", &[], &empty_kw, None)
        .await
        .expect("mailMgr.GetHeaders");
    assert!(matches!(mail_headers, PyRep::Rowset { .. }));

    // h) bookmark.GetBookmarks
    let bms = ctx
        .dispatch_rpc(&mut sess, "bookmark", None, "GetBookmarks", &[], &empty_kw, None)
        .await
        .expect("bookmark.GetBookmarks");
    assert!(matches!(bms, PyRep::Rowset { .. }));

    // i) marketProxy.GetOrders & marketProxy.GetCharOrders
    let orders = ctx
        .dispatch_rpc(
            &mut sess,
            "marketProxy",
            None,
            "GetOrders",
            &[PyRep::Integer(34)],
            &empty_kw,
            None,
        )
        .await
        .expect("marketProxy.GetOrders");
    assert!(matches!(orders, PyRep::Tuple(ref items) if items.len() == 2));

    let char_orders = ctx
        .dispatch_rpc(
            &mut sess,
            "marketProxy",
            None,
            "GetCharOrders",
            &[],
            &empty_kw,
            None,
        )
        .await
        .expect("marketProxy.GetCharOrders");
    assert!(matches!(char_orders, PyRep::Rowset { .. }));

    // j) charFittingMgr.GetFittings
    let fittings = ctx
        .dispatch_rpc(
            &mut sess,
            "charFittingMgr",
            None,
            "GetFittings",
            &[],
            &empty_kw,
            None,
        )
        .await
        .expect("charFittingMgr.GetFittings");
    assert!(matches!(fittings, PyRep::Dict(_)));

    // 5. Test MachoBindObject with piggybacked method call
    let bind_res = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "MachoBindObject",
            &[
                PyRep::Integer(60_003_760),
                PyRep::Tuple(vec![
                    PyRep::String("List".into()),
                    PyRep::Tuple(vec![]),
                    PyRep::Dict(vec![]),
                ]),
            ],
            &empty_kw,
            None,
        )
        .await
        .expect("MachoBindObject piggyback call");
    assert!(matches!(bind_res, PyRep::Tuple(ref items) if items.len() == 2));

    // 6. Test security & access controls:
    // a) Unauthenticated call to protected service
    let mut unauth_sess = SessionState::new(999);
    let unauth_err = ctx
        .dispatch_rpc(
            &mut unauth_sess,
            "charUnboundMgr",
            None,
            "GetCharacterInfo",
            &[],
            &empty_kw,
            None,
        )
        .await
        .expect_err("Unauthenticated protected call must fail");
    assert_eq!(unauth_err.code, "Unauthorized");

    // b) Non-GM call to GM slash command
    let gm_err = ctx
        .dispatch_rpc(
            &mut sess,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String("/giveisk 1000000".into())],
            &empty_kw,
            None,
        )
        .await
        .expect_err("Non-GM slash command call must fail");
    assert_eq!(gm_err.code, "InsufficientRole");
}

#[tokio::test]
// parity: plan:P-5
async fn test_z2_systematic_notification_emission_pipelines_and_scope_multicast() {
    // parity: rpc:charUnboundMgr.SelectCharacterID
    let ctx = ServerContext::new();
    ctx.seed_demo_world_fixtures();
    let empty_kw = HashMap::new();

    // 1. Provision & login 3 distinct pilots for multi-target multicast testing:
    // Pilot 1 (Demo, char 90000001, corp 98000001, sys 30000142, station 60003760)
    let mut sess1 = SessionState::new(201);
    common::login_pilot(&ctx, &mut sess1, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut sess1,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &empty_kw,
        None,
    )
    .await
    .expect("Select Pilot 1");

    // Pilot 2 (Rival/Other, char 90000002, corp 98000001, sys 30000142, station 60003760)
    let mut sess2 = SessionState::new(202);
    common::login_pilot(&ctx, &mut sess2, "other", "other-secret").await;
    ctx.dispatch_rpc(
        &mut sess2,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_002)],
        &empty_kw,
        None,
    )
    .await
    .expect("Select Pilot 2");

    // Pilot 3 (Third pilot, char 90000003 in DIFFERENT solar system 30000144 and different corp 98000002)
    let p3_uid = ctx
        .db
        .provision_player_account("pilot3", "pilot3-secret");
    let p3_char = ctx
        .db
        .create_character(
            p3_uid,
            "Pilot Three",
            1,
            1,
            11,
            17,
            &ctx.sde,
        )
        .expect("create character for pilot3");
    let p3_char_id = p3_char.character_id;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&p3_char_id) {
            ch.solar_system_id = 30_000_144;
            ch.corporation_id = 98_000_002;
            ch.station_id = Some(60_003_754);
        }
    });

    let mut sess3 = SessionState::new(203);
    common::login_pilot(&ctx, &mut sess3, "pilot3", "pilot3-secret").await;
    ctx.dispatch_rpc(
        &mut sess3,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(p3_char_id)],
        &empty_kw,
        None,
    )
    .await
    .expect("Select Pilot 3");

    // Align Pilot 1 and Pilot 2 to Corporation 98000001 (Pilot 3 is in 98000002)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.corporation_id = 98_000_001;
            ch.station_id = Some(60_003_760);
        }
        if let Some(ch) = snap.characters.get_mut(&90_000_002) {
            ch.corporation_id = 98_000_001;
            ch.station_id = Some(60_003_760);
        }
    });

    // Drain any bootstrap notifications
    let _ = ctx.drain_notifications_for_char(90_000_001);
    let _ = ctx.drain_notifications_for_char(90_000_002);
    let _ = ctx.drain_notifications_for_char(p3_char_id);

    // =========================================================================
    // Pipeline 1: OnAccountChange
    // =========================================================================
    let notifs1 = ctx.notify_account_change(90_000_001, "cash", 90_000_001, 75_000_000.0);
    assert_eq!(notifs1.len(), 1);
    assert_eq!(notifs1[0].event, "OnAccountChange");
    assert_eq!(notifs1[0].id_type, "charid");
    assert_eq!(notifs1[0].args.len(), 3);
    assert_eq!(notifs1[0].args[0], PyRep::String("cash".into()));
    assert_eq!(notifs1[0].args[1], PyRep::Integer(90_000_001));
    assert_eq!(notifs1[0].args[2], PyRep::Real(75_000_000.0));

    // Verify binary packet marshal round-trip
    let rep1 = marshal::decode(&notifs1[0].marshaled_packet).expect("decode OnAccountChange");
    let pkt1 = MachoPacket::from_pyrep(&rep1).expect("parse OnAccountChange packet");
    assert_eq!(pkt1.packet_type, MachoPacketType::Notification);

    // =========================================================================
    // Pipeline 2: OnItemChange
    // =========================================================================
    let sample_item = PyRep::Tuple(vec![
        PyRep::Integer(100_001),
        PyRep::Integer(34),
        PyRep::Integer(90_000_001),
        PyRep::Integer(60_003_760),
        PyRep::Integer(4),
        PyRep::Integer(500),
    ]);
    let notifs2 = ctx.notify_item_change(
        90_000_001,
        sample_item.clone(),
        vec![(5, PyRep::Integer(600))],
    );
    assert_eq!(notifs2.len(), 1);
    assert_eq!(notifs2[0].event, "OnItemChange");
    assert_eq!(notifs2[0].id_type, "charid");
    assert_eq!(notifs2[0].args.len(), 2);
    assert_eq!(notifs2[0].args[0], sample_item);
    let rep2 = marshal::decode(&notifs2[0].marshaled_packet).expect("decode OnItemChange");
    let pkt2 = MachoPacket::from_pyrep(&rep2).expect("parse OnItemChange packet");
    assert_eq!(pkt2.packet_type, MachoPacketType::Notification);

    // =========================================================================
    // Pipeline 3: OnSkillTrained
    // =========================================================================
    let notifs3 = ctx.notify_skill_trained(90_000_001, 501, 3300, 5);
    assert_eq!(notifs3.len(), 1);
    assert_eq!(notifs3[0].event, "OnSkillTrained");
    assert_eq!(notifs3[0].id_type, "charid");
    assert_eq!(
        notifs3[0].args,
        vec![
            PyRep::Integer(501),
            PyRep::Integer(3300),
            PyRep::Integer(5),
        ]
    );

    // =========================================================================
    // Pipeline 4: OnDamageStateChange (SolarSystem multicast scope)
    // =========================================================================
    let notifs4 = ctx.notify_damage_state_change(
        NotificationTarget::SolarSystem(30_000_142),
        100_001,
        0.80,
        0.95,
        1.0,
    );
    // Pilot 1 and Pilot 2 are in 30000142, Pilot 3 is in 30000144
    assert_eq!(
        notifs4.len(),
        2,
        "Both pilots in system 30000142 must receive OnDamageStateChange"
    );
    let recipients4: BTreeSet<i64> = notifs4.iter().map(|n| n.recipient_char_id).collect();
    assert!(recipients4.contains(&90_000_001));
    assert!(recipients4.contains(&90_000_002));
    assert!(!recipients4.contains(&p3_char_id), "Pilot 3 in other system must NOT receive it");
    assert_eq!(notifs4[0].event, "OnDamageStateChange");
    assert_eq!(notifs4[0].id_type, "itemid");

    // =========================================================================
    // Pipeline 5: OnGodmaShipEffect
    // =========================================================================
    let notifs5 = ctx.notify_godma_ship_effect(
        NotificationTarget::Character(90_000_001),
        100_001,
        16,
        true,
        6000,
        1,
    );
    assert_eq!(notifs5.len(), 1);
    assert_eq!(notifs5[0].event, "OnGodmaShipEffect");
    assert_eq!(notifs5[0].id_type, "charid");
    assert_eq!(notifs5[0].args.len(), 11);
    assert_eq!(notifs5[0].args[0], PyRep::Integer(100_001));
    assert_eq!(notifs5[0].args[1], PyRep::Integer(16));
    assert_eq!(notifs5[0].args[3], PyRep::Integer(1)); // start = 1
    assert_eq!(notifs5[0].args[4], PyRep::Integer(1)); // active = 1
    assert_eq!(notifs5[0].args[7], PyRep::Real(6000.0)); // duration_ms = 6000.0
    assert_eq!(notifs5[0].args[8], PyRep::Integer(1)); // repeat = 1

    // =========================================================================
    // Pipeline 6: OnFleetBroadcast (Fleet multicast scope)
    // =========================================================================
    // Create a fleet with Pilot 1 (Commander) and invite Pilot 2 (Member)
    let fid = ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let id = snap.next_entity_id;
        snap.fleets.insert(
            id,
            eve_db::FleetRecord {
                fleet_id: id,
                boss_id: 90_000_001,
                members: vec![90_000_001, 90_000_002],
                ..Default::default()
            },
        );
        id
    });

    let notifs6 = ctx.notify_fleet_broadcast(fid, "Target", 90_000_001, 7001);
    assert_eq!(notifs6.len(), 2, "Both fleet members must receive OnFleetBroadcast");
    let recipients6: BTreeSet<i64> = notifs6.iter().map(|n| n.recipient_char_id).collect();
    assert!(recipients6.contains(&90_000_001));
    assert!(recipients6.contains(&90_000_002));
    assert!(!recipients6.contains(&p3_char_id), "Non-fleet pilot must NOT receive it");
    assert_eq!(notifs6[0].event, "OnFleetBroadcast");
    assert_eq!(notifs6[0].id_type, "fleetid");
    assert_eq!(
        notifs6[0].args,
        vec![
            PyRep::String("Target".into()),
            PyRep::Integer(90_000_001),
            PyRep::Integer(7001),
            PyRep::Integer(fid),
        ]
    );

    // =========================================================================
    // Pipeline 7: OnClientEvent_ChatMessageSent (Chat broadcast message)
    // =========================================================================
    let notifs7 = ctx.notify_lsc_message(
        NotificationTarget::SolarSystem(30_000_142),
        "solarsystemid2:30000142",
        90_000_001,
        "Local broadcast test",
    );
    assert_eq!(notifs7.len(), 2);
    assert_eq!(notifs7[0].event, "OnClientEvent_ChatMessageSent");
    assert_eq!(notifs7[0].id_type, "channelid");
    assert!(notifs7[0].args.is_empty());

    // =========================================================================
    // Pipeline 8: OnWarpToAsteroidBelt
    // =========================================================================
    let notifs8 = ctx.notify_warp_to_asteroid_belt(90_000_001, 40_000_001);
    assert_eq!(notifs8.len(), 1);
    assert_eq!(notifs8[0].event, "OnWarpToAsteroidBelt");
    assert_eq!(notifs8[0].id_type, "charid");
    assert_eq!(
        notifs8[0].args,
        vec![PyRep::Integer(90_000_001), PyRep::Integer(40_000_001)]
    );

    // =========================================================================
    // Pipeline 9: OnAsteroidTerminated
    // =========================================================================
    let notifs9 = ctx.notify_asteroid_terminated(30_000_142, 55_000_001);
    assert_eq!(notifs9.len(), 2);
    assert_eq!(notifs9[0].event, "OnAsteroidTerminated");
    assert_eq!(notifs9[0].id_type, "solarsystemid");
    assert_eq!(notifs9[0].args, vec![PyRep::Integer(55_000_001)]);

    // =========================================================================
    // Pipeline 10: OnMiningLedgerGraphSettingChanged
    // =========================================================================
    let notifs10 = ctx.notify_mining_ledger_graph_setting_changed(
        90_000_001,
        "groupBy",
        PyRep::String("oreType".into()),
    );
    assert_eq!(notifs10.len(), 1);
    assert_eq!(notifs10[0].event, "OnMiningLedgerGraphSettingChanged");
    assert_eq!(notifs10[0].id_type, "charid");
    assert_eq!(
        notifs10[0].args,
        vec![
            PyRep::String("groupBy".into()),
            PyRep::String("oreType".into()),
        ]
    );

    // =========================================================================
    // Target Scope Multicast Checks: Corporation, Alliance, Station, All
    // =========================================================================
    // Corporation scope (Pilot 1 and Pilot 2 are in corp 98000001)
    let corp_notifs = ctx.notify(
        NotificationTarget::Corporation(98_000_001),
        "OnCorporationChanged",
        "corpid",
        vec![PyRep::String("Corp announcement".into())],
    );
    assert_eq!(corp_notifs.len(), 2);
    let corp_recips: BTreeSet<i64> = corp_notifs.iter().map(|n| n.recipient_char_id).collect();
    assert!(corp_recips.contains(&90_000_001));
    assert!(corp_recips.contains(&90_000_002));
    assert!(!corp_recips.contains(&p3_char_id));

    // Station scope (Pilot 1 and Pilot 2 are in station 60003760)
    let station_notifs = ctx.notify(
        NotificationTarget::Station(60_003_760),
        "OnCharNowInStation",
        "stationid",
        vec![PyRep::String("Station notice".into())],
    );
    assert_eq!(station_notifs.len(), 2);

    // All scope (all 3 online pilots)
    let all_notifs = ctx.notify(
        NotificationTarget::All,
        "OnServerMessage",
        "all",
        vec![PyRep::String("Maintenance in 15 minutes".into())],
    );
    assert_eq!(all_notifs.len(), 3);

    // =========================================================================
    // Queue Overflow Protection & Drain Verification
    // =========================================================================
    // Exceed MAX_PENDING_NOTIFICATIONS_PER_CHAR (128)
    for i in 0..150 {
        ctx.notify_account_change(90_000_001, "cash", 90_000_001, i as f64);
    }
    let p1_queue = ctx.drain_notifications_for_char(90_000_001);
    assert_eq!(
        p1_queue.len(),
        MAX_PENDING_NOTIFICATIONS_PER_CHAR,
        "Pending notification queue must be capped at MAX_PENDING_NOTIFICATIONS_PER_CHAR (128)"
    );

    // Second drain must be empty
    let empty_queue = ctx.drain_notifications_for_char(90_000_001);
    assert!(empty_queue.is_empty(), "Queue must be empty after drain");
}

}


#[allow(unused_imports, dead_code, unused_variables, unused_mut)]
mod rpc_services_remaining_ext_a {
    use super::*;

// Integration tests for 13 rock-solid `rpc:*` parity rows across `beyonce` and `contractProxy`.

use crate::common;
use eve_db::{
    BookmarkFolderRecord, ContractRecord, CorporationRecord, CosmicSignatureRecord, InventoryFlag,
    InventoryItemRecord, IskCents, UpwellStructureRecord, WorldRepository,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;
use std::sync::Arc;

const P1: i64 = 90_000_001;
const P2: i64 = 90_000_002;
const JITA_44: i64 = 60_003_760;
const JITA_SYS: u32 = 30_000_142;

async fn login_select(ctx: &ServerContext, sid: u64, user: &str, pass: &str, cid: i64) -> SessionState {
    let mut s = SessionState::new(sid);
    let empty = HashMap::new();
    ctx.dispatch_rpc(&mut s, "authentication", None, "Login", &[PyRep::String(user.into()), PyRep::String(pass.into())], &empty, None).await.expect("Login");
    ctx.dispatch_rpc(&mut s, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(cid)], &empty, None).await.expect("SelectCharacterID");
    s
}

async fn call(ctx: &ServerContext, s: &mut SessionState, svc: &str, method: &str, args: &[PyRep]) -> PyRep {
    ctx.dispatch_rpc(s, svc, None, method, args, &HashMap::new(), None).await.unwrap_or_else(|e| panic!("{svc}.{method} failed: {e:?}"))
}

async fn call_err(ctx: &ServerContext, s: &mut SessionState, svc: &str, method: &str, args: &[PyRep]) -> String {
    ctx.dispatch_rpc(s, svc, None, method, args, &HashMap::new(), None).await.expect_err(&format!("{svc}.{method} should have failed")).code
}

fn mk_item(item_id: i64, type_id: u32, owner_id: i64, location_id: i64, flag: InventoryFlag, quantity: i32, singleton: bool) -> InventoryItemRecord {
    InventoryItemRecord { item_id, type_id, owner_id, location_id, flag, quantity, singleton, version: 1, ..Default::default() }
}

fn mk_structure(structure_id: i64, type_id: u32, owner_corp_id: i64) -> UpwellStructureRecord {
    UpwellStructureRecord {
        structure_id, type_id, owner_corp_id, solar_system_id: JITA_SYS, name: "Test Structure".into(),
        state: "ShieldVulnerable".into(), position: [100_000.0, 0.0, 0.0], fuel_blocks: 100,
        quantum_core_installed: true, shield_hp: 1.0, armor_hp: 1.0, hull_hp: 1.0,
        reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
    }
}

#[tokio::test]
async fn test_rpc_sweep_group1a_bookmarks() {
    // parity: rpc:beyonce.BookmarkLocation, rpc:beyonce.BookmarkScanResult
    eve_testkit::attest(&["rpc:beyonce.BookmarkLocation", "rpc:beyonce.BookmarkScanResult"]);
    let ctx = ServerContext::with_repository(Arc::new(WorldRepository::with_demo_seed()));
    common::pin_clock(&ctx);
    let mut s1 = login_select(&ctx, 8101, "demo", "demo-secret", P1).await;

    let p1_folder_id = 501i64;
    let foreign_folder_id = 999i64;
    let bmk_item_id = 1_910_000_001i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.bookmark_folders.insert(
            p1_folder_id,
            BookmarkFolderRecord {
                folder_id: p1_folder_id,
                owner_id: P1,
                folder_name: "Fleet Pins".into(),
                is_shared: false,
                corporation_id: None,
                ..Default::default()
            },
        );
        snap.items.insert(
            bmk_item_id,
            mk_item(bmk_item_id, 34, P1, JITA_44, InventoryFlag::Hangar, 100, false),
        );
        snap.cosmic_signatures.entry(JITA_SYS).or_default().push(CosmicSignatureRecord {
            signature_id: "BMK-101".into(),
            solar_system_id: JITA_SYS,
            sig_group: "Data".into(),
            type_name: "Central Data Core".into(),
            position: [12_000.0, 24_000.0, 36_000.0],
            signal_strength: 1.0,
            wormhole_type_id: Some(9988),
            destination_system_id: None,
            created_unix: 1000,
            lifetime_seconds: 0,
            max_mass_kg: 0.0,
            remaining_mass_kg: 0.0,
            max_jump_mass_kg: 0.0,
            collapsed: false,
        });
    });

    // Exploit regression tests for BookmarkLocation:
    // 1. Specifying an inaccessible / unowned folder must fail with FolderNotFound
    assert_eq!(
        call_err(
            &ctx,
            &mut s1,
            "beyonce",
            "BookmarkLocation",
            &[
                PyRep::Integer(bmk_item_id),
                PyRep::Integer(foreign_folder_id),
                PyRep::String("Pin".into()),
                PyRep::String("Note".into()),
                PyRep::Integer(1),
                PyRep::Integer(JITA_SYS as i64)
            ]
        )
        .await,
        "FolderNotFound"
    );

    // 2. Legitimate BookmarkLocation with owned folder derives typeID (34) and locationID (JITA_44) from item:
    let bm_loc = call(
        &ctx,
        &mut s1,
        "beyonce",
        "BookmarkLocation",
        &[
            PyRep::Integer(bmk_item_id),
            PyRep::Integer(p1_folder_id),
            PyRep::String("Jita Trit".into()),
            PyRep::String("Pin".into()),
            PyRep::Integer(1),
            PyRep::Integer(JITA_SYS as i64),
        ],
    )
    .await;
    let PyRep::Tuple(bm_tuple) = bm_loc else { panic!("expected 8-tuple from beyonce.BookmarkLocation") };
    assert_eq!(bm_tuple.len(), 8);
    let bm_id = bm_tuple[0].as_i64().unwrap();
    assert!(bm_id > 0);
    assert_eq!(bm_tuple[1], PyRep::Integer(bmk_item_id));
    assert_eq!(bm_tuple[2], PyRep::Integer(34)); // derived from item type
    assert_eq!(bm_tuple[6], PyRep::Integer(JITA_44)); // derived from item location
    assert_eq!(bm_tuple[7], PyRep::Integer(p1_folder_id));

    // Exploit regression tests for BookmarkScanResult:
    // 1. Caller docked in station must fail with NotInSpace
    assert_eq!(
        call_err(
            &ctx,
            &mut s1,
            "beyonce",
            "BookmarkScanResult",
            &[
                PyRep::Integer(JITA_SYS as i64),
                PyRep::String("Scanned Core".into()),
                PyRep::String("Note".into()),
                PyRep::String("BMK-101".into()),
                PyRep::Integer(p1_folder_id),
            ]
        )
        .await,
        "NotInSpace"
    );

    // Undock into space
    call(&ctx, &mut s1, "stationSvc", "Undock", &[]).await;

    // 2. Specifying a different solar system must fail with NotInSameSystem
    assert_eq!(
        call_err(
            &ctx,
            &mut s1,
            "beyonce",
            "BookmarkScanResult",
            &[
                PyRep::Integer(30_000_144),
                PyRep::String("Scanned Core".into()),
                PyRep::String("Note".into()),
                PyRep::String("BMK-101".into()),
                PyRep::Integer(p1_folder_id),
            ]
        )
        .await,
        "NotInSameSystem"
    );

    // 3. Specifying an invalid / foreign folder must fail with FolderNotFound
    assert_eq!(
        call_err(
            &ctx,
            &mut s1,
            "beyonce",
            "BookmarkScanResult",
            &[
                PyRep::Integer(JITA_SYS as i64),
                PyRep::String("Scanned Core".into()),
                PyRep::String("Note".into()),
                PyRep::String("BMK-101".into()),
                PyRep::Integer(foreign_folder_id),
            ]
        )
        .await,
        "FolderNotFound"
    );

    // 4. Legitimate BookmarkScanResult in space derives signature typeID (9988) on server:
    let bm_scan = call(
        &ctx,
        &mut s1,
        "beyonce",
        "BookmarkScanResult",
        &[
            PyRep::Integer(JITA_SYS as i64),
            PyRep::String("Scanned Data".into()),
            PyRep::String("Note".into()),
            PyRep::String("BMK-101".into()),
            PyRep::Integer(p1_folder_id),
        ],
    )
    .await;
    let PyRep::Tuple(scan_tuple) = bm_scan else { panic!("expected 8-tuple from beyonce.BookmarkScanResult") };
    assert_eq!(scan_tuple.len(), 8);
    assert_eq!(scan_tuple[1], PyRep::Integer(JITA_SYS as i64));
    assert_eq!(scan_tuple[2], PyRep::Integer(9988)); // server-derived signature typeID
    assert_eq!(scan_tuple[3], PyRep::Real(12_000.0));
    assert_eq!(scan_tuple[6], PyRep::Integer(JITA_SYS as i64));
    assert_eq!(scan_tuple[7], PyRep::Integer(p1_folder_id));
}

#[tokio::test]
async fn test_rpc_sweep_group1b_stack() {
    // parity: rpc:contractProxy.SplitStack
    eve_testkit::attest(&["rpc:contractProxy.SplitStack"]);
    let ctx = ServerContext::with_repository(Arc::new(WorldRepository::with_demo_seed()));
    common::pin_clock(&ctx);
    let mut s1 = login_select(&ctx, 8111, "demo", "demo-secret", P1).await;
    let mut s2 = login_select(&ctx, 8112, "other", "other-secret", P2).await;

    let stack_id = 1_910_000_010i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            stack_id,
            InventoryItemRecord {
                item_id: stack_id,
                type_id: 34,
                owner_id: P1,
                location_id: JITA_44,
                flag: InventoryFlag::Hangar,
                quantity: 100,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    // Exploit regression tests for SplitStack:
    // 1. Splitting with quantity <= 0 must fail
    assert_eq!(call_err(&ctx, &mut s1, "contractProxy", "SplitStack", &[PyRep::Integer(JITA_44), PyRep::Integer(stack_id), PyRep::Integer(0), PyRep::Bool(false), PyRep::Integer(4)]).await, "SplitFailed");
    // 2. Splitting with quantity >= stack size must fail
    assert_eq!(call_err(&ctx, &mut s1, "contractProxy", "SplitStack", &[PyRep::Integer(JITA_44), PyRep::Integer(stack_id), PyRep::Integer(100), PyRep::Bool(false), PyRep::Integer(4)]).await, "SplitFailed");
    assert_eq!(call_err(&ctx, &mut s1, "contractProxy", "SplitStack", &[PyRep::Integer(JITA_44), PyRep::Integer(stack_id), PyRep::Integer(150), PyRep::Bool(false), PyRep::Integer(4)]).await, "SplitFailed");
    // 3. Unauthorized pilot (P2) splitting P1's item must fail
    assert_eq!(call_err(&ctx, &mut s2, "contractProxy", "SplitStack", &[PyRep::Integer(JITA_44), PyRep::Integer(stack_id), PyRep::Integer(40), PyRep::Bool(false), PyRep::Integer(4)]).await, "SplitFailed");

    // Legitimate split succeeds
    let split = call(
        &ctx,
        &mut s1,
        "contractProxy",
        "SplitStack",
        &[PyRep::Integer(JITA_44), PyRep::Integer(stack_id), PyRep::Integer(40), PyRep::Bool(false), PyRep::Integer(4)],
    )
    .await;
    assert_eq!(split.dict_get("originalQuantity"), Some(&PyRep::Integer(60)));
}

#[tokio::test]
async fn test_rpc_sweep_group2_beyonce_commands() {
    // parity: rpc:beyonce.CmdAbandonLoot, rpc:beyonce.CmdTransferCorpAssetOwnership, rpc:beyonce.CmdTurboDock
    eve_testkit::attest(&[
        "rpc:beyonce.CmdAbandonLoot",
        "rpc:beyonce.CmdTransferCorpAssetOwnership",
        "rpc:beyonce.CmdTurboDock",
    ]);

    let ctx = ServerContext::with_repository(Arc::new(WorldRepository::with_demo_seed()));
    common::pin_clock(&ctx);
    let mut s1 = login_select(&ctx, 8102, "demo", "demo-secret", P1).await;
    let mut s2 = login_select(&ctx, 8122, "other", "other-secret", P2).await;

    let loot_id = 1_920_000_001i64;
    let loot_hangar_id = 1_920_000_005i64;
    let st_id = 1_920_000_002i64;
    let corp_id = s1.corporation_id.unwrap_or(1_000_170);
    let target_player_corp = 98_000_001i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(loot_id, mk_item(loot_id, 23, P1, JITA_SYS as i64, InventoryFlag::None, 1, true));
        snap.items.insert(loot_hangar_id, mk_item(loot_hangar_id, 23, P1, JITA_44, InventoryFlag::Hangar, 1, true));
        snap.corporations.insert(
            target_player_corp,
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = target_player_corp;
                __corp.name = "Allied Fleet Corp".into();
                __corp.ticker = "AFC".into();
                __corp.ceo_id = P2;
                __corp.tax_rate = 0.05;
                __corp.hq_station_id = Some(JITA_44);
                __corp },
        );
        snap.structures.insert(st_id, mk_structure(st_id, 35832, corp_id));
    });

    // Exploit regression tests for CmdAbandonLoot:
    // 1. Item in hangar (not in space) must fail
    assert_eq!(call_err(&ctx, &mut s1, "beyonce", "CmdAbandonLoot", &[PyRep::Integer(loot_hangar_id)]).await, "ItemNotInSpace");
    // 2. Item not owned by caller must fail
    assert_eq!(call_err(&ctx, &mut s2, "beyonce", "CmdAbandonLoot", &[PyRep::Integer(loot_id)]).await, "AccessDenied");
    // 3. Legitimate abandonment succeeds and clears owner
    assert_eq!(call(&ctx, &mut s1, "beyonce", "CmdAbandonLoot", &[PyRep::Integer(loot_id)]).await.dict_get("abandoned"), Some(&PyRep::Bool(true)));
    ctx.db.with_snapshot(|snap| assert_eq!(snap.items[&loot_id].owner_id, 0));

    // Exploit regression tests for CmdTransferCorpAssetOwnership:
    // 1. Caller docked in station must fail with NotInSpace (P2 #97)
    s1.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
    assert_eq!(call_err(&ctx, &mut s1, "beyonce", "CmdTransferCorpAssetOwnership", &[PyRep::Integer(st_id), PyRep::Integer(target_player_corp)]).await, "NotInSpace");

    // Undock into space
    call(&ctx, &mut s1, "stationSvc", "Undock", &[]).await;

    // 2. Unprivileged member (non-director) must fail with CrpAccessDenied
    s1.corp_role = 0;
    assert_eq!(call_err(&ctx, &mut s1, "beyonce", "CmdTransferCorpAssetOwnership", &[PyRep::Integer(st_id), PyRep::Integer(target_player_corp)]).await, "CrpAccessDenied");
    // 3. Transfer to NPC corporation (e.g. 1_000_045) must fail with InvalidTargetCorporation
    s1.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
    assert_eq!(call_err(&ctx, &mut s1, "beyonce", "CmdTransferCorpAssetOwnership", &[PyRep::Integer(st_id), PyRep::Integer(1_000_045)]).await, "InvalidTargetCorporation");
    // 4. Legitimate transfer by Director in space to player corp succeeds
    assert_eq!(
        call(&ctx, &mut s1, "beyonce", "CmdTransferCorpAssetOwnership", &[PyRep::Integer(st_id), PyRep::Integer(target_player_corp)]).await.dict_get("ownerID"),
        Some(&PyRep::Integer(target_player_corp))
    );

    // Exploit regression tests for CmdTurboDock:
    // 1. Non-GM pilot must fail
    assert_eq!(call_err(&ctx, &mut s1, "beyonce", "CmdTurboDock", &[PyRep::Integer(JITA_44)]).await, "GmRoleRequired");
    // 2. GM pilot succeeds
    s1.role = eve_db::service_roles::ROLE_GM_DEFAULT;
    assert_eq!(call(&ctx, &mut s1, "beyonce", "CmdTurboDock", &[PyRep::Integer(JITA_44)]).await.dict_get("docked"), Some(&PyRep::Bool(true)));
    assert_eq!(s1.station_id, Some(JITA_44));
    assert_eq!(call(&ctx, &mut s1, "beyonce", "CmdTurboDock", &[PyRep::Integer(st_id)]).await.dict_get("docked"), Some(&PyRep::Bool(true)));
    assert_eq!(s1.structure_id, Some(st_id));
    assert_eq!(s1.station_id, None);
}

#[tokio::test]
async fn test_rpc_sweep_group3_contract_proxy_methods() {
    // parity: rpc:contractProxy.CollectMyPageInfo, rpc:contractProxy.FinishAuction, rpc:contractProxy.GetCourierContractFromItemID
    // parity: rpc:contractProxy.GetItemsInContainer, rpc:contractProxy.GetMyContractEscrow, rpc:contractProxy.GetNumItemsInContainers, rpc:contractProxy.NumOutstandingContracts
    eve_testkit::attest(&[
        "rpc:contractProxy.CollectMyPageInfo",
        "rpc:contractProxy.FinishAuction",
        "rpc:contractProxy.GetCourierContractFromItemID",
        "rpc:contractProxy.GetItemsInContainer",
        "rpc:contractProxy.GetMyContractEscrow",
        "rpc:contractProxy.GetNumItemsInContainers",
        "rpc:contractProxy.NumOutstandingContracts",
    ]);

    let repo = Arc::new(WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_repository(repo);
    common::pin_clock(&ctx);
    let mut s1 = login_select(&ctx, 8103, "demo", "demo-secret", P1).await;

    let cid_del = 1_930_000_001i64;
    let cid_auc = 1_930_000_002i64;
    let cid_unexpired = 1_930_000_006i64;
    let cid_unauth = 1_930_000_007i64;
    let item_auc = 1_930_000_003i64;
    let box_id = 1_930_000_004i64;
    let box_child = 1_930_000_005i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            item_auc,
            InventoryItemRecord {
                item_id: item_auc,
                type_id: 34,
                owner_id: P1,
                location_id: JITA_44,
                flag: InventoryFlag::Locked,
                quantity: 500,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            box_id,
            InventoryItemRecord {
                item_id: box_id,
                type_id: 3293,
                owner_id: P1,
                location_id: JITA_44,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            box_child,
            InventoryItemRecord {
                item_id: box_child,
                type_id: 34,
                owner_id: P1,
                location_id: box_id,
                flag: InventoryFlag::Cargo,
                quantity: 25,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.contracts.insert(
            cid_del,
            ContractRecord {
                contract_id: cid_del,
                issuer_id: P1,
                assignee_id: 0,
                acceptor_id: None,
                contract_type: "Courier".into(),
                availability: "Public".into(),
                status: "Outstanding".into(),
                start_station_id: JITA_44,
                end_station_id: JITA_44,
                price: IskCents(0),
                reward: IskCents::from_f64_isk(50_000.0),
                collateral: IskCents(0),
                item_ids: vec![box_id],
                requested_items: vec![],
                current_bid: IskCents(0),
                current_bidder_id: None,
                title: "Courier".into(),
                date_issued_unix: 1000,
                expiration_unix: 2_000_000_000,
                ..Default::default()
            },
        );
        snap.contracts.insert(
            cid_auc,
            ContractRecord {
                contract_id: cid_auc,
                issuer_id: P1,
                assignee_id: 0,
                acceptor_id: None,
                contract_type: "Auction".into(),
                availability: "Public".into(),
                status: "Outstanding".into(),
                start_station_id: JITA_44,
                end_station_id: JITA_44,
                price: IskCents::from_f64_isk(100_000.0),
                reward: IskCents(0),
                collateral: IskCents(0),
                item_ids: vec![item_auc],
                requested_items: vec![],
                current_bid: IskCents::from_f64_isk(150_000.0),
                current_bidder_id: Some(P2),
                title: "Auction".into(),
                date_issued_unix: 100,
                expiration_unix: 500,
                ..Default::default()
            },
        );
        snap.contracts.insert(
            cid_unexpired,
            ContractRecord {
                contract_id: cid_unexpired,
                issuer_id: P1,
                assignee_id: 0,
                acceptor_id: None,
                contract_type: "Auction".into(),
                availability: "Public".into(),
                status: "Outstanding".into(),
                start_station_id: JITA_44,
                end_station_id: JITA_44,
                price: IskCents::from_f64_isk(100_000.0),
                reward: IskCents(0),
                collateral: IskCents(0),
                item_ids: vec![],
                requested_items: vec![],
                current_bid: IskCents(0),
                current_bidder_id: None,
                title: "Unexpired Auction".into(),
                date_issued_unix: 100,
                expiration_unix: 2_000_000_000,
                ..Default::default()
            },
        );
        snap.contracts.insert(
            cid_unauth,
            ContractRecord {
                contract_id: cid_unauth,
                issuer_id: 999_999,
                assignee_id: 0,
                acceptor_id: None,
                contract_type: "Auction".into(),
                availability: "Public".into(),
                status: "Outstanding".into(),
                start_station_id: JITA_44,
                end_station_id: JITA_44,
                price: IskCents::from_f64_isk(100_000.0),
                reward: IskCents(0),
                collateral: IskCents(0),
                item_ids: vec![],
                requested_items: vec![],
                current_bid: IskCents(0),
                current_bidder_id: None,
                title: "Other Auction".into(),
                date_issued_unix: 100,
                expiration_unix: 500,
                ..Default::default()
            },
        );
    });

    let page = call(&ctx, &mut s1, "contractProxy", "CollectMyPageInfo", &[]).await;
    assert_eq!(page.dict_get("numOutstandingContractsNonCorp"), Some(&PyRep::Integer(3)));
    assert_eq!(call(&ctx, &mut s1, "contractProxy", "NumOutstandingContracts", &[]).await, PyRep::Integer(3));
    let escrow = call(&ctx, &mut s1, "contractProxy", "GetMyContractEscrow", &[]).await;
    assert_eq!(escrow.dict_get("iskEscrow"), Some(&PyRep::Real(50_000.0)));
    assert_eq!(escrow.dict_get("itemsEscrow"), Some(&PyRep::Integer(2)));

    assert_eq!(call(&ctx, &mut s1, "contractProxy", "GetCourierContractFromItemID", &[PyRep::Integer(box_id)]).await.dict_get("contractID"), Some(&PyRep::Integer(cid_del)));
    assert_eq!(call(&ctx, &mut s1, "contractProxy", "GetCourierContractFromItemID", &[PyRep::Integer(item_auc)]).await, PyRep::None);

    let in_box = call(&ctx, &mut s1, "contractProxy", "GetItemsInContainer", &[PyRep::Integer(JITA_44), PyRep::Integer(box_id), PyRep::Bool(false), PyRep::Integer(4)]).await;
    assert!(matches!(in_box, PyRep::List(ref l) if l.len() == 1));
    let counts = call(&ctx, &mut s1, "contractProxy", "GetNumItemsInContainers", &[PyRep::Integer(JITA_44), PyRep::List(vec![PyRep::Integer(box_id)]), PyRep::Bool(false), PyRep::Integer(4)]).await;
    assert!(matches!(counts, PyRep::Dict(ref d) if d.contains(&(PyRep::Integer(box_id), PyRep::Integer(1)))));

    // Exploit regression tests for FinishAuction:
    // 1. Finishing unexpired auction must fail with AuctionNotExpired
    assert_eq!(call_err(&ctx, &mut s1, "contractProxy", "FinishAuction", &[PyRep::Integer(cid_unexpired), PyRep::Bool(true)]).await, "AuctionNotExpired");
    // 2. Finishing non-auction contract (e.g. Courier) must fail with InvalidContractType
    assert_eq!(call_err(&ctx, &mut s1, "contractProxy", "FinishAuction", &[PyRep::Integer(cid_del), PyRep::Bool(true)]).await, "InvalidContractType");
    // 3. Unauthorized caller (P1 who is neither issuer nor bidder on cid_unauth) must fail with ContractOwnershipViolation
    assert_eq!(call_err(&ctx, &mut s1, "contractProxy", "FinishAuction", &[PyRep::Integer(cid_unauth), PyRep::Bool(true)]).await, "ContractOwnershipViolation");
    // 4. Legitimate finish by issuer or winner after expiration succeeds
    assert_eq!(call(&ctx, &mut s1, "contractProxy", "FinishAuction", &[PyRep::Integer(cid_auc), PyRep::Bool(true)]).await, PyRep::Bool(true));
    // 5. Finishing already finished auction fails with InvalidContractState
    assert_eq!(call_err(&ctx, &mut s1, "contractProxy", "FinishAuction", &[PyRep::Integer(cid_auc), PyRep::Bool(true)]).await, "InvalidContractState");
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.contracts[&cid_auc].status, "Finished");
        assert_eq!(snap.items[&item_auc].owner_id, P2);
    });
}

}


#[allow(unused_imports, dead_code, unused_variables, unused_mut)]
mod rpc_services_remaining_ext_b {
    use super::*;

// Integration tests for verified solid RPC parity rows:
// `charMgr.SaveCharacterSetting` and `charMgr.DeleteCharacterSetting`.


use crate::common;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use std::collections::HashMap;

async fn setup_pilot_creds(
    ctx: &ServerContext,
    sid: u64,
    user: &str,
    pass: &str,
    char_id: i64,
) -> SessionState {
    let kw = HashMap::new();
    let mut sess = SessionState::new(sid);
    common::login_pilot(ctx, &mut sess, user, pass).await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");
    sess
}

async fn setup_pilot(ctx: &ServerContext, sid: u64, char_id: i64) -> SessionState {
    setup_pilot_creds(ctx, sid, "demo", "demo-secret", char_id).await
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None)
        .await
}

#[tokio::test]
// parity: feat:ui.server-settings-storage
// parity: rpc:charMgr.DeleteCharacterSetting
// parity: rpc:charMgr.SaveCharacterSetting
async fn test_rpc_solid_charmgr_settings_persistence() {
    eve_testkit::attest(&[
        "feat:ui.server-settings-storage",
        "rpc:charMgr.DeleteCharacterSetting",
        "rpc:charMgr.SaveCharacterSetting",
    ]);

    let ctx = ServerContext::new();
    let char_id = 90_000_001i64;
    let mut sess = setup_pilot(&ctx, 41_101, char_id).await;

    // 1. Initial settings empty
    let init_settings = rpc(&ctx, &mut sess, "charMgr", "GetCharacterSettings", &[])
        .await
        .expect("GetCharacterSettings initial");
    assert_eq!(init_settings, PyRep::Dict(vec![]));

    // 2. Save settings
    let saved1 = rpc(
        &ctx,
        &mut sess,
        "charMgr",
        "SaveCharacterSetting",
        &[PyRep::String("overview_tab".into()), PyRep::Integer(3)],
    )
    .await
    .expect("SaveCharacterSetting 1");
    assert_eq!(saved1, PyRep::Bool(true));

    let saved2 = rpc(
        &ctx,
        &mut sess,
        "charMgr",
        "SaveCharacterSetting",
        &[PyRep::String("audio_mute".into()), PyRep::Bool(false)],
    )
    .await
    .expect("SaveCharacterSetting 2");
    assert_eq!(saved2, PyRep::Bool(true));

    // 3. Verify retrieved settings contain both
    let cur_settings = rpc(&ctx, &mut sess, "charMgr", "GetCharacterSettings", &[])
        .await
        .expect("GetCharacterSettings after save");
    assert_eq!(cur_settings.dict_get("overview_tab"), Some(&PyRep::Integer(3)));
    assert_eq!(cur_settings.dict_get("audio_mute"), Some(&PyRep::Bool(false)));

    // 4. Persistence round-trip across new context state load
    let loaded = eve_server::services::cluster_macho_misc_services::ClusterMiscRuntimeState::from_db(&ctx.db);
    assert_eq!(
        loaded.char_settings.get(&char_id).and_then(|m| m.get("overview_tab")),
        Some(&PyRep::Integer(3))
    );

    // 5. Delete one setting
    let deleted = rpc(
        &ctx,
        &mut sess,
        "charMgr",
        "DeleteCharacterSetting",
        &[PyRep::String("overview_tab".into())],
    )
    .await
    .expect("DeleteCharacterSetting");
    assert_eq!(deleted, PyRep::Bool(true));

    // 6. Verify setting deleted and other setting preserved
    let updated_settings = rpc(&ctx, &mut sess, "charMgr", "GetCharacterSettings", &[])
        .await
        .expect("GetCharacterSettings after delete");
    assert_eq!(updated_settings.dict_get("overview_tab"), None);
    assert_eq!(updated_settings.dict_get("audio_mute"), Some(&PyRep::Bool(false)));

    // 7. Error validation paths
    let err_empty_save = rpc(
        &ctx,
        &mut sess,
        "charMgr",
        "SaveCharacterSetting",
        &[PyRep::String("".into()), PyRep::Integer(1)],
    )
    .await
    .expect_err("Empty key save should fail");
    assert_eq!(err_empty_save.code, "CustomError");

    let err_empty_del = rpc(
        &ctx,
        &mut sess,
        "charMgr",
        "DeleteCharacterSetting",
        &[PyRep::String("".into())],
    )
    .await
    .expect_err("Empty key delete should fail");
    assert_eq!(err_empty_del.code, "CustomError");

    let long_key = "k".repeat(200);
    let err_long_key = rpc(
        &ctx,
        &mut sess,
        "charMgr",
        "SaveCharacterSetting",
        &[PyRep::String(long_key), PyRep::Integer(1)],
    )
    .await
    .expect_err("Overly long key should fail");
    assert_eq!(err_long_key.code, "CustomError");

    let huge_val = "v".repeat(5_000);
    let err_huge_val = rpc(
        &ctx,
        &mut sess,
        "charMgr",
        "SaveCharacterSetting",
        &[PyRep::String("valid_key".into()), PyRep::String(huge_val)],
    )
    .await
    .expect_err("Overly large value should fail");
    assert_eq!(err_huge_val.code, "CustomError");

    // 8. Unauthenticated call rejects cleanly
    let mut unauth_sess = SessionState::new(41_102);
    let err_unauth = rpc(
        &ctx,
        &mut unauth_sess,
        "charMgr",
        "SaveCharacterSetting",
        &[PyRep::String("k".into()), PyRep::Integer(1)],
    )
    .await
    .expect_err("Unauthenticated session should fail");
    assert_eq!(err_unauth.code, "Unauthorized");

    // 9. Verify settings restored upon new session login
    let mut sess2 = setup_pilot(&ctx, 41_103, char_id).await;
    let login_settings = rpc(&ctx, &mut sess2, "charMgr", "GetCharacterSettings", &[])
        .await
        .expect("GetCharacterSettings restored on login");
    assert_eq!(login_settings.dict_get("audio_mute"), Some(&PyRep::Bool(false)));
    assert_eq!(login_settings.dict_get("overview_tab"), None);
}

#[tokio::test]
async fn test_collect_my_page_info_dynamic_contracts_and_no_jita_dummy() {
    let ctx = ServerContext::new();
    let mut s1 = setup_pilot(&ctx, 41_201, 90_000_001).await;
    let mut s2 = setup_pilot_creds(&ctx, 41_202, "other", "other-secret", 90_000_002).await;
    s1.role |= eve_machonet::service_roles::ROLE_GMH;
    let st_id = s1.station_id.unwrap_or(60_003_760);
    let p1_corp = s1.corporation_id.unwrap_or(1_000_009);

    // 1. Clean P2 session has zero contracts and empty outstandingContracts (no 60003760 dummy)
    let p2_empty = rpc(&ctx, &mut s2, "contractProxy", "CollectMyPageInfo", &[])
        .await
        .expect("CollectMyPageInfo empty");
    assert_eq!(
        p2_empty.dict_get("numOutstandingContractsNonCorp"),
        Some(&PyRep::Integer(0))
    );
    assert_eq!(
        p2_empty.dict_get("outstandingContracts"),
        Some(&PyRep::List(Vec::new()))
    );

    // 2. P1 creates an item via slash and assigns a personal ItemExchange contract to P2
    let item_id = rpc(&ctx, &mut s1, "slash", "SlashCmd", &[PyRep::String("/createitem 34 10".into())])
        .await
        .expect("/createitem")
        .dict_get("itemID")
        .and_then(|v| v.as_i64())
        .expect("itemID");
    let created = rpc(
        &ctx,
        &mut s1,
        "contractProxy",
        "CreateContract",
        &[PyRep::keyval([
            ("contractType", PyRep::Integer(1)),
            ("availability", PyRep::Integer(1)),
            ("assigneeID", PyRep::Integer(90_000_002)),
            ("expireTime", PyRep::Integer(1440)),
            ("numDays", PyRep::Integer(1)),
            ("startStationID", PyRep::Integer(st_id)),
            ("endStationID", PyRep::Integer(st_id)),
            ("price", PyRep::Real(0.0)),
            ("reward", PyRep::Real(0.0)),
            ("collateral", PyRep::Real(0.0)),
            ("title", PyRep::String("PageInfoTest".into())),
            ("itemList", PyRep::List(vec![PyRep::Integer(item_id)])),
        ])],
    )
    .await
    .expect("CreateContract");
    assert!(created.as_i64().or_else(|| created.dict_get("contractID").and_then(|v| v.as_i64())).is_some());

    // 3. P1 sees numOutstandingContractsNonCorp == 1; P2 sees exact assigned 4-tuple and requires-attention == 1
    let p1_info = rpc(&ctx, &mut s1, "contractProxy", "CollectMyPageInfo", &[])
        .await
        .expect("CollectMyPageInfo P1");
    assert_eq!(
        p1_info.dict_get("numOutstandingContractsNonCorp"),
        Some(&PyRep::Integer(1))
    );
    let p2_info = rpc(&ctx, &mut s2, "contractProxy", "CollectMyPageInfo", &[])
        .await
        .expect("CollectMyPageInfo P2");
    assert_eq!(
        p2_info.dict_get("numRequiresAttention"),
        Some(&PyRep::Integer(1))
    );
    let Some(PyRep::List(outstanding)) = p2_info.dict_get("outstandingContracts") else {
        panic!("expected outstandingContracts List");
    };
    let expected = PyRep::Tuple(vec![
        PyRep::Integer(90_000_001),
        PyRep::Integer(p1_corp),
        PyRep::Integer(90_000_002),
        PyRep::Integer(1),
    ]);
    assert!(
        outstanding.contains(&expected),
        "expected {expected:?} in {outstanding:?}"
    );
}

}
