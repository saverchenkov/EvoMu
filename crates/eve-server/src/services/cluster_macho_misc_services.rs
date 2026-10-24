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
use crate::system_actor::SystemHandle;
use eve_db::{OwnerNoteRecord, WorldRepository};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

static LATEST_MISC_STATE: std::sync::OnceLock<
    std::sync::RwLock<(
        Vec<LauncherRecord>,
        Vec<StarterLauncherRecord>,
        Vec<LandingPadRecord>,
    )>,
> = std::sync::OnceLock::new();

fn update_latest_misc_state(launchers: &[LauncherRecord], starter: &[StarterLauncherRecord], pads: &[LandingPadRecord]) {
    let lock = LATEST_MISC_STATE.get_or_init(|| std::sync::RwLock::new((launchers.to_vec(), starter.to_vec(), pads.to_vec())));
    if let Ok(mut g) = lock.write() {
        *g = (launchers.to_vec(), starter.to_vec(), pads.to_vec());
    }
}
const MISC_STATE_NOTE_ID: i64 = -88_000_001;
const MISC_STATE_OWNER_ID: i64 = -88;

/// Per-character limits to prevent memory and storage exhaustion.
const MAX_SETTINGS_PER_CHAR: usize = 500;
const MAX_SETTING_KEY_LEN: usize = 128;
const MAX_SETTING_VAL_SERIALIZED_BYTES: usize = 4_096;

/// Maximum operational range to a shipcaster launcher for jumping (5 km).
const SHIPCASTER_MAX_RANGE_M: f64 = 5_000.0;

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
}

#[derive(Default)]
pub struct ClusterMiscRuntimeState {
    pub char_settings: HashMap<i64, BTreeMap<String, PyRep>>,
    pub landing_pads: Vec<LandingPadRecord>,
    pub launchers: Vec<LauncherRecord>,
    pub starter_launchers: Vec<StarterLauncherRecord>,
    pub offered_load_slots: HashMap<i64, OfferedLoadSlot>,
    pub system_population_caps: HashMap<u32, usize>,
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
        };
        let persisted: Option<PersistedClusterMiscState> = db.with_snapshot(|snap| {
            snap.owner_notes
                .get(&MISC_STATE_NOTE_ID)
                .and_then(|n| serde_json::from_str(&n.note).ok())
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
        }
        update_latest_misc_state(&st.launchers, &st.starter_launchers, &st.landing_pads);
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
    let res = f(&mut guard);
    update_latest_misc_state(&guard.launchers, &guard.starter_launchers, &guard.landing_pads);
    res
}

/// Persists the character settings and cluster travel state to SQLite via owner notes sentinel record.
pub(crate) fn persist_misc_state(ctx: &ServerContext) {
    let p = with_misc_state(ctx, |st| PersistedClusterMiscState {
        char_settings: st.char_settings.clone(),
        landing_pads: st.landing_pads.clone(),
        launchers: st.launchers.clone(),
        starter_launchers: st.starter_launchers.clone(),
        offered_load_slots: st.offered_load_slots.clone(),
    });
    if let Ok(json) = serde_json::to_string(&p) {
        let _ = ctx.db.try_with_mut_snapshot(|snap| {
            snap.owner_notes.insert(
                MISC_STATE_NOTE_ID,
                OwnerNoteRecord {
                    note_id: MISC_STATE_NOTE_ID,
                    character_id: MISC_STATE_OWNER_ID,
                    item_id: 0,
                    label: "__cluster_misc_state__".into(),
                    note: json,
                    timestamp_unix: 0,
                },
            );
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
    let (launchers, starter_launchers, pads) = if let Some(lock) = LATEST_MISC_STATE.get() {
        if let Ok(g) = lock.read() {
            g.clone()
        } else {
            (derived_launchers(), derived_starter_launchers(), derived_landing_pads())
        }
    } else {
        (derived_launchers(), derived_starter_launchers(), derived_landing_pads())
    };
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

/// Enforces fail-closed operational range and ball presence in the active solar system actor.
async fn check_proximity(
    sys: &SystemHandle,
    ship_id: i64,
    target_id: i64,
    max_dist: f64,
    label: &str,
) -> Result<(), BridgeRpcError> {
    if target_id <= 0 {
        return Err(rpc_err(
            "TargetNotFound",
            format!("Invalid target ID {target_id} for {label}"),
        ));
    }
    let ship_ball = sys
        .get_ball_snapshot(ship_id)
        .await
        .map_err(|e| rpc_err("DestinyQueryFailed", e))?
        .map(|(_, b)| b)
        .ok_or_else(|| rpc_err("ShipNotInSpace", "Active ship ball is not in space in this solar system"))?;

    let target_ball = sys
        .get_ball_snapshot(target_id)
        .await
        .map_err(|e| rpc_err("DestinyQueryFailed", e))?
        .map(|(_, b)| b)
        .ok_or_else(|| {
            rpc_err(
                "TargetNotInSpace",
                format!("{label} entity {target_id} is not present in space in this solar system"),
            )
        })?;

    let dist = ship_ball.position.distance(target_ball.position);
    if dist > max_dist {
        return Err(rpc_err(
            "OutsideOperationalRange",
            format!("Ship is {dist:.1}m from {label} (must be <= {max_dist:.0}m)"),
        ));
    }
    Ok(())
}

/// Executes jump handoff across system actors, moving the ball and emitting complete session changes.
async fn execute_shipcaster_jump(
    ctx: &ServerContext,
    session: &mut SessionState,
    char_id: i64,
    ship_id: i64,
    origin_sys: u32,
    dest_sys: u32,
    arrival_pos: glam::DVec3,
) -> Result<(), BridgeRpcError> {
    let now_unix = ctx.now_unix();
    let old_handle = ctx.systems.get_or_spawn(origin_sys);
    let Some((cur_tick, ship_ball)) = old_handle
        .get_ball_snapshot(ship_id)
        .await
        .map_err(|e| rpc_err("DestinyQueryFailed", e))?
    else {
        return Err(rpc_err("ShipNotInSpace", "Active ship ball not in space in origin solar system"));
    };

    if let Some(active_timer) =
        crate::services::space_destiny::check_session_change_timer_active(ctx, char_id, now_unix, Some((origin_sys, cur_tick)))
    {
        return Err(rpc_err(
            "SessionChangeInProgress",
            format!("Cannot jump shipcaster while 10s session-change timer ({}) is active", active_timer.reason),
        ));
    }

    if let Err(e) = crate::services::tactical_modules::check_ship_navigation_lock(ctx, ship_id, "jump") {
        return Err(e);
    }

    if matches!(ship_ball.mode, eve_destiny::MovementMode::Warp { .. }) {
        return Err(rpc_err("ShipInWarp", "Cannot jump shipcaster while ship is mid-warp"));
    }

    let is_scrambled = old_handle.is_ball_scrambled(ship_id).await.unwrap_or(false);
    if is_scrambled {
        return Err(rpc_err("WarpScrambled", "Cannot jump shipcaster while warp scrambled or disrupted"));
    }

    if let Ok(Some(cs)) = old_handle.get_combat_state(ship_id).await {
        if cs.warp_scramble_strength > 0 {
            return Err(rpc_err("WarpScrambled", "Cannot jump shipcaster while warp scrambled"));
        }
    }

    let updated_ch = ctx
        .db
        .set_character_solar_system(char_id, dest_sys, &ctx.sde)
        .map_err(|e| rpc_err("SystemTransitionFailed", e.to_string()))?;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.location_id = dest_sys as i64;
        }
    });

    crate::services::space_destiny::space_warp_jump::deactivate_ship_modules_on_leave_space(ctx, char_id, ship_id);
    let existing_ball = old_handle.remove_ball(ship_id).await;
    ctx.sync_character_local_chat(char_id, Some(origin_sys), dest_sys);

    let new_handle = ctx.systems.get_or_spawn(dest_sys);
    let mut new_ball = existing_ball.unwrap_or(ship_ball);
    new_ball.position = arrival_pos;
    new_ball.velocity = glam::DVec3::ZERO;
    new_ball.speed_fraction = 0.0;
    new_ball.mode = eve_destiny::MovementMode::Stop;

    let mut combat_state = ctx.build_ship_combat_state(char_id, ship_id, new_ball.type_id);
    combat_state.is_cloaked = true;
    combat_state.gate_cloak_remaining_ticks = 60;
    combat_state.undock_invulnerable_remaining_ticks = 60;
    combat_state.sync_invuln_with_undock_timer = true;
    let mut slim_meta = ctx.build_ship_slim_meta(char_id, new_ball.type_id);
    slim_meta.is_cloaked = true;

    let new_tick = new_handle
        .spawn_ball_with_meta(new_ball, combat_state, slim_meta)
        .await
        .unwrap_or(0);

    crate::services::space_destiny::record_session_change_timer(
        ctx,
        char_id,
        now_unix,
        dest_sys,
        new_tick,
        "shipcaster",
    );

    session.solar_system_id = Some(dest_sys);
    session.constellation_id = Some(updated_ch.constellation_id);
    session.region_id = Some(updated_ch.region_id);
    session.station_id = None;
    session.structure_id = None;

    let _ = new_handle.push_set_state(ship_id).await;

    Ok(())
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
        // shipCasterLandingPadMgr: Faction Warfare Landing Pad Registry
        // --------------------------------------------------------------------
        ("shipCasterLandingPadMgr", "GetAllLandingPads") => {
            let pads = with_misc_state(ctx, |st| st.landing_pads.clone());
            Some(Ok(PyRep::List(pads.iter().map(|p| p.to_keyval()).collect())))
        }

        ("shipCasterLandingPadMgr", "GetFactionLandingPads") => {
            let faction_id = match args.first().and_then(|a| a.as_i64()) {
                Some(fid) => i32::try_from(fid).unwrap_or(0),
                None => return Some(Err(rpc_err("InvalidArguments", "GetFactionLandingPads requires factionID"))),
            };
            let pads = with_misc_state(ctx, |st| {
                st.landing_pads
                    .iter()
                    .filter(|p| p.faction_id == faction_id)
                    .map(|p| p.to_keyval())
                    .collect::<Vec<_>>()
            });
            Some(Ok(PyRep::List(pads)))
        }

        // --------------------------------------------------------------------
        // shipCasterLauncherMgr: Interstellar Shipcaster Launchers
        // --------------------------------------------------------------------
        ("shipCasterLauncherMgr", "GetFactionsWithShipcaster") => {
            let factions = with_misc_state(ctx, |st| {
                let mut set = std::collections::BTreeSet::new();
                for l in &st.launchers {
                    set.insert(l.faction_id);
                }
                set.into_iter().map(|f| PyRep::Integer(f as i64)).collect::<Vec<_>>()
            });
            Some(Ok(PyRep::List(factions)))
        }

        // --------------------------------------------------------------------
        // starterShipcasterTravelMgr: Rookie Starter Shipcaster Travel
        // --------------------------------------------------------------------
        ("starterShipcasterTravelMgr", "CanCharacterJump") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let now = ctx.now_unix();
            let has_timer = {
                let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
                cw.get_or_create(char_id).is_weapon_timer_active(now)
            };
            Some(Ok(PyRep::Bool(!has_timer)))
        }

        ("starterShipcasterTravelMgr", "CmdJumpThroughStarterShipcaster") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if session.station_id.is_some() || session.structure_id.is_some() {
                return Some(Err(rpc_err("CantDoThatWhileDocked", "Cannot jump shipcaster while docked")));
            }
            let Some(ship_id) = session.ship_id else {
                return Some(Err(rpc_err("CustomError", "CmdJumpThroughStarterShipcaster requires an active ship")));
            };
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(rpc_err("CustomError", "CmdJumpThroughStarterShipcaster requires an active solar system")));
            };
            let ship_type_id = ctx.db.with_snapshot(|s| s.items.get(&ship_id).map(|i| i.type_id)).unwrap_or(0);
            let ship_group_id = ctx.sde.types.get(&ship_type_id).map(|t| t.group_id).unwrap_or(0);
            if matches!(ship_group_id, 30 | 485 | 547 | 659 | 883 | 1538 | 4594) {
                return Some(Err(rpc_err("CapitalShipCannotJumpShipcaster", "Capital ships cannot jump through a shipcaster")));
            }
            let caster_id = match args.first().and_then(|a| a.as_i64()) {
                Some(id) if id > 0 => id,
                Some(id) => return Some(Err(rpc_err("TargetNotFound", format!("Invalid caster ID {id}")))),
                None => return Some(Err(rpc_err("InvalidArguments", "CmdJumpThroughStarterShipcaster requires caster itemID"))),
            };

            let starter_launcher = match with_misc_state(ctx, |st| {
                st.starter_launchers.iter().find(|l| l.item_id == caster_id).cloned()
            }) {
                Some(l) => l,
                None => return Some(Err(rpc_err("LauncherNotFound", format!("Starter shipcaster {caster_id} not registered")))),
            };
            if starter_launcher.solar_system_id != sys_id {
                return Some(Err(rpc_err(
                    "InvalidSolarSystem",
                    format!("Starter shipcaster {caster_id} is located in system {}, not current system {sys_id}", starter_launcher.solar_system_id),
                )));
            }

            let now = ctx.now_unix();
            {
                let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
                if cw.get_or_create(char_id).is_weapon_timer_active(now) {
                    return Some(Err(rpc_err("WeaponsTimerActive", "Cannot jump through starter shipcaster while weapons timer is active")));
                }
            }

            let sys = ctx.systems.get_or_spawn(sys_id);
            if let Err(e) = check_proximity(&sys, ship_id, caster_id, SHIPCASTER_MAX_RANGE_M, "starter shipcaster").await {
                return Some(Err(e));
            }

            let dest_sys = starter_launcher.staging_system_id;
                        let pad = with_misc_state(ctx, |st| {
                st.landing_pads.iter().find(|p| p.faction_id == starter_launcher.faction_id && p.solar_system_id == dest_sys).cloned()
            });
            let arrival_pos = pad.map(|p| p.position).unwrap_or(glam::DVec3::ZERO) + glam::DVec3::new(2_500.0, 0.0, 0.0);
            if let Err(e) = execute_shipcaster_jump(ctx, session, char_id, ship_id, sys_id, dest_sys, arrival_pos).await {
                return Some(Err(e));
            }

            Some(Ok(PyRep::None))
        }

        // --------------------------------------------------------------------
        // shipcasterTravelMgr: Interstellar Shipcaster Travel
        // --------------------------------------------------------------------
        ("shipcasterTravelMgr", "CmdJumpThroughShipcaster") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if session.station_id.is_some() || session.structure_id.is_some() {
                return Some(Err(rpc_err("CantDoThatWhileDocked", "Cannot jump shipcaster while docked")));
            }
            let Some(ship_id) = session.ship_id else {
                return Some(Err(rpc_err("CustomError", "CmdJumpThroughShipcaster requires an active ship")));
            };
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(rpc_err("CustomError", "CmdJumpThroughShipcaster requires an active solar system")));
            };

            let caster_id = match args.first().and_then(|a| a.as_i64()) {
                Some(id) if id > 0 => id,
                Some(id) => return Some(Err(rpc_err("TargetNotFound", format!("Invalid caster ID {id}")))),
                None => return Some(Err(rpc_err("InvalidArguments", "CmdJumpThroughShipcaster requires caster itemID"))),
            };
            let to_system_id = match args.get(1).and_then(|a| a.as_i64()) {
                Some(id) => u32::try_from(id).unwrap_or(0),
                None => return Some(Err(rpc_err("InvalidArguments", "CmdJumpThroughShipcaster requires to_system_id"))),
            };
            let to_pad_id = match args.get(2).and_then(|a| a.as_i64()) {
                Some(id) => id,
                None => return Some(Err(rpc_err("InvalidArguments", "CmdJumpThroughShipcaster requires to_pad_id"))),
            };

            let launcher = match with_misc_state(ctx, |st| {
                st.launchers.iter().find(|l| l.item_id == caster_id).cloned()
            }) {
                Some(l) => l,
                None => return Some(Err(rpc_err("LauncherNotFound", format!("Shipcaster launcher {caster_id} not registered")))),
            };

            let ship_type_id = ctx.db.with_snapshot(|s| s.items.get(&ship_id).map(|i| i.type_id)).unwrap_or(0);
            let ship_group_id = ctx.sde.types.get(&ship_type_id).map(|t| t.group_id).unwrap_or(0);
            if matches!(ship_group_id, 30 | 485 | 547 | 659 | 883 | 1538 | 4594) {
                return Some(Err(rpc_err("CapitalShipCannotJumpShipcaster", "Capital ships cannot jump through an interstellar shipcaster")));
            }
            let char_war_faction = ctx.db.with_snapshot(|s| s.characters.get(&char_id).and_then(|c| c.war_faction_id));
            if let Some(wf) = char_war_faction {
                if launcher.faction_id > 0 && wf != launcher.faction_id as i64 {
                    return Some(Err(rpc_err("FactionShipcasterAccessDenied", "Character militia faction does not match shipcaster faction")));
                }
            }
            if launcher.solar_system_id != sys_id {
                return Some(Err(rpc_err(
                    "InvalidSolarSystem",
                    format!("Shipcaster launcher {caster_id} is located in system {}, not current system {sys_id}", launcher.solar_system_id),
                )));
            }

            let pad = match with_misc_state(ctx, |st| {
                st.landing_pads.iter().find(|p| p.item_id == to_pad_id).cloned()
            }) {
                Some(p) => p,
                None => return Some(Err(rpc_err("LandingPadNotFound", format!("Landing pad {to_pad_id} not registered")))),
            };
            if pad.solar_system_id != to_system_id {
                return Some(Err(rpc_err(
                    "InvalidDestinationSystem",
                    format!("Landing pad {to_pad_id} is located in system {}, not specified destination {to_system_id}", pad.solar_system_id),
                )));
            }
            if !launcher.linked_pad_ids.contains(&to_pad_id) || pad.link_timestamp <= 0 {
                return Some(Err(rpc_err(
                    "LandingPadNotLinked",
                    format!("Landing pad {to_pad_id} is not currently linked to launcher {caster_id}"),
                )));
            }
            if !pad.is_built || pad.is_disrupted {
                return Some(Err(rpc_err(
                    "LandingPadUnavailable",
                    format!("Landing pad {to_pad_id} is not built or is disrupted"),
                )));
            }

            let now = ctx.now_unix();
            {
                let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
                if cw.get_or_create(char_id).is_weapon_timer_active(now) {
                    return Some(Err(rpc_err("WeaponsTimerActive", "Cannot jump through shipcaster while weapons timer is active")));
                }
            }

            let sys = ctx.systems.get_or_spawn(sys_id);
            if let Err(e) = check_proximity(&sys, ship_id, caster_id, SHIPCASTER_MAX_RANGE_M, "shipcaster launcher").await {
                return Some(Err(e));
            }

            let arrival_pos = pad.position + glam::DVec3::new(2_500.0, 0.0, 0.0);
            if let Err(e) = execute_shipcaster_jump(ctx, session, char_id, ship_id, sys_id, to_system_id, arrival_pos).await {
                return Some(Err(e));
            }

            Some(Ok(PyRep::None))
        }

        _ => None,
    }
}
