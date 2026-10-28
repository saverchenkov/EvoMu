//! `structure_combat_attack` — live weapon attack resolution against Upwell structures,
//! Mercenary Dens, Orbital Skyhooks, and FW/Insurgency structures (`structureControl.AttackStructure`).
#![forbid(unsafe_code)]

use glam::DVec3;
use eve_db::WorldStateSnapshot;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use crate::router::ServerContext;
use crate::services::station_structure::structure_lifecycle as life;
use crate::services::station_structure::structure_upwell_ops::{load_ext_state, save_ext_state};

fn err(code: &str, message: impl Into<String>) -> BridgeRpcError {
    BridgeRpcError {
        code: code.into(),
        message: message.into(),
    }
}

/// Online high-slot weapons with a loaded, skill-usable charge: `(raw dps, shortest cycle, charge items)`.
pub(crate) fn attacker_weapon_volley(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    char_id: i64,
    ship_id: i64,
    weapons: &[(u16, u32)],
) -> Result<(f64, f64, Vec<i64>), BridgeRpcError> {
    let mut total_dps = 0.0_f64;
    let mut min_cycle = f64::MAX;
    let mut charges = Vec::with_capacity(weapons.len());
    let mut skill_err = None;
    for &(flag_id, wtid) in weapons {
        let Some(wt) = ctx.sde.types.get(&wtid) else { continue };
        let loaded = snap
            .items
            .values()
            .find(|i| {
                i.owner_id == char_id
                    && i.location_id == ship_id
                    && (i.flag as u16) == flag_id
                    && i.quantity > 0
                    && ctx
                        .sde
                        .types
                        .get(&i.type_id)
                        .map(|ct| ServerContext::is_charge_compatible_with_weapon(wt, ct))
                        .unwrap_or(false)
            })
            .map(|i| (i.item_id, i.type_id));
        let Some((cid, ctid)) = loaded else { continue };
        if let Err(e) = ctx.check_type_skill_prerequisites_in_snapshot(snap, char_id, ctid) {
            skill_err.get_or_insert(e);
            continue;
        }
        let Some(ct) = ctx.sde.types.get(&ctid) else { continue };
        let cycle = (wt.default_attributes.get(&51).copied().unwrap_or(3000.0) / 1000.0).max(1.0);
        let mult = wt.default_attributes.get(&64).copied().unwrap_or(1.0);
        let volley = [114u16, 116, 117, 118]
            .iter()
            .map(|a| ct.default_attributes.get(a).copied().unwrap_or(0.0))
            .sum::<f64>()
            * mult;
        if volley > 0.0 {
            total_dps += volley / cycle;
            min_cycle = min_cycle.min(cycle);
            charges.push(cid);
        }
    }
    if total_dps <= 0.0 || charges.is_empty() {
        return Err(skill_err.unwrap_or_else(|| {
            err(
                "NoAmmoLoaded",
                format!("Active ship {ship_id} has fitted high-slot weapons but no compatible charge loaded (flags 27..=34)"),
            )
        }));
    }
    Ok((total_dps, if min_cycle.is_finite() { min_cycle } else { 3.0 }, charges))
}

/// `structureControl.AttackStructure(structureID)`: one weapon cycle of the attacker's fitted
/// weapons against the structure, resolved by [`life::apply_structure_damage`].
pub async fn handle_attack(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    if session.station_id.is_some() || session.structure_id.is_some() {
        return Err(err("ShipDocked", "Cannot attack an Upwell structure while docked"));
    }
    let ship_id = session
        .ship_id
        .ok_or_else(|| err("NoActiveShip", "Session has no active ship"))?;
    let structure_id = args
        .first()
        .and_then(|a| a.as_i64())
        .ok_or_else(|| err("MissingStructureId", "AttackStructure requires an explicit structureID argument"))?;
    let caller_sys = session.solar_system_id.unwrap_or(0);
    // Siege DPS comes from the fitting engine's online high-slot weapons, never from args.
    let fit = ctx.get_or_rebuild_ship_fitting(char_id, ship_id)?;
    let mut weapons: Vec<(u16, u32)> = fit
        .modules
        .values()
        .filter(|m| {
            (27..=34).contains(&m.flag_id)
                && m.state != eve_dogma::ModuleState::Offline
                && ctx
                    .sde
                    .types
                    .get(&m.type_id)
                    .map(|t| matches!(t.group_id, 53 | 55 | 74 | 506..=511 | 1986 | 4060 | 4500 | 4805 | 4807))
                    .unwrap_or(false)
        })
        .map(|m| (m.flag_id, m.type_id))
        .collect();
    weapons.sort_unstable();
    if weapons.is_empty() {
        return Err(err(
            "NoWeaponFitted",
            format!("Active ship {ship_id} has no fitted high-slot weapon"),
        ));
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
    enum AttackTarget {
        Special(u32, bool, f64, u64, Vec<i64>),
        Upwell(u32, life::DamageOutcome, bool),
    }
    let target = ctx.db.try_with_mut_snapshot(|snap: &mut WorldStateSnapshot| {
        let (raw_dps, cycle, charges) = attacker_weapon_volley(ctx, snap, char_id, ship_id, &weapons)?;
        let caller_corp = snap.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(0);
        let st = snap
            .structures
            .get(&structure_id)
            .cloned()
            .ok_or_else(|| err("StructureNotFound", format!("Structure {structure_id} does not exist")))?;
        if st.solar_system_id != caller_sys {
            return Err(err(
                "StructureWrongSystem",
                format!("Structure {structure_id} is in system {}, but caller is in {caller_sys}", st.solar_system_id),
            ));
        }
        if st.owner_corp_id == caller_corp {
            return Err(err(
                "CannotAttackFriendlyStructure",
                "Cannot attack a structure owned by your own corporation",
            ));
        }
        let is_fw_ins = crate::services::fw_gameplay::is_fw_or_insurgency_structure_in_snap(
            snap,
            &ctx.sde,
            structure_id,
            st.type_id,
        );
        let corr_stage5 = snap
            .system_states
            .get(&caller_sys)
            .is_some_and(|ss| ss.insurgency_corruption_stage >= 5);
        let at_war = snap.wars.values().any(|w| {
            w.active
                && now >= w.started_unix.saturating_add(86_400)
                && ((w.aggressor_corp_id == caller_corp && w.defender_corp_id == st.owner_corp_id)
                    || (w.aggressor_corp_id == st.owner_corp_id && w.defender_corp_id == caller_corp))
        });
        if sys_sec >= 0.45 && !at_war && !is_fw_ins && !corr_stage5 {
            return Err(err(
                "IllegalHighSecStructureAttack",
                "Cannot attack an Upwell structure in high-security space without an active war declaration (24h warmup complete)",
            ));
        }
        let cycle_ticks = (cycle.ceil() as u64).max(1);
        let tick = current_tick.max(1);
        if st.last_damage_tick > 0 && tick < st.last_damage_tick.saturating_add(cycle_ticks) {
            return Err(err(
                "WeaponCycling",
                format!(
                    "Weapons are cycling against structure {structure_id} until tick {}",
                    st.last_damage_tick.saturating_add(cycle_ticks)
                ),
            ));
        }
        let st_radius = ctx.sde.types.get(&st.type_id).map(|t| t.radius).unwrap_or(0.0);
        let dist = (ship_ball.position.distance(DVec3::from_array(st.position)) - st_radius - ship_ball.radius).max(0.0);
        if dist > 50_000.0 {
            return Err(err(
                "StructureOutOfRange",
                format!("Ship is {dist:.1} m from structure {structure_id} (must be <= 50000 m)"),
            ));
        }
        let volley_dmg = raw_dps * cycle;
        if st.type_id == crate::services::sov_gameplay::TYPE_MERCENARY_DEN
            || st.type_id == crate::services::sov_services::TYPE_ORBITAL_SKYHOOK
            || is_fw_ins
        {
            return Ok(AttackTarget::Special(st.type_id, is_fw_ins, volley_dmg, tick, charges));
        }
        let mut ext = load_ext_state(snap);
        let out = life::apply_structure_damage(ctx, snap, &mut ext, structure_id, volley_dmg, cycle, char_id, now)?;
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
        Ok(AttackTarget::Upwell(st.type_id, out, sys_sec >= 0.45 && !at_war && corr_stage5))
    })?;
    let outcome = match target {
        AttackTarget::Special(st_tid, is_fw, dmg, tick, charges) => {
            return crate::services::sov_gameplay::dispatch_special_structure_attack(
                ctx, char_id, structure_id, st_tid, is_fw, dmg, tick, &charges,
            )
            .await;
        }
        AttackTarget::Upwell(st_tid, out, hs_stage5_suspect) => {
            crate::services::sov_gameplay::handle_normal_structure_damage_aftermath(
                ctx, caller_sys, sys_sec, char_id, ship_id, st_tid, &out.state, hs_stage5_suspect, now,
            );
            out
        }
    };
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
