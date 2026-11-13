//! Unit and mutation-killing tests for `router::context`.

use crate::router::context::{ActiveMoonSurveyProbe, ServerContext};
use eve_db::{
    AsteroidRecord, CosmicSignatureRecord, FleetRole, InventoryFlag, InventoryItemRecord,
    MoonExtractionRecord, PersistedSpaceObjectMeta, ServerClock, TrainedSkillRecord,
    VirtualServerClock, WorldRepository,
};
use eve_dogma::{DogmaItemState, ProjectedEwarEffect};
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

fn make_item(
    item_id: i64,
    type_id: u32,
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
        singleton: quantity == 1 && flag == InventoryFlag::None,
        version: 1,
        dynamic_attributes: HashMap::new(),
        source_mutaplasmid_id: None,
        insured_payout_isk: 0.0,
        is_blueprint_copy: false,
        blueprint_runs_remaining: None,
    }
}

#[test]
fn signature_scan_tracking_and_effective_signal_strength() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let sys_id = 30_000_142_u32;
    let sig_ball_id = 888_001_i64;

    assert!(!ctx.has_scanned_signature(char_id, sys_id, sig_ball_id));
    ctx.mark_signature_scanned(char_id, sys_id, sig_ball_id);
    assert!(ctx.has_scanned_signature(char_id, sys_id, sig_ball_id));
    // Calling a second time is idempotent
    ctx.mark_signature_scanned(char_id, sys_id, sig_ball_id);
    assert!(ctx.has_scanned_signature(char_id, sys_id, sig_ball_id));
    assert!(!ctx.has_scanned_signature(char_id, sys_id, sig_ball_id + 1));

    let sig = CosmicSignatureRecord {
        signature_id: "ABC-123".to_string(),
        solar_system_id: sys_id,
        sig_group: "Relic Site".to_string(),
        type_name: "Crumbling Sansha".to_string(),
        position: [1_000.0, 2_000.0, 3_000.0],
        signal_strength: 0.15,
        wormhole_type_id: None,
        destination_system_id: None,
        created_unix: 1_700_000_000,
        lifetime_seconds: 86_400,
        max_mass_kg: 0.0,
        remaining_mass_kg: 0.0,
        max_jump_mass_kg: 0.0,
        collapsed: false,
    };

    assert_eq!(ctx.get_character_scan_progress(char_id, sys_id, "ABC-123"), None);
    assert!((ctx.effective_signature_signal_strength(char_id, sys_id, &sig) - 0.15).abs() < 1e-9);

    ctx.record_character_scan_progress(char_id, sys_id, "ABC-123", 0.85);
    assert_eq!(ctx.get_character_scan_progress(char_id, sys_id, "ABC-123"), Some(0.85));
    assert!((ctx.effective_signature_signal_strength(char_id, sys_id, &sig) - 0.85).abs() < 1e-9);
}

#[test]
fn character_skill_levels_respects_alpha_cap_and_omega() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    // Skill 3340 (Destroyers) or any skill with alpha cap < 5
    let skill_tid = 3340_u32;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.clone_grade = 2; // Omega
        }
        snap.skills.entry(char_id).or_default().insert(
            skill_tid,
            TrainedSkillRecord {
                type_id: skill_tid,
                level: 5,
                skill_points: 512_000.0,
            },
        );
    });
    let omega_levels = ctx.character_skill_levels(char_id);
    assert_eq!(omega_levels.get(&skill_tid), Some(&5));

    // Switch to Alpha (clone_grade = 1) and test a skill with alpha cap 0 (e.g. Titan / Capital Ships 20533)
    let cap_skill_tid = 20_533_u32;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.clone_grade = 1;
        }
        snap.skills.entry(char_id).or_default().insert(
            cap_skill_tid,
            TrainedSkillRecord {
                type_id: cap_skill_tid,
                level: 5,
                skill_points: 2_048_000.0,
            },
        );
    });
    let alpha_levels = ctx.character_skill_levels(char_id);
    let alpha_cap = crate::services::character_skills::alpha_skill_cap_for_character(
        &ctx.sde,
        1,
        cap_skill_tid,
    );
    assert_eq!(alpha_levels.get(&cap_skill_tid).copied(), Some(5.min(alpha_cap)));
}

#[test]
fn module_reactivation_delay_lifecycle_and_clock_controls() {
    let ctx = test_ctx();
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    let ship_id = 930_000_001_i64;
    let mod_id = 930_000_002_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            make_item(ship_id, 587, 90_000_001, 30_000_142, InventoryFlag::None, 1),
        );
        snap.items.insert(
            mod_id,
            make_item(mod_id, 440, 90_000_001, ship_id, InventoryFlag::MedSlot0, 1),
        );
    });

    assert!(!ctx.is_module_in_reactivation_delay(ship_id, mod_id));
    // Non-positive delay is ignored
    ctx.record_module_reactivation_delay(ship_id, mod_id, 0.0);
    ctx.record_module_reactivation_delay(ship_id, mod_id, -5.0);
    assert!(!ctx.is_module_in_reactivation_delay(ship_id, mod_id));

    // Record 10s delay
    ctx.record_module_reactivation_delay(ship_id, mod_id, 10.0);
    assert!(ctx.is_module_in_reactivation_delay(ship_id, mod_id));

    // Recording a shorter delay (3s) keeps the longer remaining cooldown (10s)
    ctx.record_module_reactivation_delay(ship_id, mod_id, 3.0);
    ctx.clock().advance_secs(5);
    assert!(ctx.is_module_in_reactivation_delay(ship_id, mod_id));

    // Advancing another 6s (total 11s > 10s) clears the delay
    ctx.clock().advance_secs(6);
    assert!(!ctx.is_module_in_reactivation_delay(ship_id, mod_id));

    // Clock injection and multiplier/pause controls
    let custom_clock = Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000));
    let base_now = custom_clock.now_unix_secs();
    ctx.set_clock(custom_clock);
    assert_eq!(ctx.now_unix_secs(), base_now);
    assert_eq!(ctx.now_unix(), base_now);
    assert!(ctx.now_blueos_filetime_ticks() > 0);
    assert_eq!(
        ctx.db.with_snapshot(|s| s.service_state_blobs.get("__server_now_unix").cloned()),
        Some(base_now.to_string())
    );

    ctx.set_time_acceleration_multiplier(4);
    assert_eq!(ctx.time_acceleration_multiplier(), 4);

    assert!(!ctx.is_clock_paused());
    ctx.set_clock_paused(true);
    assert!(ctx.is_clock_paused());
    assert_eq!(ctx.step_clock_tick(5), 0);
    ctx.set_clock_paused(false);
    assert_eq!(ctx.step_clock_tick(5), 20);
    assert_eq!(ctx.now_unix_secs(), base_now + 20);

    assert!(!ctx.pause_on_tick());
    ctx.set_pause_on_tick(true);
    assert!(ctx.pause_on_tick());
    ctx.set_pause_on_tick(false);
    assert!(!ctx.pause_on_tick());

    ctx.advance_virtual_clock(10);
    assert_eq!(ctx.now_unix_secs(), base_now + 30);
    ctx.advance_time_seconds(15);
    assert_eq!(ctx.now_unix_secs(), base_now + 45);
}

#[tokio::test]
async fn advance_secs_expires_jetcans_moon_extractions_and_delivers_moon_probes() {
    let ctx = test_ctx();
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    let char_id = 90_000_001_i64;
    let sys_id = 30_000_142_u32;
    let now = ctx.now_unix_secs();

    let jetcan_id = 940_000_001_i64;
    let inside_item_id = 940_000_002_i64;
    let fresh_jetcan_id = 940_000_003_i64;
    let ast_id = 940_000_010_i64;
    let struct_id = 940_000_020_i64;

    // Pick a valid moon in system 30000142 for non-empty moon ore composition
    let valid_moon_id = ctx
        .sde
        .celestials_in_system(sys_id)
        .iter()
        .find(|c| c.group_id == 8)
        .map(|c| c.celestial_id as i64)
        .unwrap_or(40_009_049);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
        }
        // Jetcan created at `now`: expires at `now + 3600`
        snap.items.insert(
            jetcan_id,
            make_item(jetcan_id, 23, char_id, sys_id as i64, InventoryFlag::None, 1),
        );
        snap.items.insert(
            inside_item_id,
            make_item(inside_item_id, 34, char_id, jetcan_id, InventoryFlag::None, 100),
        );
        snap.sp6_lifecycle.space_object_meta.insert(
            jetcan_id,
            PersistedSpaceObjectMeta {
                created_unix: Some(now),
                ..PersistedSpaceObjectMeta::default()
            },
        );
        // Fresh jetcan created 100s in the future relative to `now`
        snap.items.insert(
            fresh_jetcan_id,
            make_item(fresh_jetcan_id, 23, char_id, sys_id as i64, InventoryFlag::None, 1),
        );
        snap.sp6_lifecycle.space_object_meta.insert(
            fresh_jetcan_id,
            PersistedSpaceObjectMeta {
                created_unix: Some(now + 100),
                ..PersistedSpaceObjectMeta::default()
            },
        );

        // Moon extraction due to decay at `now + 3600`
        snap.asteroids.insert(
            ast_id,
            AsteroidRecord {
                asteroid_id: ast_id,
                solar_system_id: sys_id,
                belt_id: valid_moon_id,
                type_id: 45_490,
                quantity: 5_000,
                initial_quantity: 5_000,
                position: (1_000.0, 0.0, 0.0),
                radius: 500.0,
            },
        );
        snap.moon_extractions.insert(
            struct_id,
            MoonExtractionRecord {
                structure_id: struct_id,
                moon_id: valid_moon_id,
                extraction_start_unix: now.saturating_sub(10_000),
                chunk_arrival_unix: now.saturating_sub(5_000),
                natural_decay_unix: now + 3600,
                status: "Fractured".to_string(),
                auto_fracture: false,
                event_id: 1,
                moon_ores: vec![(45_490, 1.0)],
                spawned_asteroid_ids: vec![ast_id],
            },
        );
    });

    // Register two moon probes: one hitting valid_moon_id (arrives at now + 3600) and one kept in flight
    {
        let mut probes = ctx.active_moon_probes.write().unwrap();
        probes.push(ActiveMoonSurveyProbe {
            probe_id: 7001,
            probe_type_id: 47_908,
            owner_char_id: char_id,
            solar_system_id: sys_id,
            hit_moon_id: Some(valid_moon_id),
            arrival_unix_secs: now + 3600,
        });
        probes.push(ActiveMoonSurveyProbe {
            probe_id: 7002,
            probe_type_id: 47_908,
            owner_char_id: char_id,
            solar_system_id: sys_id,
            hit_moon_id: Some(999_999_999),
            arrival_unix_secs: now + 7200,
        });
    }

    // At +3599s, neither jetcan nor moon extraction nor probe 7001 has expired/arrived
    ctx.advance_secs(3599);
    ctx.db.with_snapshot(|snap| {
        assert!(snap.items.contains_key(&jetcan_id));
        assert!(snap.items.contains_key(&inside_item_id));
        assert_eq!(snap.moon_extractions[&struct_id].status, "Fractured");
    });
    assert_eq!(ctx.active_moon_probes.read().unwrap().len(), 2);

    // At +1s (total +3600s), jetcan_id and inside_item_id despawn, fresh_jetcan_id remains,
    // moon extraction decays, and probe 7001 arrives while probe 7002 stays in flight
    ctx.advance_secs(1);
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&jetcan_id));
        assert!(!snap.items.contains_key(&inside_item_id));
        assert!(!snap.sp6_lifecycle.space_object_meta.contains_key(&jetcan_id));
        assert!(snap.items.contains_key(&fresh_jetcan_id));
        assert_eq!(snap.moon_extractions[&struct_id].status, "Decayed");
        assert!(snap.moon_extractions[&struct_id].spawned_asteroid_ids.is_empty());
        assert!(!snap.asteroids.contains_key(&ast_id));
    });
    assert_eq!(ctx.active_moon_probes.read().unwrap().len(), 1);
}

#[test]
fn ship_fitting_cache_rebuild_and_has_fleet_extras_branches() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let ship_id = 950_000_001_i64;
    let mod_id = 950_000_002_i64;
    let drone_id = 950_000_003_i64;

    // Missing ship returns ShipNotFound
    let err = ctx
        .get_or_rebuild_ship_fitting(char_id, 999_999_888)
        .expect_err("unknown ship must fail");
    assert_eq!(err.code, "ShipNotFound");

    ctx.db.with_mut_snapshot(|snap| {
        let mut ship = make_item(ship_id, 587, char_id, 60_003_760, InventoryFlag::Hangar, 1);
        ship.dynamic_attributes.insert(eve_sde::attr_ids::MASS, 1_234_567.0);
        snap.items.insert(ship_id, ship);

        let mut m = make_item(mod_id, 440, char_id, ship_id, InventoryFlag::MedSlot0, 1);
        m.dynamic_attributes.insert(eve_sde::attr_ids::DAMAGE, 12.5);
        m.dynamic_attributes.insert(eve_sde::attr_ids::HP_STRUCT, 55.0);
        m.dynamic_attributes.insert(eve_sde::attr_ids::CPU, 5.0);
        snap.items.insert(mod_id, m);
        snap.module_online_states.insert(mod_id, true);
        snap.module_active_states.insert(mod_id);
        snap.module_active_charges.insert(mod_id, 34);

        // Owned drone in DroneBay (category 18, e.g. Warrior I = 2488)
        snap.items.insert(
            drone_id,
            make_item(drone_id, 2488, char_id, ship_id, InventoryFlag::DroneBay, 1),
        );
        for tid in [587_u32, 440_u32] {
            for (sk_tid, _) in ctx.sde.required_skills_for_type(tid) {
                snap.skills.entry(char_id).or_default().insert(
                    sk_tid,
                    TrainedSkillRecord {
                        type_id: sk_tid,
                        level: 5,
                        skill_points: 256_000.0,
                    },
                );
            }
        }
    });

    let fit = ctx
        .get_or_rebuild_ship_fitting(char_id, ship_id)
        .expect("rebuild ship fitting must succeed");
    assert!((fit.ship.base_attributes[&eve_sde::attr_ids::MASS] - 1_234_567.0).abs() < 1e-6);
    let fitted_mod = fit.modules.get(&mod_id).expect("fitted module present");
    assert!((fitted_mod.damage_hp - 12.5).abs() < 1e-6);
    assert!((fitted_mod.max_hp - 55.0).abs() < 1e-6);
    assert_eq!(fitted_mod.charge_type_id, Some(34));
    assert!(fit.drones.contains(&(drone_id, 2488)));

    // Cached lookup and recalculation when projected_ewar changes
    let ewar_mod = DogmaItemState::from_sde(888_888, 526, &ctx.sde);
    ctx.projected_ewar.write().unwrap().insert(
        ship_id,
        vec![ProjectedEwarEffect {
            source_module_id: 888_888,
            source_module_type_id: 526,
            charge_type_id: None,
            distance: 5_000.0,
            overloaded: false,
            effective_module: ewar_mod,
        }],
    );
    let fit2 = ctx
        .get_or_rebuild_ship_fitting(char_id, ship_id)
        .expect("cached fitting recalculation must succeed");
    assert_eq!(fit2.projected_ewar.len(), 1);

    ctx.invalidate_ship_fitting(ship_id);
    assert!(!ctx.ship_fittings.read().unwrap().contains_key(&ship_id));
    assert!(ctx.current_fit_rev(ship_id).0 >= 1);

    // Checked `ctx.fit` verifies pilot/owner even on a warm cache hit (`#861`), while
    // allowing an active pilot (`active_ship_id == ship_id`).
    let _warm = ctx.fit(char_id, ship_id).expect("owner fit must succeed");
    assert!(ctx.ship_fittings.read().unwrap().contains_key(&ship_id));
    let other_char = 90_000_999_i64;
    let denied = ctx
        .fit(other_char, ship_id)
        .expect_err("non-owner non-pilot must be rejected even on warm cache hit");
    assert_eq!(denied.code, "ShipNotFound");

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.insert(
            other_char,
            eve_db::CharacterRecord {
                active_ship_id: ship_id,
                ..snap.characters[&char_id].clone()
            },
        );
    });
    assert!(
        ctx.fit(other_char, ship_id).is_ok(),
        "active pilot of the ship must pass ctx.fit check"
    );
    let r1 = ctx.bump_fit_rev(ship_id);
    let r2 = ctx.bump_fit_rev(ship_id);
    assert!(r2 > r1);

    // Exercise every branch of has_fleet_extras_for
    let fid = 777_001_i64;
    assert!(!ctx.has_fleet_extras_for(fid));

    ctx.fleet_extras.lock().unwrap().target_tags.insert(fid, HashMap::new());
    assert!(ctx.has_fleet_extras_for(fid));
    ctx.fleet_extras.lock().unwrap().target_tags.clear();

    ctx.fleet_extras.lock().unwrap().watchlist.insert(fid, HashSet::new());
    assert!(ctx.has_fleet_extras_for(fid));
    ctx.fleet_extras.lock().unwrap().watchlist.clear();

    ctx.fleet_extras.lock().unwrap().options.insert(fid, PyRep::None);
    assert!(ctx.has_fleet_extras_for(fid));
    ctx.fleet_extras.lock().unwrap().options.clear();

    ctx.fleet_extras.lock().unwrap().wing_squad_hierarchy.insert(fid, (vec![], vec![]));
    assert!(ctx.has_fleet_extras_for(fid));
    ctx.fleet_extras.lock().unwrap().wing_squad_hierarchy.clear();

    ctx.fleet_extras.lock().unwrap().fleet_names.insert(fid, "Alpha".into());
    assert!(ctx.has_fleet_extras_for(fid));
    ctx.fleet_extras.lock().unwrap().fleet_names.clear();

    ctx.fleet_extras.lock().unwrap().max_sizes.insert(fid, 50);
    assert!(ctx.has_fleet_extras_for(fid));
    ctx.fleet_extras.lock().unwrap().max_sizes.clear();

    ctx.fleet_extras
        .lock()
        .unwrap()
        .invite_details
        .insert((fid, char_id), (0, 0, FleetRole::SquadMember, char_id));
    assert!(ctx.has_fleet_extras_for(fid));
    ctx.fleet_extras.lock().unwrap().invite_details.clear();

    ctx.fleet_extras.lock().unwrap().invite_timestamps.insert((fid, char_id), 100);
    assert!(ctx.has_fleet_extras_for(fid));
    ctx.fleet_extras.lock().unwrap().invite_timestamps.clear();
    assert!(!ctx.has_fleet_extras_for(fid));
}

#[test]
fn advance_skill_queues_emits_server_skills_changed_and_queue_saved_and_advance_secs_syncs_structures() {
    let ctx = test_ctx();
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    let char_id = 90_000_001_i64;
    let skill_tid = 3402_u32;
    let now = ctx.now_unix_secs();

    ctx.db.acquire_online_lock(1, 501, Some(char_id), true, now).unwrap();

    // Seed Science I in skill queue + an Onlining Upwell structure whose timer expires in +300s
    let struct_id = 1_000_000_902_001_i64;
    let corp_id = ctx.db.with_snapshot(|s| s.characters[&char_id].corporation_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.clone_grade = 2;
            ch.skill_points = 0.0;
        }
        snap.skill_queues.insert(
            char_id,
            vec![eve_db::SkillQueueEntry {
                type_id: skill_tid,
                target_level: 1,
                start_sp: 0.0,
                end_sp: 250.0,
                duration_seconds: 500.0,
                started_at_unix: Some(now),
            }],
        );
        snap.structures.insert(
            struct_id,
            eve_db::UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35_832,
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142,
                name: "Test Astrahus".into(),
                position: [1_000_000.0, 0.0, 0.0],
                state: "Onlining".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 10_000.0,
                armor_hp: 10_000.0,
                hull_hp: 10_000.0,
                reinforced_until_unix: now + 300,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
    });
    let _ = ctx.drain_notifications_for_char(char_id);

    // Partial advance (5s): does NOT finish skill level, so OnServerSkillsChanged/OnSkillTrained are not emitted
    ctx.advance_skill_queues(5.0);
    let notifs_partial = ctx.drain_notifications_for_char(char_id);
    assert!(!notifs_partial.iter().any(|n| n.event == "OnSkillTrained"));
    assert!(!notifs_partial.iter().any(|n| n.event == "OnServerSkillsChanged"));
    assert!(!notifs_partial.iter().any(|n| n.event == "OnNewSkillQueueSaved"));

    // Advance via advance_secs(3_600): completes Science I AND transitions Onlining structure to ShieldVulnerable
    ctx.advance_secs(3_600);
    let notifs_done = ctx.drain_notifications_for_char(char_id);
    assert!(notifs_done.iter().any(|n| n.event == "OnSkillTrained"));
    let skills_changed = notifs_done
        .iter()
        .find(|n| n.event == "OnServerSkillsChanged")
        .expect("OnServerSkillsChanged emitted when skill completes");
    assert_eq!(
        skills_changed.args.get(1),
        Some(&PyRep::String("SkillTrained".into()))
    );
    assert!(notifs_done.iter().any(|n| n.event == "OnNewSkillQueueSaved"));

    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.structures[&struct_id].state, "ShieldVulnerable");
        assert_eq!(snap.skills[&char_id][&skill_tid].level, 1);
    });
}

#[tokio::test]
async fn unified_clock_tick_pipeline_syncs_ballpark_stats_and_preserves_ice_belts() {
    let ctx = test_ctx();
    let base_now = 1_800_000_000_u64;
    let clock = Arc::new(VirtualServerClock::with_fixed_base(base_now));
    ctx.set_clock(clock.clone());

    let char_id = 90_000_001_i64;
    let sys_id = 30_000_142_u32;
    let ship_id = 100_000_777_i64;
    let corp_id = ctx.db.with_snapshot(|s| s.characters[&char_id].corporation_id);

    ctx.db.acquire_online_lock(1, 501, Some(char_id), true, base_now).unwrap();
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    sys_h
        .spawn_ball(eve_destiny::Ball {
            id: ship_id,
            type_id: 587,
            position: glam::DVec3::ZERO,
            velocity: glam::DVec3::ZERO,
            max_velocity: 123.0,
            inertia: 2.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await
        .unwrap();
    let _ = sys_h
        .set_combat_state(
            ship_id,
            eve_destiny::CombatState {
                shield_hp: 1.0,
                max_shield_hp: 1.0,
                armor_hp: 1.0,
                max_armor_hp: 1.0,
                structure_hp: 1.0,
                max_structure_hp: 1.0,
                capacitor: 1.0,
                max_capacitor: 1.0,
                ..Default::default()
            },
        )
        .await;

    let now_ft = ctx.now_blueos_filetime_ticks();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(
            ship_id,
            make_item(ship_id, 587, char_id, sys_id as i64, InventoryFlag::None, 1),
        );
        // Expired Expert System
        snap.node_graph_state.expert_systems.entry(char_id).or_default().insert(
            1001,
            eve_db::service_domains::ExpertSystemRecord {
                type_id: 1001,
                inserted_at_ft: now_ft - 10_000_000,
                expires_at_ft: now_ft,
                skills_granted: [(3300, 3)].into_iter().collect(),
            },
        );
        // IceBelt anomaly past generic TTL must survive, while normal Combat anomaly expires.
        let anoms = snap.cosmic_anomalies.entry(sys_id).or_default();
        anoms.push(eve_db::CosmicAnomalyRecord {
            anomaly_id: "ICE-1".into(),
            solar_system_id: sys_id,
            name: "Blue Ice Belt".into(),
            anomaly_type: "IceBelt".into(),
            position: [10_000.0, 0.0, 0.0],
            signal_strength: 1.0,
            ore_types: vec![16_262],
            remaining_volume: 10_000.0,
            created_unix: base_now - 90_000,
            lifetime_seconds: 3_600,
            cleared: false,
        });
        anoms.push(eve_db::CosmicAnomalyRecord {
            anomaly_id: "COMB-1".into(),
            solar_system_id: sys_id,
            name: "Hideaway".into(),
            anomaly_type: "CombatAnomaly".into(),
            position: [20_000.0, 0.0, 0.0],
            signal_strength: 1.0,
            ore_types: vec![],
            remaining_volume: 0.0,
            created_unix: base_now - 90_000,
            lifetime_seconds: 3_600,
            cleared: false,
        });
        // Matured pending friendly-fire toggle on corp governance.
        let gov = snap.corp_governance.entry(corp_id).or_default();
        gov.friendly_fire_legal = false;
        gov.pending_friendly_fire = Some((true, base_now + 1));
    });
    let _ = ctx.drain_notifications_for_char(char_id);

    // Paused virtual clock returns 0 advanced ticks.
    clock.set_paused(true);
    assert_eq!(ctx.step_clock_tick(10), 0);
    assert_eq!(ctx.tick_wall_second(), 0);
    clock.set_paused(false);

    // Stepping 1s wall tick triggers unified pipeline: sweeps expert systems, syncs ballpark stats,
    // expires CombatAnomaly while preserving IceBelt, and promotes matured corp grants.
    let expected = ctx.build_ship_combat_state(char_id, ship_id, 587);
    assert_eq!(ctx.tick_wall_second(), 1);

    let notifs = ctx.drain_notifications_for_char(char_id);
    assert!(notifs.iter().any(|n| n.event == "OnExpertSystemsUpdated"));
    assert!(notifs.iter().any(|n| n.event == "OnServerSkillsChanged"));

    let (_, ball) = sys_h.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let exp_vel = expected.base_max_velocity.expect("build_ship_combat_state sets base_max_velocity");
    assert!(
        (ball.max_velocity - exp_vel).abs() < 1e-3,
        "expected ballpark max_velocity {} vs {}",
        exp_vel,
        ball.max_velocity
    );
    let cs = sys_h
        .ballpark
        .read()
        .unwrap()
        .combat_states
        .get(&ship_id)
        .cloned()
        .unwrap();
    assert!((cs.max_shield_hp - expected.max_shield_hp).abs() < 1e-3);

    ctx.db.with_snapshot(|snap| {
        assert!(snap.node_graph_state.expert_systems.get(&char_id).map_or(true, |m| m.is_empty()));
        let anoms = &snap.cosmic_anomalies[&sys_id];
        assert!(anoms.iter().any(|a| a.anomaly_id == "ICE-1"));
        assert!(!anoms.iter().any(|a| a.anomaly_id == "COMB-1"));
        assert!(snap.corp_governance[&corp_id].friendly_fire_legal);
        assert!(snap.corp_governance[&corp_id].pending_friendly_fire.is_none());
    });
}

#[tokio::test]
async fn push_fit_projection_pushes_to_in_space_actor_and_bumps_rev_when_docked() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let sys_id = 30_000_142_u32;
    let ship_id = 100_000_888_i64;

    let sys_h = ctx.systems.get_or_spawn(sys_id);
    sys_h
        .spawn_ball(eve_destiny::Ball {
            id: ship_id,
            type_id: 587,
            position: glam::DVec3::ZERO,
            velocity: glam::DVec3::ZERO,
            max_velocity: 50.0,
            inertia: 5.0,
            mass: 2_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await
        .unwrap();
    let _ = sys_h
        .set_combat_state(ship_id, eve_destiny::CombatState::default())
        .await;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(
            ship_id,
            make_item(ship_id, 587, char_id, sys_id as i64, InventoryFlag::None, 1),
        );
    });

    let rev = ctx
        .push_fit_projection(char_id, ship_id)
        .expect("in-space ship must bump FitRev and push projection");
    assert!(rev.0 >= 1);

    // Flush the single FIFO actor command queue to ensure ApplyFitProjection ran.
    let (_, ball) = sys_h.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let projected = sys_h
        .latest_fit_projection(ship_id)
        .expect("system actor must store pushed FitProjection");
    assert_eq!(projected.rev, rev);
    assert!((ball.max_velocity - projected.baseline.max_velocity).abs() < 1e-3);

    // Docked ship still advances FitRev cleanly without panicking or needing an actor.
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(60_003_760);
        }
    });
    let docked_rev = ctx
        .push_fit_projection(char_id, ship_id)
        .expect("docked ship still bumps FitRev");
    assert!(docked_rev > rev);
    assert_eq!(
        sys_h.latest_fit_projection(ship_id).unwrap().rev,
        rev,
        "docked push must not overwrite in-space actor projection"
    );
}

#[tokio::test]
async fn differential_in_space_fit_mutations_match_fresh_combat_state_and_projection() {
    use crate::services::inventory_fitting::fitting_scanners_superweapons::{
        purge_expired_ship_dbuffs_all, scanners_superweapons_state, ActiveDbuffEntry,
    };

    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let sys_id = 30_000_142_u32;
    let ship_id = 100_000_999_i64;
    let dcu_mod_id = 100_000_998_i64;
    let mwd_mod_id = 100_000_997_i64;
    let ship_type_id = 587_u32; // Rifter

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(
            ship_id,
            make_item(ship_id, ship_type_id, char_id, sys_id as i64, InventoryFlag::None, 1),
        );
        snap.items.insert(
            dcu_mod_id,
            make_item(dcu_mod_id, 2048, char_id, ship_id, InventoryFlag::LoSlot0, 1), // Damage Control II
        );
        snap.items.insert(
            mwd_mod_id,
            make_item(mwd_mod_id, 5973, char_id, ship_id, InventoryFlag::MedSlot0, 1), // 5MN Y-T8 Compact MWD
        );
    });

    let initial_cs = ctx.build_ship_combat_state(char_id, ship_id, ship_type_id);
    let initial_proj = ctx
        .build_fit_projection(char_id, ship_id, ctx.current_fit_rev(ship_id))
        .expect("initial fit projection");

    let sys_h = ctx.systems.get_or_spawn(sys_id);
    sys_h
        .spawn_ball(eve_destiny::Ball {
            id: ship_id,
            type_id: ship_type_id,
            position: glam::DVec3::ZERO,
            velocity: glam::DVec3::ZERO,
            max_velocity: initial_proj.baseline.max_velocity,
            inertia: initial_proj.baseline.agility,
            mass: initial_proj.baseline.mass,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await
        .unwrap();
    let _ = sys_h.set_combat_state(ship_id, initial_cs.clone()).await;

    let assert_actor_matches_fresh = |ctx: &ServerContext, actor_cs: &eve_destiny::CombatState, ball: &eve_destiny::Ball| {
        let fresh_cs = ctx.build_ship_combat_state(char_id, ship_id, ship_type_id);
        let fresh_proj = ctx
            .build_fit_projection(char_id, ship_id, ctx.current_fit_rev(ship_id))
            .expect("fresh projection");
        assert!((actor_cs.max_shield_hp - fresh_cs.max_shield_hp).abs() < 1e-4);
        assert!((actor_cs.max_armor_hp - fresh_cs.max_armor_hp).abs() < 1e-4);
        assert!((actor_cs.max_structure_hp - fresh_cs.max_structure_hp).abs() < 1e-4);
        assert!((actor_cs.max_capacitor - fresh_cs.max_capacitor).abs() < 1e-4);
        assert!((actor_cs.signature_radius - fresh_cs.signature_radius).abs() < 1e-4);
        assert!(
            (actor_cs.base_max_velocity.unwrap_or_default()
                - fresh_cs.base_max_velocity.unwrap_or_default())
            .abs()
                < 1e-4
        );
        assert!((ball.max_velocity - fresh_proj.baseline.max_velocity).abs() < 1e-4);
        assert!((ball.mass - fresh_proj.baseline.mass).abs() < 1e-4);
        for idx in 0..4 {
            assert!(
                (actor_cs.shield_resonance[idx] - fresh_cs.shield_resonance[idx]).abs() < 1e-5,
                "shield resonance[{idx}] mismatch: actor={} vs fresh={}",
                actor_cs.shield_resonance[idx],
                fresh_cs.shield_resonance[idx]
            );
            assert!(
                (actor_cs.armor_resonance[idx] - fresh_cs.armor_resonance[idx]).abs() < 1e-5,
                "armor resonance[{idx}] mismatch: actor={} vs fresh={}",
                actor_cs.armor_resonance[idx],
                fresh_cs.armor_resonance[idx]
            );
            assert!(
                (actor_cs.structure_resonance[idx] - fresh_cs.structure_resonance[idx]).abs() < 1e-5,
                "structure resonance[{idx}] mismatch: actor={} vs fresh={}",
                actor_cs.structure_resonance[idx],
                fresh_cs.structure_resonance[idx]
            );
        }
    };

    // 1. Offline Damage Control II in-space and push projection: actor resists must match fresh rebuild.
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(dcu_mod_id, false);
    });
    let rev1 = ctx
        .invalidate_and_push_fit(ship_id)
        .expect("in-space invalidate_and_push_fit must advance rev");
    let actor_cs_1 = sys_h.get_combat_state(ship_id).await.unwrap().unwrap();
    let (_, ball_1) = sys_h.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert_eq!(sys_h.latest_fit_projection(ship_id).unwrap().rev, rev1);
    assert_actor_matches_fresh(&ctx, &actor_cs_1, &ball_1);
    assert!(actor_cs_1.structure_resonance[0] > initial_cs.structure_resonance[0]);

    // 2. Apply an active command burst / dbuff in-space (e.g., shield/velocity dbuff from SDE) and push.
    let dbuff_id = ctx
        .sde
        .dbuff_collections
        .iter()
        .find(|(_, coll)| coll.item_modifiers.contains(&263) || coll.location_modifiers.contains(&263))
        .map(|(&id, _)| id)
        .unwrap_or(1);
    let now_unix = ctx.now_unix_secs();
    {
        let mut st = scanners_superweapons_state()
            .write()
            .unwrap_or_else(|e| e.into_inner());
        st.ship_dbuffs
            .entry((ctx.instance_id, ship_id))
            .or_default()
            .push(ActiveDbuffEntry {
                dbuff_id,
                value: 20.0,
                expires_at_unix: now_unix + 60,
                source_module_id: 1,
            });
    }
    let rev2 = ctx.invalidate_and_push_fit(ship_id).unwrap();
    assert!(rev2 > rev1);
    let actor_cs_2 = sys_h.get_combat_state(ship_id).await.unwrap().unwrap();
    let (_, ball_2) = sys_h.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert_actor_matches_fresh(&ctx, &actor_cs_2, &ball_2);

    // 3. Expire the dbuff and purge expired dbuffs: actor CombatState must revert to baseline.
    {
        let mut st = scanners_superweapons_state()
            .write()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(entries) = st.ship_dbuffs.get_mut(&(ctx.instance_id, ship_id)) {
            for e in entries.iter_mut() {
                e.expires_at_unix = now_unix.saturating_sub(1);
            }
        }
    }
    purge_expired_ship_dbuffs_all(&ctx, now_unix + 1);
    let actor_cs_3 = sys_h.get_combat_state(ship_id).await.unwrap().unwrap();
    let (_, ball_3) = sys_h.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert_actor_matches_fresh(&ctx, &actor_cs_3, &ball_3);
    assert!((actor_cs_3.max_shield_hp - actor_cs_1.max_shield_hp).abs() < 1e-4);
}

#[test]
fn persisted_fit_inputs_rehydrate_stance_cooldown_and_overloaded_modules_after_invalidation() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let ship_id = 100_000_955_i64;
    let mwd_mod_id = 100_000_956_i64;
    let confessor_type_id = 34317_u32;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(
            ship_id,
            make_item(ship_id, confessor_type_id, char_id, 60_003_760, InventoryFlag::None, 1),
        );
        snap.items.insert(
            mwd_mod_id,
            make_item(mwd_mod_id, 5973, char_id, ship_id, InventoryFlag::MedSlot0, 1),
        );
        snap.module_online_states.insert(mwd_mod_id, true);
        snap.module_active_states.insert(mwd_mod_id);
    });

    let mut fit = ctx.fit(char_id, ship_id).unwrap();
    let now_ms = ctx.now_unix_secs().saturating_mul(1000);
    fit.switch_ship_stance(34321, now_ms, &ctx.sde).unwrap();
    if let Some(m) = fit.modules.get_mut(&mwd_mod_id) {
        m.state = eve_dogma::ModuleState::Overloaded;
    }
    ctx.persist_fit_inputs(ship_id, &fit.persisted_inputs());
    ctx.invalidate_ship_fitting(ship_id);

    let rebuilt = ctx.fit(char_id, ship_id).unwrap();
    assert_eq!(rebuilt.active_stance_type_id, Some(34321));
    assert_eq!(rebuilt.last_stance_switch_ms, Some(now_ms));
    assert_eq!(
        rebuilt.modules.get(&mwd_mod_id).map(|m| m.state),
        Some(eve_dogma::ModuleState::Overloaded)
    );
}
