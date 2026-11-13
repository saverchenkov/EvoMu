//! Helper predicates and state synchronization for `social_wars` (`#506`).

#![forbid(unsafe_code)]

use crate::router::{cache_notifications::NotificationTarget, ServerContext};
use eve_db::{
    IskCents, PersistedNegKind as NegKind, PersistedWarMeta as WarMeta, UpwellStructureRecord,
    WarRecord, WorldStateSnapshot,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};

pub(super) const WAR_DECLARATION_FEE_ISK: i64 = 100_000_000;
pub(super) const WAR_TIMER_SECS: u64 = 86_400;
pub(super) const PEACE_TREATY_SECS: u64 = 14 * 86_400;

pub(super) const WAR_COLUMNS: [&str; 11] = [
    "warID",
    "declaredByID",
    "againstID",
    "timeDeclared",
    "timeStarted",
    "timeFinished",
    "mutual",
    "openForAllies",
    "warHQ",
    "active",
    "combatActive",
];

pub(super) const NEG_COLUMNS: [&str; 7] = [
    "negotiationID",
    "warID",
    "ownerID",
    "iskValue",
    "message",
    "status",
    "timeEffective",
];

pub(super) fn err(code: &str, message: impl Into<String>) -> Option<Result<PyRep, BridgeRpcError>> {
    Some(Err(BridgeRpcError {
        code: code.into(),
        message: message.into(),
        client_error: None,
    }))
}

/// Validates that `s` is an eligible high-sec (`>= 0.45`), dockable non-FLEX, Quantum-Cored,
/// combat-operational Upwell War HQ owned by `owner_corp_id` (`#506`).
pub(super) fn is_valid_war_hq(
    ctx: &ServerContext,
    corporations: &std::collections::HashMap<i64, eve_db::CorporationRecord>,
    owner_corp_id: i64,
    s: &UpwellStructureRecord,
) -> bool {
    let owns_or_allied = s.owner_corp_id == owner_corp_id
        || corporations
            .get(&s.owner_corp_id)
            .and_then(|c| c.alliance_id)
            == Some(owner_corp_id);
    if !owns_or_allied || !s.quantum_core_installed {
        return false;
    }
    if !matches!(
        s.state.as_str(),
        "ShieldVulnerable"
            | "ArmorReinforced"
            | "ArmorVulnerable"
            | "HullReinforced"
            | "HullVulnerable"
    ) {
        return false;
    }
    if s.solar_system_id >= 31_000_000
        || !ctx
            .sde
            .solar_systems
            .get(&s.solar_system_id)
            .is_some_and(|sys| sys.security_status >= 0.45)
    {
        return false;
    }
    crate::services::station_structure::structure_lifecycle::structure_profile(ctx, s.type_id)
        .is_some_and(|p| {
            p.hull_class
                != crate::services::station_structure::structure_lifecycle::HullClass::Flex
        })
}

pub(super) fn same_alliance(snap: &WorldStateSnapshot, corp_a: i64, corp_b: i64) -> bool {
    let aa = snap
        .corporations
        .get(&corp_a)
        .and_then(|c| c.alliance_id)
        .filter(|&a| a > 0);
    aa.is_some()
        && aa
            == snap
                .corporations
                .get(&corp_b)
                .and_then(|c| c.alliance_id)
                .filter(|&a| a > 0)
}

pub(super) fn already_at_war(snap: &WorldStateSnapshot, corp_a: i64, corp_b: i64) -> bool {
    snap.wars.values().any(|w| {
        w.active
            && ((w.aggressor_corp_id == corp_a && w.defender_corp_id == corp_b)
                || (w.aggressor_corp_id == corp_b && w.defender_corp_id == corp_a))
    })
}

pub(super) fn is_war_participant_corp(
    snap: &WorldStateSnapshot,
    war_id: i64,
    my_corp: i64,
    now_unix: u64,
) -> bool {
    let Some(w) = snap.wars.get(&war_id) else {
        return false;
    };
    if my_corp == w.aggressor_corp_id || my_corp == w.defender_corp_id {
        return true;
    }
    if same_alliance(snap, my_corp, w.aggressor_corp_id)
        || same_alliance(snap, my_corp, w.defender_corp_id)
    {
        return true;
    }
    snap.war_state.negotiations.iter().any(|n| {
        n.war_id == war_id
            && n.kind == NegKind::Ally
            && n.status == "Accepted"
            && now_unix >= n.effective_unix.unwrap_or(0)
            && n.from_corp == my_corp
    })
}

pub(super) fn war_eligible(snap: &WorldStateSnapshot, corp_id: i64) -> bool {
    snap.structures.values().any(|s| {
        s.state != "Destroyed"
            && (s.owner_corp_id == corp_id
                || snap
                    .corporations
                    .get(&s.owner_corp_id)
                    .and_then(|c| c.alliance_id)
                    == Some(corp_id))
    })
}

pub(super) fn debit_corp(
    snap: &mut WorldStateSnapshot,
    corp_id: i64,
    amount: IskCents,
) -> Result<(), BridgeRpcError> {
    let corp = snap.corporations.get_mut(&corp_id).ok_or_else(|| BridgeRpcError {
        code: "CorporationNotFound".into(),
        message: format!("Corporation {corp_id} not found"),
        client_error: None,
    })?;
    corp.ensure_wallet_divisions();
    let cur = corp.division_balance(1);
    let next = cur.checked_sub(amount).ok_or_else(|| BridgeRpcError {
        code: "InsufficientFunds".into(),
        message: format!(
            "Corporation {corp_id} master wallet cannot cover {} ISK",
            amount.to_wire_f64()
        ),
        client_error: None,
    })?;
    corp.set_division_balance(1, next);
    Ok(())
}

pub(super) fn credit_corp(snap: &mut WorldStateSnapshot, corp_id: i64, amount: IskCents) {
    if let Some(corp) = snap.corporations.get_mut(&corp_id) {
        corp.credit_division(1, amount);
    }
}

pub(crate) fn sync_wars(ctx: &ServerContext) {
    let now = ctx.now_unix();
    let needs_end = ctx.db.with_snapshot(|snap| {
        snap.wars.values().filter(|w| w.active).any(|w| {
            let meta = snap.war_state.wars.get(&w.war_id).cloned().unwrap_or_default();
            let finished = meta.finish_at_unix.is_some_and(|f| now >= f);
            let hq_lost = meta.war_hq_id.is_some_and(|hq| {
                snap.structures
                    .get(&hq)
                    .is_none_or(|s| !is_valid_war_hq(ctx, &snap.corporations, w.aggressor_corp_id, s))
            });
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
            let hq_lost = meta.war_hq_id.is_some_and(|hq| {
                snap.structures
                    .get(&hq)
                    .is_none_or(|s| !is_valid_war_hq(ctx, &snap.corporations, w.aggressor_corp_id, s))
            });
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
        emit_war_changed(ctx, wid, vec![("active", PyRep::Bool(true), PyRep::Bool(false))]);
    }
}

pub(super) fn emit_mutual_wars_updated(
    ctx: &ServerContext,
    actor_char_id: i64,
    corp_a: i64,
    corp_b: i64,
) {
    let mut sent = ctx.notify(
        NotificationTarget::Corporation(corp_a),
        "OnMutualWarsUpdated_Remote",
        "corpid",
        vec![],
    );
    if corp_b != corp_a {
        sent.extend(ctx.notify(
            NotificationTarget::Corporation(corp_b),
            "OnMutualWarsUpdated_Remote",
            "corpid",
            vec![],
        ));
    }
    if !sent.iter().any(|n| n.recipient_char_id == actor_char_id) {
        ctx.notify(
            NotificationTarget::Character(actor_char_id),
            "OnMutualWarsUpdated_Remote",
            "charid",
            vec![],
        );
    }
}

pub(super) fn emit_war_changed(
    ctx: &ServerContext,
    war_id: i64,
    change_entries: Vec<(&str, PyRep, PyRep)>,
) {
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
            .map(|(k, old_v, new_v)| (PyRep::String(k.into()), PyRep::Tuple(vec![old_v, new_v])))
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

pub(super) fn war_row(w: &WarRecord, meta: &WarMeta, now: u64) -> Vec<PyRep> {
    let started = w.started_unix + WAR_TIMER_SECS;
    vec![
        PyRep::Integer(w.war_id),
        PyRep::Integer(w.aggressor_corp_id),
        PyRep::Integer(w.defender_corp_id),
        PyRep::Integer(w.started_unix as i64),
        PyRep::Integer(started as i64),
        meta.finish_at_unix
            .map(|f| PyRep::Integer(f as i64))
            .unwrap_or(PyRep::None),
        PyRep::Bool(w.mutual),
        PyRep::Bool(meta.open_for_allies),
        meta.war_hq_id.map(PyRep::Integer).unwrap_or(PyRep::None),
        PyRep::Bool(w.active),
        PyRep::Bool(w.active && now >= started),
    ]
}

pub(super) fn war_crowset(rows: Vec<Vec<PyRep>>) -> PyRep {
    use eve_proto::{DBRowDescriptor, DbColumnType};
    DBRowDescriptor::new([
        ("warID", DbColumnType::I8),
        ("declaredByID", DbColumnType::I8),
        ("againstID", DbColumnType::I8),
        ("timeDeclared", DbColumnType::I8),
        ("timeStarted", DbColumnType::I8),
        ("timeFinished", DbColumnType::I8),
        ("mutual", DbColumnType::Bool),
        ("openForAllies", DbColumnType::Bool),
        ("warHQ", DbColumnType::I8),
        ("active", DbColumnType::Bool),
        ("combatActive", DbColumnType::Bool),
    ])
    .build_crowset(rows)
}

pub(super) fn neg_rows_for_caller(
    ctx: &ServerContext,
    war_id: i64,
    kind: NegKind,
    my_corp: i64,
) -> Result<Vec<Vec<PyRep>>, BridgeRpcError> {
    let now_unix = ctx.now_unix();
    ctx.db.with_snapshot(|snap| {
        let is_participant = is_war_participant_corp(snap, war_id, my_corp, now_unix);
        if kind == NegKind::Surrender && !is_participant {
            return Err(BridgeRpcError {
                code: "NotWarParticipant".into(),
                message: "Only war participants may view surrender negotiations".into(),
                client_error: None,
            });
        }
        Ok(snap
            .war_state
            .negotiations
            .iter()
            .filter(|n| {
                n.war_id == war_id
                    && n.kind == kind
                    && (is_participant || (kind == NegKind::Ally && n.from_corp == my_corp))
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
            .collect())
    })
}

pub(super) fn resolve_war_stat_id(
    ctx: &ServerContext,
    session: &SessionState,
    arg0: Option<i64>,
    bound_obj_id: Option<i64>,
    my_corp: i64,
) -> i64 {
    arg0.filter(|&id| id > 0)
        .or_else(|| bound_obj_id.filter(|&id| id > 0))
        .or_else(|| {
            ctx.bound_objects
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .values()
                .filter(|b| {
                    b.session_id == session.session_id
                        && b.service == "warStatisticMgr"
                        && b.object_id > 0
                })
                .max_by_key(|b| b.bound_id)
                .map(|b| b.object_id)
        })
        .or_else(|| {
            ctx.db.with_snapshot(|snap| {
                snap.wars
                    .values()
                    .find(|w| w.aggressor_corp_id == my_corp || w.defender_corp_id == my_corp)
                    .map(|w| w.war_id)
            })
        })
        .unwrap_or(0)
}

#[cfg(test)]
#[path = "social_wars_helpers_edges_tests.rs"]
mod social_wars_helpers_edges_tests;
