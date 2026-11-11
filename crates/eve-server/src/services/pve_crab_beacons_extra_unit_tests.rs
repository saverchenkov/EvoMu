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

mod w4_ops_pve_mut_extra_tests {
    use crate::router::ServerContext;
    use crate::services::pve_crab_beacons::*;
    use crate::services::pve_special_sites::with_pve_special_reg;
    use eve_db::clock::VirtualServerClock;
    use eve_db::{InventoryFlag, InventoryItemRecord, WorldRepository};
    use eve_destiny::{Ball, MovementMode};
    use eve_machonet::SessionState;
    use eve_sde::{SdeRegistry, SolarSystemId, TypeId};
    use glam::DVec3;
    use std::sync::{Arc, OnceLock};

    fn shared_sde() -> Arc<SdeRegistry> {
        static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
        Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
    }

    fn make_ctx_with_sde(sde: Arc<SdeRegistry>) -> ServerContext {
        let db = Arc::new(WorldRepository::new_empty());
        db.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
        ServerContext::with_sde_and_repository(sde, db)
            .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    }

    fn find_lowsec(sde: &SdeRegistry) -> SolarSystemId {
        let mut ids: Vec<SolarSystemId> = sde
            .solar_systems
            .values()
            .filter(|s| s.security_status > 0.0 && validate_crab_deployment_system(sde, s.solar_system_id).is_ok())
            .map(|s| s.solar_system_id)
            .collect();
        ids.sort_unstable();
        ids[0]
    }

    fn find_ship(sde: &SdeRegistry, group_id: u32) -> TypeId {
        let mut ids: Vec<TypeId> = sde
            .types
            .values()
            .filter(|t| t.category_id == 6 && t.group_id == group_id)
            .map(|t| t.type_id)
            .collect();
        ids.sort_unstable();
        ids[0]
    }

    fn spawn_pilot(
        ctx: &ServerContext,
        char_id: i64,
        sys_id: SolarSystemId,
        ship_type_id: TypeId,
        pos: DVec3,
        station_id: Option<i64>,
    ) -> (i64, SessionState) {
        ctx.db.provision_gm_account(&format!("crab_w4_{char_id}"), "pw", char_id, "CrabW4");
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
        let ship_id = ctx.db.with_mut_snapshot(|snap| {
            snap.next_entity_id += 1;
            let sid = snap.next_entity_id;
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.solar_system_id = sys_id;
                ch.station_id = station_id;
                ch.structure_id = None;
                ch.active_ship_id = sid;
            }
            snap.items.insert(
                sid,
                InventoryItemRecord {
                    item_id: sid,
                    type_id: ship_type_id,
                    owner_id: char_id,
                    location_id: station_id.unwrap_or(i64::from(sys_id)),
                    flag: if station_id.is_some() { InventoryFlag::Hangar } else { InventoryFlag::None },
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.item_positions.insert(sid, [pos.x, pos.y, pos.z]);
            sid
        });
        if station_id.is_none() {
            let sys_h = ctx.systems.get_or_spawn(sys_id);
            let cs = ctx.build_ship_combat_state(char_id, ship_id, ship_type_id);
            sys_h.try_spawn_ball_with_combat(
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
        }
        let mut sess = SessionState::new(char_id as u64);
        sess.user_id = Some(uid);
        sess.character_id = Some(char_id);
        sess.solar_system_id = Some(sys_id);
        sess.station_id = station_id;
        sess.ship_id = Some(ship_id);
        (ship_id, sess)
    }

    #[tokio::test]
    async fn crab_ops_wave_drone_fallbacks_mutaplasmid_fallbacks_and_ceq_clamping() {
        // 1. Custom SDE with ZERO group 4037 types and ZERO group 1964 / hardcoded mutaplasmids
        let mut stripped_sde = (*shared_sde()).clone();
        let lowsec = find_lowsec(&stripped_sde);
        let carrier = find_ship(&stripped_sde, 547);
        stripped_sde.types.retain(|&tid, t| {
            t.group_id != 4037 && t.group_id != 1964 && tid != 60_476 && tid != 60_477
        });
        let ctx = make_ctx_with_sde(Arc::new(stripped_sde));
        let (ship_id, sess) = spawn_pilot(&ctx, 94_300_001, lowsec, carrier, DVec3::ZERO, None);
        let b_carrier = 950_001_i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                b_carrier,
                InventoryItemRecord {
                    item_id: b_carrier,
                    type_id: TYPE_CARRIER_CRAB_BEACON,
                    owner_id: 94_300_001,
                    location_id: i64::from(lowsec),
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.item_positions.insert(b_carrier, [0.0, 0.0, 0.0]);
        });
        register_crab_beacon(&ctx, b_carrier, TYPE_CARRIER_CRAB_BEACON, 94_300_001, lowsec, DVec3::ZERO);

        // Set interference > 1.0 in registry -> ExcessInterference on InitiateLink, then 0.90 -> clamped cost
        with_pve_special_reg(&ctx, |reg| {
            reg.interference_by_system.insert(
                lowsec,
                eve_db::SystemInterferenceRecord {
                    solar_system_id: lowsec,
                    interference_level: 2.5,
                    band: 3,
                    last_updated_sec: ctx.now_unix(),
                },
            );
        });
        let excess_res = handle_crab_component_call(
            &ctx,
            &sess,
            lowsec,
            94_300_001,
            ship_id,
            b_carrier,
            "linkWithShip",
            "InitiateLink",
            &[],
        )
        .await;
        assert!(excess_res.is_err(), "interference >= 1.0 must reject InitiateLink");

        with_pve_special_reg(&ctx, |reg| {
            reg.interference_by_system.insert(
                lowsec,
                eve_db::SystemInterferenceRecord {
                    solar_system_id: lowsec,
                    interference_level: 0.90,
                    band: 3,
                    last_updated_sec: ctx.now_unix(),
                },
            );
        });
        let state_clamped = handle_crab_component_call(
            &ctx,
            &sess,
            lowsec,
            94_300_001,
            ship_id,
            b_carrier,
            "linkWithShip",
            "GetLinkState",
            &[],
        )
        .await
        .expect("get link state");
        assert_eq!(state_clamped.dict_get("phase").and_then(|v| v.as_str()), Some("idle"));

        // InitiateLink with level 0.90 -> ceqCost == 33, modifier == 1.09
        let link_high = handle_crab_component_call(
            &ctx,
            &sess,
            lowsec,
            94_300_001,
            ship_id,
            b_carrier,
            "linkWithShip",
            "InitiateLink",
            &[],
        )
        .await
        .expect("initiate link high interference");
        assert_eq!(link_high.dict_get("ceqCost").and_then(|v| v.as_i64()), Some(33));
        assert!((link_high.dict_get("interferenceCostModifier").and_then(|v| v.as_f64()).unwrap() - 1.09).abs() < 1e-6);

        // Second beacon tested with level -0.5 clamped to 0.0 -> (30, 1.0) via quick pilot
        let (s_low, sess_low) = spawn_pilot(&ctx, 94_300_009, lowsec, carrier, DVec3::ZERO, None);
        let b_low = 950_009_i64;
        register_crab_beacon(&ctx, b_low, TYPE_CRAB_BEACON, 94_300_009, lowsec, DVec3::ZERO);
        with_pve_special_reg(&ctx, |reg| {
            reg.interference_by_system.get_mut(&lowsec).unwrap().interference_level = -0.5;
        });
        let link_ok = handle_crab_component_call(
            &ctx,
            &sess_low,
            lowsec,
            94_300_009,
            s_low,
            b_low,
            "linkWithShip",
            "InitiateLink",
            &[],
        )
        .await
        .expect("initiate link low clamped");
        assert_eq!(link_ok.dict_get("ceqCost").and_then(|v| v.as_i64()), Some(30));
        assert!((link_ok.dict_get("interferenceCostModifier").and_then(|v| v.as_f64()).unwrap() - 1.0).abs() < 1e-6);

        // Link finishes -> spawns carrier wave 1 (4 fallback drones!)
        ctx.advance_secs(CRAB_LINK_DURATION_SECS);
        let w1 = read_crab_beacons(&ctx, |b| b.get(&b_carrier).unwrap().wave_npc_ball_ids.clone());
        assert_eq!(w1.len(), 4);

        // Destroy wave 1 drones WITHOUT calling on_crab_wave_npc_destroyed -> tick auto-spawns wave 2 (5 drones)!
        let sys_h = ctx.systems.get_or_spawn(lowsec);
        for &bid in &w1 {
            sys_h.try_update_combat_state(bid, |cs| cs.structure_hp = 0.0);
        }
        tick_crab_beacons_in_system(&ctx, lowsec).await;
        let w2 = read_crab_beacons(&ctx, |b| {
            let rec = b.get(&b_carrier).unwrap();
            assert_eq!(rec.waves_spawned, 2);
            rec.wave_npc_ball_ids.clone()
        });
        assert_eq!(w2.len(), 5);

        // Kill wave 2 via on_crab_wave_npc_destroyed partial then final -> spawns wave 3 (4 drones)!
        for &bid in &w2[..w2.len() - 1] {
            let _ = sys_h.remove_ball(bid).await;
            on_crab_wave_npc_destroyed(&ctx.systems, &ctx.db, &ctx.sde, lowsec, bid).await;
        }
        assert_eq!(read_crab_beacons(&ctx, |b| b.get(&b_carrier).unwrap().waves_spawned), 2);
        let last_w2 = *w2.last().unwrap();
        let _ = sys_h.remove_ball(last_w2).await;
        on_crab_wave_npc_destroyed(&ctx.systems, &ctx.db, &ctx.sde, lowsec, last_w2).await;
        let w3 = read_crab_beacons(&ctx, |b| {
            let rec = b.get(&b_carrier).unwrap();
            assert_eq!(rec.waves_spawned, 3);
            rec.wave_npc_ball_ids.clone()
        });
        assert_eq!(w3.len(), 4);

        // Scan duration expires while w3 drones STILL ALIVE -> stays Scanning!
        ctx.advance_secs(CRAB_SCAN_DURATION_SECS + 5);
        assert_eq!(
            read_crab_beacons(&ctx, |b| b.get(&b_carrier).unwrap().phase),
            CrabBeaconPhase::Scanning
        );

        // Kill w3 drones and tick -> finalizes scan with fallback [60476, 60477] mutaplasmids + 10 survey data!
        for &bid in &w3 {
            sys_h.try_update_combat_state(bid, |cs| cs.structure_hp = 0.0);
        }
        tick_crab_beacons_in_system(&ctx, lowsec).await;
        assert_eq!(
            read_crab_beacons(&ctx, |b| b.get(&b_carrier).unwrap().phase),
            CrabBeaconPhase::LootRestricted
        );
        let loot_types: Vec<TypeId> = ctx.db.with_snapshot(|snap| {
            snap.items
                .values()
                .filter(|i| i.location_id == b_carrier)
                .map(|i| i.type_id)
                .collect()
        });
        assert!(loot_types.contains(&60_459));
        assert!(loot_types.contains(&60_476) || loot_types.contains(&60_477));

        // 2. Second stripped SDE keeping group 1964 items != 60476/60477 to hit group 1964 modulo fallback
        let mut sde_mutas = (*shared_sde()).clone();
        sde_mutas.types.retain(|&tid, _| tid != 60_476 && tid != 60_477);
        if sde_mutas.types.values().any(|t| t.group_id == 1964) {
            let ctx2 = make_ctx_with_sde(Arc::new(sde_mutas));
            let (s2, sess2) = spawn_pilot(&ctx2, 94_300_002, lowsec, carrier, DVec3::ZERO, None);
            let b2 = 950_002_i64;
            register_crab_beacon(&ctx2, b2, TYPE_CRAB_BEACON, 94_300_002, lowsec, DVec3::ZERO);
            handle_crab_component_call(&ctx2, &sess2, lowsec, 94_300_002, s2, b2, "linkWithShip", "InitiateLink", &[])
                .await
                .unwrap();
            ctx2.advance_secs(CRAB_LINK_DURATION_SECS + CRAB_SCAN_DURATION_SECS);
            let h2 = ctx2.systems.get_or_spawn(lowsec);
            for round in 0..3 {
                let ids = read_crab_beacons(&ctx2, |b| b.get(&b2).unwrap().wave_npc_ball_ids.clone());
                for id in ids {
                    h2.try_update_combat_state(id, |cs| cs.structure_hp = 0.0);
                }
                tick_crab_beacons_in_system(&ctx2, lowsec).await;
                if round == 2 {
                    break;
                }
            }
            assert_eq!(
                read_crab_beacons(&ctx2, |b| b.get(&b2).unwrap().phase),
                CrabBeaconPhase::LootRestricted
            );
        }
    }

    #[tokio::test]
    async fn crab_linking_invalidation_range_warp_dock_death_and_cluster_notify() {
        let sde = shared_sde();
        let ctx = make_ctx_with_sde(Arc::clone(&sde));
        let lowsec = find_lowsec(&sde);
        let dread = find_ship(&sde, 485);
        let mut other_sys = lowsec + 1;
        if let Some(sys) = sde.solar_systems.keys().find(|&&id| id != lowsec && id < 31_000_000) {
            other_sys = *sys;
        }

        let (ship_a, sess_a) = spawn_pilot(&ctx, 94_300_010, lowsec, dread, DVec3::ZERO, None);
        let (_ship_docked, _) =
            spawn_pilot(&ctx, 94_300_011, lowsec, dread, DVec3::ZERO, Some(60_003_760));
        let (_ship_other, _) = spawn_pilot(&ctx, 94_300_012, other_sys, dread, DVec3::ZERO, None);

        // Clusterwide notification reaches in-space AND docked pilots in `lowsec`, NOT `other_sys`
        let _ = ctx.drain_notifications_for_char(94_300_010);
        let _ = ctx.drain_notifications_for_char(94_300_011);
        let _ = ctx.drain_notifications_for_char(94_300_012);
        record_system_interference_activity_with_char(&ctx, 94_300_010, lowsec, 0.36);
        assert!(ctx
            .drain_notifications_for_char(94_300_010)
            .iter()
            .any(|n| n.event == "OnSolarsystemInterferenceChanged"));
        assert!(ctx
            .drain_notifications_for_char(94_300_011)
            .iter()
            .any(|n| n.event == "OnSolarsystemInterferenceChanged"));
        assert!(!ctx
            .drain_notifications_for_char(94_300_012)
            .iter()
            .any(|n| n.event == "OnSolarsystemInterferenceChanged"));

        // Exact ship_crab_link_expiry_unix across beacon + revert on out-of-range / warp / dock / death
        let b_a = 951_001_i64;
        register_crab_beacon(&ctx, b_a, TYPE_CRAB_BEACON, 94_300_010, lowsec, DVec3::ZERO);
        handle_crab_component_call(&ctx, &sess_a, lowsec, 94_300_010, ship_a, b_a, "linkWithShip", "InitiateLink", &[])
            .await
            .unwrap();
        let exp = ship_crab_link_expiry_unix(&ctx, ship_a);
        assert!(exp > ctx.now_unix());
        assert!(!is_ship_crab_linked(&ctx, ship_a, exp)); // now == expiry is false!

        // Out of range reverts Linking -> Idle
        let sys_h = ctx.systems.get_or_spawn(lowsec);
        let _ = sys_h
            .spawn_ball(Ball {
                id: ship_a,
                type_id: dread,
                position: DVec3::new(CRAB_LINK_RANGE_M + 5_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 100.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 500.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await;
        tick_crab_beacons_in_system(&ctx, lowsec).await;
        assert_eq!(read_crab_beacons(&ctx, |b| b.get(&b_a).unwrap().phase), CrabBeaconPhase::Idle);

        // Move back in range, re-link, then enter Warp -> reverts Linking -> Idle
        let _ = sys_h
            .spawn_ball(Ball {
                id: ship_a,
                type_id: dread,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 100.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 500.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await;
        handle_crab_component_call(&ctx, &sess_a, lowsec, 94_300_010, ship_a, b_a, "linkWithShip", "InitiateLink", &[])
            .await
            .unwrap();
        let _ = sys_h
            .spawn_ball(Ball {
                id: ship_a,
                type_id: dread,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 100.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 500.0,
                speed_fraction: 0.0,
                mode: MovementMode::Warp {
                    origin: DVec3::ZERO,
                    destination: DVec3::new(1e9, 0.0, 0.0),
                    phase: eve_destiny::WarpPhase::Cruising,
                    warp_speed_au_s: 1.5,
                    total_distance: 1e9,
                    elapsed_warp_time: 0.0,
                },
            })
            .await;
        tick_crab_beacons_in_system(&ctx, lowsec).await;
        assert_eq!(read_crab_beacons(&ctx, |b| b.get(&b_a).unwrap().phase), CrabBeaconPhase::Idle);

        // Destroy ship -> reverts Linking -> Idle
        let _ = sys_h
            .spawn_ball(Ball {
                id: ship_a,
                type_id: dread,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 100.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 500.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await;
        handle_crab_component_call(&ctx, &sess_a, lowsec, 94_300_010, ship_a, b_a, "linkWithShip", "InitiateLink", &[])
            .await
            .unwrap();
        sys_h.try_update_combat_state(ship_a, |cs| cs.structure_hp = 0.0);
        tick_crab_beacons_in_system(&ctx, lowsec).await;
        assert_eq!(read_crab_beacons(&ctx, |b| b.get(&b_a).unwrap().phase), CrabBeaconPhase::Idle);

        // Also test tick_all_crab_beacons + System interference exact boundaries 0.35 / 0.70 & decay via clock
        let now = ctx.now_unix();
        assert_eq!(
            get_system_interference_level_decayed(&ctx, lowsec, now + (INTERFERENCE_DECAY_WINDOW_SECS as u64) + 10),
            0.0
        );
        tick_all_crab_beacons(&ctx);
    }

    #[tokio::test]
    async fn batch4_crab_ops_interference_decay_and_primary_wave_types() {
        let mut sde = (*shared_sde()).clone();
        let lowsec = find_lowsec(&sde);
        let carrier = find_ship(&sde, 547);
        let tpl = sde.types.values().next().unwrap().clone();
        // Strip existing 4037/1964/60476/60477 so we control exact counts (10 primary, 2 mutaplasmids, 801 fallback)
        sde.types.retain(|&tid, t| {
            t.group_id != 4037
                && t.group_id != 1964
                && !matches!(t.group_id, 801..=805 | 844)
                && tid != 60_476
                && tid != 60_477
        });
        for i in 0..10_u32 {
            let mut d = tpl.clone();
            d.type_id = 9_700_001 + i;
            d.group_id = 4037;
            d.published = true;
            d.name = format!("Primary Rogue Drone {i}");
            sde.types.insert(d.type_id, d);
        }
        let mut d_ignore = tpl.clone();
        d_ignore.type_id = 9_700_011;
        d_ignore.group_id = 4037;
        d_ignore.published = true;
        d_ignore.name = "DO NOT TRANSLATE Drone".into();
        sde.types.insert(d_ignore.type_id, d_ignore);

        for (idx, tid) in [9_700_021_u32, 9_700_022_u32].into_iter().enumerate() {
            let mut m = tpl.clone();
            m.type_id = tid;
            m.group_id = 1964;
            m.published = true;
            m.name = format!("Synthetic Mutaplasmid {idx}");
            sde.types.insert(tid, m);
        }

        let db = Arc::new(WorldRepository::new_empty());
        let clk: Arc<dyn eve_db::ServerClock> =
            Arc::new(VirtualServerClock::with_fixed_base(1_860_000_000));
        db.set_clock(Arc::clone(&clk));
        let ctx = ServerContext::with_sde_and_repository(Arc::new(sde.clone()), db)
            .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED);
        ctx.set_clock(clk);

        // 1. Apply interference delta -> advance half decay window -> verify exact decay + band arithmetic (lines 185-187)
        let mut sess_int = SessionState::new(94_300_201);
        record_system_interference_activity(&ctx, &mut sess_int, lowsec, 0.80);
        let rec1 = with_pve_special_reg(&ctx, |r| r.interference_by_system[&lowsec].clone());
        assert!((rec1.interference_level - 0.80).abs() < 1e-9);
        assert_eq!(rec1.band, 3);

        let dt = (INTERFERENCE_DECAY_WINDOW_SECS as u64) / 2;
        ctx.advance_secs(dt);
        record_system_interference_activity(&ctx, &mut sess_int, lowsec, 0.0);
        let rec2 = with_pve_special_reg(&ctx, |r| r.interference_by_system[&lowsec].clone());
        let expected_decay = (0.80_f64 - (dt as f64) / INTERFERENCE_DECAY_WINDOW_SECS).max(0.0);
        assert!(
            (rec2.interference_level - expected_decay).abs() < 1e-6,
            "expected decayed {}, got {}",
            expected_decay,
            rec2.interference_level
        );
        assert_eq!(rec2.band, 1, "decayed to ~0.30 -> band 1 (< 0.35)");

        // Clear interference for linking
        with_pve_special_reg(&ctx, |r| {
            r.interference_by_system.get_mut(&lowsec).unwrap().interference_level = 0.0;
        });

        // 2. Full 3-wave Carrier CRAB run with 10 primary group_id=4037 drones (lines 36-41) + 2 group_id=1964 mutaplasmids (lines 103-105)
        let (ship_id, sess) = spawn_pilot(&ctx, 94_300_201, lowsec, carrier, DVec3::ZERO, None);
        let b_id = 950_201_i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                b_id,
                InventoryItemRecord {
                    item_id: b_id,
                    type_id: TYPE_CARRIER_CRAB_BEACON,
                    owner_id: 94_300_201,
                    location_id: i64::from(lowsec),
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.item_positions.insert(b_id, [0.0, 0.0, 0.0]);
        });
        register_crab_beacon(&ctx, b_id, TYPE_CARRIER_CRAB_BEACON, 94_300_201, lowsec, DVec3::ZERO);
        handle_crab_component_call(&ctx, &sess, lowsec, 94_300_201, ship_id, b_id, "linkWithShip", "InitiateLink", &[])
            .await
            .unwrap();
        ctx.advance_secs(CRAB_LINK_DURATION_SECS);

        let sys_h = ctx.systems.get_or_spawn(lowsec);
        let w1 = read_crab_beacons(&ctx, |b| b.get(&b_id).unwrap().wave_npc_ball_ids.clone());
        let mut w1_tids = Vec::new();
        for &id in &w1 {
            if let Ok(Some((_, b))) = sys_h.get_ball_snapshot(id).await {
                w1_tids.push(b.type_id);
            }
        }
        assert_eq!(w1_tids, vec![9_700_001, 9_700_002, 9_700_003, 9_700_004]);

        for &bid in &w1 {
            let _ = sys_h.remove_ball(bid).await;
            on_crab_wave_npc_destroyed(&ctx.systems, &ctx.db, &ctx.sde, lowsec, bid).await;
        }
        let w2 = read_crab_beacons(&ctx, |b| b.get(&b_id).unwrap().wave_npc_ball_ids.clone());
        let mut w2_tids = Vec::new();
        for &id in &w2 {
            if let Ok(Some((_, b))) = sys_h.get_ball_snapshot(id).await {
                w2_tids.push(b.type_id);
            }
        }
        assert_eq!(w2_tids, vec![9_700_004, 9_700_005, 9_700_006, 9_700_007, 9_700_008]);

        for &bid in &w2 {
            let _ = sys_h.remove_ball(bid).await;
            on_crab_wave_npc_destroyed(&ctx.systems, &ctx.db, &ctx.sde, lowsec, bid).await;
        }
        let w3 = read_crab_beacons(&ctx, |b| b.get(&b_id).unwrap().wave_npc_ball_ids.clone());
        let mut w3_tids = Vec::new();
        for &id in &w3 {
            if let Ok(Some((_, b))) = sys_h.get_ball_snapshot(id).await {
                w3_tids.push(b.type_id);
            }
        }
        assert_eq!(w3_tids, vec![9_700_008, 9_700_009, 9_700_010, 9_700_001]);

        for &bid in &w3 {
            let _ = sys_h.remove_ball(bid).await;
            on_crab_wave_npc_destroyed(&ctx.systems, &ctx.db, &ctx.sde, lowsec, bid).await;
        }
        ctx.advance_secs(CRAB_SCAN_DURATION_SECS + 5);
        tick_crab_beacons_in_system(&ctx, lowsec).await;
        let cargo_tids: Vec<TypeId> = ctx.db.with_snapshot(|snap| {
            snap.items
                .values()
                .filter(|it| it.location_id == b_id)
                .map(|it| it.type_id)
                .collect()
        });
        assert!(cargo_tids.contains(&9_700_021) && cargo_tids.contains(&9_700_022));

        // 3. Fallback group 801 skipping "DO NOT TRANSLATE" when primary < 10 (line 50)
        let mut sde_lt10 = sde;
        sde_lt10.types.remove(&9_700_010); // now 9 primary items (< 10!)
        let mut d801_bad = tpl.clone();
        d801_bad.type_id = 9_700_030;
        d801_bad.group_id = 801;
        d801_bad.published = true;
        d801_bad.name = "DO NOT TRANSLATE Rogue".into();
        sde_lt10.types.insert(d801_bad.type_id, d801_bad);
        let mut d801_ok = tpl;
        d801_ok.type_id = 9_700_031;
        d801_ok.group_id = 801;
        d801_ok.published = true;
        d801_ok.name = "Valid Rogue Drone".into();
        sde_lt10.types.insert(d801_ok.type_id, d801_ok);

        let ctx_fb = make_ctx_with_sde(Arc::new(sde_lt10));
        let (ship_fb, sess_fb) = spawn_pilot(&ctx_fb, 94_300_202, lowsec, carrier, DVec3::ZERO, None);
        let b_fb = 950_202_i64;
        ctx_fb.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                b_fb,
                InventoryItemRecord {
                    item_id: b_fb,
                    type_id: TYPE_CARRIER_CRAB_BEACON,
                    owner_id: 94_300_202,
                    location_id: i64::from(lowsec),
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.item_positions.insert(b_fb, [0.0, 0.0, 0.0]);
        });
        register_crab_beacon(&ctx_fb, b_fb, TYPE_CARRIER_CRAB_BEACON, 94_300_202, lowsec, DVec3::ZERO);
        handle_crab_component_call(&ctx_fb, &sess_fb, lowsec, 94_300_202, ship_fb, b_fb, "linkWithShip", "InitiateLink", &[])
            .await
            .unwrap();
        ctx_fb.advance_secs(CRAB_LINK_DURATION_SECS);
        let w_fb = read_crab_beacons(&ctx_fb, |b| b.get(&b_fb).unwrap().wave_npc_ball_ids.clone());
        let h_fb = ctx_fb.systems.get_or_spawn(lowsec);
        let tid_fb = h_fb.get_ball_snapshot(w_fb[0]).await.unwrap().unwrap().1.type_id;
        assert_eq!(tid_fb, 9_700_031);
    }

    #[tokio::test]
    async fn crab_mutants_loot_access_dbuffs_id_alloc_and_special_sites_gate() {
        let sde = shared_sde();
        let lowsec = find_lowsec(&sde);
        let dread = find_ship(&sde, 485);

        // 1. has_special_sites_state guard in on_server_clock_advanced:
        //    When pve_special_sites == Default::default(), even an expired Idle CRAB beacon is skipped
        //    by on_server_clock_advanced (`!has_special_sites_state` early return).
        let ctx_empty = make_ctx_with_sde(Arc::clone(&sde));
        register_crab_beacon(&ctx_empty, 959_001, TYPE_CRAB_BEACON, 94_400_001, lowsec, DVec3::ZERO);
        with_crab_beacons(&ctx_empty, |b| {
            b.get_mut(&959_001).unwrap().deployed_at_sec = 1;
        });
        on_server_clock_advanced(&ctx_empty, 1);
        assert_eq!(
            read_crab_beacons(&ctx_empty, |b| b.get(&959_001).unwrap().phase),
            CrabBeaconPhase::Idle,
            "default pve_special_sites must short-circuit on_server_clock_advanced"
        );

        //    Non-default pve_special_sites with due storm -> on_server_clock_advanced runs and sets sim_clock_sec
        let initial_now = ctx_empty.now_unix();
        with_pve_special_reg(&ctx_empty, |reg| {
            reg.roaming_storms.push(crate::services::pve_special_sites::RoamingStormRecord {
                storm_id: 77_001,
                weather: crate::services::pve_special_sites::AbyssalWeatherKind::Gamma,
                center_solar_system_id: lowsec,
                strong_core_systems: vec![lowsec],
                weak_periphery_systems: Vec::new(),
                step_count: 0,
                next_move_at_sec: initial_now,
                recent_centers: Vec::new(),
            });
        });
        on_server_clock_advanced(&ctx_empty, 1);
        let (sim_clock_after, storm_next_after) = read_pve_special_reg(&ctx_empty, |reg| {
            (reg.sim_clock_sec, reg.roaming_storms[0].next_move_at_sec)
        });
        assert_eq!(sim_clock_after, initial_now);
        assert!(storm_next_after > initial_now, "non-default pve_special_sites must advance storm");

        // 2. is_crab_beacon_item + ensure_beacon_registered_from_item + GetState / GetLinkState / InitiateLinkQuick
        let ctx = make_ctx_with_sde(Arc::clone(&sde));
        ctx.db.with_mut_snapshot(|s| s.next_entity_id = 1_000_000);
        let (ship_id, sess) = spawn_pilot(&ctx, 94_400_001, lowsec, dread, DVec3::ZERO, None);
        let (_other_ship, _other_sess) = spawn_pilot(&ctx, 94_400_002, lowsec, dread, DVec3::ZERO, None);
        assert!(!is_crab_beacon_item(&ctx, 999_888_777));

        let unregistered_bid = 1_500_001_i64;
        let registered_only_bid = 1_500_002_i64;
        ctx.db.with_mut_snapshot(|s| {
            s.items.insert(
                unregistered_bid,
                InventoryItemRecord {
                    item_id: unregistered_bid,
                    type_id: TYPE_CRAB_BEACON,
                    owner_id: 94_400_001,
                    location_id: i64::from(lowsec),
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            s.item_positions.insert(unregistered_bid, [0.0, 0.0, 0.0]);
        });
        assert!(is_crab_beacon_item(&ctx, unregistered_bid));
        register_crab_beacon(&ctx, registered_only_bid, TYPE_CRAB_BEACON, 94_400_001, lowsec, DVec3::ZERO);
        assert!(is_crab_beacon_item(&ctx, registered_only_bid));

        // Non-existent beacon returns CrabBeaconNotFound; wrong system returns TargetNotInSameSystem
        assert_eq!(
            handle_crab_component_call(&ctx, &sess, lowsec, 94_400_001, ship_id, 999_111, "crabBeacon", "GetState", &[])
                .await
                .unwrap_err()
                .code,
            "CrabBeaconNotFound"
        );
        assert_eq!(
            handle_crab_component_call(&ctx, &sess, lowsec + 1, 94_400_001, ship_id, registered_only_bid, "crabBeacon", "GetState", &[])
                .await
                .unwrap_err()
                .code,
            "TargetNotInSameSystem"
        );

        // GetState auto-registers unregistered item and returns Ok KeyVal
        let st_rep = handle_crab_component_call(&ctx, &sess, lowsec, 94_400_001, ship_id, unregistered_bid, "crabBeacon", "GetState", &[])
            .await
            .unwrap();
        assert_eq!(st_rep.dict_get("beaconID").and_then(|v| v.as_i64()), Some(unregistered_bid));
        assert_eq!(
            check_crab_loot_access(&ctx, unregistered_bid, 94_400_001, false).unwrap_err().code,
            "CrabScanInProgress"
        );
        assert_eq!(check_crab_loot_access(&ctx, 999_111, 94_400_001, false).unwrap(), None);

        // 3. InitiateLinkQuick applies ship dbuffs, combat crab_resonance_multiplier = 0.90, OnDbuffUpdated;
        //    valid linking ship stays Linking mid-link (no spurious revert to Idle).
        let _ = ctx.drain_notifications_for_char(94_400_001);
        handle_crab_component_call(
            &ctx, &sess, lowsec, 94_400_001, ship_id, unregistered_bid, "linkWithShip", "InitiateLinkQuick", &[],
        )
        .await
        .unwrap();
        let sys_h = ctx.systems.get_or_spawn(lowsec);
        assert_eq!(
            sys_h.get_combat_state(ship_id).await.unwrap().unwrap().crab_resonance_multiplier,
            Some(0.90)
        );
        {
            let st = crate::services::inventory_fitting::fitting_scanners_superweapons::scanners_superweapons_state()
                .read()
                .unwrap();
            assert_eq!(st.ship_dbuffs.get(&(ctx.instance_id, ship_id)).map(|v| v.len()), Some(4));
        }
        assert!(
            ctx.drain_notifications_for_char(94_400_001).iter().any(|n| n.event == "OnDbuffUpdated")
        );

        // Mid-link tick keeps phase == Linking when ship stays valid
        ctx.advance_secs(10);
        tick_crab_beacons_in_system(&ctx, lowsec).await;
        assert_eq!(
            read_crab_beacons(&ctx, |b| b.get(&unregistered_bid).unwrap().phase),
            CrabBeaconPhase::Linking
        );
        assert!(is_ship_crab_linked(&ctx, ship_id, ctx.now_unix()));

        // Complete link -> dbuffs cleared, crab_resonance_multiplier restored to None, wave 1 ball IDs > 1_000_000
        let id_before_wave = ctx.db.with_snapshot(|s| s.next_entity_id);
        ctx.advance_secs(CRAB_LINK_DURATION_SECS);
        let w1 = read_crab_beacons(&ctx, |b| {
            let rec = b.get(&unregistered_bid).unwrap();
            assert_eq!(rec.phase, CrabBeaconPhase::Scanning);
            assert!(rec.wave_npc_ball_ids.iter().all(|&bid| bid > id_before_wave));
            rec.wave_npc_ball_ids.clone()
        });
        assert!(ctx.db.with_snapshot(|s| s.next_entity_id) >= id_before_wave + w1.len() as i64);
        assert_eq!(
            sys_h.get_combat_state(ship_id).await.unwrap().unwrap().crab_resonance_multiplier,
            None
        );
        {
            let st = crate::services::inventory_fitting::fitting_scanners_superweapons::scanners_superweapons_state()
                .read()
                .unwrap();
            assert!(st.ship_dbuffs.get(&(ctx.instance_id, ship_id)).is_none_or(|v| v.is_empty()));
        }

        // Destroy waves 1, 2, 3 via on_crab_wave_npc_destroyed, verify strict next_entity_id growth, then finish scan
        for _ in 0..3 {
            let ids = read_crab_beacons(&ctx, |b| b.get(&unregistered_bid).unwrap().wave_npc_ball_ids.clone());
            let prev_id = ctx.db.with_snapshot(|s| s.next_entity_id);
            for bid in ids {
                let _ = sys_h.remove_ball(bid).await;
                on_crab_wave_npc_destroyed(&ctx.systems, &ctx.db, &ctx.sde, lowsec, bid).await;
            }
            assert!(ctx.db.with_snapshot(|s| s.next_entity_id) >= prev_id);
        }
        let id_before_loot = ctx.db.with_snapshot(|s| s.next_entity_id);
        ctx.advance_secs(CRAB_SCAN_DURATION_SECS + 5);
        assert_eq!(
            read_crab_beacons(&ctx, |b| b.get(&unregistered_bid).unwrap().phase),
            CrabBeaconPhase::LootRestricted
        );
        assert!(ctx.db.with_snapshot(|s| s.next_entity_id) > id_before_loot);
        assert!(
            ctx.db.with_snapshot(|s| s.items.values().filter(|it| it.location_id == unregistered_bid).all(|it| it.item_id > id_before_loot))
        );

        // check_crab_loot_access permissions & exact self_destruct boundary
        assert_eq!(check_crab_loot_access(&ctx, unregistered_bid, 94_400_001, false).unwrap(), Some(true));
        assert_eq!(check_crab_loot_access(&ctx, unregistered_bid, 94_400_002, true).unwrap(), Some(true));
        assert_eq!(
            check_crab_loot_access(&ctx, unregistered_bid, 94_400_002, false).unwrap_err().code,
            "LootOwnershipRestricted"
        );
        ctx.advance_secs(CRAB_LOOT_EXCLUSIVE_SECS);
        assert_eq!(
            check_crab_loot_access(&ctx, unregistered_bid, 94_400_001, false).unwrap_err().code,
            "ItemNotFound"
        );
        assert_eq!(
            read_crab_beacons(&ctx, |b| b.get(&unregistered_bid).unwrap().phase),
            CrabBeaconPhase::SelfDestructed
        );
    }
}
