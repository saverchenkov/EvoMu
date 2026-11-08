//! Protocol-agnostic Career Goals, Milestones & Reward Crediting domain services (`#593`).

#![forbid(unsafe_code)]

use crate::domain::DomainError;
use crate::services::quasar_career_goals::{CareerGoalManager, CareerMilestoneDef};
use eve_db::{IskCents, NpeProgressRecord, WorldStateSnapshot};
use eve_sde::SdeRegistry;

/// Credits ISK, free skill points, and optional EverMarks + wallet journal atomically within `snap`.
pub fn credit_character_reward_in_snap(
    snap: &mut WorldStateSnapshot,
    char_id: i64,
    isk_cents: IskCents,
    sp_reward: f64,
    evermarks: i64,
    journal_meta: Option<(&'static str, String, Option<(i32, i64, u64)>)>,
) -> Result<(IskCents, f64), DomainError> {
    let (new_bal, new_sp, char_corp) = {
        let ch = snap.characters.get_mut(&char_id).ok_or_else(|| {
            DomainError::not_found(
                "CharacterNotFound",
                format!("Character {char_id} not found"),
            )
        })?;
        let next_bal = ch.balance.checked_add(isk_cents).ok_or_else(|| {
            DomainError::invalid_input("BalanceOverflow", "Balance overflow")
        })?;
        ch.balance = next_bal;
        if sp_reward > 0.0 {
            ch.free_skill_points += sp_reward;
        }
        if evermarks > 0 {
            let em_cur = ch.loyalty_points.entry(1_000_419).or_insert(0);
            *em_cur = em_cur.saturating_add(evermarks);
        }
        (next_bal, ch.free_skill_points, ch.corporation_id)
    };
    if evermarks > 0 && char_corp > 0 {
        let corp_em = snap.corp_lp_balances.entry((char_corp, 1_000_419)).or_insert(0);
        *corp_em = corp_em.saturating_add(evermarks);
    }
    if let Some((entry_type, reason, ledger_extra)) = journal_meta {
        if let Some((ref_type_id, first_party_id, date_unix)) = ledger_extra {
            snap.append_journal_entry(eve_db::ledger::JournalEntryInput {
                owner_id: char_id,
                entry_type,
                amount: isk_cents,
                balance_after: new_bal,
                reason,
                meta: eve_db::ledger::JournalMeta {
                    ref_type_id,
                    first_party_id,
                    second_party_id: char_id,
                    account_key: eve_db::ledger::ACCOUNTING_KEY_CASH,
                    date_unix,
                    ..Default::default()
                },
            });
        } else {
            snap.append_journal(char_id, entry_type, isk_cents, new_bal, reason);
        }
    }
    Ok((new_bal, new_sp))
}

#[derive(Clone, Debug)]
pub struct ResolvedCareerGoalTarget {
    pub required_progress: u64,
    pub sp_reward: f64,
    pub career_points_progress: Option<u64>,
}

/// Resolves progress requirements and SP reward for a career goal or milestone ID across both
/// Quasar AIR Career Program milestones and SDE `career_goals`.
pub fn resolve_career_goal_or_milestone(
    sde: &SdeRegistry,
    career_goals_mgr: &CareerGoalManager,
    milestones: &[CareerMilestoneDef],
    char_id: i64,
    goal_id: u32,
) -> Option<ResolvedCareerGoalTarget> {
    if let Some(m) = milestones.iter().find(|m| m.milestone_id == goal_id) {
        let cp = u64::from(
            career_goals_mgr.career_points_for_character(char_id as u32, m.career_id),
        );
        return Some(ResolvedCareerGoalTarget {
            required_progress: u64::from(m.required_points).max(1),
            sp_reward: m.sp_reward,
            career_points_progress: Some(cp),
        });
    }
    sde.career_goals
        .get(&goal_id)
        .map(|g| ResolvedCareerGoalTarget {
            required_progress: g.target_progress.max(1),
            sp_reward: g.sp_reward as f64,
            career_points_progress: None,
        })
}

/// Atomically verifies completion and claim idempotency across `npe_progress.claimed_goals` and
/// `quasar_career_milestones`, credits `sp_reward`, and records claim state in `snap`.
pub fn claim_career_goal_or_milestone_in_snap(
    snap: &mut WorldStateSnapshot,
    career_goals_mgr: &CareerGoalManager,
    char_id: i64,
    goal_id: u32,
    current_progress: u64,
    required_progress: u64,
    sp_reward: f64,
) -> Result<f64, DomainError> {
    if career_goals_mgr.is_milestone_claimed(char_id as u32, goal_id, snap) {
        return Err(DomainError::forbidden(
            "GoalAlreadyClaimed",
            format!("Career milestone {goal_id} reward already claimed"),
        ));
    }
    if current_progress < required_progress {
        return Err(DomainError::forbidden(
            "GoalProgressIncomplete",
            format!(
                "Career goal {goal_id} requires progress {required_progress}, current is {current_progress}"
            ),
        ));
    }
    if !snap.characters.contains_key(&char_id) {
        return Err(DomainError::not_found(
            "CharacterNotFound",
            format!("Character {char_id} not found"),
        ));
    }
    let npe = snap.npe_progress.entry(char_id).or_insert_with(|| NpeProgressRecord {
        character_id: char_id,
        node_graph_id: 473,
        current_node_id: 1,
        completed: false,
        career_goal_progress: Default::default(),
        claimed_goals: Default::default(),
    });
    if !npe.claimed_goals.insert(goal_id) {
        return Err(DomainError::forbidden(
            "GoalAlreadyClaimed",
            format!("Career goal {goal_id} has already been claimed"),
        ));
    }
    let (_, new_sp) = credit_character_reward_in_snap(snap, char_id, IskCents(0), sp_reward, 0, None)?;
    career_goals_mgr.mark_milestone_claimed_in_snapshot(char_id as u32, goal_id, snap);
    Ok(new_sp)
}
