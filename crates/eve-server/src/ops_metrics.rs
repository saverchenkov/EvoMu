//! Loopback-only `--gm` ops snapshot & HTTP endpoint (`#1077 §A`).
//!
//! Serves cluster TiDi state, scheduler sleep/hibernation reasons, RPC handler latency
//! histograms, recent `[PacketPerf]` warnings, and the living-world placeholder status.

#![forbid(unsafe_code)]

use crate::router::context::ServerContext;
use crate::sso::HttpResponse;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::Ordering;

fn is_loopback_host_header(host: &str) -> bool {
    let trimmed = host.trim();
    if trimmed.is_empty() {
        return false;
    }
    let host_only = if let Some(rest) = trimmed.strip_prefix('[') {
        rest.split(']').next().unwrap_or("")
    } else {
        trimmed.split(':').next().unwrap_or("")
    };
    if host_only.eq_ignore_ascii_case("localhost") {
        return true;
    }
    host_only
        .parse::<IpAddr>()
        .is_ok_and(|ip| ip.is_loopback())
}

fn is_loopback_request(peer_ip: Option<IpAddr>, headers: &HashMap<String, String>) -> bool {
    if let Some(ip) = peer_ip {
        if !ip.is_loopback() {
            return false;
        }
    }
    if let Some(xff) = headers.get("x-forwarded-for") {
        let first = xff.split(',').next().unwrap_or("").trim();
        if !first.is_empty()
            && !first
                .parse::<IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
        {
            return false;
        }
    }
    if let Some(xri) = headers.get("x-real-ip") {
        let trimmed = xri.trim();
        if !trimmed.is_empty()
            && !trimmed
                .parse::<IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
        {
            return false;
        }
    }
    if let Some(host) = headers.get("host") {
        if !is_loopback_host_header(host) {
            return false;
        }
    }
    true
}

/// Builds the full `--gm` ops observability JSON snapshot (`#1077 §A`).
pub fn build_ops_snapshot(ctx: &ServerContext) -> Value {
    let flags = ctx.feature_flags_snapshot();
    let clock_paused = ctx.is_clock_paused();
    let dedicated_physics_ticker_active = ctx
        .dedicated_physics_ticker_active
        .load(Ordering::Relaxed);

    let (
        auto_tidi_enabled,
        node_load_tidi_threshold,
        manual_tidi_overrides,
        sim_clock_anchoring_count,
    ) = ctx
        .systems
        .cluster_nodes
        .read()
        .map(|cn| {
            (
                cn.auto_tidi_enabled,
                cn.node_load_tidi_threshold,
                cn.manual_tidi_overrides.clone(),
                cn.sim_clock_anchoring.len(),
            )
        })
        .unwrap_or((false, 0, HashMap::new(), 0));

    let mut handles = ctx.systems.active_system_handles();
    handles.sort_by_key(|(sys_id, _)| *sys_id);

    let mut active_systems = 0usize;
    let mut hibernated_systems = 0usize;
    let mut system_tidi = Vec::with_capacity(handles.len());
    let mut scheduler_systems = Vec::with_capacity(handles.len());

    for (sys_id, handle) in &handles {
        let tidi = handle.tidi_factor();
        let hibernated = handle.is_hibernated();
        let idle_ticks = handle.idle_ticks();
        let hibernated_since_unix = handle.hibernated_since_unix();
        if hibernated {
            hibernated_systems += 1;
        } else {
            active_systems += 1;
        }
        let sleep_reason = if clock_paused {
            "clock_paused"
        } else if hibernated {
            "hibernated_empty_idle"
        } else if idle_ticks > 0 {
            "idle_waiting_hibernation_threshold"
        } else {
            "active"
        };
        system_tidi.push(json!({
            "solar_system_id": sys_id,
            "tidi_factor": tidi,
            "manual_override": manual_tidi_overrides.get(sys_id).copied(),
        }));
        scheduler_systems.push(json!({
            "solar_system_id": sys_id,
            "hibernated": hibernated,
            "idle_ticks": idle_ticks,
            "hibernated_since_unix": hibernated_since_unix,
            "sleep_reason": sleep_reason,
        }));
    }

    let mut sorted_overrides: Vec<Value> = manual_tidi_overrides
        .iter()
        .map(|(sys_id, factor)| json!({ "solar_system_id": sys_id, "factor": factor }))
        .collect();
    sorted_overrides.sort_by_key(|v| v["solar_system_id"].as_u64().unwrap_or(0));

    let latency_histograms = ctx.flight_recorder.latency_histogram_report();
    let recent_packet_perf = ctx.flight_recorder.recent_packet_perf_warnings(50);
    let sequencer = ctx.flight_recorder.sequencer_observability_summary(50);
    let telemetry = crate::ops_telemetry::collect_ops_telemetry_snapshot(ctx);

    json!({
        "gm_mode": true,
        "tidi": {
            "auto_tidi_enabled": auto_tidi_enabled,
            "node_load_tidi_threshold": node_load_tidi_threshold,
            "sim_clock_anchoring_count": sim_clock_anchoring_count,
            "manual_overrides": sorted_overrides,
            "systems": system_tidi,
        },
        "scheduler": {
            "clock_paused": clock_paused,
            "dedicated_physics_ticker_active": dedicated_physics_ticker_active,
            "active_systems": active_systems,
            "hibernated_systems": hibernated_systems,
            "systems": scheduler_systems,
        },
        "latency_histograms": latency_histograms,
        "recent_packet_perf": recent_packet_perf,
        "sequencer": sequencer,
        "last_ticks": telemetry.last_ticks,
        "per_system_sessions": telemetry.per_system_sessions,
        "warn_suppressed": telemetry.warn_suppressed,
        "recent_bootstrap_timings": telemetry.recent_bootstrap_timings,
        "living_world": {
            "status": "not_implemented",
            "enabled": false,
            "feature_flag_enabled": flags.enable_living_world,
            "reason": "living-world simulation (LW-1..8) is not implemented"
        }
    })
}

/// Handles `/ops`, `/ops/status`, and `/ops/metrics` HTTP requests (`#1077 §A`).
pub fn handle_ops_http_request(
    ctx: &ServerContext,
    method: &str,
    path: &str,
    headers: &HashMap<String, String>,
    peer_ip: Option<IpAddr>,
) -> HttpResponse {
    if !ctx.is_gm_ops_enabled() {
        return HttpResponse::json(
            403,
            "Forbidden",
            json!({
                "error": "forbidden",
                "error_description": "Ops endpoint requires --gm mode"
            }),
        );
    }
    if !is_loopback_request(peer_ip, headers) {
        return HttpResponse::json(
            403,
            "Forbidden",
            json!({
                "error": "forbidden",
                "error_description": "Ops endpoint is restricted to loopback requests"
            }),
        );
    }
    if method != "GET" {
        return HttpResponse::json(
            405,
            "Method Not Allowed",
            json!({
                "error": "method_not_allowed",
                "error_description": "Ops endpoint supports GET only"
            }),
        );
    }
    match path {
        "/ops" | "/ops/status" | "/ops/metrics" => {
            HttpResponse::json(200, "OK", build_ops_snapshot(ctx))
        }
        _ => HttpResponse::json(
            404,
            "Not Found",
            json!({
                "error": "not_found",
                "error_description": format!("Unknown ops route '{path}'")
            }),
        ),
    }
}
