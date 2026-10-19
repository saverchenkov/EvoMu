//! `combat` — Core Phase 5 combat, damage formulas, death pipeline, NPC spawns & AI, drones/salvage/tractor, and CrimeWatch 2.0 (`C-1..C-4, C-7, C-8`).

#![forbid(unsafe_code)]

pub mod crimewatch;
pub mod death_pipeline;
pub mod drones_salvage_tractor;
pub mod npc_ai;

pub use crimewatch::{
    AggressionEvaluation, CharacterCrimeWatchState, ConcordExecutionReport, CrimeWatchDirector,
};
pub use death_pipeline::{execute_death_pipeline, DeathPipelineResult};
pub use drones_salvage_tractor::{
    attempt_salvage_wreck, step_tractor_beam, validate_drone_bandwidth, ActiveDrone,
    DroneController, SalvageOutcome,
};
pub use npc_ai::{NpcAiState, NpcCombatEntity, NpcSpawnManager};
