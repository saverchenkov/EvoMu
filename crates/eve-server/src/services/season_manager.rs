//! `season_manager` — Seasonal event and challenge tracking service (`seasonManager`).
//!
//! Tracks active seasons, character challenges, progress, rewards, and loot tables.
#![forbid(unsafe_code)]

use crate::router::notify_registry::{
    OnAccountChange, OnChallengeCompleted, OnChallengeExpired, OnChallengeProgressUpdate,
    OnChallengeRewardsGranted, OnItemChange, OnRedeemingQueueUpdated, OnRedeemingTokensUpdated,
    OnSeasonOmegaRewardsClaimed, OnSeasonSelected, OnSeasonalGoalCompleted,
    OnSeasonalPointsUpdated, OnSeasonsFsdDataChanged,
};
use crate::router::{NotificationTarget, ServerContext};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;

pub const SEASON_STATE_KEY_PREFIX: &str = "season_state:";

pub use eve_db::{CharacterChallenge, CharacterSeasonState};

pub fn load_season_state_from_blobs(snap: &eve_db::WorldStateSnapshot, char_id: i64) -> CharacterSeasonState {
    snap.seasons_and_campaigns
        .season_states
        .get(&char_id)
        .cloned()
        .unwrap_or_default()
}

pub fn save_season_state_to_blobs(snap: &mut eve_db::WorldStateSnapshot, char_id: i64, state: &CharacterSeasonState) {
    snap.seasons_and_campaigns
        .season_states
        .insert(char_id, state.clone());
}

pub use self::{
    load_season_state_from_blobs as load_season_state,
    save_season_state_to_blobs as save_season_state,
};

pub use super::season_fsd::*;

#[path = "season_manager_ops.rs"]
pub(crate) mod ops;

/// Challenge keyvals for `st`, sorted by challenge ID.
fn sorted_challenge_keyvals(st: &CharacterSeasonState) -> Vec<PyRep> {
    let mut list: Vec<_> = st.challenges.values().filter(|c| !c.claimed).collect();
    list.sort_by_key(|c| c.challenge_id);
    list.into_iter().map(|c| challenge_to_keyval(c, st.season_points)).collect()
}

/// Active challenge dictionary entries `(challenge_id, KeyVal)` sorted by `challenge_id` for V24 `SeasonService`.
fn sorted_challenge_dict(st: &CharacterSeasonState) -> Vec<(PyRep, PyRep)> {
    let mut list: Vec<_> = st.challenges.values().filter(|c| !c.claimed).collect();
    list.sort_by_key(|c| c.challenge_id);
    list.into_iter()
        .map(|c| (PyRep::Integer(c.challenge_id), challenge_to_keyval(c, st.season_points)))
        .collect()
}

fn emit_expired_challenges(
    ctx: &ServerContext,
    char_id: i64,
    season_points: i64,
    expired: &[(i64, CharacterChallenge)],
) {
    for (old_id, new_ch) in expired {
        ctx.emit_typed(
            NotificationTarget::Character(char_id),
            OnChallengeExpired {
                challenge_id: *old_id,
                new_challenge: challenge_to_keyval(new_ch, season_points),
            },
        );
    }
}

fn resolve_season_bounds(
    st: &CharacterSeasonState,
    snap: &eve_db::WorldStateSnapshot,
    sde: &eve_sde::SdeRegistry,
    now_ft: i64,
) -> (i64, usize) {
    ops::resolve_season_bounds_clean(st, snap, sde, now_ft)
}

fn read_character_season_state<F, R>(ctx: &ServerContext, char_id: i64, f: F) -> R
where
    F: FnOnce(&CharacterSeasonState, &eve_db::WorldStateSnapshot) -> R,
{
    ctx.db.with_snapshot(|snap| {
        let mut st = load_season_state_from_blobs(snap, char_id);
        let now_ft = ctx.now_blueos_filetime_ticks();
        let (season_end_ft, max_active) = resolve_season_bounds(&st, snap, &ctx.sde, now_ft);
        let defs = ops::load_localized_challenge_definitions(snap, &ctx.sde);
        let _ = ops::fill_and_normalize_character_challenges(&mut st, &defs, snap, char_id, now_ft, season_end_ft, max_active);
        sync_character_challenges_with_defs(&mut st, &defs, snap, char_id);
        f(&st, snap)
    })
}

pub fn with_season_state<F, R>(ctx: &ServerContext, char_id: i64, f: F) -> Result<R, BridgeRpcError>
where
    F: FnOnce(&mut CharacterSeasonState, &mut eve_db::WorldStateSnapshot) -> Result<R, BridgeRpcError>,
{
    ctx.db.try_with_mut_snapshot(|snap| {
        let mut state = load_season_state_from_blobs(snap, char_id);
        let out = f(&mut state, snap)?;
        save_season_state_to_blobs(snap, char_id, &state);
        Ok(out)
    })
}

pub fn challenge_to_keyval(c: &CharacterChallenge, season_points: i64) -> PyRep {
    PyRep::keyval([
        ("challengeID", PyRep::Integer(c.challenge_id)),
        ("seasonID", PyRep::Integer(c.season_id)),
        ("seasonPoints", PyRep::Integer(season_points)),
        ("title", PyRep::String(c.title.clone())),
        ("description", PyRep::String(c.description.clone())),
        ("points", PyRep::Integer(c.points)),
        ("progress", PyRep::Integer(c.progress)),
        ("maxProgress", PyRep::Integer(c.max_progress)),
        ("completed", PyRep::Bool(c.completed)),
        ("claimed", PyRep::Bool(c.claimed)),
        ("isDormant", PyRep::Bool(c.is_dormant)),
        ("expirationDate", PyRep::Integer(c.expiration_date)),
    ])
}


pub fn default_challenge(ch_id: i64, season_id: i64, now_ft: i64) -> CharacterChallenge {
    if let Some(def) = default_seasonal_challenge_definitions().into_iter().find(|d| d.challenge_id == ch_id) {
        def.to_character_challenge(season_id, now_ft)
    } else {
        let expiration_date = now_ft + 7 * 86_400 * 10_000_000;
        CharacterChallenge {
            challenge_id: ch_id,
            season_id,
            title: format!("Challenge {ch_id}"),
            description: String::new(),
            points: 10,
            progress: 0,
            max_progress: 1,
            completed: false,
            claimed: false,
            is_dormant: false,
            expiration_date,
            client_event_driven: true,
            reward_type_id: 0,
            reward_quantity: 0,
            baseline_progress: 0,
        }
    }
}

pub fn get_current_activity_count(
    event_type: &str,
    snap: &eve_db::WorldStateSnapshot,
    char_id: i64,
) -> i64 {
    match event_type {
        "totalMined" | "volumeMined" => snap
            .mining_ledger
            .iter()
            .filter(|e| e.character_id == char_id)
            .map(|e| e.quantity)
            .sum::<i64>(),
        "destroyHostileNPC" => snap
            .killmails
            .values()
            .filter(|k| {
                k.attacker_character_id == char_id
                    && k.victim_character_id != 0
                    && k.victim_character_id != char_id
                    && !snap.characters.contains_key(&k.victim_character_id)
                    && !snap.corporations.contains_key(&k.victim_character_id)
            })
            .count() as i64,
        "hackingMiniGameWon" => snap
            .seasons_and_campaigns
            .hacking_wins
            .get(&char_id)
            .copied()
            .unwrap_or(0) as i64,
        "industryJobsCompleted" | "typeManufacturedCount" => snap
            .industry_jobs
            .values()
            .filter(|j| {
                j.installer_id == char_id
                    && j.activity_id == 1
                    && (j.status.eq_ignore_ascii_case("delivered")
                        || j.status.eq_ignore_ascii_case("completed")
                        || j.status == "102")
            })
            .count() as i64,
        _ => 0,
    }
}

pub fn ensure_character_challenges_with_limit(
    st: &mut CharacterSeasonState,
    defs: &[SeasonalChallengeDef],
    snap: &eve_db::WorldStateSnapshot,
    char_id: i64,
    now_ft: i64,
    season_end_ft: i64,
    max_active: usize,
) -> Vec<(i64, CharacterChallenge)> {
    let max_active = max_active.max(1);
    let def_map: HashMap<i64, &SeasonalChallengeDef> = defs.iter().map(|d| (d.challenge_id, d)).collect();

    // Evict already-claimed entries out of active `challenges` into `claimed_challenges` without ever resetting them.
    let claimed_ids: Vec<i64> = st
        .challenges
        .iter()
        .filter(|(_, c)| c.claimed)
        .map(|(&cid, _)| cid)
        .collect();
    for cid in claimed_ids {
        if let Some(ch) = st.challenges.remove(&cid) {
            if def_map.get(&cid).is_some_and(|d| d.should_only_appear_once_per_season) {
                st.completed_once_per_season_ids.insert(cid);
            }
            st.claimed_challenges.insert(cid, ch);
        }
    }

    // Evict expired unclaimed non-dormant challenges instead of resetting claimed/one-shot entries in place.
    let mut expired_ids: Vec<i64> = st
        .challenges
        .iter()
        .filter(|(_, c)| !c.claimed && !c.is_dormant && c.expiration_date > 0 && now_ft >= c.expiration_date)
        .map(|(&cid, _)| cid)
        .collect();
    expired_ids.sort_unstable();
    for &cid in &expired_ids {
        st.challenges.remove(&cid);
        if def_map.get(&cid).is_some_and(|d| d.should_only_appear_once_per_season) {
            st.completed_once_per_season_ids.insert(cid);
        }
    }

    let mut active_count = st
        .challenges
        .values()
        .filter(|c| !c.is_dormant && !c.claimed)
        .count();
    let mut newly_inserted: Vec<CharacterChallenge> = Vec::new();

    for allow_expired_reuse in [false, true] {
        if active_count >= max_active {
            break;
        }
        for def in defs {
            if active_count >= max_active {
                break;
            }
            if def.is_dormant
                || st.challenges.contains_key(&def.challenge_id)
                || st.claimed_challenges.contains_key(&def.challenge_id)
                || st.completed_once_per_season_ids.contains(&def.challenge_id)
            {
                continue;
            }
            if !allow_expired_reuse && expired_ids.contains(&def.challenge_id) {
                continue;
            }
            let mut ch = def.to_character_challenge_with_end(st.selected_season_id, now_ft, season_end_ft);
            ch.baseline_progress = get_current_activity_count(&def.event_type, snap, char_id);
            newly_inserted.push(ch.clone());
            st.challenges.insert(def.challenge_id, ch);
            active_count += 1;
        }
    }
    expired_ids
        .into_iter()
        .zip(newly_inserted)
        .collect()
}

pub fn ensure_character_challenges(
    st: &mut CharacterSeasonState,
    defs: &[SeasonalChallengeDef],
    snap: &eve_db::WorldStateSnapshot,
    char_id: i64,
    now_ft: i64,
) {
    let _ = ensure_character_challenges_with_limit(
        st,
        defs,
        snap,
        char_id,
        now_ft,
        now_ft + 30 * 86_400 * 10_000_000,
        4,
    );
}

pub fn ensure_challenges_with_defs(st: &mut CharacterSeasonState, defs: &[SeasonalChallengeDef], now_ft: i64) {
    for def in defs {
        if st.claimed_challenges.contains_key(&def.challenge_id)
            || st.completed_once_per_season_ids.contains(&def.challenge_id)
        {
            continue;
        }
        st.challenges.entry(def.challenge_id).or_insert_with(|| {
            def.to_character_challenge(st.selected_season_id, now_ft)
        });
    }
}

pub fn ensure_challenges(st: &mut CharacterSeasonState, now_ft: i64) {
    let defs = default_seasonal_challenge_definitions();
    ensure_challenges_with_defs(st, &defs, now_ft);
}

pub fn sync_character_challenges_with_defs(
    st: &mut CharacterSeasonState,
    defs: &[SeasonalChallengeDef],
    snap: &eve_db::WorldStateSnapshot,
    char_id: i64,
) {
    let def_map: HashMap<i64, &SeasonalChallengeDef> = defs.iter().map(|d| (d.challenge_id, d)).collect();
    for ch in st.challenges.values_mut() {
        if ch.is_dormant {
            continue;
        }
        let Some(def) = def_map.get(&ch.challenge_id) else {
            continue;
        };
        let current = get_current_activity_count(&def.event_type, snap, char_id);
        let effective = current.saturating_sub(ch.baseline_progress);
        if effective > ch.progress {
            ch.progress = effective.min(ch.max_progress);
            if ch.progress >= ch.max_progress {
                ch.completed = true;
            }
        }
    }
}

pub fn sync_character_challenges(
    st: &mut CharacterSeasonState,
    snap: &eve_db::WorldStateSnapshot,
    char_id: i64,
) {
    let defs = load_challenge_definitions(snap);
    sync_character_challenges_with_defs(st, &defs, snap, char_id);
}

pub fn get_character_challenges_for_quasar(
    snap: &eve_db::WorldStateSnapshot,
    sde: Option<&eve_sde::SdeRegistry>,
    now_ft: i64,
    char_id: i64,
) -> Vec<CharacterChallenge> {
    let mut state = load_season_state_from_blobs(snap, char_id);
    let defs = match sde {
        Some(s) => ops::load_localized_challenge_definitions(snap, s),
        None => load_challenge_definitions(snap),
    };
    let (_, default_end_ft) = ops::active_fsd_epoch_window_ft(now_ft);
    let (season_end_ft, max_active) = match sde {
        Some(s) => resolve_season_bounds(&state, snap, s, now_ft),
        None => (default_end_ft, 4),
    };
    let _ = ops::fill_and_normalize_character_challenges(
        &mut state,
        &defs,
        snap,
        char_id,
        now_ft,
        season_end_ft,
        max_active,
    );
    sync_character_challenges_with_defs(&mut state, &defs, snap, char_id);
    let mut list: Vec<_> = state.challenges.into_values().collect();
    list.sort_by_key(|c| c.challenge_id);
    list
}

fn season_config_to_keyval(cfg: &SeasonConfig, defs: &[SeasonalChallengeDef]) -> PyRep {
    let mut tracked: Vec<String> = defs.iter().map(|d| d.event_type.clone()).collect();
    tracked.sort();
    tracked.dedup();
    PyRep::keyval([
        ("season_id", PyRep::Integer(cfg.season_id)),
        ("seasonID", PyRep::Integer(cfg.season_id)),
        ("template_id", PyRep::Integer(cfg.template_id)),
        ("title", PyRep::Integer(cfg.title_message_id)),
        ("seasonName", PyRep::String(cfg.season_name.clone())),
        ("news", PyRep::Integer(cfg.description_message_id)),
        ("navigation_card_picture_path", PyRep::String(cfg.navigation_card_picture_path.clone())),
        ("content_card_picture_path", PyRep::String(cfg.navigation_card_picture_path.clone())),
        ("background_no_logo", PyRep::String(cfg.background_no_logo.clone())),
        ("agencyLargeSeasonSelectionTitleID", PyRep::Integer(cfg.large_selection_title_id)),
        ("seasonSelectionDescriptionID", PyRep::Integer(cfg.description_message_id)),
        ("seasonSelectionHintID", PyRep::Integer(cfg.hint_message_id)),
        ("seasonSelectionTitleID", PyRep::Integer(cfg.title_message_id)),
        ("start_time", PyRep::Integer(cfg.start_time_ft)),
        ("end_time", PyRep::Integer(cfg.end_time_ft)),
        ("multiple_season_running", PyRep::Bool(cfg.multiple_season_running)),
        ("max_active_challenges", PyRep::Integer(cfg.max_active_challenges)),
        ("tracked_event_types", PyRep::List(tracked.into_iter().map(PyRep::String).collect())),
    ])
}

/// Dispatches calls to `seasonManager`.
pub fn handle_season_manager(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    if service != "seasonManager" {
        return None;
    }

    match (service, method) {
        ("seasonManager", "get_max_active_challenges") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let max_ch = ctx.db.with_snapshot(|snap| {
                if let Some(cfg) = &snap.seasons_and_campaigns.season_configuration {
                    return cfg.max_active_challenges.max(1);
                }
                let st = load_season_state_from_blobs(snap, char_id);
                let catalog = load_seasons_catalog(snap, &ctx.sde, ctx.now_blueos_filetime_ticks());
                if let Some(cfg) = catalog.get(&st.selected_season_id) {
                    return cfg.max_active_challenges.max(1);
                }
                let defs = load_challenge_definitions_with_sde(snap, &ctx.sde);
                defs.iter().filter(|d| !d.is_dormant).count().min(4).max(1) as i64
            });
            Some(Ok(PyRep::Integer(max_ch)))
        }
        ("seasonManager", "get_season_end_time") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let end_ft_opt = ctx.db.with_snapshot(|snap| {
                let st = load_season_state_from_blobs(snap, char_id);
                let now_ft = ctx.now_blueos_filetime_ticks();
                ops::resolve_season_end_time_readonly(
                    snap,
                    &ctx.sde,
                    st.selected_season_id,
                    now_ft,
                )
            });
            Some(Ok(end_ft_opt.map(PyRep::Integer).unwrap_or(PyRep::None)))
        }
        ("seasonManager", "select_season") => {
            // Resolve active season schedule from persisted catalog or SDE community_track_templates
            // when not overridden in service_state_blobs (rpc:seasonManager.select_season)
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if let Some(res) = crate::services::notify_emitters_seasons_tactical::handle_select_season_clear_if_none_or_zero(ctx, char_id, args.first()) {
                return Some(res);
            }
            let Some(season_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidSeasonId".into(),
                    message: "select_season requires an integer season ID".into(),
                    client_error: None,
                }));
            };
            let res = with_season_state(ctx, char_id, |st, snap| {
                let now_ft = ctx.now_blueos_filetime_ticks();
                let catalog = load_seasons_catalog(snap, &ctx.sde, now_ft);
                let Some(active_cfg) = catalog.get(&season_id).cloned() else {
                    return Err(BridgeRpcError {
                        code: "InvalidSeasonId".into(),
                        message: format!("Unknown or inactive season ID {season_id}"),
                        client_error: None,
                    });
                };
                if active_cfg.end_time_ft <= now_ft {
                    return Err(BridgeRpcError {
                        code: "InvalidSeasonId".into(),
                        message: format!("Season ID {season_id} has already ended"),
                        client_error: None,
                    });
                }
                st.selected_season_id = season_id;
                for ch in st.challenges.values_mut().chain(st.claimed_challenges.values_mut()) {
                    ch.season_id = season_id;
                }
                snap.seasons_and_campaigns
                    .active_seasons
                    .insert(char_id, season_id);

                let defs = load_challenge_definitions_with_sde(snap, &ctx.sde);
                let season_data_kv = season_config_to_keyval(&active_cfg, &defs);
                let character_progress_kv = PyRep::keyval([
                    ("character_id", PyRep::Integer(char_id)),
                    ("characterID", PyRep::Integer(char_id)),
                    ("season_id", PyRep::Integer(season_id)),
                    ("seasonID", PyRep::Integer(season_id)),
                    ("season_points", PyRep::Integer(st.season_points)),
                    ("seasonPoints", PyRep::Integer(st.season_points)),
                    ("challenges", PyRep::Dict(sorted_challenge_dict(st))),
                ]);
                Ok((season_data_kv, character_progress_kv))
            });
            match res {
                Ok((season_data_kv, character_progress_kv)) => {
                    ctx.emit_typed(
                        NotificationTarget::Character(char_id),
                        OnSeasonSelected {
                            character_id: char_id,
                            season_data: season_data_kv,
                            character_progress: character_progress_kv,
                        },
                    );
                    Some(Ok(PyRep::Bool(true)))
                }
                Err(e) => Some(Err(e)),
            }
        }
        ("seasonManager", "get_season_data_for_character") => {
            // Load character season state, SDE community_track_templates schedule, and
            // challenge definitions (rpc:seasonManager.get_season_data_for_character)
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let rep = ctx.db.with_mut_snapshot(|snap| {
                let st = load_season_state_from_blobs(snap, char_id);
                let now_ft = ctx.now_blueos_filetime_ticks();
                let unseeded = snap.seasons_and_campaigns.seasons_catalog.is_empty();
                let catalog = load_seasons_catalog(snap, &ctx.sde, now_ft);
                if unseeded {
                    snap.seasons_and_campaigns.seasons_catalog = catalog.clone();
                }
                let defs = load_challenge_definitions_with_sde(snap, &ctx.sde);
                let active: Vec<&SeasonConfig> = catalog
                    .values()
                    .filter(|cfg| cfg.end_time_ft > now_ft)
                    .collect();
                if active.is_empty() {
                    PyRep::None
                } else if st.selected_season_id == 0 && active.len() > 1 {
                    let list: Vec<PyRep> = active
                        .into_iter()
                        .map(|cfg| season_config_to_keyval(cfg, &defs))
                        .collect();
                    PyRep::List(list)
                } else if let Some(cfg) = catalog
                    .get(&st.selected_season_id)
                    .filter(|c| c.end_time_ft > now_ft)
                    .or_else(|| active.first().copied())
                {
                    season_config_to_keyval(cfg, &defs)
                } else {
                    PyRep::None
                }
            });
            Some(Ok(rep))
        }
        ("seasonManager", "fill_up_and_return_challenges_for_character") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let res = with_season_state(ctx, char_id, |st, snap| {
                let now_ft = ctx.now_blueos_filetime_ticks();
                let (season_end_ft, max_active) = resolve_season_bounds(st, snap, &ctx.sde, now_ft);
                let defs = ops::load_localized_challenge_definitions(snap, &ctx.sde);
                let expired = ops::fill_and_normalize_character_challenges(st, &defs, snap, char_id, now_ft, season_end_ft, max_active);
                sync_character_challenges_with_defs(st, &defs, snap, char_id);
                Ok((sorted_challenge_dict(st), st.season_points, expired))
            });
            match res {
                Ok((challenges, pts, expired)) => {
                    emit_expired_challenges(ctx, char_id, pts, &expired);
                    Some(Ok(PyRep::Dict(challenges)))
                }
                Err(e) => Some(Err(e)),
            }
        }
        ("seasonManager", "get_challenge") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(ch_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "ChallengeNotFound".into(),
                    message: "get_challenge requires a challenge ID".into(),
                    client_error: None,
                }));
            };
            let res = read_character_season_state(ctx, char_id, |st, snap| {
                if let Some(c) = st.challenges.get(&ch_id).or_else(|| st.claimed_challenges.get(&ch_id)) {
                    Ok(challenge_to_keyval(c, st.season_points))
                } else {
                    let defs = ops::load_localized_challenge_definitions(snap, &ctx.sde);
                    if let Some(def) = defs.iter().find(|d| d.challenge_id == ch_id) {
                        let now_ft = ctx.now_blueos_filetime_ticks();
                        let (season_end_ft, _) = resolve_season_bounds(st, snap, &ctx.sde, now_ft);
                        let ch = ops::build_character_challenge(def, st.selected_season_id, snap, char_id, now_ft, season_end_ft, def.is_dormant);
                        Ok(challenge_to_keyval(&ch, st.season_points))
                    } else {
                        Err(BridgeRpcError {
                            code: "ChallengeNotFound".into(),
                            message: format!("Challenge {ch_id} not found"),
                            client_error: None,
                        })
                    }
                }
            });
            Some(res)
        }
        ("seasonManager", "challenge_is_dormant") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(ch_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "ChallengeNotFound".into(),
                    message: "challenge_is_dormant requires a challenge ID".into(),
                    client_error: None,
                }));
            };
            let res = read_character_season_state(ctx, char_id, |st, snap| {
                if let Some(c) = st.challenges.get(&ch_id).or_else(|| st.claimed_challenges.get(&ch_id)) {
                    let is_dormant = c.is_dormant || (c.completed && !c.claimed);
                    Ok(PyRep::Bool(is_dormant))
                } else {
                    let defs = ops::load_localized_challenge_definitions(snap, &ctx.sde);
                    if let Some(def) = defs.iter().find(|d| d.challenge_id == ch_id) {
                        Ok(PyRep::Bool(def.is_dormant))
                    } else {
                        Err(BridgeRpcError {
                            code: "ChallengeNotFound".into(),
                            message: format!("Challenge {ch_id} not found"),
                            client_error: None,
                        })
                    }
                }
            });
            Some(res)
        }
        ("seasonManager", "get_challenge_expiration_date") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(ch_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "ChallengeNotFound".into(),
                    message: "get_challenge_expiration_date requires a challenge ID".into(),
                    client_error: None,
                }));
            };
            let res = read_character_season_state(ctx, char_id, |st, snap| {
                if let Some(c) = st.challenges.get(&ch_id).or_else(|| st.claimed_challenges.get(&ch_id)) {
                    Ok(PyRep::Integer(c.expiration_date))
                } else {
                    let defs = ops::load_localized_challenge_definitions(snap, &ctx.sde);
                    if let Some(def) = defs.iter().find(|d| d.challenge_id == ch_id) {
                        let now_ft = ctx.now_blueos_filetime_ticks();
                        let (season_end_ft, _) = resolve_season_bounds(st, snap, &ctx.sde, now_ft);
                        let ch = ops::build_character_challenge(def, st.selected_season_id, snap, char_id, now_ft, season_end_ft, def.is_dormant);
                        Ok(PyRep::Integer(ch.expiration_date))
                    } else {
                        Err(BridgeRpcError {
                            code: "ChallengeNotFound".into(),
                            message: format!("Challenge {ch_id} not found"),
                            client_error: None,
                        })
                    }
                }
            });
            Some(res)
        }
        ("seasonManager", "process_client_event") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let arg0_int = args
                .first()
                .and_then(|a| a.as_i64())
                .or_else(|| kwargs.get("challenge_id").and_then(|v| v.as_i64()));
            let arg0_str = args
                .first()
                .and_then(|a| a.as_str())
                .or_else(|| kwargs.get("event_name").and_then(|v| v.as_str()))
                .or_else(|| kwargs.get("event_type").and_then(|v| v.as_str()));
            if arg0_int.is_none() && arg0_str.is_none() {
                return Some(Err(BridgeRpcError {
                    code: "ChallengeNotFound".into(),
                    message: "Challenge not found".into(),
                    client_error: None,
                }));
            }
            let amount = if let Some(v) = kwargs.get("amount").filter(|v| !matches!(v, PyRep::None)) {
                let Some(n) = v.as_i64().filter(|&n| n > 0) else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidEventProgress".into(),
                        message: "Event progress increment must be a positive integer".into(),
                        client_error: None,
                    }));
                };
                n
            } else if let Some(v) = args.get(1).filter(|v| !matches!(v, PyRep::None | PyRep::Dict(_) | PyRep::KeyVal(_))) {
                let Some(n) = v.as_i64().filter(|&n| n > 0) else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidEventProgress".into(),
                        message: "Event progress increment must be a positive integer".into(),
                        client_error: None,
                    }));
                };
                n
            } else {
                1
            };
            let extra_data = kwargs
                .get("extra_data")
                .or_else(|| args.get(2))
                .or_else(|| args.get(1).filter(|a| matches!(a, PyRep::Dict(_) | PyRep::KeyVal(_))))
                .filter(|a| !matches!(a, PyRep::None));
            let cond_catalog = fsd_challenge_conditions(&ctx.sde);
            let res = with_season_state(ctx, char_id, |st, snap| {
                let now_ft = ctx.now_blueos_filetime_ticks();
                let (season_end_ft, max_active) = resolve_season_bounds(st, snap, &ctx.sde, now_ft);
                let defs = ops::load_localized_challenge_definitions(snap, &ctx.sde);
                let expired = ops::fill_and_normalize_character_challenges(st, &defs, snap, char_id, now_ft, season_end_ft, max_active);
                sync_character_challenges_with_defs(st, &defs, snap, char_id);
                let step = amount.min(1);
                let mut updates: Vec<(i64, i64, bool)> = Vec::new();
                if let Some(ch_id) = arg0_int {
                    let def_opt = defs.iter().find(|d| d.challenge_id == ch_id);
                    let Some(ch) = st.challenges.get_mut(&ch_id) else {
                        if def_opt.is_some_and(|d| !d.client_event_driven) {
                            return Err(BridgeRpcError {
                                code: "InvalidChallengeEvent".into(),
                                message: "Server-side activity challenge cannot be progressed via client events".into(),
                                client_error: None,
                            });
                        }
                        return Err(BridgeRpcError {
                            code: "ChallengeNotFound".into(),
                            message: "Challenge not found".into(),
                            client_error: None,
                        });
                    };
                    if ch.is_dormant {
                        return Err(BridgeRpcError {
                            code: "ChallengeDormant".into(),
                            message: "Dormant challenge cannot be progressed".into(),
                            client_error: None,
                        });
                    }
                    let is_client_driven = def_opt.map(|d| d.client_event_driven).unwrap_or(ch.client_event_driven);
                    if !is_client_driven {
                        return Err(BridgeRpcError {
                            code: "InvalidChallengeEvent".into(),
                            message: "Server-side activity challenge cannot be progressed via client events".into(),
                            client_error: None,
                        });
                    }
                    if def_opt.is_some_and(|d| !matches_challenge_conditions(d, extra_data, cond_catalog)) {
                        return Ok((expired, st.season_points, updates));
                    }
                    let was_completed = ch.completed;
                    ch.progress = ch.progress.saturating_add(step).min(ch.max_progress);
                    if ch.progress >= ch.max_progress {
                        ch.completed = true;
                    }
                    updates.push((ch_id, ch.progress, !was_completed && ch.completed));
                } else if let Some(ev_name) = arg0_str {
                    let mut matching_ids: Vec<i64> = st
                        .challenges
                        .values()
                        .filter(|ch| {
                            if ch.is_dormant || ch.claimed {
                                return false;
                            }
                            let def_opt = defs.iter().find(|d| d.challenge_id == ch.challenge_id);
                            let is_client_driven = def_opt.map(|d| d.client_event_driven).unwrap_or(ch.client_event_driven);
                            is_client_driven
                                && (def_opt.is_some_and(|d| {
                                    d.event_type == ev_name
                                        || d.event_type.strip_prefix("client_") == Some(ev_name)
                                }) || ch.title == ev_name)
                                && def_opt.is_none_or(|d| matches_challenge_conditions(d, extra_data, cond_catalog))
                        })
                        .map(|ch| ch.challenge_id)
                        .collect();
                    matching_ids.sort_unstable();
                    for cid in matching_ids {
                        if let Some(ch) = st.challenges.get_mut(&cid) {
                            let was_completed = ch.completed;
                            ch.progress = ch.progress.saturating_add(step).min(ch.max_progress);
                            if ch.progress >= ch.max_progress {
                                ch.completed = true;
                            }
                            updates.push((cid, ch.progress, !was_completed && ch.completed));
                        }
                    }
                }
                Ok((expired, st.season_points, updates))
            });
            match res {
                Ok((expired, pts, updates)) => {
                    emit_expired_challenges(ctx, char_id, pts, &expired);
                    for (cid, new_prog, newly_completed) in updates {
                        ctx.emit_typed(
                            NotificationTarget::Character(char_id),
                            OnChallengeProgressUpdate {
                                challenge_id: cid,
                                new_progress: new_prog,
                            },
                        );
                        if newly_completed {
                            ctx.emit_typed(
                                NotificationTarget::Character(char_id),
                                OnChallengeCompleted { challenge_id: cid },
                            );
                        }
                    }
                    Some(Ok(PyRep::None))
                }
                Err(e) => Some(Err(e)),
            }
        }
        ("seasonManager", "claim_challenge_rewards") => {
            let (uid, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let session_dock = session.station_id.or(session.structure_id);
            let Some(ch_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "ChallengeNotFound".into(),
                    message: "claim_challenge_rewards requires a challenge ID".into(),
                    client_error: None,
                }));
            };
            let now_unix = ctx.now_unix();
            let res = with_season_state(ctx, char_id, |st, snap| {
                let docked_location = session_dock.or_else(|| {
                    snap.characters
                        .get(&char_id)
                        .and_then(|c| c.station_id.or(c.structure_id))
                });
                let now_ft = ctx.now_blueos_filetime_ticks();
                let (season_end_ft, max_active) = resolve_season_bounds(st, snap, &ctx.sde, now_ft);
                let defs = ops::load_localized_challenge_definitions(snap, &ctx.sde);
                let expired = ops::fill_and_normalize_character_challenges(st, &defs, snap, char_id, now_ft, season_end_ft, max_active);
                let def_opt = defs.iter().find(|d| d.challenge_id == ch_id).cloned();
                sync_character_challenges_with_defs(st, &defs, snap, char_id);
                if st.claimed_challenges.contains_key(&ch_id)
                    || (!st.challenges.contains_key(&ch_id) && st.completed_once_per_season_ids.contains(&ch_id))
                {
                    return Err(BridgeRpcError {
                        code: "ChallengeAlreadyClaimed".into(),
                        message: "Challenge rewards already claimed".into(),
                        client_error: None,
                    });
                }
                let Some(ch_ref) = st.challenges.get(&ch_id) else {
                    return Err(BridgeRpcError {
                        code: "ChallengeNotFound".into(),
                        message: "Challenge not found".into(),
                        client_error: None,
                    });
                };
                if ch_ref.is_dormant {
                    return Err(BridgeRpcError {
                        code: "ChallengeDormant".into(),
                        message: "Dormant challenge cannot be claimed".into(),
                        client_error: None,
                    });
                }
                if ch_ref.claimed {
                    return Err(BridgeRpcError {
                        code: "ChallengeAlreadyClaimed".into(),
                        message: "Challenge rewards already claimed".into(),
                        client_error: None,
                    });
                }
                if !ch_ref.completed && ch_ref.progress < ch_ref.max_progress {
                    return Err(BridgeRpcError {
                        code: "ChallengeNotCompleted".into(),
                        message: "Challenge is not yet completed".into(),
                        client_error: None,
                    });
                }
                let mut ch = st.challenges.remove(&ch_id).expect("verified present");
                ch.completed = true;
                ch.claimed = true;
                let (prev_pts, reward_pts, season_id) = (st.season_points, ch.points, st.selected_season_id);
                let (r_tid, r_qty, client_driven) = (ch.reward_type_id, ch.reward_quantity, ch.client_event_driven);
                if def_opt.as_ref().is_some_and(|d| d.should_only_appear_once_per_season) {
                    st.completed_once_per_season_ids.insert(ch_id);
                }
                st.claimed_challenges.insert(ch_id, ch);
                st.season_points += reward_pts;
                let pts = st.season_points;
                let mut new_balance_cents: Option<i64> = None;
                if let Some(ref def) = def_opt {
                    if !client_driven && def.isk_awarded > 0 {
                        let add_cents = eve_db::IskCents::from_isk(def.isk_awarded).0;
                        if let Some(char_rec) = snap.characters.get_mut(&char_id) {
                            char_rec.balance = eve_db::IskCents(char_rec.balance.0.saturating_add(add_cents));
                            char_rec.version = char_rec.version.wrapping_add(1);
                            new_balance_cents = Some(char_rec.balance.0);
                        }
                        if let Some(bal) = new_balance_cents {
                            snap.next_entity_id += 1;
                            let jid = snap.next_entity_id;
                            snap.wallet_journal.push(eve_db::WalletJournalEntry {
                                entry_id: jid,
                                character_id: char_id,
                                entry_type: "seasonal_challenge_reward".into(),
                                amount: eve_db::IskCents(add_cents),
                                balance_after: eve_db::IskCents(bal),
                                reason: format!("Seasonal challenge {ch_id} ISK reward"),
                            });
                            snap.wallet_journal_timestamps.insert(jid, now_unix);
                        }
                    }
                    if let Some(next_id) = def.next_challenge {
                        let next_def = defs.iter().find(|d| d.challenge_id == next_id);
                        if let Some(existing) = st.challenges.get_mut(&next_id) {
                            if existing.is_dormant {
                                if let Some(nd) = next_def {
                                    existing.baseline_progress = get_current_activity_count(&nd.event_type, snap, char_id);
                                }
                                existing.is_dormant = false;
                            }
                        } else if let Some(nd) = next_def {
                            let mut next_ch = nd.to_character_challenge_with_end(st.selected_season_id, now_ft, season_end_ft);
                            next_ch.is_dormant = false;
                            next_ch.baseline_progress = get_current_activity_count(&nd.event_type, snap, char_id);
                            st.challenges.insert(next_id, next_ch);
                        }
                    }
                }
                let is_omega = snap.characters.get(&char_id).map(|c| c.clone_grade >= 2).unwrap_or(false);
                let mut created_item_notif: Option<(i64, u32, i64, i32)> = None;
                let mut enqueued_redeem = false;
                let delivered_rep = if !client_driven && r_tid > 0 && r_qty > 0 {
                    if let Some(loc_id) = docked_location {
                        snap.next_entity_id += 1;
                        let reward_item_id = snap.next_entity_id;
                        snap.items.insert(reward_item_id, eve_db::InventoryItemRecord {
                            item_id: reward_item_id, type_id: r_tid, owner_id: char_id,
                            location_id: loc_id, flag: eve_db::InventoryFlag::Hangar,
                            quantity: r_qty, singleton: false, version: 1, ..Default::default()
                        });
                        created_item_notif = Some((reward_item_id, r_tid, loc_id, r_qty));
                        PyRep::Integer(reward_item_id)
                    } else {
                        let tok_id = crate::services::account_services::enqueue_redeem_token_in_snapshot(
                            snap,
                            uid,
                            r_tid,
                            r_qty,
                            format!("Seasonal challenge {ch_id} reward"),
                            None,
                            None,
                        );
                        enqueued_redeem = true;
                        PyRep::Integer(tok_id)
                    }
                } else {
                    PyRep::None
                };
                let rep = PyRep::keyval([
                    ("challengeID", PyRep::Integer(ch_id)), ("claimed", PyRep::Bool(true)),
                    ("seasonPoints", PyRep::Integer(pts)), ("rewardGranted", PyRep::Bool(true)),
                    ("rewardPoints", PyRep::Integer(reward_pts)), ("deliveredItemID", delivered_rep),
                ]);
                Ok((rep, season_id, prev_pts, pts, is_omega, expired, new_balance_cents, created_item_notif, enqueued_redeem))
            });
            match res {
                Ok((rep, season_id, prev_pts, pts, is_omega, expired, new_balance_cents, created_item_notif, enqueued_redeem)) => {
                    emit_expired_challenges(ctx, char_id, pts, &expired);
                    if let Some(bal_cents) = new_balance_cents {
                        ctx.emit_typed(
                            NotificationTarget::Character(char_id),
                            OnAccountChange {
                                account_key: "cash".into(),
                                owner_id: char_id,
                                balance: eve_db::IskCents(bal_cents).to_f64_isk(),
                            },
                        );
                    }
                    if let Some((item_id, type_id, loc_id, qty)) = created_item_notif {
                        let (group_id, category_id) = ctx
                            .sde
                            .types
                            .get(&type_id)
                            .map(|t| (t.group_id, t.category_id))
                            .unwrap_or((0, 0));
                        ctx.emit_typed(
                            NotificationTarget::Character(char_id),
                            OnItemChange {
                                item: PyRep::keyval([
                                    ("itemID", PyRep::Integer(item_id)),
                                    ("typeID", PyRep::Integer(type_id as i64)),
                                    ("ownerID", PyRep::Integer(char_id)),
                                    ("locationID", PyRep::Integer(loc_id)),
                                    ("flagID", PyRep::Integer(eve_db::InventoryFlag::Hangar as i64)),
                                    ("quantity", PyRep::Integer(qty as i64)),
                                    ("stacksize", PyRep::Integer(qty as i64)),
                                    ("groupID", PyRep::Integer(i64::from(group_id))),
                                    ("categoryID", PyRep::Integer(i64::from(category_id))),
                                    ("singleton", PyRep::Bool(false)),
                                ]),
                                change: PyRep::Dict(vec![(PyRep::Integer(3), PyRep::Integer(0))]),
                                location: PyRep::None,
                            },
                        );
                    }
                    if enqueued_redeem {
                        ctx.emit_typed(
                            NotificationTarget::Character(char_id),
                            OnRedeemingQueueUpdated {},
                        );
                        ctx.emit_typed(
                            NotificationTarget::Character(char_id),
                            OnRedeemingTokensUpdated {},
                        );
                    }
                    ctx.emit_typed(
                        NotificationTarget::Character(char_id),
                        OnChallengeRewardsGranted { challenge_id: ch_id },
                    );
                    ctx.emit_typed(
                        NotificationTarget::Character(char_id),
                        OnSeasonalPointsUpdated {
                            season_id,
                            new_points: pts,
                        },
                    );
                    // Emit OnSeasonalGoalCompleted and Omega reward tier notifications across 10-point milestones
                    let (prev_goal, new_goal) = (prev_pts / 10, pts / 10);
                    if new_goal > prev_goal {
                        for g_idx in prev_goal..new_goal {
                            let goal_data = PyRep::Dict(vec![
                                (PyRep::String("goal_index".into()), PyRep::Integer(g_idx)),
                                (PyRep::String("points_required".into()), PyRep::Integer((g_idx + 1) * 10)),
                                (PyRep::String("completed".into()), PyRep::Bool(true)),
                            ]);
                            ctx.emit_typed(
                                NotificationTarget::Character(char_id),
                                OnSeasonalGoalCompleted {
                                    goal_index: g_idx,
                                    goal_data,
                                },
                            );
                        }
                        if is_omega {
                            let claimed_idxs: Vec<i64> = (prev_goal..new_goal).collect();
                            ctx.emit_typed(
                                NotificationTarget::Character(char_id),
                                OnSeasonOmegaRewardsClaimed {
                                    claimed_goal_indexes: claimed_idxs,
                                },
                            );
                        }
                    }
                    Some(Ok(rep))
                }
                Err(e) => Some(Err(e)),
            }
        }
        ("seasonManager", "get_loot_table_by_typeID") => {
            if let Err(e) = ServerContext::require_character(session) {
                return Some(Err(e));
            }
            let Some(type_id) = args
                .first()
                .and_then(|a| a.as_i64())
                .and_then(|n| u32::try_from(n).ok())
                .filter(|&tid| tid > 0 && ctx.sde.types.contains_key(&tid))
            else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidTypeId".into(),
                    message: "Invalid or unknown type ID".into(),
                    client_error: None,
                }));
            };
            let custom_loot: Option<Vec<(u32, i64, f64)>> = ctx.db.with_snapshot(|snap| {
                snap.seasons_and_campaigns
                    .season_loot_tables
                    .get(&type_id)
                    .cloned()
            });
            let entries = custom_loot.unwrap_or_else(|| {
                fsd_crate_loot_tables(&ctx.sde)
                    .get(&type_id)
                    .cloned()
                    .unwrap_or_default()
            });
            let list: Vec<PyRep> = entries
                .into_iter()
                .map(|(tid, qty, prob)| {
                    PyRep::keyval([
                        ("typeID", PyRep::Integer(tid as i64)),
                        ("quantity", PyRep::Integer(qty)),
                        ("probability", PyRep::Real(prob)),
                    ])
                })
                .collect();
            Some(Ok(PyRep::List(list)))
        }
        ("seasonManager", "get_character_progress_by_character_id") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let goal_filter = if let Some(raw_goal_id) = args.first().and_then(|a| a.as_i64()) {
                let Ok(goal_id) = u32::try_from(raw_goal_id) else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidGoalId".into(),
                        message: format!("goal_id {raw_goal_id} is out of valid u32 range"),
                        client_error: None,
                    }));
                };
                let milestones = crate::services::quasar_career_goals::default_career_milestones();
                let Some(resolved) = crate::domain::career_goals::resolve_career_goal_or_milestone(
                    &ctx.sde,
                    &ctx.quasar_gateway.progression.career_goals,
                    &milestones,
                    char_id,
                    goal_id,
                ) else {
                    return Some(Err(BridgeRpcError {
                        code: "ChallengeNotFound".into(),
                        message: format!("Goal or challenge {goal_id} not found"),
                        client_error: None,
                    }));
                };
                Some((
                    goal_id,
                    resolved
                        .career_points_progress
                        .map(|cp| (cp, resolved.required_progress)),
                ))
            } else {
                None
            };
            let res = with_season_state(ctx, char_id, |st, snap| {
                let now_ft = ctx.now_blueos_filetime_ticks();
                let (season_end_ft, max_active) = resolve_season_bounds(st, snap, &ctx.sde, now_ft);
                let defs = ops::load_localized_challenge_definitions(snap, &ctx.sde);
                let expired = ops::fill_and_normalize_character_challenges(st, &defs, snap, char_id, now_ft, season_end_ft, max_active);
                sync_character_challenges_with_defs(st, &defs, snap, char_id);
                let challenges = sorted_challenge_keyvals(st);
                let claimed_cnt = (st.claimed_challenges.len()
                    + st.challenges.values().filter(|c| c.claimed).count()) as i64;
                let eff_season_id = if st.selected_season_id > 0 { st.selected_season_id } else { 1 };
                let mut fields = vec![
                    ("season_id", PyRep::Integer(eff_season_id)),
                    ("seasonID", PyRep::Integer(eff_season_id)),
                    ("total_points", PyRep::Integer(st.season_points)),
                    ("season_points", PyRep::Integer(st.season_points)),
                    ("seasonPoints", PyRep::Integer(st.season_points)),
                    ("current_reward_index", PyRep::Integer(claimed_cnt)),
                    ("challenges", PyRep::List(challenges)),
                ];
                if let Some((goal_id, milestone_cp)) = goal_filter {
                    let (prog, req_prog, from_cp) = if let Some((cp, req)) = milestone_cp {
                        (cp, req, true)
                    } else {
                        let req = ctx
                            .sde
                            .career_goals
                            .get(&goal_id)
                            .map(|g| g.target_progress.max(1))
                            .unwrap_or(1);
                        let cur = snap
                            .npe_progress
                            .get(&char_id)
                            .and_then(|n| n.career_goal_progress.get(&goal_id).copied())
                            .unwrap_or(0);
                        (cur, req, false)
                    };
                    fields.push(("goalID", PyRep::Integer(goal_id as i64)));
                    fields.push(("currentProgress", PyRep::Integer(prog as i64)));
                    fields.push(("requiredProgress", PyRep::Integer(req_prog as i64)));
                    fields.push(("verifiedFromCareerPoints", PyRep::Bool(from_cp)));
                }
                Ok((PyRep::keyval(fields), st.season_points, expired))
            });
            match res {
                Ok((rep, pts, expired)) => {
                    emit_expired_challenges(ctx, char_id, pts, &expired);
                    Some(Ok(rep))
                }
                Err(e) => Some(Err(e)),
            }
        }
        ("seasonManager", "claim_goal_reward") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let raw_goal_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let Ok(goal_id) = u32::try_from(raw_goal_id) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidGoalId".into(),
                    message: format!("goal_id {raw_goal_id} is out of valid u32 range"),
                    client_error: None,
                }));
            };
            let milestones = crate::services::quasar_career_goals::default_career_milestones();
            let Some(resolved) = crate::domain::career_goals::resolve_career_goal_or_milestone(
                &ctx.sde,
                &ctx.quasar_gateway.progression.career_goals,
                &milestones,
                char_id,
                goal_id,
            ) else {
                return Some(Err(BridgeRpcError {
                    code: "ChallengeNotFound".into(),
                    message: format!("Goal or challenge {goal_id} not found"),
                    client_error: None,
                }));
            };
            let target_prog = resolved.required_progress;
            let sp_reward = resolved.sp_reward;
            let cp_prog = resolved.career_points_progress;
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let cur_prog = cp_prog.unwrap_or_else(|| {
                    snap.npe_progress
                        .get(&char_id)
                        .and_then(|n| n.career_goal_progress.get(&goal_id).copied())
                        .unwrap_or(0)
                });
                crate::domain::career_goals::claim_career_goal_or_milestone_in_snap(
                    snap,
                    &ctx.quasar_gateway.progression.career_goals,
                    char_id,
                    goal_id,
                    cur_prog,
                    target_prog,
                    sp_reward,
                )
                .map_err(super::domain_err_to_rpc)
            });
            match res {
                Ok(new_free_sp) => Some(Ok(PyRep::keyval([
                    ("goalID", PyRep::Integer(goal_id as i64)),
                    ("spReward", PyRep::Real(sp_reward)),
                    ("freeSkillPoints", PyRep::Real(new_free_sp)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }
        ("seasonManager", "reload_fsd_data") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if let Err(e) = ServerContext::require_gm_role(session) {
                return Some(Err(e));
            }
            let res = with_season_state(ctx, char_id, |st, snap| {
                let now_ft = ctx.now_blueos_filetime_ticks();
                let mut defs = fsd_challenge_definitions(&ctx.sde).to_vec();
                for def in &mut defs {
                    ops::localize_challenge_def(def, &ctx.sde);
                }
                snap.seasons_and_campaigns.challenge_definitions = defs.clone();
                let empty_snap = eve_db::WorldStateSnapshot::default();
                let seasons = ops::load_fsd_seasons_catalog_clean(&empty_snap, &ctx.sde, now_ft);
                let cfg = seasons.get(&st.selected_season_id).or_else(|| seasons.values().next());
                let (_, default_end_ft) = ops::active_fsd_epoch_window_ft(now_ft);
                let season_end_ft = cfg.map(|c| c.end_time_ft).unwrap_or(default_end_ft);
                let max_active = cfg
                    .map(|c| usize::try_from(c.max_active_challenges.max(1)).unwrap_or(4))
                    .unwrap_or(4);
                snap.seasons_and_campaigns.seasons_catalog = seasons;
                let expired = ops::fill_and_normalize_character_challenges(st, &defs, snap, char_id, now_ft, season_end_ft, max_active);
                Ok((st.season_points, expired))
            });
            match res {
                Ok((pts, expired)) => {
                    emit_expired_challenges(ctx, char_id, pts, &expired);
                    let sent = ctx.emit_typed(NotificationTarget::All, OnSeasonsFsdDataChanged {});
                    if !sent.iter().any(|n| n.recipient_char_id == char_id) {
                        ctx.emit_typed(
                            NotificationTarget::Character(char_id),
                            OnSeasonsFsdDataChanged {},
                        );
                    }
                    Some(Ok(PyRep::Bool(true)))
                }
                Err(e) => Some(Err(e)),
            }
        }
        _ => None,
    }
}

#[cfg(test)]
#[path = "season_manager_tests.rs"]
mod season_manager_tests;
#[cfg(test)]
#[path = "season_manager_edges2_tests.rs"]
mod season_manager_edges2_tests;

