//! Wars (`feat:war.*`): declaration with War HQ + fee, war lists, surrender negotiations and
//! 2-week peace treaties, mutual wars, defender allies and per-war statistics (`SO-3`).
//!
//! Persistent war rows live in `snap.wars`; negotiation / treaty / HQ bookkeeping lives in
//! `WAR_STATE`. War end conditions (finish timers, War HQ lost, defender no longer
//! war-eligible) are applied lazily by `sync_wars` at the start of every war RPC.

use crate::router::{cache_notifications::NotificationTarget, ServerContext};
use eve_db::{IskCents, WarRecord, WorldStateSnapshot};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::{BTreeMap, HashMap};
use std::sync::{LazyLock, Mutex};

/// CONCORD war declaration bill (ISK).
const WAR_DECLARATION_FEE_ISK: i64 = 100_000_000;
/// Spool-up before a declared / accepted war (or ally) becomes active, and wind-down after surrender.
const WAR_TIMER_SECS: u64 = 86_400;
/// Peace treaty duration after an accepted surrender.
const PEACE_TREATY_SECS: u64 = 14 * 86_400;

#[derive(Clone, Debug, Default)]
struct WarMeta {
    war_hq_id: Option<i64>,
    open_for_allies: bool,
    finish_at_unix: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NegKind {
    Surrender,
    Ally,
}

#[derive(Clone, Debug)]
struct Negotiation {
    id: i64,
    war_id: i64,
    kind: NegKind,
    from_corp: i64,
    isk: IskCents,
    message: String,
    status: &'static str,
    effective_unix: Option<u64>,
}

#[derive(Clone, Debug)]
struct PeaceTreaty {
    war_id: i64,
    corp_a: i64,
    corp_b: i64,
    expiry_unix: u64,
}

#[derive(Default)]
struct WarState {
    wars: HashMap<i64, WarMeta>,
    negotiations: Vec<Negotiation>,
    next_negotiation_id: i64,
    mutual_invites: HashMap<(i64, i64), u64>,
    blocked_mutual_invites: HashMap<i64, bool>,
    peace_treaties: Vec<PeaceTreaty>,
}

static WAR_STATE: LazyLock<Mutex<WarState>> = LazyLock::new(|| Mutex::new(WarState::default()));

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
    let mut st = WAR_STATE.lock().unwrap();
    let ended: Vec<i64> = ctx.db.with_mut_snapshot(|snap| {
        let mut ended = Vec::new();
        let ids: Vec<i64> = snap.wars.values().filter(|w| w.active).map(|w| w.war_id).collect();
        for wid in ids {
            let w = snap.wars[&wid].clone();
            let meta = st.wars.entry(wid).or_default();
            let finished = meta.finish_at_unix.is_some_and(|f| now >= f);
            let hq_lost = meta.war_hq_id.is_some_and(|hq| snap.structures.get(&hq).is_none_or(|s| s.state == "Destroyed"));
            let defender_ineligible = !w.mutual && !war_eligible(snap, w.defender_corp_id);
            if finished || hq_lost || defender_ineligible {
                if let Some(wm) = snap.wars.get_mut(&wid) {
                    wm.active = false;
                }
                meta.finish_at_unix.get_or_insert(now);
                ended.push(wid);
            }
        }
        ended
    });
    drop(st);
    for wid in ended {
        emit_war_changed(
            ctx,
            wid,
            vec![("active", PyRep::Bool(true), PyRep::Bool(false))],
        );
    }
}

fn emit_war_changed(ctx: &ServerContext, war_id: i64, change_entries: Vec<(&str, PyRep, PyRep)>) {
    let now = ctx.now_unix();
    let Some(w) = ctx.db.with_snapshot(|snap| snap.wars.get(&war_id).cloned()) else {
        return;
    };
    let meta = WAR_STATE.lock().unwrap().wars.get(&war_id).cloned().unwrap_or_default();
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
/// final-blow attacker's corporation and the victim's corporation are the war's two sides
/// (current membership). Ship group comes from the SDE type of the victim ship.
fn war_kills(ctx: &ServerContext, war_id: i64) -> Option<(i64, i64, Vec<WarKill>)> {
    ctx.db.with_snapshot(|snap| {
        let w = snap.wars.get(&war_id)?;
        let (agg, def) = (w.aggressor_corp_id, w.defender_corp_id);
        let corp_of = |cid: i64| snap.characters.get(&cid).map(|c| c.corporation_id).unwrap_or(0);
        let mut kills: Vec<WarKill> = snap
            .killmails
            .values()
            .filter_map(|km| {
                let (a, v) = (corp_of(km.attacker_character_id), corp_of(km.victim_character_id));
                ((a == agg && v == def) || (a == def && v == agg)).then(|| WarKill {
                    kill_id: km.kill_id,
                    attacker_corp: a,
                    victim_corp: v,
                    group_id: ctx.sde.types.get(&km.ship_type_id).map(|t| t.group_id).unwrap_or(0),
                    isk: km.total_value_isk,
                })
            })
            .collect();
        kills.sort_by_key(|k| k.kill_id);
        Some((agg, def, kills))
    })
}

fn neg_rows(war_id: i64, kind: NegKind) -> Vec<Vec<PyRep>> {
    WAR_STATE.lock().unwrap().negotiations.iter().filter(|n| n.war_id == war_id && n.kind == kind).map(|n| {
        vec![
            PyRep::Integer(n.id),
            PyRep::Integer(n.war_id),
            PyRep::Integer(n.from_corp),
            PyRep::Real(n.isk.to_wire_f64()),
            PyRep::String(n.message.clone()),
            PyRep::String(n.status.into()),
            n.effective_unix.map(|e| PyRep::Integer(e as i64)).unwrap_or(PyRep::None),
        ]
    }).collect()
}

const NEG_COLUMNS: [&str; 7] = ["negotiationID", "warID", "ownerID", "iskValue", "message", "status", "timeEffective"];

pub(super) fn handle_wars(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    let is_war_rpc = matches!(
        (service, method),
        ("corpRegistry", "DeclareWarAgainst")
            | ("warRegistry", "DeclareWarAgainst")
            | ("warRegistry", "GetWars")
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
            let treaty = WAR_STATE.lock().unwrap().peace_treaties.iter().any(|t| {
                t.expiry_unix > now && ((t.corp_a == my_corp && t.corp_b == defender) || (t.corp_a == defender && t.corp_b == my_corp))
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
                Ok((war_id, hq))
            });
            let (war_id, hq) = match res {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            WAR_STATE.lock().unwrap().wars.insert(war_id, WarMeta { war_hq_id: Some(hq), ..Default::default() });
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
            let st = WAR_STATE.lock().unwrap();
            let mut rows: Vec<(i64, Vec<PyRep>)> = ctx.db.with_snapshot(|snap| {
                snap.wars.values().filter_map(|w| {
                    let meta = st.wars.get(&w.war_id).cloned().unwrap_or_default();
                    let keep = match method {
                        "GetTop50" => true,
                        "GetWarsRequiringAssistance" => w.active && meta.open_for_allies,
                        "GetWarsForStructure" => meta.war_hq_id == Some(filter),
                        "GetWarsByOwners" => owner_list.contains(&w.aggressor_corp_id) || owner_list.contains(&w.defender_corp_id),
                        _ => w.aggressor_corp_id == filter || w.defender_corp_id == filter,
                    };
                    keep.then(|| (w.war_id, war_row(w, &meta, now)))
                }).collect()
            });
            rows.sort_by_key(|r| r.0);
            if method == "GetTop50" {
                rows.truncate(50);
            }
            Some(Ok(PyRep::rowset(WAR_COLUMNS.to_vec(), rows.into_iter().map(|r| r.1).collect())))
        }

        ("warsInfoMgr", "GetPublicWarInfo") => {
            let war_id = arg_i64(0).unwrap_or(0);
            let Some(w) = ctx.db.with_snapshot(|snap| snap.wars.get(&war_id).cloned()) else {
                return err("WarNotFound", format!("War {war_id} not found"));
            };
            let meta = WAR_STATE.lock().unwrap().wars.get(&war_id).cloned().unwrap_or_default();
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
            let Some(w) = ctx.db.with_snapshot(|snap| snap.wars.get(&war_id).cloned()) else {
                return err("WarNotFound", format!("War {war_id} not found"));
            };
            if !w.active {
                return err("WarNotActive", format!("War {war_id} has ended"));
            }
            let party = my_corp == w.aggressor_corp_id || my_corp == w.defender_corp_id;
            let kind = if method == "CreateSurrenderNegotiation" { NegKind::Surrender } else { NegKind::Ally };
            let mut st = WAR_STATE.lock().unwrap();
            match kind {
                NegKind::Surrender if !party => return err("NotWarParticipant", "Only war participants can negotiate surrender"),
                NegKind::Ally if party => return err("CannotAllyOwnWar", "War participants cannot offer themselves as allies"),
                NegKind::Ally if !st.wars.get(&war_id).is_some_and(|m| m.open_for_allies) => {
                    return err("WarNotOpenForAllies", "The defender has not opened this war for allies")
                }
                NegKind::Ally if !ctx.db.with_snapshot(|snap| snap.corporations.contains_key(&my_corp)) => {
                    return err("NpcCorporationCannotAlly", "Only player corporations can ally in wars")
                }
                _ => {}
            }
            st.next_negotiation_id += 1;
            let id = st.next_negotiation_id;
            st.negotiations.push(Negotiation { id, war_id, kind, from_corp: my_corp, isk: IskCents::from_f64_isk(isk), message, status: "Offered", effective_unix: None });
            Some(Ok(PyRep::keyval([
                ("negotiationID", PyRep::Integer(id)),
                ("warID", PyRep::Integer(war_id)),
                ("ownerID", PyRep::Integer(my_corp)),
                ("iskValue", PyRep::Real(isk)),
                ("status", PyRep::String("Offered".into())),
            ])))
        }

        ("warsInfoMgr", "GetSurrenderNegotiations") => {
            Some(Ok(PyRep::rowset(NEG_COLUMNS.to_vec(), neg_rows(arg_i64(0).unwrap_or(0), NegKind::Surrender))))
        }
        ("warsInfoMgr", "GetAllyNegotiations") => {
            Some(Ok(PyRep::rowset(NEG_COLUMNS.to_vec(), neg_rows(arg_i64(0).unwrap_or(0), NegKind::Ally))))
        }

        ("warsInfoMgr", "AcceptSurrender") | ("warsInfoMgr", "DeclineSurrender") | ("warsInfoMgr", "AcceptAllyNegotiation") | ("warsInfoMgr", "DeclineAllyOffer") | ("warsInfoMgr", "RetractWarAllyOffer") => {
            if !has_war_director_role {
                return err("CorpRoleDenied", "Acting on war negotiations requires CEO or Director role");
            }
            let neg_id = arg_i64(0).unwrap_or(0);
            let mut st = WAR_STATE.lock().unwrap();
            let Some(idx) = st.negotiations.iter().position(|n| n.id == neg_id && n.status == "Offered") else {
                return err("NegotiationNotFound", format!("No open negotiation {neg_id}"));
            };
            let n = st.negotiations[idx].clone();
            let Some(w) = ctx.db.with_snapshot(|snap| snap.wars.get(&n.war_id).cloned()) else {
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
            if accepting {
                if !w.active {
                    return err("WarNotActive", format!("War {} has ended", n.war_id));
                }
                // Surrender: the offering side pays the acceptor. Ally: the defender pays the ally.
                let (payer, payee) = if n.kind == NegKind::Surrender { (n.from_corp, my_corp) } else { (my_corp, n.from_corp) };
                let paid = ctx.db.try_with_mut_snapshot(|snap| {
                    debit_corp(snap, payer, n.isk)?;
                    credit_corp(snap, payee, n.isk);
                    Ok::<_, BridgeRpcError>(())
                });
                if let Err(e) = paid {
                    return Some(Err(e));
                }
            }
            let effective = now + WAR_TIMER_SECS;
            let status = match method {
                "AcceptSurrender" | "AcceptAllyNegotiation" => "Accepted",
                "RetractWarAllyOffer" => "Retracted",
                _ => "Declined",
            };
            st.negotiations[idx].status = status;
            let mut out = vec![
                ("negotiationID", PyRep::Integer(neg_id)),
                ("warID", PyRep::Integer(n.war_id)),
                ("status", PyRep::String(status.into())),
            ];
            if accepting {
                st.negotiations[idx].effective_unix = Some(effective);
                if n.kind == NegKind::Surrender {
                    st.wars.entry(n.war_id).or_default().finish_at_unix = Some(effective);
                    let expiry = effective + PEACE_TREATY_SECS;
                    st.peace_treaties.push(PeaceTreaty { war_id: n.war_id, corp_a: w.aggressor_corp_id, corp_b: w.defender_corp_id, expiry_unix: expiry });
                    out.push(("timeFinished", PyRep::Integer(effective as i64)));
                    out.push(("peaceTreatyExpiry", PyRep::Integer(expiry as i64)));
                } else {
                    out.push(("allyID", PyRep::Integer(n.from_corp)));
                    out.push(("timeStarted", PyRep::Integer(effective as i64)));
                }
            }
            drop(st);
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
            WAR_STATE.lock().unwrap().wars.entry(war_id).or_default().finish_at_unix = Some(finish);
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
            let prev = {
                let mut st = WAR_STATE.lock().unwrap();
                let m = st.wars.entry(war_id).or_default();
                std::mem::replace(&mut m.open_for_allies, open)
            };
            emit_war_changed(
                ctx,
                war_id,
                vec![("openForAllies", PyRep::Bool(prev), PyRep::Bool(open))],
            );
            Some(Ok(PyRep::keyval([("warID", PyRep::Integer(war_id)), ("openForAllies", PyRep::Bool(open))])))
        }

        ("warsInfoMgr", "GetAllyRowsByWarID") => {
            let war_id = arg_i64(0).unwrap_or(0);
            let rows: Vec<Vec<PyRep>> = WAR_STATE.lock().unwrap().negotiations.iter()
                .filter(|n| n.war_id == war_id && n.kind == NegKind::Ally && n.status == "Accepted")
                .map(|n| {
                    let start = n.effective_unix.unwrap_or(now);
                    vec![PyRep::Integer(n.from_corp), PyRep::Integer(start as i64), PyRep::Real(n.isk.to_wire_f64()), PyRep::Bool(now >= start)]
                })
                .collect();
            Some(Ok(PyRep::rowset(vec!["allyID", "timeStarted", "iskValue", "active"], rows)))
        }

        ("peaceTreatyMgr", "GetPeaceTreatiesForSession") => {
            let rows: Vec<Vec<PyRep>> = WAR_STATE.lock().unwrap().peace_treaties.iter()
                .filter(|t| t.expiry_unix > now && (t.corp_a == my_corp || t.corp_b == my_corp))
                .map(|t| {
                    let other = if t.corp_a == my_corp { t.corp_b } else { t.corp_a };
                    vec![PyRep::Integer(t.war_id), PyRep::Integer(other), PyRep::Integer(t.expiry_unix as i64)]
                })
                .collect();
            Some(Ok(PyRep::rowset(vec!["warID", "otherOwnerID", "expiryDate"], rows)))
        }

        ("mutualWarInviteMgr", "IsCorpInvitesBlockedPlayer") => {
            let corp_exists = ctx.db.with_snapshot(|snap| snap.corporations.contains_key(&my_corp));
            let blocked = corp_exists
                && WAR_STATE
                    .lock()
                    .unwrap()
                    .blocked_mutual_invites
                    .get(&my_corp)
                    .copied()
                    .unwrap_or(false);
            Some(Ok(PyRep::Bool(blocked)))
        }

        ("mutualWarInviteMgr", "SetInvitesBlockedByPlayer") => {
            if !has_war_director_role {
                return err("CorpRoleDenied", "Changing mutual war invite settings requires CEO or Director role");
            }
            let blocked = matches!(args.first(), Some(PyRep::Bool(true)) | Some(PyRep::Integer(1)));
            let _ = ctx.db.with_snapshot(|snap| snap.corporations.contains_key(&my_corp));
            WAR_STATE
                .lock()
                .unwrap()
                .blocked_mutual_invites
                .insert(my_corp, blocked);
            Some(Ok(PyRep::Bool(true)))
        }

        ("mutualWarInviteMgr", "SendInviteByPlayer") => {
            if !has_war_director_role {
                return err("CorpRoleDenied", "Sending mutual war invites requires CEO or Director role");
            }
            let to_corp = arg_i64(0).unwrap_or(0);
            let valid = ctx.db.with_snapshot(|snap| snap.corporations.contains_key(&my_corp) && snap.corporations.contains_key(&to_corp) && to_corp != my_corp);
            if !valid {
                return err("InvalidWarTarget", format!("Corporation {to_corp} cannot be invited to a mutual war"));
            }
            WAR_STATE.lock().unwrap().mutual_invites.insert((my_corp, to_corp), now);
            Some(Ok(PyRep::keyval([("fromID", PyRep::Integer(my_corp)), ("toID", PyRep::Integer(to_corp)), ("sentDate", PyRep::Integer(now as i64))])))
        }

        ("mutualWarInviteMgr", "GetPendingInvitesForSession") => {
            let rows: Vec<Vec<PyRep>> = WAR_STATE.lock().unwrap().mutual_invites.iter()
                .filter(|(&(f, t), _)| f == my_corp || t == my_corp)
                .map(|(&(f, t), &ts)| vec![PyRep::Integer(f), PyRep::Integer(t), PyRep::Integer(ts as i64)])
                .collect();
            Some(Ok(PyRep::rowset(vec!["fromID", "toID", "sentDate"], rows)))
        }

        ("mutualWarInviteMgr", "WithdrawInviteByPlayer") => {
            if !has_war_director_role {
                return err("CorpRoleDenied", "Withdrawing mutual war invites requires CEO or Director role");
            }
            let to_corp = arg_i64(0).unwrap_or(0);
            let removed = WAR_STATE.lock().unwrap().mutual_invites.remove(&(my_corp, to_corp)).is_some();
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
            if WAR_STATE.lock().unwrap().mutual_invites.remove(&(from_corp, my_corp)).is_none() {
                return err("MutualWarInviteNotFound", format!("No pending mutual war invite from {from_corp}"));
            }
            if !accept {
                return Some(Ok(PyRep::keyval([("accepted", PyRep::Bool(false)), ("warID", PyRep::None)])));
            }
            let war_id = ctx.db.with_mut_snapshot(|snap| {
                snap.next_entity_id += 1;
                let wid = snap.next_entity_id;
                snap.wars.insert(wid, WarRecord { war_id: wid, aggressor_corp_id: from_corp, defender_corp_id: my_corp, mutual: true, started_unix: now, active: true });
                wid
            });
            WAR_STATE.lock().unwrap().wars.insert(war_id, WarMeta::default());
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

        ("warStatisticMgr", "GetBaseInfo") => {
            let war_id = arg_i64(0).unwrap_or(0);
            let Some((agg_id, def_id, kills)) = war_kills(ctx, war_id) else {
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
                ("killsForAggressor", PyRep::Integer(k_agg)),
                ("killsForDefender", PyRep::Integer(k_def)),
                ("iskDestroyedByAggressor", PyRep::Real(isk_agg)),
                ("iskDestroyedByDefender", PyRep::Real(isk_def)),
            ])))
        }

        ("warStatisticMgr", "GetKills") | ("warStatisticMgr", "GetKillsByGroup") => {
            let war_id = arg_i64(0).unwrap_or(0);
            let Some((_, _, kills)) = war_kills(ctx, war_id) else {
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
