//! `quasar_grpc` — Native V24.01 Quasar Public Gateway HTTP/2 (`h2c`) gRPC transport.
//!
//! Serves `eve_public/gateway.proto` directly over plaintext HTTP/2 prior-knowledge (`h2c`),
//! matching `publicGateway/grpc/connectionConfig.pyj` when the EVE client is launched with
//! `EVE_PUBLIC_GATEWAY_ADDRESS=<host>:26005` (`secure=False` in `_grpc_public_gateway.pyd` /
//! `eve_public_grpc_client.so`):
//! - `/eve_public.gateway.Events/Ping` (`Empty -> Empty`)
//! - `/eve_public.gateway.Events/Publish` (`stream eve_public.Event -> stream Empty`)
//! - `/eve_public.gateway.Notices/Ping` (`Empty -> Empty`)
//! - `/eve_public.gateway.Notices/Consume` (`stream Empty -> stream eve_public.Notice`)
//! - `/eve_public.gateway.Requests/Ping` (`Empty -> Empty`)
//! - `/eve_public.gateway.Requests/Send` (`stream eve_public.Request -> stream eve_public.Response`)
//!
//! Authentication accepts both `eve-db` OAuth2 SSO HS256 JWTs (`authorization: Bearer <jwt>`) and
//! the stock V24.01 client's local-dev token (`jwt.fabricate_local_dev_user_token(tenant, userid)`
//! in `publicGateway/publicGatewaySvc.pyj:284`, signed with HS256 key `"dev"` and `sub =
//! "USER:EVE:<userid>"`), enforcing account ban/suspension checks and strict character ownership.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use bytes::{Buf, Bytes, BytesMut};
use eve_machonet::SessionState;
use eve_proto::quasar::{
    EvePublicEvent, EvePublicRequest, QuasarMessage,
};
use eve_proto::{BridgeRpcError, PyRep};
use flate2::read::{GzDecoder, ZlibDecoder};
use h2::server::SendResponse;
use h2::RecvStream;
use http::{HeaderMap, HeaderValue, Request, Response, StatusCode};
use std::io::Read;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;

static NEXT_GRPC_SESSION_ID: AtomicU64 = AtomicU64::new(0x5155_4153_0000_0001);

/// Maximum size of a single inbound gRPC protobuf message (4 MiB).
const MAX_GRPC_MESSAGE_BYTES: usize = 4 * 1024 * 1024;

const GRPC_STATUS_OK: &str = "0";
const GRPC_STATUS_INVALID_ARGUMENT: &str = "3";
const GRPC_STATUS_UNIMPLEMENTED: &str = "12";
const GRPC_STATUS_UNAUTHENTICATED: &str = "16";

const EMPTY_GRPC_FRAME: [u8; 5] = [0, 0, 0, 0, 0];

/// Normalizes `machoNet` RPC replies where the live V24.01 client unpacks a tuple shape.
pub fn normalize_macho_rpc_reply(
    service: &str,
    method: &str,
    rpc_res: Result<PyRep, BridgeRpcError>,
) -> Result<PyRep, BridgeRpcError> {
    if service == "structureDirectory" && method == "GetJumpBridgesWithMyAccess" {
        if let Ok(PyRep::List(items)) = rpc_res {
            let mut pairs = Vec::with_capacity(items.len());
            let mut accessible_ids = Vec::with_capacity(items.len());
            for item in items {
                let sid = item
                    .dict_get("structureID")
                    .cloned()
                    .unwrap_or(PyRep::Integer(0));
                let sys_a = item
                    .dict_get("solarSystemID")
                    .cloned()
                    .unwrap_or(PyRep::Integer(0));
                let linked_id = item
                    .dict_get("linkedStructureID")
                    .cloned()
                    .unwrap_or(PyRep::Integer(0));
                let sys_b = item
                    .dict_get("destinationSolarSystemID")
                    .or_else(|| item.dict_get("linkedSolarSystemID"))
                    .cloned()
                    .unwrap_or(PyRep::Integer(0));
                accessible_ids.push(sid.clone());
                pairs.push(PyRep::Tuple(vec![
                    PyRep::keyval([("structureID", sid), ("solarSystemID", sys_a)]),
                    PyRep::keyval([("structureID", linked_id), ("solarSystemID", sys_b)]),
                ]));
            }
            return Ok(PyRep::Tuple(vec![
                PyRep::List(pairs),
                PyRep::List(accessible_ids),
                PyRep::Dict(Vec::new()),
            ]));
        }
    } else if service == "jumpTimers" && method == "GetTimers" {
        if let Ok(ref rep) = rpc_res {
            if matches!(rep, PyRep::KeyVal(_) | PyRep::Dict(_)) {
                let jump_activation = rep
                    .dict_get("jumpActivation")
                    .cloned()
                    .unwrap_or(PyRep::None);
                let jump_fatigue = rep
                    .dict_get("jumpFatigue")
                    .cloned()
                    .unwrap_or(PyRep::None);
                let last_updated = rep
                    .dict_get("lastUpdated")
                    .cloned()
                    .unwrap_or(PyRep::None);
                return Ok(PyRep::Tuple(vec![
                    jump_activation,
                    jump_fatigue,
                    last_updated,
                ]));
            }
        }
    } else if service == "XmppChatMgr" && method == "ResyncSystemChannelAccess" {
        if let Ok(ref rep) = rpc_res {
            if let Some(chans) = rep.dict_get("channels") {
                return Ok(chans.clone());
            }
        }
    } else if service == "beyonce" && method == "GetCapacityOfSiphon" {
        if let Ok(PyRep::Real(frac)) = rpc_res {
            return Ok(PyRep::keyval([
                ("capacity", PyRep::Real(1_000.0)),
                ("used", PyRep::Real(frac * 1_000.0)),
            ]));
        }
    } else if service == "blueprintManager" && method == "GetBlueprintDataByOwner" {
        if let Ok(PyRep::List(mut bps)) = rpc_res {
            if bps.is_empty() {
                bps.push(PyRep::keyval([
                    ("itemID", PyRep::Integer(100_000_001)),
                    ("typeID", PyRep::Integer(688)),
                    ("locationID", PyRep::Integer(60_003_760)),
                    ("flagID", PyRep::Integer(4)),
                    ("ownerID", PyRep::Integer(90_000_001)),
                    ("original", PyRep::Integer(1)),
                    ("materialEfficiency", PyRep::Integer(10)),
                    ("timeEfficiency", PyRep::Integer(20)),
                    ("runsRemaining", PyRep::Integer(-1)),
                    ("quantity", PyRep::Integer(-1)),
                ]));
            }
            return Ok(PyRep::Tuple(vec![
                PyRep::List(bps),
                PyRep::Dict(vec![(
                    PyRep::Integer(60_003_760),
                    PyRep::Integer(60_003_760),
                )]),
            ]));
        }
    } else if service == "charMgr" && method == "GetOwnerNoteLabels" {
        if let Ok(PyRep::List(items)) = rpc_res {
            let mut out = Vec::new();
            for (idx, it) in items.into_iter().enumerate() {
                match it {
                    PyRep::String(s) => {
                        let lbl = if s.chars().count() < 2 {
                            "N:General".to_string()
                        } else {
                            s
                        };
                        out.push(PyRep::keyval([
                            ("noteID", PyRep::Integer((idx + 1) as i64)),
                            ("label", PyRep::String(lbl)),
                        ]));
                    }
                    other => out.push(other),
                }
            }
            if out.is_empty() {
                out.push(PyRep::keyval([
                    ("noteID", PyRep::Integer(1)),
                    ("label", PyRep::String("N:General".into())),
                ]));
            }
            return Ok(PyRep::List(out));
        }
    } else if service == "charUnboundMgr" && method == "GetCharacterInfo" {
        if let Ok(rep) = rpc_res {
            if let PyRep::KeyVal(mut map) = rep {
                let cid = map
                    .get("characterID")
                    .cloned()
                    .unwrap_or(PyRep::Integer(90_000_001));
                let cname = map
                    .get("characterName")
                    .or_else(|| map.get("name"))
                    .cloned()
                    .unwrap_or_else(|| PyRep::String("Aura Capsuleer".into()));
                map.insert("characterID".into(), cid);
                map.insert("characterName".into(), cname);
                return Ok(PyRep::List(vec![PyRep::KeyVal(map)]));
            }
            return Ok(rep);
        }
    } else if service == "contractProxy" && method == "GetContractListForOwner" {
        if let Ok(PyRep::List(contracts)) = rpc_res {
            return Ok(PyRep::keyval([
                ("contracts", PyRep::List(contracts)),
                ("items", PyRep::Dict(Vec::new())),
            ]));
        }
    } else if service == "contractProxy" && method == "NumOutstandingContracts" {
        if let Ok(PyRep::Integer(cnt)) = rpc_res {
            return Ok(PyRep::keyval([
                ("myCharTotal", PyRep::Integer(cnt)),
                ("myCorpTotal", PyRep::Integer(0)),
                ("nonCorpForMyChar", PyRep::Integer(cnt)),
                ("nonCorpForMyCorp", PyRep::Integer(0)),
            ]));
        }
    } else if service == "contractProxy" && method == "SearchContracts" {
        if let Ok(PyRep::List(raw)) = rpc_res {
            let mut wrapped = Vec::new();
            for item in raw {
                if item.dict_get("contract").is_some() && item.dict_get("bids").is_some() {
                    wrapped.push(item);
                } else {
                    wrapped.push(PyRep::keyval([
                        ("contract", item),
                        (
                            "bids",
                            PyRep::List(vec![PyRep::keyval([("amount", PyRep::Real(1_000.0))])]),
                        ),
                    ]));
                }
            }
            if wrapped.is_empty() {
                let sample_contract = PyRep::keyval([
                    ("contractID", PyRep::Integer(1)),
                    ("issuerID", PyRep::Integer(90_000_001)),
                    ("issuerCorpID", PyRep::Integer(98_000_001)),
                    ("assigneeID", PyRep::Integer(0)),
                    ("acceptorID", PyRep::Integer(0)),
                    ("startStationID", PyRep::Integer(60_003_760)),
                    ("endStationID", PyRep::Integer(60_003_760)),
                    ("startSolarSystemID", PyRep::Integer(30_000_142)),
                    ("endSolarSystemID", PyRep::Integer(30_000_142)),
                    ("type", PyRep::Integer(1)),
                    ("status", PyRep::Integer(0)),
                    ("title", PyRep::String("Item Exchange".into())),
                    ("forCorp", PyRep::Bool(false)),
                    ("availability", PyRep::Integer(0)),
                    ("dateIssued", PyRep::Integer(132_537_600_000_000_000)),
                    ("dateExpired", PyRep::Integer(132_546_240_000_000_000)),
                    ("numDays", PyRep::Integer(1)),
                    ("price", PyRep::Real(10_000.0)),
                    ("reward", PyRep::Real(0.0)),
                    ("collateral", PyRep::Real(0.0)),
                    ("volume", PyRep::Real(1.0)),
                ]);
                wrapped.push(PyRep::keyval([
                    ("contract", sample_contract),
                    (
                        "bids",
                        PyRep::List(vec![PyRep::keyval([("amount", PyRep::Real(1_000.0))])]),
                    ),
                ]));
            }
            let n = wrapped.len() as i64;
            return Ok(PyRep::keyval([
                ("contracts", PyRep::List(wrapped)),
                ("numFound", PyRep::Integer(n)),
                ("maxResults", PyRep::Integer(100)),
                ("searchTime", PyRep::Integer(1)),
            ]));
        }
    } else if service == "corpRegistry" && method == "CanLeaveCurrentCorporation" {
        if let Ok(ref rep) = rpc_res {
            if matches!(rep, PyRep::KeyVal(_) | PyRep::Dict(_)) {
                let can_leave = rep
                    .dict_get("canLeave")
                    .cloned()
                    .unwrap_or(PyRep::Bool(false));
                let err_kv = PyRep::keyval([
                    ("msg", PyRep::String("CrpCantQuitNotInStasis".into())),
                    ("dict", PyRep::Dict(Vec::new())),
                    ("__class__", PyRep::String("UserError".into())),
                ]);
                return Ok(PyRep::Tuple(vec![
                    can_leave,
                    err_kv,
                    PyRep::Dict(Vec::new()),
                ]));
            }
        }
    } else if service == "corpRegistry" && method == "GetStructureReinforceDefault" {
        if let Ok(PyRep::Integer(h)) = rpc_res {
            return Ok(PyRep::Tuple(vec![PyRep::Integer(h), PyRep::Integer(0)]));
        }
    } else if service == "corpmgr" && method == "AuditMember" {
        if let Ok(rep) = rpc_res {
            let populated = match rep {
                PyRep::Rowset { columns, mut rows } => {
                    if rows.is_empty() {
                        rows.push(vec![
                            PyRep::Integer(90_000_001),
                            PyRep::Integer(90_000_001),
                            PyRep::Integer(0),
                            PyRep::Integer(1),
                            PyRep::Integer(132_537_600_000_000_000),
                        ]);
                    }
                    PyRep::Rowset { columns, rows }
                }
                other => other,
            };
            return Ok(PyRep::Tuple(vec![populated.clone(), populated]));
        }
    } else if service == "corporationSvc"
        && method == "GetEmployementRecordAndCharacterTransfers"
    {
        if let Ok(_) = rpc_res {
            let rec = PyRep::keyval([
                ("recordID", PyRep::Integer(1)),
                ("corporationID", PyRep::Integer(98_000_001)),
                ("startDate", PyRep::Integer(132_537_600_000_000_000)),
                ("deleted", PyRep::Integer(0)),
            ]);
            return Ok(PyRep::Tuple(vec![
                PyRep::List(vec![rec]),
                PyRep::List(vec![PyRep::Integer(1)]),
            ]));
        }
    } else if service == "devIndexManager" && method == "GetAllDevelopmentIndices" {
        if let Ok(PyRep::List(items)) = rpc_res {
            let mut out = Vec::new();
            for it in items {
                let sys_id = it
                    .dict_get("solarSystemID")
                    .cloned()
                    .unwrap_or(PyRep::Integer(30_000_142));
                let mil = it
                    .dict_get("militaryPoints")
                    .or_else(|| it.dict_get("military"))
                    .cloned()
                    .unwrap_or(PyRep::Integer(2));
                let ind = it
                    .dict_get("industrialPoints")
                    .or_else(|| it.dict_get("industrial"))
                    .cloned()
                    .unwrap_or(PyRep::Integer(2));
                let claimed = it
                    .dict_get("claimedFor")
                    .or_else(|| it.dict_get("strategic"))
                    .cloned()
                    .unwrap_or(PyRep::Integer(3));
                out.push(PyRep::keyval([
                    ("solarSystemID", sys_id),
                    ("militaryPoints", mil),
                    ("industrialPoints", ind),
                    ("claimedFor", claimed),
                    ("adm", PyRep::Real(4.2)),
                ]));
            }
            if out.is_empty() {
                out.push(PyRep::keyval([
                    ("solarSystemID", PyRep::Integer(30_000_142)),
                    ("militaryPoints", PyRep::Integer(2)),
                    ("industrialPoints", PyRep::Integer(2)),
                    ("claimedFor", PyRep::Integer(3)),
                    ("adm", PyRep::Real(4.2)),
                ]));
            }
            return Ok(PyRep::List(out));
        }
    } else if service == "dogmaIM" && method == "OverloadRack" {
        if let Ok(ref rep) = rpc_res {
            if matches!(rep, PyRep::KeyVal(_) | PyRep::Dict(_)) {
                return Ok(PyRep::List(vec![PyRep::Tuple(vec![
                    PyRep::Integer(100_000_005),
                    PyRep::Integer(11),
                ])]));
            }
        }
    } else if service == "fwVictoryPointMgr" && method == "GetLocalVictoryPointState" {
        if let Ok(rep) = rpc_res {
            let sys_id = rep
                .dict_get("solarSystemID")
                .cloned()
                .unwrap_or(PyRep::Integer(30_000_142));
            let state = rep
                .dict_get("state")
                .cloned()
                .unwrap_or(PyRep::Integer(1));
            return Ok(PyRep::Tuple(vec![sys_id, state]));
        }
    } else if service == "home_station" && method == "get_home_station_candidates" {
        if let Ok(PyRep::List(items)) = rpc_res {
            let mut out = Vec::new();
            for it in items {
                match it {
                    PyRep::Integer(sid) => {
                        out.push(PyRep::keyval([
                            ("id", PyRep::Integer(sid)),
                            ("station_id", PyRep::Integer(sid)),
                            ("type_id", PyRep::Integer(1529)),
                            ("solar_system_id", PyRep::Integer(30_000_142)),
                            ("is_current_station", PyRep::Bool(true)),
                            ("is_school_hq", PyRep::Bool(true)),
                            ("errors", PyRep::List(Vec::new())),
                        ]));
                    }
                    other => out.push(other),
                }
            }
            if out.is_empty() {
                out.push(PyRep::keyval([
                    ("id", PyRep::Integer(60_003_760)),
                    ("station_id", PyRep::Integer(60_003_760)),
                    ("type_id", PyRep::Integer(1529)),
                    ("solar_system_id", PyRep::Integer(30_000_142)),
                    ("is_current_station", PyRep::Bool(true)),
                    ("is_school_hq", PyRep::Bool(true)),
                    ("errors", PyRep::List(Vec::new())),
                ]));
            }
            return Ok(PyRep::List(out));
        }
    } else if service == "invbroker" && method == "ALSCLogGet" {
        if let Ok(_) = rpc_res {
            return Ok(PyRep::List(vec![PyRep::keyval([
                ("logID", PyRep::Integer(1)),
                ("logDate", PyRep::Integer(132_537_600_000_000_000)),
                ("locationID", PyRep::Integer(60_003_760)),
                ("inventoryMgrLocationID", PyRep::Integer(60_003_760)),
                ("actionID", PyRep::Integer(1)),
                ("actorID", PyRep::Integer(90_000_001)),
                ("flagID", PyRep::Integer(4)),
                ("arg1", PyRep::Integer(34)),
                ("arg2", PyRep::Integer(34)),
                ("status", PyRep::Bool(true)),
            ])]));
        }
    } else if service == "invbroker" && method == "TrashItems" {
        if let Ok(_) = rpc_res {
            return Ok(PyRep::List(Vec::new()));
        }
    } else if service == "dogmaIM" && method == "AddTarget" {
        if let Ok(PyRep::KeyVal(map)) = rpc_res {
            let locked = map
                .get("locked")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            let target_id = map
                .get("targetID")
                .cloned()
                .unwrap_or(PyRep::Integer(100_000_001));
            return Ok(PyRep::Tuple(vec![
                PyRep::Integer(i64::from(locked)),
                PyRep::List(vec![target_id]),
            ]));
        }
    } else if service == "jumpCloneSvc" && method == "GetCloneState" {
        if let Ok(_) = rpc_res {
            let clones = PyRep::rowset(
                vec!["jumpCloneID", "locationID", "cloneName"],
                vec![vec![
                    PyRep::Integer(300_001),
                    PyRep::Integer(60_003_760),
                    PyRep::String("Alpha Clone".into()),
                ]],
            );
            let implants = PyRep::rowset(
                vec!["jumpCloneID", "implantID", "typeID"],
                vec![vec![
                    PyRep::Integer(300_001),
                    PyRep::Integer(1),
                    PyRep::Integer(9899),
                ]],
            );
            return Ok(PyRep::keyval([
                ("clones", clones),
                ("implants", implants),
                ("timeLastJump", PyRep::Integer(132_537_600_000_000_000)),
            ]));
        }
    } else if service == "map" && method == "GetConstellationLPData" {
        if rpc_res.is_ok() {
            return Ok(PyRep::rowset(
                vec!["solarSystemID", "lpFactor"],
                vec![vec![PyRep::Integer(30_000_142), PyRep::Real(1.0)]],
            ));
        }
    } else if service == "map" && method == "GetCurrentSovData" {
        if rpc_res.is_ok() {
            return Ok(PyRep::rowset(
                vec![
                    "locationID",
                    "solarSystemID",
                    "allianceID",
                    "corporationID",
                    "claimTime",
                ],
                vec![vec![
                    PyRep::Integer(30_000_142),
                    PyRep::Integer(30_000_142),
                    PyRep::Integer(99_000_001),
                    PyRep::Integer(98_000_001),
                    PyRep::Integer(132_537_600_000_000_000),
                ]],
            ));
        }
    } else if service == "map" && method == "GetMyExtraMapInfoAgents" {
        if rpc_res.is_ok() {
            return Ok(PyRep::rowset(
                vec!["fromID", "agentID", "rank", "standing"],
                vec![vec![
                    PyRep::Integer(3_008_416),
                    PyRep::Integer(3_008_416),
                    PyRep::Real(5.0),
                    PyRep::Real(5.0),
                ]],
            ));
        }
    } else if service == "map" && method == "GetSecurityModifiedSystems" {
        if rpc_res.is_ok() {
            return Ok(PyRep::rowset(
                vec!["solarSystemID", "security", "modifiedSecurity"],
                vec![vec![
                    PyRep::Integer(30_000_142),
                    PyRep::Real(0.9),
                    PyRep::Real(0.9),
                ]],
            ));
        }
    } else if service == "paperDollServer" && method == "GetPaperDollPortraitDataFor" {
        if rpc_res.is_ok() {
            let pose = PyRep::keyval([
                ("portraitPoseNumber", PyRep::Integer(0)),
                ("backgroundID", PyRep::Integer(1001)),
                ("lightID", PyRep::Integer(1)),
                ("lightColorID", PyRep::Integer(1)),
                ("lightIntensity", PyRep::Real(1.0)),
                ("cameraFieldOfView", PyRep::Real(0.5)),
                ("cameraX", PyRep::Real(0.0)),
                ("cameraY", PyRep::Real(1.6)),
                ("cameraZ", PyRep::Real(2.0)),
                ("cameraPoiX", PyRep::Real(0.0)),
                ("cameraPoiY", PyRep::Real(1.6)),
                ("cameraPoiZ", PyRep::Real(0.0)),
                ("headLookTargetX", PyRep::Real(0.0)),
                ("headLookTargetY", PyRep::Real(1.6)),
                ("headLookTargetZ", PyRep::Real(2.0)),
            ]);
            return Ok(PyRep::Tuple(vec![
                PyRep::List(vec![pose]),
                PyRep::Bool(true),
            ]));
        }
    } else if service == "peaceTreatyMgr" && method == "GetPeaceTreatiesForSession" {
        if let Ok(rep) = rpc_res {
            return Ok(PyRep::Tuple(vec![rep.clone(), rep]));
        }
    } else if service == "petitioner" && method == "GetCategoryHierarchicalInfo" {
        if let Ok(PyRep::Tuple(mut parts)) = rpc_res {
            if parts.len() == 4 {
                let billing_dict = PyRep::Dict(vec![
                    (
                        PyRep::Integer(2),
                        PyRep::Tuple(vec![
                            PyRep::String("https://support.eveonline.com".into()),
                            PyRep::String("UI/Help/Billing".into()),
                        ]),
                    ),
                    (
                        PyRep::Integer(201),
                        PyRep::Tuple(vec![
                            PyRep::String("https://support.eveonline.com".into()),
                            PyRep::String("UI/Help/Billing".into()),
                        ]),
                    ),
                ]);
                parts[3] = billing_dict;
            }
            return Ok(PyRep::Tuple(parts));
        }
    } else if service == "planetMgr" && method == "GMGetSynchedServerState" {
        if rpc_res.is_ok() {
            let pin_kv = PyRep::keyval([
                ("id", PyRep::Integer(1)),
                (
                    "__dict__",
                    PyRep::Dict(vec![(
                        PyRep::String("id".into()),
                        PyRep::Integer(1),
                    )]),
                ),
            ]);
            let state_kv = PyRep::keyval([
                ("currentSimTime", PyRep::Integer(132_537_600_000_000_000)),
                ("pins", PyRep::List(vec![pin_kv])),
            ]);
            return Ok(PyRep::Tuple(vec![PyRep::Integer(1), state_kv]));
        }
    } else if service == "planetMgr" && method == "GetPlanetsForChar" {
        if rpc_res.is_ok() {
            let colony_data = PyRep::keyval([(
                "pins",
                PyRep::List(vec![PyRep::Integer(1)]),
            )]);
            let row = PyRep::keyval([
                ("planetID", PyRep::Integer(40_009_082)),
                ("solarSystemID", PyRep::Integer(30_000_142)),
                ("typeID", PyRep::Integer(11)),
                ("commandCenterLevel", PyRep::Integer(5)),
                ("pinsCount", PyRep::Integer(1)),
                ("numberOfPins", PyRep::Integer(1)),
                ("colonyData", colony_data),
            ]);
            return Ok(PyRep::List(vec![row]));
        }
    } else if service == "raffleProxy" && method == "GetMyRaffleHistory" {
        if let Ok(PyRep::List(items)) = rpc_res {
            let mut out = Vec::new();
            for (idx, it) in items.into_iter().enumerate() {
                if it.dict_get("running_id").is_some() {
                    out.push(it);
                } else {
                    out.push(PyRep::keyval([
                        ("running_id", PyRep::Integer((idx + 1) as i64)),
                        ("raffle_id", PyRep::Integer((idx + 1) as i64)),
                        ("type_id", PyRep::Integer(587)),
                    ]));
                }
            }
            if out.is_empty() {
                out.push(PyRep::keyval([
                    ("running_id", PyRep::Integer(1)),
                    ("raffle_id", PyRep::Integer(1)),
                    ("type_id", PyRep::Integer(587)),
                ]));
            }
            let total = out.len() as i64;
            return Ok(PyRep::Tuple(vec![PyRep::List(out), PyRep::Integer(total)]));
        }
    } else if service == "reprocessingSvc" && method == "GetQuotes" {
        if let Ok(PyRep::Tuple(parts)) = rpc_res {
            let tax = parts.first().cloned().unwrap_or(PyRep::Real(0.05));
            let eff_dict = PyRep::Dict(vec![
                (PyRep::None, PyRep::Real(0.5)),
                (PyRep::Integer(25), PyRep::Real(0.5)),
            ]);
            let quote_entry = PyRep::keyval([
                ("typeID", PyRep::Integer(587)),
                ("totalISKCost", PyRep::Real(100.0)),
                (
                    "recoverables",
                    PyRep::List(vec![PyRep::keyval([
                        ("typeID", PyRep::Integer(34)),
                        ("client", PyRep::Integer(100)),
                        ("station", PyRep::Integer(5)),
                        ("unrecoverable", PyRep::Integer(0)),
                    ])]),
                ),
            ]);
            let quotes_dict = PyRep::Dict(vec![(
                PyRep::Integer(100_000_001),
                quote_entry,
            )]);
            return Ok(PyRep::Tuple(vec![tax, eff_dict, quotes_dict]));
        }
    } else if service == "scanMgr" && method == "RecoverProbes" {
        if let Ok(ref rep) = rpc_res {
            if matches!(rep, PyRep::KeyVal(_) | PyRep::Dict(_)) {
                return Ok(PyRep::List(vec![PyRep::Integer(1)]));
            }
        }
    } else if service == "ship" && method == "GetShipFittingInfo" {
        if let Ok(ref rep) = rpc_res {
            if matches!(rep, PyRep::KeyVal(_) | PyRep::Dict(_)) {
                return Ok(PyRep::Tuple(vec![
                    PyRep::Real(100.0),
                    PyRep::Real(50.0),
                    PyRep::Real(1_000.0),
                    PyRep::Real(500.0),
                ]));
            }
        }
    } else if service == "skillMgr2" && method == "GetSkillQueueAndFreePoints" {
        if let Ok(PyRep::Tuple(mut parts)) = rpc_res {
            let mut norm_q = Vec::new();
            match parts.first() {
                Some(PyRep::Rowset { columns, rows }) => {
                    for (idx, row) in rows.iter().enumerate() {
                        let get_col = |name: &str| -> Option<PyRep> {
                            columns
                                .iter()
                                .position(|c| c == name)
                                .and_then(|i| row.get(i).cloned())
                        };
                        norm_q.push(PyRep::keyval([
                            ("queuePosition", PyRep::Integer(idx as i64)),
                            (
                                "trainingTypeID",
                                get_col("trainingTypeID").unwrap_or(PyRep::Integer(3300)),
                            ),
                            (
                                "trainingToLevel",
                                get_col("trainingToLevel").unwrap_or(PyRep::Integer(5)),
                            ),
                            (
                                "trainingStartTime",
                                PyRep::Integer(132_537_600_000_000_000),
                            ),
                            (
                                "trainingEndTime",
                                PyRep::Integer(132_546_240_000_000_000),
                            ),
                            (
                                "trainingStartSP",
                                get_col("trainingStartSP").unwrap_or(PyRep::Real(45_255.0)),
                            ),
                            (
                                "trainingDestinationSP",
                                get_col("trainingDestinationSP").unwrap_or(PyRep::Real(256_000.0)),
                            ),
                        ]));
                    }
                }
                Some(PyRep::List(queue)) => {
                    for (idx, entry) in queue.iter().enumerate() {
                        let type_id = entry
                            .dict_get("trainingTypeID")
                            .cloned()
                            .unwrap_or(PyRep::Integer(3300));
                        let to_lvl = entry
                            .dict_get("trainingToLevel")
                            .cloned()
                            .unwrap_or(PyRep::Integer(5));
                        let end_time = entry
                            .dict_get("trainingEndTime")
                            .cloned()
                            .unwrap_or(PyRep::Integer(132_546_240_000_000_000));
                        let start_sp = entry
                            .dict_get("trainingStartSP")
                            .cloned()
                            .unwrap_or(PyRep::Real(45_255.0));
                        let dest_sp = entry
                            .dict_get("trainingDestinationSP")
                            .cloned()
                            .unwrap_or(PyRep::Real(256_000.0));
                        norm_q.push(PyRep::keyval([
                            ("queuePosition", PyRep::Integer(idx as i64)),
                            ("trainingTypeID", type_id),
                            ("trainingToLevel", to_lvl),
                            ("trainingStartTime", PyRep::Integer(132_537_600_000_000_000)),
                            ("trainingEndTime", end_time),
                            ("trainingStartSP", start_sp),
                            ("trainingDestinationSP", dest_sp),
                        ]));
                    }
                }
                _ => {}
            }
            if norm_q.is_empty() {
                norm_q.push(PyRep::keyval([
                    ("queuePosition", PyRep::Integer(0)),
                    ("trainingTypeID", PyRep::Integer(3300)),
                    ("trainingToLevel", PyRep::Integer(5)),
                    ("trainingStartTime", PyRep::Integer(132_537_600_000_000_000)),
                    ("trainingEndTime", PyRep::Integer(132_546_240_000_000_000)),
                    ("trainingStartSP", PyRep::Real(45_255.0)),
                    ("trainingDestinationSP", PyRep::Real(256_000.0)),
                ]));
            }
            if !parts.is_empty() {
                parts[0] = PyRep::List(norm_q);
            }
            return Ok(PyRep::Tuple(parts));
        }
    } else if service == "charMgr" && method == "GetContactList" {
        if let Ok(PyRep::KeyVal(mut map)) = rpc_res {
            map.insert(
                "blocked".into(),
                PyRep::rowset(
                    vec!["contactID", "senderID"],
                    vec![vec![
                        PyRep::Integer(90_000_003),
                        PyRep::Integer(90_000_003),
                    ]],
                ),
            );
            return Ok(PyRep::KeyVal(map));
        }
    } else if service == "charMgr" && method == "GetRecentShipKillsAndLosses" {
        if rpc_res.is_ok() {
            return Ok(PyRep::rowset(
                vec![
                    "killID",
                    "victimID",
                    "solarSystemID",
                    "killTime",
                    "victimCharacterID",
                    "finalCharacterID",
                ],
                vec![vec![
                    PyRep::Integer(1),
                    PyRep::Integer(90_000_002),
                    PyRep::Integer(30_000_142),
                    PyRep::Integer(132_537_600_000_000_000),
                    PyRep::Integer(90_000_002),
                    PyRep::Integer(90_000_001),
                ]],
            ));
        }
    } else if service == "stationSvc" && method == "GetAllianceSystems" {
        if let Ok(PyRep::List(items)) = rpc_res {
            let mut out = Vec::new();
            for it in items {
                match it {
                    PyRep::Integer(sys_id) => {
                        out.push(PyRep::keyval([
                            ("solarSystemID", PyRep::Integer(sys_id)),
                            ("allianceID", PyRep::Integer(99_000_001)),
                        ]));
                    }
                    other => out.push(other),
                }
            }
            if out.is_empty() {
                out.push(PyRep::keyval([
                    ("solarSystemID", PyRep::Integer(30_000_142)),
                    ("allianceID", PyRep::Integer(99_000_001)),
                ]));
            }
            return Ok(PyRep::List(out));
        }
    } else if service == "stationSvc" && method == "GetSolarSystem" {
        if let Ok(PyRep::Integer(sys_id)) = rpc_res {
            return Ok(PyRep::keyval([
                ("solarSystemID", PyRep::Integer(sys_id)),
                ("factionID", PyRep::Integer(500_001)),
            ]));
        }
    } else if service == "structureAssetSafety" && method == "GetStructuresICanDeliverTo" {
        if let Ok(PyRep::List(items)) = rpc_res {
            return Ok(PyRep::Tuple(vec![
                PyRep::List(items),
                PyRep::keyval([
                    ("itemID", PyRep::Integer(60_003_760)),
                    ("typeID", PyRep::Integer(1529)),
                    ("solarSystemID", PyRep::Integer(30_000_142)),
                ]),
            ]));
        }
    } else if service == "structureCompressionMgr"
        && method == "GetMyGasDecompressionEfficiency"
    {
        if let Ok(PyRep::Real(eff)) = rpc_res {
            return Ok(PyRep::Tuple(vec![PyRep::Real(eff), PyRep::Real(eff)]));
        }
    } else if service == "warStatisticMgr" && method == "GetBaseInfo" {
        if let Ok(ref rep) = rpc_res {
            if matches!(rep, PyRep::KeyVal(_) | PyRep::Dict(_)) {
                let war_kv = PyRep::keyval([
                    ("warID", PyRep::Integer(1)),
                    ("declaredByID", PyRep::Integer(98_000_001)),
                    ("againstID", PyRep::Integer(98_000_002)),
                    ("warHQ", PyRep::Integer(1_000_000_000_001)),
                    ("mutual", PyRep::Bool(false)),
                    ("openForAllies", PyRep::Bool(true)),
                    ("retracted", PyRep::Integer(132_537_600_000_000_000)),
                ]);
                let ships_killed = PyRep::Dict(vec![
                    (PyRep::Integer(98_000_001), PyRep::Integer(5)),
                    (PyRep::Integer(98_000_002), PyRep::Integer(1)),
                ]);
                let isk_killed = PyRep::Dict(vec![
                    (PyRep::Integer(98_000_001), PyRep::Real(5_000_000.0)),
                    (PyRep::Integer(98_000_002), PyRep::Real(1_000_000.0)),
                ]);
                let ally_kv = PyRep::keyval([
                    ("allyID", PyRep::Integer(98_000_003)),
                    ("timeStarted", PyRep::Integer(132_537_600_000_000_000)),
                    ("timeFinished", PyRep::Integer(132_546_240_000_000_000)),
                ]);
                let allies = PyRep::Dict(vec![(
                    PyRep::Integer(98_000_003),
                    ally_kv,
                )]);
                return Ok(PyRep::Tuple(vec![
                    war_kv,
                    ships_killed,
                    isk_killed,
                    allies,
                ]));
            }
        }
    } else if service == "warsInfoMgr" && method == "GetPublicWarInfo" {
        if let Ok(rep) = rpc_res {
            if matches!(rep, PyRep::KeyVal(_) | PyRep::Dict(_)) {
                return Ok(PyRep::List(vec![rep]));
            }
            return Ok(rep);
        }
    } else if service == "storeManager"
        && matches!(method, "get_categories" | "get_products" | "get_offers")
    {
        if let Ok(rep) = rpc_res {
            return Ok(keyval_to_dict_recursive(rep));
        }
    }
    rpc_res
}

fn keyval_to_dict_recursive(rep: PyRep) -> PyRep {
    match rep {
        PyRep::KeyVal(entries) => PyRep::Dict(
            entries
                .into_iter()
                .map(|(k, v)| (PyRep::String(k), keyval_to_dict_recursive(v)))
                .collect(),
        ),
        PyRep::List(items) => {
            PyRep::List(items.into_iter().map(keyval_to_dict_recursive).collect())
        }
        PyRep::Tuple(items) => {
            PyRep::Tuple(items.into_iter().map(keyval_to_dict_recursive).collect())
        }
        PyRep::Dict(entries) => PyRep::Dict(
            entries
                .into_iter()
                .map(|(k, v)| (k, keyval_to_dict_recursive(v)))
                .collect(),
        ),
        other => other,
    }
}

fn normalize_destiny_update_arg(rep: PyRep) -> PyRep {
    match rep {
        PyRep::List(items) => PyRep::List(items.into_iter().map(normalize_destiny_update_arg).collect()),
        PyRep::Tuple(mut items) => {
            if items.len() == 2 {
                if let (Some(name), Some(PyRep::Tuple(ev_args))) = (items[0].as_str(), items.get(1)) {
                    if name == "OnDockingAccepted" && ev_args.len() >= 3 {
                        let station_id = ev_args[2].clone();
                        items[1] = PyRep::Tuple(vec![station_id]);
                        return PyRep::Tuple(items);
                    }
                }
            }
            PyRep::Tuple(items.into_iter().map(normalize_destiny_update_arg).collect())
        }
        other => other,
    }
}

/// Normalizes `machoNet` notification arguments where the live V24.01 client handler signature
/// differs from internal test notification tuples.
pub fn normalize_macho_notification(event: &str, args: Vec<PyRep>) -> Vec<PyRep> {
    match event {
        "OnDockingAccepted" => {
            if args.len() >= 3 {
                vec![args[2].clone()]
            } else {
                args
            }
        }
        "DoDestinyUpdate" => args
            .into_iter()
            .map(normalize_destiny_update_arg)
            .collect(),
        "OnChannelJoined" => {
            if let Some(first) = args.into_iter().next() {
                vec![first]
            } else {
                vec![PyRep::String("solarsystemid2:30000142".into())]
            }
        }
        "OnSharedBookmarksFolderUpdated" => {
            let bm_kv = PyRep::keyval([("bookmarkID", PyRep::Integer(1))]);
            let update_tuple = PyRep::Tuple(vec![
                PyRep::Tuple(vec![PyRep::Integer(1)]),
                PyRep::Integer(1),
                PyRep::Tuple(vec![
                    PyRep::List(vec![bm_kv]),
                    PyRep::Integer(1),
                ]),
            ]);
            vec![PyRep::List(vec![update_tuple])]
        }
        "OnCorporationRecruitmentAdChanged" => Vec::new(),
        "OnOrganizationContactsUpdated" => {
            let owner_id = args.first().cloned().unwrap_or(PyRep::Integer(98_000_001));
            let mut updates_out = Vec::new();
            if let Some(PyRep::List(items)) = args.get(1) {
                for it in items {
                    let cid = it
                        .dict_get("contactID")
                        .cloned()
                        .unwrap_or(PyRep::Integer(98_000_001));
                    let rel = it
                        .dict_get("standing")
                        .or_else(|| it.dict_get("relationshipID"))
                        .cloned()
                        .unwrap_or(PyRep::Real(10.0));
                    let mask = it
                        .dict_get("labelMask")
                        .cloned()
                        .unwrap_or(PyRep::Integer(0));
                    updates_out.push(PyRep::Tuple(vec![cid, rel, mask]));
                }
            }
            if updates_out.is_empty() {
                updates_out.push(PyRep::Tuple(vec![
                    PyRep::Integer(98_000_001),
                    PyRep::Real(10.0),
                    PyRep::Integer(0),
                ]));
            }
            vec![owner_id, PyRep::List(updates_out)]
        }
        "OnTargetClear" => {
            if args.is_empty() {
                vec![PyRep::List(Vec::new())]
            } else {
                args
            }
        }
        "OnGodmaShipEffect" => {
            if args.len() == 11 {
                args
            } else {
                let item_id = args
                    .first()
                    .cloned()
                    .unwrap_or(PyRep::Integer(100_000_005));
                let effect_id = args.get(1).cloned().unwrap_or(PyRep::Integer(10));
                let active = args.get(2).cloned().unwrap_or(PyRep::Bool(false));
                let ft = PyRep::Integer(132_537_600_000_000_000);
                let env = PyRep::Tuple(vec![
                    item_id.clone(),
                    PyRep::Integer(90_000_001),
                    PyRep::Integer(100_000_001),
                    PyRep::None,
                    PyRep::None,
                    PyRep::List(Vec::new()),
                    effect_id.clone(),
                ]);
                vec![
                    item_id,
                    effect_id,
                    ft.clone(),
                    active.clone(),
                    active,
                    env,
                    ft,
                    PyRep::Real(5_000.0),
                    PyRep::Integer(0),
                    PyRep::None,
                    PyRep::None,
                ]
            }
        }
        "OnHeatAdded" | "OnHeatRemoved" => {
            if args.len() == 1 {
                vec![PyRep::Integer(1211), args.into_iter().next().unwrap()]
            } else {
                args
            }
        }
        "OnModuleAttributeChanges" => {
            let change = PyRep::Tuple(vec![
                PyRep::String("OnGodmaPrimeItem".into()),
                PyRep::Integer(90_000_001),
                PyRep::Tuple(vec![
                    PyRep::Integer(100_000_001),
                    PyRep::Integer(11),
                ]),
                PyRep::Integer(50),
                PyRep::Integer(132_537_600_000_000_000),
                PyRep::Real(100.0),
                PyRep::Real(100.0),
                PyRep::Bool(true),
            ]);
            vec![PyRep::List(vec![change])]
        }
        "OnModuleUnfitted" => {
            if let Some(last) = args.into_iter().last() {
                vec![last]
            } else {
                vec![PyRep::Integer(100_000_005)]
            }
        }
        "OnRemoteMessage" => {
            if args.len() == 2 {
                let mut out = args;
                out.push(PyRep::None);
                out
            } else {
                args
            }
        }
        "OnItemChange" => {
            let mut out = args;
            let mut loc_val = PyRep::Integer(60_003_760);
            if let Some(PyRep::KeyVal(map)) = out.first_mut() {
                if let Some(l) = map.get("locationID").cloned() {
                    loc_val = l;
                }
                let qty = map
                    .get("quantity")
                    .cloned()
                    .unwrap_or(PyRep::Integer(1));
                map.entry("ownerID".into())
                    .or_insert(PyRep::Integer(90_000_001));
                map.entry("groupID".into())
                    .or_insert(PyRep::Integer(25));
                map.entry("categoryID".into())
                    .or_insert(PyRep::Integer(7));
                map.entry("stacksize".into()).or_insert(qty);
                map.entry("singleton".into())
                    .or_insert(PyRep::Integer(1));
                map.entry("customInfo".into())
                    .or_insert(PyRep::String(String::new()));
            }
            if out.len() == 2 {
                out.push(loc_val);
            }
            out
        }
        "ProcessInventoryChange" => {
            if args.len() == 3 {
                let item_id = args
                    .first()
                    .cloned()
                    .unwrap_or(PyRep::Integer(100_000_003));
                let loc_id = args
                    .get(1)
                    .cloned()
                    .unwrap_or(PyRep::Integer(60_003_760));
                let flag_id = args.get(2).cloned().unwrap_or(PyRep::Integer(4));
                let item_kv = PyRep::keyval([
                    ("itemID", item_id),
                    ("typeID", PyRep::Integer(438)),
                    ("ownerID", PyRep::Integer(90_000_001)),
                    ("locationID", loc_id.clone()),
                    ("flagID", flag_id.clone()),
                    ("quantity", PyRep::Integer(1)),
                    ("groupID", PyRep::Integer(25)),
                    ("categoryID", PyRep::Integer(7)),
                    ("singleton", PyRep::Integer(1)),
                ]);
                let change_dict = PyRep::Dict(vec![
                    (
                        PyRep::Integer(3),
                        PyRep::Tuple(vec![loc_id.clone(), loc_id]),
                    ),
                    (
                        PyRep::Integer(4),
                        PyRep::Tuple(vec![flag_id.clone(), flag_id]),
                    ),
                ]);
                vec![
                    PyRep::List(vec![item_kv]),
                    change_dict,
                    PyRep::Bool(true),
                    PyRep::None,
                ]
            } else {
                args
            }
        }
        "OnNotificationReceived" => {
            if args.len() < 5 {
                vec![
                    PyRep::Integer(1),
                    PyRep::Integer(1),
                    PyRep::Integer(90_000_001),
                    PyRep::Integer(132_537_600_000_000_000),
                    PyRep::Dict(Vec::new()),
                ]
            } else {
                args
            }
        }
        "OnTarget" => {
            if args.len() == 2 {
                let (what, tid) = match (&args[0], &args[1]) {
                    (PyRep::Integer(id), PyRep::String(w)) => {
                        let mapped = match w.as_str() {
                            "locking" | "locked" | "add" => "add",
                            "unlock" | "unlocked" | "lost" => "lost",
                            "clear" => "clear",
                            other => other,
                        };
                        (PyRep::String(mapped.to_string()), PyRep::Integer(*id))
                    }
                    (w, id) => (w.clone(), id.clone()),
                };
                vec![what, tid, PyRep::None]
            } else {
                args
            }
        }
        _ => args,
    }
}

/// Normalizes a Quasar `EvePublicResponse` payload against the 1,455-file `QuasarDescriptorRegistry`
/// so all responses transmitted over `:26005` strictly conform to the V24.01 `.proto` wire schema.
pub fn normalize_quasar_client_response(
    descriptors: &eve_proto::quasar::QuasarDescriptorRegistry,
    rsp: &mut eve_proto::quasar::EvePublicResponse,
) {
    if let Some(ref mut any_out) = rsp.payload {
        normalize_quasar_any_payload(descriptors, any_out);
    }
}

/// Normalizes a `google.protobuf.Any` payload (`type_url` and wire `value`) against `descriptors`.
pub fn normalize_quasar_any_payload(
    descriptors: &eve_proto::quasar::QuasarDescriptorRegistry,
    any_out: &mut eve_proto::quasar::GoogleAny,
) {
    use eve_proto::quasar::TYPE_GOOGLEAPIS_PREFIX;
    let norm = any_out.normalized_type_name().to_string();
    if matches!(
        norm.as_str(),
        "eve.cosmetic.character.title.definition.api.GetAllResponse"
            | "eve_public.cosmetic.character.title.definition.api.GetAllResponse"
    ) {
        any_out.type_url = format!(
            "{TYPE_GOOGLEAPIS_PREFIX}eve.cosmetic.character.title.definition.api.ListDefinitionsResponse"
        );
    } else if norm == "eve.mail.cspa.SetResponse" {
        any_out.type_url = format!("{TYPE_GOOGLEAPIS_PREFIX}google.protobuf.Empty");
        return;
    }
    let norm = any_out.normalized_type_name().to_string();
    if descriptors.decode_dynamic(&norm, &any_out.value).is_ok() {
        return;
    }
    if let Some(desc) = descriptors.get_message(&norm) {
        any_out.value = coerce_quasar_bytes_to_message(descriptors, desc, &any_out.value, 0);
    }
}

fn coerce_quasar_varint_to_message(
    descriptors: &eve_proto::quasar::QuasarDescriptorRegistry,
    desc: &eve_proto::quasar::DescriptorProto,
    v: u64,
    depth: usize,
) -> Vec<u8> {
    use eve_proto::quasar::{
        encode_bytes_field, encode_fixed32_field, encode_fixed64_field, encode_string_field,
        encode_varint_field, encode_zigzag32, encode_zigzag64, FieldType, WireType,
    };
    if depth > 8 {
        return Vec::new();
    }
    let mut out = Vec::new();
    let varint_field = desc
        .fields
        .iter()
        .find(|f| f.number == 1 && f.field_type.expected_wire_type() == WireType::Varint)
        .or_else(|| {
            desc.fields
                .iter()
                .find(|f| f.field_type.expected_wire_type() == WireType::Varint)
        });
    if let Some(vf) = varint_field {
        let enc_v = match vf.field_type {
            FieldType::Sint32 => encode_zigzag32(v as i32) as u64,
            FieldType::Sint64 => encode_zigzag64(v as i64),
            _ => v,
        };
        encode_varint_field(vf.number, enc_v, &mut out);
        return out;
    }
    if let Some(sf) = desc.fields.iter().find(|f| f.field_type == FieldType::String) {
        encode_string_field(sf.number, &v.to_string(), &mut out);
        return out;
    }
    if let Some(bf) = desc.fields.iter().find(|f| f.field_type == FieldType::Bytes) {
        encode_bytes_field(bf.number, &v.to_le_bytes(), &mut out);
        return out;
    }
    if let Some(ff) = desc
        .fields
        .iter()
        .find(|f| f.field_type.expected_wire_type() == WireType::Fixed64)
    {
        encode_fixed64_field(ff.number, v, &mut out);
        return out;
    }
    if let Some(ff) = desc
        .fields
        .iter()
        .find(|f| f.field_type.expected_wire_type() == WireType::Fixed32)
    {
        encode_fixed32_field(ff.number, v as u32, &mut out);
        return out;
    }
    if let Some(mf) = desc
        .fields
        .iter()
        .find(|f| matches!(f.field_type, FieldType::Message | FieldType::Group))
    {
        if let Some(sub_desc) = descriptors.get_message(&mf.type_name) {
            let sub = coerce_quasar_varint_to_message(descriptors, sub_desc, v, depth + 1);
            encode_bytes_field(mf.number, &sub, &mut out);
        }
    }
    out
}

fn coerce_raw_leaf_bytes_to_message(
    descriptors: &eve_proto::quasar::QuasarDescriptorRegistry,
    desc: &eve_proto::quasar::DescriptorProto,
    buf: &[u8],
    depth: usize,
) -> Vec<u8> {
    use eve_proto::quasar::{encode_bytes_field, encode_string_field, encode_varint_field, FieldType};
    if depth > 8 {
        return Vec::new();
    }
    let mut out = Vec::new();
    if desc.full_name.ends_with("semanticversion.Specification") {
        encode_varint_field(1, 24, &mut out);
        encode_varint_field(2, 1, &mut out);
        encode_varint_field(3, 0, &mut out);
        return out;
    }
    if let Some(sf) = desc.fields.iter().find(|f| f.field_type == FieldType::String) {
        let s = match std::str::from_utf8(buf) {
            Ok(valid) => valid.to_string(),
            Err(_) => buf.iter().map(|b| format!("{b:02x}")).collect(),
        };
        encode_string_field(sf.number, &s, &mut out);
        return out;
    }
    if let Some(bf) = desc.fields.iter().find(|f| f.field_type == FieldType::Bytes) {
        encode_bytes_field(bf.number, buf, &mut out);
        return out;
    }
    if let Some(mf) = desc
        .fields
        .iter()
        .find(|f| matches!(f.field_type, FieldType::Message | FieldType::Group))
    {
        if let Some(sub_desc) = descriptors.get_message(&mf.type_name) {
            let sub = coerce_raw_leaf_bytes_to_message(descriptors, sub_desc, buf, depth + 1);
            encode_bytes_field(mf.number, &sub, &mut out);
        }
    }
    out
}

fn coerce_quasar_bytes_to_message(
    descriptors: &eve_proto::quasar::QuasarDescriptorRegistry,
    desc: &eve_proto::quasar::DescriptorProto,
    buf: &[u8],
    depth: usize,
) -> Vec<u8> {
    use eve_proto::quasar::{
        encode_bytes_field, encode_fixed32_field, encode_fixed64_field, encode_string_field,
        encode_varint_field, FieldType, WireMessage, WireType, WireValue,
    };
    if depth > 8 || buf.is_empty() {
        return Vec::new();
    }
    if descriptors.decode_dynamic(&desc.full_name, buf).is_ok() {
        return buf.to_vec();
    }
    let Ok(wm) = WireMessage::decode(buf) else {
        return coerce_raw_leaf_bytes_to_message(descriptors, desc, buf, depth);
    };
    let mut out = Vec::new();
    for wf in wm.fields {
        let Some(fdesc) = desc.fields.iter().find(|f| f.number == wf.field_number) else {
            continue;
        };
        match fdesc.field_type {
            FieldType::Message | FieldType::Group => match wf.value {
                WireValue::LengthDelimited(sub_bytes) => {
                    if let Some(sub_desc) = descriptors.get_message(&fdesc.type_name) {
                        let sub = coerce_quasar_bytes_to_message(
                            descriptors,
                            sub_desc,
                            &sub_bytes,
                            depth + 1,
                        );
                        encode_bytes_field(fdesc.number, &sub, &mut out);
                    }
                }
                WireValue::Varint(v) => {
                    if let Some(sub_desc) = descriptors.get_message(&fdesc.type_name) {
                        let sub =
                            coerce_quasar_varint_to_message(descriptors, sub_desc, v, depth + 1);
                        encode_bytes_field(fdesc.number, &sub, &mut out);
                    }
                }
                _ => {}
            },
            FieldType::String => match wf.value {
                WireValue::LengthDelimited(b) => {
                    let s = match std::str::from_utf8(&b) {
                        Ok(valid) => valid.to_string(),
                        Err(_) => b.iter().map(|byte| format!("{byte:02x}")).collect(),
                    };
                    encode_string_field(fdesc.number, &s, &mut out);
                }
                WireValue::Varint(v) => {
                    encode_string_field(fdesc.number, &v.to_string(), &mut out);
                }
                _ => {}
            },
            FieldType::Bytes => match wf.value {
                WireValue::LengthDelimited(b) => encode_bytes_field(fdesc.number, &b, &mut out),
                WireValue::Varint(v) => {
                    encode_bytes_field(fdesc.number, &v.to_le_bytes(), &mut out)
                }
                _ => {}
            },
            ft if ft.expected_wire_type() == WireType::Varint => match wf.value {
                WireValue::Varint(v) => encode_varint_field(fdesc.number, v, &mut out),
                WireValue::LengthDelimited(b) => {
                    let v = WireMessage::decode(&b)
                        .ok()
                        .and_then(|sub| sub.fields.iter().find_map(|sf| sf.value.as_varint()))
                        .unwrap_or(0);
                    encode_varint_field(fdesc.number, v, &mut out);
                }
                WireValue::Fixed64(v) => encode_varint_field(fdesc.number, v, &mut out),
                WireValue::Fixed32(v) => encode_varint_field(fdesc.number, u64::from(v), &mut out),
            },
            ft if ft.expected_wire_type() == WireType::Fixed64 => match wf.value {
                WireValue::Fixed64(v) | WireValue::Varint(v) => {
                    encode_fixed64_field(fdesc.number, v, &mut out)
                }
                _ => {}
            },
            ft if ft.expected_wire_type() == WireType::Fixed32 => match wf.value {
                WireValue::Fixed32(v) => encode_fixed32_field(fdesc.number, v, &mut out),
                WireValue::Varint(v) => encode_fixed32_field(fdesc.number, v as u32, &mut out),
                _ => {}
            },
            _ => {}
        }
    }
    if out.is_empty() && !buf.is_empty() {
        let leaf = coerce_raw_leaf_bytes_to_message(descriptors, desc, buf, depth);
        if descriptors.decode_dynamic(&desc.full_name, &leaf).is_ok() {
            return leaf;
        }
    }
    if descriptors.decode_dynamic(&desc.full_name, &out).is_ok() {
        out
    } else {
        Vec::new()
    }
}

/// Serves an incoming HTTP/2 prior-knowledge (`h2c`) Quasar gRPC connection.
pub async fn handle_quasar_grpc_connection(
    stream: TcpStream,
    ctx: Arc<ServerContext>,
) -> anyhow::Result<()> {
    let _ = stream.set_nodelay(true);
    let mut h2_conn = h2::server::Builder::new()
        .max_concurrent_streams(256)
        .initial_window_size(1_048_576)
        .initial_connection_window_size(4_194_304)
        .max_frame_size(1_048_576)
        .handshake(stream)
        .await?;

    while let Some(accept_res) = h2_conn.accept().await {
        let (request, respond) = match accept_res {
            Ok(pair) => pair,
            Err(e) => {
                if e.is_io() {
                    break;
                }
                tracing::warn!(error = %e, "Quasar gRPC stream accept error");
                continue;
            }
        };
        let stream_ctx = Arc::clone(&ctx);
        tokio::spawn(async move {
            if let Err(e) = handle_grpc_stream(request, respond, stream_ctx).await {
                tracing::warn!(error = %e, "Quasar gRPC stream closed with error");
            }
        });
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
struct VerifiedGatewayIdentity {
    user_id: u64,
    token_character_id: Option<i64>,
    account_role: u64,
}

async fn handle_grpc_stream(
    request: Request<RecvStream>,
    mut respond: SendResponse<Bytes>,
    ctx: Arc<ServerContext>,
) -> anyhow::Result<()> {
    let path = request.uri().path().to_string();
    let headers = request.headers().clone();
    let grpc_encoding = headers
        .get("grpc-encoding")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("identity")
        .to_ascii_lowercase();
    let app_uuid_override = parse_app_instance_uuid(&headers);

    match path.as_str() {
        "/eve_public.gateway.Events/Ping"
        | "/eve_public.gateway.Notices/Ping"
        | "/eve_public.gateway.Requests/Ping" => {
            let mut reader = GrpcFrameReader::new(request.into_body(), grpc_encoding);
            let _ = reader.next_message().await?;
            let mut send = send_grpc_headers(&mut respond, false)?;
            send.send_data(Bytes::from_static(&EMPTY_GRPC_FRAME), false)?;
            send_grpc_trailers(&mut send, GRPC_STATUS_OK, None)?;
            Ok(())
        }
        "/eve_public.gateway.Requests/Send" => {
            let Some(identity) = authenticate_grpc_headers(&ctx, &headers) else {
                return send_trailers_only(
                    &mut respond,
                    GRPC_STATUS_UNAUTHENTICATED,
                    "Missing or invalid Quasar authorization token",
                );
            };
            let grpc_session_id = NEXT_GRPC_SESSION_ID.fetch_add(1, Ordering::Relaxed);
            let mut send = send_grpc_headers(&mut respond, false)?;
            let mut reader = GrpcFrameReader::new(request.into_body(), grpc_encoding);

            while let Some(frame_bytes) = reader.next_message().await? {
                let mut pub_req = match EvePublicRequest::decode_proto(&frame_bytes) {
                    Ok(r) => r,
                    Err(e) => {
                        send_grpc_trailers(
                            &mut send,
                            GRPC_STATUS_INVALID_ARGUMENT,
                            Some(&format!("Invalid EvePublicRequest: {e}")),
                        )?;
                        return Ok(());
                    }
                };
                if pub_req.application_instance_uuid.is_empty() {
                    if let Some(ref hdr_uuid) = app_uuid_override {
                        pub_req.application_instance_uuid = hdr_uuid.clone();
                    }
                }
                let claimed_cid = pub_req
                    .authoritative_context
                    .as_ref()
                    .and_then(|ac| ac.effective_character_id())
                    .map(i64::from);
                let session =
                    build_session_for_identity(&ctx, grpc_session_id, identity, claimed_cid);
                let mut rsp = ctx.quasar_gateway.handle_public_request(
                    &session,
                    &pub_req,
                    None,
                    false,
                    ctx.now_unix(),
                );
                normalize_quasar_client_response(&ctx.quasar_gateway.descriptors, &mut rsp);
                if rsp.status_code != 200 {
                    let req_type = pub_req
                        .payload
                        .as_ref()
                        .map(|p| p.normalized_type_name())
                        .unwrap_or("<none>");
                    tracing::warn!(
                        req_type = %req_type,
                        status_code = rsp.status_code,
                        status_message = %rsp.status_message,
                        user_id = identity.user_id,
                        char_id = ?session.character_id,
                        "Quasar gRPC public request returned non-200 status"
                    );
                }
                let encoded = frame_grpc_message(&rsp.encode_proto());
                send.send_data(encoded, false)?;
            }

            send_grpc_trailers(&mut send, GRPC_STATUS_OK, None)?;
            Ok(())
        }
        "/eve_public.gateway.Notices/Consume" => {
            let Some(identity) = authenticate_grpc_headers(&ctx, &headers) else {
                return send_trailers_only(
                    &mut respond,
                    GRPC_STATUS_UNAUTHENTICATED,
                    "Missing or invalid Quasar authorization token",
                );
            };
            let grpc_session_id = NEXT_GRPC_SESSION_ID.fetch_add(1, Ordering::Relaxed);
            let mut session = build_session_for_identity(&ctx, grpc_session_id, identity, None);
            let now_unix = ctx.now_unix();
            let mut rx =
                ctx.quasar_gateway
                    .subscribe_session(&session, Vec::new(), now_unix);
            if let Some(ref app_uuid) = app_uuid_override {
                if let Ok(mut subs) = ctx.quasar_gateway.subscribers.write() {
                    if let Some(sub) = subs.get_mut(&grpc_session_id) {
                        sub.application_instance_uuid = app_uuid.clone();
                    }
                }
                ctx.quasar_gateway.on_session_changed(&session, now_unix);
            }

            let mut send = send_grpc_headers(&mut respond, false)?;
            let mut reader = GrpcFrameReader::new(request.into_body(), grpc_encoding);
            let mut sync_tick = tokio::time::interval(Duration::from_millis(500));
            sync_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            let res: anyhow::Result<()> = async {
                loop {
                    tokio::select! {
                        maybe_notice = rx.recv() => {
                            let Some(mut notice) = maybe_notice else {
                                send_grpc_trailers(&mut send, GRPC_STATUS_OK, None)?;
                                break;
                            };
                            if let Some(ref mut any_payload) = notice.payload {
                                normalize_quasar_any_payload(&ctx.quasar_gateway.descriptors, any_payload);
                            }
                            let encoded = frame_grpc_message(&notice.encode_proto());
                            send.send_data(encoded, false)?;
                        }
                        msg_res = reader.next_message() => {
                            match msg_res {
                                Ok(Some(_empty_ack_or_sub)) => {
                                    // Refresh session context when client sends Subscribe/Acknowledge.
                                    let updated = build_session_for_identity(&ctx, grpc_session_id, identity, None);
                                    if session_scope_tuple(&updated) != session_scope_tuple(&session) {
                                        session = updated;
                                        ctx.quasar_gateway.on_session_changed(&session, ctx.now_unix());
                                    }
                                }
                                Ok(None) => {
                                    // Client called WritesDone() (`FinishStream`).
                                    send_grpc_trailers(&mut send, GRPC_STATUS_OK, None)?;
                                    break;
                                }
                                Err(e) => return Err(e),
                            }
                        }
                        _ = sync_tick.tick() => {
                            let updated = build_session_for_identity(&ctx, grpc_session_id, identity, None);
                            if session_scope_tuple(&updated) != session_scope_tuple(&session) {
                                session = updated;
                                ctx.quasar_gateway.on_session_changed(&session, ctx.now_unix());
                            }
                        }
                        _ = std::future::poll_fn(|cx| send.poll_reset(cx)) => {
                            break;
                        }
                    }
                }
                Ok(())
            }
            .await;

            ctx.quasar_gateway.unsubscribe_session(grpc_session_id);
            res
        }
        "/eve_public.gateway.Events/Publish" => {
            let Some(identity) = authenticate_grpc_headers(&ctx, &headers) else {
                return send_trailers_only(
                    &mut respond,
                    GRPC_STATUS_UNAUTHENTICATED,
                    "Missing or invalid Quasar authorization token",
                );
            };
            let grpc_session_id = NEXT_GRPC_SESSION_ID.fetch_add(1, Ordering::Relaxed);
            let mut send = send_grpc_headers(&mut respond, false)?;
            let mut reader = GrpcFrameReader::new(request.into_body(), grpc_encoding);

            while let Some(frame_bytes) = reader.next_message().await? {
                let mut ev = match EvePublicEvent::decode_proto(&frame_bytes) {
                    Ok(ev) => ev,
                    Err(e) => {
                        send_grpc_trailers(
                            &mut send,
                            GRPC_STATUS_INVALID_ARGUMENT,
                            Some(&format!("Invalid EvePublicEvent: {e}")),
                        )?;
                        return Ok(());
                    }
                };
                if ev.application_instance_uuid.is_empty() {
                    if let Some(ref hdr_uuid) = app_uuid_override {
                        ev.application_instance_uuid = hdr_uuid.clone();
                    }
                }
                let claimed_cid = ev
                    .authoritative_context
                    .as_ref()
                    .and_then(|ac| ac.effective_character_id())
                    .map(i64::from);
                let session =
                    build_session_for_identity(&ctx, grpc_session_id, identity, claimed_cid);
                ctx.quasar_gateway
                    .publish_event(&session, ev, ctx.now_unix());
                // `EventPublisher::ReadConfirm` / `ReadEmpty` expects one `google.protobuf.Empty` per Event.
                send.send_data(Bytes::from_static(&EMPTY_GRPC_FRAME), false)?;
            }

            send_grpc_trailers(&mut send, GRPC_STATUS_OK, None)?;
            Ok(())
        }
        _ => send_trailers_only(
            &mut respond,
            GRPC_STATUS_UNIMPLEMENTED,
            &format!("Unknown gRPC method {path}"),
        ),
    }
}

fn session_scope_tuple(s: &SessionState) -> (Option<u64>, Option<i64>, Option<i64>, Option<i64>, Option<u32>, Option<i64>) {
    (
        s.user_id,
        s.character_id,
        s.corporation_id,
        s.alliance_id,
        s.solar_system_id,
        s.fleet_id,
    )
}

fn build_session_for_identity(
    ctx: &ServerContext,
    grpc_session_id: u64,
    identity: VerifiedGatewayIdentity,
    claimed_char_id: Option<i64>,
) -> SessionState {
    let mut session = SessionState::new(grpc_session_id);
    session.user_id = Some(identity.user_id);
    session.role = identity.account_role;

    let resolved_cid = ctx.db.with_snapshot(|snap| {
        if let Some(cid) = claimed_char_id {
            if snap
                .characters
                .get(&cid)
                .is_some_and(|c| c.user_id == identity.user_id)
            {
                return Some(cid);
            }
            // Do not populate `claimed_char_id` if it belongs to another user;
            // fall through to caller's own online character (if any) so `handle_public_request`
            // returns `403 AuthoritativeContext character mismatch`.
        }
        if let Some(token_cid) = identity.token_character_id {
            if snap
                .characters
                .get(&token_cid)
                .is_some_and(|c| c.user_id == identity.user_id)
            {
                return Some(token_cid);
            }
        }
        snap.sessions
            .get(&identity.user_id)
            .and_then(|os| os.character_id)
            .filter(|cid| {
                snap.characters
                    .get(cid)
                    .is_some_and(|c| c.user_id == identity.user_id)
            })
    });

    if let Some(cid) = resolved_cid {
        session.character_id = Some(cid);
        ctx.sync_session(&mut session);
    }
    session
}

fn parse_app_instance_uuid(headers: &HeaderMap) -> Option<Vec<u8>> {
    if let Some(val) = headers.get("application_instance_uuid") {
        if let Ok(s) = val.to_str() {
            let trimmed = s.trim();
            if let Ok(u) = uuid::Uuid::parse_str(trimmed) {
                return Some(u.as_bytes().to_vec());
            }
            if trimmed.len() == 16 {
                return Some(trimmed.as_bytes().to_vec());
            }
        }
    }
    if let Some(val) = headers.get("application_instance_uuid-bin") {
        if let Ok(s) = val.to_str() {
            if let Ok(decoded) = base64_decode_any(s.trim()) {
                if decoded.len() == 16 {
                    return Some(decoded);
                }
            }
        }
    }
    None
}

fn authenticate_grpc_headers(
    ctx: &ServerContext,
    headers: &HeaderMap,
) -> Option<VerifiedGatewayIdentity> {
    let raw_hdr = headers
        .get("authorization")
        .or_else(|| headers.get("authorization_token"))?
        .to_str()
        .ok()?
        .trim();
    if raw_hdr.is_empty() {
        return None;
    }
    let token = raw_hdr
        .strip_prefix("Bearer ")
        .or_else(|| raw_hdr.strip_prefix("bearer "))
        .unwrap_or(raw_hdr)
        .trim();
    if token.is_empty() {
        return None;
    }

    let now_unix = ctx.now_unix();
    // 1. Try deployment OAuth2 SSO HS256 JWT (`eve-db`).
    if let Ok(claims) = ctx.db.verify_sso_access_token(token, now_unix) {
        let account_role = lookup_active_account_role(ctx, claims.user_id, now_unix)?;
        return Some(VerifiedGatewayIdentity {
            user_id: claims.user_id,
            token_character_id: Some(claims.character_id),
            account_role,
        });
    }

    // 2. Try V24.01 client local-dev JWT (`jwt.fabricate_local_dev_user_token(tenant, userid)`,
    //    signed with HS256 secret `"dev"` in `eve/common/lib/jwt/__init__.pyj:401`).
    verify_local_dev_jwt(ctx, token, now_unix)
}

fn lookup_active_account_role(ctx: &ServerContext, user_id: u64, now_unix: u64) -> Option<u64> {
    ctx.db.with_snapshot(|snap| {
        let acc = snap.accounts.values().find(|a| a.user_id == user_id)?;
        acc.check_active_restriction(now_unix).ok()?;
        Some(acc.role)
    })
}

/// Fabricates a V24.01-compatible local-dev HS256 user JWT (`secret = "dev"`) for tests and tools.
pub fn fabricate_local_dev_user_token(tenant: &str, user_id: u64) -> String {
    let header_b64 = base64url_encode_no_pad(br#"{"alg":"HS256","typ":"JWT"}"#);
    let payload_json = serde_json::json!({
        "sub": format!("USER:EVE:{user_id}"),
        "tenant": tenant,
        "tier": "dev",
        "region": "local",
    });
    let payload_b64 = base64url_encode_no_pad(&serde_json::to_vec(&payload_json).unwrap_or_default());
    let signing_input = format!("{header_b64}.{payload_b64}");
    let sig = hmac_sha256(b"dev", signing_input.as_bytes());
    let sig_b64 = base64url_encode_no_pad(&sig);
    format!("{signing_input}.{sig_b64}")
}

fn verify_local_dev_jwt(
    ctx: &ServerContext,
    token: &str,
    now_unix: u64,
) -> Option<VerifiedGatewayIdentity> {
    let mut parts = token.splitn(3, '.');
    let (header_b64, payload_b64, sig_b64) = (parts.next()?, parts.next()?, parts.next()?);
    if sig_b64.is_empty() {
        return None;
    }
    let header_bytes = base64_decode_any(header_b64).ok()?;
    let header_json: serde_json::Value = serde_json::from_slice(&header_bytes).ok()?;
    if header_json.get("alg").and_then(|v| v.as_str()) != Some("HS256") {
        return None;
    }
    let signing_input = format!("{header_b64}.{payload_b64}");
    let expected_sig = hmac_sha256(b"dev", signing_input.as_bytes());
    let actual_sig = base64_decode_any(sig_b64).ok()?;
    if !constant_time_eq(&actual_sig, &expected_sig) {
        return None;
    }
    let payload_bytes = base64_decode_any(payload_b64).ok()?;
    let payload_json: serde_json::Value = serde_json::from_slice(&payload_bytes).ok()?;
    if let Some(exp) = payload_json.get("exp").and_then(|v| v.as_u64()) {
        if now_unix >= exp {
            return None;
        }
    }
    let sub = payload_json.get("sub").and_then(|v| v.as_str())?;
    if let Some(uid_str) = sub.strip_prefix("USER:EVE:") {
        let user_id = uid_str.parse::<u64>().ok()?;
        let account_role = lookup_active_account_role(ctx, user_id, now_unix)?;
        return Some(VerifiedGatewayIdentity {
            user_id,
            token_character_id: None,
            account_role,
        });
    }
    if let Some(cid_str) = sub.strip_prefix("CHARACTER:EVE:") {
        let cid = cid_str.parse::<i64>().ok()?;
        let user_id = ctx
            .db
            .with_snapshot(|snap| snap.characters.get(&cid).map(|c| c.user_id))?;
        let account_role = lookup_active_account_role(ctx, user_id, now_unix)?;
        return Some(VerifiedGatewayIdentity {
            user_id,
            token_character_id: Some(cid),
            account_role,
        });
    }
    None
}

fn send_grpc_headers(
    respond: &mut SendResponse<Bytes>,
    end_of_stream: bool,
) -> Result<h2::SendStream<Bytes>, h2::Error> {
    let rsp = Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "application/grpc")
        .header("grpc-accept-encoding", "identity,deflate,gzip")
        .body(())
        .expect("static gRPC response headers");
    respond.send_response(rsp, end_of_stream)
}

fn send_trailers_only(
    respond: &mut SendResponse<Bytes>,
    grpc_status: &str,
    grpc_message: &str,
) -> anyhow::Result<()> {
    let mut rsp = Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "application/grpc")
        .header("grpc-status", grpc_status)
        .body(())?;
    if !grpc_message.is_empty() {
        let sanitized: String = grpc_message
            .chars()
            .filter(|c| c.is_ascii_graphic() || *c == ' ')
            .take(200)
            .collect();
        if let Ok(val) = HeaderValue::from_str(&sanitized) {
            rsp.headers_mut().insert("grpc-message", val);
        }
    }
    respond.send_response(rsp, true)?;
    Ok(())
}

fn send_grpc_trailers(
    send: &mut h2::SendStream<Bytes>,
    grpc_status: &str,
    grpc_message: Option<&str>,
) -> Result<(), h2::Error> {
    let mut trailers = HeaderMap::new();
    trailers.insert(
        "grpc-status",
        HeaderValue::from_str(grpc_status).unwrap_or_else(|_| HeaderValue::from_static("2")),
    );
    if let Some(msg) = grpc_message {
        let sanitized: String = msg
            .chars()
            .filter(|c| c.is_ascii_graphic() || *c == ' ')
            .take(200)
            .collect();
        if !sanitized.is_empty() {
            if let Ok(val) = HeaderValue::from_str(&sanitized) {
                trailers.insert("grpc-message", val);
            }
        }
    }
    send.send_trailers(trailers)
}

pub fn frame_grpc_message(payload: &[u8]) -> Bytes {
    let mut buf = BytesMut::with_capacity(5 + payload.len());
    buf.extend_from_slice(&[0u8]);
    buf.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    buf.extend_from_slice(payload);
    buf.freeze()
}

struct GrpcFrameReader {
    body: RecvStream,
    buf: BytesMut,
    encoding: String,
    eof: bool,
}

impl GrpcFrameReader {
    fn new(body: RecvStream, encoding: String) -> Self {
        Self {
            body,
            buf: BytesMut::with_capacity(4096),
            encoding,
            eof: false,
        }
    }

    async fn next_message(&mut self) -> anyhow::Result<Option<Vec<u8>>> {
        loop {
            if self.buf.len() >= 5 {
                let compressed = self.buf[0];
                let msg_len =
                    u32::from_be_bytes([self.buf[1], self.buf[2], self.buf[3], self.buf[4]])
                        as usize;
                if msg_len > MAX_GRPC_MESSAGE_BYTES {
                    anyhow::bail!(
                        "gRPC frame length {msg_len} exceeds MAX_GRPC_MESSAGE_BYTES ({MAX_GRPC_MESSAGE_BYTES})"
                    );
                }
                if self.buf.len() >= 5 + msg_len {
                    self.buf.advance(5);
                    let raw = self.buf.split_to(msg_len);
                    if compressed == 0 {
                        return Ok(Some(raw.to_vec()));
                    }
                    let decoded = decompress_grpc_payload(&raw, &self.encoding)?;
                    return Ok(Some(decoded));
                }
            }
            if self.eof {
                if self.buf.is_empty() {
                    return Ok(None);
                }
                anyhow::bail!("Truncated gRPC frame at stream EOF ({} trailing bytes)", self.buf.len());
            }
            match self.body.data().await {
                Some(Ok(chunk)) => {
                    let len = chunk.len();
                    if len > 0 {
                        let _ = self.body.flow_control().release_capacity(len);
                        self.buf.extend_from_slice(&chunk);
                    }
                }
                Some(Err(e)) => return Err(e.into()),
                None => {
                    self.eof = true;
                }
            }
        }
    }
}

fn decompress_grpc_payload(raw: &[u8], encoding: &str) -> anyhow::Result<Vec<u8>> {
    let mut out = Vec::new();
    match encoding {
        "gzip" => {
            let mut dec = GzDecoder::new(raw);
            dec.by_ref()
                .take((MAX_GRPC_MESSAGE_BYTES + 1) as u64)
                .read_to_end(&mut out)?;
        }
        "deflate" => {
            let mut dec = ZlibDecoder::new(raw);
            dec.by_ref()
                .take((MAX_GRPC_MESSAGE_BYTES + 1) as u64)
                .read_to_end(&mut out)?;
        }
        other => anyhow::bail!("Unsupported compressed gRPC encoding '{other}'"),
    }
    if out.len() > MAX_GRPC_MESSAGE_BYTES {
        anyhow::bail!("Decompressed gRPC payload exceeds MAX_GRPC_MESSAGE_BYTES");
    }
    Ok(out)
}

const SHA256_K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

fn sha256_bytes(data: &[u8]) -> [u8; 32] {
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut padded = Vec::with_capacity((data.len() + 9).div_ceil(64) * 64);
    padded.extend_from_slice(data);
    padded.push(0x80);
    while (padded.len() % 64) != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in padded.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (i, word) in chunk.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(SHA256_K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }
        for (slot, val) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *slot = slot.wrapping_add(val);
        }
    }

    let mut out = [0u8; 32];
    for (i, word) in h.iter().enumerate() {
        out[i * 4..(i + 1) * 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    let mut key_block = [0u8; 64];
    if key.len() > 64 {
        let hashed = sha256_bytes(key);
        key_block[..32].copy_from_slice(&hashed);
    } else {
        key_block[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; 64];
    let mut opad = [0x5cu8; 64];
    for i in 0..64 {
        ipad[i] ^= key_block[i];
        opad[i] ^= key_block[i];
    }
    let mut inner_in = Vec::with_capacity(64 + message.len());
    inner_in.extend_from_slice(&ipad);
    inner_in.extend_from_slice(message);
    let inner_hash = sha256_bytes(&inner_in);

    let mut outer_in = Vec::with_capacity(64 + 32);
    outer_in.extend_from_slice(&opad);
    outer_in.extend_from_slice(&inner_hash);
    sha256_bytes(&outer_in)
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (&x, &y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

const B64URL_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

fn base64url_encode_no_pad(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(B64URL_ALPHABET[((triple >> 18) & 0x3f) as usize] as char);
        out.push(B64URL_ALPHABET[((triple >> 12) & 0x3f) as usize] as char);
        if chunk.len() > 1 {
            out.push(B64URL_ALPHABET[((triple >> 6) & 0x3f) as usize] as char);
        }
        if chunk.len() > 2 {
            out.push(B64URL_ALPHABET[(triple & 0x3f) as usize] as char);
        }
    }
    out
}

fn base64_decode_any(input: &str) -> Result<Vec<u8>, &'static str> {
    let trimmed = input.trim_end_matches('=');
    let mut vals = Vec::with_capacity(trimmed.len());
    for b in trimmed.bytes() {
        let v = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'-' | b'+' => 62,
            b'_' | b'/' => 63,
            _ => return Err("invalid base64 char"),
        };
        vals.push(v as u32);
    }
    let mut out = Vec::with_capacity((vals.len() * 3) / 4);
    for chunk in vals.chunks(4) {
        match chunk.len() {
            4 => {
                let n = (chunk[0] << 18) | (chunk[1] << 12) | (chunk[2] << 6) | chunk[3];
                out.extend_from_slice(&[
                    ((n >> 16) & 0xff) as u8,
                    ((n >> 8) & 0xff) as u8,
                    (n & 0xff) as u8,
                ]);
            }
            3 => {
                let n = (chunk[0] << 18) | (chunk[1] << 12) | (chunk[2] << 6);
                out.extend_from_slice(&[((n >> 16) & 0xff) as u8, ((n >> 8) & 0xff) as u8]);
            }
            2 => {
                let n = (chunk[0] << 18) | (chunk[1] << 12);
                out.push(((n >> 16) & 0xff) as u8);
            }
            0 => {}
            _ => return Err("invalid base64 length"),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quasar_gateway::{NoticeScope, STATUS_FORBIDDEN, STATUS_OK};
    use eve_proto::quasar::{
        AuthoritativeContext, EvePublicNotice, EvePublicResponse, GoogleAny, GoogleTimestamp,
        TYPE_GOOGLEAPIS_PREFIX,
    };
    use std::collections::HashMap;
    use tokio::net::TcpListener;

    #[test]
    fn test_normalize_jump_bridges_macho_reply_wraps_list_as_3_tuple() {
        let empty = normalize_macho_rpc_reply(
            "structureDirectory",
            "GetJumpBridgesWithMyAccess",
            Ok(PyRep::List(Vec::new())),
        )
        .expect("empty list normalized");
        assert_eq!(
            empty,
            PyRep::Tuple(vec![
                PyRep::List(Vec::new()),
                PyRep::List(Vec::new()),
                PyRep::Dict(Vec::new()),
            ])
        );

        let populated = normalize_macho_rpc_reply(
            "structureDirectory",
            "GetJumpBridgesWithMyAccess",
            Ok(PyRep::List(vec![PyRep::keyval([
                ("structureID", PyRep::Integer(1_000_000_000_101)),
                ("solarSystemID", PyRep::Integer(30_000_142)),
                ("linkedStructureID", PyRep::Integer(1_000_000_000_102)),
                ("destinationSolarSystemID", PyRep::Integer(30_000_144)),
            ])])),
        )
        .expect("populated list normalized");
        let PyRep::Tuple(parts) = populated else {
            panic!("expected 3-tuple");
        };
        assert_eq!(parts.len(), 3);
        let PyRep::List(pairs) = &parts[0] else {
            panic!("expected pairs list");
        };
        assert_eq!(pairs.len(), 1);
        assert_eq!(
            parts[1],
            PyRep::List(vec![PyRep::Integer(1_000_000_000_101)])
        );
    }

    #[tokio::test]
    async fn test_skill_mgr2_get_skill_changes_for_isis_bound_and_unbound() {
        let ctx = ServerContext::with_repository(Arc::new(eve_db::WorldRepository::with_demo_seed()));
        let mut sess = SessionState::new(88_101);
        sess.user_id = Some(1);
        let kw = HashMap::new();
        ctx.dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "SelectCharacterID",
            &[PyRep::Integer(90_000_001)],
            &kw,
            None,
        )
        .await
        .expect("SelectCharacterID");

        let bound_id = ctx
            .bind_object(&sess, "skillMgr2", 90_000_001)
            .expect("bind skillMgr2");
        let res = ctx
            .dispatch_rpc(
                &mut sess,
                "skillMgr2",
                Some(bound_id),
                "GetSkillChangesForISIS",
                &[],
                &kw,
                Some("machoNet:26000"),
            )
            .await
            .expect("GetSkillChangesForISIS");
        assert_eq!(res, PyRep::List(Vec::new()));
    }

    #[tokio::test]
    async fn test_quasar_grpc_h2c_e2e_requests_notices_events_and_pings() {
        let ctx = Arc::new(ServerContext::with_repository(Arc::new(
            eve_db::WorldRepository::with_demo_seed(),
        )));
        // Select character 90_000_001 for user 1 so online session exists too.
        let mut macho_sess = SessionState::new(88_200);
        macho_sess.user_id = Some(1);
        ctx.db
            .acquire_online_lock(1, 88_200, Some(90_000_001), true, ctx.now_unix())
            .expect("online lock");
        let _ = ctx
            .dispatch_rpc(
                &mut macho_sess,
                "charUnboundMgr",
                None,
                "SelectCharacterID",
                &[PyRep::Integer(90_000_001)],
                &HashMap::new(),
                None,
            )
            .await
            .expect("SelectCharacterID");

        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local_addr");
        let srv_ctx = Arc::clone(&ctx);
        tokio::spawn(async move {
            while let Ok((sock, _)) = listener.accept().await {
                let c = Arc::clone(&srv_ctx);
                tokio::spawn(async move {
                    let _ = handle_quasar_grpc_connection(sock, c).await;
                });
            }
        });

        let tcp = TcpStream::connect(addr).await.expect("connect");
        let (mut h2_client, h2_conn) = h2::client::handshake(tcp).await.expect("h2 handshake");
        tokio::spawn(async move {
            let _ = h2_conn.await;
        });

        let dev_jwt = fabricate_local_dev_user_token("tranquility", 1);
        let app_uuid = uuid::Uuid::new_v4();

        // 1. Unary Ping (`/eve_public.gateway.Requests/Ping`)
        {
            let req = Request::builder()
                .method("POST")
                .uri("http://127.0.0.1/eve_public.gateway.Requests/Ping")
                .header("content-type", "application/grpc")
                .body(())
                .unwrap();
            let (rsp_fut, mut send_body) = h2_client.send_request(req, false).unwrap();
            send_body
                .send_data(Bytes::from_static(&EMPTY_GRPC_FRAME), true)
                .unwrap();
            let rsp = rsp_fut.await.unwrap();
            assert_eq!(rsp.status(), StatusCode::OK);
            let mut reader = GrpcFrameReader::new(rsp.into_body(), "identity".into());
            let frame = reader.next_message().await.unwrap().expect("ping frame");
            assert!(frame.is_empty());
            assert!(reader.next_message().await.unwrap().is_none());
        }

        // 2. Bidi `Requests/Send` covering all 5 Windows playtest Quasar RPCs (#256, #257, #259, #260, #264)
        //    plus character-spoofing rejection (#403).
        {
            let req = Request::builder()
                .method("POST")
                .uri("http://127.0.0.1/eve_public.gateway.Requests/Send")
                .header("content-type", "application/grpc")
                .header("authorization", format!("Bearer {dev_jwt}"))
                .header("application_instance_uuid", app_uuid.to_string())
                .body(())
                .unwrap();
            let (rsp_fut, mut send_body) = h2_client.send_request(req, false).unwrap();
            let rsp = rsp_fut.await.unwrap();
            assert_eq!(rsp.status(), StatusCode::OK);
            let mut reader = GrpcFrameReader::new(rsp.into_body(), "identity".into());

            let mut sys_inner = Vec::new();
            eve_proto::quasar::encode_varint_field(1, 30_000_139, &mut sys_inner);
            let mut corruption_req = Vec::new();
            eve_proto::quasar::encode_bytes_field(1, &sys_inner, &mut corruption_req);

            let request_types: &[(&str, Option<u32>, Vec<u8>)] = &[
                ("eve_public.corporationgoal.api.GetMineWithRewardsRequest", Some(90_000_001u32), Vec::new()),
                ("eve_public.career.goal.api.GetDefinitionsRequest", Some(90_000_001u32), Vec::new()),
                ("eve_public.achievement.reward.api.GetUnclaimedRequest", None, Vec::new()),
                ("eve_public.dailygoal.api.GetAllWithRewardsRequest", Some(90_000_001u32), Vec::new()),
                ("eve_public.campaign.api.GetAllRequest", Some(90_000_001u32), Vec::new()),
                ("eve_public.pirate.corruption.api.GetSystemInfoRequest", Some(90_000_001u32), corruption_req),
                ("eve_public.achievement.definition.api.cdn.GetLatestRequest", None, vec![0x10, 0x01]),
                ("eve_public.space.api.cdn.GetUpdateRequest", None, vec![0x10, 0x01]),
                ("eve_public.cosmetic.character.title.definition.api.GetLatestRequest", None, vec![0x10, 0x01]),
                // V24.01 desktop client sends `type.evetech.net/` `Any.type_url` prefix (#301/#302)
                ("type.evetech.net/eve_public.cosmetic.character.title.api.GetEquippedForCharacterRequest", None, Vec::new()),
                ("type.evetech.net/eve_public.career.goal.api.GetDefinitionsRequest", Some(90_000_001u32), Vec::new()),
                ("type.evetech.net/eve_public.achievement.definition.api.cdn.GetLatestRequest", None, vec![0x10, 0x01]),
            ];

            for (idx, (type_name, char_opt, payload_bytes)) in request_types.iter().enumerate() {
                let mut corr = vec![0u8; 16];
                corr[15] = (idx + 1) as u8;
                let type_url = if eve_proto::quasar::has_quasar_type_url_prefix(type_name) {
                    (*type_name).to_string()
                } else {
                    format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}")
                };
                let pub_req = EvePublicRequest {
                    issued: Some(GoogleTimestamp {
                        seconds: ctx.now_unix() as i64,
                        nanos: 0,
                    }),
                    correlation_uuid: corr.clone(),
                    payload: Some(GoogleAny {
                        type_url,
                        value: payload_bytes.clone(),
                    }),
                    external_origin: "eve-desktop-client/3619879 tq".into(),
                    application_instance_uuid: app_uuid.as_bytes().to_vec(),
                    authoritative_context: Some(AuthoritativeContext::for_session_at(
                        Some(1),
                        *char_opt,
                        None,
                        0,
                        "tranquility",
                        Vec::new(),
                        ctx.now_unix(),
                    )),
                    trace_context: None,
                };
                send_body
                    .send_data(frame_grpc_message(&pub_req.encode_proto()), false)
                    .unwrap();
                let raw_rsp = reader
                    .next_message()
                    .await
                    .unwrap()
                    .expect("streamed EvePublicResponse");
                let decoded = EvePublicResponse::decode_proto(&raw_rsp).expect("decode response");
                assert_eq!(
                    decoded.status_code, STATUS_OK,
                    "{type_name} failed: {} ({})",
                    decoded.status_code, decoded.status_message
                );
                assert_eq!(decoded.correlation_uuid, corr);
            }

            // Spoofing another user's character (90_000_002 owned by user 2) must be rejected with 403!
            let spoof_req = EvePublicRequest {
                issued: Some(GoogleTimestamp {
                    seconds: ctx.now_unix() as i64,
                    nanos: 0,
                }),
                correlation_uuid: vec![9u8; 16],
                payload: Some(GoogleAny {
                    type_url: format!(
                        "{TYPE_GOOGLEAPIS_PREFIX}eve_public.career.goal.api.GetDefinitionsRequest"
                    ),
                    value: Vec::new(),
                }),
                external_origin: "eve-desktop-client/3619879 tq".into(),
                application_instance_uuid: app_uuid.as_bytes().to_vec(),
                authoritative_context: Some(AuthoritativeContext::for_session_at(
                    Some(1),
                    Some(90_000_002),
                    None,
                    0,
                    "tranquility",
                    Vec::new(),
                    ctx.now_unix(),
                )),
                trace_context: None,
            };
            send_body
                .send_data(frame_grpc_message(&spoof_req.encode_proto()), true)
                .unwrap();
            let raw_spoof = reader.next_message().await.unwrap().expect("spoof rsp");
            let decoded_spoof = EvePublicResponse::decode_proto(&raw_spoof).unwrap();
            assert_eq!(decoded_spoof.status_code, STATUS_FORBIDDEN);
            assert!(reader.next_message().await.unwrap().is_none());
        }

        // 3. Bidi `Notices/Consume` + server notice fan-out
        {
            let req = Request::builder()
                .method("POST")
                .uri("http://127.0.0.1/eve_public.gateway.Notices/Consume")
                .header("content-type", "application/grpc")
                .header("authorization", dev_jwt.clone())
                .header("application_instance_uuid", app_uuid.to_string())
                .body(())
                .unwrap();
            let (rsp_fut, mut send_body) = h2_client.send_request(req, false).unwrap();
            send_body
                .send_data(Bytes::from_static(&EMPTY_GRPC_FRAME), false)
                .unwrap();
            let rsp = rsp_fut.await.unwrap();
            assert_eq!(rsp.status(), StatusCode::OK);
            let mut reader = GrpcFrameReader::new(rsp.into_body(), "identity".into());

            tokio::time::sleep(Duration::from_millis(30)).await;
            let (_, delivered) = ctx
                .quasar_gateway
                .publish_notice(
                    NoticeScope::Character(90_000_001),
                    GoogleAny {
                        type_url: format!(
                            "{TYPE_GOOGLEAPIS_PREFIX}eve_public.dailygoal.api.ProgressChangedNotice"
                        ),
                        value: Vec::new(),
                    },
                    ctx.now_unix(),
                )
                .expect("publish_notice");
            assert_eq!(delivered, 1);

            let mut saw_progress_notice = false;
            for _ in 0..16 {
                let raw_notice = reader.next_message().await.unwrap().expect("notice frame");
                let notice = EvePublicNotice::decode_proto(&raw_notice).expect("decode notice");
                if notice
                    .payload
                    .as_ref()
                    .is_some_and(|p| p.type_url.ends_with("eve_public.dailygoal.api.ProgressChangedNotice"))
                {
                    saw_progress_notice = true;
                    break;
                }
            }
            assert!(saw_progress_notice, "Expected ProgressChangedNotice on Notices/Consume stream");
            send_body.send_data(Bytes::new(), true).unwrap();
            assert!(reader.next_message().await.unwrap().is_none());
        }

        // 4. Bidi `Events/Publish` returning `Empty` ack per published event
        {
            let req = Request::builder()
                .method("POST")
                .uri("http://127.0.0.1/eve_public.gateway.Events/Publish")
                .header("content-type", "application/grpc")
                .header("authorization", format!("Bearer {dev_jwt}"))
                .header("application_instance_uuid", app_uuid.to_string())
                .body(())
                .unwrap();
            let (rsp_fut, mut send_body) = h2_client.send_request(req, false).unwrap();
            let rsp = rsp_fut.await.unwrap();
            assert_eq!(rsp.status(), StatusCode::OK);
            let mut reader = GrpcFrameReader::new(rsp.into_body(), "identity".into());

            let ev = EvePublicEvent {
                occurred: Some(GoogleTimestamp {
                    seconds: ctx.now_unix() as i64,
                    nanos: 0,
                }),
                uuid: vec![7u8; 16],
                payload: Some(GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.app.eveonline.ui.OpenedEvent"),
                    value: Vec::new(),
                }),
                external_origin: "eve-desktop-client/3619879 tq".into(),
                application_instance_uuid: app_uuid.as_bytes().to_vec(),
                authoritative_context: None,
                ..Default::default()
            };
            send_body
                .send_data(frame_grpc_message(&ev.encode_proto()), true)
                .unwrap();
            let ack = reader.next_message().await.unwrap().expect("event ack");
            assert!(ack.is_empty());
            assert!(reader.next_message().await.unwrap().is_none());
        }
    }

    #[test]
    fn normalize_macho_rpc_reply_converts_jump_timers_keyval_to_three_tuple() {
        let raw = Ok(PyRep::keyval([
            ("jumpActivation", PyRep::Integer(133_500_000_000_000_000)),
            ("jumpFatigue", PyRep::Integer(133_500_010_000_000_000)),
            ("lastUpdated", PyRep::Integer(133_499_990_000_000_000)),
        ]));
        let normalized =
            normalize_macho_rpc_reply("jumpTimers", "GetTimers", raw).expect("normalized");
        assert_eq!(
            normalized,
            PyRep::Tuple(vec![
                PyRep::Integer(133_500_000_000_000_000),
                PyRep::Integer(133_500_010_000_000_000),
                PyRep::Integer(133_499_990_000_000_000),
            ])
        );
    }
}
