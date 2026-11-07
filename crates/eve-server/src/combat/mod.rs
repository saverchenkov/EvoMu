//! `combat` — Core Phase 5 combat, damage formulas, death pipeline, NPC spawns & AI, drones/salvage/tractor, and CrimeWatch 2.0 (`C-1..C-4, C-7, C-8`).

#![forbid(unsafe_code)]

pub mod crimewatch;
pub mod death_pipeline;
pub mod drones_salvage_tractor;
pub mod npc_ai;

pub use crimewatch::{
    compute_sec_status_loss, AggressionEvaluation, AggressionManager, AggressionRecord,
    CharacterCrimeWatchState, ConcordExecutionReport, CrimeWatchDirector,
};
pub use death_pipeline::{
    execute_death_pipeline, execute_death_pipeline_sync, execute_self_destruct,
    DeathPipelineResult, SelfDestructManager,
};
pub use drones_salvage_tractor::{
    attempt_salvage_wreck, step_tractor_beam, validate_drone_bandwidth, ActiveDrone,
    DroneController, SalvageOutcome,
};
pub use npc_ai::{
    compute_sec_status_gain, faction_damage_vector, NpcAiState, NpcCombatEntity, NpcSpawnManager,
};

#[cfg(test)]
mod combat_mechanics_tests;
#[cfg(test)]
mod salvage_ai_death_tests;
#[cfg(test)]
mod npc_ai_and_self_destruct_tests;
#[cfg(test)]
mod combat_weapons_deep_extra_unit_tests;


