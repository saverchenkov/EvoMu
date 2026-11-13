//! Typed client error model (`ClientError`, `UserErrorKey`) and total `machoNet` wire mapping (`C-5`, `#929`).
//!
//! Replaces untyped `(code, message)` string-scraping recovery with a closed `ClientError`
//! sum type whose `to_macho_error_body` mapping is a total function.

#![forbid(unsafe_code)]

use crate::{marshal, BridgeRpcError, PyRep};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{LazyLock, RwLock};

/// `cluster.MACHONETERR_WRAPPEDEXCEPTION`: `ErrorResponse.payload` is `(DumpsSanitized(exc),)`.
pub const MACHONETERR_WRAPPEDEXCEPTION: i64 = 2;

/// Maximum cached typed `ClientError` entries in [`TYPED_CLIENT_ERRORS`].
const MAX_TYPED_ERROR_CACHE_ENTRIES: usize = 16_384;

/// Thread-safe side-table mapping `(code, message)` pairs produced by typed [`BridgeRpcError`]
/// constructors to their lossless [`ClientError`] representation.
static TYPED_CLIENT_ERRORS: LazyLock<RwLock<HashMap<(String, String), ClientError>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

/// Programmatic V24.01 `eveexceptions.UserError` keys handled directly by client `.pyj` call sites
/// (`base_insurance.pyj`, skill checks, station checks) outside `res:/staticdata/dialogs.static`.
pub const V24_PROGRAMMATIC_USER_ERROR_KEYS: &[&str] = &[
    "CanOnlyDoInStation",
    "InsureShipFailed",
    "InsureShipInvalidCost",
    "SkillMissing",
];

/// V24.01 `eveexceptions.UserError` message catalogue keys (`C-5`, `#929`).
///
/// Every variant is validated at test time against either `res:/staticdata/dialogs.static`
/// ([`Self::v24_dialog_id`]) or [`V24_PROGRAMMATIC_USER_ERROR_KEYS`].
#[allow(non_camel_case_types)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum UserErrorKey {
    // --- Insurance (`eve/client/script/ui/station/insurance/base_insurance.pyj`) ---
    InsureShipFailedSingleContract,
    InsureShipFailedMultipleContracts,
    InsureShipAlreadyInsured,
    InsureShipInvalidCost,
    InsureShipFailed,

    // --- Custom message dialogs (`dialogs.static` 764..=767) ---
    CustomError,
    CustomInfo,
    CustomNotify,
    CustomQuestion,

    // --- Inventory & hold capacity (`dialogs.static` 1316..=4782) ---
    NotEnoughCargoSpace,
    NotEnoughCargoSpaceOverload,
    NotEnoughDroneBaySpace,
    NotEnoughDroneBaySpaceOverload,
    NotEnoughFighterBaySpace,
    NotEnoughSpecialBaySpace,
    NotEnoughSpecialBaySpaceOverload,
    NotEnoughSpace,
    NotEnoughChargeSpace,
    NoSpaceForThat,
    NoSpaceForThatOverload,
    ItemLocked,
    ItemNotAtStation,
    ItemNotFound,
    CanNotUnassembleInThisItemLoc,
    CantRepackageDamagedItem,
    CantDoThatWithSomeoneElsesStuff,
    SlotAlreadyOccupied,
    OwnerMaxFittings,

    // --- Market & Wallet ---
    MarketExceededOrderCount,
    NotEnoughMoney,

    // --- Navigation, Docking & Stargates ---
    DockingApproach,
    DockingRequestDenied,
    CantDockWhileModuleActive,
    CantJumpWhileModuleActive,
    WarpDisrupted,
    WarpScrambled,
    DeniedTethered,
    ShipCannotUseStargate,
    SystemCheck_JumpFailed_StandingsTooLow,
    CannotDscanInThisSystem,
    DunBlacklistCannotWarp,
    MustBeDocked,
    CanOnlyDoInStations,
    CanOnlyDoInStation,
    ShipCloaked,
    ShipInWarp,
    ShipNotBoardable,
    NoActiveBeacon,

    // --- Skills, Clones & Crimewatch ---
    SkillMissing,
    SkillUnavailableForPurchase,
    JumpCloneNotFound,
    NoCloneBayService,
    AlreadyInNewbieShip,
    CannotEjectFromCapsule,
    CannotEjectActiveWeaponsTimer,
    CannotEjectInvolvedInCrime,
    CannotStoreShipActiveWeaponsTimer,
    CannotBoardShipInHighSecAsCriminal,
    CantDockGlobalCriminalFlag,
    CantEnterWormholeGlobalCriminalFlag,
    CantWarpAwayGlobalCriminalFlag,

    // --- Combat, Modules, Drones & Fighters ---
    ModuleAlreadyActive,
    ModuleNotOnline,
    MaxBandwidthExceeded,
    TargetNotLocked,
    InvalidTargetGroup,
    ShipHasNoFighterBay,
    CannotLaunchFighterNotEnoughTubesAvailableForClass,
    CannotOpenFighterTubesWhileCloaked,
    CannotOpenFighterTubesWhileWarping,

    // --- Corporation, Fleet, Social & Planetary Interaction ---
    CrpAccessDenied,
    AccessDenied,
    InsufficientRoles,
    NoOfficeAtStation,
    AllianceNotFound,
    AlreadyAtWar,
    FleetNotFound,
    FleetNotInFleet,
    FleetHangarAccessDenied,
    CantbookmarkEveSystem,
    DuelChallengeFailNoSesssionForTarget,
    KillRightNotForSale,
    InvalidPinType,
    InvalidSchematic,
    LinkAlreadyExists,
    PISkillRequiredToSurveyPlanetResources,

    // --- Protocol & Devtools ---
    SlashError,
    UnMachoDestination,
}

impl UserErrorKey {
    /// Exhaustive slice of all typed [`UserErrorKey`] variants.
    pub const ALL: &[Self] = &[
        Self::InsureShipFailedSingleContract,
        Self::InsureShipFailedMultipleContracts,
        Self::InsureShipAlreadyInsured,
        Self::InsureShipInvalidCost,
        Self::InsureShipFailed,
        Self::CustomError,
        Self::CustomInfo,
        Self::CustomNotify,
        Self::CustomQuestion,
        Self::NotEnoughCargoSpace,
        Self::NotEnoughCargoSpaceOverload,
        Self::NotEnoughDroneBaySpace,
        Self::NotEnoughDroneBaySpaceOverload,
        Self::NotEnoughFighterBaySpace,
        Self::NotEnoughSpecialBaySpace,
        Self::NotEnoughSpecialBaySpaceOverload,
        Self::NotEnoughSpace,
        Self::NotEnoughChargeSpace,
        Self::NoSpaceForThat,
        Self::NoSpaceForThatOverload,
        Self::ItemLocked,
        Self::ItemNotAtStation,
        Self::ItemNotFound,
        Self::CanNotUnassembleInThisItemLoc,
        Self::CantRepackageDamagedItem,
        Self::CantDoThatWithSomeoneElsesStuff,
        Self::SlotAlreadyOccupied,
        Self::OwnerMaxFittings,
        Self::MarketExceededOrderCount,
        Self::NotEnoughMoney,
        Self::DockingApproach,
        Self::DockingRequestDenied,
        Self::CantDockWhileModuleActive,
        Self::CantJumpWhileModuleActive,
        Self::WarpDisrupted,
        Self::WarpScrambled,
        Self::DeniedTethered,
        Self::ShipCannotUseStargate,
        Self::SystemCheck_JumpFailed_StandingsTooLow,
        Self::CannotDscanInThisSystem,
        Self::DunBlacklistCannotWarp,
        Self::MustBeDocked,
        Self::CanOnlyDoInStations,
        Self::CanOnlyDoInStation,
        Self::ShipCloaked,
        Self::ShipInWarp,
        Self::ShipNotBoardable,
        Self::NoActiveBeacon,
        Self::SkillMissing,
        Self::SkillUnavailableForPurchase,
        Self::JumpCloneNotFound,
        Self::NoCloneBayService,
        Self::AlreadyInNewbieShip,
        Self::CannotEjectFromCapsule,
        Self::CannotEjectActiveWeaponsTimer,
        Self::CannotEjectInvolvedInCrime,
        Self::CannotStoreShipActiveWeaponsTimer,
        Self::CannotBoardShipInHighSecAsCriminal,
        Self::CantDockGlobalCriminalFlag,
        Self::CantEnterWormholeGlobalCriminalFlag,
        Self::CantWarpAwayGlobalCriminalFlag,
        Self::ModuleAlreadyActive,
        Self::ModuleNotOnline,
        Self::MaxBandwidthExceeded,
        Self::TargetNotLocked,
        Self::InvalidTargetGroup,
        Self::ShipHasNoFighterBay,
        Self::CannotLaunchFighterNotEnoughTubesAvailableForClass,
        Self::CannotOpenFighterTubesWhileCloaked,
        Self::CannotOpenFighterTubesWhileWarping,
        Self::CrpAccessDenied,
        Self::AccessDenied,
        Self::InsufficientRoles,
        Self::NoOfficeAtStation,
        Self::AllianceNotFound,
        Self::AlreadyAtWar,
        Self::FleetNotFound,
        Self::FleetNotInFleet,
        Self::FleetHangarAccessDenied,
        Self::CantbookmarkEveSystem,
        Self::DuelChallengeFailNoSesssionForTarget,
        Self::KillRightNotForSale,
        Self::InvalidPinType,
        Self::InvalidSchematic,
        Self::LinkAlreadyExists,
        Self::PISkillRequiredToSurveyPlanetResources,
        Self::SlashError,
        Self::UnMachoDestination,
    ];

    /// Exact V24 `eveexceptions.UserError` message key string sent on the wire.
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::InsureShipFailedSingleContract => "InsureShipFailedSingleContract",
            Self::InsureShipFailedMultipleContracts => "InsureShipFailedMultipleContracts",
            Self::InsureShipAlreadyInsured => "InsureShipAlreadyInsured",
            Self::InsureShipInvalidCost => "InsureShipInvalidCost",
            Self::InsureShipFailed => "InsureShipFailed",
            Self::CustomError => "CustomError",
            Self::CustomInfo => "CustomInfo",
            Self::CustomNotify => "CustomNotify",
            Self::CustomQuestion => "CustomQuestion",
            Self::NotEnoughCargoSpace => "NotEnoughCargoSpace",
            Self::NotEnoughCargoSpaceOverload => "NotEnoughCargoSpaceOverload",
            Self::NotEnoughDroneBaySpace => "NotEnoughDroneBaySpace",
            Self::NotEnoughDroneBaySpaceOverload => "NotEnoughDroneBaySpaceOverload",
            Self::NotEnoughFighterBaySpace => "NotEnoughFighterBaySpace",
            Self::NotEnoughSpecialBaySpace => "NotEnoughSpecialBaySpace",
            Self::NotEnoughSpecialBaySpaceOverload => "NotEnoughSpecialBaySpaceOverload",
            Self::NotEnoughSpace => "NotEnoughSpace",
            Self::NotEnoughChargeSpace => "NotEnoughChargeSpace",
            Self::NoSpaceForThat => "NoSpaceForThat",
            Self::NoSpaceForThatOverload => "NoSpaceForThatOverload",
            Self::ItemLocked => "ItemLocked",
            Self::ItemNotAtStation => "ItemNotAtStation",
            Self::ItemNotFound => "ItemNotFound",
            Self::CanNotUnassembleInThisItemLoc => "CanNotUnassembleInThisItemLoc",
            Self::CantRepackageDamagedItem => "CantRepackageDamagedItem",
            Self::CantDoThatWithSomeoneElsesStuff => "CantDoThatWithSomeoneElsesStuff",
            Self::SlotAlreadyOccupied => "SlotAlreadyOccupied",
            Self::OwnerMaxFittings => "OwnerMaxFittings",
            Self::MarketExceededOrderCount => "MarketExceededOrderCount",
            Self::NotEnoughMoney => "NotEnoughMoney",
            Self::DockingApproach => "DockingApproach",
            Self::DockingRequestDenied => "DockingRequestDenied",
            Self::CantDockWhileModuleActive => "CantDockWhileModuleActive",
            Self::CantJumpWhileModuleActive => "CantJumpWhileModuleActive",
            Self::WarpDisrupted => "WarpDisrupted",
            Self::WarpScrambled => "WarpScrambled",
            Self::DeniedTethered => "DeniedTethered",
            Self::ShipCannotUseStargate => "ShipCannotUseStargate",
            Self::SystemCheck_JumpFailed_StandingsTooLow => {
                "SystemCheck_JumpFailed_StandingsTooLow"
            }
            Self::CannotDscanInThisSystem => "CannotDscanInThisSystem",
            Self::DunBlacklistCannotWarp => "DunBlacklistCannotWarp",
            Self::MustBeDocked => "MustBeDocked",
            Self::CanOnlyDoInStations => "CanOnlyDoInStations",
            Self::CanOnlyDoInStation => "CanOnlyDoInStation",
            Self::ShipCloaked => "ShipCloaked",
            Self::ShipInWarp => "ShipInWarp",
            Self::ShipNotBoardable => "ShipNotBoardable",
            Self::NoActiveBeacon => "NoActiveBeacon",
            Self::SkillMissing => "SkillMissing",
            Self::SkillUnavailableForPurchase => "SkillUnavailableForPurchase",
            Self::JumpCloneNotFound => "JumpCloneNotFound",
            Self::NoCloneBayService => "NoCloneBayService",
            Self::AlreadyInNewbieShip => "AlreadyInNewbieShip",
            Self::CannotEjectFromCapsule => "CannotEjectFromCapsule",
            Self::CannotEjectActiveWeaponsTimer => "CannotEjectActiveWeaponsTimer",
            Self::CannotEjectInvolvedInCrime => "CannotEjectInvolvedInCrime",
            Self::CannotStoreShipActiveWeaponsTimer => "CannotStoreShipActiveWeaponsTimer",
            Self::CannotBoardShipInHighSecAsCriminal => "CannotBoardShipInHighSecAsCriminal",
            Self::CantDockGlobalCriminalFlag => "CantDockGlobalCriminalFlag",
            Self::CantEnterWormholeGlobalCriminalFlag => "CantEnterWormholeGlobalCriminalFlag",
            Self::CantWarpAwayGlobalCriminalFlag => "CantWarpAwayGlobalCriminalFlag",
            Self::ModuleAlreadyActive => "ModuleAlreadyActive",
            Self::ModuleNotOnline => "ModuleNotOnline",
            Self::MaxBandwidthExceeded => "MaxBandwidthExceeded",
            Self::TargetNotLocked => "TargetNotLocked",
            Self::InvalidTargetGroup => "InvalidTargetGroup",
            Self::ShipHasNoFighterBay => "ShipHasNoFighterBay",
            Self::CannotLaunchFighterNotEnoughTubesAvailableForClass => {
                "CannotLaunchFighterNotEnoughTubesAvailableForClass"
            }
            Self::CannotOpenFighterTubesWhileCloaked => "CannotOpenFighterTubesWhileCloaked",
            Self::CannotOpenFighterTubesWhileWarping => "CannotOpenFighterTubesWhileWarping",
            Self::CrpAccessDenied => "CrpAccessDenied",
            Self::AccessDenied => "AccessDenied",
            Self::InsufficientRoles => "InsufficientRoles",
            Self::NoOfficeAtStation => "NoOfficeAtStation",
            Self::AllianceNotFound => "AllianceNotFound",
            Self::AlreadyAtWar => "AlreadyAtWar",
            Self::FleetNotFound => "FleetNotFound",
            Self::FleetNotInFleet => "FleetNotInFleet",
            Self::FleetHangarAccessDenied => "FleetHangarAccessDenied",
            Self::CantbookmarkEveSystem => "CantbookmarkEveSystem",
            Self::DuelChallengeFailNoSesssionForTarget => "DuelChallengeFailNoSesssionForTarget",
            Self::KillRightNotForSale => "KillRightNotForSale",
            Self::InvalidPinType => "InvalidPinType",
            Self::InvalidSchematic => "InvalidSchematic",
            Self::LinkAlreadyExists => "LinkAlreadyExists",
            Self::PISkillRequiredToSurveyPlanetResources => {
                "PISkillRequiredToSurveyPlanetResources"
            }
            Self::SlashError => "SlashError",
            Self::UnMachoDestination => "UnMachoDestination",
        }
    }

    /// Resolves a string code into a typed [`UserErrorKey`] if it matches a catalogued variant.
    pub fn from_code(code: &str) -> Option<Self> {
        let trimmed = code.trim();
        Self::ALL.iter().copied().find(|k| k.as_str() == trimmed)
    }

    /// Expected `dialog_id` in V24.01 `res:/staticdata/dialogs.static`, or `None` for
    /// programmatic client `.pyj` `UserError` keys in [`V24_PROGRAMMATIC_USER_ERROR_KEYS`].
    pub const fn v24_dialog_id(&self) -> Option<u32> {
        match self {
            Self::InsureShipFailedSingleContract => Some(937),
            Self::InsureShipFailedMultipleContracts => Some(936),
            Self::InsureShipAlreadyInsured => Some(2289),
            Self::InsureShipInvalidCost | Self::InsureShipFailed => None,
            Self::CustomError => Some(764),
            Self::CustomInfo => Some(765),
            Self::CustomNotify => Some(766),
            Self::CustomQuestion => Some(767),
            Self::NotEnoughCargoSpace => Some(1336),
            Self::NotEnoughCargoSpaceOverload => Some(1338),
            Self::NotEnoughDroneBaySpace => Some(1342),
            Self::NotEnoughDroneBaySpaceOverload => Some(1343),
            Self::NotEnoughFighterBaySpace => Some(4782),
            Self::NotEnoughSpecialBaySpace => Some(2919),
            Self::NotEnoughSpecialBaySpaceOverload => Some(2920),
            Self::NotEnoughSpace => Some(1352),
            Self::NotEnoughChargeSpace => Some(1339),
            Self::NoSpaceForThat => Some(1316),
            Self::NoSpaceForThatOverload => Some(1318),
            Self::ItemLocked => Some(1000),
            Self::ItemNotAtStation => Some(1004),
            Self::ItemNotFound => Some(3536),
            Self::CanNotUnassembleInThisItemLoc => Some(279),
            Self::CantRepackageDamagedItem => Some(413),
            Self::CantDoThatWithSomeoneElsesStuff => Some(375),
            Self::SlotAlreadyOccupied => Some(1593),
            Self::OwnerMaxFittings => Some(2859),
            Self::MarketExceededOrderCount => Some(1146),
            Self::NotEnoughMoney => Some(1347),
            Self::DockingApproach => Some(807),
            Self::DockingRequestDenied => Some(808),
            Self::CantDockWhileModuleActive => Some(2094),
            Self::CantJumpWhileModuleActive => Some(2095),
            Self::WarpDisrupted => Some(1680),
            Self::WarpScrambled => Some(1682),
            Self::DeniedTethered => Some(4899),
            Self::ShipCannotUseStargate => Some(7882),
            Self::SystemCheck_JumpFailed_StandingsTooLow => Some(7433),
            Self::CannotDscanInThisSystem => Some(7864),
            Self::DunBlacklistCannotWarp => Some(2100),
            Self::MustBeDocked => Some(4655),
            Self::CanOnlyDoInStations => Some(2302),
            Self::CanOnlyDoInStation => None,
            Self::ShipCloaked => Some(1550),
            Self::ShipInWarp => Some(1565),
            Self::ShipNotBoardable => Some(1571),
            Self::NoActiveBeacon => Some(2211),
            Self::SkillMissing => None,
            Self::SkillUnavailableForPurchase => Some(7577),
            Self::JumpCloneNotFound => Some(1021),
            Self::NoCloneBayService => Some(7510),
            Self::AlreadyInNewbieShip => Some(7258),
            Self::CannotEjectFromCapsule => Some(3415),
            Self::CannotEjectActiveWeaponsTimer => Some(3874),
            Self::CannotEjectInvolvedInCrime => Some(3814),
            Self::CannotStoreShipActiveWeaponsTimer => Some(3875),
            Self::CannotBoardShipInHighSecAsCriminal => Some(4764),
            Self::CantDockGlobalCriminalFlag => Some(2537),
            Self::CantEnterWormholeGlobalCriminalFlag => Some(2844),
            Self::CantWarpAwayGlobalCriminalFlag => Some(3827),
            Self::ModuleAlreadyActive => Some(1224),
            Self::ModuleNotOnline => Some(1231),
            Self::MaxBandwidthExceeded => Some(2396),
            Self::TargetNotLocked => Some(1627),
            Self::InvalidTargetGroup => Some(987),
            Self::ShipHasNoFighterBay => Some(4890),
            Self::CannotLaunchFighterNotEnoughTubesAvailableForClass => Some(5112),
            Self::CannotOpenFighterTubesWhileCloaked => Some(4872),
            Self::CannotOpenFighterTubesWhileWarping => Some(4827),
            Self::CrpAccessDenied => Some(724),
            Self::AccessDenied => Some(7256),
            Self::InsufficientRoles => Some(3164),
            Self::NoOfficeAtStation => Some(2320),
            Self::AllianceNotFound => Some(43),
            Self::AlreadyAtWar => Some(3847),
            Self::FleetNotFound => Some(3093),
            Self::FleetNotInFleet => Some(2103),
            Self::FleetHangarAccessDenied => Some(3872),
            Self::CantbookmarkEveSystem => Some(1874),
            Self::DuelChallengeFailNoSesssionForTarget => Some(3920),
            Self::KillRightNotForSale => Some(3903),
            Self::InvalidPinType => Some(3268),
            Self::InvalidSchematic => Some(3276),
            Self::LinkAlreadyExists => Some(3263),
            Self::PISkillRequiredToSurveyPlanetResources => Some(3384),
            Self::SlashError => Some(1591),
            Self::UnMachoDestination => Some(1668),
        }
    }

    /// Returns `true` if this key is a hold/bay capacity `UserError` taking `{"free", "required"}`.
    pub const fn is_capacity_error(&self) -> bool {
        matches!(
            self,
            Self::NotEnoughCargoSpace
                | Self::NotEnoughCargoSpaceOverload
                | Self::NotEnoughDroneBaySpace
                | Self::NotEnoughDroneBaySpaceOverload
                | Self::NotEnoughFighterBaySpace
                | Self::NotEnoughSpecialBaySpace
                | Self::NotEnoughSpecialBaySpaceOverload
                | Self::NotEnoughSpace
                | Self::NotEnoughChargeSpace
                | Self::NoSpaceForThat
                | Self::NoSpaceForThatOverload
        )
    }

    /// Default typed dictionary arguments for keys whose V24 dialog template requires parameters.
    pub fn default_args(&self) -> Vec<(String, PyRep)> {
        if self.is_capacity_error() {
            return vec![
                ("free".into(), PyRep::Real(0.0)),
                ("required".into(), PyRep::Real(0.0)),
            ];
        }
        match self {
            Self::InsureShipFailedSingleContract => {
                vec![("ownerName".into(), PyRep::Integer(90_000_001))]
            }
            Self::MarketExceededOrderCount => vec![
                ("curCnt".into(), PyRep::Integer(0)),
                ("maxCnt".into(), PyRep::Integer(0)),
            ],
            Self::SlashError => vec![("reason".into(), PyRep::ascii_bytes(""))],
            Self::UnMachoDestination | Self::CustomInfo => {
                vec![("info".into(), PyRep::ascii_bytes(self.as_str()))]
            }
            Self::CustomNotify => vec![("notify".into(), PyRep::ascii_bytes(self.as_str()))],
            Self::CustomError => vec![("error".into(), PyRep::ascii_bytes(self.as_str()))],
            Self::CustomQuestion => vec![("question".into(), PyRep::ascii_bytes(self.as_str()))],
            _ => Vec::new(),
        }
    }

    /// Default dictionary arguments when converting a single-string [`BridgeRpcError`] into [`ClientError::User`].
    pub fn default_args_with_message(&self, message: &str) -> Vec<(String, PyRep)> {
        match self {
            Self::CustomInfo | Self::UnMachoDestination => {
                vec![("info".into(), PyRep::ascii_bytes(message))]
            }
            Self::CustomNotify => vec![("notify".into(), PyRep::ascii_bytes(message))],
            Self::CustomError => vec![("error".into(), PyRep::ascii_bytes(message))],
            Self::CustomQuestion => vec![("question".into(), PyRep::ascii_bytes(message))],
            Self::SlashError => vec![("reason".into(), PyRep::ascii_bytes(message))],
            _ => self.default_args(),
        }
    }
}

impl std::fmt::Display for UserErrorKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Typed server-to-client RPC error model (`C-5`, `#929`).
///
/// [`Self::to_macho_error_body`] is a **total function** over `ClientError`: wire exception
/// bodies are constructed directly from the enum variant and its typed `args` without ever
/// parsing or scraping human-readable `message` strings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ClientError {
    /// Named V24 `eveexceptions.UserError(key, dict(args))`.
    User {
        key: UserErrorKey,
        args: Vec<(String, PyRep)>,
    },
    /// Generic `eveexceptions.UserError('CustomInfo', {'info': msg})`.
    CustomInfo(String),
    /// Unimplemented RPC (`exceptions.RuntimeError("UnimplementedRPC: <service>.<method> is not yet implemented")`).
    Unimplemented { service: String, method: String },
    /// Internal / runtime exception (`exceptions.RuntimeError(msg)`).
    Internal(String),
    /// Session or bound-object mismatch (`eveexceptions.UserError('UnMachoDestination', {'info': msg})`).
    SessionMismatch(String),
}

impl ClientError {
    /// Constructs a [`ClientError::User`] with no extra dictionary arguments.
    pub fn user(key: UserErrorKey) -> Self {
        Self::User {
            key,
            args: Vec::new(),
        }
    }

    /// Constructs a [`ClientError::User`] with explicit dictionary arguments.
    pub fn user_with_args(
        key: UserErrorKey,
        args: impl IntoIterator<Item = (impl Into<String>, PyRep)>,
    ) -> Self {
        Self::User {
            key,
            args: args.into_iter().map(|(k, v)| (k.into(), v)).collect(),
        }
    }

    /// Total wire mapping from [`ClientError`] to a V24 `machoNetPacket.ErrorResponse` payload
    /// tuple `(original_command, MACHONETERR_WRAPPEDEXCEPTION, (marshal::encode(exc),))`.
    pub fn to_macho_error_body(&self, original_command: i64) -> PyRep {
        match self {
            Self::User { key, args } => {
                let mut merged: Vec<(String, PyRep)> = args.clone();
                for (def_k, def_v) in key.default_args() {
                    if !merged.iter().any(|(k, _)| k == &def_k) {
                        merged.push((def_k, def_v));
                    }
                }
                let dict_entries: Vec<(PyRep, PyRep)> = merged
                    .into_iter()
                    .map(|(k, v)| {
                        let wire_val = match v {
                            PyRep::String(s) => PyRep::ascii_bytes(&s),
                            other => other,
                        };
                        (PyRep::ascii_bytes(&k), wire_val)
                    })
                    .collect();
                encode_wrapped_user_error(original_command, key.as_str(), dict_entries)
            }
            Self::CustomInfo(info) => encode_wrapped_user_error(
                original_command,
                UserErrorKey::CustomInfo.as_str(),
                vec![(PyRep::ascii_bytes("info"), PyRep::ascii_bytes(info))],
            ),
            Self::Unimplemented { service, method } => {
                let msg = format!("UnimplementedRPC: {service}.{method} is not yet implemented");
                encode_wrapped_runtime_error(original_command, &msg)
            }
            Self::Internal(msg) => encode_wrapped_runtime_error(original_command, msg),
            Self::SessionMismatch(msg) => encode_wrapped_user_error(
                original_command,
                UserErrorKey::UnMachoDestination.as_str(),
                vec![(PyRep::ascii_bytes("info"), PyRep::ascii_bytes(msg))],
            ),
        }
    }
}

fn encode_wrapped_runtime_error(original_command: i64, message: &str) -> PyRep {
    let exc = PyRep::ObjectEx {
        is_type_2: false,
        header: Box::new(PyRep::Tuple(vec![
            PyRep::Token("exceptions.RuntimeError".into()),
            PyRep::Tuple(vec![PyRep::ascii_bytes(message)]),
        ])),
        list: Vec::new(),
        dict: Vec::new(),
    };
    PyRep::Tuple(vec![
        PyRep::Integer(original_command),
        PyRep::Integer(MACHONETERR_WRAPPEDEXCEPTION),
        PyRep::Tuple(vec![PyRep::Bytes(marshal::encode(&exc))]),
    ])
}

fn encode_wrapped_user_error(
    original_command: i64,
    user_error_name: &str,
    dict_entries: Vec<(PyRep, PyRep)>,
) -> PyRep {
    let exc = PyRep::ObjectEx {
        is_type_2: false,
        header: Box::new(PyRep::Tuple(vec![
            PyRep::Token("eveexceptions.UserError".into()),
            PyRep::Tuple(vec![
                PyRep::ascii_bytes(user_error_name),
                PyRep::Dict(dict_entries),
            ]),
        ])),
        list: Vec::new(),
        dict: Vec::new(),
    };
    PyRep::Tuple(vec![
        PyRep::Integer(original_command),
        PyRep::Integer(MACHONETERR_WRAPPEDEXCEPTION),
        PyRep::Tuple(vec![PyRep::Bytes(marshal::encode(&exc))]),
    ])
}

fn register_typed_client_error(code: &str, message: &str, client_error: ClientError) {
    if let Ok(mut guard) = TYPED_CLIENT_ERRORS.write() {
        if guard.len() >= MAX_TYPED_ERROR_CACHE_ENTRIES {
            guard.clear();
        }
        guard.insert((code.to_string(), message.to_string()), client_error);
    }
}

impl BridgeRpcError {
    /// Constructs a [`BridgeRpcError`] from a typed [`ClientError`] and records the lossless
    /// mapping so [`Self::to_client_error`] never parses the human-readable `message`.
    pub fn from_client_error(client_error: ClientError) -> Self {
        let (code, message) = match &client_error {
            ClientError::User { key, args } => {
                let code_str = key.as_str().to_string();
                let msg_str = if *key == UserErrorKey::InsureShipFailedSingleContract {
                    if let Some(owner_id) = args
                        .iter()
                        .find(|(k, _)| k == "ownerName")
                        .and_then(|(_, v)| v.as_i64())
                    {
                        format!("InsureShipFailedSingleContract (ownerName={owner_id})")
                    } else {
                        code_str.clone()
                    }
                } else if key.is_capacity_error() {
                    let free = args.iter().find(|(k, _)| k == "free").and_then(|(_, v)| {
                        v.as_f64().or_else(|| v.as_i64().map(|n| n as f64))
                    });
                    let req = args
                        .iter()
                        .find(|(k, _)| k == "required")
                        .and_then(|(_, v)| v.as_f64().or_else(|| v.as_i64().map(|n| n as f64)));
                    if let (Some(f), Some(r)) = (free, req) {
                        format!("free={f}, required={r}")
                    } else {
                        code_str.clone()
                    }
                } else if *key == UserErrorKey::MarketExceededOrderCount {
                    let cur = args
                        .iter()
                        .find(|(k, _)| k == "curCnt")
                        .and_then(|(_, v)| v.as_i64())
                        .unwrap_or(0);
                    let max = args
                        .iter()
                        .find(|(k, _)| k == "maxCnt")
                        .and_then(|(_, v)| v.as_i64())
                        .unwrap_or(0);
                    format!("Maximum open orders reached ({cur}/{max})")
                } else if args.len() == 1
                    && matches!(
                        args[0].0.as_str(),
                        "info" | "notify" | "error" | "reason" | "question"
                    )
                {
                    args[0].1.as_str().unwrap_or(&code_str).to_string()
                } else {
                    code_str.clone()
                };
                (code_str, msg_str)
            }
            ClientError::CustomInfo(info) => ("CustomInfo".into(), info.clone()),
            ClientError::Unimplemented { service, method } => (
                "UnimplementedRPC".into(),
                format!("{service}.{method} is not yet implemented"),
            ),
            ClientError::Internal(msg) => ("InternalError".into(), msg.clone()),
            ClientError::SessionMismatch(msg) => ("UnMachoDestination".into(), msg.clone()),
        };
        register_typed_client_error(&code, &message, client_error);
        Self { code, message }
    }

    /// Constructs a [`BridgeRpcError`] backed by [`ClientError::User`] with no extra kwargs.
    pub fn from_user_key(key: UserErrorKey) -> Self {
        Self::from_client_error(ClientError::user(key))
    }

    /// Constructs a [`BridgeRpcError`] backed by [`ClientError::User`] with typed kwargs.
    pub fn user_error_with_args(
        key: UserErrorKey,
        args: impl IntoIterator<Item = (impl Into<String>, PyRep)>,
    ) -> Self {
        Self::from_client_error(ClientError::user_with_args(key, args))
    }

    /// Constructs a [`BridgeRpcError`] with an explicit `(code, message)` pair and a typed
    /// [`ClientError`] wire representation.
    pub fn with_client_error(
        code: impl Into<String>,
        message: impl Into<String>,
        client_error: ClientError,
    ) -> Self {
        let code = code.into();
        let message = message.into();
        register_typed_client_error(&code, &message, client_error);
        Self { code, message }
    }

    /// Constructs a typed [`ClientError::CustomInfo`] RPC error.
    pub fn custom_info(info: impl Into<String>) -> Self {
        Self::from_client_error(ClientError::CustomInfo(info.into()))
    }

    /// Constructs a typed `MarketExceededOrderCount` (`MaxOpenOrdersReached`) error with
    /// integer `curCnt` and `maxCnt` kwargs.
    pub fn max_open_orders_reached(cur_cnt: i64, max_cnt: i64) -> Self {
        Self::with_client_error(
            "MaxOpenOrdersReached",
            format!("Maximum open orders reached ({cur_cnt}/{max_cnt})"),
            ClientError::user_with_args(
                UserErrorKey::MarketExceededOrderCount,
                [
                    ("curCnt", PyRep::Integer(cur_cnt)),
                    ("maxCnt", PyRep::Integer(max_cnt)),
                ],
            ),
        )
    }

    /// Constructs a typed `DockingApproach` (`OutsideDockingPerimeter`) error.
    pub fn outside_docking_perimeter(distance_m: f64, perimeter_m: f64) -> Self {
        Self::with_client_error(
            "OutsideDockingPerimeter",
            format!(
                "Ship is {distance_m:.1} m from docking target (must be within {perimeter_m:.0} m)"
            ),
            ClientError::user(UserErrorKey::DockingApproach),
        )
    }

    /// Converts this [`BridgeRpcError`] into a typed [`ClientError`] without parsing or scraping
    /// `self.message`.
    pub fn to_client_error(&self, service: &str, method: &str) -> ClientError {
        let is_slash_rpc =
            matches!(service.trim(), "slash" | "publicQaToolsServer") && method == "SlashCmd";

        if !is_slash_rpc {
            if let Ok(guard) = TYPED_CLIENT_ERRORS.read() {
                if let Some(typed) = guard.get(&(self.code.clone(), self.message.clone())) {
                    return typed.clone();
                }
            }
        }

        let err_msg = format!("{}: {}", self.code, self.message);
        match self.code.as_str() {
            "UnimplementedRPC" => ClientError::Unimplemented {
                service: service.to_string(),
                method: method.to_string(),
            },
            "InternalError" | "DatabasePersistenceError" => ClientError::Internal(err_msg),
            "FakeItemNotFound" | "CharacterNotAtStation" => {
                ClientError::Internal(self.code.clone())
            }
            "InvalidBoundObject" | "BoundObjectNotOwned" | "UnMachoDestination" => {
                ClientError::SessionMismatch(err_msg)
            }
            "SlashError" => ClientError::user_with_args(
                UserErrorKey::SlashError,
                [("reason", PyRep::ascii_bytes(&self.message))],
            ),
            "UnknownSlashCommand" | "InvalidSlashArgument" | "InvalidSlashArguments" => {
                ClientError::user_with_args(
                    UserErrorKey::SlashError,
                    [("reason", PyRep::ascii_bytes(&err_msg))],
                )
            }
            _ if is_slash_rpc => ClientError::user_with_args(
                UserErrorKey::SlashError,
                [("reason", PyRep::ascii_bytes(&err_msg))],
            ),
            "MaxOpenOrdersReached" => ClientError::User {
                key: UserErrorKey::MarketExceededOrderCount,
                args: UserErrorKey::MarketExceededOrderCount.default_args(),
            },
            "OutsideDockingPerimeter" | "DockingApproach" => {
                ClientError::user(UserErrorKey::DockingApproach)
            }
            "CustomInfo" => ClientError::CustomInfo(self.message.clone()),
            _ => {
                if let Some(key) = UserErrorKey::from_code(&self.code) {
                    ClientError::User {
                        key,
                        args: key.default_args_with_message(&self.message),
                    }
                } else {
                    ClientError::CustomInfo(err_msg)
                }
            }
        }
    }
}

impl From<ClientError> for BridgeRpcError {
    fn from(err: ClientError) -> Self {
        Self::from_client_error(err)
    }
}

impl From<UserErrorKey> for BridgeRpcError {
    fn from(key: UserErrorKey) -> Self {
        Self::from_user_key(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_user_error_keys_round_trip_and_produce_valid_macho_error_body() {
        for &key in UserErrorKey::ALL {
            assert_eq!(UserErrorKey::from_code(key.as_str()), Some(key));
            assert_eq!(key.to_string(), key.as_str());
            if key.v24_dialog_id().is_none() {
                assert!(
                    V24_PROGRAMMATIC_USER_ERROR_KEYS.contains(&key.as_str()),
                    "{key:?} must be in V24_PROGRAMMATIC_USER_ERROR_KEYS"
                );
            }
            let err = ClientError::user(key);
            let body = err.to_macho_error_body(6);
            let PyRep::Tuple(parts) = body else {
                panic!("expected ErrorResponse tuple");
            };
            assert_eq!(parts[0], PyRep::Integer(6));
            assert_eq!(parts[1], PyRep::Integer(MACHONETERR_WRAPPEDEXCEPTION));
            let PyRep::Tuple(inner) = &parts[2] else {
                panic!("expected inner payload tuple");
            };
            let PyRep::Bytes(raw) = &inner[0] else {
                panic!("expected marshaled exception bytes");
            };
            let PyRep::ObjectEx { header, .. } = marshal::decode(raw).unwrap() else {
                panic!("expected ObjectEx");
            };
            let PyRep::Tuple(h) = *header else {
                panic!("expected header tuple");
            };
            assert_eq!(h[0], PyRep::Token("eveexceptions.UserError".into()));
            let PyRep::Tuple(u_args) = &h[1] else {
                panic!("expected UserError args");
            };
            assert_eq!(u_args[0].as_str(), Some(key.as_str()));
        }
    }

    #[test]
    fn client_error_variants_map_totally_without_string_scraping() {
        let cargo = BridgeRpcError::user_error_with_args(
            UserErrorKey::NotEnoughCargoSpace,
            [
                ("free", PyRep::Real(15.5)),
                ("required", PyRep::Real(80.0)),
            ],
        );
        assert_eq!(cargo.code, "NotEnoughCargoSpace");
        assert_eq!(cargo.message, "free=15.5, required=80");
        assert_eq!(
            cargo.to_client_error("invbroker", "Add"),
            ClientError::User {
                key: UserErrorKey::NotEnoughCargoSpace,
                args: vec![
                    ("free".into(), PyRep::Real(15.5)),
                    ("required".into(), PyRep::Real(80.0)),
                ],
            }
        );

        let orders = BridgeRpcError::max_open_orders_reached(5, 5);
        assert_eq!(orders.code, "MaxOpenOrdersReached");
        assert_eq!(
            orders.to_client_error("marketProxy", "PlaceBuyOrder"),
            ClientError::User {
                key: UserErrorKey::MarketExceededOrderCount,
                args: vec![
                    ("curCnt".into(), PyRep::Integer(5)),
                    ("maxCnt".into(), PyRep::Integer(5)),
                ],
            }
        );

        let custom = BridgeRpcError::custom_info("Info message");
        assert_eq!(
            custom.to_client_error("svc", "m"),
            ClientError::CustomInfo("Info message".into())
        );

        let unimpl = ClientError::Unimplemented {
            service: "insuranceSvc".into(),
            method: "InsureShip".into(),
        };
        let body = unimpl.to_macho_error_body(6);
        let PyRep::Tuple(parts) = body else { panic!() };
        let PyRep::Tuple(inner) = &parts[2] else { panic!() };
        let PyRep::Bytes(raw) = &inner[0] else { panic!() };
        let PyRep::ObjectEx { header, .. } = marshal::decode(raw).unwrap() else {
            panic!()
        };
        let PyRep::Tuple(h) = *header else { panic!() };
        assert_eq!(h[0], PyRep::Token("exceptions.RuntimeError".into()));
    }
}
