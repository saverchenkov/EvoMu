//! Typed client error model (`ClientError`, `UserErrorKey`) and total `machoNet` wire mapping (`C-5`, `#929`).
//!
//! Replaces untyped `(code, message)` string-scraping recovery with a closed `ClientError`
//! sum type whose `to_macho_error_body` mapping is a total function.

#![forbid(unsafe_code)]

#[path = "client_error_facts.rs"]
mod client_error_facts;
pub use client_error_facts::RejectionFacts;

use crate::{marshal, BridgeRpcError, PyRep};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// `cluster.MACHONETERR_WRAPPEDEXCEPTION`: `ErrorResponse.payload` is `(DumpsSanitized(exc),)`.
pub const MACHONETERR_WRAPPEDEXCEPTION: i64 = 2;

/// Fixed-point ISK representation in cents (`1 ISK = 100 IskCents`) to prevent `f64` precision drift.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct IskCents(pub i64);

impl IskCents {
    pub const ZERO: Self = Self(0);

    pub const fn raw_cents(self) -> i64 {
        self.0
    }

    pub const fn from_raw_cents(cents: i64) -> Self {
        Self(cents)
    }

    pub const fn from_isk(whole_isk: i64) -> Self {
        Self(whole_isk.saturating_mul(100))
    }

    pub fn from_f64_isk(isk: f64) -> Self {
        if !isk.is_finite() {
            return Self(0);
        }
        let scaled = (isk * 100.0).round();
        if scaled >= (i64::MAX / 2) as f64 {
            Self(i64::MAX / 2)
        } else if scaled <= (i64::MIN / 2) as f64 {
            Self(i64::MIN / 2)
        } else {
            Self(scaled as i64)
        }
    }

    pub fn checked_add(self, rhs: Self) -> Option<Self> {
        if self.0 < 0 || rhs.0 < 0 {
            return None;
        }
        self.0.checked_add(rhs.0).map(Self)
    }

    pub fn checked_sub(self, rhs: Self) -> Option<Self> {
        if rhs.0 < 0 || self.0 < rhs.0 {
            return None;
        }
        self.0.checked_sub(rhs.0).map(Self)
    }

    pub fn checked_mul_qty(self, qty: i64) -> Option<Self> {
        if self.0 < 0 || qty < 0 {
            return None;
        }
        self.0.checked_mul(qty).map(Self)
    }

    pub fn to_wire_f64(self) -> f64 {
        (self.0 as f64) / 100.0
    }

    pub fn to_f64_isk(self) -> f64 {
        (self.0 as f64) / 100.0
    }
}

/// Validation or arithmetic error constructing or narrowing an [`ItemQty`] (`#1186`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ItemQtyError {
    /// Signed quantity is `<= 0` (must be strictly positive).
    NonPositive(i64),
    /// Unsigned quantity is `0` (must be strictly positive).
    Zero,
    /// Unsigned quantity exceeds `i64::MAX`.
    ExceedsMax(u64),
    /// Quantity exceeds `i32::MAX` when narrowing to an inventory stack size.
    ExceedsStackMax(i64),
    /// Arithmetic operation (`checked_add` / `checked_mul`) or narrowing cast overflowed.
    Overflow,
}

impl std::fmt::Display for ItemQtyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NonPositive(q) => write!(f, "Quantity must be positive, got {q}"),
            Self::Zero => f.write_str("Quantity must be positive, got 0"),
            Self::ExceedsMax(q) => write!(f, "Quantity {q} exceeds maximum {}", i64::MAX),
            Self::ExceedsStackMax(q) => {
                write!(f, "Quantity {q} exceeds stack maximum {}", i32::MAX)
            }
            Self::Overflow => f.write_str("Quantity arithmetic overflow"),
        }
    }
}

impl std::error::Error for ItemQtyError {}

impl ItemQtyError {
    /// Maps this quantity validation error into a [`BridgeRpcError`] preserving `code`.
    pub fn into_rpc_error(self, code: &str) -> BridgeRpcError {
        let msg = self.to_string();
        BridgeRpcError::with_client_error(code, msg.clone(), ClientError::CustomInfo(msg))
    }
}

/// Checked positive item/commodity quantity (`1..=i64::MAX`, `#1186`).
///
/// Guarantees strict positivity on construction and provides checked arithmetic and narrowing
/// conversions (`try_from_i64`, `try_from_u64`, `try_into_stack_i32`, `checked_add`,
/// `checked_mul`) with zero `as` casts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "i64", into = "i64")]
pub struct ItemQty(i64);

impl ItemQty {
    pub const MIN: Self = Self(1);
    pub const ONE: Self = Self(1);
    pub const STACK_MAX: Self = Self(2_147_483_647);
    pub const MAX: Self = Self(i64::MAX);

    /// Validates a strictly positive signed quantity (`1..=i64::MAX`).
    pub const fn try_from_i64(qty: i64) -> Result<Self, ItemQtyError> {
        if qty <= 0 {
            Err(ItemQtyError::NonPositive(qty))
        } else {
            Ok(Self(qty))
        }
    }

    /// Validates a strictly positive unsigned quantity (`1..=i64::MAX as u64`).
    pub fn try_from_u64(qty: u64) -> Result<Self, ItemQtyError> {
        if qty == 0 {
            return Err(ItemQtyError::Zero);
        }
        i64::try_from(qty)
            .map(Self)
            .map_err(|_| ItemQtyError::ExceedsMax(qty))
    }

    /// Validates a strictly positive 32-bit signed stack quantity (`1..=i32::MAX`).
    pub fn try_from_i32(qty: i32) -> Result<Self, ItemQtyError> {
        Self::try_from_i64(i64::from(qty))
    }

    /// Validates a strictly positive 32-bit unsigned quantity (`1..=u32::MAX`).
    pub fn try_from_u32(qty: u32) -> Result<Self, ItemQtyError> {
        Self::try_from_u64(u64::from(qty))
    }

    /// Validates a strictly positive `usize` quantity (`1..=i64::MAX`).
    pub fn try_from_usize(qty: usize) -> Result<Self, ItemQtyError> {
        if qty == 0 {
            return Err(ItemQtyError::Zero);
        }
        i64::try_from(qty).map(Self).map_err(|_| {
            ItemQtyError::ExceedsMax(u64::try_from(qty).unwrap_or(u64::MAX))
        })
    }

    /// Returns the underlying strictly positive `i64` quantity.
    pub const fn get(self) -> i64 {
        self.0
    }

    /// Alias for [`Self::get`].
    pub const fn as_i64(self) -> i64 {
        self.0
    }

    /// Returns the underlying strictly positive quantity as `u64`.
    pub fn as_u64(self) -> u64 {
        u64::try_from(self.0).unwrap_or(1)
    }

    /// Narrows this quantity to an `InventoryItemRecord` stack `i32`, failing if `> i32::MAX`.
    pub fn try_into_stack_i32(self) -> Result<i32, ItemQtyError> {
        i32::try_from(self.0).map_err(|_| ItemQtyError::ExceedsStackMax(self.0))
    }

    /// Narrows this quantity to a `u32`, failing if `> u32::MAX`.
    pub fn try_into_u32(self) -> Result<u32, ItemQtyError> {
        u32::try_from(self.0).map_err(|_| ItemQtyError::Overflow)
    }

    /// Checked addition (`self + rhs`), failing with [`ItemQtyError::Overflow`] on overflow.
    pub fn checked_add(self, rhs: Self) -> Result<Self, ItemQtyError> {
        self.0
            .checked_add(rhs.0)
            .map(Self)
            .ok_or(ItemQtyError::Overflow)
    }

    /// Checked subtraction returning `Some(self - rhs)` when `self > rhs`, or `None` if zero/underflow.
    pub const fn checked_sub(self, rhs: Self) -> Option<Self> {
        if self.0 <= rhs.0 {
            None
        } else {
            Some(Self(self.0 - rhs.0))
        }
    }

    /// Checked multiplication (`self * rhs`), failing with [`ItemQtyError::Overflow`] on overflow.
    pub fn checked_mul(self, rhs: Self) -> Result<Self, ItemQtyError> {
        self.0
            .checked_mul(rhs.0)
            .map(Self)
            .ok_or(ItemQtyError::Overflow)
    }

    /// Checked multiplication against an unsigned multiplier (`self * factor`).
    pub fn checked_mul_u64(self, factor: u64) -> Result<Self, ItemQtyError> {
        let rhs = Self::try_from_u64(factor)?;
        self.checked_mul(rhs)
    }
}

impl std::fmt::Display for ItemQty {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl TryFrom<i64> for ItemQty {
    type Error = ItemQtyError;

    fn try_from(value: i64) -> Result<Self, Self::Error> {
        Self::try_from_i64(value)
    }
}

impl TryFrom<u64> for ItemQty {
    type Error = ItemQtyError;

    fn try_from(value: u64) -> Result<Self, Self::Error> {
        Self::try_from_u64(value)
    }
}

impl TryFrom<i32> for ItemQty {
    type Error = ItemQtyError;

    fn try_from(value: i32) -> Result<Self, Self::Error> {
        Self::try_from_i32(value)
    }
}

impl TryFrom<u32> for ItemQty {
    type Error = ItemQtyError;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        Self::try_from_u32(value)
    }
}

impl TryFrom<usize> for ItemQty {
    type Error = ItemQtyError;

    fn try_from(value: usize) -> Result<Self, Self::Error> {
        Self::try_from_usize(value)
    }
}

impl TryFrom<ItemQty> for i32 {
    type Error = ItemQtyError;

    fn try_from(qty: ItemQty) -> Result<Self, Self::Error> {
        qty.try_into_stack_i32()
    }
}

impl TryFrom<ItemQty> for u32 {
    type Error = ItemQtyError;

    fn try_from(qty: ItemQty) -> Result<Self, Self::Error> {
        qty.try_into_u32()
    }
}

impl From<ItemQty> for i64 {
    fn from(qty: ItemQty) -> Self {
        qty.0
    }
}

impl From<ItemQty> for u64 {
    fn from(qty: ItemQty) -> Self {
        qty.as_u64()
    }
}

/// Canonical V24 localization label constant for supercapital/large-ship docking refusal.
pub const UI_LABEL_SHIP_TOO_LARGE: &str = "UI/Station/StationManagment/ShipTooLarge";

/// Canonical V24 localization label constant for closed-stargate jump refusal (`#1080`).
pub const UI_LABEL_GATE_CLOSED: &str = "UI/GateIcons/GateClosed";

/// Closed registry of V24 localization labels referenced by [`UserErrorArg::Label`] across server errors.
///
/// Every entry here (and every `UserErrorArg::Label("...")` literal in `crates/`) is verified against
/// V24 `res:/localizationfsd/localization_fsd_main.pickle` in `eve-testkit`.
pub const V24_USER_ERROR_LABELS: &[&str] = &[UI_LABEL_SHIP_TOO_LARGE, UI_LABEL_GATE_CLOSED];

/// Rounds a floating-point `UserError` argument to 6 decimal places to eliminate binary FP noise
/// while remaining numeric for V24 `NumericPropertyHandler` formatting (`#1086`).
pub fn round_user_error_real_6dp(value: f64) -> f64 {
    if !value.is_finite() {
        return 0.0;
    }
    let rounded = (value * 1_000_000.0).round() / 1_000_000.0;
    if rounded == 0.0 {
        0.0
    } else {
        rounded
    }
}

/// Typed V24 `eveexceptions.UserError` dictionary argument encoder (`#1086`).
///
/// Prevents untyped `PyRep` construction bugs at server call sites (such as sending raw integers for
/// `[4, typeID]` / `[101, "UI/..."]` client tags or IEEE-754 float tails on capacity messages).
#[derive(Debug, Clone, PartialEq)]
pub enum UserErrorArg {
    /// `const.UE_TYPEID` (`4`): renders as wire tuple `(4, typeID)` resolved by `cfg.FormatConvert`.
    TypeId(i64),
    /// `const.UE_LOC` (`101`): renders as wire tuple `(101, "UI/...")` resolved via `localization.GetByLabel`.
    Label(&'static str),
    /// Plain integer kwarg (e.g. `curCnt`, `maxCnt`, `limit`, `ownerName`, `{[item]x.name}` typeIDs).
    Int(i64),
    /// Floating-point numeric kwarg rounded to 6 decimal places.
    Real(f64),
    /// Text string kwarg.
    Str(String),
    /// Fixed-point ISK amount formatted as wire ISK float.
    IskCents(IskCents),
}

impl UserErrorArg {
    /// Materializes this typed argument into its V24 `eveMarshal` wire [`PyRep`].
    pub fn to_pyrep(&self) -> PyRep {
        match self {
            // `cfg.__prepdict` checks `type(v) is types.TupleType` (`PyRep::Tuple`, never `PyRep::List`).
            Self::TypeId(type_id) => {
                PyRep::Tuple(vec![PyRep::Integer(4), PyRep::Integer(*type_id)])
            }
            Self::Label(path) => {
                PyRep::Tuple(vec![PyRep::Integer(101), PyRep::String((*path).to_owned())])
            }
            Self::Int(v) => PyRep::Integer(*v),
            Self::Real(v) => PyRep::Real(round_user_error_real_6dp(*v)),
            Self::Str(s) => PyRep::String(s.clone()),
            Self::IskCents(cents) => PyRep::Real(round_user_error_real_6dp(cents.to_wire_f64())),
        }
    }

    /// Consumes this typed argument into its V24 `eveMarshal` wire [`PyRep`].
    pub fn into_pyrep(self) -> PyRep {
        match self {
            Self::TypeId(type_id) => {
                PyRep::Tuple(vec![PyRep::Integer(4), PyRep::Integer(type_id)])
            }
            Self::Label(path) => PyRep::Tuple(vec![PyRep::Integer(101), PyRep::String(path.to_owned())]),
            Self::Int(v) => PyRep::Integer(v),
            Self::Real(v) => PyRep::Real(round_user_error_real_6dp(v)),
            Self::Str(s) => PyRep::String(s),
            Self::IskCents(cents) => PyRep::Real(round_user_error_real_6dp(cents.to_wire_f64())),
        }
    }
}

#[derive(Serialize, Deserialize)]
enum SerdeUserErrorArg {
    TypeId(i64),
    Label(String),
    Int(i64),
    Real(f64),
    Str(String),
    IskCents(IskCents),
}

impl Serialize for UserErrorArg {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let proxy = match self {
            Self::TypeId(v) => SerdeUserErrorArg::TypeId(*v),
            Self::Label(s) => SerdeUserErrorArg::Label((*s).to_owned()),
            Self::Int(v) => SerdeUserErrorArg::Int(*v),
            Self::Real(v) => SerdeUserErrorArg::Real(*v),
            Self::Str(s) => SerdeUserErrorArg::Str(s.clone()),
            Self::IskCents(c) => SerdeUserErrorArg::IskCents(*c),
        };
        proxy.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for UserErrorArg {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let proxy = SerdeUserErrorArg::deserialize(deserializer)?;
        Ok(match proxy {
            SerdeUserErrorArg::TypeId(v) => Self::TypeId(v),
            SerdeUserErrorArg::Label(s) => {
                let static_label: &'static str = V24_USER_ERROR_LABELS
                    .iter()
                    .copied()
                    .find(|&known| known == s)
                    .unwrap_or_else(|| Box::leak(s.into_boxed_str()));
                Self::Label(static_label)
            }
            SerdeUserErrorArg::Int(v) => Self::Int(v),
            SerdeUserErrorArg::Real(v) => Self::Real(v),
            SerdeUserErrorArg::Str(s) => Self::Str(s),
            SerdeUserErrorArg::IskCents(c) => Self::IskCents(c),
        })
    }
}

impl From<i64> for UserErrorArg {
    fn from(v: i64) -> Self {
        Self::Int(v)
    }
}

impl From<i32> for UserErrorArg {
    fn from(v: i32) -> Self {
        Self::Int(i64::from(v))
    }
}

impl From<u32> for UserErrorArg {
    fn from(v: u32) -> Self {
        Self::Int(i64::from(v))
    }
}

impl From<f64> for UserErrorArg {
    fn from(v: f64) -> Self {
        Self::Real(v)
    }
}

impl From<String> for UserErrorArg {
    fn from(s: String) -> Self {
        Self::Str(s)
    }
}

impl From<&str> for UserErrorArg {
    fn from(s: &str) -> Self {
        Self::Str(s.to_owned())
    }
}

impl From<IskCents> for UserErrorArg {
    fn from(c: IskCents) -> Self {
        Self::IskCents(c)
    }
}

impl From<ItemQty> for UserErrorArg {
    fn from(qty: ItemQty) -> Self {
        Self::Int(qty.as_i64())
    }
}

/// Trait implemented by `(K, UserErrorArg)` pairs and `&[(&str, UserErrorArg)]` slice elements so
/// [`ClientError::user_with_args`] and [`BridgeRpcError::user_error_with_args`] accept both owned
/// arrays/iterators and `&[(&str, UserErrorArg)]` slices cleanly while forbidding untyped `PyRep`.
pub trait IntoUserErrorArg {
    fn into_user_error_pair(self) -> (String, PyRep);
}

impl<K: Into<String>> IntoUserErrorArg for (K, UserErrorArg) {
    fn into_user_error_pair(self) -> (String, PyRep) {
        (self.0.into(), self.1.into_pyrep())
    }
}

impl<'a> IntoUserErrorArg for &'a (&'a str, UserErrorArg) {
    fn into_user_error_pair(self) -> (String, PyRep) {
        ((*self.0).to_owned(), self.1.to_pyrep())
    }
}

impl<'a> IntoUserErrorArg for &'a (String, UserErrorArg) {
    fn into_user_error_pair(self) -> (String, PyRep) {
        (self.0.clone(), self.1.to_pyrep())
    }
}

/// Client UI presentation discipline mandated per [`UserErrorKey`] from V24 `dialogs.static` (`#1086`).
///
/// `Notify` corresponds to V24 `dialogType` `1` (`hint`) and `3` (`notify`), routed through
/// `player_messaging/client/dialog_handler.pyj::message_player` as toast/hint notifications.
/// `Modal` corresponds to `dialogType` `2` (`info`), `4` (`question`), `6` (`warning`), `7` (`error`),
/// `8` (`fatal`), `9` (`windowhelp`), and programmatic `UserError` popups routed through `_prompt_player`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum UserErrorPresentation {
    Modal,
    Notify,
}

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
    NotEnoughCargoSpaceFor1Unit,
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
    NotCloseEnoughToJump,
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
    WarpTooShort,
    WarpDestinationGone,

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
    EffectAlreadyActive2,
    ModuleNotOnline,
    NoCharges,
    MaxBandwidthExceeded,
    TargetNotLocked,
    TargetTooFar,
    TargetingAptitudeAlreadyFullyUtilized,
    TargetingSystemsAlreadyFullyUtilized,
    TargetingSystemsBeingJammed,
    InvalidTargetGroup,
    ShipHasNoFighterBay,
    CannotLaunchFighterNotEnoughTubesAvailableForClass,
    CannotOpenFighterTubesWhileCloaked,
    CannotOpenFighterTubesWhileWarping,
    AoeModuleActivationDeniedSafety,

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

    // --- V24 Structure, Fitting, Combat, Contracts & Navigation Rejections (#1195) ---
    CannotUseFittingInStructureWithoutDeed,
    CannotRepairInStructureWithoutDeed,
    StructureServiceUnavailable,
    StructureDockingDenied,
    NotDockable,
    NotEnoughEnergy,
    JumpNotEnoughCap,
    CantShipNotPresent,
    ModuleFitFailed,
    CannotPerformOnMultipleItems,
    InvalidTargetForModule,
    DeniedTargetSelf,
    CantDockAfterAggression,
    CantJumpAfterAggression,
    ConContractNotOutstanding,
    ConContractNotInProgress,
    ConContractInvalid,
    ConAuctionHasBids,
    ModuleNoLongerPresentForCharges,
}

impl UserErrorKey {
    /// Exhaustive slice of all 118 typed [`UserErrorKey`] variants.
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
        Self::NotEnoughCargoSpaceFor1Unit,
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
        Self::NotCloseEnoughToJump,
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
        Self::WarpTooShort,
        Self::WarpDestinationGone,
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
        Self::EffectAlreadyActive2,
        Self::ModuleNotOnline,
        Self::NoCharges,
        Self::MaxBandwidthExceeded,
        Self::TargetNotLocked,
        Self::TargetTooFar,
        Self::TargetingAptitudeAlreadyFullyUtilized,
        Self::TargetingSystemsAlreadyFullyUtilized,
        Self::TargetingSystemsBeingJammed,
        Self::InvalidTargetGroup,
        Self::ShipHasNoFighterBay,
        Self::CannotLaunchFighterNotEnoughTubesAvailableForClass,
        Self::CannotOpenFighterTubesWhileCloaked,
        Self::CannotOpenFighterTubesWhileWarping,
        Self::AoeModuleActivationDeniedSafety,
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
        Self::CannotUseFittingInStructureWithoutDeed,
        Self::CannotRepairInStructureWithoutDeed,
        Self::StructureServiceUnavailable,
        Self::StructureDockingDenied,
        Self::NotDockable,
        Self::NotEnoughEnergy,
        Self::JumpNotEnoughCap,
        Self::CantShipNotPresent,
        Self::ModuleFitFailed,
        Self::CannotPerformOnMultipleItems,
        Self::InvalidTargetForModule,
        Self::DeniedTargetSelf,
        Self::CantDockAfterAggression,
        Self::CantJumpAfterAggression,
        Self::ConContractNotOutstanding,
        Self::ConContractNotInProgress,
        Self::ConContractInvalid,
        Self::ConAuctionHasBids,
        Self::ModuleNoLongerPresentForCharges,
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
            Self::NotEnoughCargoSpaceFor1Unit => "NotEnoughCargoSpaceFor1Unit",
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
            Self::NotCloseEnoughToJump => "NotCloseEnoughToJump",
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
            Self::WarpTooShort => "WarpTooShort",
            Self::WarpDestinationGone => "WarpDestinationGone",
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
            Self::EffectAlreadyActive2 => "EffectAlreadyActive2",
            Self::ModuleNotOnline => "ModuleNotOnline",
            Self::NoCharges => "NoCharges",
            Self::MaxBandwidthExceeded => "MaxBandwidthExceeded",
            Self::TargetNotLocked => "TargetNotLocked",
            Self::TargetTooFar => "TargetTooFar",
            Self::TargetingAptitudeAlreadyFullyUtilized => "TargetingAptitudeAlreadyFullyUtilized",
            Self::TargetingSystemsAlreadyFullyUtilized => "TargetingSystemsAlreadyFullyUtilized",
            Self::TargetingSystemsBeingJammed => "TargetingSystemsBeingJammed",
            Self::InvalidTargetGroup => "InvalidTargetGroup",
            Self::ShipHasNoFighterBay => "ShipHasNoFighterBay",
            Self::CannotLaunchFighterNotEnoughTubesAvailableForClass => {
                "CannotLaunchFighterNotEnoughTubesAvailableForClass"
            }
            Self::CannotOpenFighterTubesWhileCloaked => "CannotOpenFighterTubesWhileCloaked",
            Self::CannotOpenFighterTubesWhileWarping => "CannotOpenFighterTubesWhileWarping",
            Self::AoeModuleActivationDeniedSafety => "AoeModuleActivationDeniedSafety",
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
            Self::CannotUseFittingInStructureWithoutDeed => {
                "CannotUseFittingInStructureWithoutDeed"
            }
            Self::CannotRepairInStructureWithoutDeed => {
                "CannotRepairInStructureWithoutDeed"
            }
            Self::StructureServiceUnavailable => "StructureServiceUnavailable",
            Self::StructureDockingDenied => "StructureDockingDenied",
            Self::NotDockable => "NotDockable",
            Self::NotEnoughEnergy => "NotEnoughEnergy",
            Self::JumpNotEnoughCap => "JumpNotEnoughCap",
            Self::CantShipNotPresent => "CantShipNotPresent",
            Self::ModuleFitFailed => "ModuleFitFailed",
            Self::CannotPerformOnMultipleItems => "CannotPerformOnMultipleItems",
            Self::InvalidTargetForModule => "InvalidTargetForModule",
            Self::DeniedTargetSelf => "DeniedTargetSelf",
            Self::CantDockAfterAggression => "CantDockAfterAggression",
            Self::CantJumpAfterAggression => "CantJumpAfterAggression",
            Self::ConContractNotOutstanding => "ConContractNotOutstanding",
            Self::ConContractNotInProgress => "ConContractNotInProgress",
            Self::ConContractInvalid => "ConContractInvalid",
            Self::ConAuctionHasBids => "ConAuctionHasBids",
            Self::ModuleNoLongerPresentForCharges => "ModuleNoLongerPresentForCharges",
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
            Self::NotEnoughCargoSpaceFor1Unit => Some(1337),
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
            Self::NotCloseEnoughToJump => Some(1329),
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
            Self::WarpTooShort => Some(3435),
            Self::WarpDestinationGone => Some(1679),
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
            Self::EffectAlreadyActive2 => Some(3252),
            Self::ModuleNotOnline => Some(1231),
            Self::NoCharges => Some(1291),
            Self::MaxBandwidthExceeded => Some(2396),
            Self::TargetNotLocked => Some(1627),
            Self::TargetTooFar => Some(1632),
            Self::TargetingAptitudeAlreadyFullyUtilized => Some(1634),
            Self::TargetingSystemsAlreadyFullyUtilized => Some(1636),
            Self::TargetingSystemsBeingJammed => Some(1637),
            Self::InvalidTargetGroup => Some(987),
            Self::ShipHasNoFighterBay => Some(4890),
            Self::CannotLaunchFighterNotEnoughTubesAvailableForClass => Some(5112),
            Self::CannotOpenFighterTubesWhileCloaked => Some(4872),
            Self::CannotOpenFighterTubesWhileWarping => Some(4827),
            Self::AoeModuleActivationDeniedSafety => Some(3891),
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
            Self::CannotUseFittingInStructureWithoutDeed => Some(7470),
            Self::CannotRepairInStructureWithoutDeed => Some(7469),
            Self::StructureServiceUnavailable => Some(4986),
            Self::StructureDockingDenied => Some(4886),
            Self::NotDockable => Some(1334),
            Self::NotEnoughEnergy => Some(1344),
            Self::JumpNotEnoughCap => Some(1036),
            Self::CantShipNotPresent => Some(422),
            Self::ModuleFitFailed => Some(1228),
            Self::CannotPerformOnMultipleItems => Some(2271),
            Self::InvalidTargetForModule => Some(7824),
            Self::DeniedTargetSelf => Some(788),
            Self::CantDockAfterAggression => Some(377),
            Self::CantJumpAfterAggression => Some(385),
            Self::ConContractNotOutstanding => Some(2015),
            Self::ConContractNotInProgress => Some(2085),
            Self::ConContractInvalid => Some(5048),
            Self::ConAuctionHasBids => Some(2045),
            Self::ModuleNoLongerPresentForCharges => Some(1230),
        }
    }

    /// Expected `dialogType` enum byte in V24.01 `res:/staticdata/dialogs.static`, or `None` for
    /// [`V24_PROGRAMMATIC_USER_ERROR_KEYS`]:
    /// `1`=`hint`, `2`=`info`, `3`=`notify`, `4`=`question`, `5`=`audio`, `6`=`warning`, `7`=`error`,
    /// `8`=`fatal`, `9`=`windowhelp`.
    pub const fn v24_dialog_type(&self) -> Option<u8> {
        match self {
            Self::InsureShipInvalidCost
            | Self::InsureShipFailed
            | Self::CanOnlyDoInStation
            | Self::SkillMissing => None,
            Self::OwnerMaxFittings
            | Self::CannotStoreShipActiveWeaponsTimer
            | Self::EffectAlreadyActive2
            | Self::InsufficientRoles
            | Self::KillRightNotForSale
            | Self::LinkAlreadyExists => Some(1),
            Self::InsureShipFailedSingleContract
            | Self::InsureShipFailedMultipleContracts
            | Self::CustomInfo
            | Self::NotEnoughCargoSpace
            | Self::NotEnoughCargoSpaceFor1Unit
            | Self::NotEnoughCargoSpaceOverload
            | Self::NotEnoughDroneBaySpaceOverload
            | Self::NotEnoughSpecialBaySpaceOverload
            | Self::NotEnoughSpace
            | Self::NoSpaceForThat
            | Self::NoSpaceForThatOverload
            | Self::ItemLocked
            | Self::ItemNotFound
            | Self::CanNotUnassembleInThisItemLoc
            | Self::CantRepackageDamagedItem
            | Self::MarketExceededOrderCount
            | Self::NotEnoughMoney
            | Self::ShipCannotUseStargate
            | Self::DunBlacklistCannotWarp
            | Self::CanOnlyDoInStations
            | Self::NoActiveBeacon
            | Self::SkillUnavailableForPurchase
            | Self::NoCloneBayService
            | Self::CantDockGlobalCriminalFlag
            | Self::CantEnterWormholeGlobalCriminalFlag
            | Self::CantWarpAwayGlobalCriminalFlag
            | Self::ModuleAlreadyActive
            | Self::InvalidTargetGroup
            | Self::CrpAccessDenied
            | Self::NoOfficeAtStation
            | Self::FleetNotFound
            | Self::FleetNotInFleet
            | Self::CantbookmarkEveSystem
            | Self::UnMachoDestination
            | Self::CannotPerformOnMultipleItems
            | Self::ConAuctionHasBids
            | Self::ConContractInvalid
            | Self::ConContractNotInProgress
            | Self::ConContractNotOutstanding
            | Self::JumpNotEnoughCap
            | Self::ModuleFitFailed
            | Self::NotDockable => Some(2),
            Self::CustomNotify
            | Self::NotEnoughDroneBaySpace
            | Self::NotEnoughFighterBaySpace
            | Self::NotEnoughSpecialBaySpace
            | Self::NotEnoughChargeSpace
            | Self::ItemNotAtStation
            | Self::CantDoThatWithSomeoneElsesStuff
            | Self::SlotAlreadyOccupied
            | Self::DockingApproach
            | Self::DockingRequestDenied
            | Self::CantDockWhileModuleActive
            | Self::CantJumpWhileModuleActive
            | Self::WarpDisrupted
            | Self::WarpScrambled
            | Self::DeniedTethered
            | Self::NotCloseEnoughToJump
            | Self::SystemCheck_JumpFailed_StandingsTooLow
            | Self::CannotDscanInThisSystem
            | Self::MustBeDocked
            | Self::ShipCloaked
            | Self::ShipInWarp
            | Self::ShipNotBoardable
            | Self::WarpTooShort
            | Self::WarpDestinationGone
            | Self::JumpCloneNotFound
            | Self::AlreadyInNewbieShip
            | Self::CannotEjectFromCapsule
            | Self::CannotEjectActiveWeaponsTimer
            | Self::CannotEjectInvolvedInCrime
            | Self::CannotBoardShipInHighSecAsCriminal
            | Self::ModuleNotOnline
            | Self::NoCharges
            | Self::MaxBandwidthExceeded
            | Self::TargetNotLocked
            | Self::TargetTooFar
            | Self::TargetingAptitudeAlreadyFullyUtilized
            | Self::TargetingSystemsAlreadyFullyUtilized
            | Self::TargetingSystemsBeingJammed
            | Self::ShipHasNoFighterBay
            | Self::CannotLaunchFighterNotEnoughTubesAvailableForClass
            | Self::CannotOpenFighterTubesWhileCloaked
            | Self::CannotOpenFighterTubesWhileWarping
            | Self::AoeModuleActivationDeniedSafety
            | Self::AccessDenied
            | Self::AllianceNotFound
            | Self::AlreadyAtWar
            | Self::FleetHangarAccessDenied
            | Self::DuelChallengeFailNoSesssionForTarget
            | Self::InvalidPinType
            | Self::InvalidSchematic
            | Self::PISkillRequiredToSurveyPlanetResources
            | Self::SlashError
            | Self::CannotRepairInStructureWithoutDeed
            | Self::CannotUseFittingInStructureWithoutDeed
            | Self::CantDockAfterAggression
            | Self::CantJumpAfterAggression
            | Self::CantShipNotPresent
            | Self::DeniedTargetSelf
            | Self::InvalidTargetForModule
            | Self::ModuleNoLongerPresentForCharges
            | Self::NotEnoughEnergy
            | Self::StructureDockingDenied
            | Self::StructureServiceUnavailable => Some(3),
            Self::InsureShipAlreadyInsured | Self::CustomQuestion => Some(4),
            Self::CustomError => Some(7),
        }
    }

    /// Fixed UI presentation discipline for this [`UserErrorKey`] matching V24 `dialog_handler.pyj` (`#1086`).
    ///
    /// Call sites cannot override modal vs toast behavior per call: each key's presentation is
    /// fixed by V24 `dialogs.static` (`hint`/`notify` -> [`UserErrorPresentation::Notify`], all
    /// modal dialog types & programmatic prompts -> [`UserErrorPresentation::Modal`]).
    pub const fn presentation(&self) -> UserErrorPresentation {
        match self.v24_dialog_type() {
            Some(1 | 3) => UserErrorPresentation::Notify,
            _ => UserErrorPresentation::Modal,
        }
    }

    /// Returns `true` if this key is a hold/bay capacity `UserError` taking `{"volume", "available"}`.
    pub const fn is_capacity_error(&self) -> bool {
        matches!(
            self,
            Self::NotEnoughCargoSpace
                | Self::NotEnoughDroneBaySpace
                | Self::NotEnoughFighterBaySpace
                | Self::NotEnoughSpecialBaySpace
                | Self::NotEnoughSpace
        )
    }

    /// Default typed dictionary arguments for keys whose V24 dialog template requires parameters.
    pub fn default_args(&self) -> Vec<(String, PyRep)> {
        if self.is_capacity_error() {
            return vec![
                ("volume".into(), PyRep::Real(0.0)),
                ("available".into(), PyRep::Real(0.0)),
            ];
        }
        match self {
            Self::NoSpaceForThat => vec![
                ("itemTypeName".into(), PyRep::Integer(0)),
                ("itemVolume".into(), PyRep::Real(0.0)),
                ("volumeAvailable".into(), PyRep::Real(0.0)),
            ],
            Self::NoSpaceForThatOverload => vec![
                ("item".into(), PyRep::Integer(0)),
                ("maximum".into(), PyRep::Real(0.0)),
                ("used".into(), PyRep::Real(0.0)),
            ],
            Self::NotEnoughCargoSpaceFor1Unit => vec![
                ("type".into(), PyRep::Integer(0)),
                ("free".into(), PyRep::Real(0.0)),
                ("required".into(), PyRep::Real(0.0)),
            ],
            Self::NotEnoughChargeSpace => vec![
                ("capacity".into(), PyRep::Real(0.0)),
                ("volume".into(), PyRep::Real(0.0)),
            ],
            Self::NotEnoughCargoSpaceOverload
            | Self::NotEnoughDroneBaySpaceOverload
            | Self::NotEnoughSpecialBaySpaceOverload => vec![
                ("item".into(), PyRep::ascii_bytes("")),
                ("maximum".into(), PyRep::Real(0.0)),
                ("used".into(), PyRep::Real(0.0)),
            ],
            Self::MarketExceededOrderCount => vec![
                ("curCnt".into(), PyRep::Integer(0)),
                ("maxCnt".into(), PyRep::Integer(0)),
            ],
            Self::SlashError => vec![("reason".into(), PyRep::ascii_bytes(""))],
            Self::DockingRequestDenied => vec![("reason".into(), PyRep::ascii_bytes(""))],
            Self::NoCharges => vec![("launcher".into(), PyRep::ascii_bytes(""))],
            Self::TargetTooFar => vec![
                ("targetname".into(), PyRep::ascii_bytes("")),
                ("distance".into(), PyRep::Real(0.0)),
            ],
            Self::TargetingAptitudeAlreadyFullyUtilized
            | Self::TargetingSystemsAlreadyFullyUtilized => {
                vec![("limit".into(), PyRep::Integer(0))]
            }
            Self::TargetingSystemsBeingJammed => {
                vec![("targetname".into(), PyRep::ascii_bytes(""))]
            }
            Self::EffectAlreadyActive2 => {
                vec![("modulename".into(), PyRep::ascii_bytes(""))]
            }
            Self::AoeModuleActivationDeniedSafety => {
                vec![("moduleName".into(), PyRep::ascii_bytes(""))]
            }
            Self::CustomInfo => vec![("info".into(), PyRep::ascii_bytes(self.as_str()))],
            Self::UnMachoDestination => {
                vec![("info".into(), PyRep::ascii_bytes(self.as_str()))]
            }
            Self::CustomNotify => vec![("notify".into(), PyRep::ascii_bytes(self.as_str()))],
            Self::CustomError => vec![("error".into(), PyRep::ascii_bytes(self.as_str()))],
            Self::CustomQuestion => vec![("question".into(), PyRep::ascii_bytes(self.as_str()))],
            Self::NotCloseEnoughToJump => vec![("maxdist".into(), PyRep::Integer(2_500))],
            Self::InvalidTargetGroup => vec![("groupName".into(), PyRep::ascii_bytes(""))],
            Self::CannotUseFittingInStructureWithoutDeed
            | Self::CannotRepairInStructureWithoutDeed
            | Self::StructureDockingDenied => {
                vec![("structureName".into(), PyRep::ascii_bytes(""))]
            }
            Self::StructureServiceUnavailable => vec![
                ("serviceName".into(), PyRep::ascii_bytes("")),
                ("structureName".into(), PyRep::ascii_bytes("")),
            ],
            Self::NotEnoughEnergy => vec![
                ("effectname".into(), PyRep::ascii_bytes("")),
                ("need".into(), PyRep::Real(0.0)),
                ("got".into(), PyRep::Real(0.0)),
            ],
            Self::JumpNotEnoughCap => vec![
                ("need".into(), PyRep::Real(0.0)),
                ("got".into(), PyRep::Real(0.0)),
            ],
            Self::ModuleFitFailed => vec![
                ("moduleName".into(), PyRep::ascii_bytes("Module")),
                ("reason".into(), PyRep::ascii_bytes("")),
            ],
            Self::InvalidTargetForModule => {
                vec![("moduleName".into(), PyRep::ascii_bytes(""))]
            }
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
            Self::ModuleFitFailed => vec![
                ("moduleName".into(), PyRep::ascii_bytes("Module")),
                ("reason".into(), PyRep::ascii_bytes(message)),
            ],
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
    /// Named V24 `eveexceptions.UserError(key, dict(args))` paired with structured game-state rejection facts (`#1195`).
    Rejected {
        key: UserErrorKey,
        args: Vec<(String, PyRep)>,
        facts: RejectionFacts,
    },
    /// Generic modal `eveexceptions.UserError('CustomInfo', {'info': msg})`.
    CustomInfo(String),
    /// Generic toast/non-modal `eveexceptions.UserError('CustomNotify', {'notify': msg})`.
    CustomNotify(String),
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

    /// Constructs a [`ClientError::User`] with typed dictionary arguments (`#1086`).
    ///
    /// Call sites must supply [`UserErrorArg`] values (never raw untyped [`PyRep`]).
    pub fn user_with_args(
        key: UserErrorKey,
        args: impl IntoIterator<Item = impl IntoUserErrorArg>,
    ) -> Self {
        Self::User {
            key,
            args: args
                .into_iter()
                .map(IntoUserErrorArg::into_user_error_pair)
                .collect(),
        }
    }

    /// Constructs a [`ClientError::Rejected`] with no extra dictionary arguments and structured [`RejectionFacts`].
    pub fn rejected(key: UserErrorKey, facts: RejectionFacts) -> Self {
        Self::Rejected {
            key,
            args: Vec::new(),
            facts,
        }
    }

    /// Constructs a [`ClientError::Rejected`] with typed dictionary arguments and structured [`RejectionFacts`].
    pub fn rejected_with_args(
        key: UserErrorKey,
        args: impl IntoIterator<Item = impl IntoUserErrorArg>,
        facts: RejectionFacts,
    ) -> Self {
        Self::Rejected {
            key,
            args: args
                .into_iter()
                .map(IntoUserErrorArg::into_user_error_pair)
                .collect(),
            facts,
        }
    }

    /// Returns the typed [`UserErrorKey`] and dictionary args for both `User` and `Rejected`.
    pub fn as_user_error(&self) -> Option<(UserErrorKey, &[(String, PyRep)])> {
        match self {
            Self::User { key, args } | Self::Rejected { key, args, .. } => {
                Some((*key, args.as_slice()))
            }
            _ => None,
        }
    }

    /// Returns structured rejection facts if this [`ClientError`] is [`ClientError::Rejected`].
    pub const fn rejection_facts(&self) -> Option<&RejectionFacts> {
        match self {
            Self::Rejected { facts, .. } => Some(facts),
            _ => None,
        }
    }

    /// Returns the V24 client UI presentation mode (`Modal` vs `Notify`) enforced for this error.
    pub const fn presentation(&self) -> UserErrorPresentation {
        match self {
            Self::User { key, .. } | Self::Rejected { key, .. } => key.presentation(),
            Self::CustomInfo(_)
            | Self::Unimplemented { .. }
            | Self::Internal(_)
            | Self::SessionMismatch(_) => UserErrorPresentation::Modal,
            Self::CustomNotify(_) => UserErrorPresentation::Notify,
        }
    }

    /// Constructs a typed hold/bay space capacity error (`{volume, available}` rounded to 6 dp).
    pub fn hold_capacity_error(
        key: UserErrorKey,
        volume_needed_m3: f64,
        volume_available_m3: f64,
    ) -> Self {
        debug_assert!(matches!(
            key,
            UserErrorKey::NotEnoughCargoSpace
                | UserErrorKey::NotEnoughDroneBaySpace
                | UserErrorKey::NotEnoughFighterBaySpace
                | UserErrorKey::NotEnoughSpecialBaySpace
                | UserErrorKey::NotEnoughSpace
        ));
        Self::user_with_args(
            key,
            [
                ("volume", UserErrorArg::Real(volume_needed_m3)),
                ("available", UserErrorArg::Real(volume_available_m3)),
            ],
        )
    }

    /// Constructs a typed `NotEnoughChargeSpace` (`{capacity, volume}` rounded to 6 dp).
    pub fn not_enough_charge_space(capacity_m3: f64, volume_needed_m3: f64) -> Self {
        Self::user_with_args(
            UserErrorKey::NotEnoughChargeSpace,
            [
                ("capacity", UserErrorArg::Real(capacity_m3)),
                ("volume", UserErrorArg::Real(volume_needed_m3)),
            ],
        )
    }

    /// Constructs a typed `NoSpaceForThat` (`{itemTypeName, itemVolume, volumeAvailable}`).
    pub fn no_space_for_that(
        item_type_id: i64,
        item_volume_m3: f64,
        volume_available_m3: f64,
    ) -> Self {
        Self::user_with_args(
            UserErrorKey::NoSpaceForThat,
            [
                ("itemTypeName", UserErrorArg::Int(item_type_id)),
                ("itemVolume", UserErrorArg::Real(item_volume_m3)),
                ("volumeAvailable", UserErrorArg::Real(volume_available_m3)),
            ],
        )
    }

    /// Constructs a typed `NotEnoughCargoSpaceFor1Unit` (`{type, required, free}`).
    pub fn not_enough_cargo_space_for_unit(type_id: i64, required_m3: f64, free_m3: f64) -> Self {
        Self::user_with_args(
            UserErrorKey::NotEnoughCargoSpaceFor1Unit,
            [
                ("type", UserErrorArg::Int(type_id)),
                ("required", UserErrorArg::Real(required_m3)),
                ("free", UserErrorArg::Real(free_m3)),
            ],
        )
    }

    /// Constructs a typed `DockingRequestDenied` error with a V24 localization label (`[101, label]`).
    pub fn docking_request_denied(reason_label: &'static str) -> Self {
        Self::user_with_args(
            UserErrorKey::DockingRequestDenied,
            [("reason", UserErrorArg::Label(reason_label))],
        )
    }

    /// Constructs a typed `NoCharges` (`{launcher: [4, typeID]}`).
    pub fn no_charges(launcher_type_id: i64) -> Self {
        Self::user_with_args(
            UserErrorKey::NoCharges,
            [("launcher", UserErrorArg::TypeId(launcher_type_id))],
        )
    }

    /// Constructs a typed `TargetTooFar` (`{targetname, module, distance}`).
    pub fn target_too_far(
        target_name: impl Into<String>,
        module_type_id: i64,
        distance_m: f64,
    ) -> Self {
        Self::user_with_args(
            UserErrorKey::TargetTooFar,
            [
                ("targetname", UserErrorArg::Str(target_name.into())),
                ("module", UserErrorArg::Int(module_type_id)),
                ("distance", UserErrorArg::Real(distance_m)),
            ],
        )
    }

    /// Constructs a typed `ShipCannotUseStargate` (`{shipTypeID, whitelist}`).
    pub fn ship_cannot_use_stargate(ship_type_id: i64, whitelist: impl Into<String>) -> Self {
        Self::user_with_args(
            UserErrorKey::ShipCannotUseStargate,
            [
                ("shipTypeID", UserErrorArg::Int(ship_type_id)),
                ("whitelist", UserErrorArg::Str(whitelist.into())),
            ],
        )
    }

    /// Constructs a typed `CantDockWhileModuleActive` (`{moduleType}`).
    pub fn cant_dock_while_module_active(module_type_id: i64) -> Self {
        Self::user_with_args(
            UserErrorKey::CantDockWhileModuleActive,
            [("moduleType", UserErrorArg::Int(module_type_id))],
        )
    }

    /// Constructs a typed `CantJumpWhileModuleActive` (`{moduleType}`).
    pub fn cant_jump_while_module_active(module_type_id: i64) -> Self {
        Self::user_with_args(
            UserErrorKey::CantJumpWhileModuleActive,
            [("moduleType", UserErrorArg::Int(module_type_id))],
        )
    }

    /// Total wire mapping from [`ClientError`] to a V24 `machoNetPacket.ErrorResponse` payload
    /// tuple `(original_command, MACHONETERR_WRAPPEDEXCEPTION, (marshal::encode(exc),))`.
    pub fn to_macho_error_body(&self, original_command: i64) -> PyRep {
        match self {
            Self::User { key, args } | Self::Rejected { key, args, .. } => {
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
                            PyRep::String(s) => text_to_wire_pyrep(&s),
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
                vec![(PyRep::ascii_bytes("info"), text_to_wire_pyrep(info))],
            ),
            Self::CustomNotify(notify) => encode_wrapped_user_error(
                original_command,
                UserErrorKey::CustomNotify.as_str(),
                vec![(PyRep::ascii_bytes("notify"), text_to_wire_pyrep(notify))],
            ),
            Self::Unimplemented { service, method } => {
                let msg = format!("UnimplementedRPC: {service}.{method} is not yet implemented");
                encode_wrapped_runtime_error(original_command, &msg)
            }
            Self::Internal(msg) => encode_wrapped_runtime_error(original_command, msg),
            Self::SessionMismatch(msg) => encode_wrapped_user_error(
                original_command,
                UserErrorKey::UnMachoDestination.as_str(),
                vec![(PyRep::ascii_bytes("info"), text_to_wire_pyrep(msg))],
            ),
        }
    }
}

fn text_to_wire_pyrep(text: &str) -> PyRep {
    if text.is_ascii() {
        PyRep::ascii_bytes(text)
    } else {
        PyRep::String(text.to_owned())
    }
}

fn encode_wrapped_runtime_error(original_command: i64, message: &str) -> PyRep {
    let exc = PyRep::ObjectEx {
        is_type_2: false,
        header: Box::new(PyRep::Tuple(vec![
            PyRep::Token("exceptions.RuntimeError".into()),
            PyRep::Tuple(vec![text_to_wire_pyrep(message)]),
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

fn find_numeric_arg(args: &[(String, PyRep)], name: &str) -> Option<f64> {
    args.iter()
        .find(|(k, _)| k == name)
        .and_then(|(_, v)| v.as_f64().or_else(|| v.as_i64().map(|n| n as f64)))
}

fn format_capacity_message(args: &[(String, PyRep)]) -> Option<String> {
    if let (Some(vol), Some(avail)) = (
        find_numeric_arg(args, "volume"),
        find_numeric_arg(args, "available"),
    ) {
        return Some(format!("volume={vol}, available={avail}"));
    }
    if let (Some(free), Some(req)) = (
        find_numeric_arg(args, "free"),
        find_numeric_arg(args, "required"),
    ) {
        return Some(format!("free={free}, required={req}"));
    }
    if let (Some(cap), Some(vol)) = (
        find_numeric_arg(args, "capacity"),
        find_numeric_arg(args, "volume"),
    ) {
        return Some(format!("volume={vol}, capacity={cap}"));
    }
    if let (Some(item_vol), Some(avail)) = (
        find_numeric_arg(args, "itemVolume"),
        find_numeric_arg(args, "volumeAvailable"),
    ) {
        return Some(format!(
            "itemVolume={item_vol}, volumeAvailable={avail}"
        ));
    }
    if let (Some(max), Some(used)) = (
        find_numeric_arg(args, "maximum"),
        find_numeric_arg(args, "used"),
    ) {
        return Some(format!("used={used}, maximum={max}"));
    }
    None
}

impl BridgeRpcError {
    /// Constructs a [`BridgeRpcError`] carrying `client_error` directly by value so
    /// [`Self::to_client_error`] is lossless without parsing `message` or using global state.
    pub fn from_client_error(client_error: ClientError) -> Self {
        let (code, message) = match &client_error {
            ClientError::User { key, args } | ClientError::Rejected { key, args, .. } => {
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
                } else if key.is_capacity_error()
                    || matches!(
                        key,
                        UserErrorKey::NotEnoughCargoSpaceFor1Unit
                            | UserErrorKey::NotEnoughChargeSpace
                            | UserErrorKey::NoSpaceForThat
                            | UserErrorKey::NoSpaceForThatOverload
                            | UserErrorKey::NotEnoughCargoSpaceOverload
                            | UserErrorKey::NotEnoughDroneBaySpaceOverload
                            | UserErrorKey::NotEnoughSpecialBaySpaceOverload
                    )
                {
                    format_capacity_message(args).unwrap_or_else(|| code_str.clone())
                } else if matches!(key, UserErrorKey::NotEnoughEnergy | UserErrorKey::JumpNotEnoughCap) {
                    if let (Some(need), Some(got)) = (find_numeric_arg(args, "need"), find_numeric_arg(args, "got")) {
                        format!("capacitor need={need}, got={got}")
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
            ClientError::CustomNotify(notify) => ("CustomNotify".into(), notify.clone()),
            ClientError::Unimplemented { service, method } => (
                "UnimplementedRPC".into(),
                format!("{service}.{method} is not yet implemented"),
            ),
            ClientError::Internal(msg) => ("InternalError".into(), msg.clone()),
            ClientError::SessionMismatch(msg) => ("UnMachoDestination".into(), msg.clone()),
        };
        Self {
            code,
            message,
            client_error: Some(Box::new(client_error)),
        }
    }

    /// Constructs a [`BridgeRpcError`] backed by [`ClientError::User`] with no extra kwargs.
    pub fn from_user_key(key: UserErrorKey) -> Self {
        Self::from_client_error(ClientError::user(key))
    }

    /// Constructs a [`BridgeRpcError`] backed by [`ClientError::User`] with typed kwargs (`#1086`).
    pub fn user_error_with_args(
        key: UserErrorKey,
        args: impl IntoIterator<Item = impl IntoUserErrorArg>,
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
        Self {
            code: code.into(),
            message: message.into(),
            client_error: Some(Box::new(client_error)),
        }
    }

    /// Constructs a typed modal [`ClientError::CustomInfo`] RPC error.
    pub fn custom_info(info: impl Into<String>) -> Self {
        Self::from_client_error(ClientError::CustomInfo(info.into()))
    }

    /// Constructs a typed `InvalidQuantity` error backed by [`ClientError::CustomInfo`].
    pub fn invalid_quantity(message: impl Into<String>) -> Self {
        let msg = message.into();
        Self::with_client_error("InvalidQuantity", msg.clone(), ClientError::CustomInfo(msg))
    }

    /// Constructs a typed non-modal [`ClientError::CustomNotify`] RPC toast error.
    pub fn custom_notify(notify: impl Into<String>) -> Self {
        Self::from_client_error(ClientError::CustomNotify(notify.into()))
    }

    /// Returns the V24 client presentation discipline (`Modal` vs `Notify`) for this error.
    pub fn client_presentation(&self, service: &str, method: &str) -> UserErrorPresentation {
        self.to_client_error(service, method).presentation()
    }

    /// Constructs a typed hold/bay capacity error (`{volume, available}` rounded to 6 dp).
    pub fn hold_capacity_error(
        key: UserErrorKey,
        volume_needed_m3: f64,
        volume_available_m3: f64,
    ) -> Self {
        Self::from_client_error(ClientError::hold_capacity_error(
            key,
            volume_needed_m3,
            volume_available_m3,
        ))
    }

    /// Constructs a typed `NotEnoughChargeSpace` (`{capacity, volume}`).
    pub fn not_enough_charge_space(capacity_m3: f64, volume_needed_m3: f64) -> Self {
        Self::from_client_error(ClientError::not_enough_charge_space(
            capacity_m3,
            volume_needed_m3,
        ))
    }

    /// Constructs a typed `NoSpaceForThat` (`{itemTypeName, itemVolume, volumeAvailable}`).
    pub fn no_space_for_that(
        item_type_id: i64,
        item_volume_m3: f64,
        volume_available_m3: f64,
    ) -> Self {
        Self::from_client_error(ClientError::no_space_for_that(
            item_type_id,
            item_volume_m3,
            volume_available_m3,
        ))
    }

    /// Constructs a typed `NotEnoughCargoSpaceFor1Unit` (`{type, required, free}`).
    pub fn not_enough_cargo_space_for_unit(type_id: i64, required_m3: f64, free_m3: f64) -> Self {
        Self::from_client_error(ClientError::not_enough_cargo_space_for_unit(
            type_id,
            required_m3,
            free_m3,
        ))
    }

    /// Constructs a typed `DockingRequestDenied` error with a `[101, label]` reason kwarg.
    pub fn docking_request_denied(
        reason_label: &'static str,
        message: impl Into<String>,
    ) -> Self {
        Self::with_client_error(
            "DockingRequestDenied",
            message,
            ClientError::docking_request_denied(reason_label),
        )
    }

    /// Constructs a typed `ShipCannotUseStargate` (`{shipTypeID, whitelist}`).
    pub fn ship_cannot_use_stargate(
        ship_type_id: i64,
        whitelist: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self::with_client_error(
            "ShipCannotUseStargate",
            message,
            ClientError::ship_cannot_use_stargate(ship_type_id, whitelist),
        )
    }

    /// Constructs a typed `TargetTooFar` (`TargetOutOfRange` code) error for tactical range checks.
    pub fn target_out_of_range(
        target_name: impl Into<String>,
        module_type_id: i64,
        distance_m: f64,
        message: impl Into<String>,
    ) -> Self {
        Self::with_client_error(
            "TargetOutOfRange",
            message,
            ClientError::target_too_far(target_name, module_type_id, distance_m),
        )
    }

    /// Constructs a typed `TargetTooFar` error with `code = "TargetTooFar"`.
    pub fn target_too_far(
        target_name: impl Into<String>,
        module_type_id: i64,
        distance_m: f64,
    ) -> Self {
        Self::from_client_error(ClientError::target_too_far(
            target_name,
            module_type_id,
            distance_m,
        ))
    }

    /// Constructs a typed `NoCharges` (`NoChargesLoaded` code) error with `[4, launcher_type_id]`.
    pub fn no_charges_loaded(launcher_type_id: i64, message: impl Into<String>) -> Self {
        Self::with_client_error(
            "NoChargesLoaded",
            message,
            ClientError::no_charges(launcher_type_id),
        )
    }

    /// Constructs a typed `NoCharges` error with `code = "NoCharges"`.
    pub fn no_charges(launcher_type_id: i64) -> Self {
        Self::from_client_error(ClientError::no_charges(launcher_type_id))
    }

    /// Constructs a typed `CustomInfo` RPC error with an explicit typed `info` argument
    /// (for example `UserErrorArg::Label(UI_LABEL_GATE_CLOSED)` -> `(101, "UI/GateIcons/GateClosed")`).
    pub fn custom_info_args(
        code: impl Into<String>,
        message: impl Into<String>,
        info: UserErrorArg,
    ) -> Self {
        Self::with_client_error(
            code,
            message,
            ClientError::user_with_args(UserErrorKey::CustomInfo, [("info", info)]),
        )
    }

    /// Constructs the V24 closed-stargate error:
    /// `eveexceptions.UserError("CustomInfo", {"info": (101, "UI/GateIcons/GateClosed")})`.
    pub fn gate_closed(gate_id: i64) -> Self {
        Self::custom_info_args(
            "GateClosed",
            format!("Stargate {gate_id} is closed"),
            UserErrorArg::Label(UI_LABEL_GATE_CLOSED),
        )
    }

    /// Constructs the V24 stargate jump range error (`NotCloseEnoughToJump`, `dialogs.static` 1329).
    pub fn not_close_enough_to_jump(
        surface_dist_m: f64,
        center_dist_m: f64,
        max_center_dist_m: f64,
        perimeter_m: f64,
    ) -> Self {
        Self::with_client_error(
            "NotCloseEnoughToJump",
            format!(
                "Ship is {surface_dist_m:.1} m from stargate surface (must be within {perimeter_m:.0} m; center dist {center_dist_m:.1} m, max {max_center_dist_m:.1} m)"
            ),
            ClientError::user_with_args(
                UserErrorKey::NotCloseEnoughToJump,
                [("maxdist", UserErrorArg::Int(perimeter_m.round() as i64))],
            ),
        )
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
                    ("curCnt", UserErrorArg::Int(cur_cnt)),
                    ("maxCnt", UserErrorArg::Int(max_cnt)),
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

    /// Constructs a typed `WarpTooShort` (`WarpDistanceTooShort`) error.
    pub fn warp_distance_too_short(message: impl Into<String>) -> Self {
        Self::with_client_error(
            "WarpDistanceTooShort",
            message.into(),
            ClientError::user(UserErrorKey::WarpTooShort),
        )
    }

    /// Converts this [`BridgeRpcError`] into a typed [`ClientError`] without parsing or scraping
    /// `self.message`.
    pub fn to_client_error(&self, service: &str, method: &str) -> ClientError {
        let is_slash_rpc =
            matches!(service.trim(), "slash" | "publicQaToolsServer") && method == "SlashCmd";

        if let Some(ref typed) = self.client_error {
            let is_typed_slash = matches!(
                typed.as_ref(),
                ClientError::User {
                    key: UserErrorKey::SlashError,
                    ..
                } | ClientError::Rejected {
                    key: UserErrorKey::SlashError,
                    ..
                }
            );
            if !is_slash_rpc || is_typed_slash {
                return (**typed).clone();
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
            "InvalidBoundObject"
            | "BoundObjectNotOwned"
            | "BoundObjectOwnershipViolation"
            | "BindAuthorizationDenied"
            | "UnMachoDestination" => ClientError::SessionMismatch(err_msg),
            "SlashError" => ClientError::user_with_args(
                UserErrorKey::SlashError,
                [("reason", UserErrorArg::Str(self.message.clone()))],
            ),
            "UnknownSlashCommand" | "InvalidSlashArgument" | "InvalidSlashArguments" => {
                ClientError::user_with_args(
                    UserErrorKey::SlashError,
                    [("reason", UserErrorArg::Str(err_msg))],
                )
            }
            _ if is_slash_rpc => ClientError::user_with_args(
                UserErrorKey::SlashError,
                [("reason", UserErrorArg::Str(err_msg))],
            ),
            "MaxOpenOrdersReached" => ClientError::User {
                key: UserErrorKey::MarketExceededOrderCount,
                args: UserErrorKey::MarketExceededOrderCount.default_args(),
            },
            "OutsideDockingPerimeter" | "DockingApproach" => {
                ClientError::user(UserErrorKey::DockingApproach)
            }
            "WarpDistanceTooShort" | "WarpTooShort" => {
                ClientError::user(UserErrorKey::WarpTooShort)
            }
            "InvalidWarpDestination" | "WarpDestinationGone" => {
                ClientError::user(UserErrorKey::WarpDestinationGone)
            }
            "TargetOutOfRange" => ClientError::User {
                key: UserErrorKey::TargetTooFar,
                args: UserErrorKey::TargetTooFar.default_args(),
            },
            "NoChargesLoaded" | "NoAmmoLoaded" => ClientError::User {
                key: UserErrorKey::NoCharges,
                args: UserErrorKey::NoCharges.default_args(),
            },
            "QuantumCoreRequired" => ClientError::User {
                key: UserErrorKey::CannotUseFittingInStructureWithoutDeed,
                args: UserErrorKey::CannotUseFittingInStructureWithoutDeed.default_args(),
            },
            "ShipAlreadyInWarp" | "ShipInWarp" => ClientError::user(UserErrorKey::ShipInWarp),
            "NotEnoughCapacitor" => ClientError::User {
                key: UserErrorKey::NotEnoughEnergy,
                args: UserErrorKey::NotEnoughEnergy.default_args(),
            },
            "ShipNotInSpace" => ClientError::user(UserErrorKey::CantShipNotPresent),
            "FittingValidationFailed" => ClientError::user_with_args(
                UserErrorKey::ModuleFitFailed,
                [
                    ("moduleName", UserErrorArg::Str("Module".to_owned())),
                    ("reason", UserErrorArg::Str(self.message.clone())),
                ],
            ),
            "CannotFitStackedModule" => {
                ClientError::user(UserErrorKey::CannotPerformOnMultipleItems)
            }
            "InvalidTarget" => ClientError::User {
                key: UserErrorKey::InvalidTargetForModule,
                args: UserErrorKey::InvalidTargetForModule.default_args(),
            },
            "InventoryNotAccessible" => ClientError::user(UserErrorKey::AccessDenied),
            "MustBeDockedAtStartStation" | "MustBeDocked" => {
                ClientError::user(UserErrorKey::MustBeDocked)
            }
            "StructureNotDockable" => ClientError::user(UserErrorKey::NotDockable),
            "StructureServiceAccessDenied" => ClientError::User {
                key: UserErrorKey::StructureServiceUnavailable,
                args: UserErrorKey::StructureServiceUnavailable.default_args(),
            },
            "WeaponsTimerActive" => ClientError::user(UserErrorKey::CantDockAfterAggression),
            "CannotJumpWithWeaponsTimer" => {
                ClientError::user(UserErrorKey::CantJumpAfterAggression)
            }
            "InvalidContractState" => ClientError::user(UserErrorKey::ConContractNotOutstanding),
            "ModuleNotFound" => ClientError::user(UserErrorKey::ItemNotFound),
            "OwnershipOrLocationViolation" => {
                ClientError::user(UserErrorKey::CantDoThatWithSomeoneElsesStuff)
            }
            "ContractOwnershipViolation" => ClientError::user(UserErrorKey::AccessDenied),
            "InsufficientRole"
            | "GM_InsufficientRole"
            | "UserError: GM_InsufficientRole"
            | "InsufficientRoles"
            | "GMRoleRequired"
            | "GmRoleRequired" => ClientError::user(UserErrorKey::InsufficientRoles),
            "CustomInfo" => ClientError::CustomInfo(self.message.clone()),
            "CustomNotify" => ClientError::CustomNotify(self.message.clone()),
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


#[cfg(test)]
#[path = "client_error_tests.rs"]
mod tests;
