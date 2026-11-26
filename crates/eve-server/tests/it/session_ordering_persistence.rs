//! Integration & wire-contract tests for `GH #1085` (Session ordering & persistence):
//! 1. Deferred `shipid` on NPC station dock from space (`[shipID, None]` held until station
//!    hangar `invbroker` `List`/`ListByFlags`/`GetSelfInvItem` — including `"machoNet"` wire-service
//!    bound calls — `MachoBindObject` piggybacked `List`, or `350 ms` elapsed fallback — plus
//!    intervening `Undock` masking, failure guards, and Upwell structure dock `shipid` preservation).
//! 2. Hull `OnItemChange` before `shipid` `SessionChangeNotification` on docked ship swap
//!    (`Board`, `BoardStoredShip`, `LeaveShip`), keeping `#1070` capsule tombstone after
//!    `SessionChangeNotification`, in-space `SetState`/ballpark-before-`SessionChange`, and
//!    structure control (`locationid == solar_system_id`, `shipid == structure_id`).
//! 3. Transition cooldown `[boundID, readyAt]` (`7,000 ms` wall-clock) on `Undock` (`stationSvc`,
//!    `structureDocking`, `ship`) and `beyonce.CmdStargateJump` (unscaled by TiDi; distinct
//!    from `beyonce.SetSessionCheck`'s `10 s` timer).
//! 4. `SESSION_CHANGE_KEYS` allow-list matching V24.02 `code.ccp` `basesession.pyj`, plus
//!    `SessionChange::filtered()` / `build_filtered_session_change_packet()` debug logging,
//!    dropped-key atomic counter, and all-dropped frame suppression.
//! 5. Disconnect persistence guard: live ball `(position, velocity)` persisted & restored when
//!    undocked with live ball; missing live ball logs warning and preserves prior snapshot
//!    untouched without inventing `(0, 0, 0)` or station fallback; structure-docked state is
//!    never rewritten as station-docked.
//! 6. Reconnect `DoSimClockRebase` & ballpark bootstrap ordering on `SelectCharacterID` ->
//!    `beyonce.GetFormations` -> `beyonce.MachoBindObject`, plus `DestinyDrain::Attach`
//!    hoisting `DoSimClockRebase` before `DoDestinyUpdate(SetState)` and `CallRsp`.

use crate::common;
use eve_db::types::{InventoryFlag, InventoryItemRecord, UpwellStructureRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::{
    placebo_crypto_hash, session_change_dropped_keys_count, CryptoChallengePacket,
    CryptoHandshakeAck, CryptoHandshakeResult, CryptoRequestPacket, CryptoServerHandshake,
    MachoAddress, MachoPacket, MachoPacketType, SessionChange, SessionState, V24_BOOT_BUILD,
    V24_CODENAME_AT_REGION, V24_MACHO_VERSION, V24_VERSION_NUMBER, SESSION_CHANGE_KEYS,
};
use eve_proto::PyRep;
use eve_server::log_ring_buffer::{LogRingBuffer, LogRingLayer};
use eve_server::macho_listener::{
    build_filtered_session_change_packet, handle_macho_client, read_framed_pyrep,
    write_framed_pyrep,
};
use eve_server::outbound_sequencer::{sequence_with_events, OutFrame, RpcCompletion};
use eve_server::router::ServerContext;
use eve_server::services::serverops_cluster::set_system_tidi_factor;
use eve_server::sp6_lifecycle::{
    handle_character_logoff_or_disconnect_sync, reconnect_character_in_space,
};
use eve_testkit::client_harness::{extract_bound_id_from_wire_rep, ClientWireFrameKind};
use flate2::read::{DeflateDecoder, ZlibDecoder};
use glam::DVec3;
use std::collections::{BTreeSet, HashMap};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tracing_subscriber::layer::SubscriberExt;

const CONTAINER_HANGAR: i64 = 10_004;
const CONTAINER_STRUCTURE: i64 = 10_014;
const TRANSITION_COOLDOWN_TICKS: i64 = 7_000 * 10_000;

fn decode_session_change_map(wire_packet: &PyRep) -> HashMap<String, (PyRep, PyRep)> {
    let pkt = MachoPacket::from_pyrep(wire_packet).expect("SessionChangeNotification MachoPacket");
    assert_eq!(pkt.packet_type, MachoPacketType::SessionChangeNotification);
    let sc = SessionChange::from_pyrep(&pkt.payload).expect("SessionChange::from_pyrep");
    sc.changes.into_iter().collect()
}

fn collect_destiny_action_names(updates: &PyRep, out: &mut Vec<String>) {
    let items: Vec<&PyRep> = match updates {
        PyRep::List(v) | PyRep::Tuple(v) => v.iter().collect(),
        _ => return,
    };
    for entry in items {
        let PyRep::Tuple(pair) = entry else { continue };
        let Some(action) = pair.get(1) else { continue };
        let name_opt = match action {
            PyRep::Tuple(parts) => parts.first().and_then(|v| v.as_str()),
            _ => None,
        };
        if let Some(name) = name_opt {
            out.push(name.to_string());
        }
    }
}

fn find_item_change_idx(
    frames: &[eve_testkit::client_harness::ClientWireFrame],
    item_id: i64,
    loc_id: Option<i64>,
) -> Option<usize> {
    frames.iter().position(|f| {
        matches!(
            &f.kind,
            ClientWireFrameKind::Notification { event, args }
                if event == "OnItemChange"
                    && args.first().and_then(|r| r.dbrow_get("itemID")).and_then(|v| v.as_i64()) == Some(item_id)
                    && loc_id.is_none_or(|expected| {
                        args.first().and_then(|r| r.dbrow_get("locationID")).and_then(|v| v.as_i64())
                            == Some(expected)
                    })
        )
    })
}

async fn assert_set_ball_pos(
    sys_handle: &eve_server::system_actor::SystemHandle,
    ball_id: i64,
    pos: DVec3,
    label: &str,
) {
    assert!(
        sys_handle.set_ball_position(ball_id, pos).await,
        "set_ball_position failed: {label}"
    );
}

fn seed_owned_structure(
    ctx: &ServerContext,
    structure_id: i64,
    sys_id: u32,
    corp_id: i64,
    pos: DVec3,
) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            structure_id,
            UpwellStructureRecord {
                structure_id,
                type_id: 35_832, // Astrahus
                solar_system_id: sys_id,
                owner_corp_id: corp_id,
                name: format!("Citadel {structure_id}"),
                position: pos.to_array(),
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 100_000.0,
                armor_hp: 100_000.0,
                hull_hp: 100_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
    });
}

fn make_tcp_call_packet(
    client_id: u64,
    user_id: u64,
    call_id: u64,
    service: &str,
    bound_id: Option<u64>,
    method: &str,
    args: Vec<PyRep>,
) -> MachoPacket {
    MachoPacket {
        packet_type: MachoPacketType::CallReq,
        source: MachoAddress::Client {
            client_id,
            call_id,
            service: None,
        },
        destination: MachoAddress::Node {
            node_id: eve_machonet::MACHO_PROXY_NODE_ID,
            service: Some(service.to_string()),
            call_id,
        },
        user_id: Some(user_id),
        payload: PyRep::Tuple(vec![PyRep::Tuple(vec![
            PyRep::Integer(bound_id.unwrap_or(0) as i64),
            PyRep::SubStream(Box::new(PyRep::Tuple(vec![
                PyRep::ascii_bytes(method),
                PyRep::Tuple(args),
                PyRep::Dict(Vec::new()),
            ]))),
        ])]),
        named_payload: HashMap::new(),
    }
}

async fn tcp_connect_and_login(
    addr: std::net::SocketAddr,
    username: &str,
    password: &str,
) -> (TcpStream, u64, u64) {
    let mut s = TcpStream::connect(addr).await.expect("connect macho TCP");
    let _hello = read_framed_pyrep(&mut s).await.expect("read hello").expect("hello packet");
    let client_ver = PyRep::Tuple(vec![
        PyRep::Integer(170_472),
        PyRep::Integer(i64::from(V24_MACHO_VERSION)),
        PyRep::Integer(0),
        PyRep::Real(V24_VERSION_NUMBER),
        PyRep::Integer(i64::from(V24_BOOT_BUILD)),
        PyRep::ascii_bytes(V24_CODENAME_AT_REGION),
    ]);
    write_framed_pyrep(&mut s, &client_ver).await.unwrap();
    write_framed_pyrep(&mut s, &CryptoRequestPacket::placebo().to_pyrep())
        .await
        .unwrap();
    assert_eq!(
        read_framed_pyrep(&mut s).await.unwrap().unwrap().as_str(),
        Some("OK CC")
    );
    let chal = CryptoChallengePacket::new_password_login(username, password);
    write_framed_pyrep(&mut s, &chal.to_pyrep()).await.unwrap();
    assert_eq!(
        read_framed_pyrep(&mut s).await.unwrap().unwrap(),
        PyRep::Integer(2)
    );
    let srv_hs = CryptoServerHandshake::from_pyrep(
        &read_framed_pyrep(&mut s).await.unwrap().unwrap(),
    )
    .expect("CryptoServerHandshake");
    let hs_res = CryptoHandshakeResult {
        challenge_response_hash: placebo_crypto_hash(&srv_hs.server_challenge),
        func_output: Vec::new(),
        func_result: PyRep::Integer(0),
    };
    write_framed_pyrep(&mut s, &hs_res.to_pyrep()).await.unwrap();
    let ack = CryptoHandshakeAck::from_pyrep(&read_framed_pyrep(&mut s).await.unwrap().unwrap())
        .expect("CryptoHandshakeAck");
    let init_pkt = MachoPacket::from_pyrep(&read_framed_pyrep(&mut s).await.unwrap().unwrap())
        .expect("SessionInitialStateNotification");
    let MachoAddress::Client { client_id, .. } = init_pkt.destination else {
        panic!("expected Client destination on SessionInitialStateNotification");
    };
    let _login_sc = read_framed_pyrep(&mut s).await.unwrap().unwrap();
    (s, client_id, ack.user_id)
}

async fn tcp_drain_until_call_rsp(stream: &mut TcpStream) -> Vec<MachoPacket> {
    let mut pkts = Vec::new();
    loop {
        let rep = tokio::time::timeout(Duration::from_secs(10), read_framed_pyrep(stream))
            .await
            .expect("tcp read timeout")
            .expect("tcp read error")
            .expect("unexpected EOF reading macho packet");
        let pkt = MachoPacket::from_pyrep(&rep).expect("MachoPacket::from_pyrep");
        let is_reply = matches!(
            pkt.packet_type,
            MachoPacketType::CallRsp | MachoPacketType::ErrorResponse
        );
        pkts.push(pkt);
        if is_reply {
            return pkts;
        }
    }
}

fn find_code_ccp_path() -> PathBuf {
    let mut candidates = Vec::new();
    if let Ok(explicit) = std::env::var("EVOMU_CODE_CCP") {
        let trimmed = explicit.trim();
        if !trimmed.is_empty() {
            candidates.push(PathBuf::from(trimmed));
        }
    }
    if let Ok(sc) = std::env::var("EVOMU_SHARED_CACHE") {
        let trimmed = sc.trim();
        if !trimmed.is_empty() {
            let root = Path::new(trimmed);
            candidates.push(root.join("tq/EVE.app/Contents/Resources/build/code.ccp"));
            candidates.push(root.join("tq/code.ccp"));
            candidates.push(root.join("code.ccp"));
        }
    }
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        candidates.push(
            home.join("Library/Application Support/EVE Online/SharedCache/tq/EVE.app/Contents/Resources/build/code.ccp"),
        );
        candidates.push(home.join("Library/Application Support/EVE Online/SharedCache/tq/code.ccp"));
        candidates.push(home.join(".local/share/ccp/eve/SharedCache/tq/code.ccp"));
    }
    if let Ok(local_app) = std::env::var("LOCALAPPDATA") {
        candidates.push(PathBuf::from(local_app.trim()).join(r"CCP\EVE\SharedCache\tq\code.ccp"));
    }
    if let Ok(prog_data) = std::env::var("PROGRAMDATA") {
        candidates.push(PathBuf::from(prog_data.trim()).join(r"CCP\EVE\SharedCache\tq\code.ccp"));
    }
    candidates.push(PathBuf::from(r"C:\EVE\SharedCache\tq\code.ccp"));
    candidates
        .into_iter()
        .find(|p| p.is_file())
        .expect("V24.02 client code.ccp archive must exist locally in SharedCache or EVOMU_CODE_CCP")
}

fn read_zip_entry(zip_bytes: &[u8], target_name: &str) -> Vec<u8> {
    let min_tail = zip_bytes.len().saturating_sub(65_558);
    let eocd_off = (min_tail..=zip_bytes.len().saturating_sub(22))
        .rev()
        .find(|&i| &zip_bytes[i..i + 4] == b"PK\x05\x06")
        .expect("ZIP EOCD signature");
    let cd_entries = u16::from_le_bytes([zip_bytes[eocd_off + 10], zip_bytes[eocd_off + 11]]) as usize;
    let mut off = u32::from_le_bytes([
        zip_bytes[eocd_off + 16],
        zip_bytes[eocd_off + 17],
        zip_bytes[eocd_off + 18],
        zip_bytes[eocd_off + 19],
    ]) as usize;

    for _ in 0..cd_entries {
        assert_eq!(&zip_bytes[off..off + 4], b"PK\x01\x02", "ZIP CD entry signature");
        let method = u16::from_le_bytes([zip_bytes[off + 10], zip_bytes[off + 11]]);
        let comp_size = u32::from_le_bytes([
            zip_bytes[off + 20],
            zip_bytes[off + 21],
            zip_bytes[off + 22],
            zip_bytes[off + 23],
        ]) as usize;
        let name_len = u16::from_le_bytes([zip_bytes[off + 28], zip_bytes[off + 29]]) as usize;
        let extra_len = u16::from_le_bytes([zip_bytes[off + 30], zip_bytes[off + 31]]) as usize;
        let comment_len = u16::from_le_bytes([zip_bytes[off + 32], zip_bytes[off + 33]]) as usize;
        let lh_off = u32::from_le_bytes([
            zip_bytes[off + 42],
            zip_bytes[off + 43],
            zip_bytes[off + 44],
            zip_bytes[off + 45],
        ]) as usize;
        let name = std::str::from_utf8(&zip_bytes[off + 46..off + 46 + name_len]).unwrap_or("");
        if name == target_name {
            assert_eq!(&zip_bytes[lh_off..lh_off + 4], b"PK\x03\x04", "ZIP LFH signature");
            let lh_name_len =
                u16::from_le_bytes([zip_bytes[lh_off + 26], zip_bytes[lh_off + 27]]) as usize;
            let lh_extra_len =
                u16::from_le_bytes([zip_bytes[lh_off + 28], zip_bytes[lh_off + 29]]) as usize;
            let data_start = lh_off + 30 + lh_name_len + lh_extra_len;
            let comp_slice = &zip_bytes[data_start..data_start + comp_size];
            return match method {
                0 => comp_slice.to_vec(),
                8 => {
                    let mut out = Vec::new();
                    DeflateDecoder::new(comp_slice)
                        .read_to_end(&mut out)
                        .expect("deflate ZIP entry");
                    out
                }
                other => panic!("unsupported ZIP compression method {other} for {target_name}"),
            };
        }
        off += 46 + name_len + extra_len + comment_len;
    }
    panic!("entry {target_name} not found in code.ccp");
}

#[allow(dead_code)]
#[derive(Clone)]
enum Py2Obj {
    None,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Bytes(Vec<u8>),
    Seq(Vec<Py2Obj>),
    Dict(Vec<(Py2Obj, Py2Obj)>),
    Code(Py2Code),
}

#[derive(Clone)]
struct Py2Code {
    name: String,
    code: Vec<u8>,
    consts: Vec<Py2Obj>,
    names: Vec<String>,
    varnames: Vec<String>,
}

struct Py2MarshalReader<'a> {
    buf: &'a [u8],
    pos: usize,
    interned: Vec<Vec<u8>>,
}

impl<'a> Py2MarshalReader<'a> {
    fn u8(&mut self) -> u8 {
        let b = self.buf[self.pos];
        self.pos += 1;
        b
    }

    fn i32(&mut self) -> i32 {
        let b = &self.buf[self.pos..self.pos + 4];
        self.pos += 4;
        i32::from_le_bytes([b[0], b[1], b[2], b[3]])
    }

    fn u32(&mut self) -> u32 {
        let b = &self.buf[self.pos..self.pos + 4];
        self.pos += 4;
        u32::from_le_bytes([b[0], b[1], b[2], b[3]])
    }

    fn bytes(&mut self, n: usize) -> &'a [u8] {
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        s
    }

    fn as_text(raw: &[u8]) -> String {
        raw.iter().map(|&b| b as char).collect()
    }

    fn read_obj(&mut self) -> Py2Obj {
        match self.u8() {
            b'N' => Py2Obj::None,
            b'T' => Py2Obj::Bool(true),
            b'F' => Py2Obj::Bool(false),
            b'i' => Py2Obj::Int(i64::from(self.i32())),
            b'I' => {
                let b = self.bytes(8);
                Py2Obj::Int(i64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]))
            }
            b'f' => {
                let n = self.u8() as usize;
                let s = Self::as_text(self.bytes(n));
                Py2Obj::Float(s.parse::<f64>().unwrap_or(0.0))
            }
            b'g' => {
                let b = self.bytes(8);
                Py2Obj::Float(f64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]))
            }
            b's' => {
                let n = self.u32() as usize;
                Py2Obj::Bytes(self.bytes(n).to_vec())
            }
            b'u' => {
                let n = self.u32() as usize;
                Py2Obj::Str(String::from_utf8_lossy(self.bytes(n)).into_owned())
            }
            b't' => {
                let n = self.u32() as usize;
                let raw = self.bytes(n).to_vec();
                self.interned.push(raw.clone());
                Py2Obj::Bytes(raw)
            }
            b'R' => {
                let idx = self.i32() as usize;
                Py2Obj::Bytes(self.interned[idx].clone())
            }
            b'(' | b'[' => {
                let n = self.i32() as usize;
                let mut items = Vec::with_capacity(n);
                for _ in 0..n {
                    items.push(self.read_obj());
                }
                Py2Obj::Seq(items)
            }
            b'{' => {
                let mut pairs = Vec::new();
                while self.buf[self.pos] != b'0' {
                    let k = self.read_obj();
                    let v = self.read_obj();
                    pairs.push((k, v));
                }
                self.pos += 1;
                Py2Obj::Dict(pairs)
            }
            b'c' => {
                let _argcount = self.i32();
                let _nlocals = self.i32();
                let _stacksize = self.i32();
                let _flags = self.i32();
                let Py2Obj::Bytes(code) = self.read_obj() else {
                    panic!("expected code bytes");
                };
                let Py2Obj::Seq(consts) = self.read_obj() else {
                    panic!("expected consts tuple");
                };
                let names = match self.read_obj() {
                    Py2Obj::Seq(v) => v
                        .into_iter()
                        .map(|o| match o {
                            Py2Obj::Bytes(b) => Self::as_text(&b),
                            Py2Obj::Str(s) => s,
                            _ => String::new(),
                        })
                        .collect(),
                    _ => Vec::new(),
                };
                let varnames = match self.read_obj() {
                    Py2Obj::Seq(v) => v
                        .into_iter()
                        .map(|o| match o {
                            Py2Obj::Bytes(b) => Self::as_text(&b),
                            Py2Obj::Str(s) => s,
                            _ => String::new(),
                        })
                        .collect(),
                    _ => Vec::new(),
                };
                let _freevars = self.read_obj();
                let _cellvars = self.read_obj();
                let _filename = self.read_obj();
                let name = match self.read_obj() {
                    Py2Obj::Bytes(b) => Self::as_text(&b),
                    Py2Obj::Str(s) => s,
                    _ => String::new(),
                };
                let _firstlineno = self.i32();
                let _lnotab = self.read_obj();
                Py2Obj::Code(Py2Code {
                    name,
                    code,
                    consts,
                    names,
                    varnames,
                })
            }
            other => panic!("unexpected Python 2.7 marshal tag 0x{other:02x}"),
        }
    }
}

fn find_code_by_name<'a>(co: &'a Py2Code, target: &str) -> Option<&'a Py2Code> {
    if co.name == target {
        return Some(co);
    }
    for c in &co.consts {
        if let Py2Obj::Code(child) = c {
            if let Some(found) = find_code_by_name(child, target) {
                return Some(found);
            }
        }
    }
    None
}

#[derive(Clone)]
enum EvalVal {
    StrList(Vec<String>),
    Sym(String),
    Atom(String),
    Other,
}

fn eval_persist_and_additional_keys(
    co: &Py2Code,
    known_core_persist: Option<&[String]>,
) -> (Option<Vec<String>>, Option<Vec<String>>) {
    let mut stack: Vec<EvalVal> = Vec::new();
    let mut persist_vars = None;
    let mut additional_dist = None;
    let mut pc = 0usize;
    while pc < co.code.len() {
        let op = co.code[pc];
        pc += 1;
        let arg = if op >= 90 {
            let lo = usize::from(co.code[pc]);
            let hi = usize::from(co.code[pc + 1]);
            pc += 2;
            lo | (hi << 8)
        } else {
            0
        };
        match op {
            1 => {
                let _ = stack.pop();
            }
            23 => {
                let rhs = stack.pop().unwrap_or(EvalVal::Other);
                let lhs = stack.pop().unwrap_or(EvalVal::Other);
                match (lhs, rhs) {
                    (EvalVal::StrList(mut a), EvalVal::StrList(b)) => {
                        a.extend(b);
                        stack.push(EvalVal::StrList(a));
                    }
                    _ => stack.push(EvalVal::Other),
                }
            }
            90 => {
                let val = stack.pop().unwrap_or(EvalVal::Other);
                if co.names.get(arg).map(String::as_str) == Some("__persistvars__") {
                    if let EvalVal::StrList(list) = val {
                        persist_vars = Some(list);
                        break;
                    }
                }
            }
            95 => {
                let _obj = stack.pop();
                let val = stack.pop().unwrap_or(EvalVal::Other);
                if co.names.get(arg).map(String::as_str) == Some("additionalDistributedProps") {
                    if let EvalVal::StrList(list) = val {
                        additional_dist = Some(list);
                        break;
                    }
                }
            }
            100 => match co.consts.get(arg) {
                Some(Py2Obj::Bytes(b)) => stack.push(EvalVal::Atom(Py2MarshalReader::as_text(b))),
                Some(Py2Obj::Str(s)) => stack.push(EvalVal::Atom(s.clone())),
                _ => stack.push(EvalVal::Other),
            },
            101 | 116 => {
                let sym = co.names.get(arg).cloned().unwrap_or_default();
                stack.push(EvalVal::Sym(sym));
            }
            103 => {
                let start = stack.len().saturating_sub(arg);
                let drained = stack.drain(start..).collect::<Vec<_>>();
                let mut strs = Vec::with_capacity(drained.len());
                let mut all_str = true;
                for v in drained {
                    if let EvalVal::Atom(s) = v {
                        strs.push(s);
                    } else {
                        all_str = false;
                        break;
                    }
                }
                stack.push(if all_str {
                    EvalVal::StrList(strs)
                } else {
                    EvalVal::Other
                });
            }
            106 => {
                let base = stack.pop().unwrap_or(EvalVal::Other);
                let attr = co.names.get(arg).map(String::as_str).unwrap_or("");
                if matches!(base, EvalVal::Sym(ref s) if s == "CoreSession") && attr == "__persistvars__"
                {
                    if let Some(core) = known_core_persist {
                        stack.push(EvalVal::StrList(core.to_vec()));
                    } else {
                        stack.push(EvalVal::Other);
                    }
                } else {
                    stack.push(EvalVal::Other);
                }
            }
            124 => {
                let name = co.varnames.get(arg).cloned().unwrap_or_default();
                stack.push(EvalVal::Sym(name));
            }
            131 => {
                let pos_args = arg & 0xff;
                let kw_args = (arg >> 8) & 0xff;
                let pop_n = 1 + pos_args + 2 * kw_args;
                let keep = stack.len().saturating_sub(pop_n);
                stack.truncate(keep);
                stack.push(EvalVal::Other);
            }
            _ => break,
        }
    }
    (persist_vars, additional_dist)
}

fn extract_distributed_session_keys_from_code_ccp() -> BTreeSet<String> {
    let code_ccp = find_code_ccp_path();
    let archive_bytes = std::fs::read(&code_ccp).expect("read code.ccp");
    let compressed_pyj = read_zip_entry(&archive_bytes, "carbon/common/script/sys/basesession.pyj");
    let mut pyc_bytes = Vec::new();
    ZlibDecoder::new(compressed_pyj.as_slice())
        .read_to_end(&mut pyc_bytes)
        .expect("zlib decompress basesession.pyj");
    assert_eq!(
        &pyc_bytes[..4],
        b"\x03\xf3\x0d\x0a",
        "basesession.pyj must decompress to a Python 2.7 .pyc header"
    );
    let mut reader = Py2MarshalReader {
        buf: &pyc_bytes[8..],
        pos: 0,
        interned: Vec::new(),
    };
    let Py2Obj::Code(module_co) = reader.read_obj() else {
        panic!("expected module code object in basesession.pyj");
    };
    let core_co = find_code_by_name(&module_co, "CoreSession").expect("CoreSession code object");
    let (core_persist_opt, _) = eval_persist_and_additional_keys(core_co, None);
    let core_persist = core_persist_opt.expect("CoreSession.__persistvars__");

    let session_co = find_code_by_name(&module_co, "Session").expect("Session code object");
    let (session_persist_opt, _) = eval_persist_and_additional_keys(session_co, Some(&core_persist));
    let session_persist = session_persist_opt.expect("Session.__persistvars__");

    let init_co = find_code_by_name(session_co, "__init__").expect("Session.__init__ code object");
    let (_, additional_opt) = eval_persist_and_additional_keys(init_co, Some(&core_persist));
    let additional = additional_opt.expect("Session.additionalDistributedProps");

    let mut keys = BTreeSet::new();
    for k in session_persist.into_iter().chain(additional.into_iter()) {
        keys.insert(k);
    }
    keys
}

#[tokio::test]
async fn test_1085_deferred_ship_id_on_dock_from_space_hangar_and_timeout() {
    // parity: feat:serverops.session-changes, feat:navigation.dock-station, feat:navigation.dock-structure, notify:OnSessionChanged, rpc:structureDocking.Dock
    eve_testkit::attest(&[
        "feat:serverops.session-changes",
        "feat:navigation.dock-station",
        "feat:navigation.dock-structure",
        "notify:OnSessionChanged",
        "rpc:structureDocking.Dock",
    ]);

    let ctx = Arc::new(ServerContext::new());
    common::pin_clock(&ctx);
    let kw = HashMap::new();
    let char_id = 90_000_001_i64;
    let sys_id = 30_000_142_u32;
    let station_id = 60_003_760_i64;
    let (ship_id, corp_id) = ctx
        .db
        .with_snapshot(|s| (s.characters[&char_id].active_ship_id, s.characters[&char_id].corporation_id));
    let station_pos = ctx.sde.stations[&station_id].position;

    let sess0 = SessionState::new(85_001);
    let mut harness = common::wrap_wire_harness(&ctx, sess0);
    let login_out = harness
        .call_exchange(
            "authentication",
            None,
            "Login",
            &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())],
            &kw,
        )
        .await;
    assert!(login_out.result.is_ok());

    // Initial login docked in station emits `shipid: [None, ship_id]` immediately (not deferred).
    let sel_out = harness
        .call_exchange("charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw)
        .await;
    assert!(sel_out.result.is_ok());
    let sel_sc = decode_session_change_map(&sel_out.session_change_packets[0]);
    assert_eq!(
        sel_sc.get("shipid"),
        Some(&(PyRep::None, PyRep::Integer(ship_id))),
        "initial station login must not defer shipid"
    );
    assert!(harness.transport().inner.deferred_ship_id.is_none());

    // Case 1: Station dock from space holds shipid=[ship_id, None], ignores ship inventory & errors,
    // and flushes shipid=[None, ship_id] AFTER station hangar invbroker.List CallRsp.
    ctx.advance_secs(12);
    harness.call_ok("ship", None, "Undock", &[], &kw).await;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    assert_set_ball_pos(
        &sys_handle,
        ship_id,
        station_pos + DVec3::new(1_000.0, 0.0, 0.0),
        "position ship inside Jita docking perimeter",
    )
    .await;

    let dock_out = harness
        .call_exchange("stationSvc", None, "Dock", &[PyRep::Integer(station_id)], &kw)
        .await;
    assert!(dock_out.result.is_ok());
    let dock_sc = decode_session_change_map(
        dock_out
            .session_change_packets
            .first()
            .expect("dock must emit SessionChangeNotification"),
    );
    assert_eq!(
        dock_sc.get("shipid"),
        Some(&(PyRep::Integer(ship_id), PyRep::None)),
        "station dock from space must initially clear shipid on wire"
    );
    assert_eq!(
        dock_sc.get("stationid"),
        Some(&(PyRep::None, PyRep::Integer(station_id)))
    );
    assert_eq!(
        harness.transport().inner.session.ship_id,
        Some(ship_id),
        "server session must retain active ship_id while wire shipid is deferred"
    );
    assert!(harness.transport().inner.deferred_ship_id.is_some());

    // Negative: ship inventory List & invalid bound List must NOT flush deferred shipid.
    let bind_station = harness
        .call_ok(
            "invbroker",
            None,
            "MachoBindObject",
            &[PyRep::Tuple(vec![PyRep::Integer(station_id), PyRep::Integer(15)]), PyRep::None],
            &kw,
        )
        .await;
    let mgr_bid = extract_bound_id_from_wire_rep(&bind_station).expect("station invbroker bound id");
    let ship_inv = harness
        .call_ok(
            "invbroker",
            Some(mgr_bid),
            "GetInventoryFromId",
            &[PyRep::Integer(ship_id), PyRep::Integer(0)],
            &kw,
        )
        .await;
    let ship_bid = extract_bound_id_from_wire_rep(&ship_inv).expect("ship inv bound id");
    let ship_list_out = harness
        .call_exchange("invbroker", Some(ship_bid), "List", &[], &kw)
        .await;
    assert!(ship_list_out.result.is_ok());
    assert!(
        ship_list_out.session_change_packets.is_empty(),
        "ship inventory List must not flush deferred shipid"
    );
    assert!(harness.transport().inner.deferred_ship_id.is_some());

    let bad_list_out = harness
        .call_exchange("invbroker", Some(999_999_999), "List", &[], &kw)
        .await;
    assert!(bad_list_out.result.is_err(), "bogus bound List must fail");
    assert!(harness.transport().inner.deferred_ship_id.is_some());

    // Positive: station hangar GetInventory(10_004) -> List() flushes {shipid: [None, ship_id]} AFTER CallRsp.
    let hangar_rep = harness
        .call_ok(
            "invbroker",
            Some(mgr_bid),
            "GetInventory",
            &[PyRep::Integer(CONTAINER_HANGAR), PyRep::None],
            &kw,
        )
        .await;
    let hangar_bid = extract_bound_id_from_wire_rep(&hangar_rep).expect("hangar bound id");
    let hangar_list_out = harness
        .call_exchange("invbroker", Some(hangar_bid), "List", &[], &kw)
        .await;
    assert!(hangar_list_out.result.is_ok());
    let rsp_idx = hangar_list_out
        .frames
        .iter()
        .position(|f| matches!(f.kind, ClientWireFrameKind::CallRsp { .. }))
        .expect("CallRsp on hangar List");
    let flush_idx = hangar_list_out
        .frames
        .iter()
        .position(|f| matches!(f.kind, ClientWireFrameKind::SessionChange { .. }))
        .expect("deferred SessionChange flush on hangar List");
    assert!(
        rsp_idx < flush_idx,
        "deferred shipid SessionChangeNotification (idx {flush_idx}) must follow hangar CallRsp (idx {rsp_idx})"
    );
    let flush_sc = decode_session_change_map(&hangar_list_out.session_change_packets[0]);
    assert_eq!(flush_sc.get("shipid"), Some(&(PyRep::None, PyRep::Integer(ship_id))));
    assert!(harness.transport().inner.deferred_ship_id.is_none());

    // Case 2: GetSelfInvItem on hangar bound object also flushes deferred shipid after CallRsp.
    ctx.advance_secs(12);
    harness.call_ok("ship", None, "Undock", &[], &kw).await;
    assert_set_ball_pos(
        &sys_handle,
        ship_id,
        station_pos + DVec3::new(1_000.0, 0.0, 0.0),
        "position near station",
    )
    .await;
    let dock2 = harness
        .call_exchange("stationSvc", None, "Dock", &[PyRep::Integer(station_id)], &kw)
        .await;
    assert!(dock2.result.is_ok());
    assert!(harness.transport().inner.deferred_ship_id.is_some());
    let self_out = harness
        .call_exchange("invbroker", Some(hangar_bid), "GetSelfInvItem", &[], &kw)
        .await;
    assert!(self_out.result.is_ok());
    let self_rsp_idx = self_out
        .frames
        .iter()
        .position(|f| matches!(f.kind, ClientWireFrameKind::CallRsp { .. }))
        .expect("CallRsp on GetSelfInvItem");
    let self_sc_idx = self_out
        .frames
        .iter()
        .position(|f| matches!(f.kind, ClientWireFrameKind::SessionChange { .. }))
        .expect("deferred SessionChange on GetSelfInvItem");
    assert!(self_rsp_idx < self_sc_idx);
    assert_eq!(
        decode_session_change_map(&self_out.session_change_packets[0]).get("shipid"),
        Some(&(PyRep::None, PyRep::Integer(ship_id)))
    );

    // Case 3: Upwell structure dock preserves shipid immediately (GH #1140 / #1141) without
    // arming deferred_ship_id, so godma.Prime -> MakeShipActive never sees shipID = None.
    let struct_id = 1_045_000_000_101_i64;
    let struct_pos = DVec3::new(200_000.0, 0.0, 0.0);
    seed_owned_structure(&ctx, struct_id, sys_id, corp_id, struct_pos);
    ctx.advance_secs(12);
    harness.call_ok("ship", None, "Undock", &[], &kw).await;
    assert_set_ball_pos(
        &sys_handle,
        ship_id,
        struct_pos + DVec3::new(1_500.0, 0.0, 0.0),
        "position near structure",
    )
    .await;
    let sdock_out = harness
        .call_exchange("structureDocking", None, "Dock", &[PyRep::Integer(struct_id)], &kw)
        .await;
    assert!(sdock_out.result.is_ok());
    let sdock_sc = decode_session_change_map(&sdock_out.session_change_packets[0]);
    assert_eq!(
        sdock_sc.get("structureid"),
        Some(&(PyRep::None, PyRep::Integer(struct_id)))
    );
    assert!(
        !sdock_sc.contains_key("shipid"),
        "Upwell structure dock must not clear/defer shipid: {sdock_sc:?}"
    );
    assert!(harness.transport().inner.deferred_ship_id.is_none());
    assert_eq!(harness.transport().inner.session.ship_id, Some(ship_id));

    let piggyback_out = harness
        .call_exchange(
            "invbroker",
            None,
            "MachoBindObject",
            &[
                PyRep::Tuple(vec![PyRep::Integer(struct_id), PyRep::Integer(15)]),
                PyRep::Tuple(vec![
                    PyRep::String("List".into()),
                    PyRep::Tuple(Vec::new()),
                    PyRep::Dict(Vec::new()),
                ]),
            ],
            &kw,
        )
        .await;
    assert!(piggyback_out.result.is_ok());
    assert!(
        piggyback_out.session_change_packets.is_empty(),
        "structure MachoBindObject(List) must not emit deferred shipid SessionChange"
    );

    // Case 4: Rapid intervening Undock within 350ms disarms pending timer and masks prev shipid -> [None, ship_id].
    ctx.advance_secs(12);
    harness.call_ok("structureDocking", None, "Undock", &[], &kw).await;
    assert_set_ball_pos(
        &sys_handle,
        ship_id,
        station_pos + DVec3::new(1_000.0, 0.0, 0.0),
        "position near station for rapid undock",
    )
    .await;
    harness.call_ok("stationSvc", None, "Dock", &[PyRep::Integer(station_id)], &kw).await;
    assert!(harness.transport().inner.deferred_ship_id.is_some());

    ctx.advance_secs(12);
    let rapid_undock = harness
        .call_exchange("ship", None, "Undock", &[], &kw)
        .await;
    assert!(rapid_undock.result.is_ok());
    assert!(harness.transport().inner.deferred_ship_id.is_none());
    assert_eq!(
        decode_session_change_map(&rapid_undock.session_change_packets[0]).get("shipid"),
        Some(&(PyRep::None, PyRep::Integer(ship_id))),
        "intervening Undock before 350ms flush must restore shipid from None -> ship_id"
    );
    tokio::time::sleep(Duration::from_millis(370)).await;
    assert!(
        harness.transport_mut().inner.poll_deferred_ship_id().0.is_empty(),
        "disarmed deferred timer must not fire after rapid Undock"
    );

    // Case 5: 350 ms elapsed fallback fires cleanly on ClientWireSession AND live TCP handle_macho_client.
    assert_set_ball_pos(
        &sys_handle,
        ship_id,
        station_pos + DVec3::new(1_000.0, 0.0, 0.0),
        "position near station for ClientWireSession 350ms timeout",
    )
    .await;
    harness.call_ok("stationSvc", None, "Dock", &[PyRep::Integer(station_id)], &kw).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        harness.transport_mut().inner.poll_deferred_ship_id().0.is_empty(),
        "deferred shipid must stay held before 350ms deadline"
    );
    tokio::time::sleep(Duration::from_millis(320)).await;
    let (due_frames, due_pkts) = harness.transport_mut().inner.poll_deferred_ship_id();
    assert_eq!(due_frames.len(), 1);
    assert_eq!(
        decode_session_change_map(&due_pkts[0]).get("shipid"),
        Some(&(PyRep::None, PyRep::Integer(ship_id)))
    );

    // Live TCP handle_macho_client 350ms timer test:
    drop(harness);
    let tcp_ctx = Arc::new(ServerContext::new());
    common::pin_clock(&tcp_ctx);
    let tcp_ship_id = tcp_ctx.db.with_snapshot(|s| s.characters[&char_id].active_ship_id);
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind tcp");
    let addr = listener.local_addr().unwrap();
    let srv_ctx = Arc::clone(&tcp_ctx);
    tokio::spawn(async move {
        if let Ok((stream, _)) = listener.accept().await {
            let _ = handle_macho_client(stream, srv_ctx).await;
        }
    });
    let (mut tcp_stream, cid, uid) = tcp_connect_and_login(addr, "demo", "demo-secret").await;
    write_framed_pyrep(
        &mut tcp_stream,
        &make_tcp_call_packet(
            cid,
            uid,
            1,
            "charUnboundMgr",
            None,
            "SelectCharacterID",
            vec![PyRep::Integer(char_id)],
        )
        .to_pyrep(),
    )
    .await
    .unwrap();
    let _ = tcp_drain_until_call_rsp(&mut tcp_stream).await;
    tcp_ctx.advance_secs(12);

    write_framed_pyrep(
        &mut tcp_stream,
        &make_tcp_call_packet(cid, uid, 2, "stationSvc", None, "Undock", Vec::new()).to_pyrep(),
    )
    .await
    .unwrap();
    let _ = tcp_drain_until_call_rsp(&mut tcp_stream).await;

    let tcp_sys = tcp_ctx.systems.get_or_spawn(sys_id);
    assert_set_ball_pos(
        &tcp_sys,
        tcp_ship_id,
        station_pos + DVec3::new(1_000.0, 0.0, 0.0),
        "position near station for TCP dock",
    )
    .await;
    write_framed_pyrep(
        &mut tcp_stream,
        &make_tcp_call_packet(
            cid,
            uid,
            3,
            "stationSvc",
            None,
            "Dock",
            vec![PyRep::Integer(station_id)],
        )
        .to_pyrep(),
    )
    .await
    .unwrap();
    let dock_pkts = tcp_drain_until_call_rsp(&mut tcp_stream).await;
    let tcp_dock_sc = dock_pkts
        .iter()
        .find(|p| p.packet_type == MachoPacketType::SessionChangeNotification)
        .expect("TCP dock SessionChangeNotification");
    let tcp_dock_map = SessionChange::from_pyrep(&tcp_dock_sc.payload)
        .expect("parse dock SessionChange")
        .changes
        .into_iter()
        .collect::<HashMap<_, _>>();
    assert_eq!(
        tcp_dock_map.get("shipid"),
        Some(&(PyRep::Integer(tcp_ship_id), PyRep::None))
    );

    let flush_pkt = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let raw = read_framed_pyrep(&mut tcp_stream)
                .await
                .expect("TCP read err")
                .expect("TCP deferred packet");
            let pkt = MachoPacket::from_pyrep(&raw).expect("flush MachoPacket");
            if pkt.packet_type == MachoPacketType::SessionChangeNotification {
                return pkt;
            }
        }
    })
    .await
    .expect("TCP 350ms deferred shipid flush timeout");
    let flush_map = SessionChange::from_pyrep(&flush_pkt.payload)
        .expect("parse flush SessionChange")
        .changes
        .into_iter()
        .collect::<HashMap<_, _>>();
    assert_eq!(
        flush_map.get("shipid"),
        Some(&(PyRep::None, PyRep::Integer(tcp_ship_id)))
    );
}

#[tokio::test]
async fn test_1140_1141_structure_dock_preserves_shipid_and_station_machonet_hangar_flush() {
    // parity: feat:serverops.session-changes, feat:navigation.dock-station, feat:navigation.dock-structure, notify:OnSessionChanged
    eve_testkit::attest(&[
        "feat:serverops.session-changes",
        "feat:navigation.dock-station",
        "feat:navigation.dock-structure",
        "notify:OnSessionChanged",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let kw = HashMap::new();
    let char_id = 90_000_001_i64;
    let sys_id = 30_000_142_u32;
    let station_id = 60_003_760_i64;
    let station_pos = ctx.sde.stations[&station_id].position;
    let (ship_id, corp_id) = ctx.db.with_snapshot(|s| {
        (
            s.characters[&char_id].active_ship_id,
            s.characters[&char_id].corporation_id,
        )
    });
    let struct_id = 1_045_000_000_199_i64;
    let struct_pos = DVec3::new(220_000.0, 0.0, 0.0);
    seed_owned_structure(&ctx, struct_id, sys_id, corp_id, struct_pos);

    let mut sess = SessionState::new(85_140);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let mut harness = common::wrap_wire_harness(&ctx, sess);
    harness
        .call_ok("charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw)
        .await;

    ctx.advance_secs(12);
    harness.call_ok("stationSvc", None, "Undock", &[], &kw).await;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    assert_set_ball_pos(
        &sys_handle,
        ship_id,
        struct_pos + DVec3::new(1_500.0, 0.0, 0.0),
        "position near Upwell structure",
    )
    .await;

    // Live V24 client invokes `stationSvc.Dock(struct_id)` when docking at an Upwell structure.
    let sdock_out = harness
        .call_exchange("stationSvc", None, "Dock", &[PyRep::Integer(struct_id)], &kw)
        .await;
    assert!(sdock_out.result.is_ok(), "stationSvc.Dock(struct_id) failed: {:?}", sdock_out.result);
    assert_eq!(sdock_out.session_change_packets.len(), 1);
    let sdock_sc = decode_session_change_map(&sdock_out.session_change_packets[0]);
    assert_eq!(
        sdock_sc.get("structureid"),
        Some(&(PyRep::None, PyRep::Integer(struct_id)))
    );
    assert!(
        !sdock_sc.contains_key("shipid"),
        "structure dock must keep shipid preserved immediately: {sdock_sc:?}"
    );
    assert!(harness.transport().inner.deferred_ship_id.is_none());
    assert_eq!(harness.transport().inner.session.ship_id, Some(ship_id));

    // Timer must never fire a delayed shipid change after structure dock.
    tokio::time::sleep(Duration::from_millis(370)).await;
    assert!(
        harness.transport_mut().inner.poll_deferred_ship_id().0.is_empty(),
        "structure dock must not schedule a deferred shipid flush"
    );

    // Client immediately primes dogmaIM via `GetAllInfo` on structure dock (`~7 ms` post-Dock).
    let dogma_bind = harness
        .call_ok(
            "dogmaIM",
            None,
            "MachoBindObject",
            &[
                PyRep::Tuple(vec![PyRep::Integer(i64::from(sys_id)), PyRep::Integer(15)]),
                PyRep::None,
            ],
            &kw,
        )
        .await;
    let dogma_bid = extract_bound_id_from_wire_rep(&dogma_bind).expect("dogmaIM bound id");
    let all_info = harness
        .call_ok(
            "machoNet",
            Some(dogma_bid),
            "GetAllInfo",
            &[PyRep::Bool(false), PyRep::Bool(true), PyRep::Bool(true)],
            &kw,
        )
        .await;
    assert!(
        all_info.dict_get("shipState").is_some_and(|v| !matches!(v, PyRep::None)),
        "expected non-None shipState in GetAllInfo: {all_info:?}"
    );
    assert!(
        all_info.dict_get("structureInfo").is_some_and(|v| !matches!(v, PyRep::None)),
        "expected non-None structureInfo in GetAllInfo: {all_info:?}"
    );
    assert!(
        all_info.dict_get("shipInfo").is_some_and(|v| !matches!(v, PyRep::None)),
        "expected non-None shipInfo in GetAllInfo: {all_info:?}"
    );

    // NPC station dock retains `#1085` deferred shipid and flushes on live-wire `machoNet` bound `List`.
    ctx.advance_secs(12);
    harness.call_ok("structureDocking", None, "Undock", &[], &kw).await;
    assert_set_ball_pos(
        &sys_handle,
        ship_id,
        station_pos + DVec3::new(1_000.0, 0.0, 0.0),
        "position near station for machoNet bound List flush",
    )
    .await;
    let st_dock = harness
        .call_exchange("stationSvc", None, "Dock", &[PyRep::Integer(station_id)], &kw)
        .await;
    assert!(st_dock.result.is_ok());
    assert!(harness.transport().inner.deferred_ship_id.is_some());

    let bind_hangar = harness
        .call_exchange(
            "invbroker",
            None,
            "MachoBindObject",
            &[
                PyRep::Integer(station_id),
                PyRep::Tuple(vec![
                    PyRep::String("GetInventory".into()),
                    PyRep::Tuple(vec![PyRep::Integer(CONTAINER_HANGAR), PyRep::None]),
                    PyRep::Dict(Vec::new()),
                ]),
            ],
            &kw,
        )
        .await;
    assert!(bind_hangar.result.is_ok());
    assert!(bind_hangar.session_change_packets.is_empty());
    assert!(harness.transport().inner.deferred_ship_id.is_some());
    let hangar_bid = extract_bound_id_from_wire_rep(
        bind_hangar.result.as_ref().expect("hangar MachoBindObject result"),
    )
    .expect("station hangar bound id");

    let list_out = harness
        .call_exchange("machoNet", Some(hangar_bid), "List", &[], &kw)
        .await;
    assert!(list_out.result.is_ok());
    assert!(harness.transport().inner.deferred_ship_id.is_none());
    let list_rsp_idx = list_out
        .frames
        .iter()
        .position(|f| matches!(f.kind, ClientWireFrameKind::CallRsp { .. }))
        .expect("CallRsp on machoNet bound hangar List");
    let list_sc_idx = list_out
        .frames
        .iter()
        .position(|f| matches!(f.kind, ClientWireFrameKind::SessionChange { .. }))
        .expect("deferred SessionChange after machoNet bound hangar List");
    assert!(list_rsp_idx < list_sc_idx);
    assert_eq!(
        decode_session_change_map(&list_out.session_change_packets[0]).get("shipid"),
        Some(&(PyRep::None, PyRep::Integer(ship_id)))
    );

    // NPC station dock also flushes on `invbroker.MachoBindObject((station_id, 15), ("List", ...))`.
    ctx.advance_secs(12);
    harness.call_ok("stationSvc", None, "Undock", &[], &kw).await;
    assert_set_ball_pos(
        &sys_handle,
        ship_id,
        station_pos + DVec3::new(1_000.0, 0.0, 0.0),
        "position near station for MachoBindObject piggyback flush",
    )
    .await;
    let st_dock_2 = harness
        .call_exchange("stationSvc", None, "Dock", &[PyRep::Integer(station_id)], &kw)
        .await;
    assert!(st_dock_2.result.is_ok());
    assert!(harness.transport().inner.deferred_ship_id.is_some());

    let pig_out = harness
        .call_exchange(
            "invbroker",
            None,
            "MachoBindObject",
            &[
                PyRep::Tuple(vec![PyRep::Integer(station_id), PyRep::Integer(15)]),
                PyRep::Tuple(vec![
                    PyRep::String("List".into()),
                    PyRep::Tuple(Vec::new()),
                    PyRep::Dict(Vec::new()),
                ]),
            ],
            &kw,
        )
        .await;
    assert!(pig_out.result.is_ok());
    assert!(harness.transport().inner.deferred_ship_id.is_none());
    let pig_rsp_idx = pig_out
        .frames
        .iter()
        .position(|f| matches!(f.kind, ClientWireFrameKind::CallRsp { .. }))
        .expect("CallRsp on station MachoBindObject(List)");
    let pig_sc_idx = pig_out
        .frames
        .iter()
        .position(|f| matches!(f.kind, ClientWireFrameKind::SessionChange { .. }))
        .expect("deferred SessionChange on station MachoBindObject(List)");
    assert!(pig_rsp_idx < pig_sc_idx);
    assert_eq!(
        decode_session_change_map(&pig_out.session_change_packets[0]).get("shipid"),
        Some(&(PyRep::None, PyRep::Integer(ship_id)))
    );
}

#[tokio::test]
async fn test_1085_hull_item_change_before_session_change_vs_space_and_structure_control() {
    // parity: feat:serverops.session-changes, feat:ships.board-ship, feat:ships.stored-ship-board, feat:structures.board-structure, rpc:ship.Board, rpc:ship.BoardStoredShip, rpc:ship.LeaveShip, rpc:ship.AssembleShip, rpc:structureControl.TakeControl, notify:OnItemChange, notify:OnSessionChanged, destiny:SetState
    eve_testkit::attest(&[
        "feat:serverops.session-changes",
        "feat:ships.board-ship",
        "feat:ships.stored-ship-board",
        "feat:structures.board-structure",
        "rpc:ship.Board",
        "rpc:ship.BoardStoredShip",
        "rpc:ship.LeaveShip",
        "rpc:ship.AssembleShip",
        "rpc:structureControl.TakeControl",
        "notify:OnItemChange",
        "notify:OnSessionChanged",
        "destiny:SetState",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let kw = HashMap::new();
    let char_id = 90_000_001_i64;
    let sys_id = 30_000_142_u32;
    let station_id = 60_003_760_i64;
    let carrier_id = 100_990_001_i64;
    let stored_frig_id = 100_990_002_i64;
    let stack_ship_id = 100_990_003_i64;
    let corp_id = 98_085_002_i64;
    let struct_id = 1_046_000_000_102_i64;
    let struct_pos = DVec3::new(150_000.0, 0.0, 0.0);

    seed_owned_structure(&ctx, struct_id, sys_id, corp_id, struct_pos);
    ctx.db.with_mut_snapshot(|snap| {
        let mut corp = eve_db::CorporationRecord::new(corp_id, "Session Order Corp", "SOC", char_id);
        corp.members.insert(char_id);
        corp.member_roles.insert(char_id, eve_db::corp_roles::ROLE_DIRECTOR);
        snap.corporations.insert(corp_id, corp);
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
            ch.clone_grade = 2;
        }
        let sk = snap.skills.entry(char_id).or_default();
        for tid in [3330_u32, 3334, 3327, 20315, 24311] {
            sk.insert(
                tid,
                eve_db::TrainedSkillRecord {
                    type_id: tid,
                    level: 5,
                    skill_points: 256_000.0,
                },
            );
        }
        snap.items.insert(
            carrier_id,
            InventoryItemRecord {
                item_id: carrier_id,
                type_id: 23_757, // Archon (carrier with Ship Maintenance Bay)
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            stored_frig_id,
            InventoryItemRecord {
                item_id: stored_frig_id,
                type_id: 606, // Velator
                owner_id: char_id,
                location_id: carrier_id,
                flag: InventoryFlag::ShipHangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            stack_ship_id,
            InventoryItemRecord {
                item_id: stack_ship_id,
                type_id: 606, // Velator stack (packaged)
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 2,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    let mut harness = common::wrap_wire_harness(&ctx, SessionState::new(85_002));
    harness
        .call_ok(
            "authentication",
            None,
            "Login",
            &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())],
            &kw,
        )
        .await;
    harness
        .call_ok("charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw)
        .await;

    // 1. AssembleShip emits OnItemChange for assembled hull; docked Board emits NO target-hull OnItemChange (#931 site #7).
    let asm_out = harness
        .call_exchange("ship", None, "AssembleShip", &[PyRep::Integer(stack_ship_id)], &kw)
        .await;
    let assembled_id = asm_out
        .result
        .expect("AssembleShip must succeed")
        .dict_get("itemID")
        .and_then(|v| v.as_i64())
        .expect("assembled itemID");
    assert!(
        find_item_change_idx(&asm_out.frames, assembled_id, Some(station_id)).is_some(),
        "AssembleShip must emit OnItemChange for assembled hull"
    );

    ctx.advance_secs(12);
    let board_out = harness
        .call_exchange("ship", None, "Board", &[PyRep::Integer(assembled_id)], &kw)
        .await;
    assert!(board_out.result.is_ok());
    assert!(
        find_item_change_idx(&board_out.frames, assembled_id, None).is_none(),
        "docked Board must not emit synthetic target-hull OnItemChange (#931 site #7)"
    );
    assert!(
        board_out
            .frames
            .iter()
            .any(|f| matches!(f.kind, ClientWireFrameKind::SessionChange { .. })),
        "SessionChange on Board"
    );

    // 2. LeaveShip in station emits spawned capsule OnItemChange BEFORE SessionChangeNotification.
    ctx.advance_secs(12);
    let leave_out = harness.call_exchange("ship", None, "LeaveShip", &[], &kw).await;
    let capsule_id = leave_out
        .result
        .expect("LeaveShip must succeed")
        .as_i64()
        .expect("capsule id");
    let cap_ic = find_item_change_idx(&leave_out.frames, capsule_id, Some(station_id))
        .expect("LeaveShip must emit spawned capsule OnItemChange");
    let leave_sc = leave_out
        .frames
        .iter()
        .position(|f| matches!(f.kind, ClientWireFrameKind::SessionChange { .. }))
        .expect("SessionChange on LeaveShip");
    assert!(cap_ic < leave_sc);

    // 3. BoardStoredShip while docked emits hull OnItemChange BEFORE SessionChange, keeping capsule locJunkyard=6 AFTER SessionChange.
    ctx.advance_secs(12);
    let bss_out = harness
        .call_exchange(
            "ship",
            None,
            "BoardStoredShip",
            &[PyRep::Integer(carrier_id), PyRep::Integer(stored_frig_id)],
            &kw,
        )
        .await;
    assert!(bss_out.result.is_ok());
    let stored_hull_ic = find_item_change_idx(&bss_out.frames, stored_frig_id, Some(station_id))
        .expect("BoardStoredShip must emit target hull OnItemChange");
    let bss_sc = bss_out
        .frames
        .iter()
        .position(|f| matches!(f.kind, ClientWireFrameKind::SessionChange { .. }))
        .expect("SessionChange on BoardStoredShip");
    let tomb_ic = find_item_change_idx(&bss_out.frames, capsule_id, Some(6))
        .expect("BoardStoredShip must emit capsule locJunkyard=6 OnItemChange");
    assert!(stored_hull_ic < bss_sc);
    assert!(bss_sc < tomb_ic);

    // 4. Negative control A: in-space ship swap keeps DoDestinyUpdate BEFORE SessionChangeNotification.
    ctx.advance_secs(12);
    harness.call_ok("ship", None, "Undock", &[], &kw).await;
    harness.call_ok("beyonce", None, "GetFormations", &[], &kw).await;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let ship_pos = sys_handle
        .get_ball_snapshot(stored_frig_id)
        .await
        .unwrap()
        .expect("active ship ball")
        .1
        .position;
    let space_hull_id = 100_990_010_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            space_hull_id,
            InventoryItemRecord {
                item_id: space_hull_id,
                type_id: 606,
                owner_id: char_id,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.item_positions.insert(space_hull_id, (ship_pos + DVec3::new(400.0, 0.0, 0.0)).to_array());
    });
    let b = Ball {
        id: space_hull_id,
        type_id: 606,
        position: ship_pos + DVec3::new(400.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let mut unpiloted_meta = ctx.build_ship_slim_meta(char_id, 606);
    unpiloted_meta.char_id = None;
    sys_handle
        .spawn_ball_with_meta(
            b,
            ctx.build_ship_combat_state(char_id, space_hull_id, 606),
            unpiloted_meta,
        )
        .await
        .expect("spawn unmanned hull in space");

    ctx.advance_secs(12);
    let space_board_out = harness
        .call_exchange("ship", None, "Board", &[PyRep::Integer(space_hull_id)], &kw)
        .await;
    assert!(space_board_out.result.is_ok());
    let space_dest_idx = space_board_out
        .frames
        .iter()
        .position(|f| matches!(f.kind, ClientWireFrameKind::DoDestinyUpdate { .. }))
        .expect("in-space Board must emit DoDestinyUpdate");
    let space_sc_idx = space_board_out
        .frames
        .iter()
        .position(|f| matches!(f.kind, ClientWireFrameKind::SessionChange { .. }))
        .expect("in-space Board must emit SessionChange");
    assert!(
        space_dest_idx < space_sc_idx,
        "in-space Board must send DoDestinyUpdate before SessionChangeNotification"
    );

    // 5. Negative control B: Structure control keeps locationid == solar_system_id and shipid == structure_id.
    assert_set_ball_pos(
        &sys_handle,
        space_hull_id,
        struct_pos + DVec3::new(1_500.0, 0.0, 0.0),
        "position near structure",
    )
    .await;
    ctx.advance_secs(12);
    harness
        .call_ok("structureDocking", None, "Dock", &[PyRep::Integer(struct_id)], &kw)
        .await;
    let hangar_bind = harness
        .call_ok(
            "invbroker",
            None,
            "MachoBindObject",
            &[
                PyRep::Integer(struct_id),
                PyRep::Tuple(vec![
                    PyRep::String("GetInventory".into()),
                    PyRep::Tuple(vec![PyRep::Integer(CONTAINER_STRUCTURE), PyRep::None]),
                    PyRep::Dict(Vec::new()),
                ]),
            ],
            &kw,
        )
        .await;
    let struct_hangar_bid = extract_bound_id_from_wire_rep(&hangar_bind).unwrap();
    let _ = harness
        .call_ok("invbroker", Some(struct_hangar_bid), "List", &[], &kw)
        .await;

    let ctrl_out = harness
        .call_exchange("structureControl", None, "TakeControl", &[PyRep::Integer(struct_id)], &kw)
        .await;
    assert!(ctrl_out.result.is_ok());
    assert_eq!(
        harness.transport().inner.session.effective_location_id(),
        Some(i64::from(sys_id)),
        "structure control must keep locationid == solar_system_id"
    );
    assert_eq!(harness.transport().inner.session.ship_id, Some(struct_id));
    let ctrl_sc = decode_session_change_map(&ctrl_out.session_change_packets[0]);
    assert_eq!(
        ctrl_sc.get("shipid"),
        Some(&(PyRep::Integer(space_hull_id), PyRep::Integer(struct_id)))
    );
    assert!(
        !ctrl_sc.contains_key("locationid"),
        "structure control must not rewrite locationid away from solar_system_id"
    );
}

#[tokio::test]
async fn test_1085_transition_cooldown_ready_at_wallclock_and_set_session_check_10s() {
    // parity: feat:navigation.session-change-timer, feat:navigation.undock, rpc:ship.Undock, rpc:structureDocking.Undock, rpc:beyonce.GetFormations
    eve_testkit::attest(&[
        "feat:navigation.session-change-timer",
        "feat:navigation.undock",
        "rpc:ship.Undock",
        "rpc:structureDocking.Undock",
        "rpc:beyonce.GetFormations",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let kw = HashMap::new();
    let char_id = 90_000_001_i64;
    let sys_id = 30_000_142_u32;
    let station_id = 60_003_760_i64;
    let dest_sys = 30_000_144_u32; // Perimeter
    let (ship_id, corp_id) = ctx
        .db
        .with_snapshot(|s| (s.characters[&char_id].active_ship_id, s.characters[&char_id].corporation_id));
    let gate_to_perim = ctx
        .sde
        .stargates()
        .values()
        .find(|g| g.from_solar_system_id == sys_id && g.to_solar_system_id == dest_sys)
        .expect("Jita -> Perimeter stargate in SDE");
    let struct_id = 1_047_000_000_103_i64;
    let struct_pos = DVec3::new(180_000.0, 0.0, 0.0);
    seed_owned_structure(&ctx, struct_id, sys_id, corp_id, struct_pos);

    let mut sess = SessionState::new(85_003);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let mut harness = common::wrap_wire_harness(&ctx, sess);
    harness
        .call_ok("charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw)
        .await;

    let assert_cooldown_shape = |rep: &PyRep, expected_ready_at: i64, label: &str| {
        let bid = rep
            .dict_get("beyonceBoundID")
            .and_then(|v| v.as_i64())
            .unwrap_or_else(|| panic!("{label}: missing beyonceBoundID in {rep:?}"));
        assert_eq!(
            rep.dict_get("boundResult"),
            Some(&PyRep::List(vec![
                PyRep::Integer(bid),
                PyRep::Integer(expected_ready_at),
            ])),
            "{label}: boundResult must be [boundID, readyAt]"
        );
        assert_eq!(
            rep.dict_get("sessionChangeTimerSeconds"),
            Some(&PyRep::Integer(7)),
            "{label}: sessionChangeTimerSeconds"
        );
        assert_eq!(
            rep.dict_get("sessionChangeTimerFiletime"),
            Some(&PyRep::Integer(expected_ready_at)),
            "{label}: sessionChangeTimerFiletime"
        );
        assert_eq!(
            rep.dict_get("sessionChangeTimer"),
            Some(&PyRep::Integer(expected_ready_at)),
            "{label}: sessionChangeTimer"
        );
    };

    // 1. stationSvc.Undock
    let expected_st = ctx.now_machonet_wallclock_filetime_ticks() + TRANSITION_COOLDOWN_TICKS;
    let st_undock = harness.call_ok("stationSvc", None, "Undock", &[], &kw).await;
    assert_cooldown_shape(&st_undock, expected_st, "stationSvc.Undock");

    // 2. ship.Undock after docking back at station
    let jita_handle = ctx.systems.get_or_spawn(sys_id);
    assert_set_ball_pos(
        &jita_handle,
        ship_id,
        ctx.sde.stations[&station_id].position + DVec3::new(1_000.0, 0.0, 0.0),
        "position near station",
    )
    .await;
    ctx.advance_secs(12);
    harness.call_ok("stationSvc", None, "Dock", &[PyRep::Integer(station_id)], &kw).await;
    ctx.advance_secs(12);
    let expected_ship_undock =
        ctx.now_machonet_wallclock_filetime_ticks() + TRANSITION_COOLDOWN_TICKS;
    let ship_undock = harness.call_ok("ship", None, "Undock", &[], &kw).await;
    assert_cooldown_shape(&ship_undock, expected_ship_undock, "ship.Undock");

    // 3. structureDocking.Undock
    assert_set_ball_pos(
        &jita_handle,
        ship_id,
        struct_pos + DVec3::new(1_500.0, 0.0, 0.0),
        "position near structure",
    )
    .await;
    ctx.advance_secs(12);
    harness
        .call_ok("structureDocking", None, "Dock", &[PyRep::Integer(struct_id)], &kw)
        .await;
    ctx.advance_secs(12);
    let expected_struct_undock =
        ctx.now_machonet_wallclock_filetime_ticks() + TRANSITION_COOLDOWN_TICKS;
    let struct_undock = harness.call_ok("structureDocking", None, "Undock", &[], &kw).await;
    assert_cooldown_shape(&struct_undock, expected_struct_undock, "structureDocking.Undock");

    // 4. beyonce.CmdStargateJump into 0.50 TiDi system keeps wall-clock 7,000ms readyAt (unscaled by TiDi)
    set_system_tidi_factor(&ctx, dest_sys, 0.5);
    ctx.advance_secs(12);
    assert_set_ball_pos(
        &jita_handle,
        ship_id,
        gate_to_perim.position + DVec3::new(500.0, 0.0, 0.0),
        "position at stargate",
    )
    .await;
    let bey_bind = harness
        .call_ok(
            "beyonce",
            None,
            "MachoBindObject",
            &[PyRep::Tuple(vec![PyRep::Integer(i64::from(sys_id)), PyRep::Integer(5)]), PyRep::None],
            &kw,
        )
        .await;
    let bey_bid = extract_bound_id_from_wire_rep(&bey_bind).expect("beyonce bound id");
    let expected_jump = ctx.now_machonet_wallclock_filetime_ticks() + TRANSITION_COOLDOWN_TICKS;
    let jump_rep = harness
        .call_ok(
            "beyonce",
            Some(bey_bid),
            "CmdStargateJump",
            &[PyRep::Integer(gate_to_perim.stargate_id)],
            &kw,
        )
        .await;
    assert_cooldown_shape(&jump_rep, expected_jump, "beyonce.CmdStargateJump(TiDi 0.50)");

    // 5. Negative control: SetSessionCheck keeps 10s timer (`sessionChangeTimerSeconds == 10`).
    ctx.advance_secs(12);
    let perim_bind = harness
        .call_ok(
            "beyonce",
            None,
            "MachoBindObject",
            &[PyRep::Tuple(vec![PyRep::Integer(i64::from(dest_sys)), PyRep::Integer(5)]), PyRep::None],
            &kw,
        )
        .await;
    let perim_bid = extract_bound_id_from_wire_rep(&perim_bind).expect("perim beyonce bound id");
    let check_rep = harness
        .call_ok(
            "beyonce",
            Some(perim_bid),
            "SetSessionCheck",
            &[PyRep::String("jump_drive".into())],
            &kw,
        )
        .await;
    assert_eq!(check_rep.dict_get("sessionChangeTimerSeconds"), Some(&PyRep::Integer(10)));
}

#[test]
fn test_1085_session_change_keys_matches_code_ccp_and_filters_dropped_keys() {
    // parity: feat:serverops.session-changes, notify:OnSessionChanged
    eve_testkit::attest(&["feat:serverops.session-changes", "notify:OnSessionChanged"]);

    let client_keys = extract_distributed_session_keys_from_code_ccp();
    let server_keys: BTreeSet<String> = SESSION_CHANGE_KEYS.iter().map(|s| (*s).to_string()).collect();
    assert_eq!(server_keys.len(), SESSION_CHANGE_KEYS.len(), "SESSION_CHANGE_KEYS must not duplicate entries");
    assert_eq!(
        server_keys, client_keys,
        "SESSION_CHANGE_KEYS must match V24 basesession.pyj distributed keys"
    );

    // Partial filtering: retains `shipid`, drops unknown key, increments counter, logs at DEBUG level.
    let buf = Arc::new(LogRingBuffer::new(32));
    let make_sub = || tracing_subscriber::registry().with(LogRingLayer::new(Arc::clone(&buf)));
    let before_count = session_change_dropped_keys_count();
    let mixed_raw = SessionChange::new(
        77_001,
        vec![
            ("shipid".into(), (PyRep::None, PyRep::Integer(100_000_001))),
            ("connectedServices".into(), (PyRep::None, PyRep::Integer(1))),
        ],
        Vec::new(),
    )
    .to_pyrep()
    .expect("raw SessionChange payload");

    let filtered_pkt = tracing::subscriber::with_default(make_sub(), || {
        build_filtered_session_change_packet(44, Some(99), 77_001, mixed_raw)
    })
    .expect("surviving distributed key must emit packet");
    assert_eq!(session_change_dropped_keys_count(), before_count + 1);
    let surviving = decode_session_change_map(&filtered_pkt);
    assert_eq!(surviving.get("shipid"), Some(&(PyRep::None, PyRep::Integer(100_000_001))));
    assert!(!surviving.contains_key("connectedServices"));

    // Negative: all keys dropped skips SessionChangeNotification frame entirely, increments counter, logs debug.
    let all_bad_raw = SessionChange::new(
        77_002,
        vec![
            ("localSID".into(), (PyRep::Integer(1), PyRep::Integer(2))),
            ("undocumentedDebugProp".into(), (PyRep::None, PyRep::Bool(true))),
        ],
        Vec::new(),
    )
    .to_pyrep()
    .expect("raw non-distributed SessionChange payload");
    let skipped = tracing::subscriber::with_default(make_sub(), || {
        build_filtered_session_change_packet(45, Some(99), 77_002, all_bad_raw)
    });
    assert!(skipped.is_none(), "all-dropped SessionChangeNotification must be skipped");
    assert_eq!(session_change_dropped_keys_count(), before_count + 3);

    let logs = buf.snapshot_recent(16);
    assert!(
        logs.iter().any(|l| l.contains("DEBUG") && l.contains("connectedServices")),
        "expected debug log naming dropped key connectedServices in {logs:?}"
    );
    assert!(
        logs.iter()
            .any(|l| l.contains("DEBUG") && l.contains("localSID") && l.contains("undocumentedDebugProp")),
        "expected debug log naming dropped keys localSID & undocumentedDebugProp in {logs:?}"
    );
}

#[tokio::test]
async fn test_1085_disconnect_persistence_guard_and_velocity_roundtrip() {
    // parity: feat:serverops.persistence, feat:character.safe-logoff, feat:character.emergency-warp-logoff, rpc:userSvc.UserLogOffCharacter
    eve_testkit::attest(&[
        "feat:serverops.persistence",
        "feat:character.safe-logoff",
        "feat:character.emergency-warp-logoff",
        "rpc:userSvc.UserLogOffCharacter",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let kw = HashMap::new();
    let char_id = 90_000_001_i64;
    let sys_id = 30_000_142_u32;
    let (ship_id, corp_id) = ctx
        .db
        .with_snapshot(|s| (s.characters[&char_id].active_ship_id, s.characters[&char_id].corporation_id));
    let struct_id = 1_048_000_000_104_i64;
    seed_owned_structure(&ctx, struct_id, sys_id, corp_id, DVec3::new(220_000.0, 0.0, 0.0));

    let mut sess = SessionState::new(85_004);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let mut harness = common::wrap_wire_harness(&ctx, sess);
    harness
        .call_ok("charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw)
        .await;
    harness.call_ok("ship", None, "Undock", &[], &kw).await;

    // 1. Positive: undocked ship with live ballpark ball persists exact pos + velocity and restores velocity on reconnect.
    let live_pos = DVec3::new(12_345_678.0, -456_789.0, 987_654.0);
    let live_vel = DVec3::new(142.5, -28.0, 61.25);
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    assert_set_ball_pos(&sys_handle, ship_id, live_pos, "set live pos").await;
    sys_handle
        .test_with_ballpark_mut_async(move |bp| {
            let ball = bp.balls.get_mut(&ship_id).expect("live ship ball");
            ball.velocity = live_vel;
        })
        .await;

    let disc = handle_character_logoff_or_disconnect_sync(&ctx, char_id)
        .expect("live undocked ball must produce DisconnectedShipEntry");
    assert_eq!(disc.last_position, live_pos.to_array());
    assert_eq!(disc.last_velocity, live_vel.to_array());
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.item_positions.get(&ship_id), Some(&live_pos.to_array()));
        assert_eq!(
            snap.sp6_lifecycle.item_velocities.get(&ship_id),
            Some(&live_vel.to_array())
        );
    });

    // Clear runtime ball so reconnect restores from persisted snapshot & verify velocity restored.
    sys_handle.remove_ball(ship_id).await.expect("remove runtime ball to test persisted restore");
    let (rec_sys, rec_ship, rec_pos) =
        reconnect_character_in_space(&ctx, char_id).await.expect("reconnect in space");
    assert_eq!((rec_sys, rec_ship), (sys_id, ship_id));
    assert!((rec_pos - live_pos).length() < 1.0);
    let restored_ball = sys_handle
        .get_ball_snapshot(ship_id)
        .await
        .unwrap()
        .expect("restored ship ball in ballpark")
        .1;
    assert!(
        (restored_ball.velocity - live_vel).length() < 1e-6,
        "reconnect must restore persisted velocity {live_vel:?}, got {:?}",
        restored_ball.velocity
    );

    // 2. Negative: undocked with NO live ball warns via tracing and preserves prior persisted pos/vel untouched.
    let sentinel_pos = [7_777_000.0, 888_000.0, -999_000.0];
    let sentinel_vel = [55.0, 66.0, 77.0];
    ctx.db.with_mut_snapshot(|snap| {
        snap.item_positions.insert(ship_id, sentinel_pos);
        snap.sp6_lifecycle.item_velocities.insert(ship_id, sentinel_vel);
    });
    sys_handle.remove_ball(ship_id).await.expect("remove live ball");

    let warn_buf = Arc::new(LogRingBuffer::new(32));
    let warn_sub = tracing_subscriber::registry().with(LogRingLayer::new(Arc::clone(&warn_buf)));
    let sp6_none = tracing::subscriber::with_default(warn_sub, || {
        let sp6 = handle_character_logoff_or_disconnect_sync(&ctx, char_id);
        ctx.logoff_character_sync(char_id, Some(&mut harness.transport_mut().inner.session));
        sp6
    });
    assert!(
        sp6_none.is_none(),
        "undocked disconnect without live ball must not synthesize DisconnectedShipEntry"
    );
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.item_positions.get(&ship_id), Some(&sentinel_pos));
        assert_eq!(snap.sp6_lifecycle.item_velocities.get(&ship_id), Some(&sentinel_vel));
        let ch = &snap.characters[&char_id];
        assert!(ch.station_id.is_none() && ch.structure_id.is_none());
    });
    let warn_logs = warn_buf.snapshot_recent(16);
    assert!(
        warn_logs.iter().any(|l| l.contains("WARN") && l.contains("no live ball kinematics")),
        "missing live ball must emit WARN log instead of inventing fallback: {warn_logs:?}"
    );

    // 3. Negative: structure-docked UserLogOffCharacter & logoff_character_sync NEVER rewrite to station-docked.
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = Some(struct_id);
            ch.solar_system_id = sys_id;
        }
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.location_id = struct_id;
            ship.flag = InventoryFlag::Hangar;
        }
    });
    let mut s_struct = SessionState::new(85_005);
    common::login_pilot(&ctx, &mut s_struct, "demo", "demo-secret").await;
    let mut struct_harness = common::wrap_wire_harness(&ctx, s_struct);
    struct_harness
        .call_ok("charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw)
        .await;
    let logoff_rep = struct_harness
        .call_ok("userSvc", None, "UserLogOffCharacter", &[PyRep::Integer(char_id)], &kw)
        .await;
    assert_eq!(logoff_rep, PyRep::Bool(true));
    assert!(
        handle_character_logoff_or_disconnect_sync(&ctx, char_id).is_none(),
        "structure-docked disconnect must not produce in-space DisconnectedShipEntry"
    );
    ctx.logoff_character_sync(char_id, Some(&mut struct_harness.transport_mut().inner.session));
    ctx.db.with_snapshot(|snap| {
        let ch = &snap.characters[&char_id];
        assert_eq!(ch.structure_id, Some(struct_id));
        assert_eq!(ch.station_id, None);
    });
}

#[tokio::test]
async fn test_1085_reconnect_do_sim_clock_rebase_and_attach_ordering() {
    // parity: feat:serverops.session-changes, notify:DoSimClockRebase, notify:OnSessionChanged, rpc:charUnboundMgr.SelectCharacterID, rpc:beyonce.GetFormations, destiny:SetState
    eve_testkit::attest(&[
        "feat:serverops.session-changes",
        "notify:DoSimClockRebase",
        "notify:OnSessionChanged",
        "rpc:charUnboundMgr.SelectCharacterID",
        "rpc:beyonce.GetFormations",
        "destiny:SetState",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let kw = HashMap::new();
    let char_id = 90_000_001_i64;
    let sys_id = 30_000_142_u32;
    let ship_id = ctx.db.with_snapshot(|s| s.characters[&char_id].active_ship_id);

    // Move character into space in a 0.50 TiDi system whose simulation clock lags wallclock.
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = sys_id;
        }
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.location_id = i64::from(sys_id);
            ship.flag = InventoryFlag::None;
        }
        snap.item_positions.insert(ship_id, [15_000.0, 0.0, 0.0]);
    });
    let _ = ctx.systems.get_or_spawn(sys_id);
    set_system_tidi_factor(&ctx, sys_id, 0.5);
    ctx.advance_secs(20);

    // 1. Reconnect emits SessionChangeNotification -> CallRsp(SelectCharacterID) -> DoSimClockRebase((oldSimTime > newSimTime)),
    //    and subsequent beyonce.GetFormations bootstraps SetState BEFORE CallRsp with no SetState replay on MachoBindObject.
    let mut sess = SessionState::new(85_006);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let mut harness = common::wrap_wire_harness(&ctx, sess);

    let sel_out = harness
        .call_exchange("charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw)
        .await;
    assert!(sel_out.result.is_ok());
    let sel_sc_idx = sel_out
        .frames
        .iter()
        .position(|f| matches!(f.kind, ClientWireFrameKind::SessionChange { .. }))
        .expect("SelectCharacterID SessionChangeNotification");
    let sel_rsp_idx = sel_out
        .frames
        .iter()
        .position(|f| matches!(f.kind, ClientWireFrameKind::CallRsp { .. }))
        .expect("SelectCharacterID CallRsp");
    let rebase_idx = sel_out
        .frames
        .iter()
        .position(|f| {
            matches!(
                &f.kind,
                ClientWireFrameKind::Notification { event, .. } if event == "DoSimClockRebase"
            )
        })
        .expect("dilated in-space SelectCharacterID must emit DoSimClockRebase");
    assert!(sel_sc_idx < sel_rsp_idx && sel_rsp_idx < rebase_idx);

    let rebase_args = &sel_out
        .notifications
        .iter()
        .find(|(ev, _)| ev == "DoSimClockRebase")
        .unwrap()
        .1;
    let PyRep::Tuple(times) = &rebase_args[0] else {
        panic!("DoSimClockRebase expects (oldSimTime, newSimTime) tuple, got {rebase_args:?}");
    };
    let old_sim_time = times[0].as_i64().expect("oldSimTime");
    let new_sim_time = times[1].as_i64().expect("newSimTime");
    assert!(old_sim_time > new_sim_time, "expected wallclock {old_sim_time} > dilated sim {new_sim_time}");

    // Negative: non-ballpark RPC (`skillMgr2.GetSkills`) prior to `GetFormations` must not emit DoDestinyUpdate(SetState).
    let skills_out = harness.call_exchange("skillMgr2", None, "GetSkills", &[], &kw).await;
    assert!(
        skills_out
            .frames
            .iter()
            .all(|f| !matches!(f.kind, ClientWireFrameKind::DoDestinyUpdate { has_set_state: true, .. })),
        "skillMgr2.GetSkills must not attach ballpark SetState prematurely"
    );

    // Bootstrap via beyonce.GetFormations emits DoDestinyUpdate(SetState) BEFORE CallRsp, and MachoBindObject does NOT replay SetState.
    let gf_out = harness.call_exchange("beyonce", None, "GetFormations", &[], &kw).await;
    assert!(gf_out.result.is_ok());
    let gf_set_state_idx = gf_out
        .frames
        .iter()
        .position(|f| matches!(f.kind, ClientWireFrameKind::DoDestinyUpdate { has_set_state: true, .. }))
        .expect("GetFormations must bootstrap DoDestinyUpdate(SetState)");
    let gf_rsp_idx = gf_out
        .frames
        .iter()
        .position(|f| matches!(f.kind, ClientWireFrameKind::CallRsp { .. }))
        .expect("GetFormations CallRsp");
    assert!(gf_set_state_idx < gf_rsp_idx);
    let mut gf_actions = Vec::new();
    for batch in &gf_out.destiny_batches {
        collect_destiny_action_names(batch, &mut gf_actions);
    }
    assert!(
        gf_actions.iter().any(|a| a == "SetState"),
        "GetFormations destiny batches must include SetState: {gf_actions:?}"
    );

    let bind_out = harness
        .call_exchange(
            "beyonce",
            None,
            "MachoBindObject",
            &[PyRep::Tuple(vec![PyRep::Integer(i64::from(sys_id)), PyRep::Integer(5)]), PyRep::None],
            &kw,
        )
        .await;
    assert!(
        bind_out
            .frames
            .iter()
            .all(|f| !matches!(f.kind, ClientWireFrameKind::DoDestinyUpdate { has_set_state: true, .. })),
        "post-GetFormations MachoBindObject must not replay SetState"
    );

    // 2. When DoSimClockRebase queues right into the Attach RPC (`GetFormations` on fresh wire session),
    //    sequencer hoists it BEFORE DoDestinyUpdate(SetState) which precedes CallRsp and trailing notifications.
    let mut seq_state = eve_server::outbound_sequencer::SessionWireState::new(1085, 1085);
    let batch = eve_server::outbound_sequencer::DestinyBatch {
        epoch: 1,
        stamp: 500,
        has_set_state: true,
        updates: PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Integer(500),
            PyRep::Tuple(vec![PyRep::String("SetState".into())]),
        ])]),
        wait_for_bubble: false,
        scene_scope: None,
    };
    let outcomes = sequence_with_events(
        &mut seq_state,
        RpcCompletion {
            call_id: 9,
            response_frame: PyRep::Integer(0),
            session_change_frame: Some(PyRep::Integer(1)),
            pending_tick_batches: Vec::new(),
            destiny_drain: eve_server::outbound_sequencer::DestinyDrain::Attach {
                epoch: 1,
                set_state: batch,
            },
            notifications: vec![
                eve_server::outbound_sequencer::TypedNotification {
                    event: "OnCharNoLongerInStation".into(),
                    args: Vec::new(),
                    service: None,
                    scene_scope: None,
                },
                eve_server::outbound_sequencer::TypedNotification {
                    event: "DoSimClockRebase".into(),
                    args: vec![PyRep::Tuple(vec![
                        PyRep::Integer(old_sim_time),
                        PyRep::Integer(new_sim_time),
                    ])],
                    service: None,
                    scene_scope: None,
                },
            ],
            item_change_before_session_change: false,
        },
    );
    let sc_pos = outcomes.frames.iter().position(|f| matches!(f, OutFrame::SessionChange { .. })).unwrap();
    let rb_pos = outcomes
        .frames
        .iter()
        .position(|f| matches!(f, OutFrame::Notification { event, .. } if event == "DoSimClockRebase"))
        .unwrap();
    let ss_pos = outcomes
        .frames
        .iter()
        .position(|f| matches!(f, OutFrame::DoDestinyUpdate { has_set_state: true, .. }))
        .unwrap();
    let rsp_pos = outcomes.frames.iter().position(|f| matches!(f, OutFrame::CallRsp { .. })).unwrap();
    let trailing_pos = outcomes
        .frames
        .iter()
        .position(|f| matches!(f, OutFrame::Notification { event, .. } if event == "OnCharNoLongerInStation"))
        .unwrap();
    assert!(sc_pos < rb_pos && rb_pos < ss_pos && ss_pos < rsp_pos && rsp_pos < trailing_pos);
}
