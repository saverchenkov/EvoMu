//! Unit and mutation-killing tests for `router::context`.

use crate::router::context::{ActiveMoonSurveyProbe, ModuleCooldownEntry, ServerContext};
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

#[tokio::test]
async fn ship_fitting_cache_rebuild_and_has_fleet_extras_branches() {
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
    let sys_h = ctx.systems.get_or_spawn(30_000_142);
    let ewar_mod = DogmaItemState::from_sde(888_888, 526, &ctx.sde);
    sys_h
        .upsert_projected_ewar(
            999_888_001,
            ship_id,
            ProjectedEwarEffect {
                source_module_id: 888_888,
                source_module_type_id: 526,
                charge_type_id: None,
                distance: 5_000.0,
                overloaded: false,
                effective_module: ewar_mod,
            },
        )
        .await
        .expect("upsert projected EWAR into system actor");
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
        snap.skills.entry(char_id).or_default().insert(
            skill_tid,
            eve_db::TrainedSkillRecord {
                type_id: skill_tid,
                level: 0,
                skill_points: 0.0,
            },
        );
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

#[test]
fn synthetic_clock_jump_partial_skill_progress_pushes_aligned_notifications_without_tick_spam() {
    let ctx = test_ctx();
    let base_now = 1_800_000_000_u64;
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(base_now)));
    let char_id = 90_000_001_i64;
    let short_skill_tid = 3392_u32;
    let long_skill_tid = 3402_u32;

    ctx.db
        .acquire_online_lock(1, 502, Some(char_id), true, base_now)
        .unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.clone_grade = 2;
            ch.skill_points = 45_255.0;
        }
        snap.paused_skill_queues.remove(&char_id);
        snap.skills.entry(char_id).or_default().insert(
            long_skill_tid,
            TrainedSkillRecord {
                type_id: long_skill_tid,
                level: 4,
                skill_points: 45_255.0,
            },
        );
        snap.skill_queues.insert(
            char_id,
            vec![eve_db::SkillQueueEntry {
                type_id: long_skill_tid,
                target_level: 5,
                start_sp: 45_255.0,
                end_sp: 256_000.0,
                duration_seconds: 500_000.0,
                started_at_unix: Some(base_now),
            }],
        );
    });
    let _ = ctx.drain_notifications_for_char(char_id);

    // Non-synthetic clock tick (`is_synthetic_jump == false`) must NOT emit partial SP notifications.
    ctx.clock().advance_secs(5);
    ctx.advance_skill_queues_for_clock(base_now, 5.0, false);
    let tick_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(
        !tick_notifs
            .iter()
            .any(|n| n.event == "OnServerSkillsChanged" || n.event == "OnNewSkillQueueSaved"),
        "1s wall/sim ticks must not spam partial skill notifications"
    );

    // Synthetic jump (`advance_secs_without_sim_clock_rebase`) with NO completion MUST push both
    // `OnServerSkillsChanged` and client-wallclock-aligned `OnNewSkillQueueSaved` (#1066).
    let sp_before = ctx
        .db
        .with_snapshot(|s| s.skills[&char_id][&long_skill_tid].skill_points);
    let jump_secs = 86_400_u64;
    ctx.advance_secs_without_sim_clock_rebase(jump_secs);
    let jump_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(
        !jump_notifs
            .iter()
            .any(|n| n.event == "OnSkillTrained" || n.event == "DoSimClockRebase"),
        "partial /advancetime jump must not emit OnSkillTrained or DoSimClockRebase"
    );
    let skills_changed = jump_notifs
        .iter()
        .filter(|n| n.event == "OnServerSkillsChanged")
        .collect::<Vec<_>>();
    assert_eq!(skills_changed.len(), 1, "exactly one OnServerSkillsChanged");
    assert_eq!(skills_changed[0].args.get(1), Some(&PyRep::None));
    let PyRep::Dict(ref skill_map) = skills_changed[0].args[0] else {
        panic!("OnServerSkillsChanged arg0 must be Dict");
    };
    let pushed_sp = skill_map
        .iter()
        .find(|(k, _)| k.as_i64() == Some(i64::from(long_skill_tid)))
        .and_then(|(_, v)| v.dict_get("trainedSkillPoints"))
        .and_then(|v| v.as_f64())
        .expect("updated trainedSkillPoints in OnServerSkillsChanged");
    assert!(
        pushed_sp > sp_before,
        "pushed SP {pushed_sp} must exceed pre-jump SP {sp_before}"
    );

    let queue_saved = jump_notifs
        .iter()
        .filter(|n| n.event == "OnNewSkillQueueSaved")
        .collect::<Vec<_>>();
    assert_eq!(queue_saved.len(), 1, "exactly one OnNewSkillQueueSaved");
    let PyRep::List(ref q_entries) = queue_saved[0].args[0] else {
        panic!("OnNewSkillQueueSaved arg0 must be List");
    };
    assert_eq!(q_entries.len(), 1);
    let head = &q_entries[0];
    let start_ft = head
        .dict_get("trainingStartTime")
        .and_then(|v| v.as_i64())
        .expect("trainingStartTime");
    let end_ft = head
        .dict_get("trainingEndTime")
        .and_then(|v| v.as_i64())
        .expect("trainingEndTime");
    let dur_secs = head
        .dict_get("durationSeconds")
        .and_then(|v| v.as_f64())
        .expect("durationSeconds");
    let expected_client_now_ft = ctx.now_machonet_wallclock_filetime_ticks();
    assert_eq!(
        start_ft, expected_client_now_ft,
        "trainingStartTime must stay anchored to client wallclock baseline across /advancetime"
    );
    let expected_end_ft = start_ft + (dur_secs * 10_000_000.0) as i64;
    assert_eq!(end_ft, expected_end_ft);

    // Mixed completion + partial advance in a single synthetic jump emits ONE notification pair
    // covering both skills without duplicate notifications.
    let now2 = ctx.now_unix_secs();
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            short_skill_tid,
            eve_db::TrainedSkillRecord {
                type_id: short_skill_tid,
                level: 0,
                skill_points: 0.0,
            },
        );
        let cur_long_sp = snap.skills[&char_id][&long_skill_tid].skill_points;
        snap.skill_queues.insert(
            char_id,
            vec![
                eve_db::SkillQueueEntry {
                    type_id: short_skill_tid,
                    target_level: 1,
                    start_sp: 0.0,
                    end_sp: 250.0,
                    duration_seconds: 500.0,
                    started_at_unix: Some(now2),
                },
                eve_db::SkillQueueEntry {
                    type_id: long_skill_tid,
                    target_level: 5,
                    start_sp: cur_long_sp,
                    end_sp: 256_000.0,
                    duration_seconds: 400_000.0,
                    started_at_unix: None,
                },
            ],
        );
    });
    let _ = ctx.drain_notifications_for_char(char_id);
    ctx.advance_secs_without_sim_clock_rebase(3_600);
    let mixed_notifs = ctx.drain_notifications_for_char(char_id);
    assert_eq!(
        mixed_notifs
            .iter()
            .filter(|n| n.event == "OnSkillTrained")
            .count(),
        1
    );
    let mixed_skills = mixed_notifs
        .iter()
        .filter(|n| n.event == "OnServerSkillsChanged")
        .collect::<Vec<_>>();
    assert_eq!(
        mixed_skills.len(),
        1,
        "mixed completion+partial jump must emit one OnServerSkillsChanged"
    );
    let PyRep::Dict(ref mixed_map) = mixed_skills[0].args[0] else {
        panic!("OnServerSkillsChanged arg0 must be Dict");
    };
    assert!(
        mixed_map
            .iter()
            .any(|(k, _)| k.as_i64() == Some(i64::from(short_skill_tid)))
            && mixed_map
                .iter()
                .any(|(k, _)| k.as_i64() == Some(i64::from(long_skill_tid))),
        "OnServerSkillsChanged must include both completed and partially-trained skills"
    );
    assert_eq!(
        mixed_notifs
            .iter()
            .filter(|n| n.event == "OnNewSkillQueueSaved")
            .count(),
        1,
        "mixed completion+partial jump must emit one OnNewSkillQueueSaved"
    );
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
        purge_expired_ship_dbuffs_all, ActiveDbuffEntry,
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
    ctx.scanners_superweapons.with_ship_dbuffs_mut(|dbuffs| {
        dbuffs
            .entry(ship_id)
            .or_default()
            .push(ActiveDbuffEntry {
                dbuff_id,
                value: 20.0,
                expires_at_unix: now_unix + 60,
                source_module_id: 1,
            });
    });
    let rev2 = ctx.invalidate_and_push_fit(ship_id).unwrap();
    assert!(rev2 > rev1);
    let actor_cs_2 = sys_h.get_combat_state(ship_id).await.unwrap().unwrap();
    let (_, ball_2) = sys_h.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert_actor_matches_fresh(&ctx, &actor_cs_2, &ball_2);

    // 3. Expire the dbuff and purge expired dbuffs: actor CombatState must revert to baseline.
    ctx.scanners_superweapons.with_ship_dbuffs_mut(|dbuffs| {
        if let Some(entries) = dbuffs.get_mut(&ship_id) {
            for e in entries.iter_mut() {
                e.expires_at_unix = now_unix.saturating_sub(1);
            }
        }
    });
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

#[tokio::test]
async fn persisted_future_module_reactivation_cooldown_does_not_freeze_after_restart_1036() {
    let sde = Arc::clone(&test_ctx().sde);
    let db = Arc::new(WorldRepository::with_demo_seed());
    let wall_t0 = 1_794_608_434_u64;
    let char_id = 90_000_001_i64;
    let ship_id = 1_897_064_908_i64;
    let mod_id = 1_897_065_268_i64;
    let mod_cloak_id = 1_897_065_269_i64;
    let mod_normal_id = 1_897_065_270_i64;
    let mod_type_id = 440_u32;
    let sys_id = 30_000_142_u32;

    db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(
            ship_id,
            make_item(ship_id, 587, char_id, sys_id as i64, InventoryFlag::None, 1),
        );
        snap.items.insert(
            mod_id,
            make_item(mod_id, mod_type_id, char_id, ship_id, InventoryFlag::HiSlot0, 1),
        );
        snap.items.insert(
            mod_cloak_id,
            make_item(mod_cloak_id, mod_type_id, char_id, ship_id, InventoryFlag::HiSlot1, 1),
        );
        snap.items.insert(
            mod_normal_id,
            make_item(mod_normal_id, mod_type_id, char_id, ship_id, InventoryFlag::HiSlot2, 1),
        );
    });

    // Session 1: record an already-past cooldown (at `wall_t0 - 60` with 30s delay), advance to
    // `wall_t0`, then `/advancetime` ~7.7 days (`667_793s`) and persist future-recorded cooldowns
    // (including higher `recorded_sys_sim_s` as in #1036).
    {
        db.set_clock(Arc::new(VirtualServerClock::with_fixed_base(wall_t0 - 60)));
        let ctx1 = ServerContext::with_sde_and_repository(Arc::clone(&sde), Arc::clone(&db));
        ctx1.record_module_reactivation_delay(ship_id, mod_normal_id, 30.0);
        assert!(ctx1.is_module_in_reactivation_delay(ship_id, mod_normal_id));

        ctx1.clock().advance_secs(60);
        assert!(!ctx1.is_module_in_reactivation_delay(ship_id, mod_normal_id));

        ctx1.advance_secs_without_sim_clock_rebase(667_793);
        assert!(!ctx1.is_module_in_reactivation_delay(ship_id, mod_normal_id));

        ctx1.seed_module_cooldown_entry_for_test(
            ship_id,
            mod_id,
            mod_type_id,
            ModuleCooldownEntry {
                recorded_unix_f64: 1_795_276_227.0,
                delay_s: 2.76,
                solar_system_id: Some(sys_id),
                recorded_sys_sim_s: 1_880.0,
            },
        );
        ctx1.record_module_reactivation_delay(ship_id, mod_cloak_id, 15.0);
        assert!(ctx1.is_module_in_reactivation_delay(ship_id, mod_id));
        assert!(ctx1.is_module_in_reactivation_delay(ship_id, mod_cloak_id));
        ctx1.persist_server_runtime_to_db();
    }

    // Session 2 (restart): virtual clock resets to wall clock `wall_t0` and system sim resets to `0.0`.
    // Persisted future-recorded and already-expired cooldowns must be pruned and not block activation.
    db.set_clock(Arc::new(VirtualServerClock::with_fixed_base(wall_t0)));
    let ctx2 = ServerContext::with_sde_and_repository(Arc::clone(&sde), Arc::clone(&db));
    assert!(!ctx2.is_module_in_reactivation_delay(ship_id, mod_id));
    assert!(!ctx2.is_module_in_reactivation_delay(ship_id, mod_cloak_id));
    assert!(!ctx2.is_module_in_reactivation_delay(ship_id, mod_normal_id));
    assert!(!ctx2.has_any_reactivation_delays_for_test());

    // Advancing wall clock later past the old future timestamp must not resurrect pruned entries.
    ctx2.clock().advance_secs(667_793);
    assert!(!ctx2.is_module_in_reactivation_delay(ship_id, mod_id));
    assert!(!ctx2.is_module_in_reactivation_delay(ship_id, mod_cloak_id));

    // Even if a future-recorded entry is present directly, lookup treats it as expired
    // and re-recording replaces it cleanly with the new delay.
    let future_ts = (ctx2.now_unix_secs() + 3_600) as f64;
    ctx2.seed_module_cooldown_entry_for_test(
        ship_id,
        mod_id,
        mod_type_id,
        ModuleCooldownEntry {
            recorded_unix_f64: future_ts,
            delay_s: 10.0,
            solar_system_id: Some(sys_id),
            recorded_sys_sim_s: 1_880.0,
        },
    );
    assert!(!ctx2.is_module_in_reactivation_delay(ship_id, mod_id));
    ctx2.record_module_reactivation_delay(ship_id, mod_id, 5.0);
    assert!(ctx2.is_module_in_reactivation_delay(ship_id, mod_id));
    ctx2.clock().advance_secs(6);
    assert!(!ctx2.is_module_in_reactivation_delay(ship_id, mod_id));

    // Non-regression: normal cross-restart cooldown (wall clock advancing monotonically) persists
    // for its remaining duration and then expires.
    let t_restart_base = ctx2.now_unix_secs();
    ctx2.record_module_reactivation_delay(ship_id, mod_normal_id, 30.0);
    db.set_clock(Arc::new(VirtualServerClock::with_fixed_base(
        t_restart_base + 10,
    )));
    let ctx3 = ServerContext::with_sde_and_repository(Arc::clone(&sde), Arc::clone(&db));
    assert!(ctx3.is_module_in_reactivation_delay(ship_id, mod_normal_id));
    ctx3.clock().advance_secs(21);
    assert!(!ctx3.is_module_in_reactivation_delay(ship_id, mod_normal_id));

    // Non-regression: same-session system sim clock progression expires cooldowns without wall advance.
    let sys_h = ctx3.systems.get_or_spawn(sys_id);
    ctx3.record_module_reactivation_delay(ship_id, mod_id, 12.0);
    assert!(ctx3.is_module_in_reactivation_delay(ship_id, mod_id));
    sys_h.step_manual(5.0).await.unwrap();
    assert!(ctx3.is_module_in_reactivation_delay(ship_id, mod_id));
    sys_h.step_manual(8.0).await.unwrap();
    assert!(!ctx3.is_module_in_reactivation_delay(ship_id, mod_id));
}

#[test]
fn get_or_rebuild_ship_fitting_cached_branch_rejects_non_owner_and_preserves_cache() {
    let ctx = test_ctx();
    let char_a = 90_000_001_i64;
    let char_b = 90_000_999_i64;
    let ship_a = 100_000_960_i64;
    let ship_b = 100_000_961_i64;
    let mwd_mod_id = 100_000_962_i64;
    let confessor_type_id = 34317_u32;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_a) {
            ch.active_ship_id = ship_a;
        }
        let mut ch_b = snap.characters.get(&char_a).cloned().expect("seed char_a");
        ch_b.character_id = char_b;
        ch_b.active_ship_id = ship_b;
        snap.characters.insert(char_b, ch_b);

        // Give char_b distinct skill levels/implants so any cache overwrite would be observable.
        snap.skills.insert(
            char_b,
            HashMap::from([(
                3449_u32,
                TrainedSkillRecord {
                    type_id: 3449,
                    level: 5,
                    skill_points: 256_000.0,
                },
            )]),
        );
        snap.active_implants.insert(char_b, vec![20121]);

        snap.items.insert(
            ship_a,
            make_item(
                ship_a,
                confessor_type_id,
                char_a,
                60_003_760,
                InventoryFlag::None,
                1,
            ),
        );
        snap.items.insert(
            ship_b,
            make_item(
                ship_b,
                confessor_type_id,
                char_b,
                60_003_760,
                InventoryFlag::None,
                1,
            ),
        );
        snap.items.insert(
            mwd_mod_id,
            make_item(mwd_mod_id, 5973, char_a, ship_a, InventoryFlag::MedSlot0, 1),
        );
        snap.module_online_states.insert(mwd_mod_id, true);
        snap.module_active_states.insert(mwd_mod_id);
    });

    // Populate and cache char_a's fit with stance and overloaded MWD.
    let mut fit_a = ctx.get_or_rebuild_ship_fitting(char_a, ship_a).unwrap();
    let now_ms = ctx.now_unix_secs().saturating_mul(1000);
    fit_a.switch_ship_stance(34321, now_ms, &ctx.sde).unwrap();
    if let Some(m) = fit_a.modules.get_mut(&mwd_mod_id) {
        m.state = eve_dogma::ModuleState::Overloaded;
    }
    ctx.persist_fit_inputs(ship_a, &fit_a.persisted_inputs());
    ctx.store_ship_fitting(ship_a, fit_a.clone());

    let err = ctx
        .get_or_rebuild_ship_fitting(char_b, ship_a)
        .expect_err("non-owner/non-pilot must be rejected on cached fit");
    assert_eq!(err.code, "ShipNotFound");

    // Verify cached fit on ship_a still reflects char_a's skills/implants, stance, and overload.
    let cached = ctx
        .ship_fittings
        .read()
        .unwrap()
        .get(&ship_a)
        .cloned()
        .expect("ship_a must remain cached for owner");
    assert_eq!(cached.character_skills, fit_a.character_skills);
    assert_eq!(cached.implants, fit_a.implants);
    assert_eq!(cached.active_stance_type_id, Some(34321));
    assert_eq!(cached.last_stance_switch_ms, Some(now_ms));
    assert_eq!(
        cached.modules.get(&mwd_mod_id).map(|m| m.state),
        Some(eve_dogma::ModuleState::Overloaded)
    );
}

#[test]
fn clear_persisted_module_overload_preserves_stance_and_prunes_empty_records() {
    let ctx = test_ctx();
    let ship_id = 100_000_970_i64;
    let mod_1 = 100_000_971_i64;
    let mod_2 = 100_000_972_i64;

    ctx.persist_fit_inputs(
        ship_id,
        &eve_dogma::PersistedFitInputs {
            active_stance_type_id: Some(34321),
            last_stance_switch_ms: Some(1_800_000_000_000),
            overloaded_modules: [mod_1, mod_2].into_iter().collect(),
        },
    );

    ctx.db.with_mut_snapshot(|snap| {
        ServerContext::clear_persisted_module_overload(snap, ship_id, mod_1);
        let rec = snap.ship_fit_inputs.get(&ship_id).expect("record kept");
        assert_eq!(rec.active_stance_type_id, Some(34321));
        assert_eq!(rec.last_stance_switch_ms, Some(1_800_000_000_000));
        assert!(!rec.overloaded_modules.contains(&mod_1));
        assert!(rec.overloaded_modules.contains(&mod_2));

        assert!(ServerContext::clear_all_persisted_ship_overloads(
            snap, ship_id
        ));
        let rec_after = snap
            .ship_fit_inputs
            .get(&ship_id)
            .expect("stance state keeps record after overload clear");
        assert!(rec_after.overloaded_modules.is_empty());
        assert_eq!(rec_after.active_stance_type_id, Some(34321));

        // Removing the final overload from a stance-less record prunes the map entry completely.
        snap.ship_fit_inputs
            .get_mut(&ship_id)
            .unwrap()
            .active_stance_type_id = None;
        snap.ship_fit_inputs
            .get_mut(&ship_id)
            .unwrap()
            .last_stance_switch_ms = None;
        snap.ship_fit_inputs
            .get_mut(&ship_id)
            .unwrap()
            .overloaded_modules
            .insert(mod_1);
        ServerContext::clear_persisted_module_overload(snap, ship_id, mod_1);
        assert!(!snap.ship_fit_inputs.contains_key(&ship_id));
    });
}

#[tokio::test]
async fn test_c_deadline_getters_and_advance_sim_ms_expiry() {
    let ctx = test_ctx();
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    let char_id = 90_000_001;
    let ship_id = 100_000_001;
    let mod_id = 900_880_001;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            mod_id,
            make_item(mod_id, 2048, char_id, ship_id, InventoryFlag::LoSlot0, 1),
        );
        snap.cluster_and_runtime
            .module_reload_until_unix
            .insert(mod_id, 1_800_000_005);
    });
    ctx.record_module_reactivation_delay(ship_id, mod_id, 4.0);
    crate::router::session_transition::seed_session_change_timer(
        &ctx,
        char_id,
        crate::router::session_transition::SessionChangeTimerState {
            expires_unix: 1_800_000_007,
            system_id: 30_000_142,
            started_tick: 1,
            reason: "test".into(),
        },
    );

    assert_eq!(ctx.pending_reload_complete_at(mod_id), Some(1_800_000_005));
    assert!(ctx
        .module_reactivation_cooldown_until_unix(ship_id, mod_id)
        .is_some());
    assert_eq!(ctx.session_change_timer_until(char_id), Some(1_800_000_007));

    ctx.advance_sim_ms(4_500).await;
    assert_eq!(ctx.module_reactivation_cooldown_until_unix(ship_id, mod_id), None);
    assert_eq!(ctx.pending_reload_complete_at(mod_id), Some(1_800_000_005));
    assert_eq!(ctx.session_change_timer_until(char_id), Some(1_800_000_007));

    let waited_ms = ctx
        .advance_until(|| ctx.session_change_timer_until(char_id).is_none(), 5_000)
        .await
        .expect("session_change_timer_until must expire via advance_until");
    assert!(waited_ms >= 2_500);
    assert_eq!(ctx.pending_reload_complete_at(mod_id), None);
    assert_eq!(ctx.session_change_timer_until(char_id), None);
}

#[test]
fn advance_skill_queues_completing_last_skill_removes_key_and_passes_sqlite_pre_roundtrip() {
    let ctx = test_ctx();
    let base_now = 1_800_000_000_u64;
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(base_now)));
    let char_a = 90_000_001_i64;
    let char_b = 90_000_002_i64;
    let skill_tid = 3402_u32;

    // Seed single-entry queues for two characters so both `advance_skill_queues` and
    // `advance_secs` (`advance_skill_queues_for_clock`) exercise final-skill completion.
    ctx.db.with_mut_snapshot(|snap| {
        for cid in [char_a, char_b] {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.clone_grade = 2;
                ch.skill_points = 0.0;
            }
            snap.paused_skill_queues.remove(&cid);
            snap.skills.entry(cid).or_default().insert(
                skill_tid,
                TrainedSkillRecord {
                    type_id: skill_tid,
                    level: 0,
                    skill_points: 0.0,
                },
            );
        }
        snap.skill_queues.insert(
            char_a,
            vec![eve_db::SkillQueueEntry {
                type_id: skill_tid,
                target_level: 1,
                start_sp: 0.0,
                end_sp: 250.0,
                duration_seconds: 500.0,
                started_at_unix: Some(base_now),
            }],
        );
    });

    // 1) Complete final queued skill via `advance_skill_queues`.
    ctx.advance_skill_queues(3_600.0);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.skills[&char_a][&skill_tid].level, 1);
        assert!(
            !snap.skill_queues.contains_key(&char_a),
            "completing the last queued skill must remove char_a from snap.skill_queues"
        );
    });

    let diffs_after_advance = eve_testkit::pre_check::check_persist_reload_equivalence(&ctx.db)
        .expect("SQLite PRE check after advance_skill_queues");
    assert!(
        diffs_after_advance.is_empty(),
        "expected zero SQLite PRE diffs after completing last queued skill via advance_skill_queues, got: {diffs_after_advance:?}"
    );

    // 2) Complete final queued skill via `advance_secs` (`advance_skill_queues_for_clock`).
    let now2 = ctx.now_unix_secs();
    ctx.db.with_mut_snapshot(|snap| {
        snap.skill_queues.insert(
            char_b,
            vec![eve_db::SkillQueueEntry {
                type_id: skill_tid,
                target_level: 1,
                start_sp: 0.0,
                end_sp: 250.0,
                duration_seconds: 500.0,
                started_at_unix: Some(now2),
            }],
        );
    });
    ctx.advance_secs(3_600);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.skills[&char_b][&skill_tid].level, 1);
        assert!(
            !snap.skill_queues.contains_key(&char_b),
            "completing the last queued skill via advance_secs must remove char_b from snap.skill_queues"
        );
    });

    let diffs_after_clock = eve_testkit::pre_check::check_persist_reload_equivalence(&ctx.db)
        .expect("SQLite PRE check after advance_secs");
    assert!(
        diffs_after_clock.is_empty(),
        "expected zero SQLite PRE diffs after completing last queued skill via advance_secs, got: {diffs_after_clock:?}"
    );
    let reloaded = ctx
        .db
        .reload_snapshot_from_sqlite()
        .expect("reload snapshot from SQLite");
    assert!(!reloaded.skill_queues.contains_key(&char_a));
    assert!(!reloaded.skill_queues.contains_key(&char_b));
    assert_eq!(reloaded.skills[&char_a][&skill_tid].level, 1);
    assert_eq!(reloaded.skills[&char_b][&skill_tid].level, 1);
}

#[test]
fn server_context_lock_guarded_field_ratchet_and_no_warp_maps() {
    let src = include_str!("context.rs");
    let ctx_body = src
        .split("pub struct ServerContext {")
        .nth(1)
        .and_then(|s| s.split("\n}").next())
        .expect("ServerContext struct body");
    let weak_body = src
        .split("pub(crate) struct ServerContextWeak {")
        .nth(1)
        .and_then(|s| s.split("\n}").next())
        .expect("ServerContextWeak struct body");
    for forbidden in [
        "active_warps:",
        "return_warps:",
        "pending_warp_finished:",
        "session_change_timers:",
        "weapon_cycle_state:",
        "fighters:",
        "fighter_cooldown_state:",
        "fighter_charge_state:",
        "active_fighters:",
        "fighter_tube_loads:",
        "projected_ewar:",
        "projected_ewar_sources:",
        "active_ewar_starts:",
        "module_reactivation_cooldowns:",
        "module_reload_sim_deadlines:",
    ] {
        assert!(
            !ctx_body.contains(forbidden),
            "ServerContext must not own `{forbidden}` (#1541)"
        );
        assert!(
            !weak_body.contains(forbidden),
            "ServerContextWeak must not mirror `{forbidden}` (#1541)"
        );
    }
    let mut normalized = String::new();
    for line in ctx_body.lines() {
        let trimmed = line.split("//").next().unwrap_or("").trim();
        if trimmed.is_empty() || trimmed.starts_with("#[") {
            continue;
        }
        normalized.push_str(trimmed);
        normalized.push(' ');
    }
    let mut depth = 0usize;
    let mut fields: Vec<String> = Vec::new();
    let mut cur = String::new();
    for ch in normalized.chars() {
        match ch {
            '<' | '(' | '[' | '{' => {
                depth += 1;
                cur.push(ch);
            }
            '>' | ')' | ']' | '}' => {
                depth = depth.saturating_sub(1);
                cur.push(ch);
            }
            ',' if depth == 0 => {
                if !cur.trim().is_empty() {
                    fields.push(std::mem::take(&mut cur));
                }
            }
            _ => cur.push(ch),
        }
    }
    if !cur.trim().is_empty() {
        fields.push(cur);
    }
    let locked_fields = fields
        .iter()
        .filter(|f| f.contains("Mutex<") || f.contains("RwLock<"))
        .count();
    assert!(
        locked_fields <= 35,
        "ServerContext lock-guarded field count regressed above 35: {locked_fields} (#1541)"
    );
}

fn singleton_item(item_id: i64, type_id: u32, owner_id: i64, location_id: i64, flag: InventoryFlag) -> InventoryItemRecord {
    InventoryItemRecord { item_id, type_id, owner_id, location_id, flag, quantity: 1, singleton: true, ..Default::default() }
}

fn cd_entry(now: f64, delay_s: f64, sys_id: u32) -> crate::system_actor::ModuleCooldownEntry {
    crate::system_actor::ModuleCooldownEntry { recorded_unix_f64: now, delay_s, solar_system_id: Some(sys_id), recorded_sys_sim_s: 0.0 }
}

fn cloak_cycle(ship_id: i64, module_id: i64, module_type_id: u32, owner: i64, delay_s: f64) -> crate::system_actor::ActiveModuleCycle {
    crate::system_actor::ActiveModuleCycle {
        ship_id, module_id, module_type_id, owner_char_id: Some(owner),
        effect_name: "cloaking".into(), repeat: 1, duration_s: 5.0, reactivation_delay_s: delay_s, ..Default::default()
    }
}

async fn spawn_ship_ball(h: &crate::system_actor::SystemHandle, id: i64, owner: i64) {
    h.spawn_ball_with_meta(
        eve_destiny::Ball {
            id, type_id: 587, position: glam::DVec3::ZERO, velocity: glam::DVec3::ZERO,
            max_velocity: 300.0, inertia: 2.0, mass: 1_000_000.0, radius: 40.0, speed_fraction: 0.0, mode: eve_destiny::MovementMode::Stop,
        },
        eve_destiny::CombatState::default(),
        eve_destiny::SlimItemMeta { group_id: 25, category_id: 6, owner_id: owner, char_id: Some(owner), ..Default::default() },
    ).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cross_system_expired_cooldown_does_not_shadow_active_entry_and_cold_removal_cleans_db() {
    let ctx = test_ctx();
    let (sys_a, sys_b, sys_c) = (30_000_142_u32, 30_002_187_u32, 30_000_144_u32);
    let (ship_id, mod_id, cloak_tid) = (100_980_001_i64, 100_980_002_i64, 11_370_u32);
    let now_unix = ctx.now_unix_secs() as f64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(ship_id, singleton_item(ship_id, 587, 90_000_001, sys_b as i64, InventoryFlag::None));
        snap.items.insert(mod_id, singleton_item(mod_id, cloak_tid, 90_000_001, ship_id, InventoryFlag::HiSlot0));
    });
    let (_h_a, _h_b) = (ctx.systems.get_or_spawn(sys_a), ctx.systems.get_or_spawn(sys_b));

    ctx.seed_module_cooldown_entry_for_test(ship_id, mod_id, cloak_tid, cd_entry(now_unix - 120.0, 10.0, sys_a));
    ctx.record_module_reactivation_delay(ship_id, mod_id, 25.0);
    assert!(ctx.is_module_in_reactivation_delay(ship_id, mod_id));
    assert!(ctx.module_reactivation_cooldown_until_unix(ship_id, mod_id).unwrap() > now_unix + 20.0);

    ctx.seed_module_cooldown_entry_for_test(ship_id, mod_id, cloak_tid, cd_entry(now_unix, 40.0, sys_c));
    assert!(ctx.systems.get_handle(sys_c).is_none());
    assert!(ctx.module_reactivation_cooldown_until_unix(ship_id, mod_id).unwrap() >= now_unix + 39.0);

    ctx.record_module_reactivation_delay_for_system(Some(sys_b), ship_id, mod_id, cloak_tid, 5.0);
    assert!(ctx.systems.get_handle(sys_a).unwrap().module_reactivation_cooldown(&(ship_id, mod_id, cloak_tid)).is_none());
    assert!(crate::system_actor::load_cold_system_module_cooldowns_from_db(&ctx.db, sys_c).is_empty());
    assert!(ctx.systems.get_handle(sys_b).unwrap().module_reactivation_cooldown(&(ship_id, mod_id, cloak_tid)).is_some_and(|e| e.delay_s >= 39.0));

    ctx.seed_module_cooldown_entry_for_test(ship_id, mod_id, cloak_tid, cd_entry(now_unix, 45.0, sys_c));
    assert!(ctx.remove_module_reactivation_delay(ship_id, mod_id, cloak_tid));
    assert!(!ctx.is_module_in_reactivation_delay(ship_id, mod_id));
    assert!(crate::system_actor::load_cold_system_module_cooldowns_from_db(&ctx.db, sys_c).is_empty());
    assert!(!ctx.has_any_reactivation_delays_for_test());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn same_batch_cycle_deactivation_and_recon_cooldown_override_plus_actor_isolation() {
    let ctx = test_ctx();
    let (jita, amarr, char_id) = (30_000_142_u32, 30_002_187_u32, 90_000_001_i64);
    let (ship_id, target_id, cloak_id, cloak_tid) = (100_990_001_i64, 100_990_002_i64, 100_990_003_i64, 11_370_u32);
    let now_unix = ctx.now_unix_secs() as f64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.active_ship_id = ship_id;
            ch.solar_system_id = jita;
        }
        snap.items.insert(ship_id, singleton_item(ship_id, 587, char_id, jita as i64, InventoryFlag::None));
        snap.items.insert(cloak_id, singleton_item(cloak_id, cloak_tid, char_id, ship_id, InventoryFlag::HiSlot0));
    });
    let (h_jita, h_amarr) = (ctx.systems.get_or_spawn(jita), ctx.systems.get_or_spawn(amarr));

    assert!(!Arc::ptr_eq(&h_jita.post_unlock_actions, &h_amarr.post_unlock_actions));
    let jita_drained = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let amarr_marker = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (jc, ac) = (Arc::clone(&jita_drained), Arc::clone(&amarr_marker));
    h_jita.enqueue_post_unlock_action(Box::new(move || jc.store(true, std::sync::atomic::Ordering::SeqCst)));
    h_amarr.enqueue_post_unlock_action(Box::new(move || ac.store(true, std::sync::atomic::Ordering::SeqCst)));
    let _ = h_amarr.has_active_modules_or_cloak_sync(ship_id);
    h_jita.step_manual(0.1).await.unwrap();
    assert!(jita_drained.load(std::sync::atomic::Ordering::SeqCst));
    assert!(!amarr_marker.load(std::sync::atomic::Ordering::SeqCst));

    spawn_ship_ball(&h_jita, ship_id, char_id).await;
    h_jita.record_active_ewar_start(ship_id, cloak_id, target_id, "electronic").await.unwrap();
    let (tx_add, rx_add) = tokio::sync::oneshot::channel();
    h_jita.cmd_sender().send(crate::system_actor::SystemCommand::AddModuleCycle {
        cycle: cloak_cycle(ship_id, cloak_id, cloak_tid, char_id, 30.0),
        reply: tx_add,
    }).await.unwrap();
    rx_add.await.unwrap().unwrap();

    let (tx_decloak, rx_decloak) = tokio::sync::oneshot::channel();
    let (tx_remove, rx_remove) = tokio::sync::oneshot::channel();
    let (tx_record, rx_record) = tokio::sync::oneshot::channel();
    h_jita.cmd_sender().try_send(crate::system_actor::SystemCommand::ForceDecloak {
        ball_id: ship_id, char_id: Some(char_id), reason: crate::system_actor::ProtectionBreakReason::Manual, real_now_unix: now_unix as u64, reply: tx_decloak,
    }).unwrap();
    h_jita.cmd_sender().try_send(crate::system_actor::SystemCommand::RemoveModuleCooldown {
        key: (ship_id, cloak_id, cloak_tid), reply: tx_remove,
    }).unwrap();
    h_jita.cmd_sender().try_send(crate::system_actor::SystemCommand::RecordModuleCooldown {
        ship_id, module_id: cloak_id, module_type_id: cloak_tid, now_unix_f64: now_unix, delay_s: 5.0, tidi_factor: 1.0, has_tidi_history: false, reply: tx_record,
    }).unwrap();

    rx_decloak.await.unwrap().unwrap();
    assert!(rx_remove.await.unwrap().unwrap().is_some());
    let _payload = rx_record.await.unwrap().unwrap();
    let entry = h_jita.module_reactivation_cooldown(&(ship_id, cloak_id, cloak_tid)).expect("5s Recon override recorded");
    assert!((entry.delay_s - 5.0).abs() < 1e-6, "expected 5.0s override, got {}", entry.delay_s);
    assert_eq!(ctx.systems.ewar_starts_count(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actor_ewar_deactivation_publishes_clean_fit_projection_and_concurrent_cross_sys_cloak_purges() {
    let ctx = test_ctx();
    let (jita, amarr, char_id, char2_id) = (30_000_142_u32, 30_002_187_u32, 90_000_001_i64, 90_000_002_i64);
    let (attacker_ship, target_ship, amarr_ship) = (100_991_001_i64, 100_991_002_i64, 100_991_006_i64);
    let (tp_mod_id, cloak_jita, cloak_amarr, cloak_tid) = (100_991_003_i64, 100_991_004_i64, 100_991_005_i64, 11_370_u32);
    let tp_tid = ctx.sde.types.values().find(|t| t.group_id == 379).map(|t| t.type_id).unwrap_or(2185);
    let now_unix = ctx.now_unix_secs() as f64;

    ctx.db.with_mut_snapshot(|snap| {
        for (cid, ship) in [(char_id, target_ship), (char2_id, attacker_ship)] {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                (ch.station_id, ch.structure_id, ch.solar_system_id, ch.active_ship_id) = (None, None, jita, ship);
            }
        }
        for (sid, owner, loc) in [(target_ship, char_id, jita as i64), (attacker_ship, char2_id, jita as i64), (amarr_ship, char_id, amarr as i64)] {
            snap.items.insert(sid, singleton_item(sid, 587, owner, loc, InventoryFlag::None));
        }
        snap.items.insert(tp_mod_id, singleton_item(tp_mod_id, tp_tid, char2_id, attacker_ship, InventoryFlag::MedSlot0));
    });

    let (h_jita, h_amarr) = (ctx.systems.get_or_spawn(jita), ctx.systems.get_or_spawn(amarr));
    for (h, id, owner) in [(&h_jita, target_ship, char_id), (&h_jita, attacker_ship, char2_id), (&h_amarr, amarr_ship, char_id)] {
        spawn_ship_ball(h, id, owner).await;
    }
    let _ = ctx.get_or_rebuild_ship_fitting(char_id, target_ship);
    let base_sig = ctx.effective_target_signature_radius(target_ship, 40.0);

    let mut tp_state = eve_dogma::DogmaItemState::from_sde(tp_mod_id, tp_tid, &ctx.sde);
    tp_state.base_attributes.insert(554, 40.0);
    tp_state.base_attributes.insert(eve_sde::attr_ids::MAX_RANGE, 30_000.0);
    ctx.upsert_projected_ewar_on_ship(attacker_ship, target_ship, eve_dogma::ProjectedEwarEffect {
        source_module_id: tp_mod_id, source_module_type_id: tp_tid, charge_type_id: None, distance: 5_000.0, overloaded: false, effective_module: tp_state,
    });
    ctx.record_active_ewar_start_for_test(Some(jita), attacker_ship, tp_mod_id, target_ship, "targetPainter");
    assert!(ctx.effective_target_signature_radius(target_ship, 40.0) > base_sig);

    h_jita.add_module_cycle(crate::system_actor::ActiveModuleCycle {
        ship_id: attacker_ship, module_id: tp_mod_id, module_type_id: tp_tid, owner_char_id: Some(char2_id), target_id: Some(target_ship), effect_name: "targetPaint".into(), repeat: 0, duration_s: 1.0, ..Default::default()
    }).await.unwrap();
    h_jita.step_manual(1.5).await.unwrap();
    let _ = h_jita.get_ball_snapshot(target_ship).await.unwrap();

    assert!(ctx.systems.ship_ewar_projections(target_ship).is_empty());
    assert!(ctx.ship_fittings.read().unwrap().get(&target_ship).is_some_and(|f| f.projected_ewar.is_empty()));
    let proj_after = h_jita.actor_state.read().unwrap().fit_projections.get(&target_ship).cloned().expect("FitProjection pushed after EWAR flush");
    assert!((proj_after.baseline.signature_radius - base_sig).abs() < 1e-6);
    assert_eq!(h_jita.get_combat_state(target_ship).await.unwrap().unwrap().tp_sig_multiplier, None);
    assert!((ctx.effective_target_signature_radius(target_ship, 40.0) - base_sig).abs() < 1e-6);

    h_amarr.seed_module_cooldown_entry_sync((attacker_ship, cloak_jita, cloak_tid), cd_entry(now_unix, 30.0, amarr)).unwrap();
    h_jita.seed_module_cooldown_entry_sync((amarr_ship, cloak_amarr, cloak_tid), cd_entry(now_unix, 30.0, jita)).unwrap();
    h_jita.add_module_cycle(cloak_cycle(attacker_ship, cloak_jita, cloak_tid, char2_id, 20.0)).await.unwrap();
    h_amarr.add_module_cycle(cloak_cycle(amarr_ship, cloak_amarr, cloak_tid, char_id, 20.0)).await.unwrap();
    let (r_jita, r_amarr) = tokio::join!(
        h_jita.force_decloak(attacker_ship, Some(char2_id), crate::system_actor::ProtectionBreakReason::Manual, now_unix as u64),
        h_amarr.force_decloak(amarr_ship, Some(char_id), crate::system_actor::ProtectionBreakReason::Manual, now_unix as u64),
    );
    r_jita.unwrap();
    r_amarr.unwrap();
    assert!(h_jita.module_reactivation_cooldown(&(attacker_ship, cloak_jita, cloak_tid)).is_some());
    assert!(h_amarr.module_reactivation_cooldown(&(amarr_ship, cloak_amarr, cloak_tid)).is_some());
    for _ in 0..40 {
        if h_amarr.module_reactivation_cooldown(&(attacker_ship, cloak_jita, cloak_tid)).is_none()
            && h_jita.module_reactivation_cooldown(&(amarr_ship, cloak_amarr, cloak_tid)).is_none() { break; }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(h_amarr.module_reactivation_cooldown(&(attacker_ship, cloak_jita, cloak_tid)).is_none());
    assert!(h_jita.module_reactivation_cooldown(&(amarr_ship, cloak_amarr, cloak_tid)).is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cold_tidi_query_and_hydrate_server_runtime_preserve_active_dilated_cooldown() {
    let ctx1 = test_ctx();
    let t0 = 1_800_000_000_u64;
    ctx1.set_clock(Arc::new(VirtualServerClock::with_fixed_base(t0)));
    let (sys_id, ship_id, mod_ok, mod_exp, cloak_tid) = (30_000_142_u32, 100_992_001_i64, 100_992_002_i64, 100_992_003_i64, 11_370_u32);
    ctx1.db.with_mut_snapshot(|snap| {
        snap.owner_notes.insert(-(7_500_000_000_000 + i64::from(sys_id)), eve_db::OwnerNoteRecord {
            note_id: -(7_500_000_000_000 + i64::from(sys_id)),
            character_id: -75, item_id: i64::from(sys_id), label: format!("tidi:{sys_id}"), note: "0.25".into(), timestamp_unix: t0,
        });
        snap.items.insert(ship_id, singleton_item(ship_id, 587, 90_000_001, sys_id as i64, InventoryFlag::None));
        snap.items.insert(mod_ok, singleton_item(mod_ok, cloak_tid, 90_000_001, ship_id, InventoryFlag::HiSlot0));
        snap.items.insert(mod_exp, singleton_item(mod_exp, cloak_tid, 90_000_001, ship_id, InventoryFlag::HiSlot1));
    });
    ctx1.seed_module_cooldown_entry_for_test(ship_id, mod_ok, cloak_tid, cd_entry(t0 as f64, 30.0, sys_id));
    ctx1.seed_module_cooldown_entry_for_test(ship_id, mod_exp, cloak_tid, cd_entry(t0 as f64 - 200.0, 5.0, sys_id));
    assert_eq!(ctx1.resolve_sys_tidi_mode(Some(sys_id)), (0.25, true));
    let (sde, db) = (Arc::clone(&ctx1.sde), Arc::clone(&ctx1.db));
    drop(ctx1);

    let ctx2 = ServerContext::with_sde_and_repository(sde, db);
    ctx2.set_clock(Arc::new(VirtualServerClock::with_fixed_base(t0 + 60)));
    ctx2.hydrate_server_runtime_from_db();
    assert!(ctx2.systems.get_handle(sys_id).is_none());
    assert_eq!(ctx2.resolve_sys_tidi_mode(Some(sys_id)), (0.25, true));
    assert!(ctx2.is_module_in_reactivation_delay(ship_id, mod_ok));
    assert!(!ctx2.is_module_in_reactivation_delay(ship_id, mod_exp));
    let until = ctx2.module_reactivation_cooldown_until_unix(ship_id, mod_ok).expect("dilated cold cooldown");
    assert!((until - ((t0 + 60) as f64 + 60.0)).abs() < 1.0, "expected ~60s wall left, got {}", until - (t0 + 60) as f64);
    let cold = crate::system_actor::load_cold_system_module_cooldowns_from_db(&ctx2.db, sys_id);
    assert!(cold.contains_key(&(ship_id, mod_ok, cloak_tid)));
    assert!(!cold.contains_key(&(ship_id, mod_exp, cloak_tid)));

    let h = ctx2.systems.wake_for_session_arrival_async(sys_id, crate::system_actor::SessionArrivalWakeReason::Undock).await.unwrap();
    assert!((h.tidi_factor() - 0.25).abs() < 1e-6);
    assert!(h.module_reactivation_cooldown(&(ship_id, mod_ok, cloak_tid)).is_some_and(|e| (e.delay_s - 15.0).abs() < 1.0));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn loading_transition_hook_serializes_record_remove_clear_ship_and_cross_system_purge() {
    let ctx = test_ctx();
    let now = ctx.now_unix_secs() as f64;
    let (sys_a, sys_b, cloak_tid) = (30_000_142_u32, 30_002_187_u32, 11_370_u32);
    let (s_keep, s_purge, s_clear, s_new) = (100_993_001_i64, 100_993_002_i64, 100_993_003_i64, 100_993_004_i64);
    let (m1, m2, m3, m4, m5) = (100_993_101_i64, 100_993_102_i64, 100_993_103_i64, 100_993_104_i64, 100_993_105_i64);
    ctx.db.with_mut_snapshot(|snap| {
        for s in [s_keep, s_purge, s_clear, s_new] {
            snap.items.insert(s, singleton_item(s, 587, 90_000_001, sys_a as i64, InventoryFlag::None));
        }
        for (m, s) in [(m1, s_keep), (m2, s_purge), (m3, s_clear), (m4, s_new), (m5, s_clear)] {
            snap.items.insert(m, singleton_item(m, cloak_tid, 90_000_001, s, InventoryFlag::HiSlot0));
        }
    });
    for (s, m, d) in [(s_keep, m1, 10.0), (s_purge, m2, 25.0), (s_clear, m3, 30.0)] {
        ctx.seed_module_cooldown_entry_for_test(s, m, cloak_tid, cd_entry(now, d, sys_a));
    }
    let h_b = ctx.systems.get_or_spawn(sys_b);
    let (ran, saw_new, hid_rm) = (Arc::new(std::sync::atomic::AtomicBool::new(false)), Arc::new(std::sync::atomic::AtomicBool::new(false)), Arc::new(std::sync::atomic::AtomicBool::new(false)));
    let (ctx_h, ran_c, saw_c, hid_c) = (ctx.clone(), Arc::clone(&ran), Arc::clone(&saw_new), Arc::clone(&hid_rm));
    ctx.systems.set_before_publish_hook_for_test(Some(Arc::new(move |sid, _| {
        if sid != sys_a { return; }
        ran_c.store(true, std::sync::atomic::Ordering::SeqCst);
        ctx_h.record_module_reactivation_delay_for_system(Some(sys_a), s_new, m4, cloak_tid, 40.0);
        assert!(ctx_h.remove_module_reactivation_delay(s_keep, m1, cloak_tid));
        ctx_h.record_module_reactivation_delay_for_system(Some(sys_b), s_purge, m2, cloak_tid, 12.0);
        ctx_h.record_module_reactivation_delay_for_system(Some(sys_a), s_clear, m5, cloak_tid, 35.0);
        ctx_h.clear_ship_reactivation_delays(s_clear);
        saw_c.store(ctx_h.is_module_in_reactivation_delay(s_new, m4), std::sync::atomic::Ordering::SeqCst);
        hid_c.store(!ctx_h.is_module_in_reactivation_delay(s_keep, m1), std::sync::atomic::Ordering::SeqCst);
    })));
    let h_a = ctx.systems.wake_for_session_arrival_async(sys_a, crate::system_actor::SessionArrivalWakeReason::Undock).await.unwrap();
    ctx.systems.set_before_publish_hook_for_test(None);

    assert!(ran.load(std::sync::atomic::Ordering::SeqCst));
    assert!(saw_new.load(std::sync::atomic::Ordering::SeqCst));
    assert!(hid_rm.load(std::sync::atomic::Ordering::SeqCst));
    assert!(h_a.module_reactivation_cooldown(&(s_new, m4, cloak_tid)).is_some_and(|e| e.delay_s >= 38.0));
    for k in [(s_keep, m1, cloak_tid), (s_purge, m2, cloak_tid), (s_clear, m3, cloak_tid), (s_clear, m5, cloak_tid)] {
        assert!(h_a.module_reactivation_cooldown(&k).is_none());
    }
    assert!(h_b.module_reactivation_cooldown(&(s_purge, m2, cloak_tid)).is_some_and(|e| e.delay_s >= 24.0));
    let persisted = crate::system_actor::load_cold_system_module_cooldowns_from_db(&ctx.db, sys_a);
    assert!(persisted.contains_key(&(s_new, m4, cloak_tid)) && persisted.len() == 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stopping_transition_hook_journals_record_remove_clear_ship_and_cross_system_purge() {
    let ctx = test_ctx();
    let (sys_a, sys_b, cloak_tid) = (30_000_142_u32, 30_002_187_u32, 11_370_u32);
    let (s_keep, s_rm, s_purge, s_clear, s_new) = (100_994_001_i64, 100_994_002_i64, 100_994_003_i64, 100_994_004_i64, 100_994_005_i64);
    let (m1, m2, m3, m4, m5) = (100_994_101_i64, 100_994_102_i64, 100_994_103_i64, 100_994_104_i64, 100_994_105_i64);
    let (h_a, h_b) = (ctx.systems.get_or_spawn(sys_a), ctx.systems.get_or_spawn(sys_b));
    h_a.set_tidi_factor_raw(0.5);
    for (s, m, d) in [(s_keep, m1, 10.0), (s_rm, m2, 15.0), (s_purge, m3, 30.0), (s_clear, m4, 20.0)] {
        ctx.record_module_reactivation_delay_for_system(Some(sys_a), s, m, cloak_tid, d);
    }
    let stop_flushed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let sf_c = Arc::clone(&stop_flushed);
    h_a.enqueue_post_unlock_action(Box::new(move || sf_c.store(true, std::sync::atomic::Ordering::SeqCst)));
    drop(h_a);

    let ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (ctx_h, ran_c) = (ctx.clone(), Arc::clone(&ran));
    ctx.systems.set_before_stop_finish_hook_for_test(Some(Arc::new(move |sid, _| {
        if sid != sys_a { return; }
        ran_c.store(true, std::sync::atomic::Ordering::SeqCst);
        ctx_h.record_module_reactivation_delay_for_system(Some(sys_a), s_new, m5, cloak_tid, 45.0);
        assert!(ctx_h.remove_module_reactivation_delay(s_rm, m2, cloak_tid));
        ctx_h.record_module_reactivation_delay_for_system(Some(sys_b), s_purge, m3, cloak_tid, 8.0);
        ctx_h.clear_ship_reactivation_delays(s_clear);
    })));
    assert!(ctx.systems.stop_system(sys_a).await);
    ctx.systems.set_before_stop_finish_hook_for_test(None);

    assert!(ran.load(std::sync::atomic::Ordering::SeqCst));
    assert!(stop_flushed.load(std::sync::atomic::Ordering::SeqCst));
    assert!(ctx.systems.get_handle(sys_a).is_none());
    assert!(h_b.module_reactivation_cooldown(&(s_purge, m3, cloak_tid)).is_some_and(|e| e.delay_s >= 29.0));
    let cold = crate::system_actor::load_cold_system_module_cooldowns_from_db(&ctx.db, sys_a);
    assert!(cold.contains_key(&(s_keep, m1, cloak_tid)) && cold.contains_key(&(s_new, m5, cloak_tid)));
    for k in [(s_rm, m2, cloak_tid), (s_purge, m3, cloak_tid), (s_clear, m4, cloak_tid)] {
        assert!(!cold.contains_key(&k));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn early_loading_transitions_before_prepare_survive_publish_and_abort_flush() {
    use eve_db::sqlite_store::system_checkpoint::{
        ensure_system_checkpoint_schema, replace_system_checkpoints_for_kind, CLUSTER_SYSTEM_ID,
        KIND_MODULE_COOLDOWNS,
    };
    let ctx = test_ctx();
    let now = ctx.now_unix_secs() as f64;
    let (sys_a, sys_b, cloak_tid) = (30_000_142_u32, 30_002_187_u32, 11_370_u32);
    let (s_keep, s_new, s_abort, s_u) = (100_995_001_i64, 100_995_002_i64, 100_995_003_i64, 100_995_004_i64);
    let (m_keep, m_new, m_abort, m_u) = (100_995_101_i64, 100_995_102_i64, 100_995_103_i64, 100_995_104_i64);
    ctx.db.with_mut_snapshot(|snap| {
        for (s, sys) in [(s_keep, sys_a), (s_new, sys_a), (s_abort, sys_b)] {
            snap.items.insert(s, singleton_item(s, 587, 90_000_001, sys as i64, InventoryFlag::None));
        }
        for (m, s) in [(m_keep, s_keep), (m_new, s_new), (m_abort, s_abort)] {
            snap.items.insert(m, singleton_item(m, cloak_tid, 90_000_001, s, InventoryFlag::HiSlot0));
        }
    });
    ctx.seed_module_cooldown_entry_for_test(s_keep, m_keep, cloak_tid, cd_entry(now, 25.0, sys_a));
    ctx.db.with_sqlite_conn(|c| {
        ensure_system_checkpoint_schema(c)?;
        replace_system_checkpoints_for_kind(c, CLUSTER_SYSTEM_ID, KIND_MODULE_COOLDOWNS, &[(
            crate::system_actor::module_cooldown_checkpoint_key(s_u, m_u, cloak_tid),
            ((s_u, m_u, cloak_tid), ModuleCooldownEntry { recorded_unix_f64: now, delay_s: 20.0, solar_system_id: None, recorded_sys_sim_s: 0.0 }),
        )], now as u64)
    }).unwrap();

    let (ran_claim, saw_pub) = (Arc::new(std::sync::atomic::AtomicBool::new(false)), Arc::new(std::sync::atomic::AtomicBool::new(false)));
    let (c1, c2, r_c, s_c) = (ctx.clone(), ctx.clone(), Arc::clone(&ran_claim), Arc::clone(&saw_pub));
    ctx.systems.set_after_claim_loading_hook_for_test(Some(Arc::new(move |sid, _| {
        if sid == sys_a {
            r_c.store(true, std::sync::atomic::Ordering::SeqCst);
            c1.record_module_reactivation_delay_for_system(Some(sys_a), s_new, m_new, cloak_tid, 45.0);
            assert!(c1.remove_module_reactivation_delay(s_keep, m_keep, cloak_tid));
        } else if sid == sys_b {
            c1.record_module_reactivation_delay_for_system(Some(sys_b), s_abort, m_abort, cloak_tid, 35.0);
            c1.systems.set_force_publish_wouldblock_for_test(true);
        }
    })));
    ctx.systems.set_before_publish_hook_for_test(Some(Arc::new(move |sid, _| {
        if sid == sys_a {
            s_c.store(c2.is_module_in_reactivation_delay(s_new, m_new) && !c2.is_module_in_reactivation_delay(s_keep, m_keep), std::sync::atomic::Ordering::SeqCst);
        }
    })));

    let h_a = ctx.systems.wake_for_session_arrival_async(sys_a, crate::system_actor::SessionArrivalWakeReason::Undock).await.unwrap();
    assert!(ran_claim.load(std::sync::atomic::Ordering::SeqCst) && saw_pub.load(std::sync::atomic::Ordering::SeqCst));
    assert!(h_a.module_reactivation_cooldown(&(s_new, m_new, cloak_tid)).is_some_and(|e| e.delay_s >= 44.0));
    assert!(h_a.module_reactivation_cooldown(&(s_keep, m_keep, cloak_tid)).is_none());
    let db_a = crate::system_actor::load_cold_system_module_cooldowns_from_db(&ctx.db, sys_a);
    assert!(db_a.contains_key(&(s_new, m_new, cloak_tid)) && !db_a.contains_key(&(s_keep, m_keep, cloak_tid)));

    assert!(ctx.systems.wake_for_session_arrival_async(sys_b, crate::system_actor::SessionArrivalWakeReason::Undock).await.is_err());
    ctx.systems.set_force_publish_wouldblock_for_test(false);
    assert!(crate::system_actor::load_cold_system_module_cooldowns_from_db(&ctx.db, sys_b).contains_key(&(s_abort, m_abort, cloak_tid)));

    let sys_c = 30_000_144_u32;
    let (s_pre, m_pre) = (100_995_005_i64, 100_995_105_i64);
    let hit_notify = Arc::new(tokio::sync::Notify::new());
    let (go_tx, go_rx_raw) = std::sync::mpsc::channel::<()>();
    let go_rx = Arc::new(std::sync::Mutex::new(Some(go_rx_raw)));
    let (c3, hn_c) = (ctx.clone(), Arc::clone(&hit_notify));
    ctx.systems.set_after_claim_loading_hook_for_test(Some(Arc::new(move |sid, _| {
        if sid != sys_c { return; }
        c3.record_module_reactivation_delay_for_system(Some(sys_c), s_pre, m_pre, cloak_tid, 50.0);
        hn_c.notify_one();
        if let Some(rx) = go_rx.lock().unwrap().take() {
            let _ = tokio::task::block_in_place(|| rx.recv_timeout(std::time::Duration::from_secs(3)));
        }
    })));
    let ctx_w = ctx.clone();
    let wake_pre = tokio::spawn(async move {
        ctx_w.systems.wake_for_session_arrival_async(sys_c, crate::system_actor::SessionArrivalWakeReason::Undock).await
    });
    hit_notify.notified().await;
    assert!(ctx.systems.stop_system(sys_c).await);
    let _ = go_tx.send(());
    assert!(wake_pre.await.unwrap().is_err());
    ctx.systems.set_after_claim_loading_hook_for_test(None);
    ctx.systems.set_before_publish_hook_for_test(None);
    assert!(crate::system_actor::load_cold_system_module_cooldowns_from_db(&ctx.db, sys_c).contains_key(&(s_pre, m_pre, cloak_tid)));
    ctx.purge_module_cooldown_other_systems(sys_a, s_u, m_u, cloak_tid);
    assert!(crate::system_actor::load_cold_system_module_cooldowns_from_db(&ctx.db, CLUSTER_SYSTEM_ID).is_empty());
}
