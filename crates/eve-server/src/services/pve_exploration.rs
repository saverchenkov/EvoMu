//! `pve_exploration` — PvE Cosmic Combat Anomalies, Combat Signatures, DED Rated Complexes,
//! Escalations & Expeditions, and Wormhole Sleeper Combat Sites (`Phase 8: W-3 & W-4`).
//!
// parity: feat:pve.combat-anomalies, feat:pve.combat-signatures, feat:pve.ded-complexes
// parity: feat:pve.escalations, feat:pve.expeditions, feat:pve.wormhole-sites

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::pve_hacking_sites::handle_pve_hacking;
use eve_db::lock_rank::AnyRwLock;
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
    pub self_repair_turns: HashMap<(CharacterId, ItemId), u8>,
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
pub const PVE_EXPLORATION_NOTE_ID: i64 = -8_200_000_000_000;

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

/// Reads `PveExplorationRegistry` cleanly from a DB snapshot without ever mutating `owner_notes`.
pub fn read_pve_exploration_reg_db<R>(
    db: &std::sync::Arc<eve_db::WorldRepository>,
    now_unix: u64,
    f: impl FnOnce(&PveExplorationRegistry) -> R,
) -> R {
    let mut reg = db.with_snapshot(|snap| {
        snap.owner_notes
            .get(&PVE_EXPLORATION_NOTE_ID)
            .and_then(|note| serde_json::from_str::<PveExplorationRegistry>(&note.note).ok())
            .unwrap_or_default()
    });
    reg.db_weak = std::sync::Arc::downgrade(db);
    reg.current_now_sec = now_unix;
    f(&reg)
}

pub fn read_pve_exploration_reg<R>(
    ctx: &ServerContext,
    f: impl FnOnce(&PveExplorationRegistry) -> R,
) -> R {
    read_pve_exploration_reg_db(&ctx.db, ctx.now_unix(), f)
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
    combat_ephemeral: &dyn AnyRwLock<crate::services::combat_crimewatch::CombatRuntimeState>,
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
    pending_notifications: &dyn AnyRwLock<std::collections::HashMap<i64, Vec<crate::router::cache_notifications::PendingMachoNotification>>>,
    notify_tx: &tokio::sync::broadcast::Sender<(i64, crate::router::cache_notifications::PendingMachoNotification)>,
    killer_char_id: i64,
    cleared_site_id: Option<String>,
    new_escalation: Option<EscalatingPathEntry>,
    advanced_step: Option<EscalatingPathEntry>,
) {
    crate::services::notify_emitters_tournaments_anomalies::emit_expedition_combat_notifications(
        db,
        pending_notifications,
        notify_tx,
        cleared_site_id.as_deref(),
        new_escalation.as_ref(),
        advanced_step.as_ref(),
    );

    let cleared_sys_did_opt = cleared_site_id.as_ref().and_then(|site_id| {
        db.with_snapshot(|snap| {
            snap.owner_notes
                .get(&PVE_EXPLORATION_NOTE_ID)
                .and_then(|note| serde_json::from_str::<PveExplorationRegistry>(&note.note).ok())
                .and_then(|reg| {
                    reg.sites_by_system
                        .values()
                        .flatten()
                        .find(|s| s.site_id == *site_id)
                        .map(|s| (s.solar_system_id, s.dungeon_id))
                })
        })
    });

    let fleet_info = new_escalation.as_ref().and_then(|esc| {
        db.with_snapshot(|snap| {
            let fid = snap.characters.get(&killer_char_id).and_then(|c| c.fleet_id)?;
            if fid <= 0 {
                return None;
            }
            let members = snap
                .fleets
                .get(&fid)
                .map(|fl| fl.members.clone())
                .unwrap_or_default();
            Some((
                fid,
                members,
                killer_char_id,
                i64::from(esc.effective_journal_entry_template_id()),
            ))
        })
    });

    crate::services::pve_expeditions::emit_expedition_destruction_notifications(
        db,
        pending_notifications,
        notify_tx,
        cleared_sys_did_opt,
        new_escalation.as_ref(),
        advanced_step.as_ref(),
    );

    if let Some(site_id) = cleared_site_id {
        let cleared_did_opt = cleared_sys_did_opt.map(|(_, d)| d);
        crate::services::fleet_social::fleet_social_scanning::emit_signal_tracker_site_removed_with_db(
            db, pending_notifications, notify_tx, &site_id,
        );
        crate::router::notify_registry::emit_typed_char_core(
            db,
            Some(pending_notifications),
            Some(notify_tx),
            killer_char_id,
            None,
            crate::router::notify_registry::OnCosmicAnomalyRemoved { site_id },
        );
        if let Some(did) = cleared_did_opt {
            crate::router::notify_registry::emit_typed_char_core(
                db,
                Some(pending_notifications),
                Some(notify_tx),
                killer_char_id,
                None,
                crate::router::notify_registry::OnDungeonCompleted {
                    dungeon_id: i64::from(did),
                },
            );
        }
    }

    for esc in [new_escalation, advanced_step].into_iter().flatten() {
        crate::router::notify_registry::emit_typed_char_core(
            db,
            Some(pending_notifications),
            Some(notify_tx),
            killer_char_id,
            None,
            crate::router::notify_registry::OnEscalatingPathChange,
        );
        crate::router::notify_registry::emit_typed_char_core(
            db,
            Some(pending_notifications),
            Some(notify_tx),
            killer_char_id,
            None,
            crate::router::notify_registry::OnEscalatingPathMessage {
                instance_id: esc.instance_id,
            },
        );
    }

    if let Some((fid, members, char_id, template_id)) = fleet_info {
        crate::router::notify_registry::emit_typed_to_fleet_members_core(
            db,
            Some(pending_notifications),
            Some(notify_tx),
            fid,
            &members,
            crate::router::notify_registry::OnFleetEscalatingPathMessage {
                char_id,
                journal_entry_template_id: template_id,
            },
        );
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
    crate::services::combat_crimewatch::flush_combat_runtime_to_db(ctx);

    if let Some(ref sid) = cleared_site_id {
        let now = ctx.now_unix();
        let (cat, did) = pve_site_category_and_dungeon_id(&ctx.db, now, solar_system_id, sid);
        ctx.quasar_gateway.progression.extra.on_system_tale_encounter_completed(solar_system_id, &cat, did, 1, now, &ctx.quasar_gateway);
        crate::quasar_gateway::isolate_side_effect("pve_exploration.career_combat_site_done", || {
            crate::services::quasar_career_goals::record_combat_site_completion_in_system(
                &ctx.quasar_gateway,
                &ctx.sde,
                solar_system_id,
                killer_char_id,
                &cat,
            );
        });
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
    let (_, char_id) = ServerContext::require_character(session)?;
    if session.station_id.is_some() || session.structure_id.is_some() {
        return Err(BridgeRpcError {
            code: "ShipDocked".into(),
            message: format!("Cannot advance PvE site {site_id} while docked"),
            client_error: None,
        });
    }
    if session.solar_system_id != Some(solar_system_id) {
        return Err(BridgeRpcError {
            code: "NotInSiteSystem".into(),
            message: format!("Caller must be in solar system {solar_system_id} to advance site {site_id}"),
            client_error: None,
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
                client_error: None,
            });
        };
        if site.cleared {
            return Err(BridgeRpcError {
                code: "SiteAlreadyCleared".into(),
                message: format!("PvE site {site_id} is already cleared"),
                client_error: None,
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
                client_error: None,
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
                    client_error: None,
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
                client_error: None,
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
                crate::services::combat_crimewatch::flush_combat_runtime_to_db(ctx);
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

        crate::router::notify_registry::push_typed_session_notification(
            session,
            crate::router::notify_registry::OnCosmicAnomalyRemoved {
                site_id: site_id.to_string(),
            },
        );
        ctx.quasar_gateway.progression.extra.on_system_tale_encounter_completed(
            solar_system_id, &site_category, dungeon_id, 1, ctx.now_unix(), &ctx.quasar_gateway,
        );
        crate::quasar_gateway::isolate_side_effect("pve_exploration.advance_career_combat_site", || {
            crate::services::quasar_career_goals::record_combat_site_completion_in_system(
                &ctx.quasar_gateway,
                &ctx.sde,
                solar_system_id,
                char_id,
                &site_category,
            );
        });
        ctx.emit_typed(
            crate::router::NotificationTarget::Character(char_id),
            crate::router::notify_registry::OnDungeonCompleted {
                dungeon_id: i64::from(dungeon_id),
            },
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
            client_error: None,
        })
}

fn rpc_msg_err(code: &str, message: impl Into<String>) -> BridgeRpcError {
    let msg = message.into();
    BridgeRpcError::with_client_error(
        code,
        msg.clone(),
        eve_proto::ClientError::CustomInfo(msg),
    )
}

fn require_undocked_exploration_space(
    ctx: &ServerContext,
    session: &SessionState,
    require_ship: bool,
) -> Result<(i64, SolarSystemId), BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    if session.station_id.is_some() || session.structure_id.is_some() {
        return Err(rpc_msg_err(
            "NotInSpace",
            "Cannot access dungeon site while docked",
        ));
    }
    let sys_id = resolve_session_system_id(ctx, session)?;
    if session.solar_system_id.is_none()
        && ctx.db.with_snapshot(|s| {
            s.characters
                .get(&char_id)
                .is_some_and(|c| c.station_id.is_some() || c.structure_id.is_some())
        })
    {
        return Err(rpc_msg_err(
            "NotInSpace",
            "Cannot access dungeon site while docked",
        ));
    }
    if require_ship && session.ship_id.is_none() {
        return Err(rpc_msg_err("NoActiveShip", "Session has no active ship"));
    }
    Ok((char_id, sys_id))
}

fn is_naturally_seedable_pve_site_id(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
    site_id: &str,
) -> bool {
    if !ctx.sde.solar_systems.contains_key(&sys_id) {
        return false;
    }
    crate::services::pve_dungeon_cache::unseeded_combat_anomaly_specs(ctx, sys_id)
        .iter()
        .any(|spec| spec.site_id == site_id)
        || crate::services::pve_dungeon_cache::unseeded_pve_signature_specs(ctx, sys_id)
            .iter()
            .any(|(_, sig_id)| sig_id == site_id)
}

fn is_naturally_seedable_ded_gate_ball(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
    item_id: i64,
) -> bool {
    let Some(sys) = ctx.sde.solar_systems.get(&sys_id) else {
        return false;
    };
    if !ctx.sde.types.values().any(|t| t.group_id == 366) {
        return false;
    }
    let default_ded_rating: u8 = if sys.security_status >= 0.5 {
        3
    } else if sys.security_status > 0.0 {
        5
    } else {
        10
    };
    let expected_sig_id = format!("DED-{sys_id}-{default_ded_rating}");
    let expected_ball_id =
        1_864_000_000 + i64::from(sys_id % 100_000) * 10 + i64::from(default_ded_rating);
    item_id == expected_ball_id
        && crate::services::pve_dungeon_cache::unseeded_pve_signature_specs(ctx, sys_id)
            .iter()
            .any(|(_, sig_id)| sig_id == &expected_sig_id)
}

fn is_naturally_seedable_incursion_site(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
    site_id: &str,
) -> bool {
    let Some(const_id) = ctx
        .sde
        .solar_systems
        .get(&sys_id)
        .map(|s| s.constellation_id)
    else {
        return false;
    };
    let eligible = crate::services::pve_incursion_lifecycle::eligible_incursion_constellations(ctx);
    if !eligible.into_iter().take(3).any(|(cid, _)| cid == const_id) {
        return false;
    }
    let mut sys_list: Vec<SolarSystemId> = ctx
        .sde
        .solar_systems
        .values()
        .filter(|s| s.constellation_id == const_id)
        .map(|s| s.solar_system_id)
        .collect();
    sys_list.sort_unstable();
    let (stg, vg, as_s, hq) =
        crate::services::pve_incursion_lifecycle::assign_constellation_system_roles(
            &ctx.sde,
            const_id,
            &sys_list,
        );
    let vg1 = vg[0];
    let vg2 = vg.get(1).copied().unwrap_or(vg1);
    let as1 = as_s[0];
    let as2 = as_s.get(1).copied().unwrap_or(as1);
    [
        (stg, 5164_u32),
        (vg1, 5165),
        (as1, 5168),
        (hq, 5175),
        (hq, 5158),
        (vg2, 5166),
        (as2, 5167),
        (hq, 5171),
    ]
    .into_iter()
    .any(|(expected_sys, did)| {
        expected_sys == sys_id
            && (site_id == format!("INC-{const_id}-{did}") || site_id == format!("INC-{did}"))
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
            Some(handle_get_combat_anomaly_instances(ctx, session, args).await)
        }

        // parity: feat:pve.combat-anomalies, feat:pve.wormhole-sites, feat:pve.escalations, feat:pve.expeditions
        ("scanSvc", "EnterCombatAnomaly") => {
            Some(handle_enter_combat_anomaly(ctx, session, args).await)
        }

        // parity: feat:pve.ded-complexes, feat:pve.sleeper-caches
        ("keeper", "GetDungeonShipRestrictions") | ("keeper", "CanEnterDungeonInCurrentShip") => {
            let require_ship = method == "CanEnterDungeonInCurrentShip";
            let (_, sys_id) = match require_undocked_exploration_space(ctx, session, require_ship) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };

            let arg0_int = args.first().and_then(|a| a.as_i64());
            let arg0_str = args.first().and_then(|a| a.as_str());

            let mut sleeper_tier_opt: Option<(SleeperCacheTier, u32, &'static str)> = None;
            let mut ded_rating_opt: Option<u8> = None;
            let mut explicit_sde_dungeon: Option<(u32, String)> = None;

            if let Some(did) = arg0_int {
                if did > 10 {
                    let did_u32 = u32::try_from(did).ok();
                    if let Some(tier) = did_u32.and_then(sleeper_cache_tier_for_dungeon) {
                        let name = match tier {
                            SleeperCacheTier::Limited => "Limited Sleeper Cache",
                            SleeperCacheTier::Standard => "Standard Sleeper Cache",
                            SleeperCacheTier::Superior => "Superior Sleeper Cache",
                        };
                        sleeper_tier_opt = Some((tier, did as u32, name));
                    } else if let Some(d) = did_u32.and_then(|id| ctx.sde.dungeons.get(&id)) {
                        let dname = if d.name.trim().is_empty() {
                            format!("Dungeon {did}")
                        } else {
                            d.name.trim().to_string()
                        };
                        explicit_sde_dungeon = Some((did as u32, dname));
                    } else {
                        return Some(Err(rpc_msg_err(
                            "DungeonNotFound",
                            format!("Dungeon {did} not found"),
                        )));
                    }
                } else {
                    ded_rating_opt = Some(did.clamp(1, 10) as u8);
                }
            } else if let Some(sid) = arg0_str {
                read_pve_exploration_reg(ctx, |reg| {
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
            } else if let Some((d_id, d_name)) = explicit_sde_dungeon {
                let rating = ded_rating_opt.unwrap_or(3);
                let (mass, lbl, grps) = ded_ship_restriction_for_rating(rating);
                (d_id, d_name, rating, mass, lbl, grps)
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
            handle_activate_acceleration_gate(ctx, session, method, args).await
        }

        // parity: feat:pve.sleeper-caches, feat:pve.ded-complexes
        ("keeper", "GetDungeonInfo") | ("keeper", "GetDungeonObjectivesData") => {
            Some(handle_get_dungeon_info(ctx, session, args).await)
        }

        // parity: feat:pve.escalations, feat:pve.expeditions, rpc:dungeonExplorationMgr.GetMyEscalatingPathDetails
        ("dungeonExplorationMgr", "GetMyEscalatingPathDetails")
        | ("scanSvc", "GetMyEscalatingPathDetails")
        | ("dungeonExplorationMgr", "GetEscalations") => {
            Some(handle_get_escalating_path_details(ctx, session, args))
        }

        // parity: feat:pve.escalations, feat:pve.expeditions
        ("dungeonExplorationMgr", "GetEscalatingPathDungeonsEntered")
        | ("scanSvc", "GetEscalatingPathDungeonsEntered") => {
            Some(handle_get_escalating_path_dungeons_entered(ctx, session))
        }

        _ => None,
    }
}

#[allow(clippy::too_many_arguments)]
fn combat_anomaly_keyval(
    site_id: String,
    dungeon_id: u32,
    dungeon_name: String,
    site_category: String,
    wormhole_class: Option<u8>,
    current_wave: u8,
    total_waves: u8,
    active_npc_ball_ids: Vec<i64>,
) -> PyRep {
    PyRep::keyval([
        ("siteID", PyRep::String(site_id)),
        ("dungeonID", PyRep::Integer(dungeon_id as i64)),
        ("dungeonName", PyRep::String(dungeon_name)),
        ("siteCategory", PyRep::String(site_category)),
        (
            "wormholeClass",
            wormhole_class
                .map(|c| PyRep::Integer(c as i64))
                .unwrap_or(PyRep::None),
        ),
        ("currentWave", PyRep::Integer(current_wave as i64)),
        ("totalWaves", PyRep::Integer(total_waves as i64)),
        (
            "activeNpcBallIDs",
            PyRep::List(active_npc_ball_ids.into_iter().map(PyRep::Integer).collect()),
        ),
        ("probeRequired", PyRep::Bool(false)),
        ("overviewVisible", PyRep::Bool(true)),
    ])
}

async fn handle_get_combat_anomaly_instances(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (char_id, caller_sys) = require_undocked_exploration_space(ctx, session, true)?;
    let sys_id = match args.first() {
        None | Some(PyRep::None) => caller_sys,
        Some(arg) => {
            let Some(raw) = arg.as_i64() else {
                return Err(rpc_msg_err(
                    "InvalidArguments",
                    "solarSystemID must be an integer",
                ));
            };
            let Ok(id) = u32::try_from(raw) else {
                return Err(rpc_msg_err(
                    "InvalidArguments",
                    format!("Invalid solarSystemID {raw}"),
                ));
            };
            if !ctx.sde.solar_systems.contains_key(&id) {
                return Err(rpc_msg_err(
                    "UnknownSolarSystem",
                    format!("Unknown solar system {id}"),
                ));
            }
            id
        }
    };
    let is_seeded = read_pve_exploration_reg(ctx, |reg| {
        reg.seeded_systems.get(&sys_id).copied().unwrap_or(false)
            || reg.sites_by_system.get(&sys_id).is_some_and(|s| !s.is_empty())
    });
    if !is_seeded && sys_id == caller_sys {
        ensure_system_pve_sites_seeded(ctx, sys_id).await;
    }

    let list = if is_seeded || sys_id == caller_sys {
        read_pve_exploration_reg(ctx, |reg| {
            let now = now_sec(reg);
            let owned_exp_ids: std::collections::HashSet<i64> = reg
                .escalations_by_char
                .get(&char_id)
                .map(|list| list.iter().map(|e| e.instance_id).collect())
                .unwrap_or_default();
            reg.sites_by_system
                .get(&sys_id)
                .map(|sites| {
                    sites
                        .iter()
                        .filter(|s| {
                            !s.probe_required
                                && !s.cleared
                                && (s.site_category != "Expedition"
                                    || s.expedition_instance_id
                                        .is_some_and(|id| owned_exp_ids.contains(&id)))
                                && (s.lifetime_seconds == 0
                                    || now < s.created_unix.saturating_add(s.lifetime_seconds))
                        })
                        .map(|s| {
                            combat_anomaly_keyval(
                                s.site_id.clone(),
                                s.dungeon_id,
                                s.dungeon_name.clone(),
                                s.site_category.clone(),
                                s.wormhole_class,
                                s.current_wave,
                                s.total_waves,
                                s.active_npc_ball_ids.clone(),
                            )
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        })
    } else {
        crate::services::pve_dungeon_cache::unseeded_combat_anomaly_specs(ctx, sys_id)
            .into_iter()
            .map(|s| {
                combat_anomaly_keyval(
                    s.site_id,
                    s.dungeon_id,
                    s.dungeon_name,
                    s.site_category.into(),
                    s.wormhole_class,
                    s.current_wave,
                    s.total_waves,
                    Vec::new(),
                )
            })
            .collect()
    };
    Ok(PyRep::List(list))
}

async fn handle_enter_combat_anomaly(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (char_id, sys_id) = require_undocked_exploration_space(ctx, session, true)?;
    let Some(site_id) = args
        .first()
        .and_then(|a| a.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
    else {
        return Err(rpc_msg_err("AnomalyNotFound", "Combat anomaly siteID is required"));
    };
    let (site_exists, already_seeded) = read_pve_exploration_reg(ctx, |reg| {
        (
            reg.sites_by_system
                .get(&sys_id)
                .is_some_and(|sites| sites.iter().any(|s| s.site_id == site_id)),
            reg.seeded_systems.get(&sys_id).copied().unwrap_or(false),
        )
    });
    if !site_exists {
        if !already_seeded && is_naturally_seedable_pve_site_id(ctx, sys_id, &site_id) {
            ensure_system_pve_sites_seeded(ctx, sys_id).await;
        }
        let exists_after = read_pve_exploration_reg(ctx, |reg| {
            reg.sites_by_system
                .get(&sys_id)
                .is_some_and(|sites| sites.iter().any(|s| s.site_id == site_id))
        });
        if !exists_after {
            return Err(rpc_msg_err(
                "AnomalyNotFound",
                format!("Combat anomaly {site_id} not found in system {sys_id}"),
            ));
        }
    }
    let advance_wave = args.get(1).and_then(|a| a.as_bool()).unwrap_or(false);
    if advance_wave {
        return crate::services::pve_expeditions::validate_and_advance_pve_site_room(
            ctx, session, sys_id, &site_id,
        )
        .await;
    }
    let res = crate::services::pve_expeditions::enter_combat_anomaly_site(
        ctx, char_id, sys_id, &site_id,
    );
    if res.is_ok() {
        crate::services::wormhole_pve_escalations::step_wormhole_pve_system(
            ctx, sys_id, 1.0,
        )
        .await;
    }
    res
}

async fn handle_activate_acceleration_gate(
    ctx: &ServerContext,
    session: &mut SessionState,
    method: &str,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    if method == "ActivateAccelerationGate" {
        if let Err(e) = ServerContext::require_character(session) {
            return Some(Err(e));
        }
        if let (Ok(pre_sys), Some(gate_id)) = (
            resolve_session_system_id(ctx, session),
            args.first().and_then(|a| a.as_i64()),
        ) {
            if let Some(res) = crate::services::pve_fob_observatory::try_activate_observatory_gate(
                ctx, session, pre_sys, gate_id,
            )
            .await
            {
                return Some(res);
            }
        }
    }
    let (_, sys_id) = match require_undocked_exploration_space(ctx, session, false) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let has_runtime_gate = ctx.db.with_snapshot(|snap| {
        snap.dungeon_runtime
            .instances
            .values()
            .any(|i| !i.completed && !i.despawned && i.solar_system_id == sys_id)
    });
    if has_runtime_gate {
        if session.ship_id.is_none() {
            return Some(Err(rpc_msg_err("NoActiveShip", "Session has no active ship")));
        }
        if let Some(res) = crate::services::pve_dungeon_runtime::try_activate_dungeon_runtime_gate(
            ctx, session, sys_id, args,
        )
        .await
        {
            return Some(res);
        }
    }
    let mut site_id = args.first().and_then(|a| a.as_str()).unwrap_or("").to_string();
    let special_id_arg = args
        .first()
        .and_then(|a| a.as_i64())
        .or_else(|| super::r6_t1_p0_helpers::resolve_special_site_string_instance_id(ctx, &site_id));
    if let Some(item_id) = special_id_arg {
        // Integer argument: a DED acceleration gate ball in the pilot's system resolves to its site;
        // any other gate item belongs to the in-space gate / mission handlers.
        let (mut gate_site, already_seeded) = read_pve_exploration_reg(ctx, |reg| {
            let found = reg.sites_by_system.get(&sys_id).and_then(|l| {
                l.iter()
                    .find(|s| s.gate_ball_id == Some(item_id))
                    .map(|s| s.site_id.clone())
            });
            let seeded = reg.seeded_systems.get(&sys_id).copied().unwrap_or(false);
            (found, seeded)
        });
        if gate_site.is_none()
            && !already_seeded
            && session.ship_id.is_some()
            && is_naturally_seedable_ded_gate_ball(ctx, sys_id, item_id)
        {
            ensure_system_pve_sites_seeded(ctx, sys_id).await;
            gate_site = read_pve_exploration_reg(ctx, |reg| {
                reg.sites_by_system.get(&sys_id).and_then(|l| {
                    l.iter()
                        .find(|s| s.gate_ball_id == Some(item_id))
                        .map(|s| s.site_id.clone())
                })
            });
        }
        match gate_site {
            Some(sid) => {
                if session.ship_id.is_none() {
                    return Some(Err(rpc_msg_err("NoActiveShip", "Session has no active ship")));
                }
                site_id = sid;
            }
            None if super::r6_t1_p0_helpers::is_known_special_site_instance_id(ctx, item_id) => {
                if session.ship_id.is_none() {
                    return Some(Err(rpc_msg_err("NoActiveShip", "Session has no active ship")));
                }
                let fleet_count = args.get(1).and_then(|a| a.as_i64()).unwrap_or(5).clamp(1, 10) as usize;
                let res = crate::services::pve_special_sites::complete_special_dungeon_instance(ctx, session, item_id, fleet_count).await;
                if let Ok(ref rep) = res {
                    let is_done = rep.dict_get("completed").or_else(|| rep.dict_get("cleared")) == Some(&PyRep::Bool(true));
                    if is_done {
                        let cat = if (720_000..730_000).contains(&item_id) { "Triglavian" } else if (710_000..720_000).contains(&item_id) { "Homefront" } else { "DrifterHive" };
                        let did = rep
                            .dict_get("dungeonID")
                            .and_then(|v| v.as_i64())
                            .and_then(|v| u32::try_from(v).ok())
                            .or_else(|| {
                                crate::services::pve_special_sites::read_pve_special_reg(ctx, |reg| {
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
                return Some(res);
            }
            None if method == "EnterNextDungeonRoom" => {
                if session.ship_id.is_none() {
                    return Some(Err(rpc_msg_err("NoActiveShip", "Session has no active ship")));
                }
                return Some(Err(rpc_msg_err(
                    "DungeonNotFound",
                    format!("Dungeon instance {item_id} not found"),
                )));
            }
            None => return None,
        }
    }
    if session.ship_id.is_none() {
        return Some(Err(rpc_msg_err("NoActiveShip", "Session has no active ship")));
    }
    if site_id.is_empty() {
        return Some(Err(rpc_msg_err("DungeonNotFound", "Dungeon siteID is required")));
    }
    if site_id.starts_with("INC-") {
        let (mut inc_exists, special_initialized) =
            crate::services::pve_special_sites::read_pve_special_reg(ctx, |reg| {
                let exists = reg.incursions.values().any(|inc| {
                    inc.sites.iter().any(|s| {
                        s.solar_system_id == sys_id
                            && (s.site_id == site_id
                                || s.site_id.ends_with(&site_id)
                                || site_id.ends_with(&format!("-{}", s.dungeon_id)))
                    })
                });
                (exists, reg.initialized)
            });
        if !inc_exists {
            if !special_initialized && is_naturally_seedable_incursion_site(ctx, sys_id, &site_id)
            {
                crate::services::pve_special_sites::ensure_special_sites_spawned_for_system(
                    ctx, sys_id,
                )
                .await;
                inc_exists = crate::services::pve_special_sites::read_pve_special_reg(ctx, |reg| {
                    reg.incursions.values().any(|inc| {
                        inc.sites.iter().any(|s| {
                            s.solar_system_id == sys_id
                                && (s.site_id == site_id
                                    || s.site_id.ends_with(&site_id)
                                    || site_id.ends_with(&format!("-{}", s.dungeon_id)))
                        })
                    })
                });
            }
            if !inc_exists {
                return Some(Err(rpc_msg_err(
                    "DungeonNotFound",
                    format!("Incursion site {site_id} not found"),
                )));
            }
        }
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
    let (site_exists, already_seeded) = read_pve_exploration_reg(ctx, |reg| {
        (
            reg.sites_by_system.get(&sys_id).is_some_and(|l| {
                l.iter()
                    .any(|s| s.site_id == site_id || s.dungeon_id.to_string() == site_id)
            }),
            reg.seeded_systems.get(&sys_id).copied().unwrap_or(false),
        )
    });
    if !site_exists {
        if !already_seeded && is_naturally_seedable_pve_site_id(ctx, sys_id, &site_id) {
            ensure_system_pve_sites_seeded(ctx, sys_id).await;
        }
        let exists_after = read_pve_exploration_reg(ctx, |reg| {
            reg.sites_by_system.get(&sys_id).is_some_and(|l| {
                l.iter()
                    .any(|s| s.site_id == site_id || s.dungeon_id.to_string() == site_id)
            })
        });
        if !exists_after {
            return Some(Err(rpc_msg_err(
                "DungeonNotFound",
                format!("Dungeon site {site_id} not found"),
            )));
        }
    }
    Some(
        crate::services::pve_expeditions::validate_and_advance_pve_site_room(
            ctx, session, sys_id, &site_id,
        )
        .await,
    )
}

async fn handle_get_dungeon_info(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, sys_id) = require_undocked_exploration_space(ctx, session, false)?;
    let site_id = args.first().and_then(|a| a.as_str()).unwrap_or("").to_string();
    read_pve_exploration_reg(ctx, |reg| {
        let Some(site) = reg.sites_by_system.get(&sys_id).and_then(|l| l.iter().find(|s| s.site_id == site_id)) else {
            return Err(BridgeRpcError {
                code: "DungeonNotFound".into(),
                message: format!("Dungeon site {site_id} not found"),
                client_error: None,
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
    })
}

fn handle_get_escalating_path_details(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    if !args.is_empty() {
        return Err(BridgeRpcError::user_error("InvalidArguments"));
    }
    Ok(crate::services::pve_expeditions::get_my_escalating_path_details_rowset(ctx, char_id))
}

fn handle_get_escalating_path_dungeons_entered(
    ctx: &ServerContext,
    session: &SessionState,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let entered = read_pve_exploration_reg(ctx, |reg| {
        crate::services::pve_expeditions::get_escalating_path_dungeons_entered(reg, char_id)
    });
    Ok(PyRep::List(entered))
}

#[cfg(test)]
#[path = "pve_exploration_tests.rs"]
mod pve_exploration_tests;

#[cfg(test)]
#[path = "pve_exploration_edges_tests.rs"]
mod pve_exploration_edges_tests;
#[cfg(test)]
#[path = "pve_exploration_edges_b4_tests.rs"]
mod pve_exploration_edges_b4_tests;
