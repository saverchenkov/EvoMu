//! General-purpose typed per-entity and global key-value store (`#913` / RFC-2 `P2-3`).
//!
//! Replaces ad-hoc `format!("{prefix}:{id}")` keys and unbounded JSON maps in
//! `service_state_blobs` as well as synthetic negative `owner_notes` keys (`#945`)
//! with a relational `(ns, entity_id, sub_key)` table governed by a closed [`Ns`]
//! namespace registry.

use crate::snapshot_ext::WorldStateSnapshot;
use serde::{de::DeserializeOwned, Deserialize, Deserializer, Serialize, Serializer};
use std::collections::BTreeMap;

/// Default maximum serialized JSON payload size (64 KiB) for a single `(ns, entity_id, sub_key)` row.
pub const DEFAULT_ENTITY_KV_MAX_VALUE_BYTES: usize = 65_536;

/// Entity ownership scope for an [`Ns`] namespace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum EntityKvScope {
    Character,
    User,
    Corporation,
    Contract,
    Fleet,
    Global,
}

/// Closed registry of `entity_kv` namespaces (`#913`).
///
/// Every namespace declares its canonical string key, ownership [`EntityKvScope`], per-row byte
/// budget, and whether rows cascade-delete when the owning entity is deleted. There is no
/// `Custom(String)` escape hatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Ns {
    /// Per-character flag recording whether the Alpha clone availability notification was emitted (`bool`).
    AlphaAvailNotified,
    /// Per-user flag recording whether the one-time starter 50 PLEX grant was credited (`bool`).
    StarterPlexGranted,
    /// Per-user timestamp of the most recent weekly Omega SP reward claim (`u64`).
    OmegaWeeklySpClaim,
    /// Per-character remote home-station change cooldown expiry Unix timestamp (`u64`, `#945`).
    HomeStationRemoteCooldown,
    /// Per-contract, per-item (`sub_key = item_id`) original inventory flag before escrow (`u16`).
    ContractOrigFlag,
    /// Per-contract, per-bidder (`sub_key = bidder_id`) corporation wallet division (`u8`).
    ContractBidDiv,
    /// Per-contract escrow deposit amount in ISK cents (`i64`).
    ContractDeposit,
    /// Per-user character transfer consent token (`String`).
    CharTransferConsentToken,
    /// Per-character timestamp of the last completed character transfer (`u64`).
    CharTransferLastUnix,
    /// Per-character session eviction timestamp following a character transfer (`u64`).
    CharTransferEvictSession,
    /// Per-character XMPP chat authentication token (`String`).
    XmppAuthToken,
    /// Global per-language (`sub_key = lang`) XMPP help channel name (`String`).
    XmppHelpChannel,
    /// Global XMPP server hostname (`String`).
    XmppHostname,
    /// Global cache setting entry (`sub_key = "{section}:{key}"`, `String`).
    CacheSetting,
    /// Global banned words / regex filter lists (`sub_key = "words" | "regexes"`, `String`).
    BannedWords,
    /// Global Kiring / EveGuard configuration (`sub_key = config_key`, `String`).
    KiringConfig,
    /// Global community track QA mode toggle (`bool`).
    CommunityTrackQaMode,
    /// Global Zarzakh Horizon Siege active flag (`bool`).
    ZarzakhHorizonSiegeActive,
    /// Global daily industry system cost index snapshot (`serde_json::Value`).
    IndustrySystemCostIndicesDaily,
    /// Global dynamic resource settings map (`HashMap<String, serde_json::Value>`).
    DynamicResourceSettings,
    /// Global market seeder version tag (`String`).
    MarketSeederVersion,
}

impl Ns {
    /// Exhaustive list of all registered `entity_kv` namespaces.
    pub const ALL: &'static [Ns] = &[
        Ns::AlphaAvailNotified,
        Ns::StarterPlexGranted,
        Ns::OmegaWeeklySpClaim,
        Ns::HomeStationRemoteCooldown,
        Ns::ContractOrigFlag,
        Ns::ContractBidDiv,
        Ns::ContractDeposit,
        Ns::CharTransferConsentToken,
        Ns::CharTransferLastUnix,
        Ns::CharTransferEvictSession,
        Ns::XmppAuthToken,
        Ns::XmppHelpChannel,
        Ns::XmppHostname,
        Ns::CacheSetting,
        Ns::BannedWords,
        Ns::KiringConfig,
        Ns::CommunityTrackQaMode,
        Ns::ZarzakhHorizonSiegeActive,
        Ns::IndustrySystemCostIndicesDaily,
        Ns::DynamicResourceSettings,
        Ns::MarketSeederVersion,
    ];

    /// Canonical persisted namespace identifier stored in the `entity_kv.ns` column.
    #[inline]
    pub const fn as_str(self) -> &'static str {
        match self {
            Ns::AlphaAvailNotified => "character.alpha_avail_notified",
            Ns::StarterPlexGranted => "user.starter_plex_granted",
            Ns::OmegaWeeklySpClaim => "user.omega_weekly_sp_claim",
            Ns::HomeStationRemoteCooldown => "character.home_station_remote_cooldown",
            Ns::ContractOrigFlag => "contract.orig_flag",
            Ns::ContractBidDiv => "contract.bid_div",
            Ns::ContractDeposit => "contract.deposit",
            Ns::CharTransferConsentToken => "user.char_transfer_consent_token",
            Ns::CharTransferLastUnix => "character.transfer_last_unix",
            Ns::CharTransferEvictSession => "character.transfer_evict_session",
            Ns::XmppAuthToken => "character.xmpp_token",
            Ns::XmppHelpChannel => "global.xmpp_help_channel",
            Ns::XmppHostname => "global.xmpp_hostname",
            Ns::CacheSetting => "global.cache_setting",
            Ns::BannedWords => "global.banned_words",
            Ns::KiringConfig => "global.kiring_config",
            Ns::CommunityTrackQaMode => "global.community_track_qa_mode",
            Ns::ZarzakhHorizonSiegeActive => "global.zarzakh_horizon_siege_active",
            Ns::IndustrySystemCostIndicesDaily => "global.industry_sci_daily",
            Ns::DynamicResourceSettings => "global.dynamic_resource_settings",
            Ns::MarketSeederVersion => "global.market_seeder_version",
        }
    }

    /// Parses a persisted namespace string into a registered [`Ns`] variant.
    pub fn from_str_name(s: &str) -> Option<Self> {
        match s {
            "character.alpha_avail_notified" => Some(Ns::AlphaAvailNotified),
            "user.starter_plex_granted" => Some(Ns::StarterPlexGranted),
            "user.omega_weekly_sp_claim" => Some(Ns::OmegaWeeklySpClaim),
            "character.home_station_remote_cooldown" => Some(Ns::HomeStationRemoteCooldown),
            "contract.orig_flag" => Some(Ns::ContractOrigFlag),
            "contract.bid_div" => Some(Ns::ContractBidDiv),
            "contract.deposit" => Some(Ns::ContractDeposit),
            "user.char_transfer_consent_token" => Some(Ns::CharTransferConsentToken),
            "character.transfer_last_unix" => Some(Ns::CharTransferLastUnix),
            "character.transfer_evict_session" => Some(Ns::CharTransferEvictSession),
            "character.xmpp_token" => Some(Ns::XmppAuthToken),
            "global.xmpp_help_channel" => Some(Ns::XmppHelpChannel),
            "global.xmpp_hostname" => Some(Ns::XmppHostname),
            "global.cache_setting" => Some(Ns::CacheSetting),
            "global.banned_words" => Some(Ns::BannedWords),
            "global.kiring_config" => Some(Ns::KiringConfig),
            "global.community_track_qa_mode" => Some(Ns::CommunityTrackQaMode),
            "global.zarzakh_horizon_siege_active" => Some(Ns::ZarzakhHorizonSiegeActive),
            "global.industry_sci_daily" => Some(Ns::IndustrySystemCostIndicesDaily),
            "global.dynamic_resource_settings" => Some(Ns::DynamicResourceSettings),
            "global.market_seeder_version" => Some(Ns::MarketSeederVersion),
            _ => None,
        }
    }

    /// Returns the owning entity scope for this namespace.
    #[inline]
    pub const fn scope(self) -> EntityKvScope {
        match self {
            Ns::AlphaAvailNotified
            | Ns::HomeStationRemoteCooldown
            | Ns::CharTransferLastUnix
            | Ns::CharTransferEvictSession
            | Ns::XmppAuthToken => EntityKvScope::Character,
            Ns::StarterPlexGranted | Ns::OmegaWeeklySpClaim | Ns::CharTransferConsentToken => {
                EntityKvScope::User
            }
            Ns::ContractOrigFlag | Ns::ContractBidDiv | Ns::ContractDeposit => {
                EntityKvScope::Contract
            }
            Ns::XmppHelpChannel
            | Ns::XmppHostname
            | Ns::CacheSetting
            | Ns::BannedWords
            | Ns::KiringConfig
            | Ns::CommunityTrackQaMode
            | Ns::ZarzakhHorizonSiegeActive
            | Ns::IndustrySystemCostIndicesDaily
            | Ns::DynamicResourceSettings
            | Ns::MarketSeederVersion => EntityKvScope::Global,
        }
    }

    /// Maximum serialized byte size allowed for a single row in this namespace (default 64 KiB).
    #[inline]
    pub const fn max_value_bytes(self) -> usize {
        DEFAULT_ENTITY_KV_MAX_VALUE_BYTES
    }

    /// Whether rows in this namespace should be deleted when the owning entity is deleted.
    #[inline]
    pub const fn cascade_on_owner_delete(self) -> bool {
        !matches!(self.scope(), EntityKvScope::Global)
    }
}

impl Serialize for Ns {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Ns {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Ns::from_str_name(&s)
            .ok_or_else(|| serde::de::Error::custom(format!("unknown entity_kv namespace '{s}'")))
    }
}

/// Composite primary key `(ns, entity_id, sub_key)` for the `entity_kv` table.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct EntityKvKey {
    pub ns: Ns,
    pub entity_id: i64,
    pub sub_key: String,
}

impl EntityKvKey {
    #[inline]
    pub fn new(ns: Ns, entity_id: i64, sub_key: impl Into<String>) -> Self {
        Self {
            ns,
            entity_id,
            sub_key: sub_key.into(),
        }
    }

    /// Encodes `(ns, entity_id, sub_key)` into a canonical string key for JSON map serialization.
    pub fn to_map_key_string(&self) -> String {
        format!("{}|{}|{}", self.ns.as_str(), self.entity_id, self.sub_key)
    }

    /// Parses a canonical string key produced by [`Self::to_map_key_string`].
    pub fn from_map_key_str(s: &str) -> Result<Self, String> {
        let mut parts = s.splitn(3, '|');
        let ns_str = parts
            .next()
            .ok_or_else(|| format!("invalid entity_kv key '{s}': missing namespace"))?;
        let id_str = parts
            .next()
            .ok_or_else(|| format!("invalid entity_kv key '{s}': missing entity_id"))?;
        let sub_key = parts
            .next()
            .ok_or_else(|| format!("invalid entity_kv key '{s}': missing sub_key"))?;
        let ns = Ns::from_str_name(ns_str)
            .ok_or_else(|| format!("unknown entity_kv namespace '{ns_str}'"))?;
        let entity_id = id_str
            .parse::<i64>()
            .map_err(|e| format!("invalid entity_kv entity_id '{id_str}': {e}"))?;
        Ok(Self {
            ns,
            entity_id,
            sub_key: sub_key.to_string(),
        })
    }
}

/// Value and metadata for a single row in `entity_kv`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntityKvRow {
    pub value_json: String,
    #[serde(default)]
    pub updated_unix: u64,
}

/// Serde adapter that serializes `BTreeMap<EntityKvKey, EntityKvRow>` with string map keys so
/// `serde_json::to_value` and `WorldStateSnapshot` JSON serialization work seamlessly.
pub mod entity_kv_map_serde {
    use super::{EntityKvKey, EntityKvRow};
    use serde::de::{MapAccess, Visitor};
    use serde::ser::SerializeMap;
    use serde::{Deserialize, Deserializer, Serializer};
    use std::collections::BTreeMap;
    use std::fmt;

    pub fn serialize<S>(
        map: &BTreeMap<EntityKvKey, EntityKvRow>,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut ser_map = serializer.serialize_map(Some(map.len()))?;
        for (k, v) in map {
            ser_map.serialize_entry(&k.to_map_key_string(), v)?;
        }
        ser_map.end()
    }

    pub fn deserialize<'de, D>(
        deserializer: D,
    ) -> Result<BTreeMap<EntityKvKey, EntityKvRow>, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct KvMapVisitor;
        impl<'de> Visitor<'de> for KvMapVisitor {
            type Value = BTreeMap<EntityKvKey, EntityKvRow>;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a map of 'ns|entity_id|sub_key' strings to EntityKvRow")
            }

            fn visit_map<M>(self, mut access: M) -> Result<Self::Value, M::Error>
            where
                M: MapAccess<'de>,
            {
                let mut out = BTreeMap::new();
                while let Some((k_str, row)) = access.next_entry::<String, EntityKvRow>()? {
                    let key = EntityKvKey::from_map_key_str(&k_str)
                        .map_err(serde::de::Error::custom)?;
                    out.insert(key, row);
                }
                Ok(out)
            }
        }

        deserializer.deserialize_map(KvMapVisitor)
    }
}

/// Validates a serialized JSON payload against the namespace's per-row byte budget and JSON syntax.
pub fn validate_entity_kv_value(ns: Ns, value_json: &str) -> Result<(), String> {
    let max_bytes = ns.max_value_bytes();
    if value_json.len() > max_bytes {
        return Err(format!(
            "entity_kv namespace '{}' value length {} exceeds per-row limit {}",
            ns.as_str(),
            value_json.len(),
            max_bytes
        ));
    }
    serde_json::from_str::<serde_json::Value>(value_json).map_err(|e| {
        format!(
            "entity_kv namespace '{}' value is not valid JSON: {e}",
            ns.as_str()
        )
    })?;
    Ok(())
}

/// Read-only typed handle to a single [`Ns`] namespace on [`WorldStateSnapshot`].
pub struct EntityKvReader<'a> {
    snap: &'a WorldStateSnapshot,
    ns: Ns,
}

impl<'a> EntityKvReader<'a> {
    #[inline]
    pub const fn new(snap: &'a WorldStateSnapshot, ns: Ns) -> Self {
        Self { snap, ns }
    }

    /// Looks up the raw JSON string stored at `(ns, entity_id, sub_key)`.
    pub fn get_raw(&self, entity_id: i64, sub_key: &str) -> Option<&'a str> {
        let key = EntityKvKey {
            ns: self.ns,
            entity_id,
            sub_key: sub_key.to_string(),
        };
        self.snap.entity_kv.get(&key).map(|r| r.value_json.as_str())
    }

    /// Looks up the full [`EntityKvRow`] stored at `(ns, entity_id, sub_key)`.
    pub fn get_row(&self, entity_id: i64, sub_key: &str) -> Option<&'a EntityKvRow> {
        let key = EntityKvKey {
            ns: self.ns,
            entity_id,
            sub_key: sub_key.to_string(),
        };
        self.snap.entity_kv.get(&key)
    }

    /// Deserializes the typed value `T` stored at `(ns, entity_id, sub_key)`.
    pub fn get<T: DeserializeOwned>(&self, entity_id: i64, sub_key: &str) -> Option<T> {
        let raw = self.get_raw(entity_id, sub_key)?;
        serde_json::from_str::<T>(raw).ok()
    }

    /// Returns `true` if a row exists at `(ns, entity_id, sub_key)`.
    pub fn contains(&self, entity_id: i64, sub_key: &str) -> bool {
        let key = EntityKvKey {
            ns: self.ns,
            entity_id,
            sub_key: sub_key.to_string(),
        };
        self.snap.entity_kv.contains_key(&key)
    }

    /// Iterates over all `(sub_key, row)` entries for `entity_id` within this namespace.
    pub fn iter_entity(
        &self,
        entity_id: i64,
    ) -> impl Iterator<Item = (&'a str, &'a EntityKvRow)> {
        let start = EntityKvKey {
            ns: self.ns,
            entity_id,
            sub_key: String::new(),
        };
        let ns = self.ns;
        self.snap
            .entity_kv
            .range(start..)
            .take_while(move |(k, _)| k.ns == ns && k.entity_id == entity_id)
            .map(|(k, v)| (k.sub_key.as_str(), v))
    }

    /// Iterates over all `(key, row)` entries in this namespace across all entities.
    pub fn iter_ns(&self) -> impl Iterator<Item = (&'a EntityKvKey, &'a EntityKvRow)> {
        let start = EntityKvKey {
            ns: self.ns,
            entity_id: i64::MIN,
            sub_key: String::new(),
        };
        let ns = self.ns;
        self.snap
            .entity_kv
            .range(start..)
            .take_while(move |(k, _)| k.ns == ns)
    }
}

/// Mutable typed handle to a single [`Ns`] namespace on [`WorldStateSnapshot`].
pub struct EntityKvWriter<'a> {
    snap: &'a mut WorldStateSnapshot,
    ns: Ns,
}

impl<'a> EntityKvWriter<'a> {
    #[inline]
    pub fn new(snap: &'a mut WorldStateSnapshot, ns: Ns) -> Self {
        snap.dirty_hints.other_domains_dirty = true;
        Self { snap, ns }
    }

    fn default_now_unix(&self) -> u64 {
        self.snap
            .service_state_blobs
            .get("__server_now_unix")
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0)
    }

    /// Serializes `value` as checked JSON (rejecting `NaN`/`Infinity` and enforcing
    /// [`Ns::max_value_bytes`]) and stores it at `(ns, entity_id, sub_key)`.
    pub fn set<T: Serialize>(
        &mut self,
        entity_id: i64,
        sub_key: impl Into<String>,
        value: &T,
    ) -> Result<(), String> {
        let now_unix = self.default_now_unix();
        self.set_with_unix(entity_id, sub_key, value, now_unix)
    }

    /// Serializes `value` as checked JSON with an explicit `updated_unix` timestamp.
    pub fn set_with_unix<T: Serialize>(
        &mut self,
        entity_id: i64,
        sub_key: impl Into<String>,
        value: &T,
        updated_unix: u64,
    ) -> Result<(), String> {
        let value_json = crate::sqlite_store::fingerprints::to_checked_json_string(value)?;
        self.set_raw(entity_id, sub_key, value_json, updated_unix)
    }

    /// Stores a pre-serialized JSON string at `(ns, entity_id, sub_key)` after validating JSON
    /// syntax and the namespace byte limit.
    pub fn set_raw(
        &mut self,
        entity_id: i64,
        sub_key: impl Into<String>,
        value_json: String,
        updated_unix: u64,
    ) -> Result<(), String> {
        validate_entity_kv_value(self.ns, &value_json)?;
        let key = EntityKvKey {
            ns: self.ns,
            entity_id,
            sub_key: sub_key.into(),
        };
        self.snap.entity_kv.insert(
            key,
            EntityKvRow {
                value_json,
                updated_unix,
            },
        );
        Ok(())
    }

    /// Removes the row at `(ns, entity_id, sub_key)`, returning the previous [`EntityKvRow`] if present.
    pub fn remove(&mut self, entity_id: i64, sub_key: &str) -> Option<EntityKvRow> {
        let key = EntityKvKey {
            ns: self.ns,
            entity_id,
            sub_key: sub_key.to_string(),
        };
        self.snap.entity_kv.remove(&key)
    }

    /// Removes all rows for `entity_id` within this namespace, returning the number of removed rows.
    pub fn clear_entity(&mut self, entity_id: i64) -> usize {
        let start = EntityKvKey {
            ns: self.ns,
            entity_id,
            sub_key: String::new(),
        };
        let ns = self.ns;
        let keys: Vec<EntityKvKey> = self
            .snap
            .entity_kv
            .range(start..)
            .take_while(|(k, _)| k.ns == ns && k.entity_id == entity_id)
            .map(|(k, _)| k.clone())
            .collect();
        let count = keys.len();
        for k in keys {
            self.snap.entity_kv.remove(&k);
        }
        count
    }

    /// Removes all rows across all entities within this namespace.
    pub fn clear_ns(&mut self) -> usize {
        let start = EntityKvKey {
            ns: self.ns,
            entity_id: i64::MIN,
            sub_key: String::new(),
        };
        let ns = self.ns;
        let keys: Vec<EntityKvKey> = self
            .snap
            .entity_kv
            .range(start..)
            .take_while(|(k, _)| k.ns == ns)
            .map(|(k, _)| k.clone())
            .collect();
        let count = keys.len();
        for k in keys {
            self.snap.entity_kv.remove(&k);
        }
        count
    }
}

/// Cascade-deletes all `entity_kv` rows belonging to `(scope, entity_id)` across all namespaces
/// whose [`Ns::cascade_on_owner_delete`] is `true`.
pub fn cascade_delete_entity_kv(
    snap: &mut WorldStateSnapshot,
    scope: EntityKvScope,
    entity_id: i64,
) -> usize {
    snap.dirty_hints.other_domains_dirty = true;
    let mut removed = 0usize;
    for &ns in Ns::ALL {
        if ns.scope() == scope && ns.cascade_on_owner_delete() {
            removed += EntityKvWriter::new(snap, ns).clear_entity(entity_id);
        }
    }
    removed
}
