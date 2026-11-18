//! `quasar_sov_workforce` — Equinox Sovereignty Hub workforce transfer network (`Q-8`).
//!
//! Backs `eve_public.sovereignty.hub.workforce.api` (`GetStateRequest`, `GetConfigurationRequest`,
//! `ConfigureRequest`, `GetNetworkableHubsRequest`) with the persisted
//! [`PersistedWorkforceMode`] of each hub. `ConfigureRequest` emits the canonical V24
//! `eve_public.sovereignty.resource.transfer.workforce.api.ConfiguredNotice` and, for every hub
//! whose computed transfer state changed, `StateChangedNotice` (+ `ResourcesSimulatedNotice`).

#![forbid(unsafe_code)]

use super::quasar_sov_hub::{
    caller, corp_owned_structure, emit_hub_diffs, encode_hub_upgrades,
    enforce_hub_power_overload, hub_views, owned_structure, push_notice, required_id,
    require_caller_roles, run_simulation, same_owner, transact, Caller, HubView, PendingNotices,
    RpcError, HUB_MANAGER_ROLES,
};
use super::quasar_space_services::{get_identifier_or_int, get_int_field};
use super::sov_services::TYPE_SOVEREIGNTY_HUB;
use crate::quasar_gateway::{PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_NOT_FOUND};
use eve_db::{PersistedSovHubData, PersistedWorkforceMode, WorldRepository, WorldStateSnapshot};
use eve_proto::pb::{self, ProtoBuilder};
use eve_proto::quasar::{DynamicQuasarMessage, DynamicValue};
use eve_sde::SdeRegistry;
use std::collections::{BTreeMap, HashMap, VecDeque};
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

pub(crate) fn mode(snap: &WorldStateSnapshot, hub_id: i64) -> PersistedWorkforceMode {
    snap.sov_runtime.sov_hubs.get(&hub_id).map(|h| h.workforce.clone()).unwrap_or_default()
}

/// `true` when both hubs share an owning corporation or alliance.
fn linked_owners(snap: &WorldStateSnapshot, a: i64, b: i64) -> bool {
    match (hub_owner(snap, a), hub_owner(snap, b)) {
        (Some(ca), Some(cb)) => {
            let probe = Caller { cid: 0, corp: ca, alliance: super::quasar_sov_hub::corp_alliance(snap, ca), roles: 0 };
            same_owner(snap, &probe, cb)
        }
        _ => false,
    }
}

/// Maximum number of source hubs a single hub may import workforce from (`MAX_IMPORT_SOURCES`).
/// `// DERIVED:` EVE Equinox Workforce network rules allow at most `3` concurrent Import sources per hub.
pub const MAX_IMPORT_SOURCES: usize = 3;

/// `true` when `exporter_id` (exporting to `dest_sys`) can reach `importer_id` directly or via
/// `Transit` relay hubs (cycle-safe multi-hop BFS across all owner/alliance `Transit` hubs).
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
    // Collect all live Transit relay hubs belonging to the same corporation/alliance.
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

    // State is `(transit_hub_id, cur_sys, passed_dest)`. Tracking `(tid, passed_dest)` prevents
    // infinite loops on cyclic hub graphs while allowing multi-hop chains where `dest_sys` itself
    // lies >5 jumps away beyond one or more upstream `Transit` relays.
    let mut visited = std::collections::HashSet::new();
    let mut queue = VecDeque::new();
    for &(tid, tsys) in &transit_hubs {
        if exp_reach.contains_key(&tsys) {
            let passed = dest_sys == imp_sys || tsys == dest_sys;
            if visited.insert((tid, passed)) {
                queue.push_back((tsys, passed));
            }
        }
    }
    while let Some((cur_sys, passed_dest)) = queue.pop_front() {
        let reach = systems_within_jumps(sde, cur_sys, WORKFORCE_TRANSFER_MAX_JUMPS);
        if passed_dest && reach.contains_key(&imp_sys) {
            return true;
        }
        for &(tid, tsys) in &transit_hubs {
            if reach.contains_key(&tsys) {
                let next_passed = passed_dest || tsys == dest_sys;
                if visited.insert((tid, next_passed)) {
                    queue.push_back((tsys, next_passed));
                }
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
                        let cap = super::quasar_sov_hub::max_exportable_workforce(snap, sde, *id)
                            .unwrap_or(0);
                        Some((*amount).min(cap))
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

fn fill_configuration(b: ProtoBuilder, m: &PersistedWorkforceMode) -> ProtoBuilder {
    match m {
        PersistedWorkforceMode::Inactive => b.set("inactive", true),
        PersistedWorkforceMode::Import { sources } => b.set_with("import_settings", |mut imp| {
            for s in sources {
                imp = imp.add_with("sources", |src| src.set("source.sequential", *s));
            }
            imp
        }),
        PersistedWorkforceMode::Export { destination, amount } => b.set_with("export_settings", |exp| {
            fill_export_destination(exp, *destination).set("amount", *amount)
        }),
        PersistedWorkforceMode::Transit => b.set("transit", true),
    }
}

/// Encodes `eve_public.sovereignty.hub.workforce.Configuration` for a persisted mode.
pub fn encode_configuration(m: &PersistedWorkforceMode) -> Vec<u8> {
    crate::quasar_gateway::pb_encode_or_log(
        "quasar_sov_workforce.encode_configuration",
        fill_configuration(pb::message("eve_public.sovereignty.hub.workforce.Configuration"), m),
    )
    .unwrap_or_default()
}

fn fill_export_destination(b: ProtoBuilder, destination: Option<u32>) -> ProtoBuilder {
    match destination {
        Some(d) => b.set("destination.sequential", d).set("destination_system.sequential", d),
        None => b.set("no_destination", true),
    }
}

fn fill_state(
    b: ProtoBuilder,
    snap: &WorldStateSnapshot,
    sde: &SdeRegistry,
    hub_id: i64,
) -> ProtoBuilder {
    match mode(snap, hub_id) {
        PersistedWorkforceMode::Inactive => b.set("inactive", true),
        PersistedWorkforceMode::Import { .. } => b.set_with("import_state", |mut imp| {
            for (src_sys, amount) in imports(snap, sde, hub_id) {
                imp = imp.add_with("sources", |src| {
                    src.set("source.sequential", src_sys).set("amount", amount)
                });
            }
            imp
        }),
        PersistedWorkforceMode::Export { destination, amount } => b.set_with("export_state", |exp| {
            let exp = fill_export_destination(exp, destination).set("amount", amount);
            match export_connected(snap, sde, hub_id) {
                Some(dest) => exp.set_with("connected", |c| {
                    c.set("destination_system.sequential", dest).set("exported_quantity", amount)
                }),
                None => exp.set_with("disconnected", |d| d.set("local_reserve", amount)),
            }
        }),
        PersistedWorkforceMode::Transit => b.set_with("transit", |t| t),
    }
}

/// Encodes the computed `eve_public.sovereignty.hub.workforce.State` of a hub.
pub fn encode_state(snap: &WorldStateSnapshot, sde: &SdeRegistry, hub_id: i64) -> Vec<u8> {
    crate::quasar_gateway::pb_encode_or_log(
        "quasar_sov_workforce.encode_state",
        fill_state(pb::message("eve_public.sovereignty.hub.workforce.State"), snap, sde, hub_id),
    )
    .unwrap_or_default()
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
    if matches!(
        cfg.get(4),
        Some(
            DynamicValue::Bool(true)
                | DynamicValue::Int64(1)
                | DynamicValue::Uint64(1)
                | DynamicValue::Message(_)
                | DynamicValue::Bytes(_)
        )
    ) {
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
    let sys = corp_owned_structure(snap, c, hub_id, &[TYPE_SOVEREIGNTY_HUB], "Sovereignty hub")?.solar_system_id;
    let reachable: Vec<u32> = networkable_hubs(snap, sde, hub_id).into_iter().map(|h| h.1).collect();
    match &new_mode {
        PersistedWorkforceMode::Import { sources } => {
            if sources.len() > MAX_IMPORT_SOURCES {
                return Err((
                    STATUS_BAD_REQUEST,
                    format!("Cannot import workforce from more than {MAX_IMPORT_SOURCES} sources"),
                ));
            }
            let mut seen = std::collections::HashSet::with_capacity(sources.len());
            for &src in sources {
                if src == sys {
                    return Err((
                        STATUS_BAD_REQUEST,
                        "Hub cannot import workforce from its own solar system".to_string(),
                    ));
                }
                if !seen.insert(src) {
                    return Err((
                        STATUS_BAD_REQUEST,
                        format!("Duplicate workforce import source system {src}"),
                    ));
                }
                if !reachable.contains(&src) {
                    return Err((
                        STATUS_BAD_REQUEST,
                        format!("No networkable hub in source system {src}"),
                    ));
                }
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
        let exportable = super::quasar_sov_hub::max_exportable_workforce(snap, sde, hub_id)
            .ok_or((STATUS_NOT_FOUND, "hub".to_string()))?;
        if *amount > exportable {
            hub.workforce = old;
            snap.sov_runtime.sov_hubs.insert(hub_id, hub);
            return Err((
                STATUS_BAD_REQUEST,
                format!(
                    "Cannot export {amount} workforce: only {exportable} exportable (local upgrades and Workforce Mecha-Tooling output cannot be exported)"
                ),
            ));
        }
    }
    hub.workforce = new_mode;
    snap.sov_runtime.sov_hubs.insert(hub_id, hub);
    Ok((sys, old))
}

/// Reconciles upgrade power/workforce overload and workforce export clamping across all hubs to a
/// fixed point, emitting `HubUpgradesSimulatedNotice`, `ResourcesSimulatedNotice`, and workforce
/// `StateChangedNotice` for every hub whose observable state changed relative to `before`.
pub(crate) fn reconcile_hub_workforce_network_and_emit(
    snap: &mut WorldStateSnapshot,
    sde: &SdeRegistry,
    pending: &PendingNotices,
    before: &std::collections::BTreeMap<i64, HubView>,
    extra: &[i64],
) {
    // Enforce upgrade power/workforce overload to a network-wide fixed point across all hubs
    // so severing `Transit` / `Export` / `Import` links or deactivating/destroying a Skyhook
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
            // Clamp excess export BEFORE overload evaluation so active local upgrades are not
            // starved to sustain an export whose Skyhook/Mercenary-Den/Mecha-Tooling backing fell.
            if super::quasar_sov_hub::clamp_hub_export_to_max(snap, sde, id) {
                changed_in_pass = true;
            }
            if enforce_hub_power_overload(snap, sde, id) {
                changed_in_pass = true;
            }
        }
        if !changed_in_pass {
            break;
        }
    }
    for &id in &all_hub_ids {
        let changed = snap
            .sov_runtime
            .sov_hubs
            .get(&id)
            .is_some_and(|h| prev_upgrades.get(&id) != Some(&h.upgrades));
        if changed {
            if let Some(h_sys) = hub_sys(snap, id) {
                if let Some(sim_n) = crate::quasar_gateway::pb_encode_or_log(
                    "quasar_sov_workforce.reconcile.upgrades_simulated",
                    pb::message(
                        "eve_public.sovereignty.hub.upgrade.api.HubUpgradesSimulatedNotice",
                    )
                    .set_raw_message(
                        "hub_upgrades",
                        encode_hub_upgrades(sde, id, snap.sov_runtime.sov_hubs.get(&id)),
                    ),
                ) {
                    push_notice(
                        pending,
                        h_sys,
                        "eve_public.sovereignty.hub.upgrade.api.HubUpgradesSimulatedNotice",
                        sim_n,
                    );
                }
            }
        }
    }
    emit_hub_diffs(pending, before, &hub_views(snap, sde, extra));
}

/// Collects each seed hub ID together with all connected networkable hub IDs (deduplicated).
pub(crate) fn collect_connected_hub_ids(
    snap: &WorldStateSnapshot,
    sde: &SdeRegistry,
    seed_hub_ids: &[i64],
) -> Vec<i64> {
    let mut extra: Vec<i64> = Vec::new();
    for &hid in seed_hub_ids {
        if !extra.contains(&hid) {
            extra.push(hid);
        }
        for (other_id, _) in networkable_hubs(snap, sde, hid) {
            if !extra.contains(&other_id) {
                extra.push(other_id);
            }
        }
    }
    extra
}

/// Syncs local system Sov Hubs and captures connected hub IDs plus pre-mutation `HubView`
/// snapshots before a structure or resource change mutates `snap`.
pub(crate) fn begin_system_hub_network_reconcile(
    snap: &mut WorldStateSnapshot,
    sde: &SdeRegistry,
    pending: &PendingNotices,
    sys_id: u32,
    now_unix: u64,
) -> (Vec<i64>, BTreeMap<i64, HubView>) {
    let local_hub_ids: Vec<i64> = snap
        .structures
        .iter()
        .filter(|(_, s)| {
            s.solar_system_id == sys_id
                && s.type_id == TYPE_SOVEREIGNTY_HUB
                && !s.state.eq_ignore_ascii_case("destroyed")
        })
        .map(|(id, _)| *id)
        .collect();
    for &hid in &local_hub_ids {
        run_simulation(snap, sde, pending, hid, now_unix);
    }
    let extra = collect_connected_hub_ids(snap, sde, &local_hub_ids);
    let before = hub_views(snap, sde, &extra);
    (extra, before)
}

/// Runs `mutate` inside a system Sov Hub network snapshot-reconcile bracket so mutations affecting
/// local planetary power/workforce (e.g. Skyhook activation, deactivation, or destruction)
/// immediately overload/clamp local and downstream hubs and publish Quasar notices.
pub(crate) fn with_system_hub_network_reconcile<T, E>(
    snap: &mut WorldStateSnapshot,
    sde: &SdeRegistry,
    pending: &PendingNotices,
    sys_id: u32,
    now_unix: u64,
    mutate: impl FnOnce(&mut WorldStateSnapshot) -> Result<T, E>,
) -> Result<T, E> {
    let (extra, before) =
        begin_system_hub_network_reconcile(snap, sde, pending, sys_id, now_unix);
    let result = mutate(snap)?;
    reconcile_hub_workforce_network_and_emit(snap, sde, pending, &before, &extra);
    Ok(result)
}

/// Registers `eve_public.sovereignty.hub.workforce.api` handlers.
pub fn register_handlers(router: &PublicGatewayRouter, db: Arc<WorldRepository>, sde: Arc<SdeRegistry>) {
    {
        let (db, sde) = (Arc::clone(&db), Arc::clone(&sde));
        router.register_handler(
            "eve_public.sovereignty.hub.workforce.api.GetStateRequest",
            Arc::new(move |ctx, _any, dyn_msg| {
                let hub_id = required_id(dyn_msg, 1, "Hub")?;
                db.with_snapshot(|snap| {
                    let c = caller(ctx, snap)?;
                    owned_structure(snap, &c, hub_id, &[TYPE_SOVEREIGNTY_HUB], "Sovereignty hub")?;
                    pb::message("eve_public.sovereignty.hub.workforce.api.GetStateResponse")
                        .set_with("state", |st| fill_state(st, snap, &sde, hub_id))
                        .to_any_or_internal_err()
                })
            }),
        );
    }
    {
        let db = Arc::clone(&db);
        router.register_handler(
            "eve_public.sovereignty.hub.workforce.api.GetConfigurationRequest",
            Arc::new(move |ctx, _any, dyn_msg| {
                let hub_id = required_id(dyn_msg, 1, "Hub")?;
                db.with_snapshot(|snap| {
                    let c = caller(ctx, snap)?;
                    owned_structure(snap, &c, hub_id, &[TYPE_SOVEREIGNTY_HUB], "Sovereignty hub")?;
                    let m = mode(snap, hub_id);
                    pb::message("eve_public.sovereignty.hub.workforce.api.GetConfigurationResponse")
                        .set_with("configuration", |cfg| fill_configuration(cfg, &m))
                        .to_any_or_internal_err()
                })
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
                    require_caller_roles(&c, HUB_MANAGER_ROLES, "configure sovereignty hub workforce")?;
                    corp_owned_structure(snap, &c, hub_id, &[TYPE_SOVEREIGNTY_HUB], "Sovereignty hub")?;
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
                    if let Some(n) = crate::quasar_gateway::pb_encode_or_log(
                        "quasar_sov_workforce.configure.configured",
                        pb::message(CONFIGURED_NOTICE)
                            .set("solar_system.sequential", sys)
                            .set_with("old_configuration", |c| fill_configuration(c, &old))
                            .set_with("new_configuration", |c| fill_configuration(c, &new_mode))
                            .set("hub.sequential", hub_id as u64),
                    ) {
                        push_notice(&pending, sys, CONFIGURED_NOTICE, n);
                    }
                    reconcile_hub_workforce_network_and_emit(snap, &sde, &pending, &before, &extra);
                    Ok(())
                })?;
                pb::message("eve_public.sovereignty.hub.workforce.api.ConfigureResponse")
                    .to_any_or_internal_err()
            }),
        );
    }
    {
        let (db, sde) = (Arc::clone(&db), Arc::clone(&sde));
        router.register_handler(
            "eve_public.sovereignty.hub.workforce.api.GetNetworkableHubsRequest",
            Arc::new(move |ctx, _any, dyn_msg| {
                let hub_id = required_id(dyn_msg, 1, "Hub")?;
                db.with_snapshot(|snap| {
                    let c = caller(ctx, snap)?;
                    owned_structure(snap, &c, hub_id, &[TYPE_SOVEREIGNTY_HUB], "Sovereignty hub")?;
                    let mut b = pb::message(
                        "eve_public.sovereignty.hub.workforce.api.GetNetworkableHubsResponse",
                    );
                    for (other, sys) in networkable_hubs(snap, &sde, hub_id) {
                        let m = mode(snap, other);
                        b = b.add_with("hubs", |h| {
                            h.set("hub.sequential", other as u64)
                                .set("system.sequential", sys)
                                .set_with("configuration", |cfg| fill_configuration(cfg, &m))
                                .set_with("state", |st| fill_state(st, snap, &sde, other))
                        });
                    }
                    b.to_any_or_internal_err()
                })
            }),
        );
    }
}

#[cfg(test)]
#[path = "quasar_sov_workforce_tests.rs"]
mod quasar_sov_workforce_tests;

#[cfg(test)]
#[path = "quasar_sov_workforce_edges_tests.rs"]
mod quasar_sov_workforce_edges_tests;

