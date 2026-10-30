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
            if method == "SignalTrackerRegister" {
                register_signal_tracker_subscriber(ctx, sys_id, char_id);
            }
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
            if sys_id == 30_100_000 || (32_000_000..=32_999_999).contains(&sys_id) {
                return Some(Err(BridgeRpcError::user_error("CannotDscanInThisSystem")));
            }
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
            let arg_str_opt = args.first().and_then(|a| a.as_str());
            let arg_int_opt = args.first().and_then(|a| a.as_i64());

            // Match either a signature ID string (`WST-101` or `K162-WST-101`) or a wormhole ball ID (`>= 40_000_000`).
            if arg_str_opt.is_some() || arg_int_opt.is_some_and(|id| id >= 40_000_000) {
                let _ = ensure_system_signatures(ctx, sys_id).await;
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
                    return Some(Err(BridgeRpcError {
                        code: "WormholeNotFound".into(),
                        message: format!("Wormhole not found in system {sys_id}"),
                    }));
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

            let target_sys = arg_int_opt.unwrap_or(31_000_005);
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
                }));
            }
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
            if ctx.db.with_snapshot(|snap| snap.fw_runtime.fw_complexes.contains_key(&gate_id)) {
                return Some(
                    crate::services::fw_gameplay::activate_fw_complex_gate_in_space(
                        ctx, char_id, ship_id, sys_id, gate_id,
                    )
                    .await,
                );
            }
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

        _ => None,
    }
}

/// Signatures for a system come from the shared space-layer seeding/lifetime path so the scanner,
/// signal tracker and probe results always agree.
async fn ensure_system_signatures(ctx: &ServerContext, sys_id: u32) -> Vec<CosmicSignatureRecord> {
    crate::services::space_destiny::ensure_system_signatures_and_anomalies(ctx, sys_id).await.0
}

const SIGNAL_TRACKER_SUB_PREFIX: &str = "signal_tracker_sub:";

pub(crate) fn register_signal_tracker_subscriber(ctx: &ServerContext, sys_id: u32, char_id: i64) {
    let target_key = format!("{SIGNAL_TRACKER_SUB_PREFIX}{sys_id}");
    ctx.db.with_mut_snapshot(|snap| {
        let existing_keys: Vec<String> = snap
            .service_state_blobs
            .keys()
            .filter(|k| k.starts_with(SIGNAL_TRACKER_SUB_PREFIX) && k.as_str() != target_key)
            .cloned()
            .collect();
        for key in existing_keys {
            if let Some(raw) = snap.service_state_blobs.get(&key) {
                let mut subs: Vec<i64> = serde_json::from_str(raw).unwrap_or_default();
                subs.retain(|&c| c != char_id);
                if subs.is_empty() {
                    snap.service_state_blobs.remove(&key);
                } else if let Ok(json) = serde_json::to_string(&subs) {
                    snap.service_state_blobs.insert(key, json);
                }
            }
        }
        let mut subs: Vec<i64> = snap
            .service_state_blobs
            .get(&target_key)
            .and_then(|raw| serde_json::from_str(raw).ok())
            .unwrap_or_default();
        if !subs.contains(&char_id) {
            subs.push(char_id);
            subs.sort_unstable();
        }
        if let Ok(json) = serde_json::to_string(&subs) {
            snap.service_state_blobs.insert(target_key, json);
        }
    });
}

pub(crate) fn signal_tracker_subscribers_in_snap(
    snap: &eve_db::WorldStateSnapshot,
    sys_id: u32,
) -> Vec<i64> {
    let key = format!("{SIGNAL_TRACKER_SUB_PREFIX}{sys_id}");
    let Some(raw) = snap.service_state_blobs.get(&key) else {
        return Vec::new();
    };
    let mut subs: Vec<i64> = serde_json::from_str(raw).unwrap_or_default();
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

pub(crate) fn build_signal_tracker_anomaly_kv(anom: &eve_db::CosmicAnomalyRecord) -> PyRep {
    let pos_rep = PyRep::Tuple(vec![PyRep::Real(anom.position[0]), PyRep::Real(anom.position[1]), PyRep::Real(anom.position[2])]);
    PyRep::keyval([
        ("instanceID", PyRep::String(anom.anomaly_id.clone())),
        ("position", pos_rep),
        ("targetID", PyRep::String(anom.anomaly_id.clone())),
        ("difficulty", PyRep::Integer(1)),
        ("dungeonID", PyRep::None),
        ("archetypeID", PyRep::None),
        ("dungeonNameID", PyRep::None),
        ("factionID", PyRep::None),
        ("scanStrengthAttribute", PyRep::None),
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

pub(crate) fn emit_signal_tracker_signature_delta(
    ctx: &ServerContext,
    sys_id: u32,
    added: &[CosmicSignatureRecord],
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
            .map(|sig| {
                (
                    PyRep::String(sig.signature_id.clone()),
                    build_signal_tracker_signature_kv(&ctx.sde, sig),
                )
            })
            .collect(),
    );
    let removed_list = PyRep::List(removed_ids.iter().cloned().map(PyRep::String).collect());
    let args = vec![PyRep::Integer(i64::from(sys_id)), added_dict, removed_list];
    for cid in subs {
        ctx.notify(
            NotificationTarget::Character(cid),
            "OnSignalTrackerSignatureUpdate",
            "charid",
            args.clone(),
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
                    build_signal_tracker_anomaly_kv(anom),
                )
            })
            .collect(),
    );
    let removed_list = PyRep::List(removed_ids.iter().cloned().map(PyRep::String).collect());
    let args = vec![PyRep::Integer(i64::from(sys_id)), added_dict, removed_list];
    for cid in subs {
        ctx.notify(
            NotificationTarget::Character(cid),
            "OnSignalTrackerAnomalyUpdate",
            "charid",
            args.clone(),
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
    let args = vec![PyRep::Integer(i64::from(sys_id)), added_dict, removed_list];
    for cid in subs {
        ctx.notify(
            NotificationTarget::Character(cid),
            "OnSignalTrackerStructureUpdate",
            "charid",
            args.clone(),
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
        ctx.notify(
            NotificationTarget::Character(cid),
            "OnSignalTrackerStructureUpdate",
            "charid",
            args,
        );
    }
}

pub(crate) fn emit_signal_tracker_site_removed_with_db(
    db: &eve_db::WorldRepository,
    pending_notifications: &std::sync::RwLock<
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
    let mut queues = pending_notifications.write().unwrap_or_else(|e| e.into_inner());
    for (event_name, active) in [
        ("OnSignalTrackerAnomalyUpdate", was_anom),
        ("OnSignalTrackerSignatureUpdate", was_sig),
    ] {
        if !active {
            continue;
        }
        let args = vec![
            PyRep::Integer(i64::from(sys_id)),
            PyRep::Dict(Vec::new()),
            PyRep::List(vec![PyRep::String(site_id.to_string())]),
        ];
        for &(cid, uid) in &recips {
            let (packet, marshaled_packet) = ServerContext::build_macho_notification_packet(
                cid, uid, event_name, "charid", args.clone(),
            );
            let notif = crate::router::cache_notifications::PendingMachoNotification {
                recipient_char_id: cid, event: event_name.into(), id_type: "charid".into(),
                args: args.clone(), packet, marshaled_packet, created_filetime: now_ft,
            };
            let q = queues.entry(cid).or_default();
            if q.len() >= crate::router::cache_notifications::MAX_PENDING_NOTIFICATIONS_PER_CHAR {
                q.remove(0);
            }
            q.push(notif.clone());
            let _ = notify_tx.send((cid, notif));
        }
    }
}
