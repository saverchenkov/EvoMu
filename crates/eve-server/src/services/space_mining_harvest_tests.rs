//! Unit tests for `space_mining_harvest` (mining ledger, ice belt respawn, WH ore & guard sites, phased fields).

#![forbid(unsafe_code)]

use super::*;
use crate::router::ServerContext;
use eve_db::{AsteroidRecord, CosmicAnomalyRecord, InventoryFlag, InventoryItemRecord};
use eve_sde::{InvType, RandomizedTypeMaterialEntry, SdeRegistry, TypeMaterialEntry};
use glam::DVec3;
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

fn ensure_type(sde: &mut Arc<SdeRegistry>, tid: u32, group_id: u32, volume: f64, radius: f64) {
    let reg = Arc::make_mut(sde);
    if reg.types.contains_key(&tid) {
        return;
    }
    let mut rec: InvType = reg.types.values().next().cloned().expect("base type");
    rec.type_id = tid;
    rec.group_id = group_id;
    rec.category_id = 25;
    rec.name = format!("MiningTestType_{tid}");
    rec.volume = volume;
    rec.radius = radius;
    rec.portion_size = 100;
    reg.types.insert(tid, rec);
}

#[test]
fn mining_contribution_method_maps_ice_gas_moon_and_standard_ore() {
    assert_eq!(mining_contribution_method(465), "MineIce");
    assert_eq!(mining_contribution_method(711), "MineGas");
    assert_eq!(mining_contribution_method(1884), "MineMoon");
    assert_eq!(mining_contribution_method(1920), "MineMoon");
    assert_eq!(mining_contribution_method(1923), "MineMoon");
    assert_eq!(mining_contribution_method(462), "MineAsteroid");
    assert_eq!(mining_contribution_method(0), "MineAsteroid");
}

#[test]
fn deposit_mined_ore_records_personal_and_observer_ledgers_and_zero_units_noop() {
    let mut ctx = test_ctx();
    let sys_id = 30_000_142;
    let char_id = 90_000_001;
    let ship_id = 1_090_000_001;
    let ore_tid = 996_001;
    let asteroid_id = 1_090_000_050;
    let refinery_id = 1_090_000_777;

    ensure_type(&mut ctx.sde, ore_tid, 1884, 10.0, 50.0);
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            refinery_id,
            eve_db::UpwellStructureRecord {
                structure_id: refinery_id,
                type_id: 35835,
                owner_corp_id: 1_000_009,
                solar_system_id: sys_id,
                name: "Moon Observer Athanor".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 500,
                ..Default::default()
            },
        );
        snap.items.insert(
            1_090_000_778,
            InventoryItemRecord {
                item_id: 1_090_000_778,
                type_id: 45009,
                owner_id: 1_000_009,
                location_id: refinery_id,
                flag: InventoryFlag::ServiceSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(1_090_000_778, true);
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 606,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.asteroids.insert(
            asteroid_id,
            AsteroidRecord {
                asteroid_id,
                type_id: ore_tid,
                solar_system_id: sys_id,
                belt_id: refinery_id,
                quantity: 5_000,
                initial_quantity: 5_000,
                position: (0.0, 0.0, 0.0),
                radius: 50.0,
            },
        );
    });

    deposit_mined_ore_for_asteroid_and_record_ledger_with_waste(
        &ctx,
        char_id,
        ship_id,
        sys_id,
        InventoryFlag::Cargo,
        ore_tid,
        0,
        0,
        Some(asteroid_id),
        Some([0.0, 0.0, 0.0]),
    );
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.observer_mining_ledger.contains_key(&refinery_id));
    });

    let uid = ctx.db.with_snapshot(|s| s.characters.get(&char_id).map(|c| c.user_id).unwrap_or(1));
    let _ = ctx.db.acquire_online_lock(uid, 501, Some(char_id), false, ctx.now_unix());
    let _ = ctx.drain_notifications_for_char(char_id);
    deposit_mined_ore_for_asteroid_and_record_ledger_with_waste(
        &ctx,
        char_id,
        ship_id,
        sys_id,
        InventoryFlag::Cargo,
        ore_tid,
        0,
        0,
        Some(asteroid_id),
        Some([0.0, 0.0, 0.0]),
    );
    assert!(ctx.drain_notifications_for_char(char_id).iter().all(|n| n.event != "OnItemChange"));
    deposit_mined_ore_for_asteroid_and_record_ledger_with_waste(
        &ctx,
        char_id,
        ship_id,
        sys_id,
        InventoryFlag::Cargo,
        ore_tid,
        120,
        15,
        Some(asteroid_id),
        Some([0.0, 0.0, 0.0]),
    );
    let notes_new = ctx.drain_notifications_for_char(char_id);
    let chg_new = notes_new
        .iter()
        .find(|n| n.event == "OnItemChange")
        .expect("deposit_mined_ore must emit OnItemChange for new stack");
    assert!(matches!(&chg_new.args[1], eve_proto::PyRep::Dict(d) if d.contains(&(eve_proto::PyRep::Integer(3), eve_proto::PyRep::Integer(0))) && !d.iter().any(|(k, _)| k == &eve_proto::PyRep::Integer(9))));

    deposit_mined_ore_for_asteroid_and_record_ledger_with_waste(
        &ctx,
        char_id,
        ship_id,
        sys_id,
        InventoryFlag::Cargo,
        ore_tid,
        30,
        5,
        Some(asteroid_id),
        Some([0.0, 0.0, 0.0]),
    );
    let notes_stack = ctx.drain_notifications_for_char(char_id);
    let chg_stack = notes_stack
        .iter()
        .find(|n| n.event == "OnItemChange")
        .expect("deposit_mined_ore must emit OnItemChange for stacked ore");
    assert!(matches!(&chg_stack.args[1], eve_proto::PyRep::Dict(d) if d.contains(&(eve_proto::PyRep::Integer(9), eve_proto::PyRep::Integer(120))) && !d.iter().any(|(k, _)| k == &eve_proto::PyRep::Integer(3))));

    ctx.db.with_snapshot(|snap| {
        let cargo_qty: i64 = snap
            .items
            .values()
            .filter(|i| i.location_id == ship_id && i.type_id == ore_tid)
            .map(|i| i.quantity as i64)
            .sum();
        assert_eq!(cargo_qty, 150);
        let obs = snap.observer_mining_ledger.get(&refinery_id).expect("observer ledger entry");
        assert_eq!(obs.len(), 2);
        assert_eq!(obs[0].quantity, 120);
        assert_eq!(obs[1].quantity, 30);
        assert_eq!(obs[0].character_id, char_id);
        assert!(snap
            .mining_ledger
            .iter()
            .any(|e| e.character_id == char_id && e.type_id == ore_tid && e.quantity == 120));
    });
    let with_waste = ctx.db.get_mining_ledger_with_waste(char_id);
    assert!(with_waste.iter().any(|(e, w)| e.type_id == ore_tid && e.quantity == 120 && *w == 15));
}

#[tokio::test]
async fn ice_belt_respawn_lifecycle_seeds_clears_waits_and_respawns_after_delay() {
    let mut ctx = test_ctx();
    let non_ice_sys = 30_000_142;
    tick_ice_belt_respawn_sync(&ctx, non_ice_sys);
    ctx.db.with_snapshot(|snap| {
        assert!(!snap
            .cosmic_anomalies
            .get(&non_ice_sys)
            .is_some_and(|v| v.iter().any(|a| a.anomaly_type == "IceBelt")));
    });

    let ice_sys = 30_000_180;
    if let Some(Ok(site)) = crate::services::mining_sites::resolve_regional_ice_belt(&ctx.sde, ice_sys) {
        for a in &site.asteroids {
            ensure_type(&mut ctx.sde, a.type_id, 465, 1_000.0, 120.0);
        }
    }

    tick_ice_belt_respawn_sync(&ctx, ice_sys);
    let first_anom_id = ctx.db.with_snapshot(|snap| {
        let anoms = snap.cosmic_anomalies.get(&ice_sys).expect("ice belt seeded");
        let ice = anoms.iter().find(|a| a.anomaly_type == "IceBelt" && !a.cleared).expect("uncleared ice");
        assert!(!snap.asteroids.is_empty());
        ice.anomaly_id.clone()
    });

    // Deplete all asteroids in that system and verify anomaly marks cleared + signature collapsed
    ctx.db.with_mut_snapshot(|snap| {
        for ast in snap.asteroids.values_mut() {
            if ast.solar_system_id == ice_sys {
                ast.quantity = 0;
            }
        }
    });
    tick_all_ice_belt_respawns_sync(&ctx);
    tick_ice_belt_respawn_sync(&ctx, ice_sys);
    ctx.db.with_snapshot(|snap| {
        let ice = snap.cosmic_anomalies[&ice_sys]
            .iter()
            .find(|a| a.anomaly_id == first_anom_id)
            .expect("cleared anomaly retained");
        assert!(ice.cleared);
        assert_eq!(ice.remaining_volume, 0.0);
        let sig = snap.cosmic_signatures[&ice_sys]
            .iter()
            .find(|s| s.signature_id == first_anom_id)
            .expect("signature collapsed");
        assert!(sig.collapsed);
    });

    // Before 4h elapses, calling tick_all_ice_belt_respawns_sync does not respawn
    tick_all_ice_belt_respawns_sync(&ctx);
    ctx.db.with_snapshot(|snap| {
        assert!(snap.cosmic_anomalies[&ice_sys].iter().all(|a| a.cleared));
    });

    // Backdate created_unix beyond ICE_BELT_RESPAWN_DELAY_SECS and verify fresh respawn
    ctx.db.with_mut_snapshot(|snap| {
        for a in snap.cosmic_anomalies.get_mut(&ice_sys).unwrap() {
            if a.anomaly_type == "IceBelt" {
                a.created_unix = a
                    .created_unix
                    .saturating_sub(crate::services::mining_sites::ICE_BELT_RESPAWN_DELAY_SECS + 10);
            }
        }
    });
    tick_all_ice_belt_respawns_sync(&ctx);
    ctx.db.with_snapshot(|snap| {
        let new_ice = snap.cosmic_anomalies[&ice_sys]
            .iter()
            .find(|a| a.anomaly_type == "IceBelt" && !a.cleared)
            .expect("respawned ice belt");
        assert_ne!(new_ice.anomaly_id, first_anom_id);
        assert!(new_ice.remaining_volume > 0.0);
    });
}

#[test]
fn wormhole_ore_sites_sampling_guard_waves_and_persistence_hydration() {
    let mut ctx = test_ctx();
    let c1 = wormhole_ore_sites_for_class(1);
    assert_eq!(c1.len(), 4);
    let c5 = wormhole_ore_sites_for_class(5);
    assert_eq!(c5.len(), 10);
    let c13 = wormhole_ore_sites_for_class(13);
    assert!(c13.iter().any(|d| d.dungeon_name == "Shattered Debris Field"));
    assert!(!c13.iter().any(|d| d.dungeon_name == "Isolated Core Deposit"));
    let c12 = wormhole_ore_sites_for_class(12);
    assert!(c12.iter().any(|d| d.dungeon_name == "Shattered Debris Field"));
    let fallback = wormhole_ore_sites_for_class(99);
    assert_eq!(fallback.len(), 4);

    let sampled_shattered = sample_wormhole_ore_sites_for_system(&ctx.sde, &ctx.mechanic_rng, 31_000_005, 13);
    assert_eq!(sampled_shattered.first().map(|d| d.dungeon_name), Some("Shattered Debris Field"));
    let sampled_c5 = sample_wormhole_ore_sites_for_system(&ctx.sde, &ctx.mechanic_rng, 31_002_001, 5);
    assert_eq!(sampled_c5.len(), 4);

    let sys_id = 31_000_005;
    for &tid in WORMHOLE_GUARD_TYPE_IDS {
        ensure_type(&mut ctx.sde, tid, 551, 1_000_000.0, 40.0);
    }

    let def = WORMHOLE_ORE_SITES[0];
    let mut out_waves = Vec::new();
    ctx.db.with_mut_snapshot(|snap| {
        snap.cosmic_anomalies.entry(sys_id).or_default().push(CosmicAnomalyRecord {
            anomaly_id: "WH-ANOM-1".into(),
            solar_system_id: sys_id,
            name: def.dungeon_name.into(),
            anomaly_type: "OreAnomaly".into(),
            position: [10_000.0, 0.0, 0.0],
            signal_strength: 1.0,
            ore_types: vec![46678],
            remaining_volume: 1_000.0,
            created_unix: 1_000,
            lifetime_seconds: 86_400,
            cleared: false,
        });
        record_initial_wormhole_guard_wave(snap, "WH-ANOM-1", [10_000.0, 0.0, 0.0], def.guards, &mut out_waves);
    });
    assert_eq!(out_waves.len(), 5);
    let persisted = ctx.db.with_snapshot(|snap| collect_persisted_wormhole_guard_sites(snap, sys_id));
    assert_eq!(persisted.len(), 1);
    assert_eq!(persisted[0].1.dungeon_name, def.dungeon_name);

    let destroyed_id = out_waves[0].0;
    on_wormhole_ore_guard_destroyed_db(&ctx.db, destroyed_id);
    on_wormhole_ore_guard_destroyed_db(&ctx.db, 999_999_999);

    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    rt.block_on(async {
        respawn_wormhole_ore_guards_if_missing(&ctx, sys_id, &[]).await;
        let balls = ctx.systems.get_or_spawn(sys_id).list_balls().await.unwrap();
        assert_eq!(out_waves[1..].iter().filter(|(id, _, _)| balls.iter().any(|b| b.id == *id)).count(), 4);
        assert!(!balls.iter().any(|b| b.id == destroyed_id));

        let extra_pos = DVec3::new(50_000.0, 0.0, 0.0);
        spawn_seeded_site_entities(
            &ctx,
            sys_id,
            vec![(9_900_001, 46678, 500, 16.0, 60.0, extra_pos)],
            vec![(9_900_002, WORMHOLE_GUARD_TYPE_IDS[0], extra_pos)],
        )
        .await;
        let balls2 = ctx.systems.get_or_spawn(sys_id).list_balls().await.unwrap();
        assert!(balls2.iter().any(|b| b.id == 9_900_001));
        assert!(balls2.iter().any(|b| b.id == 9_900_002));
    });
}

#[test]
fn randomized_reprocessing_and_phased_asteroid_field_state_transitions() {
    let mut ctx = test_ctx();
    let fixed_tid = 997_001;
    let rand_tid = 997_002;
    let unref_tid = 997_003;
    ensure_type(&mut ctx.sde, fixed_tid, 462, 10.0, 50.0);
    ensure_type(&mut ctx.sde, rand_tid, 462, 10.0, 50.0);
    ensure_type(&mut ctx.sde, unref_tid, 18, 1.0, 10.0);
    {
        let reg = Arc::make_mut(&mut ctx.sde);
        reg.type_materials
            .insert(fixed_tid, vec![TypeMaterialEntry { material_type_id: 34, quantity: 250 }]);
        reg.randomized_type_materials.insert(
            rand_tid,
            vec![
                RandomizedTypeMaterialEntry { material_type_id: 34, quantity_min: 100, quantity_max: 200 },
                RandomizedTypeMaterialEntry { material_type_id: 35, quantity_min: 50, quantity_max: 50 },
            ],
        );
    }

    let fixed_inv = ctx.sde.types.get(&fixed_tid).unwrap();
    let rand_inv = ctx.sde.types.get(&rand_tid).unwrap();
    let unref_inv = ctx.sde.types.get(&unref_tid).unwrap();

    assert!(resolve_reprocessable_materials(&ctx.sde, unref_inv, 1, 100, 7, true).is_none());
    let fixed_mats = resolve_reprocessable_materials_for_portions(&ctx.sde, fixed_inv, 1, 100, 7, 3, false).unwrap();
    assert_eq!(fixed_mats[0].quantity, 750);

    assert_eq!(
        resolve_reprocessable_materials_for_portions(&ctx.sde, rand_inv, 1, 100, 7, 0, true).unwrap().len(),
        0
    );
    let q_mats = resolve_reprocessable_materials_for_portions(&ctx.sde, rand_inv, 2, 100, 7, 4, true).unwrap();
    assert_eq!(q_mats.iter().find(|m| m.material_type_id == 34).unwrap().quantity, 300);
    assert_eq!(q_mats.iter().find(|m| m.material_type_id == 35).unwrap().quantity, 100);

    let scaled_mats = resolve_reprocessable_materials_for_portions(&ctx.sde, rand_inv, 2, 100, 99, 25_000, false).unwrap();
    let total_rand: u64 = scaled_mats.iter().map(|m| m.quantity).sum();
    assert!(total_rand >= 25_000 * 50 && total_rand <= 25_000 * 200);

    let mut field = PhasedAsteroidFieldState::new("PH-001".into(), 30_000_142);
    assert!((field.mining_efficiency() - 0.10).abs() < 1e-9);
    assert!(!field.connect_industrial_command_ship(10, 0));
    assert!(!field.connect_industrial_command_ship(10, 60));
    assert!(!field.connect_industrial_command_ship(10, 80));
    assert!(field.connect_industrial_command_ship(11, 40));
    assert!((field.mining_efficiency() - 1.0).abs() < 1e-9);

    field.advance_fracture(0.45);
    assert!(field.beacon_spawned);
    assert!(!field.collapsed);
    field.advance_fracture(0.5);
    assert!(field.collapsed);
    assert!(!field.phased_in);
    assert!((field.mining_efficiency() - 0.0).abs() < 1e-9);
}

#[test]
fn dispatch_mining_gameplay_events_and_resolve_wormhole_ore_site_coverage() {
    let mut ctx = test_ctx();
    let sys_id = 30_000_142;
    let char_id = 90_000_001;
    let ore_tid = 998_001;
    ensure_type(&mut ctx.sde, ore_tid, 465, 1_000.0, 120.0);

    dispatch_mining_gameplay_events(&ctx, char_id, 0, sys_id, ore_tid);
    dispatch_mining_gameplay_events(&ctx, char_id, 50, sys_id, ore_tid);

    for def in WORMHOLE_ORE_SITES {
        let _ = resolve_wormhole_ore_site(&ctx.sde, def);
        let _ = resolve_wormhole_ore_site_with_dungeon(&ctx.sde, def);
    }
}

#[cfg(test)]
#[path = "space_mining_harvest_extra_unit_tests.rs"]
mod extra_unit_tests;
