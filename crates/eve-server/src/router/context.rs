//! Server execution context, runtime state caches, and simulation clock management.

use crate::living_world::LivingWorldDirector;
use crate::system_actor::SystemRegistry;
use crate::world_seeder;
use eve_db::WorldRepository;
use eve_dogma::ShipFittingEngine;
use eve_proto::BridgeRpcError;
use eve_sde::SdeRegistry;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, RwLock};

pub static NEXT_BOUND_ID: AtomicU64 = AtomicU64::new(10_000);

pub const EMBEDDED_RPC_CATALOG_JSON: &str =
    include_str!("../../../../docs/index/rpc_service_catalog.json");
pub const EMBEDDED_QUASAR_CATALOG_JSON: &str =
    include_str!("../../../../docs/index/quasar_protobuf_catalog.json");

#[derive(Debug, Clone)]
pub struct BoundObjectEntry {
    pub bound_id: u64,
    pub session_id: u64,
    pub user_id: u64,
    pub character_id: i64,
    pub service: String,
    pub object_id: i64,
}

static NEXT_CTX_INSTANCE_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
pub(crate) fn next_ctx_instance_id() -> u64 {
    NEXT_CTX_INSTANCE_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

pub struct ServerContext {
    pub instance_id: u64,
    pub sde: Arc<SdeRegistry>,
    pub db: Arc<WorldRepository>,
    pub clock: RwLock<Arc<dyn eve_db::ServerClock>>,
    pub systems: Arc<SystemRegistry>,
    pub living_world: Arc<LivingWorldDirector>,
    pub quasar_gateway: Arc<crate::quasar_gateway::PublicGatewayRouter>,
    pub cataloged_services: HashSet<String>,
    pub quasar_domains: HashSet<String>,
    pub bound_objects: RwLock<HashMap<u64, BoundObjectEntry>>,
    pub object_cache: RwLock<HashMap<String, crate::router::cache_notifications::CachedObjectRecord>>,
    pub object_cache_timestamps: RwLock<HashMap<String, i64>>,
    pub notification_records: RwLock<Vec<crate::router::cache_notifications::NotificationRecord>>,
    pub pending_notifications: Arc<RwLock<HashMap<i64, Vec<crate::router::cache_notifications::PendingMachoNotification>>>>,
    pub notify_tx: tokio::sync::broadcast::Sender<(i64, crate::router::cache_notifications::PendingMachoNotification)>,
    pub map_landmarks: RwLock<Vec<(i64, String, f64, f64, f64)>>,
    pub session_closers: RwLock<HashMap<u64, tokio::sync::mpsc::UnboundedSender<(String, String)>>>,
    pub ship_fittings: Arc<RwLock<HashMap<i64, ShipFittingEngine>>>,
    pub projected_ewar: RwLock<HashMap<i64, Vec<eve_dogma::ProjectedEwarEffect>>>,
    pub weapon_cycle_state: RwLock<HashMap<i64, (Option<u64>, u32)>>,
    pub fighter_cooldown_state: RwLock<HashMap<(i64, u32), u64>>,
    pub fighter_charge_state: RwLock<HashMap<(i64, u32), u32>>,
    pub active_fighters: RwLock<HashMap<i64, ActiveFighterSquadron>>,
    pub fighter_tube_loads: RwLock<HashMap<(i64, usize), i64>>,
    pub active_scanner_probes: RwLock<HashMap<i64, ActiveScannerProbe>>,
    pub active_moon_probes: RwLock<Vec<ActiveMoonSurveyProbe>>,
    pub scanned_signatures: RwLock<HashSet<(i64, u32, i64)>>,
    pub character_scan_progress: RwLock<HashMap<(i64, u32, String), f64>>,
    pub invention_roll_override: Arc<RwLock<Option<f64>>>,
    pub mechanic_rng: Arc<crate::rng::MechanicRng>, // seeded test RNG: shared with `systems`; OS CSPRNG unless seeded
    pub module_reactivation_cooldowns: Arc<RwLock<HashMap<(i64, i64, u32), ModuleCooldownEntry>>>,
    /// Repeating mining-module cycles keyed by module item ID (`MN-04`).
    pub mining_cycles: RwLock<HashMap<i64, crate::services::mining_cycle::ActiveMiningCycle>>,
    pub crimewatch: crate::combat::crimewatch::CrimeWatchLock,
    pub(crate) combat_ephemeral: Arc<RwLock<crate::services::combat_crimewatch::CombatRuntimeState>>,
    pub tactical_state: Arc<RwLock<crate::services::tactical_modules::TacticalModulesState>>,
    pub session_change_timers: std::sync::Mutex<HashMap<i64, crate::services::space_destiny::SessionChangeTimerState>>,
    pub active_warps: std::sync::Mutex<HashMap<i64, i64>>,
    pub location_change_invuln: std::sync::Mutex<HashMap<i64, u64>>,
    pub feature_flags: crate::config::FeatureFlagsConfig,
    pub esi_port: u16,
    pub esi_state: Arc<crate::esi::runtime_state::EsiRuntimeState>,
    /// Per-world fleet state not persisted in `WorldRepository` (names, invites, adverts, watchlists).
    pub(crate) fleet_extras: std::sync::Mutex<crate::services::fleet_social::fleet_social_fleets::FleetExtrasState>,
    /// Per-world social state not persisted in `WorldRepository` (mailing lists, calendar, chat moderation).
    pub(crate) social_extras: std::sync::Mutex<crate::services::fleet_social::fleet_social_chat::SocialExtrasState>,
    /// Per-world cluster/macho/misc service state (`cluster_macho_misc_services`).
    pub(crate) cluster_misc_state: std::sync::Mutex<crate::services::cluster_macho_misc_services::ClusterMiscRuntimeState>,
    pub cluster_state: Arc<RwLock<crate::services::macho_cluster_mgmt::MachoClusterState>>,
    pub global_config_values: RwLock<HashMap<String, eve_proto::PyRep>>,
    pub alert_telemetry: std::sync::Mutex<crate::services::client_login::AlertTelemetryState>,
    pub npc_spawns: RwLock<HashMap<u32, crate::combat::npc_ai::NpcSpawnManager>>,
    pub combat_rng_seq: AtomicU64,
}

#[derive(Debug, Clone, Copy)]
pub struct ModuleCooldownEntry {
    pub recorded_unix_f64: f64,
    pub delay_s: f64,
    pub solar_system_id: Option<u32>,
    pub recorded_sys_sim_s: f64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ActiveFighterSquadron {
    pub ball_id: i64,
    pub item_id: i64,
    pub type_id: u32,
    pub owner_char_id: i64,
    pub parent_ship_id: i64,
    pub tube_index: usize,
    pub squadron_size: u32,
    pub target_id: Option<i64>,
    pub is_abandoned: bool,
    pub mwd_active: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ActiveScannerProbe {
    pub probe_id: i64,
    pub probe_type_id: u32,
    pub owner_char_id: i64,
    pub ship_id: i64,
    pub launcher_module_id: i64,
    pub solar_system_id: u32,
    pub position: [f64; 3],
    pub destination: [f64; 3],
    pub range_step: i32,
    pub scan_range_m: f64,
    pub base_sensor_strength: f64,
    pub max_flight_time_s: f64,
    pub launched_at_unix: u64,
    pub state: i32,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ActiveMoonSurveyProbe {
    pub probe_id: i64,
    pub probe_type_id: u32,
    pub owner_char_id: i64,
    pub solar_system_id: u32,
    pub hit_moon_id: Option<i64>,
    pub arrival_unix_secs: u64,
}

impl Default for ServerContext {
    fn default() -> Self {
        Self::new()
    }
}

impl ServerContext {
    /// Initializes a `ServerContext` with the provided `WorldRepository` and `SdeRegistry`, and seeds NPC regional markets.
    pub fn with_sde_and_repository(sde: Arc<SdeRegistry>, db: Arc<WorldRepository>) -> Self {
        let _ = world_seeder::seed_dual_anchor_markets(&db, &sde);
        let clock = db.clock();

        let catalog_map: HashMap<String, serde_json::Value> =
            serde_json::from_str(EMBEDDED_RPC_CATALOG_JSON).unwrap_or_default();
        let cataloged_services: HashSet<String> = catalog_map.into_keys().collect();
        let quasar_map: HashMap<String, serde_json::Value> =
            serde_json::from_str(EMBEDDED_QUASAR_CATALOG_JSON).unwrap_or_default();
        let quasar_domains: HashSet<String> = quasar_map.into_keys().collect();
        let (notify_tx, _) = tokio::sync::broadcast::channel(256);
        let pending_notifications = Arc::new(RwLock::new(HashMap::new()));
        let ship_fittings = Arc::new(RwLock::new(HashMap::new()));
        let invention_roll_override = Arc::new(RwLock::new(None));
        let mechanic_rng = Arc::new(crate::rng::MechanicRng::os());
        let tactical_state = Arc::new(RwLock::new(
            crate::services::tactical_modules::TacticalModulesState::default(),
        ));
        let systems = Arc::new(
            SystemRegistry::with_sde_and_db(sde.clone(), Arc::clone(&db))
                .with_mechanic_rng(Arc::clone(&mechanic_rng))
                .with_tactical_state(Arc::clone(&tactical_state)),
        );
        let combat_ephemeral = Arc::new(RwLock::new(crate::services::combat_crimewatch::CombatRuntimeState::default()));
        let quasar_gateway = Arc::new(crate::quasar_gateway::PublicGatewayRouter::new(
            eve_proto::quasar::QuasarDescriptorRegistry::open_default_or_empty(),
            Arc::clone(&db),
            Arc::clone(&sde),
        ));
        // The NPC death hook publishes Quasar notices (e.g. incursion tale stop), so it holds a
        // weak gateway handle alongside the other weak service handles.
        let gw_w = Arc::downgrade(&quasar_gateway);
        let (sys_w, db_w, sde_w, rng_w, pend_w, ntx, ce_w) = (
            Arc::downgrade(&systems),
            Arc::downgrade(&db),
            Arc::downgrade(&sde),
            Arc::downgrade(&mechanic_rng),
            Arc::downgrade(&pending_notifications),
            notify_tx.clone(),
            Arc::downgrade(&combat_ephemeral),
        );
        systems.set_npc_death_hook(Arc::new(move |sys_id, npc_id, killer_opt| {
            let gww = gw_w.clone();
            let (sw, dw, sdw, rw, pw, tx, cew) = (
                sys_w.clone(),
                db_w.clone(),
                sde_w.clone(),
                rng_w.clone(),
                pend_w.clone(),
                ntx.clone(),
                ce_w.clone(),
            );
            Box::pin(async move {
                if let (Some(sys), Some(db), Some(sde)) = (sw.upgrade(), dw.upgrade(), sdw.upgrade()) {
                    let inc = crate::services::pve_incursion_sites::on_incursion_boss_destroyed_core(&sys, &db, &sde, sys_id, npc_id, killer_opt).await;
                    if let (Some(Ok(r)), Some(gw)) = (&inc, gww.upgrade()) {
                        crate::services::pve_incursion_sites::publish_incursion_tale_stopped_if_ended(&gw, &db, r);
                    }
                    let acct_changes = crate::services::pve_special_sites::on_special_site_npc_destroyed_core(&sys, &db, &sde, sys_id, npc_id, killer_opt).await;
                    let _ = crate::services::pve_special_sites_abyssal::on_abyssal_room_npc_destroyed_core(&sys, &db, sys_id, npc_id).await;
                    crate::services::pve_special_sites_abyssal::on_abyssal_cache_destroyed_core(&sys, &db, sys_id, npc_id, killer_opt).await;
                    if let Some(pws) = pw.upgrade() {
                        crate::services::pve_special_sites::notify_special_site_account_changes(&db, &pws, &tx, &acct_changes);
                    }
                    crate::services::agents_missions::on_mission_npc_destroyed_core(&db, sys_id, npc_id, killer_opt);
                    if let Some(ce) = cew.upgrade() {
                        crate::services::combat_npc_encounters::handle_npc_destroyed_in_space(&sys, &db, &sde, &ce, sys_id, npc_id, killer_opt).await;
                    }
                    if let (Some(rng), Some(ce), Some(kid)) = (rw.upgrade(), cew.upgrade(), killer_opt) {
                        let (clr, esc, step) = crate::services::pve_exploration::handle_pve_site_npc_destruction_core(&sys, &db, &sde, &rng, &ce, sys_id, npc_id, kid, true).await;
                        if let Some(pws) = pw.upgrade() {
                            crate::services::pve_exploration::notify_pve_destruction_events(&db, &pws, &tx, kid, clr, esc, step);
                        }
                    }
                    if let Some(sh_info) = crate::services::insurgency_services::on_insurgency_stronghold_destroyed_core(&sys, &db, sys_id, npc_id, killer_opt).await {
                        crate::services::insurgency_services::notify_stronghold_destruction(
                            &db,
                            gww.upgrade().as_deref(),
                            pw.upgrade().as_deref(),
                            Some(&tx),
                            &sh_info,
                        );
                    }
                }
            })
        }));
        let (step_sys_w, step_db_w, step_sde_w, step_rng_w, step_fit_w, step_pend_w, step_ntx, step_ce_w, step_gw_w, step_inv_w) = (
            Arc::downgrade(&systems), Arc::downgrade(&db), Arc::downgrade(&sde), Arc::downgrade(&mechanic_rng),
            Arc::downgrade(&ship_fittings), Arc::downgrade(&pending_notifications), notify_tx.clone(),
            Arc::downgrade(&combat_ephemeral), Arc::downgrade(&quasar_gateway), Arc::downgrade(&invention_roll_override),
        );
        let module_reactivation_cooldowns = Arc::new(RwLock::new(HashMap::new()));
        let step_cd_w = Arc::downgrade(&module_reactivation_cooldowns);
        systems.set_system_step_hook(Arc::new(move |sys_id, dt, destroyed_balls| {
            let (sw, dw, sdw, rw, fit_w, pend_w, ntx, cew, gww, inw, cdw) = (
                step_sys_w.clone(), step_db_w.clone(), step_sde_w.clone(), step_rng_w.clone(),
                step_fit_w.clone(), step_pend_w.clone(), step_ntx.clone(), step_ce_w.clone(), step_gw_w.clone(), step_inv_w.clone(), step_cd_w.clone(),
            );
            Box::pin(async move {
                if let (Some(sys), Some(db), Some(sde), Some(rng), Some(fit), Some(pend), Some(ce), Some(gw), Some(inv_roll), Some(cd_map)) = (
                    sw.upgrade(), dw.upgrade(), sdw.upgrade(), rw.upgrade(), fit_w.upgrade(), pend_w.upgrade(), cew.upgrade(), gww.upgrade(), inw.upgrade(), cdw.upgrade(),
                ) {
                    let ctx = ServerContext::from_step_components(sys, db, sde, rng, fit, pend, ntx, ce, gw, inv_roll, cd_map);
                    for (tid, killer_opt) in destroyed_balls {
                        if let Some((vcid, acid, pos)) = ctx.db.with_snapshot(|s| {
                            let vc = s.characters.values().find(|c| c.active_ship_id == tid)?;
                            let acid = killer_opt.and_then(|kid| {
                                s.characters.get(&kid).map(|c| c.character_id)
                                    .or_else(|| s.characters.values().find(|c| c.active_ship_id == kid).map(|c| c.character_id))
                            }).unwrap_or(0);
                            let pos = s.item_positions.get(&tid).copied().unwrap_or([0.0; 3]);
                            Some((vc.character_id, acid, pos))
                        }) {
                            let _ = crate::combat::death_pipeline::execute_death_pipeline_sync(&ctx, sys_id, vcid, tid, acid, glam::DVec3::from_array(pos), ctx.now_unix());
                        }
                    }
                    crate::services::combat_weapons_spool::step_system_entropic_spools(&ctx, sys_id, dt).await;
                    crate::services::inventory_fitting::execute_pending_cycle_actions(&ctx, sys_id).await;
                }
            })
        }));
        let persisted_notifications = db.with_snapshot(|snap| {
            let mut list: Vec<crate::router::cache_notifications::NotificationRecord> = snap
                .owner_notes
                .values()
                .filter(|n| n.character_id == -79)
                .filter_map(|n| serde_json::from_str(&n.note).ok())
                .collect();
            list.sort_by_key(|r| r.notification_id);
            list
        });

        let instance_id = NEXT_CTX_INSTANCE_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if db.is_ephemeral_in_memory() {
            db.with_mut_snapshot(|snap| {
                let corp_floor = 98_100_000 + (instance_id as i64) * 10_000;
                let al_floor = 99_000_000 + (instance_id as i64) * 1_000;
                snap.next_corporation_id = snap.next_corporation_id.max(corp_floor);
                snap.next_alliance_id = snap.next_alliance_id.max(al_floor);
            });
        }
        let crimewatch = crate::combat::crimewatch::CrimeWatchLock::new(Arc::clone(&db));
        let cluster_misc =
            crate::services::cluster_macho_misc_services::ClusterMiscRuntimeState::from_db(&db);
        let persisted_global_config: HashMap<String, eve_proto::PyRep> = db.with_snapshot(|snap| {
            snap.service_state_blobs
                .get("macho_global_config")
                .and_then(|s| serde_json::from_str(s).ok())
                .unwrap_or_default()
        });
        let ctx = Self {
            instance_id,
            sde,
            db,
            clock: RwLock::new(clock),
            systems,
            living_world: Arc::new(LivingWorldDirector::new()),
            quasar_gateway,
            cataloged_services,
            quasar_domains,
            bound_objects: RwLock::new(HashMap::new()),
            object_cache: RwLock::new(HashMap::new()),
            object_cache_timestamps: RwLock::new(HashMap::new()),
            notification_records: RwLock::new(persisted_notifications),
            pending_notifications,
            notify_tx,
            map_landmarks: RwLock::new(Vec::new()),
            session_closers: RwLock::new(HashMap::new()),
            ship_fittings,
            projected_ewar: RwLock::new(HashMap::new()),
            weapon_cycle_state: RwLock::new(HashMap::new()),
            fighter_cooldown_state: RwLock::new(HashMap::new()),
            fighter_charge_state: RwLock::new(HashMap::new()),
            active_fighters: RwLock::new(HashMap::new()),
            fighter_tube_loads: RwLock::new(HashMap::new()),
            active_scanner_probes: RwLock::new(HashMap::new()),
            active_moon_probes: RwLock::new(Vec::new()),
            scanned_signatures: RwLock::new(HashSet::new()),
            character_scan_progress: RwLock::new(HashMap::new()),
            invention_roll_override,
            mechanic_rng,
            module_reactivation_cooldowns,
            mining_cycles: RwLock::new(HashMap::new()),
            crimewatch,
            combat_ephemeral,
            tactical_state,
            session_change_timers: std::sync::Mutex::new(HashMap::new()),
            active_warps: std::sync::Mutex::new(HashMap::new()),
            location_change_invuln: std::sync::Mutex::new(HashMap::new()),
            feature_flags: crate::config::FeatureFlagsConfig::default(),
            esi_port: 26_004,
            esi_state: Arc::new(crate::esi::runtime_state::EsiRuntimeState::new()),
            fleet_extras: std::sync::Mutex::new(Default::default()),
            social_extras: std::sync::Mutex::new(crate::services::fleet_social::fleet_social_chat::SocialExtrasState::new()),
            cluster_misc_state: std::sync::Mutex::new(cluster_misc),
            cluster_state: Arc::new(RwLock::new(crate::services::macho_cluster_mgmt::MachoClusterState::default())),
            global_config_values: RwLock::new(persisted_global_config),
            alert_telemetry: std::sync::Mutex::new(crate::services::client_login::AlertTelemetryState::default()),
            npc_spawns: RwLock::new(HashMap::new()),
            combat_rng_seq: AtomicU64::new(1),
        };
        ctx.db.with_mut_snapshot(|snap| {
            snap.module_active_states.clear();
            for item in snap.items.values_mut() {
                if item.dynamic_attributes.contains_key(&2114) { continue; }
                if ctx.sde.types.get(&item.type_id).is_some_and(|t| t.category_id == 6) {
                    for aid in [267, 268, 269, 270, 271, 272, 273, 274, 109, 110, 111, 112, 113] { item.dynamic_attributes.remove(&aid); }
                }
            }
        });
        crate::services::fleet_social::hydrate_social_state_from_db(&ctx);
        crate::sp6_lifecycle::seed_persisted_abandoned_drones(&ctx);
        ctx.wire_module_deactivation_hook();
        ctx
    }

    /// Initializes a `ServerContext` with the provided `WorldRepository` and seeds the NPC regional markets.
    pub fn with_repository(db: Arc<WorldRepository>) -> Self {
        let sde = Arc::new(SdeRegistry::with_canonical_seed());
        Self::with_sde_and_repository(sde, db)
    }

    /// Initializes a `ServerContext` backed by a SQLite world database at `path`.
    pub fn from_sqlite(path: impl AsRef<std::path::Path>) -> Result<Self, String> {
        let db = Arc::new(WorldRepository::open_sqlite(path)?);
        Ok(Self::with_repository(db))
    }

    /// Initializes a `ServerContext` from a [`crate::config::ServerConfig`] (`D-5`).
    pub fn from_config(cfg: &crate::config::ServerConfig) -> Result<Self, String> {
        let sde = Arc::new(SdeRegistry::open(&cfg.sde_path).map_err(|e| e.to_string())?);
        let db = Arc::new(WorldRepository::new_empty_with_snapshot_path(Some(std::path::PathBuf::from(&cfg.world_db_path)))?);
        for p in &cfg.player_accounts {
            if !p.username.trim().is_empty() && !p.password_token.trim().is_empty() {
                db.provision_player_account(p.username.trim(), p.password_token.trim());
            }
        }
        for a in &cfg.admin_accounts {
            if !a.username.trim().is_empty() && !a.password_token.trim().is_empty() {
                db.provision_gm_account(a.username.trim(), a.password_token.trim(), a.character_id, a.character_name.trim());
            }
        }
        let mut ctx = Self::with_sde_and_repository(sde, db);
        ctx.systems.set_belt_respawn_interval_secs(cfg.belt_respawn_interval_secs);
        ctx.systems.set_downtime_utc_secs_of_day(cfg.downtime_utc_secs_of_day);
        ctx.feature_flags = cfg.feature_flags.clone();
        ctx.esi_port = cfg.esi_port;
        if cfg.feature_flags.seed_demo {
            ctx.seed_demo_world_fixtures();
        }
        Ok(ctx)
    }

    /// Explicitly seeds demo convoys (`800_000_001..=800_000_003`), nullsec ESS bank balances (`10M`/`25M` ISK),
    /// and demo wormhole/FW-complex fixtures when `--seed-demo` or a test fixture explicitly requests them (`S-5`).
    pub fn seed_demo_world_fixtures(&self) {
        self.living_world.seed_default_convoys();
        self.db.with_mut_snapshot(|snap| {
            if let Some(st) = snap.system_states.get_mut(&30_002_813) {
                st.wormhole_remaining_mass_kg = 2_000_000_000.0;
                st.wormhole_destination_system_id = Some(31_000_005);
                st.signature_position = Some([15_000.0, 0.0, 0.0]);
                st.fw_complex_position = Some([250_000.0, 0.0, -150_000.0]);
                st.fw_contested_pct = 45.0;
                st.fw_capture_seconds_elapsed = 270;
                st.fw_capture_duration_seconds = 600;
            }
            if let Some(st) = snap.system_states.get_mut(&31_000_005) {
                st.wormhole_remaining_mass_kg = 2_000_000_000.0;
                st.wormhole_destination_system_id = Some(30_002_904);
                st.signature_position = Some([10_500.0, 0.0, 0.0]);
            }
            if let Some(st) = snap.system_states.get_mut(&30_002_904) {
                st.ess_main_bank = eve_db::IskCents::from_isk(10_000_000);
                st.ess_reserve_bank = eve_db::IskCents::from_isk(25_000_000);
                st.wormhole_remaining_mass_kg = 2_000_000_000.0;
                st.wormhole_destination_system_id = Some(30_002_813);
                st.signature_position = Some([12_000.0, 0.0, 0.0]);
            }
        });
    }

    /// Initializes the production server context (`R22-P1-5`, `D-4`): always starts with an empty
    /// account/character table (`WorldRepository::new_empty()`) without demo secrets, and provisions
    /// operator-supplied player and admin accounts from `EVOMU_ACCOUNTS` / `EVOMU_GM_ACCOUNT`.
    pub fn new_production() -> Self {
        let db = Arc::new(WorldRepository::new_empty());
        if let Ok(accounts_spec) = std::env::var("EVOMU_ACCOUNTS") {
            for entry in accounts_spec.split(',') {
                if let Some((user, pass)) = entry.trim().split_once(':') {
                    let u = user.trim();
                    let p = pass.trim();
                    if !u.is_empty() && !p.is_empty() {
                        db.provision_player_account(u, p);
                    }
                }
            }
        }
        if let Ok(gm_spec) = std::env::var("EVOMU_GM_ACCOUNT") {
            if let Some((user, pass)) = gm_spec.trim().split_once(':') {
                let u = user.trim();
                let p = pass.trim();
                if !u.is_empty() && !p.is_empty() {
                    db.provision_gm_account(u, p, 90_000_003, "ISD Polaris");
                }
            }
        }
        Self::with_repository(db)
    }

    pub fn new() -> Self {
        #[cfg(any(test, feature = "demo-seed"))]
        let db = {
            let disable_demo = std::env::var("EVOMU_DISABLE_DEMO_SEED").map(|v| v == "1" || v.eq_ignore_ascii_case("true")).unwrap_or(false);
            if disable_demo { Arc::new(WorldRepository::new_empty()) } else { Arc::new(WorldRepository::with_demo_seed()) }
        };
        #[cfg(not(any(test, feature = "demo-seed")))]
        let db = Arc::new(WorldRepository::new_empty());
        let ctx = Self::with_repository(db);
        #[cfg(any(test, feature = "demo-seed"))]
        let ctx = ctx.with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED);
        ctx
    }

    pub fn mark_signature_scanned(&self, char_id: i64, solar_system_id: u32, sig_ball_id: i64) {
        if self.scanned_signatures.write().unwrap_or_else(|e| e.into_inner()).insert((char_id, solar_system_id, sig_ball_id)) {
            self.quasar_gateway.progression.daily_goals.on_gameplay_event(char_id as u32, 3, 1, &self.quasar_gateway);
            self.quasar_gateway.progression.career_goals.on_gameplay_event(char_id as u32, "exploration", 1, &self.quasar_gateway);
            self.quasar_gateway.progression.extra.on_campaign_gameplay_event(char_id as u32, crate::services::quasar_campaign_state::CampaignEvent { method: "ScanSignature", type_id: None, solar_system_id: Some(solar_system_id), faction_id: None }, 1, self.now_unix(), &self.quasar_gateway);
        }
    }

    pub fn has_scanned_signature(
        &self,
        char_id: i64,
        solar_system_id: u32,
        sig_ball_id: i64,
    ) -> bool {
        self.scanned_signatures
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&(char_id, solar_system_id, sig_ball_id))
    }

    pub fn record_character_scan_progress(
        &self,
        char_id: i64,
        solar_system_id: u32,
        signature_id: &str,
        strength: f64,
    ) {
        self.character_scan_progress
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                (char_id, solar_system_id, signature_id.to_string()),
                strength,
            );
    }

    pub fn get_character_scan_progress(
        &self,
        char_id: i64,
        solar_system_id: u32,
        signature_id: &str,
    ) -> Option<f64> {
        self.character_scan_progress
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&(char_id, solar_system_id, signature_id.to_string()))
            .copied()
    }

    pub fn effective_signature_signal_strength(
        &self,
        char_id: i64,
        solar_system_id: u32,
        sig: &eve_db::CosmicSignatureRecord,
    ) -> f64 {
        self.get_character_scan_progress(char_id, solar_system_id, &sig.signature_id)
            .unwrap_or(sig.signal_strength)
    }

    pub fn character_skill_levels(&self, char_id: i64) -> HashMap<u32, u8> {
        let now_ft = self.now_blueos_filetime_ticks();
        self.db.with_snapshot(|snap| {
            let ch = snap.characters.get(&char_id);
            let is_alpha = ch.map(|c| c.clone_grade <= 1).unwrap_or(false);
            let race_id = ch.map(|c| c.race_id).unwrap_or(1);
            let mut levels: HashMap<u32, u8> = snap
                .skills
                .get(&char_id)
                .map(|m| {
                    m.iter()
                        .map(|(&tid, rec)| {
                            let lvl = if is_alpha {
                                let cap = crate::services::character_skills::alpha_skill_cap_for_character(
                                    &self.sde, race_id, tid,
                                );
                                rec.level.min(cap)
                            } else {
                                rec.level
                            };
                            (tid, lvl)
                        })
                        .collect()
                })
                .unwrap_or_default();

            let es_skills = crate::services::character_expert_systems::get_active_expert_system_skills(
                snap, char_id, now_ft,
            );
            for (skill_id, level) in es_skills {
                let entry = levels.entry(skill_id).or_insert(0);
                *entry = (*entry).max(level);
            }
            levels
        })
    }

    fn resolve_ship_cooldown_clock(&self, ship_id: i64, module_id: i64) -> (u32, Option<u32>, f64) {
        let (type_id, db_sys_opt) = self.db.with_snapshot(|s| {
            let tid = s.items.get(&module_id).map(|i| i.type_id).unwrap_or(0);
            let char_sys = s
                .characters
                .values()
                .find(|c| c.active_ship_id == ship_id)
                .map(|c| c.solar_system_id);
            let item_sys = s.items.get(&ship_id).and_then(|it| {
                (30_000_000..=33_000_000)
                    .contains(&it.location_id)
                    .then_some(it.location_id as u32)
            });
            (tid, char_sys.or(item_sys))
        });
        let sys_opt = self.systems.find_ball_system_sync(ship_id).or(db_sys_opt);
        let cur_sys_sim_s = sys_opt
            .map(|sid| self.systems.get_system_sim_elapsed(sid))
            .unwrap_or(0.0);
        (type_id, sys_opt, cur_sys_sim_s)
    }

    fn remaining_cooldown_secs(
        entry: &ModuleCooldownEntry,
        now_unix_f64: f64,
        sys_opt: Option<u32>,
        cur_sys_sim_s: f64,
    ) -> f64 {
        let wall_elapsed = (now_unix_f64 - entry.recorded_unix_f64).max(0.0);
        let sim_elapsed = if entry.solar_system_id.is_some() && entry.solar_system_id == sys_opt {
            (cur_sys_sim_s - entry.recorded_sys_sim_s).max(0.0)
        } else {
            0.0
        };
        entry.delay_s - wall_elapsed.max(sim_elapsed)
    }

    /// Checks if a module on a ship is currently under reactivation delay (`feat:dogma.reactivation-delay`, `DG-4`).
    pub fn is_module_in_reactivation_delay(&self, ship_id: i64, module_id: i64) -> bool {
        let now = self.now_unix_secs() as f64;
        let (type_id, sys_opt, cur_sys_sim_s) = self.resolve_ship_cooldown_clock(ship_id, module_id);
        let guard = self
            .module_reactivation_cooldowns
            .read()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(entry) = guard.get(&(ship_id, module_id, type_id)) {
            Self::remaining_cooldown_secs(entry, now, sys_opt, cur_sys_sim_s) > 1e-6
        } else {
            false
        }
    }

    /// Records module reactivation delay until `now + delay_s` (`feat:dogma.reactivation-delay`, `DG-4`).
    pub fn record_module_reactivation_delay(&self, ship_id: i64, module_id: i64, delay_s: f64) {
        if delay_s <= 0.0 {
            return;
        }
        let now = self.now_unix_secs() as f64;
        let (type_id, sys_opt, cur_sys_sim_s) = self.resolve_ship_cooldown_clock(ship_id, module_id);
        let mut guard = self
            .module_reactivation_cooldowns
            .write()
            .unwrap_or_else(|e| e.into_inner());
        let effective_delay = guard
            .get(&(ship_id, module_id, type_id))
            .map(|e| Self::remaining_cooldown_secs(e, now, sys_opt, cur_sys_sim_s).max(delay_s))
            .unwrap_or(delay_s);
        guard.insert(
            (ship_id, module_id, type_id),
            ModuleCooldownEntry {
                recorded_unix_f64: now,
                delay_s: effective_delay,
                solar_system_id: sys_opt,
                recorded_sys_sim_s: cur_sys_sim_s,
            },
        );
    }

    /// Returns the active [`eve_db::ServerClock`] implementation (`T-7`).
    pub fn clock(&self) -> Arc<dyn eve_db::ServerClock> {
        self.clock
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Injects a custom [`eve_db::ServerClock`] into both `ServerContext` and `WorldRepository` (`T-7`).
    pub fn set_clock(&self, clock: Arc<dyn eve_db::ServerClock>) {
        let now_u = clock.now_unix_secs();
        self.db.set_clock(clock.clone());
        *self.clock.write().unwrap_or_else(|e| e.into_inner()) = clock;
        self.db.with_mut_snapshot(|snap| {
            snap.service_state_blobs
                .insert("__server_now_unix".into(), now_u.to_string());
        });
    }

    /// Returns the current server Unix timestamp in seconds (`T-7`).
    pub fn now_unix_secs(&self) -> u64 {
        self.clock().now_unix_secs()
    }

    /// Alias for [`Self::now_unix_secs`] (`T-7`).
    pub fn now_unix(&self) -> u64 {
        self.now_unix_secs()
    }

    /// Synchronizes a live `SessionState` with current DB character spatial and vessel state.
    pub fn sync_session(&self, session: &mut eve_machonet::SessionState) {
        if let Some(cid) = session.character_id {
            if let Some((new_corp, new_ally, new_role)) = crate::services::fleet_social::fleet_social_persistence::with_social_state(self, |s| {
                s.forced_session_corp_sync.remove(&cid)
            }) {
                session.corporation_id = Some(new_corp);
                session.alliance_id = new_ally;
                session.corp_role = new_role;
            }
            if let Some((act_ship, st_id, str_id, sys_id, const_id, reg_id)) = self.db.with_snapshot(|snap| {
                snap.characters.get(&cid).map(|c| (c.active_ship_id, c.station_id, c.structure_id, c.solar_system_id, c.constellation_id, c.region_id))
            }) {
                session.solar_system_id = Some(sys_id);
                if const_id != 0 { session.constellation_id = Some(const_id); }
                if reg_id != 0 { session.region_id = Some(reg_id); }
                session.station_id = st_id;
                session.structure_id = str_id;
                if act_ship != 0 { session.ship_id = Some(act_ship); }
            }
        }
    }

    /// Advances virtual server time by `secs` seconds and runs time-driven skill queue,
    /// jetcan despawn (3600s lifetime), moon field natural decay progression (`T-7`), and
    /// SP-6 / SP-7 background cluster lifecycle timers (`plan:SP-6`, `plan:SP-7`).
    pub fn advance_secs(&self, secs: u64) {
        let prev_unix = self.now_unix_secs();
        self.clock().advance_secs(secs);
        let cur_unix = self.now_unix_secs();
        self.db.with_mut_snapshot(|snap| {
            snap.service_state_blobs
                .insert("__server_now_unix".into(), cur_unix.to_string());
        });
        if secs > 0 {
            crate::sp6_lifecycle::on_advance_secs(self, prev_unix, secs);
            crate::services::mining_advanced::tick_all_metenox_moon_drills(
                self,
                prev_unix,
                self.now_unix_secs(),
            );
            let now_after = self.now_unix_secs();
            let dt_sec = self.systems.downtime_utc_secs_of_day();
            if secs >= 86_400 || (prev_unix.saturating_sub(dt_sec) / 86_400) < (now_after.saturating_sub(dt_sec) / 86_400) {
                crate::services::fleet_social::execute_pending_auto_kicks_on_downtime(self);
            }
            crate::services::implant_plugging::purge_expired_boosters_all(
                self,
                self.now_unix_secs(),
            );
            self.advance_skill_queues(secs as f64);
            crate::services::fleet_social::fleet_social_notify::tick_fleet_lifecycle(self);
            crate::services::space_mining_harvest::tick_all_ice_belt_respawns_sync(self);
            let now = self.now_unix_secs();
            crate::services::fleet_social::sweep_expired_bookmarks_and_notify(self, now);
            let mut balls_to_remove: Vec<(u32, i64)> = Vec::new();
            self.db.with_mut_snapshot(|snap| {
                // Despawn Cargo Containers (`type_id = 23`) in space (`InventoryFlag::None`) after 3600s (`feat:mining.jetcan-mining`).
                let expired_jetcans: Vec<(i64, u32)> = snap
                    .items
                    .values()
                    .filter(|item| {
                        item.type_id == 23
                            && item.flag == eve_db::InventoryFlag::None
                            && (30_000_000..=33_000_000).contains(&item.location_id)
                            && snap
                                .sp6_lifecycle
                                .space_object_meta
                                .get(&item.item_id)
                                .and_then(|m| m.created_unix)
                                .is_some_and(|created| now >= created.saturating_add(3600))
                    })
                    .map(|item| (item.item_id, item.location_id as u32))
                    .collect();

                for (jetcan_id, sys_id) in expired_jetcans {
                    snap.items.remove(&jetcan_id);
                    snap.sp6_lifecycle.space_object_meta.remove(&jetcan_id);
                    snap.items.retain(|_, item| item.location_id != jetcan_id);
                    balls_to_remove.push((sys_id, jetcan_id));
                }

                // Expire moon extractions that have passed `natural_decay_unix` (`feat:mining.moon-field-decay`).
                for ext in snap.moon_extractions.values_mut() {
                    if ext.status != "Decayed"
                        && ext.status != "Cancelled"
                        && now >= ext.natural_decay_unix
                    {
                        let struct_sys = snap
                            .structures
                            .get(&ext.structure_id)
                            .map(|s| s.solar_system_id);
                        for &ast_id in &ext.spawned_asteroid_ids {
                            let ast_sys = snap
                                .asteroids
                                .remove(&ast_id)
                                .map(|a| a.solar_system_id)
                                .or(struct_sys);
                            if let Some(sys_id) = ast_sys {
                                balls_to_remove.push((sys_id, ast_id));
                            }
                        }
                        ext.spawned_asteroid_ids.clear();
                        ext.status = "Decayed".to_string();
                    }
                }
            });

            for (sys_id, ball_id) in balls_to_remove {
                let handle = self.systems.get_or_spawn(sys_id);
                handle.try_remove_ball(ball_id);
            }

            let arrived_moon_probes: Vec<ActiveMoonSurveyProbe> =
                if let Ok(mut probes) = self.active_moon_probes.write() {
                    let mut arrived = Vec::new();
                    probes.retain(|p| {
                        if now >= p.arrival_unix_secs {
                            arrived.push(p.clone());
                            false
                        } else {
                            true
                        }
                    });
                    arrived
                } else {
                    Vec::new()
                };

            for p in arrived_moon_probes {
                let stay_in_space = self.db.with_snapshot(|snap| {
                    snap.characters.get(&p.owner_char_id).is_some_and(|c| {
                        c.solar_system_id == p.solar_system_id
                            && c.station_id.is_none()
                            && c.structure_id.is_none()
                    })
                });
                if stay_in_space {
                    if let Some(moon_id) = p.hit_moon_id {
                        let composition = crate::services::mining_advanced::derive_moon_ores_from_sde(
                            &self.sde,
                            p.solar_system_id,
                            moon_id,
                        );
                        let resources_dict = eve_proto::PyRep::Dict(
                            composition
                                .iter()
                                .map(|&(tid, frac)| {
                                    (
                                        eve_proto::PyRep::Integer(tid as i64),
                                        eve_proto::PyRep::Real(frac),
                                    )
                                })
                                .collect(),
                        );
                        self.notify(
                            crate::router::NotificationTarget::Character(p.owner_char_id),
                            "OnMoonScanComplete",
                            "charid",
                            vec![eve_proto::PyRep::Integer(moon_id), resources_dict],
                        );
                    }
                }
                self.notify(
                    crate::router::NotificationTarget::Character(p.owner_char_id),
                    "OnRemoveMoonProbe", "charid", vec![eve_proto::PyRep::Integer(p.probe_id)],
                );
            }
            crate::services::social_corp_fleet::sync_all_alliance_timers(self);
        }
    }

    /// Advances the virtual server clock by `secs` seconds (`T-7`).
    pub fn advance_virtual_clock(&self, secs: u64) {
        self.advance_secs(secs);
    }

    /// Alias for [`Self::advance_secs`] (`T-7`).
    pub fn advance_time_seconds(&self, secs: u64) {
        self.advance_secs(secs);
    }

    /// Returns the active time-acceleration multiplier (`>= 1`) (`T-7`).
    pub fn time_acceleration_multiplier(&self) -> u64 {
        self.clock().acceleration_multiplier()
    }

    /// Configures the time-acceleration multiplier (`>= 1`) (`T-7`).
    pub fn set_time_acceleration_multiplier(&self, multiplier: u64) {
        self.clock().set_acceleration_multiplier(multiplier);
    }

    /// Returns `true` if the server clock is currently paused (`T-7`).
    pub fn is_clock_paused(&self) -> bool {
        self.clock().is_paused()
    }

    /// Pauses or unpauses the server clock (`T-7`).
    pub fn set_clock_paused(&self, paused: bool) {
        self.clock().set_paused(paused);
    }

    /// Returns `true` if pause-on-tick mode is enabled (`T-7`).
    pub fn pause_on_tick(&self) -> bool {
        self.clock().pause_on_tick()
    }

    /// Enables or disables automatic clock pause at the end of each tick (`T-7`).
    pub fn set_pause_on_tick(&self, enabled: bool) {
        self.clock().set_pause_on_tick(enabled);
    }

    /// Steps the server clock by `base_secs` real seconds scaled by the time-acceleration multiplier,
    /// advancing skill queues (`T-7`).
    pub fn step_clock_tick(&self, base_secs: u64) -> u64 {
        let advanced = self.clock().step_tick(base_secs);
        if advanced > 0 {
            self.advance_skill_queues(advanced as f64);
        }
        advanced
    }

    /// Steps a 1 Hz live server wall-clock tick, advancing skill queues (`T-7`).
    pub fn tick_wall_second(&self) -> u64 {
        let advanced = self.clock().tick_wall_second();
        if advanced > 0 {
            self.advance_skill_queues(advanced as f64);
        }
        advanced
    }

    pub fn now_blueos_filetime_ticks(&self) -> i64 {
        self.clock().now_blueos_filetime_ticks()
    }

    /// Advances all active character skill queues by `elapsed_secs` of virtual time (`T-7`).
    pub fn advance_skill_queues(&self, elapsed_secs: f64) {
        if elapsed_secs <= 0.0 || !elapsed_secs.is_finite() {
            return;
        }
        if !self.db.with_snapshot(|snap| snap.skill_queues.values().any(|q| !q.is_empty())) {
            return;
        }
        let sde = self.sde.clone();
        let now_ft = self.now_blueos_filetime_ticks();
        let now_unix = self.now_unix();
        let (invalidated_ships, completed_events, paused_chars) = self.db.with_mut_snapshot(|snap| {
            let mut ships_to_invalidate = Vec::new();
            let mut completed_events = Vec::new();
            let mut paused_chars = Vec::new();
            let char_ids: Vec<i64> = snap.skill_queues.iter().filter(|(_, q)| !q.is_empty()).map(|(&cid, _)| cid).collect();
            for char_id in char_ids {
                if snap.paused_skill_queues.contains(&char_id) {
                    continue;
                }
                let Some(ch) = snap.characters.get(&char_id).cloned() else { continue };
                let exp_opt = crate::services::account_services::subscription_expiry_for_user(snap, ch.user_id);
                let omega_lapsed_at = exp_opt.filter(|&until| until <= now_unix && ch.clone_grade >= 2);
                let is_alpha = omega_lapsed_at.is_none() && ch.clone_grade <= 1;
                let current_char_sp = ch.skill_points + ch.free_skill_points;
                if is_alpha && current_char_sp >= 5_000_000.0 {
                    snap.paused_skill_queues.insert(char_id);
                    paused_chars.push(char_id);
                    continue;
                }
                let eff_attrs = crate::services::character_skills::effective_character_attributes(&sde, snap, char_id);
                let queue = snap.skill_queues.get(&char_id).cloned().unwrap_or_default();
                let mut rem_secs = if let Some(exp_t) = omega_lapsed_at {
                    let prev_unix = now_unix.saturating_sub(elapsed_secs as u64);
                    let start_t = queue.first().and_then(|q| q.started_at_unix).unwrap_or(prev_unix).max(prev_unix);
                    exp_t.saturating_sub(start_t) as f64
                } else {
                    elapsed_secs
                };
                let mut remaining_queue = Vec::new();
                let mut total_sp_gained = 0.0;
                let mut history_to_append = Vec::new();
                let skills_map = snap.skills.entry(char_id).or_default();

                for mut q in queue {
                    if rem_secs <= 0.0 {
                        remaining_queue.push(q);
                        continue;
                    }
                    if is_alpha && (current_char_sp + total_sp_gained) >= 5_000_000.0 {
                        rem_secs = 0.0;
                        snap.paused_skill_queues.insert(char_id);
                        remaining_queue.push(q);
                        continue;
                    }
                    if is_alpha {
                        let alpha_cap = crate::services::character_skills::alpha_skill_cap_for_character(&sde, ch.race_id, q.type_id);
                        if q.target_level > alpha_cap {
                            rem_secs = 0.0;
                            snap.paused_skill_queues.insert(char_id);
                            remaining_queue.push(q);
                            continue;
                        }
                    }

                    let skill_type = sde.types.get(&q.type_id);
                    let p_attr_id = skill_type.and_then(|t| t.default_attributes.get(&eve_sde::attr_ids::PRIMARY_ATTRIBUTE)).copied().map(|v| v as u32).unwrap_or(eve_sde::attr_ids::PERCEPTION as u32);
                    let s_attr_id = skill_type.and_then(|t| t.default_attributes.get(&eve_sde::attr_ids::SECONDARY_ATTRIBUTE)).copied().map(|v| v as u32).unwrap_or(eve_sde::attr_ids::WILLPOWER as u32);
                    let p_val = crate::services::character_skills::lookup_effective_attr_from_tuple(eff_attrs, p_attr_id);
                    let s_val = crate::services::character_skills::lookup_effective_attr_from_tuple(eff_attrs, s_attr_id);
                    let clone_mult = if !is_alpha { 1.0 } else { 0.5 };
                    let sp_sec = crate::formulas::sp_per_minute(p_val, s_val, clone_mult).max(1e-6) / 60.0;

                    let needed_sp = (q.end_sp - q.start_sp).max(0.0);
                    let time_for_needed = (needed_sp / sp_sec).max(0.0);
                    let max_allowed_sp = if is_alpha { (5_000_000.0 - (current_char_sp + total_sp_gained)).max(0.0) } else { f64::INFINITY };

                    if rem_secs >= time_for_needed && needed_sp <= max_allowed_sp {
                        rem_secs = (rem_secs - time_for_needed).max(0.0);
                        total_sp_gained += needed_sp;
                        skills_map.insert(q.type_id, eve_db::TrainedSkillRecord { type_id: q.type_id, level: q.target_level, skill_points: q.end_sp });
                        completed_events.push((char_id, q.type_id, q.target_level));
                        history_to_append.push(eve_db::SkillHistoryEntry { log_date_ft: now_ft, event_type_id: 37, skill_type_id: q.type_id, skill_level: q.target_level, absolute_points: q.end_sp });
                    } else {
                        let sp_advance = (rem_secs * sp_sec).min(needed_sp).min(max_allowed_sp);
                        rem_secs = 0.0;
                        q.start_sp += sp_advance;
                        q.duration_seconds = ((q.end_sp - q.start_sp).max(0.0)) / sp_sec;
                        q.started_at_unix = Some(now_unix);
                        total_sp_gained += sp_advance;

                        let current_lvl = skills_map.get(&q.type_id).map(|s| s.level).unwrap_or(0);
                        let mut achieved_lvl = current_lvl;
                        if let Some(rank) = skill_type.and_then(|t| t.default_attributes.get(&eve_sde::attr_ids::SKILL_TIME_CONSTANT).copied()) {
                            for lvl in (current_lvl + 1)..=q.target_level {
                                if q.start_sp + 0.5 >= crate::formulas::sp_for_skill_level(rank, lvl) {
                                    achieved_lvl = lvl;
                                } else {
                                    break;
                                }
                            }
                        }
                        if achieved_lvl > current_lvl {
                            completed_events.push((char_id, q.type_id, achieved_lvl));
                            history_to_append.push(eve_db::SkillHistoryEntry { log_date_ft: now_ft, event_type_id: 37, skill_type_id: q.type_id, skill_level: achieved_lvl, absolute_points: q.start_sp });
                        }
                        skills_map.insert(q.type_id, eve_db::TrainedSkillRecord { type_id: q.type_id, level: achieved_lvl, skill_points: q.start_sp });
                        remaining_queue.push(q);
                    }
                }
                if let Some(exp_t) = omega_lapsed_at {
                    if let Some(head) = remaining_queue.first_mut() {
                        head.started_at_unix = Some(exp_t);
                    }
                    for c in snap.characters.values_mut().filter(|c| c.user_id == ch.user_id) {
                        if c.clone_grade != 1 {
                            c.clone_grade = 1;
                            c.version += 1;
                        }
                        snap.paused_skill_queues.insert(c.character_id);
                    }
                    paused_chars.push(char_id);
                } else if is_alpha && (current_char_sp + total_sp_gained) >= 5_000_000.0 {
                    snap.paused_skill_queues.insert(char_id);
                    paused_chars.push(char_id);
                } else if let Some(head) = remaining_queue.first_mut() {
                    if head.started_at_unix.is_none() {
                        head.started_at_unix = Some(now_unix);
                    }
                }
                snap.skill_queues.insert(char_id, remaining_queue);
                if !history_to_append.is_empty() {
                    snap.skill_history.entry(char_id).or_default().extend(history_to_append);
                }
                if total_sp_gained > 0.0 {
                    if let Some(ch_mut) = snap.characters.get_mut(&char_id) {
                        ch_mut.skill_points += total_sp_gained;
                        ships_to_invalidate.push(ch_mut.active_ship_id);
                    }
                }
            }
            (ships_to_invalidate, completed_events, paused_chars)
        });
        for cid in paused_chars {
            let _ = self.notify_skill_queue_paused_server(cid);
        }
        for (cid, skill_type_id, lvl) in completed_events {
            let _ = self.notify_skill_trained(cid, 0, skill_type_id, lvl);
        }
        for ship_id in invalidated_ships {
            self.invalidate_ship_fitting(ship_id);
        }
    }

    pub fn invalidate_ship_fitting(&self, ship_id: i64) {
        self.ship_fittings
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&ship_id);
    }

    pub fn store_ship_fitting(&self, ship_id: i64, fit: ShipFittingEngine) {
        self.ship_fittings
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(ship_id, fit);
    }

    pub fn upsert_projected_ewar_on_ship(
        &self,
        target_ship_id: i64,
        effect: eve_dogma::ProjectedEwarEffect,
    ) {
        {
            let mut guard = self.projected_ewar.write().unwrap_or_else(|e| e.into_inner());
            let list = guard.entry(target_ship_id).or_default();
            if let Some(existing) = list
                .iter_mut()
                .find(|e| e.source_module_id == effect.source_module_id)
            {
                *existing = effect.clone();
            } else {
                list.push(effect.clone());
            }
        }
        if let Some(fit) = self
            .ship_fittings
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .get_mut(&target_ship_id)
        {
            fit.upsert_projected_ewar(effect, &self.sde);
        }
    }

    pub fn remove_projected_ewar_by_module(&self, source_module_id: i64) -> Vec<i64> {
        let mut affected_ships = Vec::new();
        {
            let mut guard = self.projected_ewar.write().unwrap_or_else(|e| e.into_inner());
            for (&ship_id, list) in guard.iter_mut() {
                let before = list.len();
                list.retain(|e| e.source_module_id != source_module_id);
                if list.len() < before {
                    affected_ships.push(ship_id);
                }
            }
        }
        if !affected_ships.is_empty() {
            let mut fit_guard = self
                .ship_fittings
                .write()
                .unwrap_or_else(|e| e.into_inner());
            for &ship_id in &affected_ships {
                if let Some(fit) = fit_guard.get_mut(&ship_id) {
                    fit.remove_projected_ewar(source_module_id, &self.sde);
                }
            }
        }
        affected_ships
    }

    pub fn get_or_rebuild_ship_fitting(
        &self,
        char_id: i64,
        ship_id: i64,
    ) -> Result<ShipFittingEngine, BridgeRpcError> {
        let skill_levels = self.character_skill_levels(char_id);
        let active_implants = self.db.with_snapshot(|s| {
            s.active_implants.get(&char_id).cloned().unwrap_or_default()
        });
        let (active_boosters, active_booster_side_effects) =
            crate::services::implant_plugging::active_combat_boosters_for_char(
                &self.db,
                char_id,
                self.now_unix(),
            );
        let active_projected = self
            .projected_ewar
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&ship_id)
            .cloned()
            .unwrap_or_default();
        if let Some(mut existing) = self
            .ship_fittings
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&ship_id)
            .cloned()
            .filter(|e| {
                e.boosters == active_boosters
                    && e.booster_side_effects == active_booster_side_effects
            })
        {
            let needs_recalc = existing.character_skills != skill_levels
                || existing.implants != active_implants
                || existing.projected_ewar != active_projected;
            existing.set_character_skills(skill_levels);
            existing.set_implants(active_implants);
            crate::services::combat_crimewatch::with_combat_runtime(self, |rt| {
                for d in rt.active_drones_in_space.values() {
                    if d.parent_ship_id == ship_id && !d.is_abandoned {
                        if !existing.drones.iter().any(|&(id, _)| id == d.drone_item_id) {
                            existing.drones.push((d.drone_item_id, d.type_id));
                        }
                        if !existing.drones.iter().any(|&(id, _)| id == d.ball_id) {
                            existing.drones.push((d.ball_id, d.type_id));
                        }
                    }
                }
            });
            if needs_recalc {
                existing.projected_ewar = active_projected;
                existing.recalculate_stats(&self.sde);
            }
            return Ok(existing);
        }

        let items = self.db.get_items_for_character(char_id);
        let Some(ship_item) = items.iter().find(|i| i.item_id == ship_id) else {
            return Err(BridgeRpcError {
                code: "ShipNotFound".into(),
                message: format!("Ship item {ship_id} not found for character {char_id}"),
            });
        };

        let mut fit = ShipFittingEngine::from_sde(ship_id, ship_item.type_id, &self.sde)
            .with_character_skills(skill_levels)
            .with_implants(active_implants)
            .with_boosters(active_boosters);
        fit.booster_side_effects = active_booster_side_effects;
        fit.projected_ewar = active_projected;
        for (&attr_id, &val) in &ship_item.dynamic_attributes {
            fit.ship.base_attributes.insert(attr_id, val);
        }

        let (online_states, active_states, active_charges) = self.db.with_snapshot(|s| {
            (
                s.module_online_states.clone(),
                s.module_active_states.clone(),
                s.module_active_charges.clone(),
            )
        });

        let mut fittable_items: Vec<_> = items
            .iter()
            .filter(|item| {
                item.location_id == ship_id && (item.flag as u16) >= 11 && {
                    let cat = self.sde.types.get(&item.type_id).map(|t| t.category_id);
                    cat == Some(7) || cat == Some(32)
                }
            })
            .cloned()
            .collect();

        // Subsystems (125..=129), Rigs (92..=94), and Low-slot fitting mods (11..=18) first
        fittable_items.sort_by_key(|i| match i.flag as u16 {
            125..=129 => 0,
            92..=94 => 1,
            11..=18 => 2,
            _ => 3,
        });

        let mut auto_offlined = Vec::new();
        for item in fittable_items {
            let online = online_states.get(&item.item_id).copied().unwrap_or(true);
            let fit_res = fit.fit_module_with_state(
                item.item_id,
                item.type_id,
                item.flag as u16,
                online,
                &self.sde,
            );
            let is_online = if let Err(ref err_msg) = fit_res {
                if online {
                    let _ = fit.fit_module_with_state(
                        item.item_id,
                        item.type_id,
                        item.flag as u16,
                        false,
                        &self.sde,
                    );
                    if err_msg.contains("Insufficient CPU")
                        || err_msg.contains("Insufficient Powergrid")
                    {
                        auto_offlined.push(item.item_id);
                    }
                }
                false
            } else {
                online
            };
            if let Some(m) = fit.modules.get_mut(&item.item_id) {
                if let Some(&dmg) = item.dynamic_attributes.get(&eve_sde::attr_ids::DAMAGE) {
                    m.damage_hp = dmg;
                }
                if let Some(&hp) = item.dynamic_attributes.get(&eve_sde::attr_ids::HP_STRUCT) {
                    m.max_hp = hp;
                }
                let mut overrides = item.dynamic_attributes.clone();
                overrides.remove(&eve_sde::attr_ids::DAMAGE);
                overrides.remove(&eve_sde::attr_ids::HP_STRUCT);
                if !overrides.is_empty() {
                    m.dynamic_attribute_overrides = overrides;
                }
            }
            if is_online && active_states.contains(&item.item_id) {
                let _ = fit.activate_module(item.item_id, false, &self.sde);
            }
            if let Some(&active_charge) = active_charges.get(&item.item_id) {
                if let Some(m) = fit.modules.get_mut(&item.item_id) {
                    m.charge_type_id = Some(active_charge);
                    m.charge_quantity = m.charge_quantity.max(1);
                }
            }
        }
        fit.recalculate_stats(&self.sde);
        let post_offlined = fit.enforce_post_unfit_or_offline_constraints(&self.sde);
        auto_offlined.extend(post_offlined);
        if !auto_offlined.is_empty() {
            self.db.with_mut_snapshot(|snap| {
                for mid in auto_offlined {
                    snap.module_online_states.insert(mid, false);
                    snap.module_active_states.remove(&mid);
                }
            });
        }

        // Attach loaded charges / scripts and owned drones
        for item in &items {
            let cat = self.sde.types.get(&item.type_id).map(|t| t.category_id);
            if item.location_id == ship_id && cat == Some(8) {
                let slot_flag = item.flag as u16;
                if let Some(m) = fit.modules.values_mut().find(|m| m.flag_id == slot_flag) {
                    m.charge_type_id = Some(item.type_id);
                    m.charge_quantity = item.quantity.max(0) as u32;
                }
            } else if item.location_id == ship_id && cat == Some(18) {
                fit.drones.push((item.item_id, item.type_id));
            }
        }
        crate::services::combat_crimewatch::with_combat_runtime(self, |rt| {
            for d in rt.active_drones_in_space.values() {
                if d.parent_ship_id == ship_id && !d.is_abandoned {
                    if !fit.drones.iter().any(|&(id, _)| id == d.drone_item_id) {
                        fit.drones.push((d.drone_item_id, d.type_id));
                    }
                    if !fit.drones.iter().any(|&(id, _)| id == d.ball_id) {
                        fit.drones.push((d.ball_id, d.type_id));
                    }
                }
            }
        });
        fit.recalculate_stats(&self.sde);

        self.store_ship_fitting(ship_id, fit.clone());
        Ok(fit)
    }

    pub fn has_fleet_extras_for(&self, fleet_id: i64) -> bool {
        let ex = self.fleet_extras.lock().unwrap();
        ex.target_tags.contains_key(&fleet_id)
            || ex.watchlist.contains_key(&fleet_id)
            || ex.options.contains_key(&fleet_id)
            || ex.wing_squad_hierarchy.contains_key(&fleet_id)
            || ex.fleet_names.contains_key(&fleet_id)
            || ex.max_sizes.contains_key(&fleet_id)
            || ex.invite_details.keys().any(|&(fid, _)| fid == fleet_id)
            || ex.invite_timestamps.keys().any(|&(fid, _)| fid == fleet_id)
    }
}
