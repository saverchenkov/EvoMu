//! PI common structures and colony simulation helpers.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
pub use crate::services::industry_pi::pi_resource_helpers::*;
use crate::services::industry_pi::{
    extractor_overlap_factor, list_sde_planet_p0_resources, list_sde_planet_p0_resources_sde,
    pi_schematic_recipe,
};
use eve_db::{IskCents, PlanetaryColonyRecord, WorldStateSnapshot};
use eve_proto::{BridgeRpcError, ItemQty, ItemQtyError, PyRep};
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
    pub last_launch_unix: u64,
    #[serde(default)]
    pub contents: HashMap<TypeId, u64>,
    #[serde(default)]
    pub last_cycle_unix: Option<u64>,
    #[serde(default)]
    pub extractor_depletion_cycles: f64,
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
    #[serde(default)]
    pub pending_login_extracted: u64,
    #[serde(default)]
    pub pending_login_processed: u64,
    #[serde(default)]
    pub resource_depletion_cycles: HashMap<TypeId, f64>,
}

/// Canonical PI extractor reference cycle length (30 minutes) used for depletion decay and yield scaling.
pub const CANONICAL_EXTRACTOR_CYCLE_SECS: f64 = 1_800.0;

pub(crate) fn is_extractor_pin_sde(sde: &eve_sde::SdeRegistry, pin: &PiPinState) -> bool {
    pin.type_id == 2848
        || pin.type_id == 3060
        || sde
            .types
            .get(&pin.type_id)
            .map(|t| t.group_id == 1026 || t.group_id == 1063)
            .unwrap_or(false)
}

fn is_extractor_pin(ctx: &ServerContext, pin: &PiPinState) -> bool {
    is_extractor_pin_sde(&ctx.sde, pin)
}

/// Returns elapsed decay progress (in canonical 1,800s cycles) for an active extractor program up to `up_to_unix`.
pub fn extractor_elapsed_canonical_cycles(pin: &PiPinState, up_to_unix: u64) -> f64 {
    if pin.program_install_unix == 0 || pin.extractor_cycle_seconds == 0 {
        return 0.0;
    }
    let effective_exp = pin
        .expiry_time_unix
        .unwrap_or_else(|| pin.program_install_unix.saturating_add(86_400));
    if effective_exp == 0 || up_to_unix <= pin.program_install_unix {
        return 0.0;
    }
    let end_ts = up_to_unix.min(effective_exp);
    end_ts.saturating_sub(pin.program_install_unix) as f64 / CANONICAL_EXTRACTOR_CYCLE_SECS
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

/// Returns `true` if `type_id` is a valid Planetary Interaction commodity/resource
/// (SDE category `42` Planetary Resources or category `43` Planetary Commodities, or an
/// input/output entry of an SDE planet schematic when type category metadata is absent).
pub fn is_valid_pi_commodity_type(sde: &eve_sde::SdeRegistry, type_id: TypeId) -> bool {
    if type_id == 0 {
        return false;
    }
    if let Some(inv_type) = sde.types.get(&type_id) {
        return matches!(inv_type.category_id, 42 | 43);
    }
    sde.planet_schematics
        .values()
        .any(|s| s.types.iter().any(|entry| entry.type_id == type_id))
}

/// Validates that `type_id` is a valid PI commodity/resource type; otherwise returns `InvalidCommodity`.
pub fn ensure_valid_pi_commodity_type(
    sde: &eve_sde::SdeRegistry,
    type_id: TypeId,
) -> Result<(), BridgeRpcError> {
    if is_valid_pi_commodity_type(sde, type_id) {
        Ok(())
    } else {
        Err(BridgeRpcError {
            code: "InvalidCommodity".into(),
            message: format!(
                "Type {type_id} is not a valid planetary resource or planetary commodity"
            ),
            client_error: None,
        })
    }
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
            client_error: None,
        });
    }
    let src = path[0];
    let dst = *path.last().unwrap();
    if src == dst {
        return Err(BridgeRpcError {
            code: "InvalidTransferSource".into(),
            message: "Source and destination pin must be distinct".into(),
            client_error: None,
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
                client_error: None,
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
            client_error: None,
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
                client_error: None,
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
                schematic_id: p.schematic_id.and_then(|s| u32::try_from(s).ok()),
                program_install_unix: p.install_time_unix.unwrap_or(0),
                expiry_time_unix: p.expiry_time_unix,
                contents: p.contents.iter().map(|c| (c.type_id, u64::try_from(c.amount).unwrap_or(0))).collect(),
                ..Default::default()
            }).collect(),
            links: layout.links.iter().map(|l| PiLinkState {
                source_pin_id: l.source_pin_id,
                dest_pin_id: l.destination_pin_id,
                level: u8::try_from(l.link_level.clamp(0, i64::from(u8::MAX))).unwrap_or(0),
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
                    quantity: u32::try_from(r.quantity.clamp(0, i64::from(u32::MAX))).unwrap_or(0),
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
                    pin.contents.entry(c.type_id).or_insert_with(|| u64::try_from(c.amount).unwrap_or(0));
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
                        amount: i64::try_from(amt).unwrap_or(i64::MAX),
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
        col.extracted_commodities = sum_pi_pin_contents(&net.pins);
    }
}

pub fn format_pi_pins_links(net: &PiColonyNetworkState) -> (Vec<PyRep>, Vec<PyRep>, Vec<PyRep>) {
    let pins_rep = net.pins.iter().map(|p| PyRep::keyval([
        ("pinID", PyRep::Integer(p.pin_id)), ("typeID", PyRep::Integer(i64::from(p.type_id))),
        ("latitude", PyRep::Real(p.latitude)), ("longitude", PyRep::Real(p.longitude)),
        ("cpuUsage", PyRep::Real(p.cpu_usage)), ("powergridUsage", PyRep::Real(p.pg_usage)),
        ("schematicID", PyRep::Integer(i64::from(p.schematic_id.unwrap_or(0)))), ("headCount", PyRep::Integer(i64::from(p.extractor_head_count))),
    ])).collect();
    let links_rep = net.links.iter().map(|l| PyRep::keyval([
        ("endpoint1", PyRep::Integer(l.source_pin_id)), ("endpoint2", PyRep::Integer(l.dest_pin_id)),
        ("sourcePinID", PyRep::Integer(l.source_pin_id)), ("destPinID", PyRep::Integer(l.dest_pin_id)),
        ("level", PyRep::Integer(i64::from(l.level))), ("lengthKm", PyRep::Real(l.length_km)),
        ("cpuUsage", PyRep::Real(l.cpu_usage)), ("powergridUsage", PyRep::Real(l.pg_usage)), ("bandwidth", PyRep::Real(l.bandwidth)),
    ])).collect();
    let routes_rep = net.routes.iter().map(|r| PyRep::keyval([
        ("routeID", PyRep::Integer(r.route_id)), ("commodityTypeID", PyRep::Integer(i64::from(r.commodity_type_id))),
        ("quantity", PyRep::Integer(i64::from(r.quantity))), ("path", PyRep::List(r.path.iter().map(|&id| PyRep::Integer(id)).collect())),
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
    let now_ft = ctx.now_blueos_filetime_ticks();
    let now_unix_i64 = i64::try_from(now_unix).unwrap_or(i64::MAX);
    let notif_id = -(7_900_000_000_000 + i64::from(type_id) * 100_000_000 + ((pin_id.abs() ^ now_unix_i64) & 0x03FF_FFFF));
    let data_str = format!("{event}:{planet_id}:{pin_id}");
    if let Ok(mut recs) = ctx.notification_records.write() {
        if recs.iter().any(|r| r.receiver_id == recipient_char_id && r.notification_id == notif_id && !r.deleted) {
            return;
        }
        recs.push(crate::router::cache_notifications::NotificationRecord {
            notification_id: notif_id,
            type_id,
            group_id: 1,
            sender_id: recipient_char_id,
            receiver_id: recipient_char_id,
            processed: false,
            created: now_unix_i64,
            data: data_str,
            deleted: false,
            interacted: false,
        });
    }
    match event {
        "PlanetStorageFull" => crate::router::notify_registry::emit_typed_target_to_char_with_id_type(
            ctx.next_notification_seq_id(),
            Some(&*ctx.pending_notifications),
            None,
            recipient_char_id,
            None,
            <crate::router::notify_registry::PlanetStorageFull as crate::router::notify_registry::TypedNotification>::DEFAULT_ID_TYPE,
            now_ft,
            crate::router::notify_registry::PlanetStorageFull { planet_id, pin_id },
        ),
        "PlanetExtractorFinished" => crate::router::notify_registry::emit_typed_target_to_char_with_id_type(
            ctx.next_notification_seq_id(),
            Some(&*ctx.pending_notifications),
            None,
            recipient_char_id,
            None,
            <crate::router::notify_registry::PlanetExtractorFinished as crate::router::notify_registry::TypedNotification>::DEFAULT_ID_TYPE,
            now_ft,
            crate::router::notify_registry::PlanetExtractorFinished { planet_id, pin_id },
        ),
        _ => {}
    }
}

fn bounded_f64_to_u64(val: f64) -> u64 {
    if val.is_nan() || val <= 0.0 {
        0
    } else if val >= (u64::MAX as f64) {
        u64::MAX
    } else {
        val as u64
    }
}

/// Sums commodity quantities stored across all pins in a colony network.
pub fn sum_pi_pin_contents(pins: &[PiPinState]) -> HashMap<TypeId, u64> {
    let mut pin_totals: HashMap<TypeId, u64> = HashMap::new();
    for p in pins {
        for (&tid, &amt) in &p.contents {
            if amt > 0 {
                let slot = pin_totals.entry(tid).or_insert(0);
                *slot = slot.saturating_add(amt);
            }
        }
    }
    pin_totals
}

struct ExtractorCycleEvent {
    cycle_end: u64,
    cycle_start: u64,
    pin_idx: usize,
    target_pin_id: i64,
    p0_tid: TypeId,
    heads: u32,
    overlap: f64,
    cycle_scale: f64,
    min_batch: u64,
    unit_vol: f64,
    cycle_pos: f64,
}

fn extractor_active_window(
    sde: &eve_sde::SdeRegistry,
    pin: &PiPinState,
    default_p0: impl FnOnce() -> TypeId,
) -> Option<(TypeId, (u64, u64))> {
    if !is_extractor_pin_sde(sde, pin)
        || pin.extractor_cycle_seconds == 0
        || pin.extractor_head_count == 0
        || pin.program_install_unix == 0
    {
        return None;
    }
    let exp = pin
        .expiry_time_unix
        .unwrap_or_else(|| pin.program_install_unix.saturating_add(86_400));
    if exp == 0 || pin.program_install_unix >= exp {
        return None;
    }
    let tid = pin.extractor_resource_type_id.unwrap_or_else(default_p0);
    Some((tid, (pin.program_install_unix, exp)))
}

/// Collects active `[program_install_unix, expiry_time_unix)` windows keyed by P0 `TypeId`
/// across every colony on `planet_id`, substituting `override_net` for the caller's live network.
pub(crate) fn collect_planet_extractor_windows(
    sde: &eve_sde::SdeRegistry,
    snap: &WorldStateSnapshot,
    planet_id: i64,
    override_net: Option<(i64, &PiColonyNetworkState)>,
) -> HashMap<TypeId, Vec<(u64, u64)>> {
    let mut windows_by_type: HashMap<TypeId, Vec<(u64, u64)>> = HashMap::new();
    let mut default_p0_cache: Option<TypeId> = None;
    let mut push_net = |sys_id: Option<u32>, pins: &[PiPinState]| {
        for pin in pins {
            if let Some((tid, window)) = extractor_active_window(sde, pin, || {
                *default_p0_cache.get_or_insert_with(|| {
                    list_sde_planet_p0_resources_sde(sde, sys_id, planet_id)
                        .first()
                        .copied()
                        .unwrap_or(2268)
                })
            }) {
                windows_by_type.entry(tid).or_default().push(window);
            }
        }
    };
    let mut seen_override = false;
    let mut known_sys_id: Option<u32> = None;
    for (&(cid, pid), c) in &snap.planetary_colonies {
        if pid != planet_id {
            continue;
        }
        known_sys_id.get_or_insert(c.solar_system_id);
        if let Some((ov_cid, ov_net)) = override_net {
            if cid == ov_cid {
                push_net(Some(c.solar_system_id), &ov_net.pins);
                seen_override = true;
                continue;
            }
        }
        let other_net = load_pi_network(snap, cid, pid, c.command_center_level);
        push_net(Some(c.solar_system_id), &other_net.pins);
    }
    if !seen_override {
        if let Some((_, ov_net)) = override_net {
            push_net(known_sys_id, &ov_net.pins);
        }
    }
    windows_by_type
}

pub fn advance_colony_simulation(
    ctx: &ServerContext,
    col: &mut PlanetaryColonyRecord,
    net: &mut PiColonyNetworkState,
    elapsed_sec: u64,
    now_unix: u64,
) -> (u64, u64) {
    let empty_windows = HashMap::new();
    let (extracted, processed, _) =
        advance_colony_simulation_detailed(ctx, None, &empty_windows, col, net, elapsed_sec, now_unix);
    (extracted, processed)
}

pub(crate) fn advance_colony_and_update_depletion(
    ctx: &ServerContext,
    snap: &mut WorldStateSnapshot,
    planet_id: i64,
    col: &mut PlanetaryColonyRecord,
    net: &mut PiColonyNetworkState,
    baseline_unix: u64,
    elapsed_sec: u64,
    now_unix: u64,
    always_persist_idle_depletion: bool,
) -> (u64, u64) {
    let mut working_depl = load_planet_depletion(snap, planet_id);
    working_depl.planet_id = planet_id;
    let windows_by_type = collect_planet_extractor_windows(
        &ctx.sde,
        snap,
        planet_id,
        Some((col.owner_id, net)),
    );
    let (e, p, _) = advance_colony_simulation_detailed(
        ctx,
        Some(&mut working_depl),
        &windows_by_type,
        col,
        net,
        elapsed_sec,
        now_unix,
    );
    let has_existing_state = !working_depl.resource_depletion.is_empty()
        || !working_depl.local_depletion.is_empty()
        || working_depl.gm_depletion_rate > 0.0
        || !working_depl.colony_extraction_pressure.is_empty()
        || working_depl.colony_pressure_last_unix > 0
        || !working_depl.colony_pressure_last_unix_by_type.is_empty()
        || working_depl.last_updated_unix > 0;
    if e > 0 || always_persist_idle_depletion || has_existing_state {
        decay_colony_extraction_pressure_with_windows(&mut working_depl, &windows_by_type, now_unix);
        if working_depl.last_updated_unix == 0 && baseline_unix > 0 && baseline_unix < now_unix {
            working_depl.last_updated_unix = baseline_unix;
        }
        regenerate_gm_depletion(&mut working_depl, now_unix);
        save_planet_depletion(snap, &working_depl);
    }
    (e, p)
}

pub(crate) fn advance_colony_simulation_detailed(
    ctx: &ServerContext,
    mut working_depl: Option<&mut PlanetResourceDepletionState>,
    windows_by_type: &HashMap<TypeId, Vec<(u64, u64)>>,
    col: &mut PlanetaryColonyRecord,
    net: &mut PiColonyNetworkState,
    elapsed_sec: u64,
    now_unix: u64,
) -> (u64, u64, HashMap<TypeId, u64>) {
    if elapsed_sec == 0 {
        return (0, 0, HashMap::new());
    }
    if let Some(depl) = working_depl.as_deref_mut() {
        depl.planet_id = col.planet_id;
    }
    let default_p0 = list_sde_planet_p0_resources(ctx, col.solar_system_id, col.planet_id)
        .first()
        .copied()
        .unwrap_or(2268);
    let mut extracted_total = 0_u64;
    let mut processed_total = 0_u64;
    let mut extracted_by_type: HashMap<TypeId, u64> = HashMap::new();

    let mut events: Vec<ExtractorCycleEvent> = Vec::new();
    let sim_start = now_unix.saturating_sub(elapsed_sec);
    for (pin_idx, pin) in net.pins.iter().enumerate() {
        if is_extractor_pin(ctx, pin) && pin.extractor_cycle_seconds > 0 && pin.extractor_head_count > 0 {
            let cyc_secs = pin.extractor_cycle_seconds.max(60);
            let install_ts = if pin.program_install_unix > 0 {
                pin.program_install_unix
            } else {
                sim_start
            };
            let expiry = pin
                .expiry_time_unix
                .unwrap_or_else(|| install_ts.saturating_add(86_400));
            let effective_start = sim_start.max(install_ts);
            let effective_end = now_unix.min(expiry);
            if expiry == 0 || effective_start >= effective_end {
                continue;
            }
            let max_cycles_cap = ((14 * 86_400) / cyc_secs).saturating_add(1).max(200);
            let prev_completed = effective_start.saturating_sub(install_ts) / cyc_secs;
            let total_completed = effective_end.saturating_sub(install_ts) / cyc_secs;
            let cycles = total_completed.saturating_sub(prev_completed).min(max_cycles_cap);
            if cycles == 0 {
                continue;
            }
            let p0_tid = pin.extractor_resource_type_id.unwrap_or(default_p0);
            let prior_depl_cycles = if working_depl.is_some() {
                0.0
            } else {
                pin.extractor_depletion_cycles.max(
                    net.resource_depletion_cycles
                        .get(&p0_tid)
                        .copied()
                        .unwrap_or(0.0),
                )
            };
            let heads = u32::from(pin.extractor_head_count.clamp(1, 10));
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
            let cycle_scale = (cyc_secs as f64) / CANONICAL_EXTRACTOR_CYCLE_SECS;
            let min_batch = bounded_f64_to_u64((10.0 * cycle_scale).round()).max(1);
            for c in 0..cycles {
                let cycle_start = install_ts.saturating_add((prev_completed + c).saturating_mul(cyc_secs));
                let cycle_end = install_ts
                    .saturating_add((prev_completed + c + 1).saturating_mul(cyc_secs))
                    .min(effective_end);
                let cycle_pos = prior_depl_cycles + ((prev_completed + c) as f64) * cycle_scale;
                events.push(ExtractorCycleEvent {
                    cycle_end,
                    cycle_start,
                    pin_idx,
                    target_pin_id,
                    p0_tid,
                    heads,
                    overlap,
                    cycle_scale,
                    min_batch,
                    unit_vol: p0_vol_per_unit,
                    cycle_pos,
                });
            }
        }
    }
    events.sort_by_key(|ev| (ev.cycle_end, ev.pin_idx));
    let pin_index_by_id: HashMap<i64, usize> = net
        .pins
        .iter()
        .enumerate()
        .map(|(i, p)| (p.pin_id, i))
        .collect();
    let mut full_storage_pins: Vec<i64> = Vec::new();
    for ev in events {
        let batch = if let Some(depl) = working_depl.as_deref_mut() {
            regenerate_gm_depletion(depl, ev.cycle_start);
            decay_colony_pressure_for_type(depl, windows_by_type, ev.p0_tid, ev.cycle_start);
            let yield_scale = planet_resource_yield_scale_from_state(depl, ev.p0_tid);
            bounded_f64_to_u64(
                (1_000.0 * f64::from(ev.heads) * ev.overlap * ev.cycle_scale * yield_scale).round(),
            )
            .max(ev.min_batch)
        } else {
            let decay = (-0.015 * ev.cycle_pos).exp();
            bounded_f64_to_u64(
                (1_000.0 * f64::from(ev.heads) * ev.overlap * ev.cycle_scale * decay).round(),
            )
            .max(ev.min_batch)
        };
        if let Some(&t_idx) = pin_index_by_id.get(&ev.target_pin_id) {
            let target_pin = &mut net.pins[t_idx];
            let cap_m3 = pin_storage_capacity_m3(&ctx.sde, target_pin.type_id);
            let cur_vol = pin_contents_volume_m3(&ctx.sde, target_pin);
            let avail_m3 = (cap_m3 - cur_vol).max(0.0);
            let max_units = bounded_f64_to_u64((avail_m3 / ev.unit_vol).floor());
            let actual_deposit = batch.min(max_units);
            if actual_deposit > 0 {
                let pin_slot = target_pin.contents.entry(ev.p0_tid).or_insert(0);
                *pin_slot = pin_slot.saturating_add(actual_deposit);
                let col_slot = col.extracted_commodities.entry(ev.p0_tid).or_insert(0);
                *col_slot = col_slot.saturating_add(actual_deposit);
                let by_type_slot = extracted_by_type.entry(ev.p0_tid).or_insert(0);
                *by_type_slot = by_type_slot.saturating_add(actual_deposit);
                extracted_total = extracted_total.saturating_add(actual_deposit);
                if let Some(depl) = working_depl.as_deref_mut() {
                    *depl
                        .colony_extraction_pressure
                        .entry(ev.p0_tid)
                        .or_insert(0.0) += actual_deposit as f64;
                    if ev.cycle_end > 0 {
                        let ts = depl
                            .colony_pressure_last_unix_by_type
                            .entry(ev.p0_tid)
                            .or_insert(0);
                        *ts = (*ts).max(ev.cycle_end);
                    }
                }
            } else if let Some(depl) = working_depl.as_deref_mut() {
                if ev.cycle_end > 0
                    && (depl.colony_extraction_pressure.contains_key(&ev.p0_tid)
                        || colony_pressure_last_unix_for_type(depl, ev.p0_tid) > 0)
                {
                    let ts = depl
                        .colony_pressure_last_unix_by_type
                        .entry(ev.p0_tid)
                        .or_insert(0);
                    *ts = (*ts).max(ev.cycle_end);
                }
            }
            let new_vol = pin_contents_volume_m3(&ctx.sde, target_pin);
            if new_vol + ev.unit_vol > cap_m3 || (batch > 0 && actual_deposit < batch) {
                if !full_storage_pins.contains(&ev.target_pin_id) {
                    full_storage_pins.push(ev.target_pin_id);
                }
            }
        }
    }

    for pin_idx in 0..net.pins.len() {
        let pin = &net.pins[pin_idx];
        let Some(schem_id) = pin.schematic_id else { continue };
        let Some(schem_info) = ctx.sde.planet_schematics.get(&schem_id) else { continue };
        if !schem_info.pins.is_empty() && !schem_info.pins.contains(&pin.type_id) {
            continue;
        }
        let Some((cyc_secs, inputs, out_tid, out_qty)) = pi_schematic_recipe(&ctx.sde, schem_id) else {
            continue;
        };
        let pin_id = pin.pin_id;
        let all_inputs_routed = inputs.iter().all(|&(in_tid, in_qty)| {
            pin.contents.get(&in_tid).copied().unwrap_or(0) >= in_qty
                || net.routes.iter().any(|r| {
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
            net.pins[pin_idx].last_cycle_unix = Some(now_unix);
            continue;
        }
        let pin_baseline = pin
            .last_cycle_unix
            .filter(|&t| t > 0 && t <= now_unix)
            .unwrap_or(sim_start);
        let pin_elapsed = now_unix.saturating_sub(pin_baseline);
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
        let max_cycles = (pin_elapsed / cyc_secs).min(100);
        let mut cycles_ran = 0_u64;
        let mut stalled = false;
        for _ in 0..max_cycles {
            // Verify destination pin capacity before processor cycle execution
            let Some(dst_p) = net.pins.iter().find(|p| p.pin_id == out_target_pin_id) else {
                stalled = true;
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
                stalled = true;
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
                stalled = true;
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
            let col_out = col.extracted_commodities.entry(out_tid).or_insert(0);
            *col_out = col_out.saturating_add(out_qty);
            if let Some(dst_p) = net.pins.iter_mut().find(|p| p.pin_id == out_target_pin_id) {
                let dst_out = dst_p.contents.entry(out_tid).or_insert(0);
                *dst_out = dst_out.saturating_add(out_qty);
            }
            processed_total = processed_total.saturating_add(out_qty);
            cycles_ran += 1;
        }
        net.pins[pin_idx].last_cycle_unix = Some(if stalled {
            now_unix
        } else {
            pin_baseline.saturating_add(cycles_ran.saturating_mul(cyc_secs))
        });
    }

    // Check for extractor program completion or expiry
    let mut finished_extractors: Vec<i64> = Vec::new();
    for pin in net.pins.iter_mut() {
        let is_ext = is_extractor_pin(ctx, pin);
        let effective_exp = pin.expiry_time_unix.or_else(|| {
            (pin.program_install_unix > 0 && pin.extractor_cycle_seconds > 0)
                .then(|| pin.program_install_unix.saturating_add(86_400))
        });
        if is_ext && effective_exp.is_some_and(|exp| exp > 0 && now_unix >= exp) {
            let elapsed_depl = extractor_elapsed_canonical_cycles(pin, effective_exp.unwrap_or(now_unix));
            if elapsed_depl > 0.0 {
                let p0_tid = pin.extractor_resource_type_id.unwrap_or(default_p0);
                pin.extractor_depletion_cycles += elapsed_depl;
                let entry = net.resource_depletion_cycles.entry(p0_tid).or_insert(0.0);
                *entry = (*entry).max(pin.extractor_depletion_cycles);
            }
            finished_extractors.push(pin.pin_id);
            pin.program_install_unix = 0;
            pin.expiry_time_unix = Some(0);
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
    (extracted_total, processed_total, extracted_by_type)
}

pub(crate) fn colony_sim_baseline(
    col: &PlanetaryColonyRecord,
    net: &PiColonyNetworkState,
) -> u64 {
    if col.last_extracted_unix > 0 {
        col.last_extracted_unix
    } else {
        net.last_simulated_unix
    }
}

pub fn sync_character_pi_colonies_on_login(ctx: &ServerContext, char_id: i64) {
    let now_unix = ctx.now_unix();
    let _ = ctx.db.try_with_mut_snapshot(|snap| -> Result<(), BridgeRpcError> {
        let keys: Vec<(i64, i64)> = snap
            .planetary_colonies
            .keys()
            .filter(|&&(cid, _)| cid == char_id)
            .copied()
            .collect();
        for (cid, pid) in keys {
            let Some(mut col) = snap.planetary_colonies.get(&(cid, pid)).cloned() else {
                continue;
            };
            let mut net = load_pi_network(snap, cid, pid, col.command_center_level);
            let baseline = colony_sim_baseline(&col, &net);
            if baseline > 0 && now_unix > baseline {
                let elapsed = now_unix.saturating_sub(baseline);
                let (ext, proc_qty) = advance_colony_and_update_depletion(
                    ctx,
                    snap,
                    pid,
                    &mut col,
                    &mut net,
                    baseline,
                    elapsed,
                    now_unix,
                    false,
                );
                net.pending_login_extracted = net.pending_login_extracted.saturating_add(ext);
                net.pending_login_processed = net.pending_login_processed.saturating_add(proc_qty);
                snap.planetary_colonies.insert((cid, pid), col);
                save_pi_network(snap, cid, pid, &net);
            }
        }
        Ok(())
    });
}

/// Advances all planetary colonies in the background cluster tick (`#736`).
/// Per-processor `last_cycle_unix` preserves sub-cycle elapsed time across 1 Hz ticks so
/// 1,800s / 3,600s schematics complete accurately instead of truncating `1 / 1800 == 0`.
pub fn tick_all_colonies_background(ctx: &ServerContext, now_unix: u64) {
    let colony_baseline = |col: &PlanetaryColonyRecord, net: &PiColonyNetworkState| -> u64 {
        let base = colony_sim_baseline(col, net);
        if base > 0 {
            base
        } else {
            net.pins
                .iter()
                .filter_map(|p| p.last_cycle_unix.filter(|&t| t > 0 && t < now_unix))
                .min()
                .unwrap_or(0)
        }
    };
    let has_due = ctx.db.with_snapshot(|snap| {
        snap.planetary_colonies.iter().any(|(&(cid, pid), c)| {
            let net = load_pi_network(snap, cid, pid, c.command_center_level);
            let b = colony_baseline(c, &net);
            b > 0 && now_unix > b
        })
    });
    if !has_due {
        return;
    }
    ctx.db.with_mut_snapshot(|snap| {
        let keys: Vec<(i64, i64)> = snap.planetary_colonies.keys().copied().collect();
        for (cid, pid) in keys {
            let Some(mut col) = snap.planetary_colonies.get(&(cid, pid)).cloned() else {
                continue;
            };
            let mut net = load_pi_network(snap, cid, pid, col.command_center_level);
            let baseline = colony_baseline(&col, &net);
            if baseline > 0 && now_unix > baseline {
                let elapsed = now_unix.saturating_sub(baseline);
                let (ext, proc_qty) = advance_colony_and_update_depletion(
                    ctx,
                    snap,
                    pid,
                    &mut col,
                    &mut net,
                    baseline,
                    elapsed,
                    now_unix,
                    false,
                );
                net.pending_login_extracted = net.pending_login_extracted.saturating_add(ext);
                net.pending_login_processed = net.pending_login_processed.saturating_add(proc_qty);
                snap.planetary_colonies.insert((cid, pid), col);
                save_pi_network(snap, cid, pid, &net);
            }
        }
    });
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

pub fn resolve_orbital_and_planet(snap: &eve_db::WorldStateSnapshot, planet_or_orbital_id: i64) -> (Option<i64>, i64) {
    if let Some(st) = snap
        .structures
        .get(&planet_or_orbital_id)
        .filter(|s| crate::services::station_structure::structure_lifecycle::is_in_space_structure_state(&s.state))
    {
        if let Some(pid) = st.name.strip_prefix("Orbital:").and_then(|s| s.parse::<i64>().ok()) {
            return (Some(st.structure_id), pid);
        }
    }
    let tag = format!("Orbital:{planet_or_orbital_id}");
    if let Some(st) = snap
        .structures
        .values()
        .find(|s| crate::services::station_structure::structure_lifecycle::is_in_space_structure_state(&s.state) && s.name == tag)
    {
        return (Some(st.structure_id), planet_or_orbital_id);
    }
    (None, planet_or_orbital_id)
}

pub fn load_orbital_settings(snap: &eve_db::WorldStateSnapshot, planet_or_orbital_id: i64) -> Option<OrbitalCustomsSettings> {
    let nid = orbital_settings_note_id(planet_or_orbital_id);
    if let Some(cfg) = snap.owner_notes.get(&nid).and_then(|n| serde_json::from_str::<OrbitalCustomsSettings>(&n.note).ok()) {
        return Some(cfg);
    }
    let (_, planet_id) = resolve_orbital_and_planet(snap, planet_or_orbital_id);
    (planet_id != planet_or_orbital_id)
        .then(|| orbital_settings_note_id(planet_id))
        .and_then(|pid_nid| snap.owner_notes.get(&pid_nid))
        .and_then(|n| serde_json::from_str::<OrbitalCustomsSettings>(&n.note).ok())
}

pub fn save_orbital_settings(snap: &mut eve_db::WorldStateSnapshot, planet_or_orbital_id: i64, settings: &OrbitalCustomsSettings) {
    let (orbital_id_opt, resolved_planet_id) = resolve_orbital_and_planet(snap, planet_or_orbital_id);
    let planet_id = if settings.planet_id > 0 { settings.planet_id } else { resolved_planet_id };
    let nid = orbital_settings_note_id(planet_id);
    if let Ok(js) = serde_json::to_string(settings) {
        snap.owner_notes.insert(nid, eve_db::OwnerNoteRecord {
            note_id: nid, character_id: -4, item_id: planet_id,
            label: format!("orbital_settings:{planet_id}"), note: js, timestamp_unix: 0,
        });
    }
    let sys_id = snap.planetary_colonies.values()
        .find(|c| c.planet_id == planet_id || c.planet_id == planet_or_orbital_id)
        .map(|c| c.solar_system_id)
        .or_else(|| {
            let (tag_a, tag_b) = (format!("Orbital:{planet_id}"), format!("Orbital:{planet_or_orbital_id}"));
            snap.structures.values().find(|s| s.structure_id == planet_or_orbital_id || s.name == tag_a || s.name == tag_b).map(|s| s.solar_system_id)
        })
        .or_else(|| {
            snap.items.get(&planet_id).or_else(|| snap.items.get(&planet_or_orbital_id))
                .and_then(|it| u32::try_from(it.location_id).ok().filter(|id| (30_000_000..=33_000_000).contains(id)))
        })
        .or_else(|| {
            snap.characters.values().find(|c| c.corporation_id == settings.owner_corp_id).map(|c| c.solar_system_id)
        })
        .or_else(|| snap.characters.values().next().map(|c| c.solar_system_id))
        .unwrap_or(0);
    let office_id = orbital_id_opt.unwrap_or(planet_id);
    let standing_lvl = if settings.minimum_standing > 5.0 { Some("excellent".to_string()) }
        else if settings.minimum_standing > 0.0 { Some("good".to_string()) }
        else if settings.minimum_standing >= -0.01 { Some("neutral".to_string()) }
        else if settings.minimum_standing >= -5.0 { Some("bad".to_string()) }
        else { Some("terrible".to_string()) };
    let rec = eve_db::CustomsOfficeRecord {
        office_id, system_id: sys_id, reinforce_exit_start: 18, reinforce_exit_end: 20,
        allow_alliance_access: settings.allow_alliance, allow_access_with_standings: settings.allow_standings,
        standing_level: standing_lvl, corporation_tax_rate: Some(settings.corporation_tax_rate),
        alliance_tax_rate: Some(settings.alliance_tax_rate), excellent_standing_tax_rate: Some(settings.standing_high_tax_rate),
        good_standing_tax_rate: Some(settings.standing_good_tax_rate), neutral_standing_tax_rate: Some(settings.standing_neutral_tax_rate),
        bad_standing_tax_rate: Some(settings.standing_bad_tax_rate), terrible_standing_tax_rate: Some(settings.standing_horrible_tax_rate),
    };
    let offices = snap.corp_customs_offices.entry(settings.owner_corp_id).or_default();
    if let Some(existing) = offices.iter_mut().find(|o| o.office_id == office_id || o.office_id == planet_id) {
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
        return Ok((cfg.standing_neutral_tax_rate.max(0.17), 4318));
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
                    client_error: None,
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
            client_error: None,
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
            client_error: None,
        });
    };
    if target_level <= col.command_center_level {
        return Err(BridgeRpcError {
            code: "InvalidCommandCenterLevel".into(),
            message: format!("Target level {target_level} must be greater than current level {}", col.command_center_level),
            client_error: None,
        });
    }
    let cc_skill_lvl = snap.skills.get(&char_id).and_then(|m| m.get(&2505)).map(|s| s.level).unwrap_or(0);
    if cc_skill_lvl < target_level {
        return Err(BridgeRpcError {
            code: "MissingSkillForCommandCenterUpgrade".into(),
            message: format!("Command Center upgrade to level {target_level} requires Command Center Upgrades skill (2505) >= {target_level} (current: {cc_skill_lvl})"),
            client_error: None,
        });
    }
    let upgrade_cost_isk = cc_upgrade_cost_isk(col.command_center_level, target_level);
    let upgrade_cost_cents = IskCents::from_f64_isk(upgrade_cost_isk);
    if let Some(ch) = snap.characters.get_mut(&char_id) {
        if ch.balance < upgrade_cost_cents {
            return Err(BridgeRpcError {
                code: "InsufficientFunds".into(),
                message: format!("Insufficient funds for command center upgrade ({upgrade_cost_isk:.2} ISK required)"),
                client_error: None,
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
    col_mut.pins_count = u32::from(target_level) + 1;
    let new_pins = col_mut.pins_count;

    Ok(PyRep::keyval([
        ("planetID", PyRep::Integer(planet_id)),
        ("commandCenterLevel", PyRep::Integer(i64::from(target_level))),
        ("pinsCount", PyRep::Integer(i64::from(new_pins))),
        ("upgradeCostISK", PyRep::Real(upgrade_cost_isk)),
        ("cpuMax", PyRep::Real(new_cpu_max)),
        ("powergridMax", PyRep::Real(new_pg_max)),
    ]))
}

fn item_matches_pi_import_location(
    snap: &WorldStateSnapshot,
    it: &eve_db::InventoryItemRecord,
    char_id: i64,
    planet_id: i64,
    type_id: TypeId,
    ship_id_opt: Option<i64>,
    station_id_opt: Option<i64>,
    structure_id_opt: Option<i64>,
) -> bool {
    if it.owner_id != char_id
        || it.type_id != type_id
        || it.quantity <= 0
        || it.flag == eve_db::InventoryFlag::Locked
        || snap.is_item_or_ancestor_locked_or_impounded(it.item_id)
    {
        return false;
    }
    let in_cargo = ship_id_opt.is_some_and(|sid| it.location_id == sid)
        && matches!(
            it.flag,
            eve_db::InventoryFlag::Cargo
                | eve_db::InventoryFlag::SpecializedPlanetaryCommoditiesHold
        );
    let in_customs_or_hangar = (it.location_id == planet_id
        || snap
            .sp6_lifecycle
            .space_object_meta
            .get(&it.location_id)
            .and_then(|m| m.planet_id)
            == Some(planet_id)
        || station_id_opt.is_some_and(|sid| it.location_id == sid)
        || structure_id_opt.is_some_and(|sid| it.location_id == sid))
        && it.flag == eve_db::InventoryFlag::Hangar;
    in_cargo || in_customs_or_hangar
}

/// Validates and plans stack consumptions for importing `qty` units of `type_id` into a colony
/// without mutating `snap`. Rejects singletons, BPCs, dynamic/mutated items, and overflow slices.
pub fn plan_pi_import_consumptions(
    snap: &WorldStateSnapshot,
    char_id: i64,
    planet_id: i64,
    type_id: TypeId,
    qty: u64,
    ship_id_opt: Option<i64>,
    station_id_opt: Option<i64>,
    structure_id_opt: Option<i64>,
) -> Result<Vec<(i64, i32)>, BridgeRpcError> {
    let total_qty = ItemQty::try_from_u64(qty)
        .map_err(|e| e.into_rpc_error("InvalidCommodityQuantity"))?;

    let opts = eve_db::ConsumeExactOptions::for_character_commodity(char_id);
    let mut matching_item_ids: Vec<i64> = Vec::new();
    let mut first_invalid_state_err: Option<String> = None;

    for it in snap.items.values() {
        if !item_matches_pi_import_location(
            snap,
            it,
            char_id,
            planet_id,
            type_id,
            ship_id_opt,
            station_id_opt,
            structure_id_opt,
        ) {
            continue;
        }
        if let Err(err) = snap.validate_item_for_consumption(it, &opts) {
            if first_invalid_state_err.is_none() {
                first_invalid_state_err = Some(err.to_string());
            }
            continue;
        }
        matching_item_ids.push(it.item_id);
    }

    matching_item_ids.sort();

    let total_available: u128 = matching_item_ids
        .iter()
        .filter_map(|iid| snap.items.get(iid))
        .filter_map(|it| u64::try_from(it.quantity).ok())
        .map(u128::from)
        .sum();

    if total_available == 0 {
        if let Some(reason) = first_invalid_state_err {
            return Err(BridgeRpcError {
                code: "InvalidCommodityItemState".into(),
                message: format!(
                    "Commodity item type {type_id} cannot be imported because matching items are non-stackable or modified ({reason})"
                ),
                client_error: None,
            });
        }
        return Err(BridgeRpcError {
            code: "ItemNotFound".into(),
            message: format!(
                "Commodity item type {type_id} not found in ship cargo or customs office hangar"
            ),
            client_error: None,
        });
    }
    if total_available < u128::from(qty) {
        if let Some(reason) = first_invalid_state_err {
            return Err(BridgeRpcError {
                code: "InvalidCommodityItemState".into(),
                message: format!(
                    "Available stackable commodity quantity ({total_available}) is less than required import quantity ({qty}); ineligible items skipped ({reason})"
                ),
                client_error: None,
            });
        }
        return Err(BridgeRpcError {
            code: "InsufficientCommodityQuantity".into(),
            message: format!(
                "Available quantity ({total_available}) is less than required import quantity ({qty})"
            ),
            client_error: None,
        });
    }

    let mut plan = Vec::with_capacity(matching_item_ids.len());
    let mut remaining = total_qty.as_u64();
    for iid in matching_item_ids {
        if remaining == 0 {
            break;
        }
        let Some(avail_i32) = snap.items.get(&iid).map(|it| it.quantity) else {
            continue;
        };
        let Ok(avail_qty) = ItemQty::try_from_i32(avail_i32) else {
            continue;
        };
        let take_u64 = avail_qty.as_u64().min(remaining);
        let take = ItemQty::try_from_u64(take_u64)
            .and_then(ItemQty::try_into_stack_i32)
            .map_err(|e| e.into_rpc_error("InvalidCommodityQuantity"))?;
        plan.push((iid, take));
        remaining -= take_u64;
    }

    if remaining != 0 {
        return Err(BridgeRpcError {
            code: "InsufficientCommodityQuantity".into(),
            message: format!(
                "Unable to allocate full import quantity ({qty}); {remaining} remaining"
            ),
            client_error: None,
        });
    }

    Ok(plan)
}

/// Applies a pre-validated consumption plan generated by [`plan_pi_import_consumptions`].
pub fn apply_pi_import_consumptions(
    snap: &mut WorldStateSnapshot,
    char_id: i64,
    plan: &[(i64, i32)],
) -> Result<(), BridgeRpcError> {
    let opts = eve_db::ConsumeExactOptions::for_character_commodity(char_id);
    for &(iid, take) in plan {
        if take <= 0 {
            continue;
        }
        snap.consume_exact_item_quantity(iid, take, opts)
            .map_err(|e| BridgeRpcError {
                code: "InventoryError".into(),
                message: e.to_string(),
                client_error: None,
            })?;
    }
    Ok(())
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
    let plan = plan_pi_import_consumptions(
        snap,
        char_id,
        planet_id,
        type_id,
        qty,
        ship_id_opt,
        station_id_opt,
        structure_id_opt,
    )?;
    apply_pi_import_consumptions(snap, char_id, &plan)
}

/// Parses a `{typeID: quantity}` `PyRep::Dict` or `PyRep::KeyVal` commodity map.
pub fn parse_pi_commodities_map(rep: &PyRep) -> Result<BTreeMap<TypeId, u64>, BridgeRpcError> {
    let mut comm_map: BTreeMap<TypeId, u64> = BTreeMap::new();
    let add_entry = |map: &mut BTreeMap<TypeId, u64>, tid_raw: i64, qty_raw: i64| -> Result<(), BridgeRpcError> {
        let tid = u32::try_from(tid_raw).map_err(|_| BridgeRpcError {
            code: "InvalidCommodity".into(),
            message: format!("Commodity type ID {tid_raw} out of range"),
            client_error: None,
        })?;
        let qty = ItemQty::try_from_i64(qty_raw)
            .map_err(|e| e.into_rpc_error("InvalidCommodityQuantity"))?;
        let slot = map.entry(tid).or_insert(0);
        *slot = slot.checked_add(qty.as_u64()).ok_or_else(|| {
            ItemQtyError::Overflow.into_rpc_error("InvalidCommodityQuantity")
        })?;
        Ok(())
    };
    match rep {
        PyRep::Dict(entries) => {
            for (k, v) in entries {
                let tid_raw = k.as_i64().ok_or_else(|| BridgeRpcError {
                    code: "InvalidCommodity".into(),
                    message: "Commodity type ID must be an integer".into(),
                    client_error: None,
                })?;
                let qty_raw = v.as_i64().ok_or_else(|| BridgeRpcError {
                    code: "InvalidCommodityQuantity".into(),
                    message: "Commodity quantity must be an integer".into(),
                    client_error: None,
                })?;
                add_entry(&mut comm_map, tid_raw, qty_raw)?;
            }
        }
        PyRep::KeyVal(entries) => {
            for (k, v) in entries {
                let tid_raw = k.parse::<i64>().map_err(|_| BridgeRpcError {
                    code: "InvalidCommodity".into(),
                    message: "Commodity type ID must be an integer".into(),
                    client_error: None,
                })?;
                let qty_raw = v.as_i64().ok_or_else(|| BridgeRpcError {
                    code: "InvalidCommodityQuantity".into(),
                    message: "Commodity quantity must be an integer".into(),
                    client_error: None,
                })?;
                add_entry(&mut comm_map, tid_raw, qty_raw)?;
            }
        }
        _ => {
            return Err(BridgeRpcError {
                code: "InvalidCommodity".into(),
                message: "Commodities argument must be a dict mapping typeID to quantity".into(),
                client_error: None,
            });
        }
    }
    Ok(comm_map)
}

pub fn user_transfer_commodities_logic(
    ctx: &ServerContext,
    snap: &mut WorldStateSnapshot,
    char_id: i64,
    planet_id: i64,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    let (path_list, comms_rep) = if let Some(PyRep::List(path_list)) = args.first() {
        (path_list, args.get(1).or_else(|| kwargs.get("commodities")))
    } else if let Some(PyRep::List(path_list)) = args.get(1) {
        (path_list, args.get(2).or_else(|| kwargs.get("commodities")))
    } else if let Some(PyRep::List(path_list)) = kwargs.get("path") {
        (path_list, kwargs.get("commodities"))
    } else {
        return Err(BridgeRpcError {
            code: "InvalidTransferParameters".into(),
            message: "UserTransferCommodities requires (path, commodities) with a pin path list and commodities dict".into(),
            client_error: None,
        });
    };

    let path: Vec<i64> = path_list
        .iter()
        .map(|x| {
            x.as_i64().ok_or_else(|| BridgeRpcError {
                code: "PinNotFound".into(),
                message: "Pin IDs in transfer path must be integers".into(),
                client_error: None,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    let comms_rep = comms_rep.ok_or_else(|| BridgeRpcError {
        code: "InvalidCommodity".into(),
        message: "UserTransferCommodities requires a commodities dict".into(),
        client_error: None,
    })?;
    let comm_map = parse_pi_commodities_map(comms_rep)?;

    if path.len() < 2 {
        return Err(BridgeRpcError {
            code: "PinNotFound".into(),
            message: "UserTransferCommodities requires at least source and destination pins".into(),
            client_error: None,
        });
    }
    let src_pin_id = path[0];
    let dst_pin_id = *path.last().unwrap();
    if src_pin_id == dst_pin_id {
        return Err(BridgeRpcError {
            code: "InvalidTransferSource".into(),
            message: "Source and destination pin must be distinct".into(),
            client_error: None,
        });
    }

    if comm_map.is_empty() {
        return Err(BridgeRpcError {
            code: "InvalidCommodity".into(),
            message: "UserTransferCommodities requires at least one commodity to transfer".into(),
            client_error: None,
        });
    }

    let Some(col) = snap.planetary_colonies.get(&(char_id, planet_id)).cloned() else {
        return Err(BridgeRpcError {
            code: "ColonyNotFound".into(),
            message: format!("No colony found for character {char_id} on planet {planet_id}"),
            client_error: None,
        });
    };
    let mut net = load_pi_network(snap, char_id, planet_id, col.command_center_level);

    let src_idx = net.pins.iter().position(|p| p.pin_id == src_pin_id).ok_or_else(|| BridgeRpcError {
        code: "PinNotFound".into(),
        message: format!("Source pin {src_pin_id} not found on planet {planet_id}"),
        client_error: None,
    })?;
    let dst_idx = net.pins.iter().position(|p| p.pin_id == dst_pin_id).ok_or_else(|| BridgeRpcError {
        code: "PinNotFound".into(),
        message: format!("Destination pin {dst_pin_id} not found on planet {planet_id}"),
        client_error: None,
    })?;

    for &comm_type_id in comm_map.keys() {
        ensure_valid_pi_commodity_type(&ctx.sde, comm_type_id)?;
    }

    let total_transfer_vol_m3: f64 = comm_map
        .iter()
        .map(|(&tid, &qty)| {
            let unit_vol = ctx.sde.types.get(&tid).map(|t| t.volume.max(0.01)).unwrap_or(0.38);
            (qty as f64) * unit_vol
        })
        .sum();
    let cooldown_secs = bounded_f64_to_u64((300.0_f64 + 0.03_f64 * total_transfer_vol_m3 * 60.0_f64).round());
    let now_unix = ctx.now_unix();

    // Verify every hop in path exists and link bandwidth accommodates volume
    has_pi_link_path(&net.links, &path, total_transfer_vol_m3)?;

    if net.pins[src_idx].last_expedited_transfer_unix > 0
        && now_unix < net.pins[src_idx].last_expedited_transfer_unix.saturating_add(cooldown_secs)
    {
        return Err(BridgeRpcError {
            code: "ExpeditedTransferOnCooldown".into(),
            message: format!("Source pin {src_pin_id} expedited transfer is on cooldown for {cooldown_secs}s"),
            client_error: None,
        });
    }

    for (&comm_type_id, &qty) in &comm_map {
        let src_qty = net.pins[src_idx].contents.get(&comm_type_id).copied().unwrap_or(0);
        if src_qty < qty {
            return Err(BridgeRpcError {
                code: "InsufficientCommodityStorage".into(),
                message: format!("Source pin {src_pin_id} has {src_qty} of commodity {comm_type_id}, but {qty} is required"),
                client_error: None,
            });
        }
    }

    let dst_cap_m3 = pin_storage_capacity_m3(&ctx.sde, net.pins[dst_idx].type_id);
    let dst_curr_vol = pin_contents_volume_m3(&ctx.sde, &net.pins[dst_idx]);

    if dst_curr_vol + total_transfer_vol_m3 > dst_cap_m3 + 1e-6 {
        return Err(BridgeRpcError {
            code: "PinStorageFull".into(),
            message: format!("Expedited transfer volume {total_transfer_vol_m3:.1} m3 exceeds destination pin available capacity"),
            client_error: None,
        });
    }

    for (&comm_type_id, &qty) in &comm_map {
        let src_entry = net.pins[src_idx].contents.get_mut(&comm_type_id).ok_or_else(|| BridgeRpcError {
            code: "InsufficientCommodityStorage".into(),
            message: format!("Source pin missing commodity {comm_type_id}"),
            client_error: None,
        })?;
        if *src_entry < qty {
            return Err(BridgeRpcError {
                code: "InsufficientCommodityStorage".into(),
                message: format!("Source pin has insufficient commodity {comm_type_id}"),
                client_error: None,
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
            client_error: None,
        })?;
    }
    net.pins[src_idx].last_expedited_transfer_unix = now_unix;

    save_pi_network(snap, char_id, planet_id, &net);
    let sim_ret_ft = ctx.now_blueos_filetime_ticks();
    Ok(PyRep::Tuple(vec![
        PyRep::Integer(sim_ret_ft),
        PyRep::Integer(sim_ret_ft),
    ]))
}

#[cfg(test)]
#[path = "pi_helpers_tests.rs"]
mod pi_helpers_tests;

#[cfg(test)]
mod qty_tests {
    use super::*;
    use eve_db::{InventoryFlag, InventoryItemRecord, WorldRepository};

    #[test]
    fn pi_commodity_parsing_and_import_planning_enforce_item_qty_bounds() {
        let bad_zero = PyRep::Dict(vec![(PyRep::Integer(34), PyRep::Integer(0))]);
        assert_eq!(
            parse_pi_commodities_map(&bad_zero).unwrap_err().code,
            "InvalidCommodityQuantity"
        );
        let bad_neg = PyRep::Dict(vec![(PyRep::Integer(34), PyRep::Integer(-2))]);
        assert_eq!(
            parse_pi_commodities_map(&bad_neg).unwrap_err().code,
            "InvalidCommodityQuantity"
        );
        let bad_tid = PyRep::Dict(vec![(PyRep::Integer(-1), PyRep::Integer(5))]);
        assert_eq!(
            parse_pi_commodities_map(&bad_tid).unwrap_err().code,
            "InvalidCommodity"
        );

        let db = WorldRepository::with_demo_seed();
        let over_signed_max = u64::try_from(i64::MAX).unwrap() + 1;
        db.with_snapshot(|snap| {
            assert_eq!(
                plan_pi_import_consumptions(snap, 90_000_001, 40_009_082, 44, 0, None, None, None)
                    .unwrap_err()
                    .code,
                "InvalidCommodityQuantity"
            );
            assert_eq!(
                plan_pi_import_consumptions(
                    snap,
                    90_000_001,
                    40_009_082,
                    44,
                    over_signed_max,
                    None,
                    None,
                    None,
                )
                .unwrap_err()
                .code,
                "InvalidCommodityQuantity"
            );
        });

        let db_multi = WorldRepository::with_demo_seed();
        let char_id = 90_000_001_i64;
        let planet_id = 40_009_082_i64;
        db_multi.with_mut_snapshot(|snap| {
            snap.items.insert(
                939_701,
                InventoryItemRecord {
                    item_id: 939_701,
                    type_id: 44,
                    owner_id: char_id,
                    location_id: planet_id,
                    flag: InventoryFlag::Hangar,
                    quantity: 1_500_000_000,
                    ..Default::default()
                },
            );
            snap.items.insert(
                939_702,
                InventoryItemRecord {
                    item_id: 939_702,
                    type_id: 44,
                    owner_id: char_id,
                    location_id: planet_id,
                    flag: InventoryFlag::Hangar,
                    quantity: 1_500_000_000,
                    ..Default::default()
                },
            );
        });
        db_multi.with_snapshot(|snap| {
            let plan = plan_pi_import_consumptions(
                snap,
                char_id,
                planet_id,
                44,
                2_500_000_000,
                None,
                None,
                None,
            )
            .expect("multi-stack plan above i32::MAX");
            assert_eq!(plan, vec![(939_701, 1_500_000_000), (939_702, 1_000_000_000)]);
        });

        assert_eq!(bounded_f64_to_u64(-5.0), 0);
        assert_eq!(bounded_f64_to_u64(f64::NAN), 0);
        assert_eq!(bounded_f64_to_u64(f64::INFINITY), u64::MAX);
    }
}

