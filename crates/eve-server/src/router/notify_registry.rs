//! Typed notification registry with macro-generated payloads and compile-time arity checks.
//!
//! Every notification emitted through [`ServerContext::emit_typed`] (and DB-backed helpers)
//! is registered here as the single source of truth for its event name, default `id_type`,
//! positional argument types, and sample payloads verified against extracted client expectations.

#![forbid(unsafe_code)]

use crate::router::cache_notifications::{
    next_notification_seq_id, push_pending_notification_capped, PendingMachoNotification,
};
use crate::router::{NotificationTarget, ServerContext};
use eve_db::lock_rank::AnyRwLock;
use eve_proto::PyRep;
use std::collections::HashMap;
use tokio::sync::broadcast;

/// Conversion trait for strongly typed notification payload arguments.
pub trait ToNotifyArg {
    fn into_notify_arg(self) -> PyRep;
}

impl ToNotifyArg for PyRep {
    fn into_notify_arg(self) -> PyRep {
        self
    }
}

impl ToNotifyArg for i64 {
    fn into_notify_arg(self) -> PyRep {
        PyRep::Integer(self)
    }
}

impl ToNotifyArg for i32 {
    fn into_notify_arg(self) -> PyRep {
        PyRep::Integer(i64::from(self))
    }
}

impl ToNotifyArg for u32 {
    fn into_notify_arg(self) -> PyRep {
        PyRep::Integer(i64::from(self))
    }
}

impl ToNotifyArg for u64 {
    fn into_notify_arg(self) -> PyRep {
        PyRep::Integer(self as i64)
    }
}

impl ToNotifyArg for f64 {
    fn into_notify_arg(self) -> PyRep {
        PyRep::Real(self)
    }
}

impl ToNotifyArg for bool {
    fn into_notify_arg(self) -> PyRep {
        PyRep::Bool(self)
    }
}

impl ToNotifyArg for String {
    fn into_notify_arg(self) -> PyRep {
        PyRep::String(self)
    }
}

impl ToNotifyArg for &str {
    fn into_notify_arg(self) -> PyRep {
        PyRep::String(self.to_string())
    }
}

impl<T: ToNotifyArg> ToNotifyArg for Option<T> {
    fn into_notify_arg(self) -> PyRep {
        match self {
            Some(v) => v.into_notify_arg(),
            None => PyRep::None,
        }
    }
}

impl<T: ToNotifyArg> ToNotifyArg for Vec<T> {
    fn into_notify_arg(self) -> PyRep {
        PyRep::List(self.into_iter().map(ToNotifyArg::into_notify_arg).collect())
    }
}

impl<A: ToNotifyArg, B: ToNotifyArg> ToNotifyArg for (A, B) {
    fn into_notify_arg(self) -> PyRep {
        PyRep::Tuple(vec![self.0.into_notify_arg(), self.1.into_notify_arg()])
    }
}

/// Implemented by every registered notification payload struct.
pub trait TypedNotification {
    const EVENT: &'static str;
    const DEFAULT_ID_TYPE: &'static str;
    const MIN_ARITY: usize;
    const MAX_ARITY: usize;

    fn into_args(self) -> Vec<PyRep>;
}

/// Static descriptor entry exposed for contract verification across all registered events.
#[derive(Debug, Clone, Copy)]
pub struct NotificationRegistryEntry {
    pub event: &'static str,
    pub default_id_type: &'static str,
    pub min_arity: usize,
    pub max_arity: usize,
    pub sample_payloads: fn() -> Vec<Vec<PyRep>>,
}

impl ServerContext {
    /// Emits a typed notification using its registered `EVENT` name and `DEFAULT_ID_TYPE`.
    pub fn emit_typed<N: TypedNotification>(
        &self,
        target: NotificationTarget,
        notif: N,
    ) -> Vec<PendingMachoNotification> {
        self.notify(target, N::EVENT, N::DEFAULT_ID_TYPE, notif.into_args())
    }

    /// Emits a typed notification with an explicit MachoNet `id_type` override (for events whose
    /// scope varies by dispatch target, e.g. `"allianceid"` vs `"corpid"`).
    pub fn emit_typed_with_id_type<N: TypedNotification>(
        &self,
        target: NotificationTarget,
        id_type: &str,
        notif: N,
    ) -> Vec<PendingMachoNotification> {
        self.notify(target, N::EVENT, id_type, notif.into_args())
    }
}

/// Emits a typed character notification directly to `pending_notifications` and `notify_tx`
/// without holding a full [`ServerContext`] (used by `SystemActor` / dungeon hooks, retaining
/// their 256-entry FIFO queue bound).
pub fn emit_typed_char_core<N: TypedNotification>(
    db: &eve_db::WorldRepository,
    pending_notifications: Option<&dyn AnyRwLock<HashMap<i64, Vec<PendingMachoNotification>>>>,
    notify_tx: Option<&broadcast::Sender<(i64, PendingMachoNotification)>>,
    char_id: i64,
    user_id_opt: Option<u64>,
    notif: N,
) {
    if char_id <= 0 {
        return;
    }
    let now_ft = db.clock().now_blueos_filetime_ticks();
    build_and_push_typed_notification(
        pending_notifications,
        notify_tx,
        char_id,
        user_id_opt,
        N::DEFAULT_ID_TYPE,
        now_ft,
        notif,
        Some(256),
    );
}

/// Looks up `char_id`'s `user_id` in `db` and emits a typed character notification when
/// `pending_notifications` is present.
pub fn emit_typed_char_with_db<N: TypedNotification>(
    db: &eve_db::WorldRepository,
    pending_notifications: Option<&dyn AnyRwLock<HashMap<i64, Vec<PendingMachoNotification>>>>,
    notify_tx: Option<&broadcast::Sender<(i64, PendingMachoNotification)>>,
    char_id: i64,
    notif: N,
) {
    if char_id <= 0 || pending_notifications.is_none() {
        return;
    }
    let uid_opt = db.with_snapshot(|s| s.characters.get(&char_id).map(|c| c.user_id));
    emit_typed_char_core(db, pending_notifications, notify_tx, char_id, uid_opt, notif);
}

/// Builds and pushes a typed notification to a single recipient's pending queue + live broadcast
/// using the standard router [`push_pending_notification_capped`] queue bound.
pub fn emit_typed_target_to_char_with_id_type<N: TypedNotification>(
    pending_notifications: Option<&dyn AnyRwLock<HashMap<i64, Vec<PendingMachoNotification>>>>,
    notify_tx: Option<&broadcast::Sender<(i64, PendingMachoNotification)>>,
    char_id: i64,
    user_id_opt: Option<u64>,
    id_type: &str,
    created_filetime: i64,
    notif: N,
) {
    build_and_push_typed_notification(
        pending_notifications,
        notify_tx,
        char_id,
        user_id_opt,
        id_type,
        created_filetime,
        notif,
        None,
    );
}

#[allow(clippy::too_many_arguments)]
fn build_and_push_typed_notification<N: TypedNotification>(
    pending_notifications: Option<&dyn AnyRwLock<HashMap<i64, Vec<PendingMachoNotification>>>>,
    notify_tx: Option<&broadcast::Sender<(i64, PendingMachoNotification)>>,
    char_id: i64,
    user_id_opt: Option<u64>,
    id_type: &str,
    created_filetime: i64,
    notif: N,
    explicit_queue_cap: Option<usize>,
) {
    if char_id <= 0 {
        return;
    }
    let args = notif.into_args();
    let (packet, marshaled_packet) = ServerContext::build_macho_notification_packet(
        char_id,
        user_id_opt,
        N::EVENT,
        id_type,
        args.clone(),
    );
    let pending = PendingMachoNotification {
        seq_id: next_notification_seq_id(),
        recipient_char_id: char_id,
        event: N::EVENT.to_string(),
        id_type: id_type.to_string(),
        args,
        packet,
        marshaled_packet,
        created_filetime,
    };
    if let Some(pend) = pending_notifications {
        let mut queues = pend.write().unwrap_or_else(|e| e.into_inner());
        let queue = queues.entry(char_id).or_default();
        if let Some(cap) = explicit_queue_cap {
            while queue.len() >= cap {
                queue.remove(0);
            }
            queue.push(pending.clone());
        } else {
            push_pending_notification_capped(queue, pending.clone());
        }
    }
    if let Some(tx) = notify_tx {
        let _ = tx.send((char_id, pending));
    }
}

macro_rules! count_idents {
    () => { 0usize };
    ($head:ident $(, $tail:ident)*) => { 1usize + count_idents!($($tail),*) };
}

macro_rules! define_notification_item {
    // 0-argument notification (unit struct)
    ($name:ident => $event:literal ($id_type:literal) {
        samples: $samples:expr $(,)?
    }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub struct $name;

        impl TypedNotification for $name {
            const EVENT: &'static str = $event;
            const DEFAULT_ID_TYPE: &'static str = $id_type;
            const MIN_ARITY: usize = 0;
            const MAX_ARITY: usize = 0;

            fn into_args(self) -> Vec<PyRep> {
                Vec::new()
            }
        }
    };

    // Fixed-arity notification with required positional fields
    ($name:ident => $event:literal ($id_type:literal) {
        fields: { $($field:ident : $ty:ty),+ $(,)? },
        samples: $samples:expr $(,)?
    }) => {
        #[derive(Debug, Clone, PartialEq)]
        pub struct $name {
            $(pub $field: $ty),+
        }

        impl TypedNotification for $name {
            const EVENT: &'static str = $event;
            const DEFAULT_ID_TYPE: &'static str = $id_type;
            const MIN_ARITY: usize = count_idents!($($field),+);
            const MAX_ARITY: usize = Self::MIN_ARITY;

            fn into_args(self) -> Vec<PyRep> {
                vec![$(ToNotifyArg::into_notify_arg(self.$field)),+]
            }
        }
    };

    // Variable trailing optional arguments (e.g. 2..=3 timer updates)
    ($name:ident => $event:literal ($id_type:literal) {
        fields: { $($field:ident : $ty:ty),+ $(,)? },
        optional: { $($opt_field:ident : Option<$opt_ty:ty>),+ $(,)? },
        samples: $samples:expr $(,)?
    }) => {
        #[derive(Debug, Clone, PartialEq)]
        pub struct $name {
            $(pub $field: $ty,)+
            $(pub $opt_field: Option<$opt_ty>),+
        }

        impl TypedNotification for $name {
            const EVENT: &'static str = $event;
            const DEFAULT_ID_TYPE: &'static str = $id_type;
            const MIN_ARITY: usize = count_idents!($($field),+);
            const MAX_ARITY: usize = Self::MIN_ARITY + count_idents!($($opt_field),+);

            fn into_args(self) -> Vec<PyRep> {
                let mut args = vec![$(ToNotifyArg::into_notify_arg(self.$field)),+];
                $(
                    if let Some(opt_val) = self.$opt_field {
                        args.push(ToNotifyArg::into_notify_arg(opt_val));
                    }
                )+
                args
            }
        }
    };
}

macro_rules! define_notifications {
    ($(
        $name:ident => $event:literal ($id_type:literal) $body:tt
    ),* $(,)?) => {
        $(
            define_notification_item!($name => $event ($id_type) $body);
        )*

        /// Static table of all typed notifications registered in the central registry.
        pub fn all_registered_notifications() -> &'static [NotificationRegistryEntry] {
            &[
                $(
                    define_notifications!(@entry $name => $event ($id_type) $body),
                )*
            ]
        }
    };

    (@entry $name:ident => $event:literal ($id_type:literal) {
        samples: $samples:expr $(,)?
    }) => {
        NotificationRegistryEntry {
            event: <$name as TypedNotification>::EVENT,
            default_id_type: <$name as TypedNotification>::DEFAULT_ID_TYPE,
            min_arity: <$name as TypedNotification>::MIN_ARITY,
            max_arity: <$name as TypedNotification>::MAX_ARITY,
            sample_payloads: $samples,
        }
    };
    (@entry $name:ident => $event:literal ($id_type:literal) {
        fields: { $($field:ident : $ty:ty),+ $(,)? },
        samples: $samples:expr $(,)?
    }) => {
        NotificationRegistryEntry {
            event: <$name as TypedNotification>::EVENT,
            default_id_type: <$name as TypedNotification>::DEFAULT_ID_TYPE,
            min_arity: <$name as TypedNotification>::MIN_ARITY,
            max_arity: <$name as TypedNotification>::MAX_ARITY,
            sample_payloads: $samples,
        }
    };
    (@entry $name:ident => $event:literal ($id_type:literal) {
        fields: { $($field:ident : $ty:ty),+ $(,)? },
        optional: { $($opt_field:ident : Option<$opt_ty:ty>),+ $(,)? },
        samples: $samples:expr $(,)?
    }) => {
        NotificationRegistryEntry {
            event: <$name as TypedNotification>::EVENT,
            default_id_type: <$name as TypedNotification>::DEFAULT_ID_TYPE,
            min_arity: <$name as TypedNotification>::MIN_ARITY,
            max_arity: <$name as TypedNotification>::MAX_ARITY,
            sample_payloads: $samples,
        }
    };
}

fn sample_keyval_item() -> PyRep {
    PyRep::keyval([
        ("itemID", PyRep::Integer(100_000_001)),
        ("typeID", PyRep::Integer(34)),
        ("ownerID", PyRep::Integer(90_000_001)),
        ("locationID", PyRep::Integer(60_003_760)),
        ("flagID", PyRep::Integer(4)),
        ("quantity", PyRep::Integer(10)),
        ("stacksize", PyRep::Integer(10)),
        ("groupID", PyRep::Integer(18)),
        ("categoryID", PyRep::Integer(4)),
        ("singleton", PyRep::Bool(false)),
    ])
}

define_notifications! {
    DoSimClockRebase => "DoSimClockRebase" ("solarsystemid2") {
        fields: { times: PyRep },
        samples: || vec![vec![PyRep::Tuple(vec![
            PyRep::Integer(1_335_000_000_000_000_00),
            PyRep::Integer(1_335_000_001_000_000_00),
        ])]],
    },
    OnAccountChange => "OnAccountChange" ("charid") {
        fields: { account_key: String, owner_id: i64, balance: f64 },
        samples: || vec![vec![
            PyRep::String("cash".into()),
            PyRep::Integer(90_000_001),
            PyRep::Real(1_000_000.0),
        ]],
    },
    OnAchievementsReset => "OnAchievementsReset" ("charid") {
        samples: || vec![vec![]],
    },
    OnAirNpeStateChanged => "OnAirNpeStateChanged" ("charid") {
        fields: { character_id: i64, new_state_id: i64 },
        samples: || vec![vec![PyRep::Integer(90_000_001), PyRep::Integer(3)]],
    },
    OnAllianceApplicationChanged => "OnAllianceApplicationChanged" ("allianceid") {
        fields: { alliance_id: i64, corp_id: i64, change: PyRep },
        samples: || vec![vec![
            PyRep::Integer(99_000_001),
            PyRep::Integer(98_000_001),
            PyRep::Dict(vec![(
                PyRep::String("state".into()),
                PyRep::Tuple(vec![PyRep::None, PyRep::Integer(1)]),
            )]),
        ]],
    },
    OnAllianceChanged => "OnAllianceChanged" ("allianceid") {
        fields: { alliance_id: i64, change: PyRep },
        samples: || vec![vec![
            PyRep::Integer(99_000_001),
            PyRep::Dict(vec![(
                PyRep::String("executorCorpID".into()),
                PyRep::Tuple(vec![PyRep::None, PyRep::Integer(98_000_001)]),
            )]),
        ]],
    },
    OnAllianceLogoReady => "OnAllianceLogoReady" ("allianceid") {
        fields: { alliance_id: i64, size: i64 },
        samples: || vec![vec![PyRep::Integer(99_000_001), PyRep::Integer(128)]],
    },
    OnAllianceMemberChanged => "OnAllianceMemberChanged" ("allianceid") {
        fields: { alliance_id: i64, corp_id: i64, change: PyRep },
        samples: || vec![vec![
            PyRep::Integer(99_000_001),
            PyRep::Integer(98_000_001),
            PyRep::Dict(vec![(
                PyRep::String("corporationID".into()),
                PyRep::Tuple(vec![PyRep::None, PyRep::Integer(98_000_001)]),
            )]),
        ]],
    },
    OnAllianceRelationshipChanged => "OnAllianceRelationshipChanged" ("allianceid") {
        fields: { alliance_id: i64, to_id: i64, change: PyRep },
        samples: || vec![vec![
            PyRep::Integer(99_000_001),
            PyRep::Integer(99_000_002),
            PyRep::Dict(vec![(
                PyRep::String("relationship".into()),
                PyRep::Tuple(vec![PyRep::None, PyRep::Integer(5)]),
            )]),
        ]],
    },
    OnAllowWarUpdated => "OnAllowWarUpdated" ("corpid") {
        fields: { corp_id: i64, allow_war: bool },
        samples: || vec![vec![PyRep::Integer(98_000_001), PyRep::Bool(true)]],
    },
    OnAlphaInjectionAvailable => "OnAlphaInjectionAvailable" ("charid") {
        samples: || vec![vec![]],
    },
    OnAmmoInBankChanged => "OnAmmoInBankChanged" ("charid") {
        fields: { master_id: i64 },
        samples: || vec![vec![PyRep::Integer(100_000_050)]],
    },
    OnAnalysisKreditsChange => "OnAnalysisKreditsChange" ("charid") {
        samples: || vec![vec![]],
    },
    OnBeanPrime => "OnBeanPrime" ("charid") {
        fields: { beans: PyRep },
        samples: || vec![vec![PyRep::Dict(vec![(
            PyRep::Integer(12_345),
            PyRep::keyval([
                ("errorID", PyRep::Integer(12_345)),
                ("errorKeyHash", PyRep::String("test_hash".into())),
                ("logMode", PyRep::Integer(1)),
            ]),
        )])]],
    },
    OnCCPNotification => "OnCCPNotification" ("charid") {
        fields: {
            title: String,
            subtitle: String,
            text: String,
            notification_type_id: i64,
            language: Option<String>,
            reference_id: i64,
        },
        samples: || vec![vec![
            PyRep::String("Title".into()),
            PyRep::String("Subtitle".into()),
            PyRep::String("Body".into()),
            PyRep::Integer(1),
            PyRep::None,
            PyRep::Integer(0),
        ]],
    },
    OnCampaignEnrollmentChanged => "OnCampaignEnrollmentChanged" ("charid") {
        samples: || vec![vec![]],
    },
    OnCapitalSystemChanged => "OnCapitalSystemChanged" ("allianceid") {
        fields: { change: PyRep },
        samples: || vec![vec![PyRep::Tuple(vec![
            PyRep::Integer(99_000_001),
            PyRep::None,
            PyRep::Integer(30_000_142),
        ])]],
    },
    OnCargoScanComplete => "OnCargoScanComplete" ("charid") {
        fields: { ship_id: i64, cargo_list: Vec<(i32, i64)> },
        samples: || vec![vec![
            PyRep::Integer(100_000_010),
            PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(34), PyRep::Integer(10)])]),
        ]],
    },
    OnCfgDataChanged => "OnCfgDataChanged" ("solarsystemid") {
        fields: { what: String, data: PyRep },
        samples: || vec![vec![
            PyRep::String("evelocations".into()),
            PyRep::List(vec![PyRep::Integer(60_003_760), PyRep::String("Station".into())]),
        ]],
    },
    OnChallengeCompleted => "OnChallengeCompleted" ("charid") {
        fields: { challenge_id: i64 },
        samples: || vec![vec![PyRep::Integer(42)]],
    },
    OnChallengeExpired => "OnChallengeExpired" ("charid") {
        fields: { challenge_id: i64, new_challenge: PyRep },
        samples: || vec![vec![PyRep::Integer(42), PyRep::keyval([("challengeID", PyRep::Integer(43))])]],
    },
    OnChallengeProgressUpdate => "OnChallengeProgressUpdate" ("charid") {
        fields: { challenge_id: i64, new_progress: i64 },
        samples: || vec![vec![PyRep::Integer(42), PyRep::Integer(5)]],
    },
    OnChallengeRewardsGranted => "OnChallengeRewardsGranted" ("charid") {
        fields: { challenge_id: i64 },
        samples: || vec![vec![PyRep::Integer(42)]],
    },
    OnChangeFleetBoss => "OnChangeFleetBoss" ("fleetid") {
        fields: { char_id: i64, respond_to_node_id: i64 },
        samples: || vec![vec![PyRep::Integer(90_000_001), PyRep::Integer(26_000)]],
    },
    OnChannelLeft => "OnChannelLeft" ("channelid") {
        fields: { channel: PyRep },
        samples: || vec![vec![PyRep::Tuple(vec![PyRep::String("fleet".into()), PyRep::Integer(1)])]],
    },
    OnCharNoLongerInStation => "OnCharNoLongerInStation" ("charid") {
        fields: { rec: PyRep },
        samples: || vec![vec![PyRep::Tuple(vec![
            PyRep::Integer(90_000_001),
            PyRep::Integer(98_000_001),
            PyRep::None,
            PyRep::Integer(0),
        ])]],
    },
    OnCharNowInStation => "OnCharNowInStation" ("charid") {
        fields: { rec: PyRep },
        samples: || vec![vec![PyRep::Tuple(vec![
            PyRep::Integer(90_000_001),
            PyRep::Integer(98_000_001),
            PyRep::None,
            PyRep::Integer(0),
        ])]],
    },
    OnCharacterPermanentlyBanned => "OnCharacterPermanentlyBanned" ("charid") {
        samples: || vec![vec![]],
    },
    OnCharacterPokedByGM => "OnCharacterPokedByGM" ("charid") {
        fields: { gm_char_id: i64, channel_name: String },
        samples: || vec![vec![PyRep::Integer(90_000_002), PyRep::String("Help".into())]],
    },
    OnCharacterTemporarilyBanned => "OnCharacterTemporarilyBanned" ("charid") {
        fields: { ban_expiry: i64 },
        samples: || vec![vec![PyRep::Integer(1_335_000_000_000_000_00)]],
    },
    OnCharacterUnbanned => "OnCharacterUnbanned" ("charid") {
        samples: || vec![vec![]],
    },
    OnChargeBeingLoadedToModule => "OnChargeBeingLoadedToModule" ("charid") {
        fields: { item_ids: Vec<i64>, charge_type_id: i64, time_ms: f64 },
        samples: || vec![vec![
            PyRep::List(vec![PyRep::Integer(100_000_050)]),
            PyRep::Integer(219),
            PyRep::Real(5000.0),
        ]],
    },
    OnChargeLoadedToModule => "OnChargeLoadedToModule" ("charid") {
        samples: || vec![vec![]],
    },
    OnClearFetchedData => "OnClearFetchedData" ("clientID") {
        samples: || vec![vec![]],
    },
    OnClientEventMarketItemReceived => "OnClientEvent_MarketItemReceived" ("charid") {
        fields: { item_id: i64, type_id: i64, flag_id: i64, location_id: i64 },
        samples: || vec![vec![
            PyRep::Integer(100_000_001),
            PyRep::Integer(34),
            PyRep::Integer(4),
            PyRep::Integer(60_003_760),
        ]],
    },
    OnCloakModuleForceDeactivated => "OnCloakModuleForceDeactivated" ("charid") {
        fields: { notify_msg: String, notify_args: PyRep },
        samples: || vec![vec![
            PyRep::String("CantCloakCloseToEntity".into()),
            PyRep::Dict(vec![(PyRep::String("distance".into()), PyRep::Real(1500.0))]),
        ]],
    },
    OnClusterShutdownCancelled => "OnClusterShutdownCancelled" ("clientID") {
        fields: { explanation_label: String },
        samples: || vec![vec![PyRep::String("UI/System/ShutdownCancelled".into())]],
    },
    OnClusterShutdownInitiated => "OnClusterShutdownInitiated" ("clientID") {
        fields: { explanation_label: String, when: i64, duration: i64 },
        samples: || vec![vec![
            PyRep::String("UI/System/ShutdownInitiated".into()),
            PyRep::Integer(1_335_000_000_000_000_00),
            PyRep::Integer(300),
        ]],
    },
    OnCombatMessage => "OnCombatMessage" ("charid") {
        fields: { message_name: String, message_args: PyRep },
        samples: || vec![vec![
            PyRep::String("Hit".into()),
            PyRep::Dict(vec![(PyRep::String("specialObject".into()), PyRep::Integer(100_000_010))]),
        ]],
    },
    OnCommunityFittingsUpdated => "OnCommunityFittingsUpdated" ("clientID") {
        samples: || vec![vec![]],
    },
    OnComponentRemovedSystemEnvironment => "OnComponentRemovedSystemEnvironment" ("solarsystemid") {
        fields: { solar_system_id: i64 },
        samples: || vec![vec![PyRep::Integer(30_000_142)]],
    },
    OnContextualOfferPurchased => "OnContextualOfferPurchased" ("charid") {
        fields: { offer_id: i64, purchase_info: PyRep },
        samples: || vec![vec![
            PyRep::Integer(101),
            PyRep::Dict(vec![(PyRep::String("offerID".into()), PyRep::Integer(101))]),
        ]],
    },
    OnCorpWalletBillsChanged => "OnCorpWalletBillsChanged" ("corpid") {
        samples: || vec![vec![]],
    },
    OnCosmicAnomalyAdded => "OnCosmicAnomalyAdded" ("solarsystemid") {
        fields: { anomaly: PyRep },
        samples: || vec![vec![PyRep::keyval([("targetID", PyRep::String("ABC-123".into()))])]],
    },
    OnCrateLootSpawned => "OnCrateLootSpawned" ("charid") {
        samples: || vec![vec![]],
    },
    OnCrimewatchEngagementCreated => "OnCrimewatchEngagementCreated" ("charid") {
        fields: { other_char_id: i64, timeout: i64 },
        samples: || vec![vec![PyRep::Integer(90_000_002), PyRep::Integer(1_335_000_000_000_000_00)]],
    },
    OnCrimewatchEngagementEnded => "OnCrimewatchEngagementEnded" ("charid") {
        fields: { other_char_id: i64 },
        samples: || vec![vec![PyRep::Integer(90_000_002)]],
    },
    OnCrimewatchEngagementUpdated => "OnCrimewatchEngagementUpdated" ("charid") {
        fields: { other_char_id: i64, timeout: PyRep },
        optional: { do_alert: Option<bool> },
        samples: || vec![
            vec![PyRep::Integer(90_000_002), PyRep::Integer(1_335_000_000_000_000_00)],
            vec![PyRep::Integer(90_000_002), PyRep::Integer(1_335_000_000_000_000_00), PyRep::Bool(true)],
        ],
    },
    OnCriminalTimerUpdate => "OnCriminalTimerUpdate" ("charid") {
        fields: { state: i64, expiry_time: PyRep },
        optional: { do_alert: Option<bool> },
        samples: || vec![
            vec![PyRep::Integer(301), PyRep::Integer(1_335_000_000_000_000_00)],
            vec![PyRep::Integer(301), PyRep::Integer(1_335_000_000_000_000_00), PyRep::Bool(true)],
        ],
    },
    OnDamageStateChange => "OnDamageStateChange" ("charid") {
        fields: { ship_id: i64, damage_state: PyRep },
        samples: || vec![vec![
            PyRep::Integer(100_000_010),
            PyRep::Tuple(vec![
                PyRep::Tuple(vec![PyRep::Real(1.0), PyRep::Real(250.0), PyRep::Integer(1_335_000_000_000_000_00)]),
                PyRep::Real(1.0),
                PyRep::Real(1.0),
            ]),
        ]],
    },
    OnDatacoreBought => "OnDatacoreBought" ("charid") {
        fields: { character_id: i64, agent_id: i64, balance: f64 },
        samples: || vec![vec![PyRep::Integer(90_000_001), PyRep::Integer(3_000_001), PyRep::Real(500.0)]],
    },
    OnDeathzoneStateUpdate => "OnDeathzoneStateUpdate" ("charid") {
        fields: { solar_system_id: i64, ship_id: i64, tracking_data: PyRep },
        samples: || vec![vec![
            PyRep::Integer(32_000_001),
            PyRep::Integer(100_000_010),
            PyRep::None,
        ]],
    },
    OnDestinationChange => "OnDestinationChange" ("charid") {
        fields: { solar_system_id: i64, clear_other_waypoints: bool, first: bool },
        samples: || vec![vec![PyRep::Integer(30_000_142), PyRep::Bool(true), PyRep::Bool(true)]],
    },
    OnDisapprovalTimerUpdate => "OnDisapprovalTimerUpdate" ("charid") {
        fields: { state: i64, expiry_time: PyRep },
        optional: { do_alert: Option<bool> },
        samples: || vec![
            vec![PyRep::Integer(501), PyRep::Integer(1_335_000_000_000_000_00)],
            vec![PyRep::Integer(501), PyRep::Integer(1_335_000_000_000_000_00), PyRep::Bool(true)],
        ],
    },
    OnDuelChallenge => "OnDuelChallenge" ("charid") {
        fields: {
            from_char_id: i64,
            from_corp_id: i64,
            from_alliance_id: Option<i64>,
            expiry_time: i64,
        },
        samples: || vec![vec![
            PyRep::Integer(90_000_002),
            PyRep::Integer(98_000_001),
            PyRep::None,
            PyRep::Integer(1_335_000_000_000_000_00),
        ]],
    },
    OnDungeonCameraInterest => "OnDungeonCameraInterest" ("charid") {
        fields: { item_id: i64, speed: f64, duration: f64 },
        samples: || vec![vec![PyRep::Integer(100_000_010), PyRep::Real(1.5), PyRep::Real(5.0)]],
    },
    OnDungeonInstanceAdded => "OnDungeonInstanceAdded" ("solarsystemid") {
        fields: { solar_system_id: i64, instance: PyRep },
        samples: || vec![vec![
            PyRep::Integer(30_000_142),
            PyRep::keyval([
                ("signatureID", PyRep::String("ABC-123".into())),
                ("scanStrengthValue", PyRep::Real(1.0)),
            ]),
        ]],
    },
    OnDungeonInstanceRemoved => "OnDungeonInstanceRemoved" ("solarsystemid") {
        fields: { solar_system_id: i64, instance_id: i64, signature_id: String, dungeon_id: i64 },
        samples: || vec![vec![
            PyRep::Integer(30_000_142),
            PyRep::Integer(7001),
            PyRep::String("ABC-123".into()),
            PyRep::Integer(42),
        ]],
    },
    OnDungeonTriggerAmbientSound => "OnDungeonTriggerAmbientSound" ("charid") {
        fields: { dungeon_id: i64, sound_id: i64, entity_item_id: i64 },
        samples: || vec![vec![PyRep::Integer(42), PyRep::Integer(7), PyRep::Integer(100_000_010)]],
    },
    OnDungeonTriggerAudio => "OnDungeonTriggerAudio" ("charid") {
        fields: { dungeon_id: i64, audio_url: String },
        samples: || vec![vec![PyRep::Integer(42), PyRep::String("msg_audio_play".into())]],
    },
    OnDungeonTriggerMessage => "OnDungeonTriggerMessage" ("charid") {
        fields: { message_type: i64, message_id: i64 },
        samples: || vec![vec![PyRep::Integer(1), PyRep::Integer(5001)]],
    },
    OnDungeonTriggerMusic => "OnDungeonTriggerMusic" ("charid") {
        fields: { dungeon_id: i64, music_trigger_id: i64 },
        samples: || vec![vec![PyRep::Integer(42), PyRep::Integer(11)]],
    },
    OnESSMainBankLinkNotification => "OnESSMainBankLinkNotification" ("solarsystemid") {
        fields: { solar_system_id: i64 },
        samples: || vec![vec![PyRep::Integer(30_000_142)]],
    },
    OnESSReserveBankLinkNotification => "OnESSReserveBankLinkNotification" ("solarsystemid") {
        fields: { solar_system_id: i64 },
        samples: || vec![vec![PyRep::Integer(30_000_142)]],
    },
    OnEffectMessage => "OnEffectMessage" ("charid") {
        fields: { msgkey: String, args: PyRep },
        samples: || vec![vec![PyRep::String("CustomError".into()), PyRep::Dict(Vec::new())]],
    },
    OnEnterTournamentSystem => "OnEnterTournamentSystem" ("charid") {
        samples: || vec![vec![]],
    },
    OnEpicJournalChange => "OnEpicJournalChange" ("charid") {
        samples: || vec![vec![]],
    },
    OnEventResponseByExternal => "OnEventResponseByExternal" ("charid") {
        fields: { event_id: i64, event_kv: PyRep, response: i64 },
        samples: || vec![vec![
            PyRep::Integer(55),
            PyRep::keyval([("flag", PyRep::Integer(1))]),
            PyRep::Integer(1),
        ]],
    },
    OnFacilitiesUpdated => "OnFacilitiesUpdated" ("charid") {
        fields: { facility_ids: Vec<i64> },
        samples: || vec![vec![PyRep::List(vec![PyRep::Integer(60_003_760)])]],
    },
    OnFastCheckoutEnabledChanged => "OnFastCheckoutEnabledChanged" ("charid") {
        fields: { is_fast_checkout_enabled: bool },
        samples: || vec![vec![PyRep::Bool(true)]],
    },
    OnFlaggedListsChange => "OnFlaggedListsChange" ("clientID") {
        samples: || vec![vec![]],
    },
    OnFleetEscalatingPathMessage => "OnFleetEscalatingPathMessage" ("fleetid") {
        fields: { char_id: i64, journal_entry_template_id: i64 },
        samples: || vec![vec![PyRep::Integer(90_000_001), PyRep::Integer(1001)]],
    },
    OnFleetJoin => "OnFleetJoin" ("fleetid") {
        fields: { member: PyRep },
        samples: || vec![vec![PyRep::keyval([("charID", PyRep::Integer(90_000_001))])]],
    },
    OnFleetLeave => "OnFleetLeave" ("fleetid") {
        fields: { char_id: i64 },
        samples: || vec![vec![PyRep::Integer(90_000_001)]],
    },
    OnFleetLootEvent => "OnFleetLootEvent" ("fleetid") {
        fields: { loot_events: PyRep },
        samples: || vec![vec![PyRep::Dict(vec![(
            PyRep::Tuple(vec![PyRep::Integer(90_000_001), PyRep::Integer(34)]),
            PyRep::Integer(100),
        )])]],
    },
    OnFleetMemberChanged => "OnFleetMemberChanged" ("fleetid") {
        fields: {
            char_id: i64,
            fleet_id: i64,
            old_wing_id: Option<i64>,
            old_squad_id: Option<i64>,
            old_role: i64,
            old_job: i64,
            old_member_opt_outs: PyRep,
            new_wing_id: Option<i64>,
            new_squad_id: Option<i64>,
            new_role: i64,
            new_job: i64,
            new_member_opt_outs: PyRep,
            is_only_member: bool,
        },
        samples: || vec![vec![
            PyRep::Integer(90_000_001),
            PyRep::Integer(1001),
            PyRep::Integer(-1),
            PyRep::Integer(-1),
            PyRep::Integer(4),
            PyRep::Integer(0),
            PyRep::List(Vec::new()),
            PyRep::Integer(-1),
            PyRep::Integer(-1),
            PyRep::Integer(4),
            PyRep::Integer(0),
            PyRep::List(Vec::new()),
            PyRep::Bool(true),
        ]],
    },
    OnFleetMotdChanged => "OnFleetMotdChanged" ("fleetid") {
        fields: { motd: String, reload_motd: bool },
        samples: || vec![vec![PyRep::String("Fleet MOTD".into()), PyRep::Bool(true)]],
    },
    OnFleetOptionsChanged => "OnFleetOptionsChanged" ("fleetid") {
        fields: { old_options: PyRep, options: PyRep },
        samples: || vec![vec![
            PyRep::keyval([("isFreeMove", PyRep::Bool(false)), ("isRegistered", PyRep::Bool(false))]),
            PyRep::keyval([("isFreeMove", PyRep::Bool(true)), ("isRegistered", PyRep::Bool(false))]),
        ]],
    },
    OnFleetRespawnPointsUpdate => "OnFleetRespawnPointsUpdate" ("fleetid") {
        fields: { respawn_points: PyRep },
        samples: || vec![vec![PyRep::List(Vec::new())]],
    },
    OnFleetStateChange => "OnFleetStateChange" ("charid") {
        fields: { fleet_state: PyRep },
        samples: || vec![vec![PyRep::keyval([(
            "targetTags",
            PyRep::Dict(vec![(PyRep::Integer(100_000_010), PyRep::String("A".into()))]),
        )])]],
    },
    OnGlobalConfigUpdated => "OnGlobalConfigUpdated" ("clientID") {
        fields: { key: String, val: PyRep },
        samples: || vec![vec![PyRep::String("featureFlag".into()), PyRep::Bool(true)]],
    },
    OnIncomingAgentMessage => "OnIncomingAgentMessage" ("charid") {
        fields: { agent_id: i64, message: PyRep },
        samples: || vec![vec![
            PyRep::Integer(3_000_001),
            PyRep::Tuple(vec![PyRep::String("MissionOffered".into()), PyRep::Dict(Vec::new())]),
        ]],
    },
    OnIncomingTransmission => "OnIncomingTransmission" ("charid") {
        fields: { transmission: PyRep, is_agent_mission: bool },
        samples: || vec![vec![
            PyRep::keyval([
                ("header", PyRep::String("Agent Transmission".into())),
                ("text", PyRep::String("Mission details".into())),
            ]),
            PyRep::Bool(true),
        ]],
    },
    OnIndustryMaterials => "OnIndustryMaterials" ("charid") {
        fields: { job_id: i64, materials: PyRep },
        samples: || vec![vec![PyRep::Integer(1), PyRep::Dict(Vec::new())]],
    },
    OnInfluenceUpdate => "OnInfluenceUpdate" ("solarsystemid") {
        fields: { tale_id: i64, new_influence_data: PyRep },
        samples: || vec![vec![
            PyRep::Integer(701),
            PyRep::keyval([
                ("influence", PyRep::Real(0.5)),
                ("lastUpdated", PyRep::Integer(1_335_000_000_000_000_00)),
            ]),
        ]],
    },
    OnInvalidateCacheForLocations => "OnInvalidateCacheForLocations" ("charid") {
        fields: { location_ids: Vec<i64> },
        samples: || vec![vec![PyRep::List(vec![PyRep::Integer(60_003_760)])]],
    },
    OnItemChange => "OnItemChange" ("charid") {
        fields: { item: PyRep, change: PyRep, location: PyRep },
        samples: || vec![vec![
            sample_keyval_item(),
            PyRep::Dict(vec![(PyRep::Integer(3), PyRep::Integer(0))]),
            PyRep::None,
        ]],
    },
    OnKillNotification => "OnKillNotification" ("charid") {
        samples: || vec![vec![]],
    },
    OnKillRightActivated => "OnKillRightActivated" ("charid") {
        samples: || vec![vec![]],
    },
    OnKillRightForYouSold => "OnKillRightForYouSold" ("charid") {
        fields: { kill_right_id: i64 },
        samples: || vec![vec![PyRep::Integer(5001)]],
    },
    OnKillRightSold => "OnKillRightSold" ("charid") {
        fields: { kill_right_id: i64 },
        samples: || vec![vec![PyRep::Integer(5001)]],
    },
    OnKillRightUsed => "OnKillRightUsed" ("charid") {
        fields: { kill_right_id: i64, to_id: i64 },
        samples: || vec![vec![PyRep::Integer(5001), PyRep::Integer(90_000_002)]],
    },
    OnLabelsCreatedByExternal => "OnLabelsCreatedByExternal" ("charid") {
        fields: { label: PyRep },
        samples: || vec![vec![PyRep::keyval([
            ("labelID", PyRep::Integer(16)),
            ("name", PyRep::String("Custom".into())),
            ("color", PyRep::String("#ffffff".into())),
        ])]],
    },
    OnLeaveTournamentSystem => "OnLeaveTournamentSystem" ("charid") {
        samples: || vec![vec![]],
    },
    OnLocationAttributeChange => "OnLocationAttributeChange" ("charid") {
        fields: {
            owner_id: i64,
            location_id: i64,
            item_key: i64,
            attribute_id: i64,
            t: i64,
            value: f64,
            old_value: f64,
        },
        samples: || vec![vec![
            PyRep::Integer(90_000_001),
            PyRep::Integer(100_000_010),
            PyRep::Integer(100_000_050),
            PyRep::Integer(9),
            PyRep::Integer(1_335_000_000_000_000_00),
            PyRep::Real(100.0),
            PyRep::Real(90.0),
        ]],
    },
    OnLoginMilestoneAchieved => "OnLoginMilestoneAchieved" ("charid") {
        fields: { milestone_id: i64, isk_amount: f64 },
        samples: || vec![vec![PyRep::Integer(1), PyRep::Real(50_000.0)]],
    },
    OnLogonSkillsTrained => "OnLogonSkillsTrained" ("charid") {
        fields: { skill_infos: PyRep, can_train: bool },
        samples: || vec![vec![
            PyRep::Dict(vec![(
                PyRep::Integer(3400),
                PyRep::keyval([
                    ("currentLevel", PyRep::Integer(3)),
                    ("skillLevel", PyRep::Integer(3)),
                ]),
            )]),
            PyRep::Bool(true),
        ]],
    },
    OnMailDeleted => "OnMailDeleted" ("charid") {
        fields: { message_ids: Vec<i64> },
        samples: || vec![vec![PyRep::List(vec![PyRep::Integer(9001)])]],
    },
    OnMailSent => "OnMailSent" ("charid") {
        fields: {
            message_id: i64,
            sender_id: i64,
            sent_date: i64,
            to_character_ids: Vec<i64>,
            to_list_id: Option<i64>,
            to_corp_or_alliance_id: Option<i64>,
            title: String,
            status_mask: i64,
        },
        samples: || vec![vec![
            PyRep::Integer(9001),
            PyRep::Integer(90_000_001),
            PyRep::Integer(1_335_000_000_000_000_00),
            PyRep::List(vec![PyRep::Integer(90_000_002)]),
            PyRep::None,
            PyRep::None,
            PyRep::String("Hello".into()),
            PyRep::Integer(0),
        ]],
    },
    OnMailUpdatedByExternal => "OnMailUpdatedByExternal" ("charid") {
        fields: { message_ids: Vec<i64>, is_read: Option<bool>, new_label: Option<i64> },
        samples: || vec![vec![
            PyRep::List(vec![PyRep::Integer(9001)]),
            PyRep::Bool(true),
            PyRep::Integer(1),
        ]],
    },
    OnMailingListDeleted => "OnMailingListDeleted" ("charid") {
        fields: { list_id: i64 },
        samples: || vec![vec![PyRep::Integer(7001)]],
    },
    OnMailingListLeave => "OnMailingListLeave" ("charid") {
        fields: { list_id: i64, character_id: i64 },
        samples: || vec![vec![PyRep::Integer(7001), PyRep::Integer(90_000_001)]],
    },
    OnMailingListSetClear => "OnMailingListSetClear" ("charid") {
        fields: { list_id: i64 },
        samples: || vec![vec![PyRep::Integer(7001)]],
    },
    OnMailingListSetMuted => "OnMailingListSetMuted" ("charid") {
        fields: { list_id: i64 },
        samples: || vec![vec![PyRep::Integer(7001)]],
    },
    OnMailingListSetOperator => "OnMailingListSetOperator" ("charid") {
        fields: { list_id: i64 },
        samples: || vec![vec![PyRep::Integer(7001)]],
    },
    OnMainBankPlayerHackSuccess => "OnMainBankPlayerHackSuccess" ("charid") {
        fields: { data: PyRep },
        samples: || vec![vec![PyRep::Dict(vec![
            (PyRep::String("characterID".into()), PyRep::Integer(90_000_001)),
            (PyRep::String("linkID".into()), PyRep::Integer(1)),
            (PyRep::String("solarSystemID".into()), PyRep::Integer(30_000_142)),
        ])]],
    },
    OnMainBankPlayerLinkDisconnected => "OnMainBankPlayerLinkDisconnected" ("charid") {
        fields: { data: PyRep },
        samples: || vec![vec![PyRep::Dict(vec![
            (PyRep::String("characterID".into()), PyRep::Integer(90_000_001)),
            (PyRep::String("linkID".into()), PyRep::Integer(1)),
            (PyRep::String("reason".into()), PyRep::Integer(1)),
            (PyRep::String("solarSystemID".into()), PyRep::Integer(30_000_142)),
        ])]],
    },
    OnMatchMakingFailure => "OnMatchMakingFailure" ("charid") {
        fields: { errors: Vec<String> },
        samples: || vec![vec![PyRep::List(vec![PyRep::String("InvalidFleet".into())])]],
    },
    OnMilestoneTimerStarted => "OnMilestoneTimerStarted" ("charid") {
        fields: { time_left: i64 },
        samples: || vec![vec![PyRep::Integer(3600)]],
    },
    OnModuleAttributeChange => "OnModuleAttributeChange" ("charid") {
        fields: {
            owner_id: i64,
            item_id: i64,
            attribute_id: i64,
            time: i64,
            new_value: f64,
            old_value: f64,
            effective_time: i64,
            scatter: bool,
        },
        samples: || vec![vec![
            PyRep::Integer(90_000_001),
            PyRep::Integer(100_000_050),
            PyRep::Integer(9),
            PyRep::Integer(1_335_000_000_000_000_00),
            PyRep::Real(100.0),
            PyRep::Real(90.0),
            PyRep::Integer(1_335_000_000_000_000_00),
            PyRep::Bool(true),
        ]],
    },
    OnModuleAttributeChanges => "OnModuleAttributeChanges" ("charid") {
        fields: { changes: PyRep },
        samples: || vec![vec![PyRep::List(vec![
            PyRep::Tuple(vec![
                PyRep::String("OnModuleAttributeChange".into()),
                PyRep::Integer(90_000_001),
                PyRep::Integer(100_000_050),
                PyRep::Integer(9),
                PyRep::Integer(1_335_000_000_000_000_00),
                PyRep::Real(100.0),
                PyRep::Real(90.0),
                PyRep::None,
            ]),
            PyRep::Tuple(vec![
                PyRep::String("OnModuleAttributeChange".into()),
                PyRep::Integer(90_000_001),
                PyRep::Tuple(vec![
                    PyRep::Integer(100_000_050),
                    PyRep::Integer(27),
                    PyRep::Integer(2454),
                ]),
                PyRep::Integer(9),
                PyRep::Integer(1_335_000_000_000_000_00),
                PyRep::Real(10.0),
                PyRep::Real(9.0),
                PyRep::None,
            ]),
        ])]],
    },
    OnMultipleCharactersTrainingUpdated => "OnMultipleCharactersTrainingUpdated" ("charid") {
        samples: || vec![vec![]],
    },
    OnNesIntroStateChanged => "OnNesIntroStateChanged" ("charid") {
        fields: { character_id: i64, new_state_id: i64 },
        samples: || vec![vec![PyRep::Integer(90_000_001), PyRep::Integer(2)]],
    },
    OnNonDiminishingInjectionsAdded => "OnNonDiminishingInjectionsAdded" ("charid") {
        fields: { amount: i64 },
        samples: || vec![vec![PyRep::Integer(1)]],
    },
    OnNonDiminishingInjectionsRemoved => "OnNonDiminishingInjectionsRemoved" ("charid") {
        samples: || vec![vec![]],
    },
    OnNotificationUndeleted => "OnNotificationUndeleted" ("charid") {
        fields: { notification_ids: Vec<i64> },
        samples: || vec![vec![PyRep::List(vec![PyRep::Integer(101)])]],
    },
    OnNpcTimerUpdate => "OnNpcTimerUpdate" ("charid") {
        fields: { state: i64, expiry_time: PyRep },
        optional: { do_alert: Option<bool> },
        samples: || vec![
            vec![PyRep::Integer(401), PyRep::Integer(1_335_000_000_000_000_00)],
            vec![PyRep::Integer(401), PyRep::Integer(1_335_000_000_000_000_00), PyRep::Bool(true)],
        ],
    },
    OnOfferReceived => "OnOfferReceived" ("charid") {
        fields: { offer_id: i64, offer: PyRep },
        samples: || vec![vec![PyRep::Integer(101), PyRep::Dict(Vec::new())]],
    },
    OnOperationDeactivated => "OnOperationDeactivated" ("charid") {
        samples: || vec![vec![]],
    },
    OnOperationReplayed => "OnOperationReplayed" ("charid") {
        fields: { category_id: i64, operation_id: i64 },
        samples: || vec![vec![PyRep::Integer(1), PyRep::Integer(10)]],
    },
    OnOperationSkipped => "OnOperationSkipped" ("charid") {
        fields: { category_id: i64, operation_id: i64 },
        samples: || vec![vec![PyRep::Integer(1), PyRep::Integer(10)]],
    },
    OnOperationTaskSkipped => "OnOperationTaskSkipped" ("charid") {
        fields: { task_list: PyRep, includes_current: bool },
        samples: || vec![vec![
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(1),
                PyRep::Integer(10),
                PyRep::Integer(100),
                PyRep::None,
            ])]),
            PyRep::Bool(true),
        ]],
    },
    OnOperationsProgressReset => "OnOperationsProgressReset" ("charid") {
        fields: { items_to_lock: Vec<i64>, items_to_skip: Vec<i64>, task_to_activate: PyRep },
        samples: || vec![vec![
            PyRep::List(vec![PyRep::Integer(1)]),
            PyRep::List(vec![PyRep::Integer(2)]),
            PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(10), PyRep::Integer(100)]),
        ]],
    },
    OnOperationsServerFSDReload => "OnOperationsServerFSDReload" ("charid") {
        samples: || vec![vec![]],
    },
    OnOreMined => "OnOreMined" ("charid") {
        fields: { data_dict: PyRep },
        samples: || vec![vec![PyRep::Dict(vec![
            (PyRep::String("itemID".into()), PyRep::Integer(100_000_020)),
            (PyRep::String("moduleItemID".into()), PyRep::Integer(100_000_050)),
            (PyRep::String("oreType".into()), PyRep::Integer(1230)),
            (PyRep::String("quantity_added".into()), PyRep::Integer(100)),
            (PyRep::String("quantity_removed".into()), PyRep::Integer(100)),
            (PyRep::String("hasRewards".into()), PyRep::Bool(false)),
            (PyRep::String("amountWasted".into()), PyRep::Integer(0)),
            (PyRep::String("amountCritBonus".into()), PyRep::Integer(0)),
        ])]],
    },
    OnOreMinedByOther => "OnOreMinedByOther" ("charid") {
        fields: { asteroid_id: i64, quantity: i64 },
        samples: || vec![vec![PyRep::Integer(100_000_020), PyRep::Integer(50)]],
    },
    OnOrganizationContactsUpdated => "OnOrganizationContactsUpdated" ("corpid") {
        fields: { owner_id: i64, updates: PyRep },
        samples: || vec![vec![
            PyRep::Integer(98_000_001),
            PyRep::List(vec![PyRep::keyval([
                ("contactID", PyRep::Integer(90_000_002)),
                ("standing", PyRep::Real(5.0)),
                ("labelMask", PyRep::Integer(0)),
            ])]),
        ]],
    },
    OnPasswordChanged => "OnPasswordChanged" ("charid") {
        fields: { which: i64, container_item: PyRep },
        samples: || vec![vec![PyRep::Integer(1), sample_keyval_item()]],
    },
    OnPlayerPodDeath => "OnPlayerPodDeath" ("charid") {
        samples: || vec![vec![]],
    },
    OnPlayerPodRespawn => "OnPlayerPodRespawn" ("charid") {
        samples: || vec![vec![]],
    },
    OnPlayerStateUpdated => "OnPlayerStateUpdated" ("charid") {
        fields: { player_state: PyRep },
        samples: || vec![vec![PyRep::Dict(Vec::new())]],
    },
    OnPlexDepositFailed => "OnPlexDepositFailed" ("charid") {
        fields: { error_key: String, reference: PyRep },
        samples: || vec![vec![PyRep::String("InsufficientFunds".into()), PyRep::Integer(1)]],
    },
    OnPlexWithdrawFailed => "OnPlexWithdrawFailed" ("charid") {
        fields: { error_key: String, reference: PyRep },
        samples: || vec![vec![PyRep::String("InsufficientFunds".into()), PyRep::Integer(1)]],
    },
    OnPodDeath => "OnPodDeath" ("charid") {
        samples: || vec![vec![]],
    },
    OnPodKilled => "OnPodKilled" ("charid") {
        fields: { destroyed_pod_id: i64 },
        samples: || vec![vec![PyRep::Integer(100_000_099)]],
    },
    OnPodOrCorvetteAssembly => "OnPodOrCorvetteAssembly" ("charid") {
        fields: { item_id: i64, type_id: i64 },
        samples: || vec![vec![PyRep::Integer(100_000_010), PyRep::Integer(606)]],
    },
    OnPrimingNeededForTradeItems => "OnPrimingNeededForTradeItems" ("charid") {
        fields: { item_ids_to_prime: Vec<i64> },
        samples: || vec![vec![PyRep::List(vec![PyRep::Integer(100_000_001)])]],
    },
    OnProbeWarpEnd => "OnProbeWarpEnd" ("charid") {
        fields: { probe_id: i64 },
        samples: || vec![vec![PyRep::Integer(100_000_200)]],
    },
    OnProgressionAdvanced => "OnProgressionAdvanced" ("charid") {
        fields: { objective_id: i64, last_completed_task_id: i64 },
        samples: || vec![vec![PyRep::Integer(10), PyRep::Integer(9)]],
    },
    OnProgressionLeft => "OnProgressionLeft" ("charid") {
        samples: || vec![vec![]],
    },
    OnProgressionTaskCompleted => "OnProgressionTaskCompleted" ("charid") {
        fields: { task_id: i64 },
        samples: || vec![vec![PyRep::Integer(9)]],
    },
    OnProgressionTaskWidgetUpdate => "OnProgressionTaskWidgetUpdate" ("charid") {
        fields: { task_id: i64, task_widget_id: i64, widget_state: PyRep },
        samples: || vec![vec![PyRep::Integer(9), PyRep::Integer(1), PyRep::Dict(Vec::new())]],
    },
    OnProvingGroundsMatchFound => "OnProvingGroundsMatchFound" ("charid") {
        samples: || vec![vec![]],
    },
    OnPvpTimerUpdate => "OnPvpTimerUpdate" ("charid") {
        fields: { state: i64, expiry_time: PyRep },
        optional: { do_alert: Option<bool> },
        samples: || vec![
            vec![PyRep::Integer(201), PyRep::Integer(1_335_000_000_000_000_00)],
            vec![PyRep::Integer(201), PyRep::Integer(1_335_000_000_000_000_00), PyRep::Bool(true)],
        ],
    },
    OnRecommendationsChanged => "OnRecommendationsChanged" ("charid") {
        samples: || vec![vec![]],
    },
    OnRedeemingQueueUpdated => "OnRedeemingQueueUpdated" ("charid") {
        samples: || vec![vec![]],
    },
    OnRedeemingTokensUpdated => "OnRedeemingTokensUpdated" ("charid") {
        samples: || vec![vec![]],
    },
    OnReserveBankPlayerLinkDisconnected => "OnReserveBankPlayerLinkDisconnected" ("charid") {
        fields: { data: PyRep },
        samples: || vec![vec![PyRep::Dict(vec![
            (PyRep::String("characterID".into()), PyRep::Integer(90_000_001)),
            (PyRep::String("reason".into()), PyRep::Integer(1)),
            (PyRep::String("solarSystemID".into()), PyRep::Integer(30_000_142)),
        ])]],
    },
    OnReserveBankUnlock => "OnReserveBankUnlock" ("solarsystemid") {
        fields: { data: PyRep },
        samples: || vec![vec![PyRep::keyval([("regionID", PyRep::Integer(10_000_002))])]],
    },
    OnRestoringInvulnUpdated => "OnRestoringInvulnUpdated" ("charid") {
        fields: { ship_id: i64, end_time: i64, duration: i64 },
        samples: || vec![vec![
            PyRep::Integer(100_000_010),
            PyRep::Integer(1_335_000_000_000_000_00),
            PyRep::Integer(600_000_000),
        ]],
    },
    OnSafetyLevelChanged => "OnSafetyLevelChanged" ("charid") {
        fields: { safety_level: i64 },
        samples: || vec![vec![PyRep::Integer(2)]],
    },
    OnSeasonEnded => "OnSeasonEnded" ("charid") {
        samples: || vec![vec![]],
    },
    OnSeasonOmegaRewardsClaimed => "OnSeasonOmegaRewardsClaimed" ("charid") {
        fields: { claimed_goal_indexes: Vec<i64> },
        samples: || vec![vec![PyRep::List(vec![PyRep::Integer(1)])]],
    },
    OnSeasonSelected => "OnSeasonSelected" ("charid") {
        fields: { character_id: i64, season_data: PyRep, character_progress: PyRep },
        samples: || vec![vec![
            PyRep::Integer(90_000_001),
            PyRep::keyval([("seasonID", PyRep::Integer(1))]),
            PyRep::keyval([("points", PyRep::Integer(10))]),
        ]],
    },
    OnSeasonSelectionCleared => "OnSeasonSelectionCleared" ("charid") {
        samples: || vec![vec![]],
    },
    OnSeasonStarted => "OnSeasonStarted" ("charid") {
        samples: || vec![vec![]],
    },
    OnSeasonalGoalCompleted => "OnSeasonalGoalCompleted" ("charid") {
        fields: { goal_index: i64, goal_data: PyRep },
        samples: || vec![vec![PyRep::Integer(1), PyRep::Dict(Vec::new())]],
    },
    OnSeasonalGoalsReset => "OnSeasonalGoalsReset" ("charid") {
        samples: || vec![vec![]],
    },
    OnSeasonalPointsUpdated => "OnSeasonalPointsUpdated" ("charid") {
        fields: { season_id: i64, new_points: i64 },
        samples: || vec![vec![PyRep::Integer(1), PyRep::Integer(50)]],
    },
    OnSeasonsFsdDataChanged => "OnSeasonsFsdDataChanged" ("charid") {
        samples: || vec![vec![]],
    },
    OnSecurityModified => "OnSecurityModified" ("solarsystemid") {
        fields: { solar_system_id: i64, modifier_amount: f64, new_security: f64 },
        samples: || vec![vec![
            PyRep::Integer(30_000_142),
            PyRep::Real(0.05),
            PyRep::Real(0.95),
        ]],
    },
    OnSecurityStatusUpdate => "OnSecurityStatusUpdate" ("charid") {
        fields: { new_security_status: f64 },
        samples: || vec![vec![PyRep::Real(1.5)]],
    },
    OnSellRepackageErrors => "OnSellRepackageErrors" ("charid") {
        fields: { repackage_error_count: PyRep },
        samples: || vec![vec![PyRep::Dict(vec![(
            PyRep::String("CantRepackageDamagedItem".into()),
            PyRep::Integer(1),
        )])]],
    },
    OnServerBrainUpdated => "OnServerBrainUpdated" ("charid") {
        fields: { brain_data: PyRep },
        samples: || vec![vec![PyRep::Tuple(vec![PyRep::Integer(1), PyRep::None])]],
    },
    OnServerMapDataPush => "OnServerMapDataPush" ("charid") {
        fields: { tricolors: PyRep, data: PyRep },
        samples: || vec![vec![
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Real(1.0),
                PyRep::Real(0.5),
                PyRep::Real(0.0),
            ])]),
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(30_000_142),
                PyRep::Integer(0),
                PyRep::Integer(10),
                PyRep::Real(1.0),
                PyRep::Tuple(vec![PyRep::None, PyRep::None, PyRep::None]),
            ])]),
        ]],
    },
    OnServerMessage => "OnServerMessage" ("clientID") {
        fields: { msg: String },
        samples: || vec![vec![PyRep::String("Broadcast message".into())]],
    },
    OnSessionChanged => "OnSessionChanged" ("charid") {
        fields: { is_remote: bool, session: PyRep, change: PyRep },
        samples: || vec![vec![PyRep::Bool(false), PyRep::None, PyRep::Dict(Vec::new())]],
    },
    OnSetAllianceStanding => "OnSetAllianceStanding" ("allianceid") {
        fields: { contact_id: i64, standing: f64 },
        samples: || vec![vec![PyRep::Integer(98_000_001), PyRep::Real(5.0)]],
    },
    OnSetCorpStanding => "OnSetCorpStanding" ("corpid") {
        fields: { contact_id: i64, standing: f64 },
        samples: || vec![vec![PyRep::Integer(90_000_002), PyRep::Real(7.5)]],
    },
    OnShipDeath => "OnShipDeath" ("charid") {
        samples: || vec![vec![]],
    },
    OnShipScanCompleted => "OnShipScanCompleted" ("charid") {
        fields: {
            ship_id: i64,
            capacitor_charge: f64,
            capacitor_capacity: f64,
            hardware_list: Vec<(i32, i64)>,
        },
        samples: || vec![vec![
            PyRep::Integer(100_000_010),
            PyRep::Real(500.0),
            PyRep::Real(1000.0),
            PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(519), PyRep::Integer(1)])]),
        ]],
    },
    OnSignalTrackerFullState => "OnSignalTrackerFullState" ("charid") {
        fields: { solar_system_id: i64, full_state: PyRep, should_remove_old_sites: bool },
        samples: || vec![vec![
            PyRep::Integer(30_000_142),
            PyRep::Tuple(vec![
                PyRep::List(Vec::new()),
                PyRep::List(Vec::new()),
                PyRep::List(Vec::new()),
                PyRep::List(Vec::new()),
            ]),
            PyRep::Bool(true),
        ]],
    },
    OnSkillBundleInjected => "OnSkillBundleInjected" ("charid") {
        fields: { number_of_skills: i64, sp: i64 },
        samples: || vec![vec![PyRep::Integer(3), PyRep::Integer(150_000)]],
    },
    OnSkillForcedRefresh => "OnSkillForcedRefresh" ("charid") {
        samples: || vec![vec![]],
    },
    OnSolarSystemLPChange => "OnSolarSystemLPChange" ("solarsystemid") {
        fields: { old_points: i64, new_points: i64 },
        samples: || vec![vec![PyRep::Integer(100), PyRep::Integer(250)]],
    },
    OnSpaceWhiteOutStart => "OnSpaceWhiteOutStart" ("charid") {
        fields: { solar_system_id: i64 },
        samples: || vec![vec![PyRep::Integer(30_000_142)]],
    },
    OnStartClientNodeGraph => "OnStartClientNodeGraph" ("charid") {
        fields: { node_graph_id: i64, node_graph_instance_id: i64, blackboard_parameters: PyRep },
        samples: || vec![vec![PyRep::Integer(10), PyRep::Integer(1), PyRep::Dict(Vec::new())]],
    },
    OnStartConversation => "OnStartConversation" ("charid") {
        fields: { conversation_id: i64, show_continue_button: bool },
        samples: || vec![vec![PyRep::Integer(101), PyRep::Bool(true)]],
    },
    OnStopAllClientNodeGraphs => "OnStopAllClientNodeGraphs" ("charid") {
        samples: || vec![vec![]],
    },
    OnStopClientNodeGraph => "OnStopClientNodeGraph" ("charid") {
        fields: { node_graph_instance_id: i64 },
        samples: || vec![vec![PyRep::Integer(1)]],
    },
    OnStopClientNodeGraphById => "OnStopClientNodeGraphById" ("charid") {
        fields: { node_graph_id: i64 },
        samples: || vec![vec![PyRep::Integer(10)]],
    },
    OnSubscriptionChangedServer => "OnSubscriptionChangedServer" ("charid") {
        fields: { new_state: i64 },
        samples: || vec![vec![PyRep::Integer(1)]],
    },
    OnSubscriptionTimeUpdateServer => "OnSubscriptionTimeUpdateServer" ("charid") {
        fields: { omega_time: i64 },
        samples: || vec![vec![PyRep::Integer(1_335_000_000_000_000_00)]],
    },
    OnSystemActivityUpdated => "OnSystemActivityUpdated" ("solarsystemid") {
        samples: || vec![vec![]],
    },
    OnSystemCriminalFlagUpdates => "OnSystemCriminalFlagUpdates" ("solarsystemid") {
        fields: { new_idles: Vec<i64>, new_suspects: Vec<i64>, new_criminals: Vec<i64> },
        samples: || vec![vec![
            PyRep::List(vec![PyRep::Integer(90_000_001)]),
            PyRep::List(vec![PyRep::Integer(90_000_002)]),
            PyRep::List(Vec::new()),
        ]],
    },
    OnSystemDisapprovalFlagUpdates => "OnSystemDisapprovalFlagUpdates" ("solarsystemid") {
        fields: { new_idles: Vec<i64>, new_naughty_boys_and_girls: Vec<i64> },
        samples: || vec![vec![
            PyRep::List(vec![PyRep::Integer(90_000_001)]),
            PyRep::List(Vec::new()),
        ]],
    },
    OnSystemDynamicBountyStateChanged => "OnSystemDynamicBountyStateChanged" ("solarsystemid") {
        fields: { system_id: i64, rate: f64 },
        samples: || vec![vec![PyRep::Integer(30_000_142), PyRep::Real(1.25)]],
    },
    OnSystemESSDataChanged => "OnSystemESSDataChanged" ("solarsystemid") {
        fields: { system_id: i64, data: PyRep },
        samples: || vec![vec![PyRep::Integer(30_000_142), PyRep::Dict(Vec::new())]],
    },
    OnSystemEffectStarted => "OnSystemEffectStarted" ("solarsystemid") {
        fields: { type_ids: Vec<i64> },
        samples: || vec![vec![PyRep::List(vec![PyRep::Integer(10001)])]],
    },
    OnSystemWideEffectStop => "OnSystemWideEffectStop" ("solarsystemid") {
        fields: { effect_id: i64, source_item_id: i64, source_type_id: i64 },
        samples: || vec![vec![
            PyRep::Integer(6001),
            PyRep::Integer(100_000_010),
            PyRep::Integer(35832),
        ]],
    },
    OnTaleData => "OnTaleData" ("solarsystemid") {
        fields: { solar_system_id: i64, data: PyRep },
        samples: || vec![vec![
            PyRep::Integer(30_000_142),
            PyRep::Dict(vec![(PyRep::Integer(701), PyRep::keyval([("taleID", PyRep::Integer(701))]))]),
        ]],
    },
    OnTaleRemove => "OnTaleRemove" ("solarsystemid") {
        fields: { tale_id: i64, template_class_id: i64, template_id: i64 },
        samples: || vec![vec![PyRep::Integer(701), PyRep::Integer(1), PyRep::Integer(51)]],
    },
    OnTournamentMatchStateChange => "OnTournamentMatchStateChange" ("charid") {
        fields: { red_points: i64, blue_points: i64, my_team: i64, match_status: i64 },
        samples: || vec![vec![
            PyRep::Integer(10),
            PyRep::Integer(5),
            PyRep::Integer(0),
            PyRep::Integer(1),
        ]],
    },
    OnTutorialSkipped => "OnTutorialSkipped" ("charid") {
        samples: || vec![vec![]],
    },
    OnUiRevealForced => "OnUiRevealForced" ("charid") {
        samples: || vec![vec![]],
    },
    OnUpdateCapitalInfo => "OnUpdateCapitalInfo" ("allianceid") {
        samples: || vec![vec![]],
    },
    OnUpdateLocalizationTextCache => "OnUpdateLocalizationTextCache" ("charid") {
        fields: { cache_data: PyRep },
        samples: || vec![vec![PyRep::Tuple(vec![
            PyRep::Dict(Vec::new()),
            PyRep::Dict(Vec::new()),
            PyRep::Dict(Vec::new()),
        ])]],
    },
    OnUpdateServiceMapping => "OnUpdateServiceMapping" ("charid") {
        fields: { svc: String, node_id: i64 },
        samples: || vec![vec![PyRep::String("dogmaIM".into()), PyRep::Integer(26_000)]],
    },
    OnVictoryPointStateUpdated => "OnVictoryPointStateUpdated" ("solarsystemid") {
        fields: { solar_system_id: i64, new_victory_point_state: PyRep },
        samples: || vec![vec![PyRep::Integer(30_000_142), PyRep::Dict(Vec::new())]],
    },
    OnWeaponsTimerUpdate => "OnWeaponsTimerUpdate" ("charid") {
        fields: { state: i64, expiry_time: PyRep },
        optional: { do_alert: Option<bool> },
        samples: || vec![
            vec![PyRep::Integer(101), PyRep::Integer(1_335_000_000_000_000_00)],
            vec![PyRep::Integer(101), PyRep::Integer(1_335_000_000_000_000_00), PyRep::Bool(true)],
        ],
    },
}
