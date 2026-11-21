//! Strongly typed complex payloads for MachoNet notifications (`GH #1562`).
//!
//! Kept separate from `notify_registry.rs` so notification payload contracts remain modular,
//! impossible to accidentally satisfy with raw primitives (`PyRep::Integer` / `PyRep::String`),
//! and aligned with V24 client expectations.

#![forbid(unsafe_code)]

use crate::router::notify_registry::ToNotifyArg;
use eve_proto::PyRep;

/// Wire entry `(typeID, flagID, quantity)` inside a fitting's `fitData` list.
pub type FittingItemTuple = (i64, i64, i64);

/// Typed `utillib.KeyVal` payload for `fittingSvc.OnFittingsUpdated(fitting)`.
/// V24 client checks `fitting is None`, compares `fitting.shipTypeID`, and sorts/iterates `fitting.fitData`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdatedFittingKeyVal {
    pub fitting_id: i64,
    pub owner_id: i64,
    pub ship_type_id: i64,
    pub name: String,
    pub description: String,
    pub fit_data: Vec<FittingItemTuple>,
}

impl UpdatedFittingKeyVal {
    /// Builds an [`UpdatedFittingKeyVal`] directly from a stored [`eve_db::SavedFittingRecord`].
    pub fn from_saved_fitting(fit: &eve_db::SavedFittingRecord) -> Self {
        let fit_data = fit
            .items
            .iter()
            .map(|&(tid, flag, qty)| (i64::from(tid), i64::from(flag as u16), i64::from(qty)))
            .collect();
        Self {
            fitting_id: fit.fitting_id,
            owner_id: fit.owner_id,
            ship_type_id: i64::from(fit.ship_type_id),
            name: fit.name.clone(),
            description: fit.description.clone(),
            fit_data,
        }
    }
}

impl ToNotifyArg for UpdatedFittingKeyVal {
    fn into_notify_arg(self) -> PyRep {
        let fit_data = self
            .fit_data
            .into_iter()
            .map(|(tid, flag_id, qty)| {
                PyRep::Tuple(vec![
                    PyRep::Integer(tid),
                    PyRep::Integer(flag_id),
                    PyRep::Integer(qty),
                ])
            })
            .collect();
        PyRep::keyval([
            ("fittingID", PyRep::Integer(self.fitting_id)),
            ("ownerID", PyRep::Integer(self.owner_id)),
            ("shipTypeID", PyRep::Integer(self.ship_type_id)),
            ("name", PyRep::String(self.name)),
            ("description", PyRep::String(self.description)),
            ("fitData", PyRep::List(fit_data)),
        ])
    }
}

/// Typed payload for `OnFittingsUpdated(fitting)`: either a full fitting `KeyVal` on save/update
/// or `Cleared` (`None`) on delete so `fittingSvc.OnFittingsUpdated` hits `fitting is None` safely
/// while `HullAndHardwareTypeBrowser.OnFittingsUpdated` still reloads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdatedFittingPayload {
    Fitting(UpdatedFittingKeyVal),
    Cleared,
}

impl ToNotifyArg for UpdatedFittingPayload {
    fn into_notify_arg(self) -> PyRep {
        match self {
            Self::Fitting(kv) => kv.into_notify_arg(),
            Self::Cleared => PyRep::None,
        }
    }
}

/// Typed 4-tuple `(offeringCharID, targetCharID, shipID, offered)` for
/// `CloneJump.OnShipJumpCloneInstallationOffered(self, args)` and `OnShipJumpCloneInstallationDone(self, args)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShipJumpCloneInstallationArgs {
    pub offering_char_id: i64,
    pub target_char_id: i64,
    pub ship_id: i64,
    pub offered: bool,
}

impl ToNotifyArg for ShipJumpCloneInstallationArgs {
    fn into_notify_arg(self) -> PyRep {
        PyRep::Tuple(vec![
            PyRep::Integer(self.offering_char_id),
            PyRep::Integer(self.target_char_id),
            PyRep::Integer(self.ship_id),
            PyRep::Bool(self.offered),
        ])
    }
}

/// Standard slim-item fields required when emitting `OnSlimItemChange` for a renamed ship in space
/// so V24 client subscribers (`InventorySvc`, `BracketMgr`, `TacticalSvc`, `WatchListPanel`) never raise `AttributeError`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpaceShipSlimFields {
    pub item_id: i64,
    pub type_id: i64,
    pub group_id: i64,
    pub category_id: i64,
    pub owner_id: i64,
    pub char_id: Option<i64>,
    pub corp_id: Option<i64>,
    pub alliance_id: Option<i64>,
    pub name: String,
}

/// Typed `utillib.KeyVal` payload for `OnSlimItemChange.new_slim`.
/// Private fields prevent callers from passing raw strings, integers, or bare dicts.
#[derive(Debug, Clone, PartialEq)]
pub struct SlimItemKeyVal {
    fields: Vec<(String, PyRep)>,
}

impl SlimItemKeyVal {
    /// Constructs a [`SlimItemKeyVal`] strictly from named key-value pairs.
    pub fn from_pairs<K: Into<String>>(pairs: impl IntoIterator<Item = (K, PyRep)>) -> Self {
        Self {
            fields: pairs.into_iter().map(|(k, v)| (k.into(), v)).collect(),
        }
    }

    /// Constructs a full V24-compliant slim-item `KeyVal` for a ship in space (e.g. `invbroker.SetLabel`).
    pub fn for_space_ship(f: SpaceShipSlimFields) -> Self {
        let opt_id = |v: Option<i64>| v.filter(|&id| id > 0).map_or(PyRep::None, PyRep::Integer);
        Self::from_pairs([
            ("itemID", PyRep::Integer(f.item_id)),
            ("typeID", PyRep::Integer(f.type_id)),
            ("ownerID", PyRep::Integer(f.owner_id)),
            ("corpID", opt_id(f.corp_id)),
            ("allianceID", opt_id(f.alliance_id)),
            ("charID", opt_id(f.char_id)),
            ("categoryID", PyRep::Integer(f.category_id)),
            ("groupID", PyRep::Integer(f.group_id)),
            ("name", PyRep::String(f.name)),
            ("controllerID", PyRep::None),
            ("incapacitated", PyRep::Bool(false)),
            ("posState", PyRep::Integer(0)),
            ("posTimestamp", PyRep::Integer(0)),
            ("isEmpty", PyRep::Bool(false)),
            ("lootRights", PyRep::None),
            ("hackingSecurityState", PyRep::None),
            ("orbitalHackerID", PyRep::None),
            ("orbitalHackerProgress", PyRep::Real(0.0)),
            ("orbitalState", PyRep::None),
            ("orbitalTimestamp", PyRep::Integer(0)),
        ])
    }
}

impl ToNotifyArg for SlimItemKeyVal {
    fn into_notify_arg(self) -> PyRep {
        PyRep::keyval(self.fields)
    }
}

/// FSD dialog message keys (`res:/staticdata/dialogs.static`) accepted by `FleetSvc.OnFleetInvite`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FleetInviteMsgName {
    AskJoinFleet,
    AskJoinFleetThroughAd,
}

impl ToNotifyArg for FleetInviteMsgName {
    fn into_notify_arg(self) -> PyRep {
        let key = match self {
            Self::AskJoinFleet => "AskJoinFleet",
            Self::AskJoinFleetThroughAd => "AskJoinFleetThroughAd",
        };
        PyRep::String(key.into())
    }
}

/// Typed `msgDict` (`dict`, NOT `KeyVal`) for `FleetSvc.OnFleetInvite`.
/// V24 reads `msgDict.get('autoAccept', False)` and writes `msgDict['name'] = character_link(inviteID)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FleetInviteMsgDict {
    pub auto_accept: bool,
}

impl ToNotifyArg for FleetInviteMsgDict {
    fn into_notify_arg(self) -> PyRep {
        PyRep::Dict(vec![(
            PyRep::String("autoAccept".into()),
            PyRep::Bool(self.auto_accept),
        )])
    }
}
