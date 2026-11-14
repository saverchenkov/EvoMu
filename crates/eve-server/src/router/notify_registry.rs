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

/// Builds one fleet-scoped typed notification and enqueues/broadcasts it to each fleet member
/// (used by dungeon/escalation hooks outside `ServerContext`, retaining the 256-entry FIFO bound).
pub fn emit_typed_to_fleet_members_core<N: TypedNotification>(
    db: &eve_db::WorldRepository,
    pending_notifications: Option<&dyn AnyRwLock<HashMap<i64, Vec<PendingMachoNotification>>>>,
    notify_tx: Option<&broadcast::Sender<(i64, PendingMachoNotification)>>,
    fleet_id: i64,
    members: &[i64],
    notif: N,
) {
    if fleet_id <= 0 || members.is_empty() {
        return;
    }
    let now_ft = db.clock().now_blueos_filetime_ticks();
    let args = notif.into_args();
    let (packet, marshaled_packet) = ServerContext::build_macho_notification_packet(
        fleet_id,
        None,
        N::EVENT,
        N::DEFAULT_ID_TYPE,
        args.clone(),
    );
    for &member_id in members {
        if member_id <= 0 {
            continue;
        }
        let pending = PendingMachoNotification {
            seq_id: next_notification_seq_id(),
            recipient_char_id: member_id,
            event: N::EVENT.to_string(),
            id_type: N::DEFAULT_ID_TYPE.to_string(),
            args: args.clone(),
            packet: packet.clone(),
            marshaled_packet: marshaled_packet.clone(),
            created_filetime: now_ft,
        };
        if let Some(pend) = pending_notifications {
            let mut queues = pend.write().unwrap_or_else(|e| e.into_inner());
            let queue = queues.entry(member_id).or_default();
            while queue.len() >= 256 {
                queue.remove(0);
            }
            queue.push(pending.clone());
        }
        if let Some(tx) = notify_tx {
            let _ = tx.send((member_id, pending));
        }
    }
}

/// Broadcasts one typed notification to all in-space characters currently in `solar_system_id`
/// outside [`ServerContext`] (used by expedition destruction hooks, retaining the 512-entry FIFO bound).
pub fn emit_typed_to_solar_system_in_space_core<N: TypedNotification>(
    db: &eve_db::WorldRepository,
    pending_notifications: &dyn AnyRwLock<HashMap<i64, Vec<PendingMachoNotification>>>,
    notify_tx: &broadcast::Sender<(i64, PendingMachoNotification)>,
    solar_system_id: u32,
    notif: N,
) {
    let now_ft = db.clock().now_blueos_filetime_ticks();
    let recips: Vec<(i64, Option<u64>)> = db.with_snapshot(|snap| {
        snap.characters
            .values()
            .filter(|c| {
                c.station_id.is_none()
                    && c.structure_id.is_none()
                    && c.solar_system_id == solar_system_id
            })
            .map(|c| (c.character_id, Some(c.user_id)))
            .collect()
    });
    let args = notif.into_args();
    for (cid, uid) in recips {
        if cid <= 0 {
            continue;
        }
        let (packet, marshaled_packet) = ServerContext::build_macho_notification_packet(
            cid,
            uid,
            N::EVENT,
            N::DEFAULT_ID_TYPE,
            args.clone(),
        );
        let pending = PendingMachoNotification {
            seq_id: next_notification_seq_id(),
            recipient_char_id: cid,
            event: N::EVENT.to_string(),
            id_type: N::DEFAULT_ID_TYPE.to_string(),
            args: args.clone(),
            packet,
            marshaled_packet,
            created_filetime: now_ft,
        };
        {
            let mut q_guard = pending_notifications
                .write()
                .unwrap_or_else(|e| e.into_inner());
            let q = q_guard.entry(cid).or_default();
            while q.len() >= 512 {
                q.remove(0);
            }
            q.push(pending.clone());
        }
        let _ = notify_tx.send((cid, pending));
    }
}

/// Pushes a typed notification onto an active [`eve_machonet::SessionState`].
pub fn push_typed_session_notification<N: TypedNotification>(
    session: &mut eve_machonet::SessionState,
    notif: N,
) {
    session.push_notification(N::EVENT, notif.into_args());
}

/// Flushes an already-constructed [`PendingMachoNotification`] onto an active [`eve_machonet::SessionState`].
pub fn push_pending_session_notification(
    session: &mut eve_machonet::SessionState,
    notif: PendingMachoNotification,
) {
    session.push_notification(notif.event, notif.args);
}

/// Encodes a registered notification by event name into a wire-ready [`PendingMachoNotification`]
/// after verifying that `event` is registered in [`all_registered_notifications`] and `args.len()`
/// satisfies its registered arity bounds.
pub fn build_pending_registered_notification(
    recipient_char_id: i64,
    user_id_opt: Option<u64>,
    event: &str,
    id_type: &str,
    args: Vec<PyRep>,
    created_filetime: i64,
) -> Option<PendingMachoNotification> {
    let entry = all_registered_notifications()
        .iter()
        .find(|e| e.event == event)?;
    if args.len() < entry.min_arity || args.len() > entry.max_arity {
        return None;
    }
    let (packet, marshaled_packet) = ServerContext::build_macho_notification_packet(
        recipient_char_id,
        user_id_opt,
        entry.event,
        id_type,
        args.clone(),
    );
    Some(PendingMachoNotification::new(
        recipient_char_id,
        entry.event,
        id_type,
        args,
        packet,
        marshaled_packet,
        created_filetime,
    ))
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

    // Zero required + trailing optional arguments (e.g. 0..=2 varargs events)
    ($name:ident => $event:literal ($id_type:literal) {
        optional: { $($opt_field:ident : Option<$opt_ty:ty>),+ $(,)? },
        samples: $samples:expr $(,)?
    }) => {
        #[derive(Debug, Clone, PartialEq)]
        pub struct $name {
            $(pub $opt_field: Option<$opt_ty>),+
        }

        impl TypedNotification for $name {
            const EVENT: &'static str = $event;
            const DEFAULT_ID_TYPE: &'static str = $id_type;
            const MIN_ARITY: usize = 0;
            const MAX_ARITY: usize = count_idents!($($opt_field),+);

            fn into_args(self) -> Vec<PyRep> {
                let mut args = Vec::new();
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
    (@entry $name:ident => $event:literal ($id_type:literal) {
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
    OnNewSkillQueueSaved => "OnNewSkillQueueSaved" ("charid") {
        fields: { new_queue: PyRep },
        samples: || vec![vec![PyRep::List(vec![PyRep::keyval([
            ("trainingTypeID", PyRep::Integer(3402)),
            ("trainingToLevel", PyRep::Integer(2)),
            ("queuePosition", PyRep::Integer(0)),
        ])])]],
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

    // --- Wave 5D Batch 2 (Refs #932) ---
    OnAgentAdded => "OnAgentAdded" ("charid") {
        fields: { entity_id: i64 },
        samples: || vec![vec![PyRep::Integer(3_018_916)]],
    },
    OnAssetSafetyCreated => "OnAssetSafetyCreated" ("charid") {
        fields: { owner_id: i64, solar_system_id: i64, location_id: i64 },
        samples: || vec![vec![
            PyRep::Integer(90_000_001),
            PyRep::Integer(30_000_142),
            PyRep::Integer(1_000_000_000_001),
        ]],
    },
    OnAssetSafetyDelivered => "OnAssetSafetyDelivered" ("charid") {
        fields: { owner_id: i64 },
        samples: || vec![vec![PyRep::Integer(90_000_001)]],
    },
    OnAssignPetition => "OnAssignPetition" ("charid") {
        fields: { petition_id: i64, assignee_id: Option<i64> },
        samples: || vec![
            vec![PyRep::Integer(1), PyRep::Integer(90_000_002)],
            vec![PyRep::Integer(1), PyRep::None],
        ],
    },
    OnAssumeStructureControl => "OnAssumeStructureControl" ("charid") {
        optional: { char_id: Option<i64>, item_id: Option<i64> },
        samples: || vec![
            vec![],
            vec![PyRep::Integer(90_000_001), PyRep::Integer(100_000_001)],
        ],
    },
    OnBillReceived => "OnBillReceived" ("corpid") {
        optional: { bill: Option<PyRep> },
        samples: || vec![
            vec![],
            vec![PyRep::keyval([("billID", PyRep::Integer(1))])],
        ],
    },
    OnBountyAddedToPayout => "OnBountyAddedToPayout" ("charid") {
        fields: { data_dict: PyRep },
        samples: || vec![vec![PyRep::Dict(vec![
            (PyRep::String("amount".into()), PyRep::Real(10_000.0)),
            (PyRep::String("payoutTime".into()), PyRep::Integer(1_335_000_000_000_000_00)),
            (PyRep::String("enemyTypeID".into()), PyRep::Integer(587)),
            (PyRep::String("isModified".into()), PyRep::Bool(false)),
        ])]],
    },
    OnChannelDeleted => "OnChannelDeleted" ("channelid") {
        fields: { channel: String },
        samples: || vec![vec![PyRep::String("player:1".into())]],
    },
    OnChannelJoined => "OnChannelJoined" ("channelid") {
        fields: { channel: String },
        optional: { character_id: Option<i64> },
        samples: || vec![
            vec![PyRep::String("solarsystemid2:30000142".into())],
            vec![PyRep::String("solarsystemid2:30000142".into()), PyRep::Integer(90_000_001)],
        ],
    },
    OnClientEventChatMessageSent => "OnClientEvent_ChatMessageSent" ("channelid") {
        samples: || vec![vec![]],
    },
    OnCloneJumpUpdate => "OnCloneJumpUpdate" ("charid") {
        samples: || vec![vec![]],
    },
    OnClosePetition => "OnClosePetition" ("charid") {
        optional: { petition_id: Option<i64> },
        samples: || vec![vec![], vec![PyRep::Integer(1)]],
    },
    OnContactNoLongerContact => "OnContactNoLongerContact" ("charid") {
        fields: { char_id: i64 },
        samples: || vec![vec![PyRep::Integer(90_000_002)]],
    },
    OnCorpAggressionSettingsChange => "OnCorpAggressionSettingsChange" ("corpid") {
        fields: { aggression_settings: PyRep },
        samples: || vec![vec![PyRep::keyval([("friendlyFire", PyRep::Bool(false))])]],
    },
    OnCorpLPChange => "OnCorpLPChange" ("corpid") {
        fields: { reason: String, issuer_corp_id: i64 },
        samples: || vec![vec![PyRep::String("take_offer".into()), PyRep::Integer(1_000_035)]],
    },
    OnCorpLogoReady => "OnCorpLogoReady" ("corpid") {
        fields: { corp_id: i64, size: i64 },
        samples: || vec![vec![PyRep::Integer(98_000_001), PyRep::Integer(128)]],
    },
    OnCorporationApplicationChanged => "OnCorporationApplicationChanged" ("corpid") {
        fields: { corp_id: i64, applicant_id: i64, application_id: i64, new_application: PyRep },
        samples: || vec![vec![
            PyRep::Integer(98_000_001),
            PyRep::Integer(90_000_001),
            PyRep::Integer(1),
            PyRep::keyval([("applicationID", PyRep::Integer(1))]),
        ]],
    },
    OnCorporationChanged => "OnCorporationChanged" ("corpid") {
        fields: { corp_id: i64, change: PyRep },
        samples: || vec![vec![
            PyRep::Integer(98_000_001),
            PyRep::Dict(vec![
                (
                    PyRep::String("isRecruiting".into()),
                    PyRep::Tuple(vec![PyRep::Bool(false), PyRep::Bool(true)]),
                ),
                (
                    PyRep::String("stationID".into()),
                    PyRep::Tuple(vec![PyRep::None, PyRep::Integer(60_003_760)]),
                ),
            ]),
        ]],
    },
    OnCorporationMemberChanged => "OnCorporationMemberChanged" ("corpid") {
        fields: { corporation_id: i64, member_id: i64, change: PyRep },
        samples: || vec![vec![
            PyRep::Integer(98_000_001),
            PyRep::Integer(90_000_001),
            PyRep::Dict(vec![(
                PyRep::String("corporationID".into()),
                PyRep::Tuple(vec![PyRep::Integer(98_000_001), PyRep::Integer(98_000_002)]),
            )]),
        ]],
    },
    OnCorporationRecruitmentAdChanged => "OnCorporationRecruitmentAdChanged" ("corpid") {
        optional: { corp_id: Option<i64>, ad_id: Option<i64> },
        samples: || vec![
            vec![],
            vec![PyRep::Integer(98_000_001), PyRep::Integer(1)],
        ],
    },
    OnCorporationStructuresUpdated => "OnCorporationStructuresUpdated" ("corpid") {
        samples: || vec![vec![]],
    },
    OnCorporationWelcomeMailChanged => "OnCorporationWelcomeMailChanged" ("corpid") {
        fields: { character_id: i64, change_date: i64 },
        samples: || vec![vec![PyRep::Integer(90_000_001), PyRep::Integer(1_700_000_000)]],
    },
    OnCrimewatchEngagementStartTimeout => "OnCrimewatchEngagementStartTimeout" ("charid") {
        fields: { other_char_id: i64, timeout: i64 },
        samples: || vec![vec![
            PyRep::Integer(90_000_002),
            PyRep::Integer(1_335_000_000_000_000_00),
        ]],
    },
    OnCrimewatchEngagementStopTimeout => "OnCrimewatchEngagementStopTimeout" ("charid") {
        fields: { other_char_id: i64 },
        samples: || vec![vec![PyRep::Integer(90_000_002)]],
    },
    OnCurrentStructureStateChanged => "OnCurrentStructureStateChanged" ("solarsystemid") {
        fields: { solar_system_id: i64, structure_id: i64, state: String },
        samples: || vec![vec![
            PyRep::Integer(30_000_142),
            PyRep::Integer(1_000_000_000_001),
            PyRep::String("Unanchoring".into()),
        ]],
    },
    OnDamageMessage => "OnDamageMessage" ("charid") {
        fields: { damage_messages_args: PyRep },
        samples: || vec![vec![PyRep::Dict(vec![
            (PyRep::String("attackType".into()), PyRep::String("other".into())),
            (PyRep::String("damage".into()), PyRep::Real(42.5)),
            (PyRep::String("source".into()), PyRep::Integer(100_000_001)),
            (PyRep::String("target".into()), PyRep::Integer(100_000_002)),
            (PyRep::String("attackerID".into()), PyRep::Integer(90_000_001)),
            (PyRep::String("typeID".into()), PyRep::Integer(561)),
            (
                PyRep::String("owner".into()),
                PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(90_000_001)]),
            ),
        ])]],
    },
    OnDamageMessages => "OnDamageMessages" ("charid") {
        fields: { dmgmsgs: Vec<PyRep> },
        samples: || vec![vec![PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::String("AttackHit".into()),
            PyRep::Dict(vec![
                (PyRep::String("attackType".into()), PyRep::String("you".into())),
                (PyRep::String("damage".into()), PyRep::Real(42.5)),
            ]),
        ])])]],
    },
    OnDirectionalScanComplete => "OnDirectionalScanComplete" ("charid") {
        fields: { angle: f64, range: f64, direction: PyRep, results: PyRep },
        samples: || vec![vec![
            PyRep::Real(1.0),
            PyRep::Real(1_000_000.0),
            PyRep::Tuple(vec![PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)]),
            PyRep::List(Vec::new()),
        ]],
    },
    OnDockingAccessChangedForCurrentSolarSystem => "OnDockingAccessChangedForCurrentSolarSystem" ("solarsystemid") {
        samples: || vec![vec![]],
    },
    OnFighterAddedToController => "OnFighterAddedToController" ("charid") {
        fields: {
            fighter_id: i64,
            fighter_type_id: i64,
            tube_flag_id: i64,
            squadron_size: i64,
            ability_slot_states: PyRep,
        },
        samples: || vec![vec![
            PyRep::Integer(100_000_050),
            PyRep::Integer(23055),
            PyRep::Integer(159),
            PyRep::Integer(9),
            PyRep::Dict(Vec::new()),
        ]],
    },
    OnFighterRemovedFromController => "OnFighterRemovedFromController" ("charid") {
        fields: { fighter_id: i64, tube_flag_id: i64 },
        samples: || vec![vec![PyRep::Integer(100_000_050), PyRep::Integer(159)]],
    },
    OnFighterTubeContentEmpty => "OnFighterTubeContentEmpty" ("charid") {
        fields: { tube_flag_id: i64 },
        samples: || vec![vec![PyRep::Integer(159)]],
    },
    OnFighterTubeContentUpdate => "OnFighterTubeContentUpdate" ("charid") {
        fields: { tube_flag_id: i64, fighter_id: i64, fighter_type_id: i64, squadron_size: i64 },
        samples: || vec![vec![
            PyRep::Integer(159),
            PyRep::Integer(100_000_050),
            PyRep::Integer(23055),
            PyRep::Integer(9),
        ]],
    },
    OnFighterTubeTaskStatus => "OnFighterTubeTaskStatus" ("charid") {
        fields: {
            tube_flag_id: i64,
            status_id: String,
            status_start_time: i64,
            status_end_time: i64,
            user_error: Option<PyRep>,
        },
        samples: || vec![vec![
            PyRep::Integer(159),
            PyRep::String("INSPACE".into()),
            PyRep::Integer(1_335_000_000_000_000_00),
            PyRep::Integer(1_335_000_000_000_000_00),
            PyRep::None,
        ]],
    },
    OnFleetFinderAdvertChanged => "OnFleetFinderAdvertChanged" ("fleetid") {
        samples: || vec![vec![]],
    },
    OnFleetFormationWndChanged => "OnFleetFormationWndChanged" ("charid") {
        samples: || vec![vec![]],
    },
    OnFleetJoinRejected => "OnFleetJoinRejected" ("charid") {
        fields: { char_id: i64 },
        samples: || vec![vec![PyRep::Integer(90_000_001)]],
    },
    OnFleetJoinRequest => "OnFleetJoinRequest" ("charid") {
        fields: { info: PyRep },
        samples: || vec![vec![PyRep::keyval([
            ("charID", PyRep::Integer(90_000_002)),
            ("corpID", PyRep::Integer(98_000_001)),
        ])]],
    },
    OnFleetSetupChanged => "OnFleetSetupChanged" ("charid") {
        samples: || vec![vec![]],
    },
    OnFleetSquadAdded => "OnFleetSquadAdded" ("fleetid") {
        fields: { wing_id: i64, squad_id: i64 },
        samples: || vec![vec![PyRep::Integer(1), PyRep::Integer(10)]],
    },
    OnFleetWingAdded => "OnFleetWingAdded" ("fleetid") {
        fields: { wing_id: i64 },
        samples: || vec![vec![PyRep::Integer(1)]],
    },
    OnHomeStationChanged => "OnHomeStationChanged" ("charid") {
        fields: { new_home_station_id: i64 },
        samples: || vec![vec![PyRep::Integer(60_003_760)]],
    },
    OnHomeStationMovedFromStructure => "OnHomeStationMovedFromStructure" ("charid") {
        samples: || vec![vec![]],
    },
    OnInSpaceSquadronSizeChanged => "OnInSpaceSquadronSizeChanged" ("charid") {
        fields: { fighter_id: i64, squadron_size: i64 },
        samples: || vec![vec![PyRep::Integer(100_000_050), PyRep::Integer(8)]],
    },
    OnInsurgencyCampaignEndingForLocation => "OnInsurgencyCampaignEndingForLocation" ("solarsystemid") {
        fields: { solar_system_id: i64 },
        samples: || vec![vec![PyRep::Integer(30_000_142)]],
    },
    OnInsurgencyCampaignStartedForLocation => "OnInsurgencyCampaignStartedForLocation" ("solarsystemid") {
        fields: { campaign_snapshot: PyRep },
        samples: || vec![vec![PyRep::keyval([(
            "coveredSolarsystemIDs",
            PyRep::List(vec![PyRep::Integer(30_000_142)]),
        )])]],
    },
    OnInsurgencyCampaignUpdatedForLocation => "OnInsurgencyCampaignUpdatedForLocation" ("solarsystemid") {
        fields: { campaign_snapshot: PyRep },
        samples: || vec![vec![PyRep::keyval([(
            "coveredSolarsystemIDs",
            PyRep::List(vec![PyRep::Integer(30_000_142)]),
        )])]],
    },
    OnInsurgencyStateChangedForFaction => "OnInsurgencyStateChangedForFaction" ("solarsystemid") {
        fields: { campaign_state: i64, pirate_faction_id: i64, origin_solar_system_id: i64 },
        samples: || vec![vec![
            PyRep::Integer(1),
            PyRep::Integer(500_010),
            PyRep::Integer(30_000_142),
        ]],
    },
    OnJoinRequestUpdate => "OnJoinRequestUpdate" ("charid") {
        fields: { join_requests: PyRep },
        samples: || vec![vec![PyRep::Dict(Vec::new())]],
    },
    OnKillRightCreated => "OnKillRightCreated" ("charid") {
        fields: { kill_right_id: i64, from_id: i64, to_id: i64, expiry_time: i64 },
        samples: || vec![vec![
            PyRep::Integer(1),
            PyRep::Integer(90_000_001),
            PyRep::Integer(90_000_002),
            PyRep::Integer(1_335_000_000_000_000_00),
        ]],
    },
    OnLPChange => "OnLPChange" ("charid") {
        fields: { change_type: String, issuer_corp_id: i64, lp_before: i64, lp_after: i64 },
        samples: || vec![vec![
            PyRep::String("take_offer".into()),
            PyRep::Integer(1_000_035),
            PyRep::Integer(10_000),
            PyRep::Integer(9_000),
        ]],
    },
    OnMoonExtractionUpdate => "OnMoonExtractionUpdate" ("corpid") {
        fields: { extraction: Option<PyRep> },
        samples: || vec![vec![PyRep::None]],
    },
    OnMutualWarsUpdatedRemote => "OnMutualWarsUpdated_Remote" ("corpid") {
        samples: || vec![vec![]],
    },
    OnNewCalendarEvent => "OnNewCalendarEvent" ("corpid") {
        fields: {
            event_id: i64,
            owner_id: i64,
            event_date_time: i64,
            event_duration: i64,
            event_title: String,
            importance: i64,
            auto_event_type: i64,
            do_blink: bool,
        },
        samples: || vec![vec![
            PyRep::Integer(1),
            PyRep::Integer(98_000_001),
            PyRep::Integer(1_335_000_000_000_000_00),
            PyRep::Integer(60),
            PyRep::String("Moon Extraction".into()),
            PyRep::Integer(1),
            PyRep::Integer(1),
            PyRep::Bool(true),
        ]],
    },
    OnNewPetition => "OnNewPetition" ("charid") {
        fields: { petition_id: i64 },
        samples: || vec![vec![PyRep::Integer(1)]],
    },
    OnNewProbe => "OnNewProbe" ("charid") {
        fields: { probe: PyRep },
        samples: || vec![vec![PyRep::keyval([("probeID", PyRep::Integer(100_000_001))])]],
    },
    OnOfficeRentalChange => "OnOfficeRentalChange" ("corpid") {
        fields: { corporation_id: i64, office_id: Option<i64> },
        samples: || vec![
            vec![PyRep::Integer(98_000_001), PyRep::Integer(100_000_010)],
            vec![PyRep::Integer(98_000_001), PyRep::None],
        ],
    },
    OnOperationCategoryChanged => "OnOperationCategoryChanged" ("charid") {
        fields: { new_category_id: i64, new_operation_id: i64 },
        samples: || vec![vec![PyRep::Integer(1), PyRep::Integer(10)]],
    },
    OnOperationCategoryCompleted => "OnOperationCategoryCompleted" ("charid") {
        fields: { category_id: i64 },
        samples: || vec![vec![PyRep::Integer(1)]],
    },
    OnOperationTaskCompleted => "OnOperationTaskCompleted" ("charid") {
        fields: { category_id: i64, operation_id: i64, task_id: i64 },
        samples: || vec![vec![PyRep::Integer(1), PyRep::Integer(10), PyRep::Integer(100)]],
    },
    OnOwnerGroupsUpdated => "OnOwnerGroupsUpdated" ("corpid") {
        fields: { changed_groups: PyRep },
        samples: || vec![vec![PyRep::Dict(vec![(
            PyRep::Integer(98_000_001),
            PyRep::Integer(1_000_000_000_001),
        )])]],
    },
    OnPersonalContactsAdded => "OnPersonalContactsAdded" ("charid") {
        fields: { contacts: Vec<PyRep>, relationship_id: f64, watchlist: bool, label_mask: i64 },
        samples: || vec![vec![
            PyRep::List(vec![PyRep::keyval([
                ("contactID", PyRep::Integer(90_000_002)),
                ("isOnline", PyRep::Bool(true)),
            ])]),
            PyRep::Real(5.0),
            PyRep::Bool(true),
            PyRep::Integer(0),
        ]],
    },
    OnPersonalContactsDeleted => "OnPersonalContactsDeleted" ("charid") {
        fields: { contacts: Vec<PyRep> },
        samples: || vec![vec![PyRep::List(vec![PyRep::keyval([(
            "contactID",
            PyRep::Integer(90_000_002),
        )])])]],
    },
    OnPersonalContactsUpdated => "OnPersonalContactsUpdated" ("charid") {
        fields: { contact_ids: Vec<i64>, standing: f64, in_watchlist: Option<bool> },
        samples: || vec![vec![
            PyRep::List(vec![PyRep::Integer(90_000_002)]),
            PyRep::Real(5.0),
            PyRep::Bool(true),
        ]],
    },
    OnPetitionCreated => "OnPetitionCreated" ("charid") {
        optional: { petition_id: Option<i64> },
        samples: || vec![vec![], vec![PyRep::Integer(1)]],
    },
    OnPetitionMessage => "OnPetitionMessage" ("charid") {
        fields: { petition_id: i64, char_id: i64, message_text: String, message_id: i64 },
        samples: || vec![vec![
            PyRep::Integer(1),
            PyRep::Integer(90_000_001),
            PyRep::String("Hello".into()),
            PyRep::Integer(10),
        ]],
    },
    OnProbeStateChanged => "OnProbeStateChanged" ("charid") {
        fields: { probe_id: i64, probe_state: i64 },
        samples: || vec![vec![PyRep::Integer(100_000_001), PyRep::Integer(0)]],
    },
    OnProbeWarpStart => "OnProbeWarpStart" ("charid") {
        fields: { probe_id: i64, from_pos: PyRep, to_pos: PyRep, start_time: i64, duration: i64 },
        samples: || vec![vec![
            PyRep::Integer(100_000_001),
            PyRep::Tuple(vec![PyRep::Real(0.0), PyRep::Real(0.0), PyRep::Real(0.0)]),
            PyRep::Tuple(vec![PyRep::Real(1000.0), PyRep::Real(0.0), PyRep::Real(0.0)]),
            PyRep::Integer(1_335_000_000_000_000_00),
            PyRep::Integer(2500),
        ]],
    },
    OnProbesIdle => "OnProbesIdle" ("charid") {
        fields: { probes: Vec<PyRep> },
        samples: || vec![vec![PyRep::List(vec![PyRep::keyval([(
            "probeID",
            PyRep::Integer(100_000_001),
        )])])]],
    },
    OnProfileSettingsChange => "OnProfileSettingsChange" ("corpid") {
        fields: { profile_ids: Vec<i64> },
        samples: || vec![vec![PyRep::List(vec![PyRep::Integer(1)])]],
    },
    OnProjectDiscoveryExperience => "OnProjectDiscoveryExperience" ("charid") {
        fields: { experience: f64 },
        samples: || vec![vec![PyRep::Real(120.0)]],
    },
    OnProjectDiscoveryLevelUp => "OnProjectDiscoveryLevelUp" ("charid") {
        fields: { new_rank: i64, xp_for_new_rank: f64, xp_for_next_rank: f64 },
        samples: || vec![vec![PyRep::Integer(2), PyRep::Real(100.0), PyRep::Real(250.0)]],
    },
    OnRaffleCreatedServer => "OnRaffleCreatedServer" ("charid") {
        fields: { raffle_id: i64, raffle_data: PyRep },
        samples: || vec![vec![
            PyRep::Integer(1),
            PyRep::keyval([("raffleID", PyRep::Integer(1))]),
        ]],
    },
    OnRaffleFinishedServer => "OnRaffleFinishedServer" ("charid") {
        fields: { raffle_id: i64, winning_ticket: Option<PyRep> },
        samples: || vec![vec![PyRep::Integer(1), PyRep::None]],
    },
    OnRaffleUpdatedServer => "OnRaffleUpdatedServer" ("charid") {
        fields: { raffle_id: i64, raffle_data: PyRep },
        samples: || vec![vec![
            PyRep::Integer(1),
            PyRep::keyval([("raffleID", PyRep::Integer(1))]),
        ]],
    },
    OnRefreshBookmarks => "OnRefreshBookmarks" ("charid") {
        samples: || vec![vec![]],
    },
    OnRelinquishStructureControl => "OnRelinquishStructureControl" ("charid") {
        optional: { char_id: Option<i64>, item_id: Option<i64> },
        samples: || vec![
            vec![],
            vec![PyRep::Integer(90_000_001), PyRep::Integer(100_000_001)],
        ],
    },
    OnRemoveCalendarEvent => "OnRemoveCalendarEvent" ("corpid") {
        fields: { event_id: i64, event_date_time: i64, is_deleted: bool },
        samples: || vec![vec![
            PyRep::Integer(1),
            PyRep::Integer(1_335_000_000_000_000_00),
            PyRep::Bool(true),
        ]],
    },
    OnRemoveProbe => "OnRemoveProbe" ("charid") {
        fields: { probe_id: i64 },
        samples: || vec![vec![PyRep::Integer(100_000_001)]],
    },
    OnRespecInfoChanged => "OnRespecInfoChanged" ("charid") {
        samples: || vec![vec![]],
    },
    OnSanctionedActionChanged => "OnSanctionedActionChanged" ("corpid") {
        fields: { corporation_id: i64, vote_case_id: i64 },
        samples: || vec![vec![PyRep::Integer(98_000_001), PyRep::Integer(1)]],
    },
    OnScannerInfoRemoved => "OnScannerInfoRemoved" ("charid") {
        samples: || vec![vec![]],
    },
    OnSetPlayerStanding => "OnSetPlayerStanding" ("charid") {
        fields: { contact_id: i64, standing: f64 },
        samples: || vec![vec![PyRep::Integer(90_000_002), PyRep::Real(5.0)]],
    },
    OnShareChange => "OnShareChange" ("corpid") {
        fields: { shareholder_id: i64, corporation_id: i64, change: PyRep },
        samples: || vec![vec![
            PyRep::Integer(90_000_001),
            PyRep::Integer(98_000_001),
            PyRep::Dict(Vec::new()),
        ]],
    },
    OnSharedBookmarksFolderDeleted => "OnSharedBookmarksFolderDeleted" ("corpid") {
        fields: { folder_id: i64 },
        samples: || vec![vec![PyRep::Integer(1)]],
    },
    OnSharedBookmarksFolderUpdated => "OnSharedBookmarksFolderUpdated" ("corpid") {
        fields: { updates: PyRep },
        samples: || vec![vec![PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Tuple(vec![PyRep::Integer(1)]),
            PyRep::String("FolderChanged".into()),
            PyRep::Tuple(vec![
                PyRep::List(vec![PyRep::keyval([("bookmarkID", PyRep::Integer(1))])]),
                PyRep::List(vec![PyRep::Integer(1)]),
            ]),
        ])])]],
    },
    OnSharedBookmarksFoldersAccessChanged => "OnSharedBookmarksFoldersAccessChanged" ("charid") {
        fields: { access_changed: PyRep },
        samples: || vec![vec![PyRep::Dict(vec![(
            PyRep::Integer(1),
            PyRep::Integer(1),
        )])]],
    },
    OnSharedBookmarksFoldersAccessLost => "OnSharedBookmarksFoldersAccessLost" ("charid") {
        fields: { folder_ids: Vec<i64> },
        samples: || vec![vec![PyRep::List(vec![PyRep::Integer(1)])]],
    },
    OnSignalTrackerAnomalyUpdate => "OnSignalTrackerAnomalyUpdate" ("charid") {
        fields: { solar_system_id: i64, added_anomalies: PyRep, removed_anomalies: PyRep },
        samples: || vec![vec![
            PyRep::Integer(30_000_142),
            PyRep::Dict(Vec::new()),
            PyRep::List(Vec::new()),
        ]],
    },
    OnSignalTrackerSignatureUpdate => "OnSignalTrackerSignatureUpdate" ("charid") {
        fields: { solar_system_id: i64, added_signatures: PyRep, removed_signatures: PyRep },
        samples: || vec![vec![
            PyRep::Integer(30_000_142),
            PyRep::Dict(Vec::new()),
            PyRep::List(Vec::new()),
        ]],
    },
    OnSignalTrackerStructureUpdate => "OnSignalTrackerStructureUpdate" ("charid") {
        fields: { solar_system_id: i64, added_structures: PyRep, removed_structures: PyRep },
        samples: || vec![vec![
            PyRep::Integer(30_000_142),
            PyRep::Dict(Vec::new()),
            PyRep::List(Vec::new()),
        ]],
    },
    OnSkyhookCanBeHacked => "OnSkyhookCanBeHacked" ("solarsystemid") {
        fields: { solar_system_id: i64, skyhook_id: i64 },
        samples: || vec![vec![PyRep::Integer(30_000_142), PyRep::Integer(1_000_000_000_001)]],
    },
    OnSlimItemChange => "OnSlimItemChange" ("solarsystemid") {
        fields: { old_slim: PyRep, new_slim: PyRep },
        samples: || vec![vec![
            PyRep::Integer(1_000_000_000_001),
            PyRep::keyval([
                ("itemID", PyRep::Integer(1_000_000_000_001)),
                ("typeID", PyRep::Integer(35832)),
                ("ownerID", PyRep::Integer(98_000_001)),
                ("corpID", PyRep::Integer(98_000_001)),
                ("allianceID", PyRep::None),
                ("charID", PyRep::None),
                ("categoryID", PyRep::Integer(65)),
                ("groupID", PyRep::Integer(1657)),
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
            ]),
        ]],
    },
    OnSolarSystemDevIndexChanged => "OnSolarSystemDevIndexChanged" ("solarsystemid") {
        fields: { solar_system_id: i64 },
        samples: || vec![vec![PyRep::Integer(30_000_142)]],
    },
    OnSolarSystemSovStructuresUpdated => "OnSolarSystemSovStructuresUpdated" ("solarsystemid") {
        fields: { solar_system_id: i64, solar_system_structures_info: PyRep, changes: PyRep },
        samples: || vec![vec![
            PyRep::Integer(30_000_142),
            PyRep::List(Vec::new()),
            PyRep::Dict(vec![(
                PyRep::Integer(1_000_000_000_001),
                PyRep::Integer(1),
            )]),
        ]],
    },
    OnSovHubHacked => "OnSovHubHacked" ("solarsystemid") {
        fields: { sov_hub_id: i64, upgrades: Vec<PyRep> },
        samples: || vec![vec![
            PyRep::Integer(1_000_000_000_001),
            PyRep::List(vec![PyRep::Integer(81986)]),
        ]],
    },
    OnSovereigntyAudioEvent => "OnSovereigntyAudioEvent" ("solarsystemid") {
        fields: { event_id: i64, text_params: PyRep },
        samples: || vec![vec![PyRep::Integer(1), PyRep::Dict(Vec::new())]],
    },
    OnSovereigntyChanged => "OnSovereigntyChanged" ("solarsystemid") {
        fields: { solar_system_id: i64, alliance_id: Option<i64> },
        samples: || vec![vec![PyRep::Integer(30_000_142), PyRep::Integer(99_000_001)]],
    },
    OnStructureHacked => "OnStructureHacked" ("corpid") {
        fields: { structure_id: i64, reinforce_timing: PyRep },
        samples: || vec![vec![
            PyRep::Integer(1_000_000_000_001),
            PyRep::Tuple(vec![
                PyRep::Integer(5),
                PyRep::Integer(18),
                PyRep::Integer(6),
                PyRep::Integer(20),
                PyRep::Integer(1_700_000_000),
            ]),
        ]],
    },
    OnStructureStateChanged => "OnStructureStateChanged" ("solarsystemid") {
        fields: { solar_system_id: i64, structure_id: i64, state: String },
        samples: || vec![vec![
            PyRep::Integer(30_000_142),
            PyRep::Integer(1_000_000_000_001),
            PyRep::String("Unanchoring".into()),
        ]],
    },
    OnStructureTargetAdded => "OnStructureTargetAdded" ("charid") {
        fields: { sid: i64, tid: i64 },
        samples: || vec![vec![
            PyRep::Integer(1_000_000_000_001),
            PyRep::Integer(100_000_001),
        ]],
    },
    OnSystemFullyCorruptedForFaction => "OnSystemFullyCorruptedForFaction" ("solarsystemid") {
        fields: {
            solar_system_id: i64,
            pirate_faction_id: i64,
            pirate_points: i64,
            points_required: i64,
        },
        samples: || vec![vec![
            PyRep::Integer(30_000_142),
            PyRep::Integer(500_010),
            PyRep::Integer(1000),
            PyRep::Integer(1000),
        ]],
    },
    OnSystemFullySuppressedForFaction => "OnSystemFullySuppressedForFaction" ("solarsystemid") {
        fields: { solar_system_id: i64, antipirate_points: i64, points_required: i64 },
        samples: || vec![vec![
            PyRep::Integer(30_000_142),
            PyRep::Integer(1000),
            PyRep::Integer(1000),
        ]],
    },
    OnSystemScanDone => "OnSystemScanDone" ("charid") {
        samples: || vec![vec![]],
    },
    OnSystemScanStarted => "OnSystemScanStarted" ("charid") {
        fields: { start_time: i64, duration_ms: i64, probes: PyRep },
        samples: || vec![vec![
            PyRep::Integer(1_335_000_000_000_000_00),
            PyRep::Integer(5000),
            PyRep::List(vec![PyRep::Integer(100_000_001)]),
        ]],
    },
    OnSystemScanStopped => "OnSystemScanStopped" ("charid") {
        fields: { probes: Vec<PyRep>, results: Vec<PyRep>, absent_targets: Vec<PyRep> },
        samples: || vec![vec![
            PyRep::List(vec![PyRep::Integer(100_000_001)]),
            PyRep::List(vec![PyRep::keyval([("id", PyRep::String("ABC-123".into()))])]),
            PyRep::List(Vec::new()),
        ]],
    },
    OnTarget => "OnTarget" ("charid") {
        fields: { arg0: PyRep, arg1: PyRep },
        optional: { reason: Option<PyRep> },
        samples: || vec![
            vec![PyRep::String("add".into()), PyRep::Integer(100_000_001)],
            vec![PyRep::String("add".into()), PyRep::Integer(100_000_001), PyRep::None],
        ],
    },
    OnTargetOBO => "OnTargetOBO" ("charid") {
        fields: { what: String, sid: i64, tid: Option<i64>, reason: Option<PyRep> },
        samples: || vec![vec![
            PyRep::String("clear".into()),
            PyRep::Integer(100_000_001),
            PyRep::None,
            PyRep::None,
        ]],
    },
    OnTicketsUpdatedServer => "OnTicketsUpdatedServer" ("charid") {
        fields: { ticket_updates: PyRep },
        samples: || vec![vec![PyRep::Dict(vec![(PyRep::Integer(1), PyRep::Integer(5))])]],
    },
    OnTitleChanged => "OnTitleChanged" ("corpid") {
        fields: { corp_id: i64, title_id: i64, change: PyRep },
        samples: || vec![vec![
            PyRep::Integer(98_000_001),
            PyRep::Integer(1),
            PyRep::Dict(Vec::new()),
        ]],
    },
    OnUnblockContact => "OnUnblockContact" ("charid") {
        fields: { contact_id: i64 },
        samples: || vec![vec![PyRep::Integer(90_000_002)]],
    },
    OnVoteCast => "OnVoteCast" ("corpid") {
        fields: { corporation_id: i64, vote_case_id: i64 },
        samples: || vec![vec![PyRep::Integer(98_000_001), PyRep::Integer(1)]],
    },
    OnWarChanged => "OnWarChanged" ("corpid") {
        fields: { war: PyRep, owner_ids: PyRep, change: PyRep },
        samples: || vec![vec![
            PyRep::keyval([("warID", PyRep::Integer(1)), ("warHQ", PyRep::None)]),
            PyRep::List(vec![PyRep::Integer(98_000_001), PyRep::Integer(98_000_002)]),
            PyRep::Dict(Vec::new()),
        ]],
    },
    ProcessSovStatusChanged => "ProcessSovStatusChanged" ("solarsystemid") {
        fields: { solar_system_id: i64, sov_info: PyRep },
        samples: || vec![vec![
            PyRep::Integer(30_000_142),
            PyRep::keyval([
                ("solarSystemID", PyRep::Integer(30_000_142)),
                ("allianceID", PyRep::Integer(99_000_001)),
            ]),
        ]],
    },
    // Batch 3 (#932) registrations
    DoDestinyUpdate => "DoDestinyUpdate" ("clientID") {
        fields: { state: PyRep, wait_for_bubble: PyRep },
        optional: { dogma_messages: Option<PyRep>, extra: Option<PyRep> },
        samples: || vec![vec![
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(1),
                PyRep::Tuple(vec![
                    PyRep::String("SetMaxSpeed".into()),
                    PyRep::Tuple(vec![PyRep::Integer(100_000_001), PyRep::Real(1.0)]),
                ]),
            ])]),
            PyRep::Bool(false),
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::String("OnEffectMessage".into()),
                PyRep::Tuple(Vec::new()),
            ])]),
        ]],
    },
    OnAbyssalContentFinished => "OnAbyssalContentFinished" ("charid") {
        optional: { content_id: Option<i64>, timer_type: Option<i64> },
        samples: || vec![vec![PyRep::Integer(1), PyRep::Integer(1)]],
    },
    OnAbyssalLocationExited => "OnAbyssalLocationExited" ("charid") {
        samples: || vec![vec![]],
    },
    OnAddExpiryDurationForVoidSpaceContent => "OnAddExpiryDurationForVoidSpaceContent" ("charid") {
        fields: { end_time: i64 },
        samples: || vec![vec![PyRep::Integer(133_500_000_000_000_000)]],
    },
    OnCharacterEnteredStructure => "OnCharacterEnteredStructure" ("stationid") {
        fields: {
            character_id: i64,
            corporation_id: i64,
            alliance_id: Option<i64>,
            faction_id: Option<i64>,
        },
        samples: || vec![vec![
            PyRep::Integer(90_000_001),
            PyRep::Integer(98_000_001),
            PyRep::None,
            PyRep::None,
        ]],
    },
    OnCharacterLeftStructure => "OnCharacterLeftStructure" ("stationid") {
        fields: { character_id: i64 },
        samples: || vec![vec![PyRep::Integer(90_000_001)]],
    },
    OnClientNodeGraphMessage => "OnClientNodeGraphMessage" ("charid") {
        fields: { node_graph_instance_id: i64, arg1: PyRep, arg2: PyRep },
        optional: { arg3: Option<PyRep> },
        samples: || vec![vec![
            PyRep::Integer(1),
            PyRep::String("msg".into()),
            PyRep::Integer(1),
        ]],
    },
    OnClientNodeGraphUpdate => "OnClientNodeGraphUpdate" ("charid") {
        fields: { node_graph_instance_id: i64, arg1: PyRep },
        optional: { arg2: Option<PyRep> },
        samples: || vec![vec![
            PyRep::Integer(1),
            PyRep::Dict(vec![(PyRep::String("progress".into()), PyRep::Integer(1))]),
        ]],
    },
    OnCorpAllowedEnlistmentFactionsSet => "OnCorpAllowedEnlistmentFactionsSet" ("corpid") {
        optional: { factions: Option<Vec<i64>> },
        samples: || vec![vec![]],
    },
    OnCreateNewMailWindow => "OnCreateNewMailWindow" ("charid") {
        fields: {
            recipients: Vec<i64>,
            corp_or_alliance_recipient: PyRep,
            to_list_id: PyRep,
            subject: String,
            body: String,
        },
        samples: || vec![vec![
            PyRep::List(vec![PyRep::Integer(90_000_002)]),
            PyRep::None,
            PyRep::None,
            PyRep::String("Hello".into()),
            PyRep::String("Body".into()),
        ]],
    },
    OnDockingAccepted => "OnDockingAccepted" ("charid") {
        fields: { item_id: i64 },
        samples: || vec![vec![PyRep::Integer(60_003_760)]],
    },
    OnDockingFinished => "OnDockingFinished" ("charid") {
        fields: { item_id: i64 },
        samples: || vec![vec![PyRep::Integer(60_003_760)]],
    },
    OnDungeonCompleted => "OnDungeonCompleted" ("charid") {
        fields: { dungeon_id: i64 },
        samples: || vec![vec![PyRep::Integer(1)]],
    },
    OnEnlistmentCooldownCleared => "OnEnlistmentCooldownCleared" ("charid") {
        samples: || vec![vec![]],
    },
    OnEnlistmentStateUpdated => "OnEnlistmentStateUpdated" ("charid") {
        fields: { faction_id: PyRep, direct_faction_id: PyRep, corp_faction_id: PyRep },
        samples: || vec![vec![
            PyRep::Integer(500_001),
            PyRep::Integer(500_001),
            PyRep::None,
        ]],
    },
    OnEscalatingPathChange => "OnEscalatingPathChange" ("charid") {
        samples: || vec![vec![]],
    },
    OnEscalatingPathMessage => "OnEscalatingPathMessage" ("charid") {
        fields: { instance_id: i64 },
        samples: || vec![vec![PyRep::Integer(1)]],
    },
    OnEwarStart => "OnEwarStart" ("charid") {
        fields: {
            source_ball_id: i64,
            module_id: i64,
            target_ball_id: i64,
            jamming_type: String,
        },
        samples: || vec![vec![
            PyRep::Integer(100_000_001),
            PyRep::Integer(100_000_010),
            PyRep::Integer(100_000_002),
            PyRep::String("electronic".into()),
        ]],
    },
    OnFacWarBunkerNonVulnerable => "OnFacWarBunkerNonVulnerable" ("solarsystemid") {
        fields: { attacker_id: i64 },
        samples: || vec![vec![PyRep::Integer(500_001)]],
    },
    OnFacWarBunkerVulnerable => "OnFacWarBunkerVulnerable" ("solarsystemid") {
        fields: { attacker_id: i64 },
        samples: || vec![vec![PyRep::Integer(500_001)]],
    },
    OnFacWarDungeonCompleted => "OnFacWarDungeonCompleted" ("solarsystemid") {
        fields: { was_conquered: bool, faction_id: i64 },
        samples: || vec![vec![PyRep::Bool(true), PyRep::Integer(500_001)]],
    },
    OnFittingAdded => "OnFittingAdded" ("charid") {
        fields: { owner_id: i64, fit_id: PyRep },
        samples: || vec![vec![
            PyRep::Integer(90_000_001),
            PyRep::List(vec![PyRep::Integer(1)]),
        ]],
    },
    OnFittingDeleted => "OnFittingDeleted" ("charid") {
        fields: { owner_id: i64, fit_id: i64 },
        samples: || vec![vec![PyRep::Integer(90_000_001), PyRep::Integer(1)]],
    },
    OnFittingsUpdated => "OnFittingsUpdated" ("charid") {
        fields: { fitting: PyRep },
        samples: || vec![vec![PyRep::keyval([
            ("shipTypeID", PyRep::Integer(587)),
            (
                "fitData",
                PyRep::List(vec![PyRep::Tuple(vec![
                    PyRep::Integer(3170),
                    PyRep::Integer(12),
                    PyRep::Integer(1),
                ])]),
            ),
        ])]],
    },
    OnFleetDisbanded => "OnFleetDisbanded" ("charid") {
        fields: { char_ids: Vec<i64> },
        samples: || vec![vec![PyRep::List(vec![PyRep::Integer(90_000_001)])]],
    },
    OnFleetInvite => "OnFleetInvite" ("charid") {
        fields: { fleet_id: i64, invite_id: i64, msg_name: PyRep, msg_dict: PyRep },
        optional: { role_num: Option<i64> },
        samples: || vec![vec![
            PyRep::Integer(1),
            PyRep::Integer(90_000_001),
            PyRep::String("FleetInvite".into()),
            PyRep::Dict(vec![(PyRep::String("autoAccept".into()), PyRep::Bool(false))]),
        ]],
    },
    OnFleetJoinReject => "OnFleetJoinReject" ("charid") {
        fields: { member_id: i64, reason_code: PyRep },
        samples: || vec![vec![PyRep::Integer(90_000_002), PyRep::Integer(1)]],
    },
    OnFleetMove => "OnFleetMove" ("charid") {
        samples: || vec![vec![]],
    },
    OnFleetMoveFailed => "OnFleetMoveFailed" ("charid") {
        fields: { char_id: i64, is_kicked: bool },
        samples: || vec![vec![PyRep::Integer(90_000_002), PyRep::Bool(false)]],
    },
    OnFleetSquadDeleted => "OnFleetSquadDeleted" ("fleetid") {
        fields: { squad_id: i64 },
        samples: || vec![vec![PyRep::Integer(1)]],
    },
    OnFleetSquadNameChanged => "OnFleetSquadNameChanged" ("fleetid") {
        fields: { squad_id: i64, name: String },
        samples: || vec![vec![PyRep::Integer(1), PyRep::String("Squad 1".into())]],
    },
    OnFleetWingDeleted => "OnFleetWingDeleted" ("fleetid") {
        fields: { wing_id: i64 },
        samples: || vec![vec![PyRep::Integer(1)]],
    },
    OnFleetWingNameChanged => "OnFleetWingNameChanged" ("fleetid") {
        fields: { wing_id: i64, name: String },
        samples: || vec![vec![PyRep::Integer(1), PyRep::String("Wing 1".into())]],
    },
    OnFreeSkillPointsChanged => "OnFreeSkillPointsChanged" ("charid") {
        fields: { new_free_skill_points: f64 },
        samples: || vec![vec![PyRep::Real(50_000.0)]],
    },
    OnFwObjectiveCompletedInSolarSystem => "OnFwObjectiveCompletedInSolarSystem" ("solarsystemid") {
        fields: { solar_system_id: i64, target_faction_id: i64, objective_mode: i64 },
        samples: || vec![vec![
            PyRep::Integer(30_000_142),
            PyRep::Integer(500_001),
            PyRep::Integer(1),
        ]],
    },
    OnFwScoreboardWon => "OnFwScoreboardWon" ("solarsystemid") {
        fields: { solar_system_id: i64, scoreboard_item_id: i64, winner_faction_id: i64 },
        samples: || vec![vec![
            PyRep::Integer(30_000_142),
            PyRep::Integer(1),
            PyRep::Integer(500_001),
        ]],
    },
    OnHackingStart => "OnHackingStart" ("charid") {
        fields: { event_data: PyRep },
        samples: || vec![vec![PyRep::Dict(vec![(
            PyRep::String("moduleTypeID".into()),
            PyRep::Integer(22_175),
        )])]],
    },
    OnHackingVirusChanged => "OnHackingVirusChanged" ("charid") {
        fields: { object_data: PyRep, event_id: i64 },
        samples: || vec![vec![
            PyRep::keyval([("hp", PyRep::Integer(50))]),
            PyRep::Integer(1),
        ]],
    },
    OnHangarViewStateUpdated => "OnHangarViewStateUpdated" ("charid") {
        fields: { structure_id: i64, hangar_view_state: PyRep },
        samples: || vec![vec![
            PyRep::Integer(1_000_000_000_001),
            PyRep::Integer(1),
        ]],
    },
    OnInvulunOnUndockingUpdated => "OnInvulunOnUndockingUpdated" ("charid") {
        fields: { ship_id: i64, end_time: i64, duration: PyRep },
        samples: || vec![vec![
            PyRep::Integer(100_000_001),
            PyRep::Integer(133_500_000_000_000_000),
            PyRep::Integer(30),
        ]],
    },
    OnItemsChanged => "OnItemsChanged" ("charid") {
        fields: { items: PyRep, change: PyRep, location: PyRep },
        samples: || vec![vec![
            PyRep::List(vec![sample_keyval_item()]),
            PyRep::Dict(Vec::new()),
            PyRep::Integer(60_003_760),
        ]],
    },
    OnJumpCloneCacheInvalidated => "OnJumpCloneCacheInvalidated" ("charid") {
        samples: || vec![vec![]],
    },
    OnJumpCloneTransitionCompleted => "OnJumpCloneTransitionCompleted" ("charid") {
        samples: || vec![vec![]],
    },
    OnMailCountersUpdate => "OnMailCountersUpdate" ("charid") {
        optional: { char_id: Option<i64> },
        samples: || vec![vec![]],
    },
    OnMailRestored => "OnMailRestored" ("charid") {
        fields: { message_ids: Vec<i64> },
        samples: || vec![vec![PyRep::List(vec![PyRep::Integer(1)])]],
    },
    OnMailUndeleted => "OnMailUndeleted" ("charid") {
        fields: { message_ids: Vec<i64> },
        samples: || vec![vec![PyRep::List(vec![PyRep::Integer(1)])]],
    },
    OnManyFittingsDeleted => "OnManyFittingsDeleted" ("charid") {
        fields: { owner_id: i64, fit_ids: Vec<i64> },
        samples: || vec![vec![
            PyRep::Integer(90_000_001),
            PyRep::List(vec![PyRep::Integer(1)]),
        ]],
    },
    OnMarketItemReceived => "OnMarketItemReceived" ("charid") {
        fields: { item_id: i64, type_id: i64, flag: i64, location_id: i64 },
        samples: || vec![vec![
            PyRep::Integer(100_000_001),
            PyRep::Integer(34),
            PyRep::Integer(4),
            PyRep::Integer(60_003_760),
        ]],
    },
    OnMemberlessFleetUnregistered => "OnMemberlessFleetUnregistered" ("charid") {
        samples: || vec![vec![]],
    },
    OnMercenaryDenRegistrationInvalid => "OnMercenaryDenRegistrationInvalid" ("charid") {
        fields: { notify_msg: String, notify_args: PyRep },
        samples: || vec![vec![
            PyRep::String("MercenaryDenInvalid".into()),
            PyRep::Dict(Vec::new()),
        ]],
    },
    OnMercenaryDenRegistrationSuccess => "OnMercenaryDenRegistrationSuccess" ("charid") {
        fields: { notify_msg: String, notify_args: PyRep },
        samples: || vec![vec![
            PyRep::String("MercenaryDenRegistered".into()),
            PyRep::Dict(vec![(
                PyRep::String("itemID".into()),
                PyRep::Integer(100_000_001),
            )]),
        ]],
    },
    OnModuleFitted => "OnModuleFitted" ("charid") {
        fields: { char_id: i64, flag: i64, type_id: i64, group_id: i64 },
        samples: || vec![vec![
            PyRep::Integer(90_000_001),
            PyRep::Integer(27),
            PyRep::Integer(3170),
            PyRep::Integer(53),
        ]],
    },
    OnModuleUnfitted => "OnModuleUnfitted" ("charid") {
        fields: { arg0: PyRep },
        optional: { arg1: Option<PyRep> },
        samples: || vec![vec![PyRep::Integer(3170)]],
    },
    OnMyMercenaryDenRemoved => "OnMyMercenaryDenRemoved" ("charid") {
        fields: { notify_msg: String, notify_args: PyRep },
        samples: || vec![vec![
            PyRep::String("MercenaryDenRemoved".into()),
            PyRep::Dict(vec![(
                PyRep::String("itemID".into()),
                PyRep::Integer(100_000_001),
            )]),
        ]],
    },
    OnNotificationDeleted => "OnNotificationDeleted" ("charid") {
        fields: { notification_ids: Vec<i64> },
        samples: || vec![vec![PyRep::List(vec![PyRep::Integer(1)])]],
    },
    OnOwnOrdersChanged => "OnOwnOrdersChanged" ("charid") {
        fields: { orders: PyRep, reason: PyRep, is_corp: PyRep },
        samples: || vec![vec![
            PyRep::List(vec![PyRep::keyval([
                ("orderID", PyRep::Integer(1)),
                ("typeID", PyRep::Integer(34)),
            ])]),
            PyRep::String("Add".into()),
            PyRep::Bool(false),
        ]],
    },
    OnPVPFilamentsCharacterStatistics => "OnPVPFilamentsCharacterStatistics" ("charid") {
        fields: { info: PyRep },
        samples: || vec![vec![PyRep::Dict(vec![(
            PyRep::String("statistics".into()),
            PyRep::Dict(vec![(PyRep::String("rank".into()), PyRep::Integer(1))]),
        )])]],
    },
    OnPVPFilamentsLeaderboard => "OnPVPFilamentsLeaderboard" ("charid") {
        fields: { info: PyRep },
        samples: || vec![vec![PyRep::Dict(vec![(
            PyRep::String("entries".into()),
            PyRep::List(vec![PyRep::Dict(vec![
                (PyRep::String("character_id".into()), PyRep::Integer(90_000_001)),
                (PyRep::String("rank".into()), PyRep::Integer(1)),
            ])]),
        )])]],
    },
    OnServerSkillsChanged => "OnServerSkillsChanged" ("charid") {
        fields: { skill_infos: PyRep, event: PyRep, timestamp: i64 },
        samples: || vec![vec![
            PyRep::Dict(vec![(
                PyRep::Integer(3402),
                PyRep::keyval([
                    ("skillPoints", PyRep::Integer(250)),
                    ("skillLevel", PyRep::Integer(1)),
                ]),
            )]),
            PyRep::String("OnSkillTrained".into()),
            PyRep::Integer(133_500_000_000_000_000),
        ]],
    },
    OnServerSkillsRemoved => "OnServerSkillsRemoved" ("charid") {
        fields: { skill_infos: PyRep, timestamp: i64 },
        samples: || vec![vec![
            PyRep::Dict(vec![(PyRep::Integer(3402), PyRep::None)]),
            PyRep::Integer(133_500_000_000_000_000),
        ]],
    },
    OnShipJumpCloneCacheInvalidated => "OnShipJumpCloneCacheInvalidated" ("charid") {
        fields: { location_id: i64, char_id: i64 },
        samples: || vec![vec![
            PyRep::Integer(100_000_001),
            PyRep::Integer(90_000_001),
        ]],
    },
    OnShipJumpCloneInstallationCanceled => "OnShipJumpCloneInstallationCanceled" ("charid") {
        optional: { args: Option<PyRep> },
        samples: || vec![vec![PyRep::Tuple(Vec::new())]],
    },
    OnShipJumpCloneInstallationDone => "OnShipJumpCloneInstallationDone" ("charid") {
        fields: { arg0: PyRep },
        optional: { arg1: Option<PyRep>, arg2: Option<PyRep>, arg3: Option<PyRep> },
        samples: || vec![vec![PyRep::Tuple(vec![
            PyRep::Integer(90_000_001),
            PyRep::Integer(90_000_002),
            PyRep::Integer(100_000_001),
            PyRep::Bool(true),
        ])]],
    },
    OnShipJumpCloneInstallationOffered => "OnShipJumpCloneInstallationOffered" ("charid") {
        fields: { arg0: PyRep },
        optional: { arg1: Option<PyRep>, arg2: Option<PyRep>, arg3: Option<PyRep> },
        samples: || vec![vec![PyRep::Tuple(vec![
            PyRep::Integer(90_000_001),
            PyRep::Integer(90_000_002),
            PyRep::Integer(100_000_001),
            PyRep::Bool(true),
        ])]],
    },
    OnShowContract => "OnShowContract" ("charid") {
        fields: { contract_id: i64 },
        samples: || vec![vec![PyRep::Integer(1)]],
    },
    OnShowMarketDetails => "OnShowMarketDetails" ("charid") {
        fields: { type_id: i64 },
        samples: || vec![vec![PyRep::Integer(34)]],
    },
    OnShowOwnerDetailsWindow => "OnShowOwnerDetailsWindow" ("charid") {
        fields: { target_id: i64 },
        samples: || vec![vec![PyRep::Integer(90_000_001)]],
    },
    OnSolarsystemAdvantageStateUpdatedLocal => "OnSolarsystemAdvantageStateUpdated_Local" ("solarsystemid") {
        samples: || vec![vec![]],
    },
    OnSolarsystemInterferenceChanged => "OnSolarsystemInterferenceChanged" ("solarsystemid") {
        fields: { solar_system_id: i64, new_interference_state: PyRep },
        samples: || vec![vec![
            PyRep::Integer(30_000_142),
            PyRep::keyval([("interferenceLevel", PyRep::Real(0.5))]),
        ]],
    },
    OnStationJumpCloneCacheInvalidated => "OnStationJumpCloneCacheInvalidated" ("charid") {
        fields: { location_id: i64, char_id: i64 },
        samples: || vec![vec![
            PyRep::Integer(60_003_760),
            PyRep::Integer(90_000_001),
        ]],
    },
    OnSystemWideEffectStart => "OnSystemWideEffectStart" ("solarsystemid2") {
        fields: { effect_id: i64, source_item_id: i64, source_type_id: i64 },
        samples: || vec![vec![
            PyRep::Integer(1),
            PyRep::Integer(100_000_001),
            PyRep::Integer(32_207),
        ]],
    },
    OnTradeCancel => "OnTradeCancel" ("charid") {
        fields: { container_id: i64 },
        samples: || vec![vec![PyRep::Integer(1)]],
    },
    OnTradeComplete => "OnTradeComplete" ("charid") {
        fields: { container_id: i64 },
        optional: { items: Option<PyRep> },
        samples: || vec![vec![
            PyRep::Integer(1),
            PyRep::List(vec![sample_keyval_item()]),
        ]],
    },
    OnTradeInitiate => "OnTradeInitiate" ("charid") {
        fields: { char_id: i64, trade_session: PyRep, trade_items: PyRep },
        samples: || vec![vec![
            PyRep::Integer(90_000_001),
            PyRep::keyval([(
                "List",
                PyRep::keyval([("tradeContainerID", PyRep::Integer(1))]),
            )]),
            PyRep::List(vec![sample_keyval_item()]),
        ]],
    },
    OnTradeMoneyOffer => "OnTradeMoneyOffer" ("charid") {
        fields: { container_id: i64, arg1: PyRep },
        optional: { arg2: Option<PyRep> },
        samples: || vec![vec![PyRep::Integer(1), PyRep::Real(5_000.0)]],
    },
    OnTradeOffer => "OnTradeOffer" ("charid") {
        fields: { container_id: i64, arg1: PyRep },
        optional: { arg2: Option<PyRep> },
        samples: || vec![vec![PyRep::Integer(1), PyRep::Bool(true)]],
    },
    OnTradeOfferReset => "OnTradeOfferReset" ("charid") {
        fields: { container_id: i64 },
        optional: { manifest_error: Option<bool> },
        samples: || vec![vec![PyRep::Integer(1), PyRep::Bool(false)]],
    },
    OnVoidSpaceContentFinished => "OnVoidSpaceContentFinished" ("charid") {
        fields: { content_id: i64, timer_type: i64 },
        samples: || vec![vec![PyRep::Integer(1), PyRep::Integer(1)]],
    },
    OnVoidSpaceEntered => "OnVoidSpaceEntered" ("charid") {
        fields: { client_data: PyRep },
        samples: || vec![vec![PyRep::Dict(vec![
            (PyRep::String("content_id".into()), PyRep::Integer(1)),
            (
                PyRep::String("nebula_graphic_id".into()),
                PyRep::Integer(20_001),
            ),
        ])]],
    },
    OnVoteCaseChanged => "OnVoteCaseChanged" ("corpid") {
        fields: { corporation_id: i64, vote_case_id: i64 },
        samples: || vec![vec![PyRep::Integer(98_000_001), PyRep::Integer(1)]],
    },
    OnWarzoneOccupationStateUpdated => "OnWarzoneOccupationStateUpdated" ("solarsystemid") {
        fields: { solar_system_id: i64, new_occupation_state: PyRep },
        samples: || vec![vec![
            PyRep::Integer(30_000_142),
            PyRep::keyval([
                ("adjacencyState", PyRep::Integer(1)),
                ("occupierID", PyRep::Integer(500_001)),
            ]),
        ]],
    },
    OnWormholeJumpCancel => "OnWormholeJumpCancel" ("charid") {
        samples: || vec![vec![]],
    },
    ProcessInventoryChange => "ProcessInventoryChange" ("charid") {
        fields: { arg0: PyRep, arg1: PyRep, arg2: PyRep },
        optional: { arg3: Option<PyRep> },
        samples: || vec![vec![
            PyRep::List(vec![sample_keyval_item()]),
            PyRep::Dict(vec![(
                PyRep::Integer(1),
                PyRep::Tuple(vec![PyRep::Integer(0), PyRep::Integer(1)]),
            )]),
            PyRep::Bool(false),
            PyRep::None,
        ]],
    },
    // Batch 4 (#932) registrations
    DoDestinyUpdates => "DoDestinyUpdates" ("charid") {
        fields: { updates: Vec<PyRep> },
        samples: || vec![vec![PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(1),
                PyRep::Tuple(vec![
                    PyRep::String("SetMaxSpeed".into()),
                    PyRep::Tuple(vec![PyRep::Integer(100_000_001), PyRep::Real(1.0)]),
                ]),
            ])]),
            PyRep::Bool(false),
        ])])]],
    },
    OnAbyssalLocationEntered => "OnAbyssalLocationEntered" ("charid") {
        fields: { client_data: PyRep },
        samples: || vec![vec![PyRep::Dict(vec![
            (PyRep::String("content_id".into()), PyRep::Integer(1)),
            (PyRep::String("nebula_graphic_id".into()), PyRep::Integer(22_042)),
            (PyRep::String("difficulty_tier".into()), PyRep::Integer(1)),
            (PyRep::String("location_id".into()), PyRep::Integer(1)),
            (PyRep::String("rooms_completed".into()), PyRep::Integer(0)),
        ])]],
    },
    OnAddExpiryDurationForAbyssalContent => "OnAddExpiryDurationForAbyssalContent" ("charid") {
        fields: { end_time: i64 },
        samples: || vec![vec![PyRep::Integer(133_500_000_000_000_000)]],
    },
    OnAddExpiryDurationForAbyssalPvpContent => "OnAddExpiryDurationForAbyssalPvpContent" ("charid") {
        fields: { end_time: i64 },
        samples: || vec![vec![PyRep::Integer(133_500_000_000_000_000)]],
    },
    OnAgentMissionChange => "OnAgentMissionChange" ("charid") {
        fields: { action: String, agent_id: i64 },
        samples: || vec![vec![PyRep::String("offered".into()), PyRep::Integer(3_018_600)]],
    },
    OnAgentMissionObjectivesCompleted => "OnAgentMissionObjectivesCompleted" ("charid") {
        fields: { agent_id: i64 },
        samples: || vec![vec![PyRep::Integer(3_018_600)]],
    },
    OnAgentMissionObjectivesFailed => "OnAgentMissionObjectivesFailed" ("charid") {
        fields: { agent_id: i64 },
        samples: || vec![vec![PyRep::Integer(3_018_600)]],
    },
    OnBountyPlaced => "OnBountyPlaced" ("charid") {
        fields: { owner_id: i64 },
        optional: { target_id: Option<i64>, amount_isk: Option<f64> },
        samples: || vec![vec![PyRep::Integer(90_000_002)]],
    },
    OnBridgeModeChange => "OnBridgeModeChange" ("fleetid") {
        fields: { ship_id: i64, solar_system_id: i64, item_id: i64, active: bool },
        samples: || vec![vec![
            PyRep::Integer(100_000_010),
            PyRep::Integer(30_000_142),
            PyRep::Integer(100_000_020),
            PyRep::Bool(true),
        ]],
    },
    OnCaptureChanged => "OnCaptureChanged" ("solarsystemid") {
        fields: {
            ball_id: i64,
            capture_id: i64,
            last_incident: i64,
            points: f64,
            capture_time: f64,
            last_capturing: i64,
        },
        samples: || vec![vec![
            PyRep::Integer(100_000_050),
            PyRep::Integer(500_001),
            PyRep::Integer(1_335_000_000_000_000_00),
            PyRep::Real(50.0),
            PyRep::Real(600_000.0),
            PyRep::Integer(500_001),
        ]],
    },
    OnClientContactChange => "OnClientContactChange" ("charid") {
        fields: { char_id: i64, online: bool },
        samples: || vec![vec![PyRep::Integer(90_000_002), PyRep::Bool(true)]],
    },
    OnCloneDestruction => "OnCloneDestruction" ("charid") {
        samples: || vec![vec![]],
    },
    OnContactLoggedOff => "OnContactLoggedOff" ("charid") {
        fields: { char_id: i64 },
        samples: || vec![vec![PyRep::Integer(90_000_002)]],
    },
    OnContactLoggedOn => "OnContactLoggedOn" ("charid") {
        fields: { char_id: i64 },
        samples: || vec![vec![PyRep::Integer(90_000_002)]],
    },
    OnCorporationMedalAdded => "OnCorporationMedalAdded" ("corpid") {
        fields: { medal_id: i64, entity_id: i64 },
        samples: || vec![vec![PyRep::Integer(1), PyRep::Integer(98_000_001)]],
    },
    OnCosmicSignatureAdded => "OnCosmicSignatureAdded" ("solarsystemid") {
        fields: { target_id: String },
        samples: || vec![vec![PyRep::String("XYZ-123".into())]],
    },
    OnCurrentShipSkinChange => "OnCurrentShipSkinChange" ("charid") {
        fields: { ship_id: i64, skin_id: Option<i64> },
        samples: || vec![vec![PyRep::Integer(100_000_010), PyRep::Integer(123)]],
    },
    OnDistributionDungeonEntered => "OnDistributionDungeonEntered" ("charid") {
        fields: { row: PyRep },
        samples: || vec![vec![PyRep::Object {
            type_name: "util.Row".into(),
            args: Box::new(PyRep::Dict(vec![
                (
                    PyRep::String("header".into()),
                    PyRep::List(vec![
                        PyRep::String("instanceID".into()),
                        PyRep::String("dungeonID".into()),
                        PyRep::String("dungeonName".into()),
                        PyRep::String("dungeonNameID".into()),
                        PyRep::String("solarSystemID".into()),
                        PyRep::String("expiryTime".into()),
                    ]),
                ),
                (
                    PyRep::String("line".into()),
                    PyRep::List(vec![
                        PyRep::Integer(1),
                        PyRep::Integer(100),
                        PyRep::String("Dungeon".into()),
                        PyRep::None,
                        PyRep::Integer(30_000_142),
                        PyRep::Integer(1_335_000_000_000_000_00),
                    ]),
                ),
            ])),
        }]],
    },
    OnDroneActivityChange => "OnDroneActivityChange" ("charid") {
        fields: { drone_id: i64, activity_id: i64, activity: Option<String> },
        samples: || vec![vec![
            PyRep::Integer(100_000_020),
            PyRep::Integer(1),
            PyRep::String("attack".into()),
        ]],
    },
    OnDroneStateChange => "OnDroneStateChange" ("charid") {
        fields: {
            drone_id: i64,
            owner_id: i64,
            controller_id: i64,
            activity_state: i64,
            drone_type_id: i64,
            controller_owner_id: i64,
            target_id: Option<i64>,
        },
        samples: || vec![vec![
            PyRep::Integer(100_000_020),
            PyRep::Integer(90_000_001),
            PyRep::Integer(100_000_010),
            PyRep::Integer(0),
            PyRep::Integer(2454),
            PyRep::Integer(90_000_001),
            PyRep::None,
        ]],
    },
    OnDungeonEdit => "OnDungeonEdit" ("charid") {
        fields: { dungeon_id: i64, room_id: i64, room_pos: PyRep },
        samples: || vec![vec![
            PyRep::Integer(1),
            PyRep::Integer(2),
            PyRep::Tuple(vec![PyRep::Real(0.0), PyRep::Real(0.0), PyRep::Real(0.0)]),
        ]],
    },
    OnDungeonValues => "OnDungeonValues" ("charid") {
        fields: { dungeon_id: i64, values: PyRep },
        samples: || vec![vec![
            PyRep::Integer(1),
            PyRep::Dict(vec![(PyRep::String("currentWave".into()), PyRep::Integer(1))]),
        ]],
    },
    OnEditCalendarEvent => "OnEditCalendarEvent" ("charid") {
        fields: {
            event_id: i64,
            owner_id: i64,
            old_event_date_time: i64,
            event_date_time: i64,
            event_duration: i64,
            event_title: String,
            importance: i64,
            auto_event_type: Option<i64>,
        },
        samples: || vec![vec![
            PyRep::Integer(1),
            PyRep::Integer(90_000_001),
            PyRep::Integer(1_335_000_000_000_000_00),
            PyRep::Integer(1_335_000_001_000_000_00),
            PyRep::Integer(60),
            PyRep::String("Fleet Op".into()),
            PyRep::Integer(1),
            PyRep::None,
        ]],
    },
    OnEnteringDungeonRoom => "OnEnteringDungeonRoom" ("charid") {
        fields: {
            dungeon_id: i64,
            room_id: i64,
            room_position: PyRep,
            instance_id: i64,
            dungeon_values: PyRep,
        },
        samples: || vec![vec![
            PyRep::Integer(1),
            PyRep::Integer(2),
            PyRep::Tuple(vec![PyRep::Real(0.0), PyRep::Real(0.0), PyRep::Real(0.0)]),
            PyRep::Integer(100),
            PyRep::Dict(vec![(PyRep::String("currentWave".into()), PyRep::Integer(1))]),
        ]],
    },
    OnEscalatingPathDungeonEntered => "OnEscalatingPathDungeonEntered" ("charid") {
        fields: { row: PyRep },
        samples: || vec![vec![PyRep::Object {
            type_name: "util.Row".into(),
            args: Box::new(PyRep::Dict(vec![
                (
                    PyRep::String("header".into()),
                    PyRep::List(vec![
                        PyRep::String("instanceID".into()),
                        PyRep::String("dungeonID".into()),
                        PyRep::String("dungeonName".into()),
                        PyRep::String("dungeonNameID".into()),
                        PyRep::String("solarSystemID".into()),
                        PyRep::String("expiryTime".into()),
                    ]),
                ),
                (
                    PyRep::String("line".into()),
                    PyRep::List(vec![
                        PyRep::Integer(1),
                        PyRep::Integer(100),
                        PyRep::String("Expedition".into()),
                        PyRep::None,
                        PyRep::Integer(30_000_142),
                        PyRep::Integer(1_335_000_000_000_000_00),
                    ]),
                ),
            ])),
        }]],
    },
    OnEssReserveBankInventoryFull => "OnEssReserveBankInventoryFull" ("solarsystemid") {
        samples: || vec![vec![]],
    },
    OnEwarEnd => "OnEwarEnd" ("charid") {
        fields: {
            source_ball_id: i64,
            module_id: i64,
            target_ball_id: i64,
            jamming_type: String,
        },
        samples: || vec![vec![
            PyRep::Integer(100_000_010),
            PyRep::Integer(100_000_020),
            PyRep::Integer(100_000_030),
            PyRep::String("electronic".into()),
        ]],
    },
    OnExitingDungeon => "OnExitingDungeon" ("charid") {
        fields: { dungeon_id: i64 },
        samples: || vec![vec![PyRep::Integer(1)]],
    },
    OnExpertSystemsUpdated => "OnExpertSystemsUpdated" ("charid") {
        fields: {
            expert_systems: PyRep,
            expert_system_added: bool,
            expert_system_type_id: i64,
        },
        samples: || vec![vec![
            PyRep::Dict(Vec::new()),
            PyRep::Bool(true),
            PyRep::Integer(57_800),
        ]],
    },
    OnFighterAbilitySlotActivated => "OnFighterAbilitySlotActivated" ("charid") {
        fields: {
            fighter_id: i64,
            ability_slot_id: i64,
            remaining_charge_count: Option<i64>,
            start_time: i64,
            duration_ms: f64,
            cooldown: Option<f64>,
        },
        samples: || vec![vec![
            PyRep::Integer(100_000_020),
            PyRep::Integer(0),
            PyRep::Integer(5),
            PyRep::Integer(1_335_000_000_000_000_00),
            PyRep::Real(10_000.0),
            PyRep::Real(5.0),
        ]],
    },
    OnFighterAbilitySlotDeactivated => "OnFighterAbilitySlotDeactivated" ("charid") {
        fields: {
            fighter_id: i64,
            ability_slot_id: i64,
            failure_reason: Option<PyRep>,
        },
        samples: || vec![vec![
            PyRep::Integer(100_000_020),
            PyRep::Integer(0),
            PyRep::None,
        ]],
    },
    OnFleetBoosted => "OnFleetBoosted" ("charid") {
        fields: { module_type_id: i64, num_boosted: i64 },
        samples: || vec![vec![PyRep::Integer(42_530), PyRep::Integer(3)]],
    },
    OnFleetBroadcast => "OnFleetBroadcast" ("charid") {
        fields: {
            name: String,
            scope: i64,
            char_id: i64,
            solar_system_id: i64,
            item_id: Option<i64>,
            type_id: Option<i64>,
        },
        samples: || vec![vec![
            PyRep::String("Target".into()),
            PyRep::Integer(0),
            PyRep::Integer(90_000_001),
            PyRep::Integer(30_000_142),
            PyRep::Integer(100_000_050),
            PyRep::Integer(587),
        ]],
    },
    OnFleetJumpBeaconDeployableChange => "OnFleetJumpBeaconDeployableChange" ("fleetid") {
        fields: {
            deployable_id: i64,
            solar_system_id: i64,
            beacon_id: i64,
            owner_id: i64,
            active: bool,
        },
        samples: || vec![vec![
            PyRep::Integer(100_000_020),
            PyRep::Integer(30_000_142),
            PyRep::Integer(100_000_020),
            PyRep::Integer(90_000_001),
            PyRep::Bool(true),
        ]],
    },
    OnFleetJumpBeaconModuleChange => "OnFleetJumpBeaconModuleChange" ("fleetid") {
        fields: {
            char_id: i64,
            solar_system_id: i64,
            beacon_id: i64,
            type_id: i64,
            active: bool,
        },
        samples: || vec![vec![
            PyRep::Integer(90_000_001),
            PyRep::Integer(30_000_142),
            PyRep::Integer(100_000_020),
            PyRep::Integer(21_096),
            PyRep::Bool(true),
        ]],
    },
    OnGodmaEffectInfo => "OnGodmaEffectInfo" ("charid") {
        fields: {
            item_id: i64,
            type_id: i64,
            active_effects: PyRep,
            time: i64,
        },
        samples: || vec![vec![
            PyRep::Integer(100_000_020),
            PyRep::Integer(440),
            PyRep::Dict(Vec::new()),
            PyRep::Integer(1_335_000_000_000_000_00),
        ]],
    },
    OnGodmaFlushLocationProfile => "OnGodmaFlushLocationProfile" ("charid") {
        fields: { location_id: i64, profile_id: PyRep },
        samples: || vec![vec![
            PyRep::Integer(100_000_010),
            PyRep::Tuple(vec![PyRep::String("effect".into()), PyRep::Integer(100_000_020)]),
        ]],
    },
    OnGodmaPrimeItem => "OnGodmaPrimeItem" ("charid") {
        fields: { location_id: i64, row: PyRep },
        samples: || vec![vec![
            PyRep::Integer(100_000_010),
            PyRep::keyval([
                ("itemID", PyRep::Integer(100_000_020)),
                ("invItem", sample_keyval_item()),
                (
                    "attributes",
                    PyRep::Dict(vec![(PyRep::Integer(2), PyRep::Real(1.0))]),
                ),
                ("activeEffects", PyRep::Dict(Vec::new())),
                ("time", PyRep::Integer(1_335_000_000_000_000_00)),
                ("wallclockTime", PyRep::Integer(1_335_000_000_000_000_00)),
            ]),
        ]],
    },
    OnGodmaShipEffect => "OnGodmaShipEffect" ("charid") {
        fields: { item_id: i64, effect: PyRep, start_or_time: PyRep },
        optional: {
            start: Option<PyRep>,
            active: Option<PyRep>,
            environment: Option<PyRep>,
            start_time: Option<PyRep>,
            duration: Option<PyRep>,
            repeat: Option<PyRep>,
            error: Option<PyRep>,
            actual_stop_time: Option<PyRep>,
        },
        samples: || vec![vec![
            PyRep::Integer(200_018_015),
            PyRep::String("online".into()),
            PyRep::Bool(false),
        ]],
    },
    OnGroupJumpAnchorActivated => "OnGroupJumpAnchorActivated" ("charid") {
        fields: { to_solar_system_id: i64, num_passengers: i64 },
        samples: || vec![vec![PyRep::Integer(30_000_142), PyRep::Integer(2)]],
    },
    OnGroupJumpedPassenger => "OnGroupJumpedPassenger" ("charid") {
        fields: { anchor_char_id: i64, to_solar_system_id: i64 },
        samples: || vec![vec![PyRep::Integer(90_000_001), PyRep::Integer(30_000_142)]],
    },
    OnInvulnCancelled => "OnInvulnCancelled" ("charid") {
        fields: { ship_id: i64 },
        samples: || vec![vec![PyRep::Integer(100_000_010)]],
    },
    OnJamStart => "OnJamStart" ("solarsystemid") {
        fields: {
            source_ball_id: i64,
            module_id: i64,
            target_ball_id: i64,
            jamming_type: String,
            start_time: i64,
            duration: f64,
        },
        samples: || vec![vec![
            PyRep::Integer(100_000_010),
            PyRep::Integer(100_000_020),
            PyRep::Integer(100_000_030),
            PyRep::String("electronic".into()),
            PyRep::Integer(1_335_000_000_000_000_00),
            PyRep::Real(20_000.0),
        ]],
    },
    OnJumpCloakCancelled => "OnJumpCloakCancelled" ("charid") {
        fields: { ship_id: i64 },
        samples: || vec![vec![PyRep::Integer(100_000_010)]],
    },
    OnJumpCloakUpdated => "OnJumpCloakUpdated" ("charid") {
        fields: { ship_id: i64, end_time: i64, duration: i64 },
        samples: || vec![vec![
            PyRep::Integer(100_000_010),
            PyRep::Integer(1_335_000_600_000_000_00),
            PyRep::Integer(600_000_000),
        ]],
    },
    OnJumpQueueUpdate => "OnJumpQueueUpdate" ("charid") {
        fields: {
            solar_system_id: i64,
            space: i64,
            queue: i64,
            jump_keys: Vec<i64>,
            key_life: i64,
        },
        samples: || vec![vec![
            PyRep::Integer(30_000_142),
            PyRep::Integer(10),
            PyRep::Integer(2),
            PyRep::List(vec![PyRep::Integer(1)]),
            PyRep::Integer(60),
        ]],
    },
    OnJumpTimersUpdated => "OnJumpTimersUpdated" ("charid") {
        fields: {
            jump_activation: i64,
            jump_fatigue: i64,
            last_updated: i64,
            do_alert: bool,
        },
        samples: || vec![vec![
            PyRep::Integer(1_335_000_600_000_000_00),
            PyRep::Integer(1_335_000_600_000_000_00),
            PyRep::Integer(1_335_000_000_000_000_00),
            PyRep::Bool(true),
        ]],
    },
    OnKillRightSellCancel => "OnKillRightSellCancel" ("charid") {
        fields: { kill_right_id: i64 },
        samples: || vec![vec![PyRep::Integer(1)]],
    },
    OnLocalHackProgressUpdated => "OnLocalHackProgressUpdated" ("solarsystemid") {
        fields: { hacked_object_id: i64, hack_progress: f64 },
        samples: || vec![vec![PyRep::Integer(100_000_050), PyRep::Real(0.5)]],
    },
    OnMailTrashed => "OnMailTrashed" ("charid") {
        fields: { message_ids: Vec<i64> },
        samples: || vec![vec![PyRep::List(vec![PyRep::Integer(1)])]],
    },
    OnMatchFound => "OnMatchFound" ("charid") {
        samples: || vec![vec![]],
    },
    OnMedalIssued => "OnMedalIssued" ("charid") {
        fields: { medal_id: i64, corp_id: i64, issuer_id: i64 },
        samples: || vec![vec![
            PyRep::Integer(1),
            PyRep::Integer(98_000_001),
            PyRep::Integer(90_000_001),
        ]],
    },
    OnMedalStatusChanged => "OnMedalStatusChanged" ("charid") {
        fields: { medal_id: i64, new_status: i64 },
        samples: || vec![vec![PyRep::Integer(1), PyRep::Integer(3)]],
    },
    OnMissionsUpdated => "OnMissionsUpdated" ("charid") {
        fields: { missions: Vec<PyRep> },
        samples: || vec![vec![PyRep::List(vec![PyRep::keyval([
            ("agentID", PyRep::Integer(3_018_600)),
            ("info", PyRep::Dict(Vec::new())),
        ])])]],
    },
    OnModulesDeactivating => "OnModulesDeactivating" ("charid") {
        fields: { module_ids: Vec<i64> },
        samples: || vec![vec![PyRep::List(vec![PyRep::Integer(100_000_020)])]],
    },
    OnProgressionRoomJoined => "OnProgressionRoomJoined" ("charid") {
        fields: {
            progression_name_id: i64,
            objective_id: i64,
            task_widget_states: PyRep,
            objective_history: Vec<PyRep>,
            room_id: i64,
        },
        samples: || vec![vec![
            PyRep::Integer(100),
            PyRep::Integer(11),
            PyRep::List(Vec::new()),
            PyRep::List(vec![PyRep::Integer(1)]),
            PyRep::Integer(1),
        ]],
    },
    OnSkillQueuePausedServer => "OnSkillQueuePausedServer" ("charid") {
        optional: { paused: Option<bool> },
        samples: || vec![vec![PyRep::Bool(false)]],
    },
    OnSkinLicenseActivated => "OnSkinLicenseActivated" ("charid") {
        fields: { skin_id: i64, licensee_id: i64 },
        samples: || vec![vec![PyRep::Integer(123), PyRep::Integer(90_000_001)]],
    },
    OnSkinLicenseRemoved => "OnSkinLicenseRemoved" ("charid") {
        fields: { skin_id: i64, item_id: Option<i64>, type_id: Option<i64> },
        samples: || vec![vec![
            PyRep::Integer(123),
            PyRep::Integer(100_000_020),
            PyRep::Integer(34_567),
        ]],
    },
    OnStandingSet => "OnStandingSet" ("character") {
        fields: { from_id: i64, to_id: i64, standing: f64 },
        samples: || vec![vec![
            PyRep::Integer(1_000_125),
            PyRep::Integer(90_000_001),
            PyRep::Real(1.5),
        ]],
    },
    OnStandingsModified => "OnStandingsModified" ("character") {
        fields: { modifications: Vec<PyRep> },
        samples: || vec![vec![PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Integer(1_000_125),
            PyRep::Integer(90_000_001),
            PyRep::Real(0.5),
            PyRep::Real(-10.0),
            PyRep::Real(10.0),
        ])])]],
    },
    OnStructureAdded => "OnStructureAdded" ("solarsystemid") {
        fields: { solar_system_id: i64, structure_id: i64 },
        samples: || vec![vec![
            PyRep::Integer(30_000_142),
            PyRep::Integer(1_000_000_000_001),
        ]],
    },
    OnStructureRemoved => "OnStructureRemoved" ("solarsystemid") {
        fields: { solar_system_id: i64, structure_id: i64 },
        samples: || vec![vec![
            PyRep::Integer(30_000_142),
            PyRep::Integer(1_000_000_000_001),
        ]],
    },
    OnStructureRemovedFromSpace => "OnStructureRemovedFromSpace" ("corpid") {
        fields: { structure_id: i64 },
        samples: || vec![vec![PyRep::Integer(1_000_000_000_001)]],
    },
    OnStructureServiceChanged => "OnStructureServiceChanged" ("stationid") {
        fields: { structure_id: i64 },
        samples: || vec![vec![PyRep::Integer(1_000_000_000_001)]],
    },
    OnStructureTargetRemoved => "OnStructureTargetRemoved" ("charid") {
        fields: { structure_id: i64, target_id: i64 },
        samples: || vec![vec![PyRep::Integer(1_000_000_000_001), PyRep::Integer(0)]],
    },
    OnTournamentPerformBan => "OnTournamentPerformBan" ("charid") {
        fields: {
            ban_id: i64,
            num_bans: i64,
            cur_bans: Vec<i64>,
            deadline: i64,
            respond_to_node_id: i64,
            tourney_id: i64,
        },
        samples: || vec![vec![
            PyRep::Integer(1),
            PyRep::Integer(1),
            PyRep::List(vec![PyRep::Integer(587)]),
            PyRep::Integer(1_700_000_060),
            PyRep::Integer(1),
            PyRep::Integer(10),
        ]],
    },
    OnVoidSpaceRelog => "OnVoidSpaceRelog" ("charid") {
        fields: { client_data: PyRep },
        samples: || vec![vec![PyRep::Dict(vec![
            (PyRep::String("content_id".into()), PyRep::Integer(1)),
            (PyRep::String("nebula_graphic_id".into()), PyRep::Integer(22_042)),
            (PyRep::String("end_time".into()), PyRep::Integer(133_500_000_000_000_000)),
        ])]],
    },
    OnWarpFinished => "OnWarpFinished" ("charid") {
        fields: { item_id: i64, x_or_type_id: PyRep },
        optional: { y: Option<f64>, z: Option<f64> },
        samples: || vec![vec![
            PyRep::Integer(100_000_010),
            PyRep::Real(1000.0),
            PyRep::Real(0.0),
            PyRep::Real(0.0),
        ]],
    },
    OnWeaponBanksChanged => "OnWeaponBanksChanged" ("charid") {
        fields: { ship_id: i64, info: PyRep },
        samples: || vec![vec![
            PyRep::Integer(100_000_010),
            PyRep::Dict(Vec::new()),
        ]],
    },
    OnWeaponGroupDestroyed => "OnWeaponGroupDestroyed" ("charid") {
        fields: { ship_id: i64, item_id: i64 },
        samples: || vec![vec![
            PyRep::Integer(100_000_010),
            PyRep::Integer(100_000_020),
        ]],
    },
    // === Batch 5 Events ===
    OnAttributes => "OnAttributes" ("charid") {
        fields: { changes: PyRep },
        samples: || vec![vec![PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::String("mass".into()),
            PyRep::keyval([("itemID", PyRep::Integer(100_000_001))]),
            PyRep::Real(1.0),
        ])])]],
    },
    OnAutopilotUpdated => "OnAutopilotUpdated" ("charid") {
        samples: || vec![vec![]],
    },
    OnBlueprintsUpdated => "OnBlueprintsUpdated" ("ownerID") {
        fields: { owner_id: i64 },
        samples: || vec![vec![PyRep::Integer(90_000_001)]],
    },
    OnCharacterEnergyChanged => "OnCharacterEnergyChanged" ("charid") {
        fields: { character_id: i64, new_energy_state: PyRep },
        samples: || vec![vec![
            PyRep::Integer(90_000_001),
            PyRep::keyval([("currentEnergy", PyRep::Integer(100))]),
        ]],
    },
    OnContractAccepted => "OnContractAccepted" ("charid") {
        fields: { contract_id: i64 },
        samples: || vec![vec![PyRep::Integer(1)]],
    },
    OnContractAssigned => "OnContractAssigned" ("charid") {
        fields: { contract_id: i64 },
        samples: || vec![vec![PyRep::Integer(1)]],
    },
    OnContractBuyout => "OnContractBuyout" ("charid") {
        fields: { contract_id: i64 },
        samples: || vec![vec![PyRep::Integer(1)]],
    },
    OnContractCacheCleared => "OnContractCacheCleared" ("charid") {
        samples: || vec![vec![]],
    },
    OnContractCompleted => "OnContractCompleted" ("charid") {
        fields: { contract_id: i64 },
        samples: || vec![vec![PyRep::Integer(1)]],
    },
    OnContractCreated => "OnContractCreated" ("charid") {
        samples: || vec![vec![]],
    },
    OnContractFailed => "OnContractFailed" ("charid") {
        fields: { contract_id: i64 },
        samples: || vec![vec![PyRep::Integer(1)]],
    },
    OnContractOutbid => "OnContractOutbid" ("charid") {
        fields: { contract_id: i64 },
        samples: || vec![vec![PyRep::Integer(1)]],
    },
    OnContractRejected => "OnContractRejected" ("charid") {
        fields: { contract_id: i64 },
        samples: || vec![vec![PyRep::Integer(1)]],
    },
    OnDungeonExited => "OnDungeonExited" ("charid") {
        fields: { dungeon_id: i64, instance_id: i64 },
        samples: || vec![vec![PyRep::Integer(55_001), PyRep::Integer(1)]],
    },
    OnHackingTurnComplete => "OnHackingTurnComplete" ("charid") {
        fields: { events: Vec<PyRep> },
        samples: || vec![vec![PyRep::List(vec![PyRep::keyval([(
            "eventID",
            PyRep::Integer(1),
        )])])]],
    },
    OnIndustryJob => "OnIndustryJob" ("character") {
        fields: {
            job_id: i64,
            owner_id: i64,
            blueprint_id: i64,
            installer_id: i64,
            status: i64,
            successful_runs: i64,
        },
        samples: || vec![vec![
            PyRep::Integer(1),
            PyRep::Integer(90_000_001),
            PyRep::Integer(100_000_001),
            PyRep::Integer(90_000_001),
            PyRep::Integer(1),
            PyRep::Integer(1),
        ]],
    },
    OnLockedItemChange => "OnLockedItemChange" ("charid") {
        fields: { item_id: i64, owner_id: i64, location_id: i64, is_locked: bool },
        samples: || vec![vec![
            PyRep::Integer(100_000_001),
            PyRep::Integer(90_000_001),
            PyRep::Integer(100_000_010),
            PyRep::Bool(true),
        ]],
    },
    OnMajorPlanetStateUpdate => "OnMajorPlanetStateUpdate" ("charid") {
        fields: { planet_id: i64, num_command_centers_changed: bool },
        samples: || vec![vec![PyRep::Integer(40_000_001), PyRep::Bool(false)]],
    },
    OnModuleRepaired => "OnModuleRepaired" ("charid") {
        fields: { item_id: i64 },
        samples: || vec![vec![PyRep::Integer(100_000_001)]],
    },
    OnPILaunchesChange => "OnPILaunchesChange" ("charid") {
        samples: || vec![vec![]],
    },
    OnPlexDeposited => "OnPlexDeposited" ("charid") {
        fields: { reference: i64 },
        samples: || vec![vec![PyRep::Integer(1)]],
    },
    OnPlexItemAddedToInventory => "OnPlexItemAddedToInventory" ("charid") {
        fields: { item_id: i64, reference: i64 },
        samples: || vec![vec![PyRep::Integer(100_000_001), PyRep::Integer(1)]],
    },
    OnRemoteMessage => "OnRemoteMessage" ("charid") {
        fields: { msg_id: String },
        optional: { dict: Option<PyRep>, kw: Option<PyRep> },
        samples: || vec![vec![
            PyRep::String("ParamCustomsNotification".into()),
            PyRep::Dict(Vec::new()),
            PyRep::None,
        ]],
    },
    OnSafeLogoffAborted => "OnSafeLogoffAborted" ("charid") {
        fields: { reason_code: String },
        samples: || vec![vec![PyRep::String("UI/Inflight/SafeLogoff/Aborted".into())]],
    },
    OnSafeLogoffActivated => "OnSafeLogoffActivated" ("charid") {
        samples: || vec![vec![]],
    },
}

impl OnOwnOrdersChanged {
    /// Builds [`OnOwnOrdersChanged`] from a `[orders, reason, is_corp]` tuple vector.
    pub fn from_payload(payload: Vec<PyRep>) -> Self {
        let mut iter = payload.into_iter();
        Self {
            orders: iter.next().unwrap_or(PyRep::List(Vec::new())),
            reason: iter.next().unwrap_or(PyRep::String("Add".into())),
            is_corp: iter.next().unwrap_or(PyRep::Bool(false)),
        }
    }
}

// Legacy / server-internal events not present in `client_expectations["notify"]`, typed so service
// call sites carry zero raw `.notify(` / `build_macho_notification_packet(` calls while keeping
// `all_registered_notifications()` strictly aligned with contracted V24 client expectations.
define_notification_item!(OnCosmicAnomalyRemoved => "OnCosmicAnomalyRemoved" ("charid") {
    fields: { site_id: String },
    samples: || vec![vec![PyRep::String("ABC-123".into())]],
});
define_notification_item!(OnCynoJammerChanged => "OnCynoJammerChanged" ("solarsystemid") {
    fields: { solar_system_id: i64, status: i64 },
    samples: || vec![vec![PyRep::Integer(30_000_142), PyRep::Integer(0)]],
});
define_notification_item!(OnCynoJammerChangedOpt => "OnCynoJammerChanged" ("solarsystemid") {
    fields: { solar_system_id: i64, status: Option<i64> },
    samples: || vec![vec![PyRep::Integer(30_000_142), PyRep::None]],
});
define_notification_item!(OnDbuffUpdated => "OnDbuffUpdated" ("charid") {
    fields: { ship_id: i64 },
    samples: || vec![vec![PyRep::Integer(100_000_001)]],
});
define_notification_item!(OnDroneSettingChanged => "OnDroneSettingChanged" ("charid") {
    fields: { aggressive: bool, focus_fire: bool },
    samples: || vec![vec![PyRep::Bool(true), PyRep::Bool(true)]],
});
define_notification_item!(OnDungeonTriggerAmbientSoundStr => "OnDungeonTriggerAmbientSound" ("charid") {
    fields: { dungeon_id: i64, sound_id: String, entity_item_id: i64, emitter_id: i64 },
    samples: || vec![vec![
        PyRep::Integer(42),
        PyRep::String("amb".into()),
        PyRep::Integer(100_000_010),
        PyRep::Integer(100_000_011),
    ]],
});
define_notification_item!(OnDungeonTriggerMessageStr => "OnDungeonTriggerMessage" ("charid") {
    fields: { message_type: String, message_id: i64 },
    samples: || vec![vec![PyRep::String("warn".into()), PyRep::Integer(5001)]],
});
define_notification_item!(OnFleetMoveFailedCharOnly => "OnFleetMoveFailed" ("charid") {
    fields: { char_id: i64 },
    samples: || vec![vec![PyRep::Integer(90_000_002)]],
});
define_notification_item!(OnMatchMakingFailureCodes => "OnMatchMakingFailure" ("charid") {
    fields: { errors: Vec<i64> },
    samples: || vec![vec![PyRep::List(vec![PyRep::Integer(1)])]],
});
define_notification_item!(OnNewCalendarEventOptAuto => "OnNewCalendarEvent" ("charid") {
    fields: {
        event_id: i64,
        owner_id: i64,
        event_date_time: i64,
        event_duration: i64,
        event_title: String,
        importance: i64,
        auto_event_type: Option<i64>,
        do_blink: bool,
    },
    samples: || vec![vec![
        PyRep::Integer(1),
        PyRep::Integer(90_000_001),
        PyRep::Integer(1_335_000_000_000_000_00),
        PyRep::Integer(60),
        PyRep::String("Event".into()),
        PyRep::Integer(1),
        PyRep::None,
        PyRep::Bool(true),
    ]],
});
define_notification_item!(OnNotificationChange => "OnNotificationChange" ("charid") {
    fields: { char_id: i64 },
    samples: || vec![vec![PyRep::Integer(90_000_001)]],
});
define_notification_item!(OnInsurgencyCampaignUpdatedForLocationFallback => "OnInsurgencyCampaignUpdatedForLocation" ("solarsystemid") {
    fields: { campaign_id: i64, state: i64 },
    samples: || vec![vec![PyRep::Integer(1), PyRep::Integer(1)]],
});
