//! Helper state, SDE campaign resolution, enrollment isolation, and 120-minute logon
//! milestone persistence for `campaigns_and_milestones`.

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, HashMap};

use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};

use crate::router::ServerContext;
use crate::services::campaigns_and_milestones::{get_sde_login_campaigns, SdeLoginCampaign};
use crate::services::cosmetics_ui_ops::{
    load_cosmetics_ui_state, SeasonalCampaignSchedule,
};

/// V24 `milestones/common/constants.pyj`: `MILESTONE_HUNDRED_AND_TWENTY_MINUTES_ID = 1`.
pub const MILESTONE_HUNDRED_AND_TWENTY_MINUTES_ID: i64 = 1;

/// V24 `milestones/common/constants.pyj`: `MILESTONE_REWARD_NUMBER_OF_MINUTES_GRACE_PERIOD = 119` minutes (7,140s).
pub const MILESTONE_REWARD_NUMBER_OF_MINUTES_GRACE_PERIOD_SECS: u64 = 119 * 60;

/// V24 `milestones/common/constants.pyj`: `MILESTONE_REWARD_NUMBER_OF_MINUTES = 120` minutes (7,200s).
pub const MILESTONE_REWARD_NUMBER_OF_MINUTES_SECS: u64 = 120 * 60;

/// V24 `milestones/common/constants.pyj`: `MILESTONE_HUNDRED_AND_TWENTY_MINUTES_REWARD_AMOUNT = 500000` ISK.
pub const MILESTONE_HUNDRED_AND_TWENTY_MINUTES_REWARD_ISK: i64 = 500_000;

/// Persistent per-character accumulated play-time tracking for login milestones.
pub use eve_db::{CharacterMilestoneState, ScheduledDailyCampaign};

fn err(code: &str, message: impl Into<String>) -> BridgeRpcError {
    BridgeRpcError {
        code: code.into(),
        message: message.into(),
        client_error: None,
    }
}

pub fn load_milestone_state(
    snap: &eve_db::WorldStateSnapshot,
    char_id: i64,
) -> Option<CharacterMilestoneState> {
    snap.seasons_and_campaigns
        .milestone_playtime
        .get(&char_id)
        .cloned()
}

pub fn save_milestone_state(
    snap: &mut eve_db::WorldStateSnapshot,
    char_id: i64,
    state: &CharacterMilestoneState,
) {
    snap.seasons_and_campaigns
        .milestone_playtime
        .insert(char_id, state.clone());
}

/// Updates persistent accumulated play time on character logon.
/// Offline gaps while `active_session == false` are never counted as played time.
pub fn record_milestone_logon(
    snap: &mut eve_db::WorldStateSnapshot,
    char_id: i64,
    now_unix: u64,
) -> CharacterMilestoneState {
    let mut state = match load_milestone_state(snap, char_id) {
        Some(mut existing) => {
            if existing.active_session && existing.last_activity_unix > 0 {
                existing.accumulated_playtime_secs = existing
                    .accumulated_playtime_secs
                    .saturating_add(now_unix.saturating_sub(existing.last_activity_unix));
            }
            existing.last_activity_unix = now_unix;
            existing.active_session = true;
            existing
        }
        None => CharacterMilestoneState {
            first_logon_unix: now_unix,
            last_activity_unix: now_unix,
            accumulated_playtime_secs: 0,
            active_session: true,
        },
    };
    if state.first_logon_unix == 0 {
        state.first_logon_unix = now_unix;
    }
    save_milestone_state(snap, char_id, &state);
    state
}

/// Flushes elapsed active-session playtime up to `now_unix` and pauses milestone
/// accumulation so offline time after disconnect/logoff/eviction is never counted.
pub fn pause_milestone_playtime_in_snap(
    snap: &mut eve_db::WorldStateSnapshot,
    char_id: i64,
    now_unix: u64,
) {
    let Some(mut existing) = load_milestone_state(snap, char_id) else {
        return;
    };
    if existing.active_session {
        if existing.last_activity_unix > 0 {
            existing.accumulated_playtime_secs = existing
                .accumulated_playtime_secs
                .saturating_add(now_unix.saturating_sub(existing.last_activity_unix));
        }
        existing.last_activity_unix = now_unix;
        existing.active_session = false;
        save_milestone_state(snap, char_id, &existing);
    }
}

/// Persistently flushes and pauses accumulated milestone playtime for `char_id`.
pub fn pause_milestone_playtime(
    db: &eve_db::WorldRepository,
    char_id: i64,
    now_unix: u64,
) {
    db.with_mut_snapshot(|snap| {
        pause_milestone_playtime_in_snap(snap, char_id, now_unix);
    });
}

/// Resolves the active daily login campaign for `char_id` from per-character enrollment,
/// scheduled campaign state in SQLite, or the V24 SDE catalog (`seasonalcampaignsloginrewards.fsdbinary`).
pub fn resolve_daily_campaign<'a>(
    snap: &eve_db::WorldStateSnapshot,
    char_id: i64,
    now_unix: u64,
    campaigns: &'a BTreeMap<i64, SdeLoginCampaign>,
) -> Result<(i64, bool, &'a SdeLoginCampaign), BridgeRpcError> {
    if campaigns.is_empty() {
        return Err(err(
            "CampaignNotFound",
            "No login campaigns available in SDE seasonalcampaignsloginrewards",
        ));
    }

    let ext = load_cosmetics_ui_state(snap);
    let per_char_cid = snap
        .seasons_and_campaigns
        .login_campaign_enrollments
        .get(&char_id)
        .copied();

    let scheduled = snap
        .seasons_and_campaigns
        .daily_login_campaign_schedule
        .as_ref()
        .filter(|s| {
            now_unix >= s.start_unix && s.end_unix.map_or(true, |end| now_unix < end)
        });

    let campaign_id = if let Some(cid) = per_char_cid {
        cid
    } else if let Some(ref sched) = scheduled {
        sched.campaign_id
    } else if let Some(cid) = ext.active_login_campaign_id {
        cid
    } else {
        // Select the active evergreen daily login track from the SDE catalog (non-retro-claim 30+ day track).
        campaigns
            .iter()
            .filter(|(_, c)| c.track_length() >= 30 && !c.retro_claim)
            .max_by_key(|(&cid, c)| (c.track_length(), std::cmp::Reverse(cid)))
            .or_else(|| {
                campaigns
                    .iter()
                    .max_by_key(|(&cid, c)| (c.track_length(), std::cmp::Reverse(cid)))
            })
            .map(|(&cid, _)| cid)
            .ok_or_else(|| {
                err(
                    "CampaignNotFound",
                    "No eligible daily login campaign found in SDE seasonalcampaignsloginrewards",
                )
            })?
    };

    let campaign = campaigns.get(&campaign_id).ok_or_else(|| {
        err(
            "CampaignNotFound",
            format!("Login campaign {campaign_id} not found in SDE seasonalcampaignsloginrewards"),
        )
    })?;

    let is_rookie = scheduled
        .as_ref()
        .filter(|s| s.campaign_id == campaign_id)
        .and_then(|s| s.is_rookie)
        .unwrap_or_else(|| campaign.track_length() <= 14);

    Ok((campaign_id, is_rookie, campaign))
}

/// Enrolls only `session.character_id` into a specific SDE daily login campaign without mutating
/// global campaign schedules or other characters' state, and emits `OnCampaignEnrollmentChanged`
/// scoped to that character.
#[allow(dead_code)]
pub(crate) fn handle_login_campaign_enroll(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let campaign_id = args
        .first()
        .and_then(|v| v.as_i64())
        .ok_or_else(|| err("InvalidArgument", "enroll_in_campaign requires campaign_id integer"))?;

    let campaigns = get_sde_login_campaigns(&ctx.sde);
    if !campaigns.contains_key(&campaign_id) {
        return Err(err(
            "CampaignNotFound",
            format!("Login campaign {campaign_id} not found in SDE seasonalcampaignsloginrewards"),
        ));
    }

    ctx.db.with_mut_snapshot(|snap| {
        snap.seasons_and_campaigns
            .login_campaign_enrollments
            .insert(char_id, campaign_id);
    });

    crate::services::notify_emitters_character_services::emit_campaign_enrollment_changed(
        ctx, char_id,
    );

    Ok(PyRep::Bool(true))
}

/// Resolves active seasonal campaign schedule from persisted snapshot state.
pub fn active_seasonal_schedule_from_snapshot(
    snap: &eve_db::WorldStateSnapshot,
    ext: &crate::services::cosmetics_ui_ops::CosmeticsUiExtendedState,
    now_unix: u64,
) -> Option<SeasonalCampaignSchedule> {
    ext.active_seasonal_schedule
        .as_ref()
        .filter(|s| now_unix >= s.start_unix && now_unix < s.end_unix)
        .cloned()
        .or_else(|| {
            snap.seasons_and_campaigns
                .seasonal_login_schedule
                .as_ref()
                .filter(|s| now_unix >= s.start_unix && now_unix < s.end_unix)
                .map(|s| SeasonalCampaignSchedule {
                    campaign_id: s.campaign_id,
                    start_unix: s.start_unix,
                    end_unix: s.end_unix,
                    turnover_secs: s.turnover_secs,
                })
        })
}

fn cached_type_name_ids(sde: &eve_sde::SdeRegistry) -> &'static HashMap<u32, u32> {
    static TYPE_NAME_IDS: std::sync::OnceLock<HashMap<u32, u32>> = std::sync::OnceLock::new();
    TYPE_NAME_IDS.get_or_init(|| {
        sde.load_raw_types_fsdbinary()
            .map(|(_, recs)| recs.into_iter().map(|(tid, r)| (tid, r.type_name_id)).collect())
            .unwrap_or_default()
    })
}

/// Resolves reward label message ID from `res:/staticdata/types.fsdbinary` (`type_name_id`),
/// falling back to the campaign's title message ID when the item type lacks a name ID.
pub fn resolve_reward_label_message_id(
    ctx: &ServerContext,
    type_id: u32,
    campaign: &SdeLoginCampaign,
) -> i64 {
    let msg_id = cached_type_name_ids(&ctx.sde)
        .get(&type_id)
        .copied()
        .filter(|&id| id > 0)
        .unwrap_or(campaign.campaign_name_id);
    i64::from(msg_id)
}

/// Emits `OnAccountChange` (and pushes to `session` for direct unit-test handles) on ISK balance update.
pub fn emit_account_change_isk(
    ctx: &ServerContext,
    session: &mut SessionState,
    char_id: i64,
    bal_after_cents: i64,
) {
    let bal_f64 = (bal_after_cents as f64) / 100.0;
    let notif = crate::router::notify_registry::OnAccountChange {
        account_key: "cash".into(),
        owner_id: char_id,
        balance: bal_f64,
    };
    if !session.live_notify_listener {
        crate::router::notify_registry::push_typed_session_notification(session, notif.clone());
    }
    ctx.emit_typed(crate::router::NotificationTarget::Character(char_id), notif);
}

/// Resolves the character's reward delivery station/structure from active session docking or
/// persistent character docking (`None` when undocked in space so rewards enqueue in redeeming bay).
pub fn resolve_reward_station_id(
    sess_docked: Option<i64>,
    c: &eve_db::CharacterRecord,
) -> Option<i64> {
    sess_docked
        .filter(|&id| id > 0)
        .or_else(|| c.station_id.filter(|&id| id > 0))
        .or_else(|| c.structure_id.filter(|&id| id > 0))
}
