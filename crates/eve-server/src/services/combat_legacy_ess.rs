//! `combat_legacy_ess` — Legacy weapon fire RPC compatibility & Null-Sec ESS Main/Reserve Bank linking.

#![forbid(unsafe_code)]

use super::*;

/// Returns `true` when `group_id` is a canonical SDE Category 11 non-combatant prop, structure,
/// container, billboard, cloud, capture/control point, or Homefront noncombatant rather than an
/// attackable combat NPC.
pub(crate) fn is_non_combat_npc_prop_group(group_id: u32) -> bool {
    matches!(
        group_id,
        306      // Spawn Container
            | 319  // Large Collidable Structure
            | 323  // Billboard
            | 335  // Temporary Cloud
            | 494  // Deadspace Overseer's Structure
            | 496  // Deadspace Overseer's Belongings
            | 784  // Large Collidable Ship
            | 922  // Capture Point
            | 925  // FW Infrastructure Hub
            | 952  // Mission Container
            | 1207 // Scatter Container
            | 1872 // Structure Entities
            | 1927 // Irregular Structure
            | 1928 // Irregular Container
            | 4073 // Temporary Collidable Structure
            | 4494 // Control Point
            | 4576 // Homefront Operations Structure
            | 4771 // Homefront Operations Noncombatant
    )
}

/// Returns `true` when `group_id` identifies invulnerable law-enforcement/authority NPCs
/// (CONCORD, Police Drone, Customs Official, ambient/protective Sentry Guns, Homefront Allied
/// Dreadnoughts) rather than attackable mission/combat NPCs. Note: `693..=698` (`Mission CONCORD *`)
/// are attackable mission enemies and are intentionally excluded.
pub(crate) fn is_invulnerable_npc_authority_group(group_id: u32) -> bool {
    matches!(
        group_id,
        99       // Sentry Gun
            | 180  // Protective Sentry Gun
            | 182  // Police Drone (CONCORD SWAT / surveillance police)
            | 300  // Legacy/synthetic CONCORD fixture group ID
            | 301  // Concord Drone (CONCORD Police / DED Special Operation)
            | 446  // Customs Official
            | 4572 // Homefront Operations Allied Dreadnought (all-zero damage resonances)
    )
}

/// Returns `true` when `group_id` is a canonical SDE Sleeper, Autothysian Lancer, Seeker,
/// Drifter, or Hidden Zenith Capital entity group.
pub(crate) fn is_sleeper_or_drifter_npc_group(group_id: u32) -> bool {
    matches!(
        group_id,
        959..=961       // Deadspace Sleeper Sentinel
            | 982..=1005 // Deadspace Sleeper Defender / Patroller
            | 1307       // Roaming Sleepers Cruiser (Autothysian Lancer)
            | 1310       // Drifter Battleship
            | 1529       // Deadspace Sleeper Upgraded Avenger
            | 1530       // Drifter Response Battleship
            | 1768       // Seeker Scouts (Circadian Seeker)
            | 1788       // Hidden Zenith Drifters
            | 1804..=1807 // Hidden Zenith Empire Capitals
            | 1956       // Drifter Reinforcements
            | 4035       // Drifter Entities
            | 4036       // Sleeper Entities
    )
}

/// Updates a null-sec solar system's Dynamic Bounty System (`dbs_multiplier`) based on ratting
/// volume (decays toward 0.50 floor) and capsuleer PvP ship losses (recovers toward 2.00 ceiling).
/// (`feat:pve.dynamic-bounty`)
pub fn step_dynamic_bounty_multiplier(
    ctx: &ServerContext,
    solar_system_id: u32,
    ratting_isk_destroyed: f64,
    pvp_isk_destroyed: f64,
) -> f64 {
    let (old_rate, rate) = ctx.db.with_mut_snapshot(|snap| {
        let st = snap
            .system_states
            .entry(solar_system_id)
            .or_insert_with(|| SystemDynamicState {
                solar_system_id,
                dbs_multiplier: 1.35,
                ess_main_bank: IskCents(0),
                ess_reserve_bank: IskCents(0),
                ess_linked_character_id: None,
                ess_link_started_tick: None,
                ess_cycle_started_unix: 0,
                ess_main_bank_contributors: Default::default(),
                sov_alliance_id: None,
                military_index: 1,
                industrial_index: 1,
                strategic_index: 1,
                fw_contested_pct: 0.0,
                insurgency_corruption_stage: 0,
                insurgency_suppression_stage: 0,
                wormhole_remaining_mass_kg: 0.0,
                wormhole_destination_system_id: None,
                signature_position: None,
                fw_complex_position: None,
                fw_capture_seconds_elapsed: 0,
                fw_capture_duration_seconds: 0,
                last_fw_capture_tick: 0,
                last_sov_claim_tick: 0,
            });
        let prev = st.dbs_multiplier;
        let rat_decay = (ratting_isk_destroyed.max(0.0) / 50_000_000.0) * 0.10;
        let pvp_boost = (pvp_isk_destroyed.max(0.0) / 25_000_000.0) * 0.15;
        let eq_drift = if ratting_isk_destroyed == 0.0 && pvp_isk_destroyed == 0.0 {
            (1.35 - st.dbs_multiplier) * 0.05
        } else {
            0.0
        };
        st.dbs_multiplier = (st.dbs_multiplier - rat_decay + pvp_boost + eq_drift).clamp(0.50, 2.00);
        (prev, st.dbs_multiplier)
    });
    if (rate - old_rate).abs() > 1e-9 {
        crate::services::notify_emitters_combat_ess_progression::emit_system_dynamic_bounty_state_changed(
            ctx,
            solar_system_id as i64,
            rate,
        );
    }
    rate
}

pub async fn handle_legacy_and_ess(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        // Dogma Phase 4 Exit Gate: dogmaIM.FireWeapon and beyonce.CmdAttackTarget are deleted in favor of dogmaIM.Activate
        // rpc-lint: allow Phase 4 exit gate explicitly rejects deleted dogmaIM.FireWeapon
        ("dogmaIM", "FireWeapon") => {
            Some(Err(BridgeRpcError {
                code: "UnimplementedRPC".into(),
                message: "dogmaIM.FireWeapon is deleted; use dogmaIM.Activate".into(),
                client_error: None,
            }))
        }
        // rpc-lint: allow Phase 4 exit gate explicitly rejects deleted beyonce.CmdAttackTarget
        ("beyonce", "CmdAttackTarget") => {
            Some(Err(BridgeRpcError {
                code: "UnimplementedRPC".into(),
                message: "beyonce.CmdAttackTarget is deleted; use dogmaIM.Activate".into(),
                client_error: None,
            }))
        }

        ("essMgr", "AttemptLinkToMainBank") | ("essMgr", "AttemptLinkToReserveBank") => {
            use crate::services::encounter_surveillance_system::handle_ess_link;
            Some(handle_ess_link(ctx, session, method, args).await)
        }

        _ => None,
    }
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
    Box::pin(handle_legacy_and_ess(ctx, session, service, method, args, kwargs))
}

pub(crate) const RPCS: &[RpcEntry] = &[
    rpc_entry("dogmaIM", "FireWeapon", RpcDispatchHandler::Async(dispatch_rpc)), // rpc-lint: allow Phase 4 exit gate explicitly rejects deleted dogmaIM.FireWeapon
    rpc_entry("beyonce", "CmdAttackTarget", RpcDispatchHandler::Async(dispatch_rpc)), // rpc-lint: allow Phase 4 exit gate explicitly rejects deleted beyonce.CmdAttackTarget
    rpc_entry("essMgr", "AttemptLinkToMainBank", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("essMgr", "AttemptLinkToReserveBank", RpcDispatchHandler::Async(dispatch_rpc)),
];

#[cfg(test)]
#[path = "combat_legacy_ess_tests.rs"] mod combat_legacy_ess_tests;
