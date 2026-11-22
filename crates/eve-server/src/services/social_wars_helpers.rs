//! Helper predicates and state synchronization for `social_wars` (`#506`).

#![forbid(unsafe_code)]

#[cfg(test)]
pub(super) use crate::domain::wars::credit_corp;
pub(crate) use crate::domain::wars::{
    entity_matches_war_owner, is_ally_neg_active_at, resolve_entity_alliance_id,
};
pub(super) use crate::domain::wars::{
    already_at_war, corp_represents_war_side, same_alliance, unix_to_filetime,
    WAR_DECLARATION_FEE_ISK, WAR_TIMER_SECS,
};
use crate::router::{cache_notifications::NotificationTarget, ServerContext};
#[cfg(test)]
use eve_db::IskCents;
use eve_db::{
    PersistedNegKind as NegKind, PersistedWarMeta as WarMeta, UpwellStructureRecord, WarRecord,
    WorldStateSnapshot,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};

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

pub(super) fn rpc_err(err: crate::domain::DomainError) -> BridgeRpcError {
    BridgeRpcError {
        code: err.code,
        message: err.message,
        client_error: None,
    }
}

/// Evaluates whether `my_corp` is a party to `war_id` at `now_unix`, delegating to `domain::wars`.
pub(crate) fn is_war_participant_corp(
    snap: &WorldStateSnapshot,
    war_id: i64,
    my_corp: i64,
    now_unix: u64,
) -> bool {
    crate::domain::wars::is_war_participant(snap, war_id, my_corp, None, now_unix)
}

pub(super) fn war_eligible(snap: &WorldStateSnapshot, corp_id: i64) -> bool {
    snap.structures.values().any(|s| {
        crate::services::station_structure::structure_lifecycle::is_in_space_structure_state(
            &s.state,
        ) && (s.owner_corp_id == corp_id
            || snap
                .corporations
                .get(&s.owner_corp_id)
                .and_then(|c| c.alliance_id)
                == Some(corp_id))
    })
}

#[cfg(test)]
pub(super) fn debit_corp(
    snap: &mut WorldStateSnapshot,
    corp_id: i64,
    amount: IskCents,
) -> Result<(), BridgeRpcError> {
    crate::domain::wars::debit_corp(snap, corp_id, amount).map_err(rpc_err)
}

pub(crate) fn sync_wars(ctx: &ServerContext) {
    let now = ctx.now_unix();
    let (needs_end, has_expired_allies) = ctx.db.with_snapshot(|snap| {
        let needs_end = snap.wars.values().filter(|w| w.active).any(|w| {
            let meta = snap.war_state.wars.get(&w.war_id).cloned().unwrap_or_default();
            let finished = meta.finish_at_unix.is_some_and(|f| now >= f);
            let hq_lost = meta.war_hq_id.is_some_and(|hq| {
                snap.structures
                    .get(&hq)
                    .is_none_or(|s| !is_valid_war_hq(ctx, &snap.corporations, w.aggressor_corp_id, s))
            });
            let defender_ineligible = !w.mutual && !war_eligible(snap, w.defender_corp_id);
            finished || hq_lost || defender_ineligible
        });
        let has_expired_allies = crate::domain::wars::has_expired_lifecycle_allies(snap, now);
        (needs_end, has_expired_allies)
    });
    if !needs_end && !has_expired_allies {
        return;
    }
    let ended: Vec<i64> = ctx.db.with_mut_snapshot(|snap| {
        let mut ended = Vec::new();
        if needs_end {
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
                    crate::domain::wars::deactivate_war(snap, wid, now);
                    ended.push(wid);
                }
            }
        }
        if has_expired_allies {
            crate::domain::wars::prune_expired_lifecycle_allies(snap, now);
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
    let mut sent = ctx.emit_typed(
        NotificationTarget::Corporation(corp_a),
        crate::router::notify_registry::OnMutualWarsUpdatedRemote {},
    );
    if corp_b != corp_a {
        sent.extend(ctx.emit_typed(
            NotificationTarget::Corporation(corp_b),
            crate::router::notify_registry::OnMutualWarsUpdatedRemote {},
        ));
    }
    if !sent.iter().any(|n| n.recipient_char_id == actor_char_id) {
        ctx.emit_typed_with_id_type(
            NotificationTarget::Character(actor_char_id),
            "charid",
            crate::router::notify_registry::OnMutualWarsUpdatedRemote {},
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
    let war_notif = crate::router::notify_registry::OnWarChanged {
        war: war_kv,
        owner_ids,
        change,
    };
    ctx.emit_typed(NotificationTarget::Corporation(w.aggressor_corp_id), war_notif.clone());
    if w.defender_corp_id != w.aggressor_corp_id {
        ctx.emit_typed(NotificationTarget::Corporation(w.defender_corp_id), war_notif);
    }
}

pub(super) fn opt_unix_to_filetime_py(unix_secs: Option<u64>) -> PyRep {
    unix_secs
        .map(|t| PyRep::Integer(unix_to_filetime(t)))
        .unwrap_or(PyRep::None)
}

pub(super) fn war_row(w: &WarRecord, meta: &WarMeta, now: u64) -> Vec<PyRep> {
    let started = crate::domain::wars::war_started_unix(w);
    let combat_active =
        w.active && now >= started && meta.finish_at_unix.is_none_or(|f| now < f);
    vec![
        PyRep::Integer(w.war_id),
        PyRep::Integer(w.aggressor_corp_id),
        PyRep::Integer(w.defender_corp_id),
        PyRep::Integer(unix_to_filetime(w.started_unix)),
        PyRep::Integer(unix_to_filetime(started)),
        opt_unix_to_filetime_py(meta.finish_at_unix),
        PyRep::Bool(w.mutual),
        PyRep::Bool(meta.open_for_allies),
        meta.war_hq_id.map(PyRep::Integer).unwrap_or(PyRep::None),
        PyRep::Bool(w.active),
        PyRep::Bool(combat_active),
    ]
}

pub(super) fn war_crowset(rows: Vec<Vec<PyRep>>) -> PyRep {
    use eve_proto::{DBRowDescriptor, DbColumnType};
    DBRowDescriptor::new([
        ("warID", DbColumnType::I8),
        ("declaredByID", DbColumnType::I8),
        ("againstID", DbColumnType::I8),
        ("timeDeclared", DbColumnType::FileTime),
        ("timeStarted", DbColumnType::FileTime),
        ("timeFinished", DbColumnType::FileTime),
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
                    opt_unix_to_filetime_py(n.effective_unix),
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
