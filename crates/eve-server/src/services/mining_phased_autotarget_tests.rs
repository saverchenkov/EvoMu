use super::*;
use crate::router::ServerContext;
use eve_db::{FleetRecord, InventoryFlag, InventoryItemRecord, WorldRepository};
use eve_destiny::{Ball, Ballpark, CombatState, MovementMode, SlimItemMeta};
use eve_sde::{InvType, SdeRegistry};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

fn make_inv_type(
    type_id: u32,
    group_id: u32,
    category_id: u32,
    name: &str,
    default_attributes: HashMap<u16, f64>,
) -> InvType {
    InvType {
        type_id,
        group_id,
        category_id,
        name: name.into(),
        mass: 1.0,
        volume: 10.0,
        capacity: 0.0,
        radius: 10.0,
        base_price: 100.0,
        published: true,
        portion_size: 1,
        market_group_id: None,
        race_id: None,
        faction_id: None,
        variation_parent_type_id: None,
        description_id: None,
        icon_id: None,
        graphic_id: None,
        sound_id: None,
        meta_group_id: None,
        meta_level: None,
        tech_level: None,
        default_effect_id: None,
        description: String::new(),
        effect_ids: Vec::new(),
        default_attributes,
    }
}

fn make_ctx() -> ServerContext {
    let mut sde = SdeRegistry::with_canonical_seed();
    sde.types.insert(
        550_001,
        make_inv_type(
            550_001,
            550,
            11,
            "Pirate Cruiser",
            HashMap::from([(552, 125.0)]),
        ),
    );
    sde.types.insert(
        301_001,
        make_inv_type(301_001, 301, 11, "CONCORD Battleship", HashMap::new()),
    );
    sde.types.insert(
        41_001,
        make_inv_type(
            41_001,
            41,
            7,
            "Medium Remote Shield Booster I",
            HashMap::new(),
        ),
    );

    let db = Arc::new(WorldRepository::with_demo_seed());
    ServerContext::with_sde_and_repository(Arc::new(sde), db)
}

#[test]
fn test_prismaticite_and_ics_energy_and_kspace_checks() {
    let mut ctx = make_ctx();
    let mut sde = (*ctx.sde).clone();
    sde.types.insert(
        99_991,
        make_inv_type(
            99_991,
            4915,
            25,
            "Custom Prismaticite Variant",
            HashMap::new(),
        ),
    );
    sde.types.insert(
        99_992,
        make_inv_type(99_992, 941, 6, "Fleet Orca Hull", HashMap::new()),
    );
    ctx.sde = Arc::new(sde);

    for tid in PRISMATICITE_ORE_TYPES {
        assert!(is_prismaticite_ore(&ctx.sde, tid));
    }
    assert!(is_prismaticite_ore(&ctx.sde, 99_991));
    assert!(!is_prismaticite_ore(&ctx.sde, 1230));
    assert!(!is_prismaticite_ore(&ctx.sde, 28_352));

    assert_eq!(ics_energy_for_type(&ctx.sde, 28_352), 100);
    assert_eq!(ics_energy_for_type(&ctx.sde, 28_606), 50);
    assert_eq!(ics_energy_for_type(&ctx.sde, 99_992), 50);
    assert_eq!(ics_energy_for_type(&ctx.sde, 42_244), 35);
    assert_eq!(ics_energy_for_type(&ctx.sde, 90_041), 0);
    assert_eq!(ics_energy_for_type(&ctx.sde, 777_777), 0);

    // High-sec, Pochven, and Wormhole/Abyssal boundaries
    assert!(!is_low_or_null_kspace(&ctx, 30_000_142));
    assert!(is_low_or_null_kspace(&ctx, 30_002_537));
    assert!(!is_low_or_null_kspace(&ctx, 30_000_157));
    assert!(!is_low_or_null_kspace(&ctx, 31_000_000));
    assert!(!is_low_or_null_kspace(&ctx, 32_999_999));
    assert!(!is_low_or_null_kspace(&ctx, 99_999_999));
}

#[test]
fn test_spawn_and_advance_phased_asteroid_field_and_efficiency() {
    let ctx = make_ctx();
    let hi_sec = 30_000_142;
    let low_sec = 30_002_537;

    assert!(
        spawn_phased_asteroid_field(&ctx, hi_sec, "PHS-HI", 5001, 10_000, (0.0, 0.0, 0.0))
            .is_none()
    );
    assert_eq!(phased_field_efficiency_for_ore(&ctx, low_sec, 90_041), 1.0);

    let st = spawn_phased_asteroid_field(
        &ctx,
        low_sec,
        "PHS-101",
        5002,
        25_000,
        (10_000.0, 20_000.0, 30_000.0),
    )
    .expect("should spawn in low-sec");
    assert_eq!(st.signature_id, "PHS-101");
    assert_eq!(st.asteroid_ids, vec![5002]);
    assert!(!st.phased_in);
    assert!(!st.beacon_spawned);
    assert!(!st.collapsed);

    // Non-prismaticite ore always has 1.0 efficiency
    assert_eq!(phased_field_efficiency_for_ore(&ctx, low_sec, 1230), 1.0);
    // Breached prismaticite has 0.10 efficiency
    assert!((phased_field_efficiency_for_ore(&ctx, low_sec, 90_041) - 0.10).abs() < 1e-9);
    assert!(
        (phased_field_efficiency_for_asteroid(&ctx, low_sec, 5002, 90_041) - 0.10).abs() < 1e-9
    );
    // Different asteroid ID not in field has 1.0 efficiency
    assert!(
        (phased_field_efficiency_for_asteroid(&ctx, low_sec, 9999, 90_041) - 1.0).abs() < 1e-9
    );

    // Advance fracture to 0.5 -> beacon spawned
    let mid = advance_phased_asteroid_field(&ctx, low_sec, 0.5).expect("field exists");
    assert!(mid.beacon_spawned);
    assert!(!mid.collapsed);
    let has_anom = ctx.db.with_snapshot(|snap| {
        snap.cosmic_anomalies
            .get(&low_sec)
            .is_some_and(|v| v.iter().any(|a| a.anomaly_id == "PHS-101" && !a.cleared))
    });
    assert!(has_anom);

    // Advance fracture to 1.0 -> collapsed
    let end = advance_phased_asteroid_field(&ctx, low_sec, 0.5).expect("field exists");
    assert!(end.collapsed);
    assert_eq!(phased_field_efficiency_for_ore(&ctx, low_sec, 90_041), 0.0);
    ctx.db.with_snapshot(|snap| {
        let sig = snap.cosmic_signatures[&low_sec]
            .iter()
            .find(|s| s.signature_id == "PHS-101")
            .unwrap();
        assert!(sig.collapsed);
        let anom = snap.cosmic_anomalies[&low_sec]
            .iter()
            .find(|a| a.anomaly_id == "PHS-101")
            .unwrap();
        assert!(anom.cleared);
        assert_eq!(snap.asteroids[&5002].quantity, 0);
    });
}

#[tokio::test]
async fn test_connect_ics_and_tick_phased_asteroid_fields() {
    let ctx = make_ctx();
    let low_sec = 30_002_537;
    let char_id = 90_000_001;
    let ship_id = 700_001;
    let anchor_id = 700_002;
    let core_id = 700_003;

    spawn_phased_asteroid_field(&ctx, low_sec, "PHS-202", 6001, 40_000, (1000.0, 0.0, 0.0))
        .unwrap();

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = low_sec;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 28_352, // Rorqual (100 energy)
                owner_id: char_id,
                location_id: i64::from(low_sec),
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            anchor_id,
            InventoryItemRecord {
                item_id: anchor_id,
                type_id: TYPE_MOBILE_PHASE_ANCHOR,
                owner_id: char_id,
                location_id: i64::from(low_sec),
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            core_id,
            InventoryItemRecord {
                item_id: core_id,
                type_id: 28_583, // Industrial Core I
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.module_active_states.insert(core_id);
    });

    let sys = ctx.systems.get_or_spawn(low_sec);
    let _ = sys
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 28_352,
            position: DVec3::new(0.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 100.0,
            inertia: 1.0,
            mass: 1e9,
            radius: 1000.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    let _ = sys
        .spawn_ball(Ball {
            id: anchor_id,
            type_id: TYPE_MOBILE_PHASE_ANCHOR,
            position: DVec3::new(25_000.0, 0.0, 0.0), // within 50 km
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1e6,
            radius: 100.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    let phased = connect_ics_to_phase_anchor(&ctx, low_sec, ship_id).await;
    assert_eq!(phased, Some(true));
    assert!((phased_field_efficiency_for_ore(&ctx, low_sec, 90_041) - 1.0).abs() < 1e-9);

    on_mobile_phase_anchor_deployed(&ctx, low_sec, anchor_id).await;
    let now = ctx.now_unix_secs();
    tick_phased_asteroid_fields(&ctx, now + 7_200).await;
    let state = ctx
        .phased_asteroid_fields
        .read()
        .unwrap()
        .get(&low_sec)
        .cloned()
        .unwrap();
    assert!(state.beacon_spawned);
    assert!(state.phased_in);
}

#[test]
fn test_command_burst_hull_allowed_and_offensive_cycle() {
    let ctx = make_ctx();
    let unrestricted = make_inv_type(1, 1, 7, "Unrestricted Mod", HashMap::new());
    assert!(ensure_command_burst_hull_allowed(&ctx.sde, &unrestricted, 28_352).is_ok());

    let group_restricted = make_inv_type(
        2,
        1,
        7,
        "Mining Foreman Burst I",
        HashMap::from([(1298, 941.0), (1299, 883.0)]),
    );
    assert!(ensure_command_burst_hull_allowed(&ctx.sde, &group_restricted, 28_352).is_ok());
    assert!(ensure_command_burst_hull_allowed(&ctx.sde, &group_restricted, 28_606).is_ok());
    let err = ensure_command_burst_hull_allowed(&ctx.sde, &group_restricted, 90_041).unwrap_err();
    assert_eq!(err.code, "CantFitToShip");

    let type_restricted = make_inv_type(
        3,
        1,
        7,
        "Rorqual Exclusive Burst",
        HashMap::from([(1302, 28_352.0)]),
    );
    assert!(ensure_command_burst_hull_allowed(&ctx.sde, &type_restricted, 28_352).is_ok());
    assert!(ensure_command_burst_hull_allowed(&ctx.sde, &type_restricted, 28_606).is_err());

    let base_cycle = crate::system_actor::ActiveModuleCycle {
        ship_id: 100,
        module_id: 10,
        module_type_id: 200,
        target_id: Some(20),
        effect_name: "targetAttack".into(),
        repeat: 1,
        duration_s: 5.0,
        ..Default::default()
    };
    assert!(is_offensive_aggressor_cycle(&base_cycle, Some(&ctx.sde)));

    let mut rep_cycle = base_cycle.clone();
    rep_cycle.remote_shield_amount = 50.0;
    assert!(!is_offensive_aggressor_cycle(&rep_cycle, Some(&ctx.sde)));

    let mut group_rep_cycle = base_cycle.clone();
    group_rep_cycle.module_type_id = 41_001; // group 41
    assert!(!is_offensive_aggressor_cycle(
        &group_rep_cycle,
        Some(&ctx.sde)
    ));
}

#[tokio::test]
async fn test_fleet_ships_hostile_npc_group_and_auto_targeter_tick() {
    let ctx = make_ctx();
    assert_eq!(fleet_ship_ids_for_ship(None, 100), vec![100]);

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            100,
            InventoryItemRecord {
                item_id: 100,
                owner_id: 90_000_001,
                type_id: 28_352,
                ..Default::default()
            },
        );
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.active_ship_id = 100;
        }
        let mut c2 = snap.characters[&90_000_001].clone();
        c2.character_id = 90_000_002;
        c2.active_ship_id = 200;
        snap.characters.insert(90_000_002, c2);
        snap.fleets.insert(
            1,
            FleetRecord {
                fleet_id: 1,
                boss_id: 90_000_001,
                members: vec![90_000_001, 90_000_002],
                motd: String::new(),
                free_move: false,
                ..Default::default()
            },
        );
    });
    let ships = fleet_ship_ids_for_ship(Some(&ctx.db), 100);
    assert_eq!(ships, vec![100, 200]);

    // Hostile NPC group checks
    assert!(!is_hostile_npc_group(301, None));
    assert!(!is_hostile_npc_group(99, None));
    assert!(is_hostile_npc_group(550, None));
    assert!(is_hostile_npc_group(1167, Some(500_002)));
    assert!(!is_hostile_npc_group(1167, Some(500_001)));
    assert!(is_hostile_npc_group(1166, Some(500_001)));
    assert!(!is_hostile_npc_group(1166, Some(500_002)));

    // Ballpark auto-targeter tick
    let mut bp = Ballpark::new(30_002_537);
    bp.add_ball(Ball {
        id: 100,
        type_id: 28_352,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 100.0,
        inertia: 1.0,
        mass: 1e9,
        radius: 500.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    });
    bp.add_ball(Ball {
        id: 501,
        type_id: 550_001, // hostile NPC at 5 km
        position: DVec3::new(5_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 1e7,
        radius: 120.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    });
    bp.set_ball_slim_meta(
        501,
        SlimItemMeta {
            owner_id: 500_010,
            corp_id: 1_000_125,
            alliance_id: 0,
            security_status: -1.0,
            category_id: 11,
            group_id: 550,
            ..Default::default()
        },
    );
    bp.combat_states.insert(
        501,
        CombatState {
            shield_hp: 1000.0,
            max_shield_hp: 1000.0,
            armor_hp: 1000.0,
            max_armor_hp: 1000.0,
            structure_hp: 1000.0,
            max_structure_hp: 1000.0,
            capacitor: 500.0,
            max_capacitor: 500.0,
            signature_radius: 150.0,
            ..Default::default()
        },
    );
    bp.add_ball(Ball {
        id: 502,
        type_id: 301_001, // CONCORD at 3 km -> ignored
        position: DVec3::new(3_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 500.0,
        inertia: 1.0,
        mass: 1e8,
        radius: 300.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    });

    run_auto_targeter_ballpark_tick(
        &mut bp,
        Some(&ctx.sde),
        None,
        100,
        20_000.0,
        2,
        200.0,
        &[],
        &[100, 200],
    );
    let locks = bp.get_target_locks(100);
    assert_eq!(locks.len(), 1);
    assert_eq!(locks[0].target_id, 501);
    assert!(locks[0].lock_time_ms > 0.0);

    let sys = ctx.systems.get_or_spawn(30_002_537);
    let npc_ball = bp.balls.get(&501).unwrap().clone();
    assert!(
        is_auto_target_hostile_ball(&ctx, &sys, 90_000_001, 100, &npc_ball, &[90_000_002], &[])
            .await
    );
    let concord_ball = bp.balls.get(&502).unwrap().clone();
    assert!(
        !is_auto_target_hostile_ball(
            &ctx,
            &sys,
            90_000_001,
            100,
            &concord_ball,
            &[90_000_002],
            &[]
        )
        .await
    );
}

#[cfg(test)]
#[path = "mining_phased_autotarget_extra_unit_tests.rs"]
mod extra_unit_tests;
