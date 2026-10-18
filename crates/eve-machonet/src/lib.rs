//! `eve-machonet` — EVE Online `machoNet` packet framing, addressing, and session management.

#![forbid(unsafe_code)]

use eve_proto::PyRep;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Standard EVE `machoNet` message types (`machoPacketType`).
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MachoPacketType {
    AuthenticationReq = 0,
    AuthenticationRsp = 1,
    IdentificationReq = 2,
    IdentificationRsp = 3,
    CallReq = 6,
    CallRsp = 7,
    TransportClosed = 8,
    ResolveReq = 10,
    ResolveRsp = 11,
    Notification = 12,
    ErrorResponse = 15,
    SessionChangeNotification = 16,
    SessionInitialStateNotification = 18,
    PingReq = 20,
    PingRsp = 21,
}

impl MachoPacketType {
    pub fn from_i64(v: i64) -> Option<Self> {
        match v {
            0 => Some(Self::AuthenticationReq),
            1 => Some(Self::AuthenticationRsp),
            2 => Some(Self::IdentificationReq),
            3 => Some(Self::IdentificationRsp),
            6 => Some(Self::CallReq),
            7 => Some(Self::CallRsp),
            8 => Some(Self::TransportClosed),
            10 => Some(Self::ResolveReq),
            11 => Some(Self::ResolveRsp),
            12 => Some(Self::Notification),
            15 => Some(Self::ErrorResponse),
            16 => Some(Self::SessionChangeNotification),
            18 => Some(Self::SessionInitialStateNotification),
            20 => Some(Self::PingReq),
            21 => Some(Self::PingRsp),
            _ => None,
        }
    }
}

/// `macho.MachoAddress` endpoint representation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MachoAddress {
    Node {
        node_id: u64,
        service: Option<String>,
        call_id: u64,
    },
    Client {
        client_id: u64,
        call_id: u64,
        service: Option<String>,
    },
    Broadcast {
        broadcast_id: String,
        id_type: String,
        narrowcast: Vec<u64>,
    },
    Any {
        service: Option<String>,
        call_id: u64,
    },
}

impl MachoAddress {
    pub fn to_pyrep(&self) -> PyRep {
        let tuple = match self {
            Self::Node {
                node_id,
                service,
                call_id,
            } => vec![
                PyRep::ascii_bytes("N"),
                PyRep::Integer(*node_id as i64),
                service
                    .as_ref()
                    .map(PyRep::ascii_bytes)
                    .unwrap_or(PyRep::None),
                PyRep::Integer(*call_id as i64),
            ],
            Self::Client {
                client_id,
                call_id,
                service,
            } => vec![
                PyRep::ascii_bytes("C"),
                PyRep::Integer(*client_id as i64),
                PyRep::Integer(*call_id as i64),
                service
                    .as_ref()
                    .map(PyRep::ascii_bytes)
                    .unwrap_or(PyRep::None),
            ],
            Self::Broadcast {
                broadcast_id,
                id_type,
                narrowcast,
            } => vec![
                PyRep::ascii_bytes("B"),
                PyRep::ascii_bytes(broadcast_id),
                PyRep::List(
                    narrowcast
                        .iter()
                        .map(|&id| PyRep::Integer(id as i64))
                        .collect(),
                ),
                PyRep::ascii_bytes(id_type),
            ],
            Self::Any { service, call_id } => vec![
                PyRep::ascii_bytes("A"),
                service
                    .as_ref()
                    .map(PyRep::ascii_bytes)
                    .unwrap_or(PyRep::None),
                PyRep::Integer(*call_id as i64),
            ],
        };
        PyRep::Object {
            type_name: "macho.MachoAddress".into(),
            args: Box::new(PyRep::Tuple(tuple)),
        }
    }

    pub fn from_pyrep(rep: &PyRep) -> Option<Self> {
        let tuple = match rep {
            PyRep::Object { args, .. } => match args.as_ref() {
                PyRep::Tuple(items) => items,
                _ => return None,
            },
            PyRep::ObjectEx { header, .. } => match header.as_ref() {
                PyRep::Tuple(items) if items.len() >= 2 => match &items[1] {
                    PyRep::Tuple(inner) => inner,
                    _ => items,
                },
                _ => return None,
            },
            PyRep::Tuple(items) => items,
            _ => return None,
        };
        let tag = tuple.first()?.as_str()?;
        match tag {
            "N" => Some(Self::Node {
                node_id: tuple.get(1)?.as_i64()? as u64,
                service: tuple.get(2).and_then(|v| v.as_str()).map(ToOwned::to_owned),
                call_id: tuple.get(3).and_then(|v| v.as_i64()).unwrap_or(0) as u64,
            }),
            "C" => Some(Self::Client {
                client_id: tuple.get(1)?.as_i64()? as u64,
                call_id: tuple.get(2).and_then(|v| v.as_i64()).unwrap_or(0) as u64,
                service: tuple.get(3).and_then(|v| v.as_str()).map(ToOwned::to_owned),
            }),
            "B" => {
                let broadcast_id = tuple.get(1)?.as_str()?.to_owned();
                let narrowcast = match tuple.get(2) {
                    Some(PyRep::List(items)) | Some(PyRep::Tuple(items)) => items
                        .iter()
                        .filter_map(|i| i.as_i64().map(|v| v as u64))
                        .collect(),
                    _ => Vec::new(),
                };
                let id_type = tuple
                    .get(3)
                    .and_then(|v| v.as_str())
                    .unwrap_or("*multicastID")
                    .to_owned();
                Some(Self::Broadcast {
                    broadcast_id,
                    id_type,
                    narrowcast,
                })
            }
            "A" => Some(Self::Any {
                service: tuple.get(1).and_then(|v| v.as_str()).map(ToOwned::to_owned),
                call_id: tuple.get(2).and_then(|v| v.as_i64()).unwrap_or(0) as u64,
            }),
            _ => None,
        }
    }
}

/// High-level `macho.MachoPacket` structure.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MachoPacket {
    pub packet_type: MachoPacketType,
    pub source: MachoAddress,
    pub destination: MachoAddress,
    pub user_id: Option<u64>,
    pub payload: PyRep,
    pub named_payload: HashMap<String, PyRep>,
}

impl MachoPacket {
    pub fn class_name(packet_type: MachoPacketType) -> &'static str {
        match packet_type {
            MachoPacketType::AuthenticationReq => "macho.AuthenticationReq",
            MachoPacketType::AuthenticationRsp => "macho.AuthenticationRsp",
            MachoPacketType::IdentificationReq => "macho.IdentificationReq",
            MachoPacketType::IdentificationRsp => "macho.IdentificationRsp",
            MachoPacketType::CallReq => "macho.CallReq",
            MachoPacketType::CallRsp => "macho.CallRsp",
            MachoPacketType::TransportClosed => "macho.TransportClosed",
            MachoPacketType::ResolveReq => "macho.ResolveReq",
            MachoPacketType::ResolveRsp => "macho.ResolveRsp",
            MachoPacketType::Notification => "macho.Notification",
            MachoPacketType::ErrorResponse => "macho.ErrorResponse",
            MachoPacketType::SessionChangeNotification => "macho.SessionChangeNotification",
            MachoPacketType::SessionInitialStateNotification => {
                "macho.SessionInitialStateNotification"
            }
            MachoPacketType::PingReq => "macho.PingReq",
            MachoPacketType::PingRsp => "macho.PingRsp",
        }
    }

    pub fn to_pyrep(&self) -> PyRep {
        let named = if self.named_payload.is_empty() {
            PyRep::None
        } else {
            PyRep::Dict(
                self.named_payload
                    .iter()
                    .map(|(k, v)| (PyRep::ascii_bytes(k), v.clone()))
                    .collect(),
            )
        };
        PyRep::Object {
            type_name: Self::class_name(self.packet_type).into(),
            args: Box::new(PyRep::Tuple(vec![
                PyRep::Integer(self.packet_type as i64),
                self.source.to_pyrep(),
                self.destination.to_pyrep(),
                self.user_id
                    .map(|u| PyRep::Integer(u as i64))
                    .unwrap_or(PyRep::None),
                self.payload.clone(),
                named,
                PyRep::None,
            ])),
        }
    }

    pub fn from_pyrep(rep: &PyRep) -> Option<Self> {
        let tuple = match rep {
            PyRep::Object { args, .. } => match args.as_ref() {
                PyRep::Tuple(items) => items,
                _ => return None,
            },
            PyRep::ObjectEx { header, .. } => match header.as_ref() {
                PyRep::Tuple(items) if items.len() >= 2 => match &items[1] {
                    PyRep::Tuple(inner) => inner,
                    _ => items,
                },
                _ => return None,
            },
            PyRep::Tuple(items) => items,
            _ => return None,
        };
        if tuple.len() < 6 {
            return None;
        }
        let packet_type = MachoPacketType::from_i64(tuple[0].as_i64()?)?;
        let source = MachoAddress::from_pyrep(&tuple[1])?;
        let destination = MachoAddress::from_pyrep(&tuple[2])?;
        let user_id = tuple[3].as_i64().map(|u| u as u64);
        let payload = tuple[4].clone();
        let mut named_payload = HashMap::new();
        if let PyRep::Dict(entries) = &tuple[5] {
            for (k, v) in entries {
                if let Some(key) = k.as_str() {
                    named_payload.insert(key.to_owned(), v.clone());
                }
            }
        }
        Some(Self {
            packet_type,
            source,
            destination,
            user_id,
            payload,
            named_payload,
        })
    }
}

/// Low-level `VersionExchange` handshake parameters exchanged when a client connects on TCP 26000.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionExchange {
    pub birthday: u32,
    pub macho_version: u32,
    pub user_count: u32,
    pub version_number: f64,
    pub build_version: u32,
    pub project_version: String,
}

impl Default for VersionExchange {
    /// Defaults to V24.01 (`build 3619879`, `EVE-EVE-TRANQUILITY@ccp`).
    fn default() -> Self {
        Self {
            birthday: 170472,
            macho_version: 409,
            user_count: 1,
            version_number: 24.01,
            build_version: 3619879,
            project_version: "EVE-EVE-TRANQUILITY@ccp".to_string(),
        }
    }
}

impl VersionExchange {
    pub fn to_pyrep(&self) -> PyRep {
        PyRep::Tuple(vec![
            PyRep::Integer(self.birthday as i64),
            PyRep::Integer(self.macho_version as i64),
            PyRep::Integer(self.user_count as i64),
            PyRep::Real(self.version_number),
            PyRep::Integer(self.build_version as i64),
            PyRep::ascii_bytes(&self.project_version),
        ])
    }
}

/// Tracks a connected player's `machoNet` session attributes (`userid`, `charid`, `solarsystemid2`, `stationid`, `structureid`, `locationid`, etc.).
/// New sessions start strictly unauthenticated (`user_id: None`, `role: 0`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionState {
    pub session_id: u64,
    pub user_id: Option<u64>,
    pub character_id: Option<i64>,
    pub corporation_id: Option<i64>,
    pub alliance_id: Option<i64>,
    pub station_id: Option<i64>,
    pub structure_id: Option<i64>,
    pub solar_system_id: Option<u32>,
    pub constellation_id: Option<u32>,
    pub region_id: Option<u32>,
    pub ship_id: Option<i64>,
    pub race_id: Option<u32>,
    pub corp_role: u64,
    pub fleet_id: Option<i64>,
    pub war_faction_id: Option<i64>,
    pub role: u64,
    pub language_id: String,
}

impl SessionState {
    pub fn new(session_id: u64) -> Self {
        Self {
            session_id,
            user_id: None,
            corp_role: 0,
            role: 0,
            language_id: "EN".to_string(),
            ..Default::default()
        }
    }

    pub fn is_authenticated(&self) -> bool {
        self.user_id.is_some()
    }

    /// Computes effective `locationid` (`stationid` -> `structureid` -> `solarsystemid2`).
    pub fn effective_location_id(&self) -> Option<i64> {
        self.station_id
            .or(self.structure_id)
            .or_else(|| self.solar_system_id.map(|s| s as i64))
    }

    /// Computes the `(old_value, new_value)` dictionary of changed session attributes between `self` and `next`.
    /// This exact layout is required by `SessionChangeNotification` (`packet_type = 16`) and `OnSessionChanged`.
    pub fn compute_diff(&self, next: &Self) -> HashMap<String, (PyRep, PyRep)> {
        let mut changes = HashMap::new();
        let opt_i64 = |v: Option<i64>| v.map(PyRep::Integer).unwrap_or(PyRep::None);
        let opt_u64 = |v: Option<u64>| v.map(|u| PyRep::Integer(u as i64)).unwrap_or(PyRep::None);
        let opt_u32 = |v: Option<u32>| v.map(|u| PyRep::Integer(u as i64)).unwrap_or(PyRep::None);

        if self.user_id != next.user_id {
            changes.insert(
                "userid".into(),
                (opt_u64(self.user_id), opt_u64(next.user_id)),
            );
        }
        if self.character_id != next.character_id {
            changes.insert(
                "charid".into(),
                (opt_i64(self.character_id), opt_i64(next.character_id)),
            );
        }
        if self.corporation_id != next.corporation_id {
            changes.insert(
                "corpid".into(),
                (opt_i64(self.corporation_id), opt_i64(next.corporation_id)),
            );
        }
        if self.alliance_id != next.alliance_id {
            changes.insert(
                "allianceid".into(),
                (opt_i64(self.alliance_id), opt_i64(next.alliance_id)),
            );
        }
        if self.station_id != next.station_id {
            changes.insert(
                "stationid".into(),
                (opt_i64(self.station_id), opt_i64(next.station_id)),
            );
            changes.insert(
                "stationid2".into(),
                (opt_i64(self.station_id), opt_i64(next.station_id)),
            );
        }
        if self.structure_id != next.structure_id {
            changes.insert(
                "structureid".into(),
                (opt_i64(self.structure_id), opt_i64(next.structure_id)),
            );
        }
        let old_loc = self.effective_location_id();
        let new_loc = next.effective_location_id();
        if old_loc != new_loc {
            changes.insert("locationid".into(), (opt_i64(old_loc), opt_i64(new_loc)));
        }
        if self.solar_system_id != next.solar_system_id {
            changes.insert(
                "solarsystemid2".into(),
                (opt_u32(self.solar_system_id), opt_u32(next.solar_system_id)),
            );
            // In EVE, `solarsystemid` is only set when in space (neither docked in station nor structure)
            let old_space_sys = if self.station_id.is_none() && self.structure_id.is_none() {
                self.solar_system_id
            } else {
                None
            };
            let new_space_sys = if next.station_id.is_none() && next.structure_id.is_none() {
                next.solar_system_id
            } else {
                None
            };
            if old_space_sys != new_space_sys {
                changes.insert(
                    "solarsystemid".into(),
                    (opt_u32(old_space_sys), opt_u32(new_space_sys)),
                );
            }
        } else {
            let old_in_space = self.station_id.is_none() && self.structure_id.is_none();
            let new_in_space = next.station_id.is_none() && next.structure_id.is_none();
            if old_in_space != new_in_space {
                let old_space_sys = if old_in_space {
                    self.solar_system_id
                } else {
                    None
                };
                let new_space_sys = if new_in_space {
                    next.solar_system_id
                } else {
                    None
                };
                changes.insert(
                    "solarsystemid".into(),
                    (opt_u32(old_space_sys), opt_u32(new_space_sys)),
                );
            }
        }
        if self.constellation_id != next.constellation_id {
            changes.insert(
                "constellationid".into(),
                (
                    opt_u32(self.constellation_id),
                    opt_u32(next.constellation_id),
                ),
            );
        }
        if self.region_id != next.region_id {
            changes.insert(
                "regionid".into(),
                (opt_u32(self.region_id), opt_u32(next.region_id)),
            );
        }
        if self.ship_id != next.ship_id {
            changes.insert(
                "shipid".into(),
                (opt_i64(self.ship_id), opt_i64(next.ship_id)),
            );
        }
        if self.race_id != next.race_id {
            changes.insert(
                "raceID".into(),
                (opt_u32(self.race_id), opt_u32(next.race_id)),
            );
        }
        if self.corp_role != next.corp_role {
            changes.insert(
                "corprole".into(),
                (
                    PyRep::Integer(self.corp_role as i64),
                    PyRep::Integer(next.corp_role as i64),
                ),
            );
        }
        if self.fleet_id != next.fleet_id {
            changes.insert(
                "fleetid".into(),
                (opt_i64(self.fleet_id), opt_i64(next.fleet_id)),
            );
        }
        if self.war_faction_id != next.war_faction_id {
            changes.insert(
                "warfactionid".into(),
                (opt_i64(self.war_faction_id), opt_i64(next.war_faction_id)),
            );
        }
        if self.role != next.role {
            changes.insert(
                "role".into(),
                (
                    PyRep::Integer(self.role as i64),
                    PyRep::Integer(next.role as i64),
                ),
            );
        }
        changes
    }

    /// Formats a session diff as the `PyRep` payload of a `SessionChangeNotification` (`(clueless=0, changes_dict)`).
    pub fn to_session_change_pyrep(&self, next: &Self, nodes_of_interest: Vec<i64>) -> Option<PyRep> {
        let diff = self.compute_diff(next);
        if diff.is_empty() {
            return None;
        }
        let dict_entries = diff
            .into_iter()
            .map(|(k, (old_v, new_v))| (PyRep::ascii_bytes(k), PyRep::Tuple(vec![old_v, new_v])))
            .collect();
        Some(PyRep::Tuple(vec![
            PyRep::Tuple(vec![PyRep::Integer(0), PyRep::Dict(dict_entries)]),
            PyRep::List(nodes_of_interest.into_iter().map(PyRep::Integer).collect()),
        ]))
    }

    pub fn has_gm_role(&self) -> bool {
        (self.role & service_roles::ROLE_GM_MASK) != 0
    }
}

/// Canonical CCP 64-bit account/session role constants from V24.01 `carbon/common/script/sys/serviceConst.pyj` (`S-9`).
pub mod service_roles {
    pub const ROLE_ANY: u64 = 0x0;
    pub const ROLE_IGB: u64 = 0x0000_0000_8000_0000;
    pub const ROLE_SPAWN: u64 = 0x0000_0002_0000_0000;
    pub const ROLE_GMS: u64 = 0x0000_0040_0000_0000;
    pub const ROLE_PROGRAMMER: u64 = 0x0008_0000_0000_0000;
    pub const ROLE_QA: u64 = 0x0010_0000_0000_0000;
    pub const ROLE_GMH: u64 = 0x0020_0000_0000_0000;
    pub const ROLE_GML: u64 = 0x0040_0000_0000_0000;
    pub const ROLE_CONTENT: u64 = 0x0080_0000_0000_0000;
    pub const ROLE_ADMIN: u64 = 0x0100_0000_0000_0000;
    pub const ROLE_VIPLOGIN: u64 = 0x0200_0000_0000_0000;
    pub const ROLE_ROLEADMIN: u64 = 0x0400_0000_0000_0000;
    pub const ROLE_NEWBIE: u64 = 0x0800_0000_0000_0000;
    pub const ROLE_SERVICE: u64 = 0x1000_0000_0000_0000;
    pub const ROLE_PLAYER: u64 = 0x2000_0000_0000_0000;
    pub const ROLE_LOGIN: u64 = 0x4000_0000_0000_0000;

    /// Standard player account role mask (`ROLE_PLAYER | ROLE_LOGIN`), containing zero GM/admin bits.
    pub const ROLE_PLAYER_DEFAULT: u64 = ROLE_PLAYER | ROLE_LOGIN;
    /// Mask of elevated GM / Admin role bits (`ROLE_GML | ROLE_GMH | ROLE_GMS | ROLE_ADMIN`).
    pub const ROLE_GM_MASK: u64 = ROLE_GML | ROLE_GMH | ROLE_GMS | ROLE_ADMIN;
    /// Default provisioned GM account role mask.
    pub const ROLE_GM_DEFAULT: u64 = ROLE_PLAYER_DEFAULT | ROLE_GM_MASK;
}

#[cfg(test)]
mod tests {
    use super::*;
    use eve_proto::marshal;

    #[test]
    fn test_version_exchange_and_address_marshal_roundtrip() {
        let ver = VersionExchange::default().to_pyrep();
        let encoded = marshal::encode(&ver);
        let decoded = marshal::decode(&encoded).unwrap();
        assert_eq!(ver, decoded);

        let addr = MachoAddress::Node {
            node_id: 888444,
            service: Some("stationSvc".to_string()),
            call_id: 42,
        }
        .to_pyrep();
        let addr_bytes = marshal::encode(&addr);
        let addr_decoded = marshal::decode(&addr_bytes).unwrap();
        assert_eq!(addr, addr_decoded);
    }

    // parity: plan:S-9
    #[test]
    fn test_session_starts_unauthenticated_and_computes_diffs() {
        let mut s1 = SessionState::new(7);
        assert!(!s1.is_authenticated());
        assert_eq!(s1.user_id, None);
        assert_eq!(s1.role, 0);
        assert!(!s1.has_gm_role());

        let mut s2 = s1.clone();
        s2.user_id = Some(1001);
        s2.character_id = Some(140000001);
        s2.station_id = Some(60003760);
        s2.solar_system_id = Some(30000142);
        let diff = s1.compute_diff(&s2);
        assert!(diff.contains_key("userid"));
        assert!(diff.contains_key("charid"));
        assert!(diff.contains_key("stationid"));
        assert!(diff.contains_key("solarsystemid2"));
        assert!(!diff.contains_key("solarsystemid")); // Docked in station, so solarsystemid remains None

        // Undock into space: stationid becomes None, solarsystemid becomes Some(30000142)
        let mut s3 = s2.clone();
        s3.station_id = None;
        let undock_diff = s2.compute_diff(&s3);
        assert!(undock_diff.contains_key("stationid"));
        assert!(undock_diff.contains_key("solarsystemid"));

        // Verify CCP 64-bit role constants from serviceConst.pyj
        assert_eq!(service_roles::ROLE_PLAYER_DEFAULT & service_roles::ROLE_GM_MASK, 0);
        assert_eq!(service_roles::ROLE_GML, 0x0040_0000_0000_0000);
        assert_eq!(service_roles::ROLE_GMH, 0x0020_0000_0000_0000);
        assert_eq!(service_roles::ROLE_CONTENT, 0x0080_0000_0000_0000);
        assert_eq!(service_roles::ROLE_ADMIN, 0x0100_0000_0000_0000);
        s1.role = service_roles::ROLE_PLAYER_DEFAULT;
        assert!(!s1.has_gm_role());
        s1.role |= service_roles::ROLE_GML;
        assert!(s1.has_gm_role());
    }
}
