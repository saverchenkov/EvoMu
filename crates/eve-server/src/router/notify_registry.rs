//! Typed notification registry with macro-generated payloads and compile-time arity checks.
//!
//! Every notification emitted through [`ServerContext::emit_typed`] (and DB-backed helpers)
//! is registered here as the single source of truth for its event name, default `id_type`,
//! positional argument types, and sample payloads verified against extracted client expectations.

#![forbid(unsafe_code)]

use crate::router::cache_notifications::PendingMachoNotification;
use crate::router::{NotificationTarget, ServerContext};
use eve_db::clock::{SimFileTime, WallFileTime};
use eve_proto::PyRep;

pub use crate::outbound_scene_scope::{
    build_pending_typed_notification_in_ballpark, BallparkScene, OutboundSceneScope,
};
pub use crate::router::notify_registry_emit::*;
pub use super::notify_types::*;

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

impl ToNotifyArg for WallFileTime {
    fn into_notify_arg(self) -> PyRep {
        PyRep::Integer(self.as_ticks())
    }
}

impl ToNotifyArg for SimFileTime {
    fn into_notify_arg(self) -> PyRep {
        PyRep::Integer(self.as_ticks())
    }
}

/// Wire argument for `OnGodmaShipEffect`'s 3rd positional slot (`t_or_start`), which carries
/// either a boolean start/stop flag (minimal 3-arg form) or a simulation timestamp (10/11-arg form).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GodmaEffectStartOrTime {
    Flag(bool),
    SimTime(SimFileTime),
}

impl ToNotifyArg for GodmaEffectStartOrTime {
    fn into_notify_arg(self) -> PyRep {
        match self {
            Self::Flag(b) => PyRep::Bool(b),
            Self::SimTime(ft) => ft.into_notify_arg(),
        }
    }
}

/// Wire argument for `OnCrimewatchEngagementUpdated`'s 2nd positional slot (`timeout`), which
/// carries either `true` while active/paused or a wall-clock [`WallFileTime`] expiry deadline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrimewatchEngagementTimeout {
    Active,
    EndsAt(WallFileTime),
}

impl ToNotifyArg for CrimewatchEngagementTimeout {
    fn into_notify_arg(self) -> PyRep {
        match self {
            Self::Active => PyRep::Bool(true),
            Self::EndsAt(ft) => ft.into_notify_arg(),
        }
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

/// Single bean-priming entry sent in `OnBeanPrime(beans)` (`carbon/common/script/sys/alert.pyj:Alert.OnBeanPrime`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BeanPrimeItem {
    pub error_id: i64,
    pub error_key_hash: i64,
    pub log_mode: String,
}

impl ToNotifyArg for BeanPrimeItem {
    fn into_notify_arg(self) -> PyRep {
        PyRep::keyval([
            ("errorID", PyRep::Integer(self.error_id)),
            ("errorKeyHash", PyRep::Integer(self.error_key_hash)),
            ("logMode", PyRep::String(self.log_mode)),
        ])
    }
}

/// Implemented by every registered notification payload struct.
pub trait TypedNotification {
    const EVENT: &'static str;
    const DEFAULT_ID_TYPE: &'static str;
    const MIN_ARITY: usize;
    const MAX_ARITY: usize;
    const IS_BALLPARK_SCOPED: bool = false;

    fn into_args(self) -> Vec<PyRep>;

    fn scene_scope(&self) -> Option<OutboundSceneScope> {
        None
    }

    #[cfg(any(test, feature = "testkit"))]
    fn sample_payloads() -> Vec<Vec<PyRep>>;
}

/// Converts `notif` into wire arguments and, in `test`/`testkit` builds, enforces strict recursive
/// shape matching against registered `sample_payloads` (`#933` Track 6b).
#[inline]
#[track_caller]
pub fn typed_args_checked<N: TypedNotification>(notif: N) -> Vec<PyRep> {
    let args = notif.into_args();
    #[cfg(any(test, feature = "testkit"))]
    super::notify_shape_check::assert_typed_notification_shape::<N>(&args);
    args
}

/// Static descriptor entry exposed for contract verification across all registered events.
#[derive(Debug, Clone, Copy)]
pub struct NotificationRegistryEntry {
    pub event: &'static str,
    pub default_id_type: &'static str,
    pub min_arity: usize,
    pub max_arity: usize,
    pub is_ballpark_scoped: bool,
    pub sample_payloads: fn() -> Vec<Vec<PyRep>>,
}

impl ServerContext {
    /// Emits a typed notification using its registered `EVENT` name and `DEFAULT_ID_TYPE`.
    #[track_caller] pub fn emit_typed<N: TypedNotification>(
        &self,
        target: NotificationTarget,
        notif: N,
    ) -> Vec<PendingMachoNotification> {
        let scope = notif.scene_scope();
        let args = typed_args_checked(notif);
        match scope {
            Some(_) => self.notify_in_scene(
                target,
                N::EVENT,
                N::DEFAULT_ID_TYPE,
                args,
                scope,
            ),
            None => self.notify(target, N::EVENT, N::DEFAULT_ID_TYPE, args),
        }
    }
    /// Emits a typed notification with an explicit MachoNet `id_type` override (for events whose
    /// scope varies by dispatch target, e.g. `"allianceid"` vs `"corpid"`).
    #[track_caller] pub fn emit_typed_with_id_type<N: TypedNotification>(
        &self,
        target: NotificationTarget,
        id_type: &str,
        notif: N,
    ) -> Vec<PendingMachoNotification> {
        let scope = notif.scene_scope();
        let args = typed_args_checked(notif);
        match scope {
            Some(_) => self.notify_in_scene(target, N::EVENT, id_type, args, scope),
            None => self.notify(target, N::EVENT, id_type, args),
        }
    }

    /// Emits a registered notification by event name to a single character after verifying
    /// that `event` is registered in [`all_registered_notifications`] and `args.len()`
    /// satisfies its registered arity bounds.
    #[track_caller]
    pub fn emit_registered_to_char(
        &self,
        char_id: i64,
        event: &str,
        id_type: &str,
        args: Vec<PyRep>,
    ) -> Option<Vec<PendingMachoNotification>> {
        let entry = find_registered_notification_checked(event, args.len())?;
        if entry.is_ballpark_scoped {
            return None;
        }
        #[cfg(any(test, feature = "testkit"))]
        super::notify_shape_check::assert_registered_notification_shape(entry, &args);
        Some(self.notify(
            NotificationTarget::Character(char_id),
            entry.event,
            id_type,
            args,
        ))
    }
}

/// Emits a V24 4-argument `ProcessInventoryChange(items, change, isRemote, inventory2)` notice.
pub(crate) fn emit_process_inventory_change(
    ctx: &ServerContext,
    target_char_id: i64,
    item: &eve_db::InventoryItemRecord,
    old_location_id: i64,
    old_flag_id: i64,
) {
    let (category_id, group_id) = ctx
        .sde
        .types
        .get(&item.type_id)
        .map(|t| (i64::from(t.category_id), i64::from(t.group_id)))
        .unwrap_or((0, 0));
    let item_kv = PyRep::keyval([
        ("itemID", PyRep::Integer(item.item_id)),
        ("typeID", PyRep::Integer(i64::from(item.type_id))),
        ("ownerID", PyRep::Integer(item.owner_id)),
        ("locationID", PyRep::Integer(item.location_id)),
        ("flagID", PyRep::Integer(item.flag as i64)),
        ("categoryID", PyRep::Integer(category_id)),
        ("groupID", PyRep::Integer(group_id)),
        ("singleton", PyRep::Bool(item.singleton)),
        ("quantity", PyRep::Integer(i64::from(item.quantity))),
        ("stacksize", PyRep::Integer(i64::from(item.quantity))),
    ]);
    let change_dict = PyRep::Dict(vec![
        (
            PyRep::Integer(3),
            PyRep::Tuple(vec![
                PyRep::Integer(old_location_id),
                PyRep::Integer(item.location_id),
            ]),
        ),
        (
            PyRep::Integer(4),
            PyRep::Tuple(vec![
                PyRep::Integer(old_flag_id),
                PyRep::Integer(item.flag as i64),
            ]),
        ),
    ]);
    ctx.emit_typed(
        NotificationTarget::Character(target_char_id),
        ProcessInventoryChange {
            arg0: PyRep::List(vec![item_kv]),
            arg1: change_dict,
            arg2: PyRep::Bool(true),
            arg3: Some(PyRep::None),
        },
    );
}

#[cfg(any(test, feature = "testkit"))]
#[track_caller]
pub(super) fn assert_registered_entry_shape(
    entry: &NotificationRegistryEntry,
    args: &[PyRep],
) {
    super::notify_shape_check::assert_registered_notification_shape(entry, args);
}

#[cfg(any(test, feature = "testkit"))]
pub(crate) fn assert_registered_event_shape_if_registered(event: &str, args: &[PyRep]) {
    if let Some(entry) = all_registered_notifications()
        .iter()
        .find(|e| e.event == event)
    {
        assert_registered_entry_shape(entry, args);
    }
}

pub(super) fn find_registered_notification_checked(
    event: &str,
    arg_len: usize,
) -> Option<&'static NotificationRegistryEntry> {
    let entry = all_registered_notifications()
        .iter()
        .find(|e| e.event == event)?;
    if arg_len < entry.min_arity || arg_len > entry.max_arity {
        return None;
    }
    Some(entry)
}

pub(super) fn encode_macho_packet_for_emit(
    recipient_id: i64,
    user_id_opt: Option<u64>,
    event: &str,
    id_type: &str,
    args: Vec<PyRep>,
) -> (eve_machonet::MachoPacket, Vec<u8>) {
    ServerContext::build_macho_notification_packet(recipient_id, user_id_opt, event, id_type, args)
}

pub(super) fn push_session_notification_for_emit(
    session: &mut eve_machonet::SessionState,
    event: impl Into<String>,
    args: Vec<PyRep>,
) {
    session.push_notification(event, args);
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

            #[cfg(any(test, feature = "testkit"))]
            fn sample_payloads() -> Vec<Vec<PyRep>> {
                let f: fn() -> Vec<Vec<PyRep>> = $samples;
                f()
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

            #[cfg(any(test, feature = "testkit"))]
            fn sample_payloads() -> Vec<Vec<PyRep>> {
                let f: fn() -> Vec<Vec<PyRep>> = $samples;
                f()
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

            #[cfg(any(test, feature = "testkit"))]
            fn sample_payloads() -> Vec<Vec<PyRep>> {
                let f: fn() -> Vec<Vec<PyRep>> = $samples;
                f()
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

            #[cfg(any(test, feature = "testkit"))]
            fn sample_payloads() -> Vec<Vec<PyRep>> {
                let f: fn() -> Vec<Vec<PyRep>> = $samples;
                f()
            }
        }
    };

    // Ballpark/scene-scoped variants carrying a required `scene: BallparkScene` field
    // (NOT serialized on the wire; forwarded to `OutboundSceneScope::space`).
    ($name:ident => $event:literal ($id_type:literal) @ballpark {
        samples: $samples:expr $(,)?
    }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub struct $name {
            pub scene: BallparkScene,
        }

        impl TypedNotification for $name {
            const EVENT: &'static str = $event;
            const DEFAULT_ID_TYPE: &'static str = $id_type;
            const MIN_ARITY: usize = 0;
            const MAX_ARITY: usize = 0;
            const IS_BALLPARK_SCOPED: bool = true;

            fn into_args(self) -> Vec<PyRep> {
                Vec::new()
            }

            fn scene_scope(&self) -> Option<OutboundSceneScope> {
                Some(self.scene.into_outbound_scope())
            }

            #[cfg(any(test, feature = "testkit"))]
            fn sample_payloads() -> Vec<Vec<PyRep>> {
                let f: fn() -> Vec<Vec<PyRep>> = $samples;
                f()
            }
        }
    };

    ($name:ident => $event:literal ($id_type:literal) @ballpark {
        fields: { $($field:ident : $ty:ty),+ $(,)? },
        samples: $samples:expr $(,)?
    }) => {
        #[derive(Debug, Clone, PartialEq)]
        pub struct $name {
            pub scene: BallparkScene,
            $(pub $field: $ty),+
        }

        impl TypedNotification for $name {
            const EVENT: &'static str = $event;
            const DEFAULT_ID_TYPE: &'static str = $id_type;
            const MIN_ARITY: usize = count_idents!($($field),+);
            const MAX_ARITY: usize = Self::MIN_ARITY;
            const IS_BALLPARK_SCOPED: bool = true;

            fn into_args(self) -> Vec<PyRep> {
                vec![$(ToNotifyArg::into_notify_arg(self.$field)),+]
            }

            fn scene_scope(&self) -> Option<OutboundSceneScope> {
                Some(self.scene.into_outbound_scope())
            }

            #[cfg(any(test, feature = "testkit"))]
            fn sample_payloads() -> Vec<Vec<PyRep>> {
                let f: fn() -> Vec<Vec<PyRep>> = $samples;
                f()
            }
        }
    };

    ($name:ident => $event:literal ($id_type:literal) @ballpark {
        fields: { $($field:ident : $ty:ty),+ $(,)? },
        optional: { $($opt_field:ident : Option<$opt_ty:ty>),+ $(,)? },
        samples: $samples:expr $(,)?
    }) => {
        #[derive(Debug, Clone, PartialEq)]
        pub struct $name {
            pub scene: BallparkScene,
            $(pub $field: $ty,)+
            $(pub $opt_field: Option<$opt_ty>),+
        }

        impl TypedNotification for $name {
            const EVENT: &'static str = $event;
            const DEFAULT_ID_TYPE: &'static str = $id_type;
            const MIN_ARITY: usize = count_idents!($($field),+);
            const MAX_ARITY: usize = Self::MIN_ARITY + count_idents!($($opt_field),+);
            const IS_BALLPARK_SCOPED: bool = true;

            fn into_args(self) -> Vec<PyRep> {
                let mut args = vec![$(ToNotifyArg::into_notify_arg(self.$field)),+];
                $(
                    if let Some(opt_val) = self.$opt_field {
                        args.push(ToNotifyArg::into_notify_arg(opt_val));
                    }
                )+
                args
            }

            fn scene_scope(&self) -> Option<OutboundSceneScope> {
                Some(self.scene.into_outbound_scope())
            }

            #[cfg(any(test, feature = "testkit"))]
            fn sample_payloads() -> Vec<Vec<PyRep>> {
                let f: fn() -> Vec<Vec<PyRep>> = $samples;
                f()
            }
        }
    };

    ($name:ident => $event:literal ($id_type:literal) @ballpark {
        optional: { $($opt_field:ident : Option<$opt_ty:ty>),+ $(,)? },
        samples: $samples:expr $(,)?
    }) => {
        #[derive(Debug, Clone, PartialEq)]
        pub struct $name {
            pub scene: BallparkScene,
            $(pub $opt_field: Option<$opt_ty>),+
        }

        impl TypedNotification for $name {
            const EVENT: &'static str = $event;
            const DEFAULT_ID_TYPE: &'static str = $id_type;
            const MIN_ARITY: usize = 0;
            const MAX_ARITY: usize = count_idents!($($opt_field),+);
            const IS_BALLPARK_SCOPED: bool = true;

            fn into_args(self) -> Vec<PyRep> {
                let mut args = Vec::new();
                $(
                    if let Some(opt_val) = self.$opt_field {
                        args.push(ToNotifyArg::into_notify_arg(opt_val));
                    }
                )+
                args
            }

            fn scene_scope(&self) -> Option<OutboundSceneScope> {
                Some(self.scene.into_outbound_scope())
            }

            #[cfg(any(test, feature = "testkit"))]
            fn sample_payloads() -> Vec<Vec<PyRep>> {
                let f: fn() -> Vec<Vec<PyRep>> = $samples;
                f()
            }
        }
    };
}

macro_rules! define_notifications {
    ($(
        $name:ident => $event:literal ($id_type:literal) $(@ $tag:ident)? { $($body:tt)* }
    ),* $(,)?) => {
        $(
            define_notification_item!($name => $event ($id_type) $(@ $tag)? { $($body)* });
        )*

        /// Static table of all typed notifications registered in the central registry.
        pub fn all_registered_notifications() -> &'static [NotificationRegistryEntry] {
            &[
                $(
                    NotificationRegistryEntry {
                        event: <$name as TypedNotification>::EVENT,
                        default_id_type: <$name as TypedNotification>::DEFAULT_ID_TYPE,
                        min_arity: <$name as TypedNotification>::MIN_ARITY,
                        max_arity: <$name as TypedNotification>::MAX_ARITY,
                        is_ballpark_scoped: <$name as TypedNotification>::IS_BALLPARK_SCOPED,
                        sample_payloads: define_notifications!(@samples { $($body)* }),
                    },
                )*
            ]
        }

        /// Event names marked `@ballpark` in [`define_notifications!`] (`#1190` RB3).
        pub const BALLPARK_SCOPED_NOTIFICATION_EVENTS: &[&str] = &[
            $(
                $(
                    define_notifications!(@ballpark_event $tag $event),
                )?
            )*
        ];

        /// Returns `true` when `event` requires an in-space ballpark [`OutboundSceneScope`].
        pub fn is_ballpark_scoped_notification(event: &str) -> bool {
            BALLPARK_SCOPED_NOTIFICATION_EVENTS.contains(&event)
        }
    };

    (@ballpark_event ballpark $event:literal) => {
        $event
    };

    (@samples {
        samples: $samples:expr $(,)?
    }) => {
        $samples
    };
    (@samples {
        fields: { $($field:ident : $ty:ty),+ $(,)? },
        samples: $samples:expr $(,)?
    }) => {
        $samples
    };
    (@samples {
        fields: { $($field:ident : $ty:ty),+ $(,)? },
        optional: { $($opt_field:ident : Option<$opt_ty:ty>),+ $(,)? },
        samples: $samples:expr $(,)?
    }) => {
        $samples
    };
    (@samples {
        optional: { $($opt_field:ident : Option<$opt_ty:ty>),+ $(,)? },
        samples: $samples:expr $(,)?
    }) => {
        $samples
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

fn sample_season_goal_dict() -> PyRep {
    PyRep::Dict(vec![
        (PyRep::String("goal_index".into()), PyRep::Integer(1)),
        (PyRep::String("points_required".into()), PyRep::Integer(20)),
        (PyRep::String("reward_type_id".into()), PyRep::Integer(34)),
        (PyRep::String("reward_amount".into()), PyRep::Integer(1)),
        (PyRep::String("omega_only".into()), PyRep::Bool(false)),
        (PyRep::String("completed".into()), PyRep::Bool(true)),
        (PyRep::String("claimed".into()), PyRep::Bool(false)),
    ])
}

fn sample_season_data_kv(with_card_msgs: bool) -> PyRep {
    let opt_id = if with_card_msgs {
        PyRep::Integer(583_333)
    } else {
        PyRep::None
    };
    PyRep::keyval([
        ("season_id", PyRep::Integer(1)),
        ("seasonID", PyRep::Integer(1)),
        ("template_id", PyRep::Integer(1)),
        ("title", PyRep::Integer(583_333)),
        ("seasonName", PyRep::String("Season 1".into())),
        ("news", PyRep::Integer(583_334)),
        ("navigation_card_picture_path", PyRep::String("res:/ui/texture/classes/seasons/nav.png".into())),
        ("content_card_picture_path", PyRep::String("res:/ui/texture/classes/seasons/nav.png".into())),
        ("background_no_logo", PyRep::String("res:/ui/texture/classes/seasons/bg.png".into())),
        ("agencyLargeSeasonSelectionTitleID", PyRep::Integer(583_380)),
        ("seasonSelectionDescriptionID", PyRep::Integer(583_334)),
        ("seasonSelectionHintID", PyRep::Integer(583_335)),
        ("seasonSelectionTitleID", PyRep::Integer(583_333)),
        ("agency_card_title", opt_id.clone()),
        ("agency_card_hint", opt_id),
        ("agency_card_description", PyRep::None),
        ("start_time", PyRep::Integer(133_500_000_000_000_000)),
        ("end_time", PyRep::Integer(133_600_000_000_000_000)),
        ("multiple_season_running", PyRep::Bool(false)),
        ("is_auto_claim_enabled", PyRep::Bool(false)),
        ("feature_tag_id", PyRep::None),
        ("max_active_challenges", PyRep::Integer(4)),
        ("tracked_event_types", PyRep::List(vec![PyRep::String("combat".into())])),
    ])
}

fn sample_character_progress_kv(with_goal_dicts: bool) -> PyRep {
    let goal_opt = if with_goal_dicts {
        sample_season_goal_dict()
    } else {
        PyRep::None
    };
    PyRep::keyval([
        ("character_id", PyRep::Integer(90_000_001)),
        ("characterID", PyRep::Integer(90_000_001)),
        ("season_id", PyRep::Integer(1)),
        ("seasonID", PyRep::Integer(1)),
        ("total_points", PyRep::Integer(10)),
        ("season_points", PyRep::Integer(10)),
        ("seasonal_points", PyRep::Integer(10)),
        ("seasonPoints", PyRep::Integer(10)),
        ("max_seasonal_points", PyRep::Integer(100)),
        ("seasonal_goals", PyRep::Dict(vec![(PyRep::Integer(1), sample_season_goal_dict())])),
        ("next_seasonal_goal", goal_opt.clone()),
        ("last_seasonal_goal", goal_opt),
        ("claimedRewards", PyRep::Integer(0)),
        ("current_reward_index", PyRep::Integer(0)),
        (
            "challenges",
            PyRep::Dict(vec![(PyRep::Integer(43), sample_season_challenge_object())]),
        ),
    ])
}

fn sample_insurgency_campaign_snapshot() -> PyRep {
    PyRep::keyval([
        ("campaignID", PyRep::Integer(402)),
        ("solarSystemID", PyRep::Integer(30_000_142)),
        ("originSolarsystemID", PyRep::Integer(30_000_142)),
        ("structureID", PyRep::Integer(1_040_000_001)),
        ("warzoneID", PyRep::Integer(1)),
        ("pirateFactionID", PyRep::Integer(500_010)),
        ("targetFactionID", PyRep::Integer(500_001)),
        ("fobSolarSystemID", PyRep::Integer(30_000_142)),
        ("fsmState", PyRep::String("active".into())),
        ("phase", PyRep::String("active".into())),
        ("phaseEndTime", PyRep::Integer(1_700_000_000)),
        ("isFobSystem", PyRep::Bool(true)),
        ("isFobVulnerable", PyRep::Bool(false)),
        ("corruptionStage", PyRep::Integer(0)),
        ("suppressionStage", PyRep::Integer(0)),
        ("corruptionPoints", PyRep::Integer(0)),
        ("suppressionPoints", PyRep::Integer(0)),
        ("piratePointsRequired", PyRep::Integer(1_000)),
        ("antipiratePointsRequired", PyRep::Integer(1_000)),
        ("coveredSolarsystemIDs", PyRep::List(vec![PyRep::Integer(30_000_142)])),
        ("systems", PyRep::List(vec![PyRep::Integer(30_000_142)])),
    ])
}

fn sample_scan_result_kvs() -> Vec<PyRep> {
    let mut sig_base = vec![
        ("id", PyRep::String("ABC-123".into())),
        ("signatureID", PyRep::String("ABC-123".into())),
        ("itemID", PyRep::Integer(100_000_001)),
        ("targetID", PyRep::Integer(100_000_001)),
        ("targetClass", PyRep::String("CosmicSignature".into())),
        ("scanGroupID", PyRep::Integer(3)),
        ("groupID", PyRep::Integer(502)),
        ("strengthAttributeID", PyRep::Integer(1908)),
        ("group", PyRep::String("Relic Site".into())),
        ("groupName", PyRep::None),
        ("type", PyRep::String("Unidentified Signature".into())),
        ("siteName", PyRep::None),
        ("signalStrength", PyRep::Real(0.2)),
        ("certainty", PyRep::Real(0.2)),
        ("warpable", PyRep::Bool(false)),
        ("deviation", PyRep::Real(1.0e11)),
        ("maxDeviation", PyRep::Real(1.5e11)),
        ("maxDeviationAU", PyRep::Real(1.0)),
        ("scanRangeAU", PyRep::Real(8.0)),
        ("baseScanRangeAU", PyRep::Real(8.0)),
        ("baseDeviationAU", PyRep::Real(1.0)),
        ("coveringProbes", PyRep::Integer(4)),
        ("resolved", PyRep::Bool(false)),
        ("x", PyRep::None),
        ("y", PyRep::None),
        ("z", PyRep::None),
        ("typeID", PyRep::None),
        ("data", PyRep::Tuple(vec![PyRep::Real(0.0), PyRep::Real(0.0), PyRep::Real(0.0)])),
    ];
    let sig_unresolved = PyRep::keyval(sig_base.clone());

    let sig_with_group = PyRep::keyval(sig_base.iter().map(|(k, v)| match *k {
        "groupName" => (*k, PyRep::String("Relic Site".into())),
        _ => (*k, v.clone()),
    }));

    sig_base.extend([
        ("difficultyLevel", PyRep::Integer(2)),
        ("requiredProbeStrength", PyRep::Real(20.0)),
        ("formationProbeStrength", PyRep::Real(40.0)),
    ]);
    let sig_with_name = PyRep::keyval(sig_base.iter().map(|(k, v)| match *k {
        "groupName" => (*k, PyRep::String("Relic Site".into())),
        "siteName" => (*k, PyRep::String("Ruined Angel Monument".into())),
        _ => (*k, v.clone()),
    }));

    sig_base.extend([
        ("massStage", PyRep::String("stable".into())),
        ("wormholeSize", PyRep::Real(0.0)),
        ("lifetimeStage", PyRep::String("stable".into())),
        ("wormholeAge", PyRep::Integer(0)),
        ("lifetimeDescription", PyRep::String("Not reaching end of natural lifetime".into())),
        ("massStabilityDescription", PyRep::String("Mass stability undisturbed".into())),
        ("collapsed", PyRep::Bool(false)),
    ]);
    let sig_resolved_site = PyRep::keyval(sig_base.iter().map(|(k, v)| match *k {
        "groupName" => (*k, PyRep::String("Relic Site".into())),
        "siteName" => (*k, PyRep::String("Ruined Angel Monument".into())),
        "warpable" => (*k, PyRep::Bool(true)),
        "resolved" => (*k, PyRep::Bool(true)),
        "x" | "y" | "z" => (*k, PyRep::Real(1000.0)),
        _ => (*k, v.clone()),
    }));
    let sig_resolved_wh = PyRep::keyval(sig_base.iter().map(|(k, v)| match *k {
        "groupName" => (*k, PyRep::String("Wormhole".into())),
        "siteName" => (*k, PyRep::String("K162".into())),
        "warpable" => (*k, PyRep::Bool(true)),
        "resolved" => (*k, PyRep::Bool(true)),
        "x" | "y" | "z" => (*k, PyRep::Real(1000.0)),
        "typeID" => (*k, PyRep::Integer(30_650)),
        _ => (*k, v.clone()),
    }));
    sig_base.extend([
        ("isDeadspace", PyRep::Bool(false)),
        ("wormholeOffsetMeters", PyRep::Real(30_000.0)),
        ("wormholePosition", PyRep::Tuple(vec![PyRep::Real(0.0), PyRep::Real(0.0), PyRep::Real(0.0)])),
        ("guardNpcBallIDs", PyRep::List(vec![PyRep::Integer(100_000_020)])),
    ]);
    let sig_resolved_pochven_wh = PyRep::keyval(sig_base.iter().map(|(k, v)| match *k {
        "groupName" => (*k, PyRep::String("Wormhole".into())),
        "siteName" => (*k, PyRep::String("K162".into())),
        "warpable" => (*k, PyRep::Bool(true)),
        "resolved" => (*k, PyRep::Bool(true)),
        "x" | "y" | "z" => (*k, PyRep::Real(1000.0)),
        "typeID" => (*k, PyRep::Integer(30_650)),
        _ => (*k, v.clone()),
    }));

    let mut cmb_base = vec![
        ("id", PyRep::String("CMB-100000010".into())),
        ("signatureID", PyRep::String("CMB-100000010".into())),
        ("itemID", PyRep::Integer(100_000_010)),
        ("targetID", PyRep::Integer(100_000_010)),
        ("targetClass", PyRep::String("Ship".into())),
        ("scanGroupID", PyRep::Integer(4)),
        ("group", PyRep::String("Ship".into())),
        ("type", PyRep::String("Rifter".into())),
        ("signalStrength", PyRep::Real(0.2)),
        ("certainty", PyRep::Real(0.2)),
        ("warpable", PyRep::Bool(false)),
        ("deviation", PyRep::Real(1.0e11)),
        ("maxDeviation", PyRep::Real(1.5e11)),
        ("maxDeviationAU", PyRep::Real(1.0)),
        ("coveringProbes", PyRep::Integer(4)),
        ("resolved", PyRep::Bool(false)),
        ("x", PyRep::None),
        ("y", PyRep::None),
        ("z", PyRep::None),
        ("groupID", PyRep::Integer(502)),
        ("typeID", PyRep::None),
        ("data", PyRep::Tuple(vec![PyRep::Real(0.0), PyRep::Real(0.0), PyRep::Real(0.0)])),
    ];
    let cmb_unresolved = PyRep::keyval(cmb_base.clone());
    cmb_base.push(("categoryID", PyRep::Integer(6)));
    let cmb_group_only = PyRep::keyval(cmb_base.clone());
    let cmb_name_only = PyRep::keyval(cmb_base.iter().map(|(k, v)| match *k {
        "typeID" => (*k, PyRep::Integer(587)),
        _ => (*k, v.clone()),
    }));
    let cmb_resolved = PyRep::keyval(cmb_base.iter().map(|(k, v)| match *k {
        "warpable" => (*k, PyRep::Bool(true)),
        "resolved" => (*k, PyRep::Bool(true)),
        "x" | "y" | "z" => (*k, PyRep::Real(1000.0)),
        "typeID" => (*k, PyRep::Integer(587)),
        _ => (*k, v.clone()),
    }));

    vec![
        sig_unresolved,
        sig_with_group,
        sig_with_name,
        sig_resolved_site,
        sig_resolved_wh,
        sig_resolved_pochven_wh,
        cmb_unresolved,
        cmb_group_only,
        cmb_name_only,
        cmb_resolved,
    ]
}

fn sample_war_kv(time_finished_set: bool, war_hq_set: bool) -> PyRep {
    PyRep::keyval([
        ("warID", PyRep::Integer(1)),
        ("declaredByID", PyRep::Integer(98_000_001)),
        ("againstID", PyRep::Integer(98_000_002)),
        ("timeDeclared", PyRep::Integer(1_335_000_000_000_000_00)),
        ("timeStarted", PyRep::Integer(1_335_000_000_000_000_00)),
        (
            "timeFinished",
            if time_finished_set {
                PyRep::Integer(1_335_000_001_000_000_00)
            } else {
                PyRep::None
            },
        ),
        ("mutual", PyRep::Bool(false)),
        ("openForAllies", PyRep::Bool(false)),
        (
            "warHQ",
            if war_hq_set {
                PyRep::Integer(1_000_000_000_001)
            } else {
                PyRep::None
            },
        ),
        ("active", PyRep::Bool(true)),
        ("combatActive", PyRep::Bool(true)),
    ])
}

/// Sample `seasons.common.challenge.Challenge` `ObjectEx` payload matching
/// `services/season_manager.rs:322-454` (`challenge_to_v24_object`).
fn sample_season_challenge_object() -> PyRep {
    let state_dict = PyRep::Dict(vec![
        (PyRep::String("challenge_id".into()), PyRep::Integer(43)),
        (PyRep::String("challengeID".into()), PyRep::Integer(43)),
        (PyRep::String("challenge_type_id".into()), PyRep::Integer(43)),
        (PyRep::String("character_id".into()), PyRep::Integer(90_000_001)),
        (PyRep::String("season_id".into()), PyRep::Integer(1)),
        (PyRep::String("seasonID".into()), PyRep::Integer(1)),
        (PyRep::String("season_points".into()), PyRep::Integer(10)),
        (PyRep::String("seasonPoints".into()), PyRep::Integer(10)),
        (PyRep::String("event_type".into()), PyRep::String("combat".into())),
        (
            PyRep::String("expiration_date".into()),
            PyRep::Integer(1_335_000_000_000_000_00),
        ),
        (
            PyRep::String("expirationDate".into()),
            PyRep::Integer(1_335_000_000_000_000_00),
        ),
        (PyRep::String("finish_date".into()), PyRep::None),
        (
            PyRep::String("finish_date".into()),
            PyRep::Integer(1_335_000_000_000_000_00),
        ),
        (PyRep::String("is_expired".into()), PyRep::Bool(false)),
        (PyRep::String("progress".into()), PyRep::Integer(0)),
        (PyRep::String("max_progress".into()), PyRep::Integer(1)),
        (PyRep::String("maxProgress".into()), PyRep::Integer(1)),
        (PyRep::String("message_text".into()), PyRep::None),
        (PyRep::String("message_text".into()), PyRep::Integer(1)),
        (PyRep::String("progress_text".into()), PyRep::None),
        (PyRep::String("progress_text".into()), PyRep::Integer(1)),
        (PyRep::String("name".into()), PyRep::None),
        (PyRep::String("name".into()), PyRep::Integer(1)),
        (PyRep::String("challenge_category".into()), PyRep::String("PvE".into())),
        (PyRep::String("challenge_security".into()), PyRep::String(String::new())),
        (PyRep::String("points_awarded".into()), PyRep::Integer(10)),
        (PyRep::String("isk_awarded".into()), PyRep::Integer(0)),
        (PyRep::String("is_coop".into()), PyRep::Bool(false)),
        (PyRep::String("expiration_minutes".into()), PyRep::Integer(0)),
        (
            PyRep::String("received_date".into()),
            PyRep::Integer(1_335_000_000_000_000_00),
        ),
        (
            PyRep::String("start_date".into()),
            PyRep::Integer(1_335_000_000_000_000_00),
        ),
        (PyRep::String("should_be_active_on_season_start".into()), PyRep::Bool(true)),
        (PyRep::String("should_only_appear_once_per_season".into()), PyRep::Bool(false)),
        (PyRep::String("next_challenge".into()), PyRep::None),
        (PyRep::String("next_challenge".into()), PyRep::Integer(44)),
        (PyRep::String("is_chained".into()), PyRep::Bool(false)),
        (PyRep::String("should_not_expire_at_downtime".into()), PyRep::Bool(false)),
        (PyRep::String("is_dormant".into()), PyRep::Bool(false)),
        (PyRep::String("isDormant".into()), PyRep::Bool(false)),
        (PyRep::String("reward_type".into()), PyRep::None),
        (PyRep::String("reward_type".into()), PyRep::Integer(34)),
        (PyRep::String("reward_amount".into()), PyRep::Integer(0)),
        (PyRep::String("reward_date".into()), PyRep::None),
        (
            PyRep::String("reward_date".into()),
            PyRep::Integer(1_335_000_000_000_000_00),
        ),
        (PyRep::String("reward_type_soul_bound".into()), PyRep::Bool(false)),
        (PyRep::String("title".into()), PyRep::String("Challenge".into())),
        (PyRep::String("description".into()), PyRep::String(String::new())),
        (PyRep::String("points".into()), PyRep::Integer(10)),
        (PyRep::String("completed".into()), PyRep::Bool(false)),
        (PyRep::String("claimed".into()), PyRep::Bool(false)),
    ]);
    PyRep::ObjectEx {
        is_type_2: true,
        header: Box::new(PyRep::Tuple(vec![
            PyRep::Tuple(vec![PyRep::Token("seasons.common.challenge.Challenge".into())]),
            state_dict,
        ])),
        list: Vec::new(),
        dict: Vec::new(),
    }
}

fn sample_dbrow_item() -> PyRep {
    eve_proto::DBRowDescriptor::inv_item_header().build_dbrow(vec![
        PyRep::Integer(100_000_001),
        PyRep::Integer(34),
        PyRep::Integer(90_000_001),
        PyRep::Integer(60_003_760),
        PyRep::Integer(4),
        PyRep::Integer(10),
        PyRep::Integer(18),
        PyRep::Integer(4),
        PyRep::String(String::new()),
    ])
}

fn sample_session_change_dict() -> PyRep {
    // Session change dicts emit `(old, new)` transitions across nullable integer/string/bool keys
    // (`router/session_transition.rs:1578-1632` and `SessionMgr.OnSessionChanged`).
    let mut entries = Vec::new();
    for key in [
        "solarsystemid2",
        "solarsystemid",
        "stationid",
        "structureid",
        "shipid",
        "locationid",
        "charid",
        "corpid",
        "allianceid",
        "userid",
        "constellationid",
        "regionid",
        "fleetid",
        "fleetrole",
        "wingid",
        "squadid",
        "warfactionid",
        "corprole",
        "rolesAtAll",
        "rolesAtHQ",
        "rolesAtBase",
        "rolesAtOther",
        "role",
        "hqid",
        "hqID",
        "baseID",
        "cloneStationID",
        "maxSessionTime",
        "userType",
        "corpAccountKey",
        "raceID",
        "bloodlineID",
        "genderID",
    ] {
        entries.push((
            PyRep::String(key.into()),
            PyRep::Tuple(vec![PyRep::Integer(30_000_142), PyRep::Integer(30_000_144)]),
        ));
        entries.push((
            PyRep::String(key.into()),
            PyRep::Tuple(vec![PyRep::None, PyRep::Integer(60_003_760)]),
        ));
        entries.push((
            PyRep::String(key.into()),
            PyRep::Tuple(vec![PyRep::Integer(60_003_760), PyRep::None]),
        ));
        // Undock / transition clears or re-emits nullable session keys with `(None, None)`
        // (`router/session_transition.rs:1592`).
        entries.push((
            PyRep::String(key.into()),
            PyRep::Tuple(vec![PyRep::None, PyRep::None]),
        ));
    }
    for key in ["languageID", "address"] {
        entries.push((
            PyRep::String(key.into()),
            PyRep::Tuple(vec![PyRep::None, PyRep::ascii_bytes("EN")]),
        ));
        entries.push((
            PyRep::String(key.into()),
            PyRep::Tuple(vec![PyRep::ascii_bytes("EN"), PyRep::ascii_bytes("DE")]),
        ));
        entries.push((
            PyRep::String(key.into()),
            PyRep::Tuple(vec![PyRep::None, PyRep::String("EN".into())]),
        ));
        entries.push((
            PyRep::String(key.into()),
            PyRep::Tuple(vec![PyRep::String("EN".into()), PyRep::String("DE".into())]),
        ));
    }
    entries.push((
        PyRep::String("charname".into()),
        PyRep::Tuple(vec![PyRep::None, PyRep::String("Pilot".into())]),
    ));
    entries.push((
        PyRep::String("flag".into()),
        PyRep::Tuple(vec![PyRep::Bool(false), PyRep::Bool(true)]),
    ));
    PyRep::Dict(entries)
}

fn sample_session_kv_variants() -> Vec<Vec<PyRep>> {
    let change = sample_session_change_dict();
    let i = || PyRep::Integer(1);
    let s = || PyRep::String("en".into());
    let n = || PyRep::None;
    let schemas: Vec<(Vec<(&'static str, PyRep)>, Vec<(&'static str, PyRep)>)> = vec![
        // 24-key `macho_cluster_mgmt::session_state_to_keyval`
        (
            vec![
                ("userid", i()), ("charid", i()), ("corpid", i()), ("allianceid", i()),
                ("stationid", i()), ("structureid", i()), ("locationid", i()),
                ("solarsystemid", i()), ("solarsystemid2", i()), ("constellationid", i()),
                ("regionid", i()), ("shipid", i()), ("raceID", i()), ("bloodlineID", i()),
                ("genderID", i()), ("corprole", i()), ("corpAccountKey", i()), ("fleetid", i()),
                ("warfactionid", i()), ("role", i()), ("languageID", s()), ("address", s()),
                ("userType", i()), ("sessionID", i()),
            ],
            vec![
                ("userid", n()), ("charid", i()), ("corpid", n()), ("allianceid", n()),
                ("stationid", n()), ("structureid", n()), ("locationid", n()),
                ("solarsystemid", n()), ("solarsystemid2", n()), ("constellationid", n()),
                ("regionid", n()), ("shipid", n()), ("raceID", n()), ("bloodlineID", n()),
                ("genderID", n()), ("corprole", i()), ("corpAccountKey", i()), ("fleetid", n()),
                ("warfactionid", n()), ("role", i()), ("languageID", s()), ("address", s()),
                ("userType", i()), ("sessionID", i()),
            ],
        ),
        // 18-key `fleet_social_corp_tail`
        (
            vec![
                ("userid", i()), ("charid", i()), ("corpid", i()), ("allianceid", i()),
                ("corprole", i()), ("rolesAtAll", i()), ("rolesAtHQ", i()),
                ("rolesAtBase", i()), ("rolesAtOther", i()), ("shipid", i()),
                ("stationid", i()), ("structureid", i()), ("solarsystemid", i()),
                ("solarsystemid2", i()), ("constellationid", i()), ("regionid", i()),
                ("sessionChangeReason", s()), ("nextSessionChange", i()),
            ],
            vec![
                ("userid", i()), ("charid", i()), ("corpid", i()), ("allianceid", n()),
                ("corprole", i()), ("rolesAtAll", i()), ("rolesAtHQ", i()),
                ("rolesAtBase", i()), ("rolesAtOther", i()), ("shipid", n()),
                ("stationid", n()), ("structureid", n()), ("solarsystemid", n()),
                ("solarsystemid2", i()), ("constellationid", i()), ("regionid", i()),
                ("sessionChangeReason", s()), ("nextSessionChange", n()),
            ],
        ),
        // 15-key `router::session_transition::build_session_state_keyval`
        (
            vec![
                ("userid", i()), ("charid", i()), ("corpid", i()), ("allianceid", i()),
                ("shipid", i()), ("stationid", i()), ("structureid", i()),
                ("solarsystemid2", i()), ("solarsystemid", i()), ("locationid", i()),
                ("constellationid", i()), ("regionid", i()), ("raceID", i()),
                ("bloodlineID", i()), ("genderID", i()),
            ],
            vec![
                ("userid", n()), ("charid", i()), ("corpid", n()), ("allianceid", n()),
                ("shipid", n()), ("stationid", n()), ("structureid", n()),
                ("solarsystemid2", n()), ("solarsystemid", n()), ("locationid", n()),
                ("constellationid", n()), ("regionid", n()), ("raceID", n()),
                ("bloodlineID", n()), ("genderID", n()),
            ],
        ),
        // 12-key `air_npe_*` / `structure_lifecycle:1208`
        (
            vec![
                ("userid", i()), ("charid", i()), ("corpid", i()), ("allianceid", i()),
                ("shipid", i()), ("stationid", i()), ("structureid", i()),
                ("solarsystemid2", i()), ("solarsystemid", i()), ("locationid", i()),
                ("constellationid", i()), ("regionid", i()),
            ],
            vec![
                ("userid", n()), ("charid", i()), ("corpid", n()), ("allianceid", n()),
                ("shipid", n()), ("stationid", n()), ("structureid", n()),
                ("solarsystemid2", n()), ("solarsystemid", n()), ("locationid", n()),
                ("constellationid", n()), ("regionid", n()),
            ],
        ),
        // 10-key `router::session_transition:1632` (`nextSessionChange`)
        (
            vec![
                ("charid", i()), ("userid", i()), ("corpid", i()), ("allianceid", i()),
                ("stationid", i()), ("structureid", i()), ("solarsystemid2", i()),
                ("solarsystemid", i()), ("shipid", i()), ("nextSessionChange", i()),
            ],
            vec![
                ("charid", i()), ("userid", n()), ("corpid", n()), ("allianceid", n()),
                ("stationid", n()), ("structureid", n()), ("solarsystemid2", i()),
                ("solarsystemid", n()), ("shipid", n()), ("nextSessionChange", n()),
            ],
        ),
        // 9-key `pvp_filament_arena:753`
        (
            vec![
                ("charid", i()), ("userid", i()), ("corpid", i()), ("allianceid", i()),
                ("stationid", i()), ("structureid", i()), ("solarsystemid2", i()),
                ("solarsystemid", i()), ("shipid", i()),
            ],
            vec![
                ("charid", i()), ("userid", i()), ("corpid", i()), ("allianceid", n()),
                ("stationid", n()), ("structureid", n()), ("solarsystemid2", i()),
                ("solarsystemid", n()), ("shipid", i()),
            ],
        ),
        // 7-key `pvp_filament_arena:785`
        (
            vec![
                ("charid", i()), ("userid", i()), ("corpid", i()), ("allianceid", i()),
                ("solarsystemid2", i()), ("solarsystemid", i()), ("shipid", i()),
            ],
            vec![
                ("charid", i()), ("userid", i()), ("corpid", i()), ("allianceid", n()),
                ("solarsystemid2", i()), ("solarsystemid", i()), ("shipid", i()),
            ],
        ),
        // 7-key `space_cyno_filaments:858`
        (
            vec![
                ("charid", i()), ("stationid", i()), ("structureid", i()),
                ("solarsystemid2", i()), ("solarsystemid", i()), ("constellationid", i()),
                ("regionid", i()),
            ],
            vec![
                ("charid", i()), ("stationid", n()), ("structureid", n()),
                ("solarsystemid2", i()), ("solarsystemid", i()), ("constellationid", i()),
                ("regionid", i()),
            ],
        ),
        // 4-key `tourney_*`
        (
            vec![
                ("charid", i()), ("stationid", i()), ("solarsystemid2", i()),
                ("solarsystemid", i()),
            ],
            vec![
                ("charid", i()), ("stationid", n()), ("solarsystemid2", i()),
                ("solarsystemid", n()),
            ],
        ),
        // 4-key `pvp_filament_arena:1229`
        (
            vec![
                ("charid", i()), ("solarsystemid2", i()), ("solarsystemid", i()),
                ("matchID", i()),
            ],
            vec![
                ("charid", i()), ("solarsystemid2", i()), ("solarsystemid", i()),
                ("matchID", i()),
            ],
        ),
        // 3-key `fleet_social_notify:279`
        (
            vec![("charid", i()), ("fleetid", i()), ("fleetrole", i())],
            vec![("charid", i()), ("fleetid", n()), ("fleetrole", n())],
        ),
        // 3-key `pve_abyssal_lifecycle:698` / `pvp_filament_arena:1527`
        (
            vec![("charid", i()), ("solarsystemid2", i()), ("solarsystemid", i())],
            vec![("charid", i()), ("solarsystemid2", i()), ("solarsystemid", n())],
        ),
        // 2-key `fw_enlistment:223`
        (
            vec![("charid", i()), ("warfactionid", i())],
            vec![("charid", i()), ("warfactionid", n())],
        ),
        // 2-key `structure_control_ops:928` / `structure_lifecycle:971`
        (
            vec![("charid", i()), ("shipid", i())],
            vec![("charid", i()), ("shipid", n())],
        ),
    ];
    let mut out = vec![
        vec![PyRep::Bool(false), PyRep::None, change.clone()],
        vec![
            PyRep::Bool(false),
            PyRep::keyval([("charid", PyRep::Integer(90_000_001))]),
            change.clone(),
        ],
    ];
    for (all_some, all_none) in schemas {
        out.push(vec![PyRep::Bool(false), PyRep::keyval(all_some), change.clone()]);
        out.push(vec![PyRep::Bool(false), PyRep::keyval(all_none), change.clone()]);
    }
    out
}

fn sample_godma_env(char_id: PyRep, target_id: PyRep, charge_type_id: PyRep) -> PyRep {
    PyRep::Tuple(vec![
        PyRep::Integer(100_000_020),
        char_id,
        PyRep::Integer(100_000_010),
        target_id,
        charge_type_id,
        PyRep::List(Vec::new()),
        PyRep::Integer(10),
    ])
}

fn sample_skill_info_kv(level: i64, sp: f64) -> PyRep {
    PyRep::keyval([
        ("typeID", PyRep::Integer(3402)),
        ("skillLevel", PyRep::Integer(level)),
        ("trainedSkillLevel", PyRep::Integer(level)),
        ("effectiveSkillLevel", PyRep::Integer(level)),
        ("activeSkillLevel", PyRep::Integer(level)),
        ("virtualSkillLevel", PyRep::None),
        ("skillPoints", PyRep::Real(sp)),
        ("trainedSkillPoints", PyRep::Real(sp)),
        ("skillRank", PyRep::Real(1.0)),
    ])
}

fn sample_godma_ship_effect_variants() -> Vec<Vec<PyRep>> {
    // V24.02 `Godma.OnGodmaShipEffect`:
    // - 3-arg minimal `(itemID, effect_str_or_id, active_bool)` (`combat_weapons_spool.rs:82`,
    //   `tactical_modules_superweapon_ops.rs:1288`, `fitting_module_cycles_pending_actions.rs:101`).
    // - 10-arg (`cache_notifications.rs:1952`, `binding.rs:1837`) and 11-arg
    //   (`outbound_scene_scope.rs:865`, `mining_cycle.rs:871`) with `Integer(effect_id)` and 7-tuple
    //   `environment = (moduleID, charID|None, shipID, targetID|None, chargeTypeID|None, sublocs, effectID)`.
    let sim_ft = 1_335_000_000_000_000_00_i64;
    let mut out = vec![
        vec![
            PyRep::Integer(200_018_015),
            PyRep::String("online".into()),
            PyRep::Bool(false),
        ],
        vec![
            PyRep::Integer(200_018_015),
            PyRep::Integer(16),
            PyRep::Bool(true),
        ],
    ];
    let env_shapes = [
        sample_godma_env(PyRep::None, PyRep::None, PyRep::None),
        sample_godma_env(PyRep::Integer(90_000_001), PyRep::None, PyRep::None),
        sample_godma_env(PyRep::Integer(90_000_001), PyRep::Integer(100_000_099), PyRep::None),
        sample_godma_env(
            PyRep::Integer(90_000_001),
            PyRep::Integer(100_000_099),
            PyRep::Integer(219),
        ),
        sample_godma_env(PyRep::Integer(90_000_001), PyRep::None, PyRep::Integer(219)),
    ];
    for env in env_shapes {
        for dur in [PyRep::Real(1000.0), PyRep::Integer(1000)] {
            for err in [PyRep::None, PyRep::Integer(0)] {
                let base10 = vec![
                    PyRep::Integer(200_018_015),
                    PyRep::Integer(10),
                    PyRep::Integer(sim_ft),
                    PyRep::Integer(1),
                    PyRep::Integer(1),
                    env.clone(),
                    PyRep::Integer(sim_ft),
                    dur.clone(),
                    PyRep::Integer(0),
                    err,
                ];
                let mut with_stop_none = base10.clone();
                with_stop_none.push(PyRep::None);
                let mut with_stop_time = base10.clone();
                with_stop_time.push(PyRep::Integer(sim_ft));
                out.push(base10);
                out.push(with_stop_none);
                out.push(with_stop_time);
            }
        }
    }
    out
}

define_notifications! {
    DoSimClockRebase => "DoSimClockRebase" ("solarsystemid2") {
        fields: { times: (SimFileTime, SimFileTime) },
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
        samples: || vec![
            vec![
                PyRep::Integer(99_000_001),
                PyRep::Integer(98_000_001),
                PyRep::Dict(vec![
                    (
                        PyRep::String("allianceID".into()),
                        PyRep::Tuple(vec![PyRep::None, PyRep::Integer(99_000_001)]),
                    ),
                    (
                        PyRep::String("corporationID".into()),
                        PyRep::Tuple(vec![PyRep::None, PyRep::Integer(98_000_001)]),
                    ),
                    (
                        PyRep::String("applicationText".into()),
                        PyRep::Tuple(vec![PyRep::None, PyRep::String("Apply".into())]),
                    ),
                    (
                        PyRep::String("state".into()),
                        PyRep::Tuple(vec![PyRep::None, PyRep::String("Pending".into())]),
                    ),
                ]),
            ],
            vec![
                PyRep::Integer(99_000_001),
                PyRep::Integer(98_000_001),
                PyRep::Dict(vec![
                    (
                        PyRep::String("state".into()),
                        PyRep::Tuple(vec![
                            PyRep::String("Pending".into()),
                            PyRep::String("Accepted".into()),
                        ]),
                    ),
                    (
                        PyRep::String("joinEffectiveTime".into()),
                        PyRep::Tuple(vec![PyRep::None, PyRep::Integer(1_800_086_400)]),
                    ),
                ]),
            ],
        ],
    },
    OnAllianceChanged => "OnAllianceChanged" ("allianceid") {
        fields: { alliance_id: i64, change: PyRep },
        // `services/social_corp_fleet.rs:414,681,1300,1797` emits alliance creation, executor,
        // logo, and description/URL `(old, new)` transitions.
        samples: || vec![vec![
            PyRep::Integer(99_000_001),
            PyRep::Dict(vec![
                (
                    PyRep::String("allianceID".into()),
                    PyRep::Tuple(vec![PyRep::None, PyRep::Integer(99_000_001)]),
                ),
                (
                    PyRep::String("allianceName".into()),
                    PyRep::Tuple(vec![PyRep::None, PyRep::String("Alliance".into())]),
                ),
                (
                    PyRep::String("shortName".into()),
                    PyRep::Tuple(vec![PyRep::None, PyRep::String("ALLY".into())]),
                ),
                (
                    PyRep::String("executorCorpID".into()),
                    PyRep::Tuple(vec![PyRep::None, PyRep::Integer(98_000_001)]),
                ),
                (
                    PyRep::String("executorCorpID".into()),
                    PyRep::Tuple(vec![PyRep::Integer(98_000_001), PyRep::Integer(98_000_002)]),
                ),
                (
                    PyRep::String("shape1".into()),
                    PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(2)]),
                ),
                (
                    PyRep::String("shape2".into()),
                    PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(2)]),
                ),
                (
                    PyRep::String("shape3".into()),
                    PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(2)]),
                ),
                (
                    PyRep::String("color1".into()),
                    PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(2)]),
                ),
                (
                    PyRep::String("color2".into()),
                    PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(2)]),
                ),
                (
                    PyRep::String("color3".into()),
                    PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(2)]),
                ),
                (
                    PyRep::String("description".into()),
                    PyRep::Tuple(vec![PyRep::String(String::new()), PyRep::String("Desc".into())]),
                ),
                (
                    PyRep::String("url".into()),
                    PyRep::Tuple(vec![PyRep::String(String::new()), PyRep::String("https://example.invalid".into())]),
                ),
            ]),
        ]],
    },
    OnAllianceLogoReady => "OnAllianceLogoReady" ("allianceid") {
        fields: { alliance_id: i64, size: i64 },
        samples: || vec![vec![PyRep::Integer(99_000_001), PyRep::Integer(128)]],
    },
    OnAllianceMemberChanged => "OnAllianceMemberChanged" ("allianceid") {
        fields: { alliance_id: i64, corp_id: i64, change: PyRep },
        samples: || vec![
            vec![
                PyRep::Integer(99_000_001),
                PyRep::Integer(98_000_001),
                PyRep::Dict(vec![
                    (
                        PyRep::String("allianceID".into()),
                        PyRep::Tuple(vec![PyRep::None, PyRep::Integer(99_000_001)]),
                    ),
                    (
                        PyRep::String("corporationID".into()),
                        PyRep::Tuple(vec![PyRep::None, PyRep::Integer(98_000_001)]),
                    ),
                ]),
            ],
            vec![
                PyRep::Integer(99_000_001),
                PyRep::Integer(98_000_001),
                PyRep::Dict(vec![
                    (
                        PyRep::String("allianceID".into()),
                        PyRep::Tuple(vec![PyRep::Integer(99_000_001), PyRep::None]),
                    ),
                    (
                        PyRep::String("corporationID".into()),
                        PyRep::Tuple(vec![PyRep::Integer(98_000_001), PyRep::None]),
                    ),
                ]),
            ],
        ],
    },
    OnAllianceRelationshipChanged => "OnAllianceRelationshipChanged" ("allianceid") {
        fields: { alliance_id: i64, to_id: i64, change: PyRep },
        samples: || vec![
            vec![
                PyRep::Integer(99_000_001),
                PyRep::Integer(99_000_002),
                PyRep::Dict(vec![
                    (
                        PyRep::String("toID".into()),
                        PyRep::Tuple(vec![PyRep::None, PyRep::Integer(99_000_002)]),
                    ),
                    (
                        PyRep::String("relationshipID".into()),
                        PyRep::Tuple(vec![PyRep::None, PyRep::Real(-10.0)]),
                    ),
                    (
                        PyRep::String("labelMask".into()),
                        PyRep::Tuple(vec![PyRep::None, PyRep::Integer(4)]),
                    ),
                ]),
            ],
            vec![
                PyRep::Integer(99_000_001),
                PyRep::Integer(99_000_002),
                PyRep::Dict(vec![
                    (
                        PyRep::String("toID".into()),
                        PyRep::Tuple(vec![PyRep::Integer(99_000_002), PyRep::Integer(99_000_002)]),
                    ),
                    (
                        PyRep::String("relationshipID".into()),
                        PyRep::Tuple(vec![PyRep::Real(-10.0), PyRep::Real(5.0)]),
                    ),
                    (
                        PyRep::String("labelMask".into()),
                        PyRep::Tuple(vec![PyRep::Integer(4), PyRep::Integer(8)]),
                    ),
                ]),
            ],
            vec![
                PyRep::Integer(99_000_001),
                PyRep::Integer(99_000_002),
                PyRep::Dict(vec![
                    (
                        PyRep::String("toID".into()),
                        PyRep::Tuple(vec![PyRep::Integer(99_000_002), PyRep::None]),
                    ),
                    (
                        PyRep::String("relationshipID".into()),
                        PyRep::Tuple(vec![PyRep::Real(5.0), PyRep::None]),
                    ),
                ]),
            ],
            vec![
                PyRep::Integer(99_000_001),
                PyRep::Integer(99_000_002),
                PyRep::Dict(vec![
                    (
                        PyRep::String("toID".into()),
                        PyRep::Tuple(vec![PyRep::Integer(99_000_002), PyRep::None]),
                    ),
                    (
                        PyRep::String("relationshipID".into()),
                        PyRep::Tuple(vec![PyRep::None, PyRep::None]),
                    ),
                ]),
            ],
        ],
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
        fields: { beans: Vec<BeanPrimeItem> },
        samples: || vec![vec![PyRep::List(vec![BeanPrimeItem {
            error_id: 12_345,
            error_key_hash: 0x1234_5678,
            log_mode: "B".into(),
        }
        .into_notify_arg()])]],
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
        // `services/notify_emitters_skills_trade_godma.rs:1122` & `notify_emitters_space_dungeons.rs:112`.
        samples: || vec![vec![
            PyRep::String("evelocations".into()),
            PyRep::List(vec![
                PyRep::Integer(60_003_760),
                PyRep::String("Station".into()),
                PyRep::Integer(30_000_142),
                PyRep::Real(0.0),
                PyRep::None,
            ]),
        ]],
    },
    OnChallengeCompleted => "OnChallengeCompleted" ("charid") {
        fields: { challenge_id: i64 },
        samples: || vec![vec![PyRep::Integer(42)]],
    },
    OnChallengeExpired => "OnChallengeExpired" ("charid") {
        fields: { challenge_id: i64, new_challenge: PyRep },
        // `services/season_manager.rs:74` emits `seasons.common.challenge.Challenge` `ObjectEx`.
        samples: || vec![
            vec![PyRep::Integer(42), sample_season_challenge_object()],
            vec![PyRep::Integer(42), PyRep::keyval([("challengeID", PyRep::Integer(43))])],
        ],
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
        // `XmppChatChannels.OnChannelLeft(channel)` (`services/fleet_social_notify.rs:396`)
        // emits bare string channel IDs; legacy tuple form still normalized by `client_compat`.
        samples: || vec![
            vec![PyRep::Tuple(vec![PyRep::String("fleet".into()), PyRep::Integer(1)])],
            vec![PyRep::String("fleet_1".into())],
        ],
    },
    OnCharNoLongerInStation => "OnCharNoLongerInStation" ("charid") {
        fields: { rec: PyRep },
        // `allianceID` and `warFactionID` are nullable (`None` or `Integer`, `router/dispatch.rs:445-455`).
        samples: || vec![
            vec![PyRep::Tuple(vec![
                PyRep::Integer(90_000_001),
                PyRep::Integer(98_000_001),
                PyRep::None,
                PyRep::Integer(0),
            ])],
            vec![PyRep::Tuple(vec![
                PyRep::Integer(90_000_001),
                PyRep::Integer(98_000_001),
                PyRep::None,
                PyRep::None,
            ])],
            vec![PyRep::Tuple(vec![
                PyRep::Integer(90_000_001),
                PyRep::Integer(98_000_001),
                PyRep::Integer(99_000_001),
                PyRep::Integer(500_001),
            ])],
            vec![PyRep::Tuple(vec![
                PyRep::Integer(90_000_001),
                PyRep::Integer(98_000_001),
                PyRep::Integer(99_000_001),
                PyRep::None,
            ])],
        ],
    },
    OnCharNowInStation => "OnCharNowInStation" ("charid") {
        fields: { rec: PyRep },
        // `allianceID` and `warFactionID` are nullable (`None` or `Integer`, `router/dispatch.rs:445-455`).
        samples: || vec![
            vec![PyRep::Tuple(vec![
                PyRep::Integer(90_000_001),
                PyRep::Integer(98_000_001),
                PyRep::None,
                PyRep::Integer(0),
            ])],
            vec![PyRep::Tuple(vec![
                PyRep::Integer(90_000_001),
                PyRep::Integer(98_000_001),
                PyRep::None,
                PyRep::None,
            ])],
            vec![PyRep::Tuple(vec![
                PyRep::Integer(90_000_001),
                PyRep::Integer(98_000_001),
                PyRep::Integer(99_000_001),
                PyRep::Integer(500_001),
            ])],
            vec![PyRep::Tuple(vec![
                PyRep::Integer(90_000_001),
                PyRep::Integer(98_000_001),
                PyRep::Integer(99_000_001),
                PyRep::None,
            ])],
        ],
    },
    OnCharacterPermanentlyBanned => "OnCharacterPermanentlyBanned" ("charid") {
        samples: || vec![vec![]],
    },
    OnCharacterPokedByGM => "OnCharacterPokedByGM" ("charid") {
        fields: { gm_char_id: i64, channel_name: String },
        samples: || vec![vec![PyRep::Integer(90_000_002), PyRep::String("Help".into())]],
    },
    OnCharacterTemporarilyBanned => "OnCharacterTemporarilyBanned" ("charid") {
        fields: { ban_expiry: WallFileTime },
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
        // `services/mobile_deployables_ext.rs:1412-1418` passes `shipID`/`moduleID` formatting kwargs.
        samples: || vec![vec![
            PyRep::String("CantCloakCloseToEntity".into()),
            PyRep::Dict(vec![
                (PyRep::String("distance".into()), PyRep::Real(1500.0)),
                (PyRep::String("shipID".into()), PyRep::Integer(100_000_010)),
                (PyRep::String("moduleID".into()), PyRep::Integer(100_000_020)),
            ]),
        ]],
    },
    OnClusterShutdownCancelled => "OnClusterShutdownCancelled" ("clientID") {
        fields: { explanation_label: String },
        samples: || vec![vec![PyRep::String("UI/System/ShutdownCancelled".into())]],
    },
    OnClusterShutdownInitiated => "OnClusterShutdownInitiated" ("clientID") {
        fields: { explanation_label: String, when: WallFileTime, duration: i64 },
        samples: || vec![vec![
            PyRep::String("UI/System/ShutdownInitiated".into()),
            PyRep::Integer(1_335_000_000_000_000_00),
            PyRep::Integer(300),
        ]],
    },
    OnCombatMessage => "OnCombatMessage" ("charid") {
        fields: { message_name: String, message_args: PyRep },
        // `services/notify_emitters_combat_ess_progression.rs:117` & `notify_emitters_ess_bounties.rs:167`.
        samples: || vec![vec![
            PyRep::String("Hit".into()),
            PyRep::Dict(vec![
                (PyRep::String("specialObject".into()), PyRep::Integer(100_000_010)),
                (PyRep::String("damage".into()), PyRep::Real(42.5)),
                (PyRep::String("type".into()), PyRep::Integer(561)),
                (PyRep::String("weapon".into()), PyRep::Integer(561)),
                (PyRep::String("hitQualityText".into()), PyRep::String("Wrecks".into())),
                (PyRep::String("isWrecking".into()), PyRep::Bool(false)),
            ]),
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
        // `services/notify_emitters_character_services.rs:258` emits offerID, purchasedAt, confirmed.
        samples: || vec![vec![
            PyRep::Integer(101),
            PyRep::Dict(vec![
                (PyRep::String("offerID".into()), PyRep::Integer(101)),
                (
                    PyRep::String("purchasedAt".into()),
                    PyRep::Integer(1_335_000_000_000_000_00),
                ),
                (PyRep::String("confirmed".into()), PyRep::Bool(true)),
            ]),
        ]],
    },
    OnCorpWalletBillsChanged => "OnCorpWalletBillsChanged" ("corpid") {
        samples: || vec![vec![]],
    },
    OnCosmicAnomalyAdded => "OnCosmicAnomalyAdded" ("solarsystemid") {
        fields: { anomaly: PyRep },
        // `notify_emitters_tournaments_anomalies.rs:128-135` (with `dungeonNameID`) and
        // `notify_emitters_space_dungeons.rs:267-273` (without `dungeonNameID`).
        samples: || vec![
            vec![PyRep::keyval([
                ("targetID", PyRep::String("ABC-123".into())),
                ("instanceID", PyRep::Integer(910_000_001)),
                ("dungeonID", PyRep::Integer(100)),
                ("dungeonName", PyRep::String("Combat Anomaly".into())),
                ("solarSystemID", PyRep::Integer(30_000_142)),
            ])],
            vec![PyRep::keyval([
                ("targetID", PyRep::String("ABC-123".into())),
                ("instanceID", PyRep::Integer(910_000_001)),
                ("dungeonID", PyRep::Integer(100)),
                ("dungeonName", PyRep::String("Combat Anomaly".into())),
                ("dungeonNameID", PyRep::None),
                ("solarSystemID", PyRep::Integer(30_000_142)),
            ])],
            vec![PyRep::keyval([
                ("targetID", PyRep::String("ABC-123".into())),
                ("instanceID", PyRep::Integer(910_000_001)),
                ("dungeonID", PyRep::Integer(100)),
                ("dungeonName", PyRep::String("Combat Anomaly".into())),
                ("dungeonNameID", PyRep::Integer(1_000)),
                ("solarSystemID", PyRep::Integer(30_000_142)),
            ])],
        ],
    },
    OnCrateLootSpawned => "OnCrateLootSpawned" ("charid") {
        samples: || vec![vec![]],
    },
    OnCrimewatchEngagementCreated => "OnCrimewatchEngagementCreated" ("charid") {
        fields: { other_char_id: i64, timeout: WallFileTime },
        samples: || vec![vec![PyRep::Integer(90_000_002), PyRep::Integer(1_335_000_000_000_000_00)]],
    },
    OnCrimewatchEngagementEnded => "OnCrimewatchEngagementEnded" ("charid") {
        fields: { other_char_id: i64 },
        samples: || vec![vec![PyRep::Integer(90_000_002)]],
    },
    OnCrimewatchEngagementUpdated => "OnCrimewatchEngagementUpdated" ("charid") {
        fields: { other_char_id: i64, timeout: CrimewatchEngagementTimeout },
        optional: { do_alert: Option<bool> },
        samples: || vec![
            vec![PyRep::Integer(90_000_002), PyRep::Integer(1_335_000_000_000_000_00)],
            vec![PyRep::Integer(90_000_002), PyRep::Integer(1_335_000_000_000_000_00), PyRep::Bool(true)],
            vec![PyRep::Integer(90_000_002), PyRep::Bool(true)],
            vec![PyRep::Integer(90_000_002), PyRep::Bool(true), PyRep::Bool(true)],
        ],
    },
    OnCriminalTimerUpdate => "OnCriminalTimerUpdate" ("charid") {
        fields: { state: i64, expiry_time: Option<SimFileTime> },
        optional: { do_alert: Option<bool> },
        // `services/notify_emitters_fleet_combat.rs:184` emits `expiry_time = None` on timer expiry.
        samples: || vec![
            vec![PyRep::Integer(301), PyRep::Integer(1_335_000_000_000_000_00)],
            vec![PyRep::Integer(300), PyRep::None],
            vec![PyRep::Integer(301), PyRep::Integer(1_335_000_000_000_000_00), PyRep::Bool(true)],
            vec![PyRep::Integer(300), PyRep::None, PyRep::Bool(false)],
        ],
    },
    OnDamageStateChange => "OnDamageStateChange" ("charid") @ballpark {
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
        // Canonical V24 `DeathzoneSvc.OnDeathzoneStateUpdate` passes `None` or tracking `KeyVal`
        // (`services/notify_emitters_tournaments_anomalies.rs:1024`, `space_navigation_advanced.rs:1058`).
        samples: || vec![
            vec![
                PyRep::Integer(32_000_001),
                PyRep::Integer(100_000_010),
                PyRep::None,
            ],
            vec![
                PyRep::Integer(32_000_001),
                PyRep::Integer(100_000_010),
                PyRep::keyval([
                    ("damageStartTimestamp", PyRep::Integer(1_335_000_000_000_000_00)),
                    ("damageMaxTimestamp", PyRep::Integer(1_335_000_150_000_000_00)),
                    ("dangerLevel", PyRep::Real(1.0)),
                ]),
            ],
        ],
    },
    OnDestinationChange => "OnDestinationChange" ("charid") {
        fields: { solar_system_id: i64, clear_other_waypoints: bool, first: bool },
        samples: || vec![vec![PyRep::Integer(30_000_142), PyRep::Bool(true), PyRep::Bool(true)]],
    },
    OnDisapprovalTimerUpdate => "OnDisapprovalTimerUpdate" ("charid") {
        fields: { state: i64, expiry_time: Option<SimFileTime> },
        optional: { do_alert: Option<bool> },
        // `services/crimewatch_timers.rs:341` emits `expiry_time = None` on timer expiry.
        samples: || vec![
            vec![PyRep::Integer(501), PyRep::Integer(1_335_000_000_000_000_00)],
            vec![PyRep::Integer(500), PyRep::None],
            vec![PyRep::Integer(501), PyRep::Integer(1_335_000_000_000_000_00), PyRep::Bool(true)],
            vec![PyRep::Integer(500), PyRep::None, PyRep::Bool(false)],
        ],
    },
    OnDuelChallenge => "OnDuelChallenge" ("charid") {
        fields: {
            from_char_id: i64,
            from_corp_id: i64,
            from_alliance_id: Option<i64>,
            expiry_time: WallFileTime,
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
                ("instanceID", PyRep::Integer(1)),
                ("dungeonID", PyRep::Integer(100)),
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
        // Accepts both integer and string sound IDs (`notify_emitters_tournaments_anomalies.rs:451`).
        samples: || vec![
            vec![PyRep::Integer(42), PyRep::Integer(7), PyRep::Integer(100_000_010)],
            vec![PyRep::Integer(42), PyRep::String("amb".into()), PyRep::Integer(100_000_010)],
        ],
    },
    OnDungeonTriggerAudio => "OnDungeonTriggerAudio" ("charid") {
        fields: { dungeon_id: i64, audio_url: String },
        samples: || vec![vec![PyRep::Integer(42), PyRep::String("msg_audio_play".into())]],
    },
    OnDungeonTriggerMessage => "OnDungeonTriggerMessage" ("charid") {
        fields: { message_type: i64, message_id: i64 },
        // Accepts both integer and string message types (`OnDungeonTriggerMessageStr`).
        samples: || vec![
            vec![PyRep::Integer(1), PyRep::Integer(5001)],
            vec![PyRep::String("warn".into()), PyRep::Integer(5001)],
        ],
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
        // `Godma.OnEffectMessage` forwards formatting kwargs (`Integer`, `Real`, `String`)
        // from `services/mining_cycle.rs:376` and `services/notify_emitters_missing.rs:16`.
        samples: || vec![vec![
            PyRep::String("CustomError".into()),
            PyRep::Dict(vec![
                (PyRep::String("k".into()), PyRep::Integer(1)),
                (PyRep::String("detail".into()), PyRep::String("msg".into())),
                (PyRep::String("effectname".into()), PyRep::String("mining".into())),
                (PyRep::String("need".into()), PyRep::Real(10.0)),
                (PyRep::String("got".into()), PyRep::Real(5.0)),
                (PyRep::String("moduleID".into()), PyRep::Integer(100_000_001)),
            ]),
        ]],
    },
    OnEnterTournamentSystem => "OnEnterTournamentSystem" ("charid") {
        samples: || vec![vec![]],
    },
    OnEpicJournalChange => "OnEpicJournalChange" ("charid") {
        samples: || vec![vec![]],
    },
    OnEventResponseByExternal => "OnEventResponseByExternal" ("charid") {
        fields: { event_id: i64, event_kv: PyRep, response: i64 },
        // `services/mail_notify.rs:211-225`.
        samples: || vec![vec![
            PyRep::Integer(55),
            PyRep::keyval([
                ("eventID", PyRep::Integer(55)),
                ("ownerID", PyRep::Integer(90_000_001)),
                ("eventDateTime", PyRep::Integer(1_335_000_000_000_000_00)),
                ("eventDuration", PyRep::Integer(60)),
                ("eventTitle", PyRep::String("Event".into())),
                ("importance", PyRep::Integer(0)),
                ("flag", PyRep::Integer(1)),
                ("isDeleted", PyRep::Bool(false)),
            ]),
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
        samples: || {
            let opt_outs = PyRep::keyval([
                ("acceptsFleetWarp", PyRep::Bool(true)),
                ("acceptsConduitJumps", PyRep::Bool(true)),
                ("acceptsFleetRegroups", PyRep::Bool(true)),
                ("acceptsRegroup", PyRep::Bool(true)),
            ]);
            vec![
                vec![PyRep::keyval([
                    ("charID", PyRep::Integer(90_000_001)),
                    ("wingID", PyRep::Integer(1)),
                    ("squadID", PyRep::Integer(2)),
                    ("role", PyRep::Integer(1)),
                    ("job", PyRep::Integer(2)),
                    ("memberOptOuts", opt_outs.clone()),
                ])],
                vec![PyRep::keyval([
                    ("charID", PyRep::Integer(90_000_001)),
                    ("wingID", PyRep::None),
                    ("squadID", PyRep::None),
                    ("role", PyRep::Integer(4)),
                    ("job", PyRep::Integer(0)),
                    ("memberOptOuts", opt_outs),
                ])],
            ]
        },
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
        // `services/fleet_social_notify.rs:95-120` sends `None` wing/squad for boss/unassigned roles
        // and KeyVal `{acceptsConduitJumps, acceptsFleetRegroups, acceptsFleetWarp, acceptsRegroup}` opt-outs.
        samples: || {
            let opt_outs_kv = || {
                PyRep::keyval([
                    ("acceptsConduitJumps", PyRep::Bool(true)),
                    ("acceptsFleetRegroups", PyRep::Bool(true)),
                    ("acceptsFleetWarp", PyRep::Bool(true)),
                    ("acceptsRegroup", PyRep::Bool(true)),
                ])
            };
            let row = |ow: PyRep, os: PyRep, oo: PyRep, nw: PyRep, ns: PyRep, no: PyRep, only: bool| {
                vec![
                    PyRep::Integer(90_000_001),
                    PyRep::Integer(1001),
                    ow,
                    os,
                    PyRep::Integer(4),
                    PyRep::Integer(0),
                    oo,
                    nw,
                    ns,
                    PyRep::Integer(4),
                    PyRep::Integer(0),
                    no,
                    PyRep::Bool(only),
                ]
            };
            let states = [
                (PyRep::None, PyRep::None),
                (PyRep::Integer(-1), PyRep::None),
                (PyRep::Integer(-1), PyRep::Integer(-1)),
            ];
            let mut samples = Vec::with_capacity(10);
            for (ow, os) in &states {
                for (nw, ns) in &states {
                    samples.push(row(
                        ow.clone(),
                        os.clone(),
                        opt_outs_kv(),
                        nw.clone(),
                        ns.clone(),
                        opt_outs_kv(),
                        false,
                    ));
                }
            }
            samples.push(row(
                PyRep::Integer(-1),
                PyRep::Integer(-1),
                PyRep::List(Vec::new()),
                PyRep::Integer(-1),
                PyRep::Integer(-1),
                PyRep::List(Vec::new()),
                true,
            ));
            samples
        },
    },
    OnFleetMotdChanged => "OnFleetMotdChanged" ("fleetid") {
        fields: { motd: String, reload_motd: bool },
        samples: || vec![vec![PyRep::String("Fleet MOTD".into()), PyRep::Bool(true)]],
    },
    OnFleetOptionsChanged => "OnFleetOptionsChanged" ("fleetid") {
        fields: { old_options: PyRep, options: PyRep },
        samples: || vec![
            vec![
                PyRep::keyval([
                    ("isFreeMove", PyRep::Bool(false)),
                    ("isRegistered", PyRep::Bool(false)),
                    ("autoJoinSquadID", PyRep::None),
                ]),
                PyRep::keyval([
                    ("isFreeMove", PyRep::Bool(true)),
                    ("isRegistered", PyRep::Bool(false)),
                    ("autoJoinSquadID", PyRep::Integer(2)),
                ]),
            ],
            vec![
                PyRep::keyval([
                    ("isFreeMove", PyRep::Bool(true)),
                    ("isRegistered", PyRep::Bool(true)),
                    ("autoJoinSquadID", PyRep::Integer(2)),
                ]),
                PyRep::keyval([
                    ("isFreeMove", PyRep::Bool(false)),
                    ("isRegistered", PyRep::Bool(false)),
                    ("autoJoinSquadID", PyRep::None),
                ]),
            ],
        ],
    },
    OnFleetRespawnPointsUpdate => "OnFleetRespawnPointsUpdate" ("fleetid") {
        fields: { respawn_points: PyRep },
        // `services/notify_emitters_character_services.rs:678-683` emits `KeyVal` respawn entries.
        samples: || vec![
            vec![PyRep::List(vec![PyRep::keyval([
                ("solarsystemID", PyRep::Integer(30_000_142)),
                ("extraInfo", PyRep::Integer(1_000_000_000_001)),
                ("locationID", PyRep::Integer(1_000_000_000_001)),
                ("typeID", PyRep::Integer(35_832)),
            ])])],
            vec![PyRep::List(Vec::new())],
        ],
    },
    OnFleetStateChange => "OnFleetStateChange" ("charid") {
        fields: { fleet_state: PyRep },
        samples: || {
            let damage_dict = PyRep::Dict(vec![(
                PyRep::Integer(90_000_001),
                PyRep::Tuple(vec![PyRep::Real(1.0), PyRep::Real(1.0), PyRep::Real(1.0)]),
            )]);
            let tags = PyRep::Dict(vec![(PyRep::Integer(100_000_010), PyRep::String("A".into()))]);
            vec![
                vec![PyRep::keyval([
                    ("targetTags", tags.clone()),
                    (
                        "options",
                        PyRep::keyval([
                            ("isFreeMove", PyRep::Bool(false)),
                            ("isRegistered", PyRep::Bool(false)),
                            ("autoJoinSquadID", PyRep::None),
                        ]),
                    ),
                    ("characterID", PyRep::Integer(90_000_001)),
                    ("shieldRatio", PyRep::Real(1.0)),
                    ("armorRatio", PyRep::Real(1.0)),
                    ("hullRatio", PyRep::Real(1.0)),
                    ("damageState", damage_dict.clone()),
                ])],
                vec![PyRep::keyval([
                    ("targetTags", tags),
                    (
                        "options",
                        PyRep::keyval([
                            ("isFreeMove", PyRep::Bool(true)),
                            ("isRegistered", PyRep::Bool(true)),
                            ("autoJoinSquadID", PyRep::Integer(2)),
                        ]),
                    ),
                    ("characterID", PyRep::Integer(90_000_001)),
                    ("shieldRatio", PyRep::Real(1.0)),
                    ("armorRatio", PyRep::Real(1.0)),
                    ("hullRatio", PyRep::Real(1.0)),
                    ("damageState", damage_dict),
                ])],
            ]
        },
    },
    OnGlobalConfigUpdated => "OnGlobalConfigUpdated" ("clientID") {
        fields: { key: String, val: PyRep },
        // `services/auth_cache.rs:223` & `DynamicResourceSvc.OnGlobalConfigUpdated` (`compareisnull`).
        samples: || vec![
            vec![PyRep::String("featureFlag".into()), PyRep::Bool(true)],
            vec![PyRep::String("featureFlag".into()), PyRep::String("enabled".into())],
            vec![PyRep::String("featureFlag".into()), PyRep::Integer(1)],
            vec![PyRep::String("featureFlag".into()), PyRep::Real(1.5)],
            vec![PyRep::String("featureFlag".into()), PyRep::None],
        ],
    },
    OnIncomingAgentMessage => "OnIncomingAgentMessage" ("charid") {
        fields: { agent_id: i64, message: PyRep },
        // `services/notify_emitters_character_services.rs:147-160`.
        samples: || vec![vec![
            PyRep::Integer(3_000_001),
            PyRep::Tuple(vec![
                PyRep::String("UI/Agents/Dialogue/StandardMissionAccepted".into()),
                PyRep::Dict(vec![
                    (PyRep::String("missionName".into()), PyRep::String("Agent Transmission".into())),
                    (PyRep::String("agentID".into()), PyRep::Integer(3_000_001)),
                ]),
            ]),
        ]],
    },
    OnIncomingTransmission => "OnIncomingTransmission" ("charid") {
        fields: { transmission: PyRep, is_agent_mission: bool },
        samples: || vec![vec![
            PyRep::keyval([
                ("header", PyRep::String("Agent Transmission".into())),
                ("text", PyRep::String("Mission details".into())),
                ("icon", PyRep::String("res:/UI/Texture/WindowIcons/agent.png".into())),
                ("agentID", PyRep::Integer(3_018_600)),
            ]),
            PyRep::Bool(true),
        ]],
    },
    OnIndustryMaterials => "OnIndustryMaterials" ("charid") {
        fields: { job_id: i64, materials: PyRep },
        // `services/notify_emitters_character_services.rs:497` & `delta_item_corp.rs:437` emit `{typeID: qty}`.
        samples: || vec![
            vec![
                PyRep::Integer(1),
                PyRep::Dict(vec![(PyRep::Integer(34), PyRep::Integer(1_000))]),
            ],
            vec![PyRep::Integer(1), PyRep::Dict(Vec::new())],
        ],
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
        fields: { item: PyRep, change: PyRep },
        optional: { location: Option<PyRep> },
        // `delta_projection` emits 2-arg `[DBRow, change_dict]` (`ixOwnerID=2, ixLocationID=3, ixFlag=4,
        // ixStackSize=9, ixSingleton=10`), padded on wire by `client_compat` (`location` may also be `None|KeyVal`).
        samples: || {
            let change_all = || {
                PyRep::Dict(vec![
                    (PyRep::Integer(2), PyRep::Integer(90_000_001)),
                    (PyRep::Integer(3), PyRep::Integer(60_003_760)),
                    (PyRep::Integer(4), PyRep::Integer(4)),
                    (PyRep::Integer(9), PyRep::Integer(1)),
                    (PyRep::Integer(10), PyRep::Integer(0)),
                ])
            };
            vec![
                vec![sample_dbrow_item(), change_all()],
                vec![sample_dbrow_item(), change_all(), PyRep::None],
                vec![sample_keyval_item(), change_all(), PyRep::None],
                vec![sample_keyval_item(), change_all()],
            ]
        },
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
            t: SimFileTime,
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
                    ("skillPoints", PyRep::Real(8_000.0)),
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
            sent_date: WallFileTime,
            to_character_ids: Vec<i64>,
            to_list_id: Option<i64>,
            to_corp_or_alliance_id: Option<i64>,
            title: String,
            status_mask: i64,
        },
        // `services/mail_notify.rs:84` varies `to_list_id` / `to_corp_or_alliance_id` (`Option<i64>`).
        samples: || {
            let row = |list_id: PyRep, corp_or_ally: PyRep| {
                vec![
                    PyRep::Integer(9001),
                    PyRep::Integer(90_000_001),
                    PyRep::Integer(1_335_000_000_000_000_00),
                    PyRep::List(vec![PyRep::Integer(90_000_002)]),
                    list_id,
                    corp_or_ally,
                    PyRep::String("Hello".into()),
                    PyRep::Integer(0),
                ]
            };
            vec![
                row(PyRep::None, PyRep::None),
                row(PyRep::Integer(7001), PyRep::None),
                row(PyRep::None, PyRep::Integer(98_000_001)),
                row(PyRep::Integer(7001), PyRep::Integer(98_000_001)),
            ]
        },
    },
    OnMailUpdatedByExternal => "OnMailUpdatedByExternal" ("charid") {
        fields: { message_ids: Vec<i64>, is_read: Option<bool>, new_label: Option<i64> },
        // `services/mail_notify.rs:121` updates read status and/or label mask independently.
        samples: || vec![
            vec![PyRep::List(vec![PyRep::Integer(9001)]), PyRep::Bool(true), PyRep::Integer(1)],
            vec![PyRep::List(vec![PyRep::Integer(9001)]), PyRep::Bool(true), PyRep::None],
            vec![PyRep::List(vec![PyRep::Integer(9001)]), PyRep::None, PyRep::Integer(1)],
            vec![PyRep::List(vec![PyRep::Integer(9001)]), PyRep::None, PyRep::None],
        ],
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
        // `notify_emitters_space_dungeons.rs:160` optionally includes `"amount": Real`.
        samples: || vec![vec![PyRep::Dict(vec![
            (PyRep::String("characterID".into()), PyRep::Integer(90_000_001)),
            (PyRep::String("linkID".into()), PyRep::Integer(1)),
            (PyRep::String("solarSystemID".into()), PyRep::Integer(30_000_142)),
            (PyRep::String("amount".into()), PyRep::Real(50_000_000.0)),
        ])]],
    },
    OnMainBankPlayerLinkDisconnected => "OnMainBankPlayerLinkDisconnected" ("charid") {
        fields: { data: PyRep },
        // Canonical V24 `DynamicResourceSvc.OnMainBankPlayerLinkDisconnected` emits String `reason`.
        samples: || vec![vec![PyRep::Dict(vec![
            (PyRep::String("characterID".into()), PyRep::Integer(90_000_001)),
            (PyRep::String("linkID".into()), PyRep::Integer(1)),
            (PyRep::String("reason".into()), PyRep::String("Completed".into())),
            (PyRep::String("solarSystemID".into()), PyRep::Integer(30_000_142)),
        ])]],
    },
    OnMatchMakingFailure => "OnMatchMakingFailure" ("charid") {
        fields: { errors: Vec<String> },
        // Accepts both string codes and numeric `OnMatchMakingFailureCodes` (`notify_emitters_space_dungeons.rs:1001`).
        samples: || vec![
            vec![PyRep::List(vec![PyRep::String("InvalidFleet".into())])],
            vec![PyRep::List(vec![PyRep::Integer(404)])],
        ],
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
            time: SimFileTime,
            new_value: f64,
            old_value: f64,
            effective_time: SimFileTime,
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
        samples: || vec![
            vec![PyRep::List(vec![PyRep::keyval([
                ("trainingTypeID", PyRep::Integer(3402)),
                ("trainingToLevel", PyRep::Integer(2)),
                ("trainingStartSP", PyRep::Real(250.0)),
                ("trainingDestinationSP", PyRep::Real(1_414.0)),
                ("durationSeconds", PyRep::Real(600.0)),
                ("queuePosition", PyRep::Integer(0)),
                ("trainingStartTime", PyRep::Integer(1_335_000_000_000_000_00)),
                ("trainingEndTime", PyRep::Integer(1_335_000_600_000_000_00)),
            ])])],
            vec![PyRep::List(vec![PyRep::keyval([
                ("trainingTypeID", PyRep::Integer(3402)),
                ("trainingToLevel", PyRep::Integer(2)),
                ("trainingStartSP", PyRep::Real(250.0)),
                ("trainingDestinationSP", PyRep::Real(1_414.0)),
                ("durationSeconds", PyRep::Real(600.0)),
                ("queuePosition", PyRep::Integer(0)),
                ("trainingStartTime", PyRep::None),
                ("trainingEndTime", PyRep::None),
            ])])],
        ],
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
        fields: { state: i64, expiry_time: Option<SimFileTime> },
        optional: { do_alert: Option<bool> },
        // `services/crimewatch_timers.rs:331` emits `expiry_time = None` on timer expiry.
        samples: || vec![
            vec![PyRep::Integer(401), PyRep::Integer(1_335_000_000_000_000_00)],
            vec![PyRep::Integer(401), PyRep::None],
            vec![PyRep::Integer(401), PyRep::Integer(1_335_000_000_000_000_00), PyRep::Bool(true)],
            vec![PyRep::Integer(400), PyRep::None, PyRep::Bool(false)],
        ],
    },
    OnOfferReceived => "OnOfferReceived" ("charid") {
        fields: { offer_id: i64, offer: PyRep },
        // `services/notify_emitters_missing2.rs:36-94` and its tests emit structured offer dicts.
        samples: || vec![vec![
            PyRep::Integer(101),
            PyRep::Dict(vec![
                (PyRep::String("offerID".into()), PyRep::Integer(101)),
                (PyRep::String("validTo".into()), PyRep::Integer(1_335_000_000_000_000_00)),
                (PyRep::String("validFrom".into()), PyRep::Integer(1_335_000_000_000_000_00)),
                (
                    PyRep::String("price".into()),
                    PyRep::Dict(vec![
                        (PyRep::String("original".into()), PyRep::String("$9.99".into())),
                        (PyRep::String("offer".into()), PyRep::String("$4.99".into())),
                        (PyRep::String("percentageOff".into()), PyRep::String("-50%".into())),
                    ]),
                ),
                (
                    PyRep::String("purchaseStep".into()),
                    PyRep::Dict(vec![
                        (PyRep::String("Title".into()), PyRep::String("Special Offer".into())),
                        (PyRep::String("Body".into()), PyRep::String("Limited Time Offer".into())),
                        (PyRep::String("Background".into()), PyRep::String("res:/bg.png".into())),
                        (PyRep::String("ButtonTitle".into()), PyRep::String("BUY".into())),
                        (PyRep::String("PurchaseURL".into()), PyRep::String("https://example.invalid".into())),
                    ]),
                ),
                (
                    PyRep::String("notification".into()),
                    PyRep::Dict(vec![
                        (PyRep::String("Title".into()), PyRep::String("New offer available".into())),
                        (PyRep::String("Body".into()), PyRep::String("Check out this offer".into())),
                        (PyRep::String("Type".into()), PyRep::Integer(1)),
                    ]),
                ),
                (
                    PyRep::String("tooltip".into()),
                    PyRep::Dict(vec![
                        (PyRep::String("header".into()), PyRep::String("Offer Details".into())),
                        (PyRep::String("content".into()), PyRep::String("Discounted bundle".into())),
                    ]),
                ),
                (PyRep::String("custom".into()), PyRep::Bool(true)),
                (PyRep::String("customKey".into()), PyRep::Integer(99)),
            ]),
        ]],
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
        // `services/notify_emitters_operations_nodegraph.rs:330` sets element 3 to `Some(next_task)` or `None`.
        samples: || vec![vec![
            PyRep::List(vec![
                PyRep::Tuple(vec![
                    PyRep::Integer(1),
                    PyRep::Integer(10),
                    PyRep::Integer(100),
                    PyRep::None,
                ]),
                PyRep::Tuple(vec![
                    PyRep::Integer(1),
                    PyRep::Integer(10),
                    PyRep::Integer(100),
                    PyRep::Integer(101),
                ]),
            ]),
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
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(90_000_002),
                PyRep::Real(5.0),
                PyRep::Integer(0),
            ])]),
        ]],
    },
    OnPasswordChanged => "OnPasswordChanged" ("charid") {
        fields: { which: i64, container_item: PyRep },
        // `services/notify_emitters_fleet_combat.rs:57` emits `(which, KeyVal { itemID, typeID })`
        // for `invCache.RemoveInventoryContainer`.
        samples: || vec![
            vec![PyRep::Integer(1), sample_keyval_item()],
            vec![
                PyRep::Integer(1),
                PyRep::keyval([
                    ("itemID", PyRep::Integer(100_000_010)),
                    ("typeID", PyRep::Integer(17366)),
                ]),
            ],
        ],
    },
    OnPlayerPodDeath => "OnPlayerPodDeath" ("charid") {
        samples: || vec![vec![]],
    },
    OnPlayerPodRespawn => "OnPlayerPodRespawn" ("charid") {
        samples: || vec![vec![]],
    },
    OnPlayerStateUpdated => "OnPlayerStateUpdated" ("charid") {
        fields: { player_state: PyRep },
        // `services/project_discovery_rewards.rs:331-342` emits `util.KeyVal` read by `Rewards.OnPlayerStateUpdated`.
        samples: || vec![vec![PyRep::keyval([
            ("rank", PyRep::Integer(1)),
            ("experience", PyRep::Real(100.0)),
            ("totalXP", PyRep::Real(100.0)),
            ("accuracy", PyRep::Real(0.9)),
            ("score", PyRep::Real(0.9)),
            ("analysisKredits", PyRep::Integer(10)),
            ("finishedTutorial", PyRep::Bool(true)),
        ])]],
    },
    OnPlexDepositFailed => "OnPlexDepositFailed" ("charid") {
        fields: { error_key: String, reference: PyRep },
        samples: || vec![vec![PyRep::String("InsufficientFunds".into()), PyRep::Integer(1)]],
    },
    OnPlexWithdrawFailed => "OnPlexWithdrawFailed" ("charid") {
        fields: { error_key: String, reference: PyRep },
        // `vgs/common/listeners.pyj` compares `reference` (`Integer` or `String`).
        samples: || vec![
            vec![PyRep::String("InsufficientFunds".into()), PyRep::Integer(1)],
            vec![PyRep::String("VaultLocked".into()), PyRep::String("ref-2".into())],
        ],
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
        // Canonical V24 `ProgressionService.OnProgressionTaskWidgetUpdate` emits Dict `widget_state`
        // (`notifies_progression_and_services.rs:171-175`).
        samples: || vec![vec![
            PyRep::Integer(9),
            PyRep::Integer(1),
            PyRep::Dict(vec![
                (PyRep::String("counter_value".into()), PyRep::Integer(5)),
                (PyRep::String("completed".into()), PyRep::Bool(false)),
            ]),
        ]],
    },
    OnProvingGroundsMatchFound => "OnProvingGroundsMatchFound" ("charid") {
        samples: || vec![vec![]],
    },
    OnPvpTimerUpdate => "OnPvpTimerUpdate" ("charid") {
        fields: { state: i64, expiry_time: Option<SimFileTime> },
        optional: { do_alert: Option<bool> },
        // `services/notify_emitters_fleet_combat.rs:166` emits `expiry_time = None` on timer expiry.
        samples: || vec![
            vec![PyRep::Integer(201), PyRep::Integer(1_335_000_000_000_000_00)],
            vec![PyRep::Integer(200), PyRep::None],
            vec![PyRep::Integer(201), PyRep::Integer(1_335_000_000_000_000_00), PyRep::Bool(true)],
            vec![PyRep::Integer(200), PyRep::None, PyRep::Bool(false)],
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
        // `services/notify_emitters_combat_ess_progression.rs:383` and `services/space_destiny.rs:520-525`.
        samples: || vec![
            vec![PyRep::keyval([("regionID", PyRep::Integer(10_000_002))])],
            vec![PyRep::keyval([
                ("regionID", PyRep::Integer(10_000_002)),
                ("solarSystemID", PyRep::Integer(30_000_142)),
                ("characterID", PyRep::Integer(90_000_001)),
                ("unlockUntilUnix", PyRep::Integer(1_700_000_000)),
                ("durationMinutes", PyRep::Integer(15)),
            ])],
        ],
    },
    OnRestoringInvulnUpdated => "OnRestoringInvulnUpdated" ("charid") {
        fields: { ship_id: i64, end_time: SimFileTime, duration: i64 },
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
        // `services/season_manager.rs:459-497` (`build_character_progress_keyval`), `756-791` (`build_season_data_keyval`).
        samples: || vec![
            vec![
                PyRep::Integer(90_000_001),
                sample_season_data_kv(true),
                sample_character_progress_kv(true),
            ],
            vec![
                PyRep::Integer(90_000_001),
                sample_season_data_kv(false),
                sample_character_progress_kv(false),
            ],
        ],
    },
    OnSeasonSelectionCleared => "OnSeasonSelectionCleared" ("charid") {
        samples: || vec![vec![]],
    },
    OnSeasonStarted => "OnSeasonStarted" ("charid") {
        samples: || vec![vec![]],
    },
    OnSeasonalGoalCompleted => "OnSeasonalGoalCompleted" ("charid") {
        fields: { goal_index: i64, goal_data: PyRep },
        // `services/season_manager_ops.rs:204-222` (`build_seasonal_goal_dict`).
        samples: || vec![vec![PyRep::Integer(1), sample_season_goal_dict()]],
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
        // `services/notify_emitters_missing2.rs:144` emits `(version, bytes)` for `ProcessBrainData`.
        samples: || vec![
            vec![PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Bytes(vec![0])])],
            vec![PyRep::Tuple(vec![PyRep::Integer(1), PyRep::None])],
        ],
    },
    OnServerMapDataPush => "OnServerMapDataPush" ("charid") {
        fields: { tricolors: PyRep, data: PyRep },
        // `services/notify_emitters_missing2.rs:162-181`: `(sys_id, size, scale, text, rgb_or_none_tuple)`.
        samples: || vec![vec![
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Real(1.0),
                PyRep::Real(0.5),
                PyRep::Real(0.0),
            ])]),
            PyRep::List(vec![
                PyRep::Tuple(vec![
                    PyRep::Integer(30_000_142),
                    PyRep::Real(1.5),
                    PyRep::Real(2.0),
                    PyRep::String("Jita".into()),
                    PyRep::Tuple(vec![PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.5)]),
                ]),
                PyRep::Tuple(vec![
                    PyRep::Integer(30_002_187),
                    PyRep::Real(0.8),
                    PyRep::Real(1.0),
                    PyRep::String("Amarr".into()),
                    PyRep::Tuple(vec![PyRep::None, PyRep::None, PyRep::None]),
                ]),
            ]),
        ]],
    },
    OnServerMessage => "OnServerMessage" ("clientID") {
        fields: { msg: String },
        samples: || vec![vec![PyRep::String("Broadcast message".into())]],
    },
    OnSessionChanged => "OnSessionChanged" ("charid") {
        fields: { is_remote: bool, session: PyRep, change: PyRep },
        samples: sample_session_kv_variants,
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
        // `services/notify_emitters_tournaments_anomalies.rs:197-377` emits 4 `Dict` buckets:
        // `(anomalies, signatures, static_sites, structures)`.
        samples: || vec![vec![
            PyRep::Integer(30_000_142),
            PyRep::Tuple(vec![
                PyRep::Dict(vec![
                    (
                        PyRep::String("ANOM-1".into()),
                        PyRep::keyval([
                            ("instanceID", PyRep::Integer(1)),
                            ("siteID", PyRep::String("ANOM-1".into())),
                            ("targetID", PyRep::String("ANOM-1".into())),
                            ("dungeonID", PyRep::Integer(100)),
                            ("dungeonName", PyRep::String("Combat Anomaly".into())),
                            ("dungeonNameID", PyRep::None),
                            ("anomalyType", PyRep::String("Combat".into())),
                            ("archetypeID", PyRep::Integer(1)),
                            ("difficulty", PyRep::Integer(1)),
                            ("factionID", PyRep::Integer(500_010)),
                            ("scanStrength", PyRep::Real(100.0)),
                            ("signalStrength", PyRep::Real(100.0)),
                            ("scanStrengthAttribute", PyRep::Integer(10)),
                            ("deviation", PyRep::Real(0.0)),
                            ("groupID", PyRep::None),
                            ("typeID", PyRep::None),
                            ("expiryTime", PyRep::None),
                            ("isSiteExpired", PyRep::Bool(false)),
                            ("allowedTypes", PyRep::None),
                            (
                                "position",
                                PyRep::Tuple(vec![
                                    PyRep::Real(0.0),
                                    PyRep::Real(0.0),
                                    PyRep::Real(0.0),
                                ]),
                            ),
                        ]),
                    ),
                    (
                        PyRep::String("ANOM-1".into()),
                        PyRep::keyval([
                            ("instanceID", PyRep::Integer(2)),
                            ("siteID", PyRep::String("ANOM-1".into())),
                            ("targetID", PyRep::String("ANOM-1".into())),
                            ("dungeonID", PyRep::Integer(101)),
                            ("dungeonName", PyRep::String("Combat Anomaly 2".into())),
                            ("dungeonNameID", PyRep::Integer(1_000)),
                            ("anomalyType", PyRep::String("Combat".into())),
                            ("archetypeID", PyRep::Integer(1)),
                            ("difficulty", PyRep::Integer(1)),
                            ("factionID", PyRep::Integer(500_010)),
                            ("scanStrength", PyRep::Real(100.0)),
                            ("signalStrength", PyRep::Real(100.0)),
                            ("scanStrengthAttribute", PyRep::Integer(10)),
                            ("deviation", PyRep::Real(0.0)),
                            ("groupID", PyRep::None),
                            ("typeID", PyRep::None),
                            ("expiryTime", PyRep::None),
                            ("isSiteExpired", PyRep::Bool(false)),
                            ("allowedTypes", PyRep::None),
                            (
                                "position",
                                PyRep::Tuple(vec![
                                    PyRep::Real(1.0),
                                    PyRep::Real(0.0),
                                    PyRep::Real(0.0),
                                ]),
                            ),
                        ]),
                    ),
                    (
                        PyRep::String("ANOM-2".into()),
                        PyRep::keyval([
                            ("instanceID", PyRep::Integer(2)),
                            ("siteID", PyRep::String("ANOM-2".into())),
                            ("targetID", PyRep::String("ANOM-2".into())),
                            ("dungeonID", PyRep::Integer(101)),
                            ("dungeonName", PyRep::String("Combat Anomaly 2".into())),
                            ("dungeonNameID", PyRep::Integer(1_000)),
                            ("anomalyType", PyRep::String("Combat".into())),
                            ("archetypeID", PyRep::Integer(1)),
                            ("difficulty", PyRep::Integer(1)),
                            ("factionID", PyRep::Integer(500_010)),
                            ("scanStrength", PyRep::Real(100.0)),
                            ("signalStrength", PyRep::Real(100.0)),
                            ("scanStrengthAttribute", PyRep::Integer(10)),
                            ("deviation", PyRep::Real(0.0)),
                            ("groupID", PyRep::None),
                            ("typeID", PyRep::None),
                            ("expiryTime", PyRep::None),
                            ("isSiteExpired", PyRep::Bool(false)),
                            ("allowedTypes", PyRep::None),
                            (
                                "position",
                                PyRep::Tuple(vec![
                                    PyRep::Real(1.0),
                                    PyRep::Real(0.0),
                                    PyRep::Real(0.0),
                                ]),
                            ),
                        ]),
                    ),
                ]),
                PyRep::Dict(vec![(
                    PyRep::String("SIG-1".into()),
                    PyRep::keyval([
                        ("targetID", PyRep::String("SIG-1".into())),
                        ("sigGroup", PyRep::String("Relic".into())),
                        ("typeName", PyRep::String("Relic Site".into())),
                        ("scanStrength", PyRep::Real(25.0)),
                        ("difficulty", PyRep::Integer(1)),
                        ("dungeonID", PyRep::None),
                        ("archetypeID", PyRep::None),
                        ("deviation", PyRep::Real(0.0)),
                        (
                            "position",
                            PyRep::Tuple(vec![
                                PyRep::Real(0.0),
                                PyRep::Real(0.0),
                                PyRep::Real(0.0),
                            ]),
                        ),
                    ]),
                )]),
                PyRep::Dict(vec![(
                    PyRep::Integer(60_003_760),
                    PyRep::keyval([
                        ("siteID", PyRep::Integer(60_003_760)),
                        ("instanceID", PyRep::Integer(60_003_760)),
                        ("groupID", PyRep::Integer(15)),
                        ("typeID", PyRep::Integer(1_531)),
                        ("dungeonID", PyRep::None),
                        ("dungeonNameID", PyRep::None),
                        ("factionID", PyRep::None),
                        (
                            "position",
                            PyRep::Tuple(vec![
                                PyRep::Real(0.0),
                                PyRep::Real(0.0),
                                PyRep::Real(0.0),
                            ]),
                        ),
                    ]),
                )]),
                PyRep::Dict(vec![(
                    PyRep::Integer(1_000_000_000_001),
                    PyRep::keyval([
                        ("itemID", PyRep::Integer(1_000_000_000_001)),
                        ("targetID", PyRep::Integer(1_000_000_000_001)),
                        ("typeID", PyRep::Integer(35_832)),
                        ("groupID", PyRep::Integer(1_657)),
                        ("categoryID", PyRep::Integer(65)),
                        ("name", PyRep::String("Astrahus".into())),
                        (
                            "position",
                            PyRep::Tuple(vec![
                                PyRep::Real(0.0),
                                PyRep::Real(0.0),
                                PyRep::Real(0.0),
                            ]),
                        ),
                    ]),
                )]),
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
        // `services/notify_emitters_operations_nodegraph.rs:66-83` (`blackboard_to_py_dict`).
        samples: || vec![vec![
            PyRep::Integer(10),
            PyRep::Integer(1),
            PyRep::Dict(vec![
                (PyRep::String("step".into()), PyRep::Integer(1)),
                (PyRep::String("active".into()), PyRep::Bool(true)),
                (PyRep::String("label".into()), PyRep::String("init".into())),
            ]),
        ]],
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
        fields: { omega_time: WallFileTime },
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
            PyRep::List(vec![PyRep::Integer(90_000_003)]),
        ]],
    },
    OnSystemDisapprovalFlagUpdates => "OnSystemDisapprovalFlagUpdates" ("solarsystemid") {
        fields: { new_idles: Vec<i64>, new_naughty_boys_and_girls: Vec<i64> },
        samples: || vec![vec![
            PyRep::List(vec![PyRep::Integer(90_000_001)]),
            PyRep::List(vec![PyRep::Integer(90_000_002)]),
        ]],
    },
    OnSystemDynamicBountyStateChanged => "OnSystemDynamicBountyStateChanged" ("solarsystemid") {
        fields: { system_id: i64, rate: f64 },
        samples: || vec![vec![PyRep::Integer(30_000_142), PyRep::Real(1.25)]],
    },
    OnSystemESSDataChanged => "OnSystemESSDataChanged" ("solarsystemid") {
        fields: { system_id: i64, data: PyRep },
        // Canonical V24 `DynamicResourceSvc.OnSystemESSDataChanged` passes `Dict` (`solarSystemID`, `mainBank`, `reserveBank`).
        samples: || vec![vec![
            PyRep::Integer(30_000_142),
            PyRep::Dict(vec![
                (PyRep::String("solarSystemID".into()), PyRep::Integer(30_000_142)),
                (PyRep::String("mainBank".into()), PyRep::Real(50_000_000.0)),
                (PyRep::String("reserveBank".into()), PyRep::Real(100_000_000.0)),
            ]),
        ]],
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
        // `services/pve_incursion_lifecycle.rs:923-948` emits full incursion tale Dicts
        // keyed by `taleID` (matching `taleMgr` subscript consumers); also allow KeyVal shape.
        samples: || vec![
            vec![
                PyRep::Integer(30_000_142),
                PyRep::Dict(vec![(
                    PyRep::Integer(701),
                    PyRep::Dict(vec![
                        (PyRep::String("taleID".into()), PyRep::Integer(701)),
                        (PyRep::String("templateID".into()), PyRep::Integer(51)),
                        (PyRep::String("templateClassID".into()), PyRep::String("worldEvents".into())),
                        (PyRep::String("managerSolarSystemID".into()), PyRep::Integer(30_000_142)),
                        (PyRep::String("solar_system_id".into()), PyRep::Integer(30_000_142)),
                        (PyRep::String("locationID".into()), PyRep::Integer(30_000_142)),
                        (PyRep::String("constellationID".into()), PyRep::Integer(20_000_020)),
                        (PyRep::String("constellationName".into()), PyRep::String("Kimotoro".into())),
                        (PyRep::String("influence".into()), PyRep::Real(1.0)),
                        (PyRep::String("state".into()), PyRep::String("Established".into())),
                        (PyRep::String("systemState".into()), PyRep::String("Established".into())),
                        (
                            PyRep::String("systemStates".into()),
                            PyRep::Dict(vec![(
                                PyRep::Integer(30_000_142),
                                PyRep::String("Established".into()),
                            )]),
                        ),
                        (PyRep::String("aggressorFactionID".into()), PyRep::Integer(500_019)),
                        (PyRep::String("templateNameID".into()), PyRep::Integer(564_035)),
                        (PyRep::String("templateDescriptionID".into()), PyRep::Integer(564_036)),
                        (PyRep::String("endTime".into()), PyRep::Integer(0)),
                        (PyRep::String("hasFinalEncounter".into()), PyRep::Bool(false)),
                        (PyRep::String("mothershipSpawned".into()), PyRep::Bool(false)),
                        (PyRep::String("mothershipDefeated".into()), PyRep::Bool(false)),
                        (
                            PyRep::String("sceneTypesByLocationID".into()),
                            PyRep::Dict(vec![(
                                PyRep::Integer(30_000_142),
                                PyRep::String("incursionStaging".into()),
                            )]),
                        ),
                        (
                            PyRep::String("sites".into()),
                            PyRep::List(vec![PyRep::keyval([
                                ("siteID", PyRep::String("inc-1".into())),
                                ("dungeonID", PyRep::Integer(1)),
                                ("dungeonName", PyRep::String("Assault".into())),
                                ("solarSystemID", PyRep::Integer(30_000_142)),
                                ("tier", PyRep::String("Vanguard".into())),
                                ("minPilots", PyRep::Integer(5)),
                                ("maxOptimalPilots", PyRep::Integer(10)),
                                ("baseIsk", PyRep::Integer(10_000_000)),
                                ("baseConcordLP", PyRep::Integer(1_400)),
                                ("bossNpcBallID", PyRep::Integer(0)),
                                ("available", PyRep::Bool(true)),
                            ])]),
                        ),
                    ]),
                )]),
            ],
        ],
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
        samples: || vec![
            vec![PyRep::Tuple(vec![
                PyRep::Dict(Vec::new()),
                PyRep::Dict(Vec::new()),
                PyRep::Dict(Vec::new()),
            ])],
            vec![PyRep::Tuple(vec![
                PyRep::Dict(vec![(PyRep::Integer(1), PyRep::String("Hello".into()))]),
                PyRep::Dict(Vec::new()),
                PyRep::Dict(Vec::new()),
            ])],
        ],
    },
    OnUpdateServiceMapping => "OnUpdateServiceMapping" ("charid") {
        fields: { svc: String, node_id: i64 },
        samples: || vec![vec![PyRep::String("dogmaIM".into()), PyRep::Integer(26_000)]],
    },
    OnUpdateSystemWideEffectsInfo => "OnUpdateSystemWideEffectsInfo" ("charid") {
        fields: { system_wide_effects_on_ship: PyRep },
        samples: || vec![
            vec![PyRep::Dict(Vec::new())],
            vec![PyRep::Dict(vec![(
                PyRep::Tuple(vec![PyRep::Integer(30_000_142), PyRep::Integer(30_844)]),
                PyRep::Set(vec![PyRep::Integer(6_753)]),
            )])],
        ],
    },
    OnVictoryPointStateUpdated => "OnVictoryPointStateUpdated" ("solarsystemid") {
        fields: { solar_system_id: i64, new_victory_point_state: PyRep },
        // `services/notify_emitters_fleet_combat.rs:125` & `services/fw_services.rs:361`.
        samples: || vec![vec![
            PyRep::Integer(30_000_142),
            PyRep::keyval([
                ("solarSystemID", PyRep::Integer(30_000_142)),
                ("victoryPoints", PyRep::Integer(10)),
                ("contestedPercentage", PyRep::Real(5.0)),
                ("threshold", PyRep::Integer(200)),
            ]),
        ]],
    },
    OnWeaponsTimerUpdate => "OnWeaponsTimerUpdate" ("charid") {
        fields: { state: i64, expiry_time: Option<SimFileTime> },
        optional: { do_alert: Option<bool> },
        // `services/notify_emitters_fleet_combat.rs:148` emits `expiry_time = None` on timer expiry.
        samples: || vec![
            vec![PyRep::Integer(101), PyRep::Integer(1_335_000_000_000_000_00)],
            vec![PyRep::Integer(100), PyRep::None],
            vec![PyRep::Integer(101), PyRep::Integer(1_335_000_000_000_000_00), PyRep::Bool(true)],
            vec![PyRep::Integer(100), PyRep::None, PyRep::Bool(false)],
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
        // `services/wallet_serverops.rs:589-599` (`create_bill_in_snapshot`).
        samples: || vec![
            vec![],
            vec![PyRep::keyval([
                ("billID", PyRep::Integer(1)),
                ("billTypeID", PyRep::Integer(2)),
                ("debtorID", PyRep::Integer(98_000_001)),
                ("creditorID", PyRep::Integer(1_000_001)),
                ("amount", PyRep::Real(10_000.0)),
                ("dueDateTime", PyRep::Integer(133_500_000_000_000_000)),
                ("interest", PyRep::Real(0.05)),
                ("externalID", PyRep::Integer(-1)),
                ("externalID2", PyRep::Integer(-1)),
            ])],
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
        samples: || vec![vec![PyRep::String("solarsystemid2:30000142".into())]],
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
        // `services/fleet_social_corp_tail_dispatch.rs:789-829` (`build_aggression_settings_object`)
        // emits 7-field `crimewatch.corp_aggression.settings.AggressionSettings` `PyRep::Object`.
        samples: || vec![
            vec![PyRep::keyval([("friendlyFire", PyRep::Bool(false))])],
            vec![PyRep::Object {
                type_name: "crimewatch.corp_aggression.settings.AggressionSettings".into(),
                args: Box::new(PyRep::Dict(vec![
                    (PyRep::String("_enableAfter".into()), PyRep::None),
                    (PyRep::String("_disableAfter".into()), PyRep::Integer(0)),
                    (PyRep::String("corporationID".into()), PyRep::Integer(98_000_001)),
                    (PyRep::String("isFriendlyFireLegal".into()), PyRep::Bool(true)),
                    (PyRep::String("friendlyFireLegal".into()), PyRep::Bool(true)),
                    (PyRep::String("pendingFriendlyFireLegal".into()), PyRep::Bool(false)),
                    (PyRep::String("pendingFriendlyFireLegal".into()), PyRep::None),
                    (
                        PyRep::String("changeAtTime".into()),
                        PyRep::Integer(1_335_000_000_000_000_00),
                    ),
                    (PyRep::String("changeAtTime".into()), PyRep::None),
                ])),
            }],
            vec![PyRep::Object {
                type_name: "crimewatch.corp_aggression.settings.AggressionSettings".into(),
                args: Box::new(PyRep::Dict(vec![
                    (
                        PyRep::String("_enableAfter".into()),
                        PyRep::Integer(1_335_000_000_000_000_00),
                    ),
                    (PyRep::String("_disableAfter".into()), PyRep::None),
                    (PyRep::String("_disableAfter".into()), PyRep::Integer(0)),
                    (PyRep::String("corporationID".into()), PyRep::Integer(98_000_001)),
                    (PyRep::String("isFriendlyFireLegal".into()), PyRep::Bool(false)),
                    (PyRep::String("friendlyFireLegal".into()), PyRep::Bool(false)),
                    (PyRep::String("pendingFriendlyFireLegal".into()), PyRep::Bool(true)),
                    (PyRep::String("pendingFriendlyFireLegal".into()), PyRep::None),
                    (
                        PyRep::String("changeAtTime".into()),
                        PyRep::Integer(1_335_000_000_000_000_00),
                    ),
                    (PyRep::String("changeAtTime".into()), PyRep::None),
                ])),
            }],
        ],
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
            PyRep::keyval([
                ("applicationID", PyRep::Integer(1)),
                ("corporationID", PyRep::Integer(98_000_001)),
                ("characterID", PyRep::Integer(90_000_001)),
                ("applicationText", PyRep::String("Applying to join".into())),
                ("status", PyRep::String("Applied".into())),
                ("applicationDateTime", PyRep::Integer(1_775_400_000)),
            ]),
        ]],
    },
    OnCorporationChanged => "OnCorporationChanged" ("corpid") {
        fields: { corp_id: i64, change: PyRep },
        // `fleet_social_corps.rs:1192-1375`, `fleet_social_corp_gov.rs:774-818,1541,1906`, and
        // `fleet_social_corp_tail_dispatch.rs:176-215,462` emit `(fieldName, (old, new))` entries.
        samples: || vec![vec![
            PyRep::Integer(98_000_001),
            PyRep::Dict(vec![
                (
                    PyRep::String("corporationID".into()),
                    PyRep::Tuple(vec![PyRep::None, PyRep::Integer(98_000_001)]),
                ),
                (
                    PyRep::String("corporationName".into()),
                    PyRep::Tuple(vec![PyRep::None, PyRep::String("Corp".into())]),
                ),
                (
                    PyRep::String("ticker".into()),
                    PyRep::Tuple(vec![PyRep::None, PyRep::String("CRP".into())]),
                ),
                (
                    PyRep::String("tickerName".into()),
                    PyRep::Tuple(vec![PyRep::None, PyRep::String("CRP".into())]),
                ),
                (
                    PyRep::String("ceoID".into()),
                    PyRep::Tuple(vec![PyRep::None, PyRep::Integer(90_000_001)]),
                ),
                (
                    PyRep::String("ceoID".into()),
                    PyRep::Tuple(vec![PyRep::Integer(90_000_001), PyRep::Integer(90_000_002)]),
                ),
                (
                    PyRep::String("creatorID".into()),
                    PyRep::Tuple(vec![PyRep::None, PyRep::Integer(90_000_001)]),
                ),
                (
                    PyRep::String("memberCount".into()),
                    PyRep::Tuple(vec![PyRep::None, PyRep::Integer(1)]),
                ),
                (
                    PyRep::String("memberLimit".into()),
                    PyRep::Tuple(vec![PyRep::None, PyRep::Integer(10)]),
                ),
                (
                    PyRep::String("taxRate".into()),
                    PyRep::Tuple(vec![PyRep::None, PyRep::Real(0.05)]),
                ),
                (
                    PyRep::String("taxRate".into()),
                    PyRep::Tuple(vec![PyRep::Real(0.1), PyRep::Real(0.05)]),
                ),
                (
                    PyRep::String("iskTaxRate".into()),
                    PyRep::Tuple(vec![PyRep::None, PyRep::Real(0.05)]),
                ),
                (
                    PyRep::String("loyaltyPointTaxRate".into()),
                    PyRep::Tuple(vec![PyRep::None, PyRep::Real(0.05)]),
                ),
                (
                    PyRep::String("allianceID".into()),
                    PyRep::Tuple(vec![PyRep::None, PyRep::None]),
                ),
                (
                    PyRep::String("allianceID".into()),
                    PyRep::Tuple(vec![PyRep::None, PyRep::Integer(99_000_001)]),
                ),
                (
                    PyRep::String("allianceID".into()),
                    PyRep::Tuple(vec![PyRep::Integer(99_000_001), PyRep::None]),
                ),
                (
                    PyRep::String("stationID".into()),
                    PyRep::Tuple(vec![PyRep::None, PyRep::None]),
                ),
                (
                    PyRep::String("stationID".into()),
                    PyRep::Tuple(vec![PyRep::None, PyRep::Integer(60_003_760)]),
                ),
                (
                    PyRep::String("stationID".into()),
                    PyRep::Tuple(vec![PyRep::Integer(60_003_760), PyRep::None]),
                ),
                (
                    PyRep::String("stationID".into()),
                    PyRep::Tuple(vec![PyRep::Integer(60_003_760), PyRep::Integer(60_003_761)]),
                ),
                (
                    PyRep::String("description".into()),
                    PyRep::Tuple(vec![PyRep::None, PyRep::String("desc".into())]),
                ),
                (
                    PyRep::String("description".into()),
                    PyRep::Tuple(vec![PyRep::String("old".into()), PyRep::String("new".into())]),
                ),
                (
                    PyRep::String("url".into()),
                    PyRep::Tuple(vec![PyRep::None, PyRep::String("https://eve.example".into())]),
                ),
                (
                    PyRep::String("url".into()),
                    PyRep::Tuple(vec![PyRep::String("a".into()), PyRep::String("b".into())]),
                ),
                (
                    PyRep::String("shares".into()),
                    PyRep::Tuple(vec![PyRep::None, PyRep::Integer(1_000)]),
                ),
                (
                    PyRep::String("shape1".into()),
                    PyRep::Tuple(vec![PyRep::None, PyRep::Integer(1)]),
                ),
                (
                    PyRep::String("shape1".into()),
                    PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(2)]),
                ),
                (
                    PyRep::String("shape2".into()),
                    PyRep::Tuple(vec![PyRep::None, PyRep::Integer(1)]),
                ),
                (
                    PyRep::String("shape2".into()),
                    PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(2)]),
                ),
                (
                    PyRep::String("shape3".into()),
                    PyRep::Tuple(vec![PyRep::None, PyRep::Integer(1)]),
                ),
                (
                    PyRep::String("shape3".into()),
                    PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(2)]),
                ),
                (
                    PyRep::String("color1".into()),
                    PyRep::Tuple(vec![PyRep::None, PyRep::Integer(1)]),
                ),
                (
                    PyRep::String("color1".into()),
                    PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(2)]),
                ),
                (
                    PyRep::String("color2".into()),
                    PyRep::Tuple(vec![PyRep::None, PyRep::Integer(1)]),
                ),
                (
                    PyRep::String("color2".into()),
                    PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(2)]),
                ),
                (
                    PyRep::String("color3".into()),
                    PyRep::Tuple(vec![PyRep::None, PyRep::Integer(1)]),
                ),
                (
                    PyRep::String("color3".into()),
                    PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(2)]),
                ),
                (
                    PyRep::String("applicationsEnabled".into()),
                    PyRep::Tuple(vec![PyRep::None, PyRep::Bool(true)]),
                ),
                (
                    PyRep::String("isRecruiting".into()),
                    PyRep::Tuple(vec![PyRep::None, PyRep::Bool(true)]),
                ),
                (
                    PyRep::String("isRecruiting".into()),
                    PyRep::Tuple(vec![PyRep::Bool(false), PyRep::Bool(true)]),
                ),
                (
                    PyRep::String("allowWar".into()),
                    PyRep::Tuple(vec![PyRep::None, PyRep::Bool(true)]),
                ),
                (
                    PyRep::String("allowWar".into()),
                    PyRep::Tuple(vec![PyRep::Bool(true), PyRep::Bool(false)]),
                ),
                (
                    PyRep::String("friendlyFire".into()),
                    PyRep::Tuple(vec![PyRep::None, PyRep::Bool(false)]),
                ),
                (
                    PyRep::String("restrictCorpMails".into()),
                    PyRep::Tuple(vec![PyRep::Bool(false), PyRep::Bool(true)]),
                ),
                (
                    PyRep::String("acceptStructures".into()),
                    PyRep::Tuple(vec![PyRep::Bool(false), PyRep::Bool(true)]),
                ),
                (
                    PyRep::String("deleted".into()),
                    PyRep::Tuple(vec![PyRep::None, PyRep::Integer(0)]),
                ),
                (
                    PyRep::String("typeface".into()),
                    PyRep::Tuple(vec![PyRep::None, PyRep::String("default".into())]),
                ),
                (
                    PyRep::String("typeface".into()),
                    PyRep::Tuple(vec![PyRep::None, PyRep::Integer(1)]),
                ),
                (
                    PyRep::String("division1".into()),
                    PyRep::Tuple(vec![PyRep::None, PyRep::String("1st Division".into())]),
                ),
                (
                    PyRep::String("division1".into()),
                    PyRep::Tuple(vec![PyRep::String("old".into()), PyRep::String("new".into())]),
                ),
                (
                    PyRep::String("walletDivision1".into()),
                    PyRep::Tuple(vec![PyRep::None, PyRep::String("Master Wallet".into())]),
                ),
                (
                    PyRep::String("walletDivision1".into()),
                    PyRep::Tuple(vec![PyRep::String("old".into()), PyRep::String("new".into())]),
                ),
                (
                    PyRep::String("division2".into()),
                    PyRep::Tuple(vec![PyRep::None, PyRep::String("2nd Division".into())]),
                ),
                (
                    PyRep::String("division2".into()),
                    PyRep::Tuple(vec![PyRep::String("old".into()), PyRep::String("new".into())]),
                ),
                (
                    PyRep::String("walletDivision2".into()),
                    PyRep::Tuple(vec![PyRep::None, PyRep::String("Division 2".into())]),
                ),
                (
                    PyRep::String("walletDivision2".into()),
                    PyRep::Tuple(vec![PyRep::String("old".into()), PyRep::String("new".into())]),
                ),
                (
                    PyRep::String("division3".into()),
                    PyRep::Tuple(vec![PyRep::None, PyRep::String("3rd Division".into())]),
                ),
                (
                    PyRep::String("division3".into()),
                    PyRep::Tuple(vec![PyRep::String("old".into()), PyRep::String("new".into())]),
                ),
                (
                    PyRep::String("walletDivision3".into()),
                    PyRep::Tuple(vec![PyRep::None, PyRep::String("Division 3".into())]),
                ),
                (
                    PyRep::String("walletDivision3".into()),
                    PyRep::Tuple(vec![PyRep::String("old".into()), PyRep::String("new".into())]),
                ),
                (
                    PyRep::String("division4".into()),
                    PyRep::Tuple(vec![PyRep::None, PyRep::String("4th Division".into())]),
                ),
                (
                    PyRep::String("division4".into()),
                    PyRep::Tuple(vec![PyRep::String("old".into()), PyRep::String("new".into())]),
                ),
                (
                    PyRep::String("walletDivision4".into()),
                    PyRep::Tuple(vec![PyRep::None, PyRep::String("Division 4".into())]),
                ),
                (
                    PyRep::String("walletDivision4".into()),
                    PyRep::Tuple(vec![PyRep::String("old".into()), PyRep::String("new".into())]),
                ),
                (
                    PyRep::String("division5".into()),
                    PyRep::Tuple(vec![PyRep::None, PyRep::String("5th Division".into())]),
                ),
                (
                    PyRep::String("division5".into()),
                    PyRep::Tuple(vec![PyRep::String("old".into()), PyRep::String("new".into())]),
                ),
                (
                    PyRep::String("walletDivision5".into()),
                    PyRep::Tuple(vec![PyRep::None, PyRep::String("Division 5".into())]),
                ),
                (
                    PyRep::String("walletDivision5".into()),
                    PyRep::Tuple(vec![PyRep::String("old".into()), PyRep::String("new".into())]),
                ),
                (
                    PyRep::String("division6".into()),
                    PyRep::Tuple(vec![PyRep::None, PyRep::String("6th Division".into())]),
                ),
                (
                    PyRep::String("division6".into()),
                    PyRep::Tuple(vec![PyRep::String("old".into()), PyRep::String("new".into())]),
                ),
                (
                    PyRep::String("walletDivision6".into()),
                    PyRep::Tuple(vec![PyRep::None, PyRep::String("Division 6".into())]),
                ),
                (
                    PyRep::String("walletDivision6".into()),
                    PyRep::Tuple(vec![PyRep::String("old".into()), PyRep::String("new".into())]),
                ),
                (
                    PyRep::String("division7".into()),
                    PyRep::Tuple(vec![PyRep::None, PyRep::String("7th Division".into())]),
                ),
                (
                    PyRep::String("division7".into()),
                    PyRep::Tuple(vec![PyRep::String("old".into()), PyRep::String("new".into())]),
                ),
                (
                    PyRep::String("walletDivision7".into()),
                    PyRep::Tuple(vec![PyRep::None, PyRep::String("Division 7".into())]),
                ),
                (
                    PyRep::String("walletDivision7".into()),
                    PyRep::Tuple(vec![PyRep::String("old".into()), PyRep::String("new".into())]),
                ),
            ]),
        ]],
    },
    OnCorporationMemberChanged => "OnCorporationMemberChanged" ("corpid") {
        fields: { corporation_id: i64, member_id: i64, change: PyRep },
        // `services/corp_registry_ops.rs:822,1024,1050,1076`: integer keys + `description` `(String, String)`.
        samples: || vec![
            vec![
                PyRep::Integer(98_000_001),
                PyRep::Integer(90_000_001),
                PyRep::Dict(vec![
                    (
                        PyRep::String("corporationID".into()),
                        PyRep::Tuple(vec![PyRep::Integer(98_000_001), PyRep::Integer(98_000_002)]),
                    ),
                    (
                        PyRep::String("roles".into()),
                        PyRep::Tuple(vec![PyRep::Integer(0), PyRep::Integer(1)]),
                    ),
                    (
                        PyRep::String("accountKey".into()),
                        PyRep::Tuple(vec![PyRep::Integer(1000), PyRep::Integer(1001)]),
                    ),
                    (
                        PyRep::String("titleMask".into()),
                        PyRep::Tuple(vec![PyRep::Integer(0), PyRep::Integer(2)]),
                    ),
                ]),
            ],
            vec![
                PyRep::Integer(98_000_001),
                PyRep::Integer(90_000_001),
                PyRep::Dict(vec![(
                    PyRep::String("description".into()),
                    PyRep::Tuple(vec![PyRep::String("old".into()), PyRep::String("new".into())]),
                )]),
            ],
        ],
    },
    OnCorporationRecruitmentAdChanged => "OnCorporationRecruitmentAdChanged" ("corpid") {
        samples: || vec![vec![]],
    },
    OnCorporationStructuresUpdated => "OnCorporationStructuresUpdated" ("corpid") {
        samples: || vec![vec![]],
    },
    OnCorporationWelcomeMailChanged => "OnCorporationWelcomeMailChanged" ("corpid") {
        fields: { character_id: i64, change_date: WallFileTime },
        samples: || vec![vec![PyRep::Integer(90_000_001), PyRep::Integer(1_335_000_000_000_000_00)]],
    },
    OnCrimewatchEngagementStartTimeout => "OnCrimewatchEngagementStartTimeout" ("charid") {
        fields: { other_char_id: i64, timeout: WallFileTime },
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
    OnDamageMessage => "OnDamageMessage" ("charid") @ballpark {
        fields: { damage_messages_args: PyRep },
        // `system_actor/cycles.rs:2660-2705` & `tactical_modules_superweapon_ops.rs:1304`.
        samples: || vec![vec![PyRep::Dict(vec![
            (PyRep::String("attackType".into()), PyRep::String("other".into())),
            (PyRep::String("damage".into()), PyRep::Real(42.5)),
            (PyRep::String("source".into()), PyRep::Integer(100_000_001)),
            (PyRep::String("target".into()), PyRep::Integer(100_000_002)),
            (PyRep::String("target_ID".into()), PyRep::Integer(100_000_002)),
            (PyRep::String("attackerID".into()), PyRep::Integer(90_000_001)),
            (PyRep::String("typeID".into()), PyRep::Integer(561)),
            (PyRep::String("weapon".into()), PyRep::Integer(561)),
            (PyRep::String("hitQuality".into()), PyRep::Integer(5)),
            (PyRep::String("isWrecking".into()), PyRep::Bool(false)),
            (PyRep::String("isBanked".into()), PyRep::Bool(false)),
            (
                PyRep::String("owner".into()),
                PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(90_000_001)]),
            ),
        ])]],
    },
    OnDamageMessages => "OnDamageMessages" ("charid") @ballpark {
        fields: { dmgmsgs: Vec<PyRep> },
        samples: || vec![vec![PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::String("AttackHit".into()),
            PyRep::Dict(vec![
                (PyRep::String("attackType".into()), PyRep::String("you".into())),
                (PyRep::String("damage".into()), PyRep::Real(42.5)),
                (PyRep::String("source".into()), PyRep::Integer(100_000_001)),
                (PyRep::String("target".into()), PyRep::Integer(100_000_002)),
                (PyRep::String("target_ID".into()), PyRep::Integer(100_000_002)),
                (PyRep::String("attackerID".into()), PyRep::Integer(90_000_001)),
                (PyRep::String("weapon".into()), PyRep::Integer(561)),
                (PyRep::String("hitQuality".into()), PyRep::Integer(5)),
                (PyRep::String("isWrecking".into()), PyRep::Bool(false)),
                (PyRep::String("isBanked".into()), PyRep::Bool(false)),
                (
                    PyRep::String("owner".into()),
                    PyRep::Tuple(vec![PyRep::Integer(2), PyRep::Integer(90_000_001)]),
                ),
            ]),
        ])])]],
    },
    OnDirectionalScanComplete => "OnDirectionalScanComplete" ("charid") {
        fields: { angle: f64, range: f64, direction: PyRep, results: PyRep },
        // `services/fleet_social_scanning.rs:916` emits 6-column `util.Rowset` results.
        samples: || vec![
            vec![
                PyRep::Real(1.0),
                PyRep::Real(1_000_000.0),
                PyRep::Tuple(vec![PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)]),
                PyRep::rowset(
                    vec!["ballID", "typeID", "distance", "x", "y", "z"],
                    vec![vec![
                        PyRep::Integer(100_000_010),
                        PyRep::Integer(587),
                        PyRep::Real(1_000.0),
                        PyRep::Real(0.0),
                        PyRep::Real(0.0),
                        PyRep::Real(0.0),
                    ]],
                ),
            ],
            vec![
                PyRep::Real(1.0),
                PyRep::Real(1_000_000.0),
                PyRep::Tuple(vec![PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)]),
                PyRep::List(Vec::new()),
            ],
        ],
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
        // Canonical V24 `ShipFighterState.OnFighterAddedToController` passes Dict `abilitySlotStates`
        // keyed by `ability_slot_id` (`0..=2`), mapping to `(start_time, duration_ms)` or `None`.
        samples: || vec![vec![
            PyRep::Integer(100_000_050),
            PyRep::Integer(23055),
            PyRep::Integer(159),
            PyRep::Integer(9),
            PyRep::Dict(vec![
                (
                    PyRep::Integer(0),
                    PyRep::Tuple(vec![
                        PyRep::Integer(1_335_000_000_000_000_00),
                        PyRep::Real(10_000.0),
                    ]),
                ),
                (
                    PyRep::Integer(2),
                    PyRep::Tuple(vec![
                        PyRep::Integer(1_335_000_000_000_000_00),
                        PyRep::Integer(1_335_000_001_000_000_00),
                    ]),
                ),
                (PyRep::Integer(1), PyRep::None),
            ]),
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
            status_start_time: SimFileTime,
            status_end_time: SimFileTime,
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
        // `services/fleet_social_finder_tactical.rs:1462`.
        samples: || vec![
            vec![PyRep::keyval([
                ("charID", PyRep::Integer(90_000_002)),
                ("corpID", PyRep::Integer(98_000_001)),
            ])],
            vec![PyRep::keyval([
                ("charID", PyRep::Integer(90_000_002)),
                ("corpID", PyRep::None),
            ])],
        ],
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
        // `services/insurgency_services.rs:110-132` (`build_campaign_snapshot_for_system`).
        samples: || vec![vec![sample_insurgency_campaign_snapshot()]],
    },
    OnInsurgencyCampaignUpdatedForLocation => "OnInsurgencyCampaignUpdatedForLocation" ("solarsystemid") {
        fields: { campaign_snapshot: PyRep },
        // `services/insurgency_services.rs:110-132`, `865-872`; `fw_gameplay_insurgency_lifecycle.rs:553-557`.
        samples: || vec![
            vec![sample_insurgency_campaign_snapshot()],
            vec![PyRep::keyval([
                ("campaignID", PyRep::Integer(402)),
                ("solarSystemID", PyRep::Integer(30_000_142)),
                ("suppressionStage", PyRep::Integer(0)),
                ("suppressionPoints", PyRep::Integer(0)),
                ("coveredSolarsystemIDs", PyRep::List(vec![PyRep::Integer(30_000_142)])),
                ("systems", PyRep::List(vec![PyRep::Integer(30_000_142)])),
            ])],
            vec![PyRep::keyval([
                ("campaignID", PyRep::Integer(402)),
                ("state", PyRep::Integer(5)),
                ("coveredSolarsystemIDs", PyRep::List(vec![PyRep::Integer(30_000_142)])),
            ])],
        ],
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
        // `services/fleet_social_finder_tactical.rs:1588,1648` emits `{char_id: KeyVal({charID})}`.
        samples: || vec![
            vec![PyRep::Dict(vec![(
                PyRep::Integer(90_000_002),
                PyRep::keyval([("charID", PyRep::Integer(90_000_002))]),
            )])],
            vec![PyRep::Dict(Vec::new())],
        ],
    },
    OnKillRightCreated => "OnKillRightCreated" ("charid") {
        fields: { kill_right_id: i64, from_id: i64, to_id: i64, expiry_time: WallFileTime },
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
        samples: || vec![
            vec![PyRep::None],
            vec![PyRep::keyval([
                ("structureID", PyRep::Integer(1_000_000_010)),
                ("moonID", PyRep::Integer(40_000_001)),
                ("extractionStartTime", PyRep::Integer(1_800_000_000)),
                ("chunkArrivalTime", PyRep::Integer(1_800_518_400)),
                ("naturalDecayTime", PyRep::Integer(1_800_529_200)),
                ("chunkVolumeM3", PyRep::Real(1_200_000.0)),
                ("status", PyRep::String("Started".into())),
                ("eventID", PyRep::Integer(1)),
                ("readyToFracture", PyRep::Bool(false)),
                (
                    "moonOres",
                    PyRep::List(vec![PyRep::keyval([
                        ("typeID", PyRep::Integer(45_490)),
                        ("volumeM3", PyRep::Real(400_000.0)),
                        ("yield", PyRep::Real(0.33)),
                    ])]),
                ),
            ])],
            vec![PyRep::keyval([
                ("structureID", PyRep::Integer(1_000_000_010)),
                ("moonID", PyRep::Integer(40_000_001)),
                ("extractionStartTime", PyRep::Integer(1_800_000_000)),
                ("chunkArrivalTime", PyRep::Integer(1_800_518_400)),
                ("naturalDecayTime", PyRep::Integer(1_800_529_200)),
                ("status", PyRep::String("Fractured".into())),
                ("autoFractured", PyRep::Bool(true)),
                ("readyToFracture", PyRep::Bool(true)),
            ])],
            vec![PyRep::keyval([
                ("structureID", PyRep::Integer(1_000_000_010)),
                ("moonID", PyRep::Integer(40_000_001)),
                ("extractionStartTime", PyRep::Integer(1_800_000_000)),
                ("chunkArrivalTime", PyRep::Integer(1_800_518_400)),
                ("naturalDecayTime", PyRep::Integer(1_800_529_200)),
                ("eventID", PyRep::Integer(1)),
                ("readyToFracture", PyRep::Bool(false)),
                ("autoFractured", PyRep::Bool(false)),
                ("fractured", PyRep::Bool(true)),
                ("status", PyRep::String("Fractured".into())),
                ("chunkVolumeM3", PyRep::Real(1_200_000.0)),
                ("asteroidCount", PyRep::Integer(1)),
                ("asteroidIDs", PyRep::List(vec![PyRep::Integer(50_000_001)])),
                ("oreTypes", PyRep::List(vec![PyRep::Integer(45_490)])),
                ("isJackpot", PyRep::Bool(false)),
                ("jackpotBonus", PyRep::Real(1.0)),
            ])],
        ],
    },
    OnMoonScanComplete => "OnMoonScanComplete" ("charid") {
        fields: { moon_id: i64, results: PyRep },
        samples: || vec![vec![
            PyRep::Integer(40_000_001),
            PyRep::Dict(vec![(PyRep::Integer(45_490), PyRep::Real(0.25))]),
        ]],
    },
    OnMutualWarsUpdatedRemote => "OnMutualWarsUpdated_Remote" ("corpid") {
        samples: || vec![vec![]],
    },
    OnNewCalendarEvent => "OnNewCalendarEvent" ("corpid") {
        fields: {
            event_id: i64,
            owner_id: i64,
            event_date_time: WallFileTime,
            event_duration: i64,
            event_title: String,
            importance: i64,
            auto_event_type: i64,
            do_blink: bool,
        },
        // Covers both automated (`i64`) and manual (`None`, via `OnNewCalendarEventOptAuto`) events.
        samples: || vec![
            vec![
                PyRep::Integer(1),
                PyRep::Integer(98_000_001),
                PyRep::Integer(1_335_000_000_000_000_00),
                PyRep::Integer(60),
                PyRep::String("Moon Extraction".into()),
                PyRep::Integer(1),
                PyRep::Integer(1),
                PyRep::Bool(true),
            ],
            vec![
                PyRep::Integer(1),
                PyRep::Integer(90_000_001),
                PyRep::Integer(1_335_000_000_000_000_00),
                PyRep::Integer(60),
                PyRep::String("Event".into()),
                PyRep::Integer(1),
                PyRep::None,
                PyRep::Bool(true),
            ],
        ],
    },
    OnNewPetition => "OnNewPetition" ("charid") {
        fields: { petition_id: i64 },
        samples: || vec![vec![PyRep::Integer(1)]],
    },
    OnNewProbe => "OnNewProbe" ("charid") {
        fields: { probe: PyRep },
        // `services/space_probe_scanning.rs:2634-2655`, `services/fleet_social_scanning.rs:1094-1103`.
        samples: || vec![vec![PyRep::keyval([
            ("probeID", PyRep::Integer(100_000_001)),
            ("typeID", PyRep::Integer(30_013)),
            ("scanRange", PyRep::Real(8.0)),
            ("expiry", PyRep::Integer(133_500_000_000_000_000)),
            ("pos", PyRep::Tuple(vec![PyRep::Real(0.0), PyRep::Real(0.0), PyRep::Real(0.0)])),
            (
                "destination",
                PyRep::Tuple(vec![PyRep::Real(0.0), PyRep::Real(0.0), PyRep::Real(0.0)]),
            ),
            ("state", PyRep::Integer(1)),
            (
                "scanBonuses",
                PyRep::Dict(vec![
                    (
                        PyRep::String("strength".into()),
                        PyRep::Dict(vec![
                            (PyRep::String("modules".into()), PyRep::Real(0.0)),
                            (PyRep::String("ship".into()), PyRep::Real(0.0)),
                        ]),
                    ),
                    (
                        PyRep::String("deviation".into()),
                        PyRep::Dict(vec![
                            (PyRep::String("modules".into()), PyRep::Real(0.0)),
                            (PyRep::String("ship".into()), PyRep::Real(0.0)),
                        ]),
                    ),
                ]),
            ),
        ])]],
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
        // `services/character_contacts.rs:796`: `inWatchlist: None` preserves current state.
        samples: || vec![
            vec![
                PyRep::List(vec![PyRep::Integer(90_000_002)]),
                PyRep::Real(5.0),
                PyRep::Bool(true),
            ],
            vec![
                PyRep::List(vec![PyRep::Integer(90_000_002)]),
                PyRep::Real(5.0),
                PyRep::None,
            ],
        ],
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
        fields: { probe_id: i64, from_pos: PyRep, to_pos: PyRep, start_time: SimFileTime, duration: i64 },
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
        // `services/space_probe_scanning.rs:2273,2606`.
        samples: || vec![vec![PyRep::List(vec![PyRep::keyval([
            ("probeID", PyRep::Integer(100_000_001)),
            (
                "destination",
                PyRep::Tuple(vec![PyRep::Real(0.0), PyRep::Real(0.0), PyRep::Real(0.0)]),
            ),
        ])])]],
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
        samples: || {
            let sold_dict = PyRep::Dict(vec![(PyRep::Integer(0), PyRep::Integer(90_000_001))]);
            let base = |winning_ticket: PyRep, winner_id: PyRep| {
                PyRep::keyval([
                    ("raffle_id", PyRep::Integer(1)),
                    ("raffleID", PyRep::Integer(1)),
                    ("running_id", PyRep::Integer(1)),
                    ("owner_id", PyRep::Integer(90_000_001)),
                    ("location_id", PyRep::Integer(60_003_760)),
                    ("item_id", PyRep::Integer(100_000_001)),
                    ("type_id", PyRep::Integer(587)),
                    ("ticket_count", PyRep::Integer(8)),
                    ("tickets_remaining", PyRep::Integer(7)),
                    ("ticket_price", PyRep::Real(100_000.0)),
                    ("hypercores_used", PyRep::Integer(10)),
                    ("expiration_time", PyRep::Integer(1_775_486_400)),
                    ("sold_tickets", sold_dict.clone()),
                    ("winning_ticket", winning_ticket),
                    ("winner_id", winner_id),
                    ("is_expired", PyRep::Bool(false)),
                    ("is_finished", PyRep::Bool(false)),
                    ("item_awarded", PyRep::Bool(false)),
                ])
            };
            vec![
                vec![PyRep::Integer(1), base(PyRep::None, PyRep::None)],
                vec![PyRep::Integer(1), base(PyRep::Integer(0), PyRep::Integer(90_000_001))],
            ]
        },
    },
    OnRaffleFinishedServer => "OnRaffleFinishedServer" ("charid") {
        fields: { raffle_id: i64, winning_ticket: Option<PyRep> },
        samples: || vec![
            vec![PyRep::Integer(1), PyRep::None],
            vec![PyRep::Integer(1), PyRep::Integer(1)],
        ],
    },
    OnRaffleUpdatedServer => "OnRaffleUpdatedServer" ("charid") {
        fields: { raffle_id: i64, raffle_data: PyRep },
        samples: || {
            let sold_dict = PyRep::Dict(vec![(PyRep::Integer(0), PyRep::Integer(90_000_001))]);
            let base = |winning_ticket: PyRep, winner_id: PyRep| {
                PyRep::keyval([
                    ("raffle_id", PyRep::Integer(1)),
                    ("raffleID", PyRep::Integer(1)),
                    ("running_id", PyRep::Integer(1)),
                    ("owner_id", PyRep::Integer(90_000_001)),
                    ("location_id", PyRep::Integer(60_003_760)),
                    ("item_id", PyRep::Integer(100_000_001)),
                    ("type_id", PyRep::Integer(587)),
                    ("ticket_count", PyRep::Integer(8)),
                    ("tickets_remaining", PyRep::Integer(7)),
                    ("ticket_price", PyRep::Real(100_000.0)),
                    ("hypercores_used", PyRep::Integer(10)),
                    ("expiration_time", PyRep::Integer(1_775_486_400)),
                    ("sold_tickets", sold_dict.clone()),
                    ("winning_ticket", winning_ticket),
                    ("winner_id", winner_id),
                    ("is_expired", PyRep::Bool(false)),
                    ("is_finished", PyRep::Bool(false)),
                    ("item_awarded", PyRep::Bool(false)),
                ])
            };
            vec![
                vec![PyRep::Integer(1), base(PyRep::None, PyRep::None)],
                vec![PyRep::Integer(1), base(PyRep::Integer(0), PyRep::Integer(90_000_001))],
            ]
        },
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
        fields: { event_id: i64, event_date_time: WallFileTime, is_deleted: bool },
        samples: || vec![vec![
            PyRep::Integer(1),
            PyRep::Integer(1_335_000_000_000_000_00),
            PyRep::Bool(true),
        ]],
    },
    OnRemoveMoonProbe => "OnRemoveMoonProbe" ("charid") {
        fields: { probe_id: i64 },
        samples: || vec![vec![PyRep::Integer(1)]],
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
    OnScanNoMaterials => "OnScanNoMaterials" ("charid") {
        fields: { moon_id: i64 },
        samples: || vec![vec![PyRep::Integer(40_000_001)]],
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
        // `services/fleet_social_corp_gov.rs:1012,1025,1103,1113,1895` emits `{"shares": (old, new)}`.
        samples: || vec![vec![
            PyRep::Integer(90_000_001),
            PyRep::Integer(98_000_001),
            PyRep::Dict(vec![(
                PyRep::String("shares".into()),
                PyRep::Tuple(vec![PyRep::Integer(0), PyRep::Integer(500)]),
            )]),
        ]],
    },
    OnSharedBookmarksFolderDeleted => "OnSharedBookmarksFolderDeleted" ("corpid") {
        fields: { folder_id: i64 },
        samples: || vec![vec![PyRep::Integer(1)]],
    },
    OnSharedBookmarksFolderUpdated => "OnSharedBookmarksFolderUpdated" ("corpid") {
        fields: { updates: PyRep },
        // Accepts V24 tuple payloads plus pre-`client_compat` `List[Integer]` (`client_compat.rs:800`);
        // scalar `Integer`/`Bool` updates (`FolderChanged`) are allowlisted in `INCOMPLETE_SAMPLE_EVENTS`.
        samples: || vec![
            vec![PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Tuple(vec![PyRep::Integer(1)]),
                PyRep::String("FolderChanged".into()),
                PyRep::Tuple(vec![
                    PyRep::List(vec![PyRep::keyval([("bookmarkID", PyRep::Integer(1))])]),
                    PyRep::List(vec![PyRep::Integer(1)]),
                ]),
            ])])],
            vec![PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Tuple(vec![PyRep::Integer(1)]),
                PyRep::String("FolderChanged".into()),
                PyRep::List(vec![PyRep::Integer(1)]),
            ])])],
        ],
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
        // `services/fleet_social_scanning.rs:1781-1923` (`build_signal_tracker_anomaly_kv`).
        samples: || vec![vec![
            PyRep::Integer(30_000_142),
            PyRep::Dict(vec![
                (
                    PyRep::String("ANOM-1".into()),
                    PyRep::keyval([
                        ("instanceID", PyRep::String("ANOM-1".into())),
                        (
                            "position",
                            PyRep::Tuple(vec![PyRep::Real(0.0), PyRep::Real(0.0), PyRep::Real(0.0)]),
                        ),
                        ("targetID", PyRep::String("ANOM-1".into())),
                        ("difficulty", PyRep::Integer(1)),
                        ("dungeonID", PyRep::Integer(2_134)),
                        ("archetypeID", PyRep::Integer(24)),
                        ("dungeonName", PyRep::String("Anomaly".into())),
                        ("dungeonNameID", PyRep::None),
                        ("factionID", PyRep::Integer(500_010)),
                        ("scanStrengthAttribute", PyRep::Integer(210)),
                        ("allowedTypes", PyRep::None),
                    ]),
                ),
                (
                    PyRep::String("ANOM-2".into()),
                    PyRep::keyval([
                        ("instanceID", PyRep::String("ANOM-2".into())),
                        (
                            "position",
                            PyRep::Tuple(vec![PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)]),
                        ),
                        ("targetID", PyRep::String("ANOM-2".into())),
                        ("difficulty", PyRep::Integer(1)),
                        ("dungeonID", PyRep::Integer(2_134)),
                        ("archetypeID", PyRep::Integer(24)),
                        ("dungeonName", PyRep::String("Anomaly 2".into())),
                        ("dungeonNameID", PyRep::Integer(1_000)),
                        ("factionID", PyRep::Integer(500_010)),
                        ("scanStrengthAttribute", PyRep::Integer(210)),
                        ("allowedTypes", PyRep::None),
                    ]),
                ),
            ]),
            PyRep::List(vec![PyRep::String("ANOM-OLD".into())]),
        ]],
    },
    OnSignalTrackerSignatureUpdate => "OnSignalTrackerSignatureUpdate" ("charid") {
        fields: { solar_system_id: i64, added_signatures: PyRep, removed_signatures: PyRep },
        // `services/fleet_social_scanning.rs:1650-1885` (`build_signal_tracker_signature_kv`).
        samples: || vec![vec![
            PyRep::Integer(30_000_142),
            PyRep::Dict(vec![(
                PyRep::String("ABC-123".into()),
                PyRep::keyval([
                    (
                        "position",
                        PyRep::Tuple(vec![PyRep::Real(0.0), PyRep::Real(0.0), PyRep::Real(0.0)]),
                    ),
                    ("targetID", PyRep::String("ABC-123".into())),
                    ("difficulty", PyRep::Integer(1)),
                    ("dungeonID", PyRep::None),
                    ("archetypeID", PyRep::None),
                    ("deviation", PyRep::Real(0.0)),
                ]),
            )]),
            PyRep::List(vec![PyRep::String("OLD-999".into())]),
        ]],
    },
    OnSignalTrackerStructureUpdate => "OnSignalTrackerStructureUpdate" ("charid") {
        fields: { solar_system_id: i64, added_structures: PyRep, removed_structures: PyRep },
        // `services/fleet_social_scanning.rs:1806-1960` (`build_signal_tracker_structure_kv`).
        samples: || vec![vec![
            PyRep::Integer(30_000_142),
            PyRep::Dict(vec![(
                PyRep::Integer(1_000_000_000_001),
                PyRep::keyval([
                    ("typeID", PyRep::Integer(35_832)),
                    ("groupID", PyRep::Integer(1_657)),
                    ("categoryID", PyRep::Integer(65)),
                    (
                        "position",
                        PyRep::Tuple(vec![PyRep::Real(0.0), PyRep::Real(0.0), PyRep::Real(0.0)]),
                    ),
                    ("targetID", PyRep::Integer(1_000_000_000_001)),
                ]),
            )]),
            PyRep::List(vec![PyRep::Integer(1_000_000_000_002)]),
        ]],
    },
    OnSkyhookCanBeHacked => "OnSkyhookCanBeHacked" ("solarsystemid") {
        fields: { solar_system_id: i64, skyhook_id: i64 },
        samples: || vec![vec![PyRep::Integer(30_000_142), PyRep::Integer(1_000_000_000_001)]],
    },
    OnSlimItemChange => "OnSlimItemChange" ("solarsystemid") @ballpark {
        fields: { old_slim: PyRep, new_slim: SlimItemKeyVal },
        samples: || {
            let ship_all_none = SlimItemKeyVal::for_space_ship(SpaceShipSlimFields {
                item_id: 100_000_001, type_id: 587, group_id: 25, category_id: 6,
                owner_id: 90_000_001, char_id: None, corp_id: None,
                alliance_id: None, name: "Rifter".into(),
            })
            .into_notify_arg();
            let ship_with_ally = SlimItemKeyVal::for_space_ship(SpaceShipSlimFields {
                item_id: 100_000_001, type_id: 587, group_id: 25, category_id: 6,
                owner_id: 90_000_001, char_id: Some(90_000_001), corp_id: Some(98_000_001),
                alliance_id: Some(99_000_001), name: "Rifter".into(),
            })
            .into_notify_arg();
            let unowned_obj = SlimItemKeyVal::for_space_ship(SpaceShipSlimFields {
                item_id: 100_000_050, type_id: 26_505, group_id: 186, category_id: 2,
                owner_id: 0, char_id: None, corp_id: None,
                alliance_id: None, name: String::new(),
            })
            .with_extra_pairs([
                ("securityStatus", PyRep::Real(0.0)),
                ("isEmpty", PyRep::Bool(true)),
            ])
            .into_notify_arg();
            let ship_cosmetics_none = SlimItemKeyVal::for_space_ship(SpaceShipSlimFields {
                item_id: 100_000_001, type_id: 587, group_id: 25, category_id: 6,
                owner_id: 90_000_001, char_id: Some(90_000_001), corp_id: Some(98_000_001),
                alliance_id: None, name: String::new(),
            })
            .with_extra_pairs([
                ("securityStatus", PyRep::Real(0.0)),
                ("skinID", PyRep::None),
                ("skinMaterialSetID", PyRep::None),
            ])
            .into_notify_arg();
            let ship_cosmetics_some = SlimItemKeyVal::for_space_ship(SpaceShipSlimFields {
                item_id: 100_000_001, type_id: 587, group_id: 25, category_id: 6,
                owner_id: 90_000_001, char_id: Some(90_000_001), corp_id: Some(98_000_001),
                alliance_id: Some(99_000_001), name: String::new(),
            })
            .with_extra_pairs([
                ("securityStatus", PyRep::Real(0.0)),
                ("skinID", PyRep::Integer(1)),
                ("skinMaterialSetID", PyRep::Integer(1)),
            ])
            .into_notify_arg();
            let shipcaster_pad = SlimItemKeyVal::for_space_ship(SpaceShipSlimFields {
                item_id: 100_000_060, type_id: 45_511, group_id: 4_547, category_id: 2,
                owner_id: 500_001, char_id: None, corp_id: None,
                alliance_id: None, name: String::new(),
            })
            .with_extra_pairs([
                (
                    "component_underConstruction_progress",
                    PyRep::Dict(vec![(PyRep::Integer(34), PyRep::Integer(100))]),
                ),
                ("component_underConstruction_completed", PyRep::Bool(false)),
            ])
            .into_notify_arg();
            let crab_base = |corp: Option<i64>, ally: Option<i64>| {
                SlimItemKeyVal::for_space_ship(SpaceShipSlimFields {
                    item_id: 100_000_070, type_id: 60_244, group_id: 4_137, category_id: 22,
                    owner_id: 90_000_001, char_id: None, corp_id: corp,
                    alliance_id: ally, name: "CONCORD Rogue Analysis Beacon".into(),
                })
            };
            let crab_idle_none = crab_base(None, None).into_notify_arg();
            let crab_idle_some = crab_base(Some(98_000_001), Some(99_000_001)).into_notify_arg();
            let crab_linking_none = crab_base(None, None)
                .with_extra_pairs([
                    ("crabPhase", PyRep::String("Linking".into())),
                    ("crabLinkedShipID", PyRep::Integer(100_000_001)),
                    ("crabLinkCompletesAt", PyRep::Integer(1_775_400_240)),
                ])
                .into_notify_arg();
            let crab_linking_some = crab_base(Some(98_000_001), Some(99_000_001))
                .with_extra_pairs([
                    ("crabPhase", PyRep::String("Linking".into())),
                    ("crabLinkedShipID", PyRep::Integer(100_000_001)),
                    ("crabLinkCompletesAt", PyRep::Integer(1_775_400_240)),
                ])
                .into_notify_arg();
            let crab_scanning_none = crab_base(None, None)
                .with_extra_pairs([("crabPhase", PyRep::String("Scanning".into()))])
                .into_notify_arg();
            let crab_scanning_some = crab_base(Some(98_000_001), Some(99_000_001))
                .with_extra_pairs([("crabPhase", PyRep::String("Scanning".into()))])
                .into_notify_arg();
            let crab_loot_none = crab_base(None, None)
                .with_extra_pairs([
                    ("crabPhase", PyRep::String("LootRestricted".into())),
                    ("crabSelfDestructAt", PyRep::Integer(1_775_400_960)),
                ])
                .into_notify_arg();
            let crab_loot_some = crab_base(Some(98_000_001), Some(99_000_001))
                .with_extra_pairs([
                    ("crabPhase", PyRep::String("LootRestricted".into())),
                    ("crabSelfDestructAt", PyRep::Integer(1_775_400_960)),
                ])
                .into_notify_arg();
            let crab_destructed_none = crab_base(None, None)
                .with_extra_pairs([
                    ("crabPhase", PyRep::String("SelfDestructed".into())),
                    ("openContainerID", PyRep::Integer(0)),
                ])
                .into_notify_arg();
            let crab_destructed_some = crab_base(Some(98_000_001), Some(99_000_001))
                .with_extra_pairs([
                    ("crabPhase", PyRep::String("SelfDestructed".into())),
                    ("openContainerID", PyRep::Integer(0)),
                ])
                .into_notify_arg();
            vec![
                vec![ship_all_none.clone(), ship_all_none],
                vec![ship_with_ally.clone(), ship_with_ally],
                vec![unowned_obj.clone(), unowned_obj],
                vec![ship_cosmetics_none.clone(), ship_cosmetics_some.clone()],
                vec![ship_cosmetics_some.clone(), ship_cosmetics_none],
                vec![shipcaster_pad.clone(), shipcaster_pad],
                vec![crab_idle_none.clone(), crab_linking_none.clone()],
                vec![crab_idle_some.clone(), crab_linking_some.clone()],
                vec![crab_linking_none.clone(), crab_loot_none.clone()],
                vec![crab_linking_some.clone(), crab_loot_some.clone()],
                vec![crab_scanning_none.clone(), crab_loot_none.clone()],
                vec![crab_scanning_some.clone(), crab_loot_some.clone()],
                vec![crab_idle_none, crab_destructed_none.clone()],
                vec![crab_idle_some, crab_destructed_some.clone()],
                vec![crab_linking_none, crab_destructed_none.clone()],
                vec![crab_linking_some, crab_destructed_some.clone()],
                vec![crab_scanning_none, crab_destructed_none.clone()],
                vec![crab_scanning_some, crab_destructed_some.clone()],
                vec![crab_loot_none, crab_destructed_none],
                vec![crab_loot_some, crab_destructed_some],
            ]
        },
    },
    OnSolarSystemDevIndexChanged => "OnSolarSystemDevIndexChanged" ("solarsystemid") {
        fields: { solar_system_id: i64 },
        samples: || vec![vec![PyRep::Integer(30_000_142)]],
    },
    OnSolarSystemSovStructuresUpdated => "OnSolarSystemSovStructuresUpdated" ("solarsystemid") {
        fields: { solar_system_id: i64, solar_system_structures_info: PyRep, changes: PyRep },
        // `services/sov_services.rs:290-305,492-515`.
        samples: || vec![
            vec![
                PyRep::Integer(30_000_142),
                PyRep::List(vec![PyRep::keyval([
                    ("itemID", PyRep::Integer(1_000_000_000_001)),
                    ("structureID", PyRep::Integer(1_000_000_000_001)),
                    ("typeID", PyRep::Integer(35832)),
                    ("corporationID", PyRep::Integer(98_000_001)),
                    ("allianceID", PyRep::None),
                    ("solarSystemID", PyRep::Integer(30_000_142)),
                    ("vulnerableStartTime", PyRep::Integer(0)),
                    ("vulnerableEndTime", PyRep::Integer(0)),
                ])]),
                PyRep::Dict(vec![(
                    PyRep::Integer(1_000_000_000_001),
                    PyRep::List(vec![PyRep::Integer(0)]),
                )]),
            ],
            vec![
                PyRep::Integer(30_000_142),
                PyRep::List(vec![PyRep::keyval([
                    ("itemID", PyRep::Integer(1_000_000_000_001)),
                    ("structureID", PyRep::Integer(1_000_000_000_001)),
                    ("typeID", PyRep::Integer(35832)),
                    ("corporationID", PyRep::Integer(98_000_001)),
                    ("allianceID", PyRep::Integer(99_000_001)),
                    ("solarSystemID", PyRep::Integer(30_000_142)),
                    ("vulnerableStartTime", PyRep::Integer(0)),
                    ("vulnerableEndTime", PyRep::Integer(0)),
                ])]),
                PyRep::Dict(vec![(
                    PyRep::Integer(1_000_000_000_001),
                    PyRep::List(vec![PyRep::Integer(0)]),
                )]),
            ],
        ],
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
        // `services/sov_gameplay_entosis_capture.rs:353` & `sov_services.rs:457` emit system/alliance IDs.
        samples: || vec![vec![
            PyRep::Integer(1),
            PyRep::Dict(vec![
                (PyRep::String("solarSystemID".into()), PyRep::Integer(30_000_142)),
                (PyRep::String("allianceID".into()), PyRep::Integer(99_000_001)),
            ]),
        ]],
    },
    OnSovereigntyChanged => "OnSovereigntyChanged" ("solarsystemid") {
        fields: { solar_system_id: i64, alliance_id: Option<i64> },
        // `services/sov_services.rs:55` emits `None` when sovereignty becomes unowned.
        samples: || vec![
            vec![PyRep::Integer(30_000_142), PyRep::Integer(99_000_001)],
            vec![PyRep::Integer(30_000_142), PyRep::None],
        ],
    },
    OnStructureHacked => "OnStructureHacked" ("corpid") {
        fields: { structure_id: i64, reinforce_timing: PyRep },
        // `services/structure_lifecycle.rs:2093-2096` emits `None` in slots `2..=4` when unscheduled.
        samples: || vec![
            vec![
                PyRep::Integer(1_000_000_000_001),
                PyRep::Tuple(vec![
                    PyRep::Integer(5),
                    PyRep::Integer(18),
                    PyRep::Integer(6),
                    PyRep::Integer(20),
                    PyRep::Integer(1_700_000_000),
                ]),
            ],
            vec![
                PyRep::Integer(1_000_000_000_001),
                PyRep::Tuple(vec![
                    PyRep::Integer(5),
                    PyRep::Integer(18),
                    PyRep::None,
                    PyRep::None,
                    PyRep::None,
                ]),
            ],
        ],
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
        fields: { start_time: SimFileTime, duration_ms: i64, probes: PyRep },
        // `space_probe_scanning_math.rs:126-143` (`probe_scan_started_probes_dict`).
        samples: || vec![
            vec![
                PyRep::Integer(1_335_000_000_000_000_00),
                PyRep::Integer(5000),
                PyRep::List(vec![PyRep::Integer(100_000_001)]),
            ],
            vec![
                PyRep::Integer(1_335_000_000_000_000_00),
                PyRep::Integer(5000),
                PyRep::Dict(vec![(
                    PyRep::Integer(100_000_001),
                    PyRep::keyval([
                        (
                            "pos",
                            PyRep::Tuple(vec![PyRep::Real(0.0), PyRep::Real(0.0), PyRep::Real(0.0)]),
                        ),
                        ("state", PyRep::Integer(4)),
                    ]),
                )]),
            ],
        ],
    },
    OnSystemScanStopped => "OnSystemScanStopped" ("charid") {
        fields: { probes: Vec<PyRep>, results: Vec<PyRep>, absent_targets: Vec<PyRep> },
        // `services/space_probe_scanning.rs:2066-2124`, `:2237-2267`, `:3253-3278`.
        samples: || vec![vec![
            PyRep::List(vec![PyRep::Integer(100_000_001)]),
            PyRep::List(sample_scan_result_kvs()),
            PyRep::List(Vec::new()),
        ]],
    },
    OnTarget => "OnTarget" ("charid") @ballpark {
        fields: { arg0: PyRep, arg1: PyRep },
        optional: { reason: Option<PyRep> },
        // V24 `(what, tid[, reason])` plus pre-`client_compat` `(tid, what[, reason])`
        // emitted by `system_actor/cycles.rs:2281` & `fitting_dogma_*` (`client_compat.rs:913`).
        samples: || vec![
            vec![PyRep::String("add".into()), PyRep::Integer(100_000_001)],
            vec![PyRep::String("add".into()), PyRep::Integer(100_000_001), PyRep::None],
            vec![PyRep::Integer(100_000_001), PyRep::String("locking".into())],
            vec![PyRep::Integer(100_000_001), PyRep::String("locking".into()), PyRep::None],
        ],
    },
    OnTargetOBO => "OnTargetOBO" ("charid") @ballpark {
        fields: { what: String, sid: i64, tid: Option<i64>, reason: Option<PyRep> },
        // `wallet_serverops.rs:991` ("clear", None, None) and `fitting_dogma_ops.rs:343,444` ("add"/"lost", Some, None|String).
        samples: || vec![
            vec![
                PyRep::String("clear".into()),
                PyRep::Integer(100_000_001),
                PyRep::None,
                PyRep::None,
            ],
            vec![
                PyRep::String("add".into()),
                PyRep::Integer(100_000_001),
                PyRep::Integer(100_000_002),
                PyRep::None,
            ],
            vec![
                PyRep::String("lost".into()),
                PyRep::Integer(100_000_001),
                PyRep::Integer(100_000_002),
                PyRep::String("unlocked".into()),
            ],
        ],
    },
    OnTicketsUpdatedServer => "OnTicketsUpdatedServer" ("charid") {
        fields: { ticket_updates: PyRep },
        samples: || vec![vec![PyRep::Dict(vec![(PyRep::Integer(1), PyRep::Integer(5))])]],
    },
    OnTitleChanged => "OnTitleChanged" ("corpid") {
        fields: { corp_id: i64, title_id: i64, change: PyRep },
        // `services/fleet_social_corp_gov.rs:1406,1478`.
        samples: || vec![vec![
            PyRep::Integer(98_000_001),
            PyRep::Integer(1),
            PyRep::Dict(vec![
                (
                    PyRep::String("titleName".into()),
                    PyRep::Tuple(vec![PyRep::None, PyRep::String("Director".into())]),
                ),
                (
                    PyRep::String("titleName".into()),
                    PyRep::Tuple(vec![PyRep::String("Old".into()), PyRep::String("Director".into())]),
                ),
                (
                    PyRep::String("roles".into()),
                    PyRep::Tuple(vec![PyRep::Integer(0), PyRep::Integer(1)]),
                ),
            ]),
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
        // `services/social_wars_helpers.rs:185,232` & `services/social_wars.rs:322-415,534,946,989,1009,1042,1254`.
        // Note: outer `vec![` must stay first after `samples:` for `notify_registry_timestamps_exhaustive_guard`.
        samples: || vec![
            vec![
                sample_war_kv(false, false),
                PyRep::List(Vec::from([PyRep::Integer(98_000_001), PyRep::Integer(98_000_002)])),
                PyRep::Dict(Vec::from([
                    (
                        PyRep::String("warID".into()),
                        PyRep::Tuple(Vec::from([PyRep::None, PyRep::Integer(1)])),
                    ),
                    (
                        PyRep::String("declaredByID".into()),
                        PyRep::Tuple(Vec::from([PyRep::None, PyRep::Integer(98_000_001)])),
                    ),
                    (
                        PyRep::String("againstID".into()),
                        PyRep::Tuple(Vec::from([PyRep::None, PyRep::Integer(98_000_002)])),
                    ),
                    (
                        PyRep::String("allyID".into()),
                        PyRep::Tuple(Vec::from([PyRep::None, PyRep::Integer(98_000_003)])),
                    ),
                    (
                        PyRep::String("allyID".into()),
                        PyRep::Tuple(Vec::from([PyRep::Integer(98_000_003), PyRep::Integer(98_000_003)])),
                    ),
                    (
                        PyRep::String("allyID".into()),
                        PyRep::Tuple(Vec::from([PyRep::Integer(98_000_003), PyRep::None])),
                    ),
                    (
                        PyRep::String("allyID".into()),
                        PyRep::Tuple(Vec::from([PyRep::None, PyRep::None])),
                    ),
                    (
                        PyRep::String("active".into()),
                        PyRep::Tuple(Vec::from([PyRep::Bool(true), PyRep::Bool(false)])),
                    ),
                    (
                        PyRep::String("combatActive".into()),
                        PyRep::Tuple(Vec::from([PyRep::Bool(false), PyRep::Bool(true)])),
                    ),
                    (
                        PyRep::String("mutual".into()),
                        PyRep::Tuple(Vec::from([PyRep::Bool(false), PyRep::Bool(true)])),
                    ),
                    (
                        PyRep::String("mutual".into()),
                        PyRep::Tuple(Vec::from([PyRep::None, PyRep::Bool(true)])),
                    ),
                    (
                        PyRep::String("openForAllies".into()),
                        PyRep::Tuple(Vec::from([PyRep::Bool(false), PyRep::Bool(true)])),
                    ),
                    (
                        PyRep::String("timeDeclared".into()),
                        PyRep::Tuple(Vec::from([PyRep::None, PyRep::Integer(1_335_000_000_000_000_00)])),
                    ),
                    (
                        PyRep::String("timeDeclared".into()),
                        PyRep::Tuple(Vec::from([
                            PyRep::Integer(1_335_000_000_000_000_00),
                            PyRep::Integer(1_335_000_001_000_000_00),
                        ])),
                    ),
                    (
                        PyRep::String("timeStarted".into()),
                        PyRep::Tuple(Vec::from([PyRep::None, PyRep::Integer(1_335_000_000_000_000_00)])),
                    ),
                    (
                        PyRep::String("timeStarted".into()),
                        PyRep::Tuple(Vec::from([
                            PyRep::Integer(1_335_000_000_000_000_00),
                            PyRep::Integer(1_335_000_001_000_000_00),
                        ])),
                    ),
                    (
                        PyRep::String("timeFinished".into()),
                        PyRep::Tuple(Vec::from([PyRep::None, PyRep::Integer(1_335_000_000_000_000_00)])),
                    ),
                    (
                        PyRep::String("timeFinished".into()),
                        PyRep::Tuple(Vec::from([
                            PyRep::Integer(1_335_000_000_000_000_00),
                            PyRep::Integer(1_335_000_001_000_000_00),
                        ])),
                    ),
                    (
                        PyRep::String("timeFinished".into()),
                        PyRep::Tuple(Vec::from([PyRep::Integer(1_335_000_000_000_000_00), PyRep::None])),
                    ),
                    (
                        PyRep::String("timeFinished".into()),
                        PyRep::Tuple(Vec::from([PyRep::None, PyRep::None])),
                    ),
                    (
                        PyRep::String("retracted".into()),
                        PyRep::Tuple(Vec::from([PyRep::None, PyRep::Integer(1_335_000_000_000_000_00)])),
                    ),
                    (
                        PyRep::String("warHQ".into()),
                        PyRep::Tuple(Vec::from([PyRep::None, PyRep::Integer(1_000_000_000_001)])),
                    ),
                    (
                        PyRep::String("warHQ".into()),
                        PyRep::Tuple(Vec::from([PyRep::Integer(1_000_000_000_001), PyRep::None])),
                    ),
                ])),
            ],
            vec![
                sample_war_kv(true, true),
                PyRep::List(Vec::from([PyRep::Integer(98_000_001), PyRep::Integer(98_000_002)])),
                PyRep::Dict(Vec::from([
                    (
                        PyRep::String("warID".into()),
                        PyRep::Tuple(Vec::from([PyRep::None, PyRep::Integer(1)])),
                    ),
                    (
                        PyRep::String("declaredByID".into()),
                        PyRep::Tuple(Vec::from([PyRep::None, PyRep::Integer(98_000_001)])),
                    ),
                    (
                        PyRep::String("againstID".into()),
                        PyRep::Tuple(Vec::from([PyRep::None, PyRep::Integer(98_000_002)])),
                    ),
                    (
                        PyRep::String("allyID".into()),
                        PyRep::Tuple(Vec::from([PyRep::None, PyRep::Integer(98_000_003)])),
                    ),
                    (
                        PyRep::String("allyID".into()),
                        PyRep::Tuple(Vec::from([PyRep::Integer(98_000_003), PyRep::Integer(98_000_003)])),
                    ),
                    (
                        PyRep::String("allyID".into()),
                        PyRep::Tuple(Vec::from([PyRep::Integer(98_000_003), PyRep::None])),
                    ),
                    (
                        PyRep::String("allyID".into()),
                        PyRep::Tuple(Vec::from([PyRep::None, PyRep::None])),
                    ),
                    (
                        PyRep::String("active".into()),
                        PyRep::Tuple(Vec::from([PyRep::Bool(true), PyRep::Bool(false)])),
                    ),
                    (
                        PyRep::String("combatActive".into()),
                        PyRep::Tuple(Vec::from([PyRep::Bool(false), PyRep::Bool(true)])),
                    ),
                    (
                        PyRep::String("mutual".into()),
                        PyRep::Tuple(Vec::from([PyRep::Bool(false), PyRep::Bool(true)])),
                    ),
                    (
                        PyRep::String("mutual".into()),
                        PyRep::Tuple(Vec::from([PyRep::None, PyRep::Bool(true)])),
                    ),
                    (
                        PyRep::String("openForAllies".into()),
                        PyRep::Tuple(Vec::from([PyRep::Bool(false), PyRep::Bool(true)])),
                    ),
                    (
                        PyRep::String("timeDeclared".into()),
                        PyRep::Tuple(Vec::from([PyRep::None, PyRep::Integer(1_335_000_000_000_000_00)])),
                    ),
                    (
                        PyRep::String("timeDeclared".into()),
                        PyRep::Tuple(Vec::from([
                            PyRep::Integer(1_335_000_000_000_000_00),
                            PyRep::Integer(1_335_000_001_000_000_00),
                        ])),
                    ),
                    (
                        PyRep::String("timeStarted".into()),
                        PyRep::Tuple(Vec::from([PyRep::None, PyRep::Integer(1_335_000_000_000_000_00)])),
                    ),
                    (
                        PyRep::String("timeStarted".into()),
                        PyRep::Tuple(Vec::from([
                            PyRep::Integer(1_335_000_000_000_000_00),
                            PyRep::Integer(1_335_000_001_000_000_00),
                        ])),
                    ),
                    (
                        PyRep::String("timeFinished".into()),
                        PyRep::Tuple(Vec::from([PyRep::None, PyRep::Integer(1_335_000_000_000_000_00)])),
                    ),
                    (
                        PyRep::String("timeFinished".into()),
                        PyRep::Tuple(Vec::from([
                            PyRep::Integer(1_335_000_000_000_000_00),
                            PyRep::Integer(1_335_000_001_000_000_00),
                        ])),
                    ),
                    (
                        PyRep::String("timeFinished".into()),
                        PyRep::Tuple(Vec::from([PyRep::Integer(1_335_000_000_000_000_00), PyRep::None])),
                    ),
                    (
                        PyRep::String("timeFinished".into()),
                        PyRep::Tuple(Vec::from([PyRep::None, PyRep::None])),
                    ),
                    (
                        PyRep::String("retracted".into()),
                        PyRep::Tuple(Vec::from([PyRep::None, PyRep::Integer(1_335_000_000_000_000_00)])),
                    ),
                    (
                        PyRep::String("warHQ".into()),
                        PyRep::Tuple(Vec::from([PyRep::None, PyRep::Integer(1_000_000_000_001)])),
                    ),
                    (
                        PyRep::String("warHQ".into()),
                        PyRep::Tuple(Vec::from([PyRep::Integer(1_000_000_000_001), PyRep::None])),
                    ),
                ])),
            ],
        ],
    },
    ProcessSovStatusChanged => "ProcessSovStatusChanged" ("solarsystemid") {
        fields: { solar_system_id: i64, sov_info: PyRep },
        // `services/sov_services.rs:467-474`.
        samples: || vec![vec![
            PyRep::Integer(30_000_142),
            PyRep::keyval([
                ("solarSystemID", PyRep::Integer(30_000_142)),
                ("allianceID", PyRep::Integer(99_000_001)),
                ("corporationID", PyRep::Integer(98_000_001)),
            ]),
        ]],
    },
    // Batch 3 (#932) registrations
    DoDestinyUpdate => "DoDestinyUpdate" ("clientID") {
        fields: { state: PyRep, wait_for_bubble: PyRep },
        optional: { dogma_messages: Option<PyRep>, extra: Option<PyRep> },
        samples: || vec![
            vec![
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
            ],
            vec![
                PyRep::List(vec![PyRep::Tuple(vec![
                    PyRep::Integer(1),
                    PyRep::Tuple(vec![
                        PyRep::String("RemoveBalls".into()),
                        PyRep::Tuple(vec![PyRep::List(vec![PyRep::Integer(100_000_001)])]),
                    ]),
                ])]),
                PyRep::Bool(false),
                PyRep::List(vec![PyRep::Tuple(vec![
                    PyRep::String("OnEffectMessage".into()),
                    PyRep::Tuple(Vec::new()),
                ])]),
            ],
        ],
    },
    OnAbyssalContentFinished => "OnAbyssalContentFinished" ("charid") {
        optional: { content_id: Option<i64>, timer_type: Option<i64> },
        samples: || vec![vec![PyRep::Integer(1), PyRep::Integer(1)]],
    },
    OnAbyssalLocationExited => "OnAbyssalLocationExited" ("charid") {
        samples: || vec![vec![]],
    },
    OnAddExpiryDurationForVoidSpaceContent => "OnAddExpiryDurationForVoidSpaceContent" ("charid") {
        fields: { end_time: SimFileTime },
        samples: || vec![vec![PyRep::Integer(133_500_000_000_000_000)]],
    },
    OnCharacterEnteredStructure => "OnCharacterEnteredStructure" ("stationid") {
        fields: {
            character_id: i64,
            corporation_id: i64,
            alliance_id: Option<i64>,
            faction_id: Option<i64>,
        },
        // `services/structure_upwell_ops.rs:1399` emits optional `alliance_id` / `faction_id`.
        samples: || vec![
            vec![PyRep::Integer(90_000_001), PyRep::Integer(98_000_001), PyRep::None, PyRep::None],
            vec![PyRep::Integer(90_000_001), PyRep::Integer(98_000_001), PyRep::Integer(99_000_001), PyRep::None],
            vec![PyRep::Integer(90_000_001), PyRep::Integer(98_000_001), PyRep::None, PyRep::Integer(500_001)],
            vec![PyRep::Integer(90_000_001), PyRep::Integer(98_000_001), PyRep::Integer(99_000_001), PyRep::Integer(500_001)],
        ],
    },
    OnCharacterLeftStructure => "OnCharacterLeftStructure" ("stationid") {
        fields: { character_id: i64 },
        samples: || vec![vec![PyRep::Integer(90_000_001)]],
    },
    OnClientNodeGraphMessage => "OnClientNodeGraphMessage" ("charid") {
        fields: { node_graph_instance_id: i64, key: String, value: PyRep },
        // `nodegraph/client/service.pyj:NodeGraphService.OnClientNodeGraphMessage` expects 3 args:
        // `(node_graph_instance_id, key, value)` (`air_npe_nodegraph_runner.rs:446`).
        samples: || vec![
            vec![
                PyRep::Integer(1),
                PyRep::String("pocket_entered".into()),
                PyRep::keyval([
                    ("dungeonID", PyRep::Integer(1)),
                    ("instanceID", PyRep::Integer(1)),
                    ("solarSystemID", PyRep::Integer(30_000_142)),
                    ("checkpointID", PyRep::Integer(14)),
                ]),
            ],
            vec![
                PyRep::Integer(1),
                PyRep::String("checkpoint_completed".into()),
                PyRep::keyval([
                    ("completedCheckpointID", PyRep::Integer(14)),
                    ("nextCheckpointID", PyRep::Integer(15)),
                    ("act", PyRep::Integer(1)),
                    ("scene", PyRep::String("s1".into())),
                    ("objective", PyRep::String("obj".into())),
                ]),
            ],
            vec![
                PyRep::Integer(1),
                PyRep::String("msg".into()),
                PyRep::Integer(1),
            ],
            vec![
                PyRep::Integer(1),
                PyRep::String("msg".into()),
                PyRep::None,
            ],
        ],
    },
    OnClientNodeGraphUpdate => "OnClientNodeGraphUpdate" ("charid") {
        fields: { node_graph_instance_id: i64, blackboard_parameters: PyRep },
        // `nodegraph/client/service.pyj:NodeGraphService.OnClientNodeGraphUpdate` expects 2 args:
        // `(node_graph_instance_id, blackboard_parameters)` where `bb_to_py_dict`
        // (`services/air_npe_nodegraph_runner.rs:352`) produces `String -> Integer|Bool|String`.
        samples: || vec![vec![
            PyRep::Integer(1),
            PyRep::Dict(vec![
                (PyRep::String("progress".into()), PyRep::Integer(1)),
                (PyRep::String("moved_into_pocket".into()), PyRep::Bool(false)),
                (PyRep::String("act_label".into()), PyRep::String("act1".into())),
            ]),
        ]],
    },
    OnCorpAllowedEnlistmentFactionsSet => "OnCorpAllowedEnlistmentFactionsSet" ("corpid") {
        // `CorporationWindow.OnCorpAllowedEnlistmentFactionsSet(self)`: exact V24 0-arg signature.
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
        // `services/fw_enlistment.rs:332`: all three faction IDs are `Option<i64>`.
        samples: || vec![
            vec![PyRep::Integer(500_001), PyRep::Integer(500_001), PyRep::Integer(500_001)],
            vec![PyRep::Integer(500_001), PyRep::Integer(500_001), PyRep::None],
            vec![PyRep::Integer(500_001), PyRep::None, PyRep::Integer(500_001)],
            vec![PyRep::Integer(500_001), PyRep::None, PyRep::None],
            vec![PyRep::None, PyRep::Integer(500_001), PyRep::Integer(500_001)],
            vec![PyRep::None, PyRep::Integer(500_001), PyRep::None],
            vec![PyRep::None, PyRep::None, PyRep::Integer(500_001)],
            vec![PyRep::None, PyRep::None, PyRep::None],
        ],
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
        fields: { owner_id: i64, fit_id: i64 },
        samples: || vec![vec![PyRep::Integer(90_000_001), PyRep::Integer(1)]],
    },
    OnFittingDeleted => "OnFittingDeleted" ("charid") {
        fields: { owner_id: i64, fit_id: i64 },
        samples: || vec![vec![PyRep::Integer(90_000_001), PyRep::Integer(1)]],
    },
    OnFittingsUpdated => "OnFittingsUpdated" ("charid") {
        fields: { fitting: UpdatedFittingPayload },
        samples: || vec![
            vec![UpdatedFittingPayload::Fitting(UpdatedFittingKeyVal {
                fitting_id: 1, owner_id: 90_000_001, ship_type_id: 587,
                name: "Rifter Fit".into(), description: String::new(), fit_data: vec![(3170, 12, 1)],
            }).into_notify_arg()],
            vec![UpdatedFittingPayload::Cleared.into_notify_arg()],
        ],
    },
    OnFleetDisbanded => "OnFleetDisbanded" ("charid") {
        fields: { char_ids: Vec<i64> },
        samples: || vec![vec![PyRep::List(vec![PyRep::Integer(90_000_001)])]],
    },
    OnFleetInvite => "OnFleetInvite" ("charid") {
        fields: { fleet_id: i64, invite_id: i64, msg_name: FleetInviteMsgName, msg_dict: FleetInviteMsgDict },
        samples: || vec![vec![
            PyRep::Integer(1), PyRep::Integer(90_000_001),
            FleetInviteMsgName::AskJoinFleet.into_notify_arg(),
            FleetInviteMsgDict { auto_accept: false }.into_notify_arg(),
        ]],
    },
    OnFleetJoinReject => "OnFleetJoinReject" ("charid") {
        fields: { member_id: i64, reason_code: PyRep },
        samples: || vec![
            vec![PyRep::Integer(90_000_002), PyRep::Integer(1)],
            vec![PyRep::Integer(90_000_002), PyRep::None],
        ],
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
        // `services/fitting_scanners_superweapons.rs:694-702`, `services/sov_gameplay.rs:1545-1553`.
        samples: || vec![vec![PyRep::Dict(vec![
            (PyRep::String("moduleTypeID".into()), PyRep::Integer(22_175)),
            (PyRep::String("moduleID".into()), PyRep::Integer(100_000_020)),
            (PyRep::String("containerID".into()), PyRep::Integer(100_000_050)),
            (PyRep::String("targetID".into()), PyRep::Integer(100_000_050)),
        ])]],
    },
    OnHackingVirusChanged => "OnHackingVirusChanged" ("charid") {
        fields: { object_data: PyRep, event_id: i64 },
        // `fitting_scanners_superweapons.rs:709` & `sov_gameplay.rs:1560`.
        samples: || vec![vec![
            PyRep::keyval([
                ("containerID", PyRep::Integer(100_000_050)),
                ("coherence", PyRep::Real(60.0)),
                ("strength", PyRep::Real(20.0)),
            ]),
            PyRep::Integer(1),
        ]],
    },
    OnHangarViewStateUpdated => "OnHangarViewStateUpdated" ("charid") {
        fields: { structure_id: i64, hangar_view_state: PyRep },
        // `services/structure_upwell_ops.rs:1287-1380`: timers emit `Real` progress + `Integer` FILETIME,
        // and unshipped pilots emit `shipID = None` / `shipTypeID = None`.
        samples: || vec![
            vec![
                PyRep::Integer(1_000_000_000_001),
                PyRep::keyval([
                    ("characterID", PyRep::Integer(90_000_001)),
                    ("structureID", PyRep::Integer(1_000_000_000_001)),
                    ("structureTypeID", PyRep::Integer(35_834)),
                    ("shipID", PyRep::Integer(1_000_000_000_002)),
                    ("shipTypeID", PyRep::Integer(606)),
                    ("upkeepState", PyRep::Integer(1)),
                    ("operatingState", PyRep::Integer(110)),
                    ("state", PyRep::Integer(110)),
                    ("damageState", PyRep::Tuple(vec![PyRep::Real(1.0), PyRep::Real(1.0), PyRep::Real(1.0)])),
                    ("timerProgress", PyRep::None),
                    ("timerIsProgressing", PyRep::Bool(false)),
                    ("timerIsPaused", PyRep::Bool(false)),
                    ("timerEndAt", PyRep::None),
                ]),
            ],
            vec![
                PyRep::Integer(1_000_000_000_001),
                PyRep::keyval([
                    ("characterID", PyRep::Integer(90_000_001)),
                    ("structureID", PyRep::Integer(1_000_000_000_001)),
                    ("structureTypeID", PyRep::Integer(35_834)),
                    ("shipID", PyRep::Integer(1_000_000_000_002)),
                    ("shipTypeID", PyRep::Integer(606)),
                    ("upkeepState", PyRep::Integer(1)),
                    ("operatingState", PyRep::Integer(110)),
                    ("state", PyRep::Integer(110)),
                    ("damageState", PyRep::Tuple(vec![PyRep::Real(1.0), PyRep::Real(1.0), PyRep::Real(1.0)])),
                    ("timerProgress", PyRep::Real(0.5)),
                    ("timerIsProgressing", PyRep::Bool(true)),
                    ("timerIsPaused", PyRep::Bool(false)),
                    ("timerEndAt", PyRep::Integer(1_335_000_000_000_000_00)),
                ]),
            ],
            vec![
                PyRep::Integer(1_000_000_000_001),
                PyRep::keyval([
                    ("characterID", PyRep::Integer(90_000_001)),
                    ("structureID", PyRep::Integer(1_000_000_000_001)),
                    ("structureTypeID", PyRep::Integer(35_834)),
                    ("shipID", PyRep::None),
                    ("shipTypeID", PyRep::None),
                    ("upkeepState", PyRep::Integer(1)),
                    ("operatingState", PyRep::Integer(110)),
                    ("state", PyRep::Integer(110)),
                    ("damageState", PyRep::Tuple(vec![PyRep::Real(1.0), PyRep::Real(1.0), PyRep::Real(1.0)])),
                    ("timerProgress", PyRep::None),
                    ("timerIsProgressing", PyRep::Bool(false)),
                    ("timerIsPaused", PyRep::Bool(false)),
                    ("timerEndAt", PyRep::None),
                ]),
            ],
        ],
    },
    OnInvulunOnUndockingUpdated => "OnInvulunOnUndockingUpdated" ("charid") {
        fields: { ship_id: i64, end_time: SimFileTime, duration: PyRep },
        samples: || vec![vec![
            PyRep::Integer(100_000_001),
            PyRep::Integer(133_500_000_000_000_000),
            PyRep::Integer(30),
        ]],
    },
    OnItemsChanged => "OnItemsChanged" ("charid") {
        fields: { items: PyRep, change: PyRep, location: PyRep },
        // `change` may be empty `{}` or keyed by inventory change column IDs (`character_skill_ops.rs:713`).
        samples: || vec![
            vec![
                PyRep::List(vec![sample_dbrow_item()]),
                PyRep::Dict(Vec::new()),
                PyRep::None,
            ],
            vec![
                PyRep::List(vec![sample_dbrow_item()]),
                PyRep::Dict(vec![
                    (PyRep::Integer(3), PyRep::Integer(60_003_760)),
                    (PyRep::Integer(4), PyRep::Integer(4)),
                ]),
                PyRep::None,
            ],
        ],
    },
    OnJumpCloneCacheInvalidated => "OnJumpCloneCacheInvalidated" ("charid") {
        samples: || vec![vec![]],
    },
    OnJumpCloneTransitionCompleted => "OnJumpCloneTransitionCompleted" ("charid") {
        samples: || vec![vec![]],
    },
    OnMailCountersUpdate => "OnMailCountersUpdate" ("charid") {
        optional: { char_id: Option<i64> },
        // `MailForm.OnMailCountersUpdate(*args)`; callers (`fleet_social_mail.rs:732`) pass `char_id`.
        samples: || vec![vec![], vec![PyRep::Integer(90_000_001)]],
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
        // `services/sov_gameplay.rs:236` passes `{"itemID": notify_item_id}`.
        samples: || vec![vec![
            PyRep::String("SovereigntyNullsecRequired".into()),
            PyRep::Dict(vec![(
                PyRep::String("itemID".into()),
                PyRep::Integer(100_000_001),
            )]),
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
    OnNewNotificationReceived => "OnNewNotificationReceived" ("charid") {
        fields: { notification: PyRep },
        // Accepts V24 notification KeyVal or Dict records.
        samples: || vec![
            vec![PyRep::keyval([
                ("notificationID", PyRep::Integer(1)),
                ("typeID", PyRep::Integer(1)),
            ])],
            vec![PyRep::Dict(vec![(
                PyRep::String("notificationID".into()),
                PyRep::Integer(1),
            )])],
        ],
    },
    OnNotificationDeleted => "OnNotificationDeleted" ("charid") {
        fields: { notification_ids: Vec<i64> },
        samples: || vec![vec![PyRep::List(vec![PyRep::Integer(1)])]],
    },
    OnNotificationReceived => "OnNotificationReceived" ("charid") {
        fields: {
            notification_id: i64,
            type_id: i64,
            sender_id: Option<i64>,
            created: WallFileTime,
            data: PyRep,
        },
        // `services/auth_cache.rs:148-157`: `sender_id` and `data` may be `None`, string, dict, or KeyVal.
        samples: || vec![
            vec![
                PyRep::Integer(1),
                PyRep::Integer(1),
                PyRep::Integer(90_000_001),
                PyRep::Integer(1_335_000_000_000_000_00),
                PyRep::String("{}".into()),
            ],
            vec![
                PyRep::Integer(1),
                PyRep::Integer(1),
                PyRep::Integer(90_000_001),
                PyRep::Integer(1_335_000_000_000_000_00),
                PyRep::None,
            ],
            vec![
                PyRep::Integer(1),
                PyRep::Integer(1),
                PyRep::None,
                PyRep::Integer(1_335_000_000_000_000_00),
                PyRep::String("{}".into()),
            ],
            vec![
                PyRep::Integer(1),
                PyRep::Integer(1),
                PyRep::None,
                PyRep::Integer(1_335_000_000_000_000_00),
                PyRep::None,
            ],
            vec![
                PyRep::Integer(1),
                PyRep::Integer(1),
                PyRep::Integer(90_000_001),
                PyRep::Integer(1_335_000_000_000_000_00),
                PyRep::Dict(vec![(PyRep::String("key".into()), PyRep::Integer(1))]),
            ],
        ],
    },
    OnOwnOrdersChanged => "OnOwnOrdersChanged" ("charid") {
        fields: { orders: PyRep, reason: PyRep, is_corp: PyRep },
        samples: || vec![vec![
            PyRep::List(vec![PyRep::keyval([
                ("orderID", PyRep::Integer(1)),
                ("typeID", PyRep::Integer(34)),
                ("bid", PyRep::Bool(false)),
                ("duration", PyRep::Integer(90)),
                ("volRemaining", PyRep::Integer(100)),
                ("volEntered", PyRep::Integer(100)),
                ("minVolume", PyRep::Integer(1)),
                ("price", PyRep::Real(100.0)),
                ("stationID", PyRep::Integer(60_003_760)),
                ("regionID", PyRep::Integer(10_000_002)),
                ("solarSystemID", PyRep::Integer(30_000_142)),
                ("constellationID", PyRep::Integer(20_000_020)),
                ("range", PyRep::Integer(-1)),
                ("issueDate", PyRep::Integer(1_335_000_000_000_000_00)),
                ("isCorp", PyRep::Bool(false)),
                ("orderState", PyRep::Integer(1)),
            ])]),
            PyRep::String("Add".into()),
            PyRep::Bool(false),
        ]],
    },
    OnPVPFilamentsCharacterStatistics => "OnPVPFilamentsCharacterStatistics" ("charid") {
        fields: { info: PyRep },
        // `services/pvp_filament_arena.rs:1118-1124` & `pvp_filament_mgr.rs:553-563`.
        samples: || vec![vec![PyRep::Dict(vec![
            (PyRep::String("eventID".into()), PyRep::Integer(1)),
            (PyRep::String("event_id".into()), PyRep::Integer(1)),
            (PyRep::String("matchTypeID".into()), PyRep::Integer(1)),
            (PyRep::String("scheduleID".into()), PyRep::Integer(1)),
            (
                PyRep::String("statistics".into()),
                PyRep::Dict(vec![
                    (PyRep::String("character_id".into()), PyRep::Integer(90_000_001)),
                    (PyRep::String("rank".into()), PyRep::Integer(1)),
                    (PyRep::String("score".into()), PyRep::Integer(100)),
                    (PyRep::String("wins".into()), PyRep::Integer(1)),
                    (PyRep::String("losses".into()), PyRep::Integer(0)),
                    (PyRep::String("draws".into()), PyRep::Integer(0)),
                    (PyRep::String("kills".into()), PyRep::Integer(1)),
                    (PyRep::String("damageDealt".into()), PyRep::Real(1200.0)),
                ]),
            ),
        ])]],
    },
    OnPVPFilamentsLeaderboard => "OnPVPFilamentsLeaderboard" ("charid") {
        fields: { info: PyRep },
        // `services/pvp_filament_arena.rs:1110-1116` & `pvp_filament_mgr.rs:518-525`.
        samples: || vec![vec![PyRep::Dict(vec![
            (PyRep::String("eventID".into()), PyRep::Integer(1)),
            (PyRep::String("event_id".into()), PyRep::Integer(1)),
            (PyRep::String("matchTypeID".into()), PyRep::Integer(1)),
            (PyRep::String("scheduleID".into()), PyRep::Integer(1)),
            (
                PyRep::String("entries".into()),
                PyRep::List(vec![PyRep::Dict(vec![
                    (PyRep::String("character_id".into()), PyRep::Integer(90_000_001)),
                    (PyRep::String("rank".into()), PyRep::Integer(1)),
                    (PyRep::String("score".into()), PyRep::Integer(100)),
                    (PyRep::String("wins".into()), PyRep::Integer(1)),
                    (PyRep::String("losses".into()), PyRep::Integer(0)),
                    (PyRep::String("draws".into()), PyRep::Integer(0)),
                ])]),
            ),
        ])]],
    },
    OnServerSkillsChanged => "OnServerSkillsChanged" ("charid") {
        fields: { skill_infos: PyRep, event: PyRep, timestamp: WallFileTime },
        // `skillPoints` is float (`PyRep::Real`, `skillsvc.pyj` / `delta_projection.rs:1277` /
        // `jump_clone_names.rs:244`); `event` is `String` or `None` (`SkillsSvc.OnServerSkillsChanged`).
        samples: || vec![
            vec![
                PyRep::Dict(vec![(PyRep::Integer(3402), sample_skill_info_kv(1, 250.0))]),
                PyRep::String("OnSkillTrained".into()),
                PyRep::Integer(133_500_000_000_000_000),
            ],
            vec![
                PyRep::Dict(vec![(PyRep::Integer(3402), sample_skill_info_kv(1, 250.0))]),
                PyRep::None,
                PyRep::Integer(133_500_000_000_000_000),
            ],
        ],
    },
    OnServerSkillsRemoved => "OnServerSkillsRemoved" ("charid") {
        fields: { skill_infos: PyRep, timestamp: WallFileTime },
        // Removed entries carry `KeyVal` with `trainedSkillPoints = -1.0` (`jump_clone_names.rs:287`,
        // `delta_projection.rs:1372`) or `None`.
        samples: || vec![
            vec![
                PyRep::Dict(vec![(PyRep::Integer(3402), PyRep::None)]),
                PyRep::Integer(133_500_000_000_000_000),
            ],
            vec![
                PyRep::Dict(vec![(PyRep::Integer(3402), sample_skill_info_kv(0, -1.0))]),
                PyRep::Integer(133_500_000_000_000_000),
            ],
        ],
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
        fields: { args: ShipJumpCloneInstallationArgs },
        samples: || vec![vec![ShipJumpCloneInstallationArgs {
            offering_char_id: 90_000_001, target_char_id: 90_000_002, ship_id: 100_000_001, offered: true,
        }.into_notify_arg()]],
    },
    OnShipJumpCloneInstallationOffered => "OnShipJumpCloneInstallationOffered" ("charid") {
        fields: { args: ShipJumpCloneInstallationArgs },
        samples: || vec![vec![ShipJumpCloneInstallationArgs {
            offering_char_id: 90_000_001, target_char_id: 90_000_002, ship_id: 100_000_001, offered: true,
        }.into_notify_arg()]],
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
            PyRep::keyval([
                ("solarSystemID", PyRep::Integer(30_000_142)),
                ("interferenceLevel", PyRep::Real(0.5)),
                ("interferencePct", PyRep::Real(50.0)),
                ("interferenceBand", PyRep::Integer(2)),
                ("bandLabel", PyRep::String("Medium".into())),
                ("crabActivationsRemaining", PyRep::Integer(6)),
                ("isAtQuiescentLevel", PyRep::Bool(false)),
                ("quiescentInterferenceLevel", PyRep::Real(0.0)),
                ("maxInterferenceLevel", PyRep::Real(1.0)),
                ("normalisedInterferenceLevel", PyRep::Real(0.5)),
                ("lastDecayTickTimestamp", PyRep::Integer(1_335_000_000_000_000_00)),
                ("decayTickPeriod", PyRep::Integer(6_000_000_000)),
                ("interferenceDecreasePerDecayTick", PyRep::Real(0.01)),
            ]),
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
        fields: { container_id: i64, items: PyRep },
        // `services/wallet_services_ext.rs:924-958` (`trade_item_to_keyval`) emits integer `singleton`.
        samples: || vec![
            vec![
                PyRep::Integer(1),
                PyRep::List(vec![PyRep::keyval([
                    ("itemID", PyRep::Integer(100_000_001)),
                    ("typeID", PyRep::Integer(34)),
                    ("ownerID", PyRep::Integer(90_000_001)),
                    ("locationID", PyRep::Integer(60_003_760)),
                    ("flagID", PyRep::Integer(4)),
                    ("quantity", PyRep::Integer(10)),
                    ("stacksize", PyRep::Integer(10)),
                    ("singleton", PyRep::Integer(0)),
                    ("groupID", PyRep::Integer(18)),
                    ("categoryID", PyRep::Integer(4)),
                    ("customInfo", PyRep::String(String::new())),
                ])]),
            ],
            vec![PyRep::Integer(1), PyRep::List(vec![sample_keyval_item()])],
        ],
    },
    OnTradeInitiate => "OnTradeInitiate" ("charid") {
        fields: { char_id: i64, trade_session: PyRep, trade_items: PyRep },
        samples: || vec![vec![
            PyRep::Integer(90_000_001),
            PyRep::SubStruct(Box::new(PyRep::SubStream(Box::new(PyRep::Tuple(vec![
                PyRep::ascii_bytes("N=1001:1"),
                PyRep::Integer(1),
            ]))))),
            PyRep::List(vec![]),
        ]],
    },
    OnTradeMoneyOffer => "OnTradeMoneyOffer" ("charid") {
        fields: { container_id: i64, money: f64 },
        samples: || vec![vec![PyRep::Integer(1), PyRep::Real(5_000.0)]],
    },
    OnTradeOffer => "OnTradeOffer" ("charid") {
        fields: { container_id: i64, state: bool },
        samples: || vec![vec![PyRep::Integer(1), PyRep::Bool(true)]],
    },
    OnTradeOfferReset => "OnTradeOfferReset" ("charid") {
        fields: { container_id: i64, manifest_error: bool },
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
        // `services/fw_services.rs:268-288,340-359` emits `adjacencyState: String` (`"frontline"` etc.).
        samples: || vec![vec![
            PyRep::Integer(30_000_142),
            PyRep::keyval([
                ("solarSystemID", PyRep::Integer(30_000_142)),
                ("ownerID", PyRep::Integer(500_001)),
                ("ownerFactionID", PyRep::Integer(500_001)),
                ("occupierID", PyRep::Integer(500_001)),
                ("occupierFactionID", PyRep::Integer(500_001)),
                ("adjacencyState", PyRep::String("frontline".into())),
                ("frontlineStatus", PyRep::Integer(2)),
                ("isFrontline", PyRep::Bool(true)),
                ("contestedPercentage", PyRep::Real(10.0)),
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
        // Accepts V24 `(updates_list, wait_for_bubble)` pairs plus pre-`client_compat`
        // `(stamp, (action, args))` tuples (`space_nav_travel.rs:974`, `client_compat.rs:989`).
        samples: || vec![
            vec![PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::List(vec![
                    PyRep::Tuple(vec![
                        PyRep::Integer(1),
                        PyRep::Tuple(vec![
                            PyRep::String("SetMaxSpeed".into()),
                            PyRep::Tuple(vec![PyRep::Integer(100_000_001), PyRep::Real(1.0)]),
                        ]),
                    ]),
                    PyRep::Tuple(vec![
                        PyRep::Integer(1),
                        PyRep::Tuple(vec![
                            PyRep::String("Stop".into()),
                            PyRep::Tuple(vec![PyRep::Integer(100_000_001)]),
                        ]),
                    ]),
                    PyRep::Tuple(vec![
                        PyRep::Integer(1),
                        PyRep::Tuple(vec![
                            PyRep::String("SetBallVelocity".into()),
                            PyRep::Tuple(vec![
                                PyRep::Integer(100_000_001),
                                PyRep::Real(0.0),
                                PyRep::Real(0.0),
                                PyRep::Real(0.0),
                            ]),
                        ]),
                    ]),
                ]),
                PyRep::Bool(false),
            ])])],
            vec![PyRep::List(vec![
                PyRep::Tuple(vec![
                    PyRep::Integer(1),
                    PyRep::Tuple(vec![
                        PyRep::String("SetMaxSpeed".into()),
                        PyRep::Tuple(vec![PyRep::Integer(100_000_001), PyRep::Real(1.0)]),
                    ]),
                ]),
                PyRep::Tuple(vec![
                    PyRep::Integer(1),
                    PyRep::Tuple(vec![
                        PyRep::String("Stop".into()),
                        PyRep::Tuple(vec![PyRep::Integer(100_000_001)]),
                    ]),
                ]),
                PyRep::Tuple(vec![
                    PyRep::Integer(1),
                    PyRep::Tuple(vec![
                        PyRep::String("SetBallVelocity".into()),
                        PyRep::Tuple(vec![
                            PyRep::Integer(100_000_001),
                            PyRep::Real(0.0),
                            PyRep::Real(0.0),
                            PyRep::Real(0.0),
                        ]),
                    ]),
                ]),
            ])],
        ],
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
        fields: { end_time: SimFileTime },
        samples: || vec![vec![PyRep::Integer(133_500_000_000_000_000)]],
    },
    OnAddExpiryDurationForAbyssalPvpContent => "OnAddExpiryDurationForAbyssalPvpContent" ("charid") {
        fields: { end_time: SimFileTime },
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
        // `BountyContainer.OnBountyPlaced(ownerID)`: exact V24 1-arg signature.
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
    OnCapacitorChange => "OnCapacitorChange" ("charid") {
        fields: { ship_id: i64, new_cap: f64, max_cap: f64 },
        samples: || vec![vec![
            PyRep::Integer(100_000_010),
            PyRep::Real(250.0),
            PyRep::Real(500.0),
        ]],
    },
    OnCaptureChanged => "OnCaptureChanged" ("solarsystemid") {
        fields: {
            ball_id: i64,
            capture_id: i64,
            last_incident: SimFileTime,
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
        // `services/cosmetics_ui_ops.rs:535` emits `None` when clearing a ship SKIN.
        samples: || vec![
            vec![PyRep::Integer(100_000_010), PyRep::Integer(123)],
            vec![PyRep::Integer(100_000_010), PyRep::None],
        ],
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
        // `services/notify_emitters_space_dungeons.rs:516` passes `None` when idle.
        samples: || vec![
            vec![
                PyRep::Integer(100_000_020),
                PyRep::Integer(1),
                PyRep::String("attack".into()),
            ],
            vec![
                PyRep::Integer(100_000_020),
                PyRep::Integer(0),
                PyRep::None,
            ],
        ],
    },
    OnDroneControlLost => "OnDroneControlLost" ("charid") {
        fields: { drone_id: i64 },
        samples: || vec![vec![PyRep::Integer(100_000_020)]],
    },
    OnDroneStateChange => "OnDroneStateChange" ("charid") @ballpark {
        fields: {
            drone_id: i64,
            owner_id: i64,
            controller_id: i64,
            activity_state: i64,
            drone_type_id: i64,
            controller_owner_id: i64,
            target_id: Option<i64>,
        },
        // `sp6_lifecycle/drone_return.rs:172` sets `target_id: Some(...)` while engaged.
        samples: || vec![
            vec![
                PyRep::Integer(100_000_020),
                PyRep::Integer(90_000_001),
                PyRep::Integer(100_000_010),
                PyRep::Integer(0),
                PyRep::Integer(2454),
                PyRep::Integer(90_000_001),
                PyRep::None,
            ],
            vec![
                PyRep::Integer(100_000_020),
                PyRep::Integer(90_000_001),
                PyRep::Integer(100_000_010),
                PyRep::Integer(1),
                PyRep::Integer(2454),
                PyRep::Integer(90_000_001),
                PyRep::Integer(100_000_030),
            ],
        ],
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
        // `services/notify_emitters_space_dungeons.rs:365-378`.
        samples: || vec![vec![
            PyRep::Integer(1),
            PyRep::Dict(vec![
                (PyRep::String("currentWave".into()), PyRep::Integer(1)),
                (PyRep::String("totalWaves".into()), PyRep::Integer(3)),
                (PyRep::String("completed".into()), PyRep::Bool(false)),
            ]),
        ]],
    },
    OnEditCalendarEvent => "OnEditCalendarEvent" ("charid") {
        fields: {
            event_id: i64,
            owner_id: i64,
            old_event_date_time: WallFileTime,
            event_date_time: WallFileTime,
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
        // `services/notify_emitters_space_dungeons.rs:365-378`.
        samples: || vec![vec![
            PyRep::Integer(1),
            PyRep::Integer(2),
            PyRep::Tuple(vec![PyRep::Real(0.0), PyRep::Real(0.0), PyRep::Real(0.0)]),
            PyRep::Integer(100),
            PyRep::Dict(vec![
                (PyRep::String("currentWave".into()), PyRep::Integer(1)),
                (PyRep::String("totalWaves".into()), PyRep::Integer(3)),
                (PyRep::String("completed".into()), PyRep::Bool(false)),
            ]),
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
        // `services/character_expert_systems.rs:138-151` (`expert_systems_to_py_dict`).
        samples: || vec![
            vec![
                PyRep::Dict(vec![(
                    PyRep::Integer(57_800),
                    PyRep::keyval([
                        ("typeID", PyRep::Integer(57_800)),
                        ("insertedAt", PyRep::Integer(1_335_000_000_000_000_00)),
                        ("expiresAt", PyRep::Integer(1_335_000_001_000_000_00)),
                    ]),
                )]),
                PyRep::Bool(true),
                PyRep::Integer(57_800),
            ],
            vec![
                PyRep::Dict(Vec::new()),
                PyRep::Bool(false),
                PyRep::Integer(57_800),
            ],
        ],
    },
    OnFighterAbilitySlotActivated => "OnFighterAbilitySlotActivated" ("charid") {
        fields: {
            fighter_id: i64,
            ability_slot_id: i64,
            remaining_charge_count: Option<i64>,
            start_time: SimFileTime,
            duration_ms: f64,
            cooldown: Option<f64>,
        },
        // `services/combat_fighters_abilities.rs:1278` sets `remaining_charge_count` & `cooldown`
        // to `None` when unlimited / zero cooldown (`compareisnull` in client).
        samples: || vec![
            vec![
                PyRep::Integer(100_000_020),
                PyRep::Integer(0),
                PyRep::Integer(5),
                PyRep::Integer(1_335_000_000_000_000_00),
                PyRep::Real(10_000.0),
                PyRep::Real(5.0),
            ],
            vec![
                PyRep::Integer(100_000_020),
                PyRep::Integer(0),
                PyRep::None,
                PyRep::Integer(1_335_000_000_000_000_00),
                PyRep::Real(10_000.0),
                PyRep::Real(5.0),
            ],
            vec![
                PyRep::Integer(100_000_020),
                PyRep::Integer(0),
                PyRep::Integer(5),
                PyRep::Integer(1_335_000_000_000_000_00),
                PyRep::Real(10_000.0),
                PyRep::None,
            ],
            vec![
                PyRep::Integer(100_000_020),
                PyRep::Integer(0),
                PyRep::None,
                PyRep::Integer(1_335_000_000_000_000_00),
                PyRep::Real(10_000.0),
                PyRep::None,
            ],
        ],
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
        // `services/fleet_social_fleet_ops.rs:164,203`: item/type IDs are `None` for non-target broadcasts.
        samples: || vec![
            vec![
                PyRep::String("Target".into()),
                PyRep::Integer(0),
                PyRep::Integer(90_000_001),
                PyRep::Integer(30_000_142),
                PyRep::Integer(100_000_050),
                PyRep::Integer(587),
            ],
            vec![
                PyRep::String("NeedArmor".into()),
                PyRep::Integer(0),
                PyRep::Integer(90_000_001),
                PyRep::Integer(30_000_142),
                PyRep::None,
                PyRep::None,
            ],
            vec![
                PyRep::String("WarpTo".into()),
                PyRep::Integer(0),
                PyRep::Integer(90_000_001),
                PyRep::Integer(30_000_142),
                PyRep::Integer(100_000_050),
                PyRep::None,
            ],
        ],
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
            time: SimFileTime,
        },
        // `services/notify_emitters_space_dungeons.rs:557-578` emits 10-element effect state lists
        // (`[8]` duration ms `Integer`, `[9]` `Integer` or `Real`, `[3]` target `Integer` or `None`).
        samples: || {
            let line = |target: PyRep, dur: PyRep, stop: PyRep| {
                PyRep::Dict(vec![(
                    PyRep::Integer(10),
                    PyRep::List(vec![
                        PyRep::Integer(100_000_020),
                        PyRep::Integer(90_000_001),
                        PyRep::Integer(100_000_010),
                        target,
                        PyRep::None,
                        PyRep::List(Vec::new()),
                        PyRep::Integer(10),
                        PyRep::Integer(1_335_000_000_000_000_00),
                        dur,
                        stop,
                    ]),
                )])
            };
            let t = PyRep::Integer(1_335_000_000_000_000_00);
            vec![
                vec![
                    PyRep::Integer(100_000_020),
                    PyRep::Integer(440),
                    line(PyRep::None, PyRep::Integer(1000), PyRep::Integer(0)),
                    t.clone(),
                ],
                vec![
                    PyRep::Integer(100_000_020),
                    PyRep::Integer(440),
                    line(PyRep::Integer(100_000_030), PyRep::Integer(1000), PyRep::Integer(0)),
                    t.clone(),
                ],
                vec![
                    PyRep::Integer(100_000_020),
                    PyRep::Integer(440),
                    line(PyRep::None, PyRep::Integer(1000), PyRep::Real(1000.0)),
                    t,
                ],
            ]
        },
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
        // `services/fitting_module_cycles_helpers.rs:734,1164,1208` emits `invItem` as `DBRow`
        // (and `None` for `(ship_id, flag_id, type_id)` charge sublocations).
        samples: || {
            let active_fx = PyRep::List(vec![
                PyRep::Integer(100_000_020),
                PyRep::Integer(90_000_001),
                PyRep::Integer(100_000_010),
                PyRep::None,
                PyRep::None,
                PyRep::List(Vec::new()),
                PyRep::Integer(10),
                PyRep::Integer(1_335_000_000_000_000_00),
                PyRep::Real(1000.0),
                PyRep::Integer(0),
            ]);
            vec![
                vec![
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
                ],
                vec![
                    PyRep::Integer(100_000_010),
                    PyRep::keyval([
                        ("itemID", PyRep::Integer(100_000_020)),
                        ("invItem", sample_dbrow_item()),
                        (
                            "attributes",
                            PyRep::Dict(vec![(PyRep::Integer(2), PyRep::Real(1.0))]),
                        ),
                        (
                            "activeEffects",
                            PyRep::Dict(vec![(PyRep::Integer(10), active_fx)]),
                        ),
                        ("time", PyRep::Integer(1_335_000_000_000_000_00)),
                        ("wallclockTime", PyRep::Integer(1_335_000_000_000_000_00)),
                        ("description", PyRep::String(String::new())),
                    ]),
                ],
                vec![
                    PyRep::Integer(100_000_010),
                    PyRep::keyval([
                        (
                            "itemID",
                            PyRep::Tuple(vec![
                                PyRep::Integer(100_000_010),
                                PyRep::Integer(27),
                                PyRep::Integer(185),
                            ]),
                        ),
                        ("invItem", PyRep::None),
                        (
                            "attributes",
                            PyRep::Dict(vec![(PyRep::Integer(2), PyRep::Real(1.0))]),
                        ),
                        ("activeEffects", PyRep::Dict(Vec::new())),
                        ("time", PyRep::Integer(1_335_000_000_000_000_00)),
                        ("wallclockTime", PyRep::Integer(1_335_000_000_000_000_00)),
                        ("description", PyRep::String(String::new())),
                    ]),
                ],
            ]
        },
    },
    OnGodmaShipEffect => "OnGodmaShipEffect" ("charid") {
        fields: { item_id: i64, effect: PyRep, start_or_time: GodmaEffectStartOrTime },
        optional: {
            start: Option<PyRep>,
            active: Option<PyRep>,
            environment: Option<PyRep>,
            start_time: Option<SimFileTime>,
            duration: Option<PyRep>,
            repeat: Option<PyRep>,
            error: Option<PyRep>,
            actual_stop_time: Option<Option<SimFileTime>>,
        },
        samples: sample_godma_ship_effect_variants,
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
            start_time: SimFileTime,
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
        fields: { ship_id: i64, end_time: SimFileTime, duration: i64 },
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
            jump_activation: WallFileTime,
            jump_fatigue: WallFileTime,
            last_updated: WallFileTime,
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
        // Canonical V24 `MissionObjectivesTracker.OnMissionsUpdated` subscripts `missions[]["agentID"]` and `["info"]`
        // (`PyRep::Dict`); legacy `services/agents_standings_arcs.rs:2555-2569` KeyVal tracked in `DEFERRED_6B2_EMITTER_COMPAT_EVENTS`.
        samples: || vec![vec![PyRep::List(vec![PyRep::Dict(vec![
            (PyRep::String("missionID".into()), PyRep::Integer(1)),
            (PyRep::String("agentID".into()), PyRep::Integer(3_018_600)),
            (PyRep::String("solarSystemID".into()), PyRep::Integer(30_000_142)),
            (PyRep::String("state".into()), PyRep::String("Accepted".into())),
            (PyRep::String("currentRoomID".into()), PyRep::Integer(1)),
            (PyRep::String("npcsRemaining".into()), PyRep::Integer(2)),
            (PyRep::String("objectivesCompleted".into()), PyRep::Bool(false)),
            (
                PyRep::String("info".into()),
                PyRep::String("Mission 1 for agent 3018600".into()),
            ),
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
        // `SkillQueueService.OnSkillQueuePausedServer(*args)` (`arity_min = 0`): emitted with 0 args
        // (`delta_projection.rs:593`, `binding.rs:1632`) or `Some(bool)` (`character_skills_helpers.rs:760`).
        samples: || vec![vec![], vec![PyRep::Bool(false)]],
    },
    OnSkinLicenseActivated => "OnSkinLicenseActivated" ("charid") {
        fields: { skin_id: i64, licensee_id: i64 },
        samples: || vec![vec![PyRep::Integer(123), PyRep::Integer(90_000_001)]],
    },
    OnSkinLicenseRemoved => "OnSkinLicenseRemoved" ("charid") {
        fields: { skin_id: i64, item_id: Option<i64>, type_id: Option<i64> },
        // `services/cosmetics_ui_ops.rs:226` emits `None` item/type when no ship matched.
        samples: || vec![
            vec![
                PyRep::Integer(123),
                PyRep::Integer(100_000_020),
                PyRep::Integer(34_567),
            ],
            vec![PyRep::Integer(123), PyRep::None, PyRep::None],
        ],
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
            deadline: WallFileTime,
            respond_to_node_id: i64,
            tourney_id: i64,
        },
        samples: || vec![vec![
            PyRep::Integer(1),
            PyRep::Integer(1),
            PyRep::List(vec![PyRep::Integer(587)]),
            PyRep::Integer(1_335_000_000_000_000_00),
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
        // `services/combat_weapons_spool.rs:1298`: `{master_id: [slave_ids]}`.
        samples: || vec![vec![
            PyRep::Integer(100_000_010),
            PyRep::Dict(vec![(
                PyRep::Integer(100_000_020),
                PyRep::List(vec![PyRep::Integer(100_000_021)]),
            )]),
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
            PyRep::keyval([
                ("currentEnergy", PyRep::Integer(100)),
                ("maxEnergy", PyRep::Integer(100)),
                ("lastUpdateTime", PyRep::Integer(1_335_000_000_000_000_00)),
                ("rechargeRate", PyRep::Real(100.0 / 1_320.0)),
                ("energyLevel", PyRep::Integer(100)),
                ("lastRechargeTickTimestamp", PyRep::Integer(1_335_000_000_000_000_00)),
                ("energyIncreasePerRechargeTick", PyRep::Real(6000.0 / 1_320.0)),
                ("rechargeTickPeriod", PyRep::Integer(3_600)),
                ("minEnergyLevel", PyRep::Integer(0)),
                ("quiescentEnergyLevel", PyRep::Integer(100)),
                ("isAtQuiescentLevel", PyRep::Bool(true)),
                ("energyRange", PyRep::Integer(100)),
                ("normalisedEnergyLevel", PyRep::Real(1.0)),
            ]),
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
        // `HackingSvc.OnHackingTurnComplete` subscripts `event["eventID"]`
        // (`services/pve_hacking_sites.rs:781-795` start event, `:1070-1091` turn event).
        samples: || vec![
            vec![PyRep::List(vec![PyRep::Dict(vec![
                (PyRep::String("eventID".into()), PyRep::Integer(1)),
                (
                    PyRep::String("eventData".into()),
                    PyRep::keyval([
                        ("gameID", PyRep::Integer(1)),
                        ("containerID", PyRep::Integer(100_000_050)),
                        ("tileIndex", PyRep::Integer(0)),
                        ("gridTopology", PyRep::String("Hexagonal".into())),
                        ("status", PyRep::String("InProgress".into())),
                        ("virusCoherence", PyRep::Integer(60)),
                        ("virusStrength", PyRep::Integer(20)),
                        ("distanceIndicator", PyRep::Integer(1)),
                    ]),
                ),
            ])])],
            vec![PyRep::List(vec![PyRep::Dict(vec![
                (PyRep::String("eventID".into()), PyRep::Integer(1)),
                (
                    PyRep::String("eventData".into()),
                    PyRep::keyval([
                        ("containerID", PyRep::Integer(100_000_050)),
                        ("tileIndex", PyRep::Integer(0)),
                        ("hexQ", PyRep::Integer(0)),
                        ("hexR", PyRep::Integer(0)),
                        ("nodeKind", PyRep::String("Firewall".into())),
                        ("status", PyRep::String("InProgress".into())),
                        ("virusCoherence", PyRep::Integer(60)),
                        ("virusStrength", PyRep::Integer(20)),
                        ("nodeCoherence", PyRep::Integer(40)),
                        ("distanceIndicator", PyRep::Integer(1)),
                        ("containerExploded", PyRep::Bool(false)),
                        ("containerDestroyed", PyRep::Bool(false)),
                        ("explosionDamage", PyRep::Real(0.0)),
                        ("damagedShipIDs", PyRep::List(vec![PyRep::Integer(100_000_010)])),
                        ("destroyedLootCount", PyRep::Integer(0)),
                    ]),
                ),
            ])])],
        ],
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
        // `combat_wrecks_killmails.rs:716`, `combat_npc_police.rs:515`, `character_gm_helpers.rs:146`,
        // `fitting_module_cycles_pending_actions.rs:850`, and `system_actor/cycles.rs:2933`
        // emit 2-arg `[msgID, dict]` when `kw: None`.
        samples: || {
            let sample_dict = PyRep::Dict(vec![
                (PyRep::String("notify".into()), PyRep::String("Notice".into())),
                (PyRep::String("typeID".into()), PyRep::Integer(34)),
                (PyRep::String("quantity".into()), PyRep::Integer(10)),
                (PyRep::String("fineISK".into()), PyRep::Real(1000.0)),
                (PyRep::String("factionID".into()), PyRep::Integer(500_001)),
                (
                    PyRep::String("type".into()),
                    PyRep::Tuple(vec![PyRep::Integer(4), PyRep::Integer(561)]),
                ),
                (
                    PyRep::String("what".into()),
                    PyRep::Tuple(vec![PyRep::Integer(4), PyRep::Integer(587)]),
                ),
                (PyRep::String("what".into()), PyRep::String("ship".into())),
                (PyRep::String("when".into()), PyRep::Integer(120)),
                (PyRep::String("charID".into()), PyRep::Integer(90_000_001)),
                (PyRep::String("shipTypeID".into()), PyRep::Integer(587)),
                (PyRep::String("timeRemaining".into()), PyRep::Integer(1_200_000_000)),
                (PyRep::String("timeLeft".into()), PyRep::Integer(1_200_000_000)),
            ]);
            vec![
                vec![PyRep::String("ParamCustomsNotification".into()), sample_dict.clone()],
                vec![
                    PyRep::String("ParamCustomsNotification".into()),
                    sample_dict,
                    PyRep::None,
                ],
            ]
        },
    },
    OnSafeLogoffAborted => "OnSafeLogoffAborted" ("charid") {
        fields: { reason_code: String },
        samples: || vec![vec![PyRep::String("UI/Inflight/SafeLogoff/Aborted".into())]],
    },
    OnSafeLogoffActivated => "OnSafeLogoffActivated" ("charid") {
        samples: || vec![vec![]],
    },
    OnSafeLogoffFailed => "OnSafeLogoffFailed" ("charid") {
        fields: { failed_conditions: Vec<String> },
        samples: || vec![vec![PyRep::List(vec![PyRep::String(
            "SAFE_LOGOFF_DOCKED".into(),
        )])]],
    },
    OnSafeLogoffTimerStarted => "OnSafeLogoffTimerStarted" ("charid") {
        fields: { safe_logoff_time: SimFileTime },
        samples: || vec![vec![PyRep::Integer(1_335_000_000_000_000_00)]],
    },
    ProcessGodmaPrimeLocation => "ProcessGodmaPrimeLocation" ("charid") {
        fields: { location_id: i64, data: PyRep },
        // `services/fitting_module_cycles_helpers.rs:1080-1300` emits `invItem` as `DBRow`
        // (and `None` for `(ship_id, flag_id, type_id)` charge sublocations).
        samples: || {
            let inv_row = eve_proto::DBRowDescriptor::inv_item_header().build_dbrow(vec![
                PyRep::Integer(100_000_010),
                PyRep::Integer(587),
                PyRep::Integer(90_000_001),
                PyRep::Integer(60_003_760),
                PyRep::Integer(4),
                PyRep::Integer(-1),
                PyRep::Integer(25),
                PyRep::Integer(6),
                PyRep::String(String::new()),
            ]);
            let active_fx_entry = PyRep::List(vec![
                PyRep::Integer(100_000_020),
                PyRep::Integer(90_000_001),
                PyRep::Integer(100_000_010),
                PyRep::None,
                PyRep::None,
                PyRep::List(Vec::new()),
                PyRep::Integer(10),
                PyRep::Integer(1_335_000_000_000_000_00),
                PyRep::Real(1000.0),
                PyRep::Integer(0),
            ]);
            vec![
                vec![
                    PyRep::Integer(100_000_010),
                    PyRep::Dict(vec![(
                        PyRep::Integer(100_000_010),
                        PyRep::keyval([
                            ("itemID", PyRep::Integer(100_000_010)),
                            ("invItem", sample_keyval_item()),
                            (
                                "attributes",
                                PyRep::Dict(vec![(PyRep::Integer(2), PyRep::Real(1.0))]),
                            ),
                        ]),
                    )]),
                ],
                vec![
                    PyRep::Integer(100_000_010),
                    PyRep::Dict(vec![
                        (
                            PyRep::Integer(100_000_010),
                            PyRep::keyval([
                                ("itemID", PyRep::Integer(100_000_010)),
                                ("invItem", inv_row),
                                (
                                    "attributes",
                                    PyRep::Dict(vec![(PyRep::Integer(2), PyRep::Real(1.0))]),
                                ),
                                (
                                    "activeEffects",
                                    PyRep::Dict(vec![(PyRep::Integer(10), active_fx_entry)]),
                                ),
                                ("time", PyRep::Integer(1_335_000_000_000_000_00)),
                                ("wallclockTime", PyRep::Integer(1_335_000_000_000_000_00)),
                                ("description", PyRep::String(String::new())),
                            ]),
                        ),
                        (
                            PyRep::Tuple(vec![
                                PyRep::Integer(100_000_010),
                                PyRep::Integer(27),
                                PyRep::Integer(185),
                            ]),
                            PyRep::keyval([
                                (
                                    "itemID",
                                    PyRep::Tuple(vec![
                                        PyRep::Integer(100_000_010),
                                        PyRep::Integer(27),
                                        PyRep::Integer(185),
                                    ]),
                                ),
                                ("invItem", PyRep::None),
                                (
                                    "attributes",
                                    PyRep::Dict(vec![(PyRep::Integer(2), PyRep::Real(1.0))]),
                                ),
                                ("activeEffects", PyRep::Dict(Vec::new())),
                                ("time", PyRep::Integer(1_335_000_000_000_000_00)),
                                ("wallclockTime", PyRep::Integer(1_335_000_000_000_000_00)),
                                ("description", PyRep::String(String::new())),
                            ]),
                        ),
                    ]),
                ],
            ]
        },
    },
    // === Fitting & Dogma Batch Events (#932) ===
    OnAsteroidRevealed => "OnAsteroidRevealed" ("character") {
        fields: { asteroid_id: i64 },
        samples: || vec![vec![PyRep::Integer(100_000_900)]],
    },
    OnGodmaFlushLocation => "OnGodmaFlushLocation" ("charid") {
        fields: { location_id: i64 },
        samples: || vec![vec![PyRep::Integer(100_000_010)]],
    },
    OnHeatAdded => "OnHeatAdded" ("charid") {
        fields: { heat_id: i64, module_id: i64 },
        samples: || vec![vec![PyRep::Integer(1175), PyRep::Integer(100_000_020)]],
    },
    OnHeatRemoved => "OnHeatRemoved" ("charid") {
        fields: { heat_id: i64, module_id: i64 },
        samples: || vec![vec![PyRep::Integer(1175), PyRep::Integer(100_000_020)]],
    },
    OnMultiModuleRepairStarted => "OnMultiModuleRepairStarted" ("charid") {
        fields: { module_ids: Vec<PyRep> },
        samples: || vec![vec![PyRep::List(vec![PyRep::Integer(100_000_020)])]],
    },
    OnMultipleItemChange => "OnMultipleItemChange" ("charid") {
        fields: { items: Vec<PyRep>, change: PyRep },
        // `delta_item_corp.rs:450`, `character_skill_ops.rs`, and `fitting_saved_*` emit `DBRow`
        // or `KeyVal` item entries and `ix*` integer column keys (`2, 3, 4, 9, 10`).
        samples: || {
            let change_all = || {
                PyRep::Dict(vec![
                    (PyRep::Integer(2), PyRep::Integer(90_000_001)),
                    (PyRep::Integer(3), PyRep::Integer(60_003_760)),
                    (PyRep::Integer(4), PyRep::Integer(4)),
                    (PyRep::Integer(9), PyRep::Integer(1)),
                    (PyRep::Integer(10), PyRep::Integer(0)),
                ])
            };
            vec![
                vec![PyRep::List(vec![sample_dbrow_item()]), change_all()],
                vec![PyRep::List(vec![sample_keyval_item()]), change_all()],
            ]
        },
    },
    OnNewMoonProbe => "OnNewMoonProbe" ("charid") {
        fields: { probe: PyRep },
        // `services/fitting_module_cycles.rs:874`: `moonID` is `None` when no moon is targeted.
        samples: || vec![
            vec![PyRep::keyval([
                ("probeID", PyRep::Integer(1)),
                ("typeID", PyRep::Integer(45_511)),
                ("moonID", PyRep::Integer(40_000_001)),
                ("expiry", PyRep::Integer(1_700_000_060)),
            ])],
            vec![PyRep::keyval([
                ("probeID", PyRep::Integer(1)),
                ("typeID", PyRep::Integer(45_511)),
                ("moonID", PyRep::None),
                ("expiry", PyRep::Integer(1_700_000_060)),
            ])],
        ],
    },
    OnRepairDone => "OnRepairDone" ("charid") {
        fields: { item_ids: Vec<i64> },
        samples: || vec![vec![PyRep::List(vec![PyRep::Integer(100_000_020)])]],
    },
    OnShipInsured => "OnShipInsured" ("charid") {
        samples: || vec![vec![]],
    },
    OnSpecialFX => "OnSpecialFX" ("charid") @ballpark {
        fields: {
            ship_id: i64,
            module_id: Option<i64>,
            module_type_id: Option<i64>,
            target_id: Option<i64>,
            other_type_id: Option<i64>,
            guid: String,
            is_offensive: bool,
            start: i64,
            active: i64,
            duration: f64,
            repeat: i64,
            start_time: SimFileTime,
            time_from_start: f64,
            graphic_info: Option<PyRep>,
        },
        samples: || {
            let fx = |module_id: PyRep,
                      module_type_id: PyRep,
                      target_id: PyRep,
                      other_type_id: PyRep,
                      graphic_info: PyRep| {
                vec![
                    PyRep::Integer(100_000_010),
                    module_id,
                    module_type_id,
                    target_id,
                    other_type_id,
                    PyRep::String("effects.Laser".into()),
                    PyRep::Bool(false),
                    PyRep::Integer(0),
                    PyRep::Integer(0),
                    PyRep::Real(0.0),
                    PyRep::Integer(0),
                    PyRep::Integer(1_335_000_000_000_000_00),
                    PyRep::Real(0.0),
                    graphic_info,
                ]
            };
            // `outbound_scene_scope.rs`, `notify_registry_emit.rs`, and `space_warp_jump.rs` vary all
            // 4 nullable `(module_id, module_type_id, target_id, other_type_id)` slots + `graphic_info`.
            let opt_vals = |some_rep: PyRep| [PyRep::None, some_rep];
            let mut out = Vec::with_capacity(18);
            for mod_id in opt_vals(PyRep::Integer(100_000_020)) {
                for mod_tid in opt_vals(PyRep::Integer(483)) {
                    for target_id in opt_vals(PyRep::Integer(100_000_030)) {
                        for other_tid in opt_vals(PyRep::Integer(587)) {
                            out.push(fx(mod_id.clone(), mod_tid.clone(), target_id.clone(), other_tid, PyRep::None));
                        }
                    }
                }
            }
            out.push(fx(
                PyRep::None,
                PyRep::None,
                PyRep::Integer(100_000_030),
                PyRep::None,
                PyRep::List(vec![PyRep::Integer(30_000_144)]),
            ));
            out.push(fx(
                PyRep::Integer(100_000_020),
                PyRep::Integer(483),
                PyRep::Integer(100_000_030),
                PyRep::None,
                PyRep::List(vec![PyRep::Integer(30_000_144)]),
            ));
            out
        },
    },
    OnStanceActive => "OnStanceActive" ("charid") {
        fields: { ship_id: i64, stance_id: i64 },
        samples: || vec![vec![PyRep::Integer(100_000_010), PyRep::Integer(1)]],
    },
    OnSystemWideEffectAdd => "OnSystemWideEffectAdd" ("charid") {
        fields: { source_item_id: i64, source_type_id: i64 },
        samples: || vec![vec![PyRep::Integer(30_000_142), PyRep::Integer(30_574)]],
    },
    OnSystemWideEffectRemove => "OnSystemWideEffectRemove" ("charid") {
        fields: { source_item_id: i64 },
        samples: || vec![vec![PyRep::Integer(30_000_142)]],
    },
    OnTargetClear => "OnTargetClear" ("charid") {
        optional: { targets_to_skip: Option<Vec<i64>> },
        samples: || vec![
            vec![],
            vec![PyRep::List(vec![PyRep::Integer(100_000_010)])],
        ],
    },
    // === RFC-3 C-8 Typed Notification Batch ===
    OnMachoObjectDisconnect => "OnMachoObjectDisconnect" ("clientID") {
        fields: { object_id: PyRep, client_id: i64, ref_id: i64 },
        samples: || vec![vec![
            PyRep::ascii_bytes("N=1000000000001:1".to_string()),
            PyRep::Integer(1_000_000_000_001),
            PyRep::Integer(1),
        ]],
    },
    OnMultiEvent => "OnMultiEvent" ("charid") {
        fields: { events: Vec<PyRep> },
        samples: || vec![vec![PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Tuple(vec![
                PyRep::String("OnTarget".into()),
                PyRep::String("add".into()),
                PyRep::Integer(100_000_010),
                PyRep::String(String::new()),
            ]),
            PyRep::Integer(0),
        ])])]],
    },
    OnNonDiminishingInjectionsUsed => "OnNonDiminishingInjectionsUsed" ("charid") {
        fields: { amount: i64 },
        samples: || vec![vec![PyRep::Integer(1)]],
    },
    OnReprocessingMaterialsChanged => "OnReprocessingMaterialsChanged" ("charid") {
        samples: || vec![vec![]],
    },
    OnServerBoostersChanged => "OnServerBoostersChanged" ("charid") {
        samples: || vec![vec![]],
    },
    OnServerImplantsChanged => "OnServerImplantsChanged" ("charid") {
        samples: || vec![vec![]],
    },
    OnShipcasterTargetStateChanged => "OnShipcasterTargetStateChanged" ("solarsystemid") {
        fields: { item_id: i64 },
        samples: || vec![vec![PyRep::Integer(1_000_000_000_001)]],
    },
    OnShipCosmeticChanged => "OnShipCosmeticChanged" ("solarsystemid") {
        fields: { ship_id: i64, slot_index: i64, cosmetic: PyRep },
        samples: || vec![
            vec![
                PyRep::Integer(100_000_010),
                PyRep::Integer(0),
                PyRep::Integer(2),
            ],
            vec![
                PyRep::Integer(100_000_010),
                PyRep::Integer(0),
                PyRep::None,
            ],
        ],
    },
    OnShipCosmeticsChanged => "OnShipCosmeticsChanged" ("solarsystemid") {
        fields: { ship_id: i64, cosmetics: PyRep },
        samples: || vec![vec![
            PyRep::Integer(100_000_010),
            PyRep::Dict(vec![(PyRep::Integer(0), PyRep::Integer(2))]),
        ]],
    },
    OnSkyhookFirstActivation => "OnSkyhookFirstActivation" ("solarsystemid") {
        fields: { skyhook_id: i64, planet_id: i64 },
        samples: || vec![vec![
            PyRep::Integer(1_000_000_000_001),
            PyRep::Integer(40_000_001),
        ]],
    },
    OnSkyhookSiloDamageUpdated => "OnSkyhookSiloDamageUpdated" ("solarsystemid") {
        fields: { shield_percentage: f64 },
        samples: || vec![vec![PyRep::Real(0.5)]],
    },
    OnSurveyRewardsAvailable => "OnSurveyRewardsAvailable" ("charid") {
        fields: { survey_id: i64 },
        samples: || vec![vec![PyRep::Integer(2801)]],
    },
    OnWreckLootAll => "OnWreckLootAll" ("charid") {
        fields: { inv_id: i64, items: Vec<i64> },
        samples: || vec![vec![
            PyRep::Integer(100_000_900),
            PyRep::List(vec![PyRep::Integer(100_000_901)]),
        ]],
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
    samples: || vec![
        vec![PyRep::Integer(30_000_142), PyRep::None],
        vec![PyRep::Integer(30_000_142), PyRep::Integer(1)],
    ],
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
define_notification_item!(OnDungeonTriggerAmbientSound3Str => "OnDungeonTriggerAmbientSound" ("charid") {
    fields: { dungeon_id: i64, sound_id: String, entity_item_id: i64 },
    samples: || vec![vec![
        PyRep::Integer(42),
        PyRep::String("amb".into()),
        PyRep::Integer(100_000_010),
    ]],
});
define_notification_item!(OnDungeonTriggerMessageStr => "OnDungeonTriggerMessage" ("charid") {
    fields: { message_type: String, message_id: i64 },
    samples: || vec![vec![PyRep::String("warn".into()), PyRep::Integer(5001)]],
});
define_notification_item!(OnMatchMakingFailureCodes => "OnMatchMakingFailure" ("charid") {
    fields: { errors: Vec<i64> },
    samples: || vec![vec![PyRep::List(vec![PyRep::Integer(1)])]],
});
define_notification_item!(OnNewCalendarEventOptAuto => "OnNewCalendarEvent" ("charid") {
    fields: {
        event_id: i64,
        owner_id: i64,
        event_date_time: WallFileTime,
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
define_notification_item!(DoBallsAddedShipRef => "DoBallsAdded" ("solarsystemid") {
    fields: { item_id: i64, type_id: i64 },
    samples: || vec![vec![PyRep::Integer(100_000_010), PyRep::Integer(587)]],
});
define_notification_item!(OnAsteroidRevealedDetailed => "OnAsteroidRevealed" ("character") {
    fields: { asteroid_id: i64, type_id: i64, quantity: i64 },
    samples: || vec![vec![
        PyRep::Integer(100_000_900),
        PyRep::Integer(1230),
        PyRep::Integer(5000),
    ]],
});
define_notification_item!(PlanetExtractorFinished => "PlanetExtractorFinished" ("charid") {
    fields: { planet_id: i64, pin_id: i64 },
    samples: || vec![vec![PyRep::Integer(40_000_001), PyRep::Integer(701)]],
});
define_notification_item!(PlanetStorageFull => "PlanetStorageFull" ("charid") {
    fields: { planet_id: i64, pin_id: i64 },
    samples: || vec![vec![PyRep::Integer(40_000_001), PyRep::Integer(501)]],
});
define_notification_item!(FailLockTarget => "FailLockTarget" ("service:target") {
    fields: { target_id: i64 },
    samples: || vec![vec![PyRep::Integer(100_000_010)]],
});
