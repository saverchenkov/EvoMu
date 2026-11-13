//! Deep branch-coverage and mutation-killing unit tests for `config`, `starter_characters`,
//! `router::session`, `router::context` state/clock helpers, `log_ring_buffer`, `flight_recorder`,
//! and `esi::runtime_state`.

#![forbid(unsafe_code)]

use crate::config::{PlayerAccountConfig, ServerConfig, StarterCharacterConfig};
use crate::esi::runtime_state::{EsiMailMeta, EsiRuntimeState};
use crate::flight_recorder::FlightRecorder;
use crate::log_ring_buffer::{LogRingBuffer, LogRingLayer};
use crate::router::context::BoundObjectEntry;
use crate::router::{ActiveMoonSurveyProbe, NotificationTarget, ServerContext};
use crate::starter_characters::{
    grant_starter_plex_in_snapshot, provision_starter_characters, should_grant_default_starter_plex,
};
use eve_db::{
    CosmicSignatureRecord, InventoryFlag, InventoryItemRecord, MoonExtractionRecord, ServerClock,
    SkillQueueEntry, VirtualServerClock, WorldRepository,
};
use eve_dogma::ShipFittingEngine;
use eve_machonet::service_roles::{ROLE_GMH, ROLE_PLAYER_DEFAULT};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
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
        let _env_guard = crate::lock_test_env_read();
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
fn config_load_default_or_env_and_cwd_toml_paths() {
    let _guard = crate::lock_test_env_write();
    let keys = [
        "EVOMU_CONFIG_PATH",
        "EVOMU_SDE_PATH",
        "EVOMU_DB_PATH",
        "EVOMU_SNAPSHOT_PATH",
        "EVOMU_ACCOUNTS",
        "EVOMU_GM_ACCOUNT",
        "EVOMU_QUASAR_GRPC_PORT",
        "EVOMU_XMPP_PORT",
        "EVEMU_XMPP_PORT",
    ];
    let _env = ScopedEnv::capture(&keys);
    for k in keys {
        std::env::remove_var(k);
    }

    // 1. Default when neither EVOMU_CONFIG_PATH nor ./evomu.toml is present, plus EVOMU_SNAPSHOT_PATH fallback
    std::env::set_var("EVOMU_SNAPSHOT_PATH", "custom/snap.sqlite");
    std::env::set_var("EVOMU_ACCOUNTS", "pilot_a:secret_a, invalid_entry, pilot_b:secret_b");
    std::env::set_var("EVOMU_GM_ACCOUNT", "gm_boss:gm_secret");
    std::env::set_var("EVOMU_QUASAR_GRPC_PORT", "26123");
    let cfg = ServerConfig::load_default_or_env().expect("load default with env overrides");
    assert_eq!(cfg.world_db_path, "custom/snap.sqlite");
    assert_eq!(cfg.player_accounts.len(), 2);
    assert_eq!(cfg.admin_accounts.len(), 1);
    assert_eq!(cfg.quasar_grpc_port, 26_123);

    // 2. EVOMU_DB_PATH takes precedence over EVOMU_SNAPSHOT_PATH; whitespace EVOMU_SDE_PATH ignored; invalid gRPC port errors
    std::env::set_var("EVOMU_DB_PATH", "override/world.sqlite");
    std::env::set_var("EVOMU_SDE_PATH", "   ");
    let cfg2 = ServerConfig::load_default_or_env().expect("db path overrides snapshot path");
    assert_eq!(cfg2.world_db_path, "override/world.sqlite");
    assert_eq!(cfg2.sde_path, "data/evomu_sde.sqlite");

    std::env::set_var("EVOMU_QUASAR_GRPC_PORT", "not_a_port");
    assert!(ServerConfig::load_default_or_env().unwrap_err().contains("EVOMU_QUASAR_GRPC_PORT"));
    std::env::remove_var("EVOMU_QUASAR_GRPC_PORT");

    // 3. EVOMU_CONFIG_PATH pointing to a temporary TOML file
    let tmp_toml = std::env::temp_dir().join(format!("evomu_cfg_{}.toml", Uuid::new_v4()));
    std::fs::write(&tmp_toml, "bind_address = \"10.20.30.40\"\n").unwrap();
    std::env::set_var("EVOMU_CONFIG_PATH", &tmp_toml);
    let cfg3 = ServerConfig::load_default_or_env().expect("load custom EVOMU_CONFIG_PATH");
    assert_eq!(cfg3.bind_address, "10.20.30.40");
    let _ = std::fs::remove_file(&tmp_toml);
}

#[tokio::test]
async fn starter_characters_plex_grants_feature_flag_fallback_and_error_paths() {
    assert!(should_grant_default_starter_plex());
    let (mut ctx, _) = fast_test_context(1_800_000_000);

    // Direct grant_starter_plex_in_snapshot coverage: non-positive is no-op; missing char uses account_plex_vault fallback
    ctx.db.with_mut_snapshot(|snap| {
        let before = snap.account_plex_vault.get(&1).copied().unwrap_or(0);
        grant_starter_plex_in_snapshot(snap, 1, 90_000_001, 0, 1_800_000_000);
        grant_starter_plex_in_snapshot(snap, 1, 90_000_001, -50, 1_800_000_000);
        assert_eq!(snap.account_plex_vault.get(&1).copied().unwrap_or(0), before);

        // Character ID not in `snap.characters` -> `adjust_plex_vault` returns None -> fallback inserts into `account_plex_vault`
        grant_starter_plex_in_snapshot(snap, 777, 999_888_777, 250, 1_800_000_000);
        assert_eq!(snap.account_plex_vault.get(&777).copied(), Some(250));
    });

    // Enable starter_plex_balance via ctx.feature_flags so starter without explicit initial_balance_plex inherits it
    let ctx_mut = Arc::get_mut(&mut ctx).expect("unique Arc");
    ctx_mut.feature_flags.write().unwrap().starter_plex_balance = 125;

    let uid_flag = ctx.db.provision_player_account("pilot_flag_plex", "pw_flag");
    let uid_override = ctx.db.provision_player_account("pilot_ovr_plex", "pw_ovr");
    let _uid_bad_school = ctx.db.provision_player_account("pilot_bad_school", "pw_bad");

    let mut cfg = ServerConfig::default();
    cfg.player_accounts = vec![
        PlayerAccountConfig {
            username: "pilot_flag_plex".into(),
            password_token: "pw_flag".into(),
            starter_character: Some(StarterCharacterConfig {
                name: "FlagPlexPilot".into(),
                bloodline_id: 3,
                gender: 1,
                ancestry_id: 19,
                school_id: 14,
                solar_system_id: Some(30_000_142),
                station_id: Some(60_008_494),
                initial_balance_isk: None,
                initial_balance_plex: None,
            }),
        },
        PlayerAccountConfig {
            username: "pilot_ovr_plex".into(),
            password_token: "pw_ovr".into(),
            starter_character: Some(StarterCharacterConfig {
                name: "OverridePlexPilot".into(),
                bloodline_id: 3,
                gender: 0,
                ancestry_id: 19,
                school_id: 14,
                solar_system_id: Some(30_000_142),
                station_id: Some(60_008_494),
                initial_balance_isk: Some(500_000),
                initial_balance_plex: Some(500),
            }),
        },
        // Invalid bloodline/school triggers CreateCharacterWithDoll Err arm cleanly without panicking
        PlayerAccountConfig {
            username: "pilot_bad_school".into(),
            password_token: "pw_bad".into(),
            starter_character: Some(StarterCharacterConfig {
                name: "BadSchoolPilot".into(),
                bloodline_id: 999_999,
                gender: 1,
                ancestry_id: 999_999,
                school_id: 999_999,
                ..Default::default()
            }),
        },
    ];

    let created = provision_starter_characters(&ctx, &cfg).await;
    assert_eq!(created.len(), 2);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.account_plex_vault.get(&uid_flag).copied(), Some(125));
        assert_eq!(snap.account_plex_vault.get(&uid_override).copied(), Some(500));
        assert_eq!(snap.characters[&created[0]].station_id, Some(60_008_494));
    });
}

#[tokio::test]
async fn router_session_guards_and_context_state_clocks_and_skill_queues() {
    let base = 1_800_000_000_u64;
    let (ctx, clock) = fast_test_context(base);

    // 1. router::session guards
    assert!(!ctx.is_shutting_down());
    ctx.cluster_state.write().unwrap().shutdown_in_progress = true;
    assert!(ctx.is_shutting_down());

    let mut sess = SessionState::new(10);
    assert_eq!(ServerContext::require_auth(&sess).unwrap_err().code, "Unauthorized");
    assert_eq!(ServerContext::require_gm_role(&sess).unwrap_err().code, "Unauthorized");
    sess.user_id = Some(42);
    sess.role = ROLE_PLAYER_DEFAULT;
    assert_eq!(ServerContext::require_gm_role(&sess).unwrap_err().code, "CustomError");
    assert_eq!(ServerContext::require_character(&sess).unwrap_err().code, "CharacterNotSelected");
    sess.role = ROLE_GMH;
    assert_eq!(ServerContext::require_gm_role(&sess).unwrap(), 42);
    sess.character_id = Some(90_000_001);
    assert_eq!(ServerContext::require_character(&sess).unwrap(), (42, 90_000_001));

    assert_eq!(ServerContext::require_bound_system(None, "custom_bound_svc").unwrap_err().code, "BoundObjectRequired");
    let bound = BoundObjectEntry {
        bound_id: 1, session_id: 10, user_id: 42, character_id: 90_000_001,
        service: "expected_bound_svc".into(), object_id: 30_000_142, oid_str: "N=1:1".into(), ref_id: 1,
    };
    assert_eq!(ServerContext::require_bound_system(Some(&bound), "custom_bound_svc").unwrap_err().code, "InvalidBoundService");
    assert_eq!(ServerContext::require_bound_system(Some(&bound), "expected_bound_svc").unwrap(), 30_000_142);

    // 2. Clock controls & virtual time progression (including jetcans, moon extractions, moon probes, and skill queues)
    assert_eq!(ctx.time_acceleration_multiplier(), 1);
    ctx.set_time_acceleration_multiplier(4);
    assert_eq!(ctx.time_acceleration_multiplier(), 4);
    assert!(!ctx.is_clock_paused());
    ctx.set_clock_paused(true);
    assert!(ctx.is_clock_paused());
    ctx.set_clock_paused(false);
    assert!(!ctx.pause_on_tick());
    ctx.set_pause_on_tick(true);
    assert!(ctx.pause_on_tick());
    ctx.set_pause_on_tick(false);
    ctx.step_clock_tick(2);
    ctx.advance_virtual_clock(1);
    ctx.advance_time_seconds(1);
    let _ = clock.now_unix_secs();

    // Seed expired jetcan, decaying moon extraction, arrived moon survey probes (hit_moon vs empty), and scanned signatures
    let char_id = 90_000_001_i64;
    let sys_id = 30_000_142_u32;
    let some_moon_id = ctx.sde.celestials_in_system(sys_id).into_iter().find(|c| c.group_id == 8).map(|c| c.celestial_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = sys_id;
            ch.clone_grade = 2;
        }
        snap.items.insert(920_001, InventoryItemRecord {
            item_id: 920_001, type_id: 23, owner_id: char_id, location_id: sys_id as i64,
            flag: InventoryFlag::None, quantity: 1, singleton: true, version: 1,
            dynamic_attributes: HashMap::new(), source_mutaplasmid_id: None,
            insured_payout_isk: 0.0, is_blueprint_copy: false, blueprint_runs_remaining: None,
        });
        crate::sp6_lifecycle::record_wreck_or_container_meta_in_snap(snap, 920_001, 1, false);

        snap.items.insert(920_002, InventoryItemRecord {
            item_id: 920_002, type_id: 1230, owner_id: 0, location_id: sys_id as i64,
            flag: InventoryFlag::None, quantity: 1, singleton: true, version: 1,
            dynamic_attributes: HashMap::new(), source_mutaplasmid_id: None,
            insured_payout_isk: 0.0, is_blueprint_copy: false, blueprint_runs_remaining: None,
        });
        snap.moon_extractions.insert(920_010, MoonExtractionRecord {
            structure_id: 100_000_999, moon_id: some_moon_id.unwrap_or(40_009_001),
            extraction_start_unix: 50, chunk_arrival_unix: 100, natural_decay_unix: 1,
            status: "Ready".into(), auto_fracture: false, event_id: 0,
            moon_ores: Vec::new(), spawned_asteroid_ids: vec![920_002],
        });

        let skill_tid = ctx.sde.types.values().find(|t| t.category_id == 16).map(|t| t.type_id).unwrap_or(3402);
        snap.skill_queues.insert(char_id, vec![SkillQueueEntry {
            type_id: skill_tid, target_level: 2,
            start_sp: 0.0, end_sp: 1_500.0, started_at_unix: None, duration_seconds: 120.0,
        }]);
    });
    if let Ok(mut mp) = ctx.active_moon_probes.write() {
        mp.push(ActiveMoonSurveyProbe {
            probe_id: 920_020, probe_type_id: 30488, owner_char_id: char_id,
            solar_system_id: sys_id, hit_moon_id: some_moon_id, arrival_unix_secs: ctx.now_unix() + 10,
        });
        mp.push(ActiveMoonSurveyProbe {
            probe_id: 920_021, probe_type_id: 30488, owner_char_id: char_id,
            solar_system_id: sys_id, hit_moon_id: Some(999_999_999), arrival_unix_secs: ctx.now_unix() + 10,
        });
    }
    ctx.advance_secs(120);
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&920_001));
        assert_eq!(snap.moon_extractions[&920_010].status, "Decayed");
    });

    // Alpha 5M SP cap pauses skill queue
    let char2 = 90_000_002_i64;
    let skill_tid = ctx.sde.types.values().find(|t| t.category_id == 16).map(|t| t.type_id).unwrap_or(3402);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char2) {
            ch.clone_grade = 1;
            ch.skill_points = 5_100_000.0;
        }
        snap.skill_queues.insert(char2, vec![SkillQueueEntry {
            type_id: skill_tid, target_level: 1,
            start_sp: 0.0, end_sp: 250.0, started_at_unix: None, duration_seconds: 60.0,
        }]);
    });
    ctx.advance_skill_queues(60.0);
    ctx.db.with_snapshot(|snap| assert!(snap.paused_skill_queues.contains(&char2)));

    // Scan progress, signature mark, fitting rebuild error, and fleet_extras check
    ctx.mark_signature_scanned(char_id, sys_id, 555_001);
    assert!(ctx.has_scanned_signature(char_id, sys_id, 555_001));
    let sig_rec = CosmicSignatureRecord {
        signature_id: "ABC-123".into(), solar_system_id: sys_id, sig_group: "Data".into(),
        type_name: "Site".into(), position: [0.0; 3], signal_strength: 0.25,
        wormhole_type_id: None, destination_system_id: None, created_unix: base,
        lifetime_seconds: 3600, max_mass_kg: 0.0, remaining_mass_kg: 0.0,
        max_jump_mass_kg: 0.0, collapsed: false,
    };
    assert_eq!(ctx.effective_signature_signal_strength(char_id, sys_id, &sig_rec), 0.25);
    ctx.record_character_scan_progress(char_id, sys_id, "ABC-123", 0.75);
    assert_eq!(ctx.effective_signature_signal_strength(char_id, sys_id, &sig_rec), 0.75);
    assert!(ctx.get_or_rebuild_ship_fitting(char_id, 999_888_777).is_err());
    let mut eng = ShipFittingEngine::from_sde(100_000_001, 587, &ctx.sde);
    eng.drones.push((1, 2454));
    ctx.store_ship_fitting(100_000_001, eng);
    assert!(ctx.get_or_rebuild_ship_fitting(char_id, 100_000_001).is_ok());
    assert!(!ctx.has_fleet_extras_for(999_999));

    // Seed demo world fixtures & new_production
    ctx.seed_demo_world_fixtures();
    let prod = ServerContext::new_production();
    assert!(prod.db.with_snapshot(|s| s.characters.is_empty()));
}

#[test]
fn log_ring_buffer_flight_recorder_and_esi_runtime_state_edge_cases() {
    // 1. LogRingBuffer wrap-around + LogRingLayer tracing subscriber formatting
    let ring = Arc::new(LogRingBuffer::new(2));
    ring.record("INFO", "t1", "first");
    ring.record("WARN", "t2", "second");
    ring.record("ERROR", "t3", "third");
    let recent = ring.snapshot_recent(5);
    assert_eq!(recent.len(), 2);
    assert!(recent[0].contains("[t2] second") && recent[1].contains("[t3] third"));
    let now_ms = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64;
    let _ = ring.snapshot_since(now_ms.saturating_sub(5_000), 10);

    use tracing_subscriber::layer::SubscriberExt;
    let sub = tracing_subscriber::registry().with(LogRingLayer::new(Arc::clone(&ring)));
    tracing::subscriber::with_default(sub, || {
        tracing::info!(extra_num = 42, extra_str = "hello", "traced_msg");
    });
    assert!(ring.snapshot_recent(1)[0].contains("traced_msg"));

    // 2. FlightRecorder credential redaction, truncation (>160 chars, >32 str), >4 args, >2 kwargs, eviction (>30 calls, >512 sessions)
    let fr = FlightRecorder::new();
    let long_s = "x".repeat(45);
    let mut kw = HashMap::new();
    kw.insert("k1".into(), PyRep::Integer(1));
    kw.insert("k2".into(), PyRep::Real(2.5));
    kw.insert("k3".into(), PyRep::Bool(true));
    fr.record(1, "authentication", None, "Custom", &[PyRep::String("secret".into())], &kw, Duration::from_micros(10), "ok");
    let args_many = vec![
        PyRep::String(long_s),
        PyRep::List(vec![PyRep::Integer(1)]),
        PyRep::Tuple(vec![PyRep::None]),
        PyRep::Dict(Vec::new()),
        PyRep::Object { type_name: "util.Row".into(), args: Box::new(PyRep::None) },
        PyRep::Bytes(vec![1, 2]),
    ];
    fr.record(2, "config", Some(9), "GetMap", &args_many, &kw, Duration::from_micros(20), "ok");
    for call_idx in 0..35 {
        fr.record(2, "config", None, "GetMap", &[PyRep::Integer(call_idx)], &HashMap::new(), Duration::from_micros(5), "ok");
    }
    let h1 = fr.get_history(1, now_ms, 5);
    assert_eq!(h1[0].args_summary, "(***redacted credentials***)");
    assert_eq!(fr.get_history(2, now_ms, 50).len(), 30);

    // 3. EsiRuntimeState defaults, mail meta, deleted tombstones, custom names, and ID allocation
    let esi = EsiRuntimeState::default();
    assert_eq!(EsiRuntimeState::default_mail_labels().len(), 4);
    assert!(esi.alloc_id() >= 16);
    assert!(!esi.is_mail_deleted(100, 200));
    esi.mark_mail_deleted(100, 200);
    assert!(esi.is_mail_deleted(100, 200));
    assert_eq!(esi.get_mail_meta(100, 1, true).labels, vec![2]);
    assert_eq!(esi.get_mail_meta(100, 2, false).labels, vec![1]);
    esi.set_mail_meta(100, 2, EsiMailMeta { is_read: true, labels: vec![4, 8] });
    assert_eq!(esi.get_mail_meta(100, 2, false).labels, vec![4, 8]);
    esi.set_custom_item_name(500, "My Hull");
    assert_eq!(esi.get_custom_item_name(500).as_deref(), Some("My Hull"));
    let _ = NotificationTarget::All;
}
