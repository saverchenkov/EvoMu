//! `eve-machonet` — EVE Online `machoNet` packet framing, addressing, and session management.

#![forbid(unsafe_code)]

use eve_proto::PyRep;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub mod handshake;
pub use handshake::*;

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

/// `const.ADDRESS_TYPE_NODE` (V24 `carbon/common/lib/const.pyj`).
pub const ADDRESS_TYPE_NODE: i64 = 1;
/// `const.ADDRESS_TYPE_CLIENT`.
pub const ADDRESS_TYPE_CLIENT: i64 = 2;
/// `const.ADDRESS_TYPE_BROADCAST`.
pub const ADDRESS_TYPE_BROADCAST: i64 = 4;
/// `const.ADDRESS_TYPE_ANY`.
pub const ADDRESS_TYPE_ANY: i64 = 8;
/// Module path blue uses to unpickle `MachoAddress` on the V24 client (there is no `macho` module).
pub const MACHO_ADDRESS_CLASS: &str = "carbon.common.script.net.machoNetAddress.MachoAddress";
/// `session.SESSION_TYPE_GAME` (V24 `carbon/common/lib/session.pyj`).
pub const SESSION_TYPE_GAME: i64 = 5;

/// `cluster.MACHONETERR_WRAPPEDEXCEPTION`: `ErrorResponse.payload` is `(DumpsSanitized(exc),)`.
pub const MACHONETERR_WRAPPEDEXCEPTION: i64 = 2;

/// Body of a V24 `machoNetPacket.ErrorResponse` (`('originalCommand', 'code', 'payload')`) carrying a
/// wrapped `exceptions.RuntimeError`. The client's `ExceptionWrapperGPCS` re-raises
/// `machobase.LoadsSanitized(payload[0])`, so `payload[0]` is the marshal *string* of the
/// exception, pickled as blue does for `BaseException.__reduce__`: `ObjectEx1((cls, (msg,)))`.
pub fn wrapped_runtime_error_body(original_command: i64, message: &str) -> PyRep {
    let exc = PyRep::ObjectEx {
        is_type_2: false,
        header: Box::new(PyRep::Tuple(vec![
            PyRep::Token("exceptions.RuntimeError".into()),
            PyRep::Tuple(vec![PyRep::ascii_bytes(message)]),
        ])),
        list: Vec::new(),
        dict: Vec::new(),
    };
    PyRep::Tuple(vec![
        PyRep::Integer(original_command),
        PyRep::Integer(MACHONETERR_WRAPPEDEXCEPTION),
        PyRep::Tuple(vec![PyRep::Bytes(eve_proto::marshal::encode(&exc))]),
    ])
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
                PyRep::Integer(ADDRESS_TYPE_NODE),
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
                PyRep::Integer(ADDRESS_TYPE_CLIENT),
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
                PyRep::Integer(ADDRESS_TYPE_BROADCAST),
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
                PyRep::Integer(ADDRESS_TYPE_ANY),
                service
                    .as_ref()
                    .map(PyRep::ascii_bytes)
                    .unwrap_or(PyRep::None),
                PyRep::Integer(*call_id as i64),
            ],
        };
        PyRep::Object {
            type_name: MACHO_ADDRESS_CLASS.into(),
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
        // V24 `MachoAddress.__getstate__` leads with `const.ADDRESS_TYPE_*`; older captures use letters.
        let tag = match tuple.first()? {
            PyRep::Integer(ADDRESS_TYPE_NODE) => "N",
            PyRep::Integer(ADDRESS_TYPE_CLIENT) => "C",
            PyRep::Integer(ADDRESS_TYPE_BROADCAST) => "B",
            PyRep::Integer(ADDRESS_TYPE_ANY) => "A",
            other => other.as_str()?,
        };
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
            MachoPacketType::AuthenticationReq => "carbon.common.script.net.machoNetPacket.AuthenticationReq",
            MachoPacketType::AuthenticationRsp => "carbon.common.script.net.machoNetPacket.AuthenticationRsp",
            MachoPacketType::IdentificationReq => "carbon.common.script.net.machoNetPacket.IdentificationReq",
            MachoPacketType::IdentificationRsp => "carbon.common.script.net.machoNetPacket.IdentificationRsp",
            MachoPacketType::CallReq => "carbon.common.script.net.machoNetPacket.CallReq",
            MachoPacketType::CallRsp => "carbon.common.script.net.machoNetPacket.CallRsp",
            MachoPacketType::TransportClosed => "carbon.common.script.net.machoNetPacket.TransportClosed",
            MachoPacketType::ResolveReq => "carbon.common.script.net.machoNetPacket.ResolveReq",
            MachoPacketType::ResolveRsp => "carbon.common.script.net.machoNetPacket.ResolveRsp",
            MachoPacketType::Notification => "carbon.common.script.net.machoNetPacket.Notification",
            MachoPacketType::ErrorResponse => "carbon.common.script.net.machoNetPacket.ErrorResponse",
            MachoPacketType::SessionChangeNotification => "carbon.common.script.net.machoNetPacket.SessionChangeNotification",
            MachoPacketType::SessionInitialStateNotification => {
                "carbon.common.script.net.machoNetPacket.SessionInitialStateNotification"
            }
            MachoPacketType::PingReq => "carbon.common.script.net.machoNetPacket.PingReq",
            MachoPacketType::PingRsp => "carbon.common.script.net.machoNetPacket.PingRsp",
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
                // contextKey, journeyID, trace_id, parent_span_id, sampled, ingress_id,
                // sample_rate, cut_parent (V24 `MachoPacket.__getstate__` 14-tuple).
                PyRep::None,
                PyRep::None,
                PyRep::None,
                PyRep::None,
                PyRep::None,
                PyRep::None,
                PyRep::None,
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

    /// Builds a V24.01 server-to-client broadcast `MachoPacketType::Notification` (`packet_type = 12`)
    /// matching the 3-layer GPCS chain (`ObjectCall.NotifyDown` -> `ServiceCall.NotifyDown` ->
    /// `BroadcastStuff.NotifyDown` -> `NarrowcastByClientIDsWithoutTheStars`):
    /// `payload = (0, SubStream((0, (1, Tuple(args)))))`.
    pub fn new_broadcast_notification(
        node_id: u64,
        client_id: u64,
        user_id: Option<u64>,
        broadcast_id: impl Into<String>,
        args: Vec<PyRep>,
    ) -> Self {
        Self {
            packet_type: MachoPacketType::Notification,
            source: MachoAddress::Node {
                node_id,
                service: None,
                call_id: 0,
            },
            destination: MachoAddress::Broadcast {
                broadcast_id: broadcast_id.into(),
                id_type: "clientID".into(),
                narrowcast: vec![client_id],
            },
            user_id,
            payload: PyRep::Tuple(vec![PyRep::Tuple(vec![
                PyRep::Integer(0),
                PyRep::SubStream(Box::new(PyRep::Tuple(vec![
                    PyRep::Integer(0),
                    PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Tuple(args)]),
                ]))),
            ])]),
            named_payload: HashMap::new(),
        }
    }

    /// Extracts `(broadcast_id, args)` from a V24.01 GPCS `MachoPacketType::Notification` packet.
    pub fn extract_broadcast_notification(&self) -> Option<(&str, &[PyRep])> {
        if self.packet_type != MachoPacketType::Notification {
            return None;
        }
        let MachoAddress::Broadcast { broadcast_id, .. } = &self.destination else {
            return None;
        };
        let PyRep::Tuple(body) = &self.payload else {
            return None;
        };
        let PyRep::Tuple(outer) = body.first()? else {
            return None;
        };
        let PyRep::SubStream(sub) = outer.get(1)? else {
            return None;
        };
        let PyRep::Tuple(svc_layer) = sub.as_ref() else {
            return None;
        };
        let PyRep::Tuple(bcast_layer) = svc_layer.get(1)? else {
            return None;
        };
        let PyRep::Tuple(args) = bcast_layer.get(1)? else {
            return None;
        };
        Some((broadcast_id.as_str(), args.as_slice()))
    }
}

/// Low-level `VersionExchange` handshake parameters exchanged when a client connects on TCP 26000.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VersionExchange {
    pub birthday: u32,
    pub macho_version: u32,
    pub user_count: u32,
    pub version_number: f64,
    pub build_version: u32,
    pub project_version: String,
}

/// `GPSTransport.__HANDSHAKE_SECRET_KEY` in V24.01 `carbon/common/script/net/GPS.pyj`.
pub const HANDSHAKE_SECRET_KEY: u32 = 170_472;
/// `eveMachoNetVersion.machoVersion` in the installed V24.01 client
/// (`eve/common/script/net/eveMachoNetVersion.pyj`, re-exported as `machobase.version`).
pub const V24_MACHO_VERSION: u32 = 496;
/// `[main] build` of the installed V24.01 client's `start.ini` (`boot.build`).
pub const V24_BOOT_BUILD: u32 = 3_624_425;
/// `[main] codename` + `@` + `[main] region` of the installed V24.01 client's `start.ini`.
pub const V24_CODENAME_AT_REGION: &str = "V24.01@ccp";

impl Default for VersionExchange {
    /// Defaults to the boot values of the installed V24.01 client (`start.ini`: `version = 24.01`,
    /// `build = 3624425`, `codename = V24.01`, `region = ccp`) and `machoVersion = 496`.
    /// `GPS.Authenticate` on the client rejects any server whose codename/region, version or macho
    /// version differ, or whose build is newer than its own.
    fn default() -> Self {
        Self {
            birthday: HANDSHAKE_SECRET_KEY,
            macho_version: V24_MACHO_VERSION,
            user_count: 1,
            version_number: 24.01,
            build_version: V24_BOOT_BUILD,
            project_version: V24_CODENAME_AT_REGION.to_string(),
        }
    }
}

impl VersionExchange {
    /// `boot.codename` half of `project_version` (`"<codename>@<region>"`).
    pub fn codename(&self) -> &str {
        self.project_version
            .split_once('@')
            .map(|(c, _)| c)
            .unwrap_or(&self.project_version)
    }

    /// `boot.region` half of `project_version` (empty when there is no `@`, as in `GPS.pyj`).
    pub fn region(&self) -> &str {
        self.project_version
            .split_once('@')
            .map(|(_, r)| r)
            .unwrap_or("")
    }

    /// Server-initiated 7-tuple written on accept by `GPSTransport.HandleClientAuthentication`:
    /// `(secret, macho.version, loggedOnUserCount, boot.version, boot.build, codename@region,
    /// updateInfo)`. The client's `GPS.Authenticate` reads `response[6]` as `update_info`; it is
    /// `None` here because this server publishes no client code-hash updates
    /// (`machoNet.GetValidClientCodeHash` has no row).
    pub fn to_server_hello_pyrep(&self) -> PyRep {
        let PyRep::Tuple(mut items) = self.to_pyrep() else {
            unreachable!("VersionExchange::to_pyrep always yields a tuple")
        };
        items.push(PyRep::None);
        PyRep::Tuple(items)
    }

    /// Client-side 6-tuple reply (`GPS.Authenticate` `request`).
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

    pub fn from_pyrep(rep: &PyRep) -> Option<Self> {
        let PyRep::Tuple(items) = rep else {
            return None;
        };
        if items.len() < 6 || items.len() > 7 {
            return None;
        }
        let birthday = items[0].as_i64()? as u32;
        let macho_version = items[1].as_i64()? as u32;
        let user_count = match &items[2] {
            PyRep::Integer(i) if *i >= 0 => *i as u32,
            PyRep::None => 0,
            _ => return None,
        };
        let version_number = match &items[3] {
            PyRep::Real(r) => *r,
            PyRep::Integer(i) => *i as f64,
            _ => return None,
        };
        let build_version = items[4].as_i64()? as u32;
        let project_version = items[5].as_str()?.to_string();
        Some(Self {
            birthday,
            macho_version,
            user_count,
            version_number,
            build_version,
            project_version,
        })
    }
    pub fn validate_client(&self, client: &Self) -> Result<(), GPSTransportClosed> {
        if client.birthday != self.birthday {
            return Err(GPSTransportClosed::new(
                format!(
                    "Handshake Failed - Birthday Mismatch - Birthday is {} but should be {}",
                    client.birthday, self.birthday
                ),
                "HANDSHAKE_INCOMPATIBLERELEASE",
            ));
        }
        if client.macho_version != self.macho_version {
            return Err(GPSTransportClosed::new(
                format!(
                    "Handshake Failed - Macho Version Mismatch - Macho Version is {} but should be {}",
                    client.macho_version, self.macho_version
                ),
                "HANDSHAKE_INCOMPATIBLEPROTOCOL",
            ));
        }
        if (client.version_number - self.version_number).abs() > 1e-4 {
            return Err(GPSTransportClosed::new(
                format!(
                    "Handshake Failed - Version Mismatch - Version is {} but should be {}",
                    client.version_number, self.version_number
                ),
                "HANDSHAKE_INCOMPATIBLEVERSION",
            ));
        }
        // `GPS.HandleClientAuthenticationStep1`: `if boot.build > response[4]` -> incompatible build
        // (an older client is refused; a newer client build is accepted).
        if self.build_version > client.build_version {
            return Err(GPSTransportClosed::new(
                format!(
                    "Handshake Failed - Insecure Boot Build Mismatch - Client: {} - Server: {}",
                    client.build_version, self.build_version
                ),
                "HANDSHAKE_INCOMPATIBLEBUILD",
            ));
        }
        if client.region() != self.region() {
            return Err(GPSTransportClosed::new(
                format!(
                    "Handshake Failed - Insecure Region Code Mismatch - Client: {} - Server: {}",
                    client.region(),
                    self.region()
                ),
                "HANDSHAKE_INCOMPATIBLEREGION",
            ));
        }
        if client.codename() != self.codename() {
            return Err(GPSTransportClosed::new(
                format!(
                    "Handshake Failed - Insecure Boot Code Name Mismatch - Client: {} - Server: {}",
                    client.codename(),
                    self.codename()
                ),
                "HANDSHAKE_INCOMPATIBLERELEASE",
            ));
        }
        Ok(())
    }
}

fn parse_bound_oid_str(rep: &PyRep) -> Option<u64> {
    let s = rep.as_str()?;
    let (_prefix, id_str) = s.rsplit_once(':')?;
    id_str.parse::<u64>().ok().filter(|&id| id > 0)
}

/// Extracts `(bound_id, method, args, kwargs)` from a `macho.CallReq` packet payload (`PyRep`).
pub fn extract_macho_call_req(
    payload: &PyRep,
) -> (Option<u64>, String, Vec<PyRep>, HashMap<String, PyRep>) {
    let mut bound_id = None;
    let mut method = String::new();
    let mut args = Vec::new();
    let mut kwargs = HashMap::new();

    let outer = match payload {
        PyRep::Tuple(items) => items.as_slice(),
        _ => return (bound_id, method, args, kwargs),
    };
    let Some(first) = outer.first() else {
        return (bound_id, method, args, kwargs);
    };
    let mut inner_call = match first {
        PyRep::Tuple(t) if t.len() >= 2 => match &t[1] {
            PyRep::SubStream(s) => {
                // In V24 `ObjectCall`, `t[0]` is `0` (ServiceCall) or `1` (ObjectCall); only treat
                // `t[0] > 1` as an explicit numeric bound_id for synthetic test frames.
                bound_id = t[0].as_i64().filter(|&v| v > 1).map(|v| v as u64);
                s.as_ref()
            }
            other => {
                bound_id = t[0].as_i64().filter(|&v| v > 0).map(|v| v as u64);
                other
            }
        },
        PyRep::SubStream(s) => s.as_ref(),
        other => other,
    };
    if let PyRep::Tuple(t) = inner_call {
        if t.len() == 2 {
            if let PyRep::SubStream(s) = &t[1] {
                inner_call = s.as_ref();
            }
        }
    }

    if let PyRep::Tuple(call_items) = inner_call {
        // Support both 4-tuple `(remoteObject, method, args, kwargs)` and 3-tuple `(method, args, kwargs)`
        let offset = if let Some(oid) = call_items.first().and_then(parse_bound_oid_str) {
            bound_id = Some(oid);
            1
        } else if call_items.first().and_then(|v| v.as_str()).is_some() {
            0
        } else {
            1
        };
        if let Some(m) = call_items.get(offset).and_then(|v| v.as_str()) {
            method = m.to_owned();
        }
        if let Some(PyRep::Tuple(a) | PyRep::List(a)) = call_items.get(offset + 1) {
            args = a.clone();
        }
        if let Some(PyRep::Dict(entries)) = call_items.get(offset + 2) {
            for (k, v) in entries {
                if let Some(ks) = k.as_str() {
                    kwargs.insert(ks.to_owned(), v.clone());
                }
            }
        }
    }

    (bound_id, method, args, kwargs)
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
    #[serde(default)]
    pub corp_account_key: i32,
    pub fleet_id: Option<i64>,
    pub war_faction_id: Option<i64>,
    pub role: u64,
    pub language_id: String,
    pub address: String,
    pub user_type: i64,
    pub max_session_time: Option<i64>,
    #[serde(skip)]
    pub notifications: Vec<(String, Vec<PyRep>)>,
}

impl SessionState {
    pub fn new(session_id: u64) -> Self {
        Self {
            session_id,
            user_id: None,
            corp_role: 0,
            corp_account_key: 1000,
            role: 0,
            language_id: "EN".to_string(),
            address: String::new(),
            user_type: 30,
            max_session_time: None,
            notifications: Vec::new(),
            ..Default::default()
        }
    }

    pub fn push_notification(&mut self, event: impl Into<String>, args: Vec<PyRep>) {
        self.notifications.push((event.into(), args));
    }

    pub fn drain_notifications(&mut self) -> Vec<(String, Vec<PyRep>)> {
        std::mem::take(&mut self.notifications)
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
        if self.language_id != next.language_id {
            changes.insert(
                "languageID".into(),
                (
                    if self.language_id.is_empty() {
                        PyRep::None
                    } else {
                        PyRep::ascii_bytes(&self.language_id)
                    },
                    PyRep::ascii_bytes(&next.language_id),
                ),
            );
        }
        if self.address != next.address && !next.address.is_empty() {
            changes.insert(
                "address".into(),
                (
                    if self.address.is_empty() {
                        PyRep::None
                    } else {
                        PyRep::ascii_bytes(&self.address)
                    },
                    PyRep::ascii_bytes(&next.address),
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
        // `SessionChangeNotification` params: `('sid', 'change', 'nodesOfInterest?')`.
        Some(PyRep::Tuple(vec![
            PyRep::Integer(self.session_id as i64),
            PyRep::Tuple(vec![PyRep::Integer(0), PyRep::Dict(dict_entries)]),
            PyRep::List(nodes_of_interest.into_iter().map(PyRep::Integer).collect()),
        ]))
    }

    /// Builds `macho.SessionInitialStateNotification` (`packet_type = 18`) establishing initial session state (`userid`, `role`, `languageID`, `address`, `userType`, `sessionID`).
    pub fn to_initial_session_notification(&self, node_id: u64, client_id: u64) -> MachoPacket {
        let values = PyRep::Dict(vec![
            (
                PyRep::ascii_bytes("userid"),
                self.user_id
                    .map(|u| PyRep::Integer(u as i64))
                    .unwrap_or(PyRep::None),
            ),
            (
                PyRep::ascii_bytes("role"),
                PyRep::Integer(self.role as i64),
            ),
            (
                PyRep::ascii_bytes("languageID"),
                PyRep::ascii_bytes(&self.language_id),
            ),
            (
                PyRep::ascii_bytes("userType"),
                PyRep::Integer(self.user_type),
            ),
            (
                PyRep::ascii_bytes("address"),
                PyRep::ascii_bytes(&self.address),
            ),
            (
                PyRep::ascii_bytes("sessionID"),
                PyRep::Integer(self.session_id as i64),
            ),
        ]);
        let mut named_payload = HashMap::new();
        named_payload.insert("channel".into(), PyRep::ascii_bytes("sessionchange"));
        MachoPacket {
            packet_type: MachoPacketType::SessionInitialStateNotification,
            source: MachoAddress::Node {
                node_id,
                service: None,
                call_id: 0,
            },
            destination: MachoAddress::Client {
                client_id,
                call_id: 0,
                service: None,
            },
            user_id: self.user_id,
            // `SessionInitialStateNotification` params: `('sid', 'sessionType', 'initialstate')`.
            payload: PyRep::Tuple(vec![
                PyRep::Integer(self.session_id as i64),
                PyRep::Integer(SESSION_TYPE_GAME),
                values,
            ]),
            named_payload,
        }
    }

    /// Builds initial post-login `macho.SessionChangeNotification` (`packet_type = 16`) containing `(None, new_val)` for `userid`, `role`, `languageID`, `userType`, and `address`.
    pub fn to_login_session_change_packet(&self, node_id: u64, client_id: u64) -> MachoPacket {
        let mut base = SessionState::new(self.session_id);
        base.language_id.clear();
        base.address.clear();
        let mut diff = base.compute_diff(self);
        diff.insert(
            "userType".into(),
            (PyRep::None, PyRep::Integer(self.user_type)),
        );
        let dict_entries = diff
            .into_iter()
            .map(|(k, (old_v, new_v))| (PyRep::ascii_bytes(k), PyRep::Tuple(vec![old_v, new_v])))
            .collect();
        let mut named_payload = HashMap::new();
        named_payload.insert("channel".into(), PyRep::ascii_bytes("sessionchange"));
        MachoPacket {
            packet_type: MachoPacketType::SessionChangeNotification,
            source: MachoAddress::Node {
                node_id,
                service: None,
                call_id: 0,
            },
            destination: MachoAddress::Client {
                client_id,
                call_id: 0,
                service: None,
            },
            user_id: self.user_id,
            payload: PyRep::Tuple(vec![
                PyRep::Integer(self.session_id as i64),
                PyRep::Tuple(vec![PyRep::Integer(0), PyRep::Dict(dict_entries)]),
                PyRep::List(vec![PyRep::Integer(node_id as i64)]),
            ]),
            named_payload,
        }
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
    pub const ROLE_WORLDMOD: u64 = 0x0002_0000_0000_0000;
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
    /// Mask of elevated GM / Admin role bits (`ROLE_GML | ROLE_GMH | ROLE_GMS | ROLE_ADMIN | ROLE_WORLDMOD`).
    pub const ROLE_GM_MASK: u64 = ROLE_GML | ROLE_GMH | ROLE_GMS | ROLE_ADMIN | ROLE_WORLDMOD;
    /// Default provisioned GM account role mask.
    pub const ROLE_GM_DEFAULT: u64 = ROLE_PLAYER_DEFAULT | ROLE_GM_MASK;
}

#[cfg(test)]
mod tests {
    use super::*;
    use eve_proto::marshal;

    #[test]
    fn test_version_exchange_and_address_marshal_roundtrip() {
        let ver_struct = VersionExchange::default();
        let ver = ver_struct.to_pyrep();
        let encoded = marshal::encode(&ver);
        let decoded = marshal::decode(&encoded).unwrap();
        assert_eq!(ver, decoded);
        assert_eq!(VersionExchange::from_pyrep(&decoded), Some(ver_struct));
        assert_eq!(VersionExchange::from_pyrep(&PyRep::None), None);
        assert_eq!(VersionExchange::from_pyrep(&PyRep::Tuple(vec![PyRep::None; 5])), None);

        let addr = MachoAddress::Node {
            node_id: 888444,
            service: Some("stationSvc".to_string()),
            call_id: 42,
        }
        .to_pyrep();
        let addr_bytes = marshal::encode(&addr);
        let addr_decoded = marshal::decode(&addr_bytes).unwrap();
        assert_eq!(addr, addr_decoded);

        // Bound 4-tuple and unbound 3-tuple CallReq payload extraction
        let bound_payload = PyRep::Tuple(vec![PyRep::Tuple(vec![
            PyRep::Integer(77),
            PyRep::SubStream(Box::new(PyRep::Tuple(vec![
                PyRep::Integer(1),
                PyRep::String("GetSolarSystemInventory".into()),
                PyRep::Tuple(vec![PyRep::Integer(30000142)]),
                PyRep::Dict(vec![(PyRep::String("flag".into()), PyRep::Integer(4))]),
            ]))),
        ])]);
        let (b_id, method, args, kwargs) = extract_macho_call_req(&bound_payload);
        assert_eq!(b_id, Some(77));
        assert_eq!(method, "GetSolarSystemInventory");
        assert_eq!(args, vec![PyRep::Integer(30000142)]);
        assert_eq!(kwargs.get("flag"), Some(&PyRep::Integer(4)));

        let unbound_payload = PyRep::Tuple(vec![PyRep::SubStream(Box::new(PyRep::Tuple(vec![
            PyRep::String("MachoBindObject".into()),
            PyRep::List(vec![PyRep::Integer(30000142)]),
            PyRep::Dict(vec![]),
        ])))]);
        let (b_id2, method2, args2, kwargs2) = extract_macho_call_req(&unbound_payload);
        assert_eq!(b_id2, None);
        assert_eq!(method2, "MachoBindObject");
        assert_eq!(args2, vec![PyRep::Integer(30000142)]);
        assert!(kwargs2.is_empty());

        let empty_call = extract_macho_call_req(&PyRep::None);
        assert_eq!(empty_call.0, None);
        assert!(empty_call.1.is_empty());
    }

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
        assert_eq!(service_roles::ROLE_GMS, 0x0000_0040_0000_0000);
        assert_eq!(service_roles::ROLE_WORLDMOD, 0x0002_0000_0000_0000);
        assert_eq!(service_roles::ROLE_CONTENT, 0x0080_0000_0000_0000);
        assert_eq!(service_roles::ROLE_ADMIN, 0x0100_0000_0000_0000);
        s1.role = service_roles::ROLE_PLAYER_DEFAULT;
        assert!(!s1.has_gm_role());
        s1.role |= service_roles::ROLE_GML;
        assert!(s1.has_gm_role());
        s1.role = service_roles::ROLE_PLAYER_DEFAULT | service_roles::ROLE_WORLDMOD;
        assert!(s1.has_gm_role());
    }
}
