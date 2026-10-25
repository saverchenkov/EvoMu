//! PI common structures and colony simulation helpers.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
pub use crate::services::industry_pi::pi_resource_helpers::*;
use crate::services::industry_pi::{
    extractor_overlap_factor, list_sde_planet_p0_resources, pi_schematic_recipe,
};
use eve_db::{IskCents, PlanetaryColonyRecord, WorldStateSnapshot};
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::TypeId;
use std::collections::{BTreeMap, HashMap};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct PiPinState {
    pub pin_id: i64,
    pub type_id: TypeId,
    pub latitude: f64,
    pub longitude: f64,
    pub schematic_id: Option<u32>,
    pub cpu_usage: f64,
    pub pg_usage: f64,
    #[serde(default)]
    pub extractor_resource_type_id: Option<TypeId>,
    #[serde(default)]
    pub extractor_cycle_seconds: u64,
    #[serde(default)]
    pub extractor_head_count: u8,
    #[serde(default)]
    pub extractor_heads: Vec<(i64, f64, f64)>,
    #[serde(default)]
    pub extractor_head_radius: f64,
    #[serde(default)]
    pub program_install_unix: u64,
    #[serde(default)]
    pub expiry_time_unix: Option<u64>,
    #[serde(default)]
    pub last_expedited_transfer_unix: u64,
    #[serde(default)]
    pub contents: HashMap<TypeId, u64>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct PiLinkState {
    pub source_pin_id: i64,
    pub dest_pin_id: i64,
    pub level: u8,
    pub length_km: f64,
    pub cpu_usage: f64,
    pub pg_usage: f64,
    pub bandwidth: f64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct PiRouteState {
    pub route_id: i64,
    pub path: Vec<i64>,
    pub commodity_type_id: TypeId,
    pub quantity: u32,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct PiColonyNetworkState {
    pub pins: Vec<PiPinState>,
    pub links: Vec<PiLinkState>,
    pub routes: Vec<PiRouteState>,
    pub cpu_used: f64,
    pub cpu_max: f64,
    pub pg_used: f64,
    pub pg_max: f64,
    #[serde(default)]
    pub last_simulated_unix: u64,
}

pub fn cc_level_info(level: u8) -> (f64, f64, f64) {
    match level {
        0 => (1_675.0, 6_000.0, 0.0), 1 => (7_057.0, 9_000.0, 580_000.0),
        2 => (12_136.0, 12_000.0, 1_510_000.0), 3 => (17_215.0, 15_000.0, 2_710_000.0),
        4 => (21_315.0, 17_000.0, 4_210_000.0), _ => (25_415.0, 19_000.0, 6_310_000.0),
    }
}

pub fn cc_level_cpu_pg(level: u8) -> (f64, f64) {
    let (cpu, pg, _) = cc_level_info(level);
    (cpu, pg)
}

pub fn cc_upgrade_cost_isk(current_level: u8, desired_level: u8) -> f64 {
    if desired_level <= current_level {
        return 0.0;
    }
    let (_, _, cur_cum) = cc_level_info(current_level);
    let (_, _, des_cum) = cc_level_info(desired_level);
    (des_cum - cur_cum).max(0.0)
}

pub fn is_command_center_type(ctx: &ServerContext, type_id: TypeId) -> bool {
    is_command_center_pin(&ctx.sde, type_id)
}

pub fn pin_storage_capacity_m3(sde: &eve_sde::SdeRegistry, type_id: TypeId) -> f64 {
    sde.types.get(&type_id).map(|t| {
        if t.capacity > 0.0 {
            t.capacity
        } else {
            match t.group_id {
                1027 => 500.0,
                1030 => 10_000.0,
                1029 => 12_000.0,
                _ => 10_000.0,
            }
        }
    }).unwrap_or(10_000.0)
}

pub fn is_launchpad_pin(sde: &eve_sde::SdeRegistry, type_id: TypeId) -> bool {
    sde.types.get(&type_id).map(|t| t.group_id == 1030).unwrap_or(false)
        || matches!(type_id, 2256 | 2542 | 2543 | 2544 | 2552 | 2555 | 2556 | 2557)
}

pub fn is_storage_pin(sde: &eve_sde::SdeRegistry, type_id: TypeId) -> bool {
    sde.types.get(&type_id).map(|t| t.group_id == 1029).unwrap_or(false)
        || matches!(type_id, 2257 | 2535 | 2536 | 2541 | 2558 | 2560 | 2561 | 2562)
}

pub fn is_command_center_pin(sde: &eve_sde::SdeRegistry, type_id: TypeId) -> bool {
    sde.types.get(&type_id).map(|t| t.group_id == 1027).unwrap_or(false)
        || matches!(type_id, 2254 | 2524 | 2525 | 2533 | 2534 | 2549 | 2550 | 2551 | 2129..=2160 | 2574..=2586)
}

pub fn is_storage_or_cc_or_launchpad_pin(sde: &eve_sde::SdeRegistry, type_id: TypeId) -> bool {
    sde.types.get(&type_id).map(|t| t.group_id == 1030 || t.group_id == 1029 || t.group_id == 1027).unwrap_or(false)
        || is_launchpad_pin(sde, type_id)
        || is_storage_pin(sde, type_id)
        || is_command_center_pin(sde, type_id)
}

pub fn pin_contents_volume_m3(sde: &eve_sde::SdeRegistry, pin: &PiPinState) -> f64 {
    pin.contents
        .iter()
        .map(|(&t, &amt)| (amt as f64) * sde.types.get(&t).map(|ty| ty.volume.max(0.01)).unwrap_or(0.38))
        .sum()
}

pub fn extract_pi_pin_id(rep: Option<&PyRep>) -> i64 {
    match rep {
        Some(PyRep::Integer(id)) => *id,
        Some(PyRep::Tuple(els) | PyRep::List(els)) => els
            .last()
            .and_then(|v| v.as_i64())
            .or_else(|| els.first().and_then(|v| v.as_i64()))
            .unwrap_or(0),
        _ => 0,
    }
}

pub fn resolve_pi_pin_stats(ctx: &ServerContext, type_id: TypeId) -> (f64, f64, f64) {
    if is_command_center_type(ctx, type_id) {
        return (0.0, 0.0, 0.0);
    }
    if let Some(inv_type) = ctx.sde.types.get(&type_id) {
        let cpu = inv_type.default_attributes.get(&49).copied().or_else(|| inv_type.default_attributes.get(&50).copied()).filter(|&v| v > 0.0);
        let pg = inv_type.default_attributes.get(&15).copied().or_else(|| inv_type.default_attributes.get(&11).copied()).filter(|&v| v > 0.0);
        let base_isk = if inv_type.base_price > 0.0 { inv_type.base_price } else { 45_000.0 };
        let (fallback_cpu, fallback_pg) = match inv_type.group_id {
            1026 => (400.0, 2_600.0), 1028 => (200.0, 800.0), 1029 => (500.0, 700.0), 1030 => (3_600.0, 700.0), _ => (200.0, 800.0),
        };
        (cpu.unwrap_or(fallback_cpu), pg.unwrap_or(fallback_pg), base_isk)
    } else {
        (200.0, 800.0, 45_000.0)
    }
}

pub fn pi_network_note_id(char_id: i64, planet_id: i64) -> i64 {
    let mixed = ((char_id.unsigned_abs() & 0xFFFF_FFFF) << 24)
        ^ (planet_id.unsigned_abs() & 0xFFFF_FFFF);
    -((mixed as i64).max(1) + 3_000_000_000_000)
}

pub fn has_pi_link_path(links: &[PiLinkState], path: &[i64], transfer_vol_m3: f64) -> Result<(), BridgeRpcError> {
    if path.len() < 2 {
        return Err(BridgeRpcError {
            code: "InvalidTransferSource".into(),
            message: "Source and destination pin must be distinct".into(),
        });
    }
    let src = path[0];
    let dst = *path.last().unwrap();
    if src == dst {
        return Err(BridgeRpcError {
            code: "InvalidTransferSource".into(),
            message: "Source and destination pin must be distinct".into(),
        });
    }

    let hops: Vec<(i64, i64)> = if path.len() > 2 {
        path.windows(2).map(|w| (w[0], w[1])).collect()
    } else if links.iter().any(|l| {
        (l.source_pin_id == src && l.dest_pin_id == dst)
            || (l.source_pin_id == dst && l.dest_pin_id == src)
    }) {
        vec![(src, dst)]
    } else {
        let mut visited = std::collections::HashSet::new();
        let mut queue = std::collections::VecDeque::new();
        let mut parent: std::collections::HashMap<i64, i64> = std::collections::HashMap::new();
        queue.push_back(src);
        visited.insert(src);
        let mut found = false;
        while let Some(curr) = queue.pop_front() {
            if curr == dst {
                found = true;
                break;
            }
            for link in links {
                let next = if link.source_pin_id == curr {
                    Some(link.dest_pin_id)
                } else if link.dest_pin_id == curr {
                    Some(link.source_pin_id)
                } else {
                    None
                };
                if let Some(n) = next {
                    if visited.insert(n) {
                        parent.insert(n, curr);
                        queue.push_back(n);
                    }
                }
            }
        }
        if !found {
            return Err(BridgeRpcError {
                code: "NoLinkBetweenPins".into(),
                message: format!("No link or route path exists between pin {src} and pin {dst}"),
            });
        }
        let mut curr = dst;
        let mut resolved = vec![curr];
        while let Some(&p) = parent.get(&curr) {
            resolved.push(p);
            curr = p;
        }
        resolved.reverse();
        resolved.windows(2).map(|w| (w[0], w[1])).collect()
    };

    for (u, v) in hops {
        let link = links.iter().find(|l| {
            (l.source_pin_id == u && l.dest_pin_id == v)
                || (l.source_pin_id == v && l.dest_pin_id == u)
        }).ok_or_else(|| BridgeRpcError {
            code: "NoLinkBetweenPins".into(),
            message: format!("No link exists between hop pin {u} and pin {v}"),
        })?;

        let effective_bw = if link.bandwidth > 0.0 {
            link.bandwidth
        } else {
            1_250.0 * 2.0_f64.powi(link.level as i32)
        };

        if transfer_vol_m3 > effective_bw + 1e-6 {
            return Err(BridgeRpcError {
                code: "InsufficientLinkBandwidth".into(),
                message: format!(
                    "Link between pin {u} and pin {v} bandwidth ({effective_bw:.1} m3) is insufficient for transfer volume ({transfer_vol_m3:.1} m3)"
                ),
            });
        }
    }

    Ok(())
}

pub fn load_pi_network(
    snap: &eve_db::WorldStateSnapshot,
    char_id: i64,
    planet_id: i64,
    cc_level: u8,
) -> PiColonyNetworkState {
    let nid = pi_network_note_id(char_id, planet_id);
    let mut net = if let Some(note) = snap.owner_notes.get(&nid) {
        if let Ok(mut loaded) = serde_json::from_str::<PiColonyNetworkState>(&note.note) {
            let (c_max, p_max) = cc_level_cpu_pg(cc_level);
            if loaded.cpu_max < c_max {
                loaded.cpu_max = c_max;
            }
            if loaded.pg_max < p_max {
                loaded.pg_max = p_max;
            }
            loaded
        } else {
            let (cpu_max, pg_max) = cc_level_cpu_pg(cc_level);
            let mut net = PiColonyNetworkState {
                cpu_max,
                pg_max,
                ..Default::default()
            };
            if let Some(col) = snap.planetary_colonies.get(&(char_id, planet_id)) {
                let cc_pin_id = 200_000_000 + planet_id;
                net.pins.push(PiPinState {
                    pin_id: cc_pin_id,
                    type_id: 2534,
                    contents: col.extracted_commodities.clone(),
                    ..Default::default()
                });
            }
            net
        }
    } else if let Some(layout) = snap.planetary_colony_layouts.get(&(char_id, planet_id)) {
        let (cpu_max, pg_max) = cc_level_cpu_pg(cc_level);
        PiColonyNetworkState {
            cpu_max,
            pg_max,
            pins: layout.pins.iter().map(|p| PiPinState {
                pin_id: p.pin_id,
                type_id: p.type_id,
                latitude: p.latitude,
                longitude: p.longitude,
                schematic_id: p.schematic_id.map(|s| s as u32),
                program_install_unix: p.install_time_unix.unwrap_or(0),
                expiry_time_unix: p.expiry_time_unix,
                contents: p.contents.iter().map(|c| (c.type_id, c.amount.max(0) as u64)).collect(),
                ..Default::default()
            }).collect(),
            links: layout.links.iter().map(|l| PiLinkState {
                source_pin_id: l.source_pin_id,
                dest_pin_id: l.destination_pin_id,
                level: l.link_level as u8,
                ..Default::default()
            }).collect(),
            routes: layout.routes.iter().map(|r| {
                let mut path = vec![r.source_pin_id];
                path.extend_from_slice(&r.waypoints);
                path.push(r.destination_pin_id);
                PiRouteState {
                    route_id: r.route_id,
                    path,
                    commodity_type_id: r.content_type_id,
                    quantity: r.quantity.max(0) as u32,
                }
            }).collect(),
            ..Default::default()
        }
    } else {
        let (cpu_max, pg_max) = cc_level_cpu_pg(cc_level);
        PiColonyNetworkState {
            cpu_max,
            pg_max,
            ..Default::default()
        }
    };

    if let Some(layout) = snap.planetary_colony_layouts.get(&(char_id, planet_id)) {
        for lpin in &layout.pins {
            if let Some(pin) = net.pins.iter_mut().find(|p| p.pin_id == lpin.pin_id) {
                for c in &lpin.contents {
                    pin.contents.entry(c.type_id).or_insert(c.amount.max(0) as u64);
                }
            }
        }
    }

    if let Some(col) = snap.planetary_colonies.get(&(char_id, planet_id)) {
        if net.pins.is_empty() {
            let default_cc_type_id: TypeId = 2543;
            let default_pin_id: i64 = 200_000_000 + (planet_id.abs() % 1_000_000);
            net.pins.push(PiPinState {
                pin_id: default_pin_id,
                type_id: default_cc_type_id,
                contents: col.extracted_commodities.clone(),
                ..Default::default()
            });
        }
    }

    net
}

pub fn save_pi_network(
    snap: &mut eve_db::WorldStateSnapshot,
    char_id: i64,
    planet_id: i64,
    net: &PiColonyNetworkState,
) {
    let nid = pi_network_note_id(char_id, planet_id);
    if let Ok(js) = serde_json::to_string(net) {
        snap.owner_notes.insert(
            nid,
            eve_db::OwnerNoteRecord {
                note_id: nid,
                character_id: -1,
                item_id: planet_id,
                label: format!("pi_net:{char_id}:{planet_id}"),
                note: js,
                timestamp_unix: 0,
            },
        );
    }
    let layout = eve_db::PlanetaryColonyLayoutRecord {
        pins: net
            .pins
            .iter()
            .map(|p| eve_db::PlanetaryPinRecord {
                pin_id: p.pin_id,
                type_id: p.type_id,
                latitude: p.latitude,
                longitude: p.longitude,
                schematic_id: p.schematic_id.map(i64::from),
                install_time_unix: (p.program_install_unix > 0).then_some(p.program_install_unix),
                expiry_time_unix: p.expiry_time_unix,
                last_cycle_start_unix: (net.last_simulated_unix > 0).then_some(net.last_simulated_unix),
                contents: p
                    .contents
                    .iter()
                    .map(|(&tid, &amt)| eve_db::PlanetaryPinContentRecord {
                        type_id: tid,
                        amount: amt as i64,
                    })
                    .collect(),
            })
            .collect(),
        links: net
            .links
            .iter()
            .map(|l| eve_db::PlanetaryLinkRecord {
                source_pin_id: l.source_pin_id,
                destination_pin_id: l.dest_pin_id,
                link_level: i64::from(l.level),
            })
            .collect(),
        routes: net
            .routes
            .iter()
            .map(|r| eve_db::PlanetaryRouteRecord {
                route_id: r.route_id,
                source_pin_id: r.path.first().copied().unwrap_or(0),
                destination_pin_id: r.path.last().copied().unwrap_or(0),
                content_type_id: r.commodity_type_id,
                quantity: i64::from(r.quantity),
                waypoints: if r.path.len() > 2 {
                    r.path[1..r.path.len() - 1].to_vec()
                } else {
                    Vec::new()
                },
            })
            .collect(),
    };
    snap.planetary_colony_layouts.insert((char_id, planet_id), layout);
    if let Some(col) = snap.planetary_colonies.get_mut(&(char_id, planet_id)) {
        let mut pin_totals: HashMap<TypeId, u64> = HashMap::new();
        for p in &net.pins {
            for (&tid, &amt) in &p.contents {
                *pin_totals.entry(tid).or_insert(0) += amt;
            }
        }
        col.extracted_commodities = pin_totals;
    }
}

pub fn format_pi_pins_links(net: &PiColonyNetworkState) -> (Vec<PyRep>, Vec<PyRep>, Vec<PyRep>) {
    let pins_rep = net.pins.iter().map(|p| PyRep::keyval([
        ("pinID", PyRep::Integer(p.pin_id)), ("typeID", PyRep::Integer(p.type_id as i64)),
        ("latitude", PyRep::Real(p.latitude)), ("longitude", PyRep::Real(p.longitude)),
        ("cpuUsage", PyRep::Real(p.cpu_usage)), ("powergridUsage", PyRep::Real(p.pg_usage)),
        ("schematicID", PyRep::Integer(p.schematic_id.unwrap_or(0) as i64)), ("headCount", PyRep::Integer(p.extractor_head_count as i64)),
    ])).collect();
    let links_rep = net.links.iter().map(|l| PyRep::keyval([
        ("endpoint1", PyRep::Integer(l.source_pin_id)), ("endpoint2", PyRep::Integer(l.dest_pin_id)),
        ("sourcePinID", PyRep::Integer(l.source_pin_id)), ("destPinID", PyRep::Integer(l.dest_pin_id)),
        ("level", PyRep::Integer(l.level as i64)), ("lengthKm", PyRep::Real(l.length_km)),
        ("cpuUsage", PyRep::Real(l.cpu_usage)), ("powergridUsage", PyRep::Real(l.pg_usage)), ("bandwidth", PyRep::Real(l.bandwidth)),
    ])).collect();
    let routes_rep = net.routes.iter().map(|r| PyRep::keyval([
        ("routeID", PyRep::Integer(r.route_id)), ("commodityTypeID", PyRep::Integer(r.commodity_type_id as i64)),
        ("quantity", PyRep::Integer(r.quantity as i64)), ("path", PyRep::List(r.path.iter().map(|&id| PyRep::Integer(id)).collect())),
    ])).collect();
    (pins_rep, links_rep, routes_rep)
}

pub fn queue_pi_notification(
    ctx: &ServerContext,
    recipient_char_id: i64,
    event: &str,
    planet_id: i64,
    pin_id: i64,
    type_id: i32,
    now_unix: u64,
) {
    let args = vec![PyRep::Integer(planet_id), PyRep::Integer(pin_id)];
    let now_ft = ctx.now_blueos_filetime_ticks();
    let (packet, marshaled_packet) =
        ServerContext::build_macho_notification_packet(recipient_char_id, None, event, "charid", args.clone());
    let notif = crate::router::cache_notifications::PendingMachoNotification {
        recipient_char_id,
        event: event.to_string(),
        id_type: "charid".to_string(),
        args,
        packet,
        marshaled_packet,
        created_filetime: now_ft,
    };
    if let Ok(mut queues) = ctx.pending_notifications.write() {
        let q = queues.entry(recipient_char_id).or_default();
        if q.len() >= crate::router::cache_notifications::MAX_PENDING_NOTIFICATIONS_PER_CHAR {
            q.remove(0);
        }
        q.push(notif);
    }

    let rec = crate::router::cache_notifications::NotificationRecord {
        notification_id: -(7_900_000_000_000 + (type_id as i64) * 1_000_000 + pin_id.abs()),
        type_id,
        group_id: 1,
        sender_id: recipient_char_id,
        receiver_id: recipient_char_id,
        processed: false,
        created: now_unix as i64,
        data: format!("{event}:{planet_id}:{pin_id}"),
        deleted: false,
        interacted: false,
    };
    if let Ok(mut recs) = ctx.notification_records.write() {
        recs.push(rec);
    }
}

pub fn advance_colony_simulation(
    ctx: &ServerContext,
    col: &mut PlanetaryColonyRecord,
    net: &mut PiColonyNetworkState,
    elapsed_sec: u64,
    now_unix: u64,
) -> (u64, u64) {
    if elapsed_sec == 0 {
        return (0, 0);
    }
    let default_p0 = list_sde_planet_p0_resources(ctx, col.solar_system_id, col.planet_id)
        .first()
        .copied()
        .unwrap_or(2268);
    let mut extracted_total = 0_u64;
    let mut processed_total = 0_u64;

    let mut extracted_batches: Vec<(i64, TypeId, u64, f64)> = Vec::new();
    for pin in &net.pins {
        let is_ext = pin.type_id == 2848
            || pin.type_id == 3060
            || ctx.sde.types.get(&pin.type_id).map(|t| t.group_id == 1026 || t.group_id == 1063).unwrap_or(false);
        if is_ext && pin.extractor_cycle_seconds > 0 && pin.extractor_head_count > 0 {
            let cyc_secs = pin.extractor_cycle_seconds.max(60);
            let cycles = (elapsed_sec / cyc_secs).min(200);
            let p0_tid = pin.extractor_resource_type_id.unwrap_or(default_p0);
            let heads = u64::from(pin.extractor_head_count.clamp(1, 10));
            let overlap = extractor_overlap_factor(&pin.extractor_heads, pin.extractor_head_radius.max(0.02));
            let valid_route = net
                .routes
                .iter()
                .find(|r| {
                    r.path.first() == Some(&pin.pin_id)
                        && r.commodity_type_id == p0_tid
                        && (r.path.len() < 2 || has_pi_link_path(&net.links, &r.path, 0.0).is_ok())
                });
            let target_pin_id = valid_route
                .and_then(|r| r.path.last().copied())
                .unwrap_or(pin.pin_id);
            let p0_vol_per_unit = ctx.sde.types.get(&p0_tid).map(|t| t.volume.max(0.01)).unwrap_or(0.38);
            for c in 0..cycles {
                let decay = (-0.015 * (c as f64)).exp();
                let batch = ((1_000.0 * (heads as f64) * overlap * decay).round() as u64).max(10);
                extracted_batches.push((target_pin_id, p0_tid, batch, p0_vol_per_unit));
            }
        }
    }
    let mut full_storage_pins: Vec<i64> = Vec::new();
    for (t_pin_id, p0_tid, batch, unit_vol) in extracted_batches {
        if let Some(target_pin) = net.pins.iter_mut().find(|p| p.pin_id == t_pin_id) {
            let cap_m3 = pin_storage_capacity_m3(&ctx.sde, target_pin.type_id);
            let cur_vol = pin_contents_volume_m3(&ctx.sde, target_pin);
            let avail_m3 = (cap_m3 - cur_vol).max(0.0);
            let max_units = (avail_m3 / unit_vol).floor() as u64;
            let actual_deposit = batch.min(max_units);
            if actual_deposit > 0 {
                *target_pin.contents.entry(p0_tid).or_insert(0) += actual_deposit;
                *col.extracted_commodities.entry(p0_tid).or_insert(0) += actual_deposit;
                extracted_total += actual_deposit;
            }
            let new_vol = pin_contents_volume_m3(&ctx.sde, target_pin);
            if new_vol + unit_vol > cap_m3 || (batch > 0 && actual_deposit < batch) {
                if !full_storage_pins.contains(&t_pin_id) {
                    full_storage_pins.push(t_pin_id);
                }
            }
        }
    }

    for pin_idx in 0..net.pins.len() {
        let pin = &net.pins[pin_idx];
        let Some(schem_id) = pin.schematic_id else { continue };
        let (cyc_secs, inputs, out_tid, out_qty) = pi_schematic_recipe(schem_id, default_p0);
        let pin_id = pin.pin_id;
        let all_inputs_routed = inputs.iter().all(|&(in_tid, _)| {
            net.routes.iter().any(|r| {
                r.path.last() == Some(&pin_id)
                    && r.commodity_type_id == in_tid
                    && r.path.len() >= 2
                    && has_pi_link_path(&net.links, &r.path, 0.0).is_ok()
            })
        });
        let output_route = net.routes.iter().find(|r| {
            r.path.first() == Some(&pin_id)
                && r.commodity_type_id == out_tid
                && r.path.len() >= 2
                && has_pi_link_path(&net.links, &r.path, 0.0).is_ok()
        });
        if !all_inputs_routed || output_route.is_none() {
            continue;
        }
        let out_target_pin_id = output_route.and_then(|r| r.path.last().copied()).unwrap_or(pin_id);
        let input_sources: Vec<(TypeId, u64, Vec<i64>)> = inputs
            .iter()
            .map(|&(in_tid, in_qty)| {
                let mut pids: Vec<i64> = net
                    .routes
                    .iter()
                    .filter(|r| {
                        r.path.last() == Some(&pin_id)
                            && r.commodity_type_id == in_tid
                            && r.path.len() >= 2
                            && has_pi_link_path(&net.links, &r.path, 0.0).is_ok()
                    })
                    .filter_map(|r| r.path.first().copied())
                    .collect();
                if !pids.contains(&pin_id) {
                    pids.push(pin_id);
                }
                (in_tid, in_qty, pids)
            })
            .collect();

        let out_unit_vol = ctx.sde.types.get(&out_tid).map(|t| t.volume.max(0.01)).unwrap_or(0.38);
        let max_cycles = (elapsed_sec / cyc_secs).min(100);
        for _ in 0..max_cycles {
            // Verify destination pin capacity before processor cycle execution
            let Some(dst_p) = net.pins.iter().find(|p| p.pin_id == out_target_pin_id) else {
                break;
            };
            let dst_cap_m3 = pin_storage_capacity_m3(&ctx.sde, dst_p.type_id);
            let dst_cur_vol = pin_contents_volume_m3(&ctx.sde, dst_p);
            let needed_vol = out_unit_vol * (out_qty as f64);
            if dst_cur_vol + needed_vol > dst_cap_m3 + 1e-6 {
                // Destination storage full, cannot deposit processor output
                if !full_storage_pins.contains(&out_target_pin_id) {
                    full_storage_pins.push(out_target_pin_id);
                }
                break;
            }

            // Verify processor schematic inputs against connected pin contents rather than colony-wide totals
            let can_fulfill = input_sources.iter().all(|&(in_tid, in_qty, ref pids)| {
                let avail: u64 = pids
                    .iter()
                    .filter_map(|&pid| net.pins.iter().find(|p| p.pin_id == pid))
                    .map(|p| p.contents.get(&in_tid).copied().unwrap_or(0))
                    .sum();
                avail >= in_qty
            });
            if !can_fulfill {
                break;
            }

            // Deduct inputs from connected pins and update colony total
            for &(in_tid, in_qty, ref pids) in &input_sources {
                let mut remaining = in_qty;
                for &pid in pids {
                    if remaining == 0 {
                        break;
                    }
                    if let Some(src_p) = net.pins.iter_mut().find(|p| p.pin_id == pid) {
                        if let Some(c) = src_p.contents.get_mut(&in_tid) {
                            if *c <= remaining {
                                remaining -= *c;
                                src_p.contents.remove(&in_tid);
                            } else {
                                *c -= remaining;
                                remaining = 0;
                            }
                        }
                    }
                }
                if let Some(tot) = col.extracted_commodities.get_mut(&in_tid) {
                    *tot = tot.saturating_sub(in_qty);
                }
            }

            // Output is only minted when inputs have been deducted from connected pins
            *col.extracted_commodities.entry(out_tid).or_insert(0) += out_qty;
            if let Some(dst_p) = net.pins.iter_mut().find(|p| p.pin_id == out_target_pin_id) {
                *dst_p.contents.entry(out_tid).or_insert(0) += out_qty;
            }
            processed_total += out_qty;
        }
    }

    // Check for extractor program completion or expiry
    let mut finished_extractors: Vec<i64> = Vec::new();
    for pin in net.pins.iter_mut() {
        let is_ext = pin.type_id == 2848
            || pin.type_id == 3060
            || ctx.sde.types.get(&pin.type_id).map(|t| t.group_id == 1026 || t.group_id == 1063).unwrap_or(false);
        if is_ext && (pin.program_install_unix > 0 || pin.expiry_time_unix.is_some()) {
            let is_expired = if let Some(exp) = pin.expiry_time_unix {
                exp > 0 && now_unix >= exp
            } else if pin.program_install_unix > 0 {
                now_unix >= pin.program_install_unix.saturating_add(86_400)
            } else {
                false
            };
            if is_expired {
                finished_extractors.push(pin.pin_id);
                pin.program_install_unix = 0;
                pin.expiry_time_unix = None;
            }
        }
    }

    // Check for any pins in the colony that have reached storage capacity
    for pin in &net.pins {
        let cap_m3 = pin_storage_capacity_m3(&ctx.sde, pin.type_id);
        if cap_m3 > 0.0 {
            let cur_vol = pin_contents_volume_m3(&ctx.sde, pin);
            if cur_vol >= cap_m3 - 1e-6 {
                if !full_storage_pins.contains(&pin.pin_id) {
                    full_storage_pins.push(pin.pin_id);
                }
            }
        }
    }

    // Emit storage full notifications for pins that reached capacity
    for pid in full_storage_pins {
        queue_pi_notification(ctx, col.owner_id, "PlanetStorageFull", col.planet_id, pid, 102, now_unix);
    }

    // Emit extractor finished notifications for expired extractor head programs
    for pid in finished_extractors {
        queue_pi_notification(ctx, col.owner_id, "PlanetExtractorFinished", col.planet_id, pid, 101, now_unix);
    }

    net.last_simulated_unix = now_unix;
    col.last_extracted_unix = now_unix;
    (extracted_total, processed_total)
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OrbitalCustomsSettings {
    pub planet_id: i64,
    pub orbital_type_id: TypeId, // 2233 = POCO, 4318 = InterBus, 81080 = Orbital Skyhook
    pub owner_corp_id: i64,
    pub corporation_tax_rate: f64,
    pub alliance_tax_rate: f64,
    pub standing_high_tax_rate: f64,
    pub standing_good_tax_rate: f64,
    pub standing_neutral_tax_rate: f64,
    pub standing_bad_tax_rate: f64,
    pub standing_horrible_tax_rate: f64,
    pub allow_alliance: bool,
    pub allow_standings: bool,
    pub minimum_standing: f64,
}

impl Default for OrbitalCustomsSettings {
    fn default() -> Self {
        Self {
            planet_id: 0,
            orbital_type_id: 2233,
            owner_corp_id: 1_000_009,
            corporation_tax_rate: 0.05,
            alliance_tax_rate: 0.05,
            standing_high_tax_rate: 0.05,
            standing_good_tax_rate: 0.05,
            standing_neutral_tax_rate: 0.05,
            standing_bad_tax_rate: 0.05,
            standing_horrible_tax_rate: 0.05,
            allow_alliance: true,
            allow_standings: true,
            minimum_standing: -10.0,
        }
    }
}

pub fn orbital_settings_note_id(planet_or_orbital_id: i64) -> i64 {
    -(4_000_000_000_000_i64.saturating_add(planet_or_orbital_id.abs()))
}

pub fn load_orbital_settings(
    snap: &eve_db::WorldStateSnapshot,
    planet_or_orbital_id: i64,
) -> Option<OrbitalCustomsSettings> {
    let nid = orbital_settings_note_id(planet_or_orbital_id);
    snap.owner_notes
        .get(&nid)
        .and_then(|n| serde_json::from_str::<OrbitalCustomsSettings>(&n.note).ok())
}

pub fn save_orbital_settings(
    snap: &mut eve_db::WorldStateSnapshot,
    planet_or_orbital_id: i64,
    settings: &OrbitalCustomsSettings,
) {
    let nid = orbital_settings_note_id(planet_or_orbital_id);
    if let Ok(js) = serde_json::to_string(settings) {
        snap.owner_notes.insert(
            nid,
            eve_db::OwnerNoteRecord {
                note_id: nid,
                character_id: -4,
                item_id: planet_or_orbital_id,
                label: format!("orbital_settings:{planet_or_orbital_id}"),
                note: js,
                timestamp_unix: 0,
            },
        );
    }
    let sys_id = snap
        .planetary_colonies
        .values()
        .find(|c| c.planet_id == settings.planet_id || c.planet_id == planet_or_orbital_id)
        .map(|c| c.solar_system_id)
        .unwrap_or(30_000_142);
    let standing_lvl = if settings.minimum_standing > 5.0 {
        Some("excellent".to_string())
    } else if settings.minimum_standing > 0.0 {
        Some("good".to_string())
    } else if settings.minimum_standing >= -0.01 {
        Some("neutral".to_string())
    } else if settings.minimum_standing >= -5.0 {
        Some("bad".to_string())
    } else {
        Some("terrible".to_string())
    };
    let rec = eve_db::CustomsOfficeRecord {
        office_id: planet_or_orbital_id,
        system_id: sys_id,
        reinforce_exit_start: 18,
        reinforce_exit_end: 20,
        allow_alliance_access: settings.allow_alliance,
        allow_access_with_standings: settings.allow_standings,
        standing_level: standing_lvl,
        corporation_tax_rate: Some(settings.corporation_tax_rate),
        alliance_tax_rate: Some(settings.alliance_tax_rate),
        excellent_standing_tax_rate: Some(settings.standing_high_tax_rate),
        good_standing_tax_rate: Some(settings.standing_good_tax_rate),
        neutral_standing_tax_rate: Some(settings.standing_neutral_tax_rate),
        bad_standing_tax_rate: Some(settings.standing_bad_tax_rate),
        terrible_standing_tax_rate: Some(settings.standing_horrible_tax_rate),
    };
    let offices = snap.corp_customs_offices.entry(settings.owner_corp_id).or_default();
    if let Some(existing) = offices.iter_mut().find(|o| o.office_id == planet_or_orbital_id) {
        *existing = rec;
    } else {
        offices.push(rec);
    }
}

pub fn resolve_character_orbital_access_and_tax(
    snap: &eve_db::WorldStateSnapshot,
    char_id: i64,
    planet_id: i64,
) -> Result<(f64, TypeId), eve_proto::BridgeRpcError> {
    let Some(cfg) = load_orbital_settings(snap, planet_id) else {
        return Ok((0.05, 2233));
    };
    if cfg.orbital_type_id == 4318 {
        return Ok((0.0, 4318));
    }
    let ch = snap.characters.get(&char_id);
    let char_corp = ch.map(|c| c.corporation_id).unwrap_or(0);
    if char_corp == cfg.owner_corp_id || char_id == cfg.owner_corp_id {
        return Ok((cfg.corporation_tax_rate, cfg.orbital_type_id));
    }
    let char_alliance = snap.corporations.get(&char_corp).and_then(|c| c.alliance_id);
    let owner_alliance = snap.corporations.get(&cfg.owner_corp_id).and_then(|c| c.alliance_id);
    if let (Some(ca), Some(oa)) = (char_alliance, owner_alliance) {
        if ca == oa {
            if !cfg.allow_alliance {
                return Err(eve_proto::BridgeRpcError {
                    code: "CustomsOfficeAccessDenied".into(),
                    message: format!("Customs Office on planet {planet_id} denies alliance access"),
                });
            }
            return Ok((cfg.alliance_tax_rate, cfg.orbital_type_id));
        }
    }
    let standing = ch
        .and_then(|c| c.standings.get(&cfg.owner_corp_id).copied())
        .unwrap_or(0.0);
    if !cfg.allow_standings || standing < cfg.minimum_standing {
        return Err(eve_proto::BridgeRpcError {
            code: "CustomsOfficeAccessDenied".into(),
            message: format!(
                "Customs Office / Skyhook on planet {planet_id} denies access (standing {standing:.1} < minimum {:.1})",
                cfg.minimum_standing
            ),
        });
    }
    let rate = if standing > 5.0 {
        cfg.standing_high_tax_rate
    } else if standing > 0.0 {
        cfg.standing_good_tax_rate
    } else if standing >= -0.01 {
        cfg.standing_neutral_tax_rate
    } else if standing >= -5.0 {
        cfg.standing_bad_tax_rate
    } else {
        cfg.standing_horrible_tax_rate
    };
    Ok((rate, cfg.orbital_type_id))
}

pub fn upgrade_command_center_logic(
    snap: &mut WorldStateSnapshot,
    char_id: i64,
    planet_id: i64,
    target_level: u8,
) -> Result<PyRep, BridgeRpcError> {
    let Some(col) = snap.planetary_colonies.get(&(char_id, planet_id)) else {
        return Err(BridgeRpcError {
            code: "ColonyNotFound".into(),
            message: format!("No colony found for character {char_id} on planet {planet_id}"),
        });
    };
    if target_level <= col.command_center_level {
        return Err(BridgeRpcError {
            code: "InvalidCommandCenterLevel".into(),
            message: format!("Target level {target_level} must be greater than current level {}", col.command_center_level),
        });
    }
    let cc_skill_lvl = snap.skills.get(&char_id).and_then(|m| m.get(&2505)).map(|s| s.level).unwrap_or(0);
    if cc_skill_lvl < target_level {
        return Err(BridgeRpcError {
            code: "MissingSkillForCommandCenterUpgrade".into(),
            message: format!("Command Center upgrade to level {target_level} requires Command Center Upgrades skill (2505) >= {target_level} (current: {cc_skill_lvl})"),
        });
    }
    let upgrade_cost_isk = cc_upgrade_cost_isk(col.command_center_level, target_level);
    let upgrade_cost_cents = IskCents::from_f64_isk(upgrade_cost_isk);
    if let Some(ch) = snap.characters.get_mut(&char_id) {
        if ch.balance < upgrade_cost_cents {
            return Err(BridgeRpcError {
                code: "InsufficientFunds".into(),
                message: format!("Insufficient funds for command center upgrade ({upgrade_cost_isk:.2} ISK required)"),
            });
        }
        ch.balance = ch.balance.checked_sub(upgrade_cost_cents).unwrap_or(IskCents(0));
        let bal_after = ch.balance;
        snap.append_journal(char_id, "PlanetCommandCenterUpgrade", upgrade_cost_cents, bal_after, format!("Upgraded Command Center on planet {planet_id} to level {target_level}"));
    }
    let mut net = load_pi_network(snap, char_id, planet_id, target_level);
    let (new_cpu_max, new_pg_max) = cc_level_cpu_pg(target_level);
    net.cpu_max = new_cpu_max;
    net.pg_max = new_pg_max;
    save_pi_network(snap, char_id, planet_id, &net);

    let col_mut = snap.planetary_colonies.get_mut(&(char_id, planet_id)).expect("colony exists");
    col_mut.command_center_level = target_level;
    col_mut.pins_count = (target_level as u32) + 1;
    let new_pins = col_mut.pins_count;

    Ok(PyRep::keyval([
        ("planetID", PyRep::Integer(planet_id)),
        ("commandCenterLevel", PyRep::Integer(target_level as i64)),
        ("pinsCount", PyRep::Integer(new_pins as i64)),
        ("upgradeCostISK", PyRep::Real(upgrade_cost_isk)),
        ("cpuMax", PyRep::Real(new_cpu_max)),
        ("powergridMax", PyRep::Real(new_pg_max)),
    ]))
}

pub fn consume_pi_import_commodities(
    snap: &mut WorldStateSnapshot,
    char_id: i64,
    planet_id: i64,
    type_id: TypeId,
    qty: u64,
    ship_id_opt: Option<i64>,
    station_id_opt: Option<i64>,
    structure_id_opt: Option<i64>,
) -> Result<(), BridgeRpcError> {
    let mut matching_item_ids: Vec<i64> = snap
        .items
        .values()
        .filter(|it| {
            if it.owner_id != char_id || it.type_id != type_id || it.quantity <= 0 {
                return false;
            }
            let in_cargo = ship_id_opt.is_some_and(|sid| it.location_id == sid)
                && matches!(
                    it.flag,
                    eve_db::InventoryFlag::Cargo
                        | eve_db::InventoryFlag::SpecializedPlanetaryCommoditiesHold
                );
            let in_customs_or_hangar = (it.location_id == planet_id
                || station_id_opt.is_some_and(|sid| it.location_id == sid)
                || structure_id_opt.is_some_and(|sid| it.location_id == sid))
                && it.flag == eve_db::InventoryFlag::Hangar;
            in_cargo || in_customs_or_hangar
        })
        .map(|it| it.item_id)
        .collect();

    let total_available: i64 = matching_item_ids
        .iter()
        .map(|iid| snap.items.get(iid).map(|i| i.quantity as i64).unwrap_or(0))
        .sum();

    if total_available == 0 {
        return Err(BridgeRpcError {
            code: "ItemNotFound".into(),
            message: format!("Commodity item type {type_id} not found in ship cargo or customs office hangar"),
        });
    }
    if total_available < qty as i64 {
        return Err(BridgeRpcError {
            code: "InsufficientCommodityQuantity".into(),
            message: format!("Available quantity ({total_available}) is less than required import quantity ({qty})"),
        });
    }

    matching_item_ids.sort();
    let mut remaining = qty as i32;
    for iid in matching_item_ids {
        let Some(item) = snap.items.get_mut(&iid) else { continue };
        if item.quantity <= remaining {
            remaining -= item.quantity;
            snap.items.remove(&iid);
        } else {
            item.quantity -= remaining;
            item.version += 1;
            break;
        }
        if remaining == 0 {
            break;
        }
    }

    Ok(())
}

pub fn user_transfer_commodities_logic(
    ctx: &ServerContext,
    snap: &mut WorldStateSnapshot,
    char_id: i64,
    planet_id: i64,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    let mut comm_map: BTreeMap<TypeId, u64> = BTreeMap::new();
    let path = if let Some(PyRep::List(path_list)) = args.get(1) {
        let p: Vec<i64> = path_list.iter().filter_map(|x| x.as_i64()).collect();
        match args.get(2) {
            Some(PyRep::Dict(entries)) => {
                for (k, v) in entries {
                    let tid_raw = k.as_i64().ok_or_else(|| BridgeRpcError {
                        code: "InvalidCommodity".into(),
                        message: "Commodity type ID must be an integer".into(),
                    })?;
                    let tid = u32::try_from(tid_raw).map_err(|_| BridgeRpcError {
                        code: "InvalidCommodity".into(),
                        message: format!("Commodity type ID {tid_raw} out of range"),
                    })?;
                    let qty_raw = v.as_i64().ok_or_else(|| BridgeRpcError {
                        code: "InvalidCommodityQuantity".into(),
                        message: "Commodity quantity must be an integer".into(),
                    })?;
                    if qty_raw <= 0 {
                        return Err(BridgeRpcError {
                            code: "InvalidCommodityQuantity".into(),
                            message: format!("Commodity quantity must be positive, got {qty_raw}"),
                        });
                    }
                    let slot = comm_map.entry(tid).or_insert(0);
                    *slot = slot.checked_add(qty_raw as u64).ok_or_else(|| BridgeRpcError {
                        code: "InvalidCommodityQuantity".into(),
                        message: "Commodity quantity overflow".into(),
                    })?;
                }
            }
            Some(PyRep::KeyVal(entries)) => {
                for (k, v) in entries {
                    let tid_raw = k.parse::<i64>().map_err(|_| BridgeRpcError {
                        code: "InvalidCommodity".into(),
                        message: "Commodity type ID must be an integer".into(),
                    })?;
                    let tid = u32::try_from(tid_raw).map_err(|_| BridgeRpcError {
                        code: "InvalidCommodity".into(),
                        message: format!("Commodity type ID {tid_raw} out of range"),
                    })?;
                    let qty_raw = v.as_i64().ok_or_else(|| BridgeRpcError {
                        code: "InvalidCommodityQuantity".into(),
                        message: "Commodity quantity must be an integer".into(),
                    })?;
                    if qty_raw <= 0 {
                        return Err(BridgeRpcError {
                            code: "InvalidCommodityQuantity".into(),
                            message: format!("Commodity quantity must be positive, got {qty_raw}"),
                        });
                    }
                    let slot = comm_map.entry(tid).or_insert(0);
                    *slot = slot.checked_add(qty_raw as u64).ok_or_else(|| BridgeRpcError {
                        code: "InvalidCommodityQuantity".into(),
                        message: "Commodity quantity overflow".into(),
                    })?;
                }
            }
            _ => {}
        }
        p
    } else {
        let src_pin_id = args
            .get(1)
            .and_then(|a| a.as_i64())
            .or_else(|| kwargs.get("sourcePinID").and_then(|v| v.as_i64()))
            .unwrap_or(0);
        let dst_pin_id = args
            .get(2)
            .and_then(|a| a.as_i64())
            .or_else(|| kwargs.get("destPinID").and_then(|v| v.as_i64()))
            .unwrap_or(0);
        let comm_raw = args
            .get(3)
            .and_then(|a| a.as_i64())
            .or_else(|| kwargs.get("commodityTypeID").and_then(|v| v.as_i64()))
            .unwrap_or(0);
        let comm_type_id = u32::try_from(comm_raw).map_err(|_| BridgeRpcError {
            code: "InvalidCommodity".into(),
            message: format!("Commodity type ID {comm_raw} out of range"),
        })?;
        let qty_raw = args
            .get(4)
            .and_then(|a| a.as_i64())
            .or_else(|| kwargs.get("quantity").and_then(|v| v.as_i64()))
            .unwrap_or(1);
        if qty_raw <= 0 {
            return Err(BridgeRpcError {
                code: "InvalidCommodityQuantity".into(),
                message: format!("Commodity quantity must be positive, got {qty_raw}"),
            });
        }
        comm_map.insert(comm_type_id, qty_raw as u64);
        vec![src_pin_id, dst_pin_id]
    };

    if path.len() < 2 {
        return Err(BridgeRpcError {
            code: "PinNotFound".into(),
            message: "UserTransferCommodities requires at least source and destination pins".into(),
        });
    }
    let src_pin_id = path[0];
    let dst_pin_id = *path.last().unwrap();
    if src_pin_id == dst_pin_id {
        return Err(BridgeRpcError {
            code: "InvalidTransferSource".into(),
            message: "Source and destination pin must be distinct".into(),
        });
    }

    if comm_map.is_empty() {
        return Err(BridgeRpcError {
            code: "InvalidCommodity".into(),
            message: "UserTransferCommodities requires at least one commodity to transfer".into(),
        });
    }

    let total_transfer_vol_m3: f64 = comm_map
        .iter()
        .map(|(&tid, &qty)| {
            let unit_vol = ctx.sde.types.get(&tid).map(|t| t.volume.max(0.01)).unwrap_or(0.38);
            (qty as f64) * unit_vol
        })
        .sum();
    let cooldown_secs = (300.0_f64 + 0.03_f64 * total_transfer_vol_m3 * 60.0_f64).round() as u64;
    let now_unix = ctx.now_unix();

    let Some(col) = snap.planetary_colonies.get(&(char_id, planet_id)).cloned() else {
        return Err(BridgeRpcError {
            code: "ColonyNotFound".into(),
            message: format!("No colony found for character {char_id} on planet {planet_id}"),
        });
    };
    let mut net = load_pi_network(snap, char_id, planet_id, col.command_center_level);

    let src_idx = net.pins.iter().position(|p| p.pin_id == src_pin_id).ok_or_else(|| BridgeRpcError {
        code: "PinNotFound".into(),
        message: format!("Source pin {src_pin_id} not found on planet {planet_id}"),
    })?;
    let dst_idx = net.pins.iter().position(|p| p.pin_id == dst_pin_id).ok_or_else(|| BridgeRpcError {
        code: "PinNotFound".into(),
        message: format!("Destination pin {dst_pin_id} not found on planet {planet_id}"),
    })?;

    // Verify every hop in path exists and link bandwidth accommodates volume
    has_pi_link_path(&net.links, &path, total_transfer_vol_m3)?;

    if net.pins[src_idx].last_expedited_transfer_unix > 0
        && now_unix < net.pins[src_idx].last_expedited_transfer_unix.saturating_add(cooldown_secs)
    {
        return Err(BridgeRpcError {
            code: "ExpeditedTransferOnCooldown".into(),
            message: format!("Source pin {src_pin_id} expedited transfer is on cooldown for {cooldown_secs}s"),
        });
    }

    for (&comm_type_id, &qty) in &comm_map {
        let src_qty = net.pins[src_idx].contents.get(&comm_type_id).copied().unwrap_or(0);
        if src_qty < qty {
            return Err(BridgeRpcError {
                code: "InsufficientCommodityStorage".into(),
                message: format!("Source pin {src_pin_id} has {src_qty} of commodity {comm_type_id}, but {qty} is required"),
            });
        }
    }

    let dst_cap_m3 = pin_storage_capacity_m3(&ctx.sde, net.pins[dst_idx].type_id);
    let dst_curr_vol = pin_contents_volume_m3(&ctx.sde, &net.pins[dst_idx]);

    if dst_curr_vol + total_transfer_vol_m3 > dst_cap_m3 + 1e-6 {
        return Err(BridgeRpcError {
            code: "PinStorageFull".into(),
            message: format!("Expedited transfer volume {total_transfer_vol_m3:.1} m3 exceeds destination pin available capacity"),
        });
    }

    for (&comm_type_id, &qty) in &comm_map {
        let src_entry = net.pins[src_idx].contents.get_mut(&comm_type_id).ok_or_else(|| BridgeRpcError {
            code: "InsufficientCommodityStorage".into(),
            message: format!("Source pin missing commodity {comm_type_id}"),
        })?;
        if *src_entry < qty {
            return Err(BridgeRpcError {
                code: "InsufficientCommodityStorage".into(),
                message: format!("Source pin has insufficient commodity {comm_type_id}"),
            });
        }
        if *src_entry == qty {
            net.pins[src_idx].contents.remove(&comm_type_id);
        } else {
            *src_entry -= qty;
        }
        let dst_entry = net.pins[dst_idx].contents.entry(comm_type_id).or_insert(0);
        *dst_entry = dst_entry.checked_add(qty).ok_or_else(|| BridgeRpcError {
            code: "InvalidCommodityQuantity".into(),
            message: "Destination pin quantity overflow".into(),
        })?;
    }
    net.pins[src_idx].last_expedited_transfer_unix = now_unix;

    save_pi_network(snap, char_id, planet_id, &net);
    let (first_tid, first_qty) = comm_map.iter().next().map(|(&t, &q)| (t, q)).unwrap_or((0, 0));
    Ok(PyRep::keyval([
        ("planetID", PyRep::Integer(planet_id)),
        ("sourcePinID", PyRep::Integer(src_pin_id)),
        ("destPinID", PyRep::Integer(dst_pin_id)),
        ("commodityTypeID", PyRep::Integer(first_tid as i64)),
        ("quantity", PyRep::Integer(first_qty as i64)),
        ("volumeM3", PyRep::Real(total_transfer_vol_m3)),
        ("cooldownSeconds", PyRep::Integer(cooldown_secs as i64)),
    ]))
}
