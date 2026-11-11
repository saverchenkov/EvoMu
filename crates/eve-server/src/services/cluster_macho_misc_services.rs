//! Cluster Miscellaneous and Shipcaster Space Travel RPC Service Handlers.
//!
//! Provides SDE-backed, persisted implementations for:
//! - `charMgr.SaveCharacterSetting` and `charMgr.DeleteCharacterSetting`
//! - `shipCasterLandingPadMgr.GetAllLandingPads` and `shipCasterLandingPadMgr.GetFactionLandingPads`
//! - `shipCasterLauncherMgr.GetFactionsWithShipcaster`
//! - `starterShipcasterTravelMgr.CanCharacterJump` and `starterShipcasterTravelMgr.CmdJumpThroughStarterShipcaster`
//! - `shipcasterTravelMgr.CmdJumpThroughShipcaster`

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use eve_db::WorldRepository;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::SdeRegistry;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

/// Per-character limits to prevent memory and storage exhaustion.
const MAX_SETTINGS_PER_CHAR: usize = 500;
const MAX_SETTING_KEY_LEN: usize = 128;
const MAX_SETTING_VAL_SERIALIZED_BYTES: usize = 4_096;

/// Dynamic Faction Warfare Landing Pad (Shipcaster Beacon / Constructor) record.
/// Constructed in-space from SDE `underConstruction` (`75932..=75935`) into `builtLandingPad` (`75928..=75931`).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LandingPadRecord {
    pub item_id: i64,
    pub type_id: u32,
    #[serde(default)]
    pub built_type_id: u32,
    pub solar_system_id: u32,
    pub faction_id: i32,
    pub is_built: bool,
    pub link_timestamp: i64,
    pub is_disrupted: bool,
    pub position: glam::DVec3,
    #[serde(default)]
    pub required_materials: BTreeMap<u32, u32>,
    #[serde(default)]
    pub contributed_materials: BTreeMap<u32, u32>,
}

impl LandingPadRecord {
    pub fn to_keyval(&self) -> PyRep {
        let is_linked = self.is_built && !self.is_disrupted && self.link_timestamp > 0;
        let can_be_linked = self.is_built && !self.is_disrupted;
        PyRep::keyval([
            ("itemID", PyRep::Integer(self.item_id)),
            ("typeID", PyRep::Integer(self.type_id as i64)),
            ("solarSystemID", PyRep::Integer(self.solar_system_id as i64)),
            ("factionID", PyRep::Integer(self.faction_id as i64)),
            ("isBuilt", PyRep::Bool(self.is_built)),
            ("linkTimestamp", PyRep::Integer(self.link_timestamp)),
            ("isDisrupted", PyRep::Bool(self.is_disrupted)),
            ("isLinked", PyRep::Bool(is_linked)),
            ("canBeLinked", PyRep::Bool(can_be_linked)),
        ])
    }
}

/// SDE-backed Interstellar Shipcaster Launcher (`76465..=76468` in Empire War HQ systems, `79018` in Zarzakh).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LauncherRecord {
    pub item_id: i64,
    pub type_id: u32,
    pub solar_system_id: u32,
    pub faction_id: i32,
    pub linked_pad_ids: Vec<i64>,
    pub position: glam::DVec3,
}

/// SDE-backed Rookie Starter Shipcaster (`93968`, `93980`, `93984`, `94018`) across school starter,
/// career-agent, and Exordium embassy solar systems.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StarterLauncherRecord {
    pub item_id: i64,
    pub type_id: u32,
    pub solar_system_id: u32,
    pub faction_id: i32,
    pub staging_system_id: u32,
    #[serde(default)]
    pub staging_station_id: i64,
    pub position: glam::DVec3,
}

pub use eve_db::{
    HorizonSiegePhase, OfferedLoadSlot, PendingZarzakhTollQuote, ZarzakhHorizonSiegeState,
};

/// DERIVED: Zarzakh Horizon Siege state machine timing constants (`feat:navigation.zarzakh-local-horizon-siege`,
/// `Zarzakh.md:39`, `auditor_rows.json:6968-6974`).
pub const HORIZON_SIEGE_WARNING_SECS: u64 = 300;
pub const HORIZON_SIEGE_ACTIVE_SECS: u64 = 3_600;
pub const HORIZON_SIEGE_COOLDOWN_SECS: u64 = 7_200;

fn dvec3_from_arr(a: [f64; 3]) -> glam::DVec3 {
    glam::DVec3::new(a[0], a[1], a[2])
}

fn arr_from_dvec3(v: glam::DVec3) -> [f64; 3] {
    [v.x, v.y, v.z]
}

fn pad_from_db(p: eve_db::PersistedLandingPadRecord) -> LandingPadRecord {
    LandingPadRecord {
        item_id: p.item_id,
        type_id: p.type_id,
        built_type_id: p.built_type_id,
        solar_system_id: p.solar_system_id,
        faction_id: p.faction_id,
        is_built: p.is_built,
        link_timestamp: p.link_timestamp,
        is_disrupted: p.is_disrupted,
        position: dvec3_from_arr(p.position),
        required_materials: p.required_materials,
        contributed_materials: p.contributed_materials,
    }
}

fn pad_to_db(p: &LandingPadRecord) -> eve_db::PersistedLandingPadRecord {
    eve_db::PersistedLandingPadRecord {
        item_id: p.item_id,
        type_id: p.type_id,
        built_type_id: p.built_type_id,
        solar_system_id: p.solar_system_id,
        faction_id: p.faction_id,
        is_built: p.is_built,
        link_timestamp: p.link_timestamp,
        is_disrupted: p.is_disrupted,
        position: arr_from_dvec3(p.position),
        required_materials: p.required_materials.clone(),
        contributed_materials: p.contributed_materials.clone(),
    }
}

fn launcher_from_db(l: eve_db::PersistedLauncherRecord) -> LauncherRecord {
    LauncherRecord {
        item_id: l.item_id,
        type_id: l.type_id,
        solar_system_id: l.solar_system_id,
        faction_id: l.faction_id,
        linked_pad_ids: l.linked_pad_ids,
        position: dvec3_from_arr(l.position),
    }
}

fn launcher_to_db(l: &LauncherRecord) -> eve_db::PersistedLauncherRecord {
    eve_db::PersistedLauncherRecord {
        item_id: l.item_id,
        type_id: l.type_id,
        solar_system_id: l.solar_system_id,
        faction_id: l.faction_id,
        linked_pad_ids: l.linked_pad_ids.clone(),
        position: arr_from_dvec3(l.position),
    }
}

fn starter_launcher_from_db(l: eve_db::PersistedStarterLauncherRecord) -> StarterLauncherRecord {
    StarterLauncherRecord {
        item_id: l.item_id,
        type_id: l.type_id,
        solar_system_id: l.solar_system_id,
        faction_id: l.faction_id,
        staging_system_id: l.staging_system_id,
        staging_station_id: l.staging_station_id,
        position: dvec3_from_arr(l.position),
    }
}

fn starter_launcher_to_db(l: &StarterLauncherRecord) -> eve_db::PersistedStarterLauncherRecord {
    eve_db::PersistedStarterLauncherRecord {
        item_id: l.item_id,
        type_id: l.type_id,
        solar_system_id: l.solar_system_id,
        faction_id: l.faction_id,
        staging_system_id: l.staging_system_id,
        staging_station_id: l.staging_station_id,
        position: arr_from_dvec3(l.position),
    }
}

#[derive(Default)]
pub struct ClusterMiscRuntimeState {
    pub sde_initialized: bool,
    pub char_settings: HashMap<i64, BTreeMap<String, PyRep>>,
    pub landing_pads: Vec<LandingPadRecord>,
    pub launchers: Vec<LauncherRecord>,
    pub starter_launchers: Vec<StarterLauncherRecord>,
    pub used_starter_shipcaster_chars: BTreeSet<i64>,
    pub offered_load_slots: HashMap<i64, OfferedLoadSlot>,
    pub system_population_caps: HashMap<u32, usize>,
    pub char_energy: HashMap<i64, crate::services::character_energy::CharacterEnergyState>,
    pub target_cooldowns: HashMap<i64, u64>,
    pub target_charges: HashMap<i64, u32>,
    pub zarzakh_toll_quotes: HashMap<i64, PendingZarzakhTollQuote>,
    pub zarzakh_horizon_siege: ZarzakhHorizonSiegeState,
}

/// Pirate Insurgency Forward Operating Base landing pads linked to the Zarzakh Deathless Shipcaster.
/// Empire FW landing pads (`500001..=500004`) start empty and must be dynamically deployed, constructed, and linked.
pub fn default_pirate_fob_pads(now_unix: u64) -> Vec<LandingPadRecord> {
    let link_ts = now_unix.max(1) as i64;
    vec![
        LandingPadRecord {
            item_id: 104_000_020,
            type_id: 78260,
            built_type_id: 78260,
            solar_system_id: 30002056,
            faction_id: 500010,
            is_built: true,
            link_timestamp: link_ts,
            is_disrupted: false,
            position: glam::DVec3::new(120_000.0, 0.0, 0.0),
            required_materials: BTreeMap::new(),
            contributed_materials: BTreeMap::new(),
        },
        LandingPadRecord {
            item_id: 104_000_021,
            type_id: 79172,
            built_type_id: 79172,
            solar_system_id: 30002811,
            faction_id: 500011,
            is_built: true,
            link_timestamp: link_ts,
            is_disrupted: false,
            position: glam::DVec3::new(120_000.0, 0.0, 0.0),
            required_materials: BTreeMap::new(),
            contributed_materials: BTreeMap::new(),
        },
        // DERIVED: Zarzakh (`30_100_000`) and SDE type `79018` (`Deathless Interstellar Shipcaster`)
        // are authoritative SDE IDs; landing-pad item ID `104_000_030` and coordinates are server-side placements (`feat:navigation.deathless-shipcaster-zarzakh`).
        LandingPadRecord {
            item_id: 104_000_030,
            type_id: 79018, // Deathless Interstellar Shipcaster
            built_type_id: 79018,
            solar_system_id: 30_100_000, // Zarzakh
            faction_id: 0,
            is_built: true,
            link_timestamp: link_ts,
            is_disrupted: false,
            position: glam::DVec3::new(250_000.0, 0.0, 0.0),
            required_materials: BTreeMap::new(),
            contributed_materials: BTreeMap::new(),
        },
    ]
}

fn starter_type_and_faction_for_race(race_id: u32) -> Option<(u32, i32)> {
    match race_id {
        1 => Some((93980, 500_001)), // Caldari State Rookie Shipcaster
        2 => Some((94018, 500_002)), // Minmatar Republic Rookie Shipcaster
        4 => Some((93968, 500_003)), // Amarr Imperial Rookie Shipcaster
        8 => Some((93984, 500_004)), // Gallente Federation Rookie Shipcaster
        _ => None,
    }
}

/// Builds the 4 Empire War HQ Interstellar Shipcasters (`Mehatoor`, `Onnamon`, `Intaki`, `Amo`)
/// and the Zarzakh Deathless Interstellar Shipcaster directly from V24.01 SDE components.
pub fn sde_launchers(sde: &SdeRegistry) -> Vec<LauncherRecord> {
    type LaunchersCacheKey = (std::path::PathBuf, u64, usize, usize);
    static LAUNCHERS_CACHE: std::sync::OnceLock<
        std::sync::Mutex<HashMap<LaunchersCacheKey, Vec<LauncherRecord>>>,
    > = std::sync::OnceLock::new();

    if let Some(ref p) = sde.db_path {
        if let Ok(meta) = std::fs::metadata(p) {
            let key = (p.clone(), meta.len(), sde.stations.len(), sde.resources.len());
            let cache =
                LAUNCHERS_CACHE.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
            if let Some(cached) = cache
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&key)
                .cloned()
            {
                return cached;
            }
            let built = build_sde_launchers(sde);
            cache
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(key, built.clone());
            return built;
        }
    }
    build_sde_launchers(sde)
}

fn build_sde_launchers(sde: &SdeRegistry) -> Vec<LauncherRecord> {
    let Ok(comps) = sde.shipcaster_sde_components() else {
        return Vec::new();
    };
    let mut war_hq_by_faction: HashMap<u32, (u32, i64)> = HashMap::new();
    for &race_id in &[1_u32, 2, 4, 8] {
        if let Some((starter_tid, faction_id)) = starter_type_and_faction_for_race(race_id) {
            if let Some(&(dest_sys, dest_station)) = comps.starter_shipcasters.get(&starter_tid) {
                war_hq_by_faction.insert(faction_id as u32, (dest_sys, dest_station));
            }
        }
    }
    let mut out = Vec::new();
    for (&launcher_tid, &faction_u32) in &comps.launcher_factions {
        if faction_u32 == 500_029 {
            out.push(LauncherRecord {
                item_id: 105_000_010,
                type_id: launcher_tid,
                solar_system_id: 30_100_000,
                faction_id: faction_u32 as i32,
                linked_pad_ids: vec![104_000_020, 104_000_021],
                position: glam::DVec3::new(250_000.0, 0.0, 0.0),
            });
        } else if let Some(&(war_hq_sys, war_hq_station)) = war_hq_by_faction.get(&faction_u32) {
            let anchor_pos = sde
                .stations
                .get(&war_hq_station)
                .map(|st| st.position + glam::DVec3::new(50_000.0, 0.0, 0.0))
                .unwrap_or_else(|| glam::DVec3::new(200_000.0, 0.0, 0.0));
            out.push(LauncherRecord {
                item_id: 105_000_000 + i64::from(launcher_tid),
                type_id: launcher_tid,
                solar_system_id: war_hq_sys,
                faction_id: faction_u32 as i32,
                linked_pad_ids: Vec::new(),
                position: anchor_pos,
            });
        }
    }
    for (item_id, solar_system_id) in default_deathless_zarzakh_entry_beacons() {
        out.push(LauncherRecord {
            item_id,
            type_id: 79018,
            solar_system_id,
            faction_id: 0,
            linked_pad_ids: vec![104_000_030],
            position: glam::DVec3::new(250_000.0, 0.0, 0.0),
        });
    }
    out.sort_by_key(|l| l.faction_id);
    out
}

fn default_deathless_zarzakh_entry_beacons() -> [(i64, u32); 4] {
    [
        (105_000_011_i64, 30_002_086_u32), // Turnur
        (105_000_012_i64, 30_002_538_u32), // Vard
        (105_000_013_i64, 30_003_841_u32), // Alsavoinon
        (105_000_014_i64, 30_045_338_u32), // Hikkoken
    ]
}

/// Builds Rookie Starter Shipcaster launchers across all 12 `sde_schools` starter & career-agent
/// solar systems and the 4 Exordium embassy systems from V24.01 SDE components.
pub fn sde_starter_launchers(sde: &SdeRegistry) -> Vec<StarterLauncherRecord> {
    type StarterCacheKey = (std::path::PathBuf, u64, usize, usize);
    static STARTER_LAUNCHERS_CACHE: std::sync::OnceLock<
        std::sync::Mutex<HashMap<StarterCacheKey, Vec<StarterLauncherRecord>>>,
    > = std::sync::OnceLock::new();

    if let Some(ref p) = sde.db_path {
        if let Ok(meta) = std::fs::metadata(p) {
            let key = (p.clone(), meta.len(), sde.stations.len(), sde.resources.len());
            let cache = STARTER_LAUNCHERS_CACHE
                .get_or_init(|| std::sync::Mutex::new(HashMap::new()));
            if let Some(cached) = cache
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&key)
                .cloned()
            {
                return cached;
            }
            let built = build_sde_starter_launchers(sde);
            cache
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(key, built.clone());
            return built;
        }
    }
    build_sde_starter_launchers(sde)
}

fn build_sde_starter_launchers(sde: &SdeRegistry) -> Vec<StarterLauncherRecord> {
    let Ok(comps) = sde.shipcaster_sde_components() else {
        return Vec::new();
    };
    let systems_by_race = sde.starter_shipcaster_systems_by_race();
    let mut out = Vec::new();
    for (sys_id, (race_id, station_opt)) in systems_by_race {
        let Some((starter_tid, faction_id)) = starter_type_and_faction_for_race(race_id) else {
            continue;
        };
        let Some(&(dest_sys, dest_station)) = comps.starter_shipcasters.get(&starter_tid) else {
            continue;
        };
        let position = station_opt
            .and_then(|st_id| sde.stations.get(&st_id))
            .map(|st| st.position + glam::DVec3::new(30_000.0, 0.0, 0.0))
            .unwrap_or_else(|| glam::DVec3::new(300_000.0, 0.0, 0.0));
        out.push(StarterLauncherRecord {
            item_id: 105_100_000 + i64::from(sys_id.saturating_sub(30_000_000)),
            type_id: starter_tid,
            solar_system_id: sys_id,
            faction_id,
            staging_system_id: dest_sys,
            staging_station_id: dest_station,
            position,
        });
    }
    out
}

impl ClusterMiscRuntimeState {
    /// Deserializes previously persisted cluster miscellaneous settings from the world repository snapshot.
    pub fn from_db(db: &Arc<WorldRepository>) -> Self {
        let mut st = Self {
            sde_initialized: false,
            char_settings: HashMap::new(),
            landing_pads: default_pirate_fob_pads(db.clock().now_unix_secs()),
            launchers: Vec::new(),
            starter_launchers: Vec::new(),
            used_starter_shipcaster_chars: BTreeSet::new(),
            offered_load_slots: HashMap::new(),
            system_population_caps: HashMap::new(),
            char_energy: HashMap::new(),
            target_cooldowns: HashMap::new(),
            target_charges: HashMap::new(),
            zarzakh_toll_quotes: HashMap::new(),
            zarzakh_horizon_siege: ZarzakhHorizonSiegeState::default(),
        };
        let persisted = db.with_snapshot(|snap| snap.cluster_and_runtime.cluster_misc_state.clone());
        if let Some(p) = persisted {
            st.char_settings = p.char_settings;
            st.used_starter_shipcaster_chars = p.used_starter_shipcaster_chars;
            st.offered_load_slots = p.offered_load_slots;
            st.char_energy = p
                .char_energy
                .into_iter()
                .map(|(k, e)| {
                    (
                        k,
                        crate::services::character_energy::CharacterEnergyState {
                            current_energy: e.current_energy,
                            max_energy: e.max_energy,
                            last_update_time: e.last_update_time,
                            recharge_rate: e.recharge_rate,
                            energy_rem: e.energy_rem,
                        },
                    )
                })
                .collect();
            st.target_cooldowns = p.target_cooldowns;
            st.target_charges = p.target_charges;
            st.zarzakh_toll_quotes = p.zarzakh_toll_quotes;
            st.zarzakh_horizon_siege = p.zarzakh_horizon_siege;
            if p.persisted_shipcaster_state {
                st.landing_pads = p.landing_pads.into_iter().map(pad_from_db).collect();
                st.launchers = p.launchers.into_iter().map(launcher_from_db).collect();
                st.starter_launchers = p
                    .starter_launchers
                    .into_iter()
                    .map(starter_launcher_from_db)
                    .collect();
                st.sde_initialized = !st.launchers.is_empty() && !st.starter_launchers.is_empty();
            }
        }
        if !st.launchers.iter().any(|l| l.item_id == 105_000_010) {
            st.launchers.push(LauncherRecord {
                item_id: 105_000_010,
                type_id: 79018,
                solar_system_id: 30_100_000,
                faction_id: 500_029,
                linked_pad_ids: vec![104_000_020, 104_000_021],
                position: glam::DVec3::new(250_000.0, 0.0, 0.0),
            });
        }
        for (item_id, solar_system_id) in default_deathless_zarzakh_entry_beacons() {
            if !st.launchers.iter().any(|l| l.item_id == item_id) {
                st.launchers.push(LauncherRecord {
                    item_id,
                    type_id: 79018,
                    solar_system_id,
                    faction_id: 0,
                    linked_pad_ids: vec![104_000_030],
                    position: glam::DVec3::new(250_000.0, 0.0, 0.0),
                });
            }
        }
        st
    }

    pub fn ensure_sde_initialized(&mut self, sde: &SdeRegistry) {
        if self.sde_initialized {
            return;
        }
        if !self.launchers.iter().any(|l| matches!(l.faction_id, 500_001..=500_004)) {
            for l in sde_launchers(sde) {
                if !self.launchers.iter().any(|existing| existing.item_id == l.item_id) {
                    self.launchers.push(l);
                }
            }
            self.launchers.sort_by_key(|l| l.faction_id);
        }
        if self.starter_launchers.is_empty() {
            self.starter_launchers = sde_starter_launchers(sde);
        }
        self.sde_initialized = true;
    }
}

/// Executes a closure with exclusive access to the miscellaneous runtime state mutex,
/// lazily seeding SDE-backed shipcaster launchers on first access.
pub fn with_misc_state<R>(
    ctx: &ServerContext,
    f: impl FnOnce(&mut ClusterMiscRuntimeState) -> R,
) -> R {
    with_misc_state_arc(&ctx.cluster_misc_state, &ctx.sde, f)
}

pub(crate) fn with_misc_state_arc<R>(
    misc: &std::sync::Arc<std::sync::Mutex<ClusterMiscRuntimeState>>,
    sde: &SdeRegistry,
    f: impl FnOnce(&mut ClusterMiscRuntimeState) -> R,
) -> R {
    let mut guard = misc.lock().unwrap_or_else(|e| e.into_inner());
    guard.ensure_sde_initialized(sde);
    f(&mut guard)
}

/// Persists the character settings and cluster travel state to SQLite via `service_state_blobs`.
pub(crate) fn persist_misc_state(ctx: &ServerContext) {
    persist_misc_state_arc(&ctx.cluster_misc_state, &ctx.sde, &ctx.db);
}

pub(crate) fn persist_misc_state_arc(
    misc: &std::sync::Arc<std::sync::Mutex<ClusterMiscRuntimeState>>,
    sde: &SdeRegistry,
    db: &WorldRepository,
) {
    let p = with_misc_state_arc(misc, sde, |st| eve_db::PersistedClusterMiscState {
        persisted_shipcaster_state: st.sde_initialized,
        char_settings: st.char_settings.clone(),
        landing_pads: st.landing_pads.iter().map(pad_to_db).collect(),
        launchers: st.launchers.iter().map(launcher_to_db).collect(),
        starter_launchers: st
            .starter_launchers
            .iter()
            .map(starter_launcher_to_db)
            .collect(),
        used_starter_shipcaster_chars: st.used_starter_shipcaster_chars.clone(),
        offered_load_slots: st.offered_load_slots.clone(),
        char_energy: st
            .char_energy
            .iter()
            .map(|(&k, e)| {
                (
                    k,
                    eve_db::CharacterEnergyState {
                        current_energy: e.current_energy,
                        max_energy: e.max_energy,
                        last_update_time: e.last_update_time,
                        recharge_rate: e.recharge_rate,
                        energy_rem: e.energy_rem,
                    },
                )
            })
            .collect(),
        target_cooldowns: st.target_cooldowns.clone(),
        target_charges: st.target_charges.clone(),
        zarzakh_toll_quotes: st.zarzakh_toll_quotes.clone(),
        zarzakh_horizon_siege: st.zarzakh_horizon_siege.clone(),
    });
    let _ = db.try_with_mut_snapshot(|snap| {
        snap.cluster_and_runtime.cluster_misc_state = Some(p);
        Ok::<(), String>(())
    });
}

/// Returns the dictionary of saved character UI settings for consumption by `charMgr.GetCharacterSettings`.
pub(crate) fn get_character_settings_dict(ctx: &ServerContext, char_id: i64) -> PyRep {
    crate::services::nav_warp_interdiction::sync_waypoints_on_login(ctx, char_id);
    with_misc_state(ctx, |st| {
        let settings = st.char_settings.get(&char_id).cloned().unwrap_or_default();
        let entries: Vec<(PyRep, PyRep)> = settings
            .into_iter()
            .map(|(k, v)| (PyRep::String(k), v))
            .collect();
        PyRep::Dict(entries)
    })
}

pub fn populate_shipcaster_balls(solar_system_id: u32, ballpark: &mut eve_destiny::Ballpark) {
    populate_shipcaster_balls_with_sde(solar_system_id, ballpark, None, None);
}

pub fn populate_shipcaster_balls_from_db(
    solar_system_id: u32,
    ballpark: &mut eve_destiny::Ballpark,
    db: Option<&Arc<WorldRepository>>,
) {
    populate_shipcaster_balls_with_sde(solar_system_id, ballpark, None, db);
}

pub fn populate_shipcaster_balls_with_sde(
    solar_system_id: u32,
    ballpark: &mut eve_destiny::Ballpark,
    sde: Option<&SdeRegistry>,
    db: Option<&Arc<WorldRepository>>,
) {
    let persisted = db.and_then(|repo| {
        repo.with_snapshot(|snap| snap.cluster_and_runtime.cluster_misc_state.clone())
    });
    let launchers = persisted
        .as_ref()
        .filter(|p| p.persisted_shipcaster_state && !p.launchers.is_empty())
        .map(|p| p.launchers.iter().cloned().map(launcher_from_db).collect())
        .or_else(|| sde.map(sde_launchers))
        .unwrap_or_default();
    let starter_launchers = persisted
        .as_ref()
        .filter(|p| p.persisted_shipcaster_state && !p.starter_launchers.is_empty())
        .map(|p| {
            p.starter_launchers
                .iter()
                .cloned()
                .map(starter_launcher_from_db)
                .collect()
        })
        .or_else(|| sde.map(sde_starter_launchers))
        .unwrap_or_default();
    let now_unix = db.map(|d| d.clock().now_unix_secs()).unwrap_or(0);
    let pads = persisted
        .filter(|p| p.persisted_shipcaster_state)
        .map(|p| p.landing_pads.into_iter().map(pad_from_db).collect())
        .unwrap_or_else(|| default_pirate_fob_pads(now_unix));
    for l in launchers {
        if l.solar_system_id == solar_system_id {
            let (group_id, category_id) = if l.type_id == 79_018 {
                (4549, 65)
            } else {
                sde.and_then(|s| s.types.get(&l.type_id))
                    .map(|t| (t.group_id, t.category_id))
                    .unwrap_or((4549, 2))
            };
            ballpark.add_static_ball_with_meta(
                eve_destiny::Ball {
                    id: l.item_id,
                    type_id: l.type_id,
                    position: l.position,
                    velocity: glam::DVec3::ZERO,
                    max_velocity: 0.0,
                    inertia: 0.0,
                    mass: 1.0e10,
                    radius: 20_000.0,
                    speed_fraction: 0.0,
                    mode: eve_destiny::MovementMode::Rigid,
                },
                eve_destiny::SlimItemMeta {
                    group_id,
                    category_id,
                    owner_id: l.faction_id as i64,
                    char_id: None,
                    corp_id: 0,
                    alliance_id: -1,
                    security_status: 0.0,
                    is_cloaked: false,
                    ..Default::default()
                },
            );
        }
    }
    for sl in starter_launchers {
        if sl.solar_system_id == solar_system_id {
            let (group_id, category_id) = sde
                .and_then(|s| s.types.get(&sl.type_id))
                .map(|t| (t.group_id, t.category_id))
                .unwrap_or((4549, 2));
            ballpark.add_static_ball_with_meta(
                eve_destiny::Ball {
                    id: sl.item_id,
                    type_id: sl.type_id,
                    position: sl.position,
                    velocity: glam::DVec3::ZERO,
                    max_velocity: 0.0,
                    inertia: 0.0,
                    mass: 1.0e10,
                    radius: 20_000.0,
                    speed_fraction: 0.0,
                    mode: eve_destiny::MovementMode::Rigid,
                },
                eve_destiny::SlimItemMeta {
                    group_id,
                    category_id,
                    owner_id: sl.faction_id as i64,
                    char_id: None,
                    corp_id: 0,
                    alliance_id: -1,
                    security_status: 0.0,
                    is_cloaked: false,
                    ..Default::default()
                },
            );
        }
    }
    for p in pads {
        if p.solar_system_id == solar_system_id {
            let (group_id, category_id) = if p.type_id == 79_018 {
                (4549, 65)
            } else {
                sde.and_then(|s| s.types.get(&p.type_id))
                    .map(|t| (t.group_id, t.category_id))
                    .unwrap_or((4547, 2))
            };
            ballpark.add_static_ball_with_meta(
                eve_destiny::Ball {
                    id: p.item_id,
                    type_id: p.type_id,
                    position: p.position,
                    velocity: glam::DVec3::ZERO,
                    max_velocity: 0.0,
                    inertia: 0.0,
                    mass: 1.0e10,
                    radius: 20_000.0,
                    speed_fraction: 0.0,
                    mode: eve_destiny::MovementMode::Rigid,
                },
                eve_destiny::SlimItemMeta {
                    group_id,
                    category_id,
                    owner_id: p.faction_id as i64,
                    char_id: None,
                    corp_id: 0,
                    alliance_id: -1,
                    security_status: 0.0,
                    is_cloaked: false,
                    ..Default::default()
                },
            );
        }
    }
}

pub fn get_or_create_offered_load_slot(
    ctx: &ServerContext,
    char_id: i64,
    current_sys: u32,
) -> Option<(String, u32)> {
    let cap = with_misc_state(ctx, |st| {
        st.system_population_caps
            .get(&current_sys)
            .copied()
            .unwrap_or(usize::MAX)
    });

    let chars_in_sys: Vec<i64> = ctx.db.with_snapshot(|snap| {
        snap.characters
            .values()
            .filter(|c| c.solar_system_id == current_sys)
            .map(|c| c.character_id)
            .collect()
    });
    let online_count = chars_in_sys
        .into_iter()
        .filter(|&cid| crate::services::character_contacts::is_character_online(ctx, cid))
        .count();

    if online_count >= cap {
        let mut adj_systems: Vec<u32> = ctx
            .sde
            .jumps
            .values()
            .filter(|j| j.from_solar_system_id == current_sys)
            .map(|j| j.to_solar_system_id)
            .collect();
        if adj_systems.is_empty() {
            return None;
        }
        adj_systems.sort_by_key(|&sys| {
            let sys_chars: Vec<i64> = ctx.db.with_snapshot(|snap| {
                snap.characters
                    .values()
                    .filter(|c| c.solar_system_id == sys)
                    .map(|c| c.character_id)
                    .collect()
            });
            sys_chars
                .into_iter()
                .filter(|&cid| crate::services::character_contacts::is_character_online(ctx, cid))
                .count()
        });
        let alt_sys = adj_systems[0];
        let slot_key = uuid::Uuid::new_v4().to_string();
        with_misc_state(ctx, |st| {
            st.offered_load_slots.insert(
                char_id,
                OfferedLoadSlot {
                    slot_key: slot_key.clone(),
                    original_system_id: current_sys,
                    offered_system_ids: vec![alt_sys],
                    expires_unix: ctx.now_unix() + 300,
                },
            );
        });
        Some((slot_key, alt_sys))
    } else {
        None
    }
}

pub fn validate_and_consume_offered_load_slot(
    ctx: &ServerContext,
    char_id: i64,
    selected_sys: u32,
    slot_key: &str,
) -> bool {
    with_misc_state(ctx, |st| {
        if let Some(offer) = st.offered_load_slots.get(&char_id) {
            if offer.slot_key == slot_key
                && offer.offered_system_ids.contains(&selected_sys)
                && ctx.now_unix() <= offer.expires_unix
            {
                st.offered_load_slots.remove(&char_id);
                return true;
            }
        }
        false
    })
}

fn rpc_err(code: impl Into<String>, msg: impl Into<String>) -> BridgeRpcError {
    BridgeRpcError {
        code: code.into(),
        message: msg.into(),
    }
}

/// Dispatches requests for miscellaneous cluster and shipcaster RPC services.
pub async fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &std::collections::HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("charMgr", "SaveCharacterSetting") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(key) = args.first().and_then(|v| v.as_str()).filter(|s| !s.is_empty()) else {
                return Some(Err(rpc_err(
                    "CustomError",
                    "SaveCharacterSetting requires a non-empty setting key",
                )));
            };
            if key.len() > MAX_SETTING_KEY_LEN {
                return Some(Err(rpc_err("CustomError", "Setting key exceeds maximum length")));
            }
            let val = args.get(1).cloned().unwrap_or(PyRep::Bool(true));
            let serialized_len = serde_json::to_string(&val).map(|s| s.len()).unwrap_or(usize::MAX);
            if serialized_len > MAX_SETTING_VAL_SERIALIZED_BYTES {
                return Some(Err(rpc_err(
                    "CustomError",
                    "Setting value exceeds maximum allowed size",
                )));
            }

            if key == "waypoints" {
                return Some(
                    crate::services::nav_warp_interdiction::sync_waypoints_from_char_settings(
                        ctx, char_id, &val,
                    )
                    .map(|()| PyRep::Bool(true)),
                );
            }

            let insert_res = with_misc_state(ctx, |st| {
                let user_settings = st.char_settings.entry(char_id).or_default();
                if !user_settings.contains_key(key) && user_settings.len() >= MAX_SETTINGS_PER_CHAR {
                    return Err(rpc_err(
                        "CustomError",
                        "Character settings limit reached (maximum 500 keys)",
                    ));
                }
                user_settings.insert(key.to_owned(), val);
                Ok(())
            });

            if let Err(e) = insert_res {
                return Some(Err(e));
            }

            persist_misc_state(ctx);
            Some(Ok(PyRep::Bool(true)))
        }

        ("charMgr", "DeleteCharacterSetting") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(key) = args.first().and_then(|v| v.as_str()).filter(|s| !s.is_empty()) else {
                return Some(Err(rpc_err(
                    "CustomError",
                    "DeleteCharacterSetting requires a non-empty setting key",
                )));
            };
            if key == "waypoints" {
                ctx.db.with_mut_snapshot(|snap| {
                    if let Some(ch) = snap.characters.get_mut(&char_id) {
                        ch.autopilot_waypoints.clear();
                    }
                });
            }
            let _ = with_misc_state(ctx, |st| {
                st.char_settings
                    .get_mut(&char_id)
                    .and_then(|m| m.remove(key))
            });
            persist_misc_state(ctx);
            Some(Ok(PyRep::Bool(true)))
        }

        // --------------------------------------------------------------------
        // shipCasterLandingPadMgr, shipCasterLauncherMgr, starterShipcasterTravelMgr,
        // shipcasterTravelMgr: Interstellar and Starter Shipcaster Operations
        // --------------------------------------------------------------------
        _ => {
            if let Some(res) = crate::services::notify_emitters_skills_trade_godma::handle_misc_batch9(
                ctx,
                session,
                service,
                method,
                args,
            ) {
                return Some(res);
            }
            crate::services::shipcaster_ops::handle(ctx, session, service, method, args, _kwargs).await
        }
    }
}

pub fn check_and_apply_shipcaster_target_cooldown(
    ctx: &ServerContext,
    pad_id: i64,
    cooldown_secs: u64,
) -> Result<(), BridgeRpcError> {
    let now = ctx.now_unix();
    let res = with_misc_state(ctx, |st| {
        if let Some(&expires) = st.target_cooldowns.get(&pad_id) {
            if now < expires {
                return Err(BridgeRpcError {
                    code: "ShipcasterTargetOnCooldown".into(),
                    message: format!(
                        "Shipcaster destination pad {pad_id} is on cooldown for {} more seconds",
                        expires - now
                    ),
                });
            }
            st.target_charges.insert(pad_id, 1);
        }
        let charges = st.target_charges.entry(pad_id).or_insert(1);
        if *charges == 0 {
            return Err(BridgeRpcError {
                code: "ShipcasterTargetNoCharges".into(),
                message: format!("Shipcaster destination pad {pad_id} has 0 remaining charges"),
            });
        }
        *charges -= 1;
        st.target_cooldowns.insert(pad_id, now + cooldown_secs);
        Ok(())
    });
    if res.is_ok() {
        persist_misc_state(ctx);
    }
    res
}

fn advance_horizon_siege_at(state: &mut ZarzakhHorizonSiegeState, now_unix: u64) -> bool {
    let prev_phase = state.phase;
    let prev_restricted = state.fulcrum_restricted_to_pirates;
    if state.phase == HorizonSiegePhase::Warning
        && state.active_start_unix > 0
        && now_unix >= state.active_start_unix
    {
        if now_unix < state.active_until_unix {
            state.phase = HorizonSiegePhase::Active;
            state.phase_started_unix = state.active_start_unix;
            state.fulcrum_restricted_to_pirates = true;
            state.instability_points = state.instability_points.max(100);
        } else if now_unix < state.cooldown_until_unix {
            state.phase = HorizonSiegePhase::Stabilizing;
            state.phase_started_unix = state.active_until_unix;
            state.fulcrum_restricted_to_pirates = false;
        } else {
            state.phase = HorizonSiegePhase::Normal;
            state.phase_started_unix = state.cooldown_until_unix;
            state.fulcrum_restricted_to_pirates = false;
            state.instability_points = 0;
        }
    }
    if state.phase == HorizonSiegePhase::Active
        && state.active_until_unix > 0
        && now_unix >= state.active_until_unix
    {
        if now_unix < state.cooldown_until_unix {
            state.phase = HorizonSiegePhase::Stabilizing;
            state.phase_started_unix = state.active_until_unix;
            state.fulcrum_restricted_to_pirates = false;
        } else {
            state.phase = HorizonSiegePhase::Normal;
            state.phase_started_unix = state.cooldown_until_unix;
            state.fulcrum_restricted_to_pirates = false;
            state.instability_points = 0;
        }
    }
    if state.phase == HorizonSiegePhase::Stabilizing
        && state.cooldown_until_unix > 0
        && now_unix >= state.cooldown_until_unix
    {
        state.phase = HorizonSiegePhase::Normal;
        state.phase_started_unix = state.cooldown_until_unix;
        state.fulcrum_restricted_to_pirates = false;
        state.instability_points = 0;
    }
    state.phase != prev_phase || state.fulcrum_restricted_to_pirates != prev_restricted
}

fn emit_zarzakh_horizon_siege_changed(_ctx: &ServerContext, _state: &ZarzakhHorizonSiegeState) {
    // Horizon Siege state is kept server-side only; do not emit `OnInsurgencyStateChangedForFaction`
    // with a synthetic keyval payload.
}

pub fn tick_zarzakh_horizon_siege(ctx: &ServerContext, now_unix: u64) -> ZarzakhHorizonSiegeState {
    let (state, changed) = with_misc_state(ctx, |st| {
        let changed = advance_horizon_siege_at(&mut st.zarzakh_horizon_siege, now_unix);
        (st.zarzakh_horizon_siege.clone(), changed)
    });
    if changed {
        let is_active = state.phase == HorizonSiegePhase::Active;
        ctx.db.with_mut_snapshot(|snap| {
            snap.service_state_blobs
                .insert("zarzakh_horizon_siege_active".into(), is_active.to_string());
        });
        persist_misc_state(ctx);
        emit_zarzakh_horizon_siege_changed(ctx, &state);
    }
    state
}

pub fn get_zarzakh_horizon_siege_state(ctx: &ServerContext) -> ZarzakhHorizonSiegeState {
    tick_zarzakh_horizon_siege(ctx, ctx.now_unix())
}

/// Schedules a Zarzakh Horizon Siege warning countdown (`HORIZON_SIEGE_WARNING_SECS = 300s`)
/// when an Insurgency Forward Operating Base enters armor reinforcement (`feat:navigation.zarzakh-local-horizon-siege`).
pub fn schedule_zarzakh_horizon_siege(
    ctx: &ServerContext,
    campaign_id: Option<i64>,
) -> ZarzakhHorizonSiegeState {
    let now = ctx.now_unix();
    let state = with_misc_state(ctx, |st| {
        let _ = advance_horizon_siege_at(&mut st.zarzakh_horizon_siege, now);
        if st.zarzakh_horizon_siege.phase != HorizonSiegePhase::Active {
            let active_start = now + HORIZON_SIEGE_WARNING_SECS;
            let active_until = active_start + HORIZON_SIEGE_ACTIVE_SECS;
            let cooldown_until = active_until + HORIZON_SIEGE_COOLDOWN_SECS;
            st.zarzakh_horizon_siege = ZarzakhHorizonSiegeState {
                phase: HorizonSiegePhase::Warning,
                phase_started_unix: now,
                active_start_unix: active_start,
                active_until_unix: active_until,
                cooldown_until_unix: cooldown_until,
                instability_points: st
                    .zarzakh_horizon_siege
                    .instability_points
                    .saturating_add(50)
                    .min(100),
                triggered_by_campaign_id: campaign_id,
                fulcrum_restricted_to_pirates: false,
            };
        }
        st.zarzakh_horizon_siege.clone()
    });
    ctx.db.with_mut_snapshot(|snap| {
        snap.service_state_blobs.insert(
            "zarzakh_horizon_siege_active".into(),
            (state.phase == HorizonSiegePhase::Active).to_string(),
        );
    });
    persist_misc_state(ctx);
    emit_zarzakh_horizon_siege_changed(ctx, &state);
    state
}

/// Immediately activates a Zarzakh Horizon Siege (`HorizonSiegePhase::Active`) when an Insurgency FOB
/// is destroyed or an Insurgency resolves as `EmpireWin` (`feat:navigation.zarzakh-local-horizon-siege`,
/// `feat:navigation.deathless-shipcaster-zarzakh`).
pub fn trigger_zarzakh_horizon_siege(
    ctx: &ServerContext,
    campaign_id: Option<i64>,
    duration_secs: u64,
) -> ZarzakhHorizonSiegeState {
    let now = ctx.now_unix();
    let dur = duration_secs.max(1);
    let state = with_misc_state(ctx, |st| {
        st.zarzakh_horizon_siege = ZarzakhHorizonSiegeState {
            phase: HorizonSiegePhase::Active,
            phase_started_unix: now,
            active_start_unix: now,
            active_until_unix: now + dur,
            cooldown_until_unix: now + dur + HORIZON_SIEGE_COOLDOWN_SECS,
            instability_points: 100,
            triggered_by_campaign_id: campaign_id,
            fulcrum_restricted_to_pirates: true,
        };
        st.zarzakh_horizon_siege.clone()
    });
    ctx.db.with_mut_snapshot(|snap| {
        snap.service_state_blobs
            .insert("zarzakh_horizon_siege_active".into(), "true".into());
    });
    persist_misc_state(ctx);
    emit_zarzakh_horizon_siege_changed(ctx, &state);
    state
}

pub fn is_zarzakh_horizon_siege_active(ctx: &ServerContext) -> bool {
    let state = get_zarzakh_horizon_siege_state(ctx);
    if state.phase == HorizonSiegePhase::Active {
        return true;
    }
    ctx.db.with_snapshot(|snap| {
        snap.service_state_blobs
            .get("zarzakh_horizon_siege_active")
            .and_then(|v| v.parse::<bool>().ok())
            .unwrap_or(false)
    })
}

pub fn set_zarzakh_horizon_siege_active(ctx: &ServerContext, active: bool) {
    if active {
        let _ = trigger_zarzakh_horizon_siege(ctx, None, HORIZON_SIEGE_ACTIVE_SECS);
    } else {
        let now = ctx.now_unix();
        let state = with_misc_state(ctx, |st| {
            st.zarzakh_horizon_siege = ZarzakhHorizonSiegeState {
                phase: HorizonSiegePhase::Normal,
                phase_started_unix: now,
                active_start_unix: 0,
                active_until_unix: 0,
                cooldown_until_unix: 0,
                instability_points: 0,
                triggered_by_campaign_id: None,
                fulcrum_restricted_to_pirates: false,
            };
            st.zarzakh_horizon_siege.clone()
        });
        ctx.db.with_mut_snapshot(|snap| {
            snap.service_state_blobs
                .insert("zarzakh_horizon_siege_active".into(), "false".into());
        });
        persist_misc_state(ctx);
        emit_zarzakh_horizon_siege_changed(ctx, &state);
    }
}

#[cfg(test)] #[path = "cluster_macho_misc_services_tests.rs"] mod cluster_macho_misc_services_tests;

