//! Cluster Miscellaneous and Shipcaster Space Travel RPC Service Handlers.
//!
//! Provides solid, state-backed implementations for:
//! - `charMgr.SaveCharacterSetting` and `charMgr.DeleteCharacterSetting`
//! - `shipCasterLandingPadMgr.GetAllLandingPads` and `shipCasterLandingPadMgr.GetFactionLandingPads`
//! - `shipCasterLauncherMgr.GetFactionsWithShipcaster`
//! - `starterShipcasterTravelMgr.CanCharacterJump` and `starterShipcasterTravelMgr.CmdJumpThroughStarterShipcaster`
//! - `shipcasterTravelMgr.CmdJumpThroughShipcaster`
//!
//! # Game Mechanics & Client Protocol Rationale
//! - **Proximity & Distance**: Shipcaster jumping requires an active launcher ball in the current
//!   solar system and operational range (<= 5,000m). Proximity checks fail closed if balls are absent.
//! - **Combat/Weapons Timers**: Active crimewatch weapons timers prohibit shipcaster jumping.
//! - **Destination Verification**: Shipcaster jumps enforce linked, active, non-disrupted landing pad registries.
//! - **Jump Handoff & Session State**: Interstellar jumps remove the ship ball from the origin system,
//!   spawn the ship ball with combat state at the destination, update character and item records in DB,
//!   and emit complete `OnSessionChanged` events with `solarsystemid`, `solarsystemid2`, `constellationid`,
//!   and `regionid`.
//! - **Storage Safety**: Mutex `cluster_misc_state` is acquired and released prior to taking database
//!   snapshot write locks, eliminating lock-order inversion and deadlock risks.
//!
//! All unhandled, hollow, or unverified RPC arms return `None` to fall through to `UnimplementedRPC`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use eve_db::WorldRepository;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

const MISC_STATE_BLOB_KEY: &str = "cluster_misc_state";

/// Per-character limits to prevent memory and storage exhaustion.
const MAX_SETTINGS_PER_CHAR: usize = 500;
const MAX_SETTING_KEY_LEN: usize = 128;
const MAX_SETTING_VAL_SERIALIZED_BYTES: usize = 4_096;


/// DERIVED: Canonical Faction Warfare Landing Pad (Shipcaster Beacon) record.
/// Traced to authentic SDE type IDs 75928..=75931.
#[derive(Clone, Serialize, Deserialize)]
pub struct LandingPadRecord {
    pub item_id: i64,
    pub type_id: u32,
    pub solar_system_id: u32,
    pub faction_id: i32,
    pub is_built: bool,
    pub link_timestamp: i64,
    pub is_disrupted: bool,
    pub position: glam::DVec3,
}

impl LandingPadRecord {
    pub fn to_keyval(&self) -> PyRep {
        let is_linked = self.link_timestamp > 0;
        let can_be_linked = self.is_built && !self.is_disrupted;
        PyRep::keyval([
            ("itemID", PyRep::Integer(self.item_id)),
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

/// DERIVED: Canonical Faction Warfare Interstellar Shipcaster Launcher record.
/// Traced to authentic SDE type IDs 76465..=76468.
#[derive(Clone, Serialize, Deserialize)]
pub struct LauncherRecord {
    pub item_id: i64,
    pub type_id: u32,
    pub solar_system_id: u32,
    pub faction_id: i32,
    pub linked_pad_ids: Vec<i64>,
    pub position: glam::DVec3,
}

/// DERIVED: Canonical Starter Shipcaster (Rookie Shipcaster) record.
/// Traced to authentic SDE type IDs 93968..=94018.
#[derive(Clone, Serialize, Deserialize)]
pub struct StarterLauncherRecord {
    pub item_id: i64,
    pub type_id: u32,
    pub solar_system_id: u32,
    pub faction_id: i32,
    pub staging_system_id: u32,
    pub position: glam::DVec3,
}

/// Server-offered alternative load slot when a solar system is under population cap restrictions.
#[derive(Clone, Serialize, Deserialize)]
pub struct OfferedLoadSlot {
    pub slot_key: String,
    pub original_system_id: u32,
    pub offered_system_ids: Vec<u32>,
    pub expires_unix: u64,
}

#[derive(Default, Clone, Serialize, Deserialize)]
struct PersistedClusterMiscState {
    #[serde(default)]
    char_settings: HashMap<i64, BTreeMap<String, PyRep>>,
    #[serde(default)]
    landing_pads: Vec<LandingPadRecord>,
    #[serde(default)]
    launchers: Vec<LauncherRecord>,
    #[serde(default)]
    starter_launchers: Vec<StarterLauncherRecord>,
    #[serde(default)]
    offered_load_slots: HashMap<i64, OfferedLoadSlot>,
    #[serde(default)]
    char_energy: HashMap<i64, crate::services::character_energy::CharacterEnergyState>,
}

#[derive(Default)]
pub struct ClusterMiscRuntimeState {
    pub char_settings: HashMap<i64, BTreeMap<String, PyRep>>,
    pub landing_pads: Vec<LandingPadRecord>,
    pub launchers: Vec<LauncherRecord>,
    pub starter_launchers: Vec<StarterLauncherRecord>,
    pub offered_load_slots: HashMap<i64, OfferedLoadSlot>,
    pub system_population_caps: HashMap<u32, usize>,
    pub char_energy: HashMap<i64, crate::services::character_energy::CharacterEnergyState>,
}

/// DERIVED: Canonical Empire Landing Pads (Shipcaster Beacons) in Faction Warfare warzones.
pub fn derived_landing_pads() -> Vec<LandingPadRecord> {
    vec![
        LandingPadRecord {
            item_id: 104_000_001,
            type_id: 75929, // State Shipcaster Beacon
            solar_system_id: 30002811, // Onatoh / Caldari warzone
            faction_id: 500001,
            is_built: true,
            link_timestamp: 1_700_000_000,
            is_disrupted: false,
            position: glam::DVec3::new(100_000.0, 0.0, 0.0),
        },
        LandingPadRecord {
            item_id: 104_000_002,
            type_id: 75931, // Republic Shipcaster Beacon
            solar_system_id: 30002056, // Resbroko / Minmatar warzone
            faction_id: 500002,
            is_built: true,
            link_timestamp: 1_700_000_000,
            is_disrupted: false,
            position: glam::DVec3::new(100_000.0, 0.0, 0.0),
        },
        LandingPadRecord {
            item_id: 104_000_003,
            type_id: 75928, // Imperial Shipcaster Beacon
            solar_system_id: 30002185, // NI-J0B / Amarr warzone
            faction_id: 500003,
            is_built: true,
            link_timestamp: 1_700_000_000,
            is_disrupted: false,
            position: glam::DVec3::new(100_000.0, 0.0, 0.0),
        },
        LandingPadRecord {
            item_id: 104_000_004,
            type_id: 75930, // Federation Shipcaster Beacon
            solar_system_id: 30003855, // Mantenault / Gallente warzone
            faction_id: 500004,
            is_built: true,
            link_timestamp: 1_700_000_000,
            is_disrupted: false,
            position: glam::DVec3::new(100_000.0, 0.0, 0.0),
        },
    ]
}

/// DERIVED: Canonical Interstellar Shipcaster Launchers in Empire staging systems.
pub fn derived_launchers() -> Vec<LauncherRecord> {
    vec![
        LauncherRecord {
            item_id: 105_000_001,
            type_id: 76466, // State Interstellar Shipcaster
            solar_system_id: 30002813, // Tama / Caldari staging
            faction_id: 500001,
            linked_pad_ids: vec![104_000_001],
            position: glam::DVec3::new(200_000.0, 0.0, 0.0),
        },
        LauncherRecord {
            item_id: 105_000_002,
            type_id: 76468, // Republic Interstellar Shipcaster
            solar_system_id: 30002057, // Hadozeko / Minmatar staging
            faction_id: 500002,
            linked_pad_ids: vec![104_000_002],
            position: glam::DVec3::new(200_000.0, 0.0, 0.0),
        },
        LauncherRecord {
            item_id: 105_000_003,
            type_id: 76465, // Imperial Interstellar Shipcaster
            solar_system_id: 30002187, // Amarr staging
            faction_id: 500003,
            linked_pad_ids: vec![104_000_003],
            position: glam::DVec3::new(200_000.0, 0.0, 0.0),
        },
        LauncherRecord {
            item_id: 105_000_004,
            type_id: 76467, // Federation Interstellar Shipcaster
            solar_system_id: 30003856, // Athounon / Gallente staging
            faction_id: 500004,
            linked_pad_ids: vec![104_000_004],
            position: glam::DVec3::new(200_000.0, 0.0, 0.0),
        },
    ]
}

/// DERIVED: Canonical Rookie Starter Shipcasters in Empire beginner systems.
pub fn derived_starter_launchers() -> Vec<StarterLauncherRecord> {
    vec![
        StarterLauncherRecord {
            item_id: 105_000_101,
            type_id: 93980, // State Rookie Shipcaster
            solar_system_id: 30002800, // Starter system
            faction_id: 500001,
            staging_system_id: 30002813, // Tama staging
            position: glam::DVec3::new(300_000.0, 0.0, 0.0),
        },
        StarterLauncherRecord {
            item_id: 105_000_102,
            type_id: 94018, // Republic Rookie Shipcaster
            solar_system_id: 30002050,
            faction_id: 500002,
            staging_system_id: 30002057,
            position: glam::DVec3::new(300_000.0, 0.0, 0.0),
        },
        StarterLauncherRecord {
            item_id: 105_000_103,
            type_id: 93968, // Imperial Rookie Shipcaster
            solar_system_id: 30002180,
            faction_id: 500003,
            staging_system_id: 30002187,
            position: glam::DVec3::new(300_000.0, 0.0, 0.0),
        },
        StarterLauncherRecord {
            item_id: 105_000_104,
            type_id: 93984, // Federation Rookie Shipcaster
            solar_system_id: 30003850,
            faction_id: 500004,
            staging_system_id: 30003856,
            position: glam::DVec3::new(300_000.0, 0.0, 0.0),
        },
    ]
}

impl ClusterMiscRuntimeState {
    /// Deserializes previously persisted cluster miscellaneous settings from the world repository snapshot.
    pub fn from_db(db: &Arc<WorldRepository>) -> Self {
        let mut st = Self {
            char_settings: HashMap::new(),
            landing_pads: derived_landing_pads(),
            launchers: derived_launchers(),
            starter_launchers: derived_starter_launchers(),
            offered_load_slots: HashMap::new(),
            system_population_caps: HashMap::new(),
            char_energy: HashMap::new(),
        };
        let persisted: Option<PersistedClusterMiscState> = db.with_snapshot(|snap| {
            snap.service_state_blobs
                .get(MISC_STATE_BLOB_KEY)
                .and_then(|json| serde_json::from_str(json).ok())
        });
        if let Some(p) = persisted {
            st.char_settings = p.char_settings;
            if !p.landing_pads.is_empty() {
                st.landing_pads = p.landing_pads;
            }
            if !p.launchers.is_empty() {
                st.launchers = p.launchers;
            }
            if !p.starter_launchers.is_empty() {
                st.starter_launchers = p.starter_launchers;
            }
            st.offered_load_slots = p.offered_load_slots;
            st.char_energy = p.char_energy;
        }
        st
    }
}

/// Executes a closure with exclusive access to the miscellaneous runtime state mutex.
pub fn with_misc_state<R>(
    ctx: &ServerContext,
    f: impl FnOnce(&mut ClusterMiscRuntimeState) -> R,
) -> R {
    let mut guard = ctx
        .cluster_misc_state
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    f(&mut guard)
}

/// Persists the character settings and cluster travel state to SQLite via `service_state_blobs`.
pub(crate) fn persist_misc_state(ctx: &ServerContext) {
    let p = with_misc_state(ctx, |st| PersistedClusterMiscState {
        char_settings: st.char_settings.clone(),
        landing_pads: st.landing_pads.clone(),
        launchers: st.launchers.clone(),
        starter_launchers: st.starter_launchers.clone(),
        offered_load_slots: st.offered_load_slots.clone(),
        char_energy: st.char_energy.clone(),
    });
    if let Ok(json) = serde_json::to_string(&p) {
        let _ = ctx.db.try_with_mut_snapshot(|snap| {
            snap.service_state_blobs
                .insert(MISC_STATE_BLOB_KEY.to_string(), json);
            Ok::<(), String>(())
        });
    }
}

/// Returns the dictionary of saved character UI settings for consumption by `charMgr.GetCharacterSettings`.
pub(crate) fn get_character_settings_dict(ctx: &ServerContext, char_id: i64) -> PyRep {
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
    populate_shipcaster_balls_from_db(solar_system_id, ballpark, None);
}

pub fn populate_shipcaster_balls_from_db(
    solar_system_id: u32,
    ballpark: &mut eve_destiny::Ballpark,
    db: Option<&Arc<WorldRepository>>,
) {
    let persisted = db.and_then(|repo| {
        repo.with_snapshot(|snap| {
            snap.service_state_blobs
                .get(MISC_STATE_BLOB_KEY)
                .and_then(|json| serde_json::from_str::<PersistedClusterMiscState>(json).ok())
        })
    });
    let launchers = persisted
        .as_ref()
        .filter(|p| !p.launchers.is_empty())
        .map(|p| p.launchers.clone())
        .unwrap_or_else(derived_launchers);
    let starter_launchers = persisted
        .as_ref()
        .filter(|p| !p.starter_launchers.is_empty())
        .map(|p| p.starter_launchers.clone())
        .unwrap_or_else(derived_starter_launchers);
    let pads = persisted
        .filter(|p| !p.landing_pads.is_empty())
        .map(|p| p.landing_pads)
        .unwrap_or_else(derived_landing_pads);
    for l in launchers {
        if l.solar_system_id == solar_system_id {
            ballpark.add_static_ball_with_meta(
                eve_destiny::Ball { id: l.item_id, type_id: l.type_id, position: l.position, velocity: glam::DVec3::ZERO, max_velocity: 0.0, inertia: 0.0, mass: 1.0e10, radius: 20_000.0, speed_fraction: 0.0, mode: eve_destiny::MovementMode::Rigid },
                eve_destiny::SlimItemMeta { group_id: 2017, category_id: 65, owner_id: l.faction_id as i64, char_id: None, corp_id: 0, alliance_id: -1, security_status: 0.0, is_cloaked: false }
            );
        }
    }
    for sl in starter_launchers {
        if sl.solar_system_id == solar_system_id {
            ballpark.add_static_ball_with_meta(
                eve_destiny::Ball { id: sl.item_id, type_id: sl.type_id, position: sl.position, velocity: glam::DVec3::ZERO, max_velocity: 0.0, inertia: 0.0, mass: 1.0e10, radius: 20_000.0, speed_fraction: 0.0, mode: eve_destiny::MovementMode::Rigid },
                eve_destiny::SlimItemMeta { group_id: 2017, category_id: 65, owner_id: sl.faction_id as i64, char_id: None, corp_id: 0, alliance_id: -1, security_status: 0.0, is_cloaked: false }
            );
        }
    }
    for p in pads {
        if p.solar_system_id == solar_system_id {
            ballpark.add_static_ball_with_meta(
                eve_destiny::Ball { id: p.item_id, type_id: p.type_id, position: p.position, velocity: glam::DVec3::ZERO, max_velocity: 0.0, inertia: 0.0, mass: 1.0e10, radius: 20_000.0, speed_fraction: 0.0, mode: eve_destiny::MovementMode::Rigid },
                eve_destiny::SlimItemMeta { group_id: 2016, category_id: 65, owner_id: p.faction_id as i64, char_id: None, corp_id: 0, alliance_id: -1, security_status: 0.0, is_cloaked: false }
            );
        }
    }
}

pub fn get_or_create_offered_load_slot(
    ctx: &ServerContext,
    char_id: i64,
    current_sys: u32,
) -> Option<(String, u32)> {
    let cap = with_misc_state(ctx, |st| st.system_population_caps.get(&current_sys).copied().unwrap_or(usize::MAX));
    
    let chars_in_sys: Vec<i64> = ctx.db.with_snapshot(|snap| {
        snap.characters.values()
            .filter(|c| c.solar_system_id == current_sys)
            .map(|c| c.character_id)
            .collect()
    });
    let online_count = chars_in_sys.into_iter()
        .filter(|&cid| crate::services::character_contacts::is_character_online(ctx, cid))
        .count();

    if online_count >= cap {
        let mut adj_systems: Vec<u32> = ctx.sde.jumps.values()
            .filter(|j| j.from_solar_system_id == current_sys)
            .map(|j| j.to_solar_system_id)
            .collect();
        if adj_systems.is_empty() {
            return None;
        }
        adj_systems.sort_by_key(|&sys| {
            let sys_chars: Vec<i64> = ctx.db.with_snapshot(|snap| {
                snap.characters.values()
                    .filter(|c| c.solar_system_id == sys)
                    .map(|c| c.character_id)
                    .collect()
            });
            sys_chars.into_iter()
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
        // --------------------------------------------------------------------
        // charMgr: Client UI settings persistence
        // --------------------------------------------------------------------
        ("charMgr", "SaveCharacterSetting") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(key) = args.first().and_then(|v| v.as_str()).filter(|s| !s.is_empty()) else {
                return Some(Err(rpc_err("CustomError", "SaveCharacterSetting requires a non-empty setting key")));
            };
            if key.len() > MAX_SETTING_KEY_LEN {
                return Some(Err(rpc_err("CustomError", "Setting key exceeds maximum length")));
            }
            let val = args.get(1).cloned().unwrap_or(PyRep::Bool(true));
            let serialized_len = serde_json::to_string(&val).map(|s| s.len()).unwrap_or(usize::MAX);
            if serialized_len > MAX_SETTING_VAL_SERIALIZED_BYTES {
                return Some(Err(rpc_err("CustomError", "Setting value exceeds maximum allowed size")));
            }

            let insert_res = with_misc_state(ctx, |st| {
                let user_settings = st.char_settings.entry(char_id).or_default();
                if !user_settings.contains_key(key) && user_settings.len() >= MAX_SETTINGS_PER_CHAR {
                    return Err(rpc_err("CustomError", "Character settings limit reached (maximum 500 keys)"));
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
                return Some(Err(rpc_err("CustomError", "DeleteCharacterSetting requires a non-empty setting key")));
            };
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
        _ => crate::services::shipcaster_ops::handle(ctx, session, service, method, args, _kwargs).await,
    }
}
