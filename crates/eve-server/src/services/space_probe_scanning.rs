//! `space_probe_scanning` — Core & Combat scanner probe launch, range/deviation math,
//! target class filtering, signature reveal thresholds, difficulty levels, and flight-time expiry.

#![forbid(unsafe_code)]

use crate::router::{BoundObjectEntry, ServerContext};
use crate::services::space_destiny::{
    compute_wormhole_stages_with_sde, derive_wormhole_buckets, ensure_system_signatures_and_anomalies,
    pochven_wormhole_ball_position, wormhole_item_id_for_signature, AU_IN_METERS,
};
use crate::services::wormhole_lifecycle_ops::system_wormhole_profile_sde;
use eve_db::types::{CosmicAnomalyRecord, CosmicSignatureRecord, InventoryFlag};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::SolarSystemId;
use glam::DVec3;
use std::collections::{HashMap, HashSet};

pub const PROBE_STATE_INACTIVE: i32 = 0; pub const PROBE_STATE_IDLE: i32 = 1; pub const PROBE_STATE_MOVING: i32 = 2;
pub const PROBE_STATE_WARPING: i32 = 3; pub const PROBE_STATE_SCANNING: i32 = 4; pub const PROBE_STATE_RETURNING: i32 = 5;
const MAX_ACTIVE_SCANNER_PROBES: usize = 8;

fn resolve_caller_solar_system(ctx: &ServerContext, session: &SessionState, char_id: i64) -> u32 {
    session
        .solar_system_id
        .or_else(|| ctx.db.with_snapshot(|s| s.characters.get(&char_id).map(|c| c.solar_system_id)))
        .unwrap_or(0)
}

fn probe_rpc_err(code: &str, message: impl Into<String>) -> BridgeRpcError {
    let msg = message.into();
    BridgeRpcError::with_client_error(
        code,
        msg.clone(),
        eve_proto::ClientError::CustomInfo(msg),
    )
}

fn resolve_probe_space_context(
    ctx: &ServerContext,
    session: &SessionState,
) -> Result<(i64, u32), BridgeRpcError> {
    match ctx.require_undocked_space_system(session, "NotInSpace", "Cannot scan probes while docked", false, false) {
        Ok(v) => Ok(v),
        Err(e) if e.code == "NoSolarSystem" => {
            if ctx.is_session_docked_strict_policy(session, false) {
                return Err(probe_rpc_err("NotInSpace", "Cannot scan probes while docked"));
            }
            let (_, char_id) = ServerContext::require_character(session)?;
            let sys_id = resolve_caller_solar_system(ctx, session, char_id);
            if sys_id == 0 {
                Err(e)
            } else {
                Ok((char_id, sys_id))
            }
        }
        Err(e) => Err(e),
    }
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

#[derive(Clone, Copy)]
enum CrossSystemScanCode {
    CrossSystemScanDenied,
    AccessDenied,
}

impl CrossSystemScanCode {
    fn into_remote_system_error(self, action: &str, req_sys: u32) -> BridgeRpcError {
        let message = format!("Cannot {action} for remote solar system {req_sys}");
        match self {
            Self::CrossSystemScanDenied => BridgeRpcError {
                code: "CrossSystemScanDenied".into(),
                message,
                client_error: None,
            },
            Self::AccessDenied => BridgeRpcError {
                code: "AccessDenied".into(),
                message,
                client_error: None,
            },
        }
    }
}

fn verify_scan_mgr_bound(
    session: &SessionState,
    char_id: i64,
    caller_sys: u32,
    bound_entry: Option<&BoundObjectEntry>,
    action: &str,
    cross_system_code: CrossSystemScanCode,
) -> Result<Option<u32>, BridgeRpcError> {
    let Some(b) = bound_entry else {
        return Ok(None);
    };
    let is_gm = session.has_gm_role();
    if b.character_id != char_id && !is_gm {
        let msg = format!("Cannot {action}: scanMgr bound object belongs to another character");
        return Err(BridgeRpcError::with_client_error(
            "AccessDenied",
            msg.clone(),
            eve_proto::ClientError::CustomInfo(msg),
        ));
    }
    if b.object_id < 0 {
        let msg = format!("Invalid bound solar system {} for {action}", b.object_id);
        return Err(BridgeRpcError::with_client_error(
            "InvalidSolarSystem",
            msg.clone(),
            eve_proto::ClientError::CustomInfo(msg),
        ));
    }
    if b.object_id == 0 {
        return Ok(None);
    }
    let Ok(req_sys) = u32::try_from(b.object_id) else {
        let msg = format!("Invalid bound solar system {} for {action}", b.object_id);
        return Err(BridgeRpcError::with_client_error(
            "InvalidSolarSystem",
            msg.clone(),
            eve_proto::ClientError::CustomInfo(msg),
        ));
    };
    if req_sys != caller_sys && !is_gm {
        return Err(cross_system_code.into_remote_system_error(action, req_sys));
    }
    Ok(Some(req_sys))
}

fn parse_optional_sys_arg(
    ctx: &ServerContext,
    session: &SessionState,
    caller_sys: u32,
    args: &[PyRep],
    action: &str,
    cross_system_code: CrossSystemScanCode,
) -> Result<Option<u32>, BridgeRpcError> {
    if args.len() > 1 {
        let msg = format!("{action} accepts at most 1 argument");
        return Err(BridgeRpcError::with_client_error(
            "InvalidArguments",
            msg.clone(),
            eve_proto::ClientError::CustomInfo(msg),
        ));
    }
    let Some(arg) = args.first() else {
        return Ok(None);
    };
    if matches!(arg, PyRep::None) {
        return Ok(None);
    }
    let Some(raw_sys) = exact_int_arg(arg) else {
        let msg = format!("Invalid solar system argument for {action}");
        return Err(BridgeRpcError::with_client_error(
            "InvalidSolarSystem",
            msg.clone(),
            eve_proto::ClientError::CustomInfo(msg),
        ));
    };
    let Ok(req_sys) = u32::try_from(raw_sys) else {
        let msg = format!("Invalid solar system {raw_sys} for {action}");
        return Err(BridgeRpcError::with_client_error(
            "InvalidSolarSystem",
            msg.clone(),
            eve_proto::ClientError::CustomInfo(msg),
        ));
    };
    if req_sys == 0 {
        let msg = format!("Invalid solar system 0 for {action}");
        return Err(BridgeRpcError::with_client_error(
            "InvalidSolarSystem",
            msg.clone(),
            eve_proto::ClientError::CustomInfo(msg),
        ));
    }
    if req_sys != caller_sys {
        if !session.has_gm_role() {
            return Err(cross_system_code.into_remote_system_error(action, req_sys));
        }
        if !ctx.sde.solar_systems.is_empty() && !ctx.sde.solar_systems.contains_key(&req_sys) {
            let msg = format!("Unknown solar system {req_sys} for {action}");
            return Err(BridgeRpcError::with_client_error(
                "InvalidSolarSystem",
                msg.clone(),
                eve_proto::ClientError::CustomInfo(msg),
            ));
        }
    }
    Ok(Some(req_sys))
}

fn is_signature_active(sig: &CosmicSignatureRecord, now_unix: u64) -> bool {
    !sig.collapsed
        && (sig.lifetime_seconds == 0
            || now_unix < sig.created_unix.saturating_add(sig.lifetime_seconds))
}

fn is_anomaly_active(anom: &CosmicAnomalyRecord, now_unix: u64) -> bool {
    !anom.cleared
        && (anom.lifetime_seconds == 0
            || now_unix < anom.created_unix.saturating_add(anom.lifetime_seconds))
}

/// Reads cached cosmic signatures & anomalies purely from snapshot when native sites are
/// already seeded and unexpired (`None` indicates seeding or expiry cleanup is needed).
fn read_system_signatures_and_anomalies_ready(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
    now_unix: u64,
) -> Option<(Vec<CosmicSignatureRecord>, Vec<CosmicAnomalyRecord>)> {
    ctx.db.with_snapshot(|snap| {
        let sigs = snap.cosmic_signatures.get(&sys_id)?;
        let has_native_sigs = sigs.iter().any(|s| {
            !s.signature_id.starts_with("K162-") && !s.signature_id.starts_with("DNG-")
        });
        if !has_native_sigs {
            return None;
        }
        let any_sig_expired = sigs.iter().any(|s| {
            !s.collapsed
                && s.lifetime_seconds > 0
                && now_unix >= s.created_unix.saturating_add(s.lifetime_seconds)
        });
        if any_sig_expired {
            return None;
        }
        let anoms = snap
            .cosmic_anomalies
            .get(&sys_id)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let any_anom_expired = anoms.iter().any(|a| {
            !a.cleared
                && a.lifetime_seconds > 0
                && now_unix >= a.created_unix.saturating_add(a.lifetime_seconds)
        });
        if any_anom_expired {
            return None;
        }
        Some((sigs.clone(), anoms.to_vec()))
    })
}

/// Materializes missing Ballpark wormhole balls for existing active wormhole
/// signatures without mutating `EveDb`.
async fn sync_existing_wormhole_balls(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
    sigs: &[CosmicSignatureRecord],
    now_unix: u64,
) {
    let has_active_wh = sigs
        .iter()
        .any(|s| s.sig_group == "Wormhole" && is_signature_active(s, now_unix));
    if !has_active_wh {
        return;
    }
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    for sig in sigs {
        if sig.sig_group == "Wormhole" && is_signature_active(sig, now_unix) {
            let wh_item_id = wormhole_item_id_for_signature(sys_id, &sig.signature_id);
            if sys_handle
                .get_ball_snapshot(wh_item_id)
                .await
                .ok()
                .flatten()
                .is_none()
            {
                let ball_pos = pochven_wormhole_ball_position(&ctx.sde, sig);
                let _ = sys_handle
                    .spawn_ball(Ball {
                        id: wh_item_id,
                        type_id: sig.wormhole_type_id.unwrap_or(26272),
                        position: ball_pos,
                        velocity: DVec3::ZERO,
                        max_velocity: 0.0,
                        inertia: 1.0,
                        mass: sig.remaining_mass_kg.max(1.0),
                        radius: 1500.0,
                        speed_fraction: 0.0,
                        mode: MovementMode::Rigid,
                    })
                    .await;
            }
            let (_, wh_size, _, wh_age, _) =
                compute_wormhole_stages_with_sde(sig, now_unix, Some(&ctx.sde));
            let _ = sys_handle
                .set_ball_wormhole_stages(wh_item_id, wh_age, wh_size)
                .await;
        }
    }
}

async fn load_system_signatures_and_anomalies_readonly(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
    now_unix: u64,
) -> (Vec<CosmicSignatureRecord>, Vec<CosmicAnomalyRecord>) {
    let (sigs, anoms) = ctx.db.with_snapshot(|snap| {
        let mut sigs = snap.cosmic_signatures.get(&sys_id).cloned().unwrap_or_default();
        sigs.retain(|s| is_signature_active(s, now_unix));
        let mut anoms = snap.cosmic_anomalies.get(&sys_id).cloned().unwrap_or_default();
        anoms.retain(|a| is_anomaly_active(a, now_unix));
        (sigs, anoms)
    });
    sync_existing_wormhole_balls(ctx, sys_id, &sigs, now_unix).await;
    (sigs, anoms)
}

async fn load_system_signatures_and_anomalies(
    ctx: &ServerContext,
    session: &SessionState,
    sys_id: SolarSystemId,
    now_unix: u64,
) -> (Vec<CosmicSignatureRecord>, Vec<CosmicAnomalyRecord>) {
    if let Some((sigs, anoms)) = read_system_signatures_and_anomalies_ready(ctx, sys_id, now_unix) {
        sync_existing_wormhole_balls(ctx, sys_id, &sigs, now_unix).await;
        return (sigs, anoms);
    }
    // Docked or shipless pilots may inspect readouts, so avoid mutating DB if unseeded.
    if session.station_id.is_some()
        || session.structure_id.is_some()
        || session.ship_id.is_none()
    {
        return load_system_signatures_and_anomalies_readonly(ctx, sys_id, now_unix).await;
    }
    ensure_system_signatures_and_anomalies(ctx, sys_id).await
}

#[path = "space_probe_scanning_math.rs"]
mod space_probe_scanning_math;
pub use space_probe_scanning_math::{
    compute_probe_max_deviation_au, is_combat_scanner_probe, probe_allowed_ranges_au,
    probe_base_max_deviation_au, probe_base_scan_range_au, probe_base_sensor_strength,
    probe_max_flight_time_s, signature_difficulty_level_and_min_strength,
};
use space_probe_scanning_math::{
    evaluate_probe_dogma_state, probe_scan_started_probes_dict,
};

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

    let loaded_charge_opt = ctx.db.with_snapshot(|snap| -> Result<Option<(i64, u32, i32)>, BridgeRpcError> {
        if let Some(ch) = snap.characters.get(&char_id) {
            if ch.station_id.is_some()
                || ch.structure_id.is_some()
                || ch.solar_system_id != sys_id
                || ch.active_ship_id != ship_id
            {
                return Err(BridgeRpcError::ship_not_in_space(ship_id, Some(char_id), Some(sys_id), "activate_scan_probe_launcher", format!("Active ship {ship_id} is not in space in solar system {sys_id}")));
            }
        }
        Ok(snap.items.values().filter(|i| {
            i.location_id == ship_id
                && (i.flag as u16) == module_flag_id
                && i.item_id != module_id
                && i.quantity > 0
                && crate::services::fitting_cycles_overheat::is_valid_loaded_assembled_charge(snap, Some(char_id), Some(ship_id), i)
                && ctx.sde.types.get(&i.type_id).is_some_and(|t| t.group_id == 479)
        }).min_by_key(|i| i.item_id).map(|i| (i.item_id, i.type_id, i.quantity)))
    })?;
    let Some((charge_item_id, probe_type_id, loaded_qty)) = loaded_charge_opt else {
        return Err(BridgeRpcError {
            code: "NoProbesLoaded".into(),
            message: "Scan probe launcher has no scanner probes loaded".into(),
            client_error: None,
        });
    };

    if is_combat_scanner_probe(&ctx.sde, probe_type_id) && matches!(module_type_id, 17938 | 28758 | 30651) {
        return Err(BridgeRpcError {
            code: "InvalidProbeForLauncher".into(),
            message: "Combat scanner probes require an Expanded Probe Launcher".into(),
            client_error: None,
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
                client_error: None,
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
                return Err(BridgeRpcError::ship_not_in_space(ship_id, Some(char_id), Some(sys_id), "activate_scan_probe_launcher", format!("Active ship {ship_id} is not in space in solar system {sys_id}")));
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
                client_error: None,
            })?;
        let actual_launch = (reservation_ids.len() as i32).min(ci.quantity);
        if actual_launch <= 0 {
            return Err(BridgeRpcError {
                code: "NoProbesLoaded".into(),
                message: "Scan probe launcher has no scanner probes loaded".into(),
                client_error: None,
            });
        }
        let mut opts = eve_db::ConsumeExactOptions::for_owner_exact(Some(char_id), ci.owner_id, eve_db::ItemClassPolicy::ExactAssembledItem);
        opts.reject_fitted = false;
        opts.active_ship_id = Some(ship_id);
        let rem = match snap.consume_exact_item_quantity(charge_item_id, actual_launch, opts) {
            Ok(Some(it)) => it.quantity,
            Ok(None) => 0,
            Err(e) => {
                return Err(BridgeRpcError {
                    code: "ItemLocked".into(),
                    message: e.to_string(),
                    client_error: None,
                });
            }
        };
        snap.module_active_states.remove(&module_id);
        let mut ids = Vec::with_capacity(actual_launch as usize);
        for _ in 0..actual_launch {
            snap.next_entity_id += 1;
            ids.push(snap.next_entity_id);
        }
        Ok((ids, rem, actual_launch, ci))
    });
    let (allocated_probe_ids, remaining_in_launcher, to_launch, pre_charge_item) = match db_res {
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
    if matches!(pre_charge_item.flag as u16, 11..=34 | 125..=132) {
        crate::services::inventory_fitting::fitting_module_cycles::emit_charge_sublocation_quantity_change(
            ctx,
            char_id,
            ship_id,
            pre_charge_item.flag,
            probe_type_id,
            pre_charge_item.quantity,
            remaining_in_launcher,
        );
    }

    let probe_sde = ctx.sde.types.get(&probe_type_id);
    let launch_fit_opt = ctx.get_or_rebuild_ship_fitting(char_id, ship_id).ok();
    let launch_probe_state = evaluate_probe_dogma_state(
        launch_fit_opt.as_ref(),
        allocated_probe_ids.first().copied().unwrap_or(0),
        probe_type_id,
        &ctx.sde,
    );
    let base_sensor_strength = probe_base_sensor_strength(&ctx.sde, probe_type_id);
    let max_flight_time_s = launch_probe_state
        .as_ref()
        .map(|ps| ps.get_attribute(281, &ctx.sde) / 1000.0)
        .filter(|&v| v > 0.0)
        .unwrap_or_else(|| probe_max_flight_time_s(&ctx.sde, probe_type_id));
    let base_scan_range_au = launch_probe_state
        .as_ref()
        .map(|ps| ps.get_attribute(1370, &ctx.sde))
        .filter(|&v| v > 0.0)
        .unwrap_or_else(|| probe_base_scan_range_au(&ctx.sde, probe_type_id));
    let base_max_deviation_au = probe_base_max_deviation_au(&ctx.sde, probe_type_id);
    let probe_max_vel = launch_probe_state
        .as_ref()
        .map(|ps| ps.get_attribute(37, &ctx.sde))
        .filter(|&v| v > 0.0)
        .or_else(|| probe_sde.and_then(|t| t.default_attributes.get(&37).copied()))
        .unwrap_or(3_000.0);
    let can_scan_ships = is_combat_scanner_probe(&ctx.sde, probe_type_id);
    let allowed_ranges = probe_allowed_ranges_au(&ctx.sde, probe_type_id);
    let range_step = kwargs.get("rangeStep").and_then(|v| v.as_i64()).unwrap_or(4).clamp(1, 8) as i32;
    let scan_range_m = base_scan_range_au * (2.0_f64.powi(range_step - 1)) * AU_IN_METERS;

    let formation_offsets: [[f64; 3]; 8] = [
        [2_500.0, 0.0, 0.0],
        [-2_500.0, 0.0, 0.0],
        [0.0, 2_500.0, 0.0],
        [0.0, -2_500.0, 0.0],
        [0.0, 0.0, 2_500.0],
        [0.0, 0.0, -2_500.0],
        [1_800.0, 1_800.0, 0.0],
        [-1_800.0, -1_800.0, 0.0],
    ];

    let mut spawned_probes: Vec<crate::router::ActiveScannerProbe> = Vec::with_capacity(allocated_probe_ids.len());
    {
        let mut probes = ctx.active_scanner_probes.write().unwrap_or_else(|e| e.into_inner());
        for rid in &reservation_ids {
            probes.remove(rid);
        }
        for (idx, &probe_id) in allocated_probe_ids.iter().enumerate() {
            let off = formation_offsets[idx % formation_offsets.len()];
            let pos = DVec3::new(ship_pos.x + off[0], ship_pos.y + off[1], ship_pos.z + off[2]);
            let probe = crate::router::ActiveScannerProbe {
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
            };
            spawned_probes.push(probe.clone());
            probes.insert(probe_id, probe);
        }
    }
    sync_scanner_probes_to_db(ctx);

    for p in &spawned_probes {
        let ball = Ball {
            id: p.probe_id,
            type_id: probe_type_id,
            position: DVec3::from_array(p.position),
            velocity: DVec3::ZERO,
            max_velocity: probe_max_vel,
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

    if let Some(scene) =
        crate::router::notify_registry::BallparkScene::try_solar_system(sys.solar_system_id)
    {
        let _ = ctx.notify_module_effect_started(
            scene,
            char_id,
            ship_id,
            module_id,
            module_type_id,
            Some(effect_name),
            target_id_opt,
        );
    }
    let stop_eff_id = ctx
        .effect_id_for_module(module_type_id, Some(effect_name))
        .unwrap_or(101) as i32;
    let _ = ctx.notify_godma_ship_effect(
        crate::router::NotificationTarget::Character(char_id),
        module_id,
        stop_eff_id,
        false,
        0,
        0,
    );
    let expiry_ft = ctx.now_blueos_filetime_ticks().saturating_add((max_flight_time_s.round() as i64).saturating_mul(10_000_000));
    let scan_bonuses = default_probe_scan_bonuses();
    for probe in &spawned_probes {
        let probe_kv = build_probe_kv(probe, expiry_ft, &scan_bonuses);
        emit_probe_registered(ctx, char_id, probe.probe_id, probe_kv, probe.state);
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
    let mut persist_runtime = false;
    let res = match (service, method) {
        ("scanMgr", "GetSystemScanMgr") => {
            handle_get_system_scan_mgr(ctx, session, bound_entry, service, args).await
        }
        ("scanMgr", "GetAnomalies") => handle_get_anomalies(ctx, session, bound_entry, args).await,
        ("scanMgr", "RequestScans") => {
            handle_request_scans(ctx, session, bound_entry, args, kwargs).await
        }
        ("scanMgr", "RecoverProbes") => handle_recover_probes(ctx, session, bound_entry, args).await,
        ("scanMgr", "ReconnectToLostProbes") => {
            handle_reconnect_probes(ctx, session, bound_entry, args).await
        }
        ("scanMgr", "DestroyProbe") => handle_destroy_probe(ctx, session, bound_entry, args).await,
        ("scanMgr", "GetFullState") => handle_get_full_state(ctx, session, bound_entry, args).await,
        ("scanMgr", "QAOverrideProbeExpiry") => handle_qa_override_probe_expiry(ctx, session, args),
        ("scanMgr", "QAScanSites") => {
            persist_runtime = true;
            handle_qa_scan_sites(ctx, session, bound_entry).await
        }
        ("scanMgr", "SetActivityState") => {
            handle_set_activity_state(ctx, session, bound_entry, args)
        }
        _ => None,
    };
    if persist_runtime && matches!(res, Some(Ok(_))) {
        ctx.persist_server_runtime_to_db();
    }
    res
}

async fn handle_get_full_state(
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
    let is_gm = session.has_gm_role();
    let bound_sys = match verify_scan_mgr_bound(
        session,
        char_id,
        caller_sys,
        bound_entry,
        "query GetFullState",
        CrossSystemScanCode::CrossSystemScanDenied,
    ) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let arg_sys = match parse_optional_sys_arg(
        ctx,
        session,
        caller_sys,
        args,
        "GetFullState",
        CrossSystemScanCode::CrossSystemScanDenied,
    ) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let sys_id = if is_gm {
        arg_sys.or(bound_sys).unwrap_or(caller_sys)
    } else {
        caller_sys
    };
    if sys_id == 0 {
        let msg = "Session has no active solar system";
        return Some(Err(BridgeRpcError::with_client_error(
            "NoSolarSystem",
            msg,
            eve_proto::ClientError::CustomInfo(msg.into()),
        )));
    }
    let now_unix = ctx.now_unix();
    let (sigs, mut anomalies) =
        load_system_signatures_and_anomalies_readonly(ctx, sys_id, now_unix).await;
    anomalies.retain(|a| is_anomaly_active(a, now_unix));
    let anom_ids: std::collections::HashSet<String> =
        anomalies.iter().map(|a| a.anomaly_id.clone()).collect();
    let anom_id_refs: std::collections::HashSet<&str> =
        anom_ids.iter().map(String::as_str).collect();
    let anom_entries: Vec<(PyRep, PyRep)> = anomalies
        .into_iter()
        .map(|a| {
            let kv = crate::services::fleet_social::fleet_social_scanning::build_signal_tracker_anomaly_kv(&ctx.sde, &a);
            (PyRep::String(a.anomaly_id), kv)
        })
        .collect();
    let sig_entries: Vec<(PyRep, PyRep)> = sigs
        .into_iter()
        .filter(|s| {
            is_signature_active(s, now_unix)
                && !crate::services::fleet_social::fleet_social_scanning::is_anomaly_colliding_signature(
                    s,
                    &anom_id_refs,
                )
        })
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
        return Some(Err(BridgeRpcError::insufficient_role_with_code("GMRoleRequired", eve_machonet::service_roles::ROLE_GM_MASK, session.role, "scanMgr.QAOverrideProbeExpiry", "scanMgr.QAOverrideProbeExpiry requires GM/QA role")));
    }
    if args.len() > 1 {
        let msg = "scanMgr.QAOverrideProbeExpiry accepts at most 1 argument";
        return Some(Err(BridgeRpcError::with_client_error(
            "InvalidArguments",
            msg,
            eve_proto::ClientError::CustomInfo(msg.into()),
        )));
    }
    let secs = match args.first() {
        None | Some(PyRep::None) => 60.0,
        Some(arg) => match exact_float_arg(arg).filter(|v| v.is_finite() && *v > 0.0) {
            Some(v) => v.max(1.0),
            None => {
                let msg = "Invalid probe expiry override duration";
                return Some(Err(BridgeRpcError::with_client_error(
                    "InvalidArguments",
                    msg,
                    eve_proto::ClientError::CustomInfo(msg.into()),
                )));
            }
        },
    };
    let mut updated_any = false;
    {
        let mut map = ctx.active_scanner_probes.write().unwrap_or_else(|e| e.into_inner());
        for p in map.values_mut() {
            if p.owner_char_id == char_id {
                p.max_flight_time_s = secs;
                updated_any = true;
            }
        }
    }
    if updated_any {
        sync_scanner_probes_to_db(ctx);
    }
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
        return Some(Err(BridgeRpcError::insufficient_role_with_code("GMRoleRequired", eve_machonet::service_roles::ROLE_GM_MASK, session.role, "scanMgr.QAScanSites", "scanMgr.QAScanSites requires GM/QA role")));
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
            client_error: None,
        }));
    };
    let (sigs, anomalies) = ensure_system_signatures_and_anomalies(ctx, sys_id).await;
    let now_unix = ctx.now_unix();
    let anom_ids: std::collections::HashSet<String> =
        anomalies.iter().map(|a| a.anomaly_id.clone()).collect();
    let anom_id_refs: std::collections::HashSet<&str> =
        anom_ids.iter().map(String::as_str).collect();
    let mut results = format_anomalies(anomalies, now_unix);
    for s in sigs {
        if s.collapsed
            || crate::services::fleet_social::fleet_social_scanning::is_anomaly_colliding_signature(
                &s,
                &anom_id_refs,
            )
        {
            continue;
        }
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

enum SetActivityStateMutationError {
    ProbeNotFound(i64),
    MaxProbesLaunched,
}

/// Validates target ownership and enforces EVE's 8-probe active cap before applying
/// `scanMgr.SetActivityState` transitions inside the probe map write lock.
fn apply_set_activity_state_updates(
    map: &mut HashMap<i64, crate::router::ActiveScannerProbe>,
    char_id: i64,
    sys_id: u32,
    per_probe_states: &HashMap<i64, i32>,
    bulk_state: Option<i32>,
) -> Result<Vec<(i64, i32)>, SetActivityStateMutationError> {
    let mut changed = Vec::new();
    let matches_scope = |p: &crate::router::ActiveScannerProbe| {
        p.owner_char_id == char_id && (sys_id == 0 || p.solar_system_id == sys_id)
    };

    if !per_probe_states.is_empty() {
        let mut target_pids: Vec<i64> = per_probe_states.keys().copied().collect();
        target_pids.sort_unstable();
        for &pid in &target_pids {
            let valid_target = map
                .get(&pid)
                .is_some_and(|p| matches_scope(p) && p.state >= 0);
            if !valid_target {
                return Err(SetActivityStateMutationError::ProbeNotFound(pid));
            }
        }
        // Reject atomically before mutating if activating inactive (`0`) probes would exceed 8 active.
        let activating_from_inactive = target_pids.iter().any(|pid| {
            per_probe_states[pid] > PROBE_STATE_INACTIVE
                && map
                    .get(pid)
                    .is_some_and(|p| p.state == PROBE_STATE_INACTIVE)
        });
        if activating_from_inactive {
            let projected_active = map
                .values()
                .filter(|p| matches_scope(p))
                .filter(|p| {
                    per_probe_states
                        .get(&p.probe_id)
                        .copied()
                        .unwrap_or(p.state)
                        > PROBE_STATE_INACTIVE
                })
                .count();
            if projected_active > MAX_ACTIVE_SCANNER_PROBES {
                return Err(SetActivityStateMutationError::MaxProbesLaunched);
            }
        }
        for pid in target_pids {
            let st = per_probe_states[&pid];
            if let Some(p) = map.get_mut(&pid) {
                if p.state != st {
                    p.state = st;
                    changed.push((pid, st));
                }
            }
        }
    } else if let Some(st) = bulk_state {
        if st > PROBE_STATE_INACTIVE {
            // Cap bulk `0 -> >0` reactivations to remaining active slots in deterministic probe_id order
            // (matching `handle_reconnect_probes`) while still updating already-active probes.
            let existing_active = map
                .values()
                .filter(|p| matches_scope(p) && p.state > PROBE_STATE_INACTIVE)
                .count();
            let remaining_slots = MAX_ACTIVE_SCANNER_PROBES.saturating_sub(existing_active);
            let mut inactive_ids: Vec<i64> = map
                .values()
                .filter(|p| matches_scope(p) && p.state == PROBE_STATE_INACTIVE)
                .map(|p| p.probe_id)
                .collect();
            inactive_ids.sort_unstable();
            inactive_ids.truncate(remaining_slots);

            for p in map.values_mut() {
                if matches_scope(p) && p.state > PROBE_STATE_INACTIVE && p.state != st {
                    p.state = st;
                    changed.push((p.probe_id, st));
                }
            }
            for pid in inactive_ids {
                if let Some(p) = map.get_mut(&pid) {
                    p.state = st;
                    changed.push((pid, st));
                }
            }
            changed.sort_unstable_by_key(|&(pid, _)| pid);
        } else {
            for p in map.values_mut() {
                if !matches_scope(p) || p.state < 0 {
                    continue;
                }
                if p.state != st {
                    p.state = st;
                    changed.push((p.probe_id, st));
                }
            }
            changed.sort_unstable_by_key(|&(pid, _)| pid);
        }
    }
    Ok(changed)
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
    if caller_sys == 0 {
        let msg = "Session has no active solar system";
        return Some(Err(BridgeRpcError::with_client_error(
            "NoSolarSystem",
            msg,
            eve_proto::ClientError::CustomInfo(msg.into()),
        )));
    }
    let bound_sys = match verify_scan_mgr_bound(
        session,
        char_id,
        caller_sys,
        bound_entry,
        "set probe activity state",
        CrossSystemScanCode::AccessDenied,
    ) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let sys_id = bound_sys.unwrap_or(caller_sys);

    let mut per_probe_states: HashMap<i64, i32> = HashMap::new();
    let mut bulk_state: Option<i32> = None;

    let invalid_state_err = |msg: String| {
        Some(Err(BridgeRpcError {
            code: "InvalidProbeState".into(),
            message: msg,
            client_error: None,
        }))
    };

    match args {
        [PyRep::Dict(entries)] => {
            if entries.is_empty() {
                return invalid_state_err("Empty probe state dictionary".into());
            }
            for (k, v) in entries {
                let (Some(pid), Some(raw_st)) = (exact_int_arg(k), exact_int_arg(v)) else {
                    return invalid_state_err("Invalid probe state dictionary entry".into());
                };
                if pid <= 0 {
                    return invalid_state_err(format!("Invalid probe ID {pid}"));
                }
                let Ok(st) = i32::try_from(raw_st) else {
                    return invalid_state_err(format!("Invalid scanner probe state {raw_st}"));
                };
                per_probe_states.insert(pid, st);
            }
        }
        [first, second] => {
            let Some(raw_st) = exact_int_arg(second) else {
                return invalid_state_err("Invalid scanner probe state argument".into());
            };
            let Ok(st_i32) = i32::try_from(raw_st) else {
                return invalid_state_err(format!("Invalid scanner probe state {raw_st}"));
            };
            match first {
                PyRep::List(ids) | PyRep::Tuple(ids) => {
                    if ids.is_empty() {
                        return invalid_state_err("Empty probe ID list".into());
                    }
                    for id in ids {
                        let Some(pid) = exact_int_arg(id).filter(|&v| v > 0) else {
                            return invalid_state_err("Invalid probe ID in state list".into());
                        };
                        per_probe_states.insert(pid, st_i32);
                    }
                }
                _ => {
                    let Some(pid) = exact_int_arg(first).filter(|&v| v > 0) else {
                        return invalid_state_err("Invalid probe ID for SetActivityState".into());
                    };
                    per_probe_states.insert(pid, st_i32);
                }
            }
        }
        [first] => {
            let Some(raw_st) = exact_int_arg(first) else {
                return invalid_state_err("Invalid scanner probe state argument".into());
            };
            let Ok(st_i32) = i32::try_from(raw_st) else {
                return invalid_state_err(format!("Invalid scanner probe state {raw_st}"));
            };
            bulk_state = Some(st_i32);
        }
        [] => return invalid_state_err("Missing scanner probe state argument".into()),
        _ => {
            let msg = "SetActivityState accepts 1 or 2 arguments";
            return Some(Err(BridgeRpcError::with_client_error(
                "InvalidArguments",
                msg,
                eve_proto::ClientError::CustomInfo(msg.into()),
            )));
        }
    }

    let valid_state = |st: i32| {
        (PROBE_STATE_INACTIVE..=PROBE_STATE_RETURNING).contains(&st)
    };
    if let Some(st) = bulk_state {
        if !valid_state(st) {
            return Some(Err(BridgeRpcError {
                code: "InvalidProbeState".into(),
                message: format!("Invalid scanner probe state {st}"),
                client_error: None,
            }));
        }
    }
    for &st in per_probe_states.values() {
        if !valid_state(st) {
            return Some(Err(BridgeRpcError {
                code: "InvalidProbeState".into(),
                message: format!("Invalid scanner probe state {st}"),
                client_error: None,
            }));
        }
    }
    if let Err(e) = resolve_probe_space_context(ctx, session) {
        return Some(Err(e));
    }

    let mutation_res = {
        let mut map = ctx.active_scanner_probes.write().unwrap_or_else(|e| e.into_inner());
        apply_set_activity_state_updates(&mut map, char_id, sys_id, &per_probe_states, bulk_state)
    };
    let changed = match mutation_res {
        Ok(c) => c,
        Err(SetActivityStateMutationError::ProbeNotFound(pid)) => {
            return Some(Err(probe_rpc_err(
                "ProbeNotFound",
                format!("Scanner probe {pid} not found"),
            )));
        }
        Err(SetActivityStateMutationError::MaxProbesLaunched) => {
            return Some(Err(probe_rpc_err(
                "MaxProbesLaunched",
                "Maximum of 8 active scanner probes already in space",
            )));
        }
    };
    if !changed.is_empty() {
        sync_scanner_probes_to_db(ctx);
    }
    for (pid, st) in changed {
        let _ = ctx.emit_typed(
            crate::router::NotificationTarget::Character(char_id),
            crate::router::notify_registry::OnProbeStateChanged {
                probe_id: pid,
                probe_state: i64::from(st),
            },
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
    bound_entry: Option<&BoundObjectEntry>,
    service: &str,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (_, char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let caller_sys = resolve_caller_solar_system(ctx, session, char_id);
    let bound_sys = match verify_scan_mgr_bound(
        session,
        char_id,
        caller_sys,
        bound_entry,
        "query scan manager",
        CrossSystemScanCode::AccessDenied,
    ) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let arg_sys = match parse_optional_sys_arg(
        ctx,
        session,
        caller_sys,
        args,
        "query scan manager",
        CrossSystemScanCode::AccessDenied,
    ) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let is_gm = session.has_gm_role();
    let sys_id = if is_gm {
        arg_sys.or(bound_sys).unwrap_or(caller_sys)
    } else {
        arg_sys.unwrap_or(caller_sys)
    };
    if sys_id == 0 {
        let msg = "Session has no active solar system";
        return Some(Err(BridgeRpcError::with_client_error(
            "NoSolarSystem",
            msg,
            eve_proto::ClientError::CustomInfo(msg.into()),
        )));
    }
    let now_unix = ctx.now_unix();
    let is_undocked_with_ship = session.station_id.is_none()
        && session.structure_id.is_none()
        && session.ship_id.is_some();
    if is_undocked_with_ship {
        let _ = expire_overdue_probes_for_char(ctx, char_id, now_unix).await;
        prune_stale_pending_scans_for_char(ctx, char_id, session.session_id, None);
    }
    let (sigs, anomalies) =
        load_system_signatures_and_anomalies(ctx, session, sys_id, ctx.now_unix()).await;
    let bound_id = match ctx.bind_object(session, service, sys_id as i64) {
        Ok(b) => b,
        Err(e) => return Some(Err(e)),
    };
    let (wh_class, is_thera, is_shattered, env_effect) =
        system_wormhole_profile_sde(&ctx.sde, sys_id);

    let anom_ids: std::collections::HashSet<&str> =
        anomalies.iter().map(|a| a.anomaly_id.as_str()).collect();
    let sig_items: Vec<PyRep> = sigs
        .into_iter()
        .filter(|s| {
            !s.collapsed
                && !crate::services::fleet_social::fleet_social_scanning::is_anomaly_colliding_signature(
                    s,
                    &anom_ids,
                )
                && (s.lifetime_seconds == 0
                    || now_unix < s.created_unix.saturating_add(s.lifetime_seconds))
        })
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
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (_, char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let caller_sys = resolve_caller_solar_system(ctx, session, char_id);
    let bound_sys = match verify_scan_mgr_bound(
        session,
        char_id,
        caller_sys,
        bound_entry,
        "query anomalies",
        CrossSystemScanCode::AccessDenied,
    ) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let arg_sys = match parse_optional_sys_arg(
        ctx,
        session,
        caller_sys,
        args,
        "query anomalies",
        CrossSystemScanCode::AccessDenied,
    ) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let sys_id = if session.has_gm_role() {
        arg_sys.or(bound_sys).unwrap_or(caller_sys)
    } else {
        caller_sys
    };
    if sys_id == 0 {
        let msg = "Session has no active solar system";
        return Some(Err(BridgeRpcError::with_client_error(
            "NoSolarSystem",
            msg,
            eve_proto::ClientError::CustomInfo(msg.into()),
        )));
    }
    let now_unix = ctx.now_unix();
    let (_, anomalies) = load_system_signatures_and_anomalies_readonly(ctx, sys_id, now_unix).await;
    Some(Ok(PyRep::List(format_anomalies(anomalies, now_unix))))
}

type StagedProbeScanUpdate = (i64, Option<[f64; 3]>, Option<(i32, f64)>);

/// Validates and stages `scanMgr.RequestScans` probe updates without mutating `probe_map`.
fn stage_probe_scan_updates(
    ctx: &ServerContext,
    char_id: i64,
    sys_id: SolarSystemId,
    args: &[PyRep],
    override_range_step: Option<i32>,
    probe_map: &HashMap<i64, crate::router::ActiveScannerProbe>,
) -> Option<(Vec<StagedProbeScanUpdate>, Option<i32>)> {
    let mut staged_updates = Vec::new();
    let mut apply_bulk_step = None;

    if let Some(PyRep::Dict(entries)) = args.first() {
        let mut matched_owned = 0usize;
        for (k, v) in entries {
            let pid = exact_int_arg(k)?;
            if !matches!(v, PyRep::KeyVal(_) | PyRep::Dict(_)) {
                return None;
            }
            let probe_opt = match probe_map.get(&pid) {
                Some(probe) => {
                    if probe.owner_char_id != char_id
                        || probe.solar_system_id != sys_id
                        || probe.state <= 0
                    {
                        return None;
                    }
                    matched_owned += 1;
                    Some(probe)
                }
                None => None,
            };
            let base_au = probe_opt
                .map(|p| probe_base_scan_range_au(&ctx.sde, p.probe_type_id))
                .unwrap_or(0.25);
            let find_in_v = |key: &str| -> Option<&PyRep> {
                match v {
                    PyRep::KeyVal(kv) => kv.iter().find(|(ek, _)| *ek == key).map(|(_, ev)| ev),
                    PyRep::Dict(kv) => kv
                        .iter()
                        .find(|(ek, _)| ek.as_str() == Some(key))
                        .map(|(_, ev)| ev),
                    _ => None,
                }
            };
            let step_from_radius = |rad: f64, denom: f64| -> Option<(i32, f64)> {
                if !rad.is_finite() || rad <= 0.0 || !denom.is_finite() || denom <= 0.0 {
                    return None;
                }
                let ratio = (rad / denom).max(1.0);
                let step = if !ratio.is_finite() {
                    8
                } else {
                    (ratio.log2().round().clamp(0.0, 7.0) as i32) + 1
                };
                Some((step, base_au * AU_IN_METERS * 2.0_f64.powi(step - 1)))
            };
            let mut dest_opt: Option<[f64; 3]> = None;
            if let Some(dest_rep) = find_in_v("destination").or_else(|| find_in_v("pos")) {
                let (PyRep::Tuple(c) | PyRep::List(c)) = dest_rep else {
                    return None;
                };
                if c.len() < 3 {
                    return None;
                }
                let x = exact_float_arg(&c[0]).filter(|v| v.is_finite())?;
                let y = exact_float_arg(&c[1]).filter(|v| v.is_finite())?;
                let z = exact_float_arg(&c[2]).filter(|v| v.is_finite())?;
                dest_opt = Some([x, y, z]);
            } else if let (Some(rx), Some(ry), Some(rz)) =
                (find_in_v("x"), find_in_v("y"), find_in_v("z"))
            {
                let x = exact_float_arg(rx).filter(|v| v.is_finite())?;
                let y = exact_float_arg(ry).filter(|v| v.is_finite())?;
                let z = exact_float_arg(rz).filter(|v| v.is_finite())?;
                dest_opt = Some([x, y, z]);
            }
            let mut range_opt: Option<(i32, f64)> = None;
            if let Some(step_rep) = find_in_v("rangeStep") {
                let step = exact_int_arg(step_rep)?.clamp(1, 8) as i32;
                range_opt = Some((step, base_au * AU_IN_METERS * 2.0_f64.powi(step - 1)));
            } else if let Some(rad_rep) = find_in_v("scanRangeAU") {
                range_opt = Some(
                    exact_float_arg(rad_rep).and_then(|rad_au| step_from_radius(rad_au, base_au))?,
                );
            } else if let Some(rad_rep) = find_in_v("scanRange") {
                range_opt = Some(
                    exact_float_arg(rad_rep)
                        .and_then(|rad_m| step_from_radius(rad_m, base_au * AU_IN_METERS))?,
                );
            }
            if let Some(probe) = probe_opt {
                if dest_opt.is_some() || range_opt.is_some() {
                    staged_updates.push((probe.probe_id, dest_opt, range_opt));
                }
            }
        }
        if !entries.is_empty() && matched_owned == 0 {
            return None;
        }
    } else if let Some(PyRep::List(list)) = args.first() {
        let mut owner_probe_ids: Vec<i64> = probe_map
            .values()
            .filter(|p| p.owner_char_id == char_id && p.solar_system_id == sys_id && p.state > 0)
            .map(|p| p.probe_id)
            .collect();
        owner_probe_ids.sort_unstable();
        for (idx, item) in list.iter().enumerate() {
            let (PyRep::List(coords) | PyRep::Tuple(coords)) = item else {
                return None;
            };
            if coords.len() < 3 {
                return None;
            }
            let x = exact_float_arg(&coords[0]).filter(|v| v.is_finite())?;
            let y = exact_float_arg(&coords[1]).filter(|v| v.is_finite())?;
            let z = exact_float_arg(&coords[2]).filter(|v| v.is_finite())?;
            let step_opt = match coords.get(3) {
                None | Some(PyRep::None) => override_range_step,
                Some(a) => Some(exact_int_arg(a)?.clamp(1, 8) as i32),
            };
            if let Some(&pid) = owner_probe_ids.get(idx) {
                if let Some(probe) = probe_map.get(&pid) {
                    let base_au = probe_base_scan_range_au(&ctx.sde, probe.probe_type_id);
                    let range_opt =
                        step_opt.map(|step| (step, base_au * AU_IN_METERS * 2.0_f64.powi(step - 1)));
                    staged_updates.push((pid, Some([x, y, z]), range_opt));
                }
            }
        }
    } else {
        apply_bulk_step = override_range_step;
    }

    Some((staged_updates, apply_bulk_step))
}

fn is_invalid_first_scan_arg(args: &[PyRep]) -> bool {
    !matches!(
        args.first(),
        None | Some(PyRep::None | PyRep::Dict(_) | PyRep::List(_))
    )
}

fn invalid_probe_coords_err() -> BridgeRpcError {
    BridgeRpcError::user_error("InvalidProbeCoordinates")
}

fn no_active_probes_err() -> BridgeRpcError {
    let msg = "No active scanner probes in space";
    BridgeRpcError::with_client_error(
        "NoActiveProbes",
        msg,
        eve_proto::ClientError::CustomInfo(msg.into()),
    )
}

async fn handle_request_scans(
    ctx: &ServerContext,
    session: &mut SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (char_id, caller_sys) = match resolve_probe_space_context(ctx, session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let has_any_tracked_probes = ctx
        .active_scanner_probes
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .values()
        .any(|p| p.owner_char_id == char_id && p.state >= 0);
    if !has_any_tracked_probes && session.ship_id.is_none() {
        let msg = "Session has no active ship";
        return Some(Err(BridgeRpcError::with_client_error(
            "NoActiveShip",
            msg,
            eve_proto::ClientError::CustomInfo(msg.into()),
        )));
    }
    if !has_any_tracked_probes
        && bound_entry.is_none()
        && (args.is_empty() || matches!(args.first(), Some(PyRep::Real(_))))
        && kwargs.is_empty()
    {
        return None;
    }

    if let Err(e) = verify_scan_mgr_bound(
        session,
        char_id,
        caller_sys,
        bound_entry,
        "request probe scans",
        CrossSystemScanCode::AccessDenied,
    ) {
        return Some(Err(e));
    }
    if args.len() > 2 {
        let msg = "RequestScans accepts at most 2 arguments";
        return Some(Err(BridgeRpcError::with_client_error(
            "InvalidArguments",
            msg,
            eve_proto::ClientError::CustomInfo(msg.into()),
        )));
    }
    if is_invalid_first_scan_arg(args) {
        return Some(Err(invalid_probe_coords_err()));
    }
    let raw_range_step_arg = match args.get(1) {
        None | Some(PyRep::None) => None,
        Some(a) => match exact_int_arg(a) {
            Some(v) => Some(v),
            None => return Some(Err(invalid_probe_coords_err())),
        },
    };
    let raw_range_step_kw = match kwargs.get("rangeStep") {
        None | Some(PyRep::None) => None,
        Some(a) => match exact_int_arg(a) {
            Some(v) => Some(v),
            None => return Some(Err(invalid_probe_coords_err())),
        },
    };
    let override_range_step = raw_range_step_arg
        .or(raw_range_step_kw)
        .map(|v| v.clamp(1, 8) as i32);
    let sys_id = caller_sys;
    {
        let probe_map = ctx.active_scanner_probes.read().unwrap_or_else(|e| e.into_inner());
        let has_active_before = probe_map
            .values()
            .any(|p| p.owner_char_id == char_id && p.solar_system_id == sys_id && p.state > 0);
        if !has_active_before {
            return Some(Err(no_active_probes_err()));
        }
        if stage_probe_scan_updates(ctx, char_id, sys_id, args, override_range_step, &probe_map).is_none() {
            return Some(Err(invalid_probe_coords_err()));
        }
    }

    let now_unix = ctx.now_unix();
    if has_any_tracked_probes {
        let _ = expire_overdue_probes_for_char(ctx, char_id, now_unix).await;
    }
    let has_active_in_sys = ctx
        .active_scanner_probes
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .values()
        .any(|p| p.owner_char_id == char_id && p.solar_system_id == sys_id && p.state > 0);
    if !has_active_in_sys {
        return Some(Err(no_active_probes_err()));
    }

    let mut moved_balls: Vec<(i64, DVec3)> = Vec::new();
    {
        let mut probe_map = ctx.active_scanner_probes.write().unwrap_or_else(|e| e.into_inner());
        let Some((staged_updates, apply_bulk_step)) =
            stage_probe_scan_updates(ctx, char_id, sys_id, args, override_range_step, &probe_map)
        else {
            return Some(Err(invalid_probe_coords_err()));
        };
        if let Some(step) = apply_bulk_step {
            for p in probe_map.values_mut() {
                if p.owner_char_id == char_id && p.solar_system_id == sys_id && p.state > 0 {
                    let base_au = probe_base_scan_range_au(&ctx.sde, p.probe_type_id);
                    p.range_step = step;
                    p.scan_range_m = base_au * AU_IN_METERS * 2.0_f64.powi(step - 1);
                }
            }
        }
        for (pid, dest_opt, range_opt) in staged_updates {
            if let Some(probe) = probe_map.get_mut(&pid) {
                if let Some(dest) = dest_opt {
                    probe.destination = dest;
                    moved_balls.push((pid, DVec3::from_array(dest)));
                }
                if let Some((step, scan_range_m)) = range_opt {
                    probe.range_step = step;
                    probe.scan_range_m = scan_range_m;
                }
            }
        }
    }
    let sys_handle = ctx.systems.get_or_spawn(sys_id);

    let scan_ship_id_for_move = session.ship_id.or_else(|| {
        ctx.active_scanner_probes
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .find(|p| p.owner_char_id == char_id && p.solar_system_id == sys_id && p.state > 0)
            .map(|p| p.ship_id)
    });
    let move_fit_opt = scan_ship_id_for_move
        .filter(|&sid| sid > 0)
        .and_then(|sid| ctx.get_or_rebuild_ship_fitting(char_id, sid).ok());

    let (warp_ft, wall_warp_ft) = (ctx.now_system_sim_filetime(Some(sys_id)), ctx.now_blueos_filetime_ticks());
    let mut max_warp_ms: i64 = 0;
    for (pid, new_pos) in moved_balls {
        if let Ok(Some((_, b))) = sys_handle.get_ball_snapshot(pid).await {
            let old_pos = b.position;
            let dist = (old_pos - new_pos).length();
            if dist > 1.0 {
                let p_tid = ctx.active_scanner_probes.read().unwrap_or_else(|e| e.into_inner()).get(&pid).map(|p| p.probe_type_id).unwrap_or(30_013);
                let p_sde = ctx.sde.types.get(&p_tid);
                let p_dogma = evaluate_probe_dogma_state(move_fit_opt.as_ref(), pid, p_tid, &ctx.sde);
                let warp_au: f64 = p_dogma
                    .as_ref()
                    .and_then(|ps| {
                        let w600 = ps.get_attribute(600, &ctx.sde);
                        let w1281 = Some(ps.get_attribute(1281, &ctx.sde)).filter(|&m| m > 0.0).unwrap_or(1.0);
                        (w600 > 0.0).then_some(w600 * w1281)
                    })
                    .or_else(|| {
                        p_sde.map(|t| {
                            t.default_attributes.get(&600).copied().unwrap_or(15.0)
                                * t.default_attributes.get(&1281).copied().unwrap_or(1.0)
                        })
                    })
                    .unwrap_or(15.0)
                    .max(1.0);
                let max_vel = p_dogma
                    .as_ref()
                    .map(|ps| ps.get_attribute(37, &ctx.sde))
                    .filter(|&v| v > 0.0)
                    .or_else(|| p_sde.and_then(|t| t.default_attributes.get(&37).copied()))
                    .unwrap_or(3_000.0)
                    .max(500.0);
                if dist <= 150_000.0 {
                    let move_ms = ((dist / max_vel) * 1000.0).round().clamp(200.0, 5_000.0) as i64;
                    let p_end_ft = wall_warp_ft + move_ms * 10_000;
                    max_warp_ms = max_warp_ms.max(move_ms);
                    let _ = sys_handle.set_movement_mode(pid, eve_destiny::MovementMode::GotoDirection { direction: (new_pos - old_pos).normalize_or_zero() }).await;
                    let _ = sys_handle.set_speed_fraction(pid, 1.0).await;
                    let _ = ctx.emit_typed(
                        crate::router::NotificationTarget::Character(char_id),
                        crate::router::notify_registry::OnProbeStateChanged {
                            probe_id: pid,
                            probe_state: PROBE_STATE_MOVING as i64,
                        },
                    );
                    {
                        let mut probe_map = ctx.active_scanner_probes.write().unwrap_or_else(|e| e.into_inner());
                        if let Some(p) = probe_map.get_mut(&pid) { p.warp_end_ft = Some(p_end_ft); p.state = PROBE_STATE_MOVING; }
                    }
                } else {
                    let accel_ms = 50.0 * (dist / 1_000.0).max(1.0).ln();
                    let cruise_ms = (dist / (warp_au * AU_IN_METERS)) * 1000.0;
                    let warp_ms = (accel_ms + cruise_ms).round().clamp(500.0, 30_000.0) as i64;
                    let p_end_ft = wall_warp_ft + warp_ms * 10_000;
                    max_warp_ms = max_warp_ms.max(warp_ms);
                    let _ = sys_handle.set_movement_mode(pid, eve_destiny::MovementMode::new_warp(old_pos, new_pos, warp_au)).await;
                    let from_t = PyRep::Tuple(vec![PyRep::Real(old_pos.x), PyRep::Real(old_pos.y), PyRep::Real(old_pos.z)]);
                    let to_t = PyRep::Tuple(vec![PyRep::Real(new_pos.x), PyRep::Real(new_pos.y), PyRep::Real(new_pos.z)]);
                    let _ = ctx.emit_typed(
                        crate::router::NotificationTarget::Character(char_id),
                        crate::router::notify_registry::OnProbeStateChanged {
                            probe_id: pid,
                            probe_state: PROBE_STATE_WARPING as i64,
                        },
                    );
                    let _ = ctx.emit_typed(
                        crate::router::NotificationTarget::Character(char_id),
                        crate::router::notify_registry::OnProbeWarpStart {
                            probe_id: pid,
                            from_pos: from_t,
                            to_pos: to_t,
                            start_time: warp_ft,
                            duration: warp_ms,
                        },
                    );
                    {
                        let mut probe_map = ctx.active_scanner_probes.write().unwrap_or_else(|e| e.into_inner());
                        if let Some(p) = probe_map.get_mut(&pid) { p.warp_end_ft = Some(p_end_ft); p.state = PROBE_STATE_WARPING; }
                    }
                }
            }
        }
    }

    let active_probes: Vec<crate::router::ActiveScannerProbe> = ctx
        .active_scanner_probes
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .values()
        .filter(|p| p.owner_char_id == char_id && p.solar_system_id == sys_id && p.state > 0)
        .cloned()
        .collect();
    if active_probes.is_empty() {
        return Some(Err(BridgeRpcError { code: "NoActiveProbes".into(), message: "No active scanner probes in space".into(), client_error: None }));
    }
    if read_system_signatures_and_anomalies_ready(ctx, sys_id, now_unix).is_none() {
        let _ = ensure_system_signatures_and_anomalies(ctx, sys_id).await;
    }
    let balls = sys_handle.list_balls().await.unwrap_or_default();
    let inhibitor_positions = crate::services::space_destiny::space_cyno_filaments::active_scan_inhibitor_positions(ctx, sys_id, &balls);
    let caller_ship_id = session.ship_id.or_else(|| active_probes.first().map(|p| p.ship_id));
    if let Some(cb) = caller_ship_id.and_then(|csid| balls.iter().find(|b| b.id == csid)) {
        if inhibitor_positions.iter().any(|pos| cb.position.distance(*pos) <= 30_000.0) {
            sync_scanner_probes_to_db(ctx);
            return Some(Ok(PyRep::List(Vec::new())));
        }
    }

    let ship_id_for_scan = active_probes.first().map(|pr| pr.ship_id).or(session.ship_id).unwrap_or(0);
    let scan_fit_opt = (ship_id_for_scan > 0)
        .then(|| ctx.get_or_rebuild_ship_fitting(char_id, ship_id_for_scan).ok())
        .flatten();
    let (
        astro_lvl,
        real_rangefinding_lvl,
        legacy_rangefinding_lvl,
        real_pinpointing_lvl,
        legacy_pinpointing_lvl,
        fitted_scan_mods_1136,
    ) = ctx.db.with_snapshot(|snap| {
        let sk = snap.skills.get(&char_id);
        let a = sk.and_then(|m| m.get(&3412)).map(|s| s.level).unwrap_or(0);
        let rf_real = sk.and_then(|m| m.get(&25739)).map(|s| s.level).unwrap_or(0);
        let rf_legacy = sk.and_then(|m| m.get(&25810)).map(|s| s.level).unwrap_or(0);
        let pp_real = sk.and_then(|m| m.get(&25810)).map(|s| s.level).unwrap_or(0);
        let pp_legacy = sk.and_then(|m| m.get(&25811)).map(|s| s.level).unwrap_or(0);
        let mod_bonuses: Vec<(i64, f64)> = snap
            .items
            .values()
            .filter(|i| {
                i.location_id == ship_id_for_scan
                    && matches!(i.flag as u16, 11..=34 | 92..=94 | 125..=132)
                    && snap.module_online_states.get(&i.item_id).copied().unwrap_or(true)
            })
            .filter_map(|i| {
                let b = ctx.sde.types.get(&i.type_id)?.default_attributes.get(&1136).copied()?;
                Some((i.item_id, b))
            })
            .collect();
        (a, rf_real, rf_legacy, pp_real, pp_legacy, mod_bonuses)
    });
    let eff_rf_lvl = if real_rangefinding_lvl > 0 {
        real_rangefinding_lvl
    } else {
        legacy_rangefinding_lvl
    };
    let eff_pp_lvl = if real_pinpointing_lvl > 0 {
        real_pinpointing_lvl
    } else {
        legacy_pinpointing_lvl
    };
    let total_1136_pct: f64 = fitted_scan_mods_1136.iter().map(|&(_, b)| b).sum();
    let skill_mult = (1.0 + 0.05 * astro_lvl as f64)
        * (1.0 + 0.05 * eff_rf_lvl as f64)
        * (1.0 + total_1136_pct / 100.0);

    let eval_probe_sensor_and_dev =
        |probe: &crate::router::ActiveScannerProbe, range_au: f64, base_au: f64| -> (f64, f64) {
            let sde_base_sensor = probe_base_sensor_strength(&ctx.sde, probe.probe_type_id);
            let sde_base_dev = probe_base_max_deviation_au(&ctx.sde, probe.probe_type_id);
            if let Some(p_state) = evaluate_probe_dogma_state(
                scan_fit_opt.as_ref(),
                probe.probe_id,
                probe.probe_type_id,
                &ctx.sde,
            ) {
                let sensor = if p_state.base_attributes.contains_key(&1371)
                    || p_state.modifiers.iter().any(|m| m.target_attribute_id == 1371)
                {
                    let mut s = p_state.get_attribute(1371, &ctx.sde);
                    let has_3412 = p_state
                        .modifiers
                        .iter()
                        .any(|m| m.source_item_id == 3412 && m.target_attribute_id == 1371);
                    if !has_3412 && astro_lvl > 0 {
                        s *= 1.0 + 0.05 * (astro_lvl.min(5) as f64);
                    }
                    let has_25739 = p_state
                        .modifiers
                        .iter()
                        .any(|m| m.source_item_id == 25739 && m.target_attribute_id == 1371);
                    if !has_25739 && real_rangefinding_lvl == 0 && legacy_rangefinding_lvl > 0 {
                        s *= 1.0 + 0.05 * (legacy_rangefinding_lvl.min(5) as f64);
                    }
                    let unapplied_1136: f64 = fitted_scan_mods_1136
                        .iter()
                        .filter(|&&(mid, _)| {
                            !p_state
                                .modifiers
                                .iter()
                                .any(|m| m.source_item_id == mid && m.target_attribute_id == 1371)
                        })
                        .map(|&(_, b)| b)
                        .sum();
                    if unapplied_1136.abs() > 1e-9 {
                        s *= 1.0 + unapplied_1136 / 100.0;
                    }
                    s
                } else {
                    sde_base_sensor * skill_mult
                };

                let dev_au = if p_state.base_attributes.contains_key(&1372)
                    || p_state.modifiers.iter().any(|m| m.target_attribute_id == 1372)
                {
                    let mut d = p_state.get_attribute(1372, &ctx.sde);
                    let has_3412_dev = p_state
                        .modifiers
                        .iter()
                        .any(|m| m.source_item_id == 3412 && m.target_attribute_id == 1372);
                    if !has_3412_dev && astro_lvl > 0 {
                        d *= 1.0 - 0.05 * (astro_lvl.min(5) as f64);
                    }
                    let has_25810_dev = p_state
                        .modifiers
                        .iter()
                        .any(|m| m.source_item_id == 25810 && m.target_attribute_id == 1372);
                    if !has_25810_dev && real_pinpointing_lvl == 0 && legacy_pinpointing_lvl > 0 {
                        d *= 1.0 - 0.05 * (legacy_pinpointing_lvl.min(5) as f64);
                    }
                    d * (range_au / base_au.max(0.01))
                } else {
                    compute_probe_max_deviation_au(
                        sde_base_dev,
                        range_au,
                        base_au,
                        astro_lvl,
                        eff_pp_lvl,
                    )
                };
                return (sensor, dev_au);
            }
            (
                sde_base_sensor * skill_mult,
                compute_probe_max_deviation_au(sde_base_dev, range_au, base_au, astro_lvl, eff_pp_lvl),
            )
        };

    let first_probe = &active_probes[0];
    let lead_base_range_au = probe_base_scan_range_au(&ctx.sde, first_probe.probe_type_id);
    let lead_base_dev_au = probe_base_max_deviation_au(&ctx.sde, first_probe.probe_type_id);
    let lead_range_au = lead_base_range_au * 2.0_f64.powi(first_probe.range_step - 1);
    let (_, lead_max_dev_au) = eval_probe_sensor_and_dev(first_probe, lead_range_au, lead_base_range_au);
    let lead_max_dev_m = lead_max_dev_au * AU_IN_METERS;

    let scan_results = ctx.db.with_snapshot(|snap| {
        let mut out = Vec::new();
        let anom_ids: std::collections::HashSet<&str> = snap
            .cosmic_anomalies
            .get(&sys_id)
            .map(|list| list.iter().map(|a| a.anomaly_id.as_str()).collect())
            .unwrap_or_default();
        if let Some(sigs) = snap.cosmic_signatures.get(&sys_id) {
            for sig in sigs {
                if sig.collapsed
                    || crate::services::fleet_social::fleet_social_scanning::is_anomaly_colliding_signature(
                        sig,
                        &anom_ids,
                    )
                {
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
                        let p_range_au = base_au * 2.0_f64.powi(probe.range_step - 1);
                        let (dogma_sensor, dogma_dev_au) =
                            eval_probe_sensor_and_dev(probe, p_range_au, base_au);
                        let eff_probe_sensor = dogma_sensor * step_factor * dist_factor;
                        sum_probe_strength += eff_probe_sensor;
                        sum_covering_dev_au += dogma_dev_au;
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
        if resolved && ctx.mark_signature_scanned(char_id, sys_id, wh_item_id)
            && sig.sig_group.eq_ignore_ascii_case("Wormhole")
        {
            crate::quasar_gateway::isolate_side_effect("space_probe_scanning.career_wormhole", || {
                crate::services::quasar_career_goals::record_career_sub_kind(
                    ctx,
                    char_id,
                    "wormholes",
                    None,
                    1,
                );
            });
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
            ("scanGroupID", PyRep::Integer(3)),
            ("groupID", PyRep::Integer(502)),
            ("strengthAttributeID", PyRep::Integer(1908)),
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
                    sync_scanner_probes_to_db(ctx);
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
                let p_pos = DVec3::from_array(probe.destination);
                let dist = p_pos.distance(ball.position);
                let base_au = probe_base_scan_range_au(&ctx.sde, probe.probe_type_id);
                let radius = probe.scan_range_m.max(base_au * AU_IN_METERS);
                if dist <= radius {
                    covering += 1;
                    let step_factor = 0.5_f64.powi(probe.range_step - 1);
                    let dist_factor = (-(dist / radius).powi(2)).exp();
                    let p_range_au = base_au * 2.0_f64.powi(probe.range_step - 1);
                    let (dogma_sensor, _) = eval_probe_sensor_and_dev(probe, p_range_au, base_au);
                    let sde_sensor = probe_base_sensor_strength(&ctx.sde, probe.probe_type_id).max(1e-6);
                    let custom_scale = if probe.base_sensor_strength > 0.0 {
                        probe.base_sensor_strength / sde_sensor
                    } else {
                        1.0
                    };
                    total_s += dogma_sensor * custom_scale * step_factor * dist_factor * 2.0;
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
            let cmb_scan_group_id = match target_class {
                "Ship" => 4,
                "Structure" => 5,
                "Drone" => 6,
                "AbyssalTrace" => 14,
                _ => 3,
            };
            let mut cmb_kv = vec![
                ("id", PyRep::String(cmb_id.clone())),
                ("signatureID", PyRep::String(cmb_id)),
                ("itemID", PyRep::Integer(ball.id)),
                ("targetID", PyRep::Integer(ball.id)),
                ("targetClass", PyRep::String(if show_group { target_class.into() } else { "Unknown".into() })),
                ("scanGroupID", PyRep::Integer(cmb_scan_group_id)),
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
    let scan_probe_positions: Vec<(i64, [f64; 3])> = active_probes.iter().map(|p| (p.probe_id, p.destination)).collect();
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
    let scan_end_ft = wall_warp_ft + (max_warp_ms + scan_ms) * 10_000;
    if max_warp_ms == 0 {
        {
            let mut pm = ctx.active_scanner_probes.write().unwrap_or_else(|e| e.into_inner());
            for p in &active_probes { if let Some(mp) = pm.get_mut(&p.probe_id) { mp.state = PROBE_STATE_SCANNING; } }
        }
        for p in &active_probes {
            let _ = ctx.emit_typed(
                crate::router::NotificationTarget::Character(char_id),
                crate::router::notify_registry::OnProbeStateChanged {
                    probe_id: p.probe_id,
                    probe_state: PROBE_STATE_SCANNING as i64,
                },
            );
        }
    }
    sync_scanner_probes_to_db(ctx);
    let _ = ctx.emit_typed(
        crate::router::NotificationTarget::Character(char_id),
        crate::router::notify_registry::OnSystemScanStarted {
            start_time: warp_ft.saturating_add_ms(max_warp_ms.max(0) as u64),
            duration_ms: scan_ms,
            probes: probe_scan_started_probes_dict(&scan_probe_positions),
        },
    );
    let p_ids: Vec<i64> = active_probes.iter().map(|p| p.probe_id).collect();
    let scan_ctx = ctx.db.with_mut_snapshot(|snap| {
        let ch_st = char_space_session_state(snap, char_id);
        let ctx_rec = ActiveProbeScanContext {
            probe_ids: p_ids.clone(),
            sys_id,
            ship_id: caller_ship_id
                .or_else(|| ch_st.as_ref().map(|s| s.active_ship_id))
                .unwrap_or(0),
            system_epoch: ch_st.as_ref().map(|s| s.system_epoch).unwrap_or(0),
            online_session_id: ch_st.and_then(|s| s.online_session_id),
            rpc_session_id: session.session_id,
        };
        snap.fleet_and_social_ops.pending_probe_scans.push((
            char_id,
            p_ids,
            scan_probe_ids,
            rows.clone(),
            idle_probes,
            scan_end_ft,
        ));
        ctx_rec
    });
    record_active_probe_scan_context(ctx, char_id, scan_ctx);
    ctx.persist_server_runtime_to_db();
    Some(Ok(PyRep::List(rows)))
}

async fn handle_recover_probes(
    ctx: &ServerContext,
    session: &mut SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (char_id, caller_sys) = match resolve_probe_space_context(ctx, session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let bound_sys = match verify_scan_mgr_bound(
        session,
        char_id,
        caller_sys,
        bound_entry,
        "recover probes",
        CrossSystemScanCode::AccessDenied,
    ) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let sys_id = bound_sys.unwrap_or(caller_sys);

    if args.len() > 1 {
        let msg = "RecoverProbes accepts at most 1 argument";
        return Some(Err(BridgeRpcError::with_client_error(
            "InvalidArguments",
            msg,
            eve_proto::ClientError::CustomInfo(msg.into()),
        )));
    }
    let mut target_probe_ids: Vec<i64> = Vec::new();
    let mut explicit_empty_list = false;
    match args.first() {
        None | Some(PyRep::None) => {}
        Some(PyRep::List(list) | PyRep::Tuple(list)) => {
            for item in list {
                let Some(pid) = exact_int_arg(item).filter(|&v| v > 0) else {
                    let msg = "Invalid probe ID in RecoverProbes list";
                    return Some(Err(BridgeRpcError::with_client_error(
                        "InvalidArguments",
                        msg,
                        eve_proto::ClientError::CustomInfo(msg.into()),
                    )));
                };
                target_probe_ids.push(pid);
            }
            if list.is_empty() {
                explicit_empty_list = true;
            } else {
                let map = ctx.active_scanner_probes.read().unwrap_or_else(|e| e.into_inner());
                for &pid in &target_probe_ids {
                    if let Some(p) = map.get(&pid) {
                        if p.owner_char_id != char_id || p.solar_system_id != sys_id || p.state <= 0
                        {
                            let msg = format!("Scanner probe {pid} not found");
                            return Some(Err(BridgeRpcError::with_client_error(
                                "ProbeNotFound",
                                msg.clone(),
                                eve_proto::ClientError::CustomInfo(msg),
                            )));
                        }
                    }
                }
            }
        }
        Some(other) => {
            let Some(pid) = exact_int_arg(other).filter(|&v| v > 0) else {
                let msg = "Invalid probe ID argument for RecoverProbes";
                return Some(Err(BridgeRpcError::with_client_error(
                    "InvalidArguments",
                    msg,
                    eve_proto::ClientError::CustomInfo(msg.into()),
                )));
            };
            let found_owned = {
                let map = ctx.active_scanner_probes.read().unwrap_or_else(|e| e.into_inner());
                map.get(&pid)
                    .is_some_and(|p| p.owner_char_id == char_id && p.solar_system_id == sys_id && p.state > 0)
            };
            if !found_owned {
                let msg = format!("Scanner probe {pid} not found");
                return Some(Err(BridgeRpcError::with_client_error(
                    "ProbeNotFound",
                    msg.clone(),
                    eve_proto::ClientError::CustomInfo(msg),
                )));
            }
            target_probe_ids.push(pid);
        }
    }

    let now_unix = ctx.now_unix();
    if !explicit_empty_list {
        let _ = expire_overdue_probes_for_char(ctx, char_id, now_unix).await;
    }

    let candidate_probes: Vec<crate::router::ActiveScannerProbe> = if explicit_empty_list {
        Vec::new()
    } else {
        let mut map = ctx.active_scanner_probes.write().unwrap_or_else(|e| e.into_inner());
        let ids: Vec<i64> = map.values()
            .filter(|p| p.owner_char_id == char_id && p.solar_system_id == sys_id && p.state > 0 && (target_probe_ids.is_empty() || target_probe_ids.contains(&p.probe_id)))
            .map(|p| p.probe_id)
            .collect();
        ids.into_iter().filter_map(|id| map.remove(&id)).collect()
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

    let (recovered_probes, unrecovered_probes, item_diffs) = if !candidate_probes.is_empty() {
        ctx.db.with_mut_snapshot(|snap| {
            let (mut rec, mut unrec) = (Vec::new(), Vec::new());
            let mut touched_pre: HashMap<i64, Option<eve_db::InventoryItemRecord>> = HashMap::new();
            let mut add_stack = |items: &mut eve_db::CowMap<i64, eve_db::InventoryItemRecord>, next_id: &mut i64, ship_id: i64, flag: InventoryFlag, p_type_id: u32, qty: i32, exclude_id: i64| {
                let existing_id = items
                    .values()
                    .find(|i| i.owner_id == char_id && i.location_id == ship_id && i.flag == flag && i.item_id != exclude_id && i.type_id == p_type_id)
                    .map(|i| i.item_id);
                if let Some(st) = existing_id.and_then(|id| items.get_mut(&id)) {
                    touched_pre.entry(st.item_id).or_insert_with(|| Some(st.clone()));
                    st.quantity += qty;
                    st.version += 1;
                } else {
                    *next_id += 1;
                    let new_id = *next_id;
                    touched_pre.insert(new_id, None);
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
                if let Some(li) = snap
                    .items
                    .get(&launcher_id)
                    .cloned()
                    .filter(|i| i.location_id == ship_id && matches!(i.flag as u16, 27..=34))
                {
                    let cur_clip: i32 = snap.items.values()
                        .filter(|i| i.owner_id == char_id && i.location_id == ship_id && i.flag == li.flag && i.item_id != launcher_id)
                        .map(|i| i.quantity)
                        .sum();
                    let matching_clip = snap.items.values()
                        .any(|i| i.owner_id == char_id && i.location_id == ship_id && i.flag == li.flag && i.item_id != launcher_id && i.type_id == p_type_id);
                    if cur_clip == 0 || (cur_clip < 8 && matching_clip) {
                        add_stack(&mut snap.items, &mut snap.next_entity_id, ship_id, li.flag, p_type_id, 1, launcher_id);
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
                        rec.push(p);
                    } else {
                        unrec.push(p);
                    }
                }
            }
            let mut diffs: Vec<(Option<eve_db::InventoryItemRecord>, eve_db::InventoryItemRecord)> = touched_pre
                .into_iter()
                .filter_map(|(iid, pre_opt)| snap.items.get(&iid).cloned().map(|post| (pre_opt, post)))
                .collect();
            diffs.sort_by_key(|(_, post)| post.item_id);
            (rec, unrec, diffs)
        })
    } else {
        (Vec::new(), Vec::new(), Vec::new())
    };
    let mut recovered_into_launcher = false;
    if let Some(sid) = ship_id_opt {
        if !recovered_probes.is_empty() {
            ctx.invalidate_ship_fitting(sid);
        }
    }
    for (pre_opt, post) in &item_diffs {
        if matches!(post.flag as u16, 11..=34 | 125..=132)
            && post.quantity > 0
            && ctx.sde.types.get(&post.type_id).is_some_and(|t| t.category_id == 8)
        {
            if let Some(pre) = pre_opt {
                crate::services::inventory_fitting::fitting_module_cycles::emit_charge_sublocation_quantity_change(
                    ctx,
                    char_id,
                    post.location_id,
                    post.flag,
                    post.type_id,
                    pre.quantity,
                    post.quantity,
                );
            } else {
                crate::services::inventory_fitting::fitting_module_cycles::emit_godma_charge_sublocation_prime(
                    ctx,
                    char_id,
                    post.location_id,
                    post,
                );
                crate::services::inventory_fitting::fitting_module_cycles::emit_charge_sublocation_quantity_change(
                    ctx,
                    char_id,
                    post.location_id,
                    post.flag,
                    post.type_id,
                    0,
                    post.quantity,
                );
                recovered_into_launcher = true;
            }
        }
    }
    if recovered_into_launcher {
        ctx.emit_typed(
            crate::router::NotificationTarget::Character(char_id),
            crate::router::notify_registry::OnChargeLoadedToModule {},
        );
    }

    if !unrecovered_probes.is_empty() {
        let mut map = ctx.active_scanner_probes.write().unwrap_or_else(|e| e.into_inner());
        for p in unrecovered_probes {
            map.insert(p.probe_id, p);
        }
    }
    if !recovered_probes.is_empty() {
        let recovered_ids: Vec<i64> = recovered_probes.iter().map(|p| p.probe_id).collect();
        prune_probe_ids_from_pending_scans(ctx, char_id, &recovered_ids);
        sync_scanner_probes_to_db(ctx);
    }
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    for p in &recovered_probes {
        let _ = sys_handle.remove_ball(p.probe_id).await;
    }

    let rec_ids: Vec<PyRep> = recovered_probes.iter().map(|p| PyRep::Integer(p.probe_id)).collect();
    for p in &recovered_probes {
        let _ = ctx.emit_typed(
            crate::router::NotificationTarget::Character(char_id),
            crate::router::notify_registry::OnProbeStateChanged {
                probe_id: p.probe_id,
                probe_state: PROBE_STATE_RETURNING as i64,
            },
        );
    }
    if !recovered_probes.is_empty() {
        let idle_kvs: Vec<PyRep> = recovered_probes.iter().map(|p| PyRep::keyval([("probeID", PyRep::Integer(p.probe_id)), ("destination", PyRep::Tuple(vec![PyRep::Real(p.destination[0]), PyRep::Real(p.destination[1]), PyRep::Real(p.destination[2])]))])).collect();
        let _ = ctx.emit_typed(
            crate::router::NotificationTarget::Character(char_id),
            crate::router::notify_registry::OnProbesIdle { probes: idle_kvs },
        );
    }
    Some(Ok(PyRep::List(rec_ids)))
}

fn default_probe_scan_bonuses() -> PyRep {
    PyRep::Dict(vec![
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
    ])
}

fn build_probe_kv(probe: &crate::router::ActiveScannerProbe, expiry_ft: i64, scan_bonuses: &PyRep) -> PyRep {
    let pos_tuple = PyRep::Tuple(vec![
        PyRep::Real(probe.position[0]),
        PyRep::Real(probe.position[1]),
        PyRep::Real(probe.position[2]),
    ]);
    let dest_tuple = PyRep::Tuple(vec![
        PyRep::Real(probe.destination[0]),
        PyRep::Real(probe.destination[1]),
        PyRep::Real(probe.destination[2]),
    ]);
    PyRep::keyval([
        ("probeID", PyRep::Integer(probe.probe_id)),
        ("typeID", PyRep::Integer(probe.probe_type_id as i64)),
        ("scanRange", PyRep::Real(probe.scan_range_m)),
        ("expiry", PyRep::Integer(expiry_ft)),
        ("pos", pos_tuple),
        ("destination", dest_tuple),
        ("state", PyRep::Integer(i64::from(probe.state))),
        ("scanBonuses", scan_bonuses.clone()),
    ])
}

/// Emits `OnNewProbe` strictly before `OnProbeStateChanged` so V24.02 `scanSvc.probeTracker.AddProbe`
/// populates `probeData` before `UpdateProbeState` processes the state transition (GH #1121).
fn emit_probe_registered(ctx: &ServerContext, char_id: i64, probe_id: i64, probe_kv: PyRep, state: i32) {
    let _ = ctx.emit_typed(
        crate::router::NotificationTarget::Character(char_id),
        crate::router::notify_registry::OnNewProbe { probe: probe_kv },
    );
    let _ = ctx.emit_typed(
        crate::router::NotificationTarget::Character(char_id),
        crate::router::notify_registry::OnProbeStateChanged {
            probe_id,
            probe_state: i64::from(state),
        },
    );
}

async fn handle_reconnect_probes(
    ctx: &ServerContext,
    session: &mut SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (char_id, caller_sys) = match resolve_probe_space_context(ctx, session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let bound_sys = match verify_scan_mgr_bound(
        session,
        char_id,
        caller_sys,
        bound_entry,
        "reconnect lost probes",
        CrossSystemScanCode::AccessDenied,
    ) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    if !(args.is_empty() || matches!(args, [PyRep::None])) {
        let msg = "ReconnectToLostProbes does not accept arguments";
        return Some(Err(BridgeRpcError::with_client_error(
            "InvalidArguments",
            msg,
            eve_proto::ClientError::CustomInfo(msg.into()),
        )));
    }
    // Unbound or synthetic `object_id <= 0` calls fall back to the caller's active solar system.
    let sys_id = bound_sys.unwrap_or(caller_sys);
    let now_unix = ctx.now_unix();
    let (auto_returned, expired_lost) = expire_overdue_probes_for_char(ctx, char_id, now_unix).await;
    let ship_id = session.ship_id.unwrap_or(0);
    let mut reconnected_probes = Vec::new();
    {
        let mut map = ctx.active_scanner_probes.write().unwrap_or_else(|e| e.into_inner());
        let existing_active = map
            .values()
            .filter(|p| {
                p.owner_char_id == char_id
                    && p.solar_system_id == sys_id
                    && p.state > 0
                    && (ship_id == 0 || p.ship_id == ship_id)
            })
            .count();
        // Enforce EVE's 8-probe active deployment cap after counting probes already bound to this ship.
        let remaining_slots = 8usize.saturating_sub(existing_active);
        let mut candidate_ids: Vec<i64> = map
            .values()
            .filter(|p| {
                p.owner_char_id == char_id
                    && p.solar_system_id == sys_id
                    && (p.state == PROBE_STATE_INACTIVE || (ship_id != 0 && p.ship_id != ship_id && p.state > 0))
            })
            .map(|p| p.probe_id)
            .collect();
        candidate_ids.sort_unstable();
        candidate_ids.truncate(remaining_slots);
        reconnected_probes.reserve(candidate_ids.len());
        for pid in candidate_ids {
            if let Some(p) = map.get_mut(&pid) {
                p.state = PROBE_STATE_IDLE;
                // Clear any stale in-flight warp timer so reconnected probes remain idle until next scan.
                p.warp_end_ft = None;
                if ship_id != 0 { p.ship_id = ship_id; }
                reconnected_probes.push(p.clone());
            }
        }
    }
    if !reconnected_probes.is_empty() {
        sync_scanner_probes_to_db(ctx);
    }
    let now_ft = ctx.now_blueos_filetime_ticks();
    let scan_bonuses = default_probe_scan_bonuses();
    for probe in &reconnected_probes {
        let remaining_s = probe
            .launched_at_unix
            .saturating_add(probe.max_flight_time_s.round().max(0.0) as u64)
            .saturating_sub(now_unix);
        let expiry_ft = now_ft.saturating_add(
            i64::try_from(remaining_s)
                .unwrap_or(i64::MAX)
                .saturating_mul(10_000_000),
        );
        let probe_kv = build_probe_kv(probe, expiry_ft, &scan_bonuses);
        emit_probe_registered(ctx, char_id, probe.probe_id, probe_kv, PROBE_STATE_IDLE);
    }
    let reconnected_ids: Vec<i64> = reconnected_probes.iter().map(|p| p.probe_id).collect();
    let reconnected_set: HashSet<i64> = reconnected_ids.iter().copied().collect();
    prune_stale_pending_scans_for_char(
        ctx,
        char_id,
        session.session_id,
        (!reconnected_set.is_empty()).then_some(&reconnected_set),
    );
    let count = reconnected_ids.len() as i64;
    Some(Ok(PyRep::keyval([
        ("reconnectedProbeIDs", PyRep::List(reconnected_ids.into_iter().map(PyRep::Integer).collect())),
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
    let (char_id, caller_sys) = match resolve_probe_space_context(ctx, session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let bound_sys = match verify_scan_mgr_bound(
        session,
        char_id,
        caller_sys,
        bound_entry,
        "destroy scanner probe",
        CrossSystemScanCode::AccessDenied,
    ) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    if args.len() > 1 {
        let msg = "DestroyProbe accepts at most 1 argument";
        return Some(Err(BridgeRpcError::with_client_error(
            "InvalidArguments",
            msg,
            eve_proto::ClientError::CustomInfo(msg.into()),
        )));
    }
    let target_pid = match args.first() {
        None | Some(PyRep::None) => None,
        Some(arg) => match exact_int_arg(arg).filter(|&v| v > 0) {
            Some(pid) => Some(pid),
            None => {
                let msg = "Invalid probe ID argument for DestroyProbe";
                return Some(Err(BridgeRpcError::with_client_error(
                    "InvalidArguments",
                    msg,
                    eve_proto::ClientError::CustomInfo(msg.into()),
                )));
            }
        },
    };
    let sys_id = bound_sys.unwrap_or(caller_sys);
    if let Some(pid) = target_pid {
        enum ProbeTargetCheck {
            OwnedDestroyable,
            // Owned cargo/in-hangar sentinel (`state < 0`) must stay an idempotent 0-destroyed no-op (preserve cargo).
            OwnedCargoSentinel,
            RejectNotFound,
            NotInTracker,
        }
        let check = {
            let map = ctx.active_scanner_probes.read().unwrap_or_else(|e| e.into_inner());
            match map.get(&pid) {
                Some(p) if p.owner_char_id == char_id && p.solar_system_id == sys_id => {
                    if p.state >= 0 {
                        ProbeTargetCheck::OwnedDestroyable
                    } else {
                        ProbeTargetCheck::OwnedCargoSentinel
                    }
                }
                Some(_) => ProbeTargetCheck::RejectNotFound,
                None => ProbeTargetCheck::NotInTracker,
            }
        };
        match check {
            ProbeTargetCheck::OwnedDestroyable | ProbeTargetCheck::OwnedCargoSentinel => {}
            ProbeTargetCheck::RejectNotFound => {
                let msg = format!("Scanner probe {pid} not found");
                return Some(Err(BridgeRpcError::with_client_error(
                    "ProbeNotFound",
                    msg.clone(),
                    eve_proto::ClientError::CustomInfo(msg),
                )));
            }
            ProbeTargetCheck::NotInTracker => {
                // Reject known non-probe entities while keeping already-expired/returned probe IDs idempotent.
                let is_non_probe_entity = ctx.db.with_snapshot(|snap| {
                    snap.characters.get(&pid).is_some()
                        || snap.corporations.get(&pid).is_some()
                        || snap.structures.get(&pid).is_some()
                        || snap.items.get(&pid).is_some_and(|it| {
                            ctx.sde.types.get(&it.type_id).is_none_or(|t| t.group_id != 479)
                        })
                }) || ctx.sde.stations.contains_key(&pid)
                    || ctx.sde.stargate_meta.contains_key(&pid)
                    || u32::try_from(pid)
                        .is_ok_and(|id| ctx.sde.solar_systems.contains_key(&id));
                if is_non_probe_entity {
                    let msg = format!("Scanner probe {pid} not found");
                    return Some(Err(BridgeRpcError::with_client_error(
                        "ProbeNotFound",
                        msg.clone(),
                        eve_proto::ClientError::CustomInfo(msg),
                    )));
                }
            }
        }
    }
    let destroyed_probes: Vec<crate::router::ActiveScannerProbe> = {
        let mut map = ctx.active_scanner_probes.write().unwrap_or_else(|e| e.into_inner());
        let ids: Vec<i64> = map
            .values()
            .filter(|p| {
                p.owner_char_id == char_id
                    && p.solar_system_id == sys_id
                    && p.state >= 0
                    && (target_pid.is_none() || target_pid == Some(p.probe_id))
            })
            .map(|p| p.probe_id)
            .collect();
        ids.into_iter().filter_map(|id| map.remove(&id)).collect()
    };
    if !destroyed_probes.is_empty() {
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        for p in &destroyed_probes {
            let _ = sys_handle.remove_ball(p.probe_id).await;
            let _ = ctx.emit_typed(
                crate::router::NotificationTarget::Character(char_id),
                crate::router::notify_registry::OnRemoveProbe { probe_id: p.probe_id },
            );
        }
        let destroyed_ids: Vec<i64> = destroyed_probes.iter().map(|p| p.probe_id).collect();
        prune_probe_ids_from_pending_scans(ctx, char_id, &destroyed_ids);
        sync_scanner_probes_to_db(ctx);
        let any_left = ctx
            .active_scanner_probes
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .any(|p| p.owner_char_id == char_id && p.solar_system_id == sys_id && p.state >= 0);
        if !any_left {
            let _ = ctx.emit_typed(
                crate::router::NotificationTarget::Character(char_id),
                crate::router::notify_registry::OnScannerInfoRemoved {},
            );
        }
    }
    Some(Ok(PyRep::keyval([
        ("destroyedProbeID", destroyed_probes.first().map(|p| PyRep::Integer(p.probe_id)).unwrap_or(PyRep::None)),
        ("destroyedCount", PyRep::Integer(destroyed_probes.len() as i64)),
        ("destroyed", PyRep::Bool(!destroyed_probes.is_empty())),
    ])))
}

/// Emits `OnProbeWarpEnd`, `OnProbeStateChanged`, and `OnSystemScanStopped` as probe timers elapse.
#[derive(Clone, Debug)]
pub(crate) struct ActiveProbeScanContext {
    probe_ids: Vec<i64>,
    sys_id: u32,
    ship_id: i64,
    system_epoch: u64,
    online_session_id: Option<u64>,
    rpc_session_id: u64,
}

#[derive(Clone, Debug)]
struct CharSpaceSessionState {
    in_space: bool,
    solar_system_id: u32,
    active_ship_id: i64,
    system_epoch: u64,
    online_session_id: Option<u64>,
}

fn char_space_session_state(
    snap: &eve_db::WorldStateSnapshot,
    char_id: i64,
) -> Option<CharSpaceSessionState> {
    let ch = snap.characters.get(&char_id)?;
    let online_session_id = snap
        .sessions
        .values()
        .find(|rec| rec.character_id == Some(char_id))
        .map(|rec| rec.session_id);
    Some(CharSpaceSessionState {
        in_space: ch.station_id.is_none() && ch.structure_id.is_none(),
        solar_system_id: ch.solar_system_id,
        active_ship_id: ch.active_ship_id,
        system_epoch: ch.system_epoch,
        online_session_id,
    })
}

fn is_scan_context_valid_for_state(
    scan_ctx: &ActiveProbeScanContext,
    st: Option<&CharSpaceSessionState>,
) -> bool {
    let Some(st) = st else {
        return false;
    };
    st.in_space
        && st.solar_system_id == scan_ctx.sys_id
        && st.system_epoch == scan_ctx.system_epoch
        && (scan_ctx.ship_id == 0
            || st.active_ship_id == 0
            || st.active_ship_id == scan_ctx.ship_id)
        && st.online_session_id == scan_ctx.online_session_id
}

fn extract_probe_id_from_pyrep(rep: &PyRep) -> Option<i64> {
    rep.as_i64().or_else(|| match rep {
        PyRep::KeyVal(kv) => kv
            .iter()
            .find(|(k, _)| *k == "probeID")
            .and_then(|(_, v)| v.as_i64()),
        PyRep::Dict(kv) => kv
            .iter()
            .find(|(k, _)| k.as_str() == Some("probeID"))
            .and_then(|(_, v)| v.as_i64()),
        _ => None,
    })
}

fn record_active_probe_scan_context(
    ctx: &ServerContext,
    char_id: i64,
    scan_ctx: ActiveProbeScanContext,
) {
    ctx.active_probe_scan_contexts
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .entry(char_id)
        .or_default()
        .push(scan_ctx);
}

fn take_matching_scan_context(
    ctx: &ServerContext,
    char_id: i64,
    p_ids: &[i64],
) -> Option<ActiveProbeScanContext> {
    let mut guard = ctx.active_probe_scan_contexts.write().unwrap_or_else(|e| e.into_inner());
    let list = guard.get_mut(&char_id)?;
    let idx = list.iter().position(|sc| sc.probe_ids == p_ids).or_else(|| {
        if p_ids.is_empty() {
            None
        } else {
            list.iter()
                .position(|sc| p_ids.iter().all(|pid| sc.probe_ids.contains(pid)))
        }
    })?;
    let removed = list.remove(idx);
    if list.is_empty() {
        guard.remove(&char_id);
    }
    Some(removed)
}

fn prune_probe_ids_from_pending_scans(ctx: &ServerContext, char_id: i64, removed_ids: &[i64]) {
    if removed_ids.is_empty() {
        return;
    }
    let removed_set: HashSet<i64> = removed_ids.iter().copied().collect();
    {
        let mut guard = ctx.active_probe_scan_contexts.write().unwrap_or_else(|e| e.into_inner());
        if let Some(list) = guard.get_mut(&char_id) {
            for sc in list.iter_mut() {
                sc.probe_ids.retain(|id| !removed_set.contains(id));
            }
            list.retain(|sc| !sc.probe_ids.is_empty());
            if list.is_empty() {
                guard.remove(&char_id);
            }
        }
    }
    ctx.db.with_mut_snapshot(|snap| {
        let scans = &mut snap.fleet_and_social_ops.pending_probe_scans;
        for (cid, p_ids, scan_probe_ids, _rows, idle_probes, _end_ft) in scans.iter_mut() {
            if *cid != char_id {
                continue;
            }
            p_ids.retain(|id| !removed_set.contains(id));
            scan_probe_ids.retain(|rep| {
                !extract_probe_id_from_pyrep(rep).is_some_and(|id| removed_set.contains(&id))
            });
            idle_probes.retain(|rep| {
                !extract_probe_id_from_pyrep(rep).is_some_and(|id| removed_set.contains(&id))
            });
        }
        scans.retain(|(cid, p_ids, ..)| *cid != char_id || !p_ids.is_empty());
    });
}

fn prune_stale_pending_scans_for_char(
    ctx: &ServerContext,
    char_id: i64,
    session_id: u64,
    clear_overlapping_ids: Option<&HashSet<i64>>,
) {
    let cur_state = ctx
        .db
        .with_snapshot(|snap| char_space_session_state(snap, char_id));
    let has_db_scans = ctx.db.with_snapshot(|snap| {
        snap.fleet_and_social_ops
            .pending_probe_scans
            .iter()
            .any(|(cid, ..)| *cid == char_id)
    });
    let has_ctx_scans = ctx.active_probe_scan_contexts.read().unwrap_or_else(|e| e.into_inner()).contains_key(&char_id);
    if !has_db_scans && !has_ctx_scans {
        return;
    }

    let live_probe_ids: HashSet<i64> = ctx.active_scanner_probes.read().unwrap_or_else(|e| e.into_inner())
        .values().filter(|p| p.owner_char_id == char_id && is_probe_live_for_space_state(p, cur_state.as_ref())).map(|p| p.probe_id).collect();

    let mut valid_scan_probe_sets: Vec<Vec<i64>> = Vec::new();
    {
        let mut guard = ctx.active_probe_scan_contexts.write().unwrap_or_else(|e| e.into_inner());
        if let Some(list) = guard.get_mut(&char_id) {
            list.retain(|sc| {
                let overlaps_cleared = clear_overlapping_ids
                    .is_some_and(|ids| sc.probe_ids.iter().any(|pid| ids.contains(pid)));
                let has_live_probe = sc.probe_ids.iter().any(|pid| live_probe_ids.contains(pid));
                !overlaps_cleared
                    && has_live_probe
                    && sc.rpc_session_id == session_id
                    && is_scan_context_valid_for_state(sc, cur_state.as_ref())
            });
            valid_scan_probe_sets = list.iter().map(|sc| sc.probe_ids.clone()).collect();
            if list.is_empty() {
                guard.remove(&char_id);
            }
        }
    }

    let mut pruned_probe_ids: HashSet<i64> = HashSet::new();
    if has_db_scans {
        ctx.db.with_mut_snapshot(|snap| {
            let mut remaining_valid = valid_scan_probe_sets;
            snap.fleet_and_social_ops
                .pending_probe_scans
                .retain(|(cid, p_ids, ..)| {
                    if *cid != char_id {
                        return true;
                    }
                    let overlaps_cleared = clear_overlapping_ids
                        .is_some_and(|ids| p_ids.iter().any(|pid| ids.contains(pid)));
                    if !overlaps_cleared {
                        if let Some(pos) = remaining_valid.iter().position(|vp| vp == p_ids) {
                            remaining_valid.remove(pos);
                            return true;
                        }
                    }
                    pruned_probe_ids.extend(p_ids.iter().copied());
                    false
                });
        });
    }

    if !pruned_probe_ids.is_empty() {
        let mut changed = false;
        {
            let mut probe_map = ctx.active_scanner_probes.write().unwrap_or_else(|e| e.into_inner());
            for pid in pruned_probe_ids {
                if let Some(p) = probe_map.get_mut(&pid) {
                    if p.owner_char_id == char_id {
                        if p.warp_end_ft.take().is_some() {
                            changed = true;
                        }
                        if matches!(
                            p.state,
                            PROBE_STATE_SCANNING | PROBE_STATE_WARPING | PROBE_STATE_MOVING
                        ) {
                            p.position = p.destination;
                            p.state = PROBE_STATE_IDLE;
                            changed = true;
                        }
                    }
                }
            }
        }
        if changed {
            sync_scanner_probes_to_db(ctx);
        }
    }
}

/// Returns true if `p` is in an active in-space state belonging to the owner's current ship/system.
fn is_probe_live_for_space_state(
    p: &crate::router::ActiveScannerProbe,
    st_opt: Option<&CharSpaceSessionState>,
) -> bool {
    matches!(
        p.state,
        PROBE_STATE_IDLE | PROBE_STATE_MOVING | PROBE_STATE_WARPING | PROBE_STATE_SCANNING
    ) && st_opt.is_some_and(|st| {
        st.in_space
            && p.solar_system_id == st.solar_system_id
            && (p.ship_id == 0 || st.active_ship_id == 0 || st.active_ship_id == p.ship_id)
    })
}

/// Transitions due warping/moving probes whose owner is still in space in the same session,
/// while silently clearing stale `warp_end_ft` timers for detached/inactive probes.
fn process_due_probe_arrivals(ctx: &ServerContext, now_ft: i64) -> bool {
    let arrival_char_ids: HashSet<i64> = ctx.active_scanner_probes.read().unwrap_or_else(|e| e.into_inner())
        .values().filter(|p| p.warp_end_ft.is_some_and(|end_ft| now_ft >= end_ft)).map(|p| p.owner_char_id).collect();
    if arrival_char_ids.is_empty() {
        return false;
    }

    let char_states: HashMap<i64, CharSpaceSessionState> = ctx.db.with_snapshot(|snap| {
        arrival_char_ids
            .iter()
            .filter_map(|&cid| char_space_session_state(snap, cid).map(|st| (cid, st)))
            .collect()
    });
    let scan_ctxs_by_char: HashMap<i64, Vec<ActiveProbeScanContext>> = {
        let guard = ctx.active_probe_scan_contexts.read().unwrap_or_else(|e| e.into_inner());
        arrival_char_ids.iter().filter_map(|&cid| guard.get(&cid).map(|list| (cid, list.clone()))).collect()
    };

    let mut due_arrivals = Vec::new();
    let mut cleared_any_warp_end = false;
    {
        let mut probe_map = ctx.active_scanner_probes.write().unwrap_or_else(|e| e.into_inner());
        for p in probe_map.values_mut() {
            if !p.warp_end_ft.is_some_and(|end_ft| now_ft >= end_ft) {
                continue;
            }
            p.warp_end_ft = None;
            cleared_any_warp_end = true;
            let st_opt = char_states.get(&p.owner_char_id);
            let in_active_space = is_probe_live_for_space_state(p, st_opt);
            let ctx_allows = match scan_ctxs_by_char.get(&p.owner_char_id) {
                Some(ctxs) => {
                    let matching: Vec<&ActiveProbeScanContext> = ctxs
                        .iter()
                        .filter(|sc| sc.probe_ids.contains(&p.probe_id))
                        .collect();
                    matching.is_empty()
                        || matching
                            .into_iter()
                            .any(|sc| is_scan_context_valid_for_state(sc, st_opt))
                }
                None => true,
            };
            if in_active_space && ctx_allows {
                let was_warping = p.state == PROBE_STATE_WARPING;
                p.position = p.destination;
                p.state = PROBE_STATE_SCANNING;
                due_arrivals.push((
                    p.owner_char_id,
                    p.solar_system_id,
                    p.probe_id,
                    p.destination,
                    was_warping,
                ));
            }
        }
    }

    for (char_id, sys_id, pid, dest, was_warping) in due_arrivals {
        let sys_h = ctx.systems.get_or_spawn(sys_id);
        sys_h.try_set_ball_position(pid, DVec3::from_array(dest));
        sys_h.try_stop_ball(pid);
        if was_warping {
            crate::services::notify_emitters_tournaments_anomalies::emit_probe_warp_end(
                ctx, char_id, pid,
            );
        }
        let _ = ctx.emit_typed(
            crate::router::NotificationTarget::Character(char_id),
            crate::router::notify_registry::OnProbeStateChanged {
                probe_id: pid,
                probe_state: PROBE_STATE_SCANNING as i64,
            },
        );
    }
    cleared_any_warp_end
}

fn ensure_scan_result_wire_fields(rep: PyRep) -> PyRep {
    let PyRep::KeyVal(mut entries) = rep else {
        return rep;
    };
    entries
        .entry("groupID".to_string())
        .or_insert(PyRep::Integer(502));
    entries
        .entry("typeID".to_string())
        .or_insert(PyRep::None);
    if matches!(entries.get("data"), None | Some(PyRep::None)) {
        let coord = |k: &str| match entries.get(k) {
            Some(PyRep::Real(v)) => *v,
            Some(PyRep::Integer(v)) => *v as f64,
            _ => 0.0,
        };
        entries.insert(
            "data".to_string(),
            PyRep::Tuple(vec![
                PyRep::Real(coord("x")),
                PyRep::Real(coord("y")),
                PyRep::Real(coord("z")),
            ]),
        );
    }
    PyRep::KeyVal(entries)
}

/// Finalizes due `pending_probe_scans`, suppressing client notifications when the scan originated
/// from a prior server instance, system, epoch, or client session (#1334).
fn process_due_probe_scans(ctx: &ServerContext, now_ft: i64) -> bool {
    let has_due_scans = ctx.db.with_snapshot(|snap| {
        snap.fleet_and_social_ops
            .pending_probe_scans
            .iter()
            .any(|(_, _, _, _, _, end_ft)| now_ft >= *end_ft)
    });
    if !has_due_scans {
        return false;
    }

    let due_scans = ctx.db.with_mut_snapshot(|snap| {
        let all = std::mem::take(&mut snap.fleet_and_social_ops.pending_probe_scans);
        let (done, rem): (Vec<_>, Vec<_>) = all
            .into_iter()
            .partition(|(_, _, _, _, _, end_ft)| now_ft >= *end_ft);
        snap.fleet_and_social_ops.pending_probe_scans = rem;
        done
    });
    if due_scans.is_empty() {
        return false;
    }

    let due_char_ids: HashSet<i64> = due_scans.iter().map(|(cid, ..)| *cid).collect();
    let char_states: HashMap<i64, CharSpaceSessionState> = ctx.db.with_snapshot(|snap| {
        due_char_ids
            .iter()
            .filter_map(|&cid| char_space_session_state(snap, cid).map(|st| (cid, st)))
            .collect()
    });
    for (char_id, p_ids, scan_probe_ids, rows, idle_probes, _) in due_scans {
        let scan_ctx_opt = take_matching_scan_context(ctx, char_id, &p_ids);
        let st_opt = char_states.get(&char_id);
        let session_valid = scan_ctx_opt
            .as_ref()
            .is_some_and(|sc| is_scan_context_valid_for_state(sc, st_opt));

        let mut live_p_ids = Vec::new();
        {
            let mut probe_map = ctx.active_scanner_probes.write().unwrap_or_else(|e| e.into_inner());
            for &pid in &p_ids {
                if let Some(p) = probe_map.get_mut(&pid) {
                    let probe_still_live = session_valid
                        && p.owner_char_id == char_id
                        && is_probe_live_for_space_state(p, st_opt);
                    if probe_still_live {
                        live_p_ids.push(pid);
                    }
                    p.warp_end_ft = None;
                    p.position = p.destination;
                    if matches!(
                        p.state,
                        PROBE_STATE_SCANNING | PROBE_STATE_WARPING | PROBE_STATE_MOVING
                    ) {
                        p.state = PROBE_STATE_IDLE;
                    }
                }
            }
        }

        if live_p_ids.is_empty() {
            continue;
        }
        let live_set: HashSet<i64> = live_p_ids.iter().copied().collect();
        let filtered_scan_probe_ids: Vec<PyRep> = scan_probe_ids
            .into_iter()
            .filter(|rep| {
                extract_probe_id_from_pyrep(rep).is_some_and(|id| live_set.contains(&id))
            })
            .collect();
        let filtered_idle_probes: Vec<PyRep> = idle_probes
            .into_iter()
            .filter(|rep| {
                extract_probe_id_from_pyrep(rep).is_some_and(|id| live_set.contains(&id))
            })
            .collect();

        for pid in live_p_ids {
            let _ = ctx.emit_typed(
                crate::router::NotificationTarget::Character(char_id),
                crate::router::notify_registry::OnProbeStateChanged {
                    probe_id: pid,
                    probe_state: PROBE_STATE_IDLE as i64,
                },
            );
        }

        let wire_results: Vec<PyRep> = rows
            .into_iter()
            .map(ensure_scan_result_wire_fields)
            .collect();
        let _ = ctx.emit_typed(
            crate::router::NotificationTarget::Character(char_id),
            crate::router::notify_registry::OnSystemScanStopped {
                probes: filtered_scan_probe_ids,
                results: wire_results,
                absent_targets: Vec::new(),
            },
        );
        if !filtered_idle_probes.is_empty() {
            let _ = ctx.emit_typed(
                crate::router::NotificationTarget::Character(char_id),
                crate::router::notify_registry::OnProbesIdle {
                    probes: filtered_idle_probes,
                },
            );
        }
    }
    true
}

pub fn tick_probe_warp_end(ctx: &ServerContext) {
    let now_ft = ctx.now_blueos_filetime_ticks();
    let had_arrivals = process_due_probe_arrivals(ctx, now_ft);
    let had_due_scans = process_due_probe_scans(ctx, now_ft);
    if had_arrivals || had_due_scans {
        sync_scanner_probes_to_db(ctx);
    }
}

/// Persists non-expired tracked scanner probes (`state >= 0`) to `Sp6PersistedState`.
/// Locks `db` before `active_scanner_probes.read()` to prevent stale out-of-order overwrites.
pub(crate) fn sync_scanner_probes_to_db(ctx: &ServerContext) {
    let probes_differ = ctx.db.with_snapshot(|snap| {
        let map = ctx.active_scanner_probes.read().unwrap_or_else(|e| e.into_inner());
        let persisted = &snap.sp6_lifecycle.active_scanner_probes;
        let live_count = map.values().filter(|p| p.state >= 0).count();
        persisted.len() != live_count
            || map
                .iter()
                .filter(|(_, p)| p.state >= 0)
                .any(|(id, p)| persisted.get(id) != Some(p))
    });
    if probes_differ {
        crate::sp6_lifecycle::with_mut_sp6_state(&ctx.db, |st| {
            let map = ctx.active_scanner_probes.read().unwrap_or_else(|e| e.into_inner());
            st.active_scanner_probes = map
                .iter()
                .filter(|(_, p)| p.state >= 0)
                .map(|(&id, p)| (id, p.clone()))
                .collect();
        });
        ctx.persist_server_runtime_to_db();
    }
}

use crate::router::rpc_registry::{rpc_entry, RpcBoxFuture, RpcDispatchHandler, RpcEntry, RpcSceneClass};

fn dispatch_rpc<'a>(
    ctx: &'a ServerContext,
    session: &'a mut SessionState,
    bound_entry: Option<&'a BoundObjectEntry>,
    service: &'a str,
    method: &'a str,
    args: &'a [PyRep],
    kwargs: &'a HashMap<String, PyRep>,
) -> RpcBoxFuture<'a> {
    Box::pin(handle_scan_and_wormhole_extras(
        ctx,
        session,
        bound_entry,
        service,
        method,
        args,
        kwargs,
    ))
}

fn dispatch_request_scans<'a>(
    ctx: &'a ServerContext,
    session: &'a mut SessionState,
    bound_entry: Option<&'a BoundObjectEntry>,
    service: &'a str,
    method: &'a str,
    args: &'a [PyRep],
    kwargs: &'a HashMap<String, PyRep>,
) -> RpcBoxFuture<'a> {
    Box::pin(async move {
        if let Some(res) = handle_scan_and_wormhole_extras(
            ctx,
            session,
            bound_entry,
            service,
            method,
            args,
            kwargs,
        )
        .await
        {
            return Some(res);
        }
        if let Some(res) =
            crate::services::fleet_social::fleet_social_scanning::handle_scanning_and_wormholes(
                ctx, session, service, method, args, kwargs,
            )
            .await
        {
            return Some(res);
        }
        if is_invalid_first_scan_arg(args) {
            return Some(Err(invalid_probe_coords_err()));
        }
        Some(Err(no_active_probes_err()))
    })
}

pub(crate) const RPCS: &[RpcEntry] = &[
    rpc_entry("scanMgr", "RequestScans", RpcDispatchHandler::BoundAsync(dispatch_request_scans)).with_scene(RpcSceneClass::InSpace),
    rpc_entry("scanMgr", "GetSystemScanMgr", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("scanMgr", "GetAnomalies", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("scanMgr", "RecoverProbes", RpcDispatchHandler::BoundAsync(dispatch_rpc)).with_scene(RpcSceneClass::InSpace),
    rpc_entry("scanMgr", "ReconnectToLostProbes", RpcDispatchHandler::BoundAsync(dispatch_rpc)).with_scene(RpcSceneClass::InSpace),
    rpc_entry("scanMgr", "DestroyProbe", RpcDispatchHandler::BoundAsync(dispatch_rpc)).with_scene(RpcSceneClass::InSpace),
    rpc_entry("scanMgr", "GetFullState", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("scanMgr", "QAOverrideProbeExpiry", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("scanMgr", "QAScanSites", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("scanMgr", "SetActivityState", RpcDispatchHandler::BoundAsync(dispatch_rpc)).with_scene(RpcSceneClass::InSpace),
];

#[cfg(test)]
#[path = "space_probe_scanning_edges2_tests.rs"]
mod space_probe_scanning_edges2_tests;
