//! Deep branch-coverage and mutation-killing unit tests for `pve_special_sites_abyssal`,
//! `pve_abyssal_hazards`, and `pve_abyssal_lifecycle`.

use crate::router::ServerContext;
use crate::services::abyssal_void_gates::{
    handle_abyssal_end_gate_activation, handle_abyssal_gate_activation,
};
use crate::services::abyssal_void_mgr::VOID_SPACE_SYSTEM_ID;
use crate::services::pve_abyssal_hazards::{
    apply_abyssal_weather_dogma_modifiers_to_ship, apply_abyssal_weather_to_combat_and_nav,
    check_abyssal_in_pocket_rpc_restrictions, check_filament_1000km_proximity,
    check_not_in_deadspace_pocket, dark_weather_midpocket_turret_mult,
    evaluate_abyssal_spatial_clouds_for_ship, evaluate_highsec_abyssal_tier_policy,
    is_abyssal_local_channel_disabled, is_abyssal_system, restore_abyssal_weather_dogma_modifiers,
    roll_abyssal_weather_penalty_pct, spatial_cloud_radius_m, validate_abyssal_hull_and_capacity,
    validate_and_consume_abyssal_filaments, BIOLUMINESCENT_CLOUD_LARGE_TYPE_ID,
    BIOLUMINESCENT_CLOUD_SMALL_TYPE_ID, BIOLUMINESCENT_CLOUD_TYPE_ID, FILAMENT_CLOUD_LARGE_TYPE_ID,
    FILAMENT_CLOUD_SMALL_TYPE_ID, FILAMENT_CLOUD_TYPE_ID, PROVING_TACHYON_CLOUD_TYPE_ID,
    TACHYON_CLOUD_TYPE_ID,
};
use crate::services::pve_abyssal_lifecycle::{
    collapse_abyssal_pocket_and_pod_pilot_at, evacuate_abyssal_pockets_for_downtime,
    reconcile_abyssal_login, sweep_expired_void_space_sessions_sync,
};
use crate::services::pve_special_sites::{
    with_pve_special_reg, AbyssalTraceRecord, AbyssalWeatherKind, VoidSpaceSessionRecord,
};
use crate::services::pve_special_sites_abyssal::{
    abyssal_mutaplasmid_for_tier, abyssal_room_gate_type, classify_abyssal_filament,
    enter_abyssal_pocket_for_character, on_abyssal_room_npc_destroyed_core, pocket_grid_offset,
};
use eve_db::clock::VirtualServerClock;
use eve_db::{InventoryFlag, InventoryItemRecord, WorldRepository};
use eve_destiny::{Ball, CombatState, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{SdeRegistry, SolarSystemId, TypeId};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn new_ctx() -> ServerContext {
    let db = Arc::new(WorldRepository::new_empty());
    let ctx = ServerContext::with_sde_and_repository(shared_sde(), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_785_000_000)));
    ctx
}

fn make_item(
    item_id: i64,
    type_id: TypeId,
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
        singleton: quantity == 1 && flag != InventoryFlag::Cargo,
        version: 1,
        ..Default::default()
    }
}

fn make_ball(id: i64, type_id: TypeId, position: DVec3, radius: f64) -> Ball {
    Ball {
        id,
        type_id,
        position,
        velocity: DVec3::ZERO,
        max_velocity: 200.0,
        inertia: 0.6,
        mass: 10_000_000.0,
        radius,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }
}

#[test]
fn deep_classify_filament_all_tiers_mutaplasmid_gate_and_highsec_policy() {
    let cases: &[(TypeId, u8, AbyssalWeatherKind)] = &[
        (56131, 0, AbyssalWeatherKind::Electrical),
        (56132, 0, AbyssalWeatherKind::Dark),
        (56133, 0, AbyssalWeatherKind::Exotic),
        (56134, 0, AbyssalWeatherKind::Firestorm),
        (56136, 0, AbyssalWeatherKind::Gamma),
        (47761, 1, AbyssalWeatherKind::Exotic),
        (47762, 1, AbyssalWeatherKind::Dark),
        (47763, 1, AbyssalWeatherKind::Firestorm),
        (47764, 1, AbyssalWeatherKind::Gamma),
        (47765, 1, AbyssalWeatherKind::Electrical),
        (47888, 2, AbyssalWeatherKind::Exotic),
        (47892, 2, AbyssalWeatherKind::Dark),
        (47896, 2, AbyssalWeatherKind::Firestorm),
        (47900, 2, AbyssalWeatherKind::Gamma),
        (47904, 2, AbyssalWeatherKind::Electrical),
        (47889, 3, AbyssalWeatherKind::Exotic),
        (47893, 3, AbyssalWeatherKind::Dark),
        (47897, 3, AbyssalWeatherKind::Firestorm),
        (47901, 3, AbyssalWeatherKind::Gamma),
        (47905, 3, AbyssalWeatherKind::Electrical),
        (47890, 4, AbyssalWeatherKind::Exotic),
        (47894, 4, AbyssalWeatherKind::Dark),
        (47898, 4, AbyssalWeatherKind::Firestorm),
        (47902, 4, AbyssalWeatherKind::Gamma),
        (47906, 4, AbyssalWeatherKind::Electrical),
        (47891, 5, AbyssalWeatherKind::Exotic),
        (47895, 5, AbyssalWeatherKind::Dark),
        (47899, 5, AbyssalWeatherKind::Firestorm),
        (47903, 5, AbyssalWeatherKind::Gamma),
        (47907, 5, AbyssalWeatherKind::Electrical),
        (56139, 6, AbyssalWeatherKind::Electrical),
        (56140, 6, AbyssalWeatherKind::Dark),
        (56141, 6, AbyssalWeatherKind::Exotic),
        (56142, 6, AbyssalWeatherKind::Firestorm),
        (56143, 6, AbyssalWeatherKind::Gamma),
    ];
    for &(tid, expected_tier, expected_weather) in cases {
        assert_eq!(
            classify_abyssal_filament(tid),
            Some((expected_tier, expected_weather)),
            "filament {tid} mismatch"
        );
    }
    for invalid in [0, 47760, 47770, 56130, 56135, 56137, 56138, 56144] {
        assert_eq!(classify_abyssal_filament(invalid), None);
    }

    assert_eq!(abyssal_mutaplasmid_for_tier(0), 47_699);
    assert_eq!(abyssal_mutaplasmid_for_tier(2), 47_699);
    assert_eq!(abyssal_mutaplasmid_for_tier(3), 47_701);
    assert_eq!(abyssal_mutaplasmid_for_tier(4), 47_701);
    assert_eq!(abyssal_mutaplasmid_for_tier(5), 47_700);
    assert_eq!(abyssal_mutaplasmid_for_tier(6), 47_700);

    assert_eq!(abyssal_room_gate_type(1), 47_685);
    assert_eq!(abyssal_room_gate_type(2), 47_685);
    assert_eq!(abyssal_room_gate_type(3), 47_686);

    let ctx = new_ctx();
    for t in 0..=3 {
        let p = roll_abyssal_weather_penalty_pct(&ctx, t);
        assert!(p == 30.0 || p == 50.0, "tier {t} rolled {p}");
    }
    for t in 4..=6 {
        let p = roll_abyssal_weather_penalty_pct(&ctx, t);
        assert!(p == 50.0 || p == 70.0, "tier {t} rolled {p}");
    }

    assert_eq!(evaluate_highsec_abyssal_tier_policy(0.85, 0).unwrap(), false);
    assert_eq!(
        evaluate_highsec_abyssal_tier_policy(0.85, 1).unwrap_err().code,
        "FilamentDisallowedInHighSec"
    );
    for (sec, tier, expected) in [
        (0.84, 3, false),
        (0.84, 4, true),
        (0.75, 3, false),
        (0.75, 4, true),
        (0.74, 4, false),
        (0.65, 4, false),
        (0.65, 5, true),
        (0.64, 5, false),
        (0.55, 5, false),
        (0.55, 6, true),
        (0.54, 6, false),
    ] {
        assert_eq!(evaluate_highsec_abyssal_tier_policy(sec, tier).unwrap(), expected);
    }
}

#[test]
fn deep_weather_combat_nav_and_ship_dogma_modifiers_and_restore_all_weathers() {
    let ctx = new_ctx();
    let char_id = 90_301_001;
    let ship_id = 1_301_000_001;
    let turret_id = 1_301_000_002;
    let cruiser_tid = ctx.sde.types.values().find(|t| t.group_id == 26).unwrap().type_id;
    let turret_tid = ctx
        .sde
        .types
        .values()
        .find(|t| matches!(t.group_id, 53 | 55 | 74))
        .unwrap()
        .type_id;

    let mut cs = CombatState::new();
    cs.max_shield_hp = 1000.0;
    cs.shield_hp = 1000.0;
    cs.max_armor_hp = 2000.0;
    cs.armor_hp = 2000.0;
    cs.max_structure_hp = 1500.0;
    cs.structure_hp = 1500.0;
    cs.shield_resonance = [0.5, 0.5, 0.5, 0.5];
    cs.armor_resonance = [0.4, 0.4, 0.4, 0.4];
    cs.structure_resonance = [0.6, 0.6, 0.6, 0.6];
    cs.capacitor_recharge_time_ms = 10_000.0;
    let (mut vel, mut opt) = (200.0, 10_000.0);

    apply_abyssal_weather_to_combat_and_nav(AbyssalWeatherKind::Electrical, 50.0, 50.0, &mut cs, &mut vel, &mut opt);
    assert!((cs.shield_resonance[0] - 0.75).abs() < 1e-9);
    assert!((cs.armor_resonance[0] - 0.60).abs() < 1e-9);
    assert!((cs.structure_resonance[0] - 0.90).abs() < 1e-9);
    assert!((cs.capacitor_recharge_time_ms - 5_000.0).abs() < 1e-9);

    apply_abyssal_weather_to_combat_and_nav(AbyssalWeatherKind::Dark, 95.0, 50.0, &mut cs, &mut vel, &mut opt);
    assert!((vel - 300.0).abs() < 1e-9);
    assert!((opt - 1_000.0).abs() < 1e-9);

    apply_abyssal_weather_to_combat_and_nav(AbyssalWeatherKind::Exotic, 50.0, 50.0, &mut cs, &mut vel, &mut opt);
    assert!((cs.shield_resonance[2] - 0.75).abs() < 1e-9);
    assert!((cs.armor_resonance[2] - 0.60).abs() < 1e-9);
    assert!((cs.structure_resonance[2] - 0.90).abs() < 1e-9);

    apply_abyssal_weather_to_combat_and_nav(AbyssalWeatherKind::Firestorm, 50.0, 50.0, &mut cs, &mut vel, &mut opt);
    assert!((cs.max_armor_hp - 3000.0).abs() < 1e-9);
    assert!((cs.armor_hp - 3000.0).abs() < 1e-9);
    assert!((cs.shield_resonance[1] - 0.75).abs() < 1e-9);

    apply_abyssal_weather_to_combat_and_nav(AbyssalWeatherKind::Gamma, 50.0, 50.0, &mut cs, &mut vel, &mut opt);
    assert!((cs.max_shield_hp - 1500.0).abs() < 1e-9);
    assert!((cs.shield_hp - 1500.0).abs() < 1e-9);
    assert!((cs.shield_resonance[3] - 0.75).abs() < 1e-9);

    ctx.db.with_mut_snapshot(|snap| {
        let mut sh = make_item(ship_id, cruiser_tid, char_id, 32_000_010, InventoryFlag::None, 1);
        sh.dynamic_attributes = HashMap::from([(37, 240.0), (54, 20_000.0)]);
        snap.items.insert(ship_id, sh);
        let mut tur = make_item(turret_id, turret_tid, char_id, ship_id, InventoryFlag::HiSlot0, 1);
        tur.dynamic_attributes = HashMap::from([(54, 12_000.0), (158, 8_000.0)]);
        snap.items.insert(turret_id, tur);
    });

    for weather in [
        AbyssalWeatherKind::Electrical,
        AbyssalWeatherKind::Exotic,
        AbyssalWeatherKind::Firestorm,
        AbyssalWeatherKind::Gamma,
        AbyssalWeatherKind::Dark,
    ] {
        let saved = apply_abyssal_weather_dogma_modifiers_to_ship(&ctx, ship_id, weather, 50.0, 50.0);
        assert!(!saved.is_empty());
        if weather == AbyssalWeatherKind::Dark {
            ctx.db.with_snapshot(|snap| {
                let sh = &snap.items[&ship_id];
                assert!((sh.dynamic_attributes[&37] - 360.0).abs() < 1e-6);
                assert!((sh.dynamic_attributes[&54] - 10_000.0).abs() < 1e-6);
                let tur = &snap.items[&turret_id];
                assert!((tur.dynamic_attributes[&54] - 6_000.0).abs() < 1e-6);
                assert!((tur.dynamic_attributes[&158] - 4_000.0).abs() < 1e-6);
                assert!(tur.dynamic_attributes.contains_key(&9954));
            });
            let t_type = ctx.sde.types.get(&turret_tid).unwrap();
            assert_eq!(
                dark_weather_midpocket_turret_mult(&ctx, char_id, 32_000_010, false, turret_id, t_type),
                1.0
            );
        }
        restore_abyssal_weather_dogma_modifiers(&ctx, ship_id, &saved);
    }

    ctx.db.with_snapshot(|snap| {
        let sh = &snap.items[&ship_id];
        assert!((sh.dynamic_attributes[&37] - 240.0).abs() < 1e-6);
        assert!((sh.dynamic_attributes[&54] - 20_000.0).abs() < 1e-6);
        let tur = &snap.items[&turret_id];
        assert!((tur.dynamic_attributes[&54] - 12_000.0).abs() < 1e-6);
        assert!((tur.dynamic_attributes[&158] - 8_000.0).abs() < 1e-6);
        assert!(!tur.dynamic_attributes.contains_key(&9954));
        assert!(!tur.dynamic_attributes.contains_key(&9958));
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deep_spatial_hazard_clouds_radius_and_ballpark_modifiers() {
    let ctx = new_ctx();
    for (tid, fb, expected) in [
        (BIOLUMINESCENT_CLOUD_SMALL_TYPE_ID, 0.0, 5_000.0),
        (FILAMENT_CLOUD_SMALL_TYPE_ID, 0.0, 5_000.0),
        (TACHYON_CLOUD_TYPE_ID, 0.0, 15_000.0),
        (BIOLUMINESCENT_CLOUD_TYPE_ID, 0.0, 15_000.0),
        (FILAMENT_CLOUD_TYPE_ID, 0.0, 15_000.0),
        (PROVING_TACHYON_CLOUD_TYPE_ID, 0.0, 40_000.0),
        (BIOLUMINESCENT_CLOUD_LARGE_TYPE_ID, 0.0, 40_000.0),
        (FILAMENT_CLOUD_LARGE_TYPE_ID, 0.0, 40_000.0),
        (999_999, 22_000.0, 22_000.0),
    ] {
        assert_eq!(spatial_cloud_radius_m(None, tid, fb), expected);
    }

    let ab_sys: SolarSystemId = 32_000_111;
    let char_id: i64 = 90_302_001;
    let ship_id: i64 = 1_302_000_001;
    let cruiser_tid = ctx.sde.types.values().find(|t| t.group_id == 26).unwrap().type_id;
    ctx.db.provision_gm_account("cloud_pilot", "pw", char_id, "CloudPilot");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = ab_sys;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(
            ship_id,
            make_item(ship_id, cruiser_tid, char_id, ab_sys as i64, InventoryFlag::None, 1),
        );
    });

    assert_eq!(
        evaluate_abyssal_spatial_clouds_for_ship(&ctx, VOID_SPACE_SYSTEM_ID, ship_id).await,
        None
    );

    let sys_handle = ctx.systems.get_or_spawn(ab_sys);
    let base_cs = ctx.build_ship_combat_state(char_id, ship_id, cruiser_tid);
    let base_sig = base_cs.signature_radius;
    let _ = sys_handle
        .spawn_ball_with_combat(make_ball(ship_id, cruiser_tid, DVec3::ZERO, 120.0), base_cs)
        .await;

    let cloud_id = 1_890_000_001;
    let _ = sys_handle
        .spawn_ball(make_ball(cloud_id, TACHYON_CLOUD_TYPE_ID, DVec3::new(1_000.0, 0.0, 0.0), 15_000.0))
        .await;
    assert_eq!(
        evaluate_abyssal_spatial_clouds_for_ship(&ctx, ab_sys, ship_id).await,
        Some("TachyonCloud")
    );
    let cs_tach = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!((cs_tach.signature_radius - base_sig * 4.0).abs() < 1e-3);

    let _ = sys_handle.remove_ball(cloud_id).await;
    let _ = sys_handle
        .spawn_ball(make_ball(cloud_id, FILAMENT_CLOUD_TYPE_ID, DVec3::new(1_000.0, 0.0, 0.0), 15_000.0))
        .await;
    assert_eq!(
        evaluate_abyssal_spatial_clouds_for_ship(&ctx, ab_sys, ship_id).await,
        Some("FilamentCloud")
    );
    let cs_fil = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!((cs_fil.shield_boost_multiplier - 0.6).abs() < 1e-6);

    let _ = sys_handle.remove_ball(cloud_id).await;
    assert_eq!(
        evaluate_abyssal_spatial_clouds_for_ship(&ctx, ab_sys, ship_id).await,
        None
    );
    let cs_clear = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!((cs_clear.shield_boost_multiplier - 1.0).abs() < 1e-6);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deep_hull_capacity_filament_validation_and_in_pocket_rpc_restrictions() {
    let ctx = new_ctx();
    let origin_sys: SolarSystemId = 30_000_142;
    let char_id: i64 = 90_303_001;
    let (frig_ship_id, dessie_ship_id, bs_ship_id, fil_item_id) =
        (1_303_000_001, 1_303_000_002, 1_303_000_003, 1_303_000_050);

    let frig_tid = ctx.sde.types.values().find(|t| t.group_id == 25).unwrap().type_id;
    let dessie_tid = ctx.sde.types.values().find(|t| t.group_id == 420).unwrap().type_id;
    let bs_tid = ctx.sde.types.values().find(|t| t.group_id == 27).unwrap().type_id;

    ctx.db.provision_gm_account("val_pilot", "pw", char_id, "ValPilot");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = origin_sys;
            ch.station_id = None;
            ch.active_ship_id = frig_ship_id;
        }
        for (sid, tid) in [(frig_ship_id, frig_tid), (dessie_ship_id, dessie_tid), (bs_ship_id, bs_tid)] {
            snap.items.insert(sid, make_item(sid, tid, char_id, origin_sys as i64, InventoryFlag::None, 1));
        }
        snap.items.insert(
            fil_item_id,
            make_item(fil_item_id, 56131, char_id, frig_ship_id, InventoryFlag::Cargo, 2),
        );
    });

    assert_eq!(validate_abyssal_hull_and_capacity(&ctx, frig_ship_id).unwrap().2, 3);
    assert_eq!(validate_abyssal_hull_and_capacity(&ctx, dessie_ship_id).unwrap().2, 2);
    assert_eq!(
        validate_abyssal_hull_and_capacity(&ctx, bs_ship_id).unwrap_err().code,
        "DisallowedShipType"
    );
    assert_eq!(
        validate_and_consume_abyssal_filaments(&ctx, char_id, frig_ship_id, origin_sys, 0)
            .await
            .unwrap_err()
            .code,
        "MissingRequiredArgument"
    );
    assert_eq!(
        validate_and_consume_abyssal_filaments(&ctx, char_id, frig_ship_id, origin_sys, fil_item_id)
            .await
            .unwrap_err()
            .code,
        "NotInFleet"
    );

    let far_pos = DVec3::new(9.0e11, -8.0e11, 7.0e11);
    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.insert(
            8801,
            eve_db::types::FleetRecord {
                fleet_id: 8801,
                boss_id: char_id,
                members: vec![char_id],
                ..Default::default()
            },
        );
        snap.item_positions.insert(frig_ship_id, far_pos.to_array());
    });
    assert_eq!(
        validate_and_consume_abyssal_filaments(&ctx, char_id, frig_ship_id, origin_sys, fil_item_id)
            .await
            .unwrap_err()
            .code,
        "InsufficientFilamentQuantity"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&fil_item_id).unwrap().quantity = 4;
    });
    let (_, consumed_tid, tier, max_ships, _, _, fleet_opt) =
        validate_and_consume_abyssal_filaments(&ctx, char_id, frig_ship_id, origin_sys, fil_item_id)
            .await
            .expect("frigate filament activation succeeds");
    assert_eq!((consumed_tid, tier, max_ships, fleet_opt), (56131, 0, 3, Some(8801)));
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&fil_item_id].quantity), 1);

    with_pve_special_reg(&ctx, |reg| {
        reg.abyssal_traces.insert(
            1_897_000_999,
            AbyssalTraceRecord {
                trace_ball_id: 1_897_000_999,
                char_id,
                filament_type_id: 56131,
                origin_solar_system_id: origin_sys,
                position: (far_pos + DVec3::new(100_000.0, 0.0, 0.0)).to_array(),
                created_unix: ctx.now_unix(),
                expires_at_sec: ctx.now_unix() + 900,
                max_ships: 3,
                entered_char_ids: Vec::new(),
                pocket_id: None,
                fleet_id: Some(8801),
            },
        );
    });
    assert_eq!(
        check_filament_1000km_proximity(&ctx, origin_sys, far_pos).await.unwrap_err().code,
        "ProximityRestriction"
    );
    assert_eq!(
        check_not_in_deadspace_pocket(&ctx, 32_000_050, far_pos).await.unwrap_err().code,
        "ShipInDungeon"
    );

    assert!(is_abyssal_system(32_000_001));
    assert!(!is_abyssal_system(30_000_142));
    assert!(is_abyssal_local_channel_disabled("solarsystemid2:32000001", None));
    assert!(is_abyssal_local_channel_disabled("local", Some(32_000_001)));
    assert!(!is_abyssal_local_channel_disabled("local", Some(30_000_142)));

    let mut sess = SessionState::new(1);
    sess.solar_system_id = Some(32_000_001);
    assert_eq!(
        check_abyssal_in_pocket_rpc_restrictions(&sess, "scanSvc", "ConeScan", &[])
            .unwrap()
            .unwrap_err()
            .code,
        "CannotDscanInThisSystem"
    );
    assert_eq!(
        check_abyssal_in_pocket_rpc_restrictions(&sess, "LSC", "SendMessage", &[PyRep::String("local".into())])
            .unwrap()
            .unwrap_err()
            .code,
        "LocalChannelDisabledInAbyssalSpace"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deep_room_1_2_3_conduit_progression_pocket_collapse_and_downtime_evacuation() {
    let ctx = new_ctx();
    let origin_sys: SolarSystemId = 30_000_142;
    let char_id: i64 = 90_304_001;
    let ship_id: i64 = 1_304_000_001;
    let cruiser_tid = ctx.sde.types.values().find(|t| t.group_id == 26).unwrap().type_id;

    ctx.db.provision_gm_account("prog_pilot", "pw", char_id, "ProgPilot");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = origin_sys;
            ch.station_id = None;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(
            ship_id,
            make_item(ship_id, cruiser_tid, char_id, origin_sys as i64, InventoryFlag::None, 1),
        );
    });

    let mut session = SessionState::new(77);
    session.user_id = Some(1);
    session.character_id = Some(char_id);
    session.solar_system_id = Some(origin_sys);
    session.ship_id = Some(ship_id);

    let (pocket_id, r1_npcs) =
        enter_abyssal_pocket_for_character(&ctx, &mut session, char_id, 56131, 0, origin_sys).await;
    let ab_sys = session.solar_system_id.unwrap();
    let ab_handle = ctx.systems.get_or_spawn(ab_sys);
    let base = DVec3::from_array(pocket_grid_offset(&ctx, pocket_id));
    let _ = ab_handle
        .spawn_ball(make_ball(ship_id, cruiser_tid, base, 120.0))
        .await;
    let kwargs = HashMap::new();

    let r1_gate = 1_895_000_000 + pocket_id * 10 + 1;
    assert_eq!(
        handle_abyssal_gate_activation(&ctx, &mut session, char_id, &[PyRep::Integer(r1_gate)], &kwargs)
            .await
            .unwrap_err()
            .code,
        "GateLocked"
    );

    for r_idx in 1..=3_i64 {
        let npcs = with_pve_special_reg(&ctx, |reg| {
            reg.abyssal_pockets[&char_id].rooms[(r_idx - 1) as usize].npc_ball_ids.clone()
        });
        for &nid in &npcs {
            let _ = ab_handle.remove_ball(nid).await;
            on_abyssal_room_npc_destroyed_core(&ctx.systems, &ctx.db, ab_sys, nid).await;
        }
        let gate_id = 1_895_000_000 + pocket_id * 10 + r_idx;
        let gate_pos = base + DVec3::new(0.0, 0.0, ((r_idx - 1) as f64) * 50_000.0);
        let _ = ab_handle.set_ball_position(ship_id, gate_pos).await;
        if r_idx < 3 {
            let res = handle_abyssal_gate_activation(
                &ctx,
                &mut session,
                char_id,
                &[PyRep::Integer(gate_id)],
                &kwargs,
            )
            .await
            .expect("transfer gate activation succeeds");
            assert_eq!(res.dict_get("room").and_then(|v| v.as_i64()), Some(r_idx + 1));
        } else {
            let res = handle_abyssal_end_gate_activation(
                &ctx,
                &mut session,
                char_id,
                &[PyRep::Integer(gate_id)],
            )
            .await
            .expect("exit conduit activation succeeds");
            assert_eq!(res.dict_get("exitedAbyssalSpace").and_then(|v| v.as_bool()), Some(true));
        }
    }
    assert_eq!(session.solar_system_id, Some(origin_sys));
    assert_eq!(r1_npcs.len(), 2);

    let (pid2, _) =
        enter_abyssal_pocket_for_character(&ctx, &mut session, char_id, 47896, 4, origin_sys).await;
    assert!(pid2 > pocket_id);
    let ab_sys2 = session.solar_system_id.unwrap();
    assert_eq!(reconcile_abyssal_login(&ctx, char_id, ab_sys2, ship_id), ab_sys2);
    assert_eq!(evacuate_abyssal_pockets_for_downtime(&ctx), 1);
    assert_eq!(
        ctx.db.with_snapshot(|s| s.characters[&char_id].solar_system_id),
        origin_sys
    );

    with_pve_special_reg(&ctx, |reg| {
        reg.void_space_sessions.insert(
            char_id,
            VoidSpaceSessionRecord {
                char_id,
                ship_id,
                origin_solar_system_id: origin_sys,
                origin_position: [1000.0, 2000.0, 3000.0],
                entered_unix: 100,
                expires_at_sec: 200,
                target_id: 77,
            },
        );
    });
    sweep_expired_void_space_sessions_sync(&ctx, 250);
    assert!(with_pve_special_reg(&ctx, |reg| reg.void_space_sessions.is_empty()));

    let home_sys = collapse_abyssal_pocket_and_pod_pilot_at(&ctx, char_id, ship_id, ab_sys2, ctx.now_unix());
    assert!(home_sys > 0);
    assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&ship_id)));
}
