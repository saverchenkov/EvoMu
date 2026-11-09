//! `space_probe_scanning` — Core & Combat scanner probe launch, range/deviation math,
//! target class filtering, signature reveal thresholds, difficulty levels, and flight-time expiry.

#![forbid(unsafe_code)]

use crate::router::{BoundObjectEntry, ServerContext};
use crate::services::space_destiny::{
    compute_wormhole_stages_with_sde, derive_wormhole_buckets, ensure_system_signatures_and_anomalies,
    system_wormhole_profile, wormhole_item_id_for_signature, AU_IN_METERS,
};
use eve_db::types::{CosmicAnomalyRecord, CosmicSignatureRecord, InventoryFlag};
use eve_destiny::Ball;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::{SdeRegistry, SolarSystemId};
use glam::DVec3;
use std::collections::HashMap;

pub const PROBE_STATE_INACTIVE: i32 = 0; pub const PROBE_STATE_IDLE: i32 = 1; pub const PROBE_STATE_MOVING: i32 = 2;
pub const PROBE_STATE_WARPING: i32 = 3; pub const PROBE_STATE_SCANNING: i32 = 4; pub const PROBE_STATE_RETURNING: i32 = 5;

fn resolve_caller_solar_system(ctx: &ServerContext, session: &SessionState, char_id: i64) -> u32 {
    session
        .solar_system_id
        .or_else(|| ctx.db.with_snapshot(|s| s.characters.get(&char_id).map(|c| c.solar_system_id)))
        .unwrap_or(0)
}

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

// parity: feat:scanning.probe-range-deviation
pub use crate::services::r6_t1_p0_helpers::{
    expire_all_overdue_probes, expire_overdue_probes_for_char,
};

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

    let ship_pos = sys.get_ball_snapshot(ship_id).await.ok().flatten().map(|(_, b)| b.position).unwrap_or(DVec3::ZERO);

    static NEXT_PROBE_RES_ID: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(-100_000);
    let requested_count = kwargs.get("probeCount").and_then(|v| v.as_i64()).unwrap_or(loaded_qty as i64) as i32;
    let reservation_ids: Vec<i64> = {
        let mut probes = ctx.active_scanner_probes.write().unwrap_or_else(|e| e.into_inner());
        let existing_active_count = probes
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
        let to_reserve = requested_count.clamp(1, max_can_launch).min(loaded_qty);
        let mut res_ids = Vec::with_capacity(to_reserve as usize);
        for _ in 0..to_reserve {
            let rid = NEXT_PROBE_RES_ID.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
            probes.insert(
                rid,
                crate::router::ActiveScannerProbe {
                    probe_id: rid,
                    probe_type_id,
                    owner_char_id: char_id,
                    ship_id,
                    launcher_module_id: module_id,
                    solar_system_id: sys_id,
                    position: [0.0; 3],
                    destination: [0.0; 3],
                    range_step: 4,
                    scan_range_m: 0.0,
                    base_sensor_strength: 0.0,
                    max_flight_time_s: 0.0,
                    launched_at_unix: 0,
                    state: PROBE_STATE_IDLE,
                    warp_end_ft: None,
                },
            );
            res_ids.push(rid);
        }
        res_ids
    };

    let db_res = ctx.db.try_with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get(&char_id) {
            if ch.station_id.is_some() || ch.structure_id.is_some() || ch.solar_system_id != sys_id || ch.active_ship_id != ship_id {
                return Err(BridgeRpcError {
                    code: "ShipNotInSpace".into(),
                    message: format!("Active ship {ship_id} is not in space in solar system {sys_id}"),
                });
            }
        }
        let ci = snap
            .items
            .get(&charge_item_id)
            .filter(|ci| ci.location_id == ship_id && (ci.flag as u16) == module_flag_id && ci.quantity > 0)
            .cloned()
            .ok_or_else(|| BridgeRpcError {
                code: "NoProbesLoaded".into(),
                message: "Scan probe launcher has no scanner probes loaded".into(),
            })?;
        let actual_launch = (reservation_ids.len() as i32).min(ci.quantity);
        if actual_launch <= 0 {
            return Err(BridgeRpcError {
                code: "NoProbesLoaded".into(),
                message: "Scan probe launcher has no scanner probes loaded".into(),
            });
        }
        let mut opts = eve_db::ConsumeExactOptions::for_owner_exact(Some(char_id), ci.owner_id);
        opts.reject_fitted = false;
        opts.active_ship_id = Some(ship_id);
        let rem = match snap.consume_exact_item_quantity(charge_item_id, actual_launch, opts) {
            Ok(Some(it)) => it.quantity,
            Ok(None) => 0,
            Err(e) => {
                return Err(BridgeRpcError {
                    code: "ItemLocked".into(),
                    message: e.to_string(),
                });
            }
        };
        let mut ids = Vec::with_capacity(actual_launch as usize);
        for _ in 0..actual_launch {
            snap.next_entity_id += 1;
            ids.push(snap.next_entity_id);
        }
        Ok((ids, rem, actual_launch))
    });
    let (allocated_probe_ids, remaining_in_launcher, to_launch) = match db_res {
        Ok(v) => v,
        Err(e) => {
            let mut probes = ctx.active_scanner_probes.write().unwrap_or_else(|e| e.into_inner());
            for rid in &reservation_ids {
                probes.remove(rid);
            }
            return Err(e);
        }
    };
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

    let mut spawned_probe_positions: Vec<(i64, DVec3)> = Vec::with_capacity(allocated_probe_ids.len());
    {
        let mut probes = ctx.active_scanner_probes.write().unwrap_or_else(|e| e.into_inner());
        for rid in &reservation_ids {
            probes.remove(rid);
        }
        for (idx, &probe_id) in allocated_probe_ids.iter().enumerate() {
            let off = formation_offsets[idx % formation_offsets.len()];
            let pos = DVec3::new(ship_pos.x + off[0], ship_pos.y + off[1], ship_pos.z + off[2]);
            spawned_probe_positions.push((probe_id, pos));
            probes.insert(
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
                    state: PROBE_STATE_IDLE,
                    warp_end_ft: None,
                },
            );
        }
    }
    sync_scanner_probes_to_db(ctx);

    for &(probe_id, pos) in &spawned_probe_positions {
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
                    is_cloaked: false, ..Default::default()
                },
            )
            .await;
    }

    let _ = ctx.notify_module_effect_started(char_id, ship_id, module_id, module_type_id, Some(effect_name), target_id_opt);
    let expiry_ft = ctx.now_blueos_filetime_ticks().saturating_add((max_flight_time_s.round() as i64).saturating_mul(10_000_000));
    for &(pid, ppos) in &spawned_probe_positions {
        let pt = PyRep::Tuple(vec![PyRep::Real(ppos.x), PyRep::Real(ppos.y), PyRep::Real(ppos.z)]);
        let probe_kv = PyRep::keyval([
            ("probeID", PyRep::Integer(pid)), ("typeID", PyRep::Integer(probe_type_id as i64)),
            ("scanRange", PyRep::Real(scan_range_m)), ("expiry", PyRep::Integer(expiry_ft)),
            ("pos", pt.clone()), ("destination", pt), ("state", PyRep::Integer(PROBE_STATE_IDLE as i64)),
        ]);
        let _ = ctx.notify(crate::router::NotificationTarget::Character(char_id), "OnNewProbe", "charid", vec![probe_kv]);
        let _ = ctx.notify(crate::router::NotificationTarget::Character(char_id), "OnProbeStateChanged", "charid", vec![PyRep::Integer(pid), PyRep::Integer(PROBE_STATE_IDLE as i64)]);
    }
    ctx.persist_server_runtime_to_db();
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
    let mut mutated = false;
    let res = match (service, method) {
        ("scanMgr", "GetSystemScanMgr") => handle_get_system_scan_mgr(ctx, session, service, args).await,
        ("scanMgr", "GetAnomalies") => handle_get_anomalies(ctx, session, bound_entry).await,
        ("scanMgr", "RequestScans") => {
            mutated = true;
            handle_request_scans(ctx, session, bound_entry, args, kwargs).await
        }
        ("scanMgr", "RecoverProbes") => {
            mutated = true;
            handle_recover_probes(ctx, session, bound_entry, args).await
        }
        ("scanMgr", "ReconnectToLostProbes") => {
            mutated = true;
            handle_reconnect_probes(ctx, session, bound_entry).await
        }
        ("scanMgr", "DestroyProbe") => {
            mutated = true;
            handle_destroy_probe(ctx, session, bound_entry, args).await
        }
        ("scanMgr", "GetFullState") => handle_get_full_state(ctx, session, bound_entry).await,
        ("scanMgr", "QAOverrideProbeExpiry") => {
            mutated = true;
            handle_qa_override_probe_expiry(ctx, session, args)
        }
        ("scanMgr", "QAScanSites") => {
            mutated = true;
            handle_qa_scan_sites(ctx, session, bound_entry).await
        }
        ("scanMgr", "SetActivityState") => {
            mutated = true;
            handle_set_activity_state(ctx, session, bound_entry, args)
        }
        _ => None,
    };
    if mutated && matches!(res, Some(Ok(_))) {
        ctx.persist_server_runtime_to_db();
    }
    res
}

async fn handle_get_full_state(
    ctx: &ServerContext,
    session: &SessionState,
    bound_entry: Option<&BoundObjectEntry>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (_, char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let caller_sys = resolve_caller_solar_system(ctx, session, char_id);
    let is_gm = session.has_gm_role();
    if let Some(b) = bound_entry {
        let req_sys = b.object_id as u32;
        if req_sys > 0 && req_sys != caller_sys && !is_gm {
            return Some(Err(BridgeRpcError {
                code: "CrossSystemScanDenied".into(),
                message: format!("Cannot query GetFullState for remote solar system {req_sys}"),
            }));
        }
    }
    let sys_id = if is_gm {
        bound_entry
            .map(|b| b.object_id as u32)
            .filter(|&id| id > 0)
            .unwrap_or(caller_sys)
    } else {
        caller_sys
    };
    let (sigs, anomalies) = ensure_system_signatures_and_anomalies(ctx, sys_id).await;
    let anom_entries: Vec<(PyRep, PyRep)> = anomalies
        .into_iter()
        .map(|a| {
            let kv = crate::services::fleet_social::fleet_social_scanning::build_signal_tracker_anomaly_kv(&ctx.sde, &a);
            (PyRep::String(a.anomaly_id), kv)
        })
        .collect();
    let sig_entries: Vec<(PyRep, PyRep)> = sigs
        .into_iter()
        .map(|mut s| {
            let eff_strength = if is_gm {
                1.0
            } else {
                ctx.effective_signature_signal_strength(char_id, sys_id, &s)
            };
            s.signal_strength = eff_strength;
            let kv = crate::services::fleet_social::fleet_social_scanning::build_signal_tracker_signature_kv(&ctx.sde, &s);
            (PyRep::String(s.signature_id), kv)
        })
        .collect();
    Some(Ok(PyRep::Tuple(vec![
        PyRep::Dict(anom_entries),
        PyRep::Dict(sig_entries),
        PyRep::Dict(Vec::new()),
        PyRep::Dict(Vec::new()),
    ])))
}

fn handle_qa_override_probe_expiry(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (_, char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    if !session.has_gm_role() {
        return Some(Err(BridgeRpcError {
            code: "GMRoleRequired".into(),
            message: "scanMgr.QAOverrideProbeExpiry requires GM/QA role".into(),
        }));
    }
    let secs = args.first().and_then(|a| a.as_f64()).unwrap_or(60.0).max(1.0);
    if let Ok(mut map) = ctx.active_scanner_probes.write() {
        for p in map.values_mut() {
            if p.owner_char_id == char_id {
                p.max_flight_time_s = secs;
            }
        }
    }
    sync_scanner_probes_to_db(ctx);
    Some(Ok(PyRep::None))
}

async fn handle_qa_scan_sites(
    ctx: &ServerContext,
    session: &SessionState,
    bound_entry: Option<&BoundObjectEntry>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let _ = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    if !session.has_gm_role() {
        return Some(Err(BridgeRpcError {
            code: "GMRoleRequired".into(),
            message: "scanMgr.QAScanSites requires GM/QA role".into(),
        }));
    }
    let Some(sys_id) = bound_entry
        .map(|b| b.object_id as u32)
        .filter(|&v| v > 0)
        .or(session.solar_system_id)
        .or_else(|| {
            session.character_id.and_then(|cid| {
                ctx.db.with_snapshot(|s| s.characters.get(&cid).map(|c| c.solar_system_id))
            })
        })
    else {
        return Some(Err(BridgeRpcError {
            code: "InvalidSolarSystem".into(),
            message: "scanMgr.QAScanSites requires an active solar system".into(),
        }));
    };
    let (sigs, anomalies) = ensure_system_signatures_and_anomalies(ctx, sys_id).await;
    let now_unix = ctx.now_unix();
    let mut results = format_anomalies(anomalies, now_unix);
    for s in sigs {
        results.push(PyRep::keyval([
            ("id", PyRep::String(s.signature_id.clone())),
            ("signatureID", PyRep::String(s.signature_id)),
            ("signalStrength", PyRep::Real(1.0)),
            ("resolved", PyRep::Bool(true)),
        ]));
    }
    if results.is_empty() {
        results.push(PyRep::keyval([
            ("id", PyRep::String("ABC-123".into())),
            ("signatureID", PyRep::String("ABC-123".into())),
            ("signalStrength", PyRep::Real(1.0)),
            ("resolved", PyRep::Bool(true)),
        ]));
    }
    Some(Ok(PyRep::List(results)))
}

fn handle_set_activity_state(
    ctx: &ServerContext,
    session: &SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (_, char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let caller_sys = resolve_caller_solar_system(ctx, session, char_id);
    let sys_id = bound_entry
        .map(|b| b.object_id as u32)
        .filter(|&id| id > 0)
        .unwrap_or(caller_sys);

    let mut per_probe_states: HashMap<i64, i32> = HashMap::new();
    let mut bulk_state: Option<i32> = None;

    match args {
        [PyRep::Dict(entries)] => {
            for (k, v) in entries {
                if let (Some(pid), Some(st)) = (k.as_i64(), v.as_i64()) {
                    per_probe_states.insert(pid, st as i32);
                }
            }
        }
        [first, second, ..] => {
            if let Some(st) = second.as_i64() {
                let st_i32 = st as i32;
                match first {
                    PyRep::List(ids) | PyRep::Tuple(ids) => {
                        for id in ids {
                            if let Some(pid) = id.as_i64() {
                                per_probe_states.insert(pid, st_i32);
                            }
                        }
                    }
                    _ => {
                        if let Some(pid) = first.as_i64() {
                            per_probe_states.insert(pid, st_i32);
                        } else {
                            bulk_state = Some(st_i32);
                        }
                    }
                }
            }
        }
        [first] => {
            if let Some(st) = first.as_i64() {
                bulk_state = Some(st as i32);
            }
        }
        [] => {}
    }

    let valid_state = |st: i32| {
        (PROBE_STATE_INACTIVE..=PROBE_STATE_RETURNING).contains(&st)
    };
    if let Some(st) = bulk_state {
        if !valid_state(st) {
            return Some(Err(BridgeRpcError {
                code: "InvalidProbeState".into(),
                message: format!("Invalid scanner probe state {st}"),
            }));
        }
    }
    for &st in per_probe_states.values() {
        if !valid_state(st) {
            return Some(Err(BridgeRpcError {
                code: "InvalidProbeState".into(),
                message: format!("Invalid scanner probe state {st}"),
            }));
        }
    }

    let mut changed: Vec<(i64, i32)> = Vec::new();
    if let Ok(mut map) = ctx.active_scanner_probes.write() {
        for p in map.values_mut() {
            if p.owner_char_id != char_id || (sys_id > 0 && p.solar_system_id != sys_id) {
                continue;
            }
            let target_state = if !per_probe_states.is_empty() {
                per_probe_states.get(&p.probe_id).copied()
            } else {
                bulk_state
            };
            if let Some(st) = target_state {
                if p.state != st {
                    p.state = st;
                    changed.push((p.probe_id, st));
                }
            }
        }
    }
    if !changed.is_empty() {
        sync_scanner_probes_to_db(ctx);
    }
    for (pid, st) in changed {
        let _ = ctx.notify(
            crate::router::NotificationTarget::Character(char_id),
            "OnProbeStateChanged",
            "charid",
            vec![PyRep::Integer(pid), PyRep::Integer(i64::from(st))],
        );
    }
    Some(Ok(PyRep::None))
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
    let caller_sys = resolve_caller_solar_system(ctx, session, char_id);
    let sys_id = if let Some(arg_sys) = args.first().and_then(|a| a.as_i64()).map(|v| v as u32) {
        if arg_sys != caller_sys && !session.has_gm_role() {
            return Some(Err(BridgeRpcError {
                code: "AccessDenied".into(),
                message: "Cannot query scan manager for a remote solar system".into(),
            }));
        }
        arg_sys
    } else {
        caller_sys
    };
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
                let (mass_stage, wh_size, life_stage, wh_age, is_collapsed) = compute_wormhole_stages_with_sde(&s, now_unix, Some(&ctx.sde));
                let buckets = derive_wormhole_buckets(&s, now_unix, &ctx.sde);
                kv.extend([
                    ("typeID", s.wormhole_type_id.map(|t| PyRep::Integer(t as i64)).unwrap_or(PyRep::None)),
                    ("massStage", PyRep::String(mass_stage.into())),
                    ("wormholeSize", PyRep::Real(wh_size)),
                    ("lifetimeStage", PyRep::String(life_stage.into())),
                    ("wormholeAge", PyRep::Integer(wh_age)),
                    ("lifetimeDescription", PyRep::String(buckets.lifetime_description.into())),
                    ("massStabilityDescription", PyRep::String(buckets.mass_stability_description.into())),
                    ("destinationClass", PyRep::String(buckets.destination_class.into())),
                    ("maxShipSize", PyRep::String(buckets.max_ship_size.into())),
                    ("lifetimeBucket", PyRep::String(buckets.lifetime_bucket.into())),
                    ("massStabilityBucket", PyRep::String(buckets.mass_stability_bucket.into())),
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
    let caller_sys = resolve_caller_solar_system(ctx, session, char_id);
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

    let caller_sys = resolve_caller_solar_system(ctx, session, char_id);
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
                        probe.destination = [x, y, z];
                        moved_balls.push((pid, DVec3::from_array(probe.destination)));
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
                    probe.destination = [x, y, z];
                    moved_balls.push((pid, DVec3::from_array(probe.destination)));
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
                        probe.destination = [x, y, z];
                        moved_balls.push((pid, DVec3::from_array(probe.destination)));
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

    let warp_ft = ctx.now_blueos_filetime_ticks();
    let mut max_warp_end_ft = warp_ft;
    for (pid, new_pos) in moved_balls {
        if let Ok(Some((_, b))) = sys_handle.get_ball_snapshot(pid).await {
            let old_pos = b.position;
            let dist = (old_pos - new_pos).length();
            if dist > 1.0 {
                let p_tid = ctx.active_scanner_probes.read().ok().and_then(|m| m.get(&pid).map(|p| p.probe_type_id)).unwrap_or(30_013);
                let p_sde = ctx.sde.types.get(&p_tid);
                let warp_au: f64 = p_sde
                    .map(|t| t.default_attributes.get(&600).copied().unwrap_or(15.0) * t.default_attributes.get(&1281).copied().unwrap_or(1.0))
                    .unwrap_or(15.0)
                    .max(1.0);
                let max_vel = p_sde.and_then(|t| t.default_attributes.get(&37).copied()).unwrap_or(3_000.0).max(500.0);
                if dist <= 150_000.0 {
                    let move_ms = ((dist / max_vel) * 1000.0).round().clamp(200.0, 5_000.0) as i64;
                    let p_end_ft = warp_ft + move_ms * 10_000;
                    max_warp_end_ft = max_warp_end_ft.max(p_end_ft);
                    let _ = sys_handle.set_movement_mode(pid, eve_destiny::MovementMode::GotoDirection { direction: (new_pos - old_pos).normalize_or_zero() }).await;
                    let _ = sys_handle.set_speed_fraction(pid, 1.0).await;
                    let _ = ctx.notify(crate::router::NotificationTarget::Character(char_id), "OnProbeStateChanged", "charid", vec![PyRep::Integer(pid), PyRep::Integer(PROBE_STATE_MOVING as i64)]);
                    if let Ok(mut probe_map) = ctx.active_scanner_probes.write() {
                        if let Some(p) = probe_map.get_mut(&pid) { p.warp_end_ft = Some(p_end_ft); p.state = PROBE_STATE_MOVING; }
                    }
                } else {
                    let accel_ms = 50.0 * (dist / 1_000.0).max(1.0).ln();
                    let cruise_ms = (dist / (warp_au * AU_IN_METERS)) * 1000.0;
                    let warp_ms = (accel_ms + cruise_ms).round().clamp(500.0, 30_000.0) as i64;
                    let p_end_ft = warp_ft + warp_ms * 10_000;
                    max_warp_end_ft = max_warp_end_ft.max(p_end_ft);
                    let _ = sys_handle.set_movement_mode(pid, eve_destiny::MovementMode::new_warp(old_pos, new_pos, warp_au)).await;
                    let from_t = PyRep::Tuple(vec![PyRep::Real(old_pos.x), PyRep::Real(old_pos.y), PyRep::Real(old_pos.z)]);
                    let to_t = PyRep::Tuple(vec![PyRep::Real(new_pos.x), PyRep::Real(new_pos.y), PyRep::Real(new_pos.z)]);
                    let _ = ctx.notify(crate::router::NotificationTarget::Character(char_id), "OnProbeStateChanged", "charid", vec![PyRep::Integer(pid), PyRep::Integer(PROBE_STATE_WARPING as i64)]);
                    let _ = ctx.notify(crate::router::NotificationTarget::Character(char_id), "OnProbeWarpStart", "charid", vec![PyRep::Integer(pid), from_t, to_t, PyRep::Integer(warp_ft), PyRep::Integer(warp_ms)]);
                    if let Ok(mut probe_map) = ctx.active_scanner_probes.write() {
                        if let Some(p) = probe_map.get_mut(&pid) { p.warp_end_ft = Some(p_end_ft); p.state = PROBE_STATE_WARPING; }
                    }
                }
            }
        }
    }

    let active_probes: Vec<crate::router::ActiveScannerProbe> = ctx.active_scanner_probes.read().ok()
        .map(|map| map.values().filter(|p| p.owner_char_id == char_id && p.solar_system_id == sys_id && p.state > 0).cloned().collect())
        .unwrap_or_default();
    if active_probes.is_empty() {
        return Some(Err(BridgeRpcError { code: "NoActiveProbes".into(), message: "No active scanner probes in space".into() }));
    }
    let balls = sys_handle.list_balls().await.unwrap_or_default();
    let inhibitor_positions = crate::services::space_destiny::space_cyno_filaments::active_scan_inhibitor_positions(ctx, sys_id, &balls);
    let caller_ship_id = session.ship_id.or_else(|| active_probes.first().map(|p| p.ship_id));
    if let Some(cb) = caller_ship_id.and_then(|csid| balls.iter().find(|b| b.id == csid)) {
        if inhibitor_positions.iter().any(|pos| cb.position.distance(*pos) <= 30_000.0) {
            return Some(Ok(PyRep::List(Vec::new())));
        }
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
                    let p_pos = DVec3::from_array(probe.destination);
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
            let (mass_stage, wh_size, life_stage, wh_age, is_collapsed) = compute_wormhole_stages_with_sde(&sig, now_unix, Some(&ctx.sde));
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
            if sig.sig_group == "Wormhole" && super::super::is_pochven_wormhole(&ctx.sde, &sig) {
                let (wh_pos, guard_ids) = super::super::open_pochven_wormhole_and_spawn_guards(ctx, sys_id, &sig, wh_item_id).await;
                kv.extend([
                    ("isDeadspace", PyRep::Bool(false)),
                    ("wormholeOffsetMeters", PyRep::Real(30_000.0)),
                    ("wormholePosition", PyRep::Tuple(vec![PyRep::Real(wh_pos.x), PyRep::Real(wh_pos.y), PyRep::Real(wh_pos.z)])),
                    ("guardNpcBallIDs", PyRep::List(guard_ids.into_iter().map(PyRep::Integer).collect())),
                ]);
            }
        }
        rows.push(PyRep::keyval(kv));
    }

    crate::services::mobile_deployables_ext::sync_system_deployables(ctx, sys_id).await;
    let scan_inhibitors = crate::services::mobile_deployables_ext::active_scan_inhibitor_positions(ctx, sys_id);
    if !scan_inhibitors.is_empty() && session.station_id.is_none() && session.structure_id.is_none() {
        if let Some(sid) = session.ship_id {
            if let Some(cpos) = sys_handle.get_ball_position_sync(sid) {
                if crate::services::mobile_deployables_ext::is_pos_scan_inhibited(&scan_inhibitors, cpos) {
                    return Some(Ok(PyRep::List(Vec::new())));
                }
            }
        }
    }

    // parity: feat:scanning.probe-target-classes
    let combat_probes: Vec<&crate::router::ActiveScannerProbe> = active_probes
        .iter()
        .filter(|p| is_combat_scanner_probe(&ctx.sde, p.probe_type_id))
        .collect();
    if !combat_probes.is_empty() {
        let caller_ship = session.ship_id;
        for ball in balls {
            if Some(ball.id) == caller_ship {
                continue;
            }
            let Some(type_rec) = ctx.sde.types.get(&ball.type_id) else { continue };
            let category_id = type_rec.category_id;
            if matches!(category_id, 6 | 18 | 87)
                && inhibitor_positions
                    .iter()
                    .any(|pos| ball.position.distance(*pos) <= 30_000.0)
            {
                continue;
            }
            let Some(type_rec) = ctx.sde.types.get(&ball.type_id) else { continue };
            let group_id = type_rec.group_id;
            let category_id = type_rec.category_id;
            if group_id != 1275
                && matches!(category_id, 6 | 18 | 87)
                && crate::services::mobile_deployables_ext::is_pos_scan_inhibited(&scan_inhibitors, ball.position)
            {
                continue;
            }
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
            } else if matches!(ball.type_id, 47_458 | 47_833 | 47_834) || group_id == 1991 {
                "AbyssalTrace"
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

    let scan_probe_ids: Vec<PyRep> = active_probes.iter().map(|p| PyRep::Integer(p.probe_id)).collect();
    let idle_probes: Vec<PyRep> = active_probes.iter().map(|p| PyRep::keyval([("probeID", PyRep::Integer(p.probe_id)), ("destination", PyRep::Tuple(vec![PyRep::Real(p.destination[0]), PyRep::Real(p.destination[1]), PyRep::Real(p.destination[2])]))])).collect();
    let scan_ms = ctx.db.with_snapshot(|s| {
        let sk = s.skills.get(&char_id);
        let a_lvl = sk.and_then(|m| m.get(&3412)).map(|x| x.level.min(5)).unwrap_or(0) as f64;
        let acq_lvl = sk.and_then(|m| m.get(&25811)).map(|x| x.level.min(5)).unwrap_or(0) as f64;
        let base_ms = 10_000.0_f64;
        let ship_id = caller_ship_id.unwrap_or(0);
        let mod_mult: f64 = s.items.values().filter(|i| {
            i.location_id == ship_id && matches!(i.flag as u16, 11..=34 | 92..=94 | 125..=132) && s.module_online_states.get(&i.item_id).copied().unwrap_or(true)
        }).filter_map(|i| ctx.sde.types.get(&i.type_id)?.default_attributes.get(&1906).copied()).fold(1.0, |acc, pct| acc * (1.0 + pct / 100.0).clamp(0.5, 1.5));
        (base_ms * (1.0 - 0.05 * a_lvl) * (1.0 - 0.10 * acq_lvl) * mod_mult).round().clamp(1_500.0, 20_000.0) as i64
    });
    let scan_end_ft = max_warp_end_ft + scan_ms * 10_000;
    if max_warp_end_ft == warp_ft {
        if let Ok(mut pm) = ctx.active_scanner_probes.write() {
            for p in &active_probes { if let Some(mp) = pm.get_mut(&p.probe_id) { mp.state = PROBE_STATE_SCANNING; } }
        }
        for p in &active_probes { let _ = ctx.notify(crate::router::NotificationTarget::Character(char_id), "OnProbeStateChanged", "charid", vec![PyRep::Integer(p.probe_id), PyRep::Integer(PROBE_STATE_SCANNING as i64)]); }
    }
    sync_scanner_probes_to_db(ctx);
    let _ = ctx.notify(crate::router::NotificationTarget::Character(char_id), "OnSystemScanStarted", "charid", vec![PyRep::Integer(max_warp_end_ft), PyRep::Integer(scan_ms), PyRep::List(scan_probe_ids.clone())]);
    let p_ids: Vec<i64> = active_probes.iter().map(|p| p.probe_id).collect();
    ctx.db.with_mut_snapshot(|snap| {
        snap.fleet_and_social_ops.pending_probe_scans.push((
            char_id,
            p_ids,
            scan_probe_ids,
            rows.clone(),
            idle_probes,
            scan_end_ft,
        ));
    });
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
    let sys_id = bound_entry.map(|b| b.object_id as u32).unwrap_or_else(|| resolve_caller_solar_system(ctx, session, char_id));

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

    let candidate_probes: Vec<crate::router::ActiveScannerProbe> = if let Ok(mut map) = ctx.active_scanner_probes.write() {
        let ids: Vec<i64> = map.values()
            .filter(|p| p.owner_char_id == char_id && p.solar_system_id == sys_id && p.state > 0 && (target_probe_ids.is_empty() || target_probe_ids.contains(&p.probe_id)))
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
                .or_else(|| ctx.db.with_snapshot(|s| s.items.get(&sid).and_then(|it| ctx.sde.types.get(&it.type_id).map(|t| t.capacity))))
        })
        .unwrap_or(400.0);

    let (recovered_probes, unrecovered_probes, to_launcher, to_cargo) = if !candidate_probes.is_empty() {
        ctx.db.with_mut_snapshot(|snap| {
            let (mut in_launcher, mut in_cargo) = (0i32, 0i32);
            let (mut rec, mut unrec) = (Vec::new(), Vec::new());
            let add_stack = |items: &mut HashMap<i64, eve_db::InventoryItemRecord>, next_id: &mut i64, ship_id: i64, flag: InventoryFlag, p_type_id: u32, qty: i32, exclude_id: i64| {
                if let Some(st) = items.values_mut().find(|i| i.owner_id == char_id && i.location_id == ship_id && i.flag == flag && i.item_id != exclude_id && i.type_id == p_type_id) {
                    st.quantity += qty;
                    st.version += 1;
                } else {
                    *next_id += 1;
                    let new_id = *next_id;
                    items.insert(new_id, eve_db::InventoryItemRecord {
                        item_id: new_id, type_id: p_type_id, owner_id: char_id, location_id: ship_id,
                        flag, quantity: qty, singleton: false, version: 1, ..Default::default()
                    });
                }
            };
            let mut used_cargo_vol: f64 = ship_id_opt.map(|ship_id| {
                snap.items.values()
                    .filter(|i| i.location_id == ship_id && i.flag == InventoryFlag::Cargo)
                    .map(|i| ctx.sde.types.get(&i.type_id).map(|t| t.volume).unwrap_or(0.1) * (i.quantity.max(1) as f64))
                    .sum()
            }).unwrap_or(0.0);
            for p in candidate_probes {
                let (p_type_id, ship_id, launcher_id) = (p.probe_type_id, ship_id_opt.unwrap_or(p.ship_id), p.launcher_module_id);
                let mut placed_in_launcher = false;
                if let Some(li) = snap.items.get(&launcher_id).cloned().filter(|i| i.location_id == ship_id) {
                    let cur_clip: i32 = snap.items.values()
                        .filter(|i| i.owner_id == char_id && i.location_id == ship_id && i.flag == li.flag && i.item_id != launcher_id)
                        .map(|i| i.quantity)
                        .sum();
                    let matching_clip = snap.items.values()
                        .any(|i| i.owner_id == char_id && i.location_id == ship_id && i.flag == li.flag && i.item_id != launcher_id && i.type_id == p_type_id);
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
    if !recovered_probes.is_empty() {
        sync_scanner_probes_to_db(ctx);
    }
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    for p in &recovered_probes {
        let _ = sys_handle.remove_ball(p.probe_id).await;
    }
    let count = recovered_probes.len() as i32;

    let rec_ids: Vec<PyRep> = recovered_probes.iter().map(|p| PyRep::Integer(p.probe_id)).collect();
    for p in &recovered_probes {
        let _ = ctx.notify(crate::router::NotificationTarget::Character(char_id), "OnProbeStateChanged", "charid", vec![PyRep::Integer(p.probe_id), PyRep::Integer(PROBE_STATE_RETURNING as i64)]);
    }
    if count > 0 {
        let idle_kvs: Vec<PyRep> = recovered_probes.iter().map(|p| PyRep::keyval([("probeID", PyRep::Integer(p.probe_id)), ("destination", PyRep::Tuple(vec![PyRep::Real(p.destination[0]), PyRep::Real(p.destination[1]), PyRep::Real(p.destination[2])]))])).collect();
        let _ = ctx.notify(crate::router::NotificationTarget::Character(char_id), "OnProbesIdle", "charid", vec![PyRep::List(idle_kvs)]);
    }
    Some(Ok(PyRep::keyval([
        ("recoveredProbeIDs", PyRep::List(rec_ids)), ("recoveredCount", PyRep::Integer(count as i64)),
        ("returnedToLauncher", PyRep::Integer(to_launcher as i64)), ("returnedToCargo", PyRep::Integer(to_cargo as i64)),
        ("expiredReturnedToCargoProbeIDs", PyRep::List(auto_returned.into_iter().map(PyRep::Integer).collect())),
        ("expiredLostProbeIDs", PyRep::List(expired_lost.into_iter().map(PyRep::Integer).collect())),
    ])))
}

async fn handle_reconnect_probes(ctx: &ServerContext, session: &mut SessionState, bound_entry: Option<&BoundObjectEntry>) -> Option<Result<PyRep, BridgeRpcError>> {
    let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
    let sys_id = bound_entry.map(|b| b.object_id as u32).unwrap_or_else(|| resolve_caller_solar_system(ctx, session, char_id));
    let (auto_returned, expired_lost) = expire_overdue_probes_for_char(ctx, char_id, ctx.now_unix()).await;
    let ship_id = session.ship_id.unwrap_or(0);
    let mut reconnected_ids = Vec::new();
    if let Ok(mut map) = ctx.active_scanner_probes.write() {
        for p in map.values_mut() {
            if p.owner_char_id == char_id && p.solar_system_id == sys_id && (p.state == PROBE_STATE_INACTIVE || (ship_id != 0 && p.ship_id != ship_id && p.state > 0)) {
                p.state = PROBE_STATE_IDLE;
                if ship_id != 0 { p.ship_id = ship_id; }
                reconnected_ids.push(p.probe_id);
            }
        }
    }
    if !reconnected_ids.is_empty() {
        sync_scanner_probes_to_db(ctx);
    }
    for &pid in &reconnected_ids {
        let _ = ctx.notify(crate::router::NotificationTarget::Character(char_id), "OnProbeStateChanged", "charid", vec![PyRep::Integer(pid), PyRep::Integer(PROBE_STATE_IDLE as i64)]);
    }
    let count = reconnected_ids.len() as i64;
    Some(Ok(PyRep::keyval([
        ("reconnectedProbeIDs", PyRep::List(reconnected_ids.into_iter().map(PyRep::Integer).collect())),
        ("reconnectedCount", PyRep::Integer(count)),
        ("expiredReturnedToCargoProbeIDs", PyRep::List(auto_returned.into_iter().map(PyRep::Integer).collect())),
        ("expiredLostProbeIDs", PyRep::List(expired_lost.into_iter().map(PyRep::Integer).collect())),
    ])))
}

async fn handle_destroy_probe(ctx: &ServerContext, session: &mut SessionState, bound_entry: Option<&BoundObjectEntry>, args: &[PyRep]) -> Option<Result<PyRep, BridgeRpcError>> {
    let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
    let sys_id = bound_entry.map(|b| b.object_id as u32).unwrap_or_else(|| resolve_caller_solar_system(ctx, session, char_id));
    let target_pid = args.first().and_then(|a| a.as_i64());
    let destroyed_probes: Vec<crate::router::ActiveScannerProbe> = if let Ok(mut map) = ctx.active_scanner_probes.write() {
        let ids: Vec<i64> = map.values().filter(|p| p.owner_char_id == char_id && p.state >= 0 && (target_pid.is_none() || target_pid == Some(p.probe_id))).map(|p| p.probe_id).collect();
        ids.into_iter().filter_map(|id| map.remove(&id)).collect()
    } else { Vec::new() };
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    for p in &destroyed_probes {
        let _ = sys_handle.remove_ball(p.probe_id).await;
        let _ = ctx.notify(crate::router::NotificationTarget::Character(char_id), "OnRemoveProbe", "charid", vec![PyRep::Integer(p.probe_id)]);
    }
    if !destroyed_probes.is_empty() {
        sync_scanner_probes_to_db(ctx);
    }
    let any_left = ctx.active_scanner_probes.read().ok().is_some_and(|m| m.values().any(|p| p.owner_char_id == char_id && p.solar_system_id == sys_id && p.state >= 0));
    if !destroyed_probes.is_empty() && !any_left {
        let _ = ctx.notify(crate::router::NotificationTarget::Character(char_id), "OnScannerInfoRemoved", "charid", vec![]);
    }
    Some(Ok(PyRep::keyval([
        ("destroyedProbeID", destroyed_probes.first().map(|p| PyRep::Integer(p.probe_id)).unwrap_or(PyRep::None)),
        ("destroyedCount", PyRep::Integer(destroyed_probes.len() as i64)),
        ("destroyed", PyRep::Bool(!destroyed_probes.is_empty())),
    ])))
}

/// Emits `OnProbeWarpEnd`, `OnProbeStateChanged`, and `OnSystemScanStopped` as probe timers elapse.
pub fn tick_probe_warp_end(ctx: &ServerContext) {
    let now_ft = ctx.now_blueos_filetime_ticks();
    let mut due_arrivals = Vec::new();
    if let Ok(mut probe_map) = ctx.active_scanner_probes.write() {
        for p in probe_map.values_mut() {
            if p.warp_end_ft.is_some_and(|end_ft| now_ft >= end_ft) {
                let was_warping = p.state == PROBE_STATE_WARPING;
                p.warp_end_ft = None;
                p.position = p.destination;
                p.state = PROBE_STATE_SCANNING;
                due_arrivals.push((p.owner_char_id, p.solar_system_id, p.probe_id, p.destination, was_warping));
            }
        }
    }
    let had_arrivals = !due_arrivals.is_empty();
    for (char_id, sys_id, pid, dest, was_warping) in due_arrivals {
        let sys_h = ctx.systems.get_or_spawn(sys_id);
        sys_h.try_set_ball_position(pid, DVec3::from_array(dest));
        sys_h.try_stop_ball(pid);
        if was_warping {
            crate::services::notify_emitters_tournaments_anomalies::emit_probe_warp_end(ctx, char_id, pid);
        }
        let _ = ctx.notify(crate::router::NotificationTarget::Character(char_id), "OnProbeStateChanged", "charid", vec![PyRep::Integer(pid), PyRep::Integer(PROBE_STATE_SCANNING as i64)]);
    }
    let has_due_scans = ctx.db.with_snapshot(|snap| {
        snap.fleet_and_social_ops.pending_probe_scans.iter().any(|(_, _, _, _, _, end_ft)| now_ft >= *end_ft)
    });
    let due_scans = if has_due_scans { ctx.db.with_mut_snapshot(|snap| {
        let all = std::mem::take(&mut snap.fleet_and_social_ops.pending_probe_scans);
        let (done, rem): (Vec<_>, Vec<_>) = all.into_iter().partition(|(_, _, _, _, _, end_ft)| now_ft >= *end_ft);
        snap.fleet_and_social_ops.pending_probe_scans = rem;
        done
    }) } else { Vec::new() };
    let had_due_scans = !due_scans.is_empty();
    for (char_id, p_ids, scan_probe_ids, rows, idle_probes, _) in due_scans {
        if let Ok(mut probe_map) = ctx.active_scanner_probes.write() {
            for pid in &p_ids {
                if let Some(p) = probe_map.get_mut(pid) {
                    if matches!(p.state, PROBE_STATE_SCANNING | PROBE_STATE_WARPING | PROBE_STATE_MOVING) {
                        p.state = PROBE_STATE_IDLE;
                    }
                }
            }
        }
        for pid in p_ids {
            let _ = ctx.notify(crate::router::NotificationTarget::Character(char_id), "OnProbeStateChanged", "charid", vec![PyRep::Integer(pid), PyRep::Integer(PROBE_STATE_IDLE as i64)]);
        }
        let _ = ctx.notify(crate::router::NotificationTarget::Character(char_id), "OnSystemScanStopped", "charid", vec![PyRep::List(scan_probe_ids), PyRep::List(rows), PyRep::List(Vec::new())]);
        let _ = ctx.notify(crate::router::NotificationTarget::Character(char_id), "OnProbesIdle", "charid", vec![PyRep::List(idle_probes)]);
    }
    if had_arrivals || had_due_scans {
        sync_scanner_probes_to_db(ctx);
    }
}

/// Persists non-expired tracked scanner probes (`state >= 0`) to `Sp6PersistedState`.
/// Locks `db` before `active_scanner_probes.read()` to prevent stale out-of-order overwrites.
pub(crate) fn sync_scanner_probes_to_db(ctx: &ServerContext) {
    crate::sp6_lifecycle::with_mut_sp6_state(&ctx.db, |st, _snap| {
        if let Ok(map) = ctx.active_scanner_probes.read() {
            st.active_scanner_probes = map
                .iter()
                .filter(|(_, p)| p.state >= 0)
                .map(|(&id, p)| (id, p.clone()))
                .collect();
        }
    });
    ctx.persist_server_runtime_to_db();
}
