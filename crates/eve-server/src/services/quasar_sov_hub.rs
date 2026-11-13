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
use super::sov_services::{
    TYPE_MAGMATIC_GAS, TYPE_ORBITAL_SKYHOOK, TYPE_SOVEREIGNTY_HUB, TYPE_SUPERIONIC_ICE,
};
use crate::quasar_gateway::{
    NoticeScope, PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_INTERNAL_ERROR,
    STATUS_NOT_FOUND, STATUS_UNAUTHORIZED,
};
use eve_db::{
    corp_roles, FromWalError, InventoryItemRecord, PersistedHubUpgrade, PersistedSovHubData,
    UpwellStructureRecord, WorldRepository, WorldStateSnapshot,
};
use eve_proto::pb::{self, ProtoBuilder};
use eve_proto::quasar::{
    AuthoritativeContext, DynamicQuasarMessage, DynamicValue, GoogleAny, GoogleTimestamp, WireField,
    WireMessage, WireValue, TYPE_GOOGLEAPIS_PREFIX,
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
    let sub = WireMessage {
        fields: vec![WireField { field_number: 1, value: WireValue::Varint(id) }],
    }
    .encode();
    out.extend(
        WireMessage {
            fields: vec![WireField {
                field_number: field,
                value: WireValue::LengthDelimited(sub),
            }],
        }
        .encode(),
    );
}

/// Encodes a `{ seconds = 1 }` timestamp sub-message at `field` (for excluded `quasar_sov_skyhook`).
pub(crate) fn ts_field(field: u32, seconds: u64, out: &mut Vec<u8>) {
    id_field(field, seconds, out);
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
    pub roles: u64,
}

pub(crate) fn corp_alliance(snap: &WorldStateSnapshot, corp_id: i64) -> Option<i64> {
    snap.corporations.get(&corp_id).and_then(|c| c.alliance_id)
}

pub(crate) fn caller(ctx: &AuthoritativeContext, snap: &WorldStateSnapshot) -> Result<Caller, RpcError> {
    let cid = ctx
        .effective_character_id()
        .ok_or((STATUS_UNAUTHORIZED, "Authenticated character required".to_string()))? as i64;
    let ch = snap.characters.get(&cid);
    let ctx_corp = ctx.effective_corporation_id().map(i64::from).filter(|c| *c > 0);
    let corp = match ch.map(|c| c.corporation_id) {
        Some(dc) if (1_000_000..2_000_000).contains(&dc) => ctx_corp
            .filter(|&cc| {
                cc != dc
                    && snap
                        .corporations
                        .get(&cc)
                        .is_some_and(|c| c.ceo_id == cid || c.members.contains(&cid))
            })
            .unwrap_or(dc),
        Some(dc) if dc > 0 => dc,
        _ => ctx_corp.ok_or((STATUS_FORBIDDEN, format!("Character {cid} has no corporation")))?,
    };
    let alliance = ch.and_then(|c| c.alliance_id).or_else(|| corp_alliance(snap, corp));
    let (is_ceo, eff) =
        super::social_corp_fleet::social_corp_projects::effective_corp_member_roles(
            snap,
            corp,
            cid,
            0,
            ctx.now_unix(),
        );
    let roles = if is_ceo { u64::MAX } else { eff };
    Ok(Caller { cid, corp, alliance, roles })
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

/// Resolves a structure of one of `types` strictly owned by the caller's active corporation
/// (preventing same-alliance alt-corp directors from mutating another corporation's infrastructure).
pub(crate) fn corp_owned_structure<'a>(
    snap: &'a WorldStateSnapshot,
    caller: &Caller,
    structure_id: i64,
    types: &[u32],
    what: &str,
) -> Result<&'a UpwellStructureRecord, RpcError> {
    let st = owned_structure(snap, caller, structure_id, types, what)?;
    if st.owner_corp_id != caller.corp {
        return Err((
            STATUS_FORBIDDEN,
            format!("{what} {structure_id} is not owned by your corporation"),
        ));
    }
    Ok(st)
}

/// Requires at least one of the corporation `roles` bits in the caller's live DB corporation state.
pub(crate) fn require_caller_roles(c: &Caller, roles: u64, action: &str) -> Result<(), RpcError> {
    if c.roles & roles == 0 {
        return Err((STATUS_FORBIDDEN, format!("Missing corporation role required to {action}")));
    }
    Ok(())
}

/// Requires at least one of the corporation `roles` bits in the caller's active identity.
#[allow(dead_code)]
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

/// Builds the definition of an SDE Sovereignty Hub upgrade type from the `44` upgrade rows
/// in `res:/staticdata/sovereigntyupgrades.fsdbinary` (`SdeRegistry::sovereignty_upgrades` /
/// `eve-wiki/articles/Sovereignty.md:155-198`, SDE build `3538132`), with
/// `fuel_startup = fuel_hourly_upkeep * 2` (`fuel_per_hour * 2`).
pub fn upgrade_definition(sde: &SdeRegistry, type_id: u32) -> Option<UpgradeDefinition> {
    let t = sde.types.get(&type_id)?;
    if !t.published || !SOV_HUB_UPGRADE_GROUPS.contains(&t.group_id) {
        return None;
    }
    let (base, _) = split_tier(&t.name);
    // 44 upgrade definitions from `res:/staticdata/sovereigntyupgrades.fsdbinary` (build 3538132).
    let (power_required, workforce_required, fuel_type, fuel_per_hour, exclusive_group, power_produced, workforce_produced) =
        match type_id {
            // Group 4772: Sovereignty Hub Service Infrastructure Upgrades
            81_623 => (1_750, 17_500, TYPE_SUPERIONIC_ICE, 90, base, 0, 0),
            81_621 => (500, 18_100, TYPE_SUPERIONIC_ICE, 25, base, 0, 0),
            81_619 => (250, 4_500, TYPE_MAGMATIC_GAS, 205, base, 0, 0),
            81_615 => (250, 1_500, TYPE_MAGMATIC_GAS, 205, base, 0, 0),
            // Group 4768: Sovereignty Hub Site Detection Upgrades (all 0 reagents/hr in V24)
            82_499 => (200, 1_800, 0, 0, base, 0, 0),
            82_500 => (500, 3_800, 0, 0, base, 0, 0),
            82_492 => (700, 5_400, 0, 0, base, 0, 0),
            82_496 => (400, 2_700, 0, 0, base, 0, 0),
            82_497 => (900, 5_400, 0, 0, base, 0, 0),
            82_498 => (1_300, 7_300, 0, 0, base, 0, 0),
            82_579 | 82_581 | 82_583 | 82_585 | 82_587 | 82_589 | 82_591 => {
                (450, 6_400, 0, 0, base, 0, 0)
            }
            82_580 | 82_582 | 82_584 | 82_586 | 82_588 | 82_590 | 82_592 => {
                (1_220, 12_700, 0, 0, base, 0, 0)
            }
            87_702 | 87_704 | 87_705 | 87_706 | 87_707 | 87_708 | 87_709 => {
                (1_800, 18_100, 0, 0, base, 0, 0)
            }
            87_948 => (400, 4_000, 0, 0, base, 0, 0),
            87_953 => (850, 8_000, 0, 0, base, 0, 0),
            87_954 => (1_300, 12_000, 0, 0, base, 0, 0),
            // Group 4839: Sovereignty Hub System Effect Generator Upgrades
            87_815 | 87_949 | 87_950 | 87_951 => {
                (200, 2_700, TYPE_MAGMATIC_GAS, 7, "SystemEffectGenerator", 0, 0)
            }
            // Group 4838: Sovereignty Hub Colony Resources Management Upgrades (`Sovereignty.md:36` — mutually exclusive across the group)
            87_703 => (0, 2_500, TYPE_SUPERIONIC_ICE, 8, "ResourceManagement", 200, 0),
            88_221 => (0, 10_000, TYPE_SUPERIONIC_ICE, 25, "ResourceManagement", 500, 0),
            88_227 => (0, 25_000, TYPE_SUPERIONIC_ICE, 42, "ResourceManagement", 1_000, 0),
            87_710 => (200, 0, TYPE_SUPERIONIC_ICE, 8, "ResourceManagement", 0, 5_000),
            88_228 => (500, 0, TYPE_SUPERIONIC_ICE, 25, "ResourceManagement", 0, 10_000),
            88_229 => (800, 0, TYPE_SUPERIONIC_ICE, 42, "ResourceManagement", 0, 15_000),
            _ => return None,
        };
    let fuel_startup = if fuel_type > 0 { fuel_per_hour * 2 } else { 0 };
    Some(UpgradeDefinition {
        type_id,
        power_required,
        workforce_required,
        fuel_type,
        fuel_per_hour,
        fuel_startup,
        exclusive_group: exclusive_group.to_string(),
        power_produced,
        workforce_produced,
    })
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
/// Follows the V24.01 Equinox stellar luminosity rules (`Sovereignty.md`): hotter spectral classes
/// yield more power, 800 (O) down to 200 (M).
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
/// Follows the V24.01 Equinox planetary resource rules (`Sovereignty.md:107-110`): Temperate /
/// Oceanic / Barren planets yield workforce, Plasma > Storm > Gas planets yield power, and Ice /
/// Oceanic (`Superionic Ice`) plus Lava (`Magmatic Gas`) yield colony reagents via Orbital Skyhooks.
pub const PLANET_RESOURCE_TABLE: [(u32, &str, u64, u64); 8] = [
    (11, "Temperate", 0, 1_500),
    (2014, "Oceanic", 0, 1_000),
    (2016, "Barren", 0, 500),
    (13, "Gas", 150, 0),
    (2017, "Storm", 175, 0),
    (2063, "Plasma", 200, 0),
    (12, "Ice", 0, 0),
    (2015, "Lava", 0, 0),
];

/// `(power, workforce)` harvested from a planet of the given SDE planet type
/// (looked up in [`PLANET_RESOURCE_TABLE`]).
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

/// Returns `true` when `planet_id` in `sys_id` has an active, non-destroyed Orbital Skyhook (`81080`).
pub fn is_planet_harvested_by_active_skyhook(
    snap: &WorldStateSnapshot,
    sys_id: u32,
    planet_id: i64,
) -> bool {
    snap.sov_runtime.skyhook_planets.iter().any(|(&sh_id, &pid)| {
        pid == planet_id
            && snap.structures.get(&sh_id).is_some_and(|s| {
                s.solar_system_id == sys_id
                    && s.type_id == TYPE_ORBITAL_SKYHOOK
                    && !s.state.eq_ignore_ascii_case("destroyed")
            })
            && snap
                .sov_runtime
                .skyhook_active
                .get(&sh_id)
                .copied()
                .unwrap_or(true)
    })
}

fn hub_local_and_base_pool(
    snap: &WorldStateSnapshot,
    sde: &SdeRegistry,
    hub_id: i64,
) -> Option<(u64, u64, u64, u64)> {
    let st = snap.structures.get(&hub_id)?;
    let sys_id = st.solar_system_id;
    let mut star_p = 0u64;
    let mut harvested_p = 0u64;
    let mut harvested_w = 0u64;
    for c in sde.celestials_in_system(sys_id) {
        if c.group_id == GROUP_SUN {
            star_p += star_power(sde, c.type_id);
        } else if c.group_id == GROUP_PLANET
            && is_planet_harvested_by_active_skyhook(snap, sys_id, c.celestial_id)
        {
            let (p, mut w) = planet_yield(c.type_id);
            if w > 0 {
                let siphoned: u64 = snap
                    .sov_runtime
                    .mercenary_dens
                    .values()
                    .filter(|d| {
                        d.solar_system_id == sys_id && d.enabled && d.planet_id == c.celestial_id
                    })
                    .map(|d| {
                        let stage = super::quasar_sov_mercden::stage_for_level(d.anarchy_level);
                        let pct = super::quasar_sov_mercden::anarchy_workforce_disruption_pct(stage);
                        (w * pct) / 100
                    })
                    .sum();
                w = w.saturating_sub(siphoned);
            }
            harvested_p += p;
            harvested_w += w;
        }
    }
    let power_local = star_p + harvested_p;
    let workforce_local = harvested_w;
    let imported: u64 = super::quasar_sov_workforce::imports(snap, sde, hub_id)
        .iter()
        .map(|s| s.1)
        .sum();
    let exported = super::quasar_sov_workforce::exported_amount(snap, hub_id);
    let base_workforce = (workforce_local + imported).saturating_sub(exported);
    Some((power_local, workforce_local, power_local, base_workforce))
}

/// Computes hub resources from SDE star power, Orbital Skyhook planetary harvest, active upgrades, and workforce transfers.
pub fn compute_hub_resources(snap: &WorldStateSnapshot, sde: &SdeRegistry, hub_id: i64) -> Option<HubResources> {
    let (power_local, workforce_local, base_p, base_w) =
        hub_local_and_base_pool(snap, sde, hub_id)?;
    let mut r = HubResources {
        power_local,
        workforce_local,
        ..Default::default()
    };
    let (mut produced_p, mut produced_w) = (0u64, 0u64);
    if let Some(h) = snap.sov_runtime.sov_hubs.get(&hub_id) {
        for d in h
            .upgrades
            .iter()
            .filter(|u| is_active(u))
            .filter_map(|u| upgrade_definition(sde, u.type_id))
        {
            r.power_allocated += d.power_required;
            r.workforce_allocated += d.workforce_required;
            produced_p += d.power_produced;
            produced_w += d.workforce_produced;
        }
    }
    r.power_available = base_p + produced_p;
    r.workforce_available = base_w + produced_w;
    Some(r)
}

/// Evaluates active upgrades strictly in priority-slot order (`0..upgrades.len()`), dropping any
/// slot whose power/workforce demand exceeds the base pool plus output of strictly higher-priority
/// slots into `low_power = true` (`Sovereignty.md:124`).
pub fn enforce_hub_power_overload(snap: &mut WorldStateSnapshot, sde: &SdeRegistry, hub_id: i64) -> bool {
    let Some((_, _, mut running_p, mut running_w)) = hub_local_and_base_pool(snap, sde, hub_id)
    else {
        return false;
    };
    let Some(h) = snap.sov_runtime.sov_hubs.get_mut(&hub_id) else {
        return false;
    };
    let mut changed = false;
    for u in &mut h.upgrades {
        if !is_active(u) {
            continue;
        }
        let Some(d) = upgrade_definition(sde, u.type_id) else {
            continue;
        };
        if d.power_required <= running_p && d.workforce_required <= running_w {
            running_p = running_p - d.power_required + d.power_produced;
            running_w = running_w - d.workforce_required + d.workforce_produced;
        } else {
            u.low_power = true;
            changed = true;
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
                state: super::quasar_sov_workforce::encode_state(snap, sde, id),
            }))
        })
        .collect()
}

fn fill_resource_pair(b: ProtoBuilder, available: u64, allocated: u64, local: u64) -> ProtoBuilder {
    b.set("available", available).set("allocated", allocated).set("local_harvest", local)
}

/// Emits `ResourcesSimulatedNotice` / workforce `StateChangedNotice` for every hub whose view changed.
pub(crate) fn emit_hub_diffs(pending: &PendingNotices, before: &BTreeMap<i64, HubView>, after: &BTreeMap<i64, HubView>) {
    for (hub_id, now) in after {
        let prev = before.get(hub_id);
        if prev.map(|p| p.resources) != Some(now.resources) {
            let r = now.resources;
            let n = pb::message("eve_public.sovereignty.hub.api.ResourcesSimulatedNotice")
                .set("hub.sequential", *hub_id as u64)
                .set("solar_system.sequential", now.sys)
                .set_with("power", |p| {
                    fill_resource_pair(p, r.power_available, r.power_allocated, r.power_local)
                })
                .set_with("workforce", |w| {
                    fill_resource_pair(
                        w,
                        r.workforce_available,
                        r.workforce_allocated,
                        r.workforce_local,
                    )
                })
                .must_encode();
            push_notice(pending, now.sys, "eve_public.sovereignty.hub.api.ResourcesSimulatedNotice", n);
        }
        if let Some(p) = prev {
            if p.state != now.state {
                let n = pb::message(
                    "eve_public.sovereignty.resource.transfer.workforce.api.StateChangedNotice",
                )
                .set("solar_system.sequential", now.sys)
                .set_raw_message("old_state", &p.state)
                .set_raw_message("new_state", &now.state)
                .set("hub.sequential", *hub_id as u64)
                .must_encode();
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
        if d.fuel_type == 0 || d.fuel_per_hour == 0 {
            continue;
        }
        let need = u64::from(d.fuel_per_hour) * hours;
        let have = hub.fuel.entry(d.fuel_type).or_insert(0);
        if u64::from(*have) >= need {
            *have -= need as u32;
        } else {
            *have %= d.fuel_per_hour;
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
    let n = pb::message("eve_public.sovereignty.hub.upgrade.api.HubUpgradesSimulatedNotice")
        .set_with("hub_upgrades", |b| {
            fill_hub_upgrades(b, sde, hub_id, snap.sov_runtime.sov_hubs.get(&hub_id))
        })
        .must_encode();
    push_notice(pending, sys, "eve_public.sovereignty.hub.upgrade.api.HubUpgradesSimulatedNotice", n);
    let after = hub_views(snap, sde, &[hub_id]);
    emit_hub_diffs(pending, &before, &after);
}

// ---------------------------------------------------------------------------------------------
// Upgrade encoders
// ---------------------------------------------------------------------------------------------

fn fill_definition(b: ProtoBuilder, d: &UpgradeDefinition) -> ProtoBuilder {
    b.set("installation_type.sequential", d.type_id)
        .set("power_required", d.power_required)
        .set("workforce_required", d.workforce_required)
        .when(d.fuel_type > 0, |b| {
            b.set("fuel_type.sequential", d.fuel_type)
                .set("fuel_consumption_per_hour", d.fuel_per_hour)
                .set("fuel_startup_cost", d.fuel_startup)
        })
        .set("mutually_exclusive_group", d.exclusive_group.as_str())
        .set("power_produced", d.power_produced)
        .set("workforce_produced", d.workforce_produced)
}

pub(crate) fn encode_definition(d: &UpgradeDefinition) -> Vec<u8> {
    fill_definition(pb::message("eve_public.sovereignty.hub.upgrade.Definition"), d).must_encode()
}

fn power_state(u: &PersistedHubUpgrade) -> u64 {
    match (u.online, u.low_power) {
        (false, _) => 1,    // POWER_STATE_OFFLINE
        (true, false) => 2, // POWER_STATE_ONLINE
        (true, true) => 3,  // POWER_STATE_LOW
    }
}

fn fill_upgrade_identifier(b: ProtoBuilder, hub_id: i64, type_id: u32) -> ProtoBuilder {
    b.set("hub.sequential", hub_id as u64)
        .set("installation_type.sequential", type_id)
        .set("upgrade_type.sequential", type_id)
}

fn fill_upgrade_attributes(b: ProtoBuilder, sde: &SdeRegistry, u: &PersistedHubUpgrade) -> ProtoBuilder {
    let b = if let Some(d) = upgrade_definition(sde, u.type_id) {
        b.set_with("definition", |def| fill_definition(def, &d))
    } else {
        b
    };
    b.set("power_state", power_state(u))
}

fn fill_hub_upgrades(
    mut b: ProtoBuilder,
    sde: &SdeRegistry,
    hub_id: i64,
    hub: Option<&PersistedSovHubData>,
) -> ProtoBuilder {
    b = b.set("hub.sequential", hub_id as u64);
    if let Some(h) = hub {
        for u in &h.upgrades {
            b = b.add_with("upgrades", |up| {
                up.set_with("identifier", |id| fill_upgrade_identifier(id, hub_id, u.type_id))
                    .set_with("attributes", |att| fill_upgrade_attributes(att, sde, u))
            });
        }
        b = b.set("last_updated", GoogleTimestamp::from_unix(h.last_updated_unix as i64, 0));
    }
    b
}

/// Encodes `eve_public.sovereignty.hub.upgrade.HubUpgrades` from persisted hub state.
pub(crate) fn encode_hub_upgrades(sde: &SdeRegistry, hub_id: i64, hub: Option<&PersistedSovHubData>) -> Vec<u8> {
    fill_hub_upgrades(pb::message("eve_public.sovereignty.hub.upgrade.HubUpgrades"), sde, hub_id, hub)
        .must_encode()
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

/// Applies `ProcessConfigurationRequest` to the world: installs upgrade items, reorders priority
/// slots when configured, toggles online state, transitions any overloaded lower-priority slots into
/// `POWER_STATE_LOW` (`Sovereignty.md:38-39`), and returns the hub's solar system.
fn process_configuration(
    snap: &mut WorldStateSnapshot,
    sde: &SdeRegistry,
    caller: &Caller,
    hub_id: i64,
    new_items: &[i64],
    config: &[(u32, bool)],
    now: u64,
) -> Result<u32, RpcError> {
    let sys = corp_owned_structure(snap, caller, hub_id, &[TYPE_SOVEREIGNTY_HUB], "Sovereignty hub")?.solar_system_id;
    let mut hub = snap.clone_sov_hub(hub_id);
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
            if d.fuel_type > 0 && (d.fuel_per_hour > 0 || d.fuel_startup > 0) {
                let have = hub.fuel.get(&d.fuel_type).copied().unwrap_or(0);
                let required = d.fuel_startup.saturating_add(d.fuel_per_hour);
                if have < required {
                    return Err((
                        STATUS_BAD_REQUEST,
                        format!(
                            "Insufficient fuel type {} to online upgrade {type_id}: have {have}, require {required}",
                            d.fuel_type
                        ),
                    ));
                }
                if d.fuel_startup > 0 {
                    hub.fuel.insert(d.fuel_type, have - d.fuel_startup);
                }
            }
        }
        u.online = *online;
        // A starved upgrade only leaves POWER_STATE_LOW once the bay holds an hour of its fuel.
        let fueled = upgrade_definition(sde, *type_id).is_some_and(|d| {
            d.fuel_type == 0
                || d.fuel_per_hour == 0
                || hub.fuel.get(&d.fuel_type).copied().unwrap_or(0) >= d.fuel_per_hour
        });
        if !*online || fueled {
            u.low_power = false;
        }
    }
    if config.len() > 1 {
        let mut reordered = Vec::with_capacity(hub.upgrades.len());
        for (tid, _) in config {
            if let Some(pos) = hub.upgrades.iter().position(|u| u.type_id == *tid) {
                reordered.push(hub.upgrades.remove(pos));
            }
        }
        reordered.append(&mut hub.upgrades);
        hub.upgrades = reordered;
    }
    let is_hacked = snap.sov_runtime.hacked_hubs.get(&hub_id).copied().is_some_and(|until| now < until);
    if !is_hacked {
        for u in &mut hub.upgrades {
            if u.online && u.low_power {
                if let Some(d) = upgrade_definition(sde, u.type_id) {
                    if d.fuel_type == 0
                        || d.fuel_per_hour == 0
                        || hub.fuel.get(&d.fuel_type).copied().unwrap_or(0) >= d.fuel_per_hour
                    {
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
    enforce_hub_power_overload(snap, sde, hub_id);
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
                | eve_db::InventoryFlag::SpecializedAsteroidHold
        )
    } else if caller.corp > 0 && it.owner_id == caller.corp {
        matches!(
            it.flag,
            eve_db::InventoryFlag::CorpSAG1
                | eve_db::InventoryFlag::CorpSAG2
                | eve_db::InventoryFlag::CorpSAG3
                | eve_db::InventoryFlag::CorpSAG4
                | eve_db::InventoryFlag::CorpSAG5
                | eve_db::InventoryFlag::CorpSAG6
                | eve_db::InventoryFlag::CorpSAG7
        ) && crate::services::mining_advanced::mining_compression::caller_can_take_corp_hangar_item(
            snap,
            caller.cid,
            caller.corp,
            it.flag,
            caller.roles,
        )
    } else {
        false
    };
    valid_owner
        && it.quantity >= 1
        && !snap.is_item_or_ancestor_locked_or_impounded(it.item_id)
        && !eve_db::WorldStateSnapshot::is_fitted_slot_flag(it.flag)
        && (it.location_id == hub_id
            || snap
                .characters
                .get(&caller.cid)
                .is_some_and(|ch| ch.active_ship_id == it.location_id && ch.solar_system_id == hub_sys))
}

/// Removes `qty` units from an inventory stack via the centralized snapshot consumer, deleting it when empty.
pub(crate) fn consume_items(snap: &mut WorldStateSnapshot, item_id: i64, qty: i32) {
    if let Some(owner_id) = snap.items.get(&item_id).map(|it| it.owner_id) {
        let mut opts = eve_db::ConsumeExactOptions::for_owner_exact(None, owner_id);
        opts.reject_singleton = false;
        let _ = snap.consume_exact_item_quantity(item_id, qty, opts);
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
                Ok(pb::message("eve_public.sovereignty.hub.api.GetResourcesResponse")
                    .set_with("power", |p| {
                        fill_resource_pair(p, r.power_available, r.power_allocated, r.power_local)
                    })
                    .set_with("workforce", |w| {
                        fill_resource_pair(
                            w,
                            r.workforce_available,
                            r.workforce_allocated,
                            r.workforce_local,
                        )
                    })
                    .must_to_any())
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
                    if d.fuel_type > 0 && d.fuel_per_hour > 0 {
                        *burn.entry(d.fuel_type).or_insert(0) += u64::from(d.fuel_per_hour);
                    }
                }
                let mut resp = pb::message("eve_public.sovereignty.hub.fuel.api.GetResponse");
                for (fuel_type, per_hour) in burn {
                    let amount = hub.fuel.get(&fuel_type).copied().unwrap_or(0);
                    resp = resp.add_with("fuels", |f| {
                        f.set("fuel_type.sequential", fuel_type)
                            .set("amount", amount)
                            .set("burned_per_hour", per_hour)
                    });
                }
                Ok(resp
                    .set("last_updated", GoogleTimestamp::from_unix(hub.last_updated_unix as i64, 0))
                    .must_to_any())
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
                    let hub_st = snap
                        .structures
                        .get(&hub_id)
                        .filter(|s| s.type_id == TYPE_SOVEREIGNTY_HUB && !s.state.eq_ignore_ascii_case("destroyed"))
                        .ok_or((STATUS_NOT_FOUND, format!("Sovereignty hub {hub_id} not found")))?;
                    let sys = hub_st.solar_system_id;
                    if !super::sov_services::can_access_sov_hub_fuel(
                        snap,
                        hub_id,
                        c.cid,
                        c.corp,
                        c.alliance,
                        c.roles,
                        true,
                    ) {
                        return Err((
                            STATUS_FORBIDDEN,
                            format!("Not authorized by fuel access group or corporation role on Sovereignty hub {hub_id}"),
                        ));
                    }
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
                    let now_ts = ctx.now_unix();
                    let is_hacked = snap
                        .sov_runtime
                        .hacked_hubs
                        .get(&hub_id)
                        .copied()
                        .is_some_and(|until| now_ts < until);
                    let prev_upgrades = hub.upgrades.clone();
                    if !is_hacked {
                        for u in hub.upgrades.iter_mut().filter(|u| u.online && u.low_power) {
                            if upgrade_definition(&sde, u.type_id).is_some_and(|d| {
                                d.fuel_type == 0
                                    || d.fuel_per_hour == 0
                                    || hub.fuel.get(&d.fuel_type).copied().unwrap_or(0) >= d.fuel_per_hour
                            }) {
                                u.low_power = false;
                            }
                        }
                    }
                    if hub.last_updated_unix == 0 {
                        hub.last_updated_unix = now_ts;
                    }
                    snap.sov_runtime.sov_hubs.insert(hub_id, hub);
                    let _ = enforce_hub_power_overload(snap, &sde, hub_id);
                    let restored = snap
                        .sov_runtime
                        .sov_hubs
                        .get(&hub_id)
                        .is_some_and(|h| h.upgrades != prev_upgrades);
                    if restored {
                        let n = pb::message(
                            "eve_public.sovereignty.hub.upgrade.api.HubUpgradesSimulatedNotice",
                        )
                        .set_with("hub_upgrades", |b| {
                            fill_hub_upgrades(b, &sde, hub_id, snap.sov_runtime.sov_hubs.get(&hub_id))
                        })
                        .must_encode();
                        push_notice(&pending, sys, "eve_public.sovereignty.hub.upgrade.api.HubUpgradesSimulatedNotice", n);
                    }
                    emit_hub_diffs(&pending, &before, &hub_views(snap, &sde, &[hub_id]));
                    Ok(())
                })?;
                Ok(pb::message("eve_public.sovereignty.hub.fuel.api.AddResponse").must_to_any())
            }),
        );
    }

    // eve_public.sovereignty.hub.upgrade.api.GetDefinitionsRequest
    {
        let sde = Arc::clone(&sde);
        router.register_handler(
            "eve_public.sovereignty.hub.upgrade.api.GetDefinitionsRequest",
            Arc::new(move |_ctx, _any, _dyn_msg| {
                let mut resp =
                    pb::message("eve_public.sovereignty.hub.upgrade.api.GetDefinitionsResponse");
                for d in all_upgrade_definitions(&sde) {
                    resp = resp.add_with("definitions", |def| fill_definition(def, &d));
                }
                Ok(resp.must_to_any())
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
                Ok(pb::message("eve_public.sovereignty.hub.upgrade.api.GetHubUpgradesResponse")
                    .set_with("hub_upgrades", |b| fill_hub_upgrades(b, &sde, hub_id, Some(&hub)))
                    .must_to_any())
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
                Ok(pb::message("eve_public.sovereignty.hub.upgrade.api.GetResponse")
                    .set_with("attributes", |att| fill_upgrade_attributes(att, &sde, &up))
                    .must_to_any())
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
                    require_caller_roles(&c, HUB_MANAGER_ROLES, "configure sovereignty hub upgrades")?;
                    corp_owned_structure(snap, &c, hub_id, &[TYPE_SOVEREIGNTY_HUB], "Sovereignty hub")?;
                    run_simulation(snap, &sde, &pending, hub_id, ctx.now_unix());
                    let before = hub_views(snap, &sde, &[hub_id]);
                    let sys = process_configuration(snap, &sde, &c, hub_id, &new_items, &config, ctx.now_unix())?;
                    let encoded = encode_hub_upgrades(&sde, hub_id, snap.sov_runtime.sov_hubs.get(&hub_id));
                    let n = pb::message(
                        "eve_public.sovereignty.hub.upgrade.api.HubUpgradesConfiguredNotice",
                    )
                    .set_raw_message("hub_upgrades", &encoded)
                    .must_encode();
                    push_notice(&pending, sys, "eve_public.sovereignty.hub.upgrade.api.HubUpgradesConfiguredNotice", n);
                    emit_hub_diffs(&pending, &before, &hub_views(snap, &sde, &[hub_id]));
                    Ok::<_, RpcError>(encoded)
                })?;
                Ok(pb::message("eve_public.sovereignty.hub.upgrade.api.ProcessConfigurationResponse")
                    .set_raw_message("hub_upgrades", hub_upgrades)
                    .must_to_any())
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
                    require_caller_roles(&c, HUB_MANAGER_ROLES, "uninstall sovereignty hub upgrades")?;
                    let st = corp_owned_structure(snap, &c, hub_id, &[TYPE_SOVEREIGNTY_HUB], "Sovereignty hub")?;
                    let sys = st.solar_system_id;
                    run_simulation(snap, &sde, &pending, hub_id, ctx.now_unix());
                    let before = hub_views(snap, &sde, &[hub_id]);
                    let prev_states: Vec<(u32, u64)> = snap
                        .sov_runtime
                        .sov_hubs
                        .get(&hub_id)
                        .map(|h| {
                            h.upgrades
                                .iter()
                                .filter(|u| u.type_id != type_id)
                                .map(|u| (u.type_id, power_state(u)))
                                .collect()
                        })
                        .unwrap_or_default();
                    let mut hub = snap.clone_sov_hub(hub_id);
                    let pos = hub
                        .upgrades
                        .iter()
                        .position(|u| u.type_id == type_id)
                        .ok_or((STATUS_NOT_FOUND, format!("Upgrade {type_id} is not installed in hub {hub_id}")))?;
                    // Per `Sovereignty.md:37`, decommissioning destroys the installed upgrade without returning an item.
                    hub.upgrades.remove(pos);
                    let is_hacked = snap
                        .sov_runtime
                        .hacked_hubs
                        .get(&hub_id)
                        .copied()
                        .is_some_and(|until| ctx.now_unix() < until);
                    if !is_hacked {
                        for u in &mut hub.upgrades {
                            if u.online && u.low_power {
                                if let Some(d) = upgrade_definition(&sde, u.type_id) {
                                    if d.fuel_type == 0
                                        || d.fuel_per_hour == 0
                                        || hub.fuel.get(&d.fuel_type).copied().unwrap_or(0) >= d.fuel_per_hour
                                    {
                                        u.low_power = false;
                                    }
                                }
                            }
                        }
                    }
                    snap.sov_runtime.sov_hubs.insert(hub_id, hub);
                    enforce_hub_power_overload(snap, &sde, hub_id);
                    let next_states: Vec<(u32, u64)> = snap
                        .sov_runtime
                        .sov_hubs
                        .get(&hub_id)
                        .map(|h| h.upgrades.iter().map(|u| (u.type_id, power_state(u))).collect())
                        .unwrap_or_default();
                    let n = pb::message("eve_public.sovereignty.hub.upgrade.api.UninstalledNotice")
                        .set_with("upgrade", |id| fill_upgrade_identifier(id, hub_id, type_id))
                        .set("upgrade_type.sequential", type_id)
                        .set("hub.sequential", hub_id as u64)
                        .must_encode();
                    push_notice(&pending, sys, "eve_public.sovereignty.hub.upgrade.api.UninstalledNotice", n);
                    if prev_states != next_states {
                        let sim_n = pb::message(
                            "eve_public.sovereignty.hub.upgrade.api.HubUpgradesSimulatedNotice",
                        )
                        .set_with("hub_upgrades", |b| {
                            fill_hub_upgrades(
                                b,
                                &sde,
                                hub_id,
                                snap.sov_runtime.sov_hubs.get(&hub_id),
                            )
                        })
                        .must_encode();
                        push_notice(
                            &pending,
                            sys,
                            "eve_public.sovereignty.hub.upgrade.api.HubUpgradesSimulatedNotice",
                            sim_n,
                        );
                    }
                    emit_hub_diffs(&pending, &before, &hub_views(snap, &sde, &[hub_id]));
                    Ok(())
                })?;
                Ok(pb::message("eve_public.sovereignty.hub.upgrade.api.UninstallResponse")
                    .must_to_any())
            }),
        );
    }

    super::quasar_sov_workforce::register_handlers(router, Arc::clone(&db), Arc::clone(&sde));
}

#[cfg(test)]
#[path = "quasar_sov_hub_tests.rs"]
mod quasar_sov_hub_tests;

#[path = "quasar_sov_hub_cov_unit_tests.rs"]
#[cfg(test)]
mod quasar_sov_hub_cov_unit_tests;

