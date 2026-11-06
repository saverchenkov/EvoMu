#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::character_energy::{consume_character_energy, DEFAULT_MAX_ENERGY};
use crate::services::pve_crab_beacons::{
    check_crab_loot_access, get_system_interference_level_decayed, handle_crab_component_call,
    is_crab_beacon_item, is_eligible_crab_ship, is_ship_crab_linked, on_crab_wave_npc_destroyed,
    on_server_clock_advanced, record_system_interference_activity_with_char, register_crab_beacon,
    tick_all_crab_beacons, validate_crab_deployment_system, CrabBeaconPhase, CRAB_LINK_DURATION_SECS,
    CRAB_SCAN_DURATION_SECS, TYPE_CARRIER_CRAB_BEACON, TYPE_CRAB_BEACON,
};
use crate::services::pvp_filament_mgr::{
    build_character_statistics_for_event, build_leaderboard_rows_for_event, handle as handle_pvp,
    load_pvp_store, record_proving_grounds_combat_result, save_pvp_store, PvpQueueCharacterEntry,
};
use eve_db::{InventoryFlag, InventoryItemRecord, VirtualServerClock, WorldRepository};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::SdeRegistry;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| {
        let mut sde = SdeRegistry::with_canonical_seed();
        for (id, region_id, name, security_status) in [
            (30_099_142_u32, 10_000_002_u32, "SyntheticHighSec", 0.95_f64),
            (30_099_544_u32, 10_000_030_u32, "SyntheticLowSec", 0.25_f64),
            (30_100_000_u32, 11_000_001_u32, "J100001", -1.0_f64),
        ] {
            sde.solar_systems.insert(
                id,
                eve_sde::SolarSystem {
                solar_system_id: id,
                constellation_id: 20_000_020,
                region_id,
                name: name.into(),
                security_status,
                radius: 1.0e12,
                galactic_pos_ly: [0.0, 0.0, 0.0],
                planet_ids: vec![],
                stations: vec![],
                wormhole_class_id: None,
            });
        }
        Arc::new(sde)
    }))
}

fn test_ctx(now_unix: u64) -> ServerContext {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(shared_sde(), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(now_unix)));
    ctx
}

fn find_system_with_sec<F: Fn(f64, u32) -> bool>(sde: &SdeRegistry, pred: F) -> u32 {
    sde.solar_systems
        .values()
        .find(|s| s.wormhole_class_id.is_none() && s.solar_system_id < 30_100_000 && pred(s.security_status, s.region_id))
        .map(|s| s.solar_system_id)
        .expect("matching solar system in SDE")
}

fn setup_pilot_with_ship(ctx: &ServerContext, char_id: i64, corp_id: i64, sys_id: u32, ship_id: i64, ship_tid: u32) {
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("demo char");
        ch.character_id = char_id;
        ch.user_id = char_id as u64;
        ch.name = format!("Pilot {char_id}");
        ch.corporation_id = corp_id;
        ch.station_id = None;
        ch.structure_id = None;
        ch.solar_system_id = sys_id;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: ship_tid,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
}

fn make_space_session(char_id: i64, corp_id: i64, sys_id: u32, ship_id: i64, docked_station: Option<i64>) -> SessionState {
    SessionState {
        session_id: char_id as u64,
        user_id: Some(char_id as u64),
        character_id: Some(char_id),
        corporation_id: Some(corp_id),
        solar_system_id: Some(sys_id),
        ship_id: Some(ship_id),
        station_id: docked_station,
        ..Default::default()
    }
}

fn dict_get<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match rep {
        PyRep::Dict(entries) => entries.iter().find(|(k, _)| k.as_str() == Some(key)).map(|(_, v)| v),
        PyRep::KeyVal(map) => map.get(key),
        _ => None,
    }
}

async fn call_pvp_rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    handle_pvp(ctx, sess, "pvpFilamentMgr", method, args, &HashMap::new())
        .await
        .expect("unhandled pvpFilamentMgr route")
}

#[tokio::test]
async fn crab_beacon_validation_linking_waves_loot_restriction_and_decay() {
    let base_t = 1_800_000_000_u64;
    let ctx = test_ctx(base_t);
    let sde = &ctx.sde;

    // 1. Deployment system & ship hull validation
    assert_eq!(CrabBeaconPhase::Idle.as_str(), "idle");
    assert_eq!(CrabBeaconPhase::SelfDestructed.as_str(), "self_destructed");
    assert_eq!(validate_crab_deployment_system(sde, 99_999_999).unwrap_err().code, "CantDeployInSystem");
    assert_eq!(validate_crab_deployment_system(sde, 30_100_000).unwrap_err().code, "CantDeployInWormhole");
    let hisec_sys = find_system_with_sec(sde, |sec, _| sec >= 0.45);
    let low_sys = find_system_with_sec(sde, |sec, r| sec < 0.45 && !matches!(r, 10_000_004 | 10_000_017 | 10_000_019));
    assert_eq!(validate_crab_deployment_system(sde, hisec_sys).unwrap_err().code, "CantDeployInHighSec");
    assert!(validate_crab_deployment_system(sde, low_sys).is_ok());

    // Carrier (23757 Archon) vs Rifter (587)
    assert!(!is_eligible_crab_ship(sde, TYPE_CRAB_BEACON, 34));
    assert!(!is_eligible_crab_ship(sde, TYPE_CRAB_BEACON, 587));
    assert!(is_eligible_crab_ship(sde, TYPE_CARRIER_CRAB_BEACON, 23757));

    // 2. Beacon registration, range/cloak/eligibility guards & linking
    let deployer_id = 94_000_101_i64;
    let intruder_id = 94_000_102_i64;
    let corp_id = 98_000_401_i64;
    let carrier_id = 1_000_950_001_i64;
    let rifter_id = 1_000_950_002_i64;
    let beacon_id = 1_000_950_010_i64;

    setup_pilot_with_ship(&ctx, deployer_id, corp_id, low_sys, carrier_id, 23757);
    setup_pilot_with_ship(&ctx, intruder_id, corp_id, low_sys, rifter_id, 587);
    register_crab_beacon(&ctx, beacon_id, TYPE_CARRIER_CRAB_BEACON, deployer_id, low_sys, DVec3::ZERO);
    assert!(is_crab_beacon_item(&ctx, beacon_id));
    assert!(!is_ship_crab_linked(&ctx, carrier_id, base_t));

    let sess_dep = make_space_session(deployer_id, corp_id, low_sys, carrier_id, None);
    let sess_int = make_space_session(intruder_id, corp_id, low_sys, rifter_id, None);
    assert_eq!(
        handle_crab_component_call(&ctx, &sess_dep, low_sys, deployer_id, carrier_id, 999_999_999, "linkWithShip", "GetLinkState", &[])
            .await
            .unwrap_err()
            .code,
        "CrabBeaconNotFound"
    );
    assert_eq!(
        handle_crab_component_call(&ctx, &sess_dep, hisec_sys, deployer_id, carrier_id, beacon_id, "linkWithShip", "GetLinkState", &[])
            .await
            .unwrap_err()
            .code,
        "TargetNotInSameSystem"
    );

    // Spawn carrier out of range first -> OutOfCrabLinkRange
    let sys_handle = ctx.systems.get_or_spawn(low_sys);
    let make_ship_ball = |id: i64, tid: u32, pos: DVec3| Ball {
        id,
        type_id: tid,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 100.0,
        inertia: 1.0,
        mass: 10_000_000.0,
        radius: 200.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let _ = sys_handle
        .spawn_ball_with_meta(
            make_ship_ball(carrier_id, 23757, DVec3::new(30_000.0, 0.0, 0.0)),
            ctx.build_ship_combat_state(deployer_id, carrier_id, 23757),
            ctx.build_ship_slim_meta(deployer_id, 23757),
        )
        .await;
    assert_eq!(
        handle_crab_component_call(&ctx, &sess_dep, low_sys, deployer_id, carrier_id, beacon_id, "linkWithShip", "InitiateLink", &[])
            .await
            .unwrap_err()
            .code,
        "OutOfCrabLinkRange"
    );

    // Subcap within range -> IneligibleShipForCrabLink
    let _ = sys_handle
        .spawn_ball_with_meta(
            make_ship_ball(rifter_id, 587, DVec3::new(2_000.0, 0.0, 0.0)),
            ctx.build_ship_combat_state(intruder_id, rifter_id, 587),
            ctx.build_ship_slim_meta(intruder_id, 587),
        )
        .await;
    assert_eq!(
        handle_crab_component_call(&ctx, &sess_int, low_sys, intruder_id, rifter_id, beacon_id, "linkWithShip", "InitiateLink", &[])
            .await
            .unwrap_err()
            .code,
        "IneligibleShipForCrabLink"
    );

    // Move carrier within range -> InitiateLink succeeds, consumes 30 CEQ, sets Linking, blocks re-link
    let _ = sys_handle.remove_ball(carrier_id).await;
    let _ = sys_handle
        .spawn_ball_with_meta(
            make_ship_ball(carrier_id, 23757, DVec3::new(2_000.0, 0.0, 0.0)),
            ctx.build_ship_combat_state(deployer_id, carrier_id, 23757),
            ctx.build_ship_slim_meta(deployer_id, 23757),
        )
        .await;
    let link_kv = handle_crab_component_call(&ctx, &sess_dep, low_sys, deployer_id, carrier_id, beacon_id, "linkWithShip", "InitiateLink", &[])
        .await
        .unwrap();
    assert_eq!(dict_get(&link_kv, "linked"), Some(&PyRep::Bool(true)));
    assert_eq!(dict_get(&link_kv, "ceqRemaining"), Some(&PyRep::Integer(DEFAULT_MAX_ENERGY - 30)));
    assert!(is_ship_crab_linked(&ctx, carrier_id, base_t + 10));
    assert_eq!(check_crab_loot_access(&ctx, beacon_id, deployer_id, false).unwrap_err().code, "CrabScanInProgress");

    // 4th CEQ consumption fails with InsufficientEnergy
    assert!(consume_character_energy(&ctx, deployer_id, 30).is_ok());
    assert!(consume_character_energy(&ctx, deployer_id, 30).is_ok());
    assert_eq!(consume_character_energy(&ctx, deployer_id, 30).unwrap_err().code, "InsufficientEnergy");

    // Advance clock past link -> Scanning + wave clearing -> LootRestricted -> deployer vs intruder access
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(base_t + CRAB_LINK_DURATION_SECS + 1)));
    on_server_clock_advanced(&ctx, CRAB_LINK_DURATION_SECS + 1);
    tokio::task::yield_now().await;
    on_crab_wave_npc_destroyed(&ctx.systems, &ctx.db, &ctx.sde, low_sys, 999_999_999).await;

    // Interference band & decay verification
    record_system_interference_activity_with_char(&ctx, deployer_id, low_sys, 0.65);
    let lvl = get_system_interference_level_decayed(&ctx, low_sys, base_t + CRAB_LINK_DURATION_SECS + 1);
    assert!(lvl >= 0.70);
    let decayed = get_system_interference_level_decayed(&ctx, low_sys, base_t + CRAB_LINK_DURATION_SECS + 20_000);
    assert!(decayed < lvl);

    // Advance past scan, spawn & clear all 3 waves, transition to LootRestricted, then SelfDestructed
    let t_loot = base_t + CRAB_LINK_DURATION_SECS + CRAB_SCAN_DURATION_SECS + 5;
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(t_loot)));
    tick_all_crab_beacons(&ctx);
    tokio::task::yield_now().await;
    let drone_ids: Vec<i64> = ctx.db.with_snapshot(|snap| {
        snap.service_state_blobs
            .get("incursions_state")
            .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
            .and_then(|v| {
                v.get("crab_beacons")?
                    .get(&beacon_id.to_string())?
                    .get("wave_npc_ball_ids")?
                    .as_array()
                    .map(|arr| arr.iter().filter_map(|x| x.as_i64()).collect())
            })
            .unwrap_or_default()
    });
    for did in drone_ids {
        let _ = sys_handle.remove_ball(did).await;
        on_crab_wave_npc_destroyed(&ctx.systems, &ctx.db, &ctx.sde, low_sys, did).await;
    }
    tick_all_crab_beacons(&ctx);
    assert_eq!(check_crab_loot_access(&ctx, beacon_id, intruder_id, false).unwrap_err().code, "LootOwnershipRestricted");
    assert_eq!(check_crab_loot_access(&ctx, beacon_id, deployer_id, false).unwrap(), Some(true));
    assert_eq!(check_crab_loot_access(&ctx, beacon_id, intruder_id, true).unwrap(), Some(true));
    assert_eq!(check_crab_loot_access(&ctx, 999_999_999, intruder_id, false).unwrap(), None);

    // Advance past exclusive loot + self-destruct window -> SelfDestructed
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(t_loot + 5_000)));
    tick_all_crab_beacons(&ctx);
    assert_eq!(check_crab_loot_access(&ctx, beacon_id, deployer_id, false).unwrap_err().code, "ItemNotFound");
}

#[tokio::test]
async fn pvp_filament_mgr_queue_leave_events_and_leaderboard_math() {
    let now = 1_800_000_000_u64;
    let ctx = test_ctx(now);
    let sys_id = find_system_with_sec(&ctx.sde, |sec, _| sec < 0.45);
    let c1 = 94_000_201_i64;
    let c2 = 94_000_202_i64;
    let corp_id = 98_000_402_i64;
    let ship1 = 1_000_960_001_i64;
    let ship2 = 1_000_960_002_i64;
    let fil_item1 = 1_000_960_010_i64;

    setup_pilot_with_ship(&ctx, c1, corp_id, sys_id, ship1, 587);
    setup_pilot_with_ship(&ctx, c2, corp_id, sys_id, ship2, 587);

    // Docked session blocked from JoinPVPQueue
    let mut docked = make_space_session(c1, corp_id, sys_id, ship1, Some(60_003_760));
    assert_eq!(
        call_pvp_rpc(&ctx, &mut docked, "JoinPVPQueue", &[PyRep::Integer(fil_item1)]).await.unwrap_err().code,
        "MustBeInSpace"
    );

    // Space checks: invalid itemID, pod hull, missing item, unowned item
    let mut s1 = make_space_session(c1, corp_id, sys_id, ship1, None);
    assert_eq!(
        call_pvp_rpc(&ctx, &mut s1, "JoinPVPQueue", &[PyRep::Integer(0)]).await.unwrap_err().code,
        "InvalidArgument"
    );
    assert_eq!(
        call_pvp_rpc(&ctx, &mut s1, "JoinPVPQueue", &[PyRep::Integer(fil_item1)]).await.unwrap_err().code,
        "ItemNotFound"
    );

    // Capsule blocked
    let pod_id = 1_000_960_020_i64;
    setup_pilot_with_ship(&ctx, c1, corp_id, sys_id, pod_id, 670);
    s1.ship_id = Some(pod_id);
    assert_eq!(
        call_pvp_rpc(&ctx, &mut s1, "JoinPVPQueue", &[PyRep::Integer(fil_item1)]).await.unwrap_err().code,
        "InvalidShipForFilament"
    );
    s1.ship_id = Some(ship1);

    // Unowned item & non-cargo item blocked
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            fil_item1,
            InventoryItemRecord {
                item_id: fil_item1,
                type_id: 54952,
                owner_id: c2,
                location_id: ship1,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                ..Default::default()
            },
        );
    });
    assert_eq!(
        call_pvp_rpc(&ctx, &mut s1, "JoinPVPQueue", &[PyRep::Integer(fil_item1)]).await.unwrap_err().code,
        "ItemNotOwned"
    );
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(it) = snap.items.get_mut(&fil_item1) {
            it.owner_id = c1;
            it.flag = InventoryFlag::Hangar;
        }
    });
    assert_eq!(
        call_pvp_rpc(&ctx, &mut s1, "JoinPVPQueue", &[PyRep::Integer(fil_item1)]).await.unwrap_err().code,
        "ItemNotInCargo"
    );

    // LeavePVPQueue: NotInPvpQueue, FilamentNotFound, and valid refund
    assert_eq!(call_pvp_rpc(&ctx, &mut s1, "LeavePVPQueue", &[]).await.unwrap_err().code, "NotInPvpQueue");
    ctx.db.with_mut_snapshot(|snap| {
        let mut store = load_pvp_store(snap);
        store.queue.insert(
            c1,
            PvpQueueCharacterEntry {
                item_id: fil_item1,
                type_id: 54952,
                location_id: ship1,
                flag: InventoryFlag::Cargo as u16,
                solar_system_id: sys_id,
                joined_unix: now,
            },
        );
        save_pvp_store(snap, &store);
    });
    assert_eq!(
        call_pvp_rpc(&ctx, &mut s1, "LeavePVPQueue", &[PyRep::Integer(fil_item1 + 99)]).await.unwrap_err().code,
        "FilamentNotFound"
    );
    assert_eq!(call_pvp_rpc(&ctx, &mut s1, "LeavePVPQueue", &[PyRep::Integer(fil_item1)]).await.unwrap(), PyRep::None);

    // Events, combat result recording, leaderboard ordering & character stats
    assert!(call_pvp_rpc(&ctx, &mut s1, "GetAllEvents", &[]).await.is_ok());
    assert!(call_pvp_rpc(&ctx, &mut s1, "GetActiveEvents", &[]).await.is_ok());
    assert!(call_pvp_rpc(&ctx, &mut s1, "GetMostRecentEvent", &[]).await.is_ok());
    let _ = call_pvp_rpc(&ctx, &mut s1, "GetNextEventDate", &[]).await.unwrap();

    ctx.db.with_mut_snapshot(|snap| {
        let mut store = load_pvp_store(snap);
        for cid in [c1, c2] {
            store.queue.insert(
                cid,
                PvpQueueCharacterEntry {
                    item_id: cid,
                    type_id: 54952,
                    location_id: ship1,
                    flag: InventoryFlag::Cargo as u16,
                    solar_system_id: sys_id,
                    joined_unix: now,
                },
            );
        }
        save_pvp_store(snap, &store);
    });
    record_proving_grounds_combat_result(&ctx, c1, c2, 12_500.0).unwrap();
    let lb = call_pvp_rpc(&ctx, &mut s1, "GetLeaderboard", &[PyRep::Integer(171)]).await.unwrap();
    let stats = call_pvp_rpc(&ctx, &mut s1, "GetCharacterStatistics", &[PyRep::Integer(171), PyRep::Integer(c1)]).await.unwrap();
    assert!(matches!(lb, PyRep::List(v) if !v.is_empty()));
    assert!(dict_get(&stats, "wins").is_some());
    ctx.db.with_snapshot(|snap| {
        let st = load_pvp_store(snap);
        assert!(!build_leaderboard_rows_for_event(&st, 171).is_empty());
        assert_eq!(dict_get(&build_character_statistics_for_event(&st, 171, c1), "losses"), Some(&PyRep::Integer(0)));
        assert_eq!(dict_get(&build_character_statistics_for_event(&st, 171, c2), "losses"), Some(&PyRep::Integer(1)));
    });

    assert!(handle_pvp(&ctx, &mut s1, "machoNet", "GetAllEvents", &[], &HashMap::new()).await.is_none());
}
