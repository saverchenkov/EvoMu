//! Extra unit and mutation-killing tests for `services::pve_crab_beacons`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::pve_crab_beacons::*;
use crate::services::pve_special_sites::{
    read_pve_special_reg, with_pve_special_reg, AbyssalWeatherKind, RoamingStormRecord,
};
use eve_db::clock::VirtualServerClock;
use eve_db::{InventoryFlag, InventoryItemRecord, WorldRepository};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_sde::{SdeRegistry, SolarSystem, SolarSystemId, TypeId};
use glam::DVec3;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed()))
        .clone()
}

fn build_test_ctx() -> Arc<ServerContext> {
    let db = Arc::new(WorldRepository::new_empty());
    db.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_850_000_000)));
    Arc::new(ServerContext::with_sde_and_repository(shared_sde(), db))
}

fn find_nullsec_system(sde: &SdeRegistry) -> SolarSystemId {
    let mut ids: Vec<SolarSystemId> = sde
        .solar_systems
        .values()
        .filter(|s| {
            s.security_status <= 0.0
                && validate_crab_deployment_system(sde, s.solar_system_id).is_ok()
        })
        .map(|s| s.solar_system_id)
        .collect();
    ids.sort_unstable();
    ids[0]
}

fn find_ship_by_group(sde: &SdeRegistry, group_id: u32) -> TypeId {
    let mut tids: Vec<TypeId> = sde
        .types
        .values()
        .filter(|t| t.category_id == 6 && t.group_id == group_id)
        .map(|t| t.type_id)
        .collect();
    tids.sort_unstable();
    tids[0]
}

fn setup_pilot_in_space(
    ctx: &ServerContext,
    username: &str,
    char_id: i64,
    sys_id: SolarSystemId,
    ship_type_id: TypeId,
    pos: DVec3,
) -> (i64, SessionState) {
    ctx.db
        .provision_gm_account(username, "secret", char_id, username);
    let uid = ctx
        .db
        .with_snapshot(|snap| snap.characters.get(&char_id).map(|c| c.user_id).unwrap_or(1));
    let now = ctx.now_unix();
    let _ = ctx.db.acquire_online_lock_with_computer_hash(
        uid,
        char_id as u64,
        Some(char_id),
        None,
        true,
        now,
    );

    let sys_rec = ctx.sde.solar_systems.get(&sys_id).unwrap();
    let ship_id = ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let sid = snap.next_entity_id;
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.constellation_id = sys_rec.constellation_id;
            ch.region_id = sys_rec.region_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = sid;
        }
        snap.items.insert(
            sid,
            InventoryItemRecord {
                item_id: sid,
                type_id: ship_type_id,
                owner_id: char_id,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.item_positions.insert(sid, [pos.x, pos.y, pos.z]);
        sid
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let cs = ctx.build_ship_combat_state(char_id, ship_id, ship_type_id);
    sys_handle.try_spawn_ball_with_combat(
        Ball {
            id: ship_id,
            type_id: ship_type_id,
            position: pos,
            velocity: DVec3::ZERO,
            max_velocity: 100.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 500.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        cs,
    );

    let mut sess = SessionState::new(char_id as u64);
    sess.user_id = Some(uid);
    sess.character_id = Some(char_id);
    sess.solar_system_id = Some(sys_id);
    sess.constellation_id = Some(sys_rec.constellation_id);
    sess.region_id = Some(sys_rec.region_id);
    sess.ship_id = Some(ship_id);
    (ship_id, sess)
}

#[test]
fn system_validation_jove_wormhole_and_ship_type_list_fallbacks() {
    let mut sde = (*shared_sde()).clone();
    let nullsec = find_nullsec_system(&sde);
    let seed_sys: SolarSystem = sde.solar_systems.get(&nullsec).unwrap().clone();

    // Custom Jove region systems (10000004, 10000017, 10000019), Zarzakh (30100000), and wormhole_class_id Some(2)
    for (sid, reg_id, wh_class) in [
        (30_099_001, 10_000_004, None),
        (30_099_002, 10_000_017, None),
        (30_099_003, 10_000_019, None),
        (30_099_004, 10_000_001, Some(2)),
        (30_100_000, 10_000_001, None),
    ] {
        let mut s = seed_sys.clone();
        s.solar_system_id = sid;
        s.region_id = reg_id;
        s.security_status = -0.5;
        s.wormhole_class_id = wh_class;
        sde.solar_systems.insert(sid, s);
        assert_eq!(
            validate_crab_deployment_system(&sde, sid).unwrap_err().code,
            "CantDeployInWormhole"
        );
    }

    // Custom group_id == GROUP_CRAB_BEACON (4137) type
    let mut custom_crab = sde.types.get(&587).unwrap().clone();
    custom_crab.type_id = 999_601;
    custom_crab.group_id = GROUP_CRAB_BEACON;
    sde.types.insert(999_601, custom_crab);
    assert!(is_crab_beacon_type(&sde, 999_601));

    // Remove type_lists 300 & 946 to exercise fallback group_id matching in is_eligible_crab_ship
    let dread = find_ship_by_group(&sde, 485);
    let carrier = find_ship_by_group(&sde, 547);
    let fax = find_ship_by_group(&sde, 1538);
    let frigate = find_ship_by_group(&sde, 25);
    assert!(is_eligible_crab_ship(&sde, TYPE_CRAB_BEACON, fax));

    sde.type_lists.remove(&300);
    sde.type_lists.remove(&946);
    assert!(is_eligible_crab_ship(&sde, TYPE_CARRIER_CRAB_BEACON, carrier));
    assert!(!is_eligible_crab_ship(&sde, TYPE_CARRIER_CRAB_BEACON, dread));
    assert!(is_eligible_crab_ship(&sde, TYPE_CRAB_BEACON, dread));
    assert!(is_eligible_crab_ship(&sde, TYPE_CRAB_BEACON, carrier));
    assert!(!is_eligible_crab_ship(&sde, TYPE_CRAB_BEACON, frigate));
}

#[tokio::test]
async fn crab_scanning_state_queries_and_unregistered_loot_access_guard() {
    let ctx = build_test_ctx();
    let nullsec = find_nullsec_system(&ctx.sde);
    let carrier = find_ship_by_group(&ctx.sde, 547);
    let (ship_id, sess) = setup_pilot_in_space(
        &ctx,
        "crab_extra",
        91_500_001,
        nullsec,
        carrier,
        DVec3::ZERO,
    );

    // Item in snap.items with TYPE_CRAB_BEACON but not yet in reg.crab_beacons -> CrabScanInProgress
    let unreg_id = 895_001_i64;
    let wrong_sys_id = 895_002_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            unreg_id,
            InventoryItemRecord {
                item_id: unreg_id,
                type_id: TYPE_CRAB_BEACON,
                owner_id: 91_500_001,
                location_id: i64::from(nullsec),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            wrong_sys_id,
            InventoryItemRecord {
                item_id: wrong_sys_id,
                type_id: TYPE_CRAB_BEACON,
                owner_id: 91_500_001,
                location_id: 30_000_142, // different system
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    assert_eq!(
        check_crab_loot_access(&ctx, unreg_id, 91_500_001, false)
            .unwrap_err()
            .code,
        "CrabScanInProgress"
    );
    // Item in wrong system is rejected by ensure_beacon_registered_from_item -> CrabBeaconNotFound
    assert_eq!(
        handle_crab_component_call(
            &ctx,
            &sess,
            nullsec,
            91_500_001,
            ship_id,
            wrong_sys_id,
            "crabBeacon",
            "GetState",
            &[],
        )
        .await
        .unwrap_err()
        .code,
        "CrabBeaconNotFound"
    );

    // Initiate link on unreg_id (auto-registers from item), then advance into Scanning phase and query GetState
    handle_crab_component_call(
        &ctx,
        &sess,
        nullsec,
        91_500_001,
        ship_id,
        unreg_id,
        "linkWithShip",
        "InitiateLink",
        &[],
    )
    .await
    .expect("InitiateLink on auto-registered beacon");

    ctx.advance_secs(CRAB_LINK_DURATION_SECS + 50);
    let scan_state = handle_crab_component_call(
        &ctx,
        &sess,
        nullsec,
        91_500_001,
        ship_id,
        unreg_id,
        "crabBeacon",
        "GetState",
        &[],
    )
    .await
    .expect("GetState in Scanning phase");
    assert_eq!(
        scan_state.dict_get("phase").and_then(|v| v.as_str()),
        Some("scanning")
    );
    assert_eq!(
        scan_state
            .dict_get("scanRemainingSeconds")
            .and_then(|v| v.as_i64()),
        Some((CRAB_SCAN_DURATION_SECS - 50) as i64)
    );
    assert_eq!(
        scan_state.dict_get("wavesSpawned").and_then(|v| v.as_i64()),
        Some(1)
    );
    assert!(
        scan_state
            .dict_get("activeWaveDrones")
            .and_then(|v| v.as_i64())
            .unwrap_or(0)
            > 0
    );

    // Advance +150s (total 200s into scan) -> wave 2 spawns automatically even while wave 1 drones are alive
    ctx.advance_secs(150);
    let scan_w2 = handle_crab_component_call(
        &ctx,
        &sess,
        nullsec,
        91_500_001,
        ship_id,
        unreg_id,
        "linkWithShip",
        "GetLinkState",
        &[],
    )
    .await
    .unwrap();
    assert_eq!(
        scan_w2.dict_get("wavesSpawned").and_then(|v| v.as_i64()),
        Some(2)
    );
}

#[test]
fn clock_advance_roaming_storms_and_zero_delta_interference_noop() {
    let ctx = build_test_ctx();
    let nullsec = find_nullsec_system(&ctx.sde);
    let mut sess = SessionState::new(88);

    // Before special sites state exists, on_server_clock_advanced and tick_all_crab_beacons are fast no-ops
    on_server_clock_advanced(&ctx, 10);
    tick_all_crab_beacons(&ctx);

    // Zero delta on an already-initialized interference entry at band 1 emits no duplicate notification
    record_system_interference_activity(&ctx, &mut sess, nullsec, 0.20);
    assert_eq!(sess.notifications.len(), 1);
    record_system_interference_activity(&ctx, &mut sess, nullsec, 0.0);
    assert_eq!(sess.notifications.len(), 1);

    // Seed a due roaming storm and advance clock to trigger storm movement & sync
    let now = ctx.now_unix();
    with_pve_special_reg(&ctx, |reg| {
        reg.roaming_storms.push(RoamingStormRecord {
            storm_id: 1,
            weather: AbyssalWeatherKind::Electrical,
            center_solar_system_id: nullsec,
            strong_core_systems: vec![nullsec],
            weak_periphery_systems: Vec::new(),
            step_count: 0,
            next_move_at_sec: now,
            recent_centers: vec![nullsec],
        });
    });
    on_server_clock_advanced(&ctx, 60);
    let next_move = read_pve_special_reg(&ctx, |reg| reg.roaming_storms[0].next_move_at_sec);
    assert!(next_move >= now);
}
