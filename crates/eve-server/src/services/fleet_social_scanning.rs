//! Scanning & Dungeons (`SO-5`) and Wormholes (`SO-6`).

use crate::router::{cache_notifications::NotificationTarget, ServerContext};
use eve_db::lock_rank::AnyRwLock;
use eve_db::CosmicSignatureRecord;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_destiny::MovementMode;
use glam::DVec3;
use std::collections::HashMap;

const AU_IN_METERS: f64 = 149_597_870_700.0;

/// Reads cached cosmic signatures and anomaly IDs purely from snapshot when native signatures
/// are already seeded and none have expired (`None` triggers one-shot seeding/expiry).
fn read_system_signatures_ready(
    ctx: &ServerContext,
    sys_id: u32,
    now_unix: u64,
) -> Option<(Vec<CosmicSignatureRecord>, Vec<String>)> {
    ctx.db.with_snapshot(|snap| {
        let sigs = snap.cosmic_signatures.get(&sys_id)?;
        let has_native = sigs.iter().any(|s| {
            !s.signature_id.starts_with("K162-") && !s.signature_id.starts_with("DNG-")
        });
        if !has_native {
            return None;
        }
        let any_expired = sigs.iter().any(|s| {
            !s.collapsed
                && s.lifetime_seconds > 0
                && now_unix >= s.created_unix.saturating_add(s.lifetime_seconds)
        });
        if any_expired {
            return None;
        }
        let anom_ids = snap
            .cosmic_anomalies
            .get(&sys_id)
            .map(|l| l.iter().map(|a| a.anomaly_id.clone()).collect())
            .unwrap_or_default();
        let active = sigs.iter().filter(|s| !s.collapsed).cloned().collect();
        Some((active, anom_ids))
    })
}

fn scan_rpc_err(code: &str, message: impl Into<String>) -> BridgeRpcError {
    let msg = message.into();
    BridgeRpcError::with_client_error(
        code,
        msg.clone(),
        eve_proto::ClientError::CustomInfo(msg),
    )
}

fn exact_int_arg(rep: &PyRep) -> Option<i64> {
    match rep {
        PyRep::Integer(v) => Some(*v),
        _ => None,
    }
}

fn exact_float_arg(rep: &PyRep) -> Option<f64> {
    match rep {
        PyRep::Real(v) => Some(*v),
        PyRep::Integer(v) => Some(*v as f64),
        _ => None,
    }
}

fn parse_optional_scan_sys_arg(
    ctx: &ServerContext,
    session: &SessionState,
    caller_sys: u32,
    args: &[PyRep],
    action: &str,
) -> Result<u32, BridgeRpcError> {
    if args.len() > 1 {
        return Err(scan_rpc_err(
            "InvalidArguments",
            format!("{action} accepts at most 1 argument"),
        ));
    }
    let Some(arg) = args.first().filter(|a| !matches!(a, PyRep::None)) else {
        if caller_sys == 0 {
            return Err(scan_rpc_err(
                "NoSolarSystem",
                "Session has no active solar system",
            ));
        }
        return Ok(caller_sys);
    };
    let Some(raw_sys) = exact_int_arg(arg) else {
        return Err(scan_rpc_err(
            "InvalidSolarSystem",
            format!("Invalid solar system argument for {action}"),
        ));
    };
    let Ok(req_sys) = u32::try_from(raw_sys) else {
        return Err(scan_rpc_err(
            "InvalidSolarSystem",
            format!("Invalid solar system {raw_sys} for {action}"),
        ));
    };
    if req_sys == 0 {
        return Err(scan_rpc_err(
            "InvalidSolarSystem",
            format!("Invalid solar system 0 for {action}"),
        ));
    }
    if req_sys != caller_sys {
        if !session.has_gm_role() {
            return Err(scan_rpc_err(
                "AccessDenied",
                format!("Cannot {action} for remote solar system {req_sys}"),
            ));
        }
        if !ctx.sde.solar_systems.is_empty() && !ctx.sde.solar_systems.contains_key(&req_sys) {
            return Err(scan_rpc_err(
                "InvalidSolarSystem",
                format!("Unknown solar system {req_sys} for {action}"),
            ));
        }
    }
    Ok(req_sys)
}

fn validate_coords_triple(rep: &PyRep) -> Option<[f64; 3]> {
    let (PyRep::List(c) | PyRep::Tuple(c)) = rep else {
        return None;
    };
    if c.len() < 3 {
        return None;
    }
    let x = exact_float_arg(&c[0])?;
    let y = exact_float_arg(&c[1])?;
    let z = exact_float_arg(&c[2])?;
    (x.is_finite() && y.is_finite() && z.is_finite()).then_some([x, y, z])
}

fn validate_scan_probe_entry(rep: &PyRep) -> Option<[f64; 3]> {
    if let Some(coords) = validate_coords_triple(rep) {
        return Some(coords);
    }
    if !matches!(rep, PyRep::Dict(_) | PyRep::KeyVal(_)) {
        return None;
    }
    let find_key = |key: &str| -> Option<&PyRep> {
        match rep {
            PyRep::KeyVal(kv) => kv.iter().find(|(k, _)| *k == key).map(|(_, v)| v),
            PyRep::Dict(kv) => kv
                .iter()
                .find(|(k, _)| k.as_str() == Some(key))
                .map(|(_, v)| v),
            _ => None,
        }
    };
    if let Some(pos) = find_key("pos").or_else(|| find_key("destination")) {
        return validate_coords_triple(pos);
    }
    let x = find_key("x").and_then(exact_float_arg).filter(|v| v.is_finite())?;
    let y = find_key("y").and_then(exact_float_arg).filter(|v| v.is_finite())?;
    let z = find_key("z").and_then(exact_float_arg).filter(|v| v.is_finite())?;
    Some([x, y, z])
}

pub(crate) async fn handle_scanning_and_wormholes(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        // =====================================================================
        // SO-5: Scanning & Dungeons (`scanMgr`, `keeper`, `dungeon*`)
        // =====================================================================
        ("scanMgr", "SignalTrackerRegister") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let caller_sys = session.solar_system_id.unwrap_or(0);
            if caller_sys == 0 {
                return Some(Err(BridgeRpcError::user_error("NoSolarSystem")));
            }
            let sys_id = match parse_optional_scan_sys_arg(
                ctx,
                session,
                caller_sys,
                args,
                "register signal tracker",
            ) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let now_unix = ctx.now_unix();
            let is_docked = session.station_id.is_some() || session.structure_id.is_some();
            let sigs = if is_docked {
                ctx.db.with_snapshot(|snap| {
                    snap.cosmic_signatures
                        .get(&sys_id)
                        .map(|list| {
                            list.iter()
                                .filter(|s| {
                                    !s.collapsed
                                        && (s.lifetime_seconds == 0
                                            || now_unix
                                                < s.created_unix.saturating_add(s.lifetime_seconds))
                                })
                                .cloned()
                                .collect()
                        })
                        .unwrap_or_default()
                })
            } else {
                if !crate::services::pve_exploration::read_pve_exploration_reg(ctx, |reg| {
                    reg.seeded_systems.get(&sys_id).copied().unwrap_or(false)
                }) {
                    crate::services::pve_exploration::ensure_system_pve_sites_seeded(ctx, sys_id)
                        .await;
                }
                match read_system_signatures_ready(ctx, sys_id, now_unix) {
                    Some((ready_sigs, _)) => ready_sigs,
                    _ => ensure_system_signatures(ctx, sys_id).await,
                }
            };
            let (anom_count, anom_rows, struct_rows, sig_count) = ctx.db.with_snapshot(|snap| {
                let anom_ids: std::collections::HashSet<&str> = snap
                    .cosmic_anomalies
                    .get(&sys_id)
                    .map(|l| l.iter().map(|a| a.anomaly_id.as_str()).collect())
                    .unwrap_or_default();
                let a_rows: Vec<Vec<PyRep>> = snap
                    .cosmic_anomalies
                    .get(&sys_id)
                    .map(|l| {
                        l.iter()
                            .filter(|a| {
                                !a.cleared
                                    && (a.lifetime_seconds == 0
                                        || now_unix < a.created_unix.saturating_add(a.lifetime_seconds))
                            })
                            .map(|a| {
                                vec![
                                    PyRep::String(a.anomaly_id.clone()),
                                    PyRep::String(a.anomaly_type.clone()),
                                    PyRep::String(a.name.clone()),
                                    PyRep::Real(a.signal_strength),
                                    PyRep::Real(a.position[0]),
                                    PyRep::Real(a.position[1]),
                                    PyRep::Real(a.position[2]),
                                ]
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let ac = a_rows.len();
                let st_rows: Vec<Vec<PyRep>> = snap
                    .structures
                    .values()
                    .filter(|st| st.solar_system_id == sys_id)
                    .map(|st| {
                        vec![
                            PyRep::Integer(st.structure_id),
                            PyRep::Integer(st.type_id as i64),
                            PyRep::String(st.name.clone()),
                            PyRep::Real(1.0),
                            PyRep::Real(st.position[0]),
                            PyRep::Real(st.position[1]),
                            PyRep::Real(st.position[2]),
                        ]
                    })
                    .collect();
                let sc = sigs
                    .iter()
                    .filter(|s| !s.collapsed && !is_anomaly_colliding_signature(s, &anom_ids))
                    .count();
                (ac, a_rows, st_rows, sc)
            });
            if !is_docked && method == "SignalTrackerRegister" {
                register_signal_tracker_subscriber(ctx, sys_id, char_id);
                crate::services::notify_emitters_tournaments_anomalies::emit_signal_tracker_full_state_for_system(
                    ctx, char_id, sys_id, &sigs,
                );
            }
            Some(Ok(PyRep::keyval([
                ("solarSystemID", PyRep::Integer(sys_id as i64)),
                ("service", PyRep::String("scanMgr".into())),
                ("registered", PyRep::Bool(!is_docked)),
                ("signatureCount", PyRep::Integer(sig_count as i64)),
                ("anomalyCount", PyRep::Integer(anom_count as i64)),
                ("structureCount", PyRep::Integer(struct_rows.len() as i64)),
                (
                    "anomalies",
                    PyRep::rowset(
                        vec![
                            "anomalyID",
                            "type",
                            "name",
                            "signalStrength",
                            "x",
                            "y",
                            "z",
                        ],
                        anom_rows,
                    ),
                ),
                (
                    "structures",
                    PyRep::rowset(
                        vec![
                            "structureID",
                            "typeID",
                            "name",
                            "signalStrength",
                            "x",
                            "y",
                            "z",
                        ],
                        struct_rows,
                    ),
                ),
            ])))
        }

        ("scanMgr", "GetSignatures") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let caller_sys = session
                .solar_system_id
                .or_else(|| ctx.db.with_snapshot(|s| s.characters.get(&char_id).map(|c| c.solar_system_id)))
                .unwrap_or(0);
            let sys_id = match parse_optional_scan_sys_arg(
                ctx,
                session,
                caller_sys,
                args,
                "query signatures",
            ) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let now_unix = ctx.now_unix();
            let is_docked = session.station_id.is_some() || session.structure_id.is_some();
            let (signatures, anom_ids_owned) =
                if let Some((ready_sigs, ready_anoms)) = read_system_signatures_ready(ctx, sys_id, now_unix) {
                    (ready_sigs, ready_anoms)
                } else if is_docked {
                    ctx.db.with_snapshot(|snap| {
                        let anom_ids: Vec<String> = snap
                            .cosmic_anomalies
                            .get(&sys_id)
                            .map(|l| l.iter().map(|a| a.anomaly_id.clone()).collect())
                            .unwrap_or_default();
                        let sigs = snap
                            .cosmic_signatures
                            .get(&sys_id)
                            .map(|l| {
                                l.iter()
                                    .filter(|sig| {
                                        !sig.collapsed
                                            && (sig.lifetime_seconds == 0
                                                || now_unix
                                                    < sig
                                                        .created_unix
                                                        .saturating_add(sig.lifetime_seconds))
                                    })
                                    .cloned()
                                    .collect()
                            })
                            .unwrap_or_default();
                        (sigs, anom_ids)
                    })
                } else {
                    let _ = ensure_system_signatures(ctx, sys_id).await;
                    let any_expired = ctx.db.with_snapshot(|snap| {
                        snap.cosmic_signatures.get(&sys_id).is_some_and(|sigs| {
                            sigs.iter().any(|sig| {
                                sig.collapsed
                                    || (sig.lifetime_seconds > 0
                                        && now_unix
                                            >= sig.created_unix.saturating_add(sig.lifetime_seconds))
                            })
                        })
                    });
                    if any_expired {
                        ctx.db
                            .try_with_mut_snapshot(|snap| {
                                let anom_ids: Vec<String> = snap
                                    .cosmic_anomalies
                                    .get(&sys_id)
                                    .map(|l| l.iter().map(|a| a.anomaly_id.clone()).collect())
                                    .unwrap_or_default();
                                let sigs = snap.cosmic_signatures.entry(sys_id).or_default();
                                sigs.retain(|sig| {
                                    !sig.collapsed
                                        && (sig.lifetime_seconds == 0
                                            || now_unix
                                                < sig.created_unix.saturating_add(sig.lifetime_seconds))
                                });
                                Ok::<_, BridgeRpcError>((sigs.clone(), anom_ids))
                            })
                            .unwrap_or_default()
                    } else {
                        ctx.db.with_snapshot(|snap| {
                            let anom_ids: Vec<String> = snap
                                .cosmic_anomalies
                                .get(&sys_id)
                                .map(|l| l.iter().map(|a| a.anomaly_id.clone()).collect())
                                .unwrap_or_default();
                            let sigs = snap
                                .cosmic_signatures
                                .get(&sys_id)
                                .map(|l| {
                                    l.iter()
                                        .filter(|sig| {
                                            !sig.collapsed
                                                && (sig.lifetime_seconds == 0
                                                    || now_unix
                                                        < sig
                                                            .created_unix
                                                            .saturating_add(sig.lifetime_seconds))
                                        })
                                        .cloned()
                                        .collect()
                                })
                                .unwrap_or_default();
                            (sigs, anom_ids)
                        })
                    }
                };
            let anom_ids: std::collections::HashSet<&str> =
                anom_ids_owned.iter().map(String::as_str).collect();
            let rows: Vec<Vec<PyRep>> = signatures
                .into_iter()
                .filter(|sig| !is_anomaly_colliding_signature(sig, &anom_ids))
                .map(|sig| {
                    let group = if sig.signal_strength >= 0.25 {
                        sig.sig_group
                    } else {
                        "Unknown".into()
                    };
                    let type_name = if sig.signal_strength >= 1.0 {
                        sig.type_name
                    } else {
                        "Unidentified Signature".into()
                    };
                    vec![
                        PyRep::String(sig.signature_id),
                        PyRep::String(group),
                        PyRep::String(type_name),
                        PyRep::Real(sig.signal_strength),
                    ]
                })
                .collect();
            Some(Ok(PyRep::rowset(
                vec!["signatureID", "group", "type", "signalStrength"],
                rows,
            )))
        }

        ("scanMgr", "RequestScans") => {
            if args.len() <= 1 && (args.is_empty() || matches!(args.first(), Some(PyRep::Real(_)))) {
                return None;
            }
            let (char_id, sys_id, _) = match ctx.require_undocked_space_ship(
                session,
                "NotInSpace",
                "Cannot request probe scans while docked",
                false,
                false,
            ) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if args.len() > 3 {
                let msg = "RequestScans accepts at most 3 arguments";
                return Some(Err(BridgeRpcError::with_client_error(
                    "InvalidArguments",
                    msg,
                    eve_proto::ClientError::CustomInfo(msg.into()),
                )));
            }
            let (probe_positions_rep, scan_radius_meters, base_probe_strength) =
                if matches!(args.first(), Some(PyRep::Real(_))) {
                    let Some(r) = exact_float_arg(&args[0]).filter(|v| v.is_finite() && *v > 0.0) else {
                        let msg = "Invalid scan radius argument";
                        return Some(Err(BridgeRpcError::with_client_error(
                            "InvalidProbeCoordinates",
                            msg,
                            eve_proto::ClientError::CustomInfo(msg.into()),
                        )));
                    };
                    let s = match args.get(1) {
                        None | Some(PyRep::None) => 40.0,
                        Some(a) => match exact_float_arg(a).filter(|v| v.is_finite() && *v > 0.0) {
                            Some(v) => v,
                            None => {
                                let msg = "Invalid probe sensor strength argument";
                                return Some(Err(BridgeRpcError::with_client_error(
                                    "InvalidProbeCoordinates",
                                    msg,
                                    eve_proto::ClientError::CustomInfo(msg.into()),
                                )));
                            }
                        },
                    };
                    if let Some(pos_rep) = args.get(2).filter(|a| !matches!(a, PyRep::None)) {
                        if !matches!(pos_rep, PyRep::List(_) | PyRep::Tuple(_)) {
                            let msg = "Probe coordinates must be a list or tuple";
                            return Some(Err(BridgeRpcError::with_client_error(
                                "InvalidProbeCoordinates",
                                msg,
                                eve_proto::ClientError::CustomInfo(msg.into()),
                            )));
                        }
                    }
                    (args.get(2), r.max(0.25 * AU_IN_METERS), s)
                } else {
                    if let Some(first) = args.first().filter(|a| !matches!(a, PyRep::None)) {
                        if !matches!(first, PyRep::List(_) | PyRep::Tuple(_)) {
                            let msg = "Invalid scan request target argument";
                            return Some(Err(BridgeRpcError::with_client_error(
                                "InvalidProbeCoordinates",
                                msg,
                                eve_proto::ClientError::CustomInfo(msg.into()),
                            )));
                        }
                    }
                    let r = match args.get(1) {
                        None | Some(PyRep::None) => 4.0 * AU_IN_METERS,
                        Some(a) => match exact_float_arg(a).filter(|v| v.is_finite() && *v > 0.0) {
                            Some(v) => v,
                            None => {
                                let msg = "Invalid scan radius argument";
                                return Some(Err(BridgeRpcError::with_client_error(
                                    "InvalidProbeCoordinates",
                                    msg,
                                    eve_proto::ClientError::CustomInfo(msg.into()),
                                )));
                            }
                        },
                    }
                    .max(0.25 * AU_IN_METERS);
                    let s = match args.get(2) {
                        None | Some(PyRep::None) => 40.0,
                        Some(a) => match exact_float_arg(a).filter(|v| v.is_finite() && *v > 0.0) {
                            Some(v) => v,
                            None => {
                                let msg = "Invalid probe sensor strength argument";
                                return Some(Err(BridgeRpcError::with_client_error(
                                    "InvalidProbeCoordinates",
                                    msg,
                                    eve_proto::ClientError::CustomInfo(msg.into()),
                                )));
                            }
                        },
                    };
                    (args.first(), r, s)
                };

            let mut probes = Vec::new();
            if let Some(rep) = probe_positions_rep.filter(|a| !matches!(a, PyRep::None)) {
                let (PyRep::List(list) | PyRep::Tuple(list)) = rep else {
                    let msg = "Probe coordinates must be a list or tuple";
                    return Some(Err(BridgeRpcError::with_client_error(
                        "InvalidProbeCoordinates",
                        msg,
                        eve_proto::ClientError::CustomInfo(msg.into()),
                    )));
                };
                for item in list {
                    let Some(coords) = validate_scan_probe_entry(item) else {
                        let msg = "Probe coordinate entry must contain 3 finite numbers";
                        return Some(Err(BridgeRpcError::with_client_error(
                            "InvalidProbeCoordinates",
                            msg,
                            eve_proto::ClientError::CustomInfo(msg.into()),
                        )));
                    };
                    probes.push(DVec3::new(coords[0], coords[1], coords[2]));
                }
            }

            let now_unix = ctx.now_unix();
            if read_system_signatures_ready(ctx, sys_id, now_unix).is_none() {
                let _ = ensure_system_signatures(ctx, sys_id).await;
            }
            if probes.is_empty() {
                if let Some(launched) = ctx.social_extras.lock().unwrap().active_probes.get(&char_id) {
                    for (_, pos, _, exp) in launched {
                        if *exp > now_unix {
                            probes.push(DVec3::new(pos[0], pos[1], pos[2]));
                        }
                    }
                }
            }
            if probes.is_empty() {
                let center = DVec3::new(12.0 * AU_IN_METERS, 2.5 * AU_IN_METERS, -4.0 * AU_IN_METERS);
                let d = scan_radius_meters * 0.5;
                probes = vec![
                    center,
                    center + DVec3::new(d, 0.0, 0.0),
                    center + DVec3::new(-d, 0.0, 0.0),
                    center + DVec3::new(0.0, d, 0.0),
                    center + DVec3::new(0.0, -d, 0.0),
                    center + DVec3::new(0.0, 0.0, d),
                    center + DVec3::new(0.0, 0.0, -d),
                    center + DVec3::new(d * 0.5, d * 0.5, d * 0.5),
                ];
            }

            let (astrometrics_lvl, rangefinding_lvl, pinpointing_lvl, acquisition_lvl) = ctx.db.with_snapshot(|snap| {
                let sk = snap.skills.get(&char_id);
                (
                    sk.and_then(|m| m.get(&3412)).map(|s| s.level).unwrap_or(3),
                    sk.and_then(|m| m.get(&25810)).map(|s| s.level).unwrap_or(0),
                    sk.and_then(|m| m.get(&25811)).map(|s| s.level).unwrap_or(0),
                    sk.and_then(|m| m.get(&25812)).map(|s| s.level).unwrap_or(0),
                )
            });
            let skill_mult = 1.0
                + (astrometrics_lvl as f64 * 0.05)
                + (rangefinding_lvl as f64 * 0.05)
                + (pinpointing_lvl as f64 * 0.02);
            let scan_time_ms = (10_000.0
                * (1.0 - 0.05 * astrometrics_lvl as f64).max(0.5)
                * (1.0 - 0.10 * acquisition_lvl as f64).max(0.5))
            .round() as i64;
            let radius_au = scan_radius_meters / AU_IN_METERS;
            let now_ft = ctx.now_system_sim_filetime(Some(sys_id));
            let probe_ids_rep: Vec<PyRep> = (0..probes.len()).map(|i| PyRep::Integer(900_000 + i as i64)).collect();

            let updated_results = ctx.db.try_with_mut_snapshot(|snap| {
                let anom_ids_owned: Vec<String> = snap
                    .cosmic_anomalies
                    .get(&sys_id)
                    .map(|l| l.iter().map(|a| a.anomaly_id.clone()).collect())
                    .unwrap_or_default();
                let anom_ids: std::collections::HashSet<&str> =
                    anom_ids_owned.iter().map(String::as_str).collect();
                let Some(sigs) = snap.cosmic_signatures.get_mut(&sys_id) else {
                    return Err(BridgeRpcError {
                        code: "NoSignaturesInSystem".into(),
                        message: format!("No cosmic signatures initialized in solar system {sys_id}"),
                        client_error: None,
                    });
                };
                sigs.retain(|sig| {
                    !sig.collapsed
                        && (sig.lifetime_seconds == 0
                            || now_unix < sig.created_unix.saturating_add(sig.lifetime_seconds))
                });
                let mut out = Vec::new();
                for sig in sigs.iter_mut() {
                    if is_anomaly_colliding_signature(sig, &anom_ids) {
                        continue;
                    }
                    let sig_pos = DVec3::new(sig.position[0], sig.position[1], sig.position[2]);
                    let mut probes_in_range = 0;
                    let mut total_probe_contrib = 0.0;
                    for probe in &probes {
                        let dist = probe.distance(sig_pos);
                        if dist <= scan_radius_meters {
                            probes_in_range += 1;
                            let dist_factor = (1.0_f64 - (dist / scan_radius_meters)).max(0.0);
                            let probe_signal =
                                dist_factor * (1.0 / radius_au) * (base_probe_strength / 40.0) * skill_mult;
                            total_probe_contrib += probe_signal;
                        }
                    }
                    let coverage_mult = if probes_in_range >= 4 {
                        0.50
                    } else if probes_in_range >= 2 {
                        0.20
                    } else if probes_in_range == 1 {
                        0.05
                    } else {
                        0.0
                    };
                    let signal_gain = total_probe_contrib * coverage_mult;
                    sig.signal_strength = (sig.signal_strength + signal_gain).clamp(0.0, 1.0);
                    let resolved = sig.signal_strength >= 1.0;
                    out.push((
                        sig.signature_id.clone(),
                        sig.sig_group.clone(),
                        sig.type_name.clone(),
                        sig.signal_strength,
                        resolved,
                        sig.position,
                    ));
                }
                Ok(out)
            });

            match updated_results {
                Ok(results) => {
                    ctx.emit_typed(
                        NotificationTarget::Character(char_id),
                        crate::router::notify_registry::OnSystemScanStarted {
                            start_time: now_ft,
                            duration_ms: scan_time_ms,
                            probes: PyRep::List(probe_ids_rep),
                        },
                    );
                    crate::services::pve_exploration::with_pve_exploration_reg(ctx, |reg| {
                        let now = crate::services::pve_exploration::now_sec(reg);
                        if let Some(sites) = reg.sites_by_system.get_mut(&sys_id) {
                            for (sid, _, _, strn, _, _) in &results {
                                if let Some(site) = sites.iter_mut().find(|s| &s.site_id == sid) {
                                    site.signal_strength = *strn;
                                    if site.signal_strength >= 1.0
                                        && site.site_category == "Ghost"
                                        && site.ghost_timer_started_at.is_none()
                                    {
                                        site.ghost_timer_started_at = Some(now);
                                    }
                                }
                            }
                        }
                    });
                    let rows: Vec<Vec<PyRep>> = results
                        .into_iter()
                        .map(|(id, grp, tname, strn, resolved, pos)| {
                            vec![
                                PyRep::String(id),
                                PyRep::String(if strn >= 0.25 { grp } else { "Unknown".into() }),
                                PyRep::String(if resolved {
                                    tname
                                } else {
                                    "Unidentified Signature".into()
                                }),
                                PyRep::Real(strn),
                                PyRep::Bool(resolved),
                                if resolved { PyRep::Real(pos[0]) } else { PyRep::None },
                                if resolved { PyRep::Real(pos[1]) } else { PyRep::None },
                                if resolved { PyRep::Real(pos[2]) } else { PyRep::None },
                            ]
                        })
                        .collect();
                    ctx.emit_typed(
                        NotificationTarget::Character(char_id),
                        crate::router::notify_registry::OnSystemScanDone {},
                    );
                    Some(Ok(PyRep::rowset(
                        vec![
                            "signatureID",
                            "group",
                            "type",
                            "signalStrength",
                            "resolved",
                            "x",
                            "y",
                            "z",
                        ],
                        rows,
                    )))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("scanMgr", "ConeScan") | ("scanMgr", "DirectionalScan") => {
            let (char_id, sys_id) = match ctx.require_undocked_space_system(
                session,
                "CannotDirectionalScanWhileDocked",
                "Directional scanner cannot be used while docked",
                false,
                false,
            ) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if sys_id == 30_100_000 || (32_000_000..=32_999_999).contains(&sys_id) {
                return Some(Err(BridgeRpcError::from_user_key(eve_proto::UserErrorKey::CannotDscanInThisSystem)));
            }
            if session.ship_id.is_none() {
                let msg = "Directional scanner requires an active ship";
                return Some(Err(BridgeRpcError::with_client_error(
                    "NoActiveShip",
                    msg,
                    eve_proto::ClientError::CustomInfo(msg.into()),
                )));
            }
            if args.len() > 5 {
                let msg = "Directional scan accepts at most 5 arguments";
                return Some(Err(BridgeRpcError::with_client_error(
                    "InvalidArgs",
                    msg,
                    eve_proto::ClientError::CustomInfo(msg.into()),
                )));
            }
            let scan_angle = match args.first() {
                None | Some(PyRep::None) => 360.0,
                Some(a) => match exact_float_arg(a)
                    .filter(|v| v.is_finite() && *v >= 0.0 && *v <= 360.0)
                {
                    Some(v) => v.clamp(5.0, 360.0),
                    None => {
                        let msg = "Invalid directional scan angle";
                        return Some(Err(BridgeRpcError::with_client_error(
                            "InvalidArgs",
                            msg,
                            eve_proto::ClientError::CustomInfo(msg.into()),
                        )));
                    }
                },
            };
            let max_scan_range = 14.3 * AU_IN_METERS;
            let scan_range = match args.get(1) {
                None | Some(PyRep::None) => max_scan_range,
                Some(a) => match exact_float_arg(a).filter(|v| v.is_finite() && *v >= 0.0) {
                    Some(v) => v.clamp(0.0, max_scan_range),
                    None => {
                        let msg = "Invalid directional scan range";
                        return Some(Err(BridgeRpcError::with_client_error(
                            "InvalidArgs",
                            msg,
                            eve_proto::ClientError::CustomInfo(msg.into()),
                        )));
                    }
                },
            };
            match &args.get(2..) {
                Some([]) | None => {}
                Some([dir_rep]) => {
                    if !matches!(dir_rep, PyRep::None) && validate_coords_triple(dir_rep).is_none() {
                        let msg = "Invalid directional scan vector";
                        return Some(Err(BridgeRpcError::with_client_error(
                            "InvalidArgs",
                            msg,
                            eve_proto::ClientError::CustomInfo(msg.into()),
                        )));
                    }
                }
                Some([dx, dy, dz]) => {
                    if [dx, dy, dz]
                        .into_iter()
                        .any(|r| exact_float_arg(r).is_none_or(|v| !v.is_finite()))
                    {
                        let msg = "Invalid directional scan vector components";
                        return Some(Err(BridgeRpcError::with_client_error(
                            "InvalidArgs",
                            msg,
                            eve_proto::ClientError::CustomInfo(msg.into()),
                        )));
                    }
                }
                Some(_) => {
                    let msg = "Invalid directional scan arguments";
                    return Some(Err(BridgeRpcError::with_client_error(
                        "InvalidArgs",
                        msg,
                        eve_proto::ClientError::CustomInfo(msg.into()),
                    )));
                }
            }

            let sys_handle = ctx.systems.get_or_spawn(sys_id);
            let caller_ship = session.ship_id;
            let caller_ball = if let Some(sid) = caller_ship {
                sys_handle.get_ball_snapshot(sid).await.ok().flatten().map(|(_, b)| b)
            } else {
                None
            };
            let caller_pos = caller_ball.as_ref().map(|b| b.position).unwrap_or(DVec3::ZERO);
            let caller_dir = caller_ball
                .as_ref()
                .map(|b| {
                    if b.velocity.length_squared() > 0.001 {
                        b.velocity.normalize()
                    } else {
                        DVec3::new(0.0, 0.0, 1.0)
                    }
                })
                .unwrap_or(DVec3::new(0.0, 0.0, 1.0));

            let cos_half_angle = (scan_angle.to_radians() * 0.5).cos();
            let caller_bubble = if let Some(sid) = caller_ship {
                sys_handle.get_bubble_id_for_ball(sid).await.ok().flatten()
            } else {
                None
            };
            let balls = sys_handle.list_balls().await.unwrap_or_default();
            let mut rows = Vec::new();
            for ball in balls {
                if Some(ball.id) == caller_ship {
                    continue;
                }
                if crate::services::inventory_fitting::fitting_cyno_portal::is_covert_beacon_type(ball.type_id) {
                    let b_bub = sys_handle.get_bubble_id_for_ball(ball.id).await.ok().flatten();
                    if caller_bubble.is_none() || b_bub != caller_bubble {
                        continue;
                    }
                }
                let dist = ball.position.distance(caller_pos);
                if dist > scan_range {
                    continue;
                }
                if scan_angle < 360.0 {
                    let to_target = (ball.position - caller_pos).normalize_or_zero();
                    if to_target.dot(caller_dir) < cos_half_angle {
                        continue;
                    }
                }
                rows.push(vec![
                    PyRep::Integer(ball.id),
                    PyRep::Integer(ball.type_id as i64),
                    PyRep::Real(dist),
                    PyRep::Real(ball.position.x),
                    PyRep::Real(ball.position.y),
                    PyRep::Real(ball.position.z),
                ]);
            }
            let result_rowset = PyRep::rowset(
                vec!["ballID", "typeID", "distance", "x", "y", "z"],
                rows,
            );
            ctx.emit_typed(
                NotificationTarget::Character(char_id),
                crate::router::notify_registry::OnDirectionalScanComplete {
                    angle: scan_angle.to_radians(),
                    range: scan_range,
                    direction: PyRep::Tuple(vec![
                        PyRep::Real(caller_dir.x),
                        PyRep::Real(caller_dir.y),
                        PyRep::Real(caller_dir.z),
                    ]),
                    results: result_rowset.clone(),
                },
            );
            Some(Ok(result_rowset))
        }

        ("scanMgr", "LaunchProbes") => {
            let (char_id, _sys_id, _) = match ctx.require_undocked_space_ship(
                session,
                "NotInSpace",
                "Cannot launch scanner probes while docked",
                false,
                false,
            ) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if args.len() > 4 {
                let msg = "LaunchProbes accepts at most 4 arguments";
                return Some(Err(BridgeRpcError::with_client_error(
                    "InvalidArguments",
                    msg,
                    eve_proto::ClientError::CustomInfo(msg.into()),
                )));
            }
            let max_probes = ctx.db.with_snapshot(|snap| {
                snap.skills
                    .get(&char_id)
                    .and_then(|m| m.get(&3412))
                    .map(|s| (4 + s.level as i64).min(8))
                    .unwrap_or(8)
            });
            let requested = match args.first() {
                None | Some(PyRep::None) => max_probes,
                Some(PyRep::String(s)) if !s.trim().is_empty() => max_probes,
                Some(a) => match exact_int_arg(a).filter(|&v| v > 0) {
                    Some(v) => v,
                    None => {
                        let msg = "Invalid probe count requested";
                        return Some(Err(BridgeRpcError::with_client_error(
                            "InvalidArguments",
                            msg,
                            eve_proto::ClientError::CustomInfo(msg.into()),
                        )));
                    }
                },
            };
            if requested > max_probes {
                return Some(Err(BridgeRpcError {
                    code: "MaxProbesExceeded".into(),
                    message: format!("Astrometrics skill allows at most {max_probes} active probes"),
                    client_error: None,
                }));
            }
            let probe_count = requested.clamp(1, max_probes);
            let formation_id = match args.get(1) {
                None | Some(PyRep::None) => 0,
                Some(a) => match exact_int_arg(a) {
                    Some(v) => v,
                    None => {
                        let msg = "Invalid probe formation ID";
                        return Some(Err(BridgeRpcError::with_client_error(
                            "InvalidArguments",
                            msg,
                            eve_proto::ClientError::CustomInfo(msg.into()),
                        )));
                    }
                },
            };
            let (formation_name, scan_radius_m, spread_factor) = match formation_id {
                1 => ("Spread", 4.0 * AU_IN_METERS, 0.85),
                2 => ("Custom", 1.0 * AU_IN_METERS, 0.50),
                _ => ("Pinpoint", 0.5 * AU_IN_METERS, 0.40),
            };
            let center = match args.get(2) {
                None | Some(PyRep::None) => [0.0, 0.0, 0.0],
                Some(rep) => match validate_coords_triple(rep) {
                    Some(c) => c,
                    None => {
                        let msg = "Invalid LaunchProbes center coordinates";
                        return Some(Err(BridgeRpcError::with_client_error(
                            "InvalidProbeCoordinates",
                            msg,
                            eve_proto::ClientError::CustomInfo(msg.into()),
                        )));
                    }
                },
            };
            let now_unix = ctx.now_unix();
            let d = scan_radius_m * spread_factor;
            let base_offsets = [
                [0.0, 0.0, 0.0],
                [d, 0.0, 0.0],
                [-d, 0.0, 0.0],
                [0.0, d, 0.0],
                [0.0, -d, 0.0],
                [0.0, 0.0, d],
                [0.0, 0.0, -d],
                [d * 0.5, d * 0.5, d * 0.5],
            ];
            let mut custom_offsets: Vec<[f64; 3]> = Vec::new();
            if let Some(offsets_rep) = args.get(3).filter(|a| !matches!(a, PyRep::None)) {
                let (PyRep::List(list) | PyRep::Tuple(list)) = offsets_rep else {
                    let msg = "Invalid LaunchProbes custom offsets";
                    return Some(Err(BridgeRpcError::with_client_error(
                        "InvalidProbeCoordinates",
                        msg,
                        eve_proto::ClientError::CustomInfo(msg.into()),
                    )));
                };
                for item in list {
                    let Some(coords) = validate_coords_triple(item) else {
                        let msg = "Invalid LaunchProbes custom offset coordinates";
                        return Some(Err(BridgeRpcError::with_client_error(
                            "InvalidProbeCoordinates",
                            msg,
                            eve_proto::ClientError::CustomInfo(msg.into()),
                        )));
                    };
                    custom_offsets.push(coords);
                }
            }
            let mut pos_reps = Vec::new();
            {
                let mut st = ctx.social_extras.lock().unwrap();
                let list = st.active_probes.entry(char_id).or_default();
                list.clear();
                for i in 0..probe_count {
                    let off = custom_offsets
                        .get(i as usize)
                        .copied()
                        .or_else(|| base_offsets.get(i as usize).copied())
                        .unwrap_or([0.0, 0.0, 0.0]);
                    let pos = [center[0] + off[0], center[1] + off[1], center[2] + off[2]];
                    list.push((900_000 + i, pos, scan_radius_m, now_unix + 3600));
                    pos_reps.push(PyRep::List(vec![
                        PyRep::Real(pos[0]),
                        PyRep::Real(pos[1]),
                        PyRep::Real(pos[2]),
                    ]));
                }
            }
            let expiry_ft = ctx.now_blueos_filetime_ticks().saturating_add(3_600 * 10_000_000);
            let scan_bonuses = PyRep::Dict(vec![
                (
                    PyRep::String("strength".into()),
                    PyRep::Dict(vec![
                        (PyRep::String("modules".into()), PyRep::Real(0.0)),
                        (PyRep::String("ship".into()), PyRep::Real(0.0)),
                    ]),
                ),
                (
                    PyRep::String("deviation".into()),
                    PyRep::Dict(vec![
                        (PyRep::String("modules".into()), PyRep::Real(0.0)),
                        (PyRep::String("ship".into()), PyRep::Real(0.0)),
                    ]),
                ),
            ]);
            for (idx, pr) in pos_reps.iter().enumerate() {
                let pt = match pr {
                    PyRep::List(c) if c.len() >= 3 => PyRep::Tuple(vec![c[0].clone(), c[1].clone(), c[2].clone()]),
                    other => other.clone(),
                };
                let probe_kv = PyRep::keyval([
                    ("probeID", PyRep::Integer(900_000 + idx as i64)),
                    ("typeID", PyRep::Integer(30_013)),
                    ("scanRange", PyRep::Real(scan_radius_m)),
                    ("expiry", PyRep::Integer(expiry_ft)),
                    ("pos", pt.clone()),
                    ("destination", pt),
                    ("state", PyRep::Integer(1)),
                    ("scanBonuses", scan_bonuses.clone()),
                ]);
                ctx.emit_typed(
                    NotificationTarget::Character(char_id),
                    crate::router::notify_registry::OnNewProbe { probe: probe_kv },
                );
            }
            Some(Ok(PyRep::keyval([
                ("success", PyRep::Bool(true)),
                ("probeCount", PyRep::Integer(probe_count)),
                ("maxProbesAllowed", PyRep::Integer(max_probes)),
                ("formationID", PyRep::Integer(formation_id)),
                ("formationName", PyRep::String(formation_name.into())),
                ("scanRadiusMeters", PyRep::Real(scan_radius_m)),
                ("probePositions", PyRep::List(pos_reps)),
            ])))
        }

        // =====================================================================
        // SO-6: Wormholes (`wormholeMgr`)
        // =====================================================================
        ("wormholeMgr", "GetWormholePolarization") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            if args.len() != 1 {
                let msg = "GetWormholePolarization requires exactly 1 wormhole target argument";
                return Some(Err(BridgeRpcError::with_client_error(
                    "WormholeNotFound",
                    msg,
                    eve_proto::ClientError::CustomInfo(msg.into()),
                )));
            }
            let sys_id = session
                .solar_system_id
                .or_else(|| ctx.db.with_snapshot(|s| s.characters.get(&char_id).map(|c| c.solar_system_id)))
                .unwrap_or(0);
            let now_unix = ctx.now_unix();
            let arg_str_opt = args
                .first()
                .and_then(|a| a.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty());
            let arg_int_opt = args.first().and_then(exact_int_arg);

            // Match either a signature ID string (`WST-101` or `K162-WST-101`) or a wormhole ball ID (`>= 40_000_000`),
            // or a valid solar system ID (`30_000_000..=39_999_999` in SDE).
            // Pure snapshot lookup: reading polarization must never seed or mutate DB state.
            if arg_str_opt.is_some() || arg_int_opt.is_some_and(|id| id >= 40_000_000) {
                let wh_match = ctx.db.with_snapshot(|snap| {
                    if let Some(list) = snap.cosmic_signatures.get(&sys_id) {
                        if let Some(s) = list.iter().find(|s| {
                            s.sig_group == "Wormhole"
                                && (arg_str_opt.is_some_and(|sid| {
                                    s.signature_id == sid || s.signature_id == format!("K162-{sid}")
                                }) || arg_int_opt.is_some_and(|bid| {
                                    crate::services::space_destiny::wormhole_item_id_for_signature(
                                        sys_id,
                                        &s.signature_id,
                                    ) == bid
                                }))
                        }) {
                            let ent_id = crate::services::space_destiny::wormhole_item_id_for_signature(
                                sys_id,
                                &s.signature_id,
                            );
                            let dest = s.destination_system_id.unwrap_or(31_000_005);
                            let ex_id = crate::services::space_destiny::wormhole_exit_item_id_for_signature(
                                dest,
                                &s.signature_id,
                            );
                            return Some((s.clone(), ent_id, ex_id));
                        }
                    }
                    if let Some(bid) = arg_int_opt {
                        for (&psys, list) in &snap.cosmic_signatures {
                            if let Some(s) = list.iter().find(|s| {
                                s.sig_group == "Wormhole"
                                    && s.destination_system_id == Some(sys_id)
                                    && crate::services::space_destiny::wormhole_exit_item_id_for_signature(
                                        sys_id,
                                        &s.signature_id,
                                    ) == bid
                            }) {
                                let ent_id = crate::services::space_destiny::wormhole_item_id_for_signature(
                                    psys,
                                    &s.signature_id,
                                );
                                return Some((s.clone(), ent_id, bid));
                            }
                        }
                    }
                    None
                });
                let Some((wh, entrance_wh_id, _exit_wh_id)) = wh_match else {
                    let msg = format!("Wormhole not found in system {sys_id}");
                    return Some(Err(BridgeRpcError::with_client_error(
                        "WormholeNotFound",
                        msg.clone(),
                        eve_proto::ClientError::CustomInfo(msg),
                    )));
                };
                let dest_sys = wh.destination_system_id.unwrap_or(31_000_005) as i64;
                let pol_key = arg_int_opt.unwrap_or(entrance_wh_id);
                let exp_unix = ctx.db.with_snapshot(|snap| {
                    snap.wh_polarizations
                        .get(&(char_id, pol_key))
                        .or_else(|| snap.wh_polarizations.get(&(char_id, dest_sys)))
                        .copied()
                        .unwrap_or(0)
                });
                if exp_unix > now_unix {
                    let end_ft = crate::sp6_lifecycle::unix_to_filetime(exp_unix);
                    let duration_ticks = 300_i64 * 10_000_000;
                    return Some(Ok(PyRep::Tuple(vec![
                        PyRep::Integer(end_ft),
                        PyRep::Integer(duration_ticks),
                    ])));
                }
                return Some(Ok(PyRep::None));
            }

            let Some(target_sys) = arg_int_opt.filter(|&id| {
                u32::try_from(id).ok().is_some_and(|sys| {
                    (30_000_000..=39_999_999).contains(&sys)
                        && (ctx.sde.solar_systems.is_empty()
                            || ctx.sde.solar_systems.contains_key(&sys))
                })
            }) else {
                let msg = "Unknown or invalid wormhole target";
                return Some(Err(BridgeRpcError::with_client_error(
                    "WormholeNotFound",
                    msg,
                    eve_proto::ClientError::CustomInfo(msg.into()),
                )));
            };
            let exp_unix = ctx.db.with_snapshot(|snap| {
                snap.wh_polarizations
                    .get(&(char_id, target_sys))
                    .copied()
                    .unwrap_or(0)
            });
            if exp_unix > now_unix {
                let end_ft = crate::sp6_lifecycle::unix_to_filetime(exp_unix);
                let duration_ticks = 300_i64 * 10_000_000;
                Some(Ok(PyRep::Tuple(vec![
                    PyRep::Integer(end_ft),
                    PyRep::Integer(duration_ticks),
                ])))
            } else {
                Some(Ok(PyRep::None))
            }
        }

        // =====================================================================
        // Dungeons, Escalations & Anomalies (`keeper`, `dungeon*`)
        // =====================================================================
        ("keeper", "ActivateAccelerationGate")
        | ("beyonce", "ActivateAccelerationGate") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            if session.station_id.is_some() || session.structure_id.is_some() {
                return Some(Err(BridgeRpcError {
                    code: "NotInSpace".into(),
                    message: "Cannot activate an acceleration gate while docked".into(),
                    client_error: None,
                }));
            }
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "Session has no active solar system".into(),
                    client_error: None,
                }));
            };
            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "Session has no active ship".into(),
                    client_error: None,
                }));
            };
            if let Err(e) = crate::services::structure_control_ops::reject_if_controlling_structure(ctx, ship_id, "activate acceleration gate") {
                return Some(Err(e));
            }
            if args.is_empty() || args.len() > 2 {
                let msg = "ActivateAccelerationGate requires gate ID and optional dungeon ID";
                return Some(Err(BridgeRpcError::with_client_error(
                    "InvalidArguments",
                    msg,
                    eve_proto::ClientError::CustomInfo(msg.into()),
                )));
            }
            let Some(gate_id) = exact_int_arg(&args[0]).filter(|&id| id > 0) else {
                let msg = "Invalid acceleration gate ID";
                return Some(Err(BridgeRpcError::with_client_error(
                    "InvalidGateID",
                    msg,
                    eve_proto::ClientError::CustomInfo(msg.into()),
                )));
            };
            let override_dungeon_id = match args.get(1) {
                None | Some(PyRep::None) => None,
                Some(a) => match exact_int_arg(a).and_then(|v| u32::try_from(v).ok()) {
                    Some(v) if v > 0 => Some(v),
                    _ => {
                        let msg = "Invalid dungeon override argument";
                        return Some(Err(BridgeRpcError::with_client_error(
                            "InvalidArguments",
                            msg,
                            eve_proto::ClientError::CustomInfo(msg.into()),
                        )));
                    }
                },
            };
            if ctx.db.with_snapshot(|snap| snap.fw_runtime.fw_complexes.contains_key(&gate_id)) {
                return Some(
                    crate::services::fw_gameplay::activate_fw_complex_gate_in_space(
                        ctx, char_id, ship_id, sys_id, gate_id,
                    )
                    .await,
                );
            }
            if ctx.db.with_snapshot(|snap| {
                crate::services::insurgency_complex_ops::is_outpost_raid_gate(snap, gate_id)
            }) {
                return Some(
                    crate::services::insurgency_complex_ops::activate_outpost_raid_gate_in_space(
                        ctx, char_id, ship_id, sys_id, gate_id,
                    )
                    .await,
                );
            }
            if let Some(res) = crate::services::pve_ess_reserve_banks::try_activate_ess_acceleration_gate(
                ctx, char_id, sys_id, ship_id, gate_id,
            )
            .await
            {
                return Some(res);
            }
            let validated_gate = match ctx
                .validate_ship_movement(
                    char_id,
                    ship_id,
                    crate::services::space_nav_movement_guard::MovementIntent::AccelerationGate {
                        sys_id,
                        kind: crate::services::space_nav_movement_guard::AccelerationGateKind::Generic,
                    },
                )
                .await
            {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let sys_handle = ctx.systems.get_or_spawn(sys_id);
            let Some(ship_ball) = validated_gate.ball else {
                return Some(Err(BridgeRpcError::ship_not_in_space(
                    ship_id,
                    Some(char_id),
                    session.solar_system_id,
                    "activate_acceleration_gate",
                    "Ship is not in space",
                )));
            };
            let ship_group_id = validated_gate.ship_group_id;
            let ship_rig_size = ctx
                .sde
                .types
                .get(&validated_gate.ship_type_id)
                .and_then(|t| t.default_attributes.get(&1547).copied())
                .unwrap_or(0.0);
            if let Err(e) = crate::services::pve_special_sites::check_drifter_hive_gate_access(ctx, sys_id, gate_id) {
                return Some(Err(e));
            }

            if gate_id > 0 {
                if let Ok(Some((_, gate_ball))) = sys_handle.get_ball_snapshot(gate_id).await {
                    let dist = ship_ball.position.distance(gate_ball.position);
                    if dist > 2_500.0 {
                        return Some(Err(BridgeRpcError {
                            code: "OutsideGateActivationRange".into(),
                            message: format!("Ship is {:.1} m from gate (must be <= 2500 m)", dist),
                            client_error: None,
                        }));
                    }
                }
            }

            let gate_preview = ctx.db.with_snapshot(|snap| -> Result<(Option<u32>, Option<u32>, bool, Option<i64>), BridgeRpcError> {
                let (dungeon_id_opt, req_key_opt, consume_key) = snap
                    .items
                    .get(&gate_id)
                    .map(|g| {
                        let did = g.dynamic_attributes.get(&1001).map(|v| *v as u32);
                        let key_tid = g.dynamic_attributes.get(&1002).map(|v| *v as u32);
                        let consume = g.dynamic_attributes.get(&1003).is_some_and(|v| *v >= 0.5);
                        (did, key_tid, consume)
                    })
                    .unwrap_or((None, None, false));

                let effective_dungeon_id = dungeon_id_opt.or(override_dungeon_id);

                if let Some(did) = effective_dungeon_id {
                    if let Some(dungeon) = ctx.sde.dungeons.get(&did) {
                        match dungeon.difficulty {
                            Some(1) => {
                                // SDE dogma rigSize: 0.0 (shuttle/corvette/capsule) or 1.0 (small hull: Frigate / Destroyer).
                                // Medium hulls (rigSize 2.0+), Large (3.0+), and Capital (4.0+) are restricted.
                                let is_small_hull = ship_rig_size <= 1.0 && ship_ball.mass <= 15_000_000.0;
                                if ship_group_id > 0 && !is_small_hull {
                                    return Err(BridgeRpcError {
                                        code: "ShipRestrictedByDungeonGate".into(),
                                        message: format!(
                                            "Dungeon '{}' (ID {did}, difficulty 1) restricts gate {gate_id} to Frigate/Destroyer hulls",
                                            dungeon.name
                                        ),
                                        client_error: None,
                                    });
                                }
                            }
                            Some(2) | Some(3) => {
                                // SDE dogma rigSize: 3.0 = Large hull (Battleship / Marauder / Black Ops)
                                let is_battleship_or_larger = ship_rig_size >= 3.0 || ship_ball.mass >= 50_000_000.0;
                                if is_battleship_or_larger {
                                    return Err(BridgeRpcError {
                                        code: "ShipRestrictedByDungeonGate".into(),
                                        message: format!(
                                            "Dungeon '{}' (ID {did}) restricts gate {gate_id} against Battleship hulls",
                                            dungeon.name
                                        ),
                                        client_error: None,
                                    });
                                }
                            }
                            _ => {}
                        }
                    }
                }

                let mut consume_item_id = None;
                if let Some(req_key_type_id) = req_key_opt.filter(|&k| k > 0) {
                    let key_item_id = snap
                        .items
                        .values()
                        .find(|i| {
                            i.location_id == ship_id
                                && i.flag == eve_db::InventoryFlag::Cargo
                                && i.type_id == req_key_type_id
                                && i.quantity >= 1
                        })
                        .map(|i| i.item_id);

                    let Some(kid) = key_item_id else {
                        return Err(BridgeRpcError {
                            code: "MissingGateKey".into(),
                            message: format!(
                                "Acceleration gate {gate_id} requires key item type {req_key_type_id} in active ship cargo"
                            ),
                            client_error: None,
                        });
                    };

                    if consume_key {
                        consume_item_id = Some(kid);
                    }
                }
                Ok((effective_dungeon_id, req_key_opt, consume_item_id.is_some(), consume_item_id))
            });

            let (dungeon_id_used, key_type_used, key_consumed, consume_item_id) = match gate_preview {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };

            if let Some(kid) = consume_item_id {
                if let Err(e) = consume_gate_key_in_cargo(ctx, ship_id, kid, key_type_used) {
                    return Some(Err(e));
                }
            }

            if !validated_gate.is_covert_cloaked {
                let _ = crate::services::notify_emitters_seasons_tactical::break_gate_cloak_and_notify(
                    ctx,
                    &sys_handle,
                    char_id,
                    ship_id,
                )
                .await;
            }
            let pocket_pos = ship_ball.position + DVec3::new(500_000.0, 0.0, 0.0);
            let warp_mode = MovementMode::new_warp(ship_ball.position, pocket_pos, 3.0);
            if sys_handle
                .set_warp_movement_mode(
                    ship_id,
                    warp_mode,
                    crate::system_actor::WarpProvenance::PlayerOrFleetWarp,
                )
                .await
                .is_ok()
            {
                crate::services::space_destiny::track_player_or_fleet_warp(
                    &sys_handle,
                    ship_id,
                    char_id,
                );
            }

            Some(Ok(PyRep::keyval([
                ("success", PyRep::Bool(true)),
                ("gateID", PyRep::Integer(gate_id)),
                ("dungeonID", dungeon_id_used.map(|d| PyRep::Integer(d as i64)).unwrap_or(PyRep::None)),
                ("requiredKeyTypeID", key_type_used.map(|k| PyRep::Integer(k as i64)).unwrap_or(PyRep::None)),
                ("keyConsumed", PyRep::Bool(key_consumed)),
                ("destinationX", PyRep::Real(pocket_pos.x)),
                ("destinationY", PyRep::Real(pocket_pos.y)),
                ("destinationZ", PyRep::Real(pocket_pos.z)),
            ])))
        }

        ("keeper", "Bind") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let _ = ctx.db.with_snapshot(|snap| snap.characters.contains_key(&char_id));
            Some(Ok(PyRep::None))
        }

        _ => None,
    }
}

/// Signatures for a system come from the shared space-layer seeding/lifetime path so the scanner,
/// signal tracker and probe results always agree.
async fn ensure_system_signatures(ctx: &ServerContext, sys_id: u32) -> Vec<CosmicSignatureRecord> {
    crate::services::space_destiny::ensure_system_signatures_and_anomalies(ctx, sys_id).await.0
}

pub(crate) fn register_signal_tracker_subscriber(ctx: &ServerContext, sys_id: u32, char_id: i64) {
    let already_registered = ctx.db.with_snapshot(|snap| {
        let in_target = snap
            .fleet_and_social_ops
            .signal_tracker_subs
            .get(&sys_id)
            .is_some_and(|subs| subs.contains(&char_id));
        let in_other = snap
            .fleet_and_social_ops
            .signal_tracker_subs
            .iter()
            .any(|(&s, subs)| s != sys_id && subs.contains(&char_id));
        in_target && !in_other
    });
    if already_registered {
        return;
    }
    ctx.db.with_mut_snapshot(|snap| {
        for (&other_sys, subs) in snap.fleet_and_social_ops.signal_tracker_subs.iter_mut() {
            if other_sys != sys_id {
                subs.retain(|&c| c != char_id);
            }
        }
        snap.fleet_and_social_ops
            .signal_tracker_subs
            .retain(|_, subs| !subs.is_empty());
        let subs = snap
            .fleet_and_social_ops
            .signal_tracker_subs
            .entry(sys_id)
            .or_default();
        if !subs.contains(&char_id) {
            subs.push(char_id);
            subs.sort_unstable();
        }
    });
}

pub(crate) fn signal_tracker_subscribers_in_snap(
    snap: &eve_db::WorldStateSnapshot,
    sys_id: u32,
) -> Vec<i64> {
    let Some(mut subs) = snap
        .fleet_and_social_ops
        .signal_tracker_subs
        .get(&sys_id)
        .cloned()
    else {
        return Vec::new();
    };
    subs.retain(|cid| {
        snap.characters.get(cid).is_some_and(|c| {
            c.solar_system_id == sys_id
                && snap
                    .sessions
                    .get(&c.user_id)
                    .is_some_and(|os| os.character_id == Some(*cid))
        })
    });
    subs.sort_unstable();
    subs.dedup();
    subs
}

pub(crate) fn consume_gate_key_in_cargo(
    ctx: &ServerContext,
    ship_id: i64,
    kid: i64,
    key_type_used: Option<u32>,
) -> Result<(), BridgeRpcError> {
    ctx.db.try_with_mut_snapshot(|snap| {
        let missing_key_err = || {
            let msg = "Gate key item is no longer present in cargo";
            BridgeRpcError::with_client_error(
                "MissingGateKey",
                msg,
                eve_proto::ClientError::CustomInfo(msg.into()),
            )
        };
        let Some(item) = snap.items.get_mut(&kid) else {
            return Err(missing_key_err());
        };
        if item.location_id != ship_id
            || item.flag != eve_db::InventoryFlag::Cargo
            || key_type_used != Some(item.type_id)
            || item.quantity < 1
        {
            return Err(missing_key_err());
        }
        item.quantity -= 1;
        if item.quantity <= 0 {
            snap.items.remove(&kid);
        }
        Ok(())
    })
}

fn random_sphere_offset(dev_radius: f64, sig_id: &str) -> glam::DVec3 {
    let mut b = [0u8; 24];
    if getrandom::getrandom(&mut b).is_err() {
        let t = sig_id.bytes().fold(0x517c_c1b7_2722_0a95_u64, |a, c| (a ^ (c as u64)).wrapping_mul(1099511628211));
        b[..8].copy_from_slice(&t.to_le_bytes());
        b[8..16].copy_from_slice(&(t.wrapping_mul(0x9E37_79B9_7F4A_7C15)).to_le_bytes());
        b[16..24].copy_from_slice(&(t.wrapping_mul(0xBF58_476D_1CE4_E5B9)).to_le_bytes());
    }
    let u1 = ((u64::from_le_bytes(b[0..8].try_into().unwrap()) >> 11) as f64) * (1.0 / ((1u64 << 53) as f64));
    let u2 = ((u64::from_le_bytes(b[8..16].try_into().unwrap()) >> 11) as f64) * (1.0 / ((1u64 << 53) as f64));
    let u3 = ((u64::from_le_bytes(b[16..24].try_into().unwrap()) >> 11) as f64) * (1.0 / ((1u64 << 53) as f64));
    let theta = u1 * 2.0 * std::f64::consts::PI;
    let z = u2 * 2.0 - 1.0;
    let xy_r = (1.0 - z * z).max(0.0).sqrt();
    let dir = glam::DVec3::new(xy_r * theta.cos(), xy_r * theta.sin(), z).try_normalize().unwrap_or(glam::DVec3::X);
    let r_factor = u3.cbrt().clamp(0.1, 1.0);
    dir * (dev_radius * r_factor)
}

pub fn build_signal_tracker_signature_kv(
    sde: &eve_sde::SdeRegistry,
    sig: &CosmicSignatureRecord,
) -> PyRep {
    let (diff_level, _) = crate::services::space_destiny::signature_difficulty_level_and_min_strength(
        sde,
        sig.solar_system_id,
        sig,
    );
    let dev_radius = if sig.signal_strength >= 1.0 {
        0.0
    } else {
        (1.0 - sig.signal_strength.clamp(0.0, 1.0)) * (diff_level as f64) * AU_IN_METERS
    };
    let reported_pos = if dev_radius > 0.0 {
        let off = random_sphere_offset(dev_radius, &sig.signature_id);
        [sig.position[0] + off.x, sig.position[1] + off.y, sig.position[2] + off.z]
    } else {
        sig.position
    };
    let pos_rep = PyRep::Tuple(vec![PyRep::Real(reported_pos[0]), PyRep::Real(reported_pos[1]), PyRep::Real(reported_pos[2])]);
    PyRep::keyval([
        ("position", pos_rep),
        ("targetID", PyRep::String(sig.signature_id.clone())),
        ("difficulty", PyRep::Integer(i64::from(diff_level))),
        ("dungeonID", PyRep::None),
        ("archetypeID", PyRep::None),
        ("deviation", PyRep::Real(dev_radius)),
    ])
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AnomalyDungeonMetadata {
    pub dungeon_id: u32,
    pub dungeon_name_id: Option<u32>,
    pub archetype_id: u32,
    pub faction_id: u32,
    pub difficulty: u32,
    pub scan_strength_attribute: i64,
}

fn scan_strength_attribute_for_anomaly(anom_type: &str, archetype_id: u32) -> i64 {
    match archetype_id {
        45 => 208, // scanRadarStrength (Data)
        30 => 209, // scanLadarStrength (Gas)
        44 => 210, // scanMagnetometricStrength (Relic)
        27 | 28 => 211, // scanGravimetricStrength (Ore / Ice)
        _ => match anom_type.trim().to_ascii_lowercase().as_str() {
            "data" | "data site" => 208,
            "gas" | "gas site" => 209,
            "relic" | "relic site" => 210,
            _ => 211,
        },
    }
}

pub(crate) fn resolve_anomaly_dungeon_metadata(
    sde: &eve_sde::SdeRegistry,
    anom: &eve_db::CosmicAnomalyRecord,
) -> AnomalyDungeonMetadata {
    let parsed_dungeon_id = anom
        .anomaly_id
        .strip_prefix("DNG-")
        .and_then(|rest| rest.split('-').next())
        .and_then(|num| num.parse::<u32>().ok())
        .filter(|&id| id > 0);

    let anom_name = anom.name.trim();
    let by_id = parsed_dungeon_id.and_then(|id| sde.dungeons.get(&id));
    let by_name = if by_id.is_none() && !anom_name.is_empty() {
        sde.dungeons
            .values()
            .filter(|d| d.name.trim().eq_ignore_ascii_case(anom_name))
            .min_by_key(|d| (u8::from(d.archetype_id.is_none()), d.dungeon_id))
    } else {
        None
    };

    let lower_type = anom.anomaly_type.trim().to_ascii_lowercase();
    let (default_dungeon_id, default_archetype_id, default_faction_id, default_difficulty) =
        if matches!(lower_type.as_str(), "ore" | "oreanomaly" | "sovore") {
            (10_778_u32, 27_u32, 500_021_u32, 1_u32)
        } else if matches!(lower_type.as_str(), "icebelt" | "ice") {
            (5_496_u32, 28_u32, 500_021_u32, 2_u32)
        } else if matches!(lower_type.as_str(), "gas" | "gas site") {
            (1_427_u32, 30_u32, 500_021_u32, 2_u32)
        } else if matches!(lower_type.as_str(), "relic" | "relic site") {
            (1_612_u32, 44_u32, 500_011_u32, 2_u32)
        } else if matches!(lower_type.as_str(), "data" | "data site") {
            (1_617_u32, 45_u32, 500_010_u32, 2_u32)
        } else {
            (2_134_u32, 24_u32, 500_010_u32, 1_u32)
        };

    let matched = by_id.or(by_name).or_else(|| {
        sde.dungeons.get(&default_dungeon_id).or_else(|| {
            sde.dungeons
                .values()
                .filter(|d| d.archetype_id == Some(default_archetype_id))
                .min_by_key(|d| d.dungeon_id)
        })
    });

    let (dungeon_id, archetype_id, faction_id, difficulty) = if let Some(d) = matched {
        (
            d.dungeon_id,
            d.archetype_id.unwrap_or(default_archetype_id),
            d.faction_id.unwrap_or(default_faction_id),
            d.difficulty.unwrap_or(default_difficulty).clamp(1, 5),
        )
    } else {
        (
            parsed_dungeon_id.unwrap_or(default_dungeon_id),
            default_archetype_id,
            default_faction_id,
            default_difficulty,
        )
    };
    AnomalyDungeonMetadata {
        dungeon_id,
        dungeon_name_id: sde.dungeon_name_id(dungeon_id),
        archetype_id,
        faction_id,
        difficulty,
        scan_strength_attribute: scan_strength_attribute_for_anomaly(
            &anom.anomaly_type,
            archetype_id,
        ),
    }
}

pub(crate) fn build_signal_tracker_anomaly_kv(
    sde: &eve_sde::SdeRegistry,
    anom: &eve_db::CosmicAnomalyRecord,
) -> PyRep {
    let pos_rep = PyRep::Tuple(vec![PyRep::Real(anom.position[0]), PyRep::Real(anom.position[1]), PyRep::Real(anom.position[2])]);
    let meta = resolve_anomaly_dungeon_metadata(sde, anom);
    PyRep::keyval([
        ("instanceID", PyRep::String(anom.anomaly_id.clone())),
        ("position", pos_rep),
        ("targetID", PyRep::String(anom.anomaly_id.clone())),
        ("difficulty", PyRep::Integer(i64::from(meta.difficulty))),
        ("dungeonID", PyRep::Integer(i64::from(meta.dungeon_id))),
        ("archetypeID", PyRep::Integer(i64::from(meta.archetype_id))),
        ("dungeonName", PyRep::String(anom.name.clone())),
        (
            "dungeonNameID",
            meta.dungeon_name_id
                .map_or(PyRep::None, |id| PyRep::Integer(i64::from(id))),
        ),
        ("factionID", PyRep::Integer(i64::from(meta.faction_id))),
        ("scanStrengthAttribute", PyRep::Integer(meta.scan_strength_attribute)),
        ("allowedTypes", PyRep::None),
    ])
}

pub(crate) fn build_signal_tracker_structure_kv(
    ctx: &ServerContext,
    st: &eve_db::UpwellStructureRecord,
) -> PyRep {
    let (group_id, category_id) = ctx
        .sde
        .types
        .get(&st.type_id)
        .map(|t| (i64::from(t.group_id), i64::from(t.category_id)))
        .unwrap_or((1657, 65));
    let pos_rep = PyRep::Tuple(vec![PyRep::Real(st.position[0]), PyRep::Real(st.position[1]), PyRep::Real(st.position[2])]);
    PyRep::keyval([
        ("typeID", PyRep::Integer(i64::from(st.type_id))),
        ("groupID", PyRep::Integer(group_id)),
        ("categoryID", PyRep::Integer(category_id)),
        ("position", pos_rep),
        ("targetID", PyRep::Integer(st.structure_id)),
    ])
}

/// Returns `true` when `sig` represents or collides with a cosmic anomaly and
/// must be excluded from client-facing signature maps/notifications so V24
/// `SignatureSiteData` never shadows `AnomalyBracket` on `OnSystemScanDone` (#1034).
pub(crate) fn is_anomaly_colliding_signature(
    sig: &CosmicSignatureRecord,
    anom_ids: &std::collections::HashSet<&str>,
) -> bool {
    matches!(sig.sig_group.as_str(), "SovOre" | "OreAnomaly" | "IceBelt")
        || anom_ids.contains(sig.signature_id.as_str())
}

pub(crate) fn emit_signal_tracker_signature_delta(
    ctx: &ServerContext,
    sys_id: u32,
    added: &[CosmicSignatureRecord],
    removed_ids: &[String],
) {
    if added.is_empty() && removed_ids.is_empty() {
        return;
    }
    let (subs, anom_ids) = ctx.db.with_snapshot(|snap| {
        let subs = signal_tracker_subscribers_in_snap(snap, sys_id);
        let anom_ids: std::collections::HashSet<String> = snap
            .cosmic_anomalies
            .get(&sys_id)
            .map(|list| list.iter().map(|a| a.anomaly_id.clone()).collect())
            .unwrap_or_default();
        (subs, anom_ids)
    });
    if subs.is_empty() {
        return;
    }
    let anom_id_refs: std::collections::HashSet<&str> =
        anom_ids.iter().map(String::as_str).collect();
    let added_dict = PyRep::Dict(
        added
            .iter()
            .filter(|sig| !sig.collapsed && !is_anomaly_colliding_signature(sig, &anom_id_refs))
            .map(|sig| {
                (
                    PyRep::String(sig.signature_id.clone()),
                    build_signal_tracker_signature_kv(&ctx.sde, sig),
                )
            })
            .collect(),
    );
    if matches!(&added_dict, PyRep::Dict(entries) if entries.is_empty()) && removed_ids.is_empty() {
        return;
    }
    let removed_list = PyRep::List(removed_ids.iter().cloned().map(PyRep::String).collect());
    for cid in subs {
        ctx.emit_typed(
            NotificationTarget::Character(cid),
            crate::router::notify_registry::OnSignalTrackerSignatureUpdate {
                solar_system_id: i64::from(sys_id),
                added_signatures: added_dict.clone(),
                removed_signatures: removed_list.clone(),
            },
        );
    }
}

pub(crate) fn emit_signal_tracker_anomaly_delta(
    ctx: &ServerContext,
    sys_id: u32,
    added: &[eve_db::CosmicAnomalyRecord],
    removed_ids: &[String],
) {
    if added.is_empty() && removed_ids.is_empty() {
        return;
    }
    let subs = ctx.db.with_snapshot(|snap| signal_tracker_subscribers_in_snap(snap, sys_id));
    if subs.is_empty() {
        return;
    }
    let added_dict = PyRep::Dict(
        added
            .iter()
            .map(|anom| {
                (
                    PyRep::String(anom.anomaly_id.clone()),
                    build_signal_tracker_anomaly_kv(&ctx.sde, anom),
                )
            })
            .collect(),
    );
    let removed_list = PyRep::List(removed_ids.iter().cloned().map(PyRep::String).collect());
    for cid in subs {
        ctx.emit_typed(
            NotificationTarget::Character(cid),
            crate::router::notify_registry::OnSignalTrackerAnomalyUpdate {
                solar_system_id: i64::from(sys_id),
                added_anomalies: added_dict.clone(),
                removed_anomalies: removed_list.clone(),
            },
        );
    }
}

pub(crate) fn emit_signal_tracker_structure_delta(
    ctx: &ServerContext,
    sys_id: u32,
    added: &[eve_db::UpwellStructureRecord],
    removed_ids: &[i64],
) {
    if added.is_empty() && removed_ids.is_empty() {
        return;
    }
    let subs = ctx.db.with_snapshot(|snap| signal_tracker_subscribers_in_snap(snap, sys_id));
    if subs.is_empty() {
        return;
    }
    let added_dict = PyRep::Dict(
        added
            .iter()
            .map(|st| {
                (
                    PyRep::Integer(st.structure_id),
                    build_signal_tracker_structure_kv(ctx, st),
                )
            })
            .collect(),
    );
    let removed_list = PyRep::List(removed_ids.iter().copied().map(PyRep::Integer).collect());
    for cid in subs {
        ctx.emit_typed(
            NotificationTarget::Character(cid),
            crate::router::notify_registry::OnSignalTrackerStructureUpdate {
                solar_system_id: i64::from(sys_id),
                added_structures: added_dict.clone(),
                removed_structures: removed_list.clone(),
            },
        );
    }
}

pub(crate) fn emit_signal_tracker_structure_removed_with_snap(
    _ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    sys_id: u32,
    structure_id: i64,
) -> Vec<(i64, Vec<PyRep>)> {
    let subs = signal_tracker_subscribers_in_snap(snap, sys_id);
    if subs.is_empty() {
        return Vec::new();
    }
    let args = vec![
        PyRep::Integer(i64::from(sys_id)),
        PyRep::Dict(Vec::new()),
        PyRep::List(vec![PyRep::Integer(structure_id)]),
    ];
    subs.into_iter().map(|cid| (cid, args.clone())).collect()
}

pub(crate) fn dispatch_signal_tracker_notifications(
    ctx: &ServerContext,
    notifs: impl IntoIterator<Item = (i64, Vec<PyRep>)>,
) {
    for (cid, args) in notifs {
        let mut it = args.into_iter();
        let solar_system_id = it.next().and_then(|r| r.as_i64()).unwrap_or(0);
        let added_structures = it.next().unwrap_or_else(|| PyRep::Dict(Vec::new()));
        let removed_structures = it.next().unwrap_or_else(|| PyRep::List(Vec::new()));
        ctx.emit_typed(
            NotificationTarget::Character(cid),
            crate::router::notify_registry::OnSignalTrackerStructureUpdate {
                solar_system_id,
                added_structures,
                removed_structures,
            },
        );
    }
}

pub(crate) fn emit_signal_tracker_site_removed_with_db(
    db: &eve_db::WorldRepository,
    pending_notifications: &dyn AnyRwLock<
        HashMap<i64, Vec<crate::router::cache_notifications::PendingMachoNotification>>,
    >,
    notify_tx: &tokio::sync::broadcast::Sender<(
        i64,
        crate::router::cache_notifications::PendingMachoNotification,
    )>,
    site_id: &str,
) {
    let (sys_id_opt, was_anom, was_sig, recips) = db.with_snapshot(|snap| {
        let mut found_sys = None;
        let mut is_a = false;
        let mut is_s = false;
        for (&sid, anoms) in &snap.cosmic_anomalies {
            if anoms.iter().any(|a| a.anomaly_id == site_id && a.cleared) {
                found_sys = Some(sid);
                is_a = true;
                is_s = snap
                    .cosmic_signatures
                    .get(&sid)
                    .is_some_and(|l| l.iter().any(|s| s.signature_id == site_id && s.collapsed));
                break;
            }
        }
        if found_sys.is_none() {
            for (&sid, sigs) in &snap.cosmic_signatures {
                if sigs.iter().any(|s| s.signature_id == site_id && s.collapsed) {
                    found_sys = Some(sid);
                    is_s = true;
                    break;
                }
            }
        }
        let rec: Vec<(i64, Option<u64>)> = found_sys
            .map(|sid| {
                signal_tracker_subscribers_in_snap(snap, sid)
                    .into_iter()
                    .map(|cid| (cid, snap.characters.get(&cid).map(|c| c.user_id)))
                    .collect()
            })
            .unwrap_or_default();
        (found_sys, is_a, is_s, rec)
    });
    let Some(sys_id) = sys_id_opt else { return };
    if recips.is_empty() {
        return;
    }
    let now_ft = db.clock().now_blueos_filetime_ticks();
    let removed = PyRep::List(vec![PyRep::String(site_id.to_string())]);
    if was_anom {
        let notif = crate::router::notify_registry::OnSignalTrackerAnomalyUpdate {
            solar_system_id: i64::from(sys_id),
            added_anomalies: PyRep::Dict(Vec::new()),
            removed_anomalies: removed.clone(),
        };
        for &(cid, uid) in &recips {
            crate::router::notify_registry::emit_typed_target_to_char_with_id_type(
                db.next_notification_seq_id(),
                Some(pending_notifications),
                Some(notify_tx),
                cid,
                uid,
                "charid",
                now_ft,
                notif.clone(),
            );
        }
    }
    if was_sig {
        let notif = crate::router::notify_registry::OnSignalTrackerSignatureUpdate {
            solar_system_id: i64::from(sys_id),
            added_signatures: PyRep::Dict(Vec::new()),
            removed_signatures: removed,
        };
        for &(cid, uid) in &recips {
            crate::router::notify_registry::emit_typed_target_to_char_with_id_type(
                db.next_notification_seq_id(),
                Some(pending_notifications),
                Some(notify_tx),
                cid,
                uid,
                "charid",
                now_ft,
                notif.clone(),
            );
        }
    }
}

use crate::router::rpc_registry::{
    rpc_entry, RpcBoxFuture, RpcDispatchHandler, RpcEntry, RpcSceneClass,
};

fn dispatch_rpc<'a>(
    ctx: &'a ServerContext,
    session: &'a mut SessionState,
    service: &'a str,
    method: &'a str,
    args: &'a [PyRep],
    kwargs: &'a HashMap<String, PyRep>,
) -> RpcBoxFuture<'a> {
    Box::pin(async move {
        if let Some(res) =
            crate::services::pve_abyssal_hazards::check_abyssal_in_pocket_rpc_restrictions(
                session, service, method, args,
            )
        {
            return Some(res);
        }
        handle_scanning_and_wormholes(ctx, session, service, method, args, kwargs).await
    })
}

fn dispatch_wormhole_polarization<'a>(
    ctx: &'a ServerContext,
    session: &'a mut SessionState,
    service: &'a str,
    method: &'a str,
    args: &'a [PyRep],
    kwargs: &'a HashMap<String, PyRep>,
) -> RpcBoxFuture<'a> {
    Box::pin(async move {
        if let Some(res) =
            crate::services::wormhole_lifecycle_ops::resolve_cross_system_wormhole_polarization(
                ctx, session, args,
            )
        {
            return Some(res);
        }
        handle_scanning_and_wormholes(ctx, session, service, method, args, kwargs).await
    })
}

fn dispatch_keeper_bind<'a>(
    ctx: &'a ServerContext,
    session: &'a mut SessionState,
    service: &'a str,
    method: &'a str,
    args: &'a [PyRep],
    kwargs: &'a HashMap<String, PyRep>,
) -> RpcBoxFuture<'a> {
    Box::pin(async move {
        if let Some(res) =
            crate::services::pve_dungeon_cache::handle(ctx, session, service, method, args, kwargs)
                .await
        {
            return Some(res);
        }
        handle_scanning_and_wormholes(ctx, session, service, method, args, kwargs).await
    })
}

pub(crate) const RPCS: &[RpcEntry] = &[
    rpc_entry("wormholeMgr", "GetWormholePolarization", RpcDispatchHandler::Async(dispatch_wormhole_polarization)),
    rpc_entry("keeper", "Bind", RpcDispatchHandler::Async(dispatch_keeper_bind)),
    rpc_entry("scanMgr", "SignalTrackerRegister", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("scanMgr", "GetSignatures", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("scanMgr", "DirectionalScan", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("scanMgr", "LaunchProbes", RpcDispatchHandler::Async(dispatch_rpc)).with_scene(RpcSceneClass::InSpace),
];

#[cfg(test)] #[path = "fleet_social_scanning_tests.rs"]
mod fleet_social_scanning_tests;
#[cfg(test)]
#[path = "fleet_social_scanning_edges_tests.rs"]
mod fleet_social_scanning_edges_tests;

