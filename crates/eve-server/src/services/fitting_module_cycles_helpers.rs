//! Helpers for active module cycle execution (`feat:mining.survey-scanner`, `dogmaIM.Activate`).

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::system_actor::SystemHandle;
use eve_db::InventoryFlag;
use eve_dogma::{DogmaItemState, ShipFittingEngine};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::{attr_ids, InvType};
use glam::DVec3;
use std::collections::HashMap;

pub(crate) fn set_modules_reload_until(
    ctx: &ServerContext,
    module_ids: &[i64],
    reload_until_unix: u64,
) {
    ctx.db.with_mut_snapshot(|snap| {
        let mut map: HashMap<i64, u64> = snap
            .service_state_blobs
            .get("module_reload_until_unix")
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default();
        for &mid in module_ids {
            map.insert(mid, reload_until_unix);
        }
        if let Ok(json) = serde_json::to_string(&map) {
            snap.service_state_blobs
                .insert("module_reload_until_unix".into(), json);
        }
    });
}

pub(crate) fn check_module_reloading(
    ctx: &ServerContext,
    ship_id: i64,
    module_id: i64,
) -> Result<(), BridgeRpcError> {
    let now_unix = ctx.now_unix_secs();
    let reloading_until = ctx.db.with_snapshot(|snap| {
        let map: HashMap<i64, u64> = snap
            .service_state_blobs
            .get("module_reload_until_unix")
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default();
        if let Some(&until) = map.get(&module_id) {
            if now_unix < until {
                return Some(until);
            }
        }
        if let Some(bank) = snap.weapon_banks.get(&ship_id).and_then(|b| b.get(&module_id)) {
            for &mid in bank {
                if let Some(&until) = map.get(&mid) {
                    if now_unix < until {
                        return Some(until);
                    }
                }
            }
        }
        None
    });
    if let Some(until) = reloading_until {
        return Err(BridgeRpcError {
            code: "ModuleIsReloading".into(),
            message: format!("Module {module_id} is reloading until {until} (now {now_unix})"),
        });
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn handle_survey_scanner_activation(
    ctx: &ServerContext,
    sys: &SystemHandle,
    sys_id: u32,
    char_id: i64,
    ship_id: i64,
    module_id: i64,
    mod_type: &InvType,
    mod_state: &DogmaItemState,
) -> PyRep {
    let mut scan_range = mod_state.get_attribute(197, &ctx.sde);
    if scan_range <= 0.0 {
        scan_range = mod_type
            .default_attributes
            .get(&197)
            .copied()
            .or_else(|| mod_type.default_attributes.get(&54).copied())
            .unwrap_or(15_000.0);
    }
    let mut ship_pos = DVec3::ZERO;
    if let Ok(Some((_, ship_ball))) = sys.get_ball_snapshot(ship_id).await {
        ship_pos = ship_ball.position;
    }
    let asteroids = ctx.db.get_asteroids_in_system(sys_id);
    let mut scanned_asteroids = Vec::new();
    for ast in asteroids {
        if ast.quantity <= 0 {
            continue;
        }
        let ast_pos = DVec3::new(ast.position.0, ast.position.1, ast.position.2);
        let dist = if ship_pos != DVec3::ZERO {
            (ship_pos - ast_pos).length()
        } else {
            0.0
        };
        if dist <= scan_range || ship_pos == DVec3::ZERO {
            let type_name = ctx
                .sde
                .types
                .get(&ast.type_id)
                .map(|t| t.name.clone())
                .unwrap_or_else(|| format!("Asteroid {}", ast.type_id));
            let vol = ctx.sde.types.get(&ast.type_id).map(|t| t.volume).unwrap_or(0.1);
            ctx.notify(
                crate::router::NotificationTarget::Character(char_id),
                "OnAsteroidRevealed",
                "character",
                vec![
                    PyRep::Integer(ast.asteroid_id),
                    PyRep::Integer(ast.type_id as i64),
                    PyRep::Integer(ast.quantity),
                ],
            );
            scanned_asteroids.push(PyRep::keyval([
                ("asteroidID", PyRep::Integer(ast.asteroid_id)),
                ("typeID", PyRep::Integer(ast.type_id as i64)),
                ("typeName", PyRep::String(type_name)),
                ("quantity", PyRep::Integer(ast.quantity)),
                ("initialQuantity", PyRep::Integer(ast.initial_quantity)),
                ("distance", PyRep::Real(dist)),
                ("volume", PyRep::Real(vol)),
            ]));
        }
    }
    PyRep::keyval([
        ("moduleID", PyRep::Integer(module_id)),
        ("active", PyRep::Bool(true)),
        ("scanRange", PyRep::Real(scan_range)),
        ("count", PyRep::Integer(scanned_asteroids.len() as i64)),
        ("asteroidCount", PyRep::Integer(scanned_asteroids.len() as i64)),
        ("asteroids", PyRep::List(scanned_asteroids)),
    ])
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn build_specialized_module_result(
    method: &str,
    fit: &ShipFittingEngine,
    ctx: &ServerContext,
    module_id: i64,
    mod_type: &InvType,
    hic_bubble_radius_m: Option<f64>,
    warp_scramble_points: i32,
    active_burst_charge_tid: Option<u32>,
) -> Option<PyRep> {
    if method == "Overload" {
        let max_vel = fit.ship.get_attribute(attr_ids::MAX_VELOCITY, &ctx.sde);
        Some(PyRep::keyval([
            ("moduleID", PyRep::Integer(module_id)),
            ("overloaded", PyRep::Bool(true)),
            ("effectiveMaxVelocity", PyRep::Real(max_vel)),
        ]))
    } else if mod_type.group_id == 899 {
        let weapon_timer_s = mod_type
            .default_attributes
            .get(&2037)
            .copied()
            .unwrap_or(60_000.0)
            / 1000.0;
        Some(PyRep::keyval([
            ("moduleID", PyRep::Integer(module_id)),
            ("active", PyRep::Bool(true)),
            ("scripted", PyRep::Bool(hic_bubble_radius_m.is_none())),
            (
                "bubbleRadius",
                hic_bubble_radius_m.map(PyRep::Real).unwrap_or(PyRep::None),
            ),
            ("warpScrambleStrength", PyRep::Integer(warp_scramble_points as i64)),
            ("disallowAssistance", PyRep::Bool(hic_bubble_radius_m.is_some())),
            ("weaponTimerSeconds", PyRep::Real(weapon_timer_s)),
        ]))
    } else if matches!(mod_type.group_id, 515 | 1770 | 4174) {
        Some(PyRep::keyval([
            ("moduleID", PyRep::Integer(module_id)),
            ("active", PyRep::Bool(true)),
            (
                "chargeTypeID",
                active_burst_charge_tid
                    .map(|c| PyRep::Integer(c as i64))
                    .unwrap_or(PyRep::None),
            ),
        ]))
    } else {
        None
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn handle_scan_probe_launcher_activation(
    ctx: &ServerContext,
    session: &SessionState,
    sys: &SystemHandle,
    sys_id: u32,
    char_id: i64,
    ship_id: i64,
    module_id: i64,
    m_type_id: u32,
    m_flag_id: u16,
    effect_name: &str,
    target_id_opt: Option<i64>,
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    let loaded_charge_opt = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .find(|i| {
                i.location_id == ship_id
                    && (i.flag as u16) == m_flag_id
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

    let existing_active_count = ctx
        .active_scanner_probes
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .values()
        .filter(|p| p.owner_char_id == char_id && p.solar_system_id == sys_id)
        .count();
    if existing_active_count >= 8 {
        return Err(BridgeRpcError {
            code: "MaxProbesLaunched".into(),
            message: "Maximum of 8 active scanner probes already in space".into(),
        });
    }

    let max_can_launch = (8 - existing_active_count) as i32;
    let requested_count = kwargs
        .get("probeCount")
        .and_then(|v| v.as_i64())
        .unwrap_or(loaded_qty as i64) as i32;
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
    let base_sensor_strength = probe_sde
        .and_then(|t| t.default_attributes.get(&1909).copied())
        .unwrap_or(18.0);
    let max_flight_time_s = probe_sde
        .and_then(|t| t.default_attributes.get(&281).copied())
        .map(|ms| ms / 1000.0)
        .unwrap_or(3_600.0);
    let range_step = kwargs
        .get("rangeStep")
        .and_then(|v| v.as_i64())
        .unwrap_or(4)
        .clamp(1, 8) as i32;
    let base_scan_range_au = probe_sde
        .and_then(|t| t.default_attributes.get(&1911).copied())
        .unwrap_or(0.25);
    let scan_range_m = base_scan_range_au * (2.0_f64.powi(range_step - 1)) * 149_597_870_700.0;

    let ship_pos = sys
        .get_ball_snapshot(ship_id)
        .await
        .ok()
        .flatten()
        .map(|(_, b)| b.position)
        .unwrap_or(DVec3::ZERO);

    let now_unix = ctx.now_unix_secs();
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
        let ball = eve_destiny::Ball {
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

    let _ = ctx.notify_module_effect_started(
        char_id,
        ship_id,
        module_id,
        m_type_id,
        Some(effect_name),
        target_id_opt,
    );
    for &pid in &allocated_probe_ids {
        let probe_kv = PyRep::keyval([
            ("probeID", PyRep::Integer(pid)),
            ("typeID", PyRep::Integer(probe_type_id as i64)),
            ("scanRange", PyRep::Real(scan_range_m)),
            ("expiry", PyRep::Integer(now_unix as i64 + max_flight_time_s as i64)),
        ]);
        let _ = ctx.notify(
            crate::router::NotificationTarget::Character(char_id),
            "OnNewProbe",
            "charid",
            vec![probe_kv],
        );
    }
    Ok(PyRep::keyval([
        ("moduleID", PyRep::Integer(module_id)),
        ("launcherID", PyRep::Integer(module_id)),
        ("probeTypeID", PyRep::Integer(probe_type_id as i64)),
        (
            "probeIDs",
            PyRep::List(allocated_probe_ids.into_iter().map(PyRep::Integer).collect()),
        ),
        ("launchedCount", PyRep::Integer(to_launch as i64)),
        ("remainingInLauncher", PyRep::Integer(remaining_in_launcher as i64)),
        ("baseSensorStrength", PyRep::Real(base_sensor_strength)),
        ("maxFlightTimeS", PyRep::Real(max_flight_time_s)),
    ]))
}

pub(crate) fn handle_unload_ammo(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let ship_id = args.first().and_then(|a| a.as_i64()).or(session.ship_id).unwrap_or(0);
    let owns_ship = ctx.db.with_snapshot(|s| s.items.get(&ship_id).is_some_and(|sh| sh.owner_id == char_id));
    if !owns_ship {
        return Err(BridgeRpcError {
            code: "ShipOwnershipViolation".into(),
            message: format!("Ship {ship_id} is not owned by character {char_id}"),
        });
    }
    let requested_module_ids: Vec<i64> = match args.get(1) {
        Some(PyRep::List(v) | PyRep::Tuple(v)) => v.iter().filter_map(|x| x.as_i64()).collect(),
        Some(v) => v.as_i64().into_iter().collect(),
        None => Vec::new(),
    };

    let all_target_modules: Vec<i64> = ctx.db.with_snapshot(|snap| {
        let mut out = Vec::new();
        for &req_id in &requested_module_ids {
            let resolved_mod_id = if snap.items.contains_key(&req_id) {
                req_id
            } else if let Some(found) = snap.items.values().find(|i| i.location_id == ship_id && (i.flag as i64) == req_id) {
                found.item_id
            } else {
                req_id
            };
            if let Some(bank) = snap.weapon_banks.get(&ship_id).and_then(|b| b.get(&resolved_mod_id)) {
                for &m in bank {
                    if !out.contains(&m) {
                        out.push(m);
                    }
                }
            } else if !out.contains(&resolved_mod_id) {
                out.push(resolved_mod_id);
            }
        }
        out
    });

    let ship_cargo_cap = ctx
        .get_or_rebuild_ship_fitting(char_id, ship_id)
        .ok()
        .map(|f| f.ship.get_attribute(attr_ids::CAPACITY, &ctx.sde))
        .or_else(|| {
            ctx.db.with_snapshot(|s| {
                s.items
                    .get(&ship_id)
                    .and_then(|sh| ctx.sde.types.get(&sh.type_id))
                    .map(|t| t.capacity)
            })
        })
        .unwrap_or(1_000_000.0);

    ctx.db.try_with_mut_snapshot(|snap| {
        let mut used_cargo: f64 = snap
            .items
            .values()
            .filter(|i| i.location_id == ship_id && i.flag == InventoryFlag::Cargo)
            .map(|i| {
                ctx.sde
                    .types
                    .get(&i.type_id)
                    .map(|t| t.volume * (i.quantity.max(1) as f64))
                    .unwrap_or(0.0)
            })
            .sum();

        for &mod_id in &all_target_modules {
            let Some(mod_item) = snap.items.get(&mod_id).cloned() else {
                continue;
            };
            if mod_item.location_id != ship_id || !matches!(mod_item.flag as u16, 11..=34 | 125..=132) {
                continue;
            }
            let slot_flag = mod_item.flag;
            let charges: Vec<(i64, f64)> = snap
                .items
                .values()
                .filter(|i| {
                    i.location_id == ship_id
                        && i.flag == slot_flag
                        && i.item_id != mod_id
                        && ctx.sde.types.get(&i.type_id).is_some_and(|t| t.category_id == 8)
                })
                .map(|i| {
                    let vol = ctx
                        .sde
                        .types
                        .get(&i.type_id)
                        .map(|t| t.volume * (i.quantity.max(1) as f64))
                        .unwrap_or(0.0);
                    (i.item_id, vol)
                })
                .collect();
            for (cid, vol) in charges {
                if ship_cargo_cap > 0.0 && used_cargo + vol > ship_cargo_cap + 1e-4 {
                    return Err(BridgeRpcError {
                        code: "NotEnoughCargoSpace".into(),
                        message: "Not enough cargo space to unload ammo".into(),
                    });
                }
                if let Some(ci) = snap.items.get_mut(&cid) {
                    ci.flag = InventoryFlag::Cargo;
                    used_cargo += vol;
                }
            }
        }
        Ok(())
    })?;

    ctx.invalidate_ship_fitting(ship_id);
    Ok(PyRep::None)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn build_all_info(
    ctx: &ServerContext,
    session: &SessionState,
    char_id: i64,
    ship_id: i64,
    ship_type_id: u32,
    active_stance: PyRep,
    entries: Vec<(PyRep, PyRep)>,
    module_entries: Vec<(PyRep, PyRep)>,
    args: &[PyRep],
) -> PyRep {
    let now_ft = ctx.now_blueos_filetime_ticks();
    let header = eve_proto::DBRowDescriptor::inv_item_header();
    let prime_char = !matches!(args.first(), Some(PyRep::Bool(false) | PyRep::Integer(0) | PyRep::None));
    let prime_ship = !matches!(args.get(1), Some(PyRep::Bool(false) | PyRep::Integer(0) | PyRep::None));

    let ship_inv_row = if let Some(mut it) = ctx.db.get_item(ship_id) {
        it.singleton = true;
        header.build_dbrow(super::super::item_to_packed_cells(ctx, &it))
    } else {
        let (grp, cat) = ctx
            .sde
            .types
            .get(&ship_type_id)
            .map(|t| (i64::from(t.group_id), i64::from(t.category_id)))
            .unwrap_or((25, 6));
        let loc = session.station_id.or(session.solar_system_id.map(i64::from)).unwrap_or(0);
        header.build_dbrow(vec![
            PyRep::Integer(ship_id),
            PyRep::Integer(i64::from(ship_type_id)),
            PyRep::Integer(char_id),
            PyRep::Integer(loc),
            PyRep::Integer(4),
            PyRep::Integer(-1),
            PyRep::Integer(grp),
            PyRep::Integer(cat),
            PyRep::String(String::new()),
        ])
    };
    let mut ship_info_entries = vec![(
        PyRep::Integer(ship_id),
        PyRep::keyval([
            ("itemID", PyRep::Integer(ship_id)),
            ("invItem", ship_inv_row),
            ("attributes", PyRep::Dict(entries.clone())),
            ("activeEffects", PyRep::Dict(Vec::new())),
            ("time", PyRep::Integer(now_ft)),
            ("wallclockTime", PyRep::Integer(now_ft)),
            ("description", PyRep::String(String::new())),
        ]),
    )];
    for (mod_id_rep, mod_attrs_rep) in &module_entries {
        if let Some(mod_id) = mod_id_rep.as_i64() {
            if let Some(mut mod_item) = ctx.db.get_item(mod_id) {
                mod_item.singleton = true;
                let inv_row = header.build_dbrow(super::super::item_to_packed_cells(ctx, &mod_item));
                ship_info_entries.push((
                    PyRep::Integer(mod_id),
                    PyRep::keyval([
                        ("itemID", PyRep::Integer(mod_id)),
                        ("invItem", inv_row),
                        ("attributes", mod_attrs_rep.clone()),
                        ("activeEffects", PyRep::Dict(Vec::new())),
                        ("time", PyRep::Integer(now_ft)),
                        ("wallclockTime", PyRep::Integer(now_ft)),
                        ("description", PyRep::String(String::new())),
                    ]),
                ));
            }
        }
    }

    ctx.notify(
        crate::router::NotificationTarget::Character(char_id),
        "ProcessGodmaPrimeLocation",
        "charid",
        vec![PyRep::Integer(ship_id), PyRep::Dict(ship_info_entries.clone())],
    );

    let char_info = if prime_char {
        let char_type_id = 1373u32;
        let (cha, int, mem, per, wil) = ctx.db.with_snapshot(|snap| {
            crate::services::character_skills::effective_character_attributes(&ctx.sde, snap, char_id)
        });
        let (char_grp, char_cat) = ctx
            .sde
            .types
            .get(&char_type_id)
            .map(|t| (i64::from(t.group_id), i64::from(t.category_id)))
            .unwrap_or((1, 1));
        let char_inv_row = header.build_dbrow(vec![
            PyRep::Integer(char_id),
            PyRep::Integer(i64::from(char_type_id)),
            PyRep::Integer(char_id),
            PyRep::Integer(ship_id),
            PyRep::Integer(57),
            PyRep::Integer(-1),
            PyRep::Integer(char_grp),
            PyRep::Integer(char_cat),
            PyRep::String(String::new()),
        ]);
        let mut char_attrs_map = ctx
            .sde
            .types
            .get(&char_type_id)
            .map(|t| t.default_attributes.clone())
            .unwrap_or_default();
        for (aid, val) in [(164u16, cha), (165, int), (166, mem), (167, per), (168, wil)] {
            char_attrs_map.insert(aid, val);
        }
        let mut char_attrs: Vec<(PyRep, PyRep)> = char_attrs_map
            .into_iter()
            .map(|(k, v)| (PyRep::Integer(i64::from(k)), PyRep::Real(v)))
            .collect();
        char_attrs.sort_by_key(|(k, _)| k.as_i64().unwrap_or(0));
        let char_row = PyRep::keyval([
            ("itemID", PyRep::Integer(char_id)),
            ("invItem", char_inv_row),
            ("attributes", PyRep::Dict(char_attrs)),
            ("activeEffects", PyRep::Dict(Vec::new())),
            ("time", PyRep::Integer(now_ft)),
            ("wallclockTime", PyRep::Integer(now_ft)),
            ("description", PyRep::String(String::new())),
        ]);
        let char_brain = PyRep::Tuple(vec![
            PyRep::Integer(0),
            PyRep::Tuple(Vec::new()),
            PyRep::Tuple(Vec::new()),
            PyRep::Tuple(Vec::new()),
        ]);
        PyRep::Tuple(vec![PyRep::Dict(vec![(PyRep::Integer(char_id), char_row)]), char_brain])
    } else {
        PyRep::None
    };

    let ship_info = if prime_ship { PyRep::Dict(ship_info_entries) } else { PyRep::None };
    let ship_state = PyRep::Tuple(vec![
        PyRep::Dict(Vec::new()),
        PyRep::Dict(Vec::new()),
        PyRep::Dict(Vec::new()),
        PyRep::Dict(Vec::new()),
    ]);

    PyRep::keyval([
        ("shipID", PyRep::Integer(ship_id)),
        ("charID", PyRep::Integer(char_id)),
        ("activeStance", active_stance),
        ("shipAttributes", PyRep::Dict(entries)),
        ("moduleAttributes", PyRep::Dict(module_entries)),
        ("structureInfo", PyRep::None),
        ("shipInfo", ship_info),
        ("charInfo", char_info),
        ("shipModifiedCharAttribs", PyRep::None),
        ("systemWideEffectsOnShip", PyRep::Dict(Vec::new())),
        ("shipState", ship_state),
    ])
}
