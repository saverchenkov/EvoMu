//! Unified atomic `SessionTransition` pipeline (`[Arch-08]`, `#590`).
//!
//! Centralizes character spatial (`solar_system_id`, `constellation_id`, `region_id`,
//! `station_id`, `structure_id`) and active-vessel (`active_ship_id`) transitions across
//! `WorldStateSnapshot`, `InventoryItemRecord` locations/flags, `Ballpark` state,
//! bound-object invalidation, local chat membership, `quasar_gateway`, `SessionState`,
//! and `OnSessionChanged` notifications.

#![forbid(unsafe_code)]

use std::collections::HashMap;

use eve_db::{CharacterRecord, DbError, InventoryFlag, WorldStateSnapshot};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::SdeRegistry;
use glam::DVec3;

use super::{NotificationTarget, ServerContext};

/// Destination location domain for an atomic character/session transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransitionLocation {
    KeepCurrent,
    Space { solar_system_id: u32 },
    Station { solar_system_id: Option<u32>, station_id: i64 },
    Structure { solar_system_id: Option<u32>, structure_id: i64 },
    ClearAll,
}

/// Canonical gameplay/lifecycle event driving a `SessionTransition`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransitionKind {
    Dock {
        dock_id: i64,
        is_structure: bool,
    },
    Undock {
        solar_system_id: u32,
    },
    JumpSystem {
        solar_system_id: u32,
    },
    BoardShip {
        ship_id: i64,
    },
    EjectToPod {
        capsule_id: i64,
    },
    CloneJump {
        solar_system_id: u32,
        dock_id: Option<(i64, bool)>,
        ship_id: Option<i64>,
    },
    DeathRespawn {
        solar_system_id: u32,
        dock_id: Option<(i64, bool)>,
        ship_id: i64,
    },
    Teleport {
        solar_system_id: u32,
        dock_id: Option<(i64, bool)>,
        ship_id: Option<i64>,
    },
    CharacterLogin {
        solar_system_id: u32,
        dock_id: Option<(i64, bool)>,
        ship_id: Option<i64>,
    },
    Logout,
}

impl TransitionKind {
    pub fn destination_location(self) -> TransitionLocation {
        match self {
            Self::Dock {
                dock_id,
                is_structure: false,
            } => TransitionLocation::Station {
                solar_system_id: None,
                station_id: dock_id,
            },
            Self::Dock {
                dock_id,
                is_structure: true,
            } => TransitionLocation::Structure {
                solar_system_id: None,
                structure_id: dock_id,
            },
            Self::Undock { solar_system_id } | Self::JumpSystem { solar_system_id } => {
                TransitionLocation::Space { solar_system_id }
            }
            Self::BoardShip { .. } | Self::EjectToPod { .. } => TransitionLocation::KeepCurrent,
            Self::CloneJump {
                solar_system_id,
                dock_id,
                ..
            }
            | Self::Teleport {
                solar_system_id,
                dock_id,
                ..
            }
            | Self::CharacterLogin {
                solar_system_id,
                dock_id,
                ..
            } => match dock_id {
                Some((id, true)) => TransitionLocation::Structure {
                    solar_system_id: Some(solar_system_id),
                    structure_id: id,
                },
                Some((id, false)) => TransitionLocation::Station {
                    solar_system_id: Some(solar_system_id),
                    station_id: id,
                },
                None => TransitionLocation::Space { solar_system_id },
            },
            Self::DeathRespawn {
                solar_system_id,
                dock_id,
                ..
            } => match dock_id {
                Some((id, true)) => TransitionLocation::Structure {
                    solar_system_id: Some(solar_system_id),
                    structure_id: id,
                },
                Some((id, false)) => TransitionLocation::Station {
                    solar_system_id: Some(solar_system_id),
                    station_id: id,
                },
                None => TransitionLocation::Space { solar_system_id },
            },
            Self::Logout => TransitionLocation::ClearAll,
        }
    }

    pub fn target_ship_id(self) -> Option<i64> {
        match self {
            Self::BoardShip { ship_id }
            | Self::EjectToPod {
                capsule_id: ship_id,
            }
            | Self::DeathRespawn { ship_id, .. } => Some(ship_id),
            Self::CloneJump { ship_id, .. }
            | Self::Teleport { ship_id, .. }
            | Self::CharacterLogin { ship_id, .. } => ship_id,
            Self::Dock { .. } | Self::Undock { .. } | Self::JumpSystem { .. } | Self::Logout => {
                None
            }
        }
    }
}

/// Full specification for executing an atomic `SessionTransition`.
#[derive(Debug, Clone)]
pub struct SessionTransitionSpec {
    pub char_id: i64,
    pub kind: TransitionKind,
    pub reason: &'static str,
    pub validate_destination: bool,
    pub move_ship_item: bool,
    pub apply_db: bool,
    pub cleanup_departing_space: bool,
    pub deactivate_modules: bool,
    pub abort_self_destruct: bool,
    pub abandon_drones: bool,
    pub sync_local_chat: bool,
    pub invalidate_bounds: bool,
    pub sync_quasar_gateway: bool,
    pub emit_on_session_changed: bool,
    pub arrive_ball_pos: Option<DVec3>,
    pub spawn_destination_ball: bool,
    pub accept_ballpark_dock: Option<(i64, DVec3)>,
    pub preserve_session_ship: bool,
}

impl SessionTransitionSpec {
    pub fn for_kind(char_id: i64, kind: TransitionKind, reason: &'static str) -> Self {
        match kind {
            TransitionKind::Dock { .. } => Self {
                char_id,
                kind,
                reason,
                validate_destination: true,
                move_ship_item: true,
                apply_db: true,
                cleanup_departing_space: true,
                deactivate_modules: true,
                abort_self_destruct: true,
                abandon_drones: true,
                sync_local_chat: false,
                invalidate_bounds: true,
                sync_quasar_gateway: true,
                emit_on_session_changed: true,
                arrive_ball_pos: None,
                spawn_destination_ball: false,
                accept_ballpark_dock: None,
                preserve_session_ship: false,
            },
            TransitionKind::Undock { .. } => Self {
                char_id,
                kind,
                reason,
                validate_destination: true,
                move_ship_item: true,
                apply_db: true,
                cleanup_departing_space: false,
                deactivate_modules: false,
                abort_self_destruct: false,
                abandon_drones: false,
                sync_local_chat: false,
                invalidate_bounds: true,
                sync_quasar_gateway: true,
                emit_on_session_changed: true,
                arrive_ball_pos: None,
                spawn_destination_ball: false,
                accept_ballpark_dock: None,
                preserve_session_ship: false,
            },
            TransitionKind::JumpSystem { .. } => Self {
                char_id,
                kind,
                reason,
                validate_destination: true,
                move_ship_item: true,
                apply_db: true,
                cleanup_departing_space: true,
                deactivate_modules: true,
                abort_self_destruct: true,
                abandon_drones: true,
                sync_local_chat: true,
                invalidate_bounds: true,
                sync_quasar_gateway: true,
                emit_on_session_changed: true,
                arrive_ball_pos: None,
                spawn_destination_ball: false,
                accept_ballpark_dock: None,
                preserve_session_ship: false,
            },
            TransitionKind::BoardShip { .. } | TransitionKind::EjectToPod { .. } => Self {
                char_id,
                kind,
                reason,
                validate_destination: false,
                move_ship_item: false,
                apply_db: true,
                cleanup_departing_space: false,
                deactivate_modules: false,
                abort_self_destruct: true,
                abandon_drones: false,
                sync_local_chat: false,
                invalidate_bounds: true,
                sync_quasar_gateway: true,
                emit_on_session_changed: true,
                arrive_ball_pos: None,
                spawn_destination_ball: false,
                accept_ballpark_dock: None,
                preserve_session_ship: false,
            },
            TransitionKind::CloneJump { .. } => Self {
                char_id,
                kind,
                reason,
                validate_destination: true,
                move_ship_item: false,
                apply_db: true,
                cleanup_departing_space: true,
                deactivate_modules: true,
                abort_self_destruct: true,
                abandon_drones: true,
                sync_local_chat: true,
                invalidate_bounds: true,
                sync_quasar_gateway: true,
                emit_on_session_changed: true,
                arrive_ball_pos: None,
                spawn_destination_ball: false,
                accept_ballpark_dock: None,
                preserve_session_ship: false,
            },
            TransitionKind::DeathRespawn { .. } => Self {
                char_id,
                kind,
                reason,
                validate_destination: true,
                move_ship_item: true,
                apply_db: true,
                cleanup_departing_space: true,
                deactivate_modules: true,
                abort_self_destruct: true,
                abandon_drones: true,
                sync_local_chat: true,
                invalidate_bounds: true,
                sync_quasar_gateway: true,
                emit_on_session_changed: true,
                arrive_ball_pos: None,
                spawn_destination_ball: false,
                accept_ballpark_dock: None,
                preserve_session_ship: false,
            },
            TransitionKind::Teleport { .. } => Self {
                char_id,
                kind,
                reason,
                validate_destination: false,
                move_ship_item: true,
                apply_db: true,
                cleanup_departing_space: false,
                deactivate_modules: false,
                abort_self_destruct: false,
                abandon_drones: false,
                sync_local_chat: true,
                invalidate_bounds: true,
                sync_quasar_gateway: true,
                emit_on_session_changed: true,
                arrive_ball_pos: None,
                spawn_destination_ball: false,
                accept_ballpark_dock: None,
                preserve_session_ship: false,
            },
            TransitionKind::CharacterLogin { .. } => Self {
                char_id,
                kind,
                reason,
                validate_destination: false,
                move_ship_item: false,
                apply_db: false,
                cleanup_departing_space: false,
                deactivate_modules: false,
                abort_self_destruct: false,
                abandon_drones: false,
                sync_local_chat: true,
                invalidate_bounds: false,
                sync_quasar_gateway: true,
                emit_on_session_changed: false,
                arrive_ball_pos: None,
                spawn_destination_ball: false,
                accept_ballpark_dock: None,
                preserve_session_ship: false,
            },
            TransitionKind::Logout => Self {
                char_id,
                kind,
                reason,
                validate_destination: false,
                move_ship_item: false,
                apply_db: false,
                cleanup_departing_space: false,
                deactivate_modules: false,
                abort_self_destruct: false,
                abandon_drones: false,
                sync_local_chat: false,
                invalidate_bounds: true,
                sync_quasar_gateway: false,
                emit_on_session_changed: false,
                arrive_ball_pos: None,
                spawn_destination_ball: false,
                accept_ballpark_dock: None,
                preserve_session_ship: false,
            },
        }
    }

    pub fn with_apply_db(mut self, apply_db: bool) -> Self {
        self.apply_db = apply_db;
        self
    }

    pub fn with_move_ship_item(mut self, move_ship_item: bool) -> Self {
        self.move_ship_item = move_ship_item;
        self
    }

    pub fn with_validate_destination(mut self, validate_destination: bool) -> Self {
        self.validate_destination = validate_destination;
        self
    }

    pub fn with_emit_on_session_changed(mut self, emit: bool) -> Self {
        self.emit_on_session_changed = emit;
        self
    }

    pub fn with_sync_local_chat(mut self, sync: bool) -> Self {
        self.sync_local_chat = sync;
        self
    }

    pub fn with_invalidate_bounds(mut self, invalidate: bool) -> Self {
        self.invalidate_bounds = invalidate;
        self
    }

    pub fn with_cleanup_departing_space(mut self, cleanup: bool) -> Self {
        self.cleanup_departing_space = cleanup;
        self
    }

    pub fn with_arrive_ball_pos(mut self, pos: DVec3) -> Self {
        self.arrive_ball_pos = Some(pos);
        self.spawn_destination_ball = true;
        self
    }

    pub fn with_accept_ballpark_dock(mut self, dock_id: i64, dock_pos: DVec3) -> Self {
        self.accept_ballpark_dock = Some((dock_id, dock_pos));
        self
    }

    pub fn with_preserve_session_ship(mut self, preserve: bool) -> Self {
        self.preserve_session_ship = preserve;
        self
    }
}

/// Result returned from an atomic `SessionTransition`.
#[derive(Debug, Clone)]
pub struct SessionTransitionResult {
    pub updated_character: Option<CharacterRecord>,
    pub diff: HashMap<String, (PyRep, PyRep)>,
    pub removed_departing_ball: Option<Ball>,
    pub arrival_tick: Option<u64>,
}

fn db_err_to_rpc(err: DbError) -> BridgeRpcError {
    match err {
        DbError::InvalidDestinationLocation { .. } => BridgeRpcError {
            code: "InvalidDestinationLocation".into(),
            message: err.to_string(),
            client_error: None,
        },
        DbError::CharacterNotFound(_) => BridgeRpcError {
            code: "CharacterNotFound".into(),
            message: err.to_string(),
            client_error: None,
        },
        DbError::NotDocked(_) => BridgeRpcError::user_error("NotDocked"),
        other => BridgeRpcError {
            code: "SessionTransitionFailed".into(),
            message: other.to_string(),
            client_error: None,
        },
    }
}

fn resolve_system_hierarchy(
    sde: &SdeRegistry,
    sys_id: u32,
    fallback_const: u32,
    fallback_reg: u32,
) -> (u32, u32) {
    sde.solar_systems
        .get(&sys_id)
        .map(|s| (s.constellation_id, s.region_id))
        .unwrap_or_else(|| {
            let c = if fallback_const != 0 {
                fallback_const
            } else {
                20_000_020
            };
            let r = if fallback_reg != 0 {
                fallback_reg
            } else {
                10_000_002
            };
            (c, r)
        })
}

fn is_known_solar_system(snap: &WorldStateSnapshot, sde: &SdeRegistry, sys_id: u32) -> bool {
    sys_id > 0
        && (sde.solar_systems.contains_key(&sys_id)
            || snap.system_states.contains_key(&sys_id)
            || sys_id >= 31_000_000)
}

/// Resolved destination solar-system hierarchy and docking container IDs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ResolvedDestinationLocation {
    pub solar_system_id: u32,
    pub constellation_id: u32,
    pub region_id: u32,
    pub station_id: Option<i64>,
    pub structure_id: Option<i64>,
}

/// Resolves destination `(solar_system_id, constellation_id, region_id, station_id, structure_id)`
/// and optionally validates that the target system/station/structure exists and is usable.
pub fn resolve_destination_location(
    snap: &WorldStateSnapshot,
    sde: &SdeRegistry,
    char_id: i64,
    current_ch: &CharacterRecord,
    location: TransitionLocation,
    validate_destination: bool,
) -> Result<ResolvedDestinationLocation, DbError> {
    match location {
        TransitionLocation::KeepCurrent | TransitionLocation::ClearAll => {
            let (const_id, reg_id) = resolve_system_hierarchy(
                sde,
                current_ch.solar_system_id,
                current_ch.constellation_id,
                current_ch.region_id,
            );
            Ok(ResolvedDestinationLocation {
                solar_system_id: current_ch.solar_system_id,
                constellation_id: const_id,
                region_id: reg_id,
                station_id: current_ch.station_id,
                structure_id: current_ch.structure_id,
            })
        }
        TransitionLocation::Space { solar_system_id } => {
            if solar_system_id == 0
                || (validate_destination && !is_known_solar_system(snap, sde, solar_system_id))
            {
                return Err(DbError::InvalidDestinationLocation {
                    character_id: char_id,
                    location_id: solar_system_id as i64,
                });
            }
            let (const_id, reg_id) = resolve_system_hierarchy(
                sde,
                solar_system_id,
                current_ch.constellation_id,
                current_ch.region_id,
            );
            Ok(ResolvedDestinationLocation {
                solar_system_id,
                constellation_id: const_id,
                region_id: reg_id,
                station_id: None,
                structure_id: None,
            })
        }
        TransitionLocation::Station {
            solar_system_id,
            station_id,
        } => {
            let resolved_sys = sde
                .stations
                .get(&station_id)
                .map(|s| s.solar_system_id)
                .or(solar_system_id);
            let Some(sys_id) = resolved_sys else {
                return Err(DbError::InvalidDestinationLocation {
                    character_id: char_id,
                    location_id: station_id,
                });
            };
            let station_exists = sde.stations.contains_key(&station_id);
            if validate_destination && (!station_exists || !is_known_solar_system(snap, sde, sys_id))
            {
                return Err(DbError::InvalidDestinationLocation {
                    character_id: char_id,
                    location_id: station_id,
                });
            }
            let (const_id, reg_id) = resolve_system_hierarchy(
                sde,
                sys_id,
                current_ch.constellation_id,
                current_ch.region_id,
            );
            Ok(ResolvedDestinationLocation {
                solar_system_id: sys_id,
                constellation_id: const_id,
                region_id: reg_id,
                station_id: Some(station_id),
                structure_id: None,
            })
        }
        TransitionLocation::Structure {
            solar_system_id,
            structure_id,
        } => {
            let st_opt = snap.structures.get(&structure_id);
            if validate_destination {
                let Some(st) = st_opt else {
                    return Err(DbError::InvalidDestinationLocation {
                        character_id: char_id,
                        location_id: structure_id,
                    });
                };
                if st.state == "Destroyed" {
                    return Err(DbError::InvalidDestinationLocation {
                        character_id: char_id,
                        location_id: structure_id,
                    });
                }
            }
            let sys_id = st_opt
                .map(|s| s.solar_system_id)
                .or(solar_system_id)
                .unwrap_or(current_ch.solar_system_id);
            if validate_destination && !is_known_solar_system(snap, sde, sys_id) {
                return Err(DbError::InvalidDestinationLocation {
                    character_id: char_id,
                    location_id: structure_id,
                });
            }
            let (const_id, reg_id) = resolve_system_hierarchy(
                sde,
                sys_id,
                current_ch.constellation_id,
                current_ch.region_id,
            );
            Ok(ResolvedDestinationLocation {
                solar_system_id: sys_id,
                constellation_id: const_id,
                region_id: reg_id,
                station_id: None,
                structure_id: Some(structure_id),
            })
        }
    }
}

/// Atomically updates `CharacterRecord` spatial/vessel fields (`solar_system_id`, `constellation_id`,
/// `region_id`, `station_id`, `structure_id`, `active_ship_id`, `version`, `system_epoch`) and
/// optionally moves the active ship `InventoryItemRecord` inside `WorldStateSnapshot`.
pub fn apply_character_transition_in_snap(
    snap: &mut WorldStateSnapshot,
    sde: &SdeRegistry,
    char_id: i64,
    location: TransitionLocation,
    new_ship_id: Option<i64>,
    move_ship_item: bool,
    validate_destination: bool,
) -> Result<CharacterRecord, DbError> {
    apply_character_transition_in_snap_with_trades(
        snap,
        sde,
        char_id,
        location,
        new_ship_id,
        move_ship_item,
        validate_destination,
    )
    .map(|(ch, _)| ch)
}

fn apply_character_transition_in_snap_with_trades(
    snap: &mut WorldStateSnapshot,
    sde: &SdeRegistry,
    char_id: i64,
    location: TransitionLocation,
    new_ship_id: Option<i64>,
    move_ship_item: bool,
    validate_destination: bool,
) -> Result<(CharacterRecord, Vec<(i64, i64, i64)>), DbError> {
    let ch_pre = snap
        .characters
        .get(&char_id)
        .cloned()
        .ok_or(DbError::CharacterNotFound(char_id))?;

    let ResolvedDestinationLocation {
        solar_system_id: sys_id,
        constellation_id: const_id,
        region_id: reg_id,
        station_id,
        structure_id,
    } = resolve_destination_location(
        snap,
        sde,
        char_id,
        &ch_pre,
        location,
        validate_destination,
    )?;

    let old_dock = ch_pre.station_id.or(ch_pre.structure_id);
    let new_dock = if matches!(location, TransitionLocation::ClearAll) {
        None
    } else if matches!(location, TransitionLocation::KeepCurrent) {
        old_dock
    } else {
        station_id.or(structure_id)
    };
    let cancelled_trades = if old_dock != new_dock {
        crate::services::wallet_services_ext::cancel_open_trades_for_character_in_snap(
            snap, char_id,
        )
    } else {
        Vec::new()
    };

    let eff_ship_id = {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        if !matches!(
            location,
            TransitionLocation::KeepCurrent | TransitionLocation::ClearAll
        ) {
            ch.solar_system_id = sys_id;
            ch.constellation_id = const_id;
            ch.region_id = reg_id;
            ch.station_id = station_id;
            ch.structure_id = structure_id;
        } else {
            if ch.constellation_id == 0 {
                ch.constellation_id = const_id;
            }
            if ch.region_id == 0 {
                ch.region_id = reg_id;
            }
        }
        if let Some(sid) = new_ship_id {
            ch.active_ship_id = sid;
        }
        ch.version += 1;
        if !matches!(
            location,
            TransitionLocation::KeepCurrent | TransitionLocation::ClearAll
        ) {
            ch.system_epoch += 1;
        }
        ch.active_ship_id
    };

    if move_ship_item
        && eff_ship_id > 0
        && !matches!(
            location,
            TransitionLocation::KeepCurrent | TransitionLocation::ClearAll
        )
    {
        if let Some(ship) = snap.items.get_mut(&eff_ship_id) {
            if let Some(dock_id) = station_id.or(structure_id) {
                ship.location_id = dock_id;
                ship.flag = InventoryFlag::Hangar;
            } else {
                ship.location_id = sys_id as i64;
                ship.flag = InventoryFlag::None;
            }
            ship.version += 1;
        }
    }

    Ok((snap.characters[&char_id].clone(), cancelled_trades))
}

/// Clears all character/spatial attributes on a `SessionState` (used on logoff / character clear).
pub fn clear_session_character_state(session: &mut SessionState) {
    session.station_id = None;
    session.structure_id = None;
    session.solar_system_id = None;
    session.constellation_id = None;
    session.region_id = None;
    session.ship_id = None;
}

/// Synchronizes all spatial and active-ship attributes on `SessionState` from a `CharacterRecord`.
pub fn apply_session_from_character(
    session: &mut SessionState,
    ch: &CharacterRecord,
    override_ship: Option<Option<i64>>,
    sde: Option<&SdeRegistry>,
) {
    let (resolved_const, resolved_reg) = if let Some(sde) = sde {
        resolve_system_hierarchy(
            sde,
            ch.solar_system_id,
            ch.constellation_id,
            ch.region_id,
        )
    } else {
        (ch.constellation_id, ch.region_id)
    };
    session.solar_system_id = Some(ch.solar_system_id);
    if resolved_const != 0 {
        session.constellation_id = Some(resolved_const);
    }
    if resolved_reg != 0 {
        session.region_id = Some(resolved_reg);
    }
    session.station_id = ch.station_id;
    session.structure_id = ch.structure_id;
    session.ship_id =
        override_ship.unwrap_or_else(|| (ch.active_ship_id > 0).then_some(ch.active_ship_id));
}

fn synthetic_session_from_character(
    char_id: i64,
    ch: &CharacterRecord,
    override_ship: Option<Option<i64>>,
    sde: &SdeRegistry,
) -> SessionState {
    let mut s = SessionState::new(0);
    s.character_id = Some(char_id);
    s.user_id = Some(ch.user_id);
    s.corporation_id = Some(ch.corporation_id);
    s.alliance_id = ch.alliance_id;
    apply_session_from_character(&mut s, ch, override_ship, Some(sde));
    s
}

/// Builds the full keyval representation of current session attributes expected by `OnSessionChanged`.
pub fn build_session_state_keyval(session: &SessionState, char_id: i64) -> PyRep {
    let eff_loc = session.effective_location_id();
    let in_space_sys = if session.station_id.is_none() && session.structure_id.is_none() {
        session.solar_system_id.map(|s| PyRep::Integer(i64::from(s)))
    } else {
        None
    };
    PyRep::keyval([
        (
            "userid",
            session
                .user_id
                .map(|u| PyRep::Integer(u as i64))
                .unwrap_or(PyRep::None),
        ),
        ("charid", PyRep::Integer(char_id)),
        (
            "corpid",
            session
                .corporation_id
                .map(PyRep::Integer)
                .unwrap_or(PyRep::None),
        ),
        (
            "allianceid",
            session
                .alliance_id
                .map(PyRep::Integer)
                .unwrap_or(PyRep::None),
        ),
        (
            "shipid",
            session
                .ship_id
                .filter(|&id| id > 0)
                .map(PyRep::Integer)
                .unwrap_or(PyRep::None),
        ),
        (
            "stationid",
            session
                .station_id
                .map(PyRep::Integer)
                .unwrap_or(PyRep::None),
        ),
        (
            "structureid",
            session
                .structure_id
                .map(PyRep::Integer)
                .unwrap_or(PyRep::None),
        ),
        (
            "solarsystemid2",
            session
                .solar_system_id
                .map(|s| PyRep::Integer(i64::from(s)))
                .unwrap_or(PyRep::None),
        ),
        ("solarsystemid", in_space_sys.unwrap_or(PyRep::None)),
        (
            "locationid",
            eff_loc.map(PyRep::Integer).unwrap_or(PyRep::None),
        ),
        (
            "constellationid",
            session
                .constellation_id
                .map(|c| PyRep::Integer(i64::from(c)))
                .unwrap_or(PyRep::None),
        ),
        (
            "regionid",
            session
                .region_id
                .map(|r| PyRep::Integer(i64::from(r)))
                .unwrap_or(PyRep::None),
        ),
    ])
}

/// Pushes an `OnSessionChanged` notification onto `session` (if provided) and delivers it
/// to `NotificationTarget::Character(char_id)`.
pub fn emit_on_session_changed(
    ctx: &ServerContext,
    mut session: Option<&mut SessionState>,
    snap_session: &SessionState,
    char_id: i64,
    diff: &HashMap<String, (PyRep, PyRep)>,
) {
    if diff.is_empty() {
        return;
    }
    let sess_rep = build_session_state_keyval(snap_session, char_id);
    let diff_entries = diff
        .iter()
        .map(|(k, (old_v, new_v))| {
            (
                PyRep::String(k.clone()),
                PyRep::Tuple(vec![old_v.clone(), new_v.clone()]),
            )
        })
        .collect();
    let args = vec![PyRep::Bool(false), sess_rep, PyRep::Dict(diff_entries)];
    if let Some(sess) = session.as_deref_mut() {
        sess.push_notification("OnSessionChanged", args.clone());
    }
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnSessionChanged",
        "charid",
        args,
    );
}

/// Applies `CharacterRecord` (or clears character state on logout) to `SessionState`,
/// invalidates bound objects, updates `quasar_gateway`, and emits `OnSessionChanged` diff.
#[allow(clippy::too_many_arguments)]
pub fn sync_session_from_character(
    ctx: &ServerContext,
    session: &mut SessionState,
    prev_snapshot: Option<&SessionState>,
    ch_opt: Option<&CharacterRecord>,
    reason: &str,
    invalidate_bounds: bool,
    sync_quasar: bool,
    emit_changed: bool,
) -> HashMap<String, (PyRep, PyRep)> {
    sync_session_from_character_with_override(
        ctx,
        session,
        prev_snapshot,
        ch_opt,
        None,
        reason,
        invalidate_bounds,
        sync_quasar,
        emit_changed,
    )
}

#[allow(clippy::too_many_arguments)]
fn sync_session_from_character_with_override(
    ctx: &ServerContext,
    session: &mut SessionState,
    prev_snapshot: Option<&SessionState>,
    ch_opt: Option<&CharacterRecord>,
    override_ship: Option<Option<i64>>,
    reason: &str,
    invalidate_bounds: bool,
    sync_quasar: bool,
    emit_changed: bool,
) -> HashMap<String, (PyRep, PyRep)> {
    let prev = prev_snapshot.cloned().unwrap_or_else(|| session.clone());
    match ch_opt {
        Some(ch) => {
            apply_session_from_character(session, ch, override_ship, Some(&ctx.sde));
        }
        None => {
            clear_session_character_state(session);
        }
    }
    if invalidate_bounds {
        if let Some(cid) = session.character_id.or_else(|| prev.character_id) {
            let sys_id = session
                .solar_system_id
                .or(prev.solar_system_id)
                .unwrap_or(0);
            let active_dock = session.station_id.or(session.structure_id);
            ctx.invalidate_bound_objects_for_session_transition(cid, sys_id, active_dock, reason);
        }
    }
    if sync_quasar {
        ctx.quasar_gateway.on_session_changed(session, ctx.now_unix());
    }
    let diff = prev.compute_diff(session);
    if emit_changed && !diff.is_empty() {
        if let Some(cid) = session.character_id.or_else(|| ch_opt.map(|c| c.character_id)) {
            let snap_s = session.clone();
            emit_on_session_changed(ctx, Some(session), &snap_s, cid, &diff);
        }
    }
    diff
}

fn run_departing_space_cleanup_sync(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    sys_id: u32,
    spec: &SessionTransitionSpec,
) {
    if !spec.cleanup_departing_space || ship_id <= 0 || sys_id == 0 {
        return;
    }
    if spec.abandon_drones {
        crate::sp6_lifecycle::abandon_drones_and_fighters_for_ship_sync(
            ctx, char_id, ship_id, sys_id,
        );
    }
    if spec.abort_self_destruct {
        crate::sp6_lifecycle::abort_self_destruct_for_ship(ctx, ship_id);
    }
    if spec.deactivate_modules {
        crate::services::space_navigation_advanced::deactivate_ship_modules_on_leave_space(
            ctx, char_id, ship_id,
        );
    }
}

fn apply_spec_destination_to_character_copy(
    sde: &SdeRegistry,
    ch: &mut CharacterRecord,
    dest_loc: TransitionLocation,
    target_ship: Option<i64>,
) {
    match dest_loc {
        TransitionLocation::Space { solar_system_id } => {
            ch.solar_system_id = solar_system_id;
            let (cid, rid) =
                resolve_system_hierarchy(sde, solar_system_id, ch.constellation_id, ch.region_id);
            ch.constellation_id = cid;
            ch.region_id = rid;
            ch.station_id = None;
            ch.structure_id = None;
        }
        TransitionLocation::Station {
            solar_system_id,
            station_id,
        } => {
            if let Some(sys_id) = solar_system_id {
                ch.solar_system_id = sys_id;
                let (cid, rid) =
                    resolve_system_hierarchy(sde, sys_id, ch.constellation_id, ch.region_id);
                ch.constellation_id = cid;
                ch.region_id = rid;
            }
            ch.station_id = Some(station_id);
            ch.structure_id = None;
        }
        TransitionLocation::Structure {
            solar_system_id,
            structure_id,
        } => {
            if let Some(sys_id) = solar_system_id {
                ch.solar_system_id = sys_id;
                let (cid, rid) =
                    resolve_system_hierarchy(sde, sys_id, ch.constellation_id, ch.region_id);
                ch.constellation_id = cid;
                ch.region_id = rid;
            }
            ch.structure_id = Some(structure_id);
            ch.station_id = None;
        }
        TransitionLocation::KeepCurrent | TransitionLocation::ClearAll => {}
    }
    if let Some(sid) = target_ship {
        ch.active_ship_id = sid;
    }
}

impl ServerContext {
    /// Executes the atomic `SessionTransition` pipeline synchronously (for sync RPC handlers,
    /// combat ticks, login/logout, and unit tests without async runtime overhead).
    pub fn execute_session_transition_sync(
        &self,
        mut session: Option<&mut SessionState>,
        spec: SessionTransitionSpec,
    ) -> Result<SessionTransitionResult, BridgeRpcError> {
        let pre_ch = self.db.get_character(spec.char_id).ok();
        let prev_session = match session.as_deref() {
            Some(s) => s.clone(),
            None => match pre_ch.as_ref() {
                Some(ch) => synthetic_session_from_character(spec.char_id, ch, None, &self.sde),
                None => SessionState::new(0),
            },
        };

        let old_sys = pre_ch
            .as_ref()
            .map(|c| c.solar_system_id)
            .or(prev_session.solar_system_id)
            .unwrap_or(0);
        let old_dock = pre_ch
            .as_ref()
            .and_then(|c| c.station_id.or(c.structure_id))
            .or(prev_session.station_id)
            .or(prev_session.structure_id);
        let old_ship = pre_ch
            .as_ref()
            .map(|c| c.active_ship_id)
            .or(prev_session.ship_id)
            .unwrap_or(0);

        if spec.cleanup_departing_space && old_dock.is_none() {
            run_departing_space_cleanup_sync(self, spec.char_id, old_ship, old_sys, &spec);
        } else if spec.abort_self_destruct && old_ship > 0 {
            crate::sp6_lifecycle::abort_self_destruct_for_ship(self, old_ship);
        }

        let dest_loc = spec.kind.destination_location();
        let target_ship = spec.kind.target_ship_id();
        let updated_ch = if matches!(spec.kind, TransitionKind::Logout) {
            crate::services::wallet_services_ext::cancel_open_trades_for_character(
                self,
                spec.char_id,
            );
            None
        } else if spec.apply_db {
            let (ch, cancelled_trades) = self
                .db
                .try_with_mut_snapshot(|snap| {
                    apply_character_transition_in_snap_with_trades(
                        snap,
                        &self.sde,
                        spec.char_id,
                        dest_loc,
                        target_ship,
                        spec.move_ship_item,
                        spec.validate_destination,
                    )
                })
                .map_err(db_err_to_rpc)?;
            for (trade_id, char_a, char_b) in cancelled_trades {
                for party in [char_a, char_b] {
                    self.notify(
                        NotificationTarget::Character(party),
                        "OnTradeCancel",
                        "charid",
                        vec![PyRep::Integer(trade_id)],
                    );
                }
            }
            Some(ch)
        } else {
            let mut ch_opt = self.db.get_character(spec.char_id).ok();
            if !matches!(spec.kind, TransitionKind::CharacterLogin { .. }) {
                if let Some(ref mut ch) = ch_opt {
                    apply_spec_destination_to_character_copy(
                        &self.sde,
                        ch,
                        dest_loc,
                        target_ship,
                    );
                }
            }
            ch_opt
        };

        if spec.sync_local_chat {
            if let Some(ref ch) = updated_ch {
                let prev_opt = if matches!(spec.kind, TransitionKind::CharacterLogin { .. })
                    || old_sys == 0
                {
                    None
                } else {
                    Some(old_sys)
                };
                self.sync_character_local_chat(spec.char_id, prev_opt, ch.solar_system_id);
            }
        }

        let override_ship = if spec.preserve_session_ship && prev_session.ship_id.is_some() {
            Some(prev_session.ship_id)
        } else {
            None
        };
        let diff = if let Some(sess) = session.as_deref_mut() {
            sync_session_from_character_with_override(
                self,
                sess,
                Some(&prev_session),
                updated_ch.as_ref(),
                override_ship,
                spec.reason,
                spec.invalidate_bounds,
                spec.sync_quasar_gateway,
                spec.emit_on_session_changed,
            )
        } else {
            let mut next_synth = prev_session.clone();
            if let Some(ref ch) = updated_ch {
                apply_session_from_character(&mut next_synth, ch, override_ship, Some(&self.sde));
            } else {
                clear_session_character_state(&mut next_synth);
            }
            if spec.invalidate_bounds {
                let eff_sys = next_synth
                    .solar_system_id
                    .or(prev_session.solar_system_id)
                    .unwrap_or(0);
                let active_dock = next_synth.station_id.or(next_synth.structure_id);
                self.invalidate_bound_objects_for_session_transition(
                    spec.char_id,
                    eff_sys,
                    active_dock,
                    spec.reason,
                );
            }
            if spec.sync_quasar_gateway {
                self.quasar_gateway
                    .on_session_changed(&next_synth, self.now_unix());
            }
            let diff = prev_session.compute_diff(&next_synth);
            if spec.emit_on_session_changed && !diff.is_empty() {
                emit_on_session_changed(self, None, &next_synth, spec.char_id, &diff);
            }
            diff
        };

        Ok(SessionTransitionResult {
            updated_character: updated_ch,
            diff,
            removed_departing_ball: None,
            arrival_tick: None,
        })
    }

    /// Executes the atomic `SessionTransition` pipeline asynchronously, including `Ballpark`
    /// removal (`remove_ball` / `accept_docking` / `clear_target_locks`) and destination spawn.
    pub async fn execute_session_transition(
        &self,
        mut session: Option<&mut SessionState>,
        spec: SessionTransitionSpec,
    ) -> Result<SessionTransitionResult, BridgeRpcError> {
        let pre_ch = self.db.get_character(spec.char_id).ok();
        let prev_session = match session.as_deref() {
            Some(s) => s.clone(),
            None => match pre_ch.as_ref() {
                Some(ch) => synthetic_session_from_character(spec.char_id, ch, None, &self.sde),
                None => SessionState::new(0),
            },
        };

        let old_sys = pre_ch
            .as_ref()
            .map(|c| c.solar_system_id)
            .or(prev_session.solar_system_id)
            .unwrap_or(0);
        let old_dock = pre_ch
            .as_ref()
            .and_then(|c| c.station_id.or(c.structure_id))
            .or(prev_session.station_id)
            .or(prev_session.structure_id);
        let old_ship = pre_ch
            .as_ref()
            .map(|c| c.active_ship_id)
            .or(prev_session.ship_id)
            .unwrap_or(0);

        if spec.validate_destination {
            if let Some(ref ch) = pre_ch {
                self.db
                    .with_snapshot(|snap| {
                        resolve_destination_location(
                            snap,
                            &self.sde,
                            spec.char_id,
                            ch,
                            spec.kind.destination_location(),
                            true,
                        )
                    })
                    .map_err(db_err_to_rpc)?;
            }
        }

        let mut removed_ball = None;
        if spec.cleanup_departing_space && old_dock.is_none() && old_sys != 0 && old_ship > 0 {
            run_departing_space_cleanup_sync(self, spec.char_id, old_ship, old_sys, &spec);
            let old_handle = self.systems.get_or_spawn(old_sys);
            let _ = old_handle.clear_target_locks(old_ship).await;
            if let Some((dock_id, dock_pos)) = spec.accept_ballpark_dock {
                removed_ball = old_handle.accept_docking(old_ship, dock_id, dock_pos).await;
            } else {
                removed_ball = old_handle.remove_ball(old_ship).await;
            }
        } else if spec.abort_self_destruct && old_ship > 0 {
            crate::sp6_lifecycle::abort_self_destruct_for_ship(self, old_ship);
        }

        let dest_loc = spec.kind.destination_location();
        let target_ship = spec.kind.target_ship_id();
        let updated_ch = if matches!(spec.kind, TransitionKind::Logout) {
            crate::services::wallet_services_ext::cancel_open_trades_for_character(
                self,
                spec.char_id,
            );
            None
        } else if spec.apply_db {
            let (ch, cancelled_trades) = self
                .db
                .try_with_mut_snapshot(|snap| {
                    apply_character_transition_in_snap_with_trades(
                        snap,
                        &self.sde,
                        spec.char_id,
                        dest_loc,
                        target_ship,
                        spec.move_ship_item,
                        spec.validate_destination,
                    )
                })
                .map_err(db_err_to_rpc)?;
            for (trade_id, char_a, char_b) in cancelled_trades {
                for party in [char_a, char_b] {
                    self.notify(
                        NotificationTarget::Character(party),
                        "OnTradeCancel",
                        "charid",
                        vec![PyRep::Integer(trade_id)],
                    );
                }
            }
            Some(ch)
        } else {
            let mut ch_opt = self.db.get_character(spec.char_id).ok();
            if !matches!(spec.kind, TransitionKind::CharacterLogin { .. }) {
                if let Some(ref mut ch) = ch_opt {
                    apply_spec_destination_to_character_copy(
                        &self.sde,
                        ch,
                        dest_loc,
                        target_ship,
                    );
                }
            }
            ch_opt
        };

        let mut arrival_tick = None;
        if spec.spawn_destination_ball {
            if let Some(ref ch) = updated_ch {
                if ch.station_id.is_none() && ch.structure_id.is_none() && ch.active_ship_id > 0 {
                    let ship_id = ch.active_ship_id;
                    let ship_item = self.db.with_snapshot(|snap| snap.items.get(&ship_id).cloned());
                    let type_id = ship_item.as_ref().map(|it| it.type_id).unwrap_or(670);
                    let mut ball = removed_ball.clone().unwrap_or_else(|| {
                        let radius = self
                            .sde
                            .types
                            .get(&type_id)
                            .map(|t| t.radius.max(10.0))
                            .unwrap_or(35.0);
                        Ball {
                            id: ship_id,
                            type_id,
                            position: DVec3::ZERO,
                            velocity: DVec3::ZERO,
                            max_velocity: 335.0,
                            inertia: 0.25,
                            mass: 1_000_000.0,
                            radius,
                            speed_fraction: 0.0,
                            mode: MovementMode::Stop,
                        }
                    });
                    if let Some(pos) = spec.arrive_ball_pos {
                        ball.position = pos;
                    }
                    ball.velocity = DVec3::ZERO;
                    ball.speed_fraction = 0.0;
                    ball.mode = MovementMode::Stop;
                    let combat_state = self.build_ship_combat_state(spec.char_id, ship_id, type_id);
                    let slim_meta = self.build_ship_slim_meta(spec.char_id, type_id);
                    let dest_handle = self.systems.get_or_spawn(ch.solar_system_id);
                    arrival_tick = dest_handle
                        .spawn_ball_with_meta(ball, combat_state, slim_meta)
                        .await
                        .ok();
                    let _ = dest_handle.push_set_state(ship_id).await;
                }
            }
        }

        if spec.sync_local_chat {
            if let Some(ref ch) = updated_ch {
                let prev_opt = if matches!(spec.kind, TransitionKind::CharacterLogin { .. })
                    || old_sys == 0
                {
                    None
                } else {
                    Some(old_sys)
                };
                self.sync_character_local_chat(spec.char_id, prev_opt, ch.solar_system_id);
            }
        }

        let override_ship = if spec.preserve_session_ship && prev_session.ship_id.is_some() {
            Some(prev_session.ship_id)
        } else {
            None
        };
        let diff = if let Some(sess) = session.as_deref_mut() {
            sync_session_from_character_with_override(
                self,
                sess,
                Some(&prev_session),
                updated_ch.as_ref(),
                override_ship,
                spec.reason,
                spec.invalidate_bounds,
                spec.sync_quasar_gateway,
                spec.emit_on_session_changed,
            )
        } else {
            let mut next_synth = prev_session.clone();
            if let Some(ref ch) = updated_ch {
                apply_session_from_character(&mut next_synth, ch, override_ship, Some(&self.sde));
            } else {
                clear_session_character_state(&mut next_synth);
            }
            if spec.invalidate_bounds {
                let eff_sys = next_synth
                    .solar_system_id
                    .or(prev_session.solar_system_id)
                    .unwrap_or(0);
                let active_dock = next_synth.station_id.or(next_synth.structure_id);
                self.invalidate_bound_objects_for_session_transition(
                    spec.char_id,
                    eff_sys,
                    active_dock,
                    spec.reason,
                );
            }
            if spec.sync_quasar_gateway {
                self.quasar_gateway
                    .on_session_changed(&next_synth, self.now_unix());
            }
            let diff = prev_session.compute_diff(&next_synth);
            if spec.emit_on_session_changed && !diff.is_empty() {
                emit_on_session_changed(self, None, &next_synth, spec.char_id, &diff);
            }
            diff
        };

        Ok(SessionTransitionResult {
            updated_character: updated_ch,
            diff,
            removed_departing_ball: removed_ball,
            arrival_tick,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eve_db::{
        IskCents, InventoryItemRecord, NeuralAttributes, UpwellStructureRecord, WorldRepository,
    };
    use std::sync::Arc;

    fn test_ctx() -> ServerContext {
        ServerContext::with_repository(Arc::new(WorldRepository::new_empty()))
    }

    fn seed_test_character_and_ship(ctx: &ServerContext, char_id: i64, ship_id: i64, st_id: i64) {
        let sys_id = ctx
            .sde
            .stations
            .get(&st_id)
            .map(|s| s.solar_system_id)
            .unwrap_or(30_000_142);
        let (const_id, reg_id) = ctx
            .sde
            .solar_systems
            .get(&sys_id)
            .map(|s| (s.constellation_id, s.region_id))
            .unwrap_or((20_000_020, 10_000_002));
        ctx.db.with_mut_snapshot(|snap| {
            snap.characters.insert(
                char_id,
                CharacterRecord {
                    activity_status: None,
                    character_id: char_id,
                    user_id: 9_001,
                    name: "Transition Pilot".into(),
                    corporation_id: 1_000_009,
                    alliance_id: None,
                    war_faction_id: None,
                    solar_system_id: sys_id,
                    constellation_id: const_id,
                    region_id: reg_id,
                    station_id: Some(st_id),
                    structure_id: None,
                    home_station_id: st_id,
                    active_ship_id: ship_id,
                    balance: IskCents::from_isk(1_000_000),
                    plex_balance: 0,
                    loyalty_points: Default::default(),
                    standings: Default::default(),
                    security_status: 0.0,
                    clone_grade: 1,
                    race_id: 1,
                    bloodline_id: 1,
                    ancestry_id: 1,
                    school_id: 1,
                    neural: NeuralAttributes {
                        intelligence: 20,
                        memory: 20,
                        perception: 20,
                        willpower: 20,
                        charisma: 19,
                        bonus_remaps: 0,
                        last_remap_unix: None,
                        remap_cooldown_until_unix: None,
                    },
                    skill_points: 0.0,
                    free_skill_points: 0.0,
                    last_clone_jump_unix: 0,
                    jump_fatigue_minutes: 0.0,
                    jump_cooldown_until_unix: 0,
                    jump_fatigue_until_unix: 0,
                    description: String::new(),
                    title: String::new(),
                    gender: 1,
                    create_date_unix: 0,
                    fleet_id: None,
                    autopilot_waypoints: Vec::new(),
                    version: 1,
                    system_epoch: 1,
                },
            );
            snap.items.insert(
                ship_id,
                InventoryItemRecord {
                    item_id: ship_id,
                    type_id: 587,
                    owner_id: char_id,
                    location_id: st_id,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.accounts
                .entry("u_9001".to_string())
                .or_insert_with(|| eve_db::AccountRecord {
                    user_id: 9_001,
                    username: "u_9001".into(),
                    password_token: "test".into(),
                    role: eve_db::service_roles::ROLE_PLAYER,
                    banned_until_unix: None,
                    ban_reason: None,
                });
        });
    }

    #[tokio::test]
    async fn execute_session_transition_handles_undock_cross_region_jump_and_structure_dock() {
        let ctx = test_ctx();
        let char_id = 91_001;
        let ship_id = 91_101;
        // Jita IV - Moon 4 Caldari Navy Assembly Plant
        let jita_station = 60_003_760;
        let jita_sys = 30_000_142;
        // Amarr VIII (Oris) in Domain region (cross-region)
        let amarr_sys = 30_002_187;
        let amarr_struct = 1_000_000_091_001;
        seed_test_character_and_ship(&ctx, char_id, ship_id, jita_station);

        ctx.db.with_mut_snapshot(|snap| {
            snap.structures.insert(
                amarr_struct,
                UpwellStructureRecord {
                    structure_id: amarr_struct,
                    type_id: 35_834,
                    owner_corp_id: 98_000_001,
                    solar_system_id: amarr_sys,
                    name: "Amarr Trade Keepstar".into(),
                    position: [1_000.0, 0.0, 0.0],
                    state: "ShieldVulnerable".into(),
                    quantum_core_installed: true,
                    ..Default::default()
                },
            );
        });

        let mut session = SessionState::new(77);
        session.user_id = Some(9_001);
        session.character_id = Some(char_id);
        let ch0 = ctx.db.get_character(char_id).unwrap();
        apply_session_from_character(&mut session, &ch0, None, Some(&ctx.sde));

        // Bind a station service in Jita that must be evicted on undock
        let station_bid = ctx
            .bind_object(&session, "stationSvc", jita_station)
            .expect("bind stationSvc");
        assert!(ctx.verify_bound_object(&session, station_bid).is_ok());

        // 1. Undock in Jita -> spawns ball, clears station_id, sets ship in space, evicts station bound object
        let undock_res = ctx
            .execute_session_transition(
                Some(&mut session),
                SessionTransitionSpec::for_kind(
                    char_id,
                    TransitionKind::Undock {
                        solar_system_id: jita_sys,
                    },
                    "undock",
                )
                .with_arrive_ball_pos(DVec3::new(500.0, 0.0, 0.0)),
            )
            .await
            .expect("undock succeeds");
        assert!(undock_res.diff.contains_key("stationid"));
        assert!(undock_res.diff.contains_key("solarsystemid"));
        assert!(undock_res.diff.contains_key("locationid"));
        assert!(undock_res.arrival_tick.is_some());
        assert_eq!(session.station_id, None);
        assert_eq!(session.solar_system_id, Some(jita_sys));
        assert!(ctx.verify_bound_object(&session, station_bid).is_err());

        let ship_after_undock = ctx.db.with_snapshot(|s| s.items[&ship_id].clone());
        assert_eq!(ship_after_undock.location_id, jita_sys as i64);
        assert_eq!(ship_after_undock.flag, InventoryFlag::None);

        // Bind beyonce in Jita; must be evicted on jump to Amarr
        let jita_beyonce = ctx
            .bind_object(&session, "beyonce", jita_sys as i64)
            .expect("bind jita beyonce");

        // 2. Cross-region jump Jita -> Amarr: updates solar_system_id, constellation_id, region_id, moves ball & item
        let jump_res = ctx
            .execute_session_transition(
                Some(&mut session),
                SessionTransitionSpec::for_kind(
                    char_id,
                    TransitionKind::JumpSystem {
                        solar_system_id: amarr_sys,
                    },
                    "stargate_jump",
                )
                .with_arrive_ball_pos(DVec3::new(2_500.0, 0.0, 0.0)),
            )
            .await
            .expect("jump succeeds");
        assert!(jump_res.removed_departing_ball.is_some());
        assert!(jump_res.diff.contains_key("solarsystemid2"));
        assert!(jump_res.diff.contains_key("constellationid"));
        assert!(jump_res.diff.contains_key("regionid"));
        let amarr_sde = &ctx.sde.solar_systems[&amarr_sys];
        assert_eq!(session.constellation_id, Some(amarr_sde.constellation_id));
        assert_eq!(session.region_id, Some(amarr_sde.region_id));
        assert!(ctx.verify_bound_object(&session, jita_beyonce).is_err());

        let jita_pos = ctx.systems.get_ball_position_sync(jita_sys, ship_id);
        let amarr_pos = ctx.systems.get_ball_position_sync(amarr_sys, ship_id);
        assert!(jita_pos.is_none());
        assert!(amarr_pos.is_some());

        // 3. Dock in Amarr Upwell structure -> removes space ball, updates structure_id & ship Hangar flag
        let dock_res = ctx
            .execute_session_transition(
                Some(&mut session),
                SessionTransitionSpec::for_kind(
                    char_id,
                    TransitionKind::Dock {
                        dock_id: amarr_struct,
                        is_structure: true,
                    },
                    "dock",
                )
                .with_accept_ballpark_dock(amarr_struct, DVec3::new(1_000.0, 0.0, 0.0)),
            )
            .await
            .expect("dock succeeds");
        assert!(dock_res.removed_departing_ball.is_some());
        assert!(dock_res.diff.contains_key("structureid"));
        assert!(dock_res.diff.contains_key("solarsystemid"));
        assert_eq!(session.structure_id, Some(amarr_struct));
        assert_eq!(session.station_id, None);
        assert!(ctx.systems.get_ball_position_sync(amarr_sys, ship_id).is_none());

        let ship_after_dock = ctx.db.with_snapshot(|s| s.items[&ship_id].clone());
        assert_eq!(ship_after_dock.location_id, amarr_struct);
        assert_eq!(ship_after_dock.flag, InventoryFlag::Hangar);
    }

    #[tokio::test]
    async fn execute_session_transition_handles_ship_swap_eject_clone_death_and_invalid_dest() {
        let ctx = test_ctx();
        let char_id = 91_002;
        let ship_a = 91_201;
        let capsule = 91_202;
        let ship_b = 91_203;
        let jita_station = 60_003_760;
        let jita_sys = 30_000_142;
        let amarr_station = 60_008_494;
        let amarr_sys = 30_002_187;
        seed_test_character_and_ship(&ctx, char_id, ship_a, jita_station);

        ctx.db.with_mut_snapshot(|snap| {
            for (id, tid) in [(capsule, 670), (ship_b, 588)] {
                snap.items.insert(
                    id,
                    InventoryItemRecord {
                        item_id: id,
                        type_id: tid,
                        owner_id: char_id,
                        location_id: jita_station,
                        flag: InventoryFlag::Hangar,
                        quantity: 1,
                        singleton: true,
                        version: 1,
                        ..Default::default()
                    },
                );
            }
        });

        let mut session = SessionState::new(88);
        session.user_id = Some(9_001);
        session.character_id = Some(char_id);
        let ch0 = ctx.db.get_character(char_id).unwrap();
        apply_session_from_character(&mut session, &ch0, None, Some(&ctx.sde));

        // 1. Ship swap -> updates active_ship_id & session.ship_id with `shipid` diff
        let board_res = ctx
            .execute_session_transition(
                Some(&mut session),
                SessionTransitionSpec::for_kind(
                    char_id,
                    TransitionKind::BoardShip { ship_id: ship_b },
                    "ship_board",
                ),
            )
            .await
            .expect("board ship");
        assert!(board_res.diff.contains_key("shipid"));
        assert_eq!(session.ship_id, Some(ship_b));

        // 2. Eject to pod -> switches active_ship_id & session.ship_id to capsule
        let eject_res = ctx
            .execute_session_transition(
                Some(&mut session),
                SessionTransitionSpec::for_kind(
                    char_id,
                    TransitionKind::EjectToPod {
                        capsule_id: capsule,
                    },
                    "eject",
                ),
            )
            .await
            .expect("eject to pod");
        assert!(eject_res.diff.contains_key("shipid"));
        assert_eq!(session.ship_id, Some(capsule));

        // 3. Invalid destination system/station is atomically rejected without mutating character or session
        let err = ctx
            .execute_session_transition(
                Some(&mut session),
                SessionTransitionSpec::for_kind(
                    char_id,
                    TransitionKind::JumpSystem {
                        solar_system_id: 999_999,
                    },
                    "stargate_jump",
                ),
            )
            .await
            .expect_err("invalid destination rejected");
        assert_eq!(err.code, "InvalidDestinationLocation");
        assert_eq!(session.solar_system_id, Some(jita_sys));
        assert_eq!(session.station_id, Some(jita_station));

        let err_st = ctx
            .execute_session_transition(
                Some(&mut session),
                SessionTransitionSpec::for_kind(
                    char_id,
                    TransitionKind::Dock {
                        dock_id: 9_999_999_999_999,
                        is_structure: true,
                    },
                    "dock",
                ),
            )
            .await
            .expect_err("nonexistent structure rejected");
        assert_eq!(err_st.code, "InvalidDestinationLocation");
        assert_eq!(session.station_id, Some(jita_station));

        // 4. Death respawn -> moves character & capsule to home station, updates constellation/region
        let death_res = ctx
            .execute_session_transition(
                Some(&mut session),
                SessionTransitionSpec::for_kind(
                    char_id,
                    TransitionKind::DeathRespawn {
                        solar_system_id: amarr_sys,
                        dock_id: Some((amarr_station, false)),
                        ship_id: capsule,
                    },
                    "death_respawn",
                ),
            )
            .await
            .expect("death respawn");
        assert!(death_res.diff.contains_key("solarsystemid2"));
        assert!(death_res.diff.contains_key("stationid"));
        assert_eq!(session.station_id, Some(amarr_station));
        assert_eq!(session.solar_system_id, Some(amarr_sys));
        assert_eq!(
            ctx.db.with_snapshot(|s| s.items[&capsule].location_id),
            amarr_station
        );

        // 5. Logout clears session attributes cleanly
        let logout_res = ctx
            .execute_session_transition(
                Some(&mut session),
                SessionTransitionSpec::for_kind(char_id, TransitionKind::Logout, "logout"),
            )
            .await
            .expect("logout");
        assert!(logout_res.diff.contains_key("solarsystemid2"));
        assert_eq!(session.solar_system_id, None);
        assert_eq!(session.station_id, None);
        assert_eq!(session.ship_id, None);
    }

    #[test]
    fn transition_kind_destination_and_target_ship_id_and_spec_builders() {
        // 1. Exhaustive `TransitionKind::destination_location()` and `target_ship_id()`
        let dock_st = TransitionKind::Dock {
            dock_id: 60_003_760,
            is_structure: false,
        };
        assert_eq!(
            dock_st.destination_location(),
            TransitionLocation::Station {
                solar_system_id: None,
                station_id: 60_003_760,
            }
        );
        assert_eq!(dock_st.target_ship_id(), None);

        let dock_struct = TransitionKind::Dock {
            dock_id: 1_000_000_000_001,
            is_structure: true,
        };
        assert_eq!(
            dock_struct.destination_location(),
            TransitionLocation::Structure {
                solar_system_id: None,
                structure_id: 1_000_000_000_001,
            }
        );
        assert_eq!(dock_struct.target_ship_id(), None);

        let undock = TransitionKind::Undock {
            solar_system_id: 30_000_142,
        };
        assert_eq!(
            undock.destination_location(),
            TransitionLocation::Space {
                solar_system_id: 30_000_142,
            }
        );
        assert_eq!(undock.target_ship_id(), None);

        let jump = TransitionKind::JumpSystem {
            solar_system_id: 30_002_187,
        };
        assert_eq!(
            jump.destination_location(),
            TransitionLocation::Space {
                solar_system_id: 30_002_187,
            }
        );
        assert_eq!(jump.target_ship_id(), None);

        let board = TransitionKind::BoardShip { ship_id: 501 };
        assert_eq!(board.destination_location(), TransitionLocation::KeepCurrent);
        assert_eq!(board.target_ship_id(), Some(501));

        let eject = TransitionKind::EjectToPod { capsule_id: 502 };
        assert_eq!(eject.destination_location(), TransitionLocation::KeepCurrent);
        assert_eq!(eject.target_ship_id(), Some(502));

        for (dock_opt, expected) in [
            (
                Some((60_003_760, false)),
                TransitionLocation::Station {
                    solar_system_id: Some(30_000_142),
                    station_id: 60_003_760,
                },
            ),
            (
                Some((1_000_000_000_002, true)),
                TransitionLocation::Structure {
                    solar_system_id: Some(30_000_142),
                    structure_id: 1_000_000_000_002,
                },
            ),
            (
                None,
                TransitionLocation::Space {
                    solar_system_id: 30_000_142,
                },
            ),
        ] {
            let cj = TransitionKind::CloneJump {
                solar_system_id: 30_000_142,
                dock_id: dock_opt,
                ship_id: Some(601),
            };
            assert_eq!(cj.destination_location(), expected);
            assert_eq!(cj.target_ship_id(), Some(601));

            let tp = TransitionKind::Teleport {
                solar_system_id: 30_000_142,
                dock_id: dock_opt,
                ship_id: None,
            };
            assert_eq!(tp.destination_location(), expected);
            assert_eq!(tp.target_ship_id(), None);

            let login = TransitionKind::CharacterLogin {
                solar_system_id: 30_000_142,
                dock_id: dock_opt,
                ship_id: Some(602),
            };
            assert_eq!(login.destination_location(), expected);
            assert_eq!(login.target_ship_id(), Some(602));

            let death = TransitionKind::DeathRespawn {
                solar_system_id: 30_000_142,
                dock_id: dock_opt,
                ship_id: 603,
            };
            assert_eq!(death.destination_location(), expected);
            assert_eq!(death.target_ship_id(), Some(603));
        }

        assert_eq!(
            TransitionKind::Logout.destination_location(),
            TransitionLocation::ClearAll
        );
        assert_eq!(TransitionKind::Logout.target_ship_id(), None);

        // 2. `SessionTransitionSpec::for_kind` default matrix across all 10 kinds + all 9 `with_*` builders
        let spec_dock = SessionTransitionSpec::for_kind(1, dock_st, "dock");
        assert!(spec_dock.validate_destination);
        assert!(spec_dock.move_ship_item);
        assert!(spec_dock.cleanup_departing_space);
        assert!(spec_dock.deactivate_modules);
        assert!(spec_dock.abort_self_destruct);
        assert!(spec_dock.abandon_drones);
        assert!(!spec_dock.sync_local_chat);
        assert!(spec_dock.invalidate_bounds);
        assert!(spec_dock.sync_quasar_gateway);
        assert!(spec_dock.emit_on_session_changed);

        let spec_login = SessionTransitionSpec::for_kind(
            1,
            TransitionKind::CharacterLogin {
                solar_system_id: 30_000_142,
                dock_id: None,
                ship_id: Some(10),
            },
            "login",
        );
        assert!(!spec_login.validate_destination);
        assert!(!spec_login.move_ship_item);
        assert!(!spec_login.apply_db);
        assert!(spec_login.sync_local_chat);
        assert!(!spec_login.invalidate_bounds);
        assert!(spec_login.sync_quasar_gateway);
        assert!(!spec_login.emit_on_session_changed);

        let spec_logout = SessionTransitionSpec::for_kind(1, TransitionKind::Logout, "logout");
        assert!(!spec_logout.apply_db);
        assert!(spec_logout.invalidate_bounds);
        assert!(!spec_logout.sync_quasar_gateway);
        assert!(!spec_logout.emit_on_session_changed);

        let custom = spec_dock
            .with_apply_db(false)
            .with_move_ship_item(false)
            .with_validate_destination(false)
            .with_emit_on_session_changed(false)
            .with_sync_local_chat(true)
            .with_invalidate_bounds(false)
            .with_cleanup_departing_space(false)
            .with_arrive_ball_pos(DVec3::new(1.0, 2.0, 3.0))
            .with_accept_ballpark_dock(60_003_760, DVec3::new(4.0, 5.0, 6.0))
            .with_preserve_session_ship(true);
        assert!(!custom.apply_db);
        assert!(!custom.move_ship_item);
        assert!(!custom.validate_destination);
        assert!(!custom.emit_on_session_changed);
        assert!(custom.sync_local_chat);
        assert!(!custom.invalidate_bounds);
        assert!(!custom.cleanup_departing_space);
        assert_eq!(custom.arrive_ball_pos, Some(DVec3::new(1.0, 2.0, 3.0)));
        assert!(custom.spawn_destination_ball);
        assert_eq!(
            custom.accept_ballpark_dock,
            Some((60_003_760, DVec3::new(4.0, 5.0, 6.0)))
        );
        assert!(custom.preserve_session_ship);
    }

    #[test]
    fn resolve_destination_and_apply_character_transition_edge_branches() {
        let ctx = test_ctx();
        let char_id = 92_001;
        let ship_id = 92_101;
        let jita_station = 60_003_760;
        seed_test_character_and_ship(&ctx, char_id, ship_id, jita_station);

        // 1. `db_err_to_rpc` maps all 4 branches distinctly.
        assert_eq!(
            db_err_to_rpc(DbError::InvalidDestinationLocation {
                character_id: char_id,
                location_id: 999
            })
            .code,
            "InvalidDestinationLocation"
        );
        assert_eq!(
            db_err_to_rpc(DbError::CharacterNotFound(char_id)).code,
            "CharacterNotFound"
        );
        assert_eq!(
            db_err_to_rpc(DbError::NotDocked(char_id)).code,
            "NotDocked"
        );
        assert_eq!(
            db_err_to_rpc(DbError::ItemNotFound(ship_id)).code,
            "SessionTransitionFailed"
        );

        // 2. `resolve_system_hierarchy` fallback when system is unknown:
        //    non-zero fallback_const/fallback_reg preserved, zero falls back to `(20_000_020, 10_000_002)`.
        assert_eq!(
            resolve_system_hierarchy(&ctx.sde, 999_999_999, 20_000_777, 10_000_888),
            (20_000_777, 10_000_888)
        );
        assert_eq!(
            resolve_system_hierarchy(&ctx.sde, 999_999_999, 0, 0),
            (20_000_020, 10_000_002)
        );

        // 3. `is_known_solar_system`: `0 -> false`, SDE system -> `true`, wormhole `>= 31_000_000 -> true`,
        //    unknown `< 31_000_000 -> false` unless in `snap.system_states`.
        ctx.db.with_snapshot(|snap| {
            assert!(!is_known_solar_system(snap, &ctx.sde, 0));
            assert!(is_known_solar_system(snap, &ctx.sde, 30_000_142));
            assert!(is_known_solar_system(snap, &ctx.sde, 31_000_005));
            assert!(!is_known_solar_system(snap, &ctx.sde, 29_999_999));

            let ch = &snap.characters[&char_id];
            // Space(0) fails even when `validate_destination == false`.
            assert!(matches!(
                resolve_destination_location(
                    snap,
                    &ctx.sde,
                    char_id,
                    ch,
                    TransitionLocation::Space { solar_system_id: 0 },
                    false,
                ),
                Err(DbError::InvalidDestinationLocation { .. })
            ));
            // Station with unknown station_id and `solar_system_id: None` fails even when `validate_destination == false`.
            assert!(matches!(
                resolve_destination_location(
                    snap,
                    &ctx.sde,
                    char_id,
                    ch,
                    TransitionLocation::Station {
                        solar_system_id: None,
                        station_id: 999_999_999,
                    },
                    false,
                ),
                Err(DbError::InvalidDestinationLocation { .. })
            ));
            // Station with unknown station_id fails when `validate_destination == true`, succeeds when `false` with explicit system.
            assert!(resolve_destination_location(
                snap,
                &ctx.sde,
                char_id,
                ch,
                TransitionLocation::Station {
                    solar_system_id: Some(30_000_142),
                    station_id: 999_999_999,
                },
                true,
            )
            .is_err());
            assert!(resolve_destination_location(
                snap,
                &ctx.sde,
                char_id,
                ch,
                TransitionLocation::Station {
                    solar_system_id: Some(30_000_142),
                    station_id: 999_999_999,
                },
                false,
            )
            .is_ok());
        });

        // 4. Destroyed structure rejected when `validate_destination == true`, accepted when `false`.
        let destroyed_struct = 1_000_000_999_001;
        ctx.db.with_mut_snapshot(|snap| {
            snap.structures.insert(
                destroyed_struct,
                UpwellStructureRecord {
                    structure_id: destroyed_struct,
                    type_id: 35_832,
                    owner_corp_id: 98_000_001,
                    solar_system_id: 30_000_142,
                    name: "Destroyed Astrahus".into(),
                    position: [0.0; 3],
                    state: "Destroyed".into(),
                    ..Default::default()
                },
            );
            let ch = snap.characters[&char_id].clone();
            assert!(resolve_destination_location(
                snap,
                &ctx.sde,
                char_id,
                &ch,
                TransitionLocation::Structure {
                    solar_system_id: None,
                    structure_id: destroyed_struct,
                },
                true,
            )
            .is_err());
            assert!(resolve_destination_location(
                snap,
                &ctx.sde,
                char_id,
                &ch,
                TransitionLocation::Structure {
                    solar_system_id: None,
                    structure_id: destroyed_struct,
                },
                false,
            )
            .is_ok());

            // 5. `apply_character_transition_in_snap`:
            // - `KeepCurrent` increments `version` by 1, leaves `system_epoch` and ship item `version` unchanged,
            //   and backfills `constellation_id == 0` / `region_id == 0`.
            snap.characters.get_mut(&char_id).unwrap().constellation_id = 0;
            snap.characters.get_mut(&char_id).unwrap().region_id = 0;
            let v_before = snap.characters[&char_id].version;
            let ep_before = snap.characters[&char_id].system_epoch;
            let ship_v_before = snap.items[&ship_id].version;

            let ch_keep = apply_character_transition_in_snap(
                snap,
                &ctx.sde,
                char_id,
                TransitionLocation::KeepCurrent,
                None,
                true,
                true,
            )
            .unwrap();
            assert_eq!(ch_keep.version, v_before + 1);
            assert_eq!(ch_keep.system_epoch, ep_before);
            assert_ne!(ch_keep.constellation_id, 0);
            assert_ne!(ch_keep.region_id, 0);
            assert_eq!(snap.items[&ship_id].version, ship_v_before);
        });
    }

    #[test]
    fn session_state_helpers_keyval_and_emit_on_session_changed() {
        let ctx = test_ctx();
        let char_id = 93_001;
        let ship_id = 93_101;
        let jita_station = 60_003_760;
        seed_test_character_and_ship(&ctx, char_id, ship_id, jita_station);
        let ch = ctx.db.get_character(char_id).unwrap();

        // 1. `apply_session_from_character` with `override_ship`: `None`, `Some(Some(999))`, `Some(None)`,
        //    plus `ch.active_ship_id == 0 -> ship_id: None`.
        let mut sess = SessionState::new(1);
        apply_session_from_character(&mut sess, &ch, None, Some(&ctx.sde));
        assert_eq!(sess.ship_id, Some(ship_id));
        apply_session_from_character(&mut sess, &ch, Some(Some(777_777)), Some(&ctx.sde));
        assert_eq!(sess.ship_id, Some(777_777));
        apply_session_from_character(&mut sess, &ch, Some(None), Some(&ctx.sde));
        assert_eq!(sess.ship_id, None);

        let mut ch_no_ship = ch.clone();
        ch_no_ship.active_ship_id = 0;
        apply_session_from_character(&mut sess, &ch_no_ship, None, None);
        assert_eq!(sess.ship_id, None);

        // 2. `build_session_state_keyval`:
        // - While docked at station: `solarsystemid` is `PyRep::None`, `solarsystemid2` is `Integer(30_000_142)`,
        //   `locationid` is `Integer(jita_station)`.
        apply_session_from_character(&mut sess, &ch, None, Some(&ctx.sde));
        let kv_docked = build_session_state_keyval(&sess, char_id);
        assert_eq!(kv_docked.dict_get("solarsystemid"), Some(&PyRep::None));
        assert_eq!(
            kv_docked.dict_get("solarsystemid2"),
            Some(&PyRep::Integer(30_000_142))
        );
        assert_eq!(
            kv_docked.dict_get("locationid"),
            Some(&PyRep::Integer(jita_station))
        );

        // - While in space: both `solarsystemid` and `solarsystemid2` are `Integer(30_000_142)`.
        sess.station_id = None;
        sess.structure_id = None;
        let kv_space = build_session_state_keyval(&sess, char_id);
        assert_eq!(
            kv_space.dict_get("solarsystemid"),
            Some(&PyRep::Integer(30_000_142))
        );
        assert_eq!(
            kv_space.dict_get("locationid"),
            Some(&PyRep::Integer(30_000_142))
        );

        // 3. `emit_on_session_changed`: empty diff is a no-op; non-empty diff pushes notification
        //    to both `session` and `ctx.pending_notifications`.
        let empty_diff = HashMap::new();
        let snap_empty = sess.clone();
        emit_on_session_changed(&ctx, Some(&mut sess), &snap_empty, char_id, &empty_diff);
        assert!(sess.drain_notifications().is_empty());
        assert!(ctx.drain_notifications(char_id).is_empty());

        let mut non_empty_diff = HashMap::new();
        non_empty_diff.insert(
            "shipid".to_string(),
            (PyRep::Integer(ship_id), PyRep::Integer(ship_id + 1)),
        );
        let uid = ctx
            .db
            .with_snapshot(|s| s.characters.get(&char_id).map(|c| c.user_id).unwrap_or(1));
        ctx.db
            .acquire_online_lock(uid, 1, Some(char_id), false, ctx.now_unix())
            .unwrap();
        let snap_s = sess.clone();
        emit_on_session_changed(&ctx, Some(&mut sess), &snap_s, char_id, &non_empty_diff);
        assert_eq!(sess.drain_notifications().len(), 1);
        assert_eq!(ctx.drain_notifications(char_id).len(), 1);

        // 4. `execute_session_transition_sync` with `preserve_session_ship = true` preserves `session.ship_id`.
        sess.ship_id = Some(888_888);
        let res_sync = ctx
            .execute_session_transition_sync(
                Some(&mut sess),
                SessionTransitionSpec::for_kind(
                    char_id,
                    TransitionKind::BoardShip { ship_id },
                    "board_preserve",
                )
                .with_preserve_session_ship(true),
            )
            .expect("sync transition succeeds");
        assert_eq!(sess.ship_id, Some(888_888));
        assert!(!res_sync.diff.contains_key("shipid"));
    }
}
