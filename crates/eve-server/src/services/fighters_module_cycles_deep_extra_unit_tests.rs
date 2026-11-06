//! Deep branch-coverage and mutation-killing unit tests for `combat_fighters` and `fitting_module_cycles`.

#![forbid(unsafe_code)]

use crate::router::{ActiveFighterSquadron, ServerContext};
use crate::services::combat_crimewatch::combat_fighters::{
    apply_damage_to_fighter_squadron, handle_fighters,
};
use crate::services::inventory_fitting::fitting_module_cycles::handle_module_cycles;
use eve_db::{
    InventoryFlag, InventoryItemRecord, ServerClock, VirtualServerClock, WorldRepository,
};
use eve_destiny::{Ball, MovementMode};
use eve_dogma::{ModuleState, ShipFittingEngine};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| {
        let base = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let p = [
            SdeRegistry::default_db_path(),
            base.join("../../data/evomu_sde.sqlite"),
            base.join("../../../../data/evomu_sde.sqlite"),
        ]
        .into_iter()
        .find(|cand| cand.exists())
        .unwrap_or_else(SdeRegistry::default_db_path);
        Arc::new(SdeRegistry::open(&p).expect("open default SDE"))
    }))
}

fn fast_test_context(base_unix: u64) -> (Arc<ServerContext>, Arc<VirtualServerClock>) {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = Arc::new(ServerContext::with_sde_and_repository(shared_sde(), db));
    let clock = Arc::new(VirtualServerClock::with_fixed_base(base_unix));
    ctx.set_clock(Arc::clone(&clock) as Arc<dyn ServerClock>);
    (ctx, clock)
}

#[tokio::test]
async fn combat_fighters_all_commands_error_guards_and_squadron_damage() {
    let base = 1_800_000_000_u64;
    let (ctx, _) = fast_test_context(base);
    let char_id = 90_000_001_i64;
    let ship_id = 100_999_001_i64;
    let sys_id = 30_000_142_u32;

    // 1. Squadron damage & total destruction
    let zero_hit = apply_damage_to_fighter_squadron(
        &ctx,
        sys_id,
        950_001,
        eve_destiny::DamageVector { em: 250.0, thermal: 0.0, kinetic: 0.0, explosive: 0.0 },
    ).await;
    assert_eq!(zero_hit, (0, 0, false));
    ctx.active_fighters.write().unwrap().insert(
        950_001,
        ActiveFighterSquadron {
            ball_id: 950_001, item_id: 950_002, type_id: 23055, owner_char_id: char_id,
            parent_ship_id: ship_id, tube_index: 0, squadron_size: 9, target_id: None,
            is_abandoned: false, mwd_active: false,
        },
    );
    ctx.fighter_tube_loads.write().unwrap().insert((ship_id, 0), 950_002);
    let partial = apply_damage_to_fighter_squadron(
        &ctx,
        sys_id,
        950_001,
        eve_destiny::DamageVector { em: 50.0, thermal: 0.0, kinetic: 0.0, explosive: 0.0 },
    ).await;
    assert_eq!(partial, (0, 9, false));
    let kill = apply_damage_to_fighter_squadron(
        &ctx,
        sys_id,
        950_001,
        eve_destiny::DamageVector { em: 100_000.0, thermal: 0.0, kinetic: 0.0, explosive: 0.0 },
    ).await;
    assert!(kill.2);
    assert_eq!(kill.1, 0);
    assert!(!ctx.active_fighters.read().unwrap().contains_key(&950_001));

    // 2. Unauthenticated & NoActiveShip & AccessDenied guards
    let mut sess = SessionState::new(8);
    assert_eq!(
        handle_fighters(&ctx, &mut sess, "fighterMgr", "GetFightersForShip", &[], &HashMap::new())
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "Unauthorized"
    );
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    assert_eq!(
        handle_fighters(&ctx, &mut sess, "fighterMgr", "GetFightersForShip", &[], &HashMap::new())
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NoActiveShip"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            777_001,
            InventoryItemRecord {
                item_id: 777_001, type_id: 587, owner_id: 90_000_002, location_id: sys_id as i64,
                flag: InventoryFlag::None, quantity: 1, singleton: true, version: 1,
                dynamic_attributes: HashMap::new(), source_mutaplasmid_id: None,
                insured_payout_isk: 0.0, is_blueprint_copy: false, blueprint_runs_remaining: None,
            },
        );
    });
    assert_eq!(
        handle_fighters(&ctx, &mut sess, "fighterMgr", "GetFightersForShip", &[PyRep::Integer(777_001)], &HashMap::new())
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "AccessDenied"
    );

    // 3. Upgrade active ship to Thanatos carrier with fighter tubes & test LoadFightersToTube guards + happy path
    let carrier_tid = ctx
        .sde
        .types
        .iter()
        .find(|(_, t)| {
            t.category_id == 6
                && t.default_attributes.get(&2216).copied().unwrap_or(0.0) >= 3.0
                && t.default_attributes.get(&2217).copied().unwrap_or(0.0) >= 2.0
        })
        .map(|(&tid, _)| tid)
        .unwrap_or(23911);
    let fighter_tid = ctx
        .sde
        .types
        .iter()
        .find(|(_, t)| t.category_id == 87 && t.default_attributes.get(&2217).copied().unwrap_or(0.0) > 0.0)
        .map(|(&tid, _)| tid)
        .unwrap_or(23055);
    let f_item_id = 950_100_i64;
    let foreign_item_id = 950_101_i64;
    let non_fighter_id = 950_102_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let ship_rec = snap.items.entry(ship_id).or_insert(InventoryItemRecord {
            item_id: ship_id, type_id: carrier_tid, owner_id: char_id, location_id: 60_003_760,
            flag: InventoryFlag::Hangar, quantity: 1, singleton: true, version: 1,
            dynamic_attributes: HashMap::new(), source_mutaplasmid_id: None,
            insured_payout_isk: 0.0, is_blueprint_copy: false, blueprint_runs_remaining: None,
        });
        ship_rec.type_id = carrier_tid;
        snap.items.insert(
            foreign_item_id,
            InventoryItemRecord {
                item_id: foreign_item_id, type_id: fighter_tid, owner_id: 90_000_002,
                location_id: ship_id, flag: InventoryFlag::FighterBay, quantity: 9, singleton: false,
                version: 1, dynamic_attributes: HashMap::new(), source_mutaplasmid_id: None,
                insured_payout_isk: 0.0, is_blueprint_copy: false, blueprint_runs_remaining: None,
            },
        );
        snap.items.insert(
            non_fighter_id,
            InventoryItemRecord {
                item_id: non_fighter_id, type_id: 34, owner_id: char_id,
                location_id: ship_id, flag: InventoryFlag::FighterBay, quantity: 100, singleton: false,
                version: 1, dynamic_attributes: HashMap::new(), source_mutaplasmid_id: None,
                insured_payout_isk: 0.0, is_blueprint_copy: false, blueprint_runs_remaining: None,
            },
        );
        snap.items.insert(
            f_item_id,
            InventoryItemRecord {
                item_id: f_item_id, type_id: fighter_tid, owner_id: char_id,
                location_id: ship_id, flag: InventoryFlag::Cargo, quantity: 9, singleton: false,
                version: 1, dynamic_attributes: HashMap::new(), source_mutaplasmid_id: None,
                insured_payout_isk: 0.0, is_blueprint_copy: false, blueprint_runs_remaining: None,
            },
        );
    });
    sess.ship_id = Some(ship_id);

    assert_eq!(
        handle_fighters(&ctx, &mut sess, "fighterMgr", "LoadFightersToTube", &[PyRep::Integer(f_item_id), PyRep::Integer(99)], &HashMap::new())
            .await.unwrap().unwrap_err().code,
        "InvalidFighterTube"
    );
    assert_eq!(
        handle_fighters(&ctx, &mut sess, "fighterMgr", "LoadFightersToTube", &[PyRep::Integer(999_999), PyRep::Integer(0)], &HashMap::new())
            .await.unwrap().unwrap_err().code,
        "ItemNotFound"
    );
    assert_eq!(
        handle_fighters(&ctx, &mut sess, "fighterMgr", "LoadFightersToTube", &[PyRep::Integer(foreign_item_id), PyRep::Integer(0)], &HashMap::new())
            .await.unwrap().unwrap_err().code,
        "OwnershipViolation"
    );
    assert_eq!(
        handle_fighters(&ctx, &mut sess, "fighterMgr", "LoadFightersToTube", &[PyRep::Integer(f_item_id), PyRep::Integer(0)], &HashMap::new())
            .await.unwrap().unwrap_err().code,
        "ItemNotInFighterBay"
    );
    assert_eq!(
        handle_fighters(&ctx, &mut sess, "fighterMgr", "LoadFightersToTube", &[PyRep::Integer(non_fighter_id), PyRep::Integer(0)], &HashMap::new())
            .await.unwrap().unwrap_err().code,
        "ItemNotAFighter"
    );

    // Move fighter into FighterBay -> Load into tube 0 -> try loading same fighter into tube 1 (FighterAlreadyLoadedInTube) -> Unload tube 0
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(it) = snap.items.get_mut(&f_item_id) {
            it.flag = InventoryFlag::FighterBay;
        }
    });
    let load_ok = handle_fighters(&ctx, &mut sess, "fighterMgr", "LoadFightersToTube", &[PyRep::Integer(f_item_id), PyRep::Integer(0)], &HashMap::new()).await.unwrap();
    assert!(load_ok.is_ok());
    assert_eq!(
        handle_fighters(&ctx, &mut sess, "fighterMgr", "LoadFightersToTube", &[PyRep::Integer(f_item_id), PyRep::Integer(1)], &HashMap::new())
            .await.unwrap().unwrap_err().code,
        "FighterAlreadyLoaded"
    );
    assert_eq!(
        handle_fighters(&ctx, &mut sess, "fighterMgr", "LaunchFightersFromTubes", &[PyRep::List(vec![PyRep::Integer(0)])], &HashMap::new())
            .await.unwrap().unwrap_err().code,
        "NoSolarSystem"
    );

    // 4. Launch guards (ShipNotInSpace, Warping, EmptyFighterTube, valid launch, ExecuteMovement, Abandon, Scoop)
    sess.solar_system_id = Some(sys_id);
    assert_eq!(
        handle_fighters(&ctx, &mut sess, "fighterMgr", "LaunchFightersFromTubes", &[PyRep::List(vec![PyRep::Integer(0)])], &HashMap::new())
            .await.unwrap().unwrap_err().code,
        "ShipNotInSpace"
    );

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle.spawn_ball_with_meta(
        Ball {
            id: ship_id, type_id: carrier_tid, position: DVec3::ZERO, velocity: DVec3::ZERO,
            max_velocity: 120.0, inertia: 1.0, mass: 1e8, radius: 400.0, speed_fraction: 1.0,
            mode: MovementMode::new_warp(DVec3::ZERO, DVec3::new(1e9, 0.0, 0.0), 1.5),
        },
        ctx.build_ship_combat_state(char_id, ship_id, carrier_tid),
        ctx.build_ship_slim_meta(char_id, carrier_tid),
    ).await;
    assert_eq!(
        handle_fighters(&ctx, &mut sess, "fighterMgr", "LaunchFightersFromTubes", &[PyRep::List(vec![PyRep::Integer(0)])], &HashMap::new())
            .await.unwrap().unwrap_err().code,
        "CannotOpenFighterTubesWhileWarping"
    );

    let _ = sys_handle.remove_ball(ship_id).await;
    let _ = sys_handle.spawn_ball_with_meta(
        Ball {
            id: ship_id, type_id: carrier_tid, position: DVec3::ZERO, velocity: DVec3::ZERO,
            max_velocity: 120.0, inertia: 1.0, mass: 1e8, radius: 400.0, speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        ctx.build_ship_combat_state(char_id, ship_id, carrier_tid),
        ctx.build_ship_slim_meta(char_id, carrier_tid),
    ).await;
    assert_eq!(
        handle_fighters(&ctx, &mut sess, "fighterMgr", "LaunchFightersFromTubes", &[PyRep::List(vec![PyRep::Integer(1)])], &HashMap::new())
            .await.unwrap().unwrap_err().code,
        "EmptyFighterTube"
    );

    let launch_res = handle_fighters(&ctx, &mut sess, "fighterMgr", "LaunchFightersFromTubes", &[PyRep::List(vec![PyRep::Integer(0)])], &HashMap::new()).await.unwrap();
    assert!(launch_res.is_ok());

    let spawned_fid = ctx.active_fighters.read().unwrap().keys().copied().next().unwrap();
    let mv_ok = handle_fighters(
        &ctx,
        &mut sess,
        "fighterMgr",
        "ExecuteMovementCommandOnFighters",
        &[PyRep::List(vec![PyRep::Integer(spawned_fid)]), PyRep::String("STOP".into())],
        &HashMap::new(),
    )
    .await
    .unwrap();
    assert!(mv_ok.is_ok());

    let ab_ok = handle_fighters(&ctx, &mut sess, "fighterMgr", "CmdAbandonFighter", &[PyRep::Integer(spawned_fid)], &HashMap::new()).await.unwrap();
    assert!(ab_ok.is_ok());
    let scoop_ok = handle_fighters(&ctx, &mut sess, "fighterMgr", "CmdScoopAbandonedFighterFromSpace", &[PyRep::Integer(spawned_fid)], &HashMap::new()).await.unwrap();
    assert!(scoop_ok.is_ok());
}

#[tokio::test]
async fn fitting_module_cycles_error_guards_and_activation_paths() {
    let base = 1_800_000_000_u64;
    let (ctx, clock) = fast_test_context(base);
    let char_id = 90_000_001_i64;
    let ship_id = 100_999_001_i64;
    let sys_id = 30_000_142_u32;

    let mut sess = SessionState::new(12);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.station_id = None;

    // 1. NoActiveShip & NoSolarSystem
    assert_eq!(
        handle_module_cycles(&ctx, &mut sess, None, "dogmaIM", "Activate", &[PyRep::Integer(990_001)], &HashMap::new())
            .await.unwrap().unwrap_err().code,
        "NoActiveShip"
    );
    sess.ship_id = Some(ship_id);
    assert_eq!(
        handle_module_cycles(&ctx, &mut sess, None, "dogmaIM", "Activate", &[PyRep::Integer(990_001)], &HashMap::new())
            .await.unwrap().unwrap_err().code,
        "NoSolarSystem"
    );

    // 2. Non-cycle dogmaIM method & non-dogmaIM service return None
    assert!(handle_module_cycles(&ctx, &mut sess, None, "dogmaIM", "GetAllInfo", &[], &HashMap::new()).await.is_none());
    assert!(handle_module_cycles(&ctx, &mut sess, None, "config", "GetStation", &[], &HashMap::new()).await.is_none());

    // 3. Seed ship fitting with Overdrive Injector (519), Cap Booster (3568, no charge), Survey Probe Launcher (30486, no probe)
    sess.solar_system_id = Some(sys_id);
    let mod_low_id = 960_001_i64;
    let cap_booster_id = 960_002_i64;
    let cloak_id = 960_003_i64;
    let bubble_id = 960_004_i64;
    let cap_booster_tid = ctx.sde.types.values().find(|t| t.group_id == 76).map(|t| t.type_id).unwrap_or(3568);
    let cloak_tid = 11370_u32;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = sys_id;
        }
        snap.items.entry(ship_id).or_insert(InventoryItemRecord {
            item_id: ship_id, type_id: 587, owner_id: char_id, location_id: i64::from(sys_id),
            flag: InventoryFlag::None, quantity: 1, singleton: true, version: 1,
            dynamic_attributes: HashMap::new(), source_mutaplasmid_id: None,
            insured_payout_isk: 0.0, is_blueprint_copy: false, blueprint_runs_remaining: None,
        });
        snap.items.insert(
            mod_low_id,
            InventoryItemRecord {
                item_id: mod_low_id, type_id: 439, owner_id: char_id, location_id: ship_id,
                flag: InventoryFlag::MedSlot2, quantity: 1, singleton: true, version: 1,
                dynamic_attributes: HashMap::new(), source_mutaplasmid_id: None,
                insured_payout_isk: 0.0, is_blueprint_copy: false, blueprint_runs_remaining: None,
            },
        );
        snap.module_online_states.insert(mod_low_id, false); // Initially offline -> ModuleOffline
        snap.items.insert(
            cap_booster_id,
            InventoryItemRecord {
                item_id: cap_booster_id, type_id: cap_booster_tid, owner_id: char_id, location_id: ship_id,
                flag: InventoryFlag::MedSlot0, quantity: 1, singleton: true, version: 1,
                dynamic_attributes: HashMap::new(), source_mutaplasmid_id: None,
                insured_payout_isk: 0.0, is_blueprint_copy: false, blueprint_runs_remaining: None,
            },
        );
        snap.module_online_states.insert(cap_booster_id, true);
        snap.items.insert(
            cloak_id,
            InventoryItemRecord {
                item_id: cloak_id, type_id: cloak_tid, owner_id: char_id, location_id: ship_id,
                flag: InventoryFlag::HiSlot0, quantity: 1, singleton: true, version: 1,
                dynamic_attributes: HashMap::new(), source_mutaplasmid_id: None,
                insured_payout_isk: 0.0, is_blueprint_copy: false, blueprint_runs_remaining: None,
            },
        );
        snap.module_online_states.insert(cloak_id, true);
        snap.items.insert(
            bubble_id,
            InventoryItemRecord {
                item_id: bubble_id, type_id: 22782, owner_id: char_id, location_id: ship_id,
                flag: InventoryFlag::HiSlot1, quantity: 1, singleton: true, version: 1,
                dynamic_attributes: HashMap::new(), source_mutaplasmid_id: None,
                insured_payout_isk: 0.0, is_blueprint_copy: false, blueprint_runs_remaining: None,
            },
        );
        snap.module_online_states.insert(bubble_id, true);
    });

    // Missing module -> ModuleNotFound
    assert_eq!(
        handle_module_cycles(&ctx, &mut sess, None, "dogmaIM", "Activate", &[PyRep::Integer(999_111)], &HashMap::new())
            .await.unwrap().unwrap_err().code,
        "ModuleNotFound"
    );

    // Module under nanite repair -> ModuleBeingRepaired
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    sys_h
        .start_nanite_repair(crate::system_actor::ActiveNaniteRepair {
            char_id,
            ship_id,
            module_id: mod_low_id,
            initial_damage_hp: 10.0,
            paste_total_needed: 1,
            paste_consumed_so_far: 0,
            duration_s: 30.0,
            elapsed_s: 0.0,
        })
        .await
        .unwrap();
    assert_eq!(
        handle_module_cycles(&ctx, &mut sess, None, "dogmaIM", "Activate", &[PyRep::Integer(mod_low_id)], &HashMap::new())
            .await.unwrap().unwrap_err().code,
        "ModuleBeingRepaired"
    );
    sys_h.cancel_ship_nanite_repairs_sync(ship_id, false);

    // Offline module -> ModuleOffline
    assert_eq!(
        handle_module_cycles(&ctx, &mut sess, None, "dogmaIM", "Activate", &[PyRep::Integer(mod_low_id)], &HashMap::new())
            .await.unwrap().unwrap_err().code,
        "ModuleOffline"
    );

    // Unloaded cap booster -> NoChargesLoaded; online active cloak while docked -> ShipDocked
    assert_eq!(
        handle_module_cycles(&ctx, &mut sess, None, "dogmaIM", "Activate", &[PyRep::Integer(cap_booster_id)], &HashMap::new())
            .await.unwrap().unwrap_err().code,
        "NoChargesLoaded"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(mod_low_id, true);
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(60_003_760);
        }
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.location_id = 60_003_760;
            sh.flag = InventoryFlag::Hangar;
        }
    });
    sess.station_id = Some(60_003_760);
    assert_eq!(
        handle_module_cycles(&ctx, &mut sess, None, "dogmaIM", "Activate", &[PyRep::Integer(bubble_id)], &HashMap::new())
            .await.unwrap().unwrap_err().code,
        "ShipDocked"
    );

    sess.station_id = None;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
        }
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.location_id = i64::from(sys_id);
            sh.flag = InventoryFlag::None;
        }
    });
    ctx.invalidate_ship_fitting(ship_id);
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle.spawn_ball_with_meta(
        Ball {
            id: ship_id, type_id: 587, position: DVec3::ZERO, velocity: DVec3::ZERO,
            max_velocity: 300.0, inertia: 2.0, mass: 1e6, radius: 40.0, speed_fraction: 1.0,
            mode: MovementMode::Warp {
                origin: DVec3::ZERO,
                destination: DVec3::new(1e9, 0.0, 0.0),
                warp_speed_au_s: 3.0,
                phase: eve_destiny::WarpPhase::Cruising,
                total_distance: 1e9,
                elapsed_warp_time: 10.0,
            },
        },
        ctx.build_ship_combat_state(char_id, ship_id, 587),
        ctx.build_ship_slim_meta(char_id, 587),
    ).await;
    assert_eq!(
        handle_module_cycles(&ctx, &mut sess, None, "dogmaIM", "Activate", &[PyRep::Integer(bubble_id)], &HashMap::new())
            .await.unwrap().unwrap_err().code,
        "CantActivateInWarp"
    );

    let _ = sys_handle.remove_ball(ship_id).await;
    let _ = sys_handle.spawn_ball_with_meta(
        Ball {
            id: ship_id, type_id: 587, position: DVec3::ZERO, velocity: DVec3::ZERO,
            max_velocity: 300.0, inertia: 2.0, mass: 1e6, radius: 40.0, speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        ctx.build_ship_combat_state(char_id, ship_id, 587),
        ctx.build_ship_slim_meta(char_id, 587),
    ).await;

    // Cloak inside Abyssal Deadspace (sys_id >= 32_000_000) -> CannotCloakInAbyssalSpace
    let abyss_sys = 32_000_001_u32;
    let _ = ctx.systems.get_or_spawn(abyss_sys);
    sess.solar_system_id = Some(abyss_sys);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = abyss_sys;
        }
    });
    assert_eq!(
        handle_module_cycles(&ctx, &mut sess, None, "dogmaIM", "Activate", &[PyRep::Integer(cloak_id)], &HashMap::new())
            .await.unwrap().unwrap_err().code,
        "CannotCloakInAbyssalSpace"
    );

    // Return to K-space: test reactivation delay block and unloaded Cap Booster -> NoChargesLoaded
    sess.solar_system_id = Some(sys_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
        }
    });
    ctx.record_module_reactivation_delay(ship_id, mod_low_id, 20.0);
    assert!(handle_module_cycles(&ctx, &mut sess, None, "dogmaIM", "Activate", &[PyRep::Integer(mod_low_id)], &HashMap::new())
        .await.unwrap().is_err());
    clock.advance_secs(21);

    // Ensure fitting has all modules cleanly loaded
    let mut fit = ShipFittingEngine::from_sde(ship_id, 587, &ctx.sde);
    let _ = fit.fit_module_with_state(cap_booster_id, cap_booster_tid, InventoryFlag::MedSlot0 as u16, true, &ctx.sde);
    let _ = fit.fit_module_with_state(mod_low_id, 439, InventoryFlag::MedSlot2 as u16, true, &ctx.sde);
    ctx.store_ship_fitting(ship_id, fit);

    assert_eq!(
        handle_module_cycles(&ctx, &mut sess, None, "dogmaIM", "Activate", &[PyRep::Integer(cap_booster_id)], &HashMap::new())
            .await.unwrap().unwrap_err().code,
        "NoChargesLoaded"
    );
    let act_ok = handle_module_cycles(&ctx, &mut sess, None, "dogmaIM", "Activate", &[PyRep::Integer(mod_low_id)], &HashMap::new())
        .await
        .unwrap();
    assert!(act_ok.is_ok());
    let _ = ModuleState::Online;
}
