//! `combat_npc_encounters` — Multi-Wave NPC Encounter Sites & Destruction Triggers (`feat:npc.npc-spawn-encounter`).
//!
//! Handles:
//! - Wave-based NPC encounter sites where destroying the designated trigger NPC in space
//!   spawns the next wave N+1 on the next tick with authentic NPC types from the encounter definition.
//! - Elimination of the final wave marks the encounter site complete.
//! - Real weapon/damage destruction path hook integration (`apply_damage_to_ball` / `step_combat_system`).

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::combat_crimewatch::combat_npc_ai::spawn_specialized_npc_entity;
use crate::services::combat_crimewatch::{
    with_combat_runtime, with_mut_combat_runtime, with_mut_combat_runtime_tx_locks,
    with_mut_combat_runtime_with_locks, CombatRuntimeState, EncounterWaveDirector, ExtendedNpcEntity,
};
use crate::system_actor::SystemRegistry;
use eve_db::WorldRepository;
use eve_proto::ItemQty;
use eve_sde::{SdeRegistry, SolarSystemId};
use glam::DVec3;
use eve_db::lock_rank::AnyRwLock;
use std::sync::Arc;

/// Definition of a single wave in an NPC encounter site.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EncounterWaveDefinition {
    pub wave_number: u32,
    pub npc_type_ids: Vec<u32>,
    pub trigger_index: usize,
}

/// Creates a multi-wave NPC encounter site with authentic NPC types from SDE (`feat:npc.npc-spawn-encounter`).
pub async fn create_encounter_wave_site(
    ctx: &ServerContext,
    encounter_id: u32,
    solar_system_id: SolarSystemId,
    npc_type_id: u32,
    max_waves: u32,
    pos: DVec3,
) -> Vec<i64> {
    // Generate wave definitions: wave 1 uses npc_type_id, subsequent waves derive authentic types from SDE spawn groups
    let mut waves = Vec::new();
    let num_waves = max_waves.max(1);

    // Wave 1: initial types
    waves.push(vec![npc_type_id, npc_type_id]);

    // Subsequent waves: derive authentic types from SDE spawn groups
    let candidate_types: Vec<u32> = ctx
        .sde
        .npc_spawn_groups
        .values()
        .flat_map(|sg| sg.npc_type_ids.iter().copied())
        .filter(|&tid| tid != npc_type_id)
        .take(10)
        .collect();

    for w in 2..=num_waves {
        let t1 = candidate_types.get(((w as usize) * 2) % candidate_types.len().max(1)).copied().unwrap_or(npc_type_id);
        let t2 = candidate_types.get(((w as usize) * 2 + 1) % candidate_types.len().max(1)).copied().unwrap_or(npc_type_id);
        waves.push(vec![t1, t2]);
    }

    create_custom_encounter_site(ctx, encounter_id, solar_system_id, waves, pos).await
}

/// Creates an encounter site with explicit wave definitions containing authentic NPC types.
pub async fn create_custom_encounter_site(
    ctx: &ServerContext,
    encounter_id: u32,
    solar_system_id: SolarSystemId,
    waves: Vec<Vec<u32>>,
    pos: DVec3,
) -> Vec<i64> {
    if waves.is_empty() {
        return Vec::new();
    }
    let max_waves = waves.len() as u32;
    let wave1_types = &waves[0];

    let mut wave1_balls = Vec::new();
    for (i, &tid) in wave1_types.iter().enumerate() {
        let offset = DVec3::new((i as f64) * 1_000.0, 0.0, 0.0);
        let bid = spawn_specialized_npc_entity(
            ctx,
            solar_system_id,
            tid,
            "BeltRat",
            encounter_id,
            pos + offset,
        )
        .await;
        wave1_balls.push(bid);
    }

    let trigger_ball_id = wave1_balls.first().copied().unwrap_or(0);
    let npc_type_id = wave1_types.first().copied().unwrap_or(0);

    with_mut_combat_runtime(ctx, |rt| {
        rt.encounter_waves.insert(
            encounter_id,
            EncounterWaveDirector {
                encounter_id,
                solar_system_id,
                current_wave: 1,
                max_waves,
                trigger_ball_id,
                npc_type_id,
                active_wave_ball_ids: wave1_balls.clone(),
                completed: false,
                wave_npc_types: waves,
                origin_position: pos,
                next_wave_pending: false,
            },
        );
    });

    wave1_balls
}

/// Checks if an encounter wave trigger was destroyed, spawning next wave N+1 or marking site complete (`feat:npc.npc-spawn-encounter`).
pub async fn step_encounter_wave_on_kill(
    ctx: &ServerContext,
    solar_system_id: SolarSystemId,
    killed_ball_id: i64,
    pos: DVec3,
) {
    let triggered_info: Option<(u32, u32, Vec<u32>, DVec3)> = with_mut_combat_runtime(ctx, |rt| {
        let mut spawn_req = None;
        for enc in rt.encounter_waves.values_mut() {
            if enc.solar_system_id == solar_system_id && !enc.completed {
                enc.active_wave_ball_ids.retain(|&id| id != killed_ball_id);
                let is_trigger = killed_ball_id == enc.trigger_ball_id;

                if is_trigger || enc.active_wave_ball_ids.is_empty() {
                    if enc.current_wave < enc.max_waves {
                        enc.current_wave += 1;
                        let next_idx = (enc.current_wave - 1) as usize;
                        let next_types = enc
                            .wave_npc_types
                            .get(next_idx)
                            .cloned()
                            .unwrap_or_else(|| vec![enc.npc_type_id, enc.npc_type_id]);
                        spawn_req = Some((enc.encounter_id, enc.current_wave, next_types, enc.origin_position));
                    } else if enc.active_wave_ball_ids.is_empty() {
                        enc.completed = true;
                    }
                }
            }
        }
        spawn_req
    });

    if let Some((enc_id, _wave_num, next_types, base_pos)) = triggered_info {
        let mut new_balls = Vec::new();
        for (i, &tid) in next_types.iter().enumerate() {
            let offset = DVec3::new(500.0 + (i as f64) * 1_000.0, 0.0, 0.0);
            let nb = spawn_specialized_npc_entity(
                ctx,
                solar_system_id,
                tid,
                "BeltRat",
                enc_id,
                pos.lerp(base_pos, 0.5) + offset,
            )
            .await;
            new_balls.push(nb);
        }
        with_mut_combat_runtime(ctx, |rt| {
            if let Some(enc) = rt.encounter_waves.get_mut(&enc_id) {
                enc.trigger_ball_id = new_balls.first().copied().unwrap_or(0);
                enc.active_wave_ball_ids.extend(new_balls);
            }
        });
    }
}

/// Invoked from `npc_death_hook` when any NPC ball is destroyed in space by damage
/// (`apply_damage_to_ball` / `step_combat_system`), emitting standing-change notifications
/// to the killer's client socket in addition to DB updates (`feat:standings.standing-loss`).
pub async fn handle_npc_destroyed_in_space_with_ctx(
    ctx: &ServerContext,
    solar_system_id: SolarSystemId,
    killed_ball_id: i64,
    killer_char_id: Option<i64>,
) {
    handle_npc_destroyed_in_space_inner(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        &*ctx.combat_ephemeral,
        Some(ctx),
        solar_system_id,
        killed_ball_id,
        killer_char_id,
    )
    .await;
}

/// Invoked from `npc_death_hook` when any NPC ball is destroyed in space by damage (`apply_damage_to_ball` / `step_combat_system`).
pub async fn handle_npc_destroyed_in_space(
    systems: &Arc<SystemRegistry>,
    db: &Arc<WorldRepository>,
    sde: &Arc<SdeRegistry>,
    combat_ephemeral: &dyn AnyRwLock<CombatRuntimeState>,
    solar_system_id: SolarSystemId,
    killed_ball_id: i64,
    killer_char_id: Option<i64>,
) {
    handle_npc_destroyed_in_space_inner(
        systems,
        db,
        sde,
        combat_ephemeral,
        None,
        solar_system_id,
        killed_ball_id,
        killer_char_id,
    )
    .await;
}

#[allow(clippy::too_many_arguments)]
async fn handle_npc_destroyed_in_space_inner(
    systems: &Arc<SystemRegistry>,
    db: &Arc<WorldRepository>,
    sde: &Arc<SdeRegistry>,
    combat_ephemeral: &dyn AnyRwLock<CombatRuntimeState>,
    notify_ctx: Option<&ServerContext>,
    solar_system_id: SolarSystemId,
    killed_ball_id: i64,
    killer_char_id: Option<i64>,
) {
    let killer = killer_char_id.unwrap_or(0);
    crate::services::space_mining_harvest::on_wormhole_ore_guard_destroyed_db(db, killed_ball_id);
    if killer > 0 {
        crate::services::social_corp_fleet::social_corp_projects::record_server_project_activity_in_db(
            db,
            None,
            killer,
            &["DamageNPCShips", "DestroyNPCShips"],
            1,
            Some(i64::from(solar_system_id)),
        );
    }

    // 1. Advance encounter waves if killed_ball_id belongs to an active encounter
    let triggered_info: Option<(u32, u32, Vec<u32>, DVec3)> =
        with_mut_combat_runtime_with_locks(db, combat_ephemeral, |rt| {
            let mut spawn_req = None;
            for enc in rt.encounter_waves.values_mut() {
                if enc.solar_system_id == solar_system_id && !enc.completed {
                    enc.active_wave_ball_ids.retain(|&id| id != killed_ball_id);
                    let is_trigger = killed_ball_id == enc.trigger_ball_id;

                    if is_trigger || enc.active_wave_ball_ids.is_empty() {
                        if enc.current_wave < enc.max_waves {
                            enc.current_wave += 1;
                            let next_idx = (enc.current_wave - 1) as usize;
                            let next_types = enc
                                .wave_npc_types
                                .get(next_idx)
                                .cloned()
                                .unwrap_or_else(|| vec![enc.npc_type_id, enc.npc_type_id]);
                            spawn_req = Some((
                                enc.encounter_id,
                                enc.current_wave,
                                next_types,
                                enc.origin_position,
                            ));
                        } else if enc.active_wave_ball_ids.is_empty() {
                            enc.completed = true;
                        }
                    }
                }
            }
            spawn_req
        });

    if let Some((enc_id, _wave_num, next_types, base_pos)) = triggered_info {
        let handle = systems.get_or_spawn(solar_system_id);
        let mut new_balls = Vec::new();
        let mut new_entities = Vec::with_capacity(next_types.len());
        for (i, &tid) in next_types.iter().enumerate() {
            let offset = DVec3::new(500.0 + (i as f64) * 1_000.0, 0.0, 0.0);
            let pos = base_pos + offset;
            let ball_id = db.with_mut_snapshot(|snap| {
                snap.next_entity_id += 1;
                snap.next_entity_id
            });

            let sde_type = sde.types.get(&tid);
            let attr = |aid: u16| -> Option<f64> {
                sde_type.and_then(|t| t.default_attributes.get(&aid).copied())
            };
            let shield = attr(eve_sde::attr_ids::SHIELD_CAPACITY).unwrap_or(500.0);
            let armor = attr(eve_sde::attr_ids::ARMOR_HP).unwrap_or(500.0);
            let hull = attr(eve_sde::attr_ids::HP_STRUCT).unwrap_or(500.0);
            let bounty = attr(481).unwrap_or(0.0);
            let max_vel = attr(37).unwrap_or(250.0);
            let opt_range = attr(54).unwrap_or(15_000.0).max(5_000.0);
            let orb_range = (opt_range * 0.6).clamp(2_500.0, 20_000.0);
            let dmg_mult = attr(64).unwrap_or(2.0).max(1.0);
            let raw_em = attr(114).unwrap_or(0.0);
            let raw_exp = attr(116).unwrap_or(0.0);
            let raw_kin = attr(117).unwrap_or(0.0);
            let raw_thm = attr(118).unwrap_or(0.0);
            let damage_vector = if raw_em + raw_exp + raw_kin + raw_thm > 0.0 {
                eve_destiny::DamageVector {
                    em: raw_em * dmg_mult,
                    explosive: raw_exp * dmg_mult,
                    kinetic: raw_kin * dmg_mult,
                    thermal: raw_thm * dmg_mult,
                }
            } else {
                eve_destiny::DamageVector::new(25.0, 25.0, 25.0, 25.0)
            };
            let faction_id = sde_type.and_then(|t| t.faction_id).unwrap_or(500_010);

            let ball = eve_destiny::Ball {
                id: ball_id,
                type_id: tid,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: max_vel,
                inertia: 2.0,
                mass: 500_000.0,
                radius: 30.0,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            };

            let _ = handle.spawn_ball_with_combat(
                ball,
                eve_destiny::CombatState {
                    shield_hp: shield,
                    max_shield_hp: shield,
                    armor_hp: armor,
                    max_armor_hp: armor,
                    structure_hp: hull,
                    max_structure_hp: hull,
                    ..Default::default()
                },
            ).await;

            new_entities.push(ExtendedNpcEntity {
                ball_id,
                type_id: tid,
                faction_id,
                archetype: "BeltRat".into(),
                solar_system_id,
                spawn_group_id: enc_id,
                target_id: None,
                aggression_threat: std::collections::HashMap::new(),
                threat_table: std::collections::HashMap::new(),
                bounty_isk: bounty,
                optimal_range: opt_range,
                orbit_range: orb_range,
                damage_vector,
                entropic_cycles: 0,
                drifter_overshield_hp: 0.0,
                drifter_doomsday_fired: false,
                web_speed_factor: 0.0,
                web_range: 0.0,
                scram_strength: 0.0,
                scram_range: 0.0,
                neut_amount: 0.0,
                ecm_jammed: false,
                remote_shield_rep: 0.0,
                remote_armor_rep: 0.0,
                remote_rep_range: 0.0,
                tag_drop_type_id: None,
                spawn_position: pos,
                sleeper_wh_class: 0,
                custom_cargo_loot: None,
            });

            new_balls.push(ball_id);
        }

        with_mut_combat_runtime_with_locks(db, combat_ephemeral, |rt| {
            for entity in new_entities {
                rt.npc_entities.insert(entity.ball_id, entity);
            }
            if let Some(enc) = rt.encounter_waves.get_mut(&enc_id) {
                enc.trigger_ball_id = new_balls.first().copied().unwrap_or(0);
                enc.active_wave_ball_ids.extend(new_balls);
            }
        });
    }

    // 2. Remove from npc_entities, cache metadata for wreck/loot creation, and award bounty once
    let npc_opt =
        with_mut_combat_runtime_with_locks(db, combat_ephemeral, |rt| rt.npc_entities.remove(&killed_ball_id));
    if let Some(npc) = npc_opt {
        let now = db.now_unix_secs();
        let s_stage = db.with_snapshot(|snap| {
            crate::services::security_mgr::insurgency_system_suppression_stage_in_snap(snap, now, solar_system_id)
        });
        let supp_mult = 1.0 + 0.05 * f64::from(s_stage.min(5));
        let eff_bounty = npc.bounty_isk * supp_mult;
        let drop_drifter_sleeper_loot =
            npc.archetype == "Drifter" && npc.custom_cargo_loot.is_some();
        let drop_pos = npc.spawn_position;
        let drop_cls = npc.sleeper_wh_class.max(5);
        let base_bounty = npc.bounty_isk;
        let npc_type_id = npc.type_id;
        let npc_faction_id = npc.faction_id;
        let should_credit_bounty = solar_system_id < 31_000_000
            && npc.archetype != "Sleeper"
            && eff_bounty > 0.0
            && killer > 0;
        if should_credit_bounty {
            // Acquire db snapshot BEFORE combat_ephemeral to match lock hierarchy and persist
            // pending bounty ticks atomically even when killer sec status is already >= 5.0.
            with_mut_combat_runtime_tx_locks(db, combat_ephemeral, |snap, rt| {
                let credited = rt.bounty_credited_balls.insert(killed_ball_id);
                if credited {
                    let clock_now = db.clock().now_unix_secs();
                    let cw = &mut snap.combat_crimewatch_state;
                    cw.pending_bounty_tick_started_unix
                        .entry((solar_system_id, killer))
                        .or_insert(clock_now);
                    *cw.pending_bounty_ticks
                        .entry((solar_system_id, killer))
                        .or_insert(0.0) += eff_bounty;
                    cw.bounty_tick_next_flush_unix
                        .entry((solar_system_id, killer))
                        .or_insert_with(|| clock_now.saturating_add(1_200));

                    let old_sec = snap
                        .characters
                        .get(&killer)
                        .map(|c| c.security_status)
                        .unwrap_or(0.0);
                    if old_sec < 5.0 {
                        let gain =
                            crate::combat::npc_ai::compute_sec_status_gain(old_sec, base_bounty)
                                * supp_mult;
                        let new_sec = (old_sec + gain).min(5.0).clamp(-10.0, 10.0);
                        crate::services::combat_crimewatch::record_sec_status_change_in_snap(
                            snap,
                            killer,
                            crate::services::combat_crimewatch::SecStatusTransaction {
                                timestamp_unix: now,
                                old_sec,
                                new_sec,
                                delta: new_sec - old_sec,
                                reason: "NPC Pirate Bounty".into(),
                                solar_system_id,
                                event_type_id: crate::services::combat_crimewatch::crimewatch_timers::EVENT_SEC_STATUS_KILL_PIRATE_NPC,
                                other_owner_id: None,
                                other_type_id: Some(npc_type_id),
                            },
                        );
                    }
                }
                rt.destroyed_npc_cache.insert(killed_ball_id, npc);
            });
        } else {
            with_mut_combat_runtime_with_locks(db, combat_ephemeral, |rt| {
                rt.destroyed_npc_cache.insert(killed_ball_id, npc);
            });
        }
        if killer > 0 {
            let explicit_fid = (npc_faction_id > 0).then_some(npc_faction_id);
            if let Some(ctx) = notify_ctx {
                crate::services::agents_missions::agents_standings_arcs::apply_npc_kill_faction_standing_loss(
                    ctx,
                    solar_system_id,
                    killer,
                    npc_type_id,
                    explicit_fid,
                    None,
                    now,
                );
            } else {
                db.with_mut_snapshot(|snap| {
                    let _ = crate::services::agents_missions::agents_standings_arcs::apply_npc_kill_faction_standing_loss_in_snap_sde(
                        sde,
                        snap,
                        solar_system_id,
                        killer,
                        npc_type_id,
                        explicit_fid,
                        None,
                        now,
                    );
                });
            }
        }
        if drop_drifter_sleeper_loot && killer > 0 {
            let _ = crate::services::pve_exploration_sites::drop_sleeper_wreck_core(
                db,
                systems,
                sde,
                solar_system_id,
                killer,
                drop_pos,
                drop_cls,
            )
            .await;
        }
    }
}

/// Flushes all pending 20-minute (`1,200 s`) NPC bounty ticks that have matured by `now_unix`.
pub fn tick_overdue_npc_bounty_payouts(ctx: &ServerContext, now_unix: u64) {
    let due_keys: Vec<(u32, i64)> = ctx.db.with_snapshot(|snap| {
        let cw = &snap.combat_crimewatch_state;
        cw.pending_bounty_ticks
            .keys()
            .copied()
            .filter(|k| {
                let started = cw
                    .pending_bounty_tick_started_unix
                    .get(k)
                    .copied()
                    .unwrap_or(now_unix);
                now_unix.saturating_sub(started) >= 1_200
            })
            .collect()
    });
    for (sys_id, char_id) in due_keys {
        let _ = flush_npc_bounty_tick(ctx, sys_id, char_id, 0.0);
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct NpcBountyTickPayout {
    pub pilot_isk: f64,
    pub main_ess_isk: f64,
    pub reserve_ess_isk: f64,
}

/// Flushes the 20-minute (`1,200s`) batched NPC bounty tick for `(solar_system_id, char_id)`
/// applying the system's Dynamic Bounty System (`dbs_multiplier`), corporation tax, and K-space null-sec ESS split (`feat:combat.bounty-payout-npc`).
pub fn flush_npc_bounty_tick(
    ctx: &ServerContext,
    solar_system_id: u32,
    char_id: i64,
    corp_tax_rate: f64,
) -> NpcBountyTickPayout {
    use eve_db::{IskCents, WalletJournalEntry};
    use crate::formulas::compute_dbs_and_ess_bounty_split;

    let key = (solar_system_id, char_id);
    let raw_bounty = ctx.db.with_mut_snapshot(|snap| {
        let has_any = snap.combat_crimewatch_state.pending_bounty_ticks.contains_key(&key)
            || snap
                .combat_crimewatch_state
                .bounty_tick_next_flush_unix
                .contains_key(&key)
            || snap
                .combat_crimewatch_state
                .pending_bounty_tick_started_unix
                .contains_key(&key);
        if !has_any {
            return 0.0;
        }
        let cw = &mut snap.combat_crimewatch_state;
        cw.bounty_tick_next_flush_unix.remove(&key);
        cw.pending_bounty_tick_started_unix.remove(&key);
        cw.pending_bounty_ticks.remove(&key).unwrap_or(0.0)
    });
    if raw_bounty <= 0.0 {
        return NpcBountyTickPayout::default();
    }
    let sys_sec = ctx
        .sde
        .solar_systems
        .get(&solar_system_id)
        .map(|s| s.security_status)
        .unwrap_or(0.5);
    let is_kspace_nullsec = sys_sec <= 0.0 && solar_system_id < 31_000_000;
    let dbs_mult = if is_kspace_nullsec {
        ctx.db
            .with_snapshot(|s| {
                s.system_states
                    .get(&solar_system_id)
                    .map(|st| st.dbs_multiplier)
                    .filter(|&m| m > 0.0)
            })
            .unwrap_or(1.35)
    } else {
        1.0
    };

    let (char_corp_id, is_player_corp, resolved_tax_rate) = ctx.db.with_snapshot(|s| {
        let cid = s.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(0);
        match s.corporations.get(&cid) {
            Some(c) => (cid, true, c.tax_rate.clamp(0.0, 1.0)),
            None => (cid, false, 0.11),
        }
    });
    let modified_gross = raw_bounty * dbs_mult;
    let use_threshold = corp_tax_rate <= 0.0 || (corp_tax_rate - 0.11).abs() < 1e-6;
    let effective_tax_rate = if use_threshold && modified_gross < 100_000.0 {
        0.0
    } else if corp_tax_rate > 0.0 {
        corp_tax_rate.clamp(0.0, 1.0)
    } else {
        resolved_tax_rate
    };

    let (pilot_isk, tax_isk, main_ess, reserve_ess) = if is_kspace_nullsec {
        compute_dbs_and_ess_bounty_split(raw_bounty, dbs_mult, effective_tax_rate)
    } else {
        let tax = modified_gross * effective_tax_rate;
        (modified_gross - tax, tax, 0.0, 0.0)
    };

    ctx.db.with_mut_snapshot(|snap| {
        let pilot_cents = IskCents::from_f64_isk(pilot_isk);
        let tax_cents = IskCents::from_f64_isk(tax_isk);
        let main_cents = IskCents::from_f64_isk(main_ess);
        let reserve_cents = IskCents::from_f64_isk(reserve_ess);
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.balance = ch.balance.checked_add(pilot_cents).unwrap_or(ch.balance);
        }
        if is_player_corp && tax_cents.raw_cents() > 0 {
            if let Some(corp) = snap.corporations.get_mut(&char_corp_id) {
                corp.credit_division(1, tax_cents);
            }
        }
        if is_kspace_nullsec {
            if let Some(st) = snap.system_states.get_mut(&solar_system_id) {
                st.ess_main_bank = st
                    .ess_main_bank
                    .checked_add(main_cents)
                    .unwrap_or(st.ess_main_bank);
                crate::services::pve_ess_reserve_banks::record_ess_main_bank_contribution(
                    st,
                    char_id,
                    main_cents.0,
                    ctx.now_unix(),
                );
                st.ess_reserve_bank = st
                    .ess_reserve_bank
                    .checked_add(reserve_cents)
                    .unwrap_or(st.ess_reserve_bank);
            }
        }
        let bal_after = snap
            .characters
            .get(&char_id)
            .map(|c| c.balance)
            .unwrap_or(pilot_cents);
        snap.next_entity_id += 1;
        let jid = snap.next_entity_id;
        snap.wallet_journal.push(WalletJournalEntry {
            entry_id: jid,
            character_id: char_id,
            entry_type: "bounty_prizes".to_string(),
            amount: pilot_cents,
            balance_after: bal_after,
            reason: format!("20m Bounty Tick in system {solar_system_id} (DBS {dbs_mult:.2}x)"),
        });
        if tax_cents.raw_cents() > 0 {
            snap.next_entity_id += 1;
            let tjid = snap.next_entity_id;
            snap.wallet_journal.push(WalletJournalEntry {
                entry_id: tjid,
                character_id: char_id,
                entry_type: "bounty_prize_corporation_tax".to_string(),
                amount: IskCents::from_raw_cents(-tax_cents.raw_cents()),
                balance_after: bal_after,
                reason: format!("Corporation tax ({:.0}%) on bounty tick in {solar_system_id}", effective_tax_rate * 100.0),
            });
        }
    });
    if is_kspace_nullsec && raw_bounty > 0.0 {
        let _ = crate::services::combat_crimewatch::step_dynamic_bounty_multiplier(
            ctx,
            solar_system_id,
            raw_bounty,
            0.0,
        );
    }

    if pilot_isk >= 1.0 {
        crate::quasar_gateway::isolate_side_effect("combat_npc_encounters.career_bounty", || {
            crate::services::quasar_career_goals::record_bounty_career_event(
                ctx,
                char_id,
                pilot_isk.round() as u64,
            );
        });
    }

    NpcBountyTickPayout {
        pilot_isk,
        main_ess_isk: main_ess,
        reserve_ess_isk: reserve_ess,
    }
}

/// Resolves SDE special module or BPC drop for Officer, Faction Commander, and Mordu's Legion NPCs (`feat:npc.officer-npc`, `feat:npc.belt-rats`).
pub fn resolve_officer_or_commander_sde_drop(ctx: &ServerContext, npc_type_id: u32) -> Option<(u32, bool)> {
    let sde_type = ctx.sde.types.get(&npc_type_id)?;
    match sde_type.group_id {
        1285 => return Some((33817, true)), // Garmur Blueprint (1-run BPC)
        1286 => return Some((33819, true)), // Orthrus Blueprint (1-run BPC)
        1287 => return Some((33821, true)), // Barghest Blueprint (1-run BPC)
        _ => {}
    }
    if matches!(sde_type.group_id, 553 | 559 | 564 | 569 | 574 | 1174 | 4795..=4804) {
        let first_name = sde_type.name.split_whitespace().next().unwrap_or("");
        let mut candidates: Vec<u32> = ctx
            .sde
            .types
            .values()
            .filter(|t| {
                t.published
                    && t.category_id == 7
                    && t.meta_group_id == Some(5)
                    && !first_name.is_empty()
                    && t.name.starts_with(first_name)
            })
            .map(|t| t.type_id)
            .collect();
        if candidates.is_empty() {
            candidates = ctx
                .sde
                .types
                .values()
                .filter(|t| t.published && t.category_id == 7 && t.meta_group_id == Some(5))
                .map(|t| t.type_id)
                .collect();
        }
        candidates.sort_unstable();
        if !candidates.is_empty() {
            let draw = ctx.mechanic_rng.try_next_u64().unwrap_or(0) as usize;
            return Some((candidates[draw % candidates.len()], false));
        }
    }
    let faction_prefix = if sde_type.name.contains("Dread Guristas") {
        Some("Dread Guristas")
    } else if sde_type.name.contains("Shadow Serpentis") {
        Some("Shadow Serpentis")
    } else if sde_type.name.contains("Dark Blood") {
        Some("Dark Blood")
    } else if sde_type.name.contains("True Sansha") {
        Some("True Sansha")
    } else if sde_type.name.contains("Domination") {
        Some("Domination")
    } else {
        None
    };
    if let Some(prefix) = faction_prefix {
        let mut candidates: Vec<u32> = ctx
            .sde
            .types
            .values()
            .filter(|t| {
                t.published
                    && t.category_id == 7
                    && t.meta_group_id == Some(4)
                    && t.name.starts_with(prefix)
            })
            .map(|t| t.type_id)
            .collect();
        candidates.sort_unstable();
        if !candidates.is_empty() {
            let draw = ctx.mechanic_rng.try_next_u64().unwrap_or(0) as usize;
            return Some((candidates[draw % candidates.len()], false));
        }
    }
    None
}

/// DERIVED: Resolves a fallback NPC faction ID for generic wreck loot selection when
/// `sde_types.faction_id` is `NULL` (for example `/spawnnpc Pithi Invader`, type `16997`).
/// The name/group/wreck-title keyword table below is a synthesized fallback mapping rather than
/// an imported CCP faction-drop relation, followed by runtime `npc_faction_id`, `spawn_faction_id`,
/// and `race_id` fallbacks.
fn resolve_npc_faction_id_for_loot(
    ctx: &ServerContext,
    npc_type_id: u32,
    npc_faction_id: Option<u32>,
    spawn_faction_id: Option<u32>,
) -> u32 {
    if let Some(fid) = ctx
        .sde
        .types
        .get(&npc_type_id)
        .and_then(|t| t.faction_id)
        .filter(|&f| f > 0)
    {
        return fid;
    }
    let group_name = ctx
        .sde
        .types
        .get(&npc_type_id)
        .and_then(|t| ctx.sde.groups.get(&t.group_id))
        .map(|g| g.name.as_str())
        .unwrap_or("");
    let type_name = ctx
        .sde
        .types
        .get(&npc_type_id)
        .map(|t| t.name.as_str())
        .unwrap_or("");
    let wreck_name = ctx
        .sde
        .resolve_wreck_type_id(npc_type_id, None)
        .and_then(|wid| ctx.sde.types.get(&wid))
        .map(|w| w.name.as_str())
        .unwrap_or("");

    let keyword_table: &[(&[&str], u32)] = &[
        (&["Guristas", "Pithi", "Pithum", "Pith"], 500_010),
        (&["Angel", "Gist", "Domination"], 500_011),
        (&["Blood", "Corpus", "Corpior", "Corpatis"], 500_012),
        (&["Sansha", "Centii", "Centum", "Centus"], 500_019),
        (&["Serpentis", "Coreli", "Corelum", "Corelior", "Corelatis"], 500_020),
        (&["Caldari"], 500_001),
        (&["Minmatar"], 500_002),
        (&["Amarr"], 500_003),
        (&["Gallente"], 500_004),
    ];
    for text in [group_name, type_name, wreck_name] {
        for &(tokens, fid) in keyword_table {
            if tokens.iter().any(|tok| text.contains(tok)) {
                return fid;
            }
        }
    }
    npc_faction_id
        .filter(|&f| f > 0)
        .or_else(|| spawn_faction_id.filter(|&f| f > 0))
        .or_else(|| {
            ctx.sde
                .types
                .get(&npc_type_id)
                .and_then(|t| t.race_id)
                .and_then(|rid| match rid {
                    1 => Some(500_001),
                    2 => Some(500_002),
                    4 => Some(500_003),
                    8 => Some(500_004),
                    _ => None,
                })
        })
        .unwrap_or(0)
}

/// DERIVED: Synthesized default fallback loot table for killed NPC entities (`category_id == 11`
/// or tracked NPC spawn) when specialized SDE/mission/convoy/Sleeper/Triglavian loot tables leave
/// `loot_to_insert` empty. Uses clamped `type_materials` (`1..=50`), synthesized `Metal Scraps`
/// (`15_331` x10, or `34` fallback), and hardcoded faction-matched ammo x100 matching `npc_ai.rs`
/// rather than imported CCP NPC loot drop tables.
fn append_default_npc_wreck_loot(
    ctx: &ServerContext,
    npc_type_id: u32,
    faction_id: u32,
    loot_to_insert: &mut Vec<(u32, i32, bool)>,
) {
    if let Some(mats) = ctx.sde.type_materials.get(&npc_type_id) {
        for m in mats {
            let qty = i32::try_from(m.quantity.clamp(1, 50)).unwrap_or(50);
            if qty > 0 && !loot_to_insert.iter().any(|&(tid, _, _)| tid == m.material_type_id) {
                loot_to_insert.push((m.material_type_id, qty, false));
            }
        }
    }
    let scrap_tid = if ctx.sde.types.contains_key(&15_331) {
        15_331
    } else {
        34
    };
    if ctx.sde.types.contains_key(&scrap_tid)
        && !loot_to_insert.iter().any(|&(tid, _, _)| tid == scrap_tid)
    {
        loot_to_insert.push((scrap_tid, 10, false));
    }
    let ammo_type_id = match faction_id {
        500_010 | 500_001 => 204,
        500_012 | 500_019 | 500_003 => 178,
        500_020 | 500_004 => 230,
        500_011 | 500_002 => 213,
        _ => 212,
    };
    if ctx.sde.types.contains_key(&ammo_type_id)
        && !loot_to_insert.iter().any(|&(tid, _, _)| tid == ammo_type_id)
    {
        loot_to_insert.push((ammo_type_id, 100, false));
    }
}

/// Populates specialized NPC loot, convoy cargo, cooldowns, and wave hooks when an NPC is killed via turret/spool damage (`combat_weapons_spool.rs`).
pub async fn populate_npc_wreck_on_turret_kill(
    ctx: &ServerContext,
    solar_system_id: SolarSystemId,
    npc_ball_id: i64,
    npc_type_id: u32,
    pos: DVec3,
    wreck_id: i64,
    killer_char_id: i64,
    dropped_entries: &mut Vec<eve_db::KillmailItemEntry>,
) -> String {
    let npc_opt = with_mut_combat_runtime(ctx, |rt| {
        rt.npc_entities
            .remove(&npc_ball_id)
            .or_else(|| rt.destroyed_npc_cache.remove(&npc_ball_id))
    });
    let respawn_at = ctx.now_unix().saturating_add(1_200);
    let mut spawn_faction_id = None;
    if let Ok(mut spawns) = ctx.npc_spawns.write() {
        if let Some(mgr) = spawns.get_mut(&solar_system_id) {
            if let Some(removed) = mgr.entities.remove(&npc_ball_id) {
                if removed.faction_id > 0 {
                    spawn_faction_id = Some(removed.faction_id);
                }
                let group_empty = !mgr
                    .entities
                    .values()
                    .any(|e| e.spawn_group_id == removed.spawn_group_id);
                if group_empty {
                    mgr.respawn_timers.insert(
                        (solar_system_id, removed.spawn_group_id),
                        respawn_at,
                    );
                }
            }
        }
    }
    let convoy_opt = ctx.living_world.get_convoy(npc_ball_id);
    if convoy_opt.is_some() {
        if wreck_id > 0 {
            ctx.living_world
                .mark_convoy_destroyed_in_combat(npc_ball_id);
        } else {
            let _ = ctx
                .living_world
                .interdict_convoy(npc_ball_id, killer_char_id, &ctx.db);
            ctx.living_world.mark_convoy_destroyed(npc_ball_id);
        }
    }

    let archetype = npc_opt
        .as_ref()
        .map(|n| n.archetype.clone())
        .unwrap_or_else(|| {
            if convoy_opt.is_some() {
                "Hauler".to_string()
            } else {
                crate::services::combat_crimewatch::combat_npc_ai::derive_npc_archetype(ctx, npc_type_id)
                    .to_string()
            }
        });
    if archetype == "CloneSoldier" {
        let until = ctx.now_unix().saturating_add(1_200);
        with_mut_combat_runtime(ctx, |rt| {
            rt.clone_soldier_cooldown_until.insert(solar_system_id, until);
        });
    }

    let mut loot_to_insert: Vec<(u32, i32, bool)> = Vec::new();
    if let Some(tag_tid) = npc_opt.as_ref().and_then(|n| n.tag_drop_type_id) {
        loot_to_insert.push((tag_tid, 1, false));
    }
    if let Some((custom_tid, custom_qty)) = npc_opt.as_ref().and_then(|n| n.custom_cargo_loot) {
        let bounded_qty = ItemQty::try_from_i64(custom_qty.max(1))
            .and_then(ItemQty::try_into_stack_i32)
            .unwrap_or(i32::MAX);
        loot_to_insert.push((custom_tid, bounded_qty, false));
    } else if let Some(ref conv) = convoy_opt {
        if conv.cargo_quantity > 0 {
            loot_to_insert.push((conv.cargo_type_id, conv.cargo_quantity, false));
        }
    }
    if let Some((special_tid, is_bpc)) = resolve_officer_or_commander_sde_drop(ctx, npc_type_id) {
        loot_to_insert.push((special_tid, 1, is_bpc));
    }
    crate::services::insurgency_complex_ops::append_insurgency_npc_wreck_loot(
        npc_type_id,
        &mut loot_to_insert,
    );
    if archetype == "Sleeper" {
        loot_to_insert.push((30744, 1, false));
        loot_to_insert.push((30259, 1, false));
    } else if archetype == "PochvenHypnosianSleeper" {
        if let Some((&tid, _)) = ctx.sde.types.iter().find(|(_, t)| t.name == "Interstellar Navigation Logs") {
            loot_to_insert.push((tid, 1, false));
        }
    } else if archetype == "Triglavian" {
        loot_to_insert.push((48121, 1, false));
    }

    let is_npc_entity = ctx
        .sde
        .types
        .get(&npc_type_id)
        .is_some_and(|t| t.category_id == 11)
        || npc_opt.is_some()
        || spawn_faction_id.is_some();
    if wreck_id > 0 && is_npc_entity && loot_to_insert.is_empty() {
        let effective_faction_id = resolve_npc_faction_id_for_loot(
            ctx,
            npc_type_id,
            npc_opt.as_ref().map(|n| n.faction_id),
            spawn_faction_id,
        );
        append_default_npc_wreck_loot(ctx, npc_type_id, effective_faction_id, &mut loot_to_insert);
    }

    if wreck_id > 0 && !loot_to_insert.is_empty() {
        ctx.db.with_mut_snapshot(|snap| {
            for &(ltid, qty, is_bpc) in &loot_to_insert {
                snap.next_entity_id += 1;
                let lid = snap.next_entity_id;
                snap.items.insert(
                    lid,
                    eve_db::InventoryItemRecord {
                        item_id: lid,
                        type_id: ltid,
                        owner_id: killer_char_id,
                        location_id: wreck_id,
                        flag: eve_db::InventoryFlag::Cargo,
                        quantity: qty,
                        singleton: is_bpc,
                        version: 1,
                        dynamic_attributes: std::collections::HashMap::new(),
                        source_mutaplasmid_id: None,
                        insured_payout_isk: 0.0,
                        is_blueprint_copy: is_bpc,
                        blueprint_runs_remaining: is_bpc.then_some(1),
                    },
                );
                dropped_entries.push(eve_db::KillmailItemEntry {
                    type_id: ltid,
                    flag: eve_db::InventoryFlag::Cargo as u16,
                    qty_dropped: qty,
                    qty_destroyed: 0,
                    singleton: is_bpc,
                });
            }
        });
    }

    step_encounter_wave_on_kill(ctx, solar_system_id, npc_ball_id, pos).await;
    crate::services::space_mining_harvest::on_wormhole_ore_guard_destroyed_db(&ctx.db, npc_ball_id);
    crate::services::pve_exploration::handle_pve_site_npc_destruction(
        ctx,
        solar_system_id,
        npc_ball_id,
        killer_char_id,
    )
    .await;
    crate::services::insurgency_complex_ops::on_insurgency_npc_destroyed(
        ctx,
        solar_system_id,
        npc_ball_id,
        npc_type_id,
        wreck_id,
        killer_char_id,
    )
    .await;
    if is_npc_entity && killer_char_id > 0 {
        let explicit_fid = npc_opt
            .as_ref()
            .map(|n| n.faction_id)
            .filter(|&f| f > 0)
            .or_else(|| spawn_faction_id.filter(|&f| f > 0));
        crate::services::agents_missions::agents_standings_arcs::apply_npc_kill_faction_standing_loss(
            ctx,
            solar_system_id,
            killer_char_id,
            npc_type_id,
            explicit_fid,
            None,
            ctx.now_unix(),
        );
    }
    archetype
}

/// Spawns space wrecks and specialized loot for NPCs destroyed by `ActiveModuleCycle` weapon ticks.
pub async fn finalize_cycle_destroyed_npcs(ctx: &ServerContext, solar_system_id: SolarSystemId) {
    let has_pending = with_combat_runtime(ctx, |rt| {
        rt.destroyed_npc_cache
            .values()
            .any(|n| n.solar_system_id == solar_system_id)
    });
    if !has_pending {
        return;
    }
    let pending: Vec<ExtendedNpcEntity> = with_mut_combat_runtime(ctx, |rt| {
        let ids: Vec<i64> = rt
            .destroyed_npc_cache
            .iter()
            .filter(|(_, n)| n.solar_system_id == solar_system_id)
            .map(|(&id, _)| id)
            .collect();
        ids.into_iter()
            .filter_map(|id| rt.destroyed_npc_cache.remove(&id))
            .collect()
    });
    if pending.is_empty() {
        return;
    }
    let handle = ctx.systems.get_or_spawn(solar_system_id);
    let now_unix = ctx.now_unix();
    for npc in pending {
        with_mut_combat_runtime(ctx, |rt| {
            rt.destroyed_npc_cache.insert(npc.ball_id, npc.clone());
        });
        let killer_char_id = npc
            .threat_table
            .iter()
            .chain(npc.aggression_threat.iter())
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(&k, _)| k)
            .or(npc.target_id)
            .and_then(|killer_id| {
                let effective_killer_id =
                    crate::services::agents_missions::agents_standings_arcs::resolve_runtime_killer_char_id(
                        ctx,
                        killer_id,
                    );
                ctx.db.with_snapshot(|s| {
                    if s.characters.contains_key(&effective_killer_id) {
                        Some(effective_killer_id)
                    } else if let Some(ship) = s.get_any_item(effective_killer_id) {
                        if s.characters.contains_key(&ship.owner_id) {
                            Some(ship.owner_id)
                        } else {
                            None
                        }
                    } else if let Some((&char_id, _)) = s
                        .characters
                        .iter()
                        .find(|(_, c)| c.active_ship_id == effective_killer_id)
                    {
                        Some(char_id)
                    } else {
                        None
                    }
                })
            })
            .unwrap_or(0);
        if (32_000_000..=32_999_999).contains(&solar_system_id) {
            let mut dropped = Vec::new();
            let _ = populate_npc_wreck_on_turret_kill(
                ctx,
                solar_system_id,
                npc.ball_id,
                npc.type_id,
                npc.spawn_position,
                0,
                killer_char_id,
                &mut dropped,
            )
            .await;
            continue;
        }
        let wreck_tid = ctx.sde.resolve_wreck_type_id(npc.type_id, None).unwrap_or_else(|| {
            match npc.archetype.as_str() {
                "Drifter" => 37531,
                "Sleeper" => 30495,
                _ => 26557,
            }
        });
        let wreck_id = ctx.db.with_mut_snapshot(|snap| {
            snap.next_entity_id += 1;
            let wid = snap.next_entity_id;
            crate::sp6_lifecycle::record_wreck_or_container_meta_in_snap(snap, wid, now_unix, true);
            snap.items.insert(
                wid,
                eve_db::InventoryItemRecord {
                    item_id: wid,
                    type_id: wreck_tid,
                    owner_id: killer_char_id,
                    location_id: solar_system_id as i64,
                    flag: eve_db::InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.item_positions
                .insert(wid, [npc.spawn_position.x, npc.spawn_position.y, npc.spawn_position.z]);
            wid
        });
        let wreck_sde = ctx.sde.types.get(&wreck_tid);
        let wreck_group_id = wreck_sde.map(|t| t.group_id).unwrap_or(186);
        let wreck_category_id = wreck_sde.map(|t| t.category_id).unwrap_or(2);
        let wreck_name = wreck_sde
            .map(|t| t.name.clone())
            .filter(|name| !name.is_empty());
        let is_abandoned = killer_char_id == 0
            || ctx
                .crimewatch
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .get_or_create(killer_char_id)
                .is_criminal(now_unix);
        let _ = handle
            .spawn_ball_with_meta(
                eve_destiny::Ball {
                    id: wreck_id,
                    type_id: wreck_tid,
                    position: npc.spawn_position,
                    velocity: DVec3::ZERO,
                    max_velocity: 0.0,
                    inertia: 1.0,
                    mass: 1_000.0,
                    radius: 25.0,
                    speed_fraction: 0.0,
                    mode: eve_destiny::MovementMode::Stop,
                },
                ctx.build_type_combat_state(wreck_tid),
                eve_destiny::SlimItemMeta {
                    group_id: wreck_group_id,
                    category_id: wreck_category_id,
                    owner_id: killer_char_id,
                    char_id: None,
                    corp_id: -1,
                    alliance_id: -1,
                    security_status: 0.0,
                    is_cloaked: false,
                    name: wreck_name,
                    ..Default::default()
                },
            )
            .await;
        crate::services::combat_crimewatch::register_container_lifetime(
            ctx,
            wreck_id,
            solar_system_id,
            wreck_tid,
            killer_char_id,
            now_unix,
            is_abandoned,
        );
        let mut dropped = Vec::new();
        let _ = populate_npc_wreck_on_turret_kill(
            ctx,
            solar_system_id,
            npc.ball_id,
            npc.type_id,
            npc.spawn_position,
            wreck_id,
            killer_char_id,
            &mut dropped,
        )
        .await;
    }
}

pub fn record_npc_kill_progression(
    ctx: &ServerContext,
    killer_char_id: i64,
    npc_type_id: Option<eve_sde::TypeId>,
) {
    if killer_char_id <= 0 {
        return;
    }
    crate::quasar_gateway::isolate_side_effect("combat_npc_encounters.npc_destroyed_progression", || {
        ctx.quasar_gateway.progression.daily_goals.on_gameplay_event(
            killer_char_id as u32,
            1,
            1,
            &ctx.quasar_gateway,
        );
        let ship_group_id = crate::services::quasar_career_goals::character_active_ship_group_id(
            &ctx.db,
            &ctx.sde,
            killer_char_id,
        );
        ctx.quasar_gateway.progression.career_goals.on_career_gameplay_event(
            killer_char_id as u32,
            crate::services::quasar_career_goals::CareerGameplayEvent {
                activity_kind: "combat",
                type_id: npc_type_id,
                ship_group_id,
                ..Default::default()
            },
            1,
            &ctx.quasar_gateway,
        );
        if npc_type_id.is_some_and(|tid| {
            crate::services::combat_crimewatch::combat_npc_ai::derive_npc_archetype(ctx, tid)
                == "Triglavian"
        }) {
            crate::services::quasar_career_goals::record_career_sub_kind(
                ctx,
                killer_char_id,
                "abyssal",
                Some("kill_triglavians"),
                1,
            );
        }
        ctx.quasar_gateway.progression.achievements.on_gameplay_event_by_name(
            killer_char_id as u32,
            &["destroyHostileNPC", "entityGroupDestroyed"],
            1,
            ctx.now_unix(),
            &ctx.quasar_gateway,
        );
        crate::services::social_corp_fleet::social_corp_projects::record_server_project_activity(
            ctx,
            killer_char_id,
            &["DamageNPCShips", "DestroyNPCShips"],
            1,
            npc_type_id.map(|t| t as i64),
        );
        let tid = npc_type_id.unwrap_or(0);
        let gid = ctx.sde.types.get(&tid).map(|t| t.group_id).unwrap_or(0);
        let params = [("typeID", tid.to_string()), ("groupID", gid.to_string())];
        crate::services::operations_manager_helpers::record_server_operation_event(
            ctx,
            killer_char_id,
            "somethingExplode",
            &params,
            1,
        );
    });
}

#[cfg(test)]
#[path = "combat_npc_encounters_tests.rs"] mod combat_npc_encounters_tests;

#[path = "combat_npc_encounters_cov_unit_tests.rs"]
#[cfg(test)]
mod combat_npc_encounters_cov_unit_tests;

#[path = "combat_npc_encounters_edges2_tests.rs"]
#[cfg(test)]
mod combat_npc_encounters_edges2_tests;
