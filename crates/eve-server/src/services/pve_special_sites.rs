//! `pve_special_sites` — Sansha Incursions (Constellation Influence, Site Tiers, Fleet ISK+LP Payouts,
//! Mothership HQ Finale), Homefront Operations, Metaliminal Roaming Storms, Drifter Hives, Triglavian
//! Pochven/EDENCOM Sites, and Solar System Interference (`Phase 8: W-3 & W-4`).
//!
// parity: feat:pve.incursion-system, feat:pve.incursion-payout, feat:pve.incursion-mothership
// parity: feat:pve.homefront-ops, feat:pve.metaliminal-storms, feat:pve.drifter-hives, feat:pve.triglavian-sites, feat:pve.interference

#![forbid(unsafe_code)]

use crate::router::notify_registry::{
    OnSolarsystemInterferenceChanged,
};
use crate::router::ServerContext;
use eve_db::{InventoryFlag, InventoryItemRecord, WorldRepository};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::SolarSystemId;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

pub use crate::services::pve_incursion_sites::*;
pub use crate::services::pve_homefront_sites::*;
pub use crate::services::pve_metaliminal_storms::*;
pub use crate::services::pve_special_sites_abyssal::*;
pub use crate::services::pve_special_sites_combat::*;
pub use crate::services::pve_crab_beacons::*;

pub use eve_db::{
    DrifterHiveRecord, DrifterHiveRoom, HomefrontSiteRecord, PveSpecialSitesState,
    SeasonalEventSiteRecord, SystemInterferenceRecord, TriglavianSiteRecord,
};

pub type PveSpecialSitesRegistry = PveSpecialSitesState;

pub fn with_pve_special_reg_db<R>(
    db: &Arc<WorldRepository>,
    now_sec: u64,
    f: impl FnOnce(&mut PveSpecialSitesRegistry) -> R,
) -> R {
    db.with_mut_snapshot(|snap| {
        if snap.pve_special_sites.sim_clock_sec < now_sec {
            snap.pve_special_sites.sim_clock_sec = now_sec;
        }
        let out = f(&mut snap.pve_special_sites);
        for (&cid, inc) in &snap.pve_special_sites.incursions {
            if inc.state == "Ended" {
                snap.incursions.remove(&cid);
            } else {
                let mut infested = vec![inc.staging_solar_system_id];
                for &s in &inc.vanguard_solar_system_ids {
                    if !infested.contains(&s) {
                        infested.push(s);
                    }
                }
                for &s in &inc.assault_solar_system_ids {
                    if !infested.contains(&s) {
                        infested.push(s);
                    }
                }
                if !infested.contains(&inc.hq_solar_system_id) {
                    infested.push(inc.hq_solar_system_id);
                }
                snap.incursions.insert(
                    cid,
                    eve_db::IncursionRecord {
                        constellation_id: cid,
                        faction_id: 500_019,
                        has_boss: inc.mothership_spawned && !inc.mothership_defeated,
                        infested_solar_systems: infested,
                        influence: inc.influence,
                        staging_solar_system_id: inc.staging_solar_system_id,
                        state: inc.state.to_lowercase(),
                        incursion_type: "Incursion".to_string(),
                    },
                );
            }
        }
        out
    })
}

pub fn read_pve_special_reg<R>(
    ctx: &ServerContext,
    f: impl FnOnce(&PveSpecialSitesRegistry) -> R,
) -> R {
    ctx.db.with_snapshot(|snap| f(&snap.pve_special_sites))
}

/// Populates deterministic SDE-derived special-site defaults into an in-memory registry
/// without mutating `EveDb`, used by read-only queries before world initialization.
fn populate_special_sites_defaults_in_memory(
    ctx: &ServerContext,
    reg: &mut PveSpecialSitesRegistry,
) {
    let now_sec = ctx.now_unix();
    if reg.sim_clock_sec < now_sec {
        reg.sim_clock_sec = now_sec;
    }
    if reg.incursions.is_empty() {
        let eligible_constellations =
            crate::services::pve_incursion_lifecycle::eligible_incursion_constellations(ctx);
        for &(cid, _) in eligible_constellations.iter().take(3) {
            seed_incursion_for_constellation(ctx, reg, cid);
        }
    }
    if reg.edencom_fortress_systems.is_empty() {
        reg.edencom_fortress_systems = edencom_fortress_systems();
    }
    if reg.edencom_minor_victory_systems.is_empty() {
        reg.edencom_minor_victory_systems = edencom_minor_victory_systems();
    }
    if reg.triglavian_minor_victory_systems.is_empty() {
        reg.triglavian_minor_victory_systems = triglavian_minor_victory_systems();
    }
    if reg.roaming_storms.is_empty() {
        crate::services::pve_special_sites_ops::ensure_full_roaming_storm_set(ctx, reg);
    }
    crate::services::pve_special_sites_ops::init_drifter_hives_from_sde(&ctx.sde, reg);
    for sys_id in reg
        .edencom_minor_victory_systems
        .iter()
        .chain(reg.triglavian_minor_victory_systems.iter())
        .copied()
        .collect::<Vec<_>>()
    {
        persist_triglavian_sites_for_system_sde(&ctx.sde, reg, sys_id);
    }
}

/// Reads `PveSpecialSitesRegistry` without ever mutating `EveDb`.
/// When `snap.pve_special_sites.initialized` is `true`, reads directly from the persisted snapshot;
/// when uninitialized, computes a deterministic read-pure in-memory view from SDE.
pub fn with_readonly_pve_special_reg<R>(
    ctx: &ServerContext,
    f: impl FnOnce(&PveSpecialSitesRegistry) -> R,
) -> R {
    if read_pve_special_reg(ctx, |reg| reg.initialized) {
        return read_pve_special_reg(ctx, f);
    }
    let mut view = read_pve_special_reg(ctx, Clone::clone);
    populate_special_sites_defaults_in_memory(ctx, &mut view);
    f(&view)
}

fn empire_faction_for_system(sde: &eve_sde::SdeRegistry, sys_id: SolarSystemId) -> u32 {
    sde.solar_systems
        .get(&sys_id)
        .and_then(|s| {
            sde.constellations
                .get(&s.constellation_id)
                .and_then(|c| c.faction_id)
                .or_else(|| sde.regions.get(&s.region_id).and_then(|r| r.faction_id))
                .or_else(|| {
                    s.stations
                        .iter()
                        .find_map(|st| st.owner_faction_id.map(|f| f as u32))
                })
        })
        .unwrap_or(500_002)
}

pub fn with_pve_special_reg<R>(
    ctx: &ServerContext,
    f: impl FnOnce(&mut PveSpecialSitesRegistry) -> R,
) -> R {
    with_pve_special_reg_db(&ctx.db, ctx.now_unix(), f)
}

/// Loads a JSON-serialized helper map stored in `snap.owner_notes`.
pub(crate) fn load_json_owner_note<T: Default + serde::de::DeserializeOwned>(
    snap: &eve_db::WorldStateSnapshot,
    note_id: i64,
) -> T {
    snap.owner_notes
        .get(&note_id)
        .and_then(|rec| serde_json::from_str(&rec.note).ok())
        .unwrap_or_default()
}

/// Saves or removes a JSON-serialized helper map in `snap.owner_notes`.
pub(crate) fn save_json_owner_note<T: serde::Serialize>(
    snap: &mut eve_db::WorldStateSnapshot,
    note_id: i64,
    label: &str,
    empty: bool,
    val: &T,
) {
    if empty {
        snap.owner_notes.remove(&note_id);
    } else if let Ok(json) = serde_json::to_string(val) {
        snap.owner_notes.insert(
            note_id,
            eve_db::OwnerNoteRecord {
                note_id,
                character_id: -1,
                item_id: note_id,
                label: label.to_string(),
                note: json,
                timestamp_unix: 0,
            },
        );
    }
}

pub(crate) const TRIGLAVIAN_DESTROYED_ESCORTS_NOTE_ID: i64 = -8_300_000_000_003;

pub(crate) fn load_triglavian_destroyed_escorts(
    snap: &eve_db::WorldStateSnapshot,
) -> std::collections::BTreeSet<i64> {
    load_json_owner_note(snap, TRIGLAVIAN_DESTROYED_ESCORTS_NOTE_ID)
}

pub(crate) fn save_triglavian_destroyed_escorts(
    snap: &mut eve_db::WorldStateSnapshot,
    destroyed: &std::collections::BTreeSet<i64>,
) {
    save_json_owner_note(
        snap,
        TRIGLAVIAN_DESTROYED_ESCORTS_NOTE_ID,
        "triglavian_destroyed_escorts",
        destroyed.is_empty(),
        destroyed,
    );
}

pub(crate) fn prune_triglavian_destroyed_escorts(
    snap: &mut eve_db::WorldStateSnapshot,
    escort_ball_ids: &[i64],
) {
    if escort_ball_ids.is_empty() {
        return;
    }
    let mut destroyed = load_triglavian_destroyed_escorts(snap);
    let before = destroyed.len();
    for &ebid in escort_ball_ids {
        destroyed.remove(&ebid);
    }
    if destroyed.len() != before {
        save_triglavian_destroyed_escorts(snap, &destroyed);
    }
}

fn rpc_err(code: &str, message: impl Into<String>) -> BridgeRpcError {
    BridgeRpcError { code: code.into(), message: message.into(), client_error: None }
}

#[derive(Clone, Copy)]
enum SpecialSiteRpcErrorCode {
    NotInSiteSystem,
    SiteAlreadyCompleted,
    WaveNpcsRemaining,
    SiteNotSpawned,
    BallparkSpawnFailed,
    DungeonNotFound,
}

impl SpecialSiteRpcErrorCode {
    fn as_str(self) -> &'static str {
        match self {
            Self::NotInSiteSystem => "NotInSiteSystem",
            Self::SiteAlreadyCompleted => "SiteAlreadyCompleted",
            Self::WaveNpcsRemaining => "WaveNpcsRemaining",
            Self::SiteNotSpawned => "SiteNotSpawned",
            Self::BallparkSpawnFailed => "BallparkSpawnFailed",
            Self::DungeonNotFound => "DungeonNotFound",
        }
    }

    fn build(self, message: impl Into<String>) -> BridgeRpcError {
        let msg = message.into();
        BridgeRpcError::with_client_error(
            self.as_str(),
            msg.clone(),
            eve_proto::ClientError::CustomInfo(msg),
        )
    }
}

pub(crate) fn next_spec_seq(reg: &mut PveSpecialSitesRegistry) -> i64 {
    reg.next_seq += 1;
    reg.next_seq
}

pub(crate) fn spec_now_sec(reg: &PveSpecialSitesRegistry) -> u64 {
    if reg.sim_clock_sec > 0 { reg.sim_clock_sec } else { 1_775_000_000 }
}

pub fn advance_pve_special_clock(ctx: &ServerContext, delta_sec: u64) {
    ensure_special_sites_initialized(ctx);
    let band_changes: Vec<(SolarSystemId, f64, u8, u64)> = with_pve_special_reg(ctx, |reg| {
        if reg.sim_clock_sec == 0 {
            reg.sim_clock_sec = 1_775_000_000;
        }
        reg.sim_clock_sec += delta_sec;
        if delta_sec >= 3_600 {
            advance_roaming_storms(ctx, reg, delta_sec);
        }
        let now_sec = ctx.now_unix().max(reg.sim_clock_sec);
        let mut changes = Vec::new();
        for (&sid, entry) in reg.interference_by_system.iter_mut() {
            let (level, band, changed_from) = compute_decayed_interference(entry, now_sec);
            if changed_from.is_some() || (entry.interference_level - level).abs() > 1e-9 {
                entry.interference_level = level;
                entry.band = band;
                entry.last_updated_sec = now_sec;
                if changed_from.is_some() {
                    changes.push((sid, level, band, now_sec));
                }
            }
        }
        changes
    });
    for (sid, level, band, ts) in band_changes {
        let local_state = build_interference_state_keyval_at(sid, level, band, ts);
        ctx.emit_typed(
            crate::router::NotificationTarget::SolarSystem(sid),
            OnSolarsystemInterferenceChanged {
                solar_system_id: sid as i64,
                new_interference_state: local_state,
            },
        );
    }
    crate::services::pve_crab_beacons::tick_all_crab_beacons(ctx);
    crate::services::pve_homefront_sites::tick_homefront_operations_sync(ctx);
    if delta_sec > 0 {
        crate::services::pve_incursion_lifecycle::advance_incursion_lifecycles(ctx);
    }
    sync_metaliminal_storm_effects_and_sites(ctx);
}

pub(crate) fn build_default_triglavian_sites_for_system_sde(
    sde: &eve_sde::SdeRegistry,
    sys_id: SolarSystemId,
    edencom_mv: &[SolarSystemId],
    trig_mv: &[SolarSystemId],
) -> Vec<TriglavianSiteRecord> {
    let is_pochven = sde
        .solar_systems
        .get(&sys_id)
        .is_some_and(|s| s.region_id == 10_000_070);
    let is_edencom = edencom_mv.contains(&sys_id);
    let is_trig = trig_mv.contains(&sys_id);
    if !is_pochven && !is_edencom && !is_trig {
        return Vec::new();
    }
    let empire_fac = if is_pochven {
        500_002
    } else {
        empire_faction_for_system(sde, sys_id)
    };
    crate::services::pve_special_sites_ops::build_triglavian_sites_from_sde(
        sde,
        sys_id,
        is_edencom,
        empire_navy_ships(empire_fac),
    )
}

pub(crate) fn persist_triglavian_sites_for_system_sde(
    sde: &eve_sde::SdeRegistry,
    reg: &mut PveSpecialSitesRegistry,
    sys_id: SolarSystemId,
) -> Vec<TriglavianSiteRecord> {
    if let Some(existing) = reg.triglavian_sites.get(&sys_id) {
        return existing.clone();
    }
    let edencom_mv = if reg.edencom_minor_victory_systems.is_empty() {
        edencom_minor_victory_systems()
    } else {
        reg.edencom_minor_victory_systems.clone()
    };
    let trig_mv = if reg.triglavian_minor_victory_systems.is_empty() {
        triglavian_minor_victory_systems()
    } else {
        reg.triglavian_minor_victory_systems.clone()
    };
    let sites = build_default_triglavian_sites_for_system_sde(sde, sys_id, &edencom_mv, &trig_mv);
    if !sites.is_empty() {
        reg.triglavian_sites.insert(sys_id, sites.clone());
    }
    sites
}

fn empire_navy_ships(empire_fac: u32) -> (u32, u32, u32) {
    match empire_fac {
        500_001 => (52_940, 52_938, 52_934), // Caldari: State Navy Raven, Drake, Raptor
        500_002 => (52_963, 52_961, 52_957), // Minmatar: Republic Fleet Tempest, Hurricane, Claw
        500_003 => (52_931, 52_929, 52_925), // Amarr: Imperial Navy Apocalypse, Harbinger, Crusader
        _ => (52_954, 52_952, 52_948),       // Gallente/default: Federal Navy Megathron, Brutix, Taranis
    }
}

pub fn edencom_fortress_systems() -> Vec<SolarSystemId> {
    vec![
        30000004, 30000005, 30000105, 30000113, 30000118, 30000188, 30002242, 30002243, 30002251, 30002253, 30002266,
        30002385, 30002386, 30002530, 30002651, 30002662, 30002665, 30002700, 30002704, 30002986, 30003050, 30003392,
        30003397, 30003398, 30003490, 30003514, 30003515, 30003539, 30003541, 30003548, 30003553, 30003556, 30003573,
        30003574, 30003883, 30003885, 30004084, 30004090, 30004100, 30004103, 30004141, 30004150, 30004248, 30004250,
        30004305, 30004973, 30004992, 30005052, 30005058, 30005251, 30005252, 30005260, 30045322,
    ]
}

pub fn edencom_minor_victory_systems() -> Vec<SolarSystemId> {
    vec![
        30000012, 30000048, 30000060, 30000062, 30000102, 30000109, 30000160, 30001376, 30001660, 30001696, 30001718,
        30002048, 30002051, 30002239, 30002241, 30002397, 30002506, 30002513, 30002644, 30002724, 30002755, 30002772,
        30002999, 30003058, 30003061, 30003074, 30003078, 30003088, 30003090, 30003460, 30003463, 30003478, 30003480,
        30003481, 30003482, 30003558, 30003570, 30003587, 30003788, 30003794, 30003809, 30003823, 30003824, 30003829,
        30003854, 30003894, 30003900, 30003904, 30003908, 30003918, 30003919, 30003927, 30003931, 30003932, 30004108,
        30004231, 30004254, 30004256, 30004257, 30004263, 30004268, 30004284, 30004287, 30004289, 30004295, 30004301,
        30004302, 30004978, 30004999, 30005034, 30005066, 30005074, 30005086, 30005209, 30005213, 30005219, 30005222,
        30005236, 30005255, 30005263, 30005267, 30005284, 30005308, 30005334,
    ]
}

pub fn triglavian_minor_victory_systems() -> Vec<SolarSystemId> {
    vec![
        30000163, 30000182, 30000205, 30001358, 30001383, 30001390, 30001391, 30001400, 30001401, 30001447, 30001685,
        30002557, 30002575, 30002645, 30002760, 30002771, 30002795, 30003073, 30003076, 30003464, 30003856, 30004244,
        30004981, 30005330, 30045331, 30045338, 30045345, 30045354,
    ]
}

pub fn ensure_triglavian_sites_for_system(reg: &mut PveSpecialSitesRegistry, sys_id: SolarSystemId, empire_fac: u32) {
    if reg.triglavian_sites.contains_key(&sys_id) {
        return;
    }
    if reg.edencom_fortress_systems.is_empty() { reg.edencom_fortress_systems = edencom_fortress_systems(); }
    if reg.edencom_minor_victory_systems.is_empty() { reg.edencom_minor_victory_systems = edencom_minor_victory_systems(); }
    if reg.triglavian_minor_victory_systems.is_empty() { reg.triglavian_minor_victory_systems = triglavian_minor_victory_systems(); }
    if reg.edencom_fortress_systems.contains(&sys_id) {
        return;
    }
    let is_edencom = reg.edencom_minor_victory_systems.contains(&sys_id);
    let base_inst = (sys_id as i64) * 100;
    let make_site = |inst, did: u32, name: &str, fac, isk, drop, boss_tid, esc_tids: Vec<u32>| {
        let esc_bids = (1..=esc_tids.len()).map(|i| 1_897_700_000 + inst * 10 + (i as i64)).collect();
        TriglavianSiteRecord {
            instance_id: inst, dungeon_id: did, dungeon_name: name.into(), solar_system_id: sys_id,
            faction_owner_id: fac, base_isk: isk, survey_db_drop: drop, completed: false,
            boss_npc_ball_id: 1_897_700_000 + inst, boss_type_id: boss_tid,
            escort_ball_ids: esc_bids, escort_type_ids: esc_tids,
            beacon_ball_id: 1_897_700_000 + inst * 10 + 9,
        }
    };
    let sites = if is_edencom {
        let (bs, bc, fr) = empire_navy_ships(empire_fac);
        vec![
            make_site(base_inst + 1, 8808, "EDENCOM Field Base", 500_027, 35_000_000, 150, bs, vec![bc, fr]),
            make_site(base_inst + 2, 8792, "EDENCOM Forward Post", 500_027, 25_000_000, 100, bc, vec![fr]),
        ]
    } else {
        vec![
            make_site(base_inst + 1, 8802, "Major Conduit", 500_026, 35_000_000, 150, 48_122, vec![48_086, 48_092]),
            make_site(base_inst + 2, 8742, "Minor Conduit", 500_026, 25_000_000, 100, 48_126, vec![48_088]),
        ]
    };
    reg.triglavian_sites.insert(sys_id, sites);
}

fn populate_special_sites_static_system_lists(reg: &mut PveSpecialSitesRegistry) {
    if reg.edencom_fortress_systems.is_empty() {
        reg.edencom_fortress_systems = edencom_fortress_systems();
    }
    if reg.edencom_minor_victory_systems.is_empty() {
        reg.edencom_minor_victory_systems = edencom_minor_victory_systems();
    }
    if reg.triglavian_minor_victory_systems.is_empty() {
        reg.triglavian_minor_victory_systems = triglavian_minor_victory_systems();
    }
}

pub fn ensure_special_sites_initialized(ctx: &ServerContext) {
    if read_pve_special_reg(ctx, |reg| reg.initialized) {
        return;
    }
    let started_tales: Vec<(i64, SolarSystemId)> = with_pve_special_reg(ctx, |reg| {
        if reg.initialized {
            return Vec::new();
        }
        reg.initialized = true;

        let eligible_constellations =
            crate::services::pve_incursion_lifecycle::eligible_incursion_constellations(ctx);
        for &(cid, _) in eligible_constellations.iter().take(3) {
            seed_incursion_for_constellation(ctx, reg, cid);
        }

        populate_special_sites_static_system_lists(reg);

        init_homefront_sites(ctx, reg);
        crate::services::pve_special_sites_ops::ensure_full_roaming_storm_set(ctx, reg);
        crate::services::pve_special_sites_ops::init_drifter_hives_from_sde(&ctx.sde, reg);

        for sys_id in reg
            .edencom_minor_victory_systems
            .iter()
            .chain(reg.triglavian_minor_victory_systems.iter())
            .copied()
            .collect::<Vec<_>>()
        {
            persist_triglavian_sites_for_system_sde(&ctx.sde, reg, sys_id);
        }
        reg.incursions
            .values()
            .filter(|inc| inc.state != "Ended")
            .map(|inc| (inc.tale_id, inc.staging_solar_system_id))
            .collect()
    });
    // Each newly seeded incursion is a running world-event tale; announce it to subscribed
    // clients (published after the registry snapshot lock is released).
    for (tale_id, staging) in started_tales {
        crate::services::pve_incursion_sites::publish_incursion_tale_notice(
            ctx.quasar_gateway.as_ref(),
            true,
            tale_id,
            staging,
            ctx.now_unix(),
        );
    }
    crate::services::pve_incursion_lifecycle::sync_incursion_system_effect_beacons(ctx);
    sync_metaliminal_storm_effects_and_sites(ctx);
}

fn collect_activation_special_site_balls(
    reg: &PveSpecialSitesRegistry,
    sys_id: SolarSystemId,
) -> (
    Vec<(SolarSystemId, i64, u32, DVec3)>,
    Vec<(SolarSystemId, i64, u32, DVec3)>,
) {
    let mut g_list = Vec::new();
    if let Some(hive) = reg.drifter_hives.get(&sys_id) {
        if !hive.cleared {
            if let Some(room) = hive
                .rooms
                .iter()
                .find(|r| r.room_index == hive.current_room && !r.cleared)
            {
                for (&bid, &tid) in room.npc_ball_ids.iter().zip(&room.npc_type_ids) {
                    if !room.destroyed_npc_ball_ids.contains(&bid) {
                        g_list.push((
                            hive.solar_system_id,
                            bid,
                            tid,
                            DVec3::new(10_000.0, 0.0, 0.0),
                        ));
                    }
                }
                if room.beacon_ball_id != 0 {
                    g_list.push((
                        hive.solar_system_id,
                        room.beacon_ball_id,
                        42_538,
                        DVec3::new(12_000.0, 0.0, 0.0),
                    ));
                }
            }
        }
    }
    let t_list = reg
        .triglavian_sites
        .get(&sys_id)
        .map(|sites| collect_triglavian_site_balls(sites))
        .unwrap_or_default();
    (g_list, t_list)
}

pub(crate) fn seed_special_sites_in_activation(
    seed_ctx: &mut crate::system_actor::SystemContentSeedContext<'_>,
    sde: &Arc<eve_sde::SdeRegistry>,
    db: &Arc<WorldRepository>,
    combat_ephemeral: &dyn eve_db::lock_rank::AnyRwLock<
        crate::services::combat_crimewatch::CombatRuntimeState,
    >,
) {
    if !seed_ctx.db_read_safe {
        return;
    }
    let sys_id = seed_ctx.solar_system_id;
    let now_sec = seed_ctx.now_unix_secs();

    let is_drifter = (31_000_001..=31_000_005).contains(&sys_id);
    let is_pochven = sde
        .solar_systems
        .get(&sys_id)
        .is_some_and(|s| s.region_id == 10_000_070);
    let is_trig_candidate = is_pochven
        || edencom_minor_victory_systems().contains(&sys_id)
        || triglavian_minor_victory_systems().contains(&sys_id);

    let (needs_db_write, ro_general_balls, ro_trig_balls, destroyed_escorts) =
        db.with_snapshot(|snap| {
            let reg = &snap.pve_special_sites;
            let needs_hive = is_drifter && !reg.drifter_hives.contains_key(&sys_id);
            let needs_trig = is_trig_candidate && !reg.triglavian_sites.contains_key(&sys_id);
            let (g_list, t_list) = if needs_hive || needs_trig {
                (Vec::new(), Vec::new())
            } else {
                collect_activation_special_site_balls(reg, sys_id)
            };
            (
                needs_hive || needs_trig,
                g_list,
                t_list,
                load_triglavian_destroyed_escorts(snap),
            )
        });

    let (general_balls, trig_balls) = if needs_db_write {
        with_pve_special_reg_db(db, now_sec, |reg| {
            if is_drifter {
                crate::services::pve_special_sites_ops::init_drifter_hive_for_system_from_sde(
                    sde, reg, sys_id,
                );
            }
            if is_trig_candidate {
                populate_special_sites_static_system_lists(reg);
                persist_triglavian_sites_for_system_sde(sde, reg, sys_id);
            }
            collect_activation_special_site_balls(reg, sys_id)
        })
    } else {
        (ro_general_balls, ro_trig_balls)
    };

    for (ball_sys_id, bid, tid, pos) in general_balls.into_iter().chain(trig_balls) {
        if bid <= 0 || ball_sys_id != sys_id || destroyed_escorts.contains(&bid) {
            continue;
        }
        if !seed_ctx.claim_special_site_ball(bid) {
            continue;
        }
        if seed_ctx.ballpark.balls.contains_key(&bid) {
            continue;
        }
        let ball = make_site_npc_ball(bid, tid, pos);
        if tid == 42_538 {
            seed_ctx.spawn_ball(ball);
        } else {
            let cs = build_special_site_npc_combat_state(sde, tid);
            seed_ctx.spawn_ball_with_combat(ball, cs);
        }
    }

    crate::services::pve_special_sites_combat::sync_seasonal_event_sites_in_seed_ctx(
        seed_ctx,
        sde,
        db,
        combat_ephemeral,
    );
}


pub(crate) fn build_special_site_npc_combat_state(
    sde: &eve_sde::SdeRegistry,
    type_id: u32,
) -> eve_destiny::CombatState {
    if let Some(t) = sde.types.get(&type_id) {
        let a = &t.default_attributes;
        // HP and capacitor come straight from the SDE type (no clamping to an invented band).
        let shield = a.get(&eve_sde::attr_ids::SHIELD_CAPACITY).copied().unwrap_or(0.0);
        let armor = a.get(&eve_sde::attr_ids::ARMOR_HP).copied().unwrap_or(0.0);
        let hull = a.get(&eve_sde::attr_ids::HP_STRUCT).copied().unwrap_or(0.0);
        let cap = a.get(&eve_sde::attr_ids::CAPACITOR_CAPACITY).copied().unwrap_or(0.0);
        let sig = a
            .get(&eve_sde::attr_ids::SIGNATURE_RADIUS)
            .copied()
            .unwrap_or_else(|| t.radius.max(35.0));
        let shield_rech = a
            .get(&eve_sde::attr_ids::SHIELD_RECHARGE_RATE)
            .copied()
            .unwrap_or_else(|| {
                sde.attributes
                    .get(&eve_sde::attr_ids::SHIELD_RECHARGE_RATE)
                    .map(|d| d.default_value)
                    .unwrap_or(0.0)
            });
        let cap_rech = a
            .get(&eve_sde::attr_ids::RECHARGE_RATE)
            .copied()
            .unwrap_or_else(|| {
                sde.attributes
                    .get(&eve_sde::attr_ids::RECHARGE_RATE)
                    .map(|d| d.default_value)
                    .unwrap_or(0.0)
            });
        return eve_destiny::CombatState {
            shield_hp: shield,
            max_shield_hp: shield,
            armor_hp: armor,
            max_armor_hp: armor,
            structure_hp: hull,
            max_structure_hp: hull,
            capacitor: cap,
            max_capacitor: cap,
            signature_radius: sig,
            shield_recharge_time_ms: shield_rech,
            capacitor_recharge_time_ms: cap_rech,
            shield_resonance: [
                a.get(&eve_sde::attr_ids::SHIELD_EM_DAMAGE_RESONANCE).copied().unwrap_or(1.0),
                a.get(&eve_sde::attr_ids::SHIELD_THERMAL_DAMAGE_RESONANCE).copied().unwrap_or(1.0),
                a.get(&eve_sde::attr_ids::SHIELD_KINETIC_DAMAGE_RESONANCE).copied().unwrap_or(1.0),
                a.get(&eve_sde::attr_ids::SHIELD_EXPLOSIVE_DAMAGE_RESONANCE).copied().unwrap_or(1.0),
            ],
            armor_resonance: [
                a.get(&eve_sde::attr_ids::ARMOR_EM_DAMAGE_RESONANCE).copied().unwrap_or(1.0),
                a.get(&eve_sde::attr_ids::ARMOR_THERMAL_DAMAGE_RESONANCE).copied().unwrap_or(1.0),
                a.get(&eve_sde::attr_ids::ARMOR_KINETIC_DAMAGE_RESONANCE).copied().unwrap_or(1.0),
                a.get(&eve_sde::attr_ids::ARMOR_EXPLOSIVE_DAMAGE_RESONANCE).copied().unwrap_or(1.0),
            ],
            structure_resonance: [
                a.get(&eve_sde::attr_ids::EM_DAMAGE_RESONANCE).copied().unwrap_or(1.0),
                a.get(&eve_sde::attr_ids::THERMAL_DAMAGE_RESONANCE).copied().unwrap_or(1.0),
                a.get(&eve_sde::attr_ids::KINETIC_DAMAGE_RESONANCE).copied().unwrap_or(1.0),
                a.get(&eve_sde::attr_ids::EXPLOSIVE_DAMAGE_RESONANCE).copied().unwrap_or(1.0),
            ],
            ..eve_destiny::CombatState::default()
        };
    }
    eve_destiny::CombatState {
        shield_hp: 1_200.0,
        max_shield_hp: 1_200.0,
        armor_hp: 1_200.0,
        max_armor_hp: 1_200.0,
        structure_hp: 1_000.0,
        max_structure_hp: 1_000.0,
        ..eve_destiny::CombatState::default()
    }
}

pub(crate) fn deposit_loot_into_ship_or_container_in_snapshot(
    snap: &mut eve_db::WorldStateSnapshot,
    sde: &eve_sde::SdeRegistry,
    char_id: i64,
    solar_system_id: SolarSystemId,
    ship_id: i64,
    items_to_award: &[(i64, u32, i32)],
) -> Option<i64> {
    let loot_vol: f64 = items_to_award
        .iter()
        .map(|&(_, tid, qty)| {
            sde.types
                .get(&tid)
                .map(|t| t.volume.max(0.01))
                .unwrap_or(0.1)
                * f64::from(qty.max(1))
        })
        .sum();
    let ship_can_hold = if ship_id > 0 {
        if let Some(ship_item) = snap.items.get(&ship_id) {
            if let Some(st) = sde.types.get(&ship_item.type_id) {
                if st.category_id == 6 {
                    let cap = st
                        .capacity
                        .max(st.default_attributes.get(&38).copied().unwrap_or(0.0));
                    let used: f64 = snap
                        .items
                        .values()
                        .filter(|i| i.location_id == ship_id && i.flag == InventoryFlag::Cargo)
                        .map(|i| {
                            sde.types
                                .get(&i.type_id)
                                .map(|t| t.volume.max(0.0))
                                .unwrap_or(1.0)
                                * f64::from(i.quantity.max(1))
                        })
                        .sum();
                    cap <= 0.0 || used + loot_vol <= cap + 1e-4
                } else {
                    false
                }
            } else {
                false
            }
        } else {
            false
        }
    } else {
        false
    };

    let (target_loc_id, spawned_container_id) = if ship_can_hold {
        (ship_id, None)
    } else {
        let cid = snap.alloc_entity_id();
        snap.items.insert(
            cid,
            InventoryItemRecord {
                item_id: cid,
                type_id: 23, // Cargo Container
                owner_id: char_id,
                location_id: solar_system_id as i64,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        (cid, Some(cid))
    };

    for &(iid, tid, qty) in items_to_award {
        let item_id = if iid > 0 && !snap.items.contains_key(&iid) {
            iid
        } else {
            snap.alloc_entity_id()
        };
        snap.items.insert(
            item_id,
            InventoryItemRecord {
                item_id,
                type_id: tid,
                owner_id: char_id,
                location_id: target_loc_id,
                flag: InventoryFlag::Cargo,
                quantity: qty,
                singleton: false,
                ..Default::default()
            },
        );
    }
    spawned_container_id
}

#[cfg(any(test, feature = "testkit"))]
pub async fn ensure_special_sites_spawned(ctx: &ServerContext) {
    if let Err(err) = ensure_special_sites_spawned_for_systems(ctx, None, true).await {
        tracing::warn!(error = %err.message, code = %err.code, "Failed spawning special sites");
    }
}

/// Lazily initializes and spawns special-site NPCs/beacons ONLY for `sys_id` so querying one
/// system does not spawn actors across all unvisited Drifter/EDENCOM/Triglavian systems (`#1159`).
#[cfg(any(test, feature = "testkit"))]
pub async fn ensure_special_sites_spawned_for_system(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
) {
    if let Err(err) = ensure_special_sites_spawned_for_systems(ctx, Some(sys_id), true).await {
        tracing::warn!(system_id = sys_id, error = %err.message, code = %err.code, "Failed spawning special sites for system");
    }
}

/// Spawns special-site NPCs/beacons in `sys_id` ONLY when `sys_id` is already loaded/live; fails
/// closed with no actor wake and no DB write if `sys_id` is cold (`#1542`).
pub(crate) async fn ensure_special_sites_spawned_if_live_for_system(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
) {
    if ctx.systems.get_handle(sys_id).is_none() {
        return;
    }
    if let Err(err) = ensure_special_sites_spawned_for_systems(ctx, Some(sys_id), false).await {
        tracing::warn!(system_id = sys_id, error = %err.message, code = %err.code, "Failed spawning special sites for live system");
    }
}

fn collect_triglavian_site_balls(sites: &[TriglavianSiteRecord]) -> Vec<(SolarSystemId, i64, u32, DVec3)> {
    let mut t_list = Vec::new();
    for (s_idx, s) in sites.iter().enumerate() {
        if !s.completed {
            let boss_tid = if s.boss_type_id != 0 { s.boss_type_id } else { 48_122 };
            let base_x = 10_000.0 + (s_idx as f64) * 20_000.0;
            if s.boss_npc_ball_id > 0 {
                t_list.push((s.solar_system_id, s.boss_npc_ball_id, boss_tid, DVec3::new(base_x, 0.0, 0.0)));
            }
            if s.beacon_ball_id > 0 {
                t_list.push((s.solar_system_id, s.beacon_ball_id, 42_538, DVec3::new(base_x - 500.0, 0.0, 0.0)));
            }
            for (e_idx, (&esc_bid, &esc_tid)) in s.escort_ball_ids.iter().zip(&s.escort_type_ids).enumerate() {
                if esc_bid > 0 {
                    let offset_x = base_x + 5_000.0 + (e_idx as f64) * 1_000.0;
                    let offset_y = 2_000.0 + (e_idx as f64) * 1_500.0;
                    t_list.push((s.solar_system_id, esc_bid, esc_tid, DVec3::new(offset_x, offset_y, 0.0)));
                }
            }
        }
    }
    t_list
}

async fn spawn_special_site_balls(
    ctx: &ServerContext,
    balls: Vec<(SolarSystemId, i64, u32, DVec3)>,
) -> Result<(), BridgeRpcError> {
    let destroyed_escorts = ctx.db.with_snapshot(load_triglavian_destroyed_escorts);
    for (sys_id, bid, tid, pos) in balls {
        if bid <= 0 || destroyed_escorts.contains(&bid) {
            continue;
        }
        let Some(sys_handle) = ctx.systems.get_handle(sys_id) else {
            continue;
        };
        let Some(spawn_guard) = ctx.systems.begin_special_site_ball_spawn(sys_id, bid).await else {
            continue;
        };
        let snapshot = sys_handle.get_ball_snapshot(bid).await.map_err(|err| {
            SpecialSiteRpcErrorCode::BallparkSpawnFailed.build(format!(
                "Failed querying ballpark for special site ball {bid} in system {sys_id}: {err}"
            ))
        })?;
        if snapshot.is_none() {
            let spawn_res = if tid == 42_538 {
                sys_handle.spawn_ball(make_site_npc_ball(bid, tid, pos)).await
            } else {
                let cs = build_special_site_npc_combat_state(&ctx.sde, tid);
                sys_handle
                    .spawn_ball_with_combat(make_site_npc_ball(bid, tid, pos), cs)
                    .await
            };
            spawn_res.map_err(|err| {
                SpecialSiteRpcErrorCode::BallparkSpawnFailed.build(format!(
                    "Failed spawning special site ball {bid} in system {sys_id}: {err}"
                ))
            })?;
        }
        spawn_guard.confirm();
    }
    Ok(())
}

async fn ensure_special_sites_spawned_for_systems(
    ctx: &ServerContext,
    filter_sys: Option<SolarSystemId>,
    allow_wake: bool,
) -> Result<(), BridgeRpcError> {
    if !allow_wake {
        if let Some(sys_id) = filter_sys {
            if ctx.systems.get_handle(sys_id).is_none() {
                return Ok(());
            }
        }
    }
    ensure_special_sites_initialized(ctx);
    if let Some(sys_id) = filter_sys {
        with_pve_special_reg(ctx, |reg| {
            crate::services::pve_homefront_sites::ensure_homefront_sites_for_system_locked(
                ctx, reg, sys_id,
            );
            persist_triglavian_sites_for_system_sde(&ctx.sde, reg, sys_id);
        });
    }
    let want_sys = |sid: SolarSystemId| filter_sys.is_none_or(|target| target == sid);
    let (general_to_spawn, hf_to_spawn, trig_to_spawn) = read_pve_special_reg(ctx, |reg| {
        let mut g_list = Vec::new();
        for inc in reg.incursions.values() {
            if inc.state == "Ended" || inc.state == "Withdrawing" { continue; }
            for s in &inc.sites {
                if !want_sys(s.solar_system_id) {
                    continue;
                }
                if s.locked_until_low_influence && (!inc.mothership_spawned || inc.influence > 0.10) {
                    continue;
                }
                let on_final_wave = reg
                    .incursion_site_waves
                    .get(&s.site_id)
                    .is_none_or(|w| w.current_wave >= w.total_waves);
                if on_final_wave && !s.boss_destroyed {
                    g_list.push((
                        s.solar_system_id,
                        s.boss_npc_ball_id,
                        s.boss_type_id,
                        DVec3::from_array(s.position),
                    ));
                }
                for esc in &s.escort_spawns {
                    if !esc.destroyed {
                        let esc_pos =
                            DVec3::from_array(s.position) + DVec3::new(esc.offset[0], esc.offset[1], esc.offset[2]);
                        g_list.push((s.solar_system_id, esc.npc_ball_id, esc.type_id, esc_pos));
                    }
                }
            }
        }
        let mut hf_list = Vec::new();
        for sites in reg.homefront_sites.values() {
            for s in sites {
                if want_sys(s.solar_system_id) && !s.completed {
                    hf_list.push((s.solar_system_id, s.instance_id));
                }
            }
        }
        for hive in reg.drifter_hives.values() {
            if !want_sys(hive.solar_system_id) {
                continue;
            }
            if !hive.cleared {
                if let Some(room) = hive.rooms.iter().find(|r| r.room_index == hive.current_room && !r.cleared) {
                    for (&bid, &tid) in room.npc_ball_ids.iter().zip(&room.npc_type_ids) {
                        if room.destroyed_npc_ball_ids.contains(&bid) { continue; }
                        g_list.push((hive.solar_system_id, bid, tid, DVec3::new(10_000.0, 0.0, 0.0)));
                    }
                    if room.beacon_ball_id != 0 {
                        g_list.push((hive.solar_system_id, room.beacon_ball_id, 42_538, DVec3::new(12_000.0, 0.0, 0.0)));
                    }
                }
            }
        }
        let mut t_list = Vec::new();
        for sites in reg.triglavian_sites.values() {
            if sites.first().is_some_and(|s| want_sys(s.solar_system_id)) {
                t_list.extend(collect_triglavian_site_balls(sites));
            }
        }
        (g_list, hf_list, t_list)
    });

    if allow_wake {
        let mut systems_to_wake = std::collections::BTreeSet::new();
        for &(sys_id, _, _, _) in &general_to_spawn {
            systems_to_wake.insert(sys_id);
        }
        for &(sys_id, _, _, _) in &trig_to_spawn {
            systems_to_wake.insert(sys_id);
        }
        for &(sys_id, _) in &hf_to_spawn {
            systems_to_wake.insert(sys_id);
        }
        for sys_id in systems_to_wake {
            let _ = ctx
                .systems
                .wake_for_gm_command_async(sys_id, "EnsureSpecialSitesSpawned")
                .await;
        }
    }

    // Homefront Operations spawn ONLY their outer entry beacon + acceleration gate;
    // pocket NPCs and archetype objectives spawn upon gate activation.
    for (sys_id, inst_id) in hf_to_spawn {
        if allow_wake || ctx.systems.get_handle(sys_id).is_some() {
            crate::services::pve_homefront_sites::ensure_homefront_outer_entries_sync(
                ctx,
                Some(sys_id),
                Some(inst_id),
            );
        }
    }
    spawn_special_site_balls(ctx, general_to_spawn).await?;
    spawn_special_site_balls(ctx, trig_to_spawn).await?;
    Ok(())
}

pub async fn on_warp_to_homefront_site(
    ctx: &ServerContext,
    solar_system_id: SolarSystemId,
    instance_id: i64,
) {
    ensure_special_sites_initialized(ctx);
    crate::services::pve_homefront_sites::ensure_homefront_outer_entries_sync(
        ctx,
        Some(solar_system_id),
        Some(instance_id),
    );
}


async fn verify_triglavian_npc_ball_cleared(
    ctx: &ServerContext,
    sys_handle: &crate::system_actor::SystemHandle,
    sys_id: SolarSystemId,
    npc_ball_id: i64,
    role: &str,
) -> Result<(), BridgeRpcError> {
    if npc_ball_id <= 0 || !ctx.systems.is_special_site_ball_spawned(sys_id, npc_ball_id) {
        return Err(SpecialSiteRpcErrorCode::SiteNotSpawned.build(format!(
            "Triglavian site {role} NPC {npc_ball_id} has not been spawned in system {sys_id}"
        )));
    }
    let snap_opt = sys_handle.get_ball_snapshot(npc_ball_id).await.map_err(|err| {
        SpecialSiteRpcErrorCode::BallparkSpawnFailed.build(format!(
            "Failed querying ballpark for Triglavian site {role} NPC {npc_ball_id}: {err}"
        ))
    })?;
    if snap_opt.is_some() {
        return Err(SpecialSiteRpcErrorCode::WaveNpcsRemaining.build(format!(
            "Triglavian site {role} NPC {npc_ball_id} is still alive"
        )));
    }
    Ok(())
}

async fn verify_triglavian_npcs_cleared(
    ctx: &ServerContext,
    sys_handle: &crate::system_actor::SystemHandle,
    trig: &TriglavianSiteRecord,
) -> Result<(), BridgeRpcError> {
    verify_triglavian_npc_ball_cleared(
        ctx,
        sys_handle,
        trig.solar_system_id,
        trig.boss_npc_ball_id,
        "wave",
    )
    .await?;
    let destroyed_escorts = ctx.db.with_snapshot(load_triglavian_destroyed_escorts);
    for &ebid in &trig.escort_ball_ids {
        if destroyed_escorts.contains(&ebid) {
            continue;
        }
        verify_triglavian_npc_ball_cleared(
            ctx,
            sys_handle,
            trig.solar_system_id,
            ebid,
            "escort",
        )
        .await?;
    }
    Ok(())
}

pub async fn complete_special_dungeon_instance(
    ctx: &ServerContext,
    session: &mut SessionState,
    instance_id: i64,
    _fleet_count: usize,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    if session.station_id.is_some() || session.structure_id.is_some() {
        return Err(rpc_err(
            "ShipDocked",
            "Cannot complete in-space special dungeon instance while docked",
        ));
    }
    let caller_sys = session.solar_system_id.ok_or_else(|| rpc_err("NotInSpace", "Caller must be in space"))?;

    // 1. Check Drifter Hives (by solarSystemID 31000001..31000005 or dungeonID 5642..5651)
    let hive_lookup = read_pve_special_reg(ctx, |reg| {
        reg.drifter_hives.values().find(|h| h.solar_system_id as i64 == instance_id || h.dungeon_id as i64 == instance_id).cloned()
    });
    if let Some(hive) = hive_lookup {
        if caller_sys != hive.solar_system_id {
            return Err(rpc_err("NotInSiteSystem", format!("Must be in system {} to complete {}", hive.solar_system_id, hive.dungeon_name)));
        }
        if hive.cleared {
            return Err(rpc_err("SiteAlreadyCompleted", format!("Drifter Hive {} is already cleared", hive.site_id)));
        }
        let sys_handle = ctx.systems.get_handle(caller_sys).ok_or_else(|| {
            SpecialSiteRpcErrorCode::SiteNotSpawned.build(format!(
                "Drifter Hive system {caller_sys} is not active"
            ))
        })?;
        if hive.current_room < hive.total_rooms {
            let current_room_idx = hive.current_room;
            let room_already_cleared = hive.rooms.iter().find(|r| r.room_index == current_room_idx).map(|r| r.cleared).unwrap_or(false);
            if !room_already_cleared {
                if let Some(room) = hive.rooms.iter().find(|r| r.room_index == current_room_idx) {
                    for &bid in &room.npc_ball_ids {
                        if !room.destroyed_npc_ball_ids.contains(&bid) && sys_handle.get_ball_snapshot(bid).await.ok().flatten().is_some() {
                            return Err(rpc_err("WaveNpcsRemaining", format!("Room {} NPC {} has not been destroyed", current_room_idx, bid)));
                        }
                    }
                }
            }
            let (kv, next_to_spawn) = with_pve_special_reg(ctx, |reg| {
                let h = reg.drifter_hives.get_mut(&hive.solar_system_id).unwrap();
                if let Some(r) = h.rooms.iter_mut().find(|r| r.room_index == current_room_idx) {
                    r.cleared = true;
                }
                if h.current_room == current_room_idx {
                    h.current_room += 1;
                }
                let next_r_idx = h.current_room;
                let next_room_info = h.rooms.iter().find(|r| r.room_index == next_r_idx).cloned();
                let kv = PyRep::keyval([
                    ("siteID", PyRep::String(h.site_id.clone())),
                    ("dungeonID", PyRep::Integer(h.dungeon_id as i64)),
                    ("dungeonName", PyRep::String(h.dungeon_name.clone())),
                    ("solarSystemID", PyRep::Integer(h.solar_system_id as i64)),
                    ("currentRoom", PyRep::Integer(h.current_room as i64)),
                    ("totalRooms", PyRep::Integer(h.total_rooms as i64)),
                    ("cleared", PyRep::Bool(false)),
                ]);
                (kv, next_room_info)
            });
            if let Some(next_room) = next_to_spawn {
                for (&bid, &tid) in next_room.npc_ball_ids.iter().zip(&next_room.npc_type_ids) {
                    if bid <= 0 {
                        continue;
                    }
                    let Some(spawn_guard) =
                        ctx.systems.begin_special_site_ball_spawn(caller_sys, bid).await
                    else {
                        continue;
                    };
                    let cs = build_special_site_npc_combat_state(&ctx.sde, tid);
                    match sys_handle
                        .spawn_ball_with_combat(
                            make_site_npc_ball(bid, tid, DVec3::new(10_000.0, 0.0, 0.0)),
                            cs,
                        )
                        .await
                    {
                        Ok(_) => spawn_guard.confirm(),
                        Err(err) => tracing::warn!(
                            system_id = caller_sys,
                            ball_id = bid,
                            error = %err,
                            "Failed spawning Drifter Hive room NPC ball"
                        ),
                    }
                }
                if next_room.beacon_ball_id > 0 {
                    if let Some(spawn_guard) = ctx
                        .systems
                        .begin_special_site_ball_spawn(caller_sys, next_room.beacon_ball_id)
                        .await
                    {
                        match sys_handle
                            .spawn_ball(make_site_npc_ball(
                                next_room.beacon_ball_id,
                                42_538,
                                DVec3::new(12_000.0, 0.0, 0.0),
                            ))
                            .await
                        {
                            Ok(_) => spawn_guard.confirm(),
                            Err(err) => tracing::warn!(
                                system_id = caller_sys,
                                ball_id = next_room.beacon_ball_id,
                                error = %err,
                                "Failed spawning Drifter Hive room beacon ball"
                            ),
                        }
                    }
                }
            }
            return Ok(kv);
        }

        if sys_handle.get_ball_snapshot(hive.warden_npc_ball_id).await.ok().flatten().is_some() {
            return Err(rpc_err("WaveNpcsRemaining", format!("Drifter Hive warden {} has not been destroyed", hive.warden_npc_ball_id)));
        }
        crate::services::r6_t1_p0_helpers::verify_special_site_caller_on_grid(
            ctx, session, &sys_handle, char_id, DVec3::new(10_000.0, 0.0, 0.0), &hive.dungeon_name,
        ).await?;
        let (kv, loot_items) = with_pve_special_reg(ctx, |reg| {
            let elem_t = reg.drifter_hives.get(&hive.solar_system_id).map(|h| h.element_type_id).unwrap_or(hive.element_type_id);
            let items = crate::services::pve_special_sites_ops::allocate_drifter_hive_bonus_loot(reg, elem_t);
            let h = reg.drifter_hives.get_mut(&hive.solar_system_id).unwrap();
            h.cleared = true;
            if let Some(r) = h.rooms.iter_mut().find(|r| r.room_index == h.total_rooms) {
                r.cleared = true;
            }
            let kv = PyRep::keyval([
                ("siteID", PyRep::String(h.site_id.clone())),
                ("dungeonID", PyRep::Integer(h.dungeon_id as i64)),
                ("dungeonName", PyRep::String(h.dungeon_name.clone())),
                ("solarSystemID", PyRep::Integer(h.solar_system_id as i64)),
                ("elementTypeID", PyRep::Integer(h.element_type_id as i64)),
                ("elementName", PyRep::String(h.element_name.clone())),
                ("wardenNpcBallID", PyRep::Integer(h.warden_npc_ball_id)),
                ("currentRoom", PyRep::Integer(h.total_rooms as i64)),
                ("totalRooms", PyRep::Integer(h.total_rooms as i64)),
                ("cleared", PyRep::Bool(true)),
            ]);
            (kv, items)
        });
        let ship_id = session.ship_id.unwrap_or(0);
        let spawned_cid = ctx.db.with_mut_snapshot(|snap| {
            deposit_loot_into_ship_or_container_in_snapshot(
                snap,
                &ctx.sde,
                char_id,
                caller_sys,
                ship_id,
                &loot_items,
            )
        });
        if let Some(cid) = spawned_cid {
            if let Err(err) = sys_handle
                .spawn_ball(make_site_npc_ball(cid, 23, DVec3::new(10_500.0, 0.0, 0.0)))
                .await
            {
                tracing::warn!(
                    system_id = caller_sys,
                    container_id = cid,
                    error = %err,
                    "Failed spawning Drifter Hive loot container ball"
                );
            }
        }
        return Ok(kv);
    }

    // 2. Check Homefront Operations via real multi-role archetype completion.
    if is_homefront_pve_completion_target(ctx, caller_sys, instance_id) {
        let (payouts, summary) =
            complete_homefront_from_special_dungeon(ctx, session, caller_sys, instance_id)?;
        let reward_paid = payouts
            .iter()
            .find(|&&(cid, _, _)| cid == char_id)
            .map(|p| p.1)
            .unwrap_or(0);
        let arch_key = get_homefront_runtime_state(ctx, instance_id)
            .map(|s| s.blueprint.archetype.as_str())
            .unwrap_or("Unknown");
        return Ok(PyRep::keyval([
            ("instanceID", PyRep::Integer(instance_id)),
            ("completed", PyRep::Bool(true)),
            ("rewardPaidIsk", PyRep::Integer(reward_paid)),
            ("totalFleetIsk", PyRep::Integer(summary.total_fleet_isk)),
            ("archetype", PyRep::String(arch_key.into())),
        ]));
    }

    // 3. Check Triglavian Sites (720_001..720_002)
    let trig_lookup = read_pve_special_reg(ctx, |reg| {
        reg.triglavian_sites
            .get(&caller_sys)
            .and_then(|list| {
                list.iter()
                    .enumerate()
                    .find(|(_, s)| s.instance_id == instance_id)
                    .map(|(idx, s)| (idx, s.clone()))
            })
            .or_else(|| {
                reg.triglavian_sites
                    .values()
                    .flatten()
                    .find(|s| s.instance_id == instance_id)
                    .map(|s| (0, s.clone()))
            })
    });
    if let Some((trig_idx, trig)) = trig_lookup {
        if caller_sys != trig.solar_system_id {
            return Err(SpecialSiteRpcErrorCode::NotInSiteSystem.build(format!(
                "Must be in system {} to complete {}",
                trig.solar_system_id, trig.dungeon_name
            )));
        }
        if trig.completed {
            return Err(SpecialSiteRpcErrorCode::SiteAlreadyCompleted.build(format!(
                "Triglavian site {} is already completed",
                trig.instance_id
            )));
        }
        let sys_handle = ctx.systems.get_handle(caller_sys).ok_or_else(|| {
            SpecialSiteRpcErrorCode::SiteNotSpawned.build(format!(
                "Triglavian site system {caller_sys} is not active"
            ))
        })?;
        spawn_special_site_balls(ctx, collect_triglavian_site_balls(std::slice::from_ref(&trig)))
            .await?;
        verify_triglavian_npcs_cleared(ctx, &sys_handle, &trig).await?;
        let trig_pos = DVec3::new(10_000.0 + (trig_idx as f64) * 20_000.0, 0.0, 0.0);
        crate::services::r6_t1_p0_helpers::verify_special_site_caller_on_grid(
            ctx,
            session,
            &sys_handle,
            char_id,
            trig_pos,
            &trig.dungeon_name,
        )
        .await?;

        let ship_id = session.ship_id.unwrap_or(0);
        let now_sec = ctx.now_unix();
        let (inst_id, base_isk, spawned_cid) =
            ctx.db.try_with_mut_snapshot(|snap| -> Result<_, BridgeRpcError> {
                if snap.pve_special_sites.sim_clock_sec < now_sec {
                    snap.pve_special_sites.sim_clock_sec = now_sec;
                }
                let (inst_id, dungeon_name, faction_owner_id, base_isk, survey_db_drop, escort_ids) = {
                    let site = snap
                        .pve_special_sites
                        .triglavian_sites
                        .get_mut(&caller_sys)
                        .and_then(|list| list.iter_mut().find(|s| s.instance_id == instance_id))
                        .ok_or_else(|| {
                            SpecialSiteRpcErrorCode::DungeonNotFound.build(format!(
                                "Dungeon instance {instance_id} not found"
                            ))
                        })?;
                    if site.solar_system_id != caller_sys {
                        return Err(SpecialSiteRpcErrorCode::NotInSiteSystem.build(format!(
                            "Must be in system {} to complete {}",
                            site.solar_system_id, site.dungeon_name
                        )));
                    }
                    if site.completed {
                        return Err(SpecialSiteRpcErrorCode::SiteAlreadyCompleted.build(format!(
                            "Triglavian site {} is already completed",
                            site.instance_id
                        )));
                    }
                    site.completed = true;
                    (
                        site.instance_id,
                        site.dungeon_name.clone(),
                        site.faction_owner_id,
                        site.base_isk,
                        site.survey_db_drop,
                        site.escort_ball_ids.clone(),
                    )
                };
                prune_triglavian_destroyed_escorts(snap, &escort_ids);
                let loot_iid = 1_899_500_000 + next_spec_seq(&mut snap.pve_special_sites);
                let nexus_iid = 1_899_600_000 + next_spec_seq(&mut snap.pve_special_sites);
                let (gain_faction, lose_faction) = if faction_owner_id == 500_027 {
                    (500_026_i64, 500_027_i64)
                } else {
                    (500_027_i64, 500_026_i64)
                };
                let (_, _standing_pair) =
                    crate::services::pve_special_sites_ops::apply_triglavian_standings_and_isk(
                        snap,
                        char_id,
                        gain_faction,
                        lose_faction,
                        base_isk,
                        &dungeon_name,
                    );
                let cid = deposit_loot_into_ship_or_container_in_snapshot(
                    snap,
                    &ctx.sde,
                    char_id,
                    caller_sys,
                    ship_id,
                    &[
                        (loot_iid, 48_121, survey_db_drop as i32),
                        (nexus_iid, 30_747, 2),
                    ],
                );
                Ok((inst_id, base_isk, cid))
            })?;

        if let Some(cid) = spawned_cid {
            if let Err(err) = sys_handle
                .spawn_ball(make_site_npc_ball(cid, 23, DVec3::new(10_500.0, 0.0, 0.0)))
                .await
            {
                tracing::warn!(
                    system_id = caller_sys,
                    container_id = cid,
                    error = %err,
                    "Failed spawning Triglavian site loot container ball"
                );
            }
        }
        return Ok(PyRep::keyval([
            ("instanceID", PyRep::Integer(inst_id)),
            ("completed", PyRep::Bool(true)),
            ("baseIsk", PyRep::Integer(base_isk)),
        ]));
    }

    Err(SpecialSiteRpcErrorCode::DungeonNotFound.build(format!(
        "Dungeon instance {instance_id} not found"
    )))
}

fn resolve_session_system_id(
    ctx: &ServerContext,
    session: &SessionState,
) -> Result<SolarSystemId, BridgeRpcError> {
    session
        .solar_system_id
        .or_else(|| {
            session.character_id.and_then(|cid| {
                ctx.db
                    .with_snapshot(|s| s.characters.get(&cid).map(|c| c.solar_system_id))
            })
        })
        .ok_or_else(|| rpc_err("NotInSpace", "Action requires an active solar system session"))
}

fn resolve_explicit_or_session_system_id(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
    method: &str,
) -> Result<SolarSystemId, BridgeRpcError> {
    if let Some(arg0) = args.first().filter(|a| !matches!(a, PyRep::None)) {
        let Some(raw) = arg0.as_i64() else {
            return Err(rpc_err(
                "UnknownSolarSystem",
                format!("{method} requires an integer solarSystemID"),
            ));
        };
        let Some(sid) = u32::try_from(raw)
            .ok()
            .filter(|id| ctx.sde.solar_systems.contains_key(id))
        else {
            return Err(rpc_err(
                "UnknownSolarSystem",
                format!("Unknown solar system ID {raw}"),
            ));
        };
        Ok(sid)
    } else {
        resolve_session_system_id(ctx, session)
    }
}

pub async fn handle_pve_special_sites(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    if !matches!(
        service,
        "incursion"
            | "incursionSvc"
            | "taleMgr"
            | "dungeonInstanceCacheMgr"
            | "map"
            | "solarsystemInterferenceMgr"
    ) {
        return None;
    }
    if (service, method) == ("dungeonInstanceCacheMgr", "GetHomefrontSiteInstances") {
        // parity: feat:pve.homefront-ops, rpc:dungeonInstanceCacheMgr.GetHomefrontSiteInstances
        return Some(handle_get_homefront_site_instances(ctx, session, args));
    }

    if let Some(res) = crate::services::pve_incursion_sites::handle_incursion_rpc(
        ctx, session, service, method, args, _kwargs,
    )
    .await
    {
        return Some(res);
    }

    match (service, method) {

        // parity: feat:pve.triglavian-sites, rpc:dungeonInstanceCacheMgr.GetTriglavianSiteInstances
        ("dungeonInstanceCacheMgr", "GetTriglavianSiteInstances") => {
            if let Err(e) = ServerContext::require_character(session) { return Some(Err(e)); }
            let sys_id = match resolve_explicit_or_session_system_id(
                ctx,
                session,
                args,
                "GetTriglavianSiteInstances",
            ) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };

            let sites = read_pve_special_reg(ctx, |reg| {
                reg.triglavian_sites
                    .get(&sys_id)
                    .cloned()
                    .unwrap_or_default()
            });

            let mut entries: Vec<(u32, i64)> = sites
                .into_iter()
                .filter(|s| !s.completed)
                .map(|s| (s.dungeon_id, s.instance_id))
                .collect();
            entries.sort_unstable_by_key(|&(_, instance_id)| instance_id);
            let items: Vec<PyRep> = entries
                .into_iter()
                .map(|(dungeon_id, instance_id)| {
                    PyRep::keyval([
                        ("dungeonID", PyRep::Integer(dungeon_id as i64)),
                        ("instanceID", PyRep::Integer(instance_id)),
                        ("solarSystemID", PyRep::Integer(sys_id as i64)),
                    ])
                })
                .collect();
            Some(Ok(PyRep::Dict(vec![(PyRep::Integer(sys_id as i64), PyRep::List(items))])))
        }

        ("dungeonInstanceCacheMgr", "GetSignatureInstances") => {
            handle_get_signature_instances(ctx, session, args).await
        }

        // Permanent Triglavian Minor Victory systems won in Chapter 3
        ("dungeonInstanceCacheMgr", "GetTriglavianMinorVictorySystems") => {
            handle_get_triglavian_minor_victory_systems(ctx, session)
        }

        // parity: feat:pve.metaliminal-storms, rpc:map.GetAllRoamingWeatherSystems
        ("map", "GetAllRoamingWeatherSystems") => {
            handle_get_all_roaming_weather_systems(ctx, session)
        }

        // parity: feat:pve.interference
        ("solarsystemInterferenceMgr", "GetLocalInterferenceState") => {
            handle_solarsystem_interference_rpc(ctx, session, "GetLocalInterferenceState", args)
        }
        ("solarsystemInterferenceMgr", "GetSolarSystemInterferenceBand") => {
            handle_solarsystem_interference_rpc(ctx, session, "GetSolarSystemInterferenceBand", args)
        }
        ("solarsystemInterferenceMgr", "GetAllInterferenceBands") => {
            handle_solarsystem_interference_rpc(ctx, session, "GetAllInterferenceBands", args)
        }

        _ => None,
    }
}

const INTERFERENCE_DECAY_TICK_PERIOD_SECS: i64 = 3_600;
const INTERFERENCE_DECAY_TICK_PERIOD_TICKS: i64 = INTERFERENCE_DECAY_TICK_PERIOD_SECS * 10_000_000;

fn unix_secs_to_filetime(unix_secs: u64) -> i64 {
    (unix_secs as i64 + 11_644_473_600).saturating_mul(10_000_000)
}

/// Builds the V24 `newInterferenceState` keyval shared by `GetLocalInterferenceState` and `OnSolarsystemInterferenceChanged`.
pub(crate) fn build_interference_state_keyval(sys_id: SolarSystemId, level: f64, band: u8) -> PyRep {
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(1_775_000_000);
    build_interference_state_keyval_at(sys_id, level, band, now_secs)
}

pub(crate) fn build_interference_state_keyval_at(
    sys_id: SolarSystemId,
    level: f64,
    band: u8,
    last_updated_sec: u64,
) -> PyRep {
    let clamped_level = level.clamp(0.0, 1.0);
    let band_label = match band {
        1 => "Low",
        2 => "Medium",
        _ => "High",
    };
    let crab_remaining = (((1.0 + 1e-6 - clamped_level).max(0.0))
        / crate::services::pve_crab_beacons::CRAB_INTERFERENCE_DELTA)
        .floor() as i64;
    let decrease_per_tick = (INTERFERENCE_DECAY_TICK_PERIOD_SECS as f64)
        / crate::services::pve_crab_beacons::INTERFERENCE_DECAY_WINDOW_SECS;
    let is_at_quiescent = clamped_level <= 1e-9;
    PyRep::keyval([
        ("solarSystemID", PyRep::Integer(sys_id as i64)),
        ("interferenceLevel", PyRep::Real(clamped_level)),
        ("interferencePct", PyRep::Real(clamped_level * 100.0)),
        ("interferenceBand", PyRep::Integer(band as i64)),
        ("bandLabel", PyRep::String(band_label.into())),
        ("crabActivationsRemaining", PyRep::Integer(crab_remaining)),
        ("isAtQuiescentLevel", PyRep::Bool(is_at_quiescent)),
        ("quiescentInterferenceLevel", PyRep::Real(0.0)),
        ("maxInterferenceLevel", PyRep::Real(1.0)),
        ("normalisedInterferenceLevel", PyRep::Real(clamped_level)),
        (
            "lastDecayTickTimestamp",
            PyRep::Integer(unix_secs_to_filetime(last_updated_sec)),
        ),
        (
            "decayTickPeriod",
            PyRep::Integer(INTERFERENCE_DECAY_TICK_PERIOD_TICKS),
        ),
        (
            "interferenceDecreasePerDecayTick",
            PyRep::Real(decrease_per_tick),
        ),
    ])
}

fn compute_decayed_interference(entry: &SystemInterferenceRecord, now_sec: u64) -> (f64, u8, Option<u8>) {
    if entry.last_updated_sec > 0 && now_sec > entry.last_updated_sec {
        let elapsed = now_sec - entry.last_updated_sec;
        let decay = (elapsed as f64) / INTERFERENCE_DECAY_WINDOW_SECS;
        let level = (entry.interference_level - decay).max(0.0);
        let band = if level >= 0.70 {
            3
        } else if level >= 0.35 {
            2
        } else {
            1
        };
        let changed = (band != entry.band).then_some(entry.band);
        (level, band, changed)
    } else {
        (entry.interference_level, entry.band, None)
    }
}

fn handle_solarsystem_interference_rpc(
    ctx: &ServerContext,
    session: &mut SessionState,
    method: &str,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    if let Err(e) = ServerContext::require_character(session) {
        return Some(Err(e));
    }
    let now_sec = ctx.now_unix();
    if method == "GetAllInterferenceBands" {
        if args.first().is_some_and(|a| !matches!(a, PyRep::None)) {
            if let Err(e) = resolve_explicit_or_session_system_id(ctx, session, args, method) {
                return Some(Err(e));
            }
        }
        let entries: Vec<(PyRep, PyRep)> = read_pve_special_reg(ctx, |reg| {
            let mut out: Vec<(SolarSystemId, u8)> = reg
                .interference_by_system
                .iter()
                .map(|(&sid, rec)| (sid, compute_decayed_interference(rec, now_sec).1))
                .collect();
            out.sort_unstable_by_key(|(sid, _)| *sid);
            out.into_iter()
                .map(|(sid, b)| (PyRep::Integer(sid as i64), PyRep::Integer(b as i64)))
                .collect()
        });
        return Some(Ok(PyRep::Dict(entries)));
    }
    let sys_id = match resolve_explicit_or_session_system_id(ctx, session, args, method) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };

    let (level, band, last_tick_sec) = read_pve_special_reg(ctx, |reg| {
        reg.interference_by_system
            .get(&sys_id)
            .map(|entry| {
                let (lvl, b, _) = compute_decayed_interference(entry, now_sec);
                let ts = if entry.last_updated_sec > 0 {
                    entry.last_updated_sec
                } else {
                    now_sec
                };
                (lvl, b, ts)
            })
            .unwrap_or((0.0, 1, now_sec))
    });

    Some(Ok(build_interference_state_keyval_at(
        sys_id,
        level,
        band,
        last_tick_sec,
    )))
}

pub fn check_drifter_hive_gate_access(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
    gate_id: i64,
) -> Result<(), BridgeRpcError> {
    read_pve_special_reg(ctx, |reg| {
        if let Some(hive) = reg.drifter_hives.get(&sys_id) {
            if let Some(room) = hive.rooms.iter().find(|r| r.beacon_ball_id == gate_id) {
                let remaining = room.npc_ball_ids.iter().filter(|id| !room.destroyed_npc_ball_ids.contains(id)).count();
                if !room.cleared && remaining > 0 {
                    return Err(rpc_err("RoomNotCleared", format!("Room {} NPCs must be cleared before activating acceleration gate", room.room_index)));
                }
            }
        }
        Ok(())
    })
}

async fn handle_get_signature_instances(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    if let Err(e) = ServerContext::require_character(session) {
        return Some(Err(e));
    }
    let sys_id = match resolve_explicit_or_session_system_id(
        ctx,
        session,
        args,
        "GetSignatureInstances",
    ) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    if (31_000_001..=31_000_005).contains(&sys_id) {
        let hive_opt = read_pve_special_reg(ctx, |reg| reg.drifter_hives.get(&sys_id).cloned());
        let sig_py = if let Some(hive) = hive_opt {
            let by_dng = vec![(
                PyRep::Integer(hive.dungeon_id as i64),
                PyRep::List(vec![PyRep::Integer(sys_id as i64)]),
            )];
            PyRep::Dict(vec![(PyRep::Integer(sys_id as i64), PyRep::Dict(by_dng))])
        } else {
            PyRep::Dict(Vec::new())
        };
        return Some(Ok(sig_py));
    }
    None
}

fn handle_get_triglavian_minor_victory_systems(
    ctx: &ServerContext,
    session: &SessionState,
) -> Option<Result<PyRep, BridgeRpcError>> {
    if let Err(e) = ServerContext::require_character(session) {
        return Some(Err(e));
    }
    let systems = read_pve_special_reg(ctx, |reg| {
        if reg.triglavian_minor_victory_systems.is_empty() {
            triglavian_minor_victory_systems()
        } else {
            reg.triglavian_minor_victory_systems.clone()
        }
    });
    Some(Ok(PyRep::List(
        systems.into_iter().map(|s| PyRep::Integer(s as i64)).collect(),
    )))
}

fn handle_get_all_roaming_weather_systems(
    ctx: &ServerContext,
    session: &SessionState,
) -> Option<Result<PyRep, BridgeRpcError>> {
    if let Err(e) = ServerContext::require_character(session) {
        return Some(Err(e));
    }
    let list = get_all_roaming_weather_systems(ctx);
    Some(Ok(PyRep::List(list)))
}

use crate::router::rpc_registry::{rpc_entry, RpcBoxFuture, RpcDispatchHandler, RpcEntry};

fn dispatch_rpc<'a>(
    ctx: &'a ServerContext,
    session: &'a mut SessionState,
    service: &'a str,
    method: &'a str,
    args: &'a [PyRep],
    kwargs: &'a HashMap<String, PyRep>,
) -> RpcBoxFuture<'a> {
    Box::pin(handle_pve_special_sites(ctx, session, service, method, args, kwargs))
}

pub(crate) const RPCS: &[RpcEntry] = &[
    rpc_entry("dungeonInstanceCacheMgr", "GetTriglavianSiteInstances", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("dungeonInstanceCacheMgr", "GetTriglavianMinorVictorySystems", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("map", "GetAllRoamingWeatherSystems", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("solarsystemInterferenceMgr", "GetLocalInterferenceState", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("solarsystemInterferenceMgr", "GetSolarSystemInterferenceBand", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("solarsystemInterferenceMgr", "GetAllInterferenceBands", RpcDispatchHandler::Async(dispatch_rpc)),
];

#[cfg(test)]
#[path = "pve_special_sites_tests.rs"]
mod pve_special_sites_tests;


