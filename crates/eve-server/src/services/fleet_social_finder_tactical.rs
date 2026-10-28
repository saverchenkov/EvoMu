//! Fleet Finder, Join Requests, Fleet Setups, Formations, Target Tags & Watchlist (`SO-2` / `SO-3`).

use crate::router::{cache_notifications::NotificationTarget, ServerContext};
use eve_db::{FleetMemberRecord, FleetRole, FleetSquadRecord, FleetWingRecord};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::{HashMap, HashSet};

use super::fleet_social_fleets::{
    ensure_fleet_channel_member, i64_to_role, role_name_str, role_to_i64, FleetAdvertState,
    FleetSetupRecord,
};
use super::fleet_social_notify as notify;

const MAX_WATCHLIST_SIZE: usize = 15;

pub(crate) fn sync_fleet_setups_to_db(ctx: &ServerContext) {
    let map = ctx.fleet_extras.lock().unwrap().saved_setups.clone();
    if let Ok(json) = serde_json::to_string(&map) {
        ctx.db.with_mut_snapshot(|snap| {
            snap.service_state_blobs.insert("fleet_saved_setups".into(), json);
        });
    }
}

pub(crate) fn ensure_fleet_setups_loaded(ctx: &ServerContext) {
    if !ctx.fleet_extras.lock().unwrap().saved_setups.is_empty() {
        return;
    }
    let persisted = ctx.db.with_snapshot(|snap| {
        snap.service_state_blobs.get("fleet_saved_setups").and_then(|j| {
            serde_json::from_str::<HashMap<i64, HashMap<String, FleetSetupRecord>>>(j).ok()
        })
    });
    if let Some(map) = persisted {
        let mut ex = ctx.fleet_extras.lock().unwrap();
        for (cid, setups) in map {
            ex.saved_setups.entry(cid).or_insert(setups);
        }
    }
}

pub(crate) fn sync_fleet_formations_to_db(ctx: &ServerContext) {
    let map = ctx.fleet_extras.lock().unwrap().stored_formations.clone();
    if let Ok(json) = serde_json::to_string(&map) {
        ctx.db.with_mut_snapshot(|snap| {
            snap.service_state_blobs.insert("fleet_stored_formations".into(), json);
        });
    }
}

pub(crate) fn ensure_fleet_formations_loaded(ctx: &ServerContext) {
    if !ctx.fleet_extras.lock().unwrap().stored_formations.is_empty() {
        return;
    }
    let persisted = ctx.db.with_snapshot(|snap| {
        snap.service_state_blobs
            .get("fleet_stored_formations")
            .and_then(|j| serde_json::from_str::<HashMap<i64, (i64, f64, f64)>>(j).ok())
    });
    if let Some(map) = persisted {
        let mut ex = ctx.fleet_extras.lock().unwrap();
        for (cid, f) in map {
            ex.stored_formations.entry(cid).or_insert(f);
        }
    }
}

pub(crate) fn formation_name(formation_id: i64) -> &'static str {
    match formation_id {
        1 => "Plane",
        2 => "Sphere",
        3 => "Arrow",
        4 => "Wall",
        5 => "Relative",
        _ => "Point",
    }
}

async fn resolve_ship_hp_ratios(ctx: &ServerContext, target_char_id: i64) -> (Option<i64>, f64, f64, f64) {
    let info = ctx.db.with_snapshot(|snap| {
        let c = snap.characters.get(&target_char_id)?;
        let ship_id = c.active_ship_id;
        let item_info = snap.items.get(&ship_id).map(|it| {
            (
                it.type_id,
                it.dynamic_attributes.get(&264).copied(),
                it.dynamic_attributes.get(&266).copied(),
                it.dynamic_attributes.get(&3).copied(),
            )
        });
        Some((c.solar_system_id, ship_id, item_info))
    });
    let Some((sys_id, ship_id, item_info)) = info else {
        return (None, 1.0, 1.0, 1.0);
    };
    if ship_id > 0 {
        if let Some((_, sys)) = ctx.systems.active_system_handles().into_iter().find(|(id, _)| *id == sys_id) {
            if let Ok(Some(cs)) = sys.get_combat_state(ship_id).await {
                let sr = (cs.shield_hp / cs.max_shield_hp.max(1.0)).clamp(0.0, 1.0);
                let ar = (cs.armor_hp / cs.max_armor_hp.max(1.0)).clamp(0.0, 1.0);
                let hr = (cs.structure_hp / cs.max_structure_hp.max(1.0)).clamp(0.0, 1.0);
                return (Some(ship_id), sr, ar, hr);
            }
        }
        if let Some((type_id, s_chg, a_dmg, h_dmg)) = item_info {
            let mut cs = ctx.build_ship_combat_state(target_char_id, ship_id, type_id);
            if let Some(sc) = s_chg { cs.shield_hp = sc.clamp(0.0, cs.max_shield_hp); }
            if let Some(ad) = a_dmg { cs.armor_hp = (cs.max_armor_hp - ad).clamp(0.0, cs.max_armor_hp); }
            if let Some(hd) = h_dmg { cs.structure_hp = (cs.max_structure_hp - hd).clamp(0.0, cs.max_structure_hp); }
            crate::sp6_lifecycle::apply_persisted_hp_to_combat_state(ctx, ship_id, &mut cs);
            let sr = (cs.shield_hp / cs.max_shield_hp.max(1.0)).clamp(0.0, 1.0);
            let ar = (cs.armor_hp / cs.max_armor_hp.max(1.0)).clamp(0.0, 1.0);
            let hr = (cs.structure_hp / cs.max_structure_hp.max(1.0)).clamp(0.0, 1.0);
            return (Some(ship_id), sr, ar, hr);
        }
    }
    (None, 1.0, 1.0, 1.0)
}

pub async fn handle_fleet_watchlist_ops(
    ctx: &ServerContext,
    session: &mut SessionState,
    method: &str,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    // parity: feat:fleet.watchlist-broadcast-tracking
    let (_, char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let Some(fleet_id) = session.fleet_id else {
        return Some(Err(BridgeRpcError { code: "NotInFleet".into(), message: "Character is not in a fleet".into() }));
    };
    notify::ensure_fleet_watchlist_loaded(ctx);

    if method == "GetWatchlist" {
        let watched_ids: Vec<i64> = {
            let ex = ctx.fleet_extras.lock().unwrap();
            let mut v: Vec<i64> = ex.watchlist.get(&char_id).map(|s| s.iter().copied().collect()).unwrap_or_default();
            v.sort_unstable();
            v
        };
        let mut entries = Vec::with_capacity(watched_ids.len());
        for tid in watched_ids {
            let (ship_opt, sr, ar, hr) = resolve_ship_hp_ratios(ctx, tid).await;
            entries.push(PyRep::keyval([
                ("characterID", PyRep::Integer(tid)),
                ("shipID", ship_opt.map(PyRep::Integer).unwrap_or(PyRep::None)),
                ("shieldRatio", PyRep::Real(sr)),
                ("armorRatio", PyRep::Real(ar)),
                ("hullRatio", PyRep::Real(hr)),
            ]));
        }
        return Some(Ok(PyRep::List(entries)));
    }

    if method == "LockFromBroadcast" || method == "TargetFromBroadcast" {
        let Some(target_id) = args.first().and_then(|a| a.as_i64()) else {
            return Some(Err(BridgeRpcError { code: "InvalidArgument".into(), message: "LockFromBroadcast requires targetID".into() }));
        };
        let caller_sys = session.solar_system_id.unwrap_or(0);
        let target_in_sys = ctx.db.with_snapshot(|snap| {
            if let Some(ch) = snap.characters.get(&target_id) {
                ch.solar_system_id == caller_sys && ch.station_id.is_none() && ch.structure_id.is_none()
            } else {
                true
            }
        });
        if !target_in_sys {
            return Some(Err(BridgeRpcError { code: "TargetNotInSystem".into(), message: "Broadcast target is not in your solar system".into() }));
        }
        let (ship_opt, sr, ar, hr) = resolve_ship_hp_ratios(ctx, target_id).await;
        ctx.notify(
            NotificationTarget::Character(char_id),
            "OnTarget",
            "charid",
            vec![PyRep::String("add".into()), PyRep::Integer(ship_opt.unwrap_or(target_id))],
        );
        return Some(Ok(PyRep::keyval([
            ("targetID", PyRep::Integer(target_id)),
            ("shipID", PyRep::Integer(ship_opt.unwrap_or(target_id))),
            ("locked", PyRep::Bool(true)),
            ("shieldRatio", PyRep::Real(sr)),
            ("armorRatio", PyRep::Real(ar)),
            ("hullRatio", PyRep::Real(hr)),
        ])));
    }

    if method == "RegisterForDamageUpdates" {
        let mut target_ids = Vec::new();
        if let Some(first) = args.first() {
            match first {
                PyRep::List(list) | PyRep::Tuple(list) => {
                    for item in list {
                        if let Some(tid) = item.as_i64() { target_ids.push(tid); }
                    }
                }
                _ => if let Some(tid) = first.as_i64() { target_ids.push(tid); },
            }
        }
        if target_ids.len() > MAX_WATCHLIST_SIZE {
            return Some(Err(BridgeRpcError {
                code: "WatchlistFull".into(),
                message: format!("Watchlist cannot exceed {MAX_WATCHLIST_SIZE} fleet members"),
            }));
        }
        let valid = ctx.db.with_snapshot(|snap| {
            snap.fleets.get(&fleet_id).is_some_and(|f| target_ids.iter().all(|tid| f.members.contains(tid)))
        });
        if !valid {
            return Some(Err(BridgeRpcError { code: "MemberNotFound".into(), message: "One or more target characters are not in your fleet".into() }));
        }
        {
            let mut ex = ctx.fleet_extras.lock().unwrap();
            let set = ex.watchlist.entry(char_id).or_default();
            set.clear();
            for &tid in &target_ids { set.insert(tid); }
        }
        notify::sync_fleet_watchlist_to_db(ctx);
        for &tid in &target_ids {
            let (ship_opt, sr, ar, hr) = resolve_ship_hp_ratios(ctx, tid).await;
            if let Some(ship_id) = ship_opt {
                let _ = ctx.notify_damage_state_change(NotificationTarget::Character(char_id), ship_id, sr, ar, hr);
            }
            let fleet_state = notify::build_fleet_state_keyval(ctx, Some(fleet_id), tid, sr, ar, hr);
            ctx.notify(NotificationTarget::Character(char_id), "OnFleetStateChange", "charid", vec![fleet_state]);
        }
        return Some(Ok(PyRep::keyval([
            ("characterID", PyRep::Integer(char_id)),
            ("watched", PyRep::Bool(!target_ids.is_empty())),
        ])));
    }

    let mut target_ids = Vec::new();
    match args.first() {
        Some(PyRep::List(list) | PyRep::Tuple(list)) => {
            for item in list {
                if let Some(tid) = item.as_i64() { target_ids.push(tid); }
            }
        }
        Some(first) => if let Some(tid) = first.as_i64() { target_ids.push(tid); },
        None => target_ids.push(char_id),
    }
    let mut fav_ids = Vec::new();
    if let Some(PyRep::List(list) | PyRep::Tuple(list)) = args.get(1) {
        for item in list {
            if let Some(fid) = item.as_i64() { fav_ids.push(fid); }
        }
    }
    let in_fleet = !target_ids.is_empty()
        && ctx.db.with_snapshot(|snap| {
            snap.fleets.get(&fleet_id).is_some_and(|f| {
                target_ids.iter().all(|tid| f.members.contains(tid)) && fav_ids.iter().all(|fid| f.members.contains(fid))
            })
        });
    if !in_fleet {
        return Some(Err(BridgeRpcError { code: "MemberNotFound".into(), message: "Target character is not in your fleet".into() }));
    }
    let watched = method != "RemoveFromWatchlist";
    {
        let mut ex = ctx.fleet_extras.lock().unwrap();
        let set = ex.watchlist.entry(char_id).or_default();
        if watched {
            let mut projected = set.clone();
            for &fid in &fav_ids { projected.insert(fid); }
            for &tid in &target_ids { projected.insert(tid); }
            if projected.len() > MAX_WATCHLIST_SIZE {
                return Some(Err(BridgeRpcError {
                    code: "WatchlistFull".into(),
                    message: format!("Watchlist cannot exceed {MAX_WATCHLIST_SIZE} fleet members"),
                }));
            }
        }
        for &fid in &fav_ids {
            if watched || !target_ids.contains(&fid) { set.insert(fid); }
        }
        for &tid in &target_ids {
            if watched { set.insert(tid); } else { set.remove(&tid); }
        }
    }
    notify::sync_fleet_watchlist_to_db(ctx);
    let mut first_ratios = (1.0, 1.0, 1.0);
    for (idx, &tid) in target_ids.iter().enumerate() {
        let (ship_opt, s_ratio, a_ratio, h_ratio) = resolve_ship_hp_ratios(ctx, tid).await;
        if idx == 0 { first_ratios = (s_ratio, a_ratio, h_ratio); }
        if watched {
            if let Some(ship_id) = ship_opt {
                let _ = ctx.notify_damage_state_change(NotificationTarget::Character(char_id), ship_id, s_ratio, a_ratio, h_ratio);
            }
            let fleet_state = notify::build_fleet_state_keyval(ctx, Some(fleet_id), tid, s_ratio, a_ratio, h_ratio);
            ctx.notify(NotificationTarget::Character(char_id), "OnFleetStateChange", "charid", vec![fleet_state]);
        }
    }
    let primary_id = target_ids.first().copied().unwrap_or(char_id);
    Some(Ok(PyRep::keyval([
        ("characterID", PyRep::Integer(primary_id)),
        ("watched", PyRep::Bool(watched)),
        ("shieldRatio", PyRep::Real(first_ratios.0)),
        ("armorRatio", PyRep::Real(first_ratios.1)),
        ("hullRatio", PyRep::Real(first_ratios.2)),
    ])))
}

pub(super) async fn handle_finder_and_tactical(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("fleetObjectHandler", "StoreFleetSetup")
        | ("fleetMgr", "StoreFleetSetup") => {
            // parity: feat:fleet.fleet-setups
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            ensure_fleet_setups_loaded(ctx);
            let setup_name = args.first().and_then(|a| a.as_str()).or_else(|| kwargs.get("setupName").and_then(|a| a.as_str())).unwrap_or("Default Setup").trim().to_string();
            if setup_name.is_empty() {
                return Some(Err(BridgeRpcError { code: "InvalidArgument".into(), message: "Setup name cannot be empty".into() }));
            }
            let (cur_motd, cur_free, cur_max, cur_default_sq, cur_wings) = if let Some(fid) = session.fleet_id {
                let (max_sz, auto_sq) = {
                    let ex = ctx.fleet_extras.lock().unwrap();
                    (ex.max_sizes.get(&fid).copied().unwrap_or(256), ex.auto_join_squads.get(&fid).copied().flatten())
                };
                ctx.db.with_snapshot(|snap| {
                    if let Some(f) = snap.fleets.get(&fid) {
                        let mut w_list = Vec::new();
                        for &wid in &f.wings {
                            if let Some(w) = f.wing_details.get(&wid) {
                                let s_names: Vec<String> = w.squad_ids.iter().filter_map(|sid| f.squad_details.get(sid).map(|s| s.name.clone())).collect();
                                w_list.push((w.name.clone(), s_names));
                            }
                        }
                        (f.motd.clone(), f.free_move, max_sz, auto_sq, w_list)
                    } else {
                        (String::new(), false, 256, None, Vec::new())
                    }
                })
            } else {
                (String::new(), false, 256, None, Vec::new())
            };
            let motd = args.get(1).and_then(|a| a.as_str()).or_else(|| kwargs.get("motd").and_then(|a| a.as_str())).unwrap_or(&cur_motd).to_string();
            let is_free_move = args.get(2).and_then(|a| match a { PyRep::Bool(b) => Some(*b), PyRep::Integer(i) => Some(*i != 0), _ => None })
                .or_else(|| kwargs.get("isFreeMove").and_then(|a| a.as_bool())).unwrap_or(cur_free);
            let max_size = args.get(3).and_then(|a| a.as_i64()).or_else(|| kwargs.get("maxSize").and_then(|a| a.as_i64()))
                .map(|v| v.clamp(1, 256) as u16).unwrap_or(cur_max);
            let wings = if let Some(PyRep::List(w_items) | PyRep::Tuple(w_items)) = args.get(4).or_else(|| kwargs.get("wings")) {
                let mut parsed = Vec::new();
                for w_rep in w_items {
                    if let Some(w_name) = w_rep.dict_get("name").and_then(|v| v.as_str()).or_else(|| w_rep.as_str()) {
                        let mut sqs = Vec::new();
                        if let Some(PyRep::List(s_items) | PyRep::Tuple(s_items)) = w_rep.dict_get("squads") {
                            for s_rep in s_items {
                                if let Some(sn) = s_rep.as_str().or_else(|| s_rep.dict_get("name").and_then(|v| v.as_str())) {
                                    sqs.push(sn.to_string());
                                }
                            }
                        }
                        parsed.push((w_name.to_string(), sqs));
                    }
                }
                if parsed.is_empty() { cur_wings } else { parsed }
            } else {
                cur_wings
            };
            let rec = FleetSetupRecord {
                setup_name: setup_name.clone(),
                motd: motd.clone(),
                is_free_move,
                max_size,
                default_squad_id: cur_default_sq,
                wings: wings.clone(),
            };
            ctx.fleet_extras.lock().unwrap().saved_setups.entry(char_id).or_default().insert(setup_name.clone(), rec);
            sync_fleet_setups_to_db(ctx);
            ctx.notify(NotificationTarget::Character(char_id), "OnFleetSetupChanged", "charid", vec![PyRep::String(setup_name.clone())]);
            Some(Ok(PyRep::keyval([
                ("setupName", PyRep::String(setup_name)),
                ("motd", PyRep::String(motd)),
                ("isFreeMove", PyRep::Bool(is_free_move)),
                ("maxSize", PyRep::Integer(max_size as i64)),
                ("wingCount", PyRep::Integer(wings.len() as i64)),
            ])))
        }

        ("fleetObjectHandler", "GetFleetSetups") | ("fleetMgr", "GetFleetSetups") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            ensure_fleet_setups_loaded(ctx);
            let setups = ctx.fleet_extras.lock().unwrap().saved_setups.get(&char_id).cloned().unwrap_or_default();
            let mut entries = Vec::with_capacity(setups.len());
            for (name, s) in setups {
                let wings_rep: Vec<PyRep> = s.wings.into_iter().map(|(wn, sqs)| {
                    PyRep::keyval([("name", PyRep::String(wn)), ("squads", PyRep::List(sqs.into_iter().map(PyRep::String).collect()))])
                }).collect();
                entries.push((
                    PyRep::String(name.clone()),
                    PyRep::keyval([
                        ("setupName", PyRep::String(name)),
                        ("motd", PyRep::String(s.motd)),
                        ("isFreeMove", PyRep::Bool(s.is_free_move)),
                        ("maxSize", PyRep::Integer(s.max_size as i64)),
                        ("wings", PyRep::List(wings_rep)),
                    ]),
                ));
            }
            Some(Ok(PyRep::Dict(entries)))
        }

        ("fleetObjectHandler", "DeleteFleetSetup") | ("fleetMgr", "DeleteFleetSetup") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            ensure_fleet_setups_loaded(ctx);
            let setup_name = args.first().and_then(|a| a.as_str()).unwrap_or("");
            let removed = ctx.fleet_extras.lock().unwrap().saved_setups.get_mut(&char_id).map(|m| m.remove(setup_name).is_some()).unwrap_or(false);
            sync_fleet_setups_to_db(ctx);
            Some(Ok(PyRep::keyval([("setupName", PyRep::String(setup_name.to_string())), ("deleted", PyRep::Bool(removed))])))
        }

        ("fleetObjectHandler", "LoadFleetSetup") | ("fleetMgr", "LoadFleetSetup") => {
            // parity: feat:fleet.fleet-setups
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let Some(fleet_id) = session.fleet_id else {
                return Some(Err(BridgeRpcError { code: "NotInFleet".into(), message: "Character is not in a fleet".into() }));
            };
            ensure_fleet_setups_loaded(ctx);
            let setup_name = args.first().and_then(|a| a.as_str()).unwrap_or("Default Setup");
            let stored = ctx.fleet_extras.lock().unwrap().saved_setups.get(&char_id).and_then(|m| m.get(setup_name)).cloned();
            let new_motd = args.get(1).and_then(|a| a.as_str()).map(|s| s.to_string())
                .or_else(|| stored.as_ref().map(|s| s.motd.clone()))
                .unwrap_or_else(|| "Loaded Fleet Setup MOTD".to_string());
            let is_free_move = args.get(2).and_then(|a| match a { PyRep::Bool(b) => Some(*b), PyRep::Integer(i) => Some(*i != 0), _ => None })
                .or_else(|| stored.as_ref().map(|s| s.is_free_move))
                .unwrap_or(false);
            let max_size = args.get(3).and_then(|a| a.as_i64()).map(|v| v.clamp(1, 256) as u16)
                .or_else(|| stored.as_ref().map(|s| s.max_size))
                .unwrap_or(256);

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(fleet) = snap.fleets.get_mut(&fleet_id) else {
                    return Err(BridgeRpcError { code: "FleetNotFound".into(), message: format!("Fleet {fleet_id} not found") });
                };
                let is_fc = fleet.member_details.get(&char_id).is_some_and(|m| m.role == FleetRole::FleetCommander);
                if fleet.boss_id != char_id && !is_fc {
                    return Err(BridgeRpcError { code: "FleetPermissionDenied".into(), message: "Only the Fleet Boss or Fleet Commander can load a fleet setup".into() });
                }
                fleet.motd = new_motd.clone();
                let old = (fleet.free_move, fleet.is_registered);
                fleet.free_move = is_free_move;
                let mut added_wings = Vec::new();
                let mut added_squads = Vec::new();
                if let Some(st) = &stored {
                    for (idx, (w_name, sq_names)) in st.wings.iter().enumerate() {
                        let wid = if let Some(&existing_wid) = fleet.wings.get(idx) {
                            if let Some(w) = fleet.wing_details.get_mut(&existing_wid) { w.name = w_name.clone(); }
                            existing_wid
                        } else if fleet.wings.len() < 5 {
                            let nw = (fleet.wings.iter().max().copied().unwrap_or(0) + 1).max(1);
                            fleet.wings.push(nw);
                            fleet.wing_details.insert(nw, FleetWingRecord { wing_id: nw, name: w_name.clone(), commander_id: None, squad_ids: Vec::new() });
                            added_wings.push(nw);
                            nw
                        } else {
                            continue;
                        };
                        let cur_sq_ids = fleet.wing_details.get(&wid).map(|w| w.squad_ids.clone()).unwrap_or_default();
                        for (s_idx, s_name) in sq_names.iter().enumerate() {
                            if let Some(&existing_sid) = cur_sq_ids.get(s_idx) {
                                if let Some(s) = fleet.squad_details.get_mut(&existing_sid) { s.name = s_name.clone(); }
                            } else if fleet.wing_details.get(&wid).is_some_and(|w| w.squad_ids.len() < 5) {
                                let ns = (fleet.squads.iter().max().copied().unwrap_or(0) + 1).max(1);
                                if let Some(w) = fleet.wing_details.get_mut(&wid) { w.squad_ids.push(ns); }
                                fleet.squads.push(ns);
                                fleet.squad_details.insert(ns, FleetSquadRecord { squad_id: ns, wing_id: wid, name: s_name.clone(), commander_id: None });
                                added_squads.push((wid, ns));
                            }
                        }
                    }
                }
                Ok((old, fleet.is_registered, added_wings, added_squads))
            });
            match res {
                Ok(((old_free, old_reg), reg, added_wings, added_squads)) => {
                    {
                        let mut ex = ctx.fleet_extras.lock().unwrap();
                        ex.fleet_names.insert(fleet_id, setup_name.to_string());
                        ex.max_sizes.insert(fleet_id, max_size);
                        if let Some(st) = &stored {
                            if let Some(dsq) = st.default_squad_id { ex.auto_join_squads.insert(fleet_id, Some(dsq)); }
                        }
                    }
                    for wid in added_wings {
                        ctx.notify(NotificationTarget::Fleet(fleet_id), "OnFleetWingAdded", "fleetid", vec![PyRep::Integer(wid)]);
                    }
                    for (wid, sid) in added_squads {
                        ctx.notify(NotificationTarget::Fleet(fleet_id), "OnFleetSquadAdded", "fleetid", vec![PyRep::Integer(wid), PyRep::Integer(sid)]);
                    }
                    notify::emit_motd(ctx, NotificationTarget::Fleet(fleet_id), &new_motd, true);
                    let auto = ctx.fleet_extras.lock().unwrap().auto_join_squads.get(&fleet_id).copied().flatten();
                    let old = notify::options_keyval(old_free, old_reg, auto);
                    notify::emit_options_changed(ctx, fleet_id, old, notify::options_keyval(is_free_move, reg, auto));
                    Some(Ok(PyRep::keyval([
                        ("fleetID", PyRep::Integer(fleet_id)),
                        ("setupName", PyRep::String(setup_name.to_string())),
                        ("motd", PyRep::String(new_motd)),
                        ("isFreeMove", PyRep::Bool(is_free_move)),
                        ("maxSize", PyRep::Integer(max_size as i64)),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("fleetProxy", "AddFleetFinderAdvert")
        | ("fleetProxy", "UpdateAdvertInfo")
        | ("fleetProxy", "UpdateAdvertAllowedEntities")
        | ("fleetProxy", "UpdateFleetAdvertWithNewLeader") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let Some(fleet_id) = session.fleet_id else {
                return Some(Err(BridgeRpcError { code: "NotInFleet".into(), message: "Character is not in a fleet".into() }));
            };
            let is_auth = ctx.db.with_snapshot(|snap| {
                snap.fleets.get(&fleet_id).is_some_and(|fl| fl.boss_id == char_id)
                    || ctx.fleet_extras.lock().unwrap().adverts.get(&fleet_id).is_some_and(|ad| ad.leader_id == char_id)
            });
            if !is_auth {
                return Some(Err(BridgeRpcError { code: "FleetRoleDenied".into(), message: "Only fleet boss or advert leader can modify advert".into() }));
            }
            if method == "UpdateAdvertAllowedEntities" {
                let mut entities = HashSet::new();
                if let Some(PyRep::List(list) | PyRep::Tuple(list)) = args.first() {
                    for v in list {
                        if let Some(id) = v.as_i64() { entities.insert(id); }
                    }
                }
                if let Some(ad) = ctx.fleet_extras.lock().unwrap().adverts.get_mut(&fleet_id) {
                    ad.allowed_entities = entities.clone();
                }
                return Some(Ok(PyRep::keyval([("fleetID", PyRep::Integer(fleet_id)), ("allowedCount", PyRep::Integer(entities.len() as i64))])));
            }
            if method == "UpdateFleetAdvertWithNewLeader" {
                let new_leader = args.first().and_then(|a| a.as_i64()).unwrap_or(char_id);
                let is_member = ctx.db.with_snapshot(|snap| {
                    snap.fleets.get(&fleet_id).is_some_and(|fl| fl.members.contains(&new_leader) || fl.member_details.contains_key(&new_leader))
                });
                if !is_member {
                    return Some(Err(BridgeRpcError { code: "CustomError".into(), message: "New advert leader must be an active member of the fleet".into() }));
                }
                if let Some(ad) = ctx.fleet_extras.lock().unwrap().adverts.get_mut(&fleet_id) {
                    ad.leader_id = new_leader;
                }
                return Some(Ok(PyRep::keyval([("fleetID", PyRep::Integer(fleet_id)), ("leaderID", PyRep::Integer(new_leader))])));
            }
            if method == "UpdateAdvertInfo" {
                let mut ex = ctx.fleet_extras.lock().unwrap();
                if let Some(ad) = ex.adverts.get_mut(&fleet_id) {
                    if let Some(s) = args.first().and_then(|a| a.as_str()) { ad.fleet_name = s.to_string(); }
                    if let Some(s) = args.get(1).and_then(|a| a.as_str()) { ad.description = s.to_string(); }
                    if let Some(s) = args.get(2).and_then(|a| a.as_i64()) { ad.invite_scope = s as u8; }
                    return Some(Ok(PyRep::keyval([
                        ("fleetID", PyRep::Integer(fleet_id)),
                        ("fleetName", PyRep::String(ad.fleet_name.clone())),
                        ("description", PyRep::String(ad.description.clone())),
                    ])));
                }
            }
            let (fleet_name, description, invite_scope, auto_join, min_standing, min_security) =
                if let Some(advert_obj) = args.first().filter(|a| a.dict_get("fleetName").is_some() || a.dict_get("inviteScope").is_some() || a.dict_get("joinNeedsApproval").is_some()).or_else(|| kwargs.get("advertKeyVal").or_else(|| kwargs.get("advert"))) {
                    let fn_name = advert_obj.dict_get("fleetName").and_then(|a| a.as_str()).unwrap_or("Public Standing Fleet").to_string();
                    let desc = advert_obj.dict_get("description").and_then(|a| a.as_str()).unwrap_or("").to_string();
                    let scope = advert_obj.dict_get("inviteScope").and_then(|a| a.as_i64()).unwrap_or(3) as u8;
                    let needs_appr = advert_obj.dict_get("joinNeedsApproval").and_then(|a| match a { PyRep::Bool(b) => Some(*b), PyRep::Integer(i) => Some(*i != 0), _ => None }).unwrap_or(false);
                    let min_st = advert_obj.dict_get("minStanding").and_then(|a| a.as_f64()).unwrap_or(-10.0);
                    let min_sec = advert_obj.dict_get("minSecurity").and_then(|a| a.as_f64()).unwrap_or(-10.0);
                    (fn_name, desc, scope, !needs_appr, min_st, min_sec)
                } else {
                    let fn_name = args.first().and_then(|a| a.as_str()).unwrap_or("Public Standing Fleet").to_string();
                    let desc = args.get(1).and_then(|a| a.as_str()).unwrap_or("Open to corp/alliance/public").to_string();
                    let scope = args.get(2).and_then(|a| a.as_i64()).unwrap_or(3) as u8;
                    let aj = args.get(3).and_then(|a| match a { PyRep::Bool(b) => Some(*b), PyRep::Integer(i) => Some(*i != 0), _ => None }).unwrap_or(true);
                    let min_st = args.get(4).and_then(|a| a.as_f64()).unwrap_or(-10.0);
                    let min_sec = args.get(5).and_then(|a| a.as_f64()).unwrap_or(-10.0);
                    (fn_name, desc, scope, aj, min_st, min_sec)
                };
            let now_unix = ctx.now_unix();
            let sys_id = session.solar_system_id.unwrap_or(30_000_142);
            ctx.db.with_mut_snapshot(|snap| {
                if let Some(f) = snap.fleets.get_mut(&fleet_id) { f.is_registered = true; }
            });
            ctx.fleet_extras.lock().unwrap().adverts.insert(
                fleet_id,
                FleetAdvertState {
                    fleet_id, leader_id: char_id, fleet_name: fleet_name.clone(), description: description.clone(),
                    solar_system_id: sys_id, invite_scope, auto_join, min_standing, min_security,
                    allowed_entities: HashSet::new(), _created_unix: now_unix,
                },
            );
            ctx.notify(NotificationTarget::Fleet(fleet_id), "OnFleetFinderAdvertChanged", "fleetid", vec![PyRep::Integer(fleet_id), PyRep::String(fleet_name.clone())]);
            Some(Ok(PyRep::keyval([
                ("fleetID", PyRep::Integer(fleet_id)),
                ("leaderID", PyRep::Integer(char_id)),
                ("fleetName", PyRep::String(fleet_name)),
                ("description", PyRep::String(description)),
                ("inviteScope", PyRep::Integer(invite_scope as i64)),
                ("autoJoin", PyRep::Bool(auto_join)),
                ("minStanding", PyRep::Real(min_standing)),
                ("minSecurity", PyRep::Real(min_security)),
            ])))
        }

        ("fleetProxy", "GetAvailableFleetAds") | ("fleetProxy", "GetMyFleetFinderAdvert") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let caller_fleet = session.fleet_id;
            let ads: Vec<FleetAdvertState> = ctx.fleet_extras.lock().unwrap().adverts.values().cloned().collect();
            let rows = ctx.db.with_snapshot(|snap| {
                let caller = snap.characters.get(&char_id);
                let caller_corp = caller.map(|c| c.corporation_id).unwrap_or(0);
                let caller_ally = caller.and_then(|c| c.alliance_id).unwrap_or(0);
                let caller_sec = caller.map(|c| c.security_status).unwrap_or(0.0);
                ads.into_iter()
                    .filter(|ad| {
                        if method == "GetMyFleetFinderAdvert" {
                            return caller_fleet == Some(ad.fleet_id) || ad.leader_id == char_id;
                        }
                        if caller_sec < ad.min_security { return false; }
                        if !ad.allowed_entities.is_empty()
                            && !ad.allowed_entities.contains(&char_id)
                            && !ad.allowed_entities.contains(&caller_corp)
                            && (caller_ally == 0 || !ad.allowed_entities.contains(&caller_ally))
                        {
                            return false;
                        }
                        let leader = snap.characters.get(&ad.leader_id);
                        let leader_corp = leader.map(|c| c.corporation_id).unwrap_or(0);
                        let leader_ally = leader.and_then(|c| c.alliance_id).unwrap_or(0);
                        if ad.min_standing > -10.0 {
                            let standing = snap.corp_contacts.get(&leader_corp).and_then(|list| {
                                list.iter().find(|c| c.contact_id == char_id).or_else(|| list.iter().find(|c| c.contact_id == caller_corp)).map(|c| c.standing)
                            }).unwrap_or(0.0);
                            if standing < ad.min_standing { return false; }
                        }
                        match ad.invite_scope {
                            1 => caller_corp == leader_corp,
                            2 => caller_corp == leader_corp || (caller_ally != 0 && caller_ally == leader_ally),
                            _ => true,
                        }
                    })
                    .map(|ad| {
                        let num_members = snap.fleets.get(&ad.fleet_id).map(|f| f.members.len() as i64).unwrap_or(1);
                        vec![
                            PyRep::Integer(ad.fleet_id),
                            PyRep::Integer(ad.leader_id),
                            PyRep::String(ad.fleet_name),
                            PyRep::String(ad.description),
                            PyRep::Integer(ad.solar_system_id as i64),
                            PyRep::Integer(ad.invite_scope as i64),
                            PyRep::Bool(ad.auto_join),
                            PyRep::Integer(num_members),
                        ]
                    })
                    .collect::<Vec<_>>()
            });
            Some(Ok(PyRep::rowset(
                vec!["fleetID", "leaderID", "fleetName", "description", "solarSystemID", "inviteScope", "autoJoin", "numMembers"],
                rows,
            )))
        }

        ("fleetProxy", "RemoveFleetFinderAdvert") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let Some(fleet_id) = session.fleet_id else {
                return Some(Err(BridgeRpcError { code: "NotInFleet".into(), message: "Character is not in a fleet".into() }));
            };
            let res = ctx.db.try_with_mut_snapshot(|snap| -> Result<(), BridgeRpcError> {
                let Some(f) = snap.fleets.get_mut(&fleet_id) else {
                    return Err(BridgeRpcError { code: "FleetNotFound".into(), message: format!("Fleet {fleet_id} not found") });
                };
                if !f.member_details.contains_key(&char_id) {
                    return Err(BridgeRpcError { code: "NotInFleet".into(), message: "Caller is not in fleet".into() });
                }
                if f.boss_id != char_id {
                    return Err(BridgeRpcError { code: "FleetPermissionDenied".into(), message: "Only Fleet Boss can remove fleet finder advert".into() });
                }
                f.is_registered = false;
                Ok(())
            });
            match res {
                Ok(()) => {
                    ctx.fleet_extras.lock().unwrap().adverts.remove(&fleet_id);
                    Some(Ok(PyRep::keyval([("fleetID", PyRep::Integer(fleet_id)), ("removed", PyRep::Bool(true))])))
                }
                Err(e) => {
                    if e.code == "NotInFleet" || e.code == "FleetNotFound" { session.fleet_id = None; }
                    Some(Err(e))
                }
            }
        }

        ("fleetProxy", "ApplyToJoinFleet") => {
            // parity: feat:fleet.join-requests
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            if notify::character_active_fleet_id(ctx, char_id).is_some() {
                return Some(Err(BridgeRpcError { code: "AlreadyInFleet".into(), message: "Character is already in a fleet".into() }));
            }
            let Some(fleet_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError { code: "InvalidArgument".into(), message: "ApplyToJoinFleet requires fleetID".into() }));
            };
            let (advert, auto_join, max_size, auto_squad) = {
                let ex = ctx.fleet_extras.lock().unwrap();
                let ad = ex.adverts.get(&fleet_id).cloned();
                let aj = ad.as_ref().map(|a| a.auto_join).unwrap_or(false);
                let ms = ex.max_sizes.get(&fleet_id).copied().unwrap_or(256);
                let asq = ex.auto_join_squads.get(&fleet_id).copied().flatten();
                (ad, aj, ms, asq)
            };
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                if !snap.fleets.contains_key(&fleet_id) {
                    return Err(BridgeRpcError { code: "FleetNotFound".into(), message: format!("Fleet {fleet_id} not found") });
                }
                let Some(ad) = &advert else {
                    return Err(BridgeRpcError { code: "FleetAdvertNotFound".into(), message: "Fleet has no active advert".into() });
                };
                {
                    let caller = snap.characters.get(&char_id);
                    let caller_corp = caller.map(|c| c.corporation_id).unwrap_or(0);
                    let caller_ally = caller.and_then(|c| c.alliance_id).unwrap_or(0);
                    let caller_sec = caller.map(|c| c.security_status).unwrap_or(0.0);
                    if caller_sec < ad.min_security {
                        return Err(BridgeRpcError { code: "FleetJoinDenied".into(), message: "Security status below fleet minimum".into() });
                    }
                    if !ad.allowed_entities.is_empty()
                        && !ad.allowed_entities.contains(&char_id)
                        && !ad.allowed_entities.contains(&caller_corp)
                        && (caller_ally == 0 || !ad.allowed_entities.contains(&caller_ally))
                    {
                        return Err(BridgeRpcError { code: "FleetJoinDenied".into(), message: "Not in allowed entities for fleet advert".into() });
                    }
                    let leader = snap.characters.get(&ad.leader_id);
                    let leader_corp = leader.map(|c| c.corporation_id).unwrap_or(0);
                    let leader_ally = leader.and_then(|c| c.alliance_id).unwrap_or(0);
                    if ad.min_standing > -10.0 {
                        let standing = snap.corp_contacts.get(&leader_corp).and_then(|list| {
                            list.iter().find(|c| c.contact_id == char_id).or_else(|| list.iter().find(|c| c.contact_id == caller_corp)).map(|c| c.standing)
                        }).unwrap_or(0.0);
                        if standing < ad.min_standing {
                            return Err(BridgeRpcError { code: "FleetJoinDenied".into(), message: "Standing below fleet minimum".into() });
                        }
                    }
                    let scope_ok = match ad.invite_scope {
                        1 => caller_corp == leader_corp,
                        2 => caller_corp == leader_corp || (caller_ally != 0 && caller_ally == leader_ally),
                        _ => true,
                    };
                    if !scope_ok {
                        return Err(BridgeRpcError { code: "FleetJoinDenied".into(), message: "Outside fleet advert scope".into() });
                    }
                }
                let Some(fleet) = snap.fleets.get_mut(&fleet_id) else {
                    return Err(BridgeRpcError { code: "FleetNotFound".into(), message: format!("Fleet {fleet_id} not found") });
                };
                if fleet.members.len() >= max_size as usize {
                    return Err(BridgeRpcError { code: "FleetFull".into(), message: "Fleet is full".into() });
                }
                let wid = fleet.wings.first().copied().unwrap_or(1);
                let sid = auto_squad
                    .or_else(|| fleet.wing_details.get(&wid).and_then(|w| w.squad_ids.first().copied()))
                    .or_else(|| fleet.squads.first().copied())
                    .unwrap_or(1);
                let boss_id = fleet.boss_id;
                let motd = fleet.motd.clone();
                if auto_join {
                    if !fleet.members.contains(&char_id) { fleet.members.push(char_id); }
                    fleet.member_details.insert(
                        char_id,
                        FleetMemberRecord {
                            character_id: char_id, role: FleetRole::SquadMember, wing_id: Some(wid), squad_id: Some(sid),
                            takes_fleet_warp: true, accepts_regroup: true, accepts_conduit_jumps: true, job: 0,
                            role_name: "Squad Member".into(), join_time_unix: ctx.now_unix(),
                        },
                    );
                    ensure_fleet_channel_member(&mut snap.chat_channels, fleet_id, boss_id, char_id);
                    if let Some(ch) = snap.characters.get_mut(&char_id) { ch.fleet_id = Some(fleet_id); }
                    Ok((true, wid, sid, boss_id, motd))
                } else {
                    Ok((false, wid, sid, boss_id, motd))
                }
            });
            match res {
                Ok((joined, wid, sid, boss_id, motd)) => {
                    if joined {
                        session.fleet_id = Some(fleet_id);
                        notify::emit_join(ctx, fleet_id, char_id);
                        if !motd.is_empty() {
                            notify::emit_motd(ctx, NotificationTarget::Character(char_id), &motd, false);
                        }
                    } else {
                        ctx.fleet_extras.lock().unwrap().join_requests.entry(fleet_id).or_default().insert(char_id);
                        notify::sync_fleet_join_requests_to_db(ctx);
                        let corp_id = ctx.db.with_snapshot(|snap| snap.characters.get(&char_id).map(|c| c.corporation_id));
                        let info = PyRep::keyval([("charID", PyRep::Integer(char_id)), ("corpID", corp_id.map(PyRep::Integer).unwrap_or(PyRep::None))]);
                        ctx.notify(NotificationTarget::Character(boss_id), "OnFleetJoinRequest", "charid", vec![info]);
                    }
                    Some(Ok(PyRep::keyval([
                        ("fleetID", PyRep::Integer(fleet_id)),
                        ("joined", PyRep::Bool(joined)),
                        ("wingID", PyRep::Integer(wid)),
                        ("squadID", PyRep::Integer(sid)),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("fleetObjectHandler", "GetJoinRequests")
        | ("fleetMgr", "GetJoinRequests")
        | ("fleetProxy", "GetJoinRequests") => {
            // parity: feat:fleet.join-requests
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let Some(fleet_id) = session.fleet_id else {
                return Some(Err(BridgeRpcError { code: "NotInFleet".into(), message: "Character is not in a fleet".into() }));
            };
            let res = ctx.db.with_snapshot(|snap| {
                let Some(f) = snap.fleets.get(&fleet_id) else {
                    return Err(BridgeRpcError { code: "FleetNotFound".into(), message: format!("Fleet {fleet_id} not found") });
                };
                let Some(caller_m) = f.member_details.get(&char_id) else {
                    return Err(BridgeRpcError { code: "NotInFleet".into(), message: "Caller is not in fleet".into() });
                };
                if f.boss_id != char_id && role_to_i64(caller_m.role) > 3 {
                    return Err(BridgeRpcError { code: "FleetPermissionDenied".into(), message: "Only Fleet Boss or a Commander can view join requests".into() });
                }
                Ok(())
            });
            match res {
                Ok(()) => {
                    let applicants = notify::get_fleet_join_requests(ctx, fleet_id);
                    let dict: Vec<(PyRep, PyRep)> = applicants.into_iter().map(|aid| (PyRep::Integer(aid), PyRep::keyval([("charID", PyRep::Integer(aid))]))).collect();
                    Some(Ok(PyRep::Dict(dict)))
                }
                Err(e) => {
                    if e.code == "NotInFleet" || e.code == "FleetNotFound" { session.fleet_id = None; }
                    Some(Err(e))
                }
            }
        }

        ("fleetObjectHandler", "AcceptJoinRequest")
        | ("fleetMgr", "AcceptJoinRequest")
        | ("fleetProxy", "AcceptJoinRequest") => {
            // parity: feat:fleet.join-requests
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let Some(fleet_id) = session.fleet_id else {
                return Some(Err(BridgeRpcError { code: "NotInFleet".into(), message: "Character is not in a fleet".into() }));
            };
            let applicant_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let wing_opt = args.get(1).and_then(|a| a.as_i64());
            let squad_opt = args.get(2).and_then(|a| a.as_i64());
            let role = i64_to_role(args.get(3).and_then(|a| a.as_i64()).unwrap_or(4));
            let _ = notify::get_fleet_join_requests(ctx, fleet_id);
            let has_req = ctx.fleet_extras.lock().unwrap().join_requests.get(&fleet_id).is_some_and(|s| s.contains(&applicant_id));
            if !has_req {
                return Some(Err(BridgeRpcError { code: "FleetJoinRequestNotFound".into(), message: format!("No pending join request from {applicant_id}") }));
            }
            let (max_size, auto_squad) = {
                let ex = ctx.fleet_extras.lock().unwrap();
                (ex.max_sizes.get(&fleet_id).copied().unwrap_or(256), ex.auto_join_squads.get(&fleet_id).copied().flatten())
            };
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(f) = snap.fleets.get_mut(&fleet_id) else {
                    return Err(BridgeRpcError { code: "FleetNotFound".into(), message: format!("Fleet {fleet_id} not found") });
                };
                let Some(caller_m) = f.member_details.get(&char_id) else {
                    return Err(BridgeRpcError { code: "NotInFleet".into(), message: "Caller is not in fleet".into() });
                };
                if f.boss_id != char_id && (role_to_i64(caller_m.role) > 3 || role_to_i64(caller_m.role) >= role_to_i64(role)) {
                    return Err(BridgeRpcError { code: "FleetPermissionDenied".into(), message: "Only Fleet Boss or a higher-ranking Commander can accept join requests to this role".into() });
                }
                if f.members.len() >= max_size as usize {
                    return Err(BridgeRpcError { code: "FleetFull".into(), message: "Fleet is full".into() });
                }
                let wid = wing_opt.unwrap_or_else(|| f.wings.first().copied().unwrap_or(1));
                let sid = squad_opt.or(auto_squad).or_else(|| f.wing_details.get(&wid).and_then(|w| w.squad_ids.first().copied())).or_else(|| f.squads.first().copied()).unwrap_or(1);
                let pos_occupied = match role {
                    FleetRole::FleetCommander => f.member_details.values().any(|m| m.role == FleetRole::FleetCommander),
                    FleetRole::WingCommander => f.wing_details.get(&wid).and_then(|w| w.commander_id).is_some()
                        || f.member_details.values().any(|m| m.role == FleetRole::WingCommander && m.wing_id == Some(wid)),
                    FleetRole::SquadCommander => f.squad_details.get(&sid).and_then(|s| s.commander_id).is_some()
                        || f.member_details.values().any(|m| m.role == FleetRole::SquadCommander && m.squad_id == Some(sid)),
                    FleetRole::SquadMember => false,
                };
                if pos_occupied {
                    return Err(BridgeRpcError { code: "PositionOccupied".into(), message: "The requested fleet commander position is already occupied".into() });
                }
                if role == FleetRole::WingCommander {
                    if let Some(wing) = f.wing_details.get_mut(&wid) { wing.commander_id = Some(applicant_id); }
                } else if role == FleetRole::SquadCommander {
                    if let Some(sq) = f.squad_details.get_mut(&sid) { sq.commander_id = Some(applicant_id); }
                }
                if !f.members.contains(&applicant_id) { f.members.push(applicant_id); }
                f.invites.remove(&applicant_id);
                f.member_details.insert(
                    applicant_id,
                    FleetMemberRecord {
                        character_id: applicant_id, role, wing_id: Some(wid), squad_id: Some(sid),
                        takes_fleet_warp: true, accepts_regroup: true, accepts_conduit_jumps: true, job: 0,
                        role_name: role_name_str(role).into(), join_time_unix: ctx.now_unix(),
                    },
                );
                let boss_id = f.boss_id;
                let motd = f.motd.clone();
                ensure_fleet_channel_member(&mut snap.chat_channels, fleet_id, boss_id, applicant_id);
                if let Some(ch) = snap.characters.get_mut(&applicant_id) { ch.fleet_id = Some(fleet_id); }
                Ok((wid, sid, motd))
            });
            match res {
                Ok((wid, sid, motd)) => {
                    let remaining_requests = {
                        let mut ex = ctx.fleet_extras.lock().unwrap();
                        if let Some(set) = ex.join_requests.get_mut(&fleet_id) { set.remove(&applicant_id); }
                        ex.invite_details.remove(&(fleet_id, applicant_id));
                        ex.invite_timestamps.remove(&(fleet_id, applicant_id));
                        ex.join_requests.get(&fleet_id).map(|set| {
                            set.iter().map(|&cid| (PyRep::Integer(cid), PyRep::keyval([("charID", PyRep::Integer(cid))]))).collect::<Vec<_>>()
                        }).unwrap_or_default()
                    };
                    notify::sync_fleet_join_requests_to_db(ctx);
                    notify::emit_join(ctx, fleet_id, applicant_id);
                    if !motd.is_empty() {
                        notify::emit_motd(ctx, NotificationTarget::Character(applicant_id), &motd, false);
                    }
                    ctx.notify(NotificationTarget::Character(char_id), "OnJoinRequestUpdate", "charid", vec![PyRep::Dict(remaining_requests)]);
                    Some(Ok(PyRep::keyval([
                        ("fleetID", PyRep::Integer(fleet_id)),
                        ("characterID", PyRep::Integer(applicant_id)),
                        ("approved", PyRep::Bool(true)),
                        ("wingID", PyRep::Integer(wid)),
                        ("squadID", PyRep::Integer(sid)),
                        ("role", PyRep::Integer(role_to_i64(role))),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("fleetObjectHandler", "RejectJoinRequest")
        | ("fleetMgr", "RejectJoinRequest")
        | ("fleetProxy", "RejectJoinRequest") => {
            // parity: feat:fleet.join-requests
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let fleet_id = session.fleet_id.unwrap_or(0);
            let applicant_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let is_authorized = ctx.db.with_snapshot(|snap| {
                snap.fleets.get(&fleet_id).is_some_and(|f| {
                    f.boss_id == char_id || f.member_details.get(&char_id).is_some_and(|m| role_to_i64(m.role) <= 3)
                })
            });
            if !is_authorized {
                return Some(Err(BridgeRpcError { code: "FleetPermissionDenied".into(), message: "Only the Fleet Boss or a Commander can reject join requests".into() }));
            }
            let _ = notify::get_fleet_join_requests(ctx, fleet_id);
            let removed = ctx.fleet_extras.lock().unwrap().join_requests.get_mut(&fleet_id).map(|set| set.remove(&applicant_id)).unwrap_or(false);
            if !removed {
                return Some(Err(BridgeRpcError { code: "FleetJoinRequestNotFound".into(), message: format!("No pending join request from {applicant_id}") }));
            }
            {
                let mut ex = ctx.fleet_extras.lock().unwrap();
                ex.invite_details.remove(&(fleet_id, applicant_id));
                ex.invite_timestamps.remove(&(fleet_id, applicant_id));
            }
            notify::sync_fleet_join_requests_to_db(ctx);
            ctx.notify(NotificationTarget::Character(applicant_id), "OnFleetJoinRejected", "charid", vec![PyRep::Integer(char_id)]);
            let remaining_requests = {
                let ex = ctx.fleet_extras.lock().unwrap();
                ex.join_requests.get(&fleet_id).map(|set| {
                    set.iter().map(|&cid| (PyRep::Integer(cid), PyRep::keyval([("charID", PyRep::Integer(cid))]))).collect::<Vec<_>>()
                }).unwrap_or_default()
            };
            ctx.notify(NotificationTarget::Character(char_id), "OnJoinRequestUpdate", "charid", vec![PyRep::Dict(remaining_requests)]);
            Some(Ok(PyRep::keyval([("characterID", PyRep::Integer(applicant_id)), ("rejected", PyRep::Bool(true))])))
        }

        // rpc-lint: allow auditor acceptance text feat:fleet.target-tagging
        ("fleetProxy", "SetTargetTag")
        // rpc-lint: allow auditor acceptance text feat:fleet.target-tagging
        | ("fleetObjectHandler", "SetTargetTag")
        // rpc-lint: allow auditor acceptance text feat:fleet.target-tagging
        | ("fleetMgr", "SetTargetTag")
        // rpc-lint: allow auditor acceptance text feat:fleet.target-tagging
        | ("fleetObjectHandler", "TagTarget")
        // rpc-lint: allow auditor acceptance text feat:fleet.target-tagging
        | ("fleetMgr", "TagTarget")
        // rpc-lint: allow auditor acceptance text feat:fleet.target-tagging
        | ("fleetObjectHandler", "ClearTargetTags") => {
            // parity: feat:fleet.target-tagging
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let Some(fleet_id) = session.fleet_id else {
                return Some(Err(BridgeRpcError { code: "NotInFleet".into(), message: "Character is not in a fleet".into() }));
            };
            let is_lead = ctx.db.with_snapshot(|snap| {
                snap.fleets.get(&fleet_id).is_some_and(|f| {
                    f.boss_id == char_id || f.member_details.get(&char_id).is_some_and(|m| role_to_i64(m.role) <= 3)
                })
            });
            if !is_lead {
                return Some(Err(BridgeRpcError { code: "FleetRoleDenied".into(), message: "Only fleet commanders can tag targets".into() }));
            }
            if method == "ClearTargetTags" {
                ctx.fleet_extras.lock().unwrap().target_tags.remove(&fleet_id);
                let fleet_state = notify::build_fleet_state_keyval(ctx, Some(fleet_id), char_id, 1.0, 1.0, 1.0);
                ctx.notify(NotificationTarget::Fleet(fleet_id), "OnFleetStateChange", "fleetid", vec![fleet_state]);
                return Some(Ok(PyRep::keyval([("fleetID", PyRep::Integer(fleet_id)), ("targetTags", PyRep::Dict(Vec::new()))])));
            }
            let Some(target_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError { code: "InvalidArgument".into(), message: "SetTargetTag requires targetID".into() }));
            };
            let tag_str = args.get(1).and_then(|a| match a { PyRep::String(s) => Some(s.trim().to_string()), PyRep::Integer(i) => Some(i.to_string()), _ => None }).unwrap_or_default();
            let tags_dict = {
                let mut ex = ctx.fleet_extras.lock().unwrap();
                let tags = ex.target_tags.entry(fleet_id).or_default();
                if tag_str.is_empty() {
                    tags.remove(&target_id);
                } else {
                    tags.retain(|&tid, existing| tid == target_id || existing != &tag_str);
                    tags.insert(target_id, tag_str.clone());
                }
                tags.iter().map(|(&tid, t)| (PyRep::Integer(tid), PyRep::String(t.clone()))).collect::<Vec<_>>()
            };
            let fleet_state = notify::build_fleet_state_keyval(ctx, Some(fleet_id), char_id, 1.0, 1.0, 1.0);
            ctx.notify(NotificationTarget::Fleet(fleet_id), "OnFleetStateChange", "fleetid", vec![fleet_state]);
            let tag_rep = if tag_str.is_empty() { PyRep::None } else { PyRep::String(tag_str) };
            // rpc-lint: allow auditor acceptance text feat:fleet.target-tagging
            ctx.notify(NotificationTarget::Fleet(fleet_id), "OnFleetTargetTagChanged", "fleetid", vec![PyRep::Integer(fleet_id), PyRep::Integer(target_id), tag_rep.clone()]);
            Some(Ok(PyRep::keyval([
                ("fleetID", PyRep::Integer(fleet_id)),
                ("targetID", PyRep::Integer(target_id)),
                ("tag", tag_rep),
                ("targetTags", PyRep::Dict(tags_dict)),
            ])))
        }

        // rpc-lint: allow auditor acceptance text feat:fleet.target-tagging
        ("fleetObjectHandler", "GetTargetTags") | ("fleetMgr", "GetTargetTags") => {
            let (_, _char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let Some(fleet_id) = session.fleet_id else {
                return Some(Err(BridgeRpcError { code: "NotInFleet".into(), message: "Character is not in a fleet".into() }));
            };
            let tags_dict = ctx.fleet_extras.lock().unwrap().target_tags.get(&fleet_id)
                .map(|m| m.iter().map(|(&tid, t)| (PyRep::Integer(tid), PyRep::String(t.clone()))).collect::<Vec<_>>())
                .unwrap_or_default();
            Some(Ok(PyRep::Dict(tags_dict)))
        }

        // rpc-lint: allow auditor acceptance text feat:fleet.stored-formations-and-warp-shapes
        ("fleetObjectHandler", "SetFleetFormation")
        // rpc-lint: allow auditor acceptance text feat:fleet.stored-formations-and-warp-shapes
        | ("fleetMgr", "SetFleetFormation")
        // rpc-lint: allow auditor acceptance text feat:fleet.stored-formations-and-warp-shapes
        | ("fleetObjectHandler", "SaveFleetFormation") => {
            // parity: feat:fleet.stored-formations-and-warp-shapes
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            ensure_fleet_formations_loaded(ctx);
            let formation_id = args.first().and_then(|a| a.as_i64()).or_else(|| kwargs.get("formationID").and_then(|a| a.as_i64())).unwrap_or(0).clamp(0, 5);
            let default_spacing = match formation_id { 1 | 3 | 4 => 5_000.0, 2 => 10_000.0, _ => 0.0 };
            let spacing = args.get(1).and_then(|a| a.as_f64()).or_else(|| kwargs.get("formationSpacing").and_then(|a| a.as_f64())).unwrap_or(default_spacing).clamp(0.0, 100_000.0);
            let size = args.get(2).and_then(|a| a.as_f64()).or_else(|| kwargs.get("formationSize").and_then(|a| a.as_f64())).unwrap_or(spacing.max(10_000.0)).clamp(0.0, 100_000.0);
            ctx.fleet_extras.lock().unwrap().stored_formations.insert(char_id, (formation_id, spacing, size));
            sync_fleet_formations_to_db(ctx);
            Some(Ok(PyRep::keyval([
                ("formationID", PyRep::Integer(formation_id)),
                ("formationName", PyRep::String(formation_name(formation_id).into())),
                ("formationSpacing", PyRep::Real(spacing)),
                ("formationSize", PyRep::Real(size)),
            ])))
        }

        // rpc-lint: allow auditor acceptance text feat:fleet.stored-formations-and-warp-shapes
        ("fleetObjectHandler", "GetFleetFormation") | ("fleetMgr", "GetFleetFormation") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            ensure_fleet_formations_loaded(ctx);
            let (fid, spacing, size) = ctx.fleet_extras.lock().unwrap().stored_formations.get(&char_id).copied().unwrap_or((0, 0.0, 10_000.0));
            Some(Ok(PyRep::keyval([
                ("formationID", PyRep::Integer(fid)),
                ("formationName", PyRep::String(formation_name(fid).into())),
                ("formationSpacing", PyRep::Real(spacing)),
                ("formationSize", PyRep::Real(size)),
            ])))
        }

        ("fleetObjectHandler", "GetFleetID") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let fleet_id = notify::character_active_fleet_id(ctx, char_id);
            session.fleet_id = fleet_id;
            Some(Ok(fleet_id.map(PyRep::Integer).unwrap_or(PyRep::None)))
        }

        ("fleetObjectHandler", "Reconnect") | ("fleetObjectHandler", "UpdateMemberInfo") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let fleet_id = notify::character_active_fleet_id(ctx, char_id);
            session.fleet_id = fleet_id;
            let (wid, sid, role) = ctx.db.with_snapshot(|snap| {
                fleet_id
                    .and_then(|fid| snap.fleets.get(&fid))
                    .and_then(|f| f.member_details.get(&char_id))
                    .map(|m| (m.wing_id.unwrap_or(1), m.squad_id.unwrap_or(1), role_to_i64(m.role)))
                    .unwrap_or((1, 1, 4))
            });
            Some(Ok(PyRep::keyval([
                ("characterID", PyRep::Integer(char_id)),
                ("fleetID", fleet_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
                ("wingID", PyRep::Integer(wid)),
                ("squadID", PyRep::Integer(sid)),
                ("role", PyRep::Integer(role)),
            ])))
        }

        _ => None,
    }
}
