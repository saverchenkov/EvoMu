//! Typed V24 rejection metadata (`RejectionFacts`) and [`BridgeRpcError`] constructors (`#1195`).
//!
//! Carries structured game-state rejection facts inside [`ClientError::Rejected`] alongside
//! typed V24 `eveexceptions.UserError` keys and dict placeholders, enabling Playtester and tests
//! to assert rejection preconditions against game state without scraping `.message`.

use super::{round_user_error_real_6dp, ClientError, IntoUserErrorArg, UserErrorArg, UserErrorKey};
use crate::BridgeRpcError;
use serde::{Deserialize, Serialize};

/// Warp phases governing `#1454` active-warp vs warp-restricted rejection rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum WarpPhaseKind {
    Aligning,
    Accelerating,
    Cruising,
    Decelerating,
}

impl WarpPhaseKind {
    /// Canonical warp phase label stored in [`RejectionFacts::ShipInWarp`].
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Aligning => "Aligning",
            Self::Accelerating => "Accelerating",
            Self::Cruising => "Cruising",
            Self::Decelerating => "Decelerating",
        }
    }

    /// Returns `true` when active warp blocks navigation overrides, non-warp-safe module
    /// activations, Doomsdays, fighter abilities, and direct `Eject` / `BoardShip` (`Aligning` allowed).
    pub const fn blocks_active_warp_actions(&self) -> bool {
        !matches!(self, Self::Aligning)
    }

    /// Returns `true` when any warp phase (including `Aligning`) blocks restricted warp-entry,
    /// jump, filament, deployable MJU, or bay-boarding transitions (`#1454`).
    pub const fn blocks_warp_restricted_actions(&self) -> bool {
        true
    }

    /// Returns the V24 navigation `UserErrorKey` (`ShipInWarp`) for blocking active-warp phases (`#1454`).
    pub const fn active_warp_error_key(&self) -> Option<UserErrorKey> {
        if self.blocks_active_warp_actions() {
            Some(UserErrorKey::ShipInWarp)
        } else {
            None
        }
    }

    /// Returns the V24 navigation `UserErrorKey` (`ShipInWarp`) for restricted jump/deploy/bay ops.
    pub const fn restricted_warp_error_key(&self) -> UserErrorKey {
        UserErrorKey::ShipInWarp
    }

    /// Returns the V24 module-activation `UserErrorKey` (`DeniedActivateInWarp`) for active-warp phases (`#1454`).
    pub const fn active_module_error_key(&self) -> Option<UserErrorKey> {
        if self.blocks_active_warp_actions() {
            Some(UserErrorKey::DeniedActivateInWarp)
        } else {
            None
        }
    }
}

/// Canonical `#1454` per-phase rejection policy row for active-warp vs warp-restricted ops.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WarpPhasePolicy {
    pub phase: WarpPhaseKind,
    pub blocks_active_warp: bool,
    pub blocks_restricted_ops: bool,
    pub active_warp_key: Option<UserErrorKey>,
    pub active_module_key: Option<UserErrorKey>,
    pub restricted_op_key: UserErrorKey,
}

/// Canonical `#1454` warp-phase policy table across all four [`WarpPhaseKind`] states.
pub const WARP_PHASE_REJECTION_TABLE: &[WarpPhasePolicy] = &[
    WarpPhasePolicy {
        phase: WarpPhaseKind::Aligning,
        blocks_active_warp: false,
        blocks_restricted_ops: true,
        active_warp_key: None,
        active_module_key: None,
        restricted_op_key: UserErrorKey::ShipInWarp,
    },
    WarpPhasePolicy {
        phase: WarpPhaseKind::Accelerating,
        blocks_active_warp: true,
        blocks_restricted_ops: true,
        active_warp_key: Some(UserErrorKey::ShipInWarp),
        active_module_key: Some(UserErrorKey::DeniedActivateInWarp),
        restricted_op_key: UserErrorKey::ShipInWarp,
    },
    WarpPhasePolicy {
        phase: WarpPhaseKind::Cruising,
        blocks_active_warp: true,
        blocks_restricted_ops: true,
        active_warp_key: Some(UserErrorKey::ShipInWarp),
        active_module_key: Some(UserErrorKey::DeniedActivateInWarp),
        restricted_op_key: UserErrorKey::ShipInWarp,
    },
    WarpPhasePolicy {
        phase: WarpPhaseKind::Decelerating,
        blocks_active_warp: true,
        blocks_restricted_ops: true,
        active_warp_key: Some(UserErrorKey::ShipInWarp),
        active_module_key: Some(UserErrorKey::DeniedActivateInWarp),
        restricted_op_key: UserErrorKey::ShipInWarp,
    },
];

/// Structured game-state facts explaining why a server RPC rejected a client request (`#1195`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum RejectionFacts {
    /// Structure service or facility requires an installed Quantum Core (`QuantumCoreRequired`).
    QuantumCoreRequired { structure_id: i64, service: String },
    /// Action cannot proceed while the ship is in warp (`ShipAlreadyInWarp` / `ShipInWarp`, `#1454`).
    ShipInWarp { ship_id: Option<i64>, action: String, warp_phase: Option<String> },
    /// Target lock acquisition failed (`TargetingFailed`: range, cloak, decloak delay, validation).
    TargetingFailed {
        ship_id: Option<i64>,
        target_id: Option<i64>,
        reason: String,
        distance_m: Option<f64>,
        max_range_m: Option<f64>,
    },
    /// Weapon or launcher has no compatible charges loaded (`NoAmmoLoaded`).
    NoAmmoLoaded { module_id: Option<i64>, launcher_type_id: Option<i64>, required_charge_group: Option<u32> },
    /// Entity lacks enough capacitor energy (`NotEnoughCapacitor`).
    NotEnoughCapacitor { entity_id: Option<i64>, effect_name: String, required_gj: f64, available_gj: f64 },
    /// Ship is not spawned in space for the requested space action (`ShipNotInSpace`).
    ShipNotInSpace { ship_id: i64, char_id: Option<i64>, solar_system_id: Option<u32>, action: String },
    /// Module or rig failed fitting validation (`FittingValidationFailed`).
    FittingValidationFailed { ship_id: Option<i64>, item_id: Option<i64>, module_type_id: Option<i64>, reason: String },
    /// Stacked module item cannot be fitted directly without splitting (`CannotFitStackedModule`).
    CannotFitStackedModule { ship_id: Option<i64>, item_id: Option<i64>, quantity: i64 },
    /// Target is missing, self, out of system, or ineligible (`InvalidTarget`).
    InvalidTarget { target_id: Option<i64>, module_id: Option<i64>, reason: String },
    /// Inventory location or container is inaccessible to the character (`InventoryNotAccessible`).
    InventoryNotAccessible { item_id: Option<i64>, char_id: Option<i64>, location_id: Option<i64> },
    /// Caller must be docked at the contract start station (`MustBeDockedAtStartStation`).
    MustBeDockedAtStartStation { contract_id: Option<i64>, required_station_id: i64, caller_station_id: Option<i64> },
    /// Upwell or FLEX structure cannot be docked at or has no docking bay (`StructureNotDockable`).
    StructureNotDockable { structure_id: i64, state: String },
    /// Structure access list or service profile denied access (`StructureServiceAccessDenied`).
    StructureServiceAccessDenied { structure_id: i64, char_id: i64, service: String, reason: String },
    /// Weapons or PvP combat timer blocks docking or jumping (`WeaponsTimerActive`).
    WeaponsTimerActive { char_id: i64, action: String, timer_kind: String },
    /// Contract state does not permit the requested operation (`InvalidContractState`).
    InvalidContractState { contract_id: i64, expected_state: String, actual_state: String },
    /// Module item or SDE definition was not found on the ship or structure (`ModuleNotFound`).
    ModuleNotFound { module_id: i64, owner_entity_id: Option<i64>, type_id: Option<i64> },
    /// Item fails ownership or location validation (`OwnershipOrLocationViolation`).
    OwnershipOrLocationViolation {
        item_id: i64,
        owner_id: Option<i64>,
        location_id: Option<i64>,
        required_owner_id: Option<i64>,
        required_location_id: Option<i64>,
    },
    /// Session lacks required GM or admin service roles (`InsufficientRole` / `GM_InsufficientRole`).
    InsufficientRole { required_role_mask: u64, actual_role_mask: u64, action: String },
}

fn non_empty_label(value: String, fallback: impl FnOnce() -> String) -> String {
    if value.trim().is_empty() { fallback() } else { value }
}

fn pick_contract_state_key(expected_state: &str, actual_state: &str) -> UserErrorKey {
    if actual_state.eq_ignore_ascii_case("HasBids") || expected_state.eq_ignore_ascii_case("NoBids") {
        UserErrorKey::ConAuctionHasBids
    } else if expected_state.eq_ignore_ascii_case("InProgress") {
        UserErrorKey::ConContractNotInProgress
    } else if actual_state.eq_ignore_ascii_case("Cancelled") || actual_state.eq_ignore_ascii_case("Deleted") {
        UserErrorKey::ConContractInvalid
    } else {
        UserErrorKey::ConContractNotOutstanding
    }
}

impl BridgeRpcError {
    /// Returns structured game-state rejection metadata if carried by this error (`#1195`).
    pub fn rejection_facts(&self) -> Option<&RejectionFacts> {
        self.client_error.as_deref().and_then(ClientError::rejection_facts)
    }

    /// Constructs a [`BridgeRpcError`] carrying a typed [`ClientError::Rejected`] payload.
    pub fn with_rejection(
        code: impl Into<String>, message: impl Into<String>, key: UserErrorKey,
        args: impl IntoIterator<Item = impl IntoUserErrorArg>, facts: RejectionFacts,
    ) -> Self {
        Self::with_client_error(code, message, ClientError::rejected_with_args(key, args, facts))
    }

    fn reject(code: &str, message: impl Into<String>, key: UserErrorKey, facts: RejectionFacts) -> Self {
        Self::with_client_error(code, message, ClientError::rejected(key, facts))
    }

    fn quantum_core_required_with(
        key: UserErrorKey, args: impl IntoIterator<Item = impl IntoUserErrorArg>,
        structure_id: i64, service: String, message: impl Into<String>,
    ) -> Self {
        Self::with_rejection(
            "QuantumCoreRequired", message, key, args,
            RejectionFacts::QuantumCoreRequired { structure_id, service },
        )
    }

    /// Constructs a typed `QuantumCoreRequired` error for Upwell fitting service.
    pub fn quantum_core_required_fitting(
        structure_id: i64, structure_name: impl Into<String>, message: impl Into<String>,
    ) -> Self {
        let name = non_empty_label(structure_name.into(), || format!("Structure {structure_id}"));
        Self::quantum_core_required_with(
            UserErrorKey::CannotUseFittingInStructureWithoutDeed,
            [("structureName", UserErrorArg::Str(name))],
            structure_id, "fitting".to_owned(), message,
        )
    }

    /// Constructs a typed `QuantumCoreRequired` error for Upwell repair service.
    pub fn quantum_core_required_repair(
        structure_id: i64, structure_name: impl Into<String>, message: impl Into<String>,
    ) -> Self {
        let name = non_empty_label(structure_name.into(), || format!("Structure {structure_id}"));
        Self::quantum_core_required_with(
            UserErrorKey::CannotRepairInStructureWithoutDeed,
            [("structureName", UserErrorArg::Str(name))],
            structure_id, "repair".to_owned(), message,
        )
    }

    /// Constructs a typed `QuantumCoreRequired` error for general Upwell service modules.
    pub fn quantum_core_required_service(
        structure_id: i64, structure_name: impl Into<String>, service: impl Into<String>, message: impl Into<String>,
    ) -> Self {
        let svc = non_empty_label(service.into(), || "service".to_owned());
        let name = non_empty_label(structure_name.into(), || format!("Structure {structure_id}"));
        Self::quantum_core_required_with(
            UserErrorKey::StructureServiceUnavailable,
            [("serviceName", UserErrorArg::Str(svc.clone())), ("structureName", UserErrorArg::Str(name))],
            structure_id, svc, message,
        )
    }

    /// Constructs a typed `ShipAlreadyInWarp` (`ShipInWarp` V24 dialog 1565) error compatible with `#1454`.
    pub fn ship_already_in_warp(
        ship_id: Option<i64>, action: impl Into<String>, warp_phase: Option<String>, message: impl Into<String>,
    ) -> Self {
        Self::reject("ShipAlreadyInWarp", message, UserErrorKey::ShipInWarp, RejectionFacts::ShipInWarp {
            ship_id, action: action.into(), warp_phase,
        })
    }

    /// Constructs a typed `ShipInWarp` (`ShipInWarp` V24 dialog 1565) error compatible with `#1454`.
    pub fn ship_in_warp(
        ship_id: Option<i64>, action: impl Into<String>, warp_phase: Option<String>, message: impl Into<String>,
    ) -> Self {
        Self::reject("ShipInWarp", message, UserErrorKey::ShipInWarp, RejectionFacts::ShipInWarp {
            ship_id, action: action.into(), warp_phase,
        })
    }

    /// Constructs a phase-aware `ShipAlreadyInWarp` (`#1454`) error for active-warp phases.
    pub fn ship_already_in_warp_for_phase(
        ship_id: Option<i64>, action: impl Into<String>, phase: WarpPhaseKind, message: impl Into<String>,
    ) -> Self {
        debug_assert!(
            phase.blocks_active_warp_actions(),
            "Aligning must not emit ShipAlreadyInWarp for active-warp overrides"
        );
        Self::ship_already_in_warp(ship_id, action, Some(phase.as_str().to_owned()), message)
    }

    /// Constructs a phase-aware `ShipInWarp` (`#1454`) error for active-warp phases.
    pub fn ship_in_warp_for_phase(
        ship_id: Option<i64>, action: impl Into<String>, phase: WarpPhaseKind, message: impl Into<String>,
    ) -> Self {
        debug_assert!(
            phase.blocks_active_warp_actions(),
            "Use ship_in_warp_for_restricted_phase when Aligning also rejects"
        );
        Self::ship_in_warp(ship_id, action, Some(phase.as_str().to_owned()), message)
    }

    /// Constructs a phase-aware `ShipInWarp` (`#1454`) error for restricted jumps, deploys, and bay boarding (incl. `Aligning`).
    pub fn ship_in_warp_for_restricted_phase(
        ship_id: Option<i64>, action: impl Into<String>, phase: WarpPhaseKind, message: impl Into<String>,
    ) -> Self {
        Self::ship_in_warp(ship_id, action, Some(phase.as_str().to_owned()), message)
    }

    /// Constructs a phase-aware `CantActivateInWarp` (`DeniedActivateInWarp`) error for active-warp phases (`#1454`).
    pub fn cant_activate_in_warp_for_phase(
        ship_id: Option<i64>, action: impl Into<String>, phase: WarpPhaseKind, message: impl Into<String>,
    ) -> Self {
        debug_assert!(
            phase.blocks_active_warp_actions(),
            "Aligning must not emit CantActivateInWarp"
        );
        Self::cant_activate_in_warp(ship_id, action, Some(phase.as_str().to_owned()), message)
    }

    /// Constructs a typed `CantActivateInWarp` (`DeniedActivateInWarp` V24 dialog 774) error.
    pub fn cant_activate_in_warp(
        ship_id: Option<i64>, action: impl Into<String>, warp_phase: Option<String>, message: impl Into<String>,
    ) -> Self {
        Self::reject("CantActivateInWarp", message, UserErrorKey::DeniedActivateInWarp, RejectionFacts::ShipInWarp {
            ship_id, action: action.into(), warp_phase,
        })
    }

    /// Constructs a typed `NoAmmoLoaded` (`NoCharges` V24 dialog 1291) error.
    pub fn no_ammo_loaded(
        module_id: Option<i64>, launcher_type_id: Option<i64>, required_charge_group: Option<u32>,
        launcher_name: Option<&str>, message: impl Into<String>,
    ) -> Self {
        let launcher_arg = match launcher_type_id.filter(|&tid| tid > 0) {
            Some(tid) => UserErrorArg::TypeId(tid),
            None => UserErrorArg::Str(launcher_name.unwrap_or("Weapon").to_owned()),
        };
        Self::with_rejection(
            "NoAmmoLoaded", message, UserErrorKey::NoCharges, [("launcher", launcher_arg)],
            RejectionFacts::NoAmmoLoaded { module_id, launcher_type_id, required_charge_group },
        )
    }

    /// Constructs a typed `NotEnoughCapacitor` (`NotEnoughEnergy` V24 dialog 1344) error.
    pub fn not_enough_capacitor(
        entity_id: Option<i64>, effect_name: impl Into<String>, required_gj: f64, available_gj: f64,
        message: impl Into<String>,
    ) -> Self {
        let effect = non_empty_label(effect_name.into(), || "module activation".to_owned());
        let (req, avail) = (round_user_error_real_6dp(required_gj), round_user_error_real_6dp(available_gj));
        Self::with_rejection(
            "NotEnoughCapacitor", message, UserErrorKey::NotEnoughEnergy,
            [("effectname", UserErrorArg::Str(effect.clone())), ("need", UserErrorArg::Real(req)), ("got", UserErrorArg::Real(avail))],
            RejectionFacts::NotEnoughCapacitor { entity_id, effect_name: effect, required_gj: req, available_gj: avail },
        )
    }

    /// Constructs a typed jump-capacitor `NotEnoughCapacitor` (`JumpNotEnoughCap` V24 dialog 1036) error.
    pub fn jump_not_enough_capacitor(
        ship_id: Option<i64>, required_gj: f64, available_gj: f64, message: impl Into<String>,
    ) -> Self {
        let (req, avail) = (round_user_error_real_6dp(required_gj), round_user_error_real_6dp(available_gj));
        Self::with_rejection(
            "NotEnoughCapacitor", message, UserErrorKey::JumpNotEnoughCap,
            [("need", UserErrorArg::Real(req)), ("got", UserErrorArg::Real(avail))],
            RejectionFacts::NotEnoughCapacitor { entity_id: ship_id, effect_name: "Jump".to_owned(), required_gj: req, available_gj: avail },
        )
    }

    /// Constructs a typed `ShipNotInSpace` (`CantShipNotPresent` V24 dialog 422) error.
    pub fn ship_not_in_space(
        ship_id: i64, char_id: Option<i64>, solar_system_id: Option<u32>, action: impl Into<String>, message: impl Into<String>,
    ) -> Self {
        Self::reject("ShipNotInSpace", message, UserErrorKey::CantShipNotPresent, RejectionFacts::ShipNotInSpace {
            ship_id, char_id, solar_system_id, action: action.into(),
        })
    }

    /// Constructs a typed `FittingValidationFailed` (`ModuleFitFailed` V24 dialog 1228) error.
    pub fn fitting_validation_failed(
        ship_id: Option<i64>, item_id: Option<i64>, module_type_id: Option<i64>,
        module_name: impl Into<String>, reason: impl Into<String>,
    ) -> Self {
        let module = non_empty_label(module_name.into(), || "Module".to_owned());
        let why = reason.into();
        Self::with_rejection(
            "FittingValidationFailed", why.clone(), UserErrorKey::ModuleFitFailed,
            [("moduleName", UserErrorArg::Str(module)), ("reason", UserErrorArg::Str(why.clone()))],
            RejectionFacts::FittingValidationFailed { ship_id, item_id, module_type_id, reason: why },
        )
    }

    /// Constructs a typed `CannotFitStackedModule` (`CannotPerformOnMultipleItems` V24 dialog 2271) error.
    pub fn cannot_fit_stacked_module(
        ship_id: Option<i64>, item_id: Option<i64>, quantity: i64, message: impl Into<String>,
    ) -> Self {
        Self::reject(
            "CannotFitStackedModule", message, UserErrorKey::CannotPerformOnMultipleItems,
            RejectionFacts::CannotFitStackedModule { ship_id, item_id, quantity },
        )
    }

    /// Constructs a typed `InvalidTarget` (`InvalidTargetForModule` V24 dialog 7824) error.
    pub fn invalid_target(
        target_id: Option<i64>, module_id: Option<i64>, module_name: impl Into<String>,
        reason: impl Into<String>, message: impl Into<String>,
    ) -> Self {
        let module = non_empty_label(module_name.into(), || "this module".to_owned());
        Self::with_rejection(
            "InvalidTarget", message, UserErrorKey::InvalidTargetForModule, [("moduleName", UserErrorArg::Str(module))],
            RejectionFacts::InvalidTarget { target_id, module_id, reason: reason.into() },
        )
    }

    /// Constructs a non-module navigation `InvalidTarget` (`WarpDestinationGone` V24 dialog 1679) error.
    pub fn invalid_navigation_target(
        target_id: Option<i64>, reason: impl Into<String>, message: impl Into<String>,
    ) -> Self {
        Self::reject("InvalidTarget", message, UserErrorKey::WarpDestinationGone, RejectionFacts::InvalidTarget {
            target_id, module_id: None, reason: reason.into(),
        })
    }

    /// Constructs a non-module `InvalidTarget` (`InvalidTargetGroup` V24 dialog 987) error with SDE group name.
    pub fn invalid_target_group(
        target_id: Option<i64>, group_name: impl Into<String>, reason: impl Into<String>, message: impl Into<String>,
    ) -> Self {
        let group = non_empty_label(group_name.into(), || "Unknown".to_owned());
        Self::with_rejection(
            "InvalidTarget", message, UserErrorKey::InvalidTargetGroup, [("groupName", UserErrorArg::Str(group))],
            RejectionFacts::InvalidTarget { target_id, module_id: None, reason: reason.into() },
        )
    }

    /// Constructs a typed self-target `InvalidTarget` (`DeniedTargetSelf` V24 dialog 788) error.
    pub fn invalid_target_self(target_id: i64, reason: impl Into<String>, message: impl Into<String>) -> Self {
        Self::reject("InvalidTarget", message, UserErrorKey::DeniedTargetSelf, RejectionFacts::InvalidTarget {
            target_id: Some(target_id), module_id: None, reason: reason.into(),
        })
    }

    /// Constructs a typed out-of-range `TargetingFailed` (`TargetTooFar` V24 dialog 1632) error.
    pub fn targeting_too_far(
        ship_id: Option<i64>,
        target_id: Option<i64>,
        target_name: impl Into<String>,
        module_type_id: i64,
        distance_m: f64,
        max_range_m: f64,
        message: impl Into<String>,
    ) -> Self {
        let dist = round_user_error_real_6dp(distance_m);
        let max_r = round_user_error_real_6dp(max_range_m);
        Self::with_rejection(
            "TargetingFailed",
            message,
            UserErrorKey::TargetTooFar,
            ClientError::target_too_far_args(target_name, module_type_id, dist),
            RejectionFacts::TargetingFailed {
                ship_id,
                target_id,
                reason: "exceeds_max_target_range".to_owned(),
                distance_m: Some(dist),
                max_range_m: Some(max_r),
            },
        )
    }

    /// Constructs a typed `TargetingFailed` error mapped to a concrete V24 targeting [`UserErrorKey`].
    pub fn targeting_failed(
        ship_id: Option<i64>,
        target_id: Option<i64>,
        key: UserErrorKey,
        reason: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self::reject(
            "TargetingFailed",
            message,
            key,
            RejectionFacts::TargetingFailed {
                ship_id,
                target_id,
                reason: reason.into(),
                distance_m: None,
                max_range_m: None,
            },
        )
    }

    /// Constructs a typed `InventoryNotAccessible` (`AccessDenied` V24 dialog 7256) error.
    pub fn inventory_not_accessible(
        item_id: Option<i64>, char_id: Option<i64>, location_id: Option<i64>, message: impl Into<String>,
    ) -> Self {
        Self::reject("InventoryNotAccessible", message, UserErrorKey::AccessDenied, RejectionFacts::InventoryNotAccessible {
            item_id, char_id, location_id,
        })
    }

    /// Constructs a typed `MustBeDockedAtStartStation` (`MustBeDocked` V24 dialog 4655) error.
    pub fn must_be_docked_at_start_station(
        contract_id: Option<i64>, required_station_id: i64, caller_station_id: Option<i64>, message: impl Into<String>,
    ) -> Self {
        Self::reject(
            "MustBeDockedAtStartStation", message, UserErrorKey::MustBeDocked,
            RejectionFacts::MustBeDockedAtStartStation { contract_id, required_station_id, caller_station_id },
        )
    }

    /// Constructs a typed `StructureNotDockable` (`NotDockable` V24 dialog 1334) error.
    pub fn structure_not_dockable(structure_id: i64, state: impl Into<String>, message: impl Into<String>) -> Self {
        Self::reject("StructureNotDockable", message, UserErrorKey::NotDockable, RejectionFacts::StructureNotDockable {
            structure_id, state: state.into(),
        })
    }

    /// Constructs a typed `StructureServiceAccessDenied` error
    /// (`StructureDockingDenied` dialog 4886 for `"docking"`, otherwise `StructureServiceUnavailable` dialog 4986).
    pub fn structure_service_access_denied(
        structure_id: i64, char_id: i64, structure_name: impl Into<String>,
        service: impl Into<String>, reason: impl Into<String>, message: impl Into<String>,
    ) -> Self {
        let svc = non_empty_label(service.into(), || "service".to_owned());
        let name = non_empty_label(structure_name.into(), || format!("Structure {structure_id}"));
        let facts = RejectionFacts::StructureServiceAccessDenied { structure_id, char_id, service: svc.clone(), reason: reason.into() };
        if svc.eq_ignore_ascii_case("docking") {
            Self::with_rejection("StructureServiceAccessDenied", message, UserErrorKey::StructureDockingDenied, [("structureName", UserErrorArg::Str(name))], facts)
        } else {
            Self::with_rejection(
                "StructureServiceAccessDenied", message, UserErrorKey::StructureServiceUnavailable,
                [("serviceName", UserErrorArg::Str(svc)), ("structureName", UserErrorArg::Str(name))], facts,
            )
        }
    }

    /// Constructs a typed `WeaponsTimerActive` error
    /// (`CantJumpAfterAggression` dialog 385 when `jump_context`, else `CantDockAfterAggression` dialog 377).
    pub fn weapons_timer_active(
        char_id: i64, action: impl Into<String>, timer_kind: impl Into<String>, jump_context: bool, message: impl Into<String>,
    ) -> Self {
        let key = if jump_context { UserErrorKey::CantJumpAfterAggression } else { UserErrorKey::CantDockAfterAggression };
        Self::reject("WeaponsTimerActive", message, key, RejectionFacts::WeaponsTimerActive {
            char_id, action: action.into(), timer_kind: timer_kind.into(),
        })
    }

    /// Constructs a typed `CannotJumpWithWeaponsTimer` (`CantJumpAfterAggression` dialog 385) error.
    pub fn cannot_jump_with_weapons_timer(
        char_id: i64, action: impl Into<String>, timer_kind: impl Into<String>, message: impl Into<String>,
    ) -> Self {
        Self::reject(
            "CannotJumpWithWeaponsTimer", message, UserErrorKey::CantJumpAfterAggression,
            RejectionFacts::WeaponsTimerActive { char_id, action: action.into(), timer_kind: timer_kind.into() },
        )
    }

    /// Constructs a typed `InvalidContractState` error mapped to the matching V24 contract dialog.
    pub fn invalid_contract_state(
        contract_id: i64, expected_state: impl Into<String>, actual_state: impl Into<String>, message: impl Into<String>,
    ) -> Self {
        let (expected, actual) = (expected_state.into(), actual_state.into());
        let key = pick_contract_state_key(&expected, &actual);
        Self::reject("InvalidContractState", message, key, RejectionFacts::InvalidContractState {
            contract_id, expected_state: expected, actual_state: actual,
        })
    }

    /// Constructs a typed `ContractOwnershipViolation` (`AccessDenied` V24 dialog 7256) error.
    pub fn contract_ownership_violation(
        contract_id: i64, caller_id: i64, required_owner_id: Option<i64>, message: impl Into<String>,
    ) -> Self {
        Self::reject("ContractOwnershipViolation", message, UserErrorKey::AccessDenied, RejectionFacts::OwnershipOrLocationViolation {
            item_id: contract_id, owner_id: Some(caller_id), location_id: None, required_owner_id, required_location_id: None,
        })
    }

    /// Constructs a typed `ModuleNotFound` error
    /// (`ModuleNoLongerPresentForCharges` dialog 1230 if `for_charges`, else `ItemNotFound` dialog 3536).
    pub fn module_not_found(
        module_id: i64, owner_entity_id: Option<i64>, type_id: Option<i64>, for_charges: bool, message: impl Into<String>,
    ) -> Self {
        let key = if for_charges { UserErrorKey::ModuleNoLongerPresentForCharges } else { UserErrorKey::ItemNotFound };
        Self::reject("ModuleNotFound", message, key, RejectionFacts::ModuleNotFound {
            module_id, owner_entity_id, type_id,
        })
    }

    /// Constructs a typed `OwnershipOrLocationViolation` (`CantDoThatWithSomeoneElsesStuff` dialog 375) error.
    pub fn ownership_or_location_violation(
        item_id: i64, owner_id: Option<i64>, location_id: Option<i64>,
        required_owner_id: Option<i64>, required_location_id: Option<i64>, message: impl Into<String>,
    ) -> Self {
        Self::reject(
            "OwnershipOrLocationViolation", message, UserErrorKey::CantDoThatWithSomeoneElsesStuff,
            RejectionFacts::OwnershipOrLocationViolation { item_id, owner_id, location_id, required_owner_id, required_location_id },
        )
    }

    /// Constructs a typed `InsufficientRole` (`InsufficientRoles` V24 dialog 3164) error.
    pub fn insufficient_role(
        required_role_mask: u64, actual_role_mask: u64, action: impl Into<String>, message: impl Into<String>,
    ) -> Self {
        Self::insufficient_role_with_code("InsufficientRole", required_role_mask, actual_role_mask, action, message)
    }

    /// Constructs a typed role-rejection error carrying a legacy RPC error `code`
    /// (`"GMRoleRequired"`, `"GmRoleRequired"`, `"InsufficientRoles"`) and `InsufficientRoles` dialog facts.
    pub fn insufficient_role_with_code(
        code: impl Into<String>, required_role_mask: u64, actual_role_mask: u64,
        action: impl Into<String>, message: impl Into<String>,
    ) -> Self {
        Self::with_client_error(
            code,
            message,
            ClientError::rejected(UserErrorKey::InsufficientRoles, RejectionFacts::InsufficientRole {
                required_role_mask, actual_role_mask, action: action.into(),
            }),
        )
    }
}
