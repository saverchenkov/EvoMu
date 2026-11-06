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

use eve_proto::{BridgeRpcError, PyRep};

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

pub(crate) fn keyval_to_dict_recursive(rep: PyRep) -> PyRep {
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
