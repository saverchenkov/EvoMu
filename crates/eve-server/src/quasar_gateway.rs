//! `quasar_gateway` — V24.01 Quasar Public & Internal Gateway Transport, Tenancy Auth,
//! Bounded Idempotency Cache, and Scoped Notice Fan-Out Bus (`Q-2`).
//!
//! Implements the server side of:
//! - `eve_public/public.proto` (`eve_public.Request`, `eve_public.Response`, `eve_public.Notice`,
//!   `eve_public.Event`, `eve_public.AuthoritativeContext`, `eve_public.ActiveIdentity`);
//! - `eve_public/gateway.proto` (`Requests.Send`, `Notices.Consume`, `Events.Publish`);
//! - `eve_public/internal_gateway.proto` (`Requests.Handle`, `Notices.Publish`, `Events.Consume`);
//! - `eve/eve.proto` & `eve/gateway.proto` (`eve.Request`, `eve.Response`, `eve.Event`);
//! - `publicGateway/notice/notice_routing_cache.pyj` (`EXPIRY_DELAY_SECS = 10`, `MAX_SIZE_BYTES = 2_000_000`);
//! - `serviceGatewayMgr` QA hijack mode (`qa_enable_hijack_mode`, `qa_disable_hijack_mode`,
//!   `qa_is_hijack_mode_enabled` consumed by `eve/devtools/script/quasarHijackWnd.pyj`).

use eve_db::WorldRepository;
use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, AuthoritativeContext, DynamicQuasarMessage,
    DynamicValue, EveInternalRequest, EveInternalResponse, EvePublicCharacterId, EvePublicEvent,
    EvePublicNotice, EvePublicRequest, EvePublicResponse, GoogleAny, GoogleEmpty, GoogleTimestamp,
    InternalGatewayRequest, InternalGatewayResponse, NoticeTargetGroup, QuasarDescriptorRegistry,
    QuasarError, QuasarMessage, WireMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use tokio::sync::mpsc;

pub const STATUS_OK: u32 = 200;
pub const STATUS_BAD_REQUEST: u32 = 400;
pub const STATUS_UNAUTHORIZED: u32 = 401;
pub const STATUS_FORBIDDEN: u32 = 403;
pub const STATUS_NOT_FOUND: u32 = 404;
pub const STATUS_METHOD_NOT_ALLOWED: u32 = 405;
pub const STATUS_CONFLICT: u32 = 409;
pub const STATUS_PRECONDITION_FAILED: u32 = 412;
pub const STATUS_UNSUPPORTED_MEDIA_TYPE: u32 = 415;
pub const STATUS_INTERNAL_ERROR: u32 = 500;
pub const STATUS_NOT_IMPLEMENTED: u32 = 501;
pub const STATUS_GATEWAY_TIMEOUT: u32 = 504;

/// Matches `EXPIRY_DELAY_SECS = 10` in `publicGateway/notice/notice_routing_cache.pyj`.
pub const NOTICE_CACHE_EXPIRY_DELAY_SECS: u64 = 10;
/// Matches `MAX_SIZE_BYTES = 2000000` in `publicGateway/notice/notice_routing_cache.pyj`.
pub const NOTICE_CACHE_MAX_SIZE_BYTES: usize = 2_000_000;

/// Maximum buffered telemetry/domain events retained in memory (`R-Q2-P1-4`).
pub const MAX_PUBLISHED_EVENTS: usize = 1_024;
/// Maximum idempotency cache entries retained concurrently (`R-Q2-P1-4`).
pub const MAX_IDEMPOTENCY_ENTRIES: usize = 4_096;
/// TTL in seconds for cached idempotent responses (`R-Q2-P1-4`).
pub const IDEMPOTENCY_TTL_SECS: u64 = 600;
/// Maximum concurrent notice subscribers (`R-Q2-P1-4`).
pub const MAX_GATEWAY_SUBSCRIBERS: usize = 4_096;
/// Maximum queued notices per subscriber inbox and bounded channel capacity (`R-Q2-P1-4`).
pub const MAX_SUBSCRIBER_INBOX_LEN: usize = 256;

/// Target audience scope for publishing a server-originated Quasar `eve_public.Notice`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoticeScope {
    ApplicationInstance(Vec<u8>),
    SolarSystem(u32),
    User(i32),
    Character(u32),
    Corporation(u32),
    Alliance(i32),
    BubbleInstance(Vec<u8>),
    Fleet(i64),
    Broadcast,
}

impl NoticeScope {
    /// Returns the `NoticeTargetGroup` for the base notice before per-subscriber fan-out.
    /// `Fleet` and `Broadcast` have no single group variant in `eve_public.Notice.target_group`
    /// (`eve_public/public.proto`), so they are `None` on the base notice and stamped per recipient
    /// in `target_group_for_subscriber`.
    pub fn base_target_group(&self) -> Option<NoticeTargetGroup> {
        match self {
            Self::ApplicationInstance(uuid) => {
                Some(NoticeTargetGroup::ApplicationInstanceUuid(uuid.clone()))
            }
            Self::SolarSystem(sys) => Some(NoticeTargetGroup::SolarSystem(*sys)),
            Self::User(u) => Some(NoticeTargetGroup::User(*u)),
            Self::Character(c) => Some(NoticeTargetGroup::Character(*c)),
            Self::Corporation(corp) => Some(NoticeTargetGroup::Corporation(*corp)),
            Self::Alliance(all) => Some(NoticeTargetGroup::Alliance(*all)),
            Self::BubbleInstance(uuid) => Some(NoticeTargetGroup::BubbleInstanceUuid(uuid.clone())),
            Self::Fleet(_) | Self::Broadcast => None,
        }
    }

    /// Stamps the recipient-specific `NoticeTargetGroup` for a matched subscriber.
    pub fn target_group_for_subscriber(&self, sub: &GatewaySubscriber) -> NoticeTargetGroup {
        match self.base_target_group() {
            Some(tg) => tg,
            None => match sub.character_id {
                Some(cid) => NoticeTargetGroup::Character(cid),
                None => {
                    NoticeTargetGroup::ApplicationInstanceUuid(sub.application_instance_uuid.clone())
                }
            },
        }
    }

    pub fn matches_subscriber(&self, sub: &GatewaySubscriber) -> bool {
        match self {
            Self::ApplicationInstance(uuid) => sub.application_instance_uuid == *uuid,
            Self::SolarSystem(sys) => sub.solar_system_id == Some(*sys),
            Self::User(uid) => sub.user_id == Some(*uid),
            Self::Character(cid) => sub.character_id == Some(*cid),
            Self::Corporation(corpid) => sub.corporation_id == Some(*corpid),
            Self::Alliance(allid) => sub.alliance_id == Some(*allid),
            Self::BubbleInstance(b) => sub.bubble_instance_uuid.as_ref() == Some(b),
            Self::Fleet(fid) => sub.fleet_id == Some(*fid),
            Self::Broadcast => true,
        }
    }
}

#[derive(Debug, Clone)]
pub struct GatewaySubscriber {
    pub session_id: u64,
    pub subscribed_at_unix: u64,
    pub application_instance_uuid: Vec<u8>,
    pub user_id: Option<i32>,
    pub character_id: Option<u32>,
    pub corporation_id: Option<u32>,
    pub alliance_id: Option<i32>,
    pub solar_system_id: Option<u32>,
    pub fleet_id: Option<i64>,
    pub bubble_instance_uuid: Option<Vec<u8>>,
    pub subscribed_notice_types: Vec<String>,
    pub sender: mpsc::Sender<EvePublicNotice>,
    pub inbox: VecDeque<EvePublicNotice>,
}

impl GatewaySubscriber {
    pub fn update_from_session(&mut self, session: &SessionState) {
        self.user_id = session.user_id.map(|u| u as i32);
        self.character_id = session.character_id.map(|c| c as u32);
        self.corporation_id = session.corporation_id.map(|c| c as u32);
        self.alliance_id = session.alliance_id.map(|a| a as i32);
        self.solar_system_id = session.solar_system_id;
        self.fleet_id = session.fleet_id;
    }

    pub fn accepts_payload_type(&self, payload_type_name: &str) -> bool {
        if self.subscribed_notice_types.is_empty() {
            return true;
        }
        let norm = QuasarDescriptorRegistry::normalize_type_name(payload_type_name);
        self.subscribed_notice_types
            .iter()
            .any(|t| QuasarDescriptorRegistry::normalize_type_name(t) == norm)
    }

    /// Enqueues a notice into the bounded subscriber channel and bounded polling inbox (`MAX_SUBSCRIBER_INBOX_LEN`).
    pub fn push_notice(&mut self, notice: EvePublicNotice) {
        let _ = self.sender.try_send(notice.clone());
        if self.inbox.len() >= MAX_SUBSCRIBER_INBOX_LEN {
            self.inbox.pop_front();
        }
        self.inbox.push_back(notice);
    }
}

/// Entry in `NoticeRoutingCache` mirroring `publicGateway/notice/notice_routing_cache.pyj`.
#[derive(Debug, Clone)]
pub struct CachedNoticeEntry {
    pub notice: EvePublicNotice,
    pub scope: NoticeScope,
    pub byte_size: usize,
    pub expiry_unix: u64,
}

/// Mirrors `NoticeRoutingCache` in `publicGateway/notice/notice_routing_cache.pyj`:
/// caches notices whose target group (`character`, `corporation`, `alliance`, `solar_system`, `user`)
/// does not yet match a transitioning session until `session_changed` fires or `EXPIRY_DELAY_SECS` elapses.
#[derive(Debug, Default)]
pub struct NoticeRoutingCache {
    pub current_bytes: usize,
    pub max_bytes: usize,
    pub entries: VecDeque<CachedNoticeEntry>,
}

impl NoticeRoutingCache {
    pub fn new(max_bytes: usize) -> Self {
        Self {
            current_bytes: 0,
            max_bytes,
            entries: VecDeque::new(),
        }
    }

    pub fn remove_expired(&mut self, now_unix: u64) {
        while let Some(front) = self.entries.front() {
            if front.expiry_unix <= now_unix {
                let removed = self.entries.pop_front().unwrap();
                self.current_bytes = self.current_bytes.saturating_sub(removed.byte_size);
            } else {
                break;
            }
        }
    }

    pub fn add_notice(
        &mut self,
        notice: EvePublicNotice,
        scope: NoticeScope,
        now_unix: u64,
    ) -> Result<(), BridgeRpcError> {
        self.remove_expired(now_unix);
        let byte_size = notice.encode_proto().len().max(1);
        if byte_size > self.max_bytes {
            return Err(BridgeRpcError {
                code: "CacheOOMException".into(),
                message: format!(
                    "Notice size {byte_size} exceeds NoticeRoutingCache max_bytes {}",
                    self.max_bytes
                ),
            });
        }
        while self.current_bytes + byte_size > self.max_bytes {
            if let Some(evicted) = self.entries.pop_front() {
                self.current_bytes = self.current_bytes.saturating_sub(evicted.byte_size);
            } else {
                break;
            }
        }
        self.current_bytes += byte_size;
        self.entries.push_back(CachedNoticeEntry {
            notice,
            scope,
            byte_size,
            expiry_unix: now_unix + NOTICE_CACHE_EXPIRY_DELAY_SECS,
        });
        Ok(())
    }

    pub fn drain_newly_valid_for_subscriber(
        &mut self,
        sub: &GatewaySubscriber,
        now_unix: u64,
    ) -> Vec<EvePublicNotice> {
        self.remove_expired(now_unix);
        let mut dispatched = Vec::new();
        let mut remaining = VecDeque::with_capacity(self.entries.len());
        while let Some(entry) = self.entries.pop_front() {
            if entry.scope.matches_subscriber(sub) {
                self.current_bytes = self.current_bytes.saturating_sub(entry.byte_size);
                let mut targeted = entry.notice;
                targeted.target_group = Some(entry.scope.target_group_for_subscriber(sub));
                dispatched.push(targeted);
            } else {
                remaining.push_back(entry);
            }
        }
        self.entries = remaining;
        dispatched
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct QuasarHijackState {
    pub enabled: bool,
    pub error_code: Option<u32>,
    pub latency_sec: Option<f64>,
}

pub type IdempotencyKeyTuple = (i32, u32, String, Vec<u8>);

#[derive(Debug, Clone)]
pub struct CachedIdempotentResponse {
    pub status_code: u32,
    pub status_message: String,
    pub payload: Option<GoogleAny>,
    pub created_at_unix: u64,
}

#[derive(Debug, Default)]
pub struct BoundedIdempotencyCache {
    pub map: HashMap<IdempotencyKeyTuple, CachedIdempotentResponse>,
    pub order: VecDeque<(IdempotencyKeyTuple, u64)>,
}

impl BoundedIdempotencyCache {
    pub fn evict_expired(&mut self, now_unix: u64) {
        while let Some((_, ts)) = self.order.front() {
            if now_unix.saturating_sub(*ts) >= IDEMPOTENCY_TTL_SECS {
                let (k, old_ts) = self.order.pop_front().unwrap();
                if let Some(cur) = self.map.get(&k) {
                    if cur.created_at_unix == old_ts {
                        self.map.remove(&k);
                    }
                }
            } else {
                break;
            }
        }
    }

    pub fn get(&mut self, key: &IdempotencyKeyTuple, now_unix: u64) -> Option<CachedIdempotentResponse> {
        self.evict_expired(now_unix);
        self.map.get(key).cloned()
    }

    pub fn insert(
        &mut self,
        key: IdempotencyKeyTuple,
        entry: CachedIdempotentResponse,
        now_unix: u64,
    ) {
        self.evict_expired(now_unix);
        while self.map.len() >= MAX_IDEMPOTENCY_ENTRIES && !self.map.contains_key(&key) {
            if let Some((oldest_key, _)) = self.order.pop_front() {
                self.map.remove(&oldest_key);
            } else {
                break;
            }
        }
        self.order.push_back((key.clone(), now_unix));
        self.map.insert(key, entry);
    }
}

pub type QuasarHandlerFn = Arc<
    dyn Fn(&AuthoritativeContext, &GoogleAny, &DynamicQuasarMessage) -> Result<GoogleAny, (u32, String)>
        + Send
        + Sync,
>;

pub struct PublicGatewayRouter {
    pub descriptors: Arc<QuasarDescriptorRegistry>,
    pub gateway_instance_uuid: Vec<u8>,
    pub tenant: String,
    pub handlers: RwLock<HashMap<String, QuasarHandlerFn>>,
    pub idempotency_cache: RwLock<BoundedIdempotencyCache>,
    pub subscribers: RwLock<HashMap<u64, GatewaySubscriber>>,
    pub stream_receivers: RwLock<HashMap<u64, mpsc::Receiver<EvePublicNotice>>>,
    pub routing_cache: RwLock<NoticeRoutingCache>,
    pub published_events: RwLock<VecDeque<EvePublicEvent>>,
    pub server_hijack_state: RwLock<QuasarHijackState>,
    pub next_notice_seq: AtomicU64,
    pub pending_notices: Arc<RwLock<Vec<(NoticeScope, GoogleAny)>>>,
}

impl PublicGatewayRouter {
    pub fn new(
        descriptors: Arc<QuasarDescriptorRegistry>,
        db: Arc<WorldRepository>,
        sde: Arc<eve_sde::SdeRegistry>,
    ) -> Self {
        let router = Self {
            descriptors,
            gateway_instance_uuid: b"evomu-quasar-gw1".to_vec(),
            tenant: "tranquility".to_string(),
            handlers: RwLock::new(HashMap::new()),
            idempotency_cache: RwLock::new(BoundedIdempotencyCache::default()),
            subscribers: RwLock::new(HashMap::new()),
            stream_receivers: RwLock::new(HashMap::new()),
            routing_cache: RwLock::new(NoticeRoutingCache::new(NOTICE_CACHE_MAX_SIZE_BYTES)),
            published_events: RwLock::new(VecDeque::new()),
            server_hijack_state: RwLock::new(QuasarHijackState::default()),
            next_notice_seq: AtomicU64::new(1),
            pending_notices: Arc::new(RwLock::new(Vec::new())),
        };
        router.register_builtin_core_handlers(Arc::clone(&db));
        crate::services::quasar_social::register_social_services(&router, Arc::clone(&db));
        crate::services::quasar_market::register_market_handlers(
            &router,
            Arc::clone(&db),
            Arc::clone(&sde),
            Arc::clone(&router.descriptors),
        );
        crate::services::quasar_inventory_asset::register_inventory_asset_handlers(
            &router,
            Arc::clone(&db),
            Arc::clone(&sde),
            Arc::clone(&router.descriptors),
        );
        crate::services::quasar_assetholding_entitlements::register_assetholding_handlers(
            &router,
            Arc::clone(&db),
            Arc::clone(&sde),
            Arc::clone(&router.descriptors),
        );
        crate::services::quasar_economy_planetary_industry::register_economy_planetary_industry_handlers(
            &router,
            Arc::clone(&db),
            Arc::clone(&sde),
            Arc::clone(&router.descriptors),
        );
        router
    }

    pub fn enqueue_notice(&self, scope: NoticeScope, payload: GoogleAny) {
        self.pending_notices
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .push((scope, payload));
    }

    pub fn register_handler(&self, full_name_or_type_url: &str, handler: QuasarHandlerFn) {
        let key = QuasarDescriptorRegistry::normalize_type_name(full_name_or_type_url).to_string();
        self.handlers
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key, handler);
    }

    fn register_builtin_core_handlers(&self, db: Arc<WorldRepository>) {
        // 1. google.protobuf.Empty (Ping requests on PublicRequests / PublicNotices / PublicEvents gateway services)
        self.register_handler(
            "google.protobuf.Empty",
            Arc::new(|_ctx, _any, _dyn_msg| Ok(GoogleEmpty.to_any())),
        );

        // 2. eve_public.character.GetNamesByUserRequest -> eve_public.character.GetNamesByUserResponse
        let db_chars = Arc::clone(&db);
        self.register_handler(
            "eve_public.character.GetNamesByUserRequest",
            Arc::new(move |ctx, _any, _dyn_msg| {
                let Some(uid) = ctx.effective_user_id() else {
                    return Err((
                        STATUS_UNAUTHORIZED,
                        "Authenticated user required".to_string(),
                    ));
                };
                let chars = db_chars.get_characters_for_user(uid as u64);
                let mut out = Vec::new();
                for ch in chars {
                    let mut entry = Vec::new();
                    let cid = EvePublicCharacterId {
                        sequential: ch.character_id as u32,
                    };
                    encode_bytes_field(1, &cid.encode_proto(), &mut entry);
                    encode_string_field(2, &ch.name, &mut entry);
                    encode_bytes_field(1, &entry, &mut out);
                }
                Ok(GoogleAny {
                    type_url: format!(
                        "{TYPE_GOOGLEAPIS_PREFIX}eve_public.character.GetNamesByUserResponse"
                    ),
                    value: out,
                })
            }),
        );

        // 3. Q-7: Space, Celestials, Navigation, Insurgency & DBS Services
        crate::services::quasar_space_services::register_handlers(self, Arc::clone(&db));

        // 4. Q-8: Sovereignty, Faction Warfare & Operations Services
        crate::services::quasar_sov_services::register_handlers(self, Arc::clone(&db));

        // 5. Q-9: Cosmetics, SKINR, Titles, Heraldry & App Services
        crate::services::quasar_cosmetics_services::register_handlers(self, Arc::clone(&db));

        // 6. Q-3: Identity, Accounts, Entitlements, Store & Payment Services
        crate::services::quasar_auth_user::register_handlers(self, Arc::clone(&db));
        crate::services::quasar_entitlement_membership::register_handlers(self, Arc::clone(&db));
        crate::services::quasar_payment_store::register_handlers(self, Arc::clone(&db));

        // 7. Quasar Character & Progression Track (Q-4) handlers
        crate::services::quasar_progression::register_all_progression_handlers(
            self,
            Arc::clone(&db),
        );
    }

    /// Server-side policy determining whether a Quasar request message type requires an active character.
    /// User-level and Ping requests (`google.protobuf.Empty`, `eve_public.character.GetNamesByUserRequest`,
    /// and user/account/entitlement/payment/store services) can be invoked prior to character selection;
    /// only character-gameplay-scoped requests require an active character.
    pub fn requires_active_character(normalized_type_name: &str) -> bool {
        if matches!(
            normalized_type_name,
            "google.protobuf.Empty"
                | "eve_public.character.GetNamesByUserRequest"
                | "eve.quasar.config.api.GetRequest"
                | "eve_public.solarsystem.api.GetRequest"
                | "eve.constellation.GetRequest"
                | "eve.constellation.GetAllRequest"
                | "eve.constellation.SearchRequest"
                | "eve.region.GetRequest"
                | "eve.region.GetAllRequest"
                | "eve.region.SearchRequest"
                | "eve.faction.SearchRequest"
                | "eve.faction.api.GetRequest"
                | "eve.universe.navigation.GetShortestGateJumpCountRequest"
                | "eve.universe.navigation.GetShortestSystemJumpCountRequest"
                | "eve_public.app.launcher.distribution.api.LauncherDistributionClientStartRequest"
                | "eve_public.app.launcher.eve_client.GameClientStartRequest"
                | "eve_public.app.launcher.eve_client.GameClientStopRequest"
        ) {
            return false;
        }
        const USER_SCOPED_PREFIXES: &[&str] = &[
            "eve_public.auth",
            "eve.auth",
            "eve_public.login",
            "eve.login",
            "eve_public.user",
            "eve.user",
            "eve_public.entitlement",
            "eve.entitlement",
            "eve_public.omega",
            "eve.omega",
            "eve_public.membership",
            "eve.membership",
            "eve_public.payment",
            "eve.payment",
            "eve_public.plex",
            "eve.plex",
            "eve_public.store",
            "eve.store",
            "eve_public.vendors",
            "eve.vendors",
            "eve_public.fulfillment",
            "eve.fulfillment",
            "eve_public.isk",
            "eve.isk",
            "eve_public.legal",
            "eve.legal",
            "eve_public.pii",
            "eve.pii",
            "eve_public.device",
            "eve.device",
            "eve_public.cdn",
            "eve.cdn",
            "eve_public.localization",
            "eve.localization",
        ];
        if USER_SCOPED_PREFIXES
            .iter()
            .any(|prefix| normalized_type_name.starts_with(prefix))
        {
            return false;
        }
        true
    }

    pub fn default_app_instance_uuid(session_id: u64) -> Vec<u8> {
        let mut uuid = [0u8; 16];
        uuid[0..8].copy_from_slice(&0x4556_4F4D_555F_4757u64.to_be_bytes());
        uuid[8..16].copy_from_slice(&session_id.to_be_bytes());
        uuid.to_vec()
    }

    /// Subscribes a connected client session to the Quasar Notice stream (`MAX_SUBSCRIBER_INBOX_LEN` bounded).
    pub fn subscribe_session(
        &self,
        session: &SessionState,
        subscribed_notice_types: Vec<String>,
        now_unix: u64,
    ) -> mpsc::Receiver<EvePublicNotice> {
        let (tx, rx) = mpsc::channel(MAX_SUBSCRIBER_INBOX_LEN);
        let app_uuid = Self::default_app_instance_uuid(session.session_id);
        let mut sub = GatewaySubscriber {
            session_id: session.session_id,
            subscribed_at_unix: now_unix,
            application_instance_uuid: app_uuid,
            user_id: session.user_id.map(|u| u as i32),
            character_id: session.character_id.map(|c| c as u32),
            corporation_id: session.corporation_id.map(|c| c as u32),
            alliance_id: session.alliance_id.map(|a| a as i32),
            solar_system_id: session.solar_system_id,
            fleet_id: session.fleet_id,
            bubble_instance_uuid: None,
            subscribed_notice_types,
            sender: tx,
            inbox: VecDeque::new(),
        };

        let cached = self
            .routing_cache
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .drain_newly_valid_for_subscriber(&sub, now_unix);
        for notice in cached {
            if sub.accepts_payload_type(
                notice
                    .payload
                    .as_ref()
                    .map(|p| p.type_url.as_str())
                    .unwrap_or(""),
            ) {
                sub.push_notice(notice);
            }
        }

        let mut subs = self.subscribers.write().unwrap_or_else(|e| e.into_inner());
        if subs.len() >= MAX_GATEWAY_SUBSCRIBERS && !subs.contains_key(&session.session_id) {
            if let Some(oldest_sid) = subs
                .values()
                .min_by_key(|s| (s.subscribed_at_unix, s.session_id))
                .map(|s| s.session_id)
            {
                subs.remove(&oldest_sid);
                self.stream_receivers
                    .write()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&oldest_sid);
            }
        }
        subs.insert(session.session_id, sub);
        rx
    }

    pub fn has_subscriber(&self, session_id: u64) -> bool {
        self.subscribers
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(&session_id)
    }

    pub fn take_stream_receiver(&self, session_id: u64) -> Option<mpsc::Receiver<EvePublicNotice>> {
        self.stream_receivers
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&session_id)
    }

    pub fn unsubscribe_session(&self, session_id: u64) {
        self.subscribers
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&session_id);
        self.stream_receivers
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&session_id);
    }

    /// Updates a subscriber's session tenancy attributes (`charid`, `corpid`, `allianceid`, `solarsystemid2`, `fleetid`)
    /// and flushes any newly valid notices from `NoticeRoutingCache`.
    pub fn on_session_changed(&self, session: &SessionState, now_unix: u64) -> usize {
        if !self.has_subscriber(session.session_id) {
            return 0;
        }
        let mut subs = self.subscribers.write().unwrap_or_else(|e| e.into_inner());
        let Some(sub) = subs.get_mut(&session.session_id) else {
            return 0;
        };
        sub.update_from_session(session);
        let newly_valid = self
            .routing_cache
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .drain_newly_valid_for_subscriber(sub, now_unix);
        let mut delivered = 0usize;
        for notice in newly_valid {
            if sub.accepts_payload_type(
                notice
                    .payload
                    .as_ref()
                    .map(|p| p.type_url.as_str())
                    .unwrap_or(""),
            ) {
                sub.push_notice(notice);
                delivered += 1;
            }
        }
        delivered
    }

    pub fn poll_notices(&self, session_id: u64) -> Vec<EvePublicNotice> {
        let mut subs = self.subscribers.write().unwrap_or_else(|e| e.into_inner());
        let Some(sub) = subs.get_mut(&session_id) else {
            return Vec::new();
        };
        sub.inbox.drain(..).collect()
    }

    /// Server-only notice publisher: publishes an `EvePublicNotice` to all connected clients matching `scope`.
    /// Not callable by clients (`R-Q2-P0-1`). If no connected subscriber currently matches `scope`,
    /// caches the notice in `NoticeRoutingCache` so transitioning sessions receive it on `on_session_changed`.
    pub fn publish_notice(
        &self,
        scope: NoticeScope,
        payload: GoogleAny,
        now_unix: u64,
    ) -> Result<(EvePublicNotice, usize), BridgeRpcError> {
        let seq = self.next_notice_seq.fetch_add(1, Ordering::Relaxed);
        let mut uuid = [0u8; 16];
        uuid[0..8].copy_from_slice(&now_unix.to_be_bytes());
        uuid[8..16].copy_from_slice(&seq.to_be_bytes());

        let base_notice = EvePublicNotice {
            dispatched: Some(GoogleTimestamp {
                seconds: now_unix as i64,
                nanos: 0,
            }),
            uuid: uuid.to_vec(),
            internal_origin: "evomu.quasar.notice_publisher".to_string(),
            tenant: self.tenant.clone(),
            payload: Some(payload),
            target_group: scope.base_target_group(),
            trace_context: None,
        };

        let mut delivered = 0usize;
        {
            let mut subs = self.subscribers.write().unwrap_or_else(|e| e.into_inner());
            for sub in subs.values_mut() {
                if scope.matches_subscriber(sub) {
                    let payload_url = base_notice
                        .payload
                        .as_ref()
                        .map(|p| p.type_url.as_str())
                        .unwrap_or("");
                    if !sub.accepts_payload_type(payload_url) {
                        continue;
                    }
                    let mut targeted = base_notice.clone();
                    targeted.target_group = Some(scope.target_group_for_subscriber(sub));
                    sub.push_notice(targeted);
                    delivered += 1;
                }
            }
        }

        if delivered == 0 {
            self.routing_cache
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .add_notice(base_notice.clone(), scope, now_unix)?;
        }

        Ok((base_notice, delivered))
    }

    /// Extracts an embedded idempotency key (`eve.idempotency.Token`) from a dynamic message if present.
    fn extract_embedded_idempotency_key(&self, dyn_msg: &DynamicQuasarMessage) -> Option<Vec<u8>> {
        let desc = self.descriptors.get_message(&dyn_msg.full_name)?;
        for f in &desc.fields {
            let norm = QuasarDescriptorRegistry::normalize_type_name(&f.type_name);
            if norm == "eve.idempotency.Token" {
                if let Some(DynamicValue::Message(sub)) = dyn_msg.get(f.number) {
                    if let Some(DynamicValue::Bytes(uuid)) = sub.get(1) {
                        if !uuid.is_empty() {
                            return Some(uuid.clone());
                        }
                    }
                }
            }
        }
        None
    }

    /// Processes a framed `EvePublicRequest` against an authenticated `SessionState`,
    /// enforcing tenancy, bounded idempotency deduplication, QA hijack rules, and schema validation.
    pub fn handle_public_request(
        &self,
        session: &SessionState,
        request: &EvePublicRequest,
        explicit_idempotency_key: Option<Vec<u8>>,
        require_character_override: bool,
        now_unix: u64,
    ) -> EvePublicResponse {
        let app_uuid = if request.application_instance_uuid.is_empty() {
            Self::default_app_instance_uuid(session.session_id)
        } else {
            request.application_instance_uuid.clone()
        };

        let srv_hijack = self
            .server_hijack_state
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let (dispatched_sec, dispatched_nanos) = match (srv_hijack.enabled, srv_hijack.latency_sec) {
            (true, Some(lat)) if lat > 0.0 && lat.is_finite() => {
                let extra_secs = lat.trunc() as i64;
                let extra_nanos = ((lat.fract()) * 1_000_000_000.0).round() as i32;
                (now_unix as i64 + extra_secs, extra_nanos)
            }
            _ => (now_unix as i64, 0),
        };

        let build_response =
            |status_code: u32, status_message: String, payload: Option<GoogleAny>| {
                EvePublicResponse {
                    dispatched: Some(GoogleTimestamp {
                        seconds: dispatched_sec,
                        nanos: dispatched_nanos,
                    }),
                    correlation_uuid: request.correlation_uuid.clone(),
                    status_code,
                    status_message,
                    payload,
                    internal_origin: "evomu.quasar.public_gateway".to_string(),
                    application_instance_uuid: app_uuid.clone(),
                    gateway_instance_uuid: self.gateway_instance_uuid.clone(),
                    trace_context: request.trace_context.clone(),
                }
            };

        // 1. Check session authentication
        let Some(session_uid) = session.user_id.map(|u| u as i32) else {
            return build_response(
                STATUS_UNAUTHORIZED,
                "Unauthenticated gateway session: user login required".to_string(),
                None,
            );
        };
        let session_cid = session.character_id.map(|c| c as u32);
        if require_character_override && session_cid.is_none() {
            return build_response(
                STATUS_FORBIDDEN,
                "Active character selection required for character-scoped gateway request"
                    .to_string(),
                None,
            );
        }

        // 2. Verify client-supplied AuthoritativeContext does not spoof another user/character/corporation
        if let Some(req_ctx) = &request.authoritative_context {
            if let Some(claimed_uid) = req_ctx.effective_user_id() {
                if claimed_uid != session_uid {
                    return build_response(
                        STATUS_FORBIDDEN,
                        format!(
                            "AuthoritativeContext user mismatch: session user {session_uid} vs claimed {claimed_uid}"
                        ),
                        None,
                    );
                }
            }
            if let Some(claimed_cid) = req_ctx.effective_character_id() {
                if Some(claimed_cid) != session_cid {
                    return build_response(
                        STATUS_FORBIDDEN,
                        format!(
                            "AuthoritativeContext character mismatch: session character {:?} vs claimed {claimed_cid}",
                            session_cid
                        ),
                        None,
                    );
                }
            }
            if let Some(claimed_corp) = req_ctx.effective_corporation_id() {
                let session_corp = session.corporation_id.map(|c| c as u32);
                if session_corp.is_some() && Some(claimed_corp) != session_corp {
                    return build_response(
                        STATUS_FORBIDDEN,
                        format!(
                            "AuthoritativeContext corporation mismatch: session corp {:?} vs claimed {claimed_corp}",
                            session_corp
                        ),
                        None,
                    );
                }
            }
        }

        // 3. Check QA Hijack Mode (`serviceGatewayMgr`)
        {
            let srv_hijack = self
                .server_hijack_state
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            if srv_hijack.enabled {
                if let Some(code) = srv_hijack.error_code {
                    return build_response(code, format!("Error: {code}"), None);
                }
            }
        }

        // 4. Validate payload presence and lookup schema in 1,455-file QuasarDescriptorRegistry
        let Some(payload_any) = &request.payload else {
            return build_response(
                STATUS_BAD_REQUEST,
                "Missing google.protobuf.Any payload in eve_public.Request".to_string(),
                None,
            );
        };
        let norm_type = payload_any.normalized_type_name().to_string();
        if self.descriptors.get_message(&norm_type).is_none() {
            return build_response(
                STATUS_NOT_FOUND,
                format!("Unknown Quasar protobuf message type '{norm_type}'"),
                None,
            );
        }

        // Enforce server-side active-character policy for character-scoped message types
        if Self::requires_active_character(&norm_type) && session_cid.is_none() {
            return build_response(
                STATUS_FORBIDDEN,
                format!(
                    "Active character selection required for character-scoped request '{norm_type}'"
                ),
                None,
            );
        }

        let dyn_msg = match self.descriptors.decode_dynamic(&norm_type, &payload_any.value) {
            Ok(m) => m,
            Err(e) => {
                return build_response(
                    STATUS_BAD_REQUEST,
                    format!("Invalid protobuf wire payload for '{norm_type}': {e}"),
                    None,
                );
            }
        };

        // 5. Bounded idempotency key deduplication (with TTL + max-entry eviction)
        let idem_key = explicit_idempotency_key
            .or_else(|| self.extract_embedded_idempotency_key(&dyn_msg));
        let cid_key = session_cid.unwrap_or(0);
        if let Some(ref ikey) = idem_key {
            let cache_tuple = (session_uid, cid_key, norm_type.clone(), ikey.clone());
            if let Some(cached) = self
                .idempotency_cache
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .get(&cache_tuple, now_unix)
            {
                return build_response(cached.status_code, cached.status_message, cached.payload);
            }
        }

        // 6. Build authoritative server context and dispatch handler
        let auth_ctx = AuthoritativeContext::for_session_at(
            Some(session_uid),
            session_cid,
            session.corporation_id.map(|c| c as u32),
            session.corp_role,
            self.tenant.clone(),
            self.gateway_instance_uuid.clone(),
            now_unix,
        );

        let handler_opt = self
            .handlers
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&norm_type)
            .cloned();

        let (status_code, status_message, rsp_payload) = match handler_opt {
            Some(handler) => match handler(&auth_ctx, payload_any, &dyn_msg) {
                Ok(any_out) => (STATUS_OK, "OK".to_string(), Some(any_out)),
                Err((code, msg)) => (code, msg, None),
            },
            None => (
                STATUS_NOT_IMPLEMENTED,
                format!("Quasar handler for '{norm_type}' is not yet implemented"),
                None,
            ),
        };

        // Drain any notices queued by Quasar social service handlers and fan out to subscribers
        for (scope, payload, ts) in crate::services::quasar_social_chat::drain_local_notices() {
            let _ = self.publish_notice(scope, payload, ts);
        }

        if let Some(ikey) = idem_key {
            let cache_tuple = (session_uid, cid_key, norm_type, ikey);
            self.idempotency_cache
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .insert(
                    cache_tuple,
                    CachedIdempotentResponse {
                        status_code,
                        status_message: status_message.clone(),
                        payload: rsp_payload.clone(),
                        created_at_unix: now_unix,
                    },
                    now_unix,
                );
        }

        // Drain and publish any pending notices queued by request handlers
        let queued: Vec<_> = self
            .pending_notices
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .drain(..)
            .collect();
        for (scope, payload) in queued {
            let _ = self.publish_notice(scope, payload, now_unix);
        }

        build_response(status_code, status_message, rsp_payload)
    }

    /// Server-only handler for an `eve_public.internal_gateway.Request` envelope (`R-Q2-P1-3`).
    pub fn handle_internal_gateway_request(
        &self,
        session: &SessionState,
        internal_req: &InternalGatewayRequest,
        now_unix: u64,
    ) -> InternalGatewayResponse {
        let rsp = match &internal_req.request {
            Some(pub_req) => self.handle_public_request(session, pub_req, None, false, now_unix),
            None => EvePublicResponse {
                dispatched: Some(GoogleTimestamp {
                    seconds: now_unix as i64,
                    nanos: 0,
                }),
                status_code: STATUS_BAD_REQUEST,
                status_message: "Missing inner eve_public.Request".into(),
                ..Default::default()
            },
        };
        InternalGatewayResponse {
            response: Some(rsp),
            ack_id: internal_req.ack_id,
            subscription: None,
        }
    }

    /// Server-only handler for an internal `eve.Request` envelope (`R-Q2-P1-3`).
    pub fn handle_eve_internal_request(
        &self,
        session: &SessionState,
        req: &EveInternalRequest,
        now_unix: u64,
    ) -> EveInternalResponse {
        let pub_req = EvePublicRequest {
            issued: req.created,
            correlation_uuid: req.correlation_uuid.as_bytes().to_vec(),
            payload: req.payload.clone(),
            external_origin: req.publisher.clone(),
            application_instance_uuid: Self::default_app_instance_uuid(session.session_id),
            authoritative_context: None,
            trace_context: req.trace_context.clone(),
        };
        let pub_rsp = self.handle_public_request(session, &pub_req, None, false, now_unix);
        EveInternalResponse {
            correlation_uuid: req.correlation_uuid.clone(),
            status_code: pub_rsp.status_code,
            status_message: pub_rsp.status_message,
            payload: pub_rsp.payload,
            tenant: self.tenant.clone(),
            publisher: "evomu.quasar.gateway".into(),
            payload_not_cached: true,
            payload_cache_info: None,
            payload_not_paged: true,
            payload_paging_info: None,
            recipient: req.publisher.clone(),
            created: pub_rsp.dispatched,
            trace_context: pub_rsp.trace_context,
        }
    }

    /// Publishes a client telemetry/domain `EvePublicEvent` (`eve_public.gateway.Events.Publish`),
    /// capped at `MAX_PUBLISHED_EVENTS` entries (`R-Q2-P1-4`).
    pub fn publish_event(&self, session: &SessionState, mut event: EvePublicEvent, now_unix: u64) {
        if event.occurred.is_none() {
            event.occurred = Some(GoogleTimestamp {
                seconds: now_unix as i64,
                nanos: 0,
            });
        }
        event.authoritative_context = Some(AuthoritativeContext::for_session_at(
            session.user_id.map(|u| u as i32),
            session.character_id.map(|c| c as u32),
            session.corporation_id.map(|c| c as u32),
            session.corp_role,
            self.tenant.clone(),
            self.gateway_instance_uuid.clone(),
            now_unix,
        ));
        let mut events = self
            .published_events
            .write()
            .unwrap_or_else(|e| e.into_inner());
        if events.len() >= MAX_PUBLISHED_EVENTS {
            events.pop_front();
        }
        events.push_back(event);
    }

    pub fn set_server_hijack_mode(&self, enabled: bool, error: Option<u32>, latency: Option<f64>) {
        *self
            .server_hijack_state
            .write()
            .unwrap_or_else(|e| e.into_inner()) = QuasarHijackState {
            enabled,
            error_code: error,
            latency_sec: latency,
        };
    }

    pub fn is_server_hijack_mode_enabled(&self) -> bool {
        self.server_hijack_state
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .enabled
    }
}

pub fn response_to_pyrep(rsp: &EvePublicResponse) -> PyRep {
    let raw = rsp.encode_proto();
    let payload_type_url = rsp
        .payload
        .as_ref()
        .map(|p| p.type_url.clone())
        .unwrap_or_default();
    let payload_bytes = rsp
        .payload
        .as_ref()
        .map(|p| p.value.clone())
        .unwrap_or_default();
    PyRep::keyval([
        ("status_code", PyRep::Integer(rsp.status_code as i64)),
        ("status_message", PyRep::String(rsp.status_message.clone())),
        (
            "correlation_uuid",
            PyRep::Bytes(rsp.correlation_uuid.clone()),
        ),
        ("type_url", PyRep::String(payload_type_url)),
        ("payload", PyRep::Bytes(payload_bytes)),
        ("response_proto", PyRep::Bytes(raw)),
    ])
}

pub fn notice_to_pyrep(notice: &EvePublicNotice) -> PyRep {
    let raw = notice.encode_proto();
    let payload_type_url = notice
        .payload
        .as_ref()
        .map(|p| p.type_url.clone())
        .unwrap_or_default();
    let payload_bytes = notice
        .payload
        .as_ref()
        .map(|p| p.value.clone())
        .unwrap_or_default();
    let target_group = notice
        .target_group
        .as_ref()
        .map(|tg| tg.target_group_name().to_string())
        .unwrap_or_default();
    PyRep::keyval([
        ("uuid", PyRep::Bytes(notice.uuid.clone())),
        ("tenant", PyRep::String(notice.tenant.clone())),
        ("target_group", PyRep::String(target_group)),
        ("type_url", PyRep::String(payload_type_url)),
        ("payload", PyRep::Bytes(payload_bytes)),
        ("notice_proto", PyRep::Bytes(raw)),
    ])
}

/// Validates raw `WireMessage` sanity when decoding untrusted frames.
pub fn validate_raw_wire_frame(buf: &[u8]) -> Result<WireMessage, QuasarError> {
    WireMessage::decode(buf)
}

pub const SERVICE_GATEWAY_MGR_NAME: &str = "serviceGatewayMgr";

fn parse_hijack_args(
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<(Option<u32>, Option<f64>), BridgeRpcError> {
    let err_rep = args.first().or_else(|| kwargs.get("error"));
    let error = match err_rep {
        None | Some(PyRep::None) => None,
        Some(PyRep::String(s)) | Some(PyRep::Token(s)) => match s.as_str() {
            "None" | "" => None,
            "TimeoutException" => Some(STATUS_GATEWAY_TIMEOUT),
            "UnpackException" => Some(STATUS_BAD_REQUEST),
            other => {
                return Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: format!("Unsupported Quasar hijack error option '{other}'"),
                });
            }
        },
        Some(PyRep::Integer(code)) => match *code {
            400 | 401 | 403 | 404 | 405 | 409 | 412 | 415 | 500 | 501 | 503 | 504 => {
                Some(*code as u32)
            }
            other => {
                return Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: format!(
                        "Unsupported Quasar hijack HTTP status code {other} (must be V24 ERROR_OPTIONS)"
                    ),
                });
            }
        },
        Some(other) => {
            return Err(BridgeRpcError {
                code: "InvalidArgument".into(),
                message: format!("Invalid Quasar hijack error argument: {other:?}"),
            });
        }
    };

    let lat_rep = args.get(1).or_else(|| kwargs.get("latency"));
    let latency = match lat_rep {
        None | Some(PyRep::None) => None,
        Some(PyRep::Real(f)) => {
            if !f.is_finite() || *f < 0.0 || *f > 300.0 {
                return Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: format!("Invalid Quasar hijack latency {f} (must be 0.0..=300.0 sec)"),
                });
            }
            Some(*f)
        }
        Some(PyRep::Integer(i)) => {
            if *i < 0 || *i > 300 {
                return Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: format!("Invalid Quasar hijack latency {i} (must be 0..=300 sec)"),
                });
            }
            Some(*i as f64)
        }
        Some(other) => {
            return Err(BridgeRpcError {
                code: "InvalidArgument".into(),
                message: format!("Invalid Quasar hijack latency argument: {other:?}"),
            });
        }
    };

    Ok((error, latency))
}

impl PublicGatewayRouter {
    /// Handles `serviceGatewayMgr` QA hijack mode RPCs (`qa_enable_hijack_mode`, `qa_disable_hijack_mode`,
    /// `qa_is_hijack_mode_enabled`) as consumed by `ServerRequestHijackWindow` in
    /// `eve/devtools/script/quasarHijackWnd.pyj`:
    /// - `ServerRequestHijackWindow.LoadData`:
    ///   `self.is_enabled = sm.RemoteSvc('serviceGatewayMgr').qa_is_hijack_mode_enabled()` -> `PyRep::Bool`
    /// - `ServerRequestHijackWindow.EnableHijack`:
    ///   `sm.RemoteSvc('serviceGatewayMgr').qa_enable_hijack_mode(self.error, self.latency)` -> `PyRep::None`
    /// - `ServerRequestHijackWindow.DisableHijack`:
    ///   `sm.RemoteSvc('serviceGatewayMgr').qa_disable_hijack_mode()` -> `PyRep::None`
    pub fn dispatch_service_gateway_mgr_rpc(
        &self,
        method: &str,
        args: &[PyRep],
        kwargs: &HashMap<String, PyRep>,
    ) -> Option<Result<PyRep, BridgeRpcError>> {
        match method {
            "qa_enable_hijack_mode" => {
                let (error, latency) = match parse_hijack_args(args, kwargs) {
                    Ok(v) => v,
                    Err(e) => return Some(Err(e)),
                };
                self.set_server_hijack_mode(true, error, latency);
                Some(Ok(PyRep::None))
            }
            "qa_disable_hijack_mode" => {
                self.set_server_hijack_mode(false, None, None);
                Some(Ok(PyRep::None))
            }
            "qa_is_hijack_mode_enabled" => {
                Some(Ok(PyRep::Bool(self.is_server_hijack_mode_enabled())))
            }
            _ => None,
        }
    }

    /// Handles client-facing `publicGatewaySvc` RPCs (`SendRequest`, `CallQuasar`, `Send`,
    /// `Subscribe`, `SubscribeToNotice`, `ConsumeNotices`, `PollNotices`, `PublishEvent`).
    /// Server-only operations (`PublishNotice`, `HandleInternalRequest`, `ConsumeEvents`) are not
    /// exposed here (`R-Q2-P0-1`, `R-Q2-P1-3`).
    pub fn dispatch_public_gateway_rpc(
        &self,
        session: &SessionState,
        method: &str,
        args: &[PyRep],
        kwargs: &HashMap<String, PyRep>,
        now_unix: u64,
    ) -> Option<Result<PyRep, BridgeRpcError>> {
        match method {
            "Subscribe" | "SubscribeToNotice" | "ConsumeNotices" => {
                if session.user_id.is_none() {
                    return Some(Err(BridgeRpcError {
                        code: "AuthenticationRequired".into(),
                        message: "publicGatewaySvc subscription requires an authenticated session"
                            .into(),
                    }));
                }
                let mut notice_types = Vec::new();
                for arg in args {
                    match arg {
                        PyRep::String(s) => notice_types.push(s.clone()),
                        PyRep::List(items) | PyRep::Tuple(items) => {
                            for it in items {
                                if let Some(s) = it.as_str() {
                                    notice_types.push(s.to_string());
                                }
                            }
                        }
                        _ => {}
                    }
                }
                let rx = self.subscribe_session(session, notice_types, now_unix);
                self.stream_receivers
                    .write()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(session.session_id, rx);
                let app_uuid = Self::default_app_instance_uuid(session.session_id);
                Some(Ok(PyRep::keyval([
                    ("application_instance_uuid", PyRep::Bytes(app_uuid)),
                    ("subscribed", PyRep::Bool(true)),
                ])))
            }
            "PollNotices" => {
                let notices = self.poll_notices(session.session_id);
                Some(Ok(PyRep::List(
                    notices.iter().map(notice_to_pyrep).collect(),
                )))
            }
            "PublishEvent" => {
                if let Some(PyRep::Bytes(raw_event)) | Some(PyRep::Buffer(raw_event)) = args.first()
                {
                    if session.user_id.is_none() {
                        return Some(Err(BridgeRpcError {
                            code: "AuthenticationRequired".into(),
                            message: "publicGatewaySvc.PublishEvent requires authentication".into(),
                        }));
                    }
                    let event = match EvePublicEvent::decode_proto(raw_event) {
                        Ok(ev) => ev,
                        Err(e) => {
                            return Some(Err(BridgeRpcError {
                                code: "InvalidProtobuf".into(),
                                message: e.to_string(),
                            }))
                        }
                    };
                    self.publish_event(session, event, now_unix);
                    return Some(Ok(PyRep::Bool(true)));
                }
                None
            }
            "SendRequest" | "CallQuasar" | "Send" => {
                let explicit_idem = kwargs
                    .get("idempotency_key")
                    .and_then(|v| match v {
                        PyRep::Bytes(b) | PyRep::Buffer(b) => Some(b.clone()),
                        PyRep::String(s) => Some(s.as_bytes().to_vec()),
                        _ => None,
                    });

                // Binary `EvePublicRequest` wire envelope
                if let Some(PyRep::Bytes(raw_req)) | Some(PyRep::Buffer(raw_req)) =
                    args.first().or_else(|| kwargs.get("request"))
                {
                    let rsp = match EvePublicRequest::decode_proto(raw_req) {
                        Ok(req) => self.handle_public_request(
                            session,
                            &req,
                            explicit_idem,
                            false,
                            now_unix,
                        ),
                        Err(e) => EvePublicResponse {
                            dispatched: Some(GoogleTimestamp {
                                seconds: now_unix as i64,
                                nanos: 0,
                            }),
                            status_code: STATUS_BAD_REQUEST,
                            status_message: format!("Malformed eve_public.Request wire frame: {e}"),
                            internal_origin: "evomu.quasar.public_gateway".into(),
                            application_instance_uuid: Self::default_app_instance_uuid(
                                session.session_id,
                            ),
                            gateway_instance_uuid: self.gateway_instance_uuid.clone(),
                            ..Default::default()
                        },
                    };
                    return Some(Ok(response_to_pyrep(&rsp)));
                }

                None
            }
            _ => None,
        }
    }
}
