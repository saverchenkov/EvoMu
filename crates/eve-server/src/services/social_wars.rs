//! Wars (`feat:war.*`): declaration with War HQ + fee, war lists, surrender negotiations and
//! 2-week peace treaties, mutual wars, defender allies and per-war statistics (`SO-3`).
//!
//! Persistent war rows live in `snap.wars`; negotiation / treaty / HQ bookkeeping lives in
//! `WAR_STATE`. War end conditions (finish timers, War HQ lost, defender no longer
//! war-eligible) are applied lazily by `sync_wars` at the start of every war RPC.

use crate::router::{cache_notifications::NotificationTarget, ServerContext};
use eve_db::{
    IskCents, PersistedNegKind as NegKind, PersistedPeaceTreaty as PeaceTreaty,
    PersistedWarMeta as WarMeta, PersistedWarNegotiation as Negotiation, WarRecord,
    WorldStateSnapshot,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::BTreeMap;

/// CONCORD war declaration bill (ISK).
const WAR_DECLARATION_FEE_ISK: i64 = 100_000_000;
/// Spool-up before a declared / accepted war (or ally) becomes active, and wind-down after surrender.
const WAR_TIMER_SECS: u64 = 86_400;
/// Peace treaty duration after an accepted surrender.
const PEACE_TREATY_SECS: u64 = 14 * 86_400;

fn err(code: &str, message: impl Into<String>) -> Option<Result<PyRep, BridgeRpcError>> {
    Some(Err(BridgeRpcError { code: code.into(), message: message.into() }))
}

/// A corporation is war-eligible while it owns at least one structure that is not destroyed.
fn war_eligible(snap: &WorldStateSnapshot, corp_id: i64) -> bool {
    snap.structures.values().any(|s| s.owner_corp_id == corp_id && s.state != "Destroyed")
}

fn debit_corp(snap: &mut WorldStateSnapshot, corp_id: i64, amount: IskCents) -> Result<(), BridgeRpcError> {
    let corp = snap.corporations.get_mut(&corp_id).ok_or_else(|| BridgeRpcError {
        code: "CorporationNotFound".into(),
        message: format!("Corporation {corp_id} not found"),
    })?;
    corp.ensure_wallet_divisions();
    corp.wallet_divisions[0].balance = corp.wallet_divisions[0].balance.checked_sub(amount).ok_or_else(|| BridgeRpcError {
        code: "InsufficientFunds".into(),
        message: format!("Corporation {corp_id} master wallet cannot cover {} ISK", amount.to_wire_f64()),
    })?;
    corp.master_wallet = corp.wallet_divisions[0].balance;
    Ok(())
}

fn credit_corp(snap: &mut WorldStateSnapshot, corp_id: i64, amount: IskCents) {
    if let Some(corp) = snap.corporations.get_mut(&corp_id) {
        corp.ensure_wallet_divisions();
        corp.wallet_divisions[0].balance = corp.wallet_divisions[0].balance.checked_add(amount).unwrap_or(corp.wallet_divisions[0].balance);
        corp.master_wallet = corp.wallet_divisions[0].balance;
    }
}

/// Ends wars whose finish timer elapsed, whose War HQ is gone/destroyed, or whose defender lost its
/// last structure (non-mutual wars only).
fn sync_wars(ctx: &ServerContext) {
    let now = ctx.now_unix();
    let needs_end = ctx.db.with_snapshot(|snap| {
        snap.wars.values().filter(|w| w.active).any(|w| {
            let meta = snap.war_state.wars.get(&w.war_id).cloned().unwrap_or_default();
            let finished = meta.finish_at_unix.is_some_and(|f| now >= f);
            let hq_lost = meta
                .war_hq_id
                .is_some_and(|hq| snap.structures.get(&hq).is_none_or(|s| s.state == "Destroyed"));
            let defender_ineligible = !w.mutual && !war_eligible(snap, w.defender_corp_id);
            finished || hq_lost || defender_ineligible
        })
    });
    if !needs_end {
        return;
    }
    let ended: Vec<i64> = ctx.db.with_mut_snapshot(|snap| {
        let mut ended = Vec::new();
        let ids: Vec<i64> = snap.wars.values().filter(|w| w.active).map(|w| w.war_id).collect();
        for wid in ids {
            let w = snap.wars[&wid].clone();
            let meta = snap.war_state.wars.get(&wid).cloned().unwrap_or_default();
            let finished = meta.finish_at_unix.is_some_and(|f| now >= f);
            let hq_lost = meta
                .war_hq_id
                .is_some_and(|hq| snap.structures.get(&hq).is_none_or(|s| s.state == "Destroyed"));
            let defender_ineligible = !w.mutual && !war_eligible(snap, w.defender_corp_id);
            if finished || hq_lost || defender_ineligible {
                if let Some(wm) = snap.wars.get_mut(&wid) {
                    wm.active = false;
                }
                snap.war_state
                    .wars
                    .entry(wid)
                    .or_default()
                    .finish_at_unix
                    .get_or_insert(now);
                ended.push(wid);
            }
        }
        ended
    });
    for wid in ended {
        emit_war_changed(
            ctx,
            wid,
            vec![("active", PyRep::Bool(true), PyRep::Bool(false))],
        );
    }
}

fn emit_mutual_wars_updated(ctx: &ServerContext, actor_char_id: i64, corp_a: i64, corp_b: i64) {
    let mut sent = ctx.notify(NotificationTarget::Corporation(corp_a), "OnMutualWarsUpdated_Remote", "corpid", vec![]);
    if corp_b != corp_a {
        sent.extend(ctx.notify(NotificationTarget::Corporation(corp_b), "OnMutualWarsUpdated_Remote", "corpid", vec![]));
    }
    if !sent.iter().any(|n| n.recipient_char_id == actor_char_id) {
        ctx.notify(NotificationTarget::Character(actor_char_id), "OnMutualWarsUpdated_Remote", "charid", vec![]);
    }
}

fn emit_war_changed(ctx: &ServerContext, war_id: i64, change_entries: Vec<(&str, PyRep, PyRep)>) {
    let now = ctx.now_unix();
    let Some((w, meta)) = ctx.db.with_snapshot(|snap| {
        let w = snap.wars.get(&war_id).cloned()?;
        let meta = snap.war_state.wars.get(&war_id).cloned().unwrap_or_default();
        Some((w, meta))
    }) else {
        return;
    };
    let war_kv = PyRep::keyval(WAR_COLUMNS.iter().copied().zip(war_row(&w, &meta, now)));
    let owner_ids = PyRep::List(vec![
        PyRep::Integer(w.aggressor_corp_id),
        PyRep::Integer(w.defender_corp_id),
    ]);
    let change = PyRep::Dict(
        change_entries
            .into_iter()
            .map(|(k, old_v, new_v)| (PyRep::String((k).into()), PyRep::Tuple(vec![old_v, new_v])))
            .collect(),
    );
    let args = vec![war_kv, owner_ids, change];
    ctx.notify(
        NotificationTarget::Corporation(w.aggressor_corp_id),
        "OnWarChanged",
        "corpid",
        args.clone(),
    );
    if w.defender_corp_id != w.aggressor_corp_id {
        ctx.notify(
            NotificationTarget::Corporation(w.defender_corp_id),
            "OnWarChanged",
            "corpid",
            args,
        );
    }
}

fn war_row(w: &WarRecord, meta: &WarMeta, now: u64) -> Vec<PyRep> {
    let started = w.started_unix + WAR_TIMER_SECS;
    vec![
        PyRep::Integer(w.war_id),
        PyRep::Integer(w.aggressor_corp_id),
        PyRep::Integer(w.defender_corp_id),
        PyRep::Integer(w.started_unix as i64),
        PyRep::Integer(started as i64),
        meta.finish_at_unix.map(|f| PyRep::Integer(f as i64)).unwrap_or(PyRep::None),
        PyRep::Bool(w.mutual),
        PyRep::Bool(meta.open_for_allies),
        meta.war_hq_id.map(PyRep::Integer).unwrap_or(PyRep::None),
        PyRep::Bool(w.active),
        PyRep::Bool(w.active && now >= started),
    ]
}

const WAR_COLUMNS: [&str; 11] = [
    "warID", "declaredByID", "againstID", "timeDeclared", "timeStarted", "timeFinished",
    "mutual", "openForAllies", "warHQ", "active", "combatActive",
];

struct WarKill {
    kill_id: i64,
    attacker_corp: i64,
    victim_corp: i64,
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
            ((a == agg && is_def_side(v)) || (is_def_side(a) && v == agg)).then(|| WarKill {
                kill_id: km.kill_id,
                attacker_corp: if a == agg { agg } else { def },
                victim_corp: if v == agg { agg } else { def },
                group_id: ctx.sde.types.get(&km.ship_type_id).map(|t| t.group_id).unwrap_or(0),
                isk: km.total_value_isk,
            })
        })
        .collect();
    kills.sort_by_key(|k| k.kill_id);
    Some((agg, def, allies, kills))
}

fn war_kills(ctx: &ServerContext, war_id: i64) -> Option<(i64, i64, Vec<i64>, Vec<WarKill>)> {
    ctx.db.with_snapshot(|snap| war_kills_in_snap(ctx, snap, war_id))
}

fn resolve_war_stat_id(ctx: &ServerContext, session: &SessionState, arg0: Option<i64>, bound_obj_id: Option<i64>, my_corp: i64) -> i64 {
    arg0.filter(|&id| id > 0)
        .or_else(|| bound_obj_id.filter(|&id| id > 0))
        .or_else(|| {
            ctx.bound_objects
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .values()
                .filter(|b| b.session_id == session.session_id && b.service == "warStatisticMgr" && b.object_id > 0)
                .max_by_key(|b| b.bound_id)
                .map(|b| b.object_id)
        })
        .or_else(|| {
            ctx.db.with_snapshot(|snap| {
                snap.wars.values().find(|w| w.aggressor_corp_id == my_corp || w.defender_corp_id == my_corp).map(|w| w.war_id)
            })
        })
        .unwrap_or(0)
}

fn neg_rows(ctx: &ServerContext, war_id: i64, kind: NegKind) -> Vec<Vec<PyRep>> {
    ctx.db.with_snapshot(|snap| {
        snap.war_state
            .negotiations
            .iter()
            .filter(|n| n.war_id == war_id && n.kind == kind)
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
    })
}

const NEG_COLUMNS: [&str; 7] = ["negotiationID", "warID", "ownerID", "iskValue", "message", "status", "timeEffective"];

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
    sync_wars(ctx);
    let arg_i64 = |i: usize| args.get(i).and_then(|a| a.as_i64());
    let has_war_director_role = ctx.db.with_snapshot(|snap| {
        snap.corporations.get(&my_corp).is_none_or(|c| {
            c.ceo_id == char_id
                || eve_db::corp_roles::has_role(
                    c.member_roles.get(&char_id).copied().unwrap_or(0),
                    eve_db::corp_roles::ROLE_DIRECTOR,
                )
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
                if defender == my_corp || !snap.corporations.contains_key(&defender) {
                    return Err(BridgeRpcError { code: "InvalidWarTarget".into(), message: format!("Corporation {defender} is not a valid war target") });
                }
                if snap.wars.values().any(|w| w.active && ((w.aggressor_corp_id == my_corp && w.defender_corp_id == defender) || (w.aggressor_corp_id == defender && w.defender_corp_id == my_corp))) {
                    return Err(BridgeRpcError { code: "AlreadyAtWar".into(), message: "These corporations are already at war".into() });
                }
                let owned_ok = |s: &&eve_db::UpwellStructureRecord| s.owner_corp_id == my_corp && s.state != "Destroyed";
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

        ("warRegistry", "GetWars")
        | ("warsInfoMgr", "GetWarsByOwnerID")
        | ("warsInfoMgr", "GetWarsByOwners")
        | ("warsInfoMgr", "GetTop50")
        | ("warsInfoMgr", "GetWarsRequiringAssistance")
        | ("warsInfoMgr", "GetWarsForStructure") => {
            let filter = arg_i64(0).unwrap_or(my_corp);
            let owner_list: Vec<i64> = match args.first() {
                Some(PyRep::List(items)) => items.iter().filter_map(|it| it.as_i64()).collect(),
                _ => vec![filter],
            };
            let mut rows: Vec<(i64, usize, u64, Vec<PyRep>)> = ctx.db.with_snapshot(|snap| {
                snap.wars.values().filter_map(|w| {
                    let meta = snap.war_state.wars.get(&w.war_id).cloned().unwrap_or_default();
                    let keep = match method {
                        "GetTop50" => w.active,
                        "GetWarsRequiringAssistance" => w.active && meta.open_for_allies,
                        "GetWarsForStructure" => meta.war_hq_id == Some(filter),
                        "GetWarsByOwners" => owner_list.contains(&w.aggressor_corp_id) || owner_list.contains(&w.defender_corp_id),
                        _ => w.aggressor_corp_id == filter || w.defender_corp_id == filter,
                    };
                    if !keep {
                        return None;
                    }
                    let (k_cnt, k_isk) = if method == "GetTop50" {
                        war_kills_in_snap(ctx, snap, w.war_id)
                            .map(|(_, _, _, ks)| (ks.len(), ks.iter().map(|k| k.isk).sum::<f64>().to_bits()))
                            .unwrap_or((0, 0))
                    } else {
                        (0, 0)
                    };
                    Some((w.war_id, k_cnt, k_isk, war_row(w, &meta, now)))
                }).collect()
            });
            if method == "GetTop50" {
                rows.sort_by_key(|r| (std::cmp::Reverse(r.1), std::cmp::Reverse(r.2), r.0));
                rows.truncate(50);
            } else {
                rows.sort_by_key(|r| r.0);
            }
            if (service, method) == ("warRegistry", "GetWars") {
                // The bound `warRegistry` moniker answers `{warID: war}`: `war_cso.GetWars`
                // passes it to `PopulateCacheAndCleanupWars`, which iterates `wars.itervalues()`
                // (`eve/client/script/ui/services/wars/war_cso.pyj:128-138`).
                return Some(Ok(PyRep::Dict(
                    rows.into_iter()
                        .map(|(id, _, _, row)| (PyRep::Integer(id), PyRep::keyval(WAR_COLUMNS.iter().copied().zip(row))))
                        .collect(),
                )));
            }
            Some(Ok(PyRep::rowset(WAR_COLUMNS.to_vec(), rows.into_iter().map(|r| r.3).collect())))
        }

        ("warRegistry", "Bind") => {
            let object_id = arg_i64(0).unwrap_or(my_corp);
            match ctx.bind_object(session, "warRegistry", object_id) {
                Ok(bound_id) => Some(Ok(PyRep::Integer(bound_id as i64))),
                Err(e) => Some(Err(e)),
            }
        }

        ("warRegistry", "UnBind") | ("warRegistry", "Unbind") => {
            let bound_id = arg_i64(0).unwrap_or(0) as u64;
            if bound_id > 0 {
                if let Err(e) = ctx.release_bound_object(session, bound_id) {
                    return Some(Err(e));
                }
            }
            Some(Ok(PyRep::None))
        }

        ("warRegistry", "SetSessionCheck") => {
            // const-ok: session check registration returns None
            Some(Ok(PyRep::None))
        }

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

        ("warsInfoMgr", "CreateSurrenderNegotiation") | ("warsInfoMgr", "CreateWarAllyOffer") => {
            if !has_war_director_role {
                return err("CorpRoleDenied", "War negotiations require CEO or Director role");
            }
            let war_id = arg_i64(0).unwrap_or(0);
            let isk = args.get(1).and_then(|a| a.as_f64()).unwrap_or(0.0);
            let message = args.get(2).and_then(|a| a.as_str()).unwrap_or("").to_string();
            if !isk.is_finite() || isk < 0.0 {
                return err("InvalidIskValue", "ISK value must be a non-negative number");
            }
            let (Some(w), open_for_allies, is_player_corp) = ctx.db.with_snapshot(|snap| {
                (
                    snap.wars.get(&war_id).cloned(),
                    snap.war_state.wars.get(&war_id).is_some_and(|m| m.open_for_allies),
                    snap.corporations.contains_key(&my_corp),
                )
            }) else {
                return err("WarNotFound", format!("War {war_id} not found"));
            };
            if !w.active {
                return err("WarNotActive", format!("War {war_id} has ended"));
            }
            let party = my_corp == w.aggressor_corp_id || my_corp == w.defender_corp_id;
            let kind = if method == "CreateSurrenderNegotiation" { NegKind::Surrender } else { NegKind::Ally };
            match kind {
                NegKind::Surrender if !party => return err("NotWarParticipant", "Only war participants can negotiate surrender"),
                NegKind::Ally if party => return err("CannotAllyOwnWar", "War participants cannot offer themselves as allies"),
                NegKind::Ally if !open_for_allies => {
                    return err("WarNotOpenForAllies", "The defender has not opened this war for allies")
                }
                NegKind::Ally if !is_player_corp => {
                    return err("NpcCorporationCannotAlly", "Only player corporations can ally in wars")
                }
                _ => {}
            }
            let id = ctx.db.with_mut_snapshot(|snap| {
                let st = &mut snap.war_state;
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
                id
            });
            Some(Ok(PyRep::keyval([
                ("negotiationID", PyRep::Integer(id)),
                ("warID", PyRep::Integer(war_id)),
                ("ownerID", PyRep::Integer(my_corp)),
                ("iskValue", PyRep::Real(isk)),
                ("status", PyRep::String("Offered".into())),
            ])))
        }

        ("warsInfoMgr", "GetSurrenderNegotiations") => {
            Some(Ok(PyRep::rowset(NEG_COLUMNS.to_vec(), neg_rows(ctx, arg_i64(0).unwrap_or(0), NegKind::Surrender))))
        }
        ("warsInfoMgr", "GetAllyNegotiations") => {
            Some(Ok(PyRep::rowset(NEG_COLUMNS.to_vec(), neg_rows(ctx, arg_i64(0).unwrap_or(0), NegKind::Ally))))
        }

        ("warsInfoMgr", "AcceptSurrender") | ("warsInfoMgr", "DeclineSurrender") | ("warsInfoMgr", "AcceptAllyNegotiation") | ("warsInfoMgr", "DeclineAllyOffer") | ("warsInfoMgr", "RetractWarAllyOffer") => {
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
            let expected_kind = if method.contains("Surrender") { NegKind::Surrender } else { NegKind::Ally };
            if n.kind != expected_kind {
                return err("NegotiationNotFound", format!("Negotiation {neg_id} is not of the requested kind"));
            }
            let allowed = match method {
                "RetractWarAllyOffer" => my_corp == n.from_corp,
                "AcceptAllyNegotiation" | "DeclineAllyOffer" => my_corp == w.defender_corp_id,
                _ => my_corp != n.from_corp && (my_corp == w.aggressor_corp_id || my_corp == w.defender_corp_id),
            };
            if !allowed {
                return err("NotNegotiationCounterparty", "Your corporation cannot act on this negotiation");
            }
            let accepting = method.starts_with("Accept");
            if accepting && !w.active {
                return err("WarNotActive", format!("War {} has ended", n.war_id));
            }
            let effective = now + WAR_TIMER_SECS;
            let status = match method {
                "AcceptSurrender" | "AcceptAllyNegotiation" => "Accepted",
                "RetractWarAllyOffer" => "Retracted",
                _ => "Declined",
            };
            let tx_res = ctx.db.try_with_mut_snapshot(|snap| {
                if accepting {
                    // Surrender: the offering side pays the acceptor. Ally: the defender pays the ally.
                    let (payer, payee) = if n.kind == NegKind::Surrender {
                        (n.from_corp, my_corp)
                    } else {
                        (my_corp, n.from_corp)
                    };
                    debit_corp(snap, payer, n.isk)?;
                    credit_corp(snap, payee, n.isk);
                }
                let st = &mut snap.war_state;
                st.negotiations[idx].status = status.into();
                if accepting {
                    st.negotiations[idx].effective_unix = Some(effective);
                    if n.kind == NegKind::Surrender {
                        st.wars.entry(n.war_id).or_default().finish_at_unix = Some(effective);
                        let expiry = effective + PEACE_TREATY_SECS;
                        st.peace_treaties.push(PeaceTreaty {
                            war_id: n.war_id,
                            corp_a: w.aggressor_corp_id,
                            corp_b: w.defender_corp_id,
                            expiry_unix: expiry,
                        });
                        snap.war_lifecycle_meta.entry(n.war_id).or_default().finished_unix =
                            Some(effective);
                    } else {
                        snap.war_lifecycle_meta
                            .entry(n.war_id)
                            .or_default()
                            .allies
                            .push(eve_db::snapshot_ext::WarAllyRecord {
                                corporation_id: Some(n.from_corp),
                                alliance_id: None,
                            });
                    }
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
                if n.kind == NegKind::Surrender {
                    let expiry = effective + PEACE_TREATY_SECS;
                    out.push(("timeFinished", PyRep::Integer(effective as i64)));
                    out.push(("peaceTreatyExpiry", PyRep::Integer(expiry as i64)));
                } else {
                    out.push(("allyID", PyRep::Integer(n.from_corp)));
                    out.push(("timeStarted", PyRep::Integer(effective as i64)));
                }
            }
            emit_war_changed(
                ctx,
                n.war_id,
                vec![("timeFinished", PyRep::None, PyRep::Integer(effective as i64))],
            );
            Some(Ok(PyRep::keyval(out)))
        }

        ("warsInfoMgr", "RetractMutualWar") => {
            if !has_war_director_role {
                return err("CorpRoleDenied", "Retracting mutual war requires CEO or Director role");
            }
            let war_id = arg_i64(0).unwrap_or(0);
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

        ("warsInfoMgr", "SetOpenForAllies") => {
            if !has_war_director_role {
                return err("CorpRoleDenied", "Opening a war for allies requires CEO or Director role");
            }
            let war_id = arg_i64(0).unwrap_or(0);
            let open = matches!(args.get(1), Some(PyRep::Bool(true)) | Some(PyRep::Integer(1)));
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

        ("warsInfoMgr", "GetAllyRowsByWarID") => {
            let war_id = arg_i64(0).unwrap_or(0);
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

        ("mutualWarInviteMgr", "SendInviteByPlayer") => {
            if !has_war_director_role {
                return err("CorpRoleDenied", "Sending mutual war invites requires CEO or Director role");
            }
            let to_corp = arg_i64(0).unwrap_or(0);
            let (valid, blocked) = ctx.db.with_snapshot(|snap| {
                (
                    snap.corporations.contains_key(&my_corp)
                        && snap.corporations.contains_key(&to_corp)
                        && to_corp != my_corp,
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
            let war_id = ctx.db.with_mut_snapshot(|snap| {
                snap.war_state.mutual_invites.remove(&(from_corp, my_corp));
                snap.next_entity_id += 1;
                let wid = snap.next_entity_id;
                snap.wars.insert(wid, WarRecord { war_id: wid, aggressor_corp_id: from_corp, defender_corp_id: my_corp, mutual: true, started_unix: now, active: true });
                snap.war_state.wars.insert(wid, WarMeta::default());
                wid
            });
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

        ("warStatisticMgr", "UnBind") | ("warStatisticMgr", "Unbind") => {
            let bound_id = arg_i64(0).unwrap_or(0) as u64;
            if bound_id > 0 {
                if let Err(e) = ctx.release_bound_object(session, bound_id) {
                    return Some(Err(e));
                }
            }
            Some(Ok(PyRep::None))
        }

        ("warStatisticMgr", "SetSessionCheck") => {
            // const-ok: session check registration returns None
            Some(Ok(PyRep::None))
        }

        ("warStatisticMgr", "GetBaseInfo") => {
            let war_id = resolve_war_stat_id(ctx, session, arg_i64(0), bound_obj_id, my_corp);
            let Some((agg_id, def_id, allies, kills)) = war_kills(ctx, war_id) else {
                return err("WarNotFound", format!("War {war_id} not found"));
            };
            let (mut k_agg, mut k_def, mut isk_agg, mut isk_def) = (0i64, 0i64, 0.0f64, 0.0f64);
            for k in &kills {
                if k.attacker_corp == agg_id { k_agg += 1; isk_agg += k.isk; } else { k_def += 1; isk_def += k.isk; }
            }
            Some(Ok(PyRep::keyval([
                ("warID", PyRep::Integer(war_id)),
                ("declaredByID", PyRep::Integer(agg_id)),
                ("againstID", PyRep::Integer(def_id)),
                ("allies", PyRep::Dict(allies.into_iter().map(|aid| (PyRep::Integer(aid), PyRep::keyval([("allyID", PyRep::Integer(aid))]))).collect())),
                ("killsForAggressor", PyRep::Integer(k_agg)),
                ("killsForDefender", PyRep::Integer(k_def)),
                ("iskDestroyedByAggressor", PyRep::Real(isk_agg)),
                ("iskDestroyedByDefender", PyRep::Real(isk_def)),
            ])))
        }

        ("warStatisticMgr", "GetKills") | ("warStatisticMgr", "GetKillsByGroup") => {
            let war_id = resolve_war_stat_id(ctx, session, arg_i64(0), bound_obj_id, my_corp);
            let Some((_, _, _, kills)) = war_kills(ctx, war_id) else {
                return err("WarNotFound", format!("War {war_id} not found"));
            };
            if method == "GetKillsByGroup" {
                let mut by_grp: BTreeMap<u32, (i64, f64)> = BTreeMap::new();
                for k in &kills {
                    let e = by_grp.entry(k.group_id).or_default();
                    e.0 += 1;
                    e.1 += k.isk;
                }
                let rows = by_grp.into_iter().map(|(g, (n, isk))| vec![PyRep::Integer(g as i64), PyRep::Integer(n), PyRep::Real(isk)]).collect();
                return Some(Ok(PyRep::rowset(vec!["groupID", "kills", "iskDestroyed"], rows)));
            }
            let rows = kills.into_iter().map(|k| {
                vec![PyRep::Integer(k.kill_id), PyRep::Integer(war_id), PyRep::Integer(k.attacker_corp), PyRep::Integer(k.victim_corp), PyRep::Integer(k.group_id as i64), PyRep::Real(k.isk)]
            }).collect();
            Some(Ok(PyRep::rowset(vec!["killID", "warID", "attackerCorpID", "victimCorpID", "groupID", "iskLost"], rows)))
        }

        ("warStatisticMgr", "GetKillMail") => {
            let kill_id = arg_i64(0).unwrap_or(0);
            let Some(km) = ctx.db.with_snapshot(|snap| snap.killmails.get(&kill_id).cloned()) else {
                return err("KillmailNotFound", format!("Killmail {kill_id} not found"));
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
