//! `space_probe_scanning` — Core & Combat scanner probe launch, range/deviation math,
//! target class filtering, signature reveal thresholds, difficulty levels, and flight-time expiry.

#![forbid(unsafe_code)]

use crate::router::{BoundObjectEntry, ServerContext};
use crate::services::space_destiny::{
    compute_wormhole_stages, derive_wormhole_buckets, ensure_system_signatures_and_anomalies,
    system_wormhole_profile, wormhole_item_id_for_signature, AU_IN_METERS,
};
use eve_db::types::{CosmicAnomalyRecord, CosmicSignatureRecord, InventoryFlag};
use eve_destiny::Ball;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::{SdeRegistry, SolarSystemId};
use glam::DVec3;
use std::collections::HashMap;

// parity: feat:scanning.probe-target-classes, feat:scanning.probe-range-deviation
pub fn is_combat_scanner_probe(sde: &SdeRegistry, probe_type_id: u32) -> bool {
    sde.types
        .get(&probe_type_id)
        .and_then(|t| t.default_attributes.get(&1413).copied())
        .unwrap_or_else(|| sde.attributes.get(&1413).map(|a| a.default_value).unwrap_or(0.0))
        >= 0.5
}

// parity: feat:scanning.probe-range-deviation
pub fn probe_base_scan_range_au(sde: &SdeRegistry, probe_type_id: u32) -> f64 {
    sde.types
        .get(&probe_type_id)
        .and_then(|t| t.default_attributes.get(&1370).copied())
        .filter(|&v| v > 0.0)
        .unwrap_or_else(|| sde.attributes.get(&1370).map(|a| a.default_value.max(0.25)).unwrap_or(0.25))
}

// parity: feat:scanning.probe-range-deviation
pub fn probe_allowed_ranges_au(sde: &SdeRegistry, probe_type_id: u32) -> [f64; 8] {
    let base = probe_base_scan_range_au(sde, probe_type_id);
    std::array::from_fn(|i| base * 2.0_f64.powi(i as i32))
}

// parity: feat:scanning.probe-range-deviation
pub fn probe_base_max_deviation_au(sde: &SdeRegistry, probe_type_id: u32) -> f64 {
    sde.types
        .get(&probe_type_id)
        .and_then(|t| t.default_attributes.get(&1372).copied())
        .filter(|&v| v > 0.0)
        .unwrap_or_else(|| sde.attributes.get(&1372).map(|a| a.default_value.max(0.125)).unwrap_or(0.125))
}

// parity: feat:scanning.probe-range-deviation
pub fn probe_base_sensor_strength(sde: &SdeRegistry, probe_type_id: u32) -> f64 {
    sde.types
        .get(&probe_type_id)
        .and_then(|t| t.default_attributes.get(&1371).copied())
        .filter(|&v| v > 0.0)
        .unwrap_or_else(|| sde.attributes.get(&1371).map(|a| a.default_value.max(1.0)).unwrap_or(40.0))
}

// parity: feat:scanning.probe-range-deviation
pub fn probe_max_flight_time_s(sde: &SdeRegistry, probe_type_id: u32) -> f64 {
    sde.types
        .get(&probe_type_id)
        .and_then(|t| t.default_attributes.get(&281).copied())
        .filter(|&v| v > 0.0)
        .map(|ms| ms / 1000.0)
        .unwrap_or_else(|| sde.attributes.get(&281).map(|a| (a.default_value / 1000.0).max(1.0)).unwrap_or(4_000.0))
}

// parity: feat:scanning.probe-range-deviation
pub fn compute_probe_max_deviation_au(
    base_deviation_au: f64,
    range_au: f64,
    base_range_au: f64,
    astrometrics_level: u8,
    pinpointing_level: u8,
) -> f64 {
    let astro_factor = 1.0 - 0.05 * (astrometrics_level.min(5) as f64);
    let pinpoint_factor = 1.0 - 0.05 * (pinpointing_level.min(5) as f64);
    base_deviation_au * (range_au / base_range_au.max(0.01)) * astro_factor * pinpoint_factor
}

// parity: feat:scanning.signature-strength-levels
pub fn signature_difficulty_level_and_min_strength(
    sde: &SdeRegistry,
    _sys_id: SolarSystemId,
    sig: &CosmicSignatureRecord,
) -> (u8, f64) {
    let target_name = sig.type_name.trim();
    let level = sde
        .dungeons
        .values()
        .find(|d| d.name.trim().eq_ignore_ascii_case(target_name))
        .and_then(|d| d.difficulty)
        .map(|diff| (diff as u8).clamp(1, 5))
        .unwrap_or(1);
    let min_strength = match level {
        1..=3 => 42.0,
        4 => 72.0,
        _ => 104.0,
    };
    (level, min_strength)
}

/// Checks for any active probes belonging to `char_id` that have exceeded `max_flight_time_s`
/// (16m40s = 1000s for RSS Core Scanner Probe `32246`, 1h6m40s = 4000s for others).
/// Probes past their max flight time return to `InventoryFlag::Cargo` if there is free cargo room,
/// otherwise they are lost.
// parity: feat:scanning.probe-range-deviation
pub async fn expire_overdue_probes_for_char(
    ctx: &ServerContext,
    char_id: i64,
    now_unix: u64,
) -> (Vec<i64>, Vec<i64>) {
    let newly_expired: Vec<crate::router::ActiveScannerProbe> =
        if let Ok(mut map) = ctx.active_scanner_probes.write() {
            map.values_mut()
                .filter(|p| {
                    p.owner_char_id == char_id
                        && p.state >= 0
                        && now_unix >= p.launched_at_unix.saturating_add(p.max_flight_time_s.max(0.0) as u64)
                })
                .map(|p| p.clone())
                .collect()
        } else {
            Vec::new()
        };

    if newly_expired.is_empty() {
        return (Vec::new(), Vec::new());
    }

    for p in &newly_expired {
        let sys_handle = ctx.systems.get_or_spawn(p.solar_system_id);
        let _ = sys_handle.remove_ball(p.probe_id).await;
    }

    let (returned_ids, lost_ids) = ctx.db.with_mut_snapshot(|snap| {
        let mut ret = Vec::new();
        let mut lost = Vec::new();
        for p in &newly_expired {
            let probe_vol = ctx.sde.types.get(&p.probe_type_id).map(|t| t.volume.max(0.01)).unwrap_or(0.1);
            let ship_cap = snap
                .items
                .get(&p.ship_id)
                .and_then(|si| {
                    si.dynamic_attributes.get(&38).copied().or_else(|| ctx.sde.types.get(&si.type_id).map(|t| t.capacity))
                })
                .unwrap_or(400.0);
            let used_vol: f64 = snap
                .items
                .values()
                .filter(|i| i.location_id == p.ship_id && i.flag == InventoryFlag::Cargo && i.quantity > 0)
                .map(|i| (i.quantity as f64) * ctx.sde.types.get(&i.type_id).map(|t| t.volume).unwrap_or(0.1))
                .sum();
            if used_vol + probe_vol <= ship_cap + 1e-6 && snap.items.contains_key(&p.ship_id) {
                if let Some(st) = snap.items.values_mut().find(|i| {
                    i.owner_id == char_id && i.location_id == p.ship_id && i.flag == InventoryFlag::Cargo && i.type_id == p.probe_type_id
                }) {
                    st.quantity += 1;
                    st.version += 1;
                } else {
                    snap.next_entity_id += 1;
                    let new_id = snap.next_entity_id;
                    snap.items.insert(
                        new_id,
                        eve_db::InventoryItemRecord {
                            item_id: new_id,
                            type_id: p.probe_type_id,
                            owner_id: char_id,
                            location_id: p.ship_id,
                            flag: InventoryFlag::Cargo,
                            quantity: 1,
                            singleton: false,
                            version: 1,
                            ..Default::default()
                        },
                    );
                }
                ret.push(p.probe_id);
            } else {
                lost.push(p.probe_id);
            }
        }
        (ret, lost)
    });

    if let Ok(mut map) = ctx.active_scanner_probes.write() {
        for pid in &returned_ids {
            if let Some(p) = map.get_mut(pid) {
                p.state = -1; // expired & auto-returned to cargo
            }
        }
        for pid in &lost_ids {
            if let Some(p) = map.get_mut(pid) {
                p.state = -2; // expired & lost (cargo full)
            }
        }
    }

    (returned_ids, lost_ids)
}

/// Background cluster sweep that expires overdue scanner probes across all characters.
// parity: feat:scanning.probe-range-deviation
pub async fn expire_all_overdue_probes(ctx: &ServerContext, now_unix: u64) {
    let overdue_owners: Vec<i64> = if let Ok(map) = ctx.active_scanner_probes.read() {
        let mut owners: Vec<i64> = map
            .values()
            .filter(|p| {
                p.state >= 0
                    && now_unix >= p.launched_at_unix.saturating_add(p.max_flight_time_s.max(0.0) as u64)
            })
            .map(|p| p.owner_char_id)
            .collect();
        owners.sort_unstable();
        owners.dedup();
        owners
    } else {
        Vec::new()
    };
    for cid in overdue_owners {
        let _ = expire_overdue_probes_for_char(ctx, cid, now_unix).await;
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn activate_scan_probe_launcher(
    ctx: &ServerContext,
    session: &SessionState,
    sys: &crate::system_actor::SystemHandle,
    char_id: i64,
    ship_id: i64,
    sys_id: SolarSystemId,
    module_id: i64,
    module_type_id: u32,
    module_flag_id: u16,
    effect_name: &str,
    target_id_opt: Option<i64>,
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    let now_unix = ctx.now_unix_secs();
    let _ = expire_overdue_probes_for_char(ctx, char_id, now_unix).await;

    let loaded_charge_opt = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .find(|i| {
                i.location_id == ship_id
                    && (i.flag as u16) == module_flag_id
                    && i.item_id != module_id
                    && i.quantity > 0
                    && ctx.sde.types.get(&i.type_id).is_some_and(|t| t.group_id == 479)
            })
            .map(|i| (i.item_id, i.type_id, i.quantity))
    });
    let Some((charge_item_id, probe_type_id, loaded_qty)) = loaded_charge_opt else {
        return Err(BridgeRpcError {
            code: "NoProbesLoaded".into(),
            message: "Scan probe launcher has no scanner probes loaded".into(),
        });
    };

    if is_combat_scanner_probe(&ctx.sde, probe_type_id) && matches!(module_type_id, 17938 | 28758 | 30651) {
        return Err(BridgeRpcError {
            code: "InvalidProbeForLauncher".into(),
            message: "Combat scanner probes require an Expanded Probe Launcher".into(),
        });
    }

    let existing_active_count = ctx
        .active_scanner_probes
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .values()
        .filter(|p| p.owner_char_id == char_id && p.solar_system_id == sys_id && p.state > 0)
        .count();
    if existing_active_count >= 8 {
        return Err(BridgeRpcError {
            code: "MaxProbesLaunched".into(),
            message: "Maximum of 8 active scanner probes already in space".into(),
        });
    }

    let max_can_launch = (8 - existing_active_count) as i32;
    let requested_count = kwargs.get("probeCount").and_then(|v| v.as_i64()).unwrap_or(loaded_qty as i64) as i32;
    let to_launch = requested_count.clamp(1, max_can_launch).min(loaded_qty);

    let (allocated_probe_ids, remaining_in_launcher) = ctx.db.with_mut_snapshot(|snap| {
        let mut rem = 0;
        if let Some(ci) = snap.items.get_mut(&charge_item_id) {
            ci.quantity = (ci.quantity - to_launch).max(0);
            rem = ci.quantity;
        }
        if rem == 0 {
            snap.items.remove(&charge_item_id);
        }
        let mut ids = Vec::with_capacity(to_launch as usize);
        for _ in 0..to_launch {
            snap.next_entity_id += 1;
            ids.push(snap.next_entity_id);
        }
        (ids, rem)
    });
    ctx.invalidate_ship_fitting(ship_id);

    let probe_sde = ctx.sde.types.get(&probe_type_id);
    let base_sensor_strength = probe_base_sensor_strength(&ctx.sde, probe_type_id);
    let max_flight_time_s = probe_max_flight_time_s(&ctx.sde, probe_type_id);
    let base_scan_range_au = probe_base_scan_range_au(&ctx.sde, probe_type_id);
    let base_max_deviation_au = probe_base_max_deviation_au(&ctx.sde, probe_type_id);
    let can_scan_ships = is_combat_scanner_probe(&ctx.sde, probe_type_id);
    let allowed_ranges = probe_allowed_ranges_au(&ctx.sde, probe_type_id);
    let range_step = kwargs.get("rangeStep").and_then(|v| v.as_i64()).unwrap_or(4).clamp(1, 8) as i32;
    let scan_range_m = base_scan_range_au * (2.0_f64.powi(range_step - 1)) * AU_IN_METERS;

    let ship_pos = sys.get_ball_snapshot(ship_id).await.ok().flatten().map(|(_, b)| b.position).unwrap_or(DVec3::ZERO);
    let formation_offsets: [[f64; 3]; 8] = [
        [1_000.0, 0.0, 0.0],
        [-1_000.0, 0.0, 0.0],
        [0.0, 1_000.0, 0.0],
        [0.0, -1_000.0, 0.0],
        [0.0, 0.0, 1_000.0],
        [0.0, 0.0, -1_000.0],
        [707.0, 707.0, 0.0],
        [-707.0, -707.0, 0.0],
    ];

    for (idx, &probe_id) in allocated_probe_ids.iter().enumerate() {
        let off = formation_offsets[idx % formation_offsets.len()];
        let pos = DVec3::new(ship_pos.x + off[0], ship_pos.y + off[1], ship_pos.z + off[2]);
        let ball = Ball {
            id: probe_id,
            type_id: probe_type_id,
            position: pos,
            velocity: DVec3::ZERO,
            max_velocity: 3_000.0,
            inertia: 1.0,
            mass: probe_sde.map(|t| t.mass.max(1.0)).unwrap_or(1.0),
            radius: probe_sde.map(|t| t.radius.max(1.0)).unwrap_or(2.0),
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        };
        let _ = sys
            .spawn_ball_with_meta(
                ball,
                eve_destiny::CombatState::default(),
                eve_destiny::SlimItemMeta {
                    group_id: 479,
                    category_id: 8,
                    owner_id: char_id,
                    char_id: Some(char_id),
                    corp_id: session.corporation_id.unwrap_or(-1),
                    alliance_id: session.alliance_id.unwrap_or(-1),
                    security_status: 0.0,
                    is_cloaked: false,
                },
            )
            .await;
        ctx.active_scanner_probes
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                probe_id,
                crate::router::ActiveScannerProbe {
                    probe_id,
                    probe_type_id,
                    owner_char_id: char_id,
                    ship_id,
                    launcher_module_id: module_id,
                    solar_system_id: sys_id,
                    position: pos.to_array(),
                    destination: pos.to_array(),
                    range_step,
                    scan_range_m,
                    base_sensor_strength,
                    max_flight_time_s,
                    launched_at_unix: now_unix,
                    state: 1,
                },
            );
    }

    let _ = ctx.notify_module_effect_started(char_id, ship_id, module_id, module_type_id, Some(effect_name), target_id_opt);
    for &pid in &allocated_probe_ids {
        let probe_kv = PyRep::keyval([
            ("probeID", PyRep::Integer(pid)),
            ("typeID", PyRep::Integer(probe_type_id as i64)),
            ("scanRange", PyRep::Real(scan_range_m)),
            ("expiry", PyRep::Integer(now_unix as i64 + max_flight_time_s as i64)),
        ]);
        let _ = ctx.notify(crate::router::NotificationTarget::Character(char_id), "OnNewProbe", "charid", vec![probe_kv]);
    }
    Ok(PyRep::keyval([
        ("moduleID", PyRep::Integer(module_id)),
        ("launcherID", PyRep::Integer(module_id)),
        ("probeTypeID", PyRep::Integer(probe_type_id as i64)),
        ("probeIDs", PyRep::List(allocated_probe_ids.into_iter().map(PyRep::Integer).collect())),
        ("launchedCount", PyRep::Integer(to_launch as i64)),
        ("remainingInLauncher", PyRep::Integer(remaining_in_launcher as i64)),
        ("baseSensorStrength", PyRep::Real(base_sensor_strength)),
        ("baseScanRangeAU", PyRep::Real(base_scan_range_au)),
        ("baseMaxDeviationAU", PyRep::Real(base_max_deviation_au)),
        ("canScanShips", PyRep::Bool(can_scan_ships)),
        ("maxFlightTimeS", PyRep::Real(max_flight_time_s)),
        ("allowedRangesAU", PyRep::List(allowed_ranges.into_iter().map(PyRep::Real).collect())),
    ]))
}

pub(crate) async fn handle_scan_and_wormhole_extras(
    ctx: &ServerContext,
    session: &mut SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    // parity: feat:scanning.probe-launch, feat:scanning.probe-recall, feat:scanning.probe-scan-math
    // parity: feat:scanning.probe-range-deviation, feat:scanning.probe-target-classes, feat:scanning.signature-reveal-thresholds, feat:scanning.signature-strength-levels
    // parity: rpc:scanMgr.GetSystemScanMgr
    match (service, method) {
        ("scanMgr", "GetSystemScanMgr") => handle_get_system_scan_mgr(ctx, session, service, args).await,
        ("scanMgr", "GetAnomalies") => handle_get_anomalies(ctx, session, bound_entry).await,
        ("scanMgr", "RequestScans") => handle_request_scans(ctx, session, bound_entry, args, kwargs).await,
        ("scanMgr", "RecoverProbes") => handle_recover_probes(ctx, session, bound_entry, args).await,
        ("scanMgr", "ReconnectToLostProbes") => handle_reconnect_probes(ctx, session, bound_entry).await,
        ("scanMgr", "DestroyProbe") => handle_destroy_probe(ctx, session, bound_entry, args).await,
        _ => None,
    }
}

fn format_anomalies(anomalies: Vec<CosmicAnomalyRecord>, now_unix: u64) -> Vec<PyRep> {
    anomalies
        .into_iter()
        .filter(|a| !a.cleared && (a.lifetime_seconds == 0 || now_unix < a.created_unix.saturating_add(a.lifetime_seconds)))
        .map(|a| {
            PyRep::keyval([
                ("anomalyID", PyRep::String(a.anomaly_id)),
                ("name", PyRep::String(a.name)),
                ("anomalyType", PyRep::String(a.anomaly_type)),
                ("signalStrength", PyRep::Real(1.0)),
                ("resolved", PyRep::Bool(true)),
                ("warpable", PyRep::Bool(true)),
                ("x", PyRep::Real(a.position[0])),
                ("y", PyRep::Real(a.position[1])),
                ("z", PyRep::Real(a.position[2])),
            ])
        })
        .collect()
}

async fn handle_get_system_scan_mgr(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (_, char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let caller_sys = session
        .solar_system_id
        .or_else(|| ctx.db.with_snapshot(|s| s.characters.get(&char_id).map(|c| c.solar_system_id)))
        .unwrap_or(30_000_142);
    if let Some(arg_sys) = args.first().and_then(|a| a.as_i64()).map(|v| v as u32) {
        if arg_sys != caller_sys && !session.has_gm_role() {
            return Some(Err(BridgeRpcError {
                code: "AccessDenied".into(),
                message: "Cannot query scan manager for a remote solar system".into(),
            }));
        }
    }
    let sys_id = caller_sys;
    let now_unix = ctx.now_unix();
    let _ = expire_overdue_probes_for_char(ctx, char_id, now_unix).await;
    let (sigs, anomalies) = ensure_system_signatures_and_anomalies(ctx, sys_id).await;
    let bound_id = match ctx.bind_object(session, service, sys_id as i64) {
        Ok(b) => b,
        Err(e) => return Some(Err(e)),
    };
    let (wh_class, is_thera, is_shattered, env_effect) = system_wormhole_profile(sys_id);

    let sig_items: Vec<PyRep> = sigs
        .into_iter()
        .filter(|s| !s.collapsed && (s.lifetime_seconds == 0 || now_unix < s.created_unix.saturating_add(s.lifetime_seconds)))
        .map(|s| {
            let eff_strength = ctx.effective_signature_signal_strength(char_id, sys_id, &s);
            let resolved = eff_strength >= 1.0;
            let show_group = eff_strength >= 0.25;
            let show_name = eff_strength >= 0.75;
            let wh_item_id = wormhole_item_id_for_signature(sys_id, &s.signature_id);
            let mut kv = vec![
                ("id", PyRep::String(s.signature_id.clone())),
                ("signatureID", PyRep::String(s.signature_id.clone())),
                ("itemID", PyRep::Integer(wh_item_id)),
                ("targetID", PyRep::Integer(wh_item_id)),
                ("group", PyRep::String(if show_group { s.sig_group.clone() } else { "Unknown".into() })),
                ("groupName", if show_group { PyRep::String(s.sig_group.clone()) } else { PyRep::None }),
                ("type", PyRep::String(if show_name { s.type_name.clone() } else { "Unidentified Signature".into() })),
                ("siteName", if show_name { PyRep::String(s.type_name.clone()) } else { PyRep::None }),
                ("signalStrength", PyRep::Real(eff_strength)),
                ("warpable", PyRep::Bool(resolved)),
                ("resolved", PyRep::Bool(resolved)),
                ("x", if resolved { PyRep::Real(s.position[0]) } else { PyRep::None }),
                ("y", if resolved { PyRep::Real(s.position[1]) } else { PyRep::None }),
                ("z", if resolved { PyRep::Real(s.position[2]) } else { PyRep::None }),
                ("destinationSystemID", if resolved { s.destination_system_id.map(|d| PyRep::Integer(d as i64)).unwrap_or(PyRep::None) } else { PyRep::None }),
            ];
            if show_name {
                let (diff_level, req_min_strength) = signature_difficulty_level_and_min_strength(&ctx.sde, sys_id, &s);
                kv.extend([
                    ("difficultyLevel", PyRep::Integer(diff_level as i64)),
                    ("requiredProbeStrength", PyRep::Real(req_min_strength)),
                ]);
            }
            if resolved {
                let (mass_stage, wh_size, life_stage, wh_age, is_collapsed) = compute_wormhole_stages(&s, now_unix);
                let buckets = derive_wormhole_buckets(&s, now_unix, &ctx.sde);
                kv.extend([
                    ("typeID", s.wormhole_type_id.map(|t| PyRep::Integer(t as i64)).unwrap_or(PyRep::None)),
                    ("massStage", PyRep::String(mass_stage.into())),
                    ("wormholeSize", PyRep::Real(wh_size)),
                    ("lifetimeStage", PyRep::String(life_stage.into())),
                    ("wormholeAge", PyRep::Integer(wh_age)),
                    ("lifetimeDescription", PyRep::String(buckets.lifetime_description.into())),
                    ("massStabilityDescription", PyRep::String(buckets.mass_stability_description.into())),
                    ("remainingMassKg", PyRep::Real(s.remaining_mass_kg)),
                    ("maxJumpMassKg", PyRep::Real(s.max_jump_mass_kg)),
                    ("collapsed", PyRep::Bool(is_collapsed)),
                ]);
            }
            PyRep::keyval(kv)
        })
        .collect();

    let anom_items = format_anomalies(anomalies, now_unix);
    Some(Ok(PyRep::keyval([
        ("boundID", PyRep::Integer(bound_id as i64)),
        ("solarSystemID", PyRep::Integer(sys_id as i64)),
        ("wormholeClass", PyRep::Integer(wh_class as i64)),
        ("isThera", PyRep::Bool(is_thera)),
        ("isShattered", PyRep::Bool(is_shattered)),
        ("delayedLocal", PyRep::Bool(wh_class > 0)),
        ("allowsCapitalShips", PyRep::Bool(wh_class == 0 || (wh_class >= 5 && !is_shattered))),
        ("environmentalEffect", PyRep::String(env_effect.into())),
        ("signatures", PyRep::List(sig_items)),
        ("anomalies", PyRep::List(anom_items)),
    ])))
}

async fn handle_get_anomalies(
    ctx: &ServerContext,
    session: &mut SessionState,
    bound_entry: Option<&BoundObjectEntry>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (_, char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let caller_sys = session
        .solar_system_id
        .or_else(|| ctx.db.with_snapshot(|s| s.characters.get(&char_id).map(|c| c.solar_system_id)))
        .unwrap_or(30_000_142);
    if let Some(b) = bound_entry {
        if (b.object_id as u32) != caller_sys && !session.has_gm_role() {
            return Some(Err(BridgeRpcError {
                code: "AccessDenied".into(),
                message: "Cannot query anomalies for a remote solar system".into(),
            }));
        }
    }
    let sys_id = caller_sys;
    let now_unix = ctx.now_unix();
    let (_, anomalies) = ensure_system_signatures_and_anomalies(ctx, sys_id).await;
    Some(Ok(PyRep::List(format_anomalies(anomalies, now_unix))))
}

async fn handle_request_scans(
    ctx: &ServerContext,
    session: &mut SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (_, char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let has_any_tracked_probes = ctx.active_scanner_probes.read().ok().is_some_and(|map| {
        map.values().any(|p| p.owner_char_id == char_id)
    });
    if !has_any_tracked_probes
        && bound_entry.is_none()
        && (args.is_empty() || matches!(args.first(), Some(PyRep::Real(_))))
        && kwargs.is_empty()
    {
        return None;
    }

    let caller_sys = session
        .solar_system_id
        .or_else(|| ctx.db.with_snapshot(|s| s.characters.get(&char_id).map(|c| c.solar_system_id)))
        .unwrap_or(30_000_142);
    if let Some(b) = bound_entry {
        if (b.object_id as u32) != caller_sys && !session.has_gm_role() {
            return Some(Err(BridgeRpcError {
                code: "AccessDenied".into(),
                message: "Cannot request probe scans for a remote solar system".into(),
            }));
        }
    }
    let sys_id = caller_sys;
    let _ = ensure_system_signatures_and_anomalies(ctx, sys_id).await;
    let now_unix = ctx.now_unix();
    let _ = expire_overdue_probes_for_char(ctx, char_id, now_unix).await;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);

    let override_range_step = args
        .get(1)
        .and_then(|a| a.as_i64())
        .or_else(|| kwargs.get("rangeStep").and_then(|v| v.as_i64()))
        .map(|v| (v as i32).clamp(1, 8));

    let mut invalid_coords = false;
    let mut moved_balls: Vec<(i64, DVec3)> = Vec::new();
    if let Ok(mut probe_map) = ctx.active_scanner_probes.write() {
        if let Some(PyRep::Dict(entries)) = args.first() {
            for (k, v) in entries {
                let Some(pid) = k.as_i64() else { continue };
                let Some(probe) = probe_map.get_mut(&pid) else { continue };
                if probe.owner_char_id != char_id || probe.state <= 0 {
                    continue;
                }
                let base_au = probe_base_scan_range_au(&ctx.sde, probe.probe_type_id);
                let find_in_v = |key: &str| -> Option<&PyRep> {
                    match v {
                        PyRep::KeyVal(kv) => kv.iter().find(|(ek, _)| *ek == key).map(|(_, ev)| ev),
                        PyRep::Dict(kv) => kv.iter().find(|(ek, _)| ek.as_str() == Some(key)).map(|(_, ev)| ev),
                        _ => None,
                    }
                };
                if let Some(PyRep::Tuple(c) | PyRep::List(c)) = find_in_v("pos").or_else(|| find_in_v("destination")) {
                    if c.len() >= 3 {
                        let (x, y, z) = (
                            c[0].as_f64().unwrap_or(probe.position[0]),
                            c[1].as_f64().unwrap_or(probe.position[1]),
                            c[2].as_f64().unwrap_or(probe.position[2]),
                        );
                        if !x.is_finite() || !y.is_finite() || !z.is_finite() {
                            invalid_coords = true;
                            break;
                        }
                        probe.position = [x, y, z];
                        probe.destination = probe.position;
                        moved_balls.push((pid, DVec3::from_array(probe.position)));
                    }
                } else if let (Some(x), Some(y), Some(z)) = (
                    find_in_v("x").and_then(|a| a.as_f64()),
                    find_in_v("y").and_then(|a| a.as_f64()),
                    find_in_v("z").and_then(|a| a.as_f64()),
                ) {
                    if !x.is_finite() || !y.is_finite() || !z.is_finite() {
                        invalid_coords = true;
                        break;
                    }
                    probe.position = [x, y, z];
                    probe.destination = probe.position;
                    moved_balls.push((pid, DVec3::from_array(probe.position)));
                }
                if let Some(step) = find_in_v("rangeStep").and_then(|a| a.as_i64()) {
                    probe.range_step = (step as i32).clamp(1, 8);
                    probe.scan_range_m = base_au * AU_IN_METERS * 2.0_f64.powi(probe.range_step - 1);
                } else if let Some(rad_au) = find_in_v("scanRangeAU").and_then(|a| a.as_f64()) {
                    if !rad_au.is_finite() || rad_au <= 0.0 {
                        invalid_coords = true;
                        break;
                    }
                    let ratio = (rad_au / base_au).max(1.0);
                    probe.range_step = (ratio.log2().round() as i32 + 1).clamp(1, 8);
                    probe.scan_range_m = base_au * AU_IN_METERS * 2.0_f64.powi(probe.range_step - 1);
                } else if let Some(rad_m) = find_in_v("scanRange").and_then(|a| a.as_f64()) {
                    if !rad_m.is_finite() || rad_m <= 0.0 {
                        invalid_coords = true;
                        break;
                    }
                    let ratio = (rad_m / (base_au * AU_IN_METERS)).max(1.0);
                    probe.range_step = (ratio.log2().round() as i32 + 1).clamp(1, 8);
                    probe.scan_range_m = base_au * AU_IN_METERS * 2.0_f64.powi(probe.range_step - 1);
                }
            }
        } else if let Some(PyRep::List(list)) = args.first() {
            let mut owner_probe_ids: Vec<i64> = probe_map
                .values()
                .filter(|p| p.owner_char_id == char_id && p.solar_system_id == sys_id && p.state > 0)
                .map(|p| p.probe_id)
                .collect();
            owner_probe_ids.sort_unstable();
            for (idx, item) in list.iter().enumerate() {
                let Some(&pid) = owner_probe_ids.get(idx) else { break };
                let Some(probe) = probe_map.get_mut(&pid) else { continue };
                let base_au = probe_base_scan_range_au(&ctx.sde, probe.probe_type_id);
                if let PyRep::List(coords) | PyRep::Tuple(coords) = item {
                    if coords.len() >= 3 {
                        let (x, y, z) = (
                            coords[0].as_f64().unwrap_or(0.0),
                            coords[1].as_f64().unwrap_or(0.0),
                            coords[2].as_f64().unwrap_or(0.0),
                        );
                        if !x.is_finite() || !y.is_finite() || !z.is_finite() {
                            invalid_coords = true;
                            break;
                        }
                        probe.position = [x, y, z];
                        probe.destination = probe.position;
                        moved_balls.push((pid, DVec3::from_array(probe.position)));
                    }
                    if let Some(step) = coords.get(3).and_then(|a| a.as_i64()).map(|s| (s as i32).clamp(1, 8)).or(override_range_step) {
                        probe.range_step = step;
                        probe.scan_range_m = base_au * AU_IN_METERS * 2.0_f64.powi(step - 1);
                    }
                }
            }
        } else if let Some(step) = override_range_step {
            for p in probe_map.values_mut() {
                if p.owner_char_id == char_id && p.solar_system_id == sys_id && p.state > 0 {
                    let base_au = probe_base_scan_range_au(&ctx.sde, p.probe_type_id);
                    p.range_step = step;
                    p.scan_range_m = base_au * AU_IN_METERS * 2.0_f64.powi(step - 1);
                }
            }
        }
    }
    if invalid_coords {
        return Some(Err(BridgeRpcError::user_error("InvalidProbeCoordinates")));
    }

    for (pid, new_pos) in moved_balls {
        if let Ok(Some((_, mut b))) = sys_handle.get_ball_snapshot(pid).await {
            b.position = new_pos;
            let _ = sys_handle.spawn_ball(b).await;
        }
    }

    let active_probes: Vec<crate::router::ActiveScannerProbe> = ctx
        .active_scanner_probes
        .read()
        .ok()
        .map(|map| {
            map.values()
                .filter(|p| p.owner_char_id == char_id && p.solar_system_id == sys_id && p.state > 0)
                .cloned()
                .collect()
        })
        .unwrap_or_default();

    if active_probes.is_empty() {
        return Some(Err(BridgeRpcError {
            code: "NoActiveProbes".into(),
            message: "No active scanner probes in space".into(),
        }));
    }

    let (astro_lvl, rangefinding_lvl, pinpointing_lvl, fitted_scan_bonus_pct) = ctx.db.with_snapshot(|snap| {
        let sk = snap.skills.get(&char_id);
        let a = sk.and_then(|m| m.get(&3412)).map(|s| s.level).unwrap_or(0);
        let r = sk.and_then(|m| m.get(&25810)).map(|s| s.level).unwrap_or(0);
        let p = sk.and_then(|m| m.get(&25811)).map(|s| s.level).unwrap_or(0);
        let ship_id = active_probes.first().map(|pr| pr.ship_id).or(session.ship_id).unwrap_or(0);
        let bonus_pct: f64 = snap
            .items
            .values()
            .filter(|i| {
                i.location_id == ship_id
                    && matches!(i.flag as u16, 11..=34 | 92..=94 | 125..=132)
                    && snap.module_online_states.get(&i.item_id).copied().unwrap_or(true)
            })
            .filter_map(|i| ctx.sde.types.get(&i.type_id))
            .filter_map(|t| t.default_attributes.get(&1136).copied())
            .sum();
        (a, r, p, bonus_pct)
    });
    let skill_mult = (1.0 + 0.05 * astro_lvl as f64)
        * (1.0 + 0.05 * rangefinding_lvl as f64)
        * (1.0 + fitted_scan_bonus_pct / 100.0);

    let first_probe = &active_probes[0];
    let lead_base_range_au = probe_base_scan_range_au(&ctx.sde, first_probe.probe_type_id);
    let lead_base_dev_au = probe_base_max_deviation_au(&ctx.sde, first_probe.probe_type_id);
    let lead_range_au = lead_base_range_au * 2.0_f64.powi(first_probe.range_step - 1);
    let lead_max_dev_au = compute_probe_max_deviation_au(lead_base_dev_au, lead_range_au, lead_base_range_au, astro_lvl, pinpointing_lvl);
    let lead_max_dev_m = lead_max_dev_au * AU_IN_METERS;

    let scan_results = ctx.db.with_snapshot(|snap| {
        let mut out = Vec::new();
        if let Some(sigs) = snap.cosmic_signatures.get(&sys_id) {
            for sig in sigs {
                if sig.collapsed {
                    continue;
                }
                let (diff_level, req_min_strength) = signature_difficulty_level_and_min_strength(&ctx.sde, sys_id, sig);
                let sig_pos = DVec3::from_array(sig.position);
                let mut covering_probes = 0usize;
                let mut sum_probe_strength = 0.0;
                let mut sum_covering_dev_au = 0.0;
                let mut sum_covering_range_au = 0.0;

                for probe in &active_probes {
                    let p_pos = DVec3::from_array(probe.position);
                    let dist = p_pos.distance(sig_pos);
                    let base_au = probe_base_scan_range_au(&ctx.sde, probe.probe_type_id);
                    let radius = probe.scan_range_m.max(base_au * AU_IN_METERS);
                    if dist <= radius {
                        covering_probes += 1;
                        let step_factor = 0.5_f64.powi(probe.range_step - 1);
                        let inner_r = 0.5 * radius;
                        let dist_factor = if dist <= inner_r {
                            1.0
                        } else {
                            (-((dist - inner_r) / inner_r).powi(2)).exp()
                        };
                        let sde_base_sensor = probe_base_sensor_strength(&ctx.sde, probe.probe_type_id);
                        let eff_probe_sensor = sde_base_sensor * skill_mult * step_factor * dist_factor;
                        sum_probe_strength += eff_probe_sensor;
                        let p_base_dev = probe_base_max_deviation_au(&ctx.sde, probe.probe_type_id);
                        let p_range_au = base_au * 2.0_f64.powi(probe.range_step - 1);
                        sum_covering_dev_au += compute_probe_max_deviation_au(p_base_dev, p_range_au, base_au, astro_lvl, pinpointing_lvl);
                        sum_covering_range_au += p_range_au;
                    }
                }

                // DERIVED: 8-probe formation normalization factor (0.20 per covering probe; 5 covering probes = 1.0x single-probe strength, 8 probes = 1.6x).
                let formation_probe_strength = sum_probe_strength * 0.20;
                let (sig_max_dev_au, sig_range_au) = if covering_probes > 0 {
                    (sum_covering_dev_au / covering_probes as f64, sum_covering_range_au / covering_probes as f64)
                } else {
                    (lead_max_dev_au, lead_range_au)
                };
                let trilateration_cap: f64 = match covering_probes {
                    0 => 0.0,
                    1 => 0.24,
                    2 => 0.49,
                    3 => 0.74,
                    _ => 1.0,
                };
                let strength_cap = if formation_probe_strength + 1e-6 < req_min_strength {
                    (formation_probe_strength / req_min_strength).min(0.99)
                } else {
                    1.0
                };
                let effective_cap = trilateration_cap.min(strength_cap);
                let raw_strength = (formation_probe_strength / req_min_strength).min(effective_cap);
                let mut scanned_sig = sig.clone();
                scanned_sig.signal_strength = raw_strength.clamp(0.0, effective_cap);
                let resolved = scanned_sig.signal_strength >= 1.0;
                let wh_item_id = wormhole_item_id_for_signature(sys_id, &scanned_sig.signature_id);
                out.push((scanned_sig, wh_item_id, covering_probes, resolved, diff_level, req_min_strength, formation_probe_strength, sig_max_dev_au, sig_range_au));
            }
        }
        out
    });

    let mut rows = Vec::new();
    for (sig, wh_item_id, covering_probes, resolved, diff_level, req_min_strength, formation_strength, sig_max_dev_au, sig_range_au) in scan_results {
        ctx.record_character_scan_progress(char_id, sys_id, &sig.signature_id, sig.signal_strength);
        if resolved {
            ctx.mark_signature_scanned(char_id, sys_id, wh_item_id);
        }
        let sig_max_dev_m = sig_max_dev_au * AU_IN_METERS;
        let show_group = sig.signal_strength >= 0.25;
        let show_name = sig.signal_strength >= 0.75;
        let mut kv = vec![
            ("id", PyRep::String(sig.signature_id.clone())),
            ("signatureID", PyRep::String(sig.signature_id.clone())),
            ("itemID", PyRep::Integer(wh_item_id)),
            ("targetID", PyRep::Integer(wh_item_id)),
            ("targetClass", PyRep::String("CosmicSignature".into())),
            ("group", PyRep::String(if show_group { sig.sig_group.clone() } else { "Unknown".into() })),
            ("groupName", if show_group { PyRep::String(sig.sig_group.clone()) } else { PyRep::None }),
            ("type", PyRep::String(if show_name { sig.type_name.clone() } else { "Unidentified Signature".into() })),
            ("siteName", if show_name { PyRep::String(sig.type_name.clone()) } else { PyRep::None }),
            ("signalStrength", PyRep::Real(sig.signal_strength)),
            ("certainty", PyRep::Real(sig.signal_strength)),
            ("warpable", PyRep::Bool(resolved)),
            ("deviation", PyRep::Real(if resolved { 0.0 } else { (1.0 - sig.signal_strength) * sig_max_dev_m })),
            ("maxDeviation", PyRep::Real(sig_max_dev_m)),
            ("maxDeviationAU", PyRep::Real(sig_max_dev_au)),
            ("scanRangeAU", PyRep::Real(sig_range_au)),
            ("baseScanRangeAU", PyRep::Real(lead_base_range_au)),
            ("baseDeviationAU", PyRep::Real(lead_base_dev_au)),
            ("coveringProbes", PyRep::Integer(covering_probes as i64)),
            ("resolved", PyRep::Bool(resolved)),
            ("x", if resolved { PyRep::Real(sig.position[0]) } else { PyRep::None }),
            ("y", if resolved { PyRep::Real(sig.position[1]) } else { PyRep::None }),
            ("z", if resolved { PyRep::Real(sig.position[2]) } else { PyRep::None }),
        ];
        if show_name {
            kv.extend([
                ("difficultyLevel", PyRep::Integer(diff_level as i64)),
                ("requiredProbeStrength", PyRep::Real(req_min_strength)),
                ("formationProbeStrength", PyRep::Real(formation_strength)),
            ]);
        }
        if resolved {
            let (mass_stage, wh_size, life_stage, wh_age, is_collapsed) = compute_wormhole_stages(&sig, now_unix);
            let buckets = derive_wormhole_buckets(&sig, now_unix, &ctx.sde);
            kv.extend([
                ("typeID", sig.wormhole_type_id.map(|t| PyRep::Integer(t as i64)).unwrap_or(PyRep::None)),
                ("massStage", PyRep::String(mass_stage.into())),
                ("wormholeSize", PyRep::Real(wh_size)),
                ("lifetimeStage", PyRep::String(life_stage.into())),
                ("wormholeAge", PyRep::Integer(wh_age)),
                ("lifetimeDescription", PyRep::String(buckets.lifetime_description.into())),
                ("massStabilityDescription", PyRep::String(buckets.mass_stability_description.into())),
                ("collapsed", PyRep::Bool(is_collapsed)),
            ]);
        }
        rows.push(PyRep::keyval(kv));
    }

    // parity: feat:scanning.probe-target-classes
    let combat_probes: Vec<&crate::router::ActiveScannerProbe> = active_probes
        .iter()
        .filter(|p| is_combat_scanner_probe(&ctx.sde, p.probe_type_id))
        .collect();
    if !combat_probes.is_empty() {
        let caller_ship = session.ship_id;
        let balls = sys_handle.list_balls().await.unwrap_or_default();
        for ball in balls {
            if Some(ball.id) == caller_ship {
                continue;
            }
            let Some(type_rec) = ctx.sde.types.get(&ball.type_id) else { continue };
            let group_id = type_rec.group_id;
            let category_id = type_rec.category_id;
            if group_id == 479
                || group_id == 186
                || matches!(group_id, 12 | 306 | 340 | 448 | 649)
                || type_rec.name.contains("Container")
                || type_rec.name.contains("Wreck")
            {
                continue;
            }
            if let Ok(Some(cs)) = sys_handle.get_combat_state(ball.id).await {
                if cs.is_cloaked {
                    continue;
                }
            }
            let target_class = if category_id == 6 {
                "Ship"
            } else if matches!(category_id, 18 | 87) {
                "Drone"
            } else if matches!(category_id, 65 | 23 | 22) || ctx.db.with_snapshot(|s| s.structures.contains_key(&ball.id)) {
                "Structure"
            } else {
                continue;
            };

            let mut covering = 0usize;
            let mut total_s = 0.0;
            for probe in &combat_probes {
                let p_pos = DVec3::from_array(probe.position);
                let dist = p_pos.distance(ball.position);
                let base_au = probe_base_scan_range_au(&ctx.sde, probe.probe_type_id);
                let radius = probe.scan_range_m.max(base_au * AU_IN_METERS);
                if dist <= radius {
                    covering += 1;
                    let step_factor = 0.5_f64.powi(probe.range_step - 1);
                    let dist_factor = (-(dist / radius).powi(2)).exp();
                    total_s += probe.base_sensor_strength * skill_mult * step_factor * dist_factor * 2.0;
                }
            }
            if covering == 0 {
                continue;
            }
            let cap = match covering {
                1 => 0.24,
                2 => 0.49,
                3 => 0.74,
                _ => 1.0,
            };
            let sig_str = (total_s / 25.0).clamp(0.0, cap);
            let resolved = sig_str >= 1.0;
            let show_group = sig_str >= 0.25;
            let show_name = sig_str >= 0.75;
            let cmb_id = format!("CMB-{}", ball.id);
            let mut cmb_kv = vec![
                ("id", PyRep::String(cmb_id.clone())),
                ("signatureID", PyRep::String(cmb_id)),
                ("itemID", PyRep::Integer(ball.id)),
                ("targetID", PyRep::Integer(ball.id)),
                ("targetClass", PyRep::String(if show_group { target_class.into() } else { "Unknown".into() })),
                ("group", PyRep::String(if show_group { target_class.into() } else { "Unknown".into() })),
                ("type", PyRep::String(if show_name { type_rec.name.clone() } else { "Unidentified Target".into() })),
                ("signalStrength", PyRep::Real(sig_str)),
                ("certainty", PyRep::Real(sig_str)),
                ("warpable", PyRep::Bool(resolved)),
                ("deviation", PyRep::Real(if resolved { 0.0 } else { (1.0 - sig_str) * lead_max_dev_m })),
                ("maxDeviation", PyRep::Real(lead_max_dev_m)),
                ("maxDeviationAU", PyRep::Real(lead_max_dev_au)),
                ("coveringProbes", PyRep::Integer(covering as i64)),
                ("resolved", PyRep::Bool(resolved)),
                ("x", if resolved { PyRep::Real(ball.position.x) } else { PyRep::None }),
                ("y", if resolved { PyRep::Real(ball.position.y) } else { PyRep::None }),
                ("z", if resolved { PyRep::Real(ball.position.z) } else { PyRep::None }),
            ];
            if show_group {
                cmb_kv.extend([
                    ("groupID", PyRep::Integer(group_id as i64)),
                    ("categoryID", PyRep::Integer(category_id as i64)),
                ]);
            }
            if show_name {
                cmb_kv.push(("typeID", PyRep::Integer(ball.type_id as i64)));
            }
            rows.push(PyRep::keyval(cmb_kv));
        }
    }

    let _ = ctx.notify(
        crate::router::NotificationTarget::Character(char_id),
        "OnSystemScanStarted",
        "charid",
        vec![PyRep::Integer(now_unix as i64), PyRep::Integer(4000), PyRep::Integer(active_probes.len() as i64)],
    );
    let _ = ctx.notify(
        crate::router::NotificationTarget::Character(char_id),
        "OnSystemScanStopped",
        "charid",
        vec![PyRep::Integer(active_probes.len() as i64), PyRep::List(rows.clone()), PyRep::List(Vec::new())],
    );

    Some(Ok(PyRep::List(rows)))
}

async fn handle_recover_probes(
    ctx: &ServerContext,
    session: &mut SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (_, char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let sys_id = bound_entry.map(|b| b.object_id as u32).or(session.solar_system_id).unwrap_or(30_000_142);

    let mut target_probe_ids: Vec<i64> = Vec::new();
    if let Some(PyRep::List(list) | PyRep::Tuple(list)) = args.first() {
        for item in list {
            if let Some(pid) = item.as_i64() {
                target_probe_ids.push(pid);
            }
        }
    } else if let Some(pid) = args.first().and_then(|a| a.as_i64()) {
        target_probe_ids.push(pid);
    }

    let now_unix = ctx.now_unix();
    let (auto_returned, expired_lost) = expire_overdue_probes_for_char(ctx, char_id, now_unix).await;

    let candidate_probes: Vec<crate::router::ActiveScannerProbe> =
        if let Ok(mut map) = ctx.active_scanner_probes.write() {
            let ids: Vec<i64> = map
                .values()
                .filter(|p| {
                    p.owner_char_id == char_id
                        && p.solar_system_id == sys_id
                        && p.state > 0
                        && (target_probe_ids.is_empty() || target_probe_ids.contains(&p.probe_id))
                })
                .map(|p| p.probe_id)
                .collect();
            ids.into_iter().filter_map(|id| map.remove(&id)).collect()
        } else {
            Vec::new()
        };

    let ship_id_opt = session.ship_id.or_else(|| candidate_probes.first().map(|p| p.ship_id));
    let ship_cargo_cap = ship_id_opt
        .and_then(|sid| {
            ctx.get_or_rebuild_ship_fitting(char_id, sid)
                .ok()
                .map(|fs| fs.ship.get_attribute(eve_sde::attr_ids::CAPACITY, &ctx.sde))
                .filter(|&c| c > 0.0)
                .or_else(|| {
                    ctx.db.with_snapshot(|s| {
                        s.items.get(&sid).and_then(|it| ctx.sde.types.get(&it.type_id).map(|t| t.capacity))
                    })
                })
        })
        .unwrap_or(400.0);

    let (recovered_probes, unrecovered_probes, to_launcher, to_cargo) = if !candidate_probes.is_empty() {
        ctx.db.with_mut_snapshot(|snap| {
            let mut in_launcher = 0i32;
            let mut in_cargo = 0i32;
            let mut rec = Vec::new();
            let mut unrec = Vec::new();
            let add_stack = |items: &mut HashMap<i64, eve_db::InventoryItemRecord>,
                             next_id: &mut i64,
                             ship_id: i64,
                             flag: InventoryFlag,
                             p_type_id: u32,
                             qty: i32,
                             exclude_id: i64| {
                if let Some(st) = items.values_mut().find(|i| {
                    i.owner_id == char_id
                        && i.location_id == ship_id
                        && i.flag == flag
                        && i.item_id != exclude_id
                        && i.type_id == p_type_id
                }) {
                    st.quantity += qty;
                    st.version += 1;
                } else {
                    *next_id += 1;
                    let new_id = *next_id;
                    items.insert(
                        new_id,
                        eve_db::InventoryItemRecord {
                            item_id: new_id,
                            type_id: p_type_id,
                            owner_id: char_id,
                            location_id: ship_id,
                            flag,
                            quantity: qty,
                            singleton: false,
                            version: 1,
                            ..Default::default()
                        },
                    );
                }
            };
            let mut used_cargo_vol: f64 = if let Some(ship_id) = ship_id_opt {
                snap.items
                    .values()
                    .filter(|i| i.location_id == ship_id && i.flag == InventoryFlag::Cargo)
                    .map(|i| {
                        let uv = ctx.sde.types.get(&i.type_id).map(|t| t.volume).unwrap_or(0.1);
                        uv * (i.quantity.max(1) as f64)
                    })
                    .sum()
            } else {
                0.0
            };
            for p in candidate_probes {
                let p_type_id = p.probe_type_id;
                let ship_id = ship_id_opt.unwrap_or(p.ship_id);
                let launcher_id = p.launcher_module_id;
                let mut placed_in_launcher = false;
                if let Some(li) = snap.items.get(&launcher_id).cloned().filter(|i| i.location_id == ship_id) {
                    let cur_clip: i32 = snap
                        .items
                        .values()
                        .filter(|i| {
                            i.owner_id == char_id
                                && i.location_id == ship_id
                                && i.flag == li.flag
                                && i.item_id != launcher_id
                        })
                        .map(|i| i.quantity)
                        .sum();
                    let matching_clip = snap.items.values().any(|i| {
                        i.owner_id == char_id
                            && i.location_id == ship_id
                            && i.flag == li.flag
                            && i.item_id != launcher_id
                            && i.type_id == p_type_id
                    });
                    if cur_clip == 0 || (cur_clip < 8 && matching_clip) {
                        add_stack(&mut snap.items, &mut snap.next_entity_id, ship_id, li.flag, p_type_id, 1, launcher_id);
                        in_launcher += 1;
                        placed_in_launcher = true;
                    }
                }
                if placed_in_launcher {
                    rec.push(p);
                } else {
                    let probe_vol = ctx.sde.types.get(&p_type_id).map(|t| t.volume).filter(|&v| v > 0.0).unwrap_or(0.1);
                    if ship_cargo_cap <= 0.0 || used_cargo_vol + probe_vol <= ship_cargo_cap + 1e-6 {
                        add_stack(&mut snap.items, &mut snap.next_entity_id, ship_id, InventoryFlag::Cargo, p_type_id, 1, 0);
                        used_cargo_vol += probe_vol;
                        in_cargo += 1;
                        rec.push(p);
                    } else {
                        unrec.push(p);
                    }
                }
            }
            (rec, unrec, in_launcher, in_cargo)
        })
    } else {
        (Vec::new(), Vec::new(), 0, 0)
    };

    if !unrecovered_probes.is_empty() {
        if let Ok(mut map) = ctx.active_scanner_probes.write() {
            for p in unrecovered_probes {
                map.insert(p.probe_id, p);
            }
        }
    }
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    for p in &recovered_probes {
        let _ = sys_handle.remove_ball(p.probe_id).await;
    }
    let count = recovered_probes.len() as i32;

    let rec_ids: Vec<PyRep> = recovered_probes.iter().map(|p| PyRep::Integer(p.probe_id)).collect();
    if count > 0 {
        let _ = ctx.notify(
            crate::router::NotificationTarget::Character(char_id),
            "OnProbesIdle",
            "charid",
            vec![PyRep::List(rec_ids.clone())],
        );
    }
    Some(Ok(PyRep::keyval([
        ("recoveredProbeIDs", PyRep::List(rec_ids)),
        ("recoveredCount", PyRep::Integer(count as i64)),
        ("returnedToLauncher", PyRep::Integer(to_launcher as i64)),
        ("returnedToCargo", PyRep::Integer(to_cargo as i64)),
        ("expiredReturnedToCargoProbeIDs", PyRep::List(auto_returned.into_iter().map(PyRep::Integer).collect())),
        ("expiredLostProbeIDs", PyRep::List(expired_lost.into_iter().map(PyRep::Integer).collect())),
    ])))
}

async fn handle_reconnect_probes(
    ctx: &ServerContext,
    session: &mut SessionState,
    bound_entry: Option<&BoundObjectEntry>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (_, char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let sys_id = bound_entry.map(|b| b.object_id as u32).or(session.solar_system_id).unwrap_or(30_000_142);
    let now_unix = ctx.now_unix();
    let (auto_returned, expired_lost) = expire_overdue_probes_for_char(ctx, char_id, now_unix).await;
    let ship_id = session.ship_id.unwrap_or(0);

    let mut reconnected = Vec::new();
    if let Ok(mut map) = ctx.active_scanner_probes.write() {
        for p in map.values_mut() {
            if p.owner_char_id == char_id && p.solar_system_id == sys_id && p.state >= 0 {
                p.state = 1;
                if ship_id != 0 {
                    p.ship_id = ship_id;
                }
                reconnected.push(PyRep::Integer(p.probe_id));
            }
        }
    }
    let count = reconnected.len() as i64;
    Some(Ok(PyRep::keyval([
        ("reconnectedProbeIDs", PyRep::List(reconnected)),
        ("reconnectedCount", PyRep::Integer(count)),
        ("expiredReturnedToCargoProbeIDs", PyRep::List(auto_returned.into_iter().map(PyRep::Integer).collect())),
        ("expiredLostProbeIDs", PyRep::List(expired_lost.into_iter().map(PyRep::Integer).collect())),
    ])))
}

async fn handle_destroy_probe(
    ctx: &ServerContext,
    session: &mut SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (_, char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let sys_id = bound_entry.map(|b| b.object_id as u32).or(session.solar_system_id).unwrap_or(30_000_142);
    let target_pid = args.first().and_then(|a| a.as_i64());

    let destroyed_probes: Vec<crate::router::ActiveScannerProbe> =
        if let Ok(mut map) = ctx.active_scanner_probes.write() {
            let ids: Vec<i64> = map
                .values()
                .filter(|p| {
                    p.owner_char_id == char_id
                        && p.state >= 0
                        && (target_pid.is_none() || target_pid == Some(p.probe_id))
                })
                .map(|p| p.probe_id)
                .collect();
            ids.into_iter().filter_map(|id| map.remove(&id)).collect()
        } else {
            Vec::new()
        };

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    for p in &destroyed_probes {
        let _ = sys_handle.remove_ball(p.probe_id).await;
        let _ = ctx.notify(
            crate::router::NotificationTarget::Character(char_id),
            "OnRemoveProbe",
            "charid",
            vec![PyRep::Integer(p.probe_id)],
        );
    }

    Some(Ok(PyRep::keyval([
        ("destroyedProbeID", destroyed_probes.first().map(|p| PyRep::Integer(p.probe_id)).unwrap_or(PyRep::None)),
        ("destroyedCount", PyRep::Integer(destroyed_probes.len() as i64)),
        ("destroyed", PyRep::Bool(!destroyed_probes.is_empty())),
    ])))
}
