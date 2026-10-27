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
use crate::services::combat_crimewatch::{with_mut_combat_runtime, CombatRuntimeState, EncounterWaveDirector, ExtendedNpcEntity};
use crate::system_actor::SystemRegistry;
use eve_db::WorldRepository;
use eve_sde::{SdeRegistry, SolarSystemId};
use glam::DVec3;
use std::sync::{Arc, RwLock};

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

/// Invoked from `npc_death_hook` when any NPC ball is destroyed in space by damage (`apply_damage_to_ball` / `step_combat_system`).
pub async fn handle_npc_destroyed_in_space(
    systems: &Arc<SystemRegistry>,
    db: &Arc<WorldRepository>,
    sde: &Arc<SdeRegistry>,
    combat_ephemeral: &Arc<RwLock<CombatRuntimeState>>,
    solar_system_id: SolarSystemId,
    killed_ball_id: i64,
    killer_char_id: Option<i64>,
) {
    let killer = killer_char_id.unwrap_or(0);

    // 1. Advance encounter waves if killed_ball_id belongs to an active encounter
    let triggered_info: Option<(u32, u32, Vec<u32>, DVec3)> = {
        let mut rt = combat_ephemeral.write().unwrap_or_else(|e| e.into_inner());
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
    };

    if let Some((enc_id, _wave_num, next_types, base_pos)) = triggered_info {
        let handle = systems.get_or_spawn(solar_system_id);
        let mut new_balls = Vec::new();
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

            {
                let mut rt = combat_ephemeral.write().unwrap_or_else(|e| e.into_inner());
                rt.npc_entities.insert(
                    ball_id,
                    ExtendedNpcEntity {
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
                    },
                );
            }

            new_balls.push(ball_id);
        }

        {
            let mut rt = combat_ephemeral.write().unwrap_or_else(|e| e.into_inner());
            if let Some(enc) = rt.encounter_waves.get_mut(&enc_id) {
                enc.trigger_ball_id = new_balls.first().copied().unwrap_or(0);
                enc.active_wave_ball_ids.extend(new_balls);
            }
        }
    }

    // 2. Remove from npc_entities, cache metadata for wreck/loot creation, and award bounty once
    let npc_opt = {
        let mut rt = combat_ephemeral.write().unwrap_or_else(|e| e.into_inner());
        rt.npc_entities.remove(&killed_ball_id)
    };
    if let Some(npc) = npc_opt {
        let mut rt = combat_ephemeral.write().unwrap_or_else(|e| e.into_inner());
        if solar_system_id < 31_000_000
            && npc.archetype != "Sleeper"
            && npc.bounty_isk > 0.0
            && killer > 0
            && rt.bounty_credited_balls.insert(killed_ball_id)
        {
            *rt.pending_bounty_ticks
                .entry((solar_system_id, killer))
                .or_insert(0.0) += npc.bounty_isk;
        }
        rt.destroyed_npc_cache.insert(killed_ball_id, npc);
    }
}

/// Flushes the 20-minute (`1,200s`) batched NPC bounty tick for `(solar_system_id, char_id)`
/// applying the system's Dynamic Bounty System (`dbs_multiplier`), corporation tax, and K-space null-sec ESS split (`feat:combat.bounty-payout-npc`).
pub fn flush_npc_bounty_tick(
    ctx: &ServerContext,
    solar_system_id: u32,
    char_id: i64,
    corp_tax_rate: f64,
) -> (f64, f64, f64) {
    use eve_db::{IskCents, WalletJournalEntry};
    use crate::formulas::compute_dbs_and_ess_bounty_split;

    let raw_bounty = with_mut_combat_runtime(ctx, |rt| {
        rt.pending_bounty_ticks
            .remove(&(solar_system_id, char_id))
            .unwrap_or(0.0)
    });
    if raw_bounty <= 0.0 {
        return (0.0, 0.0, 0.0);
    }
    let sys_sec = ctx
        .sde
        .solar_systems
        .get(&solar_system_id)
        .map(|s| s.security_status)
        .unwrap_or(0.5);
    let is_kspace_nullsec = sys_sec <= 0.0 && solar_system_id < 31_000_000;
    let dbs_mult = ctx
        .db
        .with_snapshot(|s| {
            s.system_states
                .get(&solar_system_id)
                .map(|st| st.dbs_multiplier)
        })
        .unwrap_or(1.35);

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
                corp.ensure_wallet_divisions();
                corp.master_wallet = corp.master_wallet.checked_add(tax_cents).unwrap_or(corp.master_wallet);
                if let Some(d0) = corp.wallet_divisions.first_mut() {
                    d0.balance = d0.balance.checked_add(tax_cents).unwrap_or(d0.balance);
                }
            }
        }
        if is_kspace_nullsec {
            if let Some(st) = snap.system_states.get_mut(&solar_system_id) {
                st.ess_main_bank = st
                    .ess_main_bank
                    .checked_add(main_cents)
                    .unwrap_or(st.ess_main_bank);
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

    (pilot_isk, main_ess, reserve_ess)
}

