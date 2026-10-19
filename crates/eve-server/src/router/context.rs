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

pub struct ServerContext {
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
    pub pending_notifications:
        RwLock<HashMap<i64, Vec<crate::router::cache_notifications::PendingMachoNotification>>>,
    pub notify_tx: tokio::sync::broadcast::Sender<(
        i64,
        crate::router::cache_notifications::PendingMachoNotification,
    )>,
    pub map_landmarks: RwLock<Vec<(i64, String, f64, f64, f64)>>,
    pub session_closers: RwLock<HashMap<u64, tokio::sync::mpsc::UnboundedSender<(String, String)>>>,
    pub ship_fittings: RwLock<HashMap<i64, ShipFittingEngine>>,
    pub weapon_cycle_state: RwLock<HashMap<i64, (Option<u64>, u32)>>,
    pub fighter_cooldown_state: RwLock<HashMap<(i64, u32), u64>>,
    pub active_fighters: RwLock<HashMap<i64, ActiveFighterSquadron>>,
    pub fighter_tube_loads: RwLock<HashMap<(i64, usize), i64>>,
    pub scanned_signatures: RwLock<HashSet<(i64, u32, i64)>>,
    pub invention_roll_override: RwLock<Option<f64>>,
    pub feature_flags: crate::config::FeatureFlagsConfig,
    pub esi_port: u16,
    pub esi_state: Arc<crate::esi::runtime_state::EsiRuntimeState>,
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

        let systems = Arc::new(SystemRegistry::with_sde_and_db(sde.clone(), Arc::clone(&db)));
        let quasar_descriptors =
            eve_proto::quasar::QuasarDescriptorRegistry::open_default_or_empty();
        let quasar_gateway = Arc::new(crate::quasar_gateway::PublicGatewayRouter::new(
            quasar_descriptors,
            Arc::clone(&db),
        ));
        crate::quasar_services::register_all_quasar_services(
            &quasar_gateway,
            Arc::clone(&db),
            Arc::clone(&sde),
        );

        Self {
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
            notification_records: RwLock::new(Vec::new()),
            pending_notifications: RwLock::new(HashMap::new()),
            notify_tx,
            map_landmarks: RwLock::new(Vec::new()),
            session_closers: RwLock::new(HashMap::new()),
            ship_fittings: RwLock::new(HashMap::new()),
            weapon_cycle_state: RwLock::new(HashMap::new()),
            fighter_cooldown_state: RwLock::new(HashMap::new()),
            active_fighters: RwLock::new(HashMap::new()),
            fighter_tube_loads: RwLock::new(HashMap::new()),
            scanned_signatures: RwLock::new(HashSet::new()),
            invention_roll_override: RwLock::new(None),
            feature_flags: crate::config::FeatureFlagsConfig::default(),
            esi_port: 26_004,
            esi_state: Arc::new(crate::esi::runtime_state::EsiRuntimeState::new()),
        }
    }

    /// Initializes a `ServerContext` with the provided `WorldRepository` and seeds the NPC regional markets.
    pub fn with_repository(db: Arc<WorldRepository>) -> Self {
        let sde = Arc::new(SdeRegistry::with_canonical_seed());
        Self::with_sde_and_repository(sde, db)
    }

    /// Initializes a `ServerContext` from a [`crate::config::ServerConfig`] (`D-5`).
    pub fn from_config(cfg: &crate::config::ServerConfig) -> Result<Self, String> {
        let sde = Arc::new(SdeRegistry::open(&cfg.sde_path).map_err(|e| e.to_string())?);
        let db = Arc::new(WorldRepository::new_empty_with_snapshot_path(Some(
            std::path::PathBuf::from(&cfg.world_db_path),
        ))?);
        for player in &cfg.player_accounts {
            if !player.username.trim().is_empty() && !player.password_token.trim().is_empty() {
                db.provision_player_account(player.username.trim(), player.password_token.trim());
            }
        }
        for admin in &cfg.admin_accounts {
            if !admin.username.trim().is_empty() && !admin.password_token.trim().is_empty() {
                db.provision_gm_account(
                    admin.username.trim(),
                    admin.password_token.trim(),
                    admin.character_id,
                    admin.character_name.trim(),
                );
            }
        }
        let mut ctx = Self::with_sde_and_repository(sde, db);
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
            let disable_demo = std::env::var("EVOMU_DISABLE_DEMO_SEED")
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(false);
            if disable_demo {
                Arc::new(WorldRepository::new_empty())
            } else {
                Arc::new(WorldRepository::with_demo_seed())
            }
        };
        #[cfg(not(any(test, feature = "demo-seed")))]
        let db = Arc::new(WorldRepository::new_empty());
        Self::with_repository(db)
    }

    pub fn mark_signature_scanned(&self, char_id: i64, solar_system_id: u32, sig_ball_id: i64) {
        self.scanned_signatures
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert((char_id, solar_system_id, sig_ball_id));
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

    pub fn character_skill_levels(&self, char_id: i64) -> HashMap<u32, u8> {
        self.db.with_snapshot(|snap| {
            snap.skills
                .get(&char_id)
                .map(|m| m.iter().map(|(&tid, rec)| (tid, rec.level)).collect())
                .unwrap_or_default()
        })
    }

    pub fn set_invention_roll_override(&self, roll: Option<f64>) {
        *self
            .invention_roll_override
            .write()
            .unwrap_or_else(|e| e.into_inner()) = roll.map(|r| r.clamp(0.0, 0.999_999));
    }

    pub fn next_invention_roll(&self, job_id: u64, installer_id: u64, now_unix: u64) -> f64 {
        if let Some(r) = *self
            .invention_roll_override
            .read()
            .unwrap_or_else(|e| e.into_inner())
        {
            return r;
        }
        if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
            let mut buf = [0u8; 8];
            if std::io::Read::read_exact(&mut f, &mut buf).is_ok() {
                let raw = u64::from_le_bytes(buf) >> 11;
                return (raw as f64) / ((1u64 << 53) as f64);
            }
        }
        let ticks = (self.now_blueos_filetime_ticks() as u64).max(now_unix);
        let mut z = job_id
            .wrapping_add(installer_id.wrapping_mul(0x517c_c1b7_2722_0a95))
            .wrapping_add(ticks)
            .wrapping_add(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        ((z >> 11) as f64) / ((1u64 << 53) as f64)
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
        self.db.set_clock(clock.clone());
        *self.clock.write().unwrap_or_else(|e| e.into_inner()) = clock;
    }

    /// Returns the current server Unix timestamp in seconds (`T-7`).
    pub fn now_unix_secs(&self) -> u64 {
        self.clock().now_unix_secs()
    }

    /// Alias for [`Self::now_unix_secs`] (`T-7`).
    pub fn now_unix(&self) -> u64 {
        self.now_unix_secs()
    }

    /// Advances virtual server time by `secs` seconds and runs time-driven skill queue progression (`T-7`).
    pub fn advance_secs(&self, secs: u64) {
        self.clock().advance_secs(secs);
        if secs > 0 {
            self.advance_skill_queues(secs as f64);
        }
    }

    /// Advances the virtual server clock by `secs` seconds (`T-7`).
    pub fn advance_virtual_clock(&self, secs: u64) {
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
        let has_queued = self
            .db
            .with_snapshot(|snap| snap.skill_queues.values().any(|q| !q.is_empty()));
        if !has_queued {
            return;
        }
        let sde = self.sde.clone();
        let (invalidated_ships, completed_events) = self.db.with_mut_snapshot(|snap| {
            let mut ships_to_invalidate = Vec::new();
            let mut completed_events = Vec::new();
            let char_ids: Vec<i64> = snap
                .skill_queues
                .iter()
                .filter(|(_, q)| !q.is_empty())
                .map(|(&cid, _)| cid)
                .collect();
            for char_id in char_ids {
                let Some(ch) = snap.characters.get(&char_id).cloned() else {
                    continue;
                };
                let is_alpha = ch.clone_grade == 1;
                let current_char_sp = ch.skill_points;
                // Alpha clones cannot gain SP beyond 5,000,000 SP (CH-1)
                if is_alpha && current_char_sp >= 5_000_000.0 {
                    continue;
                }

                let queue = snap
                    .skill_queues
                    .get(&char_id)
                    .cloned()
                    .unwrap_or_default();
                let mut rem_secs = elapsed_secs;
                let mut remaining_queue = Vec::new();
                let mut total_sp_gained = 0.0;
                let skills_map = snap.skills.entry(char_id).or_default();

                for mut q in queue {
                    if rem_secs <= 0.0 {
                        remaining_queue.push(q);
                        continue;
                    }
                    if is_alpha && (current_char_sp + total_sp_gained) >= 5_000_000.0 {
                        remaining_queue.push(q);
                        continue;
                    }

                    let skill_type = sde.types.get(&q.type_id);
                    let p_attr_id = skill_type
                        .and_then(|t| t.default_attributes.get(&eve_sde::attr_ids::PRIMARY_ATTRIBUTE))
                        .copied()
                        .map(|v| v as u32)
                        .unwrap_or(eve_sde::attr_ids::PERCEPTION as u32);
                    let s_attr_id = skill_type
                        .and_then(|t| t.default_attributes.get(&eve_sde::attr_ids::SECONDARY_ATTRIBUTE))
                        .copied()
                        .map(|v| v as u32)
                        .unwrap_or(eve_sde::attr_ids::WILLPOWER as u32);
                    let p_val = ch.neural.get_attribute_by_id(p_attr_id);
                    let s_val = ch.neural.get_attribute_by_id(s_attr_id);
                    let clone_mult = if ch.clone_grade == 2 { 1.0 } else { 0.5 };
                    let sp_min = crate::formulas::sp_per_minute(p_val, s_val, clone_mult).max(1e-6);
                    let sp_sec = sp_min / 60.0;

                    let needed_sp = (q.end_sp - q.start_sp).max(0.0);
                    let time_for_needed = (needed_sp / sp_sec).max(0.0);
                    let max_allowed_sp = if is_alpha {
                        (5_000_000.0 - (current_char_sp + total_sp_gained)).max(0.0)
                    } else {
                        f64::INFINITY
                    };

                    if rem_secs >= time_for_needed && needed_sp <= max_allowed_sp {
                        // Skill level completed: auto-advance to next skill in queue (CH-1)
                        rem_secs = (rem_secs - time_for_needed).max(0.0);
                        total_sp_gained += needed_sp;
                        skills_map.insert(
                            q.type_id,
                            eve_db::TrainedSkillRecord {
                                type_id: q.type_id,
                                level: q.target_level,
                                skill_points: q.end_sp,
                            },
                        );
                        completed_events.push((char_id, q.type_id, q.target_level));
                    } else {
                        let mut sp_advance = (rem_secs * sp_sec).min(needed_sp);
                        if sp_advance > max_allowed_sp {
                            sp_advance = max_allowed_sp;
                            rem_secs = 0.0;
                        } else {
                            rem_secs = 0.0;
                        }
                        q.start_sp += sp_advance;
                        let rem_sp = (q.end_sp - q.start_sp).max(0.0);
                        q.duration_seconds = rem_sp / sp_sec;
                        total_sp_gained += sp_advance;

                        let current_lvl = skills_map.get(&q.type_id).map(|s| s.level).unwrap_or(0);
                        let mut achieved_lvl = current_lvl;
                        if let Some(rank) = skill_type.and_then(|t| {
                            t.default_attributes.get(&eve_sde::attr_ids::SKILL_TIME_CONSTANT).copied()
                        }) {
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
                        }
                        skills_map.insert(
                            q.type_id,
                            eve_db::TrainedSkillRecord {
                                type_id: q.type_id,
                                level: achieved_lvl,
                                skill_points: q.start_sp,
                            },
                        );
                        remaining_queue.push(q);
                    }
                }
                snap.skill_queues.insert(char_id, remaining_queue);
                if total_sp_gained > 0.0 {
                    if let Some(ch_mut) = snap.characters.get_mut(&char_id) {
                        ch_mut.skill_points += total_sp_gained;
                        ships_to_invalidate.push(ch_mut.active_ship_id);
                    }
                }
            }
            (ships_to_invalidate, completed_events)
        });
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

    pub fn get_or_rebuild_ship_fitting(
        &self,
        char_id: i64,
        ship_id: i64,
    ) -> Result<ShipFittingEngine, BridgeRpcError> {
        let skill_levels = self.character_skill_levels(char_id);
        let active_implants = self.db.with_snapshot(|s| {
            s.active_implants.get(&char_id).cloned().unwrap_or_default()
        });
        if let Some(mut existing) = self
            .ship_fittings
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&ship_id)
            .cloned()
        {
            existing.set_character_skills(skill_levels);
            existing.set_implants(active_implants);
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
            .with_implants(active_implants);

        let online_states = self.db.with_snapshot(|s| s.module_online_states.clone());

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

        // Subsystems (125..=129) must be fitted first to configure hull slot counts
        fittable_items.sort_by_key(|i| {
            let flag = i.flag as u16;
            if (125..=129).contains(&flag) {
                0
            } else {
                1
            }
        });

        for item in fittable_items {
            let online = online_states.get(&item.item_id).copied().unwrap_or(true);
            let _ = fit.fit_module_with_state(
                item.item_id,
                item.type_id,
                item.flag as u16,
                online,
                &self.sde,
            );
        }

        // Attach loaded charges / scripts
        for item in &items {
            let is_charge = self
                .sde
                .types
                .get(&item.type_id)
                .map(|t| t.category_id == 8)
                .unwrap_or(false);
            if item.location_id == ship_id && is_charge {
                let slot_flag = item.flag as u16;
                if let Some(m) = fit.modules.values_mut().find(|m| m.flag_id == slot_flag) {
                    m.charge_type_id = Some(item.type_id);
                    m.charge_quantity = item.quantity.max(0) as u32;
                }
            }
        }
        fit.recalculate_stats(&self.sde);

        self.store_ship_fitting(ship_id, fit.clone());
        Ok(fit)
    }

    pub fn check_type_skill_prerequisites_in_snapshot(
        &self,
        snap: &eve_db::WorldStateSnapshot,
        char_id: i64,
        type_id: u32,
    ) -> Result<(), BridgeRpcError> {
        let Some(t) = self.sde.types.get(&type_id) else {
            return Ok(());
        };
        let char_skills = snap.skills.get(&char_id);
        for (req_skill_attr, req_lvl_attr) in [(182_u16, 277_u16), (183, 278), (184, 279)] {
            if let Some(&skill_f) = t.default_attributes.get(&req_skill_attr) {
                let skill_id = skill_f.round() as u32;
                if skill_id > 0 {
                    let req_lvl = t
                        .default_attributes
                        .get(&req_lvl_attr)
                        .copied()
                        .unwrap_or(1.0)
                        .round()
                        .clamp(1.0, 5.0) as u8;
                    let trained = char_skills
                        .and_then(|m| m.get(&skill_id))
                        .map(|r| r.level)
                        .unwrap_or(0);
                    if trained < req_lvl {
                        return Err(BridgeRpcError {
                            code: "SkillPrerequisitesNotMet".into(),
                            message: format!(
                                "{} (type {type_id}) requires skill {skill_id} at level {req_lvl} (character has level {trained})",
                                t.name
                            ),
                        });
                    }
                }
            }
        }
        Ok(())
    }

    pub fn is_charge_compatible_with_weapon(
        weapon_type: &eve_sde::InvType,
        charge_type: &eve_sde::InvType,
    ) -> bool {
        if charge_type.category_id != 8 {
            return false;
        }
        let group_ok = [604_u16, 605, 606, 609, 610].iter().any(|attr_id| {
            weapon_type
                .default_attributes
                .get(attr_id)
                .map(|g| (*g as u32) == charge_type.group_id)
                .unwrap_or(false)
        }) || charge_type
            .default_attributes
            .get(&137)
            .map(|lg| *lg as u32 == weapon_type.group_id)
            .unwrap_or(false);
        let size_ok = match (
            weapon_type.default_attributes.get(&128).copied(),
            charge_type.default_attributes.get(&128).copied(),
        ) {
            (Some(rs), Some(cs)) => (rs - cs).abs() < 0.5,
            _ => true,
        };
        group_ok && size_ok
    }

    pub fn weapon_slot_max_charge_capacity(
        weapon_type: &eve_sde::InvType,
        charge_type: &eve_sde::InvType,
    ) -> i32 {
        let cap_m3 = if weapon_type.capacity > 0.0 {
            weapon_type.capacity
        } else {
            5.0
        };
        let vol_m3 = charge_type.volume.max(1e-6);
        ((cap_m3 / vol_m3).floor() as i32).max(1)
    }

    /// Loads or swaps an ammunition/missile charge stack into a fitted high-slot weapon (`flag 27..=34`),
    /// enforcing weapon/charge group & size compatibility, character skill prerequisites (`182..=184`/`277..=279`),
    /// and `floor(weapon.capacity / charge.volume)` clip bounds (splitting oversized stacks and unloading
    /// any previously loaded charge of a different type back into `ship_id` `Cargo`).
    pub fn load_charge_into_weapon_slot(
        &self,
        char_id: i64,
        ship_id: i64,
        flag: eve_db::InventoryFlag,
        charge_item_id: i64,
        current_dock: Option<i64>,
    ) -> Result<(i64, u32, i32, u64), BridgeRpcError> {
        self.db.try_with_mut_snapshot(|snap| {
            self.load_charge_into_weapon_slot_in_snapshot(
                snap,
                char_id,
                ship_id,
                flag,
                charge_item_id,
                current_dock,
            )
        })
    }

    pub fn load_charge_into_weapon_slot_in_snapshot(
        &self,
        snap: &mut eve_db::WorldStateSnapshot,
        char_id: i64,
        ship_id: i64,
        flag: eve_db::InventoryFlag,
        charge_item_id: i64,
        current_dock: Option<i64>,
    ) -> Result<(i64, u32, i32, u64), BridgeRpcError> {
        let flag_u16 = flag as u16;
        if !(27..=34).contains(&flag_u16) {
            return Err(BridgeRpcError {
                code: "InvalidWeaponSlot".into(),
                message: format!(
                    "Charges can only be loaded into high weapon slots 27..=34 (got {flag_u16})"
                ),
            });
        }
        {
            let Some(ship_item) = snap.items.get(&ship_id).cloned() else {
                return Err(BridgeRpcError {
                    code: "ShipNotFound".into(),
                    message: format!("Target ship {ship_id} not found"),
                });
            };
            let is_ship = self
                .sde
                .types
                .get(&ship_item.type_id)
                .map(|t| t.category_id == 6)
                .unwrap_or(false);
            if ship_item.owner_id != char_id
                || !is_ship
                || !snap.is_ship_colocated_with_character(char_id, ship_id, current_dock)
            {
                return Err(BridgeRpcError {
                    code: "InvalidFittingTarget".into(),
                    message: format!("Target {ship_id} is not an owned co-located ship"),
                });
            }

            let weapon_type_id = snap
                .items
                .values()
                .find(|i| {
                    i.location_id == ship_id
                        && i.flag == flag
                        && self
                            .sde
                            .types
                            .get(&i.type_id)
                            .map(|t| t.category_id == 7)
                            .unwrap_or(false)
                })
                .map(|i| i.type_id);
            let Some(wtid) = weapon_type_id else {
                return Err(BridgeRpcError {
                    code: "NoWeaponFitted".into(),
                    message: format!(
                        "High slot {flag_u16} on ship {ship_id} has no fitted weapon module"
                    ),
                });
            };
            let Some(weapon_type) = self.sde.types.get(&wtid) else {
                return Err(BridgeRpcError {
                    code: "UnknownWeaponType".into(),
                    message: format!("Unknown weapon type {wtid}"),
                });
            };

            let Some(src_charge) = snap.items.get(&charge_item_id).cloned() else {
                return Err(BridgeRpcError {
                    code: "ItemNotFound".into(),
                    message: format!("Charge item {charge_item_id} not found"),
                });
            };
            if src_charge.location_id == ship_id && src_charge.flag == flag {
                return Ok((
                    src_charge.item_id,
                    src_charge.type_id,
                    src_charge.quantity,
                    src_charge.version,
                ));
            }
            if !snap.is_item_consumable_by(&src_charge, char_id, current_dock, Some(ship_id)) {
                return Err(BridgeRpcError {
                    code: "AmmoNotReachable".into(),
                    message: format!(
                        "Charge item {charge_item_id} must be owned, unlocked, and in ship {ship_id} Cargo or current station Hangar"
                    ),
                });
            }
            let Some(charge_type) = self.sde.types.get(&src_charge.type_id) else {
                return Err(BridgeRpcError {
                    code: "UnknownChargeType".into(),
                    message: format!("Unknown charge type {}", src_charge.type_id),
                });
            };
            if !Self::is_charge_compatible_with_weapon(weapon_type, charge_type) {
                return Err(BridgeRpcError {
                    code: "IncompatibleAmmo".into(),
                    message: format!(
                        "Charge {} (type {}) is not compatible with weapon {} (type {wtid})",
                        charge_type.name, src_charge.type_id, weapon_type.name
                    ),
                });
            }
            self.check_type_skill_prerequisites_in_snapshot(snap, char_id, src_charge.type_id)?;

            let max_cap = Self::weapon_slot_max_charge_capacity(weapon_type, charge_type);

            // Check if a charge is already in `(ship_id, flag)`
            let existing_loaded: Option<(i64, u32, i32)> = snap
                .items
                .values()
                .find(|i| {
                    i.item_id != charge_item_id
                        && i.location_id == ship_id
                        && i.flag == flag
                        && self
                            .sde
                            .types
                            .get(&i.type_id)
                            .map(|t| t.category_id == 8)
                            .unwrap_or(false)
                })
                .map(|i| (i.item_id, i.type_id, i.quantity));

            let mut current_same_id_qty: Option<(i64, i32)> = None;
            if let Some((ex_id, ex_type_id, ex_qty)) = existing_loaded {
                if ex_type_id == src_charge.type_id {
                    current_same_id_qty = Some((ex_id, ex_qty));
                } else {
                    // ChangeAmmo: unload previous charge stack back to `(ship_id, Cargo)`
                    let merge_cargo_id = snap
                        .items
                        .values()
                        .find(|i| {
                            i.item_id != ex_id
                                && i.owner_id == char_id
                                && i.location_id == ship_id
                                && i.flag == eve_db::InventoryFlag::Cargo
                                && i.type_id == ex_type_id
                                && !i.singleton
                        })
                        .map(|i| i.item_id);
                    if let Some(mc_id) = merge_cargo_id {
                        snap.items.remove(&ex_id);
                        if let Some(mc) = snap.items.get_mut(&mc_id) {
                            mc.quantity = mc.quantity.saturating_add(ex_qty);
                            mc.version += 1;
                        }
                    } else if let Some(ex_item) = snap.items.get_mut(&ex_id) {
                        ex_item.location_id = ship_id;
                        ex_item.flag = eve_db::InventoryFlag::Cargo;
                        ex_item.singleton = false;
                        ex_item.version += 1;
                    }
                }
            }

            let cur_qty = current_same_id_qty.map(|(_, q)| q).unwrap_or(0);
            let room = (max_cap - cur_qty).max(0);
            if room == 0 {
                let (ex_id, ex_q) = current_same_id_qty.unwrap();
                let ver = snap.items.get(&ex_id).map(|i| i.version).unwrap_or(1);
                return Ok((ex_id, src_charge.type_id, ex_q, ver));
            }
            let to_load = src_charge.quantity.min(room);

            if let Some((ex_id, _)) = current_same_id_qty {
                if src_charge.quantity > to_load {
                    if let Some(src_mut) = snap.items.get_mut(&charge_item_id) {
                        src_mut.quantity -= to_load;
                        src_mut.version += 1;
                    }
                } else {
                    snap.items.remove(&charge_item_id);
                }
                let ex_mut = snap.items.get_mut(&ex_id).unwrap();
                ex_mut.quantity += to_load;
                ex_mut.version += 1;
                Ok((ex_id, ex_mut.type_id, ex_mut.quantity, ex_mut.version))
            } else if src_charge.quantity > to_load {
                if let Some(src_mut) = snap.items.get_mut(&charge_item_id) {
                    src_mut.quantity -= to_load;
                    src_mut.version += 1;
                }
                snap.next_entity_id += 1;
                let loaded_id = snap.next_entity_id;
                snap.items.insert(
                    loaded_id,
                    eve_db::InventoryItemRecord {
                        item_id: loaded_id,
                        type_id: src_charge.type_id,
                        owner_id: char_id,
                        location_id: ship_id,
                        flag,
                        quantity: to_load,
                        singleton: false,
                        version: 1,
                        dynamic_attributes: HashMap::new(),
                        source_mutaplasmid_id: None,
                        insured_payout_isk: 0.0,
                        is_blueprint_copy: false,
                        blueprint_runs_remaining: None,
                    },
                );
                Ok((loaded_id, src_charge.type_id, to_load, 1))
            } else {
                let src_mut = snap.items.get_mut(&charge_item_id).unwrap();
                src_mut.location_id = ship_id;
                src_mut.flag = flag;
                src_mut.singleton = false;
                src_mut.version += 1;
                Ok((
                    src_mut.item_id,
                    src_mut.type_id,
                    src_mut.quantity,
                    src_mut.version,
                ))
            }
        }
    }

    /// Builds a `CombatState` for a player's ship populated from its Dogma `ShipFittingEngine`
    /// attributes (or SDE `default_attributes` fallback), including all 12 shield/armor/hull resonances.
    pub fn build_ship_combat_state(
        &self,
        char_id: i64,
        ship_id: i64,
        type_id: u32,
    ) -> eve_destiny::CombatState {
        if let Ok(fit) = self.get_or_rebuild_ship_fitting(char_id, ship_id) {
            let get_attr = |attr_id: u16, fallback: f64| -> f64 {
                if fit.ship.base_attributes.contains_key(&attr_id)
                    || self.sde.attributes.contains_key(&attr_id)
                    || fit
                        .ship
                        .modifiers
                        .iter()
                        .any(|m| m.target_attribute_id == attr_id)
                {
                    fit.ship.get_attribute(attr_id, &self.sde)
                } else {
                    fallback
                }
            };
            let shield = get_attr(eve_sde::attr_ids::SHIELD_CAPACITY, 450.0);
            let armor = get_attr(eve_sde::attr_ids::ARMOR_HP, 350.0);
            let hull = get_attr(eve_sde::attr_ids::HP_STRUCT, 350.0);
            let cap = get_attr(eve_sde::attr_ids::CAPACITOR_CAPACITY, 250.0);
            let sig = get_attr(eve_sde::attr_ids::SIGNATURE_RADIUS, 35.0);
            let shield_rech = get_attr(eve_sde::attr_ids::SHIELD_RECHARGE_RATE, 120_000.0);
            let cap_rech = get_attr(eve_sde::attr_ids::RECHARGE_RATE, 120_000.0);
            return eve_destiny::CombatState {
                shield_hp: shield,
                max_shield_hp: shield,
                armor_hp: armor,
                max_armor_hp: armor,
                structure_hp: hull,
                max_structure_hp: hull,
                capacitor: cap,
                max_capacitor: cap,
                signature_radius: sig,
                shield_recharge_time_ms: shield_rech,
                capacitor_recharge_time_ms: cap_rech,
                shield_resonance: [
                    get_attr(eve_sde::attr_ids::SHIELD_EM_DAMAGE_RESONANCE, 1.0),
                    get_attr(eve_sde::attr_ids::SHIELD_THERMAL_DAMAGE_RESONANCE, 0.8),
                    get_attr(eve_sde::attr_ids::SHIELD_KINETIC_DAMAGE_RESONANCE, 0.6),
                    get_attr(eve_sde::attr_ids::SHIELD_EXPLOSIVE_DAMAGE_RESONANCE, 0.5),
                ],
                armor_resonance: [
                    get_attr(eve_sde::attr_ids::ARMOR_EM_DAMAGE_RESONANCE, 0.5),
                    get_attr(eve_sde::attr_ids::ARMOR_THERMAL_DAMAGE_RESONANCE, 0.65),
                    get_attr(eve_sde::attr_ids::ARMOR_KINETIC_DAMAGE_RESONANCE, 0.75),
                    get_attr(eve_sde::attr_ids::ARMOR_EXPLOSIVE_DAMAGE_RESONANCE, 0.9),
                ],
                structure_resonance: [
                    get_attr(eve_sde::attr_ids::EM_DAMAGE_RESONANCE, 0.67),
                    get_attr(eve_sde::attr_ids::THERMAL_DAMAGE_RESONANCE, 0.67),
                    get_attr(eve_sde::attr_ids::KINETIC_DAMAGE_RESONANCE, 0.67),
                    get_attr(eve_sde::attr_ids::EXPLOSIVE_DAMAGE_RESONANCE, 0.67),
                ],
                ..eve_destiny::CombatState::default()
            };
        }
        self.build_type_combat_state(type_id)
    }

    /// Builds a `CombatState` directly from a type's SDE `default_attributes` (for NPCs, wrecks, or fallback hulls).
    pub fn build_type_combat_state(&self, type_id: u32) -> eve_destiny::CombatState {
        if let Some(t) = self.sde.types.get(&type_id) {
            let a = &t.default_attributes;
            let shield = a
                .get(&eve_sde::attr_ids::SHIELD_CAPACITY)
                .copied()
                .unwrap_or(50.0);
            let armor = a.get(&eve_sde::attr_ids::ARMOR_HP).copied().unwrap_or(50.0);
            let hull = a
                .get(&eve_sde::attr_ids::HP_STRUCT)
                .copied()
                .unwrap_or(50.0);
            let cap = a
                .get(&eve_sde::attr_ids::CAPACITOR_CAPACITY)
                .copied()
                .unwrap_or(100.0);
            let sig = a
                .get(&eve_sde::attr_ids::SIGNATURE_RADIUS)
                .copied()
                .unwrap_or(t.radius.max(25.0));
            let shield_rech = a
                .get(&eve_sde::attr_ids::SHIELD_RECHARGE_RATE)
                .copied()
                .unwrap_or(120_000.0);
            let cap_rech = a
                .get(&eve_sde::attr_ids::RECHARGE_RATE)
                .copied()
                .unwrap_or(120_000.0);
            return eve_destiny::CombatState {
                shield_hp: shield,
                max_shield_hp: shield,
                armor_hp: armor,
                max_armor_hp: armor,
                structure_hp: hull,
                max_structure_hp: hull,
                capacitor: cap,
                max_capacitor: cap,
                signature_radius: sig,
                shield_recharge_time_ms: shield_rech,
                capacitor_recharge_time_ms: cap_rech,
                shield_resonance: [
                    a.get(&eve_sde::attr_ids::SHIELD_EM_DAMAGE_RESONANCE)
                        .copied()
                        .unwrap_or(1.0),
                    a.get(&eve_sde::attr_ids::SHIELD_THERMAL_DAMAGE_RESONANCE)
                        .copied()
                        .unwrap_or(1.0),
                    a.get(&eve_sde::attr_ids::SHIELD_KINETIC_DAMAGE_RESONANCE)
                        .copied()
                        .unwrap_or(1.0),
                    a.get(&eve_sde::attr_ids::SHIELD_EXPLOSIVE_DAMAGE_RESONANCE)
                        .copied()
                        .unwrap_or(1.0),
                ],
                armor_resonance: [
                    a.get(&eve_sde::attr_ids::ARMOR_EM_DAMAGE_RESONANCE)
                        .copied()
                        .unwrap_or(1.0),
                    a.get(&eve_sde::attr_ids::ARMOR_THERMAL_DAMAGE_RESONANCE)
                        .copied()
                        .unwrap_or(1.0),
                    a.get(&eve_sde::attr_ids::ARMOR_KINETIC_DAMAGE_RESONANCE)
                        .copied()
                        .unwrap_or(1.0),
                    a.get(&eve_sde::attr_ids::ARMOR_EXPLOSIVE_DAMAGE_RESONANCE)
                        .copied()
                        .unwrap_or(1.0),
                ],
                structure_resonance: [
                    a.get(&eve_sde::attr_ids::EM_DAMAGE_RESONANCE)
                        .copied()
                        .unwrap_or(1.0),
                    a.get(&eve_sde::attr_ids::THERMAL_DAMAGE_RESONANCE)
                        .copied()
                        .unwrap_or(1.0),
                    a.get(&eve_sde::attr_ids::KINETIC_DAMAGE_RESONANCE)
                        .copied()
                        .unwrap_or(1.0),
                    a.get(&eve_sde::attr_ids::EXPLOSIVE_DAMAGE_RESONANCE)
                        .copied()
                        .unwrap_or(1.0),
                ],
                ..eve_destiny::CombatState::default()
            };
        }
        eve_destiny::CombatState::default()
    }

    /// Builds a [`eve_destiny::SlimItemMeta`] for a player ship from SDE type metadata and the character's DB record (`P-7`).
    pub fn build_ship_slim_meta(&self, char_id: i64, type_id: u32) -> eve_destiny::SlimItemMeta {
        let (group_id, category_id) = self
            .sde
            .types
            .get(&type_id)
            .map(|t| (t.group_id, t.category_id))
            .unwrap_or((25, 6));
        let (corp_id, alliance_id, security_status) = self
            .db
            .get_character(char_id)
            .map(|ch| {
                (
                    ch.corporation_id,
                    ch.alliance_id.unwrap_or(0),
                    ch.security_status,
                )
            })
            .unwrap_or((1_000_009, 0, 0.0));
        eve_destiny::SlimItemMeta {
            group_id,
            category_id,
            owner_id: char_id,
            char_id: Some(char_id),
            corp_id,
            alliance_id,
            security_status,
            is_cloaked: false,
        }
    }
}
