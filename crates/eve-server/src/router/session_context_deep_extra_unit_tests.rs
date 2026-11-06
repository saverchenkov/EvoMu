//! Deep branch-coverage and mutation-killing unit tests for `router::session` and `router::context`.

#![forbid(unsafe_code)]

use crate::config::{AdminAccountConfig, FeatureFlagsConfig, PlayerAccountConfig, ServerConfig};
use crate::router::{
    ActiveFighterSquadron, ActiveMoonSurveyProbe, BoundObjectEntry, NotificationTarget, ServerContext,
};
use crate::services::combat_crimewatch::{with_mut_combat_runtime, DroneSpaceState};
use eve_db::{
    CosmicSignatureRecord, FleetRole, InventoryFlag, InventoryItemRecord, MoonExtractionRecord,
    OwnerNoteRecord, ServerClock, SkillQueueEntry, UpwellStructureRecord, VirtualServerClock,
    WorldRepository,
};
use eve_destiny::DamageVector;
use eve_dogma::{DogmaItemState, ProjectedEwarEffect, ShipFittingEngine};
use eve_machonet::service_roles::{ROLE_GMH, ROLE_PLAYER_DEFAULT};
use eve_machonet::SessionState;
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use uuid::Uuid;

struct ScopedEnv {
    prev: Vec<(&'static str, Option<String>)>,
}

impl ScopedEnv {
    fn capture(keys: &[&'static str]) -> Self {
        let prev = keys.iter().map(|&k| (k, std::env::var(k).ok())).collect();
        Self { prev }
    }
}

impl Drop for ScopedEnv {
    fn drop(&mut self) {
        for (k, v) in self.prev.drain(..) {
            match v {
                Some(val) => std::env::set_var(k, val),
                None => std::env::remove_var(k),
            }
        }
    }
}

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| {
        Arc::new(SdeRegistry::open(SdeRegistry::default_db_path()).expect("open default SDE"))
    }))
}

fn fast_test_context(base_unix: u64) -> (Arc<ServerContext>, Arc<VirtualServerClock>) {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = Arc::new(ServerContext::with_sde_and_repository(shared_sde(), db));
    let clock = Arc::new(VirtualServerClock::with_fixed_base(base_unix));
    ctx.set_clock(Arc::clone(&clock) as Arc<dyn ServerClock>);
    (ctx, clock)
}

#[test]
fn router_session_guards_and_shutdown_flags() {
    let (ctx, _) = fast_test_context(1_800_000_000);

    // 1. Shutdown flag toggle
    assert!(!ctx.is_shutting_down());
    ctx.cluster_state.write().unwrap().shutdown_in_progress = true;
    assert!(ctx.is_shutting_down());

    // 2. Authentication, GM role, and character selection guards
    let mut sess = SessionState::new(11);
    assert_eq!(ServerContext::require_auth(&sess).unwrap_err().code, "Unauthorized");
    assert_eq!(ServerContext::require_gm_role(&sess).unwrap_err().code, "Unauthorized");
    assert_eq!(ServerContext::require_character(&sess).unwrap_err().code, "Unauthorized");

    sess.user_id = Some(42);
    sess.role = ROLE_PLAYER_DEFAULT;
    assert_eq!(ServerContext::require_auth(&sess).unwrap(), 42);
    assert_eq!(ServerContext::require_gm_role(&sess).unwrap_err().code, "CustomError");
    assert_eq!(ServerContext::require_character(&sess).unwrap_err().code, "CharacterNotSelected");

    sess.role = ROLE_GMH;
    sess.character_id = Some(90_000_001);
    assert_eq!(ServerContext::require_gm_role(&sess).unwrap(), 42);
    assert_eq!(ServerContext::require_character(&sess).unwrap(), (42, 90_000_001));

    // 3. Bound system validation
    assert_eq!(
        ServerContext::require_bound_system(None, "custom_bound_svc").unwrap_err().code,
        "BoundObjectRequired"
    );
    let bound = BoundObjectEntry {
        bound_id: 1,
        session_id: 11,
        user_id: 42,
        character_id: 90_000_001,
        service: "expected_bound_svc".into(),
        object_id: 30_000_142,
        oid_str: "N=1:1".into(),
        ref_id: 1,
    };
    assert_eq!(
        ServerContext::require_bound_system(Some(&bound), "custom_bound_svc").unwrap_err().code,
        "InvalidBoundService"
    );
    assert_eq!(
        ServerContext::require_bound_system(Some(&bound), "expected_bound_svc").unwrap(),
        30_000_142
    );
}

#[tokio::test]
async fn context_constructors_production_accounts_and_demo_world_fixtures() {
    let tmp_dir = std::env::temp_dir().join(format!("evomu_ctx_ctor_{}", Uuid::new_v4()));
    std::fs::create_dir_all(&tmp_dir).unwrap();

    // 1. new_production + new with EVOMU_DISABLE_DEMO_SEED under TEST_ENV_LOCK
    {
        let _guard = crate::lock_test_env_write();
        let _env = ScopedEnv::capture(&["EVOMU_ACCOUNTS", "EVOMU_GM_ACCOUNT", "EVOMU_DISABLE_DEMO_SEED"]);
        std::env::set_var("EVOMU_ACCOUNTS", "alice:pass_a , malformed_entry , bob:pass_b");
        std::env::set_var("EVOMU_GM_ACCOUNT", "op_gm:gm_pass");
        let prod = ServerContext::new_production();
        let alice = prod.db.authenticate("alice", "pass_a").expect("alice provisioned");
        let bob = prod.db.authenticate("bob", "pass_b").expect("bob provisioned");
        let gm = prod.db.authenticate("op_gm", "gm_pass").expect("gm provisioned");
        assert_ne!(alice.user_id, bob.user_id);
        assert_ne!(gm.role & ROLE_GMH, 0);
        assert_eq!(prod.db.get_characters_for_user(gm.user_id)[0].name, "ISD Polaris");

        std::env::set_var("EVOMU_DISABLE_DEMO_SEED", "true");
        let empty_ctx = ServerContext::new();
        assert!(empty_ctx.db.get_characters_for_user(1).is_empty());
    }

    // 2. from_config error on invalid sde_path
    let mut bad_cfg = ServerConfig::default();
    bad_cfg.sde_path = tmp_dir.join("missing_sde_dir").to_string_lossy().into_owned();
    assert!(ServerContext::from_config(&bad_cfg).is_err());

    // 3. from_config valid with accounts, admin, custom intervals, and seed_demo = true
    let world_sqlite = tmp_dir.join("world.sqlite");
    let mut cfg = ServerConfig::default();
    cfg.sde_path = SdeRegistry::default_db_path().to_string_lossy().into_owned();
    cfg.world_db_path = world_sqlite.to_string_lossy().into_owned();
    cfg.belt_respawn_interval_secs = 2_400;
    cfg.downtime_utc_secs_of_day = 41_400;
    cfg.esi_port = 26_111;
    cfg.feature_flags = FeatureFlagsConfig {
        seed_demo: true,
        ..Default::default()
    };
    cfg.player_accounts = vec![
        PlayerAccountConfig {
            username: "  spaced_user  ".into(),
            password_token: "  pw_user  ".into(),
            starter_character: None,
        },
        PlayerAccountConfig {
            username: "   ".into(),
            password_token: "pw_ignored".into(),
            starter_character: None,
        },
    ];
    cfg.admin_accounts = vec![AdminAccountConfig {
        username: "adm_user".into(),
        password_token: "adm_pw".into(),
        character_id: 90_000_099,
        character_name: "GM Override".into(),
    }];

    let ctx = ServerContext::from_config(&cfg).expect("from_config succeeds");
    assert_eq!(ctx.esi_port, 26_111);
    assert!(ctx.db.authenticate("spaced_user", "pw_user").is_ok());
    assert!(ctx.db.authenticate("adm_user", "adm_pw").is_ok());
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items.get(&100_000_200).map(|i| i.type_id), Some(53_977));
    });
    // Also exercise seed_demo_world_fixtures when dynamic solar system states are present
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.entry(30_002_813).or_default();
        snap.system_states.entry(31_000_005).or_default();
        snap.system_states.entry(30_002_904).or_default();
    });
    ctx.seed_demo_world_fixtures();
    ctx.db.with_snapshot(|snap| {
        let st_ess = &snap.system_states[&30_002_904];
        assert_eq!(st_ess.ess_main_bank, eve_db::IskCents::from_isk(10_000_000));
        assert_eq!(st_ess.ess_reserve_bank, eve_db::IskCents::from_isk(25_000_000));
        assert_eq!(snap.system_states[&30_002_813].fw_contested_pct, 45.0);
    });

    // 4. from_sqlite round-trip
    let sqlite_ctx = ServerContext::from_sqlite(&world_sqlite).expect("open from_sqlite");
    assert!(sqlite_ctx.db.authenticate("spaced_user", "pw_user").is_ok());

    let _ = std::fs::remove_dir_all(&tmp_dir);
}

#[test]
fn context_clocks_cooldowns_scans_skills_and_all_fleet_extras_branches() {
    let base = 1_800_000_000_u64;
    let (ctx, clock) = fast_test_context(base);
    let char_id = 90_000_001_i64;
    let ship_id = 100_000_001_i64;
    let sys_id = 30_000_142_u32;

    // 1. Virtual clock pause, pause_on_tick, acceleration multiplier, step_clock_tick, tick_wall_second
    assert_eq!(ctx.now_unix(), base);
    let ft0 = ctx.now_blueos_filetime_ticks();
    assert!(ft0 > 0);
    assert!(!ctx.is_clock_paused());
    ctx.set_clock_paused(true);
    assert!(ctx.is_clock_paused());
    assert_eq!(ctx.step_clock_tick(10), 0);
    ctx.set_clock_paused(false);

    ctx.set_pause_on_tick(true);
    assert!(ctx.pause_on_tick());
    ctx.set_time_acceleration_multiplier(4);
    assert_eq!(ctx.time_acceleration_multiplier(), 4);
    assert_eq!(ctx.step_clock_tick(5), 20);
    assert_eq!(ctx.now_unix(), base + 20);
    assert!(ctx.is_clock_paused());
    ctx.set_clock_paused(false);
    ctx.set_pause_on_tick(false);

    assert_eq!(ctx.tick_wall_second(), 4);
    assert_eq!(clock.now_unix_secs(), base + 24);
    ctx.advance_virtual_clock(0);
    ctx.advance_time_seconds(0);
    assert_eq!(ctx.now_unix_secs(), base + 24);

    // 2. Reactivation cooldown delay (<= 0 ignored, > 0 recorded & max-extended, then expires)
    ctx.record_module_reactivation_delay(ship_id, 555_010, 0.0);
    assert!(!ctx.is_module_in_reactivation_delay(ship_id, 555_010));
    ctx.record_module_reactivation_delay(ship_id, 555_010, 15.0);
    ctx.record_module_reactivation_delay(ship_id, 555_010, 30.0);
    ctx.record_module_reactivation_delay(ship_id, 555_010, 10.0);
    assert!(ctx.is_module_in_reactivation_delay(ship_id, 555_010));
    clock.advance_secs(31);
    assert!(!ctx.is_module_in_reactivation_delay(ship_id, 555_010));

    // 3. Signature scan tracking and progress overrides
    assert!(!ctx.has_scanned_signature(char_id, sys_id, 888_001));
    ctx.mark_signature_scanned(char_id, sys_id, 888_001);
    ctx.mark_signature_scanned(char_id, sys_id, 888_001);
    assert!(ctx.has_scanned_signature(char_id, sys_id, 888_001));

    let sig = CosmicSignatureRecord {
        signature_id: "ABC-123".into(),
        solar_system_id: sys_id,
        sig_group: "Data".into(),
        type_name: "Site".into(),
        position: [0.0; 3],
        signal_strength: 0.35,
        wormhole_type_id: None,
        destination_system_id: None,
        created_unix: base,
        lifetime_seconds: 86_400,
        max_mass_kg: 0.0,
        remaining_mass_kg: 0.0,
        max_jump_mass_kg: 0.0,
        collapsed: false,
    };
    assert_eq!(ctx.effective_signature_signal_strength(char_id, sys_id, &sig), 0.35);
    ctx.record_character_scan_progress(char_id, sys_id, "ABC-123", 0.92);
    assert_eq!(ctx.get_character_scan_progress(char_id, sys_id, "ABC-123"), Some(0.92));
    assert_eq!(ctx.effective_signature_signal_strength(char_id, sys_id, &sig), 0.92);

    // 4. Character skill levels under Alpha clamp vs Omega
    let skill_tid = ctx.sde.types.values().find(|t| t.category_id == 16).map(|t| t.type_id).unwrap_or(3300);
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            skill_tid,
            eve_db::TrainedSkillRecord { type_id: skill_tid, level: 5, skill_points: 256_000.0 },
        );
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.clone_grade = 1;
        }
    });
    let alpha_levels = ctx.character_skill_levels(char_id);
    assert!(alpha_levels.contains_key(&skill_tid));

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.clone_grade = 2;
        }
    });
    let omega_levels = ctx.character_skill_levels(char_id);
    assert_eq!(omega_levels.get(&skill_tid).copied(), Some(5));

    // 5. All 8 `has_fleet_extras_for` OR-branches independently exercised
    for (idx, fleet_id) in (70_100_i64..70_108).enumerate() {
        assert!(!ctx.has_fleet_extras_for(fleet_id));
        {
            let mut ex = ctx.fleet_extras.lock().unwrap();
            match idx {
                0 => { ex.target_tags.insert(fleet_id, HashMap::from([(1, "A".into())])); }
                1 => { ex.watchlist.insert(fleet_id, std::collections::HashSet::from([char_id])); }
                2 => { ex.options.insert(fleet_id, eve_proto::PyRep::None); }
                3 => { ex.wing_squad_hierarchy.insert(fleet_id, (vec![1], vec![2])); }
                4 => { ex.fleet_names.insert(fleet_id, "Roam".into()); }
                5 => { ex.max_sizes.insert(fleet_id, 50); }
                6 => { ex.invite_details.insert((fleet_id, char_id), (1, 2, FleetRole::SquadMember, 1)); }
                _ => { ex.invite_timestamps.insert((fleet_id, char_id), base); }
            }
        }
        assert!(ctx.has_fleet_extras_for(fleet_id), "branch {idx} must set has_fleet_extras_for");
    }
}

#[tokio::test]
async fn context_advance_secs_and_skill_queues_omega_lapse_alpha_caps_and_partial_training() {
    let base = 1_800_000_000_u64;
    let (ctx, _clock) = fast_test_context(base);
    let char_omega_lapsing = 90_000_001_i64;
    let char_alpha_capped = 90_000_002_i64;
    let char_partial = 90_000_003_i64;
    let sys_id = 30_000_142_u32;

    // 1. Seed Omega subscription expiring at base + 60 for char 1, Alpha >5M SP for char 2, and rank-1 Science (3402) for char 3
    let skill_tid = 3402_u32;
    let rank = ctx
        .sde
        .types
        .get(&skill_tid)
        .and_then(|t| t.default_attributes.get(&eve_sde::attr_ids::SKILL_TIME_CONSTANT))
        .copied()
        .unwrap_or(1.0);
    let lvl1_sp = crate::formulas::sp_for_skill_level(rank, 1);
    let lvl3_sp = crate::formulas::sp_for_skill_level(rank, 3);
    let moon_id = ctx.sde.celestials_in_system(sys_id).into_iter().find(|c| c.group_id == 8).map(|c| c.celestial_id);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch1) = snap.characters.get_mut(&char_omega_lapsing) {
            ch1.clone_grade = 2;
            ch1.station_id = None;
            ch1.structure_id = None;
            ch1.solar_system_id = sys_id;
            let uid = ch1.user_id;
            let mut qstore = crate::services::quasar_auth_user::QuasarIdentityStore::default();
            qstore.omega_subscriptions.insert(uid, base + 60);
            snap.owner_notes.insert(
                -8_500_000_000_000_i64,
                OwnerNoteRecord {
                    note_id: -8_500_000_000_000_i64,
                    character_id: 0,
                    item_id: 0,
                    label: "quasar".into(),
                    note: serde_json::to_string(&qstore).unwrap(),
                    timestamp_unix: base,
                },
            );
        }
        snap.skill_queues.insert(char_omega_lapsing, vec![SkillQueueEntry {
            type_id: skill_tid, target_level: 5, start_sp: 0.0, end_sp: 50_000.0,
            started_at_unix: Some(base), duration_seconds: 50_000.0,
        }]);

        if let Some(ch2) = snap.characters.get_mut(&char_alpha_capped) {
            ch2.clone_grade = 1;
            ch2.skill_points = 5_000_100.0;
        }
        snap.skill_queues.insert(char_alpha_capped, vec![SkillQueueEntry {
            type_id: skill_tid, target_level: 1, start_sp: 0.0, end_sp: 250.0,
            started_at_unix: None, duration_seconds: 100.0,
        }]);

        let mut ch3 = snap.characters[&char_omega_lapsing].clone();
        ch3.character_id = char_partial;
        ch3.user_id = 33;
        ch3.clone_grade = 2;
        ch3.skill_points = 0.0;
        snap.characters.insert(char_partial, ch3);
        snap.skill_queues.insert(char_partial, vec![SkillQueueEntry {
            type_id: skill_tid, target_level: 3, start_sp: 0.0, end_sp: lvl3_sp,
            started_at_unix: None, duration_seconds: 10_000.0,
        }]);

        // Expired jetcan (type_id = 23) + cargo child item
        snap.items.insert(930_001, InventoryItemRecord {
            item_id: 930_001, type_id: 23, owner_id: char_omega_lapsing, location_id: sys_id as i64,
            flag: InventoryFlag::None, quantity: 1, singleton: true, version: 1,
            dynamic_attributes: HashMap::new(), source_mutaplasmid_id: None,
            insured_payout_isk: 0.0, is_blueprint_copy: false, blueprint_runs_remaining: None,
        });
        snap.items.insert(930_002, InventoryItemRecord {
            item_id: 930_002, type_id: 34, owner_id: char_omega_lapsing, location_id: 930_001,
            flag: InventoryFlag::Cargo, quantity: 10, singleton: false, version: 1,
            dynamic_attributes: HashMap::new(), source_mutaplasmid_id: None,
            insured_payout_isk: 0.0, is_blueprint_copy: false, blueprint_runs_remaining: None,
        });
        crate::sp6_lifecycle::record_wreck_or_container_meta_in_snap(snap, 930_001, base, false);

        // Decaying moon extraction + asteroid
        snap.items.insert(930_003, InventoryItemRecord {
            item_id: 930_003, type_id: 1230, owner_id: 0, location_id: sys_id as i64,
            flag: InventoryFlag::None, quantity: 1, singleton: true, version: 1,
            dynamic_attributes: HashMap::new(), source_mutaplasmid_id: None,
            insured_payout_isk: 0.0, is_blueprint_copy: false, blueprint_runs_remaining: None,
        });
        snap.moon_extractions.insert(930_010, MoonExtractionRecord {
            structure_id: 100_000_999, moon_id: moon_id.unwrap_or(40_009_001),
            extraction_start_unix: base, chunk_arrival_unix: base + 10, natural_decay_unix: base + 100,
            status: "Ready".into(), auto_fracture: false, event_id: 0,
            moon_ores: Vec::new(), spawned_asteroid_ids: vec![930_003],
        });
    });

    if let Ok(mut mp) = ctx.active_moon_probes.write() {
        mp.push(ActiveMoonSurveyProbe {
            probe_id: 930_020, probe_type_id: 30_488, owner_char_id: char_omega_lapsing,
            solar_system_id: sys_id, hit_moon_id: moon_id, arrival_unix_secs: base + 5,
        });
        mp.push(ActiveMoonSurveyProbe {
            probe_id: 930_021, probe_type_id: 30_488, owner_char_id: char_omega_lapsing,
            solar_system_id: sys_id, hit_moon_id: Some(999_999_999), arrival_unix_secs: base + 5,
        });
    }

    // Advance 3600s: crosses Omega lapse (60s), jetcan despawn (3600s), moon decay, probes, and partial skill leveling
    ctx.advance_secs(3_600);
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&930_001));
        assert!(!snap.items.contains_key(&930_002));
        assert_eq!(snap.moon_extractions[&930_010].status, "Decayed");
        assert_eq!(snap.characters[&char_omega_lapsing].clone_grade, 1);
        assert!(snap.paused_skill_queues.contains(&char_omega_lapsing));
        assert!(snap.paused_skill_queues.contains(&char_alpha_capped));
        assert!(snap.characters[&char_partial].skill_points >= lvl1_sp);
        assert!(!snap.skill_history.get(&char_partial).cloned().unwrap_or_default().is_empty());
    });
    assert!(ctx.active_moon_probes.read().unwrap().is_empty());
}

#[test]
fn context_ship_fitting_cache_recalc_structure_fallback_and_charges_drones() {
    let base = 1_800_000_000_u64;
    let (ctx, _) = fast_test_context(base);
    let char_id = 90_000_001_i64;
    let ship_id = 100_000_001_i64;
    let sys_id = 30_000_142_u32;

    // 1. Unknown ship -> Err(ShipNotFound)
    assert_eq!(ctx.get_or_rebuild_ship_fitting(char_id, 999_888_777).unwrap_err().code, "ShipNotFound");

    // 2. Seed mid-slot module (5MN MWD 440 on MedSlot0), charge on MedSlot0, drone in bay, and active drone in space
    let mod_tid = 440_u32;
    let charge_tid = ctx.sde.types.values().find(|t| t.category_id == 8).map(|t| t.type_id).unwrap_or(178);
    let drone_tid = ctx.sde.types.values().find(|t| t.category_id == 18).map(|t| t.type_id).unwrap_or(2454);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(m) = ctx.sde.types.get(&mod_tid) {
            for &aid in &[eve_sde::attr_ids::REQUIRED_SKILL_1, eve_sde::attr_ids::REQUIRED_SKILL_2] {
                if let Some(&sk) = m.default_attributes.get(&aid) {
                    let sk_tid = sk as u32;
                    snap.skills.entry(char_id).or_default().insert(
                        sk_tid,
                        eve_db::TrainedSkillRecord { type_id: sk_tid, level: 5, skill_points: 256_000.0 },
                    );
                }
            }
        }
        let mut dyn_attrs = HashMap::new();
        dyn_attrs.insert(eve_sde::attr_ids::DAMAGE, 4.5);
        dyn_attrs.insert(eve_sde::attr_ids::HP_STRUCT, 75.0);
        snap.items.insert(940_001, InventoryItemRecord {
            item_id: 940_001, type_id: mod_tid, owner_id: char_id, location_id: ship_id,
            flag: InventoryFlag::MedSlot0, quantity: 1, singleton: true, version: 1,
            dynamic_attributes: dyn_attrs, source_mutaplasmid_id: None,
            insured_payout_isk: 0.0, is_blueprint_copy: false, blueprint_runs_remaining: None,
        });
        snap.module_online_states.insert(940_001, true);
        snap.module_active_states.insert(940_001);
        snap.module_active_charges.insert(940_001, charge_tid);

        snap.items.insert(940_002, InventoryItemRecord {
            item_id: 940_002, type_id: charge_tid, owner_id: char_id, location_id: ship_id,
            flag: InventoryFlag::MedSlot0, quantity: 25, singleton: false, version: 1,
            dynamic_attributes: HashMap::new(), source_mutaplasmid_id: None,
            insured_payout_isk: 0.0, is_blueprint_copy: false, blueprint_runs_remaining: None,
        });
        snap.items.insert(940_003, InventoryItemRecord {
            item_id: 940_003, type_id: drone_tid, owner_id: char_id, location_id: ship_id,
            flag: InventoryFlag::DroneBay, quantity: 1, singleton: true, version: 1,
            dynamic_attributes: HashMap::new(), source_mutaplasmid_id: None,
            insured_payout_isk: 0.0, is_blueprint_copy: false, blueprint_runs_remaining: None,
        });
    });

    with_mut_combat_runtime(&ctx, |rt| {
        rt.active_drones_in_space.insert(940_010, DroneSpaceState {
            ball_id: 940_010, drone_item_id: 940_010, type_id: drone_tid, group_id: 100,
            owner_char_id: char_id, original_owner_char_id: char_id, parent_ship_id: ship_id,
            solar_system_id: sys_id, target_id: None, activity_state: 1, assist_char_id: None,
            guard_char_id: None, is_abandoned: false, is_sentry: false, bandwidth_used: 5.0,
            max_velocity: 1000.0, optimal_range: 5000.0, damage_vector: DamageVector::default(),
            shield_repair_amount: 0.0, armor_repair_amount: 0.0, hull_repair_amount: 0.0,
            web_speed_factor: 0.0, neut_amount: 0.0, ecm_strength: 0.0, tp_sig_bonus_pct: 0.0,
        });
    });

    let fit1 = ctx.get_or_rebuild_ship_fitting(char_id, ship_id).expect("rebuild succeeds");
    assert_eq!(fit1.modules[&940_001].damage_hp, 4.5);
    assert_eq!(fit1.modules[&940_001].max_hp, 75.0);
    assert_eq!(fit1.modules[&940_001].charge_type_id, Some(charge_tid));
    assert_eq!(fit1.modules[&940_001].charge_quantity, 25);
    assert!(fit1.drones.iter().any(|&(id, _)| id == 940_003));
    assert!(fit1.drones.iter().any(|&(id, _)| id == 940_010));

    // Warm hit + projected_ewar delta recalc
    ctx.projected_ewar.write().unwrap().insert(
        ship_id,
        vec![ProjectedEwarEffect {
            source_module_id: 940_001,
            source_module_type_id: mod_tid,
            charge_type_id: None,
            distance: 5_000.0,
            overloaded: false,
            effective_module: DogmaItemState::from_sde(940_001, mod_tid, &ctx.sde),
        }],
    );
    let fit2 = ctx.get_or_rebuild_ship_fitting(char_id, ship_id).expect("recalc warm cache");
    assert_eq!(fit2.projected_ewar.len(), 1);

    // invalidate_ship_fitting + store_ship_fitting + structure-as-ship fallback
    ctx.invalidate_ship_fitting(ship_id);
    ctx.store_ship_fitting(ship_id, ShipFittingEngine::from_sde(ship_id, 587, &ctx.sde));
    let struct_id = 1_000_940_001_i64;
    let struct_tid = ctx.sde.types.values().find(|t| t.category_id == 65).map(|t| t.type_id).unwrap_or(35_832);
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(struct_id, UpwellStructureRecord {
            structure_id: struct_id, type_id: struct_tid, owner_corp_id: 98_000_001,
            solar_system_id: sys_id, name: "Test Citadel".into(), position: [0.0; 3],
            state: "ShieldVulnerable".into(), quantum_core_installed: true, fuel_blocks: 100,
            shield_hp: 1000.0, armor_hp: 1000.0, hull_hp: 1000.0,
            reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0,
            self_destruct_at_unix: None,
        });
        snap.items.insert(struct_id, InventoryItemRecord {
            item_id: struct_id, type_id: struct_tid, owner_id: 98_000_001, location_id: sys_id as i64,
            flag: InventoryFlag::None, quantity: 1, singleton: true, version: 1,
            dynamic_attributes: HashMap::new(), source_mutaplasmid_id: None,
            insured_payout_isk: 0.0, is_blueprint_copy: false, blueprint_runs_remaining: None,
        });
    });
    assert!(ctx.get_or_rebuild_ship_fitting(char_id, struct_id).is_ok());

    // Keep unused imports referenced
    let _ = NotificationTarget::All;
    let _ = ActiveFighterSquadron {
        ball_id: 1, item_id: 1, type_id: 23055, owner_char_id: char_id, parent_ship_id: ship_id,
        tube_index: 0, squadron_size: 9, target_id: None, is_abandoned: false, mwd_active: false,
    };
}
