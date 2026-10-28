//! State-backed integration tests for 15 newly implemented V24 RPCs across
//! `contractProxy`, `repairSvc`, `structure`, `search`, `i2`, `jumpCloneSvc`,
//! `warpVectorMgr`, and `devIndexManager`.

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
        snap.corporations.insert(corp_id, CorporationRecord {
            corporation_id: corp_id,
            name: "Apex Vanguard Logistics".into(),
            ticker: "APXVL".into(),
            ceo_id: char_id,
            alliance_id: Some(99_000_777),
            tax_rate: 0.05,
            master_wallet: IskCents::from_f64_isk(50_000_000.0),
            members: [char_id].into_iter().collect(),
            member_roles: roles,
            ..Default::default()
        });
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
    // parity: feat:navigation.warp-vector
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
    assert!(matches!(missing_vec, Err(e) if e.code == "VectorItemNotFound"));

    let vec_res = call_rpc(&ctx, &mut sess, "warpVectorMgr", "UseVector", &[PyRep::Integer(vector_id)]).await.expect("warpVectorMgr.UseVector");
    assert_eq!(dict_get(&vec_res, "shipID"), Some(&PyRep::Integer(ship_id)));
    let (_, after_ball) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!(after_ball.position.x > 50_000.0, "ship ball must warp along vector (+X)");

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
