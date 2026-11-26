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
fn find_live_sov_hub_in_system(snap: &eve_db::WorldStateSnapshot, sys_id: u32) -> Option<i64> {
    snap.structures
        .iter()
        .filter(|(_, s)| {
            s.solar_system_id == sys_id
                && s.type_id == TYPE_SOVEREIGNTY_HUB
                && !s.state.eq_ignore_ascii_case("destroyed")
        })
        .min_by_key(|(&hid, _)| (!snap.sov_runtime.sov_hubs.contains_key(&hid), hid))
        .map(|(&hid, _)| hid)
}

pub(crate) fn resolve_sov_hub_id(
    snap: &eve_db::WorldStateSnapshot,
    arg_id_or_sys: Option<i64>,
    session_sys: Option<u32>,
) -> Option<i64> {
    if let Some(raw) = arg_id_or_sys {
        if let Some(st) = snap.structures.get(&raw) {
            return (st.type_id == TYPE_SOVEREIGNTY_HUB
                && !st.state.eq_ignore_ascii_case("destroyed"))
            .then_some(raw);
        }
        return u32::try_from(raw)
            .ok()
            .and_then(|sys_id| find_live_sov_hub_in_system(snap, sys_id));
    }
    session_sys.and_then(|sys_id| find_live_sov_hub_in_system(snap, sys_id))
}

fn caller_corp_and_alliance(snap: &eve_db::WorldStateSnapshot, char_id: i64) -> (i64, Option<i64>) {
    let char_corp = snap.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(0);
    let char_alliance = snap
        .characters
        .get(&char_id)
        .and_then(|c| c.alliance_id)
        .or_else(|| snap.corporations.get(&char_corp).and_then(|c| c.alliance_id));
    (char_corp, char_alliance)
}

fn session_is_sov_hub_owner_manager(
    snap: &eve_db::WorldStateSnapshot,
    hub_id: i64,
    char_id: i64,
    now_unix: u64,
) -> bool {
    let Some(st) = snap
        .structures
        .get(&hub_id)
        .filter(|s| s.type_id == TYPE_SOVEREIGNTY_HUB && !s.state.eq_ignore_ascii_case("destroyed"))
    else {
        return false;
    };
    let (char_corp, _) = caller_corp_and_alliance(snap, char_id);
    if st.owner_corp_id != char_corp || !snap.corporations.contains_key(&char_corp) {
        return false;
    }
    let (is_ceo, eff_roles) =
        effective_contract_helper_corp_roles(snap, char_corp, char_id, now_unix)
            .unwrap_or((false, 0));
    is_ceo
        || (eff_roles
            & (eve_db::corp_roles::ROLE_DIRECTOR | eve_db::corp_roles::ROLE_STATION_MANAGER))
            != 0
}

fn caller_can_attach_fuel_access_group(
    char_id: i64,
    caller_corp: i64,
    caller_alliance: i64,
    grp: &crate::services::station_structure::structure_upwell_ops::AccessGroupRecord,
) -> bool {
    use crate::services::structure_acl_defense_rules::{
        is_acl_creator_active_for_corp, is_acl_role_allowed, is_acl_role_excluded,
    };
    if grp.members.get(&char_id).copied().is_some_and(is_acl_role_excluded) {
        return false;
    }
    is_acl_creator_active_for_corp(char_id, caller_corp, grp)
        || (grp.members.get(&char_id).copied().is_some_and(is_acl_role_allowed)
            && (grp.owner_corp_id == 0
                || char_id != grp.creator_id
                || grp.owner_corp_id == caller_corp))
        || (caller_corp != 0
            && grp.members.get(&caller_corp).copied().is_some_and(is_acl_role_allowed))
        || (caller_alliance != 0
            && grp
                .members
                .get(&caller_alliance)
                .copied()
                .is_some_and(is_acl_role_allowed))
        || (grp.owner_corp_id != 0 && grp.owner_corp_id == caller_corp)
        || grp.members.get(&0).copied().is_some_and(is_acl_role_allowed)
}

fn parse_skyhook_ids_arg(args: &[PyRep]) -> Result<Vec<i64>, BridgeRpcError> {
    let invalid = || BridgeRpcError {
        code: "InvalidArguments".into(),
        message: "DestroySkyhooks requires a non-empty collection of positive skyhookIDs".into(),
        client_error: None,
    };
    if args.len() != 1 {
        return Err(invalid());
    }
    let items = match &args[0] {
        PyRep::List(items)
        | PyRep::Tuple(items)
        | PyRep::PySet(items)
        | PyRep::Set(items) => items,
        _ => return Err(invalid()),
    };
    if items.is_empty() {
        return Err(invalid());
    }
    let mut seen = std::collections::HashSet::with_capacity(items.len());
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let Some(id) = item.as_i64().filter(|&v| v > 0) else {
            return Err(invalid());
        };
        if seen.insert(id) {
            out.push(id);
        }
    }
    Ok(out)
}

fn session_can_access_sov_hub_fuel(
    snap: &eve_db::WorldStateSnapshot,
    hub_id: i64,
    char_id: i64,
    now_unix: u64,
) -> bool {
    let (char_corp, char_alliance) = caller_corp_and_alliance(snap, char_id);
    let effective_roles =
        effective_contract_helper_corp_roles(snap, char_corp, char_id, now_unix)
            .map(|(_, r)| r)
            .unwrap_or_else(|| {
                snap.effective_corp_roles_at_db(char_corp, char_id, None, now_unix).1
            });
    can_access_sov_hub_fuel(snap, hub_id, char_id, char_corp, char_alliance, effective_roles, false)
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
        .filter(|s| s.type_id == TYPE_SOVEREIGNTY_HUB && !s.state.eq_ignore_ascii_case("destroyed"))
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
    let (is_ceo, eff_roles) = snap.effective_corp_roles_at_db(corp_id, char_id, None, 0);
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
            let access_group_id = match args.get(1) {
                None | Some(PyRep::None) => 0,
                Some(v) => match v.as_i64() {
                    Some(id) if id >= 0 => id,
                    _ => {
                        return Some(Err(BridgeRpcError {
                            code: "InvalidArguments".into(),
                            message: "fuelAccessGroupID must be a non-negative integer or None"
                                .into(),
                            client_error: None,
                        }));
                    }
                },
            };

            let now_unix = ctx.now_unix();
            let validated = ctx.db.with_snapshot(|snap| {
                let Some(hub_id) = resolve_sov_hub_id(snap, arg0, session.solar_system_id) else {
                    return Err(BridgeRpcError {
                        code: "InvalidArguments".into(),
                        message: "SetSovHubFuelAccessGroup requires hubID or solarSystemID".into(),
                        client_error: None,
                    });
                };
                if !session_is_sov_hub_owner_manager(
                    snap,
                    hub_id,
                    char_id,
                    now_unix,
                ) {
                    return Err(BridgeRpcError {
                        code: "CorpRoleDenied".into(),
                        message: "Only owner corporation directors or station managers may set hub fuel access group".into(),
                        client_error: None,
                    });
                }
                if access_group_id > 0 {
                    let ext = crate::services::station_structure::structure_upwell_ops::load_ext_state(snap);
                    let Some(grp) = ext.owner_groups.get(&access_group_id) else {
                        return Err(BridgeRpcError {
                            code: "AccessGroupNotFound".into(),
                            message: format!("Fuel access group {access_group_id} not found"),
                            client_error: None,
                        });
                    };
                    let (char_corp, char_alliance) = caller_corp_and_alliance(snap, char_id);
                    if !caller_can_attach_fuel_access_group(
                        char_id,
                        char_corp,
                        char_alliance.unwrap_or(0),
                        grp,
                    ) {
                        return Err(BridgeRpcError {
                            code: "AccessGroupAccessDenied".into(),
                            message: "Caller does not have access to attach this fuel access group".into(),
                            client_error: None,
                        });
                    }
                }
                Ok(hub_id)
            });
            let hub_id = match validated {
                Ok(id) => id,
                Err(e) => return Some(Err(e)),
            };

            with_sov_runtime(ctx, |rt| {
                rt.fuel_access_groups.insert(hub_id, access_group_id);
            });
            Some(Ok(PyRep::Integer(access_group_id)))
        }

        ("sovMgr", "GetSovHubFuelAccessGroup") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let arg0 = args.first().and_then(|a| a.as_i64());
            let now_unix = ctx.now_unix();
            let resolved = ctx.db.with_snapshot(|snap| {
                let Some(hub_id) = resolve_sov_hub_id(snap, arg0, session.solar_system_id) else {
                    return Err(BridgeRpcError {
                        code: "InvalidArguments".into(),
                        message: "GetSovHubFuelAccessGroup requires hubID or solarSystemID".into(),
                        client_error: None,
                    });
                };
                if !session_can_access_sov_hub_fuel(
                    snap,
                    hub_id,
                    char_id,
                    now_unix,
                ) && !session_is_sov_hub_owner_manager(
                    snap,
                    hub_id,
                    char_id,
                    now_unix,
                ) {
                    return Err(BridgeRpcError {
                        code: "CorpRoleDenied".into(),
                        message: "Only authorized hub fuel ACL members or owner corporation managers may read hub fuel access group".into(),
                        client_error: None,
                    });
                }
                Ok(hub_id)
            });
            let hub_id = match resolved {
                Ok(id) => id,
                Err(e) => return Some(Err(e)),
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
            let now_unix = ctx.now_unix();
            let allowed = ctx.db.with_snapshot(|snap| {
                let Some(hub_id) = resolve_sov_hub_id(snap, arg0, session.solar_system_id) else {
                    return false;
                };
                session_can_access_sov_hub_fuel(snap, hub_id, char_id, now_unix)
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
            let requested_skyhook_ids = match parse_skyhook_ids_arg(args) {
                Ok(ids) => ids,
                Err(e) => return Some(Err(e)),
            };

            let now_unix = ctx.now_unix();
            let is_corp_director = ctx.db.with_snapshot(|snap| {
                let corp_id = snap.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(0);
                if snap.corporations.contains_key(&corp_id) {
                    let (is_ceo, role) = snap.effective_corp_roles_at_db(
                        corp_id,
                        char_id,
                        None,
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
            let pending: super::quasar_sov_hub::PendingNotices =
                std::sync::Arc::new(std::sync::RwLock::new(Vec::new()));
            let destroy_res = ctx.db.try_with_mut_snapshot(|snap| {
                let char_corp = snap.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(0);
                let mut by_system: std::collections::BTreeMap<u32, Vec<i64>> =
                    std::collections::BTreeMap::new();
                for sid in requested_skyhook_ids {
                    let Some(s) = snap.structures.get(&sid).filter(|s| {
                        s.type_id == TYPE_ORBITAL_SKYHOOK
                            && !s.state.eq_ignore_ascii_case("destroyed")
                    }) else {
                        return Err(BridgeRpcError {
                            code: "InvalidArguments".into(),
                            message: format!("Structure {sid} is not a live Orbital Skyhook"),
                            client_error: None,
                        });
                    };
                    if s.owner_corp_id != char_corp {
                        return Err(BridgeRpcError {
                            code: "CorpRoleDenied".into(),
                            message: format!(
                                "Cannot destroy Skyhook {sid} owned by another corporation"
                            ),
                            client_error: None,
                        });
                    }
                    if snap.sov_runtime.skyhook_silo_links.contains_key(&sid) {
                        return Err(BridgeRpcError {
                            code: "ActiveReagentHeist".into(),
                            message: format!(
                                "Cannot destroy Skyhook {sid} while a reagent siphon link is active"
                            ),
                            client_error: None,
                        });
                    }
                    let damaged_hp = s.shield_hp < crate::services::sov_gameplay::SKYHOOK_BASE_SHIELD_HP
                        || s.armor_hp < crate::services::sov_gameplay::SKYHOOK_BASE_ARMOR_HP
                        || s.hull_hp < crate::services::sov_gameplay::SKYHOOK_BASE_HULL_HP;
                    if matches!(
                        s.state.as_str(),
                        "Reinforced"
                            | "ArmorReinforce"
                            | "ArmorReinforced"
                            | "ArmorVulnerable"
                            | "HullReinforce"
                            | "HullReinforced"
                            | "HullVulnerable"
                    ) || s.reinforced_until_unix > now_unix
                        || damaged_hp
                    {
                        return Err(BridgeRpcError {
                            code: "StructureReinforcedOrDamaged".into(),
                            message: format!(
                                "Cannot destroy Skyhook {sid} while reinforced or damaged (state={})",
                                s.state
                            ),
                            client_error: None,
                        });
                    }
                    by_system.entry(s.solar_system_id).or_default().push(sid);
                }

                let mut per_system = Vec::with_capacity(by_system.len());
                for (sys_id, sids) in by_system {
                    let (skyhook_ids, removed_dens, removed_mto_balls) =
                        super::quasar_sov_workforce::with_system_hub_network_reconcile::<
                            _,
                            BridgeRpcError,
                        >(
                            snap,
                            &ctx.sde,
                            &pending,
                            sys_id,
                            now_unix,
                            |snap| {
                                let mut skyhook_ids = Vec::with_capacity(sids.len());
                                let mut removed_dens = Vec::new();
                                let mut removed_mto_balls = Vec::new();
                                for sid in sids {
                                    snap.structures.remove(&sid);
                                    let mut cleanup = crate::services::sov_gameplay::cleanup_skyhook_and_attached_dens_in_snap(
                                        snap, sid,
                                    );
                                    removed_dens.append(&mut cleanup.removed_dens);
                                    removed_mto_balls.append(&mut cleanup.removed_mto_balls);
                                    skyhook_ids.push(sid);
                                }
                                Ok((skyhook_ids, removed_dens, removed_mto_balls))
                            },
                        )?;
                    per_system.push((sys_id, skyhook_ids, removed_dens, removed_mto_balls));
                }
                Ok::<_, BridgeRpcError>(per_system)
            });
            let per_system = match destroy_res {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let mut total_destroyed: usize = 0;
            for (sys_id, destroyed_ids, removed_dens, removed_mto_balls) in per_system {
                let sys_handle = ctx.systems.get_or_spawn(sys_id);
                for &sid in &destroyed_ids {
                    let _ = sys_handle.remove_ball(sid).await;
                }
                for &(did, owner_cid) in &removed_dens {
                    let _ = sys_handle.remove_ball(did).await;
                    if owner_cid > 0 {
                        ctx.emit_typed(
                            NotificationTarget::Character(owner_cid),
                            crate::router::notify_registry::OnMyMercenaryDenRemoved {
                                notify_msg: "MercenaryDenRemoved".to_string(),
                                notify_args: PyRep::Dict(vec![(
                                    PyRep::String("itemID".into()),
                                    PyRep::Integer(did),
                                )]),
                            },
                        );
                    }
                }
                for &bid in &removed_mto_balls {
                    let _ = sys_handle.remove_ball(bid).await;
                }

                let mut sov_updated_ids = destroyed_ids.clone();
                sov_updated_ids.extend(removed_dens.iter().map(|(did, _)| *did));
                emit_sov_structures_updated(ctx, sys_id, &sov_updated_ids);
                total_destroyed += destroyed_ids.len();
            }
            crate::services::sov_gameplay::flush_pending_quasar(ctx, &pending, now_unix);
            Some(Ok(PyRep::Integer(total_destroyed as i64)))
        }

        ("colonyResourcesMgr", "GetMyCorporationSkyhooks") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let now_unix = ctx.now_unix();
            let skyhooks = ctx.db.with_snapshot(|snap| -> Result<Vec<PyRep>, BridgeRpcError> {
                let corp_id = snap
                    .characters
                    .get(&char_id)
                    .map(|c| c.corporation_id)
                    .or(session.corporation_id)
                    .unwrap_or(0);
                let corp_record = snap.corporations.get(&corp_id);
                if corp_id <= 0
                    || (corp_record.is_none()
                        && ((1_000_000..2_000_000).contains(&corp_id)
                            || ctx.sde.npc_corporations.contains_key(&corp_id)))
                {
                    return Err(crate::services::account_services::err(
                        "CorpRoleDenied",
                        "NPC corporations cannot manage Skyhooks",
                    ));
                }
                if !session.has_gm_role() {
                    if let Some(corp) = corp_record {
                        if corp.ceo_id != char_id
                            && !corp.members.is_empty()
                            && !corp.members.contains(&char_id)
                        {
                            return Err(crate::services::account_services::err(
                                "CorpRoleDenied",
                                format!("Character {char_id} is not a member of corporation {corp_id}"),
                            ));
                        }
                        let (is_ceo, eff_roles) = snap.effective_corp_roles_at_db(
                            corp_id,
                            char_id,
                            None,
                            now_unix,
                        );
                        if corp.ceo_id != char_id
                            && !is_ceo
                            && !eve_db::corp_roles::has_role(
                                eff_roles,
                                eve_db::corp_roles::ROLE_STATION_MANAGER,
                            )
                        {
                            return Err(crate::services::account_services::err(
                                "CorpRoleDenied",
                                "Viewing corporation Skyhooks requires Station Manager or Director role",
                            ));
                        }
                    } else {
                        return Err(crate::services::account_services::err(
                            "CorpRoleDenied",
                            "Viewing corporation Skyhooks requires Station Manager or Director role",
                        ));
                    }
                }
                Ok(snap
                    .structures
                    .values()
                    .filter(|s| {
                        s.type_id == TYPE_ORBITAL_SKYHOOK
                            && s.owner_corp_id == corp_id
                            && !s.state.eq_ignore_ascii_case("destroyed")
                    })
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
                    .collect::<Vec<PyRep>>())
            });
            Some(skyhooks.map(PyRep::List))
        }

        ("devIndexManager", "GetAllDevelopmentIndices") => {
            struct DevelopmentIndexReply {
                solar_system_id: u32,
                military: u8,
                industrial: u8,
                strategic: u8,
            }
            impl DevelopmentIndexReply {
                fn into_pyrep(self) -> PyRep {
                    PyRep::keyval([
                        ("solarSystemID", PyRep::Integer(i64::from(self.solar_system_id))),
                        ("military", PyRep::Integer(i64::from(self.military))),
                        ("industrial", PyRep::Integer(i64::from(self.industrial))),
                        ("strategic", PyRep::Integer(i64::from(self.strategic))),
                        (
                            "militaryPoints",
                            PyRep::Integer(i64::from(self.military) * 1_000),
                        ),
                        (
                            "industrialPoints",
                            PyRep::Integer(i64::from(self.industrial) * 1_000),
                        ),
                        ("claimedFor", PyRep::Integer(i64::from(self.strategic))),
                        (
                            "adm",
                            PyRep::Real(calculate_adm(
                                self.military,
                                self.industrial,
                                self.strategic,
                            )),
                        ),
                    ])
                }
            }
            let res = ctx.db.with_snapshot(|snap| {
                let mut map = Vec::new();
                for (&sys_id, st) in &snap.system_states {
                    if st.sov_alliance_id.is_some() || st.military_index > 0 || st.industrial_index > 0 {
                        let indices = DevelopmentIndexReply {
                            solar_system_id: sys_id,
                            military: st.military_index,
                            industrial: st.industrial_index,
                            strategic: st.strategic_index,
                        }
                        .into_pyrep();
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
                let has_sov = st.and_then(|s| s.sov_alliance_id).is_some();
                if !has_sov {
                    return PyRep::Dict(Vec::new());
                }
                let mil_pts = mil_lvl * 20_000;
                let ind_pts = ind_lvl * 20_000;
                let strat_pts = strat_lvl * 20_000;
                let inc = i64::from(has_sov);
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
            let filter_loc_id = match args.first() {
                None | Some(PyRep::None) => None,
                Some(v) => match v.as_i64() {
                    Some(0) => None,
                    Some(id) if id > 0 => Some(u32::try_from(id).ok()),
                    _ => {
                        return Some(Err(BridgeRpcError {
                            code: "InvalidArguments".into(),
                            message: "map.GetCurrentSovData locationID must be a positive integer or None".into(),
                            client_error: None,
                        }));
                    }
                },
            };
            let sov_data = ctx.db.with_snapshot(|snap| {
                if matches!(filter_loc_id, Some(None)) {
                    return Vec::new();
                }
                let req_loc = filter_loc_id.flatten();
                let list = snap
                    .system_states
                    .iter()
                    .filter(|&(&sys_id, _)| match req_loc {
                        None => true,
                        Some(loc) if loc == sys_id => true,
                        Some(loc) => ctx.sde.solar_system(sys_id).is_some_and(
                            |sys| sys.constellation_id == loc || sys.region_id == loc,
                        ),
                    })
                    .filter_map(|(&sys_id, st)| {
                        st.sov_alliance_id.map(|aid| {
                            let hub_id = find_live_sov_hub_in_system(snap, sys_id).or_else(|| {
                                snap.structures
                                    .values()
                                    .find(|s| {
                                        s.solar_system_id == sys_id
                                            && s.type_id == 32226
                                            && !s.state.eq_ignore_ascii_case("destroyed")
                                    })
                                    .map(|s| s.structure_id)
                            });
                            let corp_id = snap
                                .alliances
                                .get(&aid)
                                .map(|al| al.executor_corp_id)
                                .unwrap_or(0);
                            let hub_rep = hub_id.map(PyRep::Integer).unwrap_or(PyRep::None);
                            vec![
                                PyRep::Integer(i64::from(sys_id)),
                                PyRep::Integer(i64::from(sys_id)),
                                PyRep::Integer(aid),
                                PyRep::Integer(corp_id),
                                hub_rep,
                            ]
                        })
                    })
                    .collect::<Vec<Vec<PyRep>>>();
                list
            });
            Some(Ok(PyRep::rowset(
                vec![
                    "locationID",
                    "solarSystemID",
                    "allianceID",
                    "corporationID",
                    "claimStructureID",
                ],
                sov_data,
            )))
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
        return Err(BridgeRpcError::insufficient_role_with_code(
            "GMRoleRequired",
            eve_machonet::service_roles::ROLE_GM_MASK,
            session.role,
            "jumpCloneSvc.ResetLastCloneJumpTime",
            "jumpCloneSvc.ResetLastCloneJumpTime requires GM role",
        ));
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

fn resolve_contract_caller_corp(
    snap: &eve_db::WorldStateSnapshot,
    char_id: i64,
    session_corp_id: Option<i64>,
) -> i64 {
    snap.characters
        .get(&char_id)
        .map(|ch| ch.corporation_id)
        .filter(|&id| id != 0)
        .or(session_corp_id)
        .unwrap_or(0)
}

fn effective_contract_helper_corp_roles(
    snap: &eve_db::WorldStateSnapshot,
    corp_id: i64,
    char_id: i64,
    now_unix: u64,
) -> Option<(bool, u64)> {
    let _corp = snap.corporations.get(&corp_id)?;
    let (is_ceo, r) = snap.effective_corp_roles_at_db(corp_id, char_id, None, now_unix);
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
    let session_corp_id = session.corporation_id;
    let now_unix = ctx.now_unix();
    let changed = ctx.db.try_with_mut_snapshot(|snap| {
        let effective_caller_corp = resolve_contract_caller_corp(snap, char_id, session_corp_id);
        let owner_id = if for_corp { effective_caller_corp } else { char_id };
        if for_corp {
            if effective_caller_corp < 98_000_000 {
                return Err(BridgeRpcError {
                    code: "CrpAccessDenied".into(),
                    message: "Corporation contract notifications require a player corporation".into(),
                    client_error: None,
                });
            }
            if !crate::services::contracts_service::has_corp_contract_role(
                snap,
                char_id,
                effective_caller_corp,
                now_unix,
            ) {
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
    let session_corp_id = session.corporation_id;
    let now_unix = ctx.now_unix();
    let (deleted, failed, transferred_ids) = ctx.db.try_with_mut_snapshot(|snap| {
        let effective_caller_corp = resolve_contract_caller_corp(snap, char_id, session_corp_id);
        let corp_mgr = crate::services::contracts_service::has_corp_contract_role(
            snap,
            char_id,
            effective_caller_corp,
            now_unix,
        );
        let mut del = Vec::new();
        let mut fail = Vec::new();
        let mut transferred_ids = Vec::new();
        let mut restored_ids = std::collections::HashSet::new();
        for cid in cids {
            let Some(c) = snap.contracts.get(&cid).cloned() else {
                fail.push(PyRep::Integer(cid));
                continue;
            };
            let owns = if c.for_corp {
                c.issuer_corp_id == effective_caller_corp && corp_mgr
            } else {
                c.issuer_id == char_id
            };
            let has_bids = c.contract_type == "Auction"
                && (c.current_bidder_id.is_some() || !c.bid_history.is_empty());
            if c.status == "Outstanding" && owns && !has_bids {
                snap.credit_contract_issuer_with_journal(
                    &c,
                    c.reward,
                    "ContractRewardRefund",
                    eve_db::ledger::ref_types::CONTRACT_REWARD,
                    cid,
                    now_unix,
                    format!("Reward refunded on deleted contract {cid}"),
                );
                snap.refund_contract_deposit(&c, now_unix, "deleted");
                let restore_owner = if c.for_corp && c.issuer_corp_id != 0 {
                    c.issuer_corp_id
                } else {
                    c.issuer_id
                };
                let escrow_item_ids: Vec<i64> = c
                    .item_ids
                    .iter()
                    .copied()
                    .filter(|iid| {
                        !restored_ids.contains(iid)
                            && snap.items.get(iid).is_some_and(|it| {
                                it.location_id == cid
                                    || (it.location_id == c.start_station_id
                                        && it.flag == eve_db::InventoryFlag::Locked
                                        && (it.owner_id == c.issuer_id
                                            || (c.for_corp && it.owner_id == c.issuer_corp_id)
                                            || it.owner_id == 0))
                            })
                    })
                    .collect();
                restored_ids.extend(escrow_item_ids.iter().copied());
                transferred_ids.extend(snap.settle_contract_escrow_items(
                    cid,
                    &escrow_item_ids,
                    restore_owner,
                    c.start_station_id,
                    None,
                    true,
                ));
                snap.clear_contract_divisions(cid);
                if let Some(cm) = snap.contracts.get_mut(&cid) {
                    cm.status = "Deleted".into();
                }
                del.push(PyRep::Integer(cid));
            } else {
                fail.push(PyRep::Integer(cid));
            }
        }
        Ok::<_, BridgeRpcError>((del, fail, transferred_ids))
    })?;
    if !transferred_ids.is_empty() {
        crate::services::quasar_cosmetics_live::strip_ship_cosmetics_for_ships(&ctx.db, &transferred_ids);
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

use crate::router::rpc_registry::{rpc_entry, RpcBoxFuture, RpcDispatchHandler, RpcEntry};

fn dispatch_rpc<'a>(
    ctx: &'a ServerContext,
    session: &'a mut SessionState,
    service: &'a str,
    method: &'a str,
    args: &'a [PyRep],
    kwargs: &'a HashMap<String, PyRep>,
) -> RpcBoxFuture<'a> {
    Box::pin(async move {
        if let Some(res) =
            crate::services::pve_abyssal_hazards::check_abyssal_in_pocket_rpc_restrictions(
                session, service, method, args,
            )
        {
            return Some(res);
        }
        handle(ctx, session, service, method, args, kwargs).await
    })
}

pub const RPCS: &[RpcEntry] = &[
    rpc_entry("sovMgr", "SetSovHubFuelAccessGroup", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("sovMgr", "GetSovHubFuelAccessGroup", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("sovMgr", "IsOnLocalSovHubFuelAccessGroup", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("sovMgr", "ConfirmSkyhookSovManagement", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("sovMgr", "DestroySkyhooks", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("colonyResourcesMgr", "GetMyCorporationSkyhooks", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("devIndexManager", "GetAllDevelopmentIndices", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("devIndexManager", "GetDevelopmentIndicesForSystem", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("warpVectorMgr", "UseVector", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("map", "GetCurrentSovData", RpcDispatchHandler::Async(dispatch_rpc)),
];

#[path = "repair_service.rs"]
pub mod repair_service;
pub use repair_service::*;

#[cfg(test)] #[path = "sov_services_tests.rs"]
mod sov_services_tests;
