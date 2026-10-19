//! `crimewatch` — CrimeWatch 2.0 state machine, weapon/PvP timers, suspect/criminal flags, and CONCORD response (`C-8`).

#![forbid(unsafe_code)]

use crate::combat::death_pipeline::{execute_death_pipeline, DeathPipelineResult};
use crate::formulas::concord_response_delay_sec;
use crate::router::ServerContext;
use eve_db::{IskCents, KillRightRecord};
use eve_destiny::{Ball, MovementMode};
use glam::DVec3;
use std::collections::HashMap;

/// CrimeWatch timers and flags for a single character.
#[derive(Debug, Clone, Default)]
pub struct CharacterCrimeWatchState {
    pub character_id: i64,
    /// 60-second weapon timer: blocks docking, jumping stargates, or ejecting.
    pub weapon_timer_until_unix: u64,
    /// 15-minute (900s) PvP aggression timer: prevents safe logoff and emergency warp cloaking.
    pub pvp_timer_until_unix: u64,
    /// 15-minute (900s) Suspect flag: allows free engagement by any pilot in high-sec / low-sec.
    pub suspect_flag_until_unix: u64,
    /// 15-minute (900s) Criminal flag: free engagement everywhere + triggers CONCORD in high-sec.
    pub criminal_flag_until_unix: u64,
    /// Pending CONCORD arrival: `(system_id, arrival_unix, criminal_ship_id, victim_char_id)`.
    pub pending_concord_arrival: Option<(u32, u64, i64, i64)>,
}

impl CharacterCrimeWatchState {
    pub fn is_weapon_timer_active(&self, now_unix: u64) -> bool {
        self.weapon_timer_until_unix > now_unix
    }

    pub fn is_pvp_active(&self, now_unix: u64) -> bool {
        self.pvp_timer_until_unix > now_unix
    }

    pub fn is_suspect(&self, now_unix: u64) -> bool {
        self.suspect_flag_until_unix > now_unix
    }

    pub fn is_criminal(&self, now_unix: u64) -> bool {
        self.criminal_flag_until_unix > now_unix
    }
}

/// System-wide CrimeWatch director managing timers, legality evaluations, and CONCORD spawns.
#[derive(Debug, Default)]
pub struct CrimeWatchDirector {
    pub states: HashMap<i64, CharacterCrimeWatchState>,
}

impl CrimeWatchDirector {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get_or_create(&mut self, character_id: i64) -> &mut CharacterCrimeWatchState {
        self.states
            .entry(character_id)
            .or_insert_with(|| CharacterCrimeWatchState {
                character_id,
                ..Default::default()
            })
    }

    /// Records weapon activation (e.g. firing turret or launcher): grants 60-second weapon timer.
    pub fn trigger_weapon_timer(&mut self, character_id: i64, now_unix: u64) {
        let st = self.get_or_create(character_id);
        st.weapon_timer_until_unix = now_unix.saturating_add(60);
    }

    /// Evaluates aggression against another pilot:
    /// - High-sec unprovoked attack (`sys_sec >= 0.45`): Criminal flag (15m), weapon timer (60s), PvP timer (15m), schedules CONCORD.
    /// - Low-sec unprovoked attack (`0.0 < sys_sec < 0.45`): Suspect flag (15m), weapon timer (60s), PvP timer (15m).
    /// - Legal aggression (active war or suspect victim): Weapon timer (60s), PvP timer (15m).
    pub fn handle_combat_aggression(
        &mut self,
        character_id: i64,
        victim_char_id: i64,
        ship_id: i64,
        sys_id: u32,
        sys_sec: f64,
        is_legal: bool,
        now_unix: u64,
    ) -> AggressionEvaluation {
        let st = self.get_or_create(character_id);
        st.weapon_timer_until_unix = now_unix.saturating_add(60);
        st.pvp_timer_until_unix = now_unix.saturating_add(900);

        if is_legal {
            return AggressionEvaluation::LegalEngagement;
        }

        if sys_sec >= 0.45 {
            // High-sec illegal attack -> Criminal flag + CONCORD response
            st.criminal_flag_until_unix = now_unix.saturating_add(900);
            let delay_sec = concord_response_delay_sec(sys_sec);
            let arrival_unix = now_unix.saturating_add(delay_sec.ceil() as u64);
            st.pending_concord_arrival = Some((sys_id, arrival_unix, ship_id, victim_char_id));

            AggressionEvaluation::CriminalFlagTriggered {
                concord_delay_sec: delay_sec,
                arrival_unix,
            }
        } else if sys_sec > 0.0 {
            // Low-sec unprovoked attack -> Suspect flag
            st.suspect_flag_until_unix = now_unix.saturating_add(900);
            AggressionEvaluation::SuspectFlagTriggered
        } else {
            // Null-sec / W-space: no CrimeWatch penalties
            AggressionEvaluation::LegalEngagement
        }
    }

    /// Checks and executes any pending CONCORD response arrivals:
    /// - Spawns real CONCORD police battleships (Police Commander, type 11957).
    /// - Destroys the criminal's ship via the real death pipeline (`C-3`), removing instant ball deletion!
    /// - Grants 30-day Kill Right to the victim and applies security status penalty.
    pub async fn step_concord(
        &mut self,
        ctx: &ServerContext,
        now_unix: u64,
        rng_seed: u64,
    ) -> Vec<ConcordExecutionReport> {
        let mut ready = Vec::new();
        for (&char_id, st) in self.states.iter_mut() {
            if let Some((sys_id, arrival_unix, ship_id, victim_char_id)) = st.pending_concord_arrival {
                if now_unix >= arrival_unix {
                    ready.push((char_id, sys_id, ship_id, victim_char_id));
                    st.pending_concord_arrival = None;
                }
            }
        }

        let mut reports = Vec::new();

        for (criminal_char_id, sys_id, criminal_ship_id, victim_char_id) in ready {
            let sys_handle = ctx.systems.get_or_spawn(sys_id);

            let crim_pos = sys_handle
                .get_ball_snapshot(criminal_ship_id)
                .await
                .ok()
                .flatten()
                .map(|(_, b)| b.position)
                .unwrap_or(DVec3::ZERO);

            // 1. Spawn real CONCORD Police Battleship in the ballpark
            let concord_ball_id = ctx.db.with_mut_snapshot(|snap| {
                snap.next_entity_id += 1;
                snap.next_entity_id
            });
            let concord_pos = crim_pos + DVec3::new(200.0, 0.0, 200.0);
            let concord_ball = Ball {
                id: concord_ball_id,
                type_id: 11957, // CONCORD Police Commander
                position: concord_pos,
                velocity: DVec3::ZERO,
                max_velocity: 1_200.0,
                inertia: 0.1,
                mass: 10_000_000.0,
                radius: 120.0,
                speed_fraction: 1.0,
                mode: MovementMode::Stop,
            };
            let _ = sys_handle
                .spawn_ball_with_combat(concord_ball, ctx.build_type_combat_state(11957))
                .await;

            // 2. CONCORD destroys the criminal ship through the canonical death pipeline (C-3)
            let death_res = execute_death_pipeline(
                ctx,
                sys_id,
                criminal_char_id,
                criminal_ship_id,
                1_000_125, // Attacker: CONCORD
                crim_pos,
                rng_seed,
            )
            .await;

            // 3. Apply character security status penalty & grant 30-day Kill Right to victim
            let kill_right_id = ctx.db.with_mut_snapshot(|snap| {
                if let Some(crim) = snap.characters.get_mut(&criminal_char_id) {
                    crim.security_status = (crim.security_status - 0.5).max(-10.0);
                }
                snap.next_entity_id += 1;
                let kr_id = snap.next_entity_id;
                snap.kill_rights.insert(
                    kr_id,
                    KillRightRecord {
                        kill_right_id: kr_id,
                        owner_id: victim_char_id,
                        target_id: criminal_char_id,
                        activation_fee: IskCents::from_isk(1_000_000),
                        is_active_suspect: false,
                    },
                );
                kr_id
            });

            reports.push(ConcordExecutionReport {
                criminal_char_id,
                victim_char_id,
                concord_ball_id,
                kill_right_id,
                death_result: death_res,
            });
        }

        reports
    }
}

/// Evaluation result from combat aggression against another character.
#[derive(Debug, Clone, PartialEq)]
pub enum AggressionEvaluation {
    LegalEngagement,
    SuspectFlagTriggered,
    CriminalFlagTriggered {
        concord_delay_sec: f64,
        arrival_unix: u64,
    },
}

/// Report emitted when CONCORD arrives and executes a criminal.
#[derive(Debug, Clone)]
pub struct ConcordExecutionReport {
    pub criminal_char_id: i64,
    pub victim_char_id: i64,
    pub concord_ball_id: i64,
    pub kill_right_id: i64,
    pub death_result: Result<DeathPipelineResult, String>,
}
