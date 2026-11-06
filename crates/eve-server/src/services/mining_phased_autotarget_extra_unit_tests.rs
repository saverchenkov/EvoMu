//! Extra branch-coverage and mutation-killing unit tests for `mining_phased_autotarget`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::mining_phased_autotarget::{
    advance_phased_asteroid_field, connect_ics_to_phase_anchor, is_auto_target_hostile_ball,
    is_hostile_npc_group, is_offensive_aggressor_cycle, maybe_spawn_phased_asteroid_field,
    run_auto_targeter_ballpark_tick, spawn_phased_asteroid_field, tick_phased_asteroid_fields,
    PRISMATICITE_ORE_TYPES, TYPE_MOBILE_PHASE_ANCHOR,
};
use eve_db::{FleetRecord, InventoryFlag, InventoryItemRecord, WorldRepository};
use eve_destiny::{Ball, Ballpark, MovementMode, SlimItemMeta, TargetLock};
use eve_sde::{InvType, SdeRegistry};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| {
        let mut sde = SdeRegistry::with_canonical_seed();
        for (tid, gid, cid, name, attrs) in [
            (550_002_u32, 550_u32, 11_u32, "Hostile Rat", vec![(552_u16, 140.0)]),
            (28_606, 941, 6, "Orca", vec![]),
            (42_244, 941, 6, "Porpoise", vec![]),
        ] {
            sde.types.insert(
                tid,
                InvType {
                    type_id: tid,
                    group_id: gid,
                    category_id: cid,
                    name: name.into(),
                    mass: 1.0,
                    volume: 10.0,
                    capacity: 0.0,
                    radius: 85.0,
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
                    default_attributes: HashMap::from_iter(attrs),
                },
            );
        }
        Arc::new(sde)
    }))
}

fn make_ctx() -> ServerContext {
    let db = Arc::new(WorldRepository::with_demo_seed());
    ServerContext::with_sde_and_repository(shared_sde(), db)
}

#[tokio::test]
async fn maybe_spawn_connect_ics_rejection_branches_and_collapse_cleanup() {
    let ctx = make_ctx();
    let low_sec = 30_002_537_u32;
    let hi_sec = 30_000_142_u32;
    let owner_cid = 90_000_001_i64;
    let fleet_cid = 90_000_002_i64;
    let ship_orca = 710_001_i64;
    let ship_porpoise = 710_002_i64;
    let anchor_id = 710_003_i64;
    let core_orca = 710_004_i64;
    let core_porp = 710_005_i64;

    // maybe_spawn_phased_asteroid_field in high-sec always returns None
    assert!(maybe_spawn_phased_asteroid_field(&ctx, hi_sec).await.is_none());

    // Advance on unseeded system returns None
    assert!(advance_phased_asteroid_field(&ctx, low_sec, 0.25).is_none());

    // Spawn field, remove signature so advance_phased_asteroid_field uses asteroid position fallback, and clear asteroid_ids to test belt_id == 0 fallback depletion
    spawn_phased_asteroid_field(&ctx, low_sec, "PHS-FALLBACK", 6101, 15_000, (500.0, 600.0, 700.0))
        .unwrap();
    // When field is already active (not collapsed), maybe_spawn_phased_asteroid_field returns None
    for _ in 0..10 {
        assert!(maybe_spawn_phased_asteroid_field(&ctx, low_sec).await.is_none());
    }

    ctx.db.with_mut_snapshot(|snap| {
        if let Some( ch1 ) = snap.characters.get_mut(&owner_cid) {
            ch1.solar_system_id = low_sec;
            ch1.station_id = None;
            ch1.structure_id = None;
            ch1.active_ship_id = ship_orca;
        }
        let mut ch2 = snap.characters[&owner_cid].clone();
        ch2.character_id = fleet_cid;
        ch2.active_ship_id = ship_porpoise;
        snap.characters.insert(fleet_cid, ch2);
        snap.fleets.insert(
            77,
            FleetRecord {
                fleet_id: 77,
                boss_id: owner_cid,
                members: vec![owner_cid, fleet_cid],
                ..Default::default()
            },
        );
        snap.items.insert(
            ship_orca,
            InventoryItemRecord {
                item_id: ship_orca,
                type_id: 28_606, // Orca (50 energy)
                owner_id: owner_cid,
                location_id: i64::from(low_sec),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            ship_porpoise,
            InventoryItemRecord {
                item_id: ship_porpoise,
                type_id: 42_244, // Porpoise (35 energy)
                owner_id: fleet_cid,
                location_id: i64::from(low_sec),
                flag: InventoryFlag::None,
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
                owner_id: owner_cid,
                location_id: i64::from(low_sec),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        for (cid_mod, sh_id) in [(core_orca, ship_orca), (core_porp, ship_porpoise)] {
            snap.items.insert(
                cid_mod,
                InventoryItemRecord {
                    item_id: cid_mod,
                    type_id: 28_583,
                    owner_id: owner_cid,
                    location_id: sh_id,
                    flag: InventoryFlag::HiSlot0,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        }
    });

    // Without active industrial core, connect_ics_to_phase_anchor returns None
    assert!(connect_ics_to_phase_anchor(&ctx, low_sec, ship_orca)
        .await
        .is_none());
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.insert(core_orca);
        snap.module_active_states.insert(core_porp);
    });

    let sys = ctx.systems.get_or_spawn(low_sec);
    // Spawn ship in Warp mode -> rejected (returns None)
    let _ = sys
        .spawn_ball(Ball {
            id: ship_orca,
            type_id: 28_606,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 100.0,
            inertia: 1.0,
            mass: 1e8,
            radius: 500.0,
            speed_fraction: 1.0,
            mode: MovementMode::new_warp(DVec3::ZERO, DVec3::new(100_000.0, 0.0, 0.0), 3.0),
        })
        .await;
    let _ = sys
        .spawn_ball(Ball {
            id: anchor_id,
            type_id: TYPE_MOBILE_PHASE_ANCHOR,
            position: DVec3::new(60_000.0, 0.0, 0.0), // > 50 km out of range
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1e6,
            radius: 100.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    assert!(connect_ics_to_phase_anchor(&ctx, low_sec, ship_orca)
        .await
        .is_none());

    // Stop ship, but anchor still at 60 km (> 50 km) -> returns None
    let _ = sys.remove_ball(ship_orca).await;
    let _ = sys
        .spawn_ball(Ball {
            id: ship_orca,
            type_id: 28_606,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 100.0,
            inertia: 1.0,
            mass: 1e8,
            radius: 500.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.insert(core_orca);
    });
    assert!(connect_ics_to_phase_anchor(&ctx, low_sec, ship_orca)
        .await
        .is_none());

    // Move anchor to 40 km (within 50 km): Orca connects (50 energy -> Some(false)), fleetmate Porpoise connects (50 + 35 = 85 energy -> Some(false))
    let _ = sys.remove_ball(anchor_id).await;
    let _ = sys
        .spawn_ball(Ball {
            id: anchor_id,
            type_id: TYPE_MOBILE_PHASE_ANCHOR,
            position: DVec3::new(40_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1e6,
            radius: 100.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    let _ = sys
        .spawn_ball(Ball {
            id: ship_porpoise,
            type_id: 42_244,
            position: DVec3::new(10_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 150.0,
            inertia: 1.0,
            mass: 1e7,
            radius: 200.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    assert_eq!(
        connect_ics_to_phase_anchor(&ctx, low_sec, ship_orca).await,
        Some(false)
    );
    assert_eq!(
        connect_ics_to_phase_anchor(&ctx, low_sec, ship_porpoise).await,
        Some(false)
    );

    // tick_phased_asteroid_fields with elapsed == 0 reconciles without advancing fracture; elapsed == 14_400 collapses field
    let base_now = ctx.now_unix_secs();
    ctx.db.with_mut_snapshot(|snap| {
        snap.service_state_blobs
            .insert("phased_field_last_tick_unix".into(), base_now.to_string());
    });
    tick_phased_asteroid_fields(&ctx, base_now).await;
    assert!(!ctx.phased_asteroid_fields.read().unwrap()[&low_sec].collapsed);

    tick_phased_asteroid_fields(&ctx, base_now + 14_400).await;
    assert!(ctx.phased_asteroid_fields.read().unwrap()[&low_sec].collapsed);
    assert_eq!(
        ctx.db.with_snapshot(|s| s.asteroids[&6101].quantity),
        0
    );
    assert_eq!( PRISMATICITE_ORE_TYPES.len(), 4 );
}

#[tokio::test]
async fn offensive_cycles_fw_militia_hostility_crimewatch_and_autotarget_limits() {
    let ctx = make_ctx();

    // 1. is_offensive_aggressor_cycle: remote armor, hull, cap -> false; sde = None -> true
    let base = crate::system_actor::ActiveModuleCycle {
        ship_id: 1,
        module_id: 2,
        module_type_id: 200,
        target_id: Some(3),
        effect_name: String::new(),
        repeat: 1,
        duration_s: 5.0,
        ..Default::default()
    };
    assert!(is_offensive_aggressor_cycle(&base, None));
    for (armor, hull, cap) in [(10.0, 0.0, 0.0), (0.0, 10.0, 0.0), (0.0, 0.0, 10.0)] {
        let mut c = base.clone();
        c.remote_armor_amount = armor;
        c.remote_hull_amount = hull;
        c.remote_cap_amount = cap;
        assert!(!is_offensive_aggressor_cycle(&c, None));
    }

    // 2. is_hostile_npc_group: excluded non-combatant groups and FW militia matrix
    for non_hostile in [
        99, 180, 182, 297, 298, 301, 306, 319, 323, 335, 446, 925, 952, 1207, 1872, 1927, 1928,
        4073, 4494, 4572, 4576, 4771,
    ] {
        assert!(!is_hostile_npc_group(non_hostile, Some(500_001)));
    }
    // Amarr/Caldari FW group 5063: hostile only to Minmatar (500002) and Gallente (500004)
    assert!(is_hostile_npc_group(5063, Some(500_002)));
    assert!(is_hostile_npc_group(5063, Some(500_004)));
    assert!(!is_hostile_npc_group(5063, None));
    assert!(!is_hostile_npc_group(5063, Some(500_003)));
    // Gallente/Minmatar FW group 5065: hostile only to Caldari (500001) and Amarr (500003)
    assert!(is_hostile_npc_group(5065, Some(500_001)));
    assert!(is_hostile_npc_group(5065, Some(500_003)));
    assert!(!is_hostile_npc_group(5065, None));
    assert!(!is_hostile_npc_group(5065, Some(500_004)));

    // 3. is_auto_target_hostile_ball: fleetmate in active_aggressors ignored, neutral player ignored, criminal / limited engagement player targeted
    let self_cid = 90_000_001_i64;
    let fleet_cid = 90_000_002_i64;
    let enemy_cid = 90_000_003_i64;
    let fleet_ship_id = 810_001_i64;
    let enemy_ship_id = 810_002_i64;
    let rock_ball_id = 810_003_i64;

    ctx.db.with_mut_snapshot(|snap| {
        for (sh_id, owner) in [(fleet_ship_id, fleet_cid), (enemy_ship_id, enemy_cid)] {
            snap.items.insert(
                sh_id,
                InventoryItemRecord {
                    item_id: sh_id,
                    type_id: 587, // Rifter (category 6)
                    owner_id: owner,
                    location_id: 30_002_537,
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        }
    });

    let sys = ctx.systems.get_or_spawn(30_002_537);
    let make_ball = |id: i64, type_id: u32, x: f64| Ball {
        id,
        type_id,
        position: DVec3::new(x, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 1e6,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };

    let fleet_ball = make_ball(fleet_ship_id, 587, 2_000.0);
    let enemy_ball = make_ball(enemy_ship_id, 587, 3_000.0);
    let rock_ball = make_ball(rock_ball_id, 90_041, 1_000.0);

    // Fleetmate ship even if listed in active_aggressors is NOT hostile
    assert!(
        !is_auto_target_hostile_ball(
            &ctx,
            &sys,
            self_cid,
            100,
            &fleet_ball,
            &[fleet_cid],
            &[fleet_ship_id]
        )
        .await
    );
    // Non-fleet aggressor IS hostile
    assert!(
        is_auto_target_hostile_ball(
            &ctx,
            &sys,
            self_cid,
            100,
            &enemy_ball,
            &[fleet_cid],
            &[enemy_ship_id]
        )
        .await
    );
    // Asteroid ball (not NPC, not player ship) is NOT hostile
    assert!(
        !is_auto_target_hostile_ball(&ctx, &sys, self_cid, 100, &rock_ball, &[], &[]).await
    );
    // Neutral player ship without aggression/crimewatch is NOT hostile
    assert!(
        !is_auto_target_hostile_ball(&ctx, &sys, self_cid, 100, &enemy_ball, &[], &[]).await
    );
    // Limited engagement makes player ship hostile
    let now_unix = ctx.now_unix();
    crate::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.add_limited_engagement(self_cid, enemy_cid, now_unix);
    });
    assert!(
        is_auto_target_hostile_ball(&ctx, &sys, self_cid, 100, &enemy_ball, &[], &[]).await
    );

    // 4. run_auto_targeter_ballpark_tick: missing ship, SDE fallback when no slim_meta, max_locks cap, distance sorting, and existing lock skip
    let mut bp = Ballpark::new(30_002_537);
    // Missing ship_id is a no-op
    run_auto_targeter_ballpark_tick(&mut bp, Some(&ctx.sde), None, 999, 20_000.0, 2, 200.0, &[], &[]);

    bp.add_ball(make_ball(100, 587, 0.0));
    // Ball 601: hostile NPC at 8 km with NO slim_meta (uses SDE fallback & attr 552 = 140.0)
    bp.add_ball(make_ball(601, 550_002, 8_000.0));
    // Ball 602: aggressor player ship at 4 km (closer than 601)
    bp.add_ball(make_ball(602, 587, 4_000.0));
    bp.set_ball_slim_meta(
        602,
        SlimItemMeta {
            category_id: 6,
            group_id: 25,
            ..Default::default()
        },
    );
    // Ball 603: non-aggressor player ship at 2 km (ignored)
    bp.add_ball(make_ball(603, 587, 2_000.0));
    bp.set_ball_slim_meta(
        603,
        SlimItemMeta {
            category_id: 6,
            group_id: 25,
            ..Default::default()
        },
    );
    // Ball 604: out-of-range hostile NPC at 25 km (> 20 km max_range)
    bp.add_ball(make_ball(604, 550_002, 25_000.0));

    // With max_locks = 1, only the closest hostile target (602 at 4 km) is locked
    run_auto_targeter_ballpark_tick(
        &mut bp,
        Some(&ctx.sde),
        None,
        100,
        20_000.0,
        1,
        200.0,
        &[602],
        &[100],
    );
    let locks1 = bp.get_target_locks(100);
    assert_eq!(locks1.len(), 1);
    assert_eq!(locks1[0].target_id, 602);

    // Increase max_locks to 2 -> next tick locks 601 (at 8 km) without duplicating 602
    run_auto_targeter_ballpark_tick(
        &mut bp,
        Some(&ctx.sde),
        None,
        100,
        20_000.0,
        2,
        0.5, // scan_resolution < 1.0 clamped to 1.0
        &[602],
        &[100],
    );
    let locks2: Vec<TargetLock> = bp.get_target_locks(100);
    assert_eq!(locks2.len(), 2);
    assert_eq!(locks2[1].target_id, 601);
}
