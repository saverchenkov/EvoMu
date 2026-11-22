//! Wars (`feat:war.*`): declaration with War HQ + fee, war lists, surrender negotiations and
//! 2-week peace treaties, mutual wars, defender allies and per-war statistics (`SO-3`).
//!
//! Persistent war rows live in `snap.wars`; negotiation / treaty / HQ bookkeeping lives in
//! `WAR_STATE`. War end conditions (finish timers, War HQ lost, defender no longer
//! war-eligible) are applied lazily by `sync_wars` at the start of every war RPC.

#![forbid(unsafe_code)]

use crate::domain::wars::{self, AllyNegotiationAction, MutualWarAcceptOutcome};
use crate::router::ServerContext;
use eve_db::{IskCents, PersistedNegKind as NegKind, WorldStateSnapshot};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::BTreeMap;

#[path = "social_wars_helpers.rs"]
pub(crate) mod social_wars_helpers;
use social_wars_helpers::{
    already_at_war, corp_represents_war_side, emit_mutual_wars_updated, emit_war_changed,
    entity_matches_war_owner, err, is_ally_neg_active_at, is_valid_war_hq, is_war_participant_corp,
    neg_rows_for_caller, opt_unix_to_filetime_py, resolve_entity_alliance_id, resolve_war_stat_id,
    rpc_err, same_alliance, sync_wars, unix_to_filetime, war_crowset, war_eligible, war_row,
    NEG_COLUMNS, WAR_COLUMNS, WAR_DECLARATION_FEE_ISK, WAR_TIMER_SECS,
};

struct WarKill {
    kill_id: i64,
    attacker_char: i64,
    attacker_corp: i64,
    attacker_alliance: Option<i64>,
    attacker_ship_type_id: u32,
    victim_char: i64,
    victim_corp: i64,
    victim_alliance: Option<i64>,
    victim_ship_type_id: u32,
    kill_time_unix: u64,
    group_id: u32,
    isk: f64,
}

struct WarKillsSummary {
    aggressor_corp_id: i64,
    defender_corp_id: i64,
    ally_corp_ids: Vec<i64>,
    kills: Vec<WarKill>,
}

/// Per-war kills DERIVED from persisted killmails: a killmail counts towards a war when the
/// final-blow attacker's corporation and the victim's corporation are on opposite sides of the war
/// (including accepted defender allies) and the kill timestamp falls within the war window.
fn war_kills_in_snap(ctx: &ServerContext, snap: &WorldStateSnapshot, war_id: i64) -> Option<WarKillsSummary> {
    let w = snap.wars.get(&war_id)?;
    let (agg, def) = (w.aggressor_corp_id, w.defender_corp_id);
    let now = ctx.now_unix();
    let meta = snap.war_state.wars.get(&war_id).cloned().unwrap_or_default();
    let accepted_allies: Vec<&eve_db::PersistedWarNegotiation> = snap
        .war_state
        .negotiations
        .iter()
        .filter(|n| n.war_id == war_id && n.kind == NegKind::Ally && n.status == "Accepted")
        .collect();
    let is_agg_side = |cid: i64, all: Option<i64>| entity_matches_war_owner(snap, cid, all, agg);
    let is_def_side = |cid: i64, all: Option<i64>, ts: u64| {
        let check_at = if ts == 0 { now } else { ts };
        entity_matches_war_owner(snap, cid, all, def)
            || accepted_allies.iter().any(|n| {
                is_ally_neg_active_at(snap, n, check_at)
                    && entity_matches_war_owner(snap, cid, all, n.from_corp)
            })
    };
    let corp_of = |cid: i64, explicit: i64| {
        if explicit > 0 { explicit } else { snap.characters.get(&cid).map(|c| c.corporation_id).unwrap_or(0) }
    };
    let mut kills: Vec<WarKill> = snap
        .killmails
        .values()
        .filter_map(|km| {
            if km.killed_unix > 0 {
                if km.killed_unix < w.started_unix {
                    return None;
                }
                if meta.finish_at_unix.is_some_and(|fin| km.killed_unix > fin) {
                    return None;
                }
            }
            let a = corp_of(km.attacker_character_id, km.attacker_corp_id);
            let v = corp_of(km.victim_character_id, km.victim_corp_id);
            let a_all = km
                .attacker_alliance_id
                .filter(|&id| id > 0)
                .or_else(|| resolve_entity_alliance_id(snap, a));
            let v_all = km
                .victim_alliance_id
                .filter(|&id| id > 0)
                .or_else(|| resolve_entity_alliance_id(snap, v));
            let agg_att = is_agg_side(a, a_all);
            let agg_vic = is_agg_side(v, v_all);
            let def_att = is_def_side(a, a_all, km.killed_unix);
            let def_vic = is_def_side(v, v_all, km.killed_unix);
            ((agg_att && !agg_vic && def_vic) || (def_att && !def_vic && agg_vic)).then(|| {
                let att_corp = if agg_att { agg } else { def };
                let vic_corp = if agg_vic { agg } else { def };
                let att_all = resolve_entity_alliance_id(snap, att_corp);
                let vic_all = resolve_entity_alliance_id(snap, vic_corp);
                WarKill {
                    kill_id: km.kill_id,
                    attacker_char: km.attacker_character_id,
                    attacker_corp: att_corp,
                    attacker_alliance: att_all,
                    attacker_ship_type_id: km.ship_type_id,
                    victim_char: km.victim_character_id,
                    victim_corp: vic_corp,
                    victim_alliance: vic_all,
                    victim_ship_type_id: km.ship_type_id,
                    kill_time_unix: km.killed_unix,
                    group_id: ctx.sde.types.get(&km.ship_type_id).map(|t| t.group_id).unwrap_or(0),
                    isk: km.total_value_isk,
                }
            })
        })
        .collect();
    kills.sort_by_key(|k| k.kill_id);
    let ally_ids = accepted_allies
        .into_iter()
        .filter(|n| is_ally_neg_active_at(snap, n, now))
        .map(|n| n.from_corp)
        .collect();
    Some(WarKillsSummary {
        aggressor_corp_id: agg,
        defender_corp_id: def,
        ally_corp_ids: ally_ids,
        kills,
    })
}

fn war_kills(ctx: &ServerContext, war_id: i64) -> Option<WarKillsSummary> {
    ctx.db.with_snapshot(|snap| war_kills_in_snap(ctx, snap, war_id))
}

/// Returns `true` when `corp_id` has executive authority for war side `side_id`
/// (either directly owning `side_id` or serving as `executor_corp_id` of alliance `side_id`).
fn handle_war_registry_unbind(
    ctx: &ServerContext,
    session: &SessionState,
    bound_id_opt: Option<i64>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let bound_id = bound_id_opt.unwrap_or(0) as u64;
    if bound_id > 0 {
        if let Err(e) = ctx.release_bound_object(session, bound_id) {
            return Some(Err(e));
        }
    }
    Some(Ok(PyRep::None))
}

fn handle_set_mutual_war_invites_blocked(
    ctx: &ServerContext,
    _session: &SessionState,
    has_war_director_role: bool,
    my_corp: i64,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    if !has_war_director_role {
        return err("CorpRoleDenied", "Changing mutual war invite settings requires CEO or Director role");
    }
    let blocked = matches!(args.first(), Some(PyRep::Bool(true)) | Some(PyRep::Integer(1)));
    ctx.db.with_mut_snapshot(|snap| {
        snap.war_state
            .blocked_mutual_invites
            .insert(my_corp, blocked);
    });
    Some(Ok(PyRep::Bool(true)))
}

fn handle_war_stat_unbind(
    ctx: &ServerContext,
    session: &SessionState,
    bound_id_opt: Option<i64>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let bound_id = bound_id_opt.unwrap_or(0) as u64;
    if bound_id > 0 {
        if let Err(e) = ctx.release_bound_object(session, bound_id) {
            return Some(Err(e));
        }
    }
    Some(Ok(PyRep::None))
}

fn handle_war_stat_set_session_check(
    _ctx: &ServerContext,
    _session: &SessionState,
    _args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    // Session check registration for war statistic monitoring; returns None
    Some(Ok(PyRep::None))
}

fn parse_war_unix_timestamp(rep: &PyRep) -> Option<u64> {
    const WIN_FILETIME_UNIX_EPOCH: i64 = 116_444_736_000_000_000;
    const WIN_TICKS_PER_SEC: i64 = 10_000_000;
    let raw = rep.as_i64()?;
    if raw >= WIN_FILETIME_UNIX_EPOCH {
        u64::try_from((raw - WIN_FILETIME_UNIX_EPOCH) / WIN_TICKS_PER_SEC).ok()
    } else if raw >= 0 {
        u64::try_from(raw).ok()
    } else {
        None
    }
}

#[derive(Clone, Copy)]
enum WarGmError {
    InvalidArgument,
    WarNotFound,
}

impl WarGmError {
    const fn code(self) -> &'static str {
        match self {
            Self::InvalidArgument => "InvalidArgument",
            Self::WarNotFound => "WarNotFound",
        }
    }

    fn with_message(self, message: impl Into<String>) -> BridgeRpcError {
        BridgeRpcError {
            code: self.code().into(),
            message: message.into(),
            client_error: None,
        }
    }
}

struct WarGmCall<'a> {
    war_id: i64,
    second: Option<&'a PyRep>,
    third: Option<&'a PyRep>,
}

fn parse_war_gm_call<'a>(method: &str, params: &'a [PyRep]) -> Result<WarGmCall<'a>, BridgeRpcError> {
    let [war_rep, rest @ ..] = params else {
        return Err(WarGmError::InvalidArgument
            .with_message(format!("warRegistry.{method} requires positive warID")));
    };
    let Some(war_id) = war_rep.as_i64().filter(|&id| id > 0) else {
        return Err(WarGmError::InvalidArgument
            .with_message(format!("warRegistry.{method} requires positive warID")));
    };
    let (second, third) = match rest {
        [s, t, ..] => (Some(s), Some(t)),
        [s] => (Some(s), None),
        [] => (None, None),
    };
    Ok(WarGmCall {
        war_id,
        second,
        third,
    })
}

fn handle_war_registry_gm_rpc(
    ctx: &ServerContext,
    session: &SessionState,
    method: &str,
    params: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    if (session.role
        & (eve_machonet::service_roles::ROLE_GMH
            | eve_machonet::service_roles::ROLE_GML
            | eve_machonet::service_roles::ROLE_GMS
            | eve_machonet::service_roles::ROLE_ADMIN))
        == 0
    {
        return err(
            "InsufficientRole",
            format!("warRegistry.{method} requires ROLE_GMH, ROLE_GML, ROLE_GMS, or ROLE_ADMIN"),
        );
    }
    let call = match parse_war_gm_call(method, params) {
        Ok(c) => c,
        Err(e) => return Some(Err(e)),
    };
    let war_id = call.war_id;
    let now = ctx.now_unix();
    let res = ctx.db.try_with_mut_snapshot(|snap| -> Result<Vec<(&'static str, PyRep, PyRep)>, BridgeRpcError> {
        let Some(war) = snap.wars.get(&war_id).cloned() else {
            return Err(WarGmError::WarNotFound.with_message(format!("War {war_id} not found")));
        };
        match method {
            "GMJoinDefender" => {
                let Some(ally_id) = call.second.and_then(|a| a.as_i64()).filter(|&id| id > 0) else {
                    return Err(WarGmError::InvalidArgument
                        .with_message("GMJoinDefender requires positive allyID"));
                };
                let ally_rec = wars::war_ally_record_for_entity(snap, &war, ally_id).map_err(rpc_err)?;
                let effective = match call.third {
                    Some(rep) => parse_war_unix_timestamp(rep).ok_or_else(|| {
                        WarGmError::InvalidArgument.with_message(
                            "GMJoinDefender effective time must be a valid timestamp",
                        )
                    })?,
                    None => now,
                };
                let prev_open = wars::gm_join_defender(snap, war_id, ally_id, ally_rec, effective);
                Ok(vec![
                    ("allyID", PyRep::None, PyRep::Integer(ally_id)),
                    ("openForAllies", PyRep::Bool(prev_open), PyRep::Bool(true)),
                ])
            }
            "GMActivateDefender" => {
                let ally_target = match call.second {
                    Some(PyRep::None) | None => None,
                    Some(rep) => {
                        let aid = rep.as_i64().filter(|&id| id > 0).ok_or_else(|| {
                            WarGmError::InvalidArgument
                                .with_message("GMActivateDefender allyID must be a positive integer")
                        })?;
                        Some((aid, wars::war_ally_record_for_entity(snap, &war, aid).map_err(rpc_err)?))
                    }
                };
                let (prev_active, prev_open) =
                    wars::gm_activate_defender(snap, war_id, ally_target, now).map_err(rpc_err)?;
                Ok(vec![
                    ("active", PyRep::Bool(prev_active), PyRep::Bool(true)),
                    ("openForAllies", PyRep::Bool(prev_open), PyRep::Bool(true)),
                ])
            }
            "GMDeactivateDefender" => {
                let ally_id = match call.second {
                    Some(PyRep::None) | None => None,
                    Some(rep) => Some(rep.as_i64().filter(|&id| id > 0).ok_or_else(|| {
                        WarGmError::InvalidArgument
                            .with_message("GMDeactivateDefender allyID must be a positive integer")
                    })?),
                };
                match wars::gm_deactivate_defender(snap, war_id, ally_id).map_err(rpc_err)? {
                    None => Ok(vec![(
                        "allyID",
                        ally_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                        PyRep::None,
                    )]),
                    Some(prev_open) => Ok(vec![(
                        "openForAllies",
                        PyRep::Bool(prev_open),
                        PyRep::Bool(false),
                    )]),
                }
            }
            "GMExtendAllyContract" => {
                let Some(ally_id) = call.second.and_then(|a| a.as_i64()).filter(|&id| id > 0) else {
                    return Err(WarGmError::InvalidArgument
                        .with_message("GMExtendAllyContract requires positive allyID"));
                };
                let Some(ts_or_delta) = call
                    .third
                    .and_then(parse_war_unix_timestamp)
                    .filter(|&v| v > 0)
                else {
                    return Err(WarGmError::InvalidArgument.with_message(
                        "GMExtendAllyContract requires positive extension duration or timestamp",
                    ));
                };
                wars::gm_extend_ally_contract(snap, war_id, ally_id, ts_or_delta, now)
                    .map_err(rpc_err)?;
                Ok(vec![("allyID", PyRep::Integer(ally_id), PyRep::Integer(ally_id))])
            }
            "GMSetDeclareTime" => {
                let Some(declared_unix) = call.second.and_then(parse_war_unix_timestamp) else {
                    return Err(WarGmError::InvalidArgument
                        .with_message("GMSetDeclareTime requires a valid timestamp"));
                };
                let upd = wars::set_declare_time(snap, war_id, declared_unix).map_err(rpc_err)?;
                Ok(vec![
                    (
                        "timeDeclared",
                        PyRep::Integer(unix_to_filetime(upd.prev_declared_unix)),
                        PyRep::Integer(unix_to_filetime(declared_unix)),
                    ),
                    (
                        "timeStarted",
                        PyRep::Integer(unix_to_filetime(upd.prev_started_unix)),
                        PyRep::Integer(unix_to_filetime(upd.new_started_unix)),
                    ),
                ])
            }
            "GMSetWarFinishTime" => {
                let new_finish = match call.second {
                    Some(PyRep::None) => None,
                    Some(rep) => Some(parse_war_unix_timestamp(rep).ok_or_else(|| {
                        WarGmError::InvalidArgument.with_message(
                            "GMSetWarFinishTime requires a valid timestamp or None",
                        )
                    })?),
                    None => {
                        return Err(WarGmError::InvalidArgument.with_message(
                            "GMSetWarFinishTime requires timeFinished or None",
                        ));
                    }
                };
                let prev_finish =
                    wars::set_finish_time(snap, war_id, new_finish, now).map_err(rpc_err)?;
                Ok(vec![(
                    "timeFinished",
                    opt_unix_to_filetime_py(prev_finish),
                    opt_unix_to_filetime_py(new_finish),
                )])
            }
            _ => unreachable!(),
        }
    });
    match res {
        Ok(changes) => {
            emit_war_changed(ctx, war_id, changes);
            Some(Ok(PyRep::Bool(true)))
        }
        Err(e) => Some(Err(e)),
    }
}

pub(super) fn handle_wars(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    bound_obj_id: Option<i64>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let is_war_rpc = matches!(
        (service, method),
        ("corpRegistry", "DeclareWarAgainst")
            | ("warRegistry", _)
            | ("warsInfoMgr", _)
            | ("warStatisticMgr", _)
            | ("peaceTreatyMgr", "GetPeaceTreatiesForSession")
            | ("mutualWarInviteMgr", _)
    );
    if !is_war_rpc {
        return None;
    }
    let (_, char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let arg_i64 = |i: usize| args.get(i).and_then(|a| a.as_i64());
    match (service, method) {
        (
            "warRegistry",
            "GMActivateDefender"
            | "GMDeactivateDefender"
            | "GMExtendAllyContract"
            | "GMJoinDefender"
            | "GMSetDeclareTime"
            | "GMSetWarFinishTime",
        ) => {
            return handle_war_registry_gm_rpc(ctx, session, method, args);
        }
        ("warStatisticMgr", "Bind") => {
            let object_id = arg_i64(0).unwrap_or(0);
            return match ctx.bind_object(session, "warStatisticMgr", object_id) {
                Ok(bound_id) => Some(Ok(PyRep::Integer(bound_id as i64))),
                Err(e) => Some(Err(e)),
            };
        }
        _ => {}
    }
    let my_corp = ctx.db.with_snapshot(|snap| snap.characters.get(&char_id).map(|c| c.corporation_id)).or(session.corporation_id).unwrap_or(0);
    let now = ctx.now_unix();
    if my_corp > 0 {
        crate::services::fleet_social::fleet_social_corps::promote_due_pending_corp_grants(ctx, my_corp);
    }
    sync_wars(ctx);
    let has_war_director_role = ctx.db.with_snapshot(|snap| {
        snap.corporations.get(&my_corp).is_some_and(|_| {
            let (is_ceo, eff_roles) =
                crate::services::social_corp_fleet::social_corp_projects::effective_corp_member_roles(
                    snap, my_corp, char_id, 0, now,
                );
            is_ceo || (eff_roles & eve_db::corp_roles::ROLE_DIRECTOR) != 0
        })
    });

    match (service, method) {
        ("corpRegistry", "DeclareWarAgainst") | ("warRegistry", "DeclareWarAgainst") => {
            let defender = arg_i64(0).unwrap_or(0);
            let hq_arg = arg_i64(1);
            let treaty = ctx.db.with_snapshot(|snap| {
                snap.war_state.peace_treaties.iter().any(|t| {
                    t.expiry_unix > now
                        && ((t.corp_a == my_corp && t.corp_b == defender)
                            || (t.corp_a == defender && t.corp_b == my_corp))
                })
            });
            if treaty {
                return err("PeaceTreatyActive", "An active 2-week peace treaty prevents redeclaring war");
            }
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(agg) = snap.corporations.get(&my_corp) else {
                    return Err(BridgeRpcError { code: "NpcCorporationCannotDeclareWar".into(), message: "Only player corporations can declare war".into(), client_error: None });
                };
                if agg.ceo_id != char_id {
                    return Err(BridgeRpcError { code: "CeoRoleRequired".into(), message: "Only the corporation CEO can declare war".into(), client_error: None });
                }
                if defender == my_corp || !snap.corporations.contains_key(&defender) || same_alliance(snap, my_corp, defender) {
                    return Err(BridgeRpcError { code: "InvalidWarTarget".into(), message: format!("Corporation {defender} is not a valid war target"), client_error: None });
                }
                if already_at_war(snap, my_corp, defender) {
                    return Err(BridgeRpcError { code: "AlreadyAtWar".into(), message: "These corporations are already at war".into(), client_error: None });
                }
                let owned_ok = |s: &&eve_db::UpwellStructureRecord| {
                    is_valid_war_hq(ctx, &snap.corporations, my_corp, s)
                };
                let hq = match hq_arg {
                    Some(id) => snap.structures.get(&id).filter(owned_ok).map(|s| s.structure_id),
                    // No explicit War HQ: use the attacker's lowest-ID eligible structure.
                    None => snap.structures.values().filter(owned_ok).map(|s| s.structure_id).min(),
                };
                let Some(hq) = hq else {
                    return Err(BridgeRpcError { code: "WarHqRequired".into(), message: "Declaring war requires a War HQ structure owned by the attacking corporation".into(), client_error: None });
                };
                if !war_eligible(snap, defender) {
                    return Err(BridgeRpcError { code: "DefenderNotWarEligible".into(), message: format!("Corporation {defender} owns no structures and is not war-eligible"), client_error: None });
                }
                let war_id = wars::declare_war(
                    snap,
                    my_corp,
                    defender,
                    hq,
                    now,
                    IskCents::from_isk(WAR_DECLARATION_FEE_ISK),
                )
                .map_err(rpc_err)?;
                Ok((war_id, hq))
            });
            let (war_id, hq) = match res {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            emit_war_changed(
                ctx,
                war_id,
                vec![
                    ("warID", PyRep::None, PyRep::Integer(war_id)),
                    ("declaredByID", PyRep::None, PyRep::Integer(my_corp)),
                    ("againstID", PyRep::None, PyRep::Integer(defender)),
                ],
            );
            Some(Ok(PyRep::keyval([
                ("warID", PyRep::Integer(war_id)),
                ("declaredByID", PyRep::Integer(my_corp)),
                ("againstID", PyRep::Integer(defender)),
                ("warHQ", PyRep::Integer(hq)),
                ("timeDeclared", PyRep::Integer(unix_to_filetime(now))),
                ("timeStarted", PyRep::Integer(unix_to_filetime(now + WAR_TIMER_SECS))),
                ("billCost", PyRep::Real(WAR_DECLARATION_FEE_ISK as f64)),
            ])))
        }

        ("warRegistry", "GetWars") => {
            let filter = arg_i64(0).unwrap_or(my_corp);
            let mut rows: Vec<(i64, Vec<PyRep>)> = ctx.db.with_snapshot(|snap| {
                snap.wars
                    .values()
                    .filter(|w| w.aggressor_corp_id == filter || w.defender_corp_id == filter)
                    .map(|w| {
                        let meta = snap.war_state.wars.get(&w.war_id).cloned().unwrap_or_default();
                        (w.war_id, war_row(w, &meta, now))
                    })
                    .collect()
            });
            rows.sort_by_key(|r| r.0);
            // The bound `warRegistry` moniker answers `{warID: war}`: `war_cso.GetWars`
            // passes it to `PopulateCacheAndCleanupWars`, which iterates `wars.itervalues()`
            // (`eve/client/script/ui/services/wars/war_cso.pyj:128-138`).
            Some(Ok(PyRep::Dict(
                rows.into_iter()
                    .map(|(id, row)| (PyRep::Integer(id), PyRep::keyval(WAR_COLUMNS.iter().copied().zip(row))))
                    .collect(),
            )))
        }

        // parity: rpc:warsInfoMgr.GetWarsByOwnerID
        // parity: rpc:warsInfoMgr.GetWarsByOwners
        ("warsInfoMgr", "GetWarsByOwnerID") | ("warsInfoMgr", "GetWarsByOwners") => {
            let filter = arg_i64(0).unwrap_or(my_corp);
            let owner_list: Vec<i64> = match args.first() {
                Some(PyRep::List(items)) => items.iter().filter_map(|it| it.as_i64()).collect(),
                _ => vec![filter],
            };
            let mut rows: Vec<(i64, Vec<PyRep>)> = ctx.db.with_snapshot(|snap| {
                snap.wars
                    .values()
                    .filter(|w| owner_list.contains(&w.aggressor_corp_id) || owner_list.contains(&w.defender_corp_id))
                    .map(|w| {
                        let meta = snap.war_state.wars.get(&w.war_id).cloned().unwrap_or_default();
                        (w.war_id, war_row(w, &meta, now))
                    })
                    .collect()
            });
            rows.sort_by_key(|r| r.0);
            let raw_rows: Vec<Vec<PyRep>> = rows.into_iter().map(|r| r.1).collect();
            if method == "GetWarsByOwnerID" {
                Some(Ok(war_crowset(raw_rows)))
            } else {
                Some(Ok(PyRep::rowset(WAR_COLUMNS.to_vec(), raw_rows)))
            }
        }

        // parity: rpc:warsInfoMgr.GetTop50
        ("warsInfoMgr", "GetTop50") => {
            let mut rows: Vec<(i64, usize, u64, Vec<PyRep>)> = ctx.db.with_snapshot(|snap| {
                snap.wars
                    .values()
                    .filter(|w| w.active)
                    .map(|w| {
                        let meta = snap.war_state.wars.get(&w.war_id).cloned().unwrap_or_default();
                        let (k_cnt, k_isk) = war_kills_in_snap(ctx, snap, w.war_id)
                            .map(|summary| {
                                (
                                    summary.kills.len(),
                                    summary.kills.iter().map(|k| k.isk).sum::<f64>().to_bits(),
                                )
                            })
                            .unwrap_or((0, 0));
                        (w.war_id, k_cnt, k_isk, war_row(w, &meta, now))
                    })
                    .collect()
            });
            rows.sort_by_key(|r| (std::cmp::Reverse(r.1), std::cmp::Reverse(r.2), r.0));
            rows.truncate(50);
            Some(Ok(PyRep::rowset(WAR_COLUMNS.to_vec(), rows.into_iter().map(|r| r.3).collect())))
        }

        // parity: rpc:warsInfoMgr.GetWarsRequiringAssistance
        ("warsInfoMgr", "GetWarsRequiringAssistance") => {
            let mut rows: Vec<(i64, Vec<PyRep>)> = ctx.db.with_snapshot(|snap| {
                snap.wars
                    .values()
                    .filter_map(|w| {
                        let meta = snap.war_state.wars.get(&w.war_id).cloned().unwrap_or_default();
                        if w.active && meta.open_for_allies {
                            Some((w.war_id, war_row(w, &meta, now)))
                        } else {
                            None
                        }
                    })
                    .collect()
            });
            rows.sort_by_key(|r| r.0);
            Some(Ok(PyRep::rowset(WAR_COLUMNS.to_vec(), rows.into_iter().map(|r| r.1).collect())))
        }

        // parity: rpc:warsInfoMgr.GetWarsForStructure
        ("warsInfoMgr", "GetWarsForStructure") => {
            let structure_id = arg_i64(0).unwrap_or(0);
            let mut rows: Vec<(i64, Vec<PyRep>)> = ctx.db.with_snapshot(|snap| {
                snap.wars
                    .values()
                    .filter_map(|w| {
                        let meta = snap.war_state.wars.get(&w.war_id).cloned().unwrap_or_default();
                        if meta.war_hq_id == Some(structure_id) {
                            Some((w.war_id, war_row(w, &meta, now)))
                        } else {
                            None
                        }
                    })
                    .collect()
            });
            rows.sort_by_key(|r| r.0);
            Some(Ok(PyRep::rowset(WAR_COLUMNS.to_vec(), rows.into_iter().map(|r| r.1).collect())))
        }

        ("warRegistry", "Bind") => {
            let object_id = arg_i64(0).unwrap_or(my_corp);
            match ctx.bind_object(session, "warRegistry", object_id) {
                Ok(bound_id) => Some(Ok(PyRep::Integer(bound_id as i64))),
                Err(e) => Some(Err(e)),
            }
        }

        ("warRegistry", "UnBind") => handle_war_registry_unbind(ctx, session, arg_i64(0)),
        ("warRegistry", "Unbind") => handle_war_registry_unbind(ctx, session, arg_i64(0)),

        ("warRegistry", "timeDeclared") => {
            let war_id = arg_i64(0).filter(|&id| id > 0).or_else(|| {
                ctx.db.with_snapshot(|snap| {
                    snap.wars.values().find(|w| w.aggressor_corp_id == my_corp || w.defender_corp_id == my_corp).map(|w| w.war_id)
                })
            });
            let Some(wid) = war_id else {
                return err("WarNotFound", "War not specified or not found");
            };
            let Some(w) = ctx.db.with_snapshot(|snap| snap.wars.get(&wid).cloned()) else {
                return err("WarNotFound", format!("War {wid} not found"));
            };
            Some(Ok(PyRep::Integer(unix_to_filetime(w.started_unix))))
        }

        ("warRegistry", "timeStarted") => {
            let war_id = arg_i64(0).filter(|&id| id > 0).or_else(|| {
                ctx.db.with_snapshot(|snap| {
                    snap.wars.values().find(|w| w.aggressor_corp_id == my_corp || w.defender_corp_id == my_corp).map(|w| w.war_id)
                })
            });
            let Some(wid) = war_id else {
                return err("WarNotFound", "War not specified or not found");
            };
            let Some(w) = ctx.db.with_snapshot(|snap| snap.wars.get(&wid).cloned()) else {
                return err("WarNotFound", format!("War {wid} not found"));
            };
            Some(Ok(PyRep::Integer(unix_to_filetime(wars::war_started_unix(&w)))))
        }

        ("warRegistry", "timeFinished") => {
            let war_id = arg_i64(0).filter(|&id| id > 0).or_else(|| {
                ctx.db.with_snapshot(|snap| {
                    snap.wars.values().find(|w| w.aggressor_corp_id == my_corp || w.defender_corp_id == my_corp).map(|w| w.war_id)
                })
            });
            let Some(wid) = war_id else {
                return err("WarNotFound", "War not specified or not found");
            };
            let Some(meta) = ctx.db.with_snapshot(|snap| {
                let _w = snap.wars.get(&wid)?;
                Some(snap.war_state.wars.get(&wid).cloned().unwrap_or_default())
            }) else {
                return err("WarNotFound", format!("War {wid} not found"));
            };
            Some(Ok(opt_unix_to_filetime_py(meta.finish_at_unix)))
        }

        ("warsInfoMgr", "GetPublicWarInfo") => {
            let war_id = arg_i64(0).unwrap_or(0);
            let Some((w, meta)) = ctx.db.with_snapshot(|snap| {
                let w = snap.wars.get(&war_id).cloned()?;
                let meta = snap.war_state.wars.get(&war_id).cloned().unwrap_or_default();
                Some((w, meta))
            }) else {
                return err("WarNotFound", format!("War {war_id} not found"));
            };
            let row = war_row(&w, &meta, now);
            Some(Ok(PyRep::List(vec![PyRep::keyval(WAR_COLUMNS.iter().copied().zip(row))])))
        }

        ("warsInfoMgr" | "warRegistry", "CreateSurrenderNegotiation" | "CreateWarAllyOffer") => {
            if !has_war_director_role {
                return err("CorpRoleDenied", "War negotiations require CEO or Director role");
            }
            let war_id = arg_i64(0).unwrap_or(0);
            let isk = args.get(1).and_then(|a| a.as_f64()).unwrap_or(0.0);
            let message = args.get(2).and_then(|a| a.as_str()).unwrap_or("").to_string();
            if !isk.is_finite() || isk < 0.0 {
                return err("InvalidIskValue", "ISK value must be a non-negative number");
            }
            let (
                Some(w),
                open_for_allies,
                war_already_ending,
                is_player_corp,
                can_negotiate_surrender,
                is_own_war_member,
            ) = ctx.db.with_snapshot(|snap| {
                let w_opt = snap.wars.get(&war_id).cloned();
                let can_negotiate_surrender = w_opt.as_ref().is_some_and(|w| {
                    corp_represents_war_side(snap, my_corp, w.aggressor_corp_id)
                        || corp_represents_war_side(snap, my_corp, w.defender_corp_id)
                });
                let is_own_war_member = w_opt.as_ref().is_some_and(|w| {
                    my_corp == w.aggressor_corp_id
                        || my_corp == w.defender_corp_id
                        || same_alliance(snap, my_corp, w.aggressor_corp_id)
                        || same_alliance(snap, my_corp, w.defender_corp_id)
                });
                (
                    w_opt,
                    snap.war_state.wars.get(&war_id).is_some_and(|m| m.open_for_allies),
                    snap.war_state.wars.get(&war_id).is_some_and(|m| m.finish_at_unix.is_some()),
                    snap.corporations.contains_key(&my_corp),
                    can_negotiate_surrender,
                    is_own_war_member,
                )
            }) else {
                return err("WarNotFound", format!("War {war_id} not found"));
            };
            if !w.active {
                return err("WarNotActive", format!("War {war_id} has ended"));
            }
            let kind = if method == "CreateSurrenderNegotiation" { NegKind::Surrender } else { NegKind::Ally };
            match kind {
                NegKind::Surrender if !can_negotiate_surrender => {
                    return err("NotWarParticipant", "Only war participants can negotiate surrender")
                }
                NegKind::Surrender if war_already_ending => {
                    return err("WarAlreadyEnding", format!("War {war_id} surrender has already been accepted"));
                }
                NegKind::Ally if is_own_war_member => {
                    return err("CannotAllyOwnWar", "War participants cannot offer themselves as allies")
                }
                NegKind::Ally if !open_for_allies => {
                    return err("WarNotOpenForAllies", "The defender has not opened this war for allies")
                }
                NegKind::Ally if !is_player_corp => {
                    return err("NpcCorporationCannotAlly", "Only player corporations can ally in wars")
                }
                _ => {}
            }
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                wars::create_negotiation(
                    snap,
                    war_id,
                    kind,
                    my_corp,
                    IskCents::from_f64_isk(isk),
                    message.clone(),
                    now,
                )
                .map_err(rpc_err)
            });
            let id = match res {
                Ok(id) => id,
                Err(e) => return Some(Err(e)),
            };
            Some(Ok(PyRep::keyval([
                ("negotiationID", PyRep::Integer(id)),
                ("warID", PyRep::Integer(war_id)),
                ("ownerID", PyRep::Integer(my_corp)),
                ("iskValue", PyRep::Real(isk)),
                ("status", PyRep::String("Offered".into())),
            ])))
        }

        ("warsInfoMgr" | "warRegistry", "GetSurrenderNegotiations") => {
            Some(neg_rows_for_caller(ctx, arg_i64(0).unwrap_or(0), NegKind::Surrender, my_corp)
                .map(|rows| PyRep::rowset(NEG_COLUMNS.to_vec(), rows)))
        }
        ("warsInfoMgr" | "warRegistry", "GetAllyNegotiations") => {
            Some(neg_rows_for_caller(ctx, arg_i64(0).unwrap_or(0), NegKind::Ally, my_corp)
                .map(|rows| PyRep::rowset(NEG_COLUMNS.to_vec(), rows)))
        }
        ("warRegistry", "GetNegotiations") => {
            handle_get_negotiations(ctx, bound_obj_id, my_corp, args)
        }
        ("warRegistry", "GetWarNegotiation") => {
            let neg_id = arg_i64(0).unwrap_or(1);
            let wid_hint = bound_obj_id.unwrap_or(1);
            let res = ctx.db.with_snapshot(|snap| {
                let neg_opt = snap.war_state.negotiations.iter().find(|n| n.id == neg_id);
                let wid = neg_opt.map(|n| n.war_id).unwrap_or(wid_hint);
                let allowed = is_war_participant_corp(snap, wid, my_corp, now)
                    || neg_opt.is_some_and(|n| n.from_corp == my_corp);
                if !allowed {
                    return Err(BridgeRpcError {
                        code: "NotWarParticipant".into(),
                        message: "Only war participants or the negotiation offeror may inspect this negotiation".into(),
                        client_error: None,
                    });
                }
                let (agg, def) = snap
                    .wars
                    .get(&wid)
                    .map(|w| (w.aggressor_corp_id, w.defender_corp_id))
                    .unwrap_or((my_corp, my_corp));
                let (isk_val, desc, kind_id) = neg_opt
                    .map(|n| {
                        (
                            n.isk.to_wire_f64(),
                            n.message.clone(),
                            if n.kind == NegKind::Ally { 2 } else { 0 },
                        )
                    })
                    .unwrap_or((0.0, String::new(), 0));
                Ok(PyRep::keyval([
                    ("warNegotiationID", PyRep::Integer(neg_id)),
                    ("warID", PyRep::Integer(wid)),
                    ("declaredByID", PyRep::Integer(agg)),
                    ("againstID", PyRep::Integer(def)),
                    ("ownerID1", PyRep::Integer(agg)),
                    ("ownerID2", PyRep::Integer(def)),
                    ("iskValue", PyRep::Real(isk_val)),
                    ("description", PyRep::String(desc)),
                    ("warNegotiationTypeID", PyRep::Integer(kind_id)),
                ]))
            });
            Some(res)
        }

        ("warsInfoMgr" | "warRegistry", "AcceptSurrender" | "DeclineSurrender") => {
            if !has_war_director_role {
                return err("CorpRoleDenied", "Acting on war negotiations requires CEO or Director role");
            }
            let neg_id = arg_i64(0).unwrap_or(0);
            let Some((n, w_opt, allowed)) = ctx.db.with_snapshot(|snap| {
                let n = snap
                    .war_state
                    .negotiations
                    .iter()
                    .find(|n| n.id == neg_id && n.status == "Offered")?
                    .clone();
                let w = snap.wars.get(&n.war_id).cloned();
                let allowed = w.as_ref().is_some_and(|w| {
                    let caller_is_agg = corp_represents_war_side(snap, my_corp, w.aggressor_corp_id);
                    let caller_is_def = corp_represents_war_side(snap, my_corp, w.defender_corp_id);
                    let from_is_agg = corp_represents_war_side(snap, n.from_corp, w.aggressor_corp_id);
                    let from_is_def = corp_represents_war_side(snap, n.from_corp, w.defender_corp_id);
                    my_corp != n.from_corp
                        && ((caller_is_agg && from_is_def) || (caller_is_def && from_is_agg))
                });
                Some((n, w, allowed))
            }) else {
                return err("NegotiationNotFound", format!("No open negotiation {neg_id}"));
            };
            let Some(w) = w_opt else {
                return err("WarNotFound", format!("War {} not found", n.war_id));
            };
            if n.kind != NegKind::Surrender {
                return err("NegotiationNotFound", format!("Negotiation {neg_id} is not of the requested kind"));
            }
            if !allowed {
                return err("NotNegotiationCounterparty", "Your corporation cannot act on this negotiation");
            }
            let accepting = method == "AcceptSurrender";
            if accepting && !w.active {
                return err("WarNotActive", format!("War {} has ended", n.war_id));
            }
            if accepting
                && ctx.db.with_snapshot(|snap| {
                    snap.war_state
                        .wars
                        .get(&n.war_id)
                        .is_some_and(|wm| wm.finish_at_unix.is_some())
                })
            {
                return err("WarAlreadyEnding", format!("War {} surrender has already been accepted", n.war_id));
            }
            let status = if accepting { "Accepted" } else { "Declined" };
            let outcome = match ctx.db.try_with_mut_snapshot(|snap| {
                wars::resolve_surrender_negotiation(snap, neg_id, my_corp, accepting, now)
                    .map_err(rpc_err)
            }) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let mut out = vec![
                ("negotiationID", PyRep::Integer(neg_id)),
                ("warID", PyRep::Integer(n.war_id)),
                ("status", PyRep::String(status.into())),
            ];
            if let Some((effective, expiry)) = outcome {
                out.push(("timeFinished", PyRep::Integer(unix_to_filetime(effective))));
                out.push(("peaceTreatyExpiry", PyRep::Integer(unix_to_filetime(expiry))));
                emit_war_changed(
                    ctx,
                    n.war_id,
                    vec![(
                        "timeFinished",
                        PyRep::None,
                        PyRep::Integer(unix_to_filetime(effective)),
                    )],
                );
            } else {
                emit_war_changed(ctx, n.war_id, vec![]);
            }
            Some(Ok(PyRep::keyval(out)))
        }

        (
            "warsInfoMgr" | "warRegistry",
            "AcceptAllyNegotiation" | "DeclineAllyOffer" | "RetractWarAllyOffer",
        ) => {
            if !has_war_director_role {
                return err("CorpRoleDenied", "Acting on war negotiations requires CEO or Director role");
            }
            let neg_id = arg_i64(0).unwrap_or(0);
            let action = match method {
                "AcceptAllyNegotiation" => AllyNegotiationAction::Accept,
                "RetractWarAllyOffer" => AllyNegotiationAction::Retract,
                _ => AllyNegotiationAction::Decline,
            };
            let outcome = match ctx.db.try_with_mut_snapshot(|snap| {
                wars::resolve_ally_negotiation(snap, neg_id, my_corp, action, now)
                    .map_err(rpc_err)
            }) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let mut out = vec![
                ("negotiationID", PyRep::Integer(neg_id)),
                ("warID", PyRep::Integer(outcome.war_id)),
                ("status", PyRep::String(action.status_str().into())),
            ];
            if let Some(effective) = outcome.effective_unix {
                out.push(("allyID", PyRep::Integer(outcome.ally_id)));
                out.push(("timeStarted", PyRep::Integer(unix_to_filetime(effective))));
                emit_war_changed(
                    ctx,
                    outcome.war_id,
                    vec![("allyID", PyRep::None, PyRep::Integer(outcome.ally_id))],
                );
            }
            Some(Ok(PyRep::keyval(out)))
        }

        ("warsInfoMgr" | "warRegistry", "RetractMutualWar") => {
            if !has_war_director_role {
                return err("CorpRoleDenied", "Retracting mutual war requires CEO or Director role");
            }
            let war_id = arg_i64(0).or(bound_obj_id).unwrap_or(0);
            let finish = match ctx.db.try_with_mut_snapshot(|snap| {
                wars::retract_mutual_war(snap, war_id, my_corp, now).map_err(rpc_err)
            }) {
                Ok(f) => f,
                Err(e) => return Some(Err(e)),
            };
            let finish_ft = unix_to_filetime(finish);
            emit_war_changed(
                ctx,
                war_id,
                vec![("timeFinished", PyRep::None, PyRep::Integer(finish_ft))],
            );
            Some(Ok(PyRep::keyval([("warID", PyRep::Integer(war_id)), ("timeFinished", PyRep::Integer(finish_ft))])))
        }

        ("warsInfoMgr" | "warRegistry", "SetOpenForAllies") => {
            if !has_war_director_role {
                return err("CorpRoleDenied", "Opening a war for allies requires CEO or Director role");
            }
            let (war_id, open) = if service == "warRegistry" && args.len() == 1 {
                (
                    bound_obj_id.unwrap_or(1),
                    matches!(args.first(), Some(PyRep::Bool(true)) | Some(PyRep::Integer(1))),
                )
            } else {
                (
                    arg_i64(0).or(bound_obj_id).unwrap_or(0),
                    matches!(args.get(1), Some(PyRep::Bool(true)) | Some(PyRep::Integer(1))),
                )
            };
            let Some(w) = ctx.db.with_snapshot(|snap| snap.wars.get(&war_id).cloned()) else {
                return err("WarNotFound", format!("War {war_id} not found"));
            };
            if my_corp != w.defender_corp_id {
                return err("NotWarDefender", "Only the defender can open a war for allies");
            }
            let prev = ctx
                .db
                .with_mut_snapshot(|snap| wars::set_open_for_allies(snap, war_id, open));
            emit_war_changed(
                ctx,
                war_id,
                vec![("openForAllies", PyRep::Bool(prev), PyRep::Bool(open))],
            );
            Some(Ok(PyRep::keyval([("warID", PyRep::Integer(war_id)), ("openForAllies", PyRep::Bool(open))])))
        }

        ("warsInfoMgr" | "warRegistry", "GetAllyRowsByWarID") => {
            let war_id = arg_i64(0).or(bound_obj_id).unwrap_or(0);
            let rows: Vec<Vec<PyRep>> = ctx.db.with_snapshot(|snap| {
                snap.war_state
                    .negotiations
                    .iter()
                    .filter(|n| n.war_id == war_id && n.kind == NegKind::Ally && n.status == "Accepted")
                    .map(|n| {
                        let start = n.effective_unix.unwrap_or(now);
                        vec![
                            PyRep::Integer(n.from_corp),
                            PyRep::Integer(unix_to_filetime(start)),
                            PyRep::Real(n.isk.to_wire_f64()),
                            PyRep::Bool(is_ally_neg_active_at(snap, n, now)),
                        ]
                    })
                    .collect()
            });
            Some(Ok(PyRep::rowset(vec!["allyID", "timeStarted", "iskValue", "active"], rows)))
        }

        ("peaceTreatyMgr", "GetPeaceTreatiesForSession") => {
            struct PeaceTreatyReply {
                war_id: i64,
                other_owner_id: i64,
                expiry_unix: u64,
            }
            impl PeaceTreatyReply {
                fn into_pyrep(self) -> PyRep {
                    PyRep::keyval([
                        ("warID", PyRep::Integer(self.war_id)),
                        ("otherOwnerID", PyRep::Integer(self.other_owner_id)),
                        (
                            "expiryDate",
                            PyRep::Integer(unix_to_filetime(self.expiry_unix)),
                        ),
                        ("peaceReason", PyRep::Integer(4)),
                    ])
                }
            }
            let my_alliance = session.alliance_id.filter(|&a| a > 0);
            let (outgoing, incoming) = ctx.db.with_snapshot(|snap| {
                let mut outgoing = Vec::new();
                let mut incoming = Vec::new();
                for t in &snap.war_state.peace_treaties {
                    if t.expiry_unix <= now {
                        continue;
                    }
                    let is_agg = t.corp_a == my_corp || Some(t.corp_a) == my_alliance;
                    let is_def = t.corp_b == my_corp || Some(t.corp_b) == my_alliance;
                    if !is_agg && !is_def {
                        continue;
                    }
                    let other_owner_id = if is_agg { t.corp_b } else { t.corp_a };
                    let entry = PeaceTreatyReply {
                        war_id: t.war_id,
                        other_owner_id,
                        expiry_unix: t.expiry_unix,
                    }
                    .into_pyrep();
                    if is_agg {
                        outgoing.push(entry);
                    } else {
                        incoming.push(entry);
                    }
                }
                (outgoing, incoming)
            });
            Some(Ok(PyRep::Tuple(vec![
                PyRep::List(outgoing),
                PyRep::List(incoming),
            ])))
        }

        ("mutualWarInviteMgr", "IsCorpInvitesBlockedPlayer") => {
            let target_corp = arg_i64(0).unwrap_or(my_corp);
            let blocked = ctx.db.with_snapshot(|snap| {
                snap.war_state
                    .blocked_mutual_invites
                    .get(&target_corp)
                    .copied()
                    .unwrap_or(false)
            });
            Some(Ok(PyRep::Bool(blocked)))
        }

        ("mutualWarInviteMgr", "SetInvitesBlockedByPlayer") => {
            handle_set_mutual_war_invites_blocked(ctx, session, has_war_director_role, my_corp, args)
        }

        ("mutualWarInviteMgr", "SendInviteByPlayer") => {
            if !has_war_director_role {
                return err("CorpRoleDenied", "Sending mutual war invites requires CEO or Director role");
            }
            let to_corp = arg_i64(0).unwrap_or(0);
            let (valid, at_war, blocked) = ctx.db.with_snapshot(|snap| {
                (
                    snap.corporations.contains_key(&my_corp)
                        && snap.corporations.contains_key(&to_corp)
                        && to_corp != my_corp
                        && !same_alliance(snap, my_corp, to_corp),
                    already_at_war(snap, my_corp, to_corp),
                    snap.war_state
                        .blocked_mutual_invites
                        .get(&to_corp)
                        .copied()
                        .unwrap_or(false),
                )
            });
            if !valid {
                return err("InvalidWarTarget", format!("Corporation {to_corp} cannot be invited to a mutual war"));
            }
            if at_war {
                return err("AlreadyAtWar", "These corporations are already at war");
            }
            if blocked {
                return err("CorpInvitesBlocked", format!("Corporation {to_corp} has blocked mutual war invites"));
            }
            ctx.db.with_mut_snapshot(|snap| {
                snap.war_state
                    .mutual_invites
                    .insert((my_corp, to_corp), now);
            });
            emit_mutual_wars_updated(ctx, char_id, my_corp, to_corp);
            Some(Ok(PyRep::keyval([("fromID", PyRep::Integer(my_corp)), ("toID", PyRep::Integer(to_corp)), ("sentDate", PyRep::Integer(unix_to_filetime(now)))])))
        }

        ("mutualWarInviteMgr", "GetPendingInvitesForSession") => {
            let rows: Vec<Vec<PyRep>> = ctx.db.with_snapshot(|snap| {
                snap.war_state
                    .mutual_invites
                    .iter()
                    .filter(|(&(f, t), _)| f == my_corp || t == my_corp)
                    .map(|(&(f, t), &ts)| {
                        vec![
                            PyRep::Integer(f),
                            PyRep::Integer(t),
                            PyRep::Integer(unix_to_filetime(ts)),
                        ]
                    })
                    .collect()
            });
            Some(Ok(PyRep::rowset(vec!["fromID", "toID", "sentDate"], rows)))
        }

        ("mutualWarInviteMgr", "WithdrawInviteByPlayer") => {
            if !has_war_director_role {
                return err("CorpRoleDenied", "Withdrawing mutual war invites requires CEO or Director role");
            }
            let to_corp = arg_i64(0).unwrap_or(0);
            let removed = ctx.db.with_mut_snapshot(|snap| {
                snap.war_state
                    .mutual_invites
                    .remove(&(my_corp, to_corp))
                    .is_some()
            });
            if removed {
                emit_mutual_wars_updated(ctx, char_id, my_corp, to_corp);
            }
            Some(Ok(PyRep::Bool(removed)))
        }

        ("mutualWarInviteMgr", "RespondToInviteByPlayer") => {
            if !has_war_director_role {
                return err("CorpRoleDenied", "Responding to mutual war invites requires CEO or Director role");
            }
            let from_corp = arg_i64(0).unwrap_or(0);
            let accept = match args.get(1) {
                Some(PyRep::Bool(b)) => *b,
                Some(PyRep::Integer(i)) => *i != 0,
                _ => false,
            };
            let had_invite = ctx.db.with_snapshot(|snap| {
                snap.war_state.mutual_invites.contains_key(&(from_corp, my_corp))
            });
            if !had_invite {
                return err("MutualWarInviteNotFound", format!("No pending mutual war invite from {from_corp}"));
            }
            if !accept {
                ctx.db.with_mut_snapshot(|snap| {
                    snap.war_state.mutual_invites.remove(&(from_corp, my_corp));
                });
                emit_mutual_wars_updated(ctx, char_id, my_corp, from_corp);
                return Some(Ok(PyRep::keyval([("accepted", PyRep::Bool(false)), ("warID", PyRep::None)])));
            }
            let tx_res = ctx.db.try_with_mut_snapshot(|snap| {
                wars::accept_mutual_war_invite(snap, from_corp, my_corp, now).map_err(rpc_err)
            });
            let war_id = match tx_res {
                Ok(MutualWarAcceptOutcome::Started { war_id }) => war_id,
                Ok(MutualWarAcceptOutcome::RejectedSameAlliance) => {
                    emit_mutual_wars_updated(ctx, char_id, my_corp, from_corp);
                    return err(
                        "InvalidWarTarget",
                        "Corporations in the same alliance cannot enter a mutual war",
                    );
                }
                Ok(MutualWarAcceptOutcome::RejectedAlreadyAtWar) => {
                    emit_mutual_wars_updated(ctx, char_id, my_corp, from_corp);
                    return err("AlreadyAtWar", "These corporations are already at war");
                }
                Err(e) => return Some(Err(e)),
            };
            emit_mutual_wars_updated(ctx, char_id, my_corp, from_corp);
            emit_war_changed(
                ctx,
                war_id,
                vec![
                    ("warID", PyRep::None, PyRep::Integer(war_id)),
                    ("declaredByID", PyRep::None, PyRep::Integer(from_corp)),
                    ("againstID", PyRep::None, PyRep::Integer(my_corp)),
                    ("mutual", PyRep::None, PyRep::Bool(true)),
                ],
            );
            Some(Ok(PyRep::keyval([("accepted", PyRep::Bool(true)), ("warID", PyRep::Integer(war_id)), ("mutual", PyRep::Bool(true))])))
        }

        ("warStatisticMgr", "UnBind") => handle_war_stat_unbind(ctx, session, arg_i64(0)),
        ("warStatisticMgr", "Unbind") => handle_war_stat_unbind(ctx, session, arg_i64(0)),

        ("warStatisticMgr", "SetSessionCheck") => {
            handle_war_stat_set_session_check(ctx, session, args)
        }

        ("warStatisticMgr", "GetBaseInfo") => {
            let war_id = resolve_war_stat_id(ctx, session, arg_i64(0), bound_obj_id, my_corp);
            let Some(WarKillsSummary {
                aggressor_corp_id: agg_id,
                defender_corp_id: def_id,
                ally_corp_ids: allies,
                kills,
            }) = war_kills(ctx, war_id)
            else {
                return err("WarNotFound", format!("War {war_id} not found"));
            };
            let (mutual, open_for_allies, retracted_unix, war_hq, ally_windows) =
                ctx.db.with_snapshot(|snap| {
                    let mutual = snap.wars.get(&war_id).is_some_and(|w| w.mutual);
                    let meta = snap.war_state.wars.get(&war_id).cloned().unwrap_or_default();
                    let retracted_unix = snap
                        .war_lifecycle_meta
                        .get(&war_id)
                        .and_then(|lm| lm.retracted_unix);
                    let windows: Vec<(i64, u64, u64)> = allies
                        .iter()
                        .map(|&aid| {
                            let (started, finished) =
                                wars::ally_contract_window(snap, war_id, aid, now);
                            (aid, started, finished)
                        })
                        .collect();
                    (
                        mutual,
                        meta.open_for_allies,
                        retracted_unix,
                        meta.war_hq_id,
                        windows,
                    )
                });
            let (mut k_agg, mut k_def, mut isk_agg, mut isk_def) = (0i64, 0i64, 0.0f64, 0.0f64);
            for k in &kills {
                if k.attacker_corp == agg_id { k_agg += 1; isk_agg += k.isk; } else { k_def += 1; isk_def += k.isk; }
            }
            let allies_dict = PyRep::Dict(
                ally_windows
                    .into_iter()
                    .map(|(aid, started, finished)| {
                        (
                            PyRep::Integer(aid),
                            PyRep::keyval([
                                ("allyID", PyRep::Integer(aid)),
                                ("timeStarted", PyRep::Integer(unix_to_filetime(started))),
                                ("timeFinished", PyRep::Integer(unix_to_filetime(finished))),
                            ]),
                        )
                    })
                    .collect(),
            );
            let war_kv = PyRep::keyval([
                ("warID", PyRep::Integer(war_id)),
                ("declaredByID", PyRep::Integer(agg_id)),
                ("againstID", PyRep::Integer(def_id)),
                ("mutual", PyRep::Bool(mutual)),
                ("openForAllies", PyRep::Bool(open_for_allies)),
                ("retracted", opt_unix_to_filetime_py(retracted_unix)),
                ("warHQ", war_hq.map(PyRep::Integer).unwrap_or(PyRep::None)),
                ("allies", allies_dict.clone()),
                ("killsForAggressor", PyRep::Integer(k_agg)),
                ("killsForDefender", PyRep::Integer(k_def)),
                ("iskDestroyedByAggressor", PyRep::Real(isk_agg)),
                ("iskDestroyedByDefender", PyRep::Real(isk_def)),
            ]);
            let ships_killed = PyRep::Dict(vec![
                (PyRep::Integer(agg_id), PyRep::Integer(k_agg)),
                (PyRep::Integer(def_id), PyRep::Integer(k_def)),
            ]);
            let isk_killed = PyRep::Dict(vec![
                (PyRep::Integer(agg_id), PyRep::Real(isk_agg)),
                (PyRep::Integer(def_id), PyRep::Real(isk_def)),
            ]);
            Some(Ok(PyRep::Tuple(vec![war_kv, ships_killed, isk_killed, allies_dict])))
        }

        ("warStatisticMgr", "GetKills") | ("warStatisticMgr", "GetKillsByGroup") => {
            handle_war_statistic_kills(ctx, session, method, bound_obj_id, my_corp, args)
        }

        ("warStatisticMgr", "GetKillMail") => {
            let Some(kill_id) = arg_i64(0) else {
                return err("InvalidArgument", "GetKillMail requires killID");
            };
            let requested_hash = args.get(1).and_then(|a| a.as_str());
            let km = match ctx.db.with_snapshot(|snap| {
                let Some(km) = snap.killmails.get(&kill_id).cloned() else {
                    return Err(BridgeRpcError {
                        code: "KillmailNotFound".into(),
                        message: format!("Killmail {kill_id} not found"),
                        client_error: None,
                    });
                };
                let kill_ts = if km.killed_unix > 0 { km.killed_unix } else { now };
                let expected_hash =
                    crate::services::combat_crimewatch::combat_wrecks_killmails::compute_killmail_esi_hash(
                        km.victim_character_id,
                        km.attacker_character_id,
                        km.ship_type_id,
                        kill_ts,
                    );
                crate::services::combat_crimewatch::combat_wrecks_killmails::authorize_killmail_read(
                    session,
                    char_id,
                    snap,
                    &expected_hash,
                    requested_hash,
                    kill_id,
                    km.victim_character_id,
                    km.victim_corp_id,
                    None,
                    &[(km.attacker_character_id, km.attacker_corp_id, None)],
                    km.killed_unix,
                )?;
                Ok(km)
            }) {
                Ok(km) => km,
                Err(e) => return Some(Err(e)),
            };
            Some(Ok(PyRep::keyval([
                ("killID", PyRep::Integer(km.kill_id)),
                ("victimCharacterID", PyRep::Integer(km.victim_character_id)),
                ("finalCharacterID", PyRep::Integer(km.attacker_character_id)),
                ("solarSystemID", PyRep::Integer(km.solar_system_id as i64)),
                ("victimShipTypeID", PyRep::Integer(km.ship_type_id as i64)),
                ("iskLost", PyRep::Real(km.total_value_isk)),
            ])))
        }

        _ => None,
    }
}

fn handle_get_negotiations(
    ctx: &ServerContext,
    bound_obj_id: Option<i64>,
    my_corp: i64,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    let explicit_wid = args.first().and_then(|a| a.as_i64()).or_else(|| {
        bound_obj_id.filter(|bid| ctx.db.with_snapshot(|snap| snap.wars.contains_key(bid)))
    });
    let now_unix = ctx.now_unix();
    let rows = ctx.db.with_snapshot(|snap| {
        snap.war_state
            .negotiations
            .iter()
            .filter(|n| {
                let wid_ok = explicit_wid.is_none_or(|wid| n.war_id == wid);
                let visible = is_war_participant_corp(snap, n.war_id, my_corp, now_unix)
                    || (n.kind == NegKind::Ally && n.from_corp == my_corp);
                wid_ok && visible
            })
            .map(|n| {
                vec![
                    PyRep::Integer(n.id),
                    PyRep::Integer(n.war_id),
                    PyRep::Integer(n.from_corp),
                    PyRep::Real(n.isk.to_wire_f64()),
                    PyRep::String(n.message.clone()),
                    PyRep::String(n.status.clone()),
                    opt_unix_to_filetime_py(n.effective_unix),
                ]
            })
            .collect()
    });
    Some(Ok(PyRep::rowset(NEG_COLUMNS.to_vec(), rows)))
}

fn handle_war_statistic_kills(
    ctx: &ServerContext,
    session: &SessionState,
    method: &str,
    bound_obj_id: Option<i64>,
    my_corp: i64,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    let war_id = resolve_war_stat_id(
        ctx,
        session,
        args.first().and_then(|a| a.as_i64()),
        bound_obj_id,
        my_corp,
    );
    let Some(WarKillsSummary { kills, .. }) = war_kills(ctx, war_id) else {
        return Some(Err(BridgeRpcError {
            code: "WarNotFound".into(),
            message: format!("War {war_id} not found"),
            client_error: None,
        }));
    };
    if method == "GetKillsByGroup" {
        let mut by_grp: BTreeMap<u32, (i64, f64)> = BTreeMap::new();
        for k in &kills {
            let e = by_grp.entry(k.group_id).or_default();
            e.0 += 1;
            e.1 += k.isk;
        }
        let rows = by_grp
            .into_iter()
            .map(|(g, (n, isk))| vec![PyRep::Integer(g as i64), PyRep::Integer(n), PyRep::Real(isk)])
            .collect();
        return Some(Ok(PyRep::rowset(
            vec!["groupID", "kills", "iskDestroyed"],
            rows,
        )));
    }
    let rows = kills
        .into_iter()
        .map(|k| {
            vec![
                PyRep::Integer(k.kill_id),
                PyRep::Integer(war_id),
                PyRep::Integer(k.attacker_corp),
                PyRep::Integer(k.victim_corp),
                PyRep::Integer(k.group_id as i64),
                PyRep::Real(k.isk),
                PyRep::Integer(k.attacker_char),
                PyRep::Integer(k.attacker_corp),
                k.attacker_alliance.map(PyRep::Integer).unwrap_or(PyRep::None),
                PyRep::Integer(k.attacker_ship_type_id as i64),
                PyRep::Integer(k.victim_char),
                PyRep::Integer(k.victim_corp),
                k.victim_alliance.map(PyRep::Integer).unwrap_or(PyRep::None),
                PyRep::Integer(k.victim_ship_type_id as i64),
                PyRep::Integer(unix_to_filetime(k.kill_time_unix)),
            ]
        })
        .collect();
    Some(Ok(PyRep::rowset(
        vec![
            "killID",
            "warID",
            "attackerCorpID",
            "victimCorpID",
            "groupID",
            "iskLost",
            "finalCharacterID",
            "finalCorporationID",
            "finalAllianceID",
            "finalShipTypeID",
            "victimCharacterID",
            "victimCorporationID",
            "victimAllianceID",
            "victimShipTypeID",
            "killTime",
        ],
        rows,
    )))
}

#[cfg(test)]
#[path = "social_wars_tests.rs"]
mod social_wars_tests;

#[cfg(test)]
#[path = "social_wars_edges2_tests.rs"]
mod social_wars_edges2_tests;

