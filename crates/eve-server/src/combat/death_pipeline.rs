//! `death_pipeline` — Canonical ship destruction, wreck spawning, 50% loot drop, pod ejection, insurance payout, and killmail generation (`C-3`).

#![forbid(unsafe_code)]

use crate::formulas::compute_insurance_quote;
use crate::router::ServerContext;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, KillmailRecord, WalletJournalEntry};
use eve_destiny::{Ball, MovementMode, SlimItemMeta};
use glam::DVec3;
use std::collections::HashMap;

/// Result summary of the completed death pipeline execution.
#[derive(Debug, Clone)]
pub struct DeathPipelineResult {
    pub wreck_item_id: i64,
    pub wreck_type_id: u32,
    pub loot_dropped_item_ids: Vec<i64>,
    pub loot_destroyed_item_ids: Vec<i64>,
    pub ejected_capsule_id: Option<i64>,
    pub insurance_payout_isk: f64,
    pub killmail_id: i64,
}

/// Synchronously executes the canonical EVE death pipeline (DB mutations, drone/fighter
/// abandonment, disconnected-ship state cleanup, session-change timer/notification, and
/// non-blocking `SystemHandle` ball updates).
pub fn execute_death_pipeline_sync(
    ctx: &ServerContext,
    sys_id: u32,
    victim_char_id: i64,
    victim_ship_id: i64,
    attacker_char_id: i64,
    victim_pos: DVec3,
    rng_seed: u64,
) -> Result<DeathPipelineResult, String> {
    let fitted_mods: Vec<(i64, u32, u16)> = ctx
        .ship_fittings
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&victim_ship_id)
        .map(|f| {
            f.modules
                .values()
                .map(|m| (m.item_id, m.type_id, m.flag_id))
                .collect()
        })
        .unwrap_or_default();
    crate::sp6_lifecycle::abandon_drones_and_fighters_for_ship_sync(
        ctx,
        victim_char_id,
        victim_ship_id,
        sys_id,
    );

    let (victim_ship_type_id, base_price, insured_payout) = ctx.db.with_snapshot(|snap| {
        let ship = snap
            .items
            .get(&victim_ship_id)
            .ok_or_else(|| format!("Ship {victim_ship_id} not found in world repository"))?;
        let base = ctx
            .sde
            .types
            .get(&ship.type_id)
            .map(|t| t.base_price)
            .unwrap_or(230_000.0);
        Ok::<_, String>((ship.type_id, base, ship.insured_payout_isk))
    })?;

    let is_pod = victim_ship_type_id == 670
        || victim_ship_type_id == 33328
        || ctx
            .sde
            .types
            .get(&victim_ship_type_id)
            .map(|t| t.group_id == 29)
            .unwrap_or(false);
    let wreck_type_id = if is_pod {
        26557
    } else {
        ctx.sde
            .wrecks
            .get(&victim_ship_type_id)
            .copied()
            .unwrap_or(26557)
    };

    let calculated_insurance_payout = if is_pod {
        0.0
    } else if insured_payout > 0.0 {
        insured_payout
    } else {
        compute_insurance_quote(base_price, 0.40).1
    };

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let now_unix = ctx.now_unix();
    let hull_cs = ctx.build_type_combat_state(victim_ship_type_id);
    let hull_total_hp = (hull_cs.max_shield_hp + hull_cs.max_armor_hp + hull_cs.max_structure_hp).max(1.0);
    let hull_base_isk = if base_price > 0.0 {
        base_price
    } else if is_pod {
        1_000.0
    } else {
        230_000.0
    };

    let (
        wreck_item_id,
        dropped_ids,
        destroyed_ids,
        dropped_entries,
        destroyed_entries,
        total_value_isk,
        ejected_cap_id,
        killmail_id,
        vic_corp,
        vic_all,
        att_corp,
        att_all,
        att_ship_type_id,
    ) = ctx.db.try_with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let wreck_id = snap.next_entity_id;

        crate::sp6_lifecycle::record_wreck_or_container_meta_in_snap(
            snap, wreck_id, now_unix, false,
        );
        snap.items.insert(
            wreck_id,
            InventoryItemRecord {
                item_id: wreck_id,
                type_id: wreck_type_id,
                owner_id: victim_char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        snap.item_positions
            .insert(wreck_id, [victim_pos.x, victim_pos.y, victim_pos.z]);

        for &(mid, mtype, mflag) in &fitted_mods {
            snap.items.entry(mid).or_insert_with(|| InventoryItemRecord {
                item_id: mid,
                type_id: mtype,
                owner_id: victim_char_id,
                location_id: victim_ship_id,
                flag: InventoryFlag::from_u16(mflag),
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            });
        }

        let mut child_item_ids: Vec<i64> = snap
            .items
            .values()
            .filter(|i| {
                i.location_id == victim_ship_id
                    && matches!(
                        i.flag as u16,
                        4 | 5 | 11..=34 | 87 | 89 | 90 | 92..=94 | 125..=185
                    )
            })
            .map(|i| i.item_id)
            .collect();
        child_item_ids.sort_unstable();

        let mut dropped = Vec::new();
        let mut destroyed = Vec::new();
        let mut dropped_km = Vec::new();
        let mut destroyed_km = Vec::new();
        let mut items_isk = 0.0;

        let is_pochven = ctx
            .sde
            .solar_systems
            .get(&sys_id)
            .map(|s| s.region_id == 10_000_070)
            .unwrap_or(false);
        let drop_threshold = if is_pochven { 0.60 } else { 0.50 };

        for iid in child_item_ids {
            let Some(item_snap) = snap.items.get(&iid).cloned() else {
                continue;
            };
            let item_unit_isk = ctx
                .sde
                .types
                .get(&item_snap.type_id)
                .map(|t| t.base_price)
                .filter(|&p| p > 0.0)
                .unwrap_or(100.0);
            items_isk += item_unit_isk * (item_snap.quantity.max(1) as f64);
            let is_rig_or_sub = matches!(item_snap.flag as i64, 89 | 92..=94 | 125..=132);
            let roll = ((rng_seed ^ (iid as u64)).wrapping_mul(6364136223846793005) >> 32)
                as f64
                / 4294967296.0;
            if !is_pod && !is_rig_or_sub && roll < drop_threshold {
                if let Some(item) = snap.items.get_mut(&iid) {
                    item.location_id = wreck_id;
                    item.flag = InventoryFlag::Cargo;
                    item.version += 1;
                    dropped.push(iid);
                }
                dropped_km.push(eve_db::KillmailItemEntry {
                    type_id: item_snap.type_id,
                    flag: item_snap.flag as u16,
                    qty_dropped: item_snap.quantity,
                    qty_destroyed: 0,
                    singleton: item_snap.singleton,
                });
            } else {
                snap.items.remove(&iid);
                destroyed.push(iid);
                destroyed_km.push(eve_db::KillmailItemEntry {
                    type_id: item_snap.type_id,
                    flag: item_snap.flag as u16,
                    qty_dropped: 0,
                    qty_destroyed: item_snap.quantity,
                    singleton: item_snap.singleton,
                });
            }
        }
        if is_pod {
            if let Some(imps) = snap.active_implants.get(&victim_char_id) {
                for &imp_tid in imps {
                    let imp_isk = ctx
                        .sde
                        .types
                        .get(&imp_tid)
                        .map(|t| t.base_price)
                        .filter(|&p| p > 0.0)
                        .unwrap_or(1_000.0);
                    items_isk += imp_isk;
                    destroyed_km.push(eve_db::KillmailItemEntry {
                        type_id: imp_tid,
                        flag: InventoryFlag::Implant as u16,
                        qty_dropped: 0,
                        qty_destroyed: 1,
                        singleton: true,
                    });
                }
            }
        }

        snap.items.remove(&victim_ship_id);
        snap.item_positions.remove(&victim_ship_id);

        let cap_id = if !is_pod {
            snap.next_entity_id += 1;
            let cid = snap.next_entity_id;
            snap.items.insert(
                cid,
                InventoryItemRecord {
                    item_id: cid,
                    type_id: 670,
                    owner_id: victim_char_id,
                    location_id: sys_id as i64,
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    dynamic_attributes: HashMap::new(),
                    source_mutaplasmid_id: None,
                    insured_payout_isk: 0.0,
                    is_blueprint_copy: false,
                    blueprint_runs_remaining: None,
                },
            );
            snap.item_positions
                .insert(cid, [victim_pos.x, victim_pos.y, victim_pos.z]);
            if let Some(vic) = snap.characters.get_mut(&victim_char_id) {
                vic.active_ship_id = cid;
                vic.version += 1;
                vic.system_epoch += 1;
            }
            Some(cid)
        } else {
            None
        };

        if calculated_insurance_payout > 0.0 {
            let payout_cents = IskCents::from_f64_isk(calculated_insurance_payout);
            if let Some(vic) = snap.characters.get_mut(&victim_char_id) {
                vic.balance = vic.balance.checked_add(payout_cents).unwrap_or(vic.balance);
            }
            snap.next_entity_id += 1;
            let jid = snap.next_entity_id;
            snap.wallet_journal.push(WalletJournalEntry {
                entry_id: jid,
                character_id: victim_char_id,
                entry_type: "insurance_payout".to_string(),
                amount: payout_cents,
                balance_after: snap
                    .characters
                    .get(&victim_char_id)
                    .map(|c| c.balance)
                    .unwrap_or(payout_cents),
                reason: format!("Insurance payout for loss of ship {victim_ship_id}"),
            });
            snap.next_entity_id += 1;
            let mid = snap.next_entity_id;
            snap.mails.push(eve_db::MailRecord {
                message_id: mid,
                sender_id: 1_000_132,
                recipient_id: victim_char_id,
                subject: format!("Insurance Payout for Ship {victim_ship_id}"),
                body: format!(
                    "Pend Insurance has credited {:.2} ISK to your account following the destruction of ship {}.",
                    calculated_insurance_payout, victim_ship_id
                ),
            });
            snap.mail_timestamps.insert(mid, now_unix);
            snap.mail_assigned_labels.insert((victim_char_id, mid), vec![1]);
        }

        let total_val = hull_base_isk + items_isk;
        let (vic_c, vic_a) = snap
            .characters
            .get(&victim_char_id)
            .map(|c| (c.corporation_id, c.alliance_id))
            .unwrap_or((1_000_009, None));
        let (att_c, att_a, att_stid) = snap
            .characters
            .get(&attacker_char_id)
            .map(|c| {
                let st = snap
                    .items
                    .get(&c.active_ship_id)
                    .map(|i| i.type_id)
                    .unwrap_or(victim_ship_type_id);
                (c.corporation_id, c.alliance_id, st)
            })
            .unwrap_or((vic_c, vic_a, victim_ship_type_id));

        snap.next_entity_id += 1;
        let kid = snap.next_entity_id;
        snap.record_killmail(
            KillmailRecord {
                kill_id: kid,
                victim_character_id: victim_char_id,
                attacker_character_id: attacker_char_id,
                solar_system_id: sys_id,
                ship_type_id: victim_ship_type_id,
                total_value_isk: total_val,
                ..Default::default()
            },
            now_unix,
        );

        Ok::<_, String>((
            wreck_id,
            dropped,
            destroyed,
            dropped_km,
            destroyed_km,
            total_val,
            cap_id,
            kid,
            vic_c,
            vic_a,
            att_c,
            att_a,
            att_stid,
        ))
    })?;

    if attacker_char_id > 0 && victim_char_id > 0 && attacker_char_id != victim_char_id {
        let _ = crate::services::combat_crimewatch::claim_player_bounty_on_kill(
            ctx,
            attacker_char_id,
            victim_char_id,
            victim_ship_type_id,
        );
    }

    let km_hash = crate::services::combat_crimewatch::compute_killmail_esi_hash(
        victim_char_id,
        attacker_char_id,
        victim_ship_type_id,
        now_unix,
    );
    crate::services::combat_crimewatch::with_mut_combat_runtime(ctx, |rt| {
        let mut km_attackers: Vec<eve_db::KillmailAttackerEntry> = Vec::new();
        for key in [victim_ship_id, victim_char_id] {
            if let Some(list) = rt.aggression_assist_log.remove(&key) {
                for a in list {
                    if now_unix.saturating_sub(a.last_action_unix)
                        <= crate::services::combat_crimewatch::KILLMAIL_ASSIST_WINDOW_SECS
                        && !km_attackers.iter().any(|x| x.character_id == a.attacker_char_id)
                    {
                        km_attackers.push(eve_db::KillmailAttackerEntry {
                            character_id: a.attacker_char_id,
                            corporation_id: a.corporation_id,
                            alliance_id: a.alliance_id,
                            ship_type_id: a.ship_type_id,
                            weapon_type_id: a.weapon_type_id,
                            damage_done: a.damage_done,
                            final_blow: a.attacker_char_id == attacker_char_id,
                            security_status: 0.0,
                        });
                    }
                }
            }
        }
        if !km_attackers.iter().any(|a| a.character_id == attacker_char_id) {
            km_attackers.push(eve_db::KillmailAttackerEntry {
                character_id: attacker_char_id,
                corporation_id: att_corp,
                alliance_id: att_all,
                ship_type_id: att_ship_type_id,
                weapon_type_id: att_ship_type_id,
                damage_done: hull_total_hp,
                final_blow: true,
                security_status: 0.0,
            });
        }
        let assist_dmg: f64 = km_attackers.iter().map(|a| a.damage_done).sum();
        rt.detailed_killmails.insert(
            killmail_id,
            eve_db::DetailedKillmail {
                kill_id: killmail_id,
                kill_time_unix: now_unix,
                solar_system_id: sys_id,
                victim_character_id: victim_char_id,
                victim_corporation_id: vic_corp,
                victim_alliance_id: vic_all,
                victim_ship_type_id,
                victim_damage_taken: assist_dmg.max(hull_total_hp),
                victim_position: [victim_pos.x, victim_pos.y, victim_pos.z],
                attackers: km_attackers,
                dropped_items: dropped_entries,
                destroyed_items: destroyed_entries,
                total_value_isk,
                killmail_hash: km_hash,
            },
        );
    });

    if is_pod {
        let _ = ctx
            .db
            .respawn_character_at_home_station(victim_char_id, victim_ship_id, &ctx.sde);
    }

    crate::services::combat_crimewatch::combat_wrecks_killmails::register_container_lifetime(
        ctx,
        wreck_item_id,
        sys_id,
        wreck_type_id,
        victim_char_id,
        now_unix,
        false,
    );

    crate::sp6_lifecycle::with_mut_sp6_state(&ctx.db, |st, _snap| {
        if is_pod {
            st.disconnected_ships.remove(&victim_char_id);
        } else if let Some(cid) = ejected_cap_id {
            if let Some(disc) = st.disconnected_ships.get_mut(&victim_char_id) {
                if disc.ship_id == victim_ship_id {
                    disc.ship_id = cid;
                    disc.ship_type_id = 670;
                }
            }
        }
    });

    sys_handle.try_remove_ball(victim_ship_id);
    sys_handle.try_spawn_ball_with_meta(
        Ball {
            id: wreck_item_id,
            type_id: wreck_type_id,
            position: victim_pos,
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 10_000.0,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        eve_destiny::CombatState {
            shield_hp: 0.0,
            max_shield_hp: 0.0,
            armor_hp: 0.0,
            max_armor_hp: 0.0,
            structure_hp: 500.0,
            max_structure_hp: 500.0,
            ..eve_destiny::CombatState::default()
        },
        SlimItemMeta {
            group_id: 186,
            category_id: 2,
            owner_id: victim_char_id,
            char_id: None,
            corp_id: -1,
            alliance_id: -1,
            security_status: 0.0,
            is_cloaked: false,
        },
    );
    if let Some(cid) = ejected_cap_id {
        sys_handle.try_spawn_ball_with_meta(
            Ball {
                id: cid,
                type_id: 670,
                position: victim_pos,
                velocity: DVec3::ZERO,
                max_velocity: 335.0,
                inertia: 0.25,
                mass: 16_000.0,
                radius: 15.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(670),
            SlimItemMeta {
                group_id: 29,
                category_id: 6,
                owner_id: victim_char_id,
                char_id: Some(victim_char_id),
                corp_id: -1,
                alliance_id: -1,
                security_status: 0.0,
                is_cloaked: false,
            },
        );
    }

    let new_sys_id = ctx
        .db
        .with_snapshot(|s| s.characters.get(&victim_char_id).map(|c| c.solar_system_id))
        .unwrap_or(sys_id);
    let death_reason = if is_pod { "podding" } else { "ship_loss" };
    crate::services::space_destiny::record_session_change_timer(
        ctx,
        victim_char_id,
        now_unix,
        new_sys_id,
        0,
        death_reason,
    );

    if is_pod {
        crate::services::map_service::record_solar_system_visit(ctx, victim_char_id, new_sys_id);
    }

    let total_damage = base_price.max(1000.0);
    crate::services::pvp_filament_mgr::record_proving_grounds_combat_result_if_queued(
        ctx,
        attacker_char_id,
        victim_char_id,
        sys_id,
        total_damage,
    );

    Ok(DeathPipelineResult {
        wreck_item_id,
        wreck_type_id,
        loot_dropped_item_ids: dropped_ids,
        loot_destroyed_item_ids: destroyed_ids,
        ejected_capsule_id: ejected_cap_id,
        insurance_payout_isk: calculated_insurance_payout,
        killmail_id,
    })
}

/// Executes the full EVE death pipeline when a ship's structure HP reaches 0:
/// 1. SDE wreck type resolution (`ctx.sde.wrecks` or fallback 26557).
/// 2. 50% loot drop / destroyed roll for all fitted and cargo items.
/// 3. Capsule/pod ejection with session ship update (or clone respawn if pod was destroyed).
/// 4. Insurance payout calculation and wallet balance crediting.
/// 5. Killmail generation and persistence.
/// 6. Physical wreck and capsule ball spawning in the solar system Ballpark.
pub async fn execute_death_pipeline(
    ctx: &ServerContext,
    sys_id: u32,
    victim_char_id: i64,
    victim_ship_id: i64,
    attacker_char_id: i64,
    victim_pos: DVec3,
    rng_seed: u64,
) -> Result<DeathPipelineResult, String> {
    execute_death_pipeline_sync(
        ctx,
        sys_id,
        victim_char_id,
        victim_ship_id,
        attacker_char_id,
        victim_pos,
        rng_seed,
    )
}

/// Triggers self-destruct for a ship or capsule in space, executing the canonical death pipeline
/// with the character as their own attacker (`feat:ships.self-destruct`).
pub async fn execute_self_destruct(
    ctx: &ServerContext,
    sys_id: u32,
    char_id: i64,
    ship_id: i64,
    pos: DVec3,
    rng_seed: u64,
) -> Result<DeathPipelineResult, String> {
    execute_death_pipeline(ctx, sys_id, char_id, ship_id, char_id, pos, rng_seed).await
}

/// Tracks 2-minute self-destruct countdown timers and cancellations (`feat:ships.self-destruct`).
#[derive(Debug, Default, Clone)]
pub struct SelfDestructManager {
    /// Active self-destruct sequences: `ship_id -> (character_id, system_id, detonation_unix)`
    pub active_timers: HashMap<i64, (i64, u32, u64)>,
}

impl SelfDestructManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Initiates a 2-minute (120s) self-destruct sequence on a ship.
    pub fn start_self_destruct(
        &mut self,
        ship_id: i64,
        char_id: i64,
        sys_id: u32,
        now_unix: u64,
    ) -> u64 {
        let detonation_time = now_unix.saturating_add(120);
        self.active_timers.insert(ship_id, (char_id, sys_id, detonation_time));
        detonation_time
    }

    /// Cancels an active self-destruct sequence (`AbortSelfDestruct`). Returns true if a timer was cancelled.
    pub fn abort_self_destruct(&mut self, ship_id: i64) -> bool {
        self.active_timers.remove(&ship_id).is_some()
    }

    /// Returns true if the ship is currently counting down to self-destruct.
    pub fn is_self_destructing(&self, ship_id: i64) -> bool {
        self.active_timers.contains_key(&ship_id)
    }

    /// Returns remaining seconds until detonation, if self-destruct is active.
    pub fn remaining_seconds(&self, ship_id: i64, now_unix: u64) -> Option<u64> {
        self.active_timers
            .get(&ship_id)
            .map(|&(_, _, det)| det.saturating_sub(now_unix))
    }

    /// Detonates any ships whose self-destruct timer has expired.
    pub async fn step_timers(
        &mut self,
        ctx: &ServerContext,
        now_unix: u64,
        rng_seed: u64,
    ) -> Vec<(i64, Result<DeathPipelineResult, String>)> {
        let mut ready = Vec::new();
        self.active_timers.retain(|&ship_id, &mut (char_id, sys_id, det_unix)| {
            if now_unix >= det_unix {
                ready.push((ship_id, char_id, sys_id));
                false
            } else {
                true
            }
        });

        let mut results = Vec::new();
        for (ship_id, char_id, sys_id) in ready {
            let sys_handle = ctx.systems.get_or_spawn(sys_id);
            let pos = sys_handle
                .get_ball_snapshot(ship_id)
                .await
                .ok()
                .flatten()
                .map(|(_, b)| b.position)
                .unwrap_or(DVec3::ZERO);
            let res = execute_self_destruct(ctx, sys_id, char_id, ship_id, pos, rng_seed).await;
            results.push((ship_id, res));
        }
        results
    }
}

