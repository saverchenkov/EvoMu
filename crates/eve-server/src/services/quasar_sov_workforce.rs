//! `quasar_sov_workforce` — Equinox Sovereignty Hub workforce transfer network (`Q-8`).
//!
//! Backs `eve_public.sovereignty.hub.workforce.api` (`GetStateRequest`, `GetConfigurationRequest`,
//! `ConfigureRequest`, `GetNetworkableHubsRequest`) with the persisted
//! [`PersistedWorkforceMode`] of each hub. `ConfigureRequest` emits the canonical V24
//! `eve_public.sovereignty.resource.transfer.workforce.api.ConfiguredNotice` and, for every hub
//! whose computed transfer state changed, `StateChangedNotice` (+ `ResourcesSimulatedNotice`).

#![forbid(unsafe_code)]

use super::quasar_sov_hub::{
    any, caller, compute_hub_resources, emit_hub_diffs, encode_hub_upgrades,
    enforce_hub_power_overload, hub_views, id_field, owned_structure, push_notice, required_id,
    require_roles, run_simulation, same_owner, transact, Caller, RpcError, HUB_MANAGER_ROLES,
};
use super::quasar_space_services::{get_identifier_or_int, get_int_field};
use super::sov_services::TYPE_SOVEREIGNTY_HUB;
use crate::quasar_gateway::{PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_NOT_FOUND};
use eve_db::{PersistedSovHubData, PersistedWorkforceMode, WorldRepository, WorldStateSnapshot};
use eve_proto::quasar::{encode_bytes_field, encode_varint_field, DynamicQuasarMessage, DynamicValue};
use eve_sde::SdeRegistry;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

/// Maximum stargate jumps between two hubs of a workforce transfer link (server rule).
pub const WORKFORCE_TRANSFER_MAX_JUMPS: usize = 5;

const CONFIGURED_NOTICE: &str = "eve_public.sovereignty.resource.transfer.workforce.api.ConfiguredNotice";

fn hub_sys(snap: &WorldStateSnapshot, hub_id: i64) -> Option<u32> {
    snap.structures
        .get(&hub_id)
        .filter(|s| !s.state.eq_ignore_ascii_case("destroyed"))
        .map(|s| s.solar_system_id)
}

fn hub_owner(snap: &WorldStateSnapshot, hub_id: i64) -> Option<i64> {
    snap.structures.get(&hub_id).map(|s| s.owner_corp_id)
}

fn mode(snap: &WorldStateSnapshot, hub_id: i64) -> PersistedWorkforceMode {
    snap.sov_runtime.sov_hubs.get(&hub_id).map(|h| h.workforce.clone()).unwrap_or_default()
}

/// `true` when both hubs share an owning corporation or alliance.
fn linked_owners(snap: &WorldStateSnapshot, a: i64, b: i64) -> bool {
    match (hub_owner(snap, a), hub_owner(snap, b)) {
        (Some(ca), Some(cb)) => {
            let probe = Caller { cid: 0, corp: ca, alliance: super::quasar_sov_hub::corp_alliance(snap, ca) };
            same_owner(snap, &probe, cb)
        }
        _ => false,
    }
}

/// `true` when `exporter_id` (exporting to `dest_sys`) can reach `importer_id` directly or via `Transit` relay hubs.
fn can_relay_export(
    snap: &WorldStateSnapshot,
    sde: &SdeRegistry,
    exporter_id: i64,
    dest_sys: u32,
    importer_id: i64,
) -> bool {
    let Some(exp_sys) = hub_sys(snap, exporter_id) else { return false };
    let Some(imp_sys) = hub_sys(snap, importer_id) else { return false };
    if exporter_id == importer_id || !linked_owners(snap, exporter_id, importer_id) {
        return false;
    }
    let exp_reach = systems_within_jumps(sde, exp_sys, WORKFORCE_TRANSFER_MAX_JUMPS);
    if dest_sys == imp_sys && exp_reach.contains_key(&imp_sys) {
        return true;
    }
    // Collect all Transit relay hubs belonging to the same owner/alliance.
    let transit_hubs: Vec<(i64, u32)> = snap
        .sov_runtime
        .sov_hubs
        .iter()
        .filter(|(id, h)| {
            **id != exporter_id
                && **id != importer_id
                && matches!(h.workforce, PersistedWorkforceMode::Transit)
                && linked_owners(snap, exporter_id, **id)
        })
        .filter_map(|(id, _)| hub_sys(snap, *id).map(|sys| (*id, sys)))
        .collect();

    let mut visited = std::collections::HashSet::new();
    let mut queue = VecDeque::new();
    if dest_sys == imp_sys {
        for (tid, tsys) in &transit_hubs {
            if exp_reach.contains_key(tsys) && visited.insert(*tid) {
                queue.push_back(*tsys);
            }
        }
    } else {
        if !exp_reach.contains_key(&dest_sys) {
            return false;
        }
        for (tid, tsys) in &transit_hubs {
            if *tsys == dest_sys && visited.insert(*tid) {
                queue.push_back(*tsys);
            }
        }
    }
    for _ in 0..transit_hubs.len() {
        let Some(cur_sys) = queue.pop_front() else {
            break;
        };
        let reach = systems_within_jumps(sde, cur_sys, WORKFORCE_TRANSFER_MAX_JUMPS);
        if reach.contains_key(&imp_sys) {
            return true;
        }
        for (tid, tsys) in &transit_hubs {
            if reach.contains_key(tsys) && visited.insert(*tid) {
                queue.push_back(*tsys);
            }
        }
    }
    false
}

/// Selects at most one importer hub for `(exporter_id, dest_sys)` so exported workforce is never
/// duplicated across multiple importers. Prefers a direct importer in `dest_sys`, then lowest hub ID.
fn assigned_importer(
    snap: &WorldStateSnapshot,
    sde: &SdeRegistry,
    exporter_id: i64,
    dest_sys: u32,
) -> Option<i64> {
    let exp_sys = hub_sys(snap, exporter_id)?;
    let mut direct_min: Option<i64> = None;
    let mut relay_min: Option<i64> = None;
    for (cand_id, h) in &snap.sov_runtime.sov_hubs {
        if *cand_id == exporter_id {
            continue;
        }
        let PersistedWorkforceMode::Import { sources } = &h.workforce else { continue };
        if !sources.contains(&exp_sys) || !can_relay_export(snap, sde, exporter_id, dest_sys, *cand_id) {
            continue;
        }
        if hub_sys(snap, *cand_id) == Some(dest_sys) {
            direct_min = Some(direct_min.map_or(*cand_id, |m| m.min(*cand_id)));
        } else {
            relay_min = Some(relay_min.map_or(*cand_id, |m| m.min(*cand_id)));
        }
    }
    direct_min.or(relay_min)
}

/// Workforce imported by `hub_id`, per configured source system (including `Transit` relays).
pub fn imports(snap: &WorldStateSnapshot, sde: &SdeRegistry, hub_id: i64) -> Vec<(u32, u64)> {
    let PersistedWorkforceMode::Import { sources } = mode(snap, hub_id) else { return Vec::new() };
    sources
        .iter()
        .map(|src| {
            let amount = snap
                .sov_runtime
                .sov_hubs
                .iter()
                .filter(|(id, _)| **id != hub_id && hub_sys(snap, **id) == Some(*src))
                .filter_map(|(id, h)| match &h.workforce {
                    PersistedWorkforceMode::Export { destination: Some(d), amount }
                        if assigned_importer(snap, sde, *id, *d) == Some(hub_id) =>
                    {
                        Some(*amount)
                    }
                    _ => None,
                })
                .sum();
            (*src, amount)
        })
        .collect()
}

/// Workforce reserved for export by `hub_id` (connected or held as local reserve).
pub fn exported_amount(snap: &WorldStateSnapshot, hub_id: i64) -> u64 {
    match mode(snap, hub_id) {
        PersistedWorkforceMode::Export { amount, .. } => amount,
        _ => 0,
    }
}

/// `true` when the export destination has a hub importing from this hub's system (directly or via `Transit`).
fn export_connected(snap: &WorldStateSnapshot, sde: &SdeRegistry, hub_id: i64) -> Option<u32> {
    let PersistedWorkforceMode::Export { destination: Some(dest), .. } = mode(snap, hub_id) else { return None };
    assigned_importer(snap, sde, hub_id, dest).map(|_| dest)
}

/// Encodes `eve_public.sovereignty.hub.workforce.Configuration` for a persisted mode.
pub fn encode_configuration(m: &PersistedWorkforceMode) -> Vec<u8> {
    let mut b = Vec::new();
    match m {
        PersistedWorkforceMode::Inactive => encode_varint_field(1, 1, &mut b),
        PersistedWorkforceMode::Import { sources } => {
            let mut imp = Vec::new();
            for s in sources {
                let mut src = Vec::new();
                id_field(1, u64::from(*s), &mut src);
                encode_bytes_field(1, &src, &mut imp);
            }
            encode_bytes_field(2, &imp, &mut b);
        }
        PersistedWorkforceMode::Export { destination, amount } => {
            let mut exp = Vec::new();
            encode_export_destination(*destination, &mut exp);
            encode_varint_field(2, *amount, &mut exp);
            encode_bytes_field(3, &exp, &mut b);
        }
        PersistedWorkforceMode::Transit => encode_varint_field(4, 1, &mut b),
    }
    b
}

fn encode_export_destination(destination: Option<u32>, out: &mut Vec<u8>) {
    match destination {
        Some(d) => {
            id_field(1, u64::from(d), out);
            id_field(3, u64::from(d), out);
        }
        None => encode_varint_field(4, 1, out),
    }
}

/// Encodes the computed `eve_public.sovereignty.hub.workforce.State` of a hub.
pub fn encode_state(snap: &WorldStateSnapshot, sde: &SdeRegistry, hub_id: i64) -> Vec<u8> {
    let mut b = Vec::new();
    match mode(snap, hub_id) {
        PersistedWorkforceMode::Inactive => encode_varint_field(1, 1, &mut b),
        PersistedWorkforceMode::Import { .. } => {
            let mut imp = Vec::new();
            for (src_sys, amount) in imports(snap, sde, hub_id) {
                let mut src = Vec::new();
                id_field(1, u64::from(src_sys), &mut src);
                encode_varint_field(2, amount, &mut src);
                encode_bytes_field(1, &src, &mut imp);
            }
            encode_bytes_field(2, &imp, &mut b);
        }
        PersistedWorkforceMode::Export { destination, amount } => {
            let mut exp = Vec::new();
            encode_export_destination(destination, &mut exp);
            encode_varint_field(2, amount, &mut exp);
            let mut link = Vec::new();
            match export_connected(snap, sde, hub_id) {
                Some(dest) => {
                    id_field(1, u64::from(dest), &mut link);
                    encode_varint_field(2, amount, &mut link);
                    encode_bytes_field(5, &link, &mut exp);
                }
                None => {
                    encode_varint_field(1, amount, &mut link);
                    encode_bytes_field(6, &link, &mut exp);
                }
            }
            encode_bytes_field(3, &exp, &mut b);
        }
        PersistedWorkforceMode::Transit => encode_bytes_field(4, &[], &mut b),
    }
    b
}

/// Decodes a `Configuration` sub-message into a persisted workforce mode.
fn decode_configuration(cfg: &DynamicQuasarMessage) -> Result<PersistedWorkforceMode, RpcError> {
    if let Some(DynamicValue::Message(imp)) = cfg.get(2) {
        let sources = match imp.get(1) {
            Some(DynamicValue::Repeated(items)) => items
                .iter()
                .filter_map(|v| match v {
                    DynamicValue::Message(src) => get_identifier_or_int(src, 1).map(|s| s as u32),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        };
        return Ok(PersistedWorkforceMode::Import { sources });
    }
    if let Some(DynamicValue::Message(exp)) = cfg.get(3) {
        let destination = get_identifier_or_int(exp, 3).or_else(|| get_identifier_or_int(exp, 1)).map(|d| d as u32);
        let amount = get_int_field(exp, 2).unwrap_or(0);
        if amount < 0 {
            return Err((STATUS_BAD_REQUEST, "Export amount must not be negative".into()));
        }
        return Ok(PersistedWorkforceMode::Export { destination, amount: amount as u64 });
    }
    if matches!(cfg.get(4), Some(DynamicValue::Bool(true))) {
        return Ok(PersistedWorkforceMode::Transit);
    }
    Ok(PersistedWorkforceMode::Inactive)
}

/// Solar systems reachable from `origin` within `max_jumps` stargate jumps (SDE `mapJumps`).
pub fn systems_within_jumps(sde: &SdeRegistry, origin: u32, max_jumps: usize) -> HashMap<u32, usize> {
    let mut adj: HashMap<u32, Vec<u32>> = HashMap::new();
    for j in sde.jumps.values() {
        adj.entry(j.from_solar_system_id).or_default().push(j.to_solar_system_id);
    }
    let mut dist = HashMap::from([(origin, 0usize)]);
    let mut queue = VecDeque::from([origin]);
    while let Some(sys) = queue.pop_front() {
        let d = dist[&sys];
        if d >= max_jumps {
            continue;
        }
        for next in adj.get(&sys).into_iter().flatten() {
            if !dist.contains_key(next) {
                dist.insert(*next, d + 1);
                queue.push_back(*next);
            }
        }
    }
    dist
}

/// Other sovereignty hubs of the same owner within transfer range of `hub_id` (including
/// multi-hop relays through hubs in [`PersistedWorkforceMode::Transit`]), ordered by id.
pub fn networkable_hubs(snap: &WorldStateSnapshot, sde: &SdeRegistry, hub_id: i64) -> Vec<(i64, u32)> {
    let Some(my_sys) = hub_sys(snap, hub_id) else { return Vec::new() };
    let mut visited = std::collections::HashSet::from([hub_id]);
    let mut queue = VecDeque::from([(hub_id, my_sys)]);
    let mut hubs = Vec::new();
    let max_steps = snap.structures.len().saturating_add(1);
    for _ in 0..max_steps {
        let Some((_cur_id, cur_sys)) = queue.pop_front() else {
            break;
        };
        let reach = systems_within_jumps(sde, cur_sys, WORKFORCE_TRANSFER_MAX_JUMPS);
        for s in snap.structures.values() {
            if s.type_id != TYPE_SOVEREIGNTY_HUB
                || s.state.eq_ignore_ascii_case("destroyed")
                || s.solar_system_id == cur_sys
                || !reach.contains_key(&s.solar_system_id)
                || !linked_owners(snap, hub_id, s.structure_id)
            {
                continue;
            }
            if visited.insert(s.structure_id) {
                hubs.push((s.structure_id, s.solar_system_id));
                if matches!(mode(snap, s.structure_id), PersistedWorkforceMode::Transit) {
                    queue.push_back((s.structure_id, s.solar_system_id));
                }
            }
        }
    }
    hubs.sort_unstable();
    hubs
}

/// Validates and persists a new workforce mode; returns `(solar_system, old_mode)`.
fn apply_configuration(
    snap: &mut WorldStateSnapshot,
    sde: &SdeRegistry,
    c: &Caller,
    hub_id: i64,
    new_mode: PersistedWorkforceMode,
) -> Result<(u32, PersistedWorkforceMode), RpcError> {
    let sys = owned_structure(snap, c, hub_id, &[TYPE_SOVEREIGNTY_HUB], "Sovereignty hub")?.solar_system_id;
    let reachable: Vec<u32> = networkable_hubs(snap, sde, hub_id).into_iter().map(|h| h.1).collect();
    match &new_mode {
        PersistedWorkforceMode::Import { sources } => {
            if let Some(bad) = sources.iter().find(|s| !reachable.contains(s)) {
                return Err((STATUS_BAD_REQUEST, format!("No networkable hub in source system {bad}")));
            }
        }
        PersistedWorkforceMode::Export { destination: Some(d), .. } if !reachable.contains(d) => {
            return Err((STATUS_BAD_REQUEST, format!("No networkable hub in destination system {d}")));
        }
        _ => {}
    }
    let mut hub = snap
        .sov_runtime
        .sov_hubs
        .get(&hub_id)
        .cloned()
        .unwrap_or_else(|| PersistedSovHubData { hub_id, ..Default::default() });
    let old = std::mem::replace(&mut hub.workforce, PersistedWorkforceMode::Inactive);
    snap.sov_runtime.sov_hubs.insert(hub_id, hub.clone());
    if let PersistedWorkforceMode::Export { amount, .. } = &new_mode {
        let r = compute_hub_resources(snap, sde, hub_id).ok_or((STATUS_NOT_FOUND, "hub".to_string()))?;
        let spare = r.workforce_available.saturating_sub(r.workforce_allocated);
        if *amount > spare {
            hub.workforce = old;
            snap.sov_runtime.sov_hubs.insert(hub_id, hub);
            return Err((STATUS_BAD_REQUEST, format!("Cannot export {amount} workforce: only {spare} unallocated")));
        }
    }
    hub.workforce = new_mode;
    snap.sov_runtime.sov_hubs.insert(hub_id, hub);
    Ok((sys, old))
}

/// Registers `eve_public.sovereignty.hub.workforce.api` handlers.
pub fn register_handlers(router: &PublicGatewayRouter, db: Arc<WorldRepository>, sde: Arc<SdeRegistry>) {
    {
        let (db, sde) = (Arc::clone(&db), Arc::clone(&sde));
        router.register_handler(
            "eve_public.sovereignty.hub.workforce.api.GetStateRequest",
            Arc::new(move |ctx, _any, dyn_msg| {
                let hub_id = required_id(dyn_msg, 1, "Hub")?;
                let state = db.with_snapshot(|snap| {
                    let c = caller(ctx, snap)?;
                    owned_structure(snap, &c, hub_id, &[TYPE_SOVEREIGNTY_HUB], "Sovereignty hub")?;
                    Ok::<_, RpcError>(encode_state(snap, &sde, hub_id))
                })?;
                let mut out = Vec::new();
                encode_bytes_field(1, &state, &mut out);
                Ok(any("eve_public.sovereignty.hub.workforce.api.GetStateResponse", out))
            }),
        );
    }
    {
        let db = Arc::clone(&db);
        router.register_handler(
            "eve_public.sovereignty.hub.workforce.api.GetConfigurationRequest",
            Arc::new(move |ctx, _any, dyn_msg| {
                let hub_id = required_id(dyn_msg, 1, "Hub")?;
                let cfg = db.with_snapshot(|snap| {
                    let c = caller(ctx, snap)?;
                    owned_structure(snap, &c, hub_id, &[TYPE_SOVEREIGNTY_HUB], "Sovereignty hub")?;
                    Ok::<_, RpcError>(encode_configuration(&mode(snap, hub_id)))
                })?;
                let mut out = Vec::new();
                encode_bytes_field(1, &cfg, &mut out);
                Ok(any("eve_public.sovereignty.hub.workforce.api.GetConfigurationResponse", out))
            }),
        );
    }
    {
        let (db, sde, pending) = (Arc::clone(&db), Arc::clone(&sde), Arc::clone(&router.pending_notices));
        router.register_handler(
            "eve_public.sovereignty.hub.workforce.api.ConfigureRequest",
            Arc::new(move |ctx, _any, dyn_msg| {
                let hub_id = required_id(dyn_msg, 1, "Hub")?;
                let new_mode = match dyn_msg.get(2) {
                    Some(DynamicValue::Message(cfg)) => decode_configuration(cfg)?,
                    _ => return Err((STATUS_BAD_REQUEST, "Workforce configuration is required".into())),
                };
                transact(&db, |snap| {
                    let c = caller(ctx, snap)?;
                    require_roles(ctx, HUB_MANAGER_ROLES, "configure sovereignty hub workforce")?;
                    owned_structure(snap, &c, hub_id, &[TYPE_SOVEREIGNTY_HUB], "Sovereignty hub")?;
                    run_simulation(snap, &sde, &pending, hub_id, ctx.now_unix());
                    let mut extra: Vec<i64> = std::iter::once(hub_id)
                        .chain(networkable_hubs(snap, &sde, hub_id).into_iter().map(|h| h.0))
                        .collect();
                    let before = hub_views(snap, &sde, &extra);
                    let (sys, old) = apply_configuration(snap, &sde, &c, hub_id, new_mode.clone())?;
                    for (other_id, _) in networkable_hubs(snap, &sde, hub_id) {
                        if !extra.contains(&other_id) {
                            extra.push(other_id);
                        }
                    }
                    // Enforce upgrade power/workforce overload to a network-wide fixed point across
                    // `hub_id` and all linked hubs so severing `Transit` / `Export` / `Import` links
                    // immediately drops starved upgrades into `POWER_STATE_LOW` (`Sovereignty.md:38,147`).
                    let all_hub_ids: std::collections::BTreeSet<i64> = snap
                        .sov_runtime
                        .sov_hubs
                        .keys()
                        .copied()
                        .chain(extra.iter().copied())
                        .collect();
                    let prev_upgrades: std::collections::BTreeMap<i64, _> = all_hub_ids
                        .iter()
                        .filter_map(|&id| {
                            snap.sov_runtime
                                .sov_hubs
                                .get(&id)
                                .map(|h| (id, h.upgrades.clone()))
                        })
                        .collect();
                    for _ in 0..all_hub_ids.len().saturating_add(1) {
                        let mut changed_in_pass = false;
                        for &id in &all_hub_ids {
                            if enforce_hub_power_overload(snap, &sde, id) {
                                changed_in_pass = true;
                            }
                            if let Some(exported) = snap.sov_runtime.sov_hubs.get(&id).and_then(|h| {
                                match h.workforce {
                                    PersistedWorkforceMode::Export { amount, .. } if amount > 0 => {
                                        Some(amount)
                                    }
                                    _ => None,
                                }
                            }) {
                                if let Some(r) = compute_hub_resources(snap, &sde, id) {
                                    let imported_sum: u64 =
                                        imports(snap, &sde, id).iter().map(|s| s.1).sum();
                                    let base_w =
                                        (r.workforce_local + imported_sum).saturating_sub(exported);
                                    let produced_w = r.workforce_available.saturating_sub(base_w);
                                    let max_exportable = (r.workforce_local
                                        + imported_sum
                                        + produced_w)
                                        .saturating_sub(r.workforce_allocated);
                                    if exported > max_exportable {
                                        if let Some(h) = snap.sov_runtime.sov_hubs.get_mut(&id) {
                                            if let PersistedWorkforceMode::Export { amount, .. } =
                                                &mut h.workforce
                                            {
                                                *amount = max_exportable;
                                                changed_in_pass = true;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        if !changed_in_pass {
                            break;
                        }
                    }
                    let mut n = Vec::new();
                    id_field(1, u64::from(sys), &mut n);
                    encode_bytes_field(2, &encode_configuration(&old), &mut n);
                    encode_bytes_field(3, &encode_configuration(&new_mode), &mut n);
                    id_field(4, hub_id as u64, &mut n);
                    push_notice(&pending, sys, CONFIGURED_NOTICE, n);
                    for &id in &all_hub_ids {
                        let changed = snap
                            .sov_runtime
                            .sov_hubs
                            .get(&id)
                            .is_some_and(|h| prev_upgrades.get(&id) != Some(&h.upgrades));
                        if changed {
                            if let Some(h_sys) = hub_sys(snap, id) {
                                let mut sim_n = Vec::new();
                                encode_bytes_field(
                                    1,
                                    &encode_hub_upgrades(&sde, id, snap.sov_runtime.sov_hubs.get(&id)),
                                    &mut sim_n,
                                );
                                push_notice(
                                    &pending,
                                    h_sys,
                                    "eve_public.sovereignty.hub.upgrade.api.HubUpgradesSimulatedNotice",
                                    sim_n,
                                );
                            }
                        }
                    }
                    emit_hub_diffs(&pending, &before, &hub_views(snap, &sde, &extra));
                    Ok(())
                })?;
                Ok(any("eve_public.sovereignty.hub.workforce.api.ConfigureResponse", Vec::new()))
            }),
        );
    }
    {
        let (db, sde) = (Arc::clone(&db), Arc::clone(&sde));
        router.register_handler(
            "eve_public.sovereignty.hub.workforce.api.GetNetworkableHubsRequest",
            Arc::new(move |ctx, _any, dyn_msg| {
                let hub_id = required_id(dyn_msg, 1, "Hub")?;
                let out = db.with_snapshot(|snap| {
                    let c = caller(ctx, snap)?;
                    owned_structure(snap, &c, hub_id, &[TYPE_SOVEREIGNTY_HUB], "Sovereignty hub")?;
                    let mut out = Vec::new();
                    for (other, sys) in networkable_hubs(snap, &sde, hub_id) {
                        let mut h = Vec::new();
                        id_field(1, other as u64, &mut h);
                        id_field(2, u64::from(sys), &mut h);
                        encode_bytes_field(3, &encode_configuration(&mode(snap, other)), &mut h);
                        encode_bytes_field(4, &encode_state(snap, &sde, other), &mut h);
                        encode_bytes_field(1, &h, &mut out);
                    }
                    Ok::<_, RpcError>(out)
                })?;
                Ok(any("eve_public.sovereignty.hub.workforce.api.GetNetworkableHubsResponse", out))
            }),
        );
    }
}

#[cfg(test)]
#[path = "quasar_sov_workforce_tests.rs"]
mod quasar_sov_workforce_tests;

