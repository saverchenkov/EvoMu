//! Wars (`feat:war.*`): declaration with War HQ + fee, war lists, surrender negotiations and
//! 2-week peace treaties, mutual wars, defender allies and per-war statistics (`SO-3`).
//!
//! Persistent war rows live in `snap.wars`; negotiation / treaty / HQ bookkeeping lives in
//! `WAR_STATE`. War end conditions (finish timers, War HQ lost, defender no longer
//! war-eligible) are applied lazily by `sync_wars` at the start of every war RPC.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use eve_db::{
    IskCents, PersistedNegKind as NegKind, PersistedPeaceTreaty as PeaceTreaty,
    PersistedWarMeta as WarMeta, PersistedWarNegotiation as Negotiation, WarRecord,
    WorldStateSnapshot,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::BTreeMap;

#[path = "social_wars_helpers.rs"]
mod social_wars_helpers;
use social_wars_helpers::{
    already_at_war, credit_corp, debit_corp, emit_mutual_wars_updated, emit_war_changed, err,
    is_valid_war_hq, is_war_participant_corp, neg_rows_for_caller, resolve_war_stat_id,
    same_alliance, sync_wars, war_crowset, war_eligible, war_row, NEG_COLUMNS, PEACE_TREATY_SECS,
    WAR_COLUMNS, WAR_DECLARATION_FEE_ISK, WAR_TIMER_SECS,
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

/// Per-war kills DERIVED from persisted killmails: a killmail counts towards a war when the
/// final-blow attacker's corporation and the victim's corporation are on opposite sides of the war
/// (including accepted defender allies) and the kill timestamp falls within the war window.
fn war_kills_in_snap(ctx: &ServerContext, snap: &WorldStateSnapshot, war_id: i64) -> Option<(i64, i64, Vec<i64>, Vec<WarKill>)> {
    let w = snap.wars.get(&war_id)?;
    let (agg, def) = (w.aggressor_corp_id, w.defender_corp_id);
    let meta = snap.war_state.wars.get(&war_id).cloned().unwrap_or_default();
    let allies: Vec<i64> = snap
        .war_state
        .negotiations
        .iter()
        .filter(|n| n.war_id == war_id && n.kind == NegKind::Ally && n.status == "Accepted")
        .map(|n| n.from_corp)
        .collect();
    let is_def_side = |cid: i64| cid == def || allies.contains(&cid);
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
            ((a == agg && is_def_side(v)) || (is_def_side(a) && v == agg)).then(|| {
                let att_corp = if a == agg { agg } else { def };
                let vic_corp = if v == agg { agg } else { def };
                let att_all = snap.corporations.get(&att_corp).and_then(|c| c.alliance_id);
                let vic_all = snap.corporations.get(&vic_corp).and_then(|c| c.alliance_id);
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
    Some((agg, def, allies, kills))
}

fn war_kills(ctx: &ServerContext, war_id: i64) -> Option<(i64, i64, Vec<i64>, Vec<WarKill>)> {
    ctx.db.with_snapshot(|snap| war_kills_in_snap(ctx, snap, war_id))
}

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
    let my_corp = ctx.db.with_snapshot(|snap| snap.characters.get(&char_id).map(|c| c.corporation_id)).or(session.corporation_id).unwrap_or(0);
    let now = ctx.now_unix();
    if my_corp > 0 {
        crate::services::fleet_social::fleet_social_corps::promote_due_pending_corp_grants(ctx, my_corp);
    }
    sync_wars(ctx);
    let arg_i64 = |i: usize| args.get(i).and_then(|a| a.as_i64());
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
                    return Err(BridgeRpcError { code: "NpcCorporationCannotDeclareWar".into(), message: "Only player corporations can declare war".into() });
                };
                if agg.ceo_id != char_id {
                    return Err(BridgeRpcError { code: "CeoRoleRequired".into(), message: "Only the corporation CEO can declare war".into() });
                }
                if defender == my_corp || !snap.corporations.contains_key(&defender) || same_alliance(snap, my_corp, defender) {
                    return Err(BridgeRpcError { code: "InvalidWarTarget".into(), message: format!("Corporation {defender} is not a valid war target") });
                }
                if already_at_war(snap, my_corp, defender) {
                    return Err(BridgeRpcError { code: "AlreadyAtWar".into(), message: "These corporations are already at war".into() });
                }
                let owned_ok = |s: &&eve_db::UpwellStructureRecord| is_valid_war_hq(ctx, my_corp, s);
                let hq = match hq_arg {
                    Some(id) => snap.structures.get(&id).filter(owned_ok).map(|s| s.structure_id),
                    // No explicit War HQ: use the attacker's lowest-ID eligible structure.
                    None => snap.structures.values().filter(owned_ok).map(|s| s.structure_id).min(),
                };
                let Some(hq) = hq else {
                    return Err(BridgeRpcError { code: "WarHqRequired".into(), message: "Declaring war requires a War HQ structure owned by the attacking corporation".into() });
                };
                if !war_eligible(snap, defender) {
                    return Err(BridgeRpcError { code: "DefenderNotWarEligible".into(), message: format!("Corporation {defender} owns no structures and is not war-eligible") });
                }
                debit_corp(snap, my_corp, IskCents::from_isk(WAR_DECLARATION_FEE_ISK))?;
                snap.next_entity_id += 1;
                let war_id = snap.next_entity_id;
                snap.wars.insert(war_id, WarRecord { war_id, aggressor_corp_id: my_corp, defender_corp_id: defender, mutual: false, started_unix: now, active: true });
                snap.war_state.wars.insert(
                    war_id,
                    WarMeta {
                        war_hq_id: Some(hq),
                        ..Default::default()
                    },
                );
                snap.war_lifecycle_meta.insert(
                    war_id,
                    eve_db::snapshot_ext::WarLifecycleMeta {
                        declared_unix: Some(now),
                        started_unix: Some(now + WAR_TIMER_SECS),
                        open_for_allies: Some(false),
                        ..Default::default()
                    },
                );
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
                ("timeDeclared", PyRep::Integer(now as i64)),
                ("timeStarted", PyRep::Integer((now + WAR_TIMER_SECS) as i64)),
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
                            .map(|(_, _, _, ks)| (ks.len(), ks.iter().map(|k| k.isk).sum::<f64>().to_bits()))
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
            Some(Ok(PyRep::Integer(w.started_unix as i64)))
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
            Some(Ok(PyRep::Integer((w.started_unix + WAR_TIMER_SECS) as i64)))
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
            Some(Ok(meta.finish_at_unix.map(|f| PyRep::Integer(f as i64)).unwrap_or(PyRep::None)))
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
            Some(Ok(PyRep::keyval(WAR_COLUMNS.iter().copied().zip(row))))
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
            let (Some(w), open_for_allies, war_already_ending, is_player_corp) =
                ctx.db.with_snapshot(|snap| {
                    (
                        snap.wars.get(&war_id).cloned(),
                        snap.war_state.wars.get(&war_id).is_some_and(|m| m.open_for_allies),
                        snap.war_state.wars.get(&war_id).is_some_and(|m| m.finish_at_unix.is_some()),
                        snap.corporations.contains_key(&my_corp),
                    )
                })
            else {
                return err("WarNotFound", format!("War {war_id} not found"));
            };
            if !w.active {
                return err("WarNotActive", format!("War {war_id} has ended"));
            }
            let party = my_corp == w.aggressor_corp_id || my_corp == w.defender_corp_id;
            let kind = if method == "CreateSurrenderNegotiation" { NegKind::Surrender } else { NegKind::Ally };
            match kind {
                NegKind::Surrender if !party => return err("NotWarParticipant", "Only war participants can negotiate surrender"),
                NegKind::Surrender if war_already_ending => {
                    return err("WarAlreadyEnding", format!("War {war_id} surrender has already been accepted"));
                }
                NegKind::Ally if party => return err("CannotAllyOwnWar", "War participants cannot offer themselves as allies"),
                NegKind::Ally if !open_for_allies => {
                    return err("WarNotOpenForAllies", "The defender has not opened this war for allies")
                }
                NegKind::Ally if !is_player_corp => {
                    return err("NpcCorporationCannotAlly", "Only player corporations can ally in wars")
                }
                _ => {}
            }
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let st = &mut snap.war_state;
                if kind == NegKind::Ally
                    && st.negotiations.iter().any(|neg| {
                        neg.war_id == war_id
                            && neg.kind == NegKind::Ally
                            && neg.from_corp == my_corp
                            && matches!(neg.status.as_str(), "Offered" | "Accepted")
                    })
                {
                    return Err(BridgeRpcError {
                        code: "AllyOfferAlreadyExists".into(),
                        message: "Your corporation already has an active or pending ally offer for this war".into(),
                    });
                }
                if kind == NegKind::Surrender {
                    for neg in &mut st.negotiations {
                        if neg.war_id == war_id
                            && neg.kind == NegKind::Surrender
                            && neg.status == "Offered"
                        {
                            neg.status = "Cancelled".into();
                        }
                    }
                }
                st.next_negotiation_id += 1;
                let id = st.next_negotiation_id;
                st.negotiations.push(Negotiation {
                    id,
                    war_id,
                    kind,
                    from_corp: my_corp,
                    isk: IskCents::from_f64_isk(isk),
                    message,
                    status: "Offered".into(),
                    effective_unix: None,
                });
                Ok::<_, BridgeRpcError>(id)
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
                let allowed = is_war_participant_corp(snap, wid, my_corp)
                    || neg_opt.is_some_and(|n| n.from_corp == my_corp);
                if !allowed {
                    return Err(BridgeRpcError {
                        code: "NotWarParticipant".into(),
                        message: "Only war participants or the negotiation offeror may inspect this negotiation".into(),
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
        (
            "warRegistry",
            "GMActivateDefender"
            | "GMDeactivateDefender"
            | "GMExtendAllyContract"
            | "GMJoinDefender"
            | "GMSetDeclareTime"
            | "GMSetWarFinishTime",
        ) => Some(Ok(PyRep::Bool(true))),

        ("warsInfoMgr" | "warRegistry", "AcceptSurrender" | "DeclineSurrender") => {
            if !has_war_director_role {
                return err("CorpRoleDenied", "Acting on war negotiations requires CEO or Director role");
            }
            let neg_id = arg_i64(0).unwrap_or(0);
            let Some((idx, n, w_opt)) = ctx.db.with_snapshot(|snap| {
                let idx = snap
                    .war_state
                    .negotiations
                    .iter()
                    .position(|n| n.id == neg_id && n.status == "Offered")?;
                let n = snap.war_state.negotiations[idx].clone();
                let w = snap.wars.get(&n.war_id).cloned();
                Some((idx, n, w))
            }) else {
                return err("NegotiationNotFound", format!("No open negotiation {neg_id}"));
            };
            let Some(w) = w_opt else {
                return err("WarNotFound", format!("War {} not found", n.war_id));
            };
            if n.kind != NegKind::Surrender {
                return err("NegotiationNotFound", format!("Negotiation {neg_id} is not of the requested kind"));
            }
            let allowed = my_corp != n.from_corp && (my_corp == w.aggressor_corp_id || my_corp == w.defender_corp_id);
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
            let effective = now + WAR_TIMER_SECS;
            let status = if accepting { "Accepted" } else { "Declined" };
            let tx_res = ctx.db.try_with_mut_snapshot(|snap| {
                let current_status = snap
                    .war_state
                    .negotiations
                    .get(idx)
                    .map(|neg| neg.status.as_str())
                    .unwrap_or("");
                if current_status != "Offered" {
                    return Err(BridgeRpcError {
                        code: "NegotiationAlreadyResolved".into(),
                        message: format!("Negotiation {neg_id} is already {current_status}"),
                    });
                }
                if accepting {
                    let already_ending = snap
                        .war_state
                        .wars
                        .get(&n.war_id)
                        .and_then(|wm| wm.finish_at_unix)
                        .is_some();
                    if already_ending {
                        return Err(BridgeRpcError {
                            code: "WarAlreadyEnding".into(),
                            message: format!("War {} surrender has already been accepted", n.war_id),
                        });
                    }
                    debit_corp(snap, n.from_corp, n.isk)?;
                    credit_corp(snap, my_corp, n.isk);
                }
                let st = &mut snap.war_state;
                st.negotiations[idx].status = status.into();
                if accepting {
                    st.negotiations[idx].effective_unix = Some(effective);
                    st.wars.entry(n.war_id).or_default().finish_at_unix = Some(effective);
                    for (i, other) in st.negotiations.iter_mut().enumerate() {
                        if i != idx
                            && other.war_id == n.war_id
                            && other.kind == NegKind::Surrender
                            && other.status == "Offered"
                        {
                            other.status = "Cancelled".into();
                        }
                    }
                    let expiry = effective + PEACE_TREATY_SECS;
                    st.peace_treaties.push(PeaceTreaty {
                        war_id: n.war_id,
                        corp_a: w.aggressor_corp_id,
                        corp_b: w.defender_corp_id,
                        expiry_unix: expiry,
                    });
                    snap.war_lifecycle_meta.entry(n.war_id).or_default().finished_unix =
                        Some(effective);
                }
                Ok::<_, BridgeRpcError>(())
            });
            if let Err(e) = tx_res {
                return Some(Err(e));
            }
            let mut out = vec![
                ("negotiationID", PyRep::Integer(neg_id)),
                ("warID", PyRep::Integer(n.war_id)),
                ("status", PyRep::String(status.into())),
            ];
            if accepting {
                let expiry = effective + PEACE_TREATY_SECS;
                out.push(("timeFinished", PyRep::Integer(effective as i64)));
                out.push(("peaceTreatyExpiry", PyRep::Integer(expiry as i64)));
            }
            emit_war_changed(
                ctx,
                n.war_id,
                vec![("timeFinished", PyRep::None, PyRep::Integer(effective as i64))],
            );
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
            let Some((idx, n, w_opt)) = ctx.db.with_snapshot(|snap| {
                let idx = snap
                    .war_state
                    .negotiations
                    .iter()
                    .position(|n| n.id == neg_id && n.status == "Offered")?;
                let n = snap.war_state.negotiations[idx].clone();
                let w = snap.wars.get(&n.war_id).cloned();
                Some((idx, n, w))
            }) else {
                return err("NegotiationNotFound", format!("No open negotiation {neg_id}"));
            };
            let Some(w) = w_opt else {
                return err("WarNotFound", format!("War {} not found", n.war_id));
            };
            if n.kind != NegKind::Ally {
                return err("NegotiationNotFound", format!("Negotiation {neg_id} is not of the requested kind"));
            }
            let allowed = match method {
                "RetractWarAllyOffer" => my_corp == n.from_corp,
                _ => my_corp == w.defender_corp_id,
            };
            if !allowed {
                return err("NotNegotiationCounterparty", "Your corporation cannot act on this negotiation");
            }
            let accepting = method == "AcceptAllyNegotiation";
            if accepting && !w.active {
                return err("WarNotActive", format!("War {} has ended", n.war_id));
            }
            let effective = now + WAR_TIMER_SECS;
            let status = match method {
                "AcceptAllyNegotiation" => "Accepted",
                "RetractWarAllyOffer" => "Retracted",
                _ => "Declined",
            };
            let tx_res = ctx.db.try_with_mut_snapshot(|snap| {
                let cur_status = snap
                    .war_state
                    .negotiations
                    .get(idx)
                    .map(|neg| neg.status.as_str())
                    .unwrap_or("");
                if cur_status != "Offered" {
                    return Err(BridgeRpcError {
                        code: "NegotiationAlreadyResolved".into(),
                        message: format!("Negotiation {neg_id} is already {cur_status}"),
                    });
                }
                if accepting {
                    let already_allied = snap.war_state.negotiations.iter().enumerate().any(|(i, other)| {
                        i != idx
                            && other.war_id == n.war_id
                            && other.kind == NegKind::Ally
                            && other.from_corp == n.from_corp
                            && other.status == "Accepted"
                    });
                    if already_allied {
                        return Err(BridgeRpcError {
                            code: "AlreadyWarAlly".into(),
                            message: format!("Corporation {} is already an ally in war {}", n.from_corp, n.war_id),
                        });
                    }
                    debit_corp(snap, my_corp, n.isk)?;
                    credit_corp(snap, n.from_corp, n.isk);
                }
                let st = &mut snap.war_state;
                st.negotiations[idx].status = status.into();
                if accepting {
                    st.negotiations[idx].effective_unix = Some(effective);
                    for (i, other) in st.negotiations.iter_mut().enumerate() {
                        if i != idx
                            && other.war_id == n.war_id
                            && other.kind == NegKind::Ally
                            && other.from_corp == n.from_corp
                            && other.status == "Offered"
                        {
                            other.status = "Cancelled".into();
                        }
                    }
                    snap.war_lifecycle_meta
                        .entry(n.war_id)
                        .or_default()
                        .allies
                        .push(eve_db::snapshot_ext::WarAllyRecord {
                            corporation_id: Some(n.from_corp),
                            alliance_id: None,
                        });
                }
                Ok::<_, BridgeRpcError>(())
            });
            if let Err(e) = tx_res {
                return Some(Err(e));
            }
            let mut out = vec![
                ("negotiationID", PyRep::Integer(neg_id)),
                ("warID", PyRep::Integer(n.war_id)),
                ("status", PyRep::String(status.into())),
            ];
            if accepting {
                out.push(("allyID", PyRep::Integer(n.from_corp)));
                out.push(("timeStarted", PyRep::Integer(effective as i64)));
                emit_war_changed(
                    ctx,
                    n.war_id,
                    vec![("allyID", PyRep::None, PyRep::Integer(n.from_corp))],
                );
            }
            Some(Ok(PyRep::keyval(out)))
        }

        ("warsInfoMgr" | "warRegistry", "RetractMutualWar") => {
            if !has_war_director_role {
                return err("CorpRoleDenied", "Retracting mutual war requires CEO or Director role");
            }
            let war_id = arg_i64(0).or(bound_obj_id).unwrap_or(0);
            let Some(w) = ctx.db.with_snapshot(|snap| snap.wars.get(&war_id).cloned()) else {
                return err("WarNotFound", format!("War {war_id} not found"));
            };
            if !w.mutual || !w.active {
                return err("NotActiveMutualWar", format!("War {war_id} is not an active mutual war"));
            }
            if my_corp != w.aggressor_corp_id && my_corp != w.defender_corp_id {
                return err("NotWarParticipant", "Only war participants can retract a mutual war");
            }
            let finish = now + WAR_TIMER_SECS;
            ctx.db.with_mut_snapshot(|snap| {
                snap.war_state.wars.entry(war_id).or_default().finish_at_unix = Some(finish);
                let lm = snap.war_lifecycle_meta.entry(war_id).or_default();
                lm.retracted_unix = Some(now);
                lm.finished_unix = Some(finish);
            });
            emit_war_changed(
                ctx,
                war_id,
                vec![("timeFinished", PyRep::None, PyRep::Integer(finish as i64))],
            );
            Some(Ok(PyRep::keyval([("warID", PyRep::Integer(war_id)), ("timeFinished", PyRep::Integer(finish as i64))])))
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
            let prev = ctx.db.with_mut_snapshot(|snap| {
                snap.war_lifecycle_meta.entry(war_id).or_default().open_for_allies = Some(open);
                let m = snap.war_state.wars.entry(war_id).or_default();
                std::mem::replace(&mut m.open_for_allies, open)
            });
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
                            PyRep::Integer(start as i64),
                            PyRep::Real(n.isk.to_wire_f64()),
                            PyRep::Bool(now >= start),
                        ]
                    })
                    .collect()
            });
            Some(Ok(PyRep::rowset(vec!["allyID", "timeStarted", "iskValue", "active"], rows)))
        }

        ("peaceTreatyMgr", "GetPeaceTreatiesForSession") => {
            let rows: Vec<Vec<PyRep>> = ctx.db.with_snapshot(|snap| {
                snap.war_state
                    .peace_treaties
                    .iter()
                    .filter(|t| t.expiry_unix > now && (t.corp_a == my_corp || t.corp_b == my_corp))
                    .map(|t| {
                        let other = if t.corp_a == my_corp { t.corp_b } else { t.corp_a };
                        vec![
                            PyRep::Integer(t.war_id),
                            PyRep::Integer(other),
                            PyRep::Integer(t.expiry_unix as i64),
                        ]
                    })
                    .collect()
            });
            Some(Ok(PyRep::rowset(vec!["warID", "otherOwnerID", "expiryDate"], rows)))
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
            Some(Ok(PyRep::keyval([("fromID", PyRep::Integer(my_corp)), ("toID", PyRep::Integer(to_corp)), ("sentDate", PyRep::Integer(now as i64))])))
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
                            PyRep::Integer(ts as i64),
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
                if !snap.war_state.mutual_invites.contains_key(&(from_corp, my_corp)) {
                    return Err(BridgeRpcError {
                        code: "MutualWarInviteNotFound".into(),
                        message: format!("No pending mutual war invite from {from_corp}"),
                    });
                }
                if same_alliance(snap, from_corp, my_corp) {
                    snap.war_state.mutual_invites.remove(&(from_corp, my_corp));
                    return Err(BridgeRpcError {
                        code: "InvalidWarTarget".into(),
                        message: "Corporations in the same alliance cannot enter a mutual war".into(),
                    });
                }
                if already_at_war(snap, from_corp, my_corp) {
                    snap.war_state.mutual_invites.remove(&(from_corp, my_corp));
                    return Err(BridgeRpcError {
                        code: "AlreadyAtWar".into(),
                        message: "These corporations are already at war".into(),
                    });
                }
                snap.war_state.mutual_invites.remove(&(from_corp, my_corp));
                snap.war_state.mutual_invites.remove(&(my_corp, from_corp));
                snap.next_entity_id += 1;
                let wid = snap.next_entity_id;
                snap.wars.insert(wid, WarRecord { war_id: wid, aggressor_corp_id: from_corp, defender_corp_id: my_corp, mutual: true, started_unix: now, active: true });
                snap.war_state.wars.insert(wid, WarMeta::default());
                Ok::<_, BridgeRpcError>(wid)
            });
            let war_id = match tx_res {
                Ok(wid) => wid,
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

        ("warStatisticMgr", "Bind") => {
            let object_id = arg_i64(0).unwrap_or(0);
            match ctx.bind_object(session, "warStatisticMgr", object_id) {
                Ok(bound_id) => Some(Ok(PyRep::Integer(bound_id as i64))),
                Err(e) => Some(Err(e)),
            }
        }

        ("warStatisticMgr", "UnBind") => handle_war_stat_unbind(ctx, session, arg_i64(0)),
        ("warStatisticMgr", "Unbind") => handle_war_stat_unbind(ctx, session, arg_i64(0)),

        ("warStatisticMgr", "SetSessionCheck") => {
            handle_war_stat_set_session_check(ctx, session, args)
        }

        ("warStatisticMgr", "GetBaseInfo") => {
            let war_id = resolve_war_stat_id(ctx, session, arg_i64(0), bound_obj_id, my_corp);
            let Some((agg_id, def_id, allies, kills)) = war_kills(ctx, war_id) else {
                return err("WarNotFound", format!("War {war_id} not found"));
            };
            let (mutual, open_for_allies, war_hq) = ctx.db.with_snapshot(|snap| {
                let mutual = snap.wars.get(&war_id).is_some_and(|w| w.mutual);
                let meta = snap.war_state.wars.get(&war_id).cloned().unwrap_or_default();
                (mutual, meta.open_for_allies, meta.war_hq_id)
            });
            let (mut k_agg, mut k_def, mut isk_agg, mut isk_def) = (0i64, 0i64, 0.0f64, 0.0f64);
            for k in &kills {
                if k.attacker_corp == agg_id { k_agg += 1; isk_agg += k.isk; } else { k_def += 1; isk_def += k.isk; }
            }
            let allies_dict = PyRep::Dict(
                allies
                    .into_iter()
                    .map(|aid| {
                        (
                            PyRep::Integer(aid),
                            PyRep::keyval([
                                ("allyID", PyRep::Integer(aid)),
                                ("timeStarted", PyRep::Integer(now as i64)),
                                ("timeFinished", PyRep::Integer((now + WAR_TIMER_SECS) as i64)),
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
                ("retracted", PyRep::None),
                ("warHQ", war_hq.map(PyRep::Integer).unwrap_or(PyRep::None)),
                ("allies", allies_dict.clone()),
                ("killsForAggressor", PyRep::Integer(k_agg)),
                ("killsForDefender", PyRep::Integer(k_def)),
                ("iskDestroyedByAggressor", PyRep::Real(isk_agg)),
                ("iskDestroyedByDefender", PyRep::Real(isk_def)),
            ]);
            Some(Ok(war_kv))
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
    let rows = ctx.db.with_snapshot(|snap| {
        snap.war_state
            .negotiations
            .iter()
            .filter(|n| {
                let wid_ok = explicit_wid.is_none_or(|wid| n.war_id == wid);
                let visible = is_war_participant_corp(snap, n.war_id, my_corp)
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
                    n.effective_unix
                        .map(|e| PyRep::Integer(e as i64))
                        .unwrap_or(PyRep::None),
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
    let Some((_, _, _, kills)) = war_kills(ctx, war_id) else {
        return Some(Err(BridgeRpcError {
            code: "WarNotFound".into(),
            message: format!("War {war_id} not found"),
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
                PyRep::Integer(k.kill_time_unix as i64),
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

