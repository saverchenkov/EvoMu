//! `fitting_scanners_superweapons` — Titan Doomsdays (`superWeaponMgr`), Command Bursts,
//! Dynamic Buffs (`dbuff`), System-Wide Effects, Data/Relic Analyzers, Cargo/Ship Scanners,
//! Entosis Links & Clone Vat Bay (`jumpCloneSvc`).

#![forbid(unsafe_code)]

use super::*;
use std::sync::{OnceLock, RwLock};

#[derive(Debug, Clone)]
pub(crate) struct ActiveDbuffEntry {
    pub dbuff_id: u32,
    pub value: f64,
    pub expires_at_unix: u64,
    pub source_module_id: i64,
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub(crate) struct HackingSessionState {
    pub container_id: i64,
    pub analyzer_module_id: i64,
    pub virus_coherence: f64,
    pub virus_strength: f64,
    pub core_hp_remaining: f64,
    pub unlocked: bool,
}

#[derive(Default)]
pub(crate) struct ScannersSuperweaponsState {
    /// Active dbuffs per `(ServerContext::instance_id, target ship_id)` (process-wide state, so
    /// keyed by context instance to keep separate server contexts apart).
    pub ship_dbuffs: HashMap<(u64, i64), Vec<ActiveDbuffEntry>>,
    /// Active system-wide effect beacon `type_id` per `solar_system_id`
    pub system_wide_effects: HashMap<u32, u32>,
    /// Active hacking minigame state per target `container_id`
    pub hacking_states: HashMap<i64, HackingSessionState>,
    /// Contested sovereignty structure/node state per `target_id`
    pub entosis_contested_nodes: HashMap<i64, i64>,
    /// Pending ship clone installation offers: `target_char_id -> (offering_char_id, host_ship_id)`
    pub ship_clone_offers: HashMap<i64, (i64, i64)>,
}

pub(crate) fn scanners_superweapons_state() -> &'static RwLock<ScannersSuperweaponsState> {
    static STATE: OnceLock<RwLock<ScannersSuperweaponsState>> = OnceLock::new();
    STATE.get_or_init(|| RwLock::new(ScannersSuperweaponsState::default()))
}

/// Computes aggregated active `dbuff` values for `ship_id` at `now_unix`, pruning expired entries.
pub(crate) fn get_aggregated_ship_dbuffs(
    ctx: &ServerContext,
    ship_id: i64,
    now_unix: u64,
) -> HashMap<u32, (f64, u64)> {
    let mut by_id: HashMap<u32, (Vec<f64>, u64)> = HashMap::new();
    {
        let mut st = scanners_superweapons_state()
            .write()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(entries) = st.ship_dbuffs.get_mut(&(ctx.instance_id, ship_id)) {
            entries.retain(|e| e.expires_at_unix > now_unix);
            for e in entries.iter() {
                let slot = by_id.entry(e.dbuff_id).or_insert_with(|| (Vec::new(), 0));
                slot.0.push(e.value);
                slot.1 = slot.1.max(e.expires_at_unix);
            }
        }
    }
    // Include system-wide weather/storm (`group_id == 1975` / `920`) `warfareBuff*` dbuffs in the ship's solar system
    let sys_tids: Vec<u32> = ctx.db.with_snapshot(|snap| {
        let Some(ship) = snap.items.get(&ship_id) else {
            return Vec::new();
        };
        let Some(sys_id) = u32::try_from(ship.location_id)
            .ok()
            .filter(|s| ctx.sde.solar_systems.contains_key(s))
        else {
            return Vec::new();
        };
        let mut tids: Vec<u32> = snap
            .items
            .values()
            .filter(|i| {
                i.location_id == sys_id as i64
                    && ctx
                        .sde
                        .types
                        .get(&i.type_id)
                        .is_some_and(|t| matches!(t.group_id, 920 | 1975))
            })
            .map(|i| i.type_id)
            .collect();
        if let Some(&eff_tid) = scanners_superweapons_state()
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .system_wide_effects
            .get(&sys_id)
        {
            if !tids.contains(&eff_tid) {
                tids.push(eff_tid);
            }
        }
        tids
    });
    for tid in sys_tids {
        if let Some(t) = ctx.sde.types.get(&tid) {
            for (id_attr, val_attr) in [(2468u16, 2469u16), (2470, 2471), (2472, 2473), (2536, 2537)] {
                let dbuff_id = t.default_attributes.get(&id_attr).copied().unwrap_or(0.0) as u32;
                let val = t.default_attributes.get(&val_attr).copied().unwrap_or(0.0);
                if dbuff_id > 0 && val.abs() > 1e-9 {
                    let slot = by_id.entry(dbuff_id).or_insert_with(|| (Vec::new(), now_unix + 3600));
                    slot.0.push(val);
                    slot.1 = slot.1.max(now_unix + 3600);
                }
            }
        }
    }
    let mut out = HashMap::with_capacity(by_id.len());
    for (dbuff_id, (vals, max_exp)) in by_id {
        let mode = ctx
            .sde
            .dbuff_collections
            .get(&dbuff_id)
            .map(|c| c.aggregate_mode.as_str())
            .unwrap_or("Maximum");
        let agg = eve_dogma::aggregate_dbuff_values(mode, &vals);
        out.insert(dbuff_id, (agg, max_exp));
    }
    out
}

/// Applies active `dbuff` modifiers and system-wide wormhole/storm modifiers to a ship's attribute map.
pub(crate) fn apply_active_dbuffs_and_system_effects_to_ship_attrs(
    ctx: &ServerContext,
    ship_id: i64,
    attrs: &mut HashMap<u16, f64>,
) {
    let now_unix = ctx.now_unix_secs();
    let agg_dbuffs = get_aggregated_ship_dbuffs(ctx, ship_id, now_unix);
    for (dbuff_id, (val, _)) in agg_dbuffs {
        if let Some(coll) = ctx.sde.dbuff_collections.get(&dbuff_id) {
            let mut target_aids = coll.item_modifiers.clone();
            for &aid in &coll.location_modifiers {
                if !target_aids.contains(&aid) {
                    target_aids.push(aid);
                }
            }
            for &aid in &target_aids {
                let base = attrs.get(&aid).copied().unwrap_or(1.0);
                let modified = match coll.operation.as_str() {
                    "PostMul" => base * val,
                    "ModAdd" => base + val,
                    "PostAssignment" => val,
                    _ => base * (1.0 + val / 100.0), // PostPercent
                };
                attrs.insert(aid, modified);
            }
        }
    }

    let sys_id_opt = ctx.db.with_snapshot(|snap| {
        snap.items
            .get(&ship_id)
            .and_then(|i| {
                u32::try_from(i.location_id)
                    .ok()
                    .filter(|s| ctx.sde.solar_systems.contains_key(s))
            })
            .or_else(|| {
                let owner_id = snap.items.get(&ship_id).map(|i| i.owner_id)?;
                snap.characters.get(&owner_id).map(|c| c.solar_system_id)
            })
    });
    if let Some(sys_id) = sys_id_opt {
        let eff_tid_opt = scanners_superweapons_state()
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .system_wide_effects
            .get(&sys_id)
            .copied()
            .or_else(|| {
                ctx.db.with_snapshot(|snap| {
                    snap.items
                        .values()
                        .find(|i| {
                            i.location_id == sys_id as i64
                                && ctx.sde.types.get(&i.type_id).is_some_and(|t| t.group_id == 920)
                        })
                        .map(|i| i.type_id)
                })
            });
        if let Some(eff_tid) = eff_tid_opt {
            let name = ctx
                .sde
                .types
                .get(&eff_tid)
                .map(|t| t.name.to_lowercase())
                .unwrap_or_default();
            if name.contains("pulsar") || eff_tid == 30844 {
                // Pulsar: +30% Shield HP (263), +30% Signature Radius (552), -15% Armor Resists, -15% Cap Recharge (55)
                if let Some(v) = attrs.get_mut(&263) {
                    *v *= 1.30;
                }
                if let Some(v) = attrs.get_mut(&552) {
                    *v *= 1.30;
                }
                if let Some(v) = attrs.get_mut(&55) {
                    *v *= 0.85;
                }
            } else if name.contains("wolf") || eff_tid == 30849 {
                // Wolf-Rayet: +30% Armor HP (265), -15% Signature Radius (552)
                if let Some(v) = attrs.get_mut(&265) {
                    *v *= 1.30;
                }
                if let Some(v) = attrs.get_mut(&552) {
                    *v *= 0.85;
                }
            } else if name.contains("black hole") || eff_tid == 30845 {
                // Black Hole: +30% Max Velocity (37), +15% Inertia (70)
                if let Some(v) = attrs.get_mut(&37) {
                    *v *= 1.30;
                }
                if let Some(v) = attrs.get_mut(&70) {
                    *v *= 1.15;
                }
            } else if name.contains("cataclysmic") || eff_tid == 30846 {
                // Cataclysmic Variable: +30% Capacitor Capacity (482)
                if let Some(v) = attrs.get_mut(&482) {
                    *v *= 1.30;
                }
            } else if name.contains("magnetar") || eff_tid == 30847 {
                // Magnetar: +30% Damage Multiplier (64), -15% Max Target Range (76)
                let cur_dmg = attrs.get(&64).copied().unwrap_or(1.0);
                attrs.insert(64, cur_dmg * 1.30);
                if let Some(v) = attrs.get_mut(&76) {
                    *v *= 0.85;
                }
            } else if name.contains("red giant") || eff_tid == 30848 {
                // Red Giant: +15% Overload Heat Damage (1211), +30% Smartbomb Range
                let cur_hd = attrs.get(&1211).copied().unwrap_or(3.4);
                attrs.insert(1211, cur_hd * 1.15);
            }
        }
    }
    crate::services::tactical_modules::apply_tactical_module_ship_attributes(ctx, ship_id, attrs);
}

/// Applies Command Burst (`group_id == 1770`) fleet `dbuffs` to all fleet members in range.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn activate_command_burst_module(
    ctx: &ServerContext,
    sys: &crate::system_actor::SystemHandle,
    char_id: i64,
    ship_id: i64,
    sys_id: u32,
    module_id: i64,
    mod_type: &eve_sde::InvType,
    mod_state: &eve_dogma::DogmaItemState,
    charge_tid_opt: Option<TypeId>,
    _duration_s: f64,
    _kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    let fit = ctx.get_or_rebuild_ship_fitting(char_id, ship_id)?;
    crate::services::tactical_modules::activate_command_burst(
        ctx,
        sys,
        char_id,
        ship_id,
        sys_id,
        module_id,
        mod_type,
        mod_state,
        charge_tid_opt,
        &fit,
    )
    .await
}

/// Rejects a cargo/ship scan when the target ball is beyond the module's evaluated
/// `cargoScanRange` (`126`) / `shipScanRange` (`125`) SDE attribute.
async fn scanner_range_check(
    ctx: &ServerContext,
    sys: &crate::system_actor::SystemHandle,
    ship_id: i64,
    target_id: i64,
    mod_state: &eve_dogma::DogmaItemState,
    range_attr: AttributeId,
) -> Result<(), BridgeRpcError> {
    let range_m = mod_state.get_attribute(range_attr, &ctx.sde);
    let (Ok(Some((_, own))), Ok(Some((_, tgt)))) =
        (sys.get_ball_snapshot(ship_id).await, sys.get_ball_snapshot(target_id).await)
    else {
        return Err(BridgeRpcError {
            code: "TargetNotInSpace".into(),
            message: format!("Scan target {target_id} is not in space with ship {ship_id}"),
        });
    };
    let dist = (own.position - tgt.position).length() - own.radius - tgt.radius;
    if range_m > 0.0 && dist > range_m {
        return Err(BridgeRpcError {
            code: "TargetOutOfRange".into(),
            message: format!("Target {target_id} is {dist:.0} m away; scanner range is {range_m:.0} m"),
        });
    }
    Ok(())
}

/// Handles Data/Relic Analyzer (`538`), Cargo Scanner (`47`), Ship Scanner (`48`),
/// Entosis Link (`1313`), and Clone Vat Bay (`815`) activation in `dogmaIM.Activate`.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn handle_scanner_analyzer_entosis_activation(
    ctx: &ServerContext,
    sys: &crate::system_actor::SystemHandle,
    char_id: i64,
    ship_id: i64,
    module_id: i64,
    mod_type: &eve_sde::InvType,
    mod_state: &eve_dogma::DogmaItemState,
    target_id_opt: Option<i64>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match mod_type.group_id {
        // Data Analyzer (`22175`) / Relic Analyzer (`22177`, `group_id == 538`)
        538 => {
            let Some(target_id) = target_id_opt else {
                return Some(Err(BridgeRpcError {
                    code: "TargetRequired".into(),
                    message: "Data/Relic Analyzer requires a locked target container".into(),
                }));
            };
            if let Err(e) = scanner_range_check(ctx, sys, ship_id, target_id, mod_state, 54).await {
                return Some(Err(e));
            }
            let skills = ctx.character_skill_levels(char_id);
            let hack_lvl = skills.get(&3405).copied().unwrap_or(0).max(skills.get(&3408).copied().unwrap_or(0));
            let base_coherence = mod_state.get_attribute(1909, &ctx.sde).max(mod_type.default_attributes.get(&1909).copied().unwrap_or(40.0));
            let base_strength = mod_state.get_attribute(1910, &ctx.sde).max(mod_type.default_attributes.get(&1910).copied().unwrap_or(20.0));
            let coherence = base_coherence + 10.0 * (hack_lvl as f64);
            let strength = base_strength + 2.0 * (hack_lvl as f64);

            scanners_superweapons_state().write().unwrap_or_else(|e| e.into_inner()).hacking_states.insert(
                target_id,
                HackingSessionState {
                    container_id: target_id,
                    analyzer_module_id: module_id,
                    virus_coherence: coherence,
                    virus_strength: strength,
                    core_hp_remaining: 0.0,
                    unlocked: true,
                },
            );
            write_container_ship_state(ctx, |st| {
                st.unlocked_containers.insert((target_id, char_id));
            });
            ctx.db.with_mut_snapshot(|snap| {
                let locked_by_job = snap.is_item_locked_in_contract_or_job(target_id);
                if let Some(cont) = snap.items.get_mut(&target_id) {
                    if !locked_by_job && (30_000_000..=39_999_999).contains(&cont.location_id) && cont.flag == InventoryFlag::Locked {
                        cont.flag = InventoryFlag::None;
                    }
                }
            });

            ctx.notify(
                crate::router::NotificationTarget::Character(char_id),
                "OnHackingStart",
                "charid",
                vec![PyRep::Integer(target_id), PyRep::Integer(module_id)],
            );
            ctx.notify(
                crate::router::NotificationTarget::Character(char_id),
                "OnHackingVirusChanged",
                "charid",
                vec![
                    PyRep::Integer(target_id),
                    PyRep::Real(coherence),
                    PyRep::Real(strength),
                ],
            );

            Some(Ok(PyRep::keyval([
                ("moduleID", PyRep::Integer(module_id)),
                ("targetID", PyRep::Integer(target_id)),
                ("virusCoherence", PyRep::Real(coherence)),
                ("virusStrength", PyRep::Real(strength)),
                ("coreRestored", PyRep::Bool(true)),
                ("containerUnlocked", PyRep::Bool(true)),
            ])))
        }

        // Cargo Scanner (`group_id == 47`, e.g. `442` Cargo Scanner I)
        47 => {
            let Some(target_id) = target_id_opt else {
                return Some(Err(BridgeRpcError {
                    code: "TargetRequired".into(),
                    message: "Cargo Scanner requires a locked target".into(),
                }));
            };
            if let Err(e) = scanner_range_check(ctx, sys, ship_id, target_id, mod_state, 126).await {
                return Some(Err(e));
            }
            let (scanned_items, total_qty) = ctx.db.with_snapshot(|snap| {
                let mut list = Vec::new();
                let mut sum_qty = 0_i64;
                for item in snap.items.values() {
                    if item.location_id == target_id && item.quantity > 0 {
                        let flg = item.flag as u16;
                        if matches!(flg, 11..=34 | 92..=94 | 125..=132) {
                            continue;
                        }
                        sum_qty += item.quantity as i64;
                        list.push(PyRep::keyval([
                            ("itemID", PyRep::Integer(item.item_id)),
                            ("typeID", PyRep::Integer(item.type_id as i64)),
                            ("quantity", PyRep::Integer(item.quantity as i64)),
                            ("flagID", PyRep::Integer(flg as i64)),
                        ]));
                    }
                }
                (list, sum_qty)
            });
            // DERIVED payload layout: client `shipUI.OnCargoScanComplete(shipID, cargoList)`.
            let cargo_list: Vec<PyRep> = scanned_items
                .iter()
                .map(|e| {
                    PyRep::Tuple(vec![
                        e.dict_get("typeID").cloned().unwrap_or(PyRep::None),
                        e.dict_get("quantity").cloned().unwrap_or(PyRep::None),
                    ])
                })
                .collect();
            ctx.notify(
                crate::router::NotificationTarget::Character(char_id),
                "OnCargoScanComplete",
                "charid",
                vec![PyRep::Integer(target_id), PyRep::List(cargo_list)],
            );
            Some(Ok(PyRep::keyval([
                ("moduleID", PyRep::Integer(module_id)),
                ("targetID", PyRep::Integer(target_id)),
                ("totalScannedQuantity", PyRep::Integer(total_qty)),
                ("scannedItems", PyRep::List(scanned_items)),
            ])))
        }

        // Ship Scanner (`group_id == 48`, e.g. `443` Ship Scanner I)
        48 => {
            let Some(target_id) = target_id_opt else {
                return Some(Err(BridgeRpcError {
                    code: "TargetRequired".into(),
                    message: "Ship Scanner requires a locked target ship".into(),
                }));
            };
            if let Err(e) = scanner_range_check(ctx, sys, ship_id, target_id, mod_state, 125).await {
                return Some(Err(e));
            }
            let (fitted_mods, charges) = ctx.db.with_snapshot(|snap| {
                let mut mods = Vec::new();
                let mut chgs = Vec::new();
                for item in snap.items.values() {
                    if item.location_id == target_id && item.quantity > 0 {
                        let flg = item.flag as u16;
                        if matches!(flg, 11..=34 | 92..=94 | 125..=132) {
                            let cat = ctx
                                .sde
                                .types
                                .get(&item.type_id)
                                .map(|t| t.category_id)
                                .unwrap_or(7);
                            if cat == 8 {
                                chgs.push(PyRep::keyval([
                                    ("itemID", PyRep::Integer(item.item_id)),
                                    ("typeID", PyRep::Integer(item.type_id as i64)),
                                    ("flagID", PyRep::Integer(flg as i64)),
                                    ("quantity", PyRep::Integer(item.quantity as i64)),
                                ]));
                            } else {
                                mods.push(PyRep::keyval([
                                    ("itemID", PyRep::Integer(item.item_id)),
                                    ("typeID", PyRep::Integer(item.type_id as i64)),
                                    ("flagID", PyRep::Integer(flg as i64)),
                                ]));
                            }
                        }
                    }
                }
                (mods, chgs)
            });
            let cs = sys
                .get_combat_state(target_id)
                .await
                .ok()
                .flatten()
                .unwrap_or_default();
            // DERIVED payload layout: client `shipUI.OnShipScanCompleted(shipID, capacitorCharge,
            // capacitorCapacity, hardwareList)`.
            let hardware: Vec<PyRep> = fitted_mods
                .iter()
                .chain(charges.iter())
                .map(|e| {
                    PyRep::Tuple(vec![
                        e.dict_get("typeID").cloned().unwrap_or(PyRep::None),
                        e.dict_get("quantity").cloned().unwrap_or(PyRep::Integer(1)),
                    ])
                })
                .collect();
            ctx.notify(
                crate::router::NotificationTarget::Character(char_id),
                "OnShipScanCompleted",
                "charid",
                vec![
                    PyRep::Integer(target_id),
                    PyRep::Real(cs.capacitor),
                    PyRep::Real(cs.max_capacitor),
                    PyRep::List(hardware),
                ],
            );
            Some(Ok(PyRep::keyval([
                ("moduleID", PyRep::Integer(module_id)),
                ("targetID", PyRep::Integer(target_id)),
                ("fittedModules", PyRep::List(fitted_mods)),
                ("charges", PyRep::List(charges)),
                ("capacitor", PyRep::Real(cs.capacitor)),
                ("maxCapacitor", PyRep::Real(cs.max_capacitor)),
                ("shieldHP", PyRep::Real(cs.shield_hp)),
                ("armorHP", PyRep::Real(cs.armor_hp)),
                ("hullHP", PyRep::Real(cs.structure_hp)),
            ])))
        }

        // Entosis Link (`group_id == 1313`, e.g. `34593` Entosis Link I, `34595` Entosis Link II)
        1313 => Some(
            crate::services::tactical_modules::handle_entosis_link_activation(
                ctx,
                sys,
                char_id,
                ship_id,
                module_id,
                mod_type,
                target_id_opt,
            )
            .await,
        ),

        // Clone Vat Bay I (`type_id == 23735`, `group_id == 815`)
        815 => Some(Ok(PyRep::keyval([
            ("moduleID", PyRep::Integer(module_id)),
            ("shipID", PyRep::Integer(ship_id)),
            ("active", PyRep::Bool(true)),
            ("cloneVatBayActive", PyRep::Bool(true)),
        ]))),

        _ => None,
    }
}

pub(crate) async fn handle_superweapons_dbuffs_and_clones(
    ctx: &ServerContext,
    session: &mut SessionState,
    _bound_entry: Option<&BoundObjectEntry>,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("superWeaponMgr", "ActivateSinglePointTargetedModule")
        | ("superWeaponMgr", "ActivateSlashModule") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "Activating a superweapon requires an active Titan".into(),
                }));
            };
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NotInSpace".into(),
                    message: "Superweapons can only be activated in space".into(),
                }));
            };
            let is_slash = method == "ActivateSlashModule";
            let module_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let target_id_opt = if is_slash {
                None
            } else {
                args.get(1).and_then(|a| a.as_i64())
            };
            Some(
                crate::services::tactical_modules::activate_superweapon_or_lance(
                    ctx,
                    char_id,
                    ship_id,
                    sys_id,
                    module_id,
                    is_slash,
                    target_id_opt,
                )
                .await,
            )
        }

        ("dogmaIM", "GetDbuffStateForClient")
        | ("dogmaIM", "GetMappedDbuffsForItem")
        | ("dogmaIM", "ApplyBuffsToItems") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let ship_id = args
                .first()
                .and_then(|a| a.as_i64())
                .or(session.ship_id)
                .unwrap_or(0);
            let now_unix = ctx.now_unix_secs();

            if method == "ApplyBuffsToItems" {
                if !session.has_gm_role() {
                    return Some(Err(BridgeRpcError {
                        code: "GMRoleRequired".into(),
                        message: "ApplyBuffsToItems requires GM/Admin authority".into(),
                    }));
                }
                let mut target_ships = Vec::new();
                if let Some(PyRep::List(list) | PyRep::Tuple(list)) = args.first() {
                    for el in list {
                        if let Some(sid) = el.as_i64() {
                            target_ships.push(sid);
                        }
                    }
                }
                if target_ships.is_empty() {
                    target_ships.push(ship_id);
                }
                let mut buff_pairs: Vec<(u32, f64)> = Vec::new();
                if let Some(PyRep::List(list) | PyRep::Tuple(list)) = args.get(1) {
                    for el in list {
                        if let PyRep::Tuple(seq) | PyRep::List(seq) = el {
                            if let (Some(did), Some(v)) = (
                                seq.first().and_then(|x| x.as_i64()),
                                seq.get(1).and_then(|x| x.as_f64()),
                            ) {
                                buff_pairs.push((did as u32, v));
                            }
                        }
                    }
                }
                if buff_pairs.is_empty() {
                    let dbuff_id = args
                        .get(1)
                        .and_then(|a| a.as_i64())
                        .unwrap_or(12) as u32;
                    let value = args
                        .get(2)
                        .and_then(|a| a.as_f64())
                        .unwrap_or(10.0);
                    buff_pairs.push((dbuff_id, value));
                }
                let duration_s = args
                    .get(2)
                    .and_then(|a| a.as_f64())
                    .unwrap_or(60.0);
                let exp = now_unix + (duration_s.round() as u64);
                {
                    let mut st = scanners_superweapons_state()
                        .write()
                        .unwrap_or_else(|e| e.into_inner());
                    for (idx, &(dbuff_id, value)) in buff_pairs.iter().enumerate() {
                        for &tsid in &target_ships {
                            st.ship_dbuffs
                                .entry((ctx.instance_id, tsid))
                                .or_default()
                                .push(ActiveDbuffEntry {
                                    dbuff_id,
                                    value,
                                    expires_at_unix: exp,
                                    source_module_id: (idx as i64) + 1,
                                });
                        }
                    }
                }
                for &tsid in &target_ships {
                    ctx.invalidate_ship_fitting(tsid);
                    ctx.notify(
                        crate::router::NotificationTarget::Character(char_id),
                        "OnDbuffUpdated",
                        "charid",
                        vec![PyRep::Integer(tsid)],
                    );
                }
            }

            let agg = get_aggregated_ship_dbuffs(ctx, ship_id, now_unix);
            if agg.is_empty() && ship_id > 0 {
                ctx.notify(
                    crate::router::NotificationTarget::Character(char_id),
                    "OnGodmaFlushLocation",
                    "charid",
                    vec![PyRep::Integer(ship_id)],
                );
            }
            let entries: Vec<(PyRep, PyRep)> = agg
                .into_iter()
                .map(|(id, (val, exp))| {
                    (
                        PyRep::Integer(id as i64),
                        PyRep::Tuple(vec![
                            PyRep::Real(val),
                            PyRep::Integer(exp.saturating_sub(now_unix) as i64),
                        ]),
                    )
                })
                .collect();
            Some(Ok(PyRep::keyval([
                ("shipID", PyRep::Integer(ship_id)),
                ("activeCount", PyRep::Integer(entries.len() as i64)),
                ("dbuffs", PyRep::Dict(entries)),
            ])))
        }

        ("dogmaIM", "GetSystemWideEffectsOnShip")
        | ("dogmaIM", "GetSystemWideEffectsInfo")
        | ("dogmaIM", "RefreshSystemWideEnvironment")
        | ("dogmaIM", "ResetSystemWideEffects") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let sys_id = args
                .first()
                .and_then(|a| a.as_i64())
                .filter(|&s| s >= 30_000_000 && s <= 39_999_999)
                .map(|s| s as u32)
                .or(session.solar_system_id)
                .unwrap_or(31_000_001);
            let ship_id = session.ship_id.unwrap_or(0);

            if method == "ResetSystemWideEffects" {
                if !session.has_gm_role() {
                    return Some(Err(BridgeRpcError {
                        code: "GMRoleRequired".into(),
                        message: "ResetSystemWideEffects requires GM/Admin authority".into(),
                    }));
                }
                let reset_sys = args
                    .first()
                    .and_then(|a| a.as_i64())
                    .filter(|&s| s >= 30_000_000)
                    .map(|s| s as u32)
                    .unwrap_or(sys_id);
                let prev = scanners_superweapons_state().write().unwrap_or_else(|e| e.into_inner()).system_wide_effects.remove(&reset_sys);
                if ship_id > 0 {
                    ctx.invalidate_ship_fitting(ship_id);
                }
                ctx.notify(
                    crate::router::NotificationTarget::Character(char_id),
                    "OnSystemWideEffectRemove",
                    "charid",
                    vec![
                        PyRep::Integer(reset_sys as i64),
                        prev.map(|t| PyRep::Integer(t as i64)).unwrap_or(PyRep::None),
                    ],
                );
                return Some(Ok(PyRep::keyval([
                    ("solarSystemID", PyRep::Integer(reset_sys as i64)),
                    ("active", PyRep::Bool(false)),
                ])));
            }

            if method == "RefreshSystemWideEnvironment" {
                if !session.has_gm_role() {
                    return Some(Err(BridgeRpcError {
                        code: "GMRoleRequired".into(),
                        message: "RefreshSystemWideEnvironment requires GM/Admin authority".into(),
                    }));
                }
                let req_tid_opt = if args.len() >= 2 {
                    args.get(1).and_then(|a| a.as_i64())
                } else {
                    args.first().and_then(|a| a.as_i64()).filter(|&t| t < 30_000_000)
                };
                if let Some(req_tid) = req_tid_opt.filter(|&t| t > 0) {
                    scanners_superweapons_state().write().unwrap_or_else(|e| e.into_inner()).system_wide_effects.insert(sys_id, req_tid as u32);
                    if ship_id > 0 {
                        ctx.invalidate_ship_fitting(ship_id);
                    }
                    ctx.notify(
                        crate::router::NotificationTarget::Character(char_id),
                        "OnSystemWideEffectAdd",
                        "charid",
                        vec![PyRep::Integer(sys_id as i64), PyRep::Integer(req_tid)],
                    );
                }
            }

            let active_tid_opt = scanners_superweapons_state()
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .system_wide_effects
                .get(&sys_id)
                .copied();
            let eff_name = active_tid_opt
                .and_then(|tid| ctx.sde.types.get(&tid))
                .map(|t| t.name.clone())
                .unwrap_or_default();

            let mut ship_attrs = HashMap::new();
            if ship_id > 0 {
                if let Ok(fit) = ctx.get_or_rebuild_ship_fitting(char_id, ship_id) {
                    let eval_ctx = fit.evaluated_context(&ctx.sde);
                    for &aid in &[37u16, 55, 64, 70, 76, 263, 265, 482, 552, 1211] {
                        ship_attrs.insert(aid, eval_ctx.ship.get_attribute(aid, &ctx.sde));
                    }
                }
                apply_active_dbuffs_and_system_effects_to_ship_attrs(ctx, ship_id, &mut ship_attrs);
            }

            Some(Ok(PyRep::keyval([
                ("solarSystemID", PyRep::Integer(sys_id as i64)),
                ("active", PyRep::Bool(active_tid_opt.is_some())),
                (
                    "effectTypeID",
                    active_tid_opt
                        .map(|t| PyRep::Integer(t as i64))
                        .unwrap_or(PyRep::None),
                ),
                ("effectName", PyRep::String(eff_name)),
                (
                    "effectiveShieldCapacity",
                    PyRep::Real(ship_attrs.get(&263).copied().unwrap_or(0.0)),
                ),
                (
                    "effectiveArmorHP",
                    PyRep::Real(ship_attrs.get(&265).copied().unwrap_or(0.0)),
                ),
                (
                    "effectiveSignatureRadius",
                    PyRep::Real(ship_attrs.get(&552).copied().unwrap_or(0.0)),
                ),
                (
                    "effectiveMaxVelocity",
                    PyRep::Real(ship_attrs.get(&37).copied().unwrap_or(0.0)),
                ),
                (
                    "effectiveDamageMultiplier",
                    PyRep::Real(ship_attrs.get(&64).copied().unwrap_or(1.0)),
                ),
            ])))
        }

        ("jumpCloneSvc", "OfferShipCloneInstallation")
        | ("jumpCloneSvc", "AcceptShipCloneInstallation")
        | ("jumpCloneSvc", "CancelShipCloneInstallation")
        | ("jumpCloneSvc", "GetShipCloneState") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if method == "OfferShipCloneInstallation" {
                let target_char_id = args
                    .first()
                    .and_then(|a| a.as_i64())
                    .unwrap_or(char_id);
                let host_ship_id = args
                    .get(1)
                    .and_then(|a| a.as_i64())
                    .or(session.ship_id)
                    .unwrap_or(0);
                let has_vat_bay = ctx.db.with_snapshot(|snap| {
                    let valid_hull = snap
                        .items
                        .get(&host_ship_id)
                        .and_then(|s| ctx.sde.types.get(&s.type_id))
                        .is_some_and(|t| matches!(t.group_id, 30 | 883));
                    let has_mod = snap.items.values().any(|i| {
                        i.location_id == host_ship_id
                            && (i.type_id == 23735
                                || ctx.sde.types.get(&i.type_id).is_some_and(|t| t.group_id == 815))
                            && snap.module_online_states.get(&i.item_id).copied().unwrap_or(true)
                    });
                    valid_hull && has_mod
                });
                if !has_vat_bay {
                    return Some(Err(BridgeRpcError {
                        code: "NoActiveCloneVatBay".into(),
                        message: "Host ship must be a Titan or Rorqual with an online Clone Vat Bay I (type 23735)".into(),
                    }));
                }
                scanners_superweapons_state()
                    .write()
                    .unwrap_or_else(|e| e.into_inner())
                    .ship_clone_offers
                    .insert(target_char_id, (char_id, host_ship_id));
                crate::services::jump_clone_names::emit_ship_clone_offered(ctx, char_id, target_char_id, host_ship_id);
                return Some(Ok(PyRep::keyval([
                    ("offeringCharID", PyRep::Integer(char_id)),
                    ("targetCharID", PyRep::Integer(target_char_id)),
                    ("hostShipID", PyRep::Integer(host_ship_id)),
                    ("offered", PyRep::Bool(true)),
                ])));
            }

            if method == "CancelShipCloneInstallation" {
                let offerer_opt = {
                    let mut st = scanners_superweapons_state().write().unwrap_or_else(|e| e.into_inner());
                    if let Some((off, _)) = st.ship_clone_offers.remove(&char_id) {
                        Some(off)
                    } else {
                        let k = st.ship_clone_offers.iter().find(|(_, (o, _))| *o == char_id).map(|(&k, _)| k);
                        if let Some(target_k) = k {
                            st.ship_clone_offers.remove(&target_k);
                        }
                        k
                    }
                };
                crate::services::jump_clone_names::emit_ship_clone_canceled(ctx, char_id, offerer_opt);
                return Some(Ok(PyRep::Bool(true)));
            }

            if method == "AcceptShipCloneInstallation" {
                let offer_opt = {
                    let mut st = scanners_superweapons_state().write().unwrap_or_else(|e| e.into_inner());
                    if let Some(off) = st.ship_clone_offers.remove(&char_id) {
                        Some(off)
                    } else {
                        let self_key = st.ship_clone_offers.iter().find(|(_, (offerer, _))| *offerer == char_id).map(|(&k, _)| k);
                        self_key.and_then(|k| st.ship_clone_offers.remove(&k))
                    }
                };
                let Some((offerer_id, host_ship_id)) = offer_opt else {
                    return Some(Err(BridgeRpcError {
                        code: "NoCloneOfferFound".into(),
                        message: "No pending ship clone installation offer found".into(),
                    }));
                };
                let skills = ctx.character_skill_levels(char_id);
                let max_clones = ((skills.get(&33399).copied().unwrap_or(0) + skills.get(&33407).copied().unwrap_or(0)) as usize).max(1);
                let clone_res = ctx.db.try_with_mut_snapshot(|snap| {
                    let valid_hull = snap.items.get(&host_ship_id).and_then(|s| ctx.sde.types.get(&s.type_id)).is_some_and(|t| matches!(t.group_id, 30 | 883));
                    let has_mod = snap.items.values().any(|i| {
                        i.location_id == host_ship_id
                            && (i.type_id == 23735 || ctx.sde.types.get(&i.type_id).is_some_and(|t| t.group_id == 815))
                            && snap.module_online_states.get(&i.item_id).copied().unwrap_or(true)
                    });
                    if !valid_hull || !has_mod {
                        return Err(BridgeRpcError {
                            code: "NoActiveCloneVatBay".into(),
                            message: "Host ship does not have an online Clone Vat Bay".into(),
                        });
                    }
                    let existing_count = snap.jump_clones.get(&char_id).map_or(0, |v| v.len());
                    if existing_count >= max_clones {
                        return Err(BridgeRpcError {
                            code: "JumpCloneLimitReached".into(),
                            message: format!("Character {char_id} has {existing_count} jump clones (max {max_clones})"),
                        });
                    }
                    if let Some(oz_id) = snap.items.values().find(|i| i.location_id == host_ship_id && i.type_id == 16273 && i.quantity > 0).map(|i| i.item_id) {
                        let mut rem = 0;
                        if let Some(it) = snap.items.get_mut(&oz_id) {
                            it.quantity -= 1;
                            rem = it.quantity;
                        }
                        if rem == 0 {
                            snap.items.remove(&oz_id);
                        }
                    }
                    snap.next_entity_id += 1;
                    let cid = snap.next_entity_id;
                    snap.jump_clones.entry(char_id).or_default().push(eve_db::JumpCloneRecord {
                        jump_clone_id: cid,
                        character_id: char_id,
                        location_id: host_ship_id,
                        implants: Vec::new(),
                    });
                    Ok(cid)
                });
                let clone_id = match clone_res {
                    Ok(c) => c,
                    Err(e) => return Some(Err(e)),
                };
                crate::services::jump_clone_names::emit_ship_clone_accepted(ctx, offerer_id, char_id, host_ship_id);
                return Some(Ok(PyRep::keyval([
                    ("jumpCloneID", PyRep::Integer(clone_id)),
                    ("characterID", PyRep::Integer(char_id)),
                    ("locationID", PyRep::Integer(host_ship_id)),
                    ("installed", PyRep::Bool(true)),
                ])));
            }

            // GetShipCloneState
            let host_ship_id = args
                .first()
                .and_then(|a| a.as_i64())
                .or(session.ship_id)
                .unwrap_or(0);
            let rows: Vec<Vec<PyRep>> = ctx.db.with_snapshot(|snap| {
                snap.jump_clones
                    .values()
                    .flat_map(|v| v.iter())
                    .filter(|jc| jc.location_id == host_ship_id)
                    .map(|jc| {
                        vec![
                            PyRep::Integer(jc.jump_clone_id),
                            PyRep::Integer(jc.character_id),
                            PyRep::Integer(jc.location_id),
                            PyRep::String(
                                snap.item_custom_names
                                    .get(&jc.jump_clone_id)
                                    .cloned()
                                    .unwrap_or_default(),
                            ),
                        ]
                    })
                    .collect()
            });
            Some(Ok(PyRep::rowset(
                vec!["jumpCloneID", "ownerID", "locationID", "cloneName"],
                rows,
            )))
        }

        _ => None,
    }
}
