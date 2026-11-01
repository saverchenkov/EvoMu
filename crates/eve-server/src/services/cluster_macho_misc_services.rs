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


/// Six-minute (`360s`) rotation interval when an empire operates > 1 active Shipcaster Beacon (`Shipcaster.md:15`).
pub const SHIPCASTER_ROTATION_INTERVAL_SECS: u64 = 360;

/// Maximum simultaneous active Shipcaster Beacons per empire faction (`0..=3`, `Shipcaster.md:15`).
pub const MAX_ACTIVE_BEACONS_PER_FACTION: usize = 3;

/// DERIVED: Canonical Faction Warfare Landing Pad (Shipcaster Beacon) record.
/// Traced to authentic SDE type IDs 75928..=75931 (group 4547) and pirate FOBs 78260 / 79172 (group 4644).
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
        let can_be_linked = self.is_built && !self.is_disrupted;
        let is_linked = can_be_linked && self.link_timestamp > 0;
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

/// Evaluates effective link state for a landing pad at `now_unix`.
///
/// Per `Shipcaster.md:15`, each empire (`500001..=500004`) may operate `0..=3` active Shipcaster
/// Beacons simultaneously, and when `> 1` beacon is active (`isBuilt && !isDisrupted`), the
/// Interstellar Shipcaster cycles its active link every 6 minutes (`360s`).
pub fn effective_landing_pad(
    pad: &LandingPadRecord,
    all_pads: &[LandingPadRecord],
    now_unix: u64,
) -> LandingPadRecord {
    let mut out = pad.clone();
    if !matches!(pad.faction_id, 500001..=500004) {
        return out;
    }
    let mut linkable: Vec<&LandingPadRecord> = all_pads
        .iter()
        .filter(|p| p.faction_id == pad.faction_id && p.is_built && !p.is_disrupted)
        .collect();
    linkable.sort_by_key(|p| p.item_id);
    linkable.truncate(MAX_ACTIVE_BEACONS_PER_FACTION);

    if linkable.is_empty() {
        out.link_timestamp = 0;
    } else if linkable.len() == 1 {
        if pad.item_id == linkable[0].item_id {
            out.link_timestamp = pad.link_timestamp.max(1);
        } else {
            out.link_timestamp = 0;
        }
    } else {
        let slot = ((now_unix / SHIPCASTER_ROTATION_INTERVAL_SECS) as usize) % linkable.len();
        if pad.item_id == linkable[slot].item_id {
            let window_start = ((now_unix / SHIPCASTER_ROTATION_INTERVAL_SECS)
                * SHIPCASTER_ROTATION_INTERVAL_SECS)
                .max(1) as i64;
            out.link_timestamp = window_start;
        } else {
            out.link_timestamp = 0;
        }
    }
    out
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

/// Pending two-step Zarzakh stargate transit toll confirmation quote (`feat:navigation.zarzakh-gate-toll`).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PendingZarzakhTollQuote {
    pub from_gate_id: i64,
    pub to_gate_id: i64,
    pub ship_id: i64,
    pub quoted_toll_cents: eve_db::IskCents,
    pub expires_unix: u64,
}

/// DERIVED: Zarzakh Horizon Siege state machine timing constants (`feat:navigation.zarzakh-local-horizon-siege`,
/// `Zarzakh.md:39`, `auditor_rows.json:6968-6974`).
pub const HORIZON_SIEGE_WARNING_SECS: u64 = 300;
pub const HORIZON_SIEGE_ACTIVE_SECS: u64 = 3_600;
pub const HORIZON_SIEGE_COOLDOWN_SECS: u64 = 7_200;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum HorizonSiegePhase {
    #[default]
    Normal,
    Warning,
    Active,
    Stabilizing,
}

impl HorizonSiegePhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "Normal",
            Self::Warning => "Warning",
            Self::Active => "Active",
            Self::Stabilizing => "Stabilizing",
        }
    }
}

/// Persisted Zarzakh Horizon Siege state machine (`feat:navigation.zarzakh-local-horizon-siege`,
/// `feat:navigation.deathless-shipcaster-zarzakh`).
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct ZarzakhHorizonSiegeState {
    pub phase: HorizonSiegePhase,
    pub phase_started_unix: u64,
    pub active_start_unix: u64,
    pub active_until_unix: u64,
    pub cooldown_until_unix: u64,
    pub instability_points: u32,
    pub triggered_by_campaign_id: Option<i64>,
    pub fulcrum_restricted_to_pirates: bool,
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
    #[serde(default)]
    target_cooldowns: HashMap<i64, u64>,
    #[serde(default)]
    target_charges: HashMap<i64, u32>,
    #[serde(default)]
    zarzakh_toll_quotes: HashMap<i64, PendingZarzakhTollQuote>,
    #[serde(default)]
    zarzakh_horizon_siege: ZarzakhHorizonSiegeState,
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
    pub target_cooldowns: HashMap<i64, u64>,
    pub target_charges: HashMap<i64, u32>,
    pub zarzakh_toll_quotes: HashMap<i64, PendingZarzakhTollQuote>,
    pub zarzakh_horizon_siege: ZarzakhHorizonSiegeState,
}

/// DERIVED: Canonical Empire Landing Pads (Shipcaster Beacons) in Faction Warfare warzones.
/// Each empire supports `0..=3` active beacons cycling every 6 minutes (`360s`, `Shipcaster.md:15`).
pub fn derived_landing_pads() -> Vec<LandingPadRecord> {
    vec![
        // Caldari State (`500001`): Onatoh (`30002811`) & Tama (`30002813`)
        LandingPadRecord {
            item_id: 104_000_001,
            type_id: 75929, // State Shipcaster Beacon
            solar_system_id: 30002811, // Onatoh / Black Rise FW warzone
            faction_id: 500001,
            is_built: true,
            link_timestamp: 1_700_000_000,
            is_disrupted: false,
            position: glam::DVec3::new(100_000.0, 0.0, 0.0),
        },
        LandingPadRecord {
            item_id: 104_000_005,
            type_id: 75929, // State Shipcaster Beacon
            solar_system_id: 30002813, // Tama / Black Rise FW warzone
            faction_id: 500001,
            is_built: true,
            link_timestamp: 1_700_000_000,
            is_disrupted: false,
            position: glam::DVec3::new(100_000.0, 0.0, 0.0),
        },
        // Minmatar Republic (`500002`): Resbroko (`30002056`) & Amamake (`30002537`)
        LandingPadRecord {
            item_id: 104_000_002,
            type_id: 75931, // Republic Shipcaster Beacon
            solar_system_id: 30002056, // Resbroko / Metropolis FW warzone
            faction_id: 500002,
            is_built: true,
            link_timestamp: 1_700_000_000,
            is_disrupted: false,
            position: glam::DVec3::new(100_000.0, 0.0, 0.0),
        },
        LandingPadRecord {
            item_id: 104_000_006,
            type_id: 75931, // Republic Shipcaster Beacon
            solar_system_id: 30002537, // Amamake / Heimatar FW warzone
            faction_id: 500002,
            is_built: true,
            link_timestamp: 1_700_000_000,
            is_disrupted: false,
            position: glam::DVec3::new(100_000.0, 0.0, 0.0),
        },
        // Amarr Empire (`500003`): NI-J0B (`30002185`) & Huola (`30003067`)
        LandingPadRecord {
            item_id: 104_000_003,
            type_id: 75928, // Imperial Shipcaster Beacon
            solar_system_id: 30002185, // NI-J0B / Bleak Lands FW warzone
            faction_id: 500003,
            is_built: true,
            link_timestamp: 1_700_000_000,
            is_disrupted: false,
            position: glam::DVec3::new(100_000.0, 0.0, 0.0),
        },
        LandingPadRecord {
            item_id: 104_000_007,
            type_id: 75928, // Imperial Shipcaster Beacon
            solar_system_id: 30003067, // Huola / Bleak Lands FW warzone
            faction_id: 500003,
            is_built: true,
            link_timestamp: 1_700_000_000,
            is_disrupted: false,
            position: glam::DVec3::new(100_000.0, 0.0, 0.0),
        },
        // Gallente Federation (`500004`): Mantenault (`30003855`) & Oicx (`30003838`)
        LandingPadRecord {
            item_id: 104_000_004,
            type_id: 75930, // Federation Shipcaster Beacon
            solar_system_id: 30003855, // Mantenault / Essence FW warzone
            faction_id: 500004,
            is_built: true,
            link_timestamp: 1_700_000_000,
            is_disrupted: false,
            position: glam::DVec3::new(100_000.0, 0.0, 0.0),
        },
        LandingPadRecord {
            item_id: 104_000_008,
            type_id: 75930, // Federation Shipcaster Beacon
            solar_system_id: 30003838, // Oicx / Placid FW warzone
            faction_id: 500004,
            is_built: true,
            link_timestamp: 1_700_000_000,
            is_disrupted: false,
            position: glam::DVec3::new(100_000.0, 0.0, 0.0),
        },
        // Insurgency Forward Operating Base Landing Pads
        LandingPadRecord {
            item_id: 104_000_020,
            type_id: 78260, // Angel Cartel FOB (group 4644)
            solar_system_id: 30002056, // Angel Cartel Insurgency FOB
            faction_id: 500010,
            is_built: true,
            link_timestamp: 1_700_000_000,
            is_disrupted: false,
            position: glam::DVec3::new(120_000.0, 0.0, 0.0),
        },
        LandingPadRecord {
            item_id: 104_000_021,
            type_id: 79172, // Guristas Pirates FOB (group 4644)
            solar_system_id: 30002811, // Guristas Insurgency FOB
            faction_id: 500011,
            is_built: true,
            link_timestamp: 1_700_000_000,
            is_disrupted: false,
            position: glam::DVec3::new(120_000.0, 0.0, 0.0),
        },
        // DERIVED: Zarzakh (`30_100_000`) and SDE type `79018` (`Deathless Interstellar Shipcaster`)
        // are authoritative SDE IDs; landing-pad item ID `104_000_030` and coordinates are server-side placements (`feat:navigation.deathless-shipcaster-zarzakh`).
        LandingPadRecord {
            item_id: 104_000_030,
            type_id: 79018, // Deathless Interstellar Shipcaster
            solar_system_id: 30_100_000, // Zarzakh
            faction_id: 0,
            is_built: true,
            link_timestamp: 1_700_000_000,
            is_disrupted: false,
            position: glam::DVec3::new(250_000.0, 0.0, 0.0),
        },
    ]
}

/// Canonical Interstellar Shipcaster Launchers in Empire FW HQ systems (`Shipcaster.md:20-23, 30`).
pub fn derived_launchers() -> Vec<LauncherRecord> {
    vec![
        LauncherRecord {
            item_id: 105_000_001,
            type_id: 76466, // State Interstellar Shipcaster
            solar_system_id: 30045324, // Onnamon (Caldari FW HQ)
            faction_id: 500001,
            linked_pad_ids: vec![104_000_001, 104_000_005],
            position: glam::DVec3::new(200_000.0, 0.0, 0.0),
        },
        LauncherRecord {
            item_id: 105_000_002,
            type_id: 76468, // Republic Interstellar Shipcaster
            solar_system_id: 30002055, // Amo (Minmatar FW HQ)
            faction_id: 500002,
            linked_pad_ids: vec![104_000_002, 104_000_006],
            position: glam::DVec3::new(200_000.0, 0.0, 0.0),
        },
        LauncherRecord {
            item_id: 105_000_003,
            type_id: 76465, // Imperial Interstellar Shipcaster
            solar_system_id: 30002974, // Mehatoor (Amarr FW HQ)
            faction_id: 500003,
            linked_pad_ids: vec![104_000_003, 104_000_007],
            position: glam::DVec3::new(200_000.0, 0.0, 0.0),
        },
        LauncherRecord {
            item_id: 105_000_004,
            type_id: 76467, // Federation Interstellar Shipcaster
            solar_system_id: 30003788, // Intaki (Gallente FW HQ)
            faction_id: 500004,
            linked_pad_ids: vec![104_000_004, 104_000_008],
            position: glam::DVec3::new(200_000.0, 0.0, 0.0),
        },
        // Deathless Shipcaster in Zarzakh (links to Angel and Guristas pirate FOBs)
        LauncherRecord {
            item_id: 105_000_010,
            type_id: 79018, // Deathless Interstellar Shipcaster
            solar_system_id: 30100000, // Zarzakh Deathless Shipcaster
            faction_id: 0,
            linked_pad_ids: vec![104_000_020, 104_000_021],
            position: glam::DVec3::new(250_000.0, 0.0, 0.0),
        },
        // DERIVED: Turnur (`30002086`), Vard (`30002538`), Alsavoinon (`30003841`), Hikkoken (`30045338`)
        // and SDE type `79018` are authoritative SDE IDs; beacon item IDs `105_000_011..=105_000_014` and positions are server-side placements (`feat:navigation.deathless-shipcaster-zarzakh`).
        LauncherRecord {
            item_id: 105_000_011,
            type_id: 79018,
            solar_system_id: 30002086, // Turnur
            faction_id: 0,
            linked_pad_ids: vec![104_000_030],
            position: glam::DVec3::new(250_000.0, 0.0, 0.0),
        },
        LauncherRecord {
            item_id: 105_000_012,
            type_id: 79018,
            solar_system_id: 30002538, // Vard
            faction_id: 0,
            linked_pad_ids: vec![104_000_030],
            position: glam::DVec3::new(250_000.0, 0.0, 0.0),
        },
        LauncherRecord {
            item_id: 105_000_013,
            type_id: 79018,
            solar_system_id: 30003841, // Alsavoinon
            faction_id: 0,
            linked_pad_ids: vec![104_000_030],
            position: glam::DVec3::new(250_000.0, 0.0, 0.0),
        },
        LauncherRecord {
            item_id: 105_000_014,
            type_id: 79018,
            solar_system_id: 30045338, // Hikkoken
            faction_id: 0,
            linked_pad_ids: vec![104_000_030],
            position: glam::DVec3::new(250_000.0, 0.0, 0.0),
        },
    ]
}

fn starter_shipcaster_spec_for_race(race_id: u32) -> Option<(i32, u32, u32)> {
    match race_id {
        1 => Some((500001, 93980, 30045324)), // Caldari -> State Rookie Shipcaster -> Onnamon
        2 => Some((500002, 94018, 30002055)), // Minmatar -> Republic Rookie Shipcaster -> Amo
        4 => Some((500003, 93968, 30002974)), // Amarr -> Imperial Rookie Shipcaster -> Mehatoor
        8 => Some((500004, 93984, 30003788)), // Gallente -> Federation Rookie Shipcaster -> Intaki
        _ => None,
    }
}

/// Derives Rookie Starter Shipcasters (`93968`, `93980`, `93984`, `94018`) across all 12 empire
/// starter school systems (`school_id` `11..=22`) directly from [`eve_sde::SdeRegistry::schools`].
pub fn starter_launchers_from_sde(sde: &eve_sde::SdeRegistry) -> Vec<StarterLauncherRecord> {
    let mut school_ids: Vec<u32> = (11..=22).filter(|id| sde.schools.contains_key(id)).collect();
    school_ids.sort_unstable();
    let mut out = Vec::with_capacity(school_ids.len());
    for school_id in school_ids {
        let Some(school) = sde.schools.get(&school_id) else {
            continue;
        };
        let Some((faction_id, type_id, staging_system_id)) =
            starter_shipcaster_spec_for_race(school.race_id)
        else {
            continue;
        };
        out.push(StarterLauncherRecord {
            item_id: 105_000_100 + (school_id as i64),
            type_id,
            solar_system_id: school.solar_system_id,
            faction_id,
            staging_system_id,
            position: glam::DVec3::new(300_000.0, 0.0, 0.0),
        });
    }
    if out.is_empty() {
        derived_starter_launchers()
    } else {
        out
    }
}

/// Canonical Rookie Starter Shipcasters across all 12 empire starter school systems (`sde_schools` `11..=22`),
/// jumping to each empire's FW Interstellar Shipcaster HQ (`Onnamon`, `Amo`, `Mehatoor`, `Intaki`).
pub fn derived_starter_launchers() -> Vec<StarterLauncherRecord> {
    let schools: [(u32, u32, u32); 12] = [
        (11, 4, 30003489), // Chaven (Imperial Academy) -> Mehatoor
        (12, 4, 30005042), // Sehmy (Hedion University) -> Mehatoor
        (13, 4, 30001672), // Emrayur (Royal Amarr Institute) -> Mehatoor
        (14, 2, 30002547), // Ammold (Republic Military School) -> Amo
        (15, 2, 30002505), // Hulm (Republic University) -> Amo
        (16, 2, 30003410), // Ryddinjorn (Pator Tech School) -> Amo
        (17, 1, 30000141), // Kisogo (State War Academy) -> Onnamon
        (18, 1, 30001392), // Amsen (Science & Trade Institute) -> Onnamon
        (19, 1, 30001407), // Todaki (School of Applied Knowledge) -> Onnamon
        (20, 8, 30004971), // Duripant (Federal Navy Academy) -> Intaki
        (21, 8, 30002715), // Bourynes (University of Caille) -> Intaki
        (22, 8, 30005305), // Cistuvaert (Center for Advanced Studies) -> Intaki
    ];
    let mut out = Vec::with_capacity(schools.len());
    for (school_id, race_id, solar_system_id) in schools {
        if let Some((faction_id, type_id, staging_system_id)) =
            starter_shipcaster_spec_for_race(race_id)
        {
            out.push(StarterLauncherRecord {
                item_id: 105_000_100 + (school_id as i64),
                type_id,
                solar_system_id,
                faction_id,
                staging_system_id,
                position: glam::DVec3::new(300_000.0, 0.0, 0.0),
            });
        }
    }
    out
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
            target_cooldowns: HashMap::new(),
            target_charges: HashMap::new(),
            zarzakh_toll_quotes: HashMap::new(),
            zarzakh_horizon_siege: ZarzakhHorizonSiegeState::default(),
        };
        let persisted: Option<PersistedClusterMiscState> = db.with_snapshot(|snap| {
            snap.service_state_blobs
                .get(MISC_STATE_BLOB_KEY)
                .and_then(|json| serde_json::from_str(json).ok())
        });
        if let Some(p) = persisted {
            st.char_settings = p.char_settings;
            if !p.landing_pads.is_empty() {
                let mut merged = p.landing_pads;
                for canon in derived_landing_pads() {
                    if !merged.iter().any(|x| x.item_id == canon.item_id) {
                        merged.push(canon);
                    }
                }
                st.landing_pads = merged;
            }
            if !p.launchers.is_empty() {
                let mut merged = p.launchers;
                for canon in derived_launchers() {
                    if !merged.iter().any(|x| x.item_id == canon.item_id) {
                        merged.push(canon);
                    }
                }
                st.launchers = merged;
            }
            if !p.starter_launchers.is_empty() {
                let mut merged = p.starter_launchers;
                for canon in derived_starter_launchers() {
                    if !merged.iter().any(|x| x.item_id == canon.item_id) {
                        merged.push(canon);
                    }
                }
                st.starter_launchers = merged;
            }
            st.offered_load_slots = p.offered_load_slots;
            st.char_energy = p.char_energy;
            st.target_cooldowns = p.target_cooldowns;
            st.target_charges = p.target_charges;
            st.zarzakh_toll_quotes = p.zarzakh_toll_quotes;
            st.zarzakh_horizon_siege = p.zarzakh_horizon_siege;
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
        target_cooldowns: st.target_cooldowns.clone(),
        target_charges: st.target_charges.clone(),
        zarzakh_toll_quotes: st.zarzakh_toll_quotes.clone(),
        zarzakh_horizon_siege: st.zarzakh_horizon_siege.clone(),
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
        .map(|p| {
            let mut merged = p.launchers.clone();
            for canon in derived_launchers() {
                if !merged.iter().any(|x| x.item_id == canon.item_id) {
                    merged.push(canon);
                }
            }
            merged
        })
        .unwrap_or_else(derived_launchers);
    let starter_launchers = persisted
        .as_ref()
        .filter(|p| !p.starter_launchers.is_empty())
        .map(|p| {
            let mut merged = p.starter_launchers.clone();
            for canon in derived_starter_launchers() {
                if !merged.iter().any(|x| x.item_id == canon.item_id) {
                    merged.push(canon);
                }
            }
            merged
        })
        .unwrap_or_else(derived_starter_launchers);
    let pads = persisted
        .filter(|p| !p.landing_pads.is_empty())
        .map(|p| {
            let mut merged = p.landing_pads;
            for canon in derived_landing_pads() {
                if !merged.iter().any(|x| x.item_id == canon.item_id) {
                    merged.push(canon);
                }
            }
            merged
        })
        .unwrap_or_else(derived_landing_pads);
    for l in launchers {
        if l.solar_system_id == solar_system_id {
            ballpark.add_static_ball_with_meta(
                eve_destiny::Ball { id: l.item_id, type_id: l.type_id, position: l.position, velocity: glam::DVec3::ZERO, max_velocity: 0.0, inertia: 0.0, mass: 1.0e10, radius: 20_000.0, speed_fraction: 0.0, mode: eve_destiny::MovementMode::Rigid },
                eve_destiny::SlimItemMeta { group_id: 4549, category_id: 65, owner_id: l.faction_id as i64, char_id: None, corp_id: 0, alliance_id: -1, security_status: 0.0, is_cloaked: false }
            );
        }
    }
    for sl in starter_launchers {
        if sl.solar_system_id == solar_system_id {
            ballpark.add_static_ball_with_meta(
                eve_destiny::Ball { id: sl.item_id, type_id: sl.type_id, position: sl.position, velocity: glam::DVec3::ZERO, max_velocity: 0.0, inertia: 0.0, mass: 1.0e10, radius: 20_000.0, speed_fraction: 0.0, mode: eve_destiny::MovementMode::Rigid },
                eve_destiny::SlimItemMeta { group_id: 4549, category_id: 65, owner_id: sl.faction_id as i64, char_id: None, corp_id: 0, alliance_id: -1, security_status: 0.0, is_cloaked: false }
            );
        }
    }
    for p in pads {
        if p.solar_system_id == solar_system_id {
            let group_id = if matches!(p.type_id, 78260 | 79172) { 4644 } else { 4547 };
            ballpark.add_static_ball_with_meta(
                eve_destiny::Ball { id: p.item_id, type_id: p.type_id, position: p.position, velocity: glam::DVec3::ZERO, max_velocity: 0.0, inertia: 0.0, mass: 1.0e10, radius: 20_000.0, speed_fraction: 0.0, mode: eve_destiny::MovementMode::Rigid },
                eve_destiny::SlimItemMeta { group_id, category_id: 65, owner_id: p.faction_id as i64, char_id: None, corp_id: 0, alliance_id: -1, security_status: 0.0, is_cloaked: false }
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
        _ => {
            if let Some(res) = crate::services::notify_emitters_batch9_part2::handle_misc_batch9(
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
                    message: format!("Shipcaster destination pad {pad_id} is on cooldown for {} more seconds", expires - now),
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

