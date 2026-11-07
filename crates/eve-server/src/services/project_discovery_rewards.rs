//! V24 Project Discovery analyst rank XP curve, bonus sample allowance,
//! reward crate spawning, and classification reward execution helpers.
#![forbid(unsafe_code)]

use super::ops::grade_and_record_classification;
use crate::services::project_discovery::{
    load_discovery_store, save_discovery_store, PlayerProjectState, ProjectDiscoveryStore,
    ACTIVE_PROJECT_ID, ISK_MULTIPLIERS, LOOT_CRATE_TYPE_ID, MIN_SUBMISSION_INTERVAL_SECS,
    NEEDED_LEVEL_FOR_SUPERIOR_CRATES, SUPERIOR_LOOT_CRATE_TYPE_ID,
};
use eve_db::{InventoryFlag, InventoryItemRecord, WorldStateSnapshot};
use eve_proto::{BridgeRpcError, PyRep};

/// Default tutorial completion XP reward shared identically across `get_tutorial_xp_reward`
/// and `give_tutorial_rewards`.
pub const DEFAULT_TUTORIAL_XP_REWARD: f64 = 500.0;

/// Daily bonus sample allowance for Alpha vs Omega clones when no store override is active.
pub const ALPHA_BONUS_SAMPLES: i64 = 10;
pub const OMEGA_BONUS_SAMPLES: i64 = 20;

/// Effective maximum daily bonus samples for a character (Omega doubles Alpha allowance unless overridden).
pub fn effective_max_bonus_samples(store: &ProjectDiscoveryStore, clone_grade: u32) -> i64 {
    if let Some(m) = store.max_bonus_samples.filter(|&v| v > 0) {
        return m;
    }
    if clone_grade >= 2 {
        OMEGA_BONUS_SAMPLES
    } else {
        ALPHA_BONUS_SAMPLES
    }
}

/// Sync bonus samples respecting clone grade (Alpha vs Omega) and 24-hour daily refill.
pub fn sync_player_bonus_samples(
    st: &mut PlayerProjectState,
    store: &ProjectDiscoveryStore,
    clone_grade: u32,
    now_unix: u64,
) {
    let target_max = effective_max_bonus_samples(store, clone_grade);
    if st.max_bonus_samples != target_max {
        let diff = target_max.saturating_sub(st.max_bonus_samples);
        st.max_bonus_samples = target_max;
        st.bonus_samples_left = (st.bonus_samples_left + diff).clamp(0, target_max);
    }
    if st.last_bonus_refill_unix == 0 {
        st.last_bonus_refill_unix = now_unix;
        st.bonus_samples_left = st.max_bonus_samples;
    } else if now_unix >= st.last_bonus_refill_unix.saturating_add(86_400) {
        st.bonus_samples_left = st.max_bonus_samples;
        st.last_bonus_refill_unix = now_unix;
    }
}

/// Cumulative XP required to reach `rank + 1` in V24 Project Discovery.
///
/// Honors explicit MMOS curve overrides when configured; otherwise evaluates the progressive
/// analyst rank curve through Rank 25 (`NEEDED_LEVEL_FOR_SUPERIOR_CRATES`) and higher mastery tiers.
pub fn total_needed_xp_for_rank(store: &ProjectDiscoveryStore, rank: i64) -> f64 {
    if rank <= 0 {
        return 0.0;
    }
    if let Some(&override_xp) = store.xp_curve_by_rank.get(&rank) {
        return override_xp.max(0.0);
    }
    if store.xp_per_rank > 0.0 {
        return (rank as f64) * store.xp_per_rank;
    }
    // Progressive analyst rank thresholds: Rank 1 -> 500, Rank 2 -> 1,250, Rank 3 -> 2,250, ...
    let mut cumulative = 0.0;
    for r in 1..=rank {
        let tier_step = if r <= 5 {
            250.0 + 250.0 * (r as f64)
        } else if r <= 15 {
            1_500.0 + 350.0 * ((r - 5) as f64)
        } else if r <= NEEDED_LEVEL_FOR_SUPERIOR_CRATES {
            5_000.0 + 500.0 * ((r - 15) as f64)
        } else {
            10_000.0 + 750.0 * ((r - NEEDED_LEVEL_FOR_SUPERIOR_CRATES) as f64)
        };
        cumulative += tier_step;
    }
    cumulative
}

/// Compute analyst rank for `experience` using `total_needed_xp_for_rank`.
pub fn compute_rank_for_xp(store: &ProjectDiscoveryStore, experience: f64, current_rank: i64) -> i64 {
    let mut rank = current_rank.max(1);
    for _ in 0..10_000 {
        let needed = total_needed_xp_for_rank(store, rank);
        if needed > 0.0 && experience >= needed {
            rank += 1;
        } else {
            break;
        }
    }
    rank
}

/// Spawn rank-up reward crate (`LOOT_CRATE_TYPE_ID = 55358` or `SUPERIOR_LOOT_CRATE_TYPE_ID = 55359`)
/// into the character's current or home station hangar on rank advancement.
pub fn spawn_rank_reward_crate(
    snap: &mut WorldStateSnapshot,
    char_id: i64,
    crate_type_id: u32,
) {
    let station_id = snap
        .characters
        .get(&char_id)
        .and_then(|c| c.station_id.or(Some(c.home_station_id)))
        .filter(|&id| id > 0);
    let Some(loc_id) = station_id else {
        return;
    };
    snap.next_entity_id += 1;
    let item_id = snap.next_entity_id;
    snap.items.insert(
        item_id,
        InventoryItemRecord {
            item_id,
            type_id: crate_type_id,
            owner_id: char_id,
            location_id: loc_id,
            flag: InventoryFlag::Hangar,
            quantity: 1,
            singleton: false,
            version: 1,
            ..Default::default()
        },
    );
}

/// Format `get_player_statistics` reply expected by `rewardview.pyj` and `subcellularatlas.pyj`.
pub fn format_player_statistics_rep(st: &PlayerProjectState) -> PyRep {
    let proj = PyRep::Dict(vec![
        (PyRep::String("score".into()), PyRep::Real(st.score)),
        (PyRep::String("accuracy".into()), PyRep::Real(st.accuracy)),
        (PyRep::String("rank".into()), PyRep::Integer(st.rank)),
        (PyRep::String("experience".into()), PyRep::Real(st.experience)),
        (
            PyRep::String("tasks_submitted".into()),
            PyRep::Integer(st.tasks_submitted),
        ),
    ]);
    let projects_map = PyRep::Dict(vec![
        (PyRep::String(st.project_id.clone()), proj.clone()),
        (PyRep::String(ACTIVE_PROJECT_ID.into()), proj.clone()),
    ]);
    PyRep::keyval([
        ("rank", PyRep::Integer(st.rank)),
        ("experience", PyRep::Real(st.experience)),
        ("score", PyRep::Real(st.score)),
        ("accuracy", PyRep::Real(st.accuracy)),
        ("tasks_submitted", PyRep::Integer(st.tasks_submitted)),
        ("analysisKredits", PyRep::Integer(st.analysis_kredits)),
        ("project", proj.clone()),
        ("projects", projects_map),
        ("project_list", PyRep::List(vec![proj])),
    ])
}

/// Process a citizen-science classification submission end-to-end inside a snapshot transaction.
#[allow(clippy::type_complexity)]
pub fn execute_post_classification(
    snap: &mut WorldStateSnapshot,
    st: &mut PlayerProjectState,
    char_id: i64,
    req_task_index: i64,
    classification_arg: &PyRep,
    now_unix: u64,
) -> Result<
    (
        PyRep,
        PyRep,
        Option<eve_db::IskCents>,
        f64,
        f64,
        i64,
        i64,
        f64,
        f64,
        i64,
    ),
    BridgeRpcError,
> {
    if st.last_submission_unix > 0
        && (now_unix <= st.last_submission_unix
            || now_unix.saturating_sub(st.last_submission_unix) < MIN_SUBMISSION_INTERVAL_SECS)
    {
        return Err(BridgeRpcError::user_error("ProjectDiscoverySubmissionTooFast"));
    }
    if req_task_index != st.current_task_index {
        return Err(BridgeRpcError {
            code: "InvalidTaskId".into(),
            message: format!(
                "task_id {} does not match current active task {}",
                req_task_index, st.current_task_index
            ),
        });
    }
    if st.last_submitted_task_index >= st.current_task_index {
        return Err(BridgeRpcError {
            code: "TaskAlreadySubmitted".into(),
            message: "Active task has already been submitted".into(),
        });
    }

    let clone_grade = snap
        .characters
        .get(&char_id)
        .map(|c| c.clone_grade)
        .unwrap_or(1);
    let mut store = load_discovery_store(snap);
    sync_player_bonus_samples(st, &store, clone_grade, now_unix);

    let prev_tasks = st.tasks_submitted;
    st.last_submitted_task_index = st.current_task_index;
    st.last_submission_unix = now_unix;
    st.tasks_submitted += 1;

    let (sample_acc, is_accurate) =
        grade_and_record_classification(snap, &mut store, char_id, req_task_index, classification_arg);

    let base_xp_acc = if store.classification_xp_accurate > 0.0 {
        store.classification_xp_accurate
    } else {
        150.0
    };
    let base_xp_inacc = if store.mmos_configured {
        store.classification_xp_inaccurate.max(0.0)
    } else {
        25.0
    };
    let base_ak = if store.classification_ak_reward > 0 {
        store.classification_ak_reward
    } else {
        20
    };
    save_discovery_store(snap, &store);

    st.accuracy = if prev_tasks <= 0 {
        sample_acc
    } else {
        (((st.accuracy * 9.0) + sample_acc) / 10.0).clamp(0.0, 1.0)
    };
    st.score = (st.accuracy * 100.0).clamp(0.0, 1000.0);

    let had_bonus = st.bonus_samples_left > 0;
    st.bonus_samples_left = st.bonus_samples_left.saturating_sub(1);
    let bonus_mult = if had_bonus { 2.0 } else { 1.0 };
    let xp_gain = (if is_accurate { base_xp_acc } else { base_xp_inacc }) * bonus_mult;
    let ak_gain = if is_accurate { base_ak } else { 0 };

    let prev_rank = st.rank;
    st.experience += xp_gain;
    st.analysis_kredits = st.analysis_kredits.saturating_add(ak_gain);

    // V24 `projectdiscovery/common/projects/covid/iskmultipliers.pyj`: `ISK_MULTIPLIERS = [500, 800, 1000]`
    let tier_idx = if st.accuracy >= 0.9 {
        2
    } else if st.accuracy >= 0.5 {
        1
    } else {
        0
    };
    let isk_amount = if is_accurate {
        let base_isk = ISK_MULTIPLIERS[tier_idx] * st.rank.max(1);
        if had_bonus {
            base_isk * 2
        } else {
            base_isk
        }
    } else {
        0
    };
    let (_reward, new_bal) = if isk_amount > 0 {
        let r = eve_db::IskCents::from_isk(isk_amount);
        let bal = if let Some(c) = snap.characters.get_mut(&char_id) {
            c.balance = c.balance.checked_add(r).unwrap_or(c.balance);
            c.balance
        } else {
            r
        };
        snap.next_entity_id += 1;
        let jid = snap.next_entity_id;
        snap.wallet_journal.push(eve_db::WalletJournalEntry {
            entry_id: jid,
            character_id: char_id,
            entry_type: "project_discovery_reward".to_string(),
            amount: r,
            balance_after: bal,
            reason: "Project Discovery classification reward".to_string(),
        });
        snap.wallet_journal_timestamps.insert(jid, now_unix);
        (r, Some(bal))
    } else {
        (eve_db::IskCents(0), None)
    };

    let new_rank = compute_rank_for_xp(&store, st.experience, st.rank);
    let mut crates_granted = Vec::new();
    for reached_rank in (prev_rank + 1)..=new_rank {
        let crate_type_id = if reached_rank >= NEEDED_LEVEL_FOR_SUPERIOR_CRATES {
            SUPERIOR_LOOT_CRATE_TYPE_ID
        } else {
            LOOT_CRATE_TYPE_ID
        };
        spawn_rank_reward_crate(snap, char_id, crate_type_id);
        crates_granted.push(PyRep::Integer(crate_type_id as i64));
    }
    st.rank = new_rank;

    let xp_for_new_rank = total_needed_xp_for_rank(&store, st.rank);
    let xp_for_next_rank = total_needed_xp_for_rank(&store, st.rank + 1);
    let player_state_rep = PyRep::keyval([
        ("rank", PyRep::Integer(st.rank)),
        ("experience", PyRep::Real(st.experience)),
        ("totalXP", PyRep::Real(st.experience)),
        ("accuracy", PyRep::Real(st.accuracy)),
        ("score", PyRep::Real(st.score)),
        ("analysisKredits", PyRep::Integer(st.analysis_kredits)),
        (
            "finishedTutorial",
            PyRep::Bool(st.tutorial_complete || st.tutorial_skipped),
        ),
    ]);
    let rep = PyRep::keyval([
        ("score", PyRep::Real(st.score)),
        ("accuracy", PyRep::Real(st.accuracy)),
        ("experience_reward", PyRep::Real(xp_gain)),
        ("kredits_reward", PyRep::Integer(ak_gain)),
        ("XP_Reward", PyRep::Real(xp_gain)),
        ("ISK_Reward", PyRep::Real(isk_amount as f64)),
        ("AK_Reward", PyRep::Integer(ak_gain)),
        ("gotBonusXP", PyRep::Bool(had_bonus)),
        (
            "bonusSamplesAfterClassification",
            PyRep::Integer(st.bonus_samples_left),
        ),
        ("loot_crates", PyRep::List(crates_granted)),
        ("playerState", player_state_rep.clone()),
        ("player", PyRep::keyval([("score", PyRep::Real(st.accuracy))])),
        ("rank", PyRep::Integer(st.rank)),
    ]);
    Ok((
        rep,
        player_state_rep,
        new_bal,
        xp_gain,
        st.experience,
        prev_rank,
        st.rank,
        xp_for_new_rank,
        xp_for_next_rank,
        ak_gain,
    ))
}
