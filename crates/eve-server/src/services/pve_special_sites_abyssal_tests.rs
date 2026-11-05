//! Unit and integration tests for `pve_special_sites_abyssal`.

use super::*;
use crate::router::ServerContext;
use eve_db::clock::VirtualServerClock;
use eve_db::{InventoryFlag, InventoryItemRecord, WorldRepository};
use eve_destiny::{Ball, Ballpark, MovementMode};
use eve_machonet::SessionState;
use eve_sde::{SdeRegistry, SolarSystemId};
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

#[test]
fn abyssal_filament_classification_mutaplasmid_and_gate_types() {
    assert_eq!(classify_abyssal_filament(56131), Some((0, AbyssalWeatherKind::Electrical)));
    assert_eq!(classify_abyssal_filament(56132), Some((0, AbyssalWeatherKind::Dark)));
    assert_eq!(classify_abyssal_filament(56133), Some((0, AbyssalWeatherKind::Exotic)));
    assert_eq!(classify_abyssal_filament(999_999_999), None);

    assert_eq!(abyssal_mutaplasmid_for_tier(0), 47_699);
    assert_eq!(abyssal_mutaplasmid_for_tier(3), 47_701);
    assert_eq!(abyssal_mutaplasmid_for_tier(6), 47_700);

    assert_eq!(abyssal_room_gate_type(1), 47685);
    assert_eq!(abyssal_room_gate_type(2), 47685);
    assert_eq!(abyssal_room_gate_type(3), 47686);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn abyssal_pocket_entry_conduit_unlock_cache_wreck_and_filament_deploy() {
    let origin_sys: SolarSystemId = 30_000_142;
    let char_id: i64 = 90_050_001;
    let ship_id: i64 = 1_050_000_001;
    let filament_id: i64 = 1_050_000_501;

    let sde = shared_sde();
    let cruiser_tid = sde
        .types
        .values()
        .find(|t| t.group_id == 26)
        .map(|t| t.type_id)
        .expect("SDE has Cruiser ship type");
    let db = Arc::new(WorldRepository::new_empty());
    db.provision_gm_account("ab_pilot", "pw", char_id, "AbyssPilot");
    db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = origin_sys;
            ch.station_id = None;
            ch.active_ship_id = ship_id;
        }
        snap.fleets.insert(
            7702,
            eve_db::types::FleetRecord {
                fleet_id: 7702,
                boss_id: char_id,
                members: vec![char_id, char_id + 1, char_id + 2],
                wings: Default::default(),
                squads: Default::default(),
                free_move: true,
                is_registered: false,
                ..Default::default()
            },
        );
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: cruiser_tid,
                owner_id: char_id,
                location_id: origin_sys as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            filament_id,
            InventoryItemRecord {
                item_id: filament_id,
                type_id: 56131,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 2,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_600_000)));
    let origin_handle = ctx.systems.get_or_spawn(origin_sys);
    let _ = origin_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: cruiser_tid,
            position: DVec3::new(50_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_100_000.0,
            radius: 35.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    let mut session = SessionState::new(501);
    session.user_id = Some(1);
    session.character_id = Some(char_id);
    session.solar_system_id = Some(origin_sys);
    session.ship_id = Some(ship_id);
    session.fleet_id = Some(7702);

    // Deploy Abyssal filament trace in origin system
    let dep = handle_abyssal_entrance_deployment(&ctx, &mut session, &[PyRep::Integer(filament_id)])
        .await
        .expect("deployment succeeds");
    assert!(!matches!(dep, PyRep::None));

    // Enter Abyssal pocket and populate room balls
    let (pocket_id, npcs) = enter_abyssal_pocket_for_character(&ctx, &mut session, char_id, 56131, 0, origin_sys).await;
    assert!(pocket_id > 0);
    assert!(!npcs.is_empty());
    ensure_abyssal_room_balls_for_ctx(&ctx, char_id).await;
    let offset = pocket_grid_offset(&ctx, pocket_id);
    assert_eq!(offset.len(), 3);

    let ab_sys = with_pve_special_reg(&ctx, |reg| {
        reg.abyssal_pockets[&char_id].abyssal_solar_system_id
    });

    // Clear all NPCs in room 0 and unlock conduit via core hook
    let ab_handle = ctx.systems.get_or_spawn(ab_sys);
    for &nid in &npcs[..npcs.len().saturating_sub(1)] {
        let _ = ab_handle.remove_ball(nid).await;
    }
    let last_npc = *npcs.last().unwrap();
    let _ = ab_handle.remove_ball(last_npc).await;
    assert!(on_abyssal_room_npc_destroyed_core(&ctx.systems, &ctx.db, ab_sys, last_npc).await);

    // Ballpark helper returns false for K-space and false when already unlocked
    let mut bp = Ballpark::new(origin_sys);
    assert!(!on_abyssal_room_npc_destroyed_in_ballpark(&mut bp, &ctx.db, origin_sys, last_npc));

    // Cache destruction converts Bioadaptive Cache item into Bioadaptive Cache Wreck ball
    let cache_id: i64 = 1_050_000_999;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            cache_id,
            InventoryItemRecord {
                item_id: cache_id,
                type_id: BIOADAPTIVE_CACHE_TYPE_ID,
                owner_id: 500_026,
                location_id: ab_sys as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.item_positions.insert(cache_id, [100.0, 200.0, 300.0]);
    });
    on_abyssal_cache_destroyed_core(&ctx.systems, &ctx.db, ab_sys, cache_id, Some(char_id)).await;
    assert!(ctx.db.with_snapshot(|snap| !snap.items.contains_key(&cache_id)));
}
