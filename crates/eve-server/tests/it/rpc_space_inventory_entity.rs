//! Authentic state-backed integration tests for open space, inventory, and entity RPC rows:
//! - `beyonce.GetCynoJammerState`
//! - `invbroker.ValidateItemCanBeOpened`, `invbroker.ValidateItemListCanBeOpened`, `invbroker.WithdrawPlexFromVault`
//! - `entity.CmdAbandonDrone`, `entity.CmdAssist`, `entity.CmdEngage`, `entity.CmdGuard`,
//!   `entity.CmdReconnectToDrones`, `entity.CmdReturnHome`, `entity.CmdSalvage`, `entity.CmdUnanchor`,
//!   `entity.DisableBehaviorDebugging`, `entity.EnableBehaviorDebugging`, `entity.GetBehaviorTreeManager`,
//!   `entity.GetDebugSpawnPoolFleetEnemyClusters`, `entity.GetExtraFleetMembers`, `entity.OpenShippingUnitStorage`

#![forbid(unsafe_code)]

use std::collections::HashMap;
use glam::DVec3;
use eve_destiny::{Ball, MovementMode};
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};

use super::common;
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::{
    with_combat_runtime, with_mut_combat_runtime, ExtendedNpcEntity,
};

const SYS_JITA: u32 = 30_000_142;
const STATION_JITA_4_4: i64 = 60_003_760;
const PLEX_TYPE_ID: u32 = 44_992;
const HOBGOBLIN_I: u32 = 2454;
const SALVAGE_DRONE_I: u32 = 32_787;
const SMALL_SECURE_CAN: u32 = 3293;
const FREIGHT_CONTAINER: u32 = 17_366;
const TRITANIUM: u32 = 34;

fn dict_get<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match rep {
        PyRep::KeyVal(map) => map.get(key),
        PyRep::Dict(entries) => entries
            .iter()
            .find_map(|(k, v)| (k.as_str() == Some(key)).then_some(v)),
        _ => None,
    }
}

async fn rpc_call(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, kwargs, None).await
}

#[tokio::test]
async fn test_space_cyno_and_inventory_vault_containers() {
    // parity: rpc:beyonce.GetCynoJammerState
    // parity: rpc:invbroker.ValidateItemCanBeOpened
    // parity: rpc:invbroker.ValidateItemListCanBeOpened
    // parity: rpc:invbroker.WithdrawPlexFromVault

    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(40_001);
    let char_id = 90_000_001;
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;

    // 1. Authenticate and dock at Jita 4-4 station
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

    sess.solar_system_id = Some(SYS_JITA);
    sess.station_id = Some(STATION_JITA_4_4);

    // Ensure structure vulnerability window is registered so sov_services handles GetCynoJammerState
    ctx.db.with_mut_snapshot(|s| {
        s.structure_vulnerability_windows.insert(920_001, (1000, 5000));
    });

    // 2. rpc:beyonce.GetCynoJammerState: initially unjammed system returns isJammed = false
    let unjammed_state = rpc_call(&ctx, &mut sess, "beyonce", "GetCynoJammerState", &[], &kw)
        .await
        .expect("GetCynoJammerState initial");
    assert_eq!(dict_get(&unjammed_state, "isJammed"), Some(&PyRep::Bool(false)));
    assert_eq!(dict_get(&unjammed_state, "cynoSuppression"), Some(&PyRep::Bool(false)));
    assert_eq!(dict_get(&unjammed_state, "structureInhibitor"), Some(&PyRep::Bool(false)));

    // Deploy an active mobile cyno inhibitor in space in Jita
    let inhibitor_item_id = 920_001;
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(
            inhibitor_item_id,
            InventoryItemRecord {
                item_id: inhibitor_item_id,
                type_id: 33_700, // Mobile Cynosural Inhibitor
                owner_id: char_id,
                location_id: SYS_JITA as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    // Verify GetCynoJammerState now reports cyno jammed and structure inhibitor active
    let jammed_state = rpc_call(&ctx, &mut sess, "beyonce", "GetCynoJammerState", &[], &kw)
        .await
        .expect("GetCynoJammerState with inhibitor");
    assert_eq!(dict_get(&jammed_state, "isJammed"), Some(&PyRep::Bool(true)));
    assert_eq!(dict_get(&jammed_state, "active"), Some(&PyRep::Bool(true)));
    assert_eq!(dict_get(&jammed_state, "structureInhibitor"), Some(&PyRep::Bool(true)));

    // Remove inhibitor -> system returns to unjammed state
    ctx.db.with_mut_snapshot(|s| {
        s.items.remove(&inhibitor_item_id);
    });
    let cleared_state = rpc_call(&ctx, &mut sess, "beyonce", "GetCynoJammerState", &[], &kw)
        .await
        .expect("GetCynoJammerState cleared");
    assert_eq!(dict_get(&cleared_state, "isJammed"), Some(&PyRep::Bool(false)));

    // Deploy Pharolux Cyno Beacon (35841) -> beacon allows cyno generation and does NOT jam
    let beacon_structure_id = 920_002;
    ctx.db.with_mut_snapshot(|s| {
        s.structures.insert(
            beacon_structure_id,
            eve_db::UpwellStructureRecord {
                structure_id: beacon_structure_id,
                type_id: 35841, // Pharolux Cyno Beacon
                owner_corp_id: 1_000_170,
                solar_system_id: SYS_JITA,
                name: "Pharolux Beacon".into(),
                position: [1_000.0, 1_000.0, 1_000.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 1.0,
                armor_hp: 1.0,
                hull_hp: 1.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
            },
        );
    });
    let beacon_state = rpc_call(&ctx, &mut sess, "beyonce", "GetCynoJammerState", &[], &kw)
        .await
        .expect("GetCynoJammerState with beacon");
    assert_eq!(dict_get(&beacon_state, "isJammed"), Some(&PyRep::Bool(false)));

    // Deploy Tenebrex Cyno Jammer (37534) -> flex jammer actively jams system cynos
    let jammer_structure_id = 920_003;
    ctx.db.with_mut_snapshot(|s| {
        s.structures.insert(
            jammer_structure_id,
            eve_db::UpwellStructureRecord {
                structure_id: jammer_structure_id,
                type_id: 37534, // Tenebrex Cyno Jammer
                owner_corp_id: 1_000_170,
                solar_system_id: SYS_JITA,
                name: "Tenebrex Jammer".into(),
                position: [2_000.0, 2_000.0, 2_000.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 1.0,
                armor_hp: 1.0,
                hull_hp: 1.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
            },
        );
    });
    let tenebrex_state = rpc_call(&ctx, &mut sess, "beyonce", "GetCynoJammerState", &[], &kw)
        .await
        .expect("GetCynoJammerState with Tenebrex");
    assert_eq!(dict_get(&tenebrex_state, "isJammed"), Some(&PyRep::Bool(true)));

    // Clean up structures
    ctx.db.with_mut_snapshot(|s| {
        s.structures.remove(&beacon_structure_id);
        s.structures.remove(&jammer_structure_id);
    });

    // 3. rpc:invbroker.WithdrawPlexFromVault: credit vault with 500 PLEX
    ctx.db.with_mut_snapshot(|s| {
        let cur = s.plex_vault_balance(char_id);
        s.adjust_plex_vault(char_id, 500 - cur).expect("adjust vault");
    });
    assert_eq!(ctx.db.with_snapshot(|s| s.plex_vault_balance(char_id)), 500);

    // Withdraw 100 PLEX units into hangar
    let withdraw_res = rpc_call(
        &ctx,
        &mut sess,
        "invbroker",
        "WithdrawPlexFromVault",
        &[PyRep::Integer(STATION_JITA_4_4), PyRep::Integer(100)],
        &kw,
    )
    .await
    .expect("WithdrawPlexFromVault 100");

    assert_eq!(dict_get(&withdraw_res, "quantityWithdrawn"), Some(&PyRep::Integer(100)));
    assert_eq!(dict_get(&withdraw_res, "remainingVaultBalance"), Some(&PyRep::Integer(400)));
    assert_eq!(dict_get(&withdraw_res, "locationID"), Some(&PyRep::Integer(STATION_JITA_4_4)));
    assert_eq!(dict_get(&withdraw_res, "flag"), Some(&PyRep::Integer(4))); // InventoryFlag::Hangar = 4

    let physical_item_id = dict_get(&withdraw_res, "itemID")
        .and_then(|v| v.as_i64())
        .expect("withdrawn itemID");

    // Verify DB item was created with typeID = 44992 (PLEX), quantity = 100 in Hangar
    let (item_tid, item_qty, item_loc, item_flag) = ctx.db.with_snapshot(|s| {
        let it = s.items.get(&physical_item_id).expect("physical PLEX item exists");
        (it.type_id, it.quantity, it.location_id, it.flag)
    });
    assert_eq!(item_tid, PLEX_TYPE_ID);
    assert_eq!(item_qty, 100);
    assert_eq!(item_loc, STATION_JITA_4_4);
    assert_eq!(item_flag, InventoryFlag::Hangar);

    // Withdraw an additional 50 PLEX units -> merges into existing stack
    let withdraw_merge = rpc_call(
        &ctx,
        &mut sess,
        "invbroker",
        "WithdrawPlexFromVault",
        &[PyRep::Integer(STATION_JITA_4_4), PyRep::Integer(50)],
        &kw,
    )
    .await
    .expect("WithdrawPlexFromVault 50 merge");

    assert_eq!(dict_get(&withdraw_merge, "remainingVaultBalance"), Some(&PyRep::Integer(350)));
    assert_eq!(ctx.db.with_snapshot(|s| s.items.get(&physical_item_id).unwrap().quantity), 150);

    // Attempting to withdraw more PLEX than available in vault fails with NotEnoughPlex
    let withdraw_excess = rpc_call(
        &ctx,
        &mut sess,
        "invbroker",
        "WithdrawPlexFromVault",
        &[PyRep::Integer(STATION_JITA_4_4), PyRep::Integer(1_000)],
        &kw,
    )
    .await
    .unwrap_err();
    assert_eq!(withdraw_excess.code, "NotEnoughPlex");

    // Attempting to withdraw to a remote location where pilot is not docked returns NotDockedAtLocation
    let remote_loc_err = rpc_call(
        &ctx,
        &mut sess,
        "invbroker",
        "WithdrawPlexFromVault",
        &[PyRep::Integer(60_000_001), PyRep::Integer(10)], // remote station
        &kw,
    )
    .await
    .unwrap_err();
    assert_eq!(remote_loc_err.code, "NotDockedAtLocation");

    // 4. rpc:invbroker.ValidateItemCanBeOpened and rpc:invbroker.ValidateItemListCanBeOpened:
    // Create packaged container item (singleton = false)
    let packaged_can_id = 920_101;
    let assembled_can_id = 920_102;
    let non_container_id = 920_103;

    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(
            packaged_can_id,
            InventoryItemRecord {
                item_id: packaged_can_id,
                type_id: SMALL_SECURE_CAN,
                owner_id: char_id,
                location_id: STATION_JITA_4_4,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: false, // Packaged
                version: 1,
                ..Default::default()
            },
        );
        s.items.insert(
            assembled_can_id,
            InventoryItemRecord {
                item_id: assembled_can_id,
                type_id: SMALL_SECURE_CAN,
                owner_id: char_id,
                location_id: STATION_JITA_4_4,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true, // Assembled
                version: 1,
                ..Default::default()
            },
        );
        s.items.insert(
            non_container_id,
            InventoryItemRecord {
                item_id: non_container_id,
                type_id: TRITANIUM, // Mineral, not a container
                owner_id: char_id,
                location_id: STATION_JITA_4_4,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    // Packaged container cannot be opened
    let err_packaged = rpc_call(
        &ctx,
        &mut sess,
        "invbroker",
        "ValidateItemCanBeOpened",
        &[PyRep::Integer(packaged_can_id)],
        &kw,
    )
    .await
    .unwrap_err();
    assert_eq!(err_packaged.code, "CantOpenPackagedItem");

    // Non-container item cannot be opened
    let err_non_cont = rpc_call(
        &ctx,
        &mut sess,
        "invbroker",
        "ValidateItemCanBeOpened",
        &[PyRep::Integer(non_container_id)],
        &kw,
    )
    .await
    .unwrap_err();
    assert_eq!(err_non_cont.code, "NotAContainer");

    // Assembled container validates successfully
    let valid_single = rpc_call(
        &ctx,
        &mut sess,
        "invbroker",
        "ValidateItemCanBeOpened",
        &[PyRep::Integer(assembled_can_id)],
        &kw,
    )
    .await
    .expect("ValidateItemCanBeOpened assembled container");
    assert_eq!(valid_single, PyRep::Bool(true));

    // Batch validation: ValidateItemListCanBeOpened
    let valid_batch = rpc_call(
        &ctx,
        &mut sess,
        "invbroker",
        "ValidateItemListCanBeOpened",
        &[PyRep::List(vec![PyRep::Integer(assembled_can_id)])],
        &kw,
    )
    .await
    .expect("ValidateItemListCanBeOpened assembled");
    assert_eq!(valid_batch, PyRep::Bool(true));

    // Batch validation containing a packaged item fails
    let err_batch = rpc_call(
        &ctx,
        &mut sess,
        "invbroker",
        "ValidateItemListCanBeOpened",
        &[PyRep::List(vec![PyRep::Integer(assembled_can_id), PyRep::Integer(packaged_can_id)])],
        &kw,
    )
    .await
    .unwrap_err();
    assert_eq!(err_batch.code, "CantOpenPackagedItem");
}

#[tokio::test]
async fn test_entity_drone_space_commands() {
    // parity: rpc:entity.CmdAbandonDrone
    // parity: rpc:entity.CmdAssist
    // parity: rpc:entity.CmdEngage
    // parity: rpc:entity.CmdGuard
    // parity: rpc:entity.CmdReconnectToDrones
    // parity: rpc:entity.CmdReturnHome
    // parity: rpc:entity.CmdSalvage
    // parity: rpc:entity.CmdUnanchor

    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(40_002);
    let char_id = 90_000_001;
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;

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

    let ship_id = 930_001;
    ctx.db.with_mut_snapshot(|s| {
        let ch = s.characters.get_mut(&char_id).unwrap();
        ch.active_ship_id = ship_id;
        ch.station_id = None;
    });
    sess.solar_system_id = Some(SYS_JITA);
    sess.station_id = None;
    sess.ship_id = Some(ship_id);

    // Spawn ship in space
    let sys_handle = ctx.systems.get_or_spawn(SYS_JITA);
    let _ = sys_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 606, // Algos destroyer
            position: DVec3::new(10_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 250.0,
            inertia: 0.5,
            mass: 1_000_000.0,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    // Seed combat drone and salvage drone in ship's drone bay
    let combat_drone_item_id = 930_010;
    let salvage_drone_item_id = 930_020;
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 606,
                owner_id: char_id,
                location_id: SYS_JITA as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        s.items.insert(
            combat_drone_item_id,
            InventoryItemRecord {
                item_id: combat_drone_item_id,
                type_id: HOBGOBLIN_I,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::DroneBay,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        s.items.insert(
            salvage_drone_item_id,
            InventoryItemRecord {
                item_id: salvage_drone_item_id,
                type_id: SALVAGE_DRONE_I,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::DroneBay,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    // Launch drones into space
    let launched = rpc_call(
        &ctx,
        &mut sess,
        "ship",
        "LaunchDrones",
        &[PyRep::List(vec![
            PyRep::Integer(combat_drone_item_id),
            PyRep::Integer(salvage_drone_item_id),
        ])],
        &kw,
    )
    .await
    .expect("LaunchDrones");

    let launched_ball_ids: Vec<i64> = match launched {
        PyRep::List(l) => l.iter().filter_map(|x| x.as_i64()).collect(),
        _ => Vec::new(),
    };
    assert_eq!(launched_ball_ids.len(), 2);
    let drone_ball_id = launched_ball_ids[0];

    // Spawn a hostile target ball in space
    let target_ball_id = 930_500;
    let _ = sys_handle
        .spawn_ball(Ball {
            id: target_ball_id,
            type_id: 29986, // Cruiser rat
            position: DVec3::new(15_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 200.0,
            inertia: 0.5,
            mass: 2_000_000.0,
            radius: 80.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    // 1. rpc:entity.CmdEngage: command drones to engage hostile target
    let engage_res = rpc_call(
        &ctx,
        &mut sess,
        "entity",
        "CmdEngage",
        &[PyRep::Integer(target_ball_id)],
        &kw,
    )
    .await
    .expect("CmdEngage");

    assert_eq!(dict_get(&engage_res, "targetID"), Some(&PyRep::Integer(target_ball_id)));
    let dmg = dict_get(&engage_res, "totalDamageDealt").and_then(|v| v.as_f64()).unwrap_or(0.0);
    assert!(dmg > 0.0, "Engaged drones must deal combat damage");

    // Verify active drone in space updated its target_id
    let drone_target = with_combat_runtime(&ctx, |rt| {
        rt.active_drones_in_space.get(&drone_ball_id).and_then(|d| d.target_id)
    });
    assert_eq!(drone_target, Some(target_ball_id));

    // 2. rpc:entity.CmdReturnHome: command drones to return home and orbit ship
    let home_res = rpc_call(&ctx, &mut sess, "entity", "CmdReturnHome", &[], &kw)
        .await
        .expect("CmdReturnHome");
    let returned_count = home_res.as_i64().unwrap_or(0);
    assert!(returned_count >= 1, "At least 1 drone returned home");

    let drone_target_after_home = with_combat_runtime(&ctx, |rt| {
        rt.active_drones_in_space.get(&drone_ball_id).and_then(|d| d.target_id)
    });
    assert_eq!(drone_target_after_home, None, "Returning home clears combat target");

    // 3. rpc:entity.CmdAssist & rpc:entity.CmdGuard: fleet commands
    let fleetmate_char_id = 90_000_888;
    let assist_res = rpc_call(
        &ctx,
        &mut sess,
        "entity",
        "CmdAssist",
        &[PyRep::Integer(fleetmate_char_id)],
        &kw,
    )
    .await
    .expect("CmdAssist");
    assert_eq!(dict_get(&assist_res, "mode"), Some(&PyRep::String("CmdAssist".into())));
    assert_eq!(dict_get(&assist_res, "assignedToCharID"), Some(&PyRep::Integer(fleetmate_char_id)));

    let drone_assist = with_combat_runtime(&ctx, |rt| {
        rt.active_drones_in_space.get(&drone_ball_id).and_then(|d| d.assist_char_id)
    });
    assert_eq!(drone_assist, Some(fleetmate_char_id));

    let guard_res = rpc_call(
        &ctx,
        &mut sess,
        "entity",
        "CmdGuard",
        &[PyRep::Integer(fleetmate_char_id)],
        &kw,
    )
    .await
    .expect("CmdGuard");
    assert_eq!(dict_get(&guard_res, "mode"), Some(&PyRep::String("CmdGuard".into())));
    assert_eq!(dict_get(&guard_res, "assignedToCharID"), Some(&PyRep::Integer(fleetmate_char_id)));

    let drone_guard = with_combat_runtime(&ctx, |rt| {
        rt.active_drones_in_space.get(&drone_ball_id).and_then(|d| d.guard_char_id)
    });
    assert_eq!(drone_guard, Some(fleetmate_char_id));

    // 4. rpc:entity.CmdAbandonDrone: abandon active drones in space
    let abandon_res = rpc_call(&ctx, &mut sess, "entity", "CmdAbandonDrone", &[], &kw)
        .await
        .expect("CmdAbandonDrone");
    let abandoned_ids: Vec<i64> = match abandon_res {
        PyRep::List(l) => l.iter().filter_map(|x| x.as_i64()).collect(),
        _ => Vec::new(),
    };
    assert!(abandoned_ids.contains(&drone_ball_id));

    let is_abandoned = with_combat_runtime(&ctx, |rt| {
        rt.active_drones_in_space.get(&drone_ball_id).map(|d| d.is_abandoned).unwrap_or(false)
    });
    assert!(is_abandoned, "Drone must be flagged as abandoned");

    // 5. rpc:entity.CmdReconnectToDrones: reconnect to abandoned drones
    let reconnect_res = rpc_call(&ctx, &mut sess, "entity", "CmdReconnectToDrones", &[], &kw)
        .await
        .expect("CmdReconnectToDrones");
    let reconnected_ids: Vec<i64> = match reconnect_res {
        PyRep::List(l) => l.iter().filter_map(|x| x.as_i64()).collect(),
        _ => Vec::new(),
    };
    assert!(reconnected_ids.contains(&drone_ball_id));

    let reconnected_owner = with_combat_runtime(&ctx, |rt| {
        rt.active_drones_in_space.get(&drone_ball_id).map(|d| (d.is_abandoned, d.owner_char_id))
    });
    assert_eq!(reconnected_owner, Some((false, char_id)));

    // A drone farther than 250km is out of control range and cannot be reconnected
    let distant_drone_id = 930_995;
    let _ = sys_handle
        .spawn_ball(Ball {
            id: distant_drone_id,
            type_id: HOBGOBLIN_I,
            position: DVec3::new(300_000.0, 0.0, 0.0), // 290km away from ship at 10km
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1000.0,
            radius: 10.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    with_mut_combat_runtime(&ctx, |rt| {
        let mut d = rt.active_drones_in_space.get(&drone_ball_id).cloned().expect("drone");
        d.ball_id = distant_drone_id;
        d.drone_item_id = distant_drone_id;
        d.is_abandoned = true;
        rt.active_drones_in_space.insert(distant_drone_id, d);
    });
    let reconnect_distant = rpc_call(&ctx, &mut sess, "entity", "CmdReconnectToDrones", &[], &kw)
        .await
        .expect("CmdReconnectToDrones distant check");
    let rec_distant_ids: Vec<i64> = match reconnect_distant {
        PyRep::List(l) => l.iter().filter_map(|x| x.as_i64()).collect(),
        _ => Vec::new(),
    };
    assert!(!rec_distant_ids.contains(&distant_drone_id), "Distant drone > 250km cannot be reconnected");

    // 6. rpc:entity.CmdSalvage: command salvage drone to salvage an in-space wreck
    let wreck_id = 930_600;
    let _ = sys_handle
        .spawn_ball(Ball {
            id: wreck_id,
            type_id: 26557, // Cruiser wreck
            position: DVec3::new(11_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 50_000.0,
            radius: 30.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(
            wreck_id,
            InventoryItemRecord {
                item_id: wreck_id,
                type_id: 26557,
                owner_id: char_id,
                location_id: SYS_JITA as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    let salvage_res = rpc_call(
        &ctx,
        &mut sess,
        "entity",
        "CmdSalvage",
        &[PyRep::Integer(wreck_id)],
        &kw,
    )
    .await
    .expect("CmdSalvage");
    assert_eq!(dict_get(&salvage_res, "wreckID"), Some(&PyRep::Integer(wreck_id)));

    // 7. rpc:entity.CmdUnanchor: unanchor target entity in space
    let unanchor_res = rpc_call(
        &ctx,
        &mut sess,
        "entity",
        "CmdUnanchor",
        &[PyRep::Integer(drone_ball_id)],
        &kw,
    )
    .await
    .expect("CmdUnanchor");
    assert_eq!(dict_get(&unanchor_res, "targetID"), Some(&PyRep::Integer(drone_ball_id)));
    assert_eq!(dict_get(&unanchor_res, "unanchored"), Some(&PyRep::Bool(true)));
    assert_eq!(dict_get(&unanchor_res, "isDrone"), Some(&PyRep::Bool(true)));

    // Out of range unanchor (> 5,000m) rejects with TargetOutOfRange
    let distant_unanchor_id = 930_990;
    let _ = sys_handle
        .spawn_ball(Ball {
            id: distant_unanchor_id,
            type_id: HOBGOBLIN_I,
            position: DVec3::new(30_000.0, 0.0, 0.0), // 20km away from ship at 10km
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1000.0,
            radius: 10.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(
            distant_unanchor_id,
            InventoryItemRecord {
                item_id: distant_unanchor_id,
                type_id: HOBGOBLIN_I,
                owner_id: char_id,
                location_id: SYS_JITA as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    let range_err = rpc_call(
        &ctx,
        &mut sess,
        "entity",
        "CmdUnanchor",
        &[PyRep::Integer(distant_unanchor_id)],
        &kw,
    )
    .await
    .unwrap_err();
    assert_eq!(range_err.code, "TargetOutOfRange");

    // Unowned entity unanchor rejects with AccessDenied
    let other_pilot_id = 90_000_002;
    let unowned_ball_id = 930_991;
    let _ = sys_handle
        .spawn_ball(Ball {
            id: unowned_ball_id,
            type_id: HOBGOBLIN_I,
            position: DVec3::new(10_100.0, 0.0, 0.0), // 100m away from ship
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1000.0,
            radius: 10.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(
            unowned_ball_id,
            InventoryItemRecord {
                item_id: unowned_ball_id,
                type_id: HOBGOBLIN_I,
                owner_id: other_pilot_id,
                location_id: SYS_JITA as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    let access_err = rpc_call(
        &ctx,
        &mut sess,
        "entity",
        "CmdUnanchor",
        &[PyRep::Integer(unowned_ball_id)],
        &kw,
    )
    .await
    .unwrap_err();
    assert_eq!(access_err.code, "AccessDenied");
}

#[tokio::test]
async fn test_entity_grid_ai_behavior_and_shipping() {
    // parity: rpc:entity.DisableBehaviorDebugging
    // parity: rpc:entity.EnableBehaviorDebugging
    // parity: rpc:entity.GetBehaviorTreeManager
    // parity: rpc:entity.GetDebugSpawnPoolFleetEnemyClusters
    // parity: rpc:entity.GetExtraFleetMembers
    // parity: rpc:entity.OpenShippingUnitStorage

    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(40_003);
    let char_id = 90_000_001;
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;

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

    let ship_id = 940_001;
    ctx.db.with_mut_snapshot(|s| {
        let ch = s.characters.get_mut(&char_id).unwrap();
        ch.active_ship_id = ship_id;
        ch.station_id = None;
    });
    sess.solar_system_id = Some(SYS_JITA);
    sess.station_id = None;
    sess.ship_id = Some(ship_id);

    let sys_handle = ctx.systems.get_or_spawn(SYS_JITA);
    let _ = sys_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 582, // Heron frigate
            position: DVec3::new(20_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 0.5,
            mass: 1_200_000.0,
            radius: 35.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    // 1. rpc:entity.EnableBehaviorDebugging & rpc:entity.DisableBehaviorDebugging
    let enable_dbg = rpc_call(&ctx, &mut sess, "entity", "EnableBehaviorDebugging", &[], &kw)
        .await
        .expect("EnableBehaviorDebugging");
    assert_eq!(
        dict_get(&enable_dbg, "behaviorDebuggingEnabled"),
        Some(&PyRep::Bool(true))
    );
    assert_eq!(
        dict_get(&enable_dbg, "targetEntityID"),
        Some(&PyRep::Integer(char_id))
    );
    assert!(with_combat_runtime(&ctx, |rt| rt.behavior_debugging_active.contains(&char_id)));

    let disable_dbg = rpc_call(&ctx, &mut sess, "entity", "DisableBehaviorDebugging", &[], &kw)
        .await
        .expect("DisableBehaviorDebugging");
    assert_eq!(
        dict_get(&disable_dbg, "behaviorDebuggingEnabled"),
        Some(&PyRep::Bool(false))
    );
    assert!(!with_combat_runtime(&ctx, |rt| rt.behavior_debugging_active.contains(&char_id)));

    // 2. Spawn NPC entities in space for AI Behavior Tree & Enemy Cluster queries
    let npc_1_id = 940_501;
    let npc_2_id = 940_502;
    with_mut_combat_runtime(&ctx, |rt| {
        rt.npc_entities.insert(
            npc_1_id,
            ExtendedNpcEntity {
                ball_id: npc_1_id,
                type_id: 30259,
                faction_id: 500001,
                archetype: "Sleeper".into(),
                solar_system_id: SYS_JITA,
                spawn_group_id: 101,
                target_id: None,
                aggression_threat: HashMap::new(),
                bounty_isk: 250_000.0,
                optimal_range: 12_000.0,
                orbit_range: 5_000.0,
                damage_vector: eve_destiny::DamageVector::default(),
                entropic_cycles: 0,
                drifter_overshield_hp: 0.0,
                drifter_doomsday_fired: false,
                web_speed_factor: 0.0,
                web_range: 0.0,
                scram_strength: 0.0,
                scram_range: 0.0,
                neut_amount: 0.0,
                ecm_jammed: false,
                remote_shield_rep: 0.0,
                remote_armor_rep: 0.0,
                remote_rep_range: 0.0,
                tag_drop_type_id: None,
                spawn_position: DVec3::new(25_000.0, 0.0, 0.0),
                sleeper_wh_class: 1,
            },
        );
        rt.npc_entities.insert(
            npc_2_id,
            ExtendedNpcEntity {
                ball_id: npc_2_id,
                type_id: 30260,
                faction_id: 500001,
                archetype: "Sleeper".into(),
                solar_system_id: SYS_JITA,
                spawn_group_id: 101,
                target_id: None,
                aggression_threat: HashMap::new(),
                bounty_isk: 350_000.0,
                optimal_range: 15_000.0,
                orbit_range: 6_000.0,
                damage_vector: eve_destiny::DamageVector::default(),
                entropic_cycles: 0,
                drifter_overshield_hp: 0.0,
                drifter_doomsday_fired: false,
                web_speed_factor: 0.0,
                web_range: 0.0,
                scram_strength: 0.0,
                scram_range: 0.0,
                neut_amount: 0.0,
                ecm_jammed: false,
                remote_shield_rep: 0.0,
                remote_armor_rep: 0.0,
                remote_rep_range: 0.0,
                tag_drop_type_id: None,
                spawn_position: DVec3::new(26_000.0, 0.0, 0.0),
                sleeper_wh_class: 1,
            },
        );
    });

    // 3. rpc:entity.GetBehaviorTreeManager: inspect behavior trees in solar system
    let btree_res = rpc_call(&ctx, &mut sess, "entity", "GetBehaviorTreeManager", &[], &kw)
        .await
        .expect("GetBehaviorTreeManager");
    assert_eq!(dict_get(&btree_res, "solarSystemID"), Some(&PyRep::Integer(SYS_JITA as i64)));
    assert_eq!(dict_get(&btree_res, "activeTreesCount"), Some(&PyRep::Integer(2)));
    assert_eq!(dict_get(&btree_res, "managerStatus"), Some(&PyRep::String("Active".into())));

    // 4. rpc:entity.GetDebugSpawnPoolFleetEnemyClusters: query enemy cluster formations
    let clusters_res = rpc_call(
        &ctx,
        &mut sess,
        "entity",
        "GetDebugSpawnPoolFleetEnemyClusters",
        &[],
        &kw,
    )
    .await
    .expect("GetDebugSpawnPoolFleetEnemyClusters");

    let clusters_list = match clusters_res {
        PyRep::List(l) => l,
        _ => Vec::new(),
    };
    assert_eq!(clusters_list.len(), 1);
    let cluster_entry = &clusters_list[0];
    assert_eq!(dict_get(cluster_entry, "clusterID"), Some(&PyRep::Integer(101)));
    assert_eq!(dict_get(cluster_entry, "archetype"), Some(&PyRep::String("Sleeper".into())));
    let cluster_threat = dict_get(cluster_entry, "threatScore")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    assert_eq!(cluster_threat, 600_000.0);

    // 5. rpc:entity.GetExtraFleetMembers: query fleet support members
    let extra_members_res = rpc_call(&ctx, &mut sess, "entity", "GetExtraFleetMembers", &[], &kw)
        .await
        .expect("GetExtraFleetMembers");
    assert_eq!(extra_members_res, PyRep::List(vec![]));

    // 6. rpc:entity.OpenShippingUnitStorage: interact with shipping unit within 5km
    let unit_id = 940_800;
    let _ = sys_handle
        .spawn_ball(Ball {
            id: unit_id,
            type_id: FREIGHT_CONTAINER,
            position: DVec3::new(21_500.0, 0.0, 0.0), // 1,500m from ship at 20,000m
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 50_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(
            unit_id,
            InventoryItemRecord {
                item_id: unit_id,
                type_id: FREIGHT_CONTAINER,
                owner_id: char_id,
                location_id: SYS_JITA as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        // Seed 3 items stored inside the freight shipping unit
        for i in 1..=3 {
            let item_iid = unit_id + i;
            s.items.insert(
                item_iid,
                InventoryItemRecord {
                    item_id: item_iid,
                    type_id: TRITANIUM,
                    owner_id: char_id,
                    location_id: unit_id,
                    flag: InventoryFlag::Cargo,
                    quantity: 5_000 * (i as i32),
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
        }
    });

    let storage_res = rpc_call(
        &ctx,
        &mut sess,
        "entity",
        "OpenShippingUnitStorage",
        &[PyRep::Integer(unit_id)],
        &kw,
    )
    .await
    .expect("OpenShippingUnitStorage within 5km");

    assert_eq!(dict_get(&storage_res, "shippingUnitID"), Some(&PyRep::Integer(unit_id)));
    assert_eq!(dict_get(&storage_res, "storageOpened"), Some(&PyRep::Bool(true)));
    assert_eq!(dict_get(&storage_res, "itemCount"), Some(&PyRep::Integer(3)));

    // Verify distance check: unit beyond 5,000m rejects with TargetOutOfRange
    let distant_unit_id = 940_900;
    let _ = sys_handle
        .spawn_ball(Ball {
            id: distant_unit_id,
            type_id: FREIGHT_CONTAINER,
            position: DVec3::new(35_000.0, 0.0, 0.0), // 15,000m from ship at 20,000m
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 50_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    let distant_err = rpc_call(
        &ctx,
        &mut sess,
        "entity",
        "OpenShippingUnitStorage",
        &[PyRep::Integer(distant_unit_id)],
        &kw,
    )
    .await
    .unwrap_err();
    assert_eq!(distant_err.code, "TargetOutOfRange");
}
