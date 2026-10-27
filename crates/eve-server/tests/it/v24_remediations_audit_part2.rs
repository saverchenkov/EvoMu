//! Integration tests (part 2) remediating auditor-flagged rows across ESS reserve pulses,
//! fighters launching/abilities, and structure ops/asset safety.

use std::collections::HashMap;
use eve_db::{
    corp_roles, CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents,
};
use eve_destiny::{Ball, CombatState, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use glam::DVec3;
use crate::common;

async fn setup_pilot(
    ctx: &ServerContext,
    sess_id: u64,
    user_name: &str,
    secret: &str,
    char_id: i64,
) -> SessionState {
    let mut sess = SessionState::new(sess_id);
    common::login_pilot(ctx, &mut sess, user_name, secret).await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("SelectCharacterID");
    sess
}

fn make_ball(id: i64, type_id: u32, pos: DVec3) -> Ball {
    Ball {
        id,
        type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 1000.0,
        inertia: 1.0,
        mass: 100_000.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }
}

#[tokio::test]
async fn test_ess_reserve_pulses_audit() {
    // parity: rpc:essMgr.RequestUnlockReserveBank
    eve_testkit::attest(&["rpc:essMgr.RequestUnlockReserveBank"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let uid = ctx.db.provision_player_account("ess_tester", "sec123");
    let char_id = ctx
        .db
        .create_character(uid, "ESS Tester", 1, 1, 11, 17, &ctx.sde)
        .expect("create_character")
        .character_id;

    let sys_id = 30_004_759u32;
    let ship_id = 910_000_104i64;
    let key_item_id = 920_000_104i64;
    let key_type_id = 57715u32; // 45-minute key
    let ess_ball_id = 930_000_104i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.solar_system_id = sys_id;
            c.station_id = None;
            c.active_ship_id = ship_id;
        }
        snap.system_states.insert(
            sys_id,
            eve_db::SystemDynamicState {
                solar_system_id: sys_id,
                ess_reserve_bank: IskCents::from_isk(100_000_000),
                ..Default::default()
            },
        );
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            key_item_id,
            InventoryItemRecord {
                item_id: key_item_id,
                type_id: key_type_id,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                ..Default::default()
            },
        );
    });

    let mut sess = setup_pilot(&ctx, 1004, "ess_tester", "sec123", char_id).await;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.solar_system_id = sys_id;
            c.station_id = None;
            c.active_ship_id = ship_id;
        }
    });
    sess.solar_system_id = Some(sys_id);
    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = Some(ship_id);

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let ship_ball = make_ball(ship_id, 587, DVec3::new(100.0, 0.0, 0.0));
    sys_handle.spawn_ball(ship_ball).await.expect("spawn ship ball");

    let ess_ball = make_ball(ess_ball_id, 56002, DVec3::new(0.0, 0.0, 0.0));
    sys_handle
        .spawn_ball_with_combat(ess_ball, CombatState::default())
        .await
        .expect("spawn ess ball");

    // Call RequestUnlockReserveBank
    let unlock_res = ctx
        .dispatch_rpc(
            &mut sess,
            "essMgr",
            None,
            "RequestUnlockReserveBank",
            &[PyRep::Integer(key_type_id as i64)],
            &HashMap::new(),
            None,
        )
        .await
        .expect("RequestUnlockReserveBank");

    assert_eq!(
        unlock_res.dict_get("unlocked").and_then(|v| v.as_bool()),
        Some(true)
    );
    assert_eq!(
        unlock_res.dict_get("durationMinutes").and_then(|v| v.as_i64()),
        Some(45)
    );

    let has_key = ctx.db.with_snapshot(|s| s.items.contains_key(&key_item_id));
    assert!(!has_key, "Key must be consumed upon unlocking reserve bank");

    // Verify pulses in GetDataForClientSolarSystem
    let ess_data = ctx
        .dispatch_rpc(
            &mut sess,
            "essMgr",
            None,
            "GetDataForClientSolarSystem",
            &[],
            &HashMap::new(),
            None,
        )
        .await
        .expect("GetDataForClientSolarSystem");

    let pulses_rem = ess_data
        .dict_get("reserveBankPulsesRemaining")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let pulses_total = ess_data
        .dict_get("reserveBankPulsesTotal")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    assert_eq!(pulses_total, 9, "45-minute key gives 9 pulses");
    assert_eq!(pulses_rem, 9);
    assert!(ess_data.dict_get("reserveBankLastPulseInitiated").is_some());
}

#[tokio::test]
async fn test_fighters_tubes_and_multi_activation_audit() {
    // parity: rpc:fighterMgr.LaunchFightersFromTubes, rpc:fighterMgr.CmdActivateAbilitySlots
    eve_testkit::attest(&[
        "rpc:fighterMgr.LaunchFightersFromTubes",
        "rpc:fighterMgr.CmdActivateAbilitySlots",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let uid = ctx.db.provision_player_account("fighter_tester", "sec123");
    let char_id = ctx
        .db
        .create_character(uid, "Fighter Tester", 1, 1, 11, 17, &ctx.sde)
        .expect("create_character")
        .character_id;

    let sys_id = 30_000_142u32;
    let carrier_id = 910_000_105i64;
    let squadron_id = 920_000_105i64;
    let fighter_type_id = 23057u32; // Dragonfly I Light Fighter

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.solar_system_id = sys_id;
            c.station_id = None;
            c.active_ship_id = carrier_id;
        }
        snap.items.insert(
            carrier_id,
            InventoryItemRecord {
                item_id: carrier_id,
                type_id: 23757, // Archon Carrier
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            squadron_id,
            InventoryItemRecord {
                item_id: squadron_id,
                type_id: fighter_type_id,
                owner_id: char_id,
                location_id: carrier_id,
                flag: InventoryFlag::FighterBay,
                quantity: 9,
                singleton: true,
                ..Default::default()
            },
        );
    });

    // Load squadron into tube 0
    ctx.fighter_tube_loads.write().unwrap().insert((carrier_id, 0), squadron_id);

    let mut sess = setup_pilot(&ctx, 1005, "fighter_tester", "sec123", char_id).await;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.solar_system_id = sys_id;
            c.station_id = None;
            c.active_ship_id = carrier_id;
        }
    });
    sess.solar_system_id = Some(sys_id);
    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = Some(carrier_id);

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let carrier_ball = make_ball(carrier_id, 23757, DVec3::new(0.0, 0.0, 0.0));
    sys_handle.spawn_ball(carrier_ball).await.expect("spawn carrier ball");

    // 1. LaunchFightersFromTubes with tube flag 159
    let launch_res = ctx
        .dispatch_rpc(
            &mut sess,
            "fighterMgr",
            None,
            "LaunchFightersFromTubes",
            &[PyRep::List(vec![PyRep::Integer(159)])],
            &HashMap::new(),
            None,
        )
        .await
        .expect("LaunchFightersFromTubes");

    assert!(
        matches!(launch_res, PyRep::Dict(_)),
        "Flag launching must return PyRep::Dict"
    );

    let item_loc = ctx.db.with_snapshot(|s| s.items.get(&squadron_id).map(|it| it.location_id));
    assert_eq!(
        item_loc,
        Some(sys_id as i64),
        "Fighter item must have moved into space"
    );

    // 2. CmdActivateAbilitySlots fighter ability activation
    let fighter_ball_id = ctx
        .active_fighters
        .read()
        .unwrap()
        .keys()
        .copied()
        .next()
        .expect("active fighter ball");

    let act_res = ctx
        .dispatch_rpc(
            &mut sess,
            "fighterMgr",
            None,
            "CmdActivateAbilitySlots",
            &[
                PyRep::List(vec![PyRep::Integer(fighter_ball_id)]),
                PyRep::Integer(1),
            ],
            &HashMap::new(),
            None,
        )
        .await
        .expect("CmdActivateAbilitySlots");

    assert_eq!(
        act_res.dict_get("success").and_then(|v| v.as_bool()),
        Some(true)
    );
}

#[tokio::test]
async fn test_structure_ops_and_asset_safety_audit() {
    // parity: rpc:structureDeployment.Anchor, rpc:structureControl.TakeControl, rpc:structureAssetSafety.GetStructuresICanDeliverTo, feat:structures.asset-safety-manual, feat:lifecycle.asset_safety-auto_delivery
    eve_testkit::attest(&[
        "rpc:structureDeployment.Anchor",
        "rpc:structureControl.TakeControl",
        "rpc:structureAssetSafety.GetStructuresICanDeliverTo",
        "feat:structures.asset-safety-manual",
        "feat:lifecycle.asset_safety-auto_delivery",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let corp_id = 98_000_106i64;
    let uid = ctx.db.provision_player_account("struct_tester", "sec123");
    let char_id = ctx
        .db
        .create_character(uid, "Struct Tester", 1, 1, 11, 17, &ctx.sde)
        .expect("create_character")
        .character_id;

    let sys_id = 30_000_142u32;
    let ship_id = 910_000_106i64;
    let astrahus_item_id = 920_000_106i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.solar_system_id = sys_id;
            c.corporation_id = corp_id;
            c.balance = IskCents::from_f64_isk(1_000_000_000.0);
            c.station_id = None;
            c.active_ship_id = ship_id;
        }
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "Struct Corp".into(),
                ticker: "STRCT".into(),
                ceo_id: char_id,
                alliance_id: None,
                member_roles: HashMap::from([(
                    char_id,
                    corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_STATION_MANAGER,
                )]),
                ..Default::default()
            },
        );
        snap.system_states.insert(
            sys_id,
            eve_db::SystemDynamicState {
                solar_system_id: sys_id,
                ess_reserve_bank: IskCents::from_isk(100_000_000),
                ..Default::default()
            },
        );
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            astrahus_item_id,
            InventoryItemRecord {
                item_id: astrahus_item_id,
                type_id: 35832, // Astrahus Citadels (group 1657)
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    let mut sess = setup_pilot(&ctx, 1006, "struct_tester", "sec123", char_id).await;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.solar_system_id = sys_id;
            c.station_id = None;
            c.active_ship_id = ship_id;
        }
    });
    sess.solar_system_id = Some(sys_id);
    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = Some(ship_id);
    sess.corporation_id = Some(corp_id);

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let ship_ball = make_ball(ship_id, 587, DVec3::ZERO);
    sys_handle.spawn_ball(ship_ball).await.expect("spawn ship ball");

    // 1. structureDeployment.Anchor with V24 client tuple
    // (cargo_item_id, [x,y,z], profile_id, (weekday, hour), name)
    let anchor_args = vec![
        PyRep::Integer(astrahus_item_id),
        PyRep::List(vec![
            PyRep::Real(1000.0),
            PyRep::Real(2000.0),
            PyRep::Real(3000.0),
        ]),
        PyRep::Integer(1),
        PyRep::Tuple(vec![PyRep::Integer(2), PyRep::Integer(18)]),
        PyRep::String("Alpha Astrahus".into()),
    ];
    let anchor_res = ctx
        .dispatch_rpc(
            &mut sess,
            "structureDeployment",
            None,
            "Anchor",
            &anchor_args,
            &HashMap::new(),
            None,
        )
        .await
        .expect("structureDeployment.Anchor");

    let structure_id = anchor_res
        .dict_get("structureID")
        .and_then(|v| v.as_i64())
        .expect("structureID");
    assert!(structure_id > 0);

    let has_astrahus_item = ctx.db.with_snapshot(|s| s.items.contains_key(&astrahus_item_id));
    assert!(!has_astrahus_item, "Deployed item must be consumed from cargo");

    // 2. structureControl.TakeControl
    let take_ctrl_undocked = ctx
        .dispatch_rpc(
            &mut sess,
            "structureControl",
            None,
            "TakeControl",
            &[PyRep::Integer(structure_id)],
            &HashMap::new(),
            None,
        )
        .await;
    assert!(take_ctrl_undocked.is_err(), "TakeControl must fail when not docked");

    sess.structure_id = Some(structure_id);
    sess.station_id = None;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.structure_id = Some(structure_id);
            ch.station_id = None;
        }
    });
    let take_ctrl_res = ctx
        .dispatch_rpc(
            &mut sess,
            "structureControl",
            None,
            "TakeControl",
            &[PyRep::Integer(structure_id)],
            &HashMap::new(),
            None,
        )
        .await
        .expect("structureControl.TakeControl while docked with defense role");

    assert_eq!(
        sess.ship_id,
        Some(structure_id),
        "Pilot ship ID becomes structure ID upon taking control"
    );
    assert_eq!(
        take_ctrl_res.dict_get("success").and_then(|v| v.as_bool()),
        Some(true)
    );

    // 3. structureAssetSafety.GetStructuresICanDeliverTo
    let deliver_res = ctx
        .dispatch_rpc(
            &mut sess,
            "structureAssetSafety",
            None,
            "GetStructuresICanDeliverTo",
            &[],
            &HashMap::new(),
            None,
        )
        .await
        .expect("GetStructuresICanDeliverTo");
    assert!(matches!(deliver_res, PyRep::List(_)));

    // 4. MoveSafetyWrapToStructure with same-system 0.5% fee (feat:structures.asset-safety-manual)
    let wrap_id = 940_000_106i64;
    let wrapped_item_id = 950_000_106i64;
    let initial_balance = ctx
        .db
        .with_snapshot(|s| s.characters.get(&char_id).unwrap().balance);

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            wrapped_item_id,
            InventoryItemRecord {
                item_id: wrapped_item_id,
                type_id: 587,
                owner_id: char_id,
                location_id: 0,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        let mut ext = eve_server::services::station_structure::structure_upwell_ops::load_ext_state(snap);
        ext.asset_safety_wraps.insert(
            wrap_id,
            eve_server::services::station_structure::structure_upwell_ops::AssetSafetyWrapRecord {
                wrap_id,
                owner_id: char_id,
                is_corp: false,
                source_structure_id: structure_id,
                source_structure_name: "Test Structure".into(),
                solar_system_id: sys_id,
                created_unix: ctx.now_unix() - 100,
                deliverable_after_unix: ctx.now_unix() - 10,
                delivered_to_id: None,
                estimated_value_isk: 10_000_000.0,
                item_ids: vec![wrapped_item_id],
                nearest_npc_station_id: Some(60003760),
                auto_delivered: false,
                retrieval_fee_paid: false,
            },
        );
        let auto_wrap_id = 940_000_107i64;
        let auto_item_id = 950_000_107i64;
        snap.items.insert(
            auto_item_id,
            InventoryItemRecord {
                item_id: auto_item_id,
                type_id: 34,
                owner_id: char_id,
                location_id: 0,
                flag: InventoryFlag::None,
                quantity: 50,
                ..Default::default()
            },
        );
        ext.asset_safety_wraps.insert(
            auto_wrap_id,
            eve_server::services::station_structure::structure_upwell_ops::AssetSafetyWrapRecord {
                wrap_id: auto_wrap_id,
                owner_id: char_id,
                is_corp: false,
                source_structure_id: structure_id,
                source_structure_name: "Test Structure".into(),
                solar_system_id: sys_id,
                created_unix: ctx.now_unix() - (21 * 86_400),
                deliverable_after_unix: ctx.now_unix() - (15 * 86_400),
                delivered_to_id: None,
                estimated_value_isk: 500_000.0,
                item_ids: vec![auto_item_id],
                nearest_npc_station_id: Some(60003760),
                auto_delivered: false,
                retrieval_fee_paid: false,
            },
        );
        eve_server::services::station_structure::structure_upwell_ops::save_ext_state(snap, &ext);
    });

    let move_res = ctx
        .dispatch_rpc(
            &mut sess,
            "structureAssetSafety",
            None,
            "MoveSafetyWrapToStructure",
            &[PyRep::Integer(wrap_id), PyRep::Integer(structure_id)],
            &HashMap::new(),
            None,
        )
        .await
        .expect("MoveSafetyWrapToStructure");

    // feat:lifecycle.asset_safety-auto_delivery: wrap older than 20 days is lazily auto-delivered
    let auto_del = ctx.db.with_snapshot(|snap| {
        let ext = eve_server::services::station_structure::structure_upwell_ops::load_ext_state(snap);
        ext.asset_safety_wraps.get(&940_000_107i64).map(|w| (w.auto_delivered, w.delivered_to_id))
    });
    assert_eq!(auto_del, Some((true, Some(60003760))));

    let fee_paid = move_res
        .dict_get("feePaidISK")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    assert!(
        (fee_paid - 50_000.0).abs() < 1.0,
        "Expected 0.5% fee (50,000 ISK), got {fee_paid}"
    );

    let post_balance = ctx
        .db
        .with_snapshot(|s| s.characters.get(&char_id).unwrap().balance);
    assert_eq!(
        initial_balance.0 - post_balance.0,
        5_000_000,
        "50,000.00 ISK in cents"
    );
}
