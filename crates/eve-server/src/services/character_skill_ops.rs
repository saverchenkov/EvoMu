//! `character_skill_ops` — Automated server-side obsolete skill migration (`feat:skills.skill-remove-obsolete`).

#![forbid(unsafe_code)]

use crate::router::{NotificationTarget, ServerContext};
use eve_db::SkillHistoryEntry;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;

/// Automated server-side obsolete skill migration (`feat:skills.skill-remove-obsolete`).
///
/// Detects deprecated or unpublished skill types in the character's trained skills,
/// removes them, and refunds 100% of trained skill points to `free_skill_points`.
/// Emits `OnServerSkillsRemoved` and `OnFreeSkillPointsChanged` notifications and logs history event 38.
pub fn migrate_obsolete_skills_for_character(
    ctx: &ServerContext,
    char_id: i64,
) -> (usize, f64) {
    let now_ft = ctx.now_blueos_filetime_ticks();

    let res: Result<_, BridgeRpcError> = ctx.db.try_with_mut_snapshot(|snap| {
        let Some(skills_map) = snap.skills.get_mut(&char_id) else {
            return Ok((Vec::new(), 0.0, 0.0));
        };

        // Dynamically scan for any trained skill that is obsolete in SDE (!published or missing from SDE)
        let obsolete_candidates: Vec<u32> = skills_map
            .keys()
            .copied()
            .filter(|&sk_id| {
                ctx.sde
                    .types
                    .get(&sk_id)
                    .map(|t| !t.published || t.category_id != 16)
                    .unwrap_or(true)
            })
            .collect();

        if obsolete_candidates.is_empty() {
            return Ok((Vec::new(), 0.0, 0.0));
        }

        let mut to_refund = Vec::new();
        for sk_id in obsolete_candidates {
            if let Some(rec) = skills_map.remove(&sk_id) {
                to_refund.push((sk_id, rec.skill_points));
            }
        }

        let total_sp: f64 = to_refund.iter().map(|(_, sp)| *sp).sum();
        let ch = snap.characters.get_mut(&char_id).ok_or_else(|| BridgeRpcError {
            code: "CharacterNotFound".into(),
            message: format!("Character {char_id} not found"),
        })?;

        ch.skill_points = (ch.skill_points - total_sp).max(0.0);
        ch.free_skill_points += total_sp;

        // Record skill removal history entry (event 38)
        let hist = snap.skill_history.entry(char_id).or_default();
        for &(sk_id, _) in &to_refund {
            hist.push(SkillHistoryEntry {
                log_date_ft: now_ft,
                event_type_id: 38,
                skill_type_id: sk_id,
                skill_level: 0,
                absolute_points: 0.0,
            });
        }

        Ok((to_refund, total_sp, ch.free_skill_points))
    });

    match res {
        Ok((to_refund, total_sp, new_free_sp)) if !to_refund.is_empty() => {
            let removed_pairs: Vec<(u32, u8)> = to_refund.iter().map(|&(sk, _)| (sk, 0u8)).collect();
            super::jump_clone_names::emit_server_skills_removed(ctx, char_id, &removed_pairs);
            ctx.notify(
                NotificationTarget::Character(char_id),
                "OnFreeSkillPointsChanged",
                "charid",
                vec![PyRep::Real(new_free_sp)],
            );
            (to_refund.len(), total_sp)
        }
        _ => (0, 0.0),
    }
}

/// Dispatches skill ops RPCs.
pub fn handle(
    _ctx: &ServerContext,
    _session: &mut SessionState,
    _service: &str,
    _method: &str,
    _args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    None
}
