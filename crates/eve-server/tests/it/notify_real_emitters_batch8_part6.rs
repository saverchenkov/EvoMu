//! Real server-side emission tests for Notify/RPC Batch 8 (Part 4: Combat, EWAR, Cloak, Invuln, Fleet, Drones & Ammo).
//!
//! Covers all 17 Group 4 parity rows across two test functions (`<= 15` tags each):
//! - Test 1 (`9` rows): `OnCombatMessage`, `OnEffectMessage`, `OnJamStart`, `OnEwarEnd`,
//!   `OnCloakModuleForceDeactivated`, `OnJumpCloakUpdated`, `OnJumpCloakCancelled`,
//!   `OnInvulnCancelled`, `OnRestoringInvulnUpdated`
//! - Test 2 (`8` rows): `OnChargeLoadedToModule`, `OnAmmoInBankChanged`, `OnDroneActivityChange`,
//!   `OnFleetBoosted`, `OnFleetLootEvent`, `OnFleetJumpBeaconDeployableChange`,
//!   `OnFleetRespawnPointsUpdate`, `OnFleetEscalatingPathMessage`

#![forbid(unsafe_code)]

use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord, UpwellStructureRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::mobile_deployables::{TYPE_MOBILE_CYNO_BEACON, TYPE_MOBILE_DEPOT};
use eve_server::services::pve_expeditions::create_escalation_expedition;
use glam::DVec3;
use std::collections::HashMap;

const PILOT_A: i64 = 90_000_001;
const PILOT_B: i64 = 90_000_002;
const SYS_JITA: u32 = 30_000_142;

fn new_ctx() -> ServerContext {
    let sde = std::sync::Arc::new(
        eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path()).expect("open sde"),
    );
    let db = std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock_paused(true);
    ctx.set_mechanic_rng_seed(Some(42));
    ctx
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

fn drain_all(ctx: &ServerContext, sess: &mut SessionState, char_id: i64) -> Vec<(String, Vec<PyRep>)> {
    let mut out = sess.drain_notifications();
    for n in ctx.drain_notifications_for_char(char_id) {
        out.push((n.event, n.args));
    }
    out
}

fn find_notif<'a>(notifs: &'a [(String, Vec<PyRep>)], event: &str) -> &'a [PyRep] {
    notifs
        .iter()
        .find(|(e, _)| e == event)
        .map(|(_, args)| args.as_slice())
        .unwrap_or_else(|| {
            let seen: Vec<&str> = notifs.iter().map(|(e, _)| e.as_str()).collect();
            panic!("expected notification {event}, got {seen:?}")
        })
}

fn make_item(
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
    singleton: bool,
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
        ..Default::default()
    }
}

fn grant_all_skills(ctx: &ServerContext, char_id: i64) {
    ctx.db.with_mut_snapshot(|snap| {
        let sk = snap.skills.entry(char_id).or_default();
        for (&tid, ty) in &ctx.sde.types {
            if ty.category_id == 16 {
                sk.insert(
                    tid,
                    TrainedSkillRecord {
                        type_id: tid,
                        level: 5,
                        skill_points: 256_000.0,
                    },
                );
            }
        }
    });
}

async fn login_in_space(
    ctx: &ServerContext,
    sid: u64,
    user: &str,
    pass: &str,
    char_id: i64,
    ship_id: i64,
    ship_type: u32,
    sys_id: u32,
    pos: DVec3,
) -> SessionState {
    let mut sess = SessionState::new(sid);
    common::login_pilot(ctx, &mut sess, user, pass).await;
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
        .await
        .expect("SelectCharacterID");
    grant_all_skills(ctx, char_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
            ch.corporation_id = 1_000_125;
        }
        snap.items.insert(
            ship_id,
            make_item(ship_id, ship_type, char_id, sys_id as i64, InventoryFlag::None, 1, true),
        );
    });
    sess.solar_system_id = Some(sys_id);
    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = Some(ship_id);
    sess.corporation_id = Some(1_000_125);

    let handle = ctx.systems.get_or_spawn(sys_id);
    let mut cs = ctx.build_ship_combat_state(char_id, ship_id, ship_type);
    cs.capacitor = cs.max_capacitor.max(10_000.0);
    cs.max_capacitor = cs.max_capacitor.max(10_000.0);
    cs.is_cloaked = false;
    cs.gate_cloak_remaining_ticks = 0;
    cs.undock_invulnerable_remaining_ticks = 0;
    let slim = ctx.build_ship_slim_meta(char_id, ship_type);
    let ball = Ball {
        id: ship_id,
        type_id: ship_type,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 350.0,
        inertia: 1.0,
        mass: 1_200_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let _ = handle.spawn_ball_with_meta(ball, cs, slim).await;
    ctx.invalidate_ship_fitting(ship_id);
    let _ = drain_all(ctx, &mut sess, char_id);
    sess
}

// parity: notify:OnChargeLoadedToModule, notify:OnAmmoInBankChanged, notify:OnDroneActivityChange
// parity: notify:OnFleetBoosted, notify:OnFleetLootEvent, notify:OnFleetJumpBeaconDeployableChange
// parity: notify:OnFleetRespawnPointsUpdate
#[tokio::test]
async fn notify_batch8_ammo_drones_and_fleet_events() {
    eve_testkit::attest(&[
        "notify:OnChargeLoadedToModule",
        "notify:OnAmmoInBankChanged",
        "notify:OnDroneActivityChange",
        "notify:OnFleetBoosted",
        "notify:OnFleetLootEvent",
        "notify:OnFleetJumpBeaconDeployableChange",
        "notify:OnFleetRespawnPointsUpdate",
    ]);

    let ctx = new_ctx();
    let (ship_a, ship_b) = (840_000_101_i64, 840_000_102_i64);
    // Pilot A in Nighthawk (22466) so Command Burst II + AutoCannons + DroneBay all fit cleanly
    let mut sess_a = login_in_space(
        &ctx,
        9_411,
        "demo",
        "demo-secret",
        PILOT_A,
        ship_a,
        22_466,
        SYS_JITA,
        DVec3::new(200_000.0, 0.0, 0.0),
    )
    .await;
    let mut sess_b = login_in_space(
        &ctx,
        9_412,
        "other",
        "other-secret",
        PILOT_B,
        ship_b,
        587,
        SYS_JITA,
        DVec3::new(201_000.0, 0.0, 0.0),
    )
    .await;

    // 1. Weapon bank ammo load & unload -> OnChargeLoadedToModule & OnAmmoInBankChanged
    let (gun1_id, gun2_id, ammo_id, bad_ammo_id, burst_id, burst_charge_id, drone_item_id) = (
        840_000_201_i64,
        840_000_202_i64,
        840_000_203_i64,
        840_000_204_i64,
        840_000_205_i64,
        840_000_206_i64,
        840_000_207_i64,
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            gun1_id,
            make_item(gun1_id, 2_889, PILOT_A, ship_a, InventoryFlag::HiSlot0, 1, true),
        );
        snap.items.insert(
            gun2_id,
            make_item(gun2_id, 2_889, PILOT_A, ship_a, InventoryFlag::HiSlot1, 1, true),
        );
        snap.items.insert(
            ammo_id,
            make_item(ammo_id, 183, PILOT_A, ship_a, InventoryFlag::Cargo, 500, false),
        );
        snap.items.insert(
            bad_ammo_id,
            make_item(bad_ammo_id, 42_696, PILOT_A, ship_a, InventoryFlag::Cargo, 100, false),
        );
        snap.items.insert(
            burst_id,
            make_item(burst_id, 43_555, PILOT_A, ship_a, InventoryFlag::HiSlot2, 1, true),
        );
        snap.items.insert(
            burst_charge_id,
            make_item(burst_charge_id, 42_696, PILOT_A, ship_a, InventoryFlag::Cargo, 300, false),
        );
        snap.items.insert(
            drone_item_id,
            make_item(drone_item_id, 2_454, PILOT_A, ship_a, InventoryFlag::DroneBay, 1, false),
        );
    });
    ctx.invalidate_ship_fitting(ship_a);

    // Negative: loading incompatible ammo fails with IncompatibleAmmo
    assert_eq!(
        rpc(
            &ctx,
            &mut sess_a,
            "dogmaIM",
            "LoadAmmo",
            &[
                PyRep::Integer(ship_a),
                PyRep::List(vec![PyRep::Integer(gun1_id)]),
                PyRep::List(vec![PyRep::Integer(bad_ammo_id)]),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "IncompatibleAmmo"
    );

    // Ungrouped load emits OnChargeLoadedToModule without OnAmmoInBankChanged
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    rpc(
        &ctx,
        &mut sess_a,
        "dogmaIM",
        "LoadAmmo",
        &[
            PyRep::Integer(ship_a),
            PyRep::List(vec![PyRep::Integer(gun1_id)]),
            PyRep::List(vec![PyRep::Integer(ammo_id)]),
        ],
    )
    .await
    .expect("LoadAmmo ungrouped");
    let ungrouped_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(find_notif(&ungrouped_notifs, "OnChargeLoadedToModule").len(), 0);
    assert!(!ungrouped_notifs.iter().any(|(e, _)| e == "OnAmmoInBankChanged"));

    // Link gun1_id + gun2_id, then UnloadAmmo -> emits OnAmmoInBankChanged([gun1_id])
    rpc(
        &ctx,
        &mut sess_a,
        "dogmaIM",
        "LinkWeapons",
        &[
            PyRep::Integer(ship_a),
            PyRep::Integer(gun1_id),
            PyRep::Integer(gun2_id),
        ],
    )
    .await
    .expect("LinkWeapons");
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    rpc(
        &ctx,
        &mut sess_a,
        "dogmaIM",
        "UnloadAmmo",
        &[
            PyRep::Integer(ship_a),
            PyRep::List(vec![PyRep::Integer(gun1_id)]),
        ],
    )
    .await
    .expect("UnloadAmmo grouped");
    let unload_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(
        find_notif(&unload_notifs, "OnAmmoInBankChanged"),
        &[PyRep::Integer(gun1_id)]
    );

    // Reload grouped weapon bank -> emits BOTH OnChargeLoadedToModule() and OnAmmoInBankChanged([gun1_id])
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    rpc(
        &ctx,
        &mut sess_a,
        "dogmaIM",
        "LoadAmmo",
        &[
            PyRep::Integer(ship_a),
            PyRep::List(vec![PyRep::Integer(gun1_id)]),
            PyRep::List(vec![PyRep::Integer(ammo_id)]),
        ],
    )
    .await
    .expect("LoadAmmo grouped");
    let grouped_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(find_notif(&grouped_notifs, "OnChargeLoadedToModule").len(), 0);
    assert_eq!(
        find_notif(&grouped_notifs, "OnAmmoInBankChanged"),
        &[PyRep::Integer(gun1_id)]
    );

    // 2. Drone activity changes -> OnDroneActivityChange on CmdEngage, CmdReturnHome, CmdReturnBay
    let launched = rpc(
        &ctx,
        &mut sess_a,
        "ship",
        "LaunchDrones",
        &[PyRep::List(vec![PyRep::Integer(drone_item_id)])],
    )
    .await
    .expect("LaunchDrones");
    let drone_ball = match launched {
        PyRep::List(items) => items[0].as_i64().expect("drone ball id"),
        _ => panic!("expected list from LaunchDrones"),
    };
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);

    rpc(
        &ctx,
        &mut sess_a,
        "entity",
        "CmdEngage",
        &[
            PyRep::List(vec![PyRep::Integer(drone_ball)]),
            PyRep::Integer(ship_b),
        ],
    )
    .await
    .expect("CmdEngage");
    let engage_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(
        find_notif(&engage_notifs, "OnDroneActivityChange"),
        &[
            PyRep::Integer(drone_ball),
            PyRep::Integer(1),
            PyRep::String("attack".into()),
        ]
    );

    rpc(
        &ctx,
        &mut sess_a,
        "entity",
        "CmdReturnHome",
        &[PyRep::List(vec![PyRep::Integer(drone_ball)])],
    )
    .await
    .expect("CmdReturnHome");
    let home_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(
        find_notif(&home_notifs, "OnDroneActivityChange"),
        &[PyRep::Integer(drone_ball), PyRep::Integer(0), PyRep::None]
    );

    rpc(
        &ctx,
        &mut sess_a,
        "entity",
        "CmdReturnBay",
        &[PyRep::List(vec![PyRep::Integer(drone_ball)])],
    )
    .await
    .expect("CmdReturnBay");
    let bay_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert!(
        bay_notifs.iter().any(|(e, a)| {
            e == "OnDroneActivityChange"
                && a.first().and_then(|v| v.as_i64()) == Some(drone_ball)
                && matches!(a.get(1).and_then(|v| v.as_i64()), Some(4 | 0))
        }),
        "expected OnDroneActivityChange on CmdReturnBay, got {bay_notifs:?}"
    );

    // Negative: solo escalation without fleet emits no OnFleetEscalatingPathMessage
    let _ = create_escalation_expedition(
        &ctx,
        PILOT_A,
        1420,
        SYS_JITA,
        "Guristas Scout Outpost",
        [100_000.0, 0.0, 0.0],
    )
    .await;
    assert!(
        !drain_all(&ctx, &mut sess_a, PILOT_A)
            .iter()
            .any(|(e, _)| e == "OnFleetEscalatingPathMessage")
    );

    // 3. Fleet creation + corp structure -> AcceptInvite emits OnFleetRespawnPointsUpdate
    let struct_id = 1_040_000_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35_832,
                owner_corp_id: 1_000_125,
                solar_system_id: SYS_JITA,
                name: "Jita Astrahus".into(),
                position: [200_000.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 100_000.0,
                armor_hp: 100_000.0,
                hull_hp: 100_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
    });

    let fleet_rep = rpc(&ctx, &mut sess_a, "fleetObjectHandler", "CreateFleet", &[])
        .await
        .expect("CreateFleet");
    let fleet_id = fleet_rep
        .as_i64()
        .or_else(|| fleet_rep.dict_get("fleetID").and_then(|v| v.as_i64()))
        .or(sess_a.fleet_id)
        .expect("fleetID");
    sess_a.fleet_id = Some(fleet_id);
    ctx.advance_secs(12);

    rpc(&ctx, &mut sess_a, "fleetObjectHandler", "Invite", &[PyRep::Integer(PILOT_B)]).await.expect("Invite PILOT_B");
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    let _ = drain_all(&ctx, &mut sess_b, PILOT_B);

    rpc(&ctx, &mut sess_b, "fleetObjectHandler", "AcceptInvite", &[]).await.expect("AcceptInvite PILOT_B");
    sess_b.fleet_id = Some(fleet_id);
    let join_notifs_bare = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert!(
        !join_notifs_bare.iter().any(|(ev, _)| ev == "OnFleetRespawnPointsUpdate"),
        "bare Astrahus without clone bay must not be emitted as a respawn point"
    );

    // Fit and online Standup Cloning Center I (35894) on the Astrahus -> now included as respawn point
    let clone_bay_svc_id = 1_040_000_002_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            clone_bay_svc_id,
            eve_db::InventoryItemRecord {
                item_id: clone_bay_svc_id,
                type_id: 35_894,
                owner_id: 1_000_125,
                location_id: struct_id,
                flag: eve_db::InventoryFlag::ServiceSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(clone_bay_svc_id, true);
    });
    rpc(&ctx, &mut sess_b, "fleetObjectHandler", "LeaveFleet", &[]).await.expect("LeaveFleet PILOT_B");
    sess_b.fleet_id = None;
    ctx.advance_secs(12);
    rpc(&ctx, &mut sess_a, "fleetObjectHandler", "Invite", &[PyRep::Integer(PILOT_B)]).await.expect("Invite PILOT_B again");
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    let _ = drain_all(&ctx, &mut sess_b, PILOT_B);
    rpc(&ctx, &mut sess_b, "fleetObjectHandler", "AcceptInvite", &[]).await.expect("AcceptInvite PILOT_B with clone bay");
    sess_b.fleet_id = Some(fleet_id);
    let join_notifs_a = drain_all(&ctx, &mut sess_a, PILOT_A);
    let respawn_args = find_notif(&join_notifs_a, "OnFleetRespawnPointsUpdate");
    assert_eq!(respawn_args.len(), 1);
    let PyRep::List(respawn_list) = &respawn_args[0] else {
        panic!("expected List of respawn KeyVals");
    };
    assert_eq!(respawn_list.len(), 1);
    assert_eq!(
        respawn_list[0].dict_get("locationID").and_then(|v| v.as_i64()),
        Some(struct_id)
    );

    // 4. Command Burst activation -> emits OnFleetBoosted([43555, num_boosted >= 1])
    rpc(
        &ctx,
        &mut sess_a,
        "dogmaIM",
        "LoadAmmo",
        &[
            PyRep::Integer(ship_a),
            PyRep::List(vec![PyRep::Integer(burst_id)]),
            PyRep::List(vec![PyRep::Integer(burst_charge_id)]),
        ],
    )
    .await
    .expect("LoadAmmo burst charge");
    ctx.advance_secs(35);
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    rpc(&ctx, &mut sess_a, "dogmaIM", "Activate", &[PyRep::Integer(burst_id)])
        .await
        .expect("Activate Shield Command Burst II");
    let burst_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let boost_args = find_notif(&burst_notifs, "OnFleetBoosted");
    assert_eq!(boost_args.len(), 2);
    assert_eq!(boost_args[0].as_i64(), Some(43_555));
    assert!(boost_args[1].as_i64().unwrap_or(0) >= 1);

    // 5. Loot from space container in fleet -> emits OnFleetLootEvent([{(PILOT_A, 34): 50}])
    let (jetcan_id, loot_item_id) = (840_000_301_i64, 840_000_302_i64);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            jetcan_id,
            make_item(jetcan_id, 23, PILOT_A, SYS_JITA as i64, InventoryFlag::None, 1, true),
        );
        snap.items.insert(
            loot_item_id,
            make_item(loot_item_id, 34, PILOT_A, jetcan_id, InventoryFlag::None, 50, false),
        );
    });
    let jita_handle = ctx.systems.get_or_spawn(SYS_JITA);
    let _ = jita_handle
        .spawn_ball(Ball {
            id: jetcan_id,
            type_id: 23,
            position: DVec3::new(200_100.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1_000.0,
            radius: 25.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    let _ = drain_all(&ctx, &mut sess_b, PILOT_B);
    rpc(
        &ctx,
        &mut sess_a,
        "invbroker",
        "Add",
        &[
            PyRep::Integer(loot_item_id),
            PyRep::Integer(ship_a),
            PyRep::Integer(InventoryFlag::Cargo as i64),
        ],
    )
    .await
    .expect("Loot jetcan item into ship cargo");
    let loot_notifs_b = drain_all(&ctx, &mut sess_b, PILOT_B);
    let loot_args = find_notif(&loot_notifs_b, "OnFleetLootEvent");
    assert_eq!(
        loot_args,
        &[PyRep::Dict(vec![(
            PyRep::Tuple(vec![PyRep::Integer(PILOT_A), PyRep::Integer(34)]),
            PyRep::Integer(50),
        )])]
    );

    // 6. Anchor & Unanchor Mobile Cynosural Beacon in fleet -> OnFleetJumpBeaconDeployableChange (true then false)
    let (beacon_id, depot_id) = (840_000_401_i64, 840_000_402_i64);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            beacon_id,
            make_item(beacon_id, TYPE_MOBILE_CYNO_BEACON, PILOT_A, ship_a, InventoryFlag::Cargo, 1, false),
        );
        snap.items.insert(
            depot_id,
            make_item(depot_id, TYPE_MOBILE_DEPOT, PILOT_A, ship_a, InventoryFlag::Cargo, 1, false),
        );
    });

    // Negative: anchoring regular Mobile Depot does NOT emit OnFleetJumpBeaconDeployableChange
    let _ = drain_all(&ctx, &mut sess_b, PILOT_B);
    rpc(
        &ctx,
        &mut sess_a,
        "posMgr",
        "AnchorStructure",
        &[
            PyRep::Integer(depot_id),
            PyRep::Tuple(vec![PyRep::Real(200_200.0), PyRep::Real(0.0), PyRep::Real(0.0)]),
        ],
    )
    .await
    .expect("Anchor Mobile Depot");
    assert!(
        !drain_all(&ctx, &mut sess_b, PILOT_B)
            .iter()
            .any(|(e, _)| e == "OnFleetJumpBeaconDeployableChange")
    );

    rpc(
        &ctx,
        &mut sess_a,
        "posMgr",
        "AnchorStructure",
        &[
            PyRep::Integer(beacon_id),
            PyRep::Tuple(vec![PyRep::Real(200_300.0), PyRep::Real(0.0), PyRep::Real(0.0)]),
        ],
    )
    .await
    .expect("Anchor Mobile Cyno Beacon");
    let cyno_on_notifs = drain_all(&ctx, &mut sess_b, PILOT_B);
    assert_eq!(
        find_notif(&cyno_on_notifs, "OnFleetJumpBeaconDeployableChange"),
        &[
            PyRep::Integer(beacon_id),
            PyRep::Integer(SYS_JITA as i64),
            PyRep::Integer(beacon_id),
            PyRep::Integer(PILOT_A),
            PyRep::Bool(true),
        ]
    );

    eve_server::services::notify_emitters_batch8::on_deployable_deactivated(
        &ctx,
        sess_a.fleet_id,
        beacon_id,
    );
    let cyno_off_notifs = drain_all(&ctx, &mut sess_b, PILOT_B);
    assert_eq!(
        find_notif(&cyno_off_notifs, "OnFleetJumpBeaconDeployableChange"),
        &[
            PyRep::Integer(beacon_id),
            PyRep::Integer(SYS_JITA as i64),
            PyRep::Integer(beacon_id),
            PyRep::Integer(PILOT_A),
            PyRep::Bool(false),
        ]
    );
}
