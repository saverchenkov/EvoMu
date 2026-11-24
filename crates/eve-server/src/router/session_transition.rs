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

use super::binding::AtomicSceneTransitionRecord;
use super::notify_registry::{push_typed_session_notification, OnSessionChanged};
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
    /// Canonical variant label for scene-epoch transition tracking (`#1190`).
    pub const fn label(self) -> &'static str {
        match self {
            Self::Dock { .. } => "Dock",
            Self::Undock { .. } => "Undock",
            Self::JumpSystem { .. } => "JumpSystem",
            Self::BoardShip { .. } => "BoardShip",
            Self::EjectToPod { .. } => "EjectToPod",
            Self::CloneJump { .. } => "CloneJump",
            Self::DeathRespawn { .. } => "DeathRespawn",
            Self::Teleport { .. } => "Teleport",
            Self::CharacterLogin { .. } => "CharacterLogin",
            Self::Logout => "Logout",
        }
    }

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
    pub close_active_trades: bool,
    pub clear_completed_safe_logoff: bool,
    pub emit_system_wide_effect_stop: bool,
}

impl SessionTransitionSpec {
    fn minimal_defaults(char_id: i64, kind: TransitionKind, reason: &'static str) -> Self {
        Self {
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
            invalidate_bounds: false,
            sync_quasar_gateway: false,
            emit_on_session_changed: false,
            arrive_ball_pos: None,
            spawn_destination_ball: false,
            accept_ballpark_dock: None,
            preserve_session_ship: false,
            close_active_trades: true,
            clear_completed_safe_logoff: true,
            emit_system_wide_effect_stop: true,
        }
    }

    pub fn for_kind(char_id: i64, kind: TransitionKind, reason: &'static str) -> Self {
        let base = Self::minimal_defaults(char_id, kind, reason);
        match kind {
            TransitionKind::Dock { .. } => Self {
                validate_destination: true,
                move_ship_item: true,
                apply_db: true,
                cleanup_departing_space: true,
                deactivate_modules: true,
                abort_self_destruct: true,
                abandon_drones: true,
                invalidate_bounds: true,
                sync_quasar_gateway: true,
                emit_on_session_changed: true,
                ..base
            },
            TransitionKind::Undock { .. } => Self {
                validate_destination: true,
                move_ship_item: true,
                apply_db: true,
                invalidate_bounds: true,
                sync_quasar_gateway: true,
                emit_on_session_changed: true,
                ..base
            },
            TransitionKind::JumpSystem { .. } | TransitionKind::DeathRespawn { .. } => Self {
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
                ..base
            },
            TransitionKind::BoardShip { .. } | TransitionKind::EjectToPod { .. } => Self {
                apply_db: true,
                abort_self_destruct: true,
                invalidate_bounds: true,
                sync_quasar_gateway: true,
                emit_on_session_changed: true,
                ..base
            },
            TransitionKind::CloneJump { .. } => Self {
                validate_destination: true,
                apply_db: true,
                cleanup_departing_space: true,
                deactivate_modules: true,
                abort_self_destruct: true,
                abandon_drones: true,
                sync_local_chat: true,
                invalidate_bounds: true,
                sync_quasar_gateway: true,
                emit_on_session_changed: true,
                ..base
            },
            TransitionKind::Teleport { .. } => Self {
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
                ..base
            },
            TransitionKind::CharacterLogin { .. } => Self {
                sync_local_chat: true,
                sync_quasar_gateway: true,
                ..base
            },
            TransitionKind::Logout => Self {
                invalidate_bounds: true,
                ..base
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

    pub fn with_close_active_trades(mut self, close: bool) -> Self {
        self.close_active_trades = close;
        self
    }

    pub fn with_clear_completed_safe_logoff(mut self, clear: bool) -> Self {
        self.clear_completed_safe_logoff = clear;
        self
    }

    pub fn with_emit_system_wide_effect_stop(mut self, emit: bool) -> Self {
        self.emit_system_wide_effect_stop = emit;
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
    pub scene_epoch: u64,
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
        false,
    )
    .map(|(ch, _)| ch)
}

#[allow(clippy::too_many_arguments)]
fn apply_character_transition_in_snap_with_trades(
    snap: &mut WorldStateSnapshot,
    sde: &SdeRegistry,
    char_id: i64,
    location: TransitionLocation,
    new_ship_id: Option<i64>,
    move_ship_item: bool,
    validate_destination: bool,
    close_active_trades: bool,
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

    let cancelled_trades = if close_active_trades {
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

/// Resolves a valid `(bloodline_id, race_id)` pair for `ch`, deriving/falling back via `sde` if
/// `bloodline_id == 0` or unknown in a populated SDE.
pub(crate) fn resolve_character_bloodline_and_race(
    ch: &CharacterRecord,
    sde: Option<&SdeRegistry>,
) -> (u32, u32) {
    let valid_in_sde = sde.map_or(true, |s| {
        s.bloodlines.is_empty() || s.bloodlines.contains_key(&ch.bloodline_id)
    });
    if ch.bloodline_id > 0 && valid_in_sde {
        let race_id = if ch.race_id > 0 {
            ch.race_id
        } else {
            sde.and_then(|s| s.bloodlines.get(&ch.bloodline_id))
                .map(|b| b.race_id)
                .unwrap_or(1)
        };
        return (ch.bloodline_id, race_id);
    }
    if let Some(sde) = sde.filter(|s| !s.bloodlines.is_empty()) {
        let picked = (ch.race_id > 0)
            .then(|| {
                sde.bloodlines
                    .values()
                    .filter(|b| b.race_id == ch.race_id)
                    .min_by_key(|b| b.bloodline_id)
            })
            .flatten()
            .or_else(|| {
                sde.bloodlines
                    .values()
                    .filter(|b| b.race_id == 1)
                    .min_by_key(|b| b.bloodline_id)
            })
            .or_else(|| sde.bloodlines.values().min_by_key(|b| b.bloodline_id));
        if let Some(bl) = picked {
            return (bl.bloodline_id, bl.race_id);
        }
    }
    (
        if ch.bloodline_id > 0 { ch.bloodline_id } else { 1 },
        if ch.race_id > 0 { ch.race_id } else { 1 },
    )
}

/// Clears all character/spatial attributes on a `SessionState` (used on logoff / character clear).
pub fn clear_session_character_state(session: &mut SessionState) {
    session.station_id = None;
    session.structure_id = None;
    session.solar_system_id = None;
    session.constellation_id = None;
    session.region_id = None;
    session.ship_id = None;
    session.race_id = None;
    session.bloodline_id = None;
    session.gender_id = None;
}

/// Synchronizes all spatial, active-ship, and character-identity attributes on `SessionState` from a `CharacterRecord`.
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
    let (bloodline_id, race_id) = resolve_character_bloodline_and_race(ch, sde);
    session.race_id = Some(race_id);
    session.bloodline_id = Some(bloodline_id);
    session.gender_id = Some(ch.gender);
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
    let active_sys = if session.station_id.is_none() {
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
        ("solarsystemid", active_sys.unwrap_or(PyRep::None)),
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
        (
            "raceID",
            session
                .race_id
                .map(|r| PyRep::Integer(i64::from(r)))
                .unwrap_or(PyRep::None),
        ),
        (
            "bloodlineID",
            session
                .bloodline_id
                .map(|b| PyRep::Integer(i64::from(b)))
                .unwrap_or(PyRep::None),
        ),
        (
            "genderID",
            session
                .gender_id
                .map(|g| PyRep::Integer(i64::from(g)))
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
    let notif = OnSessionChanged {
        is_remote: false,
        session: sess_rep,
        change: PyRep::Dict(diff_entries),
    };
    if let Some(sess) = session.as_deref_mut() {
        push_typed_session_notification(sess, notif.clone());
    }
    ctx.emit_typed(NotificationTarget::Character(char_id), notif);
    if diff.keys().any(|k| {
        matches!(
            k.as_str(),
            "shipid" | "solarsystemid" | "solarsystemid2" | "stationid" | "structureid"
        )
    }) {
        let in_space = snap_session.station_id.is_none() && snap_session.structure_id.is_none();
        crate::services::notify_emitters_tournaments_anomalies::emit_update_system_wide_effects_info(
            ctx,
            char_id,
            snap_session.solar_system_id,
            in_space,
        );
    }
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
        ctx.sync_transition_grace_on_session_change(&prev, session);
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

fn finalize_transition_session_and_epoch(
    ctx: &ServerContext,
    mut session: Option<&mut SessionState>,
    prev_session: &SessionState,
    updated_ch: Option<&CharacterRecord>,
    departing: &super::teardown_registry::DepartingSceneSnapshot,
    spec: &SessionTransitionSpec,
) -> (HashMap<String, (PyRep, PyRep)>, u64) {
    let override_ship = (spec.preserve_session_ship && prev_session.ship_id.is_some())
        .then_some(prev_session.ship_id);
    let next_snapshot = if let Some(sess) = session.as_deref_mut() {
        match updated_ch {
            Some(ch) => apply_session_from_character(sess, ch, override_ship, Some(&ctx.sde)),
            None => clear_session_character_state(sess),
        }
        sess.clone()
    } else {
        let mut synth = prev_session.clone();
        match updated_ch {
            Some(ch) => apply_session_from_character(&mut synth, ch, override_ship, Some(&ctx.sde)),
            None => clear_session_character_state(&mut synth),
        }
        synth
    };
    let (target_sid, target_uid) = if session.is_some() {
        (next_snapshot.session_id, next_snapshot.user_id)
    } else {
        ctx.online_session_id_and_user_for_char(spec.char_id)
            .map(|(sid, uid)| (sid, Some(uid)))
            .unwrap_or_else(|| (0, next_snapshot.user_id.or(prev_session.user_id)))
    };
    let trigger = super::teardown_registry::TransitionTrigger::from_kind(spec.kind);
    let removed = if spec.invalidate_bounds
        && super::teardown_registry::policy_matches(
            super::teardown_registry::TeardownSubsystem::BoundObjects,
            trigger,
            super::teardown_registry::TeardownHookAction::InvalidateScopedBoundObjects,
        )
    {
        let eff_sys = next_snapshot.solar_system_id.or(prev_session.solar_system_id).unwrap_or(0);
        let active_dock = next_snapshot.station_id.or(next_snapshot.structure_id);
        ctx.collect_invalidated_bound_objects_for_session_transition(spec.char_id, eff_sys, active_dock, spec.reason)
    } else {
        Vec::new()
    };
    let is_logout = matches!(spec.kind, TransitionKind::Logout) || spec.reason == "logout";
    let is_login = matches!(spec.kind, TransitionKind::CharacterLogin { .. }) || spec.reason == "login";
    let can_record_departed = !is_logout && !is_login && target_sid != 0
        && target_uid.is_some_and(|uid| ctx.has_matching_online_session_ids(target_sid, uid, spec.char_id));
    let scene_epoch = ctx.record_atomic_session_transition(AtomicSceneTransitionRecord {
        session_id: target_sid,
        user_id: target_uid,
        char_id: spec.char_id,
        kind_label: spec.kind.label(),
        reason: spec.reason,
        is_logout,
        is_login,
        prev_sys: prev_session.solar_system_id.filter(|&id| id != 0),
        prev_dock: prev_session.station_id.or(prev_session.structure_id),
        next_sys: next_snapshot.solar_system_id,
        next_dock: next_snapshot.station_id.or(next_snapshot.structure_id),
        can_record_departed,
        removed_bindings: removed.clone(),
    });
    for entry in &removed {
        ctx.emit_macho_object_disconnect(entry);
    }
    if spec.sync_quasar_gateway {
        ctx.quasar_gateway.on_session_changed(&next_snapshot, ctx.now_unix());
    }
    let diff = prev_session.compute_diff(&next_snapshot);
    let mut emitted_to_char = false;
    if spec.emit_on_session_changed && !diff.is_empty() {
        if let Some(sess) = session {
            if let Some(cid) = sess.character_id.or_else(|| updated_ch.map(|c| c.character_id)) {
                emit_on_session_changed(ctx, Some(sess), &next_snapshot, cid, &diff);
                emitted_to_char = true;
            }
        } else {
            emit_on_session_changed(ctx, None, &next_snapshot, spec.char_id, &diff);
            emitted_to_char = true;
        }
    }
    super::teardown_registry::run_post_finalize_system_wide_sync(
        ctx,
        spec,
        departing,
        updated_ch,
        &diff,
        emitted_to_char,
    );
    (diff, scene_epoch)
}

fn prepare_transition_inputs(
    ctx: &ServerContext,
    session_opt: Option<&SessionState>,
    spec: &SessionTransitionSpec,
) -> (
    Option<CharacterRecord>,
    SessionState,
    super::teardown_registry::DepartingSceneSnapshot,
) {
    let pre_ch = ctx.db.get_character(spec.char_id).ok();
    let has_live_session = session_opt.is_some();
    let prev_session = match session_opt {
        Some(s) => s.clone(),
        None => match pre_ch.as_ref() {
            Some(ch) => synthetic_session_from_character(spec.char_id, ch, None, &ctx.sde),
            None => SessionState::new(0),
        },
    };
    let departing = super::teardown_registry::resolve_departing_scene(
        &prev_session,
        pre_ch.as_ref(),
        has_live_session,
        spec,
    );
    (pre_ch, prev_session, departing)
}

fn validate_transition_destination(
    ctx: &ServerContext,
    spec: &SessionTransitionSpec,
    pre_ch: Option<&CharacterRecord>,
) -> Result<(), BridgeRpcError> {
    if !spec.apply_db || !spec.validate_destination {
        return Ok(());
    }
    if let Some(ch) = pre_ch {
        ctx.db
            .with_snapshot(|snap| {
                resolve_destination_location(
                    snap,
                    &ctx.sde,
                    spec.char_id,
                    ch,
                    spec.kind.destination_location(),
                    true,
                )
            })
            .map_err(db_err_to_rpc)?;
    }
    Ok(())
}

fn apply_transition_db_and_trades(
    ctx: &ServerContext,
    spec: &SessionTransitionSpec,
) -> Result<Option<CharacterRecord>, BridgeRpcError> {
    let dest_loc = spec.kind.destination_location();
    let target_ship = spec.kind.target_ship_id();
    if matches!(spec.kind, TransitionKind::Logout) {
        super::teardown_registry::run_non_db_trade_teardown_sync(ctx, spec);
        return Ok(None);
    }
    if spec.apply_db {
        let cancel_trades = spec.close_active_trades
            && super::teardown_registry::policy_matches(
                super::teardown_registry::TeardownSubsystem::TradeSessions,
                super::teardown_registry::TransitionTrigger::from_kind(spec.kind),
                super::teardown_registry::TeardownHookAction::CancelOpenTrades,
            );
        let (ch, cancelled_trades) = ctx
            .db
            .try_with_mut_snapshot(|snap| {
                apply_character_transition_in_snap_with_trades(
                    snap,
                    &ctx.sde,
                    spec.char_id,
                    dest_loc,
                    target_ship,
                    spec.move_ship_item,
                    spec.validate_destination,
                    cancel_trades,
                )
            })
            .map_err(db_err_to_rpc)?;
        super::teardown_registry::emit_cancelled_trade_notifications(ctx, &cancelled_trades);
        return Ok(Some(ch));
    }
    super::teardown_registry::run_non_db_trade_teardown_sync(ctx, spec);
    let mut ch_opt = ctx.db.get_character(spec.char_id).ok();
    if !matches!(spec.kind, TransitionKind::CharacterLogin { .. }) {
        if let Some(ref mut ch) = ch_opt {
            apply_spec_destination_to_character_copy(&ctx.sde, ch, dest_loc, target_ship);
        }
    }
    Ok(ch_opt)
}

async fn spawn_destination_ball_if_needed(
    ctx: &ServerContext,
    spec: &SessionTransitionSpec,
    updated_ch: Option<&CharacterRecord>,
    removed_ball: Option<&Ball>,
) -> Option<u64> {
    if !spec.spawn_destination_ball {
        return None;
    }
    let ch = updated_ch?;
    if ch.station_id.is_some() || ch.structure_id.is_some() || ch.active_ship_id <= 0 {
        return None;
    }
    let ship_id = ch.active_ship_id;
    let ship_item = ctx.db.with_snapshot(|snap| snap.items.get(&ship_id).cloned());
    let type_id = ship_item.as_ref().map(|it| it.type_id).unwrap_or(670);
    let mut ball = removed_ball.cloned().unwrap_or_else(|| {
        let radius = ctx
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
    let combat_state = ctx.build_ship_combat_state(spec.char_id, ship_id, type_id);
    let slim_meta = ctx.build_ship_slim_meta(spec.char_id, type_id);
    let dest_handle = ctx.systems.get_or_spawn(ch.solar_system_id);
    let arrival_tick = dest_handle
        .spawn_ball_with_meta(ball, combat_state, slim_meta)
        .await
        .ok();
    let _ = dest_handle.push_set_state(ship_id).await;
    arrival_tick
}

impl ServerContext {
    /// Executes the atomic `SessionTransition` pipeline synchronously (for sync RPC handlers,
    /// combat ticks, login/logout, and unit tests without async runtime overhead).
    pub fn execute_session_transition_sync(
        &self,
        mut session: Option<&mut SessionState>,
        spec: SessionTransitionSpec,
    ) -> Result<SessionTransitionResult, BridgeRpcError> {
        let (pre_ch, prev_session, departing) =
            prepare_transition_inputs(self, session.as_deref(), &spec);
        validate_transition_destination(self, &spec, pre_ch.as_ref())?;
        super::teardown_registry::run_pre_transition_teardown_sync(self, &spec, &departing);
        let updated_ch = apply_transition_db_and_trades(self, &spec)?;
        super::teardown_registry::run_post_db_pre_finalize_hooks_sync(
            self,
            &spec,
            &departing,
            updated_ch.as_ref(),
        );
        let (diff, scene_epoch) = finalize_transition_session_and_epoch(
            self,
            session.as_deref_mut(),
            &prev_session,
            updated_ch.as_ref(),
            &departing,
            &spec,
        );
        Ok(SessionTransitionResult {
            updated_character: updated_ch,
            diff,
            removed_departing_ball: None,
            arrival_tick: None,
            scene_epoch,
        })
    }

    /// Executes the atomic `SessionTransition` pipeline asynchronously, including `Ballpark`
    /// removal (`remove_ball` / `accept_docking` / `clear_target_locks`) and destination spawn.
    pub async fn execute_session_transition(
        &self,
        mut session: Option<&mut SessionState>,
        spec: SessionTransitionSpec,
    ) -> Result<SessionTransitionResult, BridgeRpcError> {
        let (pre_ch, prev_session, departing) =
            prepare_transition_inputs(self, session.as_deref(), &spec);
        validate_transition_destination(self, &spec, pre_ch.as_ref())?;
        super::teardown_registry::run_pre_transition_teardown_sync(self, &spec, &departing);

        let mut removed_ball = None;
        if super::teardown_registry::should_run_departing_ballpark_cleanup(&spec, &departing) {
            let old_handle = self.systems.get_or_spawn(departing.sys_id);
            let _ = old_handle.clear_target_locks(departing.ship_id).await;
            removed_ball = if let Some((dock_id, dock_pos)) = spec.accept_ballpark_dock {
                old_handle
                    .accept_docking(departing.ship_id, dock_id, dock_pos)
                    .await
            } else {
                old_handle.remove_ball(departing.ship_id).await
            };
        }

        let updated_ch = apply_transition_db_and_trades(self, &spec)?;
        let arrival_tick = spawn_destination_ball_if_needed(
            self,
            &spec,
            updated_ch.as_ref(),
            removed_ball.as_ref(),
        )
        .await;
        super::teardown_registry::run_post_db_pre_finalize_hooks_sync(
            self,
            &spec,
            &departing,
            updated_ch.as_ref(),
        );
        let (diff, scene_epoch) = finalize_transition_session_and_epoch(
            self,
            session.as_deref_mut(),
            &prev_session,
            updated_ch.as_ref(),
            &departing,
            &spec,
        );

        Ok(SessionTransitionResult {
            updated_character: updated_ch,
            diff,
            removed_departing_ball: removed_ball,
            arrival_tick,
            scene_epoch,
        })
    }
}

/// Stargate session-change cooldown stays fixed in wall-clock seconds regardless of TiDi.
pub const STARGATE_SESSION_CHANGE_TIMER_SECS: u64 = 7;

pub type SessionChangeTimerState = eve_db::SessionChangeTimerRecord;

pub fn check_session_change_timer_active(
    ctx: &ServerContext,
    char_id: i64,
    now_unix: u64,
    current_tick_opt: Option<(u32, u64)>,
) -> Option<SessionChangeTimerState> {
    ctx.db.with_snapshot(|snap| {
        let timer = snap.sp6_lifecycle.session_change_timers.get(&char_id)?;
        if now_unix >= timer.expires_unix {
            return None;
        }
        // Stargate jump session-change timer (7 s) is wall-clock governed and does not
        // expire early on system tick count or stretch under time dilation.
        if timer.reason != "stargate_jump" {
            if let Some((sys_id, cur_tick)) = current_tick_opt {
                if timer.started_tick > 0
                    && sys_id == timer.system_id
                    && cur_tick >= timer.started_tick.saturating_add(10)
                {
                    return None;
                }
            }
        }
        Some(timer.clone())
    })
}

pub fn get_session_change_timer(
    ctx: &ServerContext,
    char_id: i64,
) -> Option<SessionChangeTimerState> {
    ctx.db.with_snapshot(|snap| {
        snap.sp6_lifecycle
            .session_change_timers
            .get(&char_id)
            .cloned()
    })
}

pub fn clear_session_change_timer(ctx: &ServerContext, char_id: i64) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.sp6_lifecycle.session_change_timers.remove(&char_id);
    });
}

#[cfg(any(test, feature = "testkit"))]
pub fn clear_all_session_change_timers(ctx: &ServerContext) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.sp6_lifecycle.session_change_timers.clear();
    });
}

#[cfg(any(test, feature = "testkit"))]
pub fn seed_session_change_timer(
    ctx: &ServerContext,
    char_id: i64,
    state: SessionChangeTimerState,
) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.sp6_lifecycle
            .session_change_timers
            .insert(char_id, state);
    });
}

pub fn record_session_change_timer(
    ctx: &ServerContext,
    char_id: i64,
    now_unix: u64,
    system_id: u32,
    started_tick: u64,
    reason: &str,
) {
    record_session_change_timer_impl(
        ctx,
        char_id,
        now_unix,
        system_id,
        started_tick,
        reason,
        true,
    );
}

pub fn record_client_session_check_timer(
    ctx: &ServerContext,
    char_id: i64,
    now_unix: u64,
    system_id: u32,
    started_tick: u64,
    reason: &str,
) {
    record_session_change_timer_impl(
        ctx,
        char_id,
        now_unix,
        system_id,
        started_tick,
        reason,
        false,
    );
}

fn record_session_change_timer_impl(
    ctx: &ServerContext,
    char_id: i64,
    now_unix: u64,
    system_id: u32,
    started_tick: u64,
    reason: &str,
    server_initiated: bool,
) {
    let duration_secs = if reason == "stargate_jump" {
        STARGATE_SESSION_CHANGE_TIMER_SECS
    } else {
        10
    };
    let expires_unix = now_unix.saturating_add(duration_secs);
    let (change_dict, sess_rep, has_entries, active_dock_id) =
        ctx.db.with_mut_snapshot(|snap| {
            snap.sp6_lifecycle.session_change_timers.insert(
                char_id,
                SessionChangeTimerState {
                    expires_unix,
                    system_id,
                    started_tick,
                    reason: reason.to_string(),
                },
            );
            let ch = snap.characters.get(&char_id);
            let station_id = ch.and_then(|c| c.station_id);
            let structure_id = ch.and_then(|c| c.structure_id);
            let ship_id = ch.map(|c| c.active_ship_id);
            let user_id = ch.map(|c| c.user_id as i64);
            let corp_id = ch.map(|c| c.corporation_id);
            let alliance_id = ch.and_then(|c| c.alliance_id);
            let mut entries = vec![];
            if system_id != 0
                && !matches!(reason, "set_account_key" | "corp_change" | "ship_loss")
            {
                entries.push((
                    PyRep::String("solarsystemid2".into()),
                    PyRep::Tuple(vec![PyRep::None, PyRep::Integer(i64::from(system_id))]),
                ));
            }
            let has_solarsystem = station_id.is_none();
            if station_id.is_some() || reason == "dock" || reason == "undock" {
                entries.push((
                    PyRep::String("stationid".into()),
                    PyRep::Tuple(vec![
                        PyRep::None,
                        station_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                    ]),
                ));
                if station_id.is_some() || (structure_id.is_none() && reason == "undock") {
                    entries.push((
                        PyRep::String("solarsystemid".into()),
                        PyRep::Tuple(vec![
                            station_id
                                .map(|_| PyRep::Integer(i64::from(system_id)))
                                .unwrap_or(PyRep::None),
                            if station_id.is_some() {
                                PyRep::None
                            } else {
                                PyRep::Integer(i64::from(system_id))
                            },
                        ]),
                    ));
                }
            } else if has_solarsystem
                && system_id != 0
                && !matches!(reason, "set_account_key" | "corp_change" | "ship_loss")
            {
                entries.push((
                    PyRep::String("solarsystemid".into()),
                    PyRep::Tuple(vec![PyRep::None, PyRep::Integer(i64::from(system_id))]),
                ));
            }
            if let Some(st_id) = structure_id {
                entries.push((
                    PyRep::String("structureid".into()),
                    PyRep::Tuple(vec![PyRep::None, PyRep::Integer(st_id)]),
                ));
            }
            if let Some(sid) = ship_id {
                entries.push((
                    PyRep::String("shipid".into()),
                    PyRep::Tuple(vec![PyRep::None, PyRep::Integer(sid)]),
                ));
            }
            let sess = PyRep::keyval([
                ("charid", PyRep::Integer(char_id)),
                ("userid", user_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
                ("corpid", corp_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
                (
                    "allianceid",
                    alliance_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                ),
                (
                    "stationid",
                    station_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                ),
                (
                    "structureid",
                    structure_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                ),
                ("solarsystemid2", PyRep::Integer(i64::from(system_id))),
                (
                    "solarsystemid",
                    if has_solarsystem {
                        PyRep::Integer(i64::from(system_id))
                    } else {
                        PyRep::None
                    },
                ),
                ("shipid", ship_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
                ("nextSessionChange", PyRep::Integer(expires_unix as i64)),
            ]);
            let has = !entries.is_empty();
            (PyRep::Dict(entries), sess, has, station_id.or(structure_id))
        });
    if server_initiated {
        ctx.invalidate_bound_objects_for_session_transition(
            char_id,
            system_id,
            active_dock_id,
            reason,
        );
    }
    if reason == "board_or_eject" || !has_entries {
        return;
    }
    ctx.emit_typed(
        NotificationTarget::Character(char_id),
        OnSessionChanged {
            is_remote: false,
            session: sess_rep,
            change: change_dict,
        },
    );
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
        assert!(!dock_res.diff.contains_key("solarsystemid"));
        assert_eq!(session.structure_id, Some(amarr_struct));
        assert_eq!(session.station_id, None);
        assert_eq!(session.effective_location_id(), Some(i64::from(amarr_sys)));
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
        assert_eq!(sess.race_id, Some(1));
        assert_eq!(sess.bloodline_id, Some(1));
        assert_eq!(sess.gender_id, Some(1));
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
        assert_eq!(kv_docked.dict_get("raceID"), Some(&PyRep::Integer(1)));
        assert_eq!(kv_docked.dict_get("bloodlineID"), Some(&PyRep::Integer(1)));
        assert_eq!(kv_docked.dict_get("genderID"), Some(&PyRep::Integer(1)));

        let mut ch_missing_bl = ch.clone();
        ch_missing_bl.bloodline_id = 0;
        ch_missing_bl.race_id = 2;
        ch_missing_bl.gender = 0;
        let (fallback_bl, fallback_race) =
            resolve_character_bloodline_and_race(&ch_missing_bl, Some(&ctx.sde));
        assert!(fallback_bl > 0);
        assert_eq!(fallback_race, 2);
        apply_session_from_character(&mut sess, &ch_missing_bl, None, Some(&ctx.sde));
        assert_eq!(sess.bloodline_id, Some(fallback_bl));
        assert_eq!(sess.race_id, Some(2));
        assert_eq!(sess.gender_id, Some(0));

        clear_session_character_state(&mut sess);
        assert_eq!(sess.race_id, None);
        assert_eq!(sess.bloodline_id, None);
        assert_eq!(sess.gender_id, None);
        apply_session_from_character(&mut sess, &ch, None, Some(&ctx.sde));

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
        let char_notifs = ctx.drain_notifications(char_id);
        let char_events: Vec<&str> = char_notifs.iter().map(|n| n.event.as_str()).collect();
        assert_eq!(char_events, vec!["OnSessionChanged", "OnUpdateSystemWideEffectsInfo"]);

        // Transitioning into space in Jita emits empty `OnUpdateSystemWideEffectsInfo` so stale
        // system-wide effect icons from prior systems never linger (`#1122`).
        let sys_diff = HashMap::from([(
            "solarsystemid".to_string(),
            (PyRep::None, PyRep::Integer(30_000_142)),
        )]);
        let mut space_jita = sess.clone();
        space_jita.solar_system_id = Some(30_000_142);
        space_jita.station_id = None;
        space_jita.structure_id = None;
        emit_on_session_changed(&ctx, None, &space_jita, char_id, &sys_diff);
        let jita_notifs = ctx.drain_notifications(char_id);
        let sw_jita = jita_notifs
            .iter()
            .find(|n| n.event == "OnUpdateSystemWideEffectsInfo")
            .expect("OnUpdateSystemWideEffectsInfo on solar system jump");
        assert_eq!(sw_jita.args, vec![PyRep::Dict(Vec::new())]);

        // Transitioning into a wormhole with a valid `infoBubbles` phenomenon populates
        // `OnUpdateSystemWideEffectsInfo`, and docking clears it back to `{}`.
        if let Some((&wh_sys_id, _)) = ctx
            .sde
            .solar_systems
            .iter()
            .find(|(&sid, _)| eve_dogma::resolve_system_wormhole_phenomenon(&ctx.sde, sid, None).is_some())
        {
            let mut space_wh = space_jita.clone();
            space_wh.solar_system_id = Some(wh_sys_id);
            let wh_diff = HashMap::from([(
                "solarsystemid".to_string(),
                (PyRep::Integer(30_000_142), PyRep::Integer(i64::from(wh_sys_id))),
            )]);
            emit_on_session_changed(&ctx, None, &space_wh, char_id, &wh_diff);
            let wh_notifs = ctx.drain_notifications(char_id);
            let sw_wh = wh_notifs
                .iter()
                .find(|n| n.event == "OnUpdateSystemWideEffectsInfo")
                .expect("OnUpdateSystemWideEffectsInfo in wormhole");
            assert!(
                matches!(sw_wh.args.first(), Some(PyRep::Dict(entries)) if !entries.is_empty()),
                "wormhole transition must populate systemWideEffectsOnShip: {:?}",
                sw_wh.args
            );

            let dock_diff = HashMap::from([
                ("stationid".to_string(), (PyRep::None, PyRep::Integer(jita_station))),
                (
                    "solarsystemid".to_string(),
                    (PyRep::Integer(i64::from(wh_sys_id)), PyRep::None),
                ),
            ]);
            let mut docked_sess = space_wh;
            docked_sess.station_id = Some(jita_station);
            emit_on_session_changed(&ctx, None, &docked_sess, char_id, &dock_diff);
            let dock_notifs = ctx.drain_notifications(char_id);
            let sw_dock = dock_notifs
                .iter()
                .find(|n| n.event == "OnUpdateSystemWideEffectsInfo")
                .expect("OnUpdateSystemWideEffectsInfo on dock");
            assert_eq!(sw_dock.args, vec![PyRep::Dict(Vec::new())]);
        }

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
