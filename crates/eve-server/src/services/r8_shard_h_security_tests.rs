//! Adversarial & regression tests for Round-8 Shard H (`#477`, `#489`, `#500`).
//! Note: `#477` command burst fallback unit tests live in `corp_registry_ops_extra_unit_tests.rs`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::market_contracts::{JITA_STATION_ID, THE_FORGE_REGION_ID};
use crate::services::market_trading::{
    handle_orders_escrow, load_market_order_meta, save_market_order_meta, MarketOrderMeta,
};
use crate::services::wallet_serverops::create_bill;
use eve_db::{
    corp_roles, CharacterRecord, CorporationRecord, FleetRecord, InventoryFlag,
    InventoryItemRecord, IskCents, MarketOrderRecord, TrainedSkillRecord, UpwellStructureRecord,
};
use eve_destiny::{CombatState, MovementMode};
use eve_machonet::{service_roles::ROLE_PLAYER, SessionState};
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use glam::DVec3;
use std::collections::{HashMap, HashSet};
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

fn docked_jita_session(char_id: i64, corp_id: i64) -> SessionState {
    let mut s = SessionState::new(9801);
    s.user_id = Some(1);
    s.character_id = Some(char_id);
    s.corporation_id = Some(corp_id);
    s.station_id = Some(JITA_STATION_ID);
    s.solar_system_id = Some(30_000_142);
    s.region_id = Some(THE_FORGE_REGION_ID);
    s.role = ROLE_PLAYER;
    s
}

fn mk_item(
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id,
        type_id,
        owner_id,
        location_id,
        flag,
        quantity,
        singleton: quantity == 1 && !matches!(flag, InventoryFlag::Deliveries | InventoryFlag::CorpSAG1 | InventoryFlag::CorpSAG2 | InventoryFlag::Impounded | InventoryFlag::Locked),
        version: 1,
        ..Default::default()
    }
}

fn sell_entry(item_id: i64, type_id: u32, qty: i64) -> PyRep {
    PyRep::Dict(vec![
        (PyRep::String("itemID".into()), PyRep::Integer(item_id)),
        (PyRep::String("typeID".into()), PyRep::Integer(i64::from(type_id))),
        (PyRep::String("quantity".into()), PyRep::Integer(qty)),
        (PyRep::String("price".into()), PyRep::Real(100.0)),
        (PyRep::String("stationID".into()), PyRep::Integer(JITA_STATION_ID)),
        (PyRep::String("duration".into()), PyRep::Integer(90)),
    ])
}

#[tokio::test]
async fn issue_489_junior_accountant_blocked_and_division_roles_enforced() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let ceo_id = 95_500_099_i64;
    let corp_id = 98_500_489_i64;
    let mut sess = docked_jita_session(char_id, corp_id);
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
            ch.station_id = Some(JITA_STATION_ID);
            ch.balance = IskCents::from_isk(100_000_000);
        }
        let mut corp = { let mut __corp = CorporationRecord::default();
            __corp.corporation_id = corp_id;
            __corp.name = "Audit489Corp".into();
            __corp.ticker = "A489".into();
            __corp.ceo_id = ceo_id;
            __corp.members = HashSet::from([char_id, ceo_id]);
            __corp.set_division_balance(1, IskCents::from_isk(500_000_000));
            __corp };
        corp.credit_division(2, IskCents::from_isk(500_000_000));
        // Grant ROLE_JUNIOR_ACCOUNTANT + full wallet/hangar take bits, but NO Trader/Accountant/Director.
        corp.member_roles.insert(
            char_id,
            corp_roles::ROLE_JUNIOR_ACCOUNTANT
                | corp_roles::ROLE_ACCOUNT_CAN_TAKE[0]
                | corp_roles::ROLE_ACCOUNT_CAN_TAKE[1]
                | corp_roles::ROLE_HANGAR_CAN_TAKE[0]
                | corp_roles::ROLE_DELIVERIES_HANGAR_TAKE,
        );
        snap.corporations.insert(corp_id, corp);
        snap.items.insert(980_101, mk_item(980_101, 34, corp_id, JITA_STATION_ID, InventoryFlag::Deliveries, 50));
        snap.items.insert(980_102, mk_item(980_102, 587, corp_id, JITA_STATION_ID, InventoryFlag::ShipHangar, 1));
        snap.market_orders.insert(
            980_201,
            MarketOrderRecord {
                order_id: 980_201,
                owner_id: corp_id,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: 34,
                is_bid: true,
                price: IskCents::from_isk(50),
                vol_entered: 10,
                vol_remaining: 10,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents::from_isk(500),
            },
        );
        save_market_order_meta(
            snap,
            980_201,
            MarketOrderMeta {
                range: -1,
                duration: 90,
                issue_unix: ctx.now_unix(),
                order_state: None,
                account_key: Some(1001),
                final_vol_remaining: None,
                closed_unix: None,
            },
        );
    });

    let bill_id = create_bill(&ctx, 2, corp_id, 1_000_035, 10_000.0, ctx.now_unix() + 86_400, 0.0, 0, 0);

    // 1. ROLE_JUNIOR_ACCOUNTANT must be denied across all corporate spend & market RPCs.
    let buy_args = [
        PyRep::Integer(JITA_STATION_ID),
        PyRep::Integer(34),
        PyRep::Real(50.0),
        PyRep::Integer(10),
        PyRep::Integer(-1),
        PyRep::Integer(1),
        PyRep::Integer(90),
        PyRep::Bool(true), // useCorp = true (arg index 7)
        PyRep::Integer(1001),
    ];
    assert_eq!(
        ctx.dispatch_rpc(&mut sess, "marketProxy", None, "PlaceBuyOrder", &buy_args, &kw, None)
            .await
            .expect_err("JuniorAccountant cannot place corp buy order")
            .code,
        "CorpRoleRequired"
    );
    assert_eq!(
        ctx.dispatch_rpc(
            &mut sess,
            "marketProxy",
            None,
            "PlaceMultiSellOrder",
            &[PyRep::List(vec![sell_entry(980_101, 34, 10)]), PyRep::Bool(true), PyRep::Integer(90), PyRep::Real(0.0), PyRep::Integer(1001)],
            &kw,
            None,
        )
        .await
        .expect_err("JuniorAccountant cannot place corp sell order")
        .code,
        "CorpRoleRequired"
    );
    assert_eq!(
        ctx.dispatch_rpc(
            &mut sess,
            "marketProxy",
            None,
            "ModifyCharOrder",
            &[PyRep::Integer(980_201), PyRep::Real(55.0), PyRep::Integer(10), PyRep::Real(50.0), PyRep::Integer(10), PyRep::Integer(90), PyRep::Integer(1001)],
            &kw,
            None,
        )
        .await
        .expect_err("JuniorAccountant cannot modify corp order")
        .code,
        "CorpRoleRequired"
    );
    assert_eq!(
        ctx.dispatch_rpc(&mut sess, "marketProxy", None, "CancelCharOrder", &[PyRep::Integer(980_201)], &kw, None)
            .await
            .expect_err("JuniorAccountant cannot cancel corp order")
            .code,
        "CorpRoleRequired"
    );
    assert_eq!(
        ctx.dispatch_rpc(&mut sess, "billMgr", None, "PayCorporationBill", &[PyRep::Integer(bill_id), PyRep::Integer(1001)], &kw, None)
            .await
            .expect_err("JuniorAccountant cannot pay corp bill")
            .code,
        "CorpRoleRequired"
    );
    let autopay_args = [PyRep::Dict(vec![(
        PyRep::Integer(corp_id),
        PyRep::Dict(vec![
            (PyRep::Integer(2), PyRep::Bool(true)),
            (PyRep::String("divisionID".into()), PyRep::Integer(1001)),
        ]),
    )])];
    assert_eq!(
        ctx.dispatch_rpc(&mut sess, "billMgr", None, "SendAutomaticPaySettings", &autopay_args, &kw, None)
            .await
            .expect_err("JuniorAccountant cannot update corp autopay")
            .code,
        "CorpRoleRequired"
    );
    assert_eq!(
        ctx.dispatch_rpc(
            &mut sess,
            "insuranceSvc",
            None,
            "InsureShip",
            &[PyRep::Integer(980_102), PyRep::Real(0.5), PyRep::Bool(true)],
            &kw,
            None,
        )
        .await
        .expect_err("JuniorAccountant cannot insure corp ship")
        .code,
        "CorpRoleDenied"
    );

    // 2. ROLE_TRADER + division 1001 take role succeeds on corp market orders for 1001, fails on bill payment.
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.get_mut(&corp_id).unwrap().member_roles.insert(
            char_id,
            corp_roles::ROLE_TRADER
                | corp_roles::ROLE_ACCOUNT_CAN_TAKE[1]
                | corp_roles::ROLE_DELIVERIES_HANGAR_TAKE,
        );
    });
    assert!(
        ctx.dispatch_rpc(&mut sess, "marketProxy", None, "PlaceBuyOrder", &buy_args, &kw, None)
            .await
            .is_ok(),
        "Trader with division 1001 take role must be able to place corp buy order on 1001"
    );
    assert_eq!(
        ctx.dispatch_rpc(&mut sess, "billMgr", None, "PayCorporationBill", &[PyRep::Integer(bill_id), PyRep::Integer(1001)], &kw, None)
            .await
            .expect_err("Trader without Accountant cannot pay corp bill")
            .code,
        "CorpRoleRequired"
    );

    // 3. ROLE_ACCOUNTANT + division 1001 take role succeeds on bill payment & ship insurance, while wrong division (1002) fails.
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.get_mut(&corp_id).unwrap().member_roles.insert(
            char_id,
            corp_roles::ROLE_ACCOUNTANT | corp_roles::ROLE_ACCOUNT_CAN_TAKE[1],
        );
    });
    assert_eq!(
        ctx.dispatch_rpc(&mut sess, "billMgr", None, "PayCorporationBill", &[PyRep::Integer(bill_id), PyRep::Integer(1002)], &kw, None)
            .await
            .expect_err("Accountant missing division 1002 take role must be denied")
            .code,
        "CorpRoleRequired"
    );
    assert!(
        ctx.dispatch_rpc(&mut sess, "billMgr", None, "PayCorporationBill", &[PyRep::Integer(bill_id), PyRep::Integer(1001)], &kw, None)
            .await
            .is_ok()
    );
    // Session's active corp_account_key is 1000 while Accountant only holds division 1001 take role -> denied on 1000, allowed when switched to 1001.
    assert_eq!(
        ctx.dispatch_rpc(
            &mut sess,
            "insuranceSvc",
            None,
            "InsureShip",
            &[PyRep::Integer(980_102), PyRep::Real(0.5), PyRep::Bool(true)],
            &kw,
            None,
        )
        .await
        .expect_err("InsureShip using division 1000 without division 1000 take role must fail")
        .code,
        "CorpRoleDenied"
    );
    sess.corp_account_key = 1001;
    assert!(
        ctx.dispatch_rpc(
            &mut sess,
            "insuranceSvc",
            None,
            "InsureShip",
            &[PyRep::Integer(980_102), PyRep::Real(0.5), PyRep::Bool(true)],
            &kw,
            None,
        )
        .await
        .is_ok()
    );
}

#[test]
fn issue_489_place_multi_sell_order_restricts_corp_flags_and_hangar_take_roles() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let ceo_id = 95_500_098_i64;
    let corp_id = 98_500_490_i64;
    let mut sess = docked_jita_session(char_id, corp_id);
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
            ch.station_id = Some(JITA_STATION_ID);
        }
        let mut corp = { let mut __corp = CorporationRecord::default();
            __corp.corporation_id = corp_id;
            __corp.name = "FlagAuditCorp".into();
            __corp.ticker = "F489".into();
            __corp.ceo_id = ceo_id;
            __corp.members = HashSet::from([char_id, ceo_id]);
            __corp.set_division_balance(1, IskCents::from_isk(100_000_000));
            __corp };
        // Trader with CorpSAG1 take only — missing CorpSAG2 take and Deliveries take.
        corp.member_roles.insert(
            char_id,
            corp_roles::ROLE_TRADER
                | corp_roles::ROLE_ACCOUNT_CAN_TAKE[0]
                | corp_roles::ROLE_HANGAR_CAN_TAKE[0],
        );
        snap.corporations.insert(corp_id, corp);
        snap.items.insert(981_001, mk_item(981_001, 34, corp_id, JITA_STATION_ID, InventoryFlag::Impounded, 100));
        snap.items.insert(981_002, mk_item(981_002, 34, corp_id, JITA_STATION_ID, InventoryFlag::Locked, 100));
        snap.items.insert(981_003, mk_item(981_003, 60, corp_id, JITA_STATION_ID, InventoryFlag::Deliveries, 100));
        snap.items.insert(981_004, mk_item(981_004, 34, corp_id, JITA_STATION_ID, InventoryFlag::CorpSAG2, 100));
        snap.items.insert(981_005, mk_item(981_005, 34, corp_id, JITA_STATION_ID, InventoryFlag::Deliveries, 100));
        snap.items.insert(981_006, mk_item(981_006, 34, corp_id, JITA_STATION_ID, InventoryFlag::CorpSAG1, 100));
    });

    // Impounded (145), Locked (63), Asset Safety (type 60), unpermitted CorpSAG2, and unpermitted Deliveries must all fail.
    for bad_item in [981_001_i64, 981_002, 981_003, 981_004, 981_005] {
        let res = handle_orders_escrow(
            &ctx,
            &mut sess,
            "marketProxy",
            "PlaceMultiSellOrder",
            &[PyRep::List(vec![sell_entry(bad_item, if bad_item == 981_003 { 60 } else { 34 }, 10)]), PyRep::Bool(true), PyRep::Integer(90), PyRep::Real(0.0), PyRep::Integer(1000)],
            &kw,
        );
        let created_any = ctx.db.with_snapshot(|s| !s.market_orders.is_empty());
        assert!(
            !created_any || res.as_ref().is_some_and(Result::is_err),
            "item {bad_item} must not be sellable by unauthorized member"
        );
    }

    // Permitted CorpSAG1 succeeds immediately.
    let ok = handle_orders_escrow(
        &ctx,
        &mut sess,
        "marketProxy",
        "PlaceMultiSellOrder",
        &[PyRep::List(vec![sell_entry(981_006, 34, 15)]), PyRep::Bool(true), PyRep::Integer(90), PyRep::Real(0.0), PyRep::Integer(1000)],
        &kw,
    )
    .unwrap()
    .expect("permitted CorpSAG1 item must sell");
    let oid = ok.dict_get("orderID").and_then(PyRep::as_i64).unwrap();
    let (ord_rec, snap_clone) = ctx.db.with_snapshot(|s| (s.market_orders[&oid].clone(), s.clone()));
    assert_eq!(load_market_order_meta(&snap_clone, &ord_rec, 0).account_key, Some(1000));
}

#[tokio::test]
async fn issue_500_jump_drive_capacitor_validation_deduction_and_pre_move_ewar() {
    let ctx = test_ctx();
    let jita = 30_000_142_u32;
    let ohkunen = 30_000_204_u32;
    let char_id = 90_000_001_i64;
    let obs_origin = 95_800_501_i64;
    let obs_dest = 95_800_502_i64;
    let kw = HashMap::new();

    let mut sess = SessionState::new(9805);
    ctx.dispatch_rpc(&mut sess, "authentication", None, "Login", &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())], &kw, None).await.unwrap();
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None).await.unwrap();
    let undock = ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None).await.unwrap();
    let beyonce = match &undock {
        PyRep::KeyVal(kv) => kv["beyonceBoundID"].as_i64().unwrap() as u64,
        _ => panic!("expected Undock KeyVal"),
    };
    let ship_id = sess.ship_id.unwrap();
    let corp_id = sess.corporation_id.unwrap();
    let beacon_id = 1_046_500_101_i64;
    let cyno_pilot = 95_800_503_i64;

    let uid_origin = ctx.db.provision_player_account("obs_origin_500", "pw");
    let uid_dest = ctx.db.provision_player_account("obs_dest_500", "pw");
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&ship_id).unwrap().type_id = 23_757; // Archon carrier
        snap.items.insert(
            982_101,
            InventoryItemRecord {
                item_id: 982_101,
                type_id: 16_274,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::SpecializedFuelBay,
                quantity: 50_000,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        for sid in [3_456_u32, 21_610, 21_611] {
            snap.skills.entry(char_id).or_default().insert(
                sid,
                TrainedSkillRecord { type_id: sid, level: 5, skill_points: 256_000.0 },
            );
        }
        snap.structures.insert(
            beacon_id,
            UpwellStructureRecord {
                structure_id: beacon_id,
                type_id: 35_840,
                solar_system_id: ohkunen,
                owner_corp_id: corp_id,
                name: "Ohkunen Cyno".into(),
                position: [1.0e6, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 50,
                shield_hp: 50_000.0,
                armor_hp: 50_000.0,
                hull_hp: 50_000.0,
                ..Default::default()
            },
        );
        let base_char: CharacterRecord = snap.characters.values().next().cloned().expect("base char");
        snap.fleets.insert(
            900_500,
            FleetRecord {
                fleet_id: 900_500,
                boss_id: char_id,
                members: vec![char_id, cyno_pilot],
                ..Default::default()
            },
        );
        snap.characters.insert(
            obs_origin,
            CharacterRecord {
                character_id: obs_origin,
                user_id: uid_origin,
                solar_system_id: jita,
                station_id: None,
                structure_id: None,
                ..base_char.clone()
            },
        );
        snap.characters.insert(
            obs_dest,
            CharacterRecord {
                character_id: obs_dest,
                user_id: uid_dest,
                solar_system_id: ohkunen,
                station_id: None,
                structure_id: None,
                ..base_char
            },
        );
    });
    ctx.db.acquire_online_lock(uid_origin, 99_501, Some(obs_origin), false, ctx.now_unix()).unwrap();
    ctx.db.acquire_online_lock(uid_dest, 99_502, Some(obs_dest), false, ctx.now_unix()).unwrap();

    ctx.invalidate_ship_fitting(ship_id);
    let jita_handle = ctx.systems.get_or_spawn(jita);
    let (_, mut ball) = jita_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    ball.type_id = 23_757;
    ball.mode = MovementMode::Stop;
    jita_handle.spawn_ball(ball).await.unwrap();

    // Register fleet cyno beacon and set low capacitor (10 GJ) -> reject BOTH beacon & fleet cyno jump before consuming fuel.
    crate::services::inventory_fitting::fitting_cyno_portal::cyno_state().write().unwrap().beacons.insert(
        (ctx.instance_id as usize, 982_201),
        crate::services::inventory_fitting::fitting_cyno_portal::FleetModuleBeacon {
            beacon_id: 982_201,
            char_id: cyno_pilot,
            ship_id: 982_201,
            beacon_type_id: 21_096,
            solar_system_id: ohkunen,
            position: DVec3::new(2.0e6, 0.0, 0.0),
            expires_at_unix: ctx.now_unix() + 600,
        },
    );
    ctx.session_change_timers.lock().unwrap().clear();
    let mut cs = jita_handle.get_combat_state(ship_id).await.unwrap().unwrap_or_default();
    let full_cap = cs.max_capacitor.max(10_000.0);
    cs.max_capacitor = full_cap;
    cs.capacitor = 10.0;
    jita_handle.set_combat_state(ship_id, cs.clone()).await.unwrap();
    assert_eq!(ctx.dispatch_rpc(&mut sess, "structureCynoBeaconMgr", None, "CmdJumpToStructureBeacon", &[PyRep::Integer(i64::from(ohkunen)), PyRep::Integer(beacon_id)], &kw, None).await.unwrap_err().code, "NotEnoughCapacitor");
    // rpc-lint: allow live crucible:src/eve-server/ship/BeyonceService.h CmdBeaconJumpFleet
    assert_eq!(ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce), "CmdBeaconJumpFleet", &[PyRep::Integer(cyno_pilot), PyRep::Integer(982_201), PyRep::Integer(i64::from(ohkunen))], &kw, None).await.unwrap_err().code, "NotEnoughCapacitor");
    assert_eq!(ctx.db.get_item(982_101).unwrap().quantity, 50_000, "failed capacitor check must not consume fuel");

    // Seed unowned incoming EWAR entry so OnEwarEnd routes to origin SolarSystem notification target.
    ctx.active_ewar_starts
        .write()
        .unwrap()
        .insert((77_900_001, 77_900_002), vec![(ship_id, "electronic"), (77_900_003, "electronic")]);
    cs.capacitor = full_cap;
    jita_handle.set_combat_state(ship_id, cs).await.unwrap();
    let _ = ctx.drain_notifications_for_char(obs_origin);
    let _ = ctx.drain_notifications_for_char(obs_dest);

    ctx.dispatch_rpc(&mut sess, "structureCynoBeaconMgr", None, "CmdJumpToStructureBeacon", &[PyRep::Integer(i64::from(ohkunen)), PyRep::Integer(beacon_id)], &kw, None)
        .await
        .expect("full capacitor jump must succeed");

    let ohk_cs = ctx.systems.get_or_spawn(ohkunen).get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(ohk_cs.capacitor < ohk_cs.max_capacitor * 0.5, "post-deduction capacitor ratio must be preserved");
    assert!(
        ctx.drain_notifications_for_char(obs_origin).iter().any(|n| n.event == "OnEwarEnd"),
        "OnEwarEnd must flush to origin solar system before ch.solar_system_id updates"
    );
    assert!(
        !ctx.drain_notifications_for_char(obs_dest).iter().any(|n| n.event == "OnEwarEnd"),
        "OnEwarEnd must NOT flush to destination solar system"
    );

    // Also verify StargateJump preserves partial pre-jump capacitor ratio.
    ctx.session_change_timers.lock().unwrap().clear();
    let gate = ctx
        .sde
        .jumps
        .values()
        .find(|j| {
            j.from_solar_system_id == ohkunen
                && ctx
                    .sde
                    .solar_systems
                    .get(&j.to_solar_system_id)
                    .is_some_and(|s| s.security_status < 0.45)
        })
        .expect("lowsec stargate in Ohkunen");
    let ohk_handle = ctx.systems.get_or_spawn(ohkunen);
    ohk_handle.set_ball_position(ship_id, gate.position).await;
    let mut partial_cs: CombatState = ohk_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    partial_cs.capacitor = partial_cs.max_capacitor * 0.37;
    ohk_handle.set_combat_state(ship_id, partial_cs).await.unwrap();
    let new_beyonce = ctx.bind_object(&mut sess, "beyonce", i64::from(ohkunen)).unwrap();
    ctx.dispatch_rpc(&mut sess, "beyonce", Some(new_beyonce), "CmdStargateJump", &[PyRep::Integer(gate.stargate_id)], &kw, None)
        .await
        .expect("stargate jump must succeed");
    let dest_cs = ctx
        .systems
        .get_or_spawn(gate.to_solar_system_id)
        .get_combat_state(ship_id)
        .await
        .unwrap()
        .unwrap();
    assert!(
        ((dest_cs.capacitor / dest_cs.max_capacitor) - 0.37).abs() < 1e-4,
        "stargate jump must preserve pre-jump capacitor ratio"
    );
}
