//! `structure_lifecycle_rpc` — client-facing entry points of the Upwell lifecycle
//! (`structureDeployment.Anchor`, structure fitting, fuel bay jettison, and live combat integration),
//! built on top of [`super::structure_lifecycle`].
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

pub(crate) fn has_structure_manager_role(snap: &eve_db::WorldStateSnapshot, session: &SessionState, char_id: i64, corp_id: i64) -> bool {
    let req_roles = eve_db::corp_roles::ROLE_DIRECTOR | eve_db::corp_roles::ROLE_STATION_MANAGER;
    session.has_gm_role()
        || (session.corp_role & req_roles) != 0
        || snap.corporations.get(&corp_id).is_some_and(|c| c.ceo_id == char_id || c.member_roles.get(&char_id).is_some_and(|&r| (r & req_roles) != 0))
        || snap.corp_role_assignments.get(&(corp_id, char_id)).is_some_and(|a| (a.roles & req_roles) != 0)
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
    if type_id == crate::services::sov_gameplay::TYPE_MERCENARY_DEN {
        return crate::services::sov_gameplay::handle_anchor_mercenary_den_rpc(ctx, session, args).await;
    }
    if type_id == crate::services::sov_services::TYPE_ORBITAL_SKYHOOK {
        return crate::services::sov_gameplay::handle_anchor_skyhook_rpc(ctx, session, args).await;
    }
    if ctx.sde.types.get(&type_id).is_some_and(|t| t.category_id == 22 || t.group_id == 361 || t.effect_ids.contains(&649)) {
        if let Some(res) = Box::pin(crate::services::station_structure::handle(
            ctx, session, "posMgr", "AnchorStructure", args, &std::collections::HashMap::new(),
        )).await {
            return res;
        }
    }
    if type_id == 40340 {
        let palatine_exists = ctx.db.with_snapshot(|snap| snap.structures.values().any(|s| s.type_id == 40340 && s.state != "Destroyed"));
        if palatine_exists {
            return Err(err("PalatineKeepstarLimitReached", "Only one Upwell Palatine Keepstar (type 40340) can exist in the universe"));
        }
    }
    let sys_id = session.solar_system_id.ok_or_else(|| err("NoSolarSystem", "Session has no active solarSystemID"))?;
    let profile = life::structure_profile(ctx, type_id).ok_or_else(|| err("InvalidStructureType", format!("Type {type_id} is not an anchorable Upwell structure (category {})", life::CATEGORY_STRUCTURE)))?;
    let (_, is_thera, is_shattered, _) = crate::services::space_destiny::system_wormhole_profile(sys_id);
    if is_thera || is_shattered {
        return Err(err("AnchoringDisallowedInTheraOrShattered", format!("Anchoring structures or starbases is disallowed in Thera or shattered wormhole system {sys_id}")));
    }
    if sys_id == 30_100_000 {
        return Err(err("AnchoringDisallowedInZarzakh", "Capsuleer structure anchoring is prohibited in Zarzakh"));
    }
    if ctx.sde.solar_systems.get(&sys_id).map(|s| s.region_id) == Some(10_000_070) {
        return Err(err("AnchoringDisallowedInPochven", "Upwell structures cannot be anchored in Pochven"));
    }
    let sys_sec = ctx.sde.solar_systems.get(&sys_id).map(|s| s.security_status).unwrap_or(1.0);
    if type_id == 35840 && sys_sec >= 0.45 {
        return Err(err("CannotAnchorCynoBeaconInHighSec", format!("Pharolux Cyno Beacon (35840) cannot be anchored in high-security solar system {sys_id} (security {sys_sec:.2} >= 0.45)")));
    }
    if type_id == ANSIBLEX_TYPE_ID && !is_sovereign_nullsec_for_ansiblex(&ctx.sde, sys_id) {
        return Err(err("CannotAnchorAnsiblexOutsideSovNull", format!("Ansiblex Jump Gate ({ANSIBLEX_TYPE_ID}) can only be anchored in sovereign null-security space (system {sys_id})")));
    }
    if matches!(type_id, 35840 | 35841 | crate::services::sov_gameplay::TYPE_TENEBREX_CYNO_JAMMER) {
        crate::services::sov_gameplay::require_flex_sov_upgrade_online(ctx, sys_id, type_id)?;
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
        let default_hour = snap
            .corp_governance
            .get(&corp_id)
            .map(|g| u64::from(g.reinforce_hour))
            .unwrap_or((now / 3_600) % 24);
        let vuln = reinforce_window.unwrap_or((default_hour, ((now / 86_400) + 4) % 7));
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
    if let Some(st_rec) = ctx.db.with_snapshot(|snap| snap.structures.get(&structure_id).cloned()) {
        crate::services::fleet_social::fleet_social_scanning::emit_signal_tracker_structure_delta(ctx, sys_id, &[st_rec], &[]);
    }
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

/// Validates whether `target_ball_id` is an Upwell structure and, if so, whether `attacker_char_id`
/// may legally apply weapon damage to it right now (`Ok(false)` when `target_ball_id` is not an Upwell structure).
pub fn validate_upwell_structure_attack_precheck(
    ctx: &ServerContext,
    sys_id: u32,
    attacker_char_id: i64,
    target_ball_id: i64,
    now_unix: u64,
) -> Result<bool, BridgeRpcError> {
    if target_ball_id < 1_000_000_000_000
        && ctx.db.with_snapshot(|snap| !snap.structures.contains_key(&target_ball_id))
    {
        return Ok(false);
    }
    life::sync_all_structures(ctx);
    let sys_sec = ctx.sde.solar_systems.get(&sys_id).map(|s| s.security_status).unwrap_or(0.0);
    ctx.db.with_snapshot(|snap| {
        let Some(st) = snap.structures.get(&target_ball_id) else {
            return Ok(false);
        };
        if st.solar_system_id != sys_id {
            return Err(err(
                "StructureWrongSystem",
                format!("Structure {target_ball_id} is in system {}, not {sys_id}", st.solar_system_id),
            ));
        }
        if st.state == life::STATE_DESTROYED {
            return Err(err(
                "StructureAlreadyDestroyed",
                format!("Structure {target_ball_id} is already destroyed"),
            ));
        }
        if st.type_id == crate::services::sov_gameplay::TYPE_MERCENARY_DEN {
            let (reinf, _vuln_end, _) = snap.sov_runtime.mercenary_den_reinforcement.get(&target_ball_id).copied().unwrap_or((0, 0, 100));
            if reinf > 0 && now_unix < reinf {
                return Err(err("MercenaryDenReinforced", format!("Mercenary Den {target_ball_id} is reinforced until {reinf}")));
            }
            // If now_unix >= vuln_end, the 6h vulnerability window expired without den destruction;
            // weapon attacks are permitted and cycle the den back to ShieldVulnerable in apply_mercenary_den_weapon_damage.
            return Ok(true);
        }
        if st.type_id == crate::services::sov_services::TYPE_ORBITAL_SKYHOOK {
            if st.state == "ArmorReinforced" && now_unix < st.reinforced_until_unix {
                return Err(err("StructureInvulnerable", format!("Orbital Skyhook {target_ball_id} is reinforced")));
            }
            return Ok(true);
        }
        if matches!(st.type_id, 79172 | 78260) || ctx.sde.types.get(&st.type_id).is_some_and(|t| t.group_id == 4644) {
            let camp_id = snap.insurgency_runtime.campaigns.iter().find(|(_, c)| c.systems.contains(&sys_id)).map(|(&id, _)| id).unwrap_or(0);
            if let Some(camp) = snap.insurgency_runtime.campaigns.get(&camp_id) {
                let (phase, _) = crate::services::insurgency_services::get_campaign_phase_and_timer(camp, now_unix);
                if phase != crate::services::insurgency_services::PHASE_ACTIVE {
                    return Err(err("FobInvulnerableInPhase", format!("Pirate FOB is invulnerable during {phase} phase")));
                }
                let emp_mod = snap.insurgency_runtime.empire_ambition_modifier.get(&camp.pirate_faction_id).copied().unwrap_or(0).min(5);
                let supp_goal = usize::from(5 + emp_mod);
                let s5 = camp.systems.iter().filter(|sys| camp.suppression_points.get(sys).copied().unwrap_or(0) >= 1_000).count();
                let existing_reinf = snap.insurgency_runtime.fob_armor_reinforced_until.get(&camp_id).copied();
                if existing_reinf.is_none() && s5 < supp_goal {
                    return Err(err("SuppressionGoalNotReached", format!("Pirate FOB {target_ball_id} requires {supp_goal} Stage-5 suppressed systems before attack (currently {s5})")));
                }
                if let Some(until) = existing_reinf {
                    if now_unix < until {
                        return Err(err("FobArmorReinforced", format!("Pirate FOB {target_ball_id} armor reinforced until {until}")));
                    }
                }
            }
            if st.state == "ArmorReinforced" && now_unix < st.reinforced_until_unix {
                return Err(err("FobArmorReinforced", format!("Pirate FOB {target_ball_id} armor reinforced until {}", st.reinforced_until_unix)));
            }
            return Ok(true);
        }
        if let Some((_, _, site_fac)) = snap.fw_runtime.supply_sites.get(&target_ball_id).cloned() {
            let att_fac = snap.characters.get(&attacker_char_id).and_then(|c| c.war_faction_id).ok_or_else(|| err("NotEnlistedInMilitia", "Only militia-enlisted pilots can attack FW Supply Caches/Depots"))?;
            if att_fac == site_fac {
                return Err(err("CannotAttackFriendlyStructure", "Cannot attack your own militia's Supply Cache/Depot"));
            }
            return Ok(true);
        }
        if st.type_id == crate::services::fw_gameplay::TYPE_FW_IHUB {
            let occ = snap.fw_system_occupations.get(&sys_id).ok_or_else(|| err("SystemNotWarzone", format!("System {sys_id} is not an FW warzone system")))?;
            let att_fac = snap.characters.get(&attacker_char_id).and_then(|c| c.war_faction_id).ok_or_else(|| err("NotEnlistedInOpposingMilitia", "Only opposing militia pilots can attack an FW Infrastructure Hub"))?;
            if crate::services::fw_services::fw_opposing_faction(occ.occupier_faction_id) != Some(att_fac) {
                return Err(err("NotEnlistedInOpposingMilitia", format!("Pilot faction {att_fac} is not the opposing militia for occupier {}", occ.occupier_faction_id)));
            }
            let surr_vuln = snap.insurgency_runtime.surrender_ihub_vulnerable_unix.get(&sys_id).copied().is_some_and(|ts| now_unix >= ts);
            let contested_100 = snap.fw_runtime.ihub_vulnerable.get(&sys_id).copied().unwrap_or(false) || snap.system_states.get(&sys_id).is_some_and(|ss| ss.fw_contested_pct >= 100.0);
            if !contested_100 && !surr_vuln {
                return Err(err("IHubInvulnerable", format!("FW I-Hub in system {sys_id} is invulnerable until the system is 100% contested or in surrender vulnerability")));
            }
            return Ok(true);
        }
        if !matches!(
            st.state.as_str(),
            life::STATE_SHIELD_VULNERABLE
                | life::STATE_ARMOR_VULNERABLE
                | life::STATE_HULL_VULNERABLE
                | life::STATE_ANCHOR_VULNERABLE
                | life::STATE_NO_CORE
                | life::STATE_ONLINING
        ) {
            return Err(err(
                "StructureReinforcedInvulnerable",
                format!(
                    "Structure {target_ball_id} is invulnerable ({}) until {}",
                    st.state, st.reinforced_until_unix
                ),
            ));
        }
        if attacker_char_id > 0 {
            if let Some(ch) = snap.characters.get(&attacker_char_id) {
                let caller_corp = ch.corporation_id;
                if st.owner_corp_id == caller_corp {
                    return Err(err(
                        "CannotAttackFriendlyStructure",
                        "Cannot attack an Upwell structure owned by your own corporation",
                    ));
                }
                if sys_sec >= 0.45
                    && !crate::services::structure_acl_defense_rules::are_corps_at_war_in_snap(
                        snap,
                        caller_corp,
                        st.owner_corp_id,
                        now_unix,
                    )
                {
                    return Err(err(
                        "IllegalHighSecStructureAttack",
                        "Cannot attack an Upwell structure in high-security space without an active war declaration (24h warmup complete)",
                    ));
                }
            }
        }
        Ok(true)
    })
}

/// Routes live weapon, missile, fighter, drone, or superweapon damage against an Upwell structure
/// through [`life::apply_structure_damage_typed`], syncing ballpark `CombatState` and returning
/// `Some((applied_damage, destroyed, shield_ratio, armor_ratio, hull_ratio, became_invuln_or_destroyed))`.
pub async fn apply_live_combat_damage_to_upwell_structure(
    ctx: &ServerContext,
    sys_handle: &crate::system_actor::SystemHandle,
    sys_id: u32,
    attacker_char_id: i64,
    target_ball_id: i64,
    dmg: eve_destiny::DamageVector,
    cycle_secs: f64,
    now_unix: u64,
) -> Result<Option<(f64, bool, f64, f64, f64, bool)>, BridgeRpcError> {
    if !validate_upwell_structure_attack_precheck(ctx, sys_id, attacker_char_id, target_ball_id, now_unix)? {
        return Ok(None);
    }
    let (st_type, is_fw_site) = ctx.db.with_snapshot(|snap| {
        let st_type = snap.structures.get(&target_ball_id).map(|s| s.type_id);
        let is_fw_site = snap.fw_runtime.supply_sites.contains_key(&target_ball_id);
        (st_type, is_fw_site)
    });
    if st_type == Some(crate::services::sov_gameplay::TYPE_MERCENARY_DEN) {
        let _ = crate::services::sov_gameplay::apply_mercenary_den_weapon_damage(ctx, target_ball_id, dmg.total()).await?;
        let (dest, s_r, a_r, h_r) = ctx.db.with_snapshot(|snap| {
            if let Some(st) = snap.structures.get(&target_ball_id) {
                let dest = st.state == "Destroyed";
                let s_r = (st.shield_hp / 5_000.0).clamp(0.0, 1.0);
                let a_r = 1.0;
                let h_r = (st.hull_hp / 5_000.0).clamp(0.0, 1.0);
                (dest, s_r, a_r, h_r)
            } else {
                (true, 0.0, 0.0, 0.0)
            }
        });
        return Ok(Some((dmg.total(), dest, s_r, a_r, h_r, dest)));
    }
    if st_type == Some(crate::services::sov_services::TYPE_ORBITAL_SKYHOOK) {
        let _ = crate::services::sov_gameplay::sov_gameplay_skyhook::apply_skyhook_weapon_damage(ctx, target_ball_id, dmg.total())?;
        let (dest, s_r, a_r, h_r, inv) = ctx.db.with_snapshot(|snap| {
            if let Some(st) = snap.structures.get(&target_ball_id) {
                let dest = st.state == "Destroyed";
                let inv = dest || st.state == "ArmorReinforced";
                let s_r = (st.shield_hp / crate::services::sov_gameplay::sov_gameplay_skyhook::SKYHOOK_BASE_SHIELD_HP).clamp(0.0, 1.0);
                let a_r = 1.0;
                let h_r = 1.0;
                (dest, s_r, a_r, h_r, inv)
            } else {
                (true, 0.0, 0.0, 0.0, true)
            }
        });
        return Ok(Some((dmg.total(), dest, s_r, a_r, h_r, inv)));
    }
    if is_fw_site
        || matches!(
            st_type,
            Some(
                crate::services::insurgency_services::PIRATE_STRONGHOLD_ANGEL_TYPE
                    | crate::services::insurgency_services::PIRATE_STRONGHOLD_GURISTAS_TYPE
                    | crate::services::fw_gameplay::TYPE_FW_IHUB
            )
        )
    {
        let _ = crate::services::fw_gameplay::apply_fw_or_insurgency_structure_damage(ctx, attacker_char_id, target_ball_id, dmg.total()).await?;
        let (dest, s_r, a_r, h_r, inv) = ctx.db.with_snapshot(|snap| {
            if let Some(st) = snap.structures.get(&target_ball_id) {
                let dest = st.state == "Destroyed";
                let inv = dest || st.state == "ArmorReinforced";
                let s_r = (st.shield_hp / 1_000_000.0).clamp(0.0, 1.0);
                let a_r = (st.armor_hp / 1_000_000.0).clamp(0.0, 1.0);
                let h_r = (st.hull_hp / 1_000_000.0).clamp(0.0, 1.0);
                (dest, s_r, a_r, h_r, inv)
            } else {
                (true, 0.0, 0.0, 0.0, true)
            }
        });
        return Ok(Some((dmg.total(), dest, s_r, a_r, h_r, inv)));
    }
    let (out, sr, ar, hr) = ctx.db.try_with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        let out = life::apply_structure_damage_typed(
            ctx,
            snap,
            &mut ext,
            target_ball_id,
            dmg.em,
            dmg.thermal,
            dmg.kinetic,
            dmg.explosive,
            cycle_secs,
            attacker_char_id,
            now_unix,
        )?;
        let st_opt = snap.structures.get(&target_ball_id).cloned();
        let (max_s, max_a, max_h) = if let Some(st) = &st_opt {
            if let Some(p) = life::structure_profile(ctx, st.type_id) {
                let pwr = life::power_state(ctx, snap, &ext, st, now_unix);
                (
                    life::effective_max_shield(ctx, snap, &p, target_ball_id, pwr).max(1.0),
                    p.armor_hp.max(1.0),
                    p.hull_hp.max(1.0),
                )
            } else {
                (1.0, 1.0, 1.0)
            }
        } else {
            (1.0, 1.0, 1.0)
        };
        save_ext_state(snap, &ext);
        Ok((
            out.clone(),
            (out.shield_hp / max_s).clamp(0.0, 1.0),
            (out.armor_hp / max_a).clamp(0.0, 1.0),
            (out.hull_hp / max_h).clamp(0.0, 1.0),
        ))
    })?;
    if let Some(ref destr) = out.destruction {
        crate::services::fleet_social::fleet_social_scanning::dispatch_signal_tracker_notifications(ctx, destr.pending_signal_tracker_notifs.clone());
    }
    let destroyed = out.state == life::STATE_DESTROYED;
    let became_invuln = destroyed
        || matches!(
            out.state.as_str(),
            life::STATE_ANCHORING | life::STATE_ARMOR_REINFORCED | life::STATE_HULL_REINFORCED
        );
    let st_type_id = ctx.db.with_snapshot(|snap| snap.structures.get(&target_ball_id).map(|s| s.type_id)).unwrap_or(0);
    if st_type_id == crate::services::sov_services::TYPE_TENEBREX_CYNO_JAMMER
        && matches!(
            out.state.as_str(),
            life::STATE_ARMOR_VULNERABLE | life::STATE_ARMOR_REINFORCED | life::STATE_HULL_REINFORCED | life::STATE_DESTROYED
        )
    {
        ctx.notify(
            crate::router::NotificationTarget::SolarSystem(sys_id),
            "OnCynoJammerChanged",
            "solarsystemid",
            vec![PyRep::Integer(sys_id as i64), PyRep::Integer(0)],
        );
    }
    if !destroyed {
        if let Ok(mut bp) = sys_handle.ballpark.write() {
            if let Some(cs) = bp.combat_states.get_mut(&target_ball_id) {
                cs.shield_hp = out.shield_hp;
                cs.max_shield_hp = (out.shield_hp / sr.max(1e-6)).max(out.shield_hp).max(1.0);
                cs.armor_hp = out.armor_hp;
                cs.max_armor_hp = (out.armor_hp / ar.max(1e-6)).max(out.armor_hp).max(1.0);
                cs.structure_hp = out.hull_hp;
                cs.max_structure_hp = (out.hull_hp / hr.max(1e-6)).max(out.hull_hp).max(1.0);
            }
        }
    }
    Ok(Some((out.applied_damage, destroyed, sr, ar, hr, became_invuln)))
}

/// Applies live combat damage to `target_ball_id`, routing Upwell structures through
/// [`apply_live_combat_damage_to_upwell_structure`] and other balls through `apply_damage_with_attacker`.
pub async fn apply_damage_structure_or_ball(
    ctx: &ServerContext,
    sys_handle: &crate::system_actor::SystemHandle,
    sys_id: u32,
    attacker_char_id: i64,
    target_ball_id: i64,
    dmg: eve_destiny::DamageVector,
    cycle_secs: f64,
    now_unix: u64,
) -> Result<Option<(f64, bool, bool)>, BridgeRpcError> {
    if let Some((app, dest, _, _, _, inv)) = apply_live_combat_damage_to_upwell_structure(
        ctx,
        sys_handle,
        sys_id,
        attacker_char_id,
        target_ball_id,
        dmg,
        cycle_secs,
        now_unix,
    )
    .await?
    {
        return Ok(Some((app, dest, inv)));
    }
    let att_opt = (attacker_char_id > 0).then_some(attacker_char_id);
    Ok(sys_handle
        .apply_damage_with_attacker(target_ball_id, dmg, att_opt)
        .await
        .ok()
        .flatten()
        .map(|(a, d, _)| (a, d, d)))
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
pub fn handle_structure_fit_move(ctx: &ServerContext, session: &mut SessionState, item_id: i64, new_loc: i64, flag: InventoryFlag, explicit_online: Option<bool>) -> Result<PyRep, BridgeRpcError> {
    let online = explicit_online.unwrap_or(true);
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
        let st_pre = snap.structures.get(&structure_id).ok_or_else(|| err("StructureNotFound", format!("Structure {structure_id} not found")))?;
        let caller_corp = snap.characters.get(&char_id).map(|c| c.corporation_id).or(session.corporation_id).unwrap_or(0);
        if st_pre.owner_corp_id != caller_corp {
            return Err(err("StructureOwnershipViolation", format!("Character {char_id} does not belong to owning corp {} of structure {structure_id}", st_pre.owner_corp_id)));
        }
        if !has_structure_manager_role(snap, session, char_id, caller_corp) {
            return Err(err("InsufficientCorpRoles", "Fitting or unfitting structure modules requires Director or Station Manager role"));
        }
        let caller_sys = session.solar_system_id.or_else(|| snap.characters.get(&char_id).map(|c| c.solar_system_id)).unwrap_or(0);
        if (caller_sys != 0 && caller_sys != st_pre.solar_system_id && session.structure_id != Some(structure_id))
            || session.structure_id.is_some_and(|sid| sid != structure_id)
        {
            return Err(err("NotAtStructure", format!("Character {char_id} must be at structure {structure_id} to modify its fitting")));
        }
        let mut ext = load_ext_state(snap);
        let (st, profile) = life::validate_structure_refit(ctx, snap, &ext, structure_id, now)?;
        if !is_target_fit_slot {
            // Unfitting from structure
            if new_loc != structure_id && session.ship_id != Some(new_loc) {
                return Err(err("InvalidItemLocation", format!("Cannot unfit module into location {new_loc}")));
            }
            if matches!(src.flag as u16, 92..=94) {
                snap.items.remove(&item_id);
                snap.module_online_states.remove(&item_id);
                return Ok((structure_id, item_id, src.type_id, src.flag as i64, true, false, false));
            }
            let was_service = (164..=171).contains(&(src.flag as u16));
            if let Some(it) = snap.items.get_mut(&item_id) {
                it.location_id = new_loc;
                it.flag = flag;
                it.version += 1;
            }
            snap.module_online_states.remove(&item_id);
            if was_service {
                if let Some(lf) = ext.lifecycle.get_mut(&structure_id) {
                    lf.online_modules.retain(|&id| id != item_id);
                }
                life::sync_structure(ctx, snap, &mut ext, structure_id, now);
                if let Some(st_after) = snap.structures.get(&structure_id).cloned() {
                    let power_after = life::power_state(ctx, snap, &ext, &st_after, now);
                    let max_after = life::effective_max_shield(ctx, snap, &profile, structure_id, power_after);
                    if let Some(s) = snap.structures.get_mut(&structure_id) {
                        s.shield_hp = s.shield_hp.min(max_after);
                    }
                }
                save_ext_state(snap, &ext);
            }
            return Ok((structure_id, item_id, src.type_id, src.flag as i64, false, false, was_service));
        }
        if matches!(src.flag as u16, 92..=94) && src.location_id == structure_id && flag != src.flag {
            return Err(err("CannotMoveFittedRig", "Fitted structure rigs cannot be moved between slots without being destroyed"));
        }
        // Fitting into structure slot
        if src.owner_id != char_id && src.owner_id != st.owner_corp_id {
            return Err(err("ItemOwnershipViolation", format!("Character {char_id} does not own module {item_id}")));
        }
        if src.owner_id == st.owner_corp_id
            && !crate::services::mining_advanced::mining_compression::caller_can_take_corp_hangar_item(
                snap,
                char_id,
                caller_corp,
                src.flag,
                session.corp_role,
            )
        {
            return Err(err("CrpAccessDenied", "Caller lacks corporate hangar division take permission for this module"));
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
        use crate::services::structure_acl_defense_rules as rules;
        if is_service {
            rules::sync_seeded_structure_fuel_bay(ctx, snap, structure_id);
        }
        let avail_fuel = snap.structures.get(&structure_id).map(|s| s.fuel_blocks).unwrap_or(0);
        let online_fuel = rules::service_module_online_fuel_cost(ctx, st.type_id, src.type_id);
        if is_service && explicit_online == Some(true) {
            if profile.core_type_id.is_some() && !st.quantum_core_installed {
                return Err(err("QuantumCoreRequired", format!("Structure {structure_id} requires an installed Quantum Core to online service modules")));
            }
            if avail_fuel < online_fuel {
                return Err(err("InsufficientStructureFuelToOnline", format!("Onlining service module requires {online_fuel} fuel blocks (bay has {avail_fuel})")));
            }
        }
        let will_online = if online && is_service {
            (profile.core_type_id.is_none() || st.quantum_core_installed) && avail_fuel >= online_fuel
        } else {
            online && !matches!(flag_u16, 92..=94)
        };
        let fitted_id = if src.quantity > 1 {
            if let Some(it) = snap.items.get_mut(&item_id) {
                it.quantity -= 1;
                it.version += 1;
            }
            snap.next_entity_id += 1;
            let new_id = snap.next_entity_id;
            snap.items.insert(
                new_id,
                eve_db::InventoryItemRecord {
                    item_id: new_id,
                    type_id: src.type_id,
                    owner_id: src.owner_id,
                    location_id: structure_id,
                    flag,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            new_id
        } else {
            if let Some(it) = snap.items.get_mut(&item_id) {
                it.location_id = structure_id;
                it.flag = flag;
                it.singleton = true;
                it.version += 1;
            }
            item_id
        };
        snap.module_online_states.insert(fitted_id, will_online);
        if is_service {
            if will_online {
                let _ = rules::consume_structure_fuel_blocks(ctx, snap, structure_id, online_fuel);
                let lf = ext.lifecycle.entry(structure_id).or_default();
                lf.last_fuel_tick_unix.get_or_insert(now);
                if !lf.online_modules.contains(&fitted_id) {
                    lf.online_modules.push(fitted_id);
                }
            }
            let max_before = life::effective_max_shield(ctx, snap, &profile, structure_id, life::PowerState::LowPower);
            life::sync_structure(ctx, snap, &mut ext, structure_id, now);
            let st_after = snap.structures.get(&structure_id).cloned().unwrap_or(st.clone());
            let power_now = life::power_state(ctx, snap, &ext, &st_after, now);
            let max_after = life::effective_max_shield(ctx, snap, &profile, structure_id, power_now);
            if let Some(s) = snap.structures.get_mut(&structure_id) {
                if will_online && st.shield_hp + 1e-3 >= max_before {
                    s.shield_hp = max_after;
                } else {
                    s.shield_hp = s.shield_hp.min(max_after);
                }
            }
            save_ext_state(snap, &ext);
        }
        Ok((structure_id, fitted_id, src.type_id, src.flag as i64, false, will_online, is_service))
    })?;
    let (structure_id, fitted_id, type_id, old_flag, destroyed, is_online, service_changed) = res;
    if service_changed {
        ctx.notify(
            crate::router::NotificationTarget::Station(structure_id),
            "OnStructureServiceChanged",
            "stationid",
            vec![PyRep::Integer(structure_id)],
        );
    }
    if destroyed {
        let _ = ctx.notify_fitted_item_trashed(char_id, fitted_id, i64::from(type_id), 1, structure_id, old_flag);
        return Ok(PyRep::keyval([("itemID", PyRep::Integer(fitted_id)), ("destroyed", PyRep::Bool(true))]));
    }
    Ok(PyRep::keyval([
        ("itemID", PyRep::Integer(fitted_id)),
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
    let (_type_id, is_service) = ctx.db.try_with_mut_snapshot(|snap| {
        if set_online {
            if let Some(st0) = snap.structures.get(&structure_id) {
                let is_svc0 = snap.items.get(&module_id).is_some_and(|i| i.location_id == structure_id && (164..=171).contains(&(i.flag as u16)));
                if is_svc0 && !st0.quantum_core_installed && life::structure_profile(ctx, st0.type_id).is_some_and(|p| p.core_type_id.is_some()) {
                    return Err(err("QuantumCoreRequired", format!("Structure {structure_id} requires an installed Quantum Core to online service modules")));
                }
            }
        }
        let mut ext = load_ext_state(snap);
        let (st, profile) = life::validate_structure_refit(ctx, snap, &ext, structure_id, now)?;
        let caller_corp = snap.characters.get(&char_id).map(|c| c.corporation_id).or(session.corporation_id).unwrap_or(0);
        if st.owner_corp_id != caller_corp {
            return Err(err("StructureOwnershipViolation", format!("Character {char_id} does not belong to owning corp {} of structure {structure_id}", st.owner_corp_id)));
        }
        if !has_structure_manager_role(snap, session, char_id, caller_corp) {
            return Err(err("InsufficientCorpRoles", "Onlining or offlining structure modules requires Director or Station Manager role"));
        }
        let caller_sys = session.solar_system_id.or_else(|| snap.characters.get(&char_id).map(|c| c.solar_system_id)).unwrap_or(0);
        if (caller_sys != 0 && caller_sys != st.solar_system_id && session.structure_id != Some(structure_id))
            || session.structure_id.is_some_and(|sid| sid != structure_id)
        {
            return Err(err("NotAtStructure", format!("Character {char_id} must be at structure {structure_id} to online/offline modules")));
        }
        let item = snap.items.get(&module_id).cloned().filter(|i| i.location_id == structure_id).ok_or_else(|| err("ModuleNotFound", format!("Module {module_id} not fitted to structure {structure_id}")))?;
        let is_service = (164..=171).contains(&(item.flag as u16));
        use crate::services::structure_acl_defense_rules as rules;
        let was_online = snap.module_online_states.get(&module_id).copied().unwrap_or(false)
            && ext.lifecycle.get(&structure_id).is_some_and(|l| l.online_modules.contains(&module_id));
        if set_online && is_service {
            if profile.core_type_id.is_some() && !st.quantum_core_installed {
                return Err(err("QuantumCoreRequired", format!("Structure {structure_id} requires an installed Quantum Core to online service modules")));
            }
            if !was_online {
                rules::sync_seeded_structure_fuel_bay(ctx, snap, structure_id);
                let online_fuel = rules::service_module_online_fuel_cost(ctx, st.type_id, item.type_id);
                if !rules::consume_structure_fuel_blocks(ctx, snap, structure_id, online_fuel) {
                    let cur = snap.structures.get(&structure_id).map(|s| s.fuel_blocks).unwrap_or(0);
                    return Err(err("InsufficientStructureFuelToOnline", format!("Onlining service module {module_id} requires {online_fuel} fuel blocks (bay has {cur})")));
                }
                let lf = ext.lifecycle.entry(structure_id).or_default();
                lf.last_fuel_tick_unix.get_or_insert(now);
                lf.online_modules.push(module_id);
            }
        } else if !set_online && is_service {
            if let Some(lf) = ext.lifecycle.get_mut(&structure_id) {
                lf.online_modules.retain(|&id| id != module_id);
            }
        }
        let max_before = life::effective_max_shield(ctx, snap, &profile, structure_id, life::power_state(ctx, snap, &ext, &st, now));
        snap.module_online_states.insert(module_id, set_online);
        life::sync_structure(ctx, snap, &mut ext, structure_id, now);
        let st_after = snap.structures.get(&structure_id).cloned().unwrap_or(st.clone());
        let power_after = life::power_state(ctx, snap, &ext, &st_after, now);
        let max_after = life::effective_max_shield(ctx, snap, &profile, structure_id, power_after);
        if let Some(s) = snap.structures.get_mut(&structure_id) {
            if set_online && st.shield_hp + 1e-3 >= max_before {
                s.shield_hp = max_after;
            } else {
                s.shield_hp = s.shield_hp.min(max_after);
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
            vec![PyRep::Integer(structure_id)],
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
    use crate::services::structure_acl_defense_rules as rules;
    let (structure_id, sys_id, container_id, jettisoned_ids, remaining_fuel, pos) =
        ctx.db.try_with_mut_snapshot(|snap| {
            let mut st_id_opt = args.first().and_then(|a| a.as_i64()).filter(|id| snap.structures.contains_key(id));
            if st_id_opt.is_none() {
                if let Some(&first_iid) = explicit_item_ids.first() {
                    st_id_opt = snap.items.get(&first_iid).map(|it| it.location_id).filter(|id| snap.structures.contains_key(id));
                } else if let Some(first_int) = args.first().and_then(|a| a.as_i64()) {
                    if let Some(loc) = snap.items.get(&first_int).map(|it| it.location_id).filter(|id| snap.structures.contains_key(id)) {
                        explicit_item_ids.push(first_int);
                        st_id_opt = Some(loc);
                    }
                }
            }
            let structure_id = st_id_opt.or(bound_struct_id).or(session.structure_id)
                .ok_or_else(|| err("MissingStructureId", "JettisonStructureFuel requires an active or bound structureID"))?;
            rules::sync_seeded_structure_fuel_bay(ctx, snap, structure_id);
            let st = snap.structures.get(&structure_id).cloned()
                .ok_or_else(|| err("StructureNotFound", format!("Structure {structure_id} not found")))?;
            if st.state == life::STATE_DESTROYED {
                return Err(err("StructureDestroyed", format!("Structure {structure_id} is destroyed")));
            }
            let caller_corp = snap.characters.get(&char_id).map(|c| c.corporation_id).or(session.corporation_id).unwrap_or(0);
            if caller_corp != st.owner_corp_id {
                return Err(err("StructureOwnershipViolation", format!("Character {char_id} does not belong to structure owning corp {}", st.owner_corp_id)));
            }
            if session.structure_id != Some(structure_id) && session.ship_id != Some(structure_id) && session.solar_system_id != Some(st.solar_system_id) {
                return Err(err("NotDockedInStructure", format!("Must be docked in or in the same solar system as structure {structure_id} to jettison fuel")));
            }
            if !rules::has_structure_fuel_bay_role(snap, session, char_id, caller_corp) {
                return Err(err("InsufficientRoles", "Jettisoning structure fuel requires Director, Station Manager, or Fuel Technician role"));
            }
            snap.next_entity_id += 1;
            let container_id = snap.next_entity_id;
            snap.items.insert(container_id, eve_db::InventoryItemRecord {
                item_id: container_id, type_id: 23, owner_id: st.owner_corp_id,
                location_id: i64::from(st.solar_system_id), flag: InventoryFlag::None,
                quantity: 1, singleton: true, version: 1, ..Default::default()
            });
            let mut moved_ids = Vec::new();
            let mut moved_blocks = 0u32;
            if !explicit_item_ids.is_empty() {
                for iid in &explicit_item_ids {
                    let it = snap.items.get_mut(iid).ok_or_else(|| err("ItemNotFound", format!("Fuel item {iid} not found")))?;
                    if it.location_id != structure_id {
                        return Err(err("ItemNotInFuelBay", format!("Item {iid} is not located in structure {structure_id}")));
                    }
                    if it.owner_id != st.owner_corp_id {
                        return Err(err("ItemOwnershipViolation", format!("Item {iid} is not owned by structure owner corp {}", st.owner_corp_id)));
                    }
                    let in_fuel_bay = it.flag == InventoryFlag::SpecializedFuelBay || it.flag == InventoryFlag::StructureFuel || it.flag as u16 == 172 || it.flag as u16 == 163;
                    if !in_fuel_bay || !rules::is_valid_structure_fuel_bay_item(ctx, it.type_id) {
                        return Err(err("ItemNotFuel", format!("Item {iid} (type {}) is not structure fuel in fuel bay", it.type_id)));
                    }
                    if rules::is_fuel_block_type(ctx, it.type_id) {
                        moved_blocks = moved_blocks.saturating_add(it.quantity.max(0) as u32);
                    }
                    it.location_id = container_id;
                    it.flag = InventoryFlag::Cargo;
                    it.version += 1;
                    moved_ids.push(*iid);
                }
                if let Some(s) = snap.structures.get_mut(&structure_id) {
                    s.fuel_blocks = s.fuel_blocks.saturating_sub(moved_blocks);
                }
            } else {
                let default_tid = snap.items.values()
                    .filter(|i| i.location_id == structure_id && rules::is_structure_fuel_flag(i.flag) && i.quantity > 0 && rules::is_fuel_block_type(ctx, i.type_id))
                    .min_by_key(|i| i.item_id).map(|i| i64::from(i.type_id)).unwrap_or(4247);
                let (fuel_tid, req_qty) = if args.len() >= 3 {
                    (args.get(1).and_then(|a| a.as_i64()).unwrap_or(default_tid), args.get(2).and_then(|a| a.as_i64()).unwrap_or(0))
                } else if args.len() == 2 {
                    (default_tid, args.get(1).and_then(|a| a.as_i64()).unwrap_or(0))
                } else {
                    (default_tid, i64::from(st.fuel_blocks))
                };
                let fuel_tid_u32 = u32::try_from(fuel_tid).map_err(|_| err("InvalidArgument", format!("Invalid fuel typeID {fuel_tid}")))?;
                if !rules::is_valid_structure_fuel_bay_item(ctx, fuel_tid_u32) {
                    return Err(err("ItemNotFuel", format!("TypeID {fuel_tid} is not a valid structure fuel type")));
                }
                let req_qty_u32 = u32::try_from(req_qty).map_err(|_| err("InvalidArgument", format!("Invalid fuel quantity {req_qty}")))?;
                let mut bay_matching: Vec<i64> = snap.items.values()
                    .filter(|i| i.location_id == structure_id && rules::is_structure_fuel_flag(i.flag) && i.type_id == fuel_tid_u32 && i.quantity > 0)
                    .map(|i| i.item_id).collect();
                bay_matching.sort_unstable();
                let matching_qty: u32 = bay_matching.iter().filter_map(|id| snap.items.get(id)).map(|i| i.quantity.max(0) as u32).fold(0, u32::saturating_add);
                let is_block = rules::is_fuel_block_type(ctx, fuel_tid_u32);
                let avail_for_tid = if is_block {
                    let backed_all: u32 = snap.items.values()
                        .filter(|i| i.location_id == structure_id && rules::is_structure_fuel_flag(i.flag) && i.quantity > 0 && rules::is_fuel_block_type(ctx, i.type_id))
                        .map(|i| i.quantity as u32).fold(0, u32::saturating_add);
                    matching_qty.saturating_add(st.fuel_blocks.saturating_sub(backed_all)).min(st.fuel_blocks)
                } else {
                    matching_qty
                };
                if req_qty_u32 == 0 || req_qty_u32 > avail_for_tid {
                    return Err(err("InsufficientStructureFuel", format!("Cannot jettison {req_qty} of type {fuel_tid_u32} from structure {structure_id} (available: {avail_for_tid})")));
                }
                let mut rem = req_qty_u32;
                for iid in bay_matching {
                    if rem == 0 { break; }
                    let remove_it = if let Some(it) = snap.items.get_mut(&iid) {
                        let take = rem.min(it.quantity.max(0) as u32);
                        it.quantity -= take as i32;
                        it.version += 1;
                        rem -= take;
                        it.quantity <= 0
                    } else { false };
                    if remove_it { snap.items.remove(&iid); }
                }
                if is_block {
                    if let Some(s) = snap.structures.get_mut(&structure_id) {
                        s.fuel_blocks = s.fuel_blocks.saturating_sub(req_qty_u32);
                    }
                }
                snap.next_entity_id += 1;
                let fuel_item_id = snap.next_entity_id;
                snap.items.insert(fuel_item_id, eve_db::InventoryItemRecord {
                    item_id: fuel_item_id, type_id: fuel_tid_u32, owner_id: st.owner_corp_id,
                    location_id: container_id, flag: InventoryFlag::Cargo,
                    quantity: i32::try_from(req_qty_u32).unwrap_or(i32::MAX), singleton: false, version: 1, ..Default::default()
                });
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

pub(crate) use super::structure_ansiblex_navigation::*;

const CARGO_DROPOFF_EXTRA_RANGE_M: f64 = 2_500.0;

pub(crate) fn handle_structure_deliver(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let structure_id = args.first().and_then(|a| a.as_i64()).ok_or_else(|| err("MissingStructureId", "structureDeliveries.Deliver requires structureID"))?;
    let item_id = args.get(1).and_then(|a| a.as_i64()).ok_or_else(|| err("MissingItemId", "structureDeliveries.Deliver requires itemID"))?;
    let recipient_id = args.get(2).and_then(|a| a.as_i64()).ok_or_else(|| err("MissingRecipientId", "structureDeliveries.Deliver requires recipient characterID"))?;
    if session.structure_id != Some(structure_id) {
        return Err(err("NotDockedInStructure", format!("Must be docked in structure {structure_id} to deliver items")));
    }
    if session.ship_id == Some(item_id) {
        return Err(err("CannotDeliverActiveShip", "Cannot deliver your active boarded ship"));
    }
    ctx.db.try_with_mut_snapshot(|snap| {
        if !snap.characters.contains_key(&recipient_id) {
            return Err(err("RecipientNotFound", format!("Recipient character {recipient_id} not found")));
        }
        let Some(it) = snap.items.get(&item_id) else {
            return Err(err("ItemNotFound", format!("Item {item_id} not found")));
        };
        if it.owner_id != char_id || it.location_id != structure_id || it.flag != InventoryFlag::Hangar {
            return Err(err("ItemNotInStructureHangar", format!("Item {item_id} must be owned by {char_id} inside structure {structure_id}")));
        }
        if snap.is_item_locked_in_contract_or_job(item_id) {
            return Err(err("ItemLocked", format!("Item {item_id} is locked in a contract or job")));
        }
        if snap.items.values().any(|child| child.location_id == item_id) {
            return Err(err("CannotDeliverNonEmptyContainer", format!("Item {item_id} contains nested items and must be repackaged or emptied before delivery")));
        }
        if let Some(it) = snap.items.get_mut(&item_id) {
            it.owner_id = recipient_id;
            it.flag = InventoryFlag::Deliveries;
            it.version += 1;
        }
        Ok(PyRep::None)
    })
}

pub(crate) async fn handle_cargo_dropoff(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    if session.station_id.is_some() || session.structure_id.is_some() {
        return Err(err("MustBeInSpace", "structureCargoDelivery.DropOffItems requires a ship in space"));
    }
    let ship_id = session.ship_id.ok_or_else(|| err("NoActiveShip", "No active ship in space"))?;
    let structure_id = args.first().and_then(|a| a.as_i64()).ok_or_else(|| err("MissingStructureId", "DropOffItems requires structureID"))?;
    let item_ids: Vec<i64> = match args.get(1) {
        Some(PyRep::List(items)) | Some(PyRep::Tuple(items)) => items.iter().filter_map(|x| x.as_i64()).collect(),
        Some(PyRep::Integer(id)) => vec![*id],
        _ => Vec::new(),
    };
    if item_ids.is_empty() { return Err(err("NoItemsSpecified", "DropOffItems requires at least one itemID")); }
    let sys_id = session.solar_system_id.unwrap_or(0);
    let (st_pos, st_type) = ctx.db.with_snapshot(|snap| snap.structures.get(&structure_id).filter(|s| s.solar_system_id == sys_id && s.state != "Destroyed").map(|s| (DVec3::from_array(s.position), s.type_id)))
        .ok_or_else(|| err("StructureNotFound", format!("Structure {structure_id} not in solar system {sys_id}")))?;
    let st_radius = ctx.sde.types.get(&st_type).map(|t| t.radius).unwrap_or(0.0);
    let (_, ship_ball) = ctx.systems.get_or_spawn(sys_id).get_ball_snapshot(ship_id).await.map_err(|e| err("DestinyQueryFailed", e))?.ok_or_else(|| err("ShipNotInSpace", "Active ship not in space"))?;
    if ship_ball.position.distance(st_pos) > st_radius + CARGO_DROPOFF_EXTRA_RANGE_M {
        return Err(err("OutsideCargoDropoffRange", "Ship is too far from the structure to drop off cargo"));
    }
    ctx.db.try_with_mut_snapshot(|snap| {
        for &iid in &item_ids {
            let valid_cargo = snap.items.get(&iid).is_some_and(|it| {
                it.owner_id == char_id
                    && it.location_id == ship_id
                    && matches!(
                        it.flag,
                        InventoryFlag::Cargo
                            | InventoryFlag::SpecializedOreHold
                            | InventoryFlag::SpecializedGasHold
                            | InventoryFlag::SpecializedIceHold
                            | InventoryFlag::SpecializedMineralHold
                            | InventoryFlag::SpecializedAmmoHold
                            | InventoryFlag::SpecializedFuelBay
                    )
            });
            if !valid_cargo {
                return Err(err("ItemNotInShipCargo", format!("Item {iid} is not in active ship {ship_id} cargo")));
            }
            if snap.is_item_locked_in_contract_or_job(iid) {
                return Err(err("ItemLocked", format!("Item {iid} is locked in a contract or job")));
            }
        }
        for &iid in &item_ids {
            if let Some(it) = snap.items.get_mut(&iid) { it.location_id = structure_id; it.flag = InventoryFlag::Deliveries; it.version += 1; }
        }
        Ok(PyRep::None)
    })
}

pub(crate) use crate::services::station_structure_handlers::{handle_get_structure_info, handle_get_structures};
