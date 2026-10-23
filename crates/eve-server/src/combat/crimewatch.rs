//! `crimewatch` — CrimeWatch 2.0 state machine, weapon/PvP timers, suspect/criminal flags, and CONCORD response (`C-8`).

#![forbid(unsafe_code)]

use crate::combat::death_pipeline::{execute_death_pipeline, DeathPipelineResult};
use crate::formulas::concord_response_delay_sec;
use crate::router::ServerContext;
pub use eve_db::{
    IskCents, KillRightRecord, PersistedAggressionRecord as AggressionRecord,
    PersistedCrimeWatchState as CharacterCrimeWatchState,
};
use eve_destiny::{Ball, MovementMode};
use glam::DVec3;
use std::collections::HashMap;

/// Computes character security status loss from criminal actions (`feat:crimewatch.sec-status-loss`).
pub fn compute_sec_status_loss(current_sec: f64, is_pod: bool, sys_sec: f64) -> f64 {
    if current_sec <= -10.0 {
        return 0.0;
    }
    let base_penalty = if is_pod { 2.0 } else { 0.5 };
    let sec_scale = (sys_sec.max(0.0) + 1.0) * 0.5;
    let distance_to_min = (current_sec - (-10.0)).max(0.0);
    (base_penalty + distance_to_min * 0.02 * sec_scale).max(base_penalty)
}

/// Aggression manager tracking PvP engagements between pilots (`feat:combat.aggression-tracking`).
#[derive(Debug, Default, Clone)]
pub struct AggressionManager {
    pub records: HashMap<(i64, i64), AggressionRecord>,
}

impl AggressionManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record_aggression(
        &mut self,
        aggressor: i64,
        victim: i64,
        system_id: u32,
        is_legal: bool,
        is_pod: bool,
        now_unix: u64,
    ) -> AggressionRecord {
        let record = AggressionRecord {
            aggressor_char_id: aggressor,
            victim_char_id: victim,
            system_id,
            started_unix: now_unix,
            expires_unix: now_unix.saturating_add(900),
            is_legal,
            is_pod,
        };
        self.records.insert((aggressor, victim), record.clone());
        record
    }

    pub fn get_aggression(&self, aggressor: i64, victim: i64, now_unix: u64) -> Option<&AggressionRecord> {
        self.records.get(&(aggressor, victim)).filter(|r| r.expires_unix > now_unix)
    }

    pub fn is_aggressor_against(&self, aggressor: i64, victim: i64, now_unix: u64) -> bool {
        self.get_aggression(aggressor, victim, now_unix).is_some()
    }

    pub fn list_active_aggressors_for_victim(&self, victim: i64, now_unix: u64) -> Vec<i64> {
        self.records
            .values()
            .filter(|r| r.victim_char_id == victim && r.expires_unix > now_unix)
            .map(|r| r.aggressor_char_id)
            .collect()
    }
}

/// System-wide CrimeWatch director managing timers, legality evaluations, aggression tracking, and CONCORD spawns.
#[derive(Debug, Default)]
pub struct CrimeWatchDirector {
    pub states: HashMap<i64, CharacterCrimeWatchState>,
    pub aggression_mgr: AggressionManager,
}

/// Durable read/write lock wrapper around [`CrimeWatchDirector`] backed directly by [`eve_db::WorldRepository`] (`D-6`).
pub struct CrimeWatchLock {
    lock: std::sync::RwLock<()>,
    db: std::sync::Arc<eve_db::WorldRepository>,
}

pub struct CrimeWatchReadGuard<'a> {
    _guard: std::sync::RwLockReadGuard<'a, ()>,
    director: CrimeWatchDirector,
}

impl<'a> std::ops::Deref for CrimeWatchReadGuard<'a> {
    type Target = CrimeWatchDirector;
    fn deref(&self) -> &Self::Target {
        &self.director
    }
}

impl CrimeWatchLock {
    pub fn new(db: std::sync::Arc<eve_db::WorldRepository>) -> Self {
        Self {
            lock: std::sync::RwLock::new(()),
            db,
        }
    }

    pub fn with_crimewatch<R>(&self, f: impl FnOnce(&CrimeWatchDirector) -> R) -> R {
        let guard = self.read().unwrap_or_else(|e| e.into_inner());
        f(&guard)
    }

    pub fn with_mut_crimewatch<R>(&self, f: impl FnOnce(&mut CrimeWatchDirector) -> R) -> R {
        let _guard = self.lock.write().unwrap_or_else(|e| e.into_inner());
        self.db.with_mut_snapshot(|snap| {
            let mut dir = CrimeWatchDirector {
                states: std::mem::take(&mut snap.crimewatch_states),
                aggression_mgr: AggressionManager {
                    records: std::mem::take(&mut snap.crimewatch_aggressions),
                },
            };
            let out = f(&mut dir);
            snap.crimewatch_states = dir.states;
            snap.crimewatch_aggressions = dir.aggression_mgr.records;
            out
        })
    }

    pub fn try_with_mut_crimewatch<R, E: eve_db::FromWalError>(
        &self,
        f: impl FnOnce(&mut CrimeWatchDirector) -> Result<R, E>,
    ) -> Result<R, E> {
        let _guard = self.lock.write().unwrap_or_else(|e| e.into_inner());
        self.db.try_with_mut_snapshot(|snap| {
            let mut dir = CrimeWatchDirector {
                states: std::mem::take(&mut snap.crimewatch_states),
                aggression_mgr: AggressionManager {
                    records: std::mem::take(&mut snap.crimewatch_aggressions),
                },
            };
            let res = f(&mut dir);
            snap.crimewatch_states = dir.states;
            snap.crimewatch_aggressions = dir.aggression_mgr.records;
            res
        })
    }

    pub fn read(
        &self,
    ) -> Result<CrimeWatchReadGuard<'_>, std::sync::PoisonError<CrimeWatchReadGuard<'_>>> {
        let guard = self.lock.read().unwrap_or_else(|e| e.into_inner());
        let (states, records) = self.db.with_snapshot(|s| {
            (
                s.crimewatch_states.clone(),
                s.crimewatch_aggressions.clone(),
            )
        });
        Ok(CrimeWatchReadGuard {
            _guard: guard,
            director: CrimeWatchDirector {
                states,
                aggression_mgr: AggressionManager { records },
            },
        })
    }

    pub fn write(
        &self,
    ) -> Result<CrimeWatchWriteGuard<'_>, std::sync::PoisonError<CrimeWatchWriteGuard<'_>>> {
        let guard = self.lock.write().unwrap_or_else(|e| e.into_inner());
        let (states, records) = self.db.with_snapshot(|s| {
            (
                s.crimewatch_states.clone(),
                s.crimewatch_aggressions.clone(),
            )
        });
        Ok(CrimeWatchWriteGuard {
            _guard: guard,
            db: &self.db,
            orig_states: states.clone(),
            orig_records: records.clone(),
            director: CrimeWatchDirector {
                states,
                aggression_mgr: AggressionManager { records },
            },
        })
    }
}

pub struct CrimeWatchWriteGuard<'a> {
    _guard: std::sync::RwLockWriteGuard<'a, ()>,
    db: &'a std::sync::Arc<eve_db::WorldRepository>,
    orig_states: HashMap<i64, CharacterCrimeWatchState>,
    orig_records: HashMap<(i64, i64), AggressionRecord>,
    director: CrimeWatchDirector,
}

impl<'a> std::ops::Deref for CrimeWatchWriteGuard<'a> {
    type Target = CrimeWatchDirector;
    fn deref(&self) -> &Self::Target {
        &self.director
    }
}

impl<'a> std::ops::DerefMut for CrimeWatchWriteGuard<'a> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.director
    }
}

impl<'a> Drop for CrimeWatchWriteGuard<'a> {
    fn drop(&mut self) {
        if self.director.states != self.orig_states
            || self.director.aggression_mgr.records != self.orig_records
        {
            let states = std::mem::take(&mut self.director.states);
            let aggs = std::mem::take(&mut self.director.aggression_mgr.records);
            self.db.with_mut_snapshot(|snap| {
                snap.crimewatch_states = states;
                snap.crimewatch_aggressions = aggs;
            });
        }
    }
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

    /// Triggers a 15-minute (900s) Suspect flag on a character.
    pub fn trigger_suspect_flag(&mut self, character_id: i64, now_unix: u64) {
        let st = self.get_or_create(character_id);
        st.suspect_flag_until_unix = now_unix.saturating_add(900);
    }

    /// Checks if a character currently has an active Suspect flag.
    pub fn is_suspect(&self, character_id: i64, now_unix: u64) -> bool {
        self.states
            .get(&character_id)
            .map(|s| s.is_suspect(now_unix))
            .unwrap_or(false)
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
        self.aggression_mgr.record_aggression(
            character_id,
            victim_char_id,
            sys_id,
            is_legal,
            false,
            now_unix,
        );

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
                    let loss = compute_sec_status_loss(crim.security_status, false, 1.0);
                    crim.security_status = (crim.security_status - loss).max(-10.0);
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
