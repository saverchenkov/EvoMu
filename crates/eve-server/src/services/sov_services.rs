//! `sov_services` — Sovereignty Management, Equinox Skyhooks & Dev Indices (`§3.8`).

#![forbid(unsafe_code)]

use crate::router::NotificationTarget;
use crate::router::ServerContext;
pub use eve_db::PersistedSovState as SovRuntimeState;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;

/// SDE Type ID for Orbital Skyhook.
pub const TYPE_ORBITAL_SKYHOOK: u32 = 81080;
/// SDE Type ID for Sovereignty Hub.
pub const TYPE_SOVEREIGNTY_HUB: u32 = 32458;
/// SDE Type ID for Pharolux Cyno Beacon.
pub const TYPE_PHAROLUX_CYNO_BEACON: u32 = 35840;
pub const TYPE_TENEBREX_CYNO_JAMMER: u32 = 37534;

/// Reagents produced by Skyhooks: Magmatic Gas (81143) and Superionic Ice (81144).
pub const TYPE_MAGMATIC_GAS: u32 = 81143;
pub const TYPE_SUPERIONIC_ICE: u32 = 81144;

pub fn with_sov_runtime<R>(ctx: &ServerContext, f: impl FnOnce(&mut SovRuntimeState) -> R) -> R {
    ctx.db.with_mut_snapshot(|snap| f(&mut snap.sov_runtime))
}

pub fn read_sov_runtime<R>(ctx: &ServerContext, f: impl FnOnce(&SovRuntimeState) -> R) -> R {
    ctx.db.with_snapshot(|snap| f(&snap.sov_runtime))
}

/// Computes the Activity Defense Multiplier (ADM) from military, industrial, and strategic indices.
///
/// Formula: ADM = 1.0 + mil_bonus + ind_bonus + strat_bonus, clamped between 1.0 and 6.0.
pub fn calculate_adm(mil_idx: u8, ind_idx: u8, strat_idx: u8) -> f64 {
    const MIL_IND_BONUS: [f64; 6] = [0.0, 0.6, 1.2, 1.7, 2.1, 2.5];
    const STRAT_BONUS: [f64; 6] = [0.0, 0.4, 0.6, 0.8, 0.9, 1.0];
    let mil_b = MIL_IND_BONUS[usize::from(mil_idx.min(5))];
    let ind_b = MIL_IND_BONUS[usize::from(ind_idx.min(5))];
    let strat_b = STRAT_BONUS[usize::from(strat_idx.min(5))];
    ((1.0 + mil_b + ind_b + strat_b).clamp(1.0, 6.0) * 10.0).round() / 10.0
}

/// Scales base entosis capture duration by ADM.
pub fn scale_entosis_capture_seconds(base_seconds: u64, adm: f64) -> u64 {
    ((base_seconds as f64) * adm.max(1.0)).round() as u64
}

/// Broadcasts `OnSovereigntyChanged(solarSystemID, allianceID)` (exact V24 2-arg signature)
/// to all sessions and queues destiny event for the solar system.
pub fn emit_sovereignty_changed(ctx: &ServerContext, sys_id: u32, alliance_id: Option<i64>, _corp_id: Option<i64>) {
    ctx.invalidate_cached_method("stationSvc", "GetAllianceSystems");
    ctx.emit_typed(
        NotificationTarget::All,
        crate::router::notify_registry::OnSovereigntyChanged {
            solar_system_id: i64::from(sys_id),
            alliance_id,
        },
    );
    let handle = ctx.systems.get_or_spawn(sys_id);
    let aid = alliance_id.unwrap_or(0);
    tokio::spawn(async move {
        let _ = handle.emit_destiny_event(0, eve_destiny::DestinyEvent::OnSovereigntyChanged {
            solar_system_id: sys_id,
            alliance_id: aid,
        }).await;
    });
}

/// Resolves a Sovereignty Hub structure ID from either a direct `hub_id`, a `solarSystemID` argument,
/// or the caller's current `session.solar_system_id`.
pub(crate) fn resolve_sov_hub_id(
    snap: &eve_db::WorldStateSnapshot,
    arg_id_or_sys: Option<i64>,
    session_sys: Option<u32>,
) -> Option<i64> {
    if let Some(raw) = arg_id_or_sys {
        if snap.structures.contains_key(&raw) {
            return Some(raw);
        }
        if let Ok(sys_id) = u32::try_from(raw) {
            if let Some((&hid, _)) = snap
                .structures
                .iter()
                .find(|(_, s)| s.solar_system_id == sys_id && s.type_id == TYPE_SOVEREIGNTY_HUB && s.state != "Destroyed")
            {
                return Some(hid);
            }
        }
        return Some(raw);
    }
    let sys_id = session_sys?;
    snap.structures
        .iter()
        .find(|(_, s)| s.solar_system_id == sys_id && s.type_id == TYPE_SOVEREIGNTY_HUB && s.state != "Destroyed")
        .map(|(&hid, _)| hid)
}

/// Evaluates whether `(char_id, corp_id, alliance_id)` is permitted to access fuel on `hub_id`.
///
/// When `fuel_access_groups[&hub_id]` references an existing [`AccessGroupRecord`] in
/// `StructureExtendedState.owner_groups`, membership is strictly evaluated with character > corp >
/// alliance precedence (`ACL_ROLE_BLOCKED = -1` denies). When no fuel ACL is configured (`0`),
/// owning-corp (or, for `allow_alliance_mgr_when_unset`, owning-alliance) `ROLE_DIRECTOR | ROLE_STATION_MANAGER`
/// / CEO is required.
pub(crate) fn can_access_sov_hub_fuel(
    snap: &eve_db::WorldStateSnapshot,
    hub_id: i64,
    char_id: i64,
    corp_id: i64,
    alliance_id: Option<i64>,
    extra_roles: u64,
    allow_alliance_mgr_when_unset: bool,
) -> bool {
    let Some(st) = snap
        .structures
        .get(&hub_id)
        .filter(|s| s.type_id == TYPE_SOVEREIGNTY_HUB && s.state != "Destroyed")
    else {
        return false;
    };
    let group_id = snap.sov_runtime.fuel_access_groups.get(&hub_id).copied().unwrap_or(0);
    if group_id > 0 {
        let ext = crate::services::station_structure::structure_upwell_ops::load_ext_state(snap);
        if let Some(grp) = ext.owner_groups.get(&group_id) {
            use crate::services::structure_acl_defense_rules::{
                is_acl_creator_active_for_corp, is_acl_role_allowed,
            };
            if let Some(&r) = grp.members.get(&char_id) {
                return is_acl_role_allowed(r)
                    && (grp.owner_corp_id == 0
                        || char_id != grp.creator_id
                        || corp_id == grp.owner_corp_id);
            }
            if is_acl_creator_active_for_corp(char_id, corp_id, grp) {
                return true;
            }
            if let Some(&r) = grp.members.get(&corp_id) {
                return is_acl_role_allowed(r);
            }
            if let Some(aid) = alliance_id.filter(|&a| a > 0) {
                if let Some(&r) = grp.members.get(&aid) {
                    return is_acl_role_allowed(r);
                }
            }
            return false;
        }
    }
    let (is_ceo, eff_roles) =
        crate::services::social_corp_fleet::social_corp_projects::effective_corp_member_roles(
            snap, corp_id, char_id, 0, 0,
        );
    let combined = eff_roles | extra_roles;
    let has_mgr = is_ceo
        || (combined
            & (eve_db::corp_roles::ROLE_DIRECTOR | eve_db::corp_roles::ROLE_STATION_MANAGER))
            != 0;
    if !has_mgr {
        return false;
    }
    if st.owner_corp_id == corp_id {
        return true;
    }
    if group_id == 0 && allow_alliance_mgr_when_unset {
        let hub_ally = snap.corporations.get(&st.owner_corp_id).and_then(|c| c.alliance_id);
        return alliance_id.is_some() && alliance_id == hub_ally;
    }
    false
}

pub fn emit_sov_structures_updated_with_snap(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    sys_id: u32,
    changed_ids: &[i64],
) {
    let structs = collect_sov_structures_from_snap(snap, sys_id);
    let changes = PyRep::Dict(
        changed_ids
            .iter()
            .copied()
            .map(|id| (PyRep::Integer(id), PyRep::List(vec![PyRep::Integer(0)])))
            .collect(),
    );
    ctx.emit_typed(
        NotificationTarget::All,
        crate::router::notify_registry::OnSolarSystemSovStructuresUpdated {
            solar_system_id: i64::from(sys_id),
            solar_system_structures_info: PyRep::List(structs),
            changes,
        },
    );
}

/// Broadcasts `OnSolarSystemSovStructuresUpdated(solarsystemID, solarSystemStructuresInfo, changes)`
/// matching V24 `SovService.OnSolarSystemSovStructuresUpdated` (3 positional arguments: `solarsystemID`,
/// list of `util.KeyVal` structure rows, and `dict` mapping `sourceItemID -> list` of change tags).
pub fn emit_sov_structures_updated(ctx: &ServerContext, sys_id: u32, changed_ids: &[i64]) {
    ctx.db.with_snapshot(|snap| {
        emit_sov_structures_updated_with_snap(ctx, snap, sys_id, changed_ids);
    });
}

/// Broadcasts `OnSolarSystemDevIndexChanged` notification (single solarsystemID payload matching V24 client expectation).
pub fn emit_dev_index_changed(ctx: &ServerContext, sys_id: u32) {
    ctx.emit_typed(
        NotificationTarget::SolarSystem(sys_id),
        crate::router::notify_registry::OnSolarSystemDevIndexChanged {
            solar_system_id: i64::from(sys_id),
        },
    );
}

/// Marks a Skyhook as hackable and emits `OnSkyhookCanBeHacked`.
pub fn emit_skyhook_can_be_hacked(ctx: &ServerContext, sys_id: u32, skyhook_id: i64) {
    ctx.emit_typed(
        NotificationTarget::SolarSystem(sys_id),
        crate::router::notify_registry::OnSkyhookCanBeHacked {
            solar_system_id: i64::from(sys_id),
            skyhook_id,
        },
    );
}

pub fn set_skyhook_hackable(ctx: &ServerContext, sys_id: u32, skyhook_id: i64, can_hack: bool) {
    with_sov_runtime(ctx, |rt| {
        rt.skyhook_hackable.insert(skyhook_id, can_hack);
        if can_hack {
            rt.skyhook_reagents.entry(skyhook_id).or_insert((TYPE_MAGMATIC_GAS, 500));
        }
    });
    if can_hack {
        emit_skyhook_can_be_hacked(ctx, sys_id, skyhook_id);
    }
}

/// Sets the reagent type and quantity for a Skyhook in the runtime state.
pub fn set_skyhook_reagents(ctx: &ServerContext, skyhook_id: i64, reagent_type_id: u32, quantity: u64) {
    with_sov_runtime(ctx, |rt| {
        rt.skyhook_reagents.insert(skyhook_id, (reagent_type_id, quantity));
    });
}

/// Records a successful hack on a Sovereignty Hub, temporarily disabling upgrades and emitting `OnSovHubHacked`.
pub fn record_hub_hack(ctx: &ServerContext, sys_id: u32, hub_id: i64, disruption_seconds: u64) {
    let now = ctx.now_unix();
    let until = now + disruption_seconds;
    ctx.db.with_mut_snapshot(|snap| {
        snap.sov_runtime.hacked_hubs.insert(hub_id, until);
        if let Some(hub) = snap.sov_runtime.sov_hubs.get_mut(&hub_id) {
            for u in &mut hub.upgrades {
                if u.online {
                    u.low_power = true;
                }
            }
        }
        if let Some(st) = snap.structures.get_mut(&hub_id) {
            if st.state == "ShieldVulnerable" {
                st.state = "ArmorReinforced".into();
            }
            if st.reinforced_until_unix == 0 {
                st.reinforced_until_unix = until;
            }
        }
        snap.sov_campaign_scores.entry(hub_id).or_insert(
            eve_db::snapshot_ext::SovCampaignScoreRecord {
                structure_id: hub_id,
                attackers_score: 0.35,
                defender_score: 0.65,
            },
        );
    });
    let upg_reps: Vec<PyRep> = ctx.db.with_snapshot(|snap| {
        snap.sov_runtime
            .sov_hubs
            .get(&hub_id)
            .map(|h| h.upgrades.iter().map(|u| PyRep::Integer(u.type_id as i64)).collect())
            .unwrap_or_default()
    });
    ctx.emit_typed(
        NotificationTarget::SolarSystem(sys_id),
        crate::router::notify_registry::OnSovHubHacked {
            sov_hub_id: hub_id,
            upgrades: upg_reps,
        },
    );
}

/// Checks whether a Sovereignty Hub is currently disrupted by a hack.
pub fn is_hub_hacked(ctx: &ServerContext, hub_id: i64) -> bool {
    let now = ctx.now_unix();
    read_sov_runtime(ctx, |rt| {
        rt.hacked_hubs.get(&hub_id).copied().is_some_and(|until| now < until)
    })
}

/// Claims sovereignty in `sys_id` for `alliance_id` with `hub_id`.
pub fn claim_system_sovereignty(
    ctx: &ServerContext,
    sys_id: u32,
    alliance_id: i64,
    corp_id: i64,
    hub_id: i64,
) -> Result<(), BridgeRpcError> {
    let now = ctx.now_unix();
    ctx.db.try_with_mut_snapshot(|snap| {
        let Some(st) = snap.system_states.get_mut(&sys_id) else {
            return Err(BridgeRpcError {
                code: "SystemNotFound".into(),
                message: format!("System {sys_id} not initialized"),
                client_error: None,
            });
        };
        if let Some(existing) = st.sov_alliance_id {
            if existing != alliance_id {
                return Err(BridgeRpcError {
                    code: "SovereigntyHeldByAnotherAlliance".into(),
                    message: format!("System {sys_id} sovereignty is held by alliance {existing}"),
                    client_error: None,
                });
            }
        }
        st.solar_system_id = sys_id;
        st.sov_alliance_id = Some(alliance_id);
        snap.sov_runtime.sov_claimed_since_unix.entry(sys_id).or_insert(now);
        Ok(())
    })?;

    emit_sovereignty_changed(ctx, sys_id, Some(alliance_id), Some(corp_id));
    emit_sov_structures_updated(ctx, sys_id, &[hub_id]);
    ctx.emit_typed_with_id_type(
        NotificationTarget::SolarSystem(sys_id),
        "solarsystemid",
        crate::router::notify_registry::OnCapitalSystemChanged {
            change: PyRep::Tuple(vec![
                PyRep::Integer(alliance_id),
                PyRep::None,
                PyRep::Integer(i64::from(sys_id)),
            ]),
        },
    );
    ctx.emit_typed(
        NotificationTarget::SolarSystem(sys_id),
        crate::router::notify_registry::OnSovereigntyAudioEvent {
            event_id: 1,
            text_params: PyRep::Dict(vec![
                (PyRep::String("solarSystemID".into()), PyRep::Integer(i64::from(sys_id))),
                (PyRep::String("allianceID".into()), PyRep::Integer(alliance_id)),
            ]),
        },
    );
    ctx.emit_typed(
        NotificationTarget::SolarSystem(sys_id),
        crate::router::notify_registry::ProcessSovStatusChanged {
            solar_system_id: i64::from(sys_id),
            sov_info: PyRep::keyval([
                ("solarSystemID", PyRep::Integer(i64::from(sys_id))),
                ("allianceID", PyRep::Integer(alliance_id)),
                ("corporationID", PyRep::Integer(corp_id)),
            ]),
        },
    );
    Ok(())
}

/// Claims sovereignty in `sys_id` and yields to allow destiny event emission.
pub async fn claim_system_sovereignty_async(
    ctx: &ServerContext,
    sys_id: u32,
    alliance_id: i64,
    corp_id: i64,
    hub_id: i64,
) -> Result<(), BridgeRpcError> {
    claim_system_sovereignty(ctx, sys_id, alliance_id, corp_id, hub_id)?;
    tokio::task::yield_now().await;
    Ok(())
}

pub fn collect_sov_structures_from_snap(snap: &eve_db::WorldStateSnapshot, sys_id: u32) -> Vec<PyRep> {
    let aid = snap.system_states.get(&sys_id).and_then(|s| s.sov_alliance_id);
    snap.structures
        .values()
        .filter(|s| s.solar_system_id == sys_id && !s.state.eq_ignore_ascii_case("destroyed"))
        .map(|s| {
            let vuln = snap
                .structure_vulnerability_windows
                .get(&s.structure_id)
                .copied()
                .unwrap_or((0, 0));
            PyRep::keyval([
                ("itemID", PyRep::Integer(s.structure_id)),
                ("structureID", PyRep::Integer(s.structure_id)),
                ("typeID", PyRep::Integer(s.type_id as i64)),
                ("corporationID", PyRep::Integer(s.owner_corp_id)),
                ("allianceID", aid.map(PyRep::Integer).unwrap_or(PyRep::None)),
                ("solarSystemID", PyRep::Integer(sys_id as i64)),
                ("vulnerableStartTime", PyRep::Integer(vuln.0 as i64)),
                ("vulnerableEndTime", PyRep::Integer(vuln.1 as i64)),
            ])
        })
        .collect()
}



pub async fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("sovMgr", "SetSovHubFuelAccessGroup") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let arg0 = args.first().and_then(|a| a.as_i64());
            let Some(hub_id) = ctx.db.with_snapshot(|snap| resolve_sov_hub_id(snap, arg0, session.solar_system_id)) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArguments".into(),
                    message: "SetSovHubFuelAccessGroup requires hubID or solarSystemID".into(),
                    client_error: None,
                }));
            };
            let access_group_id = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0);

            let now_unix = ctx.now_unix();
            if let Some(cid) = session.corporation_id.filter(|&id| id > 0) {
                crate::services::fleet_social::fleet_social_corps::promote_due_pending_corp_grants(ctx, cid);
            }
            let is_corp_director = ctx.db.with_snapshot(|snap| {
                let corp_id = snap.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(0);
                if snap.corporations.contains_key(&corp_id) {
                    let (is_ceo, eff_roles) =
                        crate::services::social_corp_fleet::social_corp_projects::effective_corp_member_roles(
                            snap, corp_id, char_id, 0, now_unix,
                        );
                    let has_role = is_ceo
                        || (eff_roles
                            & (eve_db::corp_roles::ROLE_DIRECTOR
                                | eve_db::corp_roles::ROLE_STATION_MANAGER))
                            != 0;
                    if !has_role {
                        return false;
                    }
                } else {
                    return false;
                }
                snap.structures
                    .get(&hub_id)
                    .map(|st| st.owner_corp_id == corp_id)
                    .unwrap_or(false)
            });
            if !is_corp_director {
                return Some(Err(BridgeRpcError {
                    code: "CorpRoleDenied".into(),
                    message: "Only owner corporation directors or station managers may set hub fuel access group".into(),
                    client_error: None,
                }));
            }

            with_sov_runtime(ctx, |rt| {
                rt.fuel_access_groups.insert(hub_id, access_group_id);
            });
            Some(Ok(PyRep::Integer(access_group_id)))
        }

        ("sovMgr", "GetSovHubFuelAccessGroup") => {
            let (_, _char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let arg0 = args.first().and_then(|a| a.as_i64());
            let Some(hub_id) = ctx.db.with_snapshot(|snap| resolve_sov_hub_id(snap, arg0, session.solar_system_id)) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArguments".into(),
                    message: "GetSovHubFuelAccessGroup requires hubID or solarSystemID".into(),
                    client_error: None,
                }));
            };
            let group_id = read_sov_runtime(ctx, |rt| rt.fuel_access_groups.get(&hub_id).copied().unwrap_or(0));
            Some(Ok(PyRep::Integer(group_id)))
        }

        ("sovMgr", "IsOnLocalSovHubFuelAccessGroup") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let arg0 = args.first().and_then(|a| a.as_i64());
            let allowed = ctx.db.with_snapshot(|snap| {
                let Some(hub_id) = resolve_sov_hub_id(snap, arg0, session.solar_system_id) else {
                    return false;
                };
                let char_corp = snap.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(0);
                let char_alliance = snap
                    .characters
                    .get(&char_id)
                    .and_then(|c| c.alliance_id)
                    .or_else(|| snap.corporations.get(&char_corp).and_then(|c| c.alliance_id));
                let effective_roles = effective_contract_helper_corp_roles(
                    snap,
                    char_corp,
                    char_id,
                    session.corp_role,
                    ctx.now_unix(),
                )
                .map(|(_, r)| r)
                .unwrap_or_else(|| {
                    snap.effective_corp_roles_at(char_corp, char_id, None, 0, ctx.now_unix()).1
                });
                can_access_sov_hub_fuel(
                    snap,
                    hub_id,
                    char_id,
                    char_corp,
                    char_alliance,
                    effective_roles,
                    false,
                )
            });
            Some(Ok(PyRep::Bool(allowed)))
        }

        ("sovMgr", "ConfirmSkyhookSovManagement") => {
            Some(crate::services::sov_gameplay::handle_confirm_skyhook_sov_management(ctx, session, args).await)
        }

        ("sovMgr", "DestroySkyhooks") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(sys_id) = args
                .first()
                .and_then(|a| a.as_i64())
                .and_then(|v| u32::try_from(v).ok())
                .or(session.solar_system_id)
            else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "DestroySkyhooks requires a valid solarSystemID".into(),
                    client_error: None,
                }));
            };

            let now_unix = ctx.now_unix();
            let is_corp_director = ctx.db.with_snapshot(|snap| {
                let corp_id = snap.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(0);
                if snap.corporations.contains_key(&corp_id) {
                    let (is_ceo, role) = snap.effective_corp_roles_at(
                        corp_id,
                        char_id,
                        None,
                        session.corp_role,
                        now_unix,
                    );
                    is_ceo
                        || (role
                            & (eve_db::corp_roles::ROLE_DIRECTOR
                                | eve_db::corp_roles::ROLE_STATION_MANAGER))
                            != 0
                } else {
                    false
                }
            });
            if !is_corp_director {
                return Some(Err(BridgeRpcError {
                    code: "CorpRoleDenied".into(),
                    message: "Only owner corporation directors or station managers may destroy skyhooks".into(),
                    client_error: None,
                }));
            }

            let now_unix = ctx.now_unix();
            let destroy_res = ctx.db.try_with_mut_snapshot(|snap| {
                let char_corp = snap.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(0);
                let targets: Vec<(i64, String, f64, f64, f64, u64)> = snap
                    .structures
                    .values()
                    .filter(|s| {
                        s.solar_system_id == sys_id
                            && s.type_id == TYPE_ORBITAL_SKYHOOK
                            && s.owner_corp_id == char_corp
                    })
                    .map(|s| {
                        (
                            s.structure_id,
                            s.state.clone(),
                            s.shield_hp,
                            s.armor_hp,
                            s.hull_hp,
                            s.reinforced_until_unix,
                        )
                    })
                    .collect();
                for (sid, ref state, shield_hp, armor_hp, hull_hp, reinf_until) in &targets {
                    if snap.sov_runtime.skyhook_silo_links.contains_key(sid) {
                        return Err(BridgeRpcError {
                            code: "ActiveReagentHeist".into(),
                            message: format!(
                                "Cannot destroy Skyhook {sid} while a reagent siphon link is active"
                            ),
                            client_error: None,
                        });
                    }
                    let damaged_hp = (*shield_hp > 0.0 && *shield_hp < 4_999.0)
                        || (*armor_hp > 0.0 && *armor_hp < 4_999.0)
                        || (*hull_hp > 0.0 && *hull_hp < 4_999.0);
                    if matches!(
                        state.as_str(),
                        "Reinforced"
                            | "ArmorReinforce"
                            | "ArmorReinforced"
                            | "ArmorVulnerable"
                            | "HullReinforce"
                            | "HullReinforced"
                            | "HullVulnerable"
                    ) || *reinf_until > now_unix
                        || damaged_hp
                    {
                        return Err(BridgeRpcError {
                            code: "StructureReinforcedOrDamaged".into(),
                            message: format!(
                                "Cannot destroy Skyhook {sid} while reinforced or damaged (state={state})"
                            ),
                            client_error: None,
                        });
                    }
                }
                let mut skyhook_ids = Vec::with_capacity(targets.len());
                let mut removed_den_ids = Vec::new();
                for (sid, _, _, _, _, _) in targets {
                    snap.structures.remove(&sid);
                    snap.sov_runtime.skyhook_planets.remove(&sid);
                    snap.sov_runtime.skyhook_reagent_bays.remove(&sid);
                    snap.sov_runtime.skyhook_reagents.remove(&sid);
                    snap.sov_runtime.skyhook_active.remove(&sid);
                    snap.sov_runtime.skyhook_theft_windows.remove(&sid);
                    snap.sov_runtime.skyhook_maturity_rem.remove(&sid);
                    snap.sov_runtime.skyhook_last_harvest_unix.remove(&sid);
                    snap.sov_runtime.skyhook_hackable.remove(&sid);
                    let den_ids: Vec<i64> = snap
                        .sov_runtime
                        .mercenary_dens
                        .values()
                        .filter(|md| md.skyhook_id == sid)
                        .map(|md| md.den_id)
                        .collect();
                    for did in den_ids {
                        snap.sov_runtime.mercenary_dens.remove(&did);
                        snap.sov_runtime.mercenary_den_reinforcement.remove(&did);
                        snap.sov_runtime.mercenary_den_tax_pct.remove(&did);
                        snap.sov_runtime.mercenary_den_evo_rem.remove(&did);
                        removed_den_ids.push(did);
                    }
                    skyhook_ids.push(sid);
                }
                Ok::<_, BridgeRpcError>((skyhook_ids, removed_den_ids))
            });
            let (destroyed_ids, removed_den_ids) = match destroy_res {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if !destroyed_ids.is_empty() {
                let sys_handle = ctx.systems.get_or_spawn(sys_id);
                for &sid in &destroyed_ids {
                    let _ = sys_handle.remove_ball(sid).await;
                }
                for &did in &removed_den_ids {
                    let _ = sys_handle.remove_ball(did).await;
                }
            }

            emit_sov_structures_updated(ctx, sys_id, &destroyed_ids);
            Some(Ok(PyRep::Integer(destroyed_ids.len() as i64)))
        }

        ("colonyResourcesMgr", "GetMyCorporationSkyhooks") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let skyhooks = ctx.db.with_snapshot(|snap| {
                let corp_id = snap.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(0);
                snap.structures
                    .values()
                    .filter(|s| s.type_id == TYPE_ORBITAL_SKYHOOK && s.owner_corp_id == corp_id)
                    .map(|s| {
                        let reagents = snap
                            .sov_runtime
                            .skyhook_reagents
                            .get(&s.structure_id)
                            .copied()
                            .unwrap_or((0, 0));
                        PyRep::keyval([
                            ("skyhookID", PyRep::Integer(s.structure_id)),
                            ("structureID", PyRep::Integer(s.structure_id)),
                            ("solarSystemID", PyRep::Integer(s.solar_system_id as i64)),
                            ("corporationID", PyRep::Integer(s.owner_corp_id)),
                            ("typeID", PyRep::Integer(s.type_id as i64)),
                            ("state", PyRep::String(s.state.clone())),
                            ("reagentTypeID", PyRep::Integer(i64::from(reagents.0))),
                            ("reagentQuantity", PyRep::Integer(reagents.1 as i64)),
                        ])
                    })
                    .collect::<Vec<PyRep>>()
            });
            Some(Ok(PyRep::List(skyhooks)))
        }

        ("devIndexManager", "GetAllDevelopmentIndices") => {
            let res = ctx.db.with_snapshot(|snap| {
                let mut map = Vec::new();
                for (&sys_id, st) in &snap.system_states {
                    if st.sov_alliance_id.is_some() || st.military_index > 0 || st.industrial_index > 0 {
                        let indices = PyRep::keyval([
                            ("solarSystemID", PyRep::Integer(i64::from(sys_id))),
                            ("military", PyRep::Integer(i64::from(st.military_index))),
                            ("industrial", PyRep::Integer(i64::from(st.industrial_index))),
                            ("strategic", PyRep::Integer(i64::from(st.strategic_index))),
                            ("militaryPoints", PyRep::Integer(i64::from(st.military_index) * 1_000)),
                            ("industrialPoints", PyRep::Integer(i64::from(st.industrial_index) * 1_000)),
                            ("claimedFor", PyRep::Integer(i64::from(st.strategic_index))),
                            ("adm", PyRep::Real(calculate_adm(st.military_index, st.industrial_index, st.strategic_index))),
                        ]);
                        map.push((PyRep::Integer(i64::from(sys_id)), indices));
                    }
                }
                PyRep::Dict(map)
            });
            Some(Ok(res))
        }

        ("devIndexManager", "GetDevelopmentIndicesForSystem") => {
            let _ = match ServerContext::require_auth(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let raw_sys = args
                .first()
                .and_then(|a| a.as_i64())
                .or_else(|| session.solar_system_id.map(i64::from))
                .unwrap_or(0);
            let Some(sys_id) = u32::try_from(raw_sys)
                .ok()
                .filter(|id| ctx.sde.solar_systems.contains_key(id))
            else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidSolarSystem".into(),
                    message: format!("Solar system {raw_sys} does not exist"),
                    client_error: None,
                }));
            };
            let res = ctx.db.with_snapshot(|snap| {
                let st = snap.system_states.get(&sys_id);
                let mil_lvl = st.map(|s| i64::from(s.military_index)).unwrap_or(0);
                let ind_lvl = st.map(|s| i64::from(s.industrial_index)).unwrap_or(0);
                let strat_lvl = st.map(|s| i64::from(s.strategic_index)).unwrap_or(0);
                let mil_pts = mil_lvl * 20_000;
                let ind_pts = ind_lvl * 20_000;
                let strat_pts = strat_lvl * 20_000;
                let inc = i64::from(st.and_then(|s| s.sov_alliance_id).is_some());
                let mk = |attr_id: i64, pts: i64, lvl: i64| {
                    (
                        PyRep::Integer(attr_id),
                        PyRep::keyval([
                            ("attributeID", PyRep::Integer(attr_id)),
                            ("points", PyRep::Integer(pts)),
                            ("level", PyRep::Integer(lvl)),
                            ("increasing", PyRep::Integer(inc)),
                        ]),
                    )
                };
                PyRep::Dict(vec![
                    mk(1583, mil_pts, mil_lvl),
                    mk(1584, ind_pts, ind_lvl),
                    mk(1615, strat_pts, strat_lvl),
                ])
            });
            Some(Ok(res))
        }

        ("warpVectorMgr", "UseVector") => {
            Some(handle_use_warp_vector(ctx, session, args).await)
        }

        ("map", "GetCurrentSovData") => {
            let sov_data = ctx.db.with_snapshot(|snap| {
                let list = snap
                    .system_states
                    .iter()
                    .filter_map(|(&sys_id, st)| {
                        st.sov_alliance_id.map(|aid| {
                            let hub_id = snap
                                .structures
                                .values()
                                .find(|s| {
                                    s.solar_system_id == sys_id
                                        && (s.type_id == TYPE_SOVEREIGNTY_HUB || s.type_id == 32226)
                                })
                                .map(|s| s.structure_id);
                            let corp_id = snap
                                .alliances
                                .get(&aid)
                                .map(|al| al.executor_corp_id)
                                .unwrap_or(0);
                            PyRep::keyval([
                                ("solarSystemID", PyRep::Integer(i64::from(sys_id))),
                                ("allianceID", PyRep::Integer(aid)),
                                ("corporationID", PyRep::Integer(corp_id)),
                                ("infrastructureHubID", hub_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
                                ("stationCount", PyRep::Integer(0)),
                            ])
                        })
                    })
                    .collect::<Vec<PyRep>>();
                list
            });
            Some(Ok(PyRep::List(sov_data)))
        }

        ("beyonce", "GetCynoJammerState") => {
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "Session has no active solar system".into(),
                    client_error: None,
                }));
            };
            Some(Ok(
                crate::services::inventory_fitting::fitting_cyno_portal::cyno_jammer_state_ticks(ctx, sys_id)
                    .map(PyRep::Integer)
                    .unwrap_or(PyRep::None),
            ))
        }

        _ => None,
    }
}

/// `warpVectorMgr.UseVector(itemID)` (`menuFunctions.pyj:586`): validates that the character is
/// undocked in space and applies the directional warp/jump vector to the active ship ball.
pub async fn handle_use_warp_vector(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    if session.station_id.is_none() && session.structure_id.is_none() {
        if let Some(ship_id) = session.ship_id {
            crate::services::structure_control_ops::reject_if_controlling_structure(
                ctx,
                ship_id,
                "activate warp vector",
            )?;
        }
    }
    crate::services::nav_warp_interdiction::handle_use_warp_vector(ctx, session, args).await
}

/// `jumpCloneSvc.ResetLastCloneJumpTime()` (`insider.pyj:2142`): resets the character's
/// `last_clone_jump_unix` timestamp to `0` in `WorldStateSnapshot` and emits `OnCloneJumpUpdate`.
pub fn handle_reset_last_clone_jump_time(
    ctx: &ServerContext,
    session: &SessionState,
) -> Result<PyRep, BridgeRpcError> {
    if !session.has_gm_role() {
        return Err(BridgeRpcError {
            code: "GMRoleRequired".into(),
            message: "jumpCloneSvc.ResetLastCloneJumpTime requires GM role".into(),
            client_error: None,
        });
    }
    let (_, char_id) = ServerContext::require_character(session)?;
    ctx.db.try_with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).ok_or_else(|| BridgeRpcError {
            code: "CharacterNotFound".into(),
            message: format!("Character {char_id} not found"),
            client_error: None,
        })?;
        ch.last_clone_jump_unix = 0;
        ch.version += 1;
        Ok(())
    })?;
    ctx.emit_typed(
        crate::router::NotificationTarget::Character(char_id),
        crate::router::notify_registry::OnCloneJumpUpdate {},
    );
    Ok(PyRep::Integer(0))
}

const DISMISSED_CONTRACT_NOTIFS_BLOB: &str = "contract_dismissed_notifs";

/// Returns `true` if `corp_id` has any DB-backed membership, role, title, or governance state.
///
/// Only minimal synthetic test corporations that insert a bare `CorporationRecord` with no
/// members/roles/titles/governance tables at all return `false`, ensuring a demoted or expelled
/// director can never fall back to a stale `session.corp_role`.
pub(crate) fn corp_has_db_role_table(
    snap: &eve_db::WorldStateSnapshot,
    corp_id: i64,
) -> bool {
    let Some(corp) = snap.corporations.get(&corp_id) else {
        return false;
    };
    !corp.member_roles.is_empty()
        || !corp.members.is_empty()
        || snap
            .corp_role_assignments
            .keys()
            .any(|&(cid, _)| cid == corp_id)
        || snap
            .corp_member_titles
            .keys()
            .any(|&(cid, _)| cid == corp_id)
        || snap
            .corp_titles
            .get(&corp_id)
            .is_some_and(|titles| !titles.is_empty())
        || snap.corp_governance.get(&corp_id).is_some_and(|g| {
            !g.member_titles.is_empty()
                || !g.pending_role_grants.is_empty()
                || !g.pending_scoped_role_grants.is_empty()
                || !g.pending_title_role_grants.is_empty()
                || !g.role_drop_timestamps.is_empty()
                || !g.role_audit_log.is_empty()
        })
}

fn effective_contract_helper_corp_roles(
    snap: &eve_db::WorldStateSnapshot,
    corp_id: i64,
    char_id: i64,
    session_role: u64,
    now_unix: u64,
) -> Option<(bool, u64)> {
    let _corp = snap.corporations.get(&corp_id)?;
    let (is_ceo, mut r) = snap.effective_corp_roles_at(
        corp_id,
        char_id,
        None,
        session_role,
        now_unix,
    );
    if !is_ceo && r == 0 && session_role != 0 && !corp_has_db_role_table(snap, corp_id) {
        r = session_role;
    }
    Some((is_ceo, r))
}

/// `contractProxy.DeleteContractNotification(contractID, forCorp)` and
/// `contractProxy.DeleteNotification(contractID, forCorp)` (`contracts.pyj:881,1032`).
pub fn handle_contract_delete_notif(
    ctx: &ServerContext,
    session: &SessionState,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let cid = args
        .first()
        .and_then(|a| a.as_i64())
        .filter(|&v| v > 0)
        .ok_or_else(|| BridgeRpcError {
            code: "InvalidArgument".into(),
            message: format!("contractProxy.{method} requires a positive contractID"),
            client_error: None,
        })?;
    let for_corp = args
        .get(1)
        .is_some_and(|v| v.as_bool() == Some(true) || v.as_i64().unwrap_or(0) != 0);
    let caller_corp = session.corporation_id.unwrap_or(0);
    let owner_id = if for_corp { caller_corp } else { char_id };
    let now_unix = ctx.now_unix();
    let changed = ctx.db.try_with_mut_snapshot(|snap| {
        if for_corp {
            if caller_corp < 98_000_000 {
                return Err(BridgeRpcError {
                    code: "CrpAccessDenied".into(),
                    message: "Corporation contract notifications require a player corporation".into(),
                    client_error: None,
                });
            }
            let allowed = effective_contract_helper_corp_roles(
                snap,
                caller_corp,
                char_id,
                session.corp_role,
                now_unix,
            )
            .is_some_and(|(is_ceo, r)| {
                is_ceo
                    || (r & (eve_db::corp_roles::ROLE_DIRECTOR | eve_db::corp_roles::ROLE_CONTRACT_MANAGER)) != 0
            });
            if !allowed {
                return Err(BridgeRpcError {
                    code: "CrpAccessDenied".into(),
                    message: "Deleting corporation contract notifications requires Contract Manager or Director role".into(),
                    client_error: None,
                });
            }
        }
        let Some(c) = snap.contracts.get(&cid).cloned() else {
            return Err(BridgeRpcError {
                code: "ContractNotFound".into(),
                message: format!("Contract {cid} does not exist"),
                client_error: None,
            });
        };
        let is_related = c.issuer_id == owner_id
            || c.issuer_corp_id == owner_id
            || c.assignee_id == owner_id
            || c.acceptor_id == Some(owner_id)
            || c.current_bidder_id == Some(owner_id)
            || c.bid_history.iter().any(|&(b, _, _)| b == owner_id || b == char_id);
        if !is_related {
            return Err(BridgeRpcError {
                code: "ContractAccessDenied".into(),
                message: format!("Owner {owner_id} is not associated with contract {cid}"),
                client_error: None,
            });
        }
        if c.status == "Outstanding" && c.expiration_unix > 0 && c.expiration_unix <= now_unix {
            let _ = snap.resolve_expired_contract(cid, now_unix);
        }
        let mut dismissed: Vec<String> = snap
            .quasar_state_blobs
            .get(DISMISSED_CONTRACT_NOTIFS_BLOB)
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default();
        let key = format!("{owner_id}:{cid}");
        let mut mutated = false;
        if !dismissed.contains(&key) {
            dismissed.push(key);
            if let Ok(json) = serde_json::to_string(&dismissed) {
                snap.quasar_state_blobs
                    .insert(DISMISSED_CONTRACT_NOTIFS_BLOB.to_string(), json);
            }
            mutated = true;
        }
        Ok(mutated)
    })?;
    Ok(PyRep::Bool(changed))
}

/// `contractProxy.DeleteMultipleContracts(contractIDs)` (`contracts.pyj:561`): deletes all
/// eligible `Outstanding` contracts in `contractIDs`, returning `(deletedIDs, failedIDs)`.
pub fn handle_contract_delete_multiple(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let cids: Vec<i64> = match args.first() {
        Some(PyRep::List(list) | PyRep::Tuple(list)) => {
            list.iter().filter_map(|v| v.as_i64()).collect()
        }
        Some(PyRep::Integer(id)) => vec![*id],
        _ => Vec::new(),
    };
    if cids.is_empty() {
        return Err(BridgeRpcError {
            code: "InvalidArgument".into(),
            message: "contractProxy.DeleteMultipleContracts requires a non-empty list of contractIDs".into(),
            client_error: None,
        });
    }
    let caller_corp = session.corporation_id.unwrap_or(0);
    let now_unix = ctx.now_unix();
    let (deleted, failed, item_changes) = ctx.db.try_with_mut_snapshot(|snap| {
        let corp_mgr = effective_contract_helper_corp_roles(
            snap,
            caller_corp,
            char_id,
            session.corp_role,
            now_unix,
        )
        .is_some_and(|(is_ceo, r)| {
            is_ceo
                || (r & (eve_db::corp_roles::ROLE_DIRECTOR | eve_db::corp_roles::ROLE_CONTRACT_MANAGER)) != 0
        });
        let mut del = Vec::new();
        let mut fail = Vec::new();
        let mut changes: Vec<(eve_db::InventoryItemRecord, i64, i64, eve_db::InventoryFlag)> = Vec::new();
        let mut restored_ids = std::collections::HashSet::new();
        for cid in cids {
            let Some(c) = snap.contracts.get(&cid).cloned() else {
                fail.push(PyRep::Integer(cid));
                continue;
            };
            let owns = if c.for_corp {
                c.issuer_corp_id == caller_corp && corp_mgr
            } else {
                c.issuer_id == char_id
            };
            let has_bids = c.contract_type == "Auction"
                && (c.current_bidder_id.is_some() || !c.bid_history.is_empty());
            if c.status == "Outstanding" && owns && !has_bids {
                snap.credit_contract_issuer(&c, c.reward);
                let (restore_owner, restore_flag) = if c.for_corp {
                    (c.issuer_corp_id, eve_db::InventoryFlag::Deliveries)
                } else {
                    (c.issuer_id, eve_db::InventoryFlag::Hangar)
                };
                for iid in &c.item_ids {
                    if restored_ids.contains(iid) {
                        continue;
                    }
                    if let Some(it) = snap.items.get_mut(iid) {
                        let in_escrow = it.location_id == cid
                            || (it.location_id == c.start_station_id
                                && it.flag == eve_db::InventoryFlag::Locked
                                && (it.owner_id == c.issuer_id
                                    || (c.for_corp && it.owner_id == c.issuer_corp_id)
                                    || it.owner_id == 0));
                        if in_escrow {
                            let (po, pl, pf) = (it.owner_id, it.location_id, it.flag);
                            it.owner_id = restore_owner;
                            it.location_id = c.start_station_id;
                            it.flag = restore_flag;
                            it.version += 1;
                            restored_ids.insert(it.item_id);
                            changes.push((it.clone(), po, pl, pf));
                        }
                    }
                }
                for it in snap.items.values_mut() {
                    if !restored_ids.contains(&it.item_id) && it.location_id == cid {
                        let (po, pl, pf) = (it.owner_id, it.location_id, it.flag);
                        it.owner_id = restore_owner;
                        it.location_id = c.start_station_id;
                        it.flag = restore_flag;
                        it.version += 1;
                        restored_ids.insert(it.item_id);
                        changes.push((it.clone(), po, pl, pf));
                    }
                }
                if let Some(cm) = snap.contracts.get_mut(&cid) {
                    cm.status = "Deleted".into();
                }
                del.push(PyRep::Integer(cid));
            } else {
                fail.push(PyRep::Integer(cid));
            }
        }
        Ok::<_, BridgeRpcError>((del, fail, changes))
    })?;
    for (it, prev_owner, prev_loc, prev_flag) in item_changes {
        let (group_id, category_id) = ctx
            .sde
            .types
            .get(&it.type_id)
            .map(|t| (t.group_id, t.category_id))
            .unwrap_or((0, 0));
        let row = PyRep::keyval([
            ("itemID", PyRep::Integer(it.item_id)),
            ("typeID", PyRep::Integer(i64::from(it.type_id))),
            ("ownerID", PyRep::Integer(it.owner_id)),
            ("locationID", PyRep::Integer(it.location_id)),
            ("flagID", PyRep::Integer(it.flag as i64)),
            ("quantity", PyRep::Integer(if it.singleton { -1 } else { i64::from(it.quantity) })),
            ("groupID", PyRep::Integer(i64::from(group_id))),
            ("categoryID", PyRep::Integer(i64::from(category_id))),
            ("singleton", PyRep::Bool(it.singleton)),
        ]);
        let mut ch = Vec::new();
        if prev_owner != it.owner_id {
            ch.push((2, PyRep::Integer(prev_owner)));
        }
        if prev_loc != it.location_id {
            ch.push((3, PyRep::Integer(prev_loc)));
        }
        if prev_flag != it.flag {
            ch.push((4, PyRep::Integer(prev_flag as i64)));
        }
        let _ = ctx.notify_item_change(char_id, row, ch);
    }
    Ok(PyRep::Tuple(vec![PyRep::List(deleted), PyRep::List(failed)]))
}

/// `contractProxy.GetItemsInDockableLocation(stationID, forCorp)` (`contracts.pyj:872`):
/// returns inventory items owned by the character (or corporation) at `stationID` / `structureID`.
pub fn handle_contract_get_items_in_dockable(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let loc_id = args
        .first()
        .and_then(|a| a.as_i64())
        .or_else(|| session.station_id.or(session.structure_id))
        .filter(|&id| id > 0)
        .ok_or_else(|| BridgeRpcError {
            code: "InvalidArgument".into(),
            message: "contractProxy.GetItemsInDockableLocation requires a valid dockable locationID".into(),
            client_error: None,
        })?;
    let for_corp = args
        .get(1)
        .is_some_and(|v| v.as_bool() == Some(true) || v.as_i64().unwrap_or(0) != 0);
    let owner_id = if for_corp {
        session.corporation_id.unwrap_or(0)
    } else {
        char_id
    };
    let rows = ctx.db.with_snapshot(|snap| {
        let mut items: Vec<_> = snap
            .items
            .values()
            .filter(|i| {
                i.owner_id == owner_id
                    && i.location_id == loc_id
                    && Some(i.item_id) != session.ship_id
                    && i.flag != eve_db::InventoryFlag::Locked
                    && !snap.is_item_locked_in_contract_or_job(i.item_id)
            })
            .collect();
        items.sort_by_key(|i| i.item_id);
        items
            .into_iter()
            .map(|i| {
                let (gid, cid) = ctx
                    .sde
                    .types
                    .get(&i.type_id)
                    .map(|t| (i64::from(t.group_id), i64::from(t.category_id)))
                    .unwrap_or((0, 0));
                let singleton_val = if i.is_blueprint_copy {
                    2
                } else if i.singleton {
                    1
                } else {
                    0
                };
                let qty_val = if i.is_blueprint_copy {
                    -2
                } else if i.singleton {
                    -1
                } else {
                    i64::from(i.quantity)
                };
                PyRep::keyval([
                    ("itemID", PyRep::Integer(i.item_id)),
                    ("typeID", PyRep::Integer(i64::from(i.type_id))),
                    ("groupID", PyRep::Integer(gid)),
                    ("categoryID", PyRep::Integer(cid)),
                    ("stacksize", PyRep::Integer(i64::from(i.quantity))),
                    ("quantity", PyRep::Integer(qty_val)),
                    ("flagID", PyRep::Integer(i.flag as i64)),
                    ("singleton", PyRep::Integer(singleton_val)),
                    ("ownerID", PyRep::Integer(i.owner_id)),
                    ("locationID", PyRep::Integer(i.location_id)),
                ])
            })
            .collect::<Vec<_>>()
    });
    Ok(PyRep::List(rows))
}

#[path = "repair_service.rs"]
pub mod repair_service;
pub use repair_service::*;

#[cfg(test)] #[path = "sov_services_tests.rs"]
mod sov_services_tests;
