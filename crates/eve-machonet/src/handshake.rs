//! Native V24.01 `machoNet` / `GPSTransport` low-level handshake state machine (`P-6`).
//!
//! Implements the full multi-step wire handshake defined by V24.01 `carbon/common/script/net/GPS.pyj`:
//! 1. `VersionExchange` (`LowLevelVersionExchange` server hello + `VersionExchangeClient` validation)
//! 2. `HandshakeCommand` (`(None, "QC")` queue check, `(None, "VK", vip_key)` VIP key, or `CryptoRequestPacket`)
//! 3. `CryptoRequestPacket` -> `"OK CC"`
//! 4. `CryptoChallengePacket` -> credential / SSO token verification -> `passwordVersion (2)` + `CryptoServerHandshake`
//! 5. `CryptoHandshakeResult` -> `CryptoHandshakeAck` + `SessionInitialStateNotification` + `SessionChangeNotification`

use crate::{MachoPacket, SessionState, VersionExchange};
use eve_proto::PyRep;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Default marshaled function code sent in `CryptoServerHandshake` (`None` token `b"\x74\x04\x00\x00\x00None"`).
pub const DEFAULT_HANDSHAKE_FUNC_CODE: &[u8] = b"\x74\x04\x00\x00\x00None";

/// Default handshake challenge padding length (64 zero bytes in Placebo mode).
pub const DEFAULT_HANDSHAKE_PADDING_LEN: usize = 64;

/// Default `machoNet` proxy node ID advertised during the `GPSTransport` handshake.
pub const MACHO_PROXY_NODE_ID: u64 = 888_444;

/// Standard CRC-CCITT (`binascii.crc_hqx`) with polynomial `0x1021`.
pub fn crc_hqx(data: &[u8], mut crc: u16) -> u16 {
    for &b in data {
        crc ^= (b as u16) << 8;
        for _ in 0..8 {
            if (crc & 0x8000) != 0 {
                crc = (crc << 1) ^ 0x1021;
            } else {
                crc <<= 1;
            }
        }
    }
    crc
}

/// Computes V24 `evecrypto/placebo.pyj` `crypto_hash(challenge)`:
/// `str(binascii.crc_hqx(blue.marshal.Save((challenge,)), 0))`.
///
/// The CRC is over the exact bytes `blue.marshal.Save` produces, so this builds them directly:
/// header `0x7E` + `u32` save count 0, `PyOneTuple` (`0x25`), then the `str` as `PyLongString`
/// (`0x13`, size-ex length, raw bytes). Captured real-client handshake frames show blue encoding
/// its 64-byte challenge as `0x13 0x40 …`, not the `0x10` short-string form.
pub fn placebo_crypto_hash(challenge: &[u8]) -> String {
    let mut saved = vec![0x7E, 0, 0, 0, 0, 0x25];
    match challenge.len() {
        0 => saved.push(0x0E),
        1 => {
            saved.push(0x0F);
            saved.push(challenge[0]);
        }
        len => {
            saved.push(0x13);
            if len < 0xFF {
                saved.push(len as u8);
            } else {
                saved.push(0xFF);
                saved.extend_from_slice(&(len as u32).to_le_bytes());
            }
            saved.extend_from_slice(challenge);
        }
    }
    crc_hqx(&saved, 0).to_string()
}

/// `exceptions.GPSTransportClosed` wire packet sent before closing a transport on handshake/auth/kick failure.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GPSTransportClosed {
    pub reason: String,
    pub reason_code: Option<String>,
    pub reason_args: HashMap<String, PyRep>,
    pub macho_version: u32,
    pub version: f64,
    pub build: u32,
    pub codename: String,
    pub region: String,
    pub logged_on_user_count: u32,
}

impl GPSTransportClosed {
    pub fn new(reason: impl Into<String>, reason_code: impl Into<String>) -> Self {
        let ver = VersionExchange::default();
        Self {
            reason: reason.into(),
            reason_code: Some(reason_code.into()),
            reason_args: HashMap::new(),
            macho_version: ver.macho_version,
            version: ver.version_number,
            build: ver.build_version,
            codename: ver.codename().to_string(),
            region: ver.region().to_string(),
            logged_on_user_count: ver.user_count,
        }
    }

    pub fn with_arg(mut self, key: impl Into<String>, val: PyRep) -> Self {
        self.reason_args.insert(key.into(), val);
        self
    }

    pub fn to_pyrep(&self) -> PyRep {
        let args_dict = PyRep::Dict(
            self.reason_args
                .iter()
                .map(|(k, v)| (PyRep::ascii_bytes(k), v.clone()))
                .collect(),
        );
        let state_dict = PyRep::Dict(vec![
            (
                PyRep::ascii_bytes("reason"),
                PyRep::ascii_bytes(&self.reason),
            ),
            (
                PyRep::ascii_bytes("reasonCode"),
                self.reason_code
                    .as_ref()
                    .map(PyRep::ascii_bytes)
                    .unwrap_or(PyRep::None),
            ),
            (PyRep::ascii_bytes("reasonArgs"), args_dict.clone()),
            (
                PyRep::ascii_bytes("machoVersion"),
                PyRep::Integer(self.macho_version as i64),
            ),
            (PyRep::ascii_bytes("version"), PyRep::Real(self.version)),
            (
                PyRep::ascii_bytes("build"),
                PyRep::Integer(self.build as i64),
            ),
            (
                PyRep::ascii_bytes("codename"),
                PyRep::ascii_bytes(&self.codename),
            ),
            (
                PyRep::ascii_bytes("region"),
                PyRep::ascii_bytes(&self.region),
            ),
            (
                PyRep::ascii_bytes("loggedOnUserCount"),
                PyRep::Integer(self.logged_on_user_count as i64),
            ),
        ]);
        PyRep::Object {
            type_name: "exceptions.GPSTransportClosed".into(),
            args: Box::new(PyRep::Tuple(vec![
                PyRep::ascii_bytes(&self.reason),
                self.reason_code
                    .as_ref()
                    .map(PyRep::ascii_bytes)
                    .unwrap_or(PyRep::None),
                args_dict,
                state_dict,
            ])),
        }
    }

    pub fn from_pyrep(rep: &PyRep) -> Option<Self> {
        let (type_name, items) = match rep {
            PyRep::Object { type_name, args } => match args.as_ref() {
                PyRep::Tuple(items) => (type_name.as_str(), items),
                _ => return None,
            },
            PyRep::ObjectEx { header, .. } => match header.as_ref() {
                PyRep::Tuple(items) if items.len() >= 2 => {
                    let tn = items[0].as_str()?;
                    match &items[1] {
                        PyRep::Tuple(inner) => (tn, inner),
                        _ => (tn, items),
                    }
                }
                _ => return None,
            },
            _ => return None,
        };
        if !type_name.ends_with("GPSTransportClosed")
            && !type_name.ends_with("GPSRemoteTransportClosed")
        {
            return None;
        }
        let reason = items.first().and_then(|v| v.as_str())?.to_string();
        let reason_code = items
            .get(1)
            .and_then(|v| v.as_str())
            .map(ToOwned::to_owned);
        let mut reason_args = HashMap::new();
        if let Some(PyRep::Dict(entries)) = items.get(2) {
            for (k, v) in entries {
                if let Some(ks) = k.as_str() {
                    reason_args.insert(ks.to_string(), v.clone());
                }
            }
        }
        let ver = VersionExchange::default();
        Some(Self {
            reason,
            reason_code,
            reason_args,
            macho_version: ver.macho_version,
            version: ver.version_number,
            build: ver.build_version,
            codename: ver.codename().to_string(),
            region: ver.region().to_string(),
            logged_on_user_count: ver.user_count,
        })
    }
}

/// Client `CryptoRequestPacket` (`(key_version, key_params)`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CryptoRequestPacket {
    pub key_version: String,
    pub key_params: HashMap<String, PyRep>,
}

impl CryptoRequestPacket {
    pub fn placebo() -> Self {
        Self {
            key_version: "placebo".to_string(),
            key_params: HashMap::new(),
        }
    }

    pub fn to_pyrep(&self) -> PyRep {
        let dict = PyRep::Dict(
            self.key_params
                .iter()
                .map(|(k, v)| (PyRep::ascii_bytes(k), v.clone()))
                .collect(),
        );
        PyRep::Tuple(vec![PyRep::ascii_bytes(&self.key_version), dict])
    }

    pub fn from_pyrep(rep: &PyRep) -> Option<Self> {
        let PyRep::Tuple(items) = rep else {
            return None;
        };
        if items.len() != 2 {
            return None;
        }
        let key_version = items[0].as_str()?.to_string();
        let mut key_params = HashMap::new();
        match &items[1] {
            PyRep::Dict(entries) => {
                for (k, v) in entries {
                    if let Some(ks) = k.as_str() {
                        key_params.insert(ks.to_string(), v.clone());
                    }
                }
            }
            PyRep::None => {}
            _ => return None,
        }
        Some(Self {
            key_version,
            key_params,
        })
    }
}

/// Step 2 command / crypto context packet sent by the client after `VersionExchangeClient`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum HandshakeCommand {
    /// `(None, "QC")` — Logon queue position check.
    QueueCheck,
    /// `(None, "VK", vip_key)` — VIP login key submission.
    VipKey(String),
    /// `(key_version, key_params)` — Crypto context initialization request.
    CryptoRequest(CryptoRequestPacket),
}

impl HandshakeCommand {
    pub fn to_pyrep(&self) -> PyRep {
        match self {
            Self::QueueCheck => PyRep::Tuple(vec![PyRep::None, PyRep::ascii_bytes("QC")]),
            Self::VipKey(vk) => PyRep::Tuple(vec![
                PyRep::None,
                PyRep::ascii_bytes("VK"),
                PyRep::ascii_bytes(vk),
            ]),
            Self::CryptoRequest(req) => req.to_pyrep(),
        }
    }

    pub fn from_pyrep(rep: &PyRep) -> Option<Self> {
        let PyRep::Tuple(items) = rep else {
            return None;
        };
        if items.len() == 2 && items[0] == PyRep::None {
            if items[1].as_str() == Some("QC") {
                return Some(Self::QueueCheck);
            }
        }
        if items.len() == 3 && items[0] == PyRep::None && items[1].as_str() == Some("VK") {
            let vk = items[2].as_str().unwrap_or_default().to_string();
            return Some(Self::VipKey(vk));
        }
        CryptoRequestPacket::from_pyrep(rep).map(Self::CryptoRequest)
    }
}

/// Client `CryptoChallengePacket` (`(client_challenge, handshake_dict)`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CryptoChallengePacket {
    pub client_challenge: Vec<u8>,
    pub macho_version: u32,
    pub boot_version: f64,
    pub boot_build: u32,
    pub boot_codename: String,
    pub boot_region: String,
    pub user_name: Option<String>,
    pub user_password: Option<String>,
    pub user_password_hash: Option<Vec<u8>>,
    pub user_language_id: String,
    pub user_affiliate_id: u32,
    pub user_sso_token: Option<String>,
}

impl CryptoChallengePacket {
    pub fn new_password_login(username: impl Into<String>, password: impl Into<String>) -> Self {
        let ver = VersionExchange::default();
        Self {
            client_challenge: vec![0u8; DEFAULT_HANDSHAKE_PADDING_LEN],
            macho_version: ver.macho_version,
            boot_version: ver.version_number,
            boot_build: ver.build_version,
            boot_codename: ver.codename().to_string(),
            boot_region: ver.region().to_string(),
            user_name: Some(username.into()),
            user_password: Some(password.into()),
            user_password_hash: None,
            user_language_id: "EN".to_string(),
            user_affiliate_id: 0,
            user_sso_token: None,
        }
    }

    pub fn new_sso_login(sso_token: impl Into<String>) -> Self {
        let ver = VersionExchange::default();
        Self {
            client_challenge: vec![0u8; DEFAULT_HANDSHAKE_PADDING_LEN],
            macho_version: ver.macho_version,
            boot_version: ver.version_number,
            boot_build: ver.build_version,
            boot_codename: ver.codename().to_string(),
            boot_region: ver.region().to_string(),
            user_name: Some("EVE-SSO-CONNECTION".to_string()),
            user_password: None,
            user_password_hash: None,
            user_language_id: "EN".to_string(),
            user_affiliate_id: 0,
            user_sso_token: Some(sso_token.into()),
        }
    }

    pub fn to_pyrep(&self) -> PyRep {
        let pw_rep = match &self.user_password {
            Some(pw) => PyRep::Object {
                type_name: "util.PasswordString".into(),
                args: Box::new(PyRep::Tuple(vec![PyRep::String(pw.clone())])),
            },
            None => PyRep::None,
        };
        let pw_hash_rep = match &self.user_password_hash {
            Some(h) => PyRep::Bytes(h.clone()),
            None => PyRep::None,
        };
        let dict = PyRep::Dict(vec![
            (
                PyRep::ascii_bytes("macho_version"),
                PyRep::Integer(self.macho_version as i64),
            ),
            (
                PyRep::ascii_bytes("boot_version"),
                PyRep::Real(self.boot_version),
            ),
            (
                PyRep::ascii_bytes("boot_build"),
                PyRep::Integer(self.boot_build as i64),
            ),
            (
                PyRep::ascii_bytes("boot_codename"),
                PyRep::ascii_bytes(&self.boot_codename),
            ),
            (
                PyRep::ascii_bytes("boot_region"),
                PyRep::ascii_bytes(&self.boot_region),
            ),
            (
                PyRep::ascii_bytes("user_name"),
                self.user_name
                    .as_ref()
                    .map(|u| PyRep::String(u.clone()))
                    .unwrap_or(PyRep::None),
            ),
            (PyRep::ascii_bytes("user_password"), pw_rep),
            (PyRep::ascii_bytes("user_password_hash"), pw_hash_rep),
            (
                PyRep::ascii_bytes("user_languageid"),
                PyRep::String(self.user_language_id.clone()),
            ),
            (
                PyRep::ascii_bytes("user_affiliateid"),
                PyRep::Integer(self.user_affiliate_id as i64),
            ),
            (
                PyRep::ascii_bytes("user_sso_token"),
                self.user_sso_token
                    .as_ref()
                    .map(|t| PyRep::String(t.clone()))
                    .unwrap_or(PyRep::None),
            ),
        ]);
        PyRep::Tuple(vec![PyRep::Bytes(self.client_challenge.clone()), dict])
    }

    pub fn from_pyrep(rep: &PyRep) -> Option<Self> {
        let PyRep::Tuple(items) = rep else {
            return None;
        };
        if items.len() != 2 {
            return None;
        }
        let client_challenge = match &items[0] {
            PyRep::Bytes(b) | PyRep::Buffer(b) => b.clone(),
            PyRep::String(s) => s.as_bytes().to_vec(),
            _ => return None,
        };
        let PyRep::Dict(entries) = &items[1] else {
            return None;
        };
        let mut map: HashMap<String, &PyRep> = HashMap::new();
        for (k, v) in entries {
            if let Some(ks) = k.as_str() {
                map.insert(ks.to_string(), v);
            }
        }
        let macho_version = map.get("macho_version")?.as_i64()? as u32;
        let boot_version = match map.get("boot_version")? {
            PyRep::Real(r) => *r,
            PyRep::Integer(i) => *i as f64,
            _ => return None,
        };
        let boot_build = map.get("boot_build")?.as_i64()? as u32;
        let boot_codename = map.get("boot_codename")?.as_str()?.to_string();
        let boot_region = map.get("boot_region")?.as_str()?.to_string();
        let user_name = map
            .get("user_name")
            .and_then(|v| v.as_str())
            .map(ToOwned::to_owned);
        let user_password = map.get("user_password").and_then(|v| extract_password_str(v));
        let user_password_hash = map.get("user_password_hash").and_then(|v| match v {
            PyRep::Bytes(b) | PyRep::Buffer(b) => Some(b.clone()),
            PyRep::String(s) => Some(s.as_bytes().to_vec()),
            _ => None,
        });
        let user_language_id = map
            .get("user_languageid")
            .and_then(|v| v.as_str())
            .unwrap_or("EN")
            .to_string();
        let user_affiliate_id = map
            .get("user_affiliateid")
            .and_then(|v| v.as_i64())
            .unwrap_or(0) as u32;
        let user_sso_token = map
            .get("user_sso_token")
            .and_then(|v| v.as_str())
            .map(ToOwned::to_owned);

        Some(Self {
            client_challenge,
            macho_version,
            boot_version,
            boot_build,
            boot_codename,
            boot_region,
            user_name,
            user_password,
            user_password_hash,
            user_language_id,
            user_affiliate_id,
            user_sso_token,
        })
    }

    /// Mirrors `GPS.HandleClientAuthenticationStep3` version checks (region, codename, version,
    /// macho version, then `boot.build > request['boot_build']`).
    pub fn validate_version(&self, expected: &VersionExchange) -> Result<(), GPSTransportClosed> {
        if self.boot_region != expected.region() {
            return Err(GPSTransportClosed::new(
                format!(
                    "Handshake Failed - Boot Region Mismatch - Boot Region is {} but should be {}",
                    self.boot_region,
                    expected.region()
                ),
                "HANDSHAKE_INCOMPATIBLEREGION",
            ));
        }
        if self.boot_codename != expected.codename() {
            return Err(GPSTransportClosed::new(
                format!(
                    "Handshake Failed - Boot Code Name Mismatch - Boot Code Name is {} but should be {}",
                    self.boot_codename,
                    expected.codename()
                ),
                "HANDSHAKE_INCOMPATIBLERELEASE",
            ));
        }
        if (self.boot_version - expected.version_number).abs() > 1e-4 {
            return Err(GPSTransportClosed::new(
                format!(
                    "Handshake Failed - Boot Version Mismatch - Boot Version is {} but should be {}",
                    self.boot_version, expected.version_number
                ),
                "HANDSHAKE_INCOMPATIBLEVERSION",
            ));
        }
        if self.macho_version != expected.macho_version {
            return Err(GPSTransportClosed::new(
                format!(
                    "Handshake Failed - Macho Version Mismatch - Macho Version is {} but should be {}",
                    self.macho_version, expected.macho_version
                ),
                "HANDSHAKE_INCOMPATIBLEPROTOCOL",
            ));
        }
        if expected.build_version > self.boot_build {
            return Err(GPSTransportClosed::new(
                format!(
                    "Handshake Failed - Boot Build Mismatch - Boot Build is {} but should be at least {}",
                    self.boot_build, expected.build_version
                ),
                "HANDSHAKE_INCOMPATIBLEBUILD",
            ));
        }
        Ok(())
    }
}

fn extract_password_str(rep: &PyRep) -> Option<String> {
    match rep {
        PyRep::None => None,
        PyRep::String(s) => Some(s.clone()),
        PyRep::Bytes(b) | PyRep::Buffer(b) => String::from_utf8(b.clone()).ok(),
        PyRep::Object { args, .. } => match args.as_ref() {
            PyRep::Tuple(items) => items.first().and_then(extract_password_str),
            other => extract_password_str(other),
        },
        PyRep::ObjectEx { header, .. } => match header.as_ref() {
            PyRep::Tuple(items) if items.len() >= 2 => extract_password_str(&items[1]),
            _ => None,
        },
        PyRep::Tuple(items) => items.first().and_then(extract_password_str),
        _ => None,
    }
}

/// Server `CryptoServerHandshake` (`(server_challenge, (func_marshaled_code, verification), context, response_dict)`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CryptoServerHandshake {
    pub server_challenge: Vec<u8>,
    pub func_marshaled_code: Vec<u8>,
    pub verification: bool,
    pub context: HashMap<String, PyRep>,
    pub challenge_response_hash: String,
    pub macho_version: u32,
    pub boot_version: f64,
    pub boot_build: u32,
    pub boot_codename: String,
    pub boot_region: String,
    pub cluster_user_count: u32,
    pub proxy_node_id: u64,
    pub user_logon_queue_position: u32,
    pub config_vals: HashMap<String, PyRep>,
}

impl CryptoServerHandshake {
    pub fn for_client_challenge(
        client_challenge: &[u8],
        expected: &VersionExchange,
        proxy_node_id: u64,
        cluster_user_count: u32,
    ) -> Self {
        Self {
            server_challenge: vec![0u8; DEFAULT_HANDSHAKE_PADDING_LEN],
            func_marshaled_code: DEFAULT_HANDSHAKE_FUNC_CODE.to_vec(),
            verification: false,
            context: HashMap::new(),
            challenge_response_hash: placebo_crypto_hash(client_challenge),
            macho_version: expected.macho_version,
            boot_version: expected.version_number,
            boot_build: expected.build_version,
            boot_codename: expected.codename().to_string(),
            boot_region: expected.region().to_string(),
            cluster_user_count,
            proxy_node_id,
            user_logon_queue_position: 1,
            config_vals: HashMap::new(),
        }
    }

    pub fn to_pyrep(&self) -> PyRep {
        let context_dict = PyRep::Dict(
            self.context
                .iter()
                .map(|(k, v)| (PyRep::ascii_bytes(k), v.clone()))
                .collect(),
        );
        let config_dict = PyRep::Dict(
            self.config_vals
                .iter()
                .map(|(k, v)| (PyRep::ascii_bytes(k), v.clone()))
                .collect(),
        );
        let response_dict = PyRep::Dict(vec![
            (
                PyRep::ascii_bytes("challenge_responsehash"),
                PyRep::ascii_bytes(&self.challenge_response_hash),
            ),
            (
                PyRep::ascii_bytes("macho_version"),
                PyRep::Integer(self.macho_version as i64),
            ),
            (
                PyRep::ascii_bytes("boot_version"),
                PyRep::Real(self.boot_version),
            ),
            (
                PyRep::ascii_bytes("boot_build"),
                PyRep::Integer(self.boot_build as i64),
            ),
            (
                PyRep::ascii_bytes("boot_codename"),
                PyRep::ascii_bytes(&self.boot_codename),
            ),
            (
                PyRep::ascii_bytes("boot_region"),
                PyRep::ascii_bytes(&self.boot_region),
            ),
            (
                PyRep::ascii_bytes("cluster_usercount"),
                PyRep::Integer(self.cluster_user_count as i64),
            ),
            (
                PyRep::ascii_bytes("proxy_nodeid"),
                PyRep::Integer(self.proxy_node_id as i64),
            ),
            (
                PyRep::ascii_bytes("user_logonqueueposition"),
                PyRep::Integer(self.user_logon_queue_position as i64),
            ),
            (PyRep::ascii_bytes("config_vals"), config_dict),
        ]);
        PyRep::Tuple(vec![
            PyRep::Bytes(self.server_challenge.clone()),
            PyRep::Tuple(vec![
                PyRep::Bytes(self.func_marshaled_code.clone()),
                PyRep::Bool(self.verification),
            ]),
            context_dict,
            response_dict,
        ])
    }

    pub fn from_pyrep(rep: &PyRep) -> Option<Self> {
        let PyRep::Tuple(items) = rep else {
            return None;
        };
        if items.len() != 4 {
            return None;
        }
        let server_challenge = match &items[0] {
            PyRep::Bytes(b) | PyRep::Buffer(b) => b.clone(),
            PyRep::String(s) => s.as_bytes().to_vec(),
            _ => return None,
        };
        let PyRep::Tuple(func_tuple) = &items[1] else {
            return None;
        };
        if func_tuple.len() != 2 {
            return None;
        }
        let func_marshaled_code = match &func_tuple[0] {
            PyRep::Bytes(b) | PyRep::Buffer(b) => b.clone(),
            PyRep::String(s) => s.as_bytes().to_vec(),
            _ => return None,
        };
        let verification = match &func_tuple[1] {
            PyRep::Bool(b) => *b,
            PyRep::Integer(i) => *i != 0,
            _ => false,
        };
        let mut context = HashMap::new();
        if let PyRep::Dict(entries) = &items[2] {
            for (k, v) in entries {
                if let Some(ks) = k.as_str() {
                    context.insert(ks.to_string(), v.clone());
                }
            }
        }
        let PyRep::Dict(resp_entries) = &items[3] else {
            return None;
        };
        let mut rmap: HashMap<String, &PyRep> = HashMap::new();
        for (k, v) in resp_entries {
            if let Some(ks) = k.as_str() {
                rmap.insert(ks.to_string(), v);
            }
        }
        let challenge_response_hash = rmap.get("challenge_responsehash")?.as_str()?.to_string();
        let macho_version = rmap.get("macho_version")?.as_i64()? as u32;
        let boot_version = match rmap.get("boot_version")? {
            PyRep::Real(r) => *r,
            PyRep::Integer(i) => *i as f64,
            _ => return None,
        };
        let boot_build = rmap.get("boot_build")?.as_i64()? as u32;
        let boot_codename = rmap.get("boot_codename")?.as_str()?.to_string();
        let boot_region = rmap.get("boot_region")?.as_str()?.to_string();
        let cluster_user_count = rmap
            .get("cluster_usercount")
            .and_then(|v| v.as_i64())
            .unwrap_or(1) as u32;
        let proxy_node_id = rmap
            .get("proxy_nodeid")
            .and_then(|v| v.as_i64())
            .unwrap_or(888444) as u64;
        let user_logon_queue_position = rmap
            .get("user_logonqueueposition")
            .and_then(|v| v.as_i64())
            .unwrap_or(1) as u32;
        let mut config_vals = HashMap::new();
        if let Some(PyRep::Dict(c_entries)) = rmap.get("config_vals") {
            for (k, v) in c_entries {
                if let Some(ks) = k.as_str() {
                    config_vals.insert(ks.to_string(), v.clone());
                }
            }
        }
        Some(Self {
            server_challenge,
            func_marshaled_code,
            verification,
            context,
            challenge_response_hash,
            macho_version,
            boot_version,
            boot_build,
            boot_codename,
            boot_region,
            cluster_user_count,
            proxy_node_id,
            user_logon_queue_position,
            config_vals,
        })
    }
}

/// Client `CryptoHandshakeResult` (`(challenge_responsehash, func_output, func_result)`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CryptoHandshakeResult {
    pub challenge_response_hash: String,
    pub func_output: Vec<u8>,
    pub func_result: PyRep,
}

impl CryptoHandshakeResult {
    pub fn for_server_challenge(server_challenge: &[u8]) -> Self {
        Self {
            challenge_response_hash: placebo_crypto_hash(server_challenge),
            func_output: Vec::new(),
            func_result: PyRep::None,
        }
    }

    pub fn to_pyrep(&self) -> PyRep {
        PyRep::Tuple(vec![
            PyRep::ascii_bytes(&self.challenge_response_hash),
            PyRep::Bytes(self.func_output.clone()),
            self.func_result.clone(),
        ])
    }

    pub fn from_pyrep(rep: &PyRep) -> Option<Self> {
        let PyRep::Tuple(items) = rep else {
            return None;
        };
        if items.len() != 3 {
            return None;
        }
        let challenge_response_hash = items[0].as_str()?.to_string();
        let func_output = match &items[1] {
            PyRep::Bytes(b) | PyRep::Buffer(b) => b.clone(),
            PyRep::String(s) => s.as_bytes().to_vec(),
            _ => Vec::new(),
        };
        let func_result = items[2].clone();
        Some(Self {
            challenge_response_hash,
            func_output,
            func_result,
        })
    }
}

/// Authenticated account identity bound during Step 3 of the `machoNet` handshake.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandshakeIdentity {
    pub user_id: u64,
    pub role: u64,
    pub language_id: String,
    pub user_type: i64,
    pub address: String,
    pub access_token: Option<String>,
    pub computer_hash: Option<String>,
}

/// Server `CryptoHandshakeAck` dictionary sent when `CryptoHandshakeResult` succeeds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CryptoHandshakeAck {
    pub session_id: u64,
    pub user_id: u64,
    pub role: u64,
    pub language_id: String,
    pub user_type: i64,
    pub max_session_time: Option<i64>,
    pub address: String,
    pub in_detention: Option<PyRep>,
    pub client_id: u64,
    pub client_hash: Option<String>,
    pub live_updates: Vec<PyRep>,
    pub access_token: Option<String>,
    pub computer_hash: Option<String>,
}

impl CryptoHandshakeAck {
    pub fn from_identity(session_id: u64, client_id: u64, id: &HandshakeIdentity) -> Self {
        Self {
            session_id,
            user_id: id.user_id,
            role: id.role,
            language_id: id.language_id.clone(),
            user_type: id.user_type,
            max_session_time: None,
            address: id.address.clone(),
            in_detention: None,
            client_id,
            client_hash: None,
            live_updates: Vec::new(),
            access_token: id.access_token.clone(),
            computer_hash: id.computer_hash.clone(),
        }
    }

    pub fn to_pyrep(&self) -> PyRep {
        let session_init = PyRep::Dict(vec![
            (
                PyRep::ascii_bytes("userid"),
                PyRep::Integer(self.user_id as i64),
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
                PyRep::ascii_bytes("maxSessionTime"),
                self.max_session_time
                    .map(PyRep::Integer)
                    .unwrap_or(PyRep::None),
            ),
            (
                PyRep::ascii_bytes("address"),
                PyRep::ascii_bytes(&self.address),
            ),
            (
                PyRep::ascii_bytes("inDetention"),
                self.in_detention.clone().unwrap_or(PyRep::None),
            ),
        ]);
        let mut top = vec![
            (
                PyRep::ascii_bytes("live_updates"),
                PyRep::List(self.live_updates.clone()),
            ),
            (PyRep::ascii_bytes("session_init"), session_init),
            (
                PyRep::ascii_bytes("sessionID"),
                PyRep::Integer(self.session_id as i64),
            ),
            (
                PyRep::ascii_bytes("client_hash"),
                self.client_hash
                    .as_ref()
                    .map(PyRep::ascii_bytes)
                    .unwrap_or(PyRep::None),
            ),
            (
                PyRep::ascii_bytes("user_clientid"),
                PyRep::Integer(self.client_id as i64),
            ),
        ];
        if let Some(tok) = &self.access_token {
            top.push((PyRep::ascii_bytes("access_token"), PyRep::ascii_bytes(tok)));
        }
        if let Some(ch) = &self.computer_hash {
            top.push((PyRep::ascii_bytes("computer_hash"), PyRep::ascii_bytes(ch)));
        } else if self.access_token.is_some() {
            top.push((PyRep::ascii_bytes("computer_hash"), PyRep::ascii_bytes("")));
        }
        PyRep::Dict(top)
    }

    pub fn from_pyrep(rep: &PyRep) -> Option<Self> {
        let PyRep::Dict(entries) = rep else {
            return None;
        };
        let mut top: HashMap<String, &PyRep> = HashMap::new();
        for (k, v) in entries {
            if let Some(ks) = k.as_str() {
                top.insert(ks.to_string(), v);
            }
        }
        let session_id = top.get("sessionID")?.as_i64()? as u64;
        let client_id = top.get("user_clientid")?.as_i64()? as u64;
        let PyRep::Dict(init_entries) = top.get("session_init")? else {
            return None;
        };
        let mut init: HashMap<String, &PyRep> = HashMap::new();
        for (k, v) in init_entries {
            if let Some(ks) = k.as_str() {
                init.insert(ks.to_string(), v);
            }
        }
        let user_id = init.get("userid")?.as_i64()? as u64;
        let role = init.get("role")?.as_i64()? as u64;
        let language_id = init
            .get("languageID")
            .and_then(|v| v.as_str())
            .unwrap_or("EN")
            .to_string();
        let user_type = init.get("userType").and_then(|v| v.as_i64()).unwrap_or(30);
        let max_session_time = init.get("maxSessionTime").and_then(|v| v.as_i64());
        let address = init
            .get("address")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let in_detention = init
            .get("inDetention")
            .and_then(|v| if **v == PyRep::None { None } else { Some((*v).clone()) });
        let client_hash = top
            .get("client_hash")
            .and_then(|v| v.as_str())
            .map(ToOwned::to_owned);
        let live_updates = match top.get("live_updates") {
            Some(PyRep::List(items)) => items.clone(),
            _ => Vec::new(),
        };
        let access_token = top
            .get("access_token")
            .and_then(|v| v.as_str())
            .map(ToOwned::to_owned);
        let computer_hash = top
            .get("computer_hash")
            .and_then(|v| v.as_str())
            .map(ToOwned::to_owned);

        Some(Self {
            session_id,
            user_id,
            role,
            language_id,
            user_type,
            max_session_time,
            address,
            in_detention,
            client_id,
            client_hash,
            live_updates,
            access_token,
            computer_hash,
        })
    }
}

/// Phase of the native `machoNet` low-level handshake state machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum HandshakePhase {
    AwaitingVersionExchange,
    AwaitingCommandOrCrypto,
    AwaitingCryptoChallenge,
    AwaitingHandshakeResult,
    Authenticated,
    Failed(String),
}

/// Output produced by a step of `MachoHandshakeStateMachine`.
#[derive(Debug, Clone)]
pub enum HandshakeStepOutput {
    /// Send zero or more marshaled `PyRep` frames and continue waiting for the next client handshake frame.
    SendFramesAndContinue(Vec<PyRep>),
    /// Client sent `CryptoChallengePacket`; caller must authenticate credentials or SSO token and invoke `complete_challenge`.
    NeedsAuthentication(CryptoChallengePacket),
    /// Handshake is now fully authenticated; send these marshaled `PyRep` frames and transition to authenticated RPC dispatch.
    Authenticated {
        frames: Vec<PyRep>,
        identity: HandshakeIdentity,
    },
}

/// Stateful V24.01 `machoNet` / `GPSTransport` connection handshake machine.
#[derive(Debug, Clone)]
pub struct MachoHandshakeStateMachine {
    pub expected_version: VersionExchange,
    pub phase: HandshakePhase,
    pub proxy_node_id: u64,
    pub session_id: u64,
    pub client_id: u64,
    pub vip_key: Option<String>,
    password_version_1_requested: bool,
    pending_server_challenge: Vec<u8>,
    pending_identity: Option<HandshakeIdentity>,
}

impl MachoHandshakeStateMachine {
    pub fn new(
        expected_version: VersionExchange,
        proxy_node_id: u64,
        session_id: u64,
        client_id: u64,
    ) -> Self {
        Self {
            expected_version,
            phase: HandshakePhase::AwaitingVersionExchange,
            proxy_node_id,
            session_id,
            client_id,
            vip_key: None,
            password_version_1_requested: false,
            pending_server_challenge: vec![0u8; DEFAULT_HANDSHAKE_PADDING_LEN],
            pending_identity: None,
        }
    }

    pub fn is_authenticated(&self) -> bool {
        self.phase == HandshakePhase::Authenticated
    }

    /// Initial server `LowLevelVersionExchange` frame written immediately on TCP accept.
    pub fn server_hello(&self) -> PyRep {
        self.expected_version.to_server_hello_pyrep()
    }

    /// Processes an incoming client `PyRep` frame according to the current handshake phase.
    /// Explicitly rejects unauthenticated `AuthenticationReq("ok")` stubs or premature `MachoPacket`s.
    pub fn step(&mut self, frame: &PyRep) -> Result<HandshakeStepOutput, GPSTransportClosed> {
        // Reject any legacy `AuthenticationReq("ok")` or premature `MachoPacket` before `Authenticated`.
        if let Some(pkt) = MachoPacket::from_pyrep(frame) {
            let reason = format!(
                "Unauthenticated MachoPacket ({:?}) rejected before completing GPSTransport handshake",
                pkt.packet_type
            );
            self.phase = HandshakePhase::Failed(reason.clone());
            return Err(GPSTransportClosed::new(reason, "HANDSHAKE_REQUIRED"));
        }

        match &self.phase {
            HandshakePhase::AwaitingVersionExchange => {
                let Some(client_ver) = VersionExchange::from_pyrep(frame) else {
                    let reason = "Expected 6-tuple VersionExchangeClient".to_string();
                    self.phase = HandshakePhase::Failed(reason.clone());
                    return Err(GPSTransportClosed::new(
                        reason,
                        "HANDSHAKE_INCOMPATIBLEPROTOCOL",
                    ));
                };
                if let Err(closed) = self.expected_version.validate_client(&client_ver) {
                    self.phase = HandshakePhase::Failed(closed.reason.clone());
                    return Err(closed);
                }
                self.phase = HandshakePhase::AwaitingCommandOrCrypto;
                Ok(HandshakeStepOutput::SendFramesAndContinue(Vec::new()))
            }
            HandshakePhase::AwaitingCommandOrCrypto => {
                let Some(cmd) = HandshakeCommand::from_pyrep(frame) else {
                    let reason = "Expected (None, 'QC'), (None, 'VK', key), or CryptoRequestPacket"
                        .to_string();
                    self.phase = HandshakePhase::Failed(reason.clone());
                    return Err(GPSTransportClosed::new(
                        reason,
                        "HANDSHAKE_INCOMPATIBLEPROTOCOL",
                    ));
                };
                match cmd {
                    // `GPS.HandleClientAuthenticationStep2` answers `(None, 'QC')` with the logon queue
                    // position and returns; the transport then re-enters `HandleClientAuthentication`,
                    // which writes the server version tuple again. The V24 client relies on this: after
                    // `machoNet.GetServerStatus` it caches the half-open transport and
                    // `ConnectToServer` -> `GPS.Authenticate` starts by reading that version tuple.
                    HandshakeCommand::QueueCheck => {
                        self.phase = HandshakePhase::AwaitingVersionExchange;
                        Ok(HandshakeStepOutput::SendFramesAndContinue(vec![
                            PyRep::Integer(1),
                            self.server_hello(),
                        ]))
                    }
                    HandshakeCommand::VipKey(vk) => {
                        self.vip_key = Some(vk);
                        Ok(HandshakeStepOutput::SendFramesAndContinue(Vec::new()))
                    }
                    HandshakeCommand::CryptoRequest(req) => {
                        if req.key_version != "placebo" {
                            let reason = format!(
                                "Handshake Failed - Public Key Mismatch - Public Key Version is {} but should be placebo",
                                req.key_version
                            );
                            self.phase = HandshakePhase::Failed(reason.clone());
                            return Err(GPSTransportClosed::new(
                                reason,
                                "HANDSHAKE_INCOMPATIBLEPUBLICKEY",
                            ));
                        }
                        // `evecrypto.cryptoapi` sends `getattr(macho, 'publicKeyVersion', 'placebo')`
                        // too, but its params carry the session key RSA-encrypted with CCP's key.
                        // Every later frame is then AES-encrypted with a key this server cannot
                        // recover, so refuse up front instead of replying "OK CC".
                        if req.key_params.contains_key("crypting_sessionkey") {
                            let reason = "Handshake Failed - client uses the CryptoAPI crypto pack; \
                                          this server only speaks Placebo (set cryptoPack = Placebo)"
                                .to_string();
                            self.phase = HandshakePhase::Failed(reason.clone());
                            return Err(GPSTransportClosed::new(
                                reason,
                                "HANDSHAKE_INCOMPATIBLEPUBLICKEY",
                            ));
                        }
                        self.phase = HandshakePhase::AwaitingCryptoChallenge;
                        Ok(HandshakeStepOutput::SendFramesAndContinue(vec![
                            PyRep::ascii_bytes("OK CC"),
                        ]))
                    }
                }
            }
            HandshakePhase::AwaitingCryptoChallenge => {
                let Some(challenge) = CryptoChallengePacket::from_pyrep(frame) else {
                    let reason = "Expected CryptoChallengePacket (client_challenge, handshake_dict)"
                        .to_string();
                    self.phase = HandshakePhase::Failed(reason.clone());
                    return Err(GPSTransportClosed::new(
                        reason,
                        "HANDSHAKE_FAILEDVERIFICATION",
                    ));
                };
                if let Err(closed) = challenge.validate_version(&self.expected_version) {
                    self.phase = HandshakePhase::Failed(closed.reason.clone());
                    return Err(closed);
                }
                // In V24 `GPSTransport.Authenticate`, when `passwordVersion == 2` initially on a password
                // login, Attempt 1 sends `user_password = None` and `user_sso_token = None`, then reads
                // `passwordVersion` from the server. If the server replies `1`, the client sends Attempt 2
                // with `user_password` populated and then reads `CryptoServerHandshake` directly.
                if !self.password_version_1_requested
                    && challenge.user_password.is_none()
                    && challenge.user_sso_token.is_none()
                    && challenge
                        .user_name
                        .as_deref()
                        .map(|u| !u.is_empty() && u != "EVE-SSO-CONNECTION")
                        .unwrap_or(false)
                {
                    self.password_version_1_requested = true;
                    return Ok(HandshakeStepOutput::SendFramesAndContinue(vec![
                        PyRep::Integer(1),
                    ]));
                }
                Ok(HandshakeStepOutput::NeedsAuthentication(challenge))
            }
            HandshakePhase::AwaitingHandshakeResult => {
                let Some(result) = CryptoHandshakeResult::from_pyrep(frame) else {
                    let reason =
                        "Expected 3-tuple CryptoHandshakeResult (hash, output, result)".to_string();
                    self.phase = HandshakePhase::Failed(reason.clone());
                    return Err(GPSTransportClosed::new(
                        reason,
                        "HANDSHAKE_FAILEDVERIFICATION",
                    ));
                };
                let expected_hash = placebo_crypto_hash(&self.pending_server_challenge);
                if result.challenge_response_hash != expected_hash {
                    let reason = format!(
                        "Client challenge response hash mismatch: got {}, expected {}",
                        result.challenge_response_hash, expected_hash
                    );
                    self.phase = HandshakePhase::Failed(reason.clone());
                    return Err(GPSTransportClosed::new(
                        reason,
                        "HANDSHAKE_FAILEDVERIFICATION",
                    ));
                }
                let Some(identity) = self.pending_identity.take() else {
                    let reason = "Missing authenticated identity in AwaitingHandshakeResult".into();
                    self.phase = HandshakePhase::Failed(reason);
                    return Err(GPSTransportClosed::new(
                        "Missing authenticated identity",
                        "HANDSHAKE_FAILEDVERIFICATION",
                    ));
                };
                self.phase = HandshakePhase::Authenticated;

                let ack = CryptoHandshakeAck::from_identity(
                    self.session_id,
                    self.client_id,
                    &identity,
                );
                let mut sess = SessionState::new(self.session_id);
                sess.user_id = Some(identity.user_id);
                sess.role = identity.role;
                sess.language_id = identity.language_id.clone();
                sess.address = identity.address.clone();
                sess.user_type = identity.user_type;

                let init_notif =
                    sess.to_initial_session_notification(self.proxy_node_id, self.client_id);
                let change_notif =
                    sess.to_login_session_change_packet(self.proxy_node_id, self.client_id);

                Ok(HandshakeStepOutput::Authenticated {
                    frames: vec![ack.to_pyrep(), init_notif.to_pyrep(), change_notif.to_pyrep()],
                    identity,
                })
            }
            HandshakePhase::Authenticated => Err(GPSTransportClosed::new(
                "Handshake already completed",
                "HANDSHAKE_ALREADY_COMPLETE",
            )),
            HandshakePhase::Failed(reason) => {
                Err(GPSTransportClosed::new(reason.clone(), "HANDSHAKE_FAILED"))
            }
        }
    }

    /// Completes Step 3 after `NeedsAuthentication` has been verified by the server against `WorldRepository`.
    /// On success, returns `[PyRep::Integer(2), CryptoServerHandshake]` (or `[CryptoServerHandshake]` if
    /// `passwordVersion = 1` was already sent on Attempt 1) and transitions to `AwaitingHandshakeResult`.
    pub fn complete_challenge(
        &mut self,
        challenge: &CryptoChallengePacket,
        auth_result: Result<HandshakeIdentity, GPSTransportClosed>,
        cluster_user_count: u32,
    ) -> Result<Vec<PyRep>, GPSTransportClosed> {
        let identity = match auth_result {
            Ok(id) => id,
            Err(closed) => {
                self.phase = HandshakePhase::Failed(closed.reason.clone());
                return Err(closed);
            }
        };
        let server_hs = CryptoServerHandshake::for_client_challenge(
            &challenge.client_challenge,
            &self.expected_version,
            self.proxy_node_id,
            cluster_user_count,
        );
        self.pending_server_challenge = server_hs.server_challenge.clone();
        self.pending_identity = Some(identity);
        self.phase = HandshakePhase::AwaitingHandshakeResult;
        if self.password_version_1_requested {
            Ok(vec![server_hs.to_pyrep()])
        } else {
            Ok(vec![PyRep::Integer(2), server_hs.to_pyrep()])
        }
    }
}
