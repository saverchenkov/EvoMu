//! Scanning & Dungeons (`SO-5`) and Wormholes (`SO-6`).

use crate::router::{cache_notifications::NotificationTarget, ServerContext};
use eve_db::CosmicSignatureRecord;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_destiny::MovementMode;
use glam::DVec3;
use std::collections::HashMap;

const AU_IN_METERS: f64 = 149_597_870_700.0;

pub(super) async fn handle_scanning_and_wormholes(
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
        ("scanMgr", "GetSystemScanMgr") | ("scanMgr", "SignalTrackerRegister") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let sys_id = session.solar_system_id.unwrap_or(30_000_142);
            let now_unix = ctx.now_unix();
            let sigs = ensure_system_signatures(ctx, sys_id).await;
            let (anom_count, anom_rows, struct_rows) = ctx.db.with_snapshot(|snap| {
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
                (ac, a_rows, st_rows)
            });
            ctx.notify(
                NotificationTarget::Character(char_id),
                "OnSignalTrackerSignatureUpdate",
                "charid",
                vec![PyRep::Integer(sys_id as i64), PyRep::Integer(sigs.len() as i64)],
            );
            ctx.notify(
                NotificationTarget::Character(char_id),
                "OnSignalTrackerAnomalyUpdate",
                "charid",
                vec![PyRep::Integer(sys_id as i64), PyRep::Integer(anom_count as i64)],
            );
            ctx.notify(
                NotificationTarget::Character(char_id),
                "OnSignalTrackerStructureUpdate",
                "charid",
                vec![
                    PyRep::Integer(sys_id as i64),
                    PyRep::Integer(struct_rows.len() as i64),
                ],
            );
            Some(Ok(PyRep::keyval([
                ("solarSystemID", PyRep::Integer(sys_id as i64)),
                ("service", PyRep::String("scanMgr".into())),
                ("registered", PyRep::Bool(true)),
                ("signatureCount", PyRep::Integer(sigs.len() as i64)),
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
            let _ = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let sys_id = args
                .first()
                .and_then(|a| a.as_i64())
                .map(|v| v as u32)
                .or(session.solar_system_id)
                .unwrap_or(30_000_142);
            let now_unix = ctx.now_unix();
            let _ = ensure_system_signatures(ctx, sys_id).await;
            let signatures = ctx.db.try_with_mut_snapshot(|snap| {
                let sigs = snap.cosmic_signatures.entry(sys_id).or_default();
                sigs.retain(|sig| {
                    !sig.collapsed
                        && (sig.lifetime_seconds == 0
                            || now_unix < sig.created_unix.saturating_add(sig.lifetime_seconds))
                });
                Ok::<_, BridgeRpcError>(sigs.clone())
            }).unwrap_or_default();
            let rows: Vec<Vec<PyRep>> = signatures
                .into_iter()
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
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "Session has no active solar system".into(),
                }));
            };
            let _ = ensure_system_signatures(ctx, sys_id).await;
            let now_unix = ctx.now_unix();
            let (probe_positions_rep, scan_radius_meters, base_probe_strength) =
                if let Some(PyRep::Real(r)) = args.first() {
                    let s = args.get(1).and_then(|a| a.as_f64()).unwrap_or(40.0);
                    (args.get(2), r.max(0.25 * AU_IN_METERS), s)
                } else {
                    let r = args
                        .get(1)
                        .and_then(|a| a.as_f64())
                        .unwrap_or(4.0 * AU_IN_METERS)
                        .max(0.25 * AU_IN_METERS);
                    let s = args.get(2).and_then(|a| a.as_f64()).unwrap_or(40.0);
                    (args.first(), r, s)
                };

            let mut probes = Vec::new();
            if let Some(PyRep::List(list)) = probe_positions_rep {
                for item in list {
                    if let PyRep::List(coords) | PyRep::Tuple(coords) = item {
                        if coords.len() >= 3 {
                            let x = coords[0].as_f64().unwrap_or(0.0);
                            let y = coords[1].as_f64().unwrap_or(0.0);
                            let z = coords[2].as_f64().unwrap_or(0.0);
                            probes.push(DVec3::new(x, y, z));
                        }
                    }
                }
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
            let _scan_time_ms = (4000.0
                * (1.0 - 0.05 * astrometrics_lvl as f64).max(0.5)
                * (1.0 - 0.05 * acquisition_lvl as f64).max(0.5))
            .round() as i64;
            let radius_au = scan_radius_meters / AU_IN_METERS;

            ctx.notify(
                NotificationTarget::Character(char_id),
                "OnSystemScanStarted",
                "charid",
                vec![PyRep::Integer(char_id), PyRep::Integer(probes.len() as i64)],
            );

            let updated_results = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(sigs) = snap.cosmic_signatures.get_mut(&sys_id) else {
                    return Err(BridgeRpcError {
                        code: "NoSignaturesInSystem".into(),
                        message: format!("No cosmic signatures initialized in solar system {sys_id}"),
                    });
                };
                sigs.retain(|sig| {
                    !sig.collapsed
                        && (sig.lifetime_seconds == 0
                            || now_unix < sig.created_unix.saturating_add(sig.lifetime_seconds))
                });
                let mut out = Vec::new();
                for sig in sigs.iter_mut() {
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
                    ctx.notify(
                        NotificationTarget::Character(char_id),
                        "OnSystemScanDone",
                        "charid",
                        vec![PyRep::Integer(char_id)],
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
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "Session has no active solar system".into(),
                }));
            };
            let scan_angle = args
                .first()
                .and_then(|a| a.as_f64())
                .unwrap_or(360.0)
                .clamp(5.0, 360.0);
            let max_scan_range = 14.3 * AU_IN_METERS;
            let scan_range = args
                .get(1)
                .and_then(|a| a.as_f64())
                .unwrap_or(max_scan_range)
                .clamp(0.0, max_scan_range);

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
            let balls = sys_handle.list_balls().await.unwrap_or_default();
            let mut rows = Vec::new();
            for ball in balls {
                if Some(ball.id) == caller_ship {
                    continue;
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
            ctx.notify(
                NotificationTarget::Character(char_id),
                "OnDirectionalScanComplete",
                "charid",
                vec![PyRep::Integer(rows.len() as i64)],
            );
            Some(Ok(PyRep::rowset(
                vec!["ballID", "typeID", "distance", "x", "y", "z"],
                rows,
            )))
        }

        ("scanMgr", "LaunchProbes") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let max_probes = ctx.db.with_snapshot(|snap| {
                snap.skills
                    .get(&char_id)
                    .and_then(|m| m.get(&3412))
                    .map(|s| (4 + s.level as i64).min(8))
                    .unwrap_or(8)
            });
            let requested = args.first().and_then(|a| a.as_i64()).unwrap_or(max_probes);
            if requested > max_probes {
                return Some(Err(BridgeRpcError {
                    code: "MaxProbesExceeded".into(),
                    message: format!("Astrometrics skill allows at most {max_probes} active probes"),
                }));
            }
            let probe_count = requested.clamp(1, max_probes);
            let formation_id = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0);
            let (formation_name, scan_radius_m, spread_factor) = match formation_id {
                1 => ("Spread", 4.0 * AU_IN_METERS, 0.85),
                2 => ("Custom", 1.0 * AU_IN_METERS, 0.50),
                _ => ("Pinpoint", 0.5 * AU_IN_METERS, 0.40),
            };
            let center = if let Some(PyRep::List(c) | PyRep::Tuple(c)) = args.get(2) {
                if c.len() >= 3 {
                    [
                        c[0].as_f64().unwrap_or(0.0),
                        c[1].as_f64().unwrap_or(0.0),
                        c[2].as_f64().unwrap_or(0.0),
                    ]
                } else {
                    [0.0, 0.0, 0.0]
                }
            } else {
                [0.0, 0.0, 0.0]
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
            if let Some(PyRep::List(list) | PyRep::Tuple(list)) = args.get(3) {
                for item in list {
                    if let PyRep::List(c) | PyRep::Tuple(c) = item {
                        if c.len() >= 3 {
                            custom_offsets.push([
                                c[0].as_f64().unwrap_or(0.0),
                                c[1].as_f64().unwrap_or(0.0),
                                c[2].as_f64().unwrap_or(0.0),
                            ]);
                        }
                    }
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
            ctx.notify(
                NotificationTarget::Character(char_id),
                "OnNewProbe",
                "charid",
                vec![PyRep::Integer(probe_count)],
            );
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
            let sys_id = session.solar_system_id.unwrap_or(30_000_142);
            let now_unix = ctx.now_unix();

            // If arg 0 is a signature ID string, return full wormhole status + polarization + system rules
            if let Some(sig_id) = args.first().and_then(|a| a.as_str()) {
                let _ = ensure_system_signatures(ctx, sys_id).await;
                let wh_opt = ctx.db.with_snapshot(|snap| {
                    snap.cosmic_signatures.get(&sys_id).and_then(|list| {
                        list.iter()
                            .find(|s| s.signature_id == sig_id && s.sig_group == "Wormhole")
                            .cloned()
                    })
                });
                let Some(wh) = wh_opt else {
                    return Some(Err(BridgeRpcError {
                        code: "WormholeNotFound".into(),
                        message: format!("Wormhole signature {sig_id} not found in system {sys_id}"),
                    }));
                };
                let buckets = crate::services::space_destiny::space_cyno_filaments::derive_wormhole_buckets(&wh, now_unix, &ctx.sde);
                let elapsed = now_unix.saturating_sub(wh.created_unix);
                let lifetime_pct = if elapsed >= wh.lifetime_seconds {
                    0.0
                } else {
                    (wh.lifetime_seconds - elapsed) as f64 / wh.lifetime_seconds as f64
                };
                let lifetime_stage = if lifetime_pct <= 0.0 {
                    "Collapsed_Expired"
                } else if lifetime_pct < 0.25 {
                    "ReachingEndOfLifetime"
                } else {
                    "Healthy"
                };
                let mass_pct = if wh.max_mass_kg > 0.0 {
                    wh.remaining_mass_kg / wh.max_mass_kg
                } else {
                    0.0
                };
                let mass_stage = if wh.remaining_mass_kg <= 0.0 {
                    "Collapsed_Exhausted"
                } else if mass_pct < 0.10 {
                    "Critical"
                } else if mass_pct < 0.50 {
                    "Destabilized"
                } else {
                    "Stable"
                };
                let is_collapsed = wh.collapsed || lifetime_pct <= 0.0 || wh.remaining_mass_kg <= 0.0;
                let dest_sys = wh.destination_system_id.unwrap_or(31_000_005) as i64;
                let entrance_wh_id =
                    crate::services::space_destiny::wormhole_item_id_for_signature(sys_id, &wh.signature_id);
                let exit_wh_id =
                    crate::services::space_destiny::wormhole_exit_item_id_for_signature(
                        dest_sys as u32,
                        &wh.signature_id,
                    );
                let rem_pol = ctx.db.with_snapshot(|snap| {
                    snap.wh_polarizations
                        .get(&(char_id, dest_sys))
                        .copied()
                        .unwrap_or(0)
                        .saturating_sub(now_unix)
                });
                return Some(Ok(PyRep::keyval([
                    ("signatureID", PyRep::String(wh.signature_id)),
                    ("entranceWormholeID", PyRep::Integer(entrance_wh_id)),
                    ("exitWormholeID", PyRep::Integer(exit_wh_id)),
                    (
                        "wormholeTypeID",
                        wh.wormhole_type_id
                            .map(|t| PyRep::Integer(t as i64))
                            .unwrap_or(PyRep::None),
                    ),
                    ("totalLifespanSeconds", PyRep::Integer(wh.lifetime_seconds as i64)),
                    ("elapsedSeconds", PyRep::Integer(elapsed as i64)),
                    ("lifetimeStage", PyRep::String(lifetime_stage.into())),
                    ("massStage", PyRep::String(mass_stage.into())),
                    ("remainingMassKg", PyRep::Real(wh.remaining_mass_kg)),
                    ("maxJumpMassKg", PyRep::Real(wh.max_jump_mass_kg)),
                    ("destinationSystemID", PyRep::Integer(dest_sys)),
                    ("destinationClass", PyRep::String(buckets.destination_class.into())),
                    ("maxShipSize", PyRep::String(buckets.max_ship_size.into())),
                    ("lifetimeBucket", PyRep::String(buckets.lifetime_bucket.into())),
                    ("massStabilityBucket", PyRep::String(buckets.mass_stability_bucket.into())),
                    ("lifetimeDescription", PyRep::String(buckets.lifetime_description.into())),
                    (
                        "massStabilityDescription",
                        PyRep::String(buckets.mass_stability_description.into()),
                    ),
                    ("collapsed", PyRep::Bool(is_collapsed)),
                    ("isPolarized", PyRep::Bool(rem_pol > 0)),
                    ("remainingSeconds", PyRep::Integer(rem_pol as i64)),
                ])));
            }

            let target_sys = args.first().and_then(|a| a.as_i64()).unwrap_or(31_000_005);
            let remaining = ctx.db.with_snapshot(|snap| {
                snap.wh_polarizations
                    .get(&(char_id, target_sys))
                    .copied()
                    .unwrap_or(0)
                    .saturating_sub(now_unix)
            });
            let is_polarized = remaining > 0;
            let sys_u32 = target_sys as u32;
            let is_wh = (31_000_000..=31_002_605).contains(&sys_u32);
            let is_thera = sys_u32 == 31_000_005;
            let is_shattered = is_thera || (is_wh && (sys_u32 % 10 == 0));
            let wh_class = if is_thera {
                12
            } else if is_wh {
                ((sys_u32 - 31_000_000) % 6) + 1
            } else {
                0
            };
            let allows_capitals = !is_wh || (wh_class >= 5 && !is_shattered);
            let statics = if is_thera {
                vec![
                    vec![PyRep::String("HighSec".into()), PyRep::String("HS-Static".into()), PyRep::Integer(10_000_000)],
                    vec![PyRep::String("LowSec".into()), PyRep::String("LS-Static".into()), PyRep::Integer(10_000_000)],
                    vec![PyRep::String("NullSec".into()), PyRep::String("NS-Static".into()), PyRep::Integer(10_000_000)],
                ]
            } else if is_wh {
                let code = match wh_class {
                    1 => "H296",
                    2 => "C247",
                    3 => "D845",
                    4 => "E175",
                    5 => "H900",
                    _ => "V898",
                };
                vec![vec![
                    PyRep::String(format!("C{wh_class}-Static")),
                    PyRep::String(code.into()),
                    PyRep::Integer(wh_class as i64),
                ]]
            } else {
                vec![vec![PyRep::String("Wandering".into()), PyRep::String("Unknown".into()), PyRep::Integer(0)]]
            };

            Some(Ok(PyRep::keyval([
                ("destinationSystemID", PyRep::Integer(target_sys)),
                ("systemID", PyRep::Integer(target_sys)),
                ("isPolarized", PyRep::Bool(is_polarized)),
                ("remainingSeconds", PyRep::Integer(remaining as i64)),
                ("wormholeClass", PyRep::Integer(wh_class as i64)),
                ("isThera", PyRep::Bool(is_thera)),
                ("isShattered", PyRep::Bool(is_shattered)),
                ("delayedLocal", PyRep::Bool(is_wh)),
                ("allowsCapitalShips", PyRep::Bool(allows_capitals)),
                ("statics", PyRep::rowset(vec!["name", "code", "targetClassOrSec"], statics)),
            ])))
        }

        // =====================================================================
        // Dungeons, Escalations & Anomalies (`keeper`, `dungeon*`)
        // =====================================================================
        ("keeper", "ActivateAccelerationGate")
        | ("beyonce", "ActivateAccelerationGate") => {
            let (_, _char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "Session has no active solar system".into(),
                }));
            };
            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "Session has no active ship".into(),
                }));
            };
            let gate_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let sys_handle = ctx.systems.get_or_spawn(sys_id);
            let Some((_, ship_ball)) = sys_handle.get_ball_snapshot(ship_id).await.ok().flatten() else {
                return Some(Err(BridgeRpcError {
                    code: "ShipNotInSpace".into(),
                    message: "Ship is not in space".into(),
                }));
            };

            let ship_type_id = ship_ball.type_id;
            let ship_type = ctx.sde.types.get(&ship_type_id);
            let ship_rig_size = ship_type
                .and_then(|t| t.default_attributes.get(&1547).copied())
                .unwrap_or(0.0);
            let ship_group_id = ship_type
                .map(|t| t.group_id)
                .unwrap_or(0);

            // SDE dogma rigSize: 4.0 = Capital hulls; mass > 500M kg = Freighters / Capital Industrials
            if ship_rig_size >= 4.0
                || ship_ball.mass > 500_000_000.0
                || [30, 485, 547, 659, 883, 1538, 4594].contains(&ship_group_id)
            {
                return Some(Err(BridgeRpcError {
                    code: "ShipTooLargeForGate".into(),
                    message: "Capital ships cannot activate this acceleration gate".into(),
                }));
            }

            if gate_id > 0 {
                if let Ok(Some((_, gate_ball))) = sys_handle.get_ball_snapshot(gate_id).await {
                    let dist = ship_ball.position.distance(gate_ball.position);
                    if dist > 2_500.0 {
                        return Some(Err(BridgeRpcError {
                            code: "OutsideGateActivationRange".into(),
                            message: format!("Ship is {:.1} m from gate (must be <= 2500 m)", dist),
                        }));
                    }
                }
            }

            let gate_check = ctx.db.try_with_mut_snapshot(|snap| {
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

                let effective_dungeon_id = dungeon_id_opt.or_else(|| {
                    args.get(1)
                        .and_then(|a| a.as_i64())
                        .and_then(|v| u32::try_from(v).ok())
                });

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
                                    });
                                }
                            }
                            _ => {}
                        }
                    }
                }

                let mut key_consumed = false;
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
                        });
                    };

                    if consume_key {
                        if let Some(item) = snap.items.get_mut(&kid) {
                            item.quantity -= 1;
                            if item.quantity <= 0 {
                                snap.items.remove(&kid);
                            }
                            key_consumed = true;
                        }
                    }
                }
                Ok((effective_dungeon_id, req_key_opt, key_consumed))
            });

            let (dungeon_id_used, key_type_used, key_consumed) = match gate_check {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };

            let pocket_pos = ship_ball.position + DVec3::new(500_000.0, 0.0, 0.0);
            let warp_mode = MovementMode::new_warp(ship_ball.position, pocket_pos, 3.0);
            let _ = sys_handle.set_movement_mode(ship_id, warp_mode).await;

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

        ("dungeonExplorationMgr", "GetMyEscalatingPathDetails") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let now_unix = ctx.now_unix();
            let mut st = ctx.social_extras.lock().unwrap();
            let list = st.escalations.entry(char_id).or_default();
            if list.is_empty() {
                list.push((
                    800_001,
                    "DED 4/10 Sansha Command Complex".into(),
                    30_000_144,
                    now_unix + 86_400,
                ));
            }
            let rows: Vec<Vec<PyRep>> = list
                .iter()
                .filter(|(_, _, _, exp)| *exp > now_unix)
                .map(|(eid, name, dest_sys, exp)| {
                    vec![
                        PyRep::Integer(*eid),
                        PyRep::String(name.clone()),
                        PyRep::Integer(*dest_sys as i64),
                        PyRep::Integer(*exp as i64),
                    ]
                })
                .collect();
            Some(Ok(PyRep::rowset(
                vec!["instanceID", "dungeonName", "solarSystemID", "expiryTime"],
                rows,
            )))
        }

        ("livingWorld", "GetMiningOps") => {
            let sys_id = args
                .first()
                .and_then(|a| a.as_i64())
                .map(|v| v as u32)
                .or(session.solar_system_id)
                .unwrap_or(30_000_144);
            let convoys = ctx.living_world.list_convoys();
            let mining_ops: Vec<_> = convoys
                .into_iter()
                .filter(|c| {
                    c.archetype == crate::living_world::NpcArchetype::MiningOp
                        && c.current_system_id == sys_id
                })
                .collect();
            let rows: Vec<Vec<PyRep>> = mining_ops
                .into_iter()
                .map(|c| {
                    vec![
                        PyRep::Integer(c.convoy_id),
                        PyRep::String("MiningOp".into()),
                        PyRep::Integer(c.ship_type_id as i64),
                        PyRep::Integer(648),
                        PyRep::Integer(c.current_system_id as i64),
                        PyRep::Integer(c.cargo_type_id as i64),
                        PyRep::Integer(c.cargo_quantity as i64),
                        PyRep::Bool(c.destroyed),
                    ]
                })
                .collect();
            Some(Ok(PyRep::rowset(
                vec![
                    "convoyID",
                    "archetype",
                    "minerShipTypeID",
                    "haulerShipTypeID",
                    "systemID",
                    "cargoTypeID",
                    "cargoQuantity",
                    "destroyed",
                ],
                rows,
            )))
        }

        ("livingWorld", "StepMiningOp") => {
            let Some(convoy_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "StepMiningOp requires convoyID".into(),
                }));
            };
            match ctx
                .living_world
                .step_npc_mining_op(convoy_id, &ctx.systems, &ctx.db, &ctx.sde)
                .await
            {
                Ok(res) => Some(Ok(PyRep::keyval([
                    ("convoyID", PyRep::Integer(res.convoy_id)),
                    ("systemID", PyRep::Integer(res.system_id as i64)),
                    ("asteroidID", PyRep::Integer(res.asteroid_id)),
                    ("oreTypeID", PyRep::Integer(res.ore_type_id as i64)),
                    ("minedUnits", PyRep::Integer(res.mined_units as i64)),
                    ("remainingAsteroidUnits", PyRep::Integer(res.remaining_asteroid_units as i64)),
                    ("convoyCargoUnits", PyRep::Integer(res.convoy_cargo_units as i64)),
                    ("hauledToStation", PyRep::Bool(res.hauled_to_station)),
                ]))),
                Err(e) => Some(Err(BridgeRpcError {
                    code: "NpcMiningOpFailed".into(),
                    message: e,
                })),
            }
        }

        _ => None,
    }
}

/// Signatures for a system come from the shared space-layer seeding/lifetime path so the scanner,
/// signal tracker and probe results always agree.
async fn ensure_system_signatures(ctx: &ServerContext, sys_id: u32) -> Vec<CosmicSignatureRecord> {
    crate::services::space_destiny::ensure_system_signatures_and_anomalies(ctx, sys_id).await.0
}
