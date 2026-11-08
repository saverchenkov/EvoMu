//! Unit tests for CONCORD Rogue Analysis Beacons (`pve_crab_beacons`).

use super::*;
use crate::router::ServerContext;
use eve_db::clock::VirtualServerClock;
use eve_db::{InventoryFlag, InventoryItemRecord, WorldRepository};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_sde::{SdeRegistry, SolarSystemId, TypeId};
use glam::DVec3;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())).clone()
}

fn build_test_ctx() -> Arc<ServerContext> {
    let db = Arc::new(WorldRepository::new_empty());
    db.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    Arc::new(ServerContext::with_sde_and_repository(shared_sde(), db))
}

fn find_lowsec_system(sde: &SdeRegistry) -> SolarSystemId {
    let mut ids: Vec<SolarSystemId> = sde
        .solar_systems
        .values()
        .filter(|s| {
            s.security_status > 0.0
                && validate_crab_deployment_system(sde, s.solar_system_id).is_ok()
        })
        .map(|s| s.solar_system_id)
        .collect();
    ids.sort_unstable();
    ids[0]
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
    ctx.db.provision_gm_account(username, "secret", char_id, username);
    let uid = ctx
        .db
        .with_snapshot(|snap| snap.characters.get(&char_id).map(|c| c.user_id).unwrap_or(1));
    let now = ctx.now_unix();
    let _ = ctx.db.acquire_online_lock_with_computer_hash(uid, char_id as u64, Some(char_id), None, true, now);

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
    let mut cs = ctx.build_ship_combat_state(char_id, ship_id, ship_type_id);
    cs.is_tethered = true;
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
fn test_crab_phase_type_system_and_ship_eligibility() {
    let ctx = build_test_ctx();
    assert_eq!(CrabBeaconPhase::Idle.as_str(), "idle");
    assert_eq!(CrabBeaconPhase::Linking.as_str(), "linking");
    assert_eq!(CrabBeaconPhase::Scanning.as_str(), "scanning");
    assert_eq!(CrabBeaconPhase::LootRestricted.as_str(), "loot_restricted");
    assert_eq!(CrabBeaconPhase::SelfDestructed.as_str(), "self_destructed");

    assert!(is_crab_beacon_type(&ctx.sde, TYPE_CRAB_BEACON));
    assert!(is_crab_beacon_type(&ctx.sde, TYPE_CARRIER_CRAB_BEACON));
    assert!(!is_crab_beacon_type(&ctx.sde, 587));
    assert!(!is_crab_beacon_type(&ctx.sde, 999_999_999));

    let lowsec = find_lowsec_system(&ctx.sde);
    let nullsec = find_nullsec_system(&ctx.sde);
    assert!(validate_crab_deployment_system(&ctx.sde, lowsec).is_ok());
    assert!(validate_crab_deployment_system(&ctx.sde, nullsec).is_ok());
    assert_eq!(validate_crab_deployment_system(&ctx.sde, 99_999_999).unwrap_err().code, "CantDeployInSystem");
    assert_eq!(validate_crab_deployment_system(&ctx.sde, 30_000_142).unwrap_err().code, "CantDeployInHighSec");
    assert_eq!(validate_crab_deployment_system(&ctx.sde, 31_000_005).unwrap_err().code, "CantDeployInWormhole");

    let dread = find_ship_by_group(&ctx.sde, 485);
    let carrier = find_ship_by_group(&ctx.sde, 547);
    let supercarrier = find_ship_by_group(&ctx.sde, 659);
    let rorqual = find_ship_by_group(&ctx.sde, 883);
    let frigate = find_ship_by_group(&ctx.sde, 25);

    assert!(is_eligible_crab_ship(&ctx.sde, TYPE_CRAB_BEACON, dread));
    assert!(is_eligible_crab_ship(&ctx.sde, TYPE_CRAB_BEACON, carrier));
    assert!(is_eligible_crab_ship(&ctx.sde, TYPE_CRAB_BEACON, supercarrier));
    assert!(is_eligible_crab_ship(&ctx.sde, TYPE_CRAB_BEACON, rorqual));
    assert!(!is_eligible_crab_ship(&ctx.sde, TYPE_CRAB_BEACON, frigate));
    assert!(!is_eligible_crab_ship(&ctx.sde, TYPE_CRAB_BEACON, TYPE_CRAB_BEACON));
    assert!(!is_eligible_crab_ship(&ctx.sde, TYPE_CRAB_BEACON, 999_999_999));

    assert!(is_eligible_crab_ship(&ctx.sde, TYPE_CARRIER_CRAB_BEACON, carrier));
    assert!(!is_eligible_crab_ship(&ctx.sde, TYPE_CARRIER_CRAB_BEACON, dread));
    assert!(!is_eligible_crab_ship(&ctx.sde, TYPE_CARRIER_CRAB_BEACON, frigate));

    assert!(!is_crab_beacon_item(&ctx, 777_001));
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            777_001,
            InventoryItemRecord {
                item_id: 777_001,
                type_id: TYPE_CRAB_BEACON,
                owner_id: 91_000_001,
                location_id: i64::from(lowsec),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    assert!(is_crab_beacon_item(&ctx, 777_001));
    assert!(!is_crab_beacon_item(&ctx, 777_002));
    register_crab_beacon(&ctx, 777_002, TYPE_CRAB_BEACON, 91_000_001, lowsec, DVec3::ZERO);
    assert!(is_crab_beacon_item(&ctx, 777_002));
}

#[tokio::test]
async fn test_crab_link_errors_ceq_and_interference_limits() {
    let ctx = build_test_ctx();
    let lowsec = find_lowsec_system(&ctx.sde);
    let nullsec = find_nullsec_system(&ctx.sde);
    let dread = find_ship_by_group(&ctx.sde, 485);
    let frigate = find_ship_by_group(&ctx.sde, 25);

    let (dread_id, mut sess) = setup_pilot_in_space(&ctx, "crab_err", 91_000_010, lowsec, dread, DVec3::ZERO);

    let err = handle_crab_component_call(&ctx, &sess, lowsec, 91_000_010, dread_id, 880_001, "linkWithShip", "InitiateLink", &[]).await.unwrap_err();
    assert_eq!(err.code, "CrabBeaconNotFound");

    register_crab_beacon(&ctx, 880_002, TYPE_CRAB_BEACON, 91_000_010, nullsec, DVec3::ZERO);
    let err = handle_crab_component_call(&ctx, &sess, lowsec, 91_000_010, dread_id, 880_002, "linkWithShip", "InitiateLink", &[]).await.unwrap_err();
    assert_eq!(err.code, "TargetNotInSameSystem");

    register_crab_beacon(&ctx, 880_003, TYPE_CRAB_BEACON, 91_000_010, 30_000_142, DVec3::ZERO);
    let err = handle_crab_component_call(&ctx, &sess, 30_000_142, 91_000_010, dread_id, 880_003, "linkWithShip", "InitiateLink", &[]).await.unwrap_err();
    assert_eq!(err.code, "CantDeployInHighSec");

    register_crab_beacon(&ctx, 880_004, TYPE_CRAB_BEACON, 91_000_010, lowsec, DVec3::ZERO);
    let err = handle_crab_component_call(&ctx, &sess, lowsec, 91_000_010, dread_id, 880_004, "", "", &[]).await.unwrap_err();
    assert_eq!(err.code, "UnsupportedComponentCall");

    let err = handle_crab_component_call(&ctx, &sess, lowsec, 91_000_010, 999_888_777, 880_004, "linkWithShip", "InitiateLink", &[]).await.unwrap_err();
    assert_eq!(err.code, "ShipNotInSpace");

    let sys_handle = ctx.systems.get_or_spawn(lowsec);
    sys_handle.try_update_combat_state(dread_id, |cs| cs.is_cloaked = true);
    let err = handle_crab_component_call(&ctx, &sess, lowsec, 91_000_010, dread_id, 880_004, "linkWithShip", "InitiateLink", &[]).await.unwrap_err();
    assert_eq!(err.code, "CannotLinkWhileCloaked");
    sys_handle.try_update_combat_state(dread_id, |cs| cs.is_cloaked = false);

    register_crab_beacon(&ctx, 880_005, TYPE_CRAB_BEACON, 91_000_010, lowsec, DVec3::new(25_000.0, 0.0, 0.0));
    let err = handle_crab_component_call(&ctx, &sess, lowsec, 91_000_010, dread_id, 880_005, "linkWithShip", "InitiateLink", &[]).await.unwrap_err();
    assert_eq!(err.code, "OutOfCrabLinkRange");

    let (frig_id, frig_sess) = setup_pilot_in_space(&ctx, "crab_frig", 91_000_011, lowsec, frigate, DVec3::ZERO);
    let err = handle_crab_component_call(&ctx, &frig_sess, lowsec, 91_000_011, frig_id, 880_004, "linkWithShip", "InitiateLink", &[]).await.unwrap_err();
    assert_eq!(err.code, "IneligibleShipForCrabLink");

    let low_const = ctx.sde.solar_systems.get(&lowsec).unwrap().constellation_id;
    with_pve_special_reg(&ctx, |reg| {
        crate::services::pve_incursion_sites::seed_incursion_for_constellation(&ctx, reg, low_const);
    });
    let err = handle_crab_component_call(&ctx, &sess, lowsec, 91_000_010, dread_id, 880_004, "linkWithShip", "InitiateLink", &[]).await.unwrap_err();
    assert_eq!(err.code, "CynoJammed");
    with_pve_special_reg(&ctx, |reg| {
        reg.incursions.remove(&low_const);
    });

    let ok1 = handle_crab_component_call(&ctx, &sess, lowsec, 91_000_010, dread_id, 880_004, "linkWithShip", "InitiateLink", &[]).await.unwrap();
    assert_eq!(ok1.dict_get("ceqRemaining").and_then(|v| v.as_i64()), Some(70));

    register_crab_beacon(&ctx, 880_006, TYPE_CRAB_BEACON, 91_000_010, lowsec, DVec3::ZERO);
    let err = handle_crab_component_call(&ctx, &sess, lowsec, 91_000_010, dread_id, 880_006, "linkWithShip", "InitiateLink", &[]).await.unwrap_err();
    assert_eq!(err.code, "ShipAlreadyLinkedToCrab");

    let (dread2_id, sess2) = setup_pilot_in_space(&ctx, "crab_p2", 91_000_012, lowsec, dread, DVec3::ZERO);
    let err = handle_crab_component_call(&ctx, &sess2, lowsec, 91_000_012, dread2_id, 880_004, "linkWithShip", "InitiateLink", &[]).await.unwrap_err();
    assert_eq!(err.code, "CrabBeaconAlreadyActive");

    ctx.advance_secs(CRAB_LINK_DURATION_SECS);
    for idx in 0..2 {
        let bid = 880_100 + idx;
        register_crab_beacon(&ctx, bid, TYPE_CRAB_BEACON, 91_000_010, lowsec, DVec3::ZERO);
        handle_crab_component_call(&ctx, &sess, lowsec, 91_000_010, dread_id, bid, "linkWithShip", "InitiateLinkQuick", &[]).await.unwrap();
        ctx.advance_secs(CRAB_LINK_DURATION_SECS);
    }
    register_crab_beacon(&ctx, 880_103, TYPE_CRAB_BEACON, 91_000_010, lowsec, DVec3::ZERO);
    let err = handle_crab_component_call(&ctx, &sess, lowsec, 91_000_010, dread_id, 880_103, "linkWithShip", "InitiateLink", &[]).await.unwrap_err();
    assert_eq!(err.code, "InsufficientEnergy");

    for p_idx in 0..3 {
        let cid = 91_000_020 + p_idx;
        let (sid, s) = setup_pilot_in_space(&ctx, &format!("cap_{p_idx}"), cid, lowsec, dread, DVec3::ZERO);
        for k in 0..3 {
            let bid = 880_200 + p_idx * 10 + k;
            register_crab_beacon(&ctx, bid, TYPE_CRAB_BEACON, cid, lowsec, DVec3::ZERO);
            handle_crab_component_call(&ctx, &s, lowsec, cid, sid, bid, "linkWithShip", "InitiateLink", &[]).await.unwrap();
            ctx.advance_secs(CRAB_LINK_DURATION_SECS);
        }
    }
    let (sid_extra, s_extra) = setup_pilot_in_space(&ctx, "cap_extra", 91_000_030, lowsec, dread, DVec3::ZERO);
    register_crab_beacon(&ctx, 880_299, TYPE_CRAB_BEACON, 91_000_030, lowsec, DVec3::ZERO);
    let err = handle_crab_component_call(&ctx, &s_extra, lowsec, 91_000_030, sid_extra, 880_299, "linkWithShip", "InitiateLink", &[]).await.unwrap_err();
    assert_eq!(err.code, "ExcessInterference");
    sess.notifications.clear();
}

#[tokio::test]
async fn test_crab_full_lifecycle_waves_loot_and_self_destruct() {
    let ctx = build_test_ctx();
    let nullsec = find_nullsec_system(&ctx.sde);
    let carrier = find_ship_by_group(&ctx.sde, 547);

    let (ship_id, sess) = setup_pilot_in_space(&ctx, "crab_life", 91_000_040, nullsec, carrier, DVec3::ZERO);

    let (b1, b2, b_idle) = (890_001i64, 890_002i64, 890_003i64);
    ctx.db.with_mut_snapshot(|snap| {
        for (bid, tid, pos) in [
            (b1, TYPE_CRAB_BEACON, [1_000.0, 0.0, 0.0]),
            (b2, TYPE_CARRIER_CRAB_BEACON, [2_000.0, 0.0, 0.0]),
            (b_idle, TYPE_CRAB_BEACON, [3_000.0, 0.0, 0.0]),
        ] {
            snap.items.insert(
                bid,
                InventoryItemRecord {
                    item_id: bid,
                    type_id: tid,
                    owner_id: 91_000_040,
                    location_id: i64::from(nullsec),
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.item_positions.insert(bid, pos);
        }
    });

    let st_idle = handle_crab_component_call(&ctx, &sess, nullsec, 91_000_040, ship_id, b1, "linkWithShip", "GetLinkState", &[]).await.unwrap();
    assert_eq!(st_idle.dict_get("phase").and_then(|v| v.as_str()), Some("idle"));
    assert_eq!(st_idle.dict_get("linked").and_then(|v| v.as_bool()), Some(false));
    assert_eq!(check_crab_loot_access(&ctx, b1, 91_000_040, false).unwrap_err().code, "CrabScanInProgress");
    assert_eq!(check_crab_loot_access(&ctx, 123_456_789, 91_000_040, false).unwrap(), None);

    let res = handle_crab_component_call(&ctx, &sess, nullsec, 91_000_040, ship_id, b1, "linkWithShip", "InitiateLink", &[]).await.unwrap();
    assert_eq!(res.dict_get("linked").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(res.dict_get("resistanceBonusPct").and_then(|v| v.as_f64()), Some(10.0));
    let now = ctx.now_unix();
    assert!(is_ship_crab_linked(&ctx, ship_id, now));
    assert!(is_ship_crab_linked(&ctx, ship_id, now + CRAB_LINK_DURATION_SECS - 1));

    let sys_handle = ctx.systems.get_or_spawn(nullsec);
    let cs_linked = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(!cs_linked.is_tethered);
    let notifs = ctx.drain_notifications_for_char(91_000_040);
    assert!(notifs.iter().any(|n| n.event == "OnDbuffUpdated"));
    assert!(notifs.iter().any(|n| n.event == "OnSlimItemChange"));

    ctx.advance_secs(100);
    let st_link = handle_crab_component_call(&ctx, &sess, nullsec, 91_000_040, ship_id, b1, "crabBeacon", "GetState", &[]).await.unwrap();
    assert_eq!(
        st_link.dict_get("linkRemainingSeconds").and_then(|v| v.as_i64()),
        Some((CRAB_LINK_DURATION_SECS - 100) as i64)
    );

    ctx.advance_secs(CRAB_LINK_DURATION_SECS - 100);
    assert!(!is_ship_crab_linked(&ctx, ship_id, ctx.now_unix()));
    let w1_balls = read_crab_beacons(&ctx, |beacons| {
        let b = beacons.get(&b1).unwrap();
        assert_eq!(b.phase, CrabBeaconPhase::Scanning);
        assert_eq!(b.waves_spawned, 1);
        b.wave_npc_ball_ids.clone()
    });
    assert_eq!(w1_balls.len(), WAVE_1_DRONES.len());

    for bid in w1_balls {
        let _ = sys_handle.remove_ball(bid).await;
        on_crab_wave_npc_destroyed(&ctx.systems, &ctx.db, &ctx.sde, nullsec, bid).await;
    }
    let w2_balls = read_crab_beacons(&ctx, |beacons| {
        let b = beacons.get(&b1).unwrap();
        assert_eq!(b.waves_spawned, 2);
        b.wave_npc_ball_ids.clone()
    });
    assert_eq!(w2_balls.len(), WAVE_2_DRONES.len());

    ctx.advance_secs(400);
    let all_balls = read_crab_beacons(&ctx, |beacons| {
        let b = beacons.get(&b1).unwrap();
        assert_eq!(b.waves_spawned, 3);
        b.wave_npc_ball_ids.clone()
    });
    for bid in &all_balls {
        sys_handle.try_update_combat_state(*bid, |cs| cs.structure_hp = 0.0);
    }

    ctx.advance_secs(200);
    let b1_rec = read_crab_beacons(&ctx, |beacons| beacons.get(&b1).cloned().unwrap());
    assert_eq!(b1_rec.phase, CrabBeaconPhase::LootRestricted);

    assert_eq!(check_crab_loot_access(&ctx, b1, 91_000_040, false).unwrap(), Some(true));
    assert_eq!(check_crab_loot_access(&ctx, b1, 99_999_999, true).unwrap(), Some(true));
    assert_eq!(check_crab_loot_access(&ctx, b1, 91_000_999, false).unwrap_err().code, "LootOwnershipRestricted");

    let loot_in_b1: Vec<(TypeId, i32)> = ctx.db.with_snapshot(|snap| {
        let mut v: Vec<(TypeId, i32)> = snap.items.values().filter(|i| i.location_id == b1).map(|i| (i.type_id, i.quantity)).collect();
        v.sort_unstable();
        v
    });
    assert_eq!(
        loot_in_b1,
        vec![
            (TYPE_ROGUE_DRONE_INFESTATION_DATA, 15),
            (TYPE_RADICAL_DRONE_DDA_MUTA, 1),
            (TYPE_RADICAL_FIGHTER_FSU_MUTA, 1),
        ]
    );

    ctx.advance_secs(CRAB_LOOT_EXCLUSIVE_SECS);
    let b1_after = read_crab_beacons(&ctx, |beacons| beacons.get(&b1).cloned().unwrap());
    assert_eq!(b1_after.phase, CrabBeaconPhase::SelfDestructed);
    let can_id = b1_after.open_container_id.expect("open container spawned");
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&b1));
        let can = snap.items.get(&can_id).unwrap();
        assert_eq!(can.type_id, 23);
        assert_eq!(can.owner_id, 0);
        let moved: usize = snap.items.values().filter(|i| i.location_id == can_id && i.owner_id == 0).count();
        assert_eq!(moved, 3);
    });
    assert_eq!(check_crab_loot_access(&ctx, b1, 91_000_040, false).unwrap_err().code, "ItemNotFound");

    handle_crab_component_call(&ctx, &sess, nullsec, 91_000_040, ship_id, b2, "linkWithShip", "InitiateLink", &[]).await.unwrap();
    ctx.advance_secs(CRAB_LINK_DURATION_SECS + CRAB_SCAN_DURATION_SECS);
    let b2_drones = read_crab_beacons(&ctx, |beacons| beacons.get(&b2).unwrap().wave_npc_ball_ids.clone());
    for bid in b2_drones {
        sys_handle.try_update_combat_state(bid, |cs| cs.structure_hp = 0.0);
    }
    tick_crab_beacons_in_system(&ctx, nullsec).await;
    let b2_rec = read_crab_beacons(&ctx, |beacons| beacons.get(&b2).cloned().unwrap());
    assert_eq!(b2_rec.phase, CrabBeaconPhase::LootRestricted);
    ctx.db.with_mut_snapshot(|snap| {
        let data_qty = snap.items.values().find(|i| i.location_id == b2 && i.type_id == TYPE_ROGUE_DRONE_INFESTATION_DATA).map(|i| i.quantity);
        assert_eq!(data_qty, Some(10));
        snap.items.retain(|_, i| i.location_id != b2);
    });
    ctx.advance_secs(CRAB_LOOT_EXCLUSIVE_SECS);
    let b2_after = read_crab_beacons(&ctx, |beacons| beacons.get(&b2).cloned().unwrap());
    assert_eq!(b2_after.phase, CrabBeaconPhase::SelfDestructed);
    assert_eq!(b2_after.open_container_id, None);

    ensure_beacon_registered_from_item(&ctx, nullsec, b_idle).unwrap();
    ctx.advance_secs(CRAB_BEACON_IDLE_LIFETIME_SECS);
    let idle_after = read_crab_beacons(&ctx, |beacons| beacons.get(&b_idle).cloned().unwrap());
    assert_eq!(idle_after.phase, CrabBeaconPhase::SelfDestructed);
}

#[test]
fn test_crab_interference_bands_and_decay() {
    let ctx = build_test_ctx();
    let lowsec = find_lowsec_system(&ctx.sde);
    let mut sess = SessionState::new(77);
    let now = ctx.now_unix();

    assert_eq!(get_system_interference_level_decayed(&ctx, lowsec, now), 0.0);

    record_system_interference_activity(&ctx, &mut sess, lowsec, 0.40);
    assert_eq!(sess.notifications.len(), 1);
    assert_eq!(sess.notifications[0].0, "OnSolarsystemInterferenceChanged");
    assert_eq!(read_pve_special_reg(&ctx, |reg| reg.interference_by_system.get(&lowsec).unwrap().band), 2);

    record_system_interference_activity(&ctx, &mut sess, lowsec, 0.40);
    assert_eq!(read_pve_special_reg(&ctx, |reg| reg.interference_by_system.get(&lowsec).unwrap().band), 3);

    let half_window = (INTERFERENCE_DECAY_WINDOW_SECS / 2.0) as u64;
    let decayed = get_system_interference_level_decayed(&ctx, lowsec, now + half_window);
    assert!((decayed - 0.30).abs() < 1e-6);
    assert_eq!(read_pve_special_reg(&ctx, |reg| reg.interference_by_system.get(&lowsec).unwrap().band), 1);

    let zeroed = get_system_interference_level_decayed(&ctx, lowsec, now + (INTERFERENCE_DECAY_WINDOW_SECS as u64));
    assert_eq!(zeroed, 0.0);
}
