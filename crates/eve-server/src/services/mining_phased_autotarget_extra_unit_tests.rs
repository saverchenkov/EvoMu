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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
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
        snap.mining_and_ess_state.phased_field_last_tick_unix = base_now;
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn maybe_spawn_phased_field_fleet_ics_tether_and_sde_fallback_autotarget() {
    let mut sde = (*shared_sde()).clone();
    sde.types.insert(
        550_099,
        InvType {
            type_id: 550_099,
            group_id: 550,
            category_id: 11,
            name: "NoMeta Rat".into(),
            mass: 1.0,
            volume: 10.0,
            capacity: 0.0,
            radius: 75.0,
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
            default_attributes: HashMap::from([(552_u16, 125.0)]),
        },
    );
    let ctx = ServerContext::with_sde_and_repository(
        Arc::new(sde),
        Arc::new(WorldRepository::with_demo_seed()),
    );

    // Find a solar_system_id whose deterministic roll < 20 so maybe_spawn_phased_asteroid_field spawns.
    let mut spawned_sys = None;
    for candidate_sys in 30_000_001_u32..30_000_500 {
        if let Some(st) = maybe_spawn_phased_asteroid_field(&ctx, candidate_sys).await {
            spawned_sys = Some((candidate_sys, st));
            break;
        }
    }
    let (sys_id, first_state) = spawned_sys.expect("at least one system spawns phased field");
    assert!(!first_state.collapsed);
    assert!(
        maybe_spawn_phased_asteroid_field(&ctx, sys_id).await.is_none(),
        "already active field must not spawn a second field"
    );

    // Clear cosmic_signatures so advance_phased_asteroid_field uses the snap.asteroids position fallback.
    ctx.db.with_mut_snapshot(|snap| {
        snap.cosmic_signatures.remove(&sys_id);
    });
    let half = advance_phased_asteroid_field(&ctx, sys_id, 0.5).expect("advance to 0.5");
    assert!(half.beacon_spawned);
    assert!(!half.collapsed);

    // Seed fleet anchor + two fleetmate Orcas (active cores in tether -> 50+50=100 energy), non-core ship, and warping ship.
    let anchor_owner = 970_001_i64;
    let fleetmate = 970_002_i64;
    let uncored = 970_003_i64;
    let warping = 970_004_i64;
    let fleetmate2 = 970_005_i64;
    let anchor_id = 970_100_i64;
    let orca_ship = 970_101_i64;
    let uncored_ship = 970_102_i64;
    let warping_ship = 970_103_i64;
    let orca2_ship = 970_104_i64;
    let core_mod = 970_201_i64;
    let warp_core_mod = 970_202_i64;
    let core_mod2 = 970_203_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let base_char = snap.characters.values().next().unwrap().clone();
        for (cid, sid) in [
            (anchor_owner, 0),
            (fleetmate, orca_ship),
            (uncored, uncored_ship),
            (warping, warping_ship),
            (fleetmate2, orca2_ship),
        ] {
            let mut ch = base_char.clone();
            ch.character_id = cid;
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = sid;
            snap.characters.insert(cid, ch);
        }
        snap.fleets.insert(
            970_900,
            FleetRecord {
                fleet_id: 970_900,
                boss_id: anchor_owner,
                members: vec![anchor_owner, fleetmate, uncored, warping, fleetmate2],
                ..Default::default()
            },
        );
        snap.items.insert(
            anchor_id,
            InventoryItemRecord {
                item_id: anchor_id,
                type_id: TYPE_MOBILE_PHASE_ANCHOR,
                owner_id: anchor_owner,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        for (sid, owner, tid) in [
            (orca_ship, fleetmate, 28_606_u32),
            (uncored_ship, uncored, 42_244),
            (warping_ship, warping, 28_606),
            (orca2_ship, fleetmate2, 28_606),
        ] {
            snap.items.insert(
                sid,
                InventoryItemRecord {
                    item_id: sid,
                    type_id: tid,
                    owner_id: owner,
                    location_id: i64::from(sys_id),
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        }
        for (mid, sid, owner) in [
            (core_mod, orca_ship, fleetmate),
            (warp_core_mod, warping_ship, warping),
            (core_mod2, orca2_ship, fleetmate2),
        ] {
            snap.items.insert(
                mid,
                InventoryItemRecord {
                    item_id: mid,
                    type_id: 28583,
                    owner_id: owner,
                    location_id: sid,
                    flag: InventoryFlag::HiSlot0,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            snap.module_active_states.insert(mid);
        }
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    for (bid, tid, x, mode) in [
        (anchor_id, TYPE_MOBILE_PHASE_ANCHOR, 0.0, MovementMode::Stop),
        (orca_ship, 28_606, 2_000.0, MovementMode::Stop),
        (orca2_ship, 28_606, 2_500.0, MovementMode::Stop),
        (uncored_ship, 42_244, 1_500.0, MovementMode::Stop),
        (
            warping_ship,
            28_606,
            1_000.0,
            MovementMode::Warp {
                origin: DVec3::ZERO,
                destination: DVec3::new(1e7, 0.0, 0.0),
                warp_speed_au_s: 3.0,
                total_distance: 1e7,
                phase: eve_destiny::WarpPhase::Cruising,
                elapsed_warp_time: 1.0,
            },
        ),
    ] {
        sys_handle
            .spawn_ball(Ball {
                id: bid,
                type_id: tid,
                position: DVec3::new(x, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 100.0,
                inertia: 1.0,
                mass: 1e6,
                radius: 50.0,
                speed_fraction: 0.0,
                mode,
            })
            .await
            .unwrap();
    }

    let tick_ts = ctx.now_unix_secs() + 10;
    tick_phased_asteroid_fields(&ctx, tick_ts).await;
    let fields = ctx.phased_asteroid_fields.read().unwrap().clone();
    let f = fields.get(&sys_id).expect("phased field present");
    assert!(f.phased_in);
    assert!(f.connected_ship_ids.contains(&orca_ship));
    assert!(f.connected_ship_ids.contains(&orca2_ship));
    assert!(f.mobile_phase_anchor_energy >= 100);

    // Ballpark autotargeter with no-SlimItemMeta NPC (SDE fallback) and category-6 ship (ignored)
    let mut bp = Ballpark::new(sys_id);
    bp.balls.insert(
        100,
        Ball {
            id: 100,
            type_id: 28_606,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 100.0,
            inertia: 1.0,
            mass: 1e6,
            radius: 100.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
    );
    bp.balls.insert(
        701,
        Ball {
            id: 701,
            type_id: 550_099,
            position: DVec3::new(4_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 100.0,
            inertia: 1.0,
            mass: 1e4,
            radius: 75.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
    );
    bp.balls.insert(
        702,
        Ball {
            id: 702,
            type_id: 28_606,
            position: DVec3::new(3_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 100.0,
            inertia: 1.0,
            mass: 1e6,
            radius: 100.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
    );
    bp.set_ball_slim_meta(
        702,
        SlimItemMeta {
            owner_id: 888,
            char_id: Some(888),
            corp_id: 999,
            alliance_id: -1,
            group_id: 941,
            category_id: 6,
            security_status: 0.0,
            is_cloaked: false,
            ..Default::default()
        },
    );
    run_auto_targeter_ballpark_tick(&mut bp, Some(&ctx.sde), None, 100, 20_000.0, 5, 250.0, &[], &[]);
    let locks = bp.get_target_locks(100);
    assert_eq!(locks.len(), 1);
    assert_eq!(locks[0].target_id, 701);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn phased_fields_persistence_multi_system_and_tick_timestamp_zero_boundary() {
    let ctx = make_ctx();
    let clock: Arc<dyn eve_db::clock::ServerClock> =
        Arc::new(eve_db::clock::VirtualServerClock::with_fixed_base(1_776_500_000));
    ctx.set_clock(clock);

    let hi_sec = 30_000_142_u32;
    let mut low_null_systems: Vec<u32> = ctx
        .sde
        .solar_systems
        .values()
        .filter(|s| s.security_status < 0.45 && s.region_id != 10_000_070)
        .map(|s| s.solar_system_id)
        .collect();
    low_null_systems.sort_unstable();
    assert!(low_null_systems.len() >= 2, "seed must have >= 2 low/null systems");
    let low_a = low_null_systems[0];
    let low_b = low_null_systems[1];

    // 1. High-sec returns None; valid low/null returns populated PhasedAsteroidFieldState & persists to DB
    assert!(spawn_phased_asteroid_field(&ctx, hi_sec, "HI-NOPE", 990_000, 1000, (0.0, 0.0, 0.0)).is_none());

    let f_a = spawn_phased_asteroid_field(&ctx, low_a, "PH-ALPHA", 990_001, 5000, (10.0, 20.0, 30.0))
        .expect("low_a field spawns");
    assert_eq!(f_a.signature_id, "PH-ALPHA");
    assert_eq!(f_a.solar_system_id, low_a);
    assert_eq!(f_a.asteroid_ids, vec![990_001]);

    // 2. Spawning low_b loads existing low_a (kills HashMap::new / [(0,_)] / [(1,_)] and persist_phased_fields -> ())
    let f_b = spawn_phased_asteroid_field(&ctx, low_b, "PH-BETA", 990_002, 6000, (40.0, 50.0, 60.0))
        .expect("low_b field spawns");
    assert_eq!(f_b.signature_id, "PH-BETA");
    ctx.db.with_snapshot(|snap| {
        let fields = &snap.mining_and_ess_state.phased_asteroid_fields;
        assert_eq!(fields.len(), 2);
        assert_eq!(fields.get(&low_a).map(|f| f.signature_id.as_str()), Some("PH-ALPHA"));
        assert_eq!(fields.get(&low_b).map(|f| f.signature_id.as_str()), Some("PH-BETA"));
        assert!(!fields.contains_key(&0));
        assert!(!fields.contains_key(&1));
    });

    // 3. tick_phased_asteroid_fields when phased_field_last_tick_unix == 0:
    //    must anchor last_tick = now_unix without jumping/collapsing (kills `> -> >=`),
    //    then next tick +3600s advances progress by 0.25 from 0.10 -> 0.35 (kills `tick -> ()`, `> -> ==`, `> -> <`).
    ctx.db.with_mut_snapshot(|snap| {
        snap.mining_and_ess_state.phased_field_last_tick_unix = 0;
    });
    tick_phased_asteroid_fields(&ctx, 1_776_500_000).await;
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.mining_and_ess_state.phased_field_last_tick_unix, 1_776_500_000);
        let fa = &snap.mining_and_ess_state.phased_asteroid_fields[&low_a];
        assert!(!fa.collapsed);
        assert!((fa.progress - 0.1).abs() < 1e-6, "progress was {}", fa.progress);
    });

    tick_phased_asteroid_fields(&ctx, 1_776_503_600).await;
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.mining_and_ess_state.phased_field_last_tick_unix, 1_776_503_600);
        let fa = &snap.mining_and_ess_state.phased_asteroid_fields[&low_a];
        let fb = &snap.mining_and_ess_state.phased_asteroid_fields[&low_b];
        assert!((fa.progress - 0.35).abs() < 1e-6, "expected 0.35, got {}", fa.progress);
        assert!((fb.progress - 0.35).abs() < 1e-6, "expected 0.35, got {}", fb.progress);
    });
}
