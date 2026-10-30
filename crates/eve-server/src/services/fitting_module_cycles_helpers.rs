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

#[allow(clippy::too_many_arguments)]
pub(crate) async fn handle_ecm_or_burst_jammer_activation(
    ctx: &ServerContext,
    sys: &SystemHandle,
    sys_id: u32,
    char_id: i64,
    ship_id: i64,
    module_id: i64,
    mod_type: &InvType,
    mod_state: &DogmaItemState,
    target_id_opt: Option<i64>,
    duration_s: f64,
) -> Result<(), BridgeRpcError> {
    // SDE sensor pairing: `idx 0..4` matches jammer attrs `[238 Grav, 239 Ladar, 240 Mag, 241 Radar]`
    // against target sensor attrs `[211 Grav, 209 Ladar, 210 Mag, 208 Radar]`.
    const SENSOR_ATTRS: [u16; 4] = [211, 209, 210, 208];

    if mod_type.group_id == 201 {
        let Some(target_id) = target_id_opt else {
            let _ = sys.remove_module_cycle(ship_id, module_id).await;
            return Err(BridgeRpcError {
                code: "TargetRequired".into(),
                message: "ECM module requires a locked target".into(),
            });
        };
        let mut distance_m = 0.0;
        if let (Ok(Some((_, ship_ball))), Ok(Some((_, target_ball)))) = (
            sys.get_ball_snapshot(ship_id).await,
            sys.get_ball_snapshot(target_id).await,
        ) {
            distance_m = (ship_ball.position - target_ball.position).length();
        }
        let target_type_id_opt = ctx
            .db
            .with_snapshot(|snap| snap.items.get(&target_id).map(|i| (i.owner_id, i.type_id)));
        let mut best_sensor_idx = 0usize;
        let mut target_sensor_strength = 0.0;
        if let Some((t_owner, t_tid)) = target_type_id_opt {
            if let Ok(t_fit) = ctx.get_or_rebuild_ship_fitting(t_owner, target_id) {
                for (idx, &s_attr) in SENSOR_ATTRS.iter().enumerate() {
                    let val = t_fit.ship.get_attribute(s_attr, &ctx.sde);
                    if val > target_sensor_strength {
                        target_sensor_strength = val;
                        best_sensor_idx = idx;
                    }
                }
            } else if let Some(t_type) = ctx.sde.types.get(&t_tid) {
                for (idx, &s_attr) in SENSOR_ATTRS.iter().enumerate() {
                    let val = t_type.default_attributes.get(&s_attr).copied().unwrap_or(0.0);
                    if val > target_sensor_strength {
                        target_sensor_strength = val;
                        best_sensor_idx = idx;
                    }
                }
            }
        } else if let Ok(Some((_, target_ball))) = sys.get_ball_snapshot(target_id).await {
            if let Some(t_type) = ctx.sde.types.get(&target_ball.type_id) {
                for (idx, &s_attr) in SENSOR_ATTRS.iter().enumerate() {
                    let val = t_type.default_attributes.get(&s_attr).copied().unwrap_or(0.0);
                    if val > target_sensor_strength {
                        target_sensor_strength = val;
                        best_sensor_idx = idx;
                    }
                }
            }
        }
        if target_sensor_strength <= 0.0 {
            target_sensor_strength = 12.0;
        }
        let jam_attr_id = 238u16 + (best_sensor_idx as u16);
        let jam_strength = mod_state.get_attribute(jam_attr_id, &ctx.sde);
        let optimal_range = mod_state.get_attribute(attr_ids::MAX_RANGE, &ctx.sde);
        let falloff_range = mod_state.get_attribute(attr_ids::FALLOFF, &ctx.sde);
        let jam_chance = eve_dogma::compute_ecm_jam_chance(
            jam_strength,
            target_sensor_strength,
            distance_m,
            optimal_range,
            falloff_range,
        );
        let roll = super::csprng_unit_f64(ctx);
        let jam_duration_s = if duration_s > 0.0 { duration_s } else { 20.0 };
        if roll < jam_chance && jam_chance > 0.0 {
            let _ = sys.apply_ecm_jam(target_id, jam_duration_s).await;
        }
    }

    if mod_type.group_id == 80 || mod_type.group_id == 353 {
        let burst_range = eve_dogma::compute_burst_jammer_radius_m(mod_type, mod_state, &ctx.sde);
        let sys_sec = crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
            rt.modified_system_securities.get(&sys_id).copied()
        })
        .unwrap_or_else(|| {
            ctx.sde
                .solar_systems
                .get(&sys_id)
                .map(|s| s.security_status)
                .unwrap_or(1.0)
        });
        let ship_pos = sys
            .get_ball_snapshot(ship_id)
            .await
            .ok()
            .flatten()
            .map(|(_, b)| b.position)
            .unwrap_or(DVec3::ZERO);
        let all_balls = sys.list_balls().await.unwrap_or_default();
        let mut unprovoked_neutral_victims: Vec<i64> = Vec::new();
        let mut legal_pvp_victims: Vec<i64> = Vec::new();
        let mut ship_candidates: Vec<(i64, usize, f64)> = Vec::new();
        let now_unix = ctx.now_unix_secs();
        for b in &all_balls {
            if b.id == ship_id || (b.position - ship_pos).length() > burst_range + 1e-3 {
                continue;
            }
            // Burst Jammer affects ONLY uncloaked, non-EWAR-immune Ships (`category_id == 6`) —
            // never wrecks, jetcans, drones, fighters, deployables, or structures (`R-BURST-ECM-01`).
            let is_ship_ball = ctx
                .sde
                .types
                .get(&b.type_id)
                .is_some_and(|t| t.category_id == 6);
            if !is_ship_ball {
                continue;
            }
            let cs_opt = sys.get_combat_state(b.id).await.ok().flatten();
            if cs_opt.as_ref().is_some_and(|cs| cs.is_cloaked) {
                continue;
            }
            let victim_char_opt = ctx.db.with_snapshot(|snap| {
                snap.characters
                    .values()
                    .find(|c| c.active_ship_id == b.id && c.solar_system_id == sys_id)
                    .map(|c| c.character_id)
                    .or_else(|| {
                        snap.items
                            .get(&b.id)
                            .map(|i| i.owner_id)
                            .filter(|oid| snap.characters.contains_key(oid))
                    })
            });
            if let Some(v_char) = victim_char_opt {
                if crate::services::space_destiny::is_location_change_invulnerable(ctx, v_char, now_unix) {
                    continue;
                }
                if v_char != char_id {
                    let legal = {
                        let cw = ctx.crimewatch.read().unwrap_or_else(|e| e.into_inner());
                        cw.states.get(&v_char).is_some_and(|vs| {
                            vs.is_criminal(now_unix) || vs.is_suspect(now_unix)
                        })
                    } || crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
                        rt.has_limited_engagement_with(char_id, v_char, now_unix)
                            || rt.has_limited_engagement_with(v_char, char_id, now_unix)
                    }) || ctx.db.with_snapshot(|snap| {
                        crate::services::combat_crimewatch::is_legal_war_or_suspect_target(
                            snap, char_id, v_char, now_unix,
                        )
                    });
                    if !legal {
                        if !unprovoked_neutral_victims.contains(&v_char) {
                            unprovoked_neutral_victims.push(v_char);
                        }
                    } else if !legal_pvp_victims.contains(&v_char) {
                        legal_pvp_victims.push(v_char);
                    }
                }
            }
            let mut best_sensor_idx = 0usize;
            let mut target_sensor_strength = 0.0;
            let owner_for_fit = victim_char_opt.or_else(|| {
                ctx.db
                    .with_snapshot(|snap| snap.items.get(&b.id).map(|i| i.owner_id))
            });
            if let Some(t_owner) = owner_for_fit {
                if let Ok(t_fit) = ctx.get_or_rebuild_ship_fitting(t_owner, b.id) {
                    for (idx, &s_attr) in SENSOR_ATTRS.iter().enumerate() {
                        let val = t_fit.ship.get_attribute(s_attr, &ctx.sde);
                        if val > target_sensor_strength {
                            target_sensor_strength = val;
                            best_sensor_idx = idx;
                        }
                    }
                }
            }
            if target_sensor_strength <= 0.0 {
                if let Some(t_type) = ctx.sde.types.get(&b.type_id) {
                    for (idx, &s_attr) in SENSOR_ATTRS.iter().enumerate() {
                        let val = t_type.default_attributes.get(&s_attr).copied().unwrap_or(0.0);
                        if val > target_sensor_strength {
                            target_sensor_strength = val;
                            best_sensor_idx = idx;
                        }
                    }
                }
            }
            if target_sensor_strength <= 0.0 {
                target_sensor_strength = 12.0;
            }
            ship_candidates.push((b.id, best_sensor_idx, target_sensor_strength));
        }

        if !unprovoked_neutral_victims.is_empty() && sys_sec > 0.0 {
            let safety = crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
                rt.safety_levels.get(&char_id).copied()
            })
            .unwrap_or(crate::services::combat_crimewatch::SafetyLevel::Red);
            if (sys_sec >= 0.45 && safety != crate::services::combat_crimewatch::SafetyLevel::Red)
                || (sys_sec < 0.45 && safety == crate::services::combat_crimewatch::SafetyLevel::Green)
            {
                let _ = sys.remove_module_cycle(ship_id, module_id).await;
                return Err(BridgeRpcError {
                    code: "SafetyBlocked".into(),
                    message: "Safety setting prevents activating Burst Jammer near neutral capsuleers in empire space".into(),
                });
            }
        }
        let jam_strengths = [
            mod_state.get_attribute(238, &ctx.sde),
            mod_state.get_attribute(239, &ctx.sde),
            mod_state.get_attribute(240, &ctx.sde),
            mod_state.get_attribute(241, &ctx.sde),
        ];
        let mut ship_eval_overrides = HashMap::with_capacity(ship_candidates.len());
        for (bid, s_idx, s_str) in ship_candidates {
            let ship_roll = super::csprng_unit_f64(ctx);
            ship_eval_overrides.insert(bid, (s_idx, s_str, ship_roll));
        }
        let jam_duration_s = if duration_s > 0.0 { duration_s } else { 20.0 };
        let jammed_ids = sys
            .apply_ecm_burst_ships(
                ship_id,
                burst_range,
                jam_strengths,
                jam_duration_s,
                0.5,
                Some(ship_eval_overrides),
            )
            .await
            .unwrap_or_default();
        for &jid in &jammed_ids {
            let jammed_owner_opt = ctx.db.with_snapshot(|snap| {
                snap.characters
                    .values()
                    .find(|c| c.active_ship_id == jid)
                    .map(|c| c.character_id)
                    .or_else(|| {
                        snap.items
                            .get(&jid)
                            .map(|i| i.owner_id)
                            .filter(|oid| snap.characters.contains_key(oid))
                    })
            });
            if let Some(j_char) = jammed_owner_opt {
                ctx.notify(
                    crate::router::NotificationTarget::Character(j_char),
                    "OnTargetClear",
                    "charid",
                    vec![],
                );
            }
        }
        for &v_char in &legal_pvp_victims {
            crate::sp6_lifecycle::record_pvp_aggression_and_renew_logoff_timer(
                ctx, char_id, v_char, now_unix,
            );
            let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
            let _ = cw.handle_combat_aggression(
                char_id, v_char, ship_id, sys_id, sys_sec, true, now_unix,
            );
            cw.get_or_create(v_char).pvp_timer_until_unix = now_unix.saturating_add(900);
        }
        for &v_char in &unprovoked_neutral_victims {
            crate::sp6_lifecycle::record_pvp_aggression_and_renew_logoff_timer(
                ctx, char_id, v_char, now_unix,
            );
            let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
            cw.get_or_create(v_char).pvp_timer_until_unix = now_unix.saturating_add(900);
            let _ = cw.handle_combat_aggression(
                char_id,
                v_char,
                ship_id,
                sys_id,
                sys_sec,
                sys_sec <= 0.0,
                now_unix,
            );
        }
        if !unprovoked_neutral_victims.is_empty() && sys_sec > 0.0 {
            crate::services::combat_crimewatch::emit_crimewatch_timer_notifications(
                ctx, char_id, now_unix,
            );
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_pre_notify_instant_module_effects(
    ctx: &ServerContext,
    sys: &SystemHandle,
    sys_id: u32,
    char_id: i64,
    ship_id: i64,
    module_id: i64,
    m: &eve_dogma::FittedModule,
    mod_type: &InvType,
    mod_state: &DogmaItemState,
    is_overloaded: bool,
    target_id_opt: Option<i64>,
    duration_s: f64,
    kwargs: &HashMap<String, PyRep>,
) -> Result<Option<PyRep>, BridgeRpcError> {
    if matches!(mod_type.group_id, 201 | 80 | 353) {
        handle_ecm_or_burst_jammer_activation(
            ctx,
            sys,
            sys_id,
            char_id,
            ship_id,
            module_id,
            mod_type,
            mod_state,
            target_id_opt,
            duration_s,
        )
        .await?;
        return Ok(None);
    }
    if matches!(mod_type.group_id, 72 | 650) {
        if let Ok(Some((_, attacker_ball))) = sys.get_ball_snapshot(ship_id).await {
            let sys_sec = crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
                rt.modified_system_securities.get(&sys_id).copied()
            })
            .unwrap_or_else(|| {
                ctx.sde
                    .solar_systems
                    .get(&sys_id)
                    .map(|s| s.security_status)
                    .unwrap_or(0.0)
            });
            let now_unix = ctx.now_unix_secs();
            let mut m_eff = m.clone();
            if is_overloaded {
                m_eff.state = eve_dogma::ModuleState::Overloaded;
            }
            let action_res = if mod_type.group_id == 650 {
                crate::services::combat_crimewatch::activate_salvager_or_tractor_module(
                    ctx,
                    char_id,
                    ship_id,
                    sys_id,
                    module_id,
                    &m_eff,
                    mod_type,
                    &attacker_ball,
                    target_id_opt,
                    kwargs,
                )
                .await
            } else {
                crate::services::combat_crimewatch::activate_aoe_or_defender_module_with_cap(
                    ctx,
                    char_id,
                    ship_id,
                    sys_id,
                    sys_sec,
                    now_unix,
                    module_id,
                    &m_eff,
                    mod_type,
                    &attacker_ball,
                    target_id_opt,
                    kwargs,
                    false,
                )
                .await
            };
            if let Some(res) = action_res {
                return res.map(Some);
            }
        }
    }
    Ok(None)
}

async fn deactivate_repeating_cycle_after_action(
    ctx: &ServerContext,
    sys: &SystemHandle,
    owner_char_id: Option<i64>,
    ship_id: i64,
    module_id: i64,
    effect_name: &str,
) {
    let _ = sys.remove_module_cycle(ship_id, module_id).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.remove(&module_id);
        snap.module_active_charges.remove(&module_id);
    });
    if let Some(cid) = owner_char_id {
        if let Ok(mut fit) = ctx.get_or_rebuild_ship_fitting(cid, ship_id) {
            let _ = fit.deactivate_module(module_id, &ctx.sde);
            ctx.store_ship_fitting(ship_id, fit);
        }
        ctx.notify(
            crate::router::NotificationTarget::Character(cid),
            "OnGodmaShipEffect",
            "charid",
            vec![
                PyRep::Integer(module_id),
                PyRep::String(effect_name.to_string()),
                PyRep::Bool(false),
            ],
        );
    }
}

/// Executes cycle-end wreck salvaging (`group 1122`) and auto-repeating (`repeat != 0`) AoE / tractor
/// cycle-start pulses queued by `step_ballpark_and_cycles` (`R-AOE-SALVAGE-CYCLE-01`).
pub(crate) async fn execute_pending_cycle_actions(ctx: &ServerContext, sys_id: u32) {
    let actions = ctx.systems.take_pending_cycle_actions(sys_id);
    if actions.is_empty() {
        return;
    }
    let sys = ctx.systems.get_or_spawn(sys_id);
    let empty_kw = HashMap::new();
    for action in actions {
        match action {
            crate::system_actor::PendingCycleAction::SalvageEnd {
                char_id,
                ship_id,
                module_id,
                module_type_id,
                wreck_id,
                repeat,
                effect_name,
                next_cycle_cap_deducted,
                deducted_cap_gj,
            } => {
                let res = crate::combat::drones_salvage_tractor::execute_wreck_salvage_cycle(
                    ctx,
                    sys_id,
                    char_id,
                    ship_id,
                    wreck_id,
                    Some(module_type_id),
                )
                .await;
                if repeat == 0 {
                    if let Ok(mut fit) = ctx.get_or_rebuild_ship_fitting(char_id, ship_id) {
                        let _ = fit.deactivate_module(module_id, &ctx.sde);
                        ctx.store_ship_fitting(ship_id, fit);
                    }
                } else {
                    let keep_repeating = matches!(
                        &res,
                        Ok(rep)
                            if rep.dict_get("salvaged").and_then(|v| v.as_bool()) == Some(false)
                                && rep.dict_get("reason").and_then(|v| v.as_str())
                                    == Some("CycleRollFailed")
                    );
                    if keep_repeating {
                        let dur_s = ctx
                            .sde
                            .types
                            .get(&module_type_id)
                            .and_then(|t| t.default_attributes.get(&73).copied())
                            .unwrap_or(10_000.0)
                            / 1000.0;
                        ctx.record_module_reactivation_delay(ship_id, module_id, dur_s);
                    } else {
                        deactivate_repeating_cycle_after_action(
                            ctx,
                            &sys,
                            Some(char_id),
                            ship_id,
                            module_id,
                            &effect_name,
                        )
                        .await;
                        if next_cycle_cap_deducted && deducted_cap_gj > 0.0 {
                            if let Ok(Some(cs)) = sys.get_combat_state(ship_id).await {
                                let _ = sys
                                    .set_capacitor(
                                        ship_id,
                                        (cs.capacitor + deducted_cap_gj).min(cs.max_capacitor),
                                    )
                                    .await;
                            }
                        }
                    }
                }
            }
            crate::system_actor::PendingCycleAction::RepeatCycleStart {
                char_id,
                ship_id,
                module_id,
                module_type_id,
                target_id,
                duration_s,
                is_overloaded,
                effect_name,
            } => {
                ctx.record_module_reactivation_delay(ship_id, module_id, duration_s);
                let Some(mod_type) = ctx.sde.types.get(&module_type_id).cloned() else {
                    continue;
                };
                let fit_opt = ctx.get_or_rebuild_ship_fitting(char_id, ship_id).ok();
                let mod_state = fit_opt
                    .as_ref()
                    .map(|f| {
                        f.build_effective_module_state(
                            module_id,
                            module_type_id,
                            is_overloaded,
                            &ctx.sde,
                        )
                    })
                    .unwrap_or_else(|| DogmaItemState::from_sde(char_id, module_type_id, &ctx.sde));
                let m_eff = fit_opt
                    .as_ref()
                    .and_then(|f| f.modules.get(&module_id).cloned())
                    .unwrap_or(eve_dogma::FittedModule {
                        item_id: module_id,
                        type_id: module_type_id,
                        flag_id: 27,
                        state: if is_overloaded {
                            eve_dogma::ModuleState::Overloaded
                        } else {
                            eve_dogma::ModuleState::Active
                        },
                        charge_type_id: None,
                        charge_quantity: 0,
                        damage_hp: 0.0,
                        max_hp: 40.0,
                        dynamic_attribute_overrides: HashMap::new(),
                    });
                let pulse_res = run_pre_notify_instant_module_effects(
                    ctx,
                    &sys,
                    sys_id,
                    char_id,
                    ship_id,
                    module_id,
                    &m_eff,
                    &mod_type,
                    &mod_state,
                    is_overloaded,
                    target_id,
                    duration_s,
                    &empty_kw,
                )
                .await;
                if pulse_res.is_err() {
                    deactivate_repeating_cycle_after_action(
                        ctx,
                        &sys,
                        Some(char_id),
                        ship_id,
                        module_id,
                        &effect_name,
                    )
                    .await;
                }
            }
        }
    }
}

pub(crate) async fn evaluate_module_cycle_aggression_or_assistance(
    ctx: &ServerContext,
    sys: &SystemHandle,
    sys_id: u32,
    char_id: i64,
    ship_id: i64,
    group_id: u32,
    target_id_opt: Option<i64>,
) -> Result<(), BridgeRpcError> {
    let Some(target_id) = target_id_opt else { return Ok(()); };
    if matches!(
        group_id,
        52 | 53 | 55 | 65 | 68 | 71 | 74 | 201 | 208 | 291 | 379 | 506..=512
            | 524 | 771 | 899 | 1245 | 1672 | 1674 | 1986 | 4060 | 4500 | 4805 | 4807
    ) {
        let is_pod = sys.get_ball_snapshot(target_id).await.ok().flatten().is_some_and(|(_, b)| b.type_id == 670)
            || ctx.db.with_snapshot(|s| s.items.get(&target_id).is_some_and(|i| i.type_id == 670));
        crate::services::combat_crimewatch::crimewatch_timers::evaluate_and_record_pvp_aggression(
            ctx, char_id, ship_id, sys_id, target_id, is_pod,
        )
    } else if matches!(group_id, 41 | 67 | 325 | 585 | 1697 | 1698 | 2018) {
        crate::services::combat_crimewatch::crimewatch_timers::evaluate_and_record_remote_assistance(
            ctx, char_id, ship_id, sys_id, target_id,
        )
    } else {
        Ok(())
    }
}

pub(crate) async fn try_handle_mercden_remote_shield_repair(
    ctx: &ServerContext,
    sys: &SystemHandle,
    sys_id: u32,
    char_id: i64,
    ship_id: i64,
    module_id: i64,
    m: &eve_dogma::FittedModule,
    mod_type: &InvType,
    fit: &eve_dogma::ShipFittingEngine,
    method: &str,
    target_id: i64,
) -> Result<Option<PyRep>, BridgeRpcError> {
    // 1. Target lock check
    let locks = sys.get_target_locks(ship_id).await.unwrap_or_default();
    if !locks.iter().any(|l| l.target_id == target_id && l.locked) {
        return Err(BridgeRpcError {
            code: "TargetNotLocked".into(),
            message: format!("Target {target_id} is not locked"),
        });
    }

    // 2. Cloak / tether check
    if let Ok(Some(cs)) = sys.get_combat_state(ship_id).await {
        if cs.is_cloaked {
            return Err(BridgeRpcError {
                code: "Cloaked".into(),
                message: "Cannot activate module while cloaked".into(),
            });
        }
        if cs.is_tethered {
            return Err(BridgeRpcError {
                code: "Tethered".into(),
                message: "Cannot activate remote assistance module while tethered".into(),
            });
        }
    }

    // 3. Distance check (maxRange + falloff)
    let is_overloaded = method == "Overload" || m.state == eve_dogma::ModuleState::Overloaded;
    let mod_state = fit.build_effective_module_state(module_id, m.type_id, is_overloaded, &ctx.sde);
    let opt_range = mod_state.get_attribute(attr_ids::MAX_RANGE, &ctx.sde);
    let falloff = eve_dogma::get_module_falloff(&mod_state, &ctx.sde);
    let max_range = if opt_range > 0.0 { opt_range + falloff.max(0.0) } else { 40_000.0 };
    let attacker_pos = sys.get_ball_snapshot(ship_id).await.ok().flatten().map(|(_, b)| b.position);
    let target_pos = ctx.db.with_snapshot(|s| s.structures.get(&target_id).map(|st| DVec3::from_array(st.position)));
    if let (Some(apos), Some(tpos)) = (attacker_pos, target_pos) {
        if apos.distance(tpos) > max_range {
            return Err(BridgeRpcError {
                code: "TargetOutOfRange".into(),
                message: format!("Target {target_id} is out of module range"),
            });
        }
    }

    // 4. Capacitor deduction check
    let cap_need = mod_state.get_attribute(attr_ids::CAPACITOR_NEED, &ctx.sde);
    if cap_need > 0.0 {
        let cs = sys.get_combat_state(ship_id).await.ok().flatten();
        let cur_cap = cs.as_ref().map(|c| c.capacitor).unwrap_or(0.0);
        if cur_cap < cap_need {
            return Err(BridgeRpcError {
                code: "NotEnoughCapacitor".into(),
                message: "Not enough capacitor to activate module".into(),
            });
        }
        let _ = sys.set_capacitor(ship_id, cur_cap - cap_need).await;
    }

    let rep_hp = mod_type.default_attributes.get(&68).copied().unwrap_or(1_500.0);
    let (pct, enabled) = crate::services::sov_gameplay::apply_mercenary_den_shield_repair(ctx, sys_id, ship_id, target_id, rep_hp).await?;
    crate::services::notify_emitters_batch3_space::emit_module_activation_godma_notifications(
        ctx,
        char_id,
        ship_id,
        module_id,
        m.type_id,
        "shieldRepair",
        5.0,
        Some(target_id),
    );
    Ok(Some(PyRep::keyval([
        ("moduleID", PyRep::Integer(module_id)),
        ("targetID", PyRep::Integer(target_id)),
        ("shieldPct", PyRep::Integer(pct as i64)),
        ("denEnabled", PyRep::Bool(enabled)),
    ])))
}
