//! Operational telemetry ring buffers, startup presence summary, bootstrap
//! request-to-wire timing ledger, and `#1077` ops snapshot providers (`#1087`).

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::warn_limiter::{global_warn_suppressed_summary, WarnSuppressedSummary};
use eve_sde::SolarSystemId;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet, VecDeque};
use std::sync::{Mutex, OnceLock};

/// Maximum number of recent cluster/physics tick summaries retained for the ops endpoint.
pub const MAX_TICK_SUMMARIES: usize = 120;
/// Maximum number of recent bootstrap (`login` / `undock`) timings retained.
pub const MAX_BOOTSTRAP_TIMINGS: usize = 256;

/// Summary of one server physics/cluster tick.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TickSummary {
    pub tick_seq: u64,
    pub timestamp_unix: u64,
    pub duration_ms: u64,
    pub lateness_ms: i64,
    pub active_systems: usize,
}

/// Bounded ring buffer of the last [`MAX_TICK_SUMMARIES`] ticks.
#[derive(Debug)]
pub struct TickSummaryRing {
    capacity: usize,
    entries: Mutex<VecDeque<TickSummary>>,
}

impl Default for TickSummaryRing {
    fn default() -> Self {
        Self::new(MAX_TICK_SUMMARIES)
    }
}

impl TickSummaryRing {
    pub fn new(capacity: usize) -> Self {
        let cap = capacity.max(1);
        Self {
            capacity: cap,
            entries: Mutex::new(VecDeque::with_capacity(cap)),
        }
    }

    pub fn push(&self, summary: TickSummary) {
        let mut guard = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        if guard.len() >= self.capacity {
            guard.pop_front();
        }
        guard.push_back(summary);
    }

    pub fn snapshot(&self) -> Vec<TickSummary> {
        let guard = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        guard.iter().cloned().collect()
    }

    pub fn last_lateness_ms(&self) -> i64 {
        let guard = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        guard.back().map(|t| t.lateness_ms).unwrap_or(0)
    }

    pub fn clear(&self) {
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }
}

static GLOBAL_TICK_RING: OnceLock<TickSummaryRing> = OnceLock::new();

pub fn global_tick_ring() -> &'static TickSummaryRing {
    GLOBAL_TICK_RING.get_or_init(TickSummaryRing::default)
}

/// Records a tick summary into the global 120-tick ring and updates the lifecycle lateness gauge.
pub fn record_tick_summary(summary: TickSummary) {
    crate::lifecycle::set_last_tick_lateness_ms(summary.lateness_ms);
    global_tick_ring().push(summary);
}

/// Returns up to the last 120 tick summaries in chronological order.
pub fn recent_tick_summaries() -> Vec<TickSummary> {
    global_tick_ring().snapshot()
}

/// Timing measurement attached to bootstrap (`login` / `undock`) packets for `#1077`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BootstrapPacketTiming {
    pub phase: String,
    pub service: String,
    pub method: String,
    pub session_id: u64,
    pub character_id: Option<i64>,
    pub elapsed_since_request_ms: u64,
    pub timestamp_unix: u64,
}

static GLOBAL_BOOTSTRAP_TIMINGS: OnceLock<Mutex<VecDeque<BootstrapPacketTiming>>> = OnceLock::new();

fn bootstrap_ring() -> &'static Mutex<VecDeque<BootstrapPacketTiming>> {
    GLOBAL_BOOTSTRAP_TIMINGS
        .get_or_init(|| Mutex::new(VecDeque::with_capacity(MAX_BOOTSTRAP_TIMINGS)))
}

/// Classifies whether an RPC belongs to the login or undock bootstrap path.
#[inline]
pub fn classify_bootstrap_phase(service: &str, method: &str) -> Option<&'static str> {
    match (service, method) {
        ("authentication", "Login")
        | ("charUnboundMgr", "SelectCharacterID")
        | ("charUnboundMgr", "GetCharacterSelectionData")
        | ("charUnboundMgr", "GetCharactersToSelect")
        | ("charUnboundMgr", "GetCharacterInfo")
        | ("character", "SelectCharacterID")
        | ("character", "GetCharacterSelectionInfo") => Some("login"),
        ("stationSvc", "Undock")
        | ("structureDocking", "Undock")
        | ("ship", "Undock")
        | ("beyonce", "Undock")
        | ("beyonce", "UpdateStateRequest")
        | ("beyonce", "MachoBindObject") => Some("undock"),
        _ => None,
    }
}

pub fn record_bootstrap_timing(timing: BootstrapPacketTiming) {
    let mut guard = bootstrap_ring().lock().unwrap_or_else(|e| e.into_inner());
    if guard.len() >= MAX_BOOTSTRAP_TIMINGS {
        guard.pop_front();
    }
    guard.push_back(timing);
}

pub fn recent_bootstrap_timings() -> Vec<BootstrapPacketTiming> {
    bootstrap_ring()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .cloned()
        .collect()
}

pub fn clear_bootstrap_timings_for_test() {
    bootstrap_ring()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
}

/// Startup presence summary emitted after world seeding so seeding regressions show
/// up as a diff between two server boots.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartupPresenceSummary {
    pub systems_preloaded: usize,
    pub structures_loaded: usize,
    pub npc_entities_live: usize,
    pub npc_entities_abstract: usize,
    pub anchors_beacons: usize,
    pub elapsed_ms: u64,
}

impl StartupPresenceSummary {
    /// Formats one deterministic line per category for boot logs / diffs.
    pub fn format_lines(&self) -> Vec<String> {
        vec![
            format!("startup_presence.systems_preloaded={}", self.systems_preloaded),
            format!("startup_presence.structures_loaded={}", self.structures_loaded),
            format!(
                "startup_presence.npc_entities_live={} startup_presence.npc_entities_abstract={}",
                self.npc_entities_live, self.npc_entities_abstract
            ),
            format!("startup_presence.anchors_beacons={}", self.anchors_beacons),
            format!("startup_presence.elapsed_ms={}", self.elapsed_ms),
        ]
    }
}

/// Computes the startup presence summary from `ServerContext`.
pub fn compute_startup_presence_summary(
    ctx: &ServerContext,
    elapsed_ms: u64,
) -> StartupPresenceSummary {
    let systems_preloaded = ctx.systems.active_system_handles().len();
    let (structures_loaded, anchors_beacons) = ctx.db.with_snapshot(|snap| {
        let structures = snap.structures.len();
        let anchors_beacons = snap
            .crab_beacons
            .beacons
            .len()
            .saturating_add(snap.deployable_state.deployables.len())
            .saturating_add(snap.starbase_settings.len());
        (structures, anchors_beacons)
    });

    let mut live_npc_ids: HashSet<i64> = {
        let guard = ctx.npc_spawns.read().unwrap_or_else(|e| e.into_inner());
        guard
            .values()
            .flat_map(|mgr| mgr.entities.keys().copied())
            .collect()
    };
    crate::services::combat_crimewatch::with_combat_ephemeral(ctx, |rt| {
        live_npc_ids.extend(rt.npc_entities.keys().copied());
    });
    let npc_live = live_npc_ids.len();
    let (lw_live, lw_abstract) = ctx
        .living_world
        .convoys
        .read()
        .map(|convoys| {
            let mut live = 0usize;
            let mut abs = 0usize;
            for c in convoys.values() {
                if c.destroyed {
                    continue;
                }
                if c.lod_level >= 2 {
                    live += 1;
                } else {
                    abs += 1;
                }
            }
            (live, abs)
        })
        .unwrap_or((0, 0));

    StartupPresenceSummary {
        systems_preloaded,
        structures_loaded,
        npc_entities_live: npc_live.saturating_add(lw_live),
        npc_entities_abstract: lw_abstract,
        anchors_beacons,
        elapsed_ms,
    }
}

/// Logs one `tracing::info!` line per category after world seeding finishes.
pub fn log_startup_presence_summary(ctx: &ServerContext, elapsed_ms: u64) -> StartupPresenceSummary {
    let summary = compute_startup_presence_summary(ctx, elapsed_ms);
    for line in summary.format_lines() {
        tracing::info!("{line}");
    }
    summary
}

/// Active session count for a single solar system.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemSessionCount {
    pub solar_system_id: SolarSystemId,
    pub session_count: usize,
}

/// Effective TiDi factor and manual override metadata for one solar system.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SystemTidiSummary {
    pub solar_system_id: SolarSystemId,
    pub factor: f64,
    pub is_manual_override: bool,
}

/// Complete `#1087` telemetry snapshot designed to be exposed by `#1077`'s loopback ops endpoint.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServerOpsTelemetrySnapshot {
    pub last_ticks: Vec<TickSummary>,
    pub per_system_sessions: Vec<SystemSessionCount>,
    pub auto_tidi_enabled: bool,
    pub manual_tidi_overrides: BTreeMap<SolarSystemId, f64>,
    pub system_tidi: Vec<SystemTidiSummary>,
    pub warn_suppressed: WarnSuppressedSummary,
    pub recent_bootstrap_timings: Vec<BootstrapPacketTiming>,
}

/// Collects a point-in-time telemetry snapshot from `ServerContext` and global limiters/rings.
pub fn collect_ops_telemetry_snapshot(ctx: &ServerContext) -> ServerOpsTelemetrySnapshot {
    let mut counts: BTreeMap<SolarSystemId, usize> = BTreeMap::new();
    ctx.db.with_snapshot(|snap| {
        for sess in snap.sessions.values() {
            if let Some(cid) = sess.character_id {
                if let Some(ch) = snap.characters.get(&cid) {
                    *counts.entry(ch.solar_system_id).or_insert(0) += 1;
                }
            }
        }
    });
    let per_system_sessions = counts
        .into_iter()
        .map(|(solar_system_id, session_count)| SystemSessionCount {
            solar_system_id,
            session_count,
        })
        .collect();

    let (auto_tidi_enabled, manual_tidi_overrides) = ctx
        .systems
        .cluster_nodes
        .read()
        .map(|cn| {
            let mut overrides = BTreeMap::new();
            for (&sys_id, &factor) in &cn.manual_tidi_overrides {
                overrides.insert(sys_id, factor);
            }
            (cn.auto_tidi_enabled, overrides)
        })
        .unwrap_or_else(|_| (false, BTreeMap::new()));

    let mut tidi_map: BTreeMap<SolarSystemId, SystemTidiSummary> = BTreeMap::new();
    for (sys_id, handle) in ctx.systems.active_system_handles() {
        let is_manual = manual_tidi_overrides.contains_key(&sys_id);
        tidi_map.insert(
            sys_id,
            SystemTidiSummary {
                solar_system_id: sys_id,
                factor: handle.tidi_factor(),
                is_manual_override: is_manual,
            },
        );
    }
    for (&sys_id, &factor) in &manual_tidi_overrides {
        tidi_map.entry(sys_id).or_insert(SystemTidiSummary {
            solar_system_id: sys_id,
            factor: factor.clamp(0.10, 1.0),
            is_manual_override: true,
        });
    }

    ServerOpsTelemetrySnapshot {
        last_ticks: recent_tick_summaries(),
        per_system_sessions,
        auto_tidi_enabled,
        manual_tidi_overrides,
        system_tidi: tidi_map.into_values().collect(),
        warn_suppressed: global_warn_suppressed_summary(),
        recent_bootstrap_timings: recent_bootstrap_timings(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::router::ServerContext;
    use eve_db::OnlineSessionRecord;

    #[test]
    fn tick_summary_ring_caps_at_120_and_tracks_last_lateness() {
        let ring = TickSummaryRing::default();
        for seq in 1_u64..=150 {
            ring.push(TickSummary {
                tick_seq: seq,
                timestamp_unix: 1_700_000_000 + seq,
                duration_ms: 4,
                lateness_ms: (seq as i64) - 5,
                active_systems: 2,
            });
        }
        let snap = ring.snapshot();
        assert_eq!(snap.len(), MAX_TICK_SUMMARIES);
        assert_eq!(snap.first().unwrap().tick_seq, 31);
        assert_eq!(snap.last().unwrap().tick_seq, 150);
        assert_eq!(ring.last_lateness_ms(), 145);
    }

    #[test]
    fn startup_presence_summary_and_telemetry_snapshot_reflect_sessions_and_tidi() {
        let ctx = ServerContext::new();
        let summary = log_startup_presence_summary(&ctx, 42);
        let lines = summary.format_lines();
        assert_eq!(lines.len(), 5);
        assert!(lines[0].contains("systems_preloaded="));
        assert!(lines[1].contains("structures_loaded="));
        assert!(lines[2].contains("npc_entities_live="));
        assert!(lines[2].contains("npc_entities_abstract="));
        assert!(lines[3].contains("anchors_beacons="));
        assert!(lines[4].contains("elapsed_ms=42"));

        let first_char = ctx.db.with_mut_snapshot(|snap| {
            let cid = snap.characters.keys().next().copied();
            if let Some(character_id) = cid {
                snap.sessions.insert(
                    77_001,
                    OnlineSessionRecord {
                        user_id: 1,
                        username: "telemetry_pilot".into(),
                        session_id: 77_001,
                        character_id: Some(character_id),
                        connected_at_unix: 1_700_000_000,
                        computer_hash: None,
                    },
                );
            }
            cid.and_then(|c| snap.characters.get(&c).map(|ch| ch.solar_system_id))
        });

        {
            let mut cn = ctx
                .systems
                .cluster_nodes
                .write()
                .unwrap_or_else(|e| e.into_inner());
            cn.manual_tidi_overrides.insert(30_000_142, 0.5);
        }

        let telemetry = collect_ops_telemetry_snapshot(&ctx);
        assert_eq!(telemetry.manual_tidi_overrides.get(&30_000_142), Some(&0.5));
        assert!(telemetry
            .system_tidi
            .iter()
            .any(|t| t.solar_system_id == 30_000_142 && t.is_manual_override));
        if let Some(sys_id) = first_char {
            assert!(telemetry
                .per_system_sessions
                .iter()
                .any(|s| s.solar_system_id == sys_id && s.session_count >= 1));
        }
    }

    #[test]
    fn classify_bootstrap_phase_covers_login_and_undock_rpcs() {
        assert_eq!(classify_bootstrap_phase("authentication", "Login"), Some("login"));
        assert_eq!(
            classify_bootstrap_phase("charUnboundMgr", "GetCharacterInfo"),
            Some("login")
        );
        assert_eq!(
            classify_bootstrap_phase("character", "GetCharacterSelectionInfo"),
            Some("login")
        );
        assert_eq!(classify_bootstrap_phase("ship", "Undock"), Some("undock"));
        assert_eq!(classify_bootstrap_phase("stationSvc", "Undock"), Some("undock"));
        assert_eq!(classify_bootstrap_phase("beyonce", "MachoBindObject"), Some("undock"));
        assert_eq!(classify_bootstrap_phase("marketProxy", "GetOrders"), None);
    }

    #[test]
    fn startup_presence_summary_includes_starbases_and_dedups_npc_entities() {
        use crate::combat::npc_ai::{NpcAiState, NpcCombatEntity, NpcSpawnManager};
        use crate::services::combat_crimewatch::{with_mut_combat_ephemeral, ExtendedNpcEntity};
        use eve_destiny::DamageVector;
        use glam::DVec3;
        use std::collections::HashMap;

        let ctx = ServerContext::new();
        let baseline = compute_startup_presence_summary(&ctx, 1);

        ctx.db.with_mut_snapshot(|snap| {
            snap.starbase_settings.insert(910_000_001, Default::default());
        });

        let sys_id = 30_000_142_u32;
        let shared_id = 920_000_001_i64;
        let belt_only_id = 920_000_002_i64;
        let special_only_id = 920_000_003_i64;

        let mk_belt_npc = |ball_id: i64| NpcCombatEntity {
            ball_id,
            type_id: 10_019,
            faction_id: 500_010,
            state: NpcAiState::Idle,
            spawn_group_id: 1,
            spawn_pos: DVec3::ZERO,
            target_id: None,
            bounty_isk: 1_000.0,
            optimal_range: 10_000.0,
            orbit_range: 5_000.0,
            base_dps: 10.0,
            damage_vector: DamageVector::default(),
            last_attack_tick: 0,
            has_shield_rep: false,
            has_armor_rep: false,
        };
        let mk_ext_npc = |ball_id: i64| ExtendedNpcEntity {
            ball_id,
            type_id: 10_019,
            faction_id: 500_010,
            archetype: "Drifter".into(),
            solar_system_id: sys_id,
            spawn_group_id: 1,
            target_id: None,
            aggression_threat: HashMap::new(),
            threat_table: HashMap::new(),
            bounty_isk: 10_000.0,
            optimal_range: 10_000.0,
            orbit_range: 5_000.0,
            damage_vector: DamageVector::default(),
            entropic_cycles: 0,
            drifter_overshield_hp: 0.0,
            drifter_doomsday_fired: false,
            web_speed_factor: 0.0,
            web_range: 0.0,
            scram_strength: 0.0,
            scram_range: 0.0,
            neut_amount: 0.0,
            ecm_jammed: false,
            remote_shield_rep: 0.0,
            remote_armor_rep: 0.0,
            remote_rep_range: 0.0,
            tag_drop_type_id: None,
            spawn_position: DVec3::ZERO,
            sleeper_wh_class: 0,
            custom_cargo_loot: None,
        };

        {
            let mut spawns = ctx.npc_spawns.write().unwrap_or_else(|e| e.into_inner());
            let mgr = spawns
                .entry(sys_id)
                .or_insert_with(NpcSpawnManager::default);
            mgr.entities.insert(shared_id, mk_belt_npc(shared_id));
            mgr.entities.insert(belt_only_id, mk_belt_npc(belt_only_id));
        }
        with_mut_combat_ephemeral(&ctx, |rt| {
            rt.npc_entities.insert(shared_id, mk_ext_npc(shared_id));
            rt.npc_entities
                .insert(special_only_id, mk_ext_npc(special_only_id));
        });

        let after = compute_startup_presence_summary(&ctx, 7);
        assert_eq!(after.anchors_beacons, baseline.anchors_beacons + 1);
        assert_eq!(after.npc_entities_live, baseline.npc_entities_live + 3);
        assert_eq!(after.elapsed_ms, 7);
    }
}
