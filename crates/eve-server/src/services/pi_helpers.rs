//! PI common structures and colony simulation helpers.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::industry_pi::{
    extractor_overlap_factor, list_sde_planet_p0_resources, pi_schematic_recipe,
};
use eve_db::{IskCents, PlanetaryColonyRecord, WorldStateSnapshot};
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::TypeId;
use std::collections::HashMap;

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
    pub last_expedited_transfer_unix: u64,
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
    ctx.sde.types.get(&type_id).map(|t| t.group_id == 1027).unwrap_or(false)
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

pub fn load_pi_network(
    snap: &eve_db::WorldStateSnapshot,
    char_id: i64,
    planet_id: i64,
    cc_level: u8,
) -> PiColonyNetworkState {
    let nid = pi_network_note_id(char_id, planet_id);
    if let Some(note) = snap.owner_notes.get(&nid) {
        if let Ok(mut net) = serde_json::from_str::<PiColonyNetworkState>(&note.note) {
            let (c_max, p_max) = cc_level_cpu_pg(cc_level);
            if net.cpu_max < c_max {
                net.cpu_max = c_max;
            }
            if net.pg_max < p_max {
                net.pg_max = p_max;
            }
            return net;
        }
    }
    let (cpu_max, pg_max) = cc_level_cpu_pg(cc_level);
    PiColonyNetworkState {
        cpu_max,
        pg_max,
        ..Default::default()
    }
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

    for pin in &net.pins {
        let is_ext = pin.type_id == 2848
            || ctx.sde.types.get(&pin.type_id).map(|t| t.group_id == 1026).unwrap_or(false);
        if is_ext && pin.extractor_cycle_seconds > 0 && pin.extractor_head_count > 0 {
            let cyc_secs = pin.extractor_cycle_seconds.max(60);
            let cycles = (elapsed_sec / cyc_secs).min(200);
            let p0_tid = pin.extractor_resource_type_id.unwrap_or(default_p0);
            let heads = u64::from(pin.extractor_head_count.clamp(1, 10));
            let overlap = extractor_overlap_factor(&pin.extractor_heads, pin.extractor_head_radius.max(0.02));
            for c in 0..cycles {
                let decay = (-0.015 * (c as f64)).exp();
                let batch = ((1_000.0 * (heads as f64) * overlap * decay).round() as u64).max(10);
                *col.extracted_commodities.entry(p0_tid).or_insert(0) += batch;
                extracted_total += batch;
            }
        }
    }

    for pin in &net.pins {
        let Some(schem_id) = pin.schematic_id else { continue };
        let (cyc_secs, inputs, out_tid, out_qty) = pi_schematic_recipe(schem_id, default_p0);
        let all_inputs_routed = inputs.iter().all(|&(in_tid, _)| {
            net.routes.iter().any(|r| {
                r.path.last() == Some(&pin.pin_id) && r.commodity_type_id == in_tid && r.path.len() >= 2
            })
        });
        let output_routed = net.routes.iter().any(|r| {
            r.path.first() == Some(&pin.pin_id) && r.commodity_type_id == out_tid && r.path.len() >= 2
        });
        if !all_inputs_routed || !output_routed {
            continue;
        }
        let max_cycles = (elapsed_sec / cyc_secs).min(100);
        for _ in 0..max_cycles {
            if !inputs.iter().all(|&(in_tid, in_qty)| {
                col.extracted_commodities.get(&in_tid).copied().unwrap_or(0) >= in_qty
            }) {
                break;
            }
            for &(in_tid, in_qty) in &inputs {
                *col.extracted_commodities.entry(in_tid).or_insert(0) -= in_qty;
            }
            *col.extracted_commodities.entry(out_tid).or_insert(0) += out_qty;
            processed_total += out_qty;
        }
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

pub fn user_transfer_commodities_logic(
    ctx: &ServerContext,
    snap: &mut WorldStateSnapshot,
    char_id: i64,
    planet_id: i64,
    is_expedited: bool,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    if is_expedited {
        let src_pin_id = args.get(1).and_then(|a| a.as_i64()).or_else(|| kwargs.get("sourcePinID").and_then(|v| v.as_i64())).unwrap_or(0);
        let dst_pin_id = args.get(2).and_then(|a| a.as_i64()).or_else(|| kwargs.get("destPinID").and_then(|v| v.as_i64())).unwrap_or(0);
        let comm_type_id = args.get(3).and_then(|a| a.as_i64()).or_else(|| kwargs.get("commodityTypeID").and_then(|v| v.as_i64())).unwrap_or(3645) as TypeId;
        let qty = args.get(4).and_then(|a| a.as_i64()).or_else(|| kwargs.get("quantity").and_then(|v| v.as_i64())).unwrap_or(100).max(1) as u64;
        let unit_vol = ctx.sde.types.get(&comm_type_id).map(|t| t.volume.max(0.01)).unwrap_or(0.38);
        let transfer_vol_m3 = (qty as f64) * unit_vol;
        let cooldown_secs = (300.0 + 0.03 * transfer_vol_m3 * 60.0).round() as u64;
        let now_unix = ctx.now_unix();

        let Some(col) = snap.planetary_colonies.get(&(char_id, planet_id)).cloned() else {
            return Err(BridgeRpcError { code: "ColonyNotFound".into(), message: format!("No colony found for character {char_id} on planet {planet_id}") });
        };
        let mut net = load_pi_network(snap, char_id, planet_id, col.command_center_level);
        let dst_cap_m3 = if let Some(dp) = net.pins.iter().find(|p| p.pin_id == dst_pin_id) {
            match ctx.sde.types.get(&dp.type_id).map(|t| t.group_id).unwrap_or(0) {
                1027 => 500.0, 1030 => 10_000.0, 1029 => 12_000.0, _ => 10_000.0,
            }
        } else {
            10_000.0
        };
        if transfer_vol_m3 > dst_cap_m3 {
            return Err(BridgeRpcError { code: "PinStorageFull".into(), message: format!("Expedited transfer volume {transfer_vol_m3:.1} m3 exceeds destination pin capacity {dst_cap_m3:.1} m3") });
        }
        if let Some(sp) = net.pins.iter_mut().find(|p| p.pin_id == src_pin_id) {
            if sp.last_expedited_transfer_unix > 0 && now_unix < sp.last_expedited_transfer_unix.saturating_add(cooldown_secs) {
                return Err(BridgeRpcError { code: "ExpeditedTransferOnCooldown".into(), message: format!("Source pin {src_pin_id} expedited transfer is on cooldown for {cooldown_secs}s") });
            }
            sp.last_expedited_transfer_unix = now_unix;
        }
        save_pi_network(snap, char_id, planet_id, &net);
        return Ok(PyRep::keyval([
            ("planetID", PyRep::Integer(planet_id)), ("sourcePinID", PyRep::Integer(src_pin_id)),
            ("destPinID", PyRep::Integer(dst_pin_id)), ("commodityTypeID", PyRep::Integer(comm_type_id as i64)),
            ("quantity", PyRep::Integer(qty as i64)), ("volumeM3", PyRep::Real(transfer_vol_m3)),
            ("cooldownSeconds", PyRep::Integer(cooldown_secs as i64)),
        ]));
    }

    let Some(input_type_id_raw) = args.get(1).and_then(|a| a.as_i64()) else {
        return Err(BridgeRpcError { code: "InvalidProcessorParameters".into(), message: "UserTransferCommodities requires input commodity typeID".into() });
    };
    let Some(output_type_id_raw) = args.get(2).and_then(|a| a.as_i64()) else {
        return Err(BridgeRpcError { code: "InvalidProcessorParameters".into(), message: "UserTransferCommodities requires output commodity typeID".into() });
    };
    let input_type_id = input_type_id_raw as TypeId;
    let output_type_id = output_type_id_raw as TypeId;
    let cycles = args.get(3).and_then(|a| a.as_i64()).unwrap_or(1).max(1) as u64;
    if cycles > 20_000 {
        return Err(BridgeRpcError { code: "PinStorageFull".into(), message: format!("Requested transfer/processing quantity {cycles} exceeds planetary pin storage capacity") });
    }
    let in_grp = ctx.sde.types.get(&input_type_id).map(|t| t.group_id).unwrap_or(0);
    let (input_per_cycle, output_per_cycle) = match in_grp {
        1032 => (40_u64, 5_u64), 1034 => (10_u64, 3_u64), 1040 => (6_u64, 1_u64), _ => (3_000_u64, 20_u64),
    };
    let req_input = input_per_cycle.saturating_mul(cycles);
    let produced_output = output_per_cycle.saturating_mul(cycles);

    let Some(col) = snap.planetary_colonies.get_mut(&(char_id, planet_id)) else {
        return Err(BridgeRpcError { code: "ColonyNotFound".into(), message: format!("No colony found for character {char_id} on planet {planet_id}") });
    };
    let current_input = col.extracted_commodities.get(&input_type_id).copied().unwrap_or(0);
    if current_input < req_input {
        return Err(BridgeRpcError { code: "InsufficientInputCommodities".into(), message: format!("Colony storage has {current_input}x type {input_type_id}, but {req_input}x is required for {cycles} processor cycles") });
    }
    *col.extracted_commodities.entry(input_type_id).or_insert(0) -= req_input;
    *col.extracted_commodities.entry(output_type_id).or_insert(0) += produced_output;
    let total_output = col.extracted_commodities[&output_type_id];

    Ok(PyRep::keyval([
        ("planetID", PyRep::Integer(planet_id)),
        ("inputTypeID", PyRep::Integer(input_type_id as i64)),
        ("inputQuantityConsumed", PyRep::Integer(req_input as i64)),
        ("outputTypeID", PyRep::Integer(output_type_id as i64)),
        ("outputQuantityProduced", PyRep::Integer(produced_output as i64)),
        ("totalStoredOutput", PyRep::Integer(total_output as i64)),
    ]))
}
