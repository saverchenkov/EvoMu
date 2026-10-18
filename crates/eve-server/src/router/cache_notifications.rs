//! Object caching (`objectCaching.CachedObject`, `util.CachedObject`, `objectCaching.CachedMethodCallResult`)
//! and binary-marshaled `MachoPacket` client notification routing (`P-3`, `P-5`).

#![forbid(unsafe_code)]

use crate::router::context::ServerContext;
use eve_machonet::{MachoAddress, MachoPacket, MachoPacketType};
use eve_proto::{marshal, PyRep};
use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};

static NEXT_NOTIFICATION_ID: AtomicI64 = AtomicI64::new(500_001);

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
    /// Encodes as `objectCaching.CachedObject` (`ObjectCaching.xmlp:5-31`).
    pub fn to_cached_object_pyrep(&self) -> PyRep {
        PyRep::Object {
            type_name: "objectCaching.CachedObject".into(),
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

    /// Encodes as a `util.CachedObject` cache specification hint (`ObjectCaching.xmlp:33-51`).
    pub fn to_cache_hint_pyrep(&self) -> PyRep {
        PyRep::Object {
            type_name: "util.CachedObject".into(),
            args: Box::new(PyRep::Tuple(vec![
                self.object_id.clone(),
                PyRep::Integer(self.node_id),
                PyRep::Tuple(vec![
                    PyRep::Integer(self.timestamp),
                    PyRep::Integer(self.version as i64),
                ]),
            ])),
        }
    }

    /// Encodes as an inline `objectCaching.CachedMethodCallResult` (`ObjectCaching.xmlp:53-91`).
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
            type_name: "objectCaching.CachedMethodCallResult".into(),
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

    /// Encodes as a hint-backed `objectCaching.CachedMethodCallResult` (`ObjectCaching.xmlp:93-112`).
    pub fn to_cached_method_call_hint_pyrep(&self, version_check: &str) -> PyRep {
        PyRep::Object {
            type_name: "objectCaching.CachedMethodCallResult".into(),
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

/// Queued binary-marshaled `MachoPacket` (`Notification` = `12`) ready for delivery (`P-5`).
#[derive(Debug, Clone)]
pub struct PendingMachoNotification {
    pub recipient_char_id: i64,
    pub event: String,
    pub id_type: String,
    pub args: Vec<PyRep>,
    pub packet: MachoPacket,
    pub marshaled_packet: Vec<u8>,
    pub created_filetime: i64,
}

/// Persistent notification inbox record for `notificationMgr` (`NotificationMgrService.cpp:66-90`).
#[derive(Debug, Clone, PartialEq)]
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
}

/// Maximum queued real-time notifications per online character (`R51-P1-3`).
pub const MAX_PENDING_NOTIFICATIONS_PER_CHAR: usize = 128;

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

    /// Wraps a service method result in `objectCaching.CachedMethodCallResult` and registers it in `self.object_cache`.
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
        self.db.with_snapshot(|snap| {
            let mut out = Vec::new();
            match target {
                NotificationTarget::Character(cid) => {
                    if let Some(c) = snap.characters.get(cid) {
                        if self.is_character_online(c.character_id, c.user_id) {
                            out.push((*cid, Some(c.user_id)));
                        }
                    }
                }
                NotificationTarget::Corporation(corp_id) => {
                    for c in snap.characters.values() {
                        if c.corporation_id == *corp_id
                            && self.is_character_online(c.character_id, c.user_id)
                        {
                            out.push((c.character_id, Some(c.user_id)));
                        }
                    }
                }
                NotificationTarget::Alliance(ally_id) => {
                    for c in snap.characters.values() {
                        if c.alliance_id == Some(*ally_id)
                            && self.is_character_online(c.character_id, c.user_id)
                        {
                            out.push((c.character_id, Some(c.user_id)));
                        }
                    }
                }
                NotificationTarget::Fleet(fleet_id) => {
                    if let Some(f) = snap.fleets.get(fleet_id) {
                        for &cid in &f.members {
                            if let Some(c) = snap.characters.get(&cid) {
                                if self.is_character_online(c.character_id, c.user_id) {
                                    out.push((cid, Some(c.user_id)));
                                }
                            }
                        }
                    }
                }
                NotificationTarget::SolarSystem(sys_id) => {
                    for c in snap.characters.values() {
                        if c.solar_system_id == *sys_id
                            && self.is_character_online(c.character_id, c.user_id)
                        {
                            out.push((c.character_id, Some(c.user_id)));
                        }
                    }
                }
                NotificationTarget::Station(station_id) => {
                    for c in snap.characters.values() {
                        if (c.station_id == Some(*station_id)
                            || c.structure_id == Some(*station_id))
                            && self.is_character_online(c.character_id, c.user_id)
                        {
                            out.push((c.character_id, Some(c.user_id)));
                        }
                    }
                }
                NotificationTarget::All => {
                    for c in snap.characters.values() {
                        if self.is_character_online(c.character_id, c.user_id) {
                            out.push((c.character_id, Some(c.user_id)));
                        }
                    }
                }
            }
            out.sort_by_key(|(cid, _)| *cid);
            out.dedup_by_key(|(cid, _)| *cid);
            out
        })
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
            PyRep::Integer(1),
            PyRep::SubStream(Box::new(PyRep::Tuple(vec![
                PyRep::Integer(1),
                PyRep::Tuple(args),
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
            let (packet, marshaled_packet) =
                Self::build_macho_notification_packet(cid, uid, event, id_type, args.clone());
            let notif = PendingMachoNotification {
                recipient_char_id: cid,
                event: event.to_string(),
                id_type: id_type.to_string(),
                args: args.clone(),
                packet,
                marshaled_packet,
                created_filetime: now_ft,
            };
            let q = queues.entry(cid).or_default();
            if q.len() >= MAX_PENDING_NOTIFICATIONS_PER_CHAR {
                q.remove(0);
            }
            q.push(notif.clone());
            let _ = self.notify_tx.send((cid, notif.clone()));
            emitted.push(notif);
        }
        emitted
    }

    /// Drains all pending notifications queued for `char_id`.
    pub fn drain_notifications_for_char(&self, char_id: i64) -> Vec<PendingMachoNotification> {
        self.pending_notifications
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&char_id)
            .unwrap_or_default()
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
        let notification_id = NEXT_NOTIFICATION_ID.fetch_add(1, Ordering::Relaxed);
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
        };
        self.notification_records
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .push(rec.clone());
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
}
