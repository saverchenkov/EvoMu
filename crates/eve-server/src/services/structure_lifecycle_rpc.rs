//! `structure_lifecycle_rpc` — client-facing entry points of the Upwell lifecycle:
//! `structureDeployment.Anchor` and the pre-existing `structureControl.FuelStructure` /
//! `structureControl.AttackStructure` handlers, rebuilt on top of [`super::structure_lifecycle`].
#![forbid(unsafe_code)]
use super::structure_lifecycle::{self as life, STATE_ANCHOR_VULNERABLE};
use super::structure_upwell_ops::{load_ext_state, save_ext_state};
use crate::router::ServerContext;
use eve_db::{InventoryFlag, UpwellStructureRecord};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use glam::DVec3;

fn err(code: &str, message: impl Into<String>) -> BridgeRpcError {
    BridgeRpcError { code: code.into(), message: message.into() }
}

fn has_structure_manager_role(
    snap: &eve_db::WorldStateSnapshot,
    session: &SessionState,
    char_id: i64,
    corp_id: i64,
) -> bool {
    let req_roles = eve_db::corp_roles::ROLE_DIRECTOR | eve_db::corp_roles::ROLE_STATION_MANAGER;
    session.has_gm_role()
        || (session.corp_role & req_roles) != 0
        || snap.corporations.get(&corp_id).is_some_and(|c| {
            c.ceo_id == char_id
                || c.member_roles.get(&char_id).is_some_and(|&r| (r & req_roles) != 0)
        })
        || snap
            .corp_role_assignments
            .get(&(corp_id, char_id))
            .is_some_and(|a| (a.roles & req_roles) != 0)
}

/// `structureDeployment.Anchor(typeID, name)`: deploys the structure at the anchoring ship's
/// position. The structure starts in the 15-minute hull-only anchoring timer; the quantum core
/// is NOT consumed here — it is inserted after the 24 h anchoring reinforcement.
pub async fn handle_anchor(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    if session.station_id.is_some() || session.structure_id.is_some() {
        return Err(err("ShipDocked", "Must be undocked in space to anchor an Upwell structure"));
    }
    let ship_id = session.ship_id.ok_or_else(|| err("NoActiveShip", "Anchoring requires an active ship in space"))?;
    let first_arg = args.first().and_then(|a| a.as_i64()).ok_or_else(|| err("InvalidStructureType", "Anchor requires a structure itemID or typeID"))?;
    let (cargo_item_id_opt, type_id) = ctx.db.with_snapshot(|snap| {
        let corp_id = snap.characters.get(&char_id).map(|c| c.corporation_id).or(session.corporation_id).unwrap_or(0);
        if let Some(item) = snap.items.get(&first_arg) {
            if item.owner_id != char_id && item.owner_id != corp_id {
                return Err(err("ItemOwnershipViolation", format!("Character {char_id} does not own structure item {first_arg}")));
            }
            if item.location_id != ship_id {
                return Err(err("ItemNotInCargo", format!("Item {first_arg} is not in active ship {ship_id} cargo")));
            }
            Ok((Some(first_arg), item.type_id))
        } else if let Ok(u_tid) = u32::try_from(first_arg) {
            Ok((None, u_tid))
        } else {
            Err(err("ItemNotFound", format!("Item or structure type {first_arg} not found")))
        }
    })?;
    if type_id == 40340 {
        let palatine_exists = ctx.db.with_snapshot(|snap| snap.structures.values().any(|s| s.type_id == 40340 && s.state != "Destroyed"));
        if palatine_exists {
            return Err(err("PalatineKeepstarLimitReached", "Only one Upwell Palatine Keepstar (type 40340) can exist in the universe"));
        }
    }
    let profile = life::structure_profile(ctx, type_id).ok_or_else(|| err("InvalidStructureType", format!("Type {type_id} is not an anchorable Upwell structure (category {})", life::CATEGORY_STRUCTURE)))?;
    let sys_id = session.solar_system_id.ok_or_else(|| err("NoSolarSystem", "Session has no active solarSystemID"))?;
    let (_, is_thera, is_shattered, _) = crate::services::space_destiny::system_wormhole_profile(sys_id);
    if is_thera || is_shattered {
        return Err(err("AnchoringDisallowedInTheraOrShattered", format!("Anchoring structures or starbases is disallowed in Thera or shattered wormhole system {sys_id}")));
    }
    let sys_sec = ctx.sde.solar_systems.get(&sys_id).map(|s| s.security_status).unwrap_or(1.0);
    if type_id == 35840 && sys_sec >= 0.45 {
        return Err(err("CannotAnchorCynoBeaconInHighSec", format!("Pharolux Cyno Beacon (35840) cannot be anchored in high-security solar system {sys_id} (security {sys_sec:.2} >= 0.45)")));
    }
    let (_, ship_ball) = ctx
        .systems
        .get_or_spawn(sys_id)
        .get_ball_snapshot(ship_id)
        .await
        .map_err(|e| err("DestinyQueryFailed", e))?
        .ok_or_else(|| err("ShipNotInSpace", format!("Active ship {ship_id} is not spawned in space in system {sys_id}")))?;
    let explicit_pos = match args.get(1) {
        Some(PyRep::List(coords)) | Some(PyRep::Tuple(coords)) if coords.len() >= 3 => {
            let (Some(x), Some(y), Some(z)) = (coords[0].as_f64(), coords[1].as_f64(), coords[2].as_f64()) else {
                return Err(err("InvalidAnchorCoordinates", "Anchoring coordinates must be valid numbers"));
            };
            let pos = DVec3::new(x, y, z);
            if !pos.is_finite() || pos.abs().max_element() > 1.0e14 {
                return Err(err("InvalidAnchorCoordinates", "Anchoring coordinates must be finite numbers within solar system bounds"));
            }
            if pos.distance(ship_ball.position) > 100_000.0 {
                return Err(err("AnchorPositionTooFarFromShip", "Anchoring position must be within 100 km of the deploying ship"));
            }
            Some(pos)
        }
        _ => None,
    };
    let anchor_pos: DVec3 = explicit_pos.unwrap_or(ship_ball.position);
    let profile_id_arg = args.get(2).and_then(|a| a.as_i64());
    let reinforce_window = match args.get(3) {
        Some(PyRep::Tuple(seq)) | Some(PyRep::List(seq)) if seq.len() >= 2 => {
            let a = seq[0].as_i64().unwrap_or(0) as u64;
            let b = seq[1].as_i64().unwrap_or(0) as u64;
            if a < 24 && b < 7 { Some((a, b)) } else { Some((b, a)) }
        }
        _ => None,
    };
    let name = args.iter().filter_map(|a| a.as_str()).map(str::trim).filter(|s| !s.is_empty()).last().unwrap_or("Upwell Structure").to_string();
    let now = ctx.now_unix();
    let (structure_id, owner_corp_id) = ctx.db.try_with_mut_snapshot(|snap| {
        let corp_id = snap.characters.get(&char_id).map(|c| c.corporation_id).or(session.corporation_id).unwrap_or(0);
        if corp_id < 98_000_000 || !snap.corporations.contains_key(&corp_id) {
            return Err(err("NpcCorpCannotAnchorStructure", format!("Character {char_id} is in NPC corporation {corp_id}; only player corporations may anchor Upwell structures")));
        }
        if !has_structure_manager_role(snap, session, char_id, corp_id) {
            return Err(err("InsufficientCorpRoles", "Anchoring an Upwell structure requires Director or Station Manager role"));
        }
        life::check_anchor_position(ctx, snap, &profile, sys_id, anchor_pos)?;
        let item_id = if let Some(cid) = cargo_item_id_opt {
            let item = snap.items.get(&cid).ok_or_else(|| err("MissingStructureItem", format!("Cargo item {cid} not found")))?;
            if (item.owner_id != char_id && item.owner_id != corp_id) || item.location_id != ship_id || item.quantity < 1 || item.flag == InventoryFlag::Locked || snap.is_item_locked_in_contract_or_job(cid) {
                return Err(err("MissingStructureItem", format!("Cargo item {cid} not available in cargo")));
            }
            cid
        } else {
            snap.items
                .values()
                .filter(|i| (i.owner_id == char_id || i.owner_id == corp_id) && i.location_id == ship_id && i.type_id == type_id && i.quantity >= 1 && i.flag != InventoryFlag::Locked && !snap.is_item_locked_in_contract_or_job(i.item_id))
                .map(|i| i.item_id)
                .min()
                .ok_or_else(|| err("MissingStructureItem", format!("Anchoring requires structure type {type_id} in active ship {ship_id} cargo")))?
        };
        if let Some(item) = snap.items.get_mut(&item_id) {
            if item.quantity > 1 {
                item.quantity -= 1;
                item.version += 1;
            } else {
                snap.items.remove(&item_id);
            }
        }
        snap.next_entity_id += 1;
        let id = 1_000_000_000_000 + snap.next_entity_id;
        snap.structures.insert(
            id,
            UpwellStructureRecord {
                structure_id: id,
                type_id,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: name.clone(),
                position: anchor_pos.to_array(),
                state: STATE_ANCHOR_VULNERABLE.into(),
                quantum_core_installed: false,
                fuel_blocks: 0,
                // Vulnerability.md: a freshly deployed structure has only structure (hull) HP.
                shield_hp: 0.0,
                armor_hp: 0.0,
                hull_hp: profile.hull_hp,
                reinforced_until_unix: now + life::ANCHOR_VULNERABLE_SECS,
                last_damage_tick: 0,
                last_damage_unix: 0, self_destruct_at_unix: None,
            },
        );
        let vuln = reinforce_window.unwrap_or(((now / 3_600) % 24, ((now / 86_400) + 4) % 7));
        snap.structure_vulnerability_windows.insert(id, vuln);
        if let Some(pid) = profile_id_arg {
            let mut ext = crate::services::station_structure::structure_upwell_ops::load_ext_state(snap);
            if let Some(p) = ext.profiles_by_id.get(&pid).cloned() {
                ext.access_profiles.insert(id, p);
                ext.structure_profile_id.insert(id, pid);
                crate::services::station_structure::structure_upwell_ops::save_ext_state(snap, &ext);
            }
        }
        Ok((id, corp_id))
    })?;
    ensure_structure_ball_in_space(ctx, sys_id, structure_id).await;
    ctx.notify(
        crate::router::NotificationTarget::SolarSystem(sys_id),
        "OnStructureAdded",
        "solarsystemid",
        vec![PyRep::Integer(i64::from(sys_id)), PyRep::Integer(structure_id)],
    );
    ctx.notify(
        crate::router::NotificationTarget::Corporation(owner_corp_id),
        "OnCorporationStructuresUpdated",
        "corpid",
        vec![],
    );
    let state_args = vec![
        PyRep::Integer(i64::from(sys_id)),
        PyRep::Integer(structure_id),
        PyRep::String(STATE_ANCHOR_VULNERABLE.into()),
    ];
    ctx.notify(
        crate::router::NotificationTarget::SolarSystem(sys_id),
        "OnStructureStateChanged",
        "solarsystemid",
        state_args.clone(),
    );
    ctx.notify(
        crate::router::NotificationTarget::SolarSystem(sys_id),
        "OnCurrentStructureStateChanged",
        "solarsystemid",
        state_args,
    );
    Ok(PyRep::keyval([
        ("structureID", PyRep::Integer(structure_id)),
        ("state", PyRep::String(STATE_ANCHOR_VULNERABLE.into())),
        ("stateTimerEndUnix", PyRep::Integer((now + life::ANCHOR_VULNERABLE_SECS) as i64)),
        ("quantumCoreInstalled", PyRep::Bool(false)),
        ("quantumCoreTypeID", profile.core_type_id.map(|c| PyRep::Integer(i64::from(c))).unwrap_or(PyRep::None)),
        ("hullHP", PyRep::Real(profile.hull_hp)),
        ("fuelBlocks", PyRep::Integer(0)),
    ]))
}

/// `structureControl.FuelStructure(structureID, blocks)`: moves fuel blocks of an SDE service
/// fuel group (`serviceModuleFuelConsumptionGroup`) from the structure hangar or active ship
/// into the structure fuel bay.
pub fn handle_fuel(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let structure_id = args.first().and_then(|a| a.as_i64()).or(session.structure_id).ok_or_else(|| err("MissingStructureId", "FuelStructure requires structureID"))?;
    let blocks = args.get(1).and_then(|a| a.as_i64()).and_then(|v| i32::try_from(v).ok()).filter(|v| *v > 0).ok_or_else(|| err("InvalidFuelQuantity", "Fuel block count must be strictly positive"))?;
    let caller_sys = session.solar_system_id.unwrap_or(0);
    let active_ship = session.ship_id;
    let fuel_groups = life::service_fuel_groups(ctx);
    ctx.db.try_with_mut_snapshot(|snap| {
        let caller_corp = snap.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(0);
        let st = snap.structures.get(&structure_id).cloned().ok_or_else(|| err("StructureNotFound", format!("Structure {structure_id} does not exist")))?;
        if st.owner_corp_id != caller_corp {
            return Err(err("StructureOwnershipViolation", format!("Only members of owning corporation {} may fuel structure {structure_id}", st.owner_corp_id)));
        }
        if st.solar_system_id != caller_sys {
            return Err(err("StructureWrongSystem", format!("Structure {structure_id} is in system {}, but caller is in {caller_sys}", st.solar_system_id)));
        }
        if st.state == life::STATE_DESTROYED {
            return Err(err("StructureDestroyed", format!("Structure {structure_id} is destroyed")));
        }
        let fuel_item = snap
            .items
            .values()
            .filter(|i| {
                i.owner_id == char_id
                    && i.quantity >= blocks
                    && i.flag != InventoryFlag::Locked
                    && !snap.is_item_locked_in_contract_or_job(i.item_id)
                    && (i.location_id == structure_id || active_ship == Some(i.location_id))
                    && ctx.sde.types.get(&i.type_id).map(|t| fuel_groups.contains(&t.group_id)).unwrap_or(false)
            })
            .map(|i| i.item_id)
            .min()
            .ok_or_else(|| err("InsufficientFuelBlocks", format!("Fueling requires {blocks} fuel block(s) of a service-module fuel group in structure {structure_id} hangar or active ship cargo")))?;
        if let Some(item) = snap.items.get_mut(&fuel_item) {
            if item.quantity > blocks {
                item.quantity -= blocks;
                item.version += 1;
            } else {
                snap.items.remove(&fuel_item);
            }
        }
        let s = snap.structures.get_mut(&structure_id).ok_or_else(|| err("StructureNotFound", "structure vanished"))?;
        s.fuel_blocks = s.fuel_blocks.saturating_add(blocks as u32);
        Ok(PyRep::keyval([("structureID", PyRep::Integer(structure_id)), ("fuelBlocks", PyRep::Integer(i64::from(s.fuel_blocks)))]))
    })
}

/// Online high-slot weapons with a loaded, skill-usable charge: `(raw dps, shortest cycle, charge items)`.
fn attacker_weapon_volley(ctx: &ServerContext, snap: &eve_db::WorldStateSnapshot, char_id: i64, ship_id: i64, weapons: &[(u16, u32)]) -> Result<(f64, f64, Vec<i64>), BridgeRpcError> {
    let mut total_dps = 0.0_f64;
    let mut min_cycle = f64::MAX;
    let mut charges = Vec::with_capacity(weapons.len());
    let mut skill_err = None;
    for &(flag_id, wtid) in weapons {
        let Some(wt) = ctx.sde.types.get(&wtid) else { continue };
        let loaded = snap
            .items
            .values()
            .find(|i| i.owner_id == char_id && i.location_id == ship_id && (i.flag as u16) == flag_id && i.quantity > 0 && ctx.sde.types.get(&i.type_id).map(|ct| ServerContext::is_charge_compatible_with_weapon(wt, ct)).unwrap_or(false))
            .map(|i| (i.item_id, i.type_id));
        let Some((cid, ctid)) = loaded else { continue };
        if let Err(e) = ctx.check_type_skill_prerequisites_in_snapshot(snap, char_id, ctid) {
            skill_err.get_or_insert(e);
            continue;
        }
        let Some(ct) = ctx.sde.types.get(&ctid) else { continue };
        let cycle = (wt.default_attributes.get(&51).copied().unwrap_or(3000.0) / 1000.0).max(1.0);
        let mult = wt.default_attributes.get(&64).copied().unwrap_or(1.0);
        let volley = [114u16, 116, 117, 118].iter().map(|a| ct.default_attributes.get(a).copied().unwrap_or(0.0)).sum::<f64>() * mult;
        if volley > 0.0 {
            total_dps += volley / cycle;
            min_cycle = min_cycle.min(cycle);
            charges.push(cid);
        }
    }
    if total_dps <= 0.0 || charges.is_empty() {
        return Err(skill_err.unwrap_or_else(|| err("NoAmmoLoaded", format!("Active ship {ship_id} has fitted high-slot weapons but no compatible charge loaded (flags 27..=34)"))));
    }
    Ok((total_dps, if min_cycle.is_finite() { min_cycle } else { 3.0 }, charges))
}

/// `structureControl.AttackStructure(structureID)`: one weapon cycle of the attacker's fitted
/// weapons against the structure, resolved by [`life::apply_structure_damage`].
pub async fn handle_attack(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    if session.station_id.is_some() || session.structure_id.is_some() {
        return Err(err("ShipDocked", "Cannot attack an Upwell structure while docked"));
    }
    let ship_id = session.ship_id.ok_or_else(|| err("NoActiveShip", "Session has no active ship"))?;
    let structure_id = args.first().and_then(|a| a.as_i64()).ok_or_else(|| err("MissingStructureId", "AttackStructure requires an explicit structureID argument"))?;
    let caller_sys = session.solar_system_id.unwrap_or(0);
    // Siege DPS comes from the fitting engine's online high-slot weapons, never from args.
    let fit = ctx.get_or_rebuild_ship_fitting(char_id, ship_id)?;
    let mut weapons: Vec<(u16, u32)> = fit
        .modules
        .values()
        .filter(|m| (27..=34).contains(&m.flag_id) && m.state != eve_dogma::ModuleState::Offline && ctx.sde.types.get(&m.type_id).map(|t| matches!(t.group_id, 53 | 55 | 74 | 506..=511 | 1986 | 4060 | 4500 | 4805 | 4807)).unwrap_or(false))
        .map(|m| (m.flag_id, m.type_id))
        .collect();
    weapons.sort_unstable();
    if weapons.is_empty() {
        return Err(err("NoWeaponFitted", format!("Active ship {ship_id} has no fitted high-slot weapon")));
    }
    let (current_tick, ship_ball) = ctx
        .systems
        .get_or_spawn(caller_sys)
        .get_ball_snapshot(ship_id)
        .await
        .map_err(|e| err("DestinyQueryFailed", e))?
        .ok_or_else(|| err("ShipNotInSpace", format!("Ship {ship_id} is not spawned in space")))?;
    let sys_sec = ctx.sde.solar_systems.get(&caller_sys).map(|s| s.security_status).unwrap_or(0.0);
    let now = ctx.now_unix();
    let outcome = ctx.db.try_with_mut_snapshot(|snap| {
        let (raw_dps, cycle, charges) = attacker_weapon_volley(ctx, snap, char_id, ship_id, &weapons)?;
        let caller_corp = snap.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(0);
        let st = snap.structures.get(&structure_id).cloned().ok_or_else(|| err("StructureNotFound", format!("Structure {structure_id} does not exist")))?;
        if st.solar_system_id != caller_sys {
            return Err(err("StructureWrongSystem", format!("Structure {structure_id} is in system {}, but caller is in {caller_sys}", st.solar_system_id)));
        }
        if st.owner_corp_id == caller_corp {
            return Err(err("CannotAttackFriendlyStructure", "Cannot attack a structure owned by your own corporation"));
        }
        let at_war = snap.wars.values().any(|w| {
            w.active && now >= w.started_unix.saturating_add(86_400) && ((w.aggressor_corp_id == caller_corp && w.defender_corp_id == st.owner_corp_id) || (w.aggressor_corp_id == st.owner_corp_id && w.defender_corp_id == caller_corp))
        });
        if sys_sec >= 0.45 && !at_war {
            return Err(err("IllegalHighSecStructureAttack", "Cannot attack an Upwell structure in high-security space without an active war declaration (24h warmup complete)"));
        }
        let cycle_ticks = (cycle.ceil() as u64).max(1);
        let tick = current_tick.max(1);
        if st.last_damage_tick > 0 && tick < st.last_damage_tick.saturating_add(cycle_ticks) {
            return Err(err("WeaponCycling", format!("Weapons are cycling against structure {structure_id} until tick {}", st.last_damage_tick.saturating_add(cycle_ticks))));
        }
        let st_radius = ctx.sde.types.get(&st.type_id).map(|t| t.radius).unwrap_or(0.0);
        let dist = (ship_ball.position.distance(DVec3::from_array(st.position)) - st_radius - ship_ball.radius).max(0.0);
        if dist > 50_000.0 {
            return Err(err("StructureOutOfRange", format!("Ship is {dist:.1} m from structure {structure_id} (must be <= 50000 m)")));
        }
        let mut ext = load_ext_state(snap);
        let out = life::apply_structure_damage(ctx, snap, &mut ext, structure_id, raw_dps * cycle, cycle, char_id, now)?;
        save_ext_state(snap, &ext);
        for cid in &charges {
            if let Some(ch) = snap.items.get_mut(cid) {
                if ch.quantity > 1 {
                    ch.quantity -= 1;
                    ch.version += 1;
                } else {
                    snap.items.remove(cid);
                }
            }
        }
        if let Some(s) = snap.structures.get_mut(&structure_id) {
            s.last_damage_tick = tick;
        }
        Ok(out)
    })?;
    let d = outcome.destruction.unwrap_or_default();
    Ok(PyRep::keyval([
        ("structureID", PyRep::Integer(structure_id)),
        ("appliedDamage", PyRep::Real(outcome.applied_damage)),
        ("damagedLayer", PyRep::String(outcome.layer.into())),
        ("repairTimerPaused", PyRep::Bool(outcome.repair_paused)),
        ("state", PyRep::String(outcome.state)),
        ("shieldHP", PyRep::Real(outcome.shield_hp)),
        ("armorHP", PyRep::Real(outcome.armor_hp)),
        ("hullHP", PyRep::Real(outcome.hull_hp)),
        ("reinforcedUntilUnix", PyRep::Integer(outcome.reinforced_until_unix as i64)),
        ("killmailID", d.killmail_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
        ("droppedQuantumCoreItemID", PyRep::Integer(d.dropped_core_item_id.unwrap_or(0))),
        ("assetSafetyWrapIDs", PyRep::List(d.asset_safety_wrap_ids.into_iter().map(PyRep::Integer).collect())),
        ("droppedItemIDs", PyRep::List(d.dropped_item_ids.into_iter().map(PyRep::Integer).collect())),
    ]))
}

const CAN_FIT_SHIP_GROUP_ATTRS: [u16; 20] = [
    1298, 1299, 1300, 1301, 1872, 1879, 1880, 1881, 2065, 2396,
    2476, 2477, 2478, 2479, 2480, 2481, 2482, 2483, 2484, 2485,
];
const CAN_FIT_SHIP_TYPE_ATTRS: [u16; 11] = [
    1302, 1303, 1304, 1305, 1944, 2103, 2463, 2486, 2487, 2488, 2758,
];

/// Ensures an anchored Upwell structure has an `ewar_immune: true` ball in its solar system (`feat:structures.ewar-immunity`).
pub async fn ensure_structure_ball_in_space(ctx: &ServerContext, sys_id: u32, structure_id: i64) {
    let sys = ctx.systems.get_or_spawn(sys_id);
    if sys.get_ball_snapshot(structure_id).await.ok().flatten().is_some() {
        return;
    }
    let Some(st) = ctx.db.with_snapshot(|snap| snap.structures.get(&structure_id).cloned()) else {
        return;
    };
    if st.solar_system_id != sys_id || st.state == life::STATE_DESTROYED {
        return;
    }
    let st_type = ctx.sde.types.get(&st.type_id);
    let radius = st_type.map(|t| t.radius).unwrap_or(10_000.0).max(1_000.0);
    let mass = st_type.map(|t| t.mass).unwrap_or(1e11).max(1e9);
    let ball = eve_destiny::Ball {
        id: st.structure_id,
        type_id: st.type_id,
        position: DVec3::from_array(st.position),
        velocity: DVec3::ZERO,
        max_velocity: 0.0,
        inertia: 1.0,
        mass,
        radius,
        speed_fraction: 0.0,
        mode: eve_destiny::MovementMode::Stop,
    };
    let mut cs = eve_destiny::CombatState::new();
    cs.shield_hp = st.shield_hp.max(1.0);
    cs.max_shield_hp = st.shield_hp.max(1.0);
    cs.armor_hp = st.armor_hp.max(1.0);
    cs.max_armor_hp = st.armor_hp.max(1.0);
    cs.structure_hp = st.hull_hp.max(1.0);
    cs.max_structure_hp = st.hull_hp.max(1.0);
    let meta = eve_destiny::SlimItemMeta {
        owner_id: st.owner_corp_id,
        char_id: None,
        corp_id: st.owner_corp_id,
        alliance_id: -1,
        group_id: st_type.map(|t| t.group_id).unwrap_or(1657),
        category_id: life::CATEGORY_STRUCTURE,
        security_status: 0.0,
        is_cloaked: false,
    };
    let _ = sys.spawn_ball_with_meta(ball, cs, meta).await;
}

fn check_module_fits_structure(mod_type: &eve_sde::InvType, st_type_id: u32, st_group_id: u32, profile: &life::StructureProfile, flag_u16: u16) -> Result<(), BridgeRpcError> {
    let allowed_types: Vec<u32> = CAN_FIT_SHIP_TYPE_ATTRS
        .iter()
        .filter_map(|a| mod_type.default_attributes.get(a).copied())
        .filter(|v| *v > 0.0)
        .map(|v| v as u32)
        .collect();
    let allowed_groups: Vec<u32> = CAN_FIT_SHIP_GROUP_ATTRS
        .iter()
        .filter_map(|a| mod_type.default_attributes.get(a).copied())
        .filter(|v| *v > 0.0)
        .map(|v| v as u32)
        .collect();
    if (!allowed_types.is_empty() || !allowed_groups.is_empty())
        && !allowed_types.contains(&st_type_id)
        && !allowed_groups.contains(&st_group_id)
    {
        return Err(err("ModuleCannotFitStructureType", format!("Module type {} cannot be fitted to structure type {st_type_id} (group {st_group_id})", mod_type.type_id)));
    }
    let ok_slot = match flag_u16 {
        11..=18 => u32::from(flag_u16 - 11) < profile.low_slots,
        19..=26 => u32::from(flag_u16 - 19) < profile.mid_slots,
        27..=34 => u32::from(flag_u16 - 27) < profile.high_slots,
        92..=94 => {
            if let Some(&rig_size) = mod_type.default_attributes.get(&life::ATTR_RIG_SIZE) {
                let want = match profile.hull_class {
                    life::HullClass::Medium => 2,
                    life::HullClass::Large => 3,
                    life::HullClass::XLarge => 4,
                    life::HullClass::Flex => 0,
                };
                if rig_size as i64 != want {
                    return Err(err("RigSizeMismatch", format!("Rig type {} has rigSize {} but structure requires {want}", mod_type.type_id, rig_size as i64)));
                }
            }
            u32::from(flag_u16 - 92) < profile.rig_slots
        }
        164..=171 => u32::from(flag_u16 - 164) < profile.service_slots,
        _ => false,
    };
    if !ok_slot {
        return Err(err("InvalidStructureFittingSlot", format!("Structure type {st_type_id} does not have slot flag {flag_u16}")));
    }
    Ok(())
}

/// Handles fitting or unfitting a module/rig on an Upwell structure (`feat:structures.structure-fitting`,
/// `feat:structures.fitting-state-locks`, `feat:structures.service-modules`).
pub fn handle_structure_fit_move(ctx: &ServerContext, session: &mut SessionState, item_id: i64, new_loc: i64, flag: InventoryFlag, online: bool) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    life::sync_all_structures(ctx);
    let now = ctx.now_unix();
    let flag_u16 = flag as u16;
    let is_target_fit_slot = matches!(flag_u16, 11..=34 | 92..=94 | 164..=171);
    let res = ctx.db.try_with_mut_snapshot(|snap| {
        let src = snap.items.get(&item_id).cloned().ok_or_else(|| err("ItemNotFound", format!("Item {item_id} not found")))?;
        let structure_id = if snap.structures.contains_key(&new_loc) && is_target_fit_slot {
            new_loc
        } else if snap.structures.contains_key(&src.location_id) && matches!(src.flag as u16, 11..=34 | 92..=94 | 164..=171) {
            src.location_id
        } else {
            return Err(err("StructureNotFound", "Target or source is not a structure fitting slot"));
        };
        let mut ext = load_ext_state(snap);
        let (st, profile) = life::validate_structure_refit(ctx, snap, &ext, structure_id, now)?;
        let caller_corp = snap.characters.get(&char_id).map(|c| c.corporation_id).or(session.corporation_id).unwrap_or(0);
        if st.owner_corp_id != caller_corp {
            return Err(err("StructureOwnershipViolation", format!("Character {char_id} does not belong to owning corp {} of structure {structure_id}", st.owner_corp_id)));
        }
        if !has_structure_manager_role(snap, session, char_id, caller_corp) {
            return Err(err("InsufficientCorpRoles", "Fitting or unfitting structure modules requires Director or Station Manager role"));
        }
        if !is_target_fit_slot {
            // Unfitting from structure
            if new_loc != structure_id && session.ship_id != Some(new_loc) {
                return Err(err("InvalidItemLocation", format!("Cannot unfit module into location {new_loc}")));
            }
            if matches!(src.flag as u16, 92..=94) {
                snap.items.remove(&item_id);
                snap.module_online_states.remove(&item_id);
                return Ok((structure_id, src.type_id, true, false, false));
            }
            let was_service = (164..=171).contains(&(src.flag as u16));
            if let Some(it) = snap.items.get_mut(&item_id) {
                it.location_id = new_loc;
                it.flag = flag;
                it.version += 1;
            }
            snap.module_online_states.remove(&item_id);
            if was_service {
                life::sync_structure(ctx, snap, &mut ext, structure_id, now);
                save_ext_state(snap, &ext);
            }
            return Ok((structure_id, src.type_id, false, false, was_service));
        }
        // Fitting into structure slot
        if src.owner_id != char_id && src.owner_id != st.owner_corp_id {
            return Err(err("ItemOwnershipViolation", format!("Character {char_id} does not own module {item_id}")));
        }
        if src.location_id != structure_id && session.ship_id != Some(src.location_id) {
            return Err(err("InvalidItemLocation", format!("Module {item_id} must be in structure {structure_id} or active ship cargo")));
        }
        if src.quantity < 1 || src.flag == InventoryFlag::Locked || snap.is_item_locked_in_contract_or_job(item_id) {
            return Err(err("ItemLocked", format!("Module {item_id} is locked and cannot be fitted")));
        }
        let mod_type = ctx.sde.types.get(&src.type_id).ok_or_else(|| err("UnknownType", format!("Type {} not in SDE", src.type_id)))?;
        check_module_fits_structure(mod_type, st.type_id, profile.group_id, &profile, flag_u16)?;
        if snap.items.values().any(|i| i.item_id != item_id && i.location_id == structure_id && i.flag == flag) {
            return Err(err("SlotAlreadyOccupied", format!("Structure {structure_id} slot {flag_u16} is already occupied")));
        }
        let is_service = (164..=171).contains(&flag_u16);
        let online_fuel = mod_type.default_attributes.get(&life::ATTR_SERVICE_FUEL_ONLINE).copied().unwrap_or(0.0) as u32;
        let will_online = if online && is_service {
            st.fuel_blocks >= online_fuel
        } else {
            online && !matches!(flag_u16, 92..=94)
        };
        if let Some(it) = snap.items.get_mut(&item_id) {
            it.location_id = structure_id;
            it.flag = flag;
            it.singleton = true;
            it.version += 1;
        }
        snap.module_online_states.insert(item_id, will_online);
        if is_service {
            let max_before = life::effective_max_shield(ctx, snap, &profile, structure_id, life::PowerState::LowPower);
            life::sync_structure(ctx, snap, &mut ext, structure_id, now);
            let power_now = life::power_state(ctx, snap, &ext, &st, now);
            let max_after = life::effective_max_shield(ctx, snap, &profile, structure_id, power_now);
            if will_online && st.shield_hp + 1e-3 >= max_before {
                if let Some(s) = snap.structures.get_mut(&structure_id) {
                    s.shield_hp = max_after;
                }
            }
            save_ext_state(snap, &ext);
        }
        Ok((structure_id, src.type_id, false, will_online, is_service))
    })?;
    let (structure_id, type_id, destroyed, is_online, service_changed) = res;
    if service_changed {
        ctx.notify(
            crate::router::NotificationTarget::Station(structure_id),
            "OnStructureServiceChanged",
            "stationid",
            vec![PyRep::Integer(structure_id), PyRep::Integer(i64::from(type_id)), PyRep::Bool(is_online)],
        );
    }
    if destroyed {
        return Ok(PyRep::keyval([("itemID", PyRep::Integer(item_id)), ("destroyed", PyRep::Bool(true))]));
    }
    Ok(PyRep::keyval([
        ("itemID", PyRep::Integer(item_id)),
        ("typeID", PyRep::Integer(i64::from(type_id))),
        ("locationID", PyRep::Integer(new_loc)),
        ("flagID", PyRep::Integer(flag as i64)),
        ("online", PyRep::Bool(is_online)),
    ]))
}

/// Onlines or offlines a module fitted to an Upwell structure (`feat:structures.service-modules`).
pub fn handle_structure_module_online(ctx: &ServerContext, session: &mut SessionState, structure_id: i64, module_id: i64, set_online: bool) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    life::sync_all_structures(ctx);
    let now = ctx.now_unix();
    let (type_id, is_service) = ctx.db.try_with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        let (st, profile) = life::validate_structure_refit(ctx, snap, &ext, structure_id, now)?;
        let caller_corp = snap.characters.get(&char_id).map(|c| c.corporation_id).or(session.corporation_id).unwrap_or(0);
        if st.owner_corp_id != caller_corp {
            return Err(err("StructureOwnershipViolation", format!("Character {char_id} does not belong to owning corp {} of structure {structure_id}", st.owner_corp_id)));
        }
        if !has_structure_manager_role(snap, session, char_id, caller_corp) {
            return Err(err("InsufficientCorpRoles", "Onlining or offlining structure modules requires Director or Station Manager role"));
        }
        let item = snap.items.get(&module_id).cloned().filter(|i| i.location_id == structure_id).ok_or_else(|| err("ModuleNotFound", format!("Module {module_id} not fitted to structure {structure_id}")))?;
        let is_service = (164..=171).contains(&(item.flag as u16));
        if set_online && is_service {
            let online_fuel = ctx.sde.types.get(&item.type_id).and_then(|t| t.default_attributes.get(&life::ATTR_SERVICE_FUEL_ONLINE)).copied().unwrap_or(0.0) as u32;
            if st.fuel_blocks < online_fuel {
                return Err(err("InsufficientStructureFuelToOnline", format!("Onlining service module {module_id} requires {online_fuel} fuel blocks (bay has {})", st.fuel_blocks)));
            }
        }
        let max_before = life::effective_max_shield(ctx, snap, &profile, structure_id, life::power_state(ctx, snap, &ext, &st, now));
        snap.module_online_states.insert(module_id, set_online);
        life::sync_structure(ctx, snap, &mut ext, structure_id, now);
        let power_after = life::power_state(ctx, snap, &ext, &st, now);
        let max_after = life::effective_max_shield(ctx, snap, &profile, structure_id, power_after);
        if set_online && st.shield_hp + 1e-3 >= max_before {
            if let Some(s) = snap.structures.get_mut(&structure_id) {
                s.shield_hp = max_after;
            }
        }
        save_ext_state(snap, &ext);
        Ok((item.type_id, is_service))
    })?;
    if is_service {
        ctx.notify(
            crate::router::NotificationTarget::Station(structure_id),
            "OnStructureServiceChanged",
            "stationid",
            vec![PyRep::Integer(structure_id), PyRep::Integer(i64::from(type_id)), PyRep::Bool(set_online)],
        );
    }
    Ok(PyRep::keyval([
        ("moduleID", PyRep::Integer(module_id)),
        ("structureID", PyRep::Integer(structure_id)),
        ("online", PyRep::Bool(set_online)),
    ]))
}

/// `structure.JettisonStructureFuel(itemIDs)` (`invItemFunctions.pyj:77`): validates that the
/// caller has Director or Station Manager (Fuel Technician) role in the structure's owning corp,
/// removes fuel blocks from the structure's fuel bay, and places them into a jettisoned cargo
/// container in space outside the structure.
pub async fn handle_jettison_structure_fuel(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let bound_struct_id = ctx
        .bound_objects
        .read()
        .ok()
        .and_then(|map| {
            map.values()
                .find(|b| b.session_id == session.session_id && b.service == "structure")
                .map(|b| b.object_id)
        });
    let mut explicit_item_ids = Vec::new();
    for a in args {
        match a {
            PyRep::List(list) | PyRep::Tuple(list) => {
                for v in list {
                    if let Some(id) = v.as_i64() {
                        explicit_item_ids.push(id);
                    }
                }
            }
            _ => {}
        }
    }
    let fuel_groups = life::service_fuel_groups(ctx);
    let (structure_id, sys_id, container_id, jettisoned_ids, remaining_fuel, pos) =
        ctx.db.try_with_mut_snapshot(|snap| {
            let mut st_id_opt = args
                .first()
                .and_then(|a| a.as_i64())
                .filter(|id| snap.structures.contains_key(id));
            if st_id_opt.is_none() {
                if let Some(&first_iid) = explicit_item_ids.first() {
                    if let Some(it) = snap.items.get(&first_iid) {
                        if snap.structures.contains_key(&it.location_id) {
                            st_id_opt = Some(it.location_id);
                        }
                    }
                } else if let Some(first_int) = args.first().and_then(|a| a.as_i64()) {
                    if let Some(it) = snap.items.get(&first_int) {
                        if snap.structures.contains_key(&it.location_id) {
                            explicit_item_ids.push(first_int);
                            st_id_opt = Some(it.location_id);
                        }
                    }
                }
            }
            let structure_id = st_id_opt
                .or(bound_struct_id)
                .or(session.structure_id)
                .ok_or_else(|| err("MissingStructureId", "JettisonStructureFuel requires an active or bound structureID"))?;
            let st = snap
                .structures
                .get(&structure_id)
                .cloned()
                .ok_or_else(|| err("StructureNotFound", format!("Structure {structure_id} not found")))?;
            if st.state == life::STATE_DESTROYED {
                return Err(err("StructureDestroyed", format!("Structure {structure_id} is destroyed")));
            }
            let caller_corp = snap
                .characters
                .get(&char_id)
                .map(|c| c.corporation_id)
                .or(session.corporation_id)
                .unwrap_or(0);
            if caller_corp != st.owner_corp_id {
                return Err(err("StructureOwnershipViolation", format!("Character {char_id} does not belong to structure owning corp {}", st.owner_corp_id)));
            }
            if session.structure_id != Some(structure_id)
                && session.ship_id != Some(structure_id)
                && session.solar_system_id != Some(st.solar_system_id)
            {
                return Err(err("NotDockedInStructure", format!("Must be docked in or in the same solar system as structure {structure_id} to jettison fuel")));
            }
            if !has_structure_manager_role(snap, session, char_id, caller_corp) {
                return Err(err("CorpRoleDenied", "Jettisoning structure fuel requires Director or Station Manager role"));
            }

            snap.next_entity_id += 1;
            let container_id = snap.next_entity_id;
            snap.items.insert(
                container_id,
                eve_db::InventoryItemRecord {
                    item_id: container_id,
                    type_id: 23, // Cargo Container
                    owner_id: st.owner_corp_id,
                    location_id: i64::from(st.solar_system_id),
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    dynamic_attributes: std::collections::HashMap::new(),
                    source_mutaplasmid_id: None,
                    insured_payout_isk: 0.0,
                    is_blueprint_copy: false,
                    blueprint_runs_remaining: None,
                },
            );

            let mut moved_ids = Vec::new();
            let mut moved_blocks = 0u32;
            if !explicit_item_ids.is_empty() {
                for iid in &explicit_item_ids {
                    let Some(it) = snap.items.get_mut(iid) else {
                        return Err(err("ItemNotFound", format!("Fuel item {iid} not found")));
                    };
                    if it.location_id != structure_id {
                        return Err(err("ItemNotInFuelBay", format!("Item {iid} is not located in structure {structure_id}")));
                    }
                    if it.owner_id != st.owner_corp_id {
                        return Err(err("ItemOwnershipViolation", format!("Item {iid} is not owned by structure owner corp {}", st.owner_corp_id)));
                    }
                    let is_fuel_type = ctx
                        .sde
                        .types
                        .get(&it.type_id)
                        .is_some_and(|t| fuel_groups.contains(&t.group_id));
                    let in_fuel_bay = it.flag == InventoryFlag::SpecializedFuelBay || it.flag as u16 == 163;
                    if !(in_fuel_bay && is_fuel_type) {
                        return Err(err("ItemNotFuel", format!("Item {iid} (type {}) is not structure fuel in fuel bay", it.type_id)));
                    }
                    let qty_u32 = u32::try_from(it.quantity.max(0)).unwrap_or(0);
                    moved_blocks = moved_blocks.saturating_add(qty_u32);
                    it.location_id = container_id;
                    it.flag = InventoryFlag::Cargo;
                    it.version += 1;
                    moved_ids.push(*iid);
                }
                if let Some(s) = snap.structures.get_mut(&structure_id) {
                    s.fuel_blocks = s.fuel_blocks.saturating_sub(moved_blocks);
                }
            } else {
                let (fuel_tid, req_qty) = if args.len() >= 3 {
                    let tid = args.get(1).and_then(|a| a.as_i64()).unwrap_or(4247);
                    let q = args.get(2).and_then(|a| a.as_i64()).unwrap_or(0);
                    (tid, q)
                } else if args.len() == 2 {
                    (4247i64, args.get(1).and_then(|a| a.as_i64()).unwrap_or(0))
                } else {
                    (4247i64, i64::from(st.fuel_blocks))
                };
                let fuel_tid_u32 = u32::try_from(fuel_tid).map_err(|_| {
                    err("InvalidArgument", format!("Invalid fuel typeID {fuel_tid}"))
                })?;
                let is_fuel_type = ctx
                    .sde
                    .types
                    .get(&fuel_tid_u32)
                    .is_some_and(|t| fuel_groups.contains(&t.group_id));
                if !is_fuel_type {
                    return Err(err("ItemNotFuel", format!("TypeID {fuel_tid} is not a valid structure fuel type")));
                }
                let req_qty_u32 = u32::try_from(req_qty).map_err(|_| {
                    err("InvalidArgument", format!("Invalid fuel quantity {req_qty}"))
                })?;
                if req_qty_u32 == 0 || req_qty_u32 > st.fuel_blocks {
                    return Err(err("InsufficientStructureFuel", format!("Cannot jettison {req_qty} fuel blocks from structure {structure_id} (available: {})", st.fuel_blocks)));
                }
                let qty_i32 = i32::try_from(req_qty_u32).unwrap_or(i32::MAX);
                if let Some(s) = snap.structures.get_mut(&structure_id) {
                    s.fuel_blocks -= req_qty_u32;
                }
                snap.next_entity_id += 1;
                let fuel_item_id = snap.next_entity_id;
                snap.items.insert(
                    fuel_item_id,
                    eve_db::InventoryItemRecord {
                        item_id: fuel_item_id,
                        type_id: fuel_tid_u32,
                        owner_id: st.owner_corp_id,
                        location_id: container_id,
                        flag: InventoryFlag::Cargo,
                        quantity: qty_i32,
                        singleton: false,
                        version: 1,
                        dynamic_attributes: std::collections::HashMap::new(),
                        source_mutaplasmid_id: None,
                        insured_payout_isk: 0.0,
                        is_blueprint_copy: false,
                        blueprint_runs_remaining: None,
                    },
                );
                moved_ids.push(fuel_item_id);
            }
            let rem = snap.structures.get(&structure_id).map(|s| s.fuel_blocks).unwrap_or(0);
            Ok((structure_id, st.solar_system_id, container_id, moved_ids, rem, st.position))
        })?;

    let ball = eve_destiny::Ball {
        id: container_id,
        type_id: 23,
        position: DVec3::from_array(pos) + DVec3::new(500.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 0.0,
        inertia: 1.0,
        mass: 1000.0,
        radius: 25.0,
        speed_fraction: 0.0,
        mode: eve_destiny::MovementMode::Stop,
    };
    let _ = ctx.systems.get_or_spawn(sys_id).spawn_ball(ball).await;
    Ok(PyRep::keyval([
        ("structureID", PyRep::Integer(structure_id)),
        ("containerID", PyRep::Integer(container_id)),
        ("jettisonedItemIDs", PyRep::List(jettisoned_ids.into_iter().map(PyRep::Integer).collect())),
        ("remainingFuelBlocks", PyRep::Integer(i64::from(remaining_fuel))),
    ]))
}
