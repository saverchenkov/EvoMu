//! `pve_exploration` — PvE Cosmic Combat Anomalies, Combat Signatures, DED Rated Complexes,
//! Escalations & Expeditions, and Wormhole Sleeper Combat Sites (`Phase 8: W-3 & W-4`).
//!
// parity: feat:pve.combat-anomalies, feat:pve.combat-signatures, feat:pve.ded-complexes
// parity: feat:pve.escalations, feat:pve.expeditions, feat:pve.wormhole-sites

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::pve_hacking_sites::handle_pve_hacking;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::{SolarSystemId, TypeId};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub type CharacterId = i64;
pub type ItemId = i64;

mod tuple_map_serde {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::collections::HashMap;
    use std::hash::Hash;

    pub fn serialize<S, K1, K2, V>(
        map: &HashMap<(K1, K2), V>,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
        K1: Serialize + Copy,
        K2: Serialize + Copy,
        V: Serialize,
    {
        let entries: Vec<((K1, K2), &V)> = map.iter().map(|(&(k1, k2), v)| ((k1, k2), v)).collect();
        entries.serialize(serializer)
    }

    pub fn deserialize<'de, D, K1, K2, V>(deserializer: D) -> Result<HashMap<(K1, K2), V>, D::Error>
    where
        D: Deserializer<'de>,
        K1: Deserialize<'de> + Eq + Hash,
        K2: Deserialize<'de> + Eq + Hash,
        V: Deserialize<'de>,
    {
        let entries: Vec<((K1, K2), V)> = Vec::deserialize(deserializer)?;
        Ok(entries.into_iter().collect())
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PveSiteInstance {
    pub site_id: String,
    pub dungeon_id: u32,
    pub dungeon_name: String,
    pub solar_system_id: SolarSystemId,
    pub site_category: String,
    pub ded_rating: Option<u8>,
    pub wormhole_class: Option<u8>,
    pub position: [f64; 3],
    pub probe_required: bool,
    pub signal_strength: f64,
    pub current_room: u8,
    pub total_rooms: u8,
    pub current_wave: u8,
    pub total_waves: u8,
    pub active_npc_ball_ids: Vec<i64>,
    pub hackable_can_ids: Vec<ItemId>,
    pub npc_guards_spawned: bool,
    pub ghost_timer_started_at: Option<u64>,
    pub ghost_detonate_after_sec: u64,
    pub ghost_detonated: bool,
    pub sleeper_rift_stabilized: bool,
    pub sleeper_sentry_disabled: bool,
    pub sleeper_gas_cloud_active: bool,
    pub cleared: bool,
    #[serde(default)]
    pub created_unix: u64,
    #[serde(default)]
    pub lifetime_seconds: u64,
    #[serde(default)]
    pub expedition_instance_id: Option<i64>,
    /// In-space acceleration gate ball for DED complexes (`keeper.ActivateAccelerationGate(gate_item_id)`).
    #[serde(default)]
    pub gate_ball_id: Option<i64>,
    #[serde(default)]
    pub activated_at_downtime: Option<u64>,
    #[serde(default)]
    pub dread_escalation_triggers: u8,
    #[serde(default)]
    pub carrier_escalation_triggers: u8,
    #[serde(default)]
    pub active_escalation_capitals: Vec<crate::services::wormhole_pve_escalations::ActiveCapitalEscalationEntry>,
    #[serde(default)]
    pub is_shattered_resource_site: bool,
    #[serde(default)]
    pub delayed_wave_triggered: bool,
    #[serde(default)]
    pub relay_spawned: bool,
}

pub use crate::services::pve_expeditions::EscalatingPathEntry;
pub use crate::services::pve_sleeper_caches::{
    sleeper_cache_ship_restrictions, sleeper_cache_tier_for_dungeon,
    validate_sleeper_cache_passage, SleeperCacheTier,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum HackingNodeKind {
    Empty,
    SystemCore,
    Firewall,
    Antivirus,
    RestorationNode,
    VirusSuppressor,
    UtilitySubsystem,
    DataCache,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HackingUtilityKind {
    SelfRepair = 1,
    KernelRot = 2,
    PolymorphicShield = 3,
    SecondaryVector = 4,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HackingTileState {
    pub tile_index: u32,
    pub revealed: bool,
    pub blocked_by_defensive: bool,
    pub kind: HackingNodeKind,
    pub coherence: i32,
    pub strength: i32,
    pub utility_drop: Option<HackingUtilityKind>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HackingGameSession {
    pub game_id: i64,
    pub character_id: CharacterId,
    pub solar_system_id: SolarSystemId,
    pub container_id: ItemId,
    pub site_id: String,
    pub is_relic: bool,
    pub is_ghost_site: bool,
    pub is_sleeper_cache_puzzle: bool,
    pub virus_coherence: i32,
    pub max_virus_coherence: i32,
    pub base_virus_strength: i32,
    pub polymorphic_shield_charges: u8,
    pub secondary_vector_target: Option<(u32, u8)>,
    pub utilities: Vec<Option<HackingUtilityKind>>,
    pub tiles: Vec<HackingTileState>,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpewedLootCan {
    pub can_item_id: ItemId,
    pub parent_container_id: ItemId,
    pub solar_system_id: SolarSystemId,
    pub position: [f64; 3],
    pub loot_type_id: TypeId,
    pub loot_quantity: i64,
    pub spawned_at_sec: u64,
    pub despawn_at_sec: u64,
    pub scooped: bool,
}

#[derive(Default, Serialize, Deserialize)]
pub struct PveExplorationRegistry {
    #[serde(skip)]
    pub db_weak: std::sync::Weak<eve_db::WorldRepository>,
    #[serde(skip)]
    pub current_now_sec: u64,
    pub seeded_systems: HashMap<SolarSystemId, bool>,
    pub sites_by_system: HashMap<SolarSystemId, Vec<PveSiteInstance>>,
    pub escalations_by_char: HashMap<CharacterId, Vec<EscalatingPathEntry>>,
    #[serde(default, with = "tuple_map_serde")]
    pub hacking_games: HashMap<(CharacterId, ItemId), HackingGameSession>,
    pub spewed_cans: HashMap<ItemId, SpewedLootCan>,
    #[serde(default)]
    pub hacked_containers: std::collections::HashSet<(SolarSystemId, ItemId)>,
    #[serde(default, with = "tuple_map_serde")]
    pub failure_counts: HashMap<(SolarSystemId, ItemId), u8>,
    #[serde(default, with = "tuple_map_serde")]
    pub structure_char_failures: HashMap<(CharacterId, ItemId), u8>,
    pub sim_clock_sec: u64,
    pub next_id_seq: i64,
    #[serde(default)]
    pub downtime_counter: u64,
    #[serde(default)]
    pub active_relays: HashMap<i64, crate::services::wormhole_pve_escalations::DecloakedRelayInfo>,
    #[serde(default)]
    pub sentry_last_fire_unix: HashMap<i64, f64>,
    #[serde(default)]
    pub sentry_last_fire_sim_s: HashMap<i64, f64>,
}
pub type HackingSiteRegistry = PveExplorationRegistry;
const PVE_EXPLORATION_NOTE_ID: i64 = -8_200_000_000_000;

pub fn ctx_pve_key(ctx: &ServerContext) -> usize {
    std::sync::Arc::as_ptr(&ctx.db) as usize
}

pub fn with_pve_exploration_reg_db<R>(
    db: &std::sync::Arc<eve_db::WorldRepository>,
    now_unix: u64,
    f: impl FnOnce(&mut PveExplorationRegistry) -> R,
) -> R {
    let (mut reg, orig_json) = db.with_snapshot(|snap| {
        if let Some(note) = snap.owner_notes.get(&PVE_EXPLORATION_NOTE_ID) {
            if let Ok(parsed) = serde_json::from_str::<PveExplorationRegistry>(&note.note) {
                return (parsed, note.note.clone());
            }
        }
        (PveExplorationRegistry::default(), String::new())
    });
    reg.db_weak = std::sync::Arc::downgrade(db);
    reg.current_now_sec = now_unix;
    let out = f(&mut reg);
    if let Ok(new_json) = serde_json::to_string(&reg) {
        if new_json != orig_json {
            db.with_mut_snapshot(|snap| {
                snap.owner_notes.insert(
                    PVE_EXPLORATION_NOTE_ID,
                    eve_db::OwnerNoteRecord {
                        note_id: PVE_EXPLORATION_NOTE_ID,
                        character_id: -82,
                        item_id: 0,
                        label: "pve_exploration_registry".into(),
                        note: new_json,
                        timestamp_unix: reg.sim_clock_sec,
                    },
                );
            });
        }
    }
    out
}

pub fn with_pve_exploration_reg<R>(
    ctx: &ServerContext,
    f: impl FnOnce(&mut PveExplorationRegistry) -> R,
) -> R {
    with_pve_exploration_reg_db(&ctx.db, ctx.now_unix(), f)
}

pub fn next_pve_seq(reg: &mut PveExplorationRegistry) -> i64 {
    reg.next_id_seq += 1;
    reg.next_id_seq
}

pub fn now_sec(reg: &PveExplorationRegistry) -> u64 {
    let base = if reg.current_now_sec > 0 {
        reg.current_now_sec
    } else {
        1_775_000_000
    };
    base.max(reg.sim_clock_sec)
}

pub fn advance_pve_exploration_clock(ctx: &ServerContext, delta_sec: u64) {
    let cur_now = ctx.now_unix();
    with_pve_exploration_reg(ctx, |reg| {
        if reg.sim_clock_sec < cur_now {
            reg.sim_clock_sec = cur_now;
        }
        reg.sim_clock_sec += delta_sec;
    });
}

// parity: feat:pve.wormhole-sites
pub fn wormhole_class_for_system(solar_system_id: SolarSystemId) -> Option<u8> {
    let wh_c = crate::services::wormhole_lifecycle_ops::system_wormhole_profile(solar_system_id).0;
    match wh_c {
        1..=6 | 12..=18 => Some(wh_c),
        _ => None,
    }
}

pub use crate::services::pve_exploration_seeding::{
    anomaly_dungeons_for_system, build_wave_npc_balls, ded_complex_for_rating,
    ensure_system_pve_sites_seeded, pirate_faction_for_system, PirateFaction,
};
pub use crate::services::pve_exploration_sites::ded_ship_restriction_for_rating;
pub use crate::services::wormhole_pve_escalations::{
    drop_site_loot_into_space, drop_site_loot_into_space_core,
};

/// Authoritative real in-space NPC destruction hook for PvE exploration sites (`feat:pve.escalations`,
/// `feat:pve.expeditions`, `feat:pve.ded-complexes`, `feat:pve.wormhole-sites`).
///
/// Called from the system-actor `npc_death_hook` (in-space damage) and from
/// `destroy_specialized_npc_and_queue_bounty`. Wormhole Sleeper sites (`WormholeCombat`) drop a Sleeper
/// wreck with SDE Blue Loot per kill (when `drop_npc_wreck`) and spawn the next wave as soon as the wave
/// trigger NPC (first active ball) dies. DED rooms only unlock their acceleration gate when cleared; the
/// final-room kill spawns the Overseer wreck. Other sites auto-advance waves. Site completion rolls
/// escalations (15-20%) and advances expeditions without invented client RPCs.
#[allow(clippy::too_many_arguments)]
pub async fn handle_pve_site_npc_destruction_core(
    systems: &std::sync::Arc<crate::system_actor::SystemRegistry>,
    db: &std::sync::Arc<eve_db::WorldRepository>,
    sde: &std::sync::Arc<eve_sde::SdeRegistry>,
    rng: &std::sync::Arc<crate::rng::MechanicRng>,
    combat_ephemeral: &std::sync::Arc<std::sync::RwLock<crate::services::combat_crimewatch::CombatRuntimeState>>,
    solar_system_id: SolarSystemId,
    npc_ball_id: i64,
    killer_char_id: i64,
    drop_npc_wreck: bool,
) -> (Option<String>, Option<EscalatingPathEntry>, Option<EscalatingPathEntry>) {
    let now_unix = db.clock().now_unix_secs();
    let site_info = with_pve_exploration_reg_db(db, now_unix, |reg| {
        let sites = reg.sites_by_system.get_mut(&solar_system_id)?;
        for site in sites.iter_mut() {
            if let Some(pos) = site.active_npc_ball_ids.iter().position(|&bid| bid == npc_ball_id) {
                site.active_npc_ball_ids.remove(pos);
                let wave_cleared = site.active_npc_ball_ids.is_empty();
                return Some((
                    site.site_id.clone(),
                    site.dungeon_id,
                    site.dungeon_name.clone(),
                    site.site_category.clone(),
                    site.ded_rating,
                    site.wormhole_class,
                    site.position,
                    (site.current_wave, site.total_waves),
                    wave_cleared,
                    pos == 0,
                    site.expedition_instance_id,
                ));
            }
        }
        None
    });

    let Some((
        site_id,
        dungeon_id,
        dungeon_name,
        site_category,
        ded_rating,
        wh_class,
        pos,
        (current_wave, total_waves),
        wave_cleared,
        was_wave_trigger,
        expedition_instance_id,
    )) = site_info else {
        return (None, None, None);
    };

    let sleeper_class = wh_class.filter(|_| site_category == "WormholeCombat");
    if let Some(wh) = sleeper_class {
        if drop_npc_wreck {
            let wreck_pos = glam::DVec3::from_array(pos) + glam::DVec3::new(0.0, 0.0, 1_000.0);
            crate::services::pve_exploration_sites::drop_sleeper_wreck_core(
                db, systems, sde, solar_system_id, killer_char_id, wreck_pos, wh,
            ).await;
        }
        if was_wave_trigger && current_wave < total_waves {
            let next_wave = current_wave + 1;
            let new_ids = crate::services::pve_exploration_sites::spawn_sleeper_wave_core(
                systems, db, sde, combat_ephemeral, solar_system_id, wh, next_wave, glam::DVec3::from_array(pos),
            ).await;
            with_pve_exploration_reg_db(db, now_unix, |reg| {
                if let Some(site) = reg
                    .sites_by_system
                    .get_mut(&solar_system_id)
                    .and_then(|l| l.iter_mut().find(|s| s.site_id == site_id))
                {
                    site.current_wave = next_wave;
                    let leftovers = std::mem::take(&mut site.active_npc_ball_ids);
                    site.active_npc_ball_ids = new_ids.into_iter().chain(leftovers).collect();
                }
            });
            return (None, None, None);
        }
    }

    if !wave_cleared {
        return (None, None, None);
    }
    // DED rooms advance through `keeper.ActivateAccelerationGate`; clearing a room only unlocks its gate.
    if ded_rating.is_some() && current_wave < total_waves {
        return (None, None, None);
    }

    let sys_handle = systems.get_or_spawn(solar_system_id);

    if current_wave < total_waves {
        let next_wave = current_wave + 1;
        let spawned_ids = if let Some(wh) = sleeper_class {
            crate::services::pve_exploration_sites::spawn_sleeper_wave_core(
                systems, db, sde, combat_ephemeral, solar_system_id, wh, next_wave, glam::DVec3::from_array(pos),
            ).await
        } else {
            let mut ids = Vec::new();
            for b in build_wave_npc_balls(sde, solar_system_id, dungeon_id, next_wave, pos, wh_class) {
                ids.push(b.id);
                let cs = crate::services::pve_exploration_seeding::build_type_combat_state_from_sde(sde, b.type_id);
                let _ = sys_handle.spawn_ball_with_combat(b, cs).await;
            }
            ids
        };
        with_pve_exploration_reg_db(db, now_unix, |reg| {
            if let Some(site) = reg
                .sites_by_system
                .get_mut(&solar_system_id)
                .and_then(|l| l.iter_mut().find(|s| s.site_id == site_id))
            {
                site.current_wave = next_wave;
                site.active_npc_ball_ids = spawned_ids;
            }
        });
        (None, None, None)
    } else {
        let has_unhacked_cans = matches!(site_category.as_str(), "Data" | "Relic")
            && with_pve_exploration_reg_db(db, now_unix, |reg| {
                let hacked = &reg.hacked_containers;
                reg.sites_by_system
                    .get(&solar_system_id)
                    .and_then(|l| l.iter().find(|s| s.site_id == site_id))
                    .is_some_and(|s| {
                        !s.hackable_can_ids.is_empty()
                            && s.hackable_can_ids
                                .iter()
                                .any(|cid| !hacked.contains(&(solar_system_id, *cid)))
                    })
            });
        if has_unhacked_cans {
            return (None, None, None);
        }
        with_pve_exploration_reg_db(db, now_unix, |reg| {
            if let Some(site) = reg
                .sites_by_system
                .get_mut(&solar_system_id)
                .and_then(|l| l.iter_mut().find(|s| s.site_id == site_id))
            {
                site.cleared = true;
            }
        });

        db.with_mut_snapshot(|snap| {
            if let Some(anoms) = snap.cosmic_anomalies.get_mut(&solar_system_id) {
                if let Some(a) = anoms.iter_mut().find(|a| a.anomaly_id == site_id) {
                    a.cleared = true;
                }
            }
            if let Some(sigs) = snap.cosmic_signatures.get_mut(&solar_system_id) {
                if let Some(s) = sigs.iter_mut().find(|s| s.signature_id == site_id) {
                    s.collapsed = true;
                }
            }
        });

        if let Some(rating) = ded_rating {
            crate::services::pve_exploration_sites::spawn_ded_overseer_wreck_core(
                db,
                systems,
                sde,
                solar_system_id,
                killer_char_id,
                glam::DVec3::from_array(pos),
                rating,
            ).await;
        } else if let Some(wh_c) = wh_class {
            // Site-completion Blue Loot from SDE group 880 "Sleeper Components" (class-scaled pick).
            if let Some((blue_tid, _)) = crate::services::pve_exploration_sites::sleeper_blue_loot_for_class(sde, wh_c) {
                drop_site_loot_into_space_core(db, systems, solar_system_id, killer_char_id, pos, blue_tid, (wh_c as i64) * 3).await;
            }
            if matches!(wh_c, 5 | 6)
                && matches!(site_category.as_str(), "WormholeCombat" | "Data" | "Relic")
            {
                let _ = crate::services::wormhole_pve_escalations::spawn_decloaked_transmission_relay_core(
                    systems, db, sde, combat_ephemeral, solar_system_id, &site_id, pos,
                ).await;
            }
        } else if site_category == "CombatAnomaly" || site_category == "CombatSignature" {
            let (cmd_tid, _) = crate::services::pve_exploration_seeding::pirate_commander_loot_for_system(sde, solar_system_id);
            drop_site_loot_into_space_core(db, systems, solar_system_id, killer_char_id, pos, cmd_tid, 1).await;
        }

        let mut new_escalation = None;
        let mut advanced_step = None;

        if let Some(inst_id) = expedition_instance_id {
            advanced_step = crate::services::pve_expeditions::advance_expedition_step_core(
                db,
                sde,
                systems,
                killer_char_id,
                inst_id,
            ).await.ok().flatten();
        } else if matches!(site_category.as_str(), "CombatAnomaly" | "CombatSignature" | "DED") {
            let should_escalate = rng.roll(0.20);
            if should_escalate {
                new_escalation = crate::services::pve_expeditions::create_escalation_expedition_core(
                    db,
                    sde,
                    systems,
                    killer_char_id,
                    solar_system_id,
                    dungeon_id,
                    &dungeon_name,
                    pos,
                ).await;
            }
        }
        (Some(site_id), new_escalation, advanced_step)
    }
}

pub fn notify_pve_destruction_events(
    db: &eve_db::WorldRepository,
    pending_notifications: &std::sync::RwLock<std::collections::HashMap<i64, Vec<crate::router::cache_notifications::PendingMachoNotification>>>,
    notify_tx: &tokio::sync::broadcast::Sender<(i64, crate::router::cache_notifications::PendingMachoNotification)>,
    killer_char_id: i64,
    cleared_site_id: Option<String>,
    new_escalation: Option<EscalatingPathEntry>,
    advanced_step: Option<EscalatingPathEntry>,
) {
    let now_ft = db.clock().now_blueos_filetime_ticks();
    let mut to_send = Vec::new();

    if let Some(site_id) = cleared_site_id {
        let cleared_did_opt = db.with_snapshot(|snap| {
            snap.owner_notes
                .get(&PVE_EXPLORATION_NOTE_ID)
                .and_then(|note| serde_json::from_str::<PveExplorationRegistry>(&note.note).ok())
                .and_then(|reg| {
                    reg.sites_by_system
                        .values()
                        .flatten()
                        .find(|s| s.site_id == site_id)
                        .map(|s| s.dungeon_id)
                })
        });
        crate::services::fleet_social::fleet_social_scanning::emit_signal_tracker_site_removed_with_db(
            db, pending_notifications, notify_tx, &site_id,
        );
        let (packet, marshaled_packet) = crate::router::ServerContext::build_macho_notification_packet(
            killer_char_id,
            None,
            "OnCosmicAnomalyRemoved",
            "charid",
            vec![PyRep::String(site_id.clone())],
        );
        to_send.push(crate::router::cache_notifications::PendingMachoNotification {
            recipient_char_id: killer_char_id,
            event: "OnCosmicAnomalyRemoved".to_string(),
            id_type: "charid".to_string(),
            args: vec![PyRep::String(site_id)],
            packet,
            marshaled_packet,
            created_filetime: now_ft,
        });
        if let Some(did) = cleared_did_opt {
            let d_args = vec![PyRep::Integer(i64::from(did))];
            let (d_pkt, d_mpkt) = crate::router::ServerContext::build_macho_notification_packet(
                killer_char_id,
                None,
                "OnDungeonCompleted",
                "charid",
                d_args.clone(),
            );
            to_send.push(crate::router::cache_notifications::PendingMachoNotification {
                recipient_char_id: killer_char_id,
                event: "OnDungeonCompleted".to_string(),
                id_type: "charid".to_string(),
                args: d_args,
                packet: d_pkt,
                marshaled_packet: d_mpkt,
                created_filetime: now_ft,
            });
        }
    }

    let mut fleet_to_send: Vec<(i64, crate::router::cache_notifications::PendingMachoNotification)> = Vec::new();
    if let Some(ref esc) = new_escalation {
        let fleet_info = db.with_snapshot(|snap| {
            let fid = snap.characters.get(&killer_char_id).and_then(|c| c.fleet_id)?;
            if fid <= 0 {
                return None;
            }
            let members = snap
                .fleets
                .get(&fid)
                .map(|fl| fl.members.clone())
                .unwrap_or_default();
            Some((fid, members))
        });
        if let Some((fid, members)) = fleet_info {
            let f_args = vec![
                PyRep::Integer(killer_char_id),
                PyRep::Integer(i64::from(esc.expedition_dungeon_id)),
            ];
            let (f_pkt, f_mpkt) = crate::router::ServerContext::build_macho_notification_packet(
                fid,
                None,
                "OnFleetEscalatingPathMessage",
                "fleetid",
                f_args.clone(),
            );
            for member_id in members {
                fleet_to_send.push((
                    member_id,
                    crate::router::cache_notifications::PendingMachoNotification {
                        recipient_char_id: member_id,
                        event: "OnFleetEscalatingPathMessage".to_string(),
                        id_type: "fleetid".to_string(),
                        args: f_args.clone(),
                        packet: f_pkt.clone(),
                        marshaled_packet: f_mpkt.clone(),
                        created_filetime: now_ft,
                    },
                ));
            }
        }
    }

    for esc in [new_escalation, advanced_step].into_iter().flatten() {
        let (chg_pkt, chg_mpkt) = crate::router::ServerContext::build_macho_notification_packet(
            killer_char_id,
            None,
            "OnEscalatingPathChange",
            "charid",
            vec![],
        );
        to_send.push(crate::router::cache_notifications::PendingMachoNotification {
            recipient_char_id: killer_char_id,
            event: "OnEscalatingPathChange".to_string(),
            id_type: "charid".to_string(),
            args: vec![],
            packet: chg_pkt,
            marshaled_packet: chg_mpkt,
            created_filetime: now_ft,
        });
        let msg_args = vec![PyRep::Integer(esc.instance_id)];
        let (msg_pkt, msg_mpkt) = crate::router::ServerContext::build_macho_notification_packet(
            killer_char_id,
            None,
            "OnEscalatingPathMessage",
            "charid",
            msg_args.clone(),
        );
        to_send.push(crate::router::cache_notifications::PendingMachoNotification {
            recipient_char_id: killer_char_id,
            event: "OnEscalatingPathMessage".to_string(),
            id_type: "charid".to_string(),
            args: msg_args,
            packet: msg_pkt,
            marshaled_packet: msg_mpkt,
            created_filetime: now_ft,
        });
    }

    if !to_send.is_empty() || !fleet_to_send.is_empty() {
        let mut queues = pending_notifications.write().unwrap_or_else(|e| e.into_inner());
        let q = queues.entry(killer_char_id).or_default();
        for notif in to_send {
            if q.len() >= 256 {
                q.remove(0);
            }
            q.push(notif.clone());
            let _ = notify_tx.send((killer_char_id, notif));
        }
        for (member_id, notif) in fleet_to_send {
            let mq = queues.entry(member_id).or_default();
            if mq.len() >= 256 {
                mq.remove(0);
            }
            mq.push(notif.clone());
            let _ = notify_tx.send((member_id, notif));
        }
    }
}

pub fn pve_site_category_and_dungeon_id(
    db: &std::sync::Arc<eve_db::WorldRepository>,
    now_unix: u64,
    solar_system_id: SolarSystemId,
    site_id: &str,
) -> (String, u32) {
    with_pve_exploration_reg_db(db, now_unix, |reg| {
        reg.sites_by_system
            .get(&solar_system_id)
            .and_then(|l| l.iter().find(|s| s.site_id == site_id))
            .map(|s| (s.site_category.clone(), s.dungeon_id))
            .unwrap_or_default()
    })
}

pub async fn handle_pve_site_npc_destruction(
    ctx: &ServerContext,
    solar_system_id: SolarSystemId,
    npc_ball_id: i64,
    killer_char_id: i64,
) {
    // `destroy_specialized_npc_and_queue_bounty` already dropped this NPC's wreck, so no extra Sleeper wreck.
    let (cleared_site_id, new_escalation, advanced_step) = handle_pve_site_npc_destruction_core(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        &ctx.mechanic_rng,
        &ctx.combat_ephemeral,
        solar_system_id,
        npc_ball_id,
        killer_char_id,
        false,
    ).await;

    if let Some(ref sid) = cleared_site_id {
        let now = ctx.now_unix();
        let (cat, did) = pve_site_category_and_dungeon_id(&ctx.db, now, solar_system_id, sid);
        ctx.quasar_gateway.progression.extra.on_system_tale_encounter_completed(solar_system_id, &cat, did, 1, now, &ctx.quasar_gateway);
    }
    notify_pve_destruction_events(
        &ctx.db,
        &ctx.pending_notifications,
        &ctx.notify_tx,
        killer_char_id,
        cleared_site_id,
        new_escalation,
        advanced_step,
    );
    crate::services::pve_dungeon_runtime::on_dungeon_npc_killed(
        ctx,
        solar_system_id,
        npc_ball_id,
        killer_char_id,
    )
    .await;
}

// parity: feat:pve.combat-anomalies, feat:pve.ded-complexes, feat:pve.escalations, feat:pve.expeditions, feat:pve.wormhole-sites
pub async fn advance_site_wave_or_complete(
    ctx: &ServerContext,
    session: &mut SessionState,
    solar_system_id: SolarSystemId,
    site_id: &str,
) -> Result<PyRep, BridgeRpcError> {
    let char_id = session.character_id.unwrap_or(90_000_001);
    if session.station_id.is_some() || session.structure_id.is_some() {
        return Err(BridgeRpcError {
            code: "ShipDocked".into(),
            message: format!("Cannot advance PvE site {site_id} while docked"),
        });
    }
    if session.solar_system_id != Some(solar_system_id) {
        return Err(BridgeRpcError {
            code: "NotInSiteSystem".into(),
            message: format!("Caller must be in solar system {solar_system_id} to advance site {site_id}"),
        });
    }
    ensure_system_pve_sites_seeded(ctx, solar_system_id).await;

    let (current_balls, is_sleeper_cache_stabilized) = with_pve_exploration_reg(ctx, |reg| {
        let sites = reg.sites_by_system.entry(solar_system_id).or_default();
        let Some(site) = sites
            .iter()
            .find(|s| s.site_id == site_id || s.dungeon_id.to_string() == site_id)
        else {
            return Err(BridgeRpcError {
                code: "SiteNotFound".into(),
                message: format!("PvE site {site_id} not found in system {solar_system_id}"),
            });
        };
        if site.cleared {
            return Err(BridgeRpcError {
                code: "SiteAlreadyCleared".into(),
                message: format!("PvE site {site_id} is already cleared"),
            });
        }
        if matches!(site.site_category.as_str(), "Data" | "Relic" | "Ghost")
            && !site.hackable_can_ids.is_empty()
            && !site
                .hackable_can_ids
                .iter()
                .all(|&cid| reg.hacked_containers.contains(&(solar_system_id, cid)))
        {
            return Err(BridgeRpcError {
                code: "SiteContainersUnhacked".into(),
                message: format!("Cannot complete exploration site {site_id} while hackable containers remain unhacked"),
            });
        }
        Ok((
            site.active_npc_ball_ids.clone(),
            site.site_category == "SleeperCache" && site.sleeper_rift_stabilized,
        ))
    })?;

    let sys_handle = ctx.systems.get_or_spawn(solar_system_id);
    if !is_sleeper_cache_stabilized {
        for &bid in &current_balls {
            if sys_handle.get_ball_snapshot(bid).await.ok().flatten().is_some() {
                return Err(BridgeRpcError {
                    code: "WaveNpcsRemaining".into(),
                    message: format!("Cannot advance site {site_id}: wave NPC ball {bid} has not been destroyed"),
                });
            }
        }
    }

    let (
        dungeon_id,
        dungeon_name,
        site_category,
        ded_rating,
        wh_class,
        pos,
        new_wave,
        total_waves,
        old_balls,
        completed_now,
        exp_inst_id,
    ) = with_pve_exploration_reg(ctx, |reg| {
        let sites = reg.sites_by_system.entry(solar_system_id).or_default();
        let Some(site) = sites
            .iter_mut()
            .find(|s| s.site_id == site_id || s.dungeon_id.to_string() == site_id)
        else {
            return Err(BridgeRpcError {
                code: "SiteNotFound".into(),
                message: format!("PvE site {site_id} not found in system {solar_system_id}"),
            });
        };
        let old = std::mem::take(&mut site.active_npc_ball_ids);
        let done = site.current_wave >= site.total_waves;
        if done {
            site.cleared = true;
        } else {
            site.current_wave += 1;
        }
        Ok((
            site.dungeon_id,
            site.dungeon_name.clone(),
            site.site_category.clone(),
            site.ded_rating,
            site.wormhole_class,
            site.position,
            site.current_wave,
            site.total_waves,
            old,
            done,
            site.expedition_instance_id,
        ))
    })?;

    for bid in old_balls {
        let _ = sys_handle.remove_ball(bid).await;
    }

    let mut spawned_npc_ids = Vec::new();
    let mut dropped_loot_items = Vec::new();
    let mut escalation_created: Option<EscalatingPathEntry> = None;

    if !completed_now {
        let next_balls = build_wave_npc_balls(&ctx.sde, solar_system_id, dungeon_id, new_wave, pos, wh_class);
        for b in next_balls {
            spawned_npc_ids.push(b.id);
            let cs = crate::services::pve_exploration_seeding::build_type_combat_state_from_sde(&ctx.sde, b.type_id);
            let _ = sys_handle.spawn_ball_with_combat(b, cs).await;
        }
        with_pve_exploration_reg(ctx, |reg| {
            if let Some(site) = reg
                .sites_by_system
                .get_mut(&solar_system_id)
                .and_then(|l| l.iter_mut().find(|s| s.site_id == site_id || s.dungeon_id == dungeon_id))
            {
                site.active_npc_ball_ids = spawned_npc_ids.clone();
            }
        });
    } else {
        let mut loot_specs: Vec<(TypeId, i64, String)> = Vec::new();

        // DED Overseer wrecks are only dropped by the real overseer kill in `handle_pve_site_npc_destruction_core`.
        if let Some(wh_c) = wh_class.filter(|_| {
            ded_rating.is_none()
                && !matches!(site_category.as_str(), "Data" | "Relic" | "Ghost")
        }) {
            // Site-completion Blue Loot from SDE group 880 "Sleeper Components".
            if let Some((blue_tid, _)) = crate::services::pve_exploration_sites::sleeper_blue_loot_for_class(&ctx.sde, wh_c) {
                let blue_name = ctx.sde.types.get(&blue_tid).map(|t| t.name.clone()).unwrap_or_default();
                loot_specs.push((blue_tid, (wh_c as i64) * 3, blue_name));
            }
        } else if matches!(site_category.as_str(), "CombatAnomaly" | "CombatSignature") {
            let (cmd_tid, cmd_name) = crate::services::pve_exploration_seeding::pirate_commander_loot_for_system(&ctx.sde, solar_system_id);
            loot_specs.push((cmd_tid, 1, cmd_name.to_string()));
        }

        for (l_tid, l_qty, l_name) in loot_specs {
            let item_id = drop_site_loot_into_space(ctx, solar_system_id, char_id, pos, l_tid, l_qty).await;
            dropped_loot_items.push((item_id, l_tid, l_qty, l_name));
        }

        if let Some(5 | 6) = wh_class {
            if matches!(site_category.as_str(), "WormholeCombat" | "Data" | "Relic") {
                let _ = crate::services::wormhole_pve_escalations::spawn_decloaked_transmission_relay_core(
                    &ctx.systems,
                    &ctx.db,
                    &ctx.sde,
                    &ctx.combat_ephemeral,
                    solar_system_id,
                    site_id,
                    pos,
                ).await;
            }
        }

        let should_escalate = matches!(site_category.as_str(), "CombatAnomaly" | "CombatSignature" | "DED")
            && crate::services::pve_expeditions::roll_escalation_chance(ctx);

        if let Some(inst_id) = exp_inst_id {
            let _ = crate::services::pve_expeditions::advance_expedition_step(ctx, char_id, inst_id).await;
        } else if should_escalate {
            escalation_created = crate::services::pve_expeditions::create_escalation_expedition(
                ctx,
                char_id,
                solar_system_id,
                dungeon_id,
                &dungeon_name,
                pos,
            ).await;
        }

        let (was_anom, was_sig) = ctx.db.with_mut_snapshot(|snap| {
            let mut a_hit = false;
            let mut s_hit = false;
            if let Some(anoms) = snap.cosmic_anomalies.get_mut(&solar_system_id) {
                if let Some(a) = anoms.iter_mut().find(|a| a.anomaly_id == site_id && !a.cleared) {
                    a.cleared = true;
                    a_hit = true;
                }
            }
            if let Some(sigs) = snap.cosmic_signatures.get_mut(&solar_system_id) {
                if let Some(s) = sigs.iter_mut().find(|s| s.signature_id == site_id && !s.collapsed) {
                    s.collapsed = true;
                    s_hit = true;
                }
            }
            (a_hit, s_hit)
        });
        if was_anom {
            crate::services::fleet_social::fleet_social_scanning::emit_signal_tracker_anomaly_delta(ctx, solar_system_id, &[], &[site_id.to_string()]);
        }
        if was_sig {
            crate::services::fleet_social::fleet_social_scanning::emit_signal_tracker_signature_delta(ctx, solar_system_id, &[], &[site_id.to_string()]);
        }

        session.push_notification("OnCosmicAnomalyRemoved", vec![PyRep::String(site_id.to_string())]);
        ctx.quasar_gateway.progression.extra.on_system_tale_encounter_completed(
            solar_system_id, &site_category, dungeon_id, 1, ctx.now_unix(), &ctx.quasar_gateway,
        );
        ctx.notify(
            crate::router::NotificationTarget::Character(char_id),
            "OnDungeonCompleted",
            "charid",
            vec![PyRep::Integer(i64::from(dungeon_id))],
        );
    }

    let loot_py: Vec<PyRep> = dropped_loot_items
        .iter()
        .map(|(iid, tid, qty, name)| {
            PyRep::keyval([
                ("itemID", PyRep::Integer(*iid)),
                ("typeID", PyRep::Integer(*tid as i64)),
                ("quantity", PyRep::Integer(*qty)),
                ("typeName", PyRep::String(name.clone())),
            ])
        })
        .collect();

    Ok(PyRep::keyval([
        ("siteID", PyRep::String(site_id.to_string())),
        ("dungeonID", PyRep::Integer(dungeon_id as i64)),
        ("dungeonName", PyRep::String(dungeon_name)),
        ("currentWave", PyRep::Integer(new_wave as i64)),
        ("totalWaves", PyRep::Integer(total_waves as i64)),
        ("completed", PyRep::Bool(completed_now)),
        ("activeNpcBallIDs", PyRep::List(spawned_npc_ids.into_iter().map(PyRep::Integer).collect())),
        ("droppedLoot", PyRep::List(loot_py)),
        ("escalated", PyRep::Bool(escalation_created.is_some())),
        (
            "escalationInstanceID",
            escalation_created.as_ref().map(|e| PyRep::Integer(e.instance_id)).unwrap_or(PyRep::None),
        ),
    ]))
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
        .ok_or_else(|| BridgeRpcError {
            code: "NotInSpace".into(),
            message: "Action requires an active solar system session".into(),
        })
}

pub async fn handle_pve_exploration(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    if let Some(res) = handle_pve_hacking(ctx, session, service, method, args, kwargs).await {
        return Some(res);
    }

    match (service, method) {
        // parity: feat:pve.combat-anomalies, feat:pve.wormhole-sites
        ("scanSvc", "GetCombatAnomalyInstances") => {
            if let Err(e) = ServerContext::require_character(session) { return Some(Err(e)); }
            let sys_id = match args.first().and_then(|a| a.as_i64()).map(|v| v as SolarSystemId).map(Ok).unwrap_or_else(|| resolve_session_system_id(ctx, session)) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            ensure_system_pve_sites_seeded(ctx, sys_id).await;

            let list = with_pve_exploration_reg(ctx, |reg| {
                let now = now_sec(reg);
                reg.sites_by_system
                    .get(&sys_id)
                    .map(|sites| {
                        sites
                            .iter()
                            .filter(|s| {
                                !s.probe_required
                                    && !s.cleared
                                    && (s.lifetime_seconds == 0 || now < s.created_unix.saturating_add(s.lifetime_seconds))
                            })
                            .map(|s| {
                                PyRep::keyval([
                                    ("siteID", PyRep::String(s.site_id.clone())),
                                    ("dungeonID", PyRep::Integer(s.dungeon_id as i64)),
                                    ("dungeonName", PyRep::String(s.dungeon_name.clone())),
                                    ("siteCategory", PyRep::String(s.site_category.clone())),
                                    (
                                        "wormholeClass",
                                        s.wormhole_class.map(|c| PyRep::Integer(c as i64)).unwrap_or(PyRep::None),
                                    ),
                                    ("currentWave", PyRep::Integer(s.current_wave as i64)),
                                    ("totalWaves", PyRep::Integer(s.total_waves as i64)),
                                    (
                                        "activeNpcBallIDs",
                                        PyRep::List(s.active_npc_ball_ids.iter().copied().map(PyRep::Integer).collect()),
                                    ),
                                    ("probeRequired", PyRep::Bool(false)),
                                    ("overviewVisible", PyRep::Bool(true)),
                                ])
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default()
            });
            Some(Ok(PyRep::List(list)))
        }

        // parity: feat:pve.combat-anomalies, feat:pve.wormhole-sites, feat:pve.escalations, feat:pve.expeditions
        ("scanSvc", "EnterCombatAnomaly") => {
            if let Err(e) = ServerContext::require_character(session) { return Some(Err(e)); }
            let sys_id = match resolve_session_system_id(ctx, session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            ensure_system_pve_sites_seeded(ctx, sys_id).await;
            let site_id = args
                .first()
                .and_then(|a| a.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("PVE-ANOM-{sys_id}-1"));
            let advance_wave = args.get(1).and_then(|a| a.as_bool()).unwrap_or(false);
            if advance_wave {
                return Some(
                    crate::services::pve_expeditions::validate_and_advance_pve_site_room(
                        ctx, session, sys_id, &site_id,
                    )
                    .await,
                );
            }
            let char_id = session.character_id.unwrap_or(0);
            Some(crate::services::pve_expeditions::enter_combat_anomaly_site(
                ctx, char_id, sys_id, &site_id,
            ))
        }

        // parity: feat:pve.ded-complexes, feat:pve.sleeper-caches
        ("keeper", "GetDungeonShipRestrictions") | ("keeper", "CanEnterDungeonInCurrentShip") => {
            if let Err(e) = ServerContext::require_character(session) { return Some(Err(e)); }
            let sys_id = match resolve_session_system_id(ctx, session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            ensure_system_pve_sites_seeded(ctx, sys_id).await;

            let arg0_int = args.first().and_then(|a| a.as_i64());
            let arg0_str = args.first().and_then(|a| a.as_str());

            let mut sleeper_tier_opt: Option<(SleeperCacheTier, u32, &'static str)> = None;
            let mut ded_rating_opt: Option<u8> = None;

            if let Some(did) = arg0_int {
                if did > 10 {
                    if let Some(tier) = sleeper_cache_tier_for_dungeon(did as u32) {
                        let name = match tier {
                            SleeperCacheTier::Limited => "Limited Sleeper Cache",
                            SleeperCacheTier::Standard => "Standard Sleeper Cache",
                            SleeperCacheTier::Superior => "Superior Sleeper Cache",
                        };
                        sleeper_tier_opt = Some((tier, did as u32, name));
                    }
                } else {
                    ded_rating_opt = Some(did.clamp(1, 10) as u8);
                }
            } else if let Some(sid) = arg0_str {
                with_pve_exploration_reg(ctx, |reg| {
                    if let Some(sites) = reg.sites_by_system.get(&sys_id) {
                        if let Some(s) = sites.iter().find(|x| x.site_id == sid || x.dungeon_id.to_string() == sid) {
                            if s.site_category == "SleeperCache" {
                                if let Some(tier) = sleeper_cache_tier_for_dungeon(s.dungeon_id) {
                                    sleeper_tier_opt = Some((tier, s.dungeon_id, "Sleeper Cache"));
                                }
                            } else if let Some(r) = s.ded_rating {
                                ded_rating_opt = Some(r);
                            }
                        }
                    }
                });
            }

            let (did, dname, ded_rating_val, max_mass, label, allowed_groups) = if let Some((tier, d_id, name)) = sleeper_tier_opt {
                let (mass, lbl, grps) = sleeper_cache_ship_restrictions(tier);
                (d_id, name.to_string(), 0u8, mass, lbl, grps)
            } else {
                let rating = ded_rating_opt.unwrap_or(3);
                let (mass, lbl, grps) = ded_ship_restriction_for_rating(rating);
                let (d_id, d_name) = ded_complex_for_rating(&ctx.sde, sys_id, rating);
                (d_id, d_name.to_string(), rating, mass, lbl, grps)
            };

            let (ship_type_id, ship_group_id, ship_mass) = session
                .ship_id
                .and_then(|sid| {
                    ctx.db.with_snapshot(|snap| {
                        snap.items.get(&sid).map(|item| {
                            let t = ctx.sde.types.get(&item.type_id);
                            (item.type_id, t.map(|x| x.group_id).unwrap_or(25), t.map(|x| x.mass).unwrap_or(1_200_000.0))
                        })
                    })
                })
                .unwrap_or((587, 25, 1_067_000.0));

            let can_enter = allowed_groups.contains(&ship_group_id) && ship_mass <= max_mass;
            Some(Ok(PyRep::keyval([
                ("dungeonID", PyRep::Integer(did as i64)),
                ("dungeonName", PyRep::String(dname)),
                ("dedRating", PyRep::Integer(ded_rating_val as i64)),
                ("maxShipMass", PyRep::Real(max_mass)),
                ("restrictionDescription", PyRep::String(label.into())),
                ("allowedGroupIDs", PyRep::List(allowed_groups.iter().map(|&g| PyRep::Integer(g as i64)).collect())),
                ("shipTypeID", PyRep::Integer(ship_type_id as i64)),
                ("shipGroupID", PyRep::Integer(ship_group_id as i64)),
                ("canEnter", PyRep::Bool(can_enter)),
            ])))
        }

        // parity: feat:pve.ded-complexes, feat:pve.sleeper-caches
        ("keeper", "ActivateAccelerationGate")
        | ("beyonce", "ActivateAccelerationGate")
        | ("keeper", "EnterNextDungeonRoom") => {
            if let Err(e) = ServerContext::require_character(session) { return Some(Err(e)); }
            if let Ok(sys) = resolve_session_system_id(ctx, session) {
                if let Some(res) =
                    crate::services::pve_dungeon_runtime::try_activate_dungeon_runtime_gate(
                        ctx, session, sys, args,
                    )
                    .await
                {
                    return Some(res);
                }
            }
            let mut site_id = args.first().and_then(|a| a.as_str()).unwrap_or("").to_string();
            if let Some(item_id) = args.first().and_then(|a| a.as_i64()) {
                // Integer argument: a DED acceleration gate ball in the pilot's system resolves to its site;
                // any other gate item belongs to the in-space gate / mission handlers.
                let gate_site = match resolve_session_system_id(ctx, session) {
                    Ok(sys) => {
                        ensure_system_pve_sites_seeded(ctx, sys).await;
                        with_pve_exploration_reg(ctx, |reg| {
                            reg.sites_by_system.get(&sys).and_then(|l| {
                                l.iter().find(|s| s.gate_ball_id == Some(item_id)).map(|s| s.site_id.clone())
                            })
                        })
                    }
                    Err(_) => None,
                };
                match gate_site {
                    Some(sid) => site_id = sid,
                    None if method == "EnterNextDungeonRoom" => {
                        let fleet_count = args.get(1).and_then(|a| a.as_i64()).unwrap_or(5).clamp(1, 10) as usize;
                        let res = crate::services::pve_special_sites::complete_special_dungeon_instance(ctx, session, item_id, fleet_count).await;
                        if let Ok(ref rep) = res {
                            let is_done = rep.dict_get("completed").or_else(|| rep.dict_get("cleared")) == Some(&PyRep::Bool(true));
                            if is_done {
                                if let Ok(sys_id) = resolve_session_system_id(ctx, session) {
                                    let cat = if (720_000..730_000).contains(&item_id) { "Triglavian" } else if (710_000..720_000).contains(&item_id) { "Homefront" } else { "DrifterHive" };
                                    let did = rep
                                        .dict_get("dungeonID")
                                        .and_then(|v| v.as_i64())
                                        .and_then(|v| u32::try_from(v).ok())
                                        .or_else(|| {
                                            crate::services::pve_special_sites::with_pve_special_reg(ctx, |reg| {
                                                reg.homefront_sites
                                                    .get(&sys_id)
                                                    .and_then(|l| l.iter().find(|s| s.instance_id == item_id).map(|s| s.dungeon_id))
                                                    .or_else(|| {
                                                        reg.triglavian_sites
                                                            .get(&sys_id)
                                                            .and_then(|l| l.iter().find(|s| s.instance_id == item_id).map(|s| s.dungeon_id))
                                                    })
                                            })
                                        })
                                        .unwrap_or(0);
                                    ctx.quasar_gateway.progression.extra.on_system_tale_encounter_completed(sys_id, cat, did, 1, ctx.now_unix(), &ctx.quasar_gateway);
                                }
                            }
                        }
                        return Some(res);
                    }
                    None => return None,
                }
            }
            let sys_id = match resolve_session_system_id(ctx, session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if site_id.starts_with("INC-") {
                let res = crate::services::pve_incursion_sites::process_incursion_site_completion_and_payout(
                    ctx, session.character_id, session.fleet_id, sys_id, &site_id, None,
                ).await.map(|r| {
                    PyRep::keyval([
                        ("siteID", PyRep::String(r.site_id)), ("tier", PyRep::String(r.tier)),
                        ("fleetSize", PyRep::Integer(r.fleet_size as i64)), ("fleetScaleFactor", PyRep::Real(r.fleet_scale_factor)),
                        ("iskPerPilot", PyRep::Integer(r.isk_per_pilot)), ("concordLpPerPilot", PyRep::Integer(r.concord_lp_per_pilot)),
                        ("influence", PyRep::Real(r.influence)), ("state", PyRep::String(r.state)),
                        ("mothershipSpawned", PyRep::Bool(r.mothership_spawned)), ("incursionEnded", PyRep::Bool(r.incursion_ended)),
                    ])
                });
                return Some(res);
            }
            ensure_system_pve_sites_seeded(ctx, sys_id).await;
            Some(
                crate::services::pve_expeditions::validate_and_advance_pve_site_room(
                    ctx, session, sys_id, &site_id,
                )
                .await,
            )
        }

        // parity: feat:pve.sleeper-caches, feat:pve.ded-complexes
        ("keeper", "GetDungeonInfo") | ("keeper", "GetDungeonObjectivesData") => {
            if let Err(e) = ServerContext::require_character(session) { return Some(Err(e)); }
            let sys_id = match resolve_session_system_id(ctx, session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            ensure_system_pve_sites_seeded(ctx, sys_id).await;
            let site_id = args.first().and_then(|a| a.as_str()).unwrap_or("").to_string();
            let res = with_pve_exploration_reg(ctx, |reg| {
                let Some(site) = reg.sites_by_system.get(&sys_id).and_then(|l| l.iter().find(|s| s.site_id == site_id)) else {
                    return Err(BridgeRpcError {
                        code: "DungeonNotFound".into(),
                        message: format!("Dungeon site {site_id} not found"),
                    });
                };
                Ok(PyRep::keyval([
                    ("siteID", PyRep::String(site.site_id.clone())),
                    ("dungeonID", PyRep::Integer(site.dungeon_id as i64)),
                    ("dungeonName", PyRep::String(site.dungeon_name.clone())),
                    ("siteCategory", PyRep::String(site.site_category.clone())),
                    ("currentRoom", PyRep::Integer(site.current_room as i64)),
                    ("totalRooms", PyRep::Integer(site.total_rooms as i64)),
                    ("currentWave", PyRep::Integer(site.current_wave as i64)),
                    ("totalWaves", PyRep::Integer(site.total_waves as i64)),
                    ("sleeperRiftStabilized", PyRep::Bool(site.sleeper_rift_stabilized)),
                    ("sleeperSentryDisabled", PyRep::Bool(site.sleeper_sentry_disabled)),
                    ("sleeperGasCloudActive", PyRep::Bool(site.sleeper_gas_cloud_active)),
                    ("cleared", PyRep::Bool(site.cleared)),
                ]))
            });
            Some(res)
        }

        // parity: feat:pve.escalations, feat:pve.expeditions, rpc:dungeonExplorationMgr.GetMyEscalatingPathDetails
        ("dungeonExplorationMgr", "GetMyEscalatingPathDetails")
        | ("scanSvc", "GetMyEscalatingPathDetails")
        | ("dungeonExplorationMgr", "GetEscalations") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            if !args.is_empty() {
                return Some(Err(BridgeRpcError::user_error("InvalidArguments")));
            }
            Some(Ok(
                crate::services::pve_expeditions::get_my_escalating_path_details_rowset(ctx, char_id),
            ))
        }

        // parity: feat:pve.escalations, feat:pve.expeditions
        ("dungeonExplorationMgr", "GetEscalatingPathDungeonsEntered")
        | ("scanSvc", "GetEscalatingPathDungeonsEntered") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let entered = with_pve_exploration_reg(ctx, |reg| {
                crate::services::pve_expeditions::get_escalating_path_dungeons_entered(reg, char_id)
            });
            Some(Ok(PyRep::List(entered)))
        }

        _ => None,
    }
}
