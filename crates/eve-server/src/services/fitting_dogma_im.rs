//! `fitting_dogma_im` — `dogmaIM` inspection, fitting, targeting, weapon linking,ammo & `shipStanceMgr`.

#![forbid(unsafe_code)]

use super::*;

fn handle_item_get_info(
    ctx: &ServerContext,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let Some(raw_id) = args.first().and_then(|a| a.as_i64()) else {
        return Err(BridgeRpcError { code: "InvalidArgument".into(), message: "ItemGetInfo requires itemID or typeID".into() });
    };

    // 1. Check if raw_id is an item in the world DB or a launched space drone ball
    let item_opt = ctx.db.with_snapshot(|snap| snap.items.get(&raw_id).cloned());
    let space_drone_opt = crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
        rt.active_drones_in_space
            .values()
            .find(|d| d.ball_id == raw_id || d.drone_item_id == raw_id)
            .cloned()
    });

    let (type_id, owner_id, dyn_attrs, location_id, qty, singleton) = if let Some(ref item) = item_opt {
        (
            item.type_id,
            PyRep::Integer(item.owner_id),
            item.dynamic_attributes.clone(),
            Some(item.location_id),
            item.quantity,
            item.singleton,
        )
    } else if let Some(ref sd) = space_drone_opt {
        (
            sd.type_id,
            PyRep::Integer(sd.owner_char_id),
            HashMap::new(),
            Some(sd.parent_ship_id),
            1,
            true,
        )
    } else {
        // If not found in items, raw_id is treated as a TypeId
        let tid = raw_id as TypeId;
        (tid, PyRep::None, HashMap::new(), None, 1, false)
    };

    let Some(show_info) = ctx.sde.show_info_for_type(type_id) else {
        return Err(BridgeRpcError { code: "TypeNotFound".into(), message: format!("Type {type_id} not found in SDE") });
    };

    // Merge default attributes with dynamic attributes
    let mut merged_attrs = show_info.default_attributes.clone();
    for (&attr_id, &val) in &dyn_attrs {
        merged_attrs.insert(attr_id, val);
    }
    let mut fitted_online_ship: Option<(i64, i64)> = None;
    let item_cat = ctx.sde.types.get(&type_id).map(|t| t.category_id).unwrap_or(0);
    if item_cat == 18 {
        let parent_opt = space_drone_opt
            .as_ref()
            .map(|sd| (sd.owner_char_id, sd.parent_ship_id))
            .or_else(|| item_opt.as_ref().map(|it| (it.owner_id, it.location_id)));
        if let Some((p_owner, p_ship)) = parent_opt {
            if let Ok(fit) = ctx.get_or_rebuild_ship_fitting(p_owner, p_ship) {
                let drone_state_opt = fit.drone_state(raw_id, &ctx.sde).or_else(|| {
                    fit.evaluated_drones
                        .values()
                        .find(|d| d.type_id == type_id)
                        .cloned()
                });
                if let Some(d_state) = drone_state_opt {
                    let all_aids: std::collections::BTreeSet<u16> = d_state
                        .base_attributes
                        .keys()
                        .copied()
                        .chain(d_state.modifiers.iter().map(|m| m.target_attribute_id))
                        .collect();
                    for aid in all_aids {
                        merged_attrs.insert(aid, d_state.get_attribute(aid, &ctx.sde));
                    }
                }
                let has_active_triage = fit.modules.values().any(|fm| {
                    matches!(fm.type_id, 27951 | 4294)
                        && matches!(
                            fm.state,
                            eve_dogma::ModuleState::Active | eve_dogma::ModuleState::Overloaded
                        )
                });
                if has_active_triage {
                    merged_attrs.insert(64, 0.0);
                }
            }
        }
    } else if let Some(ref item) = item_opt {
        if item_cat == 6 {
            if let Ok(fit) = ctx.get_or_rebuild_ship_fitting(item.owner_id, item.item_id) {
                let all_aids: std::collections::BTreeSet<u16> = fit
                    .ship
                    .base_attributes
                    .keys()
                    .copied()
                    .chain(fit.ship.modifiers.iter().map(|m| m.target_attribute_id))
                    .collect();
                for aid in all_aids {
                    merged_attrs.insert(aid, fit.ship.get_attribute(aid, &ctx.sde));
                }
                for (&attr_id, &val) in &dyn_attrs {
                    if matches!(attr_id, 3 | 18 | 265 | 266)
                        || !fit
                            .ship
                            .modifiers
                            .iter()
                            .any(|m| m.target_attribute_id == attr_id)
                    {
                        merged_attrs.insert(attr_id, val);
                    }
                }
                let already_applied_dbuffs: Vec<u32> =
                    fit.gang_bursts.iter().filter_map(|b| b.dbuff_id).collect();
                crate::services::inventory_fitting::fitting_scanners_superweapons::apply_active_dbuffs_and_system_effects_to_ship_attrs_excluding(
                    ctx,
                    item.item_id,
                    &already_applied_dbuffs,
                    &mut merged_attrs,
                );
            }
        } else if matches!(item.flag as u16, 11..=34 | 92..=94 | 125..=132) {
            if let Ok(fit) = ctx.get_or_rebuild_ship_fitting(item.owner_id, item.location_id) {
                if item_cat == 8 {
                    let slot_flag = item.flag as u16;
                    let launcher_id_opt = fit
                        .modules
                        .values()
                        .find(|m| m.flag_id == slot_flag)
                        .map(|m| m.item_id);
                    let charge_state_opt = launcher_id_opt
                        .and_then(|lid| fit.charge_state(lid, &ctx.sde))
                        .or_else(|| {
                            fit.evaluated_charges
                                .values()
                                .find(|c| c.type_id == item.type_id)
                                .cloned()
                        });
                    if let Some(c_state) = charge_state_opt {
                        let all_aids: std::collections::BTreeSet<u16> = c_state
                            .base_attributes
                            .keys()
                            .copied()
                            .chain(c_state.modifiers.iter().map(|m| m.target_attribute_id))
                            .collect();
                        for aid in all_aids {
                            merged_attrs.insert(aid, c_state.get_attribute(aid, &ctx.sde));
                        }
                    }
                } else if let Some(m_state) = fit.module_state(item.item_id, &ctx.sde) {
                    let all_aids: std::collections::BTreeSet<u16> = m_state
                        .base_attributes
                        .keys()
                        .copied()
                        .chain(m_state.modifiers.iter().map(|m| m.target_attribute_id))
                        .collect();
                    for aid in all_aids {
                        merged_attrs.insert(aid, m_state.get_attribute(aid, &ctx.sde));
                    }
                    crate::services::tactical_modules::apply_projected_rtc_to_module_attrs(
                        ctx,
                        item.location_id,
                        &mut merged_attrs,
                    );
                }
                if item_cat != 8 {
                    let is_mod_online = fit
                        .modules
                        .get(&item.item_id)
                        .map(|m| m.state != eve_dogma::ModuleState::Offline)
                        .unwrap_or_else(|| {
                            ctx.db.with_snapshot(|s| {
                                s.module_online_states.get(&item.item_id).copied().unwrap_or(true)
                            })
                        });
                    merged_attrs.insert(2, if is_mod_online { 1.0 } else { 0.0 });
                    if is_mod_online {
                        fitted_online_ship = Some((item.owner_id, item.location_id));
                    }
                }
                for &aid in &[3u16, 9, 84, 267, 268, 269, 270] {
                    if let Some(&v) = dyn_attrs.get(&aid) {
                        merged_attrs.insert(aid, v);
                    }
                }
            }
        }
    }

    crate::services::tactical_modules::apply_transient_item_attrs(ctx, raw_id, &mut merged_attrs);
    let mut attr_entries: Vec<(PyRep, PyRep)> = merged_attrs
        .into_iter()
        .map(|(k, v)| (PyRep::Integer(k as i64), PyRep::Real(v)))
        .collect();
    attr_entries.sort_by_key(|(k, _)| k.as_i64().unwrap_or(0));

    let mut dyn_entries: Vec<(PyRep, PyRep)> = dyn_attrs
        .into_iter()
        .map(|(k, v)| (PyRep::Integer(k as i64), PyRep::Real(v)))
        .collect();
    dyn_entries.sort_by_key(|(k, _)| k.as_i64().unwrap_or(0));

    let var_reps: Vec<PyRep> = show_info.variations.into_iter().map(|v| PyRep::Integer(v as i64)).collect();
    let custom_name = ctx.db.with_snapshot(|snap| snap.item_custom_names.get(&raw_id).cloned()).unwrap_or_default();
    let now_ft = ctx.now_blueos_filetime_ticks();
    let inv_item = ctx.db.get_item(raw_id).map(|it| eve_proto::DBRowDescriptor::inv_item_header().build_dbrow(item_to_packed_cells(ctx, &it))).unwrap_or(PyRep::None);

    let active_effects = match fitted_online_ship {
        Some((mod_owner_id, mod_ship_id)) => {
            crate::services::inventory_fitting::fitting_module_cycles::online_module_active_effects(
                raw_id,
                mod_owner_id,
                mod_ship_id,
                now_ft,
                true,
            )
        }
        None => PyRep::Dict(Vec::new()),
    };

    Ok(PyRep::keyval([
        ("itemID", PyRep::Integer(raw_id)),
        ("invItem", inv_item),
        ("typeID", PyRep::Integer(show_info.type_id as i64)),
        ("typeName", PyRep::String(show_info.name)),
        ("customName", PyRep::String(custom_name)),
        ("groupID", PyRep::Integer(show_info.group_id as i64)),
        ("categoryID", PyRep::Integer(show_info.category_id as i64)),
        ("ownerID", owner_id),
        ("locationID", location_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
        ("quantity", PyRep::Integer(qty as i64)),
        ("singleton", PyRep::Bool(singleton)),
        ("mass", PyRep::Real(show_info.mass)),
        ("volume", PyRep::Real(show_info.volume)),
        ("capacity", PyRep::Real(show_info.capacity)),
        ("portionSize", PyRep::Integer(show_info.portion_size as i64)),
        ("basePrice", PyRep::Real(show_info.base_price)),
        ("published", PyRep::Bool(show_info.published)),
        ("marketGroupID", show_info.market_group_id.map(|mg| PyRep::Integer(mg as i64)).unwrap_or(PyRep::None)),
        ("variationParentTypeID", show_info.variation_parent_type_id.map(|vp| PyRep::Integer(vp as i64)).unwrap_or(PyRep::None)),
        ("variations", PyRep::List(var_reps)),
        ("dynamicAttributes", PyRep::Dict(dyn_entries)),
        ("attributes", PyRep::Dict(attr_entries)),
        ("activeEffects", active_effects),
        ("time", PyRep::Integer(now_ft)),
        ("wallclockTime", PyRep::Integer(now_ft)),
    ]))
}

#[derive(Clone, Debug, Default)]
struct DogmaSessionCheckState {
    character_id: i64,
    ship_id: Option<i64>,
    solar_system_id: Option<u32>,
    station_id: Option<i64>,
    corporation_id: Option<i64>,
    check_count: u64,
    updated_at: u64,
}

fn dogma_session_check_store(
) -> &'static std::sync::RwLock<HashMap<(u64, u64), DogmaSessionCheckState>> {
    static STORE: std::sync::OnceLock<
        std::sync::RwLock<HashMap<(u64, u64), DogmaSessionCheckState>>,
    > = std::sync::OnceLock::new();
    STORE.get_or_init(|| std::sync::RwLock::new(HashMap::new()))
}

fn lookup_session_check_i64(
    spec: Option<&PyRep>,
    kwargs: &HashMap<String, PyRep>,
    keys: &[&str],
) -> Option<i64> {
    for &k in keys {
        if let Some(val) = kwargs.get(k).and_then(|v| v.as_i64()) {
            return Some(val);
        }
        if let Some(rep) = spec {
            if let Some(val) = rep.dict_get(k).and_then(|v| v.as_i64()) {
                return Some(val);
            }
        }
    }
    None
}

fn handle_dogma_set_session_check(
    ctx: &ServerContext,
    session: &SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let now_unix = ctx.now_unix();
    let clear_requested =
        matches!(args.first(), Some(PyRep::None)) && kwargs.is_empty();
    let spec = args.first().filter(|a| !matches!(a, PyRep::None));

    if !clear_requested {
        if let Some(exp_char) = lookup_session_check_i64(spec, kwargs, &["charid", "charID"]) {
            if exp_char != char_id {
                return Err(BridgeRpcError {
                    code: "SessionCheckMismatch".into(),
                    message: format!(
                        "Session check charid {exp_char} does not match {char_id}"
                    ),
                });
            }
        }
        if let Some(exp_sys) = lookup_session_check_i64(
            spec,
            kwargs,
            &["solarsystemid2", "solarsystemid", "solarSystemID"],
        ) {
            if session.solar_system_id.map(i64::from) != Some(exp_sys) {
                return Err(BridgeRpcError {
                    code: "SessionCheckMismatch".into(),
                    message: format!(
                        "Session check solarsystemid2 {exp_sys} does not match {:?}",
                        session.solar_system_id
                    ),
                });
            }
        }
        if let Some(exp_ship) = lookup_session_check_i64(spec, kwargs, &["shipid", "shipID"]) {
            if session.ship_id != Some(exp_ship) {
                return Err(BridgeRpcError {
                    code: "SessionCheckMismatch".into(),
                    message: format!(
                        "Session check shipid {exp_ship} does not match {:?}",
                        session.ship_id
                    ),
                });
            }
        }
        if let Some(exp_station) =
            lookup_session_check_i64(spec, kwargs, &["stationid", "stationID"])
        {
            if session.station_id != Some(exp_station) {
                return Err(BridgeRpcError {
                    code: "SessionCheckMismatch".into(),
                    message: format!(
                        "Session check stationid {exp_station} does not match {:?}",
                        session.station_id
                    ),
                });
            }
        }
        if let Some(exp_corp) = lookup_session_check_i64(spec, kwargs, &["corpid", "corpID"]) {
            if session.corporation_id != Some(exp_corp) {
                return Err(BridgeRpcError {
                    code: "SessionCheckMismatch".into(),
                    message: format!(
                        "Session check corpid {exp_corp} does not match {:?}",
                        session.corporation_id
                    ),
                });
            }
        }
    }

    let ship_in_db = session.ship_id.is_some_and(|sid| {
        ctx.db.with_snapshot(|snap| {
            snap.items
                .get(&sid)
                .is_some_and(|it| it.owner_id == char_id)
        })
    });
    let fitting_valid = match session.ship_id {
        Some(sid) if ship_in_db => ctx.get_or_rebuild_ship_fitting(char_id, sid).is_ok(),
        Some(_) => false,
        None => true,
    };

    let key = (ctx.instance_id, session.session_id);
    let (check_count, session_check_rep) = if clear_requested {
        let mut guard = dogma_session_check_store()
            .write()
            .unwrap_or_else(|e| e.into_inner());
        guard.remove(&key);
        (0u64, PyRep::None)
    } else {
        let mut guard = dogma_session_check_store()
            .write()
            .unwrap_or_else(|e| e.into_inner());
        let entry = guard.entry(key).or_default();
        entry.character_id = char_id;
        entry.ship_id = session.ship_id;
        entry.solar_system_id = session.solar_system_id;
        entry.station_id = session.station_id;
        entry.corporation_id = session.corporation_id;
        entry.check_count += 1;
        entry.updated_at = now_unix;
        let mut check_fields = vec![("charid", PyRep::Integer(entry.character_id))];
        if let Some(corp) = entry.corporation_id {
            check_fields.push(("corpid", PyRep::Integer(corp)));
        }
        if let Some(ship) = entry.ship_id {
            check_fields.push(("shipid", PyRep::Integer(ship)));
        }
        if let Some(sys) = entry.solar_system_id {
            check_fields.push(("solarsystemid2", PyRep::Integer(i64::from(sys))));
        }
        if let Some(station) = entry.station_id {
            check_fields.push(("stationid", PyRep::Integer(station)));
        }
        (entry.check_count, PyRep::keyval(check_fields))
    };

    Ok(PyRep::keyval([
        ("valid", PyRep::Bool(fitting_valid)),
        ("characterID", PyRep::Integer(char_id)),
        (
            "shipID",
            session.ship_id.map(PyRep::Integer).unwrap_or(PyRep::None),
        ),
        (
            "solarSystemID",
            session
                .solar_system_id
                .map(|s| PyRep::Integer(i64::from(s)))
                .unwrap_or(PyRep::None),
        ),
        (
            "stationID",
            session
                .station_id
                .map(PyRep::Integer)
                .unwrap_or(PyRep::None),
        ),
        (
            "corporationID",
            session
                .corporation_id
                .map(PyRep::Integer)
                .unwrap_or(PyRep::None),
        ),
        (
            "boundObjectID",
            bound_entry
                .map(|b| PyRep::Integer(b.object_id))
                .unwrap_or(PyRep::None),
        ),
        ("sessionCheck", session_check_rep),
        ("fittingValid", PyRep::Bool(fitting_valid)),
        ("checkCount", PyRep::Integer(check_count as i64)),
        ("updatedAt", PyRep::Integer(now_unix as i64)),
    ]))
}

pub(crate) async fn handle_dogma_im(
    ctx: &ServerContext,
    session: &mut SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        // parity: rpc:dogmaIM.SetSessionCheck
        ("dogmaIM", "SetSessionCheck") => {
            Some(handle_dogma_set_session_check(ctx, session, bound_entry, args, kwargs))
        }

        // rpc-lint: allow tools/client-harness/run_journeys bridge probe calls GetShipAttributes
        ("dogmaIM", "GetAllInfo") | ("dogmaIM", "GetShipAttributes") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(ship_id) = bound_entry
                .map(|b| b.object_id)
                .filter(|&id| ctx.db.get_item(id).is_some())
                .or(session.ship_id)
            else {
                return Some(Err(BridgeRpcError { code: "NoActiveShip".into(), message: "No active ship in session".into() }));
            };

            let (ship_type_id, active_stance, entries, module_entries) =
                match evaluate_ship_dogma_attributes(ctx, char_id, ship_id) {
                    Ok(v) => v,
                    Err(e) => return Some(Err(e)),
                };

            if method == "GetAllInfo" {
                Some(Ok(build_all_info(ctx, session, char_id, ship_id, ship_type_id, active_stance, entries, module_entries, args)))
            } else {
                Some(Ok(PyRep::Dict(entries)))
            }
        }

        ("shipStanceMgr", "SetShipStance") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let (ship_id_opt, stance_type_id_opt) = if args.len() >= 2 {
                (
                    args.first().and_then(|a| a.as_i64()),
                    args.get(1).and_then(|a| a.as_i64()),
                )
            } else {
                (None, args.first().and_then(|a| a.as_i64()))
            };
            let Some(ship_id) = ship_id_opt
                .filter(|&id| id > 0)
                .or_else(|| bound_entry.map(|b| b.object_id))
                .or(session.ship_id)
            else {
                return Some(Err(BridgeRpcError { code: "NoActiveShip".into(), message: "No active ship in session".into() }));
            };
            let Some(stance_type_id) = stance_type_id_opt.filter(|&id| id > 0) else {
                return Some(Err(BridgeRpcError { code: "InvalidShipStance".into(), message: "SetShipStance requires a valid stance typeID".into() }));
            };
            let now_ms = ctx.now_unix_secs().saturating_mul(1000);
            let mut fit = match ctx.get_or_rebuild_ship_fitting(char_id, ship_id) {
                Ok(f) => f,
                Err(e) => return Some(Err(e)),
            };
            if fit.active_stance_type_id.is_none() {
                fit.active_stance_type_id =
                    eve_dogma::ShipFittingEngine::default_t3d_stance_type_id(fit.ship.type_id);
            }
            if let Err(e) = fit.switch_ship_stance(stance_type_id as u32, now_ms, &ctx.sde) {
                let code = if e.contains("Cooldown") {
                    "StanceSwitchCooldown"
                } else {
                    "InvalidShipStance"
                };
                return Some(Err(BridgeRpcError { code: code.into(), message: e }));
            }
            let max_vel = fit.ship.get_attribute(eve_sde::attr_ids::MAX_VELOCITY, &ctx.sde);
            ctx.store_ship_fitting(ship_id, fit);
            ctx.notify(
                crate::router::NotificationTarget::Character(char_id),
                "OnStanceActive",
                "charid",
                vec![PyRep::Integer(ship_id), PyRep::Integer(stance_type_id)],
            );
            ctx.notify(
                crate::router::NotificationTarget::Character(char_id),
                "OnModuleAttributeChanges",
                "charid",
                vec![PyRep::List(vec![PyRep::Tuple(vec![
                    PyRep::String("OnModuleAttributeChange".into()),
                    PyRep::Integer(char_id),
                    PyRep::Integer(ship_id),
                    PyRep::Integer(eve_sde::attr_ids::MAX_VELOCITY as i64),
                    PyRep::Integer(ctx.now_blueos_filetime_ticks()),
                    PyRep::Real(max_vel),
                    PyRep::Real(max_vel),
                    PyRep::None,
                ])])],
            );
            Some(Ok(PyRep::Integer(stance_type_id)))
        }

        ("dogmaIM", "ItemGetInfo") => {
            Some(handle_item_get_info(ctx, args))
        }
        ("invbroker", "GetItemInfo") => {
            Some(handle_item_get_info(ctx, args))
        }
        ("invbroker", "GetDynamicItemInfo") => {
            Some(handle_item_get_info(ctx, args))
        }
        ("dogmaIM", "GetDynamicItemInfo") => {
            Some(handle_item_get_info(ctx, args))
        }

        ("dogmaIM", "DestroyFitting") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let Some(item_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError { code: "InvalidArgument".into(), message: "DestroyFitting requires itemID".into() }));
            };
            let undocked = session.station_id.is_none() && session.structure_id.is_none();
            if undocked {
                let now_unix = ctx.now_unix_secs();
                let has_weapons_timer = ctx.crimewatch.read().unwrap_or_else(|e| e.into_inner()).states.get(&char_id).is_some_and(|cw| cw.is_weapon_timer_active(now_unix));
                if has_weapons_timer {
                    return Some(Err(BridgeRpcError { code: "CannotRefitWithWeaponsTimer".into(), message: "Cannot destroy fitted modules or rigs in space while weapons timer is active".into() }));
                }
                let near_fitting_service = match (session.solar_system_id, session.ship_id) {
                    (Some(sys_id), Some(ship_id)) => is_near_in_space_fitting_service(ctx, sys_id, ship_id).await,
                    _ => false,
                };
                if !near_fitting_service {
                    return Some(Err(BridgeRpcError { code: "CannotUnfitInSpace".into(), message: "Destroying fittings in space requires being within range of an anchored, non-reinforced Mobile Depot or capital SMA".into() }));
                }
            }
            if let Some(sys_id) = session.solar_system_id {
                let loc_opt = ctx.db.with_snapshot(|snap| snap.items.get(&item_id).map(|i| i.location_id));
                if let Some(loc) = loc_opt {
                    let sys = ctx.systems.get_or_spawn(sys_id);
                    if sys.has_active_module_cycle_sync(loc, item_id) {
                        return Some(Err(BridgeRpcError { code: "ModuleIsActive".into(), message: format!("Module {item_id} cannot be destroyed while active") }));
                    }
                    if sys.has_active_nanite_repair_sync(loc, item_id) {
                        return Some(Err(BridgeRpcError { code: "ModuleBeingRepaired".into(), message: format!("Module {item_id} cannot be destroyed while undergoing nanite repair") }));
                    }
                }
            }
            let old_fit_loads = ctx
                .db
                .with_snapshot(|s| s.items.get(&item_id).map(|i| i.location_id))
                .and_then(|loc| {
                    ctx.get_or_rebuild_ship_fitting(char_id, loc).ok().map(|f| {
                        (
                            f.ship.get_attribute(eve_sde::attr_ids::CPU_LOAD, &ctx.sde),
                            f.ship.get_attribute(eve_sde::attr_ids::POWER_LOAD, &ctx.sde),
                        )
                    })
                });
            let destroy_res = ctx.db.try_with_mut_snapshot(|snap| {
                if let Some(item) = snap.items.get(&item_id) {
                    let loc = item.location_id;
                    let flag_u16 = item.flag as u16;
                    let is_struct_slot = matches!(flag_u16, 11..=34 | 92..=94 | 164..=171);
                    let struct_opt = snap.structures.get(&loc);
                    if let Some(st) = struct_opt.filter(|st| is_struct_slot && item.owner_id == st.owner_corp_id) {
                        let caller_corp = snap.characters.get(&char_id).map(|c| c.corporation_id).or(session.corporation_id).unwrap_or(0);
                        if st.owner_corp_id != caller_corp {
                            return Err(BridgeRpcError { code: "StructureOwnershipViolation".into(), message: "Only the owning corporation can destroy structure fittings".into() });
                        }
                        if !crate::services::station_structure::structure_lifecycle_rpc::has_structure_manager_role(snap, session, char_id, caller_corp) {
                            return Err(BridgeRpcError { code: "InsufficientCorpRoles".into(), message: "Destroying structure fittings requires Director or Station Manager role".into() });
                        }
                    } else if item.owner_id != char_id {
                        return Err(BridgeRpcError { code: "ItemNotOwnedByCharacter".into(), message: "Cannot destroy an item not owned by character".into() });
                    } else if undocked && (Some(loc) != session.ship_id || !matches!(flag_u16, 11..=34 | 92..=94 | 125..=132 | 164..=171)) {
                        return Err(BridgeRpcError { code: "ItemNotFittedToActiveShip".into(), message: "In-space DestroyFitting requires a fitted item on the active ship".into() });
                    }
                    if snap.module_active_states.contains(&item_id) {
                        return Err(BridgeRpcError { code: "ModuleIsActive".into(), message: format!("Module {item_id} cannot be destroyed while active") });
                    }
                    if (125..=132).contains(&flag_u16) {
                        let has_fitted_modules = snap.items.values().any(|other| {
                            other.location_id == loc && other.item_id != item_id && (11..=34).contains(&(other.flag as u16)) && ctx.sde.types.get(&other.type_id).is_none_or(|t| t.category_id != 8)
                        });
                        if has_fitted_modules {
                            return Err(BridgeRpcError { code: "SubsystemHasFittedModules".into(), message: "Cannot destroy a T3 Strategic Cruiser subsystem while modules are fitted".into() });
                        }
                    }
                    let found = (loc, item.flag as i64, item.type_id as i64, item.quantity as i64);
                    snap.remove_system_cleanup_item(item_id);
                    Ok(Some(found))
                } else {
                    Ok(None)
                }
            });
            let loc_opt = match destroy_res { Ok(v) => v, Err(e) => return Some(Err(e)) };
            if let Some((loc, flag, type_id, qty)) = loc_opt {
                if let Some(sys_id) = session.solar_system_id {
                    let sys = ctx.systems.get_or_spawn(sys_id);
                    let _ = sys.remove_module_cycle(loc, item_id).await;
                }
                let affected_ewar = crate::services::notify_emitters_space_dungeons::flush_module_profile_and_ewar_end(
                    ctx,
                    char_id,
                    session.solar_system_id,
                    loc,
                    item_id,
                );
                if let Some(sys_id) = session.solar_system_id {
                    let sys = ctx.systems.get_or_spawn(sys_id);
                    for tid in affected_ewar {
                        let tp_mult = ctx.effective_target_painter_multiplier(tid);
                        sys.try_recompute_combat_attributes(tid, |t_cs| {
                            t_cs.tp_sig_multiplier = (tp_mult > 1.0 + 1e-6).then_some(tp_mult);
                        });
                    }
                }
                crate::services::tactical_modules::remove_projected_remote_assistance(ctx, loc, item_id);
                ctx.invalidate_ship_fitting(loc);
                if let Ok(new_fit) = ctx.get_or_rebuild_ship_fitting(char_id, loc) {
                    if let Some((old_cpu, old_pg)) = old_fit_loads {
                        let new_cpu = new_fit.ship.get_attribute(eve_sde::attr_ids::CPU_LOAD, &ctx.sde);
                        let new_pg = new_fit.ship.get_attribute(eve_sde::attr_ids::POWER_LOAD, &ctx.sde);
                        let _ = ctx.notify_ship_fitting_load_changes(
                            char_id, loc, old_cpu, new_cpu, old_pg, new_pg,
                        );
                    }
                }
                // V24 client derives `OnModuleUnfitted` from this `OnItemChange` (`ixFlag` change).
                let _ = ctx.notify_fitted_item_trashed(char_id, item_id, type_id, qty, loc, flag);
            }
            Some(Ok(PyRep::keyval([("itemID", PyRep::Integer(item_id)), ("destroyed", PyRep::Bool(true))])))
        }

        ("dogmaIM", "SetModuleOnline") => {
            let (_, _char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let _ship_id = args.first().and_then(|a| a.as_i64()).or(session.ship_id).unwrap_or(0);
            Some(
                crate::services::notify_emitters_skills_trade_godma::handle_dogma_set_module_online(
                    ctx, session, args,
                )
                .await,
            )
        }

        ("dogmaIM", "TakeModuleOffline") => {
            Some(
                crate::services::notify_emitters_skills_trade_godma::handle_dogma_take_module_offline(
                    ctx, session, args,
                )
                .await,
            )
        }

        ("dogmaIM", "AddTarget") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError { code: "NoActiveShip".into(), message: "No active ship in session".into() }));
            };
            let Some(target_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError { code: "InvalidArgument".into(), message: "AddTarget requires integer targetID".into() }));
            };
            if ctx.db.with_snapshot(|s| s.structures.contains_key(&ship_id)) {
                return Some(crate::services::structure_weapons_ops::handle_structure_add_target(ctx, session, char_id, ship_id, target_id).await);
            }
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError { code: "NoSolarSystem".into(), message: "Session is not in space".into() }));
            };
            if let Err(e) = crate::services::tourney_service::check_tourney_lock_restricted(ctx, sys_id, char_id) {
                return Some(Err(e));
            }
            let sys = ctx.systems.get_or_spawn(sys_id);
            if crate::services::tactical_module_mechanics::is_untargetable_interdiction_probe(
                ctx, &sys, target_id,
            )
            .await
            {
                return Some(Err(BridgeRpcError {
                    code: "InvalidTarget".into(),
                    message: "Interdiction probes cannot be targeted directly; use area-of-effect weapons".into(),
                }));
            }
            let is_upwell_target = target_id >= 1_000_000_000_000
                || ctx.db.with_snapshot(|s| s.structures.contains_key(&target_id));
            if is_upwell_target {
                crate::services::station_structure::structure_lifecycle_rpc::ensure_structure_ball_in_space(
                    ctx, sys_id, target_id,
                )
                .await;
            }
            let Some((_, ship_ball)) = sys.get_ball_snapshot(ship_id).await.unwrap_or(None) else {
                return Some(Err(BridgeRpcError { code: "TargetingFailed".into(), message: format!("Ship ball {ship_id} not found in system") }));
            };
            let Some((_, target_ball)) = sys.get_ball_snapshot(target_id).await.unwrap_or(None) else {
                return Some(Err(BridgeRpcError { code: "TargetingFailed".into(), message: format!("Target ball {target_id} not found in system") }));
            };

            let target_char_id = if is_upwell_target {
                None
            } else {
                ctx.db.with_snapshot(|snap| {
                    if snap.characters.contains_key(&target_id) {
                        Some(target_id)
                    } else {
                        snap.characters.values().find(|c| c.active_ship_id == target_id).map(|c| c.character_id)
                    }
                })
            };
            if let Some(t_char) = target_char_id {
                let now_unix = ctx.now_unix();
                if crate::services::space_destiny::is_location_change_invulnerable(ctx, t_char, now_unix) {
                    return Some(Err(BridgeRpcError { code: "TargetInvulnerable".into(), message: format!("Target character {t_char} is invulnerable due to recent location change") }));
                }
            }
            if sys.get_combat_state(target_id).await.ok().flatten().is_some_and(|tcs| tcs.is_tethered) {
                return Some(Err(BridgeRpcError { code: "TargetInvulnerable".into(), message: format!("Target {target_id} is tethered and invulnerable to target locks") }));
            }
            crate::services::space_destiny::break_location_change_invulnerability(ctx, char_id);
            let fit = match ctx.get_or_rebuild_ship_fitting(char_id, ship_id) {
                Ok(f) => f,
                Err(e) => return Some(Err(e)),
            };

            if let Ok(Some(cs)) = sys.get_combat_state(ship_id).await {
                if cs.ecm_jammed_remaining_s > 0.0 {
                    return Some(Err(BridgeRpcError { code: "ShipJammed".into(), message: "Targeting systems are jammed by ECM".into() }));
                }
                if (cs.is_cloaked && cs.gate_cloak_remaining_ticks == 0) || cs.decloak_targeting_delay_remaining_s > 0.0 {
                    return Some(Err(BridgeRpcError { code: "TargetingFailed".into(), message: "Targeting systems cannot acquire lock while cloaked or during sensor recalibration delay".into() }));
                }
            }

            let max_range = fit.ship.get_attribute(attr_ids::MAX_TARGET_RANGE, &ctx.sde);
            let surface_offset = ship_ball.radius + target_ball.radius;
            let dist = ((ship_ball.position - target_ball.position).length() - surface_offset).max(0.0);
            if dist > max_range {
                return Some(Err(BridgeRpcError { code: "TargetingFailed".into(), message: format!("Target distance ({dist:.1} m) exceeds maxTargetRange ({max_range:.1} m)") }));
            }

            let unapplied_triage_ship_bonus: usize = fit
                .modules
                .values()
                .filter(|fm| {
                    matches!(
                        fm.state,
                        eve_dogma::ModuleState::Active | eve_dogma::ModuleState::Overloaded
                    ) && !fit.ship.modifiers.iter().any(|m| {
                        m.source_item_id == fm.item_id
                            && m.target_attribute_id == attr_ids::MAX_LOCKED_TARGETS
                    })
                })
                .map(|fm| match fm.type_id {
                    27951 => 4usize,
                    4294 => 5usize,
                    _ => 0usize,
                })
                .sum();
            let ship_max = (fit.ship.get_attribute(attr_ids::MAX_LOCKED_TARGETS, &ctx.sde).max(1.0) as usize)
                + unapplied_triage_ship_bonus;
            let tm_lvl = ctx.character_skill_levels(char_id).get(&3429).copied().unwrap_or(0);
            let atm_lvl = ctx.character_skill_levels(char_id).get(&3430).copied().unwrap_or(0);
            let char_eval = fit
                .get_character_effective_attribute(attr_ids::MAX_LOCKED_TARGETS, &ctx.sde)
                .round()
                .max(0.0) as usize;
            let skill_char_max = 2 + (tm_lvl.min(5) as usize) + (atm_lvl.min(5) as usize);
            let max_locked = ship_max
                .min(char_eval.max(skill_char_max))
                .max(eve_dogma::compute_effective_max_locked_targets(ship_max, tm_lvl, atm_lvl));
            // Prune stale target locks whose target ball left the ballpark, warped, cloaked, or
            // moved outside `max_range_m` before `step()` ran, so despawned/broken targets never
            // permanently occupy server target slots (`#749`).
            let raw_locks = sys.get_target_locks(ship_id).await.unwrap_or_default();
            let mut current_locks = Vec::with_capacity(raw_locks.len());
            for existing_lock in raw_locks {
                let Some((_, tb)) = sys
                    .get_ball_snapshot(existing_lock.target_id)
                    .await
                    .unwrap_or(None)
                else {
                    let _ = sys.remove_target_lock(ship_id, existing_lock.target_id).await;
                    continue;
                };
                let tb_untarg = sys
                    .get_combat_state(existing_lock.target_id)
                    .await
                    .ok()
                    .flatten()
                    .is_some_and(|c| c.is_cloaked || c.is_warp_exit_invulnerable());
                let tb_warping = matches!(tb.mode, eve_destiny::MovementMode::Warp { .. });
                let tb_out_of_range = existing_lock
                    .max_range_m
                    .is_some_and(|max_r| (ship_ball.position - tb.position).length() > max_r);
                if tb_untarg || tb_warping || tb_out_of_range {
                    let _ = sys.remove_target_lock(ship_id, existing_lock.target_id).await;
                    continue;
                }
                current_locks.push(existing_lock);
            }

            // Passive Targeter (`group_id == 82`, `feat:modules.passive-targeter`) suppresses `"targetedBy"` yellow-box notification!
            let has_active_passive_targeter = fit.modules.values().any(|fm| {
                matches!(
                    fm.state,
                    eve_dogma::ModuleState::Active | eve_dogma::ModuleState::Overloaded
                ) && ctx.sde.types.get(&fm.type_id).is_some_and(|t| t.group_id == 82)
            });

            // If `target_id` is already locked on the server (e.g. client UI dropped it via local
            // `FailLockTarget` and re-issued `AddTarget`), return `locked = true` (`flag = 0`) with
            // the active target list instead of consuming an extra slot or failing `max_locked` (`#749`).
            if let Some(existing) = current_locks.iter().find(|l| l.target_id == target_id) {
                if existing.locked {
                    let locked_ids: Vec<PyRep> = current_locks
                        .iter()
                        .filter(|l| l.locked)
                        .map(|l| PyRep::Integer(l.target_id))
                        .collect();
                    return Some(Ok(PyRep::keyval([
                        ("targetID", PyRep::Integer(target_id)),
                        ("targetIDs", PyRep::List(locked_ids)),
                        ("locked", PyRep::Bool(true)),
                        ("lockTimeMs", PyRep::Real(0.0)),
                        ("passiveLock", PyRep::Bool(has_active_passive_targeter)),
                    ])));
                }
                return Some(Ok(PyRep::keyval([
                    ("targetID", PyRep::Integer(target_id)),
                    ("locked", PyRep::Bool(false)),
                    ("lockTimeMs", PyRep::Real(existing.remaining_lock_ms.max(0.0))),
                    ("passiveLock", PyRep::Bool(has_active_passive_targeter)),
                ])));
            } else if current_locks.len() >= max_locked {
                return Some(Err(BridgeRpcError { code: "TargetingFailed".into(), message: format!("Max locked targets exceeded ({max_locked})") }));
            }

            let scan_res = fit.ship.get_attribute(attr_ids::SCAN_RESOLUTION, &ctx.sde).max(1.0);
            let raw_sig = sys
                .get_combat_state(target_id)
                .await
                .ok()
                .flatten()
                .map(|cs| cs.base_signature_radius.unwrap_or(cs.signature_radius))
                .filter(|&s| s > 0.0)
                .unwrap_or(0.0)
                .max(target_ball.radius)
                .max(35.0);
            let target_sig = ctx.effective_target_signature_radius(target_id, raw_sig);
            let lock_sec = eve_destiny::combat::compute_lock_time_seconds(scan_res, target_sig);
            let lock_time_ms = lock_sec * 1000.0;

            let lock = eve_destiny::TargetLock {
                target_id,
                lock_time_ms,
                remaining_lock_ms: lock_time_ms,
                locked: false,
                max_range_m: Some(max_range + surface_offset),
            };
            if let Err(e) = sys.add_target_lock(ship_id, lock).await {
                return Some(Err(BridgeRpcError { code: "TargetingFailed".into(), message: e }));
            }
            crate::services::space_destiny::break_location_change_invulnerability(ctx, char_id);

            if !has_active_passive_targeter {
                if let Some(target_owner_id) = ctx
                    .db
                    .with_snapshot(|s| s.items.get(&target_id).map(|i| i.owner_id))
                    .filter(|&oid| oid != char_id && oid > 0)
                {
                    ctx.notify(
                        crate::router::NotificationTarget::Character(target_owner_id),
                        "OnTarget",
                        "charid",
                        vec![PyRep::Integer(ship_id), PyRep::String("targetedBy".into())],
                    );
                }
            }

            Some(Ok(PyRep::keyval([
                ("targetID", PyRep::Integer(target_id)),
                ("locked", PyRep::Bool(false)),
                ("lockTimeMs", PyRep::Real(lock_time_ms)),
                ("passiveLock", PyRep::Bool(has_active_passive_targeter)),
            ])))
        }

        ("dogmaIM", "RemoveTarget") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError { code: "NoActiveShip".into(), message: "No active ship in session".into() }));
            };
            let Some(target_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError { code: "InvalidArgument".into(), message: "RemoveTarget requires targetID".into() }));
            };
            let (removed, sys_id_opt) = match (session.station_id, session.solar_system_id) {
                (None, Some(sys_id)) => {
                    let sys = ctx.systems.get_or_spawn(sys_id);
                    (sys.remove_target_lock(ship_id, target_id).await.unwrap_or(false), Some(sys_id))
                }
                _ => (false, None),
            };
            crate::services::tactical_modules::clear_entosis_warmup_on_unlock(ctx, ship_id, Some(target_id));
            crate::services::combat_weapons_spool::clear_entropic_spools_on_target_unlock(ctx, ship_id, char_id, Some(target_id));
            if removed {
                ctx.notify(
                    crate::router::NotificationTarget::Character(char_id),
                    "OnTarget",
                    "charid",
                    vec![PyRep::Integer(target_id), PyRep::String("lost".into())],
                );
                let _ = ctx.notify_target_events(char_id, &[("lost", target_id, "")]);
                if let (Some(sys_id), Some(target_char)) = (
                    sys_id_opt,
                    ctx.db.with_snapshot(|s| {
                        let oid = s.items.get(&target_id)?.owner_id;
                        if oid != char_id && s.characters.get(&oid).is_some_and(|c| c.station_id.is_none() && c.solar_system_id == sys_id_opt.unwrap_or(0)) {
                            Some(oid)
                        } else {
                            None
                        }
                    }),
                ) {
                    let _ = sys_id;
                    let _ = ctx.notify_target_events(target_char, &[("otherlost", ship_id, "")]);
                }
            }
            Some(Ok(PyRep::Integer(target_id)))
        }

        ("dogmaIM", "ClearTargets") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError { code: "NoActiveShip".into(), message: "No active ship in session".into() }));
            };
            let (cleared_targets, sys_id_opt) = match (session.station_id, session.solar_system_id) {
                (None, Some(sys_id)) => {
                    let sys = ctx.systems.get_or_spawn(sys_id);
                    let prior = sys.get_target_locks(ship_id).await.unwrap_or_default();
                    let _ = sys.clear_target_locks(ship_id).await;
                    let locked_ids: Vec<i64> = prior.into_iter().filter(|l| l.locked).map(|l| l.target_id).collect();
                    (locked_ids, Some(sys_id))
                }
                _ => (Vec::new(), None),
            };
            crate::services::tactical_modules::clear_entosis_warmup_on_unlock(ctx, ship_id, None);
            crate::services::combat_weapons_spool::clear_entropic_spools_on_target_unlock(ctx, ship_id, char_id, None);
            if let Some(sys_id) = sys_id_opt {
                if !cleared_targets.is_empty() {
                    let target_chars: Vec<i64> = ctx.db.with_snapshot(|s| {
                        cleared_targets
                            .iter()
                            .filter_map(|tid| {
                                let oid = s.items.get(tid)?.owner_id;
                                if oid != char_id && s.characters.get(&oid).is_some_and(|c| c.station_id.is_none() && c.solar_system_id == sys_id) {
                                    Some(oid)
                                } else {
                                    None
                                }
                            })
                            .collect()
                    });
                    for target_char in target_chars {
                        let _ = ctx.notify_target_events(target_char, &[("otherlost", ship_id, "")]);
                    }
                    let _ = ctx.notify_target_events(char_id, &[("clear", 0, "")]);
                }
            }
            ctx.notify(
                crate::router::NotificationTarget::Character(char_id),
                "OnTargetClear",
                "charid",
                vec![],
            );
            Some(Ok(PyRep::None))
        }

        // parity: rpc:dogmaIM.GetTargets
        ("dogmaIM", "GetTargets") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if session.station_id.is_none() && session.structure_id.is_none() {
                if let (Some(sys_id), Some(ship_id)) = (session.solar_system_id, session.ship_id) {
                    let sys = ctx.systems.get_or_spawn(sys_id);
                    let locks = sys.get_target_locks(ship_id).await.unwrap_or_default();
                    let ids = locks
                        .into_iter()
                        .map(|l| PyRep::Integer(l.target_id))
                        .collect();
                    return Some(Ok(PyRep::List(ids)));
                }
            }
            Some(Ok(PyRep::List(Vec::new())))
        }

        ("dogmaIM", "GetTargeters") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if session.station_id.is_none() && session.structure_id.is_none() {
                if let (Some(sys_id), Some(ship_id)) = (session.solar_system_id, session.ship_id) {
                    let sys = ctx.systems.get_or_spawn(sys_id);
                    let targeter_ids: Vec<PyRep> = {
                        let bp = sys.ballpark.read().unwrap_or_else(|e| e.into_inner());
                        bp.target_locks
                            .iter()
                            .filter_map(|(&source_id, locks)| {
                                if source_id != ship_id
                                    && locks.iter().any(|l| l.target_id == ship_id)
                                {
                                    Some(PyRep::Integer(source_id))
                                } else {
                                    None
                                }
                            })
                            .collect()
                    };
                    return Some(Ok(PyRep::List(targeter_ids)));
                }
            }
            Some(Ok(PyRep::List(Vec::new())))
        }

        ("dogmaIM", "LinkWeapons") | ("charFittingMgr", "LinkWeapons") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let rpc = crate::router::RpcArgs::positional(args);
            let ship_id = match rpc.opt_positive_i64(0, "shipID") {
                Ok(v) => v.or(session.ship_id).unwrap_or(0),
                Err(e) => return Some(Err(e)),
            };
            if !ctx.db.with_snapshot(|s| s.items.get(&ship_id).is_some_and(|it| it.owner_id == char_id)) {
                return Some(Err(BridgeRpcError { code: "ShipOwnershipViolation".into(), message: format!("Ship {ship_id} is not owned by {char_id}") }));
            }
            let master_id = match rpc.req_positive_i64(1, "masterID") {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let slave_id = match rpc.req_positive_i64(2, "slaveID") {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                if !snap.items.get(&ship_id).is_some_and(|s| s.owner_id == char_id) {
                    return Err(BridgeRpcError { code: "ShipOwnershipViolation".into(), message: format!("Ship {ship_id} is not owned by {char_id}") });
                }
                let Some(master_it) = snap.items.get(&master_id) else {
                    return Err(BridgeRpcError { code: "InvalidWeaponGroup".into(), message: format!("Master module {master_id} not found") });
                };
                let Some(slave_it) = snap.items.get(&slave_id) else {
                    return Err(BridgeRpcError { code: "InvalidWeaponGroup".into(), message: format!("Slave module {slave_id} not found") });
                };
                if master_it.location_id != ship_id
                    || slave_it.location_id != ship_id
                    || !matches!(master_it.flag as u16, 27..=34)
                    || !matches!(slave_it.flag as u16, 27..=34)
                    || master_it.type_id != slave_it.type_id
                {
                    return Err(BridgeRpcError { code: "InvalidWeaponGroup".into(), message: "Only identical high-slot weapons fitted to the ship can be grouped".into() });
                }
                let banks = snap.weapon_banks.entry(ship_id).or_default();
                let bank = banks.entry(master_id).or_insert_with(|| vec![master_id]);
                if !bank.contains(&slave_id) {
                    bank.push(slave_id);
                }
                Ok(crate::services::combat_crimewatch::combat_weapons_spool::build_weapon_banks_dict(snap, ship_id))
            });
            if let Ok(ref info) = res {
                crate::services::combat_crimewatch::combat_weapons_spool::notify_weapon_banks_updated(ctx, char_id, ship_id, &[], info);
            }
            Some(res)
        }

        ("dogmaIM", "PeelAndLink") | ("charFittingMgr", "PeelAndLink") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let rpc = crate::router::RpcArgs::positional(args);
            let ship_id = match rpc.opt_positive_i64(0, "shipID") {
                Ok(v) => v.or(session.ship_id).unwrap_or(0),
                Err(e) => return Some(Err(e)),
            };
            if !ctx.db.with_snapshot(|s| s.items.get(&ship_id).is_some_and(|it| it.owner_id == char_id)) {
                return Some(Err(BridgeRpcError { code: "ShipOwnershipViolation".into(), message: format!("Ship {ship_id} is not owned by {char_id}") }));
            }
            let new_mod_id = match rpc.req_positive_i64(1, "newModID") {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let target_bank_mod_id = match rpc.req_positive_i64(2, "targetBankModID") {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                if !snap.items.get(&ship_id).is_some_and(|s| s.owner_id == char_id) {
                    return Err(BridgeRpcError { code: "ShipOwnershipViolation".into(), message: format!("Ship {ship_id} is not owned by {char_id}") });
                }
                let Some(new_it) = snap.items.get(&new_mod_id) else {
                    return Err(BridgeRpcError { code: "InvalidWeaponGroup".into(), message: format!("Module {new_mod_id} not found") });
                };
                let Some(target_it) = snap.items.get(&target_bank_mod_id) else {
                    return Err(BridgeRpcError { code: "InvalidWeaponGroup".into(), message: format!("Target bank module {target_bank_mod_id} not found") });
                };
                if new_it.location_id != ship_id
                    || target_it.location_id != ship_id
                    || !matches!(new_it.flag as u16, 27..=34)
                    || !matches!(target_it.flag as u16, 27..=34)
                    || new_it.type_id != target_it.type_id
                {
                    return Err(BridgeRpcError { code: "InvalidWeaponGroup".into(), message: "Only identical high-slot weapons fitted to the ship can be grouped".into() });
                }
                let banks = snap.weapon_banks.entry(ship_id).or_default();
                let mut found_master = None;
                for (&m, list) in banks.iter() {
                    if m == target_bank_mod_id || list.contains(&target_bank_mod_id) {
                        found_master = Some(m);
                        break;
                    }
                }
                let master = found_master.unwrap_or(target_bank_mod_id);
                let list = banks.entry(master).or_insert_with(|| vec![master]);
                if !list.contains(&new_mod_id) {
                    list.push(new_mod_id);
                }
                Ok(crate::services::combat_crimewatch::combat_weapons_spool::build_weapon_banks_dict(snap, ship_id))
            });
            Some(res.map(|info| {
                crate::services::combat_crimewatch::combat_weapons_spool::notify_weapon_banks_updated(ctx, char_id, ship_id, &[], &info);
                PyRep::None
            }))
        }

        ("dogmaIM", "MergeModuleGroups") | ("charFittingMgr", "MergeModuleGroups") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let rpc = crate::router::RpcArgs::positional(args);
            let ship_id = match rpc.opt_positive_i64(0, "shipID") {
                Ok(v) => v.or(session.ship_id).unwrap_or(0),
                Err(e) => return Some(Err(e)),
            };
            if !ctx.db.with_snapshot(|s| s.items.get(&ship_id).is_some_and(|it| it.owner_id == char_id)) {
                return Some(Err(BridgeRpcError { code: "ShipOwnershipViolation".into(), message: format!("Ship {ship_id} is not owned by {char_id}") }));
            }
            let master_id = match rpc.req_positive_i64(1, "masterID") {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let other_master_id = match rpc.req_positive_i64(2, "otherMasterID") {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                if !snap.items.get(&ship_id).is_some_and(|s| s.owner_id == char_id) {
                    return Err(BridgeRpcError { code: "ShipOwnershipViolation".into(), message: format!("Ship {ship_id} is not owned by {char_id}") });
                }
                let Some(m1) = snap.items.get(&master_id) else {
                    return Err(BridgeRpcError { code: "InvalidWeaponGroup".into(), message: format!("Master module {master_id} not found") });
                };
                let Some(m2) = snap.items.get(&other_master_id) else {
                    return Err(BridgeRpcError { code: "InvalidWeaponGroup".into(), message: format!("Module {other_master_id} not found") });
                };
                if m1.location_id != ship_id
                    || m2.location_id != ship_id
                    || !matches!(m1.flag as u16, 27..=34)
                    || !matches!(m2.flag as u16, 27..=34)
                    || m1.type_id != m2.type_id
                {
                    return Err(BridgeRpcError { code: "InvalidWeaponGroup".into(), message: "Only identical high-slot weapons fitted to the ship can be merged".into() });
                }
                let banks = snap.weapon_banks.entry(ship_id).or_default();
                let had_other = banks.contains_key(&other_master_id) && other_master_id != master_id;
                let other_modules = banks.remove(&other_master_id).unwrap_or_else(|| vec![other_master_id]);
                let master_list = banks.entry(master_id).or_insert_with(|| vec![master_id]);
                for id in other_modules {
                    if !master_list.contains(&id) {
                        master_list.push(id);
                    }
                }
                let destroyed = if had_other { vec![other_master_id] } else { Vec::new() };
                Ok((destroyed, crate::services::combat_crimewatch::combat_weapons_spool::build_weapon_banks_dict(snap, ship_id)))
            });
            Some(res.map(|(destroyed, info)| {
                crate::services::combat_crimewatch::combat_weapons_spool::notify_weapon_banks_updated(ctx, char_id, ship_id, &destroyed, &info);
                PyRep::None
            }))
        }

        ("dogmaIM", "UnlinkModule") | ("charFittingMgr", "UnlinkModule") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let rpc = crate::router::RpcArgs::positional(args);
            let ship_id = match rpc.opt_positive_i64(0, "shipID") {
                Ok(v) => v.or(session.ship_id).unwrap_or(0),
                Err(e) => return Some(Err(e)),
            };
            let mod_id = match rpc.req_positive_i64(1, "moduleID") {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                if !snap.items.get(&ship_id).is_some_and(|s| s.owner_id == char_id) {
                    return Err(BridgeRpcError { code: "ShipOwnershipViolation".into(), message: format!("Ship {ship_id} is not owned by {char_id}") });
                }
                let mut destroyed = Vec::new();
                if let Some(banks) = snap.weapon_banks.get_mut(&ship_id) {
                    for list in banks.values_mut() {
                        list.retain(|&id| id != mod_id);
                    }
                    destroyed = banks.iter().filter(|(k, v)| **k == mod_id || v.len() <= 1).map(|(k, _)| *k).collect();
                    destroyed.sort_unstable();
                    banks.retain(|k, v| *k != mod_id && v.len() > 1);
                }
                Ok((destroyed, crate::services::combat_crimewatch::combat_weapons_spool::build_weapon_banks_dict(snap, ship_id)))
            });
            Some(res.map(|(destroyed, info)| {
                crate::services::combat_crimewatch::combat_weapons_spool::notify_weapon_banks_updated(ctx, char_id, ship_id, &destroyed, &info);
                PyRep::None
            }))
        }

        ("dogmaIM", "DestroyWeaponBank") | ("charFittingMgr", "DestroyWeaponBank") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let rpc = crate::router::RpcArgs::positional(args);
            let ship_id = match rpc.opt_positive_i64(0, "shipID") {
                Ok(v) => v.or(session.ship_id).unwrap_or(0),
                Err(e) => return Some(Err(e)),
            };
            let master_id = match rpc.req_positive_i64(1, "masterID") {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                if !snap.items.get(&ship_id).is_some_and(|s| s.owner_id == char_id) {
                    return Err(BridgeRpcError { code: "ShipOwnershipViolation".into(), message: format!("Ship {ship_id} is not owned by {char_id}") });
                }
                let removed = snap.weapon_banks.get_mut(&ship_id).and_then(|b| b.remove(&master_id)).is_some();
                let destroyed = if removed { vec![master_id] } else { Vec::new() };
                Ok((destroyed, crate::services::combat_crimewatch::combat_weapons_spool::build_weapon_banks_dict(snap, ship_id)))
            });
            Some(res.map(|(destroyed, info)| {
                crate::services::combat_crimewatch::combat_weapons_spool::notify_weapon_banks_updated(ctx, char_id, ship_id, &destroyed, &info);
                PyRep::None
            }))
        }

        ("dogmaIM", "LinkAllWeapons") | ("charFittingMgr", "LinkAllWeapons") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let ship_id = match crate::router::RpcArgs::positional(args).opt_positive_i64(0, "shipID") {
                Ok(v) => v.or(session.ship_id).unwrap_or(0),
                Err(e) => return Some(Err(e)),
            };
            let res = crate::services::combat_crimewatch::combat_weapons::handle_link_all_weapons(ctx, char_id, ship_id);
            Some(res)
        }

        ("dogmaIM", "UnlinkAllModules") | ("charFittingMgr", "UnlinkAllModules") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let ship_id = match crate::router::RpcArgs::positional(args).opt_positive_i64(0, "shipID") {
                Ok(v) => v.or(session.ship_id).unwrap_or(0),
                Err(e) => return Some(Err(e)),
            };
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                if !snap.items.get(&ship_id).is_some_and(|s| s.owner_id == char_id) {
                    return Err(BridgeRpcError { code: "ShipOwnershipViolation".into(), message: format!("Ship {ship_id} is not owned by {char_id}") });
                }
                let mut destroyed: Vec<i64> = snap.weapon_banks.remove(&ship_id).map(|b| b.into_keys().collect()).unwrap_or_default();
                destroyed.sort_unstable();
                Ok(destroyed)
            });
            Some(res.map(|destroyed| {
                crate::services::combat_crimewatch::combat_weapons_spool::notify_weapon_banks_updated(ctx, char_id, ship_id, &destroyed, &PyRep::Dict(Vec::new()));
                PyRep::None
            }))
        }

        ("dogmaIM", "LoadAmmo") => Some(crate::services::notify_emitters_seasons_tactical::handle_load_ammo(ctx, session, args)),

        ("dogmaIM", "UnloadAmmo") => Some(handle_unload_ammo(ctx, session, args)),

        ("dogmaIM", "GetLayerDamageValuesByItems") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let mut item_ids: Vec<i64> = match args.first() {
                Some(PyRep::Integer(id)) if *id > 0 => vec![*id],
                arg => crate::services::combat_crimewatch::extract_id_collection(arg)
                    .into_iter()
                    .filter(|id| *id > 0)
                    .collect(),
            };
            if item_ids.is_empty() {
                if let Some(sid) = session.ship_id {
                    item_ids.push(sid);
                } else {
                    item_ids.push(1);
                }
            }
            let now_ft = ctx.now_blueos_filetime_ticks();
            let entries: Vec<(PyRep, PyRep)> = ctx.db.with_snapshot(|snap| {
                item_ids
                    .into_iter()
                    .map(|iid| {
                        let (armor_dmg, hull_dmg, shield_cur, shield_cap) = snap
                            .items
                            .get(&iid)
                            .filter(|it| it.owner_id == char_id || session.has_gm_role())
                            .map(|it| {
                                let state = crate::sp6_lifecycle::read_unified_ship_damage_state(
                                    ctx,
                                    snap,
                                    iid,
                                    it.type_id,
                                    None,
                                );
                                (state.armor_damage, state.hull_damage, state.shield_hp, state.max_shield)
                            })
                            .unwrap_or((0.0, 0.0, 400.0, 400.0));
                        let kv = PyRep::keyval([
                            (
                                "shieldInfo",
                                PyRep::Tuple(vec![
                                    PyRep::Real(shield_cur),
                                    PyRep::Real(shield_cap),
                                    PyRep::Integer(now_ft),
                                ]),
                            ),
                            ("armorDamage", PyRep::Real(armor_dmg)),
                            ("hullDamage", PyRep::Real(hull_dmg)),
                        ]);
                        (PyRep::Integer(iid), kv)
                    })
                    .collect()
            });
            Some(Ok(PyRep::Dict(entries)))
        }

        _ => None,
    }
}

#[cfg(test)]
#[path = "fitting_dogma_im_tests.rs"]
mod fitting_dogma_im_tests;

