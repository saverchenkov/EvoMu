//! `quasar_sov_hub` — Equinox Sovereignty Hub state model and Quasar handlers (`Q-8`).
//!
//! Backs `eve_public.sovereignty.hub.api`, `eve_public.sovereignty.hub.fuel.api` and
//! `eve_public.sovereignty.hub.upgrade.api` with persisted [`PersistedSovHubData`]
//! (`WorldStateSnapshot.sov_runtime.sov_hubs`) plus SDE star / planet / upgrade-type lookups.
//! Workforce transfer handlers live in `quasar_sov_workforce`.
//!
//! Every `ResourcesSimulatedNotice`, `HubUpgradesConfiguredNotice`, `HubUpgradesSimulatedNotice`
//! and `UninstalledNotice` is emitted from a server-side state transition (configuration change,
//! fuel delivery or hourly fuel-burn simulation), never from a free-standing helper.

#![forbid(unsafe_code)]

use super::quasar_space_services::{effective_roles, get_identifier_or_int, get_int_field};
use super::sov_services::{TYPE_MAGMATIC_GAS, TYPE_SOVEREIGNTY_HUB, TYPE_SUPERIONIC_ICE};
use crate::quasar_gateway::{
    NoticeScope, PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_INTERNAL_ERROR,
    STATUS_NOT_FOUND, STATUS_UNAUTHORIZED,
};
use eve_db::{
    corp_roles, FromWalError, InventoryItemRecord, PersistedHubUpgrade, PersistedSovHubData,
    UpwellStructureRecord, WorldRepository, WorldStateSnapshot,
};
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, AuthoritativeContext,
    DynamicQuasarMessage, DynamicValue, GoogleAny, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_sde::SdeRegistry;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, RwLock};

pub(crate) type PendingNotices = Arc<RwLock<Vec<(NoticeScope, GoogleAny)>>>;
pub(crate) type RpcError = (u32, String);

/// SDE groups of Equinox Sovereignty Hub upgrades (category 39 "Infrastructure Upgrades"):
/// Site Detection, Service Infrastructure, Colony Resources Management, System Effect Generator.
pub const SOV_HUB_UPGRADE_GROUPS: [u32; 4] = [4768, 4772, 4838, 4839];
const GROUP_SUN: u32 = 6;
const GROUP_PLANET: u32 = 7;
/// Corporation roles allowed to operate a Sovereignty Hub.
pub const HUB_MANAGER_ROLES: u64 = corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_STATION_MANAGER;

pub(crate) fn any(name: &str, value: Vec<u8>) -> GoogleAny {
    GoogleAny { type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{name}"), value }
}

/// Encodes a `{ sequential = 1 }` identifier sub-message at `field`.
pub(crate) fn id_field(field: u32, id: u64, out: &mut Vec<u8>) {
    let mut buf = Vec::new();
    encode_varint_field(1, id, &mut buf);
    encode_bytes_field(field, &buf, out);
}

/// Encodes a `google.protobuf.Timestamp` sub-message at `field`.
pub(crate) fn ts_field(field: u32, seconds: u64, out: &mut Vec<u8>) {
    let mut buf = Vec::new();
    encode_varint_field(1, seconds, &mut buf);
    encode_bytes_field(field, &buf, out);
}

pub(crate) fn push_notice(pending: &PendingNotices, sys: u32, name: &str, value: Vec<u8>) {
    pending
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .push((NoticeScope::SolarSystem(sys), any(name, value)));
}

pub(crate) fn required_id(msg: &DynamicQuasarMessage, field: u32, what: &str) -> Result<i64, RpcError> {
    get_identifier_or_int(msg, field)
        .filter(|v| *v > 0)
        .ok_or((STATUS_BAD_REQUEST, format!("{what} identifier is required")))
}

/// Error wrapper so gateway `(status, message)` errors can drive
/// `WorldRepository::try_with_mut_snapshot`, which rolls back every in-memory mutation on `Err`.
pub(crate) struct TxError(u32, String);

impl FromWalError for TxError {
    fn from_wal_error(err: String) -> Self {
        TxError(STATUS_INTERNAL_ERROR, err)
    }
}

/// Runs a fallible world mutation transactionally (`S-8`): committed only on `Ok`, rolled back on `Err`.
pub(crate) fn transact<T>(
    db: &WorldRepository,
    f: impl FnOnce(&mut WorldStateSnapshot) -> Result<T, RpcError>,
) -> Result<T, RpcError> {
    db.try_with_mut_snapshot(|snap| f(snap).map_err(|(code, msg)| TxError(code, msg)))
        .map_err(|TxError(code, msg)| (code, msg))
}

// ---------------------------------------------------------------------------------------------
// Caller identity, ownership and role checks
// ---------------------------------------------------------------------------------------------

/// Authenticated caller resolved from the authoritative context and world state.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Caller {
    pub cid: i64,
    pub corp: i64,
    pub alliance: Option<i64>,
}

pub(crate) fn corp_alliance(snap: &WorldStateSnapshot, corp_id: i64) -> Option<i64> {
    snap.corporations.get(&corp_id).and_then(|c| c.alliance_id)
}

pub(crate) fn caller(ctx: &AuthoritativeContext, snap: &WorldStateSnapshot) -> Result<Caller, RpcError> {
    let cid = ctx
        .effective_character_id()
        .ok_or((STATUS_UNAUTHORIZED, "Authenticated character required".to_string()))? as i64;
    let ch = snap.characters.get(&cid);
    let corp = ctx
        .effective_corporation_id()
        .map(i64::from)
        .or_else(|| ch.map(|c| c.corporation_id))
        .filter(|c| *c > 0)
        .ok_or((STATUS_FORBIDDEN, format!("Character {cid} has no corporation")))?;
    let alliance = ch.and_then(|c| c.alliance_id).or_else(|| corp_alliance(snap, corp));
    Ok(Caller { cid, corp, alliance })
}

/// Returns `true` when `corp_id` is the caller's corporation or belongs to the caller's alliance.
pub(crate) fn same_owner(snap: &WorldStateSnapshot, caller: &Caller, corp_id: i64) -> bool {
    corp_id == caller.corp || (caller.alliance.is_some() && caller.alliance == corp_alliance(snap, corp_id))
}

/// Resolves a structure of one of `types` owned by the caller's corporation or alliance.
pub(crate) fn owned_structure<'a>(
    snap: &'a WorldStateSnapshot,
    caller: &Caller,
    structure_id: i64,
    types: &[u32],
    what: &str,
) -> Result<&'a UpwellStructureRecord, RpcError> {
    let st = snap
        .structures
        .get(&structure_id)
        .filter(|s| types.contains(&s.type_id))
        .ok_or((STATUS_NOT_FOUND, format!("{what} {structure_id} not found")))?;
    if !same_owner(snap, caller, st.owner_corp_id) {
        return Err((
            STATUS_FORBIDDEN,
            format!("{what} {structure_id} is not owned by your corporation or alliance"),
        ));
    }
    Ok(st)
}

/// Requires at least one of the corporation `roles` bits in the caller's active identity.
pub(crate) fn require_roles(ctx: &AuthoritativeContext, roles: u64, action: &str) -> Result<(), RpcError> {
    if effective_roles(ctx) & roles == 0 {
        return Err((STATUS_FORBIDDEN, format!("Missing corporation role required to {action}")));
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Static data: upgrade definitions, star power and planet yields
// ---------------------------------------------------------------------------------------------

/// Server-side definition of one Sovereignty Hub upgrade type (`hub.upgrade.Definition`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpgradeDefinition {
    pub type_id: u32,
    pub power_required: u64,
    pub workforce_required: u64,
    pub fuel_type: u32,
    pub fuel_per_hour: u32,
    pub fuel_startup: u32,
    pub exclusive_group: String,
    pub power_produced: u64,
    pub workforce_produced: u64,
}

fn split_tier(name: &str) -> (&str, u64) {
    match name.rsplit_once(' ') {
        Some((base, t)) if !t.is_empty() && t.chars().all(|c| c.is_ascii_digit()) => {
            (base, t.parse().unwrap_or(1))
        }
        _ => (name, 1),
    }
}

/// Builds the definition of an SDE Sovereignty Hub upgrade type.
///
/// Installation types, tiers (name suffix) and production lines come from SDE `invTypes`.
/// DERIVED (synthetic — the client `sovereigntyupgrades` FSD table with per-type power /
/// workforce / fuel numbers is not part of the EvoMu SDE import): costs follow a per-group,
/// per-tier server balance table.
pub fn upgrade_definition(sde: &SdeRegistry, type_id: u32) -> Option<UpgradeDefinition> {
    let t = sde.types.get(&type_id)?;
    if !t.published || !SOV_HUB_UPGRADE_GROUPS.contains(&t.group_id) {
        return None;
    }
    let (base, tier) = split_tier(&t.name);
    let mut d = UpgradeDefinition {
        type_id,
        power_required: 0,
        workforce_required: 0,
        fuel_type: TYPE_MAGMATIC_GAS,
        fuel_per_hour: (5 * tier) as u32,
        fuel_startup: (50 * tier) as u32,
        exclusive_group: base.to_string(),
        power_produced: 0,
        workforce_produced: 0,
    };
    match t.group_id {
        4772 => {
            d.power_required = 250;
            d.workforce_required = 3_000;
            d.fuel_type = TYPE_SUPERIONIC_ICE;
            d.fuel_per_hour = 10;
            d.fuel_startup = 100;
        }
        4839 => {
            d.power_required = 400;
            d.workforce_required = 4_000;
            d.fuel_per_hour = 15;
            d.fuel_startup = 200;
            d.exclusive_group = "SystemEffectGenerator".into();
        }
        4838 if base.starts_with("Power") => {
            d.workforce_required = 500 * tier;
            d.power_produced = 250 * tier;
            d.fuel_type = TYPE_SUPERIONIC_ICE;
        }
        4838 => {
            d.power_required = 200 * tier;
            d.workforce_produced = 2_000 * tier;
            d.fuel_type = TYPE_SUPERIONIC_ICE;
        }
        _ => {
            d.power_required = 200 * tier;
            d.workforce_required = 1_000 * tier;
        }
    }
    Some(d)
}

/// All published SDE Sovereignty Hub upgrade definitions, ordered by type id.
pub fn all_upgrade_definitions(sde: &SdeRegistry) -> Vec<UpgradeDefinition> {
    let mut ids: Vec<u32> = sde
        .types
        .values()
        .filter(|t| t.published && SOV_HUB_UPGRADE_GROUPS.contains(&t.group_id))
        .map(|t| t.type_id)
        .collect();
    ids.sort_unstable();
    ids.into_iter().filter_map(|id| upgrade_definition(sde, id)).collect()
}

/// Item types accepted by `fuel.api.AddRequest` (fuel types consumed by hub upgrades).
pub fn is_hub_fuel_type(type_id: u32) -> bool {
    type_id == TYPE_MAGMATIC_GAS || type_id == TYPE_SUPERIONIC_ICE
}

/// Power output of a star by spectral class of its SDE sun type (`Sun G5 (Yellow)` -> `G`).
///
/// DERIVED (synthetic — the client `sovereignty resource star configuration` table is not part of
/// the EvoMu SDE import): hotter spectral classes yield more power, 800 (O) down to 200 (M).
pub fn star_power(sde: &SdeRegistry, star_type_id: u32) -> u64 {
    let class = sde
        .types
        .get(&star_type_id)
        .and_then(|t| t.name.strip_prefix("Sun ").and_then(|r| r.chars().next()));
    match class {
        Some('O') => 800,
        Some('B') => 700,
        Some('A') => 600,
        Some('F') => 500,
        Some('G') => 400,
        Some('K') => 300,
        Some('M') => 200,
        _ => 300,
    }
}

/// Single shared planet resource table `(SDE planet type id, name, power, workforce)` used by both
/// `resource.planet.api.GetAllDefinitionsRequest` and the hub resource computation.
///
/// DERIVED (synthetic — the client `sovereignty resource planet definitions` table is not part of
/// the EvoMu SDE import): Temperate / Oceanic / Barren planets yield workforce, Gas / Storm /
/// Plasma planets yield power, Ice / Lava planets yield colony reagents instead.
pub const PLANET_RESOURCE_TABLE: [(u32, &str, u64, u64); 8] = [
    (11, "Temperate", 0, 1_500),
    (2014, "Oceanic", 0, 1_000),
    (2016, "Barren", 0, 500),
    (13, "Gas", 150, 0),
    (2017, "Storm", 100, 0),
    (2063, "Plasma", 200, 0),
    (12, "Ice", 0, 0),
    (2015, "Lava", 0, 0),
];

/// `(power, workforce)` harvested from a planet of the given SDE planet type
/// (looked up in the DERIVED [`PLANET_RESOURCE_TABLE`]).
pub fn planet_yield(planet_type_id: u32) -> (u64, u64) {
    PLANET_RESOURCE_TABLE
        .iter()
        .find(|row| row.0 == planet_type_id)
        .map_or((0, 0), |row| (row.2, row.3))
}

/// Resource inputs of a solar system: stars (`group_id == 6`) and planets (`group_id == 7`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SystemYield {
    pub stars: Vec<(i64, u64)>,
    pub planet_power: u64,
    pub planet_workforce: u64,
}

pub fn system_yield(sde: &SdeRegistry, sys_id: u32) -> SystemYield {
    let mut y = SystemYield::default();
    for c in sde.celestials_in_system(sys_id) {
        if c.group_id == GROUP_SUN {
            y.stars.push((c.celestial_id, star_power(sde, c.type_id)));
        } else if c.group_id == GROUP_PLANET {
            let (p, w) = planet_yield(c.type_id);
            y.planet_power += p;
            y.planet_workforce += w;
        }
    }
    y
}

// ---------------------------------------------------------------------------------------------
// Hub resources, simulation and notice diffing
// ---------------------------------------------------------------------------------------------

/// Power / workforce budget of a hub (`GetResourcesResponse` / `ResourcesSimulatedNotice`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HubResources {
    pub power_available: u64,
    pub power_allocated: u64,
    pub power_local: u64,
    pub workforce_available: u64,
    pub workforce_allocated: u64,
    pub workforce_local: u64,
}

fn is_active(u: &PersistedHubUpgrade) -> bool {
    u.online && !u.low_power
}

/// Computes hub resources from the SDE system yield, active upgrades and workforce transfers.
pub fn compute_hub_resources(snap: &WorldStateSnapshot, sde: &SdeRegistry, hub_id: i64) -> Option<HubResources> {
    let st = snap.structures.get(&hub_id)?;
    let y = system_yield(sde, st.solar_system_id);
    // DERIVED (synthetic): Mercenary Den anarchy stage 2..=3 siphons 500 * (stage - 1) workforce/h; stage >= 4 freezes local workforce transport.
    let mut siphoned_w: u64 = 0;
    let mut any_workforce_frozen = false;
    for d in snap.sov_runtime.mercenary_dens.values().filter(|d| d.solar_system_id == st.solar_system_id && d.enabled) {
        let stage = u64::from(super::quasar_sov_mercden::stage_for_level(d.anarchy_level));
        if stage >= 4 {
            any_workforce_frozen = true;
        } else if stage >= 2 {
            siphoned_w += 500 * (stage - 1);
        }
    }
    let effective_workforce = if any_workforce_frozen {
        0
    } else {
        y.planet_workforce.saturating_sub(siphoned_w)
    };
    let mut r = HubResources {
        power_local: y.stars.iter().map(|s| s.1).sum::<u64>() + y.planet_power,
        workforce_local: effective_workforce,
        ..Default::default()
    };
    let (mut produced_p, mut produced_w) = (0u64, 0u64);
    if let Some(h) = snap.sov_runtime.sov_hubs.get(&hub_id) {
        for d in h.upgrades.iter().filter(|u| is_active(u)).filter_map(|u| upgrade_definition(sde, u.type_id)) {
            r.power_allocated += d.power_required;
            r.workforce_allocated += d.workforce_required;
            produced_p += d.power_produced;
            produced_w += d.workforce_produced;
        }
    }
    let imported: u64 = super::quasar_sov_workforce::imports(snap, hub_id).iter().map(|s| s.1).sum();
    let exported = super::quasar_sov_workforce::exported_amount(snap, hub_id);
    r.power_available = r.power_local + produced_p;
    r.workforce_available = (r.workforce_local + produced_w + imported).saturating_sub(exported);
    Some(r)
}

/// Takes consumer upgrades into `low_power = true` if power or workforce is overloaded
/// (e.g. when a Power Monitoring upgrade runs out of fuel or anarchy siphons workforce).
pub fn enforce_hub_power_overload(snap: &mut WorldStateSnapshot, sde: &SdeRegistry, hub_id: i64) -> bool {
    let mut changed = false;
    loop {
        let Some(r) = compute_hub_resources(snap, sde, hub_id) else {
            break;
        };
        if r.power_allocated <= r.power_available && r.workforce_allocated <= r.workforce_available {
            break;
        }
        let Some(h) = snap.sov_runtime.sov_hubs.get_mut(&hub_id) else {
            break;
        };
        if let Some(u) = h.upgrades.iter_mut().rev().find(|u| {
            is_active(u)
                && upgrade_definition(sde, u.type_id)
                    .is_some_and(|d| d.power_required > 0 || d.workforce_required > 0)
        }) {
            u.low_power = true;
            changed = true;
        } else {
            break;
        }
    }
    changed
}

/// Observable per-hub state used to diff before/after a transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HubView {
    pub sys: u32,
    pub resources: HubResources,
    pub state: Vec<u8>,
}

/// Snapshots every hub that has Equinox runtime state, plus `extra`.
pub(crate) fn hub_views(snap: &WorldStateSnapshot, sde: &SdeRegistry, extra: &[i64]) -> BTreeMap<i64, HubView> {
    let ids: BTreeSet<i64> = snap.sov_runtime.sov_hubs.keys().copied().chain(extra.iter().copied()).collect();
    ids.into_iter()
        .filter_map(|id| {
            let sys = snap.structures.get(&id)?.solar_system_id;
            Some((id, HubView {
                sys,
                resources: compute_hub_resources(snap, sde, id)?,
                state: super::quasar_sov_workforce::encode_state(snap, id),
            }))
        })
        .collect()
}

pub(crate) fn encode_resource_pair(available: u64, allocated: u64, local: u64) -> Vec<u8> {
    let mut b = Vec::new();
    encode_varint_field(1, available, &mut b);
    encode_varint_field(2, allocated, &mut b);
    encode_varint_field(3, local, &mut b);
    b
}

/// Emits `ResourcesSimulatedNotice` / workforce `StateChangedNotice` for every hub whose view changed.
pub(crate) fn emit_hub_diffs(pending: &PendingNotices, before: &BTreeMap<i64, HubView>, after: &BTreeMap<i64, HubView>) {
    for (hub_id, now) in after {
        let prev = before.get(hub_id);
        if prev.map(|p| p.resources) != Some(now.resources) {
            let r = now.resources;
            let mut n = Vec::new();
            id_field(1, *hub_id as u64, &mut n);
            id_field(2, now.sys as u64, &mut n);
            encode_bytes_field(3, &encode_resource_pair(r.power_available, r.power_allocated, r.power_local), &mut n);
            encode_bytes_field(4, &encode_resource_pair(r.workforce_available, r.workforce_allocated, r.workforce_local), &mut n);
            push_notice(pending, now.sys, "eve_public.sovereignty.hub.api.ResourcesSimulatedNotice", n);
        }
        if let Some(p) = prev {
            if p.state != now.state {
                let mut n = Vec::new();
                id_field(1, now.sys as u64, &mut n);
                encode_bytes_field(2, &p.state, &mut n);
                encode_bytes_field(3, &now.state, &mut n);
                id_field(4, *hub_id as u64, &mut n);
                push_notice(pending, now.sys, "eve_public.sovereignty.resource.transfer.workforce.api.StateChangedNotice", n);
            }
        }
    }
}

/// Burns upgrade fuel for every whole hour elapsed since `last_updated_unix`.
/// Returns `true` when an upgrade ran out of fuel and dropped to `POWER_STATE_LOW`.
pub fn simulate_fuel(sde: &SdeRegistry, hub: &mut PersistedSovHubData, now: u64) -> bool {
    if hub.last_updated_unix == 0 || now <= hub.last_updated_unix {
        if hub.last_updated_unix == 0 {
            hub.last_updated_unix = now;
        }
        return false;
    }
    let hours = (now - hub.last_updated_unix) / 3600;
    if hours == 0 {
        return false;
    }
    hub.last_updated_unix += hours * 3600;
    let mut changed = false;
    for u in hub.upgrades.iter_mut().filter(|u| is_active(u)) {
        let Some(d) = upgrade_definition(sde, u.type_id) else { continue };
        let need = u64::from(d.fuel_per_hour) * hours;
        let have = hub.fuel.entry(d.fuel_type).or_insert(0);
        if u64::from(*have) >= need {
            *have -= need as u32;
        } else {
            if d.fuel_per_hour > 0 {
                *have %= d.fuel_per_hour;
            }
            u.low_power = true;
            changed = true;
        }
    }
    hub.fuel.retain(|_, v| *v > 0);
    changed
}

/// Runs the hourly fuel simulation for `hub_id`; on a power-state transition emits
/// `HubUpgradesSimulatedNotice` plus resource diffs.
pub(crate) fn run_simulation(snap: &mut WorldStateSnapshot, sde: &SdeRegistry, pending: &PendingNotices, hub_id: i64, now: u64) {
    let Some(sys) = snap.structures.get(&hub_id).map(|s| s.solar_system_id) else { return };
    let before = hub_views(snap, sde, &[hub_id]);
    let hub = snap
        .sov_runtime
        .sov_hubs
        .entry(hub_id)
        .or_insert_with(|| PersistedSovHubData { hub_id, ..Default::default() });
    if !simulate_fuel(sde, hub, now) {
        return;
    }
    enforce_hub_power_overload(snap, sde, hub_id);
    let mut n = Vec::new();
    encode_bytes_field(1, &encode_hub_upgrades(sde, hub_id, snap.sov_runtime.sov_hubs.get(&hub_id)), &mut n);
    push_notice(pending, sys, "eve_public.sovereignty.hub.upgrade.api.HubUpgradesSimulatedNotice", n);
    let after = hub_views(snap, sde, &[hub_id]);
    emit_hub_diffs(pending, &before, &after);
}

// ---------------------------------------------------------------------------------------------
// Upgrade encoders
// ---------------------------------------------------------------------------------------------

pub(crate) fn encode_definition(d: &UpgradeDefinition) -> Vec<u8> {
    let mut b = Vec::new();
    id_field(1, d.type_id as u64, &mut b);
    encode_varint_field(2, d.power_required, &mut b);
    encode_varint_field(3, d.workforce_required, &mut b);
    id_field(4, d.fuel_type as u64, &mut b);
    encode_varint_field(5, d.fuel_per_hour as u64, &mut b);
    encode_varint_field(6, d.fuel_startup as u64, &mut b);
    encode_string_field(7, &d.exclusive_group, &mut b);
    encode_varint_field(8, d.power_produced, &mut b);
    encode_varint_field(9, d.workforce_produced, &mut b);
    b
}

fn power_state(u: &PersistedHubUpgrade) -> u64 {
    match (u.online, u.low_power) {
        (false, _) => 1,    // POWER_STATE_OFFLINE
        (true, false) => 2, // POWER_STATE_ONLINE
        (true, true) => 3,  // POWER_STATE_LOW
    }
}

fn encode_upgrade_identifier(hub_id: i64, type_id: u32) -> Vec<u8> {
    let mut b = Vec::new();
    id_field(1, hub_id as u64, &mut b);
    id_field(2, type_id as u64, &mut b);
    id_field(3, type_id as u64, &mut b);
    b
}

fn encode_upgrade_attributes(sde: &SdeRegistry, u: &PersistedHubUpgrade) -> Vec<u8> {
    let mut b = Vec::new();
    if let Some(d) = upgrade_definition(sde, u.type_id) {
        encode_bytes_field(1, &encode_definition(&d), &mut b);
    }
    encode_varint_field(2, power_state(u), &mut b);
    b
}

/// Encodes `eve_public.sovereignty.hub.upgrade.HubUpgrades` from persisted hub state.
pub(crate) fn encode_hub_upgrades(sde: &SdeRegistry, hub_id: i64, hub: Option<&PersistedSovHubData>) -> Vec<u8> {
    let mut b = Vec::new();
    id_field(1, hub_id as u64, &mut b);
    if let Some(h) = hub {
        for u in &h.upgrades {
            let mut up = Vec::new();
            encode_bytes_field(1, &encode_upgrade_identifier(hub_id, u.type_id), &mut up);
            encode_bytes_field(2, &encode_upgrade_attributes(sde, u), &mut up);
            encode_bytes_field(2, &up, &mut b);
        }
        ts_field(3, h.last_updated_unix, &mut b);
    }
    b
}

/// Reads `(hub_id, type_id)` from an `eve_public.sovereignty.hub.upgrade.Identifier` at `field`.
fn upgrade_identifier(msg: &DynamicQuasarMessage, field: u32) -> Result<(i64, u32), RpcError> {
    let Some(DynamicValue::Message(id)) = msg.get(field) else {
        return Err((STATUS_BAD_REQUEST, "Upgrade identifier is required".into()));
    };
    let hub_id = required_id(id, 1, "Hub")?;
    let type_id = get_identifier_or_int(id, 2)
        .or_else(|| get_identifier_or_int(id, 3))
        .filter(|v| *v > 0)
        .ok_or((STATUS_BAD_REQUEST, "Upgrade installation type is required".to_string()))?;
    Ok((hub_id, type_id as u32))
}

fn repeated(msg: &DynamicQuasarMessage, field: u32) -> Vec<&DynamicQuasarMessage> {
    match msg.get(field) {
        Some(DynamicValue::Repeated(items)) => items
            .iter()
            .filter_map(|v| match v {
                DynamicValue::Message(m) => Some(m.as_ref()),
                _ => None,
            })
            .collect(),
        Some(DynamicValue::Message(m)) => vec![m.as_ref()],
        _ => Vec::new(),
    }
}

/// Validates that the hub's active upgrades fit its power / workforce budget.
fn check_budget(snap: &WorldStateSnapshot, sde: &SdeRegistry, hub_id: i64) -> Result<(), RpcError> {
    let r = compute_hub_resources(snap, sde, hub_id).ok_or((STATUS_NOT_FOUND, format!("Sovereignty hub {hub_id} not found")))?;
    if r.power_allocated > r.power_available {
        return Err((STATUS_BAD_REQUEST, format!("Insufficient power: {} required, {} available", r.power_allocated, r.power_available)));
    }
    if r.workforce_allocated > r.workforce_available {
        return Err((STATUS_BAD_REQUEST, format!("Insufficient workforce: {} required, {} available", r.workforce_allocated, r.workforce_available)));
    }
    Ok(())
}

/// Applies `ProcessConfigurationRequest` to the world: installs upgrade items, toggles online
/// state (charging fuel startup cost), validates budgets, and returns the hub's solar system.
fn process_configuration(
    snap: &mut WorldStateSnapshot,
    sde: &SdeRegistry,
    caller: &Caller,
    hub_id: i64,
    new_items: &[i64],
    config: &[(u32, bool)],
    now: u64,
) -> Result<u32, RpcError> {
    let sys = owned_structure(snap, caller, hub_id, &[TYPE_SOVEREIGNTY_HUB], "Sovereignty hub")?.solar_system_id;
    let original = snap.clone_sov_hub(hub_id);
    let mut hub = original.clone();
    let mut consumed = Vec::new();
    for item_id in new_items {
        let it = snap
            .items
            .get(item_id)
            .filter(|it| it.owner_id == caller.cid || (caller.corp > 0 && it.owner_id == caller.corp))
            .ok_or((STATUS_NOT_FOUND, format!("Upgrade item {item_id} not found in your inventory")))?;
        if !item_at_hub(snap, caller, it, hub_id, sys) {
            return Err((STATUS_BAD_REQUEST, format!("Upgrade item {item_id} is not inside the hub or your ship at the hub")));
        }
        let def = upgrade_definition(sde, it.type_id)
            .ok_or((STATUS_BAD_REQUEST, format!("Item type {} is not a sovereignty hub upgrade", it.type_id)))?;
        if hub.upgrades.iter().filter_map(|u| upgrade_definition(sde, u.type_id)).any(|d| d.exclusive_group == def.exclusive_group) {
            return Err((STATUS_BAD_REQUEST, format!("Upgrade group '{}' is already installed", def.exclusive_group)));
        }
        hub.upgrades.push(PersistedHubUpgrade { type_id: def.type_id, online: false, low_power: false });
        consumed.push(*item_id);
    }
    for (type_id, online) in config {
        let u = hub
            .upgrades
            .iter_mut()
            .find(|u| u.type_id == *type_id)
            .ok_or((STATUS_NOT_FOUND, format!("Upgrade type {type_id} is not installed")))?;
        if *online && !u.online {
            let d = upgrade_definition(sde, *type_id).ok_or((STATUS_BAD_REQUEST, format!("Unknown upgrade {type_id}")))?;
            let have = hub.fuel.get(&d.fuel_type).copied().unwrap_or(0);
            if have < d.fuel_startup.max(d.fuel_per_hour) {
                return Err((STATUS_BAD_REQUEST, format!("Insufficient fuel type {} to online upgrade {type_id}", d.fuel_type)));
            }
            hub.fuel.insert(d.fuel_type, have - d.fuel_startup);
        }
        u.online = *online;
        // A starved upgrade only leaves POWER_STATE_LOW once the bay holds an hour of its fuel.
        let fueled = upgrade_definition(sde, *type_id)
            .is_some_and(|d| hub.fuel.get(&d.fuel_type).copied().unwrap_or(0) >= d.fuel_per_hour);
        if !*online || fueled {
            u.low_power = false;
        }
    }
    let is_hacked = snap.sov_runtime.hacked_hubs.get(&hub_id).copied().is_some_and(|until| now < until);
    if !is_hacked {
        for u in &mut hub.upgrades {
            if u.online && u.low_power {
                if let Some(d) = upgrade_definition(sde, u.type_id) {
                    if hub.fuel.get(&d.fuel_type).copied().unwrap_or(0) >= d.fuel_per_hour {
                        u.low_power = false;
                    }
                }
            }
        }
    }
    hub.fuel.retain(|_, v| *v > 0);
    // Keep the partial-hour remainder of the fuel-burn clock (`simulate_fuel`); only start it.
    if hub.last_updated_unix == 0 {
        hub.last_updated_unix = now;
    }
    snap.sov_runtime.sov_hubs.insert(hub_id, hub);
    if let Err(e) = check_budget(snap, sde, hub_id) {
        snap.sov_runtime.sov_hubs.insert(hub_id, original);
        return Err(e);
    }
    for item_id in consumed {
        consume_items(snap, item_id, 1);
    }
    Ok(sys)
}

/// `true` when an item may be consumed by a hub operation: it sits inside the hub itself, or in
/// the caller's active ship while the caller is in the hub's solar system.
pub(crate) fn item_at_hub(snap: &WorldStateSnapshot, caller: &Caller, it: &InventoryItemRecord, hub_id: i64, hub_sys: u32) -> bool {
    let valid_owner = if it.owner_id == caller.cid {
        matches!(
            it.flag,
            eve_db::InventoryFlag::Hangar
                | eve_db::InventoryFlag::Cargo
                | eve_db::InventoryFlag::SubSystemSlot0
        )
    } else if caller.corp > 0 && it.owner_id == caller.corp {
        crate::services::mining_advanced::mining_compression::caller_can_take_corp_hangar_item(
            snap,
            caller.cid,
            caller.corp,
            it.flag,
            0,
        )
    } else {
        false
    };
    valid_owner
        && it.quantity >= 1
        && it.flag != eve_db::InventoryFlag::Locked
        && !snap.is_item_locked_in_contract_or_job(it.item_id)
        && (it.location_id == hub_id
            || snap
                .characters
                .get(&caller.cid)
                .is_some_and(|ch| ch.active_ship_id == it.location_id && ch.solar_system_id == hub_sys))
}

/// Removes `qty` units from an inventory stack, deleting it when empty.
pub(crate) fn consume_items(snap: &mut WorldStateSnapshot, item_id: i64, qty: i32) {
    let empty = match snap.items.get_mut(&item_id) {
        Some(it) => {
            it.quantity -= qty;
            it.version += 1;
            it.quantity <= 0
        }
        None => false,
    };
    if empty {
        snap.items.remove(&item_id);
    }
}

/// Extension used to clone (or default) the runtime record of a hub.
trait SovHubSnapshotExt {
    fn clone_sov_hub(&self, hub_id: i64) -> PersistedSovHubData;
}

impl SovHubSnapshotExt for WorldStateSnapshot {
    fn clone_sov_hub(&self, hub_id: i64) -> PersistedSovHubData {
        self.sov_runtime
            .sov_hubs
            .get(&hub_id)
            .cloned()
            .unwrap_or_else(|| PersistedSovHubData { hub_id, ..Default::default() })
    }
}

// ---------------------------------------------------------------------------------------------
// Handler registration
// ---------------------------------------------------------------------------------------------

/// Registers `hub.api`, `hub.fuel.api` and `hub.upgrade.api` handlers.
pub fn register_handlers(router: &PublicGatewayRouter, db: Arc<WorldRepository>, sde: Arc<SdeRegistry>) {
    // eve_public.sovereignty.hub.api.GetResourcesRequest
    {
        let (db, sde, pending) = (Arc::clone(&db), Arc::clone(&sde), Arc::clone(&router.pending_notices));
        router.register_handler(
            "eve_public.sovereignty.hub.api.GetResourcesRequest",
            Arc::new(move |ctx, _any, dyn_msg| {
                let hub_id = required_id(dyn_msg, 1, "Hub")?;
                let r = transact(&db, |snap| {
                    let c = caller(ctx, snap)?;
                    owned_structure(snap, &c, hub_id, &[TYPE_SOVEREIGNTY_HUB], "Sovereignty hub")?;
                    run_simulation(snap, &sde, &pending, hub_id, ctx.now_unix());
                    compute_hub_resources(snap, &sde, hub_id).ok_or((STATUS_NOT_FOUND, "hub".to_string()))
                })?;
                let mut out = Vec::new();
                encode_bytes_field(1, &encode_resource_pair(r.power_available, r.power_allocated, r.power_local), &mut out);
                encode_bytes_field(2, &encode_resource_pair(r.workforce_available, r.workforce_allocated, r.workforce_local), &mut out);
                Ok(any("eve_public.sovereignty.hub.api.GetResourcesResponse", out))
            }),
        );
    }

    // eve_public.sovereignty.hub.fuel.api.GetRequest
    {
        let (db, sde, pending) = (Arc::clone(&db), Arc::clone(&sde), Arc::clone(&router.pending_notices));
        router.register_handler(
            "eve_public.sovereignty.hub.fuel.api.GetRequest",
            Arc::new(move |ctx, _any, dyn_msg| {
                let hub_id = required_id(dyn_msg, 1, "Hub")?;
                let hub = transact(&db, |snap| {
                    let c = caller(ctx, snap)?;
                    owned_structure(snap, &c, hub_id, &[TYPE_SOVEREIGNTY_HUB], "Sovereignty hub")?;
                    run_simulation(snap, &sde, &pending, hub_id, ctx.now_unix());
                    Ok::<_, RpcError>(snap.clone_sov_hub(hub_id))
                })?;
                let mut burn: BTreeMap<u32, u64> = hub.fuel.keys().map(|k| (*k, 0)).collect();
                for d in hub.upgrades.iter().filter(|u| is_active(u)).filter_map(|u| upgrade_definition(&sde, u.type_id)) {
                    *burn.entry(d.fuel_type).or_insert(0) += u64::from(d.fuel_per_hour);
                }
                let mut out = Vec::new();
                for (fuel_type, per_hour) in burn {
                    let mut f = Vec::new();
                    id_field(1, fuel_type as u64, &mut f);
                    encode_varint_field(2, u64::from(hub.fuel.get(&fuel_type).copied().unwrap_or(0)), &mut f);
                    encode_varint_field(3, per_hour, &mut f);
                    encode_bytes_field(1, &f, &mut out);
                }
                ts_field(2, hub.last_updated_unix, &mut out);
                Ok(any("eve_public.sovereignty.hub.fuel.api.GetResponse", out))
            }),
        );
    }

    // eve_public.sovereignty.hub.fuel.api.AddRequest
    {
        let (db, sde, pending) = (Arc::clone(&db), Arc::clone(&sde), Arc::clone(&router.pending_notices));
        router.register_handler(
            "eve_public.sovereignty.hub.fuel.api.AddRequest",
            Arc::new(move |ctx, _any, dyn_msg| {
                let item_id = required_id(dyn_msg, 1, "Fuel item")?;
                let amount = get_int_field(dyn_msg, 2).unwrap_or(0);
                let hub_id = required_id(dyn_msg, 3, "Hub")?;
                if amount <= 0 || amount > i64::from(i32::MAX) {
                    return Err((STATUS_BAD_REQUEST, "Fuel amount must be positive".into()));
                }
                transact(&db, |snap| {
                    let c = caller(ctx, snap)?;
                    require_roles(ctx, HUB_MANAGER_ROLES, "add sovereignty hub fuel")?;
                    let sys = owned_structure(snap, &c, hub_id, &[TYPE_SOVEREIGNTY_HUB], "Sovereignty hub")?.solar_system_id;
                    let it = snap
                        .items
                        .get(&item_id)
                        .filter(|it| it.owner_id == c.cid || (c.corp > 0 && it.owner_id == c.corp))
                        .ok_or((STATUS_NOT_FOUND, format!("Fuel item {item_id} not found in your inventory")))?;
                    if !item_at_hub(snap, &c, it, hub_id, sys) {
                        return Err((STATUS_BAD_REQUEST, format!("Fuel item {item_id} is not inside the hub or your ship at the hub")));
                    }
                    let fuel_type = it.type_id;
                    if !is_hub_fuel_type(fuel_type) {
                        return Err((STATUS_BAD_REQUEST, format!("Item type {fuel_type} is not sovereignty hub fuel")));
                    }
                    if i64::from(it.quantity) < amount {
                        return Err((STATUS_BAD_REQUEST, format!("Insufficient fuel: have {}, requested {amount}", it.quantity)));
                    }
                    run_simulation(snap, &sde, &pending, hub_id, ctx.now_unix());
                    let before = hub_views(snap, &sde, &[hub_id]);
                    let mut hub = snap.clone_sov_hub(hub_id);
                    let total = hub
                        .fuel
                        .get(&fuel_type)
                        .copied()
                        .unwrap_or(0)
                        .checked_add(amount as u32)
                        .ok_or((STATUS_BAD_REQUEST, "Hub fuel bay capacity exceeded".to_string()))?;
                    hub.fuel.insert(fuel_type, total);
                    consume_items(snap, item_id, amount as i32);
                    let mut restored = false;
                    for u in hub.upgrades.iter_mut().filter(|u| u.online && u.low_power) {
                        if upgrade_definition(&sde, u.type_id).is_some_and(|d| d.fuel_type == fuel_type && total >= d.fuel_per_hour) {
                            u.low_power = false;
                            restored = true;
                        }
                    }
                    if hub.last_updated_unix == 0 {
                        hub.last_updated_unix = ctx.now_unix();
                    }
                    snap.sov_runtime.sov_hubs.insert(hub_id, hub);
                    if restored {
                        let mut n = Vec::new();
                        encode_bytes_field(1, &encode_hub_upgrades(&sde, hub_id, snap.sov_runtime.sov_hubs.get(&hub_id)), &mut n);
                        push_notice(&pending, sys, "eve_public.sovereignty.hub.upgrade.api.HubUpgradesSimulatedNotice", n);
                    }
                    emit_hub_diffs(&pending, &before, &hub_views(snap, &sde, &[hub_id]));
                    Ok(())
                })?;
                Ok(any("eve_public.sovereignty.hub.fuel.api.AddResponse", Vec::new()))
            }),
        );
    }

    // eve_public.sovereignty.hub.upgrade.api.GetDefinitionsRequest
    {
        let sde = Arc::clone(&sde);
        router.register_handler(
            "eve_public.sovereignty.hub.upgrade.api.GetDefinitionsRequest",
            Arc::new(move |_ctx, _any, _dyn_msg| {
                let mut out = Vec::new();
                for d in all_upgrade_definitions(&sde) {
                    encode_bytes_field(1, &encode_definition(&d), &mut out);
                }
                Ok(any("eve_public.sovereignty.hub.upgrade.api.GetDefinitionsResponse", out))
            }),
        );
    }

    // eve_public.sovereignty.hub.upgrade.api.GetHubUpgradesRequest
    {
        let (db, sde, pending) = (Arc::clone(&db), Arc::clone(&sde), Arc::clone(&router.pending_notices));
        router.register_handler(
            "eve_public.sovereignty.hub.upgrade.api.GetHubUpgradesRequest",
            Arc::new(move |ctx, _any, dyn_msg| {
                let hub_id = required_id(dyn_msg, 1, "Hub")?;
                let hub = transact(&db, |snap| {
                    let c = caller(ctx, snap)?;
                    owned_structure(snap, &c, hub_id, &[TYPE_SOVEREIGNTY_HUB], "Sovereignty hub")?;
                    run_simulation(snap, &sde, &pending, hub_id, ctx.now_unix());
                    Ok::<_, RpcError>(snap.clone_sov_hub(hub_id))
                })?;
                let mut out = Vec::new();
                encode_bytes_field(1, &encode_hub_upgrades(&sde, hub_id, Some(&hub)), &mut out);
                Ok(any("eve_public.sovereignty.hub.upgrade.api.GetHubUpgradesResponse", out))
            }),
        );
    }

    // eve_public.sovereignty.hub.upgrade.api.GetRequest
    {
        let (db, sde) = (Arc::clone(&db), Arc::clone(&sde));
        router.register_handler(
            "eve_public.sovereignty.hub.upgrade.api.GetRequest",
            Arc::new(move |ctx, _any, dyn_msg| {
                let (hub_id, type_id) = upgrade_identifier(dyn_msg, 1)?;
                let up = db.with_snapshot(|snap| {
                    let c = caller(ctx, snap)?;
                    owned_structure(snap, &c, hub_id, &[TYPE_SOVEREIGNTY_HUB], "Sovereignty hub")?;
                    snap.sov_runtime
                        .sov_hubs
                        .get(&hub_id)
                        .and_then(|h| h.upgrades.iter().find(|u| u.type_id == type_id).cloned())
                        .ok_or((STATUS_NOT_FOUND, format!("Upgrade {type_id} is not installed in hub {hub_id}")))
                })?;
                let mut out = Vec::new();
                encode_bytes_field(1, &encode_upgrade_attributes(&sde, &up), &mut out);
                Ok(any("eve_public.sovereignty.hub.upgrade.api.GetResponse", out))
            }),
        );
    }

    // eve_public.sovereignty.hub.upgrade.api.ProcessConfigurationRequest
    {
        let (db, sde, pending) = (Arc::clone(&db), Arc::clone(&sde), Arc::clone(&router.pending_notices));
        router.register_handler(
            "eve_public.sovereignty.hub.upgrade.api.ProcessConfigurationRequest",
            Arc::new(move |ctx, _any, dyn_msg| {
                let hub_id = required_id(dyn_msg, 3, "Hub")?;
                let new_items: Vec<i64> = repeated(dyn_msg, 1).into_iter().filter_map(|m| get_int_field(m, 1)).collect();
                let mut config = Vec::new();
                for m in repeated(dyn_msg, 2) {
                    let type_id = required_id(m, 1, "Upgrade type")? as u32;
                    config.push((type_id, matches!(m.get(2), Some(DynamicValue::Bool(true)))));
                }
                let hub_upgrades = transact(&db, |snap| {
                    let c = caller(ctx, snap)?;
                    require_roles(ctx, HUB_MANAGER_ROLES, "configure sovereignty hub upgrades")?;
                    owned_structure(snap, &c, hub_id, &[TYPE_SOVEREIGNTY_HUB], "Sovereignty hub")?;
                    run_simulation(snap, &sde, &pending, hub_id, ctx.now_unix());
                    let before = hub_views(snap, &sde, &[hub_id]);
                    let sys = process_configuration(snap, &sde, &c, hub_id, &new_items, &config, ctx.now_unix())?;
                    let encoded = encode_hub_upgrades(&sde, hub_id, snap.sov_runtime.sov_hubs.get(&hub_id));
                    let mut n = Vec::new();
                    encode_bytes_field(1, &encoded, &mut n);
                    push_notice(&pending, sys, "eve_public.sovereignty.hub.upgrade.api.HubUpgradesConfiguredNotice", n);
                    emit_hub_diffs(&pending, &before, &hub_views(snap, &sde, &[hub_id]));
                    Ok::<_, RpcError>(encoded)
                })?;
                let mut out = Vec::new();
                encode_bytes_field(1, &hub_upgrades, &mut out);
                Ok(any("eve_public.sovereignty.hub.upgrade.api.ProcessConfigurationResponse", out))
            }),
        );
    }

    // eve_public.sovereignty.hub.upgrade.api.UninstallRequest
    {
        let (db, sde, pending) = (Arc::clone(&db), Arc::clone(&sde), Arc::clone(&router.pending_notices));
        router.register_handler(
            "eve_public.sovereignty.hub.upgrade.api.UninstallRequest",
            Arc::new(move |ctx, _any, dyn_msg| {
                let (hub_id, type_id) = upgrade_identifier(dyn_msg, 1)?;
                transact(&db, |snap| {
                    let c = caller(ctx, snap)?;
                    require_roles(ctx, HUB_MANAGER_ROLES, "uninstall sovereignty hub upgrades")?;
                    let st = owned_structure(snap, &c, hub_id, &[TYPE_SOVEREIGNTY_HUB], "Sovereignty hub")?;
                    if st.owner_corp_id != c.corp {
                        return Err((STATUS_FORBIDDEN, format!("Sovereignty hub {hub_id} is not owned by your corporation")));
                    }
                    let sys = st.solar_system_id;
                    run_simulation(snap, &sde, &pending, hub_id, ctx.now_unix());
                    let before = hub_views(snap, &sde, &[hub_id]);
                    let original = snap.clone_sov_hub(hub_id);
                    let mut hub = original.clone();
                    let pos = hub
                        .upgrades
                        .iter()
                        .position(|u| u.type_id == type_id)
                        .ok_or((STATUS_NOT_FOUND, format!("Upgrade {type_id} is not installed in hub {hub_id}")))?;
                    hub.upgrades.remove(pos);
                    snap.sov_runtime.sov_hubs.insert(hub_id, hub);
                    if let Err(e) = check_budget(snap, &sde, hub_id) {
                        snap.sov_runtime.sov_hubs.insert(hub_id, original);
                        return Err(e);
                    }
                    let out_owner = if c.corp > 0
                        && (c.corp != 98_000_001
                            || snap.corporations.get(&c.corp).is_some_and(|co| co.ceo_id != c.cid))
                    {
                        c.corp
                    } else {
                        c.cid
                    };
                    snap.next_entity_id += 1;
                    let item_id = snap.next_entity_id;
                    snap.items.insert(item_id, InventoryItemRecord {
                        item_id,
                        type_id,
                        owner_id: out_owner,
                        location_id: hub_id,
                        flag: eve_db::InventoryFlag::Hangar,
                        quantity: 1,
                        ..Default::default()
                    });
                    let mut n = Vec::new();
                    encode_bytes_field(1, &encode_upgrade_identifier(hub_id, type_id), &mut n);
                    id_field(2, type_id as u64, &mut n);
                    id_field(3, hub_id as u64, &mut n);
                    push_notice(&pending, sys, "eve_public.sovereignty.hub.upgrade.api.UninstalledNotice", n);
                    emit_hub_diffs(&pending, &before, &hub_views(snap, &sde, &[hub_id]));
                    Ok(())
                })?;
                Ok(any("eve_public.sovereignty.hub.upgrade.api.UninstallResponse", Vec::new()))
            }),
        );
    }

    super::quasar_sov_workforce::register_handlers(router, Arc::clone(&db), Arc::clone(&sde));
}
