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
    /// Defaults to `evemu_Crucible/src/eve-common/EVEVersion.h` (Crucible v1.6.5 build 360229).
    fn default() -> Self {
        Self {
            birthday: 170472,
            macho_version: 320,
            user_count: 1,
            version_number: 7.31,
            build_version: 360229,
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

/// Tracks a connected player's `machoNet` session attributes (`userid`, `charid`, `solarsystemid2`, `stationid`, etc.).
/// New sessions start strictly unauthenticated (`user_id: None`, `role: 0`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionState {
    pub session_id: u64,
    pub user_id: Option<u64>,
    pub character_id: Option<i64>,
    pub corporation_id: Option<i64>,
    pub alliance_id: Option<i64>,
    pub station_id: Option<i64>,
    pub solar_system_id: Option<u32>,
    pub constellation_id: Option<u32>,
    pub region_id: Option<u32>,
    pub ship_id: Option<i64>,
    pub role: u64,
    pub language_id: String,
}

impl SessionState {
    pub fn new(session_id: u64) -> Self {
        Self {
            session_id,
            user_id: None,
            role: 0,
            language_id: "EN".to_string(),
            ..Default::default()
        }
    }

    pub fn is_authenticated(&self) -> bool {
        self.user_id.is_some()
    }
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

    #[test]
    fn test_session_starts_unauthenticated() {
        let session = SessionState::new(7);
        assert!(!session.is_authenticated());
        assert_eq!(session.user_id, None);
        assert_eq!(session.role, 0);
    }
}
