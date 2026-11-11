//! Object caching (`carbon.common.script.net.objectCaching.CachedObject`,
//! `carbon.common.script.net.cachedObject.CachedObject`,
//! `carbon.common.script.net.objectCaching.CachedMethodCallResult`)
//! and binary-marshaled `MachoPacket` client notification routing (`P-3`, `P-5`).

#![forbid(unsafe_code)]

use crate::router::context::ServerContext;
use eve_machonet::{MachoAddress, MachoPacket, MachoPacketType};
use eve_proto::marshal::{
    self, V24_CACHED_METHOD_CALL_RESULT_CLASS, V24_CACHED_OBJECT_CLASS, V24_CACHE_HINT_CLASS,
};
use eve_proto::PyRep;
use std::collections::HashMap;

/// Standard IEEE 802.3 CRC-32 (`0xEDB8_8320`) matching `CRC32::Generate` in `CachedObjectMgr.cpp:238`.
pub fn crc32_ieee(bytes: &[u8]) -> i32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &b in bytes {
        crc ^= b as u32;
        for _ in 0..8 {
            if (crc & 1) != 0 {
                crc = (crc >> 1) ^ 0xEDB8_8320;
            } else {
                crc >>= 1;
            }
        }
    }
    (!crc) as i32
}

/// Produces a deterministic lookup key for any `objectID` `PyRep` (`CachedObjectMgr::OIDToString`).
pub fn cache_object_key(object_id: &PyRep) -> String {
    match object_id {
        PyRep::String(s) | PyRep::Token(s) => s.clone(),
        PyRep::Tuple(items) => {
            let parts: Vec<String> = items.iter().map(cache_object_key).collect();
            format!("({})", parts.join(","))
        }
        PyRep::Integer(i) => i.to_string(),
        other => serde_json::to_string(other).unwrap_or_else(|_| format!("{other:?}")),
    }
}

/// Stored server-side cache entry for `objectCaching` (`P-3`).
#[derive(Debug, Clone, PartialEq)]
pub struct CachedObjectRecord {
    pub object_id: PyRep,
    pub timestamp: i64,
    pub version: i32,
    pub node_id: i64,
    pub shared: bool,
    pub compressed: bool,
    pub marshaled_bytes: Vec<u8>,
    pub payload: PyRep,
}

impl CachedObjectRecord {
    /// Encodes as V24 `carbon.common.script.net.objectCaching.CachedObject` (`objectCaching.pyj:772`).
    pub fn to_cached_object_pyrep(&self) -> PyRep {
        PyRep::Object {
            type_name: V24_CACHED_OBJECT_CLASS.into(),
            args: Box::new(PyRep::Tuple(vec![
                PyRep::Tuple(vec![
                    PyRep::Integer(self.timestamp),
                    PyRep::Integer(self.version as i64),
                ]),
                PyRep::None,
                PyRep::Integer(self.node_id),
                PyRep::Integer(if self.shared { 1 } else { 0 }),
                PyRep::Buffer(self.marshaled_bytes.clone()),
                PyRep::Integer(if self.compressed { 1 } else { 0 }),
                self.object_id.clone(),
            ])),
        }
    }

    /// Encodes as V24 `carbon.common.script.net.cachedObject.CachedObject` (`cachedObject.pyj:12`).
    pub fn to_cache_hint_pyrep(&self) -> PyRep {
        let mut fields = vec![
            self.object_id.clone(),
            PyRep::Integer(self.node_id),
            PyRep::Tuple(vec![
                PyRep::Integer(self.timestamp),
                PyRep::Integer(self.version as i64),
            ]),
        ];
        if !self.shared {
            fields.push(PyRep::Integer(0));
        }
        PyRep::Object {
            type_name: V24_CACHE_HINT_CLASS.into(),
            args: Box::new(PyRep::Tuple(fields)),
        }
    }

    /// Encodes as an inline V24 `carbon.common.script.net.objectCaching.CachedMethodCallResult` (`objectCaching.pyj:837`).
    pub fn to_cached_method_call_result_pyrep(
        &self,
        client_when: &str,
        session_info: Option<&str>,
    ) -> PyRep {
        let mut details = vec![(
            PyRep::String("versionCheck".into()),
            PyRep::Tuple(vec![
                PyRep::String(client_when.into()),
                PyRep::None,
                PyRep::None,
            ]),
        )];
        if let Some(si) = session_info {
            details.push((PyRep::String("sessionInfo".into()), PyRep::String(si.into())));
        }
        PyRep::Object {
            type_name: V24_CACHED_METHOD_CALL_RESULT_CLASS.into(),
            args: Box::new(PyRep::Tuple(vec![
                PyRep::Dict(details),
                PyRep::SubStream(Box::new(self.payload.clone())),
                PyRep::List(vec![
                    PyRep::Integer(self.timestamp),
                    PyRep::Integer(self.version as i64),
                ]),
            ])),
        }
    }

    /// Encodes as a hint-backed V24 `carbon.common.script.net.objectCaching.CachedMethodCallResult` (`objectCaching.pyj:837`).
    pub fn to_cached_method_call_hint_pyrep(&self, version_check: &str) -> PyRep {
        PyRep::Object {
            type_name: V24_CACHED_METHOD_CALL_RESULT_CLASS.into(),
            args: Box::new(PyRep::Tuple(vec![
                PyRep::Dict(vec![(
                    PyRep::String("versionCheck".into()),
                    PyRep::String(version_check.into()),
                )]),
                self.to_cache_hint_pyrep(),
                PyRep::None,
            ])),
        }
    }
}

/// Target scope for server-initiated client notifications (`P-5`, `EntityList::Multicast`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotificationTarget {
    Character(i64),
    Corporation(i64),
    Alliance(i64),
    Fleet(i64),
    SolarSystem(u32),
    Station(i64),
    All,
}

static NEXT_NOTIFICATION_SEQ_ID: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(1);

/// Returns a monotonically increasing sequence ID for [`PendingMachoNotification`].
pub fn next_notification_seq_id() -> u64 {
    NEXT_NOTIFICATION_SEQ_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// Queued binary-marshaled `MachoPacket` (`Notification` = `12`) ready for delivery (`P-5`).
#[derive(Debug, Clone)]
pub struct PendingMachoNotification {
    pub seq_id: u64,
    pub recipient_char_id: i64,
    pub event: String,
    pub id_type: String,
    pub args: Vec<PyRep>,
    pub packet: MachoPacket,
    pub marshaled_packet: Vec<u8>,
    pub created_filetime: i64,
}

impl PendingMachoNotification {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        recipient_char_id: i64,
        event: impl Into<String>,
        id_type: impl Into<String>,
        args: Vec<PyRep>,
        packet: MachoPacket,
        marshaled_packet: Vec<u8>,
        created_filetime: i64,
    ) -> Self {
        Self {
            seq_id: next_notification_seq_id(),
            recipient_char_id,
            event: event.into(),
            id_type: id_type.into(),
            args,
            packet,
            marshaled_packet,
            created_filetime,
        }
    }
}

/// Maximum queued real-time notifications per online character (`R51-P1-3`).
pub const MAX_PENDING_NOTIFICATIONS_PER_CHAR: usize = 128;

/// Hard ceiling for notifications emitted to the active RPC caller during a single in-flight RPC (`#850` / `B12`).
pub const MAX_ACTIVE_RPC_NOTIFICATIONS_PER_CHAR: usize = 4_096;

tokio::task_local! {
    static DISPATCH_ACTIVE_CHAR_ID: std::cell::Cell<Option<i64>>;
}

/// Returns `true` if `char_id` is the character executing the current in-flight `dispatch_rpc` call (`#850`).
pub fn is_active_dispatch_char(char_id: i64) -> bool {
    DISPATCH_ACTIVE_CHAR_ID
        .try_with(|c| c.get() == Some(char_id))
        .unwrap_or(false)
}

/// Updates the active character ID inside an in-flight `dispatch_rpc` scope (e.g. during `SelectCharacterID`).
pub fn set_active_dispatch_char(char_id: Option<i64>) {
    let _ = DISPATCH_ACTIVE_CHAR_ID.try_with(|c| c.set(char_id));
}

/// Runs `fut` with `char_id` registered as the active `dispatch_rpc` caller so bulk RPCs (`MultiAdd`, `FitMany`)
/// do not evict their own in-flight notifications at `MAX_PENDING_NOTIFICATIONS_PER_CHAR` (`#850` / `B12`).
pub async fn with_active_dispatch_char<F, R>(char_id: Option<i64>, fut: F) -> R
where
    F: std::future::Future<Output = R>,
{
    DISPATCH_ACTIVE_CHAR_ID
        .scope(std::cell::Cell::new(char_id), fut)
        .await
}

/// Pushes `notif` into `queue`, evicting the oldest entry if the per-character cap is reached.
/// When `notif.recipient_char_id` is the active `dispatch_rpc` caller (`#850`), all notifications emitted
/// by the in-flight RPC are preserved up to [`MAX_ACTIVE_RPC_NOTIFICATIONS_PER_CHAR`] until the post-RPC drain.
pub fn push_pending_notification_capped(
    queue: &mut Vec<PendingMachoNotification>,
    notif: PendingMachoNotification,
) {
    let cap = if is_active_dispatch_char(notif.recipient_char_id) {
        MAX_ACTIVE_RPC_NOTIFICATIONS_PER_CHAR
    } else {
        MAX_PENDING_NOTIFICATIONS_PER_CHAR
    };
    while queue.len() >= cap {
        queue.remove(0);
    }
    queue.push(notif);
}

/// Persistent notification inbox record for `notificationMgr` (`NotificationMgrService.cpp:66-90`).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct NotificationRecord {
    pub notification_id: i64,
    pub type_id: i32,
    pub group_id: i32,
    pub sender_id: i64,
    pub receiver_id: i64,
    pub processed: bool,
    pub created: i64,
    pub data: String,
    pub deleted: bool,
    pub interacted: bool,
}

/// Returns true if `flag` represents a corporation hangar (`CorpSAG1..7` = `116..=122`) or `CorpDeliveries` (`62`).
pub fn is_corp_hangar_flag(flag: eve_db::InventoryFlag) -> bool {
    matches!(flag as i64, 62 | 116..=122)
}

impl ServerContext {
    pub fn is_corp_hangar_flag(flag: eve_db::InventoryFlag) -> bool {
        is_corp_hangar_flag(flag)
    }
}

impl ServerContext {
    /// Builds the `("Method Call", "server", (service, method))` cache `objectID` tuple.
    pub fn method_call_object_id(service: &str, method: &str) -> PyRep {
        PyRep::Tuple(vec![
            PyRep::String("Method Call".into()),
            PyRep::String("server".into()),
            PyRep::Tuple(vec![
                PyRep::String(service.into()),
                PyRep::String(method.into()),
            ]),
        ])
    }

    /// Stores or updates a cached object in `self.object_cache`, computing its IEEE CRC32 version
    /// and advancing the BlueOS filetime timestamp monotonically even across invalidations (`R51-P1-4`).
    pub fn put_cached_object(
        &self,
        object_id: PyRep,
        payload: PyRep,
        shared: bool,
    ) -> CachedObjectRecord {
        let key = cache_object_key(&object_id);
        let marshaled_bytes = marshal::encode(&payload);
        let version = crc32_ieee(&marshaled_bytes);
        let now_ticks = self.now_blueos_filetime_ticks();
        let mut cache = self.object_cache.write().unwrap_or_else(|e| e.into_inner());
        let mut ts_map = self
            .object_cache_timestamps
            .write()
            .unwrap_or_else(|e| e.into_inner());
        let prev_ts = cache
            .get(&key)
            .map(|prev| prev.timestamp)
            .or_else(|| ts_map.get(&key).copied());
        let timestamp = prev_ts
            .map(|pts| now_ticks.max(pts + 1))
            .unwrap_or(now_ticks);
        ts_map.insert(key.clone(), timestamp);
        let compressed = marshaled_bytes.first().copied() == Some(eve_proto::ZLIB_HEADER_BYTE);
        let rec = CachedObjectRecord {
            object_id,
            timestamp,
            version,
            node_id: 888_444,
            shared,
            compressed,
            marshaled_bytes,
            payload,
        };
        cache.insert(key, rec.clone());
        rec
    }

    /// Invalidates a cached object by its `objectID`, returning `true` if an entry was removed.
    pub fn invalidate_cached_object(&self, object_id: &PyRep) -> bool {
        let key = cache_object_key(object_id);
        let mut cache = self.object_cache.write().unwrap_or_else(|e| e.into_inner());
        if let Some(removed) = cache.remove(&key) {
            self.object_cache_timestamps
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .entry(key)
                .and_modify(|ts| *ts = (*ts).max(removed.timestamp))
                .or_insert(removed.timestamp);
            true
        } else {
            false
        }
    }

    /// Invalidates a cached method call `("Method Call", "server", (service, method))`.
    pub fn invalidate_cached_method(&self, service: &str, method: &str) -> bool {
        let oid = Self::method_call_object_id(service, method);
        self.invalidate_cached_object(&oid)
    }

    /// Wraps a service method result in V24 `carbon.common.script.net.objectCaching.CachedMethodCallResult`
    /// and registers it in `self.object_cache`.
    pub fn cache_method_call_result(
        &self,
        service: &str,
        method: &str,
        payload: PyRep,
        client_when: &str,
        session_info: Option<&str>,
    ) -> PyRep {
        let oid = Self::method_call_object_id(service, method);
        let rec = self.put_cached_object(oid, payload, true);
        rec.to_cached_method_call_result_pyrep(client_when, session_info)
    }

    /// Retrieves a cached object or lazily populates backed SDE/DB cache objects (`R51-P1-1`).
    pub fn get_or_populate_cached_object(
        &self,
        object_id: &PyRep,
        shared: bool,
    ) -> Option<CachedObjectRecord> {
        let key = cache_object_key(object_id);
        if let Some(existing) = self
            .object_cache
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
            .cloned()
        {
            return Some(existing);
        }

        let synthesized = match key.as_str() {
            "(Method Call,server,(config,GetUnits))"
            | "config.Units"
            | "config.BulkData.units" => Some(self.build_config_units_payload()),
            "(Method Call,server,(marketProxy,GetMarketGroups))" | "config.MarketGroups" => {
                let mut mgs: Vec<&eve_sde::MarketGroupInfo> =
                    self.sde.market_groups.values().collect();
                mgs.sort_by_key(|mg| mg.market_group_id);
                let rows = mgs
                    .into_iter()
                    .map(|mg| {
                        vec![
                            PyRep::Integer(mg.market_group_id as i64),
                            mg.parent_group_id
                                .map(|pid| PyRep::Integer(pid as i64))
                                .unwrap_or(PyRep::None),
                            PyRep::String(mg.name.clone()),
                            PyRep::String(mg.name.clone()),
                            mg.icon_id
                                .map(|iid| PyRep::Integer(iid as i64))
                                .unwrap_or(PyRep::None),
                            PyRep::Integer(i64::from(mg.has_types)),
                        ]
                    })
                    .collect();
                Some(PyRep::rowset(
                    vec![
                        "marketGroupID",
                        "parentGroupID",
                        "marketGroupName",
                        "description",
                        "iconID",
                        "hasTypes",
                    ],
                    rows,
                ))
            }
            "config.Groups" => {
                let mut grps: Vec<&eve_sde::GroupInfo> = self.sde.groups.values().collect();
                grps.sort_by_key(|g| g.group_id);
                let rows = grps
                    .into_iter()
                    .map(|g| {
                        vec![
                            PyRep::Integer(g.group_id as i64),
                            PyRep::Integer(g.category_id as i64),
                            PyRep::String(g.name.clone()),
                            PyRep::None,
                            PyRep::Integer(i64::from(g.use_base_price)),
                            PyRep::Integer(i64::from(g.anchored)),
                            PyRep::Integer(i64::from(g.anchorable)),
                            PyRep::Integer(i64::from(g.fittable_non_singleton)),
                            PyRep::Integer(i64::from(g.published)),
                        ]
                    })
                    .collect();
                Some(PyRep::rowset(
                    vec![
                        "groupID",
                        "categoryID",
                        "groupName",
                        "iconID",
                        "useBasePrice",
                        "anchored",
                        "anchorable",
                        "fittableNonSingleton",
                        "published",
                    ],
                    rows,
                ))
            }
            "config.Categories" => {
                let mut cats: Vec<&eve_sde::CategoryInfo> = self.sde.categories.values().collect();
                cats.sort_by_key(|c| c.category_id);
                let rows = cats
                    .into_iter()
                    .map(|c| {
                        vec![
                            PyRep::Integer(c.category_id as i64),
                            PyRep::String(c.name.clone()),
                            PyRep::None,
                            PyRep::Integer(i64::from(c.published)),
                        ]
                    })
                    .collect();
                Some(PyRep::rowset(
                    vec!["categoryID", "categoryName", "iconID", "published"],
                    rows,
                ))
            }
            "config.StaticOwners" => {
                let mut rows = Vec::new();
                let mut fac_ids: Vec<u32> = self.sde.factions.keys().copied().collect();
                fac_ids.sort_unstable();
                for fid in fac_ids {
                    if let Some(fac) = self.sde.factions.get(&fid) {
                        rows.push(vec![
                            PyRep::Integer(fid as i64),
                            PyRep::String(fac.name.clone()),
                            PyRep::Integer(30),
                        ]);
                    }
                }
                let mut corp_ids: Vec<i64> = self.sde.npc_corporations.keys().copied().collect();
                corp_ids.sort_unstable();
                for cid in corp_ids {
                    if let Some(corp) = self.sde.npc_corporations.get(&cid) {
                        rows.push(vec![
                            PyRep::Integer(cid),
                            PyRep::String(corp.name.clone()),
                            PyRep::Integer(2),
                        ]);
                    }
                }
                Some(PyRep::rowset(
                    vec!["ownerID", "ownerName", "typeID"],
                    rows,
                ))
            }
            "(Method Call,server,(stationSvc,GetAllianceSystems))" => {
                let rows = self.db.with_snapshot(|snap| {
                    let mut out = Vec::new();
                    let mut sys_ids: Vec<u32> = snap.system_states.keys().copied().collect();
                    sys_ids.sort_unstable();
                    for sid in sys_ids {
                        if let Some(st) = snap.system_states.get(&sid) {
                            if let Some(aid) = st.sov_alliance_id {
                                out.push(vec![
                                    PyRep::Integer(sid as i64),
                                    PyRep::Integer(aid),
                                ]);
                            }
                        }
                    }
                    out
                });
                Some(PyRep::rowset(vec!["solarSystemID", "allianceID"], rows))
            }
            "config.Races" | "charCreationInfo.races" => {
                let mut races: Vec<&eve_sde::RaceInfo> = self.sde.races.values().collect();
                races.sort_by_key(|r| r.race_id);
                let rows = races
                    .into_iter()
                    .map(|r| {
                        vec![
                            PyRep::Integer(r.race_id as i64),
                            PyRep::String(r.name.clone()),
                            PyRep::String(r.name.clone()),
                            PyRep::None,
                            PyRep::String(r.name.clone()),
                        ]
                    })
                    .collect();
                Some(PyRep::rowset(
                    vec!["raceID", "raceName", "description", "iconID", "shortDescription"],
                    rows,
                ))
            }
            "config.Bloodlines" | "charCreationInfo.bloodlines" => {
                let mut bls: Vec<&eve_sde::BloodlineInfo> = self.sde.bloodlines.values().collect();
                bls.sort_by_key(|b| b.bloodline_id);
                let rows = bls
                    .into_iter()
                    .map(|b| {
                        vec![
                            PyRep::Integer(b.bloodline_id as i64),
                            PyRep::String(b.name.clone()),
                            PyRep::Integer(b.race_id as i64),
                            PyRep::Integer(b.corporation_id),
                        ]
                    })
                    .collect();
                Some(PyRep::rowset(
                    vec!["bloodlineID", "bloodlineName", "raceID", "corporationID"],
                    rows,
                ))
            }
            "config.Ancestries" | "charCreationInfo.ancestries" => {
                let mut ancs: Vec<&eve_sde::AncestryInfo> = self.sde.ancestries.values().collect();
                ancs.sort_by_key(|a| a.ancestry_id);
                let rows = ancs
                    .into_iter()
                    .map(|a| {
                        vec![
                            PyRep::Integer(a.ancestry_id as i64),
                            PyRep::String(a.name.clone()),
                            PyRep::Integer(a.bloodline_id as i64),
                        ]
                    })
                    .collect();
                Some(PyRep::rowset(
                    vec!["ancestryID", "ancestryName", "bloodlineID"],
                    rows,
                ))
            }
            "config.Schools" | "charCreationInfo.schools" => {
                let mut schs: Vec<&eve_sde::SchoolInfo> = self.sde.schools.values().collect();
                schs.sort_by_key(|s| s.school_id);
                let rows = schs
                    .into_iter()
                    .map(|s| {
                        vec![
                            PyRep::Integer(s.school_id as i64),
                            PyRep::String(s.name.clone()),
                            PyRep::Integer(s.race_id as i64),
                            PyRep::Integer(s.corporation_id),
                            PyRep::Integer(s.station_id),
                            PyRep::Integer(s.solar_system_id as i64),
                        ]
                    })
                    .collect();
                Some(PyRep::rowset(
                    vec![
                        "schoolID",
                        "schoolName",
                        "raceID",
                        "corporationID",
                        "stationID",
                        "solarSystemID",
                    ],
                    rows,
                ))
            }
            _ => None,
        }?;

        Some(self.put_cached_object(object_id.clone(), synthesized, shared))
    }

    /// Builds the `config.GetUnits` `util.IndexRowset` keyed by `unitID` (`ConfigDB.cpp:332-341`).
    pub fn build_config_units_payload(&self) -> PyRep {
        let units: &[(i64, &str, &str)] = &[
            (1, "Length", "m"),
            (2, "Mass", "kg"),
            (3, "Time", "sec"),
            (4, "Electric Current", "A"),
            (5, "Temperature", "K"),
            (9, "Volume", "m3"),
            (10, "Speed", "m/sec"),
            (101, "Milliseconds", "s"),
            (102, "Millimeters", "mm"),
            (104, "Multiplier", "x"),
            (105, "Percentage", "%"),
            (106, "Teraflops", "tf"),
            (107, "MegaWatts", "MW"),
            (108, "Inverse Absolute Percent", "%"),
            (109, "Modifier Percent", "%"),
            (111, "Inversed Modifier Percent", "%"),
            (113, "Hitpoints", "HP"),
            (114, "capacitor units", "GJ"),
            (115, "groupID", "groupID"),
            (116, "typeID", "typeID"),
            (117, "Sizeclass", "1=small 2=medium 3=l"),
            (119, "attributeID", "attributeID"),
            (121, "realPercent", "%"),
            (122, "Fitting slots", ""),
            (133, "ISK", "ISK"),
            (134, "m3/hour", "m3/h"),
            (139, "Bonus", "+"),
            (140, "Level", "Level"),
            (141, "Hardpoints", "hardpoints"),
        ];
        let items = units
            .iter()
            .map(|&(id, name, disp)| {
                (
                    PyRep::Integer(id),
                    vec![
                        PyRep::Integer(id),
                        PyRep::String(name.into()),
                        PyRep::String(disp.into()),
                    ],
                )
            })
            .collect();
        PyRep::index_rowset("unitID", vec!["unitID", "unitName", "displayName"], items)
    }

    /// Returns `true` if `character_id` (owned by `user_id`) currently has an active online session (`R51-P1-3`).
    pub fn is_character_online(&self, character_id: i64, user_id: u64) -> bool {
        self.db
            .get_online_session(user_id)
            .is_some_and(|os| os.character_id == Some(character_id))
    }

    /// Resolves online target `(character_id, user_id)` pairs from a [`NotificationTarget`] (`R51-P1-3`).
    pub fn resolve_notification_recipients(
        &self,
        target: &NotificationTarget,
    ) -> Vec<(i64, Option<u64>)> {
        let candidates: Vec<(i64, u64)> = self.db.with_snapshot(|snap| {
            let mut out = Vec::new();
            match target {
                NotificationTarget::Character(cid) => {
                    if let Some(c) = snap.characters.get(cid) {
                        out.push((*cid, c.user_id));
                    }
                }
                NotificationTarget::Corporation(corp_id) => {
                    for c in snap.characters.values() {
                        if c.corporation_id == *corp_id {
                            out.push((c.character_id, c.user_id));
                        }
                    }
                }
                NotificationTarget::Alliance(ally_id) => {
                    for c in snap.characters.values() {
                        if c.alliance_id == Some(*ally_id) {
                            out.push((c.character_id, c.user_id));
                        }
                    }
                }
                NotificationTarget::Fleet(fleet_id) => {
                    if let Some(f) = snap.fleets.get(fleet_id) {
                        for &cid in &f.members {
                            if let Some(c) = snap.characters.get(&cid) {
                                out.push((cid, c.user_id));
                            }
                        }
                    }
                }
                NotificationTarget::SolarSystem(sys_id) => {
                    for c in snap.characters.values() {
                        if c.solar_system_id == *sys_id {
                            out.push((c.character_id, c.user_id));
                        }
                    }
                }
                NotificationTarget::Station(station_id) => {
                    for c in snap.characters.values() {
                        if c.station_id == Some(*station_id)
                            || c.structure_id == Some(*station_id)
                        {
                            out.push((c.character_id, c.user_id));
                        }
                    }
                }
                NotificationTarget::All => {
                    for c in snap.characters.values() {
                        out.push((c.character_id, c.user_id));
                    }
                }
            }
            out
        });
        let mut out: Vec<(i64, Option<u64>)> = candidates
            .into_iter()
            .filter(|&(cid, uid)| self.is_character_online(cid, uid))
            .map(|(cid, uid)| (cid, Some(uid)))
            .collect();
        out.sort_by_key(|(cid, _)| *cid);
        out.dedup_by_key(|(cid, _)| *cid);
        out
    }

    /// Constructs a `MachoPacket` (`Notification` = `12`) and encodes it into binary `eveMarshal` wire bytes.
    pub fn build_macho_notification_packet(
        recipient_char_id: i64,
        user_id: Option<u64>,
        event: &str,
        id_type: &str,
        args: Vec<PyRep>,
    ) -> (MachoPacket, Vec<u8>) {
        let stream_payload = PyRep::Tuple(vec![PyRep::Tuple(vec![
            PyRep::Integer(0),
            PyRep::SubStream(Box::new(PyRep::Tuple(vec![
                PyRep::Integer(0),
                PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Tuple(args)]),
            ]))),
        ])]);
        let pkt = MachoPacket {
            packet_type: MachoPacketType::Notification,
            source: MachoAddress::Node {
                node_id: 888_444,
                service: None,
                call_id: 0,
            },
            destination: MachoAddress::Broadcast {
                broadcast_id: event.to_string(),
                id_type: id_type.to_string(),
                narrowcast: vec![recipient_char_id.max(0) as u64],
            },
            user_id,
            payload: stream_payload,
            named_payload: HashMap::new(),
        };
        let marshaled = marshal::encode(&pkt.to_pyrep());
        (pkt, marshaled)
    }

    /// Constructs a `MachoPacket` (`CallRsp` = `7`) and encodes it into binary `eveMarshal` wire bytes (`Z-1`).
    pub fn build_macho_call_response_packet(
        source_service: Option<&str>,
        dest_client_id: u64,
        call_id: u64,
        user_id: Option<u64>,
        value: PyRep,
    ) -> (MachoPacket, Vec<u8>) {
        let pkt = MachoPacket {
            packet_type: MachoPacketType::CallRsp,
            source: MachoAddress::Node {
                node_id: 888_444,
                service: source_service.map(str::to_string),
                call_id,
            },
            destination: MachoAddress::Client {
                client_id: dest_client_id,
                call_id,
                service: None,
            },
            user_id,
            payload: PyRep::Tuple(vec![PyRep::SubStream(Box::new(value))]),
            named_payload: HashMap::new(),
        };
        let marshaled = marshal::encode(&pkt.to_pyrep());
        (pkt, marshaled)
    }

    /// Constructs a `MachoPacket` (`ErrorResponse` = `15`) and encodes it into binary `eveMarshal` wire bytes (`Z-1`).
    pub fn build_macho_error_response_packet(
        source_service: Option<&str>,
        dest_client_id: u64,
        call_id: u64,
        user_id: Option<u64>,
        error_code: &str,
        error_message: &str,
    ) -> (MachoPacket, Vec<u8>) {
        let pkt = MachoPacket {
            packet_type: MachoPacketType::ErrorResponse,
            source: MachoAddress::Node {
                node_id: 888_444,
                service: source_service.map(str::to_string),
                call_id,
            },
            destination: MachoAddress::Client {
                client_id: dest_client_id,
                call_id,
                service: None,
            },
            user_id,
            payload: PyRep::Tuple(vec![
                PyRep::Integer(MachoPacketType::CallReq as i64),
                PyRep::Integer(1),
                PyRep::Tuple(vec![PyRep::SubStream(Box::new(PyRep::keyval([
                    ("code", PyRep::String(error_code.to_string())),
                    ("message", PyRep::String(error_message.to_string())),
                ])))]),
            ]),
            named_payload: HashMap::new(),
        };
        let marshaled = marshal::encode(&pkt.to_pyrep());
        (pkt, marshaled)
    }

    /// Emits a binary-marshaled `MachoPacket` notification to all online recipients matching `target` (`P-5`, `R51-P1-3`).
    pub fn notify(
        &self,
        target: NotificationTarget,
        event: &str,
        id_type: &str,
        args: Vec<PyRep>,
    ) -> Vec<PendingMachoNotification> {
        let recipients = self.resolve_notification_recipients(&target);
        let now_ft = self.now_blueos_filetime_ticks();
        let mut emitted = Vec::with_capacity(recipients.len());
        let mut queues = self
            .pending_notifications
            .write()
            .unwrap_or_else(|e| e.into_inner());
        for (cid, uid) in recipients {
            let mut recipient_args = args.clone();
            if event == "OnModuleAttributeChanges" {
                recipient_args = crate::router::client_compat::normalize_macho_notification_with_sde(
                    event,
                    recipient_args,
                    Some(&self.sde),
                );
                if cid > 0 {
                    if let Some(PyRep::List(items)) = recipient_args.first_mut() {
                        for item in items.iter_mut() {
                            if let PyRep::Tuple(t) = item {
                                if matches!(t.get(1), Some(PyRep::None)) {
                                    t[1] = PyRep::Integer(cid);
                                }
                                if matches!(t.get(4), Some(PyRep::Integer(0))) {
                                    t[4] = PyRep::Integer(now_ft);
                                }
                            }
                        }
                    }
                }
            }
            let (packet, marshaled_packet) =
                Self::build_macho_notification_packet(cid, uid, event, id_type, recipient_args.clone());
            let notif = PendingMachoNotification::new(
                cid,
                event,
                id_type,
                recipient_args,
                packet,
                marshaled_packet,
                now_ft,
            );
            let q = queues.entry(cid).or_default();
            push_pending_notification_capped(q, notif.clone());
            let _ = self.notify_tx.send((cid, notif.clone()));
            emitted.push(notif);
        }
        emitted
    }

    /// Returns the highest queued `seq_id` for `char_id` (or `0` when empty).
    pub fn latest_notification_seq_for_char(&self, char_id: i64) -> u64 {
        self.pending_notifications
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&char_id)
            .and_then(|q| q.last().map(|n| n.seq_id))
            .unwrap_or(0)
    }

    /// Drains all pending notifications queued for `char_id`.
    pub fn drain_notifications_for_char(&self, char_id: i64) -> Vec<PendingMachoNotification> {
        self.pending_notifications
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&char_id)
            .unwrap_or_default()
    }

    /// Purges all pending notifications queued for `char_id` on logoff or character re-selection (`#881` / `C19`).
    pub fn clear_notifications_for_char(&self, char_id: i64) {
        self.pending_notifications
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&char_id);
    }

    /// Emits an `OnRemoteMessage` notification (`msg_key`, `dict`).
    pub fn notify_remote_message(
        &self,
        target: NotificationTarget,
        msg_key: &str,
        entries: Vec<(&str, PyRep)>,
    ) -> Vec<PendingMachoNotification> {
        let dict = PyRep::Dict(
            entries
                .into_iter()
                .map(|(k, v)| (PyRep::String(k.into()), v))
                .collect(),
        );
        self.notify(
            target,
            "OnRemoteMessage",
            "charid",
            vec![PyRep::String(msg_key.into()), dict],
        )
    }

    /// Emits an `OnAccountChange` notification (`accountKey`, `ownerID`, `balance`).
    pub fn notify_account_change(
        &self,
        char_id: i64,
        account_key: &str,
        owner_id: i64,
        new_balance_isk: f64,
    ) -> Vec<PendingMachoNotification> {
        self.notify(
            NotificationTarget::Character(char_id),
            "OnAccountChange",
            "charid",
            vec![
                PyRep::String(account_key.into()),
                PyRep::Integer(owner_id),
                PyRep::Real(new_balance_isk),
            ],
        )
    }

    /// Emits an `OnItemChange` notification (`itemRow` packed `DBRow`, `changeDict`).
    pub fn notify_item_change(
        &self,
        char_id: i64,
        item_row: PyRep,
        changes: Vec<(i32, PyRep)>,
    ) -> Vec<PendingMachoNotification> {
        let change_dict = PyRep::Dict(
            changes
                .into_iter()
                .map(|(col_ix, old_val)| (PyRep::Integer(col_ix as i64), old_val))
                .collect(),
        );
        self.notify(
            NotificationTarget::Character(char_id),
            "OnItemChange",
            "charid",
            vec![item_row, change_dict],
        )
    }

    /// Emits an `OnSkillTrained` notification (`skillItemID`, `skillTypeID`, `level`).
    pub fn notify_skill_trained(
        &self,
        char_id: i64,
        skill_item_id: i64,
        skill_type_id: u32,
        level: u8,
    ) -> Vec<PendingMachoNotification> {
        self.notify(
            NotificationTarget::Character(char_id),
            "OnSkillTrained",
            "charid",
            vec![
                PyRep::Integer(skill_item_id),
                PyRep::Integer(skill_type_id as i64),
                PyRep::Integer(level as i64),
            ],
        )
    }

    /// Emits an `OnSkillQueuePaused` notification to character (`feat:skills.queue-pause`).
    pub fn notify_skill_queue_paused(&self, char_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(
            NotificationTarget::Character(char_id),
            "OnSkillQueuePaused",
            "charid",
            vec![],
        )
    }

    /// Emits an `OnSkillQueueChanged` notification to character.
    pub fn notify_skill_queue_changed(&self, char_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(
            NotificationTarget::Character(char_id),
            "OnSkillQueueChanged",
            "charid",
            vec![],
        )
    }

    /// Creates a persistent notification in `self.notification_records` and pushes an `OnNotificationReceived`
    /// `MachoPacket` notification to `receiver_id`.
    pub fn create_character_notification(
        &self,
        sender_id: i64,
        receiver_id: i64,
        type_id: i32,
        group_id: i32,
        data: &str,
    ) -> NotificationRecord {
        let max_in_mem = self
            .notification_records
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|r| r.notification_id)
            .max()
            .unwrap_or(0);
        let notification_id = self
            .db
            .with_mut_snapshot(|snap| snap.allocate_notification_id_above(max_in_mem));
        let created = self.now_blueos_filetime_ticks();
        let rec = NotificationRecord {
            notification_id,
            type_id,
            group_id,
            sender_id,
            receiver_id,
            processed: false,
            created,
            data: data.to_string(),
            deleted: false,
            interacted: false,
        };
        self.notification_records
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .push(rec.clone());
        self.persist_notification_record(&rec);
        let _ = self.notify(
            NotificationTarget::Character(receiver_id),
            "OnNotificationReceived",
            "charid",
            vec![
                PyRep::Integer(notification_id),
                PyRep::Integer(type_id as i64),
                PyRep::Integer(sender_id),
                PyRep::Integer(created),
                PyRep::String(data.to_string()),
            ],
        );
        rec
    }

    pub fn persist_notification_record(&self, rec: &NotificationRecord) {
        let now = self.now_unix();
        if let Ok(json) = serde_json::to_string(rec) {
            let note_id = -(7_900_000_000_000 + rec.notification_id);
            self.db.with_mut_snapshot(|snap| {
                snap.owner_notes.insert(note_id, eve_db::OwnerNoteRecord {
                    note_id, character_id: -79, item_id: rec.receiver_id,
                    label: format!("notif:{}", rec.notification_id), note: json, timestamp_unix: now,
                });
            });
        }
    }

    pub fn sync_character_notifications_to_db(&self, char_id: i64) {
        let now = self.now_unix();
        let char_recs: Vec<NotificationRecord> = self.notification_records.read().unwrap_or_else(|e| e.into_inner())
            .iter().filter(|r| r.receiver_id == char_id).cloned().collect();
        self.db.with_mut_snapshot(|snap| {
            for rec in &char_recs {
                if let Ok(json) = serde_json::to_string(rec) {
                    let note_id = -(7_900_000_000_000 + rec.notification_id);
                    snap.owner_notes.insert(note_id, eve_db::OwnerNoteRecord {
                        note_id, character_id: -79, item_id: rec.receiver_id,
                        label: format!("notif:{}", rec.notification_id), note: json, timestamp_unix: now,
                    });
                }
            }
        });
    }

    pub fn deliver_unprocessed_notifications_on_login(&self, char_id: i64) {
        let user_id = self.db.with_snapshot(|s| s.characters.get(&char_id).map(|c| c.user_id));
        let unprocessed: Vec<NotificationRecord> = self.notification_records.read().unwrap_or_else(|e| e.into_inner())
            .iter().filter(|r| r.receiver_id == char_id && !r.deleted && !r.processed).cloned().collect();
        if unprocessed.is_empty() { return; }
        let now_ft = self.now_blueos_filetime_ticks();
        let mut queues = self.pending_notifications.write().unwrap_or_else(|e| e.into_inner());
        let q = queues.entry(char_id).or_default();
        for rec in unprocessed {
            if q.iter().any(|n| n.event == "OnNotificationReceived" && n.args.first() == Some(&PyRep::Integer(rec.notification_id))) {
                continue;
            }
            let args = vec![
                PyRep::Integer(rec.notification_id), PyRep::Integer(rec.type_id as i64),
                PyRep::Integer(rec.sender_id), PyRep::Integer(rec.created), PyRep::String(rec.data.clone()),
            ];
            let (packet, marshaled_packet) = Self::build_macho_notification_packet(char_id, user_id, "OnNotificationReceived", "charid", args.clone());
            let notif = PendingMachoNotification::new(
                char_id,
                "OnNotificationReceived",
                "charid",
                args,
                packet,
                marshaled_packet,
                now_ft,
            );
            push_pending_notification_capped(q, notif.clone());
            let _ = self.notify_tx.send((char_id, notif));
        }
    }

    /// Emits an `OnDamageStateChange` notification (`itemID`, `(shield, armor, structure)`) (`Z-2`).
    pub fn notify_damage_state_change(
        &self,
        target: NotificationTarget,
        item_id: i64,
        shield: f64,
        armor: f64,
        structure: f64,
    ) -> Vec<PendingMachoNotification> {
        let dmg = crate::services::notify_emitters_space_dungeons::build_damage_state_tuple(
            self, item_id, shield, armor, structure,
        );
        self.notify(
            target,
            "OnDamageStateChange",
            "itemid",
            vec![PyRep::Integer(item_id), dmg],
        )
    }

    /// Emits an `OnGodmaShipEffect` notification for active module cycle events (`Z-2`).
    pub fn notify_godma_ship_effect(
        &self,
        target: NotificationTarget,
        item_id: i64,
        effect_id: i32,
        active: bool,
        duration_ms: i64,
        repeat: i32,
    ) -> Vec<PendingMachoNotification> {
        let now_ticks = self.now_blueos_filetime_ticks();
        let flag = if active { 1 } else { 0 };
        self.notify(
            target,
            "OnGodmaShipEffect",
            "charid",
            vec![
                PyRep::Integer(item_id),
                PyRep::Integer(effect_id as i64),
                PyRep::Integer(now_ticks),
                PyRep::Integer(flag),
                PyRep::Integer(flag),
                PyRep::Tuple(vec![
                    PyRep::Integer(item_id),
                    PyRep::None,
                    PyRep::Integer(item_id),
                    PyRep::None,
                    PyRep::None,
                    PyRep::List(Vec::new()),
                    PyRep::Integer(effect_id as i64),
                ]),
                PyRep::Integer(now_ticks),
                PyRep::Integer(duration_ms),
                PyRep::Integer(repeat as i64),
                PyRep::Integer(0),
                PyRep::None,
            ],
        )
    }

    /// Emits an `OnFleetBroadcast` notification (`broadcastName`, `senderID`, `targetID`, `fleetID`) (`Z-2`).
    pub fn notify_fleet_broadcast(
        &self,
        fleet_id: i64,
        broadcast_name: &str,
        sender_id: i64,
        target_id: i64,
    ) -> Vec<PendingMachoNotification> {
        self.notify(
            NotificationTarget::Fleet(fleet_id),
            "OnFleetBroadcast",
            "fleetid",
            vec![
                PyRep::String(broadcast_name.to_string()),
                PyRep::Integer(sender_id),
                PyRep::Integer(target_id),
                PyRep::Integer(fleet_id),
            ],
        )
    }

    /// Emits an `OnClientEvent_ChatMessageSent` chat message notification (`channelID`, `senderID`, `text`) (`Z-2`).
    pub fn notify_lsc_message(
        &self,
        target: NotificationTarget,
        channel_id: &str,
        sender_id: i64,
        text: &str,
    ) -> Vec<PendingMachoNotification> {
        self.notify(
            target,
            "OnClientEvent_ChatMessageSent",
            "channelid",
            vec![
                PyRep::String(channel_id.to_string()),
                PyRep::Integer(sender_id),
                PyRep::String(text.to_string()),
            ],
        )
    }

    /// Emits an `OnWarpToAsteroidBelt` notification (`charID`, `beltID`) (`Z-2`).
    pub fn notify_warp_to_asteroid_belt(
        &self,
        char_id: i64,
        belt_id: i64,
    ) -> Vec<PendingMachoNotification> {
        self.notify(
            NotificationTarget::Character(char_id),
            "OnWarpToAsteroidBelt",
            "charid",
            vec![PyRep::Integer(char_id), PyRep::Integer(belt_id)],
        )
    }

    /// Emits an `OnAsteroidTerminated` notification (`asteroidID`) to the solar system (`Z-2`).
    pub fn notify_asteroid_terminated(
        &self,
        sys_id: u32,
        asteroid_id: i64,
    ) -> Vec<PendingMachoNotification> {
        self.notify(
            NotificationTarget::SolarSystem(sys_id),
            "OnAsteroidTerminated",
            "solarsystemid",
            vec![PyRep::Integer(asteroid_id)],
        )
    }

    /// Emits an `OnMiningLedgerGraphSettingChanged` notification (`settingKey`, `settingValue`) (`Z-2`).
    pub fn notify_mining_ledger_graph_setting_changed(
        &self,
        char_id: i64,
        setting_key: &str,
        setting_value: PyRep,
    ) -> Vec<PendingMachoNotification> {
        self.notify(
            NotificationTarget::Character(char_id),
            "OnMiningLedgerGraphSettingChanged",
            "charid",
            vec![PyRep::String(setting_key.to_string()), setting_value],
        )
    }

    /// Synchronizes local chat membership for a character when entering or changing solar systems (SO-3).
    pub fn sync_character_local_chat(
        &self,
        char_id: i64,
        old_sys: Option<u32>,
        new_sys: u32,
    ) {
        let old_chan_opt = old_sys
            .filter(|&old| old != new_sys)
            .map(|old| format!("solarsystemid2:{old}"));
        let candidate_speakers = old_chan_opt
            .as_deref()
            .map(|cid| {
                crate::services::fleet_social::fleet_social_chat::get_delayed_local_speakers(
                    self, cid,
                )
            })
            .unwrap_or_default();
        let mut dropped_speakers = Vec::new();
        let _ = self.db.try_with_mut_snapshot(|snap| {
            if let Some(ref old_chan_id) = old_chan_opt {
                if let Some(chan) = snap.chat_channels.get_mut(old_chan_id) {
                    chan.members.remove(&char_id);
                    let rem_set = chan.members.clone();
                    dropped_speakers = crate::services::fleet_social::fleet_social_chat::collect_wh_local_witness_speakers_to_drop_in_snap(
                        snap,
                        old_chan_id,
                        char_id,
                        &rem_set,
                        &candidate_speakers,
                    );
                }
            }
            let new_chan_id = format!("solarsystemid2:{}", new_sys);
            let chan = snap.chat_channels.entry(new_chan_id.clone()).or_insert_with(|| eve_db::ChatChannelRecord {
                channel_id: new_chan_id.clone(), name: format!("Local: {}", new_sys), motd: String::new(),
                owner_id: None, members: std::collections::HashSet::new(), is_system_local: true,
                solar_system_id: Some(new_sys), is_private: false, operators: std::collections::HashSet::new(),
                password: None, is_delayed_local: crate::services::quasar_social_chat::is_delayed_local_system(new_sys),
            });
            chan.members.insert(char_id);
            Ok::<(), std::convert::Infallible>(())
        });
        if let Some(ref old_chan_id) = old_chan_opt {
            crate::services::fleet_social::fleet_social_chat::remove_delayed_local_speakers(
                self,
                old_chan_id,
                &dropped_speakers,
            );
        }

        if old_sys.is_none() {
            self.deliver_unprocessed_notifications_on_login(char_id);
        }

        let ch_res = self.db.get_character(char_id).ok();
        {
            let mut subs = self.quasar_gateway.subscribers.write().unwrap_or_else(|e| e.into_inner());
            for sub in subs.values_mut() {
                if sub.character_id == Some(char_id as u32) {
                    sub.solar_system_id = None;
                } else if sub.character_id.is_none() && sub.user_id.is_some() {
                    if let Some(ch) = &ch_res {
                        if Some(ch.user_id as i32) == sub.user_id {
                            sub.character_id = Some(char_id as u32);
                            sub.solar_system_id = None;
                            sub.corporation_id = Some(ch.corporation_id as u32);
                            sub.alliance_id = ch.alliance_id.map(|a| a as i32);
                        }
                    }
                }
            }
        }

        let now = self.now_unix();
        if let Some(old) = old_sys {
            if old != new_sys {
                let _ = crate::services::quasar_social_chat::on_character_leave_local_chat(
                    &self.quasar_gateway, &self.db, char_id as u32, old, now,
                );
            }
        }
        let _ = crate::services::quasar_social_chat::on_character_enter_local_chat(
            &self.quasar_gateway, &self.db, char_id as u32, new_sys, now,
        );
        if let Ok(mut subs) = self.quasar_gateway.subscribers.write() {
            for sub in subs.values_mut() {
                if sub.character_id == Some(char_id as u32) {
                    sub.solar_system_id = Some(new_sys);
                }
            }
        }

        if !crate::services::quasar_social_chat::is_delayed_local_system(new_sys) {
            self.notify(
                NotificationTarget::SolarSystem(new_sys), "OnChannelJoined", "channelid",
                vec![PyRep::String(format!("solarsystemid2:{}", new_sys)), PyRep::String("Join".into()), PyRep::Integer(char_id)],
            );
        }
        if old_sys != Some(new_sys) {
            crate::services::pve_incursion_lifecycle::emit_active_incursion_tale_data_for_system(
                self, new_sys,
            );
        }
    }

    pub fn effect_id_for_module(&self, type_id: u32, effect_name: Option<&str>) -> Option<i64> {
        crate::router::binding::effect_id_for_module_sde(&self.sde, type_id, effect_name)
    }

    pub fn notify_module_effect_started(
        &self,
        char_id: i64,
        ship_id: i64,
        module_id: i64,
        module_type_id: u32,
        effect_name: Option<&str>,
        target_id: Option<i64>,
    ) -> Vec<PendingMachoNotification> {
        let effect_id = self.effect_id_for_module(module_type_id, effect_name).unwrap_or(0);
        let duration_ms = self
            .ship_fittings
            .read()
            .ok()
            .and_then(|map| map.get(&ship_id).cloned())
            .filter(|fit| fit.modules.contains_key(&module_id))
            .map(|fit| {
                let d73 = fit.get_module_effective_attribute(module_id, 73, &self.sde);
                if d73 > 0.0 {
                    d73
                } else {
                    fit.get_module_effective_attribute(module_id, 51, &self.sde)
                }
            })
            .or_else(|| {
                self.sde.types.get(&module_type_id).and_then(|t| {
                    t.default_attributes
                        .get(&73)
                        .or_else(|| t.default_attributes.get(&51))
                        .copied()
                })
            })
            .unwrap_or(0.0);
        let repeat = if effect_id == 16 { 0 } else { 1000 };
        let now = self.now_blueos_filetime_ticks();
        let environment = PyRep::Tuple(vec![
            PyRep::Integer(module_id),
            PyRep::Integer(char_id),
            PyRep::Integer(ship_id),
            target_id.map(PyRep::Integer).unwrap_or(PyRep::None),
            PyRep::None,
            PyRep::List(vec![]),
            PyRep::Integer(effect_id),
        ]);
        let notifs = self.notify(
            NotificationTarget::Character(char_id),
            "OnGodmaShipEffect",
            "charid",
            vec![
                PyRep::Integer(module_id),
                PyRep::Integer(effect_id),
                PyRep::Integer(now),
                PyRep::Integer(1),
                PyRep::Integer(1),
                environment,
                PyRep::Integer(now),
                PyRep::Real(duration_ms),
                PyRep::Integer(repeat),
                PyRep::None,
            ],
        );
        if let Some((sfx_guid, is_offensive)) =
            crate::router::binding::special_fx_guid_for_module_sde(
                &self.sde,
                module_type_id,
                effect_id,
                effect_name,
            )
            .filter(|(guid, _)| guid != "effects.Cloak")
        {
            self.notify(
                NotificationTarget::Character(char_id),
                "OnSpecialFX",
                "charid",
                vec![
                    PyRep::Integer(ship_id),
                    PyRep::Integer(module_id),
                    PyRep::Integer(module_type_id as i64),
                    target_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                    PyRep::None,
                    PyRep::String(sfx_guid),
                    PyRep::Bool(is_offensive),
                    PyRep::Integer(1),
                    PyRep::Integer(1),
                    PyRep::Real(duration_ms),
                    PyRep::Integer(0),
                    PyRep::Integer(now),
                    PyRep::Real(0.0),
                    PyRep::None,
                ],
            );
        }
        notifs
    }

    pub fn notify_module_online_state(
        &self,
        char_id: i64,
        module_id: i64,
        online: bool,
    ) -> Vec<PendingMachoNotification> {
        let (new_v, old_v) = if online { (1.0, 0.0) } else { (0.0, 1.0) };
        let now = self.now_blueos_filetime_ticks();
        let entry = PyRep::Tuple(vec![
            PyRep::String("OnModuleAttributeChange".into()),
            PyRep::Integer(char_id),
            PyRep::Integer(module_id),
            PyRep::Integer(2),
            PyRep::Integer(now),
            PyRep::Real(new_v),
            PyRep::Real(old_v),
            PyRep::None,
        ]);
        self.notify(
            NotificationTarget::Character(char_id),
            "OnModuleAttributeChanges",
            "charid",
            vec![PyRep::List(vec![entry])],
        )
    }

    /// Emits `OnModuleAttributeChanges` for ship `cpuLoad` (49) and `powerLoad` (15) changes.
    pub fn notify_ship_fitting_load_changes(
        &self,
        char_id: i64,
        ship_id: i64,
        old_cpu: f64,
        new_cpu: f64,
        old_pg: f64,
        new_pg: f64,
    ) -> Vec<PendingMachoNotification> {
        let now = self.now_blueos_filetime_ticks();
        let changes = vec![
            PyRep::Tuple(vec![
                PyRep::String("OnModuleAttributeChange".into()),
                PyRep::Integer(char_id),
                PyRep::Integer(ship_id),
                PyRep::Integer(i64::from(eve_sde::attr_ids::CPU_LOAD)),
                PyRep::Integer(now),
                PyRep::Real(new_cpu),
                PyRep::Real(old_cpu),
                PyRep::None,
            ]),
            PyRep::Tuple(vec![
                PyRep::String("OnModuleAttributeChange".into()),
                PyRep::Integer(char_id),
                PyRep::Integer(ship_id),
                PyRep::Integer(i64::from(eve_sde::attr_ids::POWER_LOAD)),
                PyRep::Integer(now),
                PyRep::Real(new_pg),
                PyRep::Real(old_pg),
                PyRep::None,
            ]),
        ];
        self.notify(
            NotificationTarget::Character(char_id),
            "OnModuleAttributeChanges",
            "charid",
            vec![PyRep::List(changes)],
        )
    }

    /// Emits a canonical `OnItemChange(row, changes)` notification using `DBRowDescriptor::inv_item_header`.
    pub fn notify_item_record_change(
        &self,
        char_id: i64,
        item: &eve_db::InventoryItemRecord,
        changes: Vec<(i32, PyRep)>,
    ) -> Vec<PendingMachoNotification> {
        self.notify_item_record_change_with_target(
            NotificationTarget::Character(char_id),
            "charid",
            item,
            changes,
        )
    }

    /// Emits a canonical `OnItemChange(row, changes)` notification to a specific [`NotificationTarget`].
    pub fn notify_item_record_change_with_target(
        &self,
        target: NotificationTarget,
        id_type: &str,
        item: &eve_db::InventoryItemRecord,
        changes: Vec<(i32, PyRep)>,
    ) -> Vec<PendingMachoNotification> {
        let cells = crate::services::inventory_bound::item_to_packed_cells(self, item);
        let row = eve_proto::DBRowDescriptor::inv_item_header().build_dbrow(cells);
        let change_dict = PyRep::Dict(
            changes
                .into_iter()
                .map(|(col_ix, old_val)| (PyRep::Integer(col_ix as i64), old_val))
                .collect(),
        );
        self.notify(target, "OnItemChange", id_type, vec![row, change_dict])
    }

    /// Computes canonical V24 `OnItemChange` delta entries (`old_value` keyed by column index).
    /// Never emits `ixQuantity` (key 5); stack size deltas use `ixStackSize` (key 9).
    pub fn compute_item_change_dict(
        old: &eve_db::InventoryItemRecord,
        new: &eve_db::InventoryItemRecord,
    ) -> Vec<(i32, PyRep)> {
        let mut changes = Vec::new();
        if old.owner_id != new.owner_id {
            changes.push((2, PyRep::Integer(old.owner_id)));
        }
        if old.location_id != new.location_id {
            changes.push((3, PyRep::Integer(old.location_id)));
        }
        if old.flag != new.flag {
            changes.push((4, PyRep::Integer(old.flag as i64)));
        }
        if old.quantity != new.quantity {
            changes.push((9, PyRep::Integer(old.quantity as i64)));
        }
        if old.singleton != new.singleton {
            changes.push((10, PyRep::Integer(if old.singleton { 1 } else { 0 })));
        }
        changes
    }

    /// Builds a `locJunkyard` (location 6) tombstone record + delta dict for deleted/consumed items.
    pub fn build_item_tombstone(
        old: &eve_db::InventoryItemRecord,
    ) -> (eve_db::InventoryItemRecord, Vec<(i32, PyRep)>) {
        let mut tomb = old.clone();
        tomb.location_id = 6; // locJunkyard
        tomb.flag = eve_db::InventoryFlag::None;
        tomb.quantity = 0;
        tomb.singleton = false;
        let changes = vec![
            (3, PyRep::Integer(old.location_id)),
            (4, PyRep::Integer(old.flag as i64)),
            (9, PyRep::Integer(old.quantity as i64)),
        ];
        (tomb, changes)
    }

    /// Resolves whether an item belongs to a corporation hangar/deliveries scope or personal scope.
    pub fn resolve_item_notification_target(
        &self,
        fallback_char_id: i64,
        item: &eve_db::InventoryItemRecord,
    ) -> (NotificationTarget, &'static str) {
        let (is_corp, is_other_char) = self.db.with_snapshot(|s| {
            let corp =
                is_corp_hangar_flag(item.flag) && s.corporations.contains_key(&item.owner_id);
            let other_char = item.owner_id > 0
                && item.owner_id != fallback_char_id
                && s.characters.contains_key(&item.owner_id);
            (corp, other_char)
        });
        if is_corp {
            (NotificationTarget::Corporation(item.owner_id), "corpid")
        } else if is_other_char {
            (NotificationTarget::Character(item.owner_id), "charid")
        } else {
            (NotificationTarget::Character(fallback_char_id), "charid")
        }
    }

    /// Emits `OnItemChange` routed to `Corporation(owner_id)` for corp hangar items (`116..=122 | 62`)
    /// or `Character(fallback_char_id)` otherwise.
    pub fn emit_item_change_routed(
        &self,
        fallback_char_id: i64,
        item: &eve_db::InventoryItemRecord,
        changes: Vec<(i32, PyRep)>,
    ) {
        let (target, id_type) = self.resolve_item_notification_target(fallback_char_id, item);
        self.notify_item_record_change_with_target(target, id_type, item, changes);
    }

    /// Emits a routed `locJunkyard` tombstone `OnItemChange` for a deleted or fully consumed item.
    pub fn emit_item_deleted_routed(
        &self,
        fallback_char_id: i64,
        old_item: &eve_db::InventoryItemRecord,
    ) {
        let (tomb, changes) = Self::build_item_tombstone(old_item);
        let (target, id_type) = self.resolve_item_notification_target(fallback_char_id, old_item);
        self.notify_item_record_change_with_target(target, id_type, &tomb, changes);
    }

    /// Emits `OnItemChange` by diffing `pre` and `post` item states, routing to both old and new targets
    /// if ownership/corp-hangar scope changed.
    pub fn notify_item_mutation(
        &self,
        fallback_char_id: i64,
        pre: Option<&eve_db::InventoryItemRecord>,
        post: Option<&eve_db::InventoryItemRecord>,
    ) {
        match (pre, post) {
            (None, Some(new_item)) => {
                self.emit_item_change_routed(fallback_char_id, new_item, Vec::new());
            }
            (Some(old_item), None) => {
                self.emit_item_deleted_routed(fallback_char_id, old_item);
            }
            (Some(old_item), Some(new_item)) => {
                let changes = Self::compute_item_change_dict(old_item, new_item);
                if changes.is_empty() {
                    return;
                }
                let (old_target, old_id_type) =
                    self.resolve_item_notification_target(fallback_char_id, old_item);
                let (new_target, new_id_type) =
                    self.resolve_item_notification_target(fallback_char_id, new_item);
                self.notify_item_record_change_with_target(
                    new_target.clone(),
                    new_id_type,
                    new_item,
                    changes.clone(),
                );
                if old_target != new_target {
                    self.notify_item_record_change_with_target(
                        old_target,
                        old_id_type,
                        new_item,
                        changes,
                    );
                }
            }
            (None, None) => {}
        }
    }

    /// Emits `OnAccountChange` for personal (`cash`) or corporate division (`cash` / `cash2..cash7`) wallets.
    pub fn notify_wallet_balance(
        &self,
        is_corp: bool,
        owner_id: i64,
        div_id: u8,
        balance: eve_db::IskCents,
    ) -> Vec<PendingMachoNotification> {
        if is_corp {
            let key = if div_id <= 1 {
                "cash".to_string()
            } else {
                format!("cash{div_id}")
            };
            self.notify(
                NotificationTarget::Corporation(owner_id),
                "OnAccountChange",
                "corpid",
                vec![
                    PyRep::String(key),
                    PyRep::Integer(owner_id),
                    PyRep::Real(balance.to_wire_f64()),
                ],
            )
        } else {
            self.notify_account_change(owner_id, "cash", owner_id, balance.to_wire_f64())
        }
    }

    pub fn notify_target_events(
        &self,
        char_id: i64,
        events: &[(&str, i64, &str)],
    ) -> Vec<PendingMachoNotification> {
        let list = events
            .iter()
            .map(|(what, tid, reason)| {
                PyRep::Tuple(vec![
                    PyRep::Tuple(vec![
                        PyRep::String("OnTarget".into()),
                        PyRep::String((*what).into()),
                        PyRep::Integer(*tid),
                        PyRep::String((*reason).into()),
                    ]),
                    PyRep::Integer(0),
                ])
            })
            .collect();
        self.notify(
            NotificationTarget::Character(char_id),
            "OnMultiEvent",
            "charid",
            vec![PyRep::List(list)],
        )
    }

    pub fn notify_system_criminal_flag_updates(
        &self,
        sys_id: u32,
        new_idles: &[i64],
        new_suspects: &[i64],
        new_criminals: &[i64],
    ) -> Vec<PendingMachoNotification> {
        let ids = |v: &[i64]| PyRep::List(v.iter().map(|c| PyRep::Integer(*c)).collect());
        self.notify(
            NotificationTarget::SolarSystem(sys_id),
            "OnSystemCriminalFlagUpdates",
            "solarsystemid",
            vec![ids(new_idles), ids(new_suspects), ids(new_criminals)],
        )
    }

    pub fn notify_fitted_item_trashed(
        &self, char_id: i64, item_id: i64, type_id: i64, quantity: i64, old_location_id: i64, old_flag: i64,
    ) -> Vec<PendingMachoNotification> {
        let (group_id, category_id) = u32::try_from(type_id)
            .ok()
            .and_then(|tid| self.sde.types.get(&tid))
            .map(|t| (i64::from(t.group_id), i64::from(t.category_id)))
            .unwrap_or((0, 0));
        // Fitted modules/rigs are singleton items, so their packed inv_item quantity is -1 when qty == 1.
        let packed_qty = if quantity == 1 { -1 } else { quantity };
        let row = eve_proto::DBRowDescriptor::inv_item_header().build_dbrow(vec![
            PyRep::Integer(item_id),
            PyRep::Integer(type_id),
            PyRep::Integer(char_id),
            PyRep::Integer(10),
            PyRep::Integer(147),
            PyRep::Integer(packed_qty),
            PyRep::Integer(group_id),
            PyRep::Integer(category_id),
            PyRep::String(String::new()),
        ]);
        self.notify_item_change(
            char_id,
            row,
            vec![
                (3, PyRep::Integer(old_location_id)),
                (4, PyRep::Integer(old_flag)),
            ],
        )
    }
}

#[cfg(test)] #[path = "cache_notifications_unit_tests.rs"] mod cache_notifications_unit_tests;
#[cfg(test)] #[path = "cache_notifications_edges_tests.rs"] mod cache_notifications_edges_tests;
